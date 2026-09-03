//! Going to a remembered position at a speed of our own choosing.
//!
//! The camera's own preset recall takes no speed. `CAM_Memory Recall` carries
//! a preset number and nothing else — seven bytes, with nowhere to put one —
//! so the camera travels at whatever rate it likes, and on this one that rate
//! is "fast". `Pan-tiltPosition Absolute Position` does take a speed on each
//! axis, so a position remembered here (see [`crate::state::Position`], filled
//! in by [`crate::worker`] as the camera arrives) can be gone to slowly and
//! deliberately in a way a preset never can.
//!
//! Only pan and tilt so far. A remembered position knows its zoom and focus
//! too, and both have direct-position commands of their own, but neither
//! carries a speed and each is a separate command that blocks until it has
//! finished — so restoring all four in the order they'd have to be sent would
//! arrive at the framing and *then* zoom into it. Getting that right means
//! overlapping the commands across VISCA's two sockets, which is its own
//! piece of work.

use grafton_visca::{
    BlockingClient, CameraId, Error,
    camera::profiles::GenericVisca,
    command::ViscaCommand,
    timeout::CommandCategory,
    transport::{BlockingTransport, HasTransportConfig},
    types::{PanSpeed, TiltSpeed},
};

use crate::state::Position;

/// How fast to travel to a shot, on each axis.
///
/// Two speeds rather than one because pan and tilt are separate motors with
/// separate ranges, and because a move with far to pan and little to tilt
/// wants them different: at a single speed the tilt finishes early and the
/// shot hooks into its final framing instead of arriving at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Travel {
    /// How fast to pan, in VISCA's `1..=`[`crate::pan_tilt::MAX_PAN_SPEED`].
    pub pan_speed: u8,
    /// How fast to tilt, in VISCA's `1..=`[`crate::pan_tilt::MAX_TILT_SPEED`].
    pub tilt_speed: u8,
}

/// `Pan-tiltPosition Absolute Position`, encoded here rather than taken from
/// grafton-visca, for the sake of how long it is given to answer.
///
/// The library files every pan/tilt command under movement and stops waiting
/// after thirty seconds. That is generous for a drive command, which the
/// camera completes as soon as it has *started* moving, and short for this
/// one, which completes only once the camera has physically arrived. Going
/// somewhere slowly is the whole point here, so the two want different
/// deadlines despite sharing an opcode family.
///
/// Getting it wrong is worse than a spurious error message: a deadline passing
/// doesn't end the attempt, it makes grafton's scheduler *resend*. A move that
/// outran its deadline would be reissued while the camera was still travelling
/// — the same fault `42eb123` fixed for power-on, and far easier to trigger
/// here.
///
/// Known bound: sixty seconds is the longest of the library's categories, and
/// a full-range move at the very slowest speed can still outrun it. Raising it
/// past that means overriding the timeout for the whole connection rather than
/// for one command, which is a bigger change than this needs.
struct AbsolutePosition {
    position: Position,
    pan_speed: PanSpeed,
    tilt_speed: TiltSpeed,
}

impl ViscaCommand for AbsolutePosition {
    const MAX_SIZE: usize = 15;

    /// Borrowed from the preset commands, which wait on exactly the same
    /// thing: a camera that answers once it has finished travelling.
    const TIMEOUT_CATEGORY: CommandCategory = CommandCategory::Preset;

    fn write_into(&self, camera_id: CameraId, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() < Self::MAX_SIZE {
            return Err(Error::BufferTooSmall {
                required: Self::MAX_SIZE,
                actual: buffer.len(),
            });
        }
        buffer[..6].copy_from_slice(&[
            camera_id.to_address_byte(),
            0x01,
            0x06,
            0x02,
            self.pan_speed.value(),
            self.tilt_speed.value(),
        ]);
        // Two's complement, which is how VISCA reads a position: the command
        // carries a signed coordinate in a field of unsigned nibbles.
        buffer[6..10].copy_from_slice(&nibbles(self.position.pan as u16));
        buffer[10..14].copy_from_slice(&nibbles(self.position.tilt as u16));
        buffer[14] = 0xFF;
        Ok(Self::MAX_SIZE)
    }
}

/// Splits `value` into VISCA's four nibbles, most significant first.
fn nibbles(value: u16) -> [u8; 4] {
    [
        (value >> 12) as u8 & 0x0F,
        (value >> 8) as u8 & 0x0F,
        (value >> 4) as u8 & 0x0F,
        value as u8 & 0x0F,
    ]
}

/// Sends the camera to `shot`, arriving under its own power at `travel`'s
/// speeds, and answers once it is standing there.
///
/// Only the pan and tilt of `shot` are used; see the module docs.
pub fn go_to<T>(
    camera: &BlockingClient<GenericVisca, T>,
    shot: Position,
    travel: Travel,
) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.execute(AbsolutePosition {
        position: shot,
        pan_speed: PanSpeed::new(travel.pan_speed)?,
        tilt_speed: TiltSpeed::new(travel.tilt_speed)?,
    })
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

    fn somewhere() -> Position {
        Position {
            pan: 0x0123,
            tilt: 0x0456,
            zoom: 0x1000,
            focus: 0x2000,
        }
    }

    #[test]
    fn going_to_a_shot_asks_for_its_coordinates_at_the_speed_given() {
        let (camera, transport) = scripted_camera();

        go_to(
            &camera,
            somewhere(),
            Travel {
                pan_speed: 5,
                tilt_speed: 3,
            },
        )
        .expect("scripted camera should ack and complete the move");

        assert_eq!(
            transport.sent(),
            vec![vec![
                0x81, 0x01, 0x06, 0x02, 0x05, 0x03, // speeds
                0x00, 0x01, 0x02, 0x03, // pan
                0x00, 0x04, 0x05, 0x06, // tilt
                0xFF,
            ]]
        );
    }

    #[test]
    fn a_shot_left_of_centre_travels_there_rather_than_the_long_way_round() {
        let (camera, transport) = scripted_camera();

        go_to(
            &camera,
            Position {
                pan: -1,
                tilt: -2,
                ..somewhere()
            },
            Travel {
                pan_speed: 1,
                tilt_speed: 1,
            },
        )
        .expect("scripted camera should ack and complete the move");

        let sent = transport.sent();
        assert_eq!(
            &sent[0][6..14],
            &[0x0F, 0x0F, 0x0F, 0x0F, 0x0F, 0x0F, 0x0F, 0x0E],
            "negative coordinates go on the wire as two's complement"
        );
    }

    #[test]
    fn a_speed_the_camera_has_no_such_gear_for_is_refused_before_it_is_sent() {
        let (camera, transport) = scripted_camera();

        let too_fast = go_to(
            &camera,
            somewhere(),
            Travel {
                pan_speed: crate::pan_tilt::MAX_PAN_SPEED + 1,
                tilt_speed: 1,
            },
        );

        assert!(too_fast.is_err());
        assert!(
            transport.sent().is_empty(),
            "nothing should reach the camera"
        );
    }

    #[test]
    fn arriving_somewhere_is_given_longer_than_setting_off_would_be() {
        // The distinction that matters: a drive command completes as soon as
        // the camera starts moving, this one only once it has arrived. Sharing
        // the drive's deadline would have the scheduler resend a slow move
        // out from under itself.
        assert!(
            AbsolutePosition::TIMEOUT_CATEGORY.default_timeout()
                > CommandCategory::Movement.default_timeout(),
            "a slow move must not outrun its deadline and be reissued"
        );
    }
}
