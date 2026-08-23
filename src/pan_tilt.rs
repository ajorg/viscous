//! Continuous pan/tilt drive.
//!
//! Movement is expressed as a velocity the camera holds until it's told
//! otherwise, not as a series of discrete relative moves. A relative move
//! accelerates, travels its distance and decelerates, and its completion
//! only comes back once it has physically finished — so a control held down
//! turns into a queue of separate hops, each of which has to play out in
//! full even after the user has let go. One drive command and one stop
//! command produce the same sweep as a single smooth motion, with nothing
//! left queued behind them.

use std::ops::RangeInclusive;

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::PanTiltDirection,
    transport::{BlockingTransport, HasTransportConfig},
    types::{PanSpeed, TiltSpeed},
};

use crate::deflection;

/// The slowest speed VISCA's drive command accepts on either axis. Zero
/// isn't "stopped" — the command rejects it outright — so this is as slow as
/// movement gets, and stopping is [`Velocity::STOP`]'s job.
const MIN_SPEED: u8 = 1;

/// The fastest pan speed VISCA's drive command accepts (0x18).
pub const MAX_PAN_SPEED: u8 = 24;

/// The fastest tilt speed VISCA's drive command accepts (0x14 — genuinely a
/// shorter range than pan's).
pub const MAX_TILT_SPEED: u8 = 20;

/// The pan speeds a control will actually ask for, slowest first.
///
/// Deliberately short of what the camera can do. The top of VISCA's range is a
/// speed for slewing somewhere, not for pointing at anything: it swings past a
/// subject before a hand can stop it, and the picture arrives as a blur either
/// way. Held to half of that, every position on a control is a speed worth
/// driving at, and the ones in the middle are the ones a shot is framed with.
///
/// Nothing is lost by giving the top up: getting somewhere far away in a hurry
/// is what the presets and [`home`] are for, and they arrive at the camera's
/// own pace without anyone having to hold a stick over.
pub const PAN_SPEEDS: RangeInclusive<u8> = MIN_SPEED..=MAX_PAN_SPEED / 2;

/// The tilt speeds a control will actually ask for, slowest first. Half of
/// what the camera accepts, for the reasons [`PAN_SPEEDS`] gives.
pub const TILT_SPEEDS: RangeInclusive<u8> = MIN_SPEED..=MAX_TILT_SPEED / 2;

/// A continuous pan/tilt drive request: which way to drive, and how fast
/// along each axis.
///
/// This is VISCA's own encoding rather than a mirrored enum of our own,
/// because the shape only makes sense as VISCA defines it. The speed fields
/// have no "stopped" value while the direction field does, so a stop is a
/// direction; and both speeds go on the wire regardless of which axes the
/// direction actually moves, so an unused axis still needs a speed for the
/// camera to ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Velocity {
    /// Which way to drive, or [`PanTiltDirection::Stop`] to stop.
    pub direction: PanTiltDirection,
    /// How fast to pan, in VISCA's `1..=`[`MAX_PAN_SPEED`] range.
    pub pan_speed: u8,
    /// How fast to tilt, in VISCA's `1..=`[`MAX_TILT_SPEED`] range.
    pub tilt_speed: u8,
}

impl Velocity {
    /// Stop moving.
    pub const STOP: Self = Self {
        direction: PanTiltDirection::Stop,
        pan_speed: MIN_SPEED,
        tilt_speed: MIN_SPEED,
    };

    /// Whether this velocity ends movement rather than starting it.
    pub fn is_stop(self) -> bool {
        self.direction == PanTiltDirection::Stop
    }
}

/// Maps one axis's deflection onto that axis's speed range, or `None` if it's
/// inside the deadzone.
fn axis_speed(deflection: f32, speeds: RangeInclusive<u8>) -> Option<u8> {
    deflection::speed(deflection.abs(), speeds)
}

/// Builds a velocity from a control's deflection along each axis, each a
/// fraction of full travel in `-1.0..=1.0` (positive being right and up).
///
/// The two axes scale independently, so a control pushed hard right and a
/// little up pans fast while tilting slowly: the diagonals cover a whole
/// range of arcs rather than a fixed 45 degrees.
pub fn velocity_from_axes(pan: f32, tilt: f32) -> Velocity {
    let pan_speed = axis_speed(pan, PAN_SPEEDS);
    let tilt_speed = axis_speed(tilt, TILT_SPEEDS);

    let direction = match (pan_speed.map(|_| pan > 0.0), tilt_speed.map(|_| tilt > 0.0)) {
        (None, None) => return Velocity::STOP,
        (Some(true), None) => PanTiltDirection::Right,
        (Some(false), None) => PanTiltDirection::Left,
        (None, Some(true)) => PanTiltDirection::Up,
        (None, Some(false)) => PanTiltDirection::Down,
        (Some(true), Some(true)) => PanTiltDirection::UpRight,
        (Some(true), Some(false)) => PanTiltDirection::DownRight,
        (Some(false), Some(true)) => PanTiltDirection::UpLeft,
        (Some(false), Some(false)) => PanTiltDirection::DownLeft,
    };

    Velocity {
        direction,
        pan_speed: pan_speed.unwrap_or(MIN_SPEED),
        tilt_speed: tilt_speed.unwrap_or(MIN_SPEED),
    }
}

