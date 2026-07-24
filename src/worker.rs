//! Runs camera commands on a dedicated thread so the UI thread never blocks
//! on VISCA's ack/completion round trip (which, for real movement commands,
//! can take as long as the physical move itself).

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    transport::{BlockingTransport, HasTransportConfig},
};

use crate::{
    focus::{self, FocusDirection},
    pan_tilt::{self, NudgeDirection},
    preset,
    state::{self, CameraState},
    zoom::{self, ZoomDirection},
};

/// A camera action requested by the UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    /// Nudge pan/tilt in a direction by the given number of degrees.
    NudgePanTilt(NudgeDirection, f64),
    /// Nudge zoom in a direction for the given duration.
    NudgeZoom(ZoomDirection, Duration),
    /// Nudge focus in a direction for the given duration.
    NudgeFocus(FocusDirection, Duration),
    /// Recall the given 1-based preset number.
    RecallPreset(u8),
    /// Save the current position to the given 1-based preset number.
    SavePreset(u8),
    /// Query the camera's current pan/tilt/zoom/focus/power state.
    QueryState,
}

/// The result of dispatching an [`Intent`], carrying data back for
/// `QueryState` rather than just success/failure, and the originating
/// intent for the others so the UI can describe what happened.
#[derive(Debug)]
pub enum Outcome {
    /// A command intent completed (or failed) with no data to report.
    Done(Intent, Result<(), Error>),
    /// A `QueryState` intent completed (or failed).
    State(Result<CameraState, Error>),
}

/// A short human description of an intent, for status/log messages.
pub fn describe(intent: Intent) -> String {
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

/// A short description of an [`Outcome`], as a single line of transcript
/// text. Used directly by the bare CLI; the TUI further routes it into
/// either the status line or the camera-state panel.
pub fn describe_outcome(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Done(intent, Ok(())) => format!("OK: {}", describe(*intent)),
        Outcome::Done(intent, Err(error)) => format!("error ({}): {error}", describe(*intent)),
        Outcome::State(Ok(camera_state)) => state::format_state(camera_state),
        Outcome::State(Err(error)) => format!("state query failed: {error}"),
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

fn dispatch<T>(camera: &BlockingClient<GenericVisca, T>, intent: Intent) -> Outcome
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match intent {
        Intent::NudgePanTilt(direction, degrees) => {
            Outcome::Done(intent, pan_tilt::nudge_pan_tilt(camera, direction, degrees))
        }
        Intent::NudgeZoom(direction, duration) => {
            Outcome::Done(intent, zoom::nudge_zoom(camera, direction, duration))
        }
        Intent::NudgeFocus(direction, duration) => {
            Outcome::Done(intent, focus::nudge_focus(camera, direction, duration))
        }
        Intent::RecallPreset(number) => {
            Outcome::Done(intent, preset::recall_preset(camera, number))
        }
        Intent::SavePreset(number) => Outcome::Done(intent, preset::save_preset(camera, number)),
        Intent::QueryState => Outcome::State(state::query_state(camera)),
    }
}

/// Runs camera intents received from `intents` against `camera` until the
/// channel closes, sending each command's outcome to `results`.
///
/// Intended to run on its own thread, started once per connected camera, so
/// a slow completion round trip never blocks the UI thread. Stops early if
/// `results` is no longer being received (the UI side has gone away).
pub fn run<T>(
    camera: &BlockingClient<GenericVisca, T>,
    intents: &Receiver<Intent>,
    results: &Sender<Outcome>,
) where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    for intent in intents {
        let outcome = dispatch(camera, intent);
        if results.send(outcome).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};
    use std::sync::mpsc::channel;

    #[test]
    fn run_dispatches_each_intent_and_reports_its_result() {
        let transport = ScriptedBlockingTransport::new(vec![
            helpers::standard_command_response(1), // preset recall
            helpers::standard_command_response(1), // zoom start
            helpers::standard_command_response(1), // zoom stop
        ]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();

        intent_tx.send(Intent::RecallPreset(1)).unwrap();
        intent_tx
            .send(Intent::NudgeZoom(ZoomDirection::In, Duration::ZERO))
            .unwrap();
        drop(intent_tx);

        run(&camera, &intent_rx, &result_tx);

        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(_, Ok(()))),
            "preset recall should succeed"
        );
        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(_, Ok(()))),
            "zoom nudge should succeed"
        );
        assert!(result_rx.try_recv().is_err(), "no further results expected");
    }

    #[test]
    fn run_reports_command_errors_without_stopping() {
        let transport = ScriptedBlockingTransport::new(vec![
            helpers::errors::syntax_error(1),
            helpers::standard_command_response(1),
        ]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();

        intent_tx.send(Intent::RecallPreset(1)).unwrap();
        intent_tx.send(Intent::RecallPreset(2)).unwrap();
        drop(intent_tx);

        run(&camera, &intent_rx, &result_tx);

        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(_, Err(_))),
            "first command was scripted to fail"
        );
        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(_, Ok(()))),
            "second command should still run"
        );
    }

    #[test]
    fn query_state_intent_reports_state() {
        let transport = ScriptedBlockingTransport::new(vec![
            helpers::power_inquiry_response(true),
            helpers::errors::syntax_error(1), // next inquiry: fine to fail, we just want Outcome::State
        ]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();

        intent_tx.send(Intent::QueryState).unwrap();
        drop(intent_tx);

        run(&camera, &intent_rx, &result_tx);

        assert!(matches!(result_rx.recv().unwrap(), Outcome::State(_)));
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

    fn sample_camera_state() -> CameraState {
        CameraState {
            power_on: true,
            pan_tilt: grafton_visca::camera::PanTiltPosition::new(0, 0),
            zoom: grafton_visca::types::ZoomPosition::try_from(0u16).unwrap(),
            focus: grafton_visca::types::FocusPosition::new(0),
        }
    }

    #[test]
    fn describe_outcome_confirms_a_successful_command() {
        assert_eq!(
            describe_outcome(&Outcome::Done(Intent::RecallPreset(2), Ok(()))),
            "OK: recall preset 2"
        );
    }

    #[test]
    fn describe_outcome_reports_a_failed_command() {
        let text = describe_outcome(&Outcome::Done(
            Intent::NudgeZoom(ZoomDirection::In, Duration::ZERO),
            Err(Error::Timeout),
        ));
        assert!(text.contains("error"));
        assert!(text.contains("zoom in"));
    }

    #[test]
    fn describe_outcome_formats_a_successful_state_query() {
        let text = describe_outcome(&Outcome::State(Ok(sample_camera_state())));
        assert!(text.contains("power=on"));
    }

    #[test]
    fn describe_outcome_reports_a_failed_state_query() {
        let text = describe_outcome(&Outcome::State(Err(Error::Timeout)));
        assert!(text.contains("state query failed"));
    }
}
