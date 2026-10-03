//! Trees, grown: a trunk, its limbs, their branches and twigs, each a chain
//! of tapering segments that bends as it grows, leaves along the twigs, and
//! roots flaring into the ground.
//!
//! After Weber and Penn ("Creation and Rendering of Realistic Trees",
//! SIGGRAPH 1995): a species is a crown's envelope and a handful of numbers
//! per level of branching — how many children a stem bears, how long they
//! grow against it, at what angle they leave it and how far round from the
//! last, how they curve and taper — so an oak, a birch and a spruce are the
//! same growth under different numbers. Each tree is grown from its own seed,
//! so no two of a species are alike, into a prototype a scene places again
//! and again.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::leaf::Outline;
use crate::prototype::{Blade, Building, Part, Prototype, Tube, BUILD_UNIT};
use crate::vector::{single, singles, Frame, Vec3};

/// The shape a crown fills: how long a limb grows by where it leaves the
/// trunk, from the crown's top to its foot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Envelope {
    Conical,
    Spherical,
    Hemispherical,
    TaperedCylindrical,
    TendFlame,
}

impl Envelope {
    /// The share of its greatest length a limb reaches that leaves the trunk
    /// `ratio` of the way down the crown, from `0.0` at its top to `1.0` at
    /// its foot (Weber and Penn's shape ratio).
    fn ratio(self, ratio: f64) -> f64 {
        let ratio = ratio.clamp(0.0, 1.0);
        match self {
            Self::Conical => 0.2 + 0.8 * ratio,
            Self::Spherical => 0.2 + 0.8 * mathf::sin(PI * ratio),
            Self::Hemispherical => 0.2 + 0.8 * mathf::sin(FRAC_PI_2 * ratio),
            Self::TaperedCylindrical => 0.5 + 0.5 * ratio,
            Self::TendFlame => {
                if ratio <= 0.7 {
                    0.5 + 0.5 * ratio / 0.7
                } else {
                    0.5 + 0.5 * (1.0 - ratio) / 0.3
                }
            }
        }
    }
}

/// How one level of branching grows.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Level {
    /// Children borne along a stem of the level above.
    pub(crate) branches: f64,
    /// A child's length over its parent's, and how much that varies.
    pub(crate) length: (f64, f64),
    /// How far a stem narrows toward its tip: `0.0` not at all, `1.0` to a
    /// point.
    pub(crate) taper: f64,
    /// The curve of that narrowing: `1.0` straight, as a cone's; lower, a
    /// stem holding its girth further up before it narrows, as a trunk does.
    pub(crate) form: f64,
    /// The angle a child leaves its parent at, in degrees, and its spread.
    pub(crate) down: (f64, f64),
    /// How far round its parent each child stands from the last, in
    /// degrees, and its spread.
    pub(crate) rotate: (f64, f64),
    /// How far a stem bends over its length, in degrees; how far back over
    /// its second half; and how much each segment strays.
    pub(crate) curve: (f64, f64, f64),
    /// The segments a stem is grown in.
    pub(crate) segments: u32,
    /// The chance a stem forks once along its length, and the angle between
    /// its two arms, in degrees.
    pub(crate) fork: (f64, f64),
}

/// How a species' leaves are borne and look.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Leafing {
    pub(crate) outline: Outline,
    /// Leaves along each twig.
    pub(crate) per_twig: u32,
    /// A leaf's length, and its greatest half-width over its length.
    pub(crate) length: f64,
    pub(crate) breadth: f64,
    /// How far its halves tilt up from the midrib.
    pub(crate) fold: f64,
    /// The angle a leaf's stalk leaves the twig at, in degrees.
    pub(crate) angle: f64,
    /// How strongly a leaf turns its face to the sky.
    pub(crate) toward_light: f64,
}

/// A kind of tree.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Species {
    pub(crate) envelope: Envelope,
    /// Its least and greatest height, in metres.
    pub(crate) height: (f64, f64),
    /// The share of the trunk bare of limbs.
    pub(crate) base: f64,
    /// The trunk's radius over the tree's height, and how far it flares at
    /// the ground.
    pub(crate) girth: f64,
    pub(crate) flare: f64,
    /// The stubs of dead branches a metre up the bare bole below the crown.
    pub(crate) stubs: f64,
    /// How a child's radius follows its length against its parent's.
    pub(crate) ratio_power: f64,
    /// Trunks rising from the ground: one for a tree, several for a shrub.
    pub(crate) trunks: u32,
    /// The trunk and the levels branching from it.
    pub(crate) levels: [Level; 4],
    /// How many of the levels grow: the last bears the leaves.
    pub(crate) depth: usize,
    /// How strongly stems turn upward, or droop where negative, and how much
    /// more so the twigs.
    pub(crate) attraction: (f64, f64),
    pub(crate) leafing: Leafing,
    /// Whether it keeps its leaves through the winter.
    pub(crate) evergreen: bool,
}

