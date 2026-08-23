//! How far a control is pushed, turned into how fast the camera moves.
//!
//! Shared by every continuous control there is — the pan/tilt pad, the zoom
//! and focus rockers, and the movement keys — so that a hand which has learned
//! one of them has learned all of them. Each control differs only in the range
//! of speeds the camera will accept from it.

use std::ops::RangeInclusive;

/// How far a control has to be pushed before it drives at all, as a fraction
/// of full deflection.
///
/// Keeps a control released near-but-not-exactly centre, or a trembling hand,
/// from creeping. A physical joystick needs this most — its springs never
/// return it to exactly zero, and without a deadzone a stick nobody is
/// touching would drift the shot — but a mouse a pixel off centre and a
/// trackpad under a resting finger have the same problem in miniature.
pub const DEADZONE: f32 = 0.1;

/// How sharply speed builds as a control is pushed further.
///
/// A straight mapping gives the fast end more of the travel than it is worth:
/// half a push asks for half the speed, and the slow speeds a shot is framed
/// with are crowded into the first part of the push, too small to hold steady.
/// Bending it hands them more room, while full deflection still means the top
/// of the range, so nothing is given up.
///
/// Steep, because framing is nearly all of what these controls do. **Half the
/// travel asks for the slowest speed the camera has**, and it takes about two
/// thirds of a push before the second speed arrives; the rest of the range
/// lives in the last third, where a hand that has decided to go somewhere
/// pushes anyway. What that is worth is easiest to see in how much travel the
/// slowest speed gets: a fifth of it under the gentler curve this replaced,
/// which is a band too narrow to find on a sprung stick and hold, against half
/// of it now.
///
/// The camera's own slowest speed is the floor here and no curve can go under
/// it — this only decides how much of a hand's travel is spent up against it.
const EXPO: f32 = 4.0;

/// What share of a control's speed range a push of `magnitude` (`0.0..=1.0`)
/// asks for, or `None` while it's still inside the deadzone.
pub fn fraction(magnitude: f32) -> Option<f32> {
    if magnitude <= DEADZONE {
        return None;
    }
    // The travel outside the deadzone spans the whole range, so the slowest
    // speed sits right at the deadzone edge and the fastest is only reachable
    // at full deflection.
    let travel = ((magnitude - DEADZONE) / (1.0 - DEADZONE)).clamp(0.0, 1.0);
    Some(travel.powf(EXPO))
}

/// The speed a push of `magnitude` asks for from a camera that accepts
/// `speeds`, or `None` while the control is at rest.
pub fn speed(magnitude: f32, speeds: RangeInclusive<u8>) -> Option<u8> {
    let span = f32::from(speeds.end() - speeds.start());
    fraction(magnitude).map(|fraction| speeds.start() + (fraction * span).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in speed range with room to see the shape of the curve.
    const SPEEDS: RangeInclusive<u8> = 0..=100;

    #[test]
    fn a_control_at_rest_asks_for_nothing() {
        assert_eq!(fraction(0.0), None);
        assert_eq!(fraction(DEADZONE), None);
        assert_eq!(speed(0.0, SPEEDS), None);
    }

    #[test]
    fn full_deflection_asks_for_everything() {
        assert_eq!(fraction(1.0), Some(1.0));
        assert_eq!(speed(1.0, SPEEDS), Some(100));
    }

    #[test]
    fn a_push_past_the_end_is_no_faster_than_the_end() {
        assert_eq!(speed(2.0, SPEEDS), speed(1.0, SPEEDS));
    }

    #[test]
    fn the_slowest_speed_sits_just_past_the_deadzone() {
        assert_eq!(speed(DEADZONE + 0.001, SPEEDS), Some(0));
    }

    #[test]
    fn a_control_at_the_middle_of_its_travel_asks_for_the_slowest_speed() {
        // The point of the curve, in the words it was asked for in: the
        // minimum speed should be what a control held near its middle drives
        // at. Camera-sized ranges rather than the wide one the other tests
        // use, since "the slowest speed" is a real number of steps down.
        assert_eq!(speed(0.5, 1..=12), Some(1));
        assert_eq!(speed(0.5, 0..=7), Some(0));
    }

    #[test]
    fn the_slowest_speed_gets_the_lion_s_share_of_the_travel() {
        // Counted rather than reasoned about, because this is the thing being
        // bought: a slowest speed reachable only in a narrow band by the
        // deadzone is one a sprung stick cannot be held at. Two fifths of the
        // travel that drives at all, which counting the deadzone is half the
        // distance from the middle of a control to its stop.
        let steps = 11..=100;
        let total = steps.clone().count();
        let slowest = steps
            .filter(|step| speed(*step as f32 / 100.0, 1..=12).expect("past the deadzone") == 1)
            .count();

        assert!(
            slowest * 5 >= total * 2,
            "the slowest speed got only {slowest} of {total} steps of travel"
        );
    }

    #[test]
    fn the_slow_speeds_get_more_of_the_travel_than_their_share() {
        // A straight mapping would spend exactly a third of the travel on the
        // slowest third of the speeds; framing happens down there, so the
        // curve owes it more room.
        let steps = 11..=100;
        let total = steps.clone().count();
        let slow = steps
            .filter(|step| {
                speed(*step as f32 / 100.0, SPEEDS).expect("past the deadzone drives") <= 33
            })
            .count();

        assert!(
            slow * 3 > total,
            "the slowest third of the speeds got only {slow} of {total} steps of travel"
        );
    }

    #[test]
    fn pushing_further_never_asks_for_less() {
        let mut previous = 0;
        for step in 11..=100 {
            let speed = speed(step as f32 / 100.0, SPEEDS).expect("past the deadzone drives");
            assert!(
                speed >= previous,
                "speed went backwards at {step}% of full push: {previous} then {speed}"
            );
            previous = speed;
        }
    }

    #[test]
    fn a_speed_range_that_does_not_start_at_zero_starts_where_it_says() {
        assert_eq!(speed(DEADZONE + 0.001, 1..=24), Some(1));
        assert_eq!(speed(1.0, 1..=24), Some(24));
    }
}
