//! Ground cover: a sward of grass, the weeds among it, and the leaves fallen
//! on the ground, a few to every cell of a grid over the land.
//!
//! A ray walks the cells it crosses in order, only over the stretch where it
//! is low enough to reach anything, and tests what each cell holds; what a
//! cell holds stands wholly within it, so the nearest thing of the first cell
//! met is the nearest of all. Nothing is stored: everything is hashed from its
//! cell. How much grows in a cell follows the land beneath it — nothing on a
//! road or bare rock, little on a trodden path, most where the ground is
//! green.
//!
//! Grass grows as a sward does: several kinds of it, each in the patches the
//! ground favours it in; in swathes rank and thin a few strides across; and
//! within them in tussocks whose leaves fountain out from their middles,
//! thinner ground between where weeds take hold. Its leaves curve and twist,
//! and some shoots are stems carrying seed heads. A sward is laid in lawns of
//! coarser cells the farther they lie from the eye, each leaving to a finer
//! one the ground that one covers; where a leaf is finer than a pixel, leaves
//! merge into fewer, broader ones covering the ground as they did, and far
//! off the sward fades into the ground's own colour of it.
//!
//! Its leaves are too many to shadow one another one by one; instead the
//! light reaching a point within the sward is thinned by the blades above it,
//! as light through any canopy is (Monsi and Saeki, 1953), so its base lies in
//! its own shade.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::heightfield::{Heightfield, ABSENT};
use crate::land::decode_lane;
use crate::leaf::Outline;
use crate::noise::{cell, cells2, hash2, noise2, smoothstep};
use crate::sample::{mix32, mix64, unit};
use crate::shade::Shade;
use crate::shape::{reciprocal, Aabb, Geometry, Hit};
use crate::vector::{Ray, Vec3};

/// The most cells a ray walks across one cover: past any a lawn holds along
/// a ray, so only a walk gone wrong ever meets it.
const MAX_CELLS: u32 = 1 << 16;
/// Cells a walk passes over the sward's top before it looks ahead for where
/// it next comes down into it.
const OVER_BEFORE_SKIP: u32 = 4;

/// The bit of a hit's mark set when it is a flower, not a blade.
pub(crate) const FLOWER: u32 = 1 << 31;
/// The bit set when it is a weed's leaf.
pub(crate) const WEED: u32 = 1 << 30;
/// The bit set when it is a fallen leaf.
pub(crate) const LITTER: u32 = 1 << 29;
/// The bit set when it is a stem's seed head.
pub(crate) const HEAD: u32 = 1 << 28;
/// Where a grass shoot's mark carries how rank the sward it grew in is, in
/// steps of a fifteenth, and which of the sward's kinds it is; below them,
/// its own key.
const VIGOUR_SHIFT: u32 = 24;
const VIGOUR_STEPS: u32 = 15;
const KIND_SHIFT: u32 = 22;
const KEY: u32 = (1 << KIND_SHIFT) - 1;

/// The most kinds of grass one sward holds, as the bits a mark carries its
/// kind in.
const KIND_BITS: u32 = 2;
const KIND_MASK: u32 = (1 << KIND_BITS) - 1;
pub(crate) const GRASS_KINDS: usize = 1 << KIND_BITS;

// A mark's key, its kind of grass, its vigour and what it is never share a
// bit, and every kind of grass has a place in it.
const _: () = {
    let kinds = FLOWER | WEED | LITTER | HEAD;
    let kind = KIND_MASK << KIND_SHIFT;
    let vigour = VIGOUR_STEPS << VIGOUR_SHIFT;
    assert!(KEY & (kinds | kind | vigour) == 0);
    assert!(kind & (kinds | vigour) == 0 && vigour & kinds == 0);
    assert!(KIND_SHIFT + KIND_BITS <= VIGOUR_SHIFT);
};

/// The widest a flower's head spreads from its stalk's tip.
const FLOWER_ROOM: f64 = 0.03;

/// The most a weed's leaves rise from the ground, as a share of their length.
const ROSETTE_RISE: f64 = 0.45;

/// How far apart tussocks stand, how far across a swathe of rank or thin
/// grass runs, and how far across a patch one kind of grass holds.
const TUSSOCK: f64 = 0.42;
const SWATHE: f64 = 9.0;
const PATCH: f64 = 14.0;
/// The tallest the sward grows a shoot, over its kind's own tallest.
const RANKEST: f64 = 1.3;
/// The tallest a stem stands over the leaves about it, and how high over its
/// own height a shoot's bow carries it.
const STEM_RISE: f64 = 1.7;
const BOW: f64 = 1.1;
/// The most shoots a cell of the rankest tussock holds, over its share.
const THICKEST: f64 = 2.4;
/// How wide on screen, in pixels, a leaf merged from finer ones stands; and
/// the most of a cell's breadth one may take, so it stays within its cell.
const MERGED: f64 = 0.7;
const WIDEST: f64 = 0.25;
/// How much broader than its stem a spike stands, and a plume spreads.
const SPIKE: f64 = 3.2;
const PLUME: f64 = 7.0;

/// The least a cell must thrive to grow anything, and the most points a
/// side a lawn is looked over at for whether anything grows on it.
const THRIVES: f64 = 0.02;
const SURVEY: u32 = 256;

/// How much of a leaf's height by its width it covers, tapering to its tip.
const BLADE_FILL: f64 = 0.6;
/// The lowest rise a way through a sward is taken at: nearer level, light
/// crosses so many blades that none gets through.
const GRAZING: f64 = 0.02;

/// Gauss–Legendre quadrature over `0..1`, eight points and their weights:
/// what light from the whole sky through a sward is averaged at.
const QUADRATURE: [(f64, f64); 8] = [
    (0.019_855_071_751_231_9, 0.050_614_268_145_188_1),
    (0.101_666_761_293_186_6, 0.111_190_517_226_687_2),
    (0.237_233_795_041_835_5, 0.156_853_322_938_943_6),
    (0.408_282_678_752_175_1, 0.181_341_891_689_181),
    (0.591_717_321_247_825, 0.181_341_891_689_181),
    (0.762_766_204_958_164_5, 0.156_853_322_938_943_6),
    (0.898_333_238_706_813_4, 0.111_190_517_226_687_2),
    (0.980_144_928_248_768_1, 0.050_614_268_145_188_1),
];

/// A cover over the ground.
#[derive(Clone, Debug)]
pub(crate) struct Lawn {
    /// The scene's height grid it lies on.
    pub(crate) field: u32,
    /// The corners of the rectangle it covers, in x and z.
    pub(crate) from: (f64, f64),
    pub(crate) to: (f64, f64),
    /// A rectangle within it that a finer lawn of the same sward covers.
    pub(crate) hole: Option<((f64, f64), (f64, f64))>,
    /// The lowest and highest the ground lies under it.
    pub(crate) floor: f64,
    pub(crate) ceiling: f64,
    /// The side of a cell.
    pub(crate) cell: f64,
    pub(crate) cover: Cover,
    /// How much of the sky the trees over it hide, where they stand over it.
    pub(crate) shade: Option<Shade>,
    /// What the sward's kinds, swathes and tussocks are drawn under: shared
    /// by every lawn of one sward, so its lawns meet without a seam and its
    /// weeds find its gaps.
    pub(crate) sward: u32,
    pub(crate) seed: u32,
    /// Where it is seen from, and how it fades with distance from there.
    pub(crate) seen: Seen,
    /// The grid of how high the sward's tallest shoots stand over each block
    /// of its cells, which a ray skips the air above the sward by; `None`
    /// for a cover low enough that its reach bounds it closely.
    pub(crate) tops: Option<Tops>,
}

