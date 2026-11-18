//! The pad cut into regions of exactly equal area, as round as they can be
//! made, one for every pair of speeds worth having.
//!
//! An experiment beside the rings of [`super::velocity_from_push`], which
//! keep every region to a ring and a slice of one and so can only make them
//! about the same size.
//!
//! Laid out in four stages, the first time anything asks:
//!
//! 1. **A home for every pair of speeds.** Each pair the ranges allow goes in
//!    the direction the camera moves when it is sent, and out from the centre
//!    in proportion to how fast it goes — on the log scale speeds are seen on
//!    — against the fastest the pad goes that way. That fastest is pan's top
//!    speed at the sides and tilt's at the top and bottom, with an even blend
//!    between, so with equal ranges the rim is one speed all the way round.
//! 2. **Only the pairs with room.** A pair is kept only if its home is at
//!    least about one speed step from every home already kept — spacing on
//!    the pad itself, so that a region can be as deep as it is wide. Near the
//!    rim that keeps directions only a few degrees apart; near the centre,
//!    few. Every speed along each axis is kept regardless.
//! 3. **Equal areas.** A push asks for the pair whose home is nearest,
//!    weighed: each home carries a weight that grows or shrinks its region.
//!    The weights that give every region the same area are found by the
//!    damped Newton method of Kitagawa, Mérigot and Thibert (*Convergence of
//!    a Newton algorithm for semi-discrete optimal transport*, 2019), which is
//!    proven to get there for any set of homes and takes a handful of steps.
//!    Every region of such a power diagram is a convex polygon.
//! 4. **Rounder.** Each home is drawn part of the way toward the middle of
//!    its region and the areas evened again, a few dozen times — the bubble
//!    relaxation of capacity-constrained centroidal power diagrams, held back
//!    so a home doesn't wander far from the direction it stands for.
//!
//! Mirror symmetry left to right and top to bottom — and across the
//! diagonals, when the ranges are equal — is imposed at every step rather
//! than hoped for.

use std::{collections::HashMap, f64::consts::PI, sync::OnceLock};

use super::{PAN_SPEEDS, STEP, TILT_SPEEDS, Velocity, velocity};
use crate::deflection::DEADZONE;

/// A point on the pad, in fractions of its radius: right, then up.
pub type Point = (f32, f32);

/// The same, worked in double precision.
type Spot = (f64, f64);

/// One region of the pad: what it asks for, and where it is.
#[derive(Debug, Clone)]
pub struct Region {
    /// What a push anywhere in this region asks the camera for.
    pub velocity: Velocity,
    /// Its edge, anticlockwise. Includes whatever of it lies inside the
    /// deadzone, where the pad asks for nothing whatever region it is in.
    pub outline: Vec<Point>,
    /// Where on the pad this pair of speeds belongs, after relaxing.
    home: Spot,
    /// How much nearer than it is this region's home is taken to be.
    weight: f64,
}

/// How many sides the polygons standing in for the rim and the deadzone have:
/// enough that no eye or hand tells either from a circle.
const RIM_SIDES: usize = 256;
const DEADZONE_SIDES: usize = 64;

/// How near two homes may be and both be kept, as a share of one speed step
/// along the radius. A little under a step, so that the steps along an axis
/// don't crowd out everything beside them.
const SPACING: f64 = 0.9;

/// How much faster than the rim's speed a pair may be and still be kept, in
/// steps — so one at very nearly the rim's speed isn't lost to rounding.
const RIM_SLACK: f64 = 0.1;

/// How far each home is drawn toward the middle of its region, and how many
/// times. All the way makes the roundest regions and the least faithful
/// directions; this is a little over half.
const PULL: f64 = 0.6;
const RELAXATIONS: usize = 40;

/// How many of its nearest neighbours a region is cut by. Regions are all
/// about the same size, so nothing further off ever shares an edge.
const NEIGHBOURS: usize = 40;

