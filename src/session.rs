//! The interactive loop the two terminal front ends share — the full-screen
//! one and the bare command mode: reads keys, turns held keys into continuous
//! camera drives, and drains the worker thread's results.
//!
//! The two differ only in where their output goes, which is what [`Report`]
//! abstracts — so they can't drift apart on how movement actually behaves.
//! (The window has its own loop, since egui owns the frame; what it shares
//! with these is [`crate::keymap`] and the worker.)

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

/// How long to wait before asking again after a state query the camera didn't
/// answer.
///
/// A camera that has just been woken takes seconds to come up, and says
/// nothing at all meanwhile — so the snapshot that reports it awake is one
/// that has to be retried rather than one that arrives by itself. Longer than
/// the quiescence interval because a retry competes with the operator's own
/// commands on a 9600-baud line, and there is no hurry: nothing is waiting on
/// it but a readout.
pub const RETRY_INTERVAL: Duration = Duration::from_millis(1000);

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

/// What to say when a focus key is held against a camera that is focusing for
/// itself — including the way out, since the focus keys are otherwise
/// dead and nothing on screen says why.
pub const AUTO_FOCUS_HINT: &str = "auto focus is on \u{2014} press f to focus by hand";

/// What to say when a movement key is held against a camera that is asleep.
/// Names the way out for the same reason [`AUTO_FOCUS_HINT`] does: nothing
/// else on screen says why the keys stopped working.
pub const STANDBY_HINT: &str = "the camera is in standby \u{2014} press p to wake it";

/// What to say while the camera isn't answering questions about itself.
///
/// The plain truth, and better than the protocol error underneath it: the
/// commonest reason for it is a camera part-way through waking up, which is
/// not a fault and shouldn't be dressed as one.
pub const NO_ANSWER_HINT: &str = "waiting for the camera to answer...";

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
    /// When to ask the camera for a fresh snapshot, or `None` when what we
    /// have is current. One instant rather than a flag and a timestamp,
    /// because the two things that arm it — a command that changed something,
    /// and a query that went unanswered — differ only in how long to wait.
    next_query_at: Option<Instant>,
    held: Option<HeldKey>,
    /// Which way the camera said it was focusing, or `None` until it has said.
    /// The focus keys can't hold against a camera focusing for itself, so the
    /// loop has to know which it's doing before it starts one of those drives.
    auto_focus: Option<bool>,
    /// Whether the camera said it was awake, or `None` until it has said. A
    /// camera in standby ignores every movement key there is.
    power_on: Option<bool>,
}

impl<'a, R: Report> Session<'a, R> {
    fn new(intents: &'a Sender<Intent>, results: &'a Receiver<Outcome>, report: &'a mut R) -> Self {
        Self {
            intents,
            results,
            report,
            // Query once up front so the state display doesn't sit empty
            // until the first command, and without waiting for it.
            next_query_at: Some(Instant::now()),
            held: None,
            auto_focus: None,
            power_on: None,
        }
    }

