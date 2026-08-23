//! Asks a camera which of the title messages it actually understands.
//!
//! `src/title.rs` encodes `CAM_Title` from the EVI-D70 command list, and a
//! camera that isn't an EVI-D70 may implement all of it, none of it, or a
//! later Sony generation's version of it — from the front end all three look
//! the same, "Syntax error in VISCA command", because the whole sequence is
//! sent before anything is reported. This sends each message on its own and
//! prints what came back, so which part the camera refuses is its own answer
//! rather than a guess from its version reply.
//!
//! Run it against the real camera, the same target string the app takes:
//!
//! ```text
//! cargo run --example probe_title -- COM3
//! cargo run --example probe_title -- tcp://192.168.1.50:5678
//! ```
//!
//! It sets the title to "PROBE", turns the display on and waits, so the video
//! output says whether anything actually arrived, then hides and clears it
//! again. Nothing here is part of the app: it's a diagnostic, and it exists
//! only for as long as this question does.

use std::{env, io, process::ExitCode};

use grafton_visca::{
    CameraId, Error,
    command::{CommandBehavior, InquiryResponseSpec, Response, ViscaCommand},
};
use viscous::connection::{self, Camera, Target};

/// The code for a space, which is what a short title is padded with.
const SPACE: u8 = 0x1B;

/// "PROBE" in the camera's own character codes, padded out to ten.
const PROBE: [u8; 10] = [
    0x0F, 0x11, 0x0E, 0x01, 0x04, SPACE, SPACE, SPACE, SPACE, SPACE,
];

/// Where the title goes and how it's drawn, as `set_title` sends it: bottom
/// left, white, steady.
const APPEARANCE: [u8; 10] = [0x0A, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0];

fn main() -> ExitCode {
    let Some(target) = env::args().nth(1) else {
        eprintln!("usage: probe_title <serial port|tcp://host[:port]>");
        return ExitCode::from(2);
    };

    let connected = match connection::connect(&Target::from(target.as_str())) {
        Ok(connected) => connected,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    println!("{}", connected.link);
    println!("{}", connection::format_version(&connected.version));
    println!();

    let camera = &connected.camera;

    // Read-only first: a camera that answers this at all has the feature,
    // whatever it makes of the commands that follow.
    inquire(camera, "title display inquiry", &[0x09, 0x04, 0x74]);

    // Then the three messages `title::set_title` sends, one at a time.
    send(camera, "title appearance", &title_set(0x00, APPEARANCE));
    send(camera, "title characters 1-10", &title_set(0x01, PROBE));
    send(
        camera,
        "title characters 11-20",
        &title_set(0x02, [SPACE; 10]),
    );
    send(camera, "title display on", &[0x01, 0x04, 0x74, 0x02]);

    // The FCB block cameras that came after the D70 keep eleven numbered
    // title lines and turn them on with 74 2p rather than 74 02. Sent on the
    // chance this camera inherited that command set instead of the D70's.
    send(
        camera,
        "title display on, all lines",
        &[0x01, 0x04, 0x74, 0x2F],
    );

    // Whether the camera can draw anything over the picture at all, which is
    // worth knowing separately from whether it can draw a title.
    send(camera, "on-screen display on", &[0x01, 0x04, 0x15, 0x02]);

    println!();
    println!("Look at the camera's video output, then press Enter.");
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    println!();

    send(camera, "on-screen display off", &[0x01, 0x04, 0x15, 0x03]);
    send(
        camera,
        "title display off, all lines",
        &[0x01, 0x04, 0x74, 0x3F],
    );
    send(camera, "title display off", &[0x01, 0x04, 0x74, 0x03]);
    send(camera, "title clear", &[0x01, 0x04, 0x74, 0x00]);

    ExitCode::SUCCESS
}

/// One `CAM_Title` set message's body: which part it is, then its ten bytes.
fn title_set(part: u8, payload: [u8; 10]) -> [u8; 14] {
    let mut body = [0x00; 14];
    body[..4].copy_from_slice(&[0x01, 0x04, 0x73, part]);
    body[4..].copy_from_slice(&payload);
    body
}

/// Sends one command and prints what the camera made of it.
fn send(camera: &Camera, what: &str, body: &[u8]) {
    let probe = Probe {
        body,
        inquiry: false,
    };
    let outcome = match camera.execute(probe) {
        Ok(()) => "OK".to_string(),
        Err(error) => format!("refused: {error}"),
    };
    report(what, body, &outcome);
}

/// Sends one inquiry and prints the data the camera replied with, which is
/// what says whether it knows the feature at all.
fn inquire(camera: &Camera, what: &str, body: &[u8]) {
    let probe = Probe {
        body,
        inquiry: true,
    };
    let outcome = match camera.send_command(&probe) {
        Ok(Response::RawInquiry(payload)) => format!("reply: {:02X?}", payload.as_slice()),
        Ok(response) => format!("reply: {response:?}"),
        Err(error) => format!("refused: {error}"),
    };
    report(what, body, &outcome);
}

/// One line of transcript: the bytes as they went out, then what came back.
fn report(what: &str, body: &[u8], outcome: &str) {
    let mut packet = vec![0x81];
    packet.extend_from_slice(body);
    packet.push(0xFF);
    let bytes = packet
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{bytes}  {what} — {outcome}");
}

/// A hand-built VISCA message, sent exactly as given.
///
/// `body` is everything between the address byte and the terminator, both of
/// which are the protocol's rather than the caller's.
struct Probe<'a> {
    body: &'a [u8],
    inquiry: bool,
}

impl ViscaCommand for Probe<'_> {
    /// The longest a VISCA message can be, since a probe's length is only
    /// known per instance — [`encoded_size`](ViscaCommand::encoded_size) is
    /// what the buffer is actually sized from.
    const MAX_SIZE: usize = 16;

    fn encoded_size(&self) -> usize {
        self.body.len() + 2
    }

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        let size = self.encoded_size();
        if buffer.len() < size {
            return Err(Error::BufferTooSmall {
                required: size,
                actual: buffer.len(),
            });
        }
        buffer[0] = camera_id.to_address_byte();
        buffer[1..size - 1].copy_from_slice(self.body);
        buffer[size - 1] = 0xFF;
        Ok(size)
    }

    fn behavior(&self) -> CommandBehavior {
        if self.inquiry {
            CommandBehavior::Inquiry(InquiryResponseSpec::Raw)
        } else {
            CommandBehavior::Command
        }
    }
}
