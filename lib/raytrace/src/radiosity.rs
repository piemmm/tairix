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

use crate::bvh::{Builder, Bvh};
use crate::detail::Records;
use crate::sample::mix32;
use crate::scene::Scene;
use crate::shape::Aabb;
use crate::tone::Encoder;
use crate::trace::Tracer;
use crate::vector::{real, share, Frame, Vec3};

/// A record holds within this share of the harmonic mean distance of what
/// its rays met: Ward's accuracy.
const ACCURACY: f64 = 0.4;

/// The most a record may hold for, as a share of the picture's height where
/// it lies: coarser would reach over changes in the light no ray of its own
/// looked at. The least is a share as fine as its detail's finest grid, finer
/// spending records on changes too small to see. Shares, not pixels, so a
/// picture costs the same to gather for at any size.
const FARTHEST: f64 = 1.0 / 20.0;

/// The cosine of the widest angle between a record's normal and a point's
/// that the record still holds for: fifteen degrees.
const COS_TURN: f64 = 0.965_925_826_289_068_3;

/// How far behind a record's surface, as a share of its radius, a point may
/// lie and still take its light: farther, and something between them may
/// shade one and not the other.
const BEHIND: f64 = 0.2;

/// The least weight the records at a point must sum to for their light to
/// stand for its own.
const HOLDS: f64 = 0.3;

/// How many rows of sites the coarsest grid records are laid over has across
/// the picture's height: each finer grid has twice as many, down to the
/// detail's finest.
const COARSEST: u32 = 15;

/// Rays of a record's hemisphere one core gathers in a unit of work, in whole
/// rows and at least one; and sites it looks over.
const GATHER_RAYS: usize = 64;
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

/// The direction, about the local `z` axis, of cell `index`'s ray of a
/// hemisphere cut as `records` has it, placed within its cell by `(u, v)`.
pub(crate) fn direction(records: &Records, index: usize, (u, v): (f64, f64)) -> Vec3 {
    let (row, column) = (index / records.columns, index % records.columns);
    let sin2 = (real(row) + u) / real(records.rows);
    let sin = mathf::sqrt(sin2);
    let cos = mathf::sqrt((1.0 - sin2).max(0.0));
    let azimuth = TAU * (real(column) + v) / real(records.columns);
    Vec3::new(sin * mathf::cos(azimuth), sin * mathf::sin(azimuth), cos)
}

/// `weights` per channel of `axis`, added to `gradient`.
fn add(gradient: &mut [Vec3; 3], weights: Vec3, axis: Vec3) {
    for (channel, slot) in gradient.iter_mut().enumerate() {
        *slot += axis * weights.along(channel);
    }
}

