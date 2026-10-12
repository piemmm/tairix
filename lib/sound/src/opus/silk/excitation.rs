//! A frame's side information and excitation (RFC 6716, sections 4.2.7.3
//! to 4.2.7.8): signal type, gains, LSF indices, pitch and LTP, the seed,
//! then the pulses, shell-coded sixteen to a block, and their signs.

use super::fixed::{log2lin, smulwb};
use super::lpc::NLSF_QUANT_MAX_AMPLITUDE;
use super::tables::{
    CB_LAGS_STAGE2, CB_LAGS_STAGE2_10_MS, CB_LAGS_STAGE3, CB_LAGS_STAGE3_10_MS, DELTA_GAIN_ICDF,
    GAIN_ICDF, LSB_ICDF, LTP_GAIN_ICDF_0, LTP_GAIN_ICDF_1, LTP_GAIN_ICDF_2, LTP_GAIN_VQ_0,
    LTP_GAIN_VQ_1, LTP_GAIN_VQ_2, LTP_PER_INDEX_ICDF, LTP_SCALE_ICDF, NLSF_EXT_ICDF,
    NLSF_INTERPOLATION_ICDF, PITCH_DELTA_ICDF, PITCH_LAG_ICDF, PULSES_PER_BLOCK_ICDF,
    RATE_LEVELS_ICDF, SHELL_CODE_TABLE0, SHELL_CODE_TABLE1, SHELL_CODE_TABLE2, SHELL_CODE_TABLE3,
    SHELL_CODE_TABLE_OFFSETS, SIGN_ICDF, TYPE_OFFSET_NO_VAD_ICDF, TYPE_OFFSET_VAD_ICDF,
    UNIFORM4_ICDF, UNIFORM8_ICDF,
};
use super::{Channel, Coding, LTP_ORDER, MAX_FRAME_LENGTH, MAX_NB_SUBFR, TYPE_VOICED};
use crate::opus::range::RangeDecoder;

const SHELL_BLOCK: usize = 16;
const MAX_SHELL_BLOCKS: usize = MAX_FRAME_LENGTH / SHELL_BLOCK;
const MAX_PULSES: usize = 16;
const RATE_LEVELS: usize = 10;

const N_LEVELS_QGAIN: i32 = 64;
const MAX_DELTA_GAIN_QUANT: i32 = 36;
const MIN_DELTA_GAIN_QUANT: i32 = -4;
/// The log gain of index zero, and the step between indices, in Q7 and Q16.
const GAIN_OFFSET: i32 = (2 * 128) / 6 + 16 * 128;
const GAIN_INVERSE_SCALE_Q16: i32 = (65536 * (((88 - 2) * 128) / 6)) / (N_LEVELS_QGAIN - 1);

impl Channel {
    /// Decode the side information of frame `frame` of the packet; a
    /// redundant frame's always codes voice activity.
    pub(super) fn decode_indices(
        &mut self,
        decoder: &mut RangeDecoder<'_>,
        frame: usize,
        redundant: bool,
        coding: Coding,
    ) {
        let symbol = if redundant || self.vad[frame] {
            decoder.icdf(&TYPE_OFFSET_VAD_ICDF, 8) as i32 + 2
        } else {
            decoder.icdf(&TYPE_OFFSET_NO_VAD_ICDF, 8) as i32
        };
        let indices = &mut self.indices;
        indices.signal_type = symbol >> 1;
        indices.quant_offset = symbol & 1;

        if coding == Coding::Conditionally {
            indices.gains[0] = decoder.icdf(&DELTA_GAIN_ICDF, 8) as i8;
        } else {
            let high = decoder.icdf(&GAIN_ICDF[indices.signal_type as usize], 8) as i8;
            indices.gains[0] = (high << 3).wrapping_add(decoder.icdf(&UNIFORM8_ICDF, 8) as i8);
        }
        for gain in &mut indices.gains[1..self.nb_subfr] {
            *gain = decoder.icdf(&DELTA_GAIN_ICDF, 8) as i8;
        }

        let codebook = self.codebook;
        let class = (indices.signal_type >> 1) as usize;
        let first = decoder.icdf(&codebook.first_icdf[class * 32..], 8);
        indices.nlsf[0] = first as i8;
        let (distribution, _) = codebook.unpack(first);
        for (index, &start) in indices.nlsf[1..]
            .iter_mut()
            .zip(distribution.iter())
            .take(codebook.order)
        {
            let mut value = decoder.icdf(&codebook.second_icdf[start..], 8) as i32;
            if value == 0 {
                value -= decoder.icdf(&NLSF_EXT_ICDF, 8) as i32;
            } else if value == 2 * NLSF_QUANT_MAX_AMPLITUDE {
                value += decoder.icdf(&NLSF_EXT_ICDF, 8) as i32;
            }
            *index = (value - NLSF_QUANT_MAX_AMPLITUDE) as i8;
        }
        indices.nlsf_interpolation_q2 = if self.nb_subfr == MAX_NB_SUBFR {
            decoder.icdf(&NLSF_INTERPOLATION_ICDF, 8) as i32
        } else {
            4
        };

        if indices.signal_type == TYPE_VOICED {
            let mut absolute = true;
            if coding == Coding::Conditionally && self.ec_prev_signal_type == TYPE_VOICED {
                let delta = decoder.icdf(&PITCH_DELTA_ICDF, 8) as i32;
                if delta > 0 {
                    indices.lag = i32::from((self.ec_prev_lag + delta - 9) as i16);
                    absolute = false;
                }
            }
            if absolute {
                let high = decoder.icdf(&PITCH_LAG_ICDF, 8) as i32 * (self.fs_khz as i32 >> 1);
                indices.lag = i32::from((high + decoder.icdf(self.lag_low_icdf, 8) as i32) as i16);
            }
            self.ec_prev_lag = indices.lag;
            indices.contour = decoder.icdf(self.contour_icdf, 8) as i8;
            indices.periodicity = decoder.icdf(&LTP_PER_INDEX_ICDF, 8);
            let icdf: &[u8] = match indices.periodicity {
                0 => &LTP_GAIN_ICDF_0,
                1 => &LTP_GAIN_ICDF_1,
                _ => &LTP_GAIN_ICDF_2,
            };
            for ltp in &mut indices.ltp[..self.nb_subfr] {
                *ltp = decoder.icdf(icdf, 8) as i8;
            }
            indices.ltp_scale = if coding == Coding::Independently {
                decoder.icdf(&LTP_SCALE_ICDF, 8)
            } else {
                0
            };
        }
        self.ec_prev_signal_type = indices.signal_type;
        indices.seed = decoder.icdf(&UNIFORM4_ICDF, 8) as i32;
    }
}

