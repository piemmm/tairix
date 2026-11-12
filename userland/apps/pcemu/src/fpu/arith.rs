//! The 8087's arithmetic on finite values: each result exact over its
//! operands' significands, then rounded once (Intel 8087 data sheet 205835;
//! the 80287 programmer's reference for its unnormal rules).
//!
//! An exact result keeps 62 bits below the 64-bit significand and a sticky
//! bit for anything further down, which is enough to round any sum,
//! product, quotient or root correctly at every precision.

use core::cmp::Ordering;

use super::real::{Binary, Real, BIAS, EXP_MAX, INTEGER};

/// The exception flags, in the status and control words' low six bits.
pub mod flag {
    /// Invalid operation.
    pub const INVALID: u16 = 0x01;
    /// Denormalized operand.
    pub const DENORMAL: u16 = 0x02;
    /// Zero divide.
    pub const ZERO_DIVIDE: u16 = 0x04;
    /// Overflow.
    pub const OVERFLOW: u16 = 0x08;
    /// Underflow.
    pub const UNDERFLOW: u16 = 0x10;
    /// Precision: the result was rounded.
    pub const PRECISION: u16 = 0x20;
    /// Every one of them.
    pub const ALL: u16 = 0x3F;
}

/// The rounding control's modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// To nearest, ties to even.
    Nearest,
    /// Toward minus infinity.
    Down,
    /// Toward plus infinity.
    Up,
    /// Toward zero.
    Chop,
}

impl Mode {
    /// The mode the control word's RC field names.
    #[must_use]
    pub const fn of(rc: u16) -> Self {
        match rc & 3 {
            0 => Self::Nearest,
            1 => Self::Down,
            2 => Self::Up,
            _ => Self::Chop,
        }
    }

    /// Whether a result inexact by any amount moves away from zero.
    const fn away(self, negative: bool) -> bool {
        match self {
            Self::Up => !negative,
            Self::Down => negative,
            Self::Nearest | Self::Chop => false,
        }
    }
}

/// How a result is rounded: the mode, and the significand bits kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rounding {
    /// The mode.
    pub mode: Mode,
    /// Significand bits kept: 24, 53 or 64.
    pub bits: u32,
}

impl Rounding {
    /// Full precision in `mode`.
    #[must_use]
    pub const fn extended(mode: Mode) -> Self {
        Self { mode, bits: 64 }
    }
}

/// How far an unmasked overflow or underflow moves a register result's
/// exponent back into range.
pub const WRAP: i32 = 24_576;

/// The integer bit's place in an [`Exact`] significand.
const POINT: u32 = 126;

/// A finite value's parts: `sig × 2^(exp − 63)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    /// The sign.
    pub negative: bool,
    /// The exponent of the significand's integer bit, unbiased.
    pub exp: i32,
    /// The significand.
    pub sig: u64,
}

impl Parts {
    /// The parts of a finite value, a denormal read as the unnormal it
    /// equals.
    #[must_use]
    pub fn of(x: Real) -> Self {
        let field = i32::from(x.exponent()).max(1);
        Self {
            negative: x.negative(),
            exp: field - BIAS,
            sig: x.sig,
        }
    }

    /// Whether the integer bit is set.
    #[must_use]
    pub const fn normal(self) -> bool {
        self.sig & INTEGER != 0
    }

    /// Whether the value is zero, as a pseudo-zero's is.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.sig == 0
    }

    /// These parts with the significand's leading zeros shifted out, the
    /// exponent lowered to match; zero stays as it is.
    #[must_use]
    pub fn normalized(self) -> Self {
        if self.sig == 0 {
            return self;
        }
        let zeros = self.sig.leading_zeros();
        Self {
            exp: self.exp - i32::try_from(zeros).unwrap_or(0),
            sig: self.sig << zeros,
            ..self
        }
    }

    /// The magnitudes compared, each normalized first.
    #[must_use]
    pub fn cmp_magnitude(self, other: Self) -> Ordering {
        let (a, b) = (self.normalized(), other.normalized());
        match (a.is_zero(), b.is_zero()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => a.exp.cmp(&b.exp).then(a.sig.cmp(&b.sig)),
        }
    }

    /// The values compared, zeros of either sign equal.
    #[must_use]
    pub fn cmp_value(self, other: Self) -> Ordering {
        if self.is_zero() && other.is_zero() {
            return Ordering::Equal;
        }
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
        }
    }
}