    /// Sends a camera command, showing it as in progress right away — the
    /// real outcome, which can take seconds, arrives later via
    /// [`Self::drain`].
    fn send(&mut self, intent: Intent) -> io::Result<()> {
        let _ = self.intents.send(intent);
        self.next_query_at = Some(Instant::now() + QUIESCENCE_INTERVAL);
        // A drive is its own progress report — the picture moves — so it says
        // nothing, and the key legend stays up while the key is held.
        if worker::is_movement(intent) {
            return Ok(());
        }
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
            Action::Camera(intent) if key.kind == KeyEventKind::Press => {
                // A camera in standby has parked its lens and will refuse a
                // step just as it refuses a drive, so say so rather than
                // spending a round trip to be told.
                if worker::is_movement(intent) && self.power_on == Some(false) {
                    self.report.status(STANDBY_HINT)?;
                } else {
                    self.send(intent)?;
                }
            }
            Action::Camera(_) => {}
            Action::ToggleAutoFocus if key.kind == KeyEventKind::Press => {
                // Offer the mode that does something when the camera hasn't
                // said which it's in yet: manual focus is what the focus keys
                // need, and needing them is why anyone presses this.
                let auto = !self.auto_focus.unwrap_or(true);
                self.auto_focus = Some(auto);
                self.send(Intent::SetAutoFocus(auto))?;
            }
            Action::ToggleAutoFocus => {}
            Action::TogglePower if key.kind == KeyEventKind::Press => {
                // Offer the state that does something when the camera hasn't
                // said which it's in yet: waking it is what every other key
                // needs, and needing them is why anyone presses this.
                //
                // Nothing is recorded here. What the camera is doing is the
                // camera's to say, and it says so when it confirms the
                // command — see [`Self::drain`].
                self.send(Intent::SetPower(!self.power_on.unwrap_or(false)))?;
            }
            Action::TogglePower => {}
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
        // A camera in standby has parked its lens and will refuse the drive;
        // starting one anyway spends a round trip to be told so.
        if self.power_on == Some(false) {
            return self.report.status(STANDBY_HINT);
        }
        // A camera focusing for itself ignores a manual focus drive, so say
        // so rather than starting one that visibly does nothing.
        if matches!(hold, Hold::Focus(_)) && self.auto_focus == Some(true) {
            return self.report.status(AUTO_FOCUS_HINT);
        }

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
                Ok(outcome) => match &outcome {
                    Outcome::State(Ok(camera_state)) => {
                        self.power_on = Some(camera_state.power_on);
                        // A camera that isn't reporting its lens hasn't told
                        // us how it's focusing either, so stop claiming to
                        // know rather than holding the last answer.
                        self.auto_focus = camera_state.lens.map(|lens| lens.auto_focus);
                        // An awake camera that isn't answering for its lens
                        // yet is one still coming up, so keep asking until it
                        // does. A sleeping one has nothing more to say and is
                        // left alone until the operator asks for something.
                        if camera_state.power_on && camera_state.lens.is_none() {
                            self.next_query_at = Some(Instant::now() + RETRY_INTERVAL);
                        }
                        self.report
                            .camera_state(&worker::describe_outcome(&outcome))?;
                    }
                    // A confirmed switch is the camera's own word on what it
                    // is doing, and better than waiting for the next inquiry
                    // to come round to it.
                    Outcome::Done(Intent::SetPower(on), Ok(())) => {
                        self.power_on = Some(*on);
                        // Whichever way it went, the lens is no longer where
                        // it was: it has just parked or is about to unpark.
                        self.auto_focus = None;
                        self.report.status(&worker::describe_outcome(&outcome))?;
                    }
                    // A camera that didn't answer is usually one that is busy
                    // waking up, so ask again rather than reporting a fault
                    // and giving up on ever noticing it came back.
                    Outcome::State(Err(_)) => {
                        self.next_query_at = Some(Instant::now() + RETRY_INTERVAL);
                        self.report.status(NO_ANSWER_HINT)?;
                    }
                    // A drive that worked was already visible before its
                    // completion arrived; one that failed is the only kind
                    // worth interrupting the legend for.
                    Outcome::Done(intent, Ok(())) if worker::is_movement(*intent) => {}
                    _ => self.report.status(&worker::describe_outcome(&outcome))?,
                },
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
        if self.held.is_some() || self.next_query_at.is_none_or(|at| Instant::now() < at) {
            return;
        }
        let _ = self.intents.send(Intent::QueryState);
        self.next_query_at = None;
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
    use crate::{
        focus::{FocusDirection, FocusDrive},
        pan_tilt::Velocity,
        zoom::{ZoomDirection, ZoomDrive},
    };
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

    /// A key event for the drive tests below, which are about the lifetime of
    /// a held drive rather than about which key starts one.
    ///
    /// Arrows carry shift, because that is what a pan/tilt drive is now: an
    /// arrow on its own steps the camera once and holds nothing. Every other
    /// key means what it always did.
    fn key(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
        let modifiers = match code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => KeyModifiers::SHIFT,
            _ => KeyModifiers::NONE,
        };
        let mut event = KeyEvent::new(code, modifiers);
        event.kind = kind;
        event
    }

    /// An arrow key with nothing on it, which steps rather than driving.
    fn unshifted(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
        let mut event = KeyEvent::new(code, KeyModifiers::NONE);
        event.kind = kind;
        event
    }

