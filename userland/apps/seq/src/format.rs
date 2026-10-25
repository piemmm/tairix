//! The `-f` format and the default formats.
//!
//! A format is literal text around one floating-point directive: flags from
//! `-+ #0'`, an optional width and precision, an optional `L`, and a
//! conversion from `efgaEFGA`, with `%%` a literal `%` on either side. Its
//! refusals are GNU `seq` 9.4's, word for word. Rendering is the shared
//! C-locale engine (`tairix_util::cfloat`, which `printf` renders through
//! too).

use alloc::string::String;
use core::fmt::{self, Write};

use tairix_util::cfloat::{FloatConversion, FloatDirective};

use crate::error::SeqError;

/// A validated format: literal prefix, one directive, literal suffix, the
/// literals' `%%` already read as `%`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Format {
    prefix: String,
    directive: FloatDirective,
    suffix: String,
}

impl Format {
    /// Validate the `-f` format `text`.
    ///
    /// # Errors
    ///
    /// GNU's four refusals: no directive, a directive cut off by the end, a
    /// conversion outside `efgaEFGA`, or a second directive.
    pub fn parse(text: &str) -> Result<Self, SeqError> {
        let refused = |make: fn(String) -> SeqError| make(String::from(text));
        let mut rest = text.chars().peekable();
        let mut prefix = String::new();
        loop {
            match rest.next() {
                None => return Err(refused(SeqError::FormatNoDirective)),
                Some('%') if rest.next_if_eq(&'%').is_some() => prefix.push('%'),
                Some('%') => break,
                Some(literal) => prefix.push(literal),
            }
        }
        let mut directive = FloatDirective::plain(FloatConversion::Fixed);
        while let Some(flag) = rest.next_if(|c| "-+ #0'".contains(*c)) {
            match flag {
                '-' => directive.left = true,
                '+' => directive.plus = true,
                ' ' => directive.space = true,
                '#' => directive.alternate = true,
                '0' => directive.zero = true,
                // Digit grouping, which the C locale has none of.
                _ => {}
            }
        }
        directive.width = number(&mut rest);
        if rest.next_if_eq(&'.').is_some() {
            directive.precision = Some(number(&mut rest).unwrap_or(0));
        }
        rest.next_if_eq(&'L');
        let conversion = rest
            .next()
            .ok_or_else(|| refused(SeqError::FormatEndsInPercent))?;
        (directive.conversion, directive.uppercase) = match conversion {
            'f' | 'F' => (FloatConversion::Fixed, conversion == 'F'),
            'e' | 'E' => (FloatConversion::Scientific, conversion == 'E'),
            'g' | 'G' => (FloatConversion::Shortest, conversion == 'G'),
            'a' | 'A' => (FloatConversion::Hex, conversion == 'A'),
            other => {
                return Err(SeqError::FormatUnknownDirective(String::from(text), other));
            }
        };
        let mut suffix = String::new();
        while let Some(literal) = rest.next() {
            if literal == '%' && rest.next_if_eq(&'%').is_none() {
                return Err(refused(SeqError::FormatTooManyDirectives));
            }
            suffix.push(literal);
        }
        Ok(Self {
            prefix,
            directive,
            suffix,
        })
    }

    /// `%g`, the format of a sequence no fixed point shows.
    #[must_use]
    pub fn shortest() -> Self {
        Self::bare(FloatDirective::plain(FloatConversion::Shortest))
    }

    /// `%.PRECISIONf`, zero-padded to `width` when it is non-zero.
    #[must_use]
    pub fn fixed(precision: usize, width: usize) -> Self {
        let mut directive = FloatDirective::plain(FloatConversion::Fixed);
        directive.precision = Some(precision);
        directive.zero = width > 0;
        directive.width = (width > 0).then_some(width);
        Self::bare(directive)
    }

    fn bare(directive: FloatDirective) -> Self {
        Self {
            prefix: String::new(),
            directive,
            suffix: String::new(),
        }
    }

    /// Write `x` as the format renders it to `out`, a field of any size in
    /// bounded memory.
    ///
    /// # Errors
    ///
    /// `out`'s own refusal.
    pub fn render_into<W: Write + ?Sized>(&self, x: f64, out: &mut W) -> fmt::Result {
        out.write_str(&self.prefix)?;
        self.directive.render_into(x, out)?;
        out.write_str(&self.suffix)
    }

