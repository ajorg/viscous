//! `viscous` — the window you point the camera from.
//!
//! The pad, the rockers and the preset column are here; everything they ask
//! the camera for goes through the `viscous` library, the same way the
//! terminal front end's keys do — a path dependency on it and nothing in
//! between, so the intents this sends and the outcomes it draws are the
//! library's own types rather than copies of them.

// Windows executables default to the console subsystem, which pops up a
// terminal alongside the GUI window; switch to the windows subsystem in
// release builds only, so `cargo run` in debug still shows println!/log
// output in a normal terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod joystick;
mod pad;
mod rocker;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Key, TextWrapMode, Ui, Vec2, vec2};
use grafton_visca::{Error, command::PanTiltDirection};
use pad::Controller;
use viscous::{
    config,
    connection::{self, Target, format_camera, format_version},
    drives::Drives,
    focus::FocusDrive,
    gamepad::{Gamepad, Pad, Press},
    keymap::{FAST_KEY_DEFLECTION, KEY_DEFLECTION},
    nudge::Step,
    pan_tilt::{self, Velocity},
    session::{self, POLL_INTERVAL, QUIESCENCE_INTERVAL, RETRY_INTERVAL},
    state::{self, CameraState},
    title::{self, Title},
    worker::{self, Intent, Outcome},
    zoom::ZoomDrive,
};

/// What to offer in the camera field before anything has connected: the shape
/// of a serial port name on this platform, so the first run has something to
/// edit rather than something to compose.
#[cfg(windows)]
const FIRST_GUESS: &str = "COM3";
#[cfg(not(windows))]
const FIRST_GUESS: &str = "/dev/ttyUSB0";

/// How many preset slots to offer — the same six the TUI's number keys reach.
const PRESETS: u8 = 6;

/// How big a numbered recall button is drawn, and how tall the description
/// field and the Mark button beside it are, both as multiples of the height of
/// a line of body text.
///
/// The recall buttons get much the largest target on the window after the pad
/// itself, because of when they're used: mid-service, in a hurry, by someone
/// watching the picture rather than the screen. A miss there puts the wrong
/// shot on air. Fixed multiples of the text rather than a share of the window,
/// so they follow the interface scale and leave the window's spare room to the
/// pad and the rockers, which are the controls that get better for having it.
const RECALL_BUTTON: Vec2 = vec2(2.4, 2.6);
const FIELD: f32 = 1.7;

/// How many titles to keep on hand. The camera holds one at a time; these are
/// the ones an operator switches between during a session.
const TITLES: u8 = 3;

/// How often to draw a frame while a controller is plugged in.
///
/// A stick is read where a frame is drawn, so how often the window draws is how
/// often the sticks are noticed — and a hand that moves a stick expects the
/// shot to move with it, not a tenth of a second later.
const CONTROLLER_INTERVAL: Duration = Duration::from_millis(16);

/// A line of feedback for the operator, and whether it reports a failure —
/// the one kind that's worth colouring, since it's the one that needs looking
/// at rather than just reading.
struct Status {
    text: String,
    failed: bool,
}

impl Status {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            failed: false,
        }
    }

    fn failed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            failed: true,
        }
    }
}

/// Where the camera connection attempt currently stands.
enum Connection {
    Disconnected,
    Connecting,
    Connected {
        link: String,
        summary: String,
        /// The rest of the version reply, kept for the header's hover text —
        /// worth having to hand, not worth the width of the window.
        details: String,
    },
    Failed(String),
}

/// What a successful [`connect`] produces: how to describe the connection,
/// plus the channels the rest of the app uses to talk to the worker thread.
struct Worker {
    link: String,
    summary: String,
    details: String,
    /// Whether this camera answered for its title feature — asked once here,
    /// while the camera is still ours and before the worker thread has any
    /// commands of its own to get through.
    titles: bool,
    intents: Sender<Intent>,
    results: Receiver<Outcome>,
}

/// Connects to whatever `target` names — a serial port or a `tcp://` endpoint,
/// same string either front end takes — and starts the worker thread that owns
/// the camera from then on.
fn connect(target: &str) -> Result<Worker, String> {
    let connected = connection::connect(&Target::from(target))?;

    let (worker_tx, worker_rx) = mpsc::channel::<Intent>();
    let (result_tx, result_rx) = mpsc::channel::<Outcome>();
    let camera = connected.camera;
    // Asked here rather than through the worker: this is the one question
    // whose answer decides what the window offers, and it is worth having
    // before the first frame is drawn rather than a round trip afterwards.
    let titles = title::supported(&camera);
    thread::spawn(move || worker::run(&camera, &worker_rx, &result_tx));

    Ok(Worker {
        link: connected.link,
        summary: format_camera(&connected.version),
        details: format_version(&connected.version),
        titles,
        intents: worker_tx,
        results: result_rx,
    })
}

struct App {
    port_input: String,
    connection: Connection,
    connect_rx: Option<Receiver<Result<Worker, String>>>,
    intents: Option<Sender<Intent>>,
    results: Option<Receiver<Outcome>>,
    /// The camera's last reported state, kept as the camera reported it
    /// rather than as text: the power lamp and the focus-mode toggle answer
    /// from it, not just the readout at the bottom.
    camera_state: Option<CameraState>,
    status: Option<Status>,
    /// What each preset is of, in the operator's own words, keyed by the same
    /// 1-based number as the buttons.
    preset_labels: BTreeMap<u8, String>,
    /// Whether the Mark buttons are locked off.
    ///
    /// Locked at every start, and never written to the config file: a preset is
    /// slow to set up and instant to destroy, and the destroying button sits in
    /// the row you reach into to fix a typo. Marking is a thing done once, on
    /// purpose, so it costs one click to arm; recall — the thing done under
    /// pressure, mid-service — costs nothing and is never locked.
    marking_locked: bool,
    /// The titles kept on hand to burn into the video output, and which of
    /// them the camera was last told to show.
    titles: BTreeMap<u8, String>,
    shown_title: Option<u8>,
    /// Whether the camera has a title command at all. Asked of the camera as
    /// part of connecting, and asked again by implication every time a title
    /// is sent: a refusal turns it off too, for the camera that had nothing to
    /// say the first time. Never looked up from a model table — that would
    /// have to be kept, and the camera already knows.
    titles_supported: bool,
    /// The camera that last connected, remembered so the next run opens on it
    /// instead of on whatever this platform's ports are usually called.
    camera: Option<String>,
    /// Where the descriptions and titles are kept between runs, if this
    /// platform has somewhere to keep them.
    config_path: Option<PathBuf>,
    /// The window size most recently asked for, so the request only goes out
    /// when the size the contents need actually changes.
    requested_size: Option<Vec2>,
    /// The frame on which the window was sized around the controls, if that
    /// has happened yet.
    ///
    /// Up to and including that frame the pad asks for its natural size, so
    /// the window is fitted to what the layout needs rather than to whatever
    /// size it happened to open at; from the next frame on the window is the
    /// user's and the pad takes whatever room it is given. Counted in frames
    /// rather than kept as a flag because egui can lay a frame out more than
    /// once, and the two halves of that must not land in the same frame.
    fitted_at: Option<u64>,
    /// How much room the window offered the contents, and how much of it
    /// everything other than the pad took, when it was last drawn. The pad is
    /// given what's left over — measuring what the rest came to is steadier
    /// than counting it up from the style.
    room: Vec2,
    around_pad: Vec2,
    pad_size: f32,
    /// When to ask the camera for a fresh snapshot, or `None` when what we
    /// have is current. Same shape and the same reasoning as the TUI's.
    next_query_at: Option<Instant>,
    /// What each continuous control was last told to do. A drive keeps running
    /// on the camera by itself, so these say what it's already doing — and a
    /// command only goes out when a control is asked for something different.
    pan_tilt: Velocity,
    zoom: Option<ZoomDrive>,
    focus: Option<FocusDrive>,
    /// Where to read a game controller, when this platform has anywhere to read
    /// one, and enough memory of the last reading to tell a press from a hold.
    controller: Option<Box<dyn Controller>>,
    gamepad: Gamepad,
    /// Where the controller's sticks and triggers were when this frame started,
    /// which the pad and the rockers draw themselves at — so there is one place
    /// to watch the camera being driven, whatever is driving it.
    stick: Pad,
    /// Whether there was a controller to read last frame, which decides how
    /// soon the next frame is asked for.
    in_hand: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            port_input: FIRST_GUESS.to_string(),
            connection: Connection::Disconnected,
            connect_rx: None,
            intents: None,
            results: None,
            camera_state: None,
            status: None,
            preset_labels: BTreeMap::new(),
            marking_locked: true,
            titles: BTreeMap::new(),
            shown_title: None,
            titles_supported: true,
            camera: None,
            config_path: None,
            requested_size: None,
            fitted_at: None,
            room: Vec2::ZERO,
            around_pad: Vec2::ZERO,
            pad_size: 0.0,
            next_query_at: None,
            pan_tilt: Velocity::STOP,
            zoom: None,
            focus: None,
            controller: None,
            gamepad: Gamepad::default(),
            stick: Pad::default(),
            in_hand: false,
        }
    }
}

