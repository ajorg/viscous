//! The interactive event loop: polls terminal input, dispatches camera
//! intents to the worker thread, and redraws.

use std::{
    sync::mpsc::{Receiver, Sender, TryRecvError},
    time::{Duration, Instant},
};

use crossterm::event::{self, Event};
use grafton_visca::Error;
use ratatui::DefaultTerminal;

use crate::{
    keymap::{self, Action},
    ui::{self, AppState, Connection},
    worker::{self, Intent, Outcome},
};

/// How long to wait for a key event before checking on worker results and
/// the quiescence timer. Shared with [`crate::cli`], which drives the same
/// poll/dispatch/drain loop against plain text instead of a rendered frame.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait after the most recent camera-changing command before
/// requesting a fresh state snapshot. Debounces a burst of nudges (e.g.
/// holding a key down) into a single query once movement actually stops,
/// rather than polling on a fixed interval regardless of whether anything
/// changed. Shared with [`crate::cli`]; see [`POLL_INTERVAL`].
pub(crate) const QUIESCENCE_INTERVAL: Duration = Duration::from_millis(300);

/// Applies one worker [`Outcome`] to `state`: everything but a successful
/// state query becomes the status line; a successful state query updates
/// the camera-state panel instead.
fn apply_outcome(state: &mut AppState, outcome: Outcome) {
    let text = worker::describe_outcome(&outcome);
    match outcome {
        Outcome::State(Ok(_)) => state.camera_state = Some(text),
        _ => state.status = Some(text),
    }
}

/// Runs the interactive event loop until the user quits or the worker
/// thread goes away.
pub fn run(
    terminal: &mut DefaultTerminal,
    connection: Connection,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> Result<(), Error> {
    let mut state = AppState {
        connection,
        ..AppState::default()
    };
    // Query once up front so the info panel doesn't sit on "(no state yet)"
    // until the first command; `elapsed() >= QUIESCENCE_INTERVAL` is already
    // true on the first loop iteration below.
    let mut pending_state_query = true;
    let mut last_command_at = Instant::now() - QUIESCENCE_INTERVAL;

    loop {
        terminal
            .draw(|frame| ui::render(frame, &state))
            .map_err(|e| Error::TransportError(e.to_string().into()))?;

        let has_key =
            event::poll(POLL_INTERVAL).map_err(|e| Error::TransportError(e.to_string().into()))?;
        if has_key {
            let event = event::read().map_err(|e| Error::TransportError(e.to_string().into()))?;
            if let Event::Key(key) = event {
                match keymap::map_key(key) {
                    Some(Action::Quit) => return Ok(()),
                    Some(Action::Camera(intent)) => {
                        let _ = intents.send(intent);
                        pending_state_query = true;
                        last_command_at = Instant::now();
                    }
                    None => {}
                }
            }
        }

        loop {
            match results.try_recv() {
                Ok(outcome) => apply_outcome(&mut state, outcome),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        if pending_state_query && last_command_at.elapsed() >= QUIESCENCE_INTERVAL {
            let _ = intents.send(Intent::QueryState);
            pending_state_query = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_camera_state() -> crate::state::CameraState {
        crate::state::CameraState {
            power_on: true,
            pan_tilt: grafton_visca::camera::PanTiltPosition::new(0, 0),
            zoom: grafton_visca::types::ZoomPosition::try_from(0u16).unwrap(),
            focus: grafton_visca::types::FocusPosition::new(0),
        }
    }

    #[test]
    fn a_successful_state_query_updates_the_camera_state_panel_not_the_status_line() {
        let mut state = AppState {
            status: Some("stale".to_string()),
            ..AppState::default()
        };
        apply_outcome(&mut state, Outcome::State(Ok(sample_camera_state())));
        assert!(state.camera_state.unwrap().contains("power=on"));
        assert_eq!(state.status.as_deref(), Some("stale"));
    }

    #[test]
    fn anything_else_updates_the_status_line_not_the_camera_state_panel() {
        let mut state = AppState::default();
        apply_outcome(&mut state, Outcome::Done(Intent::RecallPreset(3), Ok(())));
        assert!(state.status.unwrap().contains("preset 3"));
        assert_eq!(state.camera_state, None);
    }
}
