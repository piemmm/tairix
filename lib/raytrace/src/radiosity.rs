//! Radiosity: the light a diffuse surface gathers from all about it but the
//! lamps — the sky, and every surface the lamps and the sky light, bounce
//! after bounce — found once at records spread over what the picture shows
//! and interpolated between them, rather than traced afresh by every sample
//! (Ward, Rubinstein and Clear, "A Ray Tracing Solution for Diffuse
//! Interreflection", 1988).
//!
//! A record gathers through a hemisphere of rays stratified into rows of
//! equal cosine weight by columns of azimuth, each followed as a path of its
//! own, and keeps how its light changes as the point moves and as the
//! surface turns (Ward and Heckbert, "Irradiance Gradients", 1992). It holds
//! within a radius set by how near the surfaces its rays met lie, and weighs
//! less toward its edge as Tabellion and Lamorlette weigh theirs ("An
//! Approximate Global Illumination System for Computer Generated Films",
//! 2004), so one record gives way to the next without a seam.
//!
//! Records are laid over grids of the picture from coarse to fine before the
//! first pixel is traced, each grid adding them only where the coarser ones
//! do not hold; a point none holds for traces a ray of its own.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::bvh::Bvh;
use crate::sample::mix32;
use crate::scene::Scene;
use crate::shape::Aabb;
use crate::tone::Encoder;
use crate::trace::Tracer;
use crate::vector::{real, share, Frame, Vec3};

/// Rows of elevation a record's hemisphere is cut into, each carrying an
/// equal share of the cosine, by columns of azimuth: one ray a cell.
const ROWS: usize = 8;
const COLUMNS: usize = 32;
pub(crate) const CELLS: usize = ROWS * COLUMNS;

/// A record holds within this share of the harmonic mean distance of what
/// its rays met: Ward's accuracy.
const ACCURACY: f64 = 0.4;

/// The least and the most a record may hold for, as shares of the picture's
/// height where it lies: finer would spend records on changes too small to
/// see, and coarser would reach over changes in the light no ray of its own
/// looked at. Shares, not pixels, so a picture costs the same to gather for
/// at any size.
const NEAREST: f64 = 1.0 / 240.0;
const FARTHEST: f64 = 1.0 / 20.0;

/// The cosine of the widest angle between a record's normal and a point's
/// that the record still holds for: fifteen degrees.
const COS_TURN: f64 = 0.866_025_403_784_438_6;

/// How far behind a record's surface, as a share of its radius, a point may
/// lie and still take its light: farther, and something between them may
/// shade one and not the other.
const BEHIND: f64 = 0.2;

/// The least weight the records at a point must sum to for their light to
/// stand for its own.
const HOLDS: f64 = 0.3;

/// How many rows of sites each grid records are laid over has across the
/// picture's height, coarsest first; the finest as fine as the least radius.
const GRIDS: [u32; 5] = [15, 30, 60, 120, 240];

/// The most records a square of the picture as wide as it is high holds:
/// what bounds the gathering where the light changes everywhere at once;
/// and in a small picture, the pixels there must be for each.
const RECORDS_A_SQUARE: u64 = 1600;
const PIXELS_A_RECORD: u64 = 64;

/// Records one core gathers, and sites it looks over, in a unit of work.
const GATHER_UNIT: usize = 2;
const FIND_UNIT: usize = 64;

/// A prime above any grid's count of sites, which steps through a grid in an
/// order that visits every site once and spreads what a spent budget leaves
/// unvisited evenly over the picture.
const SCRAMBLE: u64 = 2_147_483_647;

/// What one of a record's rays brought back: its light, and how far away the
/// surface it left lies, infinitely for the sky.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Cell {
    pub(crate) light: Vec3,
    pub(crate) distance: f64,
}

impl Cell {
    /// A ray drawn into the surface itself, which brings back nothing.
    pub(crate) const DARK: Self = Self {
        light: Vec3::ZERO,
        distance: f64::INFINITY,
    };
}

/// Where a record may be gathered: where the eye's ray through a pixel's
/// centre first meets a diffuse surface.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Site {
    pub(crate) point: Vec3,
    /// The geometric normal, and the smooth one the record gathers about,
    /// both on the side the eye sees.
    pub(crate) facing: Vec3,
    pub(crate) normal: Vec3,
    /// How far the eye's ray came, and how tall the picture is there.
    pub(crate) travelled: f64,
    pub(crate) span: f64,
    /// What the record's rays are drawn under.
    pub(crate) seed: u32,
}

