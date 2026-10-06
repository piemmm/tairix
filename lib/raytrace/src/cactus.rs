//! A saguaro: its pleated column and arms, the felted areoles along every
//! rib's crest, and the spines each one bears.
//!
//! A saguaro's skin is folded into ribs like a bellows, so it swells with the
//! water it stores and shrinks as it dries: rounded crests between V grooves,
//! as many round a stem however thick it grows, so they crowd together over
//! the dome its apex rounds into. They wander a little as they run up the
//! stem. Along each crest an areole stands every couple of centimetres, a
//! felted cushion bearing a cluster of spines: a stout central pointing out
//! and down, a few more spreading about it, and a ring of finer radials. An
//! arm grows out of an areole, so it leaves its trunk narrow and swells to its
//! girth beyond, turns up through its elbow and rises beside the trunk.
//!
//! A stem grows only at its apex, so the farther a place lies below it the
//! older it is: its spines fade from red-brown through tan to grey and break
//! off or fall, and on the oldest stems the skin about the foot corks over.
//! Every limb's stem is therefore reckoned from its apex down.
//!
//! The ribs and their areoles are drawn from the stem's seed alone and laid in
//! the plant's own measure, so the skin its pattern draws and the spines its
//! prototype sets on it agree however large the plant is placed.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, TAU};

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::detail::Spines;
use crate::noise::{hash2, noise3, smoothstep};
use crate::prototype::{Building, Part, Prototype, Tube};
use crate::sample::{mix32, unit};
use crate::vector::{real, single, wrapped, Vec3};

/// How a stem's ribs are laid: how many run round it, and the seed their
/// wander and their areoles are drawn from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Ribs {
    pub(crate) count: u8,
    pub(crate) seed: u32,
}

/// How far a crest wanders either way of its place, as a share of the angle
/// between two crests, and over what length of stem its wander turns.
const WANDER: f64 = 0.12;
const WANDER_LENGTH: f64 = 1.4;

/// Metres between areoles along a crest, and how far each strays from its
/// place, as a share of that.
pub(crate) const AREOLE_SPACING: f64 = 0.022;
const AREOLE_STRAY: f64 = 0.3;

/// An areole's cushion: half its length along the crest and half its breadth
/// across, in metres.
pub(crate) const CUSHION: (f64, f64) = (0.0065, 0.005);

/// The share of a stem's cut its cushions stand above its crests: a crest
/// lies this far below the stem's radius and an areole's top at it.
pub(crate) const FELT: f64 = 0.08;

/// The wool a cushion is felted of: its tufts to a metre and how far they
/// lift its top, as a share of its own height.
pub(crate) const WOOL: (f64, f64) = (900.0, 0.25);

/// A rib's depth, groove to crest, as a share of its stem's girth.
pub(crate) const RIB_DEPTH: f64 = 0.11;

/// How sharply a crest is rounded, as a share of the way to its groove; the
/// steepest its profile then falls, at its groove, a share of its height over
/// a share of the way; and the profile's mean height, which folds too fine
/// for a pixel settle to.
const CREST: f64 = 0.28;
const PROFILE_STEEPEST: f64 = 1.27;
pub(crate) const MEAN_RIB: f64 = 0.582;

/// How the cuticle's grain stands, and the roughness of cork where it has
/// barked over: their waves to a metre and their shares of the skin's
/// height; and the steepest the two rise a metre between them, a noise
/// rising at most two and a half times its waves.
pub(crate) const CUTICLE: (f64, f64) = (260.0, 0.012);
pub(crate) const CORK_ROUGH: (f64, f64) = (70.0, 0.12);
const GRAINS_STEEPEST: f64 = 2.5 * (CUTICLE.0 * CUTICLE.1 + CORK_ROUGH.0 * CORK_ROUGH.1);

/// How far below its apex a stem's crown of felt reaches, in metres.
pub(crate) const CROWN: f64 = 0.04;

/// How far below its apex a stem has lived long enough for its skin to begin
/// to cork over, and for all of it to have, in metres: only the foot of the
/// tallest and oldest.
const CORKING: (f64, f64) = (6.6, 9.4);

