//! Continuous focus drive.
//!
//! Like zoom, VISCA focus only exposes continuous near/far drive commands,
//! so focus is held rather than stepped: one command to start, one to stop.

// `command::FocusSpeed`, deliberately: the library also has a
// `types::FocusSpeed` of the same shape, and only this one is the speed the
// `Focus` command variants below actually take.
use grafton_visca::command::{Focus, FocusSpeed};

use crate::deflection;

/// Which way a focus drive moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    /// Focus nearer.
    Near,
    /// Focus farther.
    Far,
}

/// A focus drive: which way, and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusDrive {
    /// Which way focus is travelling.
    pub direction: FocusDirection,
    /// How fast. Focus is the control that most wants a slow setting: the
    /// distance between sharp and soft is small, and at the camera's own pace
    /// it is easy to drive straight past it.
    ///
    /// The camera's own number, as with zoom, rather than one of the library's
    /// five named levels — see [`drive_focus`] for why that route can't
    /// deliver the slowest of them.
    pub speed: FocusSpeed,
}

/// The fastest speed VISCA's variable focus accepts; 0 is the slowest.
pub const MAX_FOCUS_SPEED: u8 = 7;

/// The fastest a control focuses at this run: `focus_top_speed` in the
/// config file, or all the way.
pub static TOP_SPEED: deflection::TopSpeed = deflection::TopSpeed::new(MAX_FOCUS_SPEED);

/// The speeds a control will focus at, slowest first.
fn focus_speeds() -> std::ops::RangeInclusive<u8> {
    0..=TOP_SPEED.get()
}

impl FocusDrive {
    /// Where along a rocker's travel each focus speed begins, slowest first,
    /// as fractions of full deflection either side of centre — for a control
    /// that wants to draw them.
    pub fn band_starts() -> Vec<f32> {
        deflection::band_starts(focus_speeds().count())
    }

    /// The drive a rocker pushed to `deflection` asks for — negative for
    /// near, positive for far — or `None` while it's still at rest.
    pub fn from_deflection(deflection: f32) -> Option<Self> {
        let direction = if deflection < 0.0 {
            FocusDirection::Near
        } else {
            FocusDirection::Far
        };
        deflection::speed(deflection.abs(), focus_speeds()).map(|speed| Self {
            direction,
            // In range by construction: `speed` never leaves 0..=7.
            speed: FocusSpeed::new(speed).expect("a focus speed from the accepted range"),
        })
    }
}

/// The command that starts `drive`, or stops the focus when it's `None`.
///
/// The library's variable-speed focus commands, rather than its
/// `focus_near`/`focus_far` helpers, which take one of five named speed
/// levels. Those can't express
/// the slowest focus there is: they turn a speed of 0 into the *standard*-speed
/// command (`81 01 04 08 03`), which is the camera's own middling default, so
/// the slowest level would come out faster than the next one up. Verified on
/// the wire, not read off the documentation.
///
/// Going direct gives focus the same 0-to-7 the zoom rocker has, in order.
pub fn drive_focus(drive: Option<FocusDrive>) -> Focus {
    match drive {
        Some(drive) => match drive.direction {
            FocusDirection::Near => Focus::NearWithSpeed(drive.speed),
            FocusDirection::Far => Focus::FarWithSpeed(drive.speed),
        },
        None => Focus::Stop,
    }
}

/// The command that switches focus between automatic and manual.
///
/// Manual is less a preference than a precondition: a camera focusing
/// automatically overrides the near/far drive the moment it sees the scene
/// again, so [`drive_focus`] only holds while focus is manual.
pub fn set_auto_focus(auto: bool) -> Focus {
    if auto { Focus::Auto } else { Focus::Manual }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::encoded;

    fn drive(direction: FocusDirection) -> Option<FocusDrive> {
        Some(FocusDrive {
            direction,
            speed: FocusSpeed::new(4).unwrap(),
        })
    }

    #[test]
    fn drive_focus_starts_a_drive_in_each_direction_at_its_speed() {
        // 81 01 04 08 2p FF focuses far at speed p, 3p near.
        assert_eq!(
            encoded(&drive_focus(drive(FocusDirection::Far))),
            [0x81, 0x01, 0x04, 0x08, 0x24, 0xFF]
        );
        assert_eq!(
            encoded(&drive_focus(drive(FocusDirection::Near))),
            [0x81, 0x01, 0x04, 0x08, 0x34, 0xFF]
        );
    }

    #[test]
    fn a_rocker_at_rest_asks_for_no_focus_and_either_way_off_centre_focuses() {
        assert_eq!(FocusDrive::from_deflection(0.0), None);
        assert_eq!(
            FocusDrive::from_deflection(1.0).map(|drive| drive.direction),
            Some(FocusDirection::Far)
        );
        assert_eq!(
            FocusDrive::from_deflection(-1.0).map(|drive| drive.direction),
            Some(FocusDirection::Near)
        );
    }

    #[test]
    fn pushing_the_rocker_further_focuses_faster() {
        let nudged = FocusDrive::from_deflection(0.2).expect("past the deadzone should drive");
        let pushed = FocusDrive::from_deflection(1.0).expect("past the deadzone should drive");

        assert_ne!(nudged.speed, pushed.speed);
    }

    #[test]
    fn a_gentle_push_asks_for_the_slowest_focus_the_camera_has() {
        // The one that matters most: the distance between sharp and soft is
        // small, and the camera's own default pace drives straight past it.
        let nudged = FocusDrive::from_deflection(0.2).expect("past the deadzone should drive");

        assert_eq!(nudged.speed.value(), 0);
    }

    #[test]
    fn every_notch_of_the_focus_rocker_is_at_least_as_fast_as_the_one_before() {
        // What went wrong before this was measured: routing speed 0 through
        // the library's named levels emits the standard-speed command, a
        // middling default, so the slowest notch outran the second-slowest.
        let mut previous = 0;
        for step in 11..=100 {
            let speed = FocusDrive::from_deflection(step as f32 / 100.0)
                .expect("past the deadzone should drive")
                .speed
                .value();
            assert!(
                speed >= previous,
                "focus speed went backwards at {step}% of full push: {previous} then {speed}"
            );
            previous = speed;
        }
    }

    #[test]
    fn drive_focus_stops_the_drive_when_given_no_direction() {
        assert_eq!(
            encoded(&drive_focus(None)),
            [0x81, 0x01, 0x04, 0x08, 0x00, 0xFF]
        );
    }

    #[test]
    fn set_auto_focus_switches_the_camera_both_ways() {
        assert_eq!(
            encoded(&set_auto_focus(true)),
            [0x81, 0x01, 0x04, 0x38, 0x02, 0xFF]
        );
        assert_eq!(
            encoded(&set_auto_focus(false)),
            [0x81, 0x01, 0x04, 0x38, 0x03, 0xFF]
        );
    }
}