impl Species {
    /// The level its twigs grow at, which bears its leaves.
    fn twigs(&self) -> Option<Level> {
        self.levels.get(self.depth.checked_sub(1)?).copied()
    }
}

/// The time of year a tree is grown for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Season {
    Spring,
    Summer,
    /// Its leaves turned, and this share of them fallen.
    Autumn {
        fallen: u8,
    },
    Winter,
}

/// The materials a tree is made in: its bark, and its leaves.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Stock {
    pub(crate) bark: u16,
    pub(crate) leaves: u16,
}

/// The most parts one tree may hold: what a forest of its kind costs. A tree
/// in leaf holds nearly this many, most of them leaves at their real size.
pub(crate) const MOST_PARTS: usize = 150_000;

/// A stem being grown.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Stem {
    level: usize,
    base: Vec3,
    frame: Frame,
    length: f64,
    radius: f64,
    /// How far along the tree's whole path from the ground its base lies,
    /// for the bark.
    travelled: f64,
    /// Whether it has forked already.
    forked: bool,
    /// The share of its whole stem below its crown: up to there it narrows
    /// by its level's taper and form, then straight to its tip as it gives
    /// its girth to its limbs. A limb's is the whole of it.
    crown: f64,
    /// The share of its whole stem below its own base: nought but for a
    /// fork's arms, which carry on its narrowing from where it forked.
    from: f64,
}

/// Heights up a tree's trunk, in its radii, at which its foot is cut, so
/// the swell of its flare is drawn as it curves rather than as one cone.
const FOOT: [f64; 4] = [0.25, 0.6, 1.2, 2.2];

/// How far up a trunk, in its radii, its flare falls to a third of its
/// swell at the ground.
const FLARE_REACH: f64 = 1.1;

/// How far up its bare bole, as a share of the way to its crown, a trunk
/// still carries the stubs of its dead branches: below that they have long
/// rotted away.
const STUBS_FROM: f64 = 0.2;

/// The share of its stray a trunk bends by in its one sweep: a trunk bows
/// gently, where a limb wanders.
const TRUNK_SWEEP: f64 = 0.35;

/// Stems grown in one step of a tree's growth.
const STEMS_PER_STEP: usize = 48;

/// A tree growing: its stems level by level, then its prototype's hierarchy.
#[derive(Debug)]
pub(crate) struct Growth {
    grower: Grower,
    height: f64,
    stage: Stage,
}

#[derive(Debug)]
enum Stage {
    /// Growing the stems of `level`, those of the next gathering behind.
    Stems {
        level: usize,
        queue: Vec<Stem>,
        next: Vec<Stem>,
    },
    Index(Building),
    Done(Prototype),
    /// The heap refused it, or it has been taken.
    Gone,
}

impl Growth {
    /// A tree of `species` `height` tall in `season`, made in `stock`, grown
    /// from `seed`; `None` when the heap will not hold its first stems.
    pub(crate) fn new(
        species: &Species,
        height: f64,
        (season, stock): (Season, Stock),
        seed: u64,
    ) -> Option<Self> {
        let mut grower = Grower {
            species: *species,
            season,
            stock,
            dice: NonCryptoRng::seed_from_u64(seed),
            parts: Vec::new(),
            key: 0,
            leaf_share: 1.0,
        };
        grower.parts.try_reserve(4096).ok()?;
        let queue = grower.trunks(height)?;
        Some(Self {
            grower,
            height,
            stage: Stage::Stems {
                level: 0,
                queue,
                next: Vec::new(),
            },
        })
    }

    /// Grow a bounded step more; whether the tree is grown, or `None` when
    /// the heap refused it.
    pub(crate) fn step(&mut self) -> Option<bool> {
        let stage = core::mem::replace(&mut self.stage, Stage::Gone);
        self.stage = match stage {
            Stage::Stems {
                level,
                mut queue,
                mut next,
            } => {
                for _ in 0..STEMS_PER_STEP {
                    let Some(stem) = queue.pop() else {
                        break;
                    };
                    self.grower.grow_stem(stem, self.height, &mut next)?;
                }
                if !queue.is_empty() {
                    Stage::Stems { level, queue, next }
                } else if level + 1 < self.grower.species.depth && !next.is_empty() {
                    // The next level is grown in the order it was borne.
                    next.reverse();
                    if level + 2 == self.grower.species.depth {
                        self.grower.budget_leaves(next.len());
                    }
                    Stage::Stems {
                        level: level + 1,
                        queue: next,
                        next: Vec::new(),
                    }
                } else {
                    let parts = core::mem::take(&mut self.grower.parts);
                    Stage::Index(Prototype::building(parts, Vec::new(), Vec::new())?)
                }
            }
            Stage::Index(mut building) => {
                if building.step(BUILD_UNIT) {
                    Stage::Done(building.finish())
                } else {
                    Stage::Index(building)
                }
            }
            done @ Stage::Done(_) => done,
            Stage::Gone => return None,
        };
        Some(matches!(self.stage, Stage::Done(_)))
    }

