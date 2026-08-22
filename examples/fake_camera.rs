//! A fake VISCA camera for manually smoke-testing `viscous` against a real
//! serial-shaped transport without physical hardware.
//!
//! This is deliberately separate from the automated test suite (that's
//! `ScriptedBlockingTransport`'s job, used throughout `src/`) — this exists
//! to exercise the real byte path end to end: opening an actual device path,
//! real framing over an actual stream, actual read/write timing. Every
//! received command and every sent reply is logged in decoded form to
//! stdout, and simulated pan/tilt/zoom/focus state actually changes so the
//! client's info panel visibly reflects what was sent.
//!
//! Commands that travel a known distance (pan/tilt relative move, pan/tilt
//! home, preset recall) reply with their ACK immediately but delay their
//! Completion by a simulated travel time — proportional to the distance
//! moved, at a fixed simulated speed — so the client's "in progress" handling
//! actually has something to exercise instead of every command resolving
//! instantly. A few presets are pre-seeded at varied distances from the origin
//! for this.
//!
//! Continuous drive commands (pan/tilt, zoom and focus) instead complete at
//! once and set a rate, exactly as a real camera does: they have no distance
//! of their own, so the simulated position keeps moving between messages for
//! as long as the drive is running and only stops when a stop command arrives.
//!
//! Command/inquiry byte layouts below are traced from grafton-visca's own
//! source (`command::pan_tilt`, `command::zoom`, `command::focus`,
//! `command::preset`, `command::inquiry_structs`), not guessed.
//!
//! Run with `cargo run --example fake_camera`, then in another terminal:
//! `cargo run -- <the printed slave path>`.
//!
//! That auto-provisioned pty is Unix-only (there's no equivalent trick on
//! Windows). Everywhere else — including Windows, via a virtual null-modem
//! pair from [com0com](https://com0com.sourceforge.net/) — pass an existing
//! port name instead: `cargo run --example fake_camera -- COM10`, then point
//! `viscous`/a GUI at the pair's other end (`COM11`).
//!
//! Passing `tcp://[host:]port` instead listens for VISCA over IP, which needs
//! no virtual serial hardware on either side and reaches across machines: run
//! `cargo run --example fake_camera -- tcp://5678` in WSL (or on any other
//! host) and point a Windows build at `tcp://localhost:5678`. A generic-VISCA
//! camera sends the same raw bytes over IP as over RS-232 — right down to the
//! `0x81` address, since VISCA over IP is fixed at device 1 — so everything
//! below the transport is shared.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

#[cfg(unix)]
use nix::fcntl::OFlag;
#[cfg(unix)]
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};

const TERMINATOR: u8 = 0xFF;
const OUR_ADDRESS: u8 = 0x81;

/// How many characters the camera's title holds, and the code it pads them
/// with — the camera's own character set, shared with the app that sends it.
const TITLE_LENGTH: usize = viscous::title::LENGTH;
const SPACE: u8 = 0x1B;

/// Simulated pan/tilt speed at full VISCA speed, in raw position units per
/// second. Chosen so a brief tap of a control moves visibly but a large preset
/// recall takes a few visible seconds — enough to exercise "in progress"
/// handling without making manual testing tedious.
const PAN_TILT_UNITS_PER_SEC: f64 = 300.0;

/// Simulated zoom/focus drive speed, in raw position units per second.
const ZOOM_FOCUS_UNITS_PER_SEC: f64 = 2000.0;

/// How far each axis can travel, in raw units. Movement stops at these
/// bounds rather than wrapping or running away, as it would on real hardware;
/// the values are `GenericVisca`'s pan/tilt ranges and zoom/focus limits.
const PAN_LIMITS: (f64, f64) = (-2880.0, 2880.0);
const TILT_LIMITS: (f64, f64) = (-1440.0, 1440.0);
const ZOOM_LIMITS: (f64, f64) = (0.0, 65535.0);
const FOCUS_LIMITS: (f64, f64) = (4096.0, 57344.0);

/// The fastest pan and tilt speeds a VISCA drive command can ask for
/// (0x18/0x14), restated here rather than shared with the client so the
/// simulator stays an independent check on what the client sends.
const MAX_PAN_SPEED: u8 = 24;
const MAX_TILT_SPEED: u8 = 20;

/// A saved preset: enough state to make recall visibly restore something,
/// and take a simulated amount of time doing it.
#[derive(Debug, Clone, Copy)]
struct Preset {
    pan: i16,
    tilt: i16,
    zoom: u16,
    focus: u16,
}

