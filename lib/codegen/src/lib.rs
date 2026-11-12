//! TAIRiX's code generator (`lib/codegen`): one SSA IR, its verifier and
//! reference interpreter, a linear-scan register allocator, and backends for
//! x86_64, aarch64 and riscv64. It is the one generator the PC emulator's
//! translator, the browser's compiled tiers and the Wasm tier share
//! (`plans/BROWSER.md` decision 6, `plans/PCEMU.md` PC18).
//!
//! A function is built with [`ir::Builder`], checked by [`verify::verify`],
//! and compiled for a [`Target`] by [`compile`] into position-independent
//! machine code following the target's C calling convention; it reaches
//! helpers through a table whose address it is given. The backend is chosen
//! from the architecture the kernel attests, never by how this crate was
//! built.
//!
//! The crate generates code; it never runs it, maps it executable or
//! touches memory it computes, so it has no `unsafe` at all.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod aarch64;
mod bits;
#[cfg(test)]
mod conformance;
pub mod interp;
pub mod ir;
mod lower;
#[cfg(any(test, feature = "test-util"))]
pub mod model;
mod moves;
mod regalloc;
mod relax;
mod riscv64;
#[cfg(any(test, feature = "test-util"))]
pub mod testgen;
mod uses;
pub mod verify;
mod x86_64;

use alloc::vec::Vec;

use tairix_abi::cpufeatures::{CpuFeature, CpuFeatureSet};
use tairix_abi::Arch;

/// Why a function could not be compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The verifier refused it.
    Verify(verify::VerifyError),
    /// Its code would outrun a branch's or an offset's reach.
    TooLarge,
    /// It reached the backend in a shape the verifier never admits.
    Malformed,
}

/// An instruction set this crate has a backend for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isa {
    /// x86_64, System V calling convention.
    X86_64,
    /// aarch64, AAPCS64 calling convention.
    Aarch64,
    /// riscv64 of the RV64GC base, LP64 calling convention.
    Riscv64,
}

impl Isa {
    /// The bits of the extensions its backend lowers to where it may.
    const fn lowered_to(self) -> u64 {
        match self {
            Self::X86_64 => 1 << CpuFeature::Bmi2.bit(),
            Self::Aarch64 => 0,
            Self::Riscv64 => 1 << CpuFeature::Zbb.bit(),
        }
    }
}

/// The machine a function is compiled for: its instruction set, and those of
/// its extensions every core the code may run on has, which the backend uses
/// where they serve — BMI2's shifts by any register and rotations by an
/// immediate on x86_64, Zbb's rotations and 16- and 8-bit extensions on
/// riscv64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    isa: Isa,
    extensions: CpuFeatureSet,
}

impl Target {
    /// x86_64 as every core of it runs.
    pub const X86_64: Self = Self::new(Isa::X86_64, CpuFeatureSet::EMPTY);
    /// aarch64 as every core of it runs.
    pub const AARCH64: Self = Self::new(Isa::Aarch64, CpuFeatureSet::EMPTY);
    /// riscv64 as every core of it runs.
    pub const RISCV64: Self = Self::new(Isa::Riscv64, CpuFeatureSet::EMPTY);

    /// Every target a backend lowers differently: each instruction set's
    /// base, and it with every extension the backend uses.
    pub const ALL: [Self; 5] = [
        Self::X86_64,
        Self::X86_64.with(CpuFeature::Bmi2),
        Self::AARCH64,
        Self::RISCV64,
        Self::RISCV64.with(CpuFeature::Zbb),
    ];

    /// `isa`, with those of `extensions` its backend lowers to.
    #[must_use]
    pub const fn new(isa: Isa, extensions: CpuFeatureSet) -> Self {
        Self {
            isa,
            extensions: CpuFeatureSet::from_bits(extensions.bits() & isa.lowered_to()),
        }
    }

    /// The target for the architecture `arch` the kernel attests, whose
    /// every core has `features`; none for one this crate has no backend
    /// for.
    #[must_use]
    pub const fn of(arch: Arch, features: CpuFeatureSet) -> Option<Self> {
        let isa = match arch {
            Arch::X86_64 => Isa::X86_64,
            Arch::Aarch64 => Isa::Aarch64,
            Arch::Riscv64 => Isa::Riscv64,
            Arch::Wasm32 => return None,
        };
        Some(Self::new(isa, features))
    }

