//! Concealing a frame that never arrived (RFC 6716, section 4.4): the last
//! pitch period extrapolated through the signal's LPC model, or, past the
//! first few losses and when the pitch is unreliable, noise at the bands'
//! fading energy.
//!
//! A file loses nothing, but Opus conceals in normal decoding too: a frame
//! sent empty — discontinuous transmission — and the 2.5 ms a switch out of
//! CELT is covered with.

use super::bands::lcg;
use super::pvq::{inner, renormalise};
use super::tables::{BANDS, WINDOW};
use super::{
    comb_filter, max16, sqrt, Celt, Separate, Work, BAND_COUNT, HISTORY, HISTORY_LEN, LPC_ORDER,
    MAX_PERIOD, OVERLAP, SHORT_BLOCK,
};

const PITCH_LAG_MAX: usize = 720;
const PITCH_LAG_MIN: usize = 100;

impl Celt {
    /// Conceal one frame at block size `lm` into the history.
    pub(super) fn conceal(&mut self, lm: usize, work: &mut Work) {
        let n = SHORT_BLOCK << lm;
        let noise = self.loss_duration >= 40 || self.start != 0 || self.skip_plc;
        if noise {
            self.conceal_with_noise(lm, n, work);
        } else {
            self.conceal_with_pitch(n);
        }
        self.loss_duration = (self.loss_duration + (1 << lm)).min(10000);
    }

    fn conceal_with_noise(&mut self, lm: usize, n: usize, work: &mut Work) {
        let channels = self.channels;
        for channel in 0..channels {
            let history = &mut self.history[channel * HISTORY_LEN..(channel + 1) * HISTORY_LEN];
            history.copy_within(n..HISTORY + OVERLAP / 2, 0);
        }
        let decay = if self.loss_duration == 0 { 1.5 } else { 0.5 };
        for channel in 0..channels {
            for band in self.start..self.end {
                let at = channel * BAND_COUNT + band;
                self.energy[at] = max16(self.background[at], self.energy[at] - decay);
            }
        }
        let mut seed = self.rng;
        let coefficients = &mut work.coefficients;
        for channel in 0..channels {
            for band in self.start..self.end {
                let at = n * channel + (BANDS[band] << lm);
                let len = (BANDS[band + 1] - BANDS[band]) << lm;
                for value in &mut coefficients[at..at + len] {
                    seed = lcg(seed);
                    *value = ((seed as i32) >> 20) as f32;
                }
                renormalise(&mut coefficients[at..at + len], 1.0);
            }
        }
        self.rng = seed;
        self.synthesise(lm, false, false, channels, work);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the reference's pitch-based concealment, kept whole so the port can be read against it"
    )]
    fn conceal_with_pitch(&mut self, n: usize) {
        let mut fade = 1.0;
        let pitch = if self.loss_duration == 0 {
            self.last_pitch = pitch_search(&self.history, self.channels);
            self.last_pitch
        } else {
            fade = 0.8;
            self.last_pitch
        };
        let excitation_len = (2 * pitch).min(MAX_PERIOD);
        let mut store = [0.0f32; MAX_PERIOD + LPC_ORDER];
        let mut filtered = [0.0f32; MAX_PERIOD];
        for channel in 0..self.channels {
            let buf = &mut self.history[channel * HISTORY_LEN..(channel + 1) * HISTORY_LEN];
            store.copy_from_slice(&buf[HISTORY - MAX_PERIOD - LPC_ORDER..HISTORY]);
            let lpc = &mut self.lpc[channel];
            if self.loss_duration == 0 {
                let mut ac = [0.0f32; LPC_ORDER + 1];
                autocorrelation(&store[LPC_ORDER..], &mut ac, Some(&WINDOW));
                ac[0] *= 1.0001;
                for (i, value) in ac.iter_mut().enumerate().skip(1) {
                    *value -= *value * (0.008 * 0.008) * i as f32 * i as f32;
                }
                lpc_from_autocorrelation(lpc, &ac);
            }
            let excitation_start = LPC_ORDER + MAX_PERIOD - excitation_len;
            fir(
                &store,
                excitation_start,
                lpc,
                &mut filtered[..excitation_len],
            );
            store[excitation_start..LPC_ORDER + MAX_PERIOD]
                .copy_from_slice(&filtered[..excitation_len]);
            let exc = &store[LPC_ORDER..];

            let decay_len = excitation_len >> 1;
            let mut e1 = 1.0f32;
            let mut e2 = 1.0f32;
            for i in 0..decay_len {
                let e = exc[MAX_PERIOD - decay_len + i];
                e1 += e * e;
                let e = exc[MAX_PERIOD - 2 * decay_len + i];
                e2 += e * e;
            }
            let e1 = super::min16(e1, e2);
            let decay = sqrt(e1 / e2);

            buf.copy_within(n..HISTORY, 0);
            let offset = MAX_PERIOD - pitch;
            let extrapolation = n + OVERLAP;
            let mut attenuation = fade * decay;
            let mut s1 = 0.0f32;
            let mut j = 0;
            for i in 0..extrapolation {
                if j >= pitch {
                    j -= pitch;
                    attenuation *= decay;
                }
                buf[HISTORY - n + i] = attenuation * exc[offset + j];
                let previous = buf[HISTORY - MAX_PERIOD - n + offset + j];
                s1 += previous * previous;
                j += 1;
            }
            let mut memory = [0.0f32; LPC_ORDER];
            for (i, value) in memory.iter_mut().enumerate() {
                *value = buf[HISTORY - n - 1 - i];
            }
            iir(
                &mut buf[HISTORY - n..HISTORY - n + extrapolation],
                lpc,
                &mut memory,
            );

            let s2 = buf[HISTORY - n..HISTORY - n + extrapolation]
                .iter()
                .fold(0.0f32, |sum, &value| sum + value * value);
            if s1.partial_cmp(&(0.2 * s2)) != Some(core::cmp::Ordering::Greater) {
                buf[HISTORY - n..HISTORY - n + extrapolation].fill(0.0);
            } else if s1 < s2 {
                let ratio = sqrt((s1 + 1.0) / (s2 + 1.0));
                for (i, value) in buf[HISTORY - n..HISTORY - n + OVERLAP]
                    .iter_mut()
                    .enumerate()
                {
                    let gain = 1.0 - WINDOW[i] * (1.0 - ratio);
                    *value *= gain;
                }
                for value in &mut buf[HISTORY - n + OVERLAP..HISTORY - n + extrapolation] {
                    *value *= ratio;
                }
            }

            let mut folded = [0.0f32; OVERLAP];
            let filter = self.filter;
            let inverse = super::PostFilter {
                gain: -filter.gain,
                ..filter
            };
            comb_filter(
                &mut Separate {
                    input: buf,
                    at: HISTORY,
                    output: &mut folded,
                },
                inverse,
                inverse,
                OVERLAP,
                None,
            );
            for i in 0..OVERLAP / 2 {
                buf[HISTORY + i] =
                    WINDOW[i] * folded[OVERLAP - 1 - i] + WINDOW[OVERLAP - i - 1] * folded[i];
            }
        }
    }
}

