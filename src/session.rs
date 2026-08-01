//! The interactive loop, shared by the TUI and the bare CLI: reads keys,
//! turns held keys into continuous camera drives, and drains the worker
//! thread's results.
//!
//! The two front ends differ only in where their output goes, which is what
//! [`Report`] abstracts — so they can't drift apart on how movement actually
//! behaves.

use std::io;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;

use crate::{
    keymap::{self, Action, Hold},
    worker::{self, Intent, Outcome},
};

/// How long to wait for a key event before checking on worker results, the
/// quiescence timer and whether a held key has gone quiet.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait after the most recent camera-changing command before
/// requesting a fresh state snapshot. Debounces a burst of commands into a
/// single query once things settle, rather than polling on a fixed interval
/// regardless of whether anything changed.
pub const QUIESCENCE_INTERVAL: Duration = Duration::from_millis(300);

/// How long a held key's drive survives without another press, repeat or
/// release arriving for it.
///
/// Terminals that implement the keyboard enhancement protocol — and the
/// Windows console, natively — report the release itself, which stops the
/// drive as soon as the key comes up; this timeout is the fallback for
/// terminals that only ever report presses, where the sole evidence a key is
/// still down is the terminal's own auto-repeat. It has to outlast the pause
/// before auto-repeat kicks in (commonly half a second) or a held key would
/// stutter, which is also how long the camera can overrun a release on those
/// terminals.
pub const HELD_KEY_TIMEOUT: Duration = Duration::from_millis(700);

/// Where a session's output goes: the TUI's status line and info panel, or
/// the bare CLI's transcript.
pub trait Report {
    /// Called once per loop pass, before it waits for input — the TUI's cue
    /// to draw a frame.
    fn refresh(&mut self) -> io::Result<()>;

    /// A command was sent, or a result other than a state snapshot came back.
    fn status(&mut self, text: &str) -> io::Result<()>;

    /// A fresh camera-state snapshot arrived.
    fn camera_state(&mut self, text: &str) -> io::Result<()>;
}

/// The movement key currently held down, and the drive it started.
#[derive(Debug)]
struct HeldKey {
    /// Which key is down. Tracked by key rather than by drive so that
    /// releasing one movement key can't stop a drive some other key started.
    code: KeyCode,
    hold: Hold,
    /// When this key was last heard from, as a press, repeat or release.
    last_seen: Instant,
}

/// One interactive session's state: what's been asked of the camera, what's
/// currently being driven, and when to ask for a fresh state snapshot.
struct Session<'a, R: Report> {
    intents: &'a Sender<Intent>,
    results: &'a Receiver<Outcome>,
    report: &'a mut R,
    pending_state_query: bool,
    last_command_at: Instant,
    held: Option<HeldKey>,
}

impl<'a, R: Report> Session<'a, R> {
    fn new(intents: &'a Sender<Intent>, results: &'a Receiver<Outcome>, report: &'a mut R) -> Self {
        Self {
            intents,
            results,
            report,
            // Query once up front so the state display doesn't sit empty
            // until the first command; the quiescence interval below is
            // already satisfied on the first pass.
            pending_state_query: true,
            last_command_at: Instant::now() - QUIESCENCE_INTERVAL,
            held: None,
        }
    }

    /// Sends a camera command, showing it as in progress right away — the
    /// real outcome, which can take seconds, arrives later via
    /// [`Self::drain`].
    fn send(&mut self, intent: Intent) -> io::Result<()> {
        let _ = self.intents.send(intent);
        self.pending_state_query = true;
        self.last_command_at = Instant::now();
        self.report.status(&worker::describe_busy(intent))
    }

