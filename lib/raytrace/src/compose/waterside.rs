//! The plants of a scene's water's edge set out about the eye: reeds and
//! reedmace standing in the shallows and along wet, level banks, water
//! lilies and pondweed floating where the water lies still, water-crowfoot
//! streaming where it runs — each only where the land's water and the light
//! the woods leave let it grow, rooting in the silt and sand the water laid
//! rather than ground its floods scour bare, and in patches with gaps
//! between.
//!
//! Each cell of two lattices about the eye holds a square patch of one plant
//! or none, a clump in the fine cells near the eye and a bed in the coarse
//! ones beyond, laid square to the lattice so neighbouring patches meet. The
//! better a place suits its plant the taller and thicker its patch, so a bed
//! thins and shortens toward its edges rather than ending in a wall. What a
//! cell holds is drawn from its place and the light there alone, so a seed
//! shows the same water's edge however its work is divided.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2};

use tairix_parallel::JobRunner;
use tairix_util::mathf;

use super::lattice::{snap, Lattice};
use super::woodland::KEPT;
use super::{rgb, Dice, Recipe, Stage};
use crate::heightfield::Heightfield;
use crate::land::Land;
use crate::leaf::Outline;
use crate::lily::Lily;
use crate::material::{Finish, Material, Relief};
use crate::noise::{fbm2, hash2, smoothstep};
use crate::pigment::{Foliage, Pigment};
use crate::sample::{mix32, unit};
use crate::shade::Shades;
use crate::shape::Shape;
use crate::tree::Season;
use crate::vector::{real, share, Frame, Pose, Vec3};
use crate::waterside::{Margin, Marsh};

/// The plants of the water's edge.
const KINDS: [Margin; 5] = [
    Margin::Reed,
    Margin::Reedmace,
    Margin::Lily,
    Margin::Pondweed,
    Margin::Crowfoot,
];
/// How many clumps and beds of each are grown to choose among, the clumps
/// first.
const CLUMPS: usize = 6;
const BEDS: usize = 3;
const PATCHES: usize = CLUMPS + BEDS;
/// The statures a plant's patches are grown at, from the shortest and
/// thinnest, where a place barely suits it, to where it thrives; and how far
/// either way of what its place gives it a patch's is drawn.
const STATURE: (f64, f64) = (0.55, 1.1);
const SCATTER: f64 = 0.3;
/// The near and the far lattice's cells, and how many near cells span the
/// near lattice: a far cell five near ones, the near lattice whole far cells.
const NEAR_CELL: f64 = 0.75;
const FAR_CELL: f64 = 5.0 * NEAR_CELL;
const NEAR_CELLS: usize = 120;
const _: () = assert!(NEAR_CELLS.is_multiple_of(10));
/// Rows of each lattice a core reads in a unit, and patches set out in one.
const NEAR_ROWS: usize = 16;
const FAR_ROWS: usize = 6;
const PLACED_A_UNIT: usize = 8192;
/// How far apart along the water its fall is measured.
const FALL_REACH: f64 = 2.0;
/// How steeply a lily pad's fine quilting between its veins stands, and how
/// many of its swellings span a metre; and likewise a reedmace spike's
/// velvet.
const PAD_QUILT: (f64, f64) = (0.03, 260.0);
const VELVET: (f64, f64) = (0.08, 600.0);
/// How near the eye no patch stands, which one there would fill the picture.
const EYE_CLEAR: f64 = 2.5;
/// The most a bank may rise above the water beside it and still be read
/// against that water.
const BANK: f64 = 0.5;

/// One plant as it grows in the scene's season: its marsh, and its patches
/// as clumps and then as beds, each planned as a prototype only once a patch
/// of it is set out, so a plant the scene's water never suits costs it
/// nothing.
#[derive(Copy, Clone, Debug)]
struct Sown {
    margin: Margin,
    marsh: Marsh,
    season: Season,
    patches: [Patch; PATCHES],
    planned: [Option<u32>; PATCHES],
}

/// One of a plant's patches as it is drawn: the cell it fills, how many
/// plants stand in it and how tall, and its seed.
#[derive(Copy, Clone, Debug)]
struct Patch {
    side: f64,
    count: u16,
    stature: f64,
    seed: u64,
}

impl Sown {
    /// The prototype of patch `patch`, if it is planned yet.
    fn planned(&self, patch: usize) -> Option<u32> {
        self.planned.get(patch).copied().flatten()
    }