/// How near to equal the areas are made, as a share of each, and the most
/// Newton steps one solve is given.
const TOLERANCE: f64 = 1e-4;
const NEWTON_STEPS: usize = 60;

/// Every region of the pad, worked out the first time anything asks.
pub fn regions() -> &'static [Region] {
    static REGIONS: OnceLock<Vec<Region>> = OnceLock::new();
    REGIONS.get_or_init(laid_out)
}

/// What a push `pan` right and `tilt` up asks for, each a fraction of the
/// pad's radius.
pub(super) fn velocity_at(pan: f32, tilt: f32) -> Velocity {
    let pushed = pan.hypot(tilt);
    if pushed <= DEADZONE {
        return Velocity::STOP;
    }
    // A stick pushed into its corners reads past the rim; it is at the rim.
    let scale = f64::from(pushed.max(1.0));
    let at = (f64::from(pan) / scale, f64::from(tilt) / scale);
    regions()
        .iter()
        .min_by(|one, other| {
            power(one.home, one.weight, at).total_cmp(&power(other.home, other.weight, at))
        })
        .map_or(Velocity::STOP, |region| region.velocity)
}

/// How far `at` is from `home`, weighed: squared distance, less the weight.
fn power(home: Spot, weight: f64, at: Spot) -> f64 {
    (at.0 - home.0).powi(2) + (at.1 - home.1).powi(2) - weight
}

// ---- which pairs of speeds, and where they belong ------------------------

/// A pair of speeds, signed: positive right and up, nought for an axis left
/// alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Speeds {
    pan: i32,
    tilt: i32,
}

impl Speeds {
    fn is_axis(self) -> bool {
        self.pan == 0 || self.tilt == 0
    }

    /// How fast each axis goes, on the model: each speed [`STEP`] times the
    /// one below.
    fn motion(self) -> Spot {
        (rate(self.pan), rate(self.tilt))
    }

    /// How fast it goes in all, in steps, nought being the slowest one axis
    /// can go alone.
    fn level(self) -> f64 {
        let (x, y) = self.motion();
        x.hypot(y).ln() / f64::from(STEP).ln()
    }

    /// Which way it goes, anticlockwise from straight right.
    fn heading(self) -> f64 {
        let (x, y) = self.motion();
        y.atan2(x)
    }

    fn velocity(self) -> Velocity {
        let axis = |speed: i32| (speed != 0).then(|| (speed.unsigned_abs() as u8, speed > 0));
        velocity(axis(self.pan), axis(self.tilt))
    }
}

fn rate(speed: i32) -> f64 {
    if speed == 0 {
        return 0.0;
    }
    f64::from(STEP).powi(speed.abs() - 1) * f64::from(speed.signum())
}

/// The fastest speed each axis is driven at.
fn tops() -> (i32, i32) {
    (i32::from(*PAN_SPEEDS.end()), i32::from(*TILT_SPEEDS.end()))
}

/// The fastest the ranges allow in direction `angle`, in steps.
fn reachable(angle: f64) -> f64 {
    let (pan, tilt) = tops();
    let (across, up) = (angle.cos().abs(), angle.sin().abs());
    let reach = match (across > 1e-9, up > 1e-9) {
        (true, true) => (rate(pan) / across).min(rate(tilt) / up),
        (true, false) => rate(pan) / across,
        _ => rate(tilt) / up,
    };
    reach.ln() / f64::from(STEP).ln()
}

/// How fast the rim is in direction `angle`, in steps: pan's top speed at the
/// sides, tilt's at the top and bottom, and an even blend of the two between —
/// never more than the camera can do that way.
fn rim(angle: f64) -> f64 {
    let (pan, tilt) = tops();
    let blend =
        f64::from(pan - 1) * angle.cos().powi(2) + f64::from(tilt - 1) * angle.sin().powi(2);
    blend.min(reachable(angle))
}