    /// Applies one key event. Returns whether the user asked to quit.
    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        let Some(action) = keymap::map_key(key) else {
            return Ok(false);
        };
        match action {
            // One-shot actions fire on the way down only — a terminal
            // reporting repeats would otherwise recall a preset over and over
            // for as long as its key was held.
            Action::Quit => return Ok(key.kind == KeyEventKind::Press),
            Action::Camera(intent) if key.kind == KeyEventKind::Press => self.send(intent)?,
            Action::Camera(_) => {}
            Action::Hold(hold) => self.handle_hold(key, hold)?,
        }
        Ok(false)
    }

    /// Starts, refreshes or ends the drive a held movement key runs.
    fn handle_hold(&mut self, key: KeyEvent, hold: Hold) -> io::Result<()> {
        let is_current = self.held.as_ref().is_some_and(|held| held.code == key.code);

        match (is_current, key.kind) {
            (true, KeyEventKind::Release) => self.stop_hold()?,
            // A repeat of the key that's already driving. The camera is
            // already doing exactly what it asks, and re-sending the same
            // drive command would only put more traffic between here and the
            // stop that has to end the move.
            (true, _) => self.refresh_hold(),
            // A release of anything else, including a key whose drive has
            // already timed out: nothing to stop.
            (false, KeyEventKind::Release) => {}
            (false, _) => self.start_hold(key.code, hold)?,
        }
        Ok(())
    }

    fn refresh_hold(&mut self) {
        if let Some(held) = self.held.as_mut() {
            held.last_seen = Instant::now();
        }
    }

    fn start_hold(&mut self, code: KeyCode, hold: Hold) -> io::Result<()> {
        // Taking over from a key on the same control just re-aims it: the new
        // drive command replaces the old velocity outright. Taking over from a
        // different control would leave that one running, so stop it first.
        if let Some(previous) = self.held.take()
            && previous.hold.stop() != hold.stop()
        {
            self.send(previous.hold.stop())?;
        }
        self.send(hold.start())?;
        self.held = Some(HeldKey {
            code,
            hold,
            last_seen: Instant::now(),
        });
        Ok(())
    }

    fn stop_hold(&mut self) -> io::Result<()> {
        match self.held.take() {
            Some(held) => self.send(held.hold.stop()),
            None => Ok(()),
        }
    }

    /// Stops a held key's drive once its events stop arriving, for terminals
    /// that never report the release itself.
    fn stop_stale_hold(&mut self) -> io::Result<()> {
        if self
            .held
            .as_ref()
            .is_some_and(|held| held.last_seen.elapsed() >= HELD_KEY_TIMEOUT)
        {
            return self.stop_hold();
        }
        Ok(())
    }

    /// Reports every result waiting from the worker. Returns whether the
    /// worker is still there.
    fn drain(&mut self) -> io::Result<bool> {
        loop {
            match self.results.try_recv() {
                Ok(outcome) => {
                    let text = worker::describe_outcome(&outcome);
                    match outcome {
                        Outcome::State(Ok(_)) => self.report.camera_state(&text)?,
                        _ => self.report.status(&text)?,
                    }
                }
                Err(TryRecvError::Empty) => return Ok(true),
                Err(TryRecvError::Disconnected) => return Ok(false),
            }
        }
    }

    /// Fires the debounced state query once the most recent command has had
    /// time to settle.
    ///
    /// Skipped entirely while something is being driven: the camera's position
    /// is a moving target that would be stale by the time it was drawn, and a
    /// state query is four inquiry round trips on the same serial line — one
    /// in flight is one more thing the stop command has to wait behind.
    fn query_if_due(&mut self) {
        if self.held.is_some()
            || !self.pending_state_query
            || self.last_command_at.elapsed() < QUIESCENCE_INTERVAL
        {
            return;
        }
        let _ = self.intents.send(Intent::QueryState);
        self.pending_state_query = false;
    }

    /// Stops anything still being driven on the way out, waiting for the
    /// camera to confirm.
    ///
    /// A continuous drive outlives the process that started it, so quitting
    /// mid-move would otherwise leave the camera panning on its own.
    fn finish(&mut self) {
        if let Some(held) = self.held.take() {
            worker::stop_and_confirm(self.intents, self.results, held.hold.stop());
        }
    }
}

