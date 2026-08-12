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
    types::SpeedLevel,
};

use crate::rocker;

/// Which way a zoom drive moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomDirection {
    /// Zoom in (telephoto).
    In,
    /// Zoom out (wide angle).
    Out,
}

/// A zoom drive: which way, and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomDrive {
    /// Which way the shot is going.
    pub direction: ZoomDirection,
    /// How fast, which the camera holds until told otherwise. A slow zoom is
    /// the difference between a shot that can go out live and one that
    /// lurches, so this is worth expressing rather than leaving to the
    /// camera's default.
    pub speed: SpeedLevel,
}

impl ZoomDrive {
    /// The drive a rocker pushed to `deflection` asks for — negative for
    /// wide, positive for tele — or `None` while it's still at rest.
    pub fn from_deflection(deflection: f32) -> Option<Self> {
        let direction = if deflection < 0.0 {
            ZoomDirection::Out
        } else {
            ZoomDirection::In
        };
        rocker::speed(deflection.abs()).map(|speed| Self { direction, speed })
    }
}

/// Starts `drive`, or stops the zoom when it's `None`.
pub fn drive_zoom<T>(
    camera: &BlockingClient<GenericVisca, T>,
    drive: Option<ZoomDrive>,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match drive {
        Some(drive) => {
            let speed = Some(drive.speed.into());
            match drive.direction {
                ZoomDirection::In => camera.zoom_tele(speed),
                ZoomDirection::Out => camera.zoom_wide(speed),
            }
        }
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

    fn drive(direction: ZoomDirection) -> Option<ZoomDrive> {
        Some(ZoomDrive {
            direction,
            speed: SpeedLevel::Medium,
        })
    }

    #[test]
    fn drive_zoom_starts_a_drive_in_each_direction() {
        drive_zoom(&scripted_camera(), drive(ZoomDirection::In))
            .expect("scripted camera should ack and complete zoom in");
        drive_zoom(&scripted_camera(), drive(ZoomDirection::Out))
            .expect("scripted camera should ack and complete zoom out");
    }

    #[test]
    fn a_rocker_at_rest_asks_for_no_zoom_and_either_way_off_centre_zooms() {
        assert_eq!(ZoomDrive::from_deflection(0.0), None);
        assert_eq!(
            ZoomDrive::from_deflection(1.0).map(|drive| drive.direction),
            Some(ZoomDirection::In)
        );
        assert_eq!(
            ZoomDrive::from_deflection(-1.0).map(|drive| drive.direction),
            Some(ZoomDirection::Out)
        );
    }

    #[test]
    fn pushing_the_rocker_further_zooms_faster() {
        let nudged = ZoomDrive::from_deflection(0.2).expect("past the deadzone should drive");
        let pushed = ZoomDrive::from_deflection(1.0).expect("past the deadzone should drive");

        assert_ne!(nudged.speed, pushed.speed);
    }

    #[test]
    fn drive_zoom_stops_the_drive_when_given_no_direction() {
        drive_zoom(&scripted_camera(), None)
            .expect("scripted camera should ack and complete the stop");
    }
}
