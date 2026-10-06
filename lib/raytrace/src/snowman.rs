//! A snowman as one is really built: balls rolled up from the snow about it,
//! none of them round. A ball rolled grows as a drum, wider round the way it
//! rolled than across it, wrapped in the sheets it took up, each sheet's end
//! a lip; it is lumped where it rolled unevenly, dented where it was patted,
//! flattened where it was set down and where the next was pressed onto it,
//! snow packed about the join, and settled under its own weight. Its face is
//! whatever came to hand: lumps of coal, a carrot, sticks for arms.

use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::noise::{noise3, smoothstep};
use crate::pigment::Spot;
use crate::prototype::{normals_of, Building, Facet, Part, Prototype, Tube};
use crate::rock::{icosahedron, subdivide};
use crate::sample::mix32;
use crate::vector::{singles, Vec3};

/// The most dents a ball was patted into.
const MOST_DENTS: usize = 10;

/// The longest a ball's facets' edges may be, about a centimetre: a snowman
/// stands a few paces from the eye. An icosahedron on the unit sphere has
/// edges this long, and each subdivision halves them; past the most
/// subdivisions a ball is coarser than asked.
const FACET: f64 = 0.009;
const ICOSAHEDRON_EDGE: f64 = 1.051_462_224_238_267_3;
const MOST_LEVELS: u32 = 6;

/// How many vertices of a ball a unit of its shaping sets.
const SHAPE_UNIT: usize = 8192;

/// How sharply a flattened face meets the rest of a ball, as a share of its
/// radius: the edge a press leaves is rounded, never cut.
const FILLET: f64 = 0.06;

/// How far round from the edge of a ball's seat the snow packed about the
/// join reaches, in radians.
const COLLAR_REACH: f64 = 0.38;

/// A ball of snow rolled for a snowman, in its own frame: its roll axis
/// along x, standing up along y.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Ball {
    pub(crate) radius: f64,
    /// How much narrower it stands along its roll axis than round it, and
    /// the second, gentler roll across it, as that roll's axis and how much.
    drum: f64,
    second: (Vec3, f64),
    /// The sheets it took up rolling, ends to a turn round its axis; how
    /// high each one's lip stands; and how far they spiral along the axis.
    sheets: (u32, f64, f64),
    /// How lumpy it rolled, as a share of its radius.
    lumps: f64,
    /// Where it was patted: each dent's middle, the cosine of the angle it
    /// reaches out to, and its depth.
    dents: [(Vec3, f64, f64); MOST_DENTS],
    dented: usize,
    /// The heights it was flattened to, unsettled: below its middle where it
    /// was set down, above where the ball on it was seated, if one was.
    foot: f64,
    seat: Option<f64>,
    /// How high the snow packed about its seat stands.
    collar: f64,
    /// The share of its height it keeps having settled under its weight.
    settled: f64,
    seed: u32,
}

/// How a ball was made and what rests on it: whether it was rolled or
/// packed by hand, how hard it was pressed down, and how far down the ball
/// seated on it, if any, was pressed into it, as shares of its radius.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Making {
    pub(crate) rolled: bool,
    pub(crate) pressed: f64,
    pub(crate) seat: Option<f64>,
}