    /// Plan patch `patch` on `stage`: its prototype, or `None` when the stage
    /// will not hold it.
    fn plan(&mut self, stage: &mut Stage, patch: usize) -> Option<u32> {
        let Patch {
            side,
            count,
            stature,
            seed,
        } = *self.patches.get(patch)?;
        let prototype = stage.plan(&Recipe::Margin {
            margin: self.margin,
            side,
            count,
            stature,
            marsh: self.marsh,
            season: self.season,
            seed,
        })?;
        *self.planned.get_mut(patch)? = Some(prototype);
        Some(prototype)
    }

    /// The material its patches are added in.
    fn material(&self) -> usize {
        usize::from(self.marsh.leaves)
    }
}

/// The plants of a scene's water's edge, to be set out once its woods'
/// shade is cast.
#[derive(Debug)]
pub(super) struct Margins {
    /// What grows in the scene's season, by kind.
    sown: [Option<Sown>; KINDS.len()],
    /// The level of the fresh lake the land's sea stands for, if it does.
    lake: Option<f64>,
    seed: u32,
    eye: (f64, f64),
    /// How far about the eye they are set out, and the most patches.
    reach: f64,
    most: u32,
    /// The patches the lattices hold, gathered as their rows are read, then
    /// kept to the nearest the eye.
    found: Vec<Placed>,
    pass: Pass,
}

/// How far a scene's water's edge is set out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Pass {
    /// Lattice `index`'s rows from `row` are to be read.
    Reading {
        index: usize,
        row: usize,
    },
    /// Every lattice is read; the patches found are to be kept to the most
    /// the scene holds, the nearest the eye.
    Keeping,
    /// The patches kept from `next` on are to be set out.
    Placing {
        next: usize,
    },
    Done,
}

/// A patch of a plant a cell holds: its kind and which of its patches,
/// where its middle stands, how it is turned, and its key.
#[derive(Copy, Clone, Debug)]
struct Placed {
    kind: u8,
    patch: u8,
    base: Vec3,
    turn: f64,
    key: u32,
}

/// What a place by the water is like: the ground there and the water's
/// level, how deep it stands, negative on the bank above it, how steeply its
/// surface falls, the ground's wetness, uprightness and the way across it,
/// what the water laid there and how much grows there, and the share of the
/// sky no crown hides from it.
#[derive(Copy, Clone, Debug)]
struct Edge {
    ground: f64,
    level: f64,
    depth: f64,
    fall: f64,
    wet: f64,
    upright: f64,
    way: f64,
    laid: f64,
    green: f64,
    lit: f64,
}

/// Ask `stage` for the plants of its water's edge about `eye` in `season`,
/// set out once its woods' shade is cast; `lake` the level of a fresh lake
/// its land's sea stands for, if it does. One draw of `dice` keys them, the
/// same at either detail. `None` when the stage will not hold them.
pub(super) fn margins(
    stage: &mut Stage,
    dice: &mut Dice,
    (eye, season, lake): ((f64, f64), Season, Option<f64>),
) -> Option<()> {
    let mut dice = Dice::keyed(dice.wide(), 0);
    let mut sown = [None; KINDS.len()];
    for (slot, margin) in sown.iter_mut().zip(KINDS) {
        if !grows(margin, season) {
            continue;
        }
        let marsh = marsh(stage, margin, season)?;
        let mut patches = [Patch {
            side: 0.0,
            count: 0,
            stature: 0.0,
            seed: 0,
        }; PATCHES];
        for (index, patch) in patches.iter_mut().enumerate() {
            let (side, (variant, variants)) = if index < CLUMPS {
                (NEAR_CELL, (index, CLUMPS))
            } else {
                (FAR_CELL, (index - CLUMPS, BEDS))
            };
            let vigour = share(variant, variants - 1);
            let count =
                mathf::round_i32((0.4 + 0.6 * vigour) * density(margin) * side * side).max(1);
            *patch = Patch {
                side,
                count: u16::try_from(count).ok()?,
                stature: STATURE.0 + (STATURE.1 - STATURE.0) * vigour,
                seed: dice.wide(),
            };
        }
        *slot = Some(Sown {
            margin,
            marsh,
            season,
            patches,
            planned: [None; PATCHES],
        });
    }
    let (reach, most) = (
        stage.densities.waterside.reach,
        stage.densities.waterside.most,
    );
    stage.margins = Some(Margins {
        sown,
        lake,
        seed: dice.seed(),
        eye,
        reach,
        most,
        found: Vec::new(),
        pass: Pass::Reading { index: 0, row: 0 },
    });
    Some(())
}

