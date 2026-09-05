//! Recalls each given preset and reports where the camera says it landed.
//!
//! This is the same harvest `worker.rs`'s `harvest` performs whenever a front
//! end recalls or saves a preset — recall, then ask where that left the
//! camera — run here standalone against real hardware, to confirm the
//! mechanism works before trusting it inside the app.
//!
//! ```text
//! cargo run --example probe_preset -- COM3 1 2 3 4 5 6
//! cargo run --example probe_preset -- tcp://192.168.1.50:5678 1 2
//! ```
//!
//! Only recalls presets, never saves over one, so nothing already programmed
//! into the camera is at risk. Reads where the camera started so it can put
//! it back once every preset has been visited.

use std::{
    env,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use viscous::{
    connection::{self, Camera, Target},
    preset,
    shot::{self, Travel},
    state::{self, Position},
};

/// How long to wait before asking again if a position query is refused right
/// after a move. `examples/probe_absolute.rs`'s hardware run found the camera
/// briefly reports itself busy in exactly that instant, always recovering by
/// the first retry.
const SETTLE: [Duration; 3] = [
    Duration::from_millis(300),
    Duration::from_millis(600),
    Duration::from_millis(1200),
];

/// The speed to return to the starting position at, once every preset has
/// been visited.
const RETURN: Travel = Travel {
    pan_speed: 4,
    tilt_speed: 4,
};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(target) = args.next() else {
        eprintln!("usage: probe_preset <serial port|tcp://host[:port]> <preset number>...");
        return ExitCode::from(2);
    };
    let numbers = match args
        .map(|arg| arg.parse::<u8>())
        .collect::<Result<Vec<u8>, _>>()
    {
        Ok(numbers) if !numbers.is_empty() => numbers,
        _ => {
            eprintln!("give at least one preset number (1-based, as shown on screen) to recall");
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
        eprintln!("This camera won't say where it is, so there is nowhere to return it to.");
        return ExitCode::FAILURE;
    };
    println!("starting from {}", state::format_position(&home));
    println!();

    for number in numbers {
        let started = Instant::now();
        if let Err(error) = preset::recall_preset(camera, number) {
            println!("preset {number:>2} — refused: {error}");
            continue;
        }
        let elapsed = started.elapsed();

        match query_after_settling(camera) {
            Ok(position) => println!(
                "preset {number:>2} — {:>5.2}s, {}",
                elapsed.as_secs_f64(),
                state::format_position(&position),
            ),
            Err(error) => println!(
                "preset {number:>2} — recalled in {:.2}s, but wouldn't say where to: {error}",
                elapsed.as_secs_f64(),
            ),
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

/// Asks where the camera is, retrying for a moment if it says the query isn't
/// executable yet — see [`SETTLE`].
fn query_after_settling(camera: &Camera) -> Result<Position, grafton_visca::Error> {
    let mut last = state::query_position(camera);
    for delay in SETTLE {
        if last.is_ok() {
            break;
        }
        thread::sleep(delay);
        last = state::query_position(camera);
    }
    last
}
