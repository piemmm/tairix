//! Numbers as text, as BASIC reads and writes them: the decimal syntax `VAL`
//! and a program's literals share, and the three layouts `@%` gives `PRINT`
//! and `STR$`.
//!
//! An exponent is an upper-case `E`, a sign perhaps, and as many digits as
//! the dialect reads. Written, a value's digits are rounded half away from
//! zero.

use core::fmt::{self, Write};

/// `@%`'s flag giving `STR$` its layout as well as `PRINT`.
const STR_FLAG: u32 = 1 << 24;

/// The flag in `@%`'s layout byte asking for a decimal comma.
const COMMA: u8 = 0x80;

/// The least decimal exponent the general layout writes in fixed notation.
const LEAST_FIXED: i32 = -2;

/// A decimal number as written: its digits times ten to a power.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written<'a> {
    /// The sign.
    pub negative: bool,
    /// The digits as written, a point among them perhaps.
    figures: &'a [u8],
    /// The power of ten the last digit stands at.
    pub exponent: i32,
    /// The bytes of text the number took.
    pub length: usize,
    /// Whether a point is written among the digits.
    point: bool,
    /// Whether an exponent is written, digits or none.
    marked: bool,
}

/// Which numbers a dialect reads as integers, as each is recorded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Integers {
    /// BASIC V's: no point, any exponent nought, the magnitude within 32
    /// bits.
    Five,
    /// BASIC VI's: digits alone, below [`SIX_INTEGERS_BELOW`].
    Six,
}

/// The least magnitude BASIC VI reads as a real though written as digits
/// alone.
pub const SIX_INTEGERS_BELOW: i64 = 0x0CC0_0000;

impl<'a> Written<'a> {
    /// No number: zero, taking no text.
    const NOTHING: Self = Self {
        negative: false,
        figures: &[],
        exponent: 0,
        length: 0,
        point: false,
        marked: false,
    };

    /// The integer it is, where `integers` reads it as one.
    #[must_use]
    pub fn integer(&self, integers: Integers) -> Option<i32> {
        let written = !self.point
            && match integers {
                Integers::Five => self.exponent == 0,
                Integers::Six => !self.marked,
            };
        if !written {
            return None;
        }
        let magnitude = self
            .digits()
            .try_fold(0_i64, |n, d| n.checked_mul(10)?.checked_add(i64::from(d)))?;
        if integers == Integers::Six && magnitude >= SIX_INTEGERS_BELOW {
            return None;
        }
        let value = if self.negative { -magnitude } else { magnitude };
        i32::try_from(value).ok()
    }

    /// The digits, each 0 to 9, the most significant first.
    pub fn digits(&self) -> impl Iterator<Item = u8> + 'a {
        self.figures
            .iter()
            .filter(|b| b.is_ascii_digit())
            .map(|&b| b - b'0')
    }

    /// What `VAL` reads from `text`: spaces, a sign, then a number with at
    /// most `exponent_digits` exponent digits; zero where no number follows.
    #[must_use]
    pub fn val(text: &'a [u8], exponent_digits: usize) -> Self {
        let spaces = text.iter().take_while(|&&b| b == b' ').count();
        let (negative, rest) = signed(&text[spaces..]);
        Self::literal(rest, exponent_digits).map_or(Self::NOTHING, |number| Self {
            negative,
            length: text.len() - rest.len() + number.length,
            ..number
        })
    }

    /// The number a program's text holds at its start: digits with a point
    /// among them perhaps, then perhaps an exponent of at most
    /// `exponent_digits` digits; `None` where no digit stands before the
    /// point or after it.
    #[must_use]
    pub fn literal(text: &'a [u8], exponent_digits: usize) -> Option<Self> {
        let whole = leading_digits(text);
        let (fraction, end) = match text.get(whole) {
            Some(b'.') => {
                let fraction = leading_digits(&text[whole + 1..]);
                (fraction, whole + 1 + fraction)
            }
            _ => (0, whole),
        };
        if whole == 0 && fraction == 0 {
            return None;
        }
        let (power, length, marked) = match text.get(end) {
            Some(b'E') => {
                let (power, taken) = exponent(&text[end + 1..], exponent_digits);
                (power, end + 1 + taken, true)
            }
            _ => (0, end, false),
        };
        let point = end != whole;
        let fraction = i32::try_from(fraction).unwrap_or(i32::MAX);
        Some(Self {
            negative: false,
            figures: &text[..end],
            exponent: power.saturating_sub(fraction),
            length,
            point,
            marked,
        })
    }
}

