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

/// Which way a focus drive moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    /// Focus nearer.
    Near,
    /// Focus farther.
    Far,
}

/// Starts driving focus in `direction`, or stops it when `direction` is
/// `None`.
pub fn drive_focus<T>(
    camera: &BlockingClient<GenericVisca, T>,
    direction: Option<FocusDirection>,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    match direction {
        Some(FocusDirection::Near) => camera.focus_near(SpeedLevel::Medium),
        Some(FocusDirection::Far) => camera.focus_far(SpeedLevel::Medium),
        None => camera.focus_stop(),
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
    fn drive_focus_starts_a_drive_in_each_direction() {
        drive_focus(&scripted_camera(), Some(FocusDirection::Near))
            .expect("scripted camera should ack and complete focus near");
        drive_focus(&scripted_camera(), Some(FocusDirection::Far))
            .expect("scripted camera should ack and complete focus far");
    }

    #[test]
    fn drive_focus_stops_the_drive_when_given_no_direction() {
        drive_focus(&scripted_camera(), None)
            .expect("scripted camera should ack and complete the stop");
    }
}