/// Where a pair of speeds belongs on the pad: in its own direction, and as
/// far out as its speed is of the rim's that way, every step of speed being
/// an equal distance along the radius.
fn home_of(speeds: Speeds) -> Spot {
    let heading = speeds.heading();
    let out = ((speeds.level() + 0.5) / (rim(heading) + 1.0)).clamp(0.0, 1.0);
    let reach = f64::from(DEADZONE) + (1.0 - f64::from(DEADZONE)) * out;
    (reach * heading.cos(), reach * heading.sin())
}

/// How far apart one speed step puts two homes along the radius, at the
/// faster axis's spacing.
fn radial_step() -> f64 {
    let (pan, tilt) = tops();
    (1.0 - f64::from(DEADZONE)) / f64::from(pan.max(tilt))
}

/// The pad's symmetries, as matrices acting alike on points and on pairs of
/// speeds taken as vectors.
type Symmetry = [[i32; 2]; 2];

fn symmetries() -> Vec<Symmetry> {
    let mut all = vec![
        [[1, 0], [0, 1]],
        [[-1, 0], [0, 1]],
        [[1, 0], [0, -1]],
        [[-1, 0], [0, -1]],
    ];
    let (pan, tilt) = tops();
    if pan == tilt {
        let swapped: Vec<Symmetry> = all
            .iter()
            .map(|m| [[m[0][1], m[0][0]], [m[1][1], m[1][0]]])
            .collect();
        all.extend(swapped);
    }
    all
}

fn mirrored(symmetry: &Symmetry, speeds: Speeds) -> Speeds {
    Speeds {
        pan: symmetry[0][0] * speeds.pan + symmetry[0][1] * speeds.tilt,
        tilt: symmetry[1][0] * speeds.pan + symmetry[1][1] * speeds.tilt,
    }
}

/// A symmetry undone on a point: its transpose, which is its inverse.
fn unmirrored(symmetry: &Symmetry, spot: Spot) -> Spot {
    (
        f64::from(symmetry[0][0]) * spot.0 + f64::from(symmetry[1][0]) * spot.1,
        f64::from(symmetry[0][1]) * spot.0 + f64::from(symmetry[1][1]) * spot.1,
    )
}

/// Every pair of speeds given a region, a whole set of mirror images at a
/// time: the axes first, then slowest first, and at one speed nearest an
/// axis first.
fn chosen() -> Vec<Speeds> {
    let (pan_top, tilt_top) = tops();
    let group = symmetries();
    let mut seen = std::collections::HashSet::new();
    let mut sets: Vec<Vec<Speeds>> = Vec::new();
    for pan in -pan_top..=pan_top {
        for tilt in -tilt_top..=tilt_top {
            let speeds = Speeds { pan, tilt };
            if (pan == 0 && tilt == 0) || seen.contains(&speeds) {
                continue;
            }
            if !speeds.is_axis() && speeds.level() > rim(speeds.heading()) + RIM_SLACK {
                continue;
            }
            let mut set: Vec<Speeds> = group.iter().map(|m| mirrored(m, speeds)).collect();
            set.sort_by_key(|speeds| (speeds.pan, speeds.tilt));
            set.dedup();
            seen.extend(set.iter().copied());
            sets.push(set);
        }
    }
    let off_axis = |speeds: Speeds| {
        let (x, y) = speeds.motion();
        x.abs().min(y.abs()).atan2(x.abs().max(y.abs()))
    };
    let key = |speeds: Speeds| (!speeds.is_axis(), speeds.level(), off_axis(speeds));
    sets.sort_by(|one, other| key(one[0]).partial_cmp(&key(other[0])).expect("no NaN"));

    let least = SPACING * radial_step();
    let crowds = |one: Speeds, other: Speeds| {
        let (a, b) = (home_of(one), home_of(other));
        (a.0 - b.0).hypot(a.1 - b.1) < least
    };
    let mut kept: Vec<Speeds> = Vec::new();
    for set in sets {
        let fits = set.iter().enumerate().all(|(i, one)| {
            set[i + 1..].iter().all(|other| !crowds(*one, *other))
                && kept.iter().all(|other| !crowds(*one, *other))
        });
        if set[0].is_axis() || fits {
            kept.extend(set);
        }
    }
    kept
}

