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
//! Position-changing commands (pan/tilt move, pan/tilt home, preset recall)
//! reply with their ACK immediately but delay their Completion by a
//! simulated travel time — proportional to the distance moved, at a fixed
//! simulated speed — so the client's "in progress" handling actually has
//! something to exercise instead of every command resolving instantly. A
//! few presets are pre-seeded at varied distances from the origin for this.
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

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::time::Duration;

#[cfg(unix)]
use nix::fcntl::OFlag;
#[cfg(unix)]
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};

const TERMINATOR: u8 = 0xFF;
const OUR_ADDRESS: u8 = 0x81;

/// Simulated pan/tilt speed, in raw position units per second (a single
/// 2-degree nudge is about 29 raw units). Chosen so one nudge settles in a
/// fraction of a second but a large preset recall takes a few visible
/// seconds — enough to exercise "in progress" handling without making
/// manual testing tedious.
const PAN_TILT_UNITS_PER_SEC: f64 = 300.0;

/// Simulated zoom/focus speed, in raw position units per second. Zoom and
/// focus commands are continuous drives with no distance of their own (the
/// client decides how long to drive by holding the command open), so this
/// only matters for preset recall, which jumps straight to a target
/// position on all four axes at once.
const ZOOM_FOCUS_UNITS_PER_SEC: f64 = 2000.0;

/// A saved preset: enough state to make recall visibly restore something,
/// and take a simulated amount of time doing it.
#[derive(Debug, Clone, Copy)]
struct Preset {
    pan: i16,
    tilt: i16,
    zoom: u16,
    focus: u16,
}

struct CameraSim {
    power_on: bool,
    zoom: u16,
    focus: u16,
    pan: i16,
    tilt: i16,
    presets: HashMap<u8, Preset>,
}

/// How far a single zoom/focus start command nudges simulated state, since
/// this simulator answers instantly rather than tracking real elapsed drive
/// time between start and stop.
const ZOOM_FOCUS_STEP: u16 = 0x0400;
/// How far a single pan/tilt relative-move command nudges simulated state
/// per unit of requested degrees-equivalent (see `nibbles_to_i16`).
const PAN_TILT_SCALE: i16 = 1;

/// How long simulated travel across `distance` raw units takes at
/// `units_per_sec`.
fn travel_time(distance: i32, units_per_sec: f64) -> Duration {
    Duration::from_secs_f64(f64::from(distance.unsigned_abs()) / units_per_sec)
}

impl CameraSim {
    fn new() -> Self {
        Self {
            power_on: true,
            zoom: 0x0000,
            focus: 0x1000,
            pan: 0,
            tilt: 0,
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

    /// Handles one complete VISCA message (including its trailing
    /// terminator). Logs what it received and returns the reply to send, if
    /// any.
    fn handle(&mut self, message: &[u8]) -> Reply {
        if message.len() < 3 || message[0] != OUR_ADDRESS {
            // Not addressed to us (e.g. the broadcast I/F Clear the client
            // sends on connect) — nothing to reply to.
            println!("RECV: {} (ignored)", describe_message(message));
            return Reply::None;
        }

        println!("RECV: {}", describe_message(message));

        match message[1] {
            0x09 => match self.handle_inquiry(&message[2..message.len() - 1]) {
                Some(bytes) => Reply::Immediate(bytes),
                None => Reply::None,
            },
            0x01 => {
                let delay = self.apply_command(&message[2..message.len() - 1]);
                Reply::Command {
                    ack: ack_reply(1),
                    delay,
                    completion: completion_reply(1),
                }
            }
            _ => Reply::None,
        }
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
            // Zoom stop: no state change.
            [0x04, 0x07, 0x00] => Duration::ZERO,
            // Zoom tele (in).
            [0x04, 0x07, 0x02] => {
                self.zoom = self.zoom.saturating_add(ZOOM_FOCUS_STEP);
                Duration::ZERO
            }
            // Zoom wide (out).
            [0x04, 0x07, 0x03] => {
                self.zoom = self.zoom.saturating_sub(ZOOM_FOCUS_STEP);
                Duration::ZERO
            }
            // Focus stop: no state change.
            [0x04, 0x08, 0x00] => Duration::ZERO,
            // Focus far, with or without an explicit speed nibble (0x02, or 0x20..=0x2F).
            [0x04, 0x08, b] if *b == 0x02 || (0x20..=0x2F).contains(b) => {
                self.focus = self.focus.saturating_add(ZOOM_FOCUS_STEP);
                Duration::ZERO
            }
            // Focus near, with or without an explicit speed nibble (0x03, or 0x30..=0x3F).
            [0x04, 0x08, b] if *b == 0x03 || (0x30..=0x3F).contains(b) => {
                self.focus = self.focus.saturating_sub(ZOOM_FOCUS_STEP);
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
        (Some(0x01), [0x06, 0x03, ..]) => "pan/tilt relative move".to_string(),
        (Some(0x01), [0x06, 0x04]) => "pan/tilt home".to_string(),
        (Some(0x01), [0x04, 0x07, 0x00]) => "zoom stop".to_string(),
        (Some(0x01), [0x04, 0x07, 0x02]) => "zoom tele (in)".to_string(),
        (Some(0x01), [0x04, 0x07, 0x03]) => "zoom wide (out)".to_string(),
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
/// either a self-provisioned pty (Unix) or a named port opened directly
/// (any platform, including a Windows com0com pair).
trait Transport: Read + Write {}
impl<T: Read + Write + ?Sized> Transport for T {}

/// Opens the transport to simulate the camera on: a named port if one was
/// given on the command line (works everywhere, including a com0com pair on
/// Windows), otherwise a self-provisioned pty pair (Unix only — there's no
/// equivalent auto-provisioning trick on Windows).
fn open_transport(port_name: Option<&str>) -> io::Result<Box<dyn Transport>> {
    if let Some(port_name) = port_name {
        let port = serialport::new(port_name, 9600)
            .timeout(Duration::from_millis(500))
            .open()
            .map_err(io::Error::other)?;
        println!("Fake camera listening on {port_name}");
        return Ok(Box::new(port));
    }

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
            "no port given, and this platform can't provision one automatically (that's a \
             Unix-only pty trick) — install com0com (https://com0com.sourceforge.net/), create \
             a port pair (e.g. COM10 <-> COM11), then run: cargo run --example fake_camera -- \
             COM10 (and point viscous/the GUI at COM11)",
        ))
    }
}

fn main() -> io::Result<()> {
    let port_name = std::env::args().nth(1);
    let mut transport = open_transport(port_name.as_deref())?;

    let mut camera = CameraSim::new();
    let mut message = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        // A pty master reads as EIO once every slave-side file descriptor is
        // closed (rather than blocking until a new one opens), and a named
        // port's read simply times out while nothing's connected. viscous
        // itself disconnects and reconnects during startup (a short
        // discovery probe, then a fresh connection for the real session),
        // so treat any read failure as "no client right now" and wait for
        // the next one instead of exiting.
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