    fn tap(code: KeyCode) -> KeyEvent {
        unshifted(code, KeyEventKind::Press)
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

    /// A camera state snapshot that differs only in how it is focusing.
    fn focusing(auto_focus: bool) -> crate::state::CameraState {
        crate::state::CameraState {
            power_on: true,
            lens: Some(crate::state::Lens {
                pan_tilt: grafton_visca::camera::PanTiltPosition::new(0, 0),
                zoom: grafton_visca::types::ZoomPosition::try_from(0u16).unwrap(),
                focus: grafton_visca::types::FocusPosition::new(0),
                auto_focus,
            }),
        }
    }

    /// What a camera in standby reports: its power, and nothing else.
    fn asleep() -> crate::state::CameraState {
        crate::state::CameraState {
            power_on: false,
            lens: None,
        }
    }

    fn zoom_direction(intent: Intent) -> ZoomDirection {
        match intent {
            Intent::DriveZoom(Some(ZoomDrive { direction, .. })) => direction,
            other => panic!("expected a zoom drive, got {other:?}"),
        }
    }

    fn focus_direction(intent: Intent) -> FocusDirection {
        match intent {
            Intent::DriveFocus(Some(FocusDrive { direction, .. })) => direction,
            other => panic!("expected a focus drive, got {other:?}"),
        }
    }

    fn pan_tilt_direction(intent: Intent) -> PanTiltDirection {
        match intent {
            Intent::DrivePanTilt(velocity) => velocity.direction,
            other => panic!("expected a pan/tilt drive, got {other:?}"),
        }
    }

    #[test]
    fn tapping_an_arrow_key_steps_the_camera_and_holds_nothing() {
        let mut harness = Harness::new();
        let held = {
            let mut session = harness.session();
            session.handle_key(tap(KeyCode::Right)).unwrap();
            session.held.is_some()
        };

        assert_eq!(
            harness.sent(),
            vec![Intent::Nudge(crate::nudge::Step::towards(
                PanTiltDirection::Right
            ))]
        );
        assert!(!held, "a step is over the moment it is asked for");
    }

    #[test]
    fn a_repeated_arrow_key_steps_once_rather_than_walking_away() {
        // Terminals without the enhancement protocol report a held key as
        // repeats, and a step per repeat would send the shot off across the
        // room. One press is one step, as it is for a preset.
        let mut harness = Harness::new();
        let mut session = harness.session();

        session.handle_key(tap(KeyCode::Right)).unwrap();
        for _ in 0..5 {
            session
                .handle_key(unshifted(KeyCode::Right, KeyEventKind::Repeat))
                .unwrap();
        }

        assert_eq!(harness.sent().len(), 1);
    }

    #[test]
    fn a_step_at_a_sleeping_camera_says_so_instead_of_being_sent() {
        let mut harness = Harness::new();
        harness.outcomes.send(Outcome::State(Ok(asleep()))).unwrap();
        let mut session = harness.session();
        session.drain().unwrap();

        session.handle_key(tap(KeyCode::Right)).unwrap();

        assert!(harness.sent().is_empty(), "a sleeping camera won't move");
        assert!(harness.report.statuses.contains(&STANDBY_HINT.to_string()));
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
    fn driving_leaves_the_status_line_to_say_something_else() {
        let mut harness = Harness::new();
        let mut session = harness.session();

        session.handle_key(press(KeyCode::Right)).unwrap();
        session.handle_key(release(KeyCode::Right)).unwrap();
        harness
            .outcomes
            .send(Outcome::Done(Intent::DrivePanTilt(Velocity::STOP), Ok(())))
            .unwrap();
        let mut session = harness.session();
        session.drain().unwrap();

        assert!(
            harness.report.statuses.is_empty(),
            "a move is its own report: {:?}",
            harness.report.statuses
        );
    }

    #[test]
    fn a_drive_that_failed_is_still_worth_saying() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::Done(
                Intent::DrivePanTilt(Velocity::STOP),
                Err(grafton_visca::Error::Timeout),
            ))
            .unwrap();

        harness.session().drain().unwrap();

        assert_eq!(harness.report.statuses.len(), 1);
        assert!(harness.report.statuses[0].contains("error"));
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
        assert_eq!(zoom_direction(sent[2]), ZoomDirection::In);
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

        let sent = harness.sent();
        assert_eq!(focus_direction(sent[0]), FocusDirection::Near);
        assert_eq!(sent[1], Intent::DriveFocus(None));
        assert_eq!(sent.len(), 2);
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
            session.next_query_at = Some(Instant::now());
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
            session.next_query_at = Some(Instant::now());
            session.query_if_due();
        }

