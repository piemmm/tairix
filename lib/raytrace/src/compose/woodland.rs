//! Woodland: trees stood over a land as a wood grows them.
//!
//! A wood covers the ground in patches, and within them its trees stand as
//! far apart as their crowns spread — closer where it grows thick, further
//! where it is open — the tallest taking the room first and the rest filling
//! the gaps they leave, the shortest under the crowns of the tallest. Each
//! kind keeps to the ground it takes to and grows in stands of its own, each
//! stand as tall as it is old. Shrubs, ferns and young trees take the light
//! that reaches the floor beneath, and the wood's dead lie among the living.
//!
//! Every tree is stood about the eye near it, where its shadow and the sky it
//! hides fall on what is seen; further off only across the view, out to the
//! land's edge or as far as a tree still spans a pixel or two. The places a
//! tree might stand are read in bands across the runner, since a forest's
//! worth of them is tens of thousands.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use super::chains::Chains;
use super::fields::Fielding;
use super::landscape::Bridging;
use super::landscape::{self, Lawning, Vantage};
use super::plants::{self, Dead, Grove, Laying, DEAD_VARIANTS};
use super::stones::{self, Bed};
use super::waterside::Margins;
use super::{Dice, Stage};
use crate::detail;
use crate::far_wood::{FarWood, Stretch};
use crate::ground::Floor;
use crate::heightfield::{Heightfield, Sealing};
use crate::land::{Land, Lie};
use crate::noise::{hash3, smoothstep};
use crate::sample::{mix32, unit};
use crate::shade::{Casting, Shade, Shades, Shading, NEAR_CELL, ROOFED};
use crate::stream::{Flow, Solving};
use crate::vector::{real, share, single, Frame, Pose, Vec3};
use crate::wood::{
    gap, rooted, wooded, Habit, Reader, Rooting, Tree, Woodland, EDGE_OPENING, PACKED, VARIANTS,
};

/// A wood a scene sets out, grown once the land stands: its trees, how they
/// grow and where, where it is seen from, and what grows beneath it.
#[derive(Clone, Debug)]
pub(super) struct Wood {
    pub(super) grove: Grove,
    pub(super) woodland: Woodland,
    pub(super) rooting: Rooting,
    pub(super) vantage: Vantage,
    pub(super) beneath: Option<Beneath>,
    pub(super) deadfall: Option<Deadfall>,
}

/// What lies and stands dead in a wood: the dead of its kinds, and how many
/// fallen trunks, stumps and standing dead to a hectare where its canopy has
/// opened, which is where they fell from; a third of that where it is whole.
#[derive(Clone, Debug)]
pub(super) struct Deadfall {
    pub(super) dead: [Option<Dead>; 2],
    pub(super) logs: f64,
    pub(super) stumps: f64,
    pub(super) snags: f64,
}

/// How far about the eye a wood's dead are laid: further off, a fallen trunk
/// is a pixel of the floor.
const DEADFALL_REACH: f64 = 160.0;

/// How many places are tried for each of a wood's dead.
const DEADFALL_TRIES: u32 = 6;

/// How near the eye nothing dead lies, so none fills the picture's foot.
const DEADFALL_CLEAR: f64 = 3.5;

/// What grows beneath a wood's canopy: shrubs and young trees, as many as
/// `plants` to a hectare of its floor where the light suits them best.
#[derive(Clone, Debug)]
pub(super) struct Beneath {
    pub(super) grove: Grove,
    pub(super) plants: f64,
}

/// How far about the eye trees stand whichever way it looks: as far as a
/// tree's shadow and the sky it hides still fall on what is seen.
const ABOUT: f64 = 160.0;

/// How far either side of the view trees stand beyond that, in radians: the
/// widest picture's half-width and a margin for the shadows cast into it.
pub(super) const ACROSS: f64 = 1.15;

/// How many pixels tall a tree far off still spans, at least.
const SPANNED: f64 = 2.0;

/// How far about the eye, in heights of its tallest trees, a wood stands all
/// the way round: as far as their shadows reach.
const SHADOWED: f64 = 8.0;

/// How closely the places a tree might stand lie, as a share of how far
/// apart the wood's trees of middling height stand where it is thickest:
/// close enough that thinning, not the lattice, sets where they stand.
const SOWN: f64 = 0.45;
const LEAST_SOWN: f64 = 0.35;

/// How much of a crown a much shorter tree may stand under: at a trunk's
/// share `UNDER` of its neighbour's height and below, its crown needs only
/// `LAYERED` of the room a peer's would.
const UNDER: (f64, f64) = (0.4, 0.75);
const LAYERED: f64 = 0.35;

/// How far about the eye what grows beneath a wood is set out: nearer than
/// that, a shrub is more than a few pixels; and how far all the way round.
const BENEATH: f64 = 140.0;
const ABOUT_BENEATH: f64 = 40.0;

/// The most plants what grows beneath a wood holds.
const MOST_BENEATH: u32 = 30_000;

/// The places of the ground read at once in one band across the runner.
const BAND: usize = 1024;

/// Room kept among the stage's objects for what a scene sets out after its
/// woods.
pub(super) const KEPT: usize = 512;

/// A place a tree might stand, and, once the ground there is read, the tree
/// that would.
#[derive(Copy, Clone, Debug)]
struct Seedling {
    at: (f64, f64),
    /// Its draws, hashed from its place in the lattice.
    draw: u32,
    tree: Option<Tree>,
}

/// What reading the ground for a wood's trees needs, shared by every band of
/// places read at once.
struct Reading<'a> {
    reader: &'a Reader,
    land: &'a Land,
    fields: &'a [Heightfield],
    /// The shade the canopy above casts, where what grows beneath it is read.
    beneath: Option<&'a Shade>,
}

impl Reading<'_> {
    /// Read the ground at `seedling`'s place for the tree it would grow.
    fn read(&self, seedling: &mut Seedling) {
        seedling.tree = self.reader.tree(
            (&self.land.grids, self.fields),
            (seedling.at, seedling.draw),
            self.beneath,
        );
    }
}

/// The room about a trunk `height` tall that other pieces keep clear of.
fn trunk(height: f64) -> f64 {
    0.25 + 0.025 * height
}

