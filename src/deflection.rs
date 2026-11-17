//! How far a control is pushed, turned into how fast the camera moves.
//!
//! Shared by every continuous control there is — the pan/tilt pad, the zoom
//! and focus rockers, and the movement keys — so that a hand which has learned
//! one of them has learned all of them. Each control differs only in the range
//! of speeds the camera will accept from it.
//!
//! A camera has a handful of speeds, not a continuum, so what a control really
//! has is a row of **bands**: stretches of its travel that each ask for one
//! speed. Everything about how the control feels comes down to where those
//! bands begin and end, which is what this module decides.

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

/// How many times wider the slowest speed's band is than the fastest's, with
/// each band in between narrower than the last by the same proportion.
///
/// The starting point is that every speed gets a band a hand can find and
/// hold. The speeds a camera offers are spaced by ratio — measured on the
/// hardware, each is around a third to two thirds faster than the one below —
/// and that is also how a change of speed is seen: the eye tells two speeds
/// apart by their proportion, not their difference, so each step up the table
/// looks about as large as any other. Steps that look alike deserve bands that
/// feel alike, which means bands of about the same width rather than widths
/// that follow the speed numbers or the rates behind them.
///
/// The curve this replaced did the opposite. It gave the slowest speed half
/// of all the travel and left the fastest eight of twelve sharing the last
/// fifth, a few hundredths of the travel each: past three quarters of a push,
/// moving a hand by less than it can feel changed the speed by half.
///
/// Not quite equal, though, because the travel is not used equally. Framing a
/// shot happens at the bottom of the range and is where a hand lingers and
/// corrects; the top is passed through on the way somewhere. So the slow end
/// gets the wider bands, tapering evenly to the fast end, and even the
/// narrowest is one a hand can stay inside.
const SLOW_END_SHARE: f32 = 3.0;

/// Where each of `count` bands begins, as a fraction of full deflection,
/// slowest first: the first begins at the [`DEADZONE`], and the last runs
/// from where it begins to full deflection.
///
/// Public because these are worth seeing as well as feeling — a control that
/// draws its own bands shows a hand where the next speed is before it gets
/// there.
pub fn band_starts(count: usize) -> Vec<f32> {
    // Each band a fixed proportion narrower than the one before, such that
    // the last is `SLOW_END_SHARE` times narrower than the first.
    let taper = if count > 1 {
        SLOW_END_SHARE.powf(-1.0 / (count - 1) as f32)
    } else {
        1.0
    };
    let widths: Vec<f32> = (0..count).map(|band| taper.powi(band as i32)).collect();
    let total: f32 = widths.iter().sum();

    let mut start = DEADZONE;
    widths
        .iter()
        .map(|width| {
            let begins = start;
            start += width / total * (1.0 - DEADZONE);
            begins
        })
        .collect()
}

/// Which of `count` bands a push of `magnitude` (`0.0..=1.0`) is in, slowest
/// being `0`, or `None` while it's still inside the deadzone.
pub fn band(magnitude: f32, count: usize) -> Option<usize> {
    if magnitude <= DEADZONE {
        return None;
    }
    band_starts(count)
        .iter()
        .rposition(|begins| magnitude >= *begins)
}

/// The speed a push of `magnitude` asks for from a camera that accepts
/// `speeds`, or `None` while the control is at rest.
pub fn speed(magnitude: f32, speeds: RangeInclusive<u8>) -> Option<u8> {
    let count = usize::from(speeds.end() - speeds.start()) + 1;
    band(magnitude, count).map(|band| speeds.start() + band as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera-sized speed range: the nine a pan is driven at.
    const SPEEDS: RangeInclusive<u8> = 1..=9;

    /// How much of the travel each speed in `speeds` gets, in hundredths of
    /// full deflection, counted rather than calculated.
    fn travel_per_speed(speeds: RangeInclusive<u8>) -> Vec<usize> {
        speeds
            .clone()
            .map(|wanted| {
                (1..=1000)
                    .filter(|step| speed(*step as f32 / 1000.0, speeds.clone()) == Some(wanted))
                    .count()
                    / 10
            })
            .collect()
    }

    #[test]
    fn a_control_at_rest_asks_for_nothing() {
        assert_eq!(band(0.0, 9), None);
        assert_eq!(band(DEADZONE, 9), None);
        assert_eq!(speed(0.0, SPEEDS), None);
    }

    #[test]
    fn full_deflection_asks_for_everything() {
        assert_eq!(speed(1.0, SPEEDS), Some(9));
    }

    #[test]
    fn a_push_past_the_end_is_no_faster_than_the_end() {
        assert_eq!(speed(2.0, SPEEDS), speed(1.0, SPEEDS));
    }

    #[test]
    fn the_slowest_speed_sits_just_past_the_deadzone() {
        assert_eq!(speed(DEADZONE + 0.001, SPEEDS), Some(1));
        assert_eq!(speed(DEADZONE + 0.001, 0..=7), Some(0));
    }

    #[test]
    fn every_speed_gets_a_band_a_hand_can_hold() {
        // The whole point. A speed reachable only inside a couple of
        // hundredths of the travel is one a hand passes through, not one it
        // can choose; a twentieth is about the least that can be stayed in.
        for speeds in [1..=9, 0..=7] {
            let travel = travel_per_speed(speeds.clone());
            assert!(
                travel.iter().all(|hundredths| *hundredths >= 5),
                "each of {speeds:?} should get at least 5% of the travel, got {travel:?}"
            );
        }
    }

    #[test]
    fn no_speed_takes_much_more_of_the_travel_than_the_rest() {
        // The fault this replaced: the slowest speed had half the travel to
        // itself, which is half a control doing one thing.
        let travel = travel_per_speed(SPEEDS);
        assert!(
            travel.iter().all(|hundredths| *hundredths <= 20),
            "no speed should get more than a fifth of the travel, got {travel:?}"
        );
    }

    #[test]
    fn the_slow_end_gets_the_wider_bands() {
        let travel = travel_per_speed(SPEEDS);
        assert!(
            travel.windows(2).all(|pair| pair[0] >= pair[1]),
            "bands should narrow toward the fast end, got {travel:?}"
        );
        assert!(
            travel[0] >= travel[travel.len() - 1] * 2,
            "the slowest band should be much the widest, got {travel:?}"
        );
    }

    #[test]
    fn the_bands_between_them_cover_all_the_travel_past_the_deadzone() {
        let starts = band_starts(9);
        assert_eq!(starts.len(), 9);
        assert_eq!(starts[0], DEADZONE);
        assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(*starts.last().unwrap() < 1.0);
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
    fn a_camera_with_one_speed_drives_at_it_everywhere() {
        assert_eq!(speed(0.2, 3..=3), Some(3));
        assert_eq!(speed(1.0, 3..=3), Some(3));
    }
}
