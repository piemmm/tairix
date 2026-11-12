//! The 8087's transcendental instructions: FPTAN, FPATAN, F2XM1, FYL2X and
//! FYL2XP1, each computed in 128-bit significands and rounded once, so a
//! result is correctly rounded unless the true value lies within 2^-120 of
//! a rounding boundary.
//!
//! Inside the operand ranges the data sheet gives they answer as the 8087
//! documents; outside them, where its answer is undefined, as the 80387
//! does.

use core::cmp::Ordering;

use super::arith::{Exact, Parts};

/// A working number of 128 significant bits: `sig × 2^(exp − 127)`, `sig`'s
/// top bit set unless the number is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Big {
    negative: bool,
    exp: i32,
    sig: u128,
}

/// π, from Machin's formula.
const PI: Big = Big::constant(1, 0xC90F_DAA2_2168_C234_C4C6_628B_80DC_1CD1);
/// ln 2, from 2 atanh(1/3).
const LN2: Big = Big::constant(-1, 0xB172_17F7_D1CF_79AB_C9E3_B398_03F2_F6AF);

/// The series stop once a term falls this many places below the sum.
const NEGLIGIBLE: i32 = 130;
/// Terms a series takes at most: each converges within 60 in its domain,
/// so the bound only keeps a call total.
const MOST_TERMS: u32 = 120;
/// Past this an exponent is beyond every format's reach, even wrapped.
const EXPONENT_REACH: i32 = 1 << 20;

impl Big {
    const ZERO: Self = Self {
        negative: false,
        exp: 0,
        sig: 0,
    };
    const ONE: Self = Self::constant(0, 1 << 127);

    const fn constant(exp: i32, sig: u128) -> Self {
        Self {
            negative: false,
            exp,
            sig,
        }
    }

    /// The value of finite `p`.
    pub(crate) fn of(p: Parts) -> Self {
        Self {
            negative: p.negative,
            exp: p.exp,
            sig: u128::from(p.sig) << 64,
        }
        .normalized()
    }

    fn small(n: u32) -> Self {
        Self {
            negative: false,
            exp: 127,
            sig: u128::from(n),
        }
        .normalized()
    }

    const fn is_zero(self) -> bool {
        self.sig == 0
    }

    fn normalized(self) -> Self {
        if self.sig == 0 {
            return Self::ZERO;
        }
        let zeros = self.sig.leading_zeros();
        Self {
            exp: self.exp - i32::try_from(zeros).unwrap_or(0),
            sig: self.sig << zeros,
            ..self
        }
    }

    const fn negated(self) -> Self {
        Self {
            negative: !self.negative,
            ..self
        }
    }

    const fn abs(self) -> Self {
        Self {
            negative: false,
            ..self
        }
    }

    /// This number times 2^n.
    fn scaled(self, n: i32) -> Self {
        if self.is_zero() {
            return self;
        }
        Self {
            exp: self.exp.saturating_add(n),
            ..self
        }
    }

