//! Volumetric cloud: heaped cumulus, a broken sheet of stratocumulus, a field
//! of altocumulus, or high streaks of cirrus, marched through as the medium
//! it is and lit by the light followed through it.
//!
//! A cloudbank is up to two decks of cloud over the round Earth, as far as
//! any of it can be seen. A heaped deck stands as heaps of its own, each a
//! thermal's (`heaps`); any other has a weather map — how far within a
//! footprint each column stands, and where its base and top lie there — over
//! which its sheets lie. Either way its clouds stand as `form` lays them, on
//! flat bases, pushed out into turrets on turrets and frayed past them, their
//! water's extinction rising with height.
//! Heights are taken over the Earth's curve about its centre below the eye,
//! as the air's are, so a deck seen low down runs on to the horizon. Its maps
//! are laid in levels about the eye, each twice the last's breadth in as many
//! columns (Losasso and Hoppe, "Geometry Clipmaps", 2004), each level's
//! weather drawn only as finely as its columns hold and its rim blended into
//! the next's, so no seam shows where they meet; a level lists the heaps over
//! it cell by cell, the same heaps on every level.
//!
//! A ray gathers what the cloud scatters toward it step by step, each step's
//! light integrated exactly over the step (Hillaire, "Physically Based Sky,
//! Atmosphere and Cloud Rendering in Frostbite", 2016), striding across the
//! air above and below the heights each weather cell bounds its cloud to, and
//! for a ray seen directly through a cloud's edges a pixel's footprint or two
//! at a time. A step's light is the sun's scattered once exactly by the
//! cloud's phase, through the optical depth toward the sun that short taps
//! through its own edges and the bank's light (`light`) beyond them hold — so
//! a cloud shades its own turrets and every cloud behind it; the sunlight its
//! forward peak carries on, scattered toward the ray through the phase
//! composed with that peak; and the diffuse light the
//! photons followed through the bank laid down there, its fluence and its
//! net flux through the full phase. Each step's sunlight has crossed the air
//! to its own place at the angle the sun stands there, so a deck still above
//! the Earth's shadow at dusk is lit red from beneath, and cloud far off
//! toward a set sun is lit after the cloud overhead has gone grey.

mod form;
mod heaps;
mod light;

use alloc::vec::Vec;
use core::f64::consts::{PI, SQRT_2};
use core::ops::{Range, RangeInclusive};

use form::{band_over, weather, Blend, Column, Edges, Envelope, Textures, EMPTY_BAND};
use heaps::{cellular, Heaps};
use light::Lights;
pub(crate) use light::Resolution;
use tairix_noise::smoothstep;
use tairix_parallel::JobRunner;
use tairix_util::mathf;

use crate::atmosphere::GROUND;
use crate::lanes::Corners;
use crate::shade::Rect;
use crate::shape::{quadratic, reciprocal};
use crate::vector::{cell_of, real, single, Ray, Vec3};
use crate::walk::Walk;

/// What a field deck's weather holds over a square: the heights between
/// which its cloud's bands let it stand, and of the weather any read over it
/// is blended to, the furthest within a footprint, the deepest, and the
/// lowest and highest base.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Holding {
    band: (f64, f64),
    edge: f64,
    depth: f64,
    base: (f64, f64),
}

impl Holding {
    /// A square no column or band of the deck's lies over.
    const NONE: Self = Self {
        band: (f64::INFINITY, f64::NEG_INFINITY),
        edge: f64::NEG_INFINITY,
        depth: 0.0,
        base: (f64::INFINITY, f64::NEG_INFINITY),
    };

    /// The weather at `place` held among the square's.
    fn hold(&mut self, place: &Blend) {
        let base = place.at(|column| column.base);
        self.edge = self.edge.max(place.at(|column| column.edge));
        self.depth = self.depth.max(place.at(|column| column.top) - base);
        self.base = (self.base.0.min(base), self.base.1.max(base));
    }
}

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
    /// Extinction per metre through its cloud: for water, a kilometre above
    /// its base, its liquid water rising evenly with height and its
    /// extinction with that water's two-thirds power; for ice, throughout.
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

/// How a cloud's matter scatters: the share of what it meets it scatters,
/// and the mean cosine it scatters through.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Scattering {
    pub(crate) albedo: f64,
    pub(crate) asymmetry: f64,
}

impl Scattering {
    /// The share of the phase δ-Eddington takes as running on unscattered,
    /// its forward peak (Joseph, Wiscombe and Weinman, 1976).
    pub(crate) const fn forward(self) -> f64 {
        self.asymmetry * self.asymmetry
    }

    /// The mean cosine of the rest of the phase, past its forward peak.
    pub(crate) const fn remainder(self) -> f64 {
        self.asymmetry / (1.0 + self.asymmetry)
    }
}

/// Cells along each side of a level's weather map: a multiple of four, so a
/// level's square lies on whole cells of the next and its columns on every
/// other one of the next's.
const WEATHER_CELLS: usize = 384;

/// The columns and rows of a level's weather cells a square covers.
type Cells = (RangeInclusive<usize>, RangeInclusive<usize>);
const _: () = assert!(WEATHER_CELLS.is_multiple_of(4));
/// The share of a level's half-breadth over which its rim is blended into
/// the next level's, its weather's and its light's alike.
const RIM: f64 = 0.125;
/// The most levels a bank's weather is laid in.
const MOST_LEVELS: usize = 8;
/// Heights the sunlight reaching the bank is tabulated at, and cosines of
/// the sun's angle from the vertical over those the bank's places see.
pub(crate) const SUNLIGHT_LEVELS: usize = 48;
pub(crate) const SUN_COSINES: usize = 33;
/// A full-HD picture's pixel through a 60° view, in radians: what a test
/// stands a bank for, so its light is held as a screen would show it.
#[cfg(test)]
pub(crate) const SCREEN_PIXEL: f64 = 1.07e-3;

/// The looks toward the sun a fine sample of water cloud takes through its
/// own billows, as fractions of a billow's breadth, the first
/// [`RESOLVED_TAPS`] through its edges as finely as it is seen; past the
/// last, each tap a stretch longer than the last by [`TAP_GROWTH`], on until
/// clear air a voxel of the bank's light deep opens past its own cloud, so the
/// light answers only for cloud beyond it, or until so much cloud is crossed
/// that no sun gets through, or so far has been looked.
const TAPS: [f64; 4] = [0.004, 0.012, 0.03, 0.1];
const RESOLVED_TAPS: usize = 3;
const TAP_GROWTH: f64 = 1.5;
const UNLIT: f64 = 8.0;
const FARTHEST_TAP: f64 = 4.0;

