//! Caustics: the sunlight water's waves focus onto what lies beneath them and
//! reflect onto what stands over them.
//!
//! A water surface is cut into beams: each triangle of a grid over it carries
//! the sunlight crossing it, bent by the waves at its corners, down to any
//! depth or up to any height, where it covers a triangle of its own (Watt,
//! "Light-Water Interaction using Backward Beam Tracing", 1990). A point
//! gathers the flux of every beam falling within the box the sun's disc and
//! the waves too fine to resolve blur a point into, over the box's area,
//! against what a level surface would send it: where beams converge it
//! brightens, where they spread it dims, and where they fold past a focus
//! every one of them still counts.
//!
//! Beams are laid over the tiles of water whose light reaches what a survey
//! of the picture finds, each tile cut at every level from two cells a side
//! to as fine as the finest point it lights asks, every level resolving the
//! waves its cells can; a point gathers at the level its own footprint asks,
//! blended with the next, so the detail changes smoothly from tile to tile.
//! A tile whose waves could not move the light by a hundredth is not laid.
//! A pyramid of each level's bounds finds the beams about a point.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::detail::Focus;
use crate::material::{fresnel, refract, Sweep};
use crate::scene::{Object, Scene, Sight, NEAR};
use crate::shape::Shape;
use crate::tone::Encoder;
use crate::trace::{lift, resolved, Tracer};
use crate::vector::{real, share, Pose, Ray, Vec3};

/// A tile's side, in metres.
const TILE: f64 = 2.0;

/// The finest a tile is cut: 2⁹ cells a side, a little under 4 mm each,
/// which resolves the shortest wave a breeze raises whole.
const FINEST: u32 = 9;

/// The least a tile's waves must move the light reaching its points, as a
/// share of it, for its beams to be laid.
const FAINTEST: f64 = 0.01;

/// How many standard deviations of the waves' slope bound how far a beam
/// strays from a level surface's.
const STRAY: f64 = 4.0;

/// The most a reflected beam drifts across, per metre it climbs, from a
/// level surface's beam as far as the waves' slopes swing it: past it a low
/// sun's glitter smears too far along its way to resolve, and the water
/// reflects its mean.
const MOST_SWING: f64 = 2.0;

/// The beams a reflected beam climbing slower than this is left out of: it
/// skims the water into the next wave.
const LEAST_CLIMB: f64 = 0.02;

/// The tilt a beam's drift per unit of slope is read over.
const TILT: f64 = 1e-4;

/// A uniform disc's radius and the half-width of a box blurring alike: the
/// box whose second moment the disc's has.
const DISC_TO_BOX: f64 = 0.866_025_403_784_438_6;

/// The least half-width of the box a point gathers over, in metres: below
/// it a point at the surface itself gathers its own beam alone.
const LEAST_GAUGE: f64 = 1e-5;

/// The most times a plan coarsens its footprints to fit the scene's room.
const MOST_COARSENINGS: u32 = 24;

/// About how many beams a point gathers at most: past a focus the beams it
/// gathers come from every glint over a patch of water as broad as the
/// waves' slopes swing them, so the cells it gathers at are no finer than
/// keeps this many of them in that patch; what they cannot resolve blurs the
/// light as the sun's disc already does at that distance.
const MOST_GATHERED: f64 = 256.0;

/// Points of the survey one core looks over in a unit of work, vertices of
/// the beams it fills, and nodes of the pyramids it seals.
const SURVEY_UNIT: usize = 128;
const FILL_UNIT: usize = 2048;
const SEAL_UNIT: usize = 8192;

/// The blocks at the foot of a level's pyramid one job seals, so a large
/// level is shared among the cores.
const SEAL_BAND: usize = 1024;

/// The side of a cell at level `level`.
fn cell(level: u32) -> f64 {
    TILE / real(1 << level)
}

/// The footprint the waves are resolved over at level `level`: every wave
/// six of its cells long, and none shorter than three.
fn cutoff(level: u32) -> f64 {
    2.0 * cell(level)
}

/// The level, among a tile's, whose cells are as fine as `fine`, as a real
/// number between whole levels: nought or less for a cell as coarse as a
/// tile, which resolves no wave and gathers a level surface's light.
fn level_of(fine: f64) -> f64 {
    mathf::ln(TILE / fine) / core::f64::consts::LN_2
}

/// The level a point seen `footprint` across gathers at: its cells the share
/// `texel` of the footprint, and never so fine that more than
/// [`MOST_GATHERED`] of them lie in the `patch` of glints it gathers from.
fn gathering_level(texel: f64, footprint: f64, patch: f64) -> f64 {
    level_of((texel * footprint).max(patch * mathf::sqrt(2.0 / MOST_GATHERED)))
}

/// The whole level a tile is cut to for a point that gathers at `wanted`:
/// the next finer, no finer than [`FINEST`]; `None` for a point gathering at
/// no level, whose light is a level surface's.
fn finest_level(wanted: f64) -> Option<u32> {
    if wanted.is_nan() || wanted <= 0.0 {
        return None;
    }
    let level = mathf::ceil(wanted).clamp(1.0, f64::from(FINEST));
    u32::try_from(mathf::round_i32(level)).ok()
}

/// One vertex of a tile's grid of beams: where the surface stands there and
/// what its waves do to the sunlight crossing it.
#[derive(Copy, Clone, Debug)]
struct Beam {
    /// The surface's height; NaN where no water stands.
    height: f32,
    /// How far a refracted beam drifts across for each metre it sinks, and a
    /// reflected one for each metre it climbs; NaN for a reflected beam that
    /// skims the water.
    drift: [[f32; 2]; 2],
    /// The flux the refracted and the reflected beam carry, each against a
    /// level surface's.
    flux: [f32; 2],
}

impl Beam {
    const DRY: Self = Self {
        height: f32::NAN,
        drift: [[f32::NAN; 2]; 2],
        flux: [0.0; 2],
    };
}

/// Which way a beam leaves the surface.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Way {
    /// Refracted into the water.
    Sunk,
    /// Reflected off it.
    Cast,
}

impl Way {
    const fn index(self) -> usize {
        match self {
            Self::Sunk => 0,
            Self::Cast => 1,
        }
    }

    /// How far a beam has gone from a surface at `surface` by the time it
    /// reaches the height `at`: down for a refracted one, up for a reflected.
    fn gone(self, surface: f64, at: f64) -> f64 {
        match self {
            Self::Sunk => surface - at,
            Self::Cast => at - surface,
        }
    }
}

/// The least and greatest a pyramid node's beams hold: where the surface
/// stands, and how far each way's beams drift along x and along z.
#[derive(Copy, Clone, Debug)]
struct Bounds {
    height: [f32; 2],
    drift: [[[f32; 2]; 2]; 2],
}

impl Bounds {
    const EMPTY: Self = Self {
        height: [f32::INFINITY, f32::NEG_INFINITY],
        drift: [[[f32::INFINITY, f32::NEG_INFINITY]; 2]; 2],
    };

    fn with(mut self, beam: &Beam) -> Self {
        if beam.height.is_nan() {
            return self;
        }
        self.height = [
            self.height[0].min(beam.height),
            self.height[1].max(beam.height),
        ];
        for (bounds, drift) in self.drift.iter_mut().zip(&beam.drift) {
            for (axis, &value) in bounds.iter_mut().zip(drift) {
                if !value.is_nan() {
                    *axis = [axis[0].min(value), axis[1].max(value)];
                }
            }
        }
        self
    }

    fn join(mut self, other: &Self) -> Self {
        self.height = [
            self.height[0].min(other.height[0]),
            self.height[1].max(other.height[1]),
        ];
        for (bounds, others) in self.drift.iter_mut().zip(&other.drift) {
            for (axis, other) in bounds.iter_mut().zip(others) {
                *axis = [axis[0].min(other[0]), axis[1].max(other[1])];
            }
        }
        self
    }

