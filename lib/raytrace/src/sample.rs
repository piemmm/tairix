//! Where a pixel's samples fall, and the order the pixels are revealed in.
//!
//! Each sample draws its random numbers two at a time from an Owen-scrambled
//! Sobol sequence (Burley, "Practical Hash-based Owen Scrambling", JCGT 2020),
//! each pair scrambled and shuffled under its own seed, so a pixel's first 4,
//! 16 or 64 samples are each stratified in every pair. Everything is hashed
//! from the pixel and the sample index alone, so a pixel traces the same on
//! whichever core takes it.

use core::f64::consts::TAU;

use tairix_util::mathf;

/// The golden ratio, `(1 + √5) / 2`.
pub(crate) const GOLDEN_RATIO: f64 = 1.618_033_988_749_895;

/// The golden angle, `2π / φ²`: points turned this far apart spread evenly
/// round a circle however many there are.
pub(crate) const GOLDEN_ANGLE: f64 = 2.399_963_229_728_653;

/// A 32-bit integer finaliser with low bias (Wellons, "lowbias32").
pub(crate) const fn mix32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

/// The `SplitMix64` finaliser.
pub(crate) const fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// `bits` as a fraction in `0.0..1.0`.
pub(crate) fn unit(bits: u32) -> f64 {
    f64::from(bits) * (1.0 / 4_294_967_296.0)
}

/// Pairs drawn from the scrambled Sobol sequence; later pairs, deep in a path
/// where stratification buys little, are hashed outright.
const SOBOL_PAIRS: u32 = 16;

/// The second Sobol dimension's generator, a byte of the index at a time.
static SOBOL_1: [[u32; 256]; 4] = sobol_1_tables();

const fn sobol_1_tables() -> [[u32; 256]; 4] {
    let mut directions = [0u32; 32];
    directions[0] = 1 << 31;
    let mut bit = 1;
    while bit < 32 {
        directions[bit] = directions[bit - 1] ^ (directions[bit - 1] >> 1);
        bit += 1;
    }
    let mut tables = [[0u32; 256]; 4];
    let mut byte = 0;
    while byte < 4 {
        let mut value = 0;
        while value < 256 {
            let mut sum = 0u32;
            let mut bit = 0;
            while bit < 8 {
                if (value >> bit) & 1 == 1 {
                    sum ^= directions[byte * 8 + bit];
                }
                bit += 1;
            }
            tables[byte][value] = sum;
            value += 1;
        }
        byte += 1;
    }
    tables
}

fn sobol_1(index: u32) -> u32 {
    let [a, b, c, d] = index.to_le_bytes();
    SOBOL_1[0][usize::from(a)]
        ^ SOBOL_1[1][usize::from(b)]
        ^ SOBOL_1[2][usize::from(c)]
        ^ SOBOL_1[3][usize::from(d)]
}

/// Owen scrambling of `x` as a binary fraction: each bit flipped by a hash of
/// the bits above it.
fn owen(x: u32, seed: u32) -> u32 {
    let mut x = x.reverse_bits().wrapping_add(seed);
    x ^= x.wrapping_mul(0x6c50_b47c);
    x ^= x.wrapping_mul(0xb82f_1e52);
    x ^= x.wrapping_mul(0xc7af_e638);
    x ^= x.wrapping_mul(0x8d22_f6e6);
    x.reverse_bits()
}

/// The random numbers one sample of one pixel draws.
pub(crate) struct Sampler {
    seed: u32,
    index: u32,
    pair: u32,
}

impl Sampler {
    /// Sample `index` of the pixel whose own seed is `seed`.
    pub(crate) const fn new(seed: u32, index: u32) -> Self {
        Self {
            seed,
            index,
            pair: 0,
        }
    }

    /// The next two numbers in `0.0..1.0`.
    pub(crate) fn next_2d(&mut self) -> (f64, f64) {
        let key = mix32(self.seed ^ self.pair.wrapping_mul(0x9e37_79b9));
        self.pair = self.pair.wrapping_add(1);
        if self.pair > SOBOL_PAIRS {
            let first = mix32(key ^ self.index.wrapping_mul(0x85eb_ca6b));
            return (unit(first), unit(mix32(first ^ key)));
        }
        let shuffled = owen(self.index, mix32(key ^ 0xa511_e9b3));
        (
            unit(owen(shuffled.reverse_bits(), mix32(key ^ 0x63d8_3595))),
            unit(owen(sobol_1(shuffled), mix32(key ^ 0x8e7c_4d2a))),
        )
    }

