//! Volumetric cloud: heaped cumulus, a broken sheet of stratocumulus, or a
//! field of altocumulus, marched through as the medium it is.
//!
//! A cloudbank is up to two decks of cloud over a square tens of kilometres
//! across. Each deck has a weather map — how much of each column it covers,
//! and where its base and top lie there, so no two clouds sit at the same
//! height — and its density within a column is a vertical profile, flat
//! beneath and heaped above, times a billowing noise, eroded at its edges by
//! a finer one (Schneider, "The Real-time Volumetric Cloudscapes of Horizon:
//! Zero Dawn", 2015). Both noises are Perlin–Worley textures that tile, built
//! once per scene, so a sample reads them rather than evaluating them.
//!
//! A ray gathers what the cloud scatters toward it step by step, each step's
//! light integrated exactly over the step (Hillaire, "Physically Based Sky,
//! Atmosphere and Cloud Rendering in Frostbite", 2016). The sun's light at a
//! step is dimmed by the cloud toward the sun — a grid of that optical depth
//! built once, and two short taps for the billows' own shadows — and spread
//! by several scattering octaves, each lighter and broader than the last, for
//! the brightness multiple scattering gives (Wrenninge, Kulla and Lundqvist,
//! "Oz: The Great and Volumetric", 2013). Each height's sunlight has already
//! crossed the air, so a deck still above the Earth's shadow at dusk is lit
//! red from beneath while the ground below lies in blue shade.

use alloc::vec::Vec;
use core::f64::consts::PI;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::band;
use crate::lanes::Corners;
use crate::noise::{fbm2, hash3, noise2, smoothstep};
use crate::sample::{mix32, unit};
use crate::shape::reciprocal;
use crate::vector::{real, Vec3};

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
    /// The breadth of the billows its clouds are made of.
    pub(crate) billow: f64,
    pub(crate) seed: u32,
}

/// Texels along each edge of the billowing noise, and of the finer one.
const SHAPE_SIDE: usize = 64;
const DETAIL_SIDE: usize = 32;
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
/// The most clear columns a ray jumps: as many walls of the weather map as a
/// straight line across it can cross.
const MOST_JUMPS: usize = 2 * WEATHER_SIDE;
const OPAQUE: f64 = 0.015;
/// A step through a deck, as a share of its billows, seen directly and seen
/// otherwise: fine enough that no edge shows where a step fell. Clear air
/// between clouds is stepped through as finely, since a stride through it
/// would step over a cloud's thinner edges.
const FINE_SHARE: f64 = 0.06;
const COARSE_SHARE: f64 = 0.25;
/// How far off a step has doubled in length, as a pixel's view of the cloud
/// has widened.
const STRETCH: f64 = 20_000.0;