    fn cmp_magnitude(self, other: Self) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => self.exp.cmp(&other.exp).then(self.sig.cmp(&other.sig)),
        }
    }

    fn add(self, other: Self) -> Self {
        if self.is_zero() {
            return other;
        }
        if other.is_zero() {
            return self;
        }
        let (big, small) = if self.cmp_magnitude(other) == Ordering::Less {
            (other, self)
        } else {
            (self, other)
        };
        let gap = u32::try_from(big.exp.saturating_sub(small.exp)).unwrap_or(u32::MAX);
        let aligned = if gap >= 128 { 0 } else { small.sig >> gap };
        if big.negative == small.negative {
            let (sum, carry) = big.sig.overflowing_add(aligned);
            if carry {
                return Self {
                    exp: big.exp.saturating_add(1),
                    sig: (sum >> 1) | (1 << 127),
                    ..big
                };
            }
            return Self { sig: sum, ..big };
        }
        Self {
            sig: big.sig - aligned,
            ..big
        }
        .normalized()
    }

    fn sub(self, other: Self) -> Self {
        self.add(other.negated())
    }

    fn mul(self, other: Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::ZERO;
        }
        let (high, low) = wide_mul(self.sig, other.sig);
        let negative = self.negative != other.negative;
        let exp = self.exp.saturating_add(other.exp);
        if high >> 127 != 0 {
            return Self {
                negative,
                exp: exp.saturating_add(1),
                sig: high,
            };
        }
        Self {
            negative,
            exp,
            sig: (high << 1) | (low >> 127),
        }
    }

    fn div(self, other: Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::ZERO;
        }
        let divisor = other.sig;
        let mut rest = self.sig;
        let mut quotient: u128 = 0;
        // A quotient of at least one takes its integer bit at once.
        let exp = self.exp.saturating_sub(other.exp);
        let (mut exp, top) = if rest >= divisor {
            rest -= divisor;
            quotient = 1 << 127;
            (exp, 126)
        } else {
            (exp.saturating_sub(1), 127)
        };
        for bit in (0..=top).rev() {
            let carry = rest >> 127 != 0;
            rest <<= 1;
            if carry || rest >= divisor {
                rest = rest.wrapping_sub(divisor);
                quotient |= 1 << bit;
            }
        }
        if quotient >> 127 == 0 {
            exp -= 1;
            quotient <<= 1;
        }
        Self {
            negative: self.negative != other.negative,
            exp,
            sig: quotient,
        }
    }

    fn div_small(self, n: u32) -> Self {
        self.div(Self::small(n))
    }

    /// The nearest integer, ties away from zero, saturating at 2^62.
    fn nearest_integer(self) -> i64 {
        if self.is_zero() || self.exp < -1 {
            return 0;
        }
        let magnitude = if self.exp >= 62 {
            1 << 62
        } else if self.exp == -1 {
            // In [½, 1): one, a half rounding away from zero.
            1
        } else {
            let places = u32::try_from(127 - self.exp).unwrap_or(127);
            let whole = (self.sig >> places) + ((self.sig >> (places - 1)) & 1);
            i64::try_from(whole).unwrap_or(1 << 62)
        };
        if self.negative {
            -magnitude
        } else {
            magnitude
        }
    }

    fn from_i64(n: i64) -> Self {
        Self {
            negative: n < 0,
            exp: 127,
            sig: u128::from(n.unsigned_abs()),
        }
        .normalized()
    }

    /// The number as an inexact result for rounding, its exponent held
    /// where it still overflows or underflows every format.
    pub(crate) fn exact(self) -> Exact {
        Exact {
            negative: self.negative,
            exp: self.exp.clamp(-EXPONENT_REACH, EXPONENT_REACH),
            sig: self.sig >> 1,
            sticky: true,
        }
    }

    /// Whether `term` no longer changes `sum`.
    fn negligible(term: Self, sum: Self) -> bool {
        term.is_zero() || (!sum.is_zero() && sum.exp.saturating_sub(term.exp) > NEGLIGIBLE)
    }
}

/// The 256-bit product of `a` and `b`: its high and low halves.
fn wide_mul(a: u128, b: u128) -> (u128, u128) {
    let mask = u128::from(u64::MAX);
    let (a1, a0) = (a >> 64, a & mask);
    let (b1, b0) = (b >> 64, b & mask);
    let low = a0 * b0;
    let mid1 = a1 * b0;
    let mid2 = a0 * b1;
    let high = a1 * b1;
    let (mid, carry) = mid1.overflowing_add(mid2);
    let (low, carry_low) = low.overflowing_add(mid << 64);
    let high = high + (mid >> 64) + (u128::from(carry) << 64) + u128::from(carry_low);
    (high, low)
}

/// sin and cos of `r`, |r| ≤ π/4, by their Taylor series.
fn sin_cos(r: Big) -> (Big, Big) {
    let square = r.mul(r);
    // Each term is the last times −r² over the next two factorial factors.
    let series = |first: Big, factor: u32| {
        let mut sum = first;
        let mut term = first;
        for n in (0..MOST_TERMS).map(|k| factor + 2 * k) {
            term = term.mul(square).div_small(n).div_small(n + 1).negated();
            sum = sum.add(term);
            if Big::negligible(term, sum) {
                break;
            }
        }
        sum
    };
    (series(r, 2), series(Big::ONE, 1))
}

/// π × `quarters` / 4.
#[must_use]
pub(crate) fn quarter_pis(quarters: u32) -> Exact {
    PI.mul(Big::small(quarters)).scaled(-2).exact()
}

/// tan x for finite nonzero `x`, |x| < 2^63: reduced by the nearest
/// multiple of π/2, as the 80387 does past the 8087's π/4.
#[must_use]
pub(crate) fn tan(x: Parts) -> Exact {
    let x = Big::of(x);
    let half_pi = PI.scaled(-1);
    let quarter_pi = PI.scaled(-2);
    let (r, odd) = if x.cmp_magnitude(quarter_pi) == Ordering::Greater {
        let k = x.div(half_pi).nearest_integer();
        (x.sub(half_pi.mul(Big::from_i64(k))), k & 1 != 0)
    } else {
        (x, false)
    };
    let (sin, cos) = sin_cos(r);
    let tan = if odd {
        cos.div(sin).negated()
    } else {
        sin.div(cos)
    };
    tan.exact()
}

