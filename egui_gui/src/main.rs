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

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use eframe::egui;
use viscous::{
    connection::{self, Target, format_version},
    focus::FocusDirection,
    pan_tilt::Velocity,
    session::{POLL_INTERVAL, QUIESCENCE_INTERVAL},
    state,
    worker::{self, Intent, Outcome},
    zoom::ZoomDirection,
};

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
    camera_state: Option<String>,
    status: Option<String>,
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
            pending_state_query: false,
            last_command_at: Instant::now(),
            pan_tilt: Velocity::STOP,
            zoom: None,
            focus: None,
        }
    }
}

impl App {
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
            Outcome::State(Ok(camera_state)) => {
                self.camera_state = Some(state::format_state(camera_state))
            }
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

    fn draw_connect_form(&mut self, ui: &mut egui::Ui) {
        ui.add_space(48.0);
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

    fn draw_controls(&mut self, ui: &mut egui::Ui) {
        ui.label(self.status.as_deref().unwrap_or("Ready"));
        ui.add_space(16.0);

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
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
            });

            ui.separator();

            ui.vertical(|ui| {
                for number in 1..=6u8 {
                    ui.horizontal(|ui| {
                        if ui.button(format!("Preset {number}")).clicked() {
                            self.send_intent(Intent::RecallPreset(number));
                        }
                        if ui.button("Save").clicked() {
                            self.send_intent(Intent::SavePreset(number));
                        }
                    });
                }
            });
        });

        if let Some(state) = &self.camera_state {
            ui.add_space(16.0);
            ui.label(state);
        }
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_connect();
        self.drain_results();
        self.send_query_state_if_due();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Viscous");

            let header = match &self.connection {
                Connection::Disconnected | Connection::Failed(_) => None,
                Connection::Connecting => Some("Connecting...".to_string()),
                Connection::Connected { link, summary } => {
                    Some(format!("{link} \u{2014} {summary}"))
                }
            };
            if let Some(text) = header {
                ui.label(text);
            }

            ui.add_space(8.0);
            if matches!(self.connection, Connection::Connected { .. }) {
                self.draw_controls(ui);
            } else {
                self.draw_connect_form(ui);
            }
        });

        // Keep polling for worker results / the debounced state query at
        // the same cadence the session loop's own event poll uses.
        ui.ctx().request_repaint_after(POLL_INTERVAL);
    }

    fn on_exit(&mut self) {
        self.stop_all_drives();
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "Viscous",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}