/// The lag of the history's strongest pitch period, searched on a signal
/// decimated by two and then by four.
#[allow(
    clippy::many_single_char_names,
    reason = "the reference's names, kept so the port can be read against it"
)]
fn pitch_search(history: &[f32], channels: usize) -> usize {
    let mut decimated = [0.0f32; HISTORY / 2];
    downsample(history, channels, &mut decimated);
    let x = &decimated[PITCH_LAG_MAX >> 1..];
    let y = &decimated[..];
    let len = HISTORY - PITCH_LAG_MAX;
    let max_pitch = PITCH_LAG_MAX - PITCH_LAG_MIN;
    let lag = len + max_pitch;
    let mut x4 = [0.0f32; (HISTORY - PITCH_LAG_MAX) >> 2];
    let mut y4 = [0.0f32; (HISTORY - PITCH_LAG_MIN) >> 2];
    for (j, value) in x4.iter_mut().enumerate().take(len >> 2) {
        *value = x[2 * j];
    }
    for (j, value) in y4.iter_mut().enumerate().take(lag >> 2) {
        *value = y[2 * j];
    }
    let mut xcorr = [0.0f32; (PITCH_LAG_MAX - PITCH_LAG_MIN) >> 1];
    for (i, value) in xcorr.iter_mut().enumerate().take(max_pitch >> 2) {
        *value = inner(&x4[..len >> 2], &y4[i..i + (len >> 2)]);
    }
    let coarse = best_pitch(&xcorr[..max_pitch >> 2], &y4, len >> 2);
    for (i, value) in xcorr.iter_mut().enumerate().take(max_pitch >> 1) {
        *value = 0.0;
        let near = |best: usize| (i as isize - 2 * best as isize).abs() <= 2;
        if !near(coarse[0]) && !near(coarse[1]) {
            continue;
        }
        let sum = inner(&x[..len >> 1], &y[i..i + (len >> 1)]);
        *value = max16(-1.0, sum);
    }
    let fine = best_pitch(&xcorr[..max_pitch >> 1], y, len >> 1);
    let best = fine[0];
    let offset = if best > 0 && best < (max_pitch >> 1) - 1 {
        let (a, b, c) = (xcorr[best - 1], xcorr[best], xcorr[best + 1]);
        if c - a > 0.7 * (b - a) {
            1
        } else if a - c > 0.7 * (b - c) {
            -1
        } else {
            0
        }
    } else {
        0
    };
    PITCH_LAG_MAX - (2 * best as isize - offset) as usize
}

