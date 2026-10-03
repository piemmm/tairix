//! Volumetric cloud: heaped cumulus, a broken sheet of stratocumulus, a field
//! of altocumulus, or high streaks of cirrus, marched through as the medium
//! it is.
//!
//! A cloudbank is up to two decks of cloud over a square tens of kilometres
//! across, thinning out toward its rim. Each deck has a weather map — how
//! much of each column it covers, and where its base and top lie there, so no
//! two clouds sit at the same height — and its density within a column is a
//! vertical profile, flat beneath and heaped above, times a billowing noise,
//! eroded at its edges by a finer one (Schneider, "The Real-time Volumetric
//! Cloudscapes of Horizon: Zero Dawn", 2015). Both noises are Perlin–Worley
//! textures that tile, built once per scene, so a sample reads them rather
//! than evaluating them.
//!
//! A ray gathers what the cloud scatters toward it step by step, each step's
//! light integrated exactly over the step (Hillaire, "Physically Based Sky,
//! Atmosphere and Cloud Rendering in Frostbite", 2016), and strides across
//! the air above and below the heights each weather cell bounds its cloud
//! to, which its cover and the billows' peak fix. The sun's light at a
//! step is dimmed by the cloud toward the sun — a grid of that optical depth
//! built once, and four short taps for the billows' own shadows — and spread
//! by several scattering octaves, each lighter and broader than the last, for
//! the brightness multiple scattering gives (Wrenninge, Kulla and Lundqvist,
//! "Oz: The Great and Volumetric", 2013). Each height's sunlight has already
//! crossed the air, so a deck still above the Earth's shadow at dusk is lit
//! red from beneath while the ground below lies in blue shade.
//!
//! Cirrus is ice, too thin for those octaves, which brighten a cloud by
//! light it is too thin to have scattered twice. Its sunlight is scattered
//! once exactly, by rough ice crystals' phase function; the light scattered
//! again and again is Hillaire's isotropic series over the deck as its mean
//! extinction lays it out by height, scaled by the similarity principle for
//! how far forward ice throws it; and the sky's light and the ground's are
//! scattered in by the share of the phase each hemisphere sends the eye.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::atmosphere::every_order;
use crate::band;
use crate::heightfield::bilinear;
use crate::lanes::Corners;
use crate::noise::{cell, fbm2, hash3, noise2, smoothstep};
use crate::sample::{mix32, unit, GOLDEN_RATIO};
use crate::shape::reciprocal;
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
/// Weather-map columns along each side of the bank.
const WEATHER_SIDE: usize = 384;
/// The sun's optical-depth grid: columns across, and layers up.
const LIGHT_SIDE: usize = 96;
const LIGHT_LAYERS: usize = 20;
/// Rows of that grid a core fills in a unit: a quarter of a layer.
const LIGHT_ROWS: usize = LIGHT_SIDE / 4;
/// Steps taken toward the sun building that grid, and across the bank for
/// the ground's shadow.
const LIGHT_STEPS: u32 = 24;
/// Texels along each side of the shadow the bank casts.
const SHADOW_SIDE: usize = 256;
/// The share of a bank's half-breadth its cover thins out over at its edge.
const EDGE: f64 = 0.1;
/// Heights the sunlight reaching the bank is tabulated at.
pub(crate) const SUNLIGHT_LEVELS: usize = 48;

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
/// The most jumps a ray takes through air no cloud can stand in: into the
/// heights cloud can reach over a weather cell and on out of the cell, for as
/// many cells as a straight line across the map can cross.
const MOST_JUMPS: usize = 4 * WEATHER_SIDE;
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

/// One column of the weather map: for each deck, how much it covers there
/// and where its base and top lie.
#[derive(Copy, Clone, Debug, Default)]
struct Column {
    cover: [f32; 2],
    base: [f32; 2],
    top: [f32; 2],
}

/// How far a cloudbank is built.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    Shape(usize),
    Detail(usize),
    Weather(usize),
    Light(usize),
    Shadow(usize),
    Scatter,
    Done,
}

