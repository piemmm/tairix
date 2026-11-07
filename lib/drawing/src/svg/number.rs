//! Numbers as an SVG file writes them.

use core::fmt;

use tairix_svg::number::opacity_to_alpha;
use tairix_util::mathf::round;

/// A number spelled exactly: the shortest text that reads back as the same
/// value, so a saved drawing reopens as itself and nothing the writer decides
/// from a value can come out differently on the next save.
pub(crate) struct Number(pub(crate) f64);

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.0;
        if !value.is_finite() {
            return Err(fmt::Error);
        }
        if value == 0.0 {
            // Negative zero included.
            f.write_str("0")
        } else if tairix_util::mathf::fabs(value) < 1e-6 {
            // A plain spelling of a tiny value is mostly zeros.
            write!(f, "{value:e}")
        } else {
            write!(f, "{value}")
        }
    }
}

/// An 8-bit alpha as an opacity: the decimal of fewest places that reads back
/// as the same alpha.
pub(crate) fn alpha(alpha: u8) -> Number {
    let exact = f64::from(alpha) / 255.0;
    let mut scale = 10.0;
    while scale < 1e4 {
        let shortened = round(exact * scale) / scale;
        if opacity_to_alpha(shortened) == alpha {
            return Number(shortened);
        }
        scale *= 10.0;
    }
    Number(exact)
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::ToString;

    use super::{alpha, Number};

    #[test]
    fn numbers_read_back_exactly() {
        assert_eq!(Number(12.5).to_string(), "12.5");
        assert_eq!(Number(3.0).to_string(), "3");
        assert_eq!(Number(-0.0).to_string(), "0");
        assert_eq!(Number(-250.0).to_string(), "-250");
        assert_eq!(Number(1e9).to_string(), "1000000000");
        assert_eq!(Number(1e-300).to_string(), "1e-300");
        for value in [
            core::f64::consts::FRAC_1_SQRT_2,
            0.1 + 0.2,
            123.456_789_012_345_67,
            -4.5e-7,
            f64::MIN_POSITIVE,
        ] {
            let spelled = Number(value).to_string();
            let read = tairix_svg::number::parse_number(&spelled).expect("an SVG number");
            assert_eq!(read.to_bits(), value.to_bits(), "{spelled}");
        }
    }

    #[test]
    fn a_number_that_is_not_finite_is_not_written() {
        let mut out = alloc::string::String::new();
        assert!(core::fmt::write(&mut out, format_args!("{}", Number(f64::NAN))).is_err());
    }

    #[test]
    fn every_alpha_is_spelled_short_and_reads_back_as_itself() {
        assert_eq!(alpha(255).to_string(), "1");
        assert_eq!(alpha(0).to_string(), "0");
        assert_eq!(alpha(128).to_string(), "0.5");
        for value in 0..=u8::MAX {
            let spelled = format!("{}", alpha(value));
            assert!(spelled.len() <= 6, "{spelled}");
            let read = tairix_svg::number::parse_opacity(&spelled).expect("an opacity");
            assert_eq!(tairix_svg::number::opacity_to_alpha(read), value);
        }
    }
}