/// How long a woken camera takes to come up, during which it answers for its
/// power and nothing else — and how long its power-on command takes to
/// complete, since the camera answers that one once it is actually up.
///
/// The real EVI-D80 takes about nine seconds, and this used to be three: long
/// enough to see, short enough not to sit through, and short enough that a
/// client giving up after five seconds looked like it worked. That is exactly
/// the bug it was meant to catch, so it is the camera's figure now.
const WAKE_TIME: Duration = Duration::from_secs(9);

struct CameraSim {
    power_on: bool,
    /// When the camera will have finished waking, if it is still doing so.
    awake_at: Option<Instant>,
    auto_focus: bool,
    /// The title the camera is holding, in its own character codes, and
    /// whether it's currently burned into the video output.
    title: [u8; TITLE_LENGTH],
    title_shown: bool,
    zoom: u16,
    focus: u16,
    pan: i16,
    tilt: i16,
    /// The rate each axis is currently being driven at, in raw units per
    /// second, and when the positions above were last brought up to date with
    /// those rates. Zero when the axis isn't being driven.
    pan_rate: f64,
    tilt_rate: f64,
    zoom_rate: f64,
    focus_rate: f64,
    last_advance: Instant,
    presets: HashMap<u8, Preset>,
}

/// How far a single pan/tilt relative-move command nudges simulated state
/// per unit of requested degrees-equivalent (see `nibbles_to_i16`).
const PAN_TILT_SCALE: i16 = 1;

/// How long simulated travel across `distance` raw units takes at
/// `units_per_sec`.
fn travel_time(distance: i32, units_per_sec: f64) -> Duration {
    Duration::from_secs_f64(f64::from(distance.unsigned_abs()) / units_per_sec)
}

/// Where an axis being driven at `rate` units per second ends up after
/// `seconds`, stopping at either end of its travel.
fn advanced(position: f64, rate: f64, seconds: f64, limits: (f64, f64)) -> f64 {
    (position + rate * seconds).clamp(limits.0, limits.1)
}

/// Which way one axis of a drive command was told to move.
///
/// VISCA gives each axis its own direction byte, where 0x03 means "leave this
/// axis alone" — and the two axes disagree about which of 0x01/0x02 is the
/// positive one, so the caller says which byte counts as positive (pan 0x01 is
/// left, tilt 0x01 is up).
fn axis_sign(direction: u8, positive: u8) -> f64 {
    match direction {
        _ if direction == positive => 1.0,
        0x01 | 0x02 => -1.0,
        _ => 0.0,
    }
}

/// How fast a variable zoom or focus drive runs, as a fraction of this
/// camera's top lens speed.
///
/// VISCA carries the speed in the low nibble of the `2p`/`3p` forms, 0 to 7,
/// where **0 is the slowest setting the camera has and not a stop** — stopping
/// is its own command (`00`). So nothing here comes out zero: a simulated
/// camera that stood still at the slowest notch would be evidence for exactly
/// the wrong conclusion. The standard-speed forms (`02`/`03`) carry no speed
/// at all and leave the pace to the camera, so they land in the middle.
fn lens_speed_fraction(command: u8) -> f64 {
    let level = if command >= 0x20 {
        f64::from(command & 0x0F)
    } else {
        4.0
    };
    (level + 1.0) / 8.0
}

/// A drive command's rate along one axis, in raw units per second: its
/// direction, at a speed scaled from VISCA's `1..=max_speed` range onto the
/// simulated top speed.
fn drive_rate(direction: u8, positive: u8, speed: u8, max_speed: u8) -> f64 {
    axis_sign(direction, positive) * PAN_TILT_UNITS_PER_SEC * f64::from(speed.clamp(1, max_speed))
        / f64::from(max_speed)
}

