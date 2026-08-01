//! Runs camera commands on a dedicated thread so the UI thread never blocks
//! on VISCA's ack/completion round trip (which, for a preset recall, can take
//! as long as the physical move itself).

use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::PanTiltDirection,
    transport::{BlockingTransport, HasTransportConfig},
};

use crate::{
    focus::{self, FocusDirection},
    pan_tilt::{self, Velocity},
    power, preset,
    state::{self, CameraState},
    zoom::{self, ZoomDirection},
};

/// How long to wait for the camera to confirm a stop sent on the way out
/// before giving up on it. Long enough for a command that's merely waiting
/// its turn on the wire, short enough not to hold up quitting.
const STOP_CONFIRM_TIMEOUT: Duration = Duration::from_millis(500);

/// A camera action requested by the UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    /// Set the continuous pan/tilt drive, or stop it.
    DrivePanTilt(Velocity),
    /// Start driving zoom in a direction, or stop it with `None`.
    DriveZoom(Option<ZoomDirection>),
    /// Start driving focus in a direction, or stop it with `None`.
    DriveFocus(Option<FocusDirection>),
    /// Focus automatically, or leave focus to the manual drive.
    SetAutoFocus(bool),
    /// Recall the given 1-based preset number.
    RecallPreset(u8),
    /// Save the current position to the given 1-based preset number.
    SavePreset(u8),
    /// Return to the home position.
    Home,
    /// Recalibrate the pan/tilt mechanism.
    ResetPanTilt,
    /// Power the camera on, or put it into standby.
    SetPower(bool),
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

/// One of the camera's continuous controls.
///
/// Two intents for the same control sitting in the queue together make the
/// earlier one meaningless: a drive command isn't a step to be replayed, it's
/// a statement of what the camera should be doing *now*, and only the last
/// one still says that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    PanTilt,
    Zoom,
    Focus,
}

/// Which continuous control an intent drives, if any.
fn control(intent: Intent) -> Option<Control> {
    match intent {
        Intent::DrivePanTilt(_) => Some(Control::PanTilt),
        Intent::DriveZoom(_) => Some(Control::Zoom),
        Intent::DriveFocus(_) => Some(Control::Focus),
        Intent::SetAutoFocus(_)
        | Intent::RecallPreset(_)
        | Intent::SavePreset(_)
        | Intent::Home
        | Intent::ResetPanTilt
        | Intent::SetPower(_)
        | Intent::QueryState => None,
    }
}

/// Drops every queued drive command that a later one for the same control
/// has already superseded, leaving everything else in order.
///
/// Without this, a control held down while the camera is slow to answer
/// builds a backlog of velocities the user has already moved on from, and
/// releasing it stops nothing until that backlog has played out.
fn coalesced(batch: &[Intent]) -> Vec<Intent> {
    let mut superseded = Vec::new();
    let mut kept: Vec<Intent> = batch
        .iter()
        .rev()
        .copied()
        .filter(|intent| match control(*intent) {
            None => true,
            Some(control) if superseded.contains(&control) => false,
            Some(control) => {
                superseded.push(control);
                true
            }
        })
        .collect();
    kept.reverse();
    kept
}