/// The light gathered at one point.
#[derive(Copy, Clone, Debug)]
struct Record {
    point: Vec3,
    normal: Vec3,
    /// The mean radiance arriving over the cosine-weighted hemisphere: the
    /// irradiance over π.
    light: Vec3,
    radius: f64,
    /// For each channel, how the light changes as the normal turns, about
    /// the axis and by the angle a vector gives, and as the point moves.
    turning: [Vec3; 3],
    moving: [Vec3; 3],
}

/// The direction, about the local `z` axis, of cell `index`'s ray, placed
/// within its cell by `(u, v)`.
pub(crate) fn direction(index: usize, (u, v): (f64, f64)) -> Vec3 {
    let (row, column) = (index / COLUMNS, index % COLUMNS);
    let sin2 = (real(row) + u) / real(ROWS);
    let sin = mathf::sqrt(sin2);
    let cos = mathf::sqrt((1.0 - sin2).max(0.0));
    let azimuth = TAU * (real(column) + v) / real(COLUMNS);
    Vec3::new(sin * mathf::cos(azimuth), sin * mathf::sin(azimuth), cos)
}

/// `weights` per channel of `axis`, added to `gradient`.
fn add(gradient: &mut [Vec3; 3], weights: Vec3, axis: Vec3) {
    for (channel, slot) in gradient.iter_mut().enumerate() {
        *slot += axis * weights.along(channel);
    }
}

impl Record {
    /// The record the rays `cells` brought back to `site`.
    fn new(site: &Site, cells: &[Cell; CELLS]) -> Self {
        let frame = Frame::around(site.normal);
        let (u, v) = (frame.x, frame.y);
        let cell = |row: usize, column: usize| {
            cells
                .get(row * COLUMNS + column % COLUMNS)
                .copied()
                .unwrap_or(Cell::DARK)
        };
        let mut light = Vec3::ZERO;
        let mut nearness = 0.0;
        let mut turning = [Vec3::ZERO; 3];
        let mut moving = [Vec3::ZERO; 3];
        for row in 0..ROWS {
            let (low, high) = (real(row) / real(ROWS), real(row + 1) / real(ROWS));
            let (sin_low, sin_high) = (mathf::sqrt(low), mathf::sqrt(high));
            let (cos_low, cos_high) = (mathf::sqrt(1.0 - low), mathf::sqrt((1.0 - high).max(0.0)));
            // ∫ sin²θ dθ over the row, which the turning of its cells weighs.
            let sweep = 0.5
                * ((mathf::asin(sin_high) - sin_high * cos_high)
                    - (mathf::asin(sin_low) - sin_low * cos_low));
            // What the edge the row shares with the one below, and its walls
            // between columns, carry of the cosine-weighted light as the
            // point moves across them.
            let edge = sin_low * cos_low * cos_low;
            let wall = sin_high - sin_low;
            for column in 0..COLUMNS {
                let start = TAU * real(column) / real(COLUMNS);
                let end = TAU * real(column + 1) / real(COLUMNS);
                let (sin_start, cos_start) = (mathf::sin(start), mathf::cos(start));
                let (sin_end, cos_end) = (mathf::sin(end), mathf::cos(end));
                let here = cell(row, column);
                light += here.light;
                if here.distance.is_finite() {
                    nearness += 1.0 / here.distance;
                }
                // ∫ n × ω over the cell, the normal and ω at right angles.
                let across = (v * (sin_end - sin_start) + u * (cos_end - cos_start)) * sweep;
                add(&mut turning, here.light, across);
                if row > 0 {
                    let below = cell(row - 1, column);
                    let near = here.distance.min(below.distance);
                    if near.is_finite() {
                        let outward = u * (sin_end - sin_start) + v * (cos_start - cos_end);
                        add(
                            &mut moving,
                            here.light - below.light,
                            outward * (edge / near),
                        );
                    }
                }
                let before = cell(row, column + COLUMNS - 1);
                let near = here.distance.min(before.distance);
                if near.is_finite() {
                    let normal_to_wall = v * cos_start - u * sin_start;
                    add(
                        &mut moving,
                        here.light - before.light,
                        normal_to_wall * (wall / near),
                    );
                }
            }
        }
        let light = light * (1.0 / real(CELLS));
        for gradient in turning.iter_mut().chain(moving.iter_mut()) {
            *gradient = *gradient * (1.0 / PI);
        }
        let harmonic = if nearness > 0.0 {
            real(CELLS) / nearness
        } else {
            f64::INFINITY
        };
        let mut radius = ACCURACY * harmonic;
        // Nowhere so far that the light, followed down its slope, would run
        // out.
        let steep = (moving[0] * 0.2126 + moving[1] * 0.7152 + moving[2] * 0.0722).length();
        if steep > 0.0 {
            radius = radius.min(light.luminance() / steep);
        }
        Self {
            point: site.point,
            normal: site.normal,
            light,
            radius: mathf::clamp(radius, NEAREST * site.span, FARTHEST * site.span),
            turning,
            moving,
        }
    }

