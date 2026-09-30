//! The two mountain ranges the sun sets between.
//!
//! Each range is a lattice on the floor's own axes, its vertices jittered so
//! no two faces are alike, lifted by a scatter of peaks beneath an envelope
//! that keeps the valley the sun sets into clear and rises towards the
//! screen's edges. A peak's height grows with its distance at the rate
//! perspective shrinks it, so near and far peaks share one silhouette and
//! stand one behind another.
//!
//! The ranges are drawn once, far to near and each row from its outer edge
//! in, which is an order in which nothing drawn can stand in front of what
//! is drawn after it; each band of the screen's rows is drawn in that order
//! on a core of its own. Every face is dark, hazed by its distance and its
//! height; every edge glows, drawn as soon as the nearer of its two faces is
//! down, so a nearer slope hides what stands behind it and no edge is drawn
//! twice.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_raster::{Canvas, ScanScratch, SUBPIXEL};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::{fallible, mathf};
use tairix_wm::{Color, Rect, Surface};

use super::{column, paint_bands, Rgb, View, CAMERA};

/// Where the ranges begin and end ahead of the viewer, and the lattice's
/// spacing, in grid cells.
const NEAR: f64 = 22.0;
const FAR: f64 = 120.0;
const CELL: f64 = 5.0;

/// How far past the screen's edge the lattice reaches, as a share of the
/// half-width, so an edge face is never cut short.
const EDGE_REACH: f64 = 1.08;

/// How far a vertex strays from its lattice point, as a share of a cell.
const JITTER: f64 = 0.32;

/// The envelope the peaks rise beneath, in screen heights: clear of the
/// valley within `CLEAR` of the centre line, then rising over `RISE` towards
/// `TALLEST`.
const CLEAR: f64 = 0.03;
const RISE: f64 = 0.45;
const TALLEST: f64 = 0.52;

/// Peaks in each range; the angles across the screen they are spread over,
/// in screen heights from the centre line; how wide one is, as a share of its
/// distance; and how much of the envelope it reaches.
const PEAKS: usize = 12;
const PEAK_SPREAD: (f64, f64) = (0.06, 1.3);
const PEAK_WIDTH: (f64, f64) = (0.11, 0.24);
const PEAK_HEIGHT: (f64, f64) = (0.55, 1.0);

/// The share of the envelope the ground between the peaks keeps, and how far
/// a vertex's height strays from the peaks', either way.
const FOOTHILLS: f64 = 0.08;
const ROUGH: f64 = 0.22;

/// A face's own dark, and the haze it fades into.
const FACE: Rgb = Rgb::new(2.0, 7.0, 20.0);
const FACE_HAZE: Rgb = Rgb::new(44.0, 68.0, 124.0);

/// An edge's glow, and the haze it fades into.
const EDGE: Rgb = Rgb::new(82.0, 138.0, 252.0);
const EDGE_HAZE: Rgb = Rgb::new(54.0, 82.0, 142.0);

/// How much of the haze a face or an edge takes at the horizon, falling off
/// with its height over `HAZE_RISE` screen heights, and how much more the
/// farthest take for their distance.
const HEIGHT_HAZE: f64 = 0.95;
const HAZE_RISE: f64 = 0.035;
const DEPTH_HAZE: f64 = 0.35;

/// How much brighter a face turned towards the valley is than one turned
/// away: the sun's glow catches it.
const FACE_LIGHT: f64 = 0.35;

/// An edge's core and its glow, in logical pixels wide, and how opaque the
/// glow is.
const EDGE_WIDTH: f64 = 1.3;
const GLOW_WIDTH: f64 = 4.5;
const GLOW_ALPHA: f64 = 0.16;

/// One vertex of a range.
#[derive(Copy, Clone, Debug, Default)]
struct Vertex {
    /// Where it lands on the screen, in pixels.
    at: (f64, f64),
    /// How far across and ahead of the viewer it stands, and how high, in
    /// cells.
    ground: (f64, f64),
    height: f64,
    /// How high above the horizon it stands, in screen heights.
    rise: f64,
}

/// A peak of one range.
#[derive(Copy, Clone, Debug)]
struct Peak {
    /// Where it stands, across and ahead, in cells.
    ground: (f64, f64),
    /// Its radius, in cells, and the share of the envelope it reaches.
    radius: f64,
    height: f64,
}

/// The two mountain ranges.
pub(super) struct Mountains {
    view: View,
    /// Both ranges' vertices, the left then the right, each row by row from
    /// the nearest and each row from the centre line out.
    vertices: Vec<Vertex>,
    /// Cells across a range, and ahead.
    columns: usize,
    rows: usize,
}