/// Where a point of a stem lies among its ribs.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Lie {
    /// The rib whose crest is nearest.
    pub(crate) rib: u32,
    /// How far from that crest round the stem, in ribs: negative toward its
    /// lesser neighbour.
    pub(crate) off: f64,
    /// The share of the way from the crest to the groove on its side.
    pub(crate) toward: f64,
}

impl Ribs {
    /// The angle between two crests, in radians.
    pub(crate) fn pitch(self) -> f64 {
        TAU / f64::from(self.count.max(1))
    }

    fn wrapped(self, rib: i32) -> u32 {
        rib.rem_euclid(i32::from(self.count.max(1))).cast_unsigned()
    }

    /// Where rib `rib`'s crest stands `stem` metres below the apex, counted in
    /// ribs round the stem: its place, wandered.
    fn crest(self, rib: i32, stem: f64) -> f64 {
        let salt = self.seed ^ self.wrapped(rib).wrapping_mul(0x9e37_79b9);
        f64::from(rib) + WANDER * noise3(Vec3::new(stem / WANDER_LENGTH, 0.37, 0.61), salt)
    }

    /// The angle round the stem, as its bark reckons angles, at which rib
    /// `rib`'s crest stands `stem` metres below the apex.
    pub(crate) fn crest_angle(self, rib: u32, stem: f64) -> f64 {
        self.crest(i32::try_from(rib).unwrap_or(0), stem) * self.pitch()
    }

    /// Where the point `angle` round the stem and `stem` metres below its
    /// apex lies among the ribs.
    pub(crate) fn lie(self, angle: f64, stem: f64) -> Lie {
        let u = angle / self.pitch();
        let middle = mathf::round_i32(u);
        let mut best = (middle, f64::INFINITY, 0.0);
        for rib in [middle - 1, middle, middle + 1] {
            let crest = self.crest(rib, stem);
            if (u - crest).abs() < best.1.abs() {
                best = (rib, u - crest, crest);
            }
        }
        let (rib, off, crest) = best;
        let gap = if off >= 0.0 {
            self.crest(rib + 1, stem) - crest
        } else {
            crest - self.crest(rib - 1, stem)
        };
        Lie {
            rib: self.wrapped(rib),
            off,
            toward: (off.abs() / (0.5 * gap).max(1e-6)).min(1.0),
        }
    }

    /// Where along its rib, in metres below the apex, areole `index` of rib
    /// `rib` stands, and that areole's key.
    pub(crate) fn areole(self, rib: u32, index: i32) -> (f64, u32) {
        let phase = unit(hash2(rib, 0x5a0e, self.seed));
        let key = hash2(rib, index.cast_unsigned(), self.seed ^ 0xa4e0);
        let stray = AREOLE_STRAY * (unit(key) - 0.5);
        ((f64::from(index) + phase + stray) * AREOLE_SPACING, key)
    }

    /// An areole of rib `rib` standing no farther below the apex than `stem`:
    /// where to begin counting areoles from.
    pub(crate) fn first_areole(self, rib: u32, stem: f64) -> i32 {
        let phase = unit(hash2(rib, 0x5a0e, self.seed));
        mathf::round_i32(stem / AREOLE_SPACING - phase) - 1
    }

    /// The areole of rib `rib` nearest `stem` metres below the apex: how far
    /// past it along the stem, in metres, and its key.
    pub(crate) fn nearest_areole(self, rib: u32, stem: f64) -> (f64, u32) {
        let first = self.first_areole(rib, stem);
        let mut best = (f64::INFINITY, 0);
        for index in first..first + 3 {
            let (at, key) = self.areole(rib, index);
            if (stem - at).abs() < best.0.abs() {
                best = (stem - at, key);
            }
        }
        best
    }

    /// How far the skin `stem` metres below the apex and `angle` round the
    /// stem has corked over with age, from nought to all of it: in tongues
    /// rising from about the foot of the oldest stems.
    pub(crate) fn corked(self, angle: f64, stem: f64) -> f64 {
        if stem < CORKING.0 - 1.6 {
            return 0.0;
        }
        let round = Vec3::new(
            0.9 * mathf::cos(angle),
            0.45 * stem,
            0.9 * mathf::sin(angle),
        );
        smoothstep(
            CORKING.0,
            CORKING.1,
            stem + 1.4 * noise3(round, self.seed ^ 0xc0c4),
        )
    }
}

