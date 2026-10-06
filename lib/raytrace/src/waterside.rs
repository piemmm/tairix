//! Plants of the water's edge: reeds and reedmace standing in the shallows
//! and along wet banks, water lilies and pondweed floating on still water,
//! and water-crowfoot streaming in running water.
//!
//! Each is grown as a square patch of its plants, so one prototype stands for
//! a clump near the eye and another for a whole bed far off, the squares
//! laid edge to edge without a seam, and at a stature of its kind's so a bed
//! can shorten toward its edges. Every plant's parts share the two low bits
//! of their key, so a plant takes one of its material's four colours whole.
//!
//! A lily's pads are meshes each shaped and worn as its key and its age have
//! it, laid one after another so each rests on those already floating
//! beneath it; its flowers float among them, each at its own stage from bud
//! to spent.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::leaf::Outline;
use crate::lily::{self, Pad, Petal, Sheet, Trim, SINUS};
use crate::noise::smoothstep;
use crate::prototype::{round, Assembly, Blade, Building, Mapping, Part, Tube};
use crate::sample::{mix32, GOLDEN_ANGLE};
use crate::tree::Season;
use crate::vector::{cell_of, power, real, single, singles, wrapped, Vec3};

/// A plant of the water's edge.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Margin {
    /// Common reed: tall stems leafy up their length, a plume nodding from
    /// each top.
    Reed,
    /// Reedmace, the bulrush: fans of strap leaves rising from the root, and
    /// flowering stems each topped by a brown velvet spike.
    Reedmace,
    /// Water lily: pads floating on still water, resting on one another, and
    /// in summer its white flowers, from bud to spent.
    Lily,
    /// Floating pondweed: small oval leaves lying on the water in rosettes.
    Pondweed,
    /// Water-crowfoot: long stems streaming down the current just beneath
    /// the surface, tufted with thread-fine leaves, and in spring and
    /// summer white flowers held on the water.
    Crowfoot,
}

/// The materials a patch is made in: its stems, a lily's the stalks of its
/// pads and flowers; its leaves or pads; its flowers' sepals; its heads — a
/// reed's plume, a reedmace's spike, a flower's petals; and a flower's
/// heart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Marsh {
    pub(crate) stems: u16,
    pub(crate) leaves: u16,
    pub(crate) sepals: u16,
    pub(crate) heads: u16,
    pub(crate) hearts: u16,
}

/// A patch of `count` of `margin`'s plants filling a square `side` across
/// about its middle, as they are in `season`, standing up from or lying on
/// its plane, as tall or as broad as `stature` of their kind's; its
/// hierarchy still to build. `None` when the heap will not hold it.
pub(crate) fn patch(
    margin: Margin,
    (side, count, stature): (f64, u16, f64),
    marsh: Marsh,
    season: Season,
    seed: u64,
) -> Option<Building> {
    let grain = if side <= FINE_SIDE { FINE } else { COARSE };
    let (parts, vertices) = room(margin, count, grain);
    let mut grower = Grower {
        assembly: Assembly::with_room(parts, vertices)?,
        dice: NonCryptoRng::seed_from_u64(seed),
        marsh,
        season,
        stature,
        grain,
    };
    // The wind the whole patch's plumes and leaves lean with, and the first
    // of the one plant in eight that flowers.
    let wind = TAU * grower.unit();
    let flowering = u32::from(FLOWERING_EVERY);
    let first = grower.below(flowering);
    let mut lilies = Lilies::default();
    if margin == Margin::Lily {
        lilies.floating.try_reserve(usize::from(count)).ok()?;
    }
    let mut key = mix32(u32::try_from(seed >> 32).unwrap_or(0) ^ 0x57a7);
    for plant in 0..count {
        let at = Vec3::new(
            side * (grower.unit() - 0.5),
            0.0,
            side * (grower.unit() - 0.5),
        );
        key = mix32(key ^ u32::from(plant));
        let flowers = |seasons: &[Season]| {
            seasons.contains(&season) && (u32::from(plant) + first).is_multiple_of(flowering)
        };
        match margin {
            Margin::Reed => grower.reed(at, wind, key)?,
            Margin::Reedmace => grower.reedmace(at, wind, key)?,
            Margin::Lily => grower.sow(&mut lilies, at, key, flowers(&[Season::Summer]))?,
            Margin::Pondweed => grower.pondweed(at, key)?,
            Margin::Crowfoot => {
                grower.crowfoot(at, key, flowers(&[Season::Spring, Season::Summer]))?;
            }
        }
    }
    grower.lilies(&lilies)?;
    grower.assembly.finish()
}

/// The widest patch whose plants are drawn in full: a clump near the eye,
/// where a pad's frayed margin and a flower's stamens are seen; a bed beyond
/// is drawn plainer.
const FINE_SIDE: f64 = 1.5;

/// The most parts and vertices a patch of `count` of `margin`'s plants
/// takes, drawn at `grain`.
fn room(margin: Margin, count: u16, grain: Grain) -> (usize, usize) {
    let count = usize::from(count);
    let flowers = count.div_ceil(usize::from(FLOWERING_EVERY));
    match margin {
        Margin::Reed => (
            count * (usize::from(2 + u16::from(REED_PIECES) * REED_LEAVES) + grain.plume_room()),
            0,
        ),
        Margin::Reedmace => (
            count * (usize::from(u16::from(MACE_PIECES) * MACE_LEAVES) + 3 + SPIKE + wide(FLUFF)),
            0,
        ),
        Margin::Lily => {
            let (pad, flower) = (grain.pad_room(), grain.flower_room());
            (
                count * pad.0 + flowers * flower.0,
                count * pad.1 + flowers * flower.1,
            )
        }
        Margin::Pondweed => (count * usize::from(ROSETTE), 0),
        Margin::Crowfoot => {
            let blossom = grain.blossom_room();
            (
                count * usize::from(STREAMERS * STREAMER_PIECES * (1 + TUFT)) + flowers * blossom.0,
                flowers * blossom.1,
            )
        }
    }
}

/// Leaves a reed's stem carries and the flat pieces each arches through,
/// the pieces its plume's rachis bows in, the leaves of one reedmace
/// plant's fan and their pieces, the pieces its spike is laid in and the
/// tufts of fluff a winter's spike bursts in, and the most leaves a
/// pondweed's rosette spreads.
const REED_LEAVES: u16 = 8;
const REED_PIECES: u8 = 3;
const RACHIS: usize = 3;
const MACE_LEAVES: u16 = 8;
const MACE_PIECES: u8 = 4;
const SPIKE: usize = 6;
const FLUFF: u32 = 5;
const ROSETTE: u16 = 5;
/// The stems a crowfoot plant streams, the pieces each runs in, and the
/// leaves each piece's tuft spreads; its flower's petals and stamens, the
/// radius of its head of carpels in the flower's size, and the rings that
/// head's dome is laid in, each a radius and a height in its own radius.
const STREAMERS: u16 = 3;
const STREAMER_PIECES: u16 = 4;
const TUFT: u16 = 4;
const BLOSSOM_PETALS: u32 = 5;
const BLOSSOM_STAMENS: u32 = 20;
const CARPELS: f64 = 0.16;
const DOME: [(f64, f64); 4] = [(0.35, 0.93), (0.65, 0.75), (0.88, 0.45), (1.0, 0.1)];
/// One lily, or crowfoot, in so many flowers in its season.
const FLOWERING_EVERY: u16 = 8;

/// How finely a patch's plants are drawn: the rings a lily pad's mesh is
/// laid in out from its stalk, in its radius, and the spokes cutting them;
/// the rows along and the columns across a lily's sepal's or petal's, and a
/// stamen's or a small flower's petal's; the fewest and the most petals a
/// lily opens; its stamens; the rings of its stigma's mesh, each a radius
/// and a height in the stigma's radius, its last running down its ovary,
/// and its spokes to each of its rays; whether the stalks under the water
/// are drawn; a reed plume's branches and the tufts each bears; and the
/// spokes of a crowfoot's head of carpels.
#[derive(Copy, Clone, Debug)]
struct Grain {
    rings: &'static [f64],
    spokes: u32,
    sheet: (u32, u32),
    strap: (u32, u32),
    petals: (u32, u32),
    stamens: u32,
    stigma: &'static [(f64, f64)],
    per_ray: u32,
    stalks: bool,
    plume: (u32, u32),
    carpels: u32,
}

const FINE: Grain = Grain {
    rings: &[0.25, 0.5, 0.7, 0.85, 1.0],
    spokes: 36,
    sheet: (9, 6),
    strap: (5, 2),
    petals: (18, 24),
    stamens: 56,
    stigma: &[
        (0.3, -0.06),
        (0.75, 0.1),
        (1.0, 0.28),
        (0.92, -0.35),
        (1.25, -0.95),
    ],
    per_ray: 2,
    stalks: true,
    plume: (18, 4),
    carpels: 14,
};