impl App {
    /// An app that keeps its preset descriptions in `path`, starting from
    /// whatever is already there.
    ///
    /// A config file that won't load says so in the status line rather than
    /// being silently replaced: the descriptions are typed by hand and worth
    /// more than the blank slate that would overwrite them.
    fn with_config(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            config_path: path,
            ..Self::default()
        };
        match app.config_path.as_deref().map(config::load) {
            Some(Ok(config)) => {
                app.preset_labels = config.presets;
                app.titles = config.titles;
                if let Some(camera) = config.camera {
                    app.port_input = camera.clone();
                    app.camera = Some(camera);
                }
            }
            Some(Err(error)) => app.status = Some(Status::failed(error.to_string())),
            None => {}
        }
        app
    }

    /// Sends a user-initiated intent: shows a busy message immediately (the
    /// real completion, which can take seconds, arrives later via
    /// [`Self::drain_results`]) and arms the debounced follow-up state
    /// query — the same pattern as [`viscous::app::run`].
    fn send_intent(&mut self, intent: Intent) {
        if let Some(intents) = &self.intents {
            let _ = intents.send(intent);
        }
        // A drive is its own progress report — the picture moves — and a drag
        // sends one of them per frame, which would leave nothing else legible.
        if !worker::is_movement(intent) {
            self.status = Some(Status::ok(worker::describe_busy(intent)));
        }
        self.next_query_at = Some(Instant::now() + QUIESCENCE_INTERVAL);
    }

    /// Whether the camera is focusing for itself, in which case the focus
    /// controls have nothing to drive: the camera overrides them the moment it
    /// sees the scene again. Unknown counts as not, so the controls stay live
    /// until the camera has actually said otherwise.
    fn auto_focusing(&self) -> bool {
        self.camera_state
            .and_then(|state| state.lens)
            .is_some_and(|lens| lens.auto_focus)
    }

    /// Whether the camera has said it is in standby, in which case it will
    /// refuse everything the controls could ask of it. Unknown counts as
    /// awake, so nothing is greyed out until the camera has actually said so.
    fn asleep(&self) -> bool {
        self.camera_state.is_some_and(|state| !state.power_on)
    }

    /// Whether any control is currently being driven.
    fn driving(&self) -> bool {
        !self.pan_tilt.is_stop() || self.zoom.is_some() || self.focus.is_some()
    }

    /// Fires the debounced state query once the most recent command has had
    /// time to settle, same timing and same suppression-while-driving rule as
    /// [`viscous::session::run`].
    ///
    /// Skipped while a control is being driven: the camera's position is a
    /// moving target that would be stale by the time it was drawn, and a state
    /// query is four inquiry round trips on the same serial line — one in
    /// flight is one more thing the stop command has to wait behind.
    fn send_query_state_if_due(&mut self) {
        if self.driving() || self.next_query_at.is_none_or(|at| Instant::now() < at) {
            return;
        }
        if let Some(intents) = &self.intents {
            let _ = intents.send(Intent::QueryState);
        }
        self.next_query_at = None;
    }

    fn start_connect(&mut self, ctx: &egui::Context) {
        let port = self.port_input.trim().to_string();
        self.connection = Connection::Connecting;
        let (tx, rx) = mpsc::channel();
        self.connect_rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(connect(&port));
            ctx.request_repaint();
        });
    }

    fn poll_connect(&mut self) {
        let Some(rx) = &self.connect_rx else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.poll_connect_result(result);
    }

    /// Takes up whatever the connection attempt came back with.
    fn poll_connect_result(&mut self, result: Result<Worker, String>) {
        self.connect_rx = None;
        match result {
            Ok(worker) => {
                // Only a camera that answered is worth coming back to; a
                // typo that failed would just be in the way next time.
                self.camera = Some(self.port_input.trim().to_string());
                self.save_config();
                self.connection = Connection::Connected {
                    link: worker.link,
                    summary: worker.summary,
                    details: worker.details,
                };
                // Per camera, not per run: connecting to a different one is
                // asking the question again, and its answer replaces the last
                // camera's.
                self.titles_supported = worker.titles;
                self.intents = Some(worker.intents);
                self.results = Some(worker.results);
                // Query once up front so the info panel doesn't sit empty
                // until the first command, and without waiting for it.
                self.next_query_at = Some(Instant::now());
            }
            Err(error) => self.connection = Connection::Failed(error),
        }
    }

    /// Applies one worker [`Outcome`]: everything but a successful state
    /// query becomes the status line; a successful state query updates the
    /// camera-state panel instead — mirrors the TUI's own `Report` impl.
    fn apply_outcome(&mut self, outcome: Outcome) {
        let failed = match &outcome {
            Outcome::State(Ok(camera_state)) => {
                self.camera_state = Some(*camera_state);
                // An awake camera that isn't answering for its lens yet is one
                // still coming up, so keep asking until it does. A sleeping
                // one has nothing more to say and is left alone.
                if camera_state.power_on && camera_state.lens.is_none() {
                    self.next_query_at = Some(Instant::now() + RETRY_INTERVAL);
                }
                return;
            }
            // A confirmed switch is the camera's own word on what it is doing,
            // and better than waiting for the next inquiry to come round to
            // it. Whichever way it went, the lens is no longer where it was:
            // it has just parked, or is about to unpark.
            Outcome::Done(Intent::SetPower(on), Ok(())) => {
                self.camera_state = Some(CameraState {
                    power_on: *on,
                    lens: None,
                });
                // Falls through to the status line: the switch worked, and
                // saying so is what every other one-off command does.
                false
            }
            // A camera that didn't answer is usually one that is busy waking
            // up, so ask again rather than reporting a fault and giving up on
            // ever noticing it came back.
            Outcome::State(Err(_)) => {
                self.next_query_at = Some(Instant::now() + RETRY_INTERVAL);
                self.status = Some(Status::ok(session::NO_ANSWER_HINT.to_string()));
                return;
            }
            // A camera that answers a syntax error to a title command hasn't
            // been sent a bad one — it has no such command. `CAM_Title` was
            // the EVI-D70 generation's; the cameras after it dropped the
            // feature and refuse both halves of it. So stop offering what
            // this camera can't do, and say that rather than quoting the
            // protocol at whoever ticked the box.
            //
            // Connecting asks the same question up front, and this is what
            // catches the camera that didn't answer it — one that was still
            // waking, or on a link that dropped the reply.
            Outcome::Done(intent, Err(Error::SyntaxError)) if worker::is_title(*intent) => {
                self.titles_supported = false;
                self.shown_title = None;
                self.status = Some(Status {
                    text: NO_TITLES_HINT.to_string(),
                    failed: true,
                });
                return;
            }
            // A drive that worked was already visible before its completion
            // arrived; one that failed is the only kind still worth saying.
            Outcome::Done(intent, Ok(())) if worker::is_movement(*intent) => return,
            Outcome::Done(_, result) => result.is_err(),
        };
        self.status = Some(Status {
            text: worker::describe_outcome(&outcome),
            failed,
        });
    }

    fn drain_results(&mut self) {
        let Some(results) = &self.results else { return };
        let outcomes: Vec<_> = results.try_iter().collect();
        for outcome in outcomes {
            self.apply_outcome(outcome);
        }
    }

    /// Draws one frame and sizes the window around what it drew.
    fn draw(&mut self, ui: &mut Ui) {
        self.poll_connect();
        self.drain_results();
        self.send_query_state_if_due();

        let frame = egui::Frame::central_panel(ui.style());
        let margin = frame.total_margin();
        // The panel fills the window whatever it holds, so it can't say how big
        // the window should be; a child inside it stops at its contents, and
        // that extent is the layout's own idea of the room it needs.
        let content = egui::CentralPanel::default()
            .frame(frame)
            .show(ui, |ui| {
                self.room = ui.available_size();
                ui.scope(|ui| self.draw_contents(ui)).response.rect
            })
            .inner;

        self.around_pad = content.size() - Vec2::splat(self.pad_size);
        let size = window_size_for(content, vec2(margin.right, margin.bottom));
        self.fit_window_to(ui.ctx(), size);
    }

    fn draw_contents(&mut self, ui: &mut Ui) {
        // Text sizes itself to what it says, like every other widget here:
        // a line that wrapped would be fitting itself to a window that is in
        // turn fitted to it.
        ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);

        ui.heading("Viscous");

        match &self.connection {
            Connection::Disconnected | Connection::Failed(_) => {}
            Connection::Connecting => {
                ui.label("Connecting...");
            }
            Connection::Connected {
                link,
                summary,
                details,
            } => {
                let camera = format!("{link} \u{2014} {summary}");
                let details = details.clone();
                self.draw_camera_row(ui, &camera, &details);
            }
        }

        space(ui, 1.0);
        if matches!(self.connection, Connection::Connected { .. }) {
            self.draw_controls(ui);
        } else {
            self.draw_connect_form(ui);
        }
    }

    /// What the camera is, whether it's awake, and the switch for that.
    fn draw_camera_row(&mut self, ui: &mut Ui, camera: &str, details: &str) {
        ui.horizontal(|ui| {
            ui.label(camera).on_hover_text(details);
            let powered = self.camera_state.map(|state| state.power_on);
            power_lamp(ui, powered);
            // A dark lamp is only a colour; a camera in standby ignores every
            // control on this window, which is worth saying in words — as the
            // camera's older Windows control panel did beside its own lamp.
            ui.label(power_words(self.camera_state));
            // Nothing is known about power until the first state reply lands,
            // and the useful thing to offer meanwhile is the one that wakes a
            // camera that turns out to be asleep.
            //
            // Never disabled, not even part-way through waking: this is the
            // one control that has to work when nothing else does, and a
            // camera that is slow to answer must not become a dead end.
            let on = powered.unwrap_or(false);
            if ui
                .button(if on { "Power off" } else { "Power on" })
                .clicked()
            {
                self.send_intent(Intent::SetPower(!on));
            }
        });
    }

    /// Asks for a window of exactly `size`, and shows it once there's a size
    /// worth showing it at.
    ///
    /// Only asks when that size changes: what comes back is never quite what
    /// was asked for — whole pixels, a platform minimum, the user's own drag —
    /// and repeating the request every frame would turn that into an argument.
    ///
    /// Once the controls have been fitted the window is left alone entirely,
    /// and made unshrinkable below that fit: from then on its size is the
    /// user's to choose, and the room they give it goes to the pad and the
    /// description fields.
    fn fit_window_to(&mut self, ctx: &egui::Context, size: Vec2) {
        if self.fitted_at.is_some() || self.requested_size == Some(size) {
            return;
        }
        let first_fit = self.requested_size.is_none();
        self.requested_size = Some(size);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        if first_fit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        if matches!(self.connection, Connection::Connected { .. }) {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(size));
            self.fitted_at = Some(ctx.cumulative_frame_nr());
        }
    }

    fn draw_connect_form(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("Camera:");
            ui.text_edit_singleline(&mut self.port_input);
        });
        ui.small("a serial port (/dev/ttyUSB0, COM3), or tcp://host:port for VISCA over IP");
        space(ui, 1.0);
        let connecting = matches!(self.connection, Connection::Connecting);
        if ui
            .add_enabled(!connecting, egui::Button::new("Connect"))
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_connect(&ctx);
        }
        if let Connection::Failed(error) = &self.connection {
            space(ui, 1.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }

    fn draw_controls(&mut self, ui: &mut Ui) {
        self.draw_status(ui);
        space(ui, 2.0);

        // A sleeping camera refuses all of this, so it is drawn dead rather
        // than left to look live and do nothing. Drawn, though, and in its
        // usual place: the pad's size is what the window is fitted around, and
        // controls that came and went would resize the window under the hand
        // reaching for them.
        let live = !self.asleep();
        // Read before anything is drawn, so the pad and the rockers show where
        // the controller is now rather than where it was a frame ago.
        let (held, pressed) = self.controller_input();
        let stick = self.stick;
        let mut pointed = Drives::STOPPED;
        ui.horizontal(|ui| {
            let drives_height = ui
                .vertical(|ui| {
                    self.pad_size = self.pad_size_in(ui);
                    pointed.pan_tilt = joystick::pan_tilt_pad(
                        ui,
                        self.pad_size,
                        live,
                        STANDBY_HINT,
                        vec2(stick.left_stick.0, stick.left_stick.1),
                    );

                    space(ui, 2.0);
                    (pointed.zoom, pointed.focus) =
                        drive_rockers(ui, self.pad_size, live, self.auto_focusing(), &stick);

                    space(ui, 1.0);
                    ui.add_enabled_ui(live, |ui| self.draw_camera_buttons(ui));
                })
                .response
                .rect
                .height();

            // Left to itself a separator fills whatever height it's offered,
            // which here is the whole window; hold it to what it separates.
            ui.scope(|ui| {
                ui.set_max_height(drives_height);
                ui.separator();
            });

            ui.vertical(|ui| self.draw_shots(ui, live));
        });

        // The pointer wins where both are asking at once: a hand on the mouse
        // is aiming at a particular shot, and a key that was never released
        // (a window that lost focus mid-drive, say) shouldn't override it.
        let typed = self.keyboard_drives(ui);
        self.apply_drives(pointed.or(typed).or(held));
        self.apply_presses(&pressed);
        if let Some(preset) = keyboard_preset(ui).filter(|_| live) {
            self.send_intent(Intent::RecallPreset(preset));
        }
        // The same keys the terminal front end uses, so fingers that learned
        // them there reach the controls drawn just above.
        if !typing(ui) {
            if live && ui.input(|input| input.key_pressed(Key::F)) {
                self.send_intent(Intent::SetAutoFocus(!self.auto_focusing()));
            }
            // Not gated on `live` — this is the way out of standby.
            if ui.input(|input| input.key_pressed(Key::P)) {
                let on = self.camera_state.is_none_or(|state| !state.power_on);
                self.send_intent(Intent::SetPower(on));
            }
        }

        space(ui, 1.0);
        draw_key_help(ui);

        if let Some(lens) = self.camera_state.and_then(|state| state.lens) {
            space(ui, 1.0);
            // Power and focus mode are already shown by the lamp and the
            // toggle, so what's left is only the numbers, kept quiet: they're
            // there to be checked, not read.
            ui.weak(state::format_position(&lens));
        }
    }

    /// The one line of feedback for whatever was last asked of the camera.
    fn draw_status(&mut self, ui: &mut Ui) {
        match &self.status {
            Some(status) if status.failed => {
                ui.colored_label(ui.visuals().error_fg_color, &status.text);
            }
            Some(status) => {
                ui.label(&status.text);
            }
            None => {
                ui.label("Ready");
            }
        }
    }

    /// How big to draw the pad: its natural size while the window is still
    /// being fitted around the layout, and afterwards whatever room the window
    /// has beyond what everything else in it needs.
    ///
    /// Square, so it takes the smaller of the two leftovers — which also
    /// leaves the width it doesn't need to the descriptions beside it.
    fn pad_size_in(&self, ui: &Ui) -> f32 {
        let least = joystick::least_pad_size(ui);
        let fitted = self
            .fitted_at
            .is_some_and(|frame| ui.ctx().cumulative_frame_nr() > frame);
        if !fitted {
            return least;
        }
        let spare = self.room - self.around_pad;
        spare.min_elem().max(least)
    }

    /// Sends whatever changed since the last frame, and nothing that didn't.
    ///
    /// A drive keeps running on the camera by itself, so what's already been
    /// asked for is what the camera is already doing: a command only goes out
    /// when a control starts asking for something else.
    fn apply_drives(&mut self, drives: Drives) {
        if drives.pan_tilt != self.pan_tilt {
            self.pan_tilt = drives.pan_tilt;
            self.send_intent(Intent::DrivePanTilt(drives.pan_tilt));
        }
        if drives.zoom != self.zoom {
            self.zoom = drives.zoom;
            self.send_intent(Intent::DriveZoom(drives.zoom));
        }
        if drives.focus != self.focus {
            self.focus = drives.focus;
            self.send_intent(Intent::DriveFocus(drives.focus));
        }
    }

    /// What the keys currently held down are asking for, having first sent a
    /// step for every arrow that was tapped rather than held.
    ///
    /// Nothing, while a description field has the keyboard: a "1" typed there
    /// is a digit, not a preset, and an arrow key is a cursor.
    fn keyboard_drives(&mut self, ui: &Ui) -> Drives {
        if typing(ui) {
            return Drives::STOPPED;
        }
        let (steps, held) = ui.input(|input| {
            (
                keyboard_steps(input),
                held_drives(|key| input.key_down(key), input.modifiers.shift),
            )
        });
        // A step is a movement like any other, so a camera that won't take a
        // drive won't take one of these either.
        if !self.asleep() {
            for step in steps {
                self.send_intent(Intent::Nudge(step));
            }
        }
        self.drivable(held)
    }

    /// What the game controller is asking for: the drives to hold for as long
    /// as its sticks are held, and whatever was just pressed on it.
    ///
    /// A stick keeps driving while a description is being typed, where a key
    /// doesn't: it is a control of its own, like the on-screen pad, and not
    /// something a text field can swallow.
    fn controller_input(&mut self) -> (Drives, Vec<Press>) {
        let read = self
            .controller
            .as_mut()
            .and_then(|controller| controller.poll());
        self.in_hand = read.is_some();
        self.stick = read.unwrap_or_default();
        let Some(pad) = read else {
            return (Drives::STOPPED, Vec::new());
        };
        let (drives, pressed) = self.gamepad.update(pad);
        (self.drivable(drives), pressed)
    }

    /// A set of drives with whatever the camera won't take dropped out of it,
    /// so that no control asks for what the window is drawn as refusing.
    fn drivable(&self, drives: Drives) -> Drives {
        // Nothing at all while the camera is asleep: the pad and the rockers
        // are drawn dead, and anything that still drove would be disagreeing
        // with what the window is showing.
        if self.asleep() {
            return Drives::STOPPED;
        }
        Drives {
            // The focus buttons are drawn disabled while the camera focuses
            // for itself; the other focus controls have to agree with them, or
            // one of the two would be lying about what the camera will do.
            focus: drives.focus.filter(|_| !self.auto_focusing()),
            ..drives
        }
    }

    /// Does whatever was just pressed on the controller — the commands that are
    /// given once, as against the drives that are held.
    fn apply_presses(&mut self, pressed: &[Press]) {
        let live = !self.asleep();
        for press in pressed {
            match press {
                // Not gated on `live`: this is the way out of standby, as the
                // P key and the switch beside the lamp are.
                Press::TogglePower => {
                    let on = self.camera_state.is_none_or(|state| !state.power_on);
                    self.send_intent(Intent::SetPower(on));
                }
                _ if !live => {}
                Press::Recall(preset) => self.send_intent(Intent::RecallPreset(*preset)),
                Press::Mark(preset) => self.send_intent(Intent::SavePreset(*preset)),
                Press::Home => self.send_intent(Intent::Home),
                Press::ToggleAutoFocus => {
                    self.send_intent(Intent::SetAutoFocus(!self.auto_focusing()));
                }
                Press::Nudge(direction) => {
                    self.send_intent(Intent::Nudge(Step::towards(*direction)));
                }
            }
        }
    }

    /// The preset slots: go to one, describe it, or store where the camera is
    /// pointing now.
    ///
    /// The description is the operator's, not the camera's — VISCA presets are
    /// bare numbers — so it lives in this program's config file and comes back
    /// on the next run.
    /// The two lists of the operator's own words: what each preset is a shot
    /// of, and what to write over the picture.
    ///
    /// Laid out as two blocks of plain rows rather than one grid: a grid cell
    /// offers a description field what's left of the row, and this column's
    /// width is in turn decided by what it asks for — so the two would talk
    /// each other down to nothing.
    ///
    /// Only the buttons answer to `live`: the descriptions and titles are the
    /// operator's own words, kept in this program's config file, and there is
    /// no reason a sleeping camera should stop anyone writing them down.
    fn draw_shots(&mut self, ui: &mut Ui, live: bool) {
        ui.horizontal(|ui| {
            ui.label("Presets");
            self.draw_marking_lock(ui);
        });
        self.draw_presets(ui, live);
        space(ui, 1.5);
        ui.label("Titles");
        self.draw_titles(ui, live);
        ui.small(if self.titles_supported {
            format!(
                "Up to {} uppercase characters, drawn over the picture",
                title::LENGTH
            )
        } else {
            NO_TITLES_HINT.to_string()
        });
    }

    /// The switch that arms the Mark column.
    ///
    /// Named for the mode rather than for the lock, and drawn pressed in when
    /// the Mark buttons work: what's on screen then matches what the column
    /// does, which a button reading "Locked" while nothing is locked would not.
    fn draw_marking_lock(&mut self, ui: &mut Ui) {
        let locked = self.marking_locked;
        let mut unlocked = !locked;
        if ui
            .toggle_value(&mut unlocked, "Marking")
            .on_hover_text(if locked {
                "Off, so a preset can't be overwritten by a mis-click. \
                 Turn it on to store new shots."
            } else {
                "On: the Mark buttons will overwrite presets. \
                 Turn it off once the shots are set."
            })
            .clicked()
        {
            self.marking_locked = !unlocked;
        }
    }

    fn draw_presets(&mut self, ui: &mut Ui, live: bool) {
        let text = ui.text_style_height(&egui::TextStyle::Body);
        // Taller than a line of text: this is a field to read back at a glance
        // rather than only to type into, and it sets the height the Mark button
        // beside it is drawn at.
        let field = egui::Margin::symmetric(4, ((text * (FIELD - 1.0)) / 2.0) as i8);
        let marking = live && !self.marking_locked;

        for number in 1..=PRESETS {
            ui.horizontal(|ui| {
                let description = self.preset_labels.get(&number).map(String::as_str);
                if ui
                    .add_enabled(
                        live,
                        egui::Button::new(number.to_string()).min_size(RECALL_BUTTON * text),
                    )
                    .on_hover_text(recall_tooltip(number, description))
                    .on_disabled_hover_text(STANDBY_HINT)
                    .clicked()
                {
                    self.send_intent(Intent::RecallPreset(number));
                }

                let description = self.preset_labels.entry(number).or_default();
                if ui
                    .add(egui::TextEdit::singleline(description).margin(field))
                    .lost_focus()
                {
                    self.save_config();
                }

                // "Mark", as the camera's older Windows control panel called
                // it: next to a description field, a button reading "Save"
                // looks like it saves the description, when what it stores is
                // where the camera is pointing.
                if ui
                    .add_enabled(
                        marking,
                        egui::Button::new("Mark").min_size(vec2(0.0, text * FIELD)),
                    )
                    .on_hover_text(format!(
                        "Store where the camera is pointing now as preset {number}"
                    ))
                    .on_disabled_hover_text(if live { LOCKED_HINT } else { STANDBY_HINT })
                    .clicked()
                {
                    self.send_intent(Intent::SavePreset(number));
                }
            });
        }
    }

    /// The titles that can be burned into the camera's video output: one
    /// shown at a time, since that's all the camera can hold.
    ///
    /// This is for whoever is watching downstream rather than for the
    /// operator — a name under a speaker, which hymn is being sung — so what
    /// goes out is what the camera can actually draw: twenty characters,
    /// uppercase, from its own character set.
    ///
    /// A camera that turns out to have no title command at all leaves the
    /// boxes drawn but dead, in place, for the same reason standby does: the
    /// rows the window is sized around shouldn't come and go.
    fn draw_titles(&mut self, ui: &mut Ui, live: bool) {
        let offered = live && self.titles_supported;
        // A box the shape of the caption itself: fixed pitch, and exactly as
        // many columns across as the camera has character cells. Measured in
        // characters rather than in text height, since an em is as tall as the
        // font rather than as wide as a letter — twenty of those would be half
        // as wide again as anything that fits in twenty cells.
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let column = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
        let margin = egui::Margin::symmetric(4, 2);
        let width = column * title::LENGTH as f32 + margin.sum().x;

        for number in 1..=TITLES {
            ui.horizontal(|ui| {
                let mut shown = self.shown_title == Some(number);
                if ui
                    .add_enabled(offered, egui::Checkbox::new(&mut shown, "Show"))
                    .on_hover_text("Burn this title into the video output")
                    .on_disabled_hover_text(if self.titles_supported {
                        STANDBY_HINT
                    } else {
                        NO_TITLES_HINT
                    })
                    .changed()
                {
                    self.show_title(shown.then_some(number));
                }

                let text = self.titles.entry(number).or_default();
                if ui
                    .add(
                        egui::TextEdit::singleline(text)
                            .font(egui::TextStyle::Monospace)
                            .char_limit(title::LENGTH)
                            .margin(margin)
                            .desired_width(width),
                    )
                    .lost_focus()
                {
                    self.save_config();
                    // Editing the one on screen changes what is on screen.
                    if self.shown_title == Some(number) {
                        self.show_title(Some(number));
                    }
                }
            });
        }
    }

    /// Shows the given title on the camera, or hides whatever is showing.
    fn show_title(&mut self, number: Option<u8>) {
        self.shown_title = number;
        match number.and_then(|number| self.titles.get(&number)) {
            Some(text) => {
                let text = Title::new(text);
                self.send_intent(Intent::SetTitle(text));
                self.send_intent(Intent::ShowTitle(true));
            }
            None => self.send_intent(Intent::ShowTitle(false)),
        }
    }

    /// Writes the preset descriptions and titles back to the config file.
    ///
    /// Slots left blank are dropped rather than stored empty: the file is
    /// meant to be readable and editable by hand, and six empty strings say
    /// nothing.
    fn save_config(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let written = |slots: &BTreeMap<u8, String>| {
            slots
                .iter()
                .filter(|(_, text)| !text.trim().is_empty())
                .map(|(number, text)| (*number, text.clone()))
                .collect()
        };
        let config = config::Config {
            camera: self.camera.clone(),
            presets: written(&self.preset_labels),
            titles: written(&self.titles),
        };
        if let Err(error) = config::save(&config, &path) {
            self.status = Some(Status::failed(error.to_string()));
        }
    }

    /// The commands that are given once rather than held: where to point, and
    /// who is focusing.
    fn draw_camera_buttons(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.button("Home").clicked() {
                self.send_intent(Intent::Home);
            }
            if ui
                .button("Reset")
                .on_hover_text("Recalibrate pan/tilt, then return home")
                .clicked()
            {
                self.send_intent(Intent::ResetPanTilt);
            }

            // A camera focusing for itself overrides the focus buttons the
            // moment it sees the scene again, so which it's doing belongs
            // where those buttons are.
            let mut auto = self.auto_focusing();
            if ui
                .toggle_value(&mut auto, "Auto focus")
                .on_hover_text("Let the camera focus itself, instead of the focus buttons")
                .changed()
            {
                self.send_intent(Intent::SetAutoFocus(auto));
            }
        });
    }

    /// Stops anything still being driven, waiting for the camera to confirm.
    ///
    /// A continuous drive outlives the process that started it, so a window
    /// closed mid-move would otherwise leave the camera moving on its own.
    fn stop_all_drives(&mut self) {
        let (Some(intents), Some(results)) = (&self.intents, &self.results) else {
            return;
        };
        if !self.pan_tilt.is_stop() {
            worker::stop_and_confirm(intents, results, Intent::DrivePanTilt(Velocity::STOP));
        }
        if self.zoom.is_some() {
            worker::stop_and_confirm(intents, results, Intent::DriveZoom(None));
        }
        if self.focus.is_some() {
            worker::stop_and_confirm(intents, results, Intent::DriveFocus(None));
        }
    }
}

