//! Measures the true cruising rate at a spread of speed numbers, on each
//! axis, by cancelling out the head's acceleration time rather than living
//! with it.
//!
//! `examples/probe_absolute.rs` times a single fixed-distance trip per speed.
//! Against real hardware that undercounts the true rate, and by more as
//! speed rises: a short trip spends a growing share of itself accelerating
//! to speed and decelerating back to a stop, so the average speed it reports
//! sits below the head's actual cruising speed. This instead times two trips
//! of different lengths at the same speed and divides the *difference* in
//! distance by the *difference* in time. Both trips spend the same time
//! ramping up to the same speed and back down to a stop, so that shared
//! overhead cancels exactly, leaving the cruising rate on its own — the
//! number [`viscous::path::Rates`] actually wants.
//!
//! ```text
//! cargo run --example calibrate_speed -- COM3
//! cargo run --example calibrate_speed -- tcp://192.168.1.50:5678 1 3 6 12 24
//! ```
//!
//! Defaults to a spread across the whole range if no speeds are given on the
//! command line. Every move is toward the centre of the camera's travel, the
//! same direction `probe_absolute.rs` and `spiral.rs` have already used —
//! but the longer of the two distances reaches further than either of them
//! has gone before, so the ramp's share of the trip stays small even at the
//! top of the range. Watch the head on the first run and be ready to
//! interrupt if it looks like it is nearing a hard stop. The camera is put
//! back at its starting position between trips and again at the end.

use std::{env, process::ExitCode, time::Instant};

use viscous::{
    connection::{self, Camera, Target},
    pan_tilt::MAX_TILT_SPEED,
    shot::{self, Travel},
    state::{self, Position},
};

/// The shorter of the two calibration distances, in camera units. Matches
/// `examples/probe_absolute.rs`'s figure, already proven safe.
const SHORT: i16 = 200;

/// The longer of the two calibration distances, reaching further than any
/// earlier probe has driven the head.
const LONG: i16 = 600;

/// The speeds to measure when none are given on the command line: finer at
/// the slow end, where a deliberate move to a shot would live, coarser
/// toward the top.
const DEFAULT_SPEEDS: [u8; 11] = [1, 2, 3, 4, 6, 8, 10, 12, 16, 20, 24];

/// The speed to return to the starting position at between measurements.
const RETURN: Travel = Travel {
    pan_speed: 4,
    tilt_speed: 4,
};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(target) = args.next() else {
        eprintln!("usage: calibrate_speed <serial port|tcp://host[:port]> [speed]...");
        return ExitCode::from(2);
    };
    let speeds = match args
        .map(|arg| arg.parse::<u8>())
        .collect::<Result<Vec<u8>, _>>()
    {
        Ok(speeds) if !speeds.is_empty() => speeds,
        Ok(_) => DEFAULT_SPEEDS.to_vec(),
        Err(error) => {
            eprintln!("not a speed number: {error}");
            return ExitCode::from(2);
        }
    };

    let connected = match connection::connect(&Target::from(target.as_str())) {
        Ok(connected) => connected,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    println!("{}", connected.link);
    let camera = &connected.camera;

    let Ok(home) = state::query_position(camera) else {
        eprintln!("This camera won't say where it is, so it can't be measured.");
        return ExitCode::FAILURE;
    };
    println!("starting from {}", state::format_position(&home));
    println!();
    println!("{:>5}  {:>10}  {:>10}", "speed", "pan u/s", "tilt u/s");

    for speed in speeds {
        match measure(camera, home, speed) {
            Ok((pan, tilt)) => println!("{speed:>5}  {pan:>10.1}  {tilt:>10.1}"),
            Err(error) => println!("{speed:>5}  refused: {error}"),
        }
    }

    println!();
    if let Err(error) = shot::go_to(camera, home, RETURN) {
        eprintln!("couldn't return to the starting position: {error}");
        return ExitCode::FAILURE;
    }
    println!("back at {}", state::format_position(&home));
    ExitCode::SUCCESS
}

/// Measures the cruising rate at `speed` on both axes. Drives one axis at a
/// time, holding the other at its slowest speed while its target stays put,
/// the same isolation `probe_absolute.rs` uses.
fn measure(camera: &Camera, home: Position, speed: u8) -> Result<(f32, f32), grafton_visca::Error> {
    let pan = trip_rate(
        camera,
        home,
        Position {
            pan: toward_centre(home.pan, SHORT),
            ..home
        },
        Position {
            pan: toward_centre(home.pan, LONG),
            ..home
        },
        Travel {
            pan_speed: speed,
            tilt_speed: 1,
        },
    )?;

    let tilt = trip_rate(
        camera,
        home,
        Position {
            tilt: toward_centre(home.tilt, SHORT),
            ..home
        },
        Position {
            tilt: toward_centre(home.tilt, LONG),
            ..home
        },
        Travel {
            pan_speed: 1,
            tilt_speed: speed.min(MAX_TILT_SPEED),
        },
    )?;

    Ok((pan, tilt))
}

/// A point `distance` closer to the middle of the travel than `value`, so a
/// run started away from centre never winds up driving into a limit.
fn toward_centre(value: i16, distance: i16) -> i16 {
    if value > 0 {
        value - distance
    } else {
        value + distance
    }
}

/// Times trips to `short` and `long` and returns the cruising rate: the
/// known distance between them (always `LONG - SHORT`, regardless of which
/// side of centre `home` sits on) divided by the difference in how long they
/// took, which cancels the ramp time shared by both. Leaves the camera at
/// `home` between the two trips and after the second.
fn trip_rate(
    camera: &Camera,
    home: Position,
    short: Position,
    long: Position,
    travel: Travel,
) -> Result<f32, grafton_visca::Error> {
    let started = Instant::now();
    shot::go_to(camera, short, travel)?;
    let short_elapsed = started.elapsed();
    shot::go_to(camera, home, travel)?;

    let started = Instant::now();
    shot::go_to(camera, long, travel)?;
    let long_elapsed = started.elapsed();
    shot::go_to(camera, home, travel)?;

    // `saturating_sub` rather than plain subtraction: if timing noise ever
    // inverts the two elapsed times, dividing by zero prints an obviously
    // wrong `inf` instead of panicking partway through a hardware run.
    let elapsed = long_elapsed.saturating_sub(short_elapsed).as_secs_f32();
    Ok(f32::from(LONG - SHORT) / elapsed)
}
