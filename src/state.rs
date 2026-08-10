//! Live camera state for the info panel: what the camera reports about
//! itself beyond the one-time version reply.

use grafton_visca::{
    BlockingClient, Error,
    camera::{PanTiltPosition, profiles::GenericVisca},
    command::FocusMode,
    transport::{BlockingTransport, HasTransportConfig},
    types::{FocusPosition, ZoomPosition},
};

/// A snapshot of the camera's current pan/tilt/zoom/focus/power state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraState {
    /// Whether the camera is powered on.
    pub power_on: bool,
    /// Current pan/tilt position, in raw camera units.
    pub pan_tilt: PanTiltPosition,
    /// Current zoom position.
    pub zoom: ZoomPosition,
    /// Current focus position.
    pub focus: FocusPosition,
    /// Whether the camera is focusing automatically, in which case a manual
    /// focus drive won't hold.
    pub auto_focus: bool,
}

/// Queries a connected camera for its current state.
pub fn query_state<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<CameraState, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    Ok(CameraState {
        power_on: camera.power_state()?,
        pan_tilt: camera.pan_tilt_position()?,
        zoom: camera.zoom_position()?,
        focus: camera.focus_position()?,
        auto_focus: camera.focus_mode()? == FocusMode::Auto,
    })
}

/// Formats where the camera is pointing, zoomed and focused.
///
/// Split out from [`format_state`] for front ends that show the camera's power
/// and focus mode in controls of their own: repeating them in a readout
/// underneath only invites the two to disagree.
pub fn format_position(state: &CameraState) -> String {
    // `ZoomPosition`/`FocusPosition`'s own `Display` impls already read
    // "Zoom 0x1000"/"Focus 0x1000" (grafton-visca bakes the label in), so
    // pairing that with our own "zoom="/"focus=" label would double it up;
    // use the raw value instead.
    format!(
        "pan={} tilt={} zoom=0x{:04X} focus=0x{:04X}",
        state.pan_tilt.pan,
        state.pan_tilt.tilt,
        state.zoom.value(),
        state.focus.value(),
    )
}

/// Formats a camera state snapshot in full, for a display with nowhere else to
/// put the power and focus mode.
pub fn format_state(state: &CameraState) -> String {
    format!(
        "power={} {} ({})",
        if state.power_on { "on" } else { "off" },
        format_position(state),
        if state.auto_focus { "auto" } else { "manual" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> CameraState {
        CameraState {
            power_on: true,
            pan_tilt: PanTiltPosition::new(-120, 45),
            zoom: ZoomPosition::try_from(0x1000u16).unwrap(),
            focus: FocusPosition::new(0x2000),
            auto_focus: false,
        }
    }

    #[test]
    fn format_state_renders_all_fields() {
        let text = format_state(&sample_state());
        assert!(text.contains("power=on"));
        assert!(text.contains("pan=-120"));
        assert!(text.contains("tilt=45"));
        assert!(text.contains("zoom=0x1000"));
        assert!(text.contains("focus=0x2000"));
        assert!(text.contains("manual"));
    }

    #[test]
    fn format_state_says_which_way_focus_is_being_driven() {
        let auto = CameraState {
            auto_focus: true,
            ..sample_state()
        };

        assert!(format_state(&auto).contains("auto"));
    }

    #[test]
    fn format_position_leaves_out_what_a_control_already_shows() {
        let text = format_position(&sample_state());

        assert!(text.contains("pan=-120"));
        assert!(text.contains("zoom=0x1000"));
        assert!(!text.contains("power"));
        assert!(!text.contains("manual"));
    }

    #[test]
    fn format_state_does_not_double_up_the_zoom_and_focus_labels() {
        // ZoomPosition/FocusPosition's own Display impls already read
        // "Zoom 0x1000"/"Focus 0x1000"; make sure format_state doesn't pair
        // that with its own "zoom="/"focus=" label.
        let text = format_state(&sample_state());
        assert!(!text.contains("Zoom"));
        assert!(!text.contains("Focus"));
    }
}
