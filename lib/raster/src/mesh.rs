//! Depth-tested triangle meshes: the scan converter's pixel-centre rule, with
//! a depth buffer and perspective-correct weights.
//!
//! A mesh is drawn as a GPU draws one without multisampling: a pixel belongs
//! to a triangle when its centre does, decided by the crossings and the tie
//! rule of [`Coverage::Centre`](crate::scan::Coverage::Centre). Triangles
//! sharing an edge therefore own every centre along it exactly once, so a
//! mesh has neither cracks nor doubled pixels.
//!
//! Depth is reciprocal: a vertex's [`Depth`] is proportional to the
//! reciprocal of its distance along the view, larger nearer, and
//! [`Depth::CLEAR`] lies beyond anything drawn. The reciprocal is affine across
//! the screen, so it interpolates exactly, and it is the perspective weight: a
//! fragment's [`Weights`] are its vertices' perspective-correct barycentric
//! coordinates.
//!
//! Coverage is integer, exactly the converter's. The depth and weight planes
//! are set up in `f64` once per triangle and evaluated per pixel in a fixed
//! order with nothing fused, so a mesh draws the same pixels on every target.

use alloc::vec::Vec;
use core::ops::Range;
use core::slice::ChunksMut;

use tairix_inline::ArrayVec;

use crate::scan::{edge_between, first_centre_from, Edge, COORD_LIMIT, UNIT};

/// Sub-units per pixel of a [`MeshVertex`] position: the converter's own grid.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the converter's grid is 256, far inside i32"
)]
pub const MESH_SUB_UNITS: i32 = UNIT as i32;

/// The sum of a fragment's [`Weights`].
pub const WEIGHT_ONE: u32 = 1 << 16;

/// Reciprocal depth: larger is nearer.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Depth(pub u32);

impl Depth {
    /// Where nothing has been drawn: beyond every vertex, which is at least
    /// one.
    pub const CLEAR: Self = Self(0);

    /// The nearest depth a vertex can carry.
    pub const NEAREST: Self = Self(u32::MAX);
}

/// How an incoming fragment's depth is compared with the one held.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DepthCompare {
    /// It passes where it is strictly nearer.
    Nearer,
    /// It passes where it is nearer or level.
    NearerOrEqual,
    /// It always passes.
    Always,
}

impl DepthCompare {
    /// Whether a fragment at `incoming` passes over one at `held`.
    #[must_use]
    pub const fn passes(self, incoming: Depth, held: Depth) -> bool {
        match self {
            Self::Nearer => incoming.0 > held.0,
            Self::NearerOrEqual => incoming.0 >= held.0,
            Self::Always => true,
        }
    }
}

/// The depth test a draw is made under.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DepthTest {
    /// How a fragment is compared with what is held.
    pub compare: DepthCompare,
    /// Whether a fragment that passes and is kept writes its depth.
    pub write: bool,
}

impl DepthTest {
    /// Opaque geometry: nearer fragments win and record themselves.
    pub const OPAQUE: Self = Self {
        compare: DepthCompare::Nearer,
        write: true,
    };
}

/// A depth plane, one value a pixel, split into row bands a pass may write
/// independently.
#[derive(Debug, Default)]
pub struct DepthBuffer {
    width: u32,
    height: u32,
    depths: Vec<Depth>,
}

impl DepthBuffer {
    /// An empty buffer, sized on its first [`Self::reset`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            depths: Vec::new(),
        }
    }

    /// Make the buffer `width` × `height` with every pixel [`Depth::CLEAR`],
    /// answering whether it fits; one that does not is left with no pixels.
    pub fn reset(&mut self, width: u32, height: u32) -> bool {
        self.depths.clear();
        (self.width, self.height) = (0, 0);
        let Ok(len) = usize::try_from(u64::from(width) * u64::from(height)) else {
            return false;
        };
        if self.depths.try_reserve(len).is_err() {
            return false;
        }
        self.depths.resize(len, Depth::CLEAR);
        (self.width, self.height) = (width, height);
        true
    }

    /// Its width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Its height, in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The depth held at `(x, y)`, [`Depth::CLEAR`] outside the buffer.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> Depth {
        self.row(y).get(x as usize).copied().unwrap_or(Depth::CLEAR)
    }

    /// Row `y`, empty outside the buffer.
    #[must_use]
    pub fn row(&self, y: u32) -> &[Depth] {
        row_of(&self.depths, self.width, 0, y)
    }

    /// Every row in bands of `rows_per_band`, the last short where they do not
    /// divide evenly, from row 0: band *i* names the same rows as band *i* of
    /// a surface of the same height cut by the same size with no clip window.
    /// A band size of `0` reads as `1`.
    pub fn bands_mut(&mut self, rows_per_band: u32) -> DepthBands<'_> {
        let per_band = rows_per_band.max(1);
        let block = usize::try_from(u64::from(per_band) * u64::from(self.width))
            .unwrap_or(usize::MAX)
            .max(1);
        DepthBands {
            chunks: self.depths.chunks_mut(block),
            next_row: 0,
            rows_per_band: per_band,
            width: self.width,
        }
    }
}