impl Margins {
    /// Whether they are all set out.
    pub(super) fn finished(&self) -> bool {
        self.pass == Pass::Done
    }

    /// How far they are set out: their lattices' rows read, most of it, and
    /// then the patches kept set out.
    pub(super) fn done(&self) -> f64 {
        let rows = |index: usize| self.lattice(index).map_or(0, |lattice| lattice.side);
        let read = |index: usize, row: usize| (0..index).map(rows).sum::<usize>() + row;
        let reading = 0.9 * share(read(2, 0), rows(0) + rows(1));
        match self.pass {
            Pass::Reading { index, row } => 0.9 * share(read(index, row), rows(0) + rows(1)),
            Pass::Keeping => reading,
            Pass::Placing { next } => reading + 0.1 * share(next, self.found.len()),
            Pass::Done => 1.0,
        }
    }

    /// Set out the next unit of the water's edge about the eye on `land`,
    /// in the light `shades` leave it, its cells read across `runner`;
    /// `None` when the heap will not hold it.
    pub(super) fn step(
        &mut self,
        stage: &mut Stage,
        (land, shades, runner): (&Land, Option<&Shades>, &dyn JobRunner),
    ) -> Option<()> {
        match self.pass {
            Pass::Reading { index, row } => self.read(stage, (land, shades, runner), (index, row)),
            Pass::Keeping => {
                self.keep();
                Some(())
            }
            Pass::Placing { next } => self.place(stage, next),
            Pass::Done => Some(()),
        }
    }

    /// Read the next unit of lattice `index`'s rows from `row` on `land`,
    /// gathering the patches its cells hold clear of `stage`'s pieces.
    fn read(
        &mut self,
        stage: &Stage,
        (land, shades, runner): (&Land, Option<&Shades>, &dyn JobRunner),
        (index, row): (usize, usize),
    ) -> Option<()> {
        let Some(lattice) = self.lattice(index) else {
            self.pass = Pass::Keeping;
            return Some(());
        };
        let per = if index == 0 { NEAR_ROWS } else { FAR_ROWS };
        let end = (row + per * runner.width().max(1)).min(lattice.side);
        let cells = lattice.read(row..end, runner, &|cell| {
            self.grows_in((land, stage, shades), &lattice, cell)
        })?;
        // Grown by doubling, as it is extended a unit at a time.
        self.found
            .try_reserve(cells.iter().flatten().count())
            .ok()?;
        self.found.extend(cells.into_iter().flatten());
        self.pass = if end < lattice.side {
            Pass::Reading { index, row: end }
        } else if index == 0 {
            Pass::Reading { index: 1, row: 0 }
        } else {
            Pass::Keeping
        };
        Some(())
    }

    /// Keep the patches found to the most the scene holds, nearest the eye
    /// first, so where the most cuts them short the water's edge ends at a
    /// distance rather than wherever the reading had come to.
    fn keep(&mut self) {
        let eye = self.eye;
        let nearer = |a: &Placed, b: &Placed| {
            let apart = |placed: &Placed| {
                let (dx, dz) = (placed.base.x - eye.0, placed.base.z - eye.1);
                dx * dx + dz * dz
            };
            apart(a)
                .total_cmp(&apart(b))
                .then(a.base.x.total_cmp(&b.base.x))
                .then(a.base.z.total_cmp(&b.base.z))
        };
        let most = usize::try_from(self.most).unwrap_or(usize::MAX);
        if self.found.len() > most {
            if let Some(last) = most.checked_sub(1) {
                self.found.select_nth_unstable_by(last, nearer);
            }
            self.found.truncate(most);
        }
        self.found.sort_unstable_by(nearer);
        self.pass = Pass::Placing { next: 0 };
    }

    /// Set out the next unit of the patches kept from `next` on, as far as
    /// the stage has room for them.
    fn place(&mut self, stage: &mut Stage, next: usize) -> Option<()> {
        let end = (next + PLACED_A_UNIT).min(self.found.len());
        let (found, sown) = (&self.found, &mut self.sown);
        for placed in found.get(next..end).unwrap_or(&[]) {
            if stage.room() <= KEPT {
                self.found = Vec::new();
                self.pass = Pass::Done;
                return Some(());
            }
            let sown = sown.get_mut(usize::from(placed.kind))?.as_mut()?;
            let patch = usize::from(placed.patch);
            let prototype = match sown.planned(patch) {
                Some(prototype) => prototype,
                None if stage.plannable() > 0 => sown.plan(stage, patch)?,
                // Once the stage plans no more, a patch never planned is
                // left out.
                None => continue,
            };
            let pose = Pose::new(placed.base, Frame::turned(placed.turn, 0.0));
            stage.add(
                Shape::Instance {
                    prototype,
                    pose,
                    scale: 1.0,
                    key: placed.key,
                },
                sown.material(),
                pose,
                false,
            )?;
        }
        if end < self.found.len() {
            self.pass = Pass::Placing { next: end };
        } else {
            self.found = Vec::new();
            self.pass = Pass::Done;
        }
        Some(())
    }

