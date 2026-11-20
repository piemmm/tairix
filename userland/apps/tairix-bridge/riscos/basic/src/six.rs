//! BASIC VI's real: an IEEE 754 double, whose arithmetic is the FPA's.
//!
//! BASIC VI keeps a double as the FPA transfers one: the word holding the
//! sign and exponent first, each word little-endian. Every operation is an
//! FPA instruction in double precision, rounded to nearest, in [`Arithmetic`]'s
//! FPA: an exception it traps is the floating-point emulator's error, not
//! BASIC's, but for a division by zero, which BASIC VI refuses itself. A
//! number is read through a packed decimal load of its first 19 digits and
//! written from a packed decimal store, so both are rounded as the FPA
//! rounds them.

use core::cmp::Ordering;

use tairix_riscos_arm::fpa::{fpsr, Exception, Format, Fpa, Precision, Rounding};
use tairix_riscos_arm::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaOperand};
use tairix_softfloat::packed::{bcd, Standard};

use crate::text::{Digits, Written};
use crate::Fault;

/// The bytes a value takes in memory.
pub const BYTES: usize = 8;

/// The most exponent digits BASIC VI reads.
pub const EXPONENT_DIGITS: usize = 3;

/// The significant digits a packed decimal holds, and so all of a number's
/// text BASIC VI reads.
const PACKED_DIGITS: usize = 19;

/// The registers an operation's operands and result take.
const RESULT: u8 = 0;
const OPERAND: u8 = 1;

/// A BASIC VI real: a double's bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Six {
    bits: u64,
}

impl Six {
    /// Positive zero.
    pub const ZERO: Self = Self::from_bits(0);

    /// One.
    pub const ONE: Self = Self::from_bits(0x3FF0_0000_0000_0000);

    /// π, as BASIC VI's `PI` holds it.
    pub const PI: Self = Self::from_bits(0x4009_21FB_5444_2D18);

    /// π/180 rounded, which `RAD` multiplies by.
    const RADIAN: Self = Self::from_bits(0x3F91_DF46_A252_9D39);

    /// 180/π rounded, which `DEG` multiplies by.
    const DEGREE: Self = Self::from_bits(0x404C_A5DC_1A63_C1F8);

    /// The double with these bits.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self { bits }
    }

    /// The double's bits.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        self.bits
    }

    /// The value `bytes` hold in memory.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; BYTES]) -> Self {
        Self::from_words([
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        ])
    }

    /// The bytes the value is kept as in memory.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; BYTES] {
        let [high, low] = self.words();
        let (high, low) = (high.to_le_bytes(), low.to_le_bytes());
        [
            high[0], high[1], high[2], high[3], low[0], low[1], low[2], low[3],
        ]
    }

    /// Whether the value is zero, of either sign.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.bits << 1 == 0
    }

    /// Whether the sign bit is set.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.bits >> 63 != 0
    }

    /// The words the FPA transfers, the high one first.
    const fn words(self) -> [u32; 2] {
        let bytes = self.bits.to_be_bytes();
        [
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        ]
    }

    const fn from_words([high, low]: [u32; 2]) -> Self {
        let (high, low) = (high.to_be_bytes(), low.to_be_bytes());
        Self::from_bits(u64::from_be_bytes([
            high[0], high[1], high[2], high[3], low[0], low[1], low[2], low[3],
        ]))
    }
}

/// BASIC VI's arithmetic, in an FPA of its own whose status a program's is
/// carried in and out of.
pub struct Arithmetic {
    fpa: Fpa,
}

impl Arithmetic {
    /// The arithmetic under `status`, the program's floating-point status:
    /// its trap enables decide which exceptions are faults.
    #[must_use]
    pub fn new(status: u32) -> Self {
        let mut arithmetic = Self { fpa: Fpa::new() };
        arithmetic.set_fpsr(status);
        arithmetic
    }

    /// The floating-point status, the flags of every exception the
    /// operations raised untrapped set in it.
    #[must_use]
    pub const fn fpsr(&self) -> u32 {
        self.fpa.fpsr()
    }

