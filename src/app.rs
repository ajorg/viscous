//! The TUI front end: renders the shared interactive session
//! ([`crate::session`]) into a ratatui frame.

use std::io;
use std::sync::mpsc::{Receiver, Sender};

use ratatui::{DefaultTerminal, Terminal, backend::Backend};

use crate::{
    session::{self, Report},
    ui::{self, AppState, Connection},
    worker::{Intent, Outcome},
};

/// The TUI's view of a session: results become the status line, a state
/// snapshot updates the camera-state panel, and every pass of the loop
/// redraws the frame.
struct Tui<'a, B: Backend> {
    terminal: &'a mut Terminal<B>,
    state: AppState,
}

impl<B: Backend> Report for Tui<'_, B>
where
    // Every backend's error is an `Error`; these are what it takes to box one
    // up as an `io::Error`, which is the currency the session loop deals in.
    B::Error: Send + Sync + 'static,
{
    fn refresh(&mut self) -> io::Result<()> {
        // Split the borrow: the draw closure reads the state that sits right
        // next to the terminal being drawn to.
        let Self { terminal, state } = self;
        terminal
            .draw(|frame| ui::render(frame, state))
            .map_err(io::Error::other)?;
        Ok(())
    }

    fn status(&mut self, text: &str) -> io::Result<()> {
        self.state.status = Some(text.to_string());
        Ok(())
    }

    fn camera_state(&mut self, text: &str) -> io::Result<()> {
        self.state.camera_state = Some(text.to_string());
        Ok(())
    }
}

/// Runs the interactive TUI until the user quits or the worker thread goes
/// away.
pub fn run(
    terminal: &mut DefaultTerminal,
    connection: Connection,
    intents: &Sender<Intent>,
    results: &Receiver<Outcome>,
) -> io::Result<()> {
    let mut tui = Tui {
        terminal,
        state: AppState {
            connection,
            ..AppState::default()
        },
    };
    session::run(intents, results, &mut tui)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn test_terminal() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(80, 10)).expect("test backend should initialize")
    }

    fn rendered(backend: &TestBackend) -> String {
        backend
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn a_result_becomes_the_status_line_and_leaves_the_state_panel_alone() {
        let mut terminal = test_terminal();
        let mut tui = Tui {
            terminal: &mut terminal,
            state: AppState::default(),
        };

        tui.status("recall preset 3...").unwrap();

        assert_eq!(tui.state.status.as_deref(), Some("recall preset 3..."));
        assert_eq!(tui.state.camera_state, None);
    }

    #[test]
    fn a_state_snapshot_becomes_the_state_panel_and_leaves_the_status_alone() {
        let mut terminal = test_terminal();
        let mut tui = Tui {
            terminal: &mut terminal,
            state: AppState {
                status: Some("stale".to_string()),
                ..AppState::default()
            },
        };

        tui.camera_state("power=on pan=0 tilt=0").unwrap();

        assert_eq!(
            tui.state.camera_state.as_deref(),
            Some("power=on pan=0 tilt=0")
        );
        assert_eq!(tui.state.status.as_deref(), Some("stale"));
    }

    #[test]
    fn refreshing_draws_the_current_state() {
        let mut terminal = test_terminal();
        let mut tui = Tui {
            terminal: &mut terminal,
            state: AppState::default(),
        };

        tui.camera_state("power=on pan=-120 tilt=45").unwrap();
        tui.refresh().unwrap();

        assert!(rendered(terminal.backend()).contains("pan=-120"));
    }
}
