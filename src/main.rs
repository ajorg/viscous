use std::io;
use std::process::ExitCode;

use viscous::{
    app, cli,
    connection::{Target, format_camera, format_version},
    ui::Connection,
    worker::{self, Worker},
};

/// Parses `viscous-tui [--cli] <target>`, returning whether bare CLI mode was
/// explicitly requested and what to connect to.
fn parse_args(args: impl Iterator<Item = String>) -> Option<(bool, String)> {
    let mut cli_requested = false;
    let mut target = None;
    for arg in args {
        if arg == "--cli" {
            cli_requested = true;
        } else if target.is_none() {
            target = Some(arg);
        }
    }
    Some((cli_requested, target?))
}

fn main() -> ExitCode {
    let Some((cli_requested, target)) = parse_args(std::env::args().skip(1)) else {
        eprintln!("usage: viscous-tui [--cli] <serial-port|tcp://host[:port]>");
        return ExitCode::FAILURE;
    };

    // The worker's thread isn't joined on the way out: a command already fully
    // written to the wire is already executing on the camera regardless of
    // whether we stick around for its reply, so there's nothing to gain by
    // blocking process exit on an in-flight command's round trip (which, for a
    // preset recall, can be tens of seconds).
    let Worker {
        link,
        version,
        intents,
        results,
        ..
    // No waking needed: the terminal session already wakes to check for
    // results between keystrokes.
    } = match worker::start(Target::from(target.as_str()), || {}) {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    // The transcript has room for the whole version reply; the TUI's one-line
    // header does not, and says who made the camera instead.
    let connection_summary = format!("{link} \u{2014} {}", format_version(&version));

    let app_result = if cli_requested {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        cli::run(&mut stdout, &connection_summary, &intents, &results)
    } else {
        let connection = Connection::Connected {
            link,
            version: format_camera(&version),
        };
        let mut terminal = ratatui::init();
        let result = app::run(&mut terminal, connection, &intents, &results);
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
