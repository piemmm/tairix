//! Volumetric cloud: heaped cumulus, a broken sheet of stratocumulus, a field
//! of altocumulus, or high streaks of cirrus, marched through as the medium
//! it is.
//!
//! A cloudbank is up to two decks of cloud over the round Earth, as far as
//! any of it can be seen. Each deck has a weather map — how much of each
//! column it covers, and where its base and top lie there, so no two clouds
//! sit at the same height — and its density within a column is a vertical
//! profile, flat beneath and heaped above, times a billowing noise, eroded at
//! its edges by a finer one (Schneider, "The Real-time Volumetric
//! Cloudscapes of Horizon: Zero Dawn", 2015). Both noises are Perlin–Worley
//! textures that tile, built once per scene, so a sample reads them rather
//! than evaluating them.
//!
//! Heights are taken over the Earth's curve about its centre below the eye,
//! as the air's are, so a deck seen low down runs on to the horizon. Its maps
//! are laid in levels about the eye, each twice the last's breadth in as many
//! columns (Losasso and Hoppe, "Geometry Clipmaps", 2004): a column spans
//! about as many pixels far off as near, each level's weather is drawn only
//! as finely as its columns hold, and each level's rim is blended into the
//! next's, so no seam shows where they meet.
//!
//! A ray gathers what the cloud scatters toward it step by step, each step's
//! light integrated exactly over the step (Hillaire, "Physically Based Sky,
//! Atmosphere and Cloud Rendering in Frostbite", 2016), and strides across
//! the air above and below the heights each weather cell bounds its cloud
//! to. The sun's light at a step is dimmed by the cloud toward the sun — a
//! grid of that optical depth, and four short taps for the billows' own
//! shadows — and spread by several scattering octaves, each lighter and
//! broader than the last, for the brightness multiple scattering gives
//! (Wrenninge, Kulla and Lundqvist, "Oz: The Great and Volumetric", 2013).
//! Each step's sunlight has crossed the air to its own place at the angle the
//! sun stands there, so a deck still above the Earth's shadow at dusk is lit
//! red from beneath, and cloud far off toward a set sun is lit after the
//! cloud overhead has gone grey.
//!
//! Cirrus is ice, too thin for those octaves, which brighten a cloud by
//! light it is too thin to have scattered twice. Its sunlight is scattered
//! once exactly, by rough ice crystals' phase function; the light scattered
//! again and again is Hillaire's isotropic series over the deck as its mean
//! extinction lays it out by height, scaled by the similarity principle for
//! how far forward ice throws it; and the sky's light and the ground's are
//! scattered in by the share of the phase each hemisphere sends the eye.

use alloc::vec::Vec;
use core::f64::consts::{PI, SQRT_2, TAU};
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::atmosphere::{every_order, GROUND};
use crate::band;
use crate::heightfield::bilinear;
use crate::lanes::Corners;
use crate::noise::{cell, fbm2_resolved, hash3, noise2, resolved, smoothstep};
use crate::sample::{mix32, unit, GOLDEN_RATIO};
use crate::shape::{quadratic, reciprocal};
use crate::vector::{cell_of, real, single, Vec3};

/// How a deck of cloud is formed.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Deck {
    /// Its base's mean height, and how far a cloud's base strays from it.
    pub(crate) base: f64,
    pub(crate) base_spread: f64,
    /// Its clouds' least and greatest depth, base to top.
    pub(crate) depth: (f64, f64),
    /// How much of the sky it covers on the whole, `0.0..=1.0`.
    pub(crate) cover: f64,
    /// How heaped its clouds are: `0.0` flat sheets, `1.0` towering heaps.
    pub(crate) heap: f64,
    /// The breadth of its weather's features, and how much longer than broad
    /// along `heading`: rolls and rows of cloud.
    pub(crate) scale: f64,
    pub(crate) stretch: f64,
    pub(crate) heading: f64,
    /// Extinction per metre at full density.
    pub(crate) thickness: f64,
    /// The breadth of the billows its clouds are made of, and how many times
    /// longer they are drawn out along `heading`: streaks of ice.
    pub(crate) billow: f64,
    pub(crate) fibre: f64,
    pub(crate) matter: Matter,
    pub(crate) seed: u32,
}

/// What a deck's cloud is made of, which says how it scatters.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Matter {
    /// Droplets of water, in clouds thick enough for light to scatter
    /// through them many times over.
    Water,
    /// Rough crystals of ice, in thin high cloud.
    Ice,
}

/// Texels along each edge of the billowing noise, and of the finer one.
const SHAPE_SIDE: usize = 64;
const DETAIL_SIDE: usize = 32;

const _: () = assert!(SHAPE_SIDE.is_power_of_two() && DETAIL_SIDE.is_power_of_two());
/// Cells along each side of a level's weather map, of its grid of the sun's
/// optical depth, and of the shadow it casts: multiples of four, so a
/// level's square lies on whole cells of the next and its columns on every
/// other one of the next's.
const WEATHER_CELLS: usize = 384;
const LIGHT_CELLS: usize = 96;
const SHADOW_CELLS: usize = 256;
const _: () = assert!(
    WEATHER_CELLS.is_multiple_of(4)
        && LIGHT_CELLS.is_multiple_of(4)
        && SHADOW_CELLS.is_multiple_of(4)
);
/// Layers up the grid of the sun's optical depth.
const LIGHT_LAYERS: usize = 20;
/// Rows a core fills in a unit of that grid, and of the shadow, each of
/// whose points marches toward the sun: some milliseconds' work through two
/// decks.
const LIGHT_ROWS: usize = 6;
const SHADOW_ROWS: usize = 1;
/// Steps taken toward the sun building that grid, and across the bank for
/// the ground's shadow.
const LIGHT_STEPS: u32 = 24;
/// The share of a level's half-breadth over which its rim is blended into
/// the next level's.
const RIM: f64 = 0.125;
/// The most levels a bank is laid in.
const MOST_LEVELS: usize = 8;
/// Heights the sunlight reaching the bank is tabulated at, and cosines of
/// the sun's angle from the vertical over those the bank's places see.
pub(crate) const SUNLIGHT_LEVELS: usize = 48;
pub(crate) const SUN_COSINES: usize = 33;

/// How far apart the finer noise's features lie, over the billows'.
const DETAIL_SCALE: f64 = 0.2;
/// How deep the finer noise bites into a cloud's edge.
const EROSION: f64 = 0.62;
/// The looks toward the sun a fine sample takes through its own billows, as
/// fractions of a billow's breadth; past the last, the grid of optical depth
/// answers for the rest of the bank.
const TAPS: [f64; 4] = [0.1, 0.3, 0.7, 1.5];

/// The most steps a ray takes through the bank, seen directly and seen
/// otherwise; and the transmittance below which what lies beyond is hidden.
const FINE_STEPS: u32 = 320;
const COARSE_STEPS: u32 = 32;
/// The most jumps a ray takes through air no cloud can stand in, on each
/// level: into the heights cloud can reach over a weather cell and on out of
/// the cell, for as many cells as a straight line can cross a level's map.
const JUMPS_A_LEVEL: usize = 4 * WEATHER_CELLS;
const OPAQUE: f64 = 0.015;
/// A step through a deck, as a share of its billows, seen directly and seen
/// otherwise: fine enough that no edge shows where a step fell. Clear air
/// between clouds, within the heights they can reach, is stepped through as
/// finely, since a stride through it would step over a cloud's thinner edges.
const FINE_SHARE: f64 = 0.06;
const COARSE_SHARE: f64 = 0.25;
/// How far off a step has doubled in length, as a pixel's view of the cloud
/// has widened.
const STRETCH: f64 = 20_000.0;

/// The asymmetry of rough ice crystals' scattering of visible light (Yang
/// et al., "Spectrally consistent scattering, absorption, and polarization
/// properties of atmospheric ice crystals", 2013), whose phase has no halo.
const ICE_ASYMMETRY: f64 = 0.75;
/// Heights through the bank ice's multiple scattering is tabulated at; the
/// columns sampled at each for its mean extinction; and the directions and
/// steps along each it is gathered over.
const SCATTER_LEVELS: usize = 16;
const SCATTER_COLUMNS: u32 = 256;
const SCATTER_DIRECTIONS: u32 = 64;
const SCATTER_STEPS: u32 = 16;
/// Elevations, from straight down to straight up, the share of ice's phase
/// the sky above sends a way is tabulated at; and the steps out from that
/// way the share is integrated over.
const HEMISPHERE: usize = 33;
const HEMISPHERE_STEPS: u32 = 512;

/// A texture of noise that repeats along each axis, a power of two texels
/// on a side.
#[derive(Clone, Debug)]
struct Tile {
    side: usize,
    texels: Vec<u8>,
}

impl Tile {
    fn new(side: usize) -> Option<Self> {
        Some(Self {
            side,
            texels: fallible::filled(side.checked_mul(side)?.checked_mul(side)?, 0)?,
        })
    }

    /// The most the texture holds anywhere, which no read of it passes.
    fn peak(&self) -> f64 {
        f64::from(self.texels.iter().copied().max().unwrap_or(u8::MAX)) / 255.0
    }

    /// The texture read trilinearly at `p`, in texels, wrapping.
    fn at(&self, p: Vec3) -> f64 {
        let (side, mask) = (self.side, self.side - 1);
        let wrap = |value: f64| {
            let (whole, fraction) = cell(value);
            (usize::try_from(whole).unwrap_or(0) & mask, fraction)
        };
        let ((x, fx), (y, fy), (z, fz)) = (wrap(p.x), wrap(p.y), wrap(p.z));
        let (x1, y1, z1) = ((x + 1) & mask, (y + 1) & mask, (z + 1) & mask);
        let texel = |x: usize, y: usize, z: usize| {
            f64::from(
                self.texels
                    .get((z * side + y) * side + x)
                    .copied()
                    .unwrap_or(0),
            )
        };
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        let face = |z: usize| {
            lerp(
                lerp(texel(x, y, z), texel(x1, y, z), fx),
                lerp(texel(x, y1, z), texel(x1, y1, z), fx),
                fy,
            )
        };
        lerp(face(z), face(z1), fz) / 255.0
    }
}

