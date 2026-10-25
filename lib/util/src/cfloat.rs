//! C-locale `printf(3)` floating-point rendering.
//!
//! TAIRiX is Rust-only, so the coreutils-compatible tools that promise
//! printf semantics (`seq -f`, `printf`'s `%e`/`%f`/`%g`/`%a`) cannot hand
//! their formats to a C library — they render through this one engine
//! instead, so C's rounding, flag, padding, and special-value rules exist
//! in exactly one place. The engine covers the four floating-point
//! conversions in upper and lower case with the five independent printf
//! flags, an optional minimum field width, and an optional precision,
//! exactly as C's `printf` renders them in the C locale.
//!
//! The values are IEEE 754 `f64` (`double`); a consumer whose GNU
//! counterpart computes in `long double` documents that divergence at its
//! own surface (`seq`, `printf`).
//!
//! Everything here is total and panic-free: every finite, infinite, and
//! NaN input renders, and an absurd width or precision saturates rather
//! than overflowing.

use alloc::string::String;
use core::fmt::{self, Write};

/// The conversion letter of a floating-point `%` directive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatConversion {
    /// `%e` / `%E` — scientific notation.
    Scientific,
    /// `%f` / `%F` — fixed-point notation.
    Fixed,
    /// `%g` / `%G` — the shorter of `%e`/`%f`, trailing zeros stripped.
    Shortest,
    /// `%a` / `%A` — hexadecimal significand notation.
    Hex,
}

/// One validated floating-point printf directive: flags, width,
/// precision, and conversion. The literal text around the directive is
/// the caller's business (`seq` carries a prefix/suffix pair, `printf`
/// walks a whole template); this type renders exactly the directive.
// The five booleans are C's five independent printf flags; the directive
// grammar defines them as free combinations, so an enum would misstate it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FloatDirective {
    /// `-` — left-justify within the field width.
    pub left: bool,
    /// `+` — always print a sign.
    pub plus: bool,
    /// space — print a space where a `+` sign would go.
    pub space: bool,
    /// `#` — alternate form (keep the point / trailing zeros).
    pub alternate: bool,
    /// `0` — pad the field with leading zeros.
    pub zero: bool,
    /// The minimum field width, if given.
    pub width: Option<usize>,
    /// The precision, if given (an empty precision is `0`, as in C).
    pub precision: Option<usize>,
    /// The conversion.
    pub conversion: FloatConversion,
    /// `true` for the uppercase spellings (`E`, `F`, `G`, `A`).
    pub uppercase: bool,
}

impl FloatDirective {
    /// A flagless, widthless directive for `conversion`.
    #[must_use]
    pub fn plain(conversion: FloatConversion) -> Self {
        Self {
            left: false,
            plus: false,
            space: false,
            alternate: false,
            zero: false,
            width: None,
            precision: None,
            conversion,
            uppercase: false,
        }
    }

