//! Local adaptation: the range one exposure cannot hold compressed as a
//! photographer would, gently, and nowhere else.
//!
//! The meter's samples are splatted into a bilateral grid over the film and
//! log luminance (Chen, Paris and Durand, "Real-time Edge-aware Image
//! Processing with the Bilateral Grid", 2007) and blurred along each axis, so
//! each cell holds the mean log luminance of the like-lit ground about it: the
//! base layer of Durand and Dorsey's decomposition ("Fast Bilateral Filtering
//! for the Display of High-Dynamic-Range Images", 2002). A window and the dark
//! room about it fall in different layers, so neither haloes the other, and
//! the correction follows only the base, so detail within a region keeps its
//! contrast.
//!
//! A sample reads the grid at its own place and luminance. Where the base lies
//! within a stop and a half of the metered key nothing changes; beyond, a
//! highlight is drawn down by half its excess, never more than two stops, and
//! a deep shadow lifted, never more than one. A scene with no base that far
//! from its key keeps no grid, and is encoded exactly as without one.

use alloc::vec::Vec;

use tairix_util::{fallible, mathf};

use crate::vector::real;

/// How many measured points across the film one cell of the grid spans each
/// way: what the base layer is smooth over, about a sixteenth of the frame.
pub(crate) const SPATIAL: usize = 8;

/// The grid's layers of luminance, a stop apart, either side of the key: the
/// sun's disc and the blackest hollow are clamped into the outermost.
const REACH: f64 = 12.0;

/// Empty nodes about the grid's measured ones, which the blur spreads into.
const PAD: usize = 2;

/// How far either side of the key, in stops, a base leaves a sample as it is.
const HELD: f64 = 1.5;

/// How much of a base's excess past what is held a sample is corrected by, at
/// first; how wide, in stops, the correction eases in over; and the most it
/// draws a highlight down and lifts a shadow, in stops.
const SHARE: f64 = 0.5;
const KNEE: f64 = 0.5;
const MOST_DOWN: f64 = 2.0;
const MOST_UP: f64 = 1.0;

/// What a grid cell measured nothing of weighs against a sample there: about
/// one sample's worth, pulling an unmeasured base back to the key.
const PRIOR: f64 = 1.0;

/// One measured sample: where on the film it fell, each of `0.0..=1.0` across
/// and down, and its exposed log luminance in stops from the key.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Sample {
    pub(crate) across: f64,
    pub(crate) down: f64,
    pub(crate) stops: f64,
}

/// The grid a sample's correction is read from.
#[derive(Debug)]
pub(crate) struct Adaptation {
    /// The exposed luminance the metered key stands at.
    key: f64,
    /// Nodes across, down and through the layers.
    columns: usize,
    rows: usize,
    layers: usize,
    /// How many nodes one unit of the film spans, across and down.
    scale: (f64, f64),
    /// Each node's sum of the stops its samples measured, and their weight.
    nodes: Vec<[f64; 2]>,
}

impl Adaptation {
    /// The grid `samples` measure about the exposed luminance `key`, spread
    /// over `points` measured points across and down the film; `None` when
    /// the heap will not hold it.
    pub(crate) fn measured(samples: &[Sample], key: f64, points: (usize, usize)) -> Option<Self> {
        let span = |points: usize| points.div_ceil(SPATIAL) + 1 + 2 * PAD;
        let (columns, rows) = (span(points.0), span(points.1));
        let layers = whole(2.0 * REACH) + 1 + 2 * PAD;
        let mut grid = Self {
            key,
            columns,
            rows,
            layers,
            scale: (
                real(points.0) / real(SPATIAL),
                real(points.1) / real(SPATIAL),
            ),
            nodes: fallible::filled(columns.checked_mul(rows)?.checked_mul(layers)?, [0.0; 2])?,
        };
        for sample in samples {
            grid.splat(sample);
        }
        let mut scratch = fallible::filled(columns.max(rows).max(layers), [0.0; 2])?;
        for axis in [Axis::Across, Axis::Down, Axis::Through] {
            grid.blur(axis, &mut scratch);
        }
        Some(grid)
    }

    /// Whether any sample's correction could be other than none: whether any
    /// measured node's base lies past what is held either side of the key. A
    /// read is a weighed blend of the bases about it and the key, so none can
    /// lie further out than they do.
    pub(crate) fn corrects(&self) -> bool {
        self.nodes
            .iter()
            .any(|&[sum, weight]| weight > 0.0 && (sum / weight).abs() > HELD)
    }

    /// The factor the exposure of a sample at `film` — across and down the
    /// film, each in `0.0..=1.0` — of exposed luminance `luminance` is
    /// corrected by.
    pub(crate) fn factor(&self, (across, down): (f64, f64), luminance: f64) -> f64 {
        let base = self.base((across, down), stops(luminance, self.key));
        let correction = correction(base);
        if correction == 0.0 {
            1.0
        } else {
            mathf::exp(correction * core::f64::consts::LN_2)
        }
    }

