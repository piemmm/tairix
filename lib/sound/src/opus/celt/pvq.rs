//! The band shapes (RFC 6716, section 4.3.4): a pyramid vector quantiser's
//! codeword, enumerated by the combinatorial index the frame states, scaled
//! to unit norm and spread by the rotation the frame chose.

use super::{cos_norm, rsqrt, SPREAD_NONE};
use crate::opus::range::RangeDecoder;

/// The widest band a partition can be: the last band at the longest block.
pub(super) const MAX_WIDTH: usize = 176;

/// The smaller of `n` and `k` a codeword count that fits in 32 bits allows.
const MAX_SMALLER: usize = 14;

/// `U(n, k)` for `n` up to [`MAX_SMALLER`] and `k` up to [`MAX_WIDTH`]: the
/// codewords of `n` dimensions and `k` pulses whose first is positive, by
/// `U(n, k) = U(n-1, k) + U(n, k-1) + U(n-1, k-1)`, saturating once a count
/// outgrows 32 bits.
const COUNTS: [[u32; MAX_WIDTH + 1]; MAX_SMALLER + 1] = {
    let mut table = [[0u32; MAX_WIDTH + 1]; MAX_SMALLER + 1];
    table[0][0] = 1;
    let mut n = 1;
    while n <= MAX_SMALLER {
        let mut k = 1;
        while k <= MAX_WIDTH {
            let sum = table[n - 1][k] as u64 + table[n][k - 1] as u64 + table[n - 1][k - 1] as u64;
            table[n][k] = if sum > u32::MAX as u64 {
                u32::MAX
            } else {
                sum as u32
            };
            k += 1;
        }
        n += 1;
    }
    table
};

/// `U(n, k)` read as the table holds it, `smaller` the lesser argument.
fn count(smaller: usize, larger: usize) -> u32 {
    COUNTS
        .get(smaller)
        .and_then(|row| row.get(larger))
        .copied()
        .unwrap_or(u32::MAX)
}

fn symmetric(n: usize, k: usize) -> u32 {
    count(n.min(k), n.max(k))
}

/// The codewords of `n` dimensions and `k` pulses.
fn codewords(n: usize, k: usize) -> u32 {
    symmetric(n, k).saturating_add(symmetric(n, k + 1))
}

/// The codeword of `k` pulses over `y.len()` dimensions, at least two, that
/// the frame states; its squared norm.
#[allow(
    clippy::many_single_char_names,
    reason = "the reference's names, kept so the port can be read against it"
)]
fn decode_pulses(decoder: &mut RangeDecoder<'_>, y: &mut [i32], k: usize) -> f32 {
    let mut index = decoder.uint(codewords(y.len(), k));
    let mut n = y.len();
    let mut k = k;
    let mut energy = 0.0f32;
    let mut put = |slot: &mut i32, value: i32| {
        *slot = value;
        energy += (value * value) as f32;
    };
    let mut slots = y.iter_mut();
    while n > 2 {
        let Some(slot) = slots.next() else {
            break;
        };
        if k >= n {
            let mut p = count(n, k + 1);
            let negative = index >= p;
            if negative {
                index -= p;
            }
            let k0 = k;
            if count(n, n) > index {
                k = n;
                loop {
                    k -= 1;
                    p = count(k, n);
                    if p <= index {
                        break;
                    }
                }
            } else {
                p = count(n, k);
                while p > index {
                    k -= 1;
                    p = count(n, k);
                }
            }
            index -= p;
            put(slot, signed(k0 - k, negative));
        } else {
            let p = count(k, n);
            let q = count(k + 1, n);
            if p <= index && index < q {
                index -= p;
                put(slot, 0);
            } else {
                let negative = index >= q;
                if negative {
                    index -= q;
                }
                let k0 = k;
                let mut p;
                loop {
                    k -= 1;
                    p = count(k, n);
                    if p <= index {
                        break;
                    }
                }
                index -= p;
                put(slot, signed(k0 - k, negative));
            }
        }
        n -= 1;
    }
    let (Some(second_last), Some(last)) = (slots.next(), slots.next()) else {
        return energy;
    };
    let p = (2 * k + 1) as u32;
    let negative = index >= p;
    if negative {
        index -= p;
    }
    let k0 = k;
    k = ((index + 1) >> 1) as usize;
    if k != 0 {
        index -= (2 * k - 1) as u32;
    }
    put(second_last, signed(k0 - k, negative));
    put(last, signed(k, index != 0));
    energy
}

fn signed(magnitude: usize, negative: bool) -> i32 {
    let magnitude = magnitude as i32;
    if negative {
        -magnitude
    } else {
        magnitude
    }
}

