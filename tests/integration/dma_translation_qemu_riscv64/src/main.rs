//! The IOM16 vertical behind a RISC-V IOMMU translating at the second stage:
//! the `vertical` module's run.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod vertical;

/// The stage the run proves: the stage the kernel takes of a unit walking one.
#[cfg(itest_riscv64)]
const STAGE: tairix_itest_translation_witness::Stage =
    tairix_itest_translation_witness::Stage::Second;

#[cfg(not(itest_riscv64))]
fn main() {}