impl Ball {
    /// A ball `radius` across made as `making` says, drawn under `seed`.
    pub(crate) fn made(radius: f64, making: Making, seed: u64) -> Self {
        let mut dice = NonCryptoRng::seed_from_u64(seed);
        let patted = usize::try_from(dice.next_below(5)).unwrap_or(0);
        let mut range = |low: f64, high: f64| low + (high - low) * dice.next_f64();
        let (drum, second, sheets, lumps) = if making.rolled {
            let turn = range(0.0, TAU);
            let across = Vec3::new(0.0, mathf::cos(turn), mathf::sin(turn));
            let ends = 2 + u32::from(range(0.0, 1.0) > 0.3) + u32::from(range(0.0, 1.0) > 0.6);
            (
                range(0.06, 0.17),
                (across, range(0.0, 0.07)),
                (ends, radius * range(0.012, 0.035), range(-0.25, 0.25)),
                range(0.03, 0.06),
            )
        } else {
            // Packed by hand: rounder, with no sheets, and lumpier for it.
            (
                range(0.0, 0.05),
                (Vec3::UP, 0.0),
                (0, 0.0, 0.0),
                range(0.05, 0.09),
            )
        };
        let mut dents = [(Vec3::UP, 1.0, 0.0); MOST_DENTS];
        let dented = (if making.rolled { 3 } else { 6 } + patted).min(MOST_DENTS);
        for dent in dents.iter_mut().take(dented) {
            let (around, rise) = (range(0.0, TAU), range(-0.6, 0.9));
            let level = mathf::sqrt(1.0 - rise * rise);
            let middle = Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around));
            // A palm's breadth, or a few fingers'.
            let breadth = range(0.05, 0.11) / radius;
            *dent = (
                middle,
                mathf::cos(breadth.min(0.9)),
                radius * range(0.008, 0.03),
            );
        }
        Self {
            radius,
            drum,
            second,
            sheets,
            lumps,
            dents,
            dented,
            foot: -radius * (1.0 - making.pressed),
            seat: making.seat.map(|seat| radius * (1.0 - seat)),
            collar: if making.seat.is_some() {
                radius * range(0.02, 0.07)
            } else {
                0.0
            },
            settled: range(0.9, 0.98),
            seed: u32::try_from(seed & 0xffff_ffff).unwrap_or(0),
        }
    }

    /// The height of its flat foot above its middle, settled: negative.
    pub(crate) fn foot(&self) -> f64 {
        self.foot * self.settled
    }

    /// The height of the flat it was pressed to where the ball above sits,
    /// settled, if one does.
    pub(crate) fn seat(&self) -> Option<f64> {
        self.seat.map(|seat| seat * self.settled)
    }

    /// The point of its surface along the unit `direction` from its middle.
    pub(crate) fn surface(&self, direction: Vec3) -> Vec3 {
        let point = direction * self.reach(direction);
        Vec3::new(point.x, point.y * self.settled, point.z)
    }

    /// How far its surface stands from its middle along the unit `d`, before
    /// it settled.
    fn reach(&self, d: Vec3) -> f64 {
        let along = d.x.abs();
        let drum = 1.0 - self.drum * along * along * along;
        let (axis, by) = self.second;
        let across = d.dot(axis).abs();
        let second = 1.0 - by * across * across * across;
        let seed = self.seed;
        let lumps = self.lumps
            * (noise3(d * 1.7, seed)
                + 0.45 * noise3(d * 3.6, seed ^ 0x2b)
                + 0.2 * noise3(d * 7.5, seed ^ 0x3c));
        let mut reach =
            self.radius * drum * second * (1.0 + lumps) + self.sheet(d) - self.dented(d);
        let fillet = FILLET * self.radius;
        if d.y < 0.0 {
            reach = rounded_min(reach, self.foot / d.y, fillet);
        }
        if let Some(seat) = self.seat {
            if d.y > 0.0 {
                reach = rounded_min(reach, seat / d.y, fillet);
            }
            reach += self.collared(d, seat);
        }
        reach.max(0.05 * self.radius)
    }

    /// How far the lip of a sheet it took up stands proud along `d`: proud
    /// where the sheet ends, thinning behind it, about its drum and not its
    /// ends.
    fn sheet(&self, d: Vec3) -> f64 {
        let (ends, lip, spiral) = self.sheets;
        if ends == 0 {
            return 0.0;
        }
        // Whole ends to a turn, so the sheets close round the axis.
        let around = mathf::atan2(d.z, d.y) / TAU;
        let wobble = 0.08 * noise3(d * 2.2, self.seed ^ 0x51);
        let turn = (around + spiral * d.x + wobble) * f64::from(ends);
        let within = turn - mathf::floor(turn);
        let band = 1.0 - smoothstep(0.45, 0.85, d.x.abs());
        lip * smoothstep(0.0, 0.05, within) * (1.0 - within) * band
    }

    /// How deep the dents it was patted into lie along `d`.
    fn dented(&self, d: Vec3) -> f64 {
        self.dents
            .iter()
            .take(self.dented)
            .map(|&(middle, edge, depth)| {
                let facing = d.dot(middle);
                if facing <= edge {
                    return 0.0;
                }
                let out = (1.0 - facing) / (1.0 - edge);
                depth * (1.0 - out) * (1.0 - out)
            })
            .sum()
    }

    /// How high the snow packed about its seat, `seat` high, stands along
    /// `d`: highest at the seat's edge, where the ball above meets it, lumped
    /// as it was pressed in by hand.
    fn collared(&self, d: Vec3, seat: f64) -> f64 {
        if self.collar <= 0.0 {
            return 0.0;
        }
        let edge = mathf::acos((seat / self.radius).clamp(-1.0, 1.0));
        let out = (mathf::acos(d.y.clamp(-1.0, 1.0)) - edge) / COLLAR_REACH;
        if !(0.0..1.0).contains(&out) {
            return 0.0;
        }
        let pressed = 0.7 + 0.6 * noise3(d * 6.0, self.seed ^ 0x6c);
        self.collar * (1.0 - out) * (1.0 - out) * (1.0 + 2.0 * out) * pressed
    }
}

