//! Discrete pan/tilt nudges via VISCA's relative-move command.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    transport::{BlockingTransport, HasTransportConfig},
    types::SpeedLevel,
    units::Degrees,
};

/// The eight directions a single pan/tilt nudge can move in.
///
/// This mirrors [`grafton_visca::command::PanTiltDirection`] but omits
/// `Stop`, which has no meaning for a discrete relative move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NudgeDirection {
    /// Move up.
    Up,
    /// Move down.
    Down,
    /// Move left.
    Left,
    /// Move right.
    Right,
    /// Move diagonally up and to the left.
    UpLeft,
    /// Move diagonally up and to the right.
    UpRight,
    /// Move diagonally down and to the left.
    DownLeft,
    /// Move diagonally down and to the right.
    DownRight,
}

/// The pan/tilt step size for a single nudge, in degrees.
pub const NUDGE_DEGREES: f64 = 2.0;

/// The step size for a nudge sent with a "faster" modifier held, in degrees.
pub const FAST_NUDGE_DEGREES: f64 = 8.0;

/// Computes the pan/tilt offset for a single nudge in the given direction,
/// using `step_degrees` for whichever axes that direction moves along.
///
/// Diagonals fall out naturally from moving both axes in the same relative
/// command, rather than needing their own case: VISCA's relative move
/// accepts independent nonzero pan and tilt deltas in one command.
pub fn nudge_offset(direction: NudgeDirection, step_degrees: f64) -> (f64, f64) {
    let (pan_sign, tilt_sign): (f64, f64) = match direction {
        NudgeDirection::Up => (0.0, 1.0),
        NudgeDirection::Down => (0.0, -1.0),
        NudgeDirection::Left => (-1.0, 0.0),
        NudgeDirection::Right => (1.0, 0.0),
        NudgeDirection::UpLeft => (-1.0, 1.0),
        NudgeDirection::UpRight => (1.0, 1.0),
        NudgeDirection::DownLeft => (-1.0, -1.0),
        NudgeDirection::DownRight => (1.0, -1.0),
    };
    (pan_sign * step_degrees, tilt_sign * step_degrees)
}

/// Sends a single relative pan/tilt nudge to a connected camera.
pub fn nudge_pan_tilt<T>(
    camera: &BlockingClient<GenericVisca, T>,
    direction: NudgeDirection,
    step_degrees: f64,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    let (pan, tilt) = nudge_offset(direction, step_degrees);
    camera.pan_tilt_relative(
        Degrees(pan as f32),
        Degrees(tilt as f32),
        SpeedLevel::Medium,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nudge_offset_moves_both_axes_for_diagonals() {
        assert_eq!(nudge_offset(NudgeDirection::UpRight, 2.0), (2.0, 2.0));
        assert_eq!(nudge_offset(NudgeDirection::DownLeft, 2.0), (-2.0, -2.0));
    }

    #[test]
    fn nudge_offset_moves_a_single_axis_for_cardinal_directions() {
        assert_eq!(nudge_offset(NudgeDirection::Up, 2.0), (0.0, 2.0));
        assert_eq!(nudge_offset(NudgeDirection::Left, 2.0), (-2.0, 0.0));
    }

    #[test]
    fn nudge_offset_scales_with_step_size() {
        assert_eq!(
            nudge_offset(NudgeDirection::Right, FAST_NUDGE_DEGREES),
            (FAST_NUDGE_DEGREES, 0.0)
        );
    }

    #[test]
    fn nudge_pan_tilt_sends_a_relative_move_command() {
        use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

        let transport = ScriptedBlockingTransport::new(vec![helpers::standard_command_response(1)]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        nudge_pan_tilt(&camera, NudgeDirection::UpRight, NUDGE_DEGREES)
            .expect("scripted camera should ack and complete the move");
    }
}