/// A lawn's canopy grid: the scene's grid it is, and how many cells a side
/// each of its vertices stands for — one, and a vertex also keeps how its
/// cell grows, so a ray need not work it out again.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Tops {
    pub(crate) field: u32,
    pub(crate) block: u32,
}

/// Where a cover is seen from: the eye's place over the land; how wide a
/// pixel is a metre from it, once the camera stands; and the distances from
/// it over which the cover fades into the ground's own colour of it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Seen {
    pub(crate) eye: (f64, f64),
    pub(crate) pixel: f64,
    pub(crate) fade: (f64, f64),
}

impl Seen {
    /// From `eye`, fading between `fade`; the pixel not yet known.
    pub(crate) const fn from(eye: (f64, f64), fade: (f64, f64)) -> Self {
        Self {
            eye,
            pixel: 0.0,
            fade,
        }
    }
}

/// What a cover is.
#[allow(
    clippy::large_enum_variant,
    reason = "a lawn holds one, and a sward a handful of lawns"
)]
#[derive(Copy, Clone, Debug)]
pub(crate) enum Cover {
    Grass(Grass),
    Weeds(Weeds),
    Litter(Litter),
}

/// A sward: the kinds of grass it holds; how many shoots stand to a square
/// metre where it grows best; and the share of them ending in a wildflower.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Grass {
    pub(crate) kinds: [Option<GrassKind>; GRASS_KINDS],
    pub(crate) shoots: f64,
    pub(crate) flowers: f64,
}

/// A kind of grass.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct GrassKind {
    /// Its leaves' least and greatest height, their width at the root, how
    /// far a leaf's tip bows out over its height, and how far back down it
    /// droops, as a share of its height.
    pub(crate) height: (f64, f64),
    pub(crate) width: f64,
    pub(crate) lean: f64,
    pub(crate) droop: f64,
    /// How thickly it stands, over the sward's own.
    pub(crate) thickness: f64,
    /// How much it grows in tussocks: `0.0` for an even sod, `1.0` for clumps
    /// with bare ground between.
    pub(crate) tufted: f64,
    /// The share of its shoots that are stems, and the seed heads they carry.
    pub(crate) stems: f64,
    pub(crate) head: Head,
    /// The ground it takes to.
    pub(crate) habit: Habit,
    /// How much of the sward it holds, against its other kinds.
    pub(crate) share: f64,
}

/// A grass's seed head.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Head {
    /// A slender spike along the stem's top: rye, timothy.
    Spike,
    /// A loose plume spreading from it: bent, hair-grass, reed.
    Plume,
}

impl Head {
    /// How far across it spreads, as a share of its stem's width, and in how
    /// many sprays across one another.
    const fn spread(self) -> (f64, u32) {
        match self {
            Self::Spike => (SPIKE, 1),
            Self::Plume => (PLUME, 2),
        }
    }
}

/// The ground a kind of grass takes to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Habit {
    /// Anywhere grass grows.
    Open,
    /// Wet ground, where it crowds the others out: rushes, tussock grass.
    Wet,
    /// Ground trodden or grazed short, where only it bears the wear.
    Trodden,
    /// Dry, thin ground: bents and fescues on a bank.
    Dry,
}

/// Weeds: the share of cells a rosette grows in where they grow best, and
/// the fewest and most leaves a rosette spreads.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Weeds {
    pub(crate) share: f64,
    pub(crate) leaves: (u32, u32),
}

/// Fallen leaves: the most lying in a cell, the shortest and longest, the
/// outline of the tree they fell from, and how long ago the most of them
/// fell, from `0.0` for this autumn's to `1.0` for last year's, rotted.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Litter {
    pub(crate) most: u32,
    pub(crate) length: (f64, f64),
    pub(crate) outline: Outline,
    pub(crate) age: f64,
}

/// How grass grows about a point: how rank the swathe it lies in is and how
/// much of it is a tussock's rather than the thin ground between, both
/// `0.0..=1.0`; the level way out from its tussock's middle; and how far out
/// toward the tussock's rim it lies.
#[derive(Copy, Clone, Debug)]
struct Sward {
    swathe: f64,
    clump: f64,
    out: (f64, f64),
    splay: f64,
}

impl Sward {
    /// How thickly, and how tall, a kind `tufted` as it is grows here, as
    /// shares of its own.
    fn growth(&self, tufted: f64) -> (f64, f64) {
        let clumped = 0.25 + 2.0 * self.clump;
        let thickness = (0.45 + 1.1 * self.swathe) * (1.0 + (clumped - 1.0) * tufted);
        let stature = (0.6 + 0.55 * self.swathe) * (0.75 + 0.35 * self.clump);
        (thickness, stature.min(RANKEST))
    }

    fn vigour(&self) -> f64 {
        (0.6 * self.swathe + 0.4 * self.clump).clamp(0.0, 1.0)
    }
}

/// The blades of a sward about a point within it: how much blade a cubic
/// metre holds, one side of each, and how far the sward rises above the
/// point and it above the ground.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Canopy {
    density: f64,
    up: f64,
    down: f64,
}

impl Canopy {
    /// How much of the light bound for the point along the unit `dir` the
    /// blades let through: upright, they stop low light more than light from
    /// overhead, and light from below crosses only those between the point
    /// and the ground.
    pub(crate) fn through(&self, dir: Vec3) -> f64 {
        let rise = dir.y.abs().max(GRAZING);
        let depth = if dir.y >= 0.0 { self.up } else { self.down };
        let projected = 0.35 + 0.3 * (1.0 - rise);
        mathf::exp(-projected * self.density * depth / rise)
    }

    /// The share of the light from the whole sky, weighed by its cosine
    /// about the vertical, that the blades let through.
    pub(crate) fn diffuse(&self) -> f64 {
        QUADRATURE
            .iter()
            .map(|&(rise, weight)| {
                let dir = Vec3::new(mathf::sqrt(1.0 - rise * rise), rise, 0.0);
                2.0 * rise * weight * self.through(dir)
            })
            .sum()
    }
}

/// How one cell of a sward grows: the kind of grass it holds, how many
/// shoots before its own draw rounds them, how tall over its kind's own, how
/// many finer shoots each of its shoots stands for, the mark bits its shoots
/// carry, and the way out from its tussock's middle with how far out toward
/// the rim it lies.
#[derive(Copy, Clone, Debug)]
struct Stand {
    kind: GrassKind,
    shoots: f64,
    stature: f64,
    merged: f64,
    marks: u32,
    out: (f64, f64),
    splay: f64,
}

/// Steps a cached stand's splay is kept in: coarser than it varies across a
/// cell, which it is taken at the middle of.
const SPLAY_STEPS: u32 = 3;

/// One shoot of grass, from its cell's first corner: where it roots; the
/// level way its tip bows toward, and how far out; how high it stands; its
/// width at the root, and the most it spreads either side of its line; how
/// far its tip droops back; how far its blade turns about itself root to tip;
/// the seed head a stem carries and the wildflower a leaf may; and its key.
#[derive(Copy, Clone, Debug)]
struct Placed {
    root: (f64, f64),
    toward: (f64, f64),
    reach: f64,
    height: f64,
    width: f64,
    breadth: f64,
    droop: f64,
    twist: f64,
    head: Option<Head>,
    flower: Option<f64>,
    key: u32,
}

/// A shoot rooted in the world: its root there and the rest as placed.
#[derive(Copy, Clone, Debug)]
struct Shoot {
    root: Vec3,
    placed: Placed,
}

