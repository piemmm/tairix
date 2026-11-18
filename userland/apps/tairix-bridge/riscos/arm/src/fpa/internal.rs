//! FPEmulator's internal format, and the round stage every result passes.

use tairix_softfloat::{Exact, Parts, Rounding as Round};

use super::{Precision, Rounding};
use crate::word::{high, low, low64};

pub(super) const SIGN: u32 = 1 << 31;
pub(super) const UNCOMMON: u32 = 1 << 30;
pub(super) const EXPONENT: u32 = 0x7FFF;
pub(super) const UNITS: u64 = 1 << 63;
pub(super) const QUIET: u64 = 1 << 62;
pub(super) const BIAS: i32 = 0x3FFF;

/// A register's value in FPEmulator's internal format, the three words `SFM`
/// stores.
///
/// The first word holds the sign in bit 31, the uncommon bit in bit 30 and
/// the biased exponent in bits 14 to 0; the mantissa's top bit is the units
/// bit. A common value is a zero or a normalised number. An uncommon one is
/// a denormal, an unnormal, an infinity or a NaN, a single or double one
/// marked by its own precision's exponent and a units bit of one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Internal {
    /// The sign, the uncommon bit and the exponent.
    pub head: u32,
    /// The units bit and the fraction.
    pub mantissa: u64,
}

impl Internal {
    /// Positive zero.
    pub const ZERO: Self = Self {
        head: 0,
        mantissa: 0,
    };

    /// The standard infinity, an extended one.
    pub(super) const fn infinity(negative: bool) -> Self {
        Self {
            head: sign(negative) | UNCOMMON | EXPONENT,
            mantissa: 0,
        }
    }

    pub(super) const fn zero(negative: bool) -> Self {
        Self {
            head: sign(negative),
            mantissa: 0,
        }
    }

    /// The value as three words.
    #[must_use]
    pub const fn words(self) -> [u32; 3] {
        [self.head, high(self.mantissa), low(self.mantissa)]
    }

    /// Three words as a value, every bit kept.
    #[must_use]
    pub const fn from_words([head, upper, lower]: [u32; 3]) -> Self {
        Self {
            head,
            mantissa: (upper as u64) << 32 | lower as u64,
        }
    }

    /// One of the eight constants a data operation may name: 0, 1, 2, 3, 4,
    /// 5, 0.5 and 10.
    #[must_use]
    pub const fn constant(n: u8) -> Self {
        let (exponent, mantissa) = match n & 7 {
            0 => (0, 0),
            1 => (0x3FFF, 0x8000_0000),
            2 => (0x4000, 0x8000_0000),
            3 => (0x4000, 0xC000_0000),
            4 => (0x4001, 0x8000_0000),
            5 => (0x4001, 0xA000_0000),
            6 => (0x3FFE, 0x8000_0000),
            _ => (0x4002, 0xA000_0000),
        };
        Self {
            head: exponent,
            mantissa: mantissa << 32,
        }
    }

    pub(super) const fn negative(self) -> bool {
        self.head & SIGN != 0
    }

    pub(super) const fn uncommon(self) -> bool {
        self.head & UNCOMMON != 0
    }

    pub(super) const fn exponent(self) -> u32 {
        self.head & EXPONENT
    }

    pub(super) const fn units(self) -> bool {
        self.mantissa & UNITS != 0
    }

    pub(super) const fn fraction(self) -> u64 {
        self.mantissa & !UNITS
    }

    /// The finite value the register holds, as the arithmetic reads it;
    /// `None` for an infinity or a NaN.
    #[must_use]
    pub fn value(self) -> Option<Parts> {
        (!self.special()).then(|| self.number().parts())
    }

    /// Whether the arithmetic takes the value for a NaN or an infinity: an
    /// uncommon value in the top half of the exponents with its units bit set,
    /// or with the extended exponent.
    pub(super) const fn special(self) -> bool {
        let exponent = self.exponent();
        self.uncommon() && exponent >= 0x4000 && (exponent == EXPONENT || self.units())
    }

    pub(super) const fn nan(self) -> bool {
        self.special() && self.fraction() != 0
    }

    pub(super) const fn infinite(self) -> bool {
        self.special() && self.fraction() == 0
    }

    /// Whether the value is zero: a zero, or the marker of a single or double
    /// denormal with no fraction, a pattern only `LFM` makes. No infinity
    /// or NaN is.
    pub(super) const fn is_zero(self) -> bool {
        !self.special() && (self.mantissa == 0 || (self.uncommon() && self.mantissa == UNITS))
    }

    pub(super) const fn signalling(self) -> bool {
        self.mantissa & QUIET == 0
    }

    pub(super) const fn with_sign(self, negative: bool) -> Self {
        Self {
            head: (self.head & !SIGN) | sign(negative),
            ..self
        }
    }

    /// A NaN's precision, from its exponent.
    pub(super) const fn nan_precision(self) -> Precision {
        match self.exponent() {
            0..=0x43FE => Precision::Single,
            0x43FF => Precision::Double,
            _ => Precision::Extended,
        }
    }