    /// Whether any beam of the node over the square `side` across from
    /// `corner` reaches `gauge` at the height `at`, going `way`.
    fn reaches(&self, gauge: &Gauge, (corner, side): ((f64, f64), f64), at: f64, way: Way) -> bool {
        let (low, high) = (f64::from(self.height[0]), f64::from(self.height[1]));
        if low > high {
            return false;
        }
        let (one, other) = (way.gone(low, at), way.gone(high, at));
        let (near, far) = (one.min(other).max(0.0), one.max(other));
        if far <= 0.0 {
            return false;
        }
        let drift = &self.drift[way.index()];
        let spread = |axis: usize| {
            let (least, most) = (f64::from(drift[axis][0]), f64::from(drift[axis][1]));
            if least > most {
                return None;
            }
            let products = [near * least, near * most, far * least, far * most];
            let low = products.iter().copied().fold(f64::INFINITY, f64::min);
            let high = products.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            Some((low, high))
        };
        let (Some((x_low, x_high)), Some((z_low, z_high))) = (spread(0), spread(1)) else {
            return false;
        };
        gauge.overlaps(
            (corner.0 + x_low, corner.0 + side + x_high),
            (corner.1 + z_low, corner.1 + side + z_high),
        )
    }
}

/// What a level surface of water does to the sun's light, refracted and
/// reflected in turn.
#[derive(Copy, Clone, Debug)]
struct Flat {
    /// How far a beam drifts along x and z per metre through a level surface.
    drift: [[f64; 2]; 2],
    /// The flux a square metre of level surface passes and reflects, as a
    /// share of the sun's irradiance square to its beam.
    flux: [f64; 2],
    /// How far the sun's disc blurs a point, per metre, along the sun's way
    /// and across it.
    disc: [(f64, f64); 2],
    /// How far a beam drifts, per metre and per unit of slope.
    bend: [f64; 2],
    /// How far the waves' slopes swing a beam from the level beam's way, per
    /// metre.
    swing: [f64; 2],
    /// Whether the reflected beams' pattern is resolved, the sun standing
    /// high enough for them to keep near the level beam's way.
    cast: bool,
}

impl Flat {
    /// What a level surface of index `ior` does to light from a sun toward
    /// `toward`, a disc `radius` across, over waves of `slope_variance`;
    /// `None` for a sun at or below the horizon.
    fn new(toward: Vec3, radius: f64, ior: f64, slope_variance: f64) -> Option<Self> {
        let cos_i = toward.y;
        if cos_i <= 1e-3 {
            return None;
        }
        let drifts = |normal: Vec3| -> Option<[[f64; 2]; 2]> {
            let sunk = refract(-toward, normal, ior)?;
            let cast = (-toward).reflect(normal);
            (sunk.y < 0.0 && cast.y > 0.0).then(|| {
                [
                    [sunk.x / -sunk.y, sunk.z / -sunk.y],
                    [cast.x / cast.y, cast.z / cast.y],
                ]
            })
        };
        // Kept as a beam keeps its own, so a level surface's beams land where
        // its flat image says.
        let level = drifts(Vec3::UP)?.map(|drift| drift.map(|value| f64::from(stored(value))));
        let mut bend = [0.0f64; 2];
        for normal in [
            Vec3::new(-TILT, 1.0, 0.0).normalized(),
            Vec3::new(0.0, 1.0, -TILT).normalized(),
        ] {
            let tilted = drifts(normal)?;
            for ((bend, tilted), level) in bend.iter_mut().zip(&tilted).zip(&level) {
                let moved = mathf::hypot(tilted[0] - level[0], tilted[1] - level[1]);
                *bend = bend.max(moved / TILT);
            }
        }
        let cos_t = mathf::sqrt((1.0 - (1.0 - cos_i * cos_i) / (ior * ior)).max(0.0));
        let reflected = fresnel(cos_i, ior);
        let swing = bend.map(|bend| bend * STRAY * mathf::sqrt(slope_variance.max(0.0)));
        Some(Self {
            drift: level,
            flux: [(1.0 - reflected) * cos_i, reflected * cos_i],
            disc: [
                (
                    radius * cos_i / (ior * cos_t * cos_t * cos_t),
                    radius / (ior * cos_t),
                ),
                (radius / (cos_i * cos_i), radius / cos_i),
            ],
            bend,
            swing,
            cast: swing[1] <= MOST_SWING,
        })
    }
}

/// A body of water: the object its surface is, what a level surface of it
/// does to the sun's light, and the tiles of beams laid over it.
#[derive(Clone, Debug)]
struct Sheet {
    object: usize,
    ior: f64,
    /// How high above the surface a probe starts down to find it.
    top: f64,
    flat: Flat,
    /// The waves' slope's spread, where a gust raises them all.
    slope: f64,
    /// How much of its clearest primary the water absorbs per metre.
    clear: f64,
    /// For each level from the first: the slope variance of the waves too
    /// fine for its cells, which blurs its beams, and the curvature of those
    /// it resolves, which focuses them.
    unresolved: [f64; FINEST as usize],
    curvature: [f64; FINEST as usize],
    /// Its tiles among the scene's, in order of place.
    tiles: Range<usize>,
    /// The farthest a beam over it drifts from a level surface's, per metre,
    /// along x and z: refracted, then reflected.
    stray: [[f64; 2]; 2],
    /// The lowest and highest its surface stands under its tiles.
    heights: (f64, f64),
}

impl Sheet {
    /// Whether the waves level `level` resolves could not move the light of
    /// beams bent `bent` metres per unit of slope, as the eye sees it, by
    /// [`FAINTEST`]: too little for any of their beams to be laid.
    fn too_faint(&self, level: u32, bent: f64) -> bool {
        per_level(&self.curvature, level) * bent < FAINTEST
    }
}

/// What `table`, which holds a value for each level from the first, holds
/// for `level`; nought for one it does not hold.
fn per_level(table: &[f64; FINEST as usize], level: u32) -> f64 {
    level
        .checked_sub(1)
        .and_then(|index| table.get(index as usize))
        .copied()
        .unwrap_or(0.0)
}

/// A square of a body of water cut into beams at every level from two cells
/// a side to 2^`finest`.
#[derive(Copy, Clone, Debug)]
struct Tile {
    /// Which square of the lattice of tiles it is.
    place: (i32, i32),
    finest: u32,
    /// Its first level's place among the scene's, the rest after it.
    layers: usize,
}

impl Tile {
    fn corner(&self) -> (f64, f64) {
        (
            TILE * f64::from(self.place.0),
            TILE * f64::from(self.place.1),
        )
    }
}

/// One level of a tile: 2^`level` cells a side, its beams and its pyramid
/// beginning where it says among the scene's.
#[derive(Copy, Clone, Debug)]
struct Layer {
    level: u32,
    beams: usize,
    bounds: usize,
}

impl Layer {
    const fn cells(&self) -> usize {
        1 << self.level
    }

    fn vertices(&self) -> usize {
        (self.cells() + 1) * (self.cells() + 1)
    }
}

/// How many nodes a pyramid over `cells` cells a side holds: one a block of
/// two by two cells at its foot, a quarter as many each level up, to one.
fn nodes(cells: usize) -> usize {
    let mut total = 0;
    let mut blocks = cells / 2;
    while blocks > 0 {
        total += blocks * blocks;
        blocks /= 2;
    }
    total
}

/// The box a point gathers beams over: about `centre`, `half.0` either way
/// along the sun's way and `half.1` across it.
#[derive(Copy, Clone, Debug)]
struct Gauge {
    centre: (f64, f64),
    way: (f64, f64),
    half: (f64, f64),
    /// How far it reaches from its centre along x and along z.
    extent: (f64, f64),
}

