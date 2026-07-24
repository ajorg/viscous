//! A bare, line-oriented command mode for when a full-screen TUI isn't
//! available (piped stdin/stdout, no controlling terminal) or isn't wanted
//! (scripted verification with a tool like `expect`).
//!
//! Prints the available commands once, then repeatedly reads a line, sends
//! the corresponding camera intent, and prints the result — the command's
//! own outcome, followed by the camera's resulting state for anything that
//! isn't already a state query itself.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{Receiver, Sender};

use crate::{
    focus::{FAST_FOCUS_NUDGE_DURATION, FOCUS_NUDGE_DURATION, FocusDirection},
    pan_tilt::{FAST_NUDGE_DEGREES, NUDGE_DEGREES, NudgeDirection},
    state,
    worker::{self, Intent, Outcome},
    zoom::{FAST_ZOOM_NUDGE_DURATION, ZOOM_NUDGE_DURATION, ZoomDirection},
};

pub const COMMAND_HELP: &str = "\
Commands:
  up, down, left, right                       pan/tilt nudge
  fast-up, fast-down, fast-left, fast-right    fast pan/tilt nudge
  zoom-in, zoom-out                            zoom nudge
  fast-zoom-in, fast-zoom-out                  fast zoom nudge
  focus-near, focus-far                        focus nudge
  fast-focus-near, fast-focus-far              fast focus nudge
  recall <1-6>                                 recall preset
  save <1-6>                                   save current position to preset
  state                                        show camera state
  quit                                         exit";

/// What one line of input means.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    Camera(Intent),
    Quit,
}

fn parse_preset_number(argument: Option<&str>) -> Result<u8, String> {
    let argument = argument.ok_or("missing preset number")?;
    argument
        .parse()
        .map_err(|_| format!("preset number must be a positive integer, got {argument:?}"))
}

fn parse_command(line: &str) -> Result<Command, String> {
    let mut words = line.split_whitespace();
    let word = words.next().ok_or("empty command")?;

    let intent = match word {
        "up" => Intent::NudgePanTilt(NudgeDirection::Up, NUDGE_DEGREES),
        "down" => Intent::NudgePanTilt(NudgeDirection::Down, NUDGE_DEGREES),
        "left" => Intent::NudgePanTilt(NudgeDirection::Left, NUDGE_DEGREES),
        "right" => Intent::NudgePanTilt(NudgeDirection::Right, NUDGE_DEGREES),
        "fast-up" => Intent::NudgePanTilt(NudgeDirection::Up, FAST_NUDGE_DEGREES),
        "fast-down" => Intent::NudgePanTilt(NudgeDirection::Down, FAST_NUDGE_DEGREES),
        "fast-left" => Intent::NudgePanTilt(NudgeDirection::Left, FAST_NUDGE_DEGREES),
        "fast-right" => Intent::NudgePanTilt(NudgeDirection::Right, FAST_NUDGE_DEGREES),

        "zoom-in" => Intent::NudgeZoom(ZoomDirection::In, ZOOM_NUDGE_DURATION),
        "zoom-out" => Intent::NudgeZoom(ZoomDirection::Out, ZOOM_NUDGE_DURATION),
        "fast-zoom-in" => Intent::NudgeZoom(ZoomDirection::In, FAST_ZOOM_NUDGE_DURATION),
        "fast-zoom-out" => Intent::NudgeZoom(ZoomDirection::Out, FAST_ZOOM_NUDGE_DURATION),

        "focus-near" => Intent::NudgeFocus(FocusDirection::Near, FOCUS_NUDGE_DURATION),
        "focus-far" => Intent::NudgeFocus(FocusDirection::Far, FOCUS_NUDGE_DURATION),
        "fast-focus-near" => Intent::NudgeFocus(FocusDirection::Near, FAST_FOCUS_NUDGE_DURATION),
        "fast-focus-far" => Intent::NudgeFocus(FocusDirection::Far, FAST_FOCUS_NUDGE_DURATION),

        "recall" => Intent::RecallPreset(parse_preset_number(words.next())?),
        "save" => Intent::SavePreset(parse_preset_number(words.next())?),

        "state" => Intent::QueryState,
        "quit" => return Ok(Command::Quit),

        other => return Err(format!("unknown command: {other}")),
    };
    Ok(Command::Camera(intent))
}

/// A short human description of an [`Outcome`], for the CLI transcript.
fn describe_outcome(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Done(intent, Ok(())) => format!("OK: {}", worker::describe(*intent)),
        Outcome::Done(intent, Err(error)) => {
            format!("error ({}): {error}", worker::describe(*intent))
        }
        Outcome::State(Ok(camera_state)) => state::format_state(camera_state),
        Outcome::State(Err(error)) => format!("state query failed: {error}"),
    }
}

