//! A bare command mode for when the TUI's full-screen redraws are more than
//! you want to parse from a script (e.g. driving `viscous` over an
//! `expect`-controlled pseudoterminal).
//!
//! Reads the exact same single-keystroke input as the TUI — see
//! [`keymap`](crate::keymap) — but skips the full-screen rendering: each
//! command's result is just printed as a plain line of text, so the session
//! reads as a linear transcript instead of a redrawn frame. Like the TUI,
//! this needs a real controlling terminal on stdin, since raw mode reads
//! individual keystrokes without waiting for Enter; it's an alternative to
//! the full-screen UI, not a way to drive the camera with no terminal at
//! all.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, Sender};

use crossterm::event::{self, Event, KeyEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::{
    keymap::{self, Action},
    state,
    ui::KEY_LEGEND,
    worker::{self, Intent, Outcome},
};

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

/// Handles one key event exactly as the TUI would, but writes the result as
/// plain text to `stdout` instead of updating a rendered frame.
///
/// Returns whether the session should continue: `false` on the quit key, or
/// once the worker thread has gone away.
fn handle_key(
    key: KeyEvent,
    stdout: &mut impl Write,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> io::Result<bool> {
    let Some(action) = keymap::map_key(key) else {
        return Ok(true);
    };
    let Action::Camera(intent) = action else {
        return Ok(false); // Action::Quit
    };

    let Some(outcome) = send_and_await(intents, results, intent) else {
        writeln!(stdout, "camera worker is gone")?;
        return Ok(false);
    };
    writeln!(stdout, "{}", describe_outcome(&outcome))?;

    // Show what the command actually did. No bound key maps to
    // Intent::QueryState itself (the TUI only issues it on its own
    // quiescence timer), so `outcome` is always `Done` here, never `State`.
    if matches!(outcome, Outcome::Done(_, Ok(()))) {
        match send_and_await(intents, results, Intent::QueryState) {
            Some(state_outcome) => writeln!(stdout, "{}", describe_outcome(&state_outcome))?,
            None => return Ok(false),
        }
    }
    Ok(true)
}

/// Runs the bare command loop: prints `connection_summary` and the key
/// legend once, then reads and acts on keystrokes until the quit key or the
/// worker thread goes away.
pub fn run(
    stdout: &mut impl Write,
    connection_summary: &str,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> io::Result<()> {
    writeln!(stdout, "{connection_summary}")?;
    writeln!(stdout, "{KEY_LEGEND}")?;

    enable_raw_mode()?;
    let result = (|| -> io::Result<()> {
        loop {
            if let Event::Key(key) = event::read()?
                && !handle_key(key, stdout, intents, results)?
            {
                return Ok(());
            }
        }
    })();
    disable_raw_mode()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use grafton_visca::camera::PanTiltPosition;
    use grafton_visca::types::{FocusPosition, ZoomPosition};
    use std::sync::mpsc::channel;
    use std::thread;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_camera_state() -> crate::state::CameraState {
        crate::state::CameraState {
            power_on: true,
            pan_tilt: PanTiltPosition::new(0, 0),
            zoom: ZoomPosition::try_from(0u16).unwrap(),
            focus: FocusPosition::new(0),
        }
    }

    /// Runs `body` with a stand-in "worker" thread that answers every
    /// intent it receives immediately.
    fn with_scripted_worker(body: impl FnOnce(&Sender<Intent>, &Receiver<Outcome>)) {
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

        body(&intent_tx, &result_rx);

        drop(intent_tx);
        responder.join().unwrap();
    }

    #[test]
    fn a_mapped_key_reports_confirmation_and_the_resulting_state() {
        with_scripted_worker(|intents, results| {
            let mut stdout = Vec::new();
            let continue_session =
                handle_key(press(KeyCode::Up), &mut stdout, intents, results).unwrap();
            assert!(continue_session);
            let output = String::from_utf8(stdout).unwrap();
            assert!(output.contains("OK: pan/tilt up"));
            assert!(output.contains("power=on"));
        });
    }

    #[test]
    fn a_failed_command_is_not_followed_by_a_state_report() {
        let (intent_tx, intent_rx) = channel::<Intent>();
        let (result_tx, result_rx) = channel::<Outcome>();
        let responder = thread::spawn(move || {
            for intent in intent_rx {
                if result_tx
                    .send(Outcome::Done(intent, Err(grafton_visca::Error::Timeout)))
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut stdout = Vec::new();
        let continue_session =
            handle_key(press(KeyCode::Up), &mut stdout, &intent_tx, &result_rx).unwrap();
        assert!(continue_session);

        drop(intent_tx);
        responder.join().unwrap();

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("error"));
        assert!(!output.contains("power="));
    }

    #[test]
    fn the_quit_key_ends_the_session_without_writing_anything() {
        with_scripted_worker(|intents, results| {
            let mut stdout = Vec::new();
            let continue_session =
                handle_key(press(KeyCode::Char('q')), &mut stdout, intents, results).unwrap();
            assert!(!continue_session);
            assert!(stdout.is_empty());
        });
    }

    #[test]
    fn an_unbound_key_is_ignored() {
        with_scripted_worker(|intents, results| {
            let mut stdout = Vec::new();
            let continue_session =
                handle_key(press(KeyCode::Char('z')), &mut stdout, intents, results).unwrap();
            assert!(continue_session);
            assert!(stdout.is_empty());
        });
    }
}
