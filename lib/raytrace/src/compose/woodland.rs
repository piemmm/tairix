//! Woodland: trees stood over a land as a wood grows them.
//!
//! A wood covers the ground in patches, and within them its trees stand as
//! far apart as their crowns spread — closer where it grows thick, further
//! where it is open — a much shorter tree standing under a taller one's
//! crown. Each kind keeps to the ground it takes to and grows in stands of
//! its own, each stand as tall as it is old. Shrubs, ferns and young trees
//! take the light that reaches the floor beneath, and the wood's dead lie
//! among the living.
//!
//! A wood grows in two layers, each sown on a lattice of its own fixed to the
//! land and thinned by the one rule [`crate::wood`] keeps, rows of places
//! read and ruled on across the runner at once. Its canopy is thinned all
//! about the eye and across the view out to where its trees still span a
//! pixel or two: the nearest it keeps are stood one by one, where their
//! shadows and the sky they hide fall on what is seen, and the rest across
//! the view are carried on as the wood far off, so the two are one wood and
//! no trunk stands in another's room where they meet. Its subcanopy, the
//! trees beneath, is stood about the eye only, each place yielding to the
//! canopy trees about it; far off, the canopy hides it.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_mix::mix32;
use tairix_parallel::JobRunner;
use tairix_scatter::{Cell, Offers, Span, Thinning};
use tairix_util::{fallible, mathf};

use super::chains::Chains;
use super::fields::Fielding;
use super::footprint::Footprints;
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
use crate::land::{Grids, Land};
use crate::shade::{Casting, Shade, Shades, Shading, NEAR_CELL, ROOFED};
use crate::stream::{Flow, Solving};
use crate::vector::{real, share, wrapped, Frame, Pose, Vec3};
use crate::wood::{
    crowd, gap, rooted, trunk, wooded, Layer, Offer, Reader, Rooting, Sown, Spread, Woodland,
    PACKED, ROUNDS, VARIANTS,
};
use tairix_land::attributes::Lie;

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
/// its canopy's close enough that thinning, not the lattice, sets where they
/// stand, its subcanopy's closer, its trees smaller and many fitting between
/// the canopy's trunks; and never closer than so many metres.
const CANOPY_SOWN: f64 = 1.0;
const SUBCANOPY_SOWN: f64 = 0.6;
const LEAST_SOWN: f64 = 0.35;

/// How far about the eye what grows beneath a wood is set out: nearer than
/// that, a shrub is more than a few pixels; and how far all the way round.
const BENEATH: f64 = 140.0;
const ABOUT_BENEATH: f64 = 40.0;

/// The most plants what grows beneath a wood holds.
const MOST_BENEATH: u32 = 30_000;

/// Room kept among the stage's objects for what a scene sets out after its
/// woods.
pub(super) const KEPT: usize = 512;

/// How many of a wood's plants a step stands one by one for each core it
/// reads them on, and the most it stands however many cores read them:
/// setting them on the stage is one core's work.
const STAND_UNIT: usize = 1 << 9;
const STAND_MOST: usize = 1 << 12;

/// How many sides of a polygon the arc a wood's region ends in is held within.
const ARC_SIDES: u32 = 16;

/// How the places a wood's plants might stand are sown: how far apart, how
/// far all the way about the eye, and how far across the view.
#[derive(Copy, Clone, Debug)]
struct Sowing {
    sown: f64,
    about: f64,
    far: f64,
}

impl Sowing {
    /// About how many places it sows: one to each cell of its lattice over
    /// the eye's round and the stretch across the view beyond.
    fn places(&self) -> f64 {
        let round = mathf::fmin(self.about, self.far);
        let ground = PI * round * round + ACROSS * (self.far * self.far - round * round);
        ground / mathf::fmax(self.sown * self.sown, 1e-18)
    }

