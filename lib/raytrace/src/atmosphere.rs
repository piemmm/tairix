//! The atmosphere: sunlight scattered by air, haze and ozone over a round
//! Earth, so the sky's colour, the sun's own colour low down, the glow after
//! sunset, the Earth's shadow and the blue of distant hills all come of the
//! same physics.
//!
//! After Hillaire ("A Scalable and Production Ready Sky and Atmosphere
//! Rendering Technique", EGSR 2020): the transmittance from any height along
//! any direction to space, the light multiple scattering adds, the sky seen
//! from the scene's eye, and the light the air between the eye and what it
//! sees scatters toward it, each tabulated once per scene. A ray then reads
//! them back in a few lookups, whatever the hour. Distances inside the model
//! are kilometres; the scene's metres enter and leave through its base
//! height above the sea.
//!
//! The sun's light is bent on its way down (`refraction`): what reaches a
//! height is kept along the bent path from its true direction, spread as the
//! path squashes the disc, and scattered about the way it arrives, so the
//! Earth's shadow and a low sun's colour come of the air as it is. The same
//! paths say where a ray seen leaving a point comes from, which lifts and
//! squashes a disc near the horizon, each channel a little apart from the
//! others.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, PI, TAU};
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::band;
use crate::refraction::{refractivity, Refraction};
use crate::vector::{cell_of, real, single, singles, Vec3};

/// The Earth's radius, and the top of its atmosphere, in kilometres.
pub(crate) const GROUND: f64 = 6360.0;
pub(crate) const TOP: f64 = 6460.0;
/// The atmosphere's depth.
const DEPTH: f64 = TOP - GROUND;

/// The red, green and blue channels' wavelengths, in micrometres: those the
/// scattering below is measured at.
pub(crate) const WAVELENGTHS: [f64; 3] = [0.680, 0.550, 0.440];

/// Rayleigh scattering at sea level, per kilometre, and its scale height.
const RAYLEIGH: Vec3 = Vec3::new(5.802e-3, 13.558e-3, 33.1e-3);
const RAYLEIGH_HEIGHT: f64 = 8.0;
/// Haze's scattering and absorption at sea level on a clear day, and its
/// scale height.
const MIE_SCATTER: f64 = 3.996e-3;
const MIE_ABSORB: f64 = 0.444e-3;
const MIE_HEIGHT: f64 = 1.2;
/// How much haze throws light forward.
const MIE_G: f64 = 0.8;
/// Ozone's absorption at the peak of its layer, which is 30 km thick about
/// 25 km up.
const OZONE: Vec3 = Vec3::new(0.650e-3, 1.881e-3, 0.085e-3);

/// The table sizes: the bent paths' directions and heights, the
/// multiple-scattering table's, the sky's azimuths and elevations, and the
/// aerial table's azimuths, elevations and slices of distance.
const PATHS: (usize, usize) = (256, 48);
const MULTIPLE: (usize, usize) = (24, 24);
const VIEW: (usize, usize) = (96, 128);
const AERIAL: (usize, usize, usize) = (32, 48, 88);

/// Steps taken along a sky or aerial ray.
const VIEW_STEPS: u32 = 30;
const MULTIPLE_STEPS: u32 = 16;
/// Directions the multiple-scattering table integrates over.
const MULTIPLE_DIRECTIONS: u32 = 64;

/// How far the aerial table reaches, in kilometres. Its slices lie as the
/// square of their number, the first 32 within 60 km where the land is
/// fine; the rest carry it on to the highest cirrus seen at the horizon.
/// Past it the farthest slice stands.
const AERIAL_REACH: f64 = 453.75;
const _: () = assert!(453_750 * 32 * 32 == 60_000 * AERIAL.2 * AERIAL.2);

/// What the atmosphere is like and where the sun stands in it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Air {
    /// The unit direction toward the sun, which may lie below the horizon.
    pub(crate) sun: Vec3,
    /// The sun's irradiance above the air.
    pub(crate) solar: Vec3,
    /// The scene's level `y = 0` above the sea, in metres.
    pub(crate) base: f64,
    /// Haze, as a multiple of a clear day's: `1.0` clear, `4.0` hazy.
    pub(crate) haze: f64,
    /// The ground's reflectance, which the air's light also bounces off.
    pub(crate) albedo: Vec3,
    /// Where the eye stands in the scene.
    pub(crate) eye: Vec3,
}

impl Air {
    /// The scene's level's distance from the Earth's centre, in metres.
    pub(crate) fn ground(&self) -> f64 {
        GROUND * 1000.0 + self.base
    }
}

/// A table of colours over the unit square, read back bilinearly.
#[derive(Clone, Debug)]
struct Table {
    columns: usize,
    rows: usize,
    cells: Vec<[f32; 3]>,
}

impl Table {
    fn new((columns, rows): (usize, usize)) -> Option<Self> {
        Some(Self {
            columns,
            rows,
            cells: fallible::filled(columns.checked_mul(rows)?, [0.0; 3])?,
        })
    }

    fn at(&self, (u, v): (f64, f64)) -> Vec3 {
        let (x, fx) = split(u, self.columns);
        let (y, fy) = split(v, self.rows);
        let cell = |column: usize, row: usize| {
            self.cells
                .get(row * self.columns + column)
                .map_or(Vec3::ZERO, |&[r, g, b]| {
                    Vec3::new(f64::from(r), f64::from(g), f64::from(b))
                })
        };
        let (right, below) = ((x + 1).min(self.columns - 1), (y + 1).min(self.rows - 1));
        let top = cell(x, y).lerp(cell(right, y), fx);
        let bottom = cell(x, below).lerp(cell(right, below), fx);
        top.lerp(bottom, fy)
    }
}

/// `at` in `0.0..=1.0` as a cell of `count` and how far toward the next.
fn split(at: f64, count: usize) -> (usize, f64) {
    let (index, along) = cell_of(at.clamp(0.0, 1.0) * real(count.saturating_sub(1)));
    (index.min(count.saturating_sub(1)), along)
}

/// The centre of cell `index` of `count` across the unit interval.
fn centre(index: usize, count: usize) -> f64 {
    real(index) / real(count.saturating_sub(1).max(1))
}