/// The bands of a [`DepthBuffer`].
#[derive(Debug)]
pub struct DepthBands<'a> {
    chunks: ChunksMut<'a, Depth>,
    next_row: u32,
    rows_per_band: u32,
    width: u32,
}

impl<'a> Iterator for DepthBands<'a> {
    type Item = DepthBand<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let depths = self.chunks.next()?;
        let rows = u32::try_from(depths.len() / (self.width as usize).max(1)).unwrap_or(0);
        let start = self.next_row;
        self.next_row = start.saturating_add(self.rows_per_band);
        Some(DepthBand {
            rows: start..start.saturating_add(rows),
            width: self.width,
            depths,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.chunks.size_hint()
    }
}

impl ExactSizeIterator for DepthBands<'_> {}

/// One band of a [`DepthBuffer`]: whole rows, exclusively borrowed.
#[derive(Debug)]
pub struct DepthBand<'a> {
    rows: Range<u32>,
    width: u32,
    depths: &'a mut [Depth],
}

impl DepthBand<'_> {
    /// The rows it holds.
    #[must_use]
    pub fn rows(&self) -> Range<u32> {
        self.rows.clone()
    }

    /// Its width, in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Row `y`, empty outside the band.
    #[must_use]
    pub fn row(&self, y: u32) -> &[Depth] {
        row_of(self.depths, self.width, self.rows.start, y)
    }

    fn row_mut(&mut self, y: u32) -> Option<&mut [Depth]> {
        let within = y.checked_sub(self.rows.start)?;
        if y >= self.rows.end {
            return None;
        }
        let width = self.width as usize;
        let start = (within as usize).checked_mul(width)?;
        self.depths.get_mut(start..start.checked_add(width)?)
    }
}

/// Row `y` of rows starting at `first`, `width` wide, empty outside them.
fn row_of(depths: &[Depth], width: u32, first: u32, y: u32) -> &[Depth] {
    let width = width as usize;
    y.checked_sub(first)
        .and_then(|within| (within as usize).checked_mul(width))
        .and_then(|start| depths.get(start..start.checked_add(width)?))
        .unwrap_or(&[])
}

/// One corner of a triangle: where it falls, in [`MESH_SUB_UNITS`] from the
/// drawing's origin, and how near it is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MeshVertex {
    /// Across, in sub-units.
    pub x: i32,
    /// Down, in sub-units.
    pub y: i32,
    /// Its reciprocal depth; [`Depth::CLEAR`] is refused.
    pub depth: Depth,
}

/// A fragment's perspective-correct barycentric coordinates in its triangle's
/// vertices, out of [`WEIGHT_ONE`], summing to it exactly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Weights(pub [u32; 3]);

impl Weights {
    /// The attribute whose values at the vertices are `values`, interpolated
    /// at this fragment and rounded toward negative infinity.
    ///
    /// Exact: three products of a weight and an `i32` sum well inside `i64`,
    /// and a sum of weights is one, so the result lies between the values.
    #[must_use]
    pub fn interpolate(self, values: [i32; 3]) -> i32 {
        let [a, b, c] = self.0;
        let sum = i64::from(a) * i64::from(values[0])
            + i64::from(b) * i64::from(values[1])
            + i64::from(c) * i64::from(values[2]);
        i32::try_from(sum >> WEIGHT_ONE.trailing_zeros()).unwrap_or(if sum < 0 {
            i32::MIN
        } else {
            i32::MAX
        })
    }
}

/// Which way the nearest edge of a fragment's triangle lies from its centre,
/// along the axis that crosses the edge more steeply.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Toward {
    /// The previous column.
    Left,
    /// The next column.
    Right,
    /// The previous row.
    Up,
    /// The next row.
    Down,
}