/// The rockers that drive zoom and focus, reporting what each is being pushed
/// to ask for.
///
/// Pushed rather than clicked: a drive runs for exactly as long as the rocker
/// is held off centre, and how far it's pushed sets how fast — which the
/// camera has always accepted on these two controls, and which a pair of
/// buttons had no way to express. A slow zoom is the difference between a
/// shot that can go out live and one that lurches.
///
/// The ends carry the marks these controls have on the camera itself rather
/// than English words: W and T for the wide and telephoto ends of the zoom,
/// and the macro flower and infinity sign for the near and far ends of focus,
/// as printed on a lens barrel. The tooltips say it in words for anyone who
/// hasn't met them.
/// Drawn `width` wide — the pad's own width, since they sit directly under it
/// and drive the same camera: room that makes the pad a finer aim should make
/// these a finer zoom.
fn drive_rockers(
    ui: &mut Ui,
    width: f32,
    live: bool,
    auto_focusing: bool,
    stick: &Pad,
) -> (Option<ZoomDrive>, Option<FocusDrive>) {
    let zoom_marks = rocker::Marks {
        label: "Zoom",
        ends: ("W", "T"),
        tooltips: (
            "Wide \u{2014} push further to widen faster",
            "Telephoto \u{2014} push further to tighten faster",
        ),
    };
    let focus_marks = rocker::Marks {
        label: "Focus",
        ends: ("\u{273F}", "\u{221E}"),
        tooltips: (
            "Near \u{2014} push further to focus closer faster",
            "Far \u{2014} push further to focus further off faster",
        ),
    };
    let row = rocker::Row::fitting(ui, width, &[zoom_marks, focus_marks]);

    let zoom = rocker::rocker(
        ui,
        &zoom_marks,
        row,
        live,
        STANDBY_HINT,
        stick.right_stick.1,
    );
    // Drawn dead rather than left to do nothing, since a camera focusing for
    // itself ignores it; the toggle that revives it is just below. A sleeping
    // camera ignores it too, and says so first: it is the nearer problem, and
    // turning off auto focus wouldn't help.
    let focus = rocker::rocker(
        ui,
        &focus_marks,
        row,
        live && !auto_focusing,
        if live { AUTO_FOCUS_HINT } else { STANDBY_HINT },
        stick.focus_deflection(),
    );
    (
        ZoomDrive::from_deflection(zoom),
        FocusDrive::from_deflection(focus),
    )
}

