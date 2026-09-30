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

/// The order a picture's pixels are revealed in: each exactly once, scattered
/// over the whole picture, and nothing stored to know it.
///
/// A keyed bijection on the smallest power-of-two range holding every pixel,
/// walked until it lands on one: alternate multiplications by odd constants,
/// which carry the low bits up, and right shifts, which carry the high bits
/// down, each invertible on the range's width.
#[derive(Copy, Clone, Debug)]
pub struct Reveal {
    count: u32,
    bits: u32,
    keys: [u32; 4],
}

impl Reveal {
    /// The order of `count` pixels under `key`.
    #[must_use]
    pub fn new(count: u32, key: u64) -> Self {
        let bits = (32 - count.saturating_sub(1).leading_zeros()).max(1);
        let keys = [1u64, 2, 3, 4].map(|round| {
            let hashed = mix64(key ^ round.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            u32::try_from(hashed >> 32).unwrap_or(0)
        });
        Self { count, bits, keys }
    }

    /// How many pixels the order visits.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// The pixel revealed `index`-th, the order counted on round from its
    /// start past its [`count`](Self::count); `0` for a picture with no
    /// pixels.
    #[must_use]
    pub fn pixel(&self, index: u32) -> u32 {
        if self.count == 0 {
            return 0;
        }
        // Within the picture, so the cycle walked below holds a pixel.
        let index = index % self.count;
        let mask = if self.bits >= 32 {
            u32::MAX
        } else {
            (1u32 << self.bits) - 1
        };
        let half = (self.bits / 2).max(1);
        let third = (self.bits / 3).max(1);
        let mut x = index & mask;
        // A permutation of the power-of-two range, so walking its cycle from a
        // pixel returns to a pixel: this ends, and the result is unique.
        loop {
            for (round, key) in self.keys.iter().enumerate() {
                x ^= key & mask;
                x = x.wrapping_mul(ODD[round]) & mask;
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