/// The height of a rib's profile the share `toward` of the way from its crest
/// to its groove: a crest rounded over its first `CREST`, its walls running
/// nearly straight down into the sharp V of its groove.
pub(crate) fn profile(toward: f64) -> f64 {
    let rim = mathf::sqrt(1.0 + CREST * CREST);
    (rim - mathf::sqrt(toward * toward + CREST * CREST)) / (rim - CREST)
}

/// How far a cushion of felt stands, from nought at its rim to `1.0` at its
/// middle, `along` metres along its stem and `across` metres round it from
/// its middle: domed low, its top flattening, its wool tufted as `wool`,
/// from about `-1.0` to `1.0`, has it.
pub(crate) fn cushion(along: f64, across: f64, wool: f64) -> f64 {
    let (x, y) = (along / CUSHION.0, across / CUSHION.1);
    let reach = x * x + y * y;
    if reach >= 1.0 {
        return 0.0;
    }
    let fall = 1.0 - reach;
    fall * mathf::sqrt(fall) * (1.0 + WOOL.1 * wool) / (1.0 + WOOL.1)
}

/// The steepest a ribbed skin of `ribs` ribs rises a metre round a stem no
/// thinner than `girth` in radius: down its walls into a groove, over the
/// gap between two crests their wander narrows most; up an areole's cushion,
/// whose fall `(1 − ρ²)^1.5` rises at most 1.5 its height over its half
/// breadth, and over its wool; over the felt of its crown; and the grain of
/// its cuticle and its cork.
pub(crate) fn steepest(ribs: u8, girth: f64) -> f64 {
    let gap = (1.0 - 2.0 * WANDER) * TAU / f64::from(ribs.max(1)) * girth.max(1e-4);
    let walls = (1.0 - FELT) * PROFILE_STEEPEST / (0.5 * gap);
    let wool = 2.5 * WOOL.0 * WOOL.1 / (1.0 + WOOL.1);
    let felt = FELT * (1.5 / CUSHION.1 + wool + 1.5 / (0.5 * CROWN));
    walls + felt + GRAINS_STEEPEST
}

/// A saguaro's makings: the materials and rib layouts of its trunk and its
/// arms, and the material its spines are made in.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Flesh {
    pub(crate) trunk: (u16, Ribs),
    pub(crate) arms: (u16, Ribs),
    pub(crate) spines: u16,
}

/// How far a trunk's segments run, at the most; how far an arm's; and how
/// far apart a stem is sampled through one of its constrictions, as a share
/// of the constriction's reach.
const TRUNK_SEGMENT: f64 = 0.32;
const ARM_SEGMENT: f64 = 0.24;
const THROUGH_BAND: f64 = 0.45;

/// How far below the ground a trunk's foot runs.
const BURIED: f64 = 0.35;

/// The most degrees an arm's elbow turns through a segment.
const ELBOW_STEP: f64 = 14.0;

/// The most parts a saguaro holds, its spines thinned evenly to keep within
/// them, and the areoles whose spines are set in one step of its growing.
pub(crate) const MOST_PARTS: usize = 150_000;
const AREOLES_PER_STEP: usize = 1500;

/// A stem as it is laid out: its points from foot to apex, each with its
/// radius; the side square to the plane it bends in, which its ribs are
/// reckoned round from, and how far round from there; and its material and
/// ribs.
#[derive(Debug)]
struct Stem {
    points: Vec<(Vec3, f64)>,
    side: Vec3,
    turn: f64,
    material: u16,
    ribs: Ribs,
}

impl Stem {
    /// The stem's length from its foot, along its points, to the apex its
    /// last point's dome rounds into.
    fn length(&self) -> f64 {
        let along: f64 = self
            .points
            .windows(2)
            .map(|pair| match pair {
                [(a, _), (b, _)] => (*b - *a).length(),
                _ => 0.0,
            })
            .sum();
        along
            + self
                .points
                .last()
                .map_or(0.0, |&(_, radius)| FRAC_PI_2 * radius)
    }

    /// How far below the apex, along the stem, point `index` lies.
    fn below(&self, index: usize) -> f64 {
        let climbed: f64 = self
            .points
            .windows(2)
            .take(index)
            .map(|pair| match pair {
                [(a, _), (b, _)] => (*b - *a).length(),
                _ => 0.0,
            })
            .sum();
        self.length() - climbed
    }

