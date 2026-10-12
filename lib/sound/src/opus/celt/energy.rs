//! The band energies (RFC 6716, section 4.3.2): the coarse energy, predicted
//! across time and frequency and coded with a Laplace model, then the fine
//! energy and the frame's leftover bits, both raw.

use super::tables::{BETA, BETA_INTRA, ENERGY_MODEL, PREDICTION, SMALL_ENERGY_ICDF};
use super::{max16, BAND_COUNT, MAX_FINE_BITS};
use crate::opus::range::RangeDecoder;

const LAPLACE_MINP: u32 = 1;
const LAPLACE_NMIN: u32 = 16;

/// The probability of `±1` after a zero of probability `zero`.
fn laplace_first(zero: u32, decay: u32) -> u32 {
    let rest = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - zero;
    (rest * (16384 - decay)) >> 15
}

/// A value from the Laplace-like distribution whose zero has probability
/// `zero` in 32768 and whose magnitudes decay by `decay` in 16384.
fn laplace(decoder: &mut RangeDecoder<'_>, zero: u32, decay: u32) -> i32 {
    let mut value = 0i32;
    let point = decoder.decode_bin(15);
    let mut low = 0;
    let mut width = zero;
    if point >= width {
        value = 1;
        low = width;
        width = laplace_first(width, decay) + LAPLACE_MINP;
        while width > LAPLACE_MINP && point >= low + 2 * width {
            width *= 2;
            low += width;
            width = ((width - 2 * LAPLACE_MINP) * decay) >> 15;
            width += LAPLACE_MINP;
            value += 1;
        }
        if width <= LAPLACE_MINP {
            let further = (point - low) >> 1;
            value += further as i32;
            low += 2 * further * LAPLACE_MINP;
        }
        if point < low + width {
            value = -value;
        } else {
            low += width;
        }
    }
    decoder.update(low, (low + width).min(32768), 32768);
    value
}

/// Decode the coarse energies of bands `start..end` into `energy`, which
/// holds the previous frame's, one row of [`BAND_COUNT`] per channel.
pub(super) fn coarse(
    decoder: &mut RangeDecoder<'_>,
    energy: &mut [f32; 2 * BAND_COUNT],
    bands: core::ops::Range<usize>,
    intra: bool,
    channels: usize,
    lm: usize,
) {
    let model = &ENERGY_MODEL[lm][usize::from(intra)];
    let (coefficient, beta) = if intra {
        (0.0, BETA_INTRA)
    } else {
        (PREDICTION[lm], BETA[lm])
    };
    let mut prediction = [0.0f32; 2];
    let budget = decoder.storage() as i32 * 8;
    for band in bands {
        for (channel, predicted) in prediction.iter_mut().enumerate().take(channels) {
            let left = budget - decoder.tell();
            let q = if left >= 15 {
                let at = 2 * band.min(20);
                laplace(
                    decoder,
                    u32::from(model[at]) << 7,
                    u32::from(model[at + 1]) << 6,
                )
            } else if left >= 2 {
                let symbol = decoder.icdf(&SMALL_ENERGY_ICDF, 2) as i32;
                (symbol >> 1) ^ -(symbol & 1)
            } else if left >= 1 {
                -i32::from(decoder.bit_logp(1))
            } else {
                -1
            } as f32;
            let previous = &mut energy[band + channel * BAND_COUNT];
            *previous = max16(-9.0, *previous);
            *previous = coefficient * *previous + *predicted + q;
            *predicted = *predicted + q - beta * q;
        }
    }
}

/// Refine each band's energy by its `fine` bits.
pub(super) fn fine(
    decoder: &mut RangeDecoder<'_>,
    energy: &mut [f32; 2 * BAND_COUNT],
    bands: core::ops::Range<usize>,
    fine: &[u32; BAND_COUNT],
    channels: usize,
) {
    for band in bands {
        let bits = fine[band];
        if bits == 0 {
            continue;
        }
        for channel in 0..channels {
            let q = decoder.bits(bits) as f32;
            let offset = (q + 0.5) * (1u32 << (14 - bits)) as f32 * (1.0 / 16384.0) - 0.5;
            energy[band + channel * BAND_COUNT] += offset;
        }
    }
}

/// Spend the frame's `left` remaining bits on one more bit of fine energy
/// each, the bands that rounded down first.
pub(super) fn finalise(
    decoder: &mut RangeDecoder<'_>,
    energy: &mut [f32; 2 * BAND_COUNT],
    bands: core::ops::Range<usize>,
    fine: &[u32; BAND_COUNT],
    priority: &[bool; BAND_COUNT],
    mut left: i32,
    channels: usize,
) {
    let channel_count = channels as i32;
    for pass in [false, true] {
        for band in bands.clone() {
            if left < channel_count {
                break;
            }
            if fine[band] >= MAX_FINE_BITS || priority[band] != pass {
                continue;
            }
            for channel in 0..channels {
                let q = decoder.bits(1) as f32;
                let offset = (q - 0.5) * (1u32 << (14 - fine[band] - 1)) as f32 * (1.0 / 16384.0);
                energy[band + channel * BAND_COUNT] += offset;
                left -= 1;
            }
        }
    }
}

/// Code `value`, or the nearest the model can state; the value coded.
#[cfg(test)]
pub(super) fn encode_laplace(
    encoder: &mut crate::opus::range::writer::RangeEncoder,
    mut value: i32,
    zero: u32,
    decay: u32,
) -> i32 {
    let mut coded = value;
    let mut low = 0;
    let mut width = zero;
    if value != 0 {
        let sign = if value < 0 { -1 } else { 0 };
        value = (value + sign) ^ sign;
        low = width;
        width = laplace_first(width, decay);
        let mut i = 1;
        while width > 0 && i < value {
            width *= 2;
            low += width + 2 * LAPLACE_MINP;
            width = (width * decay) >> 15;
            i += 1;
        }
        if width == 0 {
            let most = ((32768 - low + LAPLACE_MINP - 1) as i32 - sign) >> 1;
            let further = (value - i).min(most - 1);
            low += (2 * further + 1 + sign) as u32 * LAPLACE_MINP;
            width = LAPLACE_MINP.min(32768 - low);
            coded = (i + further + sign) ^ sign;
        } else {
            width += LAPLACE_MINP;
            low += width & !(sign as u32);
        }
    }
    encoder.encode_bin(low, low + width, 15);
    coded
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{encode_laplace, laplace};
    use crate::opus::range::writer::RangeEncoder;
    use crate::opus::range::RangeDecoder;

    #[test]
    fn laplace_values_read_back_as_written_across_the_models() {
        let values: [i32; 14] = [0, 1, -1, 2, -2, 3, -7, 12, -20, 40, -40, 0, 5, -3];
        for (zero, decay) in [(72 << 7, 127 << 6), (22 << 7, 178 << 6), (189 << 7, 6 << 6)] {
            let mut encoder = RangeEncoder::new();
            let coded: Vec<i32> = values
                .iter()
                .map(|&value| encode_laplace(&mut encoder, value, zero, decay))
                .collect();
            assert!(coded.iter().zip(&values).any(|(a, b)| a == b && *a != 0));
            let frame = encoder.finish(256).expect("fits");
            let mut decoder = RangeDecoder::new(&frame);
            for &value in &coded {
                assert_eq!(laplace(&mut decoder, zero, decay), value, "{zero} {decay}");
            }
        }
    }
}