/// Asks the terminal to report key repeats and releases, so a held movement
/// key can drive the camera until it's actually let go rather than until its
/// auto-repeats stop arriving.
///
/// This is the kitty keyboard protocol's progressive-enhancement handshake:
/// terminals that don't implement it ignore the sequence, which is why it's
/// pushed unconditionally rather than probed for. The Windows console reports
/// releases natively and needs no handshake at all.
fn request_key_event_reporting() {
    let _ = execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(
            // Releases of plain-text keys (the zoom and focus bindings) only
            // come through with all keys reported as escape codes; the arrows
            // would manage with event types alone.
            KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        )
    );
}

fn stop_key_event_reporting() {
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
}

/// Runs the interactive loop until the user quits or the worker thread goes
/// away, reporting everything through `report`.
pub fn run<R: Report>(
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
    report: &mut R,
) -> io::Result<()> {
    request_key_event_reporting();
    let result = run_loop(intents, results, report);
    stop_key_event_reporting();
    result
}

fn run_loop<R: Report>(
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
    report: &mut R,
) -> io::Result<()> {
    let mut session = Session::new(intents, results, report);

    loop {
        session.report.refresh()?;

        if event::poll(POLL_INTERVAL)?
            && let Event::Key(key) = event::read()?
            && session.handle_key(key)?
        {
            session.finish();
            return Ok(());
        }

        session.stop_stale_hold()?;
        if !session.drain()? {
            return Ok(());
        }
        session.query_if_due();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{focus::FocusDirection, pan_tilt::Velocity, zoom::ZoomDirection};
    use crossterm::event::KeyModifiers;
    use grafton_visca::command::PanTiltDirection;
    use std::sync::mpsc::channel;

    /// A [`Report`] that just records what it was told, so tests can assert on
    /// what the user would have seen.
    #[derive(Default)]
    struct Recorder {
        refreshes: usize,
        statuses: Vec<String>,
        camera_states: Vec<String>,
    }

    impl Report for Recorder {
        fn refresh(&mut self) -> io::Result<()> {
            self.refreshes += 1;
            Ok(())
        }

        fn status(&mut self, text: &str) -> io::Result<()> {
            self.statuses.push(text.to_string());
            Ok(())
        }

        fn camera_state(&mut self, text: &str) -> io::Result<()> {
            self.camera_states.push(text.to_string());
            Ok(())
        }
    }

    /// A session wired to channels the test keeps both ends of.
    struct Harness {
        intents: Sender<Intent>,
        sent: Receiver<Intent>,
        outcomes: Sender<Outcome>,
        results: Receiver<Outcome>,
        report: Recorder,
    }

    impl Harness {
        fn new() -> Self {
            let (intents, sent) = channel();
            let (outcomes, results) = channel();
            Self {
                intents,
                sent,
                outcomes,
                results,
                report: Recorder::default(),
            }
        }

        fn session(&mut self) -> Session<'_, Recorder> {
            Session::new(&self.intents, &self.results, &mut self.report)
        }

        /// Every intent sent so far, draining the channel.
        fn sent(&self) -> Vec<Intent> {
            self.sent.try_iter().collect()
        }
    }

    fn key(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
        let mut event = KeyEvent::new(code, KeyModifiers::NONE);
        event.kind = kind;
        event
    }

    fn press(code: KeyCode) -> KeyEvent {
        key(code, KeyEventKind::Press)
    }

    fn repeat(code: KeyCode) -> KeyEvent {
        key(code, KeyEventKind::Repeat)
    }

    fn release(code: KeyCode) -> KeyEvent {
        key(code, KeyEventKind::Release)
    }

    /// Backdates the held key so the staleness check treats it as timed out.
    fn go_stale(session: &mut Session<'_, Recorder>) {
        let held = session.held.as_mut().expect("a key should be held");
        held.last_seen = Instant::now() - HELD_KEY_TIMEOUT;
    }

    fn pan_tilt_direction(intent: Intent) -> PanTiltDirection {
        match intent {
            Intent::DrivePanTilt(velocity) => velocity.direction,
            other => panic!("expected a pan/tilt drive, got {other:?}"),
        }
    }

    #[test]
    fn pressing_a_movement_key_starts_a_drive() {
        let mut harness = Harness::new();
        let mut session = harness.session();

        assert!(!session.handle_key(press(KeyCode::Right)).unwrap());

        let sent = harness.sent();
        assert_eq!(sent.len(), 1, "one drive command, got {sent:?}");
        assert_eq!(pan_tilt_direction(sent[0]), PanTiltDirection::Right);
    }

    #[test]
    fn repeats_of_a_held_key_send_nothing_further() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.handle_key(repeat(KeyCode::Right)).unwrap();
            session.handle_key(repeat(KeyCode::Right)).unwrap();
            session.handle_key(press(KeyCode::Right)).unwrap();
        }

        // The camera is already driving right; saying so again would only add
        // traffic ahead of the eventual stop.
        assert_eq!(harness.sent().len(), 1);
    }

    #[test]
    fn releasing_a_held_key_stops_the_drive() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.handle_key(release(KeyCode::Right)).unwrap();
            assert!(session.held.is_none());
        }

        let sent = harness.sent();
        assert_eq!(sent.len(), 2, "a drive then a stop, got {sent:?}");
        assert_eq!(sent[1], Intent::DrivePanTilt(Velocity::STOP));
    }

    #[test]
    fn a_held_key_that_goes_quiet_stops_the_drive() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.stop_stale_hold().unwrap();
            assert!(
                session.held.is_some(),
                "a key heard from just now is still held"
            );

            go_stale(&mut session);
            session.stop_stale_hold().unwrap();
            assert!(session.held.is_none());
        }

        assert_eq!(
            harness.sent().last(),
            Some(&Intent::DrivePanTilt(Velocity::STOP))
        );
    }

    #[test]
    fn a_repeat_keeps_a_drive_alive_past_the_timeout() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            go_stale(&mut session);
            session.handle_key(repeat(KeyCode::Right)).unwrap();
            session.stop_stale_hold().unwrap();
            assert!(
                session.held.is_some(),
                "the repeat should have kept the drive going"
            );
        }

        assert_eq!(harness.sent().len(), 1, "no stop should have been sent");
    }

    #[test]
    fn releasing_a_key_that_is_not_the_held_one_stops_nothing() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.handle_key(release(KeyCode::Left)).unwrap();
            assert!(session.held.is_some());
        }

        assert_eq!(harness.sent().len(), 1);
    }

    #[test]
    fn switching_direction_on_the_same_control_just_re_aims_it() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.handle_key(press(KeyCode::Up)).unwrap();
        }

        let sent = harness.sent();
        assert_eq!(sent.len(), 2, "no intervening stop needed, got {sent:?}");
        assert_eq!(pan_tilt_direction(sent[1]), PanTiltDirection::Up);
    }

    #[test]
    fn switching_to_another_control_stops_the_one_it_leaves_running() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.handle_key(press(KeyCode::Char(']'))).unwrap();
        }

        // A zoom drive wouldn't stop the pan, so the pan needs its own stop.
        let sent = harness.sent();
        assert_eq!(pan_tilt_direction(sent[0]), PanTiltDirection::Right);
        assert_eq!(sent[1], Intent::DrivePanTilt(Velocity::STOP));
        assert_eq!(sent[2], Intent::DriveZoom(Some(ZoomDirection::In)));
        assert_eq!(sent.len(), 3);
    }

    #[test]
    fn zoom_and_focus_keys_are_held_too() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Char(','))).unwrap();
            session.handle_key(release(KeyCode::Char(','))).unwrap();
        }

        assert_eq!(
            harness.sent(),
            vec![
                Intent::DriveFocus(Some(FocusDirection::Near)),
                Intent::DriveFocus(None),
            ]
        );
    }

    #[test]
    fn a_one_shot_command_fires_on_the_press_only() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Char('3'))).unwrap();
            session.handle_key(repeat(KeyCode::Char('3'))).unwrap();
            session.handle_key(release(KeyCode::Char('3'))).unwrap();
        }

        assert_eq!(harness.sent(), vec![Intent::RecallPreset(3)]);
    }

    #[test]
    fn quitting_happens_on_the_press_not_the_release() {
        let mut harness = Harness::new();
        let mut session = harness.session();

        assert!(!session.handle_key(release(KeyCode::Char('q'))).unwrap());
        assert!(session.handle_key(press(KeyCode::Char('q'))).unwrap());
    }

    #[test]
    fn an_unbound_key_does_nothing() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            assert!(!session.handle_key(press(KeyCode::Char('z'))).unwrap());
        }
        assert!(harness.sent().is_empty());
    }

    #[test]
    fn the_state_query_fires_once_things_settle() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.query_if_due();
            session.query_if_due();
        }

        assert_eq!(
            harness.sent(),
            vec![Intent::QueryState],
            "armed up front, and only once"
        );
    }

    #[test]
    fn the_state_query_waits_for_the_debounce() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.send(Intent::RecallPreset(1)).unwrap();
            session.query_if_due();
        }

        assert_eq!(harness.sent(), vec![Intent::RecallPreset(1)]);
    }

    #[test]
    fn the_state_query_is_suppressed_while_a_control_is_being_driven() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            // Long settled, but the camera is still moving: asking where it
            // is would only get in the way of the stop command.
            session.last_command_at = Instant::now() - QUIESCENCE_INTERVAL;
            session.query_if_due();
        }

        let sent = harness.sent();
        assert_eq!(sent.len(), 1, "only the drive, got {sent:?}");
        assert!(!sent.contains(&Intent::QueryState));
    }

    #[test]
    fn the_state_query_resumes_once_the_drive_stops() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.handle_key(release(KeyCode::Right)).unwrap();
            session.last_command_at = Instant::now() - QUIESCENCE_INTERVAL;
            session.query_if_due();
        }

        assert_eq!(harness.sent().last(), Some(&Intent::QueryState));
    }

    #[test]
    fn a_state_snapshot_goes_to_the_state_display_and_everything_else_to_the_status() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::Done(Intent::RecallPreset(3), Ok(())))
            .unwrap();
        harness
            .outcomes
            .send(Outcome::State(Ok(crate::state::CameraState {
                power_on: true,
                pan_tilt: grafton_visca::camera::PanTiltPosition::new(0, 0),
                zoom: grafton_visca::types::ZoomPosition::try_from(0u16).unwrap(),
                focus: grafton_visca::types::FocusPosition::new(0),
                auto_focus: false,
            })))
            .unwrap();

        {
            let mut session = harness.session();
            assert!(session.drain().unwrap());
        }

        assert!(harness.report.statuses[0].contains("preset 3"));
        assert!(harness.report.camera_states[0].contains("power=on"));
    }

    #[test]
    fn drain_reports_a_worker_that_has_gone_away() {
        let mut harness = Harness::new();
        let (orphan_outcomes, results) = channel();
        drop(orphan_outcomes);
        harness.results = results;

        let mut session = harness.session();
        assert!(!session.drain().unwrap());
    }

    #[test]
    fn finishing_stops_a_drive_that_is_still_running() {
        let mut harness = Harness::new();
        // The worker would answer the stop; stand in for it up front so the
        // confirmation wait doesn't sit out its timeout.
        harness
            .outcomes
            .send(Outcome::Done(Intent::DrivePanTilt(Velocity::STOP), Ok(())))
            .unwrap();
        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Right)).unwrap();
            session.finish();
        }

        assert_eq!(
            harness.sent().last(),
            Some(&Intent::DrivePanTilt(Velocity::STOP))
        );
    }

    #[test]
    fn finishing_with_nothing_running_sends_nothing() {
        let mut harness = Harness::new();
        {
            let mut session = harness.session();
            session.finish();
        }
        assert!(harness.sent().is_empty());
    }
}
