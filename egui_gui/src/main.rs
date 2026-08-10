//! An eframe/egui GUI for viscous — a pure-Rust alternative to the
//! Flutter+flutter_rust_bridge prototype kept on the `flutter-gui` branch.
//! Being plain Rust, it needs no FFI/bridge/mirrored-type layer: it calls
//! `viscous`'s existing worker/connection/state types directly.

// Windows executables default to the console subsystem, which pops up a
// terminal alongside the GUI window; switch to the windows subsystem in
// release builds only, so `cargo run` in debug still shows println!/log
// output in a normal terminal.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod joystick;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use eframe::egui;
use egui::{Key, TextWrapMode, Ui, Vec2, vec2};
use viscous::{
    config,
    connection::{self, Target, format_camera, format_version},
    focus::FocusDirection,
    keymap::{FAST_KEY_DEFLECTION, KEY_DEFLECTION},
    pan_tilt::{self, Velocity},
    session::{POLL_INTERVAL, QUIESCENCE_INTERVAL},
    state::{self, CameraState},
    title::{self, Title},
    worker::{self, Intent, Outcome},
    zoom::ZoomDirection,
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

/// How many titles to keep on hand. The camera holds one at a time; these are
/// the ones an operator switches between during a session.
const TITLES: u8 = 3;

/// What the controls are asking the camera to be doing right now — one
/// snapshot per frame, whichever control it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drives {
    pan_tilt: Velocity,
    zoom: Option<ZoomDirection>,
    focus: Option<FocusDirection>,
}

impl Drives {
    /// Nothing being asked for: every control at rest.
    const STOPPED: Self = Self {
        pan_tilt: Velocity::STOP,
        zoom: None,
        focus: None,
    };

