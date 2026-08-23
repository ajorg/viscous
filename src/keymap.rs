//! Maps terminal key events to application actions, in the default
//! ("normal") input mode.
//!
//! Movement keys are hold-to-drive: pressing one starts a continuous camera
//! drive, and letting go stops it. What a terminal reports for a held key
//! varies — modern ones send presses, repeats and releases, older ones only
//! ever send presses — so the mapping here deliberately ignores which kind of
//! event a key came in as, and [`session`](crate::session) decides what a
//! press, repeat or release means for the drive it's tracking.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use grafton_visca::command::PanTiltDirection;

use crate::{
    focus::FocusDrive,
    nudge::Step,
    pan_tilt::{Velocity, velocity_from_axes},
    worker::Intent,
    zoom::ZoomDrive,
};

/// How far a movement key deflects its axis, as a fraction of full travel. A
/// keyboard can only really express a couple of speeds, so this is the one to
/// frame a shot at, and [`FAST_KEY_DEFLECTION`] is the fastest a control drives.
///
/// Chosen for the speed it comes out at rather than for the number itself: it
/// lands on 5 of the 12 pan speeds a control asks for, which is the pace a held
/// arrow key wants. Since a key has no travel to feel its way along, it doesn't
/// benefit from [`deflection`](crate::deflection)'s curve — it only has to
/// land in the same place the curve puts that speed, which is why steepening
/// the curve moved this number without changing what the key does.
pub const KEY_DEFLECTION: f32 = 0.8;

/// The fastest a key drives, which zoom and focus reach by holding shift.
///
/// Pan and tilt no longer have a second speed to reach: shift is what tells
/// an arrow to drive at all rather than to step, so it has no room left to
/// also mean "faster". The pad and the stick still cover the whole range.
pub const FAST_KEY_DEFLECTION: f32 = 1.0;

/// A camera drive that runs for as long as its key is held down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    /// Drive pan/tilt at a fixed velocity.
    PanTilt(Velocity),
    /// Drive zoom.
    Zoom(ZoomDrive),
    /// Drive focus.
    Focus(FocusDrive),
}

impl Hold {
    /// The intent that starts this drive.
    pub fn start(self) -> Intent {
        match self {
            Self::PanTilt(velocity) => Intent::DrivePanTilt(velocity),
            Self::Zoom(drive) => Intent::DriveZoom(Some(drive)),
            Self::Focus(drive) => Intent::DriveFocus(Some(drive)),
        }
    }

    /// The intent that stops whichever control this drive moves.
    ///
    /// Doubles as the identity of that control: two holds stop the same
    /// control exactly when their stop intents are equal.
    pub fn stop(self) -> Intent {
        match self {
            Self::PanTilt(_) => Intent::DrivePanTilt(Velocity::STOP),
            Self::Zoom(_) => Intent::DriveZoom(None),
            Self::Focus(_) => Intent::DriveFocus(None),
        }
    }
}

/// What a key event means to the application.
///
/// Distinct from a camera [`Intent`] since not every action (quitting)
/// involves the camera, and a held key stands for a drive plus the stop that
/// eventually has to follow it rather than for one command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Drive a camera control for as long as the key stays down.
    Hold(Hold),
    /// Send a one-shot camera command.
    Camera(Intent),
    /// Switch the camera between focusing for itself and leaving focus to
    /// the focus keys.
    ///
    /// Not a [`Self::Camera`] intent, because which way to switch depends on
    /// which way the camera is currently focusing and mapping a key can't
    /// know that.
    ToggleAutoFocus,
    /// Wake the camera, or put it back into standby.
    ///
    /// A [`Self::ToggleAutoFocus`] for the same reason: which way to switch
    /// depends on whether the camera is currently awake.
    TogglePower,
    /// Exit the application.
    Quit,
}