/// Sow ring `ring` of the places a plant might stand seen from `vantage`,
/// from `inner` to `outer` from the eye, the wood's places sown `about` it
/// all the way round and across the view beyond, within the square
/// `(centre, reach)`, into `into`; how many places the ring was drawn over,
/// those beyond the square included, or `None` when the heap will not hold
/// them.
fn sow_ring(
    (ring, (inner, outer)): (u32, (f64, f64)),
    (vantage, about): (&Vantage, f64),
    (centre, reach): ((f64, f64), f64),
    seed: u32,
    into: &mut Vec<Seedling>,
) -> Option<usize> {
    let Vantage { eye, heading } = *vantage;
    let (from, span) = if inner < about {
        (0.0, TAU)
    } else {
        (heading - ACROSS, 2.0 * ACROSS)
    };
    let breadth = (outer - inner).max(1e-9);
    let count =
        u32::try_from(mathf::round_i32(span * 0.5 * (inner + outer) / breadth).max(1)).ok()?;
    if !fallible::reserve(into, count as usize) {
        return None;
    }
    for index in 0..count {
        let draw = hash3(ring, index, 0x5eed, seed);
        let angle = from + (f64::from(index) + unit(draw)) / f64::from(count) * span;
        let radius = mathf::sqrt(
            inner * inner + unit(mix32(draw ^ 0x3c6e_f372)) * (outer * outer - inner * inner),
        );
        let at = (
            eye.x + mathf::sin(angle) * radius,
            eye.z + mathf::cos(angle) * radius,
        );
        if (at.0 - centre.0).abs() < reach && (at.1 - centre.1).abs() < reach {
            into.push(Seedling {
                at,
                draw: mix32(draw ^ 0xa54f_f53a),
                tree: None,
            });
        }
    }
    usize::try_from(count).ok()
}

/// Read the ground at every one of `seedlings` across `runner`.
fn read_all(runner: &dyn JobRunner, seedlings: &mut [Seedling], reading: &Reading<'_>) {
    tairix_parallel::for_each_drawn(runner, seedlings.chunks_mut(BAND), &|band| {
        for seedling in band.iter_mut() {
            reading.read(seedling);
        }
    });
}

/// A tree stood, as its neighbours see it.
#[derive(Copy, Clone, Debug)]
struct Stood {
    at: (f64, f64),
    height: f64,
    reach: f64,
    apart: f64,
}

/// The trees stood so far, each chained into the cell of a grid over the
/// wood its trunk stands in: a cell as broad as the most room two trees
/// keep, so that any tree too near another stands in a cell beside the
/// other's.
#[derive(Debug)]
struct Crowns {
    chains: Chains,
    trees: Vec<Stood>,
}

/// The most cells a side of the crowns' grid holds: past it, the cells grow
/// broader instead, holding more trees each.
const MOST_SIDE: usize = 1024;

impl Crowns {
    fn new((centre, reach): ((f64, f64), f64), cell: f64) -> Option<Self> {
        Some(Self {
            chains: Chains::new((centre, reach), cell.max(1.0), MOST_SIDE)?,
            trees: Vec::new(),
        })
    }

    /// Whether `tree` would crowd any tree stood: its trunk nearer one's
    /// than their crowns allow, a much shorter tree standing under a taller
    /// one's crown more readily than beside a peer's.
    fn crowds(&self, tree: &Stood) -> bool {
        let (beside, _) = self.chains.span(tree.at, self.chains.cell());
        self.chains.within(beside).any(|id| {
            let Some(other) = self.trees.get(id as usize) else {
                return false;
            };
            let (short, tall) = if tree.height < other.height {
                (tree, other)
            } else {
                (other, tree)
            };
            let layered = LAYERED
                + (1.0 - LAYERED)
                    * smoothstep(UNDER.0, UNDER.1, short.height / tall.height.max(1e-3));
            let room = (tree.apart.max(other.apart) * (tree.reach + other.reach) * layered)
                .max(trunk(tree.height) + trunk(other.height) + 0.5);
            mathf::hypot(tree.at.0 - other.at.0, tree.at.1 - other.at.1) < room
        })
    }

    fn add(&mut self, tree: Stood) -> Option<()> {
        let id = u32::try_from(self.trees.len()).ok()?;
        let (own, _) = self.chains.span(tree.at, 0.0);
        self.trees.try_reserve(1).ok()?;
        self.trees.push(tree);
        self.chains.link(id, own)
    }
}

/// How the places a wood's plants might stand are sown: how far apart, how
/// far all the way about the eye, and how far across the view.
#[derive(Copy, Clone, Debug)]
struct Sowing {
    sown: f64,
    about: f64,
    far: f64,
}

impl Sowing {
    /// How many rings its places are sown in, each as broad as they lie
    /// apart, out to the first edge at or past its far one.
    fn rings(&self) -> u32 {
        whole(mathf::ceil(self.far / self.sown.max(1e-9)))
    }

    /// Where ring `ring` begins and ends.
    fn ring(&self, ring: u32) -> (f64, f64) {
        (f64::from(ring) * self.sown, f64::from(ring + 1) * self.sown)
    }

    /// How many of its rings begin within the eye's round, sown all the way
    /// about it.
    fn round(&self) -> u32 {
        whole(mathf::ceil(self.about / self.sown.max(1e-9)))
    }

    /// About how many places its first `rings` rings sow: each as many as its
    /// span about the eye and its breadth hold, the `k`th `k + ½` times its
    /// span.
    fn places_in(&self, rings: u32) -> f64 {
        let (all, round) = (f64::from(rings), f64::from(rings.min(self.round())));
        0.5 * TAU * round * round + ACROSS * (all * all - round * round)
    }

    /// The sowing kept to `most` places, reaching only as far as its rings
    /// fit.
    fn fitted(self, most: f64) -> Self {
        if self.places_in(self.rings()) <= most {
            return self;
        }
        let round = f64::from(self.round());
        let within = 0.5 * TAU * round * round;
        let rings = if within >= most {
            mathf::sqrt(2.0 * most / TAU)
        } else {
            mathf::sqrt((most - within) / ACROSS + round * round)
        };
        Self {
            far: mathf::floor(rings) * self.sown,
            ..self
        }
    }
}

/// `value`, already whole, as a count; nought below it and the most a count
/// holds above.
fn whole(value: f64) -> u32 {
    u32::try_from(mathf::round_i32(value).max(0)).unwrap_or(u32::MAX)
}

