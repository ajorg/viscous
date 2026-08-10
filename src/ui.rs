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
    /// Still trying to reach a camera.
    #[default]
    Connecting,
    /// Connected, with the version-inquiry summary to show.
    Connected {
        /// How the connection was made, already phrased for display — the
        /// transports differ in what's worth saying (a baud rate, an address),
        /// so each one words its own.
        link: String,
        /// A human-readable version summary.
        version: String,
    },
    /// Nothing answered.
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

/// The one line of help, sized to be read rather than to list everything: it
/// shares the footer with the status line and has to fit a narrow terminal
/// whole, so it names one way to reach each control and leaves the synonyms
/// (`-=` for zoom, `<>` for focus, ctrl-D for quit) to be discovered.
pub const KEY_LEGEND: &str = "hold to move: arrows  []zoom  ,.focus  shift=fast  \
     1-6 preset  f auto  q quit";

/// The narrowest terminal [`KEY_LEGEND`] is expected to fit in.
pub const MIN_COLUMNS: usize = 80;

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
        Connection::Connected { link, version } => format!("{link} \u{2014} {version}"),
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
    fn shows_the_link_and_version_once_connected() {
        let state = AppState {
            connection: Connection::Connected {
                link: "Connected at 9600 baud".to_string(),
                version: "vendor=0x0001".to_string(),
            },
            ..AppState::default()
        };
        let content = render_to_string(&state);
        assert!(content.contains("Connected at 9600 baud"));
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
        assert!(content.contains(KEY_LEGEND));
    }

    #[test]
    fn the_key_legend_fits_a_narrow_terminal_whole() {
        let width = KEY_LEGEND.chars().count();
        assert!(
            width <= MIN_COLUMNS,
            "the legend shares the footer with the status line and gets clipped \
             rather than wrapped: {width} columns"
        );
    }

    #[test]
    fn shows_status_in_place_of_key_legend_when_set() {
        let state = AppState {
            status: Some("preset 3 saved".to_string()),
            ..AppState::default()
        };
        let content = render_to_string(&state);
        assert!(content.contains("preset 3 saved"));
        assert!(!content.contains("hold to move"));
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