impl Mountains {
    /// The ranges of `view`, scattered from `seed`, or `None` when the heap
    /// will not give their lattice.
    pub(super) fn new(view: &View, seed: u64) -> Option<Self> {
        let across = f64::from(view.width) / 2.0 / view.focal * EDGE_REACH;
        let columns = cells(FAR * across);
        let rows = cells(FAR - NEAR);
        let per_range = (columns + 1).checked_mul(rows + 1)?;
        let mut vertices = Vec::new();
        if !fallible::reserve(&mut vertices, per_range.checked_mul(2)?) {
            return None;
        }
        let mut rng = NonCryptoRng::seed_from_u64(seed);
        let tall = f64::from(view.height);
        for side in [-1.0, 1.0] {
            let peaks: [Peak; PEAKS] =
                core::array::from_fn(|index| Peak::scattered(index, tall / view.focal, &mut rng));
            for row in 0..=rows {
                for column in 0..=columns {
                    let stray = |rng: &mut NonCryptoRng, inner: bool| {
                        if inner {
                            0.0
                        } else {
                            (rng.next_f64() * 2.0 - 1.0) * JITTER * CELL
                        }
                    };
                    let edge_row = row == 0 || row == rows;
                    let depth = NEAR + index_f64(row) * CELL + stray(&mut rng, edge_row);
                    let lateral = index_f64(column) * CELL + stray(&mut rng, column == 0);
                    let rough = 1.0 + (rng.next_f64() * 2.0 - 1.0) * ROUGH;
                    let lift = envelope(lateral / depth / tall * view.focal);
                    let rise = lift * mathf::fmax(ridge(&peaks, lateral, depth), FOOTHILLS) * rough;
                    let height = rise * depth * tall / view.focal;
                    vertices.push(Vertex {
                        at: (
                            view.centre + side * lateral * view.focal / depth,
                            f64::from(view.horizon) + (CAMERA - height) * view.focal / depth,
                        ),
                        ground: (side * lateral, depth),
                        height,
                        rise: rise - CAMERA / depth * view.focal / tall,
                    });
                }
            }
        }
        Some(Self {
            view: *view,
            vertices,
            columns,
            rows,
        })
    }

    /// Draw both ranges onto `surface` above the horizon, band by band
    /// across `runner`: each band draws every face and edge reaching it, in
    /// the one order, so the bands together are the ranges drawn whole.
    pub(super) fn paint(&self, surface: &mut Surface, runner: &dyn JobRunner) {
        let width = self.view.width;
        paint_bands(surface, 0..self.view.horizon, runner, &|band| {
            let rows = band.rows();
            let area = Rect::new(
                0,
                i32::try_from(rows.start).unwrap_or(i32::MAX),
                width,
                rows.end - rows.start,
            );
            self.draw(band, area);
        });
    }

    /// Draw both ranges onto `canvas`, which holds `area` of the screen and
    /// no row at or below the horizon.
    pub(super) fn draw(&self, canvas: &mut impl Canvas, area: Rect) {
        let above = Rect::new(0, 0, self.view.width, self.view.horizon);
        let area = area.intersection(&above);
        if area.is_empty() {
            return;
        }
        let mut scratch = ScanScratch::new();
        for range in 0..2 {
            for row in (0..self.rows).rev() {
                for column in (0..self.columns).rev() {
                    self.cell(canvas, area, (range, column, row), &mut scratch);
                }
            }
        }
    }

