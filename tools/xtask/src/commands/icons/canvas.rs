//! The canvas a master is drawn on: coverage masks filled by the one scan
//! converter, the light and shadow every surface is finished with, and the
//! area-averaged reduction to the master.
//!
//! Drawing is in units, a master being [`UNITS`] across; the canvas holds
//! [`SCALE`] pixels to the unit, so a bevel or a shadow is worked out finer
//! than the master shows it. Only IEEE arithmetic and `tairix_util::mathf` are
//! used, so a master is the same bytes on every host and the drift check can
//! hold the committed file to it.

use tairix_raster::{Coverage, FillRule, ScanScratch};
use tairix_util::mathf;

/// A master's side, in units.
pub const UNITS: u16 = 256;

/// Canvas pixels to a unit.
pub const SCALE: u16 = 4;

/// The canvas side, in pixels.
pub const SIDE: usize = UNITS as usize * SCALE as usize;

/// Sub-pixels a vertex is placed to.
const SUBPIXELS: u32 = 64;

/// The direction light falls from: up and to the left, the same for every
/// surface of every master.
const LIGHT: (f32, f32) = (-0.55, -0.83);

/// A colour, each channel 0 to 1.
pub type Rgb = [f32; 3];

/// `0xRRGGBB` as a colour.
#[must_use]
pub fn rgb(hex: u32) -> Rgb {
    let channel = |shift: u32| f32::from(u8::try_from((hex >> shift) & 0xFF).unwrap_or(0)) / 255.0;
    [channel(16), channel(8), channel(0)]
}

/// `colour` with each channel multiplied by `factor`, held below white.
#[must_use]
pub fn scaled(colour: Rgb, factor: f32) -> Rgb {
    colour.map(|c| (c * factor).min(1.0))
}

/// The unit coordinate of the centre of canvas pixel `index`.
fn unit_of(index: usize) -> f32 {
    // A canvas index is far below 2^24, so it is exact in an f32.
    #[allow(clippy::cast_precision_loss)]
    let at = index as f32;
    (at + 0.5) / f32::from(SCALE)
}

/// `unit` (0 to 1) as a byte, rounded.
fn byte(unit: f32) -> u8 {
    let scaled = (unit.clamp(0.0, 1.0) * 255.0).round();
    // The clamp keeps it within 0..=255, so the conversion is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = scaled as u8;
    byte
}

/// A closed outline, in units.
pub type Contour = Vec<(f64, f64)>;

/// How much of each canvas pixel a shape covers, 0 to 1.
#[derive(Clone)]
pub struct Mask(Vec<f32>);

impl Mask {
    /// Nothing covered.
    #[must_use]
    pub fn empty() -> Self {
        Self(vec![0.0; SIDE * SIDE])
    }

    /// The area `contours` enclose, by the non-zero rule, at exact coverage.
    #[must_use]
    pub fn of(contours: &[Contour]) -> Self {
        let per_unit = f64::from(SCALE) * f64::from(SUBPIXELS);
        let placed: Vec<Vec<(i32, i32)>> = contours
            .iter()
            .map(|contour| {
                contour
                    .iter()
                    .map(|&(x, y)| {
                        (
                            mathf::round_i32(x * per_unit),
                            mathf::round_i32(y * per_unit),
                        )
                    })
                    .collect()
            })
            .collect();
        let mut mask = Self::empty();
        let mut scratch = ScanScratch::new();
        let Some(mut rows) =
            scratch.coverage_rows(&placed, SUBPIXELS, FillRule::NonZero, Coverage::Area)
        else {
            return mask;
        };
        let side = u32::from(UNITS) * u32::from(SCALE);
        let Some((x0, x1, y0, y1)) = rows.bounds((0, 0, side, side)) else {
            return mask;
        };
        let (first, width) = (x0 as usize, (x1 - x0) as usize);
        let mut line = vec![0u8; width];
        for y in y0..y1 {
            let covered = rows.row(y, x0, &mut line);
            let row = &mut mask.0[y as usize * SIDE + first..][..width];
            for (out, &c) in row[covered.clone()].iter_mut().zip(&line[covered]) {
                *out = f32::from(c) / 255.0;
            }
        }
        mask
    }