impl Gauge {
    fn new(centre: (f64, f64), way: (f64, f64), half: (f64, f64)) -> Self {
        Self {
            centre,
            way,
            half,
            extent: (
                way.0.abs() * half.0 + way.1.abs() * half.1,
                way.1.abs() * half.0 + way.0.abs() * half.1,
            ),
        }
    }

    /// `(x, z)` in the box's own frame.
    fn local(&self, (x, z): (f64, f64)) -> (f64, f64) {
        let (dx, dz) = (x - self.centre.0, z - self.centre.1);
        (
            dx * self.way.0 + dz * self.way.1,
            dz * self.way.0 - dx * self.way.1,
        )
    }

    fn area(&self) -> f64 {
        4.0 * self.half.0 * self.half.1
    }

    /// Whether the box meets the rectangle `xs` by `zs`, judged by the
    /// rectangle the box lies within.
    fn overlaps(&self, xs: (f64, f64), zs: (f64, f64)) -> bool {
        let (ex, ez) = self.extent;
        xs.0 <= self.centre.0 + ex
            && xs.1 >= self.centre.0 - ex
            && zs.0 <= self.centre.1 + ez
            && zs.1 >= self.centre.1 - ez
    }

    /// The share of the triangle `corners` lying within the box; for a
    /// triangle with no area, whether its middle does.
    fn share(&self, corners: [(f64, f64); 3]) -> f64 {
        let local = corners.map(|corner| self.local(corner));
        let (mut low, mut high) = (local[0], local[0]);
        for &(x, y) in &local[1..] {
            low = (low.0.min(x), low.1.min(y));
            high = (high.0.max(x), high.1.max(y));
        }
        let (along, across) = self.half;
        if high.0 < -along || low.0 > along || high.1 < -across || low.1 > across {
            return 0.0;
        }
        if low.0 >= -along && high.0 <= along && low.1 >= -across && high.1 <= across {
            return 1.0;
        }
        let [first, second, third] = local;
        let whole = 0.5
            * ((second.0 - first.0) * (third.1 - first.1)
                - (third.0 - first.0) * (second.1 - first.1))
                .abs();
        if whole <= 1e-24 {
            let middle = (
                (local[0].0 + local[1].0 + local[2].0) / 3.0,
                (local[0].1 + local[1].1 + local[2].1) / 3.0,
            );
            return if middle.0.abs() <= self.half.0 && middle.1.abs() <= self.half.1 {
                1.0
            } else {
                0.0
            };
        }
        (clipped_area(&local, self.half) / whole).min(1.0)
    }

    /// How much of the box the square `side` across from `corner` covers.
    fn covered(&self, corner: (f64, f64), side: f64) -> f64 {
        let square = [
            corner,
            (corner.0 + side, corner.1),
            (corner.0 + side, corner.1 + side),
            (corner.0, corner.1 + side),
        ]
        .map(|point| self.local(point));
        clipped_area(&square, self.half)
    }
}

/// The area of the polygon `points`, either way round.
fn polygon_area(points: &[(f64, f64)]) -> f64 {
    let twice: f64 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| a.0 * b.1 - b.0 * a.1)
        .sum();
    0.5 * twice.abs()
}

