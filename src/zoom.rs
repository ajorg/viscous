//! Discrete zoom nudges.
//!
//! VISCA zoom only exposes continuous tele/wide drive commands (no relative
//! move, and `GenericVisca` doesn't support direct/absolute positioning
//! either), so a discrete nudge is a brief timed drive.

use std::time::Duration;

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    transport::{BlockingTransport, HasTransportConfig},
};

use crate::drive::timed_drive;

/// Which way a zoom nudge moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomDirection {
    /// Zoom in (telephoto).
    In,
    /// Zoom out (wide angle).
    Out,
}

/// How long a single zoom nudge drives for.
pub const ZOOM_NUDGE_DURATION: Duration = Duration::from_millis(150);

/// How long a zoom nudge sent with a "faster" modifier held drives for.
pub const FAST_ZOOM_NUDGE_DURATION: Duration = Duration::from_millis(400);

/// Sends a single timed zoom nudge to a connected camera.
pub fn nudge_zoom<T>(
    camera: &BlockingClient<GenericVisca, T>,
    direction: ZoomDirection,
    duration: Duration,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    timed_drive(
        || match direction {
            ZoomDirection::In => camera.zoom_tele(None),
            ZoomDirection::Out => camera.zoom_wide(None),
        },
        || camera.zoom_stop(),
        duration,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

    #[test]
    fn nudge_zoom_sends_a_start_and_a_stop_command() {
        let transport = ScriptedBlockingTransport::new(vec![
            helpers::standard_command_response(1),
            helpers::standard_command_response(1),
        ]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        nudge_zoom(&camera, ZoomDirection::In, Duration::ZERO)
            .expect("scripted camera should ack and complete both commands");
    }
}