/// Rotate `x` by the spreading the frame chose, undoing the encoder's
/// rotation, over `blocks` interleaved short blocks.
fn spread(x: &mut [f32], blocks: usize, k: usize, spread: usize) {
    const FACTOR: [usize; 3] = [15, 10, 5];
    let len = x.len();
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let factor = FACTOR[spread - 1];
    let gain = len as f32 / (len + factor * k) as f32;
    let theta = 0.5 * (gain * gain);
    let c = cos_norm(theta);
    let s = cos_norm(1.0 - theta);
    let mut stride2 = 0;
    if len >= 8 * blocks {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * blocks + (blocks >> 2) < len {
            stride2 += 1;
        }
    }
    let block = len / blocks;
    for chunk in x.chunks_exact_mut(block) {
        if stride2 != 0 {
            rotate(chunk, stride2, s, c);
        }
        rotate(chunk, 1, c, s);
    }
}

/// One pass of the spreading rotation at `stride`, forward then back.
fn rotate(x: &mut [f32], stride: usize, c: f32, s: f32) {
    let len = x.len();
    if len <= stride {
        return;
    }
    let ms = -s;
    for i in 0..len - stride {
        let (x1, x2) = (x[i], x[i + stride]);
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 + ms * x2;
    }
    if len < 2 * stride + 1 {
        return;
    }
    for i in (0..len - 2 * stride).rev() {
        let (x1, x2) = (x[i], x[i + stride]);
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 + ms * x2;
    }
}

/// Which of `blocks` short blocks the codeword `pulses` puts a pulse in.
fn collapse_mask(pulses: &[i32], blocks: usize) -> u32 {
    if blocks <= 1 {
        return 1;
    }
    let block = pulses.len() / blocks;
    pulses
        .chunks_exact(block)
        .enumerate()
        .fold(0, |mask, (index, chunk)| {
            mask | (u32::from(chunk.iter().any(|&pulse| pulse != 0)) << index)
        })
}

/// Decode a band's shape of `k` pulses into `x`, scaled to `gain`; which
/// short blocks it fills.
pub(super) fn unquantise(
    decoder: &mut RangeDecoder<'_>,
    x: &mut [f32],
    k: usize,
    spreading: usize,
    blocks: usize,
    gain: f32,
    pulses: &mut [i32],
) -> u32 {
    let pulses = &mut pulses[..x.len()];
    let energy = decode_pulses(decoder, pulses, k);
    let g = rsqrt(energy) * gain;
    for (value, &pulse) in x.iter_mut().zip(pulses.iter()) {
        *value = g * pulse as f32;
    }
    spread(x, blocks, k, spreading);
    collapse_mask(pulses, blocks)
}

/// Scale `x` to norm `gain`.
pub(super) fn renormalise(x: &mut [f32], gain: f32) {
    let energy = super::EPSILON + inner(x, x);
    let g = rsqrt(energy) * gain;
    for value in x {
        *value *= g;
    }
}

/// `Σ x[i] y[i]`, summed in order.
pub(super) fn inner(x: &[f32], y: &[f32]) -> f32 {
    x.iter().zip(y).fold(0.0, |sum, (a, b)| sum + a * b)
}

#[cfg(test)]
mod tests {
    use super::{codewords, count, decode_pulses};
    use crate::opus::range::writer::RangeEncoder;
    use crate::opus::range::RangeDecoder;

    /// The index of codeword `y` (the encoder's enumeration).
    #[allow(
        clippy::many_single_char_names,
        reason = "the reference encoder's names, kept so the port can be read against it"
    )]
    fn index_of(y: &[i32]) -> u32 {
        let n = y.len();
        let mut j = n - 1;
        let mut i = u32::from(y[j] < 0);
        let mut k = y[j].unsigned_abs() as usize;
        loop {
            if j == 0 {
                return i;
            }
            j -= 1;
            i += count((n - j).min(k), (n - j).max(k));
            k += y[j].unsigned_abs() as usize;
            if y[j] < 0 {
                i += count((n - j).min(k + 1), (n - j).max(k + 1));
            }
        }
    }

    #[test]
    fn the_counts_match_the_closed_forms() {
        for k in 1..60 {
            assert_eq!(count(1, k), 1);
            assert_eq!(count(2, k), 2 * k as u32 - 1);
            assert_eq!(count(3, k), ((2 * k as u32 - 2) * k as u32) + 1);
            assert_eq!(codewords(2, k), 4 * k as u32);
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "integers' squares sum exactly in single precision"
    )]
    fn every_codeword_of_a_small_pyramid_reads_back_from_its_index() {
        for (n, k) in [
            (2usize, 1usize),
            (2, 5),
            (3, 4),
            (4, 3),
            (5, 5),
            (8, 2),
            (16, 3),
        ] {
            let total = codewords(n, k);
            for index in (0..total).step_by((total as usize / 97).max(1)) {
                let mut encoder = RangeEncoder::new();
                encoder.uint(index, total);
                let frame = encoder.finish(64).expect("fits");
                let mut decoder = RangeDecoder::new(&frame);
                let mut y = [0i32; 16];
                let energy = decode_pulses(&mut decoder, &mut y[..n], k);
                let pulses: i32 = y[..n].iter().map(|v| v.abs()).sum();
                assert_eq!(pulses as usize, k, "{n} {k} {index}");
                assert_eq!(energy, y[..n].iter().map(|v| (v * v) as f32).sum::<f32>());
                assert_eq!(index_of(&y[..n]), index, "{n} {k} {index}");
            }
        }
    }
}
