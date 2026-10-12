//! SILK's resampler from its internal rate to 48 kHz (the reference
//! implementation's `resampler.c`): a 2x allpass upsampler, then a
//! twelve-phase interpolating FIR, after a fixed input delay.

use super::fixed::{lshift, rshift_round, sat16, smlabb, smlawb, smulbb, smulwb, smulww};
use super::tables::{RESAMPLER_FRAC_FIR_12, RESAMPLER_UP2_HQ};
use super::API_RATE;

const FIR_ORDER: usize = 8;
const BATCH_MS: usize = 10;
/// The longest batch: 10 ms at 16 kHz.
const MAX_BATCH: usize = BATCH_MS * 16;

/// The resampler's state for one channel.
#[derive(Clone, Default)]
pub(super) struct Resampler {
    iir: [i32; 6],
    fir: [i16; FIR_ORDER],
    delay: [i16; 16],
    batch: usize,
    inverse_ratio_q16: i32,
    fs_in_khz: usize,
    fs_out_khz: usize,
    input_delay: usize,
}

impl Resampler {
    /// A resampler from `fs_khz`, 8, 12 or 16, to 48 kHz.
    pub(super) fn new(fs_khz: usize) -> Self {
        let input_delay = match fs_khz {
            8 => 0,
            12 => 4,
            _ => 7,
        };
        let fs_in = (fs_khz * 1000) as i32;
        let mut inverse_ratio_q16 = lshift(lshift(fs_in, 15) / API_RATE, 2);
        while smulww(inverse_ratio_q16, API_RATE) < lshift(fs_in, 1) {
            inverse_ratio_q16 += 1;
        }
        Self {
            batch: fs_khz * BATCH_MS,
            inverse_ratio_q16,
            fs_in_khz: fs_khz,
            fs_out_khz: (API_RATE / 1000) as usize,
            input_delay,
            ..Self::default()
        }
    }

    /// Resample `input`, at least a millisecond, into `out`.
    pub(super) fn process(&mut self, out: &mut [i16], input: &[i16]) {
        let first = self.fs_in_khz - self.input_delay;
        self.delay[self.input_delay..self.fs_in_khz].copy_from_slice(&input[..first]);
        let delayed = self.delay;
        let split = self.fs_out_khz;
        let (head, tail) = out.split_at_mut(split);
        self.interpolate(head, &delayed[..self.fs_in_khz]);
        self.interpolate(tail, &input[first..input.len() - self.input_delay]);
        let len = input.len();
        self.delay[..self.input_delay].copy_from_slice(&input[len - self.input_delay..]);
    }

    /// Upsample by two, then interpolate to the output rate, in batches.
    fn interpolate(&mut self, mut out: &mut [i16], mut input: &[i16]) {
        let mut buffer = [0i16; 2 * MAX_BATCH + FIR_ORDER];
        buffer[..FIR_ORDER].copy_from_slice(&self.fir);
        let increment = self.inverse_ratio_q16;
        let mut taken;
        loop {
            taken = input.len().min(self.batch);
            up2(
                &mut self.iir,
                &mut buffer[FIR_ORDER..FIR_ORDER + 2 * taken],
                &input[..taken],
            );
            let max_index_q16 = lshift(taken as i32, 17);
            let mut index_q16 = 0;
            let mut written = 0;
            while index_q16 < max_index_q16 {
                let phase = smulwb(index_q16 & 0xFFFF, 12) as usize;
                let at = (index_q16 >> 16) as usize;
                let samples = &buffer[at..at + FIR_ORDER];
                let forward = &RESAMPLER_FRAC_FIR_12[phase];
                let backward = &RESAMPLER_FRAC_FIR_12[11 - phase];
                let mut sum = smulbb(i32::from(samples[0]), i32::from(forward[0]));
                sum = smlabb(sum, i32::from(samples[1]), i32::from(forward[1]));
                sum = smlabb(sum, i32::from(samples[2]), i32::from(forward[2]));
                sum = smlabb(sum, i32::from(samples[3]), i32::from(forward[3]));
                sum = smlabb(sum, i32::from(samples[4]), i32::from(backward[3]));
                sum = smlabb(sum, i32::from(samples[5]), i32::from(backward[2]));
                sum = smlabb(sum, i32::from(samples[6]), i32::from(backward[1]));
                sum = smlabb(sum, i32::from(samples[7]), i32::from(backward[0]));
                if let Some(slot) = out.get_mut(written) {
                    *slot = sat16(rshift_round(sum, 15));
                }
                written += 1;
                index_q16 += increment;
            }
            out = out.get_mut(written..).unwrap_or_default();
            input = &input[taken..];
            if input.is_empty() {
                break;
            }
            buffer.copy_within(taken << 1..(taken << 1) + FIR_ORDER, 0);
        }
        self.fir
            .copy_from_slice(&buffer[taken << 1..(taken << 1) + FIR_ORDER]);
    }
}

/// Upsample by two through two allpass chains, one for each phase.
fn up2(state: &mut [i32; 6], out: &mut [i16], input: &[i16]) {
    let [first, second] = RESAMPLER_UP2_HQ;
    for (k, &sample) in input.iter().enumerate() {
        let x = lshift(i32::from(sample), 10);
        for (phase, coefficients) in [first, second].iter().enumerate() {
            let s = &mut state[3 * phase..3 * phase + 3];
            let y = x.wrapping_sub(s[0]);
            let t = smulwb(y, i32::from(coefficients[0]));
            let out1 = s[0].wrapping_add(t);
            s[0] = x.wrapping_add(t);
            let y = out1.wrapping_sub(s[1]);
            let t = smulwb(y, i32::from(coefficients[1]));
            let out2 = s[1].wrapping_add(t);
            s[1] = out1.wrapping_add(t);
            let y = out2.wrapping_sub(s[2]);
            let t = smlawb(y, y, i32::from(coefficients[2]));
            let out1 = s[2].wrapping_add(t);
            s[2] = out2.wrapping_add(t);
            out[2 * k + phase] = sat16(rshift_round(out1, 10));
        }
    }
}
