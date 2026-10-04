//! The discrete Fourier transform, radix two: a sequence a power of two long
//! transformed in place, and a grid's rows transposed so its columns can be
//! transformed as rows.
//!
//! Every operation is done in one fixed order with the twiddles worked out
//! once, so a transform comes out bit for bit the same on every target and
//! whichever core does it.

use alloc::vec::Vec;
use core::f64::consts::TAU;
use core::ops::{Add, Mul, Sub};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::band;
use crate::vector::real;

/// A complex number.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Complex {
    pub(crate) re: f64,
    pub(crate) im: f64,
}

impl Complex {
    pub(crate) const ZERO: Self = Self { re: 0.0, im: 0.0 };

    pub(crate) const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub(crate) const fn conj(self) -> Self {
        Self::new(self.re, -self.im)
    }

    pub(crate) fn scale(self, by: f64) -> Self {
        Self::new(self.re * by, self.im * by)
    }

    /// The quotient by `divisor`; nought for a divisor of nought.
    pub(crate) fn over(self, divisor: Self) -> Self {
        let norm = divisor.re * divisor.re + divisor.im * divisor.im;
        if norm == 0.0 {
            return Self::ZERO;
        }
        (self * divisor.conj()).scale(1.0 / norm)
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

/// The transform of sequences of one length, a power of two: its twiddles
/// `e^(-2πik/n)` for every `k` below half the length.
#[derive(Debug)]
pub(crate) struct Fourier {
    length: usize,
    twiddles: Vec<Complex>,
}

impl Fourier {
    /// The transform of sequences `length` long; `None` for a length that is
    /// not a power of two, or when the heap will not hold its twiddles.
    pub(crate) fn new(length: usize) -> Option<Self> {
        if !length.is_power_of_two() {
            return None;
        }
        let half = length / 2;
        let mut twiddles = Vec::new();
        if !fallible::reserve(&mut twiddles, half) {
            return None;
        }
        twiddles.extend((0..half).map(|k| {
            let angle = -TAU * real(k) / real(length);
            Complex::new(mathf::cos(angle), mathf::sin(angle))
        }));
        Some(Self { length, twiddles })
    }

    pub(crate) const fn len(&self) -> usize {
        self.length
    }

    /// `values` transformed in place, `X_k = Σ x_j e^(-2πijk/n)`, unscaled;
    /// `None`, leaving them as they were, when they are not its length.
    pub(crate) fn forward(&self, values: &mut [Complex]) -> Option<()> {
        self.transform(values, false)
    }

    /// `values` transformed back in place, `x_j = (1/n) Σ X_k e^(2πijk/n)`;
    /// `None`, leaving them as they were, when they are not its length.
    pub(crate) fn inverse(&self, values: &mut [Complex]) -> Option<()> {
        self.transform(values, true)?;
        let scale = 1.0 / real(self.length);
        for value in values.iter_mut() {
            *value = value.scale(scale);
        }
        Some(())
    }

    /// Cooley and Tukey's iterative transform: the values set in
    /// bit-reversed order, then combined in butterflies of doubling span.
    fn transform(&self, values: &mut [Complex], backward: bool) -> Option<()> {
        let length = self.length;
        if values.len() != length {
            return None;
        }
        if length < 2 {
            return Some(());
        }
        let bits = length.trailing_zeros();
        for index in 0..length {
            let reversed = index.reverse_bits() >> (usize::BITS - bits);
            if reversed > index {
                values.swap(index, reversed);
            }
        }
        let mut span = 1;
        while span < length {
            let stride = length / (2 * span);
            for start in (0..length).step_by(2 * span) {
                for offset in 0..span {
                    let twiddle = *self.twiddles.get(offset * stride)?;
                    let twiddle = if backward { twiddle.conj() } else { twiddle };
                    let (low, high) = (start + offset, start + offset + span);
                    let carried = *values.get(high)? * twiddle;
                    let kept = *values.get(low)?;
                    values[low] = kept + carried;
                    values[high] = kept - carried;
                }
            }
            span *= 2;
        }
        Some(())
    }
}

/// The signed frequency of bin `index` of a transform `length` long, in
/// cycles over the length: `0, 1, …, n/2 - 1, -n/2, …, -1`.
pub(crate) fn frequency(index: usize, length: usize) -> f64 {
    if 2 * index < length {
        real(index)
    } else {
        real(index) - real(length)
    }
}

/// Transform each of `rows` rows of `values`, `fourier`'s length apiece, in
/// bands of `per` rows across `runner`; backward when `inverse`. `None`
/// when the rows are not its length.
pub(crate) fn rows(
    fourier: &Fourier,
    values: &mut [Complex],
    (per, inverse): (usize, bool),
    runner: &dyn JobRunner,
) -> Option<()> {
    let length = fourier.len();
    if length == 0 || !values.len().is_multiple_of(length) {
        return None;
    }
    let failed = band::fold(
        runner,
        values,
        (0, per.max(1) * length),
        false,
        &|_, band| {
            band.chunks_mut(length).any(|row| {
                if inverse {
                    fourier.inverse(row).is_none()
                } else {
                    fourier.forward(row).is_none()
                }
            })
        },
        |one, other| one || other,
    );
    (!failed).then_some(())
}

/// Rows of the transpose of `from`, a grid of `rows` rows by `columns`
/// columns, written into `to` from its row `first`: each row of the
/// transpose is one of `from`'s columns, and `to` holds as many of them as
/// it has room for, in bands of `per` across `runner`. `None` when the grids
/// are not the size they are said to be.
pub(crate) fn transposed(
    from: &[Complex],
    (rows, columns): (usize, usize),
    to: &mut [Complex],
    (first, per): (usize, usize),
    runner: &dyn JobRunner,
) -> Option<()> {
    let wanted = to.len().checked_div(rows)?;
    if from.len() != rows.checked_mul(columns)?
        || !to.len().is_multiple_of(rows)
        || first.checked_add(wanted)? > columns
    {
        return None;
    }
    let per = per.max(1);
    band::for_each(runner, to, (0, per * rows), &|band, values| {
        for (offset, line) in values.chunks_mut(rows).enumerate() {
            let column = first + band * per + offset;
            for (row, value) in line.iter_mut().enumerate() {
                *value = from
                    .get(row * columns + column)
                    .copied()
                    .unwrap_or_default();
            }
        }
    });
    Some(())
}

#[cfg(test)]
#[path = "fourier_tests.rs"]
mod tests;
