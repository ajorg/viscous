//! Continuous pan/tilt drive.
//!
//! Movement is expressed as a velocity the camera holds until it's told
//! otherwise, not as a series of discrete relative moves. A relative move
//! accelerates, travels its distance and decelerates, and its completion
//! only comes back once it has physically finished — so a control held down
//! turns into a queue of separate hops, each of which has to play out in
//! full even after the user has let go. One drive command and one stop
//! command produce the same sweep as a single smooth motion, with nothing
//! left queued behind them.

use std::ops::RangeInclusive;

use grafton_visca::{
    BlockingClient, Error,
    camera::profiles::GenericVisca,
    command::PanTiltDirection,
    transport::{BlockingTransport, HasTransportConfig},
    types::{PanSpeed, TiltSpeed},
};
use serde::{Deserialize, Serialize};

use crate::deflection;

/// The slowest speed VISCA's drive command accepts on either axis. Zero
/// isn't "stopped" — the command rejects it outright — so this is as slow as
/// movement gets, and stopping is [`Velocity::STOP`]'s job.
const MIN_SPEED: u8 = 1;

/// The fastest pan speed VISCA's drive command accepts (0x18).
pub const MAX_PAN_SPEED: u8 = 24;

/// The fastest tilt speed VISCA's drive command accepts (0x14 — genuinely a
/// shorter range than pan's).
pub const MAX_TILT_SPEED: u8 = 20;

/// The fastest speed a control asks for, on either axis.
///
/// Measured rather than chosen, on the camera this was written beside (an
/// EVI-class head, `examples/probe_speeds.rs`), in its own position units per
/// second and the same on both axes to within a few percent:
///
/// ```text
/// speed   1    2    3    4    5    6    7    8     9    10    11    12
/// rate   75  101  131  185  308  516  749  947  1237  1393  1597  1819
/// ```
///
/// Up to 9, each speed is a third to two thirds faster than the one below it:
/// nine steps that can each be told from its neighbours on a live picture.
/// Past 9 the table flattens to steps of about a seventh, which is close to
/// the least difference in speed an eye can see at all — speeds that would
/// each cost a share of a control's travel and give back nothing a hand could
/// use. So the range stops where the steps stop being worth having.
///
/// It was also never a speed for pointing at anything. The top of VISCA's
/// range swings past a subject before a hand can stop it; getting somewhere
/// far away in a hurry is what the presets and [`home`] are for, and they
/// arrive at the camera's own pace without anyone having to hold a stick over.
const TOP_SPEED: u8 = 9;

/// The pan speeds a control will actually ask for, slowest first. See
/// [`TOP_SPEED`] for where they stop and why.
pub const PAN_SPEEDS: RangeInclusive<u8> = MIN_SPEED..=TOP_SPEED;

/// The tilt speeds a control will actually ask for, slowest first.
///
/// The same as [`PAN_SPEEDS`], and not only for tidiness: a speed number is
/// worth the same rate on both axes, so sharing one range is what makes a
/// control pushed equally both ways travel at 45 degrees.
pub const TILT_SPEEDS: RangeInclusive<u8> = MIN_SPEED..=TOP_SPEED;

/// A continuous pan/tilt drive request: which way to drive, and how fast
/// along each axis.
///
/// This is VISCA's own encoding rather than a mirrored enum of our own,
/// because the shape only makes sense as VISCA defines it. The speed fields
/// have no "stopped" value while the direction field does, so a stop is a
/// direction; and both speeds go on the wire regardless of which axes the
/// direction actually moves, so an unused axis still needs a speed for the
/// camera to ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Velocity {
    /// Which way to drive, or [`PanTiltDirection::Stop`] to stop.
    pub direction: PanTiltDirection,
    /// How fast to pan, in VISCA's `1..=`[`MAX_PAN_SPEED`] range.
    pub pan_speed: u8,
    /// How fast to tilt, in VISCA's `1..=`[`MAX_TILT_SPEED`] range.
    pub tilt_speed: u8,
}

impl Velocity {
    /// Stop moving.
    pub const STOP: Self = Self {
        direction: PanTiltDirection::Stop,
        pan_speed: MIN_SPEED,
        tilt_speed: MIN_SPEED,
    };

    /// Whether this velocity ends movement rather than starting it.
    pub fn is_stop(self) -> bool {
        self.direction == PanTiltDirection::Stop
    }
}

