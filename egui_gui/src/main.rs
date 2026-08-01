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
use egui::{TextWrapMode, Ui, Vec2, vec2};
use viscous::{
    config,
    connection::{self, Target, format_version},
    focus::FocusDirection,
    pan_tilt::Velocity,
    session::{POLL_INTERVAL, QUIESCENCE_INTERVAL},
    state::{self, CameraState},
    worker::{self, Intent, Outcome},
    zoom::ZoomDirection,
};

/// How many preset slots to offer — the same six the TUI's number keys reach.
const PRESETS: u8 = 6;

/// Where the camera connection attempt currently stands.
enum Connection {
    Disconnected,
    Connecting,
    Connected { link: String, summary: String },
    Failed(String),
}

/// What a successful [`connect`] produces: how to describe the connection,
/// plus the channels the rest of the app uses to talk to the worker thread.
struct Worker {
    link: String,
    summary: String,
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
        summary: format_version(&connected.version),
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
    status: Option<String>,
    /// What each preset is of, in the operator's own words, keyed by the same
    /// 1-based number as the buttons.
    preset_labels: BTreeMap<u8, String>,
    /// Where those descriptions are kept between runs, if this platform has
    /// somewhere to keep them.
    config_path: Option<PathBuf>,
    /// The window size most recently asked for, so the request only goes out
    /// when the size the contents need actually changes.
    requested_size: Option<Vec2>,
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
            port_input: "/dev/ttyUSB0".to_string(),
            connection: Connection::Disconnected,
            connect_rx: None,
            intents: None,
            results: None,
            camera_state: None,
            status: None,
            preset_labels: BTreeMap::new(),
            config_path: None,
            requested_size: None,
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
            Some(Ok(config)) => app.preset_labels = config.presets,
            Some(Err(error)) => app.status = Some(error.to_string()),
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
        self.status = Some(worker::describe_busy(intent));
        self.pending_state_query = true;
        self.last_command_at = Instant::now();
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
        self.connect_rx = None;
        match result {
            Ok(worker) => {
                self.connection = Connection::Connected {
                    link: worker.link,
                    summary: worker.summary,
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
        match &outcome {
            Outcome::State(Ok(camera_state)) => self.camera_state = Some(*camera_state),
            _ => self.status = Some(worker::describe_outcome(&outcome)),
        }
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
            .show(ui, |ui| ui.scope(|ui| self.draw_contents(ui)).response.rect)
            .inner;

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
            Connection::Connected { link, summary } => {
                let camera = format!("{link} \u{2014} {summary}");
                self.draw_camera_row(ui, &camera);
            }
        }

        ui.add_space(8.0);
        if matches!(self.connection, Connection::Connected { .. }) {
            self.draw_controls(ui);
        } else {
            self.draw_connect_form(ui);
        }
    }

    /// What the camera is, whether it's awake, and the switch for that.
    fn draw_camera_row(&mut self, ui: &mut Ui, camera: &str) {
        ui.horizontal(|ui| {
            ui.label(camera);
            let powered = self.camera_state.map(|state| state.power_on);
            power_lamp(ui, powered);
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
    fn fit_window_to(&mut self, ctx: &egui::Context, size: Vec2) {
        if self.requested_size == Some(size) {
            return;
        }
        let first_fit = self.requested_size.is_none();
        self.requested_size = Some(size);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        if first_fit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
    }

    fn draw_connect_form(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("Camera:");
            ui.text_edit_singleline(&mut self.port_input);
        });
        ui.small("a serial port (/dev/ttyUSB0, COM3), or tcp://host:port for VISCA over IP");
        ui.add_space(8.0);
        let connecting = matches!(self.connection, Connection::Connecting);
        if ui
            .add_enabled(!connecting, egui::Button::new("Connect"))
            .clicked()
        {
            let ctx = ui.ctx().clone();
            self.start_connect(&ctx);
        }
        if let Connection::Failed(error) = &self.connection {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), error);
        }
    }