    /// The limb between points `index` and `index + 1`, keyed `key`, if
    /// there is one: its stem reckoned down from the apex, which the last
    /// one's end rounds into, its ribs crowding over the dome.
    fn limb(&self, index: usize, key: u32) -> Option<Tube> {
        let (&(a, ra), &(b, rb)) = (self.points.get(index)?, self.points.get(index + 1)?);
        let run = (b - a).length();
        let below = self.below(index);
        let mut tube = Tube::new(
            (a, b),
            ((ra, rb), (below, below - run)),
            (self.material, key),
            self.side,
        );
        tube.turn += single(self.turn);
        Some(tube)
    }

    /// Whether limb `index` is the stem's last, its end its apex.
    fn topmost(&self, index: usize) -> bool {
        index + 2 == self.points.len()
    }
}

/// A saguaro growing: its trunk and arms laid out and their limbs laid, then
/// the spines set on their areoles a few stems' stretches at a time.
#[derive(Debug)]
pub(crate) struct Bristling {
    /// The trunk first, then the arms.
    stems: Vec<Stem>,
    parts: Vec<Part>,
    spines: (u16, Spines),
    /// The share of its spines each areole keeps, so a plant with more
    /// areoles than its parts allow thins evenly rather than going bare on
    /// one side.
    share: f64,
    dice: NonCryptoRng,
    /// The stem and the limb along it whose areoles are next.
    next: (usize, usize),
}

impl Bristling {
    /// A saguaro `height` tall, its trunk `girth` in radius where it is
    /// thickest and its arms `arms` as thick against the trunk where they
    /// leave it, made of `flesh`, its areoles bearing as many spines as
    /// `spines` allows, grown from `seed`; `None` when the heap will not hold
    /// its stems.
    pub(crate) fn new(
        height: f64,
        (girth, arms): (f64, f64),
        flesh: Flesh,
        spines: Spines,
        seed: u64,
    ) -> Option<Self> {
        let mut dice = NonCryptoRng::seed_from_u64(seed);
        let mut stems = Vec::new();
        stems.try_reserve(8).ok()?;
        stems.push(trunk(height, girth, flesh.trunk, &mut dice)?);
        let count = arm_count(height, &mut dice);
        let mut placed: Vec<(f64, f64)> = Vec::new();
        placed.try_reserve(usize::try_from(count).ok()?).ok()?;
        for _ in 0..count {
            let Some(trunk) = stems.first() else {
                break;
            };
            if let Some((grown, at)) = arm(trunk, (height, arms), flesh.arms, &placed, &mut dice) {
                placed.push(at);
                stems.try_reserve(1).ok()?;
                stems.push(grown);
            }
        }
        let limbs: usize = stems.iter().map(|stem| stem.points.len()).sum();
        let mut parts = Vec::new();
        parts.try_reserve(limbs).ok()?;
        let mut key = mix32(u32::try_from(seed >> 32).unwrap_or(0) ^ 0x5a6a);
        for stem in &stems {
            for index in 0..stem.points.len().saturating_sub(1) {
                key = mix32(key ^ 0x5ac0);
                parts.push(Part::Tube(stem.limb(index, key)?));
            }
        }
        let each = 1.0 + f64::from(spines.centrals.saturating_sub(1)) + f64::from(spines.radials);
        let areoles: f64 = stems
            .iter()
            .map(|stem| stem.length() * f64::from(stem.ribs.count) / AREOLE_SPACING)
            .sum();
        let room = real(MOST_PARTS.saturating_sub(parts.len()));
        Some(Self {
            stems,
            parts,
            spines: (flesh.spines, spines),
            share: (room / (areoles * each).max(1.0)).min(1.0),
            dice,
            next: (0, 0),
        })
    }

    /// Set the spines of a bounded number of areoles more; whether every
    /// areole has its spines, or `None` when the heap refused them.
    pub(crate) fn step(&mut self) -> Option<bool> {
        let mut set = 0;
        while set < AREOLES_PER_STEP {
            let (stem, limb) = self.next;
            let Some(along) = self.stems.get(stem) else {
                return Some(true);
            };
            if limb + 1 >= along.points.len() {
                self.next = (stem + 1, 0);
                continue;
            }
            set += self.spine_limb(stem, limb)?;
            self.next = (stem, limb + 1);
        }
        Some(self.next.0 >= self.stems.len())
    }