/// The smaller of `a` and `b`, the corner between them rounded over `k`.
fn rounded_min(a: f64, b: f64, k: f64) -> f64 {
    let blend = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * blend - k * blend * (1.0 - blend)
}

/// A ball being shaped a unit of its vertices at a time.
#[derive(Debug)]
pub(crate) struct Rolling {
    ball: Ball,
    vertices: Vec<Vec3>,
    faces: Vec<[u32; 3]>,
    shaped: usize,
}

impl Rolling {
    /// `ball` to shape, its sphere subdivided until its facets are as fine
    /// as a snowman wants; `None` when the heap will not hold it.
    pub(crate) fn new(ball: &Ball) -> Option<Self> {
        let (mut vertices, mut faces) = icosahedron()?;
        let mut edge = ICOSAHEDRON_EDGE * ball.radius;
        let mut levels = 0;
        while levels < MOST_LEVELS && edge > FACET {
            (vertices, faces) = subdivide(&vertices, &faces)?;
            edge *= 0.5;
            levels += 1;
        }
        Some(Self {
            ball: *ball,
            vertices,
            faces,
            shaped: 0,
        })
    }

    /// Shape a unit more of its vertices: whether every one is shaped.
    pub(crate) fn step(&mut self) -> bool {
        let end = (self.shaped + SHAPE_UNIT).min(self.vertices.len());
        if let Some(unit) = self.vertices.get_mut(self.shaped..end) {
            for vertex in unit {
                *vertex = self.ball.surface(*vertex);
            }
        }
        self.shaped = end;
        self.shaped >= self.vertices.len()
    }

    /// The shaped ball, its hierarchy still to build, made in whatever it is
    /// placed in; `None` when the heap will not hold it.
    pub(crate) fn finish(self) -> Option<Building> {
        let normals = normals_of(&self.vertices, &self.faces)?;
        let mut parts = Vec::new();
        parts.try_reserve_exact(self.faces.len()).ok()?;
        parts.extend(
            self.faces
                .iter()
                .map(|&corners| Part::Facet(Facet::plain(corners, None))),
        );
        let mut vertices = Vec::new();
        vertices.try_reserve_exact(self.vertices.len()).ok()?;
        vertices.extend(self.vertices.iter().map(|&vertex| singles(vertex)));
        Prototype::building(parts, vertices, normals)
    }
}

/// A ball of `ball`'s shape made whole on the calling thread.
#[cfg(test)]
pub(crate) fn rolled(ball: &Ball) -> Option<Building> {
    let mut rolling = Rolling::new(ball)?;
    while !rolling.step() {}
    rolling.finish()
}

/// The segments a carrot is laid in.
const CARROT_SEGMENTS: u32 = 8;