const COARSE: Grain = Grain {
    rings: &[0.5, 0.85, 1.0],
    spokes: 14,
    sheet: (4, 2),
    strap: (2, 1),
    petals: (12, 16),
    stamens: 18,
    stigma: &[(0.5, 0.0), (1.0, 0.28), (0.92, -0.35), (1.25, -0.95)],
    per_ray: 1,
    stalks: false,
    plume: (7, 1),
    carpels: 7,
};

/// A flower's sepals, the most petals an old one sheds onto the water, and
/// the most rays its stigma spreads.
const SEPALS: u32 = 4;
const SHED: u32 = 2;
const RAYS: (u32, u32) = (14, 20);

impl Grain {
    /// The parts and the vertices a pad takes, its stalk with it.
    fn pad_room(self) -> (usize, usize) {
        let (rings, spokes) = (whole(self.rings.len()), self.spokes);
        let parts = spokes + (rings - 1) * spokes * 2 + self.stalk_room();
        (wide(parts), wide(1 + rings * (spokes + 1)))
    }

    /// The rows and the columns `sheet`'s mesh is cut in.
    fn grid(self, sheet: Sheet) -> (u32, u32) {
        match sheet {
            Sheet::Sepal | Sheet::Petal | Sheet::Broad => self.sheet,
            Sheet::Stamen => self.strap,
        }
    }

    /// The parts a reed's plume takes, or the spear it grows in its stead.
    fn plume_room(self) -> usize {
        let (branches, tufts) = self.plume;
        RACHIS + wide(branches * (1 + tufts))
    }

    /// The parts and the vertices a crowfoot's flower takes.
    fn blossom_room(self) -> (usize, usize) {
        let (petal_parts, petal_vertices) = self.sheet_room(Sheet::Broad);
        let (strap_parts, strap_vertices) = self.sheet_room(Sheet::Stamen);
        let (rings, spokes) = (whole(DOME.len()), self.carpels);
        let parts = BLOSSOM_PETALS * petal_parts
            + BLOSSOM_STAMENS * strap_parts
            + spokes
            + (rings - 1) * spokes * 2;
        let vertices =
            BLOSSOM_PETALS * petal_vertices + BLOSSOM_STAMENS * strap_vertices + 1 + rings * spokes;
        (wide(parts), wide(vertices))
    }

    /// The parts and the vertices `sheet`'s mesh takes.
    fn sheet_room(self, sheet: Sheet) -> (u32, u32) {
        let (rows, columns) = self.grid(sheet);
        (rows * columns * 2, (rows + 1) * (columns + 1))
    }

    /// The parts and the vertices a flower takes at most, its stalk with it.
    fn flower_room(self) -> (usize, usize) {
        let sheets = SEPALS + self.petals.1 + SHED;
        let (sheet_parts, sheet_vertices) = self.sheet_room(Sheet::Petal);
        let (strap_parts, strap_vertices) = self.sheet_room(Sheet::Stamen);
        let spokes = self.per_ray * RAYS.1;
        let rings = whole(self.stigma.len());
        let parts = sheets * sheet_parts
            + self.stamens * strap_parts
            + spokes
            + (rings - 1) * spokes * 2
            + self.stalk_room();
        let vertices = sheets * sheet_vertices + self.stamens * strap_vertices + 1 + rings * spokes;
        (wide(parts), wide(vertices))
    }

    /// The parts a stalk takes: two limbs, where stalks are drawn.
    fn stalk_room(self) -> u32 {
        if self.stalks {
            2
        } else {
            0
        }
    }
}

/// A count of a grain's, which are all small.
fn whole(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// A grain's count as a length.
fn wide(count: u32) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX)
}

/// A patch being grown: its prototype as it is assembled, its draws, its
/// materials, the season it stands in, its plants' stature against their
/// kind's, and how finely they are drawn.
struct Grower {
    assembly: Assembly,
    dice: NonCryptoRng,
    marsh: Marsh,
    season: Season,
    stature: f64,
    grain: Grain,
}

impl Grower {
    fn unit(&mut self) -> f64 {
        self.dice.next_f64()
    }

    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    fn below(&mut self, count: u32) -> u32 {
        u32::try_from(self.dice.next_below(u64::from(count))).unwrap_or(0)
    }

    fn push(&mut self, part: Part) -> Option<()> {
        self.assembly.push(part)
    }