    /// Draw one cell of `range`, `column` cells out and `row` cells ahead:
    /// its farther face and then its nearer, then every edge whose nearer face
    /// this cell holds. The innermost cells' inner edges lie along the
    /// valley's floor, which both ranges share and neither raises, so they are
    /// not drawn.
    fn cell(
        &self,
        canvas: &mut impl Canvas,
        area: Rect,
        (range, column, row): (usize, usize, usize),
        scratch: &mut ScanScratch,
    ) {
        let vertex = |column: usize, row: usize| {
            let at = (range * (self.rows + 1) + row) * (self.columns + 1) + column;
            self.vertices.get(at).copied().unwrap_or_default()
        };
        let inner_near = vertex(column, row);
        let outer_near = vertex(column + 1, row);
        let inner_far = vertex(column, row + 1);
        let outer_far = vertex(column + 1, row + 1);
        let corners = [inner_near, outer_near, inner_far, outer_far];
        if !self.reaches(area, &corners) {
            return;
        }
        let side = if range == 0 { -1.0 } else { 1.0 };
        // The diagonal joins the higher pair, so a ridge runs along it; of the
        // two faces it makes, the one nearer in both directions is drawn last.
        let rising = inner_near.height + outer_far.height >= outer_near.height + inner_far.height;
        let diagonal = if rising {
            face(canvas, [inner_near, outer_far, inner_far], side, scratch);
            face(canvas, [inner_near, outer_near, outer_far], side, scratch);
            (inner_near, outer_far)
        } else {
            face(canvas, [outer_near, outer_far, inner_far], side, scratch);
            face(canvas, [inner_near, outer_near, inner_far], side, scratch);
            (outer_near, inner_far)
        };
        self.edge(canvas, (inner_far, outer_far), scratch);
        self.edge(canvas, (outer_near, outer_far), scratch);
        self.edge(canvas, diagonal, scratch);
        if row == 0 {
            self.edge(canvas, (inner_near, outer_near), scratch);
        }
    }

    /// Whether any of a cell with `corners` can reach a pixel of `area`.
    fn reaches(&self, area: Rect, corners: &[Vertex; 4]) -> bool {
        let reach = GLOW_WIDTH * self.view.pixel;
        let (mut left, mut top, mut right, mut bottom) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for corner in corners {
            left = mathf::fmin(left, corner.at.0);
            right = mathf::fmax(right, corner.at.0);
            top = mathf::fmin(top, corner.at.1);
            bottom = mathf::fmax(bottom, corner.at.1);
        }
        let (width, height) = (self.view.width, self.view.height);
        let (x, y) = (
            column(mathf::floor(left - reach), width),
            column(mathf::floor(top - reach), height),
        );
        let (x1, y1) = (
            column(mathf::ceil(right + reach), width),
            column(mathf::ceil(bottom + reach), height),
        );
        let bounds = Rect::new(
            i32::try_from(x).unwrap_or(i32::MAX),
            i32::try_from(y).unwrap_or(i32::MAX),
            x1.saturating_sub(x),
            y1.saturating_sub(y),
        );
        !bounds.intersection(&area).is_empty()
    }

    /// Draw the edge between `ends`: its glow, then its core.
    fn edge(&self, canvas: &mut impl Canvas, ends: (Vertex, Vertex), scratch: &mut ScanScratch) {
        let (from, to) = ends;
        let haze = haze(
            mathf::fmin(from.rise, to.rise),
            f64::midpoint(from.ground.1, to.ground.1),
        );
        let colour = EDGE.mix(EDGE_HAZE, haze);
        let glow = translucent(colour, GLOW_ALPHA);
        let pixel = self.view.pixel;
        for (width, colour) in [(GLOW_WIDTH, glow), (EDGE_WIDTH, opaque(colour))] {
            if let Some(band) = band(from.at, to.at, width * pixel / 2.0) {
                canvas.fill_polygon_subpixel(&band, colour, scratch);
            }
        }
    }
}

impl Peak {
    /// Peak `index` of a range's [`PEAKS`], spread across the screen in its
    /// own share of [`PEAK_SPREAD`] and at a depth of its own, on a screen
    /// whose height is `tall` of its lens's focal lengths.
    fn scattered(index: usize, tall: f64, rng: &mut NonCryptoRng) -> Self {
        let (least, most) = PEAK_SPREAD;
        let share = (index_f64(index) + rng.next_f64()) / index_f64(PEAKS);
        let angle = least + (most - least) * share;
        let depth = NEAR + CELL + rng.next_f64() * (FAR - NEAR - 2.0 * CELL);
        let within = |(least, most): (f64, f64), rng: &mut NonCryptoRng| {
            least + (most - least) * rng.next_f64()
        };
        Self {
            ground: (angle * depth * tall, depth),
            radius: within(PEAK_WIDTH, rng) * depth,
            height: within(PEAK_HEIGHT, rng),
        }
    }
}

/// The height the envelope allows `angle` screen heights across from the
/// centre line, in screen heights.
fn envelope(angle: f64) -> f64 {
    TALLEST * (1.0 - mathf::exp(-mathf::fmax(angle - CLEAR, 0.0) / RISE))
}