/// What to say about a focus control the camera is currently overriding.
const AUTO_FOCUS_HINT: &str = "Turn off Auto focus to focus by hand";

/// What to say about any control a sleeping camera would refuse. Points at the
/// switch rather than at the key, since the switch is right there on screen.
const STANDBY_HINT: &str = "The camera is in standby \u{2014} switch it on first";

/// What to say about a Mark button the lock is holding off. Points at the
/// switch that arms it, which is at the head of the column it disarms.
const LOCKED_HINT: &str = "Turn on Marking, above, to store presets";

/// What to say once the camera has refused a title.
///
/// Names the camera's limit rather than the operator's mistake, because there
/// was none: `CAM_Title` is an EVI-D70-era command, and a camera made after
/// it has nothing to draw a caption with.
const NO_TITLES_HINT: &str = "This camera has no title command";

/// What the button for preset `number` promises, named by what the operator
/// called the shot rather than only by the number the camera knows it as.
fn recall_tooltip(number: u8, description: Option<&str>) -> String {
    match description.map(str::trim).filter(|text| !text.is_empty()) {
        Some(shot) => format!("Go to preset {number}: {shot}"),
        None => format!("Go to preset {number}"),
    }
}

/// Whether the keyboard currently belongs to a widget — a preset description
/// being typed into — rather than to the camera.
fn typing(ui: &Ui) -> bool {
    ui.memory(|memory| memory.focused().is_some())
}

/// The drives the held keys are asking for, given a way to ask whether a key
/// is down.
///
/// The bindings are the shared ones, so the same fingers work in either front
/// end — here and in the terminal:
/// shift and the arrows pan and tilt, `[`/`]` or `-`/`=` zoom, `,`/`.` focus,
/// and shift means full speed on the two controls it hasn't been spent on.
/// Page up/down zoom as well, which is what the camera's older Windows control
/// panel used.
fn held_drives(down: impl Fn(Key) -> bool, shift: bool) -> Drives {
    let deflection = if shift {
        FAST_KEY_DEFLECTION
    } else {
        KEY_DEFLECTION
    };
    // The framing pace whatever the modifiers say, unlike zoom and focus:
    // shift is what tells an arrow to drive rather than step, so it is already
    // spoken for and can't also mean "faster".
    let axis = |negative: Key, positive: Key| match (down(negative), down(positive)) {
        (true, false) => -KEY_DEFLECTION,
        (false, true) => KEY_DEFLECTION,
        _ => 0.0,
    };

    let zoom = opposed(
        down(Key::OpenBracket) || down(Key::Minus) || down(Key::PageDown),
        down(Key::CloseBracket) || down(Key::Equals) || down(Key::PageUp),
        deflection,
    );
    let focus = opposed(down(Key::Comma), down(Key::Period), deflection);

    Drives {
        // Only with shift, and always at the framing pace. Unmodified, an
        // arrow steps the camera instead — see `keyboard_steps` — so shift is
        // spent saying "drive" and has none left over to also say "faster".
        pan_tilt: if shift {
            pan_tilt::velocity_from_axes(
                axis(Key::ArrowLeft, Key::ArrowRight),
                axis(Key::ArrowDown, Key::ArrowUp),
            )
        } else {
            Velocity::STOP
        },
        zoom: ZoomDrive::from_deflection(zoom),
        focus: FocusDrive::from_deflection(focus),
    }
}

/// The steps the arrow keys were just pressed for.
///
/// Read from the frame's key events rather than from what is held down, so
/// that one press is one step however long the key stays there. Repeats are
/// skipped for the same reason: a finger resting on an arrow is not asking
/// the camera to walk away, and there is no other way to say "no, just the
/// one" once the terminal or the window has started repeating.
///
/// Nothing here needs to time how long a key was down, which is the point.
/// The pad and the stick drive; the arrows step; neither has to guess which
/// of the two the hand meant.
fn keyboard_steps(input: &egui::InputState) -> Vec<Step> {
    input
        .events
        .iter()
        .filter_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } if !modifiers.shift => step_for(*key),
            _ => None,
        })
        .collect()
}

/// The step an arrow key asks for, if it is one.
fn step_for(key: Key) -> Option<Step> {
    let direction = match key {
        Key::ArrowUp => PanTiltDirection::Up,
        Key::ArrowDown => PanTiltDirection::Down,
        Key::ArrowLeft => PanTiltDirection::Left,
        Key::ArrowRight => PanTiltDirection::Right,
        _ => return None,
    };
    Some(Step::towards(direction))
}

/// How far two opposed keys push the rocker they share, which is nowhere when
/// both are down — a control can only go one way at a time.
fn opposed(negative: bool, positive: bool, deflection: f32) -> f32 {
    match (negative, positive) {
        (true, false) => -deflection,
        (false, true) => deflection,
        _ => 0.0,
    }
}

/// The preset a number key was just pressed for, if any.
fn keyboard_preset(ui: &Ui) -> Option<u8> {
    if typing(ui) {
        return None;
    }
    const DIGITS: [Key; PRESETS as usize] = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
    ];
    ui.input(|input| {
        DIGITS
            .iter()
            .position(|key| input.key_pressed(*key))
            .map(|index| index as u8 + 1)
    })
}

/// The keys, spelled out: they aren't discoverable from the buttons the way
/// the pad and the drives are, and the camera's older Windows control panel
/// listed its own the same way.
fn draw_key_help(ui: &mut Ui) {
    ui.small("Drag the pad and push the rockers \u{2014} further is faster");
    ui.small("Arrows pan and tilt, [ ] or PgUp/PgDn zoom, , and . focus");
    ui.small("Shift on any of those drives at full speed");
    ui.small("Keys 1-6 go to presets, f switches auto focus, p switches power");
    // Always said, whether or not one is plugged in: a controller that turns
    // out to be silent is worth being able to tell from one nobody knew this
    // window would take — and the pad and rockers follow the sticks, so what
    // this promises can be checked at a glance.
    ui.small("A controller drives too: sticks pan/tilt and zoom, triggers focus");
    ui.small("A/B/X/Y and the bumpers are presets 1-6, Start and one marks it");
    ui.small("Stick clicks are Home and auto focus, Start+Back switches power");
}

/// Opens a gap of `gaps` of the style's own spacing between widgets.
///
/// The layout's rhythm comes from the same measure egui already puts between
/// everything it lays out, so a denser or roomier style moves the whole window
/// together instead of leaving these gaps behind at a fixed size.
fn space(ui: &mut Ui, gaps: f32) {
    let gap = ui.spacing().item_spacing.x;
    ui.add_space(gap * gaps);
}

/// What the power lamp beside this means, in words.
///
/// Awake-but-not-yet-answering is worth a word of its own: it is what the
/// first few seconds after switching on look like, and an operator who is told
/// only "Camera is on" while none of the controls work has been told the
/// wrong thing.
fn power_words(state: Option<CameraState>) -> &'static str {
    match state {
        Some(CameraState {
            power_on: true,
            lens: Some(_),
        }) => "Camera is on",
        Some(CameraState {
            power_on: true,
            lens: None,
        }) => "Camera is starting up",
        Some(CameraState {
            power_on: false, ..
        }) => "Camera is in standby",
        None => "Waiting for the camera",
    }
}

/// Draws a small lamp for the camera's power state: lit when it's awake, dark
/// when it's in standby, and neither until the camera has said which.
///
/// Sized from the text beside it rather than in pixels of its own, so it stays
/// a lamp next to a line of text at any scale.
fn power_lamp(ui: &mut Ui, powered: Option<bool>) {
    let diameter = ui.text_style_height(&egui::TextStyle::Body) / 2.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), egui::Sense::hover());
    let (color, tooltip) = match powered {
        Some(true) => (egui::Color32::from_rgb(60, 180, 75), "Camera is on"),
        Some(false) => (egui::Color32::from_rgb(120, 40, 40), "Camera is in standby"),
        None => (ui.visuals().weak_text_color(), "Waiting for the camera"),
    };
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, color);
    response.on_hover_text(tooltip);
}

/// The window size that exactly holds `content`, which was measured inside the
/// panel's margins: its far corner already counts the leading margin, leaving
/// only the trailing one to add.
fn window_size_for(content: egui::Rect, trailing_margin: Vec2) -> Vec2 {
    (content.max.to_vec2() + trailing_margin).ceil()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);

        // Keep polling for worker results / the debounced state query at
        // the same cadence the session loop's own event poll uses — and far
        // more often than that while there is a stick to read, since nothing
        // wakes the window when one moves.
        ui.ctx().request_repaint_after(if self.in_hand {
            CONTROLLER_INTERVAL
        } else {
            POLL_INTERVAL
        });
    }

    // The graphics context comes with the OpenGL backend's version of this
    // callback, for an app with GPU resources of its own to release. This one
    // paints through egui and has none.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_all_drives();
        // A description still being typed when the window closes never lost
        // focus, so it would otherwise go unsaved.
        self.save_config();
    }
}