/// One column of a deck's weather map: how much the deck covers there, and
/// where its cloud's base and top lie.
#[derive(Copy, Clone, Debug, Default)]
struct Column {
    cover: f32,
    base: f32,
    top: f32,
}

impl Column {
    fn lerp(self, other: Self, t: f64) -> Self {
        let mix = |a: f32, b: f32| single(f64::from(a) + (f64::from(b) - f64::from(a)) * t);
        Self {
            cover: mix(self.cover, other.cover),
            base: mix(self.base, other.base),
            top: mix(self.top, other.top),
        }
    }
}

/// A band of heights no cloud stands in.
const EMPTY_BAND: (f32, f32) = (f32::INFINITY, f32::NEG_INFINITY);

/// One level of a bank's maps: a square about the eye, and over it each
/// deck's weather and the heights its cloud can stand between there, the
/// optical depth toward the sun of the bank's cloud and what of the sunlight
/// the bank above lets through, and the shadow the bank casts.
#[derive(Clone, Debug)]
struct Level {
    /// Half its breadth, either way of the bank's middle.
    half: f64,
    /// Each deck's map and its cells' bands, empty for a deck the bank lacks.
    weather: [Vec<Column>; 2],
    bands: [Vec<(f32, f32)>; 2],
    light: Vec<f32>,
    /// Empty with no bank above.
    overhead: Vec<f32>,
    shadow: Vec<f32>,
}

impl Level {
    /// A level `half` either way, its maps reserved whole and written as
    /// their rows are filled, so no unit of building fills more than its own
    /// rows; `None` when the heap will not hold them. The overhead is
    /// reserved once a bank above is known to need it.
    fn new(half: f64, decks: &[Option<Deck>; 2]) -> Option<Self> {
        let maps = |deck: &Option<Deck>| room(deck.is_some(), points(WEATHER_CELLS));
        let banded = |deck: &Option<Deck>| room(deck.is_some(), WEATHER_CELLS * WEATHER_CELLS);
        Some(Self {
            half,
            weather: [maps(&decks[0])?, maps(&decks[1])?],
            bands: [banded(&decks[0])?, banded(&decks[1])?],
            light: room(true, LIGHT_POINTS)?,
            overhead: Vec::new(),
            shadow: room(true, points(SHADOW_CELLS))?,
        })
    }
}

/// The vertices of a square grid of `cells` a side.
const fn points(cells: usize) -> usize {
    (cells + 1) * (cells + 1)
}

/// The points of a level's light grid, layer on layer.
const LIGHT_POINTS: usize = points(LIGHT_CELLS) * LIGHT_LAYERS;

/// Room reserved for `length` values when `held`, and none otherwise;
/// `None` when the heap will not hold it.
fn room<T>(held: bool, length: usize) -> Option<Vec<T>> {
    let mut values = Vec::new();
    if held {
        values.try_reserve_exact(length).ok()?;
    }
    Some(values)
}

/// Rows `rows` of `values`, laid `per` to a row, those not yet held added
/// as `value` within the room reserved for them; `None` when they will not
/// fit.
fn rows_of<T: Clone>(
    values: &mut Vec<T>,
    rows: Range<usize>,
    per: usize,
    value: T,
) -> Option<&mut [T]> {
    let end = rows.end.checked_mul(per)?;
    if values.len() < end {
        values.try_reserve(end - values.len()).ok()?;
        values.resize(end, value);
    }
    values.get_mut(rows.start * per..end)
}

/// Where world `(x, z)` lies on a grid of `cells` across a square `half`
/// either way of `centre`, counted in cells from its south-west corner.
fn place_on(centre: (f64, f64), half: f64, (x, z): (f64, f64), cells: usize) -> (f64, f64) {
    let per = real(cells) / (2.0 * half);
    ((x - centre.0 + half) * per, (z - centre.1 + half) * per)
}

/// Where vertex `(i, j)` of a grid of `cells` across a square `half` either
/// way of `centre` lies.
fn vertex_of(centre: (f64, f64), half: f64, (i, j): (usize, usize), cells: usize) -> (f64, f64) {
    let step = 2.0 * half / real(cells);
    (
        centre.0 - half + real(i) * step,
        centre.1 - half + real(j) * step,
    )
}

/// How far vertex `(i, j)` of a grid of `cells` is blended into the next
/// coarser level's: not at all within the rim, wholly at the level's edge.
fn rim(cells: usize, (i, j): (usize, usize)) -> f64 {
    let middle = 0.5 * real(cells);
    let out = (real(i) - middle).abs().max((real(j) - middle).abs());
    smoothstep((1.0 - RIM) * middle, middle, out)
}

/// Where vertex `(i, j)` of a level's grid of `cells` lies on the next
/// coarser level's, in its cells: exactly, every other vertex on one of its.
fn coarse_place(cells: usize, (i, j): (usize, usize)) -> (f64, f64) {
    let middle = 0.5 * real(cells);
    (
        f64::midpoint(real(i), middle),
        f64::midpoint(real(j), middle),
    )
}

/// The four entries about `(u, v)` of a grid `points` a side — south-west,
/// south-east, north-west, north-east — and how far east and north across
/// them it lies; `None` off the grid.
fn about<T: Copy>(values: &[T], points: usize, (u, v): (f64, f64)) -> Option<([T; 4], (f64, f64))> {
    let limit = real(points.checked_sub(1)?);
    if !((0.0..limit).contains(&u) && (0.0..limit).contains(&v)) {
        return None;
    }
    let ((i, east), (j, north)) = (cell_of(u), cell_of(v));
    let at = |i: usize, j: usize| values.get(j * points + i).copied();
    Some((
        [at(i, j)?, at(i + 1, j)?, at(i, j + 1)?, at(i + 1, j + 1)?],
        (east, north),
    ))
}

/// Layer `layer` of `values`, a grid `points` a side laid layer on layer,
/// read bilinearly at `(u, v)`; `None` off it.
fn plane(values: &[f32], points: usize, layer: usize, (u, v): (f64, f64)) -> Option<f64> {
    let first = layer.checked_mul(points * points)?;
    let (corners, across) = about(values.get(first..)?, points, (u, v))?;
    Some(bilinear(corners.map(f64::from), across))
}

/// Whether cell `(column, row)` of a level's weather map lies within the
/// next finer level's square.
fn inner((column, row): (usize, usize)) -> bool {
    let within = WEATHER_CELLS / 4..3 * WEATHER_CELLS / 4;
    within.contains(&column) && within.contains(&row)
}

/// How far a bank is built.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    Shape(usize),
    Detail(usize),
    /// A level's weather, from the coarsest in, each followed by its bands:
    /// by level, then row.
    Weather(usize, usize),
    Bands(usize, usize),
    /// Each level's light, then each level's shadow, from the coarsest in.
    Light(usize, usize),
    Shadow(usize, usize),
    Scatter,
    Done,
}

impl Stage {
    /// The rows the stage fills, and how many of them a core fills in a
    /// unit: each unit some milliseconds' work.
    const fn extent(self) -> (usize, usize) {
        match self {
            Self::Shape(_) => (SHAPE_SIDE, 1),
            Self::Detail(_) => (DETAIL_SIDE, 4),
            Self::Weather(..) => (WEATHER_CELLS + 1, 8),
            Self::Bands(..) => (WEATHER_CELLS, 16),
            Self::Light(..) => (LIGHT_LAYERS * (LIGHT_CELLS + 1), LIGHT_ROWS),
            Self::Shadow(..) => (SHADOW_CELLS + 1, SHADOW_ROWS),
            Self::Scatter | Self::Done => (1, 1),
        }
    }

    /// The next row the stage fills.
    const fn row(self) -> usize {
        match self {
            Self::Shape(row)
            | Self::Detail(row)
            | Self::Weather(_, row)
            | Self::Bands(_, row)
            | Self::Light(_, row)
            | Self::Shadow(_, row) => row,
            Self::Scatter | Self::Done => 0,
        }
    }

    /// The stage on at row `row`.
    const fn at(self, row: usize) -> Self {
        match self {
            Self::Shape(_) => Self::Shape(row),
            Self::Detail(_) => Self::Detail(row),
            Self::Weather(level, _) => Self::Weather(level, row),
            Self::Bands(level, _) => Self::Bands(level, row),
            Self::Light(level, _) => Self::Light(level, row),
            Self::Shadow(level, _) => Self::Shadow(level, row),
            Self::Scatter | Self::Done => self,
        }
    }

    /// The stage after this one in a bank of `levels` levels.
    fn after(self, levels: usize) -> Self {
        let coarsest = |stage: fn(usize, usize) -> Self| {
            levels
                .checked_sub(1)
                .map_or(Self::Scatter, |level| stage(level, 0))
        };
        match self {
            Self::Shape(_) => Self::Detail(0),
            Self::Detail(_) => coarsest(Self::Weather),
            Self::Weather(level, _) => Self::Bands(level, 0),
            Self::Bands(level, _) => level
                .checked_sub(1)
                .map_or_else(|| coarsest(Self::Light), |finer| Self::Weather(finer, 0)),
            Self::Light(level, _) => level
                .checked_sub(1)
                .map_or_else(|| coarsest(Self::Shadow), |finer| Self::Light(finer, 0)),
            Self::Shadow(level, _) => level
                .checked_sub(1)
                .map_or(Self::Scatter, |finer| Self::Shadow(finer, 0)),
            Self::Scatter | Self::Done => Self::Done,
        }
    }
}

