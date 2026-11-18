//! VFP and Advanced SIMD: the register file both share, the FPSCR and
//! FPEXC, the ID registers `VMRS` reads, and their arithmetic.
//!
//! The 32 doubleword registers are also the 16 quadword registers Advanced
//! SIMD names, and the first 16 the 32 single-word registers VFP's single
//! precision names. Every floating-point result is `lib/softfloat`'s, rounded
//! as the FPSCR says, with ARM's rules for NaNs, flush-to-zero and the
//! alternative half-precision format; Advanced SIMD's floating point uses the
//! architecture's standard controls instead.

pub(crate) mod crypto;
mod float;
pub(crate) mod simd;

pub use float::{Comparison, Unit};

use crate::isa::{Feature, Isa};

/// The FPSCR's fields.
pub mod fpscr {
    /// The comparison flags, in bits 31 to 28.
    pub const NZCV: u32 = 0xF << 28;
    /// Advanced SIMD's cumulative saturation flag.
    pub const QC: u32 = 1 << 27;
    /// The alternative half-precision format.
    pub const AHP: u32 = 1 << 26;
    /// Default NaN mode.
    pub const DN: u32 = 1 << 25;
    /// Flush-to-zero mode.
    pub const FZ: u32 = 1 << 24;
    /// The rounding mode's place: nearest, plus infinity, minus infinity,
    /// zero.
    pub const RMODE_SHIFT: u32 = 22;
    /// The short vectors' stride, less one, and its place.
    pub const STRIDE_SHIFT: u32 = 20;
    /// The short vectors' length, less one, and its place.
    pub const LEN_SHIFT: u32 = 16;
    /// The short vectors' length and stride fields.
    pub const VECTOR: u32 = 0x0037_0000;
    /// The input-denormal cumulative flag.
    pub const IDC: u32 = 1 << 7;
    /// The inexact cumulative flag.
    pub const IXC: u32 = 1 << 4;
    /// The underflow cumulative flag.
    pub const UFC: u32 = 1 << 3;
    /// The overflow cumulative flag.
    pub const OFC: u32 = 1 << 2;
    /// The division-by-zero cumulative flag.
    pub const DZC: u32 = 1 << 1;
    /// The invalid-operation cumulative flag.
    pub const IOC: u32 = 1;
    /// Every cumulative exception flag.
    pub const FLAGS: u32 = IDC | IXC | UFC | OFC | DZC | IOC;
    /// What `VMSR` writes: no exception traps, which no profile has.
    pub const WRITABLE: u32 = NZCV | QC | AHP | DN | FZ | 3 << RMODE_SHIFT | VECTOR | FLAGS;
}

/// FPEXC's fields.
pub mod fpexc {
    /// VFP and Advanced SIMD enabled.
    pub const EN: u32 = 1 << 30;
}

/// A system register `VMRS` and `VMSR` reach, by its number.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum System {
    /// FPSID: the unit's identity.
    Fpsid,
    /// FPSCR: the status and control register.
    Fpscr,
    /// MVFR2: ARMv8's miscellaneous features.
    Mvfr2,
    /// MVFR1: Advanced SIMD and half precision.
    Mvfr1,
    /// MVFR0: the register file and the operations.
    Mvfr0,
    /// FPEXC: the enable.
    Fpexc,
}

impl System {
    /// The register `VMRS` and `VMSR` name by `number`, if any is one.
    #[must_use]
    pub const fn of(number: u8) -> Option<Self> {
        Some(match number {
            0 => Self::Fpsid,
            1 => Self::Fpscr,
            5 => Self::Mvfr2,
            6 => Self::Mvfr1,
            7 => Self::Mvfr0,
            8 => Self::Fpexc,
            _ => return None,
        })
    }

    /// Its number.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Fpsid => 0,
            Self::Fpscr => 1,
            Self::Mvfr2 => 5,
            Self::Mvfr1 => 6,
            Self::Mvfr0 => 7,
            Self::Fpexc => 8,
        }
    }

    /// Its name, as `VMRS` and `VMSR` spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fpsid => "FPSID",
            Self::Fpscr => "FPSCR",
            Self::Mvfr2 => "MVFR2",
            Self::Mvfr1 => "MVFR1",
            Self::Mvfr0 => "MVFR0",
            Self::Fpexc => "FPEXC",
        }
    }

    /// The value an identification register holds on a processor with
    /// `isa`: a unit with an implementer of zero, as CP15 reports, that
    /// implements every feature the set has and traps no exception.
    #[must_use]
    pub const fn identity(self, isa: Isa) -> u32 {
        let simd = isa.has(Feature::Simd);
        let v8 = isa.has(Feature::FpV8);
        match self {
            // The common VFP subarchitecture, version 2.
            Self::Fpsid => 0x0003_0000,
            // Thirty-two doublewords, single and double precision, divide,
            // square root, short vectors and every rounding mode.
            Self::Mvfr0 => 0x1111_0222,
            // Denormals and NaN propagation, half-precision conversion,
            // fused multiply-add; Advanced SIMD's loads, integers, single
            // precision and half-precision conversion.
            Self::Mvfr1 => {
                0x1000_0011
                    | if v8 { 0x0300_0000 } else { 0x0200_0000 }
                    | if simd { 0x0011_1100 } else { 0 }
            }
            // VSEL to every rounding to an integral value, in either unit.
            Self::Mvfr2 => {
                if v8 {
                    0x40 | if simd { 0x3 } else { 0 }
                } else {
                    0
                }
            }
            Self::Fpscr | Self::Fpexc => 0,
        }
    }
}