    /// Lattice `index`: the near one, then the far one about it; `None`
    /// past them.
    fn lattice(&self, index: usize) -> Option<Lattice> {
        // Squared to the far lattice, so its cells and the near ones lie on
        // the land's own grid of places whatever the eye.
        let (x, z) = (snap(self.eye.0, FAR_CELL), snap(self.eye.1, FAR_CELL));
        let near = 0.5 * real(NEAR_CELLS) * NEAR_CELL;
        match index {
            0 => Some(Lattice::new((x - near, z - near), NEAR_CELLS, NEAR_CELL)),
            1 => Some(
                Lattice::about(self.eye, self.reach, FAR_CELL)?
                    .without(((x - near, z - near), (x + near, z + near))),
            ),
            _ => None,
        }
    }

    /// Whether patch `patch` of the plant in `slot`, at `(x, z)`, stands
    /// clear of every piece on `stage` — a boulder, a trunk, a pier.
    fn clears_pieces(
        &self,
        stage: &Stage,
        (slot, patch): (usize, usize),
        (x, z): (f64, f64),
    ) -> bool {
        let side = self
            .sown
            .get(slot)
            .and_then(Option::as_ref)
            .and_then(|sown| sown.patches.get(patch))
            .map(|patch| patch.side);
        // Its plants root anywhere in its square, out to the corners.
        side.is_some_and(|side| stage.clear_of_pieces((x, z), FRAC_1_SQRT_2 * side))
    }

    /// The patch cell `(column, row)` of `lattice` holds on `land`, in the
    /// light `shades` leave it, if any: the plant the place suits best among
    /// those of the season, as likely as it suits, clear of `stage`'s pieces.
    fn grows_in(
        &self,
        (land, stage, shades): (&Land, &Stage, Option<&Shades>),
        lattice: &Lattice,
        (column, row): (usize, usize),
    ) -> Option<Placed> {
        let fields = &stage.fields;
        let (x, z) = lattice.middle((column, row))?;
        let (dx, dz) = (x - self.eye.0, z - self.eye.1);
        let apart = dx * dx + dz * dz;
        if apart > self.reach * self.reach || apart < EYE_CLEAR * EYE_CLEAR {
            return None;
        }
        let far = lattice.cell > NEAR_CELL;
        let key = hash2(
            lattice.place(x),
            lattice.place(z),
            self.seed ^ u32::from(far),
        );
        let edge = edge_at((land, fields, shades), (x, z), self.lake)?;
        let mut best: Option<(usize, f64)> = None;
        for (slot, (&margin, sown)) in KINDS.iter().zip(&self.sown).enumerate() {
            if sown.is_none() {
                continue;
            }
            let held = best.map_or(0.0, |(_, held)| held);
            // Its patches can only lower how well a plant suits the place.
            let suits = suits(margin, &edge, far);
            if suits <= held {
                continue;
            }
            let score = suits * patchy(margin, (x, z), self.seed);
            if score > held {
                best = Some((slot, score));
            }
        }
        let (slot, score) = best?;
        if unit(key) >= score {
            return None;
        }
        let (first, variants) = if far { (CLUMPS, BEDS) } else { (0, CLUMPS) };
        // The better the place suits it, the taller and thicker its patch,
        // so a bed thins and shortens toward its edges.
        let vigour = (score + SCATTER * (unit(mix32(key ^ 1)) - 0.5)).clamp(0.0, 1.0);
        let last = variants - 1;
        let pick = usize::try_from(mathf::round_i32(vigour * real(last)))
            .ok()?
            .min(last);
        if !self.clears_pieces(stage, (slot, first + pick), (x, z)) {
            return None;
        }
        let margin = KINDS.get(slot).copied()?;
        let floating = matches!(margin, Margin::Lily | Margin::Pondweed | Margin::Crowfoot);
        // Crowfoot streams down the current; quarter turns keep the rest's
        // patches square to the lattice.
        let turn = match margin {
            Margin::Crowfoot => land
                .rivers
                .nearest(x, z)
                .map(|near| mathf::atan2(near.toward.0, near.toward.1))?,
            _ => FRAC_PI_2 * f64::from(mix32(key ^ 2) & 3),
        };
        Some(Placed {
            kind: u8::try_from(slot).ok()?,
            patch: u8::try_from(first + pick).ok()?,
            base: Vec3::new(x, if floating { edge.level } else { edge.ground }, z),
            turn,
            key: mix32(key ^ 3),
        })
    }
}

