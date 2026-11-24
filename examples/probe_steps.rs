//! Watches, byte for byte, what a camera says back to a step.
//!
//! A step sometimes ends in "no answer from the camera": grafton-visca waited
//! for the camera's replies to a relative move and didn't get them. A command
//! normally earns two replies, "got it" when the camera takes it and
//! "finished" when the move is done, and which of the two goes missing
//! decides the fix. Missing "finished" means the step can be counted done the
//! moment it is taken. Missing both means the camera isn't doing the step at
//! all, and what is sent has to change. The library only reports that it gave
//! up, so this talks to the serial port directly and prints every byte that
//! comes back, timed from when the step went out. Between steps it asks where
//! the head is, which says whether a step moved it at all, answered or not.
//!
//! Close the app first: a serial port has one owner at a time.
//!
//! ```text
//! cargo run --example probe_steps -- COM4
//! cargo run --example probe_steps -- COM4 38400
//! probe_steps.exe COM4 > steps.txt
//! ```
//!
//! The steps are the app's own: one, two, four and eight units, at the
//! slowest speed, each direction followed by its opposite, so the head ends
//! up where it started, give or take any step it swallowed. A few seconds of
//! barely visible movement in all.

use std::{
    env,
    io::{self, Read, Write},
    process::ExitCode,
    time::{Duration, Instant},
};

use serialport::SerialPort;

/// The camera's baud rate unless told otherwise: the only one the EVI-D70's
/// RS-232C interface has, and the one this camera has answered at.
const DEFAULT_BAUD: u32 = 9600;

/// How long to wait for a step's replies before calling it unanswered. The
/// app gives up after two seconds; this waits longer, to see whether a late
/// answer would have come at all.
const PATIENCE: Duration = Duration::from_secs(5);

/// How long to keep listening after a step has finished, for anything else
/// the camera has to say about it: a second "finished", a reply to something
/// earlier, a late error.
const LINGER: Duration = Duration::from_millis(300);

/// How long to wait for the head to say where it is.
const POSITION_PATIENCE: Duration = Duration::from_secs(1);

/// The step sizes to try, in the camera's units. One is what a tap sends.
const SIZES: [i16; 4] = [1, 2, 4, 8];

/// How many times to go round every direction at every size. The failure is
/// occasional, so once each could easily see nothing.
const ROUNDS: usize = 3;

/// The slowest speed, which is what a step travels at in the app.
const SPEED: u8 = 1;

/// A serial port read as a sequence of VISCA messages, each ended by `FF`.
struct Link {
    port: Box<dyn SerialPort>,
    /// Bytes read that haven't made up a whole message yet.
    partial: Vec<u8>,
}

impl Link {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.port.write_all(bytes)?;
        self.port.flush()
    }

    /// The next whole message to arrive before `deadline`, if one does.
    fn next(&mut self, deadline: Instant) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(end) = self.partial.iter().position(|&byte| byte == 0xFF) {
                return Ok(Some(self.partial.drain(..=end).collect()));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let mut buffer = [0u8; 64];
            match self.port.read(&mut buffer) {
                Ok(read) => self.partial.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error),
            }
        }
    }
}

/// What one reply says, in words.
fn meaning(message: &[u8]) -> String {
    let socket = message.get(1).map_or(0, |byte| byte & 0x0F);
    match (message.len(), message.get(1).map(|byte| byte & 0xF0)) {
        (3, Some(0x40)) => format!("got it (socket {socket})"),
        (3, Some(0x50)) => format!("finished (socket {socket})"),
        (11, Some(0x50)) => match position(message) {
            Some((pan, tilt)) => format!("position pan={pan} tilt={tilt}"),
            None => "a reply with data".to_string(),
        },
        (4, Some(0x60)) => {
            let error = match message[2] {
                0x01 => "message length error",
                0x02 => "syntax error",
                0x03 => "command buffer full",
                0x04 => "command cancelled",
                0x05 => "no socket",
                0x41 => "command not executable",
                _ => "unknown error",
            };
            format!("refused: {error} (socket {socket})")
        }
        _ => "not a reply this probe knows".to_string(),
    }
}

