//! Discrete focus nudges.
//!
//! Like zoom, VISCA focus only exposes continuous near/far drive commands
//! (no relative move), so a discrete nudge is a brief timed drive.

use std::time::Duration;

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    transport::{BlockingTransport, HasTransportConfig},
    types::SpeedLevel,
};

use crate::drive::timed_drive;

/// Which way a focus nudge moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    /// Focus nearer.
    Near,
    /// Focus farther.
    Far,
}

/// How long a single focus nudge drives for.
pub const FOCUS_NUDGE_DURATION: Duration = Duration::from_millis(150);

/// How long a focus nudge sent with a "faster" modifier held drives for.
pub const FAST_FOCUS_NUDGE_DURATION: Duration = Duration::from_millis(400);

/// Sends a single timed focus nudge to a connected camera.
pub fn nudge_focus<T>(
    camera: &BlockingClient<GenericVisca, T>,
    direction: FocusDirection,
    duration: Duration,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    timed_drive(
        || match direction {
            FocusDirection::Near => camera.focus_near(SpeedLevel::Medium),
            FocusDirection::Far => camera.focus_far(SpeedLevel::Medium),
        },
        || camera.focus_stop(),
        duration,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

    #[test]
    fn nudge_focus_sends_a_start_and_a_stop_command() {
        let transport = ScriptedBlockingTransport::new(vec![
            helpers::standard_command_response(1),
            helpers::standard_command_response(1),
        ]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        nudge_focus(&camera, FocusDirection::Near, Duration::ZERO)
            .expect("scripted camera should ack and complete both commands");
    }
}