/// The headings a push can be given off an axis, nearest the axis first, each
/// as how many speeds slower the axis it is pushed less along runs than the
/// other — `None` for that axis standing still, `Some(0)` for both alike.
///
/// On the [`STEP`] model a heading depends only on that difference, not on how
/// fast either axis is going, so these are the same headings at every speed:
/// about 14, 26, 35 and 45 degrees off the axis. Each is near enough ten
/// degrees from the next, which is about the narrowest stretch of a round
/// control a hand can choose to be in rather than happen to be.
///
/// The camera has more than these. Three speeds slower, at 19 degrees, sits
/// too close to its neighbours to have room of its own; five or more is
/// inside [`AXIS_SNAP_DEGREES`], where a push is taken to be along the axis.
const LAGS: [Option<u8>; 5] = [None, Some(4), Some(2), Some(1), Some(0)];

/// The headings a push in the `ring`th ring can be given: those of [`LAGS`]
/// the camera can drive near that ring's speed. Near the centre the axis
/// pushed less along would have to go slower than the slowest speed there is,
/// so the headings close to an axis only begin further out.
fn headings(ring: u8) -> Vec<Option<u8>> {
    LAGS.into_iter()
        .filter(|lag| lag.is_none_or(|lag| ring > lag))
        .collect()
}

/// How far a heading is off the axis it is nearest, in degrees.
fn degrees_off_axis(lag: Option<u8>) -> f32 {
    lag.map_or(0.0, |lag| STEP.powi(-i32::from(lag)).atan().to_degrees())
}

/// How many regions there are all the way round the `ring`th ring: one each
/// side of every heading off an axis, in each of the four quarters, and one
/// for each axis and each diagonal, which are shared between two quarters.
fn regions_around(ring: u8) -> usize {
    headings(ring)
        .into_iter()
        .map(|lag| match lag {
            Some(lag) if lag > 0 => 8,
            _ => 4,
        })
        .sum()
}

/// How fast the axis pushed further along goes, for a push in the `ring`th
/// ring at heading `lag`.
///
/// Whichever speed puts the two axes together nearest the ring's own speed:
/// a push on a diagonal moves the camera faster than either axis does alone,
/// so the axes are driven a little slower to make up for it. Never so slow
/// that the other axis would be asked for less than the slowest there is.
fn leading_speed(ring: u8, lag: Option<u8>) -> u8 {
    let Some(lag) = lag else {
        return ring;
    };
    let together = (1.0 + STEP.powi(-2 * i32::from(lag))).sqrt();
    let steps_faster = (together.ln() / STEP.ln()).round() as u8;
    ring.saturating_sub(steps_faster)
        .max(MIN_SPEED + lag)
        .min(TOP_SPEED)
}

/// How far from the centre of a round control each ring begins, innermost
/// first, as fractions of its radius. The `n`th ring is where a push along an
/// axis drives at speed `n`, and where a push in any direction is about as
/// fast in all as that.
///
/// Spaced so that every region of the control is about the same size. A ring
/// with more headings in it is cut into more regions, so it is given more of
/// the control to share between them — which leaves the slowest ring, with
/// only the axes and the diagonals, narrower than the bands of a rocker, and
/// every region inside it no smaller than any other.
pub fn ring_starts() -> Vec<f32> {
    let regions: Vec<f32> = PAN_SPEEDS.map(|ring| regions_around(ring) as f32).collect();
    let total: f32 = regions.iter().sum();
    let resting = deflection::DEADZONE.powi(2);
    let mut area = resting;
    regions
        .iter()
        .map(|share| {
            let begins = area.sqrt();
            area += (1.0 - resting) * share / total;
            begins
        })
        .collect()
}

/// Which ring a push `pushed` from the centre is in, or `None` while it is
/// still inside the deadzone.
fn ring_of(pushed: f32) -> Option<u8> {
    if pushed <= deflection::DEADZONE {
        return None;
    }
    let index = ring_starts().iter().rposition(|start| pushed >= *start)?;
    Some(MIN_SPEED + index as u8)
}

/// How finely a quarter of the control is searched for the edges of its
/// regions, and so how near a line is drawn to where the edge really is.
const STEPS_PER_DEGREE: usize = 8;

/// What a push `distance` from the centre at `degrees` up from straight right
/// asks for.
fn asked_at(distance: f32, degrees: f32) -> Velocity {
    let angle = degrees.to_radians();
    velocity_from_push(distance * angle.cos(), distance * angle.sin())
}

/// Halfway out across each of [`ring_starts`]'s rings.
fn ring_middles() -> Vec<f32> {
    let starts = ring_starts();
    let ends = starts.iter().skip(1).copied().chain([1.0]);
    starts
        .iter()
        .zip(ends)
        .map(|(start, end)| (start + end) / 2.0)
        .collect()
}