/// The ground under a cell: its middle, its height there, and its slope
/// along x and z.
#[derive(Copy, Clone, Debug)]
struct Plane {
    middle: (f64, f64),
    height: f64,
    slope: (f64, f64),
    /// The highest the ground stands across the cell.
    highest: f64,
}

impl Plane {
    fn at(&self, (x, z): (f64, f64)) -> f64 {
        let height =
            self.height + self.slope.0 * (x - self.middle.0) + self.slope.1 * (z - self.middle.1);
        height.min(self.highest)
    }

    fn normal(&self) -> Vec3 {
        Vec3::new(-self.slope.0, 1.0, -self.slope.1).normalized()
    }
}

impl Lawn {
    pub(crate) fn bounds(&self) -> Aabb {
        Aabb {
            min: Vec3::new(self.from.0, self.floor - 0.01, self.from.1),
            max: Vec3::new(self.to.0, self.ceiling + self.reach(), self.to.1),
        }
    }

    /// The highest anything of the cover stands above the ground.
    fn reach(&self) -> f64 {
        match self.cover {
            Cover::Grass(grass) => {
                let tallest = grass
                    .kinds
                    .iter()
                    .flatten()
                    .map(|kind| kind.height.1)
                    .fold(0.0, f64::max);
                tallest * RANKEST * STEM_RISE * BOW + FLOWER_ROOM
            }
            Cover::Weeds(_) => ROSETTE_RISE * 0.45 * self.cell + 0.01,
            Cover::Litter(_) => 0.05,
        }
    }

    /// The cell `(x, z)` lies in, and its middle.
    fn cell_of(&self, (x, z): (f64, f64)) -> ((u32, u32), (f64, f64)) {
        let ((cx, _), (cz, _)) = (
            cell((x - self.from.0) / self.cell),
            cell((z - self.from.1) / self.cell),
        );
        let middle = (
            self.from.0 + (f64::from(cx) + 0.5) * self.cell,
            self.from.1 + (f64::from(cz) + 0.5) * self.cell,
        );
        ((cx, cz), middle)
    }

    /// How the sward grows about `(x, z)`.
    fn sward(&self, (x, z): (f64, f64)) -> Sward {
        let swathe = 0.5 + 0.5 * noise2(x / SWATHE, z / SWATHE, self.sward ^ 0x5a);
        let found = cells2(x / TUSSOCK, z / TUSSOCK, self.sward ^ 0x7a, 0.9);
        let hearty = 0.55 + 0.45 * unit(found.id);
        let clump = (1.0 - smoothstep(0.2, 0.55, found.nearest)) * hearty;
        let away = (-found.toward.x, -found.toward.z);
        let distance = mathf::hypot(away.0, away.1);
        Sward {
            swathe,
            clump,
            out: if distance > 1e-9 {
                (away.0 / distance, away.1 / distance)
            } else {
                (1.0, 0.0)
            },
            splay: smoothstep(0.05, 0.45, found.nearest),
        }
    }

    /// Which of `grass`'s kinds holds the cell about `middle`, as wet and
    /// trodden as the land has it there: each in patches of its own, the
    /// likelier the more of the sward it is and the better the ground suits
    /// it, their borders ragged a cell at a time.
    fn kind(
        &self,
        grass: &Grass,
        (middle, cell_key): ((f64, f64), u32),
        (wet, path): (f64, f64),
    ) -> Option<(u32, GrassKind)> {
        let mut best: Option<(f64, u32, GrassKind)> = None;
        for (index, kind) in (0u32..).zip(grass.kinds.iter()) {
            let Some(kind) = kind else {
                continue;
            };
            let suits = match kind.habit {
                Habit::Open => 1.0 - 0.5 * smoothstep(0.4, 0.9, wet),
                Habit::Wet => 3.0 * smoothstep(0.35, 0.8, wet),
                Habit::Trodden => 0.35 + 3.0 * path,
                Habit::Dry => 1.2 * (1.0 - smoothstep(0.2, 0.6, wet)),
            };
            let salt = self.sward ^ 0x3c1 ^ index.wrapping_mul(0x9e37_79b9);
            let favoured = 0.5 + 0.5 * noise2(middle.0 / PATCH, middle.1 / PATCH, salt);
            let ragged = 0.8 + 0.4 * unit(mix32(cell_key ^ 0x4b ^ index));
            let weight = kind.share * suits * (0.15 + favoured * favoured) * ragged;
            if best.is_none_or(|(held, _, _)| weight > held) {
                best = Some((weight, index, *kind));
            }
        }
        best.map(|(_, index, kind)| (index, kind))
    }

    /// How the cell `(cx, cz)` about `middle` grows over `field`, worked out
    /// afresh; `None` where no grass does.
    fn grown(
        &self,
        grass: &Grass,
        field: &Heightfield,
        ((cx, cz), middle): ((u32, u32), (f64, f64)),
    ) -> Option<Stand> {
        let thrives = self.thrives(field, middle);
        if thrives <= THRIVES {
            return None;
        }
        let [wet, _, lane, _] = field.attributes_at(middle.0, middle.1);
        let (_, path) = decode_lane(lane);
        let (index, kind) = self.kind(grass, (middle, hash2(cx, cz, self.seed)), (wet, path))?;
        let sward = self.sward(middle);
        let (thickness, stature) = sward.growth(kind.tufted);
        let share = grass.shoots * kind.thickness * self.cell * self.cell;
        let vigour =
            u32::try_from(mathf::round_i32(sward.vigour() * f64::from(VIGOUR_STEPS))).unwrap_or(0);
        Some(Stand {
            kind,
            shoots: (share * thrives * thickness).min(share * THICKEST),
            stature: (0.5 + 0.5 * thrives) * stature,
            merged: self.merged(middle, kind.width),
            marks: marks(index, vigour),
            out: sward.out,
            splay: sward.splay,
        })
    }

    /// How the cell `(cx, cz)` about `middle` grows: as its canopy grid keeps
    /// it where the grid keeps a cell a vertex, else worked out afresh.
    fn stand(
        &self,
        grass: &Grass,
        (field, tops): (&Heightfield, Option<&Heightfield>),
        ((cx, cz), middle): ((u32, u32), (f64, f64)),
    ) -> Option<Stand> {
        match (tops, self.tops) {
            (Some(tops), Some(Tops { block: 1, .. })) => {
                let bytes = tops.attributes_of(cx as usize + 1, cz as usize + 1);
                self.unpack(grass, bytes, middle)
            }
            _ => self.grown(grass, field, ((cx, cz), middle)),
        }
    }

    /// The most shoots a cell of `kind` holds.
    fn most(&self, grass: &Grass, kind: &GrassKind) -> f64 {
        grass.shoots * kind.thickness * self.cell * self.cell * THICKEST
    }

    /// `stand`, in the four bytes a canopy grid's vertex keeps: its kind,
    /// vigour and splay; its shoots, as a share of the most its kind holds;
    /// its stature, as a share of the rankest; and the way out from its
    /// tussock, in 256ths of a turn. No shoots at all packs as none.
    fn pack(&self, grass: &Grass, stand: &Stand) -> [u8; 4] {
        let byte = |share: f64| {
            u8::try_from(mathf::round_i32(255.0 * share.clamp(0.0, 1.0))).unwrap_or(u8::MAX)
        };
        let kind = (stand.marks >> KIND_SHIFT) & KIND_MASK;
        let vigour = (stand.marks >> VIGOUR_SHIFT) & VIGOUR_STEPS;
        let splay = u32::try_from(mathf::round_i32(stand.splay * f64::from(SPLAY_STEPS)))
            .unwrap_or(0)
            .min(SPLAY_STEPS);
        let turn = mathf::atan2(stand.out.1, stand.out.0) / TAU;
        let heading = mathf::round_i32(256.0 * (turn - mathf::floor(turn)));
        [
            u8::try_from(kind | (vigour << 2) | (splay << 6)).unwrap_or(0),
            byte(stand.shoots / self.most(grass, &stand.kind).max(1e-9)),
            byte(stand.stature / RANKEST),
            u8::try_from(heading & 0xff).unwrap_or(0),
        ]
    }