// ---- the power diagram ---------------------------------------------------

/// What cut an edge of a region: the home on its other side, or nothing — the
/// rim.
type Edge = Option<usize>;

fn circle(radius: f64, sides: usize) -> Vec<Spot> {
    (0..sides)
        .map(|side| {
            let angle = side as f64 / sides as f64 * 2.0 * PI;
            (radius * angle.cos(), radius * angle.sin())
        })
        .collect()
}

/// The part of a convex `polygon` where `normal · point ≤ bound`, the new edge
/// along that line marked as cut by `by`.
fn clip(polygon: &[(Spot, Edge)], normal: Spot, bound: f64, by: Edge) -> Vec<(Spot, Edge)> {
    let over = |spot: &Spot| normal.0 * spot.0 + normal.1 * spot.1 - bound;
    let mut kept = Vec::with_capacity(polygon.len() + 1);
    for ((from, edge), (to, _)) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
        let (a, b) = (over(from), over(to));
        let crossing = || {
            let part = a / (a - b);
            (
                from.0 + (to.0 - from.0) * part,
                from.1 + (to.1 - from.1) * part,
            )
        };
        match (a <= 0.0, b <= 0.0) {
            (true, true) => kept.push((*from, *edge)),
            // Leaving: the edge runs on to the line, then the line takes over.
            (true, false) => {
                kept.push((*from, *edge));
                kept.push((crossing(), by));
            }
            // Coming back: from the line, the rest of the old edge.
            (false, true) => kept.push((crossing(), *edge)),
            (false, false) => {}
        }
    }
    kept
}

/// A polygon's area and centroid.
fn moments(polygon: &[Spot]) -> (f64, Spot) {
    let (mut area, mut x, mut y) = (0.0, 0.0, 0.0);
    for (from, to) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
        let cross = from.0 * to.1 - to.0 * from.1;
        area += cross;
        x += (from.0 + to.0) * cross;
        y += (from.1 + to.1) * cross;
    }
    if area.abs() < 1e-15 {
        return (0.0, (0.0, 0.0));
    }
    (area / 2.0, (x / (3.0 * area), y / (3.0 * area)))
}

/// The area and centroid of the part of `polygon` outside the deadzone.
fn outside(polygon: &[Spot], deadzone: &[Spot]) -> (f64, Spot) {
    let (whole, middle) = moments(polygon);
    let mut inside: Vec<(Spot, Edge)> = polygon.iter().map(|spot| (*spot, None)).collect();
    for (from, to) in deadzone.iter().zip(deadzone.iter().cycle().skip(1)) {
        // Anticlockwise, so inside is to the left of every edge.
        let normal = (to.1 - from.1, from.0 - to.0);
        inside = clip(&inside, normal, normal.0 * from.0 + normal.1 * from.1, None);
        if inside.is_empty() {
            break;
        }
    }
    let inside: Vec<Spot> = inside.iter().map(|(spot, _)| *spot).collect();
    let (hidden, hidden_middle) = moments(&inside);
    let area = whole - hidden;
    if area <= 1e-15 {
        return (0.0, middle);
    }
    (
        area,
        (
            (middle.0 * whole - hidden_middle.0 * hidden) / area,
            (middle.1 * whole - hidden_middle.1 * hidden) / area,
        ),
    )
}

