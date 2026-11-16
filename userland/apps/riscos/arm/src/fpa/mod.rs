//! The FPA: the floating-point coprocessor RISC OS programs reach through
//! coprocessors 1 and 2, answering as FPEmulator 4 answers.
//!
//! RISC OS 5 runs no FPA hardware: FPEmulator, the module that takes FPA
//! instructions at the undefined instruction trap, defines what its software
//! sees. So each result follows the FPA's data sheets, with FPEmulator's
//! choices among IEEE 754-1985's options, observed black-box:
//!
//! - registers hold FPEmulator's internal format ([`Internal`]), which keeps
//!   a single or double precision denormal, infinity or NaN in its own
//!   precision's form until an operation converts it;
//! - an underflow is tiny after rounding and lossy by denormalisation;
//! - an invalid operation's NaN is an extended one carrying its reason, and an
//!   overflow's infinity is the extended infinity whatever the precision;
//! - an enabled exception writes nothing: the instruction stops with it, its
//!   cumulative flag clear, for the session to raise as FPEmulator does.
//!
//! Where FPEmulator is inexact the FPA is not: a transcendental value and a
//! packed decimal conversion are rounded correctly.
//!
//! Where the data sheet leaves an operand's meaning undefined, the operand is
//! taken at its value: a `URD` result anywhere but `NRM`, and the marker of a
//! single or double denormal with no fraction, a pattern only `LFM` of
//! hand-made words loads, which is zero.
//!
//! The arithmetic is `lib/softfloat`'s.

mod arith;
mod internal;
mod packed;
mod trans;
mod transfer;

#[cfg(test)]
mod tests;

pub use internal::Internal;

use crate::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaOperand};

/// An arithmetic result's precision.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Precision {
    /// Single precision: 24 significand bits.
    Single,
    /// Double precision: 53.
    Double,
    /// Extended precision: 64.
    Extended,
}

impl Precision {
    /// Every precision, in encoding order.
    pub const ALL: [Self; 3] = [Self::Single, Self::Double, Self::Extended];

    /// The significand bits kept, the integer bit counted.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::Single => 24,
            Self::Double => 53,
            Self::Extended => 64,
        }
    }

    /// The least biased exponent of a normalised number.
    const fn min_exponent(self) -> i32 {
        match self {
            Self::Single => 0x3F81,
            Self::Double => 0x3C01,
            Self::Extended => 0,
        }
    }

    /// The greatest biased exponent of a finite number.
    const fn max_exponent(self) -> i32 {
        match self {
            Self::Single => 0x407E,
            Self::Double => 0x43FE,
            Self::Extended => 0x7FFE,
        }
    }

    /// The exponent an infinity or a NaN of the precision holds.
    const fn special_exponent(self) -> u32 {
        match self {
            Self::Single => 0x407F,
            Self::Double => 0x43FF,
            Self::Extended => 0x7FFF,
        }
    }
}

/// A transfer's memory format.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Format {
    /// IEEE 754 single precision, one word.
    Single,
    /// IEEE 754 double precision, two words, the sign and exponent first.
    Double,
    /// The FPA's extended precision, three words.
    Extended,
    /// Packed decimal: three words, or four expanded.
    Packed,
}

impl Format {
    /// Every format, in encoding order.
    pub const ALL: [Self; 4] = [Self::Single, Self::Double, Self::Extended, Self::Packed];

    /// The words the format occupies, packed decimal's as `expanded` says.
    #[must_use]
    pub const fn words(self, expanded: bool) -> usize {
        match self {
            Self::Single => 1,
            Self::Double => 2,
            Self::Extended => 3,
            Self::Packed => {
                if expanded {
                    4
                } else {
                    3
                }
            }
        }
    }
}

/// A rounding mode an arithmetic instruction names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Rounding {
    /// To nearest, ties to even.
    Nearest,
    /// Toward plus infinity.
    PlusInfinity,
    /// Toward minus infinity.
    MinusInfinity,
    /// Toward zero.
    Zero,
}

impl Rounding {
    /// Every mode, in encoding order.
    pub const ALL: [Self; 4] = [
        Self::Nearest,
        Self::PlusInfinity,
        Self::MinusInfinity,
        Self::Zero,
    ];

    const fn mode(self) -> tairix_softfloat::Mode {
        match self {
            Self::Nearest => tairix_softfloat::Mode::Nearest,
            Self::PlusInfinity => tairix_softfloat::Mode::Up,
            Self::MinusInfinity => tairix_softfloat::Mode::Down,
            Self::Zero => tairix_softfloat::Mode::Zero,
        }
    }
}