impl CameraSim {
    fn new() -> Self {
        Self {
            power_on: true,
            awake_at: None,
            auto_focus: false,
            title: [SPACE; TITLE_LENGTH],
            title_shown: false,
            zoom: 0x0000,
            focus: 0x1000,
            pan: 0,
            tilt: 0,
            pan_rate: 0.0,
            tilt_rate: 0.0,
            zoom_rate: 0.0,
            focus_rate: 0.0,
            last_advance: Instant::now(),
            // Seeded a few presets away from the origin, at varied
            // distances, so recalling them (1, 2, or 3) demonstrates a
            // visible range of simulated travel times; 4-6 are left unset,
            // which recalls instantly as a no-op, same as a real camera
            // asked to recall a preset that was never saved.
            presets: HashMap::from([
                (
                    0,
                    Preset {
                        pan: 400,
                        tilt: 150,
                        zoom: 0x1000,
                        focus: 0x1800,
                    },
                ),
                (
                    1,
                    Preset {
                        pan: -800,
                        tilt: -300,
                        zoom: 0x2000,
                        focus: 0x0800,
                    },
                ),
                (
                    2,
                    Preset {
                        pan: 200,
                        tilt: -100,
                        zoom: 0x0800,
                        focus: 0x1000,
                    },
                ),
            ]),
        }
    }

    /// Brings simulated positions up to date with however long the drives
    /// currently running have been running for.
    ///
    /// A continuous drive has no distance of its own — it runs until something
    /// stops it — so the only way to simulate one is to integrate its rate over
    /// real elapsed time. Doing that before every message means a position
    /// inquiry answers with where the camera would actually be by now.
    fn advance(&mut self) {
        let seconds = self.last_advance.elapsed().as_secs_f64();
        self.last_advance = Instant::now();

        self.pan = advanced(f64::from(self.pan), self.pan_rate, seconds, PAN_LIMITS) as i16;
        self.tilt = advanced(f64::from(self.tilt), self.tilt_rate, seconds, TILT_LIMITS) as i16;
        self.zoom = advanced(f64::from(self.zoom), self.zoom_rate, seconds, ZOOM_LIMITS) as u16;
        self.focus = advanced(
            f64::from(self.focus),
            self.focus_rate,
            seconds,
            FOCUS_LIMITS,
        ) as u16;
    }

    /// Handles one complete VISCA message (including its trailing
    /// terminator). Logs what it received and returns the reply to send, if
    /// any.
    fn handle(&mut self, message: &[u8]) -> Reply {
        self.advance();

        if message.len() < 3 || message[0] != OUR_ADDRESS {
            // Not addressed to us (e.g. the broadcast I/F Clear the client
            // sends on connect) — nothing to reply to.
            println!("RECV: {} (ignored)", describe_message(message));
            return Reply::None;
        }

        println!("RECV: {}", describe_message(message));

        let body = &message[2..message.len() - 1];
        if !self.is_awake_enough_for(body) {
            println!("      (refused: the camera is not awake)");
            return Reply::Immediate(not_executable_reply());
        }

        match message[1] {
            0x09 => match self.handle_inquiry(body) {
                Some(bytes) => Reply::Immediate(bytes),
                None => Reply::None,
            },
            0x01 => {
                let delay = self.apply_command(body);
                Reply::Command {
                    ack: ack_reply(1),
                    delay,
                    completion: completion_reply(1),
                }
            }
            _ => Reply::None,
        }
    }

    /// Whether the camera will deal with this message at all in its current
    /// state.
    ///
    /// A camera in standby has parked its lens and powered down everything
    /// that would answer, and so has one that is part-way through waking up.
    /// What both still answer is the handful of things that would otherwise
    /// leave no way back: what model it is, whether it is on, and being told
    /// to switch. Everything else is refused outright — which is the whole
    /// point of simulating standby, since a client that is never refused can
    /// never be shown to handle it.
    fn is_awake_enough_for(&self, body: &[u8]) -> bool {
        let always_answered = matches!(
            body,
            // Version inquiry: answered even asleep, so connecting to a
            // camera that happens to be in standby still works.
            [0x00, 0x02]
                // Power inquiry, and the command to switch it either way.
                | [0x04, 0x00]
                | [0x04, 0x00, 0x02 | 0x03]
        );
        always_answered || (self.power_on && self.finished_waking())
    }

    /// Whether enough time has passed since being woken for the camera to
    /// answer for its lens.
    fn finished_waking(&self) -> bool {
        self.awake_at.is_none_or(|at| Instant::now() >= at)
    }