impl Record {
    /// The record the rays `cells` of a hemisphere cut as `records` has it
    /// brought back to `site`.
    fn new(site: &Site, cells: &[Cell], records: &Records) -> Self {
        let (rows, columns) = (records.rows, records.columns);
        let frame = Frame::around(site.normal);
        let (u, v) = (frame.x, frame.y);
        let cell = |row: usize, column: usize| {
            cells
                .get(row * columns + column % columns)
                .copied()
                .unwrap_or(Cell::DARK)
        };
        let mut light = Vec3::ZERO;
        let mut nearness = 0.0;
        let mut turning = [Vec3::ZERO; 3];
        let mut moving = [Vec3::ZERO; 3];
        for row in 0..rows {
            let (low, high) = (real(row) / real(rows), real(row + 1) / real(rows));
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
            for column in 0..columns {
                let start = TAU * real(column) / real(columns);
                let end = TAU * real(column + 1) / real(columns);
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
                let before = cell(row, column + columns - 1);
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
        let light = light * (1.0 / real(records.cells()));
        for gradient in turning.iter_mut().chain(moving.iter_mut()) {
            *gradient = *gradient * (1.0 / PI);
        }
        let harmonic = if nearness > 0.0 {
            real(records.cells()) / nearness
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
            radius: mathf::clamp(
                radius,
                site.span / f64::from(records.finest),
                FARTHEST * site.span,
            ),
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

    /// A build of the hierarchy over every record laid so far, to be stepped
    /// a slice at a time; `None` when the heap will not hold it.
    fn indexing(&self) -> Option<Builder> {
        let bounds: Vec<(u32, Aabb)> = fallible::collected(
            self.records.len(),
            (0u32..)
                .zip(&self.records)
                .map(|(index, record)| (index, record.bounds())),
        )?;
        Builder::new(&bounds)
    }
}

/// How far the records' laying down has come.
#[derive(Debug)]
enum Stage {
    /// Looking over the grid's sites, from the `n`th in its scrambled order,
    /// for those no record holds for.
    Finding(usize),
    /// Gathering records at the sites found, from the `n`th row of their
    /// hemispheres counted through every site.
    Gathering(usize),
    /// The records laid so far being indexed, before the next grid looks for
    /// sites none of them holds for.
    Indexing(Builder),
}

/// Records' worth of their hierarchy built in one step.
const INDEX_UNIT: usize = 16_384;

/// The records of a scene being laid down, a grid of the picture at a time.
#[derive(Debug)]
pub(crate) struct Gathering {
    size: (u32, u32),
    encoder: Encoder,
    grid: usize,
    stage: Stage,
    /// The sites of the grid being gathered no record yet holds for.
    sites: Vec<Site>,
    /// What the rays of the sites a unit gathers for brought back, a site's
    /// cells after another's, the first holding the rows of a site an
    /// earlier unit began.
    cells: Vec<Cell>,
    held: Radiosity,
    most: usize,
    records: &'static Records,
}

impl Gathering {
    /// The records for a picture of `size`, laid as `records` has them;
    /// `None` when the heap will not hold what laying them down needs.
    pub(crate) fn new(size: (u32, u32), records: &'static Records) -> Option<Self> {
        let squares = u64::from(size.0) * records.a_square / u64::from(size.1.max(1));
        let pixels = u64::from(size.0) * u64::from(size.1) / records.pixels_each;
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
            cells: Vec::new(),
            held,
            most,
            records,
        })
    }

    /// How many rows of sites grid `grid` lays records over across the
    /// picture's height; `None` past the finest.
    fn across(&self, grid: usize) -> Option<u32> {
        let across = COARSEST.checked_shl(u32::try_from(grid).ok()?)?;
        (across <= self.records.finest).then_some(across)
    }

    /// How many grids records are laid over, coarsest to finest.
    fn grids(&self) -> usize {
        // No grid lies past a whole word's shift of the coarsest.
        (0..32)
            .take_while(|&grid| self.across(grid).is_some())
            .count()
    }

    /// Do the next unit of the work over `scene` across `runner`; whether
    /// every record is laid down, or `None` when the heap refused it.
    pub(crate) fn step(&mut self, scene: &Scene, runner: &dyn JobRunner) -> Option<bool> {
        if let Stage::Indexing(builder) = &mut self.stage {
            if builder.step(INDEX_UNIT) {
                if let Stage::Indexing(builder) =
                    core::mem::replace(&mut self.stage, Stage::Finding(0))
                {
                    self.held.bvh = builder.finish();
                }
            }
            return Some(self.whole());
        }
        let Some(across) = self.across(self.grid) else {
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
                        self.sites.try_reserve(1).ok()?;
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
                let rows = self.sites.len() * self.records.rows;
                let per_core = (GATHER_RAYS / self.records.columns).max(1);
                let end = next.saturating_add(per_core * width).min(rows);
                if next < end {
                    let held = (self.sites.as_slice(), &mut self.cells, &mut self.held);
                    gather_rows(&tracer, held, (next..end, self.records), runner)?;
                }
                if end < rows {
                    self.stage = Stage::Gathering(end);
                } else {
                    self.sites.clear();
                    let spent = self.held.records.len() >= self.most;
                    self.grid = if spent { self.grids() } else { self.grid + 1 };
                    self.stage = Stage::Indexing(self.held.indexing()?);
                }
            }
            Stage::Indexing(_) => {}
        }
        Some(self.whole())
    }

    /// Whether every grid's records are laid down and indexed.
    fn whole(&self) -> bool {
        !matches!(self.stage, Stage::Indexing(_)) && self.across(self.grid).is_none()
    }

    /// How far the laying down has come: the share of the records it may lay
    /// that it has, or of its grids it has looked over, whichever is further.
    pub(crate) fn done(&self) -> f64 {
        let within = match self.stage {
            Stage::Finding(_) | Stage::Indexing(_) => 0.0,
            Stage::Gathering(next) => 0.5 + 0.5 * share(next, self.sites.len() * self.records.rows),
        };
        let grids = (real(self.grid) + within) / real(self.grids());
        share(self.held.records.len(), self.most).max(grids.min(1.0))
    }

    /// The records laid down, their hierarchy built over every one.
    pub(crate) fn finish(self) -> Radiosity {
        self.held
    }
}

/// Gather rows `rows` of `sites`' hemispheres, cut as `records` has them and
/// counted through every site, a row a piece across `runner` into `cells`,
/// and lay down in `held` the record of each site whose last row they reach;
/// `None` when the heap refused them.
fn gather_rows(
    tracer: &Tracer<'_>,
    (sites, cells, held): (&[Site], &mut Vec<Cell>, &mut Radiosity),
    (rows, records): (core::ops::Range<usize>, &Records),
    runner: &dyn JobRunner,
) -> Option<()> {
    let (height, width) = (records.rows, records.columns);
    let whole_site = records.cells();
    let first = rows.start / height;
    let count = (rows.end - 1) / height - first + 1;
    if !fallible::grow_to(cells, count * whole_site, Cell::DARK) {
        return None;
    }
    let sites = sites.get(first..first + count)?;
    let mut pieces: Vec<(&Site, usize, &mut [Cell])> = Vec::new();
    if !fallible::reserve(&mut pieces, rows.len()) {
        return None;
    }
    for ((site_rows, site), gathered) in (first * height..)
        .step_by(height)
        .zip(sites)
        .zip(cells.chunks_mut(whole_site))
    {
        for (row, piece) in gathered.chunks_mut(width).enumerate() {
            if rows.contains(&(site_rows + row)) {
                pieces.push((site, row, piece));
            }
        }
    }
    tairix_parallel::for_each(runner, &mut pieces, &|(site, row, piece)| {
        tracer.gather(site, (records, *row), piece);
    });
    let whole = (rows.end / height).saturating_sub(first);
    for (site, gathered) in sites.iter().zip(cells.chunks(whole_site)).take(whole) {
        if held.records.len() < held.records.capacity() {
            held.records.push(Record::new(site, gathered, records));
        }
    }
    // A site the unit began but did not finish carries into the next.
    if whole < count {
        cells.copy_within((count - 1) * whole_site..count * whole_site, 0);
    }
    Some(())
}

/// What the rays of the record gathered for the site at pixel `(x, y)` are
/// drawn under.
pub(crate) fn seed((x, y): (u32, u32)) -> u32 {
    mix32(mix32(x ^ 0x2545_f491) ^ y.wrapping_mul(0x85eb_ca6b))
}

#[cfg(test)]
#[path = "radiosity_tests.rs"]
mod tests;