    /// The grown saguaro, its hierarchy still to build; `None` when the heap
    /// will not hold it.
    pub(crate) fn finish(self) -> Option<Building> {
        Prototype::building(self.parts, Vec::new(), Vec::new())
    }

    /// Set the spines of the areoles along limb `limb` of stem `stem`; how
    /// many areoles it holds.
    fn spine_limb(&mut self, stem: usize, limb: usize) -> Option<usize> {
        let (stems, parts, dice) = (&self.stems, &mut self.parts, &mut self.dice);
        let (along, trunk) = (stems.get(stem)?, stems.first()?);
        let below = along.below(limb);
        let tube = along.limb(limb, 0)?;
        let (a, b) = (
            crate::prototype::point(tube.a),
            crate::prototype::point(tube.b),
        );
        let (ra, rb) = (f64::from(tube.radii[0]), f64::from(tube.radii[1]));
        let run = (b - a).length().max(1e-12);
        let axis = (b - a) * (1.0 / run);
        let above = f64::from(tube.stem[1]);
        // Reckoned down from the apex: the dome's stretch, then the side's.
        let from = if along.topmost(limb) { 0.0 } else { above };
        let ribs = along.ribs;
        let mut held = 0;
        for rib in 0..u32::from(ribs.count) {
            let mut index = ribs.first_areole(rib, from);
            loop {
                let (at, key) = ribs.areole(rib, index);
                index += 1;
                if at < from {
                    continue;
                }
                if at >= below {
                    break;
                }
                held += 1;
                let angle = ribs.crest_angle(rib, at);
                let way = tube.way(angle);
                let (base, normal, toward) = if at < above {
                    // On the dome, `at` metres down its meridian from the
                    // apex, where its ribs crowd and their areoles thin to
                    // keep about the spacing a crest keeps.
                    let polar = (at / rb.max(1e-6)).clamp(0.0, FRAC_PI_2);
                    let room = ribs.pitch() * rb * mathf::sin(polar) / AREOLE_SPACING;
                    if room < 0.8 && unit(mix32(key ^ 0xd0e)) > room / 0.8 {
                        continue;
                    }
                    let normal = axis * mathf::cos(polar) + way * mathf::sin(polar);
                    let toward = (axis * mathf::sin(polar) - way * mathf::cos(polar)).normalized();
                    (b + normal * rb, normal, toward)
                } else {
                    let up = (below - at).clamp(0.0, run);
                    let radius = ra + (rb - ra) * (up / run);
                    (a + axis * up + way * radius, way, axis)
                };
                if stem > 0 && within(trunk, base) {
                    continue;
                }
                if ribs.corked(angle, at) > 0.4 {
                    continue;
                }
                cluster(
                    (base, normal, toward),
                    (at, key),
                    (self.spines, self.share),
                    parts,
                    dice,
                )?;
            }
        }
        Some(held)
    }
}

fn range(dice: &mut NonCryptoRng, (low, high): (f64, f64)) -> f64 {
    low + (high - low) * dice.next_f64()
}

/// The trunk of a saguaro `height` tall and `girth` in radius at its
/// thickest: narrower about its foot, which runs on below the ground, leaning
/// a little and bowing gently in one plane, pinched here and there where a
/// drought slowed it, and rounding at its apex into a dome.
fn trunk(
    height: f64,
    girth: f64,
    (material, ribs): (u16, Ribs),
    dice: &mut NonCryptoRng,
) -> Option<Stem> {
    let heading = range(dice, (0.0, TAU));
    let plane = Vec3::new(mathf::cos(heading), 0.0, mathf::sin(heading));
    let lean = range(dice, (0.2, 2.6)).to_radians();
    let bow = range(dice, (-1.5, 1.5)).to_radians();
    let bands = constrictions(dice, (0.7, height - 0.6), height);
    let top = height - girth;
    let radius = |y: f64| {
        let foot = 0.86 + 0.14 * smoothstep(-BURIED, 1.1, y);
        let taper = 1.0 - 0.08 * (y / height).clamp(0.0, 1.0);
        girth * foot * taper * pinched(&bands, y)
    };
    let heights = samples((-BURIED, top), TRUNK_SEGMENT, &bands)?;
    let mut points = Vec::new();
    points.try_reserve_exact(heights.len()).ok()?;
    let mut at = Vec3::UP * -BURIED;
    let mut last = -BURIED;
    for &y in &heights {
        let tilt = lean + bow * ((y + BURIED) / (top + BURIED));
        let way = Vec3::UP * mathf::cos(tilt) + plane * mathf::sin(tilt);
        at += way * ((y - last) / mathf::cos(tilt));
        last = y;
        points.push((at, radius(y)));
    }
    Some(Stem {
        points,
        side: Vec3::UP.cross(plane).normalized(),
        turn: 0.0,
        material,
        ribs,
    })
}

