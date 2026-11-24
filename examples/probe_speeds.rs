//! Measures how fast a camera actually moves at each speed number it accepts.
//!
//! VISCA lets a drive command ask for pan speeds up to 24, tilt up to 20 and
//! zoom and focus up to 7, and says nothing about what any of those numbers
//! is worth: whether they are evenly spaced, whether the top ones differ at
//! all, how slow the slowest really is. A control that turns a hand's push
//! into one of those numbers is only as good as what is known about them, and
//! the camera is the only thing that knows.
//!
//! Two measurements, made separately so that each can be held against the
//! other:
//!
//! **Driving**, which is what the pad and the rockers do. The camera is
//! started at a speed and its position is read over and over *while it
//! moves*, each reading stamped with the time it was taken. The rate is the
//! slope of those readings once the head is up to speed, so it rests on
//! nothing but the camera's own account of where it was and when — not on how
//! long this program believes it left the motor running, which is the
//! assumption a start-wait-stop measurement makes and cannot check.
//!
//! **Trips**, for pan and tilt, which is how a remembered position is gone
//! to. The head is sent to a fixed position at a speed and timed until the
//! camera reports it has arrived, over one distance and then over twice that
//! distance. Both trips spend the same time getting up to speed and stopping,
//! so the difference between them is the time the extra distance took at
//! full speed. A trip can also be checked with a watch, by anyone standing
//! next to the camera.
//!
//! The two need not agree, and where they don't that is a finding rather
//! than a fault: a camera is free to treat the speed in a drive and the speed
//! in a move to a position as different things.
//!
//! Run it against the real camera, the same target string the app takes, and
//! optionally name the parts to measure (all four when none are named):
//!
//! ```text
//! cargo run --example probe_speeds -- COM3
//! cargo run --example probe_speeds -- tcp://192.168.1.50:5678 pan tilt
//! ```
//!
//! **The camera moves, a lot, for a quarter of an hour if everything is
//! measured.** Pan and tilt start every run from home and swing right and up
//! from there; zoom starts from fully wide and runs to the far end; focus is
//! switched to manual, starts from fully far, and is handed back to autofocus
//! afterwards if that is where it was found. Watch the first few runs and
//! interrupt if the head looks like it is about to meet something.

use std::{
    env,
    process::ExitCode,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use grafton_visca::{
    CameraId, Error,
    camera::PanTiltPosition,
    command::{
        FocusMode, FocusModeInquiry, FocusPositionInquiry, FocusSpeed, InquiryData,
        PanTiltDirection, PanTiltPositionInquiry, VersionInfo, ViscaCommand, ZoomPositionInquiry,
    },
    timeout::CommandCategory,
    types::ZoomSpeed,
};
use tokio::runtime::Runtime;
use viscous::{
    connection::{self, Session, Target},
    focus::{self, FocusDirection, FocusDrive},
    pan_tilt::{self, MAX_PAN_SPEED, MAX_TILT_SPEED, Velocity},
    wire,
    zoom::{self, ZoomDirection, ZoomDrive},
};

/// The camera, one exchange at a time: a measurement wants each answer before
/// deciding what to ask next, which is exactly what the app's own worker is
/// built not to do.
struct Camera {
    runtime: Runtime,
    session: Session,
}

impl Camera {
    /// The camera at `target`, how it was reached, and what it said it is.
    fn open(target: &Target) -> Result<(Self, String, VersionInfo), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let connection::Opened {
            session,
            link,
            version,
        } = runtime.block_on(connection::open(target))?;
        Ok((Self { runtime, session }, link, version))
    }

    fn execute(&self, command: impl ViscaCommand) -> Result<(), Error> {
        self.runtime
            .block_on(wire::execute(self.session.camera(), command))
    }

    fn ask(&self, inquiry: impl ViscaCommand) -> Result<InquiryData, Error> {
        self.runtime
            .block_on(wire::ask(self.session.camera(), inquiry))
    }

    fn pan_tilt_position(&self) -> Result<PanTiltPosition, Error> {
        match self.ask(PanTiltPositionInquiry)? {
            InquiryData::PanTiltPosition { pan, tilt } => Ok(PanTiltPosition { pan, tilt }),
            other => Err(unexpected(other)),
        }
    }

    fn zoom_position(&self) -> Result<u16, Error> {
        match self.ask(ZoomPositionInquiry)? {
            InquiryData::ZoomPosition { position } => Ok(position),
            other => Err(unexpected(other)),
        }
    }

    fn focus_position(&self) -> Result<u16, Error> {
        match self.ask(FocusPositionInquiry)? {
            InquiryData::FocusPosition { position } => Ok(position),
            other => Err(unexpected(other)),
        }
    }

    fn focus_mode(&self) -> Result<FocusMode, Error> {
        match self.ask(FocusModeInquiry)? {
            InquiryData::FocusMode { mode } => Ok(mode),
            other => Err(unexpected(other)),
        }
    }
}

