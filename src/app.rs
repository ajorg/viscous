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
    state,
    ui::{self, AppState, Connection},
    worker::{self, Intent, Outcome},
};

/// How long to wait for a key event before checking on worker results and
/// the quiescence timer.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait after the most recent camera-changing command before
/// requesting a fresh state snapshot. Debounces a burst of nudges (e.g.
/// holding a key down) into a single query once movement actually stops,
/// rather than polling on a fixed interval regardless of whether anything
/// changed.
const QUIESCENCE_INTERVAL: Duration = Duration::from_millis(300);

/// Applies one worker [`Outcome`] to `state`.
fn apply_outcome(state: &mut AppState, outcome: Outcome) {
    match outcome {
        Outcome::Done(intent, Ok(())) => {
            state.status = Some(format!("OK: {}", worker::describe(intent)));
        }
        Outcome::Done(intent, Err(error)) => {
            state.status = Some(format!("error ({}): {error}", worker::describe(intent)));
        }
        Outcome::State(Ok(camera_state)) => {
            state.camera_state = Some(state::format_state(&camera_state));
        }
        Outcome::State(Err(error)) => {
            state.status = Some(format!("state query failed: {error}"));
        }
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
    use crate::zoom::ZoomDirection;
    use grafton_visca::camera::PanTiltPosition;
    use grafton_visca::types::{FocusPosition, ZoomPosition};

    fn sample_camera_state() -> crate::state::CameraState {
        crate::state::CameraState {
            power_on: true,
            pan_tilt: PanTiltPosition::new(0, 0),
            zoom: ZoomPosition::try_from(0u16).unwrap(),
            focus: FocusPosition::new(0),
        }
    }

    #[test]
    fn successful_command_shows_confirmation_with_description() {
        let mut state = AppState {
            status: Some("stale".to_string()),
            ..AppState::default()
        };
        apply_outcome(&mut state, Outcome::Done(Intent::RecallPreset(3), Ok(())));
        let status = state.status.unwrap();
        assert!(status.contains("OK"));
        assert!(status.contains("preset 3"));
    }

    #[test]
    fn failed_command_sets_status_message_with_description() {
        let mut state = AppState::default();
        apply_outcome(
            &mut state,
            Outcome::Done(
                Intent::NudgeZoom(ZoomDirection::In, Duration::ZERO),
                Err(Error::Timeout),
            ),
        );
        let status = state.status.unwrap();
        assert!(status.contains("error"));
        assert!(status.contains("zoom in"));
    }

    #[test]
    fn successful_state_query_updates_camera_state() {
        let mut state = AppState::default();
        apply_outcome(&mut state, Outcome::State(Ok(sample_camera_state())));
        assert!(state.camera_state.unwrap().contains("power=on"));
    }

    #[test]
    fn failed_state_query_sets_status_message() {
        let mut state = AppState::default();
        apply_outcome(&mut state, Outcome::State(Err(Error::Timeout)));
        assert!(state.status.unwrap().contains("state query failed"));
    }
}