    /// Write `x` through this directive (sign, body, padding) to `out`,
    /// exactly as C's `printf` renders it in the C locale.
    ///
    /// Memory is bounded whatever the width and precision: the digits an
    /// `f64` can carry are rendered, and the zeros past them and the padding
    /// are written to `out` as they go, so a sink that flushes holds no more
    /// than its own buffer.
    ///
    /// # Errors
    ///
    /// `out`'s own refusal.
    pub fn render_into<W: Write + ?Sized>(&self, x: f64, out: &mut W) -> fmt::Result {
        let negative = x.is_sign_negative();
        let sign = if negative {
            "-"
        } else if self.plus {
            "+"
        } else if self.space {
            " "
        } else {
            ""
        };

        let magnitude = x.abs();
        let body = if magnitude.is_infinite() {
            Body::text(if self.uppercase { "INF" } else { "inf" })
        } else if magnitude.is_nan() {
            Body::text(if self.uppercase { "NAN" } else { "nan" })
        } else {
            match self.conversion {
                FloatConversion::Fixed => self.body_fixed(magnitude),
                FloatConversion::Scientific => self.body_scientific(magnitude),
                FloatConversion::Shortest => self.body_shortest(magnitude),
                FloatConversion::Hex => self.body_hex(magnitude),
            }
        };

        let width = self.width.unwrap_or(0);
        let padding = width.saturating_sub(sign.len().saturating_add(body.len()));
        if padding == 0 {
            out.write_str(sign)?;
            return body.write(out);
        }
        if self.left {
            out.write_str(sign)?;
            body.write(out)?;
            repeat(out, SPACES, padding)
        } else if self.zero && magnitude.is_finite() {
            // Zero padding goes between the sign (and any `0x`) and the
            // digits; C ignores it for infinities.
            out.write_str(sign)?;
            let digits_at = if self.conversion == FloatConversion::Hex {
                2 // after "0x"
            } else {
                0
            };
            out.write_str(&body.digits[..digits_at])?;
            repeat(out, ZEROS, padding)?;
            out.write_str(&body.digits[digits_at..])?;
            repeat(out, ZEROS, body.zeros)?;
            out.write_str(&body.exponent)
        } else {
            repeat(out, SPACES, padding)?;
            out.write_str(sign)?;
            body.write(out)
        }
    }

    /// The value `x` reads back as once written through this directive —
    /// `x` rounded to the digits the directive shows — or `None` for a NaN.
    /// Only the digits an `f64` carries are rendered, so it costs no more
    /// for an absurd precision.
    #[must_use]
    pub fn rounded(&self, x: f64) -> Option<f64> {
        if x.is_nan() {
            return None;
        }
        let magnitude = x.abs();
        let body = match self.conversion {
            _ if magnitude.is_infinite() => return Some(x),
            FloatConversion::Fixed => self.body_fixed(magnitude),
            FloatConversion::Scientific => self.body_scientific(magnitude),
            FloatConversion::Shortest => self.body_shortest(magnitude),
            FloatConversion::Hex => self.body_hex(magnitude),
        };
        // The zeros past the held digits leave the value as it is.
        let mut text = body.digits;
        text.push_str(&body.exponent);
        let (value, _) = crate::cnum::scan_double(&text)?;
        Some(if x.is_sign_negative() { -value } else { value })
    }

    /// Render `x` through this directive into a fresh `String`.
    #[must_use]
    pub fn render(&self, x: f64) -> String {
        let mut out = String::new();
        // A `String` takes every write.
        let _ = self.render_into(x, &mut out);
        out
    }

    /// `%f`: fixed-point, default precision 6.
    fn body_fixed(&self, magnitude: f64) -> Body {
        let precision = self.precision.unwrap_or(6);
        let shown = precision.min(EXACT_DIGITS);
        let mut digits = String::new();
        let _ = write!(digits, "{magnitude:.shown$}");
        if precision == 0 && self.alternate {
            digits.push('.');
        }
        Body {
            digits,
            zeros: precision - shown,
            exponent: String::new(),
        }
    }

    /// `%e`: scientific, default precision 6, exponent `e±dd`.
    fn body_scientific(&self, magnitude: f64) -> Body {
        let precision = self.precision.unwrap_or(6);
        let shown = precision.min(EXACT_DIGITS);
        let (mut digits, exponent) = split_exponential(magnitude, shown);
        if precision == 0 && self.alternate {
            digits.push('.');
        }
        Body {
            digits,
            zeros: precision - shown,
            exponent: self.exponent_text(exponent),
        }
    }