/// The air at `height` kilometres up: its scattering by molecules and by
/// haze, and all it takes out of a beam.
#[derive(Copy, Clone, Debug)]
struct Medium {
    rayleigh: Vec3,
    mie: f64,
    extinction: Vec3,
}

/// The atmosphere of one scene: its air, the sun in it, and its tables.
#[derive(Clone, Debug)]
pub(crate) struct Atmosphere {
    air: Air,
    refraction: Refraction,
    /// Each channel's bending, against the green's.
    dispersion: Vec3,
    paths: Paths,
    /// The cosine and sine of the most any channel of a ray leaving the
    /// scene is bent.
    bending: (f64, f64),
    multiple: Table,
    view: Table,
    /// For each of the eye's directions, the light scattered toward it and
    /// the light kept, one slice of distance after another.
    aerial: Vec<Slice>,
    /// The sky's mean radiance over the upper hemisphere, once solved.
    ambient: Vec3,
    stage: Stage,
}

/// How far the tables are built.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    Paths(usize),
    Multiple(usize),
    View(usize),
    Aerial(usize),
    Done,
}

/// Rows of a table built in one unit of work by each core.
const UNIT_ROWS: usize = 1;

/// Light from a true direction as it reaches a point through the bending air.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Arriving {
    /// The unit direction it arrives from.
    pub(crate) dir: Vec3,
    /// The solid angle it arrives over, for each it set out in: under one
    /// near the horizon, where the air squashes what is seen there.
    pub(crate) stretch: f64,
    /// What the air keeps of it.
    pub(crate) kept: Vec3,
    pub(crate) bend: Bend,
}

/// A ray leaving a point, traced back out through the bending air.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Leaving {
    /// The unit true direction the green light it meets comes from.
    pub(crate) toward: Vec3,
    /// What the air keeps of that light.
    pub(crate) kept: Vec3,
    /// The solid angle that light is seen over, for each it truly fills.
    pub(crate) stretch: f64,
    pub(crate) bend: Bend,
}

/// How the air bends the green light seen along a ray: the cosine and sine
/// of the zenith angle it is seen at, and how much further from the zenith
/// it truly comes from.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Bend {
    seen: (f64, f64),
    by: f64,
}

impl Bend {
    /// No bend, for the unit `dir` under a room's walls.
    pub(crate) fn none(dir: Vec3) -> Self {
        Self {
            seen: (dir.y, mathf::hypot(dir.x, dir.z)),
            by: 0.0,
        }
    }
}

impl Atmosphere {
    /// The atmosphere of `air`, its tables still to build; `None` when the
    /// heap will not hold them.
    pub(crate) fn new(air: Air) -> Option<Self> {
        let (azimuths, elevations, slices) = AERIAL;
        let [red, green, blue] = WAVELENGTHS.map(refractivity);
        Some(Self {
            air,
            refraction: Refraction::new(green),
            dispersion: Vec3::new(red / green, 1.0, blue / green),
            paths: Paths::new()?,
            bending: (1.0, 0.0),
            multiple: Table::new(MULTIPLE)?,
            view: Table::new(VIEW)?,
            aerial: fallible::filled(
                azimuths * elevations * slices,
                Slice::stored(Scattered::NONE),
            )?,
            ambient: Vec3::ZERO,
            stage: Stage::Paths(0),
        })
    }

    /// The air the tables describe.
    pub(crate) const fn air(&self) -> &Air {
        &self.air
    }

    /// Stand the eye the sky and the air before it are seen from at `eye`,
    /// before the tables are built.
    pub(crate) fn place_eye(&mut self, eye: Vec3) {
        self.air.eye = eye;
    }

    /// Build the next unit of the tables across `runner`; whether all are
    /// built.
    pub(crate) fn step(&mut self, runner: &dyn JobRunner) -> bool {
        let unit = UNIT_ROWS * runner.width().max(1);
        match self.stage {
            Stage::Paths(row) => {
                let end = (row + unit).min(PATHS.1);
                let (air, refraction) = (self.air, &self.refraction);
                self.paths.fill(row..end, runner, &|row, slot| {
                    path_row(&air, refraction, row, slot);
                });
                if end >= PATHS.1 {
                    let most = self.paths.most_bent() * self.dispersion.max_element();
                    self.bending = (mathf::cos(most), mathf::sin(most));
                    self.stage = Stage::Multiple(0);
                } else {
                    self.stage = Stage::Paths(end);
                }
            }
            Stage::Multiple(row) => {
                let end = (row + unit).min(MULTIPLE.1);
                let (air, paths) = (self.air, &self.paths);
                fill_rows(&mut self.multiple, row..end, runner, &|u, v| {
                    multiple_texel(&air, paths, u, v)
                });
                self.stage = if end >= MULTIPLE.1 {
                    Stage::View(0)
                } else {
                    Stage::Multiple(end)
                };
            }
            Stage::View(row) => {
                let end = (row + unit).min(VIEW.1);
                let reader = Reader {
                    air: &self.air,
                    paths: &self.paths,
                    multiple: &self.multiple,
                };
                fill_rows(&mut self.view, row..end, runner, &|u, v| {
                    reader.view_texel(u, v)
                });
                if end >= VIEW.1 {
                    self.ambient = self.hemisphere();
                    self.stage = Stage::Aerial(0);
                } else {
                    self.stage = Stage::View(end);
                }
            }
            Stage::Aerial(row) => {
                let end = (row + unit).min(AERIAL.1);
                self.fill_aerial(row..end, runner);
                self.stage = if end >= AERIAL.1 {
                    Stage::Done
                } else {
                    Stage::Aerial(end)
                };
            }
            Stage::Done => {}
        }
        self.stage == Stage::Done
    }

    /// The units of building its tables done and to do, each a core's rows.
    pub(crate) fn units(&self) -> (f64, f64) {
        let tables = [PATHS.1, MULTIPLE.1, VIEW.1, AERIAL.1];
        let (finished, row) = match self.stage {
            Stage::Paths(row) => (0, row),
            Stage::Multiple(row) => (1, row),
            Stage::View(row) => (2, row),
            Stage::Aerial(row) => (3, row),
            Stage::Done => (tables.len(), 0),
        };
        let units = |rows: usize| real(rows) / real(UNIT_ROWS);
        let total = tables.iter().copied().map(units).sum();
        let done = tables
            .iter()
            .take(finished)
            .copied()
            .map(units)
            .sum::<f64>()
            + units(row);
        (done, total)
    }