/// None to three constrictions between the heights `within` of a stem
/// `height` long, each how high, how far it pinches the stem and how far
/// either way: where a drought or a hard frost slowed it.
fn constrictions(dice: &mut NonCryptoRng, within: (f64, f64), height: f64) -> [(f64, f64, f64); 3] {
    let mut bands = [(0.0, 0.0, 1.0); 3];
    let count = (dice.next_u32() % 4).min(u32::from(height > 4.0) + 2);
    if within.1 <= within.0 {
        return bands;
    }
    for band in bands.iter_mut().take(count as usize) {
        *band = (
            range(dice, within),
            range(dice, (0.02, 0.06)),
            range(dice, (0.12, 0.35)),
        );
    }
    bands
}

/// How far `bands` pinch a stem `y` along it.
fn pinched(bands: &[(f64, f64, f64)], y: f64) -> f64 {
    bands.iter().fold(1.0, |kept, &(at, depth, reach)| {
        let x = (y - at) / reach;
        kept * (1.0 - depth * mathf::exp(-x * x))
    })
}

/// Where a stem from `from` to `to` is sampled: at most `stride` apart, and
/// closely through each of `bands`.
fn samples((from, to): (f64, f64), stride: f64, bands: &[(f64, f64, f64)]) -> Option<Vec<f64>> {
    let steps = mathf::ceil((to - from) / stride).max(1.0);
    let count = u32::try_from(mathf::round_i32(steps)).ok()?;
    let mut heights = Vec::new();
    heights
        .try_reserve(usize::try_from(count).ok()? + 1 + 9 * bands.len())
        .ok()?;
    for step in 0..=count {
        heights.push(from + (to - from) * f64::from(step) / steps);
    }
    for &(at, depth, reach) in bands {
        if depth <= 0.0 {
            continue;
        }
        for step in -4..=4 {
            let y = at + f64::from(step) * THROUGH_BAND * reach;
            if y > from && y < to {
                heights.push(y);
            }
        }
    }
    heights.sort_by(f64::total_cmp);
    heights.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    Some(heights)
}

/// How many arms a saguaro `height` tall bears: none while it is young, and
/// more the older and taller it grows.
fn arm_count(height: f64, dice: &mut NonCryptoRng) -> u32 {
    let drawn = dice.next_f64() * (0.6 + 0.4 * dice.next_f64());
    if height < 3.2 {
        return 0;
    }
    let most = ((height - 3.0) * 1.1).min(5.0);
    u32::try_from(mathf::round_i32(most * drawn)).unwrap_or(0)
}