/// The area of the convex polygon `points`, of at most four corners, lying
/// within `half.0` either way of nought along x and `half.1` along y: cut
/// by each side in turn (Sutherland and Hodgman).
fn clipped_area(points: &[(f64, f64)], half: (f64, f64)) -> f64 {
    const MOST: usize = 8;
    let (mut low, mut high) = (
        (f64::INFINITY, f64::INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
    );
    for &(x, y) in points {
        low = (low.0.min(x), low.1.min(y));
        high = (high.0.max(x), high.1.max(y));
    }
    if high.0 < -half.0 || low.0 > half.0 || high.1 < -half.1 || low.1 > half.1 {
        return 0.0;
    }
    if low.0 >= -half.0 && high.0 <= half.0 && low.1 >= -half.1 && high.1 <= half.1 {
        return polygon_area(points);
    }
    let mut polygon = [(0.0, 0.0); MOST];
    let mut count = points.len().min(MOST);
    polygon[..count].copy_from_slice(&points[..count]);
    // Each side as the axis it bounds, the sign it faces and how far out.
    for (axis, sign, limit) in [
        (0, 1.0, half.0),
        (0, -1.0, half.0),
        (1, 1.0, half.1),
        (1, -1.0, half.1),
    ] {
        let along = |point: (f64, f64)| sign * if axis == 0 { point.0 } else { point.1 };
        let mut cut = [(0.0, 0.0); MOST];
        let mut kept = 0;
        for index in 0..count {
            let (here, next) = (polygon[index], polygon[(index + 1) % count]);
            let (inside_here, inside_next) = (along(here) <= limit, along(next) <= limit);
            if inside_here && kept < MOST {
                cut[kept] = here;
                kept += 1;
            }
            if inside_here != inside_next && kept < MOST {
                let t = (limit - along(here)) / (along(next) - along(here));
                cut[kept] = (
                    here.0 + (next.0 - here.0) * t,
                    here.1 + (next.1 - here.1) * t,
                );
                kept += 1;
            }
        }
        polygon = cut;
        count = kept;
        if count < 3 {
            return 0.0;
        }
    }
    polygon_area(&polygon[..count])
}

/// The light a scene's water focuses, and where it lays its beams.
#[derive(Debug, Default)]
pub(crate) struct Caustics {
    /// Which way the sun the beams carry lies, and its way across the
    /// ground, a unit along x and z.
    toward: Vec3,
    way: (f64, f64),
    /// A cell's side as a share of the footprint it resolves.
    texel: f64,
    sheets: Vec<Sheet>,
    tiles: Vec<Tile>,
    layers: Vec<Layer>,
    beams: Vec<Beam>,
    bounds: Vec<Bounds>,
}

impl Caustics {
    /// How much of a level surface's refracted sunlight reaches `at`, `depth`
    /// below the surface of water object `object` and seen `footprint`
    /// across: more where the waves focus it, less where they spread it, and
    /// all of it where no beams are laid.
    pub(crate) fn beneath(&self, object: usize, at: Vec3, depth: f64, footprint: f64) -> f64 {
        self.focused(object, (at, footprint), depth, Way::Sunk)
    }

    /// How much of a level surface's reflected sunlight reaches `at`,
    /// `height` above the surface of water object `object` and seen
    /// `footprint` across.
    pub(crate) fn over(&self, object: usize, at: Vec3, height: f64, footprint: f64) -> f64 {
        self.focused(object, (at, footprint), height, Way::Cast)
    }

    /// `tile`'s level `level`, or its finest where it is cut no finer.
    fn layer(&self, tile: &Tile, level: u32) -> Option<&Layer> {
        let level = level.min(tile.finest).max(1);
        self.layers.get(tile.layers + level as usize - 1)
    }

    fn focused(&self, object: usize, (at, footprint): (Vec3, f64), gone: f64, way: Way) -> f64 {
        let Some(sheet) = self.sheets.iter().find(|sheet| sheet.object == object) else {
            return 1.0;
        };
        let resolved = way == Way::Sunk || sheet.flat.cast;
        if gone.is_nan() || gone <= 0.0 || sheet.tiles.is_empty() || !resolved {
            return 1.0;
        }
        let patch = gone * sheet.flat.swing[way.index()];
        let wanted = gathering_level(self.texel, footprint, patch);
        if wanted.is_nan() || wanted <= 0.0 {
            return 1.0;
        }
        let wanted = wanted.min(f64::from(FINEST));
        let coarse = mathf::floor(wanted);
        let share = wanted - coarse;
        let coarse = u32::try_from(mathf::round_i32(coarse)).unwrap_or(0);
        // Below the first level, a level surface's light stands for the
        // level beneath it.
        let lower = if coarse == 0 {
            1.0
        } else {
            self.at_level(sheet, (at, gone), way, coarse)
        };
        if share <= 0.0 || coarse >= FINEST {
            return lower;
        }
        let upper = self.at_level(sheet, (at, gone), way, coarse + 1);
        lower + (upper - lower) * share
    }

    /// What `sheet`'s beams at level `level` going `way` bring `at`, `gone`
    /// from the surface, against a level surface's.
    fn at_level(&self, sheet: &Sheet, (at, gone): (Vec3, f64), way: Way, level: u32) -> f64 {
        let w = way.index();
        let flat = sheet.flat.drift[w];
        let source = (at.x - gone * flat[0], at.z - gone * flat[1]);
        // The waves too fine for the level blur a beam as a little more of
        // the sun's disc would: their slope's spread along each axis is half
        // its variance.
        let spread =
            gone * sheet.flat.bend[w] * mathf::sqrt(0.5 * per_level(&sheet.unresolved, level));
        let (along, across) = sheet.flat.disc[w];
        let blur = |disc: f64| {
            let disc = DISC_TO_BOX * gone * disc;
            mathf::sqrt(disc * disc + 3.0 * spread * spread).max(LEAST_GAUGE)
        };
        let gauge = Gauge::new((at.x, at.z), self.way, (blur(along), blur(across)));
        let (ex, ez) = gauge.extent;
        let swing = sheet.heights.1 - sheet.heights.0;
        let reach = |axis: usize, extent: f64| {
            gone * sheet.stray[w][axis] + extent + swing * flat[axis].abs()
        };
        let (reach_x, reach_z) = (reach(0, ex), reach(1, ez));
        let lattice = |at: f64| place(mathf::floor(at / TILE));
        let rows = (lattice(source.1 - reach_z), lattice(source.1 + reach_z));
        let tiles = self.tiles.get(sheet.tiles.clone()).unwrap_or(&[]);
        let (Some(first), Some(last)) = (tiles.first(), tiles.last()) else {
            return 1.0;
        };
        let columns = lattice(source.0 - reach_x).max(first.place.0)
            ..=lattice(source.0 + reach_x).min(last.place.0);
        let (mut flux, mut covered) = (0.0, 0.0);
        for column in columns {
            // A sheet's tiles run in order of place, so a column's are together
            // in order of row.
            let start = tiles.partition_point(|tile| tile.place < (column, rows.0));
            let within = tiles.get(start..).unwrap_or(&[]);
            for tile in within
                .iter()
                .take_while(|tile| tile.place <= (column, rows.1))
            {
                let Some(layer) = self.layer(tile, level) else {
                    continue;
                };
                flux += self.gather(tile, layer, (at.y, way), &gauge);
                let corner = tile.corner();
                covered +=
                    gauge.covered((corner.0 + gone * flat[0], corner.1 + gone * flat[1]), TILE);
            }
        }
        let area = gauge.area();
        (flux + (area - covered).max(0.0)) / area
    }

    /// The flux of `tile`'s beams at `layer` going `way` that reaches `gauge`
    /// at the height `at`, as an area of a level surface's.
    fn gather(&self, tile: &Tile, layer: &Layer, (at, way): (f64, Way), gauge: &Gauge) -> f64 {
        let Some(top) = layer.level.checked_sub(1) else {
            return 0.0;
        };
        let cells = layer.cells();
        let step = cell(layer.level);
        let corner = tile.corner();
        let mut starts = [0usize; FINEST as usize];
        let mut start = layer.bounds;
        for (rung, slot) in starts.iter_mut().enumerate().take(layer.level as usize) {
            *slot = start;
            let blocks = cells >> (rung + 1);
            start += blocks * blocks;
        }
        let mut stack = [(0u32, 0usize, 0usize); 4 * FINEST as usize];
        stack[0] = (top, 0, 0);
        let mut depth = 1;
        let mut flux = 0.0;
        while depth > 0 {
            depth -= 1;
            let (rung, column, row) = stack[depth];
            let blocks = cells >> (rung + 1);
            let side = step * real(1 << (rung + 1));
            let Some(node) = starts
                .get(rung as usize)
                .and_then(|&start| self.bounds.get(start + row * blocks + column))
            else {
                continue;
            };
            let at_corner = (corner.0 + side * real(column), corner.1 + side * real(row));
            if !node.reaches(gauge, (at_corner, side), at, way) {
                continue;
            }
            if rung == 0 {
                flux += self.block(tile, layer, (2 * column, 2 * row), (at, way), gauge);
                continue;
            }
            for (dc, dr) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                if depth < stack.len() {
                    stack[depth] = (rung - 1, 2 * column + dc, 2 * row + dr);
                    depth += 1;
                }
            }
        }
        flux
    }

    /// The flux of the beams of `layer`'s two by two cells from vertex
    /// `first` that reaches `gauge` at the height `at`, going `way`.
    fn block(
        &self,
        tile: &Tile,
        layer: &Layer,
        first: (usize, usize),
        (at, way): (f64, Way),
        gauge: &Gauge,
    ) -> f64 {
        let side = layer.cells() + 1;
        let step = cell(layer.level);
        let corner = tile.corner();
        let w = way.index();
        // Where each of the block's nine vertices' beams reach the height.
        let mut landed = [None; 9];
        for (slot, landing) in landed.iter_mut().enumerate() {
            let (column, row) = (first.0 + slot % 3, first.1 + slot / 3);
            let Some(beam) = self.beams.get(layer.beams + row * side + column) else {
                continue;
            };
            let drift = beam.drift[w];
            if beam.height.is_nan() || drift[0].is_nan() {
                continue;
            }
            let gone = way.gone(f64::from(beam.height), at);
            if gone < 0.0 {
                continue;
            }
            *landing = Some((
                (
                    corner.0 + step * real(column) + gone * f64::from(drift[0]),
                    corner.1 + step * real(row) + gone * f64::from(drift[1]),
                ),
                f64::from(beam.flux[w]),
            ));
        }
        let (mut xs, mut zs) = (
            (f64::INFINITY, f64::NEG_INFINITY),
            (f64::INFINITY, f64::NEG_INFINITY),
        );
        for &((x, z), _) in landed.iter().flatten() {
            xs = (xs.0.min(x), xs.1.max(x));
            zs = (zs.0.min(z), zs.1.max(z));
        }
        if !gauge.overlaps(xs, zs) {
            return 0.0;
        }
        let area = 0.5 * step * step;
        let mut flux = 0.0;
        for square in [0, 1, 3, 4] {
            let corners = [square, square + 1, square + 4, square + 3];
            for triangle in [
                [corners[0], corners[1], corners[2]],
                [corners[0], corners[2], corners[3]],
            ] {
                let [Some(a), Some(b), Some(c)] = triangle.map(|vertex| landed[vertex]) else {
                    continue;
                };
                let carried = area * (a.1 + b.1 + c.1) / 3.0;
                if carried > 0.0 {
                    flux += carried * gauge.share([a.0, b.0, c.0]);
                }
            }
        }
        flux
    }
}

/// `whole`, a lattice coordinate, as a tile's place along its axis.
fn place(whole: f64) -> i32 {
    mathf::round_i32(whole)
}

/// Which tile is asked for: the sheet it lies over, and its place on the
/// lattice of tiles.
type Key = (usize, (i32, i32));

/// The tiles a point's light may come by way of: the sheet they lie over,
/// and the columns and rows of the lattice they span, either end included.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Reach {
    sheet: usize,
    columns: (i32, i32),
    rows: (i32, i32),
}

/// What the points a tile lights ask of it: the most any of them bends a
/// beam per unit of slope, as much of it as reaches the eye; the finest
/// footprint any of them is seen over; and the narrowest patch of glints any
/// of them gathers from.
#[derive(Copy, Clone, Debug)]
struct Asked {
    bent: f64,
    fine: f64,
    patch: f64,
}

impl Asked {
    /// As much as this and `other` ask together.
    fn joined(self, other: Self) -> Self {
        Self {
            bent: self.bent.max(other.bent),
            fine: self.fine.min(other.fine),
            patch: self.patch.min(other.patch),
        }
    }
}

