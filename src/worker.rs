//! Owns the camera, on a thread of its own, so a front end never waits on
//! VISCA's round trips — which for a preset recall can take as long as the
//! physical move itself.

use std::{
    future::Future,
    pin::Pin,
    sync::mpsc::{self as std_mpsc, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use grafton_visca::{
    Error,
    command::{PanTiltDirection, VersionInfo},
};
use tokio::sync::mpsc::{UnboundedReceiver, error::TryRecvError, unbounded_channel};

use crate::{
    connection::{self, Target},
    focus::{self, FocusDirection, FocusDrive},
    nudge::{self, Step},
    pan_tilt::{self, Velocity},
    power::Power,
    preset::{self, Memory},
    state::{self, CameraState},
    title::{self, Title, TitleDisplayInquiry},
    wire::{self, Wire},
    zoom::{self, ZoomDirection, ZoomDrive},
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
    /// Move a fixed distance from wherever the camera is now, and stop there.
    Nudge(Step),
    /// Start driving zoom, or stop it with `None`.
    DriveZoom(Option<ZoomDrive>),
    /// Start driving focus, or stop it with `None`.
    DriveFocus(Option<FocusDrive>),
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
    /// Give the camera a title to hold, without showing it.
    SetTitle(Title),
    /// Show the title the camera is holding, or hide it.
    ShowTitle(bool),
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
        // A step is deliberately not a control: two drives in the queue
        // together mean the later one won, but two steps mean two steps, and
        // `coalesced` sends both.
        Intent::Nudge(_)
        | Intent::SetAutoFocus(_)
        | Intent::RecallPreset(_)
        | Intent::SavePreset(_)
        | Intent::Home
        | Intent::ResetPanTilt
        | Intent::SetPower(_)
        | Intent::SetTitle(_)
        | Intent::ShowTitle(_)
        | Intent::QueryState => None,
    }
}

/// Whether this intent moves the picture, rather than being a command whose
/// result the operator can only learn from the status line.
///
/// Both front ends use it to keep movement out of that line: a move announces
/// itself in the picture, and saying "pan/tilt right..." for every one of them
/// would bury the messages nothing else carries. Steps count for the same
/// reason and more so — they are meant to be tapped out several at a time.
pub fn is_movement(intent: Intent) -> bool {
    control(intent).is_some() || matches!(intent, Intent::Nudge(_))
}

/// Whether this intent is one of the title commands, which are the one thing
/// here that a camera may not have at all.
///
/// `CAM_Title` belongs to the EVI-D70 generation and was dropped from the
/// ones that followed — an EVI-D80 answers a syntax error rather than
/// ignoring it — so a front end offering titles has to be able to tell that
/// refusal from any other failure and stop offering them.
pub fn is_title(intent: Intent) -> bool {
    matches!(intent, Intent::SetTitle(_) | Intent::ShowTitle(_))
}

/// Drops every queued drive command that a later one for the same control
/// has already superseded, leaving everything else in order.
///
/// Without this, a control held down while the camera is slow to answer
/// builds a backlog of velocities the user has already moved on from, and
/// releasing it stops nothing until that backlog has played out.
///
/// Steps are deliberately left alone, and are safe to leave alone. Nothing
/// generates them but a press — one per press, repeats ignored — so they
/// arrive no faster than a hand can produce them and no backlog can form. And
/// they must not be touched: a drive says what the camera should be doing
/// *now*, so only the last one still means anything, but a step says how far
/// to go from here, and every one of them is something the operator asked for.
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
        Intent::Nudge(step) => format!("step {}", direction_label(step.direction())),
        Intent::DriveZoom(None) => "zoom stop".to_string(),
        Intent::DriveZoom(Some(drive)) => match drive.direction {
            ZoomDirection::In => "zoom in".to_string(),
            ZoomDirection::Out => "zoom out".to_string(),
        },
        Intent::DriveFocus(None) => "focus stop".to_string(),
        Intent::DriveFocus(Some(drive)) => match drive.direction {
            FocusDirection::Near => "focus near".to_string(),
            FocusDirection::Far => "focus far".to_string(),
        },
        Intent::SetAutoFocus(true) => "auto focus".to_string(),
        Intent::SetAutoFocus(false) => "manual focus".to_string(),
        Intent::RecallPreset(number) => format!("recall preset {number}"),
        Intent::SavePreset(number) => format!("save preset {number}"),
        Intent::Home => "home".to_string(),
        Intent::ResetPanTilt => "pan/tilt reset".to_string(),
        Intent::SetPower(true) => "power on".to_string(),
        Intent::SetPower(false) => "power off".to_string(),
        Intent::SetTitle(title) => format!("set title \"{title}\""),
        Intent::ShowTitle(true) => "show title".to_string(),
        Intent::ShowTitle(false) => "hide title".to_string(),
        Intent::QueryState => "state query".to_string(),
    }
}