    /// The value `x` reads back as once the format's directive writes it,
    /// or `None` for a NaN.
    #[must_use]
    pub fn rounded(&self, x: f64) -> Option<f64> {
        self.directive.rounded(x)
    }
}

/// A run of decimal digits, if one is next; a run too long for `usize` is
/// held at its bound.
fn number(rest: &mut core::iter::Peekable<core::str::Chars<'_>>) -> Option<usize> {
    let mut value = None;
    while let Some(digit) = rest
        .next_if(char::is_ascii_digit)
        .and_then(|c| c.to_digit(10))
    {
        value = Some(
            value
                .unwrap_or(0usize)
                .saturating_mul(10)
                .saturating_add(usize::try_from(digit).unwrap_or(0)),
        );
    }
    value
}

#[cfg(test)]
mod tests {
    use super::Format;
    use crate::error::SeqError;
    use alloc::string::String;

    fn render(format: &str, x: f64) -> String {
        let mut out = String::new();
        Format::parse(format)
            .expect("a valid format")
            .render_into(x, &mut out)
            .expect("a string takes every write");
        out
    }

    fn refusal(format: &str) -> SeqError {
        Format::parse(format).expect_err("a refused format")
    }

    #[test]
    fn the_refusals_are_gnu_s() {
        let text = String::from;
        for format in ["", "abc", "%%", "a%%b"] {
            assert_eq!(
                refusal(format),
                SeqError::FormatNoDirective(text(format)),
                "{format}"
            );
        }
        for format in ["%", "x%", "%%%", "%5", "%.3", "%L", "%-0"] {
            assert_eq!(
                refusal(format),
                SeqError::FormatEndsInPercent(text(format)),
                "{format}"
            );
        }
        for (format, conversion) in [
            ("%d", 'd'),
            ("%lf", 'l'),
            ("%hf", 'h'),
            ("%LL", 'L'),
            ("%Lq", 'q'),
            ("%*g", '*'),
            ("%1$g", '$'),
            ("%.*g", '*'),
        ] {
            assert_eq!(
                refusal(format),
                SeqError::FormatUnknownDirective(text(format), conversion),
                "{format}"
            );
        }
        for format in ["%g%", "%g %g", "%Lg%", "%g%x"] {
            assert_eq!(
                refusal(format),
                SeqError::FormatTooManyDirectives(text(format)),
                "{format}"
            );
        }
    }

    #[test]
    fn literals_wrap_the_number_and_double_percents_are_one() {
        assert_eq!(render("a%%b%.0fc%%", 5.0), "a%b5c%");
        assert_eq!(render("x%%y%+07.2fz%%", 1.5), "x%y+001.50z%");
        assert_eq!(render("%'g", 3.0), "3");
        assert_eq!(render("%5.2Lf", 3.0), " 3.00");
        assert_eq!(render("%.f", 2.0), "2", "an empty precision is zero");
        assert_eq!(render("%-+ #05.1f|", 1.0), "+1.0 |");
    }

    #[test]
    fn each_conversion_renders_through_the_shared_engine() {
        assert_eq!(render("%.2e", 12345.0), "1.23e+04");
        assert_eq!(render("%G", 1e-5), "1E-05");
        assert_eq!(render("%F", 1.0), "1.000000");
        assert_eq!(render("%a", 3.0), "0x1.8p+1");
    }

    #[test]
    fn the_default_formats_render() {
        let fixed = |precision, width, x| {
            let mut out = String::new();
            Format::fixed(precision, width)
                .render_into(x, &mut out)
                .expect("a string takes every write");
            out
        };
        assert_eq!(fixed(2, 0, 0.5), "0.50");
        assert_eq!(fixed(1, 5, -0.5), "-00.5");
        assert_eq!(fixed(0, 0, -0.0), "-0");
        let mut out = String::new();
        Format::shortest()
            .render_into(10_000_000.0, &mut out)
            .expect("a string takes every write");
        assert_eq!(out, "1e+07");
    }
}