/// An exact result before rounding: `sig × 2^(exp − 126)`, with more bits
/// below `sig`'s last one where `sticky`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exact {
    /// The sign.
    pub negative: bool,
    /// The exponent of bit 126, the integer bit's place.
    pub exp: i32,
    /// The significand, bit 127 free for a carry.
    pub sig: u128,
    /// Whether anything nonzero lies below `sig`.
    pub sticky: bool,
}

impl Exact {
    /// The exact value of `p`.
    #[must_use]
    pub fn of(p: Parts) -> Self {
        Self {
            negative: p.negative,
            exp: p.exp,
            sig: u128::from(p.sig) << (POINT - 63),
            sticky: false,
        }
    }

    /// Whether the value is exactly zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.sig == 0 && !self.sticky
    }

    /// Shifted right `n` places, what falls off kept as sticky.
    fn shifted_right(self, n: u32) -> Self {
        if n == 0 {
            return self;
        }
        let (sig, lost) = if n >= 128 {
            (0, self.sig != 0)
        } else {
            (self.sig >> n, self.sig & ((1 << n) - 1) != 0)
        };
        Self {
            exp: self
                .exp
                .saturating_add(i32::try_from(n).unwrap_or(i32::MAX)),
            sig,
            sticky: self.sticky || lost,
            ..self
        }
    }

    /// A carry out of the integer bit's place brought down; with `full`,
    /// leading zeros shifted up too.
    fn normalized(self, full: bool) -> Self {
        let mut x = self;
        if x.sig >> (POINT + 1) != 0 {
            x = x.shifted_right(1);
        }
        if full && x.sig != 0 {
            let zeros = x.sig.leading_zeros().saturating_sub(1);
            x.sig <<= zeros;
            x.exp = x.exp.saturating_sub(i32::try_from(zeros).unwrap_or(0));
        }
        x
    }

    /// The 64-bit significand, chopped.
    const fn chopped(self) -> u64 {
        let bits = (self.sig >> (POINT - 63)).to_le_bytes();
        u64::from_le_bytes([
            bits[0], bits[1], bits[2], bits[3], bits[4], bits[5], bits[6], bits[7],
        ])
    }
}

/// `a + b`, or `a − b` where `subtract`, exactly. The result is normalized
/// as the operand of larger magnitude is, as the 8087's unnormal arithmetic
/// leaves it; an exactly zero result is the caller's to sign.
#[must_use]
pub fn add(a: Parts, b: Parts, subtract: bool) -> Exact {
    let b = Parts {
        negative: b.negative != subtract,
        ..b
    };
    let (big, small) = if a.cmp_magnitude(b) == Ordering::Less {
        (b, a)
    } else {
        (a, b)
    };
    let full = big.normal();
    let (big, small) = if full {
        (big.normalized(), small.normalized())
    } else {
        (big, small)
    };
    // Aligned at the larger exponent, which an unnormal of the larger
    // magnitude need not have when the other is unnormal too; a pseudo-zero
    // adds nothing and aligns nothing.
    let exp = if small.is_zero() {
        big.exp
    } else {
        big.exp.max(small.exp)
    };
    let aligned = |p: Parts| {
        let gap = u32::try_from(exp - p.exp).unwrap_or(u32::MAX);
        Exact::of(p).shifted_right(gap)
    };
    let (x, y) = (aligned(big), aligned(small));
    // At most one side lost bits, the one shifted; they stay below the sum.
    let (sig, sticky) = if x.negative == y.negative {
        (x.sig + y.sig, x.sticky || y.sticky)
    } else if y.sticky {
        // Below `y`'s last bit lies a little more of it to take away.
        (x.sig - y.sig - 1, true)
    } else {
        (x.sig - y.sig, x.sticky)
    };
    Exact {
        negative: big.negative,
        exp,
        sig,
        sticky,
    }
    .normalized(full)
}