/// A wood's plants being stood a step at a time: the places they might stand
/// sown and the ground there read a band at a time, then those that would
/// grow thinned tallest first, a batch at a time.
#[derive(Debug)]
struct Standing {
    reader: Reader,
    sowing: Sowing,
    /// The next ring to sow.
    ring: u32,
    /// The places sown in this band, read, their room kept for the next.
    band: Vec<Seedling>,
    /// The places read that would grow, in the order they were sown.
    grown: Runs<Seedling>,
    /// Those places, tallest first.
    ranking: Ranking,
    crowns: Crowns,
    stood: u32,
    most: u32,
    /// The wood carried on past them, to be matched to them once they stand.
    beyond: Option<FarWood>,
}

/// How many places a step reads at least, across the runner, and how many it
/// thins at most, one after another since each tree stood shapes where the
/// next may.
const READ_UNIT: usize = 1 << 14;
const THIN_UNIT: usize = 1 << 9;

impl Standing {
    /// The plants `reader` reads to be stood, seen from `vantage` and sown as
    /// `sowing` has it; `None` when the heap will not hold the grid their
    /// crowns are kept apart by.
    fn new(stage: &Stage, reader: &Reader, (vantage, sowing): (&Vantage, Sowing)) -> Option<Self> {
        let tallest = reader.kinds().map(Habit::tallest).fold(0.0, f64::max);
        let widest = reader.kinds().map(|habit| habit.crown).fold(0.0, f64::max) * tallest;
        let closure = reader.woodland.closure;
        let crowned = closure.1.max(closure.0) * (1.0 + EDGE_OPENING) * 2.0 * widest;
        let most_room = crowned.max(2.0 * trunk(tallest) + 0.5);
        Some(Self {
            reader: *reader,
            sowing,
            ring: 0,
            band: Vec::new(),
            grown: Runs::default(),
            ranking: Ranking::default(),
            crowns: Crowns::new(((vantage.eye.x, vantage.eye.z), sowing.far), most_room)?,
            stood: 0,
            most: reader
                .woodland
                .most
                .min(u32::try_from(stage.room().saturating_sub(KEPT)).unwrap_or(u32::MAX)),
            beyond: None,
        })
    }

    /// How far from the eye its places reach: where its last ring ends.
    fn reach(&self) -> f64 {
        self.sowing.ring(self.rings()).0
    }

    /// How many rings its places are sown in.
    fn rings(&self) -> u32 {
        self.sowing.rings()
    }

    /// How far the standing has come: its rings read, its places ranked,
    /// then thinned.
    fn done(&self) -> f64 {
        let read = share(self.ring as usize, self.rings() as usize);
        // Until every ring is read the ranking holds only what has been, and
        // an empty one has done nothing.
        if self.ring < self.rings() {
            return 0.6 * read;
        }
        let thinned = if self.stood >= self.most {
            1.0
        } else {
            self.ranking.taken()
        };
        0.6 + 0.05 * self.ranking.sorted() + 0.35 * thinned
    }

    /// The next step of standing its plants — a wood's trees, or what grows
    /// beneath them in `shade` — seen from `vantage`: whether all are stood,
    /// or `None` when the heap will not hold them.
    fn step(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
        (vantage, shade): (&Vantage, Option<&Shade>),
    ) -> Option<bool> {
        if self.ring < self.rings() {
            self.band.clear();
            // Bounded by the places drawn, not those kept: across the view
            // from near the land's edge most of a far ring lies off it.
            let mut drawn = 0;
            while self.ring < self.rings() && drawn < READ_UNIT {
                drawn += sow_ring(
                    (self.ring, self.sowing.ring(self.ring)),
                    (vantage, self.sowing.about),
                    land.grids.traced(&stage.fields),
                    self.reader.seeds.1,
                    &mut self.band,
                )?;
                self.ring += 1;
            }
            let reading = Reading {
                reader: &self.reader,
                land,
                fields: &stage.fields,
                beneath: shade,
            };
            read_all(runner, &mut self.band, &reading);
            let growing = self.band.iter().filter(|seedling| seedling.tree.is_some());
            let count = growing.clone().count();
            if !self.grown.reserve(count) || !self.ranking.reserve(count) {
                return None;
            }
            for seedling in growing {
                let index = u32::try_from(self.grown.len()).ok()?;
                self.ranking
                    .add(seedling.tree.map_or(0.0, |tree| tree.height), index);
                self.grown.push(*seedling);
            }
            return Some(false);
        }
        if !self.ranking.ranked() {
            self.band = Vec::new();
            self.ranking.rank(runner)?;
            return Some(false);
        }
        for _ in 0..THIN_UNIT {
            if self.stood >= self.most {
                break;
            }
            let Some(index) = self.ranking.next() else {
                break;
            };
            let Some(&Seedling {
                at,
                draw,
                tree: Some(tree),
            }) = self.grown.get(index as usize)
            else {
                continue;
            };
            let standing = Stood {
                at,
                height: tree.height,
                reach: tree.reach,
                apart: tree.apart,
            };
            if walls_off(self.reader.woodland.open, vantage, at, tree.height)
                || !stage.clear(at, trunk(tree.height))
                || self.crowns.crowds(&standing)
            {
                continue;
            }
            self.crowns.add(standing)?;
            let habit = self.reader.kinds().nth(usize::from(tree.kind))?;
            plants::place(
                stage,
                habit,
                (usize::from(tree.variant), tree.scale),
                tree.placing(at, draw),
            )?;
            stage.claim(at, trunk(tree.height))?;
            self.stood += 1;
        }
        Some(self.ranking.exhausted() || self.stood >= self.most)
    }
}

/// Places in a run of a ranking one core sorts: a unit of the ranking is a
/// run a core.
const RUN: usize = 1 << 14;

/// Items kept in runs of `RUN` that never move as more are added, so a band's
/// worth more costs only its own room: no copy of what came before, and no
/// room held past the last run's.
#[derive(Debug)]
pub(super) struct Runs<T> {
    runs: Vec<Vec<T>>,
    len: usize,
}

impl<T> Default for Runs<T> {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Runs<T> {
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Room for `count` more; `false` when the heap will not hold it.
    pub(super) fn reserve(&mut self, count: usize) -> bool {
        let room = self.runs.len() * RUN;
        let wanted = (self.len + count).saturating_sub(room).div_ceil(RUN);
        if self.runs.try_reserve(wanted).is_err() {
            return false;
        }
        for _ in 0..wanted {
            let mut run = Vec::new();
            if run.try_reserve_exact(RUN).is_err() {
                return false;
            }
            self.runs.push(run);
        }
        true
    }

    /// Add `item`, within the room reserved.
    pub(super) fn push(&mut self, item: T) {
        if let Some(run) = self.runs.get_mut(self.len / RUN) {
            run.push(item);
            self.len += 1;
        }
    }

