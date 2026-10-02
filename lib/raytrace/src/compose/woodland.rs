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

use super::landscape::{self, Lawning, Vantage};
use super::plants::{self, Dead, Grove, Grown, Kind, Laying, DEAD_VARIANTS, VARIANTS};
use super::{Dice, Stage};
use crate::ground::Floor;
use crate::heightfield::Heightfield;
use crate::land::{Land, Lie};
use crate::noise::{cells2, fbm2, hash3, smoothstep};
use crate::prototype::single;
use crate::sample::{mix32, unit};
use crate::shade::{Casting, Shade, Shades, Shading, NEAR_CELL, ROOFED};
use crate::vector::{real, share, Frame, Pose, Vec3};

/// How a wood grows over a land.
#[derive(Copy, Clone, Debug)]
pub(super) struct Woodland {
    /// The share of the ground that suits trees the wood covers, and how
    /// broad its patches, and the open ground between them, are.
    pub(super) cover: f64,
    pub(super) patch: f64,
    /// How far apart trunks stand, as a share of their two crowns' reaches
    /// together, where the wood grows thickest and where it is most open.
    pub(super) closure: (f64, f64),
    /// How much of their kinds' grown height its youngest and oldest stands
    /// reach.
    pub(super) stature: (f64, f64),
    /// The share of the places a gap may open in the canopy where one has:
    /// where a tree or a stand of them fell, and light reaches the floor.
    pub(super) gaps: f64,
    /// The most trees it stands.
    pub(super) most: u32,
    /// How far ahead of the eye, in heights of a tree, and how far either
    /// side of the view in radians it keeps clear, so that no tree walls the
    /// view off.
    pub(super) open: (f64, f64),
}

/// The ground a wood takes to, beyond the somewhere to root, off roads and
/// paths and out of the water, that every tree needs.
#[derive(Copy, Clone, Debug)]
pub(super) struct Rooting {
    /// How upright the ground stands where trees begin taking to it, and
    /// where they take to it fully.
    pub(super) upright: (f64, f64),
    /// How strongly wet ground draws trees to it, out of the wood's patches
    /// as well as in them: willows along a stream across open fields.
    pub(super) streams: f64,
    /// How readily it roots where nothing grows green: under snow, where the
    /// land's green gives out though the trees stand on.
    pub(super) bare: f64,
    /// The heights over which it comes to take to the land, going up, and
    /// over which it gives out.
    pub(super) above: Option<(f64, f64)>,
    pub(super) below: Option<(f64, f64)>,
    /// A clearing it keeps out of: its middle, and how far it reaches.
    pub(super) clearing: Option<((f64, f64), f64)>,
}

/// Ground as level as most trees want it, anywhere on the land.
pub(super) const ANYWHERE: Rooting = Rooting {
    upright: (0.74, 0.88),
    streams: 0.0,
    bare: 0.0,
    above: None,
    below: None,
    clearing: None,
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
const ACROSS: f64 = 1.15;

/// How many pixels tall a tree far off still spans, at least.
const SPANNED: f64 = 2.0;

/// How far about the eye, in heights of its tallest trees, a wood stands all
/// the way round: as far as their shadows reach.
const SHADOWED: f64 = 8.0;

/// How much of the ground crowns thrown down at random, no closer than they
/// allow, come to fill: the jamming limit of random sequential adsorption.
const PACKED: f64 = 0.55;

/// How closely the places a tree might stand lie, as a share of how far
/// apart the wood's trees of middling height stand where it is thickest:
/// close enough that thinning, not the lattice, sets where they stand.
const SOWN: f64 = 0.45;
const LEAST_SOWN: f64 = 0.35;

/// How broad the stands a wood's ages and kinds come in are, in metres.
const STANDS: f64 = 140.0;

/// How much openness a wood's edge adds to the spacing of its trees.
const EDGE_OPENING: f64 = 0.4;

/// How broad the stretches a wood grows thick or open in are, as a share of
/// its patches' breadth.
const DENSITY_BREADTH: f64 = 0.45;

/// How far apart the places a gap may open in a canopy lie, and how broad
/// one is, least and most: a fallen tree's worth to a windthrown stand's.
const GAP_SPACING: f64 = 90.0;
const GAP_REACH: (f64, f64) = (8.0, 35.0);

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
const KEPT: usize = 512;

/// A place a tree might stand, and, once the ground there is read, the tree
/// that would.
#[derive(Copy, Clone, Debug)]
struct Seedling {
    at: (f64, f64),
    /// Its draws, hashed from its place in the lattice.
    draw: u32,
    tree: Option<Tree>,
}

/// A tree as the ground and the wood would grow it.
#[derive(Copy, Clone, Debug)]
struct Tree {
    /// Which of the grove's kinds, which of that kind's grown trees, and how
    /// much of that tree's size.
    kind: u8,
    variant: u8,
    scale: f64,
    height: f64,
    reach: f64,
    /// How far apart it stands from others, as a share of their crowns'
    /// reaches together.
    apart: f64,
    /// The height its trunk is based at.
    base: f64,
}

/// What reading the ground for a wood's trees needs, shared by every band of
/// places read at once.
struct Reading<'a> {
    land: &'a Land,
    fields: &'a [Heightfield],
    woodland: &'a Woodland,
    rooting: &'a Rooting,
    grove: &'a Grove,
    /// The shade the canopy above casts, where what grows beneath it is read.
    beneath: Option<&'a Shade>,
    /// The seeds the wood's patches, and its stands and kinds, are laid out
    /// under.
    seeds: (u32, u32),
}