/// How much of the segment from `a` to `b` lies outside the deadzone.
fn length_outside(a: Spot, b: Spot) -> f64 {
    let along = (b.0 - a.0, b.1 - a.1);
    let length = along.0.hypot(along.1);
    let radius = f64::from(DEADZONE);
    let qa = along.0 * along.0 + along.1 * along.1;
    let qb = 2.0 * (a.0 * along.0 + a.1 * along.1);
    let qc = a.0 * a.0 + a.1 * a.1 - radius * radius;
    let discriminant = qb * qb - 4.0 * qa * qc;
    if qa < 1e-18 || discriminant <= 0.0 {
        return length;
    }
    let root = discriminant.sqrt();
    let enters = ((-qb - root) / (2.0 * qa)).max(0.0);
    let leaves = ((-qb + root) / (2.0 * qa)).min(1.0);
    length * (1.0 - (leaves - enters).max(0.0))
}

/// One power diagram, and what the Newton step needs to know about it.
struct Diagram {
    cells: Vec<Vec<(Spot, Edge)>>,
    areas: Vec<f64>,
    centroids: Vec<Spot>,
    /// For each region, each neighbour and how much area the region would
    /// gain from it for a unit of its own weight: the length of the edge they
    /// share, over twice the distance between their homes.
    links: Vec<Vec<(usize, f64)>>,
}

fn diagram(
    homes: &[Spot],
    weights: &[f64],
    nearest: &[Vec<usize>],
    rim: &[(Spot, Edge)],
    deadzone: &[Spot],
) -> Diagram {
    let mut found = Diagram {
        cells: Vec::with_capacity(homes.len()),
        areas: Vec::with_capacity(homes.len()),
        centroids: Vec::with_capacity(homes.len()),
        links: Vec::with_capacity(homes.len()),
    };
    for (this, home) in homes.iter().enumerate() {
        let mut cell = rim.to_vec();
        for &other in &nearest[this] {
            // Where this home is no further off, weighed, than the other:
            //   2 at·(other − home) ≤ |other|² − |home|² − w_other + w_home
            let there = homes[other];
            let normal = (2.0 * (there.0 - home.0), 2.0 * (there.1 - home.1));
            let bound = there.0.powi(2) + there.1.powi(2)
                - home.0.powi(2)
                - home.1.powi(2)
                - weights[other]
                + weights[this];
            if cell
                .iter()
                .all(|(spot, _)| normal.0 * spot.0 + normal.1 * spot.1 <= bound)
            {
                continue;
            }
            cell = clip(&cell, normal, bound, Some(other));
            if cell.is_empty() {
                break;
            }
        }
        let corners: Vec<Spot> = cell.iter().map(|(spot, _)| *spot).collect();
        let (area, centroid) = outside(&corners, deadzone);
        let mut links: HashMap<usize, f64> = HashMap::new();
        for ((from, edge), (to, _)) in cell.iter().zip(cell.iter().cycle().skip(1)) {
            if let Some(other) = edge {
                let apart = (homes[*other].0 - home.0).hypot(homes[*other].1 - home.1);
                *links.entry(*other).or_default() += length_outside(*from, *to) / (2.0 * apart);
            }
        }
        found.cells.push(cell);
        found.areas.push(area);
        found.centroids.push(centroid);
        found.links.push(links.into_iter().collect());
    }
    found
}

/// Solves `L x = g` for the weighted graph Laplacian `L` the links describe,
/// by conjugate gradients. A whisker of regularisation stands in for pinning
/// one weight, since the weights only matter relative to one another.
fn solve(links: &[Vec<(usize, f64)>], g: &[f64]) -> Vec<f64> {
    let n = g.len();
    let times = |x: &[f64]| -> Vec<f64> {
        (0..n)
            .map(|i| {
                links[i]
                    .iter()
                    .map(|(j, c)| c * (x[i] - x[*j]))
                    .sum::<f64>()
                    + 1e-9 * x[i]
            })
            .collect()
    };
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let mut x = vec![0.0; n];
    let mut r = g.to_vec();
    let mut p = r.clone();
    let mut rr = dot(&r, &r);
    for _ in 0..4 * n {
        if rr.sqrt() < 1e-14 {
            break;
        }
        let lp = times(&p);
        let alpha = rr / dot(&p, &lp);
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * lp[i];
        }
        let next = dot(&r, &r);
        let beta = next / rr;
        rr = next;
        for i in 0..n {
            p[i] = r[i] + beta * p[i];
        }
    }
    x
}

