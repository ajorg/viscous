//! Single steps of the pan/tilt head, for aim that a held control can't give.
//!
//! Everything else here is a velocity: a control asks for a speed and the
//! camera holds it until told to stop, so how far the shot travels is however
//! long a hand stayed on the control. That is the right shape for framing a
//! moving subject and the wrong one for "a little to the left" — the slowest
//! speed the camera accepts still crosses more than a hair of the frame in the
//! time it takes to notice it has started.
//!
//! A step is the other thing VISCA offers: a distance rather than a speed.
//! The camera is told to move exactly this far from wherever it is, and it
//! stops there by itself. One tap is one step, and ten taps is exactly ten
//! steps — repeatable in a way that ten taps on a drive control can never be.
//!
//! Note this is the movement model the whole program used until `f3ce77e` and
//! deliberately left behind. Nothing here contradicts that: a step per keypress
//! made a *held* control queue up hops that all had to play out after release,
//! because each one only reports finishing once it physically has. Taps are
//! what a step was always for, and a distance is the one thing that can be
//! added up — see [`Step`], which is what keeps that queue from forming again.

use std::ops::Add;

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::{PanTilt, PanTiltDirection},
    transport::{BlockingTransport, HasTransportConfig},
    types::{PanSpeed, TiltSpeed},
};

/// How far one tap moves an axis, in the camera's own position units.
///
/// One unit is as fine as the head is addressable — there is no half unit to
/// ask for. Whether the head actually honours a step this small is a fact
/// about the gearing rather than the protocol, and it can only be settled by
/// watching a real one: a head with backlash swallows the first step after a
/// change of direction and then moves two at once. If that turns out to be
/// this camera, this is the number to raise, and the symptom to raise it over
/// is a first tap that does nothing.
pub const STEP_UNITS: i16 = 1;

/// How fast a step travels.
///
/// The slowest the camera accepts, which for a step is nothing but upside: the
/// distance is already decided, so speed only decides how visible the move is
/// on its way there. A step taken at a framing speed reads as a twitch; the
/// same step at the slowest speed reads as the shot settling.
const STEP_SPEED: u8 = 1;

/// How far to move, in the camera's own position units, as an offset from
/// wherever it is pointing now.
///
/// A distance rather than a direction, which is the property the whole design
/// rests on: **two steps add up into one**. Three taps to the right while the
/// camera is busy answering the first are not three commands to be queued and
/// played out one after another — that was the failure that took relative
/// moves out of this program once — nor three taps of which two get dropped,
/// which would make a tap something you can't count on. They are one move of
/// three units, which arrives in the same place, in one round trip, without
/// stopping twice on the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Step {
    pub pan: i16,
    pub tilt: i16,
}

impl Add for Step {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            // Saturating because a hand cannot tap 32,768 times, and a wrap
            // would send the camera the other way if one ever did.
            pan: self.pan.saturating_add(other.pan),
            tilt: self.tilt.saturating_add(other.tilt),
        }
    }
}

impl Step {
    /// Going nowhere: what a run of taps that cancelled each other out comes
    /// to, and what an empty sum starts from.
    pub const STILL: Self = Self { pan: 0, tilt: 0 };

    /// One step in `direction`.
    ///
    /// Diagonals move both axes by a full step rather than by a diagonal's
    /// share of one: a step is the smallest thing the camera can do, so there
    /// is nothing smaller to scale down to.
    pub fn towards(direction: PanTiltDirection) -> Self {
        let (pan, tilt) = match direction {
            PanTiltDirection::Stop => (0, 0),
            PanTiltDirection::Up => (0, 1),
            PanTiltDirection::Down => (0, -1),
            PanTiltDirection::Left => (-1, 0),
            PanTiltDirection::Right => (1, 0),
            PanTiltDirection::UpLeft => (-1, 1),
            PanTiltDirection::UpRight => (1, 1),
            PanTiltDirection::DownLeft => (-1, -1),
            PanTiltDirection::DownRight => (1, -1),
        };
        Self {
            pan: pan * STEP_UNITS,
            tilt: tilt * STEP_UNITS,
        }
    }

    /// Whether this step goes nowhere and so is not worth a command.
    pub fn is_still(self) -> bool {
        self == Self::STILL
    }

    /// Which way this step goes, for saying so in words.
    pub fn direction(self) -> PanTiltDirection {
        match (self.pan.signum(), self.tilt.signum()) {
            (0, 0) => PanTiltDirection::Stop,
            (0, 1) => PanTiltDirection::Up,
            (0, _) => PanTiltDirection::Down,
            (1, 0) => PanTiltDirection::Right,
            (-1, 0) => PanTiltDirection::Left,
            (1, 1) => PanTiltDirection::UpRight,
            (1, _) => PanTiltDirection::DownRight,
            (_, 1) => PanTiltDirection::UpLeft,
            (_, _) => PanTiltDirection::DownLeft,
        }
    }
}