    pub(super) fn get(&self, index: usize) -> Option<&T> {
        self.runs.get(index / RUN)?.get(index % RUN)
    }
}

/// Places ranked tallest first, ties in the order they were added: each run
/// sorted by a core once all are added, then merged a place at a time as they
/// are taken, so neither the sort nor the merge is ever one long unit.
#[derive(Debug, Default)]
pub(super) struct Ranking {
    /// Each place's height, as the grids hold one, and its index.
    order: Runs<(f32, u32)>,
    /// How many runs are sorted, and how many places have been taken.
    sorted: usize,
    taken: usize,
    /// The next place of each run not yet taken.
    next: Vec<usize>,
    /// The tallest place each run has left, tallest of all on top.
    heads: BinaryHeap<Head>,
}

/// The tallest place a run of a ranking has left.
#[derive(Copy, Clone, Debug)]
struct Head {
    height: f32,
    index: u32,
    run: u32,
}

impl Ord for Head {
    /// Taller first, and of two as tall, the one added first.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.height
            .total_cmp(&other.height)
            .then(other.index.cmp(&self.index))
    }
}

impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Head {}

impl Ranking {
    /// Room for `count` more places; `false` when the heap will not hold it.
    pub(super) fn reserve(&mut self, count: usize) -> bool {
        self.order.reserve(count)
    }

    /// Add place `index` of height `height`, within the room reserved.
    pub(super) fn add(&mut self, height: f64, index: u32) {
        self.order.push((single(height), index));
    }

    /// The share of its places sorted into their runs.
    fn sorted(&self) -> f64 {
        share((self.sorted * RUN).min(self.order.len()), self.order.len())
    }

    /// The share of its places taken.
    fn taken(&self) -> f64 {
        share(self.taken, self.order.len())
    }

    /// Whether every run is sorted and laid out to be merged.
    pub(super) fn ranked(&self) -> bool {
        let runs = self.order.runs.len();
        self.sorted >= runs && self.next.len() == runs
    }

    /// Sort the next runs, one a core across `runner`, and ready them to be
    /// merged once all are; `None` when the heap will not hold the merge.
    pub(super) fn rank(&mut self, runner: &dyn JobRunner) -> Option<()> {
        let count = self.order.runs.len();
        let end = (self.sorted + runner.width().max(1)).min(count);
        if let Some(runs) = self.order.runs.get_mut(self.sorted..end) {
            tairix_parallel::for_each(runner, runs, &|run| {
                run.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            });
        }
        self.sorted = end;
        if self.sorted < count {
            return Some(());
        }
        if !fallible::reserve(&mut self.next, count) || self.heads.try_reserve_exact(count).is_err()
        {
            return None;
        }
        for run in 0..count {
            self.next.push(0);
            self.push_head(run)?;
        }
        Some(())
    }

    /// The tallest place not yet taken; `None` once all are.
    pub(super) fn next(&mut self) -> Option<u32> {
        let head = self.heads.pop()?;
        self.taken += 1;
        self.push_head(head.run as usize)?;
        Some(head.index)
    }

    /// Whether every place has been taken.
    pub(super) fn exhausted(&self) -> bool {
        self.heads.is_empty()
    }

    /// Move run `run` on to its next place, if it has one left, and offer
    /// that place to the merge; `None` only for a run that is not there.
    fn push_head(&mut self, run: usize) -> Option<()> {
        let next = self.next.get_mut(run)?;
        if let Some(&(height, index)) = self.order.runs.get(run)?.get(*next) {
            *next += 1;
            self.heads.push(Head {
                height,
                index,
                run: u32::try_from(run).ok()?,
            });
        }
        Some(())
    }
}

/// The next step of a stream's flow over its bed, out of `phase`: solved a
/// unit at a time, then shaping the water's finer grid on `land` and sealing
/// it again, then back to setting out the water's edge.
fn surfacing(
    stage: &mut Stage,
    (land, runner): (&Land, &dyn JobRunner),
    phase: Phase,
) -> Option<Phase> {
    Some(match phase {
        Phase::Flowing {
            shades,
            mut solving,
            course,
        } => {
            if solving.step(runner)? {
                Phase::Surfacing {
                    shades,
                    flow: solving.finish()?,
                    course,
                    row: 0,
                }
            } else {
                Phase::Flowing {
                    shades,
                    solving,
                    course,
                }
            }
        }
        Phase::Surfacing {
            shades,
            flow,
            course,
            row,
        } => match stones::surface(stage, land, (&flow, course), row, runner)? {
            (_, true) => Phase::Resealing {
                shades,
                sealing: Sealing::BEGUN,
            },
            (row, false) => Phase::Surfacing {
                shades,
                flow,
                course,
                row,
            },
        },
        Phase::Resealing {
            shades,
            mut sealing,
        } => {
            let field = stage
                .fields
                .get_mut(land.grids.near_water?.field as usize)?;
            if sealing.step(field, runner) {
                Phase::Edging { shades }
            } else {
                Phase::Resealing { shades, sealing }
            }
        }
        other => other,
    })
}

/// The flow over the stream bed `bed` has laid, to be solved over the
/// stretch the water's finer grid on `land` spans along it, in the grid
/// `densities` sets; or, with no such grid or no water to run, straight on
/// to what follows, the water left as its own grid has it.
fn flowing(
    bed: &mut Bed,
    land: &Land,
    shades: Option<Shades>,
    densities: &detail::Bed,
) -> Option<Phase> {
    let Some(near) = land.grids.near_water else {
        return Some(Phase::Edging { shades });
    };
    // Behind the eye and ahead of it along the stream, as far as the grid
    // laid ahead of the eye reaches either way.
    let (stretch, stones) = bed.take(land, (1.2 * near.reach, 2.2 * near.reach))?;
    if !stretch.flows() {
        return Some(Phase::Edging { shades });
    }
    Some(Phase::Flowing {
        shades,
        solving: Solving::new(stretch, stones, densities.flow)?,
        course: bed.course(),
    })
}