    /// The sowing kept to `most` places, reaching only as far as they fit.
    fn fitted(self, most: f64) -> Self {
        if self.places() <= most {
            return self;
        }
        let ground = most * self.sown * self.sown;
        let round = PI * self.about * self.about;
        let far = if round >= ground {
            mathf::sqrt(ground / PI)
        } else {
            mathf::sqrt((ground - round) / ACROSS + self.about * self.about)
        };
        Self {
            about: mathf::fmin(self.about, far),
            far,
            ..self
        }
    }
}

/// The lattice `reader`'s places are sown on, cells `side` metres square,
/// drawn under a seed of its wood's and its layer's own.
fn lattice(reader: &Reader, side: f64) -> Sown {
    let layer = match reader.layer {
        Layer::Canopy => 0,
        Layer::Subcanopy => 0x9e37_79b9,
        Layer::Floor => 0x7f4a_7c15,
    };
    Sown {
        side,
        seed: mix32(reader.seeds.1 ^ 0x5eed_c311 ^ layer),
    }
}

/// Where a wood's places are thinned and stood, seen from `eye` looking
/// `heading`: all the way about the eye out to `about`, and within `across`
/// either side of the view out to `near` stood one by one, then on to `far`
/// carried on beyond, within the square its land traces.
#[derive(Copy, Clone, Debug)]
struct Region {
    eye: (f64, f64),
    heading: f64,
    across: f64,
    about: f64,
    near: f64,
    far: f64,
    traced: ((f64, f64), f64),
}

/// Which part of its wood a place kept belongs to.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Part {
    /// Stood one by one, `distance` from the eye, across the view or not.
    Near { distance: f64, across: bool },
    /// Carried on far off.
    Far,
}

impl Region {
    /// Which part of the wood a place kept at `at` belongs to; `None` off the
    /// region.
    fn part(&self, at: (f64, f64)) -> Option<Part> {
        if !on_land(self.traced, at) {
            return None;
        }
        let (dx, dz) = (at.0 - self.eye.0, at.1 - self.eye.1);
        let distance = mathf::hypot(dx, dz);
        let across = wrapped(mathf::atan2(dx, dz) - self.heading).abs() <= self.across;
        if distance < self.about || (across && distance < self.near) {
            Some(Part::Near { distance, across })
        } else if across && distance <= self.far {
            Some(Part::Far)
        } else {
            None
        }
    }

    /// The rows of `sown`'s cells it covers, rows increasing, each across
    /// every column a place of it could lie in; `None` when the heap will not
    /// hold them, or for a region further off than any land reaches.
    fn spans(&self, sown: &Sown) -> Option<Vec<Span>> {
        let fan = self.fan()?;
        let ((cx, cz), half) = self.traced;
        let (low, high) = fan.iter().fold(
            (self.eye.1 - self.about, self.eye.1 + self.about),
            |(low, high), &(_, z)| (mathf::fmin(low, z), mathf::fmax(high, z)),
        );
        let (low, high) = (mathf::fmax(low, cz - half), mathf::fmin(high, cz + half));
        if low > high {
            return Some(Vec::new());
        }
        let first = sown.cell((cx, low))?.1 - 1;
        let last = sown.cell((cx, high))?.1 + 1;
        let mut spans = Vec::new();
        spans
            .try_reserve_exact(usize::try_from(last - first + 1).ok()?)
            .ok()?;
        for row in first..=last {
            let z = f64::from(i32::try_from(row).ok()?) * sown.side;
            let strip = (z, z + sown.side);
            let round = disc_across(self.eye, self.about, strip);
            let ahead = polygon_across(&fan, strip);
            let joined = match (round, ahead) {
                (Some(a), Some(b)) => Some((mathf::fmin(a.0, b.0), mathf::fmax(a.1, b.1))),
                (one, other) => one.or(other),
            };
            let Some((from, to)) = joined else {
                continue;
            };
            let (from, to) = (mathf::fmax(from, cx - half), mathf::fmin(to, cx + half));
            if from > to {
                continue;
            }
            // A cell either side, so no place jittered to its cell's edge is
            // missed.
            let columns = (sown.cell((from, z))?.0 - 1, sown.cell((to, z))?.0 + 2);
            spans.push(Span { row, columns });
        }
        Some(spans)
    }

