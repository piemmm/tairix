//! The 8087's registers as the debugger shows them: each value exactly, to
//! the nineteen significant digits its 64-bit significand carries, rounded to
//! nearest with ties to even. A value is the integer significand times a
//! power of two, so its decimal expansion is finite and is worked out whole,
//! nine digits at a time, rather than approximated.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::fpu::real::{Class, Real, BIAS};

/// Significant digits shown.
const SHOWN: usize = 19;
/// One limb of decimal digits.
const CHUNK: u32 = 1_000_000_000;
const CHUNK_DIGITS: i32 = 9;

/// `value` in decimal: `d.dddddddddddddddddd` and a four-digit exponent, a
/// sign before a negative one; a zero, an infinity and a NaN by name.
#[must_use]
pub fn extended(value: Real) -> String {
    let sign = if value.negative() { "-" } else { "" };
    match value.class() {
        Class::Zero | Class::PseudoZero => format!("{sign}0"),
        Class::Infinity => format!("{sign}Infinity"),
        Class::Nan => format!("{sign}NaN"),
        Class::Normal | Class::Unnormal | Class::Denormal => {
            let exponent = i32::from(value.exponent()).max(1) - BIAS - 63;
            let (digits, place) = rounded(&significant(value.sig, exponent));
            let mut shown = String::with_capacity(SHOWN + 1);
            for (n, digit) in digits.iter().enumerate() {
                if n == 1 {
                    shown.push('.');
                }
                shown.push(char::from(b'0' + digit));
            }
            let mark = if place < 0 { '-' } else { '+' };
            format!("{sign}{shown}E{mark}{:04}", place.unsigned_abs())
        }
    }
}

/// The leading digits of a decimal expansion, one more than shown, and
/// whether any digit after them is not zero.
#[derive(Debug)]
struct Sink {
    digits: [u8; SHOWN + 1],
    taken: usize,
    /// The place the next digit stands at, ten to that power.
    place: i32,
    /// The place of the first significant digit.
    first: i32,
    sticky: bool,
}

impl Sink {
    const fn new(place: i32) -> Self {
        Self {
            digits: [0; SHOWN + 1],
            taken: 0,
            place,
            first: 0,
            sticky: false,
        }
    }

    const fn full(&self) -> bool {
        self.taken > SHOWN
    }

    fn push(&mut self, digit: u8) {
        if self.taken == 0 && digit == 0 {
            self.place -= 1;
            return;
        }
        if self.taken == 0 {
            self.first = self.place;
        }
        if let Some(slot) = self.digits.get_mut(self.taken) {
            *slot = digit;
            self.taken += 1;
        } else if digit != 0 {
            self.sticky = true;
        }
        self.place -= 1;
    }

    /// The nine digits of `chunk`, leading zeros and all.
    fn push_chunk(&mut self, chunk: u32) {
        let mut scale = CHUNK / 10;
        while scale > 0 {
            self.push(u8::try_from(chunk / scale % 10).unwrap_or(0));
            scale /= 10;
        }
    }
}

/// The leading digits of `sig` times two to `exponent`, `sig` not zero.
fn significant(sig: u64, exponent: i32) -> Sink {
    if exponent >= 0 {
        return whole(sig, exponent.unsigned_abs());
    }
    let shift = exponent.unsigned_abs();
    let integer = sig.checked_shr(shift).unwrap_or(0);
    let mut sink = Sink::new(19);
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
    fractional(&mut sink, fraction, shift);
    sink
}

/// The digits of `fraction` over two to `bits`, below the units place,
/// into `sink`, which they follow.
fn fractional(sink: &mut Sink, fraction: u64, bits: u32) {
    // Over a whole number of limbs, the top limb then holds each chunk as
    // it is multiplied out.
    let limbs = bits.div_ceil(32) as usize;
    let rise = u32::try_from(limbs).unwrap_or(0) * 32 - bits;
    let mut number = vec![0u32; limbs + 1];
    let low = u128::from(fraction) << rise;
    for (n, limb) in number.iter_mut().enumerate().take(limbs) {
        if let Some(part) = low.checked_shr(u32::try_from(n * 32).unwrap_or(u32::MAX)) {
            *limb = u32::try_from(part & 0xFFFF_FFFF).unwrap_or(0);
        }
    }
    while !sink.full() && number.iter().any(|&limb| limb != 0) {
        let mut carry = 0u64;
        for limb in &mut number {
            let product = u64::from(*limb) * u64::from(CHUNK) + carry;
            *limb = u32::try_from(product & 0xFFFF_FFFF).unwrap_or(0);
            carry = product >> 32;
        }
        let chunk = number[limbs];
        number[limbs] = 0;
        sink.push_chunk(chunk);
    }
    sink.sticky |= number.iter().any(|&limb| limb != 0);
}

/// The digits of `sig` times two to `exponent`, a whole number.
fn whole(sig: u64, exponent: u32) -> Sink {
    let words = exponent / 32;
    let rise = exponent % 32;
    let mut number: Vec<u32> = vec![0; words as usize];
    let wide = u128::from(sig) << rise;
    number.extend([0, 32, 64].map(|at| u32::try_from((wide >> at) & 0xFFFF_FFFF).unwrap_or(0)));
    let mut chunks = Vec::new();
    while number.iter().any(|&limb| limb != 0) {
        let mut rest = 0u64;
        for limb in number.iter_mut().rev() {
            let at = rest << 32 | u64::from(*limb);
            *limb = u32::try_from(at / u64::from(CHUNK)).unwrap_or(0);
            rest = at % u64::from(CHUNK);
        }
        chunks.push(u32::try_from(rest).unwrap_or(0));
        while number.last() == Some(&0) {
            number.pop();
        }
    }
    let count = i32::try_from(chunks.len()).unwrap_or(i32::MAX);
    let mut sink = Sink::new(count * CHUNK_DIGITS - 1);
    for &chunk in chunks.iter().rev() {
        sink.push_chunk(chunk);
    }
    sink
}

/// The shown digits, rounded to nearest with ties to even, and the place of
/// the first.
fn rounded(sink: &Sink) -> ([u8; SHOWN], i32) {
    let mut digits = [0; SHOWN];
    digits.copy_from_slice(&sink.digits[..SHOWN]);
    let next = sink.digits[SHOWN];
    let odd = digits[SHOWN - 1] % 2 == 1;
    let mut place = sink.first;
    if next > 5 || (next == 5 && (sink.sticky || odd)) {
        let mut at = SHOWN;
        loop {
            at -= 1;
            if digits[at] < 9 {
                digits[at] += 1;
                break;
            }
            digits[at] = 0;
            if at == 0 {
                digits[0] = 1;
                place += 1;
                break;
            }
        }
    }
    (digits, place)
}

#[cfg(test)]
#[path = "number_tests.rs"]
mod tests;
