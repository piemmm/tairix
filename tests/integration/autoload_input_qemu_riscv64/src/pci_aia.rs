//! The riscv64 autoload-input vertical with its keyboard and mouse as PCI
//! functions on an AIA `virt` board (`plans/IOMMU.md` IOM18.3): their shared
//! INTx line routed through the APLIC, no unit confining a message.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod kernel;

#[cfg(not(itest_riscv64))]
fn main() {}