/// An arm of `trunk` on a saguaro `height` tall, `ratio` as thick as the
/// trunk where it leaves it, kept clear of the arms already `placed` — each
/// how high it leaves and which way — and where it leaves: narrow from out
/// of the trunk, swelling to its girth, running out and turning up through
/// its elbow, or now and then sagging first, and rising beside the trunk to
/// its own dome.
fn arm(
    trunk: &Stem,
    (height, ratio): (f64, f64),
    (material, ribs): (u16, Ribs),
    placed: &[(f64, f64)],
    dice: &mut NonCryptoRng,
) -> Option<(Stem, (f64, f64))> {
    let (lowest, highest) = ((0.24 * height).max(1.5), 0.66 * height);
    let mut chosen = None;
    for _ in 0..12 {
        let (y, heading) = (range(dice, (lowest, highest)), range(dice, (0.0, TAU)));
        let clear = placed
            .iter()
            .all(|&(other, way)| (other - y).abs() > 1.2 || wrapped(way - heading).abs() > 0.95);
        if clear && highest > lowest {
            chosen = Some((y, heading));
            break;
        }
    }
    let (y, heading) = chosen?;
    let (root, trunk_radius) = along_trunk(trunk, y)?;
    let girth = trunk_radius * ratio * range(dice, (0.9, 1.05));
    let out = Vec3::new(mathf::cos(heading), 0.0, mathf::sin(heading));
    let mut angle = range(dice, (6.0, 32.0)).to_radians();
    let sags = dice.next_f64() < 0.12;
    let mut points = Vec::new();
    points.try_reserve(48).ok()?;
    let mut at = root;
    points.push((at, 0.52 * girth));
    let way = |angle: f64| out * mathf::cos(angle) + Vec3::UP * mathf::sin(angle);
    // Out of the trunk narrow, swelling to its girth once clear of it.
    for (reach, radius) in [
        (0.88 * trunk_radius, 0.56),
        (0.12 * trunk_radius + 0.3 * girth, 0.76),
        (0.45 * girth, 0.95),
    ] {
        at += way(angle) * reach;
        points.push((at, radius * girth));
    }
    let run = range(dice, (0.05, 0.4));
    let runs = mathf::ceil(run / ARM_SEGMENT).max(1.0);
    for _ in 0..u32::try_from(mathf::round_i32(runs)).ok()? {
        at += way(angle) * (run / runs);
        points.push((at, girth));
    }
    if sags {
        let sag = -range(dice, (20.0, 45.0)).to_radians();
        let bend = range(dice, (1.6, 2.6)) * girth;
        turn_through(
            &mut points,
            (&mut at, &mut angle),
            (sag, bend),
            (out, girth),
        )?;
        at += way(angle) * range(dice, (0.1, 0.5));
        points.push((at, girth));
    }
    let bend = range(dice, (1.0, 2.0)) * girth;
    turn_through(
        &mut points,
        (&mut at, &mut angle),
        (FRAC_PI_2, bend),
        (out, girth),
    )?;
    let tall = range(dice, (0.45, 1.0)) * (height - girth - at.y).max(0.4);
    let lean = range(dice, (-4.0, 6.0)).to_radians();
    let upward = Vec3::UP * mathf::cos(lean) + out * mathf::sin(lean);
    let bands = constrictions(dice, (0.1, tall - 0.3), tall);
    let steps = mathf::ceil(tall / ARM_SEGMENT).max(1.0);
    let mut climbed = 0.0;
    for step in 1..=u32::try_from(mathf::round_i32(steps)).ok()? {
        let t = f64::from(step) / steps;
        at += upward * (tall * t - climbed);
        climbed = tall * t;
        points.push((at, girth * (1.0 - 0.08 * t) * pinched(&bands, climbed)));
    }
    Some((
        Stem {
            points,
            side: Vec3::UP.cross(out).normalized(),
            turn: range(dice, (0.0, TAU)),
            material,
            ribs,
        },
        (y, heading),
    ))
}

/// Carry an arm on from `at`, heading `angle` above the level in the upright
/// plane through `out`, turned through to `to` about a bend `bend` in radius,
/// a point every few degrees, `girth` thick.
fn turn_through(
    points: &mut Vec<(Vec3, f64)>,
    (at, angle): (&mut Vec3, &mut f64),
    (to, bend): (f64, f64),
    (out, girth): (Vec3, f64),
) -> Option<()> {
    let sweep = to - *angle;
    let steps = mathf::ceil(sweep.abs().to_degrees() / ELBOW_STEP).max(1.0);
    let step = sweep / steps;
    for _ in 0..u32::try_from(mathf::round_i32(steps)).ok()? {
        let middle = *angle + 0.5 * step;
        *at += (out * mathf::cos(middle) + Vec3::UP * mathf::sin(middle)) * (bend * step.abs());
        *angle += step;
        points.try_reserve(1).ok()?;
        points.push((*at, girth));
    }
    Some(())
}