impl Reading<'_> {
    /// Read the ground at `seedling`'s place for the tree it would grow.
    fn read(&self, seedling: &mut Seedling) {
        seedling.tree = self.tree(seedling.at, seedling.draw);
    }

    fn tree(&self, at: (f64, f64), draw: u32) -> Option<Tree> {
        let (patches, stands) = self.seeds;
        let chance = unit(draw);
        // The canopy's gaps are its own; what grows beneath takes to them.
        let open = match self.beneath {
            Some(_) => 0.0,
            None => gap(self.woodland, patches, at),
        };
        let in_patches = wooded(self.woodland, patches, at) * (1.0 - open);
        let streams = self.rooting.streams;
        if streams <= 0.0 && chance >= in_patches {
            return None;
        }
        let light = match self.beneath {
            Some(canopy) => thrives_beneath(canopy.at(at.0, at.1).1),
            None => 1.0,
        };
        if chance >= light {
            return None;
        }
        let lie = self.land.lie(self.fields, at.0, at.1);
        let wooded = in_patches.max(streams * lie.wet).min(1.0);
        let suits = wooded * light * self.rooting.suits(&lie, at);
        if chance >= suits || self.land.wet_at(self.fields, at.0, at.1) {
            return None;
        }
        let (kind, grown) = self.kind(&lie, at, draw)?;
        let tallest = grown.heights.iter().copied().fold(0.0, f64::max);
        let age = smoothstep(
            0.3,
            0.7,
            0.5 + 0.5 * fbm2(at.0 / STANDS, at.1 / STANDS, stands ^ 0x41, (3, 0.5, 2.0)),
        );
        let (least, most) = self.woodland.stature;
        // A wood's edge is lower than its heart, and good ground grows taller.
        let edge = 0.82 + 0.18 * smoothstep(0.35, 0.95, wooded);
        let wanted = tallest
            * (least + (most - least) * age)
            * rank(draw)
            * edge
            * (0.92 + 0.12 * lie.green);
        let variant = grown.nearest(wanted, unit(mix32(draw ^ 0x6a09_e667)));
        let natural = *grown.heights.get(variant)?;
        let scale = plants::sized(wanted, natural);
        let height = natural * scale;
        let (thickest, openest) = self.woodland.closure;
        let breadth = DENSITY_BREADTH * self.woodland.patch.max(1.0);
        let openness = 0.5
            + 0.5
                * fbm2(
                    at.0 / breadth,
                    at.1 / breadth,
                    patches ^ 0x77,
                    (3, 0.5, 2.0),
                );
        let apart = (thickest + (openest - thickest) * smoothstep(0.3, 0.7, openness))
            * (1.0 + EDGE_OPENING * (1.0 - wooded));
        Some(Tree {
            kind,
            variant: u8::try_from(variant).ok()?,
            scale,
            height,
            reach: grown.crown * height,
            apart,
            base: rooted(&lie, height),
        })
    }

    /// Which of the grove's kinds grows at `at`, on ground `lie` describes:
    /// the one that takes to it best, in the stands its kind grows in, and
    /// its index among them.
    fn kind(&self, lie: &Lie, at: (f64, f64), draw: u32) -> Option<(u8, &Grown)> {
        let mut best: Option<(f64, u8, &Grown)> = None;
        for (index, grown) in self.grove.kinds().enumerate() {
            let index = u8::try_from(index).ok()?;
            let salt = u32::from(index).wrapping_mul(0x9e37_79b9);
            let stand = 0.5
                + 0.5
                    * fbm2(
                        at.0 / STANDS,
                        at.1 / STANDS,
                        self.seeds.1 ^ 0x5eed ^ salt,
                        (2, 0.5, 2.0),
                    );
            let own = 0.75 + 0.5 * unit(mix32(draw ^ salt));
            let score = affinity(grown.kind, lie) * (0.3 + stand) * own;
            if best.is_none_or(|(most, _, _)| score > most) {
                best = Some((score, index, grown));
            }
        }
        best.map(|(_, index, grown)| (index, grown))
    }
}