/// `a × b` exactly: normalized where both are, unnormal where either is.
#[must_use]
pub fn multiply(a: Parts, b: Parts) -> Exact {
    Exact {
        negative: a.negative != b.negative,
        exp: a.exp + b.exp,
        sig: u128::from(a.sig) * u128::from(b.sig),
        sticky: false,
    }
    .normalized(false)
}

/// `a ÷ b` exactly, `b` normal: unnormal where `a` is.
#[must_use]
pub fn divide(a: Parts, b: Parts) -> Exact {
    // sig × 2^127 / b.sig, two 64-bit steps of the long division.
    let divisor = u128::from(b.sig);
    let first = u128::from(a.sig) << 63;
    let (high, rest) = (first / divisor, first % divisor);
    let second = rest << 64;
    let (low, rest) = (second / divisor, second % divisor);
    Exact {
        negative: a.negative != b.negative,
        exp: a.exp - b.exp - 1,
        sig: (high << 64) | low,
        sticky: rest != 0,
    }
    .normalized(false)
}

/// The square root of `a`, positive and normal, exactly.
#[must_use]
pub fn square_root(a: Parts) -> Exact {
    // a = m × 2^e with an even e; sqrt(m) taken to 34 places below the
    // point, as many bits as rounding needs and two more.
    let odd = (a.exp - 63) & 1 != 0;
    let (m, e) = if odd {
        (u128::from(a.sig) << 1, a.exp - 64)
    } else {
        (u128::from(a.sig), a.exp - 63)
    };
    let (root, rest) = isqrt_scaled(m, 34);
    let x = Exact {
        negative: false,
        exp: e / 2 - 34 + 126,
        sig: root,
        sticky: rest,
    };
    x.normalized(true)
}

/// `⌊√(m × 4^places)⌋` for `m < 2^66`, and whether a remainder was left.
fn isqrt_scaled(m: u128, places: u32) -> (u128, bool) {
    let mut rest: u128 = 0;
    let mut root: u128 = 0;
    // Two bits of the radicand at a time, from the top of m's 66 bits.
    for n in (0..33 + places).rev() {
        let pair = if n >= places {
            (m >> (2 * (n - places))) & 3
        } else {
            0
        };
        rest = (rest << 2) | pair;
        let trial = (root << 2) | 1;
        root <<= 1;
        if rest >= trial {
            rest -= trial;
            root |= 1;
        }
    }
    (root, rest != 0)
}

/// A result rounded for a register: the value, or none where an unmasked
/// exception leaves the destination as it was; and the flags raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rounded {
    /// The value.
    pub value: Option<Real>,
    /// The exceptions it raised.
    pub flags: u16,
}

/// The significand rounded to `rounding`'s bits at the integer bit's fixed
/// place: the 64-bit significand, whether it carried out of the integer
/// bit, and whether it was inexact.
fn round_bits(x: Exact, rounding: Rounding) -> (u64, bool, bool) {
    let lsb = POINT + 1 - rounding.bits;
    let below = x.sig & ((1 << lsb) - 1);
    let half = 1 << (lsb - 1);
    let inexact = below != 0 || x.sticky;
    let up = match rounding.mode {
        Mode::Nearest => below > half || (below == half && (x.sticky || (x.sig >> lsb) & 1 != 0)),
        mode => inexact && mode.away(x.negative),
    };
    let mut kept = x.sig & !((1 << lsb) - 1);
    if up {
        kept += 1 << lsb;
    }
    let carried = kept >> (POINT + 1) != 0;
    if carried {
        kept >>= 1;
    }
    let sig = Exact { sig: kept, ..x }.chopped();
    (sig, carried, inexact)
}

