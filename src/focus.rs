//! Continuous focus drive.
//!
//! Like zoom, VISCA focus only exposes continuous near/far drive commands,
//! so focus is held rather than stepped: one command to start, one to stop.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    transport::{BlockingTransport, HasTransportConfig},
    types::SpeedLevel,
};

use crate::rocker;

/// Which way a focus drive moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    /// Focus nearer.
    Near,
    /// Focus farther.
    Far,
}

/// A focus drive: which way, and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusDrive {
    /// Which way focus is travelling.
    pub direction: FocusDirection,
    /// How fast. Focus is the control that most wants a slow setting: the
    /// distance between sharp and soft is small, and at the camera's own pace
    /// it is easy to drive straight past it.
    pub speed: SpeedLevel,
}

impl FocusDrive {
    /// The drive a rocker pushed to `deflection` asks for — negative for
    /// near, positive for far — or `None` while it's still at rest.
    pub fn from_deflection(deflection: f32) -> Option<Self> {
        let direction = if deflection < 0.0 {
            FocusDirection::Near
        } else {
            FocusDirection::Far
        };
        rocker::speed(deflection.abs()).map(|speed| Self { direction, speed })
    }
}

/// Starts `drive`, or stops the focus when it's `None`.
pub fn drive_focus<T>(
    camera: &BlockingClient<GenericVisca, T>,
    drive: Option<FocusDrive>,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match drive {
        Some(drive) => match drive.direction {
            FocusDirection::Near => camera.focus_near(drive.speed),
            FocusDirection::Far => camera.focus_far(drive.speed),
        },
        None => camera.focus_stop(),
    }
}

/// Switches focus between the camera's automatic and manual modes.
///
/// Manual is less a preference than a precondition: a camera focusing
/// automatically overrides the near/far drive the moment it sees the scene
/// again, so [`drive_focus`] only holds while focus is manual.
pub fn set_auto_focus<T>(camera: &BlockingClient<GenericVisca, T>, auto: bool) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    if auto {
        camera.focus_auto()
    } else {
        camera.focus_manual()
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

    fn drive(direction: FocusDirection) -> Option<FocusDrive> {
        Some(FocusDrive {
            direction,
            speed: SpeedLevel::Medium,
        })
    }

    #[test]
    fn drive_focus_starts_a_drive_in_each_direction() {
        drive_focus(&scripted_camera(), drive(FocusDirection::Near))
            .expect("scripted camera should ack and complete focus near");
        drive_focus(&scripted_camera(), drive(FocusDirection::Far))
            .expect("scripted camera should ack and complete focus far");
    }

    #[test]
    fn a_rocker_at_rest_asks_for_no_focus_and_either_way_off_centre_focuses() {
        assert_eq!(FocusDrive::from_deflection(0.0), None);
        assert_eq!(
            FocusDrive::from_deflection(1.0).map(|drive| drive.direction),
            Some(FocusDirection::Far)
        );
        assert_eq!(
            FocusDrive::from_deflection(-1.0).map(|drive| drive.direction),
            Some(FocusDirection::Near)
        );
    }

    #[test]
    fn pushing_the_rocker_further_focuses_faster() {
        let nudged = FocusDrive::from_deflection(0.2).expect("past the deadzone should drive");
        let pushed = FocusDrive::from_deflection(1.0).expect("past the deadzone should drive");

        assert_ne!(nudged.speed, pushed.speed);
    }

    #[test]
    fn drive_focus_stops_the_drive_when_given_no_direction() {
        drive_focus(&scripted_camera(), None)
            .expect("scripted camera should ack and complete the stop");
    }

    #[test]
    fn set_auto_focus_switches_the_camera_both_ways() {
        set_auto_focus(&scripted_camera(), true).expect("scripted camera should ack auto focus");
        set_auto_focus(&scripted_camera(), false).expect("scripted camera should ack manual focus");
    }
}