    fn handle_inquiry(&self, body: &[u8]) -> Option<Vec<u8>> {
        match body {
            // Version inquiry: fake but plausible vendor/model/rom/socket.
            [0x00, 0x02] => Some(inquiry_reply(&[0x00, 0x01, 0x00, 0x02, 0x01, 0x00, 0x02])),
            // Power inquiry.
            [0x04, 0x00] => {
                let status = if self.power_on { 0x02 } else { 0x03 };
                Some(inquiry_reply(&[status]))
            }
            // Focus mode inquiry.
            [0x04, 0x38] => {
                let mode = if self.auto_focus { 0x02 } else { 0x03 };
                Some(inquiry_reply(&[mode]))
            }
            // Title display inquiry.
            [0x04, 0x74] => {
                let shown = if self.title_shown { 0x02 } else { 0x03 };
                Some(inquiry_reply(&[shown]))
            }
            // Zoom position inquiry.
            [0x04, 0x47] => Some(inquiry_reply(&nibbles_u16(self.zoom))),
            // Focus position inquiry.
            [0x04, 0x48] => Some(inquiry_reply(&nibbles_u16(self.focus))),
            // Pan/tilt position inquiry: 4 nibbles pan, then 4 nibbles tilt.
            [0x06, 0x12] => {
                let mut reply = nibbles_u16(self.pan as u16).to_vec();
                reply.extend(nibbles_u16(self.tilt as u16));
                Some(inquiry_reply(&reply))
            }
            _ => None,
        }
    }

    /// Updates simulated state for a command body (bytes after the `0x01`
    /// command-vs-inquiry marker, up to but excluding the terminator), and
    /// returns how long that change should simulate taking to complete.
    fn apply_command(&mut self, body: &[u8]) -> Duration {
        match body {
            // Pan/tilt drive: 06 01 VV WW XX YY. A velocity the camera holds
            // until it's told otherwise, so it completes at once and the
            // movement it starts shows up in `advance` instead of here.
            [
                0x06,
                0x01,
                pan_speed,
                tilt_speed,
                pan_direction,
                tilt_direction,
            ] => {
                self.pan_rate = drive_rate(*pan_direction, 0x02, *pan_speed, MAX_PAN_SPEED);
                self.tilt_rate = drive_rate(*tilt_direction, 0x01, *tilt_speed, MAX_TILT_SPEED);
                Duration::ZERO
            }
            // Pan/tilt relative move: 06 03 VV WW <4 nibbles pan> <4 nibbles tilt>
            [0x06, 0x03, _pan_speed, _tilt_speed, rest @ ..] if rest.len() == 8 => {
                let pan_delta = nibbles_to_i16(&rest[0..4]).saturating_mul(PAN_TILT_SCALE);
                let tilt_delta = nibbles_to_i16(&rest[4..8]).saturating_mul(PAN_TILT_SCALE);
                self.pan = self.pan.saturating_add(pan_delta);
                self.tilt = self.tilt.saturating_add(tilt_delta);
                pan_tilt_travel_time(i32::from(pan_delta), i32::from(tilt_delta))
            }
            // Pan/tilt home.
            [0x06, 0x04] => {
                let delay = pan_tilt_travel_time(-i32::from(self.pan), -i32::from(self.tilt));
                self.pan = 0;
                self.tilt = 0;
                delay
            }
            // Pan/tilt reset: recalibrates, which on a real camera means a
            // full sweep to find the limits before returning to home.
            [0x06, 0x05] => {
                let sweep = pan_tilt_travel_time(PAN_LIMITS.1 as i32, TILT_LIMITS.1 as i32);
                let home = pan_tilt_travel_time(-i32::from(self.pan), -i32::from(self.tilt));
                self.pan = 0;
                self.tilt = 0;
                sweep + home
            }
            // Power on / off (standby). Waking is not instant: the camera
            // acknowledges at once, spends a few seconds coming up — answering
            // for its power alone in the meantime — and only then completes
            // the command that woke it. Going to sleep is immediate.
            [0x04, 0x00, on @ (0x02 | 0x03)] => {
                let waking = *on == 0x02 && !self.power_on;
                self.power_on = *on == 0x02;
                self.awake_at = waking.then(|| Instant::now() + WAKE_TIME);
                // A camera going into standby parks its lens, so whatever was
                // being driven stops with it.
                if !self.power_on {
                    self.pan_rate = 0.0;
                    self.tilt_rate = 0.0;
                    self.zoom_rate = 0.0;
                    self.focus_rate = 0.0;
                }
                if waking { WAKE_TIME } else { Duration::ZERO }
            }
            // Title set: 73 pp <10 bytes>. Part 00 is where the title goes,
            // 01 and 02 are its characters, ten at a time.
            [0x04, 0x73, part, characters @ ..] if characters.len() == 10 => {
                let offset = usize::from(part.saturating_sub(1)) * 10;
                if (0x01..=0x02).contains(part) {
                    self.title[offset..offset + 10].copy_from_slice(characters);
                }
                Duration::ZERO
            }
            // Title display: clear, on, off.
            [0x04, 0x74, action] => {
                match action {
                    0x00 => self.title = [SPACE; TITLE_LENGTH],
                    0x02 => self.title_shown = true,
                    _ => self.title_shown = false,
                }
                Duration::ZERO
            }
            // Auto / manual focus.
            [0x04, 0x38, auto @ (0x02 | 0x03)] => {
                self.auto_focus = *auto == 0x02;
                // Handing focus back to the camera ends any manual drive, the
                // same way the camera's own focusing would override it.
                if self.auto_focus {
                    self.focus_rate = 0.0;
                }
                Duration::ZERO
            }
            // Zoom stop.
            [0x04, 0x07, 0x00] => {
                self.zoom_rate = 0.0;
                Duration::ZERO
            }
            // Zoom tele (in), with or without an explicit speed nibble.
            [0x04, 0x07, b] if *b == 0x02 || (0x20..=0x2F).contains(b) => {
                self.zoom_rate = ZOOM_FOCUS_UNITS_PER_SEC * lens_speed_fraction(*b);
                Duration::ZERO
            }
            // Zoom wide (out), with or without an explicit speed nibble.
            [0x04, 0x07, b] if *b == 0x03 || (0x30..=0x3F).contains(b) => {
                self.zoom_rate = -ZOOM_FOCUS_UNITS_PER_SEC * lens_speed_fraction(*b);
                Duration::ZERO
            }
            // Focus stop.
            [0x04, 0x08, 0x00] => {
                self.focus_rate = 0.0;
                Duration::ZERO
            }
            // Focus far, with or without an explicit speed nibble (0x02, or 0x20..=0x2F).
            [0x04, 0x08, b] if *b == 0x02 || (0x20..=0x2F).contains(b) => {
                self.focus_rate = ZOOM_FOCUS_UNITS_PER_SEC * lens_speed_fraction(*b);
                Duration::ZERO
            }
            // Focus near, with or without an explicit speed nibble (0x03, or 0x30..=0x3F).
            [0x04, 0x08, b] if *b == 0x03 || (0x30..=0x3F).contains(b) => {
                self.focus_rate = -ZOOM_FOCUS_UNITS_PER_SEC * lens_speed_fraction(*b);
                Duration::ZERO
            }
            // Preset: 04 3F action preset_number
            [0x04, 0x3F, action, number] => self.apply_preset(*action, *number),
            _ => Duration::ZERO,
        }
    }