    /// Each pixel's coverage.
    #[cfg(test)]
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.0
    }

    /// Covered by either.
    #[must_use]
    pub fn union(mut self, other: &Self) -> Self {
        for (a, &b) in self.0.iter_mut().zip(&other.0) {
            *a = a.max(b);
        }
        self
    }

    /// Covered by this and not by `other`.
    #[must_use]
    pub fn minus(mut self, other: &Self) -> Self {
        for (a, &b) in self.0.iter_mut().zip(&other.0) {
            *a = (*a - b).max(0.0);
        }
        self
    }

    /// Covered by both.
    #[must_use]
    pub fn within(mut self, other: &Self) -> Self {
        for (a, &b) in self.0.iter_mut().zip(&other.0) {
            *a *= b;
        }
        self
    }

    /// This shape moved `dx`, `dy` units, the pixels it leaves uncovered.
    #[must_use]
    pub fn shifted(&self, dx: f64, dy: f64) -> Self {
        let step = |d: f64| mathf::round_i32(d * f64::from(SCALE));
        let (dx, dy) = (step(dx), step(dy));
        let mut out = Self::empty();
        for y in 0..SIDE {
            let Some(from_y) = offset(y, dy) else {
                continue;
            };
            for x in 0..SIDE {
                if let Some(from_x) = offset(x, dx) {
                    out.0[y * SIDE + x] = self.0[from_y * SIDE + from_x];
                }
            }
        }
        out
    }

    /// This shape softened by a Gaussian of `sigma` units.
    #[must_use]
    pub fn blurred(&self, sigma: f32) -> Self {
        Self(blur(&self.0, sigma * f32::from(SCALE)))
    }

    /// The band `width` units wide just outside this shape.
    #[must_use]
    pub fn outline(&self, width: f32) -> Self {
        let mut grown = self.blurred(width * 0.5);
        for (g, &m) in grown.0.iter_mut().zip(&self.0) {
            *g = ((*g * 2.2).min(1.0) - m).max(0.0);
        }
        grown
    }
}

/// Where canvas index `at` reads from in a shape moved by `by` pixels.
fn offset(at: usize, by: i32) -> Option<usize> {
    let from = i64::try_from(at).ok()? - i64::from(by);
    usize::try_from(from).ok().filter(|&from| from < SIDE)
}

/// `values`, `SIDE` square, blurred by a Gaussian of `sigma` canvas pixels:
/// three box passes each way, which come within a few per cent of it. Only
/// the window the passes can carry what is covered into is worked.
fn blur(values: &[f32], sigma: f32) -> Vec<f32> {
    let radius = box_radius(sigma);
    let mut out = vec![0.0f32; SIDE * SIDE];
    let Some((x0, y0, x1, y1)) = reach(values, 3 * radius + 1) else {
        return out;
    };
    let (width, height) = (x1 - x0, y1 - y0);
    let mut window: Vec<f32> = values
        .as_chunks::<SIDE>()
        .0
        .iter()
        .skip(y0)
        .take(height)
        .flat_map(|row| row[x0..x1].iter().copied())
        .collect();
    passes(&mut window, width, radius);
    let mut columns = transposed(&window, width, height);
    passes(&mut columns, height, radius);
    let window = transposed(&columns, height, width);
    let rows = out.as_chunks_mut::<SIDE>().0.iter_mut().skip(y0);
    for (row, blurred) in rows.zip(window.chunks_exact(width)) {
        row[x0..x1].copy_from_slice(blurred);
    }
    out
}

/// The box holding every covered pixel of `values`, grown by `margin` and
/// held to the canvas: left, top, right, bottom, the last two exclusive.
fn reach(values: &[f32], margin: usize) -> Option<(usize, usize, usize, usize)> {
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    for (y, row) in values.as_chunks::<SIDE>().0.iter().enumerate() {
        let Some(first) = row.iter().position(|&v| v > 0.0) else {
            continue;
        };
        let last = row.iter().rposition(|&v| v > 0.0).unwrap_or(first);
        bounds = Some(bounds.map_or((first, y, last, y), |(l, t, r, _)| {
            (l.min(first), t, r.max(last), y)
        }));
    }
    bounds.map(|(l, t, r, b)| {
        (
            l.saturating_sub(margin),
            t.saturating_sub(margin),
            (r + 1 + margin).min(SIDE),
            (b + 1 + margin).min(SIDE),
        )
    })
}