    /// A tube from `a` to `b`, `radii` thick at its ends and `stems` along
    /// its stem there, in `material`.
    fn tube(
        &mut self,
        (a, b): (Vec3, Vec3),
        (radii, stems): ((f64, f64), (f64, f64)),
        (material, key): (u16, u32),
    ) -> Option<()> {
        let along = (b - a).normalized();
        let side = if along.y.abs() < 0.99 {
            Vec3::UP
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        self.push(Part::Tube(Tube::new(
            (a, b),
            (radii, stems),
            (material, key),
            along.cross(side),
        )))
    }

    /// A long strap leaf from `base`, setting out along the unit `heading`
    /// and bowing down by `droop` radians over its `length`, `width` either
    /// side of its midrib, in `segments` flat pieces that keep one outline.
    fn strap(
        &mut self,
        (base, heading): (Vec3, Vec3),
        (length, width, droop): (f64, f64, f64),
        (segments, key): (u8, u32),
    ) -> Option<()> {
        let level = Vec3::new(heading.x, 0.0, heading.z);
        let across = if level.length() > 1e-6 {
            Vec3::UP.cross(level).normalized()
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        let piece = length / f64::from(segments.max(1));
        let (mut point, mut axis) = (base, heading);
        for segment in 0..segments {
            let normal = across.cross(axis).normalized();
            let normal = if normal.y < 0.0 { -normal } else { normal };
            let from = segment_mark(segment, segments);
            let to = segment_mark(segment + 1, segments);
            self.push(Part::Leaf(Blade {
                base: singles(point),
                normal: singles(normal),
                axis: singles(axis),
                length: single(piece),
                width: single(width),
                outline: Outline::Strap { from, to },
                fold: 0.35,
                material: self.marsh.leaves,
                key,
            }))?;
            point += axis * piece;
            axis = (axis - Vec3::UP * (droop / f64::from(segments.max(1)))).normalized();
        }
        Some(())
    }

    /// A common reed rooted at `root`: a stem leaning a little with the
    /// `wind`, strap leaves set alternately up its upper length, and a plume
    /// nodding from its top — on only some, last year's, in spring, and on
    /// most as this year's open over the summer.
    fn reed(&mut self, root: Vec3, wind: f64, key: u32) -> Option<()> {
        let height = self.range(1.6, 2.8) * self.stature;
        let lean = 0.05 + 0.1 * self.unit();
        let toward = Vec3::new(mathf::cos(wind), 0.0, mathf::sin(wind));
        let stem = (Vec3::UP + toward * lean).normalized();
        let top = root + stem * height;
        let middle = root + stem * (0.55 * height);
        let thick = self.range(0.0035, 0.006);
        let stems = self.marsh.stems;
        self.tube(
            (root, middle),
            ((thick, 0.8 * thick), (0.0, 0.0)),
            (stems, part(key, 0)),
        )?;
        self.tube(
            (middle, top),
            ((0.8 * thick, 0.35 * thick), (0.0, 0.0)),
            (stems, part(key, 1)),
        )?;
        let turn = TAU * self.unit();
        for leaf in 0..REED_LEAVES {
            let up = 0.22 + 0.62 * f64::from(leaf) / f64::from(REED_LEAVES);
            let around = turn + PI * f64::from(leaf) + self.range(-0.4, 0.4);
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let rise = self.range(0.5, 0.9);
            let heading = (stem * mathf::cos(rise) + out * mathf::sin(rise)).normalized();
            let length = self.range(0.28, 0.45) * (1.1 - 0.35 * up);
            let width = self.range(0.009, 0.016);
            let droop = self.range(0.4, 1.0);
            let base = root + stem * (up * height);
            self.strap(
                (base, heading),
                (length, width, droop),
                (REED_PIECES, part(key, 2 + u32::from(leaf))),
            )?;
        }
        let plumed = match self.season {
            Season::Spring => self.unit() < 0.4,
            Season::Summer => self.unit() < 0.6,
            Season::Autumn { .. } | Season::Winter => true,
        };
        if plumed {
            self.plume(top, (stem, toward), thick, part(key, 64))
        } else {
            self.spear(top, stem, thick, part(key, 65))
        }
    }

    /// A reed's growing tip at `top`, its stem rising along `stem` and
    /// `thick` at its foot: its youngest leaf still rolled about it into a
    /// spear drawn to a point.
    fn spear(&mut self, top: Vec3, stem: Vec3, thick: f64, key: u32) -> Option<()> {
        let length = self.range(0.08, 0.2) * self.stature;
        let lean = Vec3::new(self.range(-0.06, 0.06), 0.0, self.range(-0.06, 0.06));
        let tip = top + (stem + lean).normalized() * length;
        let leaves = self.marsh.leaves;
        self.tube((top, tip), ((0.35 * thick, 0.0), (0.0, 0.0)), (leaves, key))
    }

    /// A reed's plume at `top`, its stem rising along `stem` and `thick` at
    /// its foot: a slender rachis bowing over toward `toward`, its branches
    /// set round it, the longest about its middle, each tufted with silky
    /// spikelets — close while it flowers, spread wide and loose once it has
    /// seeded.
    fn plume(
        &mut self,
        top: Vec3,
        (stem, toward): (Vec3, Vec3),
        thick: f64,
        key: u32,
    ) -> Option<()> {
        let nod = self.range(0.5, 1.1);
        let nodding = (Vec3::UP * mathf::cos(nod) + toward * mathf::sin(nod)).normalized();
        let length = self.range(0.16, 0.3) * (0.7 + 0.3 * self.stature);
        let heads = self.marsh.heads;
        let mut rachis = [top; RACHIS + 1];
        let mut reached = top;
        for (piece, joint) in rachis.iter_mut().enumerate().skip(1) {
            let way = stem
                .lerp(nodding, f64::from(whole(piece)) / f64::from(whole(RACHIS)))
                .normalized();
            reached += way * (length / f64::from(whole(RACHIS)));
            *joint = reached;
        }
        let girth = |at: usize| {
            0.35 * thick * (1.0 - 0.85 * f64::from(whole(at)) / f64::from(whole(RACHIS)))
        };
        for (piece, ends) in rachis.windows(2).enumerate() {
            let (&from, &to) = (ends.first()?, ends.get(1)?);
            self.tube(
                (from, to),
                ((girth(piece), girth(piece + 1)), (0.0, 0.0)),
                (heads, key),
            )?;
        }
        let spread = match self.season {
            Season::Summer => (0.15, 0.4),
            Season::Spring | Season::Autumn { .. } | Season::Winter => (0.3, 0.75),
        };
        let (branches, tufts) = self.grain.plume;
        for branch in 0..branches {
            let along = 0.1 + 0.85 * (f64::from(branch) + self.unit()) / f64::from(branches);
            let (at, way) = along_rachis(&rachis, along)?;
            let (first, second) = round(way);
            let around = TAU * self.unit();
            let out = first * mathf::cos(around) + second * mathf::sin(around);
            let angle = self.range(spread.0, spread.1);
            let droop = self.range(0.0, 0.3);
            let reach = length * (0.15 + 0.4 * mathf::sin(PI * along)) * self.range(0.7, 1.1);
            let heading =
                (way * mathf::cos(angle) + out * mathf::sin(angle) - Vec3::UP * droop).normalized();
            self.tube(
                (at, at + heading * reach),
                ((0.0006, 0.0003), (0.0, 0.0)),
                (heads, key),
            )?;
            for tuft in 0..tufts {
                let from = at + heading * (reach * 0.7 * f64::from(tuft) / f64::from(tufts));
                let axis = (heading
                    + Vec3::new(
                        self.range(-0.2, 0.2),
                        self.range(-0.2, 0.2),
                        self.range(-0.2, 0.2),
                    ))
                .normalized();
                // Each tuft turned its own way about its branch.
                let (flat, edge) = round(axis);
                let roll = TAU * self.unit();
                let normal = flat * mathf::cos(roll) + edge * mathf::sin(roll);
                let tuft_length = reach * self.range(0.35, 0.6);
                self.push(Part::Leaf(Blade {
                    base: singles(from),
                    normal: singles(if normal.y < 0.0 { -normal } else { normal }),
                    axis: singles(axis),
                    length: single(tuft_length),
                    width: single(0.16 * tuft_length),
                    outline: Outline::Fascicle { count: 13 },
                    fold: 0.0,
                    material: heads,
                    key,
                }))?;
            }
        }
        Some(())
    }

    /// A reedmace plant rooted at `root`: a flattened fan of strap leaves
    /// rising nearly straight and bowing over at their tips, and, on many, a
    /// flowering stem topped by its brown spike and the thin spike above it.
    fn reedmace(&mut self, root: Vec3, wind: f64, key: u32) -> Option<()> {
        let height = self.range(1.3, 2.3) * self.stature;
        let fan = wind + self.range(-0.6, 0.6);
        for leaf in 0..MACE_LEAVES {
            let side = if leaf % 2 == 0 { 1.0 } else { -1.0 };
            let around = fan + side * self.range(0.0, 0.25);
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around)) * side;
            let rise = self.range(0.05, 0.25) * (0.5 + f64::from(leaf) / f64::from(MACE_LEAVES));
            let heading = (Vec3::UP * mathf::cos(rise) + out * mathf::sin(rise)).normalized();
            let length = height * self.range(0.75, 1.05);
            let width = self.range(0.007, 0.012);
            let droop = self.range(0.15, 0.6);
            self.strap(
                (root, heading),
                (length, width, droop),
                (MACE_PIECES, part(key, u32::from(leaf))),
            )?;
        }
        if self.unit() < 0.6 {
            let top = root + Vec3::UP * (height * self.range(0.85, 1.05));
            let spike = self.range(0.14, 0.22);
            let thin = self.range(0.08, 0.14);
            let head = top - Vec3::UP * thin;
            let stem = self.range(0.004, 0.006);
            let stems = self.marsh.stems;
            let low = head - Vec3::UP * spike;
            self.tube(
                (root, low),
                ((stem, 0.8 * stem), (0.0, 0.0)),
                (stems, part(key, 64)),
            )?;
            let girth = self.range(0.012, 0.017);
            self.spike((low, head), girth, part(key, 65))?;
            // The male spike above it, its pollen shed, withers to a bare
            // spire bending as it dries.
            let bend = head
                + (top - head) * 0.55
                + Vec3::new(self.range(-0.01, 0.01), 0.0, self.range(-0.01, 0.01));
            let tip = top + Vec3::new(self.range(-0.02, 0.02), 0.0, self.range(-0.02, 0.02));
            self.tube(
                (head, bend),
                ((0.45 * stem, 0.3 * stem), (0.0, 0.0)),
                (stems, part(key, 66)),
            )?;
            self.tube(
                (bend, tip),
                ((0.3 * stem, 0.0), (0.0, 0.0)),
                (stems, part(key, 67)),
            )?;
        }
        Some(())
    }

    /// A reedmace's spike from `low` up to `high`, `girth` at its fullest: a
    /// dense velvet cylinder blunt at either end, never quite straight nor
    /// even, which bursts in tufts of fluff over the winter.
    fn spike(&mut self, (low, high): (Vec3, Vec3), girth: f64, key: u32) -> Option<()> {
        let heads = self.marsh.heads;
        let length = (high - low).length();
        let bow = Vec3::new(self.range(-1.0, 1.0), 0.0, self.range(-1.0, 1.0)) * (0.05 * length);
        // Fullest a little below its middle, where its seeds ripened first.
        let fullest = self.range(0.4, 0.55);
        let mut joints = [(low, girth); SPIKE + 1];
        for (joint, held) in joints.iter_mut().enumerate() {
            let t = f64::from(whole(joint)) / f64::from(whole(SPIKE));
            let shaped = if t < fullest {
                0.5 * t / fullest
            } else {
                0.5 + 0.5 * (t - fullest) / (1.0 - fullest)
            };
            let full = 0.62 + 0.38 * power(mathf::sin(PI * shaped), 0.35);
            *held = (low.lerp(high, t) + bow * mathf::sin(PI * t), girth * full);
        }
        for (piece, ends) in joints.windows(2).enumerate() {
            let (&(from, thick), &(to, thin)) = (ends.first()?, ends.get(1)?);
            let along = |at: usize| length * f64::from(whole(at)) / f64::from(whole(SPIKE));
            self.tube(
                (from, to),
                ((thick, thin), (along(piece), along(piece + 1))),
                (heads, key),
            )?;
        }
        if self.season == Season::Winter {
            let fluff = self.marsh.sepals;
            for tuft in 0..FLUFF {
                let t = self.range(0.15, 0.85);
                let around = TAU * self.unit();
                let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
                let base = low.lerp(high, t) + bow * mathf::sin(PI * t) + out * (0.8 * girth);
                let axis = (out + Vec3::UP * self.range(-0.2, 0.5)).normalized();
                let normal = axis.cross(Vec3::UP.cross(out)).normalized();
                let reach = self.range(0.03, 0.07);
                self.push(Part::Leaf(Blade {
                    base: singles(base),
                    normal: singles(if normal.y < 0.0 { -normal } else { normal }),
                    axis: singles(axis),
                    length: single(reach),
                    width: single(0.5 * reach),
                    outline: Outline::Fascicle { count: 11 },
                    fold: 0.0,
                    material: fluff,
                    key: part(key, 1 + tuft),
                }))?;
            }
        }
        Some(())
    }

