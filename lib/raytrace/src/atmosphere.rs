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

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, PI, TAU};
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::band;
use crate::vector::{real, Vec3};

/// The Earth's radius, and the top of its atmosphere, in kilometres.
const GROUND: f64 = 6360.0;
const TOP: f64 = 6460.0;
/// The atmosphere's depth.
const DEPTH: f64 = TOP - GROUND;

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

/// The table sizes: the transmittance's heights and directions, the
/// multiple-scattering table's, the sky's azimuths and elevations, and the
/// aerial table's slices of distance.
const TRANSMITTANCE: (usize, usize) = (128, 48);
const MULTIPLE: (usize, usize) = (24, 24);
const VIEW: (usize, usize) = (96, 128);
const AERIAL: (usize, usize, usize) = (32, 48, 32);

/// Steps taken along a path to space, and along a sky or aerial ray.
const TRANSMITTANCE_STEPS: u32 = 40;
const VIEW_STEPS: u32 = 30;
const MULTIPLE_STEPS: u32 = 16;
/// Directions the multiple-scattering table integrates over.
const MULTIPLE_DIRECTIONS: u32 = 64;

/// How far the aerial table reaches, in kilometres: past it the scene is
/// the sky's.
const AERIAL_REACH: f64 = 60.0;

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
    let place = at.clamp(0.0, 1.0) * real(count.saturating_sub(1));
    let whole = mathf::floor(place);
    let index = usize::try_from(mathf::round_i32(whole)).unwrap_or(0);
    (index.min(count.saturating_sub(1)), place - whole)
}

/// The centre of cell `index` of `count` across the unit interval.
fn centre(index: usize, count: usize) -> f64 {
    real(index) / real(count.saturating_sub(1).max(1))
}

fn stored(colour: Vec3) -> [f32; 3] {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the tables hold single precision: light needs no more"
    )]
    [colour.x as f32, colour.y as f32, colour.z as f32]
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
    transmittance: Table,
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
    Transmittance(usize),
    Multiple(usize),
    View(usize),
    Aerial(usize),
    Done,
}

/// Rows of a table built in one unit of work by each core.
const UNIT_ROWS: usize = 1;