/// A short description of an [`Outcome`], as a single line of transcript
/// text. Used directly by the bare CLI; the TUI further routes it into
/// either the status line or the camera-state panel.
pub fn describe_outcome(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Done(intent, Ok(())) => format!("OK: {}", describe(*intent)),
        // Not "failed": the camera may well have done it and only the answer
        // went missing. What is known is that nothing came back.
        Outcome::Done(intent, Err(Error::Timeout)) => {
            format!("no answer from the camera ({})", describe(*intent))
        }
        Outcome::Done(intent, Err(error)) => format!("error ({}): {error}", describe(*intent)),
        Outcome::State(Ok(camera_state)) => state::format_state(camera_state),
        Outcome::State(Err(error)) => format!("state query failed: {error}"),
    }
}

/// Whether this is the camera not answering, as against the camera answering,
/// whatever it said.
///
/// A front end saying so is describing a condition rather than an event, so
/// the message is only true until the camera next answers anything; this is
/// what tells it when that has happened. A state query that failed counts,
/// because both front ends already report that as the camera not answering.
pub fn unanswered(outcome: &Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Done(_, Err(Error::Timeout)) | Outcome::State(Err(_))
    )
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

/// A connected camera, and the front end's way of talking to it.
pub struct Worker {
    /// How the connection was made, phrased for display.
    pub link: String,
    /// What the camera said it is.
    pub version: VersionInfo,
    /// Whether this camera answered for its title feature — asked once, as
    /// part of connecting, since it decides what the window offers.
    pub titles: bool,
    /// Where the front end sends what the operator asks for.
    pub intents: Sender<Intent>,
    /// What came of it.
    pub results: Receiver<Outcome>,
}

/// Connects to `target` on a thread of its own, which owns the camera from
/// then on, and returns once the camera has answered — or with a message fit
/// to show the user if it didn't.
///
/// Blocks for as long as connecting takes, so a front end with a window to
/// keep drawing calls this from a thread of its own.
///
/// `wake` is called whenever an outcome is waiting on [`Worker::results`], for
/// a front end that would otherwise have to keep checking.
pub fn start(target: Target, wake: impl Fn() + Send + 'static) -> Result<Worker, String> {
    let (ready_tx, ready_rx) = std_mpsc::channel();
    let (intent_tx, intent_rx) = std_mpsc::channel();
    let (result_tx, result_rx) = std_mpsc::channel();
    let results = Results::new(result_tx, wake);

    thread::Builder::new()
        .name("camera".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ =
                        ready_tx.send(Err(format!("Could not start the camera thread: {error}")));
                    return;
                }
            };
            runtime.block_on(async move {
                let opened = match connection::open(&target).await {
                    Ok(opened) => opened,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let camera = opened.session.camera();
                let titles = titles_supported(camera).await;
                if ready_tx
                    .send(Ok((opened.link, opened.version, titles)))
                    .is_err()
                {
                    return;
                }
                run(camera, forwarded(intent_rx), &results).await;
            });
            // Anything still running is the forwarder, waiting on a front end
            // that has gone; there is nothing left for it to wait for.
            runtime.shutdown_background();
        })
        .map_err(|error| format!("Could not start the camera thread: {error}"))?;

    let (link, version, titles) = ready_rx
        .recv()
        .map_err(|_| "The camera thread stopped while connecting".to_string())??;
    Ok(Worker {
        link,
        version,
        titles,
        intents: intent_tx,
        results: result_rx,
    })
}

/// Whether the camera has titles, asked as part of connecting.
async fn titles_supported(camera: &impl Wire) -> bool {
    let answer = tokio::time::timeout(title::ANSWER_TIME, camera.send(&TitleDisplayInquiry))
        .await
        .unwrap_or(Err(Error::Timeout));
    title::supported(&answer)
}