    /// A water-crowfoot plant rising at `at`: its stems streaming down the
    /// current, the patch's `z`, just beneath the surface and swaying from
    /// side to side, a tuft of thread-fine leaves at each joint, and, if
    /// `flowering`, a white flower held on the water at a stem's end.
    fn crowfoot(&mut self, at: Vec3, key: u32, flowering: bool) -> Option<()> {
        let stems = self.marsh.stems;
        let mut index = 0;
        for streamer in 0..STREAMERS {
            let length = self.range(0.5, 1.1) * self.stature;
            let piece = length / f64::from(STREAMER_PIECES);
            let mut point = at
                + Vec3::new(
                    self.range(-0.06, 0.06),
                    -self.range(0.015, 0.045),
                    self.range(-0.06, 0.06),
                );
            let mut sway = self.range(-0.25, 0.25);
            for _ in 0..STREAMER_PIECES {
                sway = (sway + self.range(-0.3, 0.3)).clamp(-0.6, 0.6);
                let axis = Vec3::new(sway, 0.0, 1.0).normalized();
                let next = point + axis * piece;
                index += 1;
                self.tube(
                    (point, next),
                    ((0.0018, 0.0014), (0.0, 0.0)),
                    (stems, part(key, index)),
                )?;
                for _ in 0..TUFT {
                    let spread = Vec3::new(self.range(-0.7, 0.7), self.range(-0.08, 0.02), 0.0);
                    let out = (axis + spread).normalized();
                    let across = Vec3::UP.cross(out).normalized();
                    let normal = across.cross(out).normalized();
                    let thread = self.range(0.04, 0.08);
                    index += 1;
                    self.push(Part::Leaf(Blade {
                        base: singles(next),
                        normal: singles(if normal.y < 0.0 { -normal } else { normal }),
                        axis: singles(out),
                        length: single(thread),
                        width: single(0.0025),
                        outline: Outline::Strap { from: 0, to: 255 },
                        fold: 0.0,
                        material: self.marsh.leaves,
                        key: part(key, index),
                    }))?;
                }
                point = next;
            }
            if flowering && streamer == 0 {
                self.blossom(
                    Vec3::new(point.x, at.y + 0.004, point.z),
                    part(key, 1 + index),
                )?;
            }
        }
        Some(())
    }

    /// A crowfoot's flower held on the water at `at`: its broad white petals,
    /// never quite alike nor evenly set, cupped about a domed head of green
    /// carpels ringed by its stamens.
    fn blossom(&mut self, at: Vec3, key: u32) -> Option<()> {
        let size = self.range(0.009, 0.013);
        let turn = TAU * self.unit();
        let heads = self.marsh.heads;
        for petal in 0..BLOSSOM_PETALS {
            let around = turn
                + TAU * (f64::from(petal) + self.range(-0.12, 0.12)) / f64::from(BLOSSOM_PETALS);
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let bend = Bend {
                rise: self.range(0.08, 0.35),
                bow: self.range(-0.2, 0.25),
                twist: self.range(-0.25, 0.25),
                cup: self.range(0.15, 0.4),
            };
            let length = size * self.range(0.85, 1.12);
            let key = lily::aged(part(key, 1 + petal), self.range(0.1, 0.45));
            self.sheet(
                Vec3::UP,
                ((at + out * (0.12 * size), out), (length, bend)),
                (Sheet::Broad, key, heads),
                &[],
            )?;
        }
        let carpels = CARPELS * size;
        self.carpels(at + Vec3::UP * (0.03 * size), carpels, key)?;
        let hearts = self.marsh.hearts;
        for stamen in 0..BLOSSOM_STAMENS {
            let around = turn + GOLDEN_ANGLE * f64::from(stamen);
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let bend = Bend {
                rise: self.range(0.8, 1.2),
                bow: self.range(0.1, 0.4),
                twist: self.range(-0.3, 0.3),
                cup: self.range(0.1, 0.25),
            };
            let length = size * self.range(0.2, 0.3);
            let key = lily::aged(part(key, 16 + stamen), self.range(0.1, 0.4));
            self.sheet(
                Vec3::UP,
                ((at + out * (1.05 * carpels), out), (length, bend)),
                (Sheet::Stamen, key, hearts),
                &[],
            )?;
        }
        Some(())
    }

    /// A crowfoot's head of carpels at `base`, `radius` across: a dome of
    /// them, each swelling on it, reckoned by how far out from its top a
    /// point lies.
    fn carpels(&mut self, base: Vec3, radius: f64, key: u32) -> Option<()> {
        let spokes = self.grain.carpels;
        let mut points = Vec::new();
        points
            .try_reserve_exact(wide(1 + whole(DOME.len()) * spokes))
            .ok()?;
        let mut coords = Vec::new();
        coords.try_reserve_exact(points.capacity()).ok()?;
        points.push(base + Vec3::UP * radius);
        coords.push([0.0; 2]);
        for (ring, &(out, rise)) in DOME.iter().enumerate() {
            for spoke in 0..spokes {
                let angle =
                    TAU * (f64::from(spoke) + 0.5 * f64::from(whole(ring) % 2)) / f64::from(spokes);
                // Each carpel swells on its own.
                let swell = 1.0 + self.range(-0.2, 0.25);
                let lift = rise + self.range(-0.08, 0.08);
                let way = Vec3::new(mathf::cos(angle), 0.0, mathf::sin(angle));
                points.push(base + way * (out * radius * swell) + Vec3::UP * (lift * radius));
                coords.push([single(out * radius), 0.0]);
            }
        }
        let hearts = self.marsh.hearts;
        let mut faces = Vec::new();
        faces
            .try_reserve_exact(wide(spokes + (whole(DOME.len()) - 1) * spokes * 2))
            .ok()?;
        let at = |ring: u32, spoke: u32| 1 + ring * spokes + spoke % spokes;
        for spoke in 0..spokes {
            faces.push(([0, at(0, spoke + 1), at(0, spoke)], hearts));
        }
        for ring in 0..whole(DOME.len()) - 1 {
            for spoke in 0..spokes {
                let (a, b) = (at(ring, spoke), at(ring, spoke + 1));
                let (c, d) = (at(ring + 1, spoke + 1), at(ring + 1, spoke));
                faces.push(([a, b, c], hearts));
                faces.push(([a, c, d], hearts));
            }
        }
        self.assembly.mesh_mapped(
            &points,
            &faces,
            Mapping {
                coords: &coords,
                key: lily::aged(part(key, 0), 0.2),
                size: radius,
                trim: None,
            },
        )
    }

    /// A rosette of floating pondweed leaves spreading from `at`, where their
    /// stem rises, each a few millimetres apart in height.
    fn pondweed(&mut self, at: Vec3, key: u32) -> Option<()> {
        let leaves =
            u32::from(ROSETTE) - u32::from(self.unit() < 0.5) - u32::from(self.unit() < 0.7);
        let turn = TAU * self.unit();
        for leaf in 0..leaves {
            let length = self.range(0.06, 0.11) * self.stature;
            let around = turn + TAU * (f64::from(leaf) + self.range(-0.2, 0.2)) / f64::from(leaves);
            let lift = 0.002 + 0.004 * self.unit();
            let axis = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            self.push(Part::Leaf(Blade {
                base: singles(at + axis * (0.1 * length) + Vec3::UP * lift),
                normal: singles(Vec3::UP),
                axis: singles(axis),
                length: single(length),
                width: single(0.42 * length),
                outline: Outline::Ovate { teeth: 0 },
                fold: 0.1,
                material: self.marsh.leaves,
                key: part(key, leaf),
            }))?;
        }
        Some(())
    }
}

/// A pad younger than this still unrolls from its bud.
const YOUNG: f64 = 0.08;
/// The share of grown summer pads crowding has raised off the water on their
/// stalks.
const RAISED: f64 = 0.06;
/// How far above another a pad rests where it lies over it, and how far an
/// upper lobe lies over the lower where they overlap.
const REST: f64 = 0.0025;
const OVERLAP_LIFT: f64 = 0.003;
/// How deep a stalk is drawn beneath the water, deeper than any water's
/// colour lets an eye follow it.
const STALK_DEPTH: f64 = 0.3;
/// How many places round its pad a flower looks for open water.
const SITES: u32 = 6;