/// A carrot `length` long and `radius` thick at its crown, its tip along
/// its frame's z, in `skin` and grown from `seed`: holding its girth at the
/// shoulder before tapering away to the thread of root it ends in, and bent
/// a little as it grew. `None` when the heap will not hold it.
pub(crate) fn carrot(length: f64, radius: f64, skin: u16, seed: u64) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut range = |low: f64, high: f64| low + (high - low) * dice.next_f64();
    let (droop, sway) = (range(-0.35, 0.15), range(-0.2, 0.2));
    let key = mix32(u32::try_from(seed & 0xffff_ffff).unwrap_or(0));
    let thickness = |t: f64| radius * (0.012 + 0.988 * crate::vector::power(1.0 - t, 0.75));
    let mut parts = Vec::new();
    parts.try_reserve_exact(CARROT_SEGMENTS as usize).ok()?;
    let mut at = Vec3::ZERO;
    let step = length / f64::from(CARROT_SEGMENTS);
    for segment in 0..CARROT_SEGMENTS {
        let (t0, t1) = (
            f64::from(segment) / f64::from(CARROT_SEGMENTS),
            f64::from(segment + 1) / f64::from(CARROT_SEGMENTS),
        );
        let bent = t1 * t1;
        let toward = Vec3::new(sway * bent, droop * bent, 1.0).normalized();
        let next = at + toward * step;
        parts.push(Part::Tube(Tube::new(
            (at, next),
            ((thickness(t0), thickness(t1)), (t0 * length, t1 * length)),
            (skin, key),
            Vec3::UP,
        )));
        at = next;
    }
    Prototype::building(parts, Vec::new(), Vec::new())
}

/// The segments a stick's own length is laid in, and its twigs'.
const STICK_SEGMENTS: u32 = 6;
const TWIG_SEGMENTS: u32 = 3;

/// A stick `length` long and `radius` thick at its butt, reaching out along
/// its frame's z from where it was pushed in, in `bark`: crooked, forking
/// once or twice and twiggy toward its end, as a fallen branch picked up is.
/// `None` when the heap will not hold it.
pub(crate) fn stick(length: f64, radius: f64, bark: u16, seed: u64) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut key = mix32(u32::try_from(seed & 0xffff_ffff).unwrap_or(0));
    let mut parts = Vec::new();
    parts.try_reserve(32).ok()?;
    let forks = 1 + u32::from(drawn(&mut dice, 0.0, 1.0) > 0.45);
    let mut forked_at = [0.0; 2];
    for slot in forked_at.iter_mut().take(forks as usize) {
        *slot = drawn(&mut dice, 0.35, 0.8);
    }
    let mut at = Vec3::ZERO;
    let mut heading = Vec3::new(0.0, 0.0, 1.0);
    let step = length / f64::from(STICK_SEGMENTS);
    // Narrowing on out to the fine tip of the twig it ends in.
    let girth = |t: f64| radius * (1.0 - 0.7 * t) * (1.0 - 0.8 * smoothstep(0.7, 1.0, t));
    for segment in 0..STICK_SEGMENTS {
        let (t0, t1) = (
            f64::from(segment) / f64::from(STICK_SEGMENTS),
            f64::from(segment + 1) / f64::from(STICK_SEGMENTS),
        );
        heading = (heading
            + Vec3::new(
                drawn(&mut dice, -0.25, 0.25),
                drawn(&mut dice, -0.2, 0.25),
                0.0,
            ))
        .normalized();
        let next = at + heading * step;
        key = mix32(key ^ 0x9e37_79b9);
        parts.try_reserve(1).ok()?;
        parts.push(Part::Tube(Tube::new(
            (at, next),
            ((girth(t0), girth(t1)), (t0 * length, t1 * length)),
            (bark, key),
            Vec3::UP,
        )));
        for &fork in forked_at.iter().take(forks as usize) {
            if fork < t0 || fork >= t1 {
                continue;
            }
            let from = at + (next - at) * ((fork - t0) / (t1 - t0));
            let around = drawn(&mut dice, 0.0, TAU);
            let splay = drawn(&mut dice, 0.35, 0.8);
            let out = (heading * mathf::cos(splay)
                + Vec3::new(mathf::cos(around), mathf::sin(around), 0.0) * mathf::sin(splay))
            .normalized();
            let reach = length * (1.0 - fork) * drawn(&mut dice, 0.35, 0.65);
            twig(
                &mut parts,
                (from, out, reach),
                (0.6 * girth(fork), bark, &mut key),
                &mut dice,
            )?;
        }
        at = next;
    }
    Prototype::building(parts, Vec::new(), Vec::new())
}

