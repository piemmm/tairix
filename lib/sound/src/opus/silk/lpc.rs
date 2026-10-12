//! The short-term predictor (RFC 6716, sections 4.2.7.5.1 to 4.2.7.5.8):
//! normalised line spectral frequencies decoded from their two-stage
//! codebook, stabilised, and turned into LPC coefficients that are
//! guaranteed stable.

use super::fixed::{
    clz32, inverse32_varq, lshift, rshift_round, rshift_round64, sat16, smlawb, smmul, smulbb,
    smulww, sub_sat32,
};
use super::tables::{
    LSF_COS_Q12, NLSF_CB1_ICDF_NB_MB, NLSF_CB1_ICDF_WB, NLSF_CB1_NB_MB_Q8, NLSF_CB1_WB_Q8,
    NLSF_CB1_WGHT_NB_MB_Q9, NLSF_CB1_WGHT_WB_Q9, NLSF_CB2_ICDF_NB_MB, NLSF_CB2_ICDF_WB,
    NLSF_CB2_SELECT_NB_MB, NLSF_CB2_SELECT_WB, NLSF_DELTA_MIN_NB_MB_Q15, NLSF_DELTA_MIN_WB_Q15,
    NLSF_PRED_NB_MB_Q8, NLSF_PRED_WB_Q8,
};
use super::MAX_LPC_ORDER;

/// The most amplitude an LSF residual's index states before its extension.
pub(super) const NLSF_QUANT_MAX_AMPLITUDE: i32 = 4;

/// One of the two LSF codebooks.
pub(super) struct Codebook {
    pub(super) order: usize,
    quant_step_q16: i32,
    first: &'static [u8],
    weights_q9: &'static [i16],
    pub(super) first_icdf: &'static [u8; 64],
    predictors_q8: &'static [u8],
    select: &'static [u8],
    pub(super) second_icdf: &'static [u8; 72],
    delta_min_q15: &'static [i16],
}

/// The narrow and medium band codebook, of order ten.
pub(super) const NARROW: Codebook = Codebook {
    order: 10,
    quant_step_q16: 11796,
    first: &NLSF_CB1_NB_MB_Q8,
    weights_q9: &NLSF_CB1_WGHT_NB_MB_Q9,
    first_icdf: &NLSF_CB1_ICDF_NB_MB,
    predictors_q8: &NLSF_PRED_NB_MB_Q8,
    select: &NLSF_CB2_SELECT_NB_MB,
    second_icdf: &NLSF_CB2_ICDF_NB_MB,
    delta_min_q15: &NLSF_DELTA_MIN_NB_MB_Q15,
};

/// The wideband codebook, of order sixteen.
pub(super) const WIDE: Codebook = Codebook {
    order: 16,
    quant_step_q16: 9830,
    first: &NLSF_CB1_WB_Q8,
    weights_q9: &NLSF_CB1_WGHT_WB_Q9,
    first_icdf: &NLSF_CB1_ICDF_WB,
    predictors_q8: &NLSF_PRED_WB_Q8,
    select: &NLSF_CB2_SELECT_WB,
    second_icdf: &NLSF_CB2_ICDF_WB,
    delta_min_q15: &NLSF_DELTA_MIN_WB_Q15,
};

/// The most iterations the stabiliser and the stability fit take.
const MAX_STABILIZE_LOOPS: usize = 20;
const MAX_LPC_STABILIZE_ITERATIONS: u32 = 16;
/// `NLSF_QUANT_LEVEL_ADJ` in Q10.
const NLSF_LEVEL_ADJUST_Q10: i32 = 102;

impl Codebook {
    /// Where each coefficient's second-stage distribution starts, and its
    /// backward predictor, for first-stage vector `index`.
    pub(super) fn unpack(&self, index: usize) -> ([usize; MAX_LPC_ORDER], [u8; MAX_LPC_ORDER]) {
        let mut distribution = [0usize; MAX_LPC_ORDER];
        let mut predictor = [0u8; MAX_LPC_ORDER];
        let order = self.order;
        let selects = &self.select[index * order / 2..(index + 1) * order / 2];
        for (pair, &entry) in selects.iter().enumerate() {
            let i = 2 * pair;
            let entry = usize::from(entry);
            distribution[i] = ((entry >> 1) & 7) * (2 * NLSF_QUANT_MAX_AMPLITUDE as usize + 1);
            predictor[i] = self.predictors_q8[i + (entry & 1) * (order - 1)];
            distribution[i + 1] = ((entry >> 5) & 7) * (2 * NLSF_QUANT_MAX_AMPLITUDE as usize + 1);
            predictor[i + 1] = self.predictors_q8[i + ((entry >> 4) & 1) * (order - 1) + 1];
        }
        (distribution, predictor)
    }