/// Sends `intent` and waits for its outcome, returning `None` if the worker
/// thread has gone away (its result channel disconnected).
fn send_and_await(
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
    intent: Intent,
) -> Option<Outcome> {
    intents.send(intent).ok()?;
    results.recv().ok()
}

/// Runs the bare command loop: prints `connection_summary` and the command
/// list once, then reads lines from `stdin` until `quit` or end of input.
pub fn run(
    stdout: &mut impl Write,
    stdin: &mut impl BufRead,
    connection_summary: &str,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> io::Result<()> {
    writeln!(stdout, "{connection_summary}")?;
    writeln!(stdout, "{COMMAND_HELP}")?;

    let mut line = String::new();
    loop {
        write!(stdout, "> ")?;
        stdout.flush()?;

        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let command = match parse_command(line) {
            Ok(command) => command,
            Err(message) => {
                writeln!(stdout, "{message}")?;
                continue;
            }
        };
        let Command::Camera(intent) = command else {
            return Ok(());
        };
        let is_state_query = intent == Intent::QueryState;

        let Some(outcome) = send_and_await(intents, results, intent) else {
            writeln!(stdout, "camera worker is gone")?;
            return Ok(());
        };
        writeln!(stdout, "{}", describe_outcome(&outcome))?;

        // Show what the command actually did, unless it was already a state
        // query (which just showed it).
        if !is_state_query && matches!(outcome, Outcome::Done(_, Ok(()))) {
            match send_and_await(intents, results, Intent::QueryState) {
                Some(state_outcome) => writeln!(stdout, "{}", describe_outcome(&state_outcome))?,
                None => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::camera::PanTiltPosition;
    use grafton_visca::types::{FocusPosition, ZoomPosition};
    use std::io::Cursor;
    use std::sync::mpsc::channel;
    use std::thread;

    #[test]
    fn parse_command_maps_words_to_intents() {
        assert_eq!(
            parse_command("up"),
            Ok(Command::Camera(Intent::NudgePanTilt(
                NudgeDirection::Up,
                NUDGE_DEGREES
            )))
        );
        assert_eq!(
            parse_command("recall 3"),
            Ok(Command::Camera(Intent::RecallPreset(3)))
        );
        assert_eq!(parse_command("quit"), Ok(Command::Quit));
    }

    #[test]
    fn parse_command_rejects_unknown_words() {
        assert!(parse_command("bogus").is_err());
    }

    #[test]
    fn parse_command_requires_a_valid_preset_number_for_recall_and_save() {
        assert!(parse_command("recall").is_err());
        assert!(parse_command("recall abc").is_err());
        assert!(parse_command("save").is_err());
    }

    fn sample_camera_state() -> crate::state::CameraState {
        crate::state::CameraState {
            power_on: true,
            pan_tilt: PanTiltPosition::new(0, 0),
            zoom: ZoomPosition::try_from(0u16).unwrap(),
            focus: FocusPosition::new(0),
        }
    }

    /// Runs `run` against scripted input, with a stand-in "worker" thread
    /// that answers every intent immediately, and returns everything
    /// written to stdout.
    fn run_against_input(input: &str) -> String {
        let (intent_tx, intent_rx) = channel::<Intent>();
        let (result_tx, result_rx) = channel::<Outcome>();

        let responder = thread::spawn(move || {
            for intent in intent_rx {
                let outcome = match intent {
                    Intent::QueryState => Outcome::State(Ok(sample_camera_state())),
                    other => Outcome::Done(other, Ok(())),
                };
                if result_tx.send(outcome).is_err() {
                    break;
                }
            }
        });

        let mut stdin = Cursor::new(input.as_bytes());
        let mut stdout = Vec::new();
        run(&mut stdout, &mut stdin, "Connected", &intent_tx, &result_rx)
            .expect("in-memory stdout should never fail to write");

        drop(intent_tx);
        responder.join().unwrap();

        String::from_utf8(stdout).unwrap()
    }

    #[test]
    fn prints_connection_summary_and_command_help_once() {
        let output = run_against_input("quit\n");
        assert!(output.contains("Connected"));
        assert!(output.contains("Commands:"));
    }

    #[test]
    fn successful_command_reports_confirmation_and_new_state() {
        let output = run_against_input("up\nquit\n");
        assert!(output.contains("OK: pan/tilt up"));
        assert!(output.contains("power=on"));
    }

    #[test]
    fn state_command_reports_state_only_once() {
        let output = run_against_input("state\nquit\n");
        assert_eq!(output.matches("power=on").count(), 1);
    }

    #[test]
    fn unknown_command_reports_an_error_and_the_session_continues() {
        let output = run_against_input("bogus\nup\nquit\n");
        assert!(output.contains("unknown command: bogus"));
        assert!(output.contains("OK: pan/tilt up"));
    }

    #[test]
    fn end_of_input_ends_the_session_without_requiring_quit() {
        let output = run_against_input("up\n");
        assert!(output.contains("OK: pan/tilt up"));
    }
}
