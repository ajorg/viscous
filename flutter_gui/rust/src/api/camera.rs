//! The Flutter-facing camera API: a thin translation layer over `viscous`'s
//! existing worker/connection/state types, kept in its own crate so the
//! `viscous` binary never depends on Flutter tooling (see the crate's
//! `Cargo.toml`: `flutter_rust_bridge` only appears here).
//!
//! Camera [`viscous::worker::Intent`]s and [`viscous::worker::Outcome`]s
//! aren't exposed directly across the bridge — `flutter_rust_bridge` can
//! only generate Dart bindings for types it scans in this crate, not in an
//! external one — so every type here is a small mirror that gets converted
//! at the boundary.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use grafton_visca::camera::{Connect, profiles::GenericVisca};
use crate::frb_generated::StreamSink;
use viscous::{
    app::{POLL_INTERVAL, QUIESCENCE_INTERVAL},
    connection::{
        DEFAULT_CAMERA_BAUD_RATES, ProbeOutcome, discover_baud_rate, format_version, query_version,
    },
    focus::FocusDirection,
    pan_tilt::NudgeDirection,
    worker::{self, Intent, Outcome},
    zoom::ZoomDirection,
};

/// One active camera connection: where GUI-originated intents go in, and the
/// worker's outcomes that haven't been claimed by [`subscribe_status`] yet.
struct Session {
    intents: Sender<Intent>,
    results: Option<Receiver<Outcome>>,
}

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn session() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

/// Forwards a camera intent to the active session's worker, if any (there's
/// nothing useful to do with a nudge that arrives with no camera connected,
/// same as the TUI/CLI silently dropping a keystroke sent to a dead worker).
fn send(intent: Intent) {
    if let Some(active) = session().lock().expect("session lock poisoned").as_ref() {
        let _ = active.intents.send(intent);
    }
}

