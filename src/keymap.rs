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
    focus::FocusDrive,
    pan_tilt::{Velocity, velocity_from_axes},
    worker::Intent,
    zoom::ZoomDrive,
};

/// How far a movement key deflects its axis, as a fraction of full travel. A
/// keyboard can only really express a couple of speeds, so this is the one to
/// frame a shot at, and [`FAST_KEY_DEFLECTION`] is the fastest a control drives.
///
/// Chosen for the speed it comes out at rather than for the number itself: it
/// lands on 5 of the 9 pan speeds a control asks for, which is the pace a held
/// arrow key wants, and in the middle of the zoom and focus speeds. Since a
/// key has no travel to feel its way along, it doesn't benefit from where
/// [`deflection`](crate::deflection) puts its bands — it only has to land
/// inside the one for that speed, which is why moving the bands moved this
/// number without changing what the key does.
pub const KEY_DEFLECTION: f32 = 0.66;

/// The fastest a zoom or focus key drives, which shift reaches.
pub const FAST_KEY_DEFLECTION: f32 = 1.0;

/// How far an arrow deflects pan or tilt: just past the deadzone, into the
/// camera's slowest speed.
///
/// The arrows are the fine control. A tap is a hair of movement and holding
/// one creeps, which no other control does: the pad and the stick have the
/// whole range for anything faster. Shift takes an arrow up to
/// [`KEY_DEFLECTION`]'s framing pace rather than to the top speed, which on a
/// key, with no way to ease off it, sweeps past anything worth looking at.
pub const ARROW_DEFLECTION: f32 = 0.12;

/// The pan/tilt and the zoom/focus deflections a key asks for, with or without
/// shift.
pub fn key_deflections(shift: bool) -> (f32, f32) {
    if shift {
        (KEY_DEFLECTION, FAST_KEY_DEFLECTION)
    } else {
        (ARROW_DEFLECTION, KEY_DEFLECTION)
    }
}

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
    let (aim, deflection) = key_deflections(key.modifiers.contains(KeyModifiers::SHIFT));
    let pan_tilt = |pan: f32, tilt: f32| {
        Action::Hold(Hold::PanTilt(velocity_from_axes(pan * aim, tilt * aim)))
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

    let action = match key.code {
        // Held, like every other movement key, and faster with shift; but
        // slow to start with, since the arrows are the fine control.
        KeyCode::Up => pan_tilt(0.0, 1.0),
        KeyCode::Down => pan_tilt(0.0, -1.0),
        KeyCode::Left => pan_tilt(-1.0, 0.0),
        KeyCode::Right => pan_tilt(1.0, 0.0),

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

    #[test]
    fn an_arrow_holds_a_drive_in_its_own_direction_with_or_without_shift() {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            for (code, direction) in [
                (KeyCode::Up, PanTiltDirection::Up),
                (KeyCode::Down, PanTiltDirection::Down),
                (KeyCode::Left, PanTiltDirection::Left),
                (KeyCode::Right, PanTiltDirection::Right),
            ] {
                assert_eq!(held_velocity(code, modifiers).direction, direction);
            }
        }
    }

    #[test]
    fn an_arrow_creeps_at_the_slowest_speed_the_camera_has() {
        // The arrows are the fine control. Where the bands fall decides what a
        // given deflection comes out as, and this keeps `ARROW_DEFLECTION`
        // honest about the speed it was picked for.
        let plain = held_velocity(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(plain.pan_speed, 1);
        assert_eq!(held_velocity(KeyCode::Up, KeyModifiers::NONE).tilt_speed, 1);
    }

    #[test]
    fn a_shifted_arrow_drives_slowly_enough_to_frame_with() {
        // A key has no travel to ease off along, so its fast speed has to be
        // one a shot can still be followed at, not the top of the range.
        let shifted = held_velocity(KeyCode::Right, KeyModifiers::SHIFT).pan_speed;

        assert_eq!(shifted, 5, "a shifted arrow should be a framing pace");
    }

    #[test]
    fn shift_drives_an_arrow_faster_as_it_does_every_movement_key() {
        for code in [KeyCode::Up, KeyCode::Right] {
            let plain = held_velocity(code, KeyModifiers::NONE);
            let shifted = held_velocity(code, KeyModifiers::SHIFT);
            assert!(
                shifted.pan_speed.max(shifted.tilt_speed) > plain.pan_speed.max(plain.tilt_speed),
                "{code:?}: {plain:?} then {shifted:?}"
            );
        }
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
