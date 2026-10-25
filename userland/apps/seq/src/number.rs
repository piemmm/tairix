//! Operands: the value an operand spells, and the layout its spelling gives
//! the default format.
//!
//! The value is C's `strtod` reading (`tairix_util::cnum`), which must take
//! the whole operand. The layout is what GNU `seq` 9.4 sizes its default
//! output by, as running it shows: the digits after the point an operand
//! shows, and, for `-w`, the width of the integer part it shows.

use tairix_util::cnum::{c_isspace, scan_double};

use crate::error::SeqError;

/// How an operand's spelling lays out the numbers printed beside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    /// Fixed point, as decimal spellings are.
    Fixed {
        /// The integer part's width: a `-` and leading zeros count, a `+`
        /// does not, an empty one shows as `0`, and a positive exponent
        /// moves its digits in. A hexadecimal integer shows only its sign.
        integer: usize,
        /// The digits after the point, less a positive exponent's shift
        /// and plus a negative one's.
        precision: usize,
    },
    /// Shown by no fixed point — a hexadecimal fraction or binary exponent,
    /// or an infinity — so the sequence prints with `%g`.
    Free,
}

impl Layout {
    /// The width the operand shows at `precision` digits after the point.
    #[must_use]
    pub const fn width_at(self, precision: usize) -> usize {
        match self {
            Self::Fixed { integer, .. } if precision > 0 => integer + 1 + precision,
            Self::Fixed { integer, .. } => integer,
            Self::Free => 0,
        }
    }
}

/// One scanned operand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Operand {
    /// Its value.
    pub value: f64,
    /// The layout its spelling asks for.
    pub layout: Layout,
}

/// Scan the operand `text`.
///
/// # Errors
///
/// [`SeqError::InvalidNumber`] when `text` is not wholly a C number, and
/// [`SeqError::NotANumber`] when it is a NaN.
pub fn scan_operand(text: &str) -> Result<Operand, SeqError> {
    let value = match scan_double(text) {
        Some((value, taken)) if taken == text.len() => value,
        _ => return Err(SeqError::InvalidNumber(text.into())),
    };
    if value.is_nan() {
        return Err(SeqError::NotANumber(text.into()));
    }
    let layout = if value.is_finite() {
        layout_of(text.trim_start_matches(c_isspace))
    } else {
        Layout::Free
    };
    Ok(Operand { value, layout })
}

/// The layout of a finite number's spelling, its leading space gone.
fn layout_of(spelling: &str) -> Layout {
    let (sign, body) = match spelling.as_bytes().first() {
        Some(b'-') => (1, &spelling[1..]),
        Some(b'+') => (0, &spelling[1..]),
        _ => (0, spelling),
    };
    if body
        .get(..2)
        .is_some_and(|radix| radix.eq_ignore_ascii_case("0x"))
    {
        return if body.contains(['.', 'p', 'P']) {
            Layout::Free
        } else {
            Layout::Fixed {
                integer: sign,
                precision: 0,
            }
        };
    }
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent_of(exponent)),
        None => (body, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let shift = usize::try_from(exponent.unsigned_abs()).unwrap_or(usize::MAX);
    let (integer, precision) = if exponent >= 0 {
        (
            whole.len().max(1).saturating_add(shift),
            fraction.len().saturating_sub(shift),
        )
    } else {
        (whole.len().max(1), fraction.len().saturating_add(shift))
    };
    Layout::Fixed {
        integer: integer.saturating_add(sign),
        precision,
    }
}

/// A decimal exponent's value, held at a bound far past any a finite
/// `f64` spelling needs so a long run of digits cannot overflow it.
fn exponent_of(text: &str) -> i64 {
    const BOUND: i64 = 1 << 40;
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = digits.bytes().fold(0i64, |value, digit| {
        (value * 10 + i64::from(digit - b'0')).min(BOUND)
    });
    if negative {
        -magnitude
    } else {
        magnitude
    }
}

#[cfg(test)]
mod tests {
    use super::{scan_operand, Layout};
    use crate::error::SeqError;
    use alloc::string::String;

    fn fixed(text: &str) -> (usize, usize) {
        match scan_operand(text).expect("scans").layout {
            Layout::Fixed { integer, precision } => (integer, precision),
            Layout::Free => panic!("{text} is laid out in fixed point"),
        }
    }

    #[test]
    fn a_decimal_shows_its_digits_and_its_sign() {
        assert_eq!(fixed("12"), (2, 0));
        assert_eq!(fixed("007"), (3, 0));
        assert_eq!(fixed("-2"), (2, 0));
        assert_eq!(fixed("+2"), (1, 0));
        assert_eq!(fixed("  1"), (1, 0), "leading space shows nothing");
        assert_eq!(fixed("1.50"), (1, 2));
        assert_eq!(fixed("1."), (1, 0));
        assert_eq!(fixed(".5"), (1, 1), "an empty integer part shows as 0");
        assert_eq!(fixed("-.5"), (2, 1));
    }

    #[test]
    fn an_exponent_moves_digits_across_the_point() {
        assert_eq!(fixed("1e3"), (4, 0));
        assert_eq!(fixed("1.5e1"), (2, 0));
        assert_eq!(fixed("1.00e1"), (2, 1));
        assert_eq!(fixed("0.01e2"), (3, 0));
        assert_eq!(fixed("1e-3"), (1, 3));
        // GNU widens a negative exponent's fraction but keeps the integer
        // part: `seq -w 100e-2 1 10` pads to six.
        assert_eq!(fixed("100e-2"), (3, 2));
    }

    #[test]
    fn a_hexadecimal_integer_shows_only_its_sign() {
        assert_eq!(fixed("0x10"), (0, 0));
        assert_eq!(fixed("-0x10"), (1, 0));
    }

    #[test]
    fn what_no_fixed_point_shows_is_free() {
        for text in [
            "0x1.8p1",
            "0x1p-1",
            "0X1P-1",
            "0x.8",
            "inf",
            "-Infinity",
            "1e400",
        ] {
            assert_eq!(
                scan_operand(text).expect("scans").layout,
                Layout::Free,
                "{text}"
            );
        }
    }

    #[test]
    fn a_layout_widens_to_the_precision_shown() {
        let layout = Layout::Fixed {
            integer: 2,
            precision: 0,
        };
        assert_eq!(layout.width_at(0), 2);
        assert_eq!(layout.width_at(3), 6);
    }

    #[test]
    fn a_whole_c_number_is_required() {
        for text in [
            "", " ", "abc", "1x", "1 ", "--5", "1e", "0x", "nanx", "1_000",
        ] {
            assert_eq!(
                scan_operand(text),
                Err(SeqError::InvalidNumber(String::from(text))),
                "{text:?}"
            );
        }
        assert_eq!(
            scan_operand("nan"),
            Err(SeqError::NotANumber(String::from("nan")))
        );
        assert_eq!(scan_operand("0X1P3").map(|o| o.value), Ok(8.0));
    }
}
