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
/// A straight mapping spends the whole bottom of the travel on speeds too fast
/// to frame with: half-way out on the pad is half of a top speed that crosses
/// the room in seconds, and the slow end that actually gets used is a sliver
/// too small to hold steady. Squaring it hands that sliver most of the travel
/// — half a push now asks for a fifth of the speed — while full deflection
/// still means full speed, so nothing is given up at the top.
///
/// Two rather than three: the same reasoning says cube it, but past a point
/// the far end of the travel turns into a cliff, where a little more push is a
/// lot more speed and the shot lurches.
const EXPO: f32 = 2.0;

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
    fn the_low_end_of_the_travel_is_where_the_slow_speeds_live() {
        // The whole point of the curve, and what a straight mapping does not
        // do: half a push is nowhere near half the speed.
        let half = speed(0.5, SPEEDS).expect("half a push drives");

        assert!(
            half < 25,
            "half the travel should ask for well under a quarter of the speed, got {half}"
        );
    }

    #[test]
    fn the_first_half_of_the_travel_covers_more_speeds_than_a_straight_mapping_would() {
        // Counted rather than reasoned about: how many distinct speeds the
        // slow half of the travel can actually reach. A straight mapping
        // spends half its speeds there and leaves the slow end coarse.
        let reachable = (0..=50)
            .filter_map(|step| speed(step as f32 / 100.0, SPEEDS))
            .collect::<std::collections::BTreeSet<_>>();

        assert!(
            reachable.iter().all(|&speed| speed < 25),
            "the slow half of the travel should stay in the slow speeds, got {reachable:?}"
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
