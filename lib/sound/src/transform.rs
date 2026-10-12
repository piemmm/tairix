//! The shared fast transform (`plans/SOUND.md` §The shared transform): one
//! mixed-radix complex FFT over radices 2, 3, 4 and 5, and the cosine
//! transforms the decoders reach it through — the DCT-IV, the MDCT pair over
//! it, and the DCT-II.
//!
//! The FFT is the recursive mixed-radix decomposition KISS FFT uses, its
//! butterflies the same; the DCT-IV and the MDCT ride a half- and a
//! quarter-length FFT, and the DCT-II a half-length one over its real input
//! (Makhoul, IEEE Trans. ASSP 28(1), 1980). Every constant is worked in double
//! precision and rounded once.

use alloc::vec::Vec;
use core::ops::{Add, Mul, Sub};

use tairix_util::fallible::filled;
use tairix_util::mathf::{cos, sin};

use crate::single;

/// A complex value.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Complex {
    pub(crate) re: f32,
    pub(crate) im: f32,
}

impl Complex {
    pub(crate) const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    const fn scale(self, by: f32) -> Self {
        Self::new(self.re * by, self.im * by)
    }

    const fn conj(self) -> Self {
        Self::new(self.re, -self.im)
    }

    /// `self` times `-i`.
    const fn turned_back(self) -> Self {
        Self::new(self.im, -self.re)
    }
}

impl Add for Complex {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.re + other.re, self.im + other.im)
    }
}

impl Sub for Complex {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.re - other.re, self.im - other.im)
    }
}

impl Mul for Complex {
    type Output = Self;
    fn mul(self, other: Self) -> Self {
        Self::new(
            self.re * other.re - self.im * other.im,
            self.re * other.im + self.im * other.re,
        )
    }
}

/// `exp(-iθ)`.
fn turn(theta: f64) -> Complex {
    Complex::new(single(cos(theta)), single(-sin(theta)))
}

/// `count` turns of `exp(-iπ(k + offset)/span)`, from `k = 0`.
fn turns(count: usize, offset: f64, span: f64) -> Option<Vec<Complex>> {
    let mut out = filled(count, Complex::default())?;
    for (slot, k) in out.iter_mut().zip(0u32..) {
        *slot = turn(core::f64::consts::PI * (f64::from(k) + offset) / span);
    }
    Some(out)
}

/// Stages a transform's length factors into, at most: 8192 is `4^6 · 2`.
const MAX_STAGES: usize = 16;

/// A forward complex FFT of one length, its twiddles and stages planned once.
pub(crate) struct Fft {
    /// Each stage's radix and the length below it, outermost first.
    stages: tairix_inline::ArrayVec<(usize, usize), MAX_STAGES>,
    /// `exp(-2πik/n)` for each `k` below the length.
    twiddles: Vec<Complex>,
}

impl Fft {
    /// The FFT of `len` points, or `None` for a length with a prime factor
    /// above 5, an empty one, or one whose twiddles cannot be allocated.
    pub(crate) fn new(len: usize) -> Option<Self> {
        let mut stages = tairix_inline::ArrayVec::new();
        let mut rest = len;
        while rest > 1 {
            let radix = [4, 2, 3, 5]
                .into_iter()
                .find(|radix| rest.is_multiple_of(*radix))?;
            rest /= radix;
            stages.try_push((radix, rest)).ok()?;
        }
        if len == 0 {
            return None;
        }
        let span = f64::from(u32::try_from(len).ok()?) / 2.0;
        Some(Self {
            stages,
            twiddles: turns(len, 0.0, span)?,
        })
    }

    /// Its length.
    pub(crate) fn len(&self) -> usize {
        self.twiddles.len()
    }

    /// Transform `input` into `out`, each [`Self::len`] long, in natural
    /// order: `out[j] = Σ input[k] exp(-2πijk/n)`.
    pub(crate) fn process(&self, input: &[Complex], out: &mut [Complex]) {
        let len = self.len();
        if input.len() < len || out.len() < len {
            return;
        }
        if len == 1 {
            out[0] = input[0];
            return;
        }
        self.work(&mut out[..len], input, 1, &self.stages);
    }

    fn work(
        &self,
        out: &mut [Complex],
        input: &[Complex],
        stride: usize,
        stages: &[(usize, usize)],
    ) {
        let Some((&(radix, below), inner)) = stages.split_first() else {
            return;
        };
        if below == 1 {
            for (slot, value) in out.iter_mut().zip(input.iter().step_by(stride)) {
                *slot = *value;
            }
        } else {
            for (at, part) in out.chunks_exact_mut(below).enumerate() {
                self.work(part, &input[at * stride..], stride * radix, inner);
            }
        }
        match radix {
            2 => self.butterfly2(out, stride, below),
            3 => self.butterfly3(out, stride, below),
            4 => self.butterfly4(out, stride, below),
            _ => self.butterfly5(out, stride, below),
        }
    }

    fn butterfly2(&self, out: &mut [Complex], stride: usize, m: usize) {
        let (low, high) = out.split_at_mut(m);
        for (k, (a, b)) in low.iter_mut().zip(high.iter_mut()).enumerate() {
            let t = *b * self.twiddles[k * stride];
            *b = *a - t;
            *a = *a + t;
        }
    }