    /// How much this record weighs at `point`, whose smooth normal is
    /// `normal`; `None` where it does not hold.
    fn weight(&self, point: Vec3, normal: Vec3) -> Option<f64> {
        let cos = normal.dot(self.normal);
        if cos <= COS_TURN {
            return None;
        }
        let offset = point - self.point;
        if offset.dot(normal + self.normal) < -2.0 * BEHIND * self.radius {
            return None;
        }
        let turned = mathf::sqrt((1.0 - cos) / (1.0 - COS_TURN));
        let error = (offset.length() / self.radius).max(turned);
        (error < 1.0).then_some(1.0 - error)
    }

    /// The record's light carried to `point` and turned to face `shading`.
    fn at(&self, point: Vec3, shading: Vec3) -> Vec3 {
        let turn = self.normal.cross(shading);
        let moved = point - self.point;
        let change =
            |channel: usize| self.turning[channel].dot(turn) + self.moving[channel].dot(moved);
        (self.light + Vec3::new(change(0), change(1), change(2))).max(Vec3::ZERO)
    }

    fn bounds(&self) -> Aabb {
        Aabb::around(self.point, self.radius)
    }
}

/// The records of a scene, and the hierarchy they are found through.
#[derive(Debug, Default)]
pub(crate) struct Radiosity {
    records: Vec<Record>,
    bvh: Bvh,
    /// How many of the records the hierarchy holds: the rest were gathered
    /// since it was built.
    indexed: usize,
}

impl Radiosity {
    /// The light the records give a point at `point`, whose smooth normal is
    /// `normal`, turned to face `shading`; `None` where too little of them
    /// holds.
    pub(crate) fn light(&self, point: Vec3, normal: Vec3, shading: Vec3) -> Option<Vec3> {
        let mut sum = Vec3::ZERO;
        let mut total = 0.0;
        self.bvh.containing(point, |index| {
            if let Some(record) = self.records.get(index as usize) {
                if let Some(weight) = record.weight(point, normal) {
                    sum += record.at(point, shading) * weight;
                    total += weight;
                }
            }
        });
        (total >= HOLDS).then(|| sum * (1.0 / total))
    }

    /// Whether the records hold for a point at `point` whose smooth normal is
    /// `normal`.
    fn holds(&self, point: Vec3, normal: Vec3) -> bool {
        let mut total = 0.0;
        self.bvh.containing(point, |index| {
            if let Some(weight) = self
                .records
                .get(index as usize)
                .and_then(|record| record.weight(point, normal))
            {
                total += weight;
            }
        });
        total >= HOLDS
    }

    /// Rebuild the hierarchy over every record; `None` when the heap will not
    /// hold it.
    fn index(&mut self) -> Option<()> {
        let bounds: Vec<(u32, Aabb)> = fallible::collected(
            self.records.len(),
            (0u32..)
                .zip(&self.records)
                .map(|(index, record)| (index, record.bounds())),
        )?;
        self.bvh = Bvh::build(&bounds)?;
        self.indexed = self.records.len();
        Some(())
    }
}

/// How far the records' laying down has come.
#[derive(Copy, Clone, Debug)]
enum Stage {
    /// Looking over the grid's sites, from the `n`th in its scrambled order,
    /// for those no record holds for.
    Finding(usize),
    /// Gathering records at the sites found, from the `n`th.
    Gathering(usize),
}

/// The records of a scene being laid down, a grid of the picture at a time.
#[derive(Debug)]
pub(crate) struct Gathering {
    size: (u32, u32),
    encoder: Encoder,
    grid: usize,
    stage: Stage,
    /// The sites of the grid being gathered no record yet holds for.
    sites: Vec<Site>,
    held: Radiosity,
    most: usize,
}

impl Gathering {
    /// The records for a picture of `size`; `None` when the heap will not
    /// hold what laying them down needs.
    pub(crate) fn new(size: (u32, u32)) -> Option<Self> {
        let squares = u64::from(size.0) * RECORDS_A_SQUARE / u64::from(size.1.max(1));
        let pixels = u64::from(size.0) * u64::from(size.1) / PIXELS_A_RECORD;
        let most = usize::try_from(squares.min(pixels)).ok()?.max(1);
        let mut held = Radiosity::default();
        if !fallible::reserve(&mut held.records, most) {
            return None;
        }
        Some(Self {
            size,
            encoder: Encoder::new()?,
            grid: 0,
            stage: Stage::Finding(0),
            sites: Vec::new(),
            held,
            most,
        })
    }