    /// The stand the four bytes `packed` keep for the cell about `middle`.
    fn unpack(&self, grass: &Grass, packed: [u8; 4], middle: (f64, f64)) -> Option<Stand> {
        let [head, shoots, stature, heading] = packed;
        if shoots == 0 {
            return None;
        }
        let index = u32::from(head & 3);
        let kind = grass.kinds.get(index as usize).copied().flatten()?;
        let angle = TAU * f64::from(heading) / 256.0;
        Some(Stand {
            kind,
            shoots: f64::from(shoots) / 255.0 * self.most(grass, &kind),
            stature: f64::from(stature) / 255.0 * RANKEST,
            merged: self.merged(middle, kind.width),
            marks: marks(index, u32::from(head >> 2) & VIGOUR_STEPS),
            out: (mathf::cos(angle), mathf::sin(angle)),
            splay: f64::from(u32::from(head >> 6)) / f64::from(SPLAY_STEPS),
        })
    }

    /// How many finer leaves `width` wide each leaf about `(x, z)` stands
    /// for: one near the eye, more as they grow too fine for a pixel to
    /// show, so as many pixels' worth of blade stand there as would have.
    fn merged(&self, (x, z): (f64, f64), width: f64) -> f64 {
        let distance = mathf::hypot(x - self.seen.eye.0, z - self.seen.eye.1);
        let width = width.max(1e-6);
        let wanted = MERGED * distance * self.seen.pixel / width;
        wanted.clamp(1.0, (WIDEST * self.cell / width).max(1.0))
    }

    /// The tallest a cell growing as `stand` has it stands above its ground:
    /// its tallest stem's bowed head, or a flower on its tallest leaf.
    fn tallest(stand: &Stand, flowers: f64) -> f64 {
        let kind = &stand.kind;
        let rise = if kind.stems > 0.0 { STEM_RISE } else { 1.0 };
        let flower = if flowers > 0.0 {
            FLOWER_ROOM + 0.008
        } else {
            0.0
        };
        kind.height.1 * stand.stature * rise * BOW + flower
    }

    /// How high the tallest shoot of the block of `block` by `block` cells
    /// about `(x, z)` stands, ground and all, the lawn's edge blocks standing
    /// for the ground a block beyond them — `ABSENT` where nothing grows —
    /// and, for a block of one cell, how that cell grows, packed.
    ///
    /// A lawn's canopy grid takes a vertex at the middle of each block, so a
    /// point of any block lies in a patch of the grid one of whose corners is
    /// that block's, and the patch's highest corner is at least as high as
    /// anything growing over the point.
    pub(crate) fn canopy_at(
        &self,
        ground: &Heightfield,
        (x, z): (f64, f64),
        block: u32,
    ) -> (f64, [u8; 4]) {
        let none = (f64::from(ABSENT), [0; 4]);
        let Cover::Grass(grass) = self.cover else {
            return none;
        };
        let block = block.max(1);
        let span = self.cell * f64::from(block);
        let last = |low: f64, high: f64| mathf::ceil((high - low) / span).max(1.0) - 1.0;
        let nearest = |at: f64, low: f64, high: f64| {
            let index = mathf::floor((at - low) / span);
            (index, index.clamp(0.0, last(low, high)))
        };
        let ((bx, kept_x), (bz, kept_z)) = (
            nearest(x, self.from.0, self.to.0),
            nearest(z, self.from.1, self.to.1),
        );
        if (bx - kept_x).abs() > 1.0 || (bz - kept_z).abs() > 1.0 {
            return none;
        }
        let (mut top, mut packed) = (f64::NEG_INFINITY, [0; 4]);
        for dz in 0..block {
            for dx in 0..block {
                let corner = (
                    self.from.0 + (kept_x * f64::from(block) + f64::from(dx)) * self.cell,
                    self.from.1 + (kept_z * f64::from(block) + f64::from(dz)) * self.cell,
                );
                let cell = self.cell_of((corner.0 + 0.5 * self.cell, corner.1 + 0.5 * self.cell));
                let Some(stand) = self.grown(&grass, ground, cell) else {
                    continue;
                };
                if block == 1 {
                    packed = self.pack(&grass, &stand);
                }
                let highest =
                    ground.highest_over(corner, (corner.0 + self.cell, corner.1 + self.cell));
                top = top.max(highest + Self::tallest(&stand, grass.flowers));
            }
        }
        if top.is_finite() {
            (top, packed)
        } else {
            none
        }
    }

    /// The blades about `point`, if it lies within this lawn's grass.
    pub(crate) fn canopy(&self, point: Vec3, fields: &[Heightfield]) -> Option<Canopy> {
        let Cover::Grass(grass) = self.cover else {
            return None;
        };
        let at = (point.x, point.z);
        if !self.covers(at) {
            return None;
        }
        let field = fields.get(self.field as usize)?;
        let tops = self.tops.and_then(|tops| fields.get(tops.field as usize));
        let Stand {
            kind,
            shoots,
            stature,
            ..
        } = self.stand(&grass, (field, tops), self.cell_of(at))?;
        // Most leaves stand tall: the taller of two draws, as a leaf's is.
        let top = stature * (kind.height.0 + (kind.height.1 - kind.height.0) * (2.0 / 3.0));
        let above = point.y - field.height_at(at.0, at.1);
        if shoots <= 0.0 || !(-0.05..top).contains(&above) {
            return None;
        }
        let above = above.max(0.0);
        Some(Canopy {
            density: BLADE_FILL * shoots * kind.width / (self.cell * self.cell),
            up: top - above,
            down: above,
        })
    }

    /// Whether anything grows anywhere on the lawn over `field`, looked for
    /// at points a grid of `SURVEY` a side spreads over it.
    pub(crate) fn grows(&self, field: &Heightfield) -> bool {
        let (width, depth) = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let across = |extent: f64| {
            let cells = mathf::ceil(extent / self.cell).clamp(1.0, f64::from(SURVEY));
            u32::try_from(mathf::round_i32(cells)).unwrap_or(1)
        };
        let (columns, rows) = (across(width), across(depth));
        (0..rows).any(|row| {
            (0..columns).any(|column| {
                let x = self.from.0 + width * (f64::from(column) + 0.5) / f64::from(columns);
                let z = self.from.1 + depth * (f64::from(row) + 0.5) / f64::from(rows);
                self.thrives(field, (x, z)) > THRIVES
            })
        })
    }

    /// Whether `(x, z)` lies within the ground the lawn covers itself.
    fn covers(&self, (x, z): (f64, f64)) -> bool {
        let within = |value: f64, low: f64, high: f64| (low..=high).contains(&value);
        within(x, self.from.0, self.to.0)
            && within(z, self.from.1, self.to.1)
            && self
                .hole
                .is_none_or(|(from, to)| !(within(x, from.0, to.0) && within(z, from.1, to.1)))
    }

