//! SILK's concealment of a missing frame and its comfort noise (RFC 6716,
//! section 4.4, and the reference implementation's `PLC.c` and `CNG.c`):
//! the last pitch period's excitation repeated with fading gains, or noise,
//! through the last LPC filter; the next good frame's energy glued to the
//! concealment's; and noise shaped like the background in the silences a
//! discontinuous stream leaves.

use super::fixed::{
    add_sat32, clz32, inverse32_varq, lshift, lshift_sat32, rand, rshift_round, sat16, smlawb,
    smulbb, smultt, smulwb, smulww, sqrt_approx,
};
use super::lpc::{bandwidth_expand, inverse_prediction_gain, nlsf_to_lpc};
use super::synthesis::analysis_filter;
use super::{
    lpc_prediction, output, Channel, Control, LTP_ORDER, MAX_FRAME_LENGTH, MAX_LPC_ORDER,
    MAX_NB_SUBFR, TYPE_NO_VOICE_ACTIVITY, TYPE_VOICED,
};

const HARMONIC_ATTENUATION_Q15: [i32; 2] = [32440, 31130];
const RANDOM_ATTENUATION_VOICED_Q15: [i32; 2] = [31130, 26214];
const RANDOM_ATTENUATION_UNVOICED_Q15: [i32; 2] = [32440, 29491];
/// `0.99` in Q16: the concealment's per-frame bandwidth expansion.
const BWE_Q16: i32 = 64881;
const V_PITCH_GAIN_START_MIN_Q14: i32 = 11469;
const V_PITCH_GAIN_START_MAX_Q14: i32 = 15565;
const MAX_PITCH_LAG_MS: i32 = 18;
const RANDOM_BUFFER: usize = 128;
const LOG2_INV_LPC_GAIN_HIGH: u32 = 3;
const LOG2_INV_LPC_GAIN_LOW: u32 = 8;
const PITCH_DRIFT_Q16: i32 = 655;
const MAX_LTP_MEMORY: usize = 320;

const CNG_BUF_MASK_MAX: i32 = 255;
const CNG_GAIN_SMOOTH_Q16: i32 = 4634;
const CNG_GAIN_SMOOTH_THRESHOLD_Q16: i32 = 46396;
const CNG_NLSF_SMOOTH_Q16: i32 = 16348;

/// What the concealment carries from the last good frame.
#[derive(Clone, Copy)]
pub(super) struct Plc {
    pitch_q8: i32,
    ltp_q14: [i16; LTP_ORDER],
    lpc_q12: [i16; MAX_LPC_ORDER],
    last_frame_lost: bool,
    seed: i32,
    random_scale_q14: i16,
    concealed_energy: i32,
    concealed_shift: u32,
    ltp_scale_q14: i32,
    gains_q16: [i32; 2],
    fs_khz: usize,
    nb_subfr: usize,
    subfr_length: usize,
}

impl Plc {
    pub(super) const fn new() -> Self {
        Self {
            pitch_q8: 0,
            ltp_q14: [0; LTP_ORDER],
            lpc_q12: [0; MAX_LPC_ORDER],
            last_frame_lost: false,
            seed: 0,
            random_scale_q14: 0,
            concealed_energy: 0,
            concealed_shift: 0,
            ltp_scale_q14: 0,
            gains_q16: [0; 2],
            fs_khz: 0,
            nb_subfr: 0,
            subfr_length: 0,
        }
    }
}

/// The comfort noise generator's smoothed background.
#[derive(Clone, Copy)]
pub(super) struct Cng {
    excitation_q14: [i32; MAX_FRAME_LENGTH],
    nlsf_q15: [i16; MAX_LPC_ORDER],
    synthesis_q14: [i32; MAX_LPC_ORDER],
    gain_q16: i32,
    seed: i32,
    fs_khz: usize,
}

impl Cng {
    pub(super) const fn new() -> Self {
        Self {
            excitation_q14: [0; MAX_FRAME_LENGTH],
            nlsf_q15: [0; MAX_LPC_ORDER],
            synthesis_q14: [0; MAX_LPC_ORDER],
            gain_q16: 0,
            seed: 0,
            fs_khz: 0,
        }
    }
}