/// Where a fragment's triangle ends within a pixel of its centre: the
/// distance, in [`EdgeNear::STEPS`] of a pixel, to the nearest edge along the
/// axis crossing it more steeply, and which way that is.
///
/// What a geometry-buffer anti-aliasing resolve reads: a pixel whose edge
/// lies within half a pixel is covered by its own surface for half a pixel
/// plus that distance, and by the neighbour beyond for the rest.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct EdgeNear(u8);

impl EdgeNear {
    /// No edge within a pixel.
    pub const NONE: Self = Self(u8::MAX);

    /// Steps a pixel's distance is quoted in.
    pub const STEPS: u8 = 64;

    /// An edge `distance` steps away `toward` a neighbour, or [`Self::NONE`]
    /// for one of a step short of a pixel or more: the encoding has no room
    /// for that step beside `NONE`, and an edge so far changes no resolve.
    #[must_use]
    pub const fn new(distance: u8, toward: Toward) -> Self {
        if distance >= Self::STEPS - 1 {
            return Self::NONE;
        }
        Self((distance << 2) | toward as u8)
    }

    /// The distance and the direction, or `None` for [`Self::NONE`].
    #[must_use]
    pub const fn get(self) -> Option<(u8, Toward)> {
        if self.0 == Self::NONE.0 {
            return None;
        }
        let toward = match self.0 & 3 {
            0 => Toward::Left,
            1 => Toward::Right,
            2 => Toward::Up,
            _ => Toward::Down,
        };
        Some((self.0 >> 2, toward))
    }

    /// Its stored form.
    #[must_use]
    pub const fn to_bits(self) -> u8 {
        self.0
    }

    /// The record `bits` stores; every byte is one.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
}

/// One pixel a triangle covers and passes the depth test at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Fragment {
    /// Its column.
    pub x: u32,
    /// Its row.
    pub y: u32,
    /// Its depth.
    pub depth: Depth,
    /// Where it lies among the triangle's vertices.
    pub weights: Weights,
    /// Its triangle's nearest edge, where the draw asked for it.
    pub edge: EdgeNear,
}

/// Whether a draw records each fragment's [`EdgeNear`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EdgeRecord {
    /// Every fragment's edge is [`EdgeNear::NONE`].
    Skip,
    /// Every fragment carries its triangle's nearest edge.
    Keep,
}

/// A value affine across the screen, about a reference pixel centre.
#[derive(Copy, Clone, Debug)]
struct Plane {
    at: f64,
    dx: f64,
    dy: f64,
}

impl Plane {
    /// The plane taking `values` at three points `across` and `down` of the
    /// first, whose frame's determinant is `det`.
    fn through(values: [f64; 3], across: [f64; 2], down: [f64; 2], det: f64) -> Self {
        let (rise1, rise2) = (values[1] - values[0], values[2] - values[0]);
        Self {
            at: values[0],
            dx: (rise1 * down[1] - rise2 * down[0]) / det,
            dy: (rise2 * across[0] - rise1 * across[1]) / det,
        }
    }

    fn row(self, dy: f64) -> f64 {
        self.at + self.dy * dy
    }
}

/// One side of a triangle as the signed distance a geometry-buffer resolve
/// reads: `at + dx·column + dy·row` is the side's edge function at a pixel
/// centre, non-negative inside, and `reach` is one pixel of it along the axis
/// it is measured on.
#[derive(Copy, Clone, Debug)]
struct Side {
    at: i64,
    dx: i64,
    dy: i64,
    reach: i64,
    toward: Toward,
}

impl Side {
    fn between(a: (i64, i64), b: (i64, i64), sign: i64) -> Self {
        let (run, rise) = (b.0 - a.0, b.1 - a.1);
        let half = UNIT / 2;
        let at = sign * (run * (half - a.1) - rise * (half - a.0));
        let dx = -sign * rise * UNIT;
        let dy = sign * run * UNIT;
        let (reach, toward) = if rise.abs() >= run.abs() {
            (dx.abs(), if dx < 0 { Toward::Right } else { Toward::Left })
        } else {
            (dy.abs(), if dy < 0 { Toward::Down } else { Toward::Up })
        };
        Self {
            at,
            dx,
            dy,
            reach,
            toward,
        }
    }

    /// The side's value at the centre of `(column, row)`.
    fn value(self, column: i64, row: i64) -> i64 {
        self.at + self.dx * column + self.dy * row
    }
}