/// A short human description of an intent, for status/log messages.
pub fn describe(intent: Intent) -> String {
    match intent {
        Intent::DrivePanTilt(velocity) => {
            format!("pan/tilt {}", direction_label(velocity.direction))
        }
        Intent::DriveZoom(None) => "zoom stop".to_string(),
        Intent::DriveZoom(Some(ZoomDirection::In)) => "zoom in".to_string(),
        Intent::DriveZoom(Some(ZoomDirection::Out)) => "zoom out".to_string(),
        Intent::DriveFocus(None) => "focus stop".to_string(),
        Intent::DriveFocus(Some(FocusDirection::Near)) => "focus near".to_string(),
        Intent::DriveFocus(Some(FocusDirection::Far)) => "focus far".to_string(),
        Intent::SetAutoFocus(true) => "auto focus".to_string(),
        Intent::SetAutoFocus(false) => "manual focus".to_string(),
        Intent::RecallPreset(number) => format!("recall preset {number}"),
        Intent::SavePreset(number) => format!("save preset {number}"),
        Intent::Home => "home".to_string(),
        Intent::ResetPanTilt => "pan/tilt reset".to_string(),
        Intent::SetPower(true) => "power on".to_string(),
        Intent::SetPower(false) => "power off".to_string(),
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

/// A short "in progress" description of an intent, shown as soon as it's
/// sent — before its ACK or Completion is known — so a slow command (a
/// preset recall can legitimately take tens of seconds) doesn't look like
/// nothing happened.
pub fn describe_busy(intent: Intent) -> String {
    format!("{}...", describe(intent))
}

fn direction_label(direction: PanTiltDirection) -> &'static str {
    match direction {
        PanTiltDirection::Up => "up",
        PanTiltDirection::Down => "down",
        PanTiltDirection::Left => "left",
        PanTiltDirection::Right => "right",
        PanTiltDirection::UpLeft => "up-left",
        PanTiltDirection::UpRight => "up-right",
        PanTiltDirection::DownLeft => "down-left",
        PanTiltDirection::DownRight => "down-right",
        PanTiltDirection::Stop => "stop",
    }
}

fn dispatch<T>(camera: &BlockingClient<GenericVisca, T>, intent: Intent) -> Outcome
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match intent {
        Intent::DrivePanTilt(velocity) => Outcome::Done(intent, pan_tilt::drive(camera, velocity)),
        Intent::DriveZoom(direction) => Outcome::Done(intent, zoom::drive_zoom(camera, direction)),
        Intent::DriveFocus(direction) => {
            Outcome::Done(intent, focus::drive_focus(camera, direction))
        }
        Intent::RecallPreset(number) => {
            Outcome::Done(intent, preset::recall_preset(camera, number))
        }
        Intent::SavePreset(number) => Outcome::Done(intent, preset::save_preset(camera, number)),
        Intent::SetAutoFocus(auto) => Outcome::Done(intent, focus::set_auto_focus(camera, auto)),
        Intent::Home => Outcome::Done(intent, pan_tilt::home(camera)),
        Intent::ResetPanTilt => Outcome::Done(intent, pan_tilt::reset(camera)),
        Intent::SetPower(on) => Outcome::Done(intent, power::set_power(camera, on)),
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
        // Anything that piled up while the previous command was on the wire
        // is waiting here, so take the whole backlog at once rather than one
        // per round trip: that's what makes it possible to notice a drive
        // command has already been superseded before bothering to send it.
        let mut batch = vec![intent];
        batch.extend(intents.try_iter());

        for intent in coalesced(&batch) {
            if results.send(dispatch(camera, intent)).is_err() {
                return;
            }
        }
    }
}