    /// How much of what the cover holds grows in the cell about `(x, z)`:
    /// as much as the land lets grow there, less on a path, none on a road,
    /// none where a finer lawn covers the ground instead, and fading far
    /// from the eye.
    fn thrives(&self, field: &Heightfield, (x, z): (f64, f64)) -> f64 {
        if !self.covers((x, z)) {
            return 0.0;
        }
        let distance = mathf::hypot(x - self.seen.eye.0, z - self.seen.eye.1);
        let near = 1.0 - smoothstep(self.seen.fade.0, self.seen.fade.1, distance);
        if near <= 0.0 {
            return 0.0;
        }
        let [_, _, lane, green] = field.attributes_at(x, z);
        let (road, path) = decode_lane(lane);
        let (under, hidden) = self
            .shade
            .as_ref()
            .map_or((0.0, 0.0), |shade| shade.at(x, z));
        // Grass thins as the trees about it hide the sky, to none under a
        // closed wood, and weeds all but as soon; their leaves gather under
        // their crowns. Weeds take to the trodden edges of a path and the
        // gaps between tussocks, where fallen leaves show too.
        let grows = match self.cover {
            Cover::Grass(_) => green * (1.0 - 0.65 * path) * (1.0 - smoothstep(0.3, 0.85, hidden)),
            Cover::Weeds(_) => {
                let gap = 1.0 - self.sward((x, z)).clump;
                (green * (0.6 + 0.8 * path)).min(1.0)
                    * (1.0 - smoothstep(0.35, 0.9, hidden))
                    * (0.4 + 0.9 * gap)
            }
            Cover::Litter(_) => {
                let gap = 1.0 - self.sward((x, z)).clump;
                (1.0 - 0.4 * path) * (0.3 + 0.7 * under.max(hidden)) * (0.6 + 0.6 * gap)
            }
        };
        grows * (1.0 - road) * near
    }

    /// Shoot `index` of a cell growing as `stand` has it, hashed to
    /// `cell_key`, its tip bowing `toward` by `splay` of what its height
    /// allows, a share `flowers` of leaves ending in a wildflower; `None` if
    /// `wanted`, told where it roots, how tall it stands and how broad it
    /// spreads, answers that nothing so placed can matter.
    ///
    /// It roots anywhere in its cell and bows no further than the cell's
    /// walls allow, so it stays within the cell the walk tests it in and no
    /// grid shows through the lawn; a flower is borne only where its head
    /// clears the walls too.
    fn shoot(
        &self,
        (stand, flowers): (&Stand, f64),
        (cell_key, index): (u32, u32),
        (toward, splay): ((f64, f64), f64),
        wanted: impl Fn((f64, f64), f64, f64) -> bool,
    ) -> Option<Placed> {
        let kind = &stand.kind;
        let key = mix32(cell_key ^ index.wrapping_mul(0x9e37_79b9)) & KEY;
        // Sixteen-bit draws, finer than any shoot can show, four to a mix.
        let [first, second, third] = [0, 0x9e37_79b9_7f4a_7c15, 0x7f4a_7c15_9e37_79b9]
            .map(|salt| mix64(u64::from(key) ^ salt));
        let draw = |bits: u64, at: u32| {
            u32::try_from((bits >> (16 * at)) & 0xffff).map_or(0.0, f64::from) * (1.0 / 65_536.0)
        };
        let stem = draw(third, 0) < kind.stems;
        let width =
            (kind.width * stand.merged * if stem { 0.35 } else { 1.0 }).min(WIDEST * self.cell);
        let head = stem.then_some(kind.head);
        let breadth = head
            .map_or(0.5 * width, |head| 0.5 * width * head.spread().0)
            .min(0.45 * self.cell);
        let (inner, outer) = (breadth, self.cell - breadth);
        let root = (
            inner + (outer - inner) * draw(first, 0),
            inner + (outer - inner) * draw(first, 1),
        );
        // The taller of two draws: more tall leaves than short, as a square
        // root would spread them.
        let tall = draw(first, 2).max(draw(first, 3));
        let height = (kind.height.0 + (kind.height.1 - kind.height.0) * tall)
            * stand.stature
            * if stem {
                1.3 + 0.4 * draw(third, 1)
            } else {
                1.0
            };
        if !wanted(root, height, breadth) {
            return None;
        }
        let clearance = |at: f64, d: f64| {
            if d > 1e-9 {
                (outer - at) / d
            } else if d < -1e-9 {
                (inner - at) / d
            } else {
                f64::INFINITY
            }
        };
        let bows = if stem {
            0.15
        } else {
            splay * (0.3 + 0.7 * draw(second, 0))
        };
        let reach = (kind.lean * bows * height)
            .min(clearance(root.0, toward.0))
            .min(clearance(root.1, toward.1))
            .max(0.0);
        let tip = (root.0 + reach * toward.0, root.1 + reach * toward.1);
        let clear = |at: f64| at >= FLOWER_ROOM && self.cell - at >= FLOWER_ROOM;
        // Merged leaves bear fewer flowers than they stand for, but broader.
        let flower =
            (!stem && draw(second, 1) < flowers && clear(tip.0) && clear(tip.1)).then(|| {
                ((0.012 + 0.014 * draw(second, 2)) * mathf::sqrt(stand.merged)).min(FLOWER_ROOM)
            });
        Some(Placed {
            root,
            toward,
            reach,
            height,
            width,
            breadth,
            droop: if stem {
                0.02
            } else {
                kind.droop * (0.5 + draw(third, 2))
            },
            twist: 2.4 * (draw(third, 3) - 0.5),
            head,
            flower,
            key,
        })
    }

    /// The nearest thing of the cover `ray` meets within `(near, far)`.
    pub(crate) fn intersect(
        &self,
        ray: &Ray,
        near: f64,
        far: f64,
        geometry: Geometry<'_>,
    ) -> Option<Hit> {
        let field = geometry.fields.get(self.field as usize)?;
        let tops = self
            .tops
            .and_then(|tops| geometry.fields.get(tops.field as usize));
        let (enter, leave) = self.bounds().span(ray, reciprocal(ray.dir), far)?;
        // Straight to where the ray first comes down within reach of what
        // the cover holds, before which is all air above it, and no further
        // than where it meets the ground, past which everything is hidden.
        let comes_down = |from: f64, to: f64| match tops {
            Some(tops) => tops.approach(ray, 0.0, (from, to)),
            None => field.approach(ray, self.reach(), (from, to)),
        };
        let mut t = comes_down(enter.max(near), leave)?;
        let mut walk = Walk::from(self, ray, t);
        let mut over = 0u32;
        for _ in 0..MAX_CELLS {
            if t >= leave {
                return None;
            }
            let exit = walk.exit().min(leave);
            let (x0, z0) = (
                self.from.0 + f64::from(walk.cell.0) * self.cell,
                self.from.1 + f64::from(walk.cell.1) * self.cell,
            );
            let ends = (x0 + self.cell, z0 + self.cell);
            let (entering, leaving) = (
                ray.origin.y + ray.dir.y * t,
                ray.origin.y + ray.dir.y * exit,
            );
            // Wholly under the ground across the cell: everything beyond is
            // hidden, and anything nearer was met already.
            if entering.max(leaving) < field.lowest_over((x0, z0), ends) {
                return None;
            }
            let highest = field.highest_over((x0, z0), ends);
            let ceiling = tops.map_or(highest + self.reach(), |tops| {
                tops.highest_over((x0, z0), ends)
            });
            if entering.min(leaving) > ceiling {
                over += 1;
                // Long over the sward's top: straight on to where it next
                // comes down into it.
                if over >= OVER_BEFORE_SKIP {
                    t = comes_down(exit, leave)?;
                    walk = Walk::from(self, ray, t);
                    over = 0;
                    continue;
                }
            } else {
                over = 0;
                if let Some(hit) = self.cell_hit(ray, walk.cell, (t, exit), (field, tops), highest)
                {
                    return Some(hit);
                }
            }
            walk.step();
            t = exit;
        }
        None
    }

