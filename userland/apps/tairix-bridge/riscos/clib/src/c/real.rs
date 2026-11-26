//! `strtod` and `atof` as RISC OS 5.30's library reads a number: C99's forms
//! — decimal, hexadecimal, infinity and NaN — rounded correctly, a subnormal
//! or out-of-range answer a range error; but an exponent marker with no
//! digits after it leaves the whole number unread, and the written exponent
//! is summed as an `int`, wrapping. `scanf` reads its numbers through the
//! same reading.

use ::alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::decimal::{exact_digits, Beyond, KEPT_DIGITS};
use tairix_softfloat::{Binary, Exact};

use super::errno::{self, ERANGE};
use super::fpa::{answer, single_bits, widened, DOUBLE};
use super::math::HUGE_VAL;
use super::statics::at;
use super::stdlib;
use crate::kernel::context;
use crate::kernel::statics::Statics;

/// The NaN `"nan"` reads as, genuine's.
const NAN: u64 = 0x7FF8_0000_2000_0000;
/// Hexadecimal digits a significand keeps whole, with room for a carry.
const HEX_KEPT: u32 = 31;
/// A binary exponent past which any significand is out of a double's range.
const BINARY_REACH: i64 = 4096;
/// The place an `Exact`'s integer bit stands at.
const INTEGER_PLACE: i64 = 126;