/// `x` rounded for a register under `masks`.
#[must_use]
pub fn round(x: Exact, rounding: Rounding, masks: u16) -> Rounded {
    let x = x.normalized(false);
    if x.is_zero() {
        return Rounded {
            value: Some(Real::ZERO.with_sign(x.negative)),
            flags: 0,
        };
    }
    let biased = x.exp.saturating_add(BIAS);
    if biased < 1 {
        return tiny(x, rounding, masks);
    }
    let (sig, carried, inexact) = round_bits(x, rounding);
    let biased = biased.saturating_add(i32::from(carried));
    let precision = if inexact { flag::PRECISION } else { 0 };
    if biased > i32::from(EXP_MAX) {
        if masks & flag::OVERFLOW == 0 {
            // Unmasked, the result's exponent moves down by the wrap,
            // its significand chopped.
            let wrapped = x.exp.saturating_add(BIAS - WRAP);
            return Rounded {
                value: Some(Real::new(x.negative, field(wrapped), x.chopped())),
                flags: flag::OVERFLOW,
            };
        }
        return Rounded {
            value: Some(overflowed(x, rounding)),
            flags: flag::OVERFLOW | flag::PRECISION,
        };
    }
    Rounded {
        value: Some(Real::new(x.negative, field(biased), sig)),
        flags: precision,
    }
}

/// A biased exponent known to be in range, as the field holds it.
fn field(biased: i32) -> u16 {
    u16::try_from(biased.clamp(0, i32::from(EXP_MAX))).unwrap_or(0)
}

/// A masked overflow's result: infinity, or the largest finite value where
/// the mode rounds toward zero from it. The 8087 gives infinity when
/// chopping, and an unnormal result keeps its own significand.
fn overflowed(x: Exact, rounding: Rounding) -> Real {
    let finite = match rounding.mode {
        Mode::Up => x.negative,
        Mode::Down => !x.negative,
        Mode::Nearest | Mode::Chop => false,
    };
    if !finite {
        return Real::INFINITY.with_sign(x.negative);
    }
    let kept = !0u64 << (64 - rounding.bits);
    let chopped = x.chopped();
    let sig = if chopped & INTEGER != 0 {
        kept
    } else {
        chopped & kept
    };
    Real::new(x.negative, EXP_MAX, sig)
}

/// A result below the smallest exponent: denormalized and rounded where
/// underflow is masked, flagged on tininess when chopping and on tininess
/// with loss otherwise; unmasked, its exponent moved up by the wrap.
fn tiny(x: Exact, rounding: Rounding, masks: u16) -> Rounded {
    let biased = x.exp.saturating_add(BIAS);
    let unmasked = masks & flag::UNDERFLOW == 0;
    let wrapped = biased.saturating_add(WRAP);
    if unmasked && wrapped >= 1 {
        return Rounded {
            value: Some(Real::new(x.negative, field(wrapped), x.chopped())),
            flags: flag::UNDERFLOW,
        };
    }
    let shift = u32::try_from(1i32.saturating_sub(biased)).unwrap_or(u32::MAX);
    let d = x.shifted_right(shift);
    let (sig, _, inexact) = round_bits(d, rounding);
    let exponent = u16::from(sig & INTEGER != 0);
    let mut flags = if inexact { flag::PRECISION } else { 0 };
    if inexact || rounding.mode == Mode::Chop || unmasked {
        flags |= flag::UNDERFLOW;
    }
    Rounded {
        value: Some(Real::new(x.negative, exponent, sig)),
        flags,
    }
}

