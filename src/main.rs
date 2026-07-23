use std::process::ExitCode;

use grafton_visca::camera::{Connect, profiles::GenericVisca};
use viscous::connection::{
    DEFAULT_CAMERA_BAUD_RATES, ProbeOutcome, discover_baud_rate, format_version, query_version,
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

    match outcome {
        ProbeOutcome::Connected { baud_rate, version } => {
            println!(
                "Connected at {baud_rate} baud: {}",
                format_version(&version)
            );
            ExitCode::SUCCESS
        }
        ProbeOutcome::NoResponse => {
            eprintln!(
                "No response from camera on {port} at any of the candidate baud rates: {DEFAULT_CAMERA_BAUD_RATES:?}"
            );
            ExitCode::FAILURE
        }
    }
}