/// The front end's intents, moved across from the channel it sends them on
/// to one the worker can wait on alongside the camera.
///
/// The front ends are plain threads and keep a plain channel; only the worker
/// is async, and this is the whole of the seam between the two.
fn forwarded(intents: Receiver<Intent>) -> UnboundedReceiver<Intent> {
    let (forward, forwarded) = unbounded_channel();
    tokio::task::spawn_blocking(move || {
        for intent in intents {
            if forward.send(intent).is_err() {
                break;
            }
        }
    });
    forwarded
}

/// Runs the operator's intents against `camera` until they stop coming,
/// reporting what came of each to `results`.
///
/// Two lanes. Commands go one at a time, in the order they were asked for,
/// each waiting for the camera to take it — so a stop never overtakes the
/// drive it stops — and a drive superseded while it waited is never sent.
/// Questions about the camera's state go on beside them: the camera tracks a
/// question separately from its commands, so a command never waits for one,
/// however long the camera takes to answer or whether it does at all.
pub async fn run(camera: &impl Wire, mut intents: UnboundedReceiver<Intent>, results: &Results) {
    let mut waiting: Vec<Intent> = Vec::new();
    let mut command: Option<Pending<'_>> = None;
    let mut question: Option<Pending<'_>> = None;
    // Asked for while a question was already out, so asked again once it is
    // back: the answer in flight may predate a command that changed things.
    let mut ask_again = false;
    let mut listening = true;

    loop {
        if command.is_none() && !waiting.is_empty() {
            waiting = coalesced(&waiting);
            let intent = waiting.remove(0);
            command = Some(Box::pin(dispatch(camera, intent)));
        }
        if question.is_none() && ask_again {
            ask_again = false;
            question = Some(Box::pin(async {
                Outcome::State(state::query_state(camera).await)
            }));
        }
        // Gone quiet: everything asked for has been sent. A question still
        // out is left unanswered, since nobody is left to show it to.
        if !listening && command.is_none() && waiting.is_empty() {
            return;
        }

        tokio::select! {
            // The front end first: a stop that has just arrived is worth
            // knowing about before deciding what to send next.
            biased;
            intent = intents.recv(), if listening => {
                // Everything else already waiting too, so a drive that has
                // been superseded is seen to be before it is sent.
                let mut next = intent;
                loop {
                    match next {
                        Some(Intent::QueryState) => ask_again = true,
                        Some(intent) => waiting.push(intent),
                        None => listening = false,
                    }
                    match intents.try_recv() {
                        Ok(intent) => next = Some(intent),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => next = None,
                    }
                    if !listening {
                        break;
                    }
                }
            }
            outcome = finish(&mut command) => {
                command = None;
                if !results.report(outcome) {
                    return;
                }
            }
            outcome = finish(&mut question) => {
                question = None;
                if !results.report(outcome) {
                    return;
                }
            }
        }
    }
}

/// Where the worker reports what came of each intent, and how it tells the
/// front end there is something new to read.
///
/// The telling is what lets a window sleep until there is: without it, the
/// only way to notice a result is to keep looking.
pub struct Results {
    sender: Sender<Outcome>,
    wake: Box<dyn Fn() + Send>,
}

impl Results {
    /// Reports to `sender`, calling `wake` after each outcome is sent.
    pub fn new(sender: Sender<Outcome>, wake: impl Fn() + Send + 'static) -> Self {
        Self {
            sender,
            wake: Box::new(wake),
        }
    }

    /// Sends `outcome` and wakes the front end, or returns false if there is
    /// no front end left to send it to.
    fn report(&self, outcome: Outcome) -> bool {
        if self.sender.send(outcome).is_err() {
            return false;
        }
        (self.wake)();
        true
    }
}

/// Something sent to the camera whose outcome hasn't come back yet.
type Pending<'a> = Pin<Box<dyn Future<Output = Outcome> + 'a>>;

/// Waits for `pending` to come back, or forever if nothing is.
async fn finish(pending: &mut Option<Pending<'_>>) -> Outcome {
    match pending {
        Some(pending) => pending.await,
        None => std::future::pending().await,
    }
}

