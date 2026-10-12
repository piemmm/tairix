//! A frame's synthesis (RFC 6716, sections 4.2.7.9): the excitation from
//! its pulses and seed, the long-term predictor over the rewhitened past,
//! then the short-term LPC filter and the subframe's gain.

use super::fixed::{
    add_sat32, div32_varq, inverse32_varq, lshift, lshift_sat32, rand, rshift_round, sat16, smlawb,
    smulbb, smulwb, smulww,
};
use super::tables::QUANTIZATION_OFFSETS_Q10;
use super::{
    lpc_prediction, output, Channel, Control, LTP_ORDER, MAX_FRAME_LENGTH, MAX_LPC_ORDER,
    MAX_NB_SUBFR, TYPE_VOICED,
};

const QUANT_LEVEL_ADJUST_Q10: i32 = 80;
/// The longest LTP memory: 20 ms at 16 kHz.
const MAX_LTP_MEMORY: usize = 320;

/// `out[i] = in[i] − Σ b[j] in[i − 1 − j]` from sample `order` on, the first
/// `order` zeroed; wrapping as the reference allows.
pub(super) fn analysis_filter(out: &mut [i16], input: &[i16], b_q12: &[i16], order: usize) {
    for i in order..input.len() {
        let mut sum = 0i32;
        for (j, &coefficient) in b_q12[..order].iter().enumerate() {
            sum = sum.wrapping_add(smulbb(i32::from(input[i - 1 - j]), i32::from(coefficient)));
        }
        let residual = lshift(i32::from(input[i]), 12).wrapping_sub(sum);
        out[i] = sat16(rshift_round(residual, 12));
    }
    out[..order].fill(0);
}

impl Channel {
    /// Synthesise the frame into `out` from its pulses.
    #[allow(
        clippy::too_many_lines,
        reason = "the reference's silk_decode_core, kept whole so the port can be read against it"
    )]
    pub(super) fn synthesise(
        &mut self,
        control: &mut Control,
        out: &mut [i16],
        pulses: &[i16; MAX_FRAME_LENGTH],
    ) {
        let indices = self.indices;
        let offset_q10 = i32::from(
            QUANTIZATION_OFFSETS_Q10[(indices.signal_type >> 1) as usize]
                [indices.quant_offset as usize],
        );
        let interpolated = indices.nlsf_interpolation_q2 < 4;
        let mut seed = indices.seed;
        for (excitation, &pulse) in self
            .excitation_q14
            .iter_mut()
            .zip(pulses.iter())
            .take(self.frame_length)
        {
            seed = rand(seed);
            let mut value = lshift(i32::from(pulse), 14);
            if value > 0 {
                value -= QUANT_LEVEL_ADJUST_Q10 << 4;
            } else if value < 0 {
                value += QUANT_LEVEL_ADJUST_Q10 << 4;
            }
            value += offset_q10 << 4;
            if seed < 0 {
                value = -value;
            }
            *excitation = value;
            seed = seed.wrapping_add(i32::from(pulse));
        }

        let order = self.lpc_order;
        let subframe = self.subfr_length;
        let mut lpc_state = [0i32; MAX_SUB_FRAME + MAX_LPC_ORDER];
        lpc_state[..MAX_LPC_ORDER].copy_from_slice(&self.lpc_state_q14);
        let mut ltp_state = [0i32; MAX_LTP_MEMORY + MAX_FRAME_LENGTH];
        let mut rewhitened = [0i16; MAX_LTP_MEMORY];
        let mut residual = [0i32; MAX_SUB_FRAME];
        let mut ltp_index = self.ltp_mem_length;
        for k in 0..self.nb_subfr {
            let a_q12 = control.lpc_q12[k >> 1];
            let gain = control.gains_q16[k];
            let gain_q10 = gain >> 6;
            let mut inverse_gain = inverse32_varq(gain, 47);
            let adjustment = if gain == self.prev_gain_q16 {
                1 << 16
            } else {
                let adjustment = div32_varq(self.prev_gain_q16, gain, 16);
                for value in &mut lpc_state[..MAX_LPC_ORDER] {
                    *value = smulww(adjustment, *value);
                }
                adjustment
            };
            self.prev_gain_q16 = gain;
            let mut signal_type = indices.signal_type;
            if self.loss_count != 0
                && self.prev_signal_type == TYPE_VOICED
                && indices.signal_type != TYPE_VOICED
                && k < MAX_NB_SUBFR / 2
            {
                let taps = &mut control.ltp_q14[k * LTP_ORDER..(k + 1) * LTP_ORDER];
                taps.fill(0);
                taps[LTP_ORDER / 2] = 4096;
                signal_type = TYPE_VOICED;
                control.pitch[k] = self.lag_prev;
            }
            let excitation = &self.excitation_q14[k * subframe..(k + 1) * subframe];
            if signal_type == TYPE_VOICED {
                let lag = control.pitch[k].max(0) as usize;
                if k == 0 || (k == 2 && interpolated) {
                    let start = self
                        .ltp_mem_length
                        .saturating_sub(lag + order + LTP_ORDER / 2);
                    if k == 2 {
                        self.out[self.ltp_mem_length..self.ltp_mem_length + 2 * subframe]
                            .copy_from_slice(&out[..2 * subframe]);
                    }
                    let source =
                        &self.out[start + k * subframe..self.ltp_mem_length + k * subframe];
                    analysis_filter(
                        &mut rewhitened[start..self.ltp_mem_length],
                        source,
                        &a_q12,
                        order,
                    );
                    if k == 0 {
                        inverse_gain = lshift(smulwb(inverse_gain, control.ltp_scale_q14), 2);
                    }
                    for i in 0..(lag + LTP_ORDER / 2).min(self.ltp_mem_length) {
                        ltp_state[ltp_index - i - 1] = smulwb(
                            inverse_gain,
                            i32::from(rewhitened[self.ltp_mem_length - i - 1]),
                        );
                    }
                } else if adjustment != 1 << 16 {
                    for i in 0..(lag + LTP_ORDER / 2).min(ltp_index) {
                        let at = ltp_index - i - 1;
                        ltp_state[at] = smulww(adjustment, ltp_state[at]);
                    }
                }
                let taps = &control.ltp_q14[k * LTP_ORDER..(k + 1) * LTP_ORDER];
                let lagged = ltp_index + LTP_ORDER / 2 - lag;
                for (i, (value, &coded)) in residual[..subframe]
                    .iter_mut()
                    .zip(excitation.iter())
                    .enumerate()
                {
                    let mut prediction = 2;
                    for (j, &tap) in taps.iter().enumerate() {
                        prediction = smlawb(prediction, ltp_state[lagged + i - j], i32::from(tap));
                    }
                    *value = coded.wrapping_add(lshift(prediction, 1));
                    ltp_state[ltp_index + i] = lshift(*value, 1);
                }
                ltp_index += subframe;
            } else {
                residual[..subframe].copy_from_slice(excitation);
            }
            for i in 0..subframe {
                let at = MAX_LPC_ORDER + i;
                let prediction = lpc_prediction(&lpc_state, at, &a_q12, order);
                lpc_state[at] = add_sat32(residual[i], lshift_sat32(prediction, 4));
                out[k * subframe + i] = output(lpc_state[at], gain_q10);
            }
            lpc_state.copy_within(subframe..subframe + MAX_LPC_ORDER, 0);
        }
        self.lpc_state_q14
            .copy_from_slice(&lpc_state[..MAX_LPC_ORDER]);
    }
}

const MAX_SUB_FRAME: usize = super::MAX_SUB_FRAME_LENGTH;
