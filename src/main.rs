use std::process::ExitCode;
use std::sync::mpsc::channel;
use std::thread;

use grafton_visca::camera::{Connect, profiles::GenericVisca};
use viscous::{
    app,
    connection::{
        DEFAULT_CAMERA_BAUD_RATES, ProbeOutcome, discover_baud_rate, format_version, query_version,
    },
    ui::Connection,
    worker,
};

fn main() -> ExitCode {
    let Some(port) = std::env::args().nth(1) else {
        eprintln!("usage: viscous <serial-port>");
        return ExitCode::FAILURE;
    };

    let outcome = discover_baud_rate(DEFAULT_CAMERA_BAUD_RATES, |baud_rate| {
        let camera = Connect::open_serial_blocking::<GenericVisca>(&port, baud_rate)?;
        query_version(&camera)
    });

    let (baud_rate, version) = match outcome {
        ProbeOutcome::Connected { baud_rate, version } => (baud_rate, version),
        ProbeOutcome::NoResponse => {
            eprintln!(
                "No response from camera on {port} at any of the candidate baud rates: {DEFAULT_CAMERA_BAUD_RATES:?}"
            );
            return ExitCode::FAILURE;
        }
    };

    // Discovery already proved a camera answers at `baud_rate`; reconnect
    // once more for a client the worker thread can hold onto, since the
    // discovery closure's client only lived for the duration of that probe.
    let camera = match Connect::open_serial_blocking::<GenericVisca>(&port, baud_rate) {
        Ok(camera) => camera,
        Err(error) => {
            eprintln!("Connected during discovery but the follow-up connection failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    let (intent_tx, intent_rx) = channel::<worker::Intent>();
    let (result_tx, result_rx) = channel::<worker::Outcome>();

    let worker_handle = thread::spawn(move || {
        worker::run(&camera, &intent_rx, &result_tx);
    });

    let connection = Connection::Connected {
        baud_rate,
        version: format_version(&version),
    };

    let mut terminal = ratatui::init();
    let app_result = app::run(&mut terminal, connection, &intent_tx, &result_rx);
    ratatui::restore();

    drop(intent_tx);
    let _ = worker_handle.join();

    match app_result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