    /// The nearest thing in cell `(cx, cz)` the ray meets within `(from, to)`,
    /// over the ground `field`, which stands at most `highest` across it, and
    /// under the canopy grid `tops`, if it has one.
    fn cell_hit(
        &self,
        ray: &Ray,
        (cx, cz): (u32, u32),
        (from, to): (f64, f64),
        (field, tops): (&Heightfield, Option<&Heightfield>),
        highest: f64,
    ) -> Option<Hit> {
        let x0 = self.from.0 + f64::from(cx) * self.cell;
        let z0 = self.from.1 + f64::from(cz) * self.cell;
        let middle = (x0 + 0.5 * self.cell, z0 + 0.5 * self.cell);
        let (stand, thrives) = match self.cover {
            Cover::Grass(grass) => (
                Some(self.stand(&grass, (field, tops), ((cx, cz), middle))?),
                1.0,
            ),
            Cover::Weeds(_) | Cover::Litter(_) => {
                let thrives = self.thrives(field, middle);
                if thrives <= THRIVES {
                    return None;
                }
                (None, thrives)
            }
        };
        // Everything lies on the plane of the ground across the cell, three
        // looks at it apart, and never above its highest.
        let half = 0.5 * self.cell;
        let height = field.height_at(middle.0, middle.1);
        let plane = Plane {
            middle,
            height,
            slope: (
                (field.height_at(middle.0 + half, middle.1) - height) / half,
                (field.height_at(middle.0, middle.1 + half) - height) / half,
            ),
            highest,
        };
        let cell_key = hash2(cx, cz, self.seed);
        let corner = (x0, z0);
        match (self.cover, stand) {
            (Cover::Grass(grass), Some(stand)) => self.grass_hit(
                ray,
                (&stand, grass.flowers),
                (cell_key, corner, plane),
                (from, to),
            ),
            (Cover::Weeds(weeds), _) => rosette_hit(
                ray,
                (&weeds, thrives, self.cell),
                (cell_key, corner, plane),
                (from, to),
            ),
            (Cover::Litter(litter), _) => litter_hit(
                ray,
                (&litter, thrives, self.cell),
                (cell_key, corner, plane),
                (from, to),
            ),
            (Cover::Grass(_), None) => None,
        }
    }

    /// The nearest shoot or flower of a cell growing as `stand` has it the
    /// ray meets.
    fn grass_hit(
        &self,
        ray: &Ray,
        (stand, flowers): (&Stand, f64),
        (cell_key, (x0, z0), plane): (u32, (f64, f64), Plane),
        (from, to): (f64, f64),
    ) -> Option<Hit> {
        let standing = stand.shoots / stand.merged + unit(mix32(cell_key ^ 0x51));
        let count = u32::try_from(mathf::round_i32(mathf::floor(standing))).unwrap_or(0);
        let mut best: Option<Hit> = None;
        let mut reach = to;
        let flat = ray.dir.x * ray.dir.x + ray.dir.z * ray.dir.z;
        let start = TAU * unit(mix32(cell_key ^ 0x51));
        let mut heading = (mathf::cos(start), mathf::sin(start));
        let fountain = 1.5 * stand.splay * stand.kind.tufted;
        let splay = 0.6 + 0.8 * stand.splay;
        // Whether a shoot rooted at `root` in the cell, `height` tall and
        // spreading `breadth`, can reach the ray at all: near enough its
        // line, and not wholly beneath the stretch of it over the shoot.
        let wanted = |root: (f64, f64), height: f64, breadth: f64| {
            let (rx, rz) = (x0 + root.0, z0 + root.1);
            let spread = stand.kind.lean * splay * height + breadth + FLOWER_ROOM;
            let across = ray.dir.x * (rz - ray.origin.z) - ray.dir.z * (rx - ray.origin.x);
            if across * across > spread * spread * flat {
                return false;
            }
            if flat < 1e-12 {
                return true;
            }
            let nearest =
                ((rx - ray.origin.x) * ray.dir.x + (rz - ray.origin.z) * ray.dir.z) / flat;
            let lowest =
                ray.origin.y + ray.dir.y * nearest - ray.dir.y.abs() * spread / mathf::sqrt(flat);
            lowest <= plane.at((rx, rz)) + height * BOW + FLOWER_ROOM
        };
        for index in 0..count {
            // Each shoot its own way round, fountaining out from its
            // tussock's middle the nearer the rim it roots.
            let (hx, hz) = (
                heading.0 + fountain * stand.out.0,
                heading.1 + fountain * stand.out.1,
            );
            let length = mathf::hypot(hx, hz).max(1e-9);
            heading = turn_golden(heading);
            let Some(placed) = self.shoot(
                (stand, flowers),
                (cell_key, index),
                ((hx / length, hz / length), splay),
                wanted,
            ) else {
                continue;
            };
            let (rx, rz) = (x0 + placed.root.0, z0 + placed.root.1);
            // From above, a shoot and its flower lie within a disc about the
            // middle of its bow; a ray passing wide of that meets neither.
            let (lx, lz) = placed.toward;
            let (centre_x, centre_z) = (rx + 0.5 * placed.reach * lx, rz + 0.5 * placed.reach * lz);
            let spread = 0.5 * placed.reach + placed.breadth + placed.flower.unwrap_or(0.0);
            let across =
                ray.dir.x * (centre_z - ray.origin.z) - ray.dir.z * (centre_x - ray.origin.x);
            if across * across > spread * spread * flat {
                continue;
            }
            let shoot = Shoot {
                // Rooted a little into the ground it stands on.
                root: Vec3::new(rx, plane.at((rx, rz)) - 0.01, rz),
                placed: Placed {
                    key: placed.key | stand.marks,
                    ..placed
                },
            };
            if let Some(hit) = shoot.meet(ray, (from, reach)) {
                reach = hit.t;
                best = Some(hit);
            }
            if let Some(radius) = placed.flower {
                let head = shoot.at(1.0) + Vec3::new(0.0, 0.008, 0.0);
                let facing = Vec3::new(0.35 * lx, 1.0, 0.35 * lz).normalized();
                if let Some(t) = disc(ray, head, facing, radius, (from, reach)) {
                    reach = t;
                    best = Some(member(
                        t,
                        facing,
                        shoot.placed.key | FLOWER,
                        1.0,
                        (0.0, 0.0),
                    ));
                }
            }
        }
        best
    }
}

/// Where a walk across a cover's cells has come: the cell it is in, where
/// along the ray it next crosses a wall across x and one across z, how far
/// it goes between such walls, and which way it steps across each.
struct Walk {
    cell: (u32, u32),
    next: (f64, f64),
    delta: (f64, f64),
    step: (u32, u32),
}