fn unexpected(answer: InquiryData) -> Error {
    Error::ParseError(format!("an answer to some other question: {answer:?}").into())
}

/// How far from home a drive is allowed to carry the head before it is
/// stopped, in the camera's units. Both inside what this camera has already
/// been seen to travel without meeting an end.
const PAN_FAR_ENOUGH: i32 = 3000;
const TILT_FAR_ENOUGH: i32 = 2000;

/// How far the two timed trips go on each axis. The second is twice the
/// first, so the difference between them is one more of the first.
const PAN_TRIP: i16 = 1000;
const TILT_TRIP: i16 = 600;

/// How long a drive is left running when it is too slow to get far enough.
/// Long enough to watch, and to collect a few dozen readings at any speed.
const PAN_TILT_LONG_ENOUGH: Duration = Duration::from_secs(4);
const LENS_LONG_ENOUGH: Duration = Duration::from_secs(60);

/// How long the lens is driven toward its starting end before each zoom or
/// focus run, at the top speed. More than either takes end to end.
const REWIND: Duration = Duration::from_secs(4);

/// How long a moving reading can go without changing before whatever is
/// being driven is taken to have reached the end of its travel.
const STALLED: Duration = Duration::from_millis(600);

/// How close to the fastest rate measured a speed has to come to count as
/// being that fast, as a fraction of it. Wider than the scatter between two
/// runs at the same speed, narrower than a genuine step between two speeds.
const AS_FAST: f64 = 0.97;

/// What one speed number turned out to be worth.
struct Run {
    speed: u8,
    /// Units per second while driving, from readings taken on the move, or
    /// `None` when it was over too quickly to take enough of them.
    driving: Option<f64>,
    /// Units per second over a timed trip, where trips were made.
    trip: Option<f64>,
}

/// What a drive's readings add up to.
struct Drive {
    /// Units per second over the second half of the distance, by which time
    /// it should be up to speed.
    rate: Option<f64>,
    /// Units per second over the stretch before that, which matches `rate`
    /// only if the head was already up to speed there too.
    earlier: Option<f64>,
    /// How many seconds behind an instant start the head was running.
    behind: Option<f64>,
    /// How much further it went after being told to stop.
    coasted: i32,
    /// How far it went while being watched, and for how long.
    covered: i32,
    took: Duration,
    /// Every reading taken on the move: seconds since the drive was sent, and
    /// the position the camera gave.
    readings: Vec<(f64, i32)>,
    /// Whether it stopped moving on its own, which is the end of its travel.
    stalled: bool,
}