/// A draw from `low` to `high`.
fn drawn(dice: &mut NonCryptoRng, low: f64, high: f64) -> f64 {
    low + (high - low) * dice.next_f64()
}

/// A twig of a stick from `from` along `out`, `reach` long and `radius`
/// thick where it leaves, wandering as it goes.
fn twig(
    parts: &mut Vec<Part>,
    (from, out, reach): (Vec3, Vec3, f64),
    (radius, bark, key): (f64, u16, &mut u32),
    dice: &mut NonCryptoRng,
) -> Option<()> {
    let step = reach / f64::from(TWIG_SEGMENTS);
    let (mut at, mut heading) = (from, out);
    for segment in 0..TWIG_SEGMENTS {
        let (t0, t1) = (
            f64::from(segment) / f64::from(TWIG_SEGMENTS),
            f64::from(segment + 1) / f64::from(TWIG_SEGMENTS),
        );
        let wander = |dice: &mut NonCryptoRng| 0.5 * (dice.next_f64() - 0.5);
        heading = (heading + Vec3::new(wander(dice), wander(dice), wander(dice))).normalized();
        let next = at + heading * step;
        *key = mix32(*key ^ 0x9e37_79b9);
        parts.try_reserve(1).ok()?;
        parts.push(Part::Tube(Tube::new(
            (at, next),
            (
                (radius * (1.0 - 0.6 * t0), radius * (1.0 - 0.6 * t1)),
                (t0 * reach, t1 * reach),
            ),
            (bark, *key),
            Vec3::UP,
        )));
        at = next;
    }
    Some(())
}

/// Snow rolled up from the ground, streaked with what it took up: the snow,
/// pressed greyer than it fell; the earth, dead grass and leaf it picked up;
/// and how much of its drum they streak.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Rolled {
    pub(crate) snow: Vec3,
    pub(crate) taken: [Vec3; 3],
    pub(crate) streaked: f64,
    pub(crate) seed: u32,
}

/// How many streaks run round a rolled ball's drum to a unit of its axis,
/// and how long they run round it, as a share of a turn.
const STREAKS_ACROSS: f64 = 9.0;
const STREAK_LENGTH: f64 = 0.35;

impl Rolled {
    /// The colour at `spot`, on a ball rolled about its texture's x axis.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let p = spot.p;
        let distance = p.length().max(1e-9);
        let round = mathf::hypot(p.y, p.z).max(1e-9);
        let across = p.x / distance;
        let key = mix32(self.seed ^ spot.instance);
        // Long round the drum, the way the ground was rolled over, narrow
        // across it.
        let unit_round = 1.0 / (TAU * STREAK_LENGTH);
        let q = Vec3::new(
            across * STREAKS_ACROSS,
            p.y / round * unit_round,
            p.z / round * unit_round,
        );
        let band = 1.0 - smoothstep(0.4, 0.8, across.abs());
        let streak = smoothstep(0.3, 0.6, noise3(q, key)) * band * self.streaked;
        // A streak narrower than the footprint settles to its share.
        let resolved = 1.0 - smoothstep(0.25, 1.0, spot.width * STREAKS_ACROSS / distance);
        let mean = 0.18 * band * self.streaked;
        let share = mean + (streak - mean) * resolved;
        let which = 0.5 + 0.5 * noise3(q * 0.6, key ^ 0x7d);
        let taken = if which < 0.45 {
            self.taken[0]
        } else if which < 0.8 {
            self.taken[1]
        } else {
            self.taken[2]
        };
        let grit = 0.96 + 0.04 * noise3(p * 40.0, key ^ 0x3a) * resolved;
        (self.snow * grit).lerp(taken, 0.75 * share)
    }
}

#[cfg(test)]
#[path = "snowman_tests.rs"]
mod tests;