/// What lights the bank, once the air says.
#[derive(Clone, Debug)]
pub(crate) struct Lighting {
    /// The sun's light reaching the bank: for each of its heights from its
    /// floor to its ceiling, at each of the cosines from `cosines.0` to
    /// `cosines.1` of the sun's angle from the vertical there.
    pub(crate) sunlight: Vec<Vec3>,
    pub(crate) cosines: (f64, f64),
    /// The true unit direction toward the sun those angles are taken from.
    pub(crate) toward: Vec3,
    /// The sky's light on a cloud's upper side, and the ground's on its
    /// underside.
    pub(crate) above: Vec3,
    pub(crate) below: Vec3,
}

/// What a ray meets of a bank: the light its cloud sends back along the ray,
/// how much of what lies beyond that cloud shows through, and how far off
/// the cloud lies on the whole, if it meets any; and where the ray leaves
/// the bank.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Seen {
    pub(crate) cloud: Option<(Vec3, f64, f64)>,
    pub(crate) leave: f64,
}

/// A bank of volumetric cloud.
#[derive(Clone, Debug)]
pub(crate) struct Cloudbank {
    decks: [Option<Deck>; 2],
    /// The most its finest level spans either way of the eye, and its levels
    /// about the eye once it stands, finest first.
    most: f64,
    levels: Vec<Level>,
    /// Where the eye stands, which the Earth's centre lies straight below,
    /// the scene's level `ground` metres from it and the sea `sea` metres
    /// below that level.
    centre: (f64, f64),
    ground: f64,
    sea: f64,
    /// The heights between which any of it lies.
    floor: f64,
    ceiling: f64,
    /// The unit direction its sunlight comes from, as the air bends it to
    /// the bank's height.
    sun: Vec3,
    /// Each deck's heading, as its cosine and sine, and its fibre's
    /// reciprocal.
    headings: [(f64, f64, f64); 2],
    shape: Tile,
    detail: Tile,
    /// The most the billows reach, once their texture is built.
    peak: f64,
    /// What ice scatters again and again at each height, per unit of the
    /// sunlight there; and for each way toward the eye, the share of ice's
    /// phase the sky above sends it.
    scatter: [f32; SCATTER_LEVELS],
    downward: [f32; HEMISPHERE],
    lighting: Option<Lighting>,
    stage: Stage,
}

impl Cloudbank {
    /// A bank of `decks` lit from the unit `sun` over an Earth whose centre
    /// lies `ground` metres below the scene's level, its finest level
    /// spanning no more than `most` either way of the eye; to be stood about
    /// the eye before it is built. `None` when the heap will not hold its
    /// textures.
    pub(crate) fn new(decks: [Option<Deck>; 2], most: f64, sun: Vec3, ground: f64) -> Option<Self> {
        let (mut floor, mut ceiling) = (f64::INFINITY, f64::NEG_INFINITY);
        for deck in decks.iter().flatten() {
            floor = floor.min(deck.base - deck.base_spread);
            ceiling = ceiling.max(deck.base + deck.base_spread + deck.depth.1);
        }
        let floor = floor.max(0.0);
        if floor >= ceiling || !(most > 0.0 && ground > 0.0) {
            return None;
        }
        Some(Self {
            decks,
            most,
            levels: Vec::new(),
            centre: (0.0, 0.0),
            ground,
            sea: GROUND * 1000.0 - ground,
            floor,
            ceiling,
            sun,
            headings: decks.map(|deck| {
                deck.map_or((1.0, 0.0, 1.0), |deck| {
                    (
                        mathf::cos(deck.heading),
                        mathf::sin(deck.heading),
                        1.0 / deck.fibre.max(1.0),
                    )
                })
            }),
            shape: Tile::new(SHAPE_SIDE)?,
            detail: Tile::new(DETAIL_SIDE)?,
            peak: 1.0,
            scatter: [0.0; SCATTER_LEVELS],
            downward: [0.0; HEMISPHERE],
            lighting: None,
            stage: Stage::Shape(0),
        })
    }

    /// Lay the bank's levels about `eye`, out to as far as any of its cloud
    /// can be seen from there: where the eye's sight grazing the sea runs on
    /// to the ceiling. Its building starts afresh. `None` when the heap will
    /// not hold the levels.
    pub(crate) fn stand(&mut self, eye: Vec3) -> Option<()> {
        self.centre = (eye.x, eye.z);
        let grazing = |height: f64| {
            mathf::sqrt(((height - self.sea) * (height + self.sea + 2.0 * self.ground)).max(0.0))
        };
        let reach = grazing(eye.y) + grazing(self.ceiling);
        if !(reach.is_finite() && reach > 0.0) {
            return None;
        }
        let (mut half, mut count) = (reach, 1);
        while half > self.most && count < MOST_LEVELS {
            half *= 0.5;
            count += 1;
        }
        let mut levels = room(true, count)?;
        for _ in 0..count {
            levels.push(Level::new(half, &self.decks)?);
            half *= 2.0;
        }
        self.levels = levels;
        self.stage = Stage::Shape(0);
        Some(())
    }

    /// Light the bank from the unit `sun`, the way its sunlight comes, before
    /// its light is built.
    pub(crate) fn aim(&mut self, sun: Vec3) {
        self.sun = sun;
    }

    /// Where deck `slot`'s billows are read for `point`: drawn out along its
    /// heading by its fibre, so ice streaks with the wind.
    fn grained(&self, slot: usize, deck: &Deck, point: Vec3) -> Vec3 {
        if deck.fibre <= 1.0 {
            return point;
        }
        let (cos, sin, thinning) = self.headings.get(slot).copied().unwrap_or((1.0, 0.0, 1.0));
        Vec3::new(
            (point.x * cos + point.z * sin) * thinning,
            point.y,
            point.z * cos - point.x * sin,
        )
    }

    /// The heights between which the bank lies.
    pub(crate) const fn span(&self) -> (f64, f64) {
        (self.floor, self.ceiling)
    }

    /// The least and the greatest cosine of the true sun `toward`'s angle
    /// from the vertical anywhere over the bank.
    pub(crate) fn sun_cosines(&self, toward: Vec3) -> (f64, f64) {
        let tilt = self
            .levels
            .last()
            .map_or(0.0, |outer| mathf::atan2(SQRT_2 * outer.half, self.ground));
        let zenith = mathf::acos(toward.y.clamp(-1.0, 1.0));
        (
            mathf::cos((zenith + tilt).min(PI)),
            mathf::cos((zenith - tilt).max(0.0)),
        )
    }

    /// Light the bank as `lighting` has it.
    pub(crate) fn light_by(&mut self, lighting: Lighting) {
        self.lighting = Some(lighting);
    }

    /// Build the next unit of the bank across `runner`, its sunlight already
    /// dimmed by the bank `above` it; whether it is built, or `None` when the
    /// heap will not hold it.
    pub(crate) fn step(&mut self, runner: &dyn JobRunner, above: Option<&Self>) -> Option<bool> {
        let stage = self.stage;
        let (rows, per) = stage.extent();
        let start = stage.row();
        let end = (start + per * runner.width().max(1)).min(rows);
        match stage {
            Stage::Shape(_) => {
                let seed = self.seed();
                fill_tile(&mut self.shape, start..end, runner, &|p| billows(p, seed));
                if end >= rows {
                    self.peak = self.shape.peak();
                }
            }
            Stage::Detail(_) => {
                let seed = self.seed() ^ 0xd37a;
                fill_tile(&mut self.detail, start..end, runner, &|p| wisps(p, seed));
            }
            Stage::Weather(level, _) => self.fill_weather(level, start..end, runner),
            Stage::Bands(level, _) => self.fill_bands(level, start..end, runner),
            Stage::Light(level, _) => self.fill_light(level, start..end, runner, above)?,
            Stage::Shadow(level, _) => self.fill_shadow(level, start..end, runner)?,
            Stage::Scatter => self.fill_scatter(),
            Stage::Done => {}
        }
        self.stage = if end >= rows {
            stage.after(self.levels.len())
        } else {
            stage.at(end)
        };
        Some(self.stage == Stage::Done)
    }

    /// The units of building done and to do, each a core's work at its
    /// stage, so a runner of any width does them in the same proportion.
    pub(crate) fn units(&self) -> (f64, f64) {
        let mut stage = Stage::Shape(0);
        let (mut done, mut total) = (0.0, 0.0);
        while stage != Stage::Done {
            let (rows, per) = stage.extent();
            let units = real(rows) / real(per);
            if stage == self.stage.at(0) {
                done = total + real(self.stage.row()) / real(per);
            }
            total += units;
            stage = stage.after(self.levels.len());
        }
        (
            if self.stage == Stage::Done {
                total
            } else {
                done
            },
            total,
        )
    }

    /// The seed the bank's textures are drawn from: its first deck's.
    fn seed(&self) -> u32 {
        self.decks
            .iter()
            .flatten()
            .next()
            .map_or(1, |deck| deck.seed)
    }

    /// Level `index`'s weather rows `rows`, its rim blended into the next
    /// coarser level's, which stands already.
    fn fill_weather(&mut self, index: usize, rows: Range<usize>, runner: &dyn JobRunner) {
        let (decks, centre, points) = (self.decks, self.centre, WEATHER_CELLS + 1);
        if index >= self.levels.len() {
            return;
        }
        let (finer, coarser) = self.levels.split_at_mut(index + 1);
        let Some(level) = finer.last_mut() else {
            return;
        };
        let next = coarser.first();
        let (half, spacing) = (level.half, 2.0 * level.half / real(WEATHER_CELLS));
        for ((deck, map), coarse) in decks
            .iter()
            .zip(level.weather.iter_mut())
            .zip([0, 1].map(|slot| next.and_then(|next| next.weather.get(slot))))
        {
            let Some(deck) = deck else {
                continue;
            };
            let Some(cells) = rows_of(map, rows.clone(), points, Column::default()) else {
                continue;
            };
            band::for_each(runner, cells, (rows.start, points), &|row, band| {
                for (column, cell) in band.iter_mut().enumerate() {
                    let at = vertex_of(centre, half, (column, row), WEATHER_CELLS);
                    let fine = weather(deck, at, spacing);
                    let rimmed = rim(WEATHER_CELLS, (column, row));
                    *cell = coarse
                        .filter(|_| rimmed > 0.0)
                        .and_then(|coarse| {
                            about(coarse, points, coarse_place(WEATHER_CELLS, (column, row)))
                        })
                        .map_or(fine, |(corners, across)| {
                            fine.lerp(Blend { corners, across }.column(), rimmed)
                        });
                }
            });
        }
    }

