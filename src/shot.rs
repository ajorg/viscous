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

use crate::{
    pan_tilt::{MAX_PAN_SPEED, MAX_TILT_SPEED},
    state::Position,
};

/// How fast to travel to a shot, on each axis.
///
/// Two speeds rather than one because pan and tilt are separate motors with
/// separate ranges, and because a move with far to pan and little to tilt
/// wants them different: at a single speed the tilt finishes early and the
/// shot hooks into its final framing instead of arriving at it — see
/// [`proportional`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Travel {
    /// How fast to pan, in VISCA's `1..=`[`crate::pan_tilt::MAX_PAN_SPEED`].
    pub pan_speed: u8,
    /// How fast to tilt, in VISCA's `1..=`[`crate::pan_tilt::MAX_TILT_SPEED`].
    pub tilt_speed: u8,
}

/// A deliberate, unhurried pace for arriving at a shot on purpose, rather than
/// snapping to it the way the camera's own preset recall does.
///
/// Measured against a real EVI-D80: settled and repeatable on both axes,
/// clearly slower than the camera's fastest gears without being so slow that
/// waiting for a shot feels like the wrong call.
pub const DELIBERATE: Travel = Travel {
    pan_speed: 4,
    tilt_speed: 4,
};

/// Speeds proportional to how far each axis actually has to travel from
/// `current` to `target`, so a shot that's mostly a pan and barely a tilt
/// doesn't have the tilt arrive early and leave the rest of the pan looking
/// like a straight lateral tacked onto a diagonal — [`DELIBERATE`] alone
/// can't do this, since it sends both axes at the same speed regardless of
/// distance.
///
/// The axis with farther to go travels at `DELIBERATE`'s speed for that axis;
/// the other is slowed in direct proportion to how much less distance it has
/// to cover. That's a linear approximation of the camera's true speed — the
/// real relationship between a VISCA speed number and how fast the head
/// actually moves is measurably *not* linear — so this doesn't perfectly
/// synchronise the two axes' arrival, but it is far closer than sending them
/// at identical speeds no matter the distance, which is what it replaces.
pub fn proportional(current: Position, target: Position) -> Travel {
    let pan_distance = target.pan.abs_diff(current.pan);
    let tilt_distance = target.tilt.abs_diff(current.tilt);
    let farthest = pan_distance.max(tilt_distance);
    if farthest == 0 {
        return DELIBERATE;
    }
    Travel {
        pan_speed: scaled_speed(pan_distance, farthest, DELIBERATE.pan_speed, MAX_PAN_SPEED),
        tilt_speed: scaled_speed(tilt_distance, farthest, DELIBERATE.tilt_speed, MAX_TILT_SPEED),
    }
}

/// `reference` at `farthest`, scaled down for a shorter `distance`, and never
/// below 1 — VISCA has no speed of zero, and an axis with nowhere to go still
/// has to carry some value on the wire even though it does nothing with it.
fn scaled_speed(distance: u16, farthest: u16, reference: u8, max_speed: u8) -> u8 {
    let speed = u32::from(reference) * u32::from(distance) / u32::from(farthest);
    u8::try_from(speed).unwrap_or(max_speed).clamp(1, max_speed)
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
    fn the_farther_axis_travels_at_the_deliberate_speed() {
        let travel = proportional(
            Position::default(),
            Position {
                pan: 120,
                tilt: 45,
                ..Position::default()
            },
        );

        assert_eq!(travel.pan_speed, DELIBERATE.pan_speed, "pan is the farther axis");
    }

    #[test]
    fn the_nearer_axis_is_slowed_in_proportion_to_the_farther_ones_distance() {
        let travel = proportional(
            Position::default(),
            Position {
                pan: 120,
                tilt: 60,
                ..Position::default()
            },
        );

        assert_eq!(
            travel.tilt_speed,
            DELIBERATE.tilt_speed / 2,
            "half the distance should ask for half the speed"
        );
    }

    #[test]
    fn a_move_with_nothing_to_do_on_one_axis_still_gets_a_valid_speed_for_it() {
        let travel = proportional(
            Position::default(),
            Position {
                pan: 120,
                tilt: 0,
                ..Position::default()
            },
        );

        assert_eq!(
            travel.tilt_speed, 1,
            "VISCA has no such thing as a speed of zero"
        );
    }

    #[test]
    fn already_standing_on_the_target_travels_at_the_deliberate_speed() {
        assert_eq!(proportional(somewhere(), somewhere()), DELIBERATE);
    }

    #[test]
    fn an_equal_move_on_both_axes_travels_at_the_same_speed_on_both() {
        let travel = proportional(
            Position::default(),
            Position {
                pan: 80,
                tilt: -80,
                ..Position::default()
            },
        );

        assert_eq!(travel.pan_speed, travel.tilt_speed);
        assert_eq!(travel.pan_speed, DELIBERATE.pan_speed);
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
