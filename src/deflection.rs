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
/// Bending it hands them more room — half a push now asks for about a third of
/// the range — while full deflection still means the top of the range, so
/// nothing is given up.
///
/// One and a half rather than two. Squaring it was picked while a control drove
/// the camera's whole mechanical range, where the top was so fast that the only
/// way to make the rest usable was to bury it in the last of the travel. Now
/// that the range itself stops at a speed worth driving at
/// ([`PAN_SPEEDS`](crate::pan_tilt::PAN_SPEEDS)), that much curve only pushes
/// every speed towards the far end and leaves the middle of the travel — where
/// a hand naturally sits — asking for barely any speed at all.
const EXPO: f32 = 1.5;

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

/// The one of `options` that a push of `magnitude` asks for, slowest first, or
/// `None` while the control is at rest.
///
/// For the controls whose speeds the camera only takes as a handful of named
/// levels rather than as a number.
pub fn choose<T: Copy>(magnitude: f32, options: &[T]) -> Option<T> {
    let last = options.len() - 1;
    fraction(magnitude).map(|fraction| options[(fraction * last as f32).round() as usize])
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
    fn half_a_push_asks_for_well_under_half_the_speed() {
        // The whole point of the curve, and what a straight mapping does not
        // do: the middle of the travel is worth about a third of the range.
        let half = speed(0.5, SPEEDS).expect("half a push drives");

        assert!(
            (25..40).contains(&half),
            "half the travel should ask for around a third of the speed, got {half}"
        );
    }

    #[test]
    fn the_slow_speeds_get_more_of_the_travel_than_their_share() {
        // Counted rather than reasoned about. A straight mapping would spend
        // exactly a third of the travel on the slowest third of the speeds;
        // framing happens down there, so the curve owes it more room.
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

    #[test]
    fn choosing_from_named_levels_runs_slowest_to_fastest() {
        let levels = ["slowest", "slow", "medium", "fast", "fastest"];

        assert_eq!(choose(0.0, &levels), None);
        assert_eq!(choose(DEADZONE + 0.001, &levels), Some("slowest"));
        assert_eq!(choose(1.0, &levels), Some("fastest"));
    }

    #[test]
    fn choosing_from_named_levels_favours_the_slow_ones_too() {
        let levels = ["slowest", "slow", "medium", "fast", "fastest"];

        assert_eq!(
            choose(0.5, &levels),
            Some("slow"),
            "half a push should still be near the slow end"
        );
    }
}