    /// Fill the aerial table's elevation rows `rows` across `runner`.
    fn fill_aerial(&mut self, rows: Range<usize>, runner: &dyn JobRunner) {
        let (azimuths, _, slices) = AERIAL;
        let per_row = azimuths * slices;
        let reader = Reader {
            air: &self.air,
            paths: &self.paths,
            multiple: &self.multiple,
        };
        let Some(cells) = self
            .aerial
            .get_mut(rows.start * per_row..rows.end * per_row)
        else {
            return;
        };
        band::for_each(runner, cells, (rows.start, per_row), &|row, band| {
            let v = centre(row, AERIAL.1);
            for (column, slices) in band.chunks_mut(AERIAL.2).enumerate() {
                let u = centre(column, azimuths);
                reader.aerial_column(direction(u, v, &reader.air.sun), slices);
            }
        });
    }

    /// The mean radiance of the sky's upper hemisphere, cosine-weighted.
    fn hemisphere(&self) -> Vec3 {
        let mut total = Vec3::ZERO;
        let mut weight = 0.0;
        for row in 0..VIEW.1 {
            let v = centre(row, VIEW.1);
            let elevation = elevation_of(v);
            if elevation <= 0.0 {
                continue;
            }
            // The cosine toward the zenith, times the solid angle of a texel:
            // a ring shrinks with the cosine of its elevation, and rows packed
            // as the square of the elevation span `2v - 1` of it apiece.
            let texel = mathf::sin(elevation) * mathf::cos(elevation) * (2.0 * v - 1.0);
            for column in 0..VIEW.0 {
                let u = centre(column, VIEW.0);
                total += self.view.at((u, v)) * texel;
                weight += texel;
            }
        }
        if weight > 0.0 {
            total / weight
        } else {
            Vec3::ZERO
        }
    }

    /// The sky's radiance toward the unit `dir` seen from the eye: the air's
    /// light alone, without the sun's disc or any cloud.
    pub(crate) fn sky(&self, dir: Vec3) -> Vec3 {
        let (u, v) = parametrise(dir, &self.air.sun);
        self.view.at((u, v))
    }

    /// The sky's mean radiance over its upper half.
    pub(crate) const fn ambient(&self) -> Vec3 {
        self.ambient
    }

    /// How much of the light from a true direction whose angle from the
    /// vertical has `cosine` reaches a point `height` metres above the
    /// scene's level, square to the way it arrives: kept along its bent
    /// path, and spread over the more sky or the less the air turns it into;
    /// nothing where the Earth stands between.
    pub(crate) fn sunlight(&self, height: f64, cosine: f64) -> Vec3 {
        self.paths
            .bent(radius(&self.air, height), cosine.clamp(-1.0, 1.0))
            .map_or(Vec3::ZERO, |bent| bent.lit())
    }

    /// How the light from the true direction `toward` arrives at a point
    /// `height` metres above the scene's level; `None` where the Earth
    /// stands between.
    pub(crate) fn arriving(&self, height: f64, toward: Vec3) -> Option<Arriving> {
        let bent = self
            .paths
            .bent(radius(&self.air, height), toward.y.clamp(-1.0, 1.0))?;
        let seen = bent.seen();
        Some(Arriving {
            dir: turned(toward, seen),
            stretch: bent.stretch,
            kept: bent.kept,
            bend: Bend { seen, by: bent.by },
        })
    }

    /// Where a ray leaving a point `height` metres above the scene's level
    /// along the unit `dir` comes from; `None` where it meets the ground.
    pub(crate) fn leaving(&self, height: f64, dir: Vec3) -> Option<Leaving> {
        let mu = dir.y.clamp(-1.0, 1.0);
        let way = self.paths.leaving(radius(&self.air, height), mu)?;
        let seen = (mu, mathf::hypot(dir.x, dir.z));
        let truly = tilt(seen, way.bend);
        // The solid angles' ratio, `sin z dz` against its source's; toward
        // the zenith the sines' ratio runs to the slope's inverse.
        let squeeze = if truly.1 > 1e-12 {
            seen.1 / truly.1
        } else {
            1.0 / way.slope
        };
        Some(Leaving {
            toward: turned(dir, truly),
            kept: (-way.depth).exp(),
            stretch: squeeze / way.slope,
            bend: Bend { seen, by: way.bend },
        })
    }

    /// The true directions each channel of the light seen along the unit
    /// `dir` comes from, the air bending each its own way past `bend`; the
    /// green's is `green`.
    pub(crate) fn sources(&self, dir: Vec3, bend: Bend, green: Vec3) -> [Vec3; 3] {
        let source = |share: f64| turned(dir, tilt(bend.seen, bend.by * share));
        [source(self.dispersion.x), green, source(self.dispersion.z)]
    }

    /// The cosine and sine of the most the air bends any channel of a ray
    /// leaving the scene.
    pub(crate) const fn bending(&self) -> (f64, f64) {
        self.bending
    }

    /// What a ray from the eye along the unit `dir` shows of `light` met
    /// `distance` metres away, once the air between has dimmed it and added
    /// its own: `lit` how much of the sun's light and of the sky's reaches
    /// that air.
    pub(crate) fn aerial(&self, dir: Vec3, distance: f64, light: Vec3, lit: Lit) -> Vec3 {
        let between = self.between(dir, distance);
        light * between.kept + between.lit_by(lit)
    }

    /// The light the air scatters toward the eye along `dir` over its first
    /// `distance` metres, and how much of what lies beyond it passes.
    pub(crate) fn between(&self, dir: Vec3, distance: f64) -> Scattered {
        self.sight(dir).between(distance)
    }

    /// The air seen from the eye along `dir`, to be read at any distance
    /// along it.
    pub(crate) fn sight(&self, dir: Vec3) -> Sight<'_> {
        let (azimuths, elevations, slices) = AERIAL;
        let (u, v) = parametrise(dir, &self.air.sun);
        let (column, east) = split(u, azimuths);
        let (row, north) = split(v, elevations);
        let (right, below) = (
            (column + 1).min(azimuths - 1),
            (row + 1).min(elevations - 1),
        );
        let start = |column: usize, row: usize| (row * azimuths + column) * slices;
        Sight {
            aerial: &self.aerial,
            starts: [
                start(column, row),
                start(right, row),
                start(column, below),
                start(right, below),
            ],
            across: (east, north),
        }
    }
}