/// The LTP codebook a frame's periodicity index names, Q7.
pub(super) fn ltp_codebook(periodicity: usize) -> &'static [[i8; LTP_ORDER]] {
    match periodicity {
        0 => &LTP_GAIN_VQ_0,
        1 => &LTP_GAIN_VQ_1,
        _ => &LTP_GAIN_VQ_2,
    }
}

/// The subframes' gains in Q16 from their indices, the first coded
/// absolutely or relative to the previous frame's last.
pub(super) fn dequantise_gains(
    gains_q16: &mut [i32; MAX_NB_SUBFR],
    indices: [i8; MAX_NB_SUBFR],
    previous: &mut i32,
    conditional: bool,
    nb_subfr: usize,
) {
    for k in 0..nb_subfr {
        let index = i32::from(indices[k]);
        if k == 0 && !conditional {
            *previous = index.max(*previous - 16);
        } else {
            let delta = index + MIN_DELTA_GAIN_QUANT;
            let threshold = 2 * MAX_DELTA_GAIN_QUANT - N_LEVELS_QGAIN + *previous;
            if delta > threshold {
                *previous += (delta << 1) - threshold;
            } else {
                *previous += delta;
            }
        }
        *previous = (*previous).clamp(0, N_LEVELS_QGAIN - 1);
        gains_q16[k] = log2lin((smulwb(GAIN_INVERSE_SCALE_Q16, *previous) + GAIN_OFFSET).min(3967));
    }
}

/// Each subframe's pitch lag from the frame's lag and contour.
pub(super) fn decode_pitch(
    lag: i32,
    contour: i8,
    pitch: &mut [i32; MAX_NB_SUBFR],
    fs_khz: usize,
    nb_subfr: usize,
) {
    let contour = usize::from(contour as u8);
    let offset = |k: usize| -> i32 {
        let row: &[i8] = match (fs_khz == 8, nb_subfr == MAX_NB_SUBFR) {
            (true, true) => &CB_LAGS_STAGE2[k],
            (true, false) => &CB_LAGS_STAGE2_10_MS[k],
            (false, true) => &CB_LAGS_STAGE3[k],
            (false, false) => &CB_LAGS_STAGE3_10_MS[k],
        };
        i32::from(row.get(contour).copied().unwrap_or(0))
    };
    let min_lag = 2 * fs_khz as i32;
    let max_lag = 18 * fs_khz as i32;
    let base = min_lag + lag;
    for (k, value) in pitch.iter_mut().enumerate().take(nb_subfr) {
        *value = (base + offset(k)).clamp(min_lag, max_lag);
    }
}

/// One split of a shell block: how many of `pulses` go to the first half.
fn split(decoder: &mut RangeDecoder<'_>, pulses: i16, table: &[u8; 152]) -> (i16, i16) {
    if pulses <= 0 {
        return (0, 0);
    }
    let start = usize::from(SHELL_CODE_TABLE_OFFSETS[pulses as usize]);
    let first = decoder.icdf(&table[start..], 8) as i16;
    (first, pulses - first)
}