impl Rooting {
    /// How well the ground `lie` describes at `at` suits the wood's trees,
    /// `0.0..=1.0`.
    pub(super) fn suits(&self, lie: &Lie, at: (f64, f64)) -> f64 {
        if let Some((middle, reach)) = self.clearing {
            if mathf::hypot(at.0 - middle.0, at.1 - middle.1) < reach {
                return 0.0;
            }
        }
        let above = self
            .above
            .map_or(1.0, |(low, high)| smoothstep(low, high, lie.height));
        let below = self
            .below
            .map_or(1.0, |(low, high)| 1.0 - smoothstep(low, high, lie.height));
        lie.green.max(self.bare)
            * smoothstep(self.upright.0, self.upright.1, lie.upright)
            * (1.0 - lie.road)
            * (1.0 - 0.95 * lie.path)
            * above
            * below
    }
}

/// How much of the ground `woodland` covers at `at`, `0.0..=1.0`: patches
/// `patch` across, covering the share of the land its `cover` asks for.
fn wooded(woodland: &Woodland, seed: u32, at: (f64, f64)) -> f64 {
    let patch = woodland.patch.max(1.0);
    let field = 0.5 + 0.5 * fbm2(at.0 / patch, at.1 / patch, seed, (4, 0.5, 2.0));
    // Four octaves of the plane's noise, halved and raised, fall about their
    // middle nearly as a logistic spread of 0.056 does: this threshold leaves
    // the share asked for above it.
    let cover = woodland.cover.clamp(0.02, 0.98);
    let threshold = 0.5 + 0.056 * mathf::ln((1.0 - cover) / cover);
    smoothstep(threshold - 0.02, threshold + 0.02, field)
}

/// How far into one of its canopy's gaps `at` lies, `0.0..=1.0`, for
/// `woodland` under `seed`: each lattice cell of the gaps holding one as its
/// `gaps` share has it, about the cell's jittered middle.
fn gap(woodland: &Woodland, seed: u32, at: (f64, f64)) -> f64 {
    if woodland.gaps <= 0.0 {
        return 0.0;
    }
    let found = cells2(at.0 / GAP_SPACING, at.1 / GAP_SPACING, seed ^ 0x6a95, 0.8);
    if unit(mix32(found.id)) >= woodland.gaps {
        return 0.0;
    }
    let reach = GAP_REACH.0 + (GAP_REACH.1 - GAP_REACH.0) * unit(mix32(found.id ^ 0x2545));
    1.0 - smoothstep(0.75 * reach, reach, found.nearest * GAP_SPACING)
}

/// How readily a shrub or a young tree grows under a canopy hiding `hidden`
/// of the sky, `0.0..=1.0`: least in the deepest shade, most in the gaps and
/// along the edges, and less again out in the open, where grass takes the
/// ground.
fn thrives_beneath(hidden: f64) -> f64 {
    let light = 1.0 - hidden;
    (0.03 + 0.97 * smoothstep(0.05, 0.3, light)) * (1.0 - 0.6 * smoothstep(0.7, 0.95, light))
}

