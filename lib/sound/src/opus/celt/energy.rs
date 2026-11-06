//! The band energies (RFC 6716, section 4.3.2): the coarse energy, predicted
//! across time and frequency and coded with a Laplace model, then the fine
//! energy and the frame's leftover bits, both raw.
//!
//! Each routine reads a frame's energies, or writes those nearest the ones an
//! encoder aims at; the prediction and every update are the same either way.

use tairix_util::mathf;

use super::alloc::Allocation;
use super::tables::{BETA, BETA_INTRA, ENERGY_MODEL, PREDICTION, SMALL_ENERGY_ICDF};
use super::{max16, min16, BAND_COUNT, MAX_FINE_BITS, SILENT_ENERGY};
use crate::opus::range::Coder;

const LAPLACE_MINP: u32 = 1;
const LAPLACE_NMIN: u32 = 16;
/// The largest coarse step an encoder asks for: far past any band's change
/// between frames, and well inside what the model's arithmetic holds.
const MAX_STEP: i32 = 128;
/// The bits the coarse energy keeps for each band and channel still to come
/// (RFC 6716, section 5.3.3).
const RESERVE: i32 = 3;
/// The spare bits under which a coarse step rises by one at most.
const RISE_SPARE: i32 = 24;
/// The spare bits under which a coarse step moves by one at most either way.
const FALL_SPARE: i32 = 16;
/// The most a band's energy falls in a frame, in `log2` steps of 6 dB.
const MOST_DECAY: f32 = 16.0;
/// The fall a byte pays for in a frame of more than [`DECAY_BANDS`] bands.
const DECAY_PER_BYTE: f32 = 0.125;
/// The bands past which a frame's bytes hold its energies' fall.
const DECAY_BANDS: usize = 10;

/// The probability of `±1` after a zero of probability `zero`.
fn laplace_first(zero: u32, decay: u32) -> u32 {
    let rest = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - zero;
    (rest * (16384 - decay)) >> 15
}

/// A value from the Laplace-like distribution whose zero has probability
/// `zero` in 32768 and whose magnitudes decay by `decay` in 16384; an
/// encoder writes `value`, or the nearest the model can state.
fn laplace(coder: &mut impl Coder, value: i32, zero: u32, decay: u32) -> i32 {
    let (value, low, width) = match coder.point_bin(15) {
        Some(point) => laplace_at(point, zero, decay),
        None => laplace_of(value, zero, decay),
    };
    coder.take(low, (low + width).min(32768), 32768);
    value
}

/// The value whose interval holds `point`, and that interval's start and
/// width.
fn laplace_at(point: u32, zero: u32, decay: u32) -> (i32, u32, u32) {
    let mut value = 0i32;
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
    (value, low, width)
}

/// `value`, or the nearest value the model can state, and its interval's
/// start and width.
fn laplace_of(value: i32, zero: u32, decay: u32) -> (i32, u32, u32) {
    let mut coded = value;
    let mut low = 0;
    let mut width = zero;
    if value != 0 {
        let sign = if value < 0 { -1 } else { 0 };
        let magnitude = (value + sign) ^ sign;
        low = width;
        width = laplace_first(width, decay);
        let mut i = 1;
        while width > 0 && i < magnitude {
            width *= 2;
            low += width + 2 * LAPLACE_MINP;
            width = (width * decay) >> 15;
            i += 1;
        }
        if width == 0 {
            let most = ((32768 - low + LAPLACE_MINP - 1) as i32 - sign) >> 1;
            let further = (magnitude - i).min(most - 1);
            low += (2 * further + 1 + sign) as u32 * LAPLACE_MINP;
            width = LAPLACE_MINP.min(32768 - low);
            coded = (i + further + sign) ^ sign;
        } else {
            width += LAPLACE_MINP;
            low += width & !(sign as u32);
        }
    }
    (coded, low, width)
}

/// The coarse step nearest `error`, within those an encoder asks for.
fn coarse_step(error: f32) -> i32 {
    mathf::round_i32(f64::from(error)).clamp(-MAX_STEP, MAX_STEP)
}

/// `step`, a fall toward `aim` from a band whose energy was `previous`, held
/// to `decay` under it: a band's energy is let down over frames, as a
/// frame's bytes pay for.
fn decayed(step: i32, aim: f32, previous: f32, decay: f32) -> i32 {
    let bound = max16(SILENT_ENERGY, previous) - decay;
    if step < 0 && aim < bound {
        (step + (bound - aim) as i32).min(0)
    } else {
        step
    }
}

