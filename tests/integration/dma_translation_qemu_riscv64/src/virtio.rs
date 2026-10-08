//! The MI3 vertical behind a `virtio-iommu-pci` on the ECAM host: the
//! `vertical` module's run.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod vertical;

/// Where the run proves the unit keeps its translations: itself.
#[cfg(itest_riscv64)]
const TABLES: tairix_itest_translation_witness::Tables =
    tairix_itest_translation_witness::Tables::Kept;

/// How the board's interrupts arrive: on wires through the PLIC, which no
/// unit remaps.
#[cfg(itest_riscv64)]
const INTERRUPTS: tairix_itest_translation_witness::Interrupts =
    tairix_itest_translation_witness::Interrupts::Wired;

/// Where the unit's registers must lie: past 4 GiB, in the host's 64-bit
/// window, which only the kernel half's device leaves reach.
#[cfg(itest_riscv64)]
const REGISTERS_FROM: u64 = 1 << 32;

#[cfg(not(itest_riscv64))]
fn main() {}