/// How a wood's trees are sown: closely enough for its middling trees where
/// it grows thickest, all the way about the eye as far as their shadows
/// reach, and across the view beyond out to the horizon, as far as its
/// tallest still span a pixel or two, or as far as its most trees would
/// stand thickly enough to fill, or its places fit the detail's; and how far
/// off its trees are still seen.
fn sown_for(stage: &Stage, land: &Land, reader: &Reader, vantage: &Vantage) -> Sown {
    let (count, middling, tallest) =
        reader
            .kinds()
            .fold((0.0, 0.0, 0.0f64), |(count, total, tallest), habit| {
                let typical = habit.heights.iter().sum::<f64>() / real(VARIANTS);
                (
                    count + 1.0,
                    total + habit.crown * typical,
                    tallest.max(habit.tallest()),
                )
            });
    let middling = middling / f64::max(count, 1.0);
    let woodland = &reader.woodland;
    let thickest = woodland.closure.0;
    let apart = thickest * 2.0 * middling * woodland.stature.0;
    let sown = (SOWN * apart).max(LEAST_SOWN);
    let about = ABOUT.min(SHADOWED * tallest);
    let (centre, reach) = land.grids.traced(&stage.fields);
    let corner = mathf::hypot(
        (vantage.eye.x - centre.0).abs() + reach,
        (vantage.eye.z - centre.1).abs() + reach,
    );
    let seen = corner.min(tallest / (SPANNED * stage.pixel.max(1e-6)));
    // Crowns spaced at random no closer than the wood allows fill about
    // this share of the ground they could tile.
    let spacing = thickest * middling.max(1e-3);
    let density = woodland.cover * PACKED / (PI * spacing * spacing);
    let covered = f64::from(woodland.most) / density.max(1e-9);
    let round = PI * about * about;
    let filled = if covered <= round {
        mathf::sqrt(covered / PI)
    } else {
        mathf::sqrt((covered - round) / ACROSS + about * about)
    };
    let sowing = Sowing {
        sown,
        about: about.min(filled),
        far: seen.min(filled),
    }
    .fitted(f64::from(stage.densities.woods.places));
    Sown {
        sowing,
        seen,
        apart,
    }
}

/// How a wood's trees are sown, how far off they are still seen, and how far
/// apart its middling trees stand where it grows thickest.
#[derive(Copy, Clone, Debug)]
struct Sown {
    sowing: Sowing,
    seen: f64,
    apart: f64,
}

/// Whether a tree `height` tall at `at` would wall off the view from
/// `vantage`: standing nearer than `open.0` of its heights ahead of the eye
/// and within `open.1` either side of the way it looks.
fn walls_off(open: (f64, f64), vantage: &Vantage, at: (f64, f64), height: f64) -> bool {
    let (dx, dz) = (at.0 - vantage.eye.x, at.1 - vantage.eye.z);
    let turn = crate::vector::wrapped(mathf::atan2(dx, dz) - vantage.heading);
    mathf::hypot(dx, dz) < open.0 * height && turn.abs() < open.1
}

/// A scene's woods and sward, grown once its land stands: a farmed land's
/// boundaries and woodlots first, then each wood's trees and what grows
/// beneath them, a bounded step at a time, then the sward under them all,
/// knowing their shade.
#[derive(Debug)]
pub(super) struct Growing {
    /// A land's bridges and a farmed land's boundaries and woodlots, while
    /// they are laid, and whether there were any.
    bridging: Option<Bridging>,
    fielding: Option<Fielding>,
    fielded: bool,
    woods: Vec<Wood>,
    /// The plants of the water's edge, set out once the woods' shade is
    /// cast, and a stream's bed, laid and its flow solved before them.
    margins: Option<Margins>,
    bed: Option<Bed>,
    /// Whether a stream's bed was asked for, so its share of the work stays
    /// counted once it is taken to solve its flow.
    bedded: bool,
    sward: Option<Lawning>,
    /// The wood being grown, and how far.
    next: usize,
    phase: Phase,
    /// What each wood's draws and the sward's are keyed from: however many
    /// one wood draws, the rest draw as they would have, so a scene's woods
    /// and sward lie alike at either detail.
    seed: u64,
    /// The draws of the wood, or the sward, being grown.
    dice: Dice,
}

#[allow(
    clippy::large_enum_variant,
    reason = "a planting holds one phase, and a box could not fail gracefully"
)]
#[derive(Debug)]
enum Phase {
    /// The next wood's trees are to be sown.
    Sowing,
    /// The wood's trees are standing; its patches were laid out under the
    /// seed what grows beneath it keeps to.
    Trees { standing: Standing, patches: u32 },
    /// The shade the wood's trees cast about the eye is being cast, for what
    /// grows beneath them.
    Under { casting: Casting, patches: u32 },
    /// What grows beneath the wood is standing in the shade of its trees.
    Beneath {
        standing: Standing,
        shade: Shade,
        patches: u32,
    },
    /// The wood's dead are to be laid in it.
    Deadfall { patches: u32 },
    /// Every wood stands, and the shade they cast over the land is being
    /// cast.
    Shading { shading: Shading },
    /// A stream's bed is being laid, or the water's edge set out in the
    /// light the woods leave it — in their shade, or in the open where no
    /// wood stands — on the water as the stream's flow has shaped it.
    Edging { shades: Option<Shades> },
    /// The flow of the stream down `course` over its bed is being solved.
    Flowing {
        shades: Option<Shades>,
        solving: Solving,
        course: usize,
    },
    /// The water's finer grid is being shaped by the flow from `row`, then
    /// sealed again.
    Surfacing {
        shades: Option<Shades>,
        flow: Flow,
        course: usize,
        row: usize,
    },
    /// The water's finer grid, shaped, is being sealed again.
    Resealing {
        shades: Option<Shades>,
        sealing: Sealing,
    },
    /// Every wood stands, its shade cast over the land for the sward being
    /// laid in it.
    Laying { shades: Shades, laying: Laying },
}

impl Growing {
    /// The woods, water's edge and sward `stage` has been asked for, taken
    /// from it, their draws keyed from one of `dice`; `None` if it was asked
    /// for none of them.
    pub(super) fn from(stage: &mut Stage, dice: &mut Dice) -> Option<Self> {
        let bridging = stage.bridging.take();
        let fielding = stage.fielding.take();
        let woods = core::mem::take(&mut stage.woods);
        let margins = stage.margins.take();
        let bed = stage.bed.take();
        let sward = stage.sward.take();
        let laid = bridging.is_some() || fielding.is_some();
        if !laid && woods.is_empty() && margins.is_none() && bed.is_none() && sward.is_none() {
            return None;
        }
        let seed = dice.wide();
        Some(Self {
            fielded: laid,
            bridging,
            fielding,
            woods,
            margins,
            bedded: bed.is_some(),
            bed,
            sward,
            next: 0,
            phase: Phase::Sowing,
            seed,
            dice: Dice::keyed(seed, 0),
        })
    }