/// The decimal digits `text` starts with.
fn leading_digits(text: &[u8]) -> usize {
    text.iter().take_while(|b| b.is_ascii_digit()).count()
}

/// The sign `text` starts with, and what follows it.
fn signed(text: &[u8]) -> (bool, &[u8]) {
    match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, text),
    }
}

/// The exponent after an `E`, of at most `most` digits, and the bytes it
/// took.
fn exponent(text: &[u8], most: usize) -> (i32, usize) {
    let (negative, rest) = signed(text);
    let count = leading_digits(rest).min(most);
    let value = rest[..count]
        .iter()
        .fold(0, |value, &b| value * 10 + i32::from(b - b'0'));
    let value = if negative { -value } else { value };
    (value, text.len() - rest.len() + count)
}

/// A layout `@%` gives a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Significant digits, in fixed notation where they reach the units and
    /// the value is not tiny, in exponent notation otherwise.
    General,
    /// Significant digits in exponent notation.
    Exponent,
    /// Decimal places in fixed notation.
    Fixed,
}

/// A number's layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// The layout.
    pub form: Form,
    /// Significant digits, or [`Form::Fixed`]'s decimal places.
    pub digits: u8,
    /// Whether the decimal point is written as a comma.
    pub comma: bool,
}

impl Format {
    /// The layout `@%` gives `PRINT`: byte 2 its form, 1 for exponent and 2
    /// for fixed notation and general otherwise, with bit 7 asking for a
    /// decimal comma; byte 1 its digits.
    #[must_use]
    pub const fn of(at: u32) -> Self {
        let [_, digits, layout, _] = at.to_le_bytes();
        let form = match layout & !COMMA {
            1 => Form::Exponent,
            2 => Form::Fixed,
            _ => Form::General,
        };
        Self {
            form,
            digits,
            comma: layout & COMMA != 0,
        }
    }

    /// The layout `@%` gives `STR$`: `PRINT`'s where `@%` says so, else the
    /// general layout to the most digits.
    #[must_use]
    pub const fn of_str(at: u32) -> Self {
        if at & STR_FLAG == 0 {
            Self {
                form: Form::General,
                digits: 0,
                comma: false,
            }
        } else {
            Self::of(at)
        }
    }
}

/// `@%` made from `at` by the string `text`, as genuine BASIC reads one: a
/// `+` sets the flag `STR$` follows `@%` by, which its absence clears; then
/// `G`, `E` or `F` the form, the field width, and after `.` or `,` the
/// digits, each where given and a byte each. The first byte out of place
/// ends it, what it has not reached kept.
#[must_use]
pub fn at_percent_of(at: u32, text: &[u8]) -> u32 {
    let mut bytes = text.iter().copied().peekable();
    let mut at = if bytes.next_if_eq(&b'+').is_some() {
        at | STR_FLAG
    } else {
        at & !STR_FLAG
    };
    let form = bytes.peek().and_then(|b| match b.to_ascii_uppercase() {
        b'G' => Some(0),
        b'E' => Some(1),
        b'F' => Some(2),
        _ => None,
    });
    if let Some(form) = form {
        bytes.next();
        at = at & !(u32::from(!COMMA) << 16) | form << 16;
    }
    let number = |bytes: &mut core::iter::Peekable<_>| {
        let mut n: Option<u32> = None;
        while let Some(digit) = bytes.next_if(u8::is_ascii_digit) {
            n = Some(
                n.unwrap_or(0)
                    .wrapping_mul(10)
                    .wrapping_add(u32::from(digit - b'0')),
            );
        }
        n.map(|n| n & 0xFF)
    };
    if let Some(width) = number(&mut bytes) {
        at = at & !0xFF | width;
    }
    if let Some(point) = bytes.next_if(|&b| b == b'.' || b == b',') {
        let comma = u32::from(COMMA) << 16;
        at = if point == b',' {
            at | comma
        } else {
            at & !comma
        };
        if let Some(digits) = number(&mut bytes) {
            at = at & !0xFF00 | digits << 8;
        }
    }
    at
}