/// The air seen from the eye along one direction: the four columns of the
/// aerial table about it — by where each one's slices start, south-west,
/// south-east, north-west, north-east — and how far east and north across
/// them it lies.
pub(crate) struct Sight<'a> {
    aerial: &'a [Slice],
    starts: [usize; 4],
    across: (f64, f64),
}

impl Sight<'_> {
    /// The light the air scatters toward the eye over the first `distance`
    /// metres, and how much of what lies beyond passes.
    pub(crate) fn between(&self, distance: f64) -> Scattered {
        self.blended(
            place_of(distance),
            Scattered::NONE,
            Slice::read,
            Scattered::lerp,
        )
    }

    /// Where between `from` and `to` metres the share `u` of the sun's
    /// green light the air scatters toward the eye over that stretch has
    /// been gathered: drawn with `u` even in `0.0..1.0`, a point falls along
    /// the stretch as its sunlit air's light does.
    pub(crate) fn drawn(&self, (from, to): (f64, f64), u: f64) -> f64 {
        let green = |place: f64| {
            self.blended(
                place,
                0.0,
                |slice| f64::from(slice.sun[1]),
                |a, b, t| a + (b - a) * t,
            )
        };
        let (start, end) = (place_of(from), place_of(to));
        let (low, high) = (green(start), green(end));
        // A stretch whose air gathers no sunlight holds no point it favours.
        if high <= low || high.is_nan() || low.is_nan() {
            return from + (to - from) * u;
        }
        let target = low + (high - low) * u.clamp(0.0, 1.0);
        // The light gathered runs straight between whole places: find the
        // first whole place that reaches the target, then the point within.
        let (mut least, mut most, mut reached) = (mathf::ceil(start), mathf::floor(end), None);
        while least <= most {
            let middle = mathf::floor(f64::midpoint(least, most));
            if green(middle) >= target {
                reached = Some(middle);
                most = middle - 1.0;
            } else {
                least = middle + 1.0;
            }
        }
        let (near, far) = reached.map_or((mathf::floor(end).max(start), end), |whole| {
            ((whole - 1.0).max(start), whole)
        });
        let (before, after) = (green(near), green(far));
        let place = if after > before {
            near + (far - near) * ((target - before) / (after - before)).clamp(0.0, 1.0)
        } else {
            near
        };
        let depth = (place + 1.0) / real(AERIAL.2);
        (depth * depth * AERIAL_REACH * 1000.0).clamp(from, to)
    }

    /// What `read` takes of the columns `place` slices in, blended between
    /// the slices about it and across the columns by `lerp`: before the
    /// first slice, from `none`, the eye's nothing.
    fn blended<T: Copy>(
        &self,
        place: f64,
        none: T,
        read: impl Fn(&Slice) -> T,
        lerp: impl Fn(T, T, f64) -> T,
    ) -> T {
        let last = AERIAL.2.saturating_sub(1);
        let column = |start: usize| {
            let at = |slice: usize| self.aerial.get(start + slice).map_or(none, &read);
            if place < 0.0 {
                return lerp(none, at(0), (place + 1.0).clamp(0.0, 1.0));
            }
            let (index, along) = cell_of(place);
            lerp(
                at(index.min(last)),
                at((index + 1).min(last)),
                along.clamp(0.0, 1.0),
            )
        };
        let [a, b, c, d] = self.starts.map(column);
        let (east, north) = self.across;
        lerp(lerp(a, b, east), lerp(c, d, east), north)
    }
}

/// Where `distance` metres lies among the aerial table's slices: before the
/// first at `-1.0`, at the last at one short of their count.
fn place_of(distance: f64) -> f64 {
    mathf::sqrt((distance / 1000.0 / AERIAL_REACH).clamp(0.0, 1.0)) * real(AERIAL.2) - 1.0
}

/// The light the air scatters toward the eye over a stretch of a ray — the
/// sun's own, scattered once, and the clear sky's — how much of the stretch
/// scatters whatever lights it from every way, and what of the light beyond
/// the stretch crosses it.
///
/// They are kept apart because what stands in the sun's way shadows the one
/// and not the other — the air in a wood's shadow still sees the sky — and a
/// cloud overhead stands in for the clear sky with its own light.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Scattered {
    pub(crate) sun: Vec3,
    pub(crate) sky: Vec3,
    /// The stretch's scattering depth, each step's as the light beyond it
    /// reaches the eye: what light the same from every way scatters toward
    /// the eye, per unit of it.
    pub(crate) scatter: Vec3,
    pub(crate) kept: Vec3,
}

impl Scattered {
    /// No air at all: nothing scattered, everything kept.
    pub(crate) const NONE: Self = Self {
        sun: Vec3::ZERO,
        sky: Vec3::ZERO,
        scatter: Vec3::ZERO,
        kept: Vec3::ONE,
    };

    /// All the light the stretch scatters toward the eye.
    pub(crate) fn light(&self) -> Vec3 {
        self.sun + self.sky
    }

    fn lerp(self, other: Self, t: f64) -> Self {
        Self {
            sun: self.sun.lerp(other.sun, t),
            sky: self.sky.lerp(other.sky, t),
            scatter: self.scatter.lerp(other.scatter, t),
            kept: self.kept.lerp(other.kept, t),
        }
    }

    /// The light the stretch scatters toward the eye as `lit` lights it: the
    /// sun as far as it reaches, the clear sky as far as the clouds leave it
    /// open, the clouds' own light where they stand overhead, the last two
    /// as far as crowns roofing the air let any of the sky reach it.
    pub(crate) fn lit_by(&self, lit: Lit) -> Vec3 {
        self.sun * lit.sun + (self.sky * lit.open + self.scatter * lit.glow) * lit.sky
    }
}

/// How much of the sun's light, and of the sky's, reaches the air along a
/// ray: the sun's shadowed by what stands in its way, the sky's by the
/// crowns roofing it and the clouds overhead, which light it with their own.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Lit {
    pub(crate) sun: Vec3,
    pub(crate) sky: f64,
    /// How much of the clear sky overhead the clouds leave open.
    pub(crate) open: f64,
    /// The light the clouds overhead shed down on the air.
    pub(crate) glow: Vec3,
}

