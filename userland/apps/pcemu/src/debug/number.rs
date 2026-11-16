//! The 8087's registers as the debugger shows them: each value exactly, to
//! the nineteen significant digits its 64-bit significand carries, rounded to
//! nearest with ties to even.

use alloc::format;
use alloc::string::String;

use tairix_softfloat::{Decimal, Mode, Parts};

use crate::fpu::real::{class, Class};
use tairix_softfloat::Extended;

/// Significant digits shown.
const SHOWN: u32 = 19;

/// `value` in decimal: `d.dddddddddddddddddd` and a four-digit exponent, a
/// sign before a negative one; a zero, an infinity and a NaN by name.
#[must_use]
pub fn extended(value: Extended) -> String {
    let sign = if value.negative() { "-" } else { "" };
    match class(value) {
        Class::Zero | Class::PseudoZero => format!("{sign}0"),
        Class::Infinity => format!("{sign}Infinity"),
        Class::Nan => format!("{sign}NaN"),
        Class::Normal | Class::Unnormal | Class::Denormal => {
            // Every finite temporary real lies in the conversion's range; a
            // value that somehow did not is shown as its bits.
            Decimal::of(Parts::of(value), SHOWN, Mode::Nearest).map_or_else(
                || format!("{:04X}:{:016X}", value.sign_exp, value.sig),
                |(decimal, _)| scientific(sign, decimal),
            )
        }
    }
}

/// `decimal`, of `SHOWN` digits, as `d.ddd…E±pppp`.
fn scientific(sign: &str, decimal: Decimal) -> String {
    let after = SHOWN - 1;
    let scale = 10u128.pow(after);
    let (first, rest) = (decimal.significand / scale, decimal.significand % scale);
    let place = decimal.exponent + i32::try_from(after).unwrap_or(0);
    let mark = if place < 0 { '-' } else { '+' };
    let width = after as usize;
    format!(
        "{sign}{first}.{rest:0width$}E{mark}{:04}",
        place.unsigned_abs()
    )
}

#[cfg(test)]
#[path = "number_tests.rs"]
mod tests;