/// Maps a key event to an action in the default input mode, or `None` if the
/// key isn't bound to anything.
///
/// The event's kind (press, repeat, release) is deliberately not consulted:
/// the same key means the same control whichever way it's reported, and only
/// the caller — which knows what's currently being driven — can say what a
/// release of it should do.
pub fn map_key(key: KeyEvent) -> Option<Action> {
    let deflection = if key.modifiers.contains(KeyModifiers::SHIFT) {
        FAST_KEY_DEFLECTION
    } else {
        KEY_DEFLECTION
    };
    // Always the framing pace: shift is spent telling the arrow to drive
    // instead of stepping, so there is no second pan/tilt speed for it to
    // also ask for.
    let pan_tilt = |pan: f32, tilt: f32| {
        Action::Hold(Hold::PanTilt(velocity_from_axes(
            pan * KEY_DEFLECTION,
            tilt * KEY_DEFLECTION,
        )))
    };
    // Both key deflections are well past the rocker's deadzone, so there is
    // always a drive to be had from one.
    let zoom = |way: f32, deflection: f32| {
        Action::Hold(Hold::Zoom(
            ZoomDrive::from_deflection(way * deflection).expect("a key deflects past the deadzone"),
        ))
    };
    let focus = |way: f32, deflection: f32| {
        Action::Hold(Hold::Focus(
            FocusDrive::from_deflection(way * deflection)
                .expect("a key deflects past the deadzone"),
        ))
    };

    let step = |direction| Action::Camera(Intent::Nudge(Step::towards(direction)));
    let shifted = key.modifiers.contains(KeyModifiers::SHIFT);

    let action = match key.code {
        // Tapped, an arrow steps the camera one unit; held with shift it
        // drives. Two gestures on one key, kept apart by the modifier rather
        // than by how long the key was down: a control that has to be timed
        // right is the wrong one to put precision on, and the pad and the
        // stick already drive without needing the keyboard's help.
        KeyCode::Up if shifted => pan_tilt(0.0, 1.0),
        KeyCode::Down if shifted => pan_tilt(0.0, -1.0),
        KeyCode::Left if shifted => pan_tilt(-1.0, 0.0),
        KeyCode::Right if shifted => pan_tilt(1.0, 0.0),

        KeyCode::Up => step(PanTiltDirection::Up),
        KeyCode::Down => step(PanTiltDirection::Down),
        KeyCode::Left => step(PanTiltDirection::Left),
        KeyCode::Right => step(PanTiltDirection::Right),

        KeyCode::Char('[') | KeyCode::Char('-') => zoom(-1.0, deflection),
        KeyCode::Char(']') | KeyCode::Char('=') => zoom(1.0, deflection),

        // The shifted punctuation on the same keys, which many terminals send
        // as the character alone without also setting the modifier — so they
        // are matched here rather than left to the modifier check above. They
        // mean what shift means everywhere else: the same drive, faster.
        KeyCode::Char('{') | KeyCode::Char('_') => zoom(-1.0, FAST_KEY_DEFLECTION),
        KeyCode::Char('}') | KeyCode::Char('+') => zoom(1.0, FAST_KEY_DEFLECTION),

        KeyCode::Char(',') => focus(-1.0, deflection),
        KeyCode::Char('.') => focus(1.0, deflection),
        KeyCode::Char('<') => focus(-1.0, FAST_KEY_DEFLECTION),
        KeyCode::Char('>') => focus(1.0, FAST_KEY_DEFLECTION),

        KeyCode::Char(digit @ '1'..='6') => {
            let preset = digit.to_digit(10).expect("matched an ASCII digit") as u8;
            Action::Camera(Intent::RecallPreset(preset))
        }

        // Manual focus doesn't hold while the camera is focusing for itself,
        // so the way out of that has to be reachable from the keyboard too.
        KeyCode::Char('f') => Action::ToggleAutoFocus,
        // Likewise for standby, which ignores every other key here: without
        // this one a camera that went to sleep could only be woken by
        // quitting and finding some other way to do it.
        KeyCode::Char('p') => Action::TogglePower,

        KeyCode::Char('q') => Action::Quit,
        // Ctrl-D is the conventional EOF/quit key for a terminal session.
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,

        _ => return None,
    };

    Some(action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{focus::FocusDirection, zoom::ZoomDirection};
    use grafton_visca::command::PanTiltDirection;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn held_velocity(code: KeyCode, modifiers: KeyModifiers) -> Velocity {
        match map_key(press(code, modifiers)) {
            Some(Action::Hold(Hold::PanTilt(velocity))) => velocity,
            other => panic!("expected a pan/tilt hold, got {other:?}"),
        }
    }

    /// The step an arrow key asks for on its own.
    fn stepped(code: KeyCode) -> Step {
        match map_key(press(code, KeyModifiers::NONE)) {
            Some(Action::Camera(Intent::Nudge(step))) => step,
            other => panic!("expected a step, got {other:?}"),
        }
    }

    #[test]
    fn an_arrow_on_its_own_steps_the_way_it_points() {
        assert_eq!(stepped(KeyCode::Up), Step::towards(PanTiltDirection::Up));
        assert_eq!(
            stepped(KeyCode::Down),
            Step::towards(PanTiltDirection::Down)
        );
        assert_eq!(
            stepped(KeyCode::Left),
            Step::towards(PanTiltDirection::Left)
        );
        assert_eq!(
            stepped(KeyCode::Right),
            Step::towards(PanTiltDirection::Right)
        );
    }

    #[test]
    fn shift_and_an_arrow_hold_a_drive_in_their_own_direction() {
        assert_eq!(
            held_velocity(KeyCode::Up, KeyModifiers::SHIFT).direction,
            PanTiltDirection::Up
        );
        assert_eq!(
            held_velocity(KeyCode::Down, KeyModifiers::SHIFT).direction,
            PanTiltDirection::Down
        );
        assert_eq!(
            held_velocity(KeyCode::Left, KeyModifiers::SHIFT).direction,
            PanTiltDirection::Left
        );
        assert_eq!(
            held_velocity(KeyCode::Right, KeyModifiers::SHIFT).direction,
            PanTiltDirection::Right
        );
    }

    #[test]
    fn a_shifted_arrow_drives_slowly_enough_to_frame_with() {
        // A key has no travel to feel its way along, so it has to land on a
        // usable pace by itself: fast enough to get somewhere, slow enough to
        // follow something with. The response curve moved where a given
        // deflection comes out, and this keeps `KEY_DEFLECTION` honest about
        // the speed it was picked for.
        let top = *crate::pan_tilt::PAN_SPEEDS.end();
        let driven = held_velocity(KeyCode::Right, KeyModifiers::SHIFT).pan_speed;

        assert!(
            (top / 4..top / 2).contains(&driven),
            "a driving arrow should be a framing pace, got {driven} of {top}"
        );
    }

    /// Which way a key drives zoom, and how fast.
    fn zoom_drive(code: KeyCode) -> ZoomDrive {
        match map_key(press(code, KeyModifiers::NONE)) {
            Some(Action::Hold(Hold::Zoom(drive))) => drive,
            other => panic!("expected a zoom hold, got {other:?}"),
        }
    }

    /// The same, for focus.
    fn focus_drive(code: KeyCode) -> FocusDrive {
        match map_key(press(code, KeyModifiers::NONE)) {
            Some(Action::Hold(Hold::Focus(drive))) => drive,
            other => panic!("expected a focus hold, got {other:?}"),
        }
    }

    #[test]
    fn both_zoom_key_pairs_map_to_the_same_actions() {
        assert_eq!(
            map_key(press(KeyCode::Char('['), KeyModifiers::NONE)),
            map_key(press(KeyCode::Char('-'), KeyModifiers::NONE))
        );
        assert_eq!(
            map_key(press(KeyCode::Char(']'), KeyModifiers::NONE)),
            map_key(press(KeyCode::Char('='), KeyModifiers::NONE))
        );
    }

    #[test]
    fn zoom_and_focus_keys_hold_their_own_direction() {
        assert_eq!(zoom_drive(KeyCode::Char(']')).direction, ZoomDirection::In);
        assert_eq!(zoom_drive(KeyCode::Char('[')).direction, ZoomDirection::Out);
        assert_eq!(
            focus_drive(KeyCode::Char(',')).direction,
            FocusDirection::Near
        );
        assert_eq!(
            focus_drive(KeyCode::Char('.')).direction,
            FocusDirection::Far
        );
    }

    #[test]
    fn the_shifted_punctuation_drives_the_same_way_faster() {
        // Deliberately no SHIFT modifier: many terminals fold shift into the
        // character for punctuation and don't also set the modifier bit.
        let (near, fast_near) = (
            focus_drive(KeyCode::Char(',')),
            focus_drive(KeyCode::Char('<')),
        );
        let (wide, fast_wide) = (
            zoom_drive(KeyCode::Char('[')),
            zoom_drive(KeyCode::Char('{')),
        );

        assert_eq!(fast_near.direction, near.direction);
        assert_eq!(fast_wide.direction, wide.direction);
        assert_ne!(fast_near.speed, near.speed);
        assert_ne!(fast_wide.speed, wide.speed);
        assert_eq!(
            zoom_drive(KeyCode::Char('_')),
            zoom_drive(KeyCode::Char('{'))
        );
    }

    #[test]
    fn a_hold_starts_and_stops_the_control_it_drives() {
        let drive = zoom_drive(KeyCode::Char(']'));
        let hold = Hold::Zoom(drive);

        assert_eq!(hold.start(), Intent::DriveZoom(Some(drive)));
        assert_eq!(hold.stop(), Intent::DriveZoom(None));
    }

    #[test]
    fn holds_on_the_same_control_share_a_stop_and_holds_on_others_do_not() {
        let up = Hold::PanTilt(velocity_from_axes(0.0, 1.0));
        let down = Hold::PanTilt(velocity_from_axes(0.0, -1.0));
        let zoom = Hold::Zoom(zoom_drive(KeyCode::Char(']')));
        let focus = Hold::Focus(focus_drive(KeyCode::Char('.')));

        assert_eq!(up.stop(), down.stop());
        assert_ne!(up.stop(), zoom.stop());
        assert_ne!(zoom.stop(), focus.stop());
    }

    #[test]
    fn digit_keys_recall_matching_preset() {
        assert_eq!(
            map_key(press(KeyCode::Char('3'), KeyModifiers::NONE)),
            Some(Action::Camera(Intent::RecallPreset(3)))
        );
    }

    #[test]
    fn f_switches_between_focusing_modes() {
        assert_eq!(
            map_key(press(KeyCode::Char('f'), KeyModifiers::NONE)),
            Some(Action::ToggleAutoFocus)
        );
    }

    #[test]
    fn q_quits() {
        assert_eq!(
            map_key(press(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Action::Quit)
        );
    }

    #[test]
    fn ctrl_d_quits() {
        assert_eq!(
            map_key(press(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
    }

    #[test]
    fn plain_d_is_unbound() {
        assert_eq!(map_key(press(KeyCode::Char('d'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn unbound_key_maps_to_nothing() {
        assert_eq!(map_key(press(KeyCode::Char('z'), KeyModifiers::NONE)), None);
    }
}