impl Walk {
    /// The walk across `lawn`'s cells of `ray`, from `t` along it.
    fn from(lawn: &Lawn, ray: &Ray, t: f64) -> Self {
        let first = ray.at(t);
        let ((cx, fx), (cz, fz)) = (
            cell((first.x - lawn.from.0) / lawn.cell),
            cell((first.z - lawn.from.1) / lawn.cell),
        );
        let wall = |fraction: f64, dir: f64| {
            if dir.abs() < 1e-12 {
                f64::INFINITY
            } else {
                let to = if dir > 0.0 { 1.0 - fraction } else { fraction };
                t + to * lawn.cell / dir.abs()
            }
        };
        let delta = |dir: f64| {
            if dir.abs() < 1e-12 {
                f64::INFINITY
            } else {
                lawn.cell / dir.abs()
            }
        };
        let step = |dir: f64| if dir > 0.0 { 1 } else { u32::MAX };
        Self {
            cell: (cx, cz),
            next: (wall(fx, ray.dir.x), wall(fz, ray.dir.z)),
            delta: (delta(ray.dir.x), delta(ray.dir.z)),
            step: (step(ray.dir.x), step(ray.dir.z)),
        }
    }

    /// Where along the ray the walk leaves its cell.
    fn exit(&self) -> f64 {
        self.next.0.min(self.next.1)
    }

    /// On into the next cell.
    fn step(&mut self) {
        if self.next.0 <= self.next.1 {
            self.cell.0 = self.cell.0.wrapping_add(self.step.0);
            self.next.0 += self.delta.0;
        } else {
            self.cell.1 = self.cell.1.wrapping_add(self.step.1);
            self.next.1 += self.delta.1;
        }
    }
}

impl Shoot {
    /// The point `along` its length, root to tip: a curve rising from the
    /// root and bowing out toward its tip, which droops back from where the
    /// shoot stands highest.
    fn at(&self, along: f64) -> Vec3 {
        let Placed {
            toward: (lx, lz),
            reach,
            height,
            droop,
            ..
        } = self.placed;
        let out = |share: f64| Vec3::new(lx * reach * share, 0.0, lz * reach * share);
        let bend = self.root + out(0.3) + Vec3::new(0.0, height * BOW, 0.0);
        let tip = self.root + out(1.0) + Vec3::new(0.0, height * (1.0 - droop), 0.0);
        let back = 1.0 - along;
        self.root * (back * back) + bend * (2.0 * along * back) + tip * (along * along)
    }

    /// Its width `along` its length: a leaf broad and near even most of the
    /// way, then drawn to a point; a stem even to its head.
    fn width_at(&self, along: f64) -> f64 {
        let width = self.placed.width;
        if self.placed.head.is_some() {
            return width;
        }
        width
            * if along < 0.6 {
                1.0 - 0.15 * along
            } else {
                0.91 * (1.0 - along) / 0.4
            }
    }

    /// Where `ray` first meets the shoot within `(from, to)`: along three
    /// flat pieces following its curve, each turned a little further about
    /// the shoot than the last, and a stem's seed head at its top.
    fn meet(&self, ray: &Ray, (from, to): (f64, f64)) -> Option<Hit> {
        let Placed {
            toward: (lx, lz),
            height,
            width,
            twist,
            head,
            key,
            ..
        } = self.placed;
        // Square to the shoot's line, and so to every piece of its curve,
        // which bows only in the upright plane through that line.
        let level_side = Vec3::new(-lz, 0.0, lx);
        let top = if head.is_some() { 0.82 } else { 1.0 };
        let along = [0.0, top / 3.0, 2.0 * top / 3.0, top];
        let points = along.map(|share| self.at(share));
        // Each piece turned by the twist at its middle, a sixth, a half and
        // five sixths of the way: the first turn, then twice it again twice.
        let first = twist * top / 6.0;
        let (mut cos, mut sin) = (mathf::cos(first), mathf::sin(first));
        let (cos_step, sin_step) = (mathf::cos(2.0 * first), mathf::sin(2.0 * first));
        let mut best: Option<Hit> = None;
        let mut reach = to;
        for piece in 0..3 {
            let (low, high) = (along[piece], along[piece + 1]);
            let widths = (self.width_at(low), self.width_at(high));
            if let Some((t, normal, share)) = strip(
                ray,
                (points[piece], points[piece + 1]),
                (level_side, (cos, sin)),
                widths,
                (from, reach),
            ) {
                reach = t;
                best = Some(member(
                    t,
                    normal,
                    key,
                    low + (high - low) * share,
                    (0.0, 0.0),
                ));
            }
            (cos, sin) = (
                cos * cos_step - sin * sin_step,
                sin * cos_step + cos * sin_step,
            );
        }
        if let Some(head) = head {
            // A spike is a slender ear; a plume spreads loose about the stem,
            // in sprays across one another.
            let (start, end) = (points[3], self.at(1.0) + Vec3::new(0.0, 0.08 * height, 0.0));
            let (spread, sprays) = head.spread();
            let wide = spread * width;
            let (turn_cos, turn_sin) = (mathf::cos(twist), mathf::sin(twist));
            for spray in 0..sprays {
                // Each spray a quarter turn on from the last.
                let (cos, sin) = if spray == 0 {
                    (turn_cos, turn_sin)
                } else {
                    (-turn_sin, turn_cos)
                };
                if let Some((t, normal, share)) = strip(
                    ray,
                    (start, end),
                    (level_side, (cos, sin)),
                    (wide, 0.3 * wide),
                    (from, reach),
                ) {
                    reach = t;
                    best = Some(member(t, normal, key | HEAD, share, (0.0, 0.0)));
                }
            }
        }
        best
    }
}

/// Where `ray` meets, within `(from, to)`, the flat strip from `start` to
/// `end` spread along the unit `side` — square to the strip's line — turned
/// about that line by the angle whose cosine and sine are `turn`: `wide`
/// across at its start and `narrow` at its end. Its normal there, a unit one,
/// and how far along it.
fn strip(
    ray: &Ray,
    (start, end): (Vec3, Vec3),
    (side, (cos, sin)): (Vec3, (f64, f64)),
    (wide, narrow): (f64, f64),
    (from, to): (f64, f64),
) -> Option<(f64, Vec3, f64)> {
    let line = end - start;
    let length = line.length();
    if length <= 1e-12 {
        return None;
    }
    let axis = line * (1.0 / length);
    let side = side * cos + axis.cross(side) * sin;
    let normal = side.cross(axis);
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(start - ray.origin) / facing;
    if !(t >= from && t < to) {
        return None;
    }
    let offset = ray.at(t) - start;
    let along = offset.dot(axis) / length;
    if !(0.0..=1.0).contains(&along) {
        return None;
    }
    let half = f64::midpoint(wide, (narrow - wide) * along);
    (offset.dot(side).abs() <= half).then_some((t, normal, along))
}

/// The mark bits a shoot of its sward's kind `kind` carries, grown where the
/// sward is `vigour` fifteenths rank.
pub(crate) const fn marks(kind: u32, vigour: u32) -> u32 {
    let vigour = if vigour < VIGOUR_STEPS {
        vigour
    } else {
        VIGOUR_STEPS
    };
    (vigour << VIGOUR_SHIFT) | ((kind & KIND_MASK) << KIND_SHIFT)
}

/// How rank the sward a shoot marked `mark` grew in is, `0.0` to `1.0`.
pub(crate) fn vigour(mark: u32) -> f64 {
    f64::from((mark >> VIGOUR_SHIFT) & VIGOUR_STEPS) / f64::from(VIGOUR_STEPS)
}

/// Which of its sward's kinds of grass a shoot marked `mark` is.
pub(crate) fn grass_kind(mark: u32) -> usize {
    ((mark >> KIND_SHIFT) & KIND_MASK) as usize
}