    fn apply_preset(&mut self, action: u8, number: u8) -> Duration {
        match action {
            0x00 => {
                self.presets.remove(&number);
                Duration::ZERO
            }
            0x01 => {
                self.presets.insert(
                    number,
                    Preset {
                        pan: self.pan,
                        tilt: self.tilt,
                        zoom: self.zoom,
                        focus: self.focus,
                    },
                );
                Duration::ZERO
            }
            0x02 => {
                let Some(preset) = self.presets.get(&number).copied() else {
                    return Duration::ZERO;
                };
                let delay = pan_tilt_travel_time(
                    i32::from(preset.pan) - i32::from(self.pan),
                    i32::from(preset.tilt) - i32::from(self.tilt),
                )
                .max(travel_time(
                    i32::from(preset.zoom) - i32::from(self.zoom),
                    ZOOM_FOCUS_UNITS_PER_SEC,
                ))
                .max(travel_time(
                    i32::from(preset.focus) - i32::from(self.focus),
                    ZOOM_FOCUS_UNITS_PER_SEC,
                ));
                self.pan = preset.pan;
                self.tilt = preset.tilt;
                self.zoom = preset.zoom;
                self.focus = preset.focus;
                delay
            }
            _ => Duration::ZERO,
        }
    }
}

/// How long pan and tilt take to settle when moving simultaneously: as long
/// as whichever axis has farther to go, not the sum of both.
fn pan_tilt_travel_time(pan_distance: i32, tilt_distance: i32) -> Duration {
    travel_time(pan_distance, PAN_TILT_UNITS_PER_SEC)
        .max(travel_time(tilt_distance, PAN_TILT_UNITS_PER_SEC))
}

