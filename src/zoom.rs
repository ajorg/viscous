//! Continuous zoom drive.
//!
//! VISCA zoom has only ever had continuous tele/wide drive commands — no
//! relative move, and `GenericVisca` doesn't support direct positioning
//! either — so hold-to-drive is the shape the camera already offers: one
//! command to start, one to stop, and however long the user holds the
//! control in between.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    transport::{BlockingTransport, HasTransportConfig},
};

/// Which way a zoom drive moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomDirection {
    /// Zoom in (telephoto).
    In,
    /// Zoom out (wide angle).
    Out,
}

/// Starts driving zoom in `direction`, or stops it when `direction` is
/// `None`.
pub fn drive_zoom<T>(
    camera: &BlockingClient<GenericVisca, T>,
    direction: Option<ZoomDirection>,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match direction {
        Some(ZoomDirection::In) => camera.zoom_tele(None),
        Some(ZoomDirection::Out) => camera.zoom_wide(None),
        None => camera.zoom_stop(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

    fn scripted_camera() -> BlockingClient<GenericVisca, ScriptedBlockingTransport> {
        let transport = ScriptedBlockingTransport::new(vec![helpers::standard_command_response(1)]);
        grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport")
    }

    #[test]
    fn drive_zoom_starts_a_drive_in_each_direction() {
        drive_zoom(&scripted_camera(), Some(ZoomDirection::In))
            .expect("scripted camera should ack and complete zoom in");
        drive_zoom(&scripted_camera(), Some(ZoomDirection::Out))
            .expect("scripted camera should ack and complete zoom out");
    }

    #[test]
    fn drive_zoom_stops_the_drive_when_given_no_direction() {
        drive_zoom(&scripted_camera(), None)
            .expect("scripted camera should ack and complete the stop");
    }
}
