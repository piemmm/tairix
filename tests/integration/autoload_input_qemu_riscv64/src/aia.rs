//! The riscv64 autoload-input vertical on a `virt` board whose interrupts
//! are an APLIC delivering by MSI to the hart's IMSIC file (`plans/IOMMU.md`
//! IOM18.3): the keyboard's wired line reaches its driver as an identity of
//! the hart's file.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod kernel;

#[cfg(not(itest_riscv64))]
fn main() {}