/// The share of the envelope the tallest of `peaks` reaches at `lateral`
/// cells across and `depth` ahead: each a cone, steepening towards its summit.
fn ridge(peaks: &[Peak], lateral: f64, depth: f64) -> f64 {
    peaks.iter().fold(0.0, |tallest, peak| {
        let apart = mathf::hypot(lateral - peak.ground.0, depth - peak.ground.1) / peak.radius;
        if apart >= 1.0 {
            return tallest;
        }
        let near = 1.0 - apart;
        mathf::fmax(tallest, peak.height * near * mathf::sqrt(near))
    })
}

/// How much of the haze something `rise` screen heights above the horizon,
/// `depth` cells ahead, takes.
fn haze(rise: f64, depth: f64) -> f64 {
    let low = HEIGHT_HAZE * mathf::exp(-mathf::fmax(rise, 0.0) / HAZE_RISE);
    let far = DEPTH_HAZE * mathf::clamp((depth - NEAR) / (FAR - NEAR), 0.0, 1.0);
    1.0 - (1.0 - low) * (1.0 - far)
}

/// Fill a face with `corners`, of the range on `side` of the centre line.
fn face(canvas: &mut impl Canvas, corners: [Vertex; 3], side: f64, scratch: &mut ScanScratch) {
    let rise = corners.iter().map(|corner| corner.rise).sum::<f64>() / 3.0;
    let depth = corners.iter().map(|corner| corner.ground.1).sum::<f64>() / 3.0;
    let light = 1.0 + FACE_LIGHT * facing(corners, side);
    let colour = FACE.mix(FACE_HAZE, haze(rise, depth)) * light;
    let points = corners.map(|corner| sub(corner.at));
    canvas.fill_polygon_subpixel(&points, opaque(colour), scratch);
}

/// How squarely a face with `corners`, of the range on `side` of the centre
/// line, turns towards the valley: `1.0` facing it, `-1.0` facing away.
fn facing(corners: [Vertex; 3], side: f64) -> f64 {
    let [first, second, third] =
        corners.map(|corner| (corner.ground.0, corner.height, corner.ground.1));
    let (along, aside) = (
        (second.0 - first.0, second.1 - first.1, second.2 - first.2),
        (third.0 - first.0, third.1 - first.1, third.2 - first.2),
    );
    let normal = (
        along.1 * aside.2 - along.2 * aside.1,
        along.2 * aside.0 - along.0 * aside.2,
        along.0 * aside.1 - along.1 * aside.0,
    );
    let length = mathf::sqrt(normal.0 * normal.0 + normal.1 * normal.1 + normal.2 * normal.2);
    if length <= 0.0 {
        return 0.0;
    }
    // Upward, whichever way round the corners wind.
    let up = if normal.1 < 0.0 { -1.0 } else { 1.0 };
    mathf::clamp(-side * up * normal.0 / length, -1.0, 1.0)
}

/// The quad a line `half` pixels either side of the segment `from`–`to`
/// fills, in sub-pixels; `None` for a segment with no length.
fn band(from: (f64, f64), to: (f64, f64), half: f64) -> Option<[(i32, i32); 4]> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = mathf::hypot(dx, dy);
    if length <= f64::EPSILON {
        return None;
    }
    let (ox, oy) = (-dy / length * half, dx / length * half);
    Some([
        sub((from.0 + ox, from.1 + oy)),
        sub((from.0 - ox, from.1 - oy)),
        sub((to.0 - ox, to.1 - oy)),
        sub((to.0 + ox, to.1 + oy)),
    ])
}

/// A point in pixels in the scan converter's sub-pixel units.
fn sub((x, y): (f64, f64)) -> (i32, i32) {
    let unit = f64::from(SUBPIXEL);
    (mathf::round_i32(x * unit), mathf::round_i32(y * unit))
}

/// `light` as an opaque colour.
fn opaque(light: Rgb) -> Color {
    let pixel = light.pixel(0.5);
    Color::rgb(pixel.r, pixel.g, pixel.b)
}

/// `light` as a colour `alpha` opaque.
fn translucent(light: Rgb, alpha: f64) -> Color {
    let pixel = light.pixel(0.5);
    let alpha = u8::try_from(mathf::round_i32(mathf::clamp(alpha, 0.0, 1.0) * 255.0)).unwrap_or(0);
    Color::rgba(pixel.r, pixel.g, pixel.b, alpha)
}

/// How many whole cells `span` cells needs, at the least one.
fn cells(span: f64) -> usize {
    usize::try_from(mathf::round_i32(mathf::ceil(span / CELL)).max(1)).unwrap_or(1)
}

/// A small count as a float.
fn index_f64(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

#[cfg(test)]
#[path = "mountains_tests.rs"]
mod tests;
