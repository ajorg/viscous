//! Camera power.
//!
//! "Off" here is VISCA's own idea of off — standby, not disconnected. A
//! camera in standby stops imaging and parks its lens, but keeps listening,
//! which is what makes powering it back on possible over the same link.

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    transport::{BlockingTransport, HasTransportConfig},
};

/// Powers the camera on, or puts it into standby.
pub fn set_power<T>(camera: &BlockingClient<GenericVisca, T>, on: bool) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    if on {
        camera.power_on()
    } else {
        camera.power_off()
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
    fn set_power_switches_the_camera_both_ways() {
        set_power(&scripted_camera(), true).expect("scripted camera should ack power on");
        set_power(&scripted_camera(), false).expect("scripted camera should ack power off");
    }
}