    /// The stabilised NLSFs `indices` state: the first-stage vector, then
    /// each coefficient's residual.
    pub(super) fn decode(&self, indices: &[i8; MAX_LPC_ORDER + 1]) -> [i16; MAX_LPC_ORDER] {
        let order = self.order;
        let first = usize::from(indices[0] as u8);
        let (_, predictor) = self.unpack(first);
        let mut residual = [0i32; MAX_LPC_ORDER];
        let mut out_q10 = 0i32;
        for i in (0..order).rev() {
            let prediction = smulbb(out_q10, i32::from(predictor[i])) >> 8;
            out_q10 = i32::from(indices[i + 1]) << 10;
            if out_q10 > 0 {
                out_q10 -= NLSF_LEVEL_ADJUST_Q10;
            } else if out_q10 < 0 {
                out_q10 += NLSF_LEVEL_ADJUST_Q10;
            }
            out_q10 = smlawb(prediction, out_q10, self.quant_step_q16);
            residual[i] = out_q10;
        }
        let base = &self.first[first * order..(first + 1) * order];
        let weights = &self.weights_q9[first * order..(first + 1) * order];
        let mut nlsf = [0i16; MAX_LPC_ORDER];
        for i in 0..order {
            let weight = i32::from(weights[i]);
            let scaled = if weight == 0 {
                0
            } else {
                lshift(i32::from(residual[i] as i16), 14) / weight
            };
            let value = scaled + (i32::from(base[i]) << 7);
            nlsf[i] = value.clamp(0, 32767) as i16;
        }
        stabilize(&mut nlsf[..order], self.delta_min_q15);
        nlsf
    }
}

/// Space the NLSFs at least the codebook's minimum apart and inside
/// `(0, 1)`.
pub(super) fn stabilize(nlsf: &mut [i16], delta_min: &[i16]) {
    let len = nlsf.len();
    let delta = |i: usize| i32::from(delta_min[i]);
    for _ in 0..MAX_STABILIZE_LOOPS {
        let mut min_diff = i32::from(nlsf[0]) - delta(0);
        let mut at = 0;
        for i in 1..len {
            let diff = i32::from(nlsf[i]) - (i32::from(nlsf[i - 1]) + delta(i));
            if diff < min_diff {
                min_diff = diff;
                at = i;
            }
        }
        let diff = (1 << 15) - (i32::from(nlsf[len - 1]) + delta(len));
        if diff < min_diff {
            min_diff = diff;
            at = len;
        }
        if min_diff >= 0 {
            return;
        }
        if at == 0 {
            nlsf[0] = delta(0) as i16;
        } else if at == len {
            nlsf[len - 1] = ((1 << 15) - delta(len)) as i16;
        } else {
            let min_center = (0..at).map(delta).sum::<i32>() + (delta(at) >> 1);
            let max_center =
                (1 << 15) - ((at + 1)..=len).map(delta).sum::<i32>() - (delta(at) >> 1);
            let center = super::fixed::limit(
                rshift_round(i32::from(nlsf[at - 1]) + i32::from(nlsf[at]), 1),
                min_center,
                max_center,
            ) as i16;
            nlsf[at - 1] = (i32::from(center) - (delta(at) >> 1)) as i16;
            nlsf[at] = (i32::from(nlsf[at - 1]) + delta(at)) as i16;
        }
    }
    nlsf.sort_unstable();
    nlsf[0] = nlsf[0].max(delta_min[0]);
    for i in 1..len {
        let floor = sat16(i32::from(nlsf[i - 1]) + delta(i));
        nlsf[i] = nlsf[i].max(floor);
    }
    nlsf[len - 1] = nlsf[len - 1].min(((1 << 15) - delta(len)) as i16);
    for i in (0..len - 1).rev() {
        nlsf[i] = nlsf[i].min((i32::from(nlsf[i + 1]) - delta(i + 1)) as i16);
    }
}

const QA: u32 = 16;

/// One of the two polynomials whose roots the interleaved cosines are.
fn find_poly(out: &mut [i32; MAX_LPC_ORDER / 2 + 1], cos: &[i32], dd: usize) {
    out[0] = 1 << QA;
    out[1] = -cos[0];
    for k in 1..dd {
        let f = i64::from(cos[2 * k]);
        out[k + 1] =
            lshift(out[k - 1], 1).wrapping_sub(rshift_round64(f * i64::from(out[k]), QA) as i32);
        for n in (2..=k).rev() {
            let step =
                out[n - 2].wrapping_sub(rshift_round64(f * i64::from(out[n - 1]), QA) as i32);
            out[n] = out[n].wrapping_add(step);
        }
        out[1] = out[1].wrapping_sub(cos[2 * k]);
    }
}

