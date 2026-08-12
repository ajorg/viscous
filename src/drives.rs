//! What every control on every input surface is asking the camera to do.
//!
//! One shape shared by the pointer, the keyboard and the gamepad, so that two
//! of them can be live at once without either cancelling the other, and so
//! that a front end has one thing to send rather than three.

use crate::{focus::FocusDrive, pan_tilt::Velocity, zoom::ZoomDrive};

/// What the camera's three continuous controls are being asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drives {
    pub pan_tilt: Velocity,
    pub zoom: Option<ZoomDrive>,
    pub focus: Option<FocusDrive>,
}

impl Drives {
    /// Nothing being asked for: every control at rest.
    pub const STOPPED: Self = Self {
        pan_tilt: Velocity::STOP,
        zoom: None,
        focus: None,
    };

    /// This set of drives, filled in from `other` wherever it asks for
    /// nothing — so two input surfaces can be live at once (a drag while a
    /// zoom key is held, a stick while a mouse is on the pad) without either
    /// cancelling the other.
    ///
    /// Not commutative, and deliberately so: whichever is written first wins
    /// each control it is actually asking for.
    pub fn or(self, other: Self) -> Self {
        Self {
            pan_tilt: if self.pan_tilt.is_stop() {
                other.pan_tilt
            } else {
                self.pan_tilt
            },
            zoom: self.zoom.or(other.zoom),
            focus: self.focus.or(other.focus),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pan_tilt::velocity_from_axes;

    fn panning() -> Drives {
        Drives {
            pan_tilt: velocity_from_axes(1.0, 0.0),
            ..Drives::STOPPED
        }
    }

    fn zooming() -> Drives {
        Drives {
            zoom: ZoomDrive::from_deflection(1.0),
            ..Drives::STOPPED
        }
    }

    #[test]
    fn nothing_asked_for_leaves_every_control_at_rest() {
        assert!(Drives::STOPPED.pan_tilt.is_stop());
        assert_eq!(Drives::STOPPED.zoom, None);
        assert_eq!(Drives::STOPPED.focus, None);
    }

    #[test]
    fn two_surfaces_driving_different_controls_both_get_through() {
        let both = panning().or(zooming());

        assert!(!both.pan_tilt.is_stop(), "the pan should survive");
        assert!(both.zoom.is_some(), "and so should the zoom");
    }

    #[test]
    fn the_first_surface_wins_a_control_they_both_ask_for() {
        let left = Drives {
            pan_tilt: velocity_from_axes(-1.0, 0.0),
            ..Drives::STOPPED
        };

        assert_eq!(panning().or(left).pan_tilt, panning().pan_tilt);
    }

    #[test]
    fn a_surface_asking_for_nothing_yields_to_one_that_is() {
        assert_eq!(Drives::STOPPED.or(panning()), panning());
    }
}