/// How a pad lies: how high its blade floats at its stalk, how far it is
/// held above the water and cupped there if crowding raised it; its rim
/// lifted in arcs, each a height, a count round it and where they start; its
/// blade's broad waves and its margin's fine ripples likewise; how far its
/// lobes are carried past each other, in radians, apart where negative; how
/// deep its low side has sunk as it dies, and which way; and how far its
/// sides are still rolled from the bud, in its radius.
#[derive(Copy, Clone, Debug)]
struct Lie {
    float: f64,
    raised: f64,
    cup: f64,
    rim: (f64, f64, f64),
    waves: (f64, f64, f64),
    ripples: (f64, f64, f64),
    overlap: f64,
    sink: (f64, f64),
    roll: f64,
}

/// A pad laid on the water: where its stalk meets it and how it is turned,
/// its radius, how it lies, the pad its key draws, and that key.
#[derive(Copy, Clone, Debug)]
struct Laid {
    centre: (f64, f64),
    turn: f64,
    radius: f64,
    lie: Lie,
    pad: Pad,
    key: u32,
}

impl Laid {
    /// How far its lobes are carried past one another `r` out from its
    /// stalk: not at all at the stalk, and all of the way by its margin.
    fn spread(&self, r: f64) -> f64 {
        self.lie.overlap * smoothstep(0.15, 0.7, r)
    }

    /// How far round its own flat coordinates' turning the water's own
    /// turning is at `r` out: its lobes spread as they are there.
    fn fold(&self, r: f64) -> f64 {
        (PI + self.spread(r)) / (PI - SINUS)
    }

    /// Where on the water flat `(r, angle)` of its own lies.
    fn place(&self, (r, angle): (f64, f64)) -> (f64, f64) {
        let laid = angle * self.fold(r) + self.turn;
        let out = r * self.radius;
        (
            self.centre.0 + out * mathf::cos(laid),
            self.centre.1 + out * mathf::sin(laid),
        )
    }

    /// Its blade's height above the water at flat `(r, angle)` of its own,
    /// as it would lie alone.
    fn height(&self, (r, angle): (f64, f64)) -> f64 {
        let lie = &self.lie;
        let edge = power(smoothstep(0.55, 1.0, r), 1.6);
        let rim =
            lie.rim.0 * edge * (0.3 + 0.35 * (1.0 + mathf::cos(lie.rim.1 * angle + lie.rim.2)));
        let waves = lie.waves.0
            * power(r, 2.5)
            * 0.5
            * (1.0 + mathf::sin(lie.waves.1 * angle + lie.waves.2));
        let fringe = smoothstep(0.7, 1.0, r);
        let ripples = lie.ripples.0
            * fringe
            * fringe
            * 0.5
            * (1.0 + mathf::sin(lie.ripples.1 * angle + lie.ripples.2));
        let sink = -lie.sink.0 * r * mathf::cos(angle - lie.sink.1).max(0.0);
        // Rolled up from either side of the line through its sinus.
        let rolled = smoothstep(0.15, 1.0, (r * mathf::sin(angle)).abs());
        let roll = lie.roll * self.radius * rolled * rolled;
        let cup = lie.cup * self.radius * r * r;
        lie.float
            + lie.raised
            + cup
            + rim
            + waves
            + ripples
            + sink
            + roll
            + self.overlapping((r, angle))
    }

    /// How far its upper lobe is lifted to lie over its lower where its
    /// lobes overlap.
    fn overlapping(&self, (r, angle): (f64, f64)) -> f64 {
        let spread = self.spread(r);
        if spread <= 0.0 {
            return 0.0;
        }
        let over = (PI - spread) / self.fold(r);
        OVERLAP_LIFT * smoothstep(over - 0.15, over, angle) * spread / self.lie.overlap
    }

    /// Its height above the water at `(x, z)`, as it would lie alone, if
    /// its margin reaches there: the upper lobe's where its lobes overlap.
    fn height_at(&self, (x, z): (f64, f64)) -> Option<f64> {
        let (dx, dz) = (x - self.centre.0, z - self.centre.1);
        let reach = self.radius * self.pad.most();
        if dx * dx + dz * dz > reach * reach {
            return None;
        }
        let r = mathf::hypot(dx, dz) / self.radius;
        let fold = self.fold(r);
        let laid = wrapped(mathf::atan2(dz, dx) - self.turn);
        let mut top: Option<f64> = None;
        for turns in [0.0, -TAU, TAU] {
            let angle = (laid + turns) / fold;
            if angle.abs() <= PI - SINUS && r <= self.pad.reach(angle) {
                let height = self.height((r, angle));
                top = Some(top.map_or(height, |held| held.max(height)));
            }
        }
        top
    }

    /// Where flat `(r, angle)` of it lies, resting on the pads `under` it.
    fn point(&self, (r, angle): (f64, f64), under: &[Laid]) -> Vec3 {
        let (x, z) = self.place((r, angle));
        rested(Vec3::new(x, self.height((r, angle)), z), under)
    }
}

/// How high the pads of `laid` lie at `at`, if any reaches it: each resting
/// on those laid before it.
fn resting(laid: &[Laid], at: (f64, f64)) -> Option<f64> {
    laid.iter()
        .fold(None, |under, pad| match (pad.height_at(at), under) {
            (Some(own), Some(under)) => Some(own.max(under + REST)),
            (Some(own), None) => Some(own),
            (None, under) => under,
        })
}

/// `point`, lifted to rest on the pads of `under` where they lie beneath it.
fn rested(point: Vec3, under: &[Laid]) -> Vec3 {
    match resting(under, (point.x, point.z)) {
        Some(height) => Vec3::new(point.x, point.y.max(height + REST), point.z),
        None => point,
    }
}

/// A patch's lilies as they are sown: the pads floating on the water, in the
/// order they are laid; those that rest on them but bear none — raised on
/// their stalks, or still unrolling; and the flowers among them, each where
/// its plant roots, how far from there its pad reaches, and its key.
#[derive(Debug, Default)]
struct Lilies {
    floating: Vec<Laid>,
    standing: Vec<Laid>,
    flowers: Vec<((f64, f64), f64, u32)>,
}

/// How far a lily's flower is through its life: still a bud, its sepals
/// parting, wide open, sagging open as it fades, or closed again and
/// sinking.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    Bud,
    Opening,
    Open,
    Old,
    Spent,
}

/// How a lily's flowers are shared among their stages, in a summer's bed.
const STAGES: [(Stage, f64); 5] = [
    (Stage::Bud, 0.2),
    (Stage::Opening, 0.15),
    (Stage::Open, 0.37),
    (Stage::Old, 0.18),
    (Stage::Spent, 0.1),
];

/// A lily's flower as it is grown: where its base floats and the way it
/// faces, its size, its stage and how old it is, how its parts are turned
/// about it, and its key.
#[derive(Copy, Clone, Debug)]
struct Flower {
    base: Vec3,
    axis: Vec3,
    size: f64,
    stage: Stage,
    age: f64,
    turn: f64,
    key: u32,
}

impl Flower {
    /// The two ways about its axis it reckons its turning in.
    fn round(&self) -> (Vec3, Vec3) {
        let (first, _) = round(self.axis);
        (first, first.cross(self.axis))
    }

    /// The way out from its axis `around` radians round it.
    fn out(&self, around: f64) -> Vec3 {
        let (first, second) = self.round();
        first * mathf::cos(around) + second * mathf::sin(around)
    }

    /// Where its heart sits: its stigma, ringed by its stamens.
    fn heart(&self) -> Vec3 {
        self.base + self.axis * (0.21 * self.size)
    }

    /// How much shorter than when open its parts are at its stage.
    fn shrunk(&self) -> f64 {
        match self.stage {
            Stage::Bud => 0.65,
            Stage::Spent => 0.8,
            Stage::Opening | Stage::Open | Stage::Old => 1.0,
        }
    }
}

/// How a sepal or a petal is bent: rising `rise` radians from its flower's
/// level at its base and `bow` more by its tip, twisting `twist` about its
/// length by then, and cupped `cup` of its half-width toward its inner
/// face.
#[derive(Copy, Clone, Debug)]
struct Bend {
    rise: f64,
    bow: f64,
    twist: f64,
    cup: f64,
}