/// `x`'s energy and the right shift that keeps it in 32 bits with two bits
/// to spare.
fn energy_with_shift(x: &[i16]) -> (i32, u32) {
    let pairs = |shift: u32, start: u32| {
        let mut energy = start;
        let (pairs, rest) = x.as_chunks::<2>();
        for &[first, second] in pairs {
            let a = smulbb(i32::from(first), i32::from(first)) as u32;
            let sum = a.wrapping_add(smulbb(i32::from(second), i32::from(second)) as u32);
            energy = energy.wrapping_add(sum >> shift);
        }
        if let [last] = rest {
            energy =
                energy.wrapping_add((smulbb(i32::from(*last), i32::from(*last)) as u32) >> shift);
        }
        energy as i32
    };
    let len = x.len() as i32;
    let shift = 31 - clz32(len);
    let energy = pairs(shift, len as u32);
    let shift = (shift as i32 + 3 - clz32(energy) as i32).max(0) as u32;
    (pairs(shift, 0), shift)
}

impl Channel {
    pub(super) fn reset_plc(&mut self) {
        self.plc.pitch_q8 = lshift(self.frame_length as i32, 7);
        self.plc.gains_q16 = [1 << 16; 2];
        self.plc.subfr_length = 20;
        self.plc.nb_subfr = 2;
    }

    pub(super) fn reset_cng(&mut self) {
        let step = i32::from(i16::MAX) / (self.lpc_order as i32 + 1);
        let mut accumulated = 0;
        for value in &mut self.cng.nlsf_q15[..self.lpc_order] {
            accumulated += step;
            *value = accumulated as i16;
        }
        self.cng.gain_q16 = 0;
        self.cng.seed = 3_176_576;
    }

    fn follow_rate(&mut self) {
        if self.fs_khz != self.plc.fs_khz {
            self.reset_plc();
            self.plc.fs_khz = self.fs_khz;
        }
    }

    /// Remember what a concealment of the next frame would need.
    pub(super) fn update_plc(&mut self, control: &Control) {
        self.follow_rate();
        let plc = &mut self.plc;
        self.prev_signal_type = self.indices.signal_type;
        let mut ltp_gain = 0;
        if self.indices.signal_type == TYPE_VOICED {
            let mut j = 0;
            while (j * self.subfr_length) < control.pitch[self.nb_subfr - 1].max(0) as usize {
                if j == self.nb_subfr {
                    break;
                }
                let subframe = self.nb_subfr - 1 - j;
                let taps = &control.ltp_q14[subframe * LTP_ORDER..(subframe + 1) * LTP_ORDER];
                let gain: i32 = taps.iter().map(|&tap| i32::from(tap)).sum();
                if gain > ltp_gain {
                    ltp_gain = gain;
                    plc.ltp_q14.copy_from_slice(taps);
                    plc.pitch_q8 = lshift(control.pitch[subframe], 8);
                }
                j += 1;
            }
            plc.ltp_q14 = [0; LTP_ORDER];
            plc.ltp_q14[LTP_ORDER / 2] = ltp_gain as i16;
            if ltp_gain < V_PITCH_GAIN_START_MIN_Q14 {
                let scale_q10 = lshift(V_PITCH_GAIN_START_MIN_Q14, 10) / ltp_gain.max(1);
                for tap in &mut plc.ltp_q14 {
                    *tap = (smulbb(i32::from(*tap), scale_q10) >> 10) as i16;
                }
            } else if ltp_gain > V_PITCH_GAIN_START_MAX_Q14 {
                let scale_q14 = lshift(V_PITCH_GAIN_START_MAX_Q14, 14) / ltp_gain.max(1);
                for tap in &mut plc.ltp_q14 {
                    *tap = (smulbb(i32::from(*tap), scale_q14) >> 14) as i16;
                }
            }
        } else {
            plc.pitch_q8 = lshift(smulbb(self.fs_khz as i32, 18), 8);
            plc.ltp_q14 = [0; LTP_ORDER];
        }
        plc.lpc_q12[..self.lpc_order].copy_from_slice(&control.lpc_q12[1][..self.lpc_order]);
        plc.ltp_scale_q14 = control.ltp_scale_q14;
        plc.gains_q16 = [
            control.gains_q16[self.nb_subfr - 2],
            control.gains_q16[self.nb_subfr - 1],
        ];
        plc.subfr_length = self.subfr_length;
        plc.nb_subfr = self.nb_subfr;
    }

