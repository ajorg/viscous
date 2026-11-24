//! Camera preset recall and save, addressed by the 1-based preset number an
//! operator sees on screen rather than VISCA's own 0-based numbering.

use grafton_visca::{CameraId, Error, command::ViscaCommand, timeout::CommandCategory};

/// What to do with a preset slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Store where the camera is pointing now.
    Save,
    /// Go back to what was stored.
    Recall,
}

/// `CAM_Memory`: save or recall one preset slot.
///
/// Encoded here because grafton-visca keeps its own version of this command
/// private, reachable only through calls that send it straight away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    action: Action,
    /// VISCA's 0-based slot number.
    slot: u8,
}

impl Memory {
    /// The command for the preset shown on screen as `displayed`, counting
    /// from 1.
    ///
    /// This only guards against the number being too low to convert (zero);
    /// how many presets the camera actually has is validated by the camera
    /// itself when the command is sent, so it isn't duplicated here.
    pub fn new(action: Action, displayed: u8) -> Result<Self, Error> {
        let slot = displayed.checked_sub(1).ok_or(Error::ParameterOutOfRange {
            parameter: "preset",
            value: i32::from(displayed),
            min: 1,
            max: i32::from(u8::MAX),
        })?;
        Ok(Self { action, slot })
    }
}

impl ViscaCommand for Memory {
    const MAX_SIZE: usize = 7;

    /// A recall answers once the head has arrived, which across the room is
    /// a long trip.
    const TIMEOUT_CATEGORY: CommandCategory = CommandCategory::Preset;

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() < Self::MAX_SIZE {
            return Err(Error::BufferTooSmall {
                required: Self::MAX_SIZE,
                actual: buffer.len(),
            });
        }
        let action = match self.action {
            Action::Save => 0x01,
            Action::Recall => 0x02,
        };
        buffer[..7].copy_from_slice(&[
            camera_id.to_address_byte(),
            0x01,
            0x04,
            0x3F,
            action,
            self.slot,
            0xFF,
        ]);
        Ok(Self::MAX_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::encoded;

    #[test]
    fn the_preset_on_screen_is_the_slot_before_it_in_visca() {
        assert_eq!(
            encoded(&Memory::new(Action::Recall, 1).unwrap()),
            [0x81, 0x01, 0x04, 0x3F, 0x02, 0x00, 0xFF]
        );
        assert_eq!(
            encoded(&Memory::new(Action::Save, 6).unwrap()),
            [0x81, 0x01, 0x04, 0x3F, 0x01, 0x05, 0xFF]
        );
    }

    #[test]
    fn there_is_no_preset_zero_on_screen() {
        assert!(matches!(
            Memory::new(Action::Recall, 0),
            Err(Error::ParameterOutOfRange {
                value: 0,
                min: 1,
                ..
            })
        ));
    }
}
