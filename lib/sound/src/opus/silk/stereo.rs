//! SILK's stereo coding (RFC 6716, sections 4.2.7.1 and 4.2.8): mid and
//! side, the side predicted from the mid by two coefficients interpolated
//! across the first 8 ms of each frame.

use super::fixed::{lshift, rshift_round, sat16, smlabb, smlawb, smulbb, smulwb};
use super::tables::{
    STEREO_ONLY_CODE_MID_ICDF, STEREO_PRED_JOINT_ICDF, STEREO_PRED_QUANT_Q13, UNIFORM3_ICDF,
    UNIFORM5_ICDF,
};
use crate::opus::range::RangeDecoder;

const INTERPOLATION_MS: usize = 8;
/// `0.5 / 5` in Q16: half a quantiser sub-step.
const SUB_STEP_Q16: i32 = 6554;

/// The mid and side signals' last samples and the last predictors.
#[derive(Clone, Copy, Default)]
pub(super) struct Stereo {
    prediction_q13: [i16; 2],
    mid: [i16; 2],
    side: [i16; 2],
}

/// The frame's two predictors, the first less the second.
pub(super) fn decode_prediction(decoder: &mut RangeDecoder<'_>) -> [i32; 2] {
    let joint = decoder.icdf(&STEREO_PRED_JOINT_ICDF, 8);
    let mut indices = [[0usize; 3]; 2];
    indices[0][2] = joint / 5;
    indices[1][2] = joint - 5 * indices[0][2];
    for index in &mut indices {
        index[0] = decoder.icdf(&UNIFORM3_ICDF, 8);
        index[1] = decoder.icdf(&UNIFORM5_ICDF, 8);
    }
    let mut prediction = [0i32; 2];
    for (out, index) in prediction.iter_mut().zip(&mut indices) {
        index[0] += 3 * index[2];
        let low = i32::from(STEREO_PRED_QUANT_Q13[index[0].min(15)]);
        let high = i32::from(STEREO_PRED_QUANT_Q13[(index[0] + 1).min(15)]);
        let step = smulwb(high - low, SUB_STEP_Q16);
        *out = smlabb(low, step, 2 * index[1] as i32 + 1);
    }
    prediction[0] -= prediction[1];
    prediction
}

/// Whether the frame codes its mid channel alone.
pub(super) fn decode_mid_only(decoder: &mut RangeDecoder<'_>) -> bool {
    decoder.icdf(&STEREO_ONLY_CODE_MID_ICDF, 8) != 0
}

impl Stereo {
    /// Forget the side, as a stream turning stereo does.
    pub(super) fn reset_prediction(&mut self) {
        self.prediction_q13 = [0; 2];
        self.side = [0; 2];
    }

    /// The last frame's predictors, which a concealed frame keeps.
    pub(super) fn previous_prediction(&self) -> [i32; 2] {
        self.prediction_q13.map(i32::from)
    }

    /// Delay a mono frame by the two samples a stereo one is delayed by:
    /// `x` is the frame after two samples of history.
    pub(super) fn buffer_mid(&mut self, x: &mut [i16], length: usize) {
        x[..2].copy_from_slice(&self.mid);
        self.mid.copy_from_slice(&x[length..length + 2]);
    }

    /// Turn mid and side into left and right in place, each frame after two
    /// samples of history.
    pub(super) fn ms_to_lr(
        &mut self,
        x1: &mut [i16],
        x2: &mut [i16],
        prediction: [i32; 2],
        fs_khz: usize,
        length: usize,
    ) {
        x1[..2].copy_from_slice(&self.mid);
        x2[..2].copy_from_slice(&self.side);
        self.mid.copy_from_slice(&x1[length..length + 2]);
        self.side.copy_from_slice(&x2[length..length + 2]);
        let mut pred0 = i32::from(self.prediction_q13[0]);
        let mut pred1 = i32::from(self.prediction_q13[1]);
        let interpolation = INTERPOLATION_MS * fs_khz;
        let denominator = (1 << 16) / interpolation as i32;
        let delta0 = rshift_round(smulbb(prediction[0] - pred0, denominator), 16);
        let delta1 = rshift_round(smulbb(prediction[1] - pred1, denominator), 16);
        for n in 0..length {
            if n < interpolation {
                pred0 += delta0;
                pred1 += delta1;
            } else if n == interpolation {
                pred0 = prediction[0];
                pred1 = prediction[1];
            }
            let sum = lshift(
                i32::from(x1[n]) + i32::from(x1[n + 2]) + (i32::from(x1[n + 1]) << 1),
                9,
            );
            let sum = smlawb(lshift(i32::from(x2[n + 1]), 8), sum, pred0);
            let sum = smlawb(sum, lshift(i32::from(x1[n + 1]), 11), pred1);
            x2[n + 1] = sat16(rshift_round(sum, 8));
        }
        self.prediction_q13 = [prediction[0] as i16, prediction[1] as i16];
        for n in 0..length {
            let mid = i32::from(x1[n + 1]);
            let side = i32::from(x2[n + 1]);
            x1[n + 1] = sat16(mid + side);
            x2[n + 1] = sat16(mid - side);
        }
    }
}
