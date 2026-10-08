//! The IOM16 vertical behind a RISC-V IOMMU translating at the second stage:
//! the `vertical` module's run.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod vertical;

/// Where the run proves the unit keeps its translations: the tables of the
/// stage the kernel takes of a unit walking one.
#[cfg(itest_riscv64)]
const TABLES: tairix_itest_translation_witness::Tables =
    tairix_itest_translation_witness::Tables::Walked(
        tairix_itest_translation_witness::Stage::Second,
    );

/// How the board's interrupts arrive: on wires through the PLIC, which no
/// unit remaps.
#[cfg(itest_riscv64)]
const INTERRUPTS: tairix_itest_translation_witness::Interrupts =
    tairix_itest_translation_witness::Interrupts::Wired;

/// Where the unit's registers must lie: anywhere, the board fixing them.
#[cfg(itest_riscv64)]
const REGISTERS_FROM: u64 = 0;

#[cfg(not(itest_riscv64))]
fn main() {}