/// Where an edge crosses each row centre in turn, stepped exactly as
/// [`Edge::x_at`] rounds it so a stepped crossing is the computed one.
#[derive(Copy, Clone, Debug)]
struct EdgeWalk {
    x_top: i64,
    sign: i64,
    whole: i64,
    remainder: i64,
    rise: i64,
    step_whole: i64,
    step_remainder: i64,
}

impl EdgeWalk {
    /// The walk of `edge` starting at row centre `y`, which lies within it.
    fn at(edge: &Edge, y: i64) -> Self {
        let rise = edge.bottom - edge.top;
        let run = edge.x_bottom - edge.x_top;
        let magnitude = run.abs();
        let reached = magnitude * (y - edge.top) + rise / 2;
        let step = magnitude * UNIT;
        Self {
            x_top: edge.x_top,
            sign: run.signum(),
            whole: reached / rise,
            remainder: reached % rise,
            rise,
            step_whole: step / rise,
            step_remainder: step % rise,
        }
    }

    fn x(self) -> i64 {
        self.x_top + self.sign * self.whole
    }

    fn advance(&mut self) {
        self.whole += self.step_whole;
        self.remainder += self.step_remainder;
        if self.remainder >= self.rise {
            self.remainder -= self.rise;
            self.whole += 1;
        }
    }
}

/// A triangle set up to be filled band by band.
#[derive(Clone, Debug)]
pub struct Triangle {
    edges: ArrayVec<Edge, 3>,
    /// The first row whose centre it reaches, and one past the last.
    rows: (i64, i64),
    /// Vertex zero's position, a pixel centre at each whole number.
    reference: (f64, f64),
    depth: Plane,
    first: Plane,
    second: Plane,
    sides: [Side; 3],
}

impl Triangle {
    /// Set up the triangle with corners `vertices`, or `None` where it covers
    /// no area, a corner carries [`Depth::CLEAR`], or a corner lies further
    /// out than [`MAX_DRAWING_EXTENT`](crate::scan::MAX_DRAWING_EXTENT) pixels,
    /// where its arithmetic could not be kept exact. Such a triangle draws
    /// nothing rather than a distorted shape: a caller clips its geometry to
    /// the view first.
    #[must_use]
    pub fn new(vertices: [MeshVertex; 3]) -> Option<Self> {
        if vertices.iter().any(|v| {
            v.depth == Depth::CLEAR
                || i64::from(v.x).abs() > COORD_LIMIT
                || i64::from(v.y).abs() > COORD_LIMIT
        }) {
            return None;
        }
        let at = vertices.map(|v| (i64::from(v.x), i64::from(v.y)));
        let area =
            (at[1].0 - at[0].0) * (at[2].1 - at[0].1) - (at[2].0 - at[0].0) * (at[1].1 - at[0].1);
        if area == 0 {
            return None;
        }
        let sign = area.signum();

        let mut edges = ArrayVec::new();
        for index in 0..3 {
            if let Some(edge) = edge_between(at[index], at[(index + 1) % 3]) {
                // Three sides make at most three edges.
                let _ = edges.try_push(edge);
            }
        }
        let top = at.iter().map(|p| p.1).min().unwrap_or(0);
        let bottom = at.iter().map(|p| p.1).max().unwrap_or(0);

        let pixels = |sub: i64| coordinate(sub) / coordinate(UNIT);
        let reference = (pixels(at[0].0 - UNIT / 2), pixels(at[0].1 - UNIT / 2));
        let across = [pixels(at[1].0 - at[0].0), pixels(at[2].0 - at[0].0)];
        let down = [pixels(at[1].1 - at[0].1), pixels(at[2].1 - at[0].1)];
        // From the exact area, so a sliver's determinant cannot round to
        // nought or change sign the way the difference of two large products
        // could.
        let det = area_pixels(area);
        let q = vertices.map(|v| f64::from(v.depth.0));
        Some(Self {
            edges,
            rows: (first_centre_from(top), first_centre_from(bottom)),
            reference,
            depth: Plane::through(q, across, down, det),
            first: Plane::through([0.0, q[1], 0.0], across, down, det),
            second: Plane::through([0.0, 0.0, q[2]], across, down, det),
            sides: [
                Side::between(at[1], at[2], sign),
                Side::between(at[2], at[0], sign),
                Side::between(at[0], at[1], sign),
            ],
        })
    }

