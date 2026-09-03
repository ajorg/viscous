//! Flies the camera to a nearby position along a spiral, instead of straight.
//!
//! A demonstration of [`viscous::path`] against real hardware, and the only
//! caller of it so far. It measures the head before it flies: how far the
//! camera actually travels per second at a given speed number is not something
//! VISCA states, and every velocity along the curve is computed from it, so
//! the figure is taken from this camera rather than assumed.
//!
//! ```text
//! cargo run --example spiral -- COM3
//! cargo run --example spiral -- tcp://192.168.1.50:5678 --turns 3 --seconds 12
//! ```
//!
//! Everything is relative to wherever the camera is pointing when it starts,
//! and every move is toward the centre of its travel, so it never drives into
//! a limit. It puts the camera back where it found it at the end.

use std::{
    env,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use viscous::{
    connection::{self, Camera, Target},
    pan_tilt::{self, Velocity},
    path::{Rates, Spiral},
    shot::{self, Travel},
    state::{self, Position},
};

/// How often to re-aim the drive along the curve. A drive command and its
/// answer take about 16ms on a 9600 baud line, so twenty times a second leaves
/// the wire most of its time and is far finer than the head can resolve.
const TICK: Duration = Duration::from_millis(50);

/// The speed to calibrate at: high enough to cover measurable ground in a
/// second, low enough not to lurch.
const CALIBRATION_SPEED: u8 = 6;

/// How long to hold the calibration drive.
const CALIBRATION_TIME: Duration = Duration::from_secs(1);

/// How far away to put the far end of the spiral, in camera units.
const REACH: i16 = 400;

/// The speeds to set the camera down at once the shape has been flown: slow,
/// because this is the part that shows.
const LANDING: Travel = Travel {
    pan_speed: 2,
    tilt_speed: 2,
};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(target) = args.next() else {
        eprintln!("usage: spiral <serial port|tcp://host[:port]> [--turns N] [--seconds N]");
        return ExitCode::from(2);
    };
    let (mut turns, mut seconds) = (2.0_f32, 8.0_f32);
    while let (Some(flag), Some(value)) = (args.next(), args.next()) {
        match (flag.as_str(), value.parse()) {
            ("--turns", Ok(value)) => turns = value,
            ("--seconds", Ok(value)) => seconds = value,
            _ => {
                eprintln!("unrecognised option: {flag}");
                return ExitCode::from(2);
            }
        }
    }

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
        eprintln!("This camera won't say where it is, so it can't be flown.");
        return ExitCode::FAILURE;
    };
    println!("starting from {}", state::format_position(&home));

    let rates = match measure(camera, home) {
        Ok(rates) => rates,
        Err(error) => {
            eprintln!("couldn't measure the head: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "measured {:.1} units/s pan and {:.1} units/s tilt, per speed step",
        rates.pan, rates.tilt
    );

    // Toward the middle of the travel on both axes, so a run started near a
    // limit doesn't wind into one.
    let inward = |value: i16| if value > 0 { -REACH } else { REACH };
    let far = Position {
        pan: home.pan + inward(home.pan),
        tilt: home.tilt + inward(home.tilt),
        ..home
    };

    // Flying quicker than the head can manage doesn't hurry the shape, it
    // flattens it: a saturated axis stops tracking the curve while the other
    // keeps going. Better to take longer and trace what was asked for.
    let wanted = Duration::from_secs_f32(seconds.max(1.0));
    let shortest = Spiral::shortest(home, far, turns, rates);
    let flight = wanted.max(shortest);
    if flight > wanted {
        println!(
            "{turns} turns at this reach needs {:.1}s, not {:.1}s — taking the time",
            flight.as_secs_f32(),
            wanted.as_secs_f32(),
        );
    }

    println!();
    println!("spiralling out to {}", state::format_position(&far));
    if let Err(error) = fly(camera, Spiral::new(home, far, turns, flight), rates) {
        eprintln!("the flight failed: {error}");
        return ExitCode::FAILURE;
    }

    println!("spiralling back to where we started");
    if let Err(error) = fly(camera, Spiral::new(far, home, turns, flight), rates) {
        eprintln!("the flight failed: {error}");
        return ExitCode::FAILURE;
    }

    match state::query_position(camera) {
        Ok(landed) => println!("landed on {}", state::format_position(&landed)),
        Err(error) => println!("landed, but the camera wouldn't say where: {error}"),
    }
    ExitCode::SUCCESS
}

/// Measures how far the head travels per second at one step of speed, by
/// driving it for a known time and asking where it ended up.
///
/// Both axes are driven toward the middle of their travel, and the camera is
/// put back where it started afterwards, so calibrating costs the caller its
/// position for a couple of seconds and nothing else.
fn measure(camera: &Camera, home: Position) -> Result<Rates, grafton_visca::Error> {
    let inward = |value: i16| value <= 0;
    let per_step = |travelled: i16| {
        f32::from(travelled.abs()) / CALIBRATION_TIME.as_secs_f32() / f32::from(CALIBRATION_SPEED)
    };
    let speed = i32::from(CALIBRATION_SPEED);

    let signed = |toward_positive: bool| if toward_positive { speed } else { -speed };

    pan_tilt::drive(camera, Velocity::from_signed(signed(inward(home.pan)), 0))?;
    thread::sleep(CALIBRATION_TIME);
    pan_tilt::drive(camera, Velocity::STOP)?;
    let panned = state::query_position(camera)?;

    pan_tilt::drive(camera, Velocity::from_signed(0, signed(inward(home.tilt))))?;
    thread::sleep(CALIBRATION_TIME);
    pan_tilt::drive(camera, Velocity::STOP)?;
    let tilted = state::query_position(camera)?;

    shot::go_to(camera, home, LANDING)?;

    Ok(Rates {
        pan: per_step(panned.pan - home.pan),
        tilt: per_step(tilted.tilt - panned.tilt),
    })
}

/// Flies one shape, then sets the camera down exactly where it was aimed.
///
/// The landing is not a nicety. A spiral's speed falls to nothing as it
/// converges and the slowest drive the camera accepts is 1, so the last of the
/// approach can't be flown — it has to be placed.
fn fly(camera: &Camera, flight: Spiral, rates: Rates) -> Result<(), grafton_visca::Error> {
    let started = Instant::now();
    let mut tick = 0;
    while let Some(velocity) = flight.velocity_at(started.elapsed(), rates) {
        pan_tilt::drive(camera, velocity)?;
        tick += 1;
        // Pace from the start rather than from now, so the round trip each
        // command already cost doesn't accumulate into a slower flight than
        // the one that was asked for.
        if let Some(remaining) = (started + TICK * tick).checked_duration_since(Instant::now()) {
            thread::sleep(remaining);
        }
    }
    pan_tilt::drive(camera, Velocity::STOP)?;
    shot::go_to(camera, flight.destination(), LANDING)
}