    /// The polygon holding the stretch across the view: the eye, and corners
    /// out along the view's span far enough that each side touches the arc
    /// of its far edge.
    fn fan(&self) -> Option<Vec<(f64, f64)>> {
        let step = 2.0 * self.across / f64::from(ARC_SIDES);
        let out = self.far / mathf::cos(0.5 * step);
        let mut corners = Vec::new();
        corners
            .try_reserve_exact(usize::try_from(ARC_SIDES).ok()? + 2)
            .ok()?;
        corners.push(self.eye);
        for side in 0..=ARC_SIDES {
            let angle = self.heading - self.across + f64::from(side) * step;
            corners.push((
                self.eye.0 + out * mathf::sin(angle),
                self.eye.1 + out * mathf::cos(angle),
            ));
        }
        Some(corners)
    }
}

/// Whether `at` lies on the land the square `traced` holds.
fn on_land(((cx, cz), half): ((f64, f64), f64), at: (f64, f64)) -> bool {
    (at.0 - cx).abs() < half && (at.1 - cz).abs() < half
}

/// Where along x the disc `radius` about `centre` lies within the strip of z
/// from `low` to `high`, if it reaches it.
fn disc_across(centre: (f64, f64), radius: f64, (low, high): (f64, f64)) -> Option<(f64, f64)> {
    let off = mathf::fmax(mathf::fmax(low - centre.1, centre.1 - high), 0.0);
    (off < radius).then(|| {
        let half = mathf::sqrt(radius * radius - off * off);
        (centre.0 - half, centre.0 + half)
    })
}

/// Where along x the convex polygon `corners` lies within the strip of z
/// from `low` to `high`, if it reaches it: at its corners within the strip,
/// or where its sides cross the strip's edges.
fn polygon_across(corners: &[(f64, f64)], (low, high): (f64, f64)) -> Option<(f64, f64)> {
    let mut across: Option<(f64, f64)> = None;
    let mut take = |x: f64| {
        across = Some(across.map_or((x, x), |(from, to)| {
            (mathf::fmin(from, x), mathf::fmax(to, x))
        }));
    };
    let next = corners.iter().cycle().skip(1);
    for (&(x, z), &(nx, nz)) in corners.iter().zip(next) {
        if (low..=high).contains(&z) {
            take(x);
        }
        for edge in [low, high] {
            if (z - edge) * (nz - edge) < 0.0 {
                take(x + (nx - x) * (edge - z) / (nz - z));
            }
        }
    }
    across
}

/// The canopy a subcanopy grows beneath: its trees stood one by one, each
/// chained into the cell of a grid about the eye its trunk stands in, a cell
/// as broad as the most room a canopy tree and a tree beneath keep.
#[derive(Debug)]
struct Canopy {
    trees: Vec<Offer>,
    chains: Chains,
    room: f64,
}

/// The most cells a side of a canopy's grid holds: past it, the cells grow
/// broader instead, holding more trees each.
const MOST_SIDE: usize = 1024;

impl Canopy {
    /// A canopy to hold `count` trees, all within `reach` of `eye`, keeping
    /// no more than `room` with a tree beneath it; `None` when the heap will
    /// not hold it.
    fn new(count: usize, (eye, reach): ((f64, f64), f64), room: f64) -> Option<Self> {
        let mut trees = Vec::new();
        trees.try_reserve_exact(count).ok()?;
        Some(Self {
            trees,
            chains: Chains::new((eye, reach + room), room.max(1.0), MOST_SIDE)?,
            room,
        })
    }

    /// Chain `tree` into the cell its trunk stands in.
    fn add(&mut self, tree: Offer) -> Option<()> {
        let index = u32::try_from(self.trees.len()).ok()?;
        self.trees.try_reserve(1).ok()?;
        let (own, _) = self.chains.span(tree.at, 0.0);
        self.chains.link(index, own)?;
        self.trees.push(tree);
        Some(())
    }

