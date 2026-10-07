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

use crate::flare::{Flare, MOST_LOBES};
use crate::foot::{Foot, FLARE_TOP, ROOTS};
use crate::fracture::{snapped, Grain};
use crate::leaf::Outline;
use crate::prototype::{Assembly, Blade, Building, Part, Prototype, Tube, BUILD_UNIT};
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
    /// Whether it stands dead, its trunk and its limbs snapped off.
    pub(crate) snapped: bool,
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

/// The materials a tree is made in: its bark, its leaves, and the wood its
/// breaks show; and the fruit it bears in its season, if it does.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Stock {
    pub(crate) bark: u16,
    pub(crate) leaves: u16,
    pub(crate) grain: Grain,
    pub(crate) fruit: Option<Fruit>,
}

/// A tree's fruit: the material it is in, how far across it is, and the
/// share of the places along its twigs a leaf could stand that one hangs
/// at.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Fruit {
    pub(crate) material: u16,
    pub(crate) radius: f64,
    pub(crate) share: f64,
}

/// The most parts one tree may hold: what a forest of its kind costs. A tree
/// in leaf holds nearly this many, most of them leaves at their real size.
pub(crate) const MOST_PARTS: usize = 150_000;

/// The share of a twig's length from its base that bears no leaves.
const LEAFLESS_FOOT: f64 = 0.25;

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
    /// How far up it from its base a trunk comes out into the open, out of
    /// the ground or out of the stool a shrub's stems rise from.
    emerges: f64,
}

/// How long a living tree's dead stubs, and a dead tree's breaks, have stood
/// weathering, as a break's age.
const SNAPPED_AGE: f64 = 0.4;

/// The stool a shrub's stems rise from: its radius.
#[derive(Copy, Clone, Debug)]
struct Stool {
    radius: f64,
}