    /// The rows it reaches, clamped at the top of the drawing.
    #[must_use]
    pub fn rows(&self) -> Range<u32> {
        let clamp = |row: i64| u32::try_from(row.max(0)).unwrap_or(u32::MAX);
        clamp(self.rows.0)..clamp(self.rows.1)
    }

    /// Fill the part of the triangle `band` holds under `test`, handing each
    /// fragment that passes to `shade`, which answers whether it keeps it; a
    /// kept fragment writes its depth where the test does.
    pub fn fill(
        &self,
        band: &mut DepthBand<'_>,
        test: DepthTest,
        record: EdgeRecord,
        mut shade: impl FnMut(Fragment) -> bool,
    ) {
        let held = band.rows();
        let start = self.rows.0.max(i64::from(held.start));
        let end = self.rows.1.min(i64::from(held.end));
        let width = i64::from(band.width);
        let mut walks: [Option<EdgeWalk>; 3] = [None; 3];
        for row in start..end {
            let y = row * UNIT + UNIT / 2;
            let mut crossings = [0i64; 2];
            let mut found = 0;
            for (edge, walk) in self.edges.iter().zip(&mut walks) {
                if y < edge.top || y >= edge.bottom {
                    continue;
                }
                let walk = walk.get_or_insert_with(|| EdgeWalk::at(edge, y));
                if let Some(slot) = crossings.get_mut(found) {
                    *slot = walk.x();
                    found += 1;
                }
                walk.advance();
            }
            if found < 2 {
                continue;
            }
            let (left, right) = (
                crossings[0].min(crossings[1]),
                crossings[0].max(crossings[1]),
            );
            let from = first_centre_from(left).clamp(0, width);
            let to = first_centre_from(right).clamp(0, width);
            let (Ok(row), Ok(from), Ok(to)) =
                (u32::try_from(row), u32::try_from(from), u32::try_from(to))
            else {
                continue;
            };
            if from >= to {
                continue;
            }
            if let Some(depths) = band.row_mut(row) {
                self.span(depths, row, from..to, test, record, &mut shade);
            }
        }
    }

    /// Test and shade the covered columns `columns` of row `row`.
    fn span(
        &self,
        depths: &mut [Depth],
        row: u32,
        columns: Range<u32>,
        test: DepthTest,
        record: EdgeRecord,
        shade: &mut impl FnMut(Fragment) -> bool,
    ) {
        let Some(slots) = depths.get_mut(columns.start as usize..columns.end as usize) else {
            return;
        };
        let down = f64::from(row) - self.reference.1;
        let (depth, first, second) = (
            self.depth.row(down),
            self.first.row(down),
            self.second.row(down),
        );
        let keep = record == EdgeRecord::Keep;
        let mut sides = [0; 3];
        if keep {
            sides = self
                .sides
                .map(|side| side.value(i64::from(columns.start), i64::from(row)));
        }
        for (x, slot) in columns.zip(slots) {
            let across = f64::from(x) - self.reference.0;
            let reciprocal = depth + self.depth.dx * across;
            let incoming = to_depth(reciprocal);
            if test.compare.passes(incoming, *slot) {
                let inverse = 1.0 / reciprocal;
                let weights = weights(
                    (first + self.first.dx * across) * inverse,
                    (second + self.second.dx * across) * inverse,
                );
                let edge = if keep {
                    nearest(&self.sides, &sides)
                } else {
                    EdgeNear::NONE
                };
                let fragment = Fragment {
                    x,
                    y: row,
                    depth: incoming,
                    weights,
                    edge,
                };
                if shade(fragment) && test.write {
                    *slot = incoming;
                }
            }
            if keep {
                for (value, side) in sides.iter_mut().zip(&self.sides) {
                    *value += side.dx;
                }
            }
        }
    }
}

/// The nearest of a fragment's sides within a pixel, given each side's value
/// at its centre.
fn nearest(sides: &[Side; 3], values: &[i64; 3]) -> EdgeNear {
    let steps = i64::from(EdgeNear::STEPS);
    let mut best = EdgeNear::NONE;
    let mut closest = steps;
    for (side, &value) in sides.iter().zip(values) {
        let inside = value.max(0);
        if inside >= side.reach {
            continue;
        }
        let distance = inside * steps / side.reach.max(1);
        if distance < closest {
            closest = distance;
            best = EdgeNear::new(u8::try_from(distance).unwrap_or(u8::MAX), side.toward);
        }
    }
    best
}