/// One of IEEE 754's five exceptions, in the FPSR's bit order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Exception {
    /// An operation with no meaningful result.
    InvalidOperation,
    /// A finite number divided by zero.
    DivisionByZero,
    /// A rounded result too large for its precision.
    Overflow,
    /// A tiny result.
    Underflow,
    /// A rounded result.
    Inexact,
}

impl Exception {
    /// Every exception, in the FPSR's bit order.
    pub const ALL: [Self; 5] = [
        Self::InvalidOperation,
        Self::DivisionByZero,
        Self::Overflow,
        Self::Underflow,
        Self::Inexact,
    ];

    /// The cumulative flag an untrapped occurrence sets.
    #[must_use]
    pub const fn flag(self) -> u32 {
        1 << self as u32
    }

    /// The trap enable that makes an occurrence stop the instruction.
    #[must_use]
    pub const fn enable(self) -> u32 {
        self.flag() << 16
    }
}

/// The FPSR's fields.
pub mod fpsr {
    /// The cumulative exception flags, by [`super::Exception::flag`].
    pub const FLAGS: u32 = 0x1F;
    /// No denormalised numbers: a tiny result flushes to zero.
    pub const ND: u32 = 1 << 8;
    /// NaN exceptions: every precision change of a signalling NaN is invalid.
    /// Clear, an extended NaN's lowest bit says which of single and double it
    /// stands for.
    pub const NE: u32 = 1 << 9;
    /// Expanded packed decimal: `LDFP` and `STFP` move four words.
    pub const EP: u32 = 1 << 11;
    /// Alternative comparison flags: unordered sets C as well as V.
    pub const AC: u32 = 1 << 12;
    /// The trap enables, by [`super::Exception::enable`].
    pub const ENABLES: u32 = 0x1F << 16;
    /// What `WFS` writes: everything below the system ID.
    pub const WRITABLE: u32 = 0x00FF_FFFF;
    /// The system ID, in bits 31 to 24: FPEmulator 4's.
    pub const SYSTEM_ID: u32 = 0x01 << 24;
}

/// The FPA's state: eight registers and the FPSR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fpa {
    registers: [Internal; 8],
    fpsr: u32,
}

impl Default for Fpa {
    fn default() -> Self {
        Self::new()
    }
}

