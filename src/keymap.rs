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

use crate::{
    focus::FocusDirection,
    pan_tilt::{Velocity, velocity_from_axes},
    worker::Intent,
    zoom::ZoomDirection,
};

/// How far a movement key deflects its axis, as a fraction of full speed. A
/// keyboard can only really express a couple of speeds, so the plain key is
/// slow enough to frame a shot with, and shift is everything the camera has.
pub const KEY_DEFLECTION: f32 = 0.35;
pub const FAST_KEY_DEFLECTION: f32 = 1.0;

/// A camera drive that runs for as long as its key is held down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    /// Drive pan/tilt at a fixed velocity.
    PanTilt(Velocity),
    /// Drive zoom in a direction.
    Zoom(ZoomDirection),
    /// Drive focus in a direction.
    Focus(FocusDirection),
}

impl Hold {
    /// The intent that starts this drive.
    pub fn start(self) -> Intent {
        match self {
            Self::PanTilt(velocity) => Intent::DrivePanTilt(velocity),
            Self::Zoom(direction) => Intent::DriveZoom(Some(direction)),
            Self::Focus(direction) => Intent::DriveFocus(Some(direction)),
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
    let pan_tilt = |pan: f32, tilt: f32| {
        Action::Hold(Hold::PanTilt(velocity_from_axes(
            pan * deflection,
            tilt * deflection,
        )))
    };

    let action = match key.code {
        KeyCode::Up => pan_tilt(0.0, 1.0),
        KeyCode::Down => pan_tilt(0.0, -1.0),
        KeyCode::Left => pan_tilt(-1.0, 0.0),
        KeyCode::Right => pan_tilt(1.0, 0.0),

        KeyCode::Char('[') | KeyCode::Char('-') => Action::Hold(Hold::Zoom(ZoomDirection::Out)),
        KeyCode::Char(']') | KeyCode::Char('=') => Action::Hold(Hold::Zoom(ZoomDirection::In)),

        // `<`/`>` are Shift+`,`/`.` on the keys that produce them. They used
        // to stand for a longer focus nudge; now that focus runs for as long
        // as the key is held, how far it travels is the user's to decide and
        // the two pairs mean the same thing.
        KeyCode::Char(',') | KeyCode::Char('<') => Action::Hold(Hold::Focus(FocusDirection::Near)),
        KeyCode::Char('.') | KeyCode::Char('>') => Action::Hold(Hold::Focus(FocusDirection::Far)),

        KeyCode::Char(digit @ '1'..='6') => {
            let preset = digit.to_digit(10).expect("matched an ASCII digit") as u8;
            Action::Camera(Intent::RecallPreset(preset))
        }

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

    #[test]
    fn arrows_hold_a_drive_in_their_own_direction() {
        assert_eq!(
            held_velocity(KeyCode::Up, KeyModifiers::NONE).direction,
            PanTiltDirection::Up
        );
        assert_eq!(
            held_velocity(KeyCode::Down, KeyModifiers::NONE).direction,
            PanTiltDirection::Down
        );
        assert_eq!(
            held_velocity(KeyCode::Left, KeyModifiers::NONE).direction,
            PanTiltDirection::Left
        );
        assert_eq!(
            held_velocity(KeyCode::Right, KeyModifiers::NONE).direction,
            PanTiltDirection::Right
        );
    }

    #[test]
    fn shift_drives_the_same_direction_faster() {
        let normal = held_velocity(KeyCode::Right, KeyModifiers::NONE);
        let fast = held_velocity(KeyCode::Right, KeyModifiers::SHIFT);
        assert_eq!(normal.direction, fast.direction);
        assert!(
            fast.pan_speed > normal.pan_speed,
            "shift should ask for a faster pan ({} vs {})",
            fast.pan_speed,
            normal.pan_speed
        );
    }

    #[test]
    fn an_unmodified_arrow_drives_slowly_enough_to_frame_with() {
        let normal = held_velocity(KeyCode::Right, KeyModifiers::NONE);
        let fast = held_velocity(KeyCode::Right, KeyModifiers::SHIFT);
        assert!(
            normal.pan_speed < fast.pan_speed / 2,
            "the plain key should be well under half speed, not a notch off the top"
        );
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
        assert_eq!(
            map_key(press(KeyCode::Char(']'), KeyModifiers::NONE)),
            Some(Action::Hold(Hold::Zoom(ZoomDirection::In)))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('['), KeyModifiers::NONE)),
            Some(Action::Hold(Hold::Zoom(ZoomDirection::Out)))
        );
        assert_eq!(
            map_key(press(KeyCode::Char(','), KeyModifiers::NONE)),
            Some(Action::Hold(Hold::Focus(FocusDirection::Near)))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('.'), KeyModifiers::NONE)),
            Some(Action::Hold(Hold::Focus(FocusDirection::Far)))
        );
    }

    #[test]
    fn shifted_focus_punctuation_holds_the_same_drive_as_the_unshifted_key() {
        // Deliberately no SHIFT modifier: many terminals fold shift into the
        // character for punctuation and don't also set the modifier bit.
        assert_eq!(
            map_key(press(KeyCode::Char('<'), KeyModifiers::NONE)),
            map_key(press(KeyCode::Char(','), KeyModifiers::NONE))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('>'), KeyModifiers::NONE)),
            map_key(press(KeyCode::Char('.'), KeyModifiers::NONE))
        );
    }

    #[test]
    fn a_hold_starts_and_stops_the_control_it_drives() {
        let hold = Hold::Zoom(ZoomDirection::In);
        assert_eq!(hold.start(), Intent::DriveZoom(Some(ZoomDirection::In)));
        assert_eq!(hold.stop(), Intent::DriveZoom(None));
    }

    #[test]
    fn holds_on_the_same_control_share_a_stop_and_holds_on_others_do_not() {
        let up = Hold::PanTilt(velocity_from_axes(0.0, 1.0));
        let down = Hold::PanTilt(velocity_from_axes(0.0, -1.0));
        assert_eq!(up.stop(), down.stop());
        assert_ne!(up.stop(), Hold::Zoom(ZoomDirection::In).stop());
        assert_ne!(
            Hold::Zoom(ZoomDirection::In).stop(),
            Hold::Focus(FocusDirection::Near).stop()
        );
    }

    #[test]
    fn digit_keys_recall_matching_preset() {
        assert_eq!(
            map_key(press(KeyCode::Char('3'), KeyModifiers::NONE)),
            Some(Action::Camera(Intent::RecallPreset(3)))
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
