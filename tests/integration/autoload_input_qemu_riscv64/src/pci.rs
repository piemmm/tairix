//! The riscv64 autoload-input vertical with its keyboard and mouse as PCI
//! functions on the `virt` board's generic ECAM host (`plans/IOMMU.md`
//! IOM13): the kernel takes the host, sets out its resources and grants each
//! function the INTx line the host's `interrupt-map` routes it to. The two
//! functions sit in slots whose INTA pins share one PLIC source, so the key
//! the keyboard's driver delivers crossed a line its sibling also binds.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod kernel;

#[cfg(not(itest_riscv64))]
fn main() {}
