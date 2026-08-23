//! A bare command mode for when the full-screen redraws are more than you want
//! to parse from a script (e.g. driving `viscous-tui` over an
//! `expect`-controlled pseudoterminal).
//!
//! This is the same interactive session as the full-screen UI — same keys, same
//! hold-to-drive movement, same debounced state query (see
//! [`session`](crate::session)) — printing each result as a line of text
//! instead of updating a rendered frame. Like the full-screen UI it needs a real
//! controlling terminal on stdin, since raw mode reads individual keystrokes
//! without waiting for Enter; it's an alternative to the full-screen UI, not a
//! way to drive the camera with no terminal at all.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, Sender};

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::{
    session::{self, Report},
    ui::KEY_LEGEND,
    worker::{Intent, Outcome},
};

/// Writes one line of output ending in an explicit `\r\n`.
///
/// Raw mode (needed for single-keystroke reads) disables the terminal's
/// usual `\n` → `\r\n` translation on output too, so a bare `writeln!` would
/// move the cursor down without returning it to the start of the line —
/// each subsequent line staircasing further right than the last.
fn write_line(stdout: &mut impl Write, text: &str) -> io::Result<()> {
    write!(stdout, "{text}\r\n")
}

/// The bare CLI's view of a session: every message is a line of transcript,
/// and there's no frame to redraw between them.
struct Transcript<'a, W: Write> {
    stdout: &'a mut W,
}

impl<W: Write> Report for Transcript<'_, W> {
    fn refresh(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn status(&mut self, text: &str) -> io::Result<()> {
        write_line(self.stdout, text)
    }

    fn camera_state(&mut self, text: &str) -> io::Result<()> {
        write_line(self.stdout, text)
    }
}

/// Runs the bare command loop: prints `connection_summary` and the key
/// legend once, then reads and acts on keystrokes until the quit key or the
/// worker thread goes away.
pub fn run(
    stdout: &mut impl Write,
    connection_summary: &str,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> io::Result<()> {
    write_line(stdout, connection_summary)?;
    write_line(stdout, KEY_LEGEND)?;

    enable_raw_mode()?;
    let result = session::run(intents, results, &mut Transcript { stdout });
    disable_raw_mode()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_line_terminates_with_a_full_carriage_return_and_newline() {
        let mut stdout = Vec::new();
        write_line(&mut stdout, "hello").unwrap();
        assert_eq!(stdout, b"hello\r\n");
    }

    #[test]
    fn every_kind_of_message_becomes_a_line_of_transcript() {
        let mut stdout = Vec::new();
        {
            let mut transcript = Transcript {
                stdout: &mut stdout,
            };
            transcript.refresh().unwrap();
            transcript.status("pan/tilt right...").unwrap();
            transcript.camera_state("power=on pan=0 tilt=0").unwrap();
        }

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "pan/tilt right...\r\npower=on pan=0 tilt=0\r\n"
        );
    }
}
