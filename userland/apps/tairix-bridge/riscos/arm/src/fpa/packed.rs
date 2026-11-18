//! Packed decimal loads and stores in the data sheet's two formats, through
//! `lib/softfloat`'s [`Packed`]: a number is converted exactly and rounded to
//! nearest, a store writing the significand digits the format guarantees and
//! the rest zero. What the data sheet leaves undefined is read and written as
//! FPEmulator is observed to: a digit above nine counts at its face value, an
//! exponent past a bound reads as an infinity or a NaN, and a NaN's payload is
//! its fraction's bits in a fixed order, written as a decimal integer.

use tairix_softfloat::packed::bcd;
use tairix_softfloat::{Beyond, Decimal, Mode, Packed};

use super::arith::{beyond_range, Unit, Value};
use super::internal::{sign, Internal, EXPONENT, QUIET, UNCOMMON};
use super::{fpsr, Exception};

/// What sets the two formats apart beyond their width.
struct Layout {
    /// The significand digits a store writes: the data sheet's guaranteed
    /// ones.
    stored: u32,
    /// The exponent, at face value, from which a load reads an infinity or a
    /// NaN.
    special: u128,
    /// What a load of a quiet NaN adds to its payload.
    quiet_offset: u64,
}

const STANDARD: Layout = Layout {
    stored: 17,
    special: 16_640,
    quiet_offset: 0,
};

const EXPANDED: Layout = Layout {
    stored: 21,
    special: 16_646_144,
    quiet_offset: 0x3F5 << 52,
};

/// A NaN's payload: the 62 fraction bits below its quiet bit.
const PAYLOAD: u64 = (1 << 62) - 1;

/// The integer `count` nibbles hold, each at face value.
fn face(nibbles: u128, count: u32) -> u128 {
    (0..count)
        .rev()
        .fold(0, |n, at| n * 10 + (nibbles >> (4 * at) & 0xF))
}

/// A NaN's fraction bits in the order a payload holds them, most
/// significant first: 10 to 0, 39 to 11, then 61 to 40.
const fn payload_of(mantissa: u64) -> u64 {
    let fraction = mantissa & PAYLOAD;
    (fraction & 0x7FF) << 51 | (fraction >> 11 & 0x1FFF_FFFF) << 22 | fraction >> 40
}

/// The fraction bits a payload holds, the inverse of [`payload_of`].
const fn fraction_of(payload: u64) -> u64 {
    (payload & 0x3F_FFFF) << 40 | (payload >> 22 & 0x1FFF_FFFF) << 11 | (payload >> 51 & 0x7FF)
}

/// The place of a format's first digit's nibble.
const fn first_digit<const W: usize>() -> u32 {
    4 * (Packed::<W>::DIGITS - 1)
}

impl Unit {
    /// `LDFP`.
    pub fn load_packed(&mut self, words: [u32; 4]) -> Result<Internal, Exception> {
        if self.fpsr & fpsr::EP != 0 {
            self.load(Packed::from_words(words), &EXPANDED)
        } else {
            let [a, b, c, _] = words;
            self.load(Packed::from_words([a, b, c]), &STANDARD)
        }
    }

    fn load<const W: usize>(
        &mut self,
        p: Packed<W>,
        layout: &Layout,
    ) -> Result<Internal, Exception> {
        let exponent = face(u128::from(p.exponent), Packed::<W>::EXPONENT_DIGITS);
        if exponent >= layout.special {
            return Ok(special_loaded(p, layout));
        }
        if p.digits == 0 {
            return Ok(Internal::zero(p.negative));
        }
        // Below the special bound, the face value fits.
        let magnitude = i32::try_from(exponent).unwrap_or(i32::MAX);
        let place = if p.exponent_negative {
            -magnitude
        } else {
            magnitude
        };
        let digits = i32::try_from(Packed::<W>::DIGITS).unwrap_or(0);
        let decimal = Decimal {
            negative: p.negative,
            significand: face(p.digits, Packed::<W>::DIGITS),
            exponent: place - (digits - 1),
        };
        let value = match decimal.exact() {
            Ok(x) => Value::Exact(x),
            Err(Beyond::Above) => beyond_range(p.negative, true),
            Err(Beyond::Below) => beyond_range(p.negative, false),
        };
        self.finish(value)
    }

    /// `STFP`.
    pub fn store_packed(&mut self, a: Internal) -> Result<[u32; 4], Exception> {
        if self.fpsr & fpsr::EP != 0 {
            self.store(a, &EXPANDED)
        } else {
            self.store::<3>(a, &STANDARD).map(|[x, y, z]| [x, y, z, 0])
        }
    }

    fn store<const W: usize>(
        &mut self,
        a: Internal,
        layout: &Layout,
    ) -> Result<[u32; W], Exception> {
        let packed = if a.special() {
            special_stored(a)
        } else {
            // Every register value lies in the conversion's range, and both
            // formats hold its digits and exponent.
            let (decimal, inexact) = Decimal::of(a.number().parts(), layout.stored, Mode::Nearest)
                .unwrap_or((
                    Decimal {
                        negative: a.negative(),
                        significand: 0,
                        exponent: 0,
                    },
                    false,
                ));
            if inexact {
                self.raise(Exception::Inexact)?;
            }
            Packed::of(decimal).unwrap_or(Packed {
                negative: a.negative(),
                exponent_negative: false,
                exponent: 0,
                digits: 0,
            })
        };
        Ok(packed.to_words().unwrap_or([0; W]))
    }
}

/// An infinity, all its digits zero, or a NaN: quiet where its first digit
/// has its top bit set.
fn special_loaded<const W: usize>(p: Packed<W>, layout: &Layout) -> Internal {
    if p.digits == 0 {
        return Internal::infinity(p.negative);
    }
    let marker = 8u128 << first_digit::<W>();
    let quiet = p.digits >= marker;
    let digits = if quiet { p.digits - marker } else { p.digits };
    let offset = if quiet { layout.quiet_offset } else { 0 };
    let payload = (face(digits, Packed::<W>::DIGITS) + u128::from(offset)) & u128::from(PAYLOAD);
    Internal {
        head: sign(p.negative) | UNCOMMON | EXPONENT,
        mantissa: if quiet { QUIET } else { 0 } | fraction_of(u64::try_from(payload).unwrap_or(0)),
    }
}

/// An infinity, the extended one alone, or as a NaN anything else that is
/// special, its payload's first digit always marked quiet; a signalling NaN
/// signals nothing.
fn special_stored<const W: usize>(a: Internal) -> Packed<W> {
    let digits = if a.mantissa == 0 {
        0
    } else {
        bcd(u128::from(payload_of(a.mantissa))) | 8 << first_digit::<W>()
    };
    Packed {
        negative: a.negative(),
        exponent_negative: false,
        exponent: (1 << (4 * Packed::<W>::EXPONENT_DIGITS)) - 1,
        digits,
    }
}