impl Fpa {
    /// Registers zero, every exception untrapped, no flag set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            registers: [Internal::ZERO; 8],
            fpsr: fpsr::SYSTEM_ID,
        }
    }

    /// The FPSR, as `RFS` reads it.
    #[must_use]
    pub const fn fpsr(&self) -> u32 {
        self.fpsr
    }

    /// Writes the FPSR, as `WFS` does: the system ID is fixed.
    pub fn write_fpsr(&mut self, value: u32) {
        self.fpsr = fpsr::SYSTEM_ID | (value & fpsr::WRITABLE);
    }

    /// Register `n`, of eight.
    #[must_use]
    pub const fn register(&self, n: u8) -> Internal {
        self.registers[(n & 7) as usize]
    }

    /// Sets register `n` to `value` as it stands, as `SFM` stores it back.
    pub fn set_register(&mut self, n: u8, value: Internal) {
        self.registers[(n & 7) as usize] = value;
    }

    fn operand(&self, operand: FpaOperand) -> Internal {
        match operand {
            FpaOperand::Register(n) => self.register(n),
            FpaOperand::Constant(n) => Internal::constant(n),
        }
    }

    /// A dyadic operation: `fd = fn op fm`.
    ///
    /// # Errors
    ///
    /// The exception an enabled trap stopped it with; nothing is written but
    /// the flags of exceptions raised before it.
    pub fn dyadic(
        &mut self,
        op: FpaDyadicOp,
        precision: Precision,
        rounding: Rounding,
        fd: u8,
        fn_: u8,
        fm: FpaOperand,
    ) -> Result<(), Exception> {
        let (a, b) = (self.register(fn_), self.operand(fm));
        let mut unit = arith::Unit::new(self.fpsr, precision, rounding);
        let result = unit.dyadic(op, a, b);
        self.fpsr = unit.fpsr;
        self.set_register(fd, result?);
        Ok(())
    }

    /// A monadic operation: `fd = op fm`.
    ///
    /// # Errors
    ///
    /// The exception an enabled trap stopped it with; nothing is written but
    /// the flags of exceptions raised before it.
    pub fn monadic(
        &mut self,
        op: FpaMonadicOp,
        precision: Precision,
        rounding: Rounding,
        fd: u8,
        fm: FpaOperand,
    ) -> Result<(), Exception> {
        let a = self.operand(fm);
        let mut unit = arith::Unit::new(self.fpsr, precision, rounding);
        let result = unit.monadic(op, a);
        self.fpsr = unit.fpsr;
        self.set_register(fd, result?);
        Ok(())
    }

    /// `FLT`: `fn` set to the signed integer `value`.
    ///
    /// # Errors
    ///
    /// The exception an enabled trap stopped it with; nothing is written but
    /// the flags of exceptions raised before it.
    pub fn float(
        &mut self,
        precision: Precision,
        rounding: Rounding,
        fn_: u8,
        value: u32,
    ) -> Result<(), Exception> {
        let mut unit = arith::Unit::new(self.fpsr, precision, rounding);
        let result = unit.float(value);
        self.fpsr = unit.fpsr;
        self.set_register(fn_, result?);
        Ok(())
    }

    /// `FIX`: `fm` rounded to a signed integer.
    ///
    /// # Errors
    ///
    /// The exception an enabled trap stopped it with.
    pub fn fix(&mut self, rounding: Rounding, fm: FpaOperand) -> Result<u32, Exception> {
        let a = self.operand(fm);
        let mut unit = arith::Unit::new(self.fpsr, Precision::Extended, rounding);
        let value = unit.fix(a);
        self.fpsr = unit.fpsr;
        value
    }

    /// A comparison: the flags it sets, in bits 31 to 28.
    ///
    /// # Errors
    ///
    /// The exception an enabled trap stopped it with.
    pub fn compare(&mut self, op: FpaCompareOp, fn_: u8, fm: FpaOperand) -> Result<u32, Exception> {
        let (a, b) = (self.register(fn_), self.operand(fm));
        let mut unit = arith::Unit::new(self.fpsr, Precision::Extended, Rounding::Nearest);
        let flags = unit.compare(op, a, b);
        self.fpsr = unit.fpsr;
        flags
    }

    /// The words a transfer of `format` moves: packed decimal's by the
    /// FPSR's EP bit.
    #[must_use]
    pub const fn transfer_words(&self, format: Format) -> usize {
        format.words(self.fpsr & fpsr::EP != 0)
    }

    /// `LDF`: `fd` loaded from `words`, [`Fpa::transfer_words`] of them. A
    /// load of single, double or extended precision raises nothing.
    ///
    /// # Errors
    ///
    /// The exception an enabled trap stopped a packed load with.
    pub fn load(&mut self, format: Format, fd: u8, words: [u32; 4]) -> Result<(), Exception> {
        let value = match format {
            Format::Single => transfer::load_single(words[0]),
            Format::Double => transfer::load_double([words[0], words[1]]),
            Format::Extended => transfer::load_extended([words[0], words[1], words[2]]),
            Format::Packed => {
                let mut unit = arith::Unit::new(self.fpsr, Precision::Extended, Rounding::Nearest);
                let value = unit.load_packed(words);
                self.fpsr = unit.fpsr;
                value?
            }
        };
        self.set_register(fd, value);
        Ok(())
    }

    /// `STF`: `fd` as [`Fpa::transfer_words`] words, the rest zero.
    ///
    /// # Errors
    ///
    /// The exception an enabled trap stopped it with; nothing is stored.
    pub fn store(&mut self, format: Format, fd: u8) -> Result<[u32; 4], Exception> {
        let value = self.register(fd);
        let mut unit = arith::Unit::new(self.fpsr, Precision::Extended, Rounding::Nearest);
        let words = match format {
            Format::Single => unit.store_single(value).map(|word| [word, 0, 0, 0]),
            Format::Double => unit
                .store_double(value)
                .map(|[high, low]| [high, low, 0, 0]),
            Format::Extended => unit.store_extended(value).map(|[a, b, c]| [a, b, c, 0]),
            Format::Packed => unit.store_packed(value),
        };
        self.fpsr = unit.fpsr;
        words
    }

    /// `LFM`: `fd` set from three words in the internal format, any pattern
    /// FPEmulator would take as uncommon marked so.
    pub fn load_multiple(&mut self, fd: u8, words: [u32; 3]) {
        self.set_register(fd, transfer::load_internal(words));
    }

    /// `SFM`: `fd`'s three words in the internal format.
    #[must_use]
    pub const fn store_multiple(&self, fd: u8) -> [u32; 3] {
        self.register(fd).words()
    }
}