/// Text a number is read from: a byte at each place from its start, nought
/// past its end.
pub(crate) trait Text {
    fn at(&mut self, os: &mut Os<'_>, place: u32) -> Result<u8, Fail>;
}

/// The client's text at an address.
struct Guest(u32);

impl Text for Guest {
    fn at(&mut self, os: &mut Os<'_>, place: u32) -> Result<u8, Fail> {
        os.as_caller().read_byte(self.0.wrapping_add(place))
    }
}

impl Text for &[u8] {
    fn at(&mut self, _os: &mut Os<'_>, place: u32) -> Result<u8, Fail> {
        Ok(usize::try_from(place)
            .ok()
            .and_then(|n| self.get(n))
            .copied()
            .unwrap_or(0))
    }
}

/// A number read: its bits, whether it is a range error, and the bytes of
/// text it took.
pub(crate) struct Number {
    pub(crate) bits: u64,
    pub(crate) range_error: bool,
    pub(crate) length: u32,
}

/// A number's digits: where they run, with at most one decimal point among
/// them, how many follow the point, the exponent written after them, and
/// where the number's text ends.
struct Digits {
    start: u32,
    stop: u32,
    after_point: u32,
    exponent: i32,
    end: u32,
}

/// What a number's digits come to.
enum Scanned {
    /// There are none.
    NoDigits,
    /// They are followed by an exponent marker with none.
    BadExponent,
    Digits(Digits),
}

/// The value of `b` as a digit in `radix`.
fn digit(b: u8, radix: u32) -> Option<u32> {
    stdlib::digit(b).filter(|&d| d < radix)
}

/// The locale's decimal point.
pub(crate) fn decimal_point(os: &mut Os<'_>, statics: Statics) -> Result<u8, Fail> {
    match statics.read(os, at::LCONV)? {
        0 => Ok(b'.'),
        point => os.as_caller().read_byte(point),
    }
}

/// Whether the text from `place` starts with `word`, letters in either case.
fn starts_with(
    os: &mut Os<'_>,
    text: &mut impl Text,
    place: u32,
    word: &[u8],
) -> Result<bool, Fail> {
    for (n, &letter) in (0u32..).zip(word) {
        if !text
            .at(os, place.wrapping_add(n))?
            .eq_ignore_ascii_case(&letter)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Past `nan`'s optional `(n-char-sequence)` at `place`.
fn past_nan_payload(os: &mut Os<'_>, text: &mut impl Text, place: u32) -> Result<u32, Fail> {
    if text.at(os, place)? != b'(' {
        return Ok(place);
    }
    let mut end = place.wrapping_add(1);
    loop {
        match text.at(os, end)? {
            b')' => return Ok(end.wrapping_add(1)),
            b if b.is_ascii_alphanumeric() || b == b'_' => end = end.wrapping_add(1),
            _ => return Ok(place),
        }
    }
}

/// The digits from `place` in `radix`, and the exponent after `marker` past
/// them.
fn digits(
    os: &mut Os<'_>,
    text: &mut impl Text,
    place: u32,
    radix: u32,
    point: u8,
    marker: u8,
) -> Result<Scanned, Fail> {
    let (mut stop, mut count, mut after_point, mut seen_point) = (place, 0u32, 0u32, false);
    loop {
        let b = text.at(os, stop)?;
        if digit(b, radix).is_some() {
            count = count.saturating_add(1);
            after_point = after_point.saturating_add(u32::from(seen_point));
        } else if b == point && !seen_point {
            seen_point = true;
        } else {
            break;
        }
        stop = stop.wrapping_add(1);
    }
    if count == 0 {
        return Ok(Scanned::NoDigits);
    }
    let (mut exponent, mut end) = (0i32, stop);
    if text.at(os, end)?.eq_ignore_ascii_case(&marker) {
        end = end.wrapping_add(1);
        let sign = text.at(os, end)?;
        if sign == b'+' || sign == b'-' {
            end = end.wrapping_add(1);
        }
        let first = end;
        while let Some(d) = digit(text.at(os, end)?, 10) {
            exponent = exponent.wrapping_mul(10).wrapping_add(d.cast_signed());
            end = end.wrapping_add(1);
        }
        if end == first {
            return Ok(Scanned::BadExponent);
        }
        if sign == b'-' {
            exponent = exponent.wrapping_neg();
        }
    }
    Ok(Scanned::Digits(Digits {
        start: place,
        stop,
        after_point,
        exponent,
        end,
    }))
}

/// A number's significant digits: the first `kept` of their values, how many
/// follow them, and whether any of those is not nought.
struct Significant {
    values: Vec<u8>,
    past: u32,
    sticky: bool,
}

/// The digits a long number's text is read between yields.
const DIGIT_RUN: u32 = 4096;

/// `digits`' text in `radix`, the point and the leading noughts left out:
/// its first `kept` significant digits, and only whether those past them
/// are nought, which is all a rounding reads of them.
fn significant(
    os: &mut Os<'_>,
    text: &mut impl Text,
    digits: &Digits,
    radix: u32,
    kept: u32,
) -> Result<Significant, Fail> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(kept as usize)
        .map_err(|_| os.no_memory())?;
    let (mut past, mut sticky) = (0u32, false);
    for place in digits.start..digits.stop {
        let Some(value) = digit(text.at(os, place)?, radix).and_then(|d| u8::try_from(d).ok())
        else {
            continue;
        };
        if values.len() < kept as usize {
            if !values.is_empty() || value != 0 {
                values.push(value);
            }
        } else {
            past = past.saturating_add(1);
            sticky |= value != 0;
        }
        if place.wrapping_sub(digits.start).is_multiple_of(DIGIT_RUN) {
            os.yield_point()?;
        }
    }
    Ok(Significant {
        values,
        past,
        sticky,
    })
}

/// `exact` rounded to a double: its bits, and whether that is a range error
/// — past the largest double, which is `HUGE_VAL`, or below the least
/// normal one.
fn rounded(exact: Result<Exact, Beyond>, negative: bool) -> (u64, bool) {
    let huge = HUGE_VAL.to_bits() | u64::from(negative) << 63;
    let zero = u64::from(negative) << 63;
    match exact {
        Ok(x) if x.is_zero() => (zero, false),
        Ok(x) => {
            let bits = DOUBLE.round(x).bits;
            let field = bits >> Binary::DOUBLE.fraction_bits() & Binary::DOUBLE.exp_mask();
            if field == Binary::DOUBLE.exp_mask() {
                (huge, true)
            } else {
                (bits, field == 0)
            }
        }
        Err(Beyond::Above) => (huge, true),
        Err(Beyond::Below) => (zero, true),
    }
}

/// A decimal number's value.
fn decimal(
    os: &mut Os<'_>,
    text: &mut impl Text,
    digits: &Digits,
    negative: bool,
) -> Result<(u64, bool), Fail> {
    let read = significant(os, text, digits, 10, KEPT_DIGITS)?;
    // The digits past those kept stand as one, nought or not, a place below
    // the last kept: all a rounding reads of them.
    let stand_in = (read.past > 0).then_some(u8::from(read.sticky));
    let exponent = i64::from(digits.exponent) - i64::from(digits.after_point)
        + i64::from(read.past.saturating_sub(1));
    let exponent =
        i32::try_from(exponent).unwrap_or(if exponent < 0 { i32::MIN } else { i32::MAX });
    let values = read.values.into_iter().chain(stand_in);
    Ok(rounded(exact_digits(negative, values, exponent), negative))
}

/// A hexadecimal number's value: its first significant digits whole, any
/// nonzero digit past them kept as a sticky bit.
fn hexadecimal(
    os: &mut Os<'_>,
    text: &mut impl Text,
    digits: &Digits,
    negative: bool,
) -> Result<(u64, bool), Fail> {
    let read = significant(os, text, digits, 16, HEX_KEPT)?;
    let sig = read
        .values
        .iter()
        .fold(0u128, |sig, &v| sig << 4 | u128::from(v));
    let past = i64::from(read.past);
    let exponent = (i64::from(digits.exponent) + 4 * (past - i64::from(digits.after_point)))
        .clamp(-BINARY_REACH, BINARY_REACH);
    let exact = Exact {
        negative,
        exp: i32::try_from(exponent + INTEGER_PLACE).unwrap_or(0),
        sig,
        sticky: read.sticky,
    };
    Ok(rounded(Ok(exact), negative))
}

/// The number at the start of `text` — an optional sign, then a number's
/// body, `point` its decimal point — none where there is none to read.
pub(crate) fn parse(
    os: &mut Os<'_>,
    text: &mut impl Text,
    point: u8,
) -> Result<Option<Number>, Fail> {
    let sign = text.at(os, 0)?;
    let negative = sign == b'-';
    let at = u32::from(sign == b'-' || sign == b'+');
    let sign_bit = u64::from(negative) << 63;
    let special = if starts_with(os, text, at, b"infinity")? {
        Some((f64::INFINITY.to_bits(), at + 8))
    } else if starts_with(os, text, at, b"inf")? {
        Some((f64::INFINITY.to_bits(), at + 3))
    } else if starts_with(os, text, at, b"nan")? {
        Some((NAN, past_nan_payload(os, text, at + 3)?))
    } else {
        None
    };
    if let Some((bits, length)) = special {
        return Ok(Some(Number {
            bits: bits | sign_bit,
            range_error: false,
            length,
        }));
    }
    let prefixed = text.at(os, at)? == b'0' && text.at(os, at + 1)?.eq_ignore_ascii_case(&b'x');
    if prefixed {
        // A prefix with no digit after it is a nought, the `x` left unread.
        match digits(os, text, at + 2, 16, point, b'p')? {
            Scanned::Digits(digits) => {
                let (bits, range_error) = hexadecimal(os, text, &digits, negative)?;
                return Ok(Some(Number {
                    bits,
                    range_error,
                    length: digits.end,
                }));
            }
            Scanned::BadExponent => return Ok(None),
            Scanned::NoDigits => {}
        }
    }
    let Scanned::Digits(digits) = digits(os, text, at, 10, point, b'e')? else {
        return Ok(None);
    };
    let (bits, range_error) = decimal(os, text, &digits, negative)?;
    Ok(Some(Number {
        bits,
        range_error,
        length: digits.end,
    }))
}

/// The number at `text` after the white space the locale's table marks, and
/// the address its text ends at; none where there is none to read.
fn read(os: &mut Os<'_>, statics: Statics, text: u32) -> Result<Option<(Number, u32)>, Fail> {
    let start = stdlib::past_space(os, statics, text)?;
    let point = decimal_point(os, statics)?;
    Ok(parse(os, &mut Guest(start), point)?.map(|number| {
        let end = start.wrapping_add(number.length);
        (number, end)
    }))
}

/// `strtod(nptr, endptr)`.
pub(super) fn strtod(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (bits, end) = match read(os, statics, regs.r[0])? {
        Some((number, end)) => {
            if number.range_error {
                errno::set(os, statics, ERANGE)?;
            }
            (number.bits, end)
        }
        None => (0, regs.r[0]),
    };
    stdlib::set_end(os, regs.r[1], end)?;
    answer(os, f64::from_bits(bits));
    Ok(())
}

/// `strtof(nptr, endptr)`: `strtod`'s number rounded to a float; a range
/// error too where the float overflows or is no normal one.
pub(crate) fn strtof(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (bits, end) = match read(os, statics, regs.r[0])? {
        Some((number, end)) => {
            let double = f64::from_bits(number.bits);
            let single = single_bits(double);
            let float = f32::from_bits(single);
            let narrowed = double.is_finite()
                && (float.is_infinite() || (double != 0.0 && !float.is_normal()));
            if number.range_error || narrowed {
                errno::set(os, statics, ERANGE)?;
            }
            (widened(single).to_bits(), end)
        }
        None => (0, regs.r[0]),
    };
    stdlib::set_end(os, regs.r[1], end)?;
    answer(os, f64::from_bits(bits));
    Ok(())
}

/// `nan(tagp)` and `nanf`: the NaN `"nan"` reads as, whatever the tag.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(crate) fn nan(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    answer(os, f64::from_bits(NAN));
    Ok(())
}

/// `atof(nptr)`: `strtod`'s answer, `errno` left as it is.
pub(super) fn atof(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let bits = read(os, statics, regs.r[0])?.map_or(0, |(number, _)| number.bits);
    answer(os, f64::from_bits(bits));
    Ok(())
}

#[cfg(test)]
mod tests;