/// What lights the bank, once the air says.
#[derive(Clone, Debug)]
pub(crate) struct Lighting {
    /// The sun's light reaching each height of the bank, from its floor to
    /// its ceiling.
    pub(crate) sunlight: Vec<Vec3>,
    /// The sky's light on a cloud's upper side, and the ground's on its
    /// underside.
    pub(crate) above: Vec3,
    pub(crate) below: Vec3,
}

/// A bank of volumetric cloud.
#[derive(Clone, Debug)]
pub(crate) struct Cloudbank {
    decks: [Option<Deck>; 2],
    /// The bank's middle, half its breadth, and the heights between which
    /// any of it lies.
    centre: (f64, f64),
    half: f64,
    floor: f64,
    ceiling: f64,
    /// The unit direction its sunlight comes from, as the air bends it to
    /// the bank's height.
    sun: Vec3,
    /// Each deck's heading, as its cosine and sine, and its fibre's
    /// reciprocal.
    headings: [(f64, f64, f64); 2],
    /// How far apart the weather map's columns lie, and how many lie to a
    /// metre; the light grid's points to a metre across and up.
    spacing: (f64, f64),
    gridding: (f64, f64),
    shape: Tile,
    detail: Tile,
    weather: Vec<Column>,
    /// For each of the weather map's cells and each deck, the heights
    /// between which that deck's cloud over it can stand: every height until
    /// the map is built.
    bands: Vec<[(f32, f32); 2]>,
    /// The optical depth of the bank's own cloud toward the sun from each
    /// point of a coarse grid; and what of the sunlight the bank above lets
    /// through to each, which is empty with none above.
    light: Vec<f32>,
    overhead: Vec<f32>,
    /// What of the sun's light the bank lets through to each point of the
    /// ground beneath it.
    shadow: Vec<f32>,
    /// What ice scatters again and again at each height, per unit of the
    /// sunlight there; and for each way toward the eye, the share of ice's
    /// phase the sky above sends it.
    scatter: [f32; SCATTER_LEVELS],
    downward: [f32; HEMISPHERE],
    lighting: Option<Lighting>,
    stage: Stage,
}