    /// `%g`: the C rule — `%e` style when the rounded exponent `X` is
    /// below -4 or at least the significant-digit count `P`, else `%f`
    /// style with precision `P-1-X`; trailing zeros stripped unless `#`.
    fn body_shortest(&self, magnitude: f64) -> Body {
        let p = match self.precision.unwrap_or(6) {
            0 => 1,
            p => p,
        };
        let fraction = p - 1;
        let (mantissa, x) = split_exponential(magnitude, fraction.min(EXACT_DIGITS));
        let (mut digits, fraction, exponent) = if x < -4 || len_i64(p) <= i64::from(x) {
            (mantissa, fraction, self.exponent_text(x))
        } else {
            let fraction = usize::try_from(len_i64(p) - 1 - i64::from(x)).unwrap_or(0);
            let shown = fraction.min(EXACT_DIGITS);
            let mut digits = String::new();
            let _ = write!(digits, "{magnitude:.shown$}");
            (digits, fraction, String::new())
        };
        let zeros = if self.alternate {
            fraction - fraction.min(EXACT_DIGITS)
        } else {
            strip_trailing_zeros(&mut digits);
            0
        };
        Body {
            digits,
            zeros,
            exponent,
        }
    }

    /// `%a`: hexadecimal significand (`0x1.8p+1`), glibc-style: normals
    /// carry a leading `1`, subnormals a leading `0`; without a precision
    /// the fraction is exact with trailing zeros stripped.
    fn body_hex(&self, magnitude: f64) -> Body {
        let bits = magnitude.to_bits();
        // The 11-bit exponent field always fits an i64.
        let raw_exponent = i64::try_from((bits >> 52) & 0x7FF).unwrap_or(0);
        let fraction = bits & ((1_u64 << 52) - 1);

        let (lead, frac, exponent) = if raw_exponent == 0 {
            if fraction == 0 {
                (0_u64, 0_u64, 0_i64) // zero prints 0x0p+0
            } else {
                (0, fraction << 8, -1022) // subnormal, glibc style
            }
        } else {
            (1, fraction << 8, raw_exponent - 1023)
        };
        // `frac` now holds the 52 fraction bits left-aligned in 60 bits —
        // 15 hex digits, the top 13 significant.

        let (lead, frac) = match self.precision {
            None => (lead, frac),
            Some(p) => round_hex_fraction(lead, frac, p),
        };

        let mut digits = String::from(if self.uppercase { "0X" } else { "0x" });
        let _ = write!(digits, "{lead}");
        let count = match self.precision {
            // Exact: the 13 fraction digits with trailing zeros stripped.
            None => {
                let mut field = frac >> 8;
                let mut count = 13_usize;
                while count > 0 && (field == 0 || field.trailing_zeros() >= 4) {
                    field >>= 4;
                    count -= 1;
                }
                count
            }
            Some(p) => p,
        };
        if count > 0 || self.alternate {
            digits.push('.');
        }
        // Past the 15-digit field every digit is a zero.
        let field = count.min(15);
        for i in 0..field {
            let digit = (frac >> ((14 - i) * 4)) & 0xF;
            let _ = write!(
                digits,
                "{}",
                char::from_digit(u32::try_from(digit).unwrap_or(0), 16).unwrap_or('0')
            );
        }
        if self.uppercase {
            digits = digits.to_ascii_uppercase();
        }
        let mut text = String::from(if self.uppercase { "P" } else { "p" });
        let _ = write!(
            text,
            "{}{}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        );
        Body {
            digits,
            zeros: count - field,
            exponent: text,
        }
    }

    /// A C-style `e±dd` exponent (sign always, at least two digits).
    fn exponent_text(&self, exponent: i32) -> String {
        let mut text = String::from(if self.uppercase { "E" } else { "e" });
        let _ = write!(
            text,
            "{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        );
        text
    }
}

/// The most digits after the point any rendering needs held: an `f64`'s
/// longest exact decimal expansion, a subnormal's, has 1074, so every digit
/// past this is a zero and is written as a count rather than held.
const EXACT_DIGITS: usize = 1100;

/// A rendered body: its digits, then `zeros` zeros, then its exponent.
struct Body {
    digits: String,
    zeros: usize,
    exponent: String,
}

impl Body {
    fn text(text: &str) -> Self {
        Self {
            digits: String::from(text),
            zeros: 0,
            exponent: String::new(),
        }
    }

    fn len(&self) -> usize {
        self.digits
            .len()
            .saturating_add(self.zeros)
            .saturating_add(self.exponent.len())
    }

    fn write<W: Write + ?Sized>(&self, out: &mut W) -> fmt::Result {
        out.write_str(&self.digits)?;
        repeat(out, ZEROS, self.zeros)?;
        out.write_str(&self.exponent)
    }
}

/// Sixty-four zeros, and sixty-four spaces: the padding blocks.
const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const SPACES: &str = "                                                                ";

/// Write `count` characters of the padding `block` is made of, a block at a
/// time.
fn repeat<W: Write + ?Sized>(out: &mut W, block: &str, count: usize) -> fmt::Result {
    let mut left = count;
    while left > 0 {
        let take = left.min(block.len());
        out.write_str(&block[..take])?;
        left -= take;
    }
    Ok(())
}

/// Round the 60-bit left-aligned hex fraction to `p` hex digits,
/// half-to-even, propagating a carry into the leading digit (which C
/// renders as `2.` — the exponent is not renormalised).
fn round_hex_fraction(lead: u64, frac: u64, p: usize) -> (u64, u64) {
    if p >= 15 {
        return (lead, frac);
    }
    let keep_bits = 4 * u32::try_from(p).unwrap_or(0); // p < 15, so 0..=56
    let dropped_bits = 60 - keep_bits;
    let kept = frac >> dropped_bits;
    // The 60-bit field sits left-aligned at bit 59, so the first dropped
    // bit (bit `59 - keep_bits`) reaches the register's top at a shift of
    // `keep_bits + 4`.
    let dropped = frac << (keep_bits + 4);
    let half = 1_u64 << 63;
    let round_up = match dropped.cmp(&half) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Equal => {
            if p == 0 {
                lead & 1 == 1
            } else {
                kept & 1 == 1
            }
        }
        core::cmp::Ordering::Less => false,
    };
    let mut kept = kept;
    let mut lead = lead;
    if round_up {
        kept += 1;
        if p == 0 || kept >> keep_bits != 0 {
            lead += 1;
            kept = 0;
        }
    }
    (lead, kept << dropped_bits)
}

/// A precision as an `i64` (a printf precision cannot approach
/// `i64::MAX`; the clamp keeps the conversion total).
fn len_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Split `magnitude` into its `%e` mantissa (rounded to `precision`
/// digits after the point) and decimal exponent.
fn split_exponential(magnitude: f64, precision: usize) -> (String, i32) {
    let mut text = String::new();
    let _ = write!(text, "{magnitude:.precision$e}");
    // Rust renders `1.50e-2`; the exponent part is always present.
    match text.rsplit_once('e') {
        Some((mantissa, exponent)) => (String::from(mantissa), exponent.parse().unwrap_or(0)),
        // Unreachable: `{:e}` always emits an exponent; fail closed.
        None => (text, 0),
    }
}

/// Remove trailing zeros of a `%g` fixed/mantissa body, and the point
/// itself when nothing follows it.
fn strip_trailing_zeros(body: &mut String) {
    if !body.contains('.') {
        return;
    }
    while body.ends_with('0') {
        body.pop();
    }
    if body.ends_with('.') {
        body.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::{FloatConversion, FloatDirective, EXACT_DIGITS};
    use alloc::string::String;

    /// Render `x` through a directive spelled as printf flags, width,
    /// precision, and a conversion letter (a tiny test-only reader; the
    /// consumers own their real grammars).
    fn render(spec: &str, x: f64) -> String {
        directive(spec).render(x)
    }

    /// The directive `spec` spells.
    fn directive(spec: &str) -> FloatDirective {
        let bytes = spec.as_bytes();
        let mut d = FloatDirective::plain(FloatConversion::Fixed);
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            match b {
                b'-' => d.left = true,
                b'+' => d.plus = true,
                b' ' => d.space = true,
                b'#' => d.alternate = true,
                b'0' => d.zero = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = None;
        while let Some(&b) = bytes.get(i) {
            if !b.is_ascii_digit() {
                break;
            }
            width = Some(width.unwrap_or(0) * 10 + usize::from(b - b'0'));
            i += 1;
        }
        d.width = width;
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            let mut precision = 0;
            while let Some(&b) = bytes.get(i) {
                if !b.is_ascii_digit() {
                    break;
                }
                precision = precision * 10 + usize::from(b - b'0');
                i += 1;
            }
            d.precision = Some(precision);
        }
        let conv = bytes.get(i).copied().unwrap_or(b'f');
        d.conversion = match conv.to_ascii_lowercase() {
            b'e' => FloatConversion::Scientific,
            b'g' => FloatConversion::Shortest,
            b'a' => FloatConversion::Hex,
            _ => FloatConversion::Fixed,
        };
        d.uppercase = conv.is_ascii_uppercase();
        d
    }

    /// A sink that keeps only the length written and the longest write.
    #[derive(Default)]
    struct Measure {
        written: usize,
        longest: usize,
        head: String,
    }

    impl core::fmt::Write for Measure {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
            self.written += text.len();
            self.longest = self.longest.max(text.len());
            if self.head.len() < 32 {
                self.head
                    .push_str(&text[..text.len().min(32 - self.head.len())]);
            }
            Ok(())
        }
    }

    fn measure(spec: &str, x: f64) -> Measure {
        let mut sink = Measure::default();
        directive(spec)
            .render_into(x, &mut sink)
            .expect("the sink takes every write");
        sink
    }

    #[test]
    fn a_field_of_any_size_is_written_without_being_held() {
        let fixed = measure(".1000000f", 1.5);
        assert_eq!(fixed.written, 1_000_002);
        assert!(fixed.head.starts_with("1.50000"), "{}", fixed.head);
        assert!(fixed.longest <= EXACT_DIGITS + 64, "{}", fixed.longest);

        let padded = measure("1000000.1f", -2.0);
        assert_eq!(padded.written, 1_000_000);
        assert!(padded.longest <= 64);

        let zero_padded = measure("0100000.2e", 3.0);
        assert_eq!(zero_padded.written, 100_000);
        assert!(zero_padded.head.starts_with("00000"));

        assert_eq!(measure("#.100000g", 0.5).written, "0.".len() + 100_000);
        assert_eq!(
            measure(".100000a", 1.0).written,
            "0x1.".len() + 100_000 + "p+0".len()
        );
    }

    #[test]
    fn a_value_reads_back_as_its_rendering_rounds_it() {
        assert_eq!(
            directive(".1f").rounded(0.300_000_000_000_000_04),
            Some(0.3)
        );
        assert_eq!(directive(".0f").rounded(2.5), Some(2.0), "ties to even");
        assert_eq!(directive("g").rounded(1.000_000_1), Some(1.0));
        assert_eq!(directive(".1000000f").rounded(0.25), Some(0.25));
        assert_eq!(directive(".3e").rounded(-12_345.0), Some(-12_340.0));
        assert_eq!(directive("a").rounded(1.5), Some(1.5));
        assert_eq!(
            directive("f").rounded(f64::NEG_INFINITY),
            Some(f64::NEG_INFINITY)
        );
        assert_eq!(directive("f").rounded(f64::NAN), None);
    }

    #[test]
    fn a_precision_past_the_exact_digits_pads_them_with_zeros() {
        use core::fmt::Write as _;
        for x in [1.0 / 3.0, 5e-324, 123_456.789, 2.0f64.powi(-1074) * 3.0] {
            let mut exact = String::new();
            let _ = write!(exact, "{x:.3000}");
            assert_eq!(render(".3000f", x), exact, "{x:e}");
            let mut exact = String::new();
            let _ = write!(exact, "{x:.2500e}");
            let mut ours = render(".2500e", x);
            // Rust spells the exponent `e-5`, C `e-05`.
            ours.truncate(ours.rfind('e').expect("an exponent"));
            assert_eq!(
                ours,
                exact[..exact.rfind('e').expect("an exponent")],
                "{x:e}"
            );
        }
    }

    #[test]
    fn fixed_matches_c_printf() {
        assert_eq!(render("f", 1.0), "1.000000");
        assert_eq!(render(".2f", 2.5), "2.50");
        assert_eq!(render(".0f", 2.5), "2", "ties to even");
        assert_eq!(render(".0f", 3.5), "4", "ties to even");
        assert_eq!(render(".0f", 0.5), "0", "ties to even");
        assert_eq!(render("#.0f", 1.0), "1.", "alternate keeps the point");
        assert_eq!(render(".1f", -0.25), "-0.2", "exact binary value rounds");
        assert_eq!(render("f", -0.0), "-0.000000", "negative zero keeps sign");
    }

    #[test]
    fn flags_and_width_match_c_printf() {
        assert_eq!(render("8.2f", 1.5), "    1.50");
        assert_eq!(render("-8.2f", 1.5), "1.50    ");
        assert_eq!(render("08.2f", -1.5), "-0001.50");
        assert_eq!(render("+.1f", 1.0), "+1.0");
        assert_eq!(render(" .1f", 1.0), " 1.0");
        assert_eq!(
            render("08f", f64::INFINITY),
            "     inf",
            "no zero padding for inf"
        );
        assert_eq!(render("F", f64::INFINITY), "INF");
        assert_eq!(render("+f", f64::INFINITY), "+inf");
        assert_eq!(render("f", f64::NAN), "nan");
        assert_eq!(render("F", f64::NAN), "NAN");
    }

    #[test]
    fn scientific_matches_c_printf() {
        assert_eq!(render("e", 1.0), "1.000000e+00");
        assert_eq!(render(".2e", 12345.0), "1.23e+04");
        assert_eq!(render(".0e", 0.5), "5e-01");
        assert_eq!(render("#.0e", 0.5), "5.e-01");
        assert_eq!(render(".2e", 0.0), "0.00e+00");
        assert_eq!(render("E", 12345.0), "1.234500E+04");
        assert_eq!(render(".1e", 1e100), "1.0e+100", "three-digit exponents");
    }

    #[test]
    fn shortest_matches_c_printf() {
        assert_eq!(render("g", 1_000_000.0), "1e+06");
        assert_eq!(render("g", 100_000.0), "100000");
        assert_eq!(render("g", 0.0001), "0.0001");
        assert_eq!(render("g", 0.00001), "1e-05");
        assert_eq!(render("g", 0.5), "0.5");
        assert_eq!(render("g", 0.0), "0");
        assert_eq!(render("g", 123_456_789.0), "1.23457e+08");
        assert_eq!(render(".3g", 1234.0), "1.23e+03");
        assert_eq!(render(".0g", 1234.0), "1e+03", "precision 0 acts as 1");
        assert_eq!(render("#g", 1.0), "1.00000", "alternate keeps zeros");
        assert_eq!(render("G", 0.00001), "1E-05");
        assert_eq!(
            render("g", 999_999.5),
            "1e+06",
            "style chosen from the rounded exponent"
        );
    }

    #[test]
    fn hex_matches_c_printf() {
        assert_eq!(render("a", 1.0), "0x1p+0");
        assert_eq!(render("a", 3.0), "0x1.8p+1");
        assert_eq!(render("a", 0.0), "0x0p+0");
        assert_eq!(render("a", -0.5), "-0x1p-1");
        assert_eq!(render(".1a", 1.5), "0x1.8p+0");
        assert_eq!(render(".0a", 1.5), "0x2p+0", "tie rounds the odd lead up");
        assert_eq!(
            render(".0a", 2.5),
            "0x1p+1",
            "tie rounds the odd digit down"
        );
        assert_eq!(render(".3a", 1.0), "0x1.000p+0");
        assert_eq!(render("#a", 1.0), "0x1.p+0", "alternate keeps the point");
        assert_eq!(render("A", 3.0), "0X1.8P+1");
        assert_eq!(
            render("a", 5e-324),
            "0x0.0000000000001p-1022",
            "least subnormal, glibc spelling"
        );
        assert_eq!(
            render("010.1a", 1.5),
            "0x001.8p+0",
            "zero padding lands after 0x"
        );
    }
}
