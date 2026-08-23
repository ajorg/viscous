//! Telling a tap of an arrow key from a hold of one.
//!
//! The arrows have always driven the camera for as long as they were held, and
//! they still do. A tap of one used to be nothing more than a very short drive
//! — the head accelerating, travelling for however long a finger stayed down,
//! and decelerating — which lands somewhere near where you wanted and never
//! twice in the same place. That is the gesture this turns into a step: one
//! tap, one smallest movement the camera can make, ten taps exactly ten of
//! them.
//!
//! Which of the two a press turns out to be can only be known by waiting, so
//! the drive now starts a fraction of a second after the key goes down rather
//! than the instant it does. That delay is the whole cost of the feature, and
//! it is paid only by the gesture that is about to move the camera a long way
//! anyway.

use std::time::{Duration, Instant};

use egui::Key;
use grafton_visca::command::PanTiltDirection;

/// How long an arrow key has to be down before it is driving rather than
/// stepping.
///
/// Long enough to sit above a deliberate tap, short enough that a hand holding
/// a key to pan doesn't notice waiting for it.
pub const HOLD: Duration = Duration::from_millis(200);

/// The arrow keys and the way each one points.
const ARROWS: [(Key, PanTiltDirection); 4] = [
    (Key::ArrowUp, PanTiltDirection::Up),
    (Key::ArrowDown, PanTiltDirection::Down),
    (Key::ArrowLeft, PanTiltDirection::Left),
    (Key::ArrowRight, PanTiltDirection::Right),
];

/// The arrow keys, and enough memory of them to tell a tap from a hold.
pub struct Arrows {
    /// When each arrow key went down, in the order [`ARROWS`] is in.
    since: [Option<Instant>; ARROWS.len()],
    /// Which of them have been down long enough to be driving.
    driving: [bool; ARROWS.len()],
    hold: Duration,
}

impl Default for Arrows {
    fn default() -> Self {
        Self::new(HOLD)
    }
}

impl Arrows {
    /// Arrows that start driving after `hold`. A zero `hold` is a keyboard
    /// with no taps in it at all, where every press drives at once.
    pub fn new(hold: Duration) -> Self {
        Self {
            since: [None; ARROWS.len()],
            driving: [false; ARROWS.len()],
            hold,
        }
    }

    /// Whether `key` is one of the arrows this is watching.
    pub fn owns(key: Key) -> bool {
        ARROWS.iter().any(|(arrow, _)| *arrow == key)
    }

    /// Takes which keys are down at `now` and reports the steps to take: one
    /// for each arrow let go of before it became a hold.
    ///
    /// Two arrows tapped together come back as two steps rather than one
    /// diagonal, which arrive one after the other and land in the same place.
    pub fn update(&mut self, down: impl Fn(Key) -> bool, now: Instant) -> Vec<PanTiltDirection> {
        let mut steps = Vec::new();
        for (index, (key, direction)) in ARROWS.iter().enumerate() {
            match (down(*key), self.since[index]) {
                (true, was) => {
                    let went_down = was.unwrap_or(now);
                    self.since[index] = Some(went_down);
                    self.driving[index] = now.duration_since(went_down) >= self.hold;
                }
                (false, Some(went_down)) => {
                    if now.duration_since(went_down) < self.hold {
                        steps.push(*direction);
                    }
                    self.since[index] = None;
                    self.driving[index] = false;
                }
                (false, None) => {}
            }
        }
        steps
    }

    /// Whether `key` is being held rather than still deciding what it is.
    ///
    /// What the drive reads instead of the key itself, so that the first
    /// moments of a press ask the camera for nothing while it is still an
    /// open question whether the key is about to come back up.
    pub fn driving(&self, key: Key) -> bool {
        ARROWS
            .iter()
            .position(|(arrow, _)| *arrow == key)
            .is_some_and(|index| self.driving[index])
    }

    /// Forgets every key that is down, without stepping for any of them.
    ///
    /// For the moments when the keyboard stops belonging to the camera — a
    /// description field taking it mid-press — where the press that is already
    /// down was never a tap aimed at the camera and shouldn't become one when
    /// it is released.
    pub fn forget(&mut self) {
        *self = Self::new(self.hold);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moment far enough into the program's life to subtract from.
    fn start() -> Instant {
        Instant::now()
    }

    /// The steps that come of holding `key` down for `held`.
    fn tap_for(held: Duration) -> Vec<PanTiltDirection> {
        let mut arrows = Arrows::default();
        let down = start();

        arrows.update(|key| key == Key::ArrowRight, down);
        arrows.update(|_| false, down + held)
    }

    #[test]
    fn a_key_let_go_of_quickly_is_a_step() {
        assert_eq!(tap_for(HOLD / 2), vec![PanTiltDirection::Right]);
    }

    #[test]
    fn a_key_held_and_then_released_has_already_driven_and_does_not_also_step() {
        assert!(
            tap_for(HOLD * 2).is_empty(),
            "a hold that ends is a drive ending, not a step starting"
        );
    }

    #[test]
    fn a_key_only_just_pressed_is_not_yet_driving() {
        let mut arrows = Arrows::default();

        arrows.update(|key| key == Key::ArrowRight, start());

        assert!(
            !arrows.driving(Key::ArrowRight),
            "a press that might still turn out to be a tap must not drive"
        );
    }

    #[test]
    fn a_key_held_past_the_threshold_drives() {
        let mut arrows = Arrows::default();
        let down = start();

        arrows.update(|key| key == Key::ArrowRight, down);
        let steps = arrows.update(|key| key == Key::ArrowRight, down + HOLD);

        assert!(arrows.driving(Key::ArrowRight));
        assert!(steps.is_empty(), "a key still down has not stepped");
    }

    #[test]
    fn a_keyboard_with_no_hold_at_all_drives_from_the_first_frame() {
        // What the drive tests use, and what the pointer and the stick have
        // always done: no waiting to see, because there is no tap to wait for.
        let mut arrows = Arrows::new(Duration::ZERO);

        arrows.update(|key| key == Key::ArrowRight, start());

        assert!(arrows.driving(Key::ArrowRight));
    }

    #[test]
    fn two_arrows_tapped_together_step_both_ways() {
        let mut arrows = Arrows::default();
        let down = start();

        arrows.update(|key| key == Key::ArrowRight || key == Key::ArrowUp, down);
        let steps = arrows.update(|_| false, down + HOLD / 2);

        assert_eq!(steps.len(), 2);
        assert!(steps.contains(&PanTiltDirection::Right));
        assert!(steps.contains(&PanTiltDirection::Up));
    }

    #[test]
    fn each_arrow_steps_the_way_it_points() {
        for (key, direction) in ARROWS {
            let mut arrows = Arrows::default();
            let down = start();

            arrows.update(|pressed| pressed == key, down);

            assert_eq!(arrows.update(|_| false, down + HOLD / 2), vec![direction]);
        }
    }

    #[test]
    fn a_key_the_camera_stopped_listening_to_does_not_step_when_it_comes_up() {
        let mut arrows = Arrows::default();
        let down = start();

        arrows.update(|key| key == Key::ArrowRight, down);
        arrows.forget();

        assert!(
            arrows.update(|_| false, down + HOLD / 2).is_empty(),
            "a key that stopped being the camera's mid-press shouldn't step"
        );
    }

    #[test]
    fn the_arrows_are_the_keys_this_owns_and_nothing_else() {
        assert!(Arrows::owns(Key::ArrowLeft));
        assert!(!Arrows::owns(Key::Comma));
    }
}