/// The point on `trunk`'s axis at `height` above the ground, and the trunk's
/// radius there.
fn along_trunk(trunk: &Stem, height: f64) -> Option<(Vec3, f64)> {
    trunk.points.windows(2).find_map(|pair| match *pair {
        [(a, ra), (b, rb)] if a.y <= height && b.y >= height => {
            let t = ((height - a.y) / (b.y - a.y).max(1e-9)).clamp(0.0, 1.0);
            Some((a.lerp(b, t), ra + (rb - ra) * t))
        }
        _ => None,
    })
}

/// Whether `point` lies within `trunk`, a couple of millimetres below its
/// skin.
fn within(trunk: &Stem, point: Vec3) -> bool {
    trunk.points.windows(2).any(|pair| match *pair {
        [(a, ra), (b, rb)] => {
            let span = b - a;
            let t = ((point - a).dot(span) / span.dot(span).max(1e-12)).clamp(0.0, 1.0);
            (point - (a + span * t)).length() < ra + (rb - ra) * t - 0.002
        }
        _ => false,
    })
}

/// The cluster of spines an areole bears at `base` on a skin facing
/// `normal`, its stem running up `toward` the apex, `stem` metres below it,
/// keyed `key`: a stout central pointing out and down, others spreading about
/// it, and a ring of finer radials — as many as `spines` allows, `share` of
/// them kept, fewer and broken the older the stem there — in `material`,
/// each spine's stem carrying its age for its colour.
fn cluster(
    (base, normal, toward): (Vec3, Vec3, Vec3),
    (stem, key): (f64, u32),
    ((material, spines), share): ((u16, Spines), f64),
    parts: &mut Vec<Part>,
    dice: &mut NonCryptoRng,
) -> Option<()> {
    let across = normal.cross(toward).normalized();
    let kept = share * (1.0 - 0.8 * smoothstep(2.4, 7.0, stem));
    let mut spine = |(way, length, thick): (Vec3, f64, f64),
                     from: Vec3,
                     dice: &mut NonCryptoRng|
     -> Option<()> {
        if dice.next_f64() > kept {
            return Some(());
        }
        // An old spine has as often snapped off part way as kept its point.
        let broken = dice.next_f64() < 0.35 * smoothstep(1.5, 5.0, stem);
        let (length, tip) = if broken {
            (length * range(dice, (0.25, 0.7)), 0.45 * thick)
        } else {
            (length, 0.03 * thick)
        };
        parts.try_reserve(1).ok()?;
        parts.push(Part::Tube(Tube::new(
            (from - normal * 0.0015, from + way * length),
            ((thick, tip), (stem, stem)),
            (material, mix32(key ^ dice.next_u32())),
            across,
        )));
        Some(())
    };
    let dip = range(dice, (0.5, 1.05));
    let main = (normal * mathf::cos(dip) - toward * mathf::sin(dip)).normalized();
    let (length, thick) = (range(dice, (0.03, 0.058)), range(dice, (0.0008, 0.0012)));
    spine((main, length, thick), base, dice)?;
    for _ in 1..spines.centrals {
        let around = range(dice, (0.0, TAU));
        let lift = range(dice, (0.3, 0.75));
        let spread = toward * mathf::cos(around) + across * mathf::sin(around);
        let way = (normal * mathf::cos(lift) + spread * mathf::sin(lift)).normalized();
        let (length, thick) = (range(dice, (0.014, 0.032)), range(dice, (0.0005, 0.0008)));
        spine((way, length, thick), base, dice)?;
    }
    let turn = range(dice, (0.0, TAU));
    for radial in 0..spines.radials {
        let around = turn
            + TAU * (f64::from(radial) + 0.3 * dice.next_f64()) / f64::from(spines.radials.max(1));
        let spread = toward * mathf::cos(around) + across * mathf::sin(around);
        let lift = range(dice, (0.15, 0.4));
        let way = (spread * mathf::cos(lift) + normal * mathf::sin(lift)).normalized();
        let (length, thick) = (range(dice, (0.008, 0.019)), range(dice, (0.00025, 0.0004)));
        spine(
            (way, length, thick),
            base + spread * (0.6 * CUSHION.1),
            dice,
        )?;
    }
    Some(())
}

#[cfg(test)]
#[path = "cactus_tests.rs"]
mod tests;