    fn butterfly3(&self, out: &mut [Complex], stride: usize, m: usize) {
        let third = self.twiddles[stride * m].im;
        for k in 0..m {
            let s1 = out[k + m] * self.twiddles[k * stride];
            let s2 = out[k + 2 * m] * self.twiddles[2 * k * stride];
            let sum = s1 + s2;
            let difference = (s1 - s2).scale(third);
            let mid = out[k] - sum.scale(0.5);
            out[k] = out[k] + sum;
            out[k + 2 * m] = Complex::new(mid.re + difference.im, mid.im - difference.re);
            out[k + m] = Complex::new(mid.re - difference.im, mid.im + difference.re);
        }
    }

    fn butterfly4(&self, out: &mut [Complex], stride: usize, m: usize) {
        for k in 0..m {
            let s0 = out[k + m] * self.twiddles[k * stride];
            let s1 = out[k + 2 * m] * self.twiddles[2 * k * stride];
            let s2 = out[k + 3 * m] * self.twiddles[3 * k * stride];
            let s5 = out[k] - s1;
            let first = out[k] + s1;
            let s3 = s0 + s2;
            let s4 = s0 - s2;
            out[k + 2 * m] = first - s3;
            out[k] = first + s3;
            out[k + m] = s5 + s4.turned_back();
            out[k + 3 * m] = s5 - s4.turned_back();
        }
    }

    fn butterfly5(&self, out: &mut [Complex], stride: usize, m: usize) {
        let ya = self.twiddles[stride * m];
        let yb = self.twiddles[2 * stride * m];
        for k in 0..m {
            let s0 = out[k];
            let s1 = out[k + m] * self.twiddles[k * stride];
            let s2 = out[k + 2 * m] * self.twiddles[2 * k * stride];
            let s3 = out[k + 3 * m] * self.twiddles[3 * k * stride];
            let s4 = out[k + 4 * m] * self.twiddles[4 * k * stride];
            let s7 = s1 + s4;
            let s10 = s1 - s4;
            let s8 = s2 + s3;
            let s9 = s2 - s3;
            out[k] = s0 + s7 + s8;
            let s5 = s0 + s7.scale(ya.re) + s8.scale(yb.re);
            let s6 = Complex::new(
                s10.im * ya.im + s9.im * yb.im,
                -s10.re * ya.im - s9.re * yb.im,
            );
            out[k + m] = s5 - s6;
            out[k + 4 * m] = s5 + s6;
            let s11 = s0 + s7.scale(yb.re) + s8.scale(ya.re);
            let s12 = Complex::new(
                -s10.im * yb.im + s9.im * ya.im,
                s10.re * yb.im - s9.re * ya.im,
            );
            out[k + 2 * m] = s11 + s12;
            out[k + 3 * m] = s11 - s12;
        }
    }
}

/// A DCT-IV of one even length `M`, over an FFT of `M / 2` points:
/// `out[m] = Σ input[k] cos(π/M (m + ½)(k + ½))`.
pub(crate) struct Dct4 {
    fft: Fft,
    /// `exp(-iπ(k + ¼)/M)`.
    pre: Vec<Complex>,
    /// `exp(-iπk/M)`.
    post: Vec<Complex>,
    folded: Vec<Complex>,
    spectrum: Vec<Complex>,
}

impl Dct4 {
    /// The DCT-IV of `len` points, or `None` for an odd length, one whose
    /// half has a prime factor above 5, or one that cannot be allocated.
    pub(crate) fn new(len: usize) -> Option<Self> {
        if len == 0 || !len.is_multiple_of(2) {
            return None;
        }
        let half = len / 2;
        let span = f64::from(u32::try_from(len).ok()?);
        Some(Self {
            fft: Fft::new(half)?,
            pre: turns(half, 0.25, span)?,
            post: turns(half, 0.0, span)?,
            folded: filled(half, Complex::default())?,
            spectrum: filled(half, Complex::default())?,
        })
    }

    /// Its length.
    pub(crate) fn len(&self) -> usize {
        2 * self.fft.len()
    }

    /// Transform `input` into `out`, each [`Self::len`] long.
    pub(crate) fn process(&mut self, input: &[f32], out: &mut [f32]) {
        let len = self.len();
        if input.len() < len || out.len() < len {
            return;
        }
        for (k, (slot, pre)) in self.folded.iter_mut().zip(&self.pre).enumerate() {
            *slot = Complex::new(input[2 * k], input[len - 1 - 2 * k]) * *pre;
        }
        self.fft.process(&self.folded, &mut self.spectrum);
        for (j, (bin, post)) in self.spectrum.iter().zip(&self.post).enumerate() {
            let turned = *bin * *post;
            out[2 * j] = turned.re;
            out[len - 1 - 2 * j] = -turned.im;
        }
    }
}

/// The MDCT pair of one block length `2M`, `M` coefficients a block, over a
/// DCT-IV of `M` points: `X[k] = Σ x[n] cos(π/M (n + ½ + M/2)(k + ½))`, and
/// the inverse summing the same kernel without scaling.
pub(crate) struct Mdct {
    dct4: Dct4,
    work: Vec<f32>,
}

