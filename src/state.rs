//! Live camera state for the info panel: what the camera reports about
//! itself beyond the one-time version reply.

use grafton_visca::{
    BlockingClient, Error,
    camera::{PanTiltPosition, profiles::GenericVisca},
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
    })
}

/// Formats a camera state snapshot for display.
pub fn format_state(state: &CameraState) -> String {
    format!(
        "power={} pan={} tilt={} zoom={} focus={}",
        if state.power_on { "on" } else { "off" },
        state.pan_tilt.pan,
        state.pan_tilt.tilt,
        state.zoom,
        state.focus,
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
        }
    }

    #[test]
    fn format_state_renders_all_fields() {
        let text = format_state(&sample_state());
        assert!(text.contains("power=on"));
        assert!(text.contains("pan=-120"));
        assert!(text.contains("tilt=45"));
    }
}