/// The least a sheet is across at its tip, in its breadth, and how far past
/// its outline its mesh reaches, so the outline it is cut to draws its edge
/// rather than its mesh's.
const TIP: f64 = 0.06;
const BEYOND: f64 = 1.12;
/// How much closer a sheet's rows are drawn by its tip than by its base.
const TIPWARD: f64 = 1.4;
/// The radius of a flower's ovary, in the flower's size: its sepals, petals
/// and stamens are set round it.
const OVARY: f64 = 0.17;

impl Grower {
    /// Sow a lily rooted at `at` among `lilies`: its pad, as old as the
    /// season makes it, and its flower if it is `flowering`.
    fn sow(&mut self, lilies: &mut Lilies, at: Vec3, key: u32, flowering: bool) -> Option<()> {
        let age = self.pad_age();
        let young = 1.0 - smoothstep(0.0, YOUNG, age);
        let radius = self.range(0.08, 0.15) * self.stature * (1.0 - 0.45 * young);
        let lie = self.lie(radius, age);
        let key = lily::aged(key, age);
        let laid = Laid {
            centre: (at.x, at.z),
            turn: TAU * self.unit(),
            radius,
            lie,
            pad: Pad::of(key),
            key,
        };
        let pads = if lie.raised > 0.0 || lie.roll > 0.0 {
            &mut lilies.standing
        } else {
            &mut lilies.floating
        };
        pads.try_reserve(1).ok()?;
        pads.push(laid);
        if flowering {
            lilies.flowers.try_reserve(1).ok()?;
            lilies.flowers.push(((at.x, at.z), radius, key));
        }
        Some(())
    }

    /// How far through its life a pad sown in the patch's season is: most
    /// unrolling in spring, grown in summer with the first of them dying,
    /// yellowing and dying through the autumn.
    fn pad_age(&mut self) -> f64 {
        let draw = self.unit();
        match self.season {
            Season::Spring if draw < 0.45 => self.range(0.0, YOUNG),
            Season::Spring => self.range(YOUNG, 0.4),
            Season::Summer if draw < 0.07 => self.range(0.0, YOUNG),
            Season::Summer if draw < 0.88 => self.range(0.12, 0.55),
            Season::Summer => self.range(0.55, 0.92),
            Season::Autumn { .. } if draw < 0.12 => self.range(0.3, 0.6),
            Season::Autumn { .. } if draw < 0.62 => self.range(0.6, 0.85),
            Season::Autumn { .. } => self.range(0.85, 1.0),
            Season::Winter => self.range(0.9, 1.0),
        }
    }

    /// How a pad `radius` across and as old as `age` lies: young, its sides
    /// still rolled; grown, flat on the water, its rim lifted here and
    /// there, or crowded up off it in summer; old, its rim curling and its
    /// margin waved; dying, sinking on one side.
    fn lie(&mut self, radius: f64, age: f64) -> Lie {
        let young = 1.0 - smoothstep(0.0, YOUNG, age);
        let old = smoothstep(0.55, 1.0, age);
        let curled = self.unit() < 0.15 + 0.3 * old;
        let raised =
            if young <= 0.0 && age < 0.7 && self.season == Season::Summer && self.unit() < RAISED {
                self.range(0.012, 0.04)
            } else {
                0.0
            };
        let cup = if raised > 0.0 {
            self.range(-0.1, 0.15)
        } else {
            0.0
        };
        let lifted = if curled {
            self.range(0.06, 0.16)
        } else {
            self.range(0.0, 0.035)
        };
        Lie {
            float: self.range(0.0012, 0.0025),
            raised,
            cup,
            rim: (
                radius * lifted,
                f64::from(1 + self.below(3)),
                self.range(0.0, TAU),
            ),
            waves: (
                radius * (self.range(0.0, 0.025) + 0.04 * old),
                f64::from(4 + self.below(5)),
                self.range(0.0, TAU),
            ),
            ripples: (
                radius * (self.range(0.002, 0.012) + 0.01 * old),
                f64::from(9 + self.below(7)),
                self.range(0.0, TAU),
            ),
            overlap: self.range(-0.14, 0.3),
            sink: (0.025 * smoothstep(0.85, 1.0, age), self.range(0.0, TAU)),
            roll: 0.6 * young,
        }
    }

    /// Lay the lilies sown: each floating pad resting on those before it,
    /// then those standing on them, then the flowers among them all.
    fn lilies(&mut self, lilies: &Lilies) -> Option<()> {
        for (index, laid) in lilies.floating.iter().enumerate() {
            self.pad(laid, lilies.floating.get(..index)?)?;
        }
        for laid in &lilies.standing {
            self.pad(laid, &lilies.floating)?;
        }
        for &(root, radius, key) in &lilies.flowers {
            self.flower((root, 1.1 * radius, key), &lilies.floating)?;
        }
        Some(())
    }

    /// The mesh of the pad `laid`, resting on the pads `under` it, and the
    /// stalk it floats on.
    fn pad(&mut self, laid: &Laid, under: &[Laid]) -> Option<()> {
        let (rings, spokes) = (self.grain.rings, self.grain.spokes);
        let (parts, vertices) = self.grain.pad_room();
        let mut points = Vec::new();
        points.try_reserve_exact(vertices).ok()?;
        let mut coords = Vec::new();
        coords.try_reserve_exact(vertices).ok()?;
        points.push(laid.point((0.0, 0.0), under));
        coords.push([0.0; 2]);
        for &ring in rings {
            for spoke in 0..=spokes {
                let angle = (PI - SINUS) * (2.0 * f64::from(spoke) / f64::from(spokes) - 1.0);
                let r = ring * laid.pad.reach(angle);
                points.push(laid.point((r, angle), under));
                let out = r * laid.radius;
                coords.push([
                    single(out * mathf::cos(angle)),
                    single(out * mathf::sin(angle)),
                ]);
            }
        }
        let leaves = self.marsh.leaves;
        let mut faces = Vec::new();
        faces.try_reserve_exact(parts).ok()?;
        let at = |ring: u32, spoke: u32| 1 + ring * (spokes + 1) + spoke;
        for spoke in 0..spokes {
            faces.push(([0, at(0, spoke + 1), at(0, spoke)], leaves));
        }
        for ring in 0..whole(rings.len()) - 1 {
            for spoke in 0..spokes {
                let (a, b) = (at(ring, spoke), at(ring, spoke + 1));
                let (c, d) = (at(ring + 1, spoke + 1), at(ring + 1, spoke));
                faces.push(([a, b, c], leaves));
                faces.push(([a, c, d], leaves));
            }
        }
        self.assembly.mesh_mapped(
            &points,
            &faces,
            Mapping {
                coords: &coords,
                key: laid.key,
                size: laid.radius,
                trim: Some(Trim::Pad),
            },
        )?;
        let middle = points.first()?;
        self.stalk(*middle, part(laid.key, 1))
    }

    /// The stalk a pad's blade or a flower rises on to its underside at
    /// `head`, leaning down from it toward its root and on below where the
    /// eye can follow it, its key `key`.
    fn stalk(&mut self, head: Vec3, key: u32) -> Option<()> {
        if !self.grain.stalks {
            return Some(());
        }
        let thick = self.range(0.0028, 0.0042);
        let away = TAU * self.unit();
        let lean = Vec3::new(mathf::cos(away), 0.0, mathf::sin(away)) * self.range(0.05, 0.16);
        // Its rounded top kept beneath the blade.
        let top = head - Vec3::UP * (thick + 0.0008);
        let knee = top + lean * 0.35 - Vec3::UP * (0.4 * (top.y + STALK_DEPTH));
        let foot = Vec3::new(top.x + lean.x, -STALK_DEPTH, top.z + lean.z);
        let (upper, lower) = ((knee - top).length(), (foot - knee).length());
        let stems = self.marsh.stems;
        self.tube(
            (top, knee),
            ((thick, 1.05 * thick), (0.0, upper)),
            (stems, key),
        )?;
        self.tube(
            (knee, foot),
            ((1.05 * thick, 1.1 * thick), (upper, upper + lower)),
            (stems, key),
        )
    }

