//! Flying a shaped path between two positions, instead of going straight there.
//!
//! [`crate::shot::go_to`] hands the camera a destination and lets it find its
//! own way, which is a straight line at a constant speed. That is the right
//! thing nearly always. This is for when the journey is the point — a reveal,
//! a rehearsed transition — and the camera should arrive by some other route.
//!
//! The shape is flown as a **velocity**, not as a series of destinations. A
//! chain of absolute moves would stop dead at every waypoint, because the
//! camera answers each one only when it has physically arrived: a hundred
//! waypoints would be a hundred little stops. A continuous drive doesn't stop
//! between commands — it just changes speed — so a path is a virtual hand on
//! the joystick, moving it along the curve, and the front end's existing tick
//! is what turns it.
//!
//! Two things this leans on, both true of the EVI family and both worth
//! re-checking on anything else:
//!
//! - **Pan and tilt share a unit scale.** On these cameras a unit is the same
//!   fraction of a degree on both axes, so a circle drawn in raw units is
//!   round rather than squashed. The limits reported by a full sweep are what
//!   would say otherwise.
//! - **Speed numbers scale linearly.** See [`Rates`].
//!
//! A path deliberately does not land itself. Its tangential speed falls to
//! nothing as it converges, and the slowest drive the camera accepts is 1 —
//! there is nothing below it — so the last fraction of the approach can't be
//! flown. Fly the shape, then let [`crate::shot::go_to`] set the camera down
//! exactly.

use std::{f32::consts::TAU, time::Duration};

use crate::{
    pan_tilt::{MAX_PAN_SPEED, MAX_TILT_SPEED, Velocity},
    state::Position,
};

/// How fast the head actually travels, in camera units per second, at speed 1
/// on each axis.
///
/// Measured rather than derived. VISCA's speed numbers are ordinals: nothing
/// in the protocol says what speed 7 means in units per second, and the two
/// axes are separate motors that needn't agree. `examples/probe_absolute`
/// measures them against a real camera.
///
/// Speeds are taken to scale linearly from here — speed n travelling n times
/// as fast as speed 1 — which is the other thing that probe is for. If the
/// scale turns out to be bent, this is where a measured table would replace
/// the multiplication, and nothing above it would have to change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    /// Camera units per second at pan speed 1.
    pub pan: f32,
    /// Camera units per second at tilt speed 1.
    pub tilt: f32,
}

/// A spiral flight: from one position to another, winding in as it goes.
///
/// The radius shrinks steadily to nothing while the angle turns whole
/// revolutions, so the camera closes on its destination from every side in
/// turn rather than heading at it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spiral {
    from: Position,
    to: Position,
    turns: f32,
    duration: Duration,
}

impl Spiral {
    /// A spiral from `from` to `to`, winding `turns` full revolutions over
    /// `duration`.
    ///
    /// Zero turns is a straight line, which is worth having: it is the same
    /// flight with the shape taken out, and the honest thing to compare
    /// against.
    pub fn new(from: Position, to: Position, turns: f32, duration: Duration) -> Self {
        Self {
            from,
            to,
            turns,
            duration,
        }
    }