/// A shell block's sixteen pulse magnitudes, `total` in all, halved
/// recursively.
fn shell_decode(decoder: &mut RangeDecoder<'_>, out: &mut [i16], total: i16) {
    let (a, b) = split(decoder, total, &SHELL_CODE_TABLE3);
    let mut quarters = [0i16; 4];
    let mut eighths = [0i16; 8];
    (quarters[0], quarters[1]) = split(decoder, a, &SHELL_CODE_TABLE2);
    (eighths[0], eighths[1]) = split(decoder, quarters[0], &SHELL_CODE_TABLE1);
    (out[0], out[1]) = split(decoder, eighths[0], &SHELL_CODE_TABLE0);
    (out[2], out[3]) = split(decoder, eighths[1], &SHELL_CODE_TABLE0);
    (eighths[2], eighths[3]) = split(decoder, quarters[1], &SHELL_CODE_TABLE1);
    (out[4], out[5]) = split(decoder, eighths[2], &SHELL_CODE_TABLE0);
    (out[6], out[7]) = split(decoder, eighths[3], &SHELL_CODE_TABLE0);
    (quarters[2], quarters[3]) = split(decoder, b, &SHELL_CODE_TABLE2);
    (eighths[4], eighths[5]) = split(decoder, quarters[2], &SHELL_CODE_TABLE1);
    (out[8], out[9]) = split(decoder, eighths[4], &SHELL_CODE_TABLE0);
    (out[10], out[11]) = split(decoder, eighths[5], &SHELL_CODE_TABLE0);
    (eighths[6], eighths[7]) = split(decoder, quarters[3], &SHELL_CODE_TABLE1);
    (out[12], out[13]) = split(decoder, eighths[6], &SHELL_CODE_TABLE0);
    (out[14], out[15]) = split(decoder, eighths[7], &SHELL_CODE_TABLE0);
}

/// The frame's excitation pulses: a rate level, each block's pulse count
/// and the low bits beyond it, the shell-coded magnitudes, then the signs.
pub(super) fn decode_pulses(
    decoder: &mut RangeDecoder<'_>,
    pulses: &mut [i16; MAX_FRAME_LENGTH],
    signal_type: i32,
    quant_offset: i32,
    frame_length: usize,
) {
    let rate_level = decoder.icdf(&RATE_LEVELS_ICDF[(signal_type >> 1) as usize], 8);
    let blocks = frame_length.div_ceil(SHELL_BLOCK).min(MAX_SHELL_BLOCKS);
    let mut sums = [0i32; MAX_SHELL_BLOCKS];
    let mut shifts = [0u32; MAX_SHELL_BLOCKS];
    for (sum, shift) in sums.iter_mut().zip(shifts.iter_mut()).take(blocks) {
        *sum = decoder.icdf(&PULSES_PER_BLOCK_ICDF[rate_level], 8) as i32;
        while *sum == MAX_PULSES as i32 + 1 {
            *shift += 1;
            let skip = usize::from(*shift == 10);
            *sum = decoder.icdf(&PULSES_PER_BLOCK_ICDF[RATE_LEVELS - 1][skip..], 8) as i32;
        }
    }
    for (block, &sum) in pulses
        .as_chunks_mut::<SHELL_BLOCK>()
        .0
        .iter_mut()
        .zip(&sums)
        .take(blocks)
    {
        if sum > 0 {
            shell_decode(decoder, block, sum as i16);
        } else {
            block.fill(0);
        }
    }
    for ((block, &shift), sum) in pulses
        .as_chunks_mut::<SHELL_BLOCK>()
        .0
        .iter_mut()
        .zip(&shifts)
        .zip(sums.iter_mut())
        .take(blocks)
    {
        if shift == 0 {
            continue;
        }
        for value in block.iter_mut() {
            let mut magnitude = i32::from(*value);
            for _ in 0..shift {
                magnitude = (magnitude << 1) + decoder.icdf(&LSB_ICDF, 8) as i32;
            }
            *value = magnitude as i16;
        }
        *sum |= (shift as i32) << 5;
    }
    let signs = &SIGN_ICDF[7 * (quant_offset + (signal_type << 1)) as usize..];
    let sign_blocks = (frame_length + SHELL_BLOCK / 2) >> 4;
    for (block, &sum) in pulses
        .as_chunks_mut::<SHELL_BLOCK>()
        .0
        .iter_mut()
        .zip(&sums)
        .take(sign_blocks)
    {
        if sum <= 0 {
            continue;
        }
        let icdf = [signs[(sum & 0x1F).min(6) as usize], 0];
        for value in block.iter_mut() {
            if *value > 0 {
                let positive = decoder.icdf(&icdf, 8) as i16;
                *value *= (positive << 1) - 1;
            }
        }
    }
}