    /// Sets the floating-point status. The packed decimal format BASIC VI
    /// reads and writes numbers through stays the standard one.
    pub fn set_fpsr(&mut self, status: u32) {
        self.fpa.write_fpsr(status & !fpsr::EP);
    }

    /// `a + b`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with, as every operation.
    pub fn add(&mut self, a: Six, b: Six) -> Result<Six, Fault> {
        self.dyadic(FpaDyadicOp::Adf, a, b)
    }

    /// `a − b`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn sub(&mut self, a: Six, b: Six) -> Result<Six, Fault> {
        self.dyadic(FpaDyadicOp::Suf, a, b)
    }

    /// `a × b`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn mul(&mut self, a: Six, b: Six) -> Result<Six, Fault> {
        self.dyadic(FpaDyadicOp::Muf, a, b)
    }

    /// `a ÷ b`.
    ///
    /// # Errors
    ///
    /// [`Fault::DivisionByZero`] for a zero divisor, else the exception a
    /// trap stopped the operation with.
    pub fn div(&mut self, a: Six, b: Six) -> Result<Six, Fault> {
        if b.is_zero() {
            return Err(Fault::DivisionByZero);
        }
        self.quotient(a, b)
    }

    /// `a ÷ b` as the FPA divides, unchecked: the division `ELLIPSE` makes,
    /// whose zero divisor is the FPA's division-by-zero exception where the
    /// `/` operator's is BASIC's own error.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn quotient(&mut self, a: Six, b: Six) -> Result<Six, Fault> {
        self.dyadic(FpaDyadicOp::Dvf, a, b)
    }

    /// `a ^ b`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn pow(&mut self, a: Six, b: Six) -> Result<Six, Fault> {
        self.dyadic(FpaDyadicOp::Pow, a, b)
    }

    /// `−a`, as zero less it: a zero of either sign negates to positive zero.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn negate(&mut self, a: Six) -> Result<Six, Fault> {
        self.load(OPERAND, a)?;
        self.fpa
            .dyadic(
                FpaDyadicOp::Rsf,
                Precision::Double,
                Rounding::Nearest,
                RESULT,
                OPERAND,
                FpaOperand::Constant(0),
            )
            .map_err(Fault::Floating)?;
        self.store(RESULT)
    }

    /// `ABS`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn abs(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Abs, a)
    }

    /// `SQR`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn sqrt(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Sqt, a)
    }

    /// `SIN`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn sin(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Sin, a)
    }

    /// `COS`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn cos(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Cos, a)
    }

    /// `TAN`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn tan(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Tan, a)
    }

    /// `ASN`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn asn(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Asn, a)
    }

    /// `ACS`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn acs(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Acs, a)
    }

    /// `ATN`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn atn(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Atn, a)
    }

    /// `EXP`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn exp(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Exp, a)
    }

    /// `LN`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn ln(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Lgn, a)
    }

    /// `LOG`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn log(&mut self, a: Six) -> Result<Six, Fault> {
        self.monadic(FpaMonadicOp::Log, a)
    }

    /// `RAD`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn rad(&mut self, a: Six) -> Result<Six, Fault> {
        self.mul(a, Six::RADIAN)
    }

    /// `DEG`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the operation with.
    pub fn deg(&mut self, a: Six) -> Result<Six, Fault> {
        self.mul(a, Six::DEGREE)
    }

    /// `n`, exactly.
    ///
    /// # Errors
    ///
    /// None arises: every integer has a double.
    pub fn from_int(&mut self, n: i32) -> Result<Six, Fault> {
        self.fpa
            .float(
                Precision::Double,
                Rounding::Nearest,
                RESULT,
                u32::from_le_bytes(n.to_le_bytes()),
            )
            .map_err(Fault::Floating)?;
        self.store(RESULT)
    }

    /// The value as an integer, truncated toward zero, as BASIC VI assigns
    /// a real to an integer variable.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the conversion with: an invalid
    /// operation past an integer's range.
    pub fn to_int(&mut self, a: Six) -> Result<i32, Fault> {
        self.fix(a, Rounding::Zero)
    }

    /// The greatest integer not above the value: BASIC VI's `INT`.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the conversion with.
    pub fn floor(&mut self, a: Six) -> Result<i32, Fault> {
        self.fix(a, Rounding::MinusInfinity)
    }

    /// How `a` compares with `b`; `None` where either is not a number.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the comparison with.
    pub fn compare(&mut self, a: Six, b: Six) -> Result<Option<Ordering>, Fault> {
        self.load(RESULT, a)?;
        self.load(OPERAND, b)?;
        let flags = self
            .fpa
            .compare(FpaCompareOp::Cmf, RESULT, FpaOperand::Register(OPERAND))
            .map_err(Fault::Floating)?;
        let flag = |bit: u32| flags >> bit & 1 != 0;
        Ok(if flag(28) {
            None
        } else if flag(30) {
            Some(Ordering::Equal)
        } else if flag(31) {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Greater)
        })
    }

    /// The number `number` writes: its first 19 digits as a packed decimal,
    /// loaded and stored as a double.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the load or store with: an overflow for
    /// a number past the largest double.
    pub fn read(&mut self, number: &Written<'_>) -> Result<Six, Fault> {
        let mut digits = number.digits().skip_while(|&d| d == 0).peekable();
        if digits.peek().is_none() {
            return Ok(Six::ZERO);
        }
        let (mut significand, mut count) = (0u128, 0usize);
        for digit in digits {
            if count < PACKED_DIGITS {
                significand = significand * 10 + u128::from(digit);
            }
            count += 1;
        }
        let kept = count.min(PACKED_DIGITS);
        // The point follows the packed significand's first digit.
        let first = i64::from(number.exponent) + i64::try_from(count).unwrap_or(i64::MAX) - 1;
        let padding = u32::try_from(PACKED_DIGITS - kept).unwrap_or(0);
        let exponent = u128::from(first.unsigned_abs());
        let packed = Standard {
            negative: number.negative,
            exponent_negative: first < 0,
            exponent: u32::try_from(bcd(exponent))
                .map_err(|_| Fault::Floating(Exception::Overflow))?,
            digits: bcd(significand * 10u128.pow(padding)),
        };
        let [a, b, c] = packed
            .to_words()
            .ok_or(Fault::Floating(Exception::Overflow))?;
        self.fpa
            .load(Format::Packed, RESULT, [a, b, c, 0])
            .map_err(Fault::Floating)?;
        self.store(RESULT)
    }

    /// The digits a packed decimal store writes of `a`, which `STR$` and
    /// `PRINT` round further.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the store with.
    pub fn digits(&mut self, a: Six) -> Result<Packed, Fault> {
        self.load(RESULT, a)?;
        let [x, y, z, _] = self
            .fpa
            .store(Format::Packed, RESULT)
            .map_err(Fault::Floating)?;
        Packed::from_words([x, y, z]).ok_or(Fault::Floating(Exception::InvalidOperation))
    }

    /// `a` rounded to `format`'s precision as `rounding` says, then stored
    /// in it: its words in the FPA's order, the high word first, those past
    /// the format's zero.
    ///
    /// # Errors
    ///
    /// The exception a trap stopped the rounding with: an overflow for a
    /// value past a single's largest.
    pub fn stored(
        &mut self,
        a: Six,
        format: Format,
        rounding: Rounding,
    ) -> Result<[u32; 3], Fault> {
        let precision = match format {
            Format::Single => Precision::Single,
            Format::Double => Precision::Double,
            Format::Extended | Format::Packed => Precision::Extended,
        };
        self.load(OPERAND, a)?;
        self.fpa
            .monadic(
                FpaMonadicOp::Mvf,
                precision,
                rounding,
                RESULT,
                FpaOperand::Register(OPERAND),
            )
            .map_err(Fault::Floating)?;
        let [x, y, z, _] = self.fpa.store(format, RESULT).map_err(Fault::Floating)?;
        Ok(match format {
            Format::Single => [x, 0, 0],
            Format::Double => [x, y, 0],
            Format::Extended | Format::Packed => [x, y, z],
        })
    }

    fn dyadic(&mut self, op: FpaDyadicOp, a: Six, b: Six) -> Result<Six, Fault> {
        self.load(RESULT, a)?;
        self.load(OPERAND, b)?;
        self.fpa
            .dyadic(
                op,
                Precision::Double,
                Rounding::Nearest,
                RESULT,
                RESULT,
                FpaOperand::Register(OPERAND),
            )
            .map_err(Fault::Floating)?;
        self.store(RESULT)
    }

    fn monadic(&mut self, op: FpaMonadicOp, a: Six) -> Result<Six, Fault> {
        self.load(OPERAND, a)?;
        self.fpa
            .monadic(
                op,
                Precision::Double,
                Rounding::Nearest,
                RESULT,
                FpaOperand::Register(OPERAND),
            )
            .map_err(Fault::Floating)?;
        self.store(RESULT)
    }

    fn fix(&mut self, a: Six, rounding: Rounding) -> Result<i32, Fault> {
        self.load(OPERAND, a)?;
        let word = self
            .fpa
            .fix(rounding, FpaOperand::Register(OPERAND))
            .map_err(Fault::Floating)?;
        Ok(i32::from_le_bytes(word.to_le_bytes()))
    }

    fn load(&mut self, register: u8, value: Six) -> Result<(), Fault> {
        let [high, low] = value.words();
        self.fpa
            .load(Format::Double, register, [high, low, 0, 0])
            .map_err(Fault::Floating)
    }

    fn store(&mut self, register: u8) -> Result<Six, Fault> {
        let [high, low, ..] = self
            .fpa
            .store(Format::Double, register)
            .map_err(Fault::Floating)?;
        Ok(Six::from_words([high, low]))
    }
}