    /// A numeric value as the arithmetic reduces it: a single or double
    /// denormal loses its marker units bit and takes the exponent above its
    /// own, and the mantissa is then normalised, the exponent going below
    /// zero where it must. A zero stays zero.
    pub(super) fn number(self) -> Number {
        self.reduced().normalised()
    }

    /// A numeric value with a single or double denormal's marker removed,
    /// not normalised.
    pub(super) fn reduced(self) -> Number {
        let mut exponent = self.exponent().cast_signed();
        let mut mantissa = self.mantissa;
        if self.uncommon() && self.units() {
            mantissa &= !UNITS;
            exponent += 1;
        }
        Number {
            negative: self.negative(),
            exponent,
            mantissa,
        }
    }
}

pub(super) const fn sign(negative: bool) -> u32 {
    if negative {
        SIGN
    } else {
        0
    }
}

/// A finite value as the arithmetic holds it: `mantissa × 2^(exponent −
/// BIAS − 63)`, the exponent biased and unbounded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Number {
    pub negative: bool,
    pub exponent: i32,
    pub mantissa: u64,
}

impl Number {
    /// The mantissa's leading zeros shifted out; zero has exponent zero.
    pub fn normalised(self) -> Self {
        if self.mantissa == 0 {
            return Self {
                exponent: 0,
                ..self
            };
        }
        let zeros = self.mantissa.leading_zeros();
        Self {
            exponent: self.exponent - zeros.cast_signed(),
            mantissa: self.mantissa << zeros,
            ..self
        }
    }

    pub const fn parts(self) -> Parts {
        Parts {
            negative: self.negative,
            exp: self.exponent - BIAS,
            sig: self.mantissa,
        }
    }

    /// The value exactly, for the round stage.
    pub fn exact(self) -> Exact {
        Exact::of(self.parts())
    }
}

/// Which way a rounding moved a value's magnitude.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Direction {
    Exact,
    Down,
    Up,
}

/// An exact result with its rounding so far: the mantissa it is to keep,
/// its biased exponent, and the bits below as the round stage reads them.
#[derive(Clone, Copy, Debug)]
pub(super) struct Rounded {
    pub negative: bool,
    pub exponent: i32,
    pub mantissa: u64,
    pub direction: Direction,
}

/// `x` rounded at `bits` from its 64-bit window's top: the place is fixed, so
/// a mantissa without its units bit keeps fewer bits, as `URD` leaves its
/// integer.
pub(super) fn round(x: Exact, bits: u32, rounding: Rounding) -> Rounded {
    // Every FPA precision keeps 24, 53 or 64 bits.
    let round = Round::new(rounding.mode(), bits).unwrap_or(Round::extended(rounding.mode()));
    let r = x.rounded(round);
    let kept = x.chopped() & keep_mask(bits);
    let direction = if !r.inexact {
        Direction::Exact
    } else if r.carried || r.sig != kept {
        Direction::Up
    } else {
        Direction::Down
    };
    Rounded {
        negative: x.negative,
        exponent: x.exp + BIAS + i32::from(r.carried),
        mantissa: r.sig,
        direction,
    }
}

/// The mantissa bits a precision of `bits` keeps.
pub(super) const fn keep_mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        !(u64::MAX >> bits)
    }
}

/// `r`'s mantissa shifted right `shift` places and rounded again at `bits`;
/// a tie is broken by which way `r` was rounded, so the two roundings round
/// once.
pub(super) fn denormalise(
    r: Rounded,
    shift: u32,
    bits: u32,
    rounding: Rounding,
) -> (u64, Direction) {
    // The mantissa over 64 more bits below it, then shifted.
    let wide = u128::from(r.mantissa) << 64;
    let (shifted, sticky) = if shift >= 128 {
        (0, r.mantissa != 0)
    } else {
        (wide >> shift, wide & ((1u128 << shift) - 1) != 0)
    };
    let below = 64 - bits;
    let lsb = 64 + below;
    let kept = low64(shifted >> lsb) << below;
    let rest = shifted & ((1u128 << lsb) - 1);
    if rest == 0 && !sticky {
        return (kept, Direction::Exact);
    }
    let half = 1u128 << (lsb - 1);
    let up = match rounding {
        Rounding::Nearest => {
            rest > half
                || (rest == half
                    && (sticky
                        || match r.direction {
                            Direction::Down => true,
                            Direction::Up => false,
                            Direction::Exact => kept & (1u64 << below) != 0,
                        }))
        }
        Rounding::PlusInfinity => !r.negative,
        Rounding::MinusInfinity => r.negative,
        Rounding::Zero => false,
    };
    if up {
        // A shift of at least one leaves room above the kept bits.
        (kept.saturating_add(1u64 << below), Direction::Up)
    } else {
        (kept, Direction::Down)
    }
}

/// A biased exponent known to lie in the field's range, as its field.
pub(super) const fn exponent_field(exponent: i32) -> u32 {
    exponent.cast_unsigned() & EXPONENT
}