    /// A lily's flower beside its pad, its plant rooted at `root` and its
    /// pad reaching `reach` from there, keyed `key`: on open water if there
    /// is any about the pad, else resting on the pads of `floor` there.
    fn flower(&mut self, (root, reach, key): ((f64, f64), f64, u32), floor: &[Laid]) -> Option<()> {
        let stage = self.stage();
        let size = self.range(0.045, 0.07) * (0.75 + 0.25 * self.stature);
        let centre = self.site(root, reach + 0.4 * size, floor);
        let resting_on = resting(floor, centre).map_or(0.0, |height| height + REST);
        let (age, rise) = match stage {
            Stage::Bud => (0.0, self.range(-0.02, 0.025)),
            Stage::Opening => (self.range(0.05, 0.15), 0.002),
            Stage::Open => (self.range(0.2, 0.5), 0.002),
            Stage::Old => (self.range(0.6, 0.85), 0.0),
            Stage::Spent => (self.range(0.9, 1.0), self.range(-0.025, -0.008)),
        };
        let lean = if stage == Stage::Bud { 0.45 } else { 0.18 };
        let tilt = self.range(0.0, lean);
        let toward = TAU * self.unit();
        let axis = Vec3::new(
            mathf::sin(tilt) * mathf::cos(toward),
            mathf::cos(tilt),
            mathf::sin(tilt) * mathf::sin(toward),
        );
        let flower = Flower {
            base: Vec3::new(centre.0, resting_on + rise, centre.1),
            axis,
            size,
            stage,
            age,
            turn: TAU * self.unit(),
            key: lily::aged(key, age),
        };
        self.sepals(&flower, floor)?;
        self.petals(&flower, floor)?;
        if stage != Stage::Bud {
            self.stamens(&flower, floor)?;
            self.stigma(&flower)?;
        }
        self.stalk(flower.base - flower.axis * 0.004, part(flower.key, 1))
    }

    /// The stage a flower is drawn at.
    fn stage(&mut self) -> Stage {
        let mut draw = self.unit();
        for (stage, share) in STAGES {
            if draw < share {
                return stage;
            }
            draw -= share;
        }
        Stage::Open
    }

    /// Where a flower floats `reach` from `root`: the first of a few places
    /// round it on open water, else the one where the pads lie lowest.
    fn site(&mut self, root: (f64, f64), reach: f64, floor: &[Laid]) -> (f64, f64) {
        let start = TAU * self.unit();
        let mut best = (root, f64::INFINITY);
        for trial in 0..SITES {
            let angle = start + TAU * f64::from(trial) / f64::from(SITES);
            let at = (
                root.0 + reach * mathf::cos(angle),
                root.1 + reach * mathf::sin(angle),
            );
            let Some(height) = resting(floor, at) else {
                return at;
            };
            if height < best.1 {
                best = (at, height);
            }
        }
        best.0
    }

    /// A flower's four sepals: closed over its bud, parting, then spread on
    /// the water once it opens.
    fn sepals(&mut self, flower: &Flower, floor: &[Laid]) -> Option<()> {
        for sepal in 0..SEPALS {
            let bend = match flower.stage {
                Stage::Bud => Bend {
                    rise: self.range(1.15, 1.35),
                    bow: self.range(0.55, 0.85),
                    twist: self.range(-0.1, 0.1),
                    cup: self.range(0.5, 0.65),
                },
                Stage::Opening => Bend {
                    rise: self.range(0.6, 0.9),
                    bow: self.range(0.1, 0.35),
                    twist: self.range(-0.2, 0.2),
                    cup: self.range(0.35, 0.5),
                },
                Stage::Open => Bend {
                    rise: self.range(0.03, 0.15),
                    bow: self.range(-0.15, 0.05),
                    twist: self.range(-0.25, 0.25),
                    cup: self.range(0.2, 0.35),
                },
                Stage::Old => Bend {
                    rise: self.range(-0.02, 0.08),
                    bow: self.range(-0.25, 0.0),
                    twist: self.range(-0.3, 0.3),
                    cup: self.range(0.15, 0.3),
                },
                Stage::Spent => Bend {
                    rise: self.range(0.7, 1.1),
                    bow: self.range(0.2, 0.5),
                    twist: self.range(-0.3, 0.3),
                    cup: self.range(0.4, 0.6),
                },
            };
            let around = flower.turn + FRAC_PI_2 * f64::from(sepal) + self.range(-0.12, 0.12);
            let out = flower.out(around);
            let base =
                flower.base + out * (OVARY * flower.size) + flower.axis * (0.01 * flower.size);
            let length = flower.size * self.range(0.85, 1.0) * flower.shrunk();
            let key = lily::aged(
                part(flower.key, 2 + sepal),
                flower.age + self.range(0.0, 0.1),
            );
            let sepals = self.marsh.sepals;
            self.sheet(
                flower.axis,
                ((base, out), (length, bend)),
                (Sheet::Sepal, key, sepals),
                floor,
            )?;
        }
        Some(())
    }

    /// A flower's petals in a spiral, the outer longest and lowest; a bud's
    /// few showing between its sepals, an old flower's shedding some onto
    /// the water about it, a spent one's collapsing and lost.
    fn petals(&mut self, flower: &Flower, floor: &[Laid]) -> Option<()> {
        let (fewest, most) = if flower.stage == Stage::Bud {
            (8, 8)
        } else {
            self.grain.petals
        };
        let count = fewest + self.below(most - fewest + 1);
        let lost = match flower.stage {
            Stage::Old => 0.2,
            Stage::Spent => 0.35,
            Stage::Bud | Stage::Opening | Stage::Open => 0.0,
        };
        let heads = self.marsh.heads;
        for petal in 0..count {
            let inward = f64::from(petal) / f64::from(count.max(2) - 1);
            let around =
                flower.turn + 0.785 + GOLDEN_ANGLE * f64::from(petal) + self.range(-0.1, 0.1);
            let length =
                flower.size * (1.0 - 0.38 * inward) * self.range(0.86, 1.08) * flower.shrunk();
            let bend = self.petal_bend(flower.stage, inward);
            let key = lily::aged(
                part(flower.key, 8 + petal),
                flower.age + self.range(-0.06, 0.12),
            );
            if self.unit() < lost {
                continue;
            }
            // Set round the ovary's wall, the inner higher up it.
            let out = flower.out(around);
            let base = flower.base
                + out * (flower.size * (OVARY + 0.05 * (1.0 - inward)))
                + flower.axis * (flower.size * (0.02 + 0.13 * inward));
            self.sheet(
                flower.axis,
                ((base, out), (length, bend)),
                (Sheet::Petal, key, heads),
                floor,
            )?;
        }
        if flower.stage == Stage::Old {
            for shed in 0..=self.below(SHED) {
                let away = TAU * self.unit();
                let out = Vec3::new(mathf::cos(away), 0.0, mathf::sin(away));
                let drift = flower.size * self.range(0.7, 1.4);
                let base = Vec3::new(
                    flower.base.x + drift * mathf::cos(away + 0.6),
                    0.0015,
                    flower.base.z + drift * mathf::sin(away + 0.6),
                );
                let bend = Bend {
                    rise: self.range(-0.01, 0.04),
                    bow: self.range(-0.1, 0.1),
                    twist: self.range(-0.3, 0.3),
                    cup: self.range(0.1, 0.3),
                };
                let length = flower.size * self.range(0.7, 1.0);
                let key = lily::aged(part(flower.key, 40 + shed), self.range(0.85, 1.0));
                self.sheet(
                    Vec3::UP,
                    ((base, out), (length, bend)),
                    (Sheet::Petal, key, heads),
                    floor,
                )?;
            }
        }
        Some(())
    }

    /// How a petal `inward` of the way from a flower's outermost to its
    /// innermost is bent at `stage`.
    fn petal_bend(&mut self, stage: Stage, inward: f64) -> Bend {
        let (rise, bow, cup) = match stage {
            Stage::Bud => (1.35 + 0.1 * inward, 0.5, self.range(0.45, 0.6)),
            Stage::Opening => (
                0.85 + 0.55 * inward + self.range(-0.1, 0.1),
                self.range(0.2, 0.4),
                self.range(0.35, 0.5),
            ),
            Stage::Open => (
                0.18 + 0.95 * power(inward, 0.8) + self.range(-0.15, 0.15),
                self.range(-0.2, 0.35),
                self.range(0.15, 0.45),
            ),
            Stage::Old => (
                -0.12 + 0.8 * inward + self.range(-0.15, 0.15),
                self.range(-0.55, 0.0),
                self.range(0.1, 0.35),
            ),
            Stage::Spent => (
                0.9 + 0.5 * inward,
                self.range(0.3, 0.6),
                self.range(0.4, 0.6),
            ),
        };
        Bend {
            rise,
            bow,
            twist: self.range(-0.35, 0.35),
            cup,
        }
    }