impl Atmosphere {
    /// The atmosphere of `air`, its tables still to build; `None` when the
    /// heap will not hold them.
    pub(crate) fn new(air: Air) -> Option<Self> {
        let (azimuths, elevations, slices) = AERIAL;
        Some(Self {
            air,
            transmittance: Table::new(TRANSMITTANCE)?,
            multiple: Table::new(MULTIPLE)?,
            view: Table::new(VIEW)?,
            aerial: fallible::filled(
                azimuths * elevations * slices,
                Slice::stored(Scattered::NONE),
            )?,
            ambient: Vec3::ZERO,
            stage: Stage::Transmittance(0),
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
            Stage::Transmittance(row) => {
                let end = (row + unit).min(TRANSMITTANCE.1);
                let air = self.air;
                fill_rows(&mut self.transmittance, row..end, runner, &|u, v| {
                    transmittance_texel(&air, u, v)
                });
                self.stage = if end >= TRANSMITTANCE.1 {
                    Stage::Multiple(0)
                } else {
                    Stage::Transmittance(end)
                };
            }
            Stage::Multiple(row) => {
                let end = (row + unit).min(MULTIPLE.1);
                let (air, table) = (self.air, &self.transmittance);
                fill_rows(&mut self.multiple, row..end, runner, &|u, v| {
                    multiple_texel(&air, table, u, v)
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
                    transmittance: &self.transmittance,
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

    /// What the finished tables read from: the transmittance and the multiple
    /// scattering, borrowed.
    fn reader(&self) -> Reader<'_> {
        Reader {
            air: &self.air,
            transmittance: &self.transmittance,
            multiple: &self.multiple,
        }
    }

    /// Fill the aerial table's elevation rows `rows` across `runner`.
    fn fill_aerial(&mut self, rows: Range<usize>, runner: &dyn JobRunner) {
        let (azimuths, _, slices) = AERIAL;
        let per_row = azimuths * slices;
        let reader = Reader {
            air: &self.air,
            transmittance: &self.transmittance,
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
            for column in 0..VIEW.0 {
                let u = centre(column, VIEW.0);
                let cos = mathf::sin(elevation);
                total += self.view.at((u, v)) * cos;
                weight += cos;
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

    /// How much of the sun's light reaches a point `height` metres above
    /// the scene's level, along the unit `toward` it: the air's, to space,
    /// and nothing where the Earth stands between.
    pub(crate) fn sunlight(&self, height: f64, toward: Vec3) -> Vec3 {
        let altitude = (self.air.base + height) / 1000.0;
        self.reader()
            .transmittance(GROUND + altitude.max(0.0), toward.y)
    }

    /// What a ray from the eye along the unit `dir` shows of `light` met
    /// `distance` metres away, once the air between has dimmed it and added
    /// its own: `lit` how much of the sun's light and of the sky's reaches
    /// that air.
    pub(crate) fn aerial(&self, dir: Vec3, distance: f64, light: Vec3, lit: Lit) -> Vec3 {
        let between = self.between(dir, distance);
        light * between.kept + between.sun * lit.sun + between.sky * lit.sky
    }

    /// The light the air scatters toward the eye along `dir` over its first
    /// `distance` metres, and how much of what lies beyond it passes.
    pub(crate) fn between(&self, dir: Vec3, distance: f64) -> Scattered {
        let (azimuths, elevations, slices) = AERIAL;
        let (u, v) = parametrise(dir, &self.air.sun);
        let depth = mathf::sqrt((distance / 1000.0 / AERIAL_REACH).clamp(0.0, 1.0));
        let place = depth * real(slices) - 1.0;
        let (column, fx) = split(u, azimuths);
        let (row, fy) = split(v, elevations);
        let right = (column + 1).min(azimuths - 1);
        let below = (row + 1).min(elevations - 1);
        let read = |column: usize, row: usize| self.slice(column, row, place);
        let top = read(column, row).lerp(read(right, row), fx);
        let bottom = read(column, below).lerp(read(right, below), fx);
        top.lerp(bottom, fy)
    }

    /// The aerial table's column at `(column, row)` read `place` slices in,
    /// before the first slice blending from the eye's nothing.
    fn slice(&self, column: usize, row: usize, place: f64) -> Scattered {
        let (azimuths, _, slices) = AERIAL;
        let base = (row * azimuths + column) * slices;
        let read = |slice: usize| {
            self.aerial
                .get(base + slice)
                .map_or(Scattered::NONE, Slice::read)
        };
        if place < 0.0 {
            return Scattered::NONE.lerp(read(0), (place + 1.0).clamp(0.0, 1.0));
        }
        let whole = mathf::floor(place);
        let index = usize::try_from(mathf::round_i32(whole)).unwrap_or(0);
        let last = slices.saturating_sub(1);
        read(index.min(last)).lerp(read((index + 1).min(last)), (place - whole).clamp(0.0, 1.0))
    }
}

/// The light the air scatters toward the eye over a stretch of a ray — the
/// sun's own, scattered once, and the sky's — and what of the light beyond
/// the stretch crosses it.
///
/// The two are kept apart because what stands in the sun's way shadows the
/// one and not the other: the air in a wood's shadow still sees the sky.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Scattered {
    pub(crate) sun: Vec3,
    pub(crate) sky: Vec3,
    pub(crate) kept: Vec3,
}

impl Scattered {
    /// No air at all: nothing scattered, everything kept.
    const NONE: Self = Self {
        sun: Vec3::ZERO,
        sky: Vec3::ZERO,
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
            kept: self.kept.lerp(other.kept, t),
        }
    }
}

/// How much of the sun's light, and of the sky's, reaches the air along a
/// ray: the sun's shadowed by what stands in its way, the sky's by the
/// crowns roofing it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Lit {
    pub(crate) sun: Vec3,
    pub(crate) sky: f64,
}

/// A slice of the aerial table, held in single precision.
#[derive(Copy, Clone, Debug)]
struct Slice {
    sun: [f32; 3],
    sky: [f32; 3],
    kept: [f32; 3],
}

impl Slice {
    fn stored(scattered: Scattered) -> Self {
        Self {
            sun: stored(scattered.sun),
            sky: stored(scattered.sky),
            kept: stored(scattered.kept),
        }
    }

    fn read(&self) -> Scattered {
        let widen = |[r, g, b]: [f32; 3]| Vec3::new(f64::from(r), f64::from(g), f64::from(b));
        Scattered {
            sun: widen(self.sun),
            sky: widen(self.sky),
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
            *cell = stored(texel(centre(column, columns), v));
        }
    });
}

/// What finished tables read from.
struct Reader<'a> {
    air: &'a Air,
    transmittance: &'a Table,
    multiple: &'a Table,
}

impl Reader<'_> {
    /// The light kept from radius `r` along a direction whose cosine to the
    /// local vertical is `mu`, to space; nothing if the ray meets the ground.
    fn transmittance(&self, r: f64, mu: f64) -> Vec3 {
        self.transmittance.at(transmittance_uv(r, mu))
    }

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
        let (phase_r, phase_m) = (rayleigh_phase(cos), mie_phase(cos, MIE_G));
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
            let sunlit = self.transmittance(r, sun_cos);
            let multiple = self.multiple(r, sun_cos);
            let single = (medium.rayleigh * phase_r + Vec3::splat(medium.mie * phase_m)) * sunlit;
            let sky = (medium.rayleigh + Vec3::splat(medium.mie)) * multiple;
            let through = (medium.extinction * -step).exp();
            let kept = gathered.kept;
            gathered.sun += kept * integrated(single * self.air.solar, medium.extinction, through);
            gathered.sky += kept * integrated(sky * self.air.solar, medium.extinction, through);
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

/// The eye's distance from the Earth's centre.
fn eye_radius(air: &Air) -> f64 {
    GROUND + ((air.base + air.eye.y) / 1000.0).max(0.001)
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

/// Where the transmittance from radius `r` at `mu` is kept: the horizon's
/// neighbourhood and the lowest air, where it changes fastest, given most.
fn transmittance_uv(r: f64, mu: f64) -> (f64, f64) {
    let mu = mu.clamp(-1.0, 1.0);
    let u = 0.5 + 0.5 * mu.signum() * mathf::sqrt(mu.abs());
    let v = mathf::sqrt(((r - GROUND) / DEPTH).clamp(0.0, 1.0));
    (u, v)
}

/// The transmittance table's texel `(u, v)`.
fn transmittance_texel(air: &Air, u: f64, v: f64) -> Vec3 {
    let signed = 2.0 * u - 1.0;
    let mu = signed.signum() * signed * signed;
    let r = GROUND + v * v * DEPTH;
    if ground_distance(r, mu).is_some() {
        return Vec3::ZERO;
    }
    let length = top_distance(r, mu);
    let step = length / f64::from(TRANSMITTANCE_STEPS);
    let mut depth = Vec3::ZERO;
    for index in 0..TRANSMITTANCE_STEPS {
        let t = (f64::from(index) + 0.5) * step;
        let height = mathf::sqrt(r * r + t * t + 2.0 * r * mu * t) - GROUND;
        depth += medium(height, air.haze).extinction * step;
    }
    (-depth).exp()
}

/// The multiple-scattering table's texel `(u, v)`: at the height `v` names
/// with the sun at the cosine `u` names, what scattering again and again
/// adds per unit of scattering, for a unit sun (Hillaire, section 5.5).
fn multiple_texel(air: &Air, transmittance: &Table, u: f64, v: f64) -> Vec3 {
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
            let sunlit = transmittance.at(transmittance_uv(radius, (point / radius).dot(sun)));
            let through = (medium.extinction * -step).exp();
            let gathered = integrated(scattering, medium.extinction, through) * kept;
            second += gathered * sunlit * isotropic;
            transfer += gathered;
            kept = kept * through;
        }
        if let Some(distance) = ground {
            let point = origin + dir * distance;
            let up = point.normalized();
            let lit = transmittance.at(transmittance_uv(GROUND, up.dot(sun)));
            second += kept * lit * air.albedo * (up.dot(sun).max(0.0) / PI);
        }
    }
    // Means over the sphere: the isotropic phase at the texel and the sphere's
    // solid angle cancel.
    let mean = 1.0 / f64::from(MULTIPLE_DIRECTIONS);
    let (second, transfer) = (second * mean, transfer * mean);
    let spread = |s: f64, f: f64| s / (1.0 - f.min(0.999));
    Vec3::new(
        spread(second.x, transfer.x),
        spread(second.y, transfer.y),
        spread(second.z, transfer.z),
    )
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