/// The pan and tilt a position reply carries, as signed units.
fn position(message: &[u8]) -> Option<(i16, i16)> {
    if message.len() != 11 || message[0] != 0x90 || message[1] != 0x50 {
        return None;
    }
    let value = |nibbles: &[u8]| {
        nibbles.iter().fold(0u16, |value, nibble| {
            (value << 4) | u16::from(nibble & 0x0F)
        }) as i16
    };
    Some((value(&message[2..6]), value(&message[6..10])))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The relative move a step is sent as, byte for byte what the app sends.
fn relative_move(pan: i16, tilt: i16) -> Vec<u8> {
    let nibbles = |value: i16| {
        let value = value as u16;
        [12, 8, 4, 0].map(|shift| ((value >> shift) & 0x0F) as u8)
    };
    let mut message = vec![0x81, 0x01, 0x06, 0x03, SPEED, SPEED];
    message.extend(nibbles(pan));
    message.extend(nibbles(tilt));
    message.push(0xFF);
    message
}

/// The pan/tilt stop command: a command every camera answers at once, for a
/// baseline of what an ordinary command's replies look like.
const STOP: [u8; 9] = [0x81, 0x01, 0x06, 0x01, SPEED, SPEED, 0x03, 0x03, 0xFF];

/// The pan/tilt position inquiry.
const WHERE: [u8; 5] = [0x81, 0x09, 0x06, 0x12, 0xFF];

/// What came of sending one command.
#[derive(Default)]
struct Trial {
    taken: Option<Duration>,
    finished: Option<Duration>,
    refused: bool,
    /// How far the head moved, where it said where it was before and after.
    moved: Option<(i16, i16)>,
}

/// Sends `command` and prints every reply until it has finished (and a little
/// after) or the patience runs out.
fn trial(link: &mut Link, command: &[u8]) -> io::Result<Trial> {
    println!("  sent      {}", hex(command));
    let sent = Instant::now();
    link.send(command)?;
    let mut trial = Trial::default();
    let mut deadline = sent + PATIENCE;
    while let Some(message) = link.next(deadline)? {
        let at = sent.elapsed();
        println!(
            "  {:>5}ms   {:<26} {}",
            at.as_millis(),
            hex(&message),
            meaning(&message)
        );
        match message.get(1).map(|byte| byte & 0xF0) {
            Some(0x40) if message.len() == 3 => {
                trial.taken.get_or_insert(at);
            }
            Some(0x50) if message.len() == 3 => {
                if trial.finished.is_none() {
                    trial.finished = Some(at);
                    deadline = Instant::now() + LINGER;
                }
            }
            Some(0x60) => {
                trial.refused = true;
                deadline = Instant::now() + LINGER;
            }
            _ => {}
        }
    }
    if trial.finished.is_none() && !trial.refused {
        println!(
            "  NOTHING MORE after {}ms: {}",
            PATIENCE.as_millis(),
            if trial.taken.is_some() {
                "taken, never finished"
            } else {
                "no reply at all"
            }
        );
    }
    Ok(trial)
}

/// Asks where the head is, printing anything else that arrives meanwhile.
fn where_is_it(link: &mut Link) -> io::Result<Option<(i16, i16)>> {
    let asked = Instant::now();
    link.send(&WHERE)?;
    let deadline = asked + POSITION_PATIENCE;
    while let Some(message) = link.next(deadline)? {
        if let Some(found) = position(&message) {
            return Ok(Some(found));
        }
        println!(
            "  {:>5}ms   {:<26} {}  (arrived while asking where the head is)",
            asked.elapsed().as_millis(),
            hex(&message),
            meaning(&message)
        );
    }
    println!("  the head didn't say where it is");
    Ok(None)
}

struct Tally {
    size: i16,
    trials: Vec<Trial>,
}

impl Tally {
    fn summary(&self) -> String {
        let count = |test: &dyn Fn(&Trial) -> bool| self.trials.iter().filter(|t| test(t)).count();
        let finishes: Vec<u128> = self
            .trials
            .iter()
            .filter_map(|trial| trial.finished.map(|at| at.as_millis()))
            .collect();
        let range = match (finishes.iter().min(), finishes.iter().max()) {
            (Some(least), Some(most)) => format!(", finished in {least}-{most}ms"),
            _ => String::new(),
        };
        format!(
            "{} unit{}: {} steps, {} taken, {} finished, {} refused, {} silent, {} moved the head{range}",
            self.size,
            if self.size == 1 { "" } else { "s" },
            self.trials.len(),
            count(&|t| t.taken.is_some()),
            count(&|t| t.finished.is_some()),
            count(&|t| t.refused),
            count(&|t| t.taken.is_none() && t.finished.is_none() && !t.refused),
            count(&|t| t.moved.is_some_and(|moved| moved != (0, 0))),
        )
    }
}

fn run(link: &mut Link) -> io::Result<()> {
    // Whatever the camera was in the middle of saying before this started.
    while link
        .next(Instant::now() + Duration::from_millis(300))?
        .is_some()
    {}

    let Some(start) = where_is_it(link)? else {
        println!("no answer to a position inquiry; is the app still holding the port?");
        return Ok(());
    };
    println!("starting at pan={} tilt={}\n", start.0, start.1);

    println!("baseline: a plain stop, which every camera answers at once");
    for _ in 0..3 {
        trial(link, &STOP)?;
    }

    let mut tallies = Vec::new();
    for size in SIZES {
        let mut tally = Tally {
            size,
            trials: Vec::new(),
        };
        for round in 1..=ROUNDS {
            for (name, pan, tilt) in [
                ("up", 0, size),
                ("down", 0, -size),
                ("right", size, 0),
                ("left", -size, 0),
            ] {
                println!("\nstep {name} {size} (round {round})");
                let before = where_is_it(link)?;
                let mut result = trial(link, &relative_move(pan, tilt))?;
                let after = where_is_it(link)?;
                if let (Some(before), Some(after)) = (before, after) {
                    let moved = (
                        after.0.wrapping_sub(before.0),
                        after.1.wrapping_sub(before.1),
                    );
                    println!("  moved     pan {:+} tilt {:+}", moved.0, moved.1);
                    result.moved = Some(moved);
                }
                tally.trials.push(result);
            }
        }
        tallies.push(tally);
    }

    println!("\nsummary");
    for tally in &tallies {
        println!("  {}", tally.summary());
    }
    if let Some(end) = where_is_it(link)? {
        println!(
            "  ended at pan={} tilt={} (started at pan={} tilt={})",
            end.0, end.1, start.0, start.1
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(port) = args.next() else {
        eprintln!("usage: probe_steps <serial port> [baud]");
        return ExitCode::from(2);
    };
    let baud = match args.next().map(|baud| baud.parse()) {
        None => DEFAULT_BAUD,
        Some(Ok(baud)) => baud,
        Some(Err(_)) => {
            eprintln!("a baud rate is a number, like {DEFAULT_BAUD}");
            return ExitCode::from(2);
        }
    };
    let port_handle = match serialport::new(&port, baud)
        .timeout(Duration::from_millis(20))
        .open()
    {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("couldn't open {port}: {error} (is the app still running?)");
            return ExitCode::FAILURE;
        }
    };
    println!("{port} at {baud} baud");
    let mut link = Link {
        port: port_handle,
        partial: Vec::new(),
    };
    match run(&mut link) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lost the port: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_goes_out_exactly_as_the_app_sends_it() {
        // The same bytes as `nudge`'s own test: right one unit, slowest speed.
        assert_eq!(
            relative_move(1, 0),
            [
                0x81, 0x01, 0x06, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
                0xFF
            ]
        );
        assert_eq!(relative_move(-1, 0)[6..10], [0x0F; 4]);
    }

    #[test]
    fn replies_are_read_for_what_they_say() {
        assert_eq!(meaning(&[0x90, 0x41, 0xFF]), "got it (socket 1)");
        assert_eq!(meaning(&[0x90, 0x52, 0xFF]), "finished (socket 2)");
        assert_eq!(
            meaning(&[0x90, 0x61, 0x41, 0xFF]),
            "refused: command not executable (socket 1)"
        );
    }

    #[test]
    fn a_position_reply_reads_as_signed_units() {
        let reply = [
            0x90, 0x50, 0x0F, 0x0F, 0x0F, 0x0E, 0x00, 0x00, 0x01, 0x02, 0xFF,
        ];
        assert_eq!(position(&reply), Some((-2, 0x12)));
    }
}