/// The most steps a ray takes through the bank, seen directly and seen
/// otherwise; and the transmittance at which it stops, what lies beyond
/// hidden and what lies deeper sending on its last sample's light.
const FINE_STEPS: u32 = 1024;
const COARSE_STEPS: u32 = 32;
/// Within a cloud's billows a ray seen directly steps so many of its
/// footprints at a time, never less than so many metres: its edges come out
/// as sharp as the pixel holds them.
const SHARP: f64 = 2.0;
const SHARPEST: f64 = 0.5;
/// The most jumps a ray takes through air no cloud can stand in, on each
/// level: into the heights cloud can reach over a weather cell and on out of
/// the cell, for as many cells as a straight line can cross a level's map.
const JUMPS_A_LEVEL: usize = 4 * WEATHER_CELLS;
const OPAQUE: f64 = 0.015;
/// A step through a deck, as a share of its billows, seen directly and seen
/// otherwise: fine enough that no edge shows where a step fell. A coarse
/// march's steps are as long as the footprint its edges are read at. Clear
/// air between clouds, within the heights they can reach, is stepped through
/// as finely, since a stride through it would step over a cloud's thinner
/// edges.
const FINE_SHARE: f64 = 0.06;
const COARSE_SHARE: f64 = form::AVERAGED;
/// How far off a step has doubled in length, as a pixel's view of the cloud
/// has widened.
const STRETCH: f64 = 20_000.0;
/// Within a cloud's body, past its edges, a ray seen directly steps on as
/// far as a share of how deep within the surface it stands, which turrets
/// can bend no closer, and no further than so much optical depth.
const WITHIN_SHARE: f64 = 0.3;
const STEP_DEPTH: f64 = 0.25;

/// The asymmetry of rough ice crystals' scattering of visible light (Yang
/// et al., "Spectrally consistent scattering, absorption, and polarization
/// properties of atmospheric ice crystals", 2013), whose phase has no halo.
const ICE_ASYMMETRY: f64 = 0.75;

/// Cloud droplets' phase: its forward lobe's asymmetry and the share a
/// backward lobe takes, and that backward lobe's asymmetry.
const DROPLET_LOBES: (f64, f64) = (0.9, 0.05);
const BACK_LOBE: f64 = -0.3;
/// How cloud droplets scatter: almost all the light they meet, through the
/// mean cosine of their phase's lobes.
const DROPLETS: Scattering = Scattering {
    albedo: 0.99998,
    asymmetry: (1.0 - DROPLET_LOBES.1) * DROPLET_LOBES.0 + DROPLET_LOBES.1 * BACK_LOBE,
};
/// Ice crystals, scattering almost all they meet as rough crystals do.
const ICE: Scattering = Scattering {
    albedo: 0.99998,
    asymmetry: ICE_ASYMMETRY,
};

/// One level of a bank's weather: a square about the eye, and over it each
/// deck's weather and the heights its cloud can stand between there.
#[derive(Clone, Debug)]
struct Level {
    /// Half its breadth, either way of the bank's middle.
    half: f64,
    /// Each deck's map, or its heaps where it stands as heaps, and its cells'
    /// bands; empty for a deck the bank lacks.
    weather: [Vec<Column>; 2],
    heaps: [Heaps; 2],
    bands: [Vec<(f32, f32)>; 2],
}

impl Level {
    /// A level `half` either way, its maps reserved whole and written as
    /// their rows are filled, so no unit of building fills more than its own
    /// rows; `None` when the heap will not hold them.
    fn new(half: f64, decks: &[Option<Deck>; 2]) -> Option<Self> {
        let maps = |deck: &Option<Deck>| room(fielded(deck.as_ref()), points(WEATHER_CELLS));
        let banded = |deck: &Option<Deck>| room(deck.is_some(), WEATHER_CELLS * WEATHER_CELLS);
        Some(Self {
            half,
            weather: [maps(&decks[0])?, maps(&decks[1])?],
            heaps: [Heaps::default(), Heaps::default()],
            bands: [banded(&decks[0])?, banded(&decks[1])?],
        })
    }
}

/// Whether `deck` is a deck whose weather is a field of cloud mapped by its
/// levels' columns.
fn fielded(deck: Option<&Deck>) -> bool {
    deck.is_some_and(|deck| !cellular(deck))
}

/// What a level's edges are found from while they are, held for one level
/// at a time. Each place a deck's cover crosses a footprint's edge between
/// two vertices lies on a row or on a column of them, so the squared
/// distance to the nearest is the lesser of the nearest's on a row, found
/// along the rows and then down the columns, and the nearest's on a column,
/// found down the columns and then along the rows (Felzenszwalb and
/// Huttenlocher's separable transform).
#[derive(Clone, Debug, Default)]
struct Edging {
    /// Each deck's cover at each vertex, laid row by row.
    covers: [Vec<f32>; 2],
    /// From each vertex, the squared distance along its row to the nearest
    /// crossing on it, laid row by row.
    along_rows: [Vec<f32>; 2],
    /// From each vertex, the squared distance along its column to the
    /// nearest crossing on it, and to the nearest crossing on any row; both
    /// laid column by column.
    along_columns: [Vec<f32>; 2],
    from_rows: [Vec<f32>; 2],
}

impl Edging {
    /// Room for `decks`' levels' edges to be found; `None` when the heap will
    /// not hold it.
    fn new(decks: &[Option<Deck>; 2]) -> Option<Self> {
        let rooms = || -> Option<[Vec<f32>; 2]> {
            Some([
                room(fielded(decks[0].as_ref()), points(WEATHER_CELLS))?,
                room(fielded(decks[1].as_ref()), points(WEATHER_CELLS))?,
            ])
        };
        Some(Self {
            covers: rooms()?,
            along_rows: rooms()?,
            along_columns: rooms()?,
            from_rows: rooms()?,
        })
    }
}

/// Lines of a level's vertices a core finds edges along at a time, sharing
/// its room for them.
const EDGE_LINES: usize = 16;

/// The vertices of a square grid of `cells` a side.
const fn points(cells: usize) -> usize {
    (cells + 1) * (cells + 1)
}

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

/// The next coarser level's columns about vertex `(i, j)` of a level's grid,
/// from its map `coarse`, where the vertex is `rimmed` into it at all.
fn rimmed_into(coarse: Option<&Vec<Column>>, (i, j): (usize, usize), rimmed: f64) -> Option<Blend> {
    let coarse = coarse.filter(|_| rimmed > 0.0)?;
    let (corners, across) = about(
        coarse,
        WEATHER_CELLS + 1,
        coarse_place(WEATHER_CELLS, (i, j)),
    )?;
    Some(Blend { corners, across })
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
    /// A level's heaps, from the coarsest in, drawn by bands and then listed
    /// by rows of its cells; its weather, followed by the distances to its
    /// footprints' edges, down its columns and then along its rows; and then
    /// its bands: by level, then row or column.
    Heaps(usize, usize),
    Lists(usize, usize),
    Weather(usize, usize),
    Squares(usize, usize),
    Edges(usize, usize),
    Bands(usize, usize),
    /// The light within the bank, once it is lit.
    Light,
    Done,
}

impl Stage {
    /// The rows the stage fills, and how many of them a core fills in a
    /// unit: each unit some milliseconds' work.
    const fn extent(self) -> (usize, usize) {
        match self {
            Self::Shape(_) => (form::SHAPE_SIDE, 1),
            Self::Detail(_) => (form::DETAIL_SIDE, 4),
            Self::Heaps(..) | Self::Lists(..) => (WEATHER_CELLS, 8),
            Self::Weather(..) => (WEATHER_CELLS + 1, 8),
            Self::Squares(..) | Self::Edges(..) => (WEATHER_CELLS + 1, EDGE_LINES),
            Self::Bands(..) => (WEATHER_CELLS, 4),
            Self::Light | Self::Done => (1, 1),
        }
    }