    /// How far the growing has come: each wood a share, and one more for
    /// the shade they cast, the water's edge and the sward laid beneath
    /// them all.
    pub(super) fn done(&self) -> f64 {
        // Setting out the water's edge is a small share of the last part, and
        // a stream's bed and its flow a larger one; laying the sward is most
        // of it.
        let edging = if self.margins.is_some() { 0.05 } else { 0.0 };
        let bedding = if self.bedded { 0.3 } else { 0.0 };
        let laying = if self.sward.is_some() {
            0.75 * (1.0 - edging - bedding)
        } else {
            0.0
        };
        let shading = 1.0 - edging - bedding - laying;
        let within = match &self.phase {
            Phase::Sowing => 0.0,
            Phase::Trees { standing, .. } => 0.55 * standing.done(),
            Phase::Under { casting, .. } => 0.55 + 0.05 * casting.done(),
            Phase::Beneath { standing, .. } => 0.6 + 0.3 * standing.done(),
            Phase::Deadfall { .. } => 0.9,
            Phase::Shading { shading: casting } => shading * casting.done(),
            Phase::Edging { .. } => {
                // A bed once taken has had its flow solved: all its share.
                let laid = self.bed.as_ref().map_or(1.0, |bed| 0.4 * bed.done());
                let edged = self.margins.as_ref().map_or(1.0, Margins::done);
                shading + bedding * laid + edging * edged
            }
            Phase::Flowing { solving, .. } => shading + bedding * (0.4 + 0.5 * solving.done()),
            Phase::Surfacing { .. } | Phase::Resealing { .. } => shading + 0.9 * bedding,
            Phase::Laying { laying: sward, .. } => {
                shading + edging + bedding + laying * sward.done()
            }
        };
        let fielded = usize::from(self.fielded);
        let parts = fielded + self.woods.len() + 1;
        // Bridges are a small share of what is laid first, a farmed land's
        // boundaries the rest.
        if let Some(bridging) = &self.bridging {
            return 0.1 * bridging.done() / real(parts);
        }
        if let Some(fielding) = &self.fielding {
            return (0.1 + 0.9 * fielding.done()) / real(parts);
        }
        share(fielded + self.next.min(self.woods.len()), parts) + within / real(parts)
    }

    /// Grow the next step on `land`; whether everything is grown, or `None`
    /// when the heap will not hold it.
    pub(super) fn step(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
    ) -> Option<bool> {
        if let Some(bridging) = self.bridging.as_mut() {
            if bridging.step(stage, land)? {
                self.bridging = None;
            }
            return Some(false);
        }
        if let Some(fielding) = self.fielding.as_mut() {
            if fielding.step(stage, (land, runner))? {
                self.fielding = None;
            }
            return Some(false);
        }
        let phase = core::mem::replace(&mut self.phase, Phase::Sowing);
        let dice = &mut self.dice;
        let Some(wood) = self.woods.get(self.next) else {
            return self.lay(stage, (land, runner), phase);
        };
        let next = match phase {
            Phase::Sowing => sow(stage, dice, land, wood)?,
            Phase::Trees {
                mut standing,
                patches,
            } => {
                if standing.step(stage, (land, runner), (&wood.vantage, None))? {
                    if let Some(far) = standing.beyond.take() {
                        let stood = standing.crowns.trees.iter().map(|tree| tree.at);
                        stage.carry_on(far.matching(stood)?)?;
                    }
                    roof(stage, wood, patches)?
                } else {
                    Phase::Trees { standing, patches }
                }
            }
            Phase::Under {
                mut casting,
                patches,
            } => {
                if casting.step(&stage.canopies, runner)? {
                    beneath(stage, dice, wood, (casting.finish(), patches))?
                } else {
                    Phase::Under { casting, patches }
                }
            }
            Phase::Beneath {
                mut standing,
                shade,
                patches,
            } => {
                if standing.step(stage, (land, runner), (&wood.vantage, Some(&shade)))? {
                    Phase::Deadfall { patches }
                } else {
                    Phase::Beneath {
                        standing,
                        shade,
                        patches,
                    }
                }
            }
            Phase::Deadfall { patches } => {
                if let Some(deadfall) = &wood.deadfall {
                    lay_deadfall(stage, dice, (land, wood, deadfall), patches)?;
                }
                self.next += 1;
                self.dice = Dice::keyed(self.seed, self.next);
                Phase::Sowing
            }
            Phase::Shading { .. }
            | Phase::Edging { .. }
            | Phase::Flowing { .. }
            | Phase::Surfacing { .. }
            | Phase::Resealing { .. }
            | Phase::Laying { .. } => return None,
        };
        self.phase = next;
        Some(false)
    }

    /// The next step once every wood stands, out of `phase`: the woods'
    /// shade cast over the land a unit at a time, the water's edge set out
    /// in the light it leaves, and then the sward laid in it; whether all is
    /// laid.
    fn lay(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
        phase: Phase,
    ) -> Option<bool> {
        let dice = &mut self.dice;
        let shades = match phase {
            Phase::Laying { shades, mut laying } => {
                if !laying.step(stage, dice, (&shades, runner))? {
                    self.phase = Phase::Laying { shades, laying };
                    return Some(false);
                }
                shades
            }
            Phase::Edging { shades } => {
                // A stream's flow shapes its water before the water's edge
                // floats on it.
                if let Some(bed) = self.bed.as_mut() {
                    if !bed.finished() {
                        bed.step(stage, (land, runner))?;
                        self.phase = Phase::Edging { shades };
                        return Some(false);
                    }
                    self.phase = flowing(bed, land, shades, &stage.densities.bed)?;
                    self.bed = None;
                    return Some(false);
                }
                if let Some(margins) = self.margins.as_mut().filter(|margins| !margins.finished()) {
                    margins.step(stage, (land, shades.as_ref(), runner))?;
                    self.phase = Phase::Edging { shades };
                    return Some(false);
                }
                let Some(shades) = shades else {
                    return Some(true);
                };
                if let Some(lawning) = &self.sward {
                    let laying = landscape::sward(stage, dice, land, lawning)?;
                    self.phase = Phase::Laying { shades, laying };
                    return Some(false);
                }
                shades
            }
            Phase::Shading { mut shading } => {
                if shading.step(&stage.canopies, runner)? {
                    self.phase = Phase::Edging {
                        shades: Some(shading.finish()),
                    };
                } else {
                    self.phase = Phase::Shading { shading };
                }
                return Some(false);
            }
            phase @ (Phase::Flowing { .. } | Phase::Surfacing { .. } | Phase::Resealing { .. }) => {
                self.phase = surfacing(stage, (land, runner), phase)?;
                return Some(false);
            }
            _ => {
                // The shade is cast about the sward's eye, or the first
                // wood's where nothing grows beneath them: the crowns roof
                // the air and strew the ground with what they shed either
                // way. A land with neither has nothing to roof or strew.
                let eye = match (&self.sward, self.woods.first()) {
                    (Some(lawning), _) => Some(lawning.eye),
                    (None, Some(wood)) => Some((wood.vantage.eye.x, wood.vantage.eye.z)),
                    (None, None) => None,
                };
                self.phase = match eye {
                    Some(eye) => Phase::Shading {
                        shading: Shading::new(
                            &stage.canopies,
                            (land.grids.centre, land.grids.reach),
                            eye,
                        )?,
                    },
                    None => Phase::Edging { shades: None },
                };
                return Some(false);
            }
        };
        // Last year's leaves, rotting, where none fell this autumn.
        let shed = self
            .sward
            .take()
            .and_then(|lawning| lawning.grassland.fallen)
            .map(|fallen| (fallen.kind, fallen.age))
            .or_else(|| Some((self.woods.first()?.grove.first()?.kind, 1.0)));
        if let (Some((kind, age)), Some(ground)) = (shed, landscape::ground_of(stage, land)) {
            let (leaves, humus, moss) = plants::litter(kind, age);
            ground.floor = Some(Floor {
                shades: shades.copied()?,
                leaves,
                humus,
                moss,
            });
        }
        stage.shades = Some(shades);
        Some(true)
    }
}