/// Three box passes of half-width `radius` along each `width`-long row of
/// `values`.
fn passes(values: &mut [f32], width: usize, radius: usize) {
    let mut line = vec![0.0f32; width];
    let mut sums = vec![0.0f64; width + 1];
    for _ in 0..3 {
        for row in values.chunks_exact_mut(width) {
            line.copy_from_slice(row);
            box_pass(&line, radius, &mut sums, row);
        }
    }
}

/// `values`, `width` across and `height` down, with its rows and columns
/// exchanged.
fn transposed(values: &[f32], width: usize, height: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; width * height];
    for (y, row) in values.chunks_exact(width).enumerate() {
        for (x, &v) in row.iter().enumerate() {
            out[x * height + y] = v;
        }
    }
    out
}

/// The box half-width three passes of which approximate a Gaussian of
/// `sigma`.
fn box_radius(sigma: f32) -> usize {
    let r = (mathf::sqrt(4.0 * f64::from(sigma) * f64::from(sigma) + 1.0) - 1.0) / 2.0;
    usize::try_from(mathf::round_i32(r).max(1)).unwrap_or(1)
}

/// One box average of half-width `radius` along `line` into `out`, the
/// edge values held beyond the ends.
fn box_pass(line: &[f32], radius: usize, sums: &mut [f64], out: &mut [f32]) {
    sums[0] = 0.0;
    for (i, &v) in line.iter().enumerate() {
        sums[i + 1] = sums[i] + f64::from(v);
    }
    let last = line.len() - 1;
    let width = f64::from(u32::try_from(2 * radius + 1).unwrap_or(1));
    let first_value = f64::from(line[0]);
    let last_value = f64::from(line[last]);
    for (i, o) in out.iter_mut().enumerate() {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius).min(last);
        let before = f64::from(u32::try_from(radius.saturating_sub(i)).unwrap_or(0));
        let after = f64::from(u32::try_from((i + radius).saturating_sub(last)).unwrap_or(0));
        let total = sums[hi + 1] - sums[lo] + before * first_value + after * last_value;
        *o = mathf::single(total / width);
    }
}

/// What colour a surface is at each point.
#[derive(Copy, Clone, Debug)]
pub enum Paint {
    /// One colour.
    Solid(Rgb),
    /// From `start` at `from` to `end` at `to`, held beyond either.
    Linear {
        from: (f32, f32),
        to: (f32, f32),
        start: Rgb,
        end: Rgb,
    },
}

impl Paint {
    /// From `start` at unit point `from` to `end` at `to`.
    #[must_use]
    pub fn linear(from: (f64, f64), to: (f64, f64), start: Rgb, end: Rgb) -> Self {
        let point = |(x, y): (f64, f64)| (mathf::single(x), mathf::single(y));
        Self::Linear {
            from: point(from),
            to: point(to),
            start,
            end,
        }
    }

    /// Top to bottom, `top` at `y0` and `bottom` at `y1`.
    #[must_use]
    pub fn vertical(top: Rgb, bottom: Rgb, y0: f64, y1: f64) -> Self {
        Self::linear((0.0, y0), (0.0, y1), top, bottom)
    }

    /// The colour at unit point `(x, y)`.
    fn at(&self, x: f32, y: f32) -> Rgb {
        match *self {
            Self::Solid(colour) => colour,
            Self::Linear {
                from,
                to,
                start,
                end,
            } => {
                let (vx, vy) = (to.0 - from.0, to.1 - from.1);
                let length = vx * vx + vy * vy;
                let t = if length > 0.0 {
                    (((x - from.0) * vx + (y - from.1) * vy) / length).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                [0, 1, 2].map(|c| start[c] + (end[c] - start[c]) * t)
            }
        }
    }
}

/// How a raised surface takes the light: a bevel `width` units deep whose
/// lit side brightens toward white by up to `highlight` and whose far side
/// darkens by up to `shade`.
#[derive(Copy, Clone, Debug)]
pub struct Relief {
    pub width: f32,
    pub highlight: f32,
    pub shade: f32,
}

impl Relief {
    /// Flat: no bevel at all.
    pub const FLAT: Self = Self {
        width: 0.0,
        highlight: 0.0,
        shade: 0.0,
    };

