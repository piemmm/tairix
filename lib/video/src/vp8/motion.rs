//! Inter prediction (RFC 6386 §18): a macroblock predicted from a reference
//! frame through its motion vectors.
//!
//! A reference reads past its edges as its nearest edge sample, however far a
//! vector reaches, which is what the format's border extension gives with no
//! border copied round each reference: a block whose filters stay inside the
//! reference reads it in place, and only one crossing an edge is gathered,
//! clamped, into scratch first.
//!
//! A fraction is interpolated in two passes, across and then down, each
//! rounded and clamped to pixel range as the reference decoder does. A pass
//! whose fraction is nought is a copy, which is all its taps would do.

use super::{clamp_u8, Plane};

/// Samples one filter weighs.
const TAPS: usize = 6;

/// How far before the sample a vector lands on a filter starts reading, and
/// how much wider than its block it reads.
const REACH: (isize, usize) = (2, TAPS - 1);

/// What a filter's weighted sum is rounded by before it is scaled back.
const ROUNDING: i32 = 64;

/// The power of two every filter's weights sum to.
const FILTER_SHIFT: u32 = 7;

/// The largest block predicted at once: a macroblock's luma.
const MAX_BLOCK: usize = 16;

/// Rows and columns the largest block's filters read.
const SPAN: usize = MAX_BLOCK + TAPS - 1;

/// The widest and tallest plane a frame decodes to: fourteen bits of
/// dimension, rounded up to whole macroblocks.
const MAX_SIDE: usize = 1 << 14;

/// The six-tap filters version 0 interpolates with, one per eighth-pel phase
/// (RFC 6386 §18.3).
const SIX_TAP: [[i32; TAPS]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

/// The bilinear filters versions 1 and 2 interpolate with, in the six-tap
/// form so one pass serves both: only the sample landed on and the next
/// weigh.
const BILINEAR: [[i32; TAPS]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, 0, 112, 16, 0, 0],
    [0, 0, 96, 32, 0, 0],
    [0, 0, 80, 48, 0, 0],
    [0, 0, 64, 64, 0, 0],
    [0, 0, 48, 80, 0, 0],
    [0, 0, 32, 96, 0, 0],
    [0, 0, 16, 112, 0, 0],
];

/// The luma subblocks of each quarter of a macroblock, in raster order, whose
/// vectors one chroma subblock averages.
const QUARTERS: [[usize; 4]; 4] = [[0, 1, 4, 5], [2, 3, 6, 7], [8, 9, 12, 13], [10, 11, 14, 15]];

/// A motion vector, in eighth-pel units of the plane it moves. A luma vector
/// is the bitstream's quarter-pel one doubled, as the format stores it, so
/// both planes interpolate at one resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Mv {
    /// Rightward.
    pub(super) x: i16,
    /// Downward.
    pub(super) y: i16,
}

/// The vectors an inter-coded macroblock moves by.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Motion {
    /// One vector for the whole macroblock.
    Whole(Mv),
    /// One vector for each luma subblock, in raster order.
    Split([Mv; 16]),
}

/// How a frame interpolates between whole samples, as its version names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Interpolation {
    /// Version 0.
    SixTap,
    /// Versions 1 and 2.
    Bilinear,
    /// Version 3, whose chroma vectors drop their fractions. Its luma
    /// interpolates as versions 1 and 2 do, as the reference decoder has it,
    /// though an encoder of the version codes only whole luma vectors.
    FullPixel,
}

impl Interpolation {
    /// The interpolation `version` names, or `None` for one the format
    /// reserves.
    pub(super) const fn of_version(version: u32) -> Option<Self> {
        match version {
            0 => Some(Self::SixTap),
            1 | 2 => Some(Self::Bilinear),
            3 => Some(Self::FullPixel),
            _ => None,
        }
    }

    /// How luma interpolates, which under version 3 is as under versions 1
    /// and 2.
    const fn luma(self) -> Self {
        match self {
            Self::FullPixel => Self::Bilinear,
            other => other,
        }
    }