/// `wood`'s trees sown over `land`: the places they might stand, to be read
/// and thinned step by step.
fn sow(stage: &Stage, dice: &mut Dice, land: &Land, wood: &Wood) -> Option<Phase> {
    let reader = Reader {
        woodland: wood.woodland,
        rooting: wood.rooting,
        kinds: wood.grove.habits(),
        seeds: (dice.seed(), dice.seed()),
    };
    let vantage = &wood.vantage;
    let sown = sown_for(stage, land, &reader, vantage);
    let mut standing = Standing::new(stage, &reader, (vantage, sown.sowing))?;
    let stood = standing.reach();
    // Shrubs are low enough to be stood one by one as far as they are seen.
    let trees = wood.grove.kinds().all(|grown| !grown.kind.shrub());
    if stage.densities.woods.beyond && trees && sown.seen > stood {
        let stretch = Stretch {
            eye: (vantage.eye.x, vantage.eye.z),
            heading: vantage.heading,
            across: ACROSS,
            from: stood,
            to: sown.seen,
            cell: sown.apart,
            sown: sown.sowing.sown,
        };
        // Over a lattice of its own, keyed from the wood's.
        let seed = mix32(reader.seeds.1 ^ 0xfa12_3e3d);
        standing.beyond = FarWood::new((reader, seed), (land.grids, &stage.fields), stretch);
    }
    Some(Phase::Trees {
        standing,
        patches: reader.seeds.0,
    })
}

/// The shade `wood`'s trees cast about the eye, to be cast for what grows
/// beneath them; the wood's dead next if nothing does.
fn roof(stage: &Stage, wood: &Wood, patches: u32) -> Option<Phase> {
    if wood.beneath.is_none() {
        return Some(Phase::Deadfall { patches });
    }
    let eye = (wood.vantage.eye.x, wood.vantage.eye.z);
    let reach = BENEATH + ROOFED;
    let square = (
        (eye.0 - reach, eye.1 - reach),
        (eye.0 + reach, eye.1 + reach),
    );
    Some(Phase::Under {
        casting: Casting::new(&stage.canopies, square, NEAR_CELL, ROOFED)?,
        patches,
    })
}

/// What grows beneath `wood`'s trees, sown in the `shade` they cast about
/// the eye, in the wood's own patches laid out under `patches`; the wood's
/// dead next, if nothing grows beneath it or no crown shades the eye's
/// ground.
fn beneath(
    stage: &Stage,
    dice: &mut Dice,
    wood: &Wood,
    (shade, patches): (Shade, u32),
) -> Option<Phase> {
    let Some(beneath) = &wood.beneath else {
        return Some(Phase::Deadfall { patches });
    };
    if shade.is_open() {
        return Some(Phase::Deadfall { patches });
    }
    let sown = 100.0 / mathf::sqrt(beneath.plants.max(1.0));
    let sowing = Sowing {
        sown,
        about: ABOUT_BENEATH,
        far: BENEATH,
    }
    .fitted(f64::from(stage.densities.woods.places));
    let reader = Reader {
        woodland: understory(&wood.woodland),
        rooting: wood.rooting,
        kinds: beneath.grove.habits(),
        // Over a lattice of its own.
        seeds: (patches, dice.seed()),
    };
    Some(Phase::Beneath {
        standing: Standing::new(stage, &reader, (&wood.vantage, sowing))?,
        shade,
        patches,
    })
}

/// A kind of a wood's dead.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Piece {
    Log,
    Stump,
    Snag,
}

