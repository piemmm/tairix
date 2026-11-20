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

/// The least unbiased exponent a finite value's top bit has: the FPA's least
/// denormal's, one below double-extended's, its denormals taking the normal
/// bias.
const LEAST_EXP: i32 = -BIAS - 63;

/// The greatest.
const MOST_EXP: i32 = EXP_MAX as i32 - BIAS;

/// The places a decimal value's first digit may stand at for the value to be
/// worked out. Past the last, the value is at least 10^4933, above
/// double-extended's largest finite value; before the first, it is below
/// 10^−4952, less than half the FPA's least denormal and so any format's.
const PLACES: core::ops::RangeInclusive<i64> = -4952..=4932;

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
            return Ok(zero(self.negative));
        }
        if let Some(x) = small(s, self.exponent) {
            return Ok(Exact {
                negative: self.negative,
                ..x
            });
        }
        scaled(
            self.negative,
            Big::from_u128(s),
            digits_of(s),
            self.exponent,
        )
    }
}

/// The value `digits` (each 0 to 9, the most significant first) times ten
/// to `exponent` read as [`Decimal::exact`] reads one, however many digits
/// there are: every one of them counts, so a value lying exactly on a tie
/// is read as one.
///
/// # Errors
///
/// [`Beyond`] for a value past double-extended's range, or with more
/// digits than conversion has room for, which no value in its range needs.
pub fn exact_digits(
    negative: bool,
    digits: impl IntoIterator<Item = u8>,
    exponent: i32,
) -> Result<Exact, Beyond> {
    let mut digits = digits.into_iter().skip_while(|&d| d == 0).peekable();
    // As many digits as a significand holds go to one, which most numbers
    // need no more than.
    let mut significand = 0u128;
    let mut count = 0u32;
    while count < MAX_DIGITS {
        let Some(digit) = digits.next() else {
            return Decimal {
                negative,
                significand,
                exponent,
            }
            .exact();
        };
        significand = significand * 10 + u128::from(digit.min(9));
        count += 1;
    }
    if digits.peek().is_none() {
        return Decimal {
            negative,
            significand,
            exponent,
        }
        .exact();
    }
    let mut big = Big::from_u128(significand);
    let (mut chunk, mut place) = (0, 1);
    for digit in digits {
        chunk = chunk * 10 + u32::from(digit.min(9));
        place *= 10;
        count = count.checked_add(1).ok_or(Beyond::Above)?;
        if place == CHUNK {
            if !big.mul(CHUNK) || !big.add(chunk) {
                return Err(Beyond::Above);
            }
            (chunk, place) = (0, 1);
        }
    }
    if place > 1 && (!big.mul(place) || !big.add(chunk)) {
        return Err(Beyond::Above);
    }
    scaled(negative, big, count, exponent)
}

/// `n × 10^exponent`, `n` not zero, exactly enough to round and positive,
/// where plain integers hold the work: a product that fits 128 bits, or a
/// quotient of a 64-bit `n` by a power of five that fits 64. `None` leaves
/// the value to [`scaled`], which gives the same answer for any.
fn small(n: u128, exponent: i32) -> Option<Exact> {
    if let Ok(k) = u32::try_from(exponent) {
        return Some(integral(n.checked_mul(10u128.checked_pow(k)?)?));
    }
    let m = exponent.unsigned_abs();
    fraction(u64::try_from(n).ok()?, 5u64.checked_pow(m)?, m)
}

/// The integer `v`, not zero.
fn integral(v: u128) -> Exact {
    let top = v.ilog2();
    let (sig, sticky) = match POINT.checked_sub(top) {
        Some(up) => (v << up, false),
        None => (v >> 1, v & 1 != 0),
    };
    Exact {
        negative: false,
        exp: step(top),
        sig,
        sticky,
    }
}

/// `n / (five × 2^m)`, `n` not zero and `five` five to the `m`.
fn fraction(n: u64, five: u64, m: u32) -> Option<Exact> {
    // n is taken to the top of a word, and a word more of places below it,
    // so the quotient has 64 bits at least; the remainder gives it the rest
    // a significand needs.
    let lead = n.leading_zeros();
    let divisor = u128::from(five);
    let dividend = u128::from(n << lead) << 64;
    let (mut quotient, mut rest) = (dividend / divisor, dividend % divisor);
    let more = POINT.saturating_sub(quotient.ilog2());
    if more > 0 {
        let extended = rest << more;
        quotient = (quotient << more) | (extended / divisor);
        rest = extended % divisor;
    }
    let top = quotient.ilog2();
    let (sig, sticky) = match POINT.checked_sub(top) {
        Some(up) => (quotient << up, rest != 0),
        None => (quotient >> 1, quotient & 1 != 0 || rest != 0),
    };
    let below = 64 + lead + m + more;
    Some(Exact {
        negative: false,
        exp: step(top).checked_sub(step(below))?,
        sig,
        sticky,
    })
}

/// A zero of sign `negative`.
const fn zero(negative: bool) -> Exact {
    Exact {
        negative,
        exp: 0,
        sig: 0,
        sticky: false,
    }
}

/// `significand`, not zero, of `count` digits, times ten to `exponent`.
fn scaled(negative: bool, significand: Big, count: u32, exponent: i32) -> Result<Exact, Beyond> {
    let first = i64::from(exponent) + i64::from(count - 1);
    if first > *PLACES.end() {
        return Err(Beyond::Above);
    }
    if first < *PLACES.start() {
        return Err(Beyond::Below);
    }
    let x = match u32::try_from(exponent) {
        Ok(k) => multiplied(significand, k).ok_or(Beyond::Above)?,
        Err(_) => divided(significand, exponent.unsigned_abs()).ok_or(Beyond::Below)?,
    };
    Ok(Exact { negative, ..x })
}

/// `big × 10^k`, `big` not zero, exactly enough to round, positive; `None`
/// where it passes what `Big` holds, which no value in `PLACES` does.
fn multiplied(mut big: Big, k: u32) -> Option<Exact> {
    // big × 10^k is big × 5^k, exactly, times 2^k.
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

/// `big / 10^m`, `big` not zero and `m` at least one, likewise.
fn divided(big: Big, m: u32) -> Option<Exact> {
    // big / 10^m is big × 2^t / (5^m × 2^u) times 2^(u−t−m), t or u chosen so
    // the quotient has 127 or 128 bits: u where big is the longer.
    let mut five = Big::from_u128(1);
    if !five.mul_pow5(m) {
        return None;
    }
    let (want, have) = (POINT + 1 + five.bit_len(), big.bit_len());
    let (t, u) = if have <= want {
        (want - have, 0)
    } else {
        (0, have - want)
    };
    let mut rest = big;
    if !rest.shl(t) || !five.shl(u) {
        return None;
    }
    let mut sig = (0..128u32).rev().fold(0u128, |sig, bit| {
        sig | u128::from(rest.sub_shifted(&five, bit)) << bit
    });
    let mut sticky = !rest.is_zero();
    let mut scale = step(t) + step(m) - step(u);
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
