//! The aarch64 autoload-input desktop vertical with its keyboard and mouse as
//! PCI functions on the `virt` board's generic ECAM host (`plans/IOMMU.md`
//! IOM13): the kernel takes the host, sets out its resources and grants each
//! function the INTx line the host's `interrupt-map` routes it to. The two
//! functions sit in slots whose INTA pins share one SPI, so every key and
//! pointer event of the run crossed a line both drivers bind.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel;

#[cfg(not(itest_aarch64))]
fn main() {}