    /// Level `index`'s bands over its weather cells' rows `rows`.
    fn fill_bands(&mut self, index: usize, rows: Range<usize>, runner: &dyn JobRunner) {
        let (decks, peak) = (self.decks, self.peak);
        let Some(Level { weather, bands, .. }) = self.levels.get_mut(index) else {
            return;
        };
        for ((deck, map), cells) in decks.iter().zip(weather.iter()).zip(bands.iter_mut()) {
            let Some(deck) = deck else {
                continue;
            };
            let Some(cells) = rows_of(cells, rows.clone(), WEATHER_CELLS, EMPTY_BAND) else {
                continue;
            };
            band::for_each(runner, cells, (rows.start, WEATHER_CELLS), &|row, band| {
                for (column, slot) in band.iter_mut().enumerate() {
                    *slot = band_over(deck, map, (column, row), peak);
                }
            });
        }
    }

    /// Level `index`'s light grid rows `rows`, counted up through its
    /// layers: at each point, its own cloud's optical depth toward the sun,
    /// and what of the sunlight the bank `above` lets through, its rim
    /// blended into the next coarser level's; `None` when the heap will not
    /// hold them. Kept apart, since cloud above dims the sunlight a cloud's
    /// scattering takes in, and adds nothing to the depth its own edges are
    /// powdered and its octaves spread by.
    fn fill_light(
        &mut self,
        index: usize,
        rows: Range<usize>,
        runner: &dyn JobRunner,
        above: Option<&Self>,
    ) -> Option<()> {
        let points = LIGHT_CELLS + 1;
        let mut values = fallible::filled(rows.len() * points, (0.0f32, 1.0f32))?;
        {
            let reader = Reader { bank: self };
            let level = self.levels.get(index)?;
            let next = self.levels.get(index + 1);
            let up = (self.ceiling - self.floor) / real(LIGHT_LAYERS - 1);
            band::for_each(runner, &mut values, (rows.start, points), &|row, band| {
                let (layer, j) = (row / points, row % points);
                let altitude = self.floor + real(layer) * up;
                for (i, slot) in band.iter_mut().enumerate() {
                    let (x, z) = vertex_of(self.centre, level.half, (i, j), LIGHT_CELLS);
                    let point = Vec3::new(x, self.height_at(altitude, (x, z)), z);
                    let mut depth = reader.toward_sun(point, LIGHT_STEPS);
                    let mut kept = above.map_or(1.0, |high| high.shadow(point, self.sun));
                    let rimmed = rim(LIGHT_CELLS, (i, j));
                    if let Some(next) = next.filter(|_| rimmed > 0.0) {
                        let place = coarse_place(LIGHT_CELLS, (i, j));
                        if let Some(coarse) = plane(&next.light, points, layer, place) {
                            depth += (coarse - depth) * rimmed;
                        }
                        if let Some(coarse) = plane(&next.overhead, points, layer, place) {
                            kept += (coarse - kept) * rimmed;
                        }
                    }
                    *slot = (single(depth), single(kept));
                }
            });
        }
        let level = self.levels.get_mut(index)?;
        for (slot, &(depth, _)) in rows_of(&mut level.light, rows.clone(), points, 0.0)?
            .iter_mut()
            .zip(&values)
        {
            *slot = depth;
        }
        if above.is_some() {
            if level.overhead.capacity() == 0 {
                level.overhead = room(true, LIGHT_POINTS)?;
            }
            for (slot, &(_, kept)) in rows_of(&mut level.overhead, rows, points, 1.0)?
                .iter_mut()
                .zip(&values)
            {
                *slot = kept;
            }
        }
        Some(())
    }

    /// Level `index`'s shadow map rows `rows`: what of the sun's light
    /// crosses the whole bank to each point of its floor, its rim blended
    /// into the next coarser level's.
    fn fill_shadow(
        &mut self,
        index: usize,
        rows: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let points = SHADOW_CELLS + 1;
        let mut values = fallible::filled(rows.len() * points, 1.0f32)?;
        {
            let reader = Reader { bank: self };
            let level = self.levels.get(index)?;
            let next = self.levels.get(index + 1);
            band::for_each(runner, &mut values, (rows.start, points), &|j, band| {
                for (i, slot) in band.iter_mut().enumerate() {
                    let (x, z) = vertex_of(self.centre, level.half, (i, j), SHADOW_CELLS);
                    let point = Vec3::new(x, self.height_at(self.floor, (x, z)), z);
                    let mut kept = mathf::exp(-reader.toward_sun(point, LIGHT_STEPS * 2));
                    let rimmed = rim(SHADOW_CELLS, (i, j));
                    if let Some(coarse) = next.filter(|_| rimmed > 0.0).and_then(|next| {
                        plane(&next.shadow, points, 0, coarse_place(SHADOW_CELLS, (i, j)))
                    }) {
                        kept += (coarse - kept) * rimmed;
                    }
                    *slot = single(kept);
                }
            });
        }
        for (slot, &kept) in rows_of(&mut self.levels.get_mut(index)?.shadow, rows, points, 1.0)?
            .iter_mut()
            .zip(&values)
        {
            *slot = kept;
        }
        Some(())
    }

    /// What ice scatters again and again at each height through the bank,
    /// per unit of the sunlight there: Hillaire's isotropic series, its
    /// scattering scaled by `1 − g` for how far forward ice throws its light,
    /// over the bank laid out as its mean extinction by height and endless
    /// across. Nothing where the bank holds no ice.
    fn fill_scatter(&mut self) {
        let Some(half) = self.levels.first().map(|finest| finest.half) else {
            return;
        };
        if !self
            .decks
            .iter()
            .flatten()
            .any(|deck| deck.matter == Matter::Ice)
        {
            return;
        }
        self.downward = downward_shares();
        let reader = Reader { bank: self };
        let (floor, span) = (self.floor, self.ceiling - self.floor);
        let level_height = |level: usize| floor + span * real(level) / real(SCATTER_LEVELS - 1);
        let scale = 1.0 - ICE_ASYMMETRY;
        let mut mean = [0.0f64; SCATTER_LEVELS];
        for (level, slot) in mean.iter_mut().enumerate() {
            let height = level_height(level);
            let mut total = 0.0;
            for column in 0..SCATTER_COLUMNS {
                // Columns spread evenly over the finest level: one coordinate
                // in even steps, the other in the golden ratio's.
                let u = (f64::from(column) + 0.5) / f64::from(SCATTER_COLUMNS);
                let (_, v) = cell(f64::from(column) * GOLDEN_RATIO);
                let (x, z) = (
                    self.centre.0 + half * (2.0 * u - 1.0),
                    self.centre.1 + half * (2.0 * v - 1.0),
                );
                let point = Vec3::new(x, self.height_at(height, (x, z)), z);
                if let Some(sample) = reader.density(point, false) {
                    if sample.matter == Matter::Ice {
                        total += sample.density * sample.thickness;
                    }
                }
            }
            *slot = scale * total / f64::from(SCATTER_COLUMNS);
        }
        let profile = Profile::new(mean, (self.floor, self.ceiling));
        // The longest way through the shell the deck lies in, grazing its
        // floor.
        let breadth = 2.0
            * mathf::sqrt(
                (self.ceiling - self.floor) * (self.ceiling + self.floor + 2.0 * self.ground),
            );
        let sun = self.sun;
        let isotropic = 1.0 / (4.0 * PI);
        let mut scatter = [0.0f32; SCATTER_LEVELS];
        for (level, slot) in scatter.iter_mut().enumerate() {
            let height = level_height(level);
            let (mut second, mut transfer) = (0.0, 0.0);
            for index in 0..SCATTER_DIRECTIONS {
                // Directions spread evenly over the sphere by their rise
                // alone, the slab the same whichever way across it they head.
                let rise = 1.0 - 2.0 * (f64::from(index) + 0.5) / f64::from(SCATTER_DIRECTIONS);
                let length = if rise > 1e-6 {
                    (self.ceiling - height) / rise
                } else if rise < -1e-6 {
                    (height - self.floor) / -rise
                } else {
                    breadth
                }
                .min(breadth);
                let step = length / f64::from(SCATTER_STEPS);
                let mut kept = 1.0;
                for sample in 0..SCATTER_STEPS {
                    let at = height + rise * (f64::from(sample) + 0.5) * step;
                    let through = mathf::exp(-profile.at(at) * step);
                    let gathered = kept * (1.0 - through);
                    second += gathered * profile.sunlit(at, sun, breadth) * isotropic;
                    transfer += gathered;
                    kept *= through;
                }
            }
            let mean = 1.0 / f64::from(SCATTER_DIRECTIONS);
            *slot = single(every_order(second * mean, transfer * mean));
        }
        self.scatter = scatter;
    }

    /// The ice's scattering again and again `height` of the way from the
    /// bank's floor to its ceiling, per unit of sunlight.
    fn scattered(&self, height: f64) -> f64 {
        along(
            &self.scatter,
            height.clamp(0.0, 1.0) * real(SCATTER_LEVELS - 1),
        )
    }

    /// The share of ice's phase the sky above sends a ray leaving its cloud
    /// along the unit `out`: the rest the ground below sends.
    fn sent_down(&self, out: Vec3) -> f64 {
        along(
            &self.downward,
            f64::midpoint(out.y.clamp(-1.0, 1.0), 1.0) * real(HEMISPHERE - 1),
        )
    }

