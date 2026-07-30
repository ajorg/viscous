//! Pure rendering: turns [`AppState`] into ratatui widgets.
//!
//! Kept free of any camera/terminal I/O so it can be exercised with
//! ratatui's `TestBackend` instead of a real terminal.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::{Block, Borders, Paragraph},
};

/// Where the camera connection attempt currently stands.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Connection {
    /// Still probing candidate baud rates.
    #[default]
    Connecting,
    /// Connected, with the version-inquiry summary to show.
    Connected {
        /// The baud rate that worked.
        baud_rate: u32,
        /// A human-readable version summary.
        version: String,
    },
    /// No candidate baud rate produced a response.
    Failed(String),
}

/// Everything the UI needs to render a frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppState {
    /// Connection status, shown in the header.
    pub connection: Connection,
    /// The most recently polled camera state, if any.
    pub camera_state: Option<String>,
    /// The result of the last command sent, shown in the footer in place of
    /// the key legend when present.
    pub status: Option<String>,
}

pub const KEY_LEGEND: &str = "hold to move \u{2014} arrows/shift+arrows: pan-tilt  []/-=: zoom  \
     ,./<>: focus  1-6: recall preset  q/ctrl-d: quit";

/// Renders `state` into `frame`.
pub fn render(frame: &mut Frame, state: &AppState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let header_text = match &state.connection {
        Connection::Connecting => "Connecting...".to_string(),
        Connection::Connected { baud_rate, version } => {
            format!("Connected at {baud_rate} baud \u{2014} {version}")
        }
        Connection::Failed(reason) => format!("Connection failed: {reason}"),
    };
    frame.render_widget(Paragraph::new(header_text), header);

    let body_text = state.camera_state.as_deref().unwrap_or("(no state yet)");
    frame.render_widget(
        Paragraph::new(body_text).block(Block::default().borders(Borders::ALL)),
        body,
    );

    let footer_text = state.status.as_deref().unwrap_or(KEY_LEGEND);
    frame.render_widget(Paragraph::new(footer_text), footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render_to_string(state: &AppState) -> String {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).expect("test backend should initialize");
        terminal
            .draw(|frame| render(frame, state))
            .expect("draw should succeed");

        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn shows_connecting_by_default() {
        let content = render_to_string(&AppState::default());
        assert!(content.contains("Connecting"));
    }

    #[test]
    fn shows_baud_rate_and_version_once_connected() {
        let state = AppState {
            connection: Connection::Connected {
                baud_rate: 9600,
                version: "vendor=0x0001".to_string(),
            },
            ..AppState::default()
        };
        let content = render_to_string(&state);
        assert!(content.contains("9600"));
        assert!(content.contains("vendor=0x0001"));
    }

    #[test]
    fn shows_failure_reason() {
        let state = AppState {
            connection: Connection::Failed("no response".to_string()),
            ..AppState::default()
        };
        let content = render_to_string(&state);
        assert!(content.contains("no response"));
    }

    #[test]
    fn shows_key_legend_when_no_status_set() {
        let content = render_to_string(&AppState::default());
        // Matched near the front of the legend: it's longer than the 80
        // columns this renders into, so the tail gets clipped.
        assert!(content.contains("pan-tilt"));
    }

    #[test]
    fn shows_status_in_place_of_key_legend_when_set() {
        let state = AppState {
            status: Some("preset 3 saved".to_string()),
            ..AppState::default()
        };
        let content = render_to_string(&state);
        assert!(content.contains("preset 3 saved"));
        assert!(!content.contains("pan-tilt"));
    }

    #[test]
    fn shows_camera_state_when_present() {
        let state = AppState {
            camera_state: Some("power=on pan=0 tilt=0".to_string()),
            ..AppState::default()
        };
        let content = render_to_string(&state);
        assert!(content.contains("power=on"));
    }
}
