//! Continuous zoom drive.
//!
//! VISCA zoom has only ever had continuous tele/wide drive commands — no
//! relative move, and `GenericVisca` doesn't support direct positioning
//! either — so hold-to-drive is the shape the camera already offers: one
//! command to start, one to stop, and however long the user holds the
//! control in between.

use grafton_visca::{command::Zoom, types::ZoomSpeed};

use crate::deflection;

/// Which way a zoom drive moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomDirection {
    /// Zoom in (telephoto).
    In,
    /// Zoom out (wide angle).
    Out,
}

/// A zoom drive: which way, and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomDrive {
    /// Which way the shot is going.
    pub direction: ZoomDirection,
    /// How fast, which the camera holds until told otherwise. A slow zoom is
    /// the difference between a shot that can go out live and one that
    /// lurches, so this is worth expressing rather than leaving to the
    /// camera's default.
    ///
    /// The camera's own number rather than one of the library's five named
    /// levels: VISCA's variable zoom takes a speed of 0 to 7 directly, and 0
    /// is a genuine slowest rather than a stop, so going through the named
    /// levels would throw away three of the eight -- including the slowest
    /// one, which is the one a zoom most wants.
    pub speed: ZoomSpeed,
}

/// The fastest speed VISCA's variable zoom accepts; 0 is the slowest.
pub const MAX_ZOOM_SPEED: u8 = 7;

/// The fastest a control zooms at this run: `zoom_top_speed` in the config
/// file, or all the way.
///
/// Worth lowering on a camera whose top speeds are hard to tell apart: on the
/// one this was written beside, 6 and 7 are within a twentieth of each other,
/// so the last band of the rocker buys almost nothing.
pub static TOP_SPEED: deflection::TopSpeed = deflection::TopSpeed::new(MAX_ZOOM_SPEED);

/// The speeds a control will zoom at, slowest first.
fn zoom_speeds() -> std::ops::RangeInclusive<u8> {
    0..=TOP_SPEED.get()
}

impl ZoomDrive {
    /// Where along a rocker's travel each zoom speed begins, slowest first, as
    /// fractions of full deflection either side of centre — for a control
    /// that wants to draw them.
    pub fn band_starts() -> Vec<f32> {
        deflection::band_starts(zoom_speeds().count())
    }

    /// The drive a rocker pushed to `deflection` asks for — negative for
    /// wide, positive for tele — or `None` while it's still at rest.
    pub fn from_deflection(deflection: f32) -> Option<Self> {
        let direction = if deflection < 0.0 {
            ZoomDirection::Out
        } else {
            ZoomDirection::In
        };
        deflection::speed(deflection.abs(), zoom_speeds()).map(|speed| Self {
            direction,
            // In range by construction: `speed` never leaves 0..=7.
            speed: ZoomSpeed::new(speed).expect("a zoom speed from the accepted range"),
        })
    }
}

/// The command that starts `drive`, or stops the zoom when it's `None`.
pub fn drive_zoom(drive: Option<ZoomDrive>) -> Zoom {
    match drive {
        Some(drive) => match drive.direction {
            ZoomDirection::In => Zoom::TeleVariable(drive.speed),
            ZoomDirection::Out => Zoom::WideVariable(drive.speed),
        },
        None => Zoom::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::encoded;

    fn drive(direction: ZoomDirection) -> Option<ZoomDrive> {
        Some(ZoomDrive {
            direction,
            speed: ZoomSpeed::new(4).unwrap(),
        })
    }

    #[test]
    fn drive_zoom_starts_a_drive_in_each_direction_at_its_speed() {
        // 81 01 04 07 2p FF zooms in at speed p, 3p out.
        assert_eq!(
            encoded(&drive_zoom(drive(ZoomDirection::In))),
            [0x81, 0x01, 0x04, 0x07, 0x24, 0xFF]
        );
        assert_eq!(
            encoded(&drive_zoom(drive(ZoomDirection::Out))),
            [0x81, 0x01, 0x04, 0x07, 0x34, 0xFF]
        );
    }

    #[test]
    fn a_rocker_at_rest_asks_for_no_zoom_and_either_way_off_centre_zooms() {
        assert_eq!(ZoomDrive::from_deflection(0.0), None);
        assert_eq!(
            ZoomDrive::from_deflection(1.0).map(|drive| drive.direction),
            Some(ZoomDirection::In)
        );
        assert_eq!(
            ZoomDrive::from_deflection(-1.0).map(|drive| drive.direction),
            Some(ZoomDirection::Out)
        );
    }

    #[test]
    fn pushing_the_rocker_further_zooms_faster() {
        let nudged = ZoomDrive::from_deflection(0.2).expect("past the deadzone should drive");
        let pushed = ZoomDrive::from_deflection(1.0).expect("past the deadzone should drive");

        assert_ne!(nudged.speed, pushed.speed);
    }

    #[test]
    fn a_gentle_push_asks_for_the_slowest_zoom_the_camera_has() {
        // The one that matters most on a live shot, and the one a mapping
        // through the library's named speed levels cannot reach at all.
        let nudged = ZoomDrive::from_deflection(0.2).expect("past the deadzone should drive");

        assert_eq!(nudged.speed.value(), 0);
    }

    #[test]
    fn half_a_push_is_still_near_the_slow_end() {
        // A straight mapping would put this in the middle of the range.
        let half = ZoomDrive::from_deflection(0.5).expect("past the deadzone should drive");

        assert!(
            half.speed.value() <= 2,
            "half a push should stay slow, got speed {}",
            half.speed.value()
        );
    }

    #[test]
    fn drive_zoom_stops_the_drive_when_given_no_direction() {
        assert_eq!(
            encoded(&drive_zoom(None)),
            [0x81, 0x01, 0x04, 0x07, 0x00, 0xFF]
        );
    }
}
