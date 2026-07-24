use std::io;
use std::process::ExitCode;
use std::sync::mpsc::channel;
use std::thread;

use grafton_visca::camera::{Connect, profiles::GenericVisca};
use viscous::{
    app, cli,
    connection::{
        DEFAULT_CAMERA_BAUD_RATES, ProbeOutcome, discover_baud_rate, format_version, query_version,
    },
    ui::Connection,
    worker,
};

/// Parses `viscous [--cli] <serial-port>`, returning whether bare CLI mode
/// was explicitly requested and the port path.
fn parse_args(args: impl Iterator<Item = String>) -> Option<(bool, String)> {
    let mut cli_requested = false;
    let mut port = None;
    for arg in args {
        if arg == "--cli" {
            cli_requested = true;
        } else if port.is_none() {
            port = Some(arg);
        }
    }
    Some((cli_requested, port?))
}

fn main() -> ExitCode {
    let Some((cli_requested, port)) = parse_args(std::env::args().skip(1)) else {
        eprintln!("usage: viscous [--cli] <serial-port>");
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

    // Not joined on the way out: a command already fully written to the
    // wire is already executing on the camera regardless of whether we
    // stick around for its reply, so there's nothing to gain by blocking
    // process exit on an in-flight command's ack/completion round trip
    // (which, for a preset recall, can be tens of seconds).
    thread::spawn(move || {
        worker::run(&camera, &intent_rx, &result_tx);
    });

    let connection_summary = format!(
        "Connected at {baud_rate} baud \u{2014} {}",
        format_version(&version)
    );

    let app_result = if cli_requested {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        cli::run(&mut stdout, &connection_summary, &intent_tx, &result_rx)
            .map_err(|error| grafton_visca::Error::TransportError(error.to_string().into()))
    } else {
        let connection = Connection::Connected {
            baud_rate,
            version: format_version(&version),
        };
        let mut terminal = ratatui::init();
        let result = app::run(&mut terminal, connection, &intent_tx, &result_rx);
        ratatui::restore();
        result
    };

    match app_result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