/// VFP's eight-bit immediate expanded to a single or double: the sign, an
/// exponent of -3 to 4 and four fraction bits, as `VFPExpandImm` gives them.
#[must_use]
pub const fn expand_immediate(imm: u8, double: bool) -> u64 {
    let sign = (imm >> 7) as u64;
    let b = ((imm >> 6) & 1) as u64;
    let cd = ((imm >> 4) & 3) as u64;
    let fraction = (imm & 15) as u64;
    if double {
        let exponent = (b ^ 1) << 10 | (if b == 1 { 0xFF } else { 0 }) << 2 | cd;
        sign << 63 | exponent << 52 | fraction << 48
    } else {
        let exponent = (b ^ 1) << 7 | (if b == 1 { 0x1F } else { 0 }) << 2 | cd;
        sign << 31 | exponent << 23 | fraction << 19
    }
}

/// The register file and its two control registers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Vfp {
    d: [u64; 32],
    fpscr: u32,
    fpexc: u32,
}

impl Default for Vfp {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfp {
    /// At reset: the registers zero, the unit disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            d: [0; 32],
            fpscr: 0,
            fpexc: 0,
        }
    }

    /// Single-word register `n` of 32: a half of a doubleword below D16.
    #[must_use]
    pub const fn s(&self, n: u8) -> u32 {
        let d = self.d[((n & 31) >> 1) as usize];
        if n & 1 == 0 {
            crate::word::low(d)
        } else {
            crate::word::high(d)
        }
    }

    /// Sets single-word register `n`.
    pub fn set_s(&mut self, n: u8, value: u32) {
        let slot = &mut self.d[usize::from((n & 31) >> 1)];
        *slot = if n & 1 == 0 {
            (*slot & !0xFFFF_FFFF) | u64::from(value)
        } else {
            (*slot & 0xFFFF_FFFF) | u64::from(value) << 32
        };
    }

    /// Doubleword register `n` of 32.
    #[must_use]
    pub const fn d(&self, n: u8) -> u64 {
        self.d[(n & 31) as usize]
    }

    /// Sets doubleword register `n`.
    pub fn set_d(&mut self, n: u8, value: u64) {
        self.d[usize::from(n & 31)] = value;
    }

    /// Quadword register `n` of 16: D`2n` in its low half.
    #[must_use]
    pub const fn q(&self, n: u8) -> u128 {
        let low = (n & 15) << 1;
        (self.d[(low + 1) as usize] as u128) << 64 | self.d[low as usize] as u128
    }

    /// Sets quadword register `n`.
    pub fn set_q(&mut self, n: u8, value: u128) {
        let low = usize::from((n & 15) << 1);
        self.d[low] = crate::word::low64(value);
        self.d[low + 1] = crate::word::low64(value >> 64);
    }

    /// The FPSCR.
    #[must_use]
    pub const fn fpscr(&self) -> u32 {
        self.fpscr
    }

    /// Writes the FPSCR, as `VMSR` does: the trap enables are not writable.
    pub fn set_fpscr(&mut self, value: u32) {
        self.fpscr = value & fpscr::WRITABLE;
    }

    /// Sets the cumulative flags in `flags`.
    pub(crate) fn raise(&mut self, flags: u32) {
        self.fpscr |= flags & (fpscr::FLAGS | fpscr::QC);
    }

    /// Sets the comparison flags to `nzcv`, in bits 31 to 28.
    pub(crate) fn set_nzcv(&mut self, nzcv: u32) {
        self.fpscr = (self.fpscr & !fpscr::NZCV) | (nzcv & fpscr::NZCV);
    }

    /// FPEXC.
    #[must_use]
    pub const fn fpexc(&self) -> u32 {
        self.fpexc
    }

    /// Writes FPEXC, as `VMSR` does: only the enable is writable.
    pub fn set_fpexc(&mut self, value: u32) {
        self.fpexc = value & fpexc::EN;
    }

    /// Whether the unit is enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.fpexc & fpexc::EN != 0
    }

    /// The short vectors' length and stride the FPSCR sets: one and one where
    /// it sets none, and `None` for a stride the architecture leaves
    /// UNPREDICTABLE.
    #[must_use]
    pub const fn vector(&self) -> Option<(u8, u8)> {
        let len = ((self.fpscr >> fpscr::LEN_SHIFT) & 7) as u8 + 1;
        match (self.fpscr >> fpscr::STRIDE_SHIFT) & 3 {
            0 => Some((len, 1)),
            3 => Some((len, 2)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