/// The Q12 LPC coefficients of the NLSFs, bandwidth-expanded until the
/// filter is stable.
pub(super) fn nlsf_to_lpc(nlsf: &[i16]) -> [i16; MAX_LPC_ORDER] {
    const ORDERING16: [usize; 16] = [0, 15, 8, 7, 4, 11, 12, 3, 2, 13, 10, 5, 6, 9, 14, 1];
    const ORDERING10: [usize; 10] = [0, 9, 6, 3, 4, 5, 8, 1, 2, 7];
    let d = nlsf.len();
    let ordering: &[usize] = if d == 16 { &ORDERING16 } else { &ORDERING10 };
    let mut cos = [0i32; MAX_LPC_ORDER];
    for (k, &value) in nlsf.iter().enumerate() {
        let value = i32::from(value.max(0));
        let index = (value >> 8).min(127) as usize;
        let frac = value - ((index as i32) << 8);
        let start = i32::from(LSF_COS_Q12[index]);
        let delta = i32::from(LSF_COS_Q12[index + 1]) - start;
        cos[ordering[k]] = rshift_round(lshift(start, 8) + delta * frac, 20 - QA);
    }
    let dd = d >> 1;
    let mut p = [0i32; MAX_LPC_ORDER / 2 + 1];
    let mut q = [0i32; MAX_LPC_ORDER / 2 + 1];
    find_poly(&mut p, &cos, dd);
    find_poly(&mut q, &cos[1..], dd);
    let mut a32 = [0i32; MAX_LPC_ORDER];
    for k in 0..dd {
        let ptmp = p[k + 1].wrapping_add(p[k]);
        let qtmp = q[k + 1].wrapping_sub(q[k]);
        a32[k] = qtmp.wrapping_neg().wrapping_sub(ptmp);
        a32[d - k - 1] = qtmp.wrapping_sub(ptmp);
    }
    let mut a_q12 = [0i16; MAX_LPC_ORDER];
    fit(&mut a_q12[..d], &mut a32[..d], 12, QA + 1);
    let mut i = 0;
    while inverse_prediction_gain(&a_q12[..d]) == 0 && i < MAX_LPC_STABILIZE_ITERATIONS {
        bandwidth_expand32(&mut a32[..d], 65536 - lshift(2, i));
        for (out, &value) in a_q12[..d].iter_mut().zip(&a32[..d]) {
            *out = rshift_round(value, QA + 1 - 12) as i16;
        }
        i += 1;
    }
    a_q12
}

/// Coefficients in Q`q_in` brought to Q`q_out` and into 16 bits, shrinking
/// the filter's bandwidth until they fit.
fn fit(out: &mut [i16], a: &mut [i32], q_out: u32, q_in: u32) {
    let shift = q_in - q_out;
    let mut iterations = 0;
    while iterations < 10 {
        let (index, largest) = a
            .iter()
            .enumerate()
            .fold((0, 0i32), |(at, most), (k, &value)| {
                let magnitude = value.wrapping_abs();
                if magnitude > most {
                    (k, magnitude)
                } else {
                    (at, most)
                }
            });
        let largest = rshift_round(largest, shift);
        if largest <= i32::from(i16::MAX) {
            break;
        }
        let largest = largest.min(163_838);
        let chirp = 65470
            - lshift(largest - i32::from(i16::MAX), 14) / ((largest * (index as i32 + 1)) >> 2);
        bandwidth_expand32(a, chirp);
        iterations += 1;
    }
    if iterations == 10 {
        for (o, value) in out.iter_mut().zip(a.iter_mut()) {
            *o = sat16(rshift_round(*value, shift));
            *value = lshift(i32::from(*o), shift);
        }
    } else {
        for (o, &value) in out.iter_mut().zip(a.iter()) {
            *o = rshift_round(value, shift) as i16;
        }
    }
}

/// Shrink a filter's bandwidth by `chirp`, in Q16.
pub(super) fn bandwidth_expand32(a: &mut [i32], mut chirp: i32) {
    let minus_one = chirp - 65536;
    let last = a.len() - 1;
    for value in &mut a[..last] {
        *value = smulww(chirp, *value);
        chirp += rshift_round(chirp.wrapping_mul(minus_one), 16);
    }
    a[last] = smulww(chirp, a[last]);
}