/// What the place `(x, z)` of `land` is like by the water, in the light
/// `shades` leave it: the fresh water's, or else the fresh `lake`'s; `None`
/// where no water lies near enough to read it against.
fn edge_at(
    (land, fields, shades): (&Land, &[Heightfield], Option<&Shades>),
    (x, z): (f64, f64),
    lake: Option<f64>,
) -> Option<Edge> {
    // Most places lie well above any water: rule them out before reading
    // the rest of the ground.
    let ground = land.grids.height(fields, x, z);
    let river = land.grids.water_level(fields, x, z);
    let level = river.or(lake).filter(|&level| level - ground > -BANK)?;
    let lie = land.grids.lie(fields, x, z);
    Some(Edge {
        ground,
        level,
        depth: level - ground,
        fall: if river.is_some() {
            fall_of(land, fields, (x, z), level)
        } else {
            0.0
        },
        wet: lie.wet,
        upright: lie.upright,
        way: lie.road + lie.path,
        laid: lie.sediment,
        green: lie.green,
        lit: 1.0 - shades.map_or(0.0, |shades| shades.at(x, z).1),
    })
}

/// How steeply the fresh water's surface falls about `(x, z)`, where it
/// stands at `level`: from either side where both hold water, from the one
/// that does at a bank.
fn fall_of(land: &Land, fields: &[Heightfield], (x, z): (f64, f64), level: f64) -> f64 {
    let at = |dx: f64, dz: f64| land.grids.water_level(fields, x + dx, z + dz);
    let slope = |before: Option<f64>, after: Option<f64>| match (before, after) {
        (Some(before), Some(after)) => (after - before) / (2.0 * FALL_REACH),
        (Some(before), None) => (level - before) / FALL_REACH,
        (None, Some(after)) => (after - level) / FALL_REACH,
        (None, None) => 0.0,
    };
    mathf::hypot(
        slope(at(-FALL_REACH, 0.0), at(FALL_REACH, 0.0)),
        slope(at(0.0, -FALL_REACH), at(0.0, FALL_REACH)),
    )
}

/// How well `margin` grows at `edge`, `0.0..=1.0`: in water as deep as it
/// takes and running as fast as it can bear — the still-water plants in the
/// stillest, crowfoot only where it runs — in as much of the sky's light as
/// it wants, its roots in the silt or the gravel it favours; or, for those
/// rooting on one, on a wet, level bank off any way that the floods leave
/// growing — more level for a far bed, whose square spans more of the slope.
fn suits(margin: Margin, edge: &Edge, far: bool) -> f64 {
    let ((shallowest, rooted, deepest, drowned), (lull, race), (gloom, open)) = match margin {
        Margin::Reed => ((-0.45, -0.15, 0.6, 1.0), (0.0, 0.02), (0.35, 0.65)),
        Margin::Reedmace => ((-0.15, 0.0, 0.45, 0.75), (0.0, 0.008), (0.4, 0.7)),
        Margin::Lily => ((0.35, 0.6, 1.8, 2.6), (0.0, 0.0015), (0.45, 0.75)),
        Margin::Pondweed => ((0.2, 0.35, 1.3, 1.8), (0.0, 0.004), (0.25, 0.55)),
        Margin::Crowfoot => ((0.06, 0.15, 0.7, 1.1), (0.0015, 0.06), (0.35, 0.65)),
    };
    let deep = smoothstep(shallowest, rooted, edge.depth)
        * (1.0 - smoothstep(deepest, drowned, edge.depth));
    let running =
        smoothstep(0.5 * lull, lull, edge.fall) * (1.0 - smoothstep(0.5 * race, race, edge.fall));
    let light = smoothstep(gloom, open, edge.lit);
    let bank = if edge.depth < 0.0 {
        let (sloping, level) = if far { (0.985, 0.995) } else { (0.95, 0.975) };
        smoothstep(0.45, 0.75, edge.wet)
            * smoothstep(sloping, level, edge.upright)
            * (1.0 - edge.way.min(1.0))
            * smoothstep(0.1, 0.4, edge.green)
    } else {
        rooting(margin, edge.laid)
    };
    deep * running * light * bank
}