/// What a ray meets of a cover: `t` along it, facing `normal`, member `mark`
/// met `along` its length at `uv` on it.
fn member(t: f64, normal: Vec3, mark: u32, along: f64, uv: (f64, f64)) -> Hit {
    Hit {
        t,
        normal,
        shading: normal,
        mark,
        along,
        uv,
        girth: 0.0,
        material: None,
        tangent: Vec3::ZERO,
    }
}

/// A flat leaf: where its stalk meets it, the unit way its midrib runs and
/// the unit way across it, how long and how broad at its widest, and its
/// outline.
#[derive(Copy, Clone, Debug)]
struct Flat {
    base: Vec3,
    axis: Vec3,
    side: Vec3,
    length: f64,
    width: f64,
    outline: Outline,
}

impl Flat {
    /// Where `ray` meets the leaf within `(from, to)`, its normal facing the
    /// ray, and the place on the leaf.
    fn meet(&self, ray: &Ray, (from, to): (f64, f64)) -> Option<(f64, Vec3, (f64, f64))> {
        let normal = self.side.cross(self.axis).normalized();
        let facing = normal.dot(ray.dir);
        if facing.abs() < 1e-12 {
            return None;
        }
        let t = normal.dot(self.base - ray.origin) / facing;
        if !(t >= from && t < to) {
            return None;
        }
        let offset = ray.at(t) - self.base;
        let (u, v) = (
            offset.dot(self.axis) / self.length,
            offset.dot(self.side) / self.width,
        );
        self.outline.covers(u, v).then_some((
            t,
            if facing < 0.0 { normal } else { -normal },
            (u, v),
        ))
    }
}

/// The nearest leaf of the rosette a cell of `weeds` may hold, if it holds
/// one: its leaves radiating from near the cell's middle and lying low over
/// the ground, each no longer than keeps it within the cell.
fn rosette_hit(
    ray: &Ray,
    (weeds, thrives, side): (&Weeds, f64, f64),
    (cell_key, (x0, z0), plane): (u32, (f64, f64), Plane),
    (from, to): (f64, f64),
) -> Option<Hit> {
    if unit(mix32(cell_key ^ 0x3d)) >= weeds.share * thrives {
        return None;
    }
    let outline = match mix32(cell_key ^ 0x7c) % 3 {
        0 => Outline::Runcinate,
        1 => Outline::Lanceolate,
        _ => Outline::Trefoil,
    };
    let reach = 0.42 * side;
    let wander = 0.5 * side - reach;
    let centre = (
        x0 + 0.5 * side + wander * (2.0 * unit(mix32(cell_key ^ 0x11)) - 1.0),
        z0 + 0.5 * side + wander * (2.0 * unit(mix32(cell_key ^ 0x12)) - 1.0),
    );
    let (least, most) = (weeds.leaves.0, weeds.leaves.1.max(weeds.leaves.0));
    let count = least + mix32(cell_key ^ 0x13) % (most - least + 1);
    let base = Vec3::new(centre.0, plane.at(centre) + 0.004, centre.1);
    let start = TAU * unit(mix32(cell_key ^ 0x14));
    let mut best: Option<Hit> = None;
    let mut near = to;
    for index in 0..count {
        let key = mix32(cell_key ^ index.wrapping_mul(0x85eb_ca6b)) & KEY;
        let angle = start + TAU * (f64::from(index) + 0.3 * unit(key)) / f64::from(count.max(1));
        let rise = match outline {
            Outline::Trefoil => 0.05,
            _ => 0.12 + 0.3 * unit(mix32(key ^ 1)),
        };
        let length = reach * (0.65 + 0.35 * unit(mix32(key ^ 2)));
        let (cos, sin) = (mathf::cos(angle), mathf::sin(angle));
        let flat = Flat {
            base,
            axis: Vec3::new(cos, rise.min(ROSETTE_RISE), sin).normalized(),
            side: Vec3::new(-sin, 0.0, cos),
            length,
            width: length
                * if outline == Outline::Trefoil {
                    0.5
                } else {
                    0.3
                },
            outline,
        };
        if let Some((t, normal, uv)) = flat.meet(ray, (from, near)) {
            near = t;
            best = Some(member(t, normal, key | WEED, uv.0, uv));
        }
    }
    best
}

/// The nearest of the leaves fallen in a cell of `litter`, as many lying
/// there as the ground `thrives` for, each flat on the ground and no longer
/// than keeps it within the cell.
fn litter_hit(
    ray: &Ray,
    (litter, thrives, side): (&Litter, f64, f64),
    (cell_key, (x0, z0), plane): (u32, (f64, f64), Plane),
    (from, to): (f64, f64),
) -> Option<Hit> {
    let lying = f64::from(litter.most) * thrives * (0.35 + 0.65 * unit(mix32(cell_key ^ 0x21)));
    let count = u32::try_from(mathf::round_i32(mathf::floor(
        lying + unit(mix32(cell_key ^ 0x22)),
    )))
    .unwrap_or(0);
    let ground = plane.normal();
    let mut best: Option<Hit> = None;
    let mut near = to;
    for index in 0..count {
        let key = mix32(cell_key ^ index.wrapping_mul(0xc2b2_ae35)) & KEY;
        let length =
            (litter.length.0 + (litter.length.1 - litter.length.0) * unit(key)).min(0.45 * side);
        let room = 0.5 * side - 0.5 * length;
        let centre = (
            x0 + 0.5 * side + room * (2.0 * unit(mix32(key ^ 3)) - 1.0),
            z0 + 0.5 * side + room * (2.0 * unit(mix32(key ^ 4)) - 1.0),
        );
        let yaw = TAU * unit(mix32(key ^ 5));
        // Along the ground, a leaf curled a little off it.
        let level = Vec3::new(mathf::cos(yaw), 0.0, mathf::sin(yaw));
        let axis = (level - ground * level.dot(ground)
            + ground * (0.15 * (unit(mix32(key ^ 6)) - 0.3)))
            .normalized();
        let across = ground.cross(axis).normalized();
        // Each leaf a little above the one fallen before it.
        let lift = 0.003 + 0.002 * f64::from(index);
        let middle = Vec3::new(centre.0, plane.at(centre) + lift, centre.1);
        let flat = Flat {
            base: middle - axis * (0.5 * length),
            axis,
            side: across,
            length,
            width: 0.5 * length,
            outline: litter.outline,
        };
        if let Some((t, normal, uv)) = flat.meet(ray, (from, near)) {
            near = t;
            // How far gone the leaf is, from fresh to near black.
            let decay = litter.age + (1.0 - litter.age) * unit(mix32(key ^ 7));
            best = Some(member(t, normal, key | LITTER, decay, uv));
        }
    }
    best
}

/// The golden angle's cosine and sine: each shoot of a cell turned this far
/// from the last spreads their headings evenly round the circle.
const GOLDEN: (f64, f64) = (-0.737_368_878_078_319_7, 0.675_490_294_261_523_9);

/// `toward` turned by the golden angle.
fn turn_golden((x, z): (f64, f64)) -> (f64, f64) {
    (x * GOLDEN.0 - z * GOLDEN.1, x * GOLDEN.1 + z * GOLDEN.0)
}

/// Where `ray` meets, within `(from, to)`, the disc of `radius` about
/// `centre` facing `normal`.
fn disc(ray: &Ray, centre: Vec3, normal: Vec3, radius: f64, (from, to): (f64, f64)) -> Option<f64> {
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(centre - ray.origin) / facing;
    let offset = ray.at(t) - centre;
    (t >= from && t < to && offset.dot(offset) <= radius * radius).then_some(t)
}

#[cfg(test)]
#[path = "grass_tests.rs"]
mod tests;