/// For each pair of speeds, where each symmetry sends it, and by which.
type Orbits = Vec<Vec<(usize, usize)>>;

fn orbits(chosen: &[Speeds], group: &[Symmetry]) -> Orbits {
    let index: HashMap<Speeds, usize> = chosen.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    chosen
        .iter()
        .map(|speeds| {
            group
                .iter()
                .enumerate()
                .map(|(k, m)| (index[&mirrored(m, *speeds)], k))
                .collect()
        })
        .collect()
}

fn symmetrise_weights(weights: &mut [f64], orbits: &Orbits) {
    let old = weights.to_vec();
    for (this, orbit) in orbits.iter().enumerate() {
        weights[this] =
            orbit.iter().map(|(other, _)| old[*other]).sum::<f64>() / orbit.len() as f64;
    }
}

fn symmetrise_homes(homes: &mut [Spot], orbits: &Orbits, group: &[Symmetry]) {
    let old = homes.to_vec();
    for (this, orbit) in orbits.iter().enumerate() {
        let (mut x, mut y) = (0.0, 0.0);
        for (other, k) in orbit {
            let spot = unmirrored(&group[*k], old[*other]);
            x += spot.0;
            y += spot.1;
        }
        homes[this] = (x / orbit.len() as f64, y / orbit.len() as f64);
    }
}