    /// The taps the eighth-pel fraction of `component` weighs with, or `None`
    /// where it lands on a whole sample or the version drops fractions.
    fn taps(self, component: i16) -> Option<&'static [i32; TAPS]> {
        let filters = match self {
            Self::SixTap => &SIX_TAP,
            Self::Bilinear => &BILINEAR,
            Self::FullPixel => return None,
        };
        match usize::try_from(component & 7) {
            Ok(0) | Err(_) => None,
            Ok(phase) => filters.get(phase),
        }
    }
}

/// A reference frame's plane as decoded, whole macroblocks included, which
/// prediction only reads. It is a view of its own rather than a [`Plane`]: a
/// reconstruction plane holds a window of rows bordered by the values the
/// intra modes invent, where a reference repeats its own edge.
#[derive(Clone, Copy)]
pub(super) struct RefPlane<'a> {
    samples: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

impl<'a> RefPlane<'a> {
    /// A view of `width`×`height` samples, rows `stride` apart from the start
    /// of `samples`, or `None` where `samples` does not hold that geometry or
    /// it passes the largest plane a frame decodes to.
    pub(super) fn new(
        samples: &'a [u8],
        stride: usize,
        width: usize,
        height: usize,
    ) -> Option<Self> {
        if width == 0 || width > MAX_SIDE || height > MAX_SIDE || stride < width {
            return None;
        }
        let last_row = height.checked_sub(1)?;
        let needed = last_row.checked_mul(stride)?.checked_add(width)?;
        (needed <= samples.len()).then_some(Self {
            samples,
            stride,
            width,
            height,
        })
    }

    /// Row `y`, clamped into the plane.
    fn row(&self, y: isize) -> &'a [u8] {
        let last = self.height.saturating_sub(1);
        let start = usize::try_from(y).map_or(0, |y| y.min(last)) * self.stride;
        let end = start + self.width;
        self.samples.get(start..end).unwrap_or_default()
    }

    /// The `size` samples from `at` on, where they lie wholly inside the plane.
    fn inside(&self, at: (isize, isize), size: (usize, usize)) -> Option<Rows<'a>> {
        let (x, y) = (usize::try_from(at.0).ok()?, usize::try_from(at.1).ok()?);
        if x + size.0 > self.width || y + size.1 > self.height {
            return None;
        }
        let samples = self.samples.get(y * self.stride + x..)?;
        Some(Rows {
            samples,
            stride: self.stride,
        })
    }
}