    /// The next row the stage fills.
    const fn row(self) -> usize {
        match self {
            Self::Shape(row)
            | Self::Detail(row)
            | Self::Heaps(_, row)
            | Self::Lists(_, row)
            | Self::Weather(_, row)
            | Self::Squares(_, row)
            | Self::Edges(_, row)
            | Self::Bands(_, row) => row,
            Self::Light | Self::Done => 0,
        }
    }

    /// The stage on at row `row`.
    const fn at(self, row: usize) -> Self {
        match self {
            Self::Shape(_) => Self::Shape(row),
            Self::Detail(_) => Self::Detail(row),
            Self::Heaps(level, _) => Self::Heaps(level, row),
            Self::Lists(level, _) => Self::Lists(level, row),
            Self::Weather(level, _) => Self::Weather(level, row),
            Self::Squares(level, _) => Self::Squares(level, row),
            Self::Edges(level, _) => Self::Edges(level, row),
            Self::Bands(level, _) => Self::Bands(level, row),
            Self::Light | Self::Done => self,
        }
    }

    /// The stage after this one in a bank of `levels` levels, its decks
    /// standing as heaps or lying as fields as `kinds` has them.
    fn after(self, levels: usize, kinds: Kinds) -> Self {
        let mapped = |level: usize| {
            if kinds.fields {
                Self::Weather(level, 0)
            } else {
                Self::Bands(level, 0)
            }
        };
        let begun = |level: usize| {
            if kinds.heaps {
                Self::Heaps(level, 0)
            } else {
                mapped(level)
            }
        };
        match self {
            Self::Shape(_) => Self::Detail(0),
            Self::Detail(_) => levels.checked_sub(1).map_or(Self::Light, begun),
            Self::Heaps(level, _) => Self::Lists(level, 0),
            Self::Lists(level, _) => mapped(level),
            Self::Weather(level, _) => Self::Squares(level, 0),
            Self::Squares(level, _) => Self::Edges(level, 0),
            Self::Edges(level, _) => Self::Bands(level, 0),
            Self::Bands(level, _) => level.checked_sub(1).map_or(Self::Light, begun),
            Self::Light | Self::Done => Self::Done,
        }
    }
}

/// Which kinds of deck a bank holds: any standing as heaps, and any lying as
/// a field.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Kinds {
    heaps: bool,
    fields: bool,
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
    /// The sky's light, as even over the sky above a cloud.
    pub(crate) above: Vec3,
    /// The sun's irradiance on the open level ground, and the ground's
    /// albedo, which gives it back to the clouds' undersides.
    pub(crate) ground: (Vec3, f64),
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
    textures: Textures,
    edging: Edging,
    lights: Lights,
    lighting: Option<Lighting>,
    stage: Stage,
}