/// `@%` as the string `LVAR` shows it: `+` where `STR$` follows it, the
/// form's letter, the field width, `.` or `,` and the digits.
pub fn write_at_percent(at: u32, out: &mut impl Write) -> fmt::Result {
    let [width, digits, layout, _] = at.to_le_bytes();
    let letter = match layout & !COMMA {
        1 => 'E',
        2 => 'F',
        _ => 'G',
    };
    let flag = if at & STR_FLAG == 0 { "" } else { "+" };
    let point = if layout & COMMA == 0 { '.' } else { ',' };
    write!(out, "{flag}{letter}{width}{point}{digits}")
}

/// A value BASIC writes as decimal digits, each read from its exact
/// expansion.
pub trait Digits {
    /// The most significant digits the dialect writes.
    const MOST: u8;

    /// The places the exponent layout left-justifies an exponent in.
    const EXPONENT_FIELD: usize;

    /// Whether the value is below zero.
    fn negative(&self) -> bool;

    /// The value to `count` significant digits, rounded half away from zero:
    /// exactly `count` digits as an integer, and the power of ten the last
    /// stands at; `None` for zero.
    fn rounded(&self, count: u32) -> Option<(u128, i32)>;

    /// The first significant digit and the power of ten it stands at;
    /// `None` for zero.
    fn first(&self) -> Option<(u8, i32)>;
}

/// `value` in `format` into `out`.
///
/// # Errors
///
/// `out`'s, where it has no room.
pub fn write<V: Digits>(value: &V, format: Format, out: &mut impl Write) -> fmt::Result {
    if value.negative() {
        out.write_char('-')?;
    }
    let significant = match format.digits {
        0 => V::MOST,
        digits => digits.min(V::MOST),
    };
    let mut out = Notation {
        out,
        point: if format.comma { ',' } else { '.' },
    };
    match format.form {
        Form::General => out.general(value, u32::from(significant)),
        Form::Exponent => out.exponential(value, u32::from(significant)),
        Form::Fixed => out.fixed(value, format.digits.min(V::MOST)),
    }
}

/// Where fixed notation puts a decimal point.
#[derive(Clone, Copy)]
enum Point {
    /// Only before a fractional digit.
    WhereFraction,
    /// Always, followed by at least this many places.
    Places(u8),
}

/// A number's text going to `out`, with `point` for its decimal point.
struct Notation<'a, W> {
    out: &'a mut W,
    point: char,
}