/// How far the laying of a scene's beams has come.
#[derive(Copy, Clone, Debug)]
enum Stage {
    /// Surveying the picture from its `n`th point.
    Surveying(usize),
    /// Filling the beams from the `n`th vertex counted through every level
    /// of every tile.
    Filling(usize),
    /// Sealing the pyramids from the `n`th level counted through every tile.
    Sealing(usize),
}

/// The tiles the survey has asked for, each once.
type Asking = HashMap<Key, Asked, BuildFastHash>;

/// What one point asks for beneath water and over it: the tiles its light
/// may come by each way, and what it asks of them.
type Wants = [Option<(Reach, Asked)>; 2];

/// A scene's beams being laid: its picture surveyed for the water whose light
/// it shows, then the tiles that light falls through filled and sealed.
#[derive(Debug)]
pub(crate) struct Focusing {
    stage: Stage,
    size: (u32, u32),
    focus: &'static Focus,
    encoder: Encoder,
    asked: Asking,
    /// How many beams and pyramid nodes the tiles planned hold, reserved
    /// whole and written as they are filled.
    planned: (usize, usize),
    laid: Caustics,
}

impl Focusing {
    /// The laying of `scene`'s beams for a picture of `size`, as `focus` has
    /// them laid; `None` when the heap will not hold what it needs to begin.
    pub(crate) fn new(scene: &Scene, size: (u32, u32), focus: &'static Focus) -> Option<Self> {
        let mut laid = Caustics {
            texel: focus.texel,
            ..Caustics::default()
        };
        if let Some((toward, cos_radius, _)) = scene.sun() {
            let horizontal = mathf::hypot(toward.x, toward.z);
            laid.toward = toward;
            laid.way = if horizontal > 1e-9 {
                (toward.x / horizontal, toward.z / horizontal)
            } else {
                (1.0, 0.0)
            };
            let radius = mathf::acos(cos_radius.clamp(-1.0, 1.0));
            if !fallible::reserve(&mut laid.sheets, scene.waters.len()) {
                return None;
            }
            for &object in &scene.waters {
                let Some(water) = scene.water(object) else {
                    continue;
                };
                let (ior, waves) = (water.ior, water.waves);
                let Some(flat) = Flat::new(toward, radius, ior, waves.slope_variance()) else {
                    continue;
                };
                let Some(top) = scene
                    .objects
                    .get(object)
                    .and_then(|found| match found.shape {
                        Shape::Plane { offset, .. } => Some(offset + 1.0),
                        Shape::Land { field } => scene
                            .fields
                            .get(field as usize)
                            .map(|grid| grid.highest() + 1.0),
                        ref shape => shape
                            .bounds(scene.geometry())
                            .map(|bounds| bounds.max.y + 1.0),
                    })
                else {
                    continue;
                };
                let (mut unresolved, mut curvature) =
                    ([0.0; FINEST as usize], [0.0; FINEST as usize]);
                for ((level, blurred), curved) in
                    (1..).zip(unresolved.iter_mut()).zip(curvature.iter_mut())
                {
                    *blurred = waves.unresolved(cutoff(level));
                    *curved = waves.curvature(cutoff(level));
                }
                laid.sheets.push(Sheet {
                    object,
                    ior,
                    top,
                    flat,
                    slope: mathf::sqrt(waves.slope_variance()),
                    clear: water.absorb.x.min(water.absorb.y).min(water.absorb.z),
                    unresolved,
                    curvature,
                    tiles: 0..0,
                    stray: [[0.0; 2]; 2],
                    heights: (f64::INFINITY, f64::NEG_INFINITY),
                });
            }
        }
        Some(Self {
            stage: Stage::Surveying(0),
            size,
            focus,
            encoder: Encoder::new()?,
            // Keyed by places the scene's own geometry picks, which no one can
            // choose to collide: the fast unkeyed hash.
            asked: HashMap::with_hasher(BuildFastHash::new()),
            planned: (0, 0),
            laid,
        })
    }

    /// Whether the scene has water the sun shines on, whose beams there is
    /// work to lay.
    pub(crate) fn watered(&self) -> bool {
        !self.laid.sheets.is_empty()
    }

    /// The survey's points across and down the picture.
    fn grid(&self) -> (u32, u32) {
        let stride = self.focus.stride.max(1);
        (self.size.0.div_ceil(stride), self.size.1.div_ceil(stride))
    }

    /// The pixel the survey's `index`th point looks through.
    fn pixel(&self, index: u32) -> (u32, u32) {
        let (columns, _) = self.grid();
        let stride = self.focus.stride.max(1);
        (
            ((index % columns.max(1)) * stride + stride / 2).min(self.size.0.saturating_sub(1)),
            ((index / columns.max(1)) * stride + stride / 2).min(self.size.1.saturating_sub(1)),
        )
    }

    fn points(&self) -> usize {
        if self.laid.sheets.is_empty() {
            return 0;
        }
        let (columns, rows) = self.grid();
        usize::try_from(u64::from(columns) * u64::from(rows)).unwrap_or(usize::MAX)
    }