/// How well `margin` roots under water where what the water laid is
/// `laid`: crowfoot anchors in gravel, the rest in silt and sand, sparser
/// over bare gravel and hardly at all on bare rock.
fn rooting(margin: Margin, laid: f64) -> f64 {
    let bare = 1.0 - 0.9 * smoothstep(-0.25, -0.7, laid);
    let fine = smoothstep(-0.05, 0.35, laid);
    bare * match margin {
        Margin::Crowfoot => 1.0 - 0.6 * fine,
        _ => 0.35 + 0.65 * fine,
    }
}

/// Where `margin` gathers, `0.0..=1.0`: a field of its own beds and the
/// gaps between them, broad for reeds and close for lilies.
fn patchy(margin: Margin, (x, z): (f64, f64), seed: u32) -> f64 {
    let (scale, salt) = match margin {
        Margin::Reed => (26.0, 0x9d),
        Margin::Reedmace => (14.0, 0x3b),
        Margin::Lily => (11.0, 0xc1),
        Margin::Pondweed => (18.0, 0x57),
        Margin::Crowfoot => (7.0, 0x6d),
    };
    smoothstep(
        -0.2,
        0.35,
        fbm2(x / scale, z / scale, seed ^ salt, (3, 0.5, 2.0)),
    )
}

/// Whether `margin` stands in `season`: the floating and the streaming
/// plants die back over winter, and the reeds and reedmace stand on, dry.
fn grows(margin: Margin, season: Season) -> bool {
    season != Season::Winter || matches!(margin, Margin::Reed | Margin::Reedmace)
}

/// Plants of `margin` to a square metre of its patch: reeds thick-set,
/// reedmace in looser fans, lily pads, pondweed's rosettes, and crowfoot's
/// streaming plants.
fn density(margin: Margin) -> f64 {
    match margin {
        Margin::Reed => 55.0,
        Margin::Reedmace => 8.0,
        Margin::Lily => 9.0,
        Margin::Pondweed => 6.0,
        Margin::Crowfoot => 5.0,
    }
}

/// The materials `margin`'s patches are made in, as they are in `season`;
/// `None` when the stage will not hold them.
fn marsh(stage: &mut Stage, margin: Margin, season: Season) -> Option<Marsh> {
    let leafy =
        |colours: [u32; 4], outline: Outline| Pigment::Foliage(foliage(colours, outline, season));
    let palette = |colours: [[u32; 4]; 4]| colours[season_index(season)];
    Some(match margin {
        Margin::Reed => {
            let stems = made(stage, leafy(palette(REED_STEMS), STRAP), leaf(0.15))?;
            let leaves = made(stage, leafy(palette(REED_LEAVES), STRAP), leaf(0.3))?;
            let heads = made(
                stage,
                leafy(palette(PLUMES), Outline::Fascicle { count: 7 }),
                leaf(0.45),
            )?;
            Marsh {
                stems,
                leaves,
                sepals: heads,
                heads,
                hearts: heads,
            }
        }
        Margin::Reedmace => mace_marsh(
            stage,
            [
                leafy(palette(MACE_STEMS), STRAP),
                leafy(palette(MACE_LEAVES), STRAP),
                leafy(SPIKES, Outline::Lanceolate),
                leafy(FLUFF, Outline::Fascicle { count: 11 }),
            ],
        )?,
        Margin::Lily => lily_marsh(stage, palette(PADS))?,
        Margin::Pondweed => {
            let leaves = made(
                stage,
                leafy(palette(PONDWEED), Outline::Ovate { teeth: 0 }),
                Finish::Coated { roughness: 0.35 },
            )?;
            Marsh {
                stems: leaves,
                leaves,
                sepals: leaves,
                heads: leaves,
                hearts: leaves,
            }
        }
        Margin::Crowfoot => {
            let stems = made(stage, leafy(palette(CROWFOOT), STRAP), leaf(0.2))?;
            let leaves = made(
                stage,
                leafy(palette(CROWFOOT), STRAP),
                Finish::Coated { roughness: 0.4 },
            )?;
            let heads = made(
                stage,
                Pigment::Lily(Lily::Broad(PETALS.map(rgb))),
                leaf(0.5),
            )?;
            let hearts = made(
                stage,
                Pigment::Lily(Lily::Hearts(HEARTS.map(rgb))),
                leaf(0.3),
            )?;
            Marsh {
                stems,
                leaves,
                sepals: heads,
                heads,
                hearts,
            }
        }
    })
}

