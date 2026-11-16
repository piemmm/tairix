//! RISC OS's numbers: the unsigned syntax `OS_ReadUnsigned` reads — decimal,
//! `&` and hex, or `base_digits` in any base from 2 to 36 — and the signed
//! decimal every integer is written in.

use alloc::vec::Vec;

use tairix_util::fallible;
use tairix_util::fmt::format_i32;

use crate::Error;

/// How [`read_unsigned`] reads a number: `OS_ReadUnsigned`'s flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reading {
    /// The base of digits with no prefix; one outside 2 to 36 reads as 10.
    pub base: u32,
    /// The largest value accepted.
    pub limit: u64,
    /// Whether 64 bits are read rather than 32.
    pub wide: bool,
    /// Whether the number must end at a space or a control character.
    pub terminated: bool,
}

impl Reading {
    /// Any 32-bit number in `base`, ending anywhere: how GSTrans, the
    /// expression evaluator and a string's conversion to an integer read one.
    #[must_use]
    pub const fn plain(base: u32) -> Self {
        Self {
            base,
            limit: u32::MAX as u64,
            wide: false,
            terminated: false,
        }
    }
}

/// The value of the digit `byte` stands for in `base`, where it stands for
/// one.
fn digit(byte: u8, base: u32) -> Option<u32> {
    let value = match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'Z' => byte - b'A' + 10,
        b'a'..=b'z' => byte - b'a' + 10,
        _ => return None,
    };
    let value = u32::from(value);
    (value < base).then_some(value)
}

/// The digits of `base` at `at` in `text`, no larger than `most`, and where
/// they end.
fn digits(text: &[u8], at: usize, base: u32, most: u64) -> Result<(u64, usize), Error> {
    let mut value = 0u64;
    let mut end = at;
    while let Some(d) = text.get(end).and_then(|&b| digit(b, base)) {
        value = value
            .checked_mul(u64::from(base))
            .and_then(|v| v.checked_add(u64::from(d)))
            .filter(|&v| v <= most)
            .ok_or(Error::NumberTooBig)?;
        end += 1;
    }
    if end == at {
        return Err(Error::BadNumber);
    }
    Ok((value, end))
}

/// The number `text` starts with, leading spaces skipped, and the bytes it
/// took, those spaces counted.
///
/// A `&` reads hex. Otherwise a decimal number followed by `_` is a base for
/// the digits after it, and anything else is read in `how.base`.
pub fn read_unsigned(text: &[u8], how: Reading) -> Result<(u64, usize), Error> {
    let base = if (2..=36).contains(&how.base) {
        how.base
    } else {
        10
    };
    let most = if how.wide {
        u64::MAX
    } else {
        u64::from(u32::MAX)
    };
    let start = text.iter().take_while(|&&b| b == b' ').count();
    let (value, end) = if text.get(start) == Some(&b'&') {
        digits(text, start + 1, 16, most)?
    } else {
        // The base before a `_` is read as a 32-bit decimal number.
        match digits(text, start, 10, u64::from(u32::MAX)) {
            Ok((radix, at)) if text.get(at) == Some(&b'_') => {
                let radix = u32::try_from(radix)
                    .ok()
                    .filter(|r| (2..=36).contains(r))
                    .ok_or(Error::BadBase)?;
                digits(text, at + 1, radix, most)?
            }
            _ => digits(text, start, base, most)?,
        }
    };
    if how.terminated && text.get(end).is_some_and(|&b| b > b' ') {
        return Err(Error::BadNumber);
    }
    if value > how.limit {
        return Err(Error::NumberTooBig);
    }
    Ok((value, end))
}

/// `n` in signed decimal, as RISC OS writes an integer: `-` before a negative
/// one, no leading zeros, `0` for zero.
pub(crate) fn decimal(n: i32) -> Result<Vec<u8>, Error> {
    let mut text = [0; 12];
    fallible::copied(format_i32(n, &mut text).as_bytes()).ok_or(Error::SystemHeapFull)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &[u8]) -> Result<(u64, usize), Error> {
        read_unsigned(text, Reading::plain(10))
    }

    #[test]
    fn decimal_hex_and_based_numbers_are_read() {
        assert_eq!(read(b"  42 rest"), Ok((42, 4)));
        assert_eq!(read(b"&fF"), Ok((255, 3)));
        assert_eq!(read(b"2_1100"), Ok((12, 6)));
        assert_eq!(read(b"36_zz"), Ok((1295, 5)));
        assert_eq!(
            read(b"16_FFAND1"),
            Ok((0xFFA, 6)),
            "a digit ends the number"
        );
        assert_eq!(read(b"4294967295"), Ok((u64::from(u32::MAX), 10)));
        assert_eq!(read(b"12d3"), Ok((12, 2)));
    }

    #[test]
    fn a_missing_digit_a_bad_base_and_too_big_a_number_are_refused() {
        assert_eq!(read(b"&"), Err(Error::BadNumber));
        assert_eq!(read(b"2_"), Err(Error::BadNumber));
        assert_eq!(read(b"8_9"), Err(Error::BadNumber));
        assert_eq!(read(b""), Err(Error::BadNumber));
        assert_eq!(read(b"1_0"), Err(Error::BadBase));
        assert_eq!(read(b"37_1"), Err(Error::BadBase));
        assert_eq!(read(b"4294967296"), Err(Error::NumberTooBig));
        assert_eq!(read(b"99999999999_1"), Err(Error::NumberTooBig));
    }

    #[test]
    fn the_flags_bound_and_end_the_number_and_widen_it() {
        let byte = Reading {
            limit: 255,
            ..Reading::plain(10)
        };
        assert_eq!(read_unsigned(b"256", byte), Err(Error::NumberTooBig));
        let ended = Reading {
            terminated: true,
            ..Reading::plain(10)
        };
        assert_eq!(read_unsigned(b"12x", ended), Err(Error::BadNumber));
        assert_eq!(read_unsigned(b"12 x", ended), Ok((12, 2)));
        let wide = Reading {
            wide: true,
            limit: u64::MAX,
            ..Reading::plain(10)
        };
        assert_eq!(read_unsigned(b"5000000000", wide), Ok((5_000_000_000, 10)));
        assert_eq!(
            read_unsigned(b"FF", Reading::plain(16)),
            Ok((255, 2)),
            "no decimal digit: the given base"
        );
        assert_eq!(read_unsigned(b"1F", Reading::plain(16)), Ok((31, 2)));
        assert_eq!(read_unsigned(b"9", Reading::plain(99)), Ok((9, 1)));
    }

    #[test]
    fn integers_are_written_in_signed_decimal() {
        assert_eq!(decimal(0).as_deref(), Ok(&b"0"[..]));
        assert_eq!(decimal(-5).as_deref(), Ok(&b"-5"[..]));
        assert_eq!(decimal(i32::MIN).as_deref(), Ok(&b"-2147483648"[..]));
        assert_eq!(decimal(i32::MAX).as_deref(), Ok(&b"2147483647"[..]));
    }
}