    /// The grown tree.
    pub(crate) fn finish(self) -> Option<Prototype> {
        match self.stage {
            Stage::Done(prototype) => Some(prototype),
            Stage::Stems { .. } | Stage::Index(_) | Stage::Gone => None,
        }
    }
}

#[derive(Debug)]
struct Grower {
    species: Species,
    season: Season,
    stock: Stock,
    dice: NonCryptoRng,
    parts: Vec<Part>,
    key: u32,
    /// The share of its leaves each twig keeps, so a tree grown past its
    /// budget thins evenly rather than going bare on one side.
    leaf_share: f64,
}

impl Grower {
    fn unit(&mut self) -> f64 {
        self.dice.next_f64()
    }

    fn range(&mut self, (low, high): (f64, f64)) -> f64 {
        low + (high - low) * self.unit()
    }

    /// A value about `mean`, `spread` either way.
    fn about(&mut self, (mean, spread): (f64, f64)) -> f64 {
        mean + spread * (2.0 * self.unit() - 1.0)
    }

    fn push(&mut self, part: Part) -> Option<()> {
        if self.parts.len() >= MOST_PARTS {
            return Some(());
        }
        self.parts.try_reserve(1).ok()?;
        self.parts.push(part);
        Some(())
    }

    /// Before its `twigs` are grown, share out what is left of the budget so
    /// every twig keeps the same share of its leaves.
    fn budget_leaves(&mut self, twigs: usize) {
        let segments = self
            .species
            .twigs()
            .map_or(1, |twigs| twigs.segments.max(1)) as usize;
        let tubes = twigs.saturating_mul(segments);
        let leaves = twigs.saturating_mul(self.species.leafing.per_twig as usize);
        let room = MOST_PARTS.saturating_sub(self.parts.len() + tubes);
        self.leaf_share = if leaves > room {
            crate::vector::real(room) / crate::vector::real(leaves.max(1))
        } else {
            1.0
        };
    }