/// Lay `deadfall` about the eye in `wood` on `land`, its patches laid out
/// under `patches`: fallen trunks, more where the canopy has opened and those
/// lying the way the wind threw them; stumps; and the standing dead among the
/// living. `None` when the stage will not hold them.
fn lay_deadfall(
    stage: &mut Stage,
    dice: &mut Dice,
    (land, wood, deadfall): (&Land, &Wood, &Deadfall),
    patches: u32,
) -> Option<()> {
    let dead: [Option<&Dead>; 2] = [deadfall.dead[0].as_ref(), deadfall.dead[1].as_ref()];
    let kinds = u32::try_from(dead.iter().flatten().count()).ok()?;
    if kinds == 0 {
        return Some(());
    }
    let eye = (wood.vantage.eye.x, wood.vantage.eye.z);
    let hectares = PI * DEADFALL_REACH * DEADFALL_REACH / 10_000.0;
    let wind = dice.range(0.0, TAU);
    for (density, piece) in [
        (deadfall.logs, Piece::Log),
        (deadfall.stumps, Piece::Stump),
        (deadfall.snags, Piece::Snag),
    ] {
        let wanted = u32::try_from(mathf::round_i32(density * hectares).max(0)).ok()?;
        let mut strewn = 0;
        for _ in 0..wanted.saturating_mul(DEADFALL_TRIES) {
            // What the scene sets out after its woods keeps its room.
            if strewn >= wanted || stage.room() <= KEPT {
                break;
            }
            let angle = dice.range(0.0, TAU);
            let distance = DEADFALL_REACH * mathf::sqrt(dice.unit());
            let at = (
                eye.0 + mathf::sin(angle) * distance,
                eye.1 + mathf::cos(angle) * distance,
            );
            let opened = gap(&wood.woodland, patches, at);
            let chance = dice.unit();
            if chance >= wooded(&wood.woodland, patches, at) * (1.0 + 2.0 * opened) / 3.0 {
                continue;
            }
            let lie = land.grids.lie(&stage.fields, at.0, at.1);
            if lie.upright < 0.8
                || lie.road > 0.0
                || lie.path > 0.3
                || wood.rooting.suits(&lie, at) < 0.2
                || land.grids.wet_at(&stage.fields, at.0, at.1)
            {
                continue;
            }
            let pick = dice.count(0, kinds - 1) as usize;
            let Some(&kind) = dead.iter().flatten().nth(pick) else {
                continue;
            };
            let placed = match piece {
                Piece::Log => fell(
                    stage,
                    dice,
                    (land, kind),
                    (at, eye),
                    if opened > 0.3 { Some(wind) } else { None },
                )?,
                Piece::Stump => stand_stump(stage, dice, (&lie, kind), (at, eye))?,
                Piece::Snag => stand_snag(stage, dice, (&lie, kind), (at, eye))?,
            };
            strewn += u32::from(placed);
        }
    }
    Some(())
}

/// Lay a fallen trunk of `dead` from its foot at `at` on `land`, clear of the
/// eye at `eye`: the way `wind` threw it, if the wind felled it, and any way
/// otherwise; whether it found room, or `None` when the stage will not hold it.
fn fell(
    stage: &mut Stage,
    dice: &mut Dice,
    (land, dead): (&Land, &Dead),
    (at, eye): ((f64, f64), (f64, f64)),
    wind: Option<f64>,
) -> Option<bool> {
    let &(prototype, length, radius) = dead
        .logs
        .get(dice.count(0, u32::try_from(DEAD_VARIANTS - 1).ok()?) as usize)?;
    let scale = dice.range(0.8, 1.15);
    let (length, radius) = (length * scale, radius * scale);
    let heading = match wind {
        Some(wind) => wind + dice.range(-0.4, 0.4),
        None => dice.range(0.0, TAU),
    };
    let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
    let room = (1.2 * radius).max(0.3);
    let discs = u32::try_from(mathf::round_i32(mathf::ceil(length / (1.5 * room))).max(2)).ok()?;
    let disc = |index: u32| {
        let share = f64::from(index) / f64::from(discs - 1);
        (at.0 + sin * length * share, at.1 + cos * length * share)
    };
    let clear = (0..discs).map(disc).all(|place| {
        mathf::hypot(place.0 - eye.0, place.1 - eye.1) > DEADFALL_CLEAR && stage.clear(place, room)
    });
    if !clear {
        return Some(false);
    }
    let tip = disc(discs - 1);
    let (foot, top) = (
        land.grids.height(&stage.fields, at.0, at.1),
        land.grids.height(&stage.fields, tip.0, tip.1),
    );
    let pitch = mathf::atan2(top - foot, length);
    let pose = Pose::new(Vec3::new(at.0, foot, at.1), Frame::turned(heading, -pitch));
    dead.lay(stage, (prototype, scale), pose, dice.seed())?;
    for index in 0..discs {
        stage.claim(disc(index), room)?;
    }
    Some(true)
}

/// Stand a stump of `dead` at `at` on ground `lie` describes, clear of the
/// eye at `eye`: whether it found room, or `None` when the stage will not
/// hold it.
fn stand_stump(
    stage: &mut Stage,
    dice: &mut Dice,
    (lie, dead): (&Lie, &Dead),
    (at, eye): ((f64, f64), (f64, f64)),
) -> Option<bool> {
    let &(prototype, _, radius) = dead
        .stumps
        .get(dice.count(0, u32::try_from(DEAD_VARIANTS - 1).ok()?) as usize)?;
    let scale = dice.range(0.8, 1.2);
    let room = (1.6 * radius * scale).max(0.3);
    if mathf::hypot(at.0 - eye.0, at.1 - eye.1) <= DEADFALL_CLEAR || !stage.clear(at, room) {
        return Some(false);
    }
    let pose = Pose::new(
        Vec3::new(at.0, lie.height, at.1),
        Frame::turned(dice.range(0.0, TAU), 0.0),
    );
    dead.lay(stage, (prototype, scale), pose, dice.seed())?;
    stage.claim(at, room)?;
    Some(true)
}

/// Stand a dead tree of `dead` at `at` on ground `lie` describes, clear of
/// the eye at `eye` and the trunks about it: whether it found room, or `None`
/// when the stage will not hold it.
fn stand_snag(
    stage: &mut Stage,
    dice: &mut Dice,
    (lie, dead): (&Lie, &Dead),
    (at, eye): ((f64, f64), (f64, f64)),
) -> Option<bool> {
    let snags = &dead.snags.habit;
    let tallest = snags.heights.iter().copied().fold(0.0, f64::max);
    let wanted = tallest * dice.range(0.5, 1.0);
    let variant = snags.nearest(wanted, dice.unit());
    let scale = snags.sized(wanted, *snags.heights.get(variant)?);
    let height = scale * *snags.heights.get(variant)?;
    if mathf::hypot(at.0 - eye.0, at.1 - eye.1) <= DEADFALL_CLEAR || !stage.clear(at, trunk(height))
    {
        return Some(false);
    }
    let base = Vec3::new(at.0, rooted(lie, height), at.1);
    let pose = Pose::new(base, Frame::turned(dice.range(0.0, TAU), 0.0));
    plants::place(stage, snags, (variant, scale), (pose, dice.seed()))?;
    stage.claim(at, trunk(height))?;
    Some(true)
}

/// How what grows beneath `woodland` grows: in the wood's own patches, its
/// plants a little apart and none in the way of the view.
fn understory(woodland: &Woodland) -> Woodland {
    Woodland {
        closure: (0.5, 0.8),
        stature: (0.6, 1.0),
        most: MOST_BENEATH,
        open: (1.5, 0.3),
        ..*woodland
    }
}

#[cfg(test)]
#[path = "woodland_tests.rs"]
mod tests;