    /// Whether any of its trees crowds `offer`.
    fn crowds(&self, offer: &Offer) -> bool {
        let (about, _) = self.chains.span(offer.at, self.room);
        self.chains.within(about).any(|id| {
            self.trees
                .get(id as usize)
                .is_some_and(|tree| crowd(offer, tree))
        })
    }
}

/// The places a wood's lattice offers on the stage: wherever its land would
/// grow a plant, less those the scene keeps open — before the eye, where one
/// would wall the view off, and under whatever already stands — and those a
/// canopy above crowds.
struct Offering<'a> {
    reader: &'a Reader,
    sown: Sown,
    grids: &'a Grids,
    fields: &'a [Heightfield],
    /// The shade the canopy above casts, where what grows beneath it is read.
    beneath: Option<&'a Shade>,
    footprints: &'a Footprints,
    canopy: Option<&'a Canopy>,
    vantage: &'a Vantage,
    traced: ((f64, f64), f64),
    reach: u32,
}

impl Offers for Offering<'_> {
    type Offer = Offer;
    type Rank = (u32, u32);
    type Bound = Spread;

    fn reach(&self) -> u32 {
        self.reach
    }

    fn offer(&self, cell: Cell) -> Option<Offer> {
        let (at, draw) = self.sown.place(cell)?;
        if !on_land(self.traced, at) {
            return None;
        }
        let tree = self
            .reader
            .tree((self.grids, self.fields), (at, draw), self.beneath)?;
        let open = self.reader.woodland.open;
        let offer = Offer::of(&tree, (at, draw));
        let barred = walls_off(open, self.vantage, at, tree.height)
            || !self.footprints.clear(at, trunk(tree.height))
            || self.canopy.is_some_and(|canopy| canopy.crowds(&offer));
        (!barred).then_some(offer)
    }

    fn rank(&self, offer: &Offer) -> (u32, u32) {
        offer.rank()
    }

    fn crowds(&self, offer: &Offer, other: &Offer) -> bool {
        crowd(offer, other)
    }

    fn bound(&self, offer: &Offer) -> Spread {
        Spread::of(offer)
    }

    fn join(&self, one: Spread, other: Spread) -> Spread {
        one.join(other)
    }

    fn within(&self, offer: &Offer, spread: Spread, cells: u32) -> bool {
        self.sown.near(cells, spread.room(offer))
    }
}

/// A place kept to stand one by one, `distance` from the eye, and whether it
/// lies across the view.
#[derive(Copy, Clone, Debug)]
struct Nearest {
    distance: f64,
    cell: Cell,
    across: bool,
    offer: Offer,
}

impl Ord for Nearest {
    /// Nearer first, and of two as near, the one in the earlier row or, in
    /// the same row, the earlier column.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.distance
            .total_cmp(&other.distance)
            .then((self.cell.1, self.cell.0).cmp(&(other.cell.1, other.cell.0)))
    }
}

