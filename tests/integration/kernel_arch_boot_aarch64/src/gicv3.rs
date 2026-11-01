//! The production aarch64 boot on a four-CPU GICv3 `virt` board: every
//! secondary finds its redistributor and joins the kernel (`plans/IOMMU.md`
//! IOM18.1).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
#[path = "kernel.rs"]
mod kernel;

/// Whether every core is held to PAN: the default model offers it.
#[cfg(itest_aarch64)]
const PAN_HELD: bool = true;

/// The GICv3 `virt` board's tree.
#[cfg(itest_aarch64)]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/gicv3_tree.rs"));
}

#[cfg(not(itest_aarch64))]
fn main() {}