    /// How far through the flight `elapsed` is, from 0 at the start to 1 at
    /// the end.
    fn fraction(&self, elapsed: Duration) -> f32 {
        (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// The polar description of the path at `fraction`: how far out from the
    /// destination, and at what angle around it.
    ///
    /// The radius runs to nothing while the angle keeps turning, which is what
    /// makes the curve close rather than orbit.
    fn polar(&self, fraction: f32) -> (f32, f32, f32) {
        let out_pan = f32::from(self.from.pan) - f32::from(self.to.pan);
        let out_tilt = f32::from(self.from.tilt) - f32::from(self.to.tilt);
        let start_radius = out_pan.hypot(out_tilt);
        let start_angle = out_tilt.atan2(out_pan);

        let radius = start_radius * (1.0 - fraction);
        let angle = start_angle + TAU * self.turns * fraction;
        (radius, angle, start_radius)
    }

    /// Where the path wants the camera to be at `elapsed`.
    ///
    /// Zoom and focus are the destination's throughout: the shape is flown by
    /// the pan/tilt head, and nothing here drives a lens.
    pub fn at(&self, elapsed: Duration) -> Position {
        let (radius, angle, _) = self.polar(self.fraction(elapsed));
        // Rounded, not truncated: `as` truncates toward zero, which would pull
        // every point of the path a fraction of a unit inward and flatten the
        // shape rather than merely blur it.
        Position {
            pan: (f32::from(self.to.pan) + radius * angle.cos()).round() as i16,
            tilt: (f32::from(self.to.tilt) + radius * angle.sin()).round() as i16,
            ..self.to
        }
    }

    /// How fast the path is travelling on each axis at `fraction`, in camera
    /// units per second.
    fn drift(&self, fraction: f32) -> (f32, f32) {
        let (radius, angle, start_radius) = self.polar(fraction);
        let seconds = self.duration.as_secs_f32();

        // d/dt of `to + radius * (cos angle, sin angle)`, with the radius
        // closing at a constant rate and the angle turning at a constant one.
        let closing = -start_radius / seconds;
        let turning = TAU * self.turns / seconds;
        (
            closing * angle.cos() - radius * turning * angle.sin(),
            closing * angle.sin() + radius * turning * angle.cos(),
        )
    }

    /// The drive the camera should be holding at `elapsed`, or `None` once the
    /// flight is over and it should be set down instead.
    ///
    /// The velocity is the path's own derivative rather than a correction
    /// toward where it should be: at a tick or two of latency there is nothing
    /// to correct against that isn't already stale, and a drive that is merely
    /// *aimed* along the curve stays smooth where one chasing a setpoint
    /// hunts.
    pub fn velocity_at(&self, elapsed: Duration, rates: Rates) -> Option<Velocity> {
        if elapsed >= self.duration {
            return None;
        }
        let (pan_per_second, tilt_per_second) = self.drift(self.fraction(elapsed));
        Some(Velocity::from_signed(
            (pan_per_second / rates.pan).round() as i32,
            (tilt_per_second / rates.tilt).round() as i32,
        ))
    }

    /// The shortest this shape can be flown in without asking the head for
    /// more speed than it has.
    ///
    /// Worth asking before flying, because outrunning the head does not merely
    /// hurry the flight — it *deforms* it. A saturated axis stops tracking the
    /// curve while the other keeps going, so the camera traces something that
    /// is not the requested shape at all, and nothing on the wire says so: the
    /// drive commands are all perfectly legal.
    ///
    /// A spiral's cost is mostly its winding rather than its reach. Doubling
    /// the turns roughly doubles the distance travelled, so a shape asked for
    /// in more turns has to be given more time or it will come out flattened.
    pub fn shortest(from: Position, to: Position, turns: f32, rates: Rates) -> Duration {
        /// Enough samples that the peak isn't missed between two of them; the
        /// curve has no features narrower than a fraction of one revolution.
        const SAMPLES: u16 = 512;

        // Speed scales with 1/duration, so the worst point of a one-second
        // flight says directly how many seconds the flight actually needs.
        let nominal = Self::new(from, to, turns, Duration::from_secs(1));
        let ceiling = |rate: f32, max: u8| rate * f32::from(max);

        let seconds = (0..=SAMPLES)
            .map(|step| {
                let (pan, tilt) = nominal.drift(f32::from(step) / f32::from(SAMPLES));
                (pan.abs() / ceiling(rates.pan, MAX_PAN_SPEED))
                    .max(tilt.abs() / ceiling(rates.tilt, MAX_TILT_SPEED))
            })
            .fold(0.0_f32, f32::max);

        Duration::from_secs_f32(seconds)
    }

    /// Where the flight is meant to finish, for setting the camera down on
    /// once the shape has been flown.
    pub fn destination(&self) -> Position {
        self.to
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pan_tilt;
    use grafton_visca::command::PanTiltDirection;

    fn at(pan: i16, tilt: i16) -> Position {
        Position {
            pan,
            tilt,
            zoom: 0x1000,
            focus: 0x2000,
        }
    }

    fn seconds(count: u64) -> Duration {
        Duration::from_secs(count)
    }

    /// Fast enough that a speed number stays well inside the camera's range
    /// for the distances these tests use.
    fn rates() -> Rates {
        Rates {
            pan: 10.0,
            tilt: 10.0,
        }
    }

    #[test]
    fn a_flight_begins_where_it_starts_and_ends_where_it_is_sent() {
        let flight = Spiral::new(at(-400, 200), at(100, -50), 2.0, seconds(10));

        assert_eq!(flight.at(Duration::ZERO), at(-400, 200));
        assert_eq!(flight.at(seconds(10)), at(100, -50));
    }

    #[test]
    fn a_flight_with_no_turns_in_it_goes_straight_there() {
        let flight = Spiral::new(at(-400, 200), at(400, -200), 0.0, seconds(10));

        let halfway = flight.at(seconds(5));

        assert_eq!(halfway.pan, 0);
        assert_eq!(halfway.tilt, 0);
    }

    #[test]
    fn a_flight_with_turns_in_it_leaves_the_straight_line() {
        let straight = Spiral::new(at(-400, 200), at(400, -200), 0.0, seconds(10));
        let spiralled = Spiral::new(at(-400, 200), at(400, -200), 1.0, seconds(10));

        // A quarter of the way round, the wound path is off the direct one by
        // most of what is left of its radius — that is the shape.
        let direct = straight.at(seconds(3));
        let wound = spiralled.at(seconds(3));

        assert!(
            (direct.pan - wound.pan).abs() > 200,
            "a turning path should be nowhere near the straight one"
        );
    }

    #[test]
    fn a_straight_flight_drives_toward_where_it_is_going() {
        // Due right and slightly up, so the direction is unambiguous.
        let flight = Spiral::new(at(0, 0), at(1000, 100), 0.0, seconds(10));

        let velocity = flight
            .velocity_at(seconds(1), rates())
            .expect("the flight is still running");

        assert_eq!(velocity.direction, PanTiltDirection::UpRight);
        assert!(
            velocity.pan_speed > velocity.tilt_speed,
            "ten times as far to pan as to tilt should pan faster than it tilts"
        );
    }

    #[test]
    fn a_wound_flight_turns_the_drive_around_as_it_goes() {
        let flight = Spiral::new(at(-500, 0), at(0, 0), 1.0, seconds(10));

        let early = flight
            .velocity_at(seconds(1), rates())
            .expect("still running");
        let late = flight
            .velocity_at(seconds(6), rates())
            .expect("still running");

        assert_ne!(
            early.direction, late.direction,
            "a full revolution should have the drive pointing elsewhere by halfway"
        );
    }

    #[test]
    fn a_faster_head_is_asked_for_a_smaller_speed_number() {
        // The same wanted velocity against two calibrations: a head that
        // covers more ground per speed step needs a lower number for it.
        let flight = Spiral::new(at(0, 0), at(1000, 0), 0.0, seconds(10));

        let brisk = flight
            .velocity_at(seconds(1), rates())
            .expect("still running");
        let sluggish = flight
            .velocity_at(
                seconds(1),
                Rates {
                    pan: 2.0,
                    ..rates()
                },
            )
            .expect("still running");

        assert!(brisk.pan_speed < sluggish.pan_speed);
    }

    #[test]
    fn a_finished_flight_asks_for_no_drive_at_all() {
        let flight = Spiral::new(at(0, 0), at(1000, 0), 1.0, seconds(10));

        assert!(flight.velocity_at(seconds(10), rates()).is_none());
        assert!(flight.velocity_at(seconds(30), rates()).is_none());
    }

    #[test]
    fn a_flight_flown_no_quicker_than_it_can_be_never_saturates() {
        let (from, to) = (at(-400, 200), at(100, -50));
        let shortest = Spiral::shortest(from, to, 3.0, rates());
        let flight = Spiral::new(from, to, 3.0, shortest);

        // Every tick of it, not just the worst one that set the duration.
        for step in 0..200_u16 {
            let elapsed = shortest.mul_f32(f32::from(step) / 200.0);
            let velocity = flight
                .velocity_at(elapsed, rates())
                .expect("still within the flight");
            assert!(
                velocity.pan_speed <= pan_tilt::MAX_PAN_SPEED
                    && velocity.tilt_speed <= pan_tilt::MAX_TILT_SPEED,
                "asked for {velocity:?} at {elapsed:?}, which the head cannot do"
            );
        }
    }

    #[test]
    fn a_shape_with_more_winding_in_it_takes_longer_to_fly() {
        // The reach is the same both times; only the distance travelled
        // getting there differs, which is what sets the floor.
        let (from, to) = (at(-400, 200), at(100, -50));

        let brief = Spiral::shortest(from, to, 1.0, rates());
        let long = Spiral::shortest(from, to, 4.0, rates());

        assert!(long > brief);
    }

    #[test]
    fn a_faster_head_can_fly_the_same_shape_sooner() {
        let (from, to) = (at(-400, 200), at(100, -50));

        let brisk = Spiral::shortest(
            from,
            to,
            3.0,
            Rates {
                pan: 40.0,
                tilt: 40.0,
            },
        );
        let sluggish = Spiral::shortest(from, to, 3.0, rates());

        assert!(brisk < sluggish);
    }

    #[test]
    fn a_flight_carries_the_lens_it_is_sent_to() {
        // Nothing here drives a lens, so every point along the way reports the
        // destination's zoom and focus rather than inventing a sweep of them.
        let flight = Spiral::new(at(-400, 200), at(100, -50), 2.0, seconds(10));

        let midway = flight.at(seconds(5));

        assert_eq!(midway.zoom, flight.destination().zoom);
        assert_eq!(midway.focus, flight.destination().focus);
    }
}