impl PartialOrd for Nearest {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Nearest {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Nearest {}

/// A wood's plants being stood a step at a time: the places of its lattice
/// thinned over its region a batch of rows at a time, then the nearest kept
/// stood one by one and the rest across the view carried on beyond.
#[derive(Debug)]
struct Standing {
    reader: Reader,
    sown: Sown,
    region: Region,
    /// How many cells apart two of its places may crowd each other.
    reach: u32,
    thinning: Thinning<Offer, (u32, u32), Spread>,
    /// What a step of the thinning kept, its room kept for the next.
    kept: Vec<(Cell, Offer)>,
    /// The nearest places kept, farthest on top, and the most that stand:
    /// once thinned, given up a batch at a time as they are stood.
    nearest: BinaryHeap<Nearest>,
    most: usize,
    batch: Vec<Nearest>,
    /// Once thinned, what it stands; and how many stand so far.
    thinned: Option<Thinned>,
    stood: usize,
    /// The wood carried on past them, and the canopy they grow beneath.
    beyond: Option<FarWood>,
    canopy: Option<Canopy>,
    /// The room a tree beneath keeps from its trees, when a subcanopy is to
    /// grow beneath them.
    sheltering: Option<f64>,
}

/// What a wood's thinning left to stand one by one: how many places, how far
/// about the eye a tree may stand and find every one that could crowd it
/// among them, and the canopy they make as they stand, when a subcanopy is
/// to grow beneath them.
#[derive(Debug)]
struct Thinned {
    places: usize,
    reach: f64,
    canopy: Option<Canopy>,
}

impl Standing {
    /// No more than `most` of the plants `reader` reads, sown on `sown` over
    /// `region`, to be stood beneath `canopy`, those across the view past
    /// what stands one by one carried on as `beyond`, sheltering a subcanopy
    /// keeping the room `sheltering` names from them; `None` when the heap
    /// will not hold its rows.
    fn new(
        stage: &Stage,
        (reader, most): (&Reader, usize),
        (sown, region): (Sown, Region),
        (beyond, canopy, sheltering): (Option<FarWood>, Option<Canopy>, Option<f64>),
    ) -> Option<Self> {
        let reach = sown.reach(reader.widest_room())?;
        Some(Self {
            reader: *reader,
            sown,
            region,
            reach,
            thinning: Thinning::new(region.spans(&sown)?, reach, ROUNDS).ok()?,
            kept: Vec::new(),
            nearest: BinaryHeap::new(),
            most: most.min(stage.room().saturating_sub(KEPT)),
            batch: Vec::new(),
            thinned: None,
            stood: 0,
            beyond,
            canopy,
            sheltering,
        })
    }

    /// The canopy its trees made as they stood, for a subcanopy to grow
    /// beneath, and how far about the eye a tree beneath may stand and find
    /// every canopy tree that could crowd it among them; `None` until they
    /// stand, or for trees that shelter none.
    fn sheltered(self) -> Option<(Canopy, f64)> {
        let Thinned { reach, canopy, .. } = self.thinned?;
        let canopy = canopy.filter(|_| self.nearest.is_empty())?;
        let room = canopy.room;
        Some((canopy, reach - room))
    }

    /// How far the standing has come: its places thinned, then stood.
    fn done(&self) -> f64 {
        match &self.thinned {
            None => 0.9 * self.thinning.done(),
            Some(thinned) => 0.9 + 0.1 * share(self.stood, thinned.places),
        }
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
        if let Some(thinned) = self.thinned.as_mut() {
            let count = (STAND_UNIT * runner.width().max(1))
                .min(STAND_MOST)
                .min(self.nearest.len());
            self.batch.clear();
            self.batch.try_reserve(count).ok()?;
            // Farthest first, as the heap gives them up: the thinning chose
            // which stand, and the order they stand in changes none.
            for _ in 0..count {
                self.batch.push(self.nearest.pop()?);
            }
            // The trees their offers were weighed by, read again as they were.
            let mut grown = fallible::filled(count, None)?;
            let (reader, sown, ground) = (&self.reader, self.sown, (&land.grids, &*stage.fields));
            let pieces = self
                .batch
                .chunks(STAND_UNIT)
                .zip(grown.chunks_mut(STAND_UNIT));
            tairix_parallel::for_each_drawn(runner, pieces, &|(nearests, trees)| {
                for (nearest, tree) in nearests.iter().zip(trees.iter_mut()) {
                    *tree = sown
                        .place(nearest.cell)
                        .and_then(|place| Some((place, reader.tree(ground, place, shade)?)));
                }
            });
            for ((at, draw), tree) in grown.into_iter().flatten() {
                let habit = self.reader.kinds().nth(usize::from(tree.kind))?;
                plants::place(
                    stage,
                    habit,
                    (usize::from(tree.variant), tree.scale),
                    tree.placing(at, draw),
                )?;
                stage.claim(at, trunk(tree.height))?;
            }
            if let Some(canopy) = thinned.canopy.as_mut() {
                for nearest in &self.batch {
                    canopy.add(nearest.offer)?;
                }
            }
            self.stood += count;
            return Some(self.nearest.is_empty());
        }
        // Nothing of this wood stands on the stage until all of it is
        // thinned, so every place is offered against the same stage.
        let offering = Offering {
            reader: &self.reader,
            sown: self.sown,
            grids: &land.grids,
            fields: &stage.fields,
            beneath: shade,
            footprints: &stage.footprints,
            canopy: self.canopy.as_ref(),
            vantage,
            traced: self.region.traced,
            reach: self.reach,
        };
        let thinned = self.thinning.step(&offering, runner, &mut self.kept).ok()?;
        let kept = core::mem::take(&mut self.kept);
        for &(cell, offer) in &kept {
            self.sort(cell, offer)?;
        }
        self.kept = kept;
        self.kept.clear();
        if thinned {
            let places = self.nearest.len();
            let reach = if places >= self.most {
                self.nearest
                    .peek()
                    .map_or(0.0, |farthest| farthest.distance)
            } else {
                mathf::fmax(self.region.about, self.region.near)
            };
            let canopy = match self.sheltering {
                Some(room) => Some(Canopy::new(places, (self.region.eye, reach), room)?),
                None => None,
            };
            self.thinned = Some(Thinned {
                places,
                reach,
                canopy,
            });
        }
        Some(false)
    }