    /// Whether this tree carries leaves, and what share of them.
    fn leafiness(&self) -> f64 {
        match self.season {
            Season::Spring | Season::Summer => 1.0,
            Season::Autumn { fallen } => 1.0 - f64::from(fallen) / 100.0,
            Season::Winter => {
                if self.species.evergreen {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    /// The tree's trunks, `height` tall, and the roots at their feet.
    fn trunks(&mut self, height: f64) -> Option<Vec<Stem>> {
        let species = self.species;
        let trunks = species.trunks.max(1);
        let mut queue: Vec<Stem> = Vec::new();
        queue.try_reserve(usize::try_from(trunks).ok()?).ok()?;
        for index in 0..trunks {
            let (lean, turn) = if trunks == 1 {
                (self.about((0.0, 4.0)), self.range((0.0, TAU)))
            } else {
                (
                    self.range((10.0, 35.0)),
                    TAU * f64::from(index) / f64::from(trunks) + self.about((0.0, 0.4)),
                )
            };
            let frame = Frame::turned(turn, lean.to_radians());
            let length = height
                * if trunks == 1 {
                    1.0
                } else {
                    self.range((0.7, 1.0))
                };
            let radius = height * species.girth / mathf::sqrt(f64::from(trunks));
            self.roots(Vec3::ZERO, radius)?;
            queue.push(Stem {
                level: 0,
                base: Vec3::new(0.0, -0.2 * radius, 0.0),
                frame,
                length,
                radius,
                travelled: 0.0,
                forked: false,
                crown: species.base,
                from: 0.0,
            });
        }
        Some(queue)
    }

    /// Roots spreading from the foot of a trunk of `radius` at `base`: each a
    /// spur swelling out of the flare down to the ground, then a run along it
    /// tapering as it dips into the soil.
    fn roots(&mut self, base: Vec3, radius: f64) -> Option<()> {
        if self.species.trunks > 1 || radius < 0.04 {
            return Some(());
        }
        let count = 5 + (self.dice.next_u32() % 4);
        let turn = self.range((0.0, TAU));
        for index in 0..count {
            let around = turn + TAU * f64::from(index) / f64::from(count) + self.about((0.0, 0.35));
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let side = Vec3::new(-out.z, 0.0, out.x);
            let thick = radius * self.range((0.22, 0.34));
            let from = base + out * (0.35 * radius) + Vec3::UP * (radius * self.range((0.8, 1.4)));
            let knee = base + out * (1.25 * radius) + Vec3::UP * (0.1 * radius);
            let tip = base
                + out * (radius * self.range((2.2, 3.6)))
                + side * (radius * self.about((0.0, 0.5)))
                - Vec3::UP * (0.3 * radius);
            let (spur, run) = ((knee - from).length(), (tip - knee).length());
            let key = self.next_key();
            for (a, b, radii, stem) in [
                (from, knee, (thick, 0.92 * thick), (0.0, spur)),
                (knee, tip, (0.92 * thick, 0.25 * thick), (spur, spur + run)),
            ] {
                // Both runs bend in the upright plane through `out`, which
                // `side` stands square to.
                self.push(Part::Tube(Tube::new(
                    (a, b),
                    (radii, stem),
                    (self.stock.bark, key),
                    side,
                )))?;
            }
        }
        Some(())
    }

    /// A tree's trunk's first segment, from `base` up `frame`'s `y`, cut
    /// short where its flare swells so the swell is drawn as it curves;
    /// `travelled` metres along the tree's path at its foot.
    fn foot(
        &mut self,
        stem: Stem,
        level: &Level,
        (base, frame): (Vec3, Frame),
        travelled: f64,
        key: u32,
    ) -> Option<()> {
        let segment = stem.length / f64::from(level.segments.max(1));
        let flare = self.species.flare;
        let radius = |rise: f64| radius_at(stem, level, rise / stem.length, flare);
        let cuts = FOOT.map(|share| share * stem.radius);
        let mut last = 0.0;
        for rise in cuts
            .into_iter()
            .filter(|&rise| rise < segment)
            .chain([segment])
        {
            self.push(Part::Tube(Tube::new(
                (base + frame.y * last, base + frame.y * rise),
                (
                    (radius(last), radius(rise)),
                    (travelled + last, travelled + rise),
                ),
                (self.stock.bark, key),
                frame.x,
            )))?;
            last = rise;
        }
        Some(())
    }

    /// The stubs of the dead branches a trunk shed as its crown rose, strewn
    /// up its bare bole where it runs from `from` to `to` in `frame`, `s0` to
    /// `s1` of its length and `travelled` metres along the tree's path at
    /// `from`: most snapped short, now and then one longer, drooping.
    fn stubs(
        &mut self,
        stem: Stem,
        level: &Level,
        (from, to, frame): (Vec3, Vec3, Frame),
        (s0, s1, travelled): (f64, f64, f64),
        height: f64,
    ) -> Option<()> {
        let species = self.species;
        let crown = species.base * stem.length;
        let low = (s0 * stem.length).max(STUBS_FROM * crown);
        let high = (s1 * stem.length).min(crown);
        if high <= low || species.stubs <= 0.0 {
            return Some(());
        }
        let count = species.stubs * (high - low);
        let whole = mathf::floor(count);
        let stubs = u32::try_from(mathf::round_i32(whole)).unwrap_or(0)
            + u32::from(self.unit() < count - whole);
        let span = ((s1 - s0) * stem.length).max(1e-9);
        let scale = (height / 20.0).clamp(0.4, 1.2);
        for _ in 0..stubs {
            let rise = self.range((low, high));
            let bole = radius_at(stem, level, rise / stem.length, 0.0);
            let around = self.range((0.0, TAU));
            let out = frame.x * mathf::cos(around) + frame.z * mathf::sin(around);
            let droop = self.range((-0.6, 0.05));
            let dir = (out * mathf::cos(droop) + frame.y * mathf::sin(droop)).normalized();
            let length = if self.unit() < 0.12 {
                self.range((0.25, 0.6))
            } else {
                self.range((0.03, 0.14))
            } * scale;
            let thick = (bole * self.range((0.1, 0.2))).max(0.012);
            let inside = rise - s0 * stem.length;
            let start = from + (to - from) * (inside / span) + out * (0.5 * bole);
            let reach = 0.5 * bole + length;
            let tip = thick * self.range((0.4, 0.8));
            let key = self.next_key();
            self.push(Part::Tube(Tube::new(
                (start, start + dir * reach),
                (
                    (thick, tip),
                    (travelled + inside, travelled + inside + reach),
                ),
                (self.stock.bark, key),
                frame.y,
            )))?;
        }
        Some(())
    }

    fn next_key(&mut self) -> u32 {
        self.key = self.key.wrapping_add(1);
        crate::sample::mix32(self.key ^ self.dice.next_u32())
    }

    /// Grow `stem`, pushing its children for the next level into `next`.
    fn grow_stem(&mut self, stem: Stem, height: f64, next: &mut Vec<Stem>) -> Option<()> {
        let species = self.species;
        let level = *species.levels.get(stem.level)?;
        let last = stem.level + 1 >= species.depth;
        let segments = level.segments.max(1);
        let stride = stem.length / f64::from(segments);
        let attraction = if last {
            species.attraction.1
        } else {
            species.attraction.0
        };
        let mut frame = stem.frame;
        let mut at = stem.base;
        let mut travelled = stem.travelled;
        let rooted = stem.level == 0 && !stem.forked;
        // A trunk bends in one gentle sweep that turns as it rises; a limb
        // strays every which way.
        let mut sweep = self.range((0.0, TAU));
        let children = if last {
            0
        } else {
            self.children(stem, height)?
        };
        // Children spread along the stem, the trunk's above its bare base.
        let start = if stem.level == 0 && !stem.forked {
            species.base
        } else {
            0.1
        };
        let mut placed = 0;
        let mut around = self.range((0.0, TAU));
        for segment in 0..segments {
            let s0 = f64::from(segment) / f64::from(segments);
            let s1 = f64::from(segment + 1) / f64::from(segments);
            let r0 = radius_at(stem, &level, s0, species.flare);
            let r1 = radius_at(stem, &level, s1, species.flare);
            frame = self.bend(frame, (stem.level, &level, s0), &mut sweep);
            // Then drawn toward the light, or weighed down.
            if attraction.abs() > 0.0 {
                let pull = Vec3::UP * (attraction / f64::from(segments));
                let dir = (frame.y + pull).normalized();
                frame = frame.aligning(frame.y, dir);
            }
            let end = at + frame.y * stride;
            let key = self.next_key();
            if rooted && segment == 0 {
                self.foot(stem, &level, (at, frame), travelled, key)?;
            } else {
                self.push(Part::Tube(Tube::new(
                    (at, end),
                    ((r0, r1), (travelled, travelled + stride)),
                    (self.stock.bark, key),
                    frame.x,
                )))?;
            }
            if rooted {
                self.stubs(stem, &level, (at, end, frame), (s0, s1, travelled), height)?;
            }
            // Children leaving along this segment.
            while placed < children {
                let offset =
                    start + (1.0 - start) * (f64::from(placed) + 0.5) / f64::from(children);
                if offset >= s1 {
                    break;
                }
                let local = ((offset - s0) / (s1 - s0)).clamp(0.0, 1.0);
                let point = at + (end - at) * local;
                let child = self.child(stem, frame, (point, offset), &mut around, height)?;
                next.try_reserve(1).ok()?;
                next.push(child);
                placed += 1;
            }
            if last {
                self.leaves_along(at, end, frame, (s0, stem.length))?;
            }
            // A fork: the stem's remaining length grows on as two arms.
            if !stem.forked
                && segment + 2 < segments
                && self.unit() < level.fork.0 / f64::from(segments)
            {
                let rest = Stem {
                    level: stem.level,
                    base: end,
                    frame,
                    length: stem.length * (1.0 - s1),
                    radius: r1,
                    travelled: travelled + stride,
                    forked: true,
                    crown: stem.crown,
                    from: stem.from + s1 * (1.0 - stem.from),
                };
                return self.fork(rest, level.fork.1, height, next);
            }
            at = end;
            travelled += stride;
        }
        Some(())
    }

    /// `frame` bent for the segment `s0` of the way along a stem of `depth`,
    /// grown at `level`: a trunk in its one gentle sweep, whose way `sweep`
    /// turns as it rises; a limb by its level's curve, back over its second
    /// half, and a stray of its own every which way.
    fn bend(
        &mut self,
        frame: Frame,
        (depth, level, s0): (usize, &Level, f64),
        sweep: &mut f64,
    ) -> Frame {
        let (curve, back, stray) = level.curve;
        let per = f64::from(level.segments.max(1));
        if depth == 0 {
            *sweep += self.about((0.0, 0.5));
            let bend = TRUNK_SWEEP * stray / per * self.range((0.2, 1.0));
            bent(frame, bend.to_radians(), *sweep)
        } else {
            let bend = if s0 < 0.5 { curve } else { back } / per + self.about((0.0, stray / per));
            bent(frame, bend.to_radians(), self.range((0.0, TAU)))
        }
    }

    /// How many children `stem` bears, on a tree `height` tall.
    fn children(&mut self, stem: Stem, height: f64) -> Option<u32> {
        // An arm of a fork shares its stem's children with its twin.
        let share = if stem.forked { 0.6 } else { 1.0 };
        let branches = self.species.levels.get(stem.level + 1)?.branches * share;
        // A short stem bears fewer children than a long one of its level.
        let count = if stem.level == 0 {
            branches
        } else {
            branches * (0.5 + 0.5 * (stem.length / (height * 0.25)).min(1.0))
        };
        let whole = mathf::floor(count);
        let extra = u32::from(self.unit() < count - whole);
        Some(u32::try_from(mathf::round_i32(whole)).unwrap_or(0) + extra)
    }

    /// The rest of a stem, `rest`, grown on as two arms `angle` degrees
    /// apart, each carrying half its cross-section.
    fn fork(&mut self, rest: Stem, angle: f64, height: f64, next: &mut Vec<Stem>) -> Option<()> {
        let angle = angle.to_radians();
        let twist = self.range((0.0, TAU));
        for side in [-1.0, 1.0] {
            let arm = Stem {
                frame: bent(rest.frame, 0.5 * angle * side, twist),
                radius: rest.radius / core::f64::consts::SQRT_2,
                ..rest
            };
            self.grow_stem(arm, height, next)?;
        }
        Some(())
    }

    /// A child of `parent` leaving at `point`, `offset` of the way along it,
    /// while the parent's frame there is `frame`.
    fn child(
        &mut self,
        parent: Stem,
        frame: Frame,
        (point, offset): (Vec3, f64),
        around: &mut f64,
        height: f64,
    ) -> Option<Stem> {
        let species = self.species;
        let level = *species.levels.get(parent.level + 1)?;
        let length = if parent.level == 0 {
            let crown = (1.0 - offset) / (1.0 - species.base).max(1e-3);
            parent.length * self.about(level.length).max(0.02) * species.envelope.ratio(crown)
        } else {
            self.about(level.length).max(0.02) * (parent.length - 0.6 * offset * parent.length)
        };
        *around += self.about(level.rotate).to_radians();
        let down = self.about(level.down).to_radians();
        let child_frame = bent(frame, down, *around);
        let parent_radius = radius_at(parent, species.levels.get(parent.level)?, offset, 0.0);
        let radius = (parent_radius
            * mathf::exp(species.ratio_power * mathf::ln((length / parent.length).max(1e-6))))
        .min(parent_radius * 0.9);
        Some(Stem {
            level: parent.level + 1,
            base: point,
            frame: child_frame,
            length: length.max(0.01 * height),
            radius,
            travelled: 0.0,
            forked: false,
            crown: 1.0,
            from: 0.0,
        })
    }

    /// The leaves along a twig's segment from `from` to `to`, `s0` of the way
    /// along a twig `length` long, its frame `frame`.
    fn leaves_along(
        &mut self,
        from: Vec3,
        to: Vec3,
        frame: Frame,
        (s0, length): (f64, f64),
    ) -> Option<()> {
        let leafing = self.species.leafing;
        let keep = self.leafiness() * self.leaf_share;
        if keep <= 0.0 {
            return Some(());
        }
        let segments = f64::from(self.species.twigs()?.segments.max(1));
        let count = f64::from(leafing.per_twig) / segments;
        let whole = mathf::floor(count);
        let extra = u32::from(self.unit() < count - whole);
        let leaves = u32::try_from(mathf::round_i32(whole)).unwrap_or(0) + extra;
        let twig_length = length.max(1e-3);
        for index in 0..leaves {
            if self.unit() > keep {
                continue;
            }
            let along = (f64::from(index) + self.unit()) / f64::from(leaves.max(1));
            // Leaves crowd toward a twig's tip, and none near its base.
            if s0 + along / segments < 0.25 {
                continue;
            }
            let base = from + (to - from) * along;
            self.leaf(base, frame, twig_length)?;
        }
        Some(())
    }

    /// One leaf borne at `base` on a twig whose frame is `frame`.
    fn leaf(&mut self, base: Vec3, frame: Frame, twig: f64) -> Option<()> {
        let leafing = self.species.leafing;
        let around = self.range((0.0, TAU));
        let angle = self.about((leafing.angle, 15.0)).to_radians();
        let out = frame.x * mathf::cos(around) + frame.z * mathf::sin(around);
        let axis = (frame.y * mathf::cos(angle) + out * mathf::sin(angle)).normalized();
        // A leaf turns its face up, toward the sky and the light, as far as
        // its stalk lets it.
        let face = (Vec3::UP * leafing.toward_light
            + out * (1.0 - leafing.toward_light)
            + Vec3::new(self.about((0.0, 0.3)), 0.0, self.about((0.0, 0.3))))
        .normalized();
        let normal = (face - axis * face.dot(axis)).normalized();
        if normal.length() < 0.5 {
            return Some(());
        }
        let size = self.range((0.8, 1.2)) * leafing.length.min(twig * 1.5);
        let fold = leafing.fold * self.range((0.6, 1.2));
        let key = self.next_key();
        self.push(Part::Leaf(Blade {
            base: singles(base),
            normal: singles(normal),
            axis: singles(axis),
            length: single(size),
            width: single(size * leafing.breadth),
            outline: leafing.outline,
            fold: single(fold),
            material: self.stock.leaves,
            key,
        }))
    }
}

/// The radius `s` of the way along `stem`, grown at `level`: narrowing up
/// its bole by the level's taper, along its form, then within its crown
/// straight to its tip, a fork's arm carrying on from where its stem forked;
/// and about the foot of a tree's trunk swelling by `flare` toward the
/// ground.
fn radius_at(stem: Stem, level: &Level, s: f64, flare: f64) -> f64 {
    let bole = |s: f64| mathf::exp(level.form * mathf::ln((1.0 - level.taper * s).max(0.05)));
    let narrowed = |s: f64| {
        if s <= stem.crown {
            bole(s)
        } else {
            bole(stem.crown) * ((1.0 - s) / (1.0 - stem.crown).max(1e-6)).max(0.04)
        }
    };
    let whole = stem.from + s * (1.0 - stem.from);
    let tapered = stem.radius * narrowed(whole) / narrowed(stem.from);
    if stem.level == 0 && !stem.forked && flare > 0.0 {
        let up = s * stem.length / (FLARE_REACH * stem.radius).max(1e-6);
        tapered * (1.0 + flare * mathf::exp(-up))
    } else {
        tapered
    }
}

/// `frame` turned `angle` radians away from its own axis `y`, toward the
/// side `around` names.
fn bent(frame: Frame, angle: f64, around: f64) -> Frame {
    let side = frame.x * mathf::cos(around) + frame.z * mathf::sin(around);
    let dir = (frame.y * mathf::cos(angle) + side * mathf::sin(angle)).normalized();
    frame.aligning(frame.y, dir)
}

/// A saguaro: a ribbed column rounded at its top, with up to three arms
/// turned up from elbows, grown from `seed` in `stock`'s flesh, its
/// hierarchy still to build; `None` when the heap will not hold it.
pub(crate) fn saguaro(height: f64, stock: Stock, seed: u64) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut parts: Vec<Part> = Vec::new();
    parts.try_reserve(64).ok()?;
    let girth = height * (0.045 + 0.02 * dice.next_f64());
    let mut key = crate::sample::mix32(u32::try_from(seed >> 32).unwrap_or(0));
    // Its ribs run on round an arm's elbow when both of its runs are begun
    // from the side square to the upright plane they bend in.
    let limb = |parts: &mut Vec<Part>, (from, to): (Vec3, Vec3), radii: (f64, f64), (key, side)| {
        parts.push(Part::Tube(Tube::new(
            (from, to),
            (radii, (0.0, (to - from).length())),
            (stock.bark, key),
            side,
        )));
    };
    limb(
        &mut parts,
        (Vec3::new(0.0, -0.3, 0.0), Vec3::UP * (height - girth)),
        (girth, 0.92 * girth),
        (key, Vec3::new(1.0, 0.0, 0.0)),
    );
    let arms = dice.next_u32() % 4;
    for arm in 0..arms {
        key = crate::sample::mix32(key ^ arm);
        let heading = TAU * dice.next_f64();
        let out = Vec3::new(mathf::cos(heading), 0.0, mathf::sin(heading));
        let joint = Vec3::UP * (height * (0.35 + 0.25 * dice.next_f64()));
        let thick = 0.72 * girth;
        let reach = out * (0.35 + 0.3 * dice.next_f64()) * height * 0.2;
        let elbow = joint + reach + Vec3::UP * (0.15 * height * 0.2);
        let rise = Vec3::UP * (height * (0.15 + 0.22 * dice.next_f64()));
        let side = Vec3::new(-out.z, 0.0, out.x);
        limb(&mut parts, (joint, elbow), (thick, thick), (key, side));
        limb(
            &mut parts,
            (elbow, elbow + rise),
            (thick, 0.9 * thick),
            (mix_key(key), side),
        );
    }
    Prototype::building(parts, Vec::new(), Vec::new())
}

fn mix_key(key: u32) -> u32 {
    crate::sample::mix32(key ^ 0x5bd1_e995)
}

/// Leaflets either side of each tenth of a palm frond's rachis.
const LEAFLETS: u32 = 5;

/// A palm, `height` tall, its trunk curving away from the vertical, and a
/// crown of `fronds` fronds, its hierarchy still to build; `None` when the
/// heap will not hold it.
pub(crate) fn palm(height: f64, stock: Stock, fronds: u16, seed: u64) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut parts: Vec<Part> = Vec::new();
    parts.try_reserve(4096).ok()?;
    let lean = (8.0 + 22.0 * dice.next_f64()).to_radians();
    let turn = TAU * dice.next_f64();
    let side = Vec3::new(mathf::cos(turn), 0.0, mathf::sin(turn));
    let segments = 24u32;
    let mut at = Vec3::ZERO;
    let radius = 0.018 * height;
    let mut dir = Vec3::UP;
    let mut key = crate::sample::mix32(u32::try_from(seed & 0xffff_ffff).unwrap_or(0));
    for segment in 0..segments {
        let s = f64::from(segment) / f64::from(segments);
        // Curving away from the vertical, most at its foot.
        let bend = lean * (1.0 - s) * 2.0 / f64::from(segments);
        dir = (dir * mathf::cos(bend) + side * mathf::sin(bend)).normalized();
        let end = at + dir * (height / f64::from(segments));
        key = crate::sample::mix32(key ^ segment);
        parts.try_reserve(1).ok()?;
        // The trunk bends in the upright plane through `side`, square to which
        // its bark is begun.
        parts.push(Part::Tube(Tube::new(
            (at, end),
            (
                (
                    radius * (1.35 - 0.4 * s),
                    radius * (1.35 - 0.4 * s - 0.4 / f64::from(segments)),
                ),
                (s * height, (s + 1.0 / f64::from(segments)) * height),
            ),
            (stock.bark, key),
            Vec3::UP.cross(side),
        )));
        at = end;
    }
    let crown = at;
    for frond in 0..fronds {
        let around = TAU * f64::from(frond) / f64::from(fronds) + 0.3 * dice.next_f64();
        let droop = (20.0 + 70.0 * dice.next_f64()).to_radians();
        let shape = Frond {
            length: height * (0.28 + 0.1 * dice.next_f64()),
            rise: 0.4 * droop,
            droop: 1.2 * droop,
            rachis: 0.012 * height,
            bare: 0.0,
            leaflet: (0.22, 0.13),
            outline: Outline::Lanceolate,
            hang: 0.35,
        };
        key = crate::sample::mix32(key ^ (u32::from(frond) << 8));
        grow_frond(&mut parts, (crown, around), &shape, (stock, key), &mut dice)?;
    }
    Prototype::building(parts, Vec::new(), Vec::new())
}

/// How a frond grows: how long it is; how far from the vertical it sets out
/// and how far it droops over its length, in radians; how thick its rachis
/// is; the share of it bare of leaflets at its foot; its leaflets' length
/// at its foot as a share of its own, and their breadth as a share of
/// theirs; their outline; and how far they hang from the rachis.
struct Frond {
    length: f64,
    rise: f64,
    droop: f64,
    rachis: f64,
    bare: f64,
    leaflet: (f64, f64),
    outline: Outline,
    hang: f64,
}

/// Grow a frond shaped as `shape` from `from`, leaving it `around` the
/// vertical, into `parts`, in `stock` and keyed from `key`.
fn grow_frond(
    parts: &mut Vec<Part>,
    (from, around): (Vec3, f64),
    shape: &Frond,
    (stock, key): (Stock, u32),
    dice: &mut NonCryptoRng,
) -> Option<()> {
    let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
    let steps = 10u32;
    let mut point = from;
    let mut key = key;
    let mut heading =
        (Vec3::UP * mathf::cos(shape.rise) + out * mathf::sin(shape.rise)).normalized();
    for step in 0..steps {
        let s = f64::from(step) / f64::from(steps);
        heading = (heading - Vec3::UP * (shape.droop / f64::from(steps))).normalized();
        let next = point + heading * (shape.length / f64::from(steps));
        key = crate::sample::mix32(key ^ step);
        parts.try_reserve(1 + 2 * LEAFLETS as usize).ok()?;
        parts.push(Part::Tube(Tube::new(
            (point, next),
            (
                (
                    shape.rachis * (1.0 - 0.8 * s),
                    shape.rachis * (1.0 - 0.8 * (s + 0.1)),
                ),
                (0.0, 0.0),
            ),
            (stock.bark, key),
            Vec3::UP.cross(out),
        )));
        if s >= shape.bare {
            // Leaflets either side of the rachis, hanging from it, shorter
            // toward its tip.
            let across = heading.cross(Vec3::UP).normalized();
            let leafy = (s - shape.bare) / (1.0 - shape.bare).max(1e-6);
            for side in [-1.0, 1.0] {
                for pair in 0..LEAFLETS {
                    let base =
                        point + (next - point) * ((f64::from(pair) + 0.5) / f64::from(LEAFLETS));
                    let axis = (across * side + heading * 0.35 - Vec3::UP * (shape.hang + 0.4 * s))
                        .normalized();
                    let normal = (axis.cross(heading) * side).normalized();
                    let normal = if normal.y < 0.0 { -normal } else { normal };
                    let leaflet = shape.length
                        * shape.leaflet.0
                        * (1.0 - 0.55 * leafy)
                        * (0.85 + 0.3 * dice.next_f64());
                    key = crate::sample::mix32(key ^ pair ^ 0x5a);
                    parts.push(Part::Leaf(Blade {
                        base: singles(base),
                        normal: singles(normal),
                        axis: singles(axis),
                        length: single(leaflet),
                        width: single(leaflet * shape.leaflet.1),
                        outline: shape.outline,
                        fold: 0.35,
                        material: stock.leaves,
                        key,
                    }));
                }
            }
        }
        point = next;
    }
    Some(())
}

/// A fern, `height` tall: a shuttlecock of `fronds` fronds rising from the
/// ground and arching out, bare at their feet, their pinnae each a row of
/// pinnules, its hierarchy still to build; `None` when the heap will not
/// hold it.
pub(crate) fn fern(height: f64, stock: Stock, fronds: u16, seed: u64) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut parts: Vec<Part> = Vec::new();
    parts
        .try_reserve(usize::from(fronds) * (1 + 2 * LEAFLETS as usize) * 10)
        .ok()?;
    let mut key = crate::sample::mix32(u32::try_from(seed >> 32).unwrap_or(0) ^ 0xf3);
    for frond in 0..fronds {
        let around = TAU * (f64::from(frond) + 0.6 * dice.next_f64()) / f64::from(fronds);
        // The inner fronds stand up, the outer arch over.
        let rise = (18.0 + 32.0 * dice.next_f64()).to_radians();
        let shape = Frond {
            length: 1.25 * height * (0.8 + 0.35 * dice.next_f64()),
            rise,
            droop: (35.0 + 45.0 * dice.next_f64()).to_radians(),
            rachis: 0.006 * height,
            bare: 0.2,
            leaflet: (0.2, 0.3),
            outline: Outline::Shoot { count: 12 },
            hang: 0.05,
        };
        key = crate::sample::mix32(key ^ (u32::from(frond) << 8));
        grow_frond(
            &mut parts,
            (Vec3::ZERO, around),
            &shape,
            (stock, key),
            &mut dice,
        )?;
    }
    Prototype::building(parts, Vec::new(), Vec::new())
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