    /// What of the sun's light toward the unit `toward` reaches `point` past
    /// the bank: the shadow at the floor where the way toward the sun rises
    /// through it, or at the point itself within the bank. Nothing is
    /// shaded where the Earth stands in that way, the air answering for it.
    pub(crate) fn shadow(&self, point: Vec3, toward: Vec3) -> f64 {
        let altitude = self.altitude(point);
        if altitude >= self.ceiling {
            return 1.0;
        }
        let at = if altitude < self.floor {
            let course = self.course_from(point, altitude, toward);
            let Some((_, rise)) = course.crossings(self.floor) else {
                return 1.0;
            };
            if course
                .crossings(self.sea)
                .is_some_and(|(earth, _)| (0.0..rise).contains(&earth))
            {
                return 1.0;
            }
            point + toward * rise
        } else {
            point
        };
        self.level_over(at)
            .and_then(|level| {
                let place = place_on(self.centre, level.half, (at.x, at.z), SHADOW_CELLS);
                plane(&level.shadow, SHADOW_CELLS + 1, 0, place)
            })
            .unwrap_or(1.0)
    }

    /// What of its own sunlight the bank lets reach `point`.
    pub(crate) fn sunlit(&self, point: Vec3) -> f64 {
        self.shadow(point, self.sun)
    }

    /// What a ray from `origin` along the unit `dir` meets of the bank;
    /// `None` when it does not run through it. `fine` for a ray seen
    /// directly, which takes every step and the cloud's finest edges;
    /// `jitter`, in `0.0..1.0`, staggers where along each step its cloud is
    /// read, so no two samples of a pixel band alike.
    pub(crate) fn seen(&self, origin: Vec3, dir: Vec3, fine: bool, jitter: f64) -> Option<Seen> {
        let lighting = self.lighting.as_ref()?;
        let (enter, leave) = self.crossing(origin, dir)?;
        let reader = Reader { bank: self };
        let (share, most) = if fine {
            (FINE_SHARE, FINE_STEPS)
        } else {
            (COARSE_SHARE, COARSE_STEPS)
        };
        let billow = self
            .decks
            .iter()
            .flatten()
            .map(|deck| deck.billow)
            .fold(f64::INFINITY, f64::min);
        // A step through cloud: a share of its finest billows, longer far off
        // where a pixel spans more of it, and longer still where the steps
        // left would not otherwise reach the bank's far side.
        let within = |t: f64, taken: u32| {
            let left = f64::from(most.saturating_sub(taken).max(1));
            (billow * share * (1.0 + t / STRETCH)).max((leave - t) / left)
        };
        let cos = dir.dot(self.sun);
        let phases = PHASES.map(|(g, back)| dual_phase(cos, g, back));
        let (ice, downward) = (henyey_greenstein(cos, ICE_ASYMMETRY), self.sent_down(-dir));
        let per_height = 1.0 / (self.ceiling - self.floor);
        let course = self.course(origin, dir);
        let mut cursor = self.cursor(origin, dir, enter);
        let mut over = self.over(&cursor);
        let most_jumps = JUMPS_A_LEVEL * self.levels.len();
        let mut t = enter;
        let mut kept = 1.0;
        let mut light = Vec3::ZERO;
        let mut depth = 0.0;
        let mut weight = 0.0;
        let mut steps = 0;
        let mut jumps = 0_usize;
        while t < leave && steps < most {
            let step = within(t, steps + 1).min(leave - t);
            if t >= over.until {
                self.advance(&mut cursor, origin, dir, t);
                over = self.over(&cursor);
            }
            // Air no cloud can stand in is crossed in one jump to where the
            // ray might next meet some, which spends none of the steps the
            // cloud beyond it is owed.
            if let Some(next) = over.clear(&course, self.altitude(origin + dir * t), t) {
                jumps += 1;
                if jumps > most_jumps {
                    break;
                }
                t = next.max(t + 1e-3 * step);
                continue;
            }
            steps += 1;
            let at = t + step * jitter;
            let point = origin + dir * at;
            let altitude = self.altitude(point);
            let Some(sample) = reader
                .density_at(point, altitude, fine)
                .filter(|sample| sample.density > 1e-4)
            else {
                t += step;
                continue;
            };
            let extinction = sample.density * sample.thickness;
            let optical = reader.optical_depth(point, fine, sample.billow);
            let height = ((altitude - self.floor) * per_height).clamp(0.0, 1.0);
            let cosine = self.cosine(point, altitude, lighting.toward);
            let sun = sunlight_at(lighting, height, cosine) * reader.overhead_at(point, altitude);
            let source = match sample.matter {
                Matter::Water => {
                    let mut scattered = Vec3::ZERO;
                    for (octave, phase) in phases.iter().enumerate() {
                        let (a, b) = OCTAVES[octave];
                        scattered += sun * (b * phase * mathf::exp(-a * optical));
                    }
                    // Powdered edges: a cloud's rim facing the sun is darker
                    // than its body, where light has scattered in from all
                    // sides.
                    let powder = 1.0 - 0.6 * mathf::exp(-2.2 * optical);
                    let low = 1.0 - sample.within;
                    let ambient = lighting.above.lerp(lighting.below, low * low)
                        * (0.35 + 0.65 * sample.within);
                    // An evenly lit sky scatters in its whole light, the phase
                    // function summing to one over the sphere.
                    (scattered * powder + ambient) * extinction
                }
                Matter::Ice => {
                    let sunward =
                        ice * mathf::exp(-optical) + (1.0 - ICE_ASYMMETRY) * self.scattered(height);
                    let sky = lighting.above * downward + lighting.below * (1.0 - downward);
                    (sun * sunward + sky) * extinction
                }
            };
            let through = mathf::exp(-extinction * step);
            let gained = (1.0 - through) / extinction.max(1e-12);
            light += source * (kept * gained);
            let lost = kept * (1.0 - through);
            depth += at * lost;
            weight += lost;
            kept *= through;
            if kept < OPAQUE {
                kept = 0.0;
                break;
            }
            t += step;
        }
        Some(Seen {
            cloud: (weight > 1e-6).then(|| (light, kept, depth / weight)),
            leave,
        })
    }

    /// `point`'s height above the scene's level over the Earth's curve.
    fn altitude(&self, point: Vec3) -> f64 {
        let (east, north) = (point.x - self.centre.0, point.z - self.centre.1);
        let across = east * east + north * north;
        let up = point.y + self.ground;
        // The difference of two radii, divided out so no precision is lost
        // to their size.
        (across + point.y * (point.y + 2.0 * self.ground))
            / (mathf::sqrt(across + up * up) + self.ground)
    }

    /// Where over `(x, z)` the height `altitude` above the scene's level
    /// lies.
    fn height_at(&self, altitude: f64, (x, z): (f64, f64)) -> f64 {
        let (east, north) = (x - self.centre.0, z - self.centre.1);
        let across = east * east + north * north;
        let radius = self.ground + altitude;
        (altitude * (altitude + 2.0 * self.ground) - across)
            / (mathf::sqrt((radius * radius - across).max(0.0)) + self.ground)
    }

    /// The cosine of the unit `toward`'s angle from the vertical at `point`,
    /// `altitude` above the scene's level.
    fn cosine(&self, point: Vec3, altitude: f64, toward: Vec3) -> f64 {
        let up = Vec3::new(
            point.x - self.centre.0,
            point.y + self.ground,
            point.z - self.centre.1,
        );
        up.dot(toward) / (self.ground + altitude)
    }

    /// How the height of a ray from `origin` along the unit `dir` runs.
    fn course(&self, origin: Vec3, dir: Vec3) -> Course {
        self.course_from(origin, self.altitude(origin), dir)
    }

    /// [`Self::course`] for an `origin` known to lie `altitude` above the
    /// scene's level.
    fn course_from(&self, origin: Vec3, altitude: f64, dir: Vec3) -> Course {
        let offset = Vec3::new(
            origin.x - self.centre.0,
            origin.y + self.ground,
            origin.z - self.centre.1,
        );
        Course {
            start: altitude,
            along: offset.dot(dir),
            ground: self.ground,
        }
    }

    /// Where a ray from `origin` along the unit `dir` runs through the bank:
    /// on from its origin, or from where it rises through the floor if it
    /// starts beneath, over the coarsest level's square and beneath the
    /// bank's ceiling, and short of the Earth, which hides what lies past it.
    fn crossing(&self, origin: Vec3, dir: Vec3) -> Option<(f64, f64)> {
        let outer = self.levels.last()?;
        let (x, z, half) = (self.centre.0, self.centre.1, outer.half);
        let square = Corners {
            min: [
                x - half,
                self.height_at(self.floor, (x + half, z + half)),
                z - half,
            ],
            max: [x + half, self.ceiling, z + half],
        };
        let (enter, leave) = square.crossing(origin, reciprocal(dir));
        let course = self.course(origin, dir);
        let (under, beyond) = course.crossings(self.ceiling)?;
        let mut enter = enter.max(under);
        // No cloud stands below the floor, so a ray from beneath it need not
        // walk the cells it passes under.
        if course.start < self.floor {
            enter = enter.max(course.crossings(self.floor)?.1);
        }
        let mut leave = leave.min(beyond);
        if let Some((earth, _)) = course.crossings(self.sea) {
            if earth > 0.0 {
                leave = leave.min(earth);
            }
        }
        (enter < leave).then_some((enter, leave))
    }

    /// The finest level whose square holds `(x, z)`.
    fn finest(&self, (x, z): (f64, f64)) -> Option<usize> {
        let out = (x - self.centre.0).abs().max((z - self.centre.1).abs());
        self.levels.iter().position(|level| out < level.half)
    }

    /// The finest level over `point`.
    fn level_over(&self, point: Vec3) -> Option<&Level> {
        self.levels.get(self.finest((point.x, point.z))?)
    }

