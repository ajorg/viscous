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
/// `QueryState` rather than just success/failure.
#[derive(Debug)]
pub enum Outcome {
    /// A command intent completed (or failed) with no data to report.
    Done(Result<(), Error>),
    /// A `QueryState` intent completed (or failed).
    State(Result<CameraState, Error>),
}

fn dispatch<T>(camera: &BlockingClient<GenericVisca, T>, intent: Intent) -> Outcome
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match intent {
        Intent::NudgePanTilt(direction, degrees) => {
            Outcome::Done(pan_tilt::nudge_pan_tilt(camera, direction, degrees))
        }
        Intent::NudgeZoom(direction, duration) => {
            Outcome::Done(zoom::nudge_zoom(camera, direction, duration))
        }
        Intent::NudgeFocus(direction, duration) => {
            Outcome::Done(focus::nudge_focus(camera, direction, duration))
        }
        Intent::RecallPreset(number) => Outcome::Done(preset::recall_preset(camera, number)),
        Intent::SavePreset(number) => Outcome::Done(preset::save_preset(camera, number)),
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
            matches!(result_rx.recv().unwrap(), Outcome::Done(Ok(()))),
            "preset recall should succeed"
        );
        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(Ok(()))),
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
            matches!(result_rx.recv().unwrap(), Outcome::Done(Err(_))),
            "first command was scripted to fail"
        );
        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(Ok(()))),
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
}