#[cfg(test)]
impl Lit {
    /// Air under an open, clear sky in the sun.
    pub(crate) const OPEN: Self = Self {
        sun: Vec3::ONE,
        sky: 1.0,
        open: 1.0,
        glow: Vec3::ZERO,
    };
}

/// A slice of the aerial table, held in single precision.
#[derive(Copy, Clone, Debug)]
struct Slice {
    sun: [f32; 3],
    sky: [f32; 3],
    scatter: [f32; 3],
    kept: [f32; 3],
}

impl Slice {
    fn stored(scattered: Scattered) -> Self {
        Self {
            sun: singles(scattered.sun),
            sky: singles(scattered.sky),
            scatter: singles(scattered.scatter),
            kept: singles(scattered.kept),
        }
    }

    fn read(&self) -> Scattered {
        let widen = |[r, g, b]: [f32; 3]| Vec3::new(f64::from(r), f64::from(g), f64::from(b));
        Scattered {
            sun: widen(self.sun),
            sky: widen(self.sky),
            scatter: widen(self.scatter),
            kept: widen(self.kept),
        }
    }
}

/// Fill rows `rows` of `table` with `texel` at each cell's centre, spread
/// over `runner`.
fn fill_rows(
    table: &mut Table,
    rows: Range<usize>,
    runner: &dyn JobRunner,
    texel: &(dyn Fn(f64, f64) -> Vec3 + Sync),
) {
    let (columns, height) = (table.columns, table.rows);
    let (start, end) = (rows.start.min(height), rows.end.min(height));
    let Some(cells) = table.cells.get_mut(start * columns..end * columns) else {
        return;
    };
    band::for_each(runner, cells, (start, columns), &|row, cells| {
        let v = centre(row, height);
        for (column, cell) in cells.iter_mut().enumerate() {
            *cell = singles(texel(centre(column, columns), v));
        }
    });
}

/// What finished tables read from.
struct Reader<'a> {
    air: &'a Air,
    paths: &'a Paths,
    multiple: &'a Table,
}

impl Reader<'_> {
    /// The light multiple scattering adds at radius `r` for a sun whose
    /// cosine to the local vertical is `mu_sun`, per unit of scattering.
    fn multiple(&self, r: f64, mu_sun: f64) -> Vec3 {
        let v = ((r - GROUND) / DEPTH).clamp(0.0, 1.0);
        self.multiple.at((0.5 + 0.5 * mu_sun, v))
    }

    /// The sky's texel `(u, v)`: the light gathered along the eye's ray in
    /// that direction out to the air's edge or the ground.
    fn view_texel(&self, u: f64, v: f64) -> Vec3 {
        let dir = direction(u, v, &self.air.sun);
        let eye = eye_radius(self.air);
        let reach = path_length(eye, dir.y);
        self.march_span(dir, (0.0, reach), VIEW_STEPS).light()
    }

    /// Fill one direction's column of aerial slices.
    fn aerial_column(&self, dir: Vec3, slices: &mut [Slice]) {
        let count = slices.len();
        let eye = eye_radius(self.air);
        let limit = path_length(eye, dir.y);
        let mut gathered = Scattered::NONE;
        let mut reached = 0.0;
        for (index, slot) in slices.iter_mut().enumerate() {
            let fraction = real(index + 1) / real(count.max(1));
            let target = (fraction * fraction * AERIAL_REACH).min(limit);
            if target > reached {
                let span = self.march_span(dir, (reached, target), 2);
                gathered = Scattered {
                    sun: gathered.sun + span.sun * gathered.kept,
                    sky: gathered.sky + span.sky * gathered.kept,
                    scatter: gathered.scatter + span.scatter * gathered.kept,
                    kept: gathered.kept * span.kept,
                };
                reached = target;
            }
            *slot = Slice::stored(gathered);
        }
    }

    /// The light scattered toward the eye along `dir` between `from` and `to`
    /// kilometres, as seen from `from`, and what passes the span.
    fn march_span(&self, dir: Vec3, (from, to): (f64, f64), steps: u32) -> Scattered {
        let sun = self.air.sun;
        let cos = dir.dot(sun);
        let eye = Vec3::new(0.0, eye_radius(self.air), 0.0);
        let step = (to - from) / f64::from(steps.max(1));
        let mut gathered = Scattered::NONE;
        for index in 0..steps {
            let t = from + (f64::from(index) + 0.5) * step;
            let point = eye + dir * t;
            let r = point.length();
            let up = point / r;
            let medium = medium(r - GROUND, self.air.haze);
            let sun_cos = up.dot(sun);
            // The sun's light arrives bent toward the vertical, and scatters
            // about the way it arrives.
            let single = self.paths.bent(r, sun_cos).map_or(Vec3::ZERO, |bent| {
                let cos = bent.toward(cos, up.dot(dir));
                (medium.rayleigh * rayleigh_phase(cos)
                    + Vec3::splat(medium.mie * mie_phase(cos, MIE_G)))
                    * bent.lit()
            });
            let multiple = self.multiple(r, sun_cos);
            let scattering = medium.rayleigh + Vec3::splat(medium.mie);
            let through = (medium.extinction * -step).exp();
            let kept = gathered.kept;
            gathered.sun += kept * integrated(single * self.air.solar, medium.extinction, through);
            gathered.sky += kept
                * integrated(
                    scattering * multiple * self.air.solar,
                    medium.extinction,
                    through,
                );
            gathered.scatter += kept * integrated(scattering, medium.extinction, through);
            gathered.kept = kept * through;
        }
        gathered
    }
}

/// The light a step adds of `source` over a stretch that keeps `through` of
/// what crosses it, integrated exactly over the stretch.
fn integrated(source: Vec3, extinction: Vec3, through: Vec3) -> Vec3 {
    let channel = |s: f64, e: f64, kept: f64| {
        if e > 1e-12 {
            s * (1.0 - kept) / e
        } else {
            0.0
        }
    };
    Vec3::new(
        channel(source.x, extinction.x, through.x),
        channel(source.y, extinction.y, through.y),
        channel(source.z, extinction.z, through.z),
    )
}

/// The distance from the Earth's centre of a point `height` metres above
/// `air`'s scene's level, in kilometres, never below the ground.
fn radius(air: &Air, height: f64) -> f64 {
    ((air.ground() + height) / 1000.0).max(GROUND)
}