    /// The walk across level `level`'s weather map of a ray from `origin`
    /// along the unit `dir`, from `t` along it.
    fn walk(&self, level: usize, origin: Vec3, dir: Vec3, t: f64) -> Walk {
        let point = origin + dir * t;
        let half = self.levels.get(level).map_or(self.most, |level| level.half);
        let per = real(WEATHER_CELLS) / (2.0 * half);
        let axis = |at: f64, centre: f64, toward: f64| {
            let place = (at - centre + half) * per;
            // Rounding may leave a ray entering at the rim a hair off the map.
            let cell = cell_of(place).0.min(WEATHER_CELLS - 1);
            let speed = toward * per;
            let (step, next) = if speed > 0.0 {
                (1, t + (real(cell) + 1.0 - place) / speed)
            } else if speed < 0.0 {
                (-1, t + (real(cell) - place) / speed)
            } else {
                (0, f64::INFINITY)
            };
            (
                isize::try_from(cell).unwrap_or(isize::MAX),
                step,
                next,
                1.0 / speed.abs(),
            )
        };
        let (east, north) = (
            axis(point.x, self.centre.0, dir.x),
            axis(point.z, self.centre.1, dir.z),
        );
        Walk {
            cell: [east.0, north.0],
            step: [east.1, north.1],
            next: [east.2, north.2],
            apart: [east.3, north.3],
        }
    }

    /// Where a march along a ray from `origin` along the unit `dir` stands
    /// `t` along it: over the finest level that holds it.
    fn cursor(&self, origin: Vec3, dir: Vec3, t: f64) -> Cursor {
        let point = origin + dir * t;
        let level = self
            .finest((point.x, point.z))
            .unwrap_or(self.levels.len().saturating_sub(1));
        Cursor {
            level,
            walk: self.walk(level, origin, dir, t),
        }
    }

    /// `cursor` on along its ray to `t`: across its level's map, and onto the
    /// finest level holding the point it has come to once it leaves its
    /// level's edge or crosses into a finer level's square.
    fn advance(&self, cursor: &mut Cursor, origin: Vec3, dir: Vec3, t: f64) {
        let before = cursor.walk.cell();
        cursor.walk.advance(t);
        let now = cursor.walk.cell();
        let outward = now.is_none() && cursor.level + 1 < self.levels.len();
        let inward = cursor.level > 0 && now.is_some_and(inner) && !before.is_some_and(inner);
        if outward || inward {
            *cursor = self.cursor(origin, dir, t);
        }
    }

    /// What a march at `cursor` stands over: the heights each deck's cloud
    /// in its cell can stand between, none off the map, and where it leaves
    /// the cell.
    fn over(&self, cursor: &Cursor) -> Over {
        let widen = |(low, high): (f32, f32)| (f64::from(low), f64::from(high));
        let bands = self
            .levels
            .get(cursor.level)
            .zip(cursor.walk.cell())
            .map_or([widen(EMPTY_BAND); 2], |(level, (column, row))| {
                level.bands.each_ref().map(|bands| {
                    widen(
                        bands
                            .get(row * WEATHER_CELLS + column)
                            .copied()
                            .unwrap_or(EMPTY_BAND),
                    )
                })
            });
        Over {
            bands,
            until: cursor.walk.until(),
        }
    }
}

/// How the height above the scene's level of a ray runs along it: its
/// origin's height, the origin's offset from the Earth's centre along the
/// ray, and the scene's level's distance from that centre.
#[derive(Copy, Clone, Debug)]
struct Course {
    start: f64,
    along: f64,
    ground: f64,
}

impl Course {
    /// Where along the ray its height is `height`, the nearer first; `None`
    /// where it never is.
    fn crossings(&self, height: f64) -> Option<(f64, f64)> {
        quadratic(
            1.0,
            self.along,
            (self.start - height) * (self.start + height + 2.0 * self.ground),
        )
    }
}

/// A bank's ice by height, its extinction scaled for how far forward ice
/// throws its light, laid out as a slab endless across.
struct Profile {
    floor: f64,
    ceiling: f64,
    /// How far apart its levels lie.
    gap: f64,
    /// The extinction at each level, evenly spaced floor to ceiling, and the
    /// depth of the slab beneath each.
    levels: [f64; SCATTER_LEVELS],
    beneath: [f64; SCATTER_LEVELS],
}

impl Profile {
    fn new(levels: [f64; SCATTER_LEVELS], (floor, ceiling): (f64, f64)) -> Self {
        let gap = (ceiling - floor) / real(SCATTER_LEVELS - 1);
        let mut beneath = [0.0; SCATTER_LEVELS];
        for index in 1..SCATTER_LEVELS {
            beneath[index] =
                beneath[index - 1] + f64::midpoint(levels[index - 1], levels[index]) * gap;
        }
        Self {
            floor,
            ceiling,
            gap,
            levels,
            beneath,
        }
    }

    /// Where `height` lies among the levels, and how far toward the next.
    fn place(&self, height: f64) -> (usize, f64) {
        let place = ((height - self.floor) / self.gap).clamp(0.0, real(SCATTER_LEVELS - 1));
        let index = cell_of(place).0.min(SCATTER_LEVELS - 2);
        (index, place - real(index))
    }

    /// The extinction at `height`, nought beyond the slab.
    fn at(&self, height: f64) -> f64 {
        if !(self.floor..=self.ceiling).contains(&height) {
            return 0.0;
        }
        let (index, along) = self.place(height);
        self.levels[index] + (self.levels[index + 1] - self.levels[index]) * along
    }

    /// The slab's optical depth beneath `height`, exact for its extinction
    /// changing evenly between levels.
    fn depth(&self, height: f64) -> f64 {
        let (index, along) = self.place(height);
        let (low, high) = (self.levels[index], self.levels[index + 1]);
        self.beneath[index] + self.gap * along * (low + 0.5 * (high - low) * along)
    }

    /// What of the sunlight from the unit `sun` reaches `height` through the
    /// slab: down through what lies above, or up through what lies below for
    /// a sun beneath its level, never along more of it than `breadth` holds.
    fn sunlit(&self, height: f64, sun: Vec3, breadth: f64) -> f64 {
        let (below, total) = (self.depth(height), self.depth(self.ceiling));
        let column = if sun.y >= 0.0 { total - below } else { below };
        let slant = sun.y.abs().max((self.ceiling - self.floor) / breadth);
        mathf::exp(-column / slant)
    }
}

/// For each way a ray can leave ice toward the eye, straight down to
/// straight up, the share of ice's phase that light travelling down sends
/// along it: integrated over the circles about the way out, each by the
/// part of it whose light comes from above.
fn downward_shares() -> [f32; HEMISPHERE] {
    let mut shares = [0.0; HEMISPHERE];
    for (index, slot) in shares.iter_mut().enumerate() {
        let rise = 2.0 * real(index) / real(HEMISPHERE - 1) - 1.0;
        let level = mathf::sqrt((1.0 - rise * rise).max(0.0));
        let mut share = 0.0;
        for step in 0..HEMISPHERE_STEPS {
            // Angles packed toward the way out, where the phase peaks.
            let t = (f64::from(step) + 0.5) / f64::from(HEMISPHERE_STEPS);
            let angle = PI * t * t;
            let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
            let (highest, lowest) = (rise * cos + level * sin, rise * cos - level * sin);
            let arc = if highest < 0.0 {
                TAU
            } else if lowest >= 0.0 {
                0.0
            } else {
                TAU - 2.0 * mathf::acos((-rise * cos / (level * sin)).clamp(-1.0, 1.0))
            };
            let width = 2.0 * PI * t / f64::from(HEMISPHERE_STEPS);
            share += henyey_greenstein(cos, ICE_ASYMMETRY) * arc * sin * width;
        }
        *slot = single(share.clamp(0.0, 1.0));
    }
    shares
}

/// The scattering octaves: how much each dims with depth and how much it
/// adds, a lighter and broader lobe each.
const OCTAVES: [(f64, f64); 3] = [(1.0, 1.0), (0.3, 0.55), (0.1, 0.3)];
/// Each octave's forward lobe asymmetry, and how much of its light a
/// backward lobe takes.
const PHASES: [(f64, f64); 3] = [(0.8, 0.28), (0.45, 0.2), (0.2, 0.1)];

/// A blend of a forward and a backward Henyey–Greenstein lobe.
fn dual_phase(cos: f64, g: f64, back: f64) -> f64 {
    henyey_greenstein(cos, g) * (1.0 - back) + henyey_greenstein(cos, -0.3) * back
}

fn henyey_greenstein(cos: f64, g: f64) -> f64 {
    let base = (1.0 + g * g - 2.0 * g * cos).max(1e-9);
    (1.0 - g * g) / (4.0 * PI * base * mathf::sqrt(base))
}