/// The weights whose second and third vertex shares are `second` and
/// `third`, rounded to [`WEIGHT_ONE`] and held summing to it.
fn weights(second: f64, third: f64) -> Weights {
    let second = share(second);
    let third = share(third).min(WEIGHT_ONE - second);
    Weights([WEIGHT_ONE - second - third, second, third])
}

/// A share in `0..=1` as a count of [`WEIGHT_ONE`], rounded.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped into 0..=WEIGHT_ONE first"
)]
fn share(value: f64) -> u32 {
    let scaled = value * f64::from(WEIGHT_ONE) + 0.5;
    if scaled.is_nan() {
        return 0;
    }
    scaled.clamp(0.0, f64::from(WEIGHT_ONE)) as u32
}

/// A plane's reciprocal depth as a [`Depth`], never [`Depth::CLEAR`].
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped into 1..=u32::MAX first"
)]
pub(crate) fn to_depth(value: f64) -> Depth {
    if value.is_nan() {
        return Depth(1);
    }
    Depth(value.clamp(1.0, f64::from(u32::MAX)) as u32)
}

/// A sub-unit coordinate, or a difference of two, as a real number: exact,
/// since it is within twice [`COORD_LIMIT`].
#[allow(
    clippy::cast_precision_loss,
    reason = "a coordinate is within 2^29, which a double holds exactly"
)]
fn coordinate(value: i64) -> f64 {
    value as f64
}

/// Twice a triangle's area in sub-units squared, as the determinant of its
/// frame in pixels: rounded once, so never nought for a triangle that has
/// area and never of the wrong sign.
#[allow(
    clippy::cast_precision_loss,
    reason = "a double rounds the area once, keeping its sign and its magnitude"
)]
fn area_pixels(area: i64) -> f64 {
    area as f64 / coordinate(UNIT * UNIT)
}

/// A polygon's reciprocal depth across the screen, for a fill tested against
/// a [`DepthBuffer`] it does not write: an affine function of the pixel
/// centre, a pixel `(x, y)`'s centre lying at `(x + ½, y + ½)`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DepthPlane {
    /// Its value at the surface's origin, the corner of pixel `(0, 0)`.
    origin: f64,
    across: f64,
    down: f64,
}

impl DepthPlane {
    /// The same depth everywhere.
    #[must_use]
    #[allow(
        clippy::cast_lossless,
        reason = "`f64::from` is not callable in a constant, and a u32 converts \
                  to a double exactly either way"
    )]
    pub const fn flat(depth: Depth) -> Self {
        Self {
            origin: depth.0 as f64,
            across: 0.0,
            down: 0.0,
        }
    }

    /// The plane through three `(x, y, reciprocal depth)` points, or `None`
    /// where they are collinear or not finite.
    #[must_use]
    pub fn through(points: [(f64, f64, f64); 3]) -> Option<Self> {
        if points
            .iter()
            .any(|(x, y, d)| !x.is_finite() || !y.is_finite() || !d.is_finite())
        {
            return None;
        }
        let across = [points[1].0 - points[0].0, points[2].0 - points[0].0];
        let down = [points[1].1 - points[0].1, points[2].1 - points[0].1];
        let det = across[0] * down[1] - across[1] * down[0];
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let plane = Plane::through(points.map(|p| p.2), across, down, det);
        let origin = plane.at - plane.dx * points[0].0 - plane.dy * points[0].1;
        origin.is_finite().then_some(Self {
            origin,
            across: plane.dx,
            down: plane.dy,
        })
    }

    /// Its depth at pixel `(x, y)`'s centre.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> Depth {
        let (centre_x, centre_y) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
        to_depth(self.origin + self.across * centre_x + self.down * centre_y)
    }
}

/// What a fill tested against a depth buffer is hidden behind.
#[derive(Copy, Clone, Debug)]
pub struct Occlusion<'a> {
    /// The depth already drawn.
    pub depth: &'a DepthBuffer,
    /// The fill's own depth.
    pub plane: DepthPlane,
    /// How the two are compared.
    pub compare: DepthCompare,
}

impl Occlusion<'_> {
    /// Whether the fill shows at pixel `(x, y)`.
    #[must_use]
    pub fn shows(&self, x: u32, y: u32) -> bool {
        self.compare
            .passes(self.plane.at(x, y), self.depth.at(x, y))
    }
}

#[cfg(test)]
#[path = "mesh_tests.rs"]
mod tests;
