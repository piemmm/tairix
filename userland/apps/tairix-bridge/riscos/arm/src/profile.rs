//! Profiles: each fixes every choice the architecture leaves to an
//! implementation (`plans/RISCOS-EMULATOR.md` §2.1).

use crate::isa::{
    Feature::{
        ConditionNever, Crc32, Crypto, Divide, FpV8, Fpa, Jazelle, LongMultiply, Modes26, Mp, Simd,
        Swp, Vfp, V2, V3, V4, V4T, V5, V5TE, V6, V6K, V6T2, V7, V8,
    },
    Isa,
};
use crate::psr;

/// What an instruction the architecture leaves UNPREDICTABLE does.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Unpredictable {
    /// What an implementation without a pipeline does, as the cores that
    /// software for the profile ran on did: an `STM` that writes back a base
    /// it lists after its first register stores the written-back base, as
    /// the ARM6 and ARM7 data sheets give it.
    Execute,
    /// It takes the undefined instruction exception, as the architecture
    /// permits, so a program that relies on one fails loudly.
    Undefined,
}

/// The system control coprocessor a profile has.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum System {
    /// The SA-110's.
    Sa110,
    /// An ARMv7-A processor's, configured as RISC OS 5 configures it.
    Armv7,
}

/// A processor's every implementation-defined choice.
#[derive(Debug)]
pub struct Profile {
    /// The profile's name.
    pub name: &'static str,
    /// The instructions it decodes.
    pub isa: Isa,
    /// What `STR` and `STM` of R15 store: the instruction's address plus
    /// this.
    pub stored_pc: u32,
    /// Whether a misaligned access faults; otherwise a word load rotates and
    /// a store ignores the low bits, as ARMv4 cores did.
    pub alignment_faults: bool,
    /// Where the exception vectors are.
    pub vectors: u32,
    /// What an UNPREDICTABLE instruction does.
    pub unpredictable: Unpredictable,
    /// The PSR bits the processor has; the rest read as zero.
    pub psr: u32,
    /// The CPSR bits an `MSR` in User mode writes.
    pub psr_user: u32,
    /// The CPSR bits an `MSR` in a privileged mode writes.
    pub psr_privileged: u32,
    /// The CPSR bits an exception return writes.
    pub psr_return: u32,
    /// Its system control coprocessor.
    pub system: System,
}

impl Profile {
    /// Whether the profile has the 26-bit modes.
    #[must_use]
    pub const fn modes26(&self) -> bool {
        self.isa.has(Modes26)
    }
}

/// ARMv4 with the 26-bit modes, as the SA-110 has them: the profile classic
/// software runs on.
pub static BIT26: Profile = Profile {
    name: "26-bit",
    isa: Isa::of(&[V2, Swp, V3, LongMultiply, V4, ConditionNever, Modes26, Fpa]),
    stored_pc: 8,
    alignment_faults: false,
    vectors: 0,
    unpredictable: Unpredictable::Execute,
    psr: psr::NZCV | psr::I | psr::F | psr::M,
    psr_user: psr::NZCV,
    psr_privileged: psr::NZCV | psr::I | psr::F | psr::M,
    psr_return: psr::NZCV | psr::I | psr::F | psr::M,
    system: System::Sa110,
};

/// ARMv7-A with ARMv8-A's AArch32 additions, configured as RISC OS 5
/// configures it: alignment faults on, the vectors high.
pub static BIT32: Profile = Profile {
    name: "32-bit",
    isa: Isa::of(&[
        V2,
        V3,
        LongMultiply,
        V4,
        V4T,
        V5,
        V5TE,
        Jazelle,
        V6,
        V6K,
        V6T2,
        V7,
        Mp,
        Divide,
        V8,
        Crc32,
        Fpa,
        Vfp,
        Simd,
        FpV8,
        Crypto,
    ]),
    stored_pc: 8,
    alignment_faults: true,
    vectors: 0xFFFF_0000,
    unpredictable: Unpredictable::Undefined,
    psr: psr::NZCV
        | psr::Q
        | psr::IT_LOW
        | psr::GE
        | psr::IT_HIGH
        | psr::E
        | psr::A
        | psr::I
        | psr::F
        | psr::T
        | psr::M,
    psr_user: psr::NZCV | psr::Q | psr::GE | psr::E,
    psr_privileged: psr::NZCV | psr::Q | psr::GE | psr::E | psr::A | psr::I | psr::F | psr::M,
    psr_return: psr::NZCV
        | psr::Q
        | psr::IT_LOW
        | psr::GE
        | psr::IT_HIGH
        | psr::E
        | psr::A
        | psr::I
        | psr::F
        | psr::T
        | psr::M,
    system: System::Armv7,
};