    /// Do the next unit of the work over `scene` across `runner`; whether
    /// every beam is laid, or `None` when the heap refused it.
    pub(crate) fn step(&mut self, scene: &Scene, runner: &dyn JobRunner) -> Option<bool> {
        let width = runner.width().max(1);
        match self.stage {
            Stage::Surveying(next) => {
                let end = next.saturating_add(SURVEY_UNIT * width).min(self.points());
                if next < end {
                    self.survey(scene, next..end, runner)?;
                }
                self.stage = if end < self.points() {
                    Stage::Surveying(end)
                } else {
                    self.plan()?;
                    Stage::Filling(0)
                };
            }
            Stage::Filling(next) => {
                let total = self.planned.0;
                let end = next.saturating_add(FILL_UNIT * width).min(total);
                if next < end {
                    // Within what the plan reserved, so it never allocates.
                    self.laid.beams.resize(end, Beam::DRY);
                    self.fill(scene, next..end, runner);
                }
                self.stage = if end < total {
                    Stage::Filling(end)
                } else {
                    Stage::Sealing(0)
                };
            }
            Stage::Sealing(next) => {
                let end = self.sealing_end(next, SEAL_UNIT * width);
                if next < end {
                    self.seal(next..end, runner)?;
                }
                if end < self.laid.layers.len() {
                    self.stage = Stage::Sealing(end);
                } else {
                    self.laid.measure();
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    /// The end of the levels from `next` whose pyramids hold about `budget`
    /// nodes between them, and at least one.
    fn sealing_end(&self, next: usize, budget: usize) -> usize {
        let layers = &self.laid.layers;
        let Some(first) = layers.get(next) else {
            return layers.len();
        };
        let limit = first.bounds.saturating_add(budget);
        let after = layers.get(next + 1..).unwrap_or(&[]);
        next + 1 + after.partition_point(|layer| layer.bounds < limit)
    }

    /// How far the laying has come: the survey a fifth of it, filling the
    /// beams most of the rest.
    pub(crate) fn done(&self) -> f64 {
        match self.stage {
            Stage::Surveying(next) => 0.2 * share(next, self.points()),
            Stage::Filling(next) => 0.2 + 0.75 * share(next, self.planned.0),
            Stage::Sealing(next) => {
                let sealed = self
                    .laid
                    .layers
                    .get(next)
                    .map_or(self.planned.1, |layer| layer.bounds);
                0.95 + 0.05 * share(sealed, self.planned.1)
            }
        }
    }

    /// The beams laid.
    pub(crate) fn finish(self) -> Caustics {
        self.laid
    }

    /// Survey the points `points` of the picture of `scene` across `runner`,
    /// asking for every tile whose light reaches what they show; `None` when
    /// the heap refused the asking.
    fn survey(
        &mut self,
        scene: &Scene,
        points: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let tracer = Tracer::new(scene, &self.encoder, self.size, 0);
        let pixel = scene.camera.pixel_angle(self.size.1);
        let (this, laid) = (&*self, &self.laid);
        let mut found: Vec<(usize, Wants)> =
            fallible::collected(points.len(), points.map(|point| (point, [None; 2])))?;
        tairix_parallel::for_each(runner, &mut found, &|(point, slot)| {
            let Ok(index) = u32::try_from(*point) else {
                return;
            };
            *slot = laid
                .lit(scene, tracer.eye_ray(this.pixel(index)), pixel)
                .map(|lit| lit.and_then(|lit| laid.wanted(&lit)));
        });
        // Points side by side mostly ask for the same tiles: a run of them
        // asks once, each way apart.
        let mut runs: Wants = [None; 2];
        for (_, asking) in &found {
            for (run, &asking) in runs.iter_mut().zip(asking) {
                let Some((reach, wanted)) = asking else {
                    continue;
                };
                match run {
                    Some((held, asked)) if *held == reach => *asked = asked.joined(wanted),
                    _ => {
                        if let Some((held, asked)) = run.replace((reach, wanted)) {
                            self.ask(held, asked)?;
                        }
                    }
                }
            }
        }
        for (reach, asked) in runs.into_iter().flatten() {
            self.ask(reach, asked)?;
        }
        Some(())
    }

    /// Ask for every tile `reach` spans as `wanted` asks; `None` when the
    /// heap will not hold the asking.
    fn ask(&mut self, reach: Reach, wanted: Asked) -> Option<()> {
        for row in reach.rows.0..=reach.rows.1 {
            for column in reach.columns.0..=reach.columns.1 {
                let key = (reach.sheet, (column, row));
                if let Some(held) = self.asked.get_mut(&key) {
                    *held = held.joined(wanted);
                } else {
                    self.asked.try_insert(key, wanted).ok()?;
                }
            }
        }
        Some(())
    }

    /// Into `planned`, the finest level each tile `asked` for is cut to, a
    /// cell the share `texel` of the finest footprint it lights, and only
    /// where its waves move that light by [`FAINTEST`] or more.
    fn levels(&self, asked: &[(Key, Asked)], texel: f64, planned: &mut Vec<(Key, u32)>) {
        planned.clear();
        for &(key, wanted) in asked {
            let Some(sheet) = self.laid.sheets.get(key.0) else {
                continue;
            };
            let Some(level) = finest_level(gathering_level(texel, wanted.fine, wanted.patch))
            else {
                continue;
            };
            if !sheet.too_faint(level, wanted.bent) {
                planned.push((key, level));
            }
        }
    }

    /// Plan the tiles to lay, coarsening every footprint alike until they fit
    /// the scene's room, and reserve their buffers; `None` when the heap
    /// will not hold them.
    fn plan(&mut self) -> Option<()> {
        let asking =
            core::mem::replace(&mut self.asked, HashMap::with_hasher(BuildFastHash::new()));
        let mut asked: Vec<(Key, Asked)> = fallible::collected(asking.len(), asking.into_iter())?;
        // Each sheet's tiles together in order of place, as a lookup finds
        // them; no two share a key, so the order is the same however sorted.
        asked.sort_unstable_by_key(|&(key, _)| key);
        // A tile's levels hold a third again as many cells as its finest.
        let cells = |planned: &[(Key, u32)]| -> usize {
            planned
                .iter()
                .map(|&(_, level)| (1usize << (2 * level)) / 3 * 4)
                .sum()
        };
        // Within the room reserved for every tile asked for, so it never
        // grows.
        let mut planned: Vec<(Key, u32)> = Vec::new();
        if !fallible::reserve(&mut planned, asked.len()) {
            return None;
        }
        self.levels(&asked, self.laid.texel, &mut planned);
        let mut coarsened = 0;
        while cells(&planned) > self.focus.cells && coarsened < MOST_COARSENINGS {
            self.laid.texel *= core::f64::consts::SQRT_2;
            self.levels(&asked, self.laid.texel, &mut planned);
            coarsened += 1;
        }
        if cells(&planned) > self.focus.cells {
            planned.clear();
        }
        let layers: usize = planned.iter().map(|&(_, level)| level as usize).sum();
        if !fallible::reserve(&mut self.laid.tiles, planned.len())
            || !fallible::reserve(&mut self.laid.layers, layers)
        {
            return None;
        }
        let (mut beams, mut bounds) = (0usize, 0usize);
        for &((sheet, place), finest) in &planned {
            let Some(sheet) = self.laid.sheets.get_mut(sheet) else {
                continue;
            };
            let index = self.laid.tiles.len();
            if sheet.tiles.is_empty() {
                sheet.tiles = index..index;
            }
            sheet.tiles.end = index + 1;
            self.laid.tiles.push(Tile {
                place,
                finest,
                layers: self.laid.layers.len(),
            });
            for level in 1..=finest {
                let layer = Layer {
                    level,
                    beams,
                    bounds,
                };
                beams += layer.vertices();
                bounds += nodes(layer.cells());
                self.laid.layers.push(layer);
            }
        }
        if !fallible::reserve(&mut self.laid.beams, beams)
            || !fallible::reserve(&mut self.laid.bounds, bounds)
        {
            return None;
        }
        self.planned = (beams, bounds);
        Some(())
    }

    /// Fill the beams `vertices`, counted through every level of every tile,
    /// across `runner`, a stretch of a row at a time.
    fn fill(&mut self, scene: &Scene, vertices: Range<usize>, runner: &dyn JobRunner) {
        let Caustics {
            toward,
            sheets,
            tiles,
            layers,
            beams,
            ..
        } = &mut self.laid;
        let (toward, sheets, tiles, layers) = (*toward, &*sheets, &*tiles, &*layers);
        let start = vertices.start;
        let Some(slots) = beams.get_mut(vertices) else {
            return;
        };
        crate::band::for_each(runner, slots, (0, FILL_UNIT), &|band, out| {
            let mut vertex = start + band * FILL_UNIT;
            let mut index = layers
                .partition_point(|layer| layer.beams <= vertex)
                .saturating_sub(1);
            let mut tile = tiles
                .partition_point(|tile| tile.layers <= index)
                .saturating_sub(1);
            let owning = |tile: usize| sheets.iter().find(|sheet| sheet.tiles.contains(&tile));
            let mut sheet = owning(tile);
            let mut sweep = Sweep::new();
            let mut rest = out;
            while !rest.is_empty() {
                while layers
                    .get(index + 1)
                    .is_some_and(|next| next.beams <= vertex)
                {
                    index += 1;
                }
                while tiles.get(tile + 1).is_some_and(|next| next.layers <= index) {
                    tile += 1;
                    sheet = owning(tile);
                }
                let (Some(layer), Some(owner), Some(sheet)) =
                    (layers.get(index), tiles.get(tile), sheet)
                else {
                    rest.fill(Beam::DRY);
                    return;
                };
                let side = layer.cells() + 1;
                let local = vertex - layer.beams;
                let run = (side - local % side).min(rest.len());
                let (here, after) = core::mem::take(&mut rest).split_at_mut(run);
                let stretch = Stretch {
                    tile: owner,
                    layer,
                    row: local / side,
                    column: local % side,
                };
                lay(scene, (sheet, toward), stretch, here, &mut sweep);
                rest = after;
                vertex += run;
            }
        });
    }

    /// Seal the pyramids of levels `chosen`, counted through every tile,
    /// across `runner`: the foot of each in bands of its rows, then the rungs
    /// above; `None` when the heap will not hold the lists of them.
    fn seal(&mut self, chosen: Range<usize>, runner: &dyn JobRunner) -> Option<()> {
        let Caustics {
            layers,
            beams,
            bounds,
            ..
        } = &mut self.laid;
        let chosen = layers.get(chosen)?;
        let first = chosen.first()?.bounds;
        let last = chosen.last()?;
        let end = last.bounds + nodes(last.cells());
        // Within what the plan reserved, so it never allocates.
        bounds.resize(end.max(bounds.len()), Bounds::EMPTY);
        let mut rest = bounds.get_mut(first..end)?;
        let mut pyramids: Vec<(&Layer, &mut [Bounds])> = Vec::new();
        if !fallible::reserve(&mut pyramids, chosen.len()) {
            return None;
        }
        let mut bands = 0;
        for layer in chosen {
            let (own, after) = rest.split_at_mut(nodes(layer.cells()).min(rest.len()));
            pyramids.push((layer, own));
            rest = after;
            bands += (layer.cells() / 2).div_ceil(foot_rows(layer));
        }
        let beams = &*beams;
        {
            let mut feet: Vec<(&Layer, usize, &mut [Bounds])> = Vec::new();
            if !fallible::reserve(&mut feet, bands) {
                return None;
            }
            for (layer, own) in &mut pyramids {
                let blocks = layer.cells() / 2;
                let rows = foot_rows(layer);
                let foot = own.get_mut(..blocks * blocks).unwrap_or_default();
                for (band, slots) in foot.chunks_mut((rows * blocks).max(1)).enumerate() {
                    feet.push((*layer, band * rows, slots));
                }
            }
            tairix_parallel::for_each(runner, &mut feet, &|(layer, row, slots)| {
                seal_foot(layer, beams, *row, slots);
            });
        }
        tairix_parallel::for_each(runner, &mut pyramids, &|(layer, own)| {
            seal_rungs(layer, own);
        });
        Some(())
    }
}

impl Caustics {
    /// Each sheet's reach and its surface's span, from its sealed levels.
    fn measure(&mut self) {
        for sheet in &mut self.sheets {
            for tile in self.tiles.get(sheet.tiles.clone()).unwrap_or(&[]) {
                for layer in self
                    .layers
                    .get(tile.layers..tile.layers + tile.finest as usize)
                    .unwrap_or(&[])
                {
                    let Some(root) = nodes(layer.cells())
                        .checked_sub(1)
                        .and_then(|last| self.bounds.get(layer.bounds + last))
                    else {
                        continue;
                    };
                    if root.height[0] > root.height[1] {
                        continue;
                    }
                    sheet.heights = (
                        sheet.heights.0.min(f64::from(root.height[0])),
                        sheet.heights.1.max(f64::from(root.height[1])),
                    );
                    for ((stray, flat), drift) in sheet
                        .stray
                        .iter_mut()
                        .zip(&sheet.flat.drift)
                        .zip(&root.drift)
                    {
                        for ((stray, &flat), &[least, most]) in
                            stray.iter_mut().zip(flat).zip(drift)
                        {
                            if least <= most {
                                *stray = stray
                                    .max((f64::from(least) - flat).abs())
                                    .max((f64::from(most) - flat).abs());
                            }
                        }
                    }
                }
            }
        }
    }

    /// The tiles whose beams may reach where `lit` says, and what it asks
    /// of them; `None` where it asks for none.
    fn wanted(&self, lit: &Lit) -> Option<(Reach, Asked)> {
        let sheet = self.sheets.get(lit.sheet)?;
        if lit.way == Way::Cast && !sheet.flat.cast {
            return None;
        }
        let w = lit.way.index();
        let patch = lit.gone * sheet.flat.swing[w];
        // What of a point's light reaches the eye beneath water, through what
        // the water absorbs down the sun's bent way and up the eye's.
        let seen = match lit.way {
            Way::Sunk => {
                let [x, z] = sheet.flat.drift[0];
                let way_down = lit.gone * mathf::sqrt(1.0 + x * x + z * z);
                mathf::exp(-sheet.clear * (way_down + lit.seen))
            }
            Way::Cast => 1.0,
        };
        let bent = lit.gone * sheet.flat.bend[w] * seen;
        // A point too deep or too high for any tile to resolve a wave for it,
        // or whose waves could not move the light it sends the eye, asks for
        // none.
        let level = finest_level(gathering_level(self.texel, lit.footprint, patch))?;
        if sheet.too_faint(level, bent) {
            return None;
        }
        let stray = lit.gone * sheet.flat.bend[w] * STRAY * sheet.slope;
        let (along, across) = sheet.flat.disc[w];
        let reach = stray + DISC_TO_BOX * lit.gone * along.max(across);
        let lattice = |at: f64| place(mathf::floor(at / TILE));
        Some((
            Reach {
                sheet: lit.sheet,
                columns: (lattice(lit.at.0 - reach), lattice(lit.at.0 + reach)),
                rows: (lattice(lit.at.1 - reach), lattice(lit.at.1 + reach)),
            },
            Asked {
                bent,
                fine: lit.footprint,
                patch,
            },
        ))
    }

    /// Where the sun's light reaches what the eye's `ray` shows by way of
    /// water, for a picture `pixel` radians a pixel: beneath a surface the eye
    /// looks into, and over one it looks across or sees mirrored in it. Its
    /// rays pass through lawns, whose blades are too fine to lay beams for.
    fn lit(&self, scene: &Scene, ray: Ray, pixel: f64) -> [Option<Lit>; 2] {
        let mut found = [None; 2];
        let Some((index, hit)) = scene.closest(&ray, f64::INFINITY, Sight::Recorded) else {
            return found;
        };
        let point = ray.at(hit.t);
        let facing = |normal: Vec3, dir: Vec3| {
            if normal.dot(dir) < 0.0 {
                normal
            } else {
                -normal
            }
        };
        let Some(sheet) = self.sheets.iter().position(|sheet| sheet.object == index) else {
            let normal = facing(hit.normal, ray.dir);
            let footprint = resolved(hit.t * pixel, normal, -ray.dir);
            found[1] = self.over_water(scene, (point, normal), footprint);
            return found;
        };
        let Some(ior) = self.sheets.get(sheet).map(|sheet| sheet.ior) else {
            return found;
        };
        let normal = facing(hit.shading, ray.dir);
        if let Some(inward) = refract(ray.dir, normal, ior) {
            let inside = Ray::new(lift(point, -normal), inward);
            if let Some((under, beneath)) = scene.closest(&inside, f64::INFINITY, Sight::Scattered)
            {
                if under != index {
                    let bed = facing(beneath.normal, inward);
                    let footprint = resolved((hit.t + beneath.t) * pixel, bed, -inward);
                    found[0] = self
                        .under_water(scene, sheet, inside.at(beneath.t), footprint)
                        .map(|lit| Lit {
                            seen: beneath.t,
                            ..lit
                        });
                }
            }
        }
        let mirrored = Ray::new(lift(point, normal), ray.dir.reflect(normal));
        if let Some((seen, met)) = scene.closest(&mirrored, f64::INFINITY, Sight::Scattered) {
            if self.sheets.iter().all(|sheet| sheet.object != seen) {
                let normal = facing(met.normal, mirrored.dir);
                let footprint = resolved((hit.t + met.t) * pixel, normal, -mirrored.dir);
                found[1] = self.over_water(scene, (mirrored.at(met.t), normal), footprint);
            }
        }
        found
    }

    /// Where the sun's light reaches `point`, seen `footprint` across
    /// beneath `sheet`'s water: up the way it came through a level surface.
    fn under_water(&self, scene: &Scene, sheet: usize, point: Vec3, footprint: f64) -> Option<Lit> {
        let water = self.sheets.get(sheet)?;
        let up = -refract(-self.toward, Vec3::UP, water.ior)?;
        let (object, t) = scene.water_toward(&Ray::new(point, up), true)?;
        if object != water.object {
            return None;
        }
        let surface = point + up * t;
        Some(Lit {
            sheet,
            way: Way::Sunk,
            at: (surface.x, surface.z),
            gone: surface.y - point.y,
            footprint,
            seen: 0.0,
        })
    }

    /// Where the sun's light reflected off water reaches `point`, whose
    /// surface faces `facing` and is seen `footprint` across: down the way it
    /// came off a level surface.
    fn over_water(
        &self,
        scene: &Scene,
        (point, facing): (Vec3, Vec3),
        footprint: f64,
    ) -> Option<Lit> {
        let down = Vec3::new(self.toward.x, -self.toward.y, self.toward.z);
        if self.toward.y <= 0.0 || facing.dot(down) <= 0.0 {
            return None;
        }
        let (object, t) = scene.water_toward(&Ray::new(lift(point, facing), down), false)?;
        let sheet = self
            .sheets
            .iter()
            .position(|sheet| sheet.object == object)?;
        let surface = point + down * t;
        Some(Lit {
            sheet,
            way: Way::Cast,
            at: (surface.x, surface.z),
            gone: point.y - surface.y,
            footprint,
            seen: 0.0,
        })
    }
}

/// How many rows of `layer`'s foot one job seals: about [`SEAL_BAND`]
/// blocks, and at least one row.
fn foot_rows(layer: &Layer) -> usize {
    (SEAL_BAND / (layer.cells() / 2).max(1)).max(1)
}

/// Seal the blocks of `layer`'s foot from row `first` into `out`, each block
/// of two by two cells from its nine vertices' beams.
fn seal_foot(layer: &Layer, beams: &[Beam], first: usize, out: &mut [Bounds]) {
    let side = layer.cells() + 1;
    let blocks = (layer.cells() / 2).max(1);
    for (row, slots) in (first..).zip(out.chunks_mut(blocks)) {
        for (column, slot) in slots.iter_mut().enumerate() {
            let mut bounds = Bounds::EMPTY;
            for vertex in 0..9 {
                let (c, r) = (2 * column + vertex % 3, 2 * row + vertex / 3);
                if let Some(beam) = beams.get(layer.beams + r * side + c) {
                    bounds = bounds.with(beam);
                }
            }
            *slot = bounds;
        }
    }
}

/// Seal the rungs of `layer`'s pyramid `own` above its sealed foot, each
/// node from the four below it.
fn seal_rungs(layer: &Layer, own: &mut [Bounds]) {
    let mut blocks = layer.cells() / 2;
    let (mut below, mut start) = (0, blocks * blocks);
    while blocks > 1 {
        let above = blocks / 2;
        for row in 0..above {
            for column in 0..above {
                let mut bounds = Bounds::EMPTY;
                for (dc, dr) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    if let Some(child) = own.get(below + (2 * row + dr) * blocks + 2 * column + dc)
                    {
                        bounds = bounds.join(child);
                    }
                }
                if let Some(slot) = own.get_mut(start + row * above + column) {
                    *slot = bounds;
                }
            }
        }
        below = start;
        start += above * above;
        blocks = above;
    }
}

/// A stretch of one level's row of beams: the tile and level it lies in,
/// its row, and the column it begins at.
#[derive(Copy, Clone, Debug)]
struct Stretch<'a> {
    tile: &'a Tile,
    layer: &'a Layer,
    row: usize,
    column: usize,
}

/// Lay `out`, the beams of `stretch` over `sheet` under a sun toward
/// `toward`, reading the waves along it with `sweep`.
fn lay(
    scene: &Scene,
    (sheet, toward): (&Sheet, Vec3),
    stretch: Stretch<'_>,
    out: &mut [Beam],
    sweep: &mut Sweep,
) {
    let (Some(object), Some(water)) = (scene.objects.get(sheet.object), scene.water(sheet.object))
    else {
        out.fill(Beam::DRY);
        return;
    };
    let (corner, level) = (stretch.tile.corner(), stretch.layer.level);
    let (step, footprint) = (cell(level), cutoff(level));
    let x = |index: usize| corner.0 + step * real(stretch.column + index);
    let z = corner.1 + step * real(stretch.row);
    let texture = object.texture;
    let swept = lies_level(&texture);
    if swept {
        water.waves.begin(
            sweep,
            texture.point_to_local(Vec3::new(x(0), 0.0, z)),
            texture.frame.to_local(Vec3::new(step, 0.0, 0.0)),
            footprint,
        );
    }
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = surface(scene, (object, sheet.top), (x(index), z)).map_or(
            Beam::DRY,
            |(point, smooth)| {
                let p = texture.point_to_local(point);
                let normal = if swept {
                    water.waves.tilted(smooth, p, sweep)
                } else {
                    water.waves.tilt(smooth, p, footprint).normal
                };
                beam(sheet, toward, point, normal)
            },
        );
        if swept {
            sweep.advance();
        }
    }
}

/// Whether the waves `texture` frames lie level, so that however a row's
/// surface rises and falls, each crest moves on evenly along it.
fn lies_level(texture: &Pose) -> bool {
    texture.frame.x.y == 0.0 && texture.frame.z.y == 0.0
}

/// Where a probe down onto `object` from the height `top` at `(x, z)` meets
/// its surface, and the surface's smooth normal there, turned up; `None`
/// where no water stands.
fn surface(
    scene: &Scene,
    (object, top): (&Object, f64),
    (x, z): (f64, f64),
) -> Option<(Vec3, Vec3)> {
    let probe = Ray::new(Vec3::new(x, top, z), Vec3::new(0.0, -1.0, 0.0));
    let hit = object
        .shape
        .intersect(&probe, NEAR, f64::INFINITY, scene.geometry())?;
    let smooth = if hit.shading.y >= 0.0 {
        hit.shading
    } else {
        -hit.shading
    };
    Some((probe.at(hit.t), smooth))
}

/// The beam from `point` on `sheet`'s surface, whose waves tilt it to
/// `normal`, under a sun toward `toward`.
fn beam(sheet: &Sheet, toward: Vec3, point: Vec3, normal: Vec3) -> Beam {
    let mut beam = Beam {
        height: stored(point.y),
        drift: sheet.flat.drift.map(|drift| drift.map(stored)),
        flux: [0.0; 2],
    };
    let cos_i = normal.dot(toward);
    // A facet turned from the sun lights nothing, but its beams still bound
    // the triangles about it as a level surface's would.
    if cos_i <= 0.0 {
        return beam;
    }
    let reflected = fresnel(cos_i, sheet.ior);
    // The sun meets a tilted facet over more or less of the level surface
    // beneath it.
    let lit = cos_i / normal.y.max(1e-6);
    if let Some(sunk) = refract(-toward, normal, sheet.ior).filter(|sunk| sunk.y < -1e-6) {
        beam.drift[0] = [stored(sunk.x / -sunk.y), stored(sunk.z / -sunk.y)];
        beam.flux[0] = stored((1.0 - reflected) * lit / sheet.flat.flux[0]);
    }
    let cast = (-toward).reflect(normal);
    beam.drift[1] = if cast.y >= LEAST_CLIMB {
        beam.flux[1] = stored(reflected * lit / sheet.flat.flux[1].max(1e-12));
        [stored(cast.x / cast.y), stored(cast.z / cast.y)]
    } else {
        [f32::NAN; 2]
    };
    beam
}

/// `value` as a beam keeps it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a beam keeps its height, drift and flux as singles, rounded to the nearest"
)]
fn stored(value: f64) -> f32 {
    value as f32
}

/// Where the survey found the sun's light reaching a point the picture shows
/// by way of a body of water: on which sheet and which way, where it crosses
/// the surface, how far below or above it the point lies, how finely the eye
/// resolves the point, and how far through the water the eye sees it.
#[derive(Copy, Clone, Debug)]
struct Lit {
    sheet: usize,
    way: Way,
    at: (f64, f64),
    gone: f64,
    footprint: f64,
    seen: f64,
}

#[cfg(test)]
#[path = "caustic_tests.rs"]
mod tests;