/// A texture of noise that repeats along each axis.
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

    /// The texture read trilinearly at `p`, in texels, wrapping.
    fn at(&self, p: Vec3) -> f64 {
        let side = self.side;
        let wrap = |value: f64| {
            let whole = mathf::floor(value);
            let index = cell(whole - mathf::floor(whole / real(side)) * real(side));
            (index.min(side - 1), value - whole)
        };
        let ((x, fx), (y, fy), (z, fz)) = (wrap(p.x), wrap(p.y), wrap(p.z));
        let (x1, y1, z1) = ((x + 1) % side, (y + 1) % side, (z + 1) % side);
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
    /// The unit direction toward the sun that lights it.
    sun: Vec3,
    shape: Tile,
    detail: Tile,
    weather: Vec<Column>,
    /// The optical depth toward the sun from each point of a coarse grid.
    light: Vec<f32>,
    /// What of the sun's light the bank lets through to each point of the
    /// ground beneath it.
    shadow: Vec<f32>,
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
        if floor >= ceiling {
            return None;
        }
        Some(Self {
            decks,
            centre,
            half,
            floor: floor.max(0.0),
            ceiling,
            sun,
            shape: Tile::new(SHAPE_SIDE)?,
            detail: Tile::new(DETAIL_SIDE)?,
            weather: fallible::filled(WEATHER_SIDE * WEATHER_SIDE, Column::default())?,
            light: fallible::filled(LIGHT_SIDE * LIGHT_SIDE * LIGHT_LAYERS, 0.0)?,
            shadow: fallible::filled(SHADOW_SIDE * SHADOW_SIDE, 1.0)?,
            lighting: None,
            stage: Stage::Shape(0),
        })
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

    /// Build the next unit of the bank across `runner`; whether it is built,
    /// or `None` when the heap will not hold it.
    pub(crate) fn step(&mut self, runner: &dyn JobRunner) -> Option<bool> {
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
                    self.stage = Stage::Light(0);
                } else {
                    self.stage = Stage::Weather(end);
                }
            }
            Stage::Light(row) => {
                let rows = LIGHT_LAYERS * LIGHT_SIDE;
                let end = (row + LIGHT_ROWS * width).min(rows);
                self.fill_light(row..end, runner)?;
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
                    Stage::Done
                } else {
                    Stage::Shadow(end)
                };
            }
            Stage::Done => {}
        }
        Some(self.stage == Stage::Done)
    }

    /// The weather map's rows `rows`.
    fn fill_weather(&mut self, rows: Range<usize>, runner: &dyn JobRunner) {
        let (decks, centre, half) = (self.decks, self.centre, self.half);
        let step = 2.0 * half / real(WEATHER_SIDE - 1);
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
                for (slot, deck) in decks.iter().enumerate() {
                    if let Some(deck) = deck {
                        let (cover, base, top) = weather(deck, x, z);
                        cell.cover[slot] = narrow(cover);
                        cell.base[slot] = narrow(base);
                        cell.top[slot] = narrow(top);
                    }
                }
            }
        });
    }

    /// The light grid's rows `rows`, counted up through its layers: at each
    /// point, the cloud's optical depth toward the sun; `None` when the heap
    /// will not hold them.
    fn fill_light(&mut self, rows: Range<usize>, runner: &dyn JobRunner) -> Option<()> {
        let reader = Reader { bank: self };
        let mut depths = fallible::filled(rows.len() * LIGHT_SIDE, 0.0f32)?;
        band::for_each(
            runner,
            &mut depths,
            (rows.start, LIGHT_SIDE),
            &|row, band| {
                let (layer, across) = (row / LIGHT_SIDE, row % LIGHT_SIDE);
                for (column, slot) in band.iter_mut().enumerate() {
                    let point = reader.light_point(column, across, layer);
                    *slot = narrow(reader.toward_sun(point, LIGHT_STEPS));
                }
            },
        );
        self.light
            .get_mut(rows.start * LIGHT_SIDE..rows.end * LIGHT_SIDE)?
            .copy_from_slice(&depths);
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
                    *slot = narrow(mathf::exp(-depth));
                }
            },
        );
        self.shadow
            .get_mut(rows.start * SHADOW_SIDE..rows.end * SHADOW_SIDE)?
            .copy_from_slice(&values);
        Some(())
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
    /// staggers the steps so no two samples of a pixel band alike.
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
        let mut t = enter + within(enter, 0) * jitter;
        let mut kept = 1.0;
        let mut light = Vec3::ZERO;
        let mut depth = 0.0;
        let mut weight = 0.0;
        let mut steps = 0;
        let mut jumps = 0_usize;
        while t < leave && steps < most {
            let point = origin + dir * t;
            let step = within(t, steps + 1);
            let Some(sample) = reader.density(point, fine) else {
                // A column no deck covers is crossed in one jump, which spends
                // none of the steps the cloud beyond it is owed.
                jumps += 1;
                if jumps > MOST_JUMPS {
                    break;
                }
                t = self.past_column(origin, dir, t).max(t + 1e-3 * step);
                continue;
            };
            steps += 1;
            if sample.density <= 1e-4 {
                t += step;
                continue;
            }
            let extinction = sample.density * sample.thickness;
            let optical = reader.optical_depth(point, fine, sample.billow);
            let height = ((point.y - self.floor) / (self.ceiling - self.floor)).clamp(0.0, 1.0);
            let sun = sunlight_at(lighting, height);
            let mut scattered = Vec3::ZERO;
            for (octave, phase) in phases.iter().enumerate() {
                let (a, b) = OCTAVES[octave];
                scattered += sun * (b * phase * mathf::exp(-a * optical));
            }
            // Powdered edges: a cloud's rim facing the sun is darker than its
            // body, where light has scattered in from all sides.
            let powder = 1.0 - 0.6 * mathf::exp(-2.2 * optical);
            let low = 1.0 - sample.within;
            let ambient =
                lighting.above.lerp(lighting.below, low * low) * (0.35 + 0.65 * sample.within);
            // An evenly lit sky scatters in its whole light, the phase
            // function summing to one over the sphere.
            let source = (scattered * powder + ambient) * extinction;
            let through = mathf::exp(-extinction * step);
            let gained = (1.0 - through) / extinction.max(1e-12);
            light += source * (kept * gained);
            let lost = kept * (1.0 - through);
            depth += t * lost;
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

    /// Where the ray from `origin` along `dir`, at `t` along it, next crosses
    /// a line of the weather map's grid, and so leaves the column it is in.
    fn past_column(&self, origin: Vec3, dir: Vec3, t: f64) -> f64 {
        let size = 2.0 * self.half / real(WEATHER_SIDE - 1);
        let at = origin + dir * t;
        let across = |position: f64, d: f64, centre: f64| {
            let low = centre - self.half;
            let index = mathf::floor((position - low) / size);
            let wall = if d > 1e-12 {
                low + (index + 1.0) * size
            } else if d < -1e-12 {
                low + index * size
            } else {
                return f64::INFINITY;
            };
            t + (wall - position) / d
        };
        across(at.x, dir.x, self.centre.0).min(across(at.z, dir.z, self.centre.1))
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
    let count = lighting.sunlight.len();
    if count == 0 {
        return Vec3::ZERO;
    }
    let place = height.clamp(0.0, 1.0) * real(count - 1);
    let whole = mathf::floor(place);
    let index = cell(whole).min(count - 1);
    let next = (index + 1).min(count - 1);
    let (a, b) = (
        lighting.sunlight.get(index).copied().unwrap_or(Vec3::ZERO),
        lighting.sunlight.get(next).copied().unwrap_or(Vec3::ZERO),
    );
    a.lerp(b, place - whole)
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
}

/// A bank read at trace time.
struct Reader<'a> {
    bank: &'a Cloudbank,
}