/// Predict one inter-coded macroblock from `reference`, the luma, blue and
/// red chroma planes of the frame it refers to, into `dst`, those of the frame
/// being reconstructed. `at` is the macroblock's column and row in the frame,
/// and `top` the luma sample row of `dst` its top lands on, its chroma's half
/// that: its frame row's where `dst` holds the frame.
pub(super) fn predict_inter(
    reference: &[RefPlane<'_>; 3],
    motion: &Motion,
    interpolation: Interpolation,
    (column, row): (usize, usize),
    dst: [&mut Plane; 3],
    top: isize,
) {
    let (Some(x), Some(y)) = (origin(column), origin(row)) else {
        return;
    };
    let [luma_ref, blue_ref, red_ref] = reference;
    let [luma_dst, blue_dst, red_dst] = dst;
    let mut luma = Site {
        reference: luma_ref,
        plane: luma_dst,
        from: (x, y),
        to: (x, top),
    };
    let (from, to) = ((x / 2, y / 2), (x / 2, top / 2));
    let mut blue = Site {
        reference: blue_ref,
        plane: blue_dst,
        from,
        to,
    };
    let mut red = Site {
        reference: red_ref,
        plane: red_dst,
        from,
        to,
    };
    let mut scratch = Scratch::new();
    match motion {
        Motion::Whole(mv) => {
            luma.predict((0, 0), 16, *mv, interpolation.luma(), &mut scratch);
            let chroma = chroma_mv([*mv; 4]);
            for site in [&mut blue, &mut red] {
                site.predict((0, 0), 8, chroma, interpolation, &mut scratch);
            }
        }
        Motion::Split(mvs) => {
            for (quarter, blocks) in (0..).zip(&QUARTERS) {
                let corner = ((quarter % 2) * 8, (quarter / 2) * 8);
                let vectors = blocks.map(|block| mvs[block]);
                if vectors.iter().all(|&mv| mv == vectors[0]) {
                    luma.predict(corner, 8, vectors[0], interpolation.luma(), &mut scratch);
                } else {
                    for (sub, mv) in (0..).zip(vectors) {
                        let offset = (corner.0 + (sub % 2) * 4, corner.1 + (sub / 2) * 4);
                        luma.predict(offset, 4, mv, interpolation.luma(), &mut scratch);
                    }
                }
                let chroma = chroma_mv(vectors);
                let half = (corner.0 / 2, corner.1 / 2);
                for site in [&mut blue, &mut red] {
                    site.predict(half, 4, chroma, interpolation, &mut scratch);
                }
            }
        }
    }
}

/// Where a block read past a reference's edges is gathered: one is enough
/// for every candidate a search weighs.
#[cfg(any(test, feature = "encode"))]
pub(super) type EdgeScratch = [u8; SPAN * SPAN];

/// A fresh [`EdgeScratch`].
#[cfg(any(test, feature = "encode"))]
pub(super) const EDGE_SCRATCH: EdgeScratch = [0; SPAN * SPAN];

/// What the 16×16 luma block of `reference` from `at`, read past its edges as
/// prediction reads it, misses `wanted` by: their absolute differences, summed.
/// A block past the edges is gathered in `edge`.
#[cfg(any(test, feature = "encode"))]
pub(super) fn luma_sad(
    reference: &RefPlane<'_>,
    at: (isize, isize),
    wanted: &[u8; MAX_BLOCK * MAX_BLOCK],
    edge: &mut EdgeScratch,
) -> u32 {
    let block = source(reference, at, (MAX_BLOCK, MAX_BLOCK), edge);
    let mut total = 0;
    for (row, line) in wanted.as_chunks::<MAX_BLOCK>().0.iter().enumerate() {
        let start = row * block.stride;
        let made = block
            .samples
            .get(start..start + MAX_BLOCK)
            .unwrap_or_default();
        for (&made, &asked) in made.iter().zip(line) {
            total += u32::from(made.abs_diff(asked));
        }
    }
    total
}

/// One plane of a macroblock being predicted: the reference it reads and the
/// plane it writes, and where the macroblock lies in each.
struct Site<'p, 'r> {
    reference: &'p RefPlane<'r>,
    plane: &'p mut Plane,
    from: (isize, isize),
    to: (isize, isize),
}

impl Site<'_, '_> {
    /// Predict the `size`×`size` block `offset` samples into the macroblock,
    /// moved by `mv`.
    fn predict(
        &mut self,
        offset: (isize, isize),
        size: usize,
        mv: Mv,
        interpolation: Interpolation,
        scratch: &mut Scratch,
    ) {
        let across = interpolation.taps(mv.x);
        let down = interpolation.taps(mv.y);
        let (left, wide) = across.map_or((0, 0), |_| REACH);
        let (above, tall) = down.map_or((0, 0), |_| REACH);
        let x = self.from.0 + offset.0 + isize::from(mv.x >> 3) - left;
        let y = self.from.1 + offset.1 + isize::from(mv.y >> 3) - above;
        let span = (size + wide, size + tall);
        let src = source(self.reference, (x, y), span, &mut scratch.edge);
        let to = (self.to.0 + offset.0, self.to.1 + offset.1);
        let mut out = rows_of(self.plane, to);
        match (across, down) {
            (None, None) => copy_block(src, size, &mut out),
            (Some(taps), None) => filter_pass(src, 1, taps, (size, size), &mut out),
            (None, Some(taps)) => filter_pass(src, src.stride, taps, (size, size), &mut out),
            (Some(across), Some(down)) => {
                let mut middle = RowsMut {
                    samples: &mut scratch.middle,
                    stride: size,
                };
                filter_pass(src, 1, across, (size, size + tall), &mut middle);
                let middle = Rows {
                    samples: &scratch.middle,
                    stride: size,
                };
                filter_pass(middle, size, down, (size, size), &mut out);
            }
        }
    }
}

/// The stack one macroblock's prediction works in.
struct Scratch {
    /// Source samples gathered from across an edge.
    edge: [u8; SPAN * SPAN],
    /// The rows the pass across leaves for the pass down.
    middle: [u8; MAX_BLOCK * SPAN],
}

impl Scratch {
    /// Scratch for one macroblock.
    const fn new() -> Self {
        Self {
            edge: [0; SPAN * SPAN],
            middle: [0; MAX_BLOCK * SPAN],
        }
    }
}

/// Sample rows `stride` apart, read from the first.
#[derive(Clone, Copy)]
struct Rows<'a> {
    samples: &'a [u8],
    stride: usize,
}