/// Moves the camera by `step`, or does nothing at all if it goes nowhere.
///
/// Sent in the camera's own units rather than in degrees, which is not a
/// micro-optimisation: grafton-visca converts degrees using the *profile's*
/// units-per-degree, and the profile this program connects with is
/// `GenericVisca`, whose figure is a conservative stand-in for cameras it has
/// never met. A step asked for in degrees would therefore be off by whatever
/// that guess misses this camera by, silently and on every step. Units have
/// nothing to convert and so nothing to get wrong.
pub fn nudge<T>(camera: &BlockingClient<GenericVisca, T>, step: Step) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    if step.is_still() {
        return Ok(());
    }
    camera.execute(PanTilt::RelativePositionRaw {
        // Two's complement, which is how VISCA reads a relative move's
        // position: the command carries a signed distance in a field the
        // library types as unsigned.
        pan_u16: step.pan as u16,
        tilt_u16: step.tilt as u16,
        pan_speed: PanSpeed::new(STEP_SPEED)?,
        tilt_speed: TiltSpeed::new(STEP_SPEED)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, Step as Scripted, helpers};

    /// A camera that answers one command, and a handle on what it was sent.
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
    fn a_step_goes_out_as_a_relative_move_of_one_unit_at_the_slowest_speed() {
        let (camera, transport) = scripted_camera();

        nudge(&camera, Step::towards(PanTiltDirection::Right))
            .expect("the camera should take a step");

        // 81 01 06 03 VV WW 0P 0P 0P 0P 0T 0T 0T 0T FF: a relative move, pan
        // and tilt speed both 1, pan +1 and tilt 0 spread over four nibbles
        // each.
        assert_eq!(
            transport.sent(),
            vec![vec![
                0x81, 0x01, 0x06, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
                0xFF
            ]]
        );
    }

    #[test]
    fn a_step_the_other_way_is_the_same_distance_backwards() {
        let (camera, transport) = scripted_camera();

        nudge(&camera, Step::towards(PanTiltDirection::Left))
            .expect("the camera should take a step");

        // -1 in two's complement is 0xFFFF, which is 0F 0F 0F 0F on the wire.
        assert_eq!(transport.sent()[0][6..10], [0x0F, 0x0F, 0x0F, 0x0F]);
    }

    #[test]
    fn steps_the_same_way_add_up_into_one_longer_move() {
        // The property the worker relies on to keep a run of taps from ever
        // becoming a queue.
        let three = Step::towards(PanTiltDirection::Right)
            + Step::towards(PanTiltDirection::Right)
            + Step::towards(PanTiltDirection::Right);

        assert_eq!(three.pan, 3 * STEP_UNITS);
        assert_eq!(three.direction(), PanTiltDirection::Right);
    }

    #[test]
    fn steps_that_undo_each_other_come_to_nothing() {
        let there_and_back =
            Step::towards(PanTiltDirection::Right) + Step::towards(PanTiltDirection::Left);

        assert!(there_and_back.is_still());
    }

    #[test]
    fn steps_on_different_axes_make_a_diagonal() {
        let corner = Step::towards(PanTiltDirection::Right) + Step::towards(PanTiltDirection::Up);

        assert_eq!(corner, Step::towards(PanTiltDirection::UpRight));
        assert_eq!(corner.direction(), PanTiltDirection::UpRight);
    }

    #[test]
    fn a_diagonal_step_moves_both_axes() {
        assert_eq!(
            Step::towards(PanTiltDirection::UpRight),
            Step {
                pan: STEP_UNITS,
                tilt: STEP_UNITS
            }
        );
        assert_eq!(
            Step::towards(PanTiltDirection::DownLeft),
            Step {
                pan: -STEP_UNITS,
                tilt: -STEP_UNITS
            }
        );
    }

    #[test]
    fn every_direction_names_itself_again_from_its_offsets() {
        for direction in [
            PanTiltDirection::Up,
            PanTiltDirection::Down,
            PanTiltDirection::Left,
            PanTiltDirection::Right,
            PanTiltDirection::UpLeft,
            PanTiltDirection::UpRight,
            PanTiltDirection::DownLeft,
            PanTiltDirection::DownRight,
        ] {
            assert_eq!(Step::towards(direction).direction(), direction);
        }
    }

    #[test]
    fn a_step_is_as_small_as_the_camera_can_be_asked_for() {
        // The point of a step, and the thing a future session would otherwise
        // be tempted to "tidy" into a rounder number.
        assert_eq!(STEP_UNITS, 1);
        assert_eq!(STEP_SPEED, 1);
    }

    #[test]
    fn a_step_nowhere_is_not_sent_at_all() {
        let transport = ScriptedBlockingTransport::new(Vec::<Scripted>::new());
        let camera = grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(transport.clone())
            .expect("camera should build from a scripted transport");

        nudge(&camera, Step::STILL).expect("a step nowhere should be no work");

        assert!(
            transport.sent().is_empty(),
            "a step going nowhere should cost the camera nothing"
        );
    }
}
