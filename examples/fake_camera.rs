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
//! Command/inquiry byte layouts below are traced from grafton-visca's own
//! source (`command::pan_tilt`, `command::zoom`, `command::focus`,
//! `command::preset`, `command::inquiry_structs`), not guessed.
//!
//! Run with `cargo run --example fake_camera`, then in another terminal:
//! `cargo run -- <the printed slave path>`.

use std::collections::HashMap;
use std::io::{Read, Write};

use nix::fcntl::OFlag;
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};

const TERMINATOR: u8 = 0xFF;
const OUR_ADDRESS: u8 = 0x81;

/// A saved preset: enough state to make recall visibly restore something.
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

impl CameraSim {
    fn new() -> Self {
        Self {
            power_on: true,
            zoom: 0x0000,
            focus: 0x1000,
            pan: 0,
            tilt: 0,
            presets: HashMap::new(),
        }
    }

    /// Handles one complete VISCA message (including its trailing
    /// terminator). Logs what it received and what (if anything) it's
    /// sending back, and returns the reply bytes to write, if any.
    fn handle(&mut self, message: &[u8]) -> Option<Vec<u8>> {
        if message.len() < 3 || message[0] != OUR_ADDRESS {
            // Not addressed to us (e.g. the broadcast I/F Clear the client
            // sends on connect) — nothing to reply to.
            println!("RECV: {} (ignored)", describe_message(message));
            return None;
        }

        println!("RECV: {}", describe_message(message));

        let reply = match message[1] {
            0x09 => self.handle_inquiry(&message[2..message.len() - 1]),
            0x01 => {
                self.apply_command(&message[2..message.len() - 1]);
                Some(ack_then_complete(1))
            }
            _ => None,
        };

        match &reply {
            Some(bytes) => println!("SEND: {}", describe_reply(bytes)),
            None => println!("SEND: (no reply)"),
        }
        reply
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
    /// command-vs-inquiry marker, up to but excluding the terminator).
    fn apply_command(&mut self, body: &[u8]) {
        match body {
            // Pan/tilt relative move: 06 03 VV WW <4 nibbles pan> <4 nibbles tilt>
            [0x06, 0x03, _pan_speed, _tilt_speed, rest @ ..] if rest.len() == 8 => {
                let pan_delta = nibbles_to_i16(&rest[0..4]).saturating_mul(PAN_TILT_SCALE);
                let tilt_delta = nibbles_to_i16(&rest[4..8]).saturating_mul(PAN_TILT_SCALE);
                self.pan = self.pan.saturating_add(pan_delta);
                self.tilt = self.tilt.saturating_add(tilt_delta);
            }
            // Pan/tilt home.
            [0x06, 0x04] => {
                self.pan = 0;
                self.tilt = 0;
            }
            // Zoom stop: no state change.
            [0x04, 0x07, 0x00] => {}
            // Zoom tele (in).
            [0x04, 0x07, 0x02] => self.zoom = self.zoom.saturating_add(ZOOM_FOCUS_STEP),
            // Zoom wide (out).
            [0x04, 0x07, 0x03] => self.zoom = self.zoom.saturating_sub(ZOOM_FOCUS_STEP),
            // Focus stop: no state change.
            [0x04, 0x08, 0x00] => {}
            // Focus far, with or without an explicit speed nibble (0x02, or 0x20..=0x2F).
            [0x04, 0x08, b] if *b == 0x02 || (0x20..=0x2F).contains(b) => {
                self.focus = self.focus.saturating_add(ZOOM_FOCUS_STEP);
            }
            // Focus near, with or without an explicit speed nibble (0x03, or 0x30..=0x3F).
            [0x04, 0x08, b] if *b == 0x03 || (0x30..=0x3F).contains(b) => {
                self.focus = self.focus.saturating_sub(ZOOM_FOCUS_STEP);
            }
            // Preset: 04 3F action preset_number
            [0x04, 0x3F, action, number] => self.apply_preset(*action, *number),
            _ => {}
        }
    }

    fn apply_preset(&mut self, action: u8, number: u8) {
        match action {
            0x00 => {
                self.presets.remove(&number);
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
            }
            0x02 => {
                if let Some(preset) = self.presets.get(&number) {
                    self.pan = preset.pan;
                    self.tilt = preset.tilt;
                    self.zoom = preset.zoom;
                    self.focus = preset.focus;
                }
            }
            _ => {}
        }
    }
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

fn ack_then_complete(socket: u8) -> Vec<u8> {
    vec![
        0x90,
        0x40 | socket,
        TERMINATOR,
        0x90,
        0x50 | socket,
        TERMINATOR,
    ]
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

fn main() -> std::io::Result<()> {
    let master = posix_openpt(OFlag::O_RDWR).expect("posix_openpt should succeed");
    grantpt(&master).expect("grantpt should succeed");
    unlockpt(&master).expect("unlockpt should succeed");
    let slave_path = ptsname_r(&master).expect("ptsname_r should succeed");

    println!("Fake camera listening on {slave_path}");
    println!("In another terminal, run: cargo run -- {slave_path}");

    let mut master = master;
    let mut camera = CameraSim::new();
    let mut message = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        // A PTY master reads as EIO once every slave-side file descriptor is
        // closed, rather than blocking until a new one opens. viscous
        // itself disconnects and reconnects during startup (a short
        // discovery probe, then a fresh connection for the real session),
        // so treat a read error as "no client right now" and wait for the
        // next one instead of exiting.
        if master.read_exact(&mut byte).is_err() {
            message.clear();
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        message.push(byte[0]);
        if byte[0] != TERMINATOR {
            continue;
        }

        if let Some(reply) = camera.handle(&message)
            && let Err(error) = master.write_all(&reply).and_then(|()| master.flush())
        {
            println!("SEND failed (client likely disconnected): {error}");
        }
        message.clear();
    }
}