/// How far a shrub's stool flares toward the roots it spreads, and how far
/// beneath the ground its stems rise from, in its radii.
const STOOL_FLARE: f64 = 0.35;
const STOOL_SUNK: f64 = 0.3;

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
            assembly: Assembly::with_room(4096, 0)?,
            key: 0,
            leaf_share: 1.0,
        };
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
                    Stage::Index(core::mem::take(&mut self.grower.assembly).finish()?)
                }
            }
            Stage::Index(mut building) => {
                if building.step(BUILD_UNIT)? {
                    Stage::Done(building.finish()?)
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
    /// What it is assembled from.
    assembly: Assembly,
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
        if self.assembly.parts() >= MOST_PARTS {
            return Some(());
        }
        self.assembly.push(part)
    }

    /// Before its `twigs` are grown, share out what is left of the budget so
    /// every twig keeps the same share of its leaves.
    fn budget_leaves(&mut self, twigs: usize) {
        let segments = self
            .species
            .twigs()
            .map_or(1, |twigs| twigs.segments.max(1)) as usize;
        let tubes = twigs.saturating_mul(segments);
        let leaves =
            crate::vector::real(twigs.saturating_mul(self.species.leafing.per_twig as usize))
                * (1.0 - LEAFLESS_FOOT);
        let room = crate::vector::real(MOST_PARTS.saturating_sub(self.assembly.parts() + tubes));
        self.leaf_share = if leaves > room {
            room / leaves.max(1.0)
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

    /// The tree's trunks, `height` tall: one rising out of the ground, or a
    /// shrub's stems rising out of the stool they share.
    fn trunks(&mut self, height: f64) -> Option<Vec<Stem>> {
        let species = self.species;
        let trunks = species.trunks.max(1);
        let radius = height * species.girth / mathf::sqrt(f64::from(trunks));
        let mut ways = [(0.0, 0.0); MOST_LOBES];
        for (index, way) in (0..trunks).zip(ways.iter_mut()) {
            *way = if trunks == 1 {
                (self.about((0.0, 4.0)), self.range((0.0, TAU)))
            } else {
                (
                    self.range((10.0, 35.0)),
                    TAU * f64::from(index) / f64::from(trunks) + self.about((0.0, 0.4)),
                )
            };
        }
        let stems = ways.get(..usize::try_from(trunks).ok()?)?;
        let stool = if trunks > 1 {
            Some(self.stool(height * species.girth, radius)?)
        } else {
            None
        };
        let mut queue: Vec<Stem> = Vec::new();
        queue.try_reserve(usize::try_from(trunks).ok()?).ok()?;
        for &(lean, turn) in stems {
            let frame = Frame::turned(turn, lean.to_radians());
            let length = height
                * if trunks == 1 {
                    1.0
                } else {
                    self.range((0.7, 1.0))
                };
            let (base, emerges) = match stool {
                // Each from its own place over the stool, the way it leans,
                // its foot swelling into its neighbours' where it leaves the
                // ground.
                Some(stool) => {
                    let spread = stool.radius * self.range((0.1, 0.55));
                    let out = Vec3::new(mathf::sin(turn), 0.0, mathf::cos(turn));
                    let sunk = STOOL_SUNK * stool.radius;
                    (
                        out * spread - Vec3::UP * sunk,
                        sunk / mathf::cos(lean.to_radians()),
                    )
                }
                None => (Vec3::new(0.0, -0.2 * radius, 0.0), 0.2 * radius),
            };
            queue.push(Stem {
                level: 0,
                base,
                frame,
                length,
                radius,
                travelled: 0.0,
                forked: false,
                // A snag snapped off below its crown, thick where it broke.
                crown: if species.snapped { 1.0 } else { species.base },
                from: 0.0,
                emerges,
            });
        }
        Some(queue)
    }

    /// The stool a shrub's stems rise from, as broad as one trunk `girth`
    /// across would stand and its stems `stem` thick each: a woody crown
    /// sunk just beneath the ground, flaring into the roots it spreads, the
    /// stubs of its oldest stems standing dead and snapped about it.
    fn stool(&mut self, girth: f64, stem: f64) -> Option<Stool> {
        let radius = girth * self.range((1.5, 2.1));
        let dome = 0.72 * radius;
        let (foot, crown) = (-1.1 * radius, -dome - 0.03 * radius);
        let bark = self.stock.bark;
        let key = self.next_key();
        let tube = Tube::new(
            (Vec3::UP * foot, Vec3::UP * crown),
            ((radius, dome), (foot, crown)),
            (bark, key),
            Vec3::new(1.0, 0.0, 0.0),
        );
        let roots = ROOTS.0 + self.dice.next_u32() % (ROOTS.1 - ROOTS.0 + 1);
        let laid = Foot::new(
            &tube,
            (radius, -foot, crown - foot),
            (STOOL_FLARE, roots),
            &mut self.dice,
        )?;
        let index = self.assembly.flare(laid.flare())?;
        self.push(Part::Tube(tube.flared(index)))?;
        let key = self.next_key();
        laid.roots((bark, key), foot, &mut |part| self.push(part))?;
        // A big stool's oldest stems died back and snapped.
        let dead = if radius > 0.04 {
            self.dice.next_u32() % 3
        } else {
            0
        };
        for _ in 0..dead {
            let turn = self.range((0.0, TAU));
            let lean = self.range((15.0, 50.0)).to_radians();
            let way = Frame::turned(turn, lean).y;
            let out = Vec3::new(mathf::sin(turn), 0.0, mathf::cos(turn));
            let from = out * (radius * self.range((0.2, 0.65))) - Vec3::UP * (STOOL_SUNK * radius);
            let thick = stem * self.range((0.6, 1.0));
            let end =
                from + way * (STOOL_SUNK * radius / mathf::cos(lean) + self.range((0.04, 0.25)));
            let key = self.next_key();
            self.push(Part::Tube(
                Tube::new(
                    (from, end),
                    ((thick, 0.85 * thick), (0.0, (end - from).length())),
                    (bark, key),
                    out,
                )
                .opened([false, true]),
            ))?;
            let broken = snapped(
                (end, way, 0.85 * thick),
                (self.stock.grain, 0.7),
                &mut self.dice,
            )?;
            self.assembly.mesh(&broken.points, &broken.faces)?;
        }
        Some(Stool { radius })
    }

    /// A tree's trunk's first segment, from `base` up `frame`'s `y` and
    /// `travelled` metres along the tree's path at its foot: its foot swelling
    /// all round and out toward each of the roots it spreads, as a buttress,
    /// each lobe curving down into its root's back.
    fn foot(
        &mut self,
        stem: Stem,
        level: &Level,
        (base, frame): (Vec3, Frame),
        travelled: f64,
        key: u32,
    ) -> Option<()> {
        let stride = stem.length / f64::from(level.segments.max(1));
        let bark = self.stock.bark;
        let between = move |from: f64, to: f64| {
            Tube::new(
                (base + frame.y * from, base + frame.y * to),
                (
                    (
                        radius_at(stem, level, from / stem.length),
                        radius_at(stem, level, to / stem.length),
                    ),
                    (travelled + from, travelled + to),
                ),
                (bark, key),
                frame.x,
            )
        };
        let flare = self.species.flare;
        if flare <= 0.0 {
            return self.push(Part::Tube(between(0.0, stride)));
        }
        // The flare is a limb of its own, no taller than it, so the bole above
        // is met as the round limb it is.
        let top = (FLARE_TOP * stem.radius).min(stride);
        let tube = between(0.0, top);
        let roots = if self.species.trunks > 1 || stem.radius < 0.04 {
            0
        } else {
            ROOTS.0 + self.dice.next_u32() % (ROOTS.1 - ROOTS.0 + 1)
        };
        let foot = Foot::new(
            &tube,
            (stem.radius, stem.emerges.min(0.9 * top), top),
            (flare, roots),
            &mut self.dice,
        )?;
        let index = self.assembly.flare(foot.flare())?;
        self.push(Part::Tube(tube.flared(index)))?;
        if top < stride {
            self.push(Part::Tube(between(top, stride)))?;
        }
        let key = self.next_key();
        foot.roots((bark, key), travelled, &mut |part| self.push(part))
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
            let bole = radius_at(stem, level, rise / stem.length);
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
            let tip = thick * self.range((0.55, 0.85));
            let key = self.next_key();
            let end = start + dir * reach;
            self.push(Part::Tube(
                Tube::new(
                    (start, end),
                    (
                        (thick, tip),
                        (travelled + inside, travelled + inside + reach),
                    ),
                    (self.stock.bark, key),
                    frame.y,
                )
                .opened([false, true]),
            ))?;
            self.snap(end, dir, tip)?;
        }
        Some(())
    }

    /// The break where wood running along `way` snapped off at `end`,
    /// `radius` thick there.
    fn snap(&mut self, end: Vec3, way: Vec3, radius: f64) -> Option<()> {
        let torn = snapped(
            (end, way, radius),
            (self.stock.grain, SNAPPED_AGE),
            &mut self.dice,
        )?;
        if self.assembly.parts() + torn.faces.len() > MOST_PARTS {
            return Some(());
        }
        self.assembly.mesh(&torn.points, &torn.faces)
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
            let r0 = radius_at(stem, &level, s0);
            let r1 = radius_at(stem, &level, s1);
            frame = self.bend(frame, (stem.level, &level, s0), &mut sweep);
            // Then drawn toward the light, or weighed down.
            if attraction.abs() > 0.0 {
                let pull = Vec3::UP * (attraction / f64::from(segments));
                let dir = (frame.y + pull).normalized();
                frame = frame.aligning(frame.y, dir);
            }
            let end = at + frame.y * stride;
            let key = self.next_key();
            // A snag's trunk and limbs end where they snapped.
            let snapped = species.snapped && stem.level <= 1 && segment + 1 == segments;
            if rooted && segment == 0 {
                self.foot(stem, &level, (at, frame), travelled, key)?;
            } else {
                self.push(Part::Tube(
                    Tube::new(
                        (at, end),
                        ((r0, r1), (travelled, travelled + stride)),
                        (self.stock.bark, key),
                        frame.x,
                    )
                    .opened([false, snapped]),
                ))?;
            }
            if snapped {
                self.snap(end, frame.y, r1)?;
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
                    emerges: 0.0,
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
        let parent_radius = radius_at(parent, species.levels.get(parent.level)?, offset);
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
            emerges: 0.0,
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
            if s0 + along / segments < LEAFLESS_FOOT {
                continue;
            }
            let base = from + (to - from) * along;
            self.leaf(base, frame, twig_length)?;
            if let Some(fruit) = self.stock.fruit {
                if self.unit() < fruit.share {
                    self.fruit(base, fruit)?;
                }
            }
        }
        Some(())
    }

    /// A fruit hanging from its stalk below `base` on a twig: no two the same
    /// size, nor quite round, a smaller lobe beneath a larger.
    fn fruit(&mut self, base: Vec3, fruit: Fruit) -> Option<()> {
        let radius = fruit.radius * (0.8 + 0.35 * self.unit());
        let stalk = base - Vec3::UP * (0.02 + 0.01 * self.unit());
        let centre = stalk - Vec3::UP * radius;
        let side = Vec3::new(1.0, 0.0, 0.0);
        let key = self.key;
        self.push(Part::Tube(Tube::new(
            (base, stalk),
            ((0.0015, 0.0012), (0.0, 0.02)),
            (self.stock.bark, key),
            side,
        )))?;
        let lobe = centre
            - Vec3::new(
                0.15 * radius * (self.unit() - 0.5),
                0.35 * radius,
                0.15 * radius * (self.unit() - 0.5),
            );
        for (at, size) in [(centre, radius), (lobe, 0.8 * radius)] {
            self.push(Part::Tube(Tube::new(
                (at, at),
                ((size, size), (0.0, 0.0)),
                (fruit.material, key),
                side,
            )))?;
        }
        Some(())
    }

    /// One leaf borne at `base` on a twig whose frame is `frame`.
    fn leaf(&mut self, base: Vec3, frame: Frame, twig: f64) -> Option<()> {
        let leaves = (self.stock.leaves, &mut self.key);
        match leaf(
            &self.species.leafing,
            (base, frame, twig),
            leaves,
            &mut self.dice,
        ) {
            Some(blade) => self.push(Part::Leaf(blade)),
            None => Some(()),
        }
    }
}

/// One leaf of `leafing`, in `material`, borne at `base` on a twig `twig`
/// long whose frame is `frame`, keyed from `count` and drawn from `dice`:
/// turned out from the twig at its stalk's angle and its face up toward the
/// sky as far as its stalk lets it; `None` where it could face nowhere.
pub(crate) fn leaf(
    leafing: &Leafing,
    (base, frame, twig): (Vec3, Frame, f64),
    (material, count): (u16, &mut u32),
    dice: &mut NonCryptoRng,
) -> Option<Blade> {
    let mut unit = || dice.next_f64();
    let around = TAU * unit();
    let angle = (leafing.angle + 15.0 * (2.0 * unit() - 1.0)).to_radians();
    let out = frame.x * mathf::cos(around) + frame.z * mathf::sin(around);
    let axis = (frame.y * mathf::cos(angle) + out * mathf::sin(angle)).normalized();
    let jitter = (0.3 * (2.0 * unit() - 1.0), 0.3 * (2.0 * unit() - 1.0));
    let face = (Vec3::UP * leafing.toward_light
        + out * (1.0 - leafing.toward_light)
        + Vec3::new(jitter.0, 0.0, jitter.1))
    .normalized();
    let normal = (face - axis * face.dot(axis)).normalized();
    if normal.length() < 0.5 {
        return None;
    }
    let size = (0.8 + 0.4 * unit()) * leafing.length.min(twig * 1.5);
    let fold = leafing.fold * (0.6 + 0.6 * unit());
    *count = count.wrapping_add(1);
    let key = crate::sample::mix32(*count ^ dice.next_u32());
    Some(Blade {
        base: singles(base),
        normal: singles(normal),
        axis: singles(axis),
        length: single(size),
        width: single(size * leafing.breadth),
        outline: leafing.outline,
        fold: single(fold),
        material,
        key,
    })
}

/// The radius `s` of the way along `stem`, grown at `level`: narrowing up
/// its bole by the level's taper, along its form, then within its crown
/// straight to its tip, a fork's arm carrying on from where its stem forked.
/// A trunk's flare is its foot's own.
fn radius_at(stem: Stem, level: &Level, s: f64) -> f64 {
    let bole = |s: f64| mathf::exp(level.form * mathf::ln((1.0 - level.taper * s).max(0.05)));
    let narrowed = |s: f64| {
        if s <= stem.crown {
            bole(s)
        } else {
            bole(stem.crown) * ((1.0 - s) / (1.0 - stem.crown).max(1e-6)).max(0.04)
        }
    };
    let whole = stem.from + s * (1.0 - stem.from);
    stem.radius * narrowed(whole) / narrowed(stem.from)
}

/// `frame` turned `angle` radians away from its own axis `y`, toward the
/// side `around` names.
fn bent(frame: Frame, angle: f64, around: f64) -> Frame {
    let side = frame.x * mathf::cos(around) + frame.z * mathf::sin(around);
    let dir = (frame.y * mathf::cos(angle) + side * mathf::sin(angle)).normalized();
    frame.aligning(frame.y, dir)
}

/// Leaflets either side of each tenth of a palm frond's rachis.
const LEAFLETS: u32 = 5;

/// How far below the ground a palm's trunk runs, and how far up it its foot
/// swells, in its radii there.
const PALM_BURIED: f64 = 0.3;
const PALM_FOOT: f64 = 2.2;

/// A palm, `height` tall, its trunk swelling at its foot into the mat of
/// roots it stands on in `roots`, curving away from the vertical, and a
/// crown of `fronds` fronds, its hierarchy still to build; `None` when the
/// heap will not hold it.
pub(crate) fn palm(
    height: f64,
    (stock, roots): (Stock, u16),
    fronds: u16,
    seed: u64,
) -> Option<Building> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let mut assembly = Assembly::with_room(4096, 0)?;
    let lean = (8.0 + 22.0 * dice.next_f64()).to_radians();
    let turn = TAU * dice.next_f64();
    let side = Vec3::new(mathf::cos(turn), 0.0, mathf::sin(turn));
    let segments = 24u32;
    let radius = 0.018 * height;
    let mut key = crate::sample::mix32(u32::try_from(seed & 0xffff_ffff).unwrap_or(0));
    let foot = 1.35 * radius;
    let mut at = palm_foot(&mut assembly, (foot, side), (stock, roots, key), &mut dice)?;
    let mut dir = Vec3::UP;
    let rising = height - at.y;
    for segment in 0..segments {
        let s = f64::from(segment) / f64::from(segments);
        // Curving away from the vertical, most at its foot.
        let bend = lean * (1.0 - s) * 2.0 / f64::from(segments);
        dir = (dir * mathf::cos(bend) + side * mathf::sin(bend)).normalized();
        let end = at + dir * (rising / f64::from(segments));
        key = crate::sample::mix32(key ^ segment);
        // The trunk bends in the upright plane through `side`, square to which
        // its bark is begun.
        assembly.push(Part::Tube(Tube::new(
            (at, end),
            (
                (
                    radius * (1.35 - 0.4 * s),
                    radius * (1.35 - 0.4 * s - 0.4 / f64::from(segments)),
                ),
                (
                    at.y + s * rising,
                    at.y + (s + 1.0 / f64::from(segments)) * rising,
                ),
            ),
            (stock.bark, key),
            Vec3::UP.cross(side),
        )))?;
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
        grow_frond(
            &mut |part| assembly.push(part),
            (crown, around),
            &shape,
            (stock, key),
            &mut dice,
        )?;
    }
    assembly.finish()
}

