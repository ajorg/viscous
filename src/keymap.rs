//! Maps terminal key events to application actions, in the default
//! ("normal") input mode.
//!
//! Movement is nudge-based rather than hold-to-drive: most terminals only
//! report key presses, not releases, so there's no portable way to know
//! when a held arrow key has been let go. A discrete nudge per keypress
//! sidesteps that entirely.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    focus::{FAST_FOCUS_NUDGE_DURATION, FOCUS_NUDGE_DURATION, FocusDirection},
    pan_tilt::{FAST_NUDGE_DEGREES, NUDGE_DEGREES, NudgeDirection},
    worker::Intent,
    zoom::{ZOOM_NUDGE_DURATION, ZoomDirection},
};

/// What a key event means to the application.
///
/// Distinct from a camera [`Intent`] since not every action (quitting)
/// involves the camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Send a camera intent to the worker thread.
    Camera(Intent),
    /// Exit the application.
    Quit,
}

/// Maps a key event to an action in the default input mode, or `None` if
/// the key isn't bound to anything (or isn't a press: most terminals never
/// report anything else, but some report repeats/releases too).
pub fn map_key(key: KeyEvent) -> Option<Action> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    let fast = key.modifiers.contains(KeyModifiers::SHIFT);
    let pan_tilt_degrees = if fast {
        FAST_NUDGE_DEGREES
    } else {
        NUDGE_DEGREES
    };

    let action = match key.code {
        KeyCode::Up => Action::Camera(Intent::NudgePanTilt(NudgeDirection::Up, pan_tilt_degrees)),
        KeyCode::Down => {
            Action::Camera(Intent::NudgePanTilt(NudgeDirection::Down, pan_tilt_degrees))
        }
        KeyCode::Left => {
            Action::Camera(Intent::NudgePanTilt(NudgeDirection::Left, pan_tilt_degrees))
        }
        KeyCode::Right => Action::Camera(Intent::NudgePanTilt(
            NudgeDirection::Right,
            pan_tilt_degrees,
        )),

        KeyCode::Char('[') | KeyCode::Char('-') => {
            Action::Camera(Intent::NudgeZoom(ZoomDirection::Out, ZOOM_NUDGE_DURATION))
        }
        KeyCode::Char(']') | KeyCode::Char('=') => {
            Action::Camera(Intent::NudgeZoom(ZoomDirection::In, ZOOM_NUDGE_DURATION))
        }

        // `<`/`>` are Shift+`,`/`.` on the keys that produce them, so they
        // stand for the "faster" focus nudge without depending on the
        // modifier flag also being set (many terminals fold the shift into
        // the character itself and don't report it separately).
        KeyCode::Char(',') => Action::Camera(Intent::NudgeFocus(
            FocusDirection::Near,
            FOCUS_NUDGE_DURATION,
        )),
        KeyCode::Char('.') => Action::Camera(Intent::NudgeFocus(
            FocusDirection::Far,
            FOCUS_NUDGE_DURATION,
        )),
        KeyCode::Char('<') => Action::Camera(Intent::NudgeFocus(
            FocusDirection::Near,
            FAST_FOCUS_NUDGE_DURATION,
        )),
        KeyCode::Char('>') => Action::Camera(Intent::NudgeFocus(
            FocusDirection::Far,
            FAST_FOCUS_NUDGE_DURATION,
        )),

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

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn arrow_without_shift_nudges_at_normal_speed() {
        assert_eq!(
            map_key(press(KeyCode::Up, KeyModifiers::NONE)),
            Some(Action::Camera(Intent::NudgePanTilt(
                NudgeDirection::Up,
                NUDGE_DEGREES
            )))
        );
    }

    #[test]
    fn shift_arrow_nudges_at_fast_speed() {
        assert_eq!(
            map_key(press(KeyCode::Right, KeyModifiers::SHIFT)),
            Some(Action::Camera(Intent::NudgePanTilt(
                NudgeDirection::Right,
                FAST_NUDGE_DEGREES
            )))
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
    fn shifted_focus_punctuation_is_the_fast_variant_regardless_of_modifier_flag() {
        // Deliberately no SHIFT modifier: many terminals fold shift into the
        // character for punctuation and don't also set the modifier bit.
        assert_eq!(
            map_key(press(KeyCode::Char('<'), KeyModifiers::NONE)),
            Some(Action::Camera(Intent::NudgeFocus(
                FocusDirection::Near,
                FAST_FOCUS_NUDGE_DURATION
            )))
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

    #[test]
    fn non_press_events_are_ignored() {
        let mut key = press(KeyCode::Char('q'), KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        assert_eq!(map_key(key), None);
    }
}
