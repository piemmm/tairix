//! Exact conversion between binary values and decimal ones. A finite value is
//! written to any number of significant digits in any rounding mode, and a
//! decimal value is read exactly enough to round correctly at any precision.
//! Both work from the whole expansion, so neither approximates.

use core::cmp::Ordering;

use crate::big::Big;
use crate::exact::{Exact, Parts, POINT};
use crate::extended::{BIAS, EXP_MAX};
use crate::mode::Mode;

/// The most significant digits a [`Decimal`] holds: as many as `u128` does.
pub const MAX_DIGITS: u32 = 38;

/// The least unbiased exponent a finite double-extended value's top bit has:
/// the least subnormal's.
const LEAST_EXP: i32 = 1 - BIAS - 63;

/// The greatest.
const MOST_EXP: i32 = EXP_MAX as i32 - BIAS;

/// The places a decimal value's first digit may stand at for the value to be
/// worked out. Past the last, the value is at least 10^4933, above
/// double-extended's largest finite value; before the first, it is below
/// 10^−4951, less than half its least subnormal.
const PLACES: core::ops::RangeInclusive<i64> = -4951..=4932;

/// Digits taken out of an expansion at a time, and the power of ten that is.
const CHUNK_DIGITS: u32 = 9;
const CHUNK: u32 = 1_000_000_000;

/// A decimal value: `±significand × 10^exponent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    /// The sign.
    pub negative: bool,
    /// The digits, as an integer.
    pub significand: u128,
    /// The power of ten the last digit stands at.
    pub exponent: i32,
}

/// Where a decimal value lies past double-extended's range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Beyond {
    /// Above its largest finite value.
    Above,
    /// Not zero, but below half its least subnormal.
    Below,
}

/// Ten to the `n`, `n` at most [`MAX_DIGITS`].
const fn pow10(n: u32) -> u128 {
    10u128.pow(n)
}

/// The digits `n`, not zero, has.
const fn digits_of(n: u128) -> u32 {
    n.ilog10() + 1
}

impl Decimal {
    /// `p` to `digits` significant digits, rounded in `mode`, and whether
    /// that was inexact. The significand has exactly `digits` digits, unless
    /// `p` is zero, when it and the exponent are zero. `None` where `digits`
    /// is not 1 to [`MAX_DIGITS`], or `p` lies outside double-extended's
    /// range.
    #[must_use]
    pub fn of(p: Parts, digits: u32, mode: Mode) -> Option<(Self, bool)> {
        if !(1..=MAX_DIGITS).contains(&digits) {
            return None;
        }
        if p.is_zero() {
            let zero = Self {
                negative: p.negative,
                significand: 0,
                exponent: 0,
            };
            return Some((zero, false));
        }
        let p = p.normalized();
        if !(LEAST_EXP..=MOST_EXP).contains(&p.exp) {
            return None;
        }
        let mut sink = Sink::new(digits);
        match u32::try_from(p.exp - 63) {
            Ok(shift) => whole(&mut sink, p.sig, shift)?,
            Err(_) => split(&mut sink, p.sig, (63 - p.exp).unsigned_abs())?,
        }
        Some(sink.rounded(p.negative, mode))
    }

    /// The value exactly enough to round correctly at any precision: its top
    /// bits, and a sticky bit for any below them. A zero is a zero of its
    /// sign.
    pub fn exact(self) -> Result<Exact, Beyond> {
        let s = self.significand;
        if s == 0 {
            return Ok(Exact {
                negative: self.negative,
                exp: 0,
                sig: 0,
                sticky: false,
            });
        }
        let first = i64::from(self.exponent) + i64::from(digits_of(s) - 1);
        if first > *PLACES.end() {
            return Err(Beyond::Above);
        }
        if first < *PLACES.start() {
            return Err(Beyond::Below);
        }
        let x = match u32::try_from(self.exponent) {
            Ok(k) => multiplied(s, k).ok_or(Beyond::Above)?,
            Err(_) => divided(s, self.exponent.unsigned_abs()).ok_or(Beyond::Below)?,
        };
        Ok(Exact {
            negative: self.negative,
            ..x
        })
    }
}

/// `s × 10^k`, `s` not zero, exactly enough to round, positive; `None`
/// where it passes what `Big` holds, which no value in `PLACES` does.
fn multiplied(s: u128, k: u32) -> Option<Exact> {
    // s × 10^k is s × 5^k, exactly, times 2^k.
    let mut big = Big::from_u128(s);
    if !big.mul_pow5(k) {
        return None;
    }
    let (sig, sticky) = big.top(POINT + 1);
    Some(Exact {
        negative: false,
        exp: step(big.bit_len() - 1) + step(k),
        sig,
        sticky,
    })
}

/// `s / 10^m`, `s` not zero and `m` at least one, likewise.
fn divided(s: u128, m: u32) -> Option<Exact> {
    // s / 10^m is s × 2^t / 5^m times 2^(−t−m), t chosen so the quotient has
    // 127 or 128 bits.
    let mut five = Big::from_u128(1);
    if !five.mul_pow5(m) {
        return None;
    }
    let t = POINT + 1 + five.bit_len() - (128 - s.leading_zeros());
    let mut rest = Big::from_u128(s);
    if !rest.shl(t) {
        return None;
    }
    let mut sig = (0..128u32).rev().fold(0u128, |sig, bit| {
        sig | u128::from(rest.sub_shifted(&five, bit)) << bit
    });
    let mut sticky = !rest.is_zero();
    let mut scale = step(t) + step(m);
    if sig >> (POINT + 1) != 0 {
        sticky |= sig & 1 != 0;
        sig >>= 1;
        scale -= 1;
    }
    Some(Exact {
        negative: false,
        exp: step(POINT) - scale,
        sig,
        sticky,
    })
}