/// A palm trunk's foot, `radius` thick where it leaves the ground and bent
/// in the upright plane square to `side`, laid into `assembly` in `stock`'s
/// bark and its roots in `roots`, keyed from `key`: buried a little and
/// swelling all round toward the ground, and from the swelling the mat of
/// roots a palm stands on, thick as a finger, running out and down into the
/// soil, a few long dead and snapped short. Where the trunk rises from it.
fn palm_foot(
    assembly: &mut Assembly,
    (radius, side): (f64, Vec3),
    (stock, roots, key): (Stock, u16, u32),
    dice: &mut NonCryptoRng,
) -> Option<Vec3> {
    let buried = PALM_BURIED * radius;
    let top = PALM_FOOT * radius;
    let tube = Tube::new(
        (Vec3::UP * -buried, Vec3::UP * top),
        ((radius, radius), (-buried, top)),
        (stock.bark, key),
        Vec3::UP.cross(side),
    );
    let swell = 0.3 + 0.3 * dice.next_f64();
    let flare = Flare::new(buried, buried + top, (swell, 0.9 * radius), &[])?;
    let index = assembly.flare(flare)?;
    assembly.push(Part::Tube(tube.flared(index)))?;
    // Roots spring from the swelling just above the ground, the most nearest
    // it, as thickly as they can stand side by side.
    let thick = (0.025 * radius).clamp(0.004, 0.009);
    let band = (0.35 * radius).clamp(0.05, 0.14);
    let around = TAU * radius * (1.0 + swell);
    let count = mathf::round_i32(around * band / (2.0 * thick * 2.0 * thick)).clamp(40, 1200);
    for root in 0..count {
        let rise = band * dice.next_f64() * (0.4 + 0.6 * dice.next_f64());
        let angle = TAU * dice.next_f64();
        let girth = radius * flare.factor(buried + rise, angle);
        let out = tube.way(angle);
        let thick = thick * (0.65 + 0.6 * dice.next_f64());
        let from = Vec3::UP * rise + out * (girth - 1.2 * thick);
        let root_key = crate::sample::mix32(key ^ u32::try_from(root).unwrap_or(0) ^ 0x7007);
        let reach = 0.015 + 0.07 * dice.next_f64() * dice.next_f64() + 0.3 * rise;
        // Out from the swelling, bowing down under its own weight into the
        // soil, and on beneath it out of sight.
        let path = |t: f64| {
            from + out * (1.2 * thick + reach * mathf::exp(0.8 * mathf::ln(t.max(1e-9))))
                - Vec3::UP * ((rise + 2.0 * thick) * t * t)
        };
        if dice.next_f64() < 0.12 {
            // Long dead, dried and snapped off short of the soil.
            let snap = 0.25 + 0.4 * dice.next_f64();
            let (near, end) = (path(0.5 * snap), path(snap));
            let way = (end - near).normalized();
            for (a, b, radii) in [
                (from, near, (thick, 0.95 * thick)),
                (near, end, (0.95 * thick, 0.9 * thick)),
            ] {
                assembly.push(Part::Tube(
                    Tube::new((a, b), (radii, (0.0, 0.0)), (roots, root_key), side)
                        .opened([false, b == end]),
                ))?;
            }
            let torn = snapped((end, way, 0.9 * thick), (stock.grain, 0.8), dice)?;
            assembly.mesh(&torn.points, &torn.faces)?;
            continue;
        }
        let mut last = from;
        for step in 1..=PALM_ROOT_STEPS {
            let next = path(f64::from(step) / f64::from(PALM_ROOT_STEPS));
            let s = f64::from(step - 1) / f64::from(PALM_ROOT_STEPS);
            assembly.push(Part::Tube(Tube::new(
                (last, next),
                (
                    (thick * (1.0 - 0.08 * s), thick * (0.92 - 0.08 * s)),
                    (0.0, 0.0),
                ),
                (roots, root_key),
                side,
            )))?;
            last = next;
        }
        let beneath = last + (out * 0.4 - Vec3::UP).normalized() * (0.05 + 0.05 * dice.next_f64());
        assembly.push(Part::Tube(Tube::new(
            (last, beneath),
            ((0.84 * thick, 0.6 * thick), (0.0, 0.0)),
            (roots, root_key),
            side,
        )))?;
    }
    Some(Vec3::UP * top)
}

/// The segments a palm's root bows down into the soil in.
const PALM_ROOT_STEPS: u32 = 5;

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
/// vertical, through `push`, in `stock` and keyed from `key`.
fn grow_frond(
    push: &mut dyn FnMut(Part) -> Option<()>,
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
        push(Part::Tube(Tube::new(
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
        )))?;
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
                    push(Part::Leaf(Blade {
                        base: singles(base),
                        normal: singles(normal),
                        axis: singles(axis),
                        length: single(leaflet),
                        width: single(leaflet * shape.leaflet.1),
                        outline: shape.outline,
                        fold: 0.35,
                        material: stock.leaves,
                        key,
                    }))?;
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
    let mut assembly =
        Assembly::with_room(usize::from(fronds) * (1 + 2 * LEAFLETS as usize) * 10, 0)?;
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
            &mut |part| assembly.push(part),
            (Vec3::ZERO, around),
            &shape,
            (stock, key),
            &mut dice,
        )?;
    }
    assembly.finish()
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