/// The digits of a packed decimal store: a significand of 17 digits, those
/// the FPA guarantees, or zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packed {
    negative: bool,
    significand: u128,
    exponent: i32,
}

impl Packed {
    /// The digits the standard format's `words` hold; `None` for an
    /// infinity's or a NaN's, which BASIC VI's arithmetic never stores.
    #[must_use]
    pub fn from_words(words: [u32; 3]) -> Option<Self> {
        let packed = Standard::from_words(words);
        let decimal = packed.value()?;
        Some(Self {
            negative: packed.negative,
            significand: decimal.significand,
            exponent: decimal.exponent,
        })
    }
}

impl Digits for Packed {
    const MOST: u8 = 18;
    const EXPONENT_FIELD: usize = 4;

    fn negative(&self) -> bool {
        self.negative
    }

    fn rounded(&self, count: u32) -> Option<(u128, i32)> {
        if self.significand == 0 {
            return None;
        }
        // The packed digits are rounded half up again to fewer.
        let have = self.significand.ilog10() + 1;
        let scale = |n: u32| 10u128.checked_pow(n);
        Some(match count.cmp(&have) {
            Ordering::Equal => (self.significand, self.exponent),
            Ordering::Greater => {
                let more = count - have;
                (
                    self.significand * scale(more)?,
                    self.exponent - i32::try_from(more).ok()?,
                )
            }
            Ordering::Less => {
                let fewer = have - count;
                let unit = scale(fewer)?;
                let mut kept = self.significand / unit;
                let mut exponent = self.exponent + i32::try_from(fewer).ok()?;
                if self.significand % unit >= unit / 2 {
                    kept += 1;
                }
                if kept == scale(count)? {
                    kept /= 10;
                    exponent += 1;
                }
                (kept, exponent)
            }
        })
    }

    fn first(&self) -> Option<(u8, i32)> {
        if self.significand == 0 {
            return None;
        }
        let places = self.significand.ilog10();
        let digit = u8::try_from(self.significand / 10u128.pow(places)).ok()?;
        Some((digit, self.exponent + i32::try_from(places).ok()?))
    }
}

#[cfg(test)]
mod tests;
