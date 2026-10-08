//! `plans/IOMMU.md` IOM18.4: behind a RISC-V IOMMU, a device's MSI lands in a
//! memory-resident interrupt file of its own.
//!
//! Two QEMU `edu` functions sit on the generic host behind the unit, A in
//! slot 2 and B in slot 3. The run confines each one's messages to a file of
//! its own whose notice is an identity of the hart's IMSIC file, then has A
//! write B's notice identity, followed by its own vector, then has B write
//! its vector. A's write naming B's notice must land in A's file and raise
//! nothing, while B's notice is live.
//!
//! It links the arch port, the RISC-V IOMMU family, `lib/pci` and `lib/fdt`
//! alone, with translation off, so every table the unit reads is at its own
//! physical address.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
extern crate alloc;

#[cfg(itest_riscv64)]
mod kernel;

#[cfg(not(itest_riscv64))]
fn main() {}
