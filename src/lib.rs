//! Core logic for the viscous VISCA camera control TUI.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::VersionInfo,
    transport::{BlockingTransport, HasTransportConfig},
    types::SpeedLevel,
    units::Degrees,
};

/// Candidate baud rates to try when a camera's serial configuration isn't
/// known ahead of time, most likely first.
///
/// The EVI-D70's RS-232C VISCA interface is fixed at 9600 baud, but other
/// VISCA-compatible cameras support configurable rates up to 38400.
pub const DEFAULT_CAMERA_BAUD_RATES: &[u32] = &[9600, 38400];

/// The result of probing a camera across one or more candidate baud rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// A candidate baud rate produced a valid version-inquiry reply.
    Connected {
        /// The baud rate that produced a response.
        baud_rate: u32,
        /// The camera's parsed version-inquiry reply.
        version: VersionInfo,
    },
    /// None of the candidate baud rates produced a response.
    NoResponse,
}

/// Queries a connected camera for its VISCA version-inquiry reply.
pub fn query_version<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<VersionInfo, Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.system().version()
}

/// Tries each candidate baud rate in order, returning the first one whose
/// connection attempt succeeds.
///
/// `attempt` performs one connection and version-inquiry at the given baud
/// rate. A wrong baud rate is expected to surface as an `Err` (typically a
/// timeout, since the camera's replies won't frame correctly) rather than a
/// failure to open the port, so any error simply moves on to the next
/// candidate.
pub fn discover_baud_rate(
    candidates: &[u32],
    mut attempt: impl FnMut(u32) -> Result<VersionInfo, Error>,
) -> ProbeOutcome {
    for &baud_rate in candidates {
        if let Ok(version) = attempt(baud_rate) {
            return ProbeOutcome::Connected { baud_rate, version };
        }
    }
    ProbeOutcome::NoResponse
}

/// Formats a camera's version-inquiry reply for display.
pub fn format_version(info: &VersionInfo) -> String {
    format!(
        "vendor=0x{:04X} model=0x{:04X} rom=0x{:08X} max_socket={}",
        info.vendor, info.model, info.rom_version, info.max_socket
    )
}

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

    fn sample_version() -> VersionInfo {
        VersionInfo {
            vendor: 0x0001,
            model: 0x0002,
            rom_version: 0x0304,
            max_socket: 2,
        }
    }

    #[test]
    fn format_version_renders_fields_as_hex() {
        assert_eq!(
            format_version(&sample_version()),
            "vendor=0x0001 model=0x0002 rom=0x00000304 max_socket=2"
        );
    }

    #[test]
    fn discover_baud_rate_returns_first_successful_candidate() {
        let outcome = discover_baud_rate(&[9600, 38400], |baud_rate| {
            if baud_rate == 9600 {
                Ok(sample_version())
            } else {
                panic!("should not try a second candidate after the first succeeds")
            }
        });

        assert_eq!(
            outcome,
            ProbeOutcome::Connected {
                baud_rate: 9600,
                version: sample_version(),
            }
        );
    }

    #[test]
    fn discover_baud_rate_falls_back_to_next_candidate_on_error() {
        let outcome = discover_baud_rate(&[9600, 38400], |baud_rate| {
            if baud_rate == 9600 {
                Err(Error::Timeout)
            } else {
                Ok(sample_version())
            }
        });

        assert_eq!(
            outcome,
            ProbeOutcome::Connected {
                baud_rate: 38400,
                version: sample_version(),
            }
        );
    }

    #[test]
    fn discover_baud_rate_reports_no_response_when_all_candidates_fail() {
        let outcome = discover_baud_rate(&[9600, 38400], |_| Err(Error::Timeout));

        assert_eq!(outcome, ProbeOutcome::NoResponse);
    }

    #[test]
    fn query_version_reads_a_scripted_camera_reply() {
        use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

        let request = vec![0x81, 0x09, 0x00, 0x02, 0xFF];
        let response = vec![0x90, 0x50, 0x00, 0x01, 0x00, 0x02, 0x03, 0x04, 0x02, 0xFF];
        let transport =
            ScriptedBlockingTransport::new(vec![helpers::inquiry_response(request, 1, response)]);

        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        let version = query_version(&camera).expect("scripted camera should reply");

        assert_eq!(version, sample_version());
    }

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
