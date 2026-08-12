//! What a game controller is asking the camera to do.
//!
//! Deliberately free of any gamepad library: this takes the sticks and buttons
//! as plain numbers and booleans, so the whole mapping — which is the part
//! that is taste rather than fact — can be exercised without a controller
//! plugged in. Reading the actual device is a thin translation layer in the
//! front end, and is the only part that needs hardware to check.
//!
//! The sticks feed the same [`deflection`](crate::deflection) curve and
//! deadzone as the pointer and the keys, so a hand that has learned the
//! on-screen pad has learned the stick.

use crate::{drives::Drives, focus::FocusDrive, pan_tilt::velocity_from_axes, zoom::ZoomDrive};

/// Which buttons are down. Named the way the layout is, not the way one
/// vendor's labels are: on an Xbox-style controller [`Self::south`] is A,
/// [`Self::east`] is B, [`Self::west`] is X and [`Self::north`] is Y.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons {
    pub south: bool,
    pub east: bool,
    pub west: bool,
    pub north: bool,
    pub left_bumper: bool,
    pub right_bumper: bool,
    /// The two small centre buttons, Start and Back/View.
    pub start: bool,
    pub back: bool,
    /// The sticks themselves, which click.
    pub left_stick: bool,
    pub right_stick: bool,
}

/// Where every control on the controller is.
///
/// Sticks run `-1.0..=1.0` per axis, positive being right and up. Triggers run
/// `0.0..=1.0`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pad {
    pub left_stick: (f32, f32),
    pub right_stick: (f32, f32),
    pub left_trigger: f32,
    pub right_trigger: f32,
    pub buttons: Buttons,
}

/// Something the operator asked for by pressing a button, as opposed to the
/// continuous drives the sticks and triggers ask for by being held.
///
/// The two toggles say what to switch rather than what to switch *to*: which
/// way they go depends on what the camera is currently doing, which only the
/// front end knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Go to this preset.
    Recall(u8),
    /// Store where the camera is pointing now as this preset.
    Mark(u8),
    /// Return to the home position.
    Home,
    /// Switch between focusing automatically and by hand.
    ToggleAutoFocus,
    /// Wake the camera, or put it back into standby.
    TogglePower,
}

/// Whether one particular button is down.
type Down = fn(&Buttons) -> bool;

/// The preset each button stands for: the four face buttons in the order their
/// labels are usually read, then the two bumpers for the rest.
const PRESET_BUTTONS: [(Down, u8); 6] = [
    (|buttons| buttons.south, 1),
    (|buttons| buttons.east, 2),
    (|buttons| buttons.west, 3),
    (|buttons| buttons.north, 4),
    (|buttons| buttons.left_bumper, 5),
    (|buttons| buttons.right_bumper, 6),
];

/// A controller, and enough memory of it to tell a press from a hold.
///
/// A held button means one preset recall, not one per frame — the same
/// distinction the keyboard front end draws between a key's press and its
/// repeats.
#[derive(Debug, Default)]
pub struct Gamepad {
    previous: Pad,
}

impl Gamepad {
    /// Takes the controller's current state and reports what it is asking for:
    /// the drives to hold, and whatever was just pressed.
    pub fn update(&mut self, pad: Pad) -> (Drives, Vec<Press>) {
        let presses = self.presses(&pad);
        self.previous = pad;
        (drives(&pad), presses)
    }

    /// What was pressed this time round that wasn't held last time.
    fn presses(&self, pad: &Pad) -> Vec<Press> {
        let newly = |down: Down| down(&pad.buttons) && !down(&self.previous.buttons);

        // Both centre buttons together, which is deliberately awkward: a
        // camera dropped into standby by a stray thumb mid-shot is worse than
        // one that takes two fingers to switch off.
        if pad.buttons.start && pad.buttons.back && (newly(|b| b.start) || newly(|b| b.back)) {
            return vec![Press::TogglePower];
        }

        let mut presses = Vec::new();
        for (down, preset) in PRESET_BUTTONS {
            if newly(down) {
                // Start held turns a preset button from "go there" into
                // "remember here" — the same button, so there is nothing extra
                // to learn, and a modifier so it can't happen by accident.
                presses.push(if pad.buttons.start {
                    Press::Mark(preset)
                } else {
                    Press::Recall(preset)
                });
            }
        }
        // Each stick clicks to do the thing that stick is already about: the
        // pan/tilt stick sends the camera home, and the zoom/focus stick is
        // what takes focus off automatic so the triggers can drive it.
        if newly(|buttons| buttons.left_stick) {
            presses.push(Press::Home);
        }
        if newly(|buttons| buttons.right_stick) {
            presses.push(Press::ToggleAutoFocus);
        }
        presses
    }
}

