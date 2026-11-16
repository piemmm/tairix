//! Loads and stores: the memory formats to the internal one and back.

use super::arith::{Unit, Value};
use super::internal::{Internal, Number, SIGN, UNCOMMON, UNITS};
use super::{Exception, Precision, Rounding};
use crate::word::{high, low};

/// The reserved bits of an extended value's first word that `LDFE` clears.
const RESERVED: u32 = 0x3FFF_8000;

/// `LDFS`: a single denormal, infinity or NaN keeps the single exponent,
/// marked uncommon, with its units bit set.
pub(super) const fn load_single(word: u32) -> Internal {
    if word << 1 == 0 {
        return Internal::zero(word & SIGN != 0);
    }
    let exponent = (word >> 23) & 0xFF;
    let uncommon = if exponent == 0 || exponent == 0xFF {
        UNCOMMON
    } else {
        0
    };
    Internal {
        head: (word & SIGN) | uncommon | (exponent + 0x3F80),
        mantissa: UNITS | ((word as u64 & 0x7F_FFFF) << 40),
    }
}

/// `LDFD`: as `LDFS`, with the double exponent.
pub(super) const fn load_double([high, low]: [u32; 2]) -> Internal {
    if high << 1 == 0 && low == 0 {
        return Internal::zero(high & SIGN != 0);
    }
    let exponent = (high >> 20) & 0x7FF;
    let uncommon = if exponent == 0 || exponent == 0x7FF {
        UNCOMMON
    } else {
        0
    };
    let fraction = (high as u64 & 0xF_FFFF) << 32 | low as u64;
    Internal {
        head: (high & SIGN) | uncommon | (exponent + 0x3C00),
        mantissa: UNITS | fraction << 11,
    }
}

/// `LDFE`: the reserved bits cleared and the uncommon bit set for anything
/// but a zero or a normalised number.
pub(super) const fn load_extended(words: [u32; 3]) -> Internal {
    let raw = Internal::from_words(words);
    let head = raw.head & !RESERVED & !UNCOMMON;
    let x = Internal {
        head,
        mantissa: raw.mantissa,
    };
    let zero = x.mantissa == 0 && x.exponent() == 0;
    let uncommon = !zero && (!x.units() || x.exponent() == 0x7FFF);
    Internal {
        head: if uncommon { head | UNCOMMON } else { head },
        mantissa: x.mantissa,
    }
}

/// `LFM`: an uncommon value as it was stored but for its reserved bits,
/// anything else as `LDFE` would load it.
pub(super) const fn load_internal(words: [u32; 3]) -> Internal {
    let raw = Internal::from_words(words);
    if raw.uncommon() {
        Internal {
            head: raw.head & !RESERVED,
            ..raw
        }
    } else {
        load_extended(words)
    }
}

impl Unit {
    /// `x` rounded to nearest at `precision`, as a store converts it.
    fn narrowed(&mut self, precision: Precision, x: Number) -> Result<Internal, Exception> {
        self.precision = precision;
        self.rounding = Rounding::Nearest;
        self.finish(Value::Exact(x.exact()))
    }

    /// The value a store of `precision` converts `a` to, still in the
    /// internal format.
    fn prepared(
        &mut self,
        precision: Precision,
        a: Internal,
        own_denormal: bool,
    ) -> Result<Internal, Exception> {
        if a.special() {
            if a.infinite() {
                return Ok(a);
            }
            return self.copy_nan(a, precision);
        }
        if a.is_zero() {
            return Ok(Internal::zero(a.negative()));
        }
        if a.uncommon() && own_denormal {
            return Ok(a);
        }
        self.narrowed(precision, a.number())
    }

    /// `STFS`.
    pub fn store_single(&mut self, a: Internal) -> Result<u32, Exception> {
        let own = a.units() && a.exponent() != 0x3C00;
        let v = self.prepared(Precision::Single, a, own)?;
        let fraction = high(v.mantissa >> 8) & 0x7F_FFFF;
        let sign = v.head & SIGN;
        Ok(if v.units() && !v.uncommon() {
            sign | ((v.exponent() ^ 0x80) & 0xFF) << 23 | fraction
        } else {
            sign | uncommon_exponent(v, 8) << 23 | fraction
        })
    }

    /// `STFD`.
    pub fn store_double(&mut self, a: Internal) -> Result<[u32; 2], Exception> {
        let own = a.units() && a.exponent() != 0x3F80;
        let v = self.prepared(Precision::Double, a, own)?;
        let high_fraction = high(v.mantissa >> 11) & 0xF_FFFF;
        let low_word = low(v.mantissa >> 11);
        let sign = v.head & SIGN;
        let high_word = if v.units() && !v.uncommon() {
            sign | ((v.exponent() ^ 0x400) & 0x7FF) << 20 | high_fraction
        } else {
            sign | uncommon_exponent(v, 11) << 20 | high_fraction
        };
        Ok([high_word, low_word])
    }

    /// `STFE`: exact, but for a NaN's conversion.
    pub fn store_extended(&mut self, a: Internal) -> Result<[u32; 3], Exception> {
        if !a.uncommon() {
            return Ok(a.words());
        }
        if a.special() {
            let v = if a.infinite() {
                a
            } else {
                self.copy_nan(a, Precision::Extended)?
            };
            if v.uncommon() && v.units() && v.fraction() == 0 {
                return Ok(Internal {
                    head: (v.head & !UNCOMMON) | 0x7FFF,
                    mantissa: 0,
                }
                .words());
            }
            return Ok(Internal {
                head: v.head & !UNCOMMON,
                ..v
            }
            .words());
        }
        if a.is_zero() {
            return Ok(Internal::zero(a.negative()).words());
        }
        let x = a.reduced();
        let zeros = x.mantissa.leading_zeros();
        let (exponent, mantissa) = match u32::try_from(x.exponent) {
            Ok(exponent) if zeros > exponent => (0, x.mantissa << exponent),
            Ok(exponent) => (exponent - zeros, x.mantissa << zeros),
            Err(_) => (0, x.mantissa),
        };
        Ok(Internal {
            head: (a.head & SIGN) | exponent,
            mantissa,
        }
        .words())
    }
}

/// An uncommon value's or a zero's exponent as FPEmulator writes it to
/// memory: all ones where its internal exponent is in the upper half, so an
/// infinity or a NaN, else zero.
const fn uncommon_exponent(v: Internal, width: u32) -> u32 {
    if v.exponent() & 0x4000 != 0 {
        (1 << width) - 1
    } else {
        0
    }
}