impl Cloudbank {
    /// A bank of `decks` centred over `centre`, `half` either way, lit from
    /// the unit `sun`; its textures and grids still to build. `None` when the
    /// heap will not hold them.
    pub(crate) fn new(
        decks: [Option<Deck>; 2],
        centre: (f64, f64),
        half: f64,
        sun: Vec3,
    ) -> Option<Self> {
        let (mut floor, mut ceiling) = (f64::INFINITY, f64::NEG_INFINITY);
        for deck in decks.iter().flatten() {
            floor = floor.min(deck.base - deck.base_spread);
            ceiling = ceiling.max(deck.base + deck.base_spread + deck.depth.1);
        }
        let floor = floor.max(0.0);
        if floor >= ceiling {
            return None;
        }
        let spacing = 2.0 * half / real(WEATHER_SIDE - 1);
        Some(Self {
            decks,
            centre,
            half,
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
            spacing: (spacing, 1.0 / spacing),
            gridding: (
                real(LIGHT_SIDE - 1) / (2.0 * half),
                real(LIGHT_LAYERS - 1) / (ceiling - floor),
            ),
            shape: Tile::new(SHAPE_SIDE)?,
            detail: Tile::new(DETAIL_SIDE)?,
            weather: fallible::filled(WEATHER_SIDE * WEATHER_SIDE, Column::default())?,
            bands: fallible::filled(
                (WEATHER_SIDE - 1) * (WEATHER_SIDE - 1),
                [(f32::NEG_INFINITY, f32::INFINITY); 2],
            )?,
            light: fallible::filled(LIGHT_SIDE * LIGHT_SIDE * LIGHT_LAYERS, 0.0)?,
            overhead: Vec::new(),
            shadow: fallible::filled(SHADOW_SIDE * SHADOW_SIDE, 1.0)?,
            scatter: [0.0; SCATTER_LEVELS],
            downward: [0.0; HEMISPHERE],
            lighting: None,
            stage: Stage::Shape(0),
        })
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

    /// Move the bank to spread about `centre`, before it is built.
    pub(crate) fn centre_on(&mut self, centre: (f64, f64)) {
        self.centre = centre;
    }

    /// The heights between which the bank lies.
    pub(crate) const fn span(&self) -> (f64, f64) {
        (self.floor, self.ceiling)
    }

    /// Light the bank as `lighting` has it.
    pub(crate) fn light_by(&mut self, lighting: Lighting) {
        self.lighting = Some(lighting);
    }

    /// Build the next unit of the bank across `runner`, its sunlight already
    /// dimmed by the bank `above` it; whether it is built, or `None` when the
    /// heap will not hold it.
    pub(crate) fn step(&mut self, runner: &dyn JobRunner, above: Option<&Self>) -> Option<bool> {
        let width = runner.width().max(1);
        match self.stage {
            Stage::Shape(layer) => {
                let end = (layer + width).min(SHAPE_SIDE);
                let seed = self
                    .decks
                    .iter()
                    .flatten()
                    .next()
                    .map_or(1, |deck| deck.seed);
                fill_tile(&mut self.shape, layer..end, runner, &|p| billows(p, seed));
                self.stage = if end >= SHAPE_SIDE {
                    Stage::Detail(0)
                } else {
                    Stage::Shape(end)
                };
            }
            Stage::Detail(layer) => {
                let end = (layer + 4 * width).min(DETAIL_SIDE);
                let seed = self
                    .decks
                    .iter()
                    .flatten()
                    .next()
                    .map_or(1, |deck| deck.seed)
                    ^ 0xd37a;
                fill_tile(&mut self.detail, layer..end, runner, &|p| wisps(p, seed));
                self.stage = if end >= DETAIL_SIDE {
                    Stage::Weather(0)
                } else {
                    Stage::Detail(end)
                };
            }
            Stage::Weather(row) => {
                let end = (row + 8 * width).min(WEATHER_SIDE);
                self.fill_weather(row..end, runner);
                if end >= WEATHER_SIDE {
                    self.fill_bands(runner);
                    self.stage = Stage::Light(0);
                } else {
                    self.stage = Stage::Weather(end);
                }
            }
            Stage::Light(row) => {
                let rows = LIGHT_LAYERS * LIGHT_SIDE;
                let end = (row + LIGHT_ROWS * width).min(rows);
                self.fill_light(row..end, runner, above)?;
                self.stage = if end >= rows {
                    Stage::Shadow(0)
                } else {
                    Stage::Light(end)
                };
            }
            Stage::Shadow(row) => {
                let end = (row + 4 * width).min(SHADOW_SIDE);
                self.fill_shadow(row..end, runner)?;
                self.stage = if end >= SHADOW_SIDE {
                    Stage::Scatter
                } else {
                    Stage::Shadow(end)
                };
            }
            Stage::Scatter => {
                self.fill_scatter();
                self.stage = Stage::Done;
            }
            Stage::Done => {}
        }
        Some(self.stage == Stage::Done)
    }

    /// The weather map's rows `rows`.
    fn fill_weather(&mut self, rows: Range<usize>, runner: &dyn JobRunner) {
        let (decks, centre, half, step) = (self.decks, self.centre, self.half, self.spacing.0);
        let Some(cells) = self
            .weather
            .get_mut(rows.start * WEATHER_SIDE..rows.end * WEATHER_SIDE)
        else {
            return;
        };
        band::for_each(runner, cells, (rows.start, WEATHER_SIDE), &|row, band| {
            let z = centre.1 - half + real(row) * step;
            for (column, cell) in band.iter_mut().enumerate() {
                let x = centre.0 - half + real(column) * step;
                // A bank thins out over the last tenth of its breadth, where
                // it would otherwise stand cut off in a wall of cloud.
                let out = (x - centre.0).abs().max((z - centre.1).abs()) / half;
                let thinning = smoothstep(1.0, 1.0 - EDGE, out);
                for (slot, deck) in decks.iter().enumerate() {
                    if let Some(deck) = deck {
                        let (cover, base, top) = weather(deck, x, z);
                        cell.cover[slot] = single(cover * thinning);
                        cell.base[slot] = single(base);
                        cell.top[slot] = single(top);
                    }
                }
            }
        });
    }

    /// Each weather cell's band, once the whole map is built.
    fn fill_bands(&mut self, runner: &dyn JobRunner) {
        let (decks, peak, side) = (self.decks, self.shape.peak(), WEATHER_SIDE - 1);
        let weather = &self.weather;
        band::for_each(runner, &mut self.bands, (0, side), &|row, cells| {
            for (column, slot) in cells.iter_mut().enumerate() {
                *slot = band_over(&decks, weather, (column, row), peak);
            }
        });
    }

    /// The light grid's rows `rows`, counted up through its layers: at each
    /// point, its own cloud's optical depth toward the sun, and what of the
    /// sunlight the bank `above` lets through; `None` when the heap will not
    /// hold them. Kept apart, since cloud above dims the sunlight a cloud's
    /// scattering takes in, and adds nothing to the depth its own edges are
    /// powdered and its octaves spread by.
    fn fill_light(
        &mut self,
        rows: Range<usize>,
        runner: &dyn JobRunner,
        above: Option<&Self>,
    ) -> Option<()> {
        let reader = Reader { bank: self };
        let sun = self.sun;
        let mut points = fallible::filled(rows.len() * LIGHT_SIDE, (0.0f32, 1.0f32))?;
        band::for_each(
            runner,
            &mut points,
            (rows.start, LIGHT_SIDE),
            &|row, band| {
                let (layer, across) = (row / LIGHT_SIDE, row % LIGHT_SIDE);
                for (column, slot) in band.iter_mut().enumerate() {
                    let point = reader.light_point(column, across, layer);
                    let overhead = above.map_or(1.0, |high| high.shadow(point, sun));
                    *slot = (
                        single(reader.toward_sun(point, LIGHT_STEPS)),
                        single(overhead),
                    );
                }
            },
        );
        let held = rows.start * LIGHT_SIDE..rows.end * LIGHT_SIDE;
        for (slot, &(depth, _)) in self.light.get_mut(held.clone())?.iter_mut().zip(&points) {
            *slot = depth;
        }
        if above.is_some() {
            if self.overhead.is_empty() {
                self.overhead = fallible::filled(self.light.len(), 1.0)?;
            }
            for (slot, &(_, kept)) in self.overhead.get_mut(held)?.iter_mut().zip(&points) {
                *slot = kept;
            }
        }
        Some(())
    }

    /// The shadow map's rows `rows`: what of the sun's light crosses the whole
    /// bank to each point of its floor.
    fn fill_shadow(&mut self, rows: Range<usize>, runner: &dyn JobRunner) -> Option<()> {
        let reader = Reader { bank: self };
        let mut values = fallible::filled(rows.len() * SHADOW_SIDE, 1.0f32)?;
        let step = 2.0 * self.half / real(SHADOW_SIDE - 1);
        let (centre, half, floor) = (self.centre, self.half, self.floor);
        band::for_each(
            runner,
            &mut values,
            (rows.start, SHADOW_SIDE),
            &|row, band| {
                let z = centre.1 - half + real(row) * step;
                for (column, slot) in band.iter_mut().enumerate() {
                    let x = centre.0 - half + real(column) * step;
                    let depth = reader.toward_sun(Vec3::new(x, floor, z), LIGHT_STEPS * 2);
                    *slot = single(mathf::exp(-depth));
                }
            },
        );
        self.shadow
            .get_mut(rows.start * SHADOW_SIDE..rows.end * SHADOW_SIDE)?
            .copy_from_slice(&values);
        Some(())
    }

    /// What ice scatters again and again at each height through the bank,
    /// per unit of the sunlight there: Hillaire's isotropic series, its
    /// scattering scaled by `1 − g` for how far forward ice throws its light,
    /// over the bank laid out as its mean extinction by height and endless
    /// across. Nothing where the bank holds no ice.
    fn fill_scatter(&mut self) {
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
            let y = level_height(level);
            let mut total = 0.0;
            for column in 0..SCATTER_COLUMNS {
                // Columns spread evenly over the bank: one coordinate in even
                // steps, the other in the golden ratio's.
                let u = (f64::from(column) + 0.5) / f64::from(SCATTER_COLUMNS);
                let (_, v) = cell(f64::from(column) * GOLDEN_RATIO);
                let point = Vec3::new(
                    self.centre.0 + self.half * (2.0 * u - 1.0),
                    y,
                    self.centre.1 + self.half * (2.0 * v - 1.0),
                );
                if let Some(sample) = reader.density(point, false) {
                    if sample.matter == Matter::Ice {
                        total += sample.density * sample.thickness;
                    }
                }
            }
            *slot = scale * total / f64::from(SCATTER_COLUMNS);
        }
        let profile = Profile::new(mean, (self.floor, self.ceiling));
        let (sun, breadth) = (self.sun, 2.0 * self.half);
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
    /// the bank.
    pub(crate) fn shadow(&self, point: Vec3, toward: Vec3) -> f64 {
        if point.y >= self.ceiling || toward.y < 0.01 {
            return 1.0;
        }
        let rise = (self.floor - point.y).max(0.0) / toward.y;
        let at = point + toward * rise;
        let texel = |value: f64, origin: f64| {
            (value - origin + self.half) / (2.0 * self.half) * real(SHADOW_SIDE - 1)
        };
        let (u, v) = (texel(at.x, self.centre.0), texel(at.z, self.centre.1));
        if !(0.0..real(SHADOW_SIDE - 1)).contains(&u) || !(0.0..real(SHADOW_SIDE - 1)).contains(&v)
        {
            return 1.0;
        }
        bilinear2(&self.shadow, SHADOW_SIDE, (u, v))
    }

    /// What a ray from `origin` along the unit `dir` meets of the bank: the
    /// light the cloud sends back along it, how much of what lies beyond it
    /// shows through, and how far off the cloud it met lies, on the whole;
    /// `None` when it meets none. `fine` for a ray seen directly, which takes
    /// every step and the cloud's finest edges; `jitter`, in `0.0..1.0`,
    /// staggers where along each step its cloud is read, so no two samples of
    /// a pixel band alike.
    pub(crate) fn seen(
        &self,
        origin: Vec3,
        dir: Vec3,
        fine: bool,
        jitter: f64,
    ) -> Option<(Vec3, f64, f64)> {
        let lighting = self.lighting.as_ref()?;
        let (enter, leave) = self.slab(origin, dir)?;
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
        let mut walk = self.walk(origin, dir, enter);
        let mut over = self.over(&walk);
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
                walk.advance(t);
                over = self.over(&walk);
            }
            // Air no cloud can stand in is crossed in one jump to where the
            // ray might next meet some, which spends none of the steps the
            // cloud beyond it is owed.
            if let Some(next) = over.clear((origin + dir * t).y, dir.y, t) {
                jumps += 1;
                if jumps > MOST_JUMPS {
                    break;
                }
                t = next.max(t + 1e-3 * step);
                continue;
            }
            steps += 1;
            let at = t + step * jitter;
            let point = origin + dir * at;
            let Some(sample) = reader
                .density(point, fine)
                .filter(|sample| sample.density > 1e-4)
            else {
                t += step;
                continue;
            };
            let extinction = sample.density * sample.thickness;
            let optical = reader.optical_depth(point, fine, sample.billow);
            let height = ((point.y - self.floor) * per_height).clamp(0.0, 1.0);
            let sun = sunlight_at(lighting, height) * reader.overhead_at(point);
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
        (weight > 1e-6).then(|| (light, kept, depth / weight))
    }

