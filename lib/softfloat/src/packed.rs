//! The FPA's packed decimal memory formats (ARM7500FE data sheet, ARM DDI
//! 0077B, 9.3.5 and 9.3.6): a sign nibble, then the exponent's BCD digits and
//! the significand's, most significant first from the top of the lowest word.

use crate::decimal::Decimal;

/// A packed decimal value as memory holds it, `W` words wide: three for the
/// standard format, four for the expanded one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Packed<const W: usize> {
    /// The number's sign: bit 31 of the first word.
    pub negative: bool,
    /// The exponent's sign: bit 30.
    pub exponent_negative: bool,
    /// The exponent's nibbles, its last digit lowest.
    pub exponent: u32,
    /// The significand's nibbles, its last digit lowest. The decimal point
    /// follows the first digit.
    pub digits: u128,
}

/// P: four exponent digits and nineteen significand digits in three words.
pub type Standard = Packed<3>;

/// EP: seven exponent digits and twenty-four significand digits in four.
pub type Expanded = Packed<4>;

impl<const W: usize> Packed<W> {
    /// The exponent's digits.
    pub const EXPONENT_DIGITS: u32 = if W == 3 { 4 } else { 7 };

    /// The significand's digits.
    pub const DIGITS: u32 = if W == 3 { 19 } else { 24 };

    /// Evaluated wherever the format is read or written, so no other width
    /// compiles.
    const FORMAT: () = assert!(
        W == 3 || W == 4,
        "the FPA's packed formats are three and four words"
    );

    const DIGIT_BITS: u32 = 4 * Self::DIGITS;
    const EXPONENT_BITS: u32 = 4 * Self::EXPONENT_DIGITS;

    /// The value `words` hold, lowest address first. Bits 29 and 28 of the
    /// first word, which the format keeps zero, are not read.
    #[must_use]
    pub fn from_words(words: [u32; W]) -> Self {
        let () = Self::FORMAT;
        let string = words
            .iter()
            .fold(0u128, |string, &word| string << 32 | u128::from(word));
        let sign = string >> (Self::DIGIT_BITS + Self::EXPONENT_BITS);
        let exponent = string >> Self::DIGIT_BITS & ((1 << Self::EXPONENT_BITS) - 1);
        Self {
            negative: sign & 0b1000 != 0,
            exponent_negative: sign & 0b0100 != 0,
            exponent: u32::try_from(exponent).unwrap_or(0),
            digits: string & ((1 << Self::DIGIT_BITS) - 1),
        }
    }

    /// The words, lowest address first; `None` where a field is wider than
    /// the format holds.
    #[must_use]
    pub fn to_words(self) -> Option<[u32; W]> {
        let () = Self::FORMAT;
        if self.exponent >> Self::EXPONENT_BITS != 0 || self.digits >> Self::DIGIT_BITS != 0 {
            return None;
        }
        let sign = u128::from(self.negative) << 3 | u128::from(self.exponent_negative) << 2;
        let string = sign << (Self::DIGIT_BITS + Self::EXPONENT_BITS)
            | u128::from(self.exponent) << Self::DIGIT_BITS
            | self.digits;
        let mut words = [0; W];
        for (from_top, word) in (1..=W).rev().zip(&mut words) {
            let shift = u32::try_from(32 * (from_top - 1)).unwrap_or(0);
            *word = u32::try_from(string >> shift & 0xFFFF_FFFF).unwrap_or(0);
        }
        Some(words)
    }

    /// The value the digits hold; `None` where a nibble is above nine, as
    /// an infinity's or a NaN's exponent is.
    #[must_use]
    pub fn value(self) -> Option<Decimal> {
        let significand = from_bcd(self.digits, Self::DIGITS)?;
        let magnitude =
            i32::try_from(from_bcd(u128::from(self.exponent), Self::EXPONENT_DIGITS)?).ok()?;
        let place = if self.exponent_negative {
            -magnitude
        } else {
            magnitude
        };
        Some(Decimal {
            negative: self.negative,
            significand,
            exponent: place - i32::try_from(Self::DIGITS - 1).ok()?,
        })
    }