        assert_eq!(harness.sent().last(), Some(&Intent::QueryState));
    }

    /// When the session next wants to ask the camera about itself, once it has
    /// taken up `outcome`.
    ///
    /// Sends the opening query first, the way the loop does, so that what's
    /// left is only what `outcome` itself asked for — otherwise every answer
    /// looks like it armed a retry.
    fn next_query_after(harness: &mut Harness, outcome: Outcome) -> Option<Instant> {
        harness.outcomes.send(outcome).unwrap();
        let mut session = harness.session();
        session.query_if_due();
        session.drain().unwrap();
        session.next_query_at
    }

    #[test]
    fn a_movement_key_against_a_sleeping_camera_says_so_instead_of_driving() {
        let mut harness = Harness::new();
        harness.outcomes.send(Outcome::State(Ok(asleep()))).unwrap();

        {
            let mut session = harness.session();
            session.drain().unwrap();
            session.handle_key(press(KeyCode::Right)).unwrap();
        }

        assert!(
            !harness
                .sent()
                .iter()
                .any(|intent| worker::is_movement(*intent)),
            "a camera in standby would only refuse the drive"
        );
        assert!(harness.report.statuses.contains(&STANDBY_HINT.to_string()));
    }

    #[test]
    fn p_wakes_a_camera_that_said_it_was_asleep() {
        let mut harness = Harness::new();
        harness.outcomes.send(Outcome::State(Ok(asleep()))).unwrap();

        {
            let mut session = harness.session();
            session.drain().unwrap();
            session.handle_key(press(KeyCode::Char('p'))).unwrap();
        }

        assert!(harness.sent().contains(&Intent::SetPower(true)));
    }

    #[test]
    fn p_puts_an_awake_camera_into_standby() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::State(Ok(focusing(false))))
            .unwrap();

        {
            let mut session = harness.session();
            session.drain().unwrap();
            session.handle_key(press(KeyCode::Char('p'))).unwrap();
        }

        assert!(harness.sent().contains(&Intent::SetPower(false)));
    }

    #[test]
    fn p_offers_to_wake_a_camera_that_has_not_said_which_it_is() {
        let mut harness = Harness::new();

        {
            let mut session = harness.session();
            session.handle_key(press(KeyCode::Char('p'))).unwrap();
        }

        // Waking is what every other key needs, and needing them is why
        // anyone reaches for this one.
        assert!(harness.sent().contains(&Intent::SetPower(true)));
    }

    #[test]
    fn the_camera_confirming_the_switch_is_what_teaches_the_session_about_power() {
        // Not the keypress: what the camera is doing is the camera's to say,
        // and it says so as soon as it confirms. Without this the next p
        // would ask for the same thing again until an inquiry came round.
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::Done(Intent::SetPower(true), Ok(())))
            .unwrap();

        {
            let mut session = harness.session();
            session.drain().unwrap();
            session.handle_key(press(KeyCode::Char('p'))).unwrap();
        }

        assert!(
            harness.sent().contains(&Intent::SetPower(false)),
            "a camera confirmed on should next be offered standby"
        );
    }

    #[test]
    fn waking_the_camera_stops_the_session_claiming_to_know_how_it_focuses() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::State(Ok(focusing(true))))
            .unwrap();
        harness
            .outcomes
            .send(Outcome::Done(Intent::SetPower(false), Ok(())))
            .unwrap();

        let mut session = harness.session();
        session.drain().unwrap();

        assert_eq!(
            session.auto_focus, None,
            "a camera that just parked its lens is no longer focusing any way at all"
        );
    }

    #[test]
    fn a_state_query_the_camera_did_not_answer_is_asked_again() {
        let mut harness = Harness::new();
        let outcome = Outcome::State(Err(grafton_visca::Error::Timeout));

        let next = next_query_after(&mut harness, outcome);

        assert!(next.is_some(), "an unanswered query should be retried");
        assert!(
            next.is_some_and(|at| at > Instant::now()),
            "and not straight away, which would be a spin"
        );
        assert!(
            harness
                .report
                .statuses
                .contains(&NO_ANSWER_HINT.to_string()),
            "an unanswered query should say so plainly, got {:?}",
            harness.report.statuses
        );
    }

    #[test]
    fn a_camera_that_is_awake_but_not_reporting_its_lens_yet_is_asked_again() {
        let mut harness = Harness::new();
        let waking = Outcome::State(Ok(crate::state::CameraState {
            power_on: true,
            lens: None,
        }));

        assert!(
            next_query_after(&mut harness, waking).is_some(),
            "a camera still coming up should be asked again"
        );
    }

    #[test]
    fn a_sleeping_camera_is_left_alone_rather_than_asked_over_and_over() {
        let mut harness = Harness::new();

        assert!(
            next_query_after(&mut harness, Outcome::State(Ok(asleep()))).is_none(),
            "standby is a settled answer, not one to keep asking about"
        );
    }

    #[test]
    fn a_camera_that_reports_its_lens_is_not_asked_again_until_something_changes() {
        let mut harness = Harness::new();
        let reporting = Outcome::State(Ok(focusing(false)));

        assert!(
            next_query_after(&mut harness, reporting).is_none(),
            "a complete answer is the end of the retry, not another round"
        );
    }

    #[test]
    fn a_camera_that_stops_reporting_its_lens_stops_claiming_a_focus_mode() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::State(Ok(focusing(true))))
            .unwrap();
        harness.outcomes.send(Outcome::State(Ok(asleep()))).unwrap();

        let mut session = harness.session();
        session.drain().unwrap();

        assert_eq!(
            session.auto_focus, None,
            "the last known focus mode isn't the camera's current answer"
        );
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
            .send(Outcome::State(Ok(focusing(false))))
            .unwrap();

        {
            let mut session = harness.session();
            assert!(session.drain().unwrap());
        }

        assert!(harness.report.statuses[0].contains("preset 3"));
        assert!(harness.report.camera_states[0].contains("power=on"));
    }

    #[test]
    fn f_hands_focus_back_and_forth_between_the_camera_and_the_keys() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::State(Ok(focusing(true))))
            .unwrap();
        {
            let mut session = harness.session();
            session.drain().unwrap();
            session.handle_key(press(KeyCode::Char('f'))).unwrap();
            session.handle_key(press(KeyCode::Char('f'))).unwrap();
        }

        assert_eq!(
            harness.sent(),
            vec![Intent::SetAutoFocus(false), Intent::SetAutoFocus(true)]
        );
    }

    #[test]
    fn f_asks_for_manual_focus_first_when_the_camera_has_not_said_which_it_is_in() {
        let mut harness = Harness::new();

        harness
            .session()
            .handle_key(press(KeyCode::Char('f')))
            .unwrap();

        assert_eq!(harness.sent(), vec![Intent::SetAutoFocus(false)]);
    }

    #[test]
    fn a_focus_key_says_why_it_does_nothing_while_the_camera_focuses_itself() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::State(Ok(focusing(true))))
            .unwrap();
        {
            let mut session = harness.session();
            session.drain().unwrap();
            session.handle_key(press(KeyCode::Char('.'))).unwrap();
            session.handle_key(release(KeyCode::Char('.'))).unwrap();
        }

        assert!(
            harness.sent().is_empty(),
            "a manual focus drive would be ignored by the camera"
        );
        assert!(
            harness
                .report
                .statuses
                .contains(&AUTO_FOCUS_HINT.to_string())
        );
    }

    #[test]
    fn the_focus_keys_drive_once_the_camera_stops_focusing_itself() {
        let mut harness = Harness::new();
        harness
            .outcomes
            .send(Outcome::State(Ok(focusing(false))))
            .unwrap();
        {
            let mut session = harness.session();
            session.drain().unwrap();
            session.handle_key(press(KeyCode::Char('.'))).unwrap();
        }

        let sent = harness.sent();
        assert_eq!(focus_direction(sent[0]), FocusDirection::Far);
        assert_eq!(sent.len(), 1);
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