    /// Do the next unit of the work over `scene` across `runner`; whether
    /// every record is laid down, or `None` when the heap refused it.
    pub(crate) fn step(&mut self, scene: &Scene, runner: &dyn JobRunner) -> Option<bool> {
        let Some(&across) = GRIDS.get(self.grid) else {
            return Some(true);
        };
        let spacing = (self.size.1 / across).max(1);
        let tracer = Tracer::new(scene, &self.encoder, self.size, 0);
        let width = runner.width().max(1);
        match self.stage {
            Stage::Finding(next) => {
                let columns = self.size.0.div_ceil(spacing);
                let count = u64::from(columns) * u64::from(self.size.1.div_ceil(spacing));
                let end = next
                    .saturating_add(FIND_UNIT * width)
                    .min(usize::try_from(count).ok()?);
                let held = &self.held;
                let picture = self.size;
                let site = |order: usize| {
                    let index = u64::try_from(order).ok()? * SCRAMBLE % count;
                    let column = u32::try_from(index % u64::from(columns)).ok()?;
                    let row = u32::try_from(index / u64::from(columns)).ok()?;
                    let x = (column * spacing + spacing / 2).min(picture.0.saturating_sub(1));
                    let y = (row * spacing + spacing / 2).min(picture.1.saturating_sub(1));
                    tracer
                        .site((x, y))
                        .filter(|site| !held.holds(site.point, site.normal))
                };
                let mut found: Vec<(usize, Option<Site>)> =
                    fallible::collected(end - next, (next..end).map(|order| (order, None)))?;
                tairix_parallel::for_each(runner, &mut found, &|(order, slot)| {
                    *slot = site(*order);
                });
                let room = self.most.saturating_sub(held.records.len());
                for (_, site) in found {
                    if self.sites.len() >= room {
                        break;
                    }
                    if let Some(site) = site {
                        if !fallible::reserve(&mut self.sites, 1) {
                            return None;
                        }
                        self.sites.push(site);
                    }
                }
                let whole = u64::try_from(end).ok()? >= count;
                self.stage = if whole || self.sites.len() >= room {
                    Stage::Gathering(0)
                } else {
                    Stage::Finding(end)
                };
            }
            Stage::Gathering(next) => {
                let end = next
                    .saturating_add(GATHER_UNIT * width)
                    .min(self.sites.len());
                let sites = self.sites.get(next..end).unwrap_or(&[]);
                let mut gathered: Vec<(Site, Option<Record>)> =
                    fallible::collected(sites.len(), sites.iter().map(|&site| (site, None)))?;
                tairix_parallel::for_each(runner, &mut gathered, &|(site, slot)| {
                    let mut cells = [Cell::DARK; CELLS];
                    tracer.gather(site, &mut cells);
                    *slot = Some(Record::new(site, &cells));
                });
                self.held
                    .records
                    .extend(gathered.into_iter().filter_map(|(_, record)| record));
                if end < self.sites.len() {
                    self.stage = Stage::Gathering(end);
                } else {
                    self.held.index()?;
                    self.sites.clear();
                    let spent = self.held.records.len() >= self.most;
                    self.grid = if spent { GRIDS.len() } else { self.grid + 1 };
                    self.stage = Stage::Finding(0);
                }
            }
        }
        Some(self.grid >= GRIDS.len())
    }

    /// How far the laying down has come: the share of the records it may lay
    /// that it has, or of its grids it has looked over, whichever is further.
    pub(crate) fn done(&self) -> f64 {
        let within = match self.stage {
            Stage::Finding(_) => 0.0,
            Stage::Gathering(next) => 0.5 + 0.5 * share(next, self.sites.len()),
        };
        let grids = (real(self.grid) + within) / real(GRIDS.len());
        share(self.held.records.len(), self.most).max(grids.min(1.0))
    }

    /// The records laid down, their hierarchy built over every one.
    pub(crate) fn finish(mut self) -> Option<Radiosity> {
        if self.held.indexed < self.held.records.len() {
            self.held.index()?;
        }
        Some(self.held)
    }
}

/// What the rays of the record gathered for the site at pixel `(x, y)` are
/// drawn under.
pub(crate) fn seed((x, y): (u32, u32)) -> u32 {
    mix32(mix32(x ^ 0x2545_f491) ^ y.wrapping_mul(0x85eb_ca6b))
}

#[cfg(test)]
#[path = "radiosity_tests.rs"]
mod tests;