/// A count of places as an exponent step; every count here is far inside
/// `i32`'s range.
fn step(n: u32) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// The leading digits of an expansion as they are produced, most significant
/// first, and what follows them.
struct Sink {
    /// Digits kept.
    wanted: u32,
    /// The first `wanted` significant digits, as an integer.
    kept: u128,
    /// Significant digits seen, at most one past `wanted`.
    taken: u32,
    /// The digit after the kept ones.
    next: u8,
    /// Whether any digit after `next` is not zero.
    sticky: bool,
    /// The place the next digit stands at.
    place: i32,
    /// The place of the first significant digit.
    first: i32,
}

impl Sink {
    const fn new(wanted: u32) -> Self {
        Self {
            wanted,
            kept: 0,
            taken: 0,
            next: 0,
            sticky: false,
            place: 0,
            first: 0,
        }
    }

    const fn full(&self) -> bool {
        self.taken > self.wanted
    }

    fn push(&mut self, digit: u8) {
        if self.taken == 0 {
            if digit == 0 {
                self.place -= 1;
                return;
            }
            self.first = self.place;
        }
        match self.taken.cmp(&self.wanted) {
            Ordering::Less => {
                self.kept = self.kept * 10 + u128::from(digit);
                self.taken += 1;
            }
            Ordering::Equal => {
                self.next = digit;
                self.taken += 1;
            }
            Ordering::Greater => self.sticky |= digit != 0,
        }
        self.place -= 1;
    }

    /// The `CHUNK_DIGITS` digits of `chunk`, leading zeros and all.
    fn push_chunk(&mut self, chunk: u32) {
        let mut scale = CHUNK / 10;
        while scale > 0 {
            self.push(digit(chunk / scale % 10));
            scale /= 10;
        }
    }

    /// The kept digits rounded in `mode`, and whether that was inexact.
    fn rounded(self, negative: bool, mode: Mode) -> (Decimal, bool) {
        // An expansion that ended early has zeros for its missing digits.
        let short = self.wanted - self.taken.min(self.wanted);
        let kept = self.kept * pow10(short);
        let inexact = self.next != 0 || self.sticky;
        let up = match mode {
            Mode::Nearest => self.next > 5 || (self.next == 5 && (self.sticky || kept % 2 == 1)),
            Mode::NearestAway => self.next >= 5,
            mode => inexact && mode.away(negative),
        };
        let mut significand = kept + u128::from(up);
        let mut first = self.first;
        if significand == pow10(self.wanted) {
            significand = pow10(self.wanted - 1);
            first += 1;
        }
        let decimal = Decimal {
            negative,
            significand,
            exponent: first - step(self.wanted - 1),
        };
        (decimal, inexact)
    }
}

/// A decimal digit's value as a byte.
fn digit(n: u32) -> u8 {
    u8::try_from(n % 10).unwrap_or(0)
}

/// The digits of `sig × 2^shift` into `sink`.
fn whole(sink: &mut Sink, sig: u64, shift: u32) -> Option<()> {
    // Chunks come out least significant first; only the last few can hold
    // the digits kept, so the rest only say whether they were zero.
    const RING: usize = 1 + MAX_DIGITS.div_ceil(CHUNK_DIGITS) as usize;
    let mut number = Big::from_u128(u128::from(sig));
    if !number.shl(shift) {
        return None;
    }
    let mut ring = [0u32; RING];
    let mut count = 0usize;
    let mut dropped = false;
    while !number.is_zero() {
        let slot = &mut ring[count % RING];
        dropped |= count >= RING && *slot != 0;
        *slot = number.div(CHUNK);
        count += 1;
    }
    let chunks = i32::try_from(count).ok()?;
    sink.place = chunks * step(CHUNK_DIGITS) - 1;
    for n in (count.saturating_sub(RING)..count).rev() {
        sink.push_chunk(ring[n % RING]);
    }
    sink.sticky |= dropped;
    Some(())
}

/// The digits of `sig × 2^−shift`, `shift` at least one, into `sink`.
fn split(sink: &mut Sink, sig: u64, shift: u32) -> Option<()> {
    let integer = sig.checked_shr(shift).unwrap_or(0);
    // The units place is the last of the twenty digits a u64 can have.
    sink.place = 19;
    let mut scale = 10_000_000_000_000_000_000u64;
    while scale > 0 {
        sink.push(u8::try_from(integer / scale % 10).unwrap_or(0));
        scale /= 10;
    }
    let fraction = if shift >= 64 {
        sig
    } else {
        sig & ((1 << shift) - 1)
    };
    // The fraction over 2^shift, placed at the top of whole words so each
    // multiplication by a chunk carries the chunk's digits out of them.
    let width = usize::try_from(shift.div_ceil(32)).ok()?;
    let mut number = Big::from_u128(u128::from(fraction));
    if !number.shl(shift.div_ceil(32) * 32 - shift) {
        return None;
    }
    while !sink.full() && !number.is_zero() {
        let chunk = number.mul_within(width, CHUNK);
        sink.push_chunk(chunk);
    }
    sink.sticky |= !number.is_zero();
    Some(())
}

#[cfg(test)]
#[path = "decimal_tests.rs"]
mod tests;