/// Within each of [`ring_starts`]'s rings, innermost first, the directions at
/// which a push starts asking for a different pair of speeds — in degrees up
/// from straight right, for the quarter of the control between right and up.
/// The other three quarters are that one mirrored.
///
/// With [`ring_edges`], these are the edges of every region of a round
/// control inside which the camera is asked for one and the same thing, for a
/// control that wants to draw them.
///
/// Found by asking [`velocity_from_push`] what it does all the way round each
/// ring, rather than worked out a second time from the rule it follows: a
/// drawing derived separately can come to disagree with the thing it is a
/// drawing of, and this cannot.
pub fn split_changes() -> Vec<Vec<f32>> {
    ring_middles()
        .into_iter()
        .map(|middle| {
            let asked = |step: usize| asked_at(middle, step as f32 / STEPS_PER_DEGREE as f32);
            (1..=90 * STEPS_PER_DEGREE)
                .filter(|step| asked(*step) != asked(step - 1))
                .map(|step| (step as f32 - 0.5) / STEPS_PER_DEGREE as f32)
                .collect()
        })
        .collect()
}

/// Along the inner edge of each of [`ring_starts`]'s rings, innermost first,
/// the stretches across which a push starts asking for something different —
/// each from one direction to another, in degrees up from straight right, for
/// the quarter between right and up.
///
/// Not the whole of every edge: at a heading whose speeds the next ring out
/// can't improve on, the region carries on across the ring as one, and
/// drawing a line through it would say there was a change where there isn't.
/// The innermost edge is always whole, since inside it is the deadzone.
pub fn ring_edges() -> Vec<Vec<(f32, f32)>> {
    let middles = ring_middles();
    (0..middles.len())
        .map(|ring| {
            if ring == 0 {
                return vec![(0.0, 90.0)];
            }
            let differs = |step: usize| {
                let degrees = (step as f32 + 0.5) / STEPS_PER_DEGREE as f32;
                asked_at(middles[ring - 1], degrees) != asked_at(middles[ring], degrees)
            };
            let mut stretches = Vec::new();
            let mut from = None;
            for step in 0..90 * STEPS_PER_DEGREE {
                match (differs(step), from) {
                    (true, None) => from = Some(step),
                    (false, Some(start)) => {
                        stretches.push((
                            start as f32 / STEPS_PER_DEGREE as f32,
                            step as f32 / STEPS_PER_DEGREE as f32,
                        ));
                        from = None;
                    }
                    _ => {}
                }
            }
            if let Some(start) = from {
                stretches.push((start as f32 / STEPS_PER_DEGREE as f32, 90.0));
            }
            stretches
        })
        .collect()
}

/// How much faster each speed is taken to be than the one below it.
///
/// A model, not a measurement, and deliberately so. Splitting a push between
/// two axes needs to know what the speed numbers are worth relative to one
/// another, and that differs from camera to camera; a table measured on one
/// would be exactly right there and unexplained anywhere else. This says only
/// that each speed is the same proportion faster than the last, which is how
/// the measured table behaves (see [`TOP_SPEED`]: sixteen and a half times
/// from 1 to 9, and no speed more than a fifth away from this) and how a
/// table has to behave if its steps are to look alike.
///
/// What it costs when it is wrong is small and of one kind: a diagonal comes
/// out at a slightly different angle than the push. Nothing is made faster or
/// slower by it — the speed comes from how far the control is pushed, which
/// needs only the order of the speeds — and a push along an axis doesn't
/// consult it at all.
const STEP: f32 = 1.42;

/// How near to straight along an axis a push has to be to be taken as
/// straight along it, in degrees either side.
///
/// A pan that stays level is the commonest move there is, and a hand cannot
/// hold a control to exactly no tilt. Without this every pan would carry
/// whatever tilt the hand's wobble asked for, at the slowest speed the camera
/// has, which is not slow. Wide enough to rest in without watching the
/// control; narrow enough that leaving it is a thing done on purpose.
pub const AXIS_SNAP_DEGREES: f32 = 9.0;

pub mod regions;

/// How a round control's push is turned into a velocity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PadLayout {
    /// Rings of speed, each cut into a slice for every heading:
    /// [`velocity_from_push`].
    #[default]
    Rings,
    /// The same commands, each given a region of exactly the same area as
    /// every other and whatever shape that takes: [`regions`].
    EqualAreas,
}

impl PadLayout {
    /// What a push `pan` right and `tilt` up asks for, each a fraction of the
    /// control's radius in `-1.0..=1.0`.
    pub fn velocity(self, pan: f32, tilt: f32) -> Velocity {
        match self {
            Self::Rings => velocity_from_push(pan, tilt),
            Self::EqualAreas => regions::velocity_at(pan, tilt),
        }
    }
}