    /// The base layer at `film` for a sample measuring `stops`: the mean of
    /// the like-lit ground about it, in stops from the key.
    fn base(&self, (across, down): (f64, f64), stops: f64) -> f64 {
        let at = self.place((across, down, stops));
        let (mut sum, mut weight) = (0.0, 0.0);
        for (node, share) in corners((self.columns, self.rows), at) {
            if let Some(&[s, w]) = self.nodes.get(node) {
                sum += s * share;
                weight += w * share;
            }
        }
        sum / (weight + PRIOR)
    }

    /// Add `sample` to the nodes about it, each by its share of the place.
    fn splat(&mut self, sample: &Sample) {
        let at = self.place((sample.across, sample.down, sample.stops));
        let stops = sample.stops.clamp(-REACH, REACH);
        for (node, share) in corners((self.columns, self.rows), at) {
            if let Some(slot) = self.nodes.get_mut(node) {
                slot[0] += stops * share;
                slot[1] += share;
            }
        }
    }

    /// Where `(across, down, stops)` lies among the nodes, clamped to the
    /// measured ones.
    fn place(&self, (across, down, stops): (f64, f64, f64)) -> [f64; 3] {
        let pad = real(PAD);
        let inside = |value: f64, nodes: usize| value.clamp(pad, real(nodes - 1 - PAD));
        [
            inside(pad + across * self.scale.0, self.columns),
            inside(pad + down * self.scale.1, self.rows),
            inside(pad + stops + REACH, self.layers),
        ]
    }

    /// Blur the nodes along `axis` by the binomial kernel 1 4 6 4 1, through
    /// `scratch`, as long as the longest axis.
    fn blur(&mut self, axis: Axis, scratch: &mut [[f64; 2]]) {
        let (length, stride, lines) = match axis {
            Axis::Across => (self.columns, 1, self.rows * self.layers),
            Axis::Down => (self.rows, self.columns, self.columns * self.layers),
            Axis::Through => (
                self.layers,
                self.columns * self.rows,
                self.columns * self.rows,
            ),
        };
        for line in 0..lines {
            let start = match axis {
                Axis::Across => line * self.columns,
                Axis::Down => {
                    (line / self.columns) * self.columns * self.rows + line % self.columns
                }
                Axis::Through => line,
            };
            for (index, out) in scratch.iter_mut().take(length).enumerate() {
                let mut held = [0.0; 2];
                for (offset, weight) in [(-2, 1.0), (-1, 4.0), (0, 6.0), (1, 4.0), (2, 1.0)] {
                    let Some(at) = index.checked_add_signed(offset).filter(|&at| at < length)
                    else {
                        continue;
                    };
                    if let Some(&[sum, mass]) = self.nodes.get(start + at * stride) {
                        held[0] += sum * weight / 16.0;
                        held[1] += mass * weight / 16.0;
                    }
                }
                *out = held;
            }
            for (index, value) in scratch.iter().take(length).enumerate() {
                if let Some(node) = self.nodes.get_mut(start + index * stride) {
                    *node = *value;
                }
            }
        }
    }
}

/// The eight nodes about `at` of a grid `columns` across and `rows` down,
/// each with its trilinear share of it.
fn corners(
    (columns, rows): (usize, usize),
    [x, y, z]: [f64; 3],
) -> impl Iterator<Item = (usize, f64)> {
    let split = |value: f64| {
        let low = mathf::floor(value);
        (whole(low), value - low)
    };
    let ((x0, fx), (y0, fy), (z0, fz)) = (split(x), split(y), split(z));
    (0..8usize).map(move |corner| {
        let (dx, dy, dz) = (corner & 1, (corner >> 1) & 1, corner >> 2);
        let pick = |d: usize, f: f64| if d == 0 { 1.0 - f } else { f };
        let node = ((z0 + dz) * rows + y0 + dy) * columns + x0 + dx;
        (node, pick(dx, fx) * pick(dy, fy) * pick(dz, fz))
    })
}

/// An axis of the grid.
#[derive(Copy, Clone)]
enum Axis {
    Across,
    Down,
    Through,
}

/// The correction, in stops, a base `base` stops from the key calls for:
/// nought within what is held, then easing in over the knee to half the
/// excess, and settling toward at most two stops down or one up.
fn correction(base: f64) -> f64 {
    let past = base.abs() - HELD;
    if past <= 0.0 {
        return 0.0;
    }
    let eased = if past < KNEE {
        past * past / (2.0 * KNEE)
    } else {
        past - 0.5 * KNEE
    };
    let most = if base > 0.0 { -MOST_DOWN } else { MOST_UP };
    most * tanh(SHARE * eased / most.abs())
}

/// How many stops exposed `luminance` lies from the exposed `key`.
pub(crate) fn stops(luminance: f64, key: f64) -> f64 {
    mathf::ln(luminance.max(1e-12) / key.max(1e-12)) / core::f64::consts::LN_2
}

/// The hyperbolic tangent of `x`, for `x` at or above nought.
fn tanh(x: f64) -> f64 {
    let e = mathf::exp(-2.0 * x);
    (1.0 - e) / (1.0 + e)
}

/// `value`, at or above nought and whole, as a count.
fn whole(value: f64) -> usize {
    usize::try_from(mathf::round_i32(value).max(0)).unwrap_or(0)
}

#[cfg(test)]
#[path = "adapt_tests.rs"]
mod tests;