/// The eye's distance from the Earth's centre: at least a metre above the
/// ground, which the sky's table splits at the eye's own horizon.
fn eye_radius(air: &Air) -> f64 {
    radius(air, air.eye.y).max(GROUND + 1e-3)
}

/// The air `height` kilometres up.
fn medium(height: f64, haze: f64) -> Medium {
    let height = height.max(0.0);
    let rayleigh = RAYLEIGH * mathf::exp(-height / RAYLEIGH_HEIGHT);
    let haze_density = mathf::exp(-height / MIE_HEIGHT) * haze;
    let ozone = (1.0 - (height - 25.0).abs() / 15.0).max(0.0);
    Medium {
        rayleigh,
        mie: MIE_SCATTER * haze_density,
        extinction: rayleigh
            + Vec3::splat((MIE_SCATTER + MIE_ABSORB) * haze_density)
            + OZONE * ozone,
    }
}

/// How far a ray from radius `r` whose cosine to the vertical is `mu` runs
/// before it meets the ground or leaves the air.
fn path_length(r: f64, mu: f64) -> f64 {
    ground_distance(r, mu).unwrap_or_else(|| top_distance(r, mu))
}

/// Where a ray from radius `r` at `mu` meets the ground, if it does.
fn ground_distance(r: f64, mu: f64) -> Option<f64> {
    let disc = r * r * (mu * mu - 1.0) + GROUND * GROUND;
    (mu < 0.0 && disc >= 0.0).then(|| (-r * mu - mathf::sqrt(disc)).max(0.0))
}

/// Where a ray from radius `r` at `mu` leaves the air.
fn top_distance(r: f64, mu: f64) -> f64 {
    let disc = r * r * (mu * mu - 1.0) + TOP * TOP;
    (-r * mu + mathf::sqrt(disc.max(0.0))).max(0.0)
}

/// One bent path out of the air, at one end's zenith angle: what it keeps,
/// how far its other end's zenith angle lies past this one's, and how fast
/// that changes with this one.
#[derive(Copy, Clone, Debug, Default)]
struct Way {
    depth: [f32; 3],
    bend: f32,
    slope: f32,
}

impl Way {
    const NONE: Self = Self {
        depth: [0.0; 3],
        bend: 0.0,
        slope: 0.0,
    };
}

/// A path read back, blended between the ways about it.
#[derive(Copy, Clone, Debug)]
struct Read {
    depth: Vec3,
    bend: f64,
    slope: f64,
}

impl Read {
    fn of(way: Way) -> Self {
        let [r, g, b] = way.depth.map(f64::from);
        Self {
            depth: Vec3::new(r, g, b),
            bend: f64::from(way.bend),
            slope: f64::from(way.slope),
        }
    }

    fn lerp(self, other: Self, t: f64) -> Self {
        Self {
            depth: self.depth.lerp(other.depth, t),
            bend: self.bend + (other.bend - self.bend) * t,
            slope: self.slope + (other.slope - self.slope) * t,
        }
    }

    /// The light reaching along this way from a true zenith cosine `mu`.
    fn bent(&self, mu: f64) -> Bent {
        let sin = mathf::sqrt(((1.0 - mu) * (1.0 + mu)).max(0.0));
        let (sin_bend, cos_bend) = (mathf::sin(self.bend), mathf::cos(self.bend));
        // The solid angles' ratio, `sin z dz` against its source's; toward
        // the zenith the sines' ratio runs to the slope itself.
        let squeeze = if sin > 1e-12 {
            cos_bend - mu * sin_bend / sin
        } else {
            self.slope
        };
        Bent {
            kept: (-self.depth).exp(),
            stretch: squeeze * self.slope,
            truly: (mu, sin),
            by: self.bend,
            bend: (cos_bend, sin_bend),
        }
    }
}

/// Light from a true direction reaching a point along a bent way.
#[derive(Copy, Clone, Debug)]
struct Bent {
    /// What of it the air keeps, and the solid angle it arrives over for
    /// each it set out in.
    kept: Vec3,
    stretch: f64,
    /// The cosine and sine of its true zenith angle; how much nearer the
    /// zenith it arrives, and that angle's cosine and sine.
    truly: (f64, f64),
    by: f64,
    bend: (f64, f64),
}

impl Bent {
    /// Its irradiance square to the way it arrives, as a share of what set
    /// out: what the air keeps, spread as the air squashes it.
    fn lit(&self) -> Vec3 {
        self.kept * self.stretch
    }

    /// The cosine and sine of the zenith angle it arrives at.
    fn seen(&self) -> (f64, f64) {
        let ((mu, sin), (cos_bend, sin_bend)) = (self.truly, self.bend);
        (
            mu * cos_bend + sin * sin_bend,
            sin * cos_bend - mu * sin_bend,
        )
    }

    /// Its cosine, as it arrives, to a ray whose cosine to its true
    /// direction is `cos` and to the vertical is `rise`.
    fn toward(&self, cos: f64, rise: f64) -> f64 {
        let ((mu, sin), (cos_bend, sin_bend)) = (self.truly, self.bend);
        if sin <= 1e-12 {
            return cos;
        }
        cos * cos_bend + sin_bend * (rise - cos * mu) / sin
    }
}

/// One height's bent paths: the rays leaving it, by their own zenith angle,
/// and the light reaching it, by the true zenith angle it comes from; and
/// the lowest zenith cosine of each, below which the ground stands.
///
/// Each runs from the way that grazes the ground, the lowest any path there
/// reaches, up to the zenith, packed as the square of the way up so the
/// horizon, where the air bends and dims most, is held finest.
#[derive(Clone, Debug)]
struct Row {
    leaving: [Way; PATHS.0],
    reaching: [Way; PATHS.0],
    lowest: (f64, f64),
}

/// The bent paths out of the air from every height.
#[derive(Clone, Debug)]
struct Paths {
    rows: Vec<Row>,
}

impl Paths {
    fn new() -> Option<Self> {
        let empty = Row {
            leaving: [Way::NONE; PATHS.0],
            reaching: [Way::NONE; PATHS.0],
            lowest: (1.0, 1.0),
        };
        Some(Self {
            rows: fallible::filled(PATHS.1, empty)?,
        })
    }