    /// The walk across the weather map of a ray from `origin` along the unit
    /// `dir`, from `t` along it, where it lies over the bank.
    fn walk(&self, origin: Vec3, dir: Vec3, t: f64) -> Walk {
        let point = origin + dir * t;
        let per = self.spacing.1;
        let axis = |at: f64, centre: f64, toward: f64| {
            let place = (at - centre + self.half) * per;
            // Rounding may leave a ray entering at the rim a hair off the map.
            let cell = cell_of(place).0.min(WEATHER_SIDE - 2);
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

    /// What a march following `walk` stands over: the heights each deck's
    /// cloud in its cell can stand between, none off the map, and where it
    /// leaves the cell.
    fn over(&self, walk: &Walk) -> Over {
        let widen = |(low, high): (f32, f32)| (f64::from(low), f64::from(high));
        Over {
            bands: walk
                .cell()
                .and_then(|(column, row)| self.bands.get(row * (WEATHER_SIDE - 1) + column))
                .map_or([(f64::INFINITY, f64::NEG_INFINITY); 2], |bands| {
                    bands.map(widen)
                }),
            until: walk.until(),
        }
    }

    /// The weather map's cell over `(x, z)`, by its south-west corner, and
    /// how far across it toward the next the point lies; `None` off the bank.
    fn weather_cell(&self, x: f64, z: f64) -> Option<((usize, f64), (usize, f64))> {
        let per = self.spacing.1;
        let (across, down) = (
            (x - self.centre.0 + self.half) * per,
            (z - self.centre.1 + self.half) * per,
        );
        let limit = real(WEATHER_SIDE - 1);
        ((0.0..limit).contains(&across) && (0.0..limit).contains(&down))
            .then(|| (cell_of(across), cell_of(down)))
    }

    /// Where a ray from `origin` along `dir` crosses the bank's slab and its
    /// square, if it does.
    fn slab(&self, origin: Vec3, dir: Vec3) -> Option<(f64, f64)> {
        let (x, z) = self.centre;
        let bank = Corners {
            min: [x - self.half, self.floor, z - self.half],
            max: [x + self.half, self.ceiling, z + self.half],
        };
        let (enter, leave) = bank.crossing(origin, reciprocal(dir));
        (enter < leave).then_some((enter, leave))
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
/// ceiling.
fn sunlight_at(lighting: &Lighting, height: f64) -> Vec3 {
    let levels = &lighting.sunlight;
    interpolated(
        levels.len(),
        height.clamp(0.0, 1.0) * real(levels.len().saturating_sub(1)),
        |index| levels.get(index).copied().unwrap_or(Vec3::ZERO),
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

/// Heights at which the sunlight reaching a bank spanning `span` is taken.
pub(crate) fn sunlight_levels((floor, ceiling): (f64, f64)) -> impl Iterator<Item = f64> {
    (0..SUNLIGHT_LEVELS)
        .map(move |level| floor + (ceiling - floor) * real(level) / real(SUNLIGHT_LEVELS - 1))
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

/// A point among the weather map's columns: the four about it, and how far
/// east and north across them it lies; each field blended only as it is
/// read.
struct Blend {
    corners: [Column; 4],
    right: f64,
    lower: f64,
}

impl Blend {
    /// Deck `slot`'s `field` at the point.
    fn at(&self, field: fn(&Column) -> [f32; 2], slot: usize) -> f64 {
        bilinear(
            self.corners.map(|corner| f64::from(field(&corner)[slot])),
            (self.right, self.lower),
        )
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
    /// Where a ray at height `y`, rising `rise` a unit along it, `t` along
    /// it in air no cloud of the cell can stand in, might next meet some:
    /// where it reaches a deck's band, or else leaves the cell; `None` where
    /// cloud can stand at `t`.
    fn clear(&self, y: f64, rise: f64, t: f64) -> Option<f64> {
        if self
            .bands
            .iter()
            .any(|&(low, high)| (low..=high).contains(&y))
        {
            return None;
        }
        let into = |&(low, high): &(f64, f64)| {
            if y < low && rise > 1e-12 {
                t + (low - y) / rise
            } else if y > high && rise < -1e-12 {
                t + (high - y) / rise
            } else {
                f64::INFINITY
            }
        };
        Some(self.bands.iter().map(into).fold(self.until, f64::min))
    }
}

/// A ray's walk across the weather map a cell at a time (Amanatides and Woo,
/// "A Fast Voxel Traversal Algorithm for Ray Tracing", 1987): the cell it
/// stands over, which may lie off the map; and east and north, which way it
/// steps, how far along the ray it next crosses a line of cells, and how far
/// apart along the ray those lines lie.
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
        let index = |at: isize| usize::try_from(at).ok().filter(|&at| at < WEATHER_SIDE - 1);
        Some((index(self.cell[0])?, index(self.cell[1])?))
    }
}

/// A bank read at trace time.
struct Reader<'a> {
    bank: &'a Cloudbank,
}

impl Reader<'_> {
    /// The densest cloud of any deck at `point`; `None` where there is none.
    fn density(&self, point: Vec3, fine: bool) -> Option<Sample> {
        let bank = self.bank;
        let column = self.column(point.x, point.z)?;
        let mut best: Option<Sample> = None;
        for (slot, deck) in bank.decks.iter().enumerate() {
            let Some(deck) = deck else {
                continue;
            };
            let cover = column.at(|corner| corner.cover, slot);
            if cover <= 0.0 {
                continue;
            }
            let (base, top) = (
                column.at(|corner| corner.base, slot),
                column.at(|corner| corner.top, slot),
            );
            if point.y <= base || point.y >= top {
                continue;
            }
            let within = (point.y - base) / (top - base);
            let grained = bank.grained(slot, deck, point);
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

    /// The weather map's column over `(x, z)`, to blend; `None` off the bank.
    fn column(&self, x: f64, z: f64) -> Option<Blend> {
        let ((west, right), (south, lower)) = self.bank.weather_cell(x, z)?;
        Some(Blend {
            corners: corners(&self.bank.weather, (west, south)),
            right,
            lower,
        })
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
        self.grid_at(&self.bank.light, point)
    }

    /// What of the sunlight the bank above lets through to `point`: all of
    /// it with none above.
    fn overhead_at(&self, point: Vec3) -> f64 {
        if self.bank.overhead.is_empty() {
            1.0
        } else {
            self.grid_at(&self.bank.overhead, point)
        }
    }

    /// `grid`, laid out as the light grid's points are, read trilinearly at
    /// `point`.
    fn grid_at(&self, grid: &[f32], point: Vec3) -> f64 {
        let bank = self.bank;
        let (across, up) = bank.gridding;
        let east = ((point.x - bank.centre.0 + bank.half) * across)
            .clamp(0.0, real(LIGHT_SIDE - 1) - 1e-6);
        let north = ((point.z - bank.centre.1 + bank.half) * across)
            .clamp(0.0, real(LIGHT_SIDE - 1) - 1e-6);
        let high = ((point.y - bank.floor) * up).clamp(0.0, real(LIGHT_LAYERS - 1) - 1e-6);
        let ((column, right), (row, lower), (layer, rise)) =
            (cell_of(east), cell_of(north), cell_of(high));
        let at = |column: usize, row: usize, layer: usize| {
            f64::from(
                grid.get((layer * LIGHT_SIDE + row) * LIGHT_SIDE + column)
                    .copied()
                    .unwrap_or(0.0),
            )
        };
        let plane = |layer: usize| {
            bilinear(
                [
                    at(column, row, layer),
                    at(column + 1, row, layer),
                    at(column, row + 1, layer),
                    at(column + 1, row + 1, layer),
                ],
                (right, lower),
            )
        };
        let (beneath, above) = (plane(layer), plane((layer + 1).min(LIGHT_LAYERS - 1)));
        beneath + (above - beneath) * rise
    }

    /// Where the light grid's point `(i, j, k)` lies.
    fn light_point(&self, i: usize, j: usize, k: usize) -> Vec3 {
        let bank = self.bank;
        let across = 2.0 * bank.half / real(LIGHT_SIDE - 1);
        let up = (bank.ceiling - bank.floor) / real(LIGHT_LAYERS - 1);
        Vec3::new(
            bank.centre.0 - bank.half + real(i) * across,
            bank.floor + real(k) * up,
            bank.centre.1 - bank.half + real(j) * across,
        )
    }

    /// The optical depth of the cloud from `point` toward the sun, out of
    /// the bank, in `steps` steps.
    fn toward_sun(&self, point: Vec3, steps: u32) -> f64 {
        let bank = self.bank;
        let sun = bank.sun;
        let Some((_, leave)) = bank.slab(point, sun) else {
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

/// A deck's column at `(x, z)`: how much it covers, and its base and top.
fn weather(deck: &Deck, x: f64, z: f64) -> (f64, f64, f64) {
    let (cos, sin) = (mathf::cos(deck.heading), mathf::sin(deck.heading));
    let along = (x * cos + z * sin) / (deck.scale * deck.stretch);
    let across = (-x * sin + z * cos) / deck.scale;
    // Pushed about by a slower field, so the cover gathers in drifts.
    let (u, v) = (
        along + 0.5 * noise2(along * 0.3, across * 0.3, deck.seed ^ 0x51),
        across + 0.5 * noise2(along * 0.3 + 3.1, across * 0.3 + 1.7, deck.seed ^ 0x93),
    );
    let field = fbm2(u, v, deck.seed, (5, 0.55, 2.02));
    let drift = fbm2(u * 0.23, v * 0.23, deck.seed ^ 0x77, (3, 0.5, 2.0));
    // How much of the sky a column holds: the deck's cover on the whole, more
    // or less where the drifts gather it.
    let cover = (deck.cover + 0.55 * field + 0.35 * drift).clamp(0.0, 1.0);
    let drift = 0.5 + 0.5 * drift;
    let lift = noise2(x / 5_000.0, z / 5_000.0, deck.seed ^ 0xba5e);
    let base = deck.base + deck.base_spread * lift;
    // Deeper where the cover is thickest: the heaps grow tall at the middle
    // of a field of cloud.
    let grown = smoothstep(0.2, 1.0, cover);
    let depth = deck.depth.0 + (deck.depth.1 - deck.depth.0) * grown * (0.6 + 0.4 * drift);
    (cover, base, base + depth)
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

/// The weather map's columns at the corners of the cell whose south-west
/// corner is `(column, row)`: south-west, south-east, north-west, north-east.
fn corners(weather: &[Column], (column, row): (usize, usize)) -> [Column; 4] {
    let at = |column: usize, row: usize| {
        weather
            .get(row * WEATHER_SIDE + column)
            .copied()
            .unwrap_or_default()
    };
    [
        at(column, row),
        at(column + 1, row),
        at(column, row + 1),
        at(column + 1, row + 1),
    ]
}

/// For each of `decks`, the heights between which its cloud over the
/// weather cell whose south-west corner is `(column, row)` can stand, its
/// billows reaching no higher than `peak`: bilinear between the corners, its
/// cover, base and top lie within theirs. Rounded out to whole metres, which
/// single precision holds exactly; empty where no cloud can stand.
fn band_over(
    decks: &[Option<Deck>; 2],
    weather: &[Column],
    (column, row): (usize, usize),
    peak: f64,
) -> [(f32, f32); 2] {
    let corners = corners(weather, (column, row));
    let mut bands = [(f32::INFINITY, f32::NEG_INFINITY); 2];
    for ((slot, deck), band) in decks.iter().enumerate().zip(&mut bands) {
        let Some(deck) = deck else {
            continue;
        };
        let values =
            |field: fn(&Column) -> [f32; 2]| corners.map(|corner| f64::from(field(&corner)[slot]));
        let greatest = |values: [f64; 4]| values.into_iter().fold(f64::NEG_INFINITY, f64::max);
        let Some(within) = highest(greatest(values(|corner| corner.cover)), deck.heap, peak) else {
            continue;
        };
        let bases = values(|corner| corner.base);
        let lowest = bases.into_iter().fold(f64::INFINITY, f64::min);
        let high = greatest(bases) + (greatest(values(|corner| corner.top)) - lowest) * within;
        *band = (single(mathf::floor(lowest)), single(mathf::ceil(high)));
    }
    bands
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

/// The blend of a square grid `side` a side at `(u, v)`, in cells.
fn bilinear2(values: &[f32], side: usize, (u, v): (f64, f64)) -> f64 {
    let ((i, fu), (j, fv)) = (cell_of(u), cell_of(v));
    let at = |i: usize, j: usize| f64::from(values.get(j * side + i).copied().unwrap_or(1.0));
    bilinear(
        [at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)],
        (fu, fv),
    )
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
