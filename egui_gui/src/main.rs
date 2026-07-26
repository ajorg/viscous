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
use std::time::{Duration, Instant};

use eframe::egui;
use grafton_visca::camera::{Connect, profiles::GenericVisca};
use viscous::{
    app::{POLL_INTERVAL, QUIESCENCE_INTERVAL},
    connection::{
        DEFAULT_CAMERA_BAUD_RATES, ProbeOutcome, discover_baud_rate, format_version, query_version,
    },
    focus::FocusDirection,
    state,
    worker::{self, Intent, Outcome},
    zoom::ZoomDirection,
};

/// How long a single zoom or focus tap drives the camera for — mirrors the
/// TUI/CLI's fixed-duration nudge per keypress rather than inventing a
/// hold-to-drive interaction the rest of the app doesn't have.
const TAP_NUDGE: Duration = Duration::from_millis(150);

/// How often the joystick sends a nudge while held past its deadzone.
const JOYSTICK_NUDGE_INTERVAL: Duration = Duration::from_millis(150);

/// Where the camera connection attempt currently stands.
enum Connection {
    Disconnected,
    Connecting,
    Connected { baud_rate: u32, summary: String },
    Failed(String),
}

/// What a successful [`connect`] produces: the version summary plus the
/// channels the rest of the app uses to talk to the worker thread.
struct Connected {
    baud_rate: u32,
    summary: String,
    intents: Sender<Intent>,
    results: Receiver<Outcome>,
}

/// Discovers and connects to a camera on `port`, replaying the same
/// probe-then-reconnect sequence `main.rs` uses (discovery's own connection
/// only lives for the duration of the probe, so a second connection is
/// opened for the worker thread to hold onto), then starts its worker
/// thread.
fn connect(port: &str) -> Result<Connected, String> {
    let outcome = discover_baud_rate(DEFAULT_CAMERA_BAUD_RATES, |baud_rate| {
        let camera = Connect::open_serial_blocking::<GenericVisca>(port, baud_rate)?;
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

    let camera =
        Connect::open_serial_blocking::<GenericVisca>(port, baud_rate).map_err(|error| {
            format!("Connected during discovery but the follow-up connection failed: {error}")
        })?;

    let (worker_tx, worker_rx) = mpsc::channel::<Intent>();
    let (result_tx, result_rx) = mpsc::channel::<Outcome>();
    thread::spawn(move || worker::run(&camera, &worker_rx, &result_tx));

    Ok(Connected {
        baud_rate,
        summary: format_version(&version),
        intents: worker_tx,
        results: result_rx,
    })
}

struct App {
    port_input: String,
    connection: Connection,
    connect_rx: Option<Receiver<Result<Connected, String>>>,
    intents: Option<Sender<Intent>>,
    results: Option<Receiver<Outcome>>,
    camera_state: Option<String>,
    status: Option<String>,
    pending_state_query: bool,
    last_command_at: Instant,
    drag_offset: egui::Vec2,
    last_nudge_sent_at: Instant,
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
            drag_offset: egui::Vec2::ZERO,
            last_nudge_sent_at: Instant::now() - JOYSTICK_NUDGE_INTERVAL,
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

    /// Fires the debounced state query once the most recent command has had
    /// time to settle, same timing as [`viscous::app::run`]/[`viscous::cli::run`].
    fn send_query_state_if_due(&mut self) {
        if self.pending_state_query && self.last_command_at.elapsed() >= QUIESCENCE_INTERVAL {
            if let Some(intents) = &self.intents {
                let _ = intents.send(Intent::QueryState);
            }
            self.pending_state_query = false;
        }
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
            Ok(connected) => {
                self.connection = Connection::Connected {
                    baud_rate: connected.baud_rate,
                    summary: connected.summary,
                };
                self.intents = Some(connected.intents);
                self.results = Some(connected.results);
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
    /// camera-state panel instead — mirrors [`viscous::app::apply_outcome`].
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
            ui.label("Serial port:");
            ui.text_edit_singleline(&mut self.port_input);
        });
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
                let (_, nudge) = joystick::pan_tilt_pad(ui, 240.0, &mut self.drag_offset);
                if let Some((direction, degrees)) = nudge
                    && self.last_nudge_sent_at.elapsed() >= JOYSTICK_NUDGE_INTERVAL
                {
                    self.send_intent(Intent::NudgePanTilt(direction, degrees));
                    self.last_nudge_sent_at = Instant::now();
                }

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button("Zoom \u{2212}").clicked() {
                        self.send_intent(Intent::NudgeZoom(ZoomDirection::Out, TAP_NUDGE));
                    }
                    if ui.button("Zoom +").clicked() {
                        self.send_intent(Intent::NudgeZoom(ZoomDirection::In, TAP_NUDGE));
                    }
                    if ui.button("Focus \u{2212}").clicked() {
                        self.send_intent(Intent::NudgeFocus(FocusDirection::Near, TAP_NUDGE));
                    }
                    if ui.button("Focus +").clicked() {
                        self.send_intent(Intent::NudgeFocus(FocusDirection::Far, TAP_NUDGE));
                    }
                });
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
                Connection::Connected { baud_rate, summary } => {
                    Some(format!("Connected at {baud_rate} baud \u{2014} {summary}"))
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
        // the same cadence app.rs's own event loop uses.
        ui.ctx().request_repaint_after(POLL_INTERVAL);
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "Viscous",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}