/// The materials reedmace's patches are made in, of its stems', leaves',
/// spikes' and fluff's pigments.
fn mace_marsh(stage: &mut Stage, [stems, leaves, spikes, fluff]: [Pigment; 4]) -> Option<Marsh> {
    let stems = made(stage, stems, leaf(0.15))?;
    let leaves = made(stage, leaves, leaf(0.25))?;
    // A spike's velvet is its seeds' hairs packed close.
    let heads = u16::try_from(stage.material(
        Material::new(spikes, Finish::Matte).with_relief(Relief::grain(VELVET.0, VELVET.1, 0x7e1)),
    )?)
    .ok()?;
    let fluff = made(stage, fluff, leaf(0.6))?;
    Some(Marsh {
        stems,
        leaves,
        sepals: fluff,
        heads,
        hearts: heads,
    })
}

/// The materials a water lily's patches are made in, its pads of `pads`.
fn lily_marsh(stage: &mut Stage, pads: [u32; 4]) -> Option<Marsh> {
    Some(Marsh {
        stems: made(stage, Pigment::Lily(Lily::Stalks), leaf(0.1))?,
        // A pad's waxed skin sheds the water and shines, quilted finely
        // between its veins.
        leaves: u16::try_from(
            stage.material(
                Material::new(
                    Pigment::Lily(Lily::Pads(pads.map(rgb))),
                    Finish::Coated { roughness: 0.15 },
                )
                .with_relief(Relief::grain(PAD_QUILT.0, PAD_QUILT.1, 0x1a7d)),
            )?,
        )
        .ok()?,
        sepals: made(stage, Pigment::Lily(Lily::Sepals), leaf(0.25))?,
        heads: made(
            stage,
            Pigment::Lily(Lily::Petals(PETALS.map(rgb))),
            leaf(0.5),
        )?,
        hearts: made(
            stage,
            Pigment::Lily(Lily::Hearts(HEARTS.map(rgb))),
            leaf(0.3),
        )?,
    })
}

/// A material of `pigment` and `finish` on `stage`, by the index a part
/// keeps; `None` when the stage will not hold it.
fn made(stage: &mut Stage, pigment: Pigment, finish: Finish) -> Option<u16> {
    u16::try_from(stage.material(Material::new(pigment, finish))?).ok()
}

/// A leaf's finish, letting `translucency` of the light through it.
const fn leaf(translucency: f64) -> Finish {
    Finish::Leaf { translucency }
}

/// A blade's outline, its whole length kept.
const STRAP: Outline = Outline::Strap { from: 0, to: 255 };

/// A plant's leaves of `colours` and `outline` as they are in `season`.
fn foliage(colours: [u32; 4], outline: Outline, season: Season) -> Foliage {
    let autumn = matches!(season, Season::Autumn { .. });
    Foliage {
        colours: colours.map(rgb),
        underside: 0.15,
        veins: 0.25,
        edge: rgb(0x6A_5A_30),
        browning: if autumn { 0.45 } else { 0.06 },
        spots: if autumn { 0.3 } else { 0.03 },
        snow: if season == Season::Winter { 0.4 } else { 0.0 },
        outline,
    }
}

/// Where `season`'s colours lie in a palette: spring, summer, autumn,
/// winter.
fn season_index(season: Season) -> usize {
    match season {
        Season::Spring => 0,
        Season::Summer => 1,
        Season::Autumn { .. } => 2,
        Season::Winter => 3,
    }
}