/// Sample rows `stride` apart, written from the first.
struct RowsMut<'a> {
    samples: &'a mut [u8],
    stride: usize,
}

/// Where macroblock `index` starts in luma samples, or `None` past the
/// largest plane a frame decodes to.
fn origin(index: usize) -> Option<isize> {
    let start = index.checked_mul(16).filter(|&start| start <= MAX_SIDE)?;
    isize::try_from(start).ok()
}

/// The `size` source samples from `at` on: the reference's own rows where
/// they lie wholly inside it, else gathered into `edge` with every coordinate
/// past an edge clamped to it.
fn source<'a>(
    reference: &RefPlane<'a>,
    at: (isize, isize),
    size: (usize, usize),
    edge: &'a mut [u8; SPAN * SPAN],
) -> Rows<'a> {
    if let Some(rows) = reference.inside(at, size) {
        return rows;
    }
    let last = reference.width.saturating_sub(1);
    for (y, line) in (at.1..).zip(edge.as_chunks_mut::<SPAN>().0.iter_mut().take(size.1)) {
        let row = reference.row(y);
        for (x, sample) in (at.0..).zip(line.iter_mut().take(size.0)) {
            let column = usize::try_from(x).map_or(0, |column| column.min(last));
            *sample = row.get(column).copied().unwrap_or_default();
        }
    }
    Rows {
        samples: edge,
        stride: SPAN,
    }
}

/// One pass of a sub-pixel filter over `width`×`height` samples: each sample
/// written weighs six of `src`, `step` apart from its own place on, and is
/// rounded and clamped to pixel range as the format does after each pass.
fn filter_pass(
    src: Rows<'_>,
    step: usize,
    taps: &[i32; TAPS],
    (width, height): (usize, usize),
    out: &mut RowsMut<'_>,
) {
    for row in 0..height {
        let mut sums = [ROUNDING; MAX_BLOCK];
        for (offset, &weight) in taps.iter().enumerate() {
            if weight == 0 {
                continue;
            }
            let start = row * src.stride + offset * step;
            let Some(samples) = src.samples.get(start..start + width) else {
                return;
            };
            for (sum, &sample) in sums.iter_mut().zip(samples) {
                *sum += weight * i32::from(sample);
            }
        }
        let start = row * out.stride;
        let Some(line) = out.samples.get_mut(start..start + width) else {
            return;
        };
        for (sample, &sum) in line.iter_mut().zip(&sums) {
            *sample = clamp_u8(sum >> FILTER_SHIFT);
        }
    }
}

/// Copy a `size`×`size` block from `src` to `out`.
fn copy_block(src: Rows<'_>, size: usize, out: &mut RowsMut<'_>) {
    for row in 0..size {
        let (from, to) = (row * src.stride, row * out.stride);
        let samples = src.samples.get(from..from + size);
        let line = out.samples.get_mut(to..to + size);
        let (Some(samples), Some(line)) = (samples, line) else {
            return;
        };
        line.copy_from_slice(samples);
    }
}

/// `plane`'s rows from sample `at` on, to be written.
fn rows_of(plane: &mut Plane, (x, y): (isize, isize)) -> RowsMut<'_> {
    let start = plane.at(x, y);
    let stride = plane.stride;
    RowsMut {
        samples: plane.samples.get_mut(start..).unwrap_or_default(),
        stride,
    }
}

/// The vector a chroma subblock moves by: the four luma subblock vectors it
/// covers averaged, halved into the chroma plane and rounded half away from
/// zero. A whole macroblock's is its one vector four times, so halved.
fn chroma_mv(luma: [Mv; 4]) -> Mv {
    let average = |sum: i32| {
        let rounded = if sum < 0 { sum - 4 } else { sum + 4 };
        i16::try_from(rounded / 8).unwrap_or_default()
    };
    Mv {
        x: average(luma.iter().map(|mv| i32::from(mv.x)).sum()),
        y: average(luma.iter().map(|mv| i32::from(mv.y)).sum()),
    }
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