/// What the sticks and triggers are asking for, which depends only on where
/// they are right now — letting go *is* the stop, exactly as it is for the
/// on-screen pad.
fn drives(pad: &Pad) -> Drives {
    Drives {
        pan_tilt: velocity_from_axes(pad.left_stick.0, pad.left_stick.1),
        // Push up to tighten, the way pushing up on the on-screen rocker's
        // right-hand end does.
        zoom: ZoomDrive::from_deflection(pad.right_stick.1),
        // Focus is on the triggers rather than on the right stick's other
        // axis, which is where it first looks like it belongs. Zoom is the
        // constant operation and focus the rare, precise one, and sharing a
        // stick would let every zoom bleed into focus — on the control where
        // the distance between sharp and soft is smallest. The triggers are
        // analogue, so they keep speed-by-deflection, and they are under
        // different fingers, so neither disturbs the other.
        focus: FocusDrive::from_deflection(pad.right_trigger - pad.left_trigger),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{focus::FocusDirection, zoom::ZoomDirection};
    use grafton_visca::command::PanTiltDirection;

    /// A controller nobody is touching.
    fn resting() -> Pad {
        Pad::default()
    }

    /// What a pad asks for, from rest, with nothing pressed before it.
    fn asked_for(pad: Pad) -> (Drives, Vec<Press>) {
        Gamepad::default().update(pad)
    }

    fn pressing(set: fn(&mut Buttons)) -> Pad {
        let mut pad = resting();
        set(&mut pad.buttons);
        pad
    }

    #[test]
    fn a_controller_nobody_is_touching_asks_for_nothing() {
        let (drives, presses) = asked_for(resting());

        assert_eq!(drives, Drives::STOPPED);
        assert!(presses.is_empty());
    }

    #[test]
    fn the_left_stick_pans_and_tilts() {
        let pad = Pad {
            left_stick: (1.0, 0.0),
            ..resting()
        };
        assert_eq!(asked_for(pad).0.pan_tilt.direction, PanTiltDirection::Right);

        let pad = Pad {
            left_stick: (0.0, 1.0),
            ..resting()
        };
        assert_eq!(asked_for(pad).0.pan_tilt.direction, PanTiltDirection::Up);
    }

    #[test]
    fn the_right_stick_zooms_tighter_pushed_up_and_wider_pushed_down() {
        let up = Pad {
            right_stick: (0.0, 1.0),
            ..resting()
        };
        let down = Pad {
            right_stick: (0.0, -1.0),
            ..resting()
        };

        assert_eq!(
            asked_for(up).0.zoom.map(|drive| drive.direction),
            Some(ZoomDirection::In)
        );
        assert_eq!(
            asked_for(down).0.zoom.map(|drive| drive.direction),
            Some(ZoomDirection::Out)
        );
    }

    #[test]
    fn the_triggers_focus_near_and_far() {
        let near = Pad {
            left_trigger: 1.0,
            ..resting()
        };
        let far = Pad {
            right_trigger: 1.0,
            ..resting()
        };

        assert_eq!(
            asked_for(near).0.focus.map(|drive| drive.direction),
            Some(FocusDirection::Near)
        );
        assert_eq!(
            asked_for(far).0.focus.map(|drive| drive.direction),
            Some(FocusDirection::Far)
        );
    }

    #[test]
    fn both_triggers_at_once_cancel_rather_than_fighting() {
        let pad = Pad {
            left_trigger: 1.0,
            right_trigger: 1.0,
            ..resting()
        };

        assert_eq!(asked_for(pad).0.focus, None);
    }

    #[test]
    fn a_stick_barely_off_centre_asks_for_nothing() {
        // The whole reason a resting stick doesn't creep the shot: springs
        // never return one to exactly zero.
        let pad = Pad {
            left_stick: (0.04, -0.03),
            right_stick: (0.0, 0.05),
            ..resting()
        };
        let (drives, _) = asked_for(pad);

        assert!(drives.pan_tilt.is_stop());
        assert_eq!(drives.zoom, None);
    }

    #[test]
    fn pushing_a_stick_further_asks_for_more_speed() {
        let gentle = Pad {
            left_stick: (0.4, 0.0),
            ..resting()
        };
        let firm = Pad {
            left_stick: (1.0, 0.0),
            ..resting()
        };

        assert!(asked_for(gentle).0.pan_tilt.pan_speed < asked_for(firm).0.pan_tilt.pan_speed);
    }

    #[test]
    fn each_preset_button_recalls_its_own_preset() {
        let recalled = |set: fn(&mut Buttons)| asked_for(pressing(set)).1;

        assert_eq!(recalled(|b| b.south = true), vec![Press::Recall(1)]);
        assert_eq!(recalled(|b| b.east = true), vec![Press::Recall(2)]);
        assert_eq!(recalled(|b| b.west = true), vec![Press::Recall(3)]);
        assert_eq!(recalled(|b| b.north = true), vec![Press::Recall(4)]);
        assert_eq!(recalled(|b| b.left_bumper = true), vec![Press::Recall(5)]);
        assert_eq!(recalled(|b| b.right_bumper = true), vec![Press::Recall(6)]);
    }

    #[test]
    fn start_turns_a_preset_button_into_a_mark() {
        let pad = pressing(|buttons| {
            buttons.start = true;
            buttons.south = true;
        });

        assert_eq!(asked_for(pad).1, vec![Press::Mark(1)]);
    }

    #[test]
    fn a_held_preset_button_recalls_once_rather_than_every_frame() {
        let mut gamepad = Gamepad::default();
        let held = pressing(|buttons| buttons.south = true);

        assert_eq!(gamepad.update(held).1, vec![Press::Recall(1)]);
        assert!(
            gamepad.update(held).1.is_empty(),
            "holding it is not asking again"
        );
        assert!(gamepad.update(resting()).1.is_empty(), "nor is letting go");
        assert_eq!(
            gamepad.update(held).1,
            vec![Press::Recall(1)],
            "but pressing it again is"
        );
    }

    #[test]
    fn the_sticks_click_to_do_what_each_stick_is_already_about() {
        assert_eq!(
            asked_for(pressing(|b| b.left_stick = true)).1,
            vec![Press::Home]
        );
        assert_eq!(
            asked_for(pressing(|b| b.right_stick = true)).1,
            vec![Press::ToggleAutoFocus]
        );
    }

    #[test]
    fn power_takes_both_centre_buttons_and_recalls_nothing_on_the_way() {
        let pad = pressing(|buttons| {
            buttons.start = true;
            buttons.back = true;
        });

        assert_eq!(asked_for(pad).1, vec![Press::TogglePower]);
    }

    #[test]
    fn either_centre_button_alone_does_not_switch_the_camera_off() {
        // The point of the combo: a stray thumb mid-shot must not park the
        // camera. Start alone is only the Mark modifier; Back alone is
        // nothing at all.
        assert!(
            !asked_for(pressing(|b| b.start = true))
                .1
                .contains(&Press::TogglePower)
        );
        assert!(
            !asked_for(pressing(|b| b.back = true))
                .1
                .contains(&Press::TogglePower)
        );
    }

    #[test]
    fn holding_the_power_combo_switches_once() {
        let mut gamepad = Gamepad::default();
        let combo = pressing(|buttons| {
            buttons.start = true;
            buttons.back = true;
        });

        assert_eq!(gamepad.update(combo).1, vec![Press::TogglePower]);
        assert!(
            gamepad.update(combo).1.is_empty(),
            "holding the combo is not asking again"
        );
    }

    #[test]
    fn the_sticks_keep_driving_while_a_button_is_pressed() {
        // A preset recalled mid-move shouldn't cancel the move; the drives
        // are what the sticks say, always.
        let pad = Pad {
            left_stick: (1.0, 0.0),
            ..pressing(|buttons| buttons.south = true)
        };
        let (drives, presses) = asked_for(pad);

        assert!(!drives.pan_tilt.is_stop());
        assert_eq!(presses, vec![Press::Recall(1)]);
    }
}
