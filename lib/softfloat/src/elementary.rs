//! The elementary functions, each computed in 128-bit significands and
//! answered as an [`Exact`] its caller rounds once, so a result is correctly
//! rounded unless the true value lies within 2^-120 of a rounding boundary.
//!
//! The constants come from exact integer series: π from Machin's formula,
//! ln 2 and ln 10 from atanh, and the 1280 bits of 2/π that reduce any
//! operand below 2^1024 by its nearest multiple of π/2 with no loss however
//! close it lies to one.

use core::cmp::Ordering;

use crate::exact::{low_word, Exact, Parts};

/// A working number of 128 significant bits: `sig × 2^(exp − 127)`, `sig`'s
/// top bit set unless the number is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wide {
    negative: bool,
    exp: i32,
    sig: u128,
}

/// π, from Machin's formula.
const PI: Wide = Wide::constant(1, 0xC90F_DAA2_2168_C234_C4C6_628B_80DC_1CD1);
/// ln 2, from 2 atanh(1/3).
const LN2: Wide = Wide::constant(-1, 0xB172_17F7_D1CF_79AB_C9E3_B398_03F2_F6AF);
/// ln 10, from 3 ln 2 + 2 atanh(1/9).
const LN10: Wide = Wide::constant(1, 0x935D_8DDD_AAA8_AC16_EA56_D62B_82D3_0A28);
/// The first 1280 bits of 2/π below the point, most significant word first:
/// a reduction's window of them lies within these for every operand below
/// 2^1024, binary64's whole range.
const TWO_OVER_PI: [u64; 20] = [
    0xA2F9_836E_4E44_1529,
    0xFC27_57D1_F534_DDC0,
    0xDB62_9599_3C43_9041,
    0xFE51_63AB_DEBB_C561,
    0xB724_6E3A_424D_D2E0,
    0x0649_2EEA_09D1_921C,
    0xFE1D_EB1C_B129_A73E,
    0xE882_35F5_2EBB_4484,
    0xE99C_7026_B45F_7E41,
    0x3991_D639_8353_39F4,
    0x9C84_5F8B_BDF9_283B,
    0x1FF8_97FF_DE05_980F,
    0xEF2F_118B_5A0A_6D1F,
    0x6D36_7ECF_27CB_09B7,
    0x4F46_3F66_9E5F_EA2D,
    0x7527_BAC7_EBE5_F17B,
    0x3D07_39F7_8A52_92EA,
    0x6BFB_5FB1_1F8D_5D08,
    0x5603_3046_FC7B_6BAB,
    0xF0CF_BC20_9AF4_361D,
];
/// The words of 2/π one reduction multiplies by: enough that the bits past
/// them move the remainder less than 2^-250 below its point.
const WINDOW: usize = 6;
/// The words of the operand's significand times the window.
const PRODUCT: usize = WINDOW + 1;

/// The series stop once a term falls this many places below the sum.
const NEGLIGIBLE: i32 = 130;
/// Terms a series takes at most: each converges within 60 in its domain,
/// so the bound only keeps a call total.
const MOST_TERMS: u32 = 120;
/// Past this an exponent is beyond every format's reach, even wrapped.
const EXPONENT_REACH: i32 = 1 << 20;
/// Below this exponent a function's next term falls under the working bits'
/// noise: its leading term stands, and which side of it the value lies.
const TINY: i32 = -58;

