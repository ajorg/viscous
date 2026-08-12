//! What a rocker asks for: zoom and focus are pushed away from a centre rest
//! position, and how far decides how fast.
//!
//! The same idea as the pan/tilt pad, on one axis instead of two — and the
//! same idea the camera's own controls use, since VISCA's zoom and focus
//! drives both take a speed alongside the direction.

use grafton_visca::types::SpeedLevel;

/// How far a rocker has to be pushed before it drives at all, as a fraction
/// of full deflection. Matches the pad's, so a hand that has learned one has
/// learned the other.
pub const DEADZONE: f32 = 0.1;

/// The speeds a rocker offers, slowest first.
///
/// [`SpeedLevel::Slowest`] is deliberately left out: it is zero on both the
/// zoom and focus scales, and grafton-visca reads a zoom speed of zero as a
/// stopped drive. A rocker whose first notch might do nothing is worse than
/// one with four notches that all move.
const LEVELS: [SpeedLevel; 4] = [
    SpeedLevel::Slow,
    SpeedLevel::Medium,
    SpeedLevel::Fast,
    SpeedLevel::Fastest,
];

/// The speed a rocker pushed `magnitude` of the way to its end asks for, or
/// `None` while it's still within the deadzone.
pub fn speed(magnitude: f32) -> Option<SpeedLevel> {
    if magnitude <= DEADZONE {
        return None;
    }
    let fraction = ((magnitude - DEADZONE) / (1.0 - DEADZONE)).clamp(0.0, 1.0);
    let last = LEVELS.len() - 1;
    let level = (fraction * last as f32).round() as usize;
    Some(LEVELS[level.min(last)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rocker_at_rest_asks_for_nothing() {
        assert_eq!(speed(0.0), None);
        assert_eq!(speed(DEADZONE), None);
    }

    #[test]
    fn pushing_further_asks_for_more_speed() {
        let nudged = speed(0.2).expect("past the deadzone should drive");
        let pushed = speed(0.6).expect("past the deadzone should drive");

        assert_eq!(speed(1.0), Some(SpeedLevel::Fastest));
        assert_ne!(nudged, pushed);
        assert_ne!(pushed, SpeedLevel::Fastest);
    }

    #[test]
    fn no_notch_of_the_rocker_stands_still() {
        // From the first notch past the deadzone to the end of the travel.
        for step in 2..=10 {
            let magnitude = step as f32 / 10.0;
            let level = speed(magnitude).expect("past the deadzone should drive");
            assert_ne!(level, SpeedLevel::Slowest, "at {magnitude} of full push");
        }
    }

    #[test]
    fn a_push_past_the_end_is_no_faster_than_the_end() {
        assert_eq!(speed(2.0), speed(1.0));
    }
}