impl Mdct {
    /// The MDCT of `block` samples, `block / 2` coefficients, or `None` for a
    /// block not a multiple of four whose quarter has a prime factor above 5.
    pub(crate) fn new(block: usize) -> Option<Self> {
        if !block.is_multiple_of(4) {
            return None;
        }
        let coefficients = block / 2;
        Some(Self {
            dct4: Dct4::new(coefficients)?,
            work: filled(coefficients, 0.0)?,
        })
    }

    /// Coefficients a block, half its samples.
    pub(crate) fn coefficients(&self) -> usize {
        self.dct4.len()
    }

    /// The `2M` samples `coefficients`' `M` make, unwindowed, into `out`.
    pub(crate) fn inverse(&mut self, coefficients: &[f32], out: &mut [f32]) {
        let m = self.coefficients();
        if coefficients.len() < m || out.len() < 2 * m {
            return;
        }
        self.dct4.process(coefficients, &mut self.work);
        let quarter = m / 2;
        let u = &self.work;
        for (n, sample) in out[..2 * m].iter_mut().enumerate() {
            *sample = if n < quarter {
                u[n + quarter]
            } else if n < m + quarter {
                -u[m + quarter - 1 - n]
            } else {
                -u[n - m - quarter]
            };
        }
    }

    /// The `M` coefficients `samples`' `2M` make, already windowed, into
    /// `out`: the direction the aliasing-cancellation test checks the inverse
    /// against, which no decoder runs.
    #[cfg(test)]
    pub(crate) fn forward(&mut self, samples: &[f32], out: &mut [f32]) {
        let coefficients = self.coefficients();
        if samples.len() < 2 * coefficients || out.len() < coefficients {
            return;
        }
        let quarter = coefficients / 2;
        let (first, rest) = samples[..2 * coefficients].split_at(quarter);
        let (second, rest) = rest.split_at(quarter);
        let (third, fourth) = rest.split_at(quarter);
        let (folded_back, folded_front) = self.work.split_at_mut(quarter);
        for (i, slot) in folded_back.iter_mut().enumerate() {
            *slot = -third[quarter - 1 - i] - fourth[i];
        }
        for (i, slot) in folded_front.iter_mut().enumerate() {
            *slot = first[i] - second[quarter - 1 - i];
        }
        self.dct4.process(&self.work, out);
    }
}

/// A DCT-II of one even length `N`, over an FFT of `N / 2` points of its real
/// input: `out[m] = Σ input[k] cos(πm(2k + 1)/(2N))`.
pub(crate) struct Dct2 {
    fft: Fft,
    /// `exp(-2πim/N)`, for `m` up to `N / 2`.
    split: Vec<Complex>,
    /// `exp(-iπm/(2N))`, for each `m`.
    shift: Vec<Complex>,
    packed: Vec<Complex>,
    spectrum: Vec<Complex>,
}

impl Dct2 {
    /// The DCT-II of `len` points, or `None` for an odd length, one whose
    /// half has a prime factor above 5, or one that cannot be allocated.
    pub(crate) fn new(len: usize) -> Option<Self> {
        if len == 0 || !len.is_multiple_of(2) {
            return None;
        }
        let half = len / 2;
        let span = f64::from(u32::try_from(len).ok()?);
        Some(Self {
            fft: Fft::new(half)?,
            split: turns(half + 1, 0.0, span / 2.0)?,
            shift: turns(len, 0.0, 2.0 * span)?,
            packed: filled(half, Complex::default())?,
            spectrum: filled(half, Complex::default())?,
        })
    }

    /// Its length.
    pub(crate) fn len(&self) -> usize {
        2 * self.fft.len()
    }

    /// Transform `input` into `out`, each [`Self::len`] long.
    pub(crate) fn process(&mut self, input: &[f32], out: &mut [f32]) {
        let len = self.len();
        let half = len / 2;
        if input.len() < len || out.len() < len {
            return;
        }
        // The even samples ascending, then the odd ones descending, packed in
        // pairs as one complex sequence of half the length.
        let reordered = |at: usize| {
            if at < half {
                input[2 * at]
            } else {
                input[2 * (len - 1 - at) + 1]
            }
        };
        for (k, slot) in self.packed.iter_mut().enumerate() {
            *slot = Complex::new(reordered(2 * k), reordered(2 * k + 1));
        }
        self.fft.process(&self.packed, &mut self.spectrum);
        let full = |m: usize| {
            let z = self.spectrum[m % half];
            let mirror = self.spectrum[(half - m % half) % half].conj();
            let even = (z + mirror).scale(0.5);
            let odd = (z - mirror).scale(0.5).turned_back();
            even + self.split[m] * odd
        };
        for (m, sample) in out[..len].iter_mut().enumerate() {
            let bin = if m <= half {
                full(m)
            } else {
                full(len - m).conj()
            };
            *sample = (self.shift[m] * bin).re;
        }
    }
}

#[cfg(test)]
#[path = "transform_tests.rs"]
mod tests;
