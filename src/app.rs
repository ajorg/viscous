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
    worker::{Intent, Outcome},
};

/// How long to wait for a key event before checking on worker results and
/// the state-refresh timer.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How often to request a fresh camera state snapshot.
const STATE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Applies one worker [`Outcome`] to `state`.
fn apply_outcome(state: &mut AppState, outcome: Outcome) {
    match outcome {
        Outcome::Done(Ok(())) => state.status = None,
        Outcome::Done(Err(error)) => state.status = Some(format!("error: {error}")),
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
    let mut last_refresh = Instant::now() - STATE_REFRESH_INTERVAL;

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

        if last_refresh.elapsed() >= STATE_REFRESH_INTERVAL {
            let _ = intents.send(Intent::QueryState);
            last_refresh = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn successful_command_clears_status() {
        let mut state = AppState {
            status: Some("stale".to_string()),
            ..AppState::default()
        };
        apply_outcome(&mut state, Outcome::Done(Ok(())));
        assert_eq!(state.status, None);
    }

    #[test]
    fn failed_command_sets_status_message() {
        let mut state = AppState::default();
        apply_outcome(&mut state, Outcome::Done(Err(Error::Timeout)));
        assert!(state.status.unwrap().contains("error"));
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