/// The sunlight reaching `height` of the way from the bank's floor to its
/// ceiling, where the sun's angle from the vertical has `cosine`.
fn sunlight_at(lighting: &Lighting, height: f64, cosine: f64) -> Vec3 {
    let (low, high) = lighting.cosines;
    let across = if high > low {
        ((cosine - low) / (high - low)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let table = &lighting.sunlight;
    let at = |level: usize, column: usize| {
        table
            .get(level * SUN_COSINES + column)
            .copied()
            .unwrap_or(Vec3::ZERO)
    };
    let row = |level: usize| {
        interpolated(
            SUN_COSINES,
            across * real(SUN_COSINES - 1),
            |column| at(level, column),
            Vec3::lerp,
        )
    };
    interpolated(
        SUNLIGHT_LEVELS,
        height.clamp(0.0, 1.0) * real(SUNLIGHT_LEVELS - 1),
        row,
        Vec3::lerp,
    )
}

/// `table` read at `place`, counted in entries from its first: linearly
/// between the two about it, and held at its ends.
fn along(table: &[f32], place: f64) -> f64 {
    interpolated(
        table.len(),
        place,
        |index| table.get(index).map_or(0.0, |&value| f64::from(value)),
        |a, b, t| a + (b - a) * t,
    )
}

/// The table of `count` entries `at` reads, at `place` counted in entries
/// from its first: blended by `lerp` between the two about it, and held at
/// its ends.
fn interpolated<T>(
    count: usize,
    place: f64,
    at: impl Fn(usize) -> T,
    lerp: impl FnOnce(T, T, f64) -> T,
) -> T {
    let last = count.saturating_sub(1);
    let (index, fraction) = cell_of(place);
    let index = index.min(last);
    lerp(at(index), at((index + 1).min(last)), fraction)
}

/// The heights and the cosines of the sun's angle from the vertical at
/// which the sunlight reaching a bank spanning `span` is taken, over
/// `cosines`, in the order its table holds them.
pub(crate) fn sunlight_places(
    (floor, ceiling): (f64, f64),
    (low, high): (f64, f64),
) -> impl Iterator<Item = (f64, f64)> {
    (0..SUNLIGHT_LEVELS).flat_map(move |level| {
        let height = floor + (ceiling - floor) * real(level) / real(SUNLIGHT_LEVELS - 1);
        (0..SUN_COSINES).map(move |column| {
            (
                height,
                low + (high - low) * real(column) / real(SUN_COSINES - 1),
            )
        })
    })
}

/// What a sample of the bank holds at a point.
struct Sample {
    density: f64,
    /// Extinction per metre at full density, of the deck it lies in.
    thickness: f64,
    /// How far up its cloud the point lies, base to top.
    within: f64,
    billow: f64,
    matter: Matter,
}

/// A point among a deck's weather columns: the four about it, and how far
/// east and north across them it lies; each field blended only as it is
/// read.
struct Blend {
    corners: [Column; 4],
    across: (f64, f64),
}

impl Blend {
    /// The deck's `field` at the point.
    fn at(&self, field: fn(&Column) -> f32) -> f64 {
        bilinear(
            self.corners.map(|corner| f64::from(field(&corner))),
            self.across,
        )
    }

    /// The deck's whole column at the point.
    fn column(&self) -> Column {
        Column {
            cover: single(self.at(|corner| corner.cover)),
            base: single(self.at(|corner| corner.base)),
            top: single(self.at(|corner| corner.top)),
        }
    }
}

/// The weather cell a march stands over: the heights each deck's cloud
/// there can stand between, and how far along the ray it leaves the cell.
#[derive(Copy, Clone, Debug)]
struct Over {
    bands: [(f64, f64); 2],
    until: f64,
}

impl Over {
    /// Where a ray running as `course` and at height `height` `t` along it,
    /// in air no cloud of the cell can stand in, might next meet some: where
    /// it reaches a deck's band, rising into it from beneath or falling into
    /// it from above, or else leaves the cell; `None` where cloud can stand
    /// at `t`.
    fn clear(&self, course: &Course, height: f64, t: f64) -> Option<f64> {
        if self
            .bands
            .iter()
            .any(|&(low, high)| (low..=high).contains(&height))
        {
            return None;
        }
        let into = |&(low, high): &(f64, f64)| {
            if low > high {
                f64::INFINITY
            } else if height < low {
                course
                    .crossings(low)
                    .map_or(f64::INFINITY, |(_, rising)| rising)
            } else {
                course
                    .crossings(high)
                    .map_or(f64::INFINITY, |(falling, _)| {
                        if falling > t {
                            falling
                        } else {
                            f64::INFINITY
                        }
                    })
            }
        };
        Some(self.bands.iter().map(into).fold(self.until, f64::min))
    }
}

/// A ray's walk across a level's weather map a cell at a time (Amanatides
/// and Woo, "A Fast Voxel Traversal Algorithm for Ray Tracing", 1987): the
/// cell it stands over, which may lie off the map; and east and north, which
/// way it steps, how far along the ray it next crosses a line of cells, and
/// how far apart along the ray those lines lie.
#[derive(Copy, Clone, Debug)]
struct Walk {
    cell: [isize; 2],
    step: [isize; 2],
    next: [f64; 2],
    apart: [f64; 2],
}

impl Walk {
    /// On to the cell the ray stands over `t` along it.
    fn advance(&mut self, t: f64) {
        for axis in 0..2 {
            while self.next[axis] <= t {
                self.cell[axis] += self.step[axis];
                self.next[axis] += self.apart[axis];
            }
        }
    }

    /// Where along the ray it leaves the cell it stands over.
    fn until(&self) -> f64 {
        self.next[0].min(self.next[1])
    }

    /// The cell it stands over, by its column and row; `None` off the map.
    fn cell(&self) -> Option<(usize, usize)> {
        let index = |at: isize| usize::try_from(at).ok().filter(|&at| at < WEATHER_CELLS);
        Some((index(self.cell[0])?, index(self.cell[1])?))
    }
}

/// Where a march stands among a bank's levels: the level it walks, and its
/// walk across that level's map.
#[derive(Copy, Clone, Debug)]
struct Cursor {
    level: usize,
    walk: Walk,
}

/// A bank read at trace time.
struct Reader<'a> {
    bank: &'a Cloudbank,
}

impl Reader<'_> {
    /// The densest cloud of any deck at `point`; `None` where there is none.
    fn density(&self, point: Vec3, fine: bool) -> Option<Sample> {
        self.density_at(point, self.bank.altitude(point), fine)
    }

    /// The densest cloud of any deck at `point`, `altitude` above the scene's
    /// level; `None` where there is none.
    fn density_at(&self, point: Vec3, altitude: f64, fine: bool) -> Option<Sample> {
        let bank = self.bank;
        let level = bank.level_over(point)?;
        let place = place_on(bank.centre, level.half, (point.x, point.z), WEATHER_CELLS);
        // The billows are read over the deck as it lies along the Earth's
        // curve.
        let lying = Vec3::new(point.x, altitude, point.z);
        let mut best: Option<Sample> = None;
        for ((slot, deck), map) in bank.decks.iter().enumerate().zip(&level.weather) {
            let Some(deck) = deck else {
                continue;
            };
            let Some((corners, across)) = about(map, WEATHER_CELLS + 1, place) else {
                continue;
            };
            let column = Blend { corners, across };
            let cover = column.at(|corner| corner.cover);
            if cover <= 0.0 {
                continue;
            }
            let (base, top) = (
                column.at(|corner| corner.base),
                column.at(|corner| corner.top),
            );
            if altitude <= base || altitude >= top {
                continue;
            }
            let within = (altitude - base) / (top - base);
            let grained = bank.grained(slot, deck, lying);
            let shape = bank
                .shape
                .at(grained * (real(SHAPE_SIDE) / (deck.billow * 8.0)));
            let mut density = remap(shape, threshold(cover, within, deck.heap), 1.0)
                * smoothstep(0.0, 0.04, within);
            if density <= 0.0 {
                continue;
            }
            if fine {
                let wisp = bank
                    .detail
                    .at(grained * (real(DETAIL_SIDE) / (deck.billow * 8.0 * DETAIL_SCALE)));
                // Ragged wisps at a cloud's base, round billows at its top.
                let bite = EROSION * (wisp + (1.0 - 2.0 * wisp) * within.min(1.0));
                density = remap(density, bite, 1.0);
            }
            if density > best.as_ref().map_or(0.0, |held| held.density) {
                best = Some(Sample {
                    density,
                    thickness: deck.thickness,
                    within,
                    billow: deck.billow,
                    matter: deck.matter,
                });
            }
        }
        best
    }

    /// The optical depth toward the sun from `point`: the grid's, and for a
    /// fine sample four short taps through its own billows.
    fn optical_depth(&self, point: Vec3, fine: bool, billow: f64) -> f64 {
        let bank = self.bank;
        if !fine {
            return self.light_at(point);
        }
        let mut depth = 0.0;
        let mut last = 0.0;
        for fraction in TAPS {
            let reach = billow * fraction;
            let at = point + bank.sun * f64::midpoint(last, reach);
            if let Some(sample) = self.density(at, false) {
                depth += sample.density * sample.thickness * (reach - last);
            }
            last = reach;
        }
        depth + self.light_at(point + bank.sun * last)
    }

    /// The light grid's depth of the bank's own cloud toward the sun at
    /// `point`.
    fn light_at(&self, point: Vec3) -> f64 {
        let altitude = self.bank.altitude(point);
        self.bank
            .level_over(point)
            .and_then(|level| self.layered(&level.light, level, point, altitude))
            .unwrap_or(0.0)
    }

    /// What of the sunlight the bank above lets through to `point`,
    /// `altitude` above the scene's level: all of it with none above.
    fn overhead_at(&self, point: Vec3, altitude: f64) -> f64 {
        self.bank
            .level_over(point)
            .and_then(|level| self.layered(&level.overhead, level, point, altitude))
            .unwrap_or(1.0)
    }

    /// `values`, one of `level`'s grids laid out as its light grid's points
    /// are, read trilinearly at `point`, `altitude` above the scene's level;
    /// `None` off it, or where it is empty.
    fn layered(&self, values: &[f32], level: &Level, point: Vec3, altitude: f64) -> Option<f64> {
        let bank = self.bank;
        let place = place_on(bank.centre, level.half, (point.x, point.z), LIGHT_CELLS);
        let high = ((altitude - bank.floor) / (bank.ceiling - bank.floor) * real(LIGHT_LAYERS - 1))
            .clamp(0.0, real(LIGHT_LAYERS - 1) - 1e-6);
        let (layer, rise) = cell_of(high);
        let points = LIGHT_CELLS + 1;
        let beneath = plane(values, points, layer, place)?;
        let above = plane(values, points, (layer + 1).min(LIGHT_LAYERS - 1), place)?;
        Some(beneath + (above - beneath) * rise)
    }

    /// The optical depth of the cloud from `point` toward the sun, out of
    /// the bank, in `steps` steps.
    fn toward_sun(&self, point: Vec3, steps: u32) -> f64 {
        let bank = self.bank;
        let sun = bank.sun;
        let Some((_, leave)) = bank.crossing(point, sun) else {
            return 0.0;
        };
        let step = leave / f64::from(steps.max(1));
        let mut depth = 0.0;
        for index in 0..steps {
            let at = point + sun * ((f64::from(index) + 0.5) * step);
            if let Some(sample) = self.density(at, false) {
                depth += sample.density * sample.thickness * step;
            }
        }
        depth
    }
}