/// What went wrong, and what was being attempted when it did. An error on
/// its own says only that something timed out or was refused; which of a
/// dozen messages that was is the whole of the diagnosis.
type Failure = String;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(target) = args.next() else {
        eprintln!(
            "usage: probe_speeds <serial port|tcp://host[:port]> [pan] [tilt] [zoom] [focus]"
        );
        return ExitCode::from(2);
    };
    let parts: Vec<String> = args.collect();
    if let Some(unknown) = parts
        .iter()
        .find(|part| !["pan", "tilt", "zoom", "focus"].contains(&part.as_str()))
    {
        eprintln!("don't know how to measure \"{unknown}\": pan, tilt, zoom or focus");
        return ExitCode::from(2);
    }
    let wanted = |part: &str| parts.is_empty() || parts.iter().any(|named| named == part);

    let (camera, link, version) = match Camera::open(&Target::from(target.as_str())) {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    println!("{link}");
    println!("{}", connection::format_version(&version));

    let camera = &camera;
    let outcome = (|| -> Result<(), Failure> {
        if wanted("pan") {
            let runs = pan_or_tilt(camera, Axis::Pan)?;
            report("pan", &runs);
        }
        if wanted("tilt") {
            let runs = pan_or_tilt(camera, Axis::Tilt)?;
            report("tilt", &runs);
        }
        if wanted("pan") || wanted("tilt") {
            go_home(camera);
        }
        if wanted("zoom") {
            let runs = zoom_runs(camera)?;
            report("zoom", &runs);
        }
        if wanted("focus") {
            let runs = focus_runs(camera)?;
            report("focus", &runs);
        }
        Ok(())
    })();

    save_readings();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Whatever was running is still running as far as the camera
            // knows; none of these can be left to drive into an end stop.
            if let Ok(stop) = pan_tilt::drive(Velocity::STOP) {
                let _ = camera.execute(stop);
            }
            let _ = camera.execute(zoom::drive_zoom(None));
            let _ = camera.execute(focus::drive_focus(None));
            eprintln!("stopped: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Pan,
    Tilt,
}

/// Names the step a camera error belongs to.
fn during<T>(what: &str, result: Result<T, Error>) -> Result<T, Failure> {
    result.map_err(|error| format!("{what}: {error}"))
}

/// Sends the head home, without making it fatal if the camera is slow to say
/// it has arrived: [`at_rest`] waits for the head to stop either way, so a
/// completion that comes late or never is only worth a note.
fn go_home(camera: &Camera) {
    if let Err(error) = camera.execute(pan_tilt::HOME) {
        println!("(home: {error} — waiting for the head to stop instead)");
    }
}

/// Measures one axis at every speed it accepts, from home each time.
fn pan_or_tilt(camera: &Camera, axis: Axis) -> Result<Vec<Run>, Failure> {
    let (name, top, far_enough, trip, direction) = match axis {
        Axis::Pan => (
            "pan",
            MAX_PAN_SPEED,
            PAN_FAR_ENOUGH,
            PAN_TRIP,
            PanTiltDirection::Right,
        ),
        Axis::Tilt => (
            "tilt",
            MAX_TILT_SPEED,
            TILT_FAR_ENOUGH,
            TILT_TRIP,
            PanTiltDirection::Up,
        ),
    };
    let position = || {
        camera.pan_tilt_position().map(|position| match axis {
            Axis::Pan => i32::from(position.pan),
            Axis::Tilt => i32::from(position.tilt),
        })
    };
    let home = |why: &str| {
        go_home(camera);
        at_rest(&format!("{name} position at home, {why}"), position)
    };

    println!();
    let mut runs = Vec::new();
    for speed in 1..=top {
        let from = home(&format!("before driving at speed {speed}"))?;
        let velocity = Velocity {
            direction,
            pan_speed: speed.min(MAX_PAN_SPEED),
            tilt_speed: speed.min(MAX_TILT_SPEED),
        };
        let drive = watched(
            &format!("{name} drive at speed {speed}"),
            from,
            far_enough,
            PAN_TILT_LONG_ENOUGH,
            || camera.execute(pan_tilt::drive(velocity)?),
            || camera.execute(pan_tilt::drive(Velocity::STOP)?),
            position,
        )?;
        note(name, speed, &drive);

        // Then the same speed as a trip to a place, twice, the second twice
        // as far. Each is timed from sending the move to the camera saying it
        // has arrived.
        let mut legs = [Duration::ZERO; 2];
        for (leg, distance) in legs.iter_mut().zip([trip, trip * 2]) {
            home(&format!("before a trip at speed {speed}"))?;
            let (pan, tilt) = match axis {
                Axis::Pan => (distance, 0),
                Axis::Tilt => (0, distance),
            };
            let sent = Instant::now();
            during(
                &format!("{name} trip of {distance} at speed {speed}"),
                camera.execute(AbsolutePosition {
                    pan,
                    tilt,
                    pan_speed: speed.min(MAX_PAN_SPEED),
                    tilt_speed: speed.min(MAX_TILT_SPEED),
                }),
            )?;
            *leg = sent.elapsed();
            let landed = at_rest(&format!("{name} position after a trip"), position)?;
            if (landed - i32::from(distance)).abs() > 2 {
                println!("              (sent to {distance} but landed on {landed})");
            }
        }
        let extra = legs[1].as_secs_f64() - legs[0].as_secs_f64();
        let trip_rate = (extra > 0.02).then(|| f64::from(trip) / extra);
        println!(
            "              trips of {trip} and {} took {:.2} s and {:.2} s{}",
            trip * 2,
            legs[0].as_secs_f64(),
            legs[1].as_secs_f64(),
            match trip_rate {
                Some(rate) => format!(", {rate:.1} units/s"),
                None => ", too close together to say how fast".to_string(),
            }
        );

        runs.push(Run {
            speed,
            driving: drive.rate,
            trip: trip_rate,
        });
    }
    Ok(runs)
}

/// Zooms in at every speed the camera accepts, from fully wide to as far as
/// it goes.
fn zoom_runs(camera: &Camera) -> Result<Vec<Run>, Failure> {
    let drive = |direction, speed| {
        Some(ZoomDrive {
            direction,
            speed: ZoomSpeed::new(speed).expect("a zoom speed from 0 to 7"),
        })
    };
    let position = || camera.zoom_position().map(i32::from);
    let rewind = || -> Result<(), Failure> {
        during(
            "zoom back to wide, starting",
            camera.execute(zoom::drive_zoom(drive(ZoomDirection::Out, 7))),
        )?;
        thread::sleep(REWIND);
        during(
            "zoom back to wide, stopping",
            camera.execute(zoom::drive_zoom(None)),
        )
    };

    println!();
    let mut runs = Vec::new();
    for speed in 0..=7 {
        rewind()?;
        let from = at_rest("zoom position at wide", position)?;
        let drive = watched(
            &format!("zoom drive at speed {speed}"),
            from,
            i32::MAX,
            LENS_LONG_ENOUGH,
            || camera.execute(zoom::drive_zoom(drive(ZoomDirection::In, speed))),
            || camera.execute(zoom::drive_zoom(None)),
            position,
        )?;
        note("zoom", speed, &drive);
        runs.push(Run {
            speed,
            driving: drive.rate,
            trip: None,
        });
    }
    rewind()?;
    Ok(runs)
}

/// Focuses nearer at every speed the camera accepts, from fully far to as
/// near as it goes, with autofocus out of the way for as long as that takes.
fn focus_runs(camera: &Camera) -> Result<Vec<Run>, Failure> {
    let drive = |direction, speed| {
        Some(FocusDrive {
            direction,
            speed: FocusSpeed::new(speed).expect("a focus speed from 0 to 7"),
        })
    };
    let position = || camera.focus_position().map(i32::from);

    let was_auto = during("focus mode inquiry", camera.focus_mode())? == FocusMode::Auto;
    during(
        "switch to manual focus",
        camera.execute(focus::set_auto_focus(false)),
    )?;

    println!();
    let measured = (|| -> Result<Vec<Run>, Failure> {
        let mut runs = Vec::new();
        for speed in 0..=7 {
            during(
                "focus back to far, starting",
                camera.execute(focus::drive_focus(drive(FocusDirection::Far, 7))),
            )?;
            thread::sleep(REWIND);
            during(
                "focus back to far, stopping",
                camera.execute(focus::drive_focus(None)),
            )?;
            let from = at_rest("focus position at far", position)?;
            let drive = watched(
                &format!("focus drive at speed {speed}"),
                from,
                i32::MAX,
                LENS_LONG_ENOUGH,
                || camera.execute(focus::drive_focus(drive(FocusDirection::Near, speed))),
                || camera.execute(focus::drive_focus(None)),
                position,
            )?;
            note("focus", speed, &drive);
            runs.push(Run {
                speed,
                driving: drive.rate,
                trip: None,
            });
        }
        Ok(runs)
    })();

    if was_auto {
        let _ = camera.execute(focus::drive_focus(None));
        during(
            "switch back to autofocus",
            camera.execute(focus::set_auto_focus(true)),
        )?;
    }
    measured
}

/// Starts a drive and reads its position for as long as it runs, stopping it
/// once it has gone `far_enough` from `from`, run `long_enough`, or stopped
/// moving by itself.
///
/// Every reading is stamped with the middle of the time it took to get, which
/// is as near as can be known to when the camera looked. Nothing here depends
/// on when the drive really started or really stopped.
fn watched(
    what: &str,
    from: i32,
    far_enough: i32,
    long_enough: Duration,
    start: impl FnOnce() -> Result<(), Error>,
    stop: impl FnOnce() -> Result<(), Error>,
    read: impl Fn() -> Result<i32, Error>,
) -> Result<Drive, Failure> {
    /// How many readings in a row a camera may refuse before it is taken to
    /// have stopped answering. A few are normal while it is busy.
    const REFUSALS: usize = 20;

    during(&format!("{what}, starting"), start())?;
    let began = Instant::now();
    let mut readings: Vec<(f64, i32)> = Vec::new();
    let mut last_change = (Duration::ZERO, from);
    let mut refusals = 0;
    let mut stalled = false;
    let watching = loop {
        let asked = began.elapsed();
        match read() {
            Ok(position) => {
                refusals = 0;
                let answered = began.elapsed();
                readings.push(((asked + answered).as_secs_f64() / 2.0, position));
                if position != last_change.1 {
                    last_change = (answered, position);
                } else if position != from && answered - last_change.0 >= STALLED {
                    stalled = true;
                    break Ok(());
                }
                if (position - from).abs() >= far_enough {
                    break Ok(());
                }
            }
            Err(error) => {
                refusals += 1;
                if refusals >= REFUSALS {
                    break Err(format!("{what}, reading the position on the move: {error}"));
                }
            }
        }
        if began.elapsed() >= long_enough {
            break Ok(());
        }
        // A serial link paces the readings by itself; a network one would
        // otherwise ask thousands of times a second and learn nothing more.
        thread::sleep(Duration::from_millis(20));
    };
    let took = began.elapsed();
    let stopped = during(&format!("{what}, stopping"), stop());
    watching?;
    stopped?;

    let covered = readings
        .last()
        .map_or(0, |(_, position)| (position - from).abs());
    let landed = at_rest(&format!("{what}, position once stopped"), &read)?;
    let late = fit(&readings, from, covered, 0.5..=0.95);
    Ok(Drive {
        rate: late.map(|line| line.slope),
        earlier: fit(&readings, from, covered, 0.2..=0.5).map(|line| line.slope),
        behind: late.map(|line| line.starts_at),
        coasted: (landed - from).abs() - covered,
        covered,
        took,
        readings,
        stalled,
    })
}

/// A straight line through part of a drive: how fast it was going, and when
/// a head that had been going that fast all along would have set off.
#[derive(Clone, Copy)]
struct Line {
    /// Units per second.
    slope: f64,
    /// Seconds after the drive command at which the line crosses the
    /// starting position.
    starts_at: f64,
}

/// The line that best fits the readings taken while the drive was between
/// two fractions of the distance it went in the end.
///
/// A fit through every reading in the stretch rather than a slope between
/// its two ends, because each reading's time is only known to within the
/// several milliseconds its reply took to come back: over a dozen readings
/// that scatter averages away, and between two it is the whole of the error.
fn fit(
    readings: &[(f64, i32)],
    from: i32,
    covered: i32,
    stretch: std::ops::RangeInclusive<f64>,
) -> Option<Line> {
    let points: Vec<(f64, f64)> = readings
        .iter()
        .map(|(at, position)| (*at, f64::from((position - from).abs())))
        .filter(|(_, gone)| stretch.contains(&(gone / f64::from(covered.max(1)))))
        .collect();
    if points.len() < 4 {
        return None;
    }
    let count = points.len() as f64;
    let mean_at = points.iter().map(|(at, _)| at).sum::<f64>() / count;
    let mean_gone = points.iter().map(|(_, gone)| gone).sum::<f64>() / count;
    let spread: f64 = points.iter().map(|(at, _)| (at - mean_at).powi(2)).sum();
    let together: f64 = points
        .iter()
        .map(|(at, gone)| (at - mean_at) * (gone - mean_gone))
        .sum();
    let slope = together / spread;
    (spread > 0.0 && slope > 0.0).then(|| Line {
        slope,
        starts_at: mean_at - mean_gone / slope,
    })
}

/// Every reading taken on the move, kept so that a conclusion drawn from them
/// can be checked against them afterwards — or a different one drawn.
static READINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Where [`READINGS`] is written when the run ends, however it ends.
const READINGS_FILE: &str = "probe_speeds_readings.csv";

/// Prints one drive as a line of transcript, and keeps its readings.
fn note(name: &str, speed: u8, drive: &Drive) {
    println!("{name} speed {speed:>2}: {}", describe(drive));
    let mut kept = READINGS.lock().expect("nothing panics holding this");
    kept.extend(
        drive
            .readings
            .iter()
            .map(|(at, position)| format!("{name},{speed},{at:.4},{position}")),
    );
}

/// Writes out what [`note`] kept.
fn save_readings() {
    let kept = READINGS.lock().expect("nothing panics holding this");
    if kept.is_empty() {
        return;
    }
    let contents = format!("part,speed,seconds,position\n{}\n", kept.join("\n"));
    match std::fs::write(READINGS_FILE, contents) {
        Ok(()) => println!("\nevery reading is in {READINGS_FILE}"),
        Err(error) => println!("\ncouldn't write {READINGS_FILE}: {error}"),
    }
}

/// One drive, as a line of transcript.
fn describe(drive: &Drive) -> String {
    let Some(rate) = drive.rate else {
        return format!(
            "driving, too quick to say how fast — {} units in {:.2} s, {} readings",
            drive.covered,
            drive.took.as_secs_f64(),
            drive.readings.len(),
        );
    };
    let mut line = format!(
        "driving, {rate:>8.1} units/s — {} units in {:.2} s, {} readings",
        drive.covered,
        drive.took.as_secs_f64(),
        drive.readings.len(),
    );
    // A head that takes time to get going ends up behind where one that set
    // off at full speed would be, by half of however long the getting going
    // took (plus what the link costs). Trailing off after the stop is the
    // same thing at the other end.
    if let Some(behind) = drive.behind {
        line += &format!(", {:.0} ms behind an instant start", behind * 1000.0);
    }
    if !drive.stalled {
        line += &format!(", ran on {} after the stop", drive.coasted);
    }
    // The first part of the run going slower than the last is a head that
    // was still gathering speed, and a rate that is therefore a lower bound.
    if let Some(earlier) = drive.earlier
        && earlier < rate * 0.95
    {
        line += &format!(", STILL SPEEDING UP (was {earlier:.1} earlier in the run)");
    }
    if drive.stalled {
        line += ", ran to the end of its travel";
    }
    line
}

/// Where the camera is once it has stopped moving, as `read` reports it.
///
/// A camera that has just been told to stop is still coasting, and one that
/// has just finished a move may refuse a position inquiry outright for a
/// moment. So this asks until two answers in a row agree, and treats a
/// refusal as one more reason to ask again rather than as an answer.
///
/// Only the one number being measured is asked for. A snapshot of everything
/// the camera knows would be several inquiries where one will do, and any one
/// of them being refused — or autofocus shifting between two readings —
/// would look like a head that never came to rest.
fn at_rest(what: &str, read: impl Fn() -> Result<i32, Error>) -> Result<i32, Failure> {
    const PATIENCE: usize = 80;
    let mut previous = None;
    let mut refused = None;
    for _ in 0..PATIENCE {
        thread::sleep(Duration::from_millis(250));
        match read() {
            Ok(now) if previous == Some(now) => return Ok(now),
            Ok(now) => previous = Some(now),
            Err(error) => {
                previous = None;
                refused = Some(error);
            }
        }
    }
    Err(match (previous, refused) {
        (Some(last), _) => format!("{what}: never held still, last read {last}"),
        (None, Some(error)) => format!("{what}: the camera would not say ({error})"),
        (None, None) => format!("{what}: no reading"),
    })
}

/// Says where one control's speeds level off, by each measurement that was
/// made of it.
fn report(name: &str, runs: &[Run]) {
    println!();
    level_off(name, "driving", runs, |run| run.driving);
    if runs.iter().any(|run| run.trip.is_some()) {
        level_off(name, "on a trip", runs, |run| run.trip);
    }
}

/// Says where the rates `of` picks out stop climbing.
fn level_off(name: &str, how: &str, runs: &[Run], of: impl Fn(&Run) -> Option<f64>) {
    let measured: Vec<(u8, f64)> = runs
        .iter()
        .filter_map(|run| of(run).map(|rate| (run.speed, rate)))
        .collect();
    let fastest = measured.iter().map(|(_, rate)| *rate).fold(0.0, f64::max);
    let (Some((ceiling, rate)), Some((top, _))) = (
        measured.iter().find(|(_, rate)| *rate >= fastest * AS_FAST),
        measured.last(),
    ) else {
        println!("{name}, {how}: nothing measured");
        return;
    };
    // The speed below the top one coming within a few percent of it is two
    // neighbours being close, not a ceiling; it takes a third to call it one.
    if top - ceiling < 2 {
        println!("{name}, {how}: still getting faster at speed {top}, the top of its range");
    } else {
        println!(
            "{name}, {how}: no faster past speed {ceiling} ({rate:.1} units/s), all the way to {top}"
        );
    }
}

/// `Pan-tiltPosition Absolute Position`: go to a place, at a speed on each
/// axis, and answer on arrival.
///
/// Encoded here because grafton-visca's own takes one of five named speed
/// levels rather than the camera's number, and gives the move thirty seconds
/// to arrive. A deadline passing makes the library send the move again, so a
/// slow trip has to be given longer than it can take: sixty seconds, the
/// allowance a preset recall gets for the same reason.
struct AbsolutePosition {
    pan: i16,
    tilt: i16,
    pan_speed: u8,
    tilt_speed: u8,
}

impl ViscaCommand for AbsolutePosition {
    const MAX_SIZE: usize = 15;
    const TIMEOUT_CATEGORY: CommandCategory = CommandCategory::Preset;

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() < Self::MAX_SIZE {
            return Err(Error::BufferTooSmall {
                required: Self::MAX_SIZE,
                actual: buffer.len(),
            });
        }
        buffer[..6].copy_from_slice(&[
            camera_id.to_address_byte(),
            0x01,
            0x06,
            0x02,
            self.pan_speed,
            self.tilt_speed,
        ]);
        // Two's complement, which is how VISCA reads a position: a signed
        // coordinate in a field of unsigned nibbles.
        buffer[6..10].copy_from_slice(&nibbles(self.pan as u16));
        buffer[10..14].copy_from_slice(&nibbles(self.tilt as u16));
        buffer[14] = 0xFF;
        Ok(Self::MAX_SIZE)
    }
}

/// Splits `value` into VISCA's four nibbles, most significant first.
fn nibbles(value: u16) -> [u8; 4] {
    [
        (value >> 12) as u8 & 0x0F,
        (value >> 8) as u8 & 0x0F,
        (value >> 4) as u8 & 0x0F,
        value as u8 & 0x0F,
    ]
}
