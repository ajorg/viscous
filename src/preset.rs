//! Camera preset recall and save, addressed by the 1-based preset number an
//! operator sees on screen rather than VISCA's own 0-based numbering.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::PresetNumber,
    transport::{BlockingTransport, HasTransportConfig},
};

/// Converts a 1-based, on-screen preset number into the 0-based
/// `PresetNumber` VISCA itself uses.
///
/// This only guards against the number being too low to convert (zero);
/// how many presets the camera actually has is validated by the camera
/// itself when the command is sent, so it isn't duplicated here.
fn preset_number(displayed: u8) -> Result<PresetNumber, Error> {
    let index = displayed.checked_sub(1).ok_or(Error::ParameterOutOfRange {
        parameter: "preset",
        value: i32::from(displayed),
        min: 1,
        max: i32::from(u8::MAX),
    })?;
    PresetNumber::new(index)
}

/// Moves the camera to a previously saved preset position.
pub fn recall_preset<T>(
    camera: &BlockingClient<GenericVisca, T>,
    displayed: u8,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.preset_recall(preset_number(displayed)?)
}

/// Saves the camera's current position to a preset slot, overwriting
/// whatever was there before.
pub fn save_preset<T>(camera: &BlockingClient<GenericVisca, T>, displayed: u8) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.preset_set(preset_number(displayed)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_number_converts_from_one_based_display() {
        assert_eq!(preset_number(1).unwrap().value(), 0);
        assert_eq!(preset_number(6).unwrap().value(), 5);
    }

    #[test]
    fn preset_number_rejects_zero() {
        assert!(matches!(
            preset_number(0),
            Err(Error::ParameterOutOfRange {
                value: 0,
                min: 1,
                ..
            })
        ));
    }

    #[test]
    fn recall_preset_sends_a_recall_command() {
        use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

        let transport = ScriptedBlockingTransport::new(vec![helpers::standard_command_response(1)]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        recall_preset(&camera, 3).expect("scripted camera should ack and complete the recall");
    }

    #[test]
    fn save_preset_sends_a_set_command() {
        use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

        let transport = ScriptedBlockingTransport::new(vec![helpers::standard_command_response(1)]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport)
            .expect("camera should build from a scripted transport");

        save_preset(&camera, 3).expect("scripted camera should ack and complete the save");
    }
}
