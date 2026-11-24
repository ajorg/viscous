//! Camera power.
//!
//! "Off" here is VISCA's own idea of off — standby, not disconnected. A
//! camera in standby stops imaging and parks its lens, but keeps listening,
//! which is what makes powering it back on possible over the same link.

use grafton_visca::{CameraId, Error, command::ViscaCommand, timeout::CommandCategory};

/// `CAM_Power`: on, or standby. Encoded here rather than taken from
/// grafton-visca, for the sake of how long it is given to answer.
///
/// The library files power with its "quick" commands and stops waiting after
/// five seconds. Going to sleep is that quick; waking up is not. The camera
/// answers this command's completion once it has actually powered up — an
/// EVI-D80 gets there in about nine seconds, and its manual allows sixteen —
/// so five seconds meant reporting a failure for a camera that was coming up
/// perfectly well, and leaving the lamp dark for one that was already on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Power(pub bool);

impl ViscaCommand for Power {
    const MAX_SIZE: usize = 6;

    /// Borrowed from the movement commands: thirty seconds is the shortest of
    /// the library's categories that a camera can finish waking inside.
    const TIMEOUT_CATEGORY: CommandCategory = CommandCategory::Movement;

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() < Self::MAX_SIZE {
            return Err(Error::BufferTooSmall {
                required: Self::MAX_SIZE,
                actual: buffer.len(),
            });
        }
        let state = if self.0 { 0x02 } else { 0x03 };
        buffer[..6].copy_from_slice(&[camera_id.to_address_byte(), 0x01, 0x04, 0x00, state, 0xFF]);
        Ok(Self::MAX_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::encoded;

    #[test]
    fn power_switches_the_camera_both_ways() {
        assert_eq!(encoded(&Power(true)), [0x81, 0x01, 0x04, 0x00, 0x02, 0xFF]);
        assert_eq!(encoded(&Power(false)), [0x81, 0x01, 0x04, 0x00, 0x03, 0xFF]);
    }

    #[test]
    fn powering_on_waits_longer_than_a_quick_command_would() {
        assert!(
            Power::TIMEOUT_CATEGORY.default_timeout() > CommandCategory::Quick.default_timeout(),
            "the camera answers once it has woken, which takes longer than that"
        );
    }
}
