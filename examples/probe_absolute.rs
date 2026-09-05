//! Asks a camera whether it can be sent to a position, and how fast it gets
//! there.
//!
//! Everything built on [`viscous::shot`] rests on two commands this program
//! has never had an answer for from real hardware: `Pan-tiltPosInq`
//! (`81 09 06 12`), which says where the camera is, and `Pan-tiltPosition
//! Absolute Position` (`81 01 06 02`), which sends it somewhere at a chosen
//! speed. The EVI-D70 command list has both. Whether *this* camera does is
//! what this asks.
//!
//! Run it against the real camera, the same target string the app takes:
//!
//! ```text
//! cargo run --example probe_absolute -- COM3
//! cargo run --example probe_absolute -- tcp://192.168.1.50:5678
//! ```
//!
//! It reads where the camera is, moves it a short way and back at a few
//! speeds, and prints how far it actually travelled and how long each move
//! took — so the reply is "yes, and about this many units per second", which
//! is the number every later piece of speed control needs. Every move is
//! toward the centre of the camera's travel, so nothing here drives into a
//! limit. Nothing here is part of the app: it's a diagnostic, and it exists
//! only for as long as this question does.

use std::{
    env,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use viscous::{
    connection::{self, Camera, Target},
    shot::{self, Travel},
    state::{self, Position},
};

/// How far to move for each measurement, in the camera's own units. Far enough
/// to time honestly, short enough to stay well inside the travel.
const DISTANCE: i16 = 200;

/// The speeds to measure, slowest first. The slow end is the one that matters
/// — it's where a deliberate move to a shot would live — and the fast end is
/// there to show whether the scale is linear.
const SPEEDS: [u8; 4] = [1, 3, 6, 12];

fn main() -> ExitCode {
    let Some(target) = env::args().nth(1) else {
        eprintln!("usage: probe_absolute <serial port|tcp://host[:port]>");
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

    // First question, and the one everything else waits on: will it say where
    // it is? If this fails, nothing below can even be attempted.
    let home = match state::query_position(camera) {
        Ok(position) => {
            println!(
                "position inquiry — OK: {}",
                state::format_position(&position)
            );
            position
        }
        Err(error) => {
            println!("position inquiry — refused: {error}");
            println!();
            println!("This camera won't say where it is, so it can't be sent");
            println!("anywhere either. Nothing further to probe.");
            return ExitCode::FAILURE;
        }
    };
    println!();

    // Toward the middle of the travel, so a probe run near a limit doesn't
    // spend its measurements driving into one.
    let away = Position {
        pan: home.pan - DISTANCE * if home.pan > 0 { 1 } else { -1 },
        ..home
    };

    for speed in SPEEDS {
        let travel = Travel {
            pan_speed: speed,
            tilt_speed: speed.min(viscous::pan_tilt::MAX_TILT_SPEED),
        };
        if !measure(camera, away, travel, "out") {
            return ExitCode::FAILURE;
        }
        measure(camera, home, travel, "back");
    }

    println!();
    println!("Units per second is what turns a wanted velocity into a speed");
    println!("number, so it is the figure any smoother movement than this");
    println!("will be built on. A scale that isn't roughly linear here is one");
    println!("that has to be measured across its whole range, not computed.");

    ExitCode::SUCCESS
}

/// How long to wait before asking again, once, after a position query is
/// refused. If the camera reports itself busy for a moment after a move
/// completes rather than immediately clearing, this is what tells the two
/// apart: recovering on the retry says "briefly busy", still refusing says
/// "stuck", and either is worth knowing before anything is built on top.
const SETTLE: [Duration; 3] = [
    Duration::from_millis(300),
    Duration::from_millis(600),
    Duration::from_millis(1200),
];

/// Sends the camera to `shot` and reports how far it went and how long it
/// took. Returns whether the move was accepted at all.
fn measure(camera: &Camera, shot: Position, travel: Travel, leg: &str) -> bool {
    let started = Instant::now();
    let sent = shot::go_to(camera, shot, travel);
    let elapsed = started.elapsed();

    if let Err(error) = sent {
        println!(
            "speed {:>2} {leg} — refused after {:.2}s: {error}",
            travel.pan_speed,
            elapsed.as_secs_f64(),
        );
        return false;
    }

    let arrived = match query_after_settling(camera) {
        Ok(position) => position,
        Err(error) => {
            println!(
                "speed {:>2} {leg} — moved in {:.2}s, but still won't say where to: {error}",
                travel.pan_speed,
                elapsed.as_secs_f64(),
            );
            return true;
        }
    };

    // What the camera did with the request, rather than what was asked for: a
    // camera that clamps, rounds or ignores the coordinate says so here.
    let missed = arrived.pan - shot.pan;
    let rate = f64::from(DISTANCE) / elapsed.as_secs_f64();
    println!(
        "speed {:>2} {leg} — {:>5.2}s, {rate:>7.1} units/s, landed on pan={} ({})",
        travel.pan_speed,
        elapsed.as_secs_f64(),
        arrived.pan,
        if missed == 0 {
            "exactly where it was sent".to_string()
        } else {
            format!("{missed:+} off")
        },
    );
    true
}

/// Asks where the camera is, and if it refuses, asks again a few times with a
/// growing pause between tries before giving up.
fn query_after_settling(camera: &Camera) -> Result<Position, grafton_visca::Error> {
    let mut last = state::query_position(camera);
    for delay in SETTLE {
        if last.is_ok() {
            break;
        }
        thread::sleep(delay);
        println!(
            "  (refused — trying again after {:.1}s)",
            delay.as_secs_f64()
        );
        last = state::query_position(camera);
    }
    last
}