    /// Conceal a frame into `out`.
    #[allow(
        clippy::too_many_lines,
        reason = "the reference's silk_PLC_conceal, kept whole so the port can be read against it"
    )]
    pub(super) fn conceal(&mut self, control: &mut Control, out: &mut [i16]) {
        self.follow_rate();
        let order = self.lpc_order;
        let plc = &mut self.plc;
        let gains_q10 = [plc.gains_q16[0] >> 6, plc.gains_q16[1] >> 6];
        if self.first_frame_after_reset {
            plc.lpc_q12 = [0; MAX_LPC_ORDER];
        }
        let subframe = self.subfr_length;
        let mut scaled = [0i16; 2 * super::MAX_SUB_FRAME_LENGTH];
        for k in 0..2 {
            for i in 0..subframe {
                let excitation = self.excitation_q14[i + (k + self.nb_subfr - 2) * subframe];
                scaled[k * subframe + i] = sat16(smulww(excitation, gains_q10[k]) >> 8);
            }
        }
        let (energy1, shift1) = energy_with_shift(&scaled[..subframe]);
        let (energy2, shift2) = energy_with_shift(&scaled[subframe..2 * subframe]);
        let random_start = if (energy1 >> shift2) < (energy2 >> shift1) {
            ((plc.nb_subfr - 1) * plc.subfr_length).saturating_sub(RANDOM_BUFFER)
        } else {
            (plc.nb_subfr * plc.subfr_length).saturating_sub(RANDOM_BUFFER)
        };
        let losses = (self.loss_count.max(0) as usize).min(1);
        let harmonic_gain = HARMONIC_ATTENUATION_Q15[losses];
        let mut random_gain = if self.prev_signal_type == TYPE_VOICED {
            RANDOM_ATTENUATION_VOICED_Q15[losses]
        } else {
            RANDOM_ATTENUATION_UNVOICED_Q15[losses]
        };
        bandwidth_expand(&mut plc.lpc_q12[..order], BWE_Q16);
        let a_q12 = plc.lpc_q12;
        let mut random_scale = plc.random_scale_q14;
        if self.loss_count == 0 {
            random_scale = 1 << 14;
            if self.prev_signal_type == TYPE_VOICED {
                for &tap in &plc.ltp_q14 {
                    random_scale = random_scale.wrapping_sub(tap);
                }
                random_scale = random_scale.max(3277);
                random_scale = (smulbb(i32::from(random_scale), plc.ltp_scale_q14) >> 14) as i16;
            } else {
                let inverse_gain = inverse_prediction_gain(&plc.lpc_q12[..order]);
                let down_scale = ((1 << 30) >> LOG2_INV_LPC_GAIN_HIGH)
                    .min(inverse_gain)
                    .max((1 << 30) >> LOG2_INV_LPC_GAIN_LOW);
                let down_scale = lshift(down_scale, LOG2_INV_LPC_GAIN_HIGH);
                random_gain = smulwb(down_scale, random_gain) >> 14;
            }
        }
        let mut seed = plc.seed;
        let mut lag = rshift_round(plc.pitch_q8, 8).max(0) as usize;
        let ltp_mem = self.ltp_mem_length;
        let mut ltp_state = [0i32; MAX_LTP_MEMORY + MAX_FRAME_LENGTH];
        let mut whitened = [0i16; MAX_LTP_MEMORY];
        let start = ltp_mem.saturating_sub(lag + order + LTP_ORDER / 2);
        analysis_filter(
            &mut whitened[start..ltp_mem],
            &self.out[start..ltp_mem],
            &a_q12,
            order,
        );
        let inverse_gain = inverse32_varq(plc.gains_q16[1], 46).min(i32::MAX >> 1);
        for i in start + order..ltp_mem {
            ltp_state[i] = smulwb(inverse_gain, i32::from(whitened[i]));
        }
        let mut index = ltp_mem;
        for _ in 0..self.nb_subfr {
            let lagged = (index + LTP_ORDER / 2).saturating_sub(lag);
            for (lagged, at) in (lagged..).zip(index..index + subframe) {
                let mut prediction = 2;
                for (j, &tap) in plc.ltp_q14.iter().enumerate() {
                    prediction = smlawb(
                        prediction,
                        ltp_state[lagged.saturating_sub(j)],
                        i32::from(tap),
                    );
                }
                seed = rand(seed);
                let pick = ((seed >> 25) as usize) & (RANDOM_BUFFER - 1);
                let noise = self.excitation_q14[random_start + pick];
                ltp_state[at] = lshift(smlawb(prediction, noise, i32::from(random_scale)), 2);
            }
            index += subframe;
            for tap in &mut plc.ltp_q14 {
                *tap = (smulbb(harmonic_gain, i32::from(*tap)) >> 15) as i16;
            }
            random_scale = (smulbb(i32::from(random_scale), random_gain) >> 15) as i16;
            plc.pitch_q8 = smlawb(plc.pitch_q8, plc.pitch_q8, PITCH_DRIFT_Q16);
            plc.pitch_q8 = plc
                .pitch_q8
                .min(lshift(smulbb(MAX_PITCH_LAG_MS, self.fs_khz as i32), 8));
            lag = rshift_round(plc.pitch_q8, 8).max(0) as usize;
        }
        let base = ltp_mem - MAX_LPC_ORDER;
        ltp_state[base..ltp_mem].copy_from_slice(&self.lpc_state_q14);
        for (i, out) in out.iter_mut().enumerate().take(self.frame_length) {
            let at = base + MAX_LPC_ORDER + i;
            let prediction = lpc_prediction(&ltp_state, at, &a_q12, order);
            ltp_state[at] = add_sat32(ltp_state[at], lshift_sat32(prediction, 4));
            *out = output(ltp_state[at], gains_q10[1]);
        }
        self.lpc_state_q14.copy_from_slice(
            &ltp_state[base + self.frame_length..base + self.frame_length + MAX_LPC_ORDER],
        );
        plc.seed = seed;
        plc.random_scale_q14 = random_scale;
        control.pitch = [lag as i32; MAX_NB_SUBFR];
        self.loss_count += 1;
    }

    /// Fade from a concealment's energy into the first good frame's.
    pub(super) fn glue_frames(&mut self, frame: &mut [i16]) {
        let plc = &mut self.plc;
        if self.loss_count != 0 {
            (plc.concealed_energy, plc.concealed_shift) = energy_with_shift(frame);
            plc.last_frame_lost = true;
            return;
        }
        if plc.last_frame_lost {
            let (mut energy, shift) = energy_with_shift(frame);
            if shift > plc.concealed_shift {
                plc.concealed_energy >>= shift - plc.concealed_shift;
            } else if shift < plc.concealed_shift {
                energy >>= plc.concealed_shift - shift;
            }
            if energy > plc.concealed_energy {
                let lz = clz32(plc.concealed_energy) as i32 - 1;
                plc.concealed_energy = lshift(plc.concealed_energy, lz.max(0) as u32);
                let energy = energy >> (24 - lz).max(0);
                let fraction_q24 = plc.concealed_energy / energy.max(1);
                let mut gain_q16 = lshift(sqrt_approx(fraction_q24), 4);
                let slope_q16 = lshift(((1 << 16) - gain_q16) / frame.len() as i32, 2);
                for sample in frame.iter_mut() {
                    *sample = smulwb(gain_q16, i32::from(*sample)) as i16;
                    gain_q16 += slope_q16;
                    if gain_q16 > 1 << 16 {
                        break;
                    }
                }
            }
        }
        plc.last_frame_lost = false;
    }

    /// Track the background while the stream is silent, and add noise
    /// shaped like it to a concealed frame.
    pub(super) fn comfort_noise(&mut self, control: &Control, frame: &mut [i16]) {
        if self.fs_khz != self.cng.fs_khz {
            self.reset_cng();
            self.cng.fs_khz = self.fs_khz;
        }
        let order = self.lpc_order;
        let cng = &mut self.cng;
        if self.loss_count == 0 && self.prev_signal_type == TYPE_NO_VOICE_ACTIVITY {
            for (smoothed, &nlsf) in cng.nlsf_q15[..order].iter_mut().zip(&self.prev_nlsf_q15) {
                let step = smulwb(i32::from(nlsf) - i32::from(*smoothed), CNG_NLSF_SMOOTH_Q16);
                *smoothed = (i32::from(*smoothed) + step) as i16;
            }
            let mut max_gain = 0;
            let mut loudest = 0;
            for (i, &gain) in control.gains_q16[..self.nb_subfr].iter().enumerate() {
                if gain > max_gain {
                    max_gain = gain;
                    loudest = i;
                }
            }
            let subframe = self.subfr_length;
            cng.excitation_q14
                .copy_within(0..(self.nb_subfr - 1) * subframe, subframe);
            cng.excitation_q14[..subframe].copy_from_slice(
                &self.excitation_q14[loudest * subframe..(loudest + 1) * subframe],
            );
            for &gain in &control.gains_q16[..self.nb_subfr] {
                cng.gain_q16 += smulwb(gain - cng.gain_q16, CNG_GAIN_SMOOTH_Q16);
                if smulww(cng.gain_q16, CNG_GAIN_SMOOTH_THRESHOLD_Q16) > gain {
                    cng.gain_q16 = gain;
                }
            }
        }
        if self.loss_count == 0 {
            cng.synthesis_q14[..order].fill(0);
            return;
        }
        let length = frame.len();
        let mut gain_q16 = smulww(i32::from(self.plc.random_scale_q14), self.plc.gains_q16[1]);
        if gain_q16 >= 1 << 21 || cng.gain_q16 > 1 << 23 {
            gain_q16 = smultt(gain_q16, gain_q16);
            gain_q16 = smultt(cng.gain_q16, cng.gain_q16).wrapping_sub(lshift(gain_q16, 5));
            gain_q16 = lshift(sqrt_approx(gain_q16), 16);
        } else {
            gain_q16 = smulww(gain_q16, gain_q16);
            gain_q16 = smulww(cng.gain_q16, cng.gain_q16).wrapping_sub(lshift(gain_q16, 5));
            gain_q16 = lshift(sqrt_approx(gain_q16), 8);
        }
        let gain_q10 = gain_q16 >> 6;
        let mut mask = CNG_BUF_MASK_MAX;
        while mask > length as i32 {
            mask >>= 1;
        }
        let mut signal = [0i32; MAX_FRAME_LENGTH + MAX_LPC_ORDER];
        for value in &mut signal[MAX_LPC_ORDER..MAX_LPC_ORDER + length] {
            cng.seed = rand(cng.seed);
            *value = cng.excitation_q14[((cng.seed >> 24) & mask) as usize];
        }
        let a_q12 = nlsf_to_lpc(&cng.nlsf_q15[..order]);
        signal[..MAX_LPC_ORDER].copy_from_slice(&cng.synthesis_q14);
        for (i, sample) in frame.iter_mut().enumerate() {
            let at = MAX_LPC_ORDER + i;
            let prediction = lpc_prediction(&signal, at, &a_q12, order);
            signal[at] = add_sat32(signal[at], lshift_sat32(prediction, 4));
            *sample = sat16(i32::from(*sample) + i32::from(output(signal[at], gain_q10)));
        }
        cng.synthesis_q14
            .copy_from_slice(&signal[length..length + MAX_LPC_ORDER]);
    }
}
