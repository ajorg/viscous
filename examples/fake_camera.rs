//! A fake VISCA camera for manually smoke-testing `viscous` against a real
//! serial-shaped transport without physical hardware.
//!
//! This is deliberately separate from the automated test suite (that's
//! `ScriptedBlockingTransport`'s job, used throughout `src/`) — this exists
//! to exercise the real byte path end to end: opening an actual device path,
//! real framing over an actual stream, actual read/write timing. It only
//! implements enough of VISCA to answer what `viscous` currently sends
//! (version/power/zoom/focus/pan-tilt inquiries, and ack+complete for any
//! command), and it doesn't track command effects on its simulated state.
//!
//! Run with `cargo run --example fake_camera`, then in another terminal:
//! `cargo run -- <the printed slave path>`.

use std::io::{Read, Write};

use nix::fcntl::OFlag;
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};

const TERMINATOR: u8 = 0xFF;
const OUR_ADDRESS: u8 = 0x81;

/// Simulated camera state. Commands are always ack+completed regardless of
/// content, so these values are static — enough to prove the wiring works,
/// not to simulate real movement.
struct CameraSim {
    power_on: bool,
    zoom: u16,
    focus: u16,
    pan: i16,
    tilt: i16,
}

impl CameraSim {
    fn new() -> Self {
        Self {
            power_on: true,
            zoom: 0x0000,
            focus: 0x1000,
            pan: 0,
            tilt: 0,
        }
    }

    /// Handles one complete VISCA message (including its trailing
    /// terminator), returning the reply bytes to write back, if any.
    fn handle(&self, message: &[u8]) -> Option<Vec<u8>> {
        if message.len() < 3 || message[0] != OUR_ADDRESS {
            return None;
        }

        match message[1] {
            0x09 => self.handle_inquiry(&message[2..message.len() - 1]),
            0x01 => Some(ack_then_complete(1)),
            _ => None,
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

fn main() -> std::io::Result<()> {
    let master = posix_openpt(OFlag::O_RDWR).expect("posix_openpt should succeed");
    grantpt(&master).expect("grantpt should succeed");
    unlockpt(&master).expect("unlockpt should succeed");
    let slave_path = ptsname_r(&master).expect("ptsname_r should succeed");

    println!("Fake camera listening on {slave_path}");
    println!("In another terminal, run: cargo run -- {slave_path}");

    let mut master = master;
    let camera = CameraSim::new();
    let mut message = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        master.read_exact(&mut byte)?;
        message.push(byte[0]);
        if byte[0] != TERMINATOR {
            continue;
        }

        if let Some(reply) = camera.handle(&message) {
            master.write_all(&reply)?;
            master.flush()?;
        }
        message.clear();
    }
}