/// Builds a velocity from where a round control is pushed to: `pan` and
/// `tilt` the push's two components, each a fraction of the control's radius
/// in `-1.0..=1.0` (positive being right and up).
///
/// **How far it is pushed says how fast; which way says where.** The control
/// is laid out as rings ([`ring_starts`]), each about one speed step faster
/// than the one inside it, and each ring is cut into regions, one for every
/// heading ([`LAGS`]) the camera can drive at about that ring's speed. A push
/// asks for the heading nearest the way it points, at the pair of speeds that
/// makes that ring's speed in all.
///
/// So the rim is the top speed whichever way the control is pushed — not so
/// when each axis picks its own speed from its own share of the push, where a
/// round control pushed to its rim on a diagonal is only seven tenths of the
/// way along either axis and goes a third as fast. And every region is a
/// heading near the push and a speed near the ring's, rather than whatever
/// rounding each axis on its own happens to give, which near the rim cut the
/// control into slivers too thin to stay in.
///
/// Near the centre there is less to choose from: the slowest speed is not very
/// slow, and an axis can't go slower than it, so the slowest ring has only the
/// axes and the diagonals.
///
/// Within [`AXIS_SNAP_DEGREES`] of an axis the push is taken to be along it.
pub fn velocity_from_push(pan: f32, tilt: f32) -> Velocity {
    let Some(ring) = ring_of(pan.hypot(tilt).min(1.0)) else {
        return Velocity::STOP;
    };

    let off_axis = pan
        .abs()
        .min(tilt.abs())
        .atan2(pan.abs().max(tilt.abs()))
        .to_degrees();
    let lag = if off_axis < AXIS_SNAP_DEGREES {
        None
    } else {
        let away = |lag: &Option<u8>| (degrees_off_axis(*lag) - off_axis).abs();
        headings(ring)
            .into_iter()
            .min_by(|one, other| away(one).total_cmp(&away(other)))
            .flatten()
    };

    let leading = leading_speed(ring, lag);
    let trailing = lag.map(|lag| leading - lag);
    let (pan_speed, tilt_speed) = if pan.abs() >= tilt.abs() {
        (Some(leading), trailing)
    } else {
        (trailing, Some(leading))
    };
    velocity(
        pan_speed.map(|speed| (speed, pan > 0.0)),
        tilt_speed.map(|speed| (speed, tilt > 0.0)),
    )
}

/// Puts a velocity together from what each axis is to do: its speed and
/// whether that is the positive way (right, up), or `None` to leave it be.
fn velocity(pan: Option<(u8, bool)>, tilt: Option<(u8, bool)>) -> Velocity {
    let direction = match (pan.map(|(_, right)| right), tilt.map(|(_, up)| up)) {
        (None, None) => return Velocity::STOP,
        (Some(true), None) => PanTiltDirection::Right,
        (Some(false), None) => PanTiltDirection::Left,
        (None, Some(true)) => PanTiltDirection::Up,
        (None, Some(false)) => PanTiltDirection::Down,
        (Some(true), Some(true)) => PanTiltDirection::UpRight,
        (Some(true), Some(false)) => PanTiltDirection::DownRight,
        (Some(false), Some(true)) => PanTiltDirection::UpLeft,
        (Some(false), Some(false)) => PanTiltDirection::DownLeft,
    };

    Velocity {
        direction,
        pan_speed: pan.map_or(MIN_SPEED, |(speed, _)| speed),
        tilt_speed: tilt.map_or(MIN_SPEED, |(speed, _)| speed),
    }
}

/// Maps one axis's deflection onto that axis's speed range, or `None` if it's
/// inside the deadzone.
fn axis_speed(deflection: f32, speeds: RangeInclusive<u8>) -> Option<u8> {
    deflection::speed(deflection.abs(), speeds)
}

/// Builds a velocity from a control's deflection along each axis, each a
/// fraction of full travel in `-1.0..=1.0` (positive being right and up).
///
/// The two axes scale independently, so a control pushed hard right and a
/// little up pans fast while tilting slowly: the diagonals cover a whole
/// range of arcs rather than a fixed 45 degrees.
///
/// For controls that really are two separate axes — a pair of arrow keys,
/// each either held or not. A control that is one thing pushed in a
/// direction wants [`velocity_from_push`] instead.
pub fn velocity_from_axes(pan: f32, tilt: f32) -> Velocity {
    velocity(
        axis_speed(pan, PAN_SPEEDS).map(|speed| (speed, pan > 0.0)),
        axis_speed(tilt, TILT_SPEEDS).map(|speed| (speed, tilt > 0.0)),
    )
}