/// What to send back for one handled message.
enum Reply {
    /// Nothing to send (an unaddressed or unrecognized message).
    None,
    /// A single reply, sent right away (inquiries).
    Immediate(Vec<u8>),
    /// A command's ACK, sent right away, followed by its Completion after a
    /// simulated travel delay.
    Command {
        ack: Vec<u8>,
        delay: Duration,
        completion: Vec<u8>,
    },
}

/// Splits `value` into 4 nibbles, matching grafton-visca's `Nibbles::u16_quad`
/// decoding: `(n[0]<<12)|(n[1]<<8)|(n[2]<<4)|n[3]`.
fn nibbles_u16(value: u16) -> [u8; 4] {
    [
        ((value >> 12) & 0xF) as u8,
        ((value >> 8) & 0xF) as u8,
        ((value >> 4) & 0xF) as u8,
        (value & 0xF) as u8,
    ]
}

/// Inverse of `nibbles_u16`, reinterpreted as signed (two's complement),
/// matching `Nibbles::i16_quad`.
fn nibbles_to_i16(nibbles: &[u8]) -> i16 {
    let value = ((u16::from(nibbles[0]) & 0xF) << 12)
        | ((u16::from(nibbles[1]) & 0xF) << 8)
        | ((u16::from(nibbles[2]) & 0xF) << 4)
        | (u16::from(nibbles[3]) & 0xF);
    value as i16
}

fn inquiry_reply(payload: &[u8]) -> Vec<u8> {
    let mut reply = vec![0x90, 0x50];
    reply.extend_from_slice(payload);
    reply.push(TERMINATOR);
    reply
}

/// What a camera says to something it won't do in its current state: VISCA
/// error 0x41, "command not executable".
fn not_executable_reply() -> Vec<u8> {
    vec![0x90, 0x60, 0x41, TERMINATOR]
}

fn ack_reply(socket: u8) -> Vec<u8> {
    vec![0x90, 0x40 | socket, TERMINATOR]
}

fn completion_reply(socket: u8) -> Vec<u8> {
    vec![0x90, 0x50 | socket, TERMINATOR]
}

/// A short human description of a received message, for the log.
fn describe_message(message: &[u8]) -> String {
    if message.len() < 3 {
        return format!("{message:02X?} (too short to be a valid VISCA message)");
    }
    let addr = message[0];
    let body = &message[2..message.len() - 1];
    let kind = match (message.get(1), body) {
        (Some(0x01), [0x06, 0x01, pan_speed, tilt_speed, 0x03, 0x03]) => {
            format!("pan/tilt stop (speeds {pan_speed}/{tilt_speed})")
        }
        (Some(0x01), [0x06, 0x01, pan_speed, tilt_speed, ..]) => {
            format!("pan/tilt drive at {pan_speed}/{tilt_speed}")
        }
        (Some(0x01), [0x06, 0x03, ..]) => "pan/tilt relative move".to_string(),
        (Some(0x01), [0x06, 0x04]) => "pan/tilt home".to_string(),
        (Some(0x01), [0x06, 0x05]) => "pan/tilt reset".to_string(),
        (Some(0x01), [0x04, 0x00, 0x02]) => "power on".to_string(),
        (Some(0x01), [0x04, 0x00, 0x03]) => "power off".to_string(),
        (Some(0x01), [0x04, 0x73, part @ (0x01 | 0x02), characters @ ..])
            if characters.len() == 10 =>
        {
            let first = (part - 1) * 10 + 1;
            format!(
                "title characters {first}-{} \"{}\"",
                first + 9,
                viscous::title::decode(characters)
            )
        }
        (Some(0x01), [0x04, 0x73, 0x00, ..]) => "title appearance".to_string(),
        (Some(0x01), [0x04, 0x74, 0x00]) => "title clear".to_string(),
        (Some(0x01), [0x04, 0x74, 0x02]) => "title display on".to_string(),
        (Some(0x01), [0x04, 0x74, 0x03]) => "title display off".to_string(),
        (Some(0x01), [0x04, 0x38, 0x02]) => "auto focus".to_string(),
        (Some(0x01), [0x04, 0x38, 0x03]) => "manual focus".to_string(),
        (Some(0x01), [0x04, 0x07, 0x00]) => "zoom stop".to_string(),
        (Some(0x01), [0x04, 0x07, b]) if *b == 0x02 || (0x20..=0x2F).contains(b) => {
            "zoom tele (in)".to_string()
        }
        (Some(0x01), [0x04, 0x07, b]) if *b == 0x03 || (0x30..=0x3F).contains(b) => {
            "zoom wide (out)".to_string()
        }
        (Some(0x01), [0x04, 0x08, 0x00]) => "focus stop".to_string(),
        (Some(0x01), [0x04, 0x08, b]) if *b == 0x02 || (0x20..=0x2F).contains(b) => {
            "focus far".to_string()
        }
        (Some(0x01), [0x04, 0x08, b]) if *b == 0x03 || (0x30..=0x3F).contains(b) => {
            "focus near".to_string()
        }
        (Some(0x01), [0x04, 0x3F, action, number]) => {
            let action_name = match action {
                0x00 => "reset",
                0x01 => "set",
                0x02 => "recall",
                _ => "unknown action",
            };
            format!("preset {action_name} {number}")
        }
        (Some(0x01), _) => "command (unrecognized)".to_string(),
        (Some(0x09), [0x00, 0x02]) => "version inquiry".to_string(),
        (Some(0x09), [0x04, 0x00]) => "power inquiry".to_string(),
        (Some(0x09), [0x04, 0x38]) => "focus mode inquiry".to_string(),
        (Some(0x09), [0x04, 0x74]) => "title display inquiry".to_string(),
        (Some(0x09), [0x04, 0x47]) => "zoom position inquiry".to_string(),
        (Some(0x09), [0x04, 0x48]) => "focus position inquiry".to_string(),
        (Some(0x09), [0x06, 0x12]) => "pan/tilt position inquiry".to_string(),
        (Some(0x09), _) => "inquiry (unrecognized)".to_string(),
        _ => "unrecognized message".to_string(),
    };
    format!("addr=0x{addr:02X} {kind} {message:02X?}")
}