    /// A bevel of `width` units at the set's usual strength.
    #[must_use]
    pub const fn of(width: f32) -> Self {
        Self {
            width,
            highlight: 0.55,
            shade: 0.45,
        }
    }
}

/// How much each pixel of `shape` faces the light, -1 to 1, under a bevel
/// `width` units deep.
fn facing(shape: &Mask, width: f32) -> Vec<f32> {
    let depth = width * f32::from(SCALE);
    let soft = blur(&shape.0, depth * 0.5);
    let gain = depth * 2.2;
    let mut out = vec![0.0f32; SIDE * SIDE];
    for y in 1..SIDE - 1 {
        for x in 1..SIDE - 1 {
            let at = y * SIDE + x;
            if shape.0[at] <= 0.0 {
                continue;
            }
            let gx = (soft[at + 1] - soft[at - 1]) * 0.5;
            let gy = (soft[at + SIDE] - soft[at - SIDE]) * 0.5;
            out[at] = (-(gx * LIGHT.0 + gy * LIGHT.1) * gain).clamp(-1.0, 1.0);
        }
    }
    out
}

/// The canvas: premultiplied colour and coverage per pixel.
pub struct Canvas(Vec<[f32; 4]>);

impl Canvas {
    /// Transparent throughout.
    #[must_use]
    pub fn new() -> Self {
        Self(vec![[0.0; 4]; SIDE * SIDE])
    }

    /// Lay `paint` over the canvas where `shape` covers it, at `opacity`,
    /// lit as `relief` says.
    pub fn paint(&mut self, shape: &Mask, paint: Paint, relief: Relief, opacity: f32) {
        let lit = (relief.width > 0.0).then(|| facing(shape, relief.width));
        for (at, (px, &cover)) in self.0.iter_mut().zip(&shape.0).enumerate() {
            let alpha = cover * opacity;
            if alpha <= 0.0 {
                continue;
            }
            let mut colour = paint.at(unit_of(at % SIDE), unit_of(at / SIDE));
            if let Some(lit) = &lit {
                let face = lit[at];
                let toward = face.max(0.0) * relief.highlight;
                let away = (-face).max(0.0) * relief.shade;
                colour = colour.map(|c| {
                    let bright = c + (1.0 - c) * toward;
                    bright - bright * away
                });
            }
            for c in 0..3 {
                px[c] = colour[c] * alpha + px[c] * (1.0 - alpha);
            }
            px[3] = alpha + px[3] * (1.0 - alpha);
        }
    }

    /// The soft shadow `shape` casts `dx`, `dy` units away, `softness` units
    /// blurred, at `opacity`.
    pub fn shadow(&mut self, shape: &Mask, dx: f64, dy: f64, softness: f32, opacity: f32) {
        let cast = shape.shifted(dx, dy).blurred(softness);
        self.paint(&cast, Paint::Solid([0.0; 3]), Relief::FLAT, opacity);
    }