/// The two lags of `xcorr` with the best normalised correlation.
fn best_pitch(xcorr: &[f32], y: &[f32], len: usize) -> [usize; 2] {
    let mut syy = y[..len]
        .iter()
        .fold(1.0f32, |sum, &value| sum + value * value);
    let mut best_num = [-1.0f32; 2];
    let mut best_den = [0.0f32; 2];
    let mut best = [0, 1];
    for (i, &correlation) in xcorr.iter().enumerate() {
        if correlation > 0.0 {
            let scaled = correlation * 1e-12;
            let num = scaled * scaled;
            if num * best_den[1] > best_num[1] * syy {
                if num * best_den[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_den[1] = best_den[0];
                    best[1] = best[0];
                    best_num[0] = num;
                    best_den[0] = syy;
                    best[0] = i;
                } else {
                    best_num[1] = num;
                    best_den[1] = syy;
                    best[1] = i;
                }
            }
        }
        syy += y[i + len] * y[i + len] - y[i] * y[i];
        syy = max16(1.0, syy);
    }
    best
}

/// The history decimated by two through a half-band filter, both channels
/// summed, then whitened by a fourth-order LPC with a zero.
fn downsample(history: &[f32], channels: usize, out: &mut [f32; HISTORY / 2]) {
    const C1: f32 = 0.8;
    let len = HISTORY;
    for channel in 0..channels {
        let x = &history[channel * HISTORY_LEN..];
        let sample = |i: usize| {
            if i == 0 {
                0.25 * x[1] + 0.5 * x[0]
            } else {
                0.25 * x[2 * i - 1] + 0.25 * x[2 * i + 1] + 0.5 * x[2 * i]
            }
        };
        for (i, value) in out.iter_mut().enumerate().take(len >> 1) {
            let decimated = sample(i);
            *value = if channel == 0 {
                decimated
            } else {
                *value + decimated
            };
        }
    }
    let mut ac = [0.0f32; 5];
    autocorrelation(&out[..len >> 1], &mut ac, None);
    ac[0] *= 1.0001;
    for (i, value) in ac.iter_mut().enumerate().skip(1) {
        let lag = 0.008 * i as f32;
        *value -= *value * lag * lag;
    }
    let mut lpc = [0.0f32; 4];
    lpc_from_autocorrelation(&mut lpc, &ac);
    let mut tmp = 1.0f32;
    for value in &mut lpc {
        tmp *= 0.9;
        *value *= tmp;
    }
    let taps = [
        lpc[0] + C1,
        lpc[1] + C1 * lpc[0],
        lpc[2] + C1 * lpc[1],
        lpc[3] + C1 * lpc[2],
        C1 * lpc[3],
    ];
    let mut memory = [0.0f32; 5];
    for value in &mut out[..len >> 1] {
        let mut sum = *value;
        for (tap, past) in taps.iter().zip(&memory) {
            sum += tap * past;
        }
        memory.copy_within(0..4, 1);
        memory[0] = *value;
        *value = sum;
    }
}

/// `ac[k] = Σ x[i] x[i + k]` for every lag `ac` holds, `x` windowed at both
/// ends first when `window` is given; summed in the reference's two parts.
fn autocorrelation(x: &[f32], ac: &mut [f32], window: Option<&[f32; OVERLAP]>) {
    let n = x.len();
    let lag = ac.len() - 1;
    let mut windowed = [0.0f32; MAX_PERIOD];
    let x = match window {
        Some(window) => {
            windowed[..n].copy_from_slice(x);
            for i in 0..OVERLAP {
                windowed[i] = x[i] * window[i];
                windowed[n - i - 1] = x[n - i - 1] * window[i];
            }
            &windowed[..n]
        }
        None => x,
    };
    let fast = n - lag;
    for (k, value) in ac.iter_mut().enumerate() {
        *value = inner(&x[..fast], &x[k..k + fast]);
    }
    for (k, value) in ac.iter_mut().enumerate() {
        let tail = (k + fast..n).fold(0.0f32, |sum, i| sum + x[i] * x[i - k]);
        *value += tail;
    }
}