    /// Keep the place `offer` at `cell` among the nearest to stand one by
    /// one, carrying on beyond whatever across the view they leave out.
    fn sort(&mut self, cell: Cell, offer: Offer) -> Option<()> {
        match self.region.part(offer.at) {
            Some(Part::Near { distance, across }) => {
                self.nearest.try_reserve(1).ok()?;
                self.nearest.push(Nearest {
                    distance,
                    cell,
                    across,
                    offer,
                });
                if self.nearest.len() > self.most {
                    if let Some(farthest) = self.nearest.pop().filter(|farthest| farthest.across) {
                        self.carry(farthest.cell, &farthest.offer)?;
                    }
                }
            }
            Some(Part::Far) => self.carry(cell, &offer)?,
            None => {}
        }
        Some(())
    }

    /// Carry the place `offer` at `cell` on in the wood far off, if there is
    /// one; `None` where that wood cannot mark it, so a tree across the view
    /// is never silently lost.
    fn carry(&mut self, cell: Cell, offer: &Offer) -> Option<()> {
        self.beyond
            .as_mut()
            .map_or(Some(()), |beyond| beyond.mark(cell, offer))
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

/// How a wood's canopy and subcanopy are sown: each closely enough for its
/// middling trees where it grows thickest, all the way about the eye as far
/// as their shadows reach, and across the view beyond out to the horizon, as
/// far as its tallest still span a pixel or two, or as far as its most trees
/// would stand thickly enough to fill, or either layer's places fit the
/// detail's; and how far off its trees are still seen.
fn sown_for(stage: &Stage, land: &Land, reader: &Reader, vantage: &Vantage) -> Sowings {
    let (count, middling, tallest) =
        reader
            .kinds()
            .fold((0.0, 0.0, 0.0f64), |(count, total, tallest), habit| {
                let typical = habit.heights.iter().sum::<f64>() / real(VARIANTS);
                (
                    count + 1.0,
                    total + habit.crown * typical,
                    mathf::fmax(tallest, habit.tallest()),
                )
            });
    let middling = middling / mathf::fmax(count, 1.0);
    let woodland = &reader.woodland;
    let thickest = woodland.closure.0;
    let apart = thickest * 2.0 * middling * woodland.stature.0;
    let sown = |share: f64| mathf::fmax(share * apart, LEAST_SOWN);
    let about = mathf::fmin(ABOUT, SHADOWED * tallest);
    let (centre, reach) = land.grids.traced(&stage.fields);
    let corner = mathf::hypot(
        (vantage.eye.x - centre.0).abs() + reach,
        (vantage.eye.z - centre.1).abs() + reach,
    );
    let seen = mathf::fmin(corner, tallest / (SPANNED * mathf::fmax(stage.pixel, 1e-6)));
    // Crowns spaced at random no closer than the wood allows fill about
    // this share of the ground they could tile.
    let spacing = thickest * mathf::fmax(middling, 1e-3);
    let density = woodland.cover * PACKED / (PI * spacing * spacing);
    let covered = f64::from(woodland.most) / mathf::fmax(density, 1e-9);
    let round = PI * about * about;
    let filled = if covered <= round {
        mathf::sqrt(covered / PI)
    } else {
        mathf::sqrt((covered - round) / ACROSS + about * about)
    };
    let places = f64::from(stage.densities.woods.places);
    let sowing = |share: f64| {
        Sowing {
            sown: sown(share),
            about: mathf::fmin(about, filled),
            far: mathf::fmin(seen, filled),
        }
        .fitted(places)
    };
    let (canopy, subcanopy) = (sowing(CANOPY_SOWN), sowing(SUBCANOPY_SOWN));
    // Both stand one by one as far as the closer sown of them fits.
    let (about, near) = (
        mathf::fmin(canopy.about, subcanopy.about),
        mathf::fmin(canopy.far, subcanopy.far),
    );
    Sowings {
        canopy: Sowing {
            about,
            far: near,
            ..canopy
        },
        subcanopy: Sowing {
            about,
            far: near,
            ..subcanopy
        },
        seen,
    }
}

/// How a wood's canopy and its subcanopy are sown, and how far off its trees
/// are still seen.
#[derive(Copy, Clone, Debug)]
struct Sowings {
    canopy: Sowing,
    subcanopy: Sowing,
    seen: f64,
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
    /// The wood's canopy is standing, its subcanopy to be sown as `beneath`
    /// has it once it stands; its patches were laid out under the seed what
    /// grows beneath it keeps to.
    Trees {
        standing: Standing,
        beneath: Sowing,
        patches: u32,
    },
    /// The wood's subcanopy is standing beneath its canopy.
    Subcanopy { standing: Standing, patches: u32 },
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
            Phase::Trees { standing, .. } => 0.4 * standing.done(),
            Phase::Subcanopy { standing, .. } => 0.4 + 0.15 * standing.done(),
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
                beneath,
                patches,
            } => {
                if standing.step(stage, (land, runner), (&wood.vantage, None))? {
                    if let Some(far) = standing.beyond.take() {
                        stage.carry_on(far)?;
                    }
                    subcanopy(stage, standing, (wood, beneath), patches)?
                } else {
                    Phase::Trees {
                        standing,
                        beneath,
                        patches,
                    }
                }
            }
            Phase::Subcanopy {
                mut standing,
                patches,
            } => {
                if standing.step(stage, (land, runner), (&wood.vantage, None))? {
                    roof(stage, wood, patches)?
                } else {
                    Phase::Subcanopy { standing, patches }
                }
            }
            Phase::Under {
                mut casting,
                patches,
            } => {
                if casting.step(&stage.canopies, runner)? {
                    beneath(stage, dice, (land, wood), (casting.finish(), patches))?
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

/// `wood`'s trees sown over `land`: the places they might stand, to be
/// thinned and stood step by step, and carried on far off across the view
/// where the detail asks for it.
fn sow(stage: &Stage, dice: &mut Dice, land: &Land, wood: &Wood) -> Option<Phase> {
    let reader = Reader {
        woodland: wood.woodland,
        rooting: wood.rooting,
        kinds: wood.grove.habits(),
        seeds: (dice.seed(), dice.seed()),
        layer: Layer::Canopy,
    };
    let vantage = &wood.vantage;
    let Sowings {
        canopy: sowing,
        subcanopy,
        seen,
    } = sown_for(stage, land, &reader, vantage);
    let sown = lattice(&reader, sowing.sown);
    let eye = (vantage.eye.x, vantage.eye.z);
    // Shrubs are low enough to be stood one by one as far as they are seen.
    let trees = wood.grove.kinds().all(|grown| !grown.kind.shrub());
    let beyond = if stage.densities.woods.beyond && trees && seen > sowing.far {
        let stretch = Stretch {
            eye,
            heading: vantage.heading,
            across: ACROSS,
            to: seen,
        };
        FarWood::new((reader, sown), (land.grids, &stage.fields), stretch)
    } else {
        None
    };
    let region = Region {
        eye,
        heading: vantage.heading,
        across: ACROSS,
        about: sowing.about,
        near: sowing.far,
        far: if beyond.is_some() { seen } else { sowing.far },
        traced: land.grids.traced(&stage.fields),
    };
    let most = usize::try_from(reader.woodland.most).unwrap_or(usize::MAX);
    let sheltering = Some(reader.widest_room());
    Some(Phase::Trees {
        standing: Standing::new(
            stage,
            (&reader, most),
            (sown, region),
            (beyond, None, sheltering),
        )?,
        beneath: subcanopy,
        patches: reader.seeds.0,
    })
}

/// The subcanopy of `wood`, sown as `sowing` has it, to stand beneath the
/// canopy `canopy` stood: as far about the eye as every canopy tree that
/// could crowd one of its trees stands one by one, and only as many as the
/// wood's most trees and the stage leave room for; the shade the wood casts
/// next, where they leave none.
fn subcanopy(
    stage: &Stage,
    canopy: Standing,
    (wood, sowing): (&Wood, Sowing),
    patches: u32,
) -> Option<Phase> {
    let reader = Reader {
        layer: Layer::Subcanopy,
        ..canopy.reader
    };
    let (region, stood) = (canopy.region, canopy.stood);
    let (above, canopied) = canopy.sheltered()?;
    let room = above.room;
    // Short of the view's edges by as much as a canopy tree's room, so none
    // that could crowd a tree beneath stands outside the canopy stood.
    let narrowed = mathf::asin(mathf::fmin(
        room / mathf::fmax(region.about - room, room),
        1.0,
    ));
    let canopied = mathf::fmax(canopied, 0.0);
    let region = Region {
        across: mathf::fmax(region.across - narrowed, 0.0),
        about: mathf::fmin(sowing.about, canopied),
        near: mathf::fmin(sowing.far, canopied),
        far: mathf::fmin(sowing.far, canopied),
        ..region
    };
    let most = usize::try_from(wood.woodland.most)
        .unwrap_or(usize::MAX)
        .saturating_sub(stood);
    let standing = Standing::new(
        stage,
        (&reader, most),
        (lattice(&reader, sowing.sown), region),
        (None, Some(above), None),
    )?;
    if standing.most == 0 {
        return roof(stage, wood, patches);
    }
    Some(Phase::Subcanopy { standing, patches })
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
    (land, wood): (&Land, &Wood),
    (shade, patches): (Shade, u32),
) -> Option<Phase> {
    let Some(beneath) = &wood.beneath else {
        return Some(Phase::Deadfall { patches });
    };
    if shade.is_open() {
        return Some(Phase::Deadfall { patches });
    }
    let sowing = Sowing {
        sown: 100.0 / mathf::sqrt(mathf::fmax(beneath.plants, 1.0)),
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
        layer: Layer::Floor,
    };
    let vantage = &wood.vantage;
    let region = Region {
        eye: (vantage.eye.x, vantage.eye.z),
        heading: vantage.heading,
        across: ACROSS,
        about: sowing.about,
        near: sowing.far,
        far: sowing.far,
        traced: land.grids.traced(&stage.fields),
    };
    let most = usize::try_from(reader.woodland.most).unwrap_or(usize::MAX);
    let standing = Standing::new(
        stage,
        (&reader, most),
        (lattice(&reader, sowing.sown), region),
        (None, None, None),
    )?;
    if standing.most == 0 {
        return Some(Phase::Deadfall { patches });
    }
    Some(Phase::Beneath {
        standing,
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
    let tallest = snags.tallest_grown();
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