/// A deck's column at `(x, z)`, its weather drawn only as finely as columns
/// `spacing` apart hold: how much it covers, and its base and top.
fn weather(deck: &Deck, (x, z): (f64, f64), spacing: f64) -> Column {
    let (cos, sin) = (mathf::cos(deck.heading), mathf::sin(deck.heading));
    let along = (x * cos + z * sin) / (deck.scale * deck.stretch);
    let across = (-x * sin + z * cos) / deck.scale;
    // Its features across the wind are its narrowest.
    let footprint = spacing / deck.scale;
    // Pushed about by a slower field, so the cover gathers in drifts.
    let (u, v) = (
        along + 0.5 * noise2(along * 0.3, across * 0.3, deck.seed ^ 0x51),
        across + 0.5 * noise2(along * 0.3 + 3.1, across * 0.3 + 1.7, deck.seed ^ 0x93),
    );
    let field = fbm2_resolved(u, v, deck.seed, (5, 0.55, 2.02), footprint);
    let drift = fbm2_resolved(
        u * 0.23,
        v * 0.23,
        deck.seed ^ 0x77,
        (3, 0.5, 2.0),
        footprint * 0.23,
    );
    // How much of the sky a column holds: the deck's cover on the whole, more
    // or less where the drifts gather it.
    let cover = (deck.cover + 0.55 * field + 0.35 * drift).clamp(0.0, 1.0);
    let drift = 0.5 + 0.5 * drift;
    let lift =
        noise2(x / 5_000.0, z / 5_000.0, deck.seed ^ 0xba5e) * resolved(spacing / 5_000.0, 1.0);
    let base = deck.base + deck.base_spread * lift;
    // Deeper where the cover is thickest: the heaps grow tall at the middle
    // of a field of cloud.
    let grown = smoothstep(0.2, 1.0, cover);
    let depth = deck.depth.0 + (deck.depth.1 - deck.depth.0) * grown * (0.6 + 0.4 * drift);
    Column {
        cover: single(cover),
        base: single(base),
        top: single(base + depth),
    }
}

/// How much billow a cloud needs at height fraction `within` of its column
/// to stand there, where the column is `cover` covered: at the base, what the
/// cover leaves; higher up, more, so only the strongest billows rise into
/// domes and turrets, and those of a heaped deck highest.
fn threshold(cover: f64, within: f64, heap: f64) -> f64 {
    let rise = within * within * steepness(heap);
    (1.0 - cover) + cover * rise
}

/// How fast a deck `heap` heaped raises its threshold with the square of
/// the height up a column.
fn steepness(heap: f64) -> f64 {
    1.5 - 0.5 * heap
}

/// The greatest height fraction up a column `cover` covered at which
/// billows reaching `peak` can stand, the `threshold` rising past them above
/// it; `None` where they stand nowhere up it.
fn highest(cover: f64, heap: f64, peak: f64) -> Option<f64> {
    let room = 1.0 - (1.0 - peak) / cover;
    (cover > 0.0 && room > 0.0).then(|| mathf::sqrt(room / steepness(heap)).min(1.0))
}

/// The heights between which `deck`'s cloud over the weather cell whose
/// south-west corner is `(column, row)` of `weather` can stand, its billows
/// reaching no higher than `peak`: bilinear between the corners, its cover,
/// base and top lie within theirs. Rounded out to whole metres, which single
/// precision holds exactly; empty where no cloud can stand.
fn band_over(
    deck: &Deck,
    weather: &[Column],
    (column, row): (usize, usize),
    peak: f64,
) -> (f32, f32) {
    let points = WEATHER_CELLS + 1;
    let at = |column: usize, row: usize| {
        weather
            .get(row * points + column)
            .copied()
            .unwrap_or_default()
    };
    let corners = [
        at(column, row),
        at(column + 1, row),
        at(column, row + 1),
        at(column + 1, row + 1),
    ];
    let values = |field: fn(&Column) -> f32| corners.map(|corner| f64::from(field(&corner)));
    let greatest = |values: [f64; 4]| values.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let Some(within) = highest(greatest(values(|corner| corner.cover)), deck.heap, peak) else {
        return EMPTY_BAND;
    };
    let bases = values(|corner| corner.base);
    let lowest = bases.into_iter().fold(f64::INFINITY, f64::min);
    let high = greatest(bases) + (greatest(values(|corner| corner.top)) - lowest) * within;
    (single(mathf::floor(lowest)), single(mathf::ceil(high)))
}

/// `value` rescaled from `low..high` to `0..1`, clamped below.
fn remap(value: f64, low: f64, high: f64) -> f64 {
    ((value - low) / (high - low).max(1e-9)).clamp(0.0, 1.0)
}

/// A cloud's billows at texel `p` of a tiling texture: inverted cellular
/// noise at three scales over a softer gradient noise.
fn billows(p: Vec3, seed: u32) -> f64 {
    let period = real(SHAPE_SIDE);
    let worley =
        |cells: f64, salt: u32| 1.0 - worley_tiled(p * (cells / period), cells, seed ^ salt);
    let cellular = 0.625 * worley(4.0, 1) + 0.25 * worley(8.0, 2) + 0.125 * worley(16.0, 3);
    let soft = 0.5 + 0.5 * perlin_tiled(p * (4.0 / period), 4.0, seed ^ 4);
    remap(soft, -(1.0 - cellular), 1.0).clamp(0.0, 1.0)
}

/// The fine wisps that erode a cloud's edge, at texel `p` of a tiling
/// texture.
fn wisps(p: Vec3, seed: u32) -> f64 {
    let period = real(DETAIL_SIDE);
    let worley =
        |cells: f64, salt: u32| 1.0 - worley_tiled(p * (cells / period), cells, seed ^ salt);
    0.625 * worley(4.0, 5) + 0.25 * worley(8.0, 6) + 0.125 * worley(16.0, 7)
}

/// Worley's cellular noise that repeats every `period` cells: the distance
/// to the nearest feature point, about `0.0..1.0`.
fn worley_tiled(p: Vec3, period: f64, seed: u32) -> f64 {
    let whole = |value: f64| mathf::floor(value);
    let (cx, cy, cz) = (whole(p.x), whole(p.y), whole(p.z));
    let wrap = |value: f64| {
        let tiled = value - mathf::floor(value / period) * period;
        mathf::round_i32(tiled).cast_unsigned()
    };
    let mut nearest = f64::INFINITY;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y, z) = (cx + f64::from(dx), cy + f64::from(dy), cz + f64::from(dz));
                let key = hash3(wrap(x), wrap(y), wrap(z), seed);
                let feature = Vec3::new(
                    x + unit(key),
                    y + unit(mix32(key ^ 1)),
                    z + unit(mix32(key ^ 2)),
                );
                let offset = feature - p;
                nearest = nearest.min(offset.dot(offset));
            }
        }
    }
    mathf::sqrt(nearest).min(1.0)
}

/// Gradient noise that repeats every `period` lattice cells: about
/// `-1.0..1.0`.
fn perlin_tiled(p: Vec3, period: f64, seed: u32) -> f64 {
    let (cx, cy, cz) = (mathf::floor(p.x), mathf::floor(p.y), mathf::floor(p.z));
    let (fx, fy, fz) = (p.x - cx, p.y - cy, p.z - cz);
    let wrap = |value: f64| {
        let tiled = value - mathf::floor(value / period) * period;
        mathf::round_i32(tiled).cast_unsigned()
    };
    let corner = |dx: f64, dy: f64, dz: f64| {
        let key = hash3(wrap(cx + dx), wrap(cy + dy), wrap(cz + dz), seed);
        let gradient = Vec3::new(
            unit(key) * 2.0 - 1.0,
            unit(mix32(key ^ 1)) * 2.0 - 1.0,
            unit(mix32(key ^ 2)) * 2.0 - 1.0,
        );
        gradient.dot(Vec3::new(fx - dx, fy - dy, fz - dz))
    };
    let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (u, v, w) = (fade(fx), fade(fy), fade(fz));
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x00 = lerp(corner(0.0, 0.0, 0.0), corner(1.0, 0.0, 0.0), u);
    let x10 = lerp(corner(0.0, 1.0, 0.0), corner(1.0, 1.0, 0.0), u);
    let x01 = lerp(corner(0.0, 0.0, 1.0), corner(1.0, 0.0, 1.0), u);
    let x11 = lerp(corner(0.0, 1.0, 1.0), corner(1.0, 1.0, 1.0), u);
    1.2 * lerp(lerp(x00, x10, v), lerp(x01, x11, v), w)
}

/// Fill layers `layers` of `tile` with `value` at each texel's centre.
fn fill_tile(
    tile: &mut Tile,
    layers: Range<usize>,
    runner: &dyn JobRunner,
    value: &(dyn Fn(Vec3) -> f64 + Sync),
) {
    let side = tile.side;
    let per_layer = side * side;
    let Some(texels) = tile
        .texels
        .get_mut(layers.start * per_layer..layers.end * per_layer)
    else {
        return;
    };
    band::for_each(runner, texels, (layers.start, per_layer), &|layer, band| {
        for (index, texel) in band.iter_mut().enumerate() {
            let p = Vec3::new(
                real(index % side) + 0.5,
                real(index / side) + 0.5,
                real(layer) + 0.5,
            );
            let level = mathf::round_i32(value(p).clamp(0.0, 1.0) * 255.0);
            *texel = u8::try_from(level).unwrap_or(u8::MAX);
        }
    });
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