/// The shares of a stand's trees overtopped by the canopy, and suppressed
/// beneath it: trees that came up late, or lost the race for the light, and
/// wait in the shade for a gap.
const OVERTOPPED: f64 = 0.25;
const SUPPRESSED: f64 = 0.15;

/// The share of its stand's height a tree as `draw` has it grows to: most
/// the canopy's, the overtopped and the suppressed lower.
fn rank(draw: u32) -> f64 {
    let (place, within) = (
        unit(mix32(draw ^ 0x1f83_d9ab)),
        unit(mix32(draw ^ 0x5be0_cd19)),
    );
    if place < SUPPRESSED {
        0.35 + 0.25 * within
    } else if place < SUPPRESSED + OVERTOPPED {
        0.6 + 0.25 * within
    } else {
        0.88 + 0.24 * within
    }
}

/// How readily `kind` takes to the ground `lie` describes against the other
/// kinds of its wood: willows and poplars to the wet, pines to the dry and
/// poor, beeches to deep and well-drained soil, birches wherever others give
/// way.
fn affinity(kind: Kind, lie: &Lie) -> f64 {
    let (wet, rich) = (lie.wet, lie.green);
    match kind {
        Kind::Willow | Kind::Poplar => 0.25 + 1.6 * wet,
        Kind::Pine => 0.7 + 0.6 * (1.0 - wet) + 0.4 * (1.0 - rich),
        Kind::Spruce => 0.8 + 0.5 * wet,
        Kind::Beech => (1.0 + 0.5 * rich - 1.2 * wet).max(0.1),
        Kind::Oak => (1.0 + 0.3 * rich - 0.6 * wet).max(0.1),
        Kind::Maple => 0.9 + 0.4 * rich,
        Kind::Birch => 0.8 + 0.5 * (1.0 - rich),
        Kind::Fern => 0.7 + 0.8 * wet,
        _ => 1.0,
    }
}

/// The height a trunk `height` tall is based at on ground `lie` describes:
/// sunk far enough that its flare meets the ground on its downhill side.
pub(super) fn rooted(lie: &Lie, height: f64) -> f64 {
    let slope = mathf::sqrt((1.0 - lie.upright * lie.upright).max(0.0)) / lie.upright.max(0.1);
    lie.height - 0.08 - 0.035 * height * slope
}

/// The room about a trunk `height` tall that other pieces keep clear of.
fn trunk(height: f64) -> f64 {
    0.25 + 0.025 * height
}

/// How many rings a wood's places are sown in.
fn rings(sowing: &Sowing) -> u32 {
    u32::try_from(mathf::round_i32(mathf::ceil(sowing.far / sowing.sown)).max(0)).unwrap_or(0)
}

