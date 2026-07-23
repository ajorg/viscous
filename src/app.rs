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
    focus::FocusDirection,
    keymap::{self, Action},
    pan_tilt::NudgeDirection,
    state,
    ui::{self, AppState, Connection},
    worker::{Intent, Outcome},
    zoom::ZoomDirection,
};

/// How long to wait for a key event before checking on worker results and
/// the state-refresh timer.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How often to request a fresh camera state snapshot.
const STATE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Applies one worker [`Outcome`] to `state`.
fn apply_outcome(state: &mut AppState, outcome: Outcome) {
    match outcome {
        Outcome::Done(intent, Ok(())) => {
            state.status = Some(format!("OK: {}", describe(intent)));
        }
        Outcome::Done(intent, Err(error)) => {
            state.status = Some(format!("error ({}): {error}", describe(intent)));
        }
        Outcome::State(Ok(camera_state)) => {
            state.camera_state = Some(state::format_state(&camera_state));
        }
        Outcome::State(Err(error)) => {
            state.status = Some(format!("state query failed: {error}"));
        }
    }
}

/// A short human description of an intent, for status messages.
fn describe(intent: Intent) -> String {
    match intent {
        Intent::NudgePanTilt(direction, _) => format!("pan/tilt {}", direction_label(direction)),
        Intent::NudgeZoom(ZoomDirection::In, _) => "zoom in".to_string(),
        Intent::NudgeZoom(ZoomDirection::Out, _) => "zoom out".to_string(),
        Intent::NudgeFocus(FocusDirection::Near, _) => "focus near".to_string(),
        Intent::NudgeFocus(FocusDirection::Far, _) => "focus far".to_string(),
        Intent::RecallPreset(number) => format!("recall preset {number}"),
        Intent::SavePreset(number) => format!("save preset {number}"),
        Intent::QueryState => "state query".to_string(),
    }
}

fn direction_label(direction: NudgeDirection) -> &'static str {
    match direction {
        NudgeDirection::Up => "up",
        NudgeDirection::Down => "down",
        NudgeDirection::Left => "left",
        NudgeDirection::Right => "right",
        NudgeDirection::UpLeft => "up-left",
        NudgeDirection::UpRight => "up-right",
        NudgeDirection::DownLeft => "down-left",
        NudgeDirection::DownRight => "down-right",
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

    #[test]
    fn describe_names_the_pan_tilt_direction() {
        assert_eq!(
            describe(Intent::NudgePanTilt(NudgeDirection::UpRight, 2.0)),
            "pan/tilt up-right"
        );
    }

    #[test]
    fn describe_distinguishes_save_from_recall() {
        assert_eq!(describe(Intent::RecallPreset(2)), "recall preset 2");
        assert_eq!(describe(Intent::SavePreset(2)), "save preset 2");
    }
}