    fn draw_controls(&mut self, ui: &mut Ui) {
        ui.label(self.status.as_deref().unwrap_or("Ready"));
        ui.add_space(16.0);

        ui.horizontal(|ui| {
            let drives_height = ui
                .vertical(|ui| {
                    let pan_tilt = joystick::pan_tilt_pad(ui, 240.0);
                    if pan_tilt != self.pan_tilt {
                        self.pan_tilt = pan_tilt;
                        self.send_intent(Intent::DrivePanTilt(pan_tilt));
                    }

                    ui.add_space(16.0);
                    let (zoom, focus) = drive_buttons(ui);
                    if zoom != self.zoom {
                        self.zoom = zoom;
                        self.send_intent(Intent::DriveZoom(zoom));
                    }
                    if focus != self.focus {
                        self.focus = focus;
                        self.send_intent(Intent::DriveFocus(focus));
                    }

                    ui.add_space(8.0);
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

            ui.vertical(|ui| self.draw_presets(ui));
        });

        if let Some(state) = &self.camera_state {
            ui.add_space(16.0);
            ui.label(state::format_state(state));
        }
    }

    /// The preset slots: go to one, describe it, or store where the camera is
    /// pointing now.
    ///
    /// The description is the operator's, not the camera's — VISCA presets are
    /// bare numbers — so it lives in this program's config file and comes back
    /// on the next run.
    fn draw_presets(&mut self, ui: &mut Ui) {
        for number in 1..=PRESETS {
            ui.horizontal(|ui| {
                if ui
                    .button(number.to_string())
                    .on_hover_text(format!("Go to preset {number}"))
                    .clicked()
                {
                    self.send_intent(Intent::RecallPreset(number));
                }

                let description = self.preset_labels.entry(number).or_default();
                if ui.text_edit_singleline(description).lost_focus() {
                    self.save_preset_labels();
                }

                if ui
                    .button("Save")
                    .on_hover_text(format!("Store this position as preset {number}"))
                    .clicked()
                {
                    self.send_intent(Intent::SavePreset(number));
                }
            });
        }
    }

    /// Writes the preset descriptions back to the config file.
    ///
    /// Slots the operator has left blank are dropped rather than stored empty:
    /// the file is meant to be readable and editable by hand, and six empty
    /// strings say nothing.
    fn save_preset_labels(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let config = config::Config {
            presets: self
                .preset_labels
                .iter()
                .filter(|(_, description)| !description.trim().is_empty())
                .map(|(number, description)| (*number, description.clone()))
                .collect(),
        };
        if let Err(error) = config::save(&config, &path) {
            self.status = Some(error.to_string());
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
            let mut auto = self.camera_state.is_some_and(|state| state.auto_focus);
            if ui.toggle_value(&mut auto, "Auto focus").changed() {
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
/// zoom and focus keys.
fn drive_buttons(ui: &mut egui::Ui) -> (Option<ZoomDirection>, Option<FocusDirection>) {
    let mut zoom = None;
    let mut focus = None;
    ui.horizontal(|ui| {
        if ui.button("Zoom \u{2212}").is_pointer_button_down_on() {
            zoom = Some(ZoomDirection::Out);
        }
        if ui.button("Zoom +").is_pointer_button_down_on() {
            zoom = Some(ZoomDirection::In);
        }
        if ui.button("Focus \u{2212}").is_pointer_button_down_on() {
            focus = Some(FocusDirection::Near);
        }
        if ui.button("Focus +").is_pointer_button_down_on() {
            focus = Some(FocusDirection::Far);
        }
    });
    (zoom, focus)
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
        self.save_preset_labels();
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
                link: "Connected at 9600 baud".to_string(),
                summary: "vendor=Sony (0x0020)".to_string(),
            },
            intents: Some(intents),
            camera_state,
            ..App::default()
        };
        (app, sent)
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
            .get_all_by_label("Save")
            .nth(2)
            .expect("there should be a Save button for every preset")
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
    fn preset_slots_left_blank_are_not_written_out() {
        let path = test_config_path("blank-presets");
        let (mut app, _sent) = connected_app(None);
        app.config_path = Some(path.clone());
        app.preset_labels.insert(1, "Podium".to_string());
        app.preset_labels.insert(2, "   ".to_string());

        app.save_preset_labels();
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
    fn the_window_grows_to_fit_the_controls_once_connected() {
        let ctx = egui::Context::default();
        let screen = vec2(1600.0, 1200.0);
        let mut app = App::default();

        let form = requested_size(&frame_on_screen(&mut app, &ctx, screen))
            .expect("the first frame should size the window");
        app.connection = Connection::Connected {
            link: "Connected at 9600 baud".to_string(),
            summary: "vendor=Sony (0x0020)".to_string(),
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
