//! Camera power.
//!
//! "Off" here is VISCA's own idea of off — standby, not disconnected. A
//! camera in standby stops imaging and parks its lens, but keeps listening,
//! which is what makes powering it back on possible over the same link.

use grafton_visca::{
    BlockingClient, CameraId, Error,
    camera::profiles::GenericVisca,
    command::ViscaCommand,
    timeout::CommandCategory,
    transport::{BlockingTransport, HasTransportConfig},
};

/// `CAM_Power On`, encoded here rather than taken from grafton-visca, for the
/// sake of how long it is given to answer.
///
/// The library files power with its "quick" commands and stops waiting after
/// five seconds. Going to sleep is that quick; waking up is not. The camera
/// answers this command's completion once it has actually powered up — an
/// EVI-D80 gets there in about nine seconds, and its manual allows sixteen —
/// so five seconds meant reporting a failure for a camera that was coming up
/// perfectly well, and leaving the lamp dark for one that was already on.
struct PowerOn;

impl ViscaCommand for PowerOn {
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
        buffer[..6].copy_from_slice(&[camera_id.to_address_byte(), 0x01, 0x04, 0x00, 0x02, 0xFF]);
        Ok(Self::MAX_SIZE)
    }
}

/// Powers the camera on, or puts it into standby.
///
/// Waking is the slow half and is left to run to its own end: the completion
/// that arrives seconds later is the camera saying it is ready, which is
/// worth more than an early answer that only says the message was heard.
pub fn set_power<T>(camera: &BlockingClient<GenericVisca, T>, on: bool) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    if on {
        camera.execute(PowerOn)
    } else {
        camera.power_off()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

    fn scripted_camera() -> (
        BlockingClient<GenericVisca, ScriptedBlockingTransport>,
        ScriptedBlockingTransport,
    ) {
        let transport = ScriptedBlockingTransport::new(vec![helpers::standard_command_response(1)]);
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport.clone())
            .expect("camera should build from a scripted transport");
        (camera, transport)
    }

    #[test]
    fn set_power_switches_the_camera_both_ways() {
        let (camera, transport) = scripted_camera();
        set_power(&camera, true).expect("scripted camera should ack power on");
        assert_eq!(
            transport.sent(),
            vec![vec![0x81, 0x01, 0x04, 0x00, 0x02, 0xFF]]
        );

        let (camera, transport) = scripted_camera();
        set_power(&camera, false).expect("scripted camera should ack power off");
        assert_eq!(
            transport.sent(),
            vec![vec![0x81, 0x01, 0x04, 0x00, 0x03, 0xFF]]
        );
    }

    #[test]
    fn powering_on_waits_longer_than_a_quick_command_would() {
        assert!(
            PowerOn::TIMEOUT_CATEGORY.default_timeout() > CommandCategory::Quick.default_timeout(),
            "the camera answers once it has woken, which takes longer than that"
        );
    }
}