    /// The master: each `SCALE`-square block of the canvas averaged into one
    /// straight-alpha RGBA8 pixel.
    #[must_use]
    pub fn master(&self) -> Vec<u8> {
        let units = usize::from(UNITS);
        let scale = usize::from(SCALE);
        let block = 1.0 / f32::from(SCALE * SCALE);
        let mut out = Vec::with_capacity(units * units * 4);
        for y in 0..units {
            for x in 0..units {
                let mut sum = [0.0f32; 4];
                for dy in 0..scale {
                    let row = (y * scale + dy) * SIDE + x * scale;
                    for px in &self.0[row..row + scale] {
                        for (total, &channel) in sum.iter_mut().zip(px) {
                            *total += channel;
                        }
                    }
                }
                let alpha = sum[3] * block;
                for &premultiplied in &sum[..3] {
                    out.push(if alpha > 0.0 {
                        byte(premultiplied * block / alpha)
                    } else {
                        0
                    });
                }
                out.push(byte(alpha));
            }
        }
        out
    }
}

/// A full turn, in radians.
const TURN: f64 = 2.0 * core::f64::consts::PI;

/// Chords an arc of `radius` units sweeping `sweep` radians is cut into: one
/// a canvas pixel or so long, never fewer than eight.
fn chords(radius: f64, sweep: f64) -> u32 {
    let length = radius.abs() * sweep.abs() * f64::from(SCALE);
    u32::try_from(mathf::round_i32(mathf::ceil(length)).max(8)).unwrap_or(8)
}

/// The points of an arc about `(cx, cy)` of `radius`, from angle `from` by
/// `sweep` radians, both ends included.
fn arc(cx: f64, cy: f64, radius: f64, from: f64, sweep: f64, into: &mut Contour) {
    let steps = chords(radius, sweep);
    for i in 0..=steps {
        let angle = from + sweep * f64::from(i) / f64::from(steps);
        into.push((
            cx + radius * mathf::cos(angle),
            cy + radius * mathf::sin(angle),
        ));
    }
}

/// A circle.
#[must_use]
pub fn circle(cx: f64, cy: f64, radius: f64) -> Contour {
    let mut out = Vec::new();
    arc(cx, cy, radius, 0.0, TURN, &mut out);
    out
}

/// A rectangle.
#[must_use]
pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Contour {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// A rectangle with corners rounded to `radius`.
#[must_use]
pub fn rounded(x0: f64, y0: f64, x1: f64, y1: f64, radius: f64) -> Contour {
    let r = radius.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    let quarter = TURN / 4.0;
    let mut out = Vec::new();
    arc(x1 - r, y0 + r, r, -quarter, quarter, &mut out);
    arc(x1 - r, y1 - r, r, 0.0, quarter, &mut out);
    arc(x0 + r, y1 - r, r, quarter, quarter, &mut out);
    arc(x0 + r, y0 + r, r, 2.0 * quarter, quarter, &mut out);
    out
}

/// A cubic Bézier from `p0` to `p3`, as points.
#[must_use]
pub fn cubic(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), p3: (f64, f64)) -> Contour {
    let steps = 200u32;
    (0..=steps)
        .map(|step| {
            let along = f64::from(step) / f64::from(steps);
            let rest = 1.0 - along;
            let weights = [
                rest * rest * rest,
                3.0 * rest * rest * along,
                3.0 * rest * along * along,
                along * along * along,
            ];
            let points = [p0, p1, p2, p3];
            let mix = |pick: fn((f64, f64)) -> f64| {
                weights
                    .iter()
                    .zip(points)
                    .map(|(w, p)| w * pick(p))
                    .sum::<f64>()
            };
            (mix(|p| p.0), mix(|p| p.1))
        })
        .collect()
}

/// Twice the signed area `contour` encloses.
fn signed_area(contour: &[(f64, f64)]) -> f64 {
    let mut sum = 0.0;
    for (i, &(x0, y0)) in contour.iter().enumerate() {
        let (x1, y1) = contour[(i + 1) % contour.len()];
        sum += x0 * y1 - x1 * y0;
    }
    sum
}

/// `contour` wound the way every shape here is, so pieces of one shape
/// filled together under the non-zero rule add rather than cancel.
#[must_use]
pub fn oriented(mut contour: Contour) -> Contour {
    if signed_area(&contour) < 0.0 {
        contour.reverse();
    }
    contour
}

/// The outline of a line `width` units thick along `points`, round at its
/// ends and its joins.
#[must_use]
pub fn stroke(points: &[(f64, f64)], width: f64) -> Vec<Contour> {
    let half = width / 2.0;
    let mut out = Vec::new();
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        let length = mathf::hypot(x1 - x0, y1 - y0);
        if length <= 0.0 {
            continue;
        }
        let (nx, ny) = (-(y1 - y0) / length * half, (x1 - x0) / length * half);
        out.push(oriented(vec![
            (x0 + nx, y0 + ny),
            (x1 + nx, y1 + ny),
            (x1 - nx, y1 - ny),
            (x0 - nx, y0 - ny),
        ]));
    }
    for &(x, y) in points {
        out.push(circle(x, y, half));
    }
    out
}
