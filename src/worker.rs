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
    zoom::{self, ZoomDirection},
};

/// A camera action requested by the UI.
#[derive(Debug, Clone, Copy)]
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
}

fn dispatch<T>(camera: &BlockingClient<GenericVisca, T>, intent: Intent) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match intent {
        Intent::NudgePanTilt(direction, degrees) => {
            pan_tilt::nudge_pan_tilt(camera, direction, degrees)
        }
        Intent::NudgeZoom(direction, duration) => zoom::nudge_zoom(camera, direction, duration),
        Intent::NudgeFocus(direction, duration) => focus::nudge_focus(camera, direction, duration),
        Intent::RecallPreset(number) => preset::recall_preset(camera, number),
        Intent::SavePreset(number) => preset::save_preset(camera, number),
    }
}

/// Runs camera intents received from `intents` against `camera` until the
/// channel closes, sending each command's result to `results`.
///
/// Intended to run on its own thread, started once per connected camera, so
/// a slow completion round trip never blocks the UI thread. Stops early if
/// `results` is no longer being received (the UI side has gone away).
pub fn run<T>(
    camera: &BlockingClient<GenericVisca, T>,
    intents: &Receiver<Intent>,
    results: &Sender<Result<(), Error>>,
) where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    for intent in intents {
        let result = dispatch(camera, intent);
        if results.send(result).is_err() {
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
            result_rx.recv().unwrap().is_ok(),
            "preset recall should succeed"
        );
        assert!(
            result_rx.recv().unwrap().is_ok(),
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
            result_rx.recv().unwrap().is_err(),
            "first command was scripted to fail"
        );
        assert!(
            result_rx.recv().unwrap().is_ok(),
            "second command should still run"
        );
    }
}
