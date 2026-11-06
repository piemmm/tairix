//! The band shapes (RFC 6716, section 4.3.4): a pyramid vector quantiser's
//! codeword, enumerated by the combinatorial index the frame states, scaled
//! to unit norm and spread by the rotation the frame chose. An encoder first
//! rotates a shape the other way and finds the codeword nearest it (section
//! 5.3.8).

use super::{cos_norm, rsqrt, EPSILON, SPREAD_NONE};
use crate::opus::range::Coder;

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
/// `index` enumerates; its squared norm.
#[allow(
    clippy::many_single_char_names,
    reason = "the reference's names, kept so the port can be read against it"
)]
fn decode_pulses(mut index: u32, y: &mut [i32], k: usize) -> f32 {
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

/// The index the frame states for codeword `y`, of at least two dimensions:
/// the inverse of [`decode_pulses`].
fn index_of(y: &[i32]) -> u32 {
    let n = y.len();
    let Some(mut j) = n.checked_sub(1) else {
        return 0;
    };
    let mut index = u32::from(y[j] < 0);
    let mut k = y[j].unsigned_abs() as usize;
    while j > 0 {
        j -= 1;
        index = index.wrapping_add(symmetric(n - j, k));
        k += y[j].unsigned_abs() as usize;
        if y[j] < 0 {
            index = index.wrapping_add(symmetric(n - j, k + 1));
        }
    }
    index
}

/// The codeword of `k` pulses nearest `x` in direction (RFC 6716, section
/// 5.3.8): `x` projected onto the pyramid of `k - 1` pulses, then each pulse
/// left placed where it most raises the codeword's normalised correlation
/// with `x`, every pulse taking its coefficient's sign.
fn search(x: &[f32], k: usize, y: &mut [i32]) {
    y.fill(0);
    if k == 0 {
        return;
    }
    let sum = x.iter().fold(0.0f32, |sum, value| sum + value.abs());
    let mut placed = 0;
    let mut correlation = 0.0f32;
    let mut energy = 0.0f32;
    if sum > EPSILON {
        let scale = (k - 1) as f32 / sum;
        for (slot, value) in y.iter_mut().zip(x) {
            let pulses = ((value.abs() * scale) as usize).min(k - 1 - placed);
            *slot = pulses as i32;
            placed += pulses;
            correlation += value.abs() * pulses as f32;
            energy += (pulses * pulses) as f32;
        }
    }
    while placed < k {
        let mut best = 0;
        let (mut best_gain, mut best_energy) = (-1.0f32, 1.0f32);
        for (j, (value, &pulses)) in x.iter().zip(y.iter()).enumerate() {
            let gain = correlation + value.abs();
            let gain = gain * gain;
            let trial = energy + (2 * pulses + 1) as f32;
            if gain * best_energy > best_gain * trial {
                best = j;
                best_gain = gain;
                best_energy = trial;
            }
        }
        correlation += x[best].abs();
        energy += (2 * y[best] + 1) as f32;
        y[best] += 1;
        placed += 1;
    }
    for (slot, value) in y.iter_mut().zip(x) {
        if *value < 0.0 {
            *slot = -*slot;
        }
    }
}

/// The spreading rotation of `len` coefficients over `blocks` holding `k`
/// pulses: its cosine, its sine and its second pass's stride, or `None`
/// where the frame spreads nothing.
fn rotation(len: usize, blocks: usize, k: usize, spread: usize) -> Option<(f32, f32, usize)> {
    const FACTOR: [usize; 3] = [15, 10, 5];
    if 2 * k >= len || spread == SPREAD_NONE {
        return None;
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
    Some((c, s, stride2))
}

/// Rotate `x` by the spreading the frame chose, undoing the encoder's
/// rotation, over `blocks` interleaved short blocks.
fn spread(x: &mut [f32], blocks: usize, k: usize, spread: usize) {
    let Some((c, s, stride2)) = rotation(x.len(), blocks, k, spread) else {
        return;
    };
    let block = x.len() / blocks;
    for chunk in x.chunks_exact_mut(block) {
        if stride2 != 0 {
            rotate(chunk, stride2, s, c);
        }
        rotate(chunk, 1, c, s);
    }
}

/// Rotate `x` as an encoder does before it quantises it: [`spread`] turns
/// it back.
fn unspread(x: &mut [f32], blocks: usize, k: usize, spreading: usize) {
    let Some((c, s, stride2)) = rotation(x.len(), blocks, k, spreading) else {
        return;
    };
    let block = x.len() / blocks;
    for chunk in x.chunks_exact_mut(block) {
        unrotate(chunk, 1, c, s);
        if stride2 != 0 {
            unrotate(chunk, stride2, s, c);
        }
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

/// Undo [`rotate`] at `stride`: each of its rotations taken back, the last
/// first.
fn unrotate(x: &mut [f32], stride: usize, c: f32, s: f32) {
    let len = x.len();
    if len <= stride {
        return;
    }
    let second = len.saturating_sub(2 * stride);
    for i in (0..second).chain((0..len - stride).rev()) {
        let (x1, x2) = (x[i], x[i + stride]);
        x[i + stride] = c * x2 - s * x1;
        x[i] = c * x1 + s * x2;
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

/// Code a band's shape of `k` pulses in `x`, at least two coefficients,
/// leaving there the shape the frame carries scaled to `gain`: an encoder
/// first writes the codeword nearest the shape `x` holds. Which short blocks
/// the shape fills.
pub(super) fn shape<C: Coder>(
    coder: &mut C,
    x: &mut [f32],
    k: usize,
    spreading: usize,
    blocks: usize,
    gain: f32,
    pulses: &mut [i32],
) -> u32 {
    let pulses = &mut pulses[..x.len()];
    let total = codewords(x.len(), k);
    let energy = if C::WRITES {
        unspread(x, blocks, k, spreading);
        search(x, k, pulses);
        coder.uint(index_of(pulses), total);
        pulses
            .iter()
            .fold(0.0f32, |sum, &pulse| sum + (pulse * pulse) as f32)
    } else {
        let index = coder.uint(0, total);
        decode_pulses(index, pulses, k)
    };
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
    extern crate std;

    use std::vec;

    use super::{codewords, count, decode_pulses, index_of, search, shape, spread, unspread};
    use crate::opus::range::writer::RangeEncoder;
    use crate::opus::range::RangeDecoder;

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
                let mut y = [0i32; 16];
                let energy = decode_pulses(index, &mut y[..n], k);
                let pulses: i32 = y[..n].iter().map(|v| v.abs()).sum();
                assert_eq!(pulses as usize, k, "{n} {k} {index}");
                assert_eq!(energy, y[..n].iter().map(|v| (v * v) as f32).sum::<f32>());
                assert_eq!(index_of(&y[..n]), index, "{n} {k} {index}");
            }
        }
    }

    #[test]
    fn the_encoder_s_rotation_is_what_the_spreading_undoes() {
        let original: [f32; 48] = core::array::from_fn(|i| ((i * 37 % 23) as f32 - 11.0) / 7.0);
        for (blocks, k, spreading) in [(1, 3, 1), (1, 5, 2), (2, 4, 3), (8, 2, 2), (1, 30, 2)] {
            let mut x = original;
            unspread(&mut x, blocks, k, spreading);
            spread(&mut x, blocks, k, spreading);
            for (a, b) in x.iter().zip(&original) {
                assert!((a - b).abs() < 1e-5, "{blocks} {k} {spreading}");
            }
        }
    }

    #[test]
    fn the_search_places_every_pulse_with_its_coefficient_s_sign() {
        let x = [0.6f32, -0.3, 0.1, 0.0, -0.7, 0.2, 0.05, -0.01];
        for k in [1usize, 2, 5, 9, 20] {
            let mut y = [0i32; 8];
            search(&x, k, &mut y);
            let placed: u32 = y.iter().map(|pulse| pulse.unsigned_abs()).sum();
            assert_eq!(placed as usize, k);
            for (pulse, value) in y.iter().zip(&x) {
                assert!(*pulse == 0 || (*pulse < 0) == (*value < 0.0), "{k}");
            }
        }
        let mut y = [0i32; 8];
        search(&x, 1, &mut y);
        assert_eq!(y, [0, 0, 0, 0, -1, 0, 0, 0], "one pulse, at the largest");
        let mut silent = [0i32; 4];
        search(&[0.0; 4], 3, &mut silent);
        assert_eq!(silent.iter().map(|pulse| pulse.abs()).sum::<i32>(), 3);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "both ends compute one shape from one codeword"
    )]
    fn a_shape_written_reads_back_as_the_encoder_left_it() {
        let input: [f32; 16] = core::array::from_fn(|i| ((i * 13 % 17) as f32 - 8.0) / 9.0);
        for (k, spreading, blocks) in [(1, 2, 1), (6, 2, 1), (5, 1, 4), (4, 3, 8), (3, 0, 2)] {
            let mut frame = vec![0u8; 64];
            let mut encoder = RangeEncoder::new(&mut frame);
            let mut written = input;
            let mut pulses = [0i32; 16];
            let mask = shape(
                &mut encoder,
                &mut written,
                k,
                spreading,
                blocks,
                1.0,
                &mut pulses,
            );
            assert!(encoder.finish(), "fits");
            let mut decoder = RangeDecoder::new(&frame);
            let mut read = [0.0f32; 16];
            let read_mask = shape(
                &mut decoder,
                &mut read,
                k,
                spreading,
                blocks,
                1.0,
                &mut pulses,
            );
            assert_eq!(
                (read, read_mask),
                (written, mask),
                "{k} {spreading} {blocks}"
            );
            let correlation: f32 = written.iter().zip(&input).map(|(a, b)| a * b).sum();
            assert!(correlation > 0.0, "{k} {spreading} {blocks}");
        }
    }
}