    /// `d`, its significand normalized to the format's digits; `None` where
    /// it has more digits than the format holds, or its first digit's place
    /// more than the exponent's digits can say. A zero has a positive
    /// exponent.
    #[must_use]
    pub fn of(d: Decimal) -> Option<Self> {
        if d.significand == 0 {
            return Some(Self {
                negative: d.negative,
                exponent_negative: false,
                exponent: 0,
                digits: 0,
            });
        }
        let count = d.significand.ilog10() + 1;
        let significand = d.significand * 10u128.pow(Self::DIGITS.checked_sub(count)?);
        let place = i64::from(d.exponent) + i64::from(count - 1);
        let magnitude = place.unsigned_abs();
        if magnitude >= 10u64.pow(Self::EXPONENT_DIGITS) {
            return None;
        }
        Some(Self {
            negative: d.negative,
            exponent_negative: place < 0,
            exponent: u32::try_from(bcd(u128::from(magnitude))).ok()?,
            digits: bcd(significand),
        })
    }
}

/// `n` in binary-coded decimal, its last digit lowest.
#[must_use]
pub fn bcd(mut n: u128) -> u128 {
    let mut nibbles = 0;
    let mut shift = 0;
    while n != 0 {
        nibbles |= (n % 10) << shift;
        n /= 10;
        shift += 4;
    }
    nibbles
}

/// The integer `digits` BCD nibbles hold, or `None` where one is above nine.
fn from_bcd(nibbles: u128, digits: u32) -> Option<u128> {
    (0..digits).rev().try_fold(0u128, |n, at| {
        let nibble = nibbles >> (4 * at) & 0xF;
        (nibble <= 9).then(|| n * 10 + nibble)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_standard_value_lies_in_its_words_as_the_data_sheet_draws_it() {
        // −1.234 × 10^−56: both signs set, e3..e0 0056, d18..d16 123, d15 4.
        let packed = Standard::of(Decimal {
            negative: true,
            significand: 1234,
            exponent: -59,
        })
        .unwrap();
        assert_eq!(
            packed.to_words(),
            Some([0xC005_6123, 0x4000_0000, 0x0000_0000])
        );
        assert_eq!(Standard::from_words(packed.to_words().unwrap()), packed);
        assert_eq!(
            packed.value(),
            Some(Decimal {
                negative: true,
                significand: 1_234_000_000_000_000_000,
                exponent: -74,
            })
        );
    }

    #[test]
    fn an_expanded_value_has_seven_exponent_digits_and_twenty_four_digits() {
        let packed = Expanded::of(Decimal {
            negative: false,
            significand: 987_654_321_012_345_678_901_234,
            exponent: 1_234_544,
        })
        .unwrap();
        assert_eq!(
            packed.to_words(),
            Some([0x0123_4567, 0x9876_5432, 0x1012_3456, 0x7890_1234])
        );
        assert_eq!(Expanded::from_words(packed.to_words().unwrap()), packed);
    }

    #[test]
    fn zero_infinity_and_the_unused_bits_read_as_the_format_defines() {
        let zero = Standard::of(Decimal {
            negative: true,
            significand: 0,
            exponent: -9,
        })
        .unwrap();
        assert_eq!(zero.to_words(), Some([0x8000_0000, 0, 0]));
        // An infinity's exponent is all ones, which no number's is.
        let infinity = Standard::from_words([0x4FFF_F000, 0, 0]);
        assert_eq!(infinity.exponent, 0xFFFF);
        assert_eq!(infinity.value(), None);
        // Bits 29 and 28 say nothing.
        assert_eq!(
            Standard::from_words([0x3000_0100, 0, 0]),
            Standard::from_words([0x0000_0100, 0, 0])
        );
    }

    #[test]
    fn a_value_the_format_cannot_hold_is_refused() {
        let wide = Decimal {
            negative: false,
            significand: 10u128.pow(19),
            exponent: 0,
        };
        assert_eq!(Standard::of(wide), None);
        assert!(Expanded::of(wide).is_some());
        let far = Decimal {
            negative: false,
            significand: 1,
            exponent: 10_000,
        };
        assert_eq!(Standard::of(far), None);
        let edge = Decimal {
            exponent: 9999,
            ..far
        };
        assert!(Standard::of(edge).is_some());
        let tiny = Decimal {
            exponent: -9999,
            ..far
        };
        assert!(Standard::of(tiny).is_some());
        let mut overfull = Standard::of(edge).unwrap();
        overfull.exponent = 0x1_0000;
        assert_eq!(overfull.to_words(), None);
        overfull.exponent = 0x9999;
        overfull.digits = 1 << 76;
        assert_eq!(overfull.to_words(), None);
    }
}