/// [`bandwidth_expand32`] for Q12 coefficients, rounding exactly.
pub(super) fn bandwidth_expand(a: &mut [i16], mut chirp: i32) {
    let minus_one = chirp - 65536;
    let last = a.len() - 1;
    for value in &mut a[..last] {
        *value = rshift_round(chirp * i32::from(*value), 16) as i16;
        chirp += rshift_round(chirp * minus_one, 16);
    }
    a[last] = rshift_round(chirp * i32::from(a[last]), 16) as i16;
}

const QA_GAIN: u32 = 24;
/// `0.99975` in Q24: a reflection coefficient beyond it is too near
/// instability.
const A_LIMIT: i32 = 16_773_022;
/// The least inverse prediction gain allowed, `1e-4` in Q30.
const MIN_INVERSE_GAIN_Q30: i32 = 107_374;

const fn frac_q(a: i32, b: i32, q: u32) -> i32 {
    rshift_round64(a as i64 * b as i64, q) as i32
}

/// The filter's inverse prediction gain in Q30, or zero if it is unstable
/// or too nearly so.
pub(super) fn inverse_prediction_gain(a_q12: &[i16]) -> i32 {
    let order = a_q12.len();
    let mut a = [0i32; MAX_LPC_ORDER];
    let mut dc = 0;
    for (out, &value) in a.iter_mut().zip(a_q12) {
        dc += i32::from(value);
        *out = i32::from(value) << (QA_GAIN - 12);
    }
    if dc >= 4096 {
        return 0;
    }
    let a = &mut a[..order];
    let mut inverse_gain = 1 << 30;
    for k in (1..order).rev() {
        if a[k] > A_LIMIT || a[k] < -A_LIMIT {
            return 0;
        }
        let rc = -lshift(a[k], 31 - QA_GAIN);
        let rc_mult1 = (1 << 30) - smmul(rc, rc);
        inverse_gain = lshift(smmul(inverse_gain, rc_mult1), 2);
        if inverse_gain < MIN_INVERSE_GAIN_Q30 {
            return 0;
        }
        let mult2_q = 32 - clz32(rc_mult1.wrapping_abs());
        let rc_mult2 = inverse32_varq(rc_mult1, mult2_q as i32 + 30);
        for n in 0..(k + 1) >> 1 {
            let tmp1 = a[n];
            let tmp2 = a[k - n - 1];
            let first = rshift_round64(
                i64::from(sub_sat32(tmp1, frac_q(tmp2, rc, 31))) * i64::from(rc_mult2),
                mult2_q,
            );
            if first > i64::from(i32::MAX) || first < i64::from(i32::MIN) {
                return 0;
            }
            a[n] = first as i32;
            let second = rshift_round64(
                i64::from(sub_sat32(tmp2, frac_q(tmp1, rc, 31))) * i64::from(rc_mult2),
                mult2_q,
            );
            if second > i64::from(i32::MAX) || second < i64::from(i32::MIN) {
                return 0;
            }
            a[k - n - 1] = second as i32;
        }
    }
    if a[0] > A_LIMIT || a[0] < -A_LIMIT {
        return 0;
    }
    let rc = -lshift(a[0], 31 - QA_GAIN);
    let rc_mult1 = (1 << 30) - smmul(rc, rc);
    inverse_gain = lshift(smmul(inverse_gain, rc_mult1), 2);
    if inverse_gain < MIN_INVERSE_GAIN_Q30 {
        return 0;
    }
    inverse_gain
}

#[cfg(test)]
mod tests {
    use super::{inverse_prediction_gain, nlsf_to_lpc, stabilize, NARROW, WIDE};

    #[test]
    fn every_first_stage_vector_decodes_to_a_stable_filter() {
        for codebook in [&NARROW, &WIDE] {
            for first in 0..32i8 {
                let mut indices = [0i8; 17];
                indices[0] = first;
                for (i, value) in indices[1..=codebook.order].iter_mut().enumerate() {
                    *value = (i as i8 % 9) - 4;
                }
                let nlsf = codebook.decode(&indices);
                let nlsf = &nlsf[..codebook.order];
                assert!(nlsf.windows(2).all(|pair| pair[0] < pair[1]), "{first}");
                let lpc = nlsf_to_lpc(nlsf);
                assert!(
                    inverse_prediction_gain(&lpc[..codebook.order]) > 0,
                    "{first}"
                );
            }
        }
    }

    #[test]
    fn stabilising_spaces_crowded_frequencies() {
        let mut nlsf = [100i16, 90, 95, 30000, 32760, 32767, 5000, 5001, 5002, 20000];
        stabilize(&mut nlsf, &[250, 3, 6, 3, 3, 3, 4, 3, 3, 3, 461]);
        assert!(nlsf.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(nlsf[0] >= 250 && i32::from(nlsf[9]) <= 32768 - 461);
    }
}