/// Sends `stop` and waits briefly for the camera to answer it.
///
/// Worth waiting for on the way out: a continuous drive keeps running on the
/// camera until something stops it, so a stop still sitting in the queue when
/// the process exits would leave the camera panning on its own.
pub fn stop_and_confirm(intents: &Sender<Intent>, results: &Receiver<Outcome>, stop: Intent) {
    if intents.send(stop).is_err() {
        return;
    }
    let deadline = Instant::now() + STOP_CONFIRM_TIMEOUT;
    // Outcomes for commands sent earlier can still be ahead of this one.
    while let Ok(outcome) = results.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        if matches!(outcome, Outcome::Done(intent, _) if intent == stop) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};
    use std::sync::mpsc::channel;

    fn scripted_camera(
        replies: Vec<grafton_visca::testing::testkit::Step>,
    ) -> BlockingClient<GenericVisca, ScriptedBlockingTransport> {
        grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(ScriptedBlockingTransport::new(replies))
            .expect("camera should build from a scripted transport")
    }

    fn drive_right() -> Intent {
        Intent::DrivePanTilt(pan_tilt::velocity_from_axes(1.0, 0.0))
    }

    fn drive_left() -> Intent {
        Intent::DrivePanTilt(pan_tilt::velocity_from_axes(-1.0, 0.0))
    }

    #[test]
    fn run_dispatches_each_intent_and_reports_its_result() {
        let camera = scripted_camera(vec![
            helpers::standard_command_response(1), // preset recall
            helpers::standard_command_response(1), // zoom drive
        ]);

        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();

        intent_tx.send(Intent::RecallPreset(1)).unwrap();
        intent_tx
            .send(Intent::DriveZoom(Some(ZoomDirection::In)))
            .unwrap();
        drop(intent_tx);

        run(&camera, &intent_rx, &result_tx);

        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(_, Ok(()))),
            "preset recall should succeed"
        );
        assert!(
            matches!(result_rx.recv().unwrap(), Outcome::Done(_, Ok(()))),
            "zoom drive should succeed"
        );
        assert!(result_rx.try_recv().is_err(), "no further results expected");
    }

    #[test]
    fn run_reports_command_errors_without_stopping() {
        let camera = scripted_camera(vec![
            helpers::errors::syntax_error(1),
            helpers::standard_command_response(1),
        ]);

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
    fn run_sends_only_the_last_of_a_queued_run_of_drive_commands() {
        // A single scripted reply, so a second drive command reaching the
        // camera would fail rather than silently pass.
        let camera = scripted_camera(vec![helpers::standard_command_response(1)]);

        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();

        intent_tx.send(drive_right()).unwrap();
        intent_tx.send(drive_left()).unwrap();
        intent_tx
            .send(Intent::DrivePanTilt(Velocity::STOP))
            .unwrap();
        drop(intent_tx);

        run(&camera, &intent_rx, &result_tx);

        assert!(
            matches!(
                result_rx.recv().unwrap(),
                Outcome::Done(Intent::DrivePanTilt(velocity), Ok(())) if velocity.is_stop()
            ),
            "only the stop should have been sent"
        );
        assert!(result_rx.try_recv().is_err(), "no further results expected");
    }

    #[test]
    fn query_state_intent_reports_state() {
        let camera = scripted_camera(vec![
            helpers::power_inquiry_response(true),
            helpers::errors::syntax_error(1), // next inquiry: fine to fail, we just want Outcome::State
        ]);

        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();

        intent_tx.send(Intent::QueryState).unwrap();
        drop(intent_tx);

        run(&camera, &intent_rx, &result_tx);

        assert!(matches!(result_rx.recv().unwrap(), Outcome::State(_)));
    }

    #[test]
    fn coalescing_keeps_only_the_last_drive_for_each_control() {
        let batch = [
            drive_right(),
            Intent::DriveZoom(Some(ZoomDirection::In)),
            drive_left(),
            Intent::DriveZoom(None),
        ];
        assert_eq!(
            coalesced(&batch),
            vec![drive_left(), Intent::DriveZoom(None)]
        );
    }

    #[test]
    fn run_dispatches_the_one_off_camera_commands() {
        let intents = [
            Intent::Home,
            Intent::ResetPanTilt,
            Intent::SetPower(false),
            Intent::SetAutoFocus(true),
        ];
        let camera = scripted_camera(
            intents
                .iter()
                .map(|_| helpers::standard_command_response(1))
                .collect(),
        );

        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();
        for intent in intents {
            intent_tx.send(intent).unwrap();
        }
        drop(intent_tx);

        run(&camera, &intent_rx, &result_tx);

        for intent in intents {
            assert!(
                matches!(result_rx.recv().unwrap(), Outcome::Done(done, Ok(())) if done == intent),
                "{} should reach the camera",
                describe(intent)
            );
        }
    }

    #[test]
    fn coalescing_leaves_one_off_commands_alone_and_in_order() {
        let batch = [
            Intent::RecallPreset(1),
            Intent::SavePreset(2),
            Intent::QueryState,
        ];
        assert_eq!(coalesced(&batch), batch.to_vec());
    }

    #[test]
    fn coalescing_keeps_a_drive_that_nothing_supersedes_in_place() {
        let batch = [drive_right(), Intent::RecallPreset(1)];
        assert_eq!(coalesced(&batch), batch.to_vec());
    }

    #[test]
    fn stop_and_confirm_sends_the_stop_and_returns_once_it_is_answered() {
        let (intent_tx, intent_rx) = channel();
        let (result_tx, result_rx) = channel();
        let stop = Intent::DrivePanTilt(Velocity::STOP);

        // An unrelated outcome ahead of the stop's own, as happens when an
        // earlier command is still being answered.
        result_tx
            .send(Outcome::Done(Intent::RecallPreset(1), Ok(())))
            .unwrap();
        result_tx.send(Outcome::Done(stop, Ok(()))).unwrap();

        stop_and_confirm(&intent_tx, &result_rx, stop);

        assert_eq!(intent_rx.try_recv().unwrap(), stop);
    }

    #[test]
    fn stop_and_confirm_gives_up_when_the_worker_is_gone() {
        let (intent_tx, intent_rx) = channel::<Intent>();
        let (_result_tx, result_rx) = channel::<Outcome>();
        drop(intent_rx);

        // Nowhere to send it and nothing to wait for: returns rather than
        // sitting out the timeout.
        stop_and_confirm(&intent_tx, &result_rx, Intent::DriveZoom(None));
    }

    fn sample_camera_state() -> CameraState {
        CameraState {
            power_on: true,
            pan_tilt: grafton_visca::camera::PanTiltPosition::new(0, 0),
            zoom: grafton_visca::types::ZoomPosition::try_from(0u16).unwrap(),
            focus: grafton_visca::types::FocusPosition::new(0),
            auto_focus: false,
        }
    }

    #[test]
    fn describe_names_the_pan_tilt_direction() {
        assert_eq!(
            describe(Intent::DrivePanTilt(pan_tilt::velocity_from_axes(1.0, 1.0))),
            "pan/tilt up-right"
        );
    }

    #[test]
    fn describe_calls_a_pan_tilt_stop_a_stop() {
        assert_eq!(
            describe(Intent::DrivePanTilt(Velocity::STOP)),
            "pan/tilt stop"
        );
    }

    #[test]
    fn describe_distinguishes_starting_a_drive_from_stopping_it() {
        assert_eq!(
            describe(Intent::DriveZoom(Some(ZoomDirection::In))),
            "zoom in"
        );
        assert_eq!(describe(Intent::DriveZoom(None)), "zoom stop");
        assert_eq!(
            describe(Intent::DriveFocus(Some(FocusDirection::Near))),
            "focus near"
        );
        assert_eq!(describe(Intent::DriveFocus(None)), "focus stop");
    }

    #[test]
    fn describe_says_which_way_a_two_state_setting_was_switched() {
        assert_eq!(describe(Intent::SetPower(true)), "power on");
        assert_eq!(describe(Intent::SetPower(false)), "power off");
        assert_eq!(describe(Intent::SetAutoFocus(true)), "auto focus");
        assert_eq!(describe(Intent::SetAutoFocus(false)), "manual focus");
    }

    #[test]
    fn describe_tells_home_apart_from_the_reset_that_ends_there() {
        assert_eq!(describe(Intent::Home), "home");
        assert_eq!(describe(Intent::ResetPanTilt), "pan/tilt reset");
    }

    #[test]
    fn describe_distinguishes_save_from_recall() {
        assert_eq!(describe(Intent::RecallPreset(2)), "recall preset 2");
        assert_eq!(describe(Intent::SavePreset(2)), "save preset 2");
    }

    #[test]
    fn describe_busy_marks_the_intent_as_in_progress() {
        assert_eq!(describe_busy(Intent::RecallPreset(2)), "recall preset 2...");
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
            Intent::DriveZoom(Some(ZoomDirection::In)),
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
