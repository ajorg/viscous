//! A bare command mode for when the TUI's full-screen redraws are more than
//! you want to parse from a script (e.g. driving `viscous` over an
//! `expect`-controlled pseudoterminal).
//!
//! Reads the exact same single-keystroke input as the TUI — see
//! [`keymap`](crate::keymap) — and drives the exact same poll/dispatch/drain
//! loop as [`app::run`](crate::app::run) (fire off a command without
//! waiting for it, so a burst of nudges doesn't back up behind the camera's
//! round trip; debounce the follow-up state query the same way) — but
//! prints each result as a line of text instead of updating a rendered
//! frame. Like the TUI, this needs a real controlling terminal on stdin,
//! since raw mode reads individual keystrokes without waiting for Enter;
//! it's an alternative to the full-screen UI, not a way to drive the camera
//! with no terminal at all.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Instant;

use crossterm::event::{self, Event};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::{
    app::{POLL_INTERVAL, QUIESCENCE_INTERVAL},
    keymap::{self, Action},
    ui::KEY_LEGEND,
    worker::{self, Intent, Outcome},
};

/// Runs the bare command loop: prints `connection_summary` and the key
/// legend once, then reads and acts on keystrokes until the quit key or the
/// worker thread goes away.
pub fn run(
    stdout: &mut impl Write,
    connection_summary: &str,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> io::Result<()> {
    writeln!(stdout, "{connection_summary}")?;
    writeln!(stdout, "{KEY_LEGEND}")?;

    enable_raw_mode()?;
    let result = run_loop(stdout, intents, results);
    disable_raw_mode()?;
    result
}

fn run_loop(
    stdout: &mut impl Write,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> io::Result<()> {
    let mut pending_state_query = true;
    let mut last_command_at = Instant::now() - QUIESCENCE_INTERVAL;

    loop {
        if event::poll(POLL_INTERVAL)?
            && let Event::Key(key) = event::read()?
        {
            match keymap::map_key(key) {
                Some(Action::Quit) => return Ok(()),
                Some(Action::Camera(intent)) => {
                    let _ = intents.send(intent);
                    pending_state_query = true;
                    last_command_at = Instant::now();
                }
                None => {}
            }
        }

        loop {
            match results.try_recv() {
                Ok(outcome) => writeln!(stdout, "{}", worker::describe_outcome(&outcome))?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        if pending_state_query && last_command_at.elapsed() >= QUIESCENCE_INTERVAL {
            let _ = intents.send(Intent::QueryState);
            pending_state_query = false;
        }
    }
}