    /// Fill rows `rows` across `runner`, each as `fill` has it.
    fn fill(
        &mut self,
        rows: Range<usize>,
        runner: &dyn JobRunner,
        fill: &(dyn Fn(usize, &mut Row) + Sync),
    ) {
        let end = rows.end.min(PATHS.1);
        let Some(slots) = self.rows.get_mut(rows.start..end) else {
            return;
        };
        band::for_each(runner, slots, (rows.start, 1), &|row, slots| {
            for slot in slots {
                fill(row, slot);
            }
        });
    }

    /// The most a way leaving any height is bent.
    fn most_bent(&self) -> f64 {
        self.rows
            .iter()
            .flat_map(|row| &row.leaving)
            .map(|way| f64::from(way.bend))
            .fold(0.0, f64::max)
    }

    /// The ray leaving radius `r` at zenith cosine `mu`; `None` below the
    /// lowest, which meets the ground.
    fn leaving(&self, r: f64, mu: f64) -> Option<Read> {
        self.read(r, mu, false)
    }

    /// The light reaching radius `r` from a true zenith cosine `mu`; `None`
    /// below the lowest, which the Earth stands in the way of.
    fn bent(&self, r: f64, mu: f64) -> Option<Bent> {
        self.read(r, mu, true).map(|way| way.bent(mu))
    }

    /// What of the light from a true zenith cosine `mu` reaches radius `r`,
    /// square to the way it arrives.
    fn sunlit(&self, r: f64, mu: f64) -> Vec3 {
        self.bent(r, mu).map_or(Vec3::ZERO, |bent| bent.lit())
    }

    /// The way at radius `r` and zenith cosine `mu` in the rows' halves for
    /// light reaching where `reaching`, else for rays leaving, blended
    /// between the two rows about `r`.
    fn read(&self, r: f64, mu: f64, reaching: bool) -> Option<Read> {
        let (columns, rows) = PATHS;
        let (row, across) = split(mathf::sqrt(((r - GROUND) / DEPTH).clamp(0.0, 1.0)), rows);
        let (here, there) = (self.rows.get(row)?, self.rows.get((row + 1).min(rows - 1))?);
        let lowest = |row: &Row| if reaching { row.lowest.1 } else { row.lowest.0 };
        let (low, high) = (lowest(here), lowest(there));
        if mu < low + (high - low) * across {
            return None;
        }
        let along = |row: &Row, lowest: f64| {
            let up = mathf::sqrt(((mu - lowest) / (1.0 - lowest).max(1e-12)).clamp(0.0, 1.0));
            let (column, rise) = split(up, columns);
            let ways = if reaching {
                &row.reaching
            } else {
                &row.leaving
            };
            let at = |column: usize| Read::of(ways.get(column).copied().unwrap_or_default());
            at(column).lerp(at((column + 1).min(columns - 1)), rise)
        };
        Some(along(here, low).lerp(along(there, high), across))
    }
}

/// How far from the Earth's centre the bent paths' row `row` lies.
fn row_radius(row: usize) -> f64 {
    let v = centre(row, PATHS.1);
    GROUND + v * v * DEPTH
}

/// The zenith cosine at radius `r` of the ray that just grazes the ground.
fn grazing(refraction: &Refraction, r: f64) -> f64 {
    let level = refraction.index(0.0) * GROUND / (refraction.index(r - GROUND) * r);
    -mathf::sqrt((1.0 - level * level).max(0.0))
}