    /// This target, its code free to use `feature` too where the backend
    /// lowers to it.
    #[must_use]
    pub const fn with(self, feature: CpuFeature) -> Self {
        Self::new(self.isa, self.extensions.with(feature))
    }

    /// Its instruction set.
    #[must_use]
    pub const fn isa(self) -> Isa {
        self.isa
    }

    /// The extensions its code may use.
    #[must_use]
    pub const fn extensions(self) -> CpuFeatureSet {
        self.extensions
    }

    /// Whether its code may use `feature`.
    #[must_use]
    pub const fn has(self, feature: CpuFeature) -> bool {
        self.extensions.contains(feature)
    }
}

/// `func` compiled for `target`: its machine code, entered at its first
/// byte with the target's C calling convention. Every argument arrives as a
/// 64-bit word, a 32-bit one zero-extended; a 32-bit answer is returned
/// zero-extended; a helper is called the same way and answers in a word,
/// a 32-bit answer in its low half.
///
/// Compiling costs time linear in the function's size and its liveness: a
/// caller bounds what it hands in.
///
/// # Errors
///
/// Why it could not be.
pub fn compile(func: &ir::Function, target: Target) -> Result<Vec<u8>, CompileError> {
    verify::verify(func).map_err(CompileError::Verify)?;
    match target.isa {
        Isa::X86_64 => x86_64::build(func, target).map(|(_, bytes)| bytes),
        Isa::Aarch64 => aarch64::build(func).map(|(_, bytes)| bytes),
        Isa::Riscv64 => riscv64::build(func, target).map(|(_, bytes)| bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_keeps_only_the_extensions_its_backend_lowers_to() {
        let every = CpuFeature::ALL
            .iter()
            .fold(CpuFeatureSet::EMPTY, |set, &feature| set.with(feature));
        let x86 = Target::new(Isa::X86_64, every);
        assert_eq!(x86, Target::X86_64.with(CpuFeature::Bmi2));
        assert!(x86.has(CpuFeature::Bmi2) && !x86.has(CpuFeature::Avx2));
        assert_eq!(Target::new(Isa::Aarch64, every), Target::AARCH64);
        assert_eq!(
            Target::new(Isa::Riscv64, every),
            Target::RISCV64.with(CpuFeature::Zbb)
        );
        assert_eq!(
            Target::X86_64.with(CpuFeature::Zbb),
            Target::X86_64,
            "another instruction set's extension"
        );
    }

    /// `ALL` names each set of extensions a backend lowers to exactly once,
    /// so a lowering added for one is held to the conformance suite.
    #[test]
    fn every_lowered_extension_set_is_a_target() {
        for isa in [Isa::X86_64, Isa::Aarch64, Isa::Riscv64] {
            let lowered = isa.lowered_to();
            let mut subset = lowered;
            loop {
                let target = Target::new(isa, CpuFeatureSet::from_bits(subset));
                let named = Target::ALL.iter().filter(|&&t| t == target).count();
                assert_eq!(named, 1, "{target:?}");
                if subset == 0 {
                    break;
                }
                subset = (subset - 1) & lowered;
            }
        }
        let subsets: u32 = [Isa::X86_64, Isa::Aarch64, Isa::Riscv64]
            .iter()
            .map(|isa| 1 << isa.lowered_to().count_ones())
            .sum();
        assert_eq!(Target::ALL.len(), subsets as usize);
    }

    #[test]
    fn the_attested_architecture_chooses_the_backend() {
        let features = CpuFeatureSet::EMPTY.with(CpuFeature::Zbb);
        assert_eq!(
            Target::of(Arch::Riscv64, features),
            Some(Target::RISCV64.with(CpuFeature::Zbb))
        );
        assert_eq!(Target::of(Arch::X86_64, features), Some(Target::X86_64));
        assert_eq!(Target::of(Arch::Aarch64, features), Some(Target::AARCH64));
        assert_eq!(Target::of(Arch::Wasm32, features), None);
    }
}