/// The LPC coefficients of an autocorrelation, by Levinson-Durbin, stopping
/// once the prediction gain reaches 30 dB.
fn lpc_from_autocorrelation(lpc: &mut [f32], ac: &[f32]) {
    lpc.fill(0.0);
    let mut error = ac[0];
    if ac[0] <= 1e-10 {
        return;
    }
    for i in 0..lpc.len() {
        let mut rr = 0.0f32;
        for j in 0..i {
            rr += lpc[j] * ac[i - j];
        }
        rr += ac[i + 1];
        let r = -(rr / error);
        lpc[i] = r;
        for j in 0..(i + 1) >> 1 {
            let tmp1 = lpc[j];
            let tmp2 = lpc[i - 1 - j];
            lpc[j] = tmp1 + r * tmp2;
            lpc[i - 1 - j] = tmp2 + r * tmp1;
        }
        error -= r * r * error;
        if error <= 0.001 * ac[0] {
            return;
        }
    }
}

/// `y[i] = x[i] + Σ lpc[j] x[i - 1 - j]` over `x = store[start..]`, the
/// samples before `start` its past.
fn fir(store: &[f32], start: usize, lpc: &[f32; LPC_ORDER], y: &mut [f32]) {
    for (i, out) in y.iter_mut().enumerate() {
        let at = start + i;
        let mut sum = store[at];
        for j in 0..LPC_ORDER {
            sum += lpc[LPC_ORDER - 1 - j] * store[at + j - LPC_ORDER];
        }
        *out = sum;
    }
}

/// The all-pole synthesis filter `1 / A(z)` over `signal` in place, its past
/// outputs `memory` newest first; the reference's four-at-a-time form,
/// whose order of summation this keeps.
fn iir(signal: &mut [f32], lpc: &[f32; LPC_ORDER], memory: &mut [f32; LPC_ORDER]) {
    let n = signal.len();
    let mut reversed = [0.0f32; LPC_ORDER];
    for (i, value) in reversed.iter_mut().enumerate() {
        *value = lpc[LPC_ORDER - 1 - i];
    }
    let mut past = [0.0f32; HISTORY / 2 + LPC_ORDER + OVERLAP];
    for i in 0..LPC_ORDER {
        past[i] = -memory[LPC_ORDER - 1 - i];
    }
    let kernel = |past: &[f32], at: usize, mut sum: [f32; 4]| -> [f32; 4] {
        for (j, &tap) in reversed.iter().enumerate() {
            for (k, value) in sum.iter_mut().enumerate() {
                *value += tap * past[at + j + k];
            }
        }
        sum
    };
    let mut i = 0;
    while i + 3 < n {
        let mut sum = kernel(
            &past,
            i,
            [signal[i], signal[i + 1], signal[i + 2], signal[i + 3]],
        );
        past[i + LPC_ORDER] = -sum[0];
        signal[i] = sum[0];
        sum[1] += past[i + LPC_ORDER] * lpc[0];
        past[i + LPC_ORDER + 1] = -sum[1];
        signal[i + 1] = sum[1];
        sum[2] += past[i + LPC_ORDER + 1] * lpc[0];
        sum[2] += past[i + LPC_ORDER] * lpc[1];
        past[i + LPC_ORDER + 2] = -sum[2];
        signal[i + 2] = sum[2];
        sum[3] += past[i + LPC_ORDER + 2] * lpc[0];
        sum[3] += past[i + LPC_ORDER + 1] * lpc[1];
        sum[3] += past[i + LPC_ORDER] * lpc[2];
        past[i + LPC_ORDER + 3] = -sum[3];
        signal[i + 3] = sum[3];
        i += 4;
    }
    while i < n {
        let mut sum = signal[i];
        for (j, &tap) in reversed.iter().enumerate() {
            sum -= tap * past[i + j];
        }
        past[i + LPC_ORDER] = sum;
        signal[i] = sum;
        i += 1;
    }
    for (i, value) in memory.iter_mut().enumerate() {
        *value = signal[n - i - 1];
    }
}
