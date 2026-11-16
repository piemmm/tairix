//! The 8087's arithmetic on finite values: each result exact over its
//! operands' significands, as `lib/softfloat`'s core holds it, then rounded
//! once by the 8087's own rules (Intel 8087 data sheet 205835; the 80287
//! programmer's reference for its unnormal rules): its register wrap, its
//! chop-mode results, and its partial remainder.

use tairix_softfloat::exact::{Exact, Parts};
use tairix_softfloat::{Mode, Rounding};

use tairix_softfloat::RoundedSig;

use tairix_softfloat::extended::{BIAS, EXP_MAX, INTEGER};
use tairix_softfloat::{Binary, Extended};

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

/// The mode the control word's RC field names.
#[must_use]
pub const fn mode_of(rc: u16) -> Mode {
    match rc & 3 {
        0 => Mode::Nearest,
        1 => Mode::Down,
        2 => Mode::Up,
        _ => Mode::Zero,
    }
}

/// How far an unmasked overflow or underflow moves a register result's
/// exponent back into range.
pub const WRAP: i32 = 24_576;

/// A result rounded for a register: the value, or none where an unmasked
/// exception leaves the destination as it was; and the flags raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rounded {
    /// The value.
    pub value: Option<Extended>,
    /// The exceptions it raised.
    pub flags: u16,
}

/// `x` rounded for a register under `masks`.
#[must_use]
pub fn round(x: Exact, rounding: Rounding, masks: u16) -> Rounded {
    let x = x.normalized(false);
    if x.is_zero() {
        return Rounded {
            value: Some(Extended::ZERO.with_sign(x.negative)),
            flags: 0,
        };
    }
    let biased = x.exp.saturating_add(BIAS);
    if biased < 1 {
        return tiny(x, rounding, masks);
    }
    let RoundedSig {
        sig,
        carried,
        inexact,
    } = x.rounded(rounding);
    let biased = biased.saturating_add(i32::from(carried));
    let precision = if inexact { flag::PRECISION } else { 0 };
    if biased > i32::from(EXP_MAX) {
        if masks & flag::OVERFLOW == 0 {
            // Unmasked, the result's exponent moves down by the wrap,
            // its significand chopped.
            let wrapped = x.exp.saturating_add(BIAS - WRAP);
            return Rounded {
                value: Some(Extended::new(x.negative, field(wrapped), x.chopped())),
                flags: flag::OVERFLOW,
            };
        }
        return Rounded {
            value: Some(overflowed(x, rounding)),
            flags: flag::OVERFLOW | flag::PRECISION,
        };
    }
    Rounded {
        value: Some(Extended::new(x.negative, field(biased), sig)),
        flags: precision,
    }
}

/// A biased exponent known to be in range, as the field holds it.
fn field(biased: i32) -> u16 {
    u16::try_from(biased.clamp(0, i32::from(EXP_MAX))).unwrap_or(0)
}

/// Whether the 8087 answers a masked overflow of sign `negative` with the
/// largest finite value rather than infinity: only where `mode` rounds toward
/// zero from it by direction, as chopping gives infinity too.
const fn overflows_to_largest(mode: Mode, negative: bool) -> bool {
    match mode {
        Mode::Up => negative,
        Mode::Down => !negative,
        Mode::Nearest | Mode::NearestAway | Mode::Zero => false,
    }
}

/// A masked overflow's register result, an unnormal one keeping its own
/// significand.
fn overflowed(x: Exact, rounding: Rounding) -> Extended {
    if !overflows_to_largest(rounding.mode(), x.negative) {
        return Extended::INFINITY.with_sign(x.negative);
    }
    let kept = !0u64 << (64 - rounding.bits());
    let chopped = x.chopped();
    let sig = if chopped & INTEGER != 0 {
        kept
    } else {
        chopped & kept
    };
    Extended::new(x.negative, EXP_MAX, sig)
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
            value: Some(Extended::new(x.negative, field(wrapped), x.chopped())),
            flags: flag::UNDERFLOW,
        };
    }
    let shift = u32::try_from(1i32.saturating_sub(biased)).unwrap_or(u32::MAX);
    let d = x.shifted_right(shift);
    let RoundedSig { sig, inexact, .. } = d.rounded(rounding);
    let exponent = u16::from(sig & INTEGER != 0);
    let mut flags = if inexact { flag::PRECISION } else { 0 };
    if inexact || rounding.mode() == Mode::Zero || unmasked {
        flags |= flag::UNDERFLOW;
    }
    Rounded {
        value: Some(Extended::new(x.negative, exponent, sig)),
        flags,
    }
}

/// A value rounded for a short or long real in memory: the bits, or none
/// where an unmasked overflow or underflow refuses the store; and the flags
/// raised. `p` must be normalized.
#[must_use]
pub fn round_binary(p: Parts, format: Binary, mode: Mode, masks: u16) -> (Option<u64>, u16) {
    if p.is_zero() {
        return (Some(format.zero(p.negative)), 0);
    }
    let rounding = Rounding::of(format, mode);
    let x = Exact::of(p);
    let pack = |field: u64, sig: u64| format.pack(p.negative, field, sig);
    if p.exp < format.min_exp() {
        if masks & flag::UNDERFLOW == 0 {
            return (None, flag::UNDERFLOW);
        }
        let shift = u32::try_from(format.min_exp().saturating_sub(p.exp)).unwrap_or(u32::MAX);
        let RoundedSig { sig, inexact, .. } = x.shifted_right(shift).rounded(rounding);
        let mut flags = if inexact { flag::PRECISION } else { 0 };
        if inexact || mode == Mode::Zero {
            flags |= flag::UNDERFLOW;
        }
        return (Some(pack(u64::from(sig & INTEGER != 0), sig)), flags);
    }
    let RoundedSig {
        sig,
        carried,
        inexact,
    } = x.rounded(rounding);
    let exp = p.exp.saturating_add(i32::from(carried));
    if exp > format.max_exp() {
        if masks & flag::OVERFLOW == 0 {
            return (None, flag::OVERFLOW);
        }
        let bits = if overflows_to_largest(mode, p.negative) {
            format.largest(p.negative)
        } else {
            format.infinity(p.negative)
        };
        return (Some(bits), flag::OVERFLOW | flag::PRECISION);
    }
    let biased = u64::try_from(exp + format.bias()).unwrap_or(0);
    let flags = if inexact { flag::PRECISION } else { 0 };
    (Some(pack(biased, sig)), flags)
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
