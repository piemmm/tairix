//! The architecture features an instruction needs and a profile has.

/// An architecture feature: the set of instructions one architecture
/// version or extension added, or a rule of the encoding space.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Feature {
    /// ARMv2: data processing, `MUL`/`MLA`, single and block transfers,
    /// branches, `SWI` and the coprocessor instructions.
    V2,
    /// `SWP` and `SWPB`, from ARMv2a until ARMv8 removed them.
    Swp,
    /// ARMv3: `MRS`, `MSR` and the 32-bit modes.
    V3,
    /// The long multiplies (ARMv3M, and every ARMv4 core here).
    LongMultiply,
    /// ARMv4: the halfword and signed-byte transfers and System mode.
    V4,
    /// ARMv4T: `BX`.
    V4T,
    /// ARMv5T: `BLX`, `CLZ`, `BKPT` and the unconditional coprocessor
    /// instructions.
    V5,
    /// ARMv5TE: the saturating and halfword multiplies, `LDRD`, `STRD`,
    /// `PLD`, `MCRR` and `MRRC`.
    V5TE,
    /// `BXJ`, which with no Jazelle hardware behaves as `BX` (ARMv5TEJ, and
    /// every ARMv6 and ARMv7 core).
    Jazelle,
    /// ARMv6: the media instructions, the reversals, `CPS`, `SETEND`, `SRS`,
    /// `RFE`, `UMAAL`, `MCRR2`, `MRRC2`, `LDREX` and `STREX`.
    V6,
    /// ARMv6K: `CLREX`, the byte, halfword and doubleword exclusives and the
    /// first hints.
    V6K,
    /// ARMv6T2: `MOVW`, `MOVT`, the bitfield instructions, `RBIT`, `MLS` and
    /// the unprivileged halfword and signed transfers.
    V6T2,
    /// ARMv7: the barriers, `PLI` and `DBG`.
    V7,
    /// The multiprocessing extensions: `PLDW`.
    Mp,
    /// `SDIV` and `UDIV` in ARM state.
    Divide,
    /// ARMv8's AArch32 additions: load-acquire and store-release, `SEVL` and
    /// `CSDB`.
    V8,
    /// ARMv8's `CRC32` and `CRC32C` instructions.
    Crc32,
    /// The FPA on coprocessors 1 and 2, as FPEmulator implements it: its
    /// data operations, register transfers, loads and stores.
    Fpa,
    /// VFPv4 with 32 doubleword registers on coprocessors 10 and 11: single
    /// and double precision, fused multiply-add, conversions to and from
    /// half precision, and VFPv2's short vectors.
    Vfp,
    /// Advanced SIMD with its fused multiply-add and half-precision
    /// conversion.
    Simd,
    /// ARMv8's floating-point additions to both: `VSEL`, `VMAXNM` and
    /// `VMINNM`, conversions and roundings in a named mode, and conversions
    /// between half and double precision.
    FpV8,
    /// ARMv8's Cryptographic Extension: AES, SHA-1, SHA-256 and the 64-bit
    /// polynomial multiply.
    Crypto,
    /// A condition field of `1111` means never, as on every core before
    /// ARMv5; without it that field marks the unconditional instructions.
    ConditionNever,
    /// The 26-bit modes, whose R15 holds the flags, I, F and the mode beside
    /// the program counter, and the comparisons' `P` forms that write them.
    Modes26,
}

impl Feature {
    const fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// A set of features: the instruction set a profile decodes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Isa(u32);

impl Isa {
    /// Whether the architecture reserves coprocessor `number`'s low four
    /// bits for a unit the set has, so that no generic coprocessor form
    /// decodes there: VFP and Advanced SIMD's 10 and 11. The FPA claims only
    /// the encodings FPEmulator implements, by its rows.
    #[must_use]
    pub const fn claims_coprocessor(self, number: u32) -> bool {
        matches!(number & 15, 10 | 11) && self.has(Feature::Vfp)
    }

    /// A set of `features`.
    #[must_use]
    pub const fn of(features: &[Feature]) -> Self {
        let mut bits = 0;
        let mut n = 0;
        while n < features.len() {
            bits |= features[n].bit();
            n += 1;
        }
        Self(bits)
    }

    /// Whether the set has `feature`.
    #[must_use]
    pub const fn has(self, feature: Feature) -> bool {
        self.0 & feature.bit() != 0
    }

    /// The set with `feature` added.
    #[must_use]
    pub const fn with(self, feature: Feature) -> Self {
        Self(self.0 | feature.bit())
    }

    /// The set without `feature`.
    #[must_use]
    pub const fn without(self, feature: Feature) -> Self {
        Self(self.0 & !feature.bit())
    }

    /// Every instruction any profile decodes, the unconditional space
    /// included: the set an encoding is checked against.
    pub const ALL: Self = Self::of(&[
        Feature::V2,
        Feature::Swp,
        Feature::V3,
        Feature::LongMultiply,
        Feature::V4,
        Feature::V4T,
        Feature::V5,
        Feature::V5TE,
        Feature::Jazelle,
        Feature::V6,
        Feature::V6K,
        Feature::V6T2,
        Feature::V7,
        Feature::Mp,
        Feature::Divide,
        Feature::V8,
        Feature::Crc32,
        Feature::Fpa,
        Feature::Vfp,
        Feature::Simd,
        Feature::FpV8,
        Feature::Crypto,
    ]);

    /// The architecture version, for the rules that change between versions.
    #[must_use]
    pub const fn version(self) -> u32 {
        if self.has(Feature::V8) {
            8
        } else if self.has(Feature::V7) {
            7
        } else if self.has(Feature::V6) {
            6
        } else if self.has(Feature::V5) {
            5
        } else if self.has(Feature::V4) {
            4
        } else if self.has(Feature::V3) {
            3
        } else {
            2
        }
    }
}