impl Wide {
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
    fn of(p: Parts) -> Self {
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

    /// A magnitude just under this nonzero one's: a unit less, which the
    /// sticky bit [`Wide::exact`] sets then puts strictly between.
    fn just_under(self) -> Self {
        Self {
            sig: self.sig - 1,
            ..self
        }
        .normalized()
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

    /// The square root of this number, which is not negative.
    fn sqrt(self) -> Self {
        if self.is_zero() {
            return Self::ZERO;
        }
        // The root of the top 64 bits is good to some 63, and each Newton
        // step doubles what is good.
        let top = u128::from(u64::try_from(self.sig >> 64).unwrap_or(u64::MAX));
        let odd = self.exp & 1 != 0;
        let (radicand, places, half) = if odd {
            (top << 62, 62, (self.exp - 1) / 2)
        } else {
            (top << 63, 63, self.exp / 2)
        };
        let mut root = Self {
            negative: false,
            exp: 127 - places,
            sig: radicand.isqrt(),
        }
        .normalized()
        .scaled(half);
        for _ in 0..2 {
            root = root.add(self.div(root)).scaled(-1);
        }
        root
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

    /// The number as a result for rounding, inexact unless it is zero, its
    /// exponent held where it still overflows or underflows every format.
    fn exact(self) -> Exact {
        Exact {
            negative: self.negative,
            exp: self.exp.clamp(-EXPONENT_REACH, EXPONENT_REACH),
            sig: self.sig >> 1,
            sticky: !self.is_zero(),
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
fn sin_cos(r: Wide) -> (Wide, Wide) {
    let square = r.mul(r);
    // Each term is the last times −r² over the next two factorial factors.
    let series = |first: Wide, factor: u32| {
        let mut sum = first;
        let mut term = first;
        for n in (0..MOST_TERMS).map(|k| factor + 2 * k) {
            term = term.mul(square).div_small(n).div_small(n + 1).negated();
            sum = sum.add(term);
            if Wide::negligible(term, sum) {
                break;
            }
        }
        sum
    };
    (series(r, 2), series(Wide::ONE, 1))
}

/// `|x|`, finite and below 2^1024, less its nearest multiple k of π/2: the
/// remainder r, |r| ≤ π/4, and k modulo 4. Past 2^1024 the answer is defined
/// but meaningless.
///
/// |x| × 2/π is taken from a window of 2/π's bits in exact integer
/// arithmetic, so its fraction keeps 128 bits however small it is, and r is
/// that fraction times π/2. The bits before the window add only multiples of
/// four to the product, which the quadrant ignores.
fn reduced(x: Parts) -> (Wide, u32) {
    let x = Parts {
        negative: false,
        ..x
    }
    .normalized();
    if x.is_zero() || Wide::of(x).cmp_magnitude(PI.scaled(-2)) != Ordering::Greater {
        return (Wide::of(x), 0);
    }
    // Word n of 2/π weighs 2^-64(n+1): its share, sig × it × 2^(exp − 63 −
    // 64(n+1)), is a multiple of four wherever 64(n+1) ≤ exp − 65.
    let skipped = usize::try_from((x.exp - 65).max(0) / 64)
        .unwrap_or(usize::MAX)
        .min(TWO_OVER_PI.len() - WINDOW);
    let exp = x.exp - 64 * i32::try_from(skipped).unwrap_or(0);
    // x × the window = sig × 2^(exp − 63) × F × 2^-384, F the window's six
    // words: the product of sig and F holds it with its point 447 − exp
    // places up.
    let mut product = [0u64; PRODUCT];
    for (n, &word) in TWO_OVER_PI[skipped..skipped + WINDOW]
        .iter()
        .rev()
        .enumerate()
    {
        let wide = u128::from(x.sig) * u128::from(word) + u128::from(product[n]);
        product[n] = low_word(wide);
        product[n + 1] = low_word(wide >> 64);
    }
    let point = u32::try_from(447 - exp.min(382)).unwrap_or(65);
    let quotient = bits_above(&product, point);
    let half = bit(&product, point - 1);
    let (k, fraction) = if half {
        // Past the half: the multiple above, the fraction below it negative.
        (quotient.wrapping_add(1), negated_below(&product, point))
    } else {
        (quotient, below(&product, point))
    };
    // The fraction is its words × 2^-point, so its top 128 bits, the highest
    // just below `lead`, sit at 2^(lead − 1 − point).
    let (top, lead) = top_bits(&fraction);
    let magnitude = Wide {
        negative: half,
        exp: i32::try_from(lead).unwrap_or(0) - 1 - i32::try_from(point).unwrap_or(0),
        sig: top,
    }
    .normalized();
    (
        magnitude.mul(PI.scaled(-1)),
        u32::try_from(k & 3).unwrap_or(0),
    )
}

/// Word `index` of the little-endian `words`, zero past them.
fn word_at(words: &[u64; PRODUCT], index: u32) -> u64 {
    usize::try_from(index)
        .ok()
        .and_then(|index| words.get(index))
        .copied()
        .unwrap_or(0)
}

/// Bit `n` of the little-endian `words`.
fn bit(words: &[u64; PRODUCT], n: u32) -> bool {
    (word_at(words, n / 64) >> (n % 64)) & 1 != 0
}

/// The 64 bits of `words` from bit `n` up.
fn bits_above(words: &[u64; PRODUCT], n: u32) -> u64 {
    let (word, place) = (n / 64, n % 64);
    let low = word_at(words, word) >> place;
    let high = if place == 0 {
        0
    } else {
        word_at(words, word + 1) << (64 - place)
    };
    low | high
}

/// `words` with every bit from `n` up cleared.
fn below(words: &[u64; PRODUCT], n: u32) -> [u64; PRODUCT] {
    let mut out = *words;
    for (start, word) in (0u32..).step_by(64).zip(&mut out) {
        if start >= n {
            *word = 0;
        } else if n - start < 64 {
            *word &= (1 << (n - start)) - 1;
        }
    }
    out
}

/// 2^n less `words` below bit `n`: the distance from the bits below `n` up
/// to the next multiple of 2^n.
fn negated_below(words: &[u64; PRODUCT], n: u32) -> [u64; PRODUCT] {
    let low = below(words, n);
    let mut out = [0u64; PRODUCT];
    let mut borrow = false;
    for (&word, slot) in low.iter().zip(&mut out) {
        let (value, under) = 0u64.overflowing_sub(word);
        let (value, under_again) = value.overflowing_sub(u64::from(borrow));
        *slot = value;
        borrow = under || under_again;
    }
    below(&out, n)
}

/// The top 128 bits of `words`, which are not all zero, and the place just
/// above the top bit.
fn top_bits(words: &[u64; PRODUCT]) -> (u128, u32) {
    let Some((start, word)) = (0u32..)
        .step_by(64)
        .zip(words)
        .filter(|&(_, &word)| word != 0)
        .last()
    else {
        return (0, 0);
    };
    let lead = start + 64 - word.leading_zeros();
    let top = if lead >= 128 {
        u128::from(bits_above(words, lead - 64)) << 64 | u128::from(bits_above(words, lead - 128))
    } else {
        (u128::from(words[1]) << 64 | u128::from(words[0])) << (128 - lead)
    };
    (top, lead)
}

/// π × `quarters` / 4.
#[must_use]
pub fn quarter_pis(quarters: u32) -> Exact {
    PI.mul(Wide::small(quarters)).scaled(-2).exact()
}

/// sin x for finite nonzero `x`, |x| < 2^1024.
#[must_use]
pub fn sin(x: Parts) -> Exact {
    if x.exp < TINY {
        // x − x³/6.
        return Wide::of(x).just_under().exact();
    }
    let (r, k) = reduced(x);
    let (sin, cos) = sin_cos(r);
    let value = match k {
        0 => sin,
        1 => cos,
        2 => sin.negated(),
        _ => cos.negated(),
    };
    if x.negative {
        value.negated().exact()
    } else {
        value.exact()
    }
}

/// cos x for finite `x`, |x| < 2^1024.
#[must_use]
pub fn cos(x: Parts) -> Exact {
    if x.exp < TINY {
        // 1 − x²/2.
        return Wide::ONE.just_under().exact();
    }
    let (r, k) = reduced(x);
    let (sin, cos) = sin_cos(r);
    match k {
        0 => cos,
        1 => sin.negated(),
        2 => cos.negated(),
        _ => sin,
    }
    .exact()
}

/// tan x for finite nonzero `x`, |x| < 2^1024.
#[must_use]
pub fn tan(x: Parts) -> Exact {
    if x.exp < TINY {
        // x + x³/3.
        return Wide::of(x).exact();
    }
    let (r, k) = reduced(x);
    let (sin, cos) = sin_cos(r);
    let tan = if k & 1 != 0 {
        cos.div(sin).negated()
    } else {
        sin.div(cos)
    };
    if x.negative {
        tan.negated().exact()
    } else {
        tan.exact()
    }
}

/// atan of `t` ≥ 0.
fn atan_positive(t: Wide) -> Wide {
    if !t.is_zero() && t.exp < TINY {
        // t − t³/3.
        return t.just_under();
    }
    if t.cmp_magnitude(Wide::ONE) == Ordering::Greater {
        return PI.scaled(-1).sub(atan_positive(Wide::ONE.div(t)));
    }
    // tan(π/8) = √2 − 1 ≈ 0.4142: above it, atan t = π/4 + atan((t−1)/(t+1)).
    let tan_eighth = Wide::constant(-2, 0xD413_CCCF_E779_9211_65F6_26CD_D52A_FA7C);
    let (base, u) = if t.cmp_magnitude(tan_eighth) == Ordering::Greater {
        (PI.scaled(-2), t.sub(Wide::ONE).div(t.add(Wide::ONE)))
    } else {
        (Wide::ZERO, t)
    };
    let square = u.mul(u);
    let mut sum = u;
    let mut power = u;
    for k in 1..MOST_TERMS {
        power = power.mul(square).negated();
        let term = power.div_small(2 * k + 1);
        sum = sum.add(term);
        if Wide::negligible(term, sum) {
            break;
        }
    }
    base.add(sum)
}

/// The angle of the point (`x`, `y`) over the whole circle, both finite and
/// not both zero: atan(y/x) for x > 0, and π less it, signed as `y`, for
/// x < 0.
#[must_use]
pub fn atan2(y: Parts, x: Parts) -> Exact {
    angle(Wide::of(y), Wide::of(x)).exact()
}

fn angle(y: Wide, x: Wide) -> Wide {
    let angle = if x.is_zero() {
        PI.scaled(-1)
    } else {
        atan_positive(y.abs().div(x.abs()))
    };
    let angle = if x.negative { PI.sub(angle) } else { angle };
    if y.negative {
        angle.negated()
    } else {
        angle
    }
}

/// √(1 − x²), as √((1 − x)(1 + x)) so it keeps every bit near |x| = 1.
fn cosine_of(x: Wide) -> Wide {
    Wide::ONE.sub(x).mul(Wide::ONE.add(x)).sqrt()
}

/// asin x for nonzero `x`, |x| ≤ 1.
#[must_use]
pub fn asin(x: Parts) -> Exact {
    if x.exp < TINY {
        // x + x³/6.
        return Wide::of(x).exact();
    }
    let x = Wide::of(x);
    angle(x, cosine_of(x)).exact()
}

/// acos x for `x`, |x| ≤ 1, x ≠ 1.
#[must_use]
pub fn acos(x: Parts) -> Exact {
    let x = Wide::of(x);
    angle(cosine_of(x), x).exact()
}

/// e^y − 1 for |y| ≤ 1 by its Taylor series.
fn expm1(y: Wide) -> Wide {
    let mut sum = y;
    let mut term = y;
    for n in 2..MOST_TERMS {
        term = term.mul(y).div_small(n);
        sum = sum.add(term);
        if Wide::negligible(term, sum) {
            break;
        }
    }
    sum
}

/// e^y for finite `y`: 2^n × e^r, r = y − n ln 2 and |r| ≤ ½ ln 2, or the
/// power of two at every exponent's reach where n lies past it.
fn exp_wide(y: Wide) -> Wide {
    if y.exp < -120 {
        // 1 + y, where y falls below the working bits.
        return if y.negative {
            Wide::ONE.just_under()
        } else {
            Wide::ONE
        };
    }
    let n = y.div(LN2).nearest_integer();
    if n.abs() > i64::from(EXPONENT_REACH) {
        return Wide::ONE.scaled(if n < 0 {
            -EXPONENT_REACH
        } else {
            EXPONENT_REACH
        });
    }
    let r = y.sub(LN2.mul(Wide::from_i64(n)));
    let n = i32::try_from(n).unwrap_or(0);
    expm1(r).add(Wide::ONE).scaled(n)
}

/// e^x for finite nonzero `x`.
#[must_use]
pub fn exp(x: Parts) -> Exact {
    exp_wide(Wide::of(x)).exact()
}

/// 2^x − 1 for finite `x`.
#[must_use]
pub fn exp2m1(x: Parts) -> Exact {
    let x = Wide::of(x);
    if x.cmp_magnitude(Wide::ONE) != Ordering::Greater {
        return expm1(x.mul(LN2)).exact();
    }
    // 2^x = 2^n × 2^f, n the nearest integer and |f| ≤ ½.
    let n = x.nearest_integer();
    if n > i64::from(EXPONENT_REACH) {
        return Wide::ONE.scaled(EXPONENT_REACH).exact();
    }
    if n < -128 {
        // 2^x falls below the working bits, which would leave −1 itself:
        // the value lies just above it.
        return Wide {
            negative: true,
            exp: -1,
            sig: u128::MAX,
        }
        .exact();
    }
    let f = x.sub(Wide::from_i64(n));
    let power = expm1(f.mul(LN2)).add(Wide::ONE);
    let n = i32::try_from(n).unwrap_or(0);
    power.scaled(n).sub(Wide::ONE).exact()
}

/// 2 atanh z for |z| < 0.18 by its series: ln((1 + z)/(1 − z)).
fn two_atanh(z: Wide) -> Wide {
    let square = z.mul(z);
    let mut sum = z;
    let mut power = z;
    for k in 1..MOST_TERMS {
        power = power.mul(square);
        let term = power.div_small(2 * k + 1);
        sum = sum.add(term);
        if Wide::negligible(term, sum) {
            break;
        }
    }
    sum.scaled(1)
}

/// ln(1 + x) for x within [1/√2 − 1, √2 − 1), as 2 atanh(x / (x + 2)):
/// exact relative to x however small it is.
fn ln_1p(x: Wide) -> Wide {
    two_atanh(x.div(x.add(Wide::small(2))))
}

/// Positive `x` as m × 2^e, m in [1/√2, √2): e, and ln m.
fn split_log(x: Wide) -> (i32, Wide) {
    let root_half = Wide::constant(-1, 0xB504_F333_F9DE_6484_597D_89B3_754A_BE9F);
    let mut e = x.exp;
    let mut m = Wide { exp: 0, ..x };
    if m.cmp_magnitude(root_half.scaled(1)) != Ordering::Less {
        m = m.scaled(-1);
        e += 1;
    }
    (e, ln_1p(m.sub(Wide::ONE)))
}

/// log2 of positive `x`.
fn log2(x: Wide) -> Wide {
    let (e, ln) = split_log(x);
    Wide::from_i64(i64::from(e)).add(ln.div(LN2))
}

/// ln of positive `x`.
fn ln_wide(x: Wide) -> Wide {
    let (e, ln) = split_log(x);
    LN2.mul(Wide::from_i64(i64::from(e))).add(ln)
}

/// ln x for positive finite `x` other than one.
#[must_use]
pub fn ln(x: Parts) -> Exact {
    ln_wide(Wide::of(x)).exact()
}

/// log10 x for positive finite `x`: exact where `x` is a power of ten.
#[must_use]
pub fn log10(x: Parts) -> Exact {
    if let Some(n) = power_of_ten(x) {
        let n = Parts {
            negative: false,
            exp: 63,
            sig: u64::from(n),
        };
        return Exact::of(n.normalized());
    }
    ln_wide(Wide::of(x)).div(LN10).exact()
}

/// The `n` with 10^n = `x`, where one does and a significand holds it: the
/// powers from 1 to 10^27, past which five's powers outgrow 64 bits.
fn power_of_ten(x: Parts) -> Option<u32> {
    let x = x.normalized();
    if x.negative || x.is_zero() {
        return None;
    }
    // 10^n is 5^n × 2^n.
    let zeros = x.sig.trailing_zeros();
    let odd = x.sig >> zeros;
    let twos = i64::from(x.exp) - 63 + i64::from(zeros);
    let mut five: u64 = 1;
    for n in 0..28 {
        if odd == five && twos == i64::from(n) {
            return Some(n);
        }
        five = five.checked_mul(5)?;
    }
    None
}

/// y × log2(x), `x` positive.
#[must_use]
pub fn y_log2_x(y: Parts, x: Parts) -> Exact {
    Wide::of(y).mul(log2(Wide::of(x))).exact()
}

/// y × log2(1 + x), x > −1: by atanh for |x| < 1 − √2/2, through log2(1 + x)
/// past it.
#[must_use]
pub fn y_log2_1p_x(y: Parts, x: Parts) -> Exact {
    let x = Wide::of(x);
    let bound = Wide::constant(-2, 0x95F6_1998_0C43_36F7_4D04_EC99_156A_82C1);
    let log = if x.cmp_magnitude(bound) == Ordering::Less {
        ln_1p(x).div(LN2)
    } else {
        log2(x.add(Wide::ONE))
    };
    Wide::of(y).mul(log).exact()
}

/// x^y for positive finite `x` other than one and finite nonzero `y`: e^(y
/// ln x), exact wherever the power is.
#[must_use]
pub fn pow(x: Parts, y: Parts) -> Exact {
    if let Some(exact) = exact_power(x, y) {
        return exact;
    }
    exp_wide(Wide::of(y).mul(ln_wide(Wide::of(x)))).exact()
}

/// x^y where it is exact. Every finite `y` is m/2^k, and the power is exact
/// only where x's 2^k-th root is and that root's mth power fits an exact
/// value's 127-bit significand.
fn exact_power(x: Parts, y: Parts) -> Option<Exact> {
    let (m, k) = dyadic(y)?;
    integer_power(exact_root(x, k)?, m)
}

/// `y` as m/2^k: m an integer below 2^31 in magnitude, odd unless k is
/// nought.
fn dyadic(y: Parts) -> Option<(i32, u32)> {
    let y = y.normalized();
    if y.is_zero() {
        return None;
    }
    let zeros = y.sig.trailing_zeros();
    let scale = y.exp - 63 + i32::try_from(zeros).ok()?;
    if scale >= 0 {
        return Some((integer(y)?, 0));
    }
    let odd = i32::try_from(y.sig >> zeros).ok()?;
    Some((if y.negative { -odd } else { odd }, scale.unsigned_abs()))
}

/// The 2^`k`-th root of positive `x`, where it is exact: each halving takes
/// an even power of two and a square odd part.
fn exact_root(x: Parts, k: u32) -> Option<Parts> {
    let x = x.normalized();
    let zeros = x.sig.trailing_zeros();
    let mut odd = x.sig >> zeros;
    let mut scale = x.exp - 63 + i32::try_from(zeros).ok()?;
    for _ in 0..k {
        let root = odd.isqrt();
        if scale % 2 != 0 || root * root != odd {
            return None;
        }
        odd = root;
        scale /= 2;
    }
    let lead = odd.leading_zeros();
    Some(Parts {
        negative: false,
        exp: scale + 63 - i32::try_from(lead).ok()?,
        sig: odd << lead,
    })
}

/// x^n where the power fits an exact value's significand, below its carry
/// bit: an odd significand's power that does, times the power of two the
/// rest is.
fn integer_power(x: Parts, n: i32) -> Option<Exact> {
    const CARRY: u128 = 1 << 127;
    let x = x.normalized();
    let zeros = x.sig.trailing_zeros();
    let odd = u128::from(x.sig >> zeros);
    if n < 0 && odd != 1 {
        return None;
    }
    let mut power: u128 = 1;
    if odd != 1 {
        for _ in 0..n.unsigned_abs() {
            power = power.checked_mul(odd).filter(|&power| power < CARRY)?;
        }
    }
    // x = odd × 2^(exp − 63 + zeros), so x^n carries n times that exponent.
    let scale = i64::from(x.exp) - 63 + i64::from(zeros);
    let lead = 127 - i64::from(power.leading_zeros());
    let exp =
        (i64::from(n) * scale + lead).clamp(-i64::from(EXPONENT_REACH), i64::from(EXPONENT_REACH));
    Some(Exact {
        negative: false,
        exp: i32::try_from(exp).ok()?,
        sig: power << (power.leading_zeros() - 1),
        sticky: false,
    })
}

/// `y` as an integer, where it is one below 2^31 in magnitude.
fn integer(y: Parts) -> Option<i32> {
    let y = y.normalized();
    if y.is_zero() || y.exp >= 31 {
        return None;
    }
    let shift = u32::try_from(63 - y.exp).ok()?;
    if shift >= 64 || y.sig & ((1u64 << shift) - 1) != 0 {
        return None;
    }
    let magnitude = i32::try_from(y.sig >> shift).ok()?;
    Some(if y.negative { -magnitude } else { magnitude })
}

#[cfg(test)]
#[path = "elementary_tests.rs"]
mod tests;
