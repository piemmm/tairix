//! The IOM16 vertical behind a RISC-V IOMMU translating at the second stage,
//! on a `virt` board whose interrupts are an APLIC and the hart's IMSIC file
//! (`plans/IOMMU.md` IOM18.3, IOM18.4): the keyboard's MSI-X lands in a
//! memory-resident interrupt file of its own, whose notice reaches its driver.

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

/// How the board's interrupts arrive: each PCI function's messages confined
/// to its own file, the unit raising its notice in the hart's.
#[cfg(itest_riscv64)]
const INTERRUPTS: tairix_itest_translation_witness::Interrupts =
    tairix_itest_translation_witness::Interrupts::Remapped {
        extended: the_hart_takes_every_notice,
    };

/// The hart's file takes every identity the controller gives a notice: there
/// is no mode it must enter first.
#[cfg(itest_riscv64)]
fn the_hart_takes_every_notice() -> bool {
    true
}

/// Where the unit's registers must lie: anywhere, the board fixing them.
#[cfg(itest_riscv64)]
const REGISTERS_FROM: u64 = 0;

#[cfg(not(itest_riscv64))]
fn main() {}