/// `step` held to what `spare` bits, past those the later bands keep, still
/// pay for, so the early bands cannot spend a short frame.
fn within(step: i32, spare: i32) -> i32 {
    match spare {
        ..FALL_SPARE => step.clamp(-1, 1),
        FALL_SPARE..RISE_SPARE => step.min(1),
        _ => step,
    }
}

/// The small model's symbol for `step`, held to the steps it states.
fn small_symbol(step: i32) -> usize {
    match step.clamp(-1, 1) {
        0 => 0,
        -1 => 1,
        _ => 2,
    }
}

/// The fine cell of `steps` across a coarse step that `error`, within half
/// a step of zero, falls in.
fn fine_step(error: f32, steps: u32) -> u32 {
    let cell = mathf::floor((f64::from(error) + 0.5) * f64::from(steps));
    cell.clamp(0.0, f64::from(steps - 1)) as u32
}

/// Code the coarse energies of bands `bands` into `energy`, which holds the
/// previous frame's, one row of [`BAND_COUNT`] per channel: an encoder
/// writes the steps nearest `aim`, a fall held to what the frame's bytes let
/// down and each step to what the frame pays for while the bands after it
/// keep their bits.
pub(super) fn coarse<C: Coder>(
    coder: &mut C,
    energy: &mut [f32; 2 * BAND_COUNT],
    aim: &[f32; 2 * BAND_COUNT],
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
    let budget = coder.storage() as i32 * 8;
    let decay = if bands.len() > DECAY_BANDS {
        min16(MOST_DECAY, DECAY_PER_BYTE * coder.storage() as f32)
    } else {
        MOST_DECAY
    };
    let (first, end) = (bands.start, bands.end);
    for band in bands {
        for (channel, predicted) in prediction.iter_mut().enumerate().take(channels) {
            let at = band + channel * BAND_COUNT;
            let left = budget - coder.tell();
            let base = coefficient * max16(-9.0, energy[at]) + *predicted;
            let wanted = if C::WRITES {
                let step = decayed(coarse_step(aim[at] - base), aim[at], energy[at], decay);
                if band == first {
                    step
                } else {
                    within(step, left - RESERVE * (channels * (end - band)) as i32)
                }
            } else {
                0
            };
            let q = if left >= 15 {
                let index = 2 * band.min(20);
                laplace(
                    coder,
                    wanted,
                    u32::from(model[index]) << 7,
                    u32::from(model[index + 1]) << 6,
                )
            } else if left >= 2 {
                let symbol = coder.icdf(small_symbol(wanted), &SMALL_ENERGY_ICDF, 2) as i32;
                (symbol >> 1) ^ -(symbol & 1)
            } else if left >= 1 {
                -i32::from(coder.bit_logp(wanted < 0, 1))
            } else {
                -1
            } as f32;
            energy[at] = base + q;
            *predicted = *predicted + q - beta * q;
        }
    }
}

/// Refine each band's energy by its `fine` bits: an encoder writes those
/// nearest `aim`.
pub(super) fn fine<C: Coder>(
    coder: &mut C,
    energy: &mut [f32; 2 * BAND_COUNT],
    aim: &[f32; 2 * BAND_COUNT],
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
            let at = band + channel * BAND_COUNT;
            let wanted = if C::WRITES {
                fine_step(aim[at] - energy[at], 1 << bits)
            } else {
                0
            };
            let q = coder.bits(wanted, bits) as f32;
            let offset = (q + 0.5) * (1u32 << (14 - bits)) as f32 * (1.0 / 16384.0) - 0.5;
            energy[at] += offset;
        }
    }
}