/// Sets the camera's continuous pan/tilt drive, or stops it.
///
/// A stop goes out as a drive in the `Stop` direction rather than through the
/// library's dedicated stop call, because that is the same command with the
/// same bytes — one code path covers both.
pub fn drive<T>(camera: &BlockingClient<GenericVisca, T>, velocity: Velocity) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.pan_tilt_move(
        velocity.direction,
        PanSpeed::new(velocity.pan_speed)?,
        TiltSpeed::new(velocity.tilt_speed)?,
    )
}

/// Sends the camera to its home position.
pub fn home<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.pan_tilt_home()
}

/// Recalibrates the pan/tilt mechanism, ending at the home position.
///
/// Worth having next to [`home`], which it otherwise looks like: a camera
/// that has been turned by hand, or has lost steps against an obstruction,
/// answers position inquiries with numbers that no longer match where it is
/// actually pointing — and every saved preset is expressed in those numbers.
/// The reset re-finds the mechanical limits and re-zeroes them, at the cost
/// of a full sweep to get there.
pub fn reset<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.pan_tilt_reset()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

    #[test]
    fn home_and_reset_each_reach_the_camera() {
        let acks = || scripted_camera(vec![helpers::standard_command_response(1)]);

        home(&acks()).expect("scripted camera should ack home");
        reset(&acks()).expect("scripted camera should ack reset");
    }

    #[test]
    fn a_centered_control_stops() {
        assert_eq!(velocity_from_axes(0.0, 0.0), Velocity::STOP);
        assert!(Velocity::STOP.is_stop());
    }

    #[test]
    fn deflection_inside_the_deadzone_stops() {
        assert_eq!(velocity_from_axes(0.05, -0.05), Velocity::STOP);
    }

    #[test]
    fn the_deadzone_is_the_one_every_other_control_uses() {
        // A hand that has learned the rockers has learned the pad.
        assert_eq!(
            velocity_from_axes(deflection::DEADZONE, 0.0),
            Velocity::STOP
        );
    }

    #[test]
    fn a_single_axis_drives_a_cardinal_direction() {
        assert_eq!(
            velocity_from_axes(1.0, 0.0).direction,
            PanTiltDirection::Right
        );
        assert_eq!(
            velocity_from_axes(-1.0, 0.0).direction,
            PanTiltDirection::Left
        );
        assert_eq!(velocity_from_axes(0.0, 1.0).direction, PanTiltDirection::Up);
        assert_eq!(
            velocity_from_axes(0.0, -1.0).direction,
            PanTiltDirection::Down
        );
    }

    #[test]
    fn both_axes_drive_a_diagonal() {
        assert_eq!(
            velocity_from_axes(1.0, 1.0).direction,
            PanTiltDirection::UpRight
        );
        assert_eq!(
            velocity_from_axes(1.0, -1.0).direction,
            PanTiltDirection::DownRight
        );
        assert_eq!(
            velocity_from_axes(-1.0, 1.0).direction,
            PanTiltDirection::UpLeft
        );
        assert_eq!(
            velocity_from_axes(-1.0, -1.0).direction,
            PanTiltDirection::DownLeft
        );
    }

    #[test]
    fn full_deflection_asks_for_the_top_speed_of_each_axis() {
        let velocity = velocity_from_axes(1.0, 1.0);
        assert_eq!(velocity.pan_speed, *PAN_SPEEDS.end());
        assert_eq!(velocity.tilt_speed, *TILT_SPEEDS.end());
    }

    #[test]
    fn the_fastest_a_control_asks_for_is_well_short_of_what_the_camera_can_do() {
        // Measured on the camera: driven at the top of VISCA's range it swings
        // past anything worth looking at, so no amount of pushing reaches it.
        assert!(
            *PAN_SPEEDS.end() <= MAX_PAN_SPEED / 2,
            "pushing the pad flat out asked for {} of the camera's {MAX_PAN_SPEED}",
            *PAN_SPEEDS.end()
        );
        assert!(
            *TILT_SPEEDS.end() <= MAX_TILT_SPEED / 2,
            "pushing the pad flat out asked for {} of the camera's {MAX_TILT_SPEED}",
            *TILT_SPEEDS.end()
        );
    }

    #[test]
    fn deflection_just_past_the_deadzone_asks_for_the_slowest_speed() {
        assert_eq!(
            velocity_from_axes(deflection::DEADZONE + 0.001, 0.0).pan_speed,
            MIN_SPEED
        );
    }

    #[test]
    fn half_a_push_asks_for_far_less_than_half_the_top_speed() {
        // What makes the pad usable for framing rather than only for getting
        // somewhere: a straight mapping would put half a push at half the top
        // speed, which crosses a room faster than a subject can be followed.
        let half = velocity_from_axes(0.5, 0.0).pan_speed;

        assert!(
            half * 3 <= *PAN_SPEEDS.end(),
            "half the travel should stay slow, got {half} of {}",
            *PAN_SPEEDS.end()
        );
    }

    #[test]
    fn the_slow_half_of_the_pad_stays_in_the_slow_speeds() {
        for step in 11..=50 {
            let speed = velocity_from_axes(step as f32 / 100.0, 0.0).pan_speed;
            assert!(
                speed * 3 <= *PAN_SPEEDS.end(),
                "{step}% of full push asked for {speed} of {}",
                *PAN_SPEEDS.end()
            );
        }
    }

    #[test]
    fn the_middle_of_the_travel_is_where_the_slowest_speeds_are() {
        // What a hand reaches for most, and what the pad is mostly for: a push
        // held somewhere short of the stops frames a shot, so the middle of
        // the travel belongs to the bottom of the range rather than the middle
        // of it. Getting somewhere in a hurry is the far end's job.
        assert_eq!(velocity_from_axes(0.5, 0.0).pan_speed, MIN_SPEED);
        assert!(
            velocity_from_axes(0.65, 0.0).pan_speed * 4 <= *PAN_SPEEDS.end(),
            "two thirds of a push asked for {} of {}",
            velocity_from_axes(0.65, 0.0).pan_speed,
            *PAN_SPEEDS.end()
        );
    }

    #[test]
    fn speed_scales_with_deflection() {
        let gentle = velocity_from_axes(0.4, 0.0).pan_speed;
        let firm = velocity_from_axes(0.9, 0.0).pan_speed;
        assert!(
            gentle < firm,
            "a further push should ask for a faster pan ({gentle} vs {firm})"
        );
    }

    #[test]
    fn the_axes_scale_independently_so_diagonals_are_not_fixed_at_45_degrees() {
        // Hard right, barely up: pan should be at its own maximum while tilt
        // stays near the bottom of its range.
        let velocity = velocity_from_axes(1.0, 0.2);
        assert_eq!(velocity.pan_speed, *PAN_SPEEDS.end());
        assert!(
            velocity.tilt_speed < *TILT_SPEEDS.end() / 2,
            "tilt should stay slow when the control is barely deflected up"
        );
    }

    #[test]
    fn an_unused_axis_still_carries_a_speed_the_camera_will_accept() {
        // VISCA rejects a zero speed on either axis even when the direction
        // doesn't move that axis, so a pure tilt still needs a pan speed.
        assert!(velocity_from_axes(0.0, 1.0).pan_speed >= MIN_SPEED);
    }

    #[test]
    fn deflection_beyond_full_travel_is_capped_rather_than_rejected() {
        assert_eq!(velocity_from_axes(3.0, 0.0).pan_speed, *PAN_SPEEDS.end());
    }

    fn scripted_camera(
        replies: Vec<grafton_visca::testing::testkit::Step>,
    ) -> BlockingClient<GenericVisca, ScriptedBlockingTransport> {
        grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(ScriptedBlockingTransport::new(replies))
            .expect("camera should build from a scripted transport")
    }

    #[test]
    fn drive_sends_a_move_command() {
        let camera = scripted_camera(vec![helpers::standard_command_response(1)]);
        drive(&camera, velocity_from_axes(1.0, 1.0))
            .expect("scripted camera should ack and complete the drive");
    }

    #[test]
    fn drive_sends_a_stop_command() {
        let camera = scripted_camera(vec![helpers::standard_command_response(1)]);
        drive(&camera, Velocity::STOP).expect("scripted camera should ack and complete the stop");
    }

    #[test]
    fn drive_reports_an_out_of_range_speed_rather_than_sending_it() {
        let camera = scripted_camera(vec![]);
        let too_fast = Velocity {
            pan_speed: MAX_PAN_SPEED + 1,
            ..velocity_from_axes(1.0, 0.0)
        };
        assert!(drive(&camera, too_fast).is_err());
    }
}