/// A short human description of a reply, for the log.
fn describe_reply(reply: &[u8]) -> String {
    format!("{reply:02X?}")
}

/// Writes `bytes` to `master`, logging the outcome with `label`. Returns
/// whether the write succeeded (a failure likely just means the client
/// disconnected, not that the simulator should exit).
fn send(master: &mut impl Write, label: &str, bytes: &[u8]) -> bool {
    match master.write_all(bytes).and_then(|()| master.flush()) {
        Ok(()) => {
            println!("SEND ({label}): {}", describe_reply(bytes));
            true
        }
        Err(error) => {
            println!("SEND failed (client likely disconnected): {error}");
            false
        }
    }
}

/// Anything the simulator can read VISCA bytes from and write replies to —
/// a self-provisioned pty (Unix), a named port opened directly (any platform,
/// including a Windows com0com pair), or a TCP client.
trait Transport: Read + Write {}
impl<T: Read + Write + ?Sized> Transport for T {}

/// The prefix that asks for a TCP listener instead of a serial port.
const TCP_SCHEME: &str = "tcp://";

/// The port a real generic-VISCA camera listens on for VISCA over IP, and so
/// the one to default to here — a client that leaves the port off its own
/// address lands on the same number.
const DEFAULT_TCP_PORT: u16 = 5678;

/// A TCP listener that serves one client at a time, waiting for the next as
/// soon as the current one goes away.
///
/// One at a time because a real camera is one camera: two clients driving it
/// at once is the client's problem to avoid, not something to simulate away.
/// But the connection itself has to be disposable — `viscous` reconnects
/// during startup, and a GUI can disconnect and reconnect at any point — so
/// the listener outlives any single client.
struct TcpPort {
    listener: TcpListener,
    client: Option<TcpStream>,
}

impl TcpPort {
    /// The client currently being served, waiting for one to connect if there
    /// isn't one.
    fn client(&mut self) -> io::Result<&mut TcpStream> {
        if self.client.is_none() {
            let (client, peer) = self.listener.accept()?;
            println!("Client connected from {peer}");
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("a client just connected"))
    }
}

impl Read for TcpPort {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.client()?.read(buffer);
        // End of stream or an error both mean this client is finished with;
        // dropping it here is what lets the next read wait for a new one
        // instead of spinning on a dead socket.
        if !matches!(read, Ok(1..)) {
            println!("Client disconnected");
            self.client = None;
        }
        read
    }
}