/// Spend the frame's `left` remaining bits on one more bit of fine energy
/// each, the bands that rounded down first: an encoder writes the halves
/// `aim` falls in.
pub(super) fn finalise<C: Coder>(
    coder: &mut C,
    energy: &mut [f32; 2 * BAND_COUNT],
    aim: &[f32; 2 * BAND_COUNT],
    bands: core::ops::Range<usize>,
    allocation: &Allocation,
    mut left: i32,
    channels: usize,
) {
    let channel_count = channels as i32;
    for pass in [false, true] {
        for band in bands.clone() {
            if left < channel_count {
                break;
            }
            let fine = allocation.fine[band];
            if fine >= MAX_FINE_BITS || allocation.priority[band] != pass {
                continue;
            }
            for channel in 0..channels {
                let at = band + channel * BAND_COUNT;
                let wanted = u32::from(C::WRITES && aim[at] >= energy[at]);
                let q = coder.bits(wanted, 1) as f32;
                let offset = (q - 0.5) * (1u32 << (14 - fine - 1)) as f32 * (1.0 / 16384.0);
                energy[at] += offset;
                left -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;
    use std::vec::Vec;

    use super::{
        coarse, fine_step, laplace, small_symbol, within, BAND_COUNT, BETA_INTRA, DECAY_PER_BYTE,
    };
    use crate::opus::range::writer::RangeEncoder;
    use crate::opus::range::RangeDecoder;

    #[test]
    fn laplace_values_read_back_as_written_across_the_models() {
        let values: [i32; 15] = [0, 1, -1, 2, -2, 3, -7, 12, -20, 40, -40, 0, 5, -3, 128];
        for (zero, decay) in [(72 << 7, 127 << 6), (22 << 7, 178 << 6), (189 << 7, 6 << 6)] {
            let mut frame = vec![0u8; 256];
            let mut encoder = RangeEncoder::new(&mut frame);
            let coded: Vec<i32> = values
                .iter()
                .map(|&value| laplace(&mut encoder, value, zero, decay))
                .collect();
            assert!(coded.iter().zip(&values).any(|(a, b)| a == b && *a != 0));
            assert!(encoder.finish(), "fits");
            let mut decoder = RangeDecoder::new(&frame);
            for &value in &coded {
                assert_eq!(
                    laplace(&mut decoder, 0, zero, decay),
                    value,
                    "{zero} {decay}"
                );
            }
        }
    }

    #[test]
    fn an_encoder_s_steps_stay_within_what_their_models_state() {
        assert_eq!([-5, -1, 0, 1, 9].map(small_symbol), [1, 1, 0, 2, 2]);
        assert_eq!(fine_step(-0.5, 8), 0);
        assert_eq!(fine_step(-0.4, 8), 0);
        assert_eq!(fine_step(0.0, 8), 4);
        assert_eq!(fine_step(0.49, 8), 7);
        assert_eq!(
            fine_step(3.0, 8),
            7,
            "an error past the step held to its last cell"
        );
        assert_eq!(fine_step(-3.0, 4), 0, "and before it to its first");
        assert_eq!(
            [within(5, 30), within(5, 20), within(-5, 20), within(-5, 10)],
            [5, 1, -5, -1],
            "a short frame's steps held to what it has left"
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "both ends add the same steps in the same order"
    )]
    fn the_lowest_bitrate_s_frame_still_moves_each_later_band_toward_its_energy() {
        let aim: [f32; 2 * BAND_COUNT] =
            core::array::from_fn(|at| 8.0 * (at % BAND_COUNT + 1) as f32);
        for channels in [1, 2] {
            let mut frame = [0u8; 14];
            let mut energy = [0.0f32; 2 * BAND_COUNT];
            let mut encoder = RangeEncoder::new(&mut frame);
            coarse(
                &mut encoder,
                &mut energy,
                &aim,
                0..BAND_COUNT,
                true,
                channels,
                3,
            );
            assert!(encoder.finish(), "{channels} channels");
            let mut prediction = [0.0f32; 2];
            for band in 0..BAND_COUNT {
                for (channel, predicted) in prediction.iter_mut().enumerate().take(channels) {
                    let step = energy[band + channel * BAND_COUNT] - *predicted;
                    assert!(
                        band + 6 < BAND_COUNT || step > 0.0,
                        "band {band} of {channels} channels stepped {step}"
                    );
                    *predicted += step - BETA_INTRA * step;
                }
            }
            let mut read = [0.0f32; 2 * BAND_COUNT];
            let mut decoder = RangeDecoder::new(&frame);
            coarse(
                &mut decoder,
                &mut read,
                &aim,
                0..BAND_COUNT,
                true,
                channels,
                3,
            );
            assert_eq!(read, energy, "{channels} channels");
        }
    }

    #[test]
    fn a_band_s_energy_falls_no_further_in_a_frame_than_its_bytes_let_it() {
        let aim = [-20.0f32; 2 * BAND_COUNT];
        let mut energy = [10.0f32; 2 * BAND_COUNT];
        let mut frame = [0u8; 100];
        let mut encoder = RangeEncoder::new(&mut frame);
        coarse(&mut encoder, &mut energy, &aim, 0..BAND_COUNT, false, 1, 3);
        assert!(encoder.finish());
        let fall = DECAY_PER_BYTE * frame.len() as f32;
        for (band, &level) in energy[..BAND_COUNT].iter().enumerate() {
            assert!(
                level >= 10.0 - fall - 1.0 && level < 0.0,
                "band {band}: {level}"
            );
        }
    }
}