    /// This set of drives, filled in from `other` wherever it asks for
    /// nothing — so two controls can be live at once (a drag while a zoom key
    /// is held) without either cancelling the other.
    fn or(self, other: Self) -> Self {
        Self {
            pan_tilt: if self.pan_tilt.is_stop() {
                other.pan_tilt
            } else {
                self.pan_tilt
            },
            zoom: self.zoom.or(other.zoom),
            focus: self.focus.or(other.focus),
        }
    }
}

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
    thread::spawn(move || worker::run(&camera, &worker_rx, &result_tx));

    Ok(Worker {
        link: connected.link,
        summary: format_camera(&connected.version),
        details: format_version(&connected.version),
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
    /// The titles kept on hand to burn into the video output, and which of
    /// them the camera was last told to show.
    titles: BTreeMap<u8, String>,
    shown_title: Option<u8>,
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
    pending_state_query: bool,
    last_command_at: Instant,
    /// What each continuous control was last told to do. A drive keeps running
    /// on the camera by itself, so these say what it's already doing — and a
    /// command only goes out when a control is asked for something different.
    pan_tilt: Velocity,
    zoom: Option<ZoomDirection>,
    focus: Option<FocusDirection>,
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
            titles: BTreeMap::new(),
            shown_title: None,
            camera: None,
            config_path: None,
            requested_size: None,
            fitted_at: None,
            room: Vec2::ZERO,
            around_pad: Vec2::ZERO,
            pad_size: 0.0,
            pending_state_query: false,
            last_command_at: Instant::now(),
            pan_tilt: Velocity::STOP,
            zoom: None,
            focus: None,
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
        if !worker::is_drive(intent) {
            self.status = Some(Status::ok(worker::describe_busy(intent)));
        }
        self.pending_state_query = true;
        self.last_command_at = Instant::now();
    }

    /// Whether the camera is focusing for itself, in which case the focus
    /// controls have nothing to drive: the camera overrides them the moment it
    /// sees the scene again. Unknown counts as not, so the controls stay live
    /// until the camera has actually said otherwise.
    fn auto_focusing(&self) -> bool {
        self.camera_state.is_some_and(|state| state.auto_focus)
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
        if self.driving()
            || !self.pending_state_query
            || self.last_command_at.elapsed() < QUIESCENCE_INTERVAL
        {
            return;
        }
        if let Some(intents) = &self.intents {
            let _ = intents.send(Intent::QueryState);
        }
        self.pending_state_query = false;
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
                self.intents = Some(worker.intents);
                self.results = Some(worker.results);
                // Query once up front so the info panel doesn't sit empty
                // until the first command; `elapsed() >= QUIESCENCE_INTERVAL`
                // is already true here.
                self.pending_state_query = true;
                self.last_command_at = Instant::now() - QUIESCENCE_INTERVAL;
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
                return;
            }
            // A drive that worked was already visible before its completion
            // arrived; one that failed is the only kind still worth saying.
            Outcome::Done(intent, Ok(())) if worker::is_drive(*intent) => return,
            Outcome::Done(_, result) => result.is_err(),
            Outcome::State(_) => true,
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
            ui.label(power_words(powered));
            // Nothing is known about power until the first state reply lands,
            // and the useful thing to offer meanwhile is the one that wakes a
            // camera that turns out to be asleep.
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

        let mut pointed = Drives::STOPPED;
        ui.horizontal(|ui| {
            let drives_height = ui
                .vertical(|ui| {
                    self.pad_size = self.pad_size_in(ui);
                    pointed.pan_tilt = joystick::pan_tilt_pad(ui, self.pad_size);

                    space(ui, 2.0);
                    (pointed.zoom, pointed.focus) = drive_buttons(ui, self.auto_focusing());

                    space(ui, 1.0);
                    self.draw_camera_buttons(ui);
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

            ui.vertical(|ui| self.draw_shots(ui));
        });

        // The pointer wins where both are asking at once: a hand on the mouse
        // is aiming at a particular shot, and a key that was never released
        // (a window that lost focus mid-drive, say) shouldn't override it.
        self.apply_drives(pointed.or(self.keyboard_drives(ui)));
        if let Some(preset) = keyboard_preset(ui) {
            self.send_intent(Intent::RecallPreset(preset));
        }
        // The same key the TUI uses, so the fingers that learned it there
        // reach the toggle drawn just above.
        if !typing(ui) && ui.input(|input| input.key_pressed(Key::F)) {
            self.send_intent(Intent::SetAutoFocus(!self.auto_focusing()));
        }

        space(ui, 1.0);
        draw_key_help(ui);

        if let Some(state) = &self.camera_state {
            space(ui, 1.0);
            // Power and focus mode are already shown by the lamp and the
            // toggle, so what's left is only the numbers, kept quiet: they're
            // there to be checked, not read.
            ui.weak(state::format_position(state));
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

    /// What the keys currently held down are asking for.
    ///
    /// Nothing, while a description field has the keyboard: a "1" typed there
    /// is a digit, not a preset, and an arrow key is a cursor.
    fn keyboard_drives(&self, ui: &Ui) -> Drives {
        if typing(ui) {
            return Drives::STOPPED;
        }
        let mut drives =
            ui.input(|input| held_drives(|key| input.key_down(key), input.modifiers.shift));
        // The focus buttons are drawn disabled while the camera focuses for
        // itself; the focus keys have to agree with them, or one of the two
        // would be lying about what the camera will do.
        if self.auto_focusing() {
            drives.focus = None;
        }
        drives
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
    fn draw_shots(&mut self, ui: &mut Ui) {
        ui.label("Presets");
        self.draw_presets(ui);
        space(ui, 1.5);
        ui.label("Titles");
        self.draw_titles(ui);
        ui.small(format!(
            "Up to {} uppercase characters, drawn over the picture",
            title::LENGTH
        ));
    }

    fn draw_presets(&mut self, ui: &mut Ui) {
        for number in 1..=PRESETS {
            ui.horizontal(|ui| {
                let description = self.preset_labels.get(&number).map(String::as_str);
                if ui
                    .button(number.to_string())
                    .on_hover_text(recall_tooltip(number, description))
                    .clicked()
                {
                    self.send_intent(Intent::RecallPreset(number));
                }

                let description = self.preset_labels.entry(number).or_default();
                if ui.text_edit_singleline(description).lost_focus() {
                    self.save_config();
                }

                // "Mark", as the camera's older Windows control panel called
                // it: next to a description field, a button reading "Save"
                // looks like it saves the description, when what it stores is
                // where the camera is pointing.
                if ui
                    .button("Mark")
                    .on_hover_text(format!(
                        "Store where the camera is pointing now as preset {number}"
                    ))
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
    fn draw_titles(&mut self, ui: &mut Ui) {
        for number in 1..=TITLES {
            ui.horizontal(|ui| {
                let mut shown = self.shown_title == Some(number);
                if ui
                    .checkbox(&mut shown, "Show")
                    .on_hover_text("Burn this title into the video output")
                    .changed()
                {
                    self.show_title(shown.then_some(number));
                }

                let text = self.titles.entry(number).or_default();
                if ui.text_edit_singleline(text).lost_focus() {
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

/// Draws the zoom and focus buttons, reporting which direction each is being
/// held in.
///
/// Held rather than clicked: a continuous drive runs for exactly as long as
/// the button is down, which is the same interaction as holding the TUI's
/// zoom and focus keys. They read "Out"/"In" and "Near"/"Far" rather than
/// "−"/"+", because neither control has a more or a less of it — zoom has a
/// wide end and a tight one, and focus has a near and a far.
fn drive_buttons(
    ui: &mut egui::Ui,
    auto_focusing: bool,
) -> (Option<ZoomDirection>, Option<FocusDirection>) {
    let mut zoom = None;
    let mut focus = None;
    // A grid rather than two rows of its own, so the buttons line up under
    // each other however wide the words in front of them turn out to be.
    egui::Grid::new("drives").show(ui, |ui| {
        ui.label("Zoom");
        if held(ui, true, "Out", "Hold to widen the shot") {
            zoom = Some(ZoomDirection::Out);
        }
        if held(ui, true, "In", "Hold to tighten the shot") {
            zoom = Some(ZoomDirection::In);
        }
        ui.end_row();

        // Drawn dead rather than left to do nothing, since a camera focusing
        // for itself ignores them; the toggle that revives them is alongside.
        let manual = !auto_focusing;
        ui.label("Focus");
        if held(ui, manual, "Near", "Hold to focus closer") {
            focus = Some(FocusDirection::Near);
        }
        if held(ui, manual, "Far", "Hold to focus further off") {
            focus = Some(FocusDirection::Far);
        }
        ui.end_row();
    });
    (zoom, focus)
}

/// What the button for preset `number` promises, named by what the operator
/// called the shot rather than only by the number the camera knows it as.
fn recall_tooltip(number: u8, description: Option<&str>) -> String {
    match description.map(str::trim).filter(|text| !text.is_empty()) {
        Some(shot) => format!("Go to preset {number}: {shot}"),
        None => format!("Go to preset {number}"),
    }
}

/// A button that reports whether it is being held down rather than whether it
/// was clicked — one continuous drive runs for exactly as long as it is.
fn held(ui: &mut Ui, enabled: bool, label: &str, tooltip: &str) -> bool {
    ui.add_enabled(enabled, egui::Button::new(label))
        .on_hover_text(tooltip)
        .on_disabled_hover_text("Turn off Auto focus to focus by hand")
        .is_pointer_button_down_on()
}

/// Whether the keyboard currently belongs to a widget — a preset description
/// being typed into — rather than to the camera.
fn typing(ui: &Ui) -> bool {
    ui.memory(|memory| memory.focused().is_some())
}

/// The drives the held keys are asking for, given a way to ask whether a key
/// is down.
///
/// The bindings are the TUI's, so the same fingers work in either front end:
/// arrows pan and tilt, `[`/`]` or `-`/`=` zoom, `,`/`.` focus, and shift
/// means full speed rather than the slower speed a shot is framed at. Page
/// up/down zoom as well, which is what the camera's older Windows control
/// panel used.
fn held_drives(down: impl Fn(Key) -> bool, shift: bool) -> Drives {
    let deflection = if shift {
        FAST_KEY_DEFLECTION
    } else {
        KEY_DEFLECTION
    };
    let axis = |negative: Key, positive: Key| match (down(negative), down(positive)) {
        (true, false) => -deflection,
        (false, true) => deflection,
        _ => 0.0,
    };

    Drives {
        pan_tilt: pan_tilt::velocity_from_axes(
            axis(Key::ArrowLeft, Key::ArrowRight),
            axis(Key::ArrowDown, Key::ArrowUp),
        ),
        zoom: direction(
            down(Key::OpenBracket) || down(Key::Minus) || down(Key::PageDown),
            down(Key::CloseBracket) || down(Key::Equals) || down(Key::PageUp),
            ZoomDirection::Out,
            ZoomDirection::In,
        ),
        focus: direction(
            down(Key::Comma),
            down(Key::Period),
            FocusDirection::Near,
            FocusDirection::Far,
        ),
    }
}

/// Which of two opposed keys is asking for its direction, or neither when
/// both are — a control can only go one way at a time.
fn direction<T>(negative: bool, positive: bool, out: T, in_: T) -> Option<T> {
    match (negative, positive) {
        (true, false) => Some(out),
        (false, true) => Some(in_),
        _ => None,
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
    ui.small("Drag the pad to pan and tilt \u{2014} further out is faster");
    ui.small("Arrows pan and tilt, with shift for full speed");
    ui.small("Zoom with [ ] or PgUp/PgDn, focus with , and .");
    ui.small("Keys 1-6 go to presets, f switches auto focus");
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
fn power_words(powered: Option<bool>) -> &'static str {
    match powered {
        Some(true) => "Camera is on",
        Some(false) => "Camera is in standby",
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
        // the same cadence the session loop's own event poll uses.
        ui.ctx().request_repaint_after(POLL_INTERVAL);
    }

    fn on_exit(&mut self) {
        self.stop_all_drives();
        // A description still being typed when the window closes never lost
        // focus, so it would otherwise go unsaved.
        self.save_config();
    }
}

fn main() -> eframe::Result {
    // Born hidden, with no size of its own: the first frame measures the
    // layout, resizes the window to fit it and only then shows it, so nothing
    // here has to guess at a size the contents already know.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_visible(false),
        ..Default::default()
    };
    eframe::run_native(
        "Viscous",
        options,
        Box::new(|_cc| Ok(Box::new(App::with_config(config::default_path().ok())))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, ViewportCommand};
    use egui_kittest::{Harness, kittest::Queryable};
    use grafton_visca::{camera::PanTiltPosition, types::FocusPosition, types::ZoomPosition};

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
            intents,
            results,
        }
    }

    fn camera_state(power_on: bool, auto_focus: bool) -> CameraState {
        CameraState {
            power_on,
            pan_tilt: PanTiltPosition::new(0, 0),
            zoom: ZoomPosition::try_from(0u16).unwrap(),
            focus: FocusPosition::new(0),
            auto_focus,
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

    #[test]
    fn the_arrow_keys_drive_pan_and_tilt() {
        assert_eq!(
            held(&[Key::ArrowUp]).pan_tilt,
            pan_tilt::velocity_from_axes(0.0, KEY_DEFLECTION)
        );
        assert_eq!(
            held(&[Key::ArrowLeft, Key::ArrowDown]).pan_tilt,
            pan_tilt::velocity_from_axes(-KEY_DEFLECTION, -KEY_DEFLECTION)
        );
    }

    #[test]
    fn shift_drives_at_the_speed_the_camera_is_capable_of() {
        let fast = held_drives(|key| key == Key::ArrowRight, true);

        assert_eq!(
            fast.pan_tilt,
            pan_tilt::velocity_from_axes(FAST_KEY_DEFLECTION, 0.0)
        );
        assert!(fast.pan_tilt.pan_speed > held(&[Key::ArrowRight]).pan_tilt.pan_speed);
    }

    #[test]
    fn opposed_keys_cancel_rather_than_pick_one() {
        let both = held(&[Key::ArrowLeft, Key::ArrowRight, Key::Comma, Key::Period]);

        assert_eq!(both, Drives::STOPPED);
    }

    #[test]
    fn the_zoom_and_focus_keys_drive_those() {
        assert_eq!(held(&[Key::CloseBracket]).zoom, Some(ZoomDirection::In));
        assert_eq!(held(&[Key::Minus]).zoom, Some(ZoomDirection::Out));
        assert_eq!(held(&[Key::PageUp]).zoom, Some(ZoomDirection::In));
        assert_eq!(held(&[Key::PageDown]).zoom, Some(ZoomDirection::Out));
        assert_eq!(held(&[Key::Comma]).focus, Some(FocusDirection::Near));
        assert_eq!(held(&[Key::Period]).focus, Some(FocusDirection::Far));
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
        assert_eq!(both.zoom, Some(ZoomDirection::In));
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
    fn a_held_arrow_key_drives_the_camera_and_releasing_it_stops() {
        let (app, sent) = connected_app(None);
        let mut harness = ui_harness(app);

        harness.key_down(Key::ArrowRight);
        harness.run();
        let driving = sent.try_iter().collect::<Vec<_>>();

        harness.key_up(Key::ArrowRight);
        harness.run();
        let released = sent.try_iter().collect::<Vec<_>>();

        assert_eq!(
            driving,
            vec![Intent::DrivePanTilt(pan_tilt::velocity_from_axes(
                KEY_DEFLECTION,
                0.0
            ))]
        );
        assert_eq!(released, vec![Intent::DrivePanTilt(Velocity::STOP)]);
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
            sent.try_iter().collect::<Vec<_>>(),
            vec![Intent::DrivePanTilt(pan_tilt::velocity_from_axes(
                KEY_DEFLECTION,
                0.0
            ))],
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

    /// The Show checkbox for title `number`, found by position for the same
    /// reason the description fields are: the rows are drawn in order.
    fn show_checkbox<'a>(harness: &'a Harness<'_, App>, number: u8) -> egui_kittest::Node<'a> {
        harness
            .get_all_by_label("Show")
            .nth(usize::from(number) - 1)
            .expect("every title should have a Show checkbox")
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

    #[test]
    fn each_preset_row_stores_its_own_slot() {
        let (app, sent) = connected_app(None);
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
        assert_eq!(power_words(Some(true)), "Camera is on");
        assert_eq!(power_words(Some(false)), "Camera is in standby");
        assert_eq!(power_words(None), "Waiting for the camera");
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

    /// The intents produced by holding the "Near" button down, with the
    /// camera focusing the given way.
    fn holding_focus_near(auto_focus: bool) -> Vec<Intent> {
        let (app, sent) = connected_app(Some(camera_state(true, auto_focus)));
        let mut harness = ui_harness(app);
        harness.run();

        let position = harness.get_by_label("Near").rect().center();
        harness.drag_at(position);
        harness.run();

        sent.try_iter().collect()
    }

    #[test]
    fn the_focus_buttons_stay_out_of_a_cameras_way_while_it_focuses_itself() {
        assert_eq!(
            holding_focus_near(false),
            vec![Intent::DriveFocus(Some(FocusDirection::Near))]
        );
        assert!(
            holding_focus_near(true).is_empty(),
            "a button drawn dead should drive nothing"
        );
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
            sent.try_iter().collect::<Vec<_>>(),
            vec![Intent::DriveFocus(Some(FocusDirection::Far))]
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