/// Sends what `intent` asks for and reports how it went, giving up once the
/// operator has waited for it as long as it deserves.
async fn dispatch(camera: &impl Wire, intent: Intent) -> Outcome {
    let result = match intent {
        Intent::DrivePanTilt(velocity) => match pan_tilt::drive(velocity) {
            Ok(command) => within(wire::AT_HAND, wire::execute(camera, command)).await,
            Err(error) => Err(error),
        },
        Intent::Nudge(step) => match nudge::nudge(step) {
            Some(command) => within(wire::AT_HAND, wire::execute(camera, command)).await,
            None => Ok(()),
        },
        Intent::DriveZoom(drive) => {
            within(
                wire::AT_HAND,
                wire::execute(camera, zoom::drive_zoom(drive)),
            )
            .await
        }
        Intent::DriveFocus(drive) => {
            within(
                wire::AT_HAND,
                wire::execute(camera, focus::drive_focus(drive)),
            )
            .await
        }
        Intent::SetAutoFocus(auto) => {
            within(
                wire::AT_HAND,
                wire::execute(camera, focus::set_auto_focus(auto)),
            )
            .await
        }
        Intent::RecallPreset(number) => match Memory::new(preset::Action::Recall, number) {
            Ok(command) => within(wire::ONE_OFF, wire::execute(camera, command)).await,
            Err(error) => Err(error),
        },
        Intent::SavePreset(number) => match Memory::new(preset::Action::Save, number) {
            Ok(command) => within(wire::ONE_OFF, wire::execute(camera, command)).await,
            Err(error) => Err(error),
        },
        Intent::Home => within(wire::ONE_OFF, wire::execute(camera, pan_tilt::HOME)).await,
        Intent::ResetPanTilt => within(wire::ONE_OFF, wire::execute(camera, pan_tilt::RESET)).await,
        Intent::SetPower(on) => within(wire::ONE_OFF, wire::execute(camera, Power(on))).await,
        Intent::SetTitle(text) => {
            within(wire::ONE_OFF, async {
                for part in title::set_title(text) {
                    wire::execute(camera, part).await?;
                }
                Ok(())
            })
            .await
        }
        Intent::ShowTitle(on) => {
            within(wire::AT_HAND, wire::execute(camera, title::show_title(on))).await
        }
        Intent::QueryState => return Outcome::State(state::query_state(camera).await),
    };
    Outcome::Done(intent, result)
}