/// The weights that give every region `target` area, by damped Newton: each
/// step solves for all the weights at once, and is shortened until no region
/// shrinks below half the smallest it started at and the areas are nearer
/// right than before.
fn equalise(
    homes: &[Spot],
    orbits: &Orbits,
    rim: &[(Spot, Edge)],
    deadzone: &[Spot],
    target: f64,
) -> (Vec<f64>, Diagram) {
    let n = homes.len();
    let nearest: Vec<Vec<usize>> = (0..n)
        .map(|this| {
            let distance = |other: usize| {
                (homes[other].0 - homes[this].0).hypot(homes[other].1 - homes[this].1)
            };
            let mut others: Vec<usize> = (0..n).filter(|other| *other != this).collect();
            others.sort_by(|a, b| distance(*a).total_cmp(&distance(*b)));
            others.truncate(NEIGHBOURS);
            others
        })
        .collect();

    let mut weights = vec![0.0; n];
    let mut found = diagram(homes, &weights, &nearest, rim, deadzone);
    let floor = 0.5 * found.areas.iter().copied().fold(target, f64::min);
    let off = |areas: &[f64]| {
        areas
            .iter()
            .map(|a| (target - a).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    for _ in 0..NEWTON_STEPS {
        let worst = found
            .areas
            .iter()
            .map(|area| (area - target).abs() / target)
            .fold(0.0, f64::max);
        if worst < TOLERANCE {
            break;
        }
        let short: Vec<f64> = found.areas.iter().map(|area| target - area).collect();
        let step = solve(&found.links, &short);
        let before = off(&found.areas);
        let mut length = 1.0;
        loop {
            let mut trial: Vec<f64> = weights
                .iter()
                .zip(&step)
                .map(|(weight, by)| weight + length * by)
                .collect();
            symmetrise_weights(&mut trial, orbits);
            let next = diagram(homes, &trial, &nearest, rim, deadzone);
            let smallest = next.areas.iter().copied().fold(f64::MAX, f64::min);
            if (smallest >= floor && off(&next.areas) <= (1.0 - length / 2.0) * before)
                || length < 1e-4
            {
                weights = trial;
                found = next;
                break;
            }
            length /= 2.0;
        }
    }
    (weights, found)
}

/// Lays the pad out.
fn laid_out() -> Vec<Region> {
    let chosen = chosen();
    let group = symmetries();
    let orbits = orbits(&chosen, &group);
    let rim: Vec<(Spot, Edge)> = circle(1.0, RIM_SIDES)
        .into_iter()
        .map(|spot| (spot, None))
        .collect();
    let deadzone = circle(f64::from(DEADZONE), DEADZONE_SIDES);
    let corners: Vec<Spot> = rim.iter().map(|(spot, _)| *spot).collect();
    let target = (moments(&corners).0 - moments(&deadzone).0) / chosen.len() as f64;

    let ideal: Vec<Spot> = chosen.iter().map(|speeds| home_of(*speeds)).collect();
    let mut homes = ideal.clone();
    let (mut weights, mut found) = equalise(&homes, &orbits, &rim, &deadzone, target);
    for _ in 0..RELAXATIONS {
        for (home, (ideal, middle)) in homes.iter_mut().zip(ideal.iter().zip(&found.centroids)) {
            *home = (
                ideal.0 + PULL * (middle.0 - ideal.0),
                ideal.1 + PULL * (middle.1 - ideal.1),
            );
        }
        symmetrise_homes(&mut homes, &orbits, &group);
        (weights, found) = equalise(&homes, &orbits, &rim, &deadzone, target);
    }

    chosen
        .iter()
        .enumerate()
        .map(|(this, speeds)| Region {
            velocity: speeds.velocity(),
            outline: found.cells[this]
                .iter()
                .map(|((x, y), _)| (*x as f32, *y as f32))
                .collect(),
            home: homes[this],
            weight: weights[this],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafton_visca::command::PanTiltDirection;

    fn outline(region: &Region) -> Vec<Spot> {
        region
            .outline
            .iter()
            .map(|(x, y)| (f64::from(*x), f64::from(*y)))
            .collect()
    }

    #[test]
    fn every_region_has_the_same_area() {
        let deadzone = circle(f64::from(DEADZONE), DEADZONE_SIDES);
        let areas: Vec<f64> = regions()
            .iter()
            .map(|region| outside(&outline(region), &deadzone).0)
            .collect();
        let mean = areas.iter().sum::<f64>() / areas.len() as f64;
        for (region, area) in regions().iter().zip(&areas) {
            assert!(
                (area / mean - 1.0).abs() < 0.01,
                "{:?} has {area} against {mean}",
                region.velocity
            );
        }
    }

    #[test]
    fn the_regions_drawn_are_the_regions_a_push_lands_in() {
        // Each outline is convex, so the average of its corners is inside
        // it; a push there must ask for what the outline says it does.
        for region in regions() {
            let count = region.outline.len() as f32;
            let middle = region.outline.iter().fold((0.0, 0.0), |sum, point| {
                (sum.0 + point.0 / count, sum.1 + point.1 / count)
            });
            if middle.0.hypot(middle.1) <= DEADZONE {
                continue;
            }
            assert_eq!(velocity_at(middle.0, middle.1), region.velocity);
        }
    }

    #[test]
    fn every_speed_along_each_axis_has_a_region() {
        let has = |velocity: Velocity| regions().iter().any(|region| region.velocity == velocity);
        for speed in PAN_SPEEDS {
            for direction in [PanTiltDirection::Left, PanTiltDirection::Right] {
                assert!(has(Velocity {
                    direction,
                    pan_speed: speed,
                    tilt_speed: 1,
                }));
            }
        }
        for speed in TILT_SPEEDS {
            for direction in [PanTiltDirection::Up, PanTiltDirection::Down] {
                assert!(has(Velocity {
                    direction,
                    pan_speed: 1,
                    tilt_speed: speed,
                }));
            }
        }
    }

    #[test]
    fn the_regions_are_round_rather_than_strips() {
        // The thing spacing the homes on the pad is for: no region much
        // longer one way than the other, which by its area against that of
        // a circle with the same edge is somewhere over a half.
        for region in regions() {
            let corners = outline(region);
            let edge: f64 = corners
                .iter()
                .zip(corners.iter().cycle().skip(1))
                .map(|(a, b)| (b.0 - a.0).hypot(b.1 - a.1))
                .sum();
            let roundness = 4.0 * PI * moments(&corners).0 / (edge * edge);
            assert!(
                roundness > 0.55,
                "{:?} is a strip: {roundness}",
                region.velocity
            );
        }
    }

    #[test]
    fn the_pad_is_the_same_mirrored_every_way() {
        let flip = |velocity: Velocity, sideways: bool| {
            use PanTiltDirection::*;
            let direction = match (velocity.direction, sideways) {
                (Left, true) => Right,
                (Right, true) => Left,
                (UpLeft, true) => UpRight,
                (UpRight, true) => UpLeft,
                (DownLeft, true) => DownRight,
                (DownRight, true) => DownLeft,
                (Up, false) => Down,
                (Down, false) => Up,
                (UpLeft, false) => DownLeft,
                (DownLeft, false) => UpLeft,
                (UpRight, false) => DownRight,
                (DownRight, false) => UpRight,
                (other, _) => other,
            };
            Velocity {
                direction,
                ..velocity
            }
        };
        for step in 0..200 {
            let angle = step as f32 * 0.0314 + 0.01;
            let distance = 0.15 + (step % 17) as f32 * 0.05;
            let (x, y) = (distance * angle.cos(), distance * angle.sin());
            let here = velocity_at(x, y);
            assert_eq!(velocity_at(-x, y), flip(here, true), "at ({x}, {y})");
            assert_eq!(velocity_at(x, -y), flip(here, false), "at ({x}, {y})");
        }
    }

    #[test]
    fn a_push_goes_the_way_it_points() {
        let at = |degrees: f32| {
            let angle = degrees.to_radians();
            velocity_at(0.8 * angle.cos(), 0.8 * angle.sin()).direction
        };
        assert_eq!(at(0.0), PanTiltDirection::Right);
        assert_eq!(at(45.0), PanTiltDirection::UpRight);
        assert_eq!(at(90.0), PanTiltDirection::Up);
        assert_eq!(at(135.0), PanTiltDirection::UpLeft);
        assert_eq!(at(180.0), PanTiltDirection::Left);
        assert_eq!(at(225.0), PanTiltDirection::DownLeft);
        assert_eq!(at(270.0), PanTiltDirection::Down);
        assert_eq!(at(315.0), PanTiltDirection::DownRight);
    }

    #[test]
    fn the_rim_is_about_one_speed_all_the_way_round() {
        // With equal ranges the rim's speed is the same every way: within a
        // step of the top, whichever direction the push is.
        let top = f64::from(*PAN_SPEEDS.end() - 1);
        for degrees in (0..360).step_by(2) {
            let angle = (degrees as f32).to_radians();
            let velocity = velocity_at(0.99 * angle.cos(), 0.99 * angle.sin());
            let speeds = Speeds {
                pan: match velocity.direction {
                    PanTiltDirection::Up | PanTiltDirection::Down => 0,
                    _ => i32::from(velocity.pan_speed),
                },
                tilt: match velocity.direction {
                    PanTiltDirection::Left | PanTiltDirection::Right => 0,
                    _ => i32::from(velocity.tilt_speed),
                },
            };
            assert!(
                (speeds.level() - top).abs() <= 1.0,
                "a full push at {degrees} degrees asked for {velocity:?}"
            );
        }
    }

    #[test]
    fn a_push_inside_the_deadzone_asks_for_nothing() {
        assert!(velocity_at(0.0, 0.0).is_stop());
        assert!(velocity_at(0.06, 0.06).is_stop());
        assert!(!velocity_at(0.2, 0.0).is_stop());
    }
}