    /// The next number in `0.0..1.0`.
    pub(crate) fn next_1d(&mut self) -> f64 {
        self.next_2d().0
    }
}

/// A point of the unit disc from a pair in the unit square, preserving its
/// stratification (Shirley and Chiu's concentric map).
pub(crate) fn disc((u, v): (f64, f64)) -> (f64, f64) {
    let (a, b) = (2.0 * u - 1.0, 2.0 * v - 1.0);
    if a == 0.0 && b == 0.0 {
        return (0.0, 0.0);
    }
    let (radius, angle) = if a * a > b * b {
        (a, core::f64::consts::FRAC_PI_4 * (b / a))
    } else {
        (
            b,
            core::f64::consts::FRAC_PI_2 - core::f64::consts::FRAC_PI_4 * (a / b),
        )
    };
    (radius * mathf::cos(angle), radius * mathf::sin(angle))
}

/// A direction about the local `z` axis within the cone `cos_max` of it,
/// uniform in solid angle.
pub(crate) fn cone(cos_max: f64, (u, v): (f64, f64)) -> (f64, f64, f64) {
    let cos = 1.0 - u * (1.0 - cos_max);
    let sin = mathf::sqrt(1.0 - cos * cos);
    let angle = TAU * v;
    (sin * mathf::cos(angle), sin * mathf::sin(angle), cos)
}

/// A direction about the local `z` axis, distributed as its cosine.
pub(crate) fn cosine_hemisphere(pair: (f64, f64)) -> (f64, f64, f64) {
    let (x, y) = disc(pair);
    (x, y, mathf::sqrt((1.0 - x * x - y * y).max(0.0)))
}

/// An offset in `-1.0..1.0` distributed as a tent, from a number in
/// `0.0..1.0`: the pixel filter's footprint, drawn so every sample weighs the
/// same.
pub(crate) fn tent(u: f64) -> f64 {
    if u < 0.5 {
        mathf::sqrt(2.0 * u) - 1.0
    } else {
        1.0 - mathf::sqrt(2.0 - 2.0 * u)
    }
}

/// The fewest blocks the first pass of a [`Reveal`] lays across the picture's
/// shorter side: few enough that the whole picture shows after a few hundred
/// pixels at most, enough that it already reads as the scene.
const FIRST_PASS_ACROSS: u32 = 8;

/// The most passes a [`Reveal`] makes: one for each power of two a block's
/// side can be within a `u32`.
const MAX_PASSES: usize = u32::BITS as usize;

/// One step of a [`Reveal`]: the pixel traced, at the block's top-left corner,
/// and the part of the picture its colour stands for until finer steps reach
/// it, clipped to the picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// The traced pixel's column, and the block's left edge.
    pub x: u32,
    /// The traced pixel's row, and the block's top edge.
    pub y: u32,
    /// How many columns the block spans.
    pub width: u32,
    /// How many rows the block spans.
    pub height: u32,
}

/// The order a picture is revealed in: coarse to fine, every pixel traced
/// exactly once, and one small record a pass all that is stored to know it.
///
/// The first pass traces the top-left pixel of each block of a grid at least
/// eight blocks across the shorter side, so a few hundred pixels at most
/// cover the whole picture. Each later pass halves the blocks and
/// traces only the three pixels in four no earlier pass reached, the last
/// tracing single pixels. A block covers no pixel an earlier step traced, so
/// painting every step's block over the last leaves each pixel showing its own
/// trace once the reveal ends. Within a pass the steps follow a keyed
/// bijection, so the whole picture sharpens at once rather than a band of it.
#[derive(Clone, Debug)]
pub struct Reveal {
    width: u32,
    height: u32,
    count: u32,
    passes: [Pass; MAX_PASSES],
    used: usize,
}

/// One pass of a [`Reveal`].
#[derive(Copy, Clone, Debug, Default)]
struct Pass {
    /// Its blocks' side, a power of two.
    side: u32,
    /// The reveal's step its first block is.
    first: u32,
    /// Its grid's blocks across and down.
    columns: u32,
    rows: u32,
    order: Scatter,
}