/// `sending`, given up on as unanswered after `patience`.
async fn within(
    patience: Duration,
    sending: impl Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    tokio::time::timeout(patience, sending)
        .await
        .unwrap_or(Err(Error::Timeout))
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
    use crate::wire::scripted::{self, ScriptedCamera, done, done_in, power, silence, taken};
    use grafton_visca::testing::testkit::Step as Scripted;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::channel,
    };

    /// Runs `intents` against a camera replying as `script` says, with the
    /// front end gone once they're sent, and returns what came of them.
    async fn outcomes(script: Vec<Scripted>, intents: &[Intent]) -> Vec<Outcome> {
        let (camera, _) = scripted::camera(script).await;
        let (results, outcomes) = channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let woken = wakes.clone();
        let results = Results::new(results, move || {
            woken.fetch_add(1, Ordering::Relaxed);
        });
        let (sender, receiver) = unbounded_channel();
        for intent in intents {
            sender.send(*intent).unwrap();
        }
        drop(sender);
        run(&camera, receiver, &results).await;
        let outcomes: Vec<_> = outcomes.try_iter().collect();
        // Every outcome is one a front end waiting to be told would miss.
        assert_eq!(
            wakes.load(Ordering::Relaxed),
            outcomes.len(),
            "each outcome should wake the front end once"
        );
        outcomes
    }

    /// A worker running against `camera` for as long as the test needs it,
    /// and the two ends a front end would hold.
    struct Running {
        intents: tokio::sync::mpsc::UnboundedSender<Intent>,
        results: Receiver<Outcome>,
    }

    impl Running {
        fn start(camera: ScriptedCamera) -> Self {
            let (intents, receiver) = unbounded_channel();
            let (results, outcomes) = channel();
            let results = Results::new(results, || {});
            tokio::task::spawn_local(async move { run(&camera, receiver, &results).await });
            Self {
                intents,
                results: outcomes,
            }
        }

        /// The next outcome, waiting for it as a front end would.
        async fn next(&self) -> Outcome {
            loop {
                if let Ok(outcome) = self.results.try_recv() {
                    return outcome;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    }

    fn zoom_in() -> ZoomDrive {
        ZoomDrive {
            direction: ZoomDirection::In,
            speed: grafton_visca::types::ZoomSpeed::new(4).unwrap(),
        }
    }

    fn drive_right() -> Intent {
        Intent::DrivePanTilt(pan_tilt::velocity_from_axes(1.0, 0.0))
    }

    fn drive_left() -> Intent {
        Intent::DrivePanTilt(pan_tilt::velocity_from_axes(-1.0, 0.0))
    }

    #[tokio::test]
    async fn run_dispatches_each_intent_and_reports_its_result() {
        let outcomes = outcomes(
            vec![done(), done()],
            &[Intent::RecallPreset(1), Intent::DriveZoom(Some(zoom_in()))],
        )
        .await;

        assert!(matches!(
            outcomes[..],
            [
                Outcome::Done(Intent::RecallPreset(1), Ok(())),
                Outcome::Done(Intent::DriveZoom(_), Ok(()))
            ]
        ));
    }

    #[tokio::test]
    async fn run_reports_command_errors_without_stopping() {
        let syntax_error = Scripted::OnSend {
            matches: None,
            responses: vec![vec![0x90, 0x60, 0x02, 0xFF]],
        };
        let outcomes = outcomes(
            vec![syntax_error, done()],
            &[Intent::RecallPreset(1), Intent::RecallPreset(2)],
        )
        .await;

        assert!(
            matches!(
                outcomes[..],
                [Outcome::Done(_, Err(_)), Outcome::Done(_, Ok(()))]
            ),
            "the first was refused and the second still sent: {outcomes:?}"
        );
    }

    #[test]
    fn every_step_asked_for_is_a_step_taken() {
        // The opposite of what happens to a queued run of drives, and the
        // whole reason a step isn't one: five presses are five steps. Dropping
        // any of them — which is what the drive rule would do — would make a
        // tap something you can't count on, and a tap you can't count on is
        // the short drive this replaced.
        let taps = [Intent::Nudge(Step::towards(PanTiltDirection::Right)); 5];

        assert_eq!(coalesced(&taps), taps);
    }

    #[test]
    fn a_step_is_movement_so_it_keeps_out_of_the_status_line() {
        assert!(is_movement(Intent::Nudge(Step::towards(
            PanTiltDirection::Up
        ))));
        assert!(!is_movement(Intent::RecallPreset(1)));
    }

    #[test]
    fn describe_says_which_way_a_step_went() {
        assert_eq!(
            describe(Intent::Nudge(Step::towards(PanTiltDirection::UpLeft))),
            "step up-left"
        );
    }

    #[tokio::test]
    async fn run_sends_only_the_last_of_a_queued_run_of_drive_commands() {
        // A single scripted reply, so a second drive command reaching the
        // camera would go unanswered rather than silently pass.
        let outcomes = outcomes(
            vec![done()],
            &[
                drive_right(),
                drive_left(),
                Intent::DrivePanTilt(Velocity::STOP),
            ],
        )
        .await;

        assert!(
            matches!(
                outcomes[..],
                [Outcome::Done(Intent::DrivePanTilt(velocity), Ok(()))] if velocity.is_stop()
            ),
            "only the stop should have been sent: {outcomes:?}"
        );
    }

    #[tokio::test]
    async fn a_command_never_waits_for_a_question_the_camera_leaves_unanswered() {
        // The whole point of the second lane: the camera saying nothing about
        // its state costs the operator nothing.
        tokio::task::LocalSet::new()
            .run_until(async {
                let (camera, _) = scripted::camera(vec![silence(), done()]).await;
                let worker = Running::start(camera);

                worker.intents.send(Intent::QueryState).unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
                let started = Instant::now();
                worker.intents.send(drive_right()).unwrap();

                assert!(matches!(
                    worker.next().await,
                    Outcome::Done(Intent::DrivePanTilt(_), Ok(()))
                ));
                assert!(
                    started.elapsed() < Duration::from_millis(100),
                    "the drive waited {:?}",
                    started.elapsed()
                );
            })
            .await;
    }

    #[tokio::test]
    async fn a_step_whose_finish_never_comes_does_not_hold_up_the_stop_behind_it() {
        // The hang as it was met: the camera takes a step and never says it
        // has finished. Everything after it used to wait half a minute, and
        // every tap made in the meantime queued up behind it.
        tokio::task::LocalSet::new()
            .run_until(async {
                // The stop in the camera's other socket, since as far as it
                // knows the first is still busy with the step.
                let (camera, _) = scripted::camera(vec![taken(), done_in(2)]).await;
                let worker = Running::start(camera);
                let stop = Intent::DrivePanTilt(Velocity::STOP);
                let started = Instant::now();

                worker
                    .intents
                    .send(Intent::Nudge(Step::towards(PanTiltDirection::Right)))
                    .unwrap();
                assert!(matches!(
                    worker.next().await,
                    Outcome::Done(Intent::Nudge(_), Err(Error::Timeout))
                ));
                worker.intents.send(stop).unwrap();
                assert!(matches!(
                    worker.next().await,
                    Outcome::Done(answered, Ok(())) if answered == stop
                ));
                assert!(
                    started.elapsed() < wire::AT_HAND * 2,
                    "the stop got through after {:?}",
                    started.elapsed()
                );
            })
            .await;
    }

    #[tokio::test]
    async fn query_state_intent_reports_state() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (camera, _) = scripted::camera(vec![power(false)]).await;
                let worker = Running::start(camera);

                worker.intents.send(Intent::QueryState).unwrap();

                assert!(matches!(
                    worker.next().await,
                    Outcome::State(Ok(CameraState {
                        power_on: false,
                        ..
                    }))
                ));
            })
            .await;
    }

    #[test]
    fn coalescing_keeps_only_the_last_drive_for_each_control() {
        let batch = [
            drive_right(),
            Intent::DriveZoom(Some(zoom_in())),
            drive_left(),
            Intent::DriveZoom(None),
        ];
        assert_eq!(
            coalesced(&batch),
            vec![drive_left(), Intent::DriveZoom(None)]
        );
    }

    #[tokio::test]
    async fn run_dispatches_the_one_off_camera_commands() {
        let intents = [
            Intent::Home,
            Intent::ResetPanTilt,
            Intent::SetPower(false),
            Intent::SetAutoFocus(true),
            Intent::Nudge(Step::towards(PanTiltDirection::Left)),
            Intent::SavePreset(2),
            Intent::ShowTitle(true),
        ];
        let outcomes = outcomes(intents.iter().map(|_| done()).collect(), &intents).await;

        assert_eq!(outcomes.len(), intents.len(), "{outcomes:?}");
        for (outcome, intent) in outcomes.iter().zip(intents) {
            assert!(
                matches!(outcome, Outcome::Done(answered, Ok(())) if *answered == intent),
                "{} should reach the camera: {outcome:?}",
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
    fn the_title_commands_are_the_ones_a_camera_may_not_have() {
        assert!(is_title(Intent::SetTitle(title::Title::new("PODIUM"))));
        assert!(is_title(Intent::ShowTitle(true)));
        assert!(!is_title(Intent::RecallPreset(1)));
        assert!(!is_title(drive_right()));
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
            lens: Some(crate::state::Lens {
                pan_tilt: grafton_visca::camera::PanTiltPosition::new(0, 0),
                zoom: grafton_visca::types::ZoomPosition::try_from(0u16).unwrap(),
                focus: grafton_visca::types::FocusPosition::new(0),
                auto_focus: false,
            }),
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
        assert_eq!(describe(Intent::DriveZoom(Some(zoom_in()))), "zoom in");
        assert_eq!(describe(Intent::DriveZoom(None)), "zoom stop");
        assert_eq!(
            describe(Intent::DriveFocus(Some(FocusDrive {
                direction: FocusDirection::Near,
                speed: grafton_visca::command::FocusSpeed::new(4).unwrap(),
            }))),
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
            Intent::DriveZoom(Some(zoom_in())),
            Err(Error::SyntaxError),
        ));
        assert!(text.contains("error"));
        assert!(text.contains("zoom in"));
    }

    #[test]
    fn describe_outcome_says_an_unanswered_command_went_unanswered() {
        // Not that it failed: the camera may have moved and only its answer
        // gone missing.
        assert_eq!(
            describe_outcome(&Outcome::Done(
                Intent::Nudge(Step::towards(PanTiltDirection::Right)),
                Err(Error::Timeout),
            )),
            "no answer from the camera (step right)"
        );
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