/// Runs the same fire-immediately/debounce-the-state-query pattern as
/// [`viscous::app::run`] and [`viscous::cli::run`], but driven by gestures
/// arriving on `gui_intents` instead of terminal key events.
fn run_coordinator(gui_intents: Receiver<Intent>, worker_intents: Sender<Intent>) {
    let mut pending_state_query = true;
    let mut last_command_at = Instant::now() - QUIESCENCE_INTERVAL;

    loop {
        match gui_intents.recv_timeout(POLL_INTERVAL) {
            Ok(intent) => {
                if worker_intents.send(intent).is_err() {
                    return;
                }
                pending_state_query = true;
                last_command_at = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        if pending_state_query && last_command_at.elapsed() >= QUIESCENCE_INTERVAL {
            if worker_intents.send(Intent::QueryState).is_err() {
                return;
            }
            pending_state_query = false;
        }
    }
}

/// The eight directions a pan/tilt drag can move in, mirroring
/// [`viscous::pan_tilt::NudgeDirection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction8 {
    Up,
    Down,
    Left,
    Right,
    UpLeft,
    UpRight,
    DownLeft,
    DownRight,
}

impl From<Direction8> for NudgeDirection {
    fn from(direction: Direction8) -> Self {
        match direction {
            Direction8::Up => NudgeDirection::Up,
            Direction8::Down => NudgeDirection::Down,
            Direction8::Left => NudgeDirection::Left,
            Direction8::Right => NudgeDirection::Right,
            Direction8::UpLeft => NudgeDirection::UpLeft,
            Direction8::UpRight => NudgeDirection::UpRight,
            Direction8::DownLeft => NudgeDirection::DownLeft,
            Direction8::DownRight => NudgeDirection::DownRight,
        }
    }
}

/// What a successful connection attempt found, mirroring the summary
/// `main.rs` builds from [`viscous::connection::ProbeOutcome`].
#[derive(Debug, Clone)]
pub struct ConnectedInfo {
    pub baud_rate: u32,
    pub summary: String,
}

/// A snapshot of the camera's pan/tilt/zoom/focus/power state, mirroring
/// [`viscous::state::CameraState`] with only bridge-safe field types.
#[derive(Debug, Clone, Copy)]
pub struct CameraState {
    pub power_on: bool,
    pub pan: i32,
    pub tilt: i32,
    pub zoom: u16,
    pub focus: u16,
}

impl From<viscous::state::CameraState> for CameraState {
    fn from(state: viscous::state::CameraState) -> Self {
        CameraState {
            power_on: state.power_on,
            pan: i32::from(state.pan_tilt.pan),
            tilt: i32::from(state.pan_tilt.tilt),
            zoom: state.zoom.value(),
            focus: state.focus.value(),
        }
    }
}

/// One update from [`subscribe_status`]: either a fresh state snapshot, or a
/// human-readable description of a just-completed (or failed) command —
/// the same split [`viscous::ui::AppState`] renders into separate panels.
#[derive(Debug, Clone)]
pub enum StatusEvent {
    State(CameraState),
    Command(String),
}

impl From<Outcome> for StatusEvent {
    fn from(outcome: Outcome) -> Self {
        match &outcome {
            Outcome::State(Ok(state)) => StatusEvent::State((*state).into()),
            _ => StatusEvent::Command(worker::describe_outcome(&outcome)),
        }
    }
}

/// Discovers and connects to a camera on `port`, replaying the same
/// probe-then-reconnect sequence `main.rs` uses (discovery's own connection
/// only lives for the duration of the probe, so a second connection is
/// opened for the worker thread to hold onto), then starts that camera's
/// worker and debounce-coordinator threads.
///
/// Replaces any previously active session.
pub fn connect(port: String) -> Result<ConnectedInfo, String> {
    let outcome = discover_baud_rate(DEFAULT_CAMERA_BAUD_RATES, |baud_rate| {
        let camera = Connect::open_serial_blocking::<GenericVisca>(&port, baud_rate)?;
        query_version(&camera)
    });

    let (baud_rate, version) = match outcome {
        ProbeOutcome::Connected { baud_rate, version } => (baud_rate, version),
        ProbeOutcome::NoResponse => {
            return Err(format!(
                "No response from camera on {port} at any of the candidate baud rates: {DEFAULT_CAMERA_BAUD_RATES:?}"
            ));
        }
    };

    let camera = Connect::open_serial_blocking::<GenericVisca>(&port, baud_rate)
        .map_err(|error| format!("Connected during discovery but the follow-up connection failed: {error}"))?;

    let (worker_tx, worker_rx) = mpsc::channel::<Intent>();
    let (result_tx, result_rx) = mpsc::channel::<Outcome>();
    thread::spawn(move || worker::run(&camera, &worker_rx, &result_tx));

    let (gui_tx, gui_rx) = mpsc::channel::<Intent>();
    let coordinator_worker_tx = worker_tx.clone();
    thread::spawn(move || run_coordinator(gui_rx, coordinator_worker_tx));

    *session().lock().expect("session lock poisoned") = Some(Session {
        intents: gui_tx,
        results: Some(result_rx),
    });

    Ok(ConnectedInfo {
        baud_rate,
        summary: format_version(&version),
    })
}

/// Streams status updates for the active session to `sink` until the
/// worker thread goes away. Only one subscriber is ever served per
/// [`connect`] call — a second call while one is already streaming returns
/// immediately with no events, since a `Receiver` can't be split between
/// two consumers; the Flutter side should subscribe once (e.g. behind a
/// single `asBroadcastStream()`) rather than per-widget.
pub fn subscribe_status(sink: StreamSink<StatusEvent>) {
    let results = {
        let mut guard = session().lock().expect("session lock poisoned");
        guard.as_mut().and_then(|active| active.results.take())
    };
    let Some(results) = results else { return };

    for outcome in results {
        if sink.add(outcome.into()).is_err() {
            return;
        }
    }
}

/// Sends a single pan/tilt nudge in `direction`, `degrees` wide — the
/// caller (the joystick widget) is expected to derive `degrees` from how
/// far the drag is from center, same idea as the D70 Commander's jog area.
pub fn nudge_pan_tilt(direction: Direction8, degrees: f64) {
    send(Intent::NudgePanTilt(direction.into(), degrees));
}

pub fn nudge_zoom_in(millis: u64) {
    send(Intent::NudgeZoom(ZoomDirection::In, Duration::from_millis(millis)));
}

pub fn nudge_zoom_out(millis: u64) {
    send(Intent::NudgeZoom(ZoomDirection::Out, Duration::from_millis(millis)));
}

pub fn nudge_focus_near(millis: u64) {
    send(Intent::NudgeFocus(FocusDirection::Near, Duration::from_millis(millis)));
}

pub fn nudge_focus_far(millis: u64) {
    send(Intent::NudgeFocus(FocusDirection::Far, Duration::from_millis(millis)));
}

pub fn recall_preset(number: u8) {
    send(Intent::RecallPreset(number));
}

pub fn save_preset(number: u8) {
    send(Intent::SavePreset(number));
}

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}