impl Reader<'_> {
    /// The cloud at `point`; `None` in a column no deck covers, which a ray
    /// may stride across.
    fn density(&self, point: Vec3, fine: bool) -> Option<Sample> {
        let bank = self.bank;
        let column = self.column(point.x, point.z)?;
        let mut best: Option<Sample> = None;
        let mut any = false;
        for (slot, deck) in bank.decks.iter().enumerate() {
            let Some(deck) = deck else {
                continue;
            };
            let cover = f64::from(column.cover[slot]);
            if cover <= 0.0 {
                continue;
            }
            any = true;
            let (base, top) = (f64::from(column.base[slot]), f64::from(column.top[slot]));
            if point.y <= base || point.y >= top {
                continue;
            }
            let within = (point.y - base) / (top - base);
            let shape = bank
                .shape
                .at(point * (real(SHAPE_SIDE) / (deck.billow * 8.0)));
            let mut density = remap(shape, threshold(cover, within, deck.heap), 1.0)
                * smoothstep(0.0, 0.04, within);
            if density <= 0.0 {
                continue;
            }
            if fine {
                let wisp = bank
                    .detail
                    .at(point * (real(DETAIL_SIDE) / (deck.billow * 8.0 * DETAIL_SCALE)));
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
                });
            }
        }
        if !any {
            return None;
        }
        Some(best.unwrap_or(Sample {
            density: 0.0,
            thickness: 0.0,
            within: 0.0,
            billow: 1.0,
        }))
    }

    /// The weather map's column over `(x, z)`, blended; `None` off the bank.
    fn column(&self, x: f64, z: f64) -> Option<Column> {
        let bank = self.bank;
        let scale = real(WEATHER_SIDE - 1) / (2.0 * bank.half);
        let (across, down) = (
            (x - bank.centre.0 + bank.half) * scale,
            (z - bank.centre.1 + bank.half) * scale,
        );
        let limit = real(WEATHER_SIDE - 1);
        if !(0.0..limit).contains(&across) || !(0.0..limit).contains(&down) {
            return None;
        }
        let (west, south) = (mathf::floor(across), mathf::floor(down));
        let (right, lower) = (across - west, down - south);
        let (west, south) = (cell(west), cell(south));
        let at = |column: usize, row: usize| {
            bank.weather
                .get(row * WEATHER_SIDE + column)
                .copied()
                .unwrap_or_default()
        };
        let corners = [
            at(west, south),
            at(west + 1, south),
            at(west, south + 1),
            at(west + 1, south + 1),
        ];
        let blend = |field: fn(&Column) -> [f32; 2]| {
            let [south_west, south_east, north_west, north_east] =
                corners.map(|corner| field(&corner).map(f64::from));
            core::array::from_fn(|slot| {
                let top = south_west[slot] + (south_east[slot] - south_west[slot]) * right;
                let bottom = north_west[slot] + (north_east[slot] - north_west[slot]) * right;
                narrow(top + (bottom - top) * lower)
            })
        };
        Some(Column {
            cover: blend(|column| column.cover),
            base: blend(|column| column.base),
            top: blend(|column| column.top),
        })
    }

    /// The optical depth toward the sun from `point`: the grid's, and for a
    /// fine sample two short taps through its own billows.
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

    /// The light grid read trilinearly at `point`.
    fn light_at(&self, point: Vec3) -> f64 {
        let bank = self.bank;
        let across = real(LIGHT_SIDE - 1) / (2.0 * bank.half);
        let up = real(LIGHT_LAYERS - 1) / (bank.ceiling - bank.floor);
        let east = ((point.x - bank.centre.0 + bank.half) * across)
            .clamp(0.0, real(LIGHT_SIDE - 1) - 1e-6);
        let north = ((point.z - bank.centre.1 + bank.half) * across)
            .clamp(0.0, real(LIGHT_SIDE - 1) - 1e-6);
        let high = ((point.y - bank.floor) * up).clamp(0.0, real(LIGHT_LAYERS - 1) - 1e-6);
        let (column, row, layer) = (mathf::floor(east), mathf::floor(north), mathf::floor(high));
        let (right, lower, rise) = (east - column, north - row, high - layer);
        let (column, row, layer) = (cell(column), cell(row), cell(layer));
        let at = |column: usize, row: usize, layer: usize| {
            f64::from(
                bank.light
                    .get((layer * LIGHT_SIDE + row) * LIGHT_SIDE + column)
                    .copied()
                    .unwrap_or(0.0),
            )
        };
        let plane = |layer: usize| {
            let top = at(column, row, layer)
                + (at(column + 1, row, layer) - at(column, row, layer)) * right;
            let bottom = at(column, row + 1, layer)
                + (at(column + 1, row + 1, layer) - at(column, row + 1, layer)) * right;
            top + (bottom - top) * lower
        };
        let (beneath, overhead) = (plane(layer), plane((layer + 1).min(LIGHT_LAYERS - 1)));
        beneath + (overhead - beneath) * rise
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
        let sun = if bank.sun.y > 0.02 {
            bank.sun
        } else {
            Vec3::new(bank.sun.x, 0.02, bank.sun.z).normalized()
        };
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
    let rise = within * within * (1.5 - 0.5 * heap);
    (1.0 - cover) + cover * rise
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
    let (cu, cv) = (mathf::floor(u), mathf::floor(v));
    let (fu, fv) = (u - cu, v - cv);
    let (i, j) = (cell(cu), cell(cv));
    let at = |i: usize, j: usize| f64::from(values.get(j * side + i).copied().unwrap_or(1.0));
    let top = at(i, j) + (at(i + 1, j) - at(i, j)) * fu;
    let bottom = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * fu;
    top + (bottom - top) * fv
}

/// A whole, non-negative float as an index; nought below it.
fn cell(whole: f64) -> usize {
    usize::try_from(mathf::round_i32(whole.max(0.0))).unwrap_or(0)
}

fn narrow(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the bank's grids hold single precision"
    )]
    {
        value as f32
    }
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