/// atan of `t` ≥ 0.
fn atan_positive(t: Big) -> Big {
    if t.cmp_magnitude(Big::ONE) == Ordering::Greater {
        return PI.scaled(-1).sub(atan_positive(Big::ONE.div(t)));
    }
    // tan(π/8) = √2 − 1 ≈ 0.4142: above it, atan t = π/4 + atan((t−1)/(t+1)).
    let tan_eighth = Big::constant(-2, 0xD413_CCCF_E779_9211_65F6_26CD_D52A_FA7C);
    let (base, u) = if t.cmp_magnitude(tan_eighth) == Ordering::Greater {
        (PI.scaled(-2), t.sub(Big::ONE).div(t.add(Big::ONE)))
    } else {
        (Big::ZERO, t)
    };
    let square = u.mul(u);
    let mut sum = u;
    let mut power = u;
    for k in 1..MOST_TERMS {
        power = power.mul(square).negated();
        let term = power.div_small(2 * k + 1);
        sum = sum.add(term);
        if Big::negligible(term, sum) {
            break;
        }
    }
    base.add(sum)
}

/// The angle of the point (`x`, `y`), both finite and not both zero, as
/// FPATAN gives it: atan(y/x) for 0 ≤ y < x, and the 80387's whole circle
/// elsewhere.
#[must_use]
pub(crate) fn atan2(y: Parts, x: Parts) -> Exact {
    let (y, x) = (Big::of(y), Big::of(x));
    let angle = if x.is_zero() {
        PI.scaled(-1)
    } else {
        atan_positive(y.abs().div(x.abs()))
    };
    let angle = if x.negative { PI.sub(angle) } else { angle };
    let angle = if y.negative { angle.negated() } else { angle };
    angle.exact()
}

/// e^y − 1 for |y| ≤ 1 by its Taylor series.
fn expm1(y: Big) -> Big {
    let mut sum = y;
    let mut term = y;
    for n in 2..MOST_TERMS {
        term = term.mul(y).div_small(n);
        sum = sum.add(term);
        if Big::negligible(term, sum) {
            break;
        }
    }
    sum
}

/// 2^x − 1 for finite `x`: within F2XM1's 0 ≤ x ≤ ½ directly, and as the
/// 80387 answers past it.
#[must_use]
pub(crate) fn exp2m1(x: Parts) -> Exact {
    let x = Big::of(x);
    if x.cmp_magnitude(Big::ONE) != Ordering::Greater {
        return expm1(x.mul(LN2)).exact();
    }
    // 2^x = 2^n × 2^f, n the nearest integer and |f| ≤ ½; the exponent's
    // own range bounds n far inside an i32.
    let n = x.nearest_integer();
    let f = x.sub(Big::from_i64(n));
    let power = expm1(f.mul(LN2)).add(Big::ONE);
    let n = i32::try_from(n.clamp(-1 << 20, 1 << 20)).unwrap_or(0);
    power.scaled(n).sub(Big::ONE).exact()
}

/// 2 atanh z for |z| < 0.18 by its series: ln((1 + z)/(1 − z)).
fn two_atanh(z: Big) -> Big {
    let square = z.mul(z);
    let mut sum = z;
    let mut power = z;
    for k in 1..MOST_TERMS {
        power = power.mul(square);
        let term = power.div_small(2 * k + 1);
        sum = sum.add(term);
        if Big::negligible(term, sum) {
            break;
        }
    }
    sum.scaled(1)
}

/// ln(1 + x) for x within [1/√2 − 1, √2 − 1), as 2 atanh(x / (x + 2)):
/// exact relative to x however small it is.
fn ln_1p(x: Big) -> Big {
    two_atanh(x.div(x.add(Big::small(2))))
}

/// log2 of positive `x`.
fn log2(x: Big) -> Big {
    // x = m × 2^e with m in [1/√2, √2).
    let root_half = Big::constant(-1, 0xB504_F333_F9DE_6484_597D_89B3_754A_BE9F);
    let mut e = x.exp;
    let mut m = Big { exp: 0, ..x };
    if m.cmp_magnitude(root_half.scaled(1)) != Ordering::Less {
        m = m.scaled(-1);
        e += 1;
    }
    let fraction = ln_1p(m.sub(Big::ONE)).div(LN2);
    Big::from_i64(i64::from(e)).add(fraction)
}

/// y × log2(x), `x` positive.
#[must_use]
pub(crate) fn y_log2_x(y: Parts, x: Parts) -> Exact {
    Big::of(y).mul(log2(Big::of(x))).exact()
}

/// y × log2(1 + x), x > −1: by atanh within FYL2XP1's |x| < 1 − √2/2, as
/// the 80387 answers past it.
#[must_use]
pub(crate) fn y_log2_1p_x(y: Parts, x: Parts) -> Exact {
    let x = Big::of(x);
    let bound = Big::constant(-2, 0x95F6_1998_0C43_36F7_4D04_EC99_156A_82C1);
    let log = if x.cmp_magnitude(bound) == Ordering::Less {
        ln_1p(x).div(LN2)
    } else {
        log2(x.add(Big::ONE))
    };
    Big::of(y).mul(log).exact()
}

#[cfg(test)]
#[path = "trans_tests.rs"]
mod tests;