/// Sets the camera's continuous pan/tilt drive, or stops it.
///
/// A stop goes out as a drive in the `Stop` direction rather than through the
/// library's dedicated stop call, because that is the same command with the
/// same bytes — one code path covers both.
pub fn drive<T>(camera: &BlockingClient<GenericVisca, T>, velocity: Velocity) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.pan_tilt_move(
        velocity.direction,
        PanSpeed::new(velocity.pan_speed)?,
        TiltSpeed::new(velocity.tilt_speed)?,
    )
}

/// Sends the camera to its home position.
pub fn home<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.pan_tilt_home()
}

/// Recalibrates the pan/tilt mechanism, ending at the home position.
///
/// Worth having next to [`home`], which it otherwise looks like: a camera
/// that has been turned by hand, or has lost steps against an obstruction,
/// answers position inquiries with numbers that no longer match where it is
/// actually pointing — and every saved preset is expressed in those numbers.
/// The reset re-finds the mechanical limits and re-zeroes them, at the cost
/// of a full sweep to get there.
pub fn reset<T>(camera: &BlockingClient<GenericVisca, T>) -> Result<(), Error>
where
    T: BlockingTransport + HasTransportConfig + 'static,
{
    camera.pan_tilt_reset()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::testing::testkit::{ScriptedBlockingTransport, helpers};

    #[test]
    fn home_and_reset_each_reach_the_camera() {
        let acks = || scripted_camera(vec![helpers::standard_command_response(1)]);

        home(&acks()).expect("scripted camera should ack home");
        reset(&acks()).expect("scripted camera should ack reset");
    }

    /// What a speed is worth, relative to the slowest, on the [`STEP`] model.
    fn modelled_rate(speed: u8) -> f32 {
        STEP.powi(i32::from(speed) - i32::from(MIN_SPEED))
    }

    /// How fast a velocity is going in all, on the model the push is split
    /// by, and which way in degrees up from pan.
    fn modelled(velocity: Velocity) -> (f32, f32) {
        use PanTiltDirection::{Down, Left, Right, Up};
        let pan = match velocity.direction {
            Up | Down => 0.0,
            _ => modelled_rate(velocity.pan_speed),
        };
        let tilt = match velocity.direction {
            Left | Right => 0.0,
            _ => modelled_rate(velocity.tilt_speed),
        };
        (pan.hypot(tilt), tilt.atan2(pan).to_degrees())
    }

    /// A push of `distance` at `degrees` up from straight right.
    fn push(distance: f32, degrees: f32) -> Velocity {
        let angle = degrees.to_radians();
        velocity_from_push(distance * angle.cos(), distance * angle.sin())
    }

    #[test]
    fn the_rim_is_the_top_speed_whichever_way_it_is_pushed() {
        // The fault this exists to fix: with each axis picking its own speed,
        // a round control at its rim on a diagonal went a third as fast as
        // one pushed straight.
        let (straight, _) = modelled(push(1.0, 0.0));
        for degrees in (0..360).step_by(5) {
            let (rate, _) = modelled(push(1.0, degrees as f32));
            assert!(
                (0.8..=1.25).contains(&(rate / straight)),
                "a full push at {degrees} degrees went {rate}, against {straight} straight"
            );
        }
    }

    #[test]
    fn pushing_further_never_goes_slower_whichever_way() {
        for degrees in [0.0, 20.0, 45.0, 70.0, 90.0] {
            let mut previous = 0.0;
            for step in 11..=100 {
                let (rate, _) = modelled(push(step as f32 / 100.0, degrees));
                assert!(
                    rate >= previous * 0.99,
                    "at {degrees} degrees, {step}% of a push went slower than less of one"
                );
                previous = rate;
            }
        }
    }

    #[test]
    fn a_push_along_an_axis_goes_at_the_speed_of_its_ring() {
        let mut reached = Vec::new();
        for step in 11..=100 {
            let distance = step as f32 / 100.0;
            let velocity = push(distance, 0.0);
            assert_eq!(velocity.direction, PanTiltDirection::Right);
            assert_eq!(Some(velocity.pan_speed), ring_of(distance));
            reached.push(velocity.pan_speed);
        }
        reached.dedup();
        assert_eq!(reached, PAN_SPEEDS.collect::<Vec<_>>());
    }

    #[test]
    fn every_region_of_the_pad_is_about_the_same_size() {
        // The point of laying the pad out this way: a region is a thing a hand
        // aims for, and one much smaller than the rest is one it can't hold.
        // Counted over a grid rather than worked out, so that it says what
        // the pad does and not what it was meant to.
        use std::collections::HashMap;
        const ACROSS: i32 = 300;
        let mut sizes: HashMap<String, usize> = HashMap::new();
        for x in -ACROSS..=ACROSS {
            for y in -ACROSS..=ACROSS {
                let (pan, tilt) = (x as f32 / ACROSS as f32, y as f32 / ACROSS as f32);
                if pan.hypot(tilt) > 1.0 {
                    continue;
                }
                let velocity = velocity_from_push(pan, tilt);
                if !velocity.is_stop() {
                    *sizes.entry(format!("{velocity:?}")).or_default() += 1;
                }
            }
        }
        let mut sizes: Vec<usize> = sizes.into_values().collect();
        sizes.sort_unstable();
        let median = sizes[sizes.len() / 2];
        let (smallest, largest) = (sizes[0], sizes[sizes.len() - 1]);
        assert!(
            smallest * 2 >= median,
            "the smallest region is {smallest} against a median of {median}"
        );
        assert!(
            largest <= median * 5 / 2,
            "the largest region is {largest} against a median of {median}"
        );
    }

    #[test]
    fn the_regions_near_the_rim_are_not_slivers() {
        // What this layout replaced: rounding each axis on its own cut the
        // outer rings into some sixty pieces a quarter turn.
        let changes = split_changes();
        let rim = changes.last().unwrap();
        assert!(
            rim.len() <= 8,
            "the rim is cut {} times a quarter",
            rim.len()
        );
        for pair in rim.windows(2) {
            assert!(
                pair[1] - pair[0] >= 8.0,
                "a region at the rim is only {} degrees wide",
                pair[1] - pair[0]
            );
        }
    }

    #[test]
    fn a_ring_edge_is_drawn_only_where_something_changes_across_it() {
        let edges = ring_edges();
        assert_eq!(edges.len(), PAN_SPEEDS.count());
        assert_eq!(edges[0], vec![(0.0, 90.0)], "the deadzone's edge is whole");
        // Along an axis the speed goes up a step every ring, so every edge
        // crosses it.
        for (ring, stretches) in edges.iter().enumerate() {
            assert!(
                stretches.iter().any(|(from, _)| *from == 0.0),
                "ring {ring}'s edge doesn't cross the axis: {stretches:?}"
            );
        }
        // On the diagonal the two slowest rings ask for the same — both axes
        // at their slowest — so nothing divides them there.
        assert!(
            !edges[1]
                .iter()
                .any(|(from, to)| (*from..=*to).contains(&45.0)),
            "{:?}",
            edges[1]
        );
    }

    #[test]
    fn a_push_near_enough_to_an_axis_stays_on_it() {
        // A level pan, held by a hand that cannot hold a control dead level.
        for distance in [0.2, 0.6, 1.0] {
            for degrees in [-8.0, -3.0, 3.0, 8.0] {
                assert_eq!(push(distance, degrees).direction, PanTiltDirection::Right);
                assert_eq!(
                    push(distance, 90.0 + degrees).direction,
                    PanTiltDirection::Up
                );
            }
        }
    }

    #[test]
    fn a_full_push_goes_roughly_the_way_it_is_pushed() {
        // Roughly, because the camera has nine speeds an axis and the way a
        // push goes is made of two of them. At the rim there is the most to
        // choose from, and the direction should be within half the widest
        // gap between two headings.
        for degrees in (10..=80).step_by(5) {
            let (_, went) = modelled(push(1.0, degrees as f32));
            assert!(
                (went - degrees as f32).abs() <= 6.5,
                "a full push at {degrees} degrees went at {went}"
            );
        }
    }

    #[test]
    fn a_push_halfway_between_the_axes_drives_both_alike() {
        for step in 11..=100 {
            let velocity = push(step as f32 / 100.0, 45.0);
            assert_eq!(velocity.direction, PanTiltDirection::UpRight);
            assert_eq!(velocity.pan_speed, velocity.tilt_speed);
        }
    }

    #[test]
    fn a_push_goes_the_way_it_points_in_every_quarter() {
        assert_eq!(push(0.8, 45.0).direction, PanTiltDirection::UpRight);
        assert_eq!(push(0.8, 135.0).direction, PanTiltDirection::UpLeft);
        assert_eq!(push(0.8, 225.0).direction, PanTiltDirection::DownLeft);
        assert_eq!(push(0.8, 315.0).direction, PanTiltDirection::DownRight);
        assert_eq!(push(0.8, 180.0).direction, PanTiltDirection::Left);
        assert_eq!(push(0.8, 270.0).direction, PanTiltDirection::Down);
    }

    #[test]
    fn a_push_is_at_rest_inside_a_round_deadzone() {
        assert!(velocity_from_push(0.0, 0.0).is_stop());
        assert!(velocity_from_push(0.06, 0.06).is_stop());
        assert!(!velocity_from_push(0.08, 0.08).is_stop());
    }

    #[test]
    fn the_regions_of_a_ring_are_the_same_either_side_of_the_diagonal() {
        // Pan and tilt are driven from the same speeds, so whatever happens
        // going up from right happens going right from up.
        for changes in split_changes() {
            for degrees in &changes {
                assert!(
                    changes
                        .iter()
                        .any(|other| (other - (90.0 - degrees)).abs() < 0.5),
                    "a change at {degrees} degrees has no partner in {changes:?}"
                );
            }
        }
    }

    #[test]
    fn the_slowest_ring_has_only_the_axes_and_the_diagonal() {
        // Along one, along the other, or both at their slowest: three
        // regions a quarter, so two edges between them.
        let changes = split_changes();
        assert_eq!(changes.len(), PAN_SPEEDS.count());
        assert_eq!(changes[0].len(), 2, "{:?}", changes[0]);
    }

    #[test]
    fn the_outermost_ring_leaves_an_axis_where_the_snap_ends() {
        let changes = split_changes();
        let first = changes.last().unwrap()[0];
        assert!(
            (first - AXIS_SNAP_DEGREES).abs() < 0.5,
            "the first change at the rim is at {first} degrees"
        );
    }

    #[test]
    fn there_is_more_to_choose_from_further_out() {
        let changes = split_changes();
        assert!(changes.last().unwrap().len() > changes[0].len());
    }

    #[test]
    fn a_stick_pushed_into_a_corner_is_a_full_push_and_no_more() {
        // A square-gated stick reads past the circle in its corners.
        assert_eq!(velocity_from_push(1.0, 1.0), push(1.0, 45.0));
    }

    #[test]
    fn the_model_is_not_far_from_the_camera_it_was_fitted_to() {
        // Units a second, measured (`examples/probe_speeds.rs`). The model
        // only has to keep a diagonal within a few degrees of where it was
        // pushed, which a fifth out in either direction does.
        let measured = [
            75.0, 101.0, 131.0, 185.0, 308.0, 516.0, 749.0, 947.0, 1237.0,
        ];
        for (speed, rate) in (1..=9).zip(measured) {
            let modelled = modelled_rate(speed) * measured[0];
            assert!(
                (0.8..=1.25).contains(&(modelled / rate)),
                "speed {speed} is modelled at {modelled}, measured at {rate}"
            );
        }
    }

    #[test]
    fn a_centered_control_stops() {
        assert_eq!(velocity_from_axes(0.0, 0.0), Velocity::STOP);
        assert!(Velocity::STOP.is_stop());
    }

    #[test]
    fn deflection_inside_the_deadzone_stops() {
        assert_eq!(velocity_from_axes(0.05, -0.05), Velocity::STOP);
    }

    #[test]
    fn the_deadzone_is_the_one_every_other_control_uses() {
        // A hand that has learned the rockers has learned the pad.
        assert_eq!(
            velocity_from_axes(deflection::DEADZONE, 0.0),
            Velocity::STOP
        );
    }

    #[test]
    fn a_single_axis_drives_a_cardinal_direction() {
        assert_eq!(
            velocity_from_axes(1.0, 0.0).direction,
            PanTiltDirection::Right
        );
        assert_eq!(
            velocity_from_axes(-1.0, 0.0).direction,
            PanTiltDirection::Left
        );
        assert_eq!(velocity_from_axes(0.0, 1.0).direction, PanTiltDirection::Up);
        assert_eq!(
            velocity_from_axes(0.0, -1.0).direction,
            PanTiltDirection::Down
        );
    }

    #[test]
    fn both_axes_drive_a_diagonal() {
        assert_eq!(
            velocity_from_axes(1.0, 1.0).direction,
            PanTiltDirection::UpRight
        );
        assert_eq!(
            velocity_from_axes(1.0, -1.0).direction,
            PanTiltDirection::DownRight
        );
        assert_eq!(
            velocity_from_axes(-1.0, 1.0).direction,
            PanTiltDirection::UpLeft
        );
        assert_eq!(
            velocity_from_axes(-1.0, -1.0).direction,
            PanTiltDirection::DownLeft
        );
    }

    #[test]
    fn full_deflection_asks_for_the_top_speed_of_each_axis() {
        let velocity = velocity_from_axes(1.0, 1.0);
        assert_eq!(velocity.pan_speed, *PAN_SPEEDS.end());
        assert_eq!(velocity.tilt_speed, *TILT_SPEEDS.end());
    }

    #[test]
    fn the_fastest_a_control_asks_for_is_well_short_of_what_the_camera_can_do() {
        // Measured on the camera: driven at the top of VISCA's range it swings
        // past anything worth looking at, so no amount of pushing reaches it.
        assert!(
            *PAN_SPEEDS.end() <= MAX_PAN_SPEED / 2,
            "pushing the pad flat out asked for {} of the camera's {MAX_PAN_SPEED}",
            *PAN_SPEEDS.end()
        );
        assert!(
            *TILT_SPEEDS.end() <= MAX_TILT_SPEED / 2,
            "pushing the pad flat out asked for {} of the camera's {MAX_TILT_SPEED}",
            *TILT_SPEEDS.end()
        );
    }

    #[test]
    fn deflection_just_past_the_deadzone_asks_for_the_slowest_speed() {
        assert_eq!(
            velocity_from_axes(deflection::DEADZONE + 0.001, 0.0).pan_speed,
            MIN_SPEED
        );
    }

    #[test]
    fn the_slow_half_of_the_pad_stays_in_the_slow_speeds() {
        // What makes the pad usable for framing rather than only for getting
        // somewhere. The speed numbers understate it: the third speed is a
        // fourteenth of the rate of the ninth, so half the travel is spent
        // below a tenth of the fastest the pad can go.
        for step in 11..=50 {
            let speed = velocity_from_axes(step as f32 / 100.0, 0.0).pan_speed;
            assert!(
                speed * 3 <= *PAN_SPEEDS.end(),
                "{step}% of full push asked for {speed} of {}",
                *PAN_SPEEDS.end()
            );
        }
    }

    #[test]
    fn both_axes_are_driven_from_the_same_speeds() {
        // A speed number is the same rate on either axis, so a push that is
        // equal both ways only travels at 45 degrees if it asks each for the
        // same number.
        assert_eq!(PAN_SPEEDS, TILT_SPEEDS);
        for step in 11..=100 {
            let push = step as f32 / 100.0;
            let velocity = velocity_from_axes(push, push);
            assert_eq!(velocity.pan_speed, velocity.tilt_speed);
        }
    }

    #[test]
    fn speed_scales_with_deflection() {
        let gentle = velocity_from_axes(0.4, 0.0).pan_speed;
        let firm = velocity_from_axes(0.9, 0.0).pan_speed;
        assert!(
            gentle < firm,
            "a further push should ask for a faster pan ({gentle} vs {firm})"
        );
    }

    #[test]
    fn the_axes_scale_independently_so_diagonals_are_not_fixed_at_45_degrees() {
        // Hard right, barely up: pan should be at its own maximum while tilt
        // stays near the bottom of its range.
        let velocity = velocity_from_axes(1.0, 0.2);
        assert_eq!(velocity.pan_speed, *PAN_SPEEDS.end());
        assert!(
            velocity.tilt_speed < *TILT_SPEEDS.end() / 2,
            "tilt should stay slow when the control is barely deflected up"
        );
    }

    #[test]
    fn an_unused_axis_still_carries_a_speed_the_camera_will_accept() {
        // VISCA rejects a zero speed on either axis even when the direction
        // doesn't move that axis, so a pure tilt still needs a pan speed.
        assert!(velocity_from_axes(0.0, 1.0).pan_speed >= MIN_SPEED);
    }

    #[test]
    fn deflection_beyond_full_travel_is_capped_rather_than_rejected() {
        assert_eq!(velocity_from_axes(3.0, 0.0).pan_speed, *PAN_SPEEDS.end());
    }

    fn scripted_camera(
        replies: Vec<grafton_visca::testing::testkit::Step>,
    ) -> BlockingClient<GenericVisca, ScriptedBlockingTransport> {
        grafton_visca::CameraBuilder::new()
            .build_blocking::<GenericVisca, _>(ScriptedBlockingTransport::new(replies))
            .expect("camera should build from a scripted transport")
    }

    #[test]
    fn drive_sends_a_move_command() {
        let camera = scripted_camera(vec![helpers::standard_command_response(1)]);
        drive(&camera, velocity_from_axes(1.0, 1.0))
            .expect("scripted camera should ack and complete the drive");
    }

    #[test]
    fn drive_sends_a_stop_command() {
        let camera = scripted_camera(vec![helpers::standard_command_response(1)]);
        drive(&camera, Velocity::STOP).expect("scripted camera should ack and complete the stop");
    }

    #[test]
    fn drive_reports_an_out_of_range_speed_rather_than_sending_it() {
        let camera = scripted_camera(vec![]);
        let too_fast = Velocity {
            pan_speed: MAX_PAN_SPEED + 1,
            ..velocity_from_axes(1.0, 0.0)
        };
        assert!(drive(&camera, too_fast).is_err());
    }
}