impl<W: Write> Notation<'_, W> {
    fn general(&mut self, value: &impl Digits, count: u32) -> fmt::Result {
        let Some((digits, last)) = value.rounded(count) else {
            return self.out.write_char('0');
        };
        let figures = Figures::of(digits).stripped();
        let first = last + to_i32(count) - 1;
        if (LEAST_FIXED..to_i32(count)).contains(&first) {
            self.fixed_notation(&figures, first, Point::WhereFraction)
        } else {
            self.mantissa(&figures, false)?;
            write!(self.out, "E{first}")
        }
    }

    fn exponential<V: Digits>(&mut self, value: &V, count: u32) -> fmt::Result {
        let (figures, first) = match value.rounded(count) {
            Some((digits, last)) => (Figures::of(digits), last + to_i32(count) - 1),
            None => (Figures::noughts(count), 0),
        };
        self.mantissa(&figures, true)?;
        write!(self.out, "E{first:<width$}", width = V::EXPONENT_FIELD)
    }

    fn fixed<V: Digits>(&mut self, value: &V, places: u8) -> fmt::Result {
        let places_i = i32::from(places);
        let Some((first_digit, leading)) = value.first() else {
            return self.fixed_notation(&Figures::EMPTY, 0, Point::Places(places));
        };
        let wanted = leading + 1 + places_i;
        if wanted > i32::from(V::MOST) {
            // Places past the dialect's digits give the general layout.
            return self.general(value, u32::from(V::MOST));
        }
        match u32::try_from(wanted).ok().filter(|&count| count > 0) {
            Some(count) => {
                let (digits, last) = value.rounded(count).unwrap_or((0, 0));
                let first = last + to_i32(count) - 1;
                self.fixed_notation(&Figures::of(digits), first, Point::Places(places))
            }
            // The first digit lies just past the places: it rounds the value
            // to one in the last place, or to nought.
            None if wanted == 0 && first_digit >= 5 => {
                self.fixed_notation(&Figures::of(1), -places_i, Point::Places(places))
            }
            None => self.fixed_notation(&Figures::EMPTY, 0, Point::Places(places)),
        }
    }

    /// `figures`, the first standing at the power of ten `first`, in fixed
    /// notation: a nought before the point where no digit stands there.
    fn fixed_notation(&mut self, figures: &Figures, first: i32, point: Point) -> fmt::Result {
        let count = to_i32(figures.len());
        let units = first + 1;
        if units <= 0 {
            self.out.write_char('0')?;
        }
        for at in 0..units {
            self.out.write_char(figures.at(at))?;
        }
        let (places, always) = match point {
            Point::WhereFraction => (0, false),
            Point::Places(places) => (i32::from(places), true),
        };
        let fraction = (count - units).max(places);
        if fraction > 0 || always {
            self.out.write_char(self.point)?;
        }
        for at in units..units + fraction {
            self.out.write_char(figures.at(at))?;
        }
        Ok(())
    }

    /// `figures` as a mantissa `d.ddd`: the point kept after a lone digit
    /// where `point`.
    fn mantissa(&mut self, figures: &Figures, point: bool) -> fmt::Result {
        let (first, rest) = figures.as_str().split_at(1.min(figures.len()));
        self.out.write_str(first)?;
        if !rest.is_empty() || point {
            self.out.write_char(self.point)?;
            self.out.write_str(rest)?;
        }
        Ok(())
    }
}

/// The decimal digits of a `u128`, at most.
const MOST_FIGURES: usize = 39;

/// A significand's decimal digits.
#[derive(Clone, Copy)]
struct Figures {
    bytes: [u8; MOST_FIGURES],
    len: usize,
}

impl Figures {
    const EMPTY: Self = Self {
        bytes: [0; MOST_FIGURES],
        len: 0,
    };

    fn of(n: u128) -> Self {
        let mut figures = Self::EMPTY;
        figures.len = n
            .checked_ilog10()
            .and_then(|log| usize::try_from(log).ok())
            .map_or(1, |log| log + 1);
        let mut rest = n;
        for slot in figures.bytes[..figures.len].iter_mut().rev() {
            *slot = b'0' + u8::try_from(rest % 10).unwrap_or(0);
            rest /= 10;
        }
        figures
    }

    fn noughts(count: u32) -> Self {
        let mut figures = Self::EMPTY;
        figures.len = usize::try_from(count).map_or(1, |count| count.clamp(1, MOST_FIGURES));
        figures.bytes[..figures.len].fill(b'0');
        figures
    }

    /// The figures less their trailing noughts, one kept.
    fn stripped(mut self) -> Self {
        let kept = self.bytes[..self.len].iter().rposition(|&b| b != b'0');
        self.len = kept.map_or(1.min(self.len), |at| at + 1);
        self
    }

    const fn len(&self) -> usize {
        self.len
    }

    /// The digit at `at`, nought past either end.
    fn at(&self, at: i32) -> char {
        usize::try_from(at)
            .ok()
            .and_then(|at| self.bytes[..self.len].get(at))
            .map_or('0', |&b| char::from(b))
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

/// A digit count as a signed place.
fn to_i32(n: impl TryInto<i32>) -> i32 {
    n.try_into().unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests;