impl Cloudbank {
    /// A bank of `decks` lit from the unit `sun` over an Earth whose centre
    /// lies `ground` metres below the scene's level, its finest level
    /// spanning no more than `most` either way of the eye and its light
    /// solved as finely as `resolution` has it; to be stood about the eye
    /// and lit before it is built. `None` when the heap will not hold its
    /// textures.
    pub(crate) fn new(
        decks: [Option<Deck>; 2],
        (most, resolution): (f64, Resolution),
        sun: Vec3,
        ground: f64,
    ) -> Option<Self> {
        let (mut floor, mut ceiling) = (f64::INFINITY, f64::NEG_INFINITY);
        for deck in decks.iter().flatten() {
            floor = floor.min(deck.base - deck.base_spread);
            // Its turrets reach as far as a billow and a half above its tops.
            ceiling = ceiling.max(deck.base + deck.base_spread + deck.depth.1 + 2.0 * deck.billow);
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
            textures: Textures::new()?,
            edging: Edging::default(),
            lights: Lights::new(resolution),
            lighting: None,
            stage: Stage::Shape(0),
        })
    }

    /// Lay the bank's levels about `eye`, out to as far as any of its cloud
    /// can be seen from there: where the eye's sight grazing the sea runs on
    /// to the ceiling, its light held finely only where a picture whose
    /// pixels are `pixel` radians across can show it. Its building starts
    /// afresh. `None` when the heap will not hold the levels.
    pub(crate) fn stand(&mut self, eye: Vec3, pixel: f64) -> Option<()> {
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
        self.edging = Edging::new(&self.decks)?;
        let mut lights = core::mem::take(&mut self.lights);
        let laid = lights.lay(self, pixel);
        self.lights = lights;
        laid?;
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

    /// The box of deck `slot`'s billows' coordinates the box over the square
    /// `square` between heights `low` and `high` lies within, as the deck's
    /// grain draws it.
    fn grained_box(
        &self,
        slot: usize,
        deck: &Deck,
        (square, (low, high)): (Rect, (f64, f64)),
    ) -> (Vec3, Vec3) {
        let (mut least, mut most) = (Vec3::splat(f64::INFINITY), Vec3::splat(f64::NEG_INFINITY));
        for (x, z) in [
            square.0,
            (square.1 .0, square.0 .1),
            (square.0 .0, square.1 .1),
            square.1,
        ] {
            for altitude in [low, high] {
                let at = self.grained(slot, deck, Vec3::new(x, altitude, z));
                least = least.min(at);
                most = most.max(at);
            }
        }
        (least, most)
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

    /// Light the bank as `lighting` has it, before its light is built.
    pub(crate) fn light_by(&mut self, lighting: Lighting) {
        self.lighting = Some(lighting);
    }

    /// Whether the bank is lit.
    pub(crate) const fn lit(&self) -> bool {
        self.lighting.is_some()
    }

    /// Build the next unit of the bank across `runner`, its sunlight dimmed
    /// by the bank `above` it; whether it is built, or `None` when the heap
    /// will not hold it.
    pub(crate) fn step(&mut self, runner: &dyn JobRunner, above: Option<&Self>) -> Option<bool> {
        let stage = self.stage;
        if stage == Stage::Light {
            let mut lights = core::mem::take(&mut self.lights);
            let solved = lights.step(self, runner, above);
            self.lights = lights;
            if solved? {
                self.stage = Stage::Done;
            }
            return Some(self.stage == Stage::Done);
        }
        let (rows, per) = stage.extent();
        let start = stage.row();
        let end = (start + per * runner.width().max(1)).min(rows);
        let seed = self.seed();
        match stage {
            Stage::Shape(_) => self.textures.fill_shape(start..end, runner, seed),
            Stage::Detail(_) => self.textures.fill_detail(start..end, runner, seed ^ 0xd37a),
            Stage::Heaps(level, _) => self.fill_heaps(level, start..end, runner)?,
            Stage::Lists(level, _) => self.list_heaps(level, start..end, runner)?,
            Stage::Weather(level, _) => self.fill_weather(level, start..end, runner),
            Stage::Squares(level, _) => self.fill_squares(level, start..end, runner)?,
            Stage::Edges(level, _) => self.fill_edges(level, start..end, runner)?,
            Stage::Bands(level, _) => self.fill_bands(level, start..end, runner),
            Stage::Light | Stage::Done => {}
        }
        self.stage = if end >= rows {
            stage.after(self.levels.len(), self.kinds())
        } else {
            stage.at(end)
        };
        if self.stage == Stage::Light {
            self.edging = Edging::default();
        }
        Some(self.stage == Stage::Done)
    }

    /// The units of building done and to do, each a core's work at its
    /// stage, so a runner of any width does them in the same proportion.
    pub(crate) fn units(&self) -> (f64, f64) {
        let mut stage = Stage::Shape(0);
        let (mut done, mut total) = (0.0, 0.0);
        while stage != Stage::Light {
            let (rows, per) = stage.extent();
            let units = real(rows) / real(per);
            if stage == self.stage.at(0) {
                done = total + real(self.stage.row()) / real(per);
            }
            total += units;
            stage = stage.after(self.levels.len(), self.kinds());
        }
        let (lit, lighting) = self.lights.units();
        match self.stage {
            Stage::Light => (total + lit, total + lighting),
            Stage::Done => (total + lighting, total + lighting),
            _ => (done, total + lighting),
        }
    }

    /// Which kinds of deck the bank holds.
    fn kinds(&self) -> Kinds {
        let mut kinds = Kinds {
            heaps: false,
            fields: false,
        };
        for deck in self.decks.iter().flatten() {
            if cellular(deck) {
                kinds.heaps = true;
            } else {
                kinds.fields = true;
            }
        }
        kinds
    }

    /// The seed the bank's textures are drawn from: its first deck's.
    fn seed(&self) -> u32 {
        self.decks
            .iter()
            .flatten()
            .next()
            .map_or(1, |deck| deck.seed)
    }

    /// How its cloud scatters: as its first deck's matter does.
    fn medium(&self) -> Matter {
        self.decks
            .iter()
            .flatten()
            .next()
            .map_or(Matter::Water, |deck| deck.matter)
    }

    /// The breadth of its finest deck's billows.
    fn finest_billow(&self) -> f64 {
        self.decks
            .iter()
            .flatten()
            .map(|deck| deck.billow)
            .fold(f64::INFINITY, f64::min)
    }

    /// Its matter's scattering.
    fn scattering(&self) -> Scattering {
        match self.medium() {
            Matter::Water => DROPLETS,
            Matter::Ice => ICE,
        }
    }

    /// Each level holding any of the square `low..high`, from the finest, and
    /// the columns and rows of its weather cells the square covers, passed to
    /// `visit`, until a level holds the whole of it.
    fn each_level_over(&self, (low, high): Rect, mut visit: impl FnMut(&Level, Cells)) {
        let (cx, cz) = self.centre;
        // As the levels' squares measure it, its nearest from the middle:
        // each level holding it is read over some of the square.
        let nearest = (cx.clamp(low.0, high.0) - cx)
            .abs()
            .max((cz.clamp(low.1, high.1) - cz).abs());
        let farthest = self.farthest((low, high));
        for level in &self.levels {
            if nearest >= level.half {
                continue;
            }
            visit(level, self.cells_under(level, (low, high)));
            // A level holding all of it leaves the coarser ones none to read.
            if farthest < level.half {
                break;
            }
        }
    }

    /// The finest level holding the whole of the square `low..high` in
    /// weather cells at least as broad as it, the coarsest if none does, and
    /// the columns and rows of its cells the square covers.
    fn level_holding(&self, (low, high): Rect) -> Option<(&Level, Cells)> {
        let (breadth, farthest) = (
            (high.0 - low.0).max(high.1 - low.1),
            self.farthest((low, high)),
        );
        let level = self
            .levels
            .iter()
            .find(|level| {
                farthest < level.half && 2.0 * level.half / real(WEATHER_CELLS) >= breadth
            })
            .or_else(|| self.levels.last())?;
        Some((level, self.cells_under(level, (low, high))))
    }

    /// How far the square `low..high` reaches from the bank's middle, as
    /// the levels' squares measure it.
    fn farthest(&self, (low, high): Rect) -> f64 {
        let (cx, cz) = self.centre;
        [low.0 - cx, high.0 - cx, low.1 - cz, high.1 - cz]
            .into_iter()
            .fold(0.0, |most: f64, offset| most.max(offset.abs()))
    }

    /// The columns and rows of `level`'s weather cells the square `low..high`
    /// covers, as far as the level's square reaches.
    fn cells_under(&self, level: &Level, (low, high): Rect) -> Cells {
        let size = 2.0 * level.half / real(WEATHER_CELLS);
        let cell = |value: f64, from: f64| cell_of((value - from) / size).0.min(WEATHER_CELLS - 1);
        let (west, south) = (self.centre.0 - level.half, self.centre.1 - level.half);
        (
            cell(low.0, west)..=cell(high.0, west),
            cell(low.1, south)..=cell(high.1, south),
        )
    }

    /// What each field deck's weather holds over the square `low..high`; a
    /// deck standing as heaps holds nothing here, its heaps bounding the
    /// square's cloud one by one.
    fn weather_over(&self, low: (f64, f64), high: (f64, f64)) -> [Holding; 2] {
        let mut overs = [Holding::NONE; 2];
        let (cx, cz) = self.centre;
        let points = WEATHER_CELLS + 1;
        self.each_level_over((low, high), |level, (columns, rows)| {
            let size = 2.0 * level.half / real(WEATHER_CELLS);
            let (west, south) = (cx - level.half, cz - level.half);
            for (((bands, map), over), deck) in level
                .bands
                .iter()
                .zip(&level.weather)
                .zip(&mut overs)
                .zip(&self.decks)
            {
                if !fielded(deck.as_ref()) {
                    continue;
                }
                for row in rows.clone() {
                    let along = row * WEATHER_CELLS;
                    let cells = bands.get(along + columns.start()..=along + columns.end());
                    for &(lowest, highest) in cells.into_iter().flatten() {
                        over.band = (
                            over.band.0.min(f64::from(lowest)),
                            over.band.1.max(f64::from(highest)),
                        );
                    }
                }
                // A cell's reads blend its four corners, so over the part of
                // it the square covers they stand within those at that part's
                // own corners.
                let within = |value: f64, from: f64, at: usize| {
                    ((value - from) / size - real(at)).clamp(0.0, 1.0)
                };
                for row in rows.clone() {
                    let across = (within(low.1, south, row), within(high.1, south, row));
                    for column in columns.clone() {
                        let Some((corners, _)) = about(map, points, (real(column), real(row)))
                        else {
                            continue;
                        };
                        let along = (within(low.0, west, column), within(high.0, west, column));
                        for u in [along.0, along.1] {
                            for v in [across.0, across.1] {
                                over.hold(&Blend {
                                    corners,
                                    across: (u, v),
                                });
                            }
                        }
                    }
                }
            }
        });
        overs
    }

    /// The most extinction any of its cloud holds between heights `low` and
    /// `high` over the square `square`, read through a `footprint` metres
    /// across, each field deck's weather as `overs` has it: none where its
    /// bands or its envelope leave no room for it, and elsewhere, however its
    /// turrets lie, no denser than its whole density risen from the lowest
    /// base to the top of the span; and each heap's the same, heap by heap.
    fn densest(
        &self,
        overs: &[Holding; 2],
        square: Rect,
        (low, high): (f64, f64),
        footprint: f64,
    ) -> f64 {
        self.decks
            .iter()
            .zip(overs)
            .enumerate()
            .map(|(slot, (deck, over))| {
                let (lowest, highest) = over.band;
                let Some(deck) = deck else {
                    return 0.0;
                };
                let reach = self.textures.reach_over(
                    deck,
                    self.grained_box(slot, deck, (square, (low, high))),
                    footprint,
                );
                if cellular(deck) {
                    // Each level's cells list every heap whose cloud can
                    // reach them, so one holding the square bounds it alone.
                    return self.level_holding(square).map_or(0.0, |(level, cells)| {
                        level.heaps.get(slot).map_or(0.0, |heaps| {
                            heaps.densest(cells, square, (low, high), (deck, reach, footprint))
                        })
                    });
                }
                let holds = high > lowest
                    && low < highest
                    && high > over.base.0
                    && form::may_hold(
                        deck,
                        (over.edge, form::shape(deck, over.depth), low - over.base.1),
                        (reach, footprint),
                    );
                if holds {
                    form::extinction(deck, high.min(highest) - lowest)
                } else {
                    0.0
                }
            })
            .sum()
    }

    /// Level `index`'s heaps drawn over bands `bands` across `runner`, for
    /// each deck standing as heaps, begun at its first; `None` when the heap
    /// will not hold them.
    fn fill_heaps(
        &mut self,
        index: usize,
        bands: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let (decks, centre) = (self.decks, self.centre);
        let level = self.levels.get_mut(index)?;
        for (deck, heaps) in decks.iter().zip(level.heaps.iter_mut()) {
            let Some(deck) = deck.as_ref().filter(|deck| cellular(deck)) else {
                continue;
            };
            if bands.start == 0 {
                let margin = form::standing_margin(deck, self.textures.reach(deck));
                *heaps = Heaps::begin(deck, (centre, level.half), margin)?;
            }
            heaps.draw(deck, bands.clone(), runner)?;
        }
        Some(())
    }

    /// Level `index`'s heaps listed over rows `rows` of its cells across
    /// `runner`, for each deck standing as heaps; `None` when the heap will
    /// not hold the lists.
    fn list_heaps(
        &mut self,
        index: usize,
        rows: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let decks = self.decks;
        let level = self.levels.get_mut(index)?;
        for (deck, heaps) in decks.iter().zip(level.heaps.iter_mut()) {
            if deck.as_ref().is_some_and(cellular) {
                heaps.list(rows.clone(), runner)?;
            }
        }
        Some(())
    }

    /// Level `index`'s weather rows `rows`, its rim's bases and tops blended
    /// into the next coarser level's, which stands already; and the covers
    /// its edges are found from, and the nearest crossing of each along its
    /// row.
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
        let Edging {
            covers, along_rows, ..
        } = &mut self.edging;
        for ((((deck, map), covers), along_rows), coarse) in decks
            .iter()
            .zip(level.weather.iter_mut())
            .zip(covers.iter_mut())
            .zip(along_rows.iter_mut())
            .zip([0, 1].map(|slot| next.and_then(|next| next.weather.get(slot))))
        {
            let Some(deck) = deck.as_ref().filter(|deck| !cellular(deck)) else {
                continue;
            };
            let (Some(cells), Some(covers), Some(along_rows)) = (
                rows_of(map, rows.clone(), points, Column::default()),
                rows_of(covers, rows.clone(), points, 0.0),
                rows_of(along_rows, rows.clone(), points, f32::INFINITY),
            ) else {
                continue;
            };
            let lines = (rows.start..).zip(
                cells
                    .chunks_mut(points)
                    .zip(covers.chunks_mut(points).zip(along_rows.chunks_mut(points))),
            );
            tairix_parallel::for_each_drawn(runner, lines, &|(row, (line, (covers, along)))| {
                for (column, (cell, cover)) in line.iter_mut().zip(covers.iter_mut()).enumerate() {
                    let at = vertex_of(centre, half, (column, row), WEATHER_CELLS);
                    let fine;
                    (*cover, fine) = weather(deck, at, spacing);
                    let rimmed = rim(WEATHER_CELLS, (column, row));
                    *cell = rimmed_into(coarse, (column, row), rimmed).map_or(fine, |coarse| {
                        let blend = |fine: f32, field: fn(&Column) -> f32| {
                            single(f64::from(fine) + (coarse.at(field) - f64::from(fine)) * rimmed)
                        };
                        Column {
                            base: blend(fine.base, |column| column.base),
                            top: blend(fine.top, |column| column.top),
                            ..fine
                        }
                    });
                }
                form::nearest_crossings(|column| covers.get(column).copied(), spacing, along);
            });
        }
    }

    /// Level `index`'s squared distances from each vertex of its columns
    /// `columns` to the nearest crossing down its column, and to the nearest
    /// on any row; `None` when the heap will not hold the room to find them.
    fn fill_squares(
        &mut self,
        index: usize,
        columns: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let points = WEATHER_CELLS + 1;
        let spacing = 2.0 * self.levels.get(index)?.half / real(WEATHER_CELLS);
        let Edging {
            covers,
            along_rows,
            along_columns,
            from_rows,
        } = &mut self.edging;
        for ((((deck, covers), along_rows), along_columns), from_rows) in self
            .decks
            .iter()
            .zip(&*covers)
            .zip(&*along_rows)
            .zip(along_columns.iter_mut())
            .zip(from_rows.iter_mut())
        {
            if !fielded(deck.as_ref()) {
                continue;
            }
            let (Some(along_columns), Some(from_rows)) = (
                rows_of(along_columns, columns.clone(), points, f32::INFINITY),
                rows_of(from_rows, columns.clone(), points, f32::INFINITY),
            ) else {
                continue;
            };
            let lines = along_columns
                .chunks_mut(points * EDGE_LINES)
                .zip(from_rows.chunks_mut(points * EDGE_LINES))
                .enumerate();
            let laid = tairix_parallel::fold_drawn(
                runner,
                lines,
                true,
                &|(number, (along, from))| {
                    let Some(mut envelope) = Envelope::new(points) else {
                        return false;
                    };
                    for (offset, (along, from)) in along
                        .chunks_mut(points)
                        .zip(from.chunks_mut(points))
                        .enumerate()
                    {
                        let column = columns.start + number * EDGE_LINES + offset;
                        let at = |row: usize| row * points + column;
                        form::nearest_crossings(|row| covers.get(at(row)).copied(), spacing, along);
                        envelope.sweep(
                            |row| {
                                along_rows
                                    .get(at(row))
                                    .map_or(f64::INFINITY, |&square| f64::from(square))
                            },
                            spacing,
                            |row, squared| {
                                if let Some(slot) = from.get_mut(row) {
                                    *slot = single(squared);
                                }
                            },
                        );
                    }
                    true
                },
                &|one, other| one && other,
            );
            if !laid {
                return None;
            }
        }
        Some(())
    }

    /// Level `index`'s edges along its rows `rows`: from each vertex the
    /// nearer of the nearest crossing on a row and the nearest on a column,
    /// its rim blended into the next coarser level's; `None` when the heap
    /// will not hold the room to find them.
    fn fill_edges(
        &mut self,
        index: usize,
        rows: Range<usize>,
        runner: &dyn JobRunner,
    ) -> Option<()> {
        let points = WEATHER_CELLS + 1;
        if index >= self.levels.len() {
            return Some(());
        }
        let (finer, coarser) = self.levels.split_at_mut(index + 1);
        let level = finer.last_mut()?;
        let next = coarser.first();
        let spacing = 2.0 * level.half / real(WEATHER_CELLS);
        // A vertex inside the rim lies at least this far from the square's
        // edge, so no nearer edge than this beyond it is lost.
        let farthest = RIM * level.half;
        let Edging {
            covers,
            along_columns,
            from_rows,
            ..
        } = &self.edging;
        for (((((deck, map), covers), along_columns), from_rows), coarse) in self
            .decks
            .iter()
            .zip(level.weather.iter_mut())
            .zip(covers)
            .zip(along_columns)
            .zip(from_rows)
            .zip([0, 1].map(|slot| next.and_then(|next| next.weather.get(slot))))
        {
            if !fielded(deck.as_ref()) {
                continue;
            }
            let Some(cells) = map.get_mut(rows.start * points..rows.end * points) else {
                continue;
            };
            let laid = tairix_parallel::fold_bands(
                runner,
                cells,
                (0, points * EDGE_LINES),
                true,
                &|number, band| {
                    let Some(mut envelope) = Envelope::new(points) else {
                        return false;
                    };
                    for (offset, line) in band.chunks_mut(points).enumerate() {
                        let row = rows.start + number * EDGE_LINES + offset;
                        let at = |column: usize| column * points + row;
                        let square = |values: &[f32], place: usize| {
                            values
                                .get(place)
                                .map_or(f64::INFINITY, |&square| f64::from(square))
                        };
                        envelope.sweep(
                            |column| square(along_columns, at(column)),
                            spacing,
                            |column, squared| {
                                let Some(cell) = line.get_mut(column) else {
                                    return;
                                };
                                let squared = squared.min(square(from_rows, at(column)));
                                let cover =
                                    covers.get(row * points + column).copied().unwrap_or(0.0);
                                let fine = form::signed_edge(cover, squared, farthest);
                                let rimmed = rim(WEATHER_CELLS, (column, row));
                                cell.edge = rimmed_into(coarse, (column, row), rimmed).map_or(
                                    fine,
                                    |coarse| {
                                        single(
                                            f64::from(fine)
                                                + (coarse.at(|column| column.edge)
                                                    - f64::from(fine))
                                                    * rimmed,
                                        )
                                    },
                                );
                            },
                        );
                    }
                    true
                },
                &|one, other| one && other,
            );
            if !laid {
                return None;
            }
        }
        Some(())
    }

    /// Level `index`'s bands over its weather cells' rows `rows`.
    fn fill_bands(&mut self, index: usize, rows: Range<usize>, runner: &dyn JobRunner) {
        let decks = self.decks;
        let reach = decks.map(|deck| deck.map_or(0.0, |deck| self.textures.reach(&deck)));
        let Some(Level {
            weather,
            heaps,
            bands,
            ..
        }) = self.levels.get_mut(index)
        else {
            return;
        };
        let points = WEATHER_CELLS + 1;
        for ((((deck, map), heaps), cells), reach) in decks
            .iter()
            .zip(weather.iter())
            .zip(heaps.iter())
            .zip(bands.iter_mut())
            .zip(reach)
        {
            let Some(deck) = deck else {
                continue;
            };
            let Some(cells) = rows_of(cells, rows.clone(), WEATHER_CELLS, EMPTY_BAND) else {
                continue;
            };
            if cellular(deck) {
                let margin = form::standing_margin(deck, reach);
                tairix_parallel::for_each_band(
                    runner,
                    cells,
                    (rows.start, WEATHER_CELLS),
                    &|row, band| {
                        for (column, slot) in band.iter_mut().enumerate() {
                            *slot = heaps.band((column, row), margin);
                        }
                    },
                );
                continue;
            }
            tairix_parallel::for_each_band(
                runner,
                cells,
                (rows.start, WEATHER_CELLS),
                &|row, band| {
                    let at = |column: usize, row: usize| {
                        map.get(row * points + column).copied().unwrap_or_default()
                    };
                    for (column, slot) in band.iter_mut().enumerate() {
                        let corners = [
                            at(column, row),
                            at(column + 1, row),
                            at(column, row + 1),
                            at(column + 1, row + 1),
                        ];
                        *slot = band_over(deck, corners, reach);
                    }
                },
            );
        }
    }

    /// What the bank does to the sky over `point`, below its ceiling: how
    /// much of the clear sky straight overhead it leaves open, and the light
    /// its cloud scatters down from its base there. A clear sky's, and none,
    /// above it or before it is lit.
    pub(crate) fn beneath(&self, point: Vec3) -> (f64, Vec3) {
        let Some(lighting) = self.lighting.as_ref() else {
            return (1.0, Vec3::ZERO);
        };
        if self.altitude(point) >= self.ceiling {
            return (1.0, Vec3::ZERO);
        }
        let (open, [sunlit, skylit]) = self.lights.beneath(self, (point.x, point.z));
        let floor = Vec3::new(
            point.x,
            self.height_at(self.floor, (point.x, point.z)),
            point.z,
        );
        let cosine = self.cosine(floor, self.floor, lighting.toward);
        let shed =
            tint(sunlight_at(lighting, 0.0, cosine)) * sunlit + tint(lighting.above) * skylit;
        (open.clamp(0.0, 1.0), (shed * (1.0 / PI)).max(Vec3::ZERO))
    }

    /// What of the sun's light toward the unit `toward` reaches `point` past
    /// the bank: the light's own depth at the floor where the way toward the
    /// sun rises through it, or at the point itself within the bank. Nothing
    /// is shaded where the Earth stands in that way, the air answering for
    /// it.
    pub(crate) fn shadow(&self, point: Vec3, toward: Vec3) -> f64 {
        let altitude = self.altitude(point);
        if altitude >= self.ceiling {
            return 1.0;
        }
        let (at, height) = if altitude < self.floor {
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
            (point + toward * rise, self.floor)
        } else {
            (point, altitude)
        };
        self.lights
            .depth(self, (at.x, height, at.z))
            .map_or(1.0, |depth| mathf::exp(-depth))
    }

    /// What of its own sunlight the bank lets reach `point`.
    pub(crate) fn sunlit(&self, point: Vec3) -> f64 {
        self.shadow(point, self.sun)
    }

    /// What a ray from `origin` along the unit `dir` meets of the bank;
    /// `None` when it does not run through it. `spread`, the angle its
    /// footprint spans, for a ray seen directly, which resolves the cloud's
    /// edges as finely as that footprint holds them; `jitter`, in
    /// `0.0..1.0`, staggers where along each step its cloud is read, so no
    /// two samples of a pixel band alike.
    pub(crate) fn seen(
        &self,
        origin: Vec3,
        dir: Vec3,
        spread: Option<f64>,
        jitter: f64,
    ) -> Option<Seen> {
        let lighting = self.lighting.as_ref()?;
        let (enter, leave) = self.crossing(origin, dir)?;
        let reader = Reader { bank: self };
        let (share, most) = if spread.is_some() {
            (FINE_SHARE, FINE_STEPS)
        } else {
            (COARSE_SHARE, COARSE_STEPS)
        };
        let billow = self.finest_billow();
        // A step through cloud: a share of its finest billows, longer far off
        // where a pixel spans more of it, and longer still where the steps
        // left would not otherwise reach the bank's far side.
        let within = |t: f64, taken: u32| {
            let left = f64::from(most.saturating_sub(taken).max(1));
            (billow * share * (1.0 + t / STRETCH)).max((leave - t) / left)
        };
        let glance = self.glance(dir);
        let per_height = 1.0 / (self.ceiling - self.floor);
        let course = self.course(origin, dir);
        let mut cursor = self.cursor(origin, dir, enter);
        let mut over = self.over(&cursor);
        let most_jumps = JUMPS_A_LEVEL * self.levels.len();
        let (mut t, mut steps, mut jumps) = (enter, 0, 0_usize);
        // What of the light beyond shows through, the light gathered, and how
        // far off the cloud lies, weighted by what each step hides.
        let (mut kept, mut light) = (1.0, Vec3::ZERO);
        let (mut depth, mut weight) = (0.0, 0.0);
        // Whether a ray seen directly is stepping finely through a cloud's
        // billows, how far it has stepped since it last met cloud, and how far
        // its last sample says it may step on within the cloud's body.
        let (mut finely, mut cleared, mut room) = (false, 0.0, 0.0);
        while t < leave && steps < most {
            let stride = within(t, steps + 1).min(leave - t);
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
                t = next.max(t + 1e-3 * stride);
                finely = false;
                continue;
            }
            steps += 1;
            // Striding on to where the billows stand, then stepping finely
            // from the stride's start, so their edge is never stepped over.
            if let (Some(spread), false) = (spread, finely) {
                let ahead = origin + dir * (t + stride);
                let bound = Edges::Bounding(spread * (t + stride));
                if reader
                    .density_at(ahead, self.altitude(ahead), bound)
                    .is_some()
                {
                    (finely, cleared) = (true, 0.0);
                } else {
                    t += stride;
                }
                continue;
            }
            let step = spread.map_or(stride, |spread| {
                (SHARP * spread * t).max(SHARPEST).max(room).min(stride)
            });
            let at = t + step * jitter;
            let point = origin + dir * at;
            let altitude = self.altitude(point);
            let edges = spread.map_or(Edges::Averaged, |spread| Edges::Resolved(spread * at));
            let Some(sample) = reader
                .density_at(point, altitude, edges)
                .filter(|sample| sample.extinction > 1e-7)
            else {
                cleared += step;
                finely &= cleared <= stride;
                room = 0.0;
                t += step;
                continue;
            };
            cleared = 0.0;
            let extinction = sample.extinction;
            room = (WITHIN_SHARE * sample.within)
                .min(STEP_DEPTH / extinction.max(1e-12))
                .max(0.0);
            let height = ((altitude - self.floor) * per_height).clamp(0.0, 1.0);
            let source = self.source(
                (lighting, &glance),
                (point, altitude, height),
                (&sample, edges),
            ) * extinction;
            let through = mathf::exp(-extinction * step);
            let gained = (1.0 - through) / extinction.max(1e-12);
            light += source * (kept * gained);
            let lost = kept * (1.0 - through);
            depth += at * lost;
            weight += lost;
            kept *= through;
            if kept < OPAQUE {
                // What lies deeper sends on what this sample does.
                light += source * (kept / extinction.max(1e-12));
                depth += at * kept;
                weight += kept;
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

    /// How a ray along the unit `dir` meets the bank's sun.
    fn glance(&self, dir: Vec3) -> Glance {
        let cos = dir.dot(self.sun);
        Glance {
            out: -dir,
            phase: match self.medium() {
                Matter::Water => dual_phase(cos, (DROPLET_LOBES.0, BACK_LOBE), DROPLET_LOBES.1),
                Matter::Ice => henyey_greenstein(cos, ICE_ASYMMETRY),
            },
            onward: match self.medium() {
                Matter::Water => {
                    let (forward, share) = DROPLET_LOBES;
                    dual_phase(cos, (forward * forward, BACK_LOBE * forward), share)
                }
                Matter::Ice => henyey_greenstein(cos, ICE_ASYMMETRY * ICE_ASYMMETRY),
            },
        }
    }

    /// The light `sample` scatters back along a ray that meets the sun as
    /// `glance` has it, per unit of its optical depth: read at `point`,
    /// `altitude` above the scene's level and `height` of the way from the
    /// bank's floor to its ceiling, its edges taken as `edges` has them.
    ///
    /// The sun's light scattered once, exactly, through the phase; what the
    /// phase's forward peak carries on past that, through the δ-Eddington
    /// extinction the beam photons were followed through, scattered toward
    /// the ray through the phase composed with the peak; and the diffuse
    /// light, its fluence and its net flux through the full phase's
    /// asymmetry, so cloud seen toward where its diffuse light runs is the
    /// brighter.
    fn source(
        &self,
        (lighting, glance): (&Lighting, &Glance),
        (point, altitude, height): (Vec3, f64, f64),
        (sample, edges): (&Sample, Edges),
    ) -> Vec3 {
        let reader = Reader { bank: self };
        let at = (point.x, altitude, point.z);
        let optical = reader.optical_depth(point, altitude, edges, sample);
        let cosine = self.cosine(point, altitude, lighting.toward);
        let sunlight = sunlight_at(lighting, height, cosine);
        let sun = sunlight * self.lights.above(self, at);
        let scattering = self.scattering();
        let (g, forward) = (scattering.asymmetry, scattering.forward());
        let (direct, carried) = (mathf::exp(-optical), mathf::exp(-(1.0 - forward) * optical));
        let once = sun * (glance.phase * direct);
        let peak = sun * (glance.onward * (carried - direct).max(0.0));
        let reading = self.lights.reading(self, at).unwrap_or_default();
        let [sunlit, skylit] = reading.fluence.map(|fluence| fluence.max(0.0));
        let (sun_tint, sky_tint) = (tint(sunlight), tint(lighting.above));
        let mean = sunlit + skylit;
        let diffuse = (sun_tint * sunlit + sky_tint * skylit) * (1.0 / (4.0 * PI));
        let first = if mean > 0.0 {
            let rising = (3.0 * g * reading.flux.dot(glance.out)).clamp(-mean, mean);
            (sun_tint * sunlit + sky_tint * skylit) * (rising / (mean * 4.0 * PI))
        } else {
            Vec3::ZERO
        };
        (once + peak + diffuse + first) * scattering.albedo
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

    /// Whether any deck's cloud can stand `altitude` above the scene's level
    /// over `(x, z)`, as the band of the weather cell beneath has it.
    fn may_stand(&self, (x, z): (f64, f64), altitude: f64) -> bool {
        let Some(level) = self.finest((x, z)).and_then(|index| self.levels.get(index)) else {
            return false;
        };
        let size = 2.0 * level.half / real(WEATHER_CELLS);
        let cell = |offset: f64| {
            cell_of((offset + level.half) / size)
                .0
                .min(WEATHER_CELLS - 1)
        };
        let at = cell(z - self.centre.1) * WEATHER_CELLS + cell(x - self.centre.0);
        level.bands.iter().any(|bands| {
            bands
                .get(at)
                .is_some_and(|&(low, high)| (f64::from(low)..=f64::from(high)).contains(&altitude))
        })
    }

    /// The walk across level `level`'s weather map of a ray from `origin`
    /// along the unit `dir`, from `t` along it.
    fn walk(&self, level: usize, origin: Vec3, dir: Vec3, t: f64) -> Walk {
        let half = self.levels.get(level).map_or(self.most, |level| level.half);
        let corner = (self.centre.0 - half, self.centre.1 - half);
        let cells = u32::try_from(WEATHER_CELLS).unwrap_or(u32::MAX);
        let grid = (corner, 2.0 * half / real(WEATHER_CELLS));
        Walk::within((grid, cells), &Ray::new(origin, dir), t)
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
        let before = mapped(&cursor.walk);
        cursor.walk.advance(t);
        let now = mapped(&cursor.walk);
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
            .zip(mapped(&cursor.walk))
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
            until: cursor.walk.exit(),
        }
    }
}

/// `light` as a colour of unit luminance: nothing where it holds none.
fn tint(light: Vec3) -> Vec3 {
    let luminance = light.luminance();
    if luminance > 1e-30 {
        light * (1.0 / luminance)
    } else {
        Vec3::ZERO
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

/// A blend of a forward and a backward Henyey–Greenstein lobe.
fn dual_phase(cos: f64, (forward, backward): (f64, f64), back: f64) -> f64 {
    henyey_greenstein(cos, forward) * (1.0 - back) + henyey_greenstein(cos, backward) * back
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

/// What a sample of the bank holds at a point: its extinction per metre;
/// the breadth of the billows of the deck holding the most of it; and how far
/// the place lies within every deck's surface it lies within, in metres.
struct Sample {
    extinction: f64,
    billow: f64,
    within: f64,
}

/// How a ray meets a bank's sun: the unit way back toward the eye, and the
/// phase at which its matter scatters sunlight back along the ray.
struct Glance {
    out: Vec3,
    phase: f64,
    /// The phase composed with its forward lobe, for the light the forward
    /// peak carries on already spread by that lobe: Henyey–Greenstein lobes
    /// compose as the product of their asymmetries.
    onward: f64,
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

/// The weather cell `walk` stands over, by its column and row; `None` off
/// the map.
fn mapped(walk: &Walk) -> Option<(usize, usize)> {
    let index = |at: u32| usize::try_from(at).ok().filter(|&at| at < WEATHER_CELLS);
    Some((index(walk.cell.0)?, index(walk.cell.1)?))
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
    /// The cloud at `point`, its edges taken as `edges` has them; `None`
    /// where there is none.
    fn density(&self, point: Vec3, edges: Edges) -> Option<Sample> {
        self.density_at(point, self.bank.altitude(point), edges)
    }

    /// The cloud at `point`, `altitude` above the scene's level: every deck's
    /// extinction there, with the billows of the deck holding the most;
    /// `None` where there is none.
    fn density_at(&self, point: Vec3, altitude: f64, edges: Edges) -> Option<Sample> {
        let bank = self.bank;
        let level = bank.level_over(point)?;
        let place = place_on(bank.centre, level.half, (point.x, point.z), WEATHER_CELLS);
        // The billows are read over the deck as it lies along the Earth's
        // curve.
        let lying = Vec3::new(point.x, altitude, point.z);
        let mut held: Option<(Sample, f64)> = None;
        for (((slot, deck), map), heaps) in bank
            .decks
            .iter()
            .enumerate()
            .zip(&level.weather)
            .zip(&level.heaps)
        {
            let Some(deck) = deck else {
                continue;
            };
            let placed = if cellular(deck) {
                let cell = |at: f64| cell_of(at).0.min(WEATHER_CELLS - 1);
                let within = (0.0..real(WEATHER_CELLS)).contains(&place.0)
                    && (0.0..real(WEATHER_CELLS)).contains(&place.1);
                within
                    .then(|| {
                        heaps.placed(
                            cell(place.1) * WEATHER_CELLS + cell(place.0),
                            (point.x, point.z),
                            altitude,
                            form::read_margin(deck, &bank.textures, edges),
                        )
                    })
                    .flatten()
            } else {
                about(map, WEATHER_CELLS + 1, place)
                    .and_then(|(corners, across)| Blend { corners, across }.placed(deck, altitude))
            };
            let Some(placed) = placed else {
                continue;
            };
            let grained = bank.grained(slot, deck, lying);
            let Some(cloud) = form::cloud(deck, &bank.textures, placed, grained, edges) else {
                continue;
            };
            let own = cloud.extinction;
            let (total, billow, most, within) = match held {
                Some((sample, most)) if most >= own => (
                    sample.extinction + own,
                    sample.billow,
                    most,
                    sample.within.min(cloud.within),
                ),
                Some((sample, _)) => (
                    sample.extinction + own,
                    deck.billow,
                    own,
                    sample.within.min(cloud.within),
                ),
                None => (own, deck.billow, own, cloud.within),
            };
            held = Some((
                Sample {
                    extinction: total,
                    billow,
                    within,
                },
                most,
            ));
        }
        held.map(|(sample, _)| sample)
    }

    /// The optical depth toward the sun from `point`, `altitude` above the
    /// scene's level, its edges read as `edges` has them: for a resolved
    /// sample short taps through its own billows, the nearest through its
    /// edges as finely, and the bank's light past the last; else the bank's
    /// light alone.
    fn optical_depth(&self, point: Vec3, altitude: f64, edges: Edges, sample: &Sample) -> f64 {
        let bank = self.bank;
        let depth = |at: Vec3, altitude: f64| {
            bank.lights
                .depth(bank, (at.x, altitude, at.z))
                .unwrap_or(0.0)
        };
        // Ice is too thin for its own streaks to shade it more finely than the
        // bank's light does.
        let (Edges::Resolved(footprint), Matter::Water) = (edges, bank.medium()) else {
            return depth(point, altitude);
        };
        let (sun, billow) = (bank.sun, sample.billow);
        let margin = bank.lights.breadth(bank, (point.x, point.z));
        let (mut tapped, mut last, mut clear, mut tap) = (0.0, 0.0, 0.0, 0);
        let mut reach = billow * TAPS[0];
        loop {
            let at = point + sun * f64::midpoint(last, reach);
            // Each tap stands for its stretch, so it resolves no more finely.
            let read = if tap < RESOLVED_TAPS {
                Edges::Resolved(footprint.max(reach - last))
            } else {
                Edges::Averaged
            };
            match self.density(at, read) {
                Some(sample) => {
                    tapped += sample.extinction * (reach - last);
                    clear = 0.0;
                }
                None => clear += reach - last,
            }
            last = reach;
            if tapped > UNLIT || clear >= margin || last >= FARTHEST_TAP * billow {
                break;
            }
            tap += 1;
            reach = TAPS
                .get(tap)
                .map_or(last * TAP_GROWTH, |&fraction| billow * fraction);
        }
        let end = point + sun * last;
        tapped + depth(end, bank.altitude(end))
    }
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