    /// A flower's stamens crowding about its stigma, each a narrow strap set
    /// on the ovary's wall, the outer longer, lower and spread wider, bending
    /// in toward its tip; splayed and fewer as it fades.
    fn stamens(&mut self, flower: &Flower, floor: &[Laid]) -> Option<()> {
        let lost = match flower.stage {
            Stage::Old => 0.25,
            Stage::Spent => 0.5,
            Stage::Bud | Stage::Opening | Stage::Open => 0.0,
        };
        let heart = flower.heart();
        let (size, axis) = (flower.size, flower.axis);
        let hearts = self.marsh.hearts;
        for stamen in 0..self.grain.stamens {
            let outer = self.unit();
            let around = flower.turn + GOLDEN_ANGLE * f64::from(stamen) + self.range(-0.05, 0.05);
            let length = size * (0.2 + 0.16 * outer) * self.range(0.85, 1.12) * flower.shrunk();
            let (rise, curl) = match flower.stage {
                Stage::Opening => (1.35 - 0.2 * outer, self.range(0.3, 0.6)),
                Stage::Old => (0.85 - 0.4 * outer, self.range(-0.2, 0.2)),
                Stage::Spent => (1.2, self.range(0.3, 0.6)),
                Stage::Bud | Stage::Open => (1.25 - 0.45 * outer, self.range(0.25, 0.6)),
            };
            let bend = Bend {
                rise: rise + self.range(-0.1, 0.1),
                bow: curl,
                twist: self.range(-0.25, 0.25),
                cup: self.range(0.1, 0.3),
            };
            let key = lily::aged(
                part(flower.key, 64 + stamen),
                flower.age + self.range(0.0, 0.1),
            );
            if self.unit() < lost {
                continue;
            }
            let out = flower.out(around);
            let foot = heart + out * (OVARY * size * (1.0 + 0.3 * outer))
                - axis * (size * (0.03 + 0.07 * outer));
            self.sheet(
                axis,
                ((foot, out), (length, bend)),
                (Sheet::Stamen, key, hearts),
                floor,
            )?;
        }
        Some(())
    }

    /// A flower's stigma: a shallow cup raised on its rim, rayed to the
    /// little horns its rim turns up in, a knob at its middle, and the top of
    /// the ovary beneath it, ribbed where its rays run down. Its surface is
    /// reckoned by how far out from its middle a point lies.
    fn stigma(&mut self, flower: &Flower) -> Option<()> {
        let radius = flower.size * self.range(0.14, 0.18);
        let rays = RAYS.0 + self.below(RAYS.1 - RAYS.0 + 1);
        let spokes = self.grain.per_ray * rays;
        let rings = self.grain.stigma;
        let (first, second) = flower.round();
        let centre = flower.heart();
        let mut points = Vec::new();
        points
            .try_reserve_exact(wide(1 + whole(rings.len()) * spokes))
            .ok()?;
        let mut coords = Vec::new();
        coords.try_reserve_exact(points.capacity()).ok()?;
        points.push(centre + flower.axis * (0.12 * radius));
        coords.push([0.0; 2]);
        for &(out, rise) in rings {
            let horned = if rise > 0.0 {
                smoothstep(0.85, 1.0, out)
            } else {
                0.0
            };
            for spoke in 0..spokes {
                let angle = TAU * f64::from(spoke) / f64::from(spokes);
                let ridge = power(mathf::cos(0.5 * f64::from(rays) * angle), 2.0);
                let height =
                    rise + 0.07 * ridge * smoothstep(0.2, 0.8, out) + 0.16 * horned * ridge * ridge;
                let way = first * mathf::cos(angle) + second * mathf::sin(angle);
                points.push(centre + way * (out * radius) + flower.axis * (height * radius));
                coords.push([single(out * radius), 0.0]);
            }
        }
        let hearts = self.marsh.hearts;
        let mut faces = Vec::new();
        faces
            .try_reserve_exact(wide(spokes + (whole(rings.len()) - 1) * spokes * 2))
            .ok()?;
        let at = |ring: u32, spoke: u32| 1 + ring * spokes + spoke % spokes;
        for spoke in 0..spokes {
            faces.push(([0, at(0, spoke + 1), at(0, spoke)], hearts));
        }
        for ring in 0..whole(rings.len()) - 1 {
            for spoke in 0..spokes {
                let (a, b) = (at(ring, spoke), at(ring, spoke + 1));
                let (c, d) = (at(ring + 1, spoke + 1), at(ring + 1, spoke));
                faces.push(([a, b, c], hearts));
                faces.push(([a, c, d], hearts));
            }
        }
        self.assembly.mesh_mapped(
            &points,
            &faces,
            Mapping {
                coords: &coords,
                key: lily::aged(part(flower.key, 0), flower.age),
                size: radius,
                trim: None,
            },
        )
    }

    /// A sepal, petal or stamen, as `sheet` says, of a flower facing `axis`,
    /// set at `base` and leaving it `out` from the axis, `length` long and
    /// bent as `bend` has it, keyed `key` and made in `material`; its inner
    /// face toward the axis, its front the face `sheet` shows outward, and
    /// resting on the pads of `floor` where it reaches over them. Its mesh
    /// reaches a little past its outline, which it is cut to.
    fn sheet(
        &mut self,
        axis: Vec3,
        ((base, out), (length, bend)): ((Vec3, Vec3), (f64, Bend)),
        (sheet, key, material): (Sheet, u32, u16),
        floor: &[Laid],
    ) -> Option<()> {
        let shape = Petal::of(key, sheet);
        let (rows, columns) = self.grain.grid(sheet);
        let (parts, vertices) = self.grain.sheet_room(sheet);
        let across = axis.cross(out).normalized();
        let mut points = Vec::new();
        points.try_reserve_exact(wide(vertices)).ok()?;
        let mut coords = Vec::new();
        coords.try_reserve_exact(wide(vertices)).ok()?;
        let heading = |u: f64| {
            let rise = bend.rise + bend.bow * u;
            out * mathf::cos(rise) + axis * mathf::sin(rise)
        };
        // Rows drawn closer toward the tip, where the outline turns fastest.
        let along = |row: u32| 1.0 - power(1.0 - f64::from(row) / f64::from(rows), TIPWARD);
        let mut spine = base;
        for row in 0..=rows {
            let u = along(row);
            let tangent = heading(u);
            let twist = bend.twist * u;
            let side = across * mathf::cos(twist) + tangent.cross(across) * mathf::sin(twist);
            let face = tangent.cross(side);
            for column in 0..=columns {
                let v = 2.0 * f64::from(column) / f64::from(columns) - 1.0;
                let half = shape
                    .half_width(u, if v < 0.0 { -1.0 } else { 1.0 })
                    .max(TIP * shape.breadth)
                    * length;
                let offset = v * half * BEYOND;
                let point = spine + side * offset + face * (bend.cup * half * v * v);
                points.push(rested(point, floor));
                coords.push([single(u * length), single(offset)]);
            }
            let next = along(row + 1);
            spine += heading(f64::midpoint(u, next)) * ((next - u) * length);
        }
        let mut faces = Vec::new();
        faces.try_reserve_exact(wide(parts)).ok()?;
        let at = |row: u32, column: u32| row * (columns + 1) + column;
        for row in 0..rows {
            for column in 0..columns {
                let (a, b) = (at(row, column), at(row + 1, column));
                let (c, d) = (at(row + 1, column + 1), at(row, column + 1));
                // A sepal fronts its outer face, the rest their inner.
                if sheet == Sheet::Sepal {
                    faces.push(([a, c, b], material));
                    faces.push(([a, d, c], material));
                } else {
                    faces.push(([a, b, c], material));
                    faces.push(([a, c, d], material));
                }
            }
        }
        self.assembly.mesh_mapped(
            &points,
            &faces,
            Mapping {
                coords: &coords,
                key,
                size: length,
                trim: Some(Trim::Sheet(sheet)),
            },
        )
    }
}

/// Where `along` of the way up `rachis`, its pieces each as long, lies, and
/// the way it runs there.
fn along_rachis(rachis: &[Vec3], along: f64) -> Option<(Vec3, Vec3)> {
    let pieces = rachis.len().checked_sub(1)?;
    let (piece, into) = cell_of(along * real(pieces));
    let piece = piece.min(pieces.checked_sub(1)?);
    let (from, to) = (*rachis.get(piece)?, *rachis.get(piece + 1)?);
    Some((from.lerp(to, into), (to - from).normalized()))
}

/// The key of part `index` of the plant keyed `key`: its own, but in the
/// plant's colour.
fn part(key: u32, index: u32) -> u32 {
    (mix32(key ^ index.wrapping_mul(0x9e37_79b9)) & !3) | (key & 3)
}

/// Where piece `segment` of a leaf in `segments` pieces begins, in 255ths of
/// the way along it.
fn segment_mark(segment: u8, segments: u8) -> u8 {
    u8::try_from(u16::from(segment) * 255 / u16::from(segments.max(1))).unwrap_or(u8::MAX)
}

#[cfg(test)]
#[path = "waterside_tests.rs"]
mod tests;