/// Sow ring `ring` of the places a plant might stand seen from `vantage`,
/// sown as `sowing` has them: all the way round near the eye and across the
/// view beyond, within the square `(centre, reach)`, into `into`; how many
/// places the ring was drawn over, those beyond the square included, or
/// `None` when the heap will not hold them.
fn sow_ring(
    ring: u32,
    (vantage, sowing): (&Vantage, &Sowing),
    (centre, reach): ((f64, f64), f64),
    seed: u32,
    into: &mut Vec<Seedling>,
) -> Option<usize> {
    let Vantage { eye, heading } = *vantage;
    let Sowing { sown, about, .. } = *sowing;
    let (inner, outer) = (f64::from(ring) * sown, f64::from(ring + 1) * sown);
    let (from, span) = if inner < about {
        (0.0, TAU)
    } else {
        (heading - ACROSS, 2.0 * ACROSS)
    };
    let count = u32::try_from(mathf::round_i32(span * 0.5 * (inner + outer) / sown).max(1)).ok()?;
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
fn read_all(
    runner: &dyn JobRunner,
    seedlings: &mut [Seedling],
    reading: &Reading<'_>,
) -> Option<()> {
    let mut bands: Vec<&mut [Seedling]> =
        fallible::collected(seedlings.len().div_ceil(BAND), seedlings.chunks_mut(BAND))?;
    tairix_parallel::for_each(runner, &mut bands, &|band| {
        for seedling in band.iter_mut() {
            reading.read(seedling);
        }
    });
    Some(())
}

/// A tree stood, as its neighbours see it.
#[derive(Copy, Clone, Debug)]
struct Stood {
    at: (f64, f64),
    height: f64,
    reach: f64,
    apart: f64,
}

/// The trees stood so far, by the cells of a grid over the wood each trunk
/// stands in: a cell as broad as the most room two trees keep, so that any
/// tree too near another stands in a cell beside the other's.
#[derive(Debug)]
struct Crowns {
    least: (f64, f64),
    cell: f64,
    side: usize,
    heads: Vec<u32>,
    /// Each tree, and the next in its cell.
    trees: Vec<(Stood, u32)>,
}

/// No tree: an empty cell, or the last of one's chain.
const NONE: u32 = u32::MAX;

/// The most cells a side of the crowns' grid holds: past it, the cells grow
/// broader instead, holding more trees each.
const MOST_SIDE: i32 = 1024;

impl Crowns {
    fn new((centre, reach): ((f64, f64), f64), cell: f64) -> Option<Self> {
        let cell = cell.max(1.0);
        let side =
            usize::try_from(mathf::round_i32(mathf::ceil(2.0 * reach / cell)).clamp(1, MOST_SIDE))
                .ok()?;
        let cell = (2.0 * reach / real(side)).max(cell);
        Some(Self {
            least: (centre.0 - reach, centre.1 - reach),
            cell,
            side,
            heads: fallible::filled(side * side, NONE)?,
            trees: Vec::new(),
        })
    }

    /// The cell `at` stands in, as its column and row.
    fn cell_of(&self, at: (f64, f64)) -> (usize, usize) {
        let place = |value: f64, least: f64| {
            let whole = mathf::round_i32(mathf::floor((value - least) / self.cell));
            usize::try_from(whole.max(0))
                .unwrap_or(0)
                .min(self.side - 1)
        };
        (place(at.0, self.least.0), place(at.1, self.least.1))
    }

    /// Whether `tree` would crowd any tree stood: its trunk nearer one's
    /// than their crowns allow, a much shorter tree standing under a taller
    /// one's crown more readily than beside a peer's.
    fn crowds(&self, tree: &Stood) -> bool {
        let (column, row) = self.cell_of(tree.at);
        let rows = row.saturating_sub(1)..=(row + 1).min(self.side - 1);
        rows.flat_map(|row| {
            let columns = column.saturating_sub(1)..=(column + 1).min(self.side - 1);
            columns.map(move |column| row * self.side + column)
        })
        .any(|cell| {
            let mut next = self.heads.get(cell).copied().unwrap_or(NONE);
            while let Some(&(other, after)) = self.trees.get(next as usize) {
                next = after;
                let (short, tall) = if tree.height < other.height {
                    (tree, &other)
                } else {
                    (&other, tree)
                };
                let layered = LAYERED
                    + (1.0 - LAYERED)
                        * smoothstep(UNDER.0, UNDER.1, short.height / tall.height.max(1e-3));
                let room = (tree.apart.max(other.apart) * (tree.reach + other.reach) * layered)
                    .max(trunk(tree.height) + trunk(other.height) + 0.5);
                if mathf::hypot(tree.at.0 - other.at.0, tree.at.1 - other.at.1) < room {
                    return true;
                }
            }
            false
        })
    }

    fn add(&mut self, tree: Stood) -> Option<()> {
        let (column, row) = self.cell_of(tree.at);
        let head = self.heads.get_mut(row * self.side + column)?;
        let index = u32::try_from(self.trees.len()).ok()?;
        self.trees.try_reserve(1).ok()?;
        self.trees.push((tree, *head));
        *head = index;
        Some(())
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

/// A wood's plants being stood a step at a time: the places they might stand
/// sown and the ground there read a band at a time, then those that would
/// grow thinned tallest first, a batch at a time.
#[derive(Debug)]
struct Standing {
    sowing: Sowing,
    /// The next ring to sow, and how many there are.
    ring: u32,
    rings: u32,
    /// The places sown in this band, read, their room kept for the next.
    band: Vec<Seedling>,
    /// The places read that would grow, in the order they were sown.
    grown: Runs<Seedling>,
    /// Those places, tallest first.
    ranking: Ranking,
    crowns: Crowns,
    stood: u32,
    most: u32,
    /// The seeds the wood's patches, and its stands and lattice, are laid
    /// out under.
    seeds: (u32, u32),
}

/// How many places a step reads at least, across the runner, and how many it
/// thins at most, one after another since each tree stood shapes where the
/// next may.
const READ_UNIT: usize = 1 << 14;
const THIN_UNIT: usize = 1 << 10;

impl Standing {
    /// `grove`'s plants to be stood as `woodland` grows them, seen from
    /// `vantage` and sown as `sowing` has it, under `seeds`; `None` when the
    /// heap will not hold the grid their crowns are kept apart by.
    fn new(
        stage: &Stage,
        (grove, woodland): (&Grove, &Woodland),
        (vantage, sowing): (&Vantage, Sowing),
        seeds: (u32, u32),
    ) -> Option<Self> {
        let tallest = grove
            .kinds()
            .map(|grown| plants::TALLEST * grown.heights.iter().copied().fold(0.0, f64::max))
            .fold(0.0, f64::max);
        let widest = grove.kinds().map(|grown| grown.crown).fold(0.0, f64::max) * tallest;
        let crowned =
            woodland.closure.1.max(woodland.closure.0) * (1.0 + EDGE_OPENING) * 2.0 * widest;
        let most_room = crowned.max(2.0 * trunk(tallest) + 0.5);
        Some(Self {
            sowing,
            ring: 0,
            rings: rings(&sowing),
            band: Vec::new(),
            grown: Runs::default(),
            ranking: Ranking::default(),
            crowns: Crowns::new(((vantage.eye.x, vantage.eye.z), sowing.far), most_room)?,
            stood: 0,
            most: woodland
                .most
                .min(u32::try_from(stage.room().saturating_sub(KEPT)).unwrap_or(u32::MAX)),
            seeds,
        })
    }

    /// How far the standing has come: its rings read, its places ranked,
    /// then thinned.
    fn done(&self) -> f64 {
        let read = share(self.ring as usize, self.rings as usize);
        let thinned = if self.stood >= self.most {
            1.0
        } else {
            self.ranking.taken()
        };
        0.6 * read + 0.05 * self.ranking.sorted() + 0.35 * thinned
    }

    /// The next step of standing `wood`'s plants — its trees, or those of
    /// `beneath` in `shade` — seen from `vantage`: whether all are stood, or
    /// `None` when the heap will not hold them.
    fn step(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
        (grove, woodland, rooting): (&Grove, &Woodland, &Rooting),
        (vantage, shade): (&Vantage, Option<&Shade>),
    ) -> Option<bool> {
        if self.ring < self.rings {
            self.band.clear();
            // Bounded by the places drawn, not those kept: across the view
            // from near the land's edge most of a far ring lies off it.
            let mut drawn = 0;
            while self.ring < self.rings && drawn < READ_UNIT {
                drawn += sow_ring(
                    self.ring,
                    (vantage, &self.sowing),
                    (land.centre, land.reach),
                    self.seeds.1,
                    &mut self.band,
                )?;
                self.ring += 1;
            }
            let reading = Reading {
                land,
                fields: &stage.fields,
                woodland,
                rooting,
                grove,
                beneath: shade,
                seeds: self.seeds,
            };
            read_all(runner, &mut self.band, &reading)?;
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
            if walls_off(woodland.open, vantage, at, tree.height)
                || !stage.clear(at, trunk(tree.height))
                || self.crowns.crowds(&standing)
            {
                continue;
            }
            self.crowns.add(standing)?;
            let grown = grove.kinds().nth(usize::from(tree.kind))?;
            let base = Vec3::new(at.0, tree.base, at.1);
            let turn = TAU * unit(mix32(draw ^ 0x510e_527f));
            plants::place(
                stage,
                grown,
                (usize::from(tree.variant), tree.scale),
                base,
                (turn, mix32(draw ^ 0x9b05_688c)),
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
struct Runs<T> {
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
    fn len(&self) -> usize {
        self.len
    }

    /// Room for `count` more; `false` when the heap will not hold it.
    fn reserve(&mut self, count: usize) -> bool {
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
    fn push(&mut self, item: T) {
        if let Some(run) = self.runs.get_mut(self.len / RUN) {
            run.push(item);
            self.len += 1;
        }
    }

    fn get(&self, index: usize) -> Option<&T> {
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

/// How a wood's trees are sown: closely enough for its middling trees where
/// it grows thickest; all the way about the eye as far as their shadows
/// reach, and across the view beyond out to the land's edge, as far as its
/// tallest still span a pixel or two, or as far as its most trees would
/// stand thickly enough to fill.
fn sown_for(
    stage: &Stage,
    land: &Land,
    (grove, woodland): (&Grove, &Woodland),
    vantage: &Vantage,
) -> Sowing {
    let (count, middling, tallest) =
        grove
            .kinds()
            .fold((0.0, 0.0, 0.0f64), |(count, total, tallest), grown| {
                let typical = grown.heights.iter().sum::<f64>() / real(VARIANTS);
                let top = plants::TALLEST * grown.heights.iter().copied().fold(0.0, f64::max);
                (count + 1.0, total + grown.crown * typical, tallest.max(top))
            });
    let middling = middling / f64::max(count, 1.0);
    let thickest = woodland.closure.0;
    let sown = (SOWN * thickest * 2.0 * middling * woodland.stature.0).max(LEAST_SOWN);
    let about = ABOUT.min(SHADOWED * tallest);
    let corner = mathf::hypot(
        (vantage.eye.x - land.centre.0).abs() + land.reach,
        (vantage.eye.z - land.centre.1).abs() + land.reach,
    );
    let seen = tallest / (SPANNED * stage.pixel.max(1e-6));
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
    Sowing {
        sown,
        about: about.min(filled),
        far: corner.min(seen).min(filled),
    }
}

/// Whether a tree `height` tall at `at` would wall off the view from
/// `vantage`: standing nearer than `open.0` of its heights ahead of the eye
/// and within `open.1` either side of the way it looks.
fn walls_off(open: (f64, f64), vantage: &Vantage, at: (f64, f64), height: f64) -> bool {
    let (dx, dz) = (at.0 - vantage.eye.x, at.1 - vantage.eye.z);
    let turn = mathf::atan2(dx, dz) - vantage.heading;
    let turn = turn - TAU * mathf::floor((turn + PI) / TAU);
    mathf::hypot(dx, dz) < open.0 * height && turn.abs() < open.1
}

/// A scene's woods and sward, grown once its land stands: each wood's trees
/// and then what grows beneath them, a bounded step at a time, then the
/// sward under them all, knowing their shade.
#[derive(Debug)]
pub(super) struct Growing {
    woods: Vec<Wood>,
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
    /// Every wood stands, its shade cast over the land for the sward being
    /// laid in it.
    Laying { shades: Shades, laying: Laying },
}

impl Growing {
    /// The woods and sward `stage` has been asked for, taken from it, their
    /// draws keyed from one of `dice`; `None` if it was asked for neither.
    pub(super) fn from(stage: &mut Stage, dice: &mut Dice) -> Option<Self> {
        let woods = core::mem::take(&mut stage.woods);
        let sward = stage.sward.take();
        if woods.is_empty() && sward.is_none() {
            return None;
        }
        let seed = dice.wide();
        Some(Self {
            woods,
            sward,
            next: 0,
            phase: Phase::Sowing,
            seed,
            dice: Dice::keyed(seed, 0),
        })
    }

    /// How far the growing has come: each wood a share, and the sward laid
    /// beneath them all one more.
    pub(super) fn done(&self) -> f64 {
        // The last part is the shade, and the sward laid in it where there
        // is one.
        let shading = if self.sward.is_some() { 0.25 } else { 1.0 };
        let within = match &self.phase {
            Phase::Sowing => 0.0,
            Phase::Trees { standing, .. } => 0.55 * standing.done(),
            Phase::Under { casting, .. } => 0.55 + 0.05 * casting.done(),
            Phase::Beneath { standing, .. } => 0.6 + 0.3 * standing.done(),
            Phase::Deadfall { .. } => 0.9,
            Phase::Shading { shading: casting } => shading * casting.done(),
            Phase::Laying { laying, .. } => 0.25 + 0.75 * laying.done(),
        };
        let parts = self.woods.len() + 1;
        share(self.next.min(self.woods.len()), parts) + within / real(parts)
    }

    /// Grow the next step on `land`; whether everything is grown, or `None`
    /// when the heap will not hold it.
    pub(super) fn step(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
    ) -> Option<bool> {
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
                let trees = (&wood.grove, &wood.woodland, &wood.rooting);
                if standing.step(stage, (land, runner), trees, (&wood.vantage, None))? {
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
                let beneath = wood.beneath.as_ref()?;
                let understory = understory(&wood.woodland);
                let plants = (&beneath.grove, &understory, &wood.rooting);
                if standing.step(stage, (land, runner), plants, (&wood.vantage, Some(&shade)))? {
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
            Phase::Shading { .. } | Phase::Laying { .. } => return None,
        };
        self.phase = next;
        Some(false)
    }

    /// The next step once every wood stands, out of `phase`: the woods'
    /// shade cast over the land a unit at a time, and then the sward laid in
    /// it; whether all is laid.
    fn lay(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
        phase: Phase,
    ) -> Option<bool> {
        let dice = &mut self.dice;
        // The shade is cast about the sward's eye, or the first wood's where
        // nothing grows beneath them: the crowns roof the air and strew the
        // ground with what they shed either way.
        let eye = match (&self.sward, self.woods.first()) {
            (Some(lawning), _) => lawning.eye,
            (None, Some(wood)) => (wood.vantage.eye.x, wood.vantage.eye.z),
            (None, None) => return Some(true),
        };
        let shades = match phase {
            Phase::Laying { shades, mut laying } => {
                if !laying.step(stage, dice, (&shades, runner))? {
                    self.phase = Phase::Laying { shades, laying };
                    return Some(false);
                }
                shades
            }
            Phase::Shading { mut shading } => {
                if !shading.step(&stage.canopies, runner)? {
                    self.phase = Phase::Shading { shading };
                    return Some(false);
                }
                let shades = shading.finish();
                if let Some(lawning) = &self.sward {
                    let laying = landscape::sward(stage, dice, land, lawning)?;
                    self.phase = Phase::Laying { shades, laying };
                    return Some(false);
                }
                shades
            }
            _ => {
                let shading = Shading::new(&stage.canopies, (land.centre, land.reach), eye)?;
                self.phase = Phase::Shading { shading };
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
    let seeds = (dice.seed(), dice.seed());
    let sowing = sown_for(stage, land, (&wood.grove, &wood.woodland), &wood.vantage);
    let standing = Standing::new(
        stage,
        (&wood.grove, &wood.woodland),
        (&wood.vantage, sowing),
        seeds,
    )?;
    Some(Phase::Trees {
        standing,
        patches: seeds.0,
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
    let sowing = Sowing {
        sown: 100.0 / mathf::sqrt(beneath.plants.max(1.0)),
        about: ABOUT_BENEATH,
        far: BENEATH,
    };
    // Over a lattice of its own.
    let seeds = (patches, dice.seed());
    let understory = understory(&wood.woodland);
    let standing = Standing::new(
        stage,
        (&beneath.grove, &understory),
        (&wood.vantage, sowing),
        seeds,
    )?;
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
            let lie = land.lie(&stage.fields, at.0, at.1);
            if lie.upright < 0.8
                || lie.road > 0.0
                || lie.path > 0.3
                || wood.rooting.suits(&lie, at) < 0.2
                || land.wet_at(&stage.fields, at.0, at.1)
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
        land.height(&stage.fields, at.0, at.1),
        land.height(&stage.fields, tip.0, tip.1),
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
    let snags = &dead.snags;
    let tallest = snags.heights.iter().copied().fold(0.0, f64::max);
    let wanted = tallest * dice.range(0.5, 1.0);
    let variant = snags.nearest(wanted, dice.unit());
    let scale = plants::sized(wanted, *snags.heights.get(variant)?);
    let height = scale * *snags.heights.get(variant)?;
    if mathf::hypot(at.0 - eye.0, at.1 - eye.1) <= DEADFALL_CLEAR || !stage.clear(at, trunk(height))
    {
        return Some(false);
    }
    let base = Vec3::new(at.0, rooted(lie, height), at.1);
    plants::place(
        stage,
        snags,
        (variant, scale),
        base,
        (dice.range(0.0, TAU), dice.seed()),
    )?;
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
