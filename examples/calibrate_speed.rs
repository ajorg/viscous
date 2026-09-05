//! Measures the true cruising rate at a spread of speed numbers, on each
//! axis — and checks, rather than assumes, that the head has actually
//! finished accelerating by the time it says so.
//!
//! `examples/probe_absolute.rs` times a single fixed-distance trip per speed.
//! Against real hardware that undercounts the true rate, and by more as
//! speed rises: a short trip spends a growing share of itself accelerating
//! to speed and decelerating back to a stop, so the average speed it reports
//! sits below the head's actual cruising speed.
//!
//! Two distances and a difference would cancel that shared ramp time — *if*
//! the head has actually reached cruising speed by the shorter of the two.
//! But two points always fit a line, whether or not that line means
//! anything: nothing about a two-point measurement can say whether the
//! shorter distance was still inside the ramp. This uses three evenly spaced
//! distances instead. If the head is truly cruising by the first of them,
//! each equal step between them takes equal time; when the two steps
//! disagree, that disagreement is itself the signal that the ramp reaches
//! past the distances tried, not something measurement noise would produce.
//!
//! ```text
//! cargo run --example calibrate_speed -- COM3
//! cargo run --example calibrate_speed -- tcp://192.168.1.50:5678 1 3 6 12 24
//! ```
//!
//! Defaults to a spread across the whole range if no speeds are given on the
//! command line. Every move is toward the centre of the camera's travel, the
//! same direction `probe_absolute.rs` and `spiral.rs` have already used —
//! but the furthest of the three distances reaches further than either of
//! them has gone before, so watch the head on the first run and be ready to
//! interrupt if it looks like it is nearing a hard stop. The camera is put
//! back at its starting position between trips and again at the end.

use std::{
    env,
    process::ExitCode,
    time::{Duration, Instant},
};

use viscous::{
    connection::{self, Camera, Target},
    pan_tilt::MAX_TILT_SPEED,
    shot::{self, Travel},
    state::{self, Position},
};

/// The three calibration distances, in camera units, evenly spaced so that
/// equal steps between them should take equal time once the head is truly
/// cruising.
const SHORT: i16 = 200;
const MID: i16 = 400;
const LONG: i16 = 600;

/// How far the two inner-step times may disagree, as a fraction of the
/// larger one, before a reading is flagged as not yet cruising rather than
/// trusted outright.
const TOLERANCE: f32 = 0.15;

/// The speeds to measure when none are given on the command line: finer at
/// the slow end, where a deliberate move to a shot would live, coarser
/// toward the top.
const DEFAULT_SPEEDS: [u8; 11] = [1, 2, 3, 4, 6, 8, 10, 12, 16, 20, 24];

/// The speed to return to the starting position at between measurements.
const RETURN: Travel = Travel {
    pan_speed: 4,
    tilt_speed: 4,
};

/// A measured cruising rate, and whether the measurement actually looked
/// like cruising.
struct Reading {
    /// Camera units per second, over the distance from [`SHORT`] to
    /// [`LONG`].
    rate: f32,
    /// Whether the two equal steps between [`SHORT`], [`MID`], and [`LONG`]
    /// took equal time — the check a two-point measurement has no way to
    /// make.
    settled: bool,
}

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
            Ok((pan, tilt)) => println!(
                "{speed:>5}  {:>10}  {:>10}",
                format_reading(&pan),
                format_reading(&tilt),
            ),
            Err(error) => println!("{speed:>5}  refused: {error}"),
        }
    }

    println!();
    println!("* the two inner steps didn't agree — the head may not have finished");
    println!("  ramping up to speed within {LONG} units; treat as a lower bound,");
    println!("  not the true cruising rate");

    println!();
    if let Err(error) = shot::go_to(camera, home, RETURN) {
        eprintln!("couldn't return to the starting position: {error}");
        return ExitCode::FAILURE;
    }
    println!("back at {}", state::format_position(&home));
    ExitCode::SUCCESS
}

fn format_reading(reading: &Reading) -> String {
    if reading.settled {
        format!("{:.1}", reading.rate)
    } else {
        format!("{:.1}*", reading.rate)
    }
}

/// Measures the cruising rate at `speed` on both axes. Drives one axis at a
/// time, holding the other at its slowest speed while its target stays put,
/// the same isolation `probe_absolute.rs` uses.
fn measure(
    camera: &Camera,
    home: Position,
    speed: u8,
) -> Result<(Reading, Reading), grafton_visca::Error> {
    let pan = axis_rate(
        camera,
        home,
        Position {
            pan: toward_centre(home.pan, SHORT),
            ..home
        },
        Position {
            pan: toward_centre(home.pan, MID),
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

    let tilt = axis_rate(
        camera,
        home,
        Position {
            tilt: toward_centre(home.tilt, SHORT),
            ..home
        },
        Position {
            tilt: toward_centre(home.tilt, MID),
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

/// Times round trips to `near`, `mid`, and `far`, then reports the cruising
/// rate — the known distance from `near` to `far` (always `LONG - SHORT`)
/// divided by how long that took — along with whether the two equal steps in
/// between took equal time, which is what says the rate means what it
/// claims to rather than just being a line drawn through two points.
fn axis_rate(
    camera: &Camera,
    home: Position,
    near: Position,
    mid: Position,
    far: Position,
    travel: Travel,
) -> Result<Reading, grafton_visca::Error> {
    let near_elapsed = timed_round_trip(camera, home, near, travel)?;
    let mid_elapsed = timed_round_trip(camera, home, mid, travel)?;
    let far_elapsed = timed_round_trip(camera, home, far, travel)?;

    let first_step = mid_elapsed.saturating_sub(near_elapsed).as_secs_f32();
    let second_step = far_elapsed.saturating_sub(mid_elapsed).as_secs_f32();
    let settled = first_step > 0.0
        && second_step > 0.0
        && (first_step - second_step).abs() / first_step.max(second_step) < TOLERANCE;

    // `saturating_sub` rather than plain subtraction: if timing noise ever
    // inverts two elapsed times, dividing by zero prints an obviously wrong
    // `inf` instead of panicking partway through a hardware run.
    let elapsed = far_elapsed.saturating_sub(near_elapsed).as_secs_f32();
    Ok(Reading {
        rate: f32::from(LONG - SHORT) / elapsed,
        settled,
    })
}

/// Sends the camera to `target`, times how long it took, then returns it to
/// `home`.
fn timed_round_trip(
    camera: &Camera,
    home: Position,
    target: Position,
    travel: Travel,
) -> Result<Duration, grafton_visca::Error> {
    let started = Instant::now();
    shot::go_to(camera, target, travel)?;
    let elapsed = started.elapsed();
    shot::go_to(camera, home, travel)?;
    Ok(elapsed)
}