/// The logo, compiled in rather than read from beside the executable: what
/// gets copied onto the machine that runs this is one file, and an icon that
/// lives in a second one is an icon that goes missing.
const LOGO: &[u8] = include_bytes!("../../viscous.png");

/// The logo decoded for the window to wear — its title bar, its taskbar
/// button, and whatever else the desktop puts a window's own icon on.
///
/// Windows takes the icon of a program that *isn't* running from the
/// executable's resources instead, which is `build.rs`'s job.
fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(LOGO).expect("the logo should be a readable PNG")
}

/// What to say when there turns out to be no window to be had: no display, no
/// OpenGL, an ssh session, a container.
///
/// Worth saying rather than leaving the toolkit's own error to speak for
/// itself, because the answer is a program that is sitting right there — the
/// same camera control, drawn in the terminal the message is being read in.
const NO_WINDOW_HINT: &str = "viscous needs a display. Where there isn't one, viscous-tui is the same \
     program in a terminal.";

fn main() -> std::process::ExitCode {
    // Born hidden, with no size of its own: the first frame measures the
    // layout, resizes the window to fit it and only then shows it, so nothing
    // here has to guess at a size the contents already know.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_visible(false)
            .with_icon(window_icon()),
        ..Default::default()
    };
    let run = eframe::run_native(
        "Viscous",
        options,
        Box::new(|_cc| {
            let mut app = App::with_config(config::default_path().ok());
            app.controller = pad::Attached::open().map(|attached| Box::new(attached) as Box<_>);
            Ok(Box::new(app))
        }),
    );
    match run {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            eprintln!("{NO_WINDOW_HINT}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, ViewportCommand};
    use egui_kittest::{
        Harness,
        kittest::{NodeT, Queryable},
    };
    use grafton_visca::{
        camera::PanTiltPosition, command::PanTiltDirection, types::FocusPosition,
        types::ZoomPosition,
    };
    use viscous::{focus::FocusDirection, zoom::ZoomDirection};

    /// An app already connected to a camera, plus the receiving end of the
    /// channel its controls send intents down — what a click actually
    /// produces, rather than what it draws.
    fn connected_app(camera_state: Option<CameraState>) -> (App, Receiver<Intent>) {
        let (intents, sent) = mpsc::channel();
        let app = App {
            connection: Connection::Connected {
                link: "COM3 at 9600 baud".to_string(),
                summary: "Sony".to_string(),
                details: "vendor=Sony (0x0020) model=0x040F".to_string(),
            },
            intents: Some(intents),
            camera_state,
            ..App::default()
        };
        (app, sent)
    }

    /// A worker with nothing on the other end of its channels — enough to
    /// stand in for a camera that answered.
    fn test_worker() -> Worker {
        let (intents, _) = mpsc::channel();
        let (_, results) = mpsc::channel();
        Worker {
            link: "COM3 at 9600 baud".to_string(),
            summary: "Sony".to_string(),
            details: "vendor=Sony (0x0020) model=0x040F".to_string(),
            titles: true,
            intents,
            results,
        }
    }

    fn camera_state(power_on: bool, auto_focus: bool) -> CameraState {
        CameraState {
            power_on,
            lens: Some(viscous::state::Lens {
                pan_tilt: PanTiltPosition::new(0, 0),
                zoom: ZoomPosition::try_from(0u16).unwrap(),
                focus: FocusPosition::new(0),
                auto_focus,
            }),
        }
    }

    /// A camera part-way through waking up: awake, but not yet answering for
    /// its lens.
    fn starting_up() -> CameraState {
        CameraState {
            power_on: true,
            lens: None,
        }
    }

    /// Drives the app the way a user does: real frames, real hit testing,
    /// widgets found by the label they're drawn with.
    fn ui_harness(app: App) -> Harness<'static, App> {
        Harness::new_ui_state(|ui, app| app.draw(ui), app)
    }

    /// Clicks the button with `label` and returns the intents that produced.
    fn click(app: App, sent: &Receiver<Intent>, label: &str) -> Vec<Intent> {
        let mut harness = ui_harness(app);
        harness.get_by_label(label).click();
        harness.run();
        sent.try_iter().collect()
    }

    /// Lays out one frame of the app on a screen of the given size, with no
    /// window and no GPU, and reports what it asked the platform for.
    fn frame_on_screen(app: &mut App, ctx: &egui::Context, screen: Vec2) -> Vec<ViewportCommand> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| app.draw(ui))
            .viewport_output
            .into_values()
            .flat_map(|viewport| viewport.commands)
            .collect()
    }

    fn requested_size(commands: &[ViewportCommand]) -> Option<Vec2> {
        commands.iter().find_map(|command| match command {
            ViewportCommand::InnerSize(size) => Some(*size),
            _ => None,
        })
    }

    fn fit_on_screen(screen: Vec2) -> Vec2 {
        let commands = frame_on_screen(&mut App::default(), &egui::Context::default(), screen);
        requested_size(&commands).expect("the first frame should size the window")
    }

    /// The same, for the controls rather than the connect form.
    fn connected_fit_on_screen(screen: Vec2) -> Vec2 {
        let (mut app, _sent) = connected_app(None);
        let commands = frame_on_screen(&mut app, &egui::Context::default(), screen);
        requested_size(&commands).expect("the first frame should size the window")
    }

    /// The description field belonging to preset `number`: the rows are drawn
    /// in order, so the nth text field is the nth preset's.
    fn description_field<'a>(harness: &'a Harness<'_, App>, number: u8) -> egui_kittest::Node<'a> {
        harness
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .nth(usize::from(number) - 1)
            .expect("every preset should have a description field")
    }

    fn test_config_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("viscous-gui-test-{name}.toml"));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// The drives asked for while exactly `held` is down.
    fn held(keys: &[Key]) -> Drives {
        held_drives(|key| keys.contains(&key), false)
    }

    /// The drives asked for while exactly `held` is down with shift.
    fn shifted(keys: &[Key]) -> Drives {
        held_drives(|key| keys.contains(&key), true)
    }

    #[test]
    fn shift_and_the_arrow_keys_drive_pan_and_tilt() {
        assert_eq!(
            shifted(&[Key::ArrowUp]).pan_tilt,
            pan_tilt::velocity_from_axes(0.0, KEY_DEFLECTION)
        );
        assert_eq!(
            shifted(&[Key::ArrowLeft, Key::ArrowDown]).pan_tilt,
            pan_tilt::velocity_from_axes(-KEY_DEFLECTION, -KEY_DEFLECTION)
        );
    }

    #[test]
    fn an_arrow_key_on_its_own_drives_nothing() {
        // It steps instead, which is `keyboard_steps`' business — and if it
        // drove as well, one press would both step and start a slew.
        assert!(held(&[Key::ArrowUp]).pan_tilt.is_stop());
        assert!(held(&[Key::ArrowLeft, Key::ArrowDown]).pan_tilt.is_stop());
    }

    #[test]
    fn shift_drives_zoom_and_focus_at_the_speed_the_camera_is_capable_of() {
        // Shift still means "faster" on the controls it hasn't been spent on.
        let fast = held_drives(|key| key == Key::CloseBracket, true);

        assert_eq!(fast.zoom, ZoomDrive::from_deflection(FAST_KEY_DEFLECTION));
        assert!(
            fast.zoom.unwrap().speed.value()
                > held(&[Key::CloseBracket]).zoom.unwrap().speed.value()
        );
    }

    #[test]
    fn opposed_keys_cancel_rather_than_pick_one() {
        let both = held(&[Key::ArrowLeft, Key::ArrowRight, Key::Comma, Key::Period]);

        assert_eq!(both, Drives::STOPPED);
    }

    /// Which way a set of held keys drives zoom, and which way focus.
    fn zoom_way(keys: &[Key]) -> Option<ZoomDirection> {
        held(keys).zoom.map(|drive| drive.direction)
    }

    fn focus_way(keys: &[Key]) -> Option<FocusDirection> {
        held(keys).focus.map(|drive| drive.direction)
    }

    #[test]
    fn the_zoom_and_focus_keys_drive_those() {
        assert_eq!(zoom_way(&[Key::CloseBracket]), Some(ZoomDirection::In));
        assert_eq!(zoom_way(&[Key::Minus]), Some(ZoomDirection::Out));
        assert_eq!(zoom_way(&[Key::PageUp]), Some(ZoomDirection::In));
        assert_eq!(zoom_way(&[Key::PageDown]), Some(ZoomDirection::Out));
        assert_eq!(focus_way(&[Key::Comma]), Some(FocusDirection::Near));
        assert_eq!(focus_way(&[Key::Period]), Some(FocusDirection::Far));
    }

    #[test]
    fn shift_drives_zoom_and_focus_faster_as_it_does_the_pan() {
        let framing = held(&[Key::CloseBracket]).zoom.expect("a held key drives");
        let fast = held_drives(|key| key == Key::CloseBracket, true)
            .zoom
            .expect("a held key drives");

        assert_eq!(fast.direction, framing.direction);
        assert_ne!(fast.speed, framing.speed);
    }

    #[test]
    fn a_drag_and_a_held_key_drive_different_controls_at_once() {
        let dragging = Drives {
            pan_tilt: pan_tilt::velocity_from_axes(1.0, 0.0),
            ..Drives::STOPPED
        };
        let zooming = held(&[Key::PageUp]);

        let both = dragging.or(zooming);

        assert_eq!(both.pan_tilt, dragging.pan_tilt);
        assert_eq!(
            both.zoom.map(|drive| drive.direction),
            Some(ZoomDirection::In)
        );
    }

    #[test]
    fn the_pointer_wins_the_control_it_shares_with_the_keyboard() {
        let dragging = Drives {
            pan_tilt: pan_tilt::velocity_from_axes(1.0, 0.0),
            ..Drives::STOPPED
        };

        assert_eq!(
            dragging.or(held(&[Key::ArrowLeft])).pan_tilt,
            dragging.pan_tilt
        );
    }

    #[test]
    fn a_held_shifted_arrow_key_drives_the_camera_and_releasing_it_stops() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.key_down_modifiers(egui::Modifiers::SHIFT, Key::ArrowRight);
        harness.run();
        let driving = commands(&sent);

        assert_eq!(
            driving.first(),
            Some(&Intent::DrivePanTilt(pan_tilt::velocity_from_axes(
                KEY_DEFLECTION,
                0.0
            ))),
            "shift and an arrow should drive"
        );
        assert!(
            !driving
                .iter()
                .any(|intent| matches!(intent, Intent::Nudge(_))),
            "and should not also step: one press is one gesture"
        );
        // The stop that follows in the same batch is the harness rather than
        // the app: kittest puts the modifiers back to none right after the
        // event it decorated, since it is built for chords like ctrl+C rather
        // than for a modifier somebody is leaning on. A window gets the real
        // modifier state with every frame.
        assert_eq!(
            driving.last(),
            Some(&Intent::DrivePanTilt(Velocity::STOP)),
            "letting go of shift should stop the camera, as letting go does"
        );
    }

    #[test]
    fn pressing_an_arrow_key_steps_the_camera_without_driving_it() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.key_down(Key::ArrowRight);
        harness.run();
        harness.key_up(Key::ArrowRight);
        harness.run();

        assert_eq!(
            commands(&sent),
            vec![Intent::Nudge(Step::towards(PanTiltDirection::Right))],
            "an arrow on its own should be one step and no drive at all"
        );
    }

    #[test]
    fn holding_an_arrow_key_down_steps_once_rather_than_walking_away() {
        // The property that replaced the timing window: a finger left resting
        // on an arrow has asked for one step, and asking again is a new press.
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.key_down(Key::ArrowRight);
        for _ in 0..5 {
            harness.run();
        }

        assert_eq!(
            commands(&sent),
            vec![Intent::Nudge(Step::towards(PanTiltDirection::Right))]
        );
    }

    #[test]
    fn pressing_an_arrow_key_at_a_sleeping_camera_asks_it_for_nothing() {
        let (mut app, sent) = connected_app(None);
        app.camera_state = Some(CameraState {
            power_on: false,
            lens: None,
        });
        let mut harness = ui_harness(app);

        harness.key_down(Key::ArrowRight);
        harness.run();
        harness.key_up(Key::ArrowRight);
        harness.run();

        assert!(
            commands(&sent).is_empty(),
            "a camera drawn as asleep shouldn't be stepped either"
        );
    }

    #[test]
    fn a_step_is_movement_and_so_says_nothing_in_the_status_line() {
        // Steps come several at a time by design; each one announcing itself
        // would bury whatever the line was carrying.
        let (mut app, _sent) = connected_app(None);

        app.send_intent(Intent::Nudge(Step::towards(PanTiltDirection::Right)));

        assert!(app.status.is_none());
    }

    #[test]
    fn driving_leaves_the_status_line_to_say_something_else() {
        let (app, _sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.key_down(Key::ArrowRight);
        harness.run();
        harness.key_up(Key::ArrowRight);
        harness.run();
        harness
            .state_mut()
            .apply_outcome(Outcome::Done(Intent::DrivePanTilt(Velocity::STOP), Ok(())));

        assert!(
            harness.state().status.is_none(),
            "a move is its own report: {:?}",
            harness.state().status.as_ref().map(|status| &status.text)
        );
    }

    #[test]
    fn a_drive_that_failed_is_reported_as_a_failure() {
        let (mut app, _sent) = connected_app(None);

        app.apply_outcome(Outcome::Done(
            Intent::DrivePanTilt(Velocity::STOP),
            Err(grafton_visca::Error::Timeout),
        ));

        let status = app.status.expect("a failed drive should be reported");
        assert!(status.failed);
        assert!(status.text.contains("error"));
    }

    #[test]
    fn a_one_off_command_says_it_is_under_way() {
        let (mut app, _sent) = connected_app(None);

        app.send_intent(Intent::RecallPreset(3));

        let status = app.status.expect("a preset recall should be reported");
        assert!(!status.failed);
        assert!(status.text.contains("preset 3"));
    }

    #[test]
    fn escape_hands_the_keyboard_back_to_the_camera() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        description_field(&harness, 1).click();
        harness.run();
        harness.key_press(Key::Escape);
        harness.run();
        let _ = sent.try_iter().count();
        harness.key_down(Key::ArrowRight);
        harness.run();

        assert_eq!(
            commands(&sent),
            vec![Intent::Nudge(Step::towards(PanTiltDirection::Right))],
            "a description field should let go of the keyboard on escape"
        );
    }

    #[test]
    fn a_number_key_goes_to_that_preset() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.key_press(Key::Num4);
        harness.run();

        assert_eq!(
            sent.try_iter().collect::<Vec<_>>(),
            vec![Intent::RecallPreset(4)]
        );
    }

    #[test]
    fn keys_meant_for_a_description_do_not_reach_the_camera() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        description_field(&harness, 1).click();
        harness.run();
        harness.key_press(Key::Num4);
        harness.key_down(Key::ArrowRight);
        harness.run();

        assert!(
            sent.try_iter().next().is_none(),
            "a field being typed into owns the keyboard"
        );
    }

    #[test]
    fn the_window_wears_the_logo() {
        let icon = window_icon();

        assert!(icon.width > 0 && icon.height == icon.width, "a square logo");
        assert_eq!(
            icon.rgba.len(),
            (icon.width * icon.height * 4) as usize,
            "four bytes a pixel, so the window has something to draw"
        );
        assert!(
            icon.rgba.chunks(4).any(|pixel| pixel[3] > 0),
            "a logo of nothing but transparency would be no logo at all"
        );
    }

    /// What the app actually asked the camera to do, leaving out the state
    /// query that connecting arms — it goes out on the first frame whatever
    /// else is happening, and says nothing about what was clicked.
    fn commands(sent: &Receiver<Intent>) -> Vec<Intent> {
        sent.try_iter()
            .filter(|intent| *intent != Intent::QueryState)
            .collect()
    }

    /// The Show checkbox for title `number`, found by position for the same
    /// reason the description fields are: the rows are drawn in order.
    fn show_checkbox<'a>(harness: &'a Harness<'_, App>, number: u8) -> egui_kittest::Node<'a> {
        harness
            .get_all_by_label("Show")
            .nth(usize::from(number) - 1)
            .expect("every title should have a Show checkbox")
    }

    /// The field for title `number`, found by position like the rest of the
    /// column: the preset descriptions are drawn first, so the titles are the
    /// text fields after them.
    fn title_field<'a>(harness: &'a Harness<'_, App>, number: u8) -> egui_kittest::Node<'a> {
        harness
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .nth(usize::from(PRESETS + number) - 1)
            .expect("every title should have a field")
    }

    #[test]
    fn a_title_field_takes_what_the_camera_can_draw_and_no_more() {
        let (app, _sent) = connected_app(None);
        let mut harness = ui_harness(app);

        title_field(&harness, 1).click();
        harness.run();
        title_field(&harness, 1).type_text("THE WHOLE CONGREGATION STANDING");
        harness.run();

        assert_eq!(
            harness.state().titles.get(&1).map(String::as_str),
            Some("THE WHOLE CONGREGATI"),
            "the field should stop where the camera does, not silently drop the rest later"
        );
    }

    #[test]
    fn a_title_field_is_as_wide_as_the_caption_it_holds() {
        let (app, _sent) = connected_app(None);
        let mut harness = ui_harness(app);
        harness.run();

        let full = "M".repeat(title::LENGTH);
        let style = harness.ctx.style_of(harness.ctx.theme());
        let font = egui::TextStyle::Monospace.resolve(&style);
        let caption = harness.ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(full, font, egui::Color32::PLACEHOLDER)
                .size()
                .x
        });
        let box_width = title_field(&harness, 1).rect().width();

        assert!(
            (box_width - caption).abs() < 12.0,
            "a {box_width}pt box for a {caption}pt caption is not the shape of what goes out"
        );
    }

    #[test]
    fn showing_a_title_sends_it_to_the_camera_and_turns_it_on() {
        let (mut app, sent) = connected_app(None);
        app.titles.insert(2, "Hymn 123".to_string());
        let mut harness = ui_harness(app);

        show_checkbox(&harness, 2).click();
        harness.run();

        assert_eq!(
            sent.try_iter().collect::<Vec<_>>(),
            vec![
                Intent::SetTitle(Title::new("Hymn 123")),
                Intent::ShowTitle(true)
            ]
        );
    }

    #[test]
    fn only_one_title_can_be_on_screen_because_the_camera_holds_one() {
        let (mut app, sent) = connected_app(None);
        app.titles.insert(1, "Podium".to_string());
        app.titles.insert(2, "Choir".to_string());
        let mut harness = ui_harness(app);

        show_checkbox(&harness, 1).click();
        harness.run();
        show_checkbox(&harness, 2).click();
        harness.run();

        assert_eq!(harness.state().shown_title, Some(2));
        assert_eq!(
            sent.try_iter().last(),
            Some(Intent::ShowTitle(true)),
            "switching titles should leave the new one showing"
        );
    }

    #[test]
    fn unchecking_the_shown_title_hides_it() {
        let (mut app, sent) = connected_app(None);
        app.titles.insert(1, "Podium".to_string());
        let mut harness = ui_harness(app);

        show_checkbox(&harness, 1).click();
        harness.run();
        let _ = sent.try_iter().count();
        show_checkbox(&harness, 1).click();
        harness.run();

        assert_eq!(harness.state().shown_title, None);
        assert_eq!(
            sent.try_iter().collect::<Vec<_>>(),
            vec![Intent::ShowTitle(false)]
        );
    }

    #[test]
    fn a_camera_that_refuses_a_title_stops_being_offered_one() {
        let (mut app, sent) = connected_app(Some(camera_state(true, false)));
        app.titles.insert(1, "Podium".to_string());
        let mut harness = ui_harness(app);

        show_checkbox(&harness, 1).click();
        harness.run();
        let _ = sent.try_iter().count();
        // Both halves are already on their way when the first is refused.
        for intent in [
            Intent::SetTitle(Title::new("Podium")),
            Intent::ShowTitle(true),
        ] {
            harness
                .state_mut()
                .apply_outcome(Outcome::Done(intent, Err(Error::SyntaxError)));
        }
        harness.run();

        let status = harness.state().status.as_ref().expect("a refusal is news");
        assert!(status.failed);
        assert_eq!(status.text, NO_TITLES_HINT);
        assert_eq!(
            harness.state().shown_title,
            None,
            "nothing is on screen, so no box should be ticked"
        );
        show_checkbox(&harness, 1).click();
        harness.run();
        assert!(
            sent.try_iter().next().is_none(),
            "a camera without the command shouldn't be sent it again"
        );
    }

    #[test]
    fn a_camera_that_says_at_connect_it_has_no_titles_is_never_offered_one() {
        let (mut app, _sent) = connected_app(Some(camera_state(true, false)));
        app.titles.insert(1, "Podium".to_string());

        // What connecting to an EVI-D80 comes back with: the camera was asked
        // and said it has no such command.
        let (intents, sent) = mpsc::channel();
        app.poll_connect_result(Ok(Worker {
            titles: false,
            intents,
            ..test_worker()
        }));
        let mut harness = ui_harness(app);

        assert!(
            harness.get_all_by_label(NO_TITLES_HINT).next().is_some(),
            "the window should say so from the first frame, not after a refusal"
        );
        show_checkbox(&harness, 1).click();
        harness.run();
        assert!(
            commands(&sent).is_empty(),
            "a camera that has already said no shouldn't be asked once for real"
        );
    }

    #[test]
    fn a_camera_that_has_titles_is_offered_them_from_the_start() {
        let (mut app, _sent) = connected_app(Some(camera_state(true, false)));
        app.titles.insert(1, "Podium".to_string());

        let (intents, sent) = mpsc::channel();
        app.poll_connect_result(Ok(Worker {
            intents,
            ..test_worker()
        }));
        let mut harness = ui_harness(app);

        show_checkbox(&harness, 1).click();
        harness.run();
        assert_eq!(
            commands(&sent),
            vec![
                Intent::SetTitle(Title::new("Podium")),
                Intent::ShowTitle(true)
            ]
        );
    }

    #[test]
    fn the_titles_a_camera_cannot_draw_say_so_where_the_length_would_be() {
        let (mut app, _sent) = connected_app(Some(camera_state(true, false)));
        app.titles_supported = false;
        let harness = ui_harness(app);

        assert!(
            harness.get_all_by_label(NO_TITLES_HINT).next().is_some(),
            "the reason should be on screen, not only in a hover"
        );
    }

    #[test]
    fn a_title_is_kept_between_runs_like_a_preset_description() {
        let path = test_config_path("titles");
        let (mut app, _sent) = connected_app(None);
        app.config_path = Some(path.clone());
        app.titles.insert(3, "Sister Jones".to_string());

        app.save_config();
        let reopened = App::with_config(Some(path.clone()));
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            reopened.titles.get(&3).map(String::as_str),
            Some("Sister Jones")
        );
    }

    #[test]
    fn a_preset_button_offers_the_shot_it_was_described_as() {
        assert_eq!(
            recall_tooltip(3, Some("Wide shot of stand")),
            "Go to preset 3: Wide shot of stand"
        );
        assert_eq!(recall_tooltip(3, None), "Go to preset 3");
        assert_eq!(recall_tooltip(3, Some("   ")), "Go to preset 3");
    }

    #[test]
    fn a_numbered_preset_button_goes_to_that_preset() {
        let (app, sent) = connected_app(None);

        assert_eq!(click(app, &sent, "3"), vec![Intent::RecallPreset(3)]);
    }

    /// An app whose Mark column has been armed, as the operator arms it: by
    /// clicking the switch at the head of the column.
    fn marking_app(camera_state: Option<CameraState>) -> (App, Receiver<Intent>) {
        let (mut app, sent) = connected_app(camera_state);
        app.marking_locked = false;
        (app, sent)
    }

    #[test]
    fn each_preset_row_stores_its_own_slot() {
        let (app, sent) = marking_app(None);
        let mut harness = ui_harness(app);

        harness
            .get_all_by_label("Mark")
            .nth(2)
            .expect("there should be a Mark button for every preset")
            .click();
        harness.run();

        assert_eq!(
            sent.try_iter().collect::<Vec<_>>(),
            vec![Intent::SavePreset(3)]
        );
    }

    #[test]
    fn a_preset_is_a_bigger_target_than_the_controls_around_it() {
        let (app, _sent) = marking_app(None);
        let harness = ui_harness(app);

        let recall = harness.get_by_label("1").rect();
        let ordinary = harness.get_by_label("Marking").rect().height();
        let mark = harness
            .get_all_by_label("Mark")
            .next()
            .expect("there should be a Mark button for every preset")
            .rect();
        let field = description_field(&harness, 1).rect();

        assert!(
            recall.height() > ordinary * 1.8 && recall.width() > ordinary * 1.4,
            "a preset is reached for in a hurry and needs the room: {recall:?} \
             against an ordinary {ordinary}"
        );
        assert!(
            (mark.height() - field.height()).abs() < 1.0,
            "Mark should sit level with the field it belongs to: {mark:?} and {field:?}"
        );
        assert!(
            field.height() < recall.height(),
            "the row should grow, but the button it is named by should grow more"
        );
    }

    #[test]
    fn marking_starts_locked_so_a_stray_click_costs_nothing() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness
            .get_all_by_label("Mark")
            .next()
            .expect("the Mark buttons are drawn whether or not they are armed")
            .click();
        harness.run();

        assert_eq!(
            commands(&sent),
            vec![],
            "a locked Mark button should not reach the camera"
        );
    }

    #[test]
    fn the_switch_at_the_head_of_the_column_arms_it() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.get_by_label("Marking").click();
        harness.run();
        harness
            .get_all_by_label("Mark")
            .next()
            .expect("there should be a Mark button for every preset")
            .click();
        harness.run();

        assert_eq!(commands(&sent), vec![Intent::SavePreset(1)]);
    }

    #[test]
    fn the_lock_holds_off_marking_alone() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.get_by_label("1").click();
        harness.run();
        description_field(&harness, 1).type_text("Lectern");
        harness.run();

        assert_eq!(
            commands(&sent),
            vec![Intent::RecallPreset(1)],
            "a locked column should still recall shots and take descriptions"
        );
    }

    #[test]
    fn a_typed_preset_description_is_there_again_next_run() {
        let path = test_config_path("preset-description");
        let (mut app, _sent) = connected_app(None);
        app.config_path = Some(path.clone());
        let mut harness = ui_harness(app);

        description_field(&harness, 2).click();
        harness.run();
        description_field(&harness, 2).type_text("Chorister");
        harness.run();
        // Clicking away is what commits the edit, the same as tabbing out of it.
        harness.get_by_label("Home").click();
        harness.run();

        let reopened = App::with_config(Some(path.clone()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            reopened.preset_labels.get(&2).map(String::as_str),
            Some("Chorister")
        );
    }

    #[test]
    fn the_camera_that_connected_is_there_again_next_run() {
        let path = test_config_path("camera");
        let (mut app, _sent) = connected_app(None);
        app.config_path = Some(path.clone());
        app.port_input = " tcp://camera.local:5678 ".to_string();

        app.poll_connect_result(Ok(test_worker()));
        let reopened = App::with_config(Some(path.clone()));
        let _ = std::fs::remove_file(&path);

        assert_eq!(reopened.port_input, "tcp://camera.local:5678");
    }

    #[test]
    fn a_camera_that_never_answered_is_not_remembered() {
        let path = test_config_path("failed-camera");
        let (mut app, _sent) = connected_app(None);
        app.config_path = Some(path.clone());
        app.port_input = "COM99".to_string();

        app.poll_connect_result(Err("no response".to_string()));
        let reopened = App::with_config(Some(path.clone()));
        let _ = std::fs::remove_file(&path);

        assert_eq!(reopened.port_input, FIRST_GUESS);
    }

    #[test]
    fn preset_slots_left_blank_are_not_written_out() {
        let path = test_config_path("blank-presets");
        let (mut app, _sent) = connected_app(None);
        app.config_path = Some(path.clone());
        app.preset_labels.insert(1, "Podium".to_string());
        app.preset_labels.insert(2, "   ".to_string());

        app.save_config();
        let written = std::fs::read_to_string(&path).expect("the config file should be written");
        let _ = std::fs::remove_file(&path);

        assert!(written.contains("Podium"));
        assert!(
            !written.contains("2 ="),
            "a blank description should be left out: {written}"
        );
    }

    #[test]
    fn a_config_file_that_will_not_load_is_reported_rather_than_overwritten() {
        let path = test_config_path("malformed");
        std::fs::write(&path, "not valid toml [[[").expect("write should succeed");

        let app = App::with_config(Some(path.clone()));
        let still_there = std::fs::read_to_string(&path).expect("the file should still be there");
        let _ = std::fs::remove_file(&path);

        assert!(app.status.is_some(), "the failure should be visible");
        assert!(app.preset_labels.is_empty());
        assert_eq!(still_there, "not valid toml [[[");
    }

    #[test]
    fn home_and_reset_are_sent_as_the_one_off_commands_they_are() {
        let (app, sent) = connected_app(None);
        assert_eq!(click(app, &sent, "Home"), vec![Intent::Home]);

        let (app, sent) = connected_app(None);
        assert_eq!(click(app, &sent, "Reset"), vec![Intent::ResetPanTilt]);
    }

    #[test]
    fn the_camera_row_says_in_words_what_the_lamp_says_in_colour() {
        assert_eq!(power_words(Some(camera_state(true, false))), "Camera is on");
        assert_eq!(
            power_words(Some(camera_state(false, false))),
            "Camera is in standby"
        );
        assert_eq!(power_words(None), "Waiting for the camera");
    }

    #[test]
    fn a_camera_that_is_awake_but_not_answering_yet_is_not_called_ready() {
        // "Camera is on" beside controls that all refuse would be a lie the
        // operator can see through but not explain.
        assert_eq!(power_words(Some(starting_up())), "Camera is starting up");
    }

    #[test]
    fn the_power_button_offers_whichever_state_the_camera_is_not_in() {
        let (app, sent) = connected_app(Some(camera_state(true, false)));
        assert_eq!(
            click(app, &sent, "Power off"),
            vec![Intent::SetPower(false)],
            "a camera that is on should be offered standby"
        );

        let (app, sent) = connected_app(Some(camera_state(false, false)));
        assert_eq!(
            click(app, &sent, "Power on"),
            vec![Intent::SetPower(true)],
            "a camera in standby should be offered waking up"
        );
    }

    #[test]
    fn the_power_button_offers_to_wake_a_camera_that_has_not_answered_yet() {
        let (app, sent) = connected_app(None);

        assert_eq!(click(app, &sent, "Power on"), vec![Intent::SetPower(true)]);
    }

    #[test]
    fn f_switches_focus_mode_as_it_does_in_the_tui() {
        let (app, sent) = connected_app(Some(camera_state(true, true)));
        let mut harness = ui_harness(app);

        harness.key_press(Key::F);
        harness.run();

        assert_eq!(
            sent.try_iter().collect::<Vec<_>>(),
            vec![Intent::SetAutoFocus(false)]
        );
    }

    /// The intents produced by pushing the focus rocker towards its near
    /// end, with the camera focusing the given way.
    fn holding_focus_near(auto_focus: bool) -> Vec<Intent> {
        let (app, sent) = connected_app(Some(camera_state(true, auto_focus)));
        let mut harness = ui_harness(app);
        harness.run();

        // Zoom's track is drawn first, so focus's is the second slider.
        let track = harness
            .get_all_by_role(egui::accesskit::Role::Slider)
            .nth(1)
            .expect("focus should have a rocker")
            .rect();
        harness.drag_at(track.left_center());
        harness.run();

        sent.try_iter().collect()
    }

    #[test]
    fn the_focus_rocker_stays_out_of_a_cameras_way_while_it_focuses_itself() {
        assert_eq!(
            holding_focus_near(false)
                .into_iter()
                .map(focus_direction)
                .collect::<Vec<_>>(),
            vec![Some(FocusDirection::Near)]
        );
        assert!(
            holding_focus_near(true).is_empty(),
            "a rocker drawn dead should drive nothing"
        );
    }

    /// The intents produced by pushing a rocker or dragging the pad, with the
    /// camera in the given state.
    fn holding_the_drives(state: CameraState) -> Vec<Intent> {
        let (app, sent) = connected_app(Some(state));
        let mut harness = ui_harness(app);
        harness.run();

        let zoom = harness
            .get_all_by_role(egui::accesskit::Role::Slider)
            .next()
            .expect("zoom should have a rocker")
            .rect();
        harness.drag_at(zoom.right_center());
        harness.run();

        sent.try_iter().collect()
    }

    #[test]
    fn a_sleeping_camera_is_not_driven() {
        // Every one of these would come back "not executable": better to draw
        // the controls dead than to spend a round trip being told so.
        assert!(
            holding_the_drives(camera_state(false, false)).is_empty(),
            "controls drawn dead should drive nothing"
        );
        assert!(
            !holding_the_drives(camera_state(true, false)).is_empty(),
            "and an awake camera should still be driveable"
        );
    }

    #[test]
    fn the_keys_stay_quiet_while_the_camera_sleeps_too() {
        // The pad and the rockers are drawn dead; keys that still drove would
        // be contradicting what the window is showing.
        let (app, _sent) = connected_app(Some(camera_state(false, false)));
        let ctx = egui::Context::default();
        let mut app = app;
        let _ = frame_on_screen(&mut app, &ctx, vec2(800.0, 600.0));

        assert_eq!(
            app.keyboard_drives(&Ui::new(
                ctx.clone(),
                egui::Id::new("keys"),
                egui::UiBuilder::new(),
            )),
            Drives::STOPPED
        );
    }

    #[test]
    fn the_camera_confirming_the_switch_is_what_flips_what_it_offers() {
        // Not the click: the camera's own confirmation. Without this the
        // button would go on offering "Power off" until an inquiry came round
        // to it, which is the shape of the bug this all started with.
        let (mut app, _sent) = connected_app(Some(camera_state(true, false)));

        app.apply_outcome(Outcome::Done(Intent::SetPower(false), Ok(())));

        assert_eq!(app.camera_state.map(|state| state.power_on), Some(false));
        assert!(
            app.asleep(),
            "a camera confirmed into standby should draw its controls dead"
        );
    }

    #[test]
    fn waking_the_camera_does_not_claim_the_lens_is_where_it_was() {
        let (mut app, _sent) = connected_app(Some(camera_state(false, false)));

        app.apply_outcome(Outcome::Done(Intent::SetPower(true), Ok(())));

        assert_eq!(
            power_words(app.camera_state),
            "Camera is starting up",
            "a camera that has just been woken is not yet a camera that is on"
        );
    }

    #[test]
    fn the_power_switch_still_works_while_everything_else_is_dead() {
        // The one control that has to survive standby: without it the window
        // would be a dead end with no way back.
        let (app, sent) = connected_app(Some(camera_state(false, false)));

        assert_eq!(click(app, &sent, "Power on"), vec![Intent::SetPower(true)]);
    }

    /// Which way an intent drives focus, for asserting on direction without
    /// pinning down the speed a particular push happened to ask for.
    fn focus_direction(intent: Intent) -> Option<FocusDirection> {
        match intent {
            Intent::DriveFocus(drive) => drive.map(|drive| drive.direction),
            other => panic!("expected a focus drive, got {other:?}"),
        }
    }

    #[test]
    fn the_focus_keys_stay_out_of_a_cameras_way_while_it_focuses_itself() {
        let (app, sent) = connected_app(Some(camera_state(true, true)));
        let mut harness = ui_harness(app);

        harness.key_down(Key::Period);
        harness.run();

        assert!(
            sent.try_iter().next().is_none(),
            "a camera focusing for itself ignores a manual focus drive"
        );
    }

    #[test]
    fn the_focus_keys_drive_once_the_camera_stops_focusing_itself() {
        let (app, sent) = connected_app(Some(camera_state(true, false)));
        let mut harness = ui_harness(app);

        harness.key_down(Key::Period);
        harness.run();

        assert_eq!(
            sent.try_iter().map(focus_direction).collect::<Vec<_>>(),
            vec![Some(FocusDirection::Far)]
        );
    }

    #[test]
    fn the_focus_mode_toggle_switches_the_camera_out_of_what_it_reports() {
        let (app, sent) = connected_app(Some(camera_state(true, false)));
        assert_eq!(
            click(app, &sent, "Auto focus"),
            vec![Intent::SetAutoFocus(true)],
            "a manually focusing camera should be offered auto focus"
        );

        let (app, sent) = connected_app(Some(camera_state(true, true)));
        assert_eq!(
            click(app, &sent, "Auto focus"),
            vec![Intent::SetAutoFocus(false)],
            "an auto focusing camera should be offered manual focus back"
        );
    }

    /// A game controller held in one position for as long as a test needs it.
    struct Held(Pad);

    impl Controller for Held {
        fn poll(&mut self) -> Option<Pad> {
            Some(self.0)
        }
    }

    /// What one frame with a controller held like `pad` asks of a camera in
    /// `state` — the whole path, from the reading through the mapping to the
    /// intents that go down the wire.
    fn controller_asks(state: CameraState, pad: Pad) -> Vec<Intent> {
        let (mut app, sent) = connected_app(Some(state));
        app.controller = Some(Box::new(Held(pad)));
        let mut harness = ui_harness(app);
        harness.run();

        sent.try_iter().collect()
    }

    /// The same, of a camera that is awake and focusing by hand.
    fn controller_asks_awake(pad: Pad) -> Vec<Intent> {
        controller_asks(camera_state(true, false), pad)
    }

    /// A controller with `set` done to its buttons and nothing else touched.
    fn pressing(set: fn(&mut viscous::gamepad::Buttons)) -> Pad {
        let mut pad = Pad::default();
        set(&mut pad.buttons);
        pad
    }

    #[test]
    fn a_stick_on_the_controller_drives_the_camera() {
        let intents = controller_asks_awake(Pad {
            left_stick: (1.0, 0.0),
            ..Pad::default()
        });

        let panning = intents.iter().any(|intent| {
            matches!(intent, Intent::DrivePanTilt(velocity)
                if velocity.direction == PanTiltDirection::Right)
        });
        assert!(panning, "the stick should pan: {intents:?}");
    }

    #[test]
    fn a_button_on_the_controller_goes_to_its_preset() {
        assert!(
            controller_asks_awake(pressing(|buttons| buttons.south = true))
                .contains(&Intent::RecallPreset(1)),
        );
    }

    #[test]
    fn start_and_a_button_stores_the_shot_instead_of_going_to_it() {
        let intents = controller_asks_awake(pressing(|buttons| {
            buttons.start = true;
            buttons.south = true;
        }));

        assert!(intents.contains(&Intent::SavePreset(1)));
        assert!(!intents.contains(&Intent::RecallPreset(1)));
    }

    #[test]
    fn the_stick_clicks_send_the_camera_home_and_switch_focus_mode() {
        assert!(
            controller_asks_awake(pressing(|buttons| buttons.left_stick = true))
                .contains(&Intent::Home)
        );
        assert!(
            controller_asks_awake(pressing(|buttons| buttons.right_stick = true))
                .contains(&Intent::SetAutoFocus(true))
        );
    }

    #[test]
    fn the_centre_buttons_together_switch_the_power() {
        let combo = pressing(|buttons| {
            buttons.start = true;
            buttons.back = true;
        });

        assert!(controller_asks_awake(combo).contains(&Intent::SetPower(false)));
        // The way back out of standby, where nothing else on the controller
        // reaches the camera.
        assert_eq!(
            controller_asks(camera_state(false, false), combo),
            vec![Intent::SetPower(true)]
        );
    }

    #[test]
    fn a_sleeping_camera_takes_nothing_else_from_the_controller() {
        let asleep = camera_state(false, false);
        let pad = Pad {
            left_stick: (1.0, 1.0),
            right_stick: (0.0, 1.0),
            right_trigger: 1.0,
            ..pressing(|buttons| buttons.south = true)
        };

        assert!(
            controller_asks(asleep, pad).is_empty(),
            "the controls are drawn dead; the stick has to agree with them"
        );
    }

    #[test]
    fn the_triggers_stay_out_of_a_cameras_way_while_it_focuses_itself() {
        let pad = Pad {
            right_trigger: 1.0,
            ..Pad::default()
        };

        assert!(
            controller_asks(camera_state(true, true), pad).is_empty(),
            "the focus rocker is drawn dead while the camera focuses itself"
        );
        assert!(
            !controller_asks_awake(pad).is_empty(),
            "and live again once it stops"
        );
    }

    /// Where the rockers are drawn, in the order they are drawn in: zoom, then
    /// focus. They report themselves as sliders, so this is what the eye sees
    /// and what a screen reader would read out.
    fn rocker_knobs(pad: Pad) -> Vec<f64> {
        let (mut app, _sent) = connected_app(Some(camera_state(true, false)));
        app.controller = Some(Box::new(Held(pad)));
        let mut harness = ui_harness(app);
        harness.run();

        harness
            .get_all_by_role(egui::accesskit::Role::Slider)
            .map(|slider| {
                slider
                    .accesskit_node()
                    .numeric_value()
                    .expect("a rocker should say where it is")
            })
            .collect()
    }

    #[test]
    fn the_rockers_show_what_the_controller_is_doing_to_them() {
        // Feedback for the hand on the controller, which is otherwise driving
        // a camera through a window that never moves.
        assert_eq!(rocker_knobs(Pad::default()), vec![0.0, 0.0]);

        let pushed = rocker_knobs(Pad {
            right_stick: (0.0, 0.5),
            left_trigger: 1.0,
            ..Pad::default()
        });
        assert!((pushed[0] - 0.5).abs() < 0.001, "zoom: {pushed:?}");
        assert!((pushed[1] + 1.0).abs() < 0.001, "focus: {pushed:?}");
    }

    #[test]
    fn window_size_for_adds_the_margin_left_on_the_far_side() {
        let content = Rect::from_min_size(egui::pos2(8.0, 8.0), vec2(100.0, 50.0));

        assert_eq!(window_size_for(content, vec2(8.0, 8.0)), vec2(116.0, 66.0));
    }

    #[test]
    fn the_window_is_asked_to_fit_the_contents_not_the_screen() {
        let small = fit_on_screen(vec2(800.0, 600.0));
        let large = fit_on_screen(vec2(1600.0, 1200.0));

        assert_eq!(small, large, "the screen should not decide the layout");
        assert!(
            small.x < 800.0 && small.y < 600.0,
            "the connect form should ask for less than the screen it was given: {small:?}"
        );
    }

    #[test]
    fn a_narrow_screen_does_not_squeeze_the_description_fields() {
        // The window is sized to fit its contents, so anything that sizes
        // itself to the window in turn — a text field taking "what's left of
        // the row" — can talk the two of them down to nothing, and the fit is
        // only asked for once.
        let cramped = connected_fit_on_screen(vec2(420.0, 300.0));
        let roomy = connected_fit_on_screen(vec2(1600.0, 1200.0));

        assert_eq!(cramped, roomy, "the screen should not decide the layout");
    }

    #[test]
    fn the_window_grows_to_fit_the_controls_once_connected() {
        let ctx = egui::Context::default();
        let screen = vec2(1600.0, 1200.0);
        let mut app = App::default();

        let form = requested_size(&frame_on_screen(&mut app, &ctx, screen))
            .expect("the first frame should size the window");
        app.connection = Connection::Connected {
            link: "COM3 at 9600 baud".to_string(),
            summary: "Sony".to_string(),
            details: "vendor=Sony (0x0020) model=0x040F".to_string(),
        };
        let controls = requested_size(&frame_on_screen(&mut app, &ctx, screen))
            .expect("swapping the form for the controls should resize the window");

        assert!(
            controls.x > form.x && controls.y > form.y,
            "the controls need more room than the form: {controls:?} vs {form:?}"
        );
        assert!(
            controls.x < 1600.0 && controls.y < 1200.0,
            "the controls should still ask for less than the screen: {controls:?}"
        );
    }

    #[test]
    fn once_fitted_the_window_is_the_users_and_the_pad_takes_the_room() {
        let ctx = egui::Context::default();
        let screen = vec2(1600.0, 1200.0);
        let (mut app, _sent) = connected_app(None);

        let fit = frame_on_screen(&mut app, &ctx, screen);
        let natural = app.pad_size;
        let after = frame_on_screen(&mut app, &ctx, screen);
        frame_on_screen(&mut app, &ctx, screen);

        assert!(
            requested_size(&fit).is_some()
                && fit
                    .iter()
                    .any(|command| matches!(command, ViewportCommand::MinInnerSize(_))),
            "the fit should size the window and stop it shrinking below that: {fit:?}"
        );
        assert_eq!(
            requested_size(&after),
            None,
            "the window is the user's to size from then on"
        );
        assert!(
            app.pad_size > natural,
            "the pad should take the room the window has spare ({natural} then {})",
            app.pad_size
        );
    }

    /// Settles the app on a screen of the given size and reports how big the
    /// pad ended up and how much width everything beside it took.
    ///
    /// Several frames, because the pad is measured from what was left over last
    /// time: the layout converges on a size rather than jumping to it.
    fn settled_on(app: &mut App, ctx: &egui::Context, screen: Vec2) -> (f32, f32) {
        for _ in 0..8 {
            frame_on_screen(app, ctx, screen);
        }
        (app.pad_size, app.around_pad.x)
    }

    #[test]
    fn a_wider_window_goes_to_the_pad_rather_than_to_the_columns_beside_it() {
        let ctx = egui::Context::default();
        let (mut app, _sent) = connected_app(None);

        let (narrow, beside) = settled_on(&mut app, &ctx, vec2(1000.0, 1400.0));
        let (wide, still_beside) = settled_on(&mut app, &ctx, vec2(1400.0, 1400.0));

        assert!(
            wide > narrow + 300.0,
            "the extra width should reach the pad ({narrow} then {wide})"
        );
        assert!(
            (still_beside - beside).abs() < 1.0,
            "the presets are a fixed width and should not take any of it \
             ({beside} then {still_beside})"
        );
    }

    #[test]
    fn the_window_is_left_alone_while_the_contents_stay_put() {
        let ctx = egui::Context::default();
        let screen = vec2(800.0, 600.0);
        let mut app = App::default();

        frame_on_screen(&mut app, &ctx, screen);
        let commands = frame_on_screen(&mut app, &ctx, screen);

        assert_eq!(requested_size(&commands), None);
    }

    #[test]
    fn the_window_is_shown_once_it_has_been_fitted() {
        let ctx = egui::Context::default();
        let screen = vec2(800.0, 600.0);
        let mut app = App::default();

        let first = frame_on_screen(&mut app, &ctx, screen);
        let second = frame_on_screen(&mut app, &ctx, screen);

        assert!(
            first.contains(&ViewportCommand::Visible(true)),
            "the hidden window should be shown after its first fit: {first:?}"
        );
        assert!(
            !second.contains(&ViewportCommand::Visible(true)),
            "showing it once is enough: {second:?}"
        );
    }
}