impl Reveal {
    /// The order a `width` by `height` picture is revealed in under `key`;
    /// `None` for a picture with no pixels, or more than a `u32` counts.
    #[must_use]
    pub fn new((width, height): (u32, u32), key: u64) -> Option<Self> {
        let count = width.checked_mul(height).filter(|count| *count > 0)?;
        let top = (width.min(height) / FIRST_PASS_ACROSS).max(1).ilog2();
        let mut passes = [Pass::default(); MAX_PASSES];
        let (mut first, mut reached, mut used) = (0u32, 0u32, 0usize);
        for ((slot, level), pass) in passes.iter_mut().zip((0..=top).rev()).zip(0u64..) {
            let side = 1u32 << level;
            let (columns, rows) = (width.div_ceil(side), height.div_ceil(side));
            let points = columns * rows;
            let steps = points.saturating_sub(reached);
            *slot = Pass {
                side,
                first,
                columns,
                rows,
                order: Scatter::new(steps, mix64(key ^ pass.wrapping_mul(0x9e37_79b9_7f4a_7c15))),
            };
            first = first.saturating_add(steps);
            reached = points;
            used += 1;
        }
        Some(Self {
            width,
            height,
            count,
            passes,
            used,
        })
    }

    /// How many steps the reveal takes: one for each pixel.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Step `index` of the reveal; `None` past its last.
    #[must_use]
    pub fn block(&self, index: u32) -> Option<Block> {
        if index >= self.count {
            return None;
        }
        // The finest pass holds three steps in four, so searching from it
        // finds most steps' pass at the first look.
        let (at, pass) = self
            .passes
            .get(..self.used)?
            .iter()
            .enumerate()
            .rev()
            .find(|(_, pass)| pass.first <= index)?;
        let step = pass.order.permute(index - pass.first);
        let (column, row) = if at == 0 {
            (step % pass.columns, step / pass.columns)
        } else {
            unreached(pass.columns, pass.rows, step)?
        };
        let (x, y) = (column * pass.side, row * pass.side);
        Some(Block {
            x,
            y,
            width: pass.side.min(self.width - x),
            height: pass.side.min(self.height - y),
        })
    }
}

/// Grid point `step` of a `columns` by `rows` grid, counted among the points
/// the grid of twice its spacing — its even columns of its even rows — does
/// not hold: the odd columns of the even rows, then the even columns of the
/// odd rows, then the odd columns of the odd rows.
fn unreached(columns: u32, rows: u32, step: u32) -> Option<(u32, u32)> {
    let (even_columns, odd_columns) = (columns.div_ceil(2), columns / 2);
    let (even_rows, odd_rows) = (rows.div_ceil(2), rows / 2);
    let across = odd_columns * even_rows;
    if step < across {
        return Some((
            2 * step.checked_rem(odd_columns)? + 1,
            2 * step.checked_div(odd_columns)?,
        ));
    }
    let step = step - across;
    let down = even_columns * odd_rows;
    if step < down {
        return Some((
            2 * step.checked_rem(even_columns)?,
            2 * step.checked_div(even_columns)? + 1,
        ));
    }
    let step = step - down;
    Some((
        2 * step.checked_rem(odd_columns)? + 1,
        2 * step.checked_div(odd_columns)? + 1,
    ))
}

/// A keyed bijection on `0..count` held as nothing but its keys.
///
/// It permutes the smallest power-of-two range holding `count`, walked until
/// it lands inside: alternate multiplications by odd constants, which carry
/// the low bits up, and right shifts, which carry the high bits down, each
/// invertible on the range's width.
#[derive(Copy, Clone, Debug, Default)]
struct Scatter {
    count: u32,
    bits: u32,
    keys: [u32; 4],
}

impl Scatter {
    fn new(count: u32, key: u64) -> Self {
        let bits = (32 - count.saturating_sub(1).leading_zeros()).max(1);
        let keys = [1u64, 2, 3, 4].map(|round| {
            let hashed = mix64(key ^ round.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            u32::try_from(hashed >> 32).unwrap_or(0)
        });
        Self { count, bits, keys }
    }

    /// Where `index` goes; `index` itself outside `0..count`, where no walk
    /// could end.
    fn permute(&self, index: u32) -> u32 {
        if index >= self.count {
            return index;
        }
        let mask = if self.bits >= 32 {
            u32::MAX
        } else {
            (1u32 << self.bits) - 1
        };
        let half = (self.bits / 2).max(1);
        let third = (self.bits / 3).max(1);
        let mut x = index;
        // A permutation of the power-of-two range, so walking its cycle from a
        // point inside `0..count` returns inside it: this ends, and is unique.
        loop {
            for (round, (key, odd)) in self.keys.iter().zip(ODD).enumerate() {
                x ^= key & mask;
                x = x.wrapping_mul(odd) & mask;
                x ^= x >> if round % 2 == 0 { half } else { third };
            }
            if x < self.count {
                return x;
            }
        }
    }
}

const ODD: [u32; 4] = [0xa3b1_95c5, 0xe170_893d, 0x0929_eb3f, 0x6935_fa69];

#[cfg(test)]
#[path = "sample_tests.rs"]
mod tests;