/// The plants' colours by season, spring to winter: a reed's stems green and
/// last year's straw in spring, gold in autumn and pale in winter; its
/// leaves; its plume, a dull mauve-brown as it flowers and buff once dry;
/// and likewise reedmace's blue-green leaves, a lily's pads — green each
/// season, as a pad's own age yellows and browns it — and pondweed.
const REED_STEMS: [[u32; 4]; 4] = [
    [0x7E_9A_44, 0xB8_A8_78, 0x86_A2_4E, 0xC4_B4_84],
    [0x6C_8A_3A, 0x78_94_4A, 0x5E_7C_34, 0x84_A0_52],
    [0xB4_9A_58, 0xA6_8A_4A, 0xC0_A8_66, 0x9A_80_48],
    [0xCD_BE_90, 0xC0_B0_80, 0xD6_C8_9E, 0xB4_A2_74],
];
const REED_LEAVES: [[u32; 4]; 4] = [
    [0x76_A0_40, 0x84_AC_4A, 0x6C_96_38, 0x8E_B4_56],
    [0x5E_80_34, 0x6A_8C_3C, 0x54_74_30, 0x76_98_48],
    [0xB8_A0_50, 0xA8_90_40, 0xC6_AE_60, 0x9E_82_38],
    [0xC8_B8_88, 0xBC_AA_78, 0xD2_C4_98, 0xAE_9C_6C],
];
const PLUMES: [[u32; 4]; 4] = [
    [0xA4_98_84, 0x9A_8C_78, 0xAE_A2_90, 0x90_82_70],
    [0x7E_68_66, 0x8C_74_70, 0x72_5E_5C, 0x96_80_7A],
    [0x8A_7A_6C, 0x9C_8A_7A, 0x7E_6E_60, 0xA8_96_84],
    [0xB8_AC_9C, 0xC2_B6_A6, 0xAE_A2_90, 0xCA_BE_AE],
];
const MACE_STEMS: [[u32; 4]; 4] = [
    [0x74_90_4E, 0x80_9A_58, 0x6A_86_46, 0x8A_A4_62],
    [0x66_80_46, 0x70_8A_4E, 0x5C_76_40, 0x7A_94_58],
    [0x9A_86_4C, 0x8A_78_42, 0xA8_94_58, 0x7C_6A_3A],
    [0xAE_9E_78, 0xA2_92_6C, 0xBA_AA_84, 0x96_86_60],
];
const MACE_LEAVES: [[u32; 4]; 4] = [
    [0x64_84_52, 0x70_90_5C, 0x5A_7A_4A, 0x7A_9A_66],
    [0x58_76_4A, 0x62_7E_50, 0x4E_6C_42, 0x6C_88_58],
    [0xA0_88_48, 0x8E_7A_3E, 0xB0_9A_58, 0x7E_6C_36],
    [0xB0_A0_7A, 0xA4_94_6E, 0xBC_AC_86, 0x98_88_62],
];
const PADS: [[u32; 4]; 4] = [
    [0x3E_6A_30, 0x46_72_34, 0x36_62_2C, 0x4E_7A_3A],
    [0x2C_54_26, 0x36_62_2C, 0x28_4A_22, 0x40_6E_34],
    [0x34_52_26, 0x3E_5A_2A, 0x2E_4A_22, 0x46_60_2E],
    [0x34_52_26, 0x3E_5A_2A, 0x2E_4A_22, 0x46_60_2E],
];
const PONDWEED: [[u32; 4]; 4] = [
    [0x66_7A_34, 0x72_86_3C, 0x5C_70_2E, 0x7C_90_44],
    [0x55_64_2A, 0x61_7032, 0x4C_5A_26, 0x6C_7C_3A],
    [0x8A_7A_38, 0x7E_70_34, 0x96_86_40, 0x72_66_30],
    [0x7A_6E_40, 0x6E_64_3A, 0x86_7A_48, 0x64_5A_34],
];
/// Water-crowfoot's dark green threads, darkening through the year.
const CROWFOOT: [[u32; 4]; 4] = [
    [0x3E_5C_22, 0x46_66_28, 0x36_52_1E, 0x4E_6E_2E],
    [0x34_50_1E, 0x3C_5A_22, 0x2E_48_1A, 0x44_62_28],
    [0x3A_48_1E, 0x44_52_22, 0x32_40_1A, 0x4C_5A_26],
    [0x2E_3C_1A, 0x36_44_1E, 0x28_3416, 0x3E_4C_22],
];

/// A reedmace's spikes and the fluff a winter's spikes burst in, a lily's
/// petals and the heart of its flower, the same each season.
const SPIKES: [u32; 4] = [0x4A_2E_1C, 0x54_34_1E, 0x40_28_18, 0x5E_3C_24];
const FLUFF: [u32; 4] = [0xE6_DE_CC, 0xDC_D2_BE, 0xEE_E8_DA, 0xD2_C8_B2];
const PETALS: [u32; 4] = [0xF2_F0_E6, 0xFA_F8_F0, 0xEA_E6_DA, 0xF6_EE_EC];
const HEARTS: [u32; 4] = [0xE2_B8_30, 0xEA_C4_40, 0xD8_AC_28, 0xF0_CC_4C];

#[cfg(test)]
#[path = "waterside_tests.rs"]
mod tests;