/// A value rounded for a short or long real in memory: the bits, or none
/// where an unmasked overflow or underflow refuses the store; and the flags
/// raised. `p` must be normalized.
#[must_use]
pub fn round_binary(p: Parts, format: Binary, mode: Mode, masks: u16) -> (Option<u64>, u16) {
    let sign = u64::from(p.negative) << (format.width - 1);
    if p.is_zero() {
        return (Some(sign), 0);
    }
    let rounding = Rounding {
        mode,
        bits: format.precision,
    };
    let x = Exact::of(p);
    let fraction_bits = format.precision - 1;
    let fraction_mask = (1u64 << fraction_bits) - 1;
    let pack = |field: u64, sig: u64| {
        sign | (field << fraction_bits) | ((sig >> (64 - format.precision)) & fraction_mask)
    };
    if p.exp < format.min_exp() {
        if masks & flag::UNDERFLOW == 0 {
            return (None, flag::UNDERFLOW);
        }
        let shift = u32::try_from(format.min_exp().saturating_sub(p.exp)).unwrap_or(u32::MAX);
        let (sig, _, inexact) = round_bits(x.shifted_right(shift), rounding);
        let mut flags = if inexact { flag::PRECISION } else { 0 };
        if inexact || mode == Mode::Chop {
            flags |= flag::UNDERFLOW;
        }
        return (Some(pack(u64::from(sig & INTEGER != 0), sig)), flags);
    }
    let (sig, carried, inexact) = round_bits(x, rounding);
    let exp = p.exp.saturating_add(i32::from(carried));
    if exp > format.max_exp() {
        if masks & flag::OVERFLOW == 0 {
            return (None, flag::OVERFLOW);
        }
        let finite = match mode {
            Mode::Up => p.negative,
            Mode::Down => !p.negative,
            Mode::Nearest | Mode::Chop => false,
        };
        let bits = if finite {
            pack(format.exp_mask() - 1, !0)
        } else {
            sign | (format.exp_mask() << fraction_bits)
        };
        return (Some(bits), flag::OVERFLOW | flag::PRECISION);
    }
    let biased = u64::try_from(exp + format.bias).unwrap_or(0);
    let flags = if inexact { flag::PRECISION } else { 0 };
    (Some(pack(biased, sig)), flags)
}

/// `p`, normalized, rounded to an integer in `mode`: its magnitude, none
/// past `u64`'s; and whether it was inexact.
#[must_use]
pub fn to_integer(p: Parts, mode: Mode) -> (Option<u64>, bool) {
    if p.is_zero() {
        return (Some(0), false);
    }
    if p.exp >= 64 {
        return (None, false);
    }
    if p.exp >= 63 {
        return (Some(p.sig), false);
    }
    let shift = u32::try_from(63 - p.exp).unwrap_or(u32::MAX);
    let (whole, below, half, sticky) = if shift >= 65 {
        (0, 0, 1, true)
    } else if shift == 64 {
        (0, p.sig, INTEGER, false)
    } else {
        let below = p.sig & ((1 << shift) - 1);
        (p.sig >> shift, below, 1 << (shift - 1), false)
    };
    let inexact = below != 0 || sticky;
    let up = match mode {
        Mode::Nearest => !sticky && (below > half || (below == half && whole & 1 != 0)),
        mode => inexact && mode.away(p.negative),
    };
    (whole.checked_add(u64::from(up)), inexact)
}

/// The partial remainder of `a` by `b`, both normal and `b` nonzero, as
/// FPREM takes it: complete where the exponents differ by less than 64,
/// else reduced by 63 places of the quotient. The remainder, and the
/// quotient where it was complete.
#[must_use]
pub fn remainder(a: Parts, b: Parts) -> (Exact, Option<u64>) {
    let gap = a.exp - b.exp;
    if gap < 0 {
        return (Exact::of(a), Some(0));
    }
    let (places, complete) = if gap < 64 {
        (u32::try_from(gap).unwrap_or(0), true)
    } else {
        (63, false)
    };
    let dividend = u128::from(a.sig) << places;
    let divisor = u128::from(b.sig);
    let (quotient, rest) = (dividend / divisor, dividend % divisor);
    // The remainder is rest × 2^(e − 63) at the scale the division ran at.
    let scale = a.exp - i32::try_from(places).unwrap_or(0);
    let rest = Parts {
        negative: a.negative,
        exp: scale,
        sig: u64::try_from(rest).unwrap_or(0),
    };
    // Below 2^64: the dividend is below 2^127 and the divisor at least 2^63.
    let quotient = complete.then(|| u64::try_from(quotient).unwrap_or(u64::MAX));
    (Exact::of(rest).normalized(true), quotient)
}

#[cfg(test)]
#[path = "arith_tests.rs"]
mod tests;