impl Write for TcpPort {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // Deliberately not `client()`: a reply with nobody left to hear it
        // should fail, not wait for the next client and then answer a question
        // that client never asked.
        match &mut self.client {
            Some(client) => client.write(buffer),
            None => Err(io::Error::other("no client connected")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.client {
            Some(client) => client.flush(),
            None => Ok(()),
        }
    }
}

/// Where a `tcp://...` argument says to listen.
///
/// A bare port — or nothing after the scheme at all — binds every interface
/// rather than loopback: the whole reason to run this over TCP instead of a
/// pty is to be reachable from somewhere else (a Windows build talking to a
/// camera in WSL), and a loopback-only bind defeats that silently.
fn bind_address(argument: &str) -> String {
    match argument {
        "" => format!("0.0.0.0:{DEFAULT_TCP_PORT}"),
        port if !port.contains(':') => format!("0.0.0.0:{port}"),
        address => address.to_string(),
    }
}

fn open_tcp(argument: &str) -> io::Result<Box<dyn Transport>> {
    let listener = TcpListener::bind(bind_address(argument))?;
    let bound = listener.local_addr()?;

    println!("Fake camera listening on tcp://{bound}");
    println!(
        "In another terminal, run: cargo run -- tcp://127.0.0.1:{}",
        bound.port()
    );
    println!(
        "From a Windows build against this camera running in WSL, connect to \
         tcp://localhost:{} — WSL forwards it.",
        bound.port()
    );

    Ok(Box::new(TcpPort {
        listener,
        client: None,
    }))
}

fn open_serial(port_name: &str) -> io::Result<Box<dyn Transport>> {
    let port = serialport::new(port_name, 9600)
        .timeout(Duration::from_millis(500))
        .open()
        .map_err(io::Error::other)?;
    println!("Fake camera listening on {port_name}");
    Ok(Box::new(port))
}

/// Provisions a pty pair and simulates the camera on the master end. Unix
/// only — there's no equivalent auto-provisioning trick on Windows.
fn open_pty() -> io::Result<Box<dyn Transport>> {
    #[cfg(unix)]
    {
        let master = posix_openpt(OFlag::O_RDWR).expect("posix_openpt should succeed");
        grantpt(&master).expect("grantpt should succeed");
        unlockpt(&master).expect("unlockpt should succeed");
        let slave_path = ptsname_r(&master).expect("ptsname_r should succeed");

        println!("Fake camera listening on {slave_path}");
        println!("In another terminal, run: cargo run -- {slave_path}");

        Ok(Box::new(master))
    }

    #[cfg(not(unix))]
    {
        Err(io::Error::other(
            "no target given, and this platform can't provision a serial port automatically \
             (that's a Unix-only pty trick) — either run: cargo run --example fake_camera -- \
             tcp://5678 (and point viscous/the GUI at tcp://localhost:5678), or install com0com \
             (https://com0com.sourceforge.net/), create a port pair (e.g. COM10 <-> COM11), then \
             run: cargo run --example fake_camera -- COM10 (and point viscous/the GUI at COM11)",
        ))
    }
}

/// Opens the transport to simulate the camera on, from the command line
/// argument naming it.
fn open_transport(target: Option<&str>) -> io::Result<Box<dyn Transport>> {
    match target {
        Some(target) => match target.strip_prefix(TCP_SCHEME) {
            Some(address) => open_tcp(address),
            None => open_serial(target),
        },
        None => open_pty(),
    }
}

fn main() -> io::Result<()> {
    let target = std::env::args().nth(1);
    let mut transport = open_transport(target.as_deref())?;

    let mut camera = CameraSim::new();
    let mut message = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        // A pty master reads as EIO once every slave-side file descriptor is
        // closed (rather than blocking until a new one opens), a named port's
        // read simply times out while nothing's connected, and a TCP read
        // ends at the moment the client hangs up. viscous itself disconnects
        // and reconnects during serial startup (a short discovery probe, then
        // a fresh connection for the real session), so treat any read failure
        // as "no client right now" and wait for the next one instead of
        // exiting.
        if transport.read_exact(&mut byte).is_err() {
            message.clear();
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        message.push(byte[0]);
        if byte[0] != TERMINATOR {
            continue;
        }

        match camera.handle(&message) {
            Reply::None => println!("SEND: (no reply)"),
            Reply::Immediate(bytes) => {
                send(&mut transport, "reply", &bytes);
            }
            Reply::Command {
                ack,
                delay,
                completion,
            } => {
                if send(&mut transport, "ack", &ack) {
                    if !delay.is_zero() {
                        println!("(simulating {delay:?} of travel time)");
                    }
                    std::thread::sleep(delay);
                    send(&mut transport, "completion", &completion);
                }
            }
        }
        message.clear();
    }
}