/// Fill `slot`, row `row` of the bent paths: trace its rays out from the
/// grazing one to the zenith, then turn them about for the light reaching
/// it from each true direction.
fn path_row(air: &Air, refraction: &Refraction, row: usize, slot: &mut Row) {
    let (columns, _) = PATHS;
    let r = row_radius(row);
    let lowest = grazing(refraction, r);
    let extinction = |height: f64| medium(height, air.haze).extinction;
    let mut seen = [0.0; PATHS.0];
    let mut true_zenith = [0.0; PATHS.0];
    let mut depth = [Vec3::ZERO; PATHS.0];
    for column in 0..columns {
        let up = centre(column, columns);
        let mu = lowest + (1.0 - lowest) * up * up;
        let zenith = mathf::acos(mu.clamp(-1.0, 1.0));
        let path = refraction
            .trace((r, mu), (GROUND, TOP), &extinction)
            .unwrap_or(crate::refraction::Path {
                zenith,
                depth: Vec3::splat(f64::from(f32::MAX)),
            });
        seen[column] = zenith;
        true_zenith[column] = path.zenith;
        depth[column] = path.depth;
    }
    // How fast the true zenith angle turns with the seen one, from either
    // neighbour, or the one there is at an end.
    let slope = |column: usize| {
        let (before, after) = (column.saturating_sub(1), (column + 1).min(columns - 1));
        let (from, to) = (
            seen[before] - seen[after],
            true_zenith[before] - true_zenith[after],
        );
        if from.abs() > 1e-15 {
            to / from
        } else {
            1.0
        }
    };
    for (column, way) in slot.leaving.iter_mut().enumerate() {
        *way = Way {
            depth: singles(depth[column]),
            bend: single(true_zenith[column] - seen[column]),
            slope: single(slope(column)),
        };
    }
    // The light reaching the row comes from no lower than its grazing ray
    // leaves toward: its true direction, as stored.
    let lowest_true = mathf::cos(seen[0] + f64::from(single(true_zenith[0] - seen[0])));
    slot.lowest = (lowest, lowest_true);
    let mut ray = 0;
    for (column, way) in slot.reaching.iter_mut().enumerate() {
        let up = centre(column, columns);
        let target = mathf::acos((lowest_true + (1.0 - lowest_true) * up * up).clamp(-1.0, 1.0));
        // The true zenith angles fall as the rays rise: find the two the
        // target lies between.
        while ray + 1 < columns - 1 && true_zenith[ray + 1] > target {
            ray += 1;
        }
        let next = (ray + 1).min(columns - 1);
        let span = true_zenith[ray] - true_zenith[next];
        let t = if span > 1e-15 {
            ((true_zenith[ray] - target) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let at = seen[ray] + (seen[next] - seen[ray]) * t;
        let turning = slope(ray) + (slope(next) - slope(ray)) * t;
        *way = Way {
            depth: singles(depth[ray].lerp(depth[next], t)),
            bend: single(target - at),
            slope: single(if turning > 1e-12 { 1.0 / turning } else { 1.0 }),
        };
    }
}

/// The unit `dir` turned in its own vertical plane to stand at the zenith
/// angle whose cosine and sine are `(cos, sin)`; as it is when it stands
/// straight up or down.
fn turned(dir: Vec3, (cos, sin): (f64, f64)) -> Vec3 {
    let level = mathf::hypot(dir.x, dir.z);
    if level < 1e-12 {
        return dir;
    }
    let across = sin / level;
    Vec3::new(dir.x * across, cos, dir.z * across)
}

/// The cosine and sine of the zenith angle whose own are `(cos, sin)`, `by`
/// further from the zenith.
fn tilt((cos, sin): (f64, f64), by: f64) -> (f64, f64) {
    let (sin_by, cos_by) = (mathf::sin(by), mathf::cos(by));
    (cos * cos_by - sin * sin_by, sin * cos_by + cos * sin_by)
}

/// The multiple-scattering table's texel `(u, v)`: at the height `v` names
/// with the sun at the cosine `u` names, what scattering again and again
/// adds per unit of scattering, for a unit sun (Hillaire, section 5.5).
fn multiple_texel(air: &Air, paths: &Paths, u: f64, v: f64) -> Vec3 {
    let mu_sun = 2.0 * u - 1.0;
    let r = GROUND + v * DEPTH + 1e-3;
    let sun = Vec3::new(mathf::sqrt((1.0 - mu_sun * mu_sun).max(0.0)), mu_sun, 0.0);
    let origin = Vec3::new(0.0, r, 0.0);
    let isotropic = 1.0 / (4.0 * PI);
    let (mut second, mut transfer) = (Vec3::ZERO, Vec3::ZERO);
    for index in 0..MULTIPLE_DIRECTIONS {
        // Directions spread evenly over the sphere by the golden spiral.
        let rise = 1.0 - 2.0 * (f64::from(index) + 0.5) / f64::from(MULTIPLE_DIRECTIONS);
        let around = f64::from(index) * crate::sample::GOLDEN_ANGLE;
        let level = mathf::sqrt((1.0 - rise * rise).max(0.0));
        let dir = Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around));
        let ground = ground_distance(r, dir.y);
        let length = ground.unwrap_or_else(|| top_distance(r, dir.y));
        let step = length / f64::from(MULTIPLE_STEPS);
        let mut kept = Vec3::ONE;
        for sample in 0..MULTIPLE_STEPS {
            let t = (f64::from(sample) + 0.5) * step;
            let point = origin + dir * t;
            let radius = point.length();
            let medium = medium(radius - GROUND, air.haze);
            let scattering = medium.rayleigh + Vec3::splat(medium.mie);
            let sunlit = paths.sunlit(radius, (point / radius).dot(sun));
            let through = (medium.extinction * -step).exp();
            let gathered = integrated(scattering, medium.extinction, through) * kept;
            second += gathered * sunlit * isotropic;
            transfer += gathered;
            kept = kept * through;
        }
        if let Some(distance) = ground {
            let up = (origin + dir * distance).normalized();
            let lit = paths
                .bent(GROUND, up.dot(sun))
                .map_or(Vec3::ZERO, |bent| bent.lit() * bent.seen().0.max(0.0));
            second += kept * lit * air.albedo * (1.0 / PI);
        }
    }
    // Means over the sphere: the isotropic phase at the texel and the sphere's
    // solid angle cancel.
    let mean = 1.0 / f64::from(MULTIPLE_DIRECTIONS);
    let (second, transfer) = (second * mean, transfer * mean);
    Vec3::new(
        every_order(second.x, transfer.x),
        every_order(second.y, transfer.y),
        every_order(second.z, transfer.z),
    )
}

/// Hillaire's sum of every order of scattering past the first, from the
/// second order's light and the share of each order's light the next
/// passes on: held short of the whole where a medium scatters nearly all it
/// takes and keeps none.
pub(crate) fn every_order(second: f64, transfer: f64) -> f64 {
    second / (1.0 - transfer.min(0.999))
}

/// Rayleigh's phase function.
fn rayleigh_phase(cos: f64) -> f64 {
    3.0 / (16.0 * PI) * (1.0 + cos * cos)
}

/// Cornette and Shanks' phase function for haze, of asymmetry `g`.
fn mie_phase(cos: f64, g: f64) -> f64 {
    let g2 = g * g;
    let base = (1.0 + g2 - 2.0 * g * cos).max(1e-9);
    3.0 / (8.0 * PI) * ((1.0 - g2) * (1.0 + cos * cos)) / ((2.0 + g2) * base * mathf::sqrt(base))
}

/// The elevation a sky texel's `v` stands for: packed toward the horizon,
/// where the sky changes fastest.
fn elevation_of(v: f64) -> f64 {
    let signed = 2.0 * v - 1.0;
    signed.signum() * signed * signed * FRAC_PI_2
}

/// The unit direction a sky texel `(u, v)` stands for: `u` the azimuth away
/// from the sun's, the sky being the same either side of it.
fn direction(u: f64, v: f64, sun: &Vec3) -> Vec3 {
    let elevation = elevation_of(v);
    let azimuth = u * PI + mathf::atan2(sun.z, sun.x);
    let level = mathf::cos(elevation);
    Vec3::new(
        level * mathf::cos(azimuth),
        mathf::sin(elevation),
        level * mathf::sin(azimuth),
    )
}

/// The sky texel the unit `dir` falls in.
fn parametrise(dir: Vec3, sun: &Vec3) -> (f64, f64) {
    let elevation = mathf::asin(dir.y.clamp(-1.0, 1.0));
    let mut azimuth = mathf::atan2(dir.z, dir.x) - mathf::atan2(sun.z, sun.x);
    azimuth -= TAU * mathf::floor(azimuth / TAU);
    if azimuth > PI {
        azimuth = TAU - azimuth;
    }
    let packed = mathf::sqrt((elevation.abs() / FRAC_PI_2).min(1.0));
    (azimuth / PI, 0.5 + 0.5 * elevation.signum() * packed)
}

#[cfg(test)]
#[path = "atmosphere_tests.rs"]
mod tests;
