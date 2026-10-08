//! `plans/IOMMU.md` IOM18.2: a GICv3 ITS drops a device's MSI for an event it
//! was not given.
//!
//! Two QEMU `edu` functions sit on the generic host, A in slot 2 and B in
//! slot 3. The run maps A's event 0 and B's events 0 and 1 to LPIs of their
//! own, then has A write event 1, which is mapped, but for B, followed by its
//! own event 0, then has B write its event 1. A's forged message must raise
//! nothing while the LPI B's event 1 raises is live: the service knows a
//! message by the `DeviceID` the fabric attaches, which A cannot choose.
//!
//! It links the arch port, `lib/pci` and `lib/fdt` alone, with the MMU off,
//! so every table the ITS reads is at its own physical address.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
extern crate alloc;

#[cfg(itest_aarch64)]
mod kernel;

/// The GICv3 `virt` board's tree.
#[cfg(itest_aarch64)]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/gicv3_tree.rs"));
}

#[cfg(not(itest_aarch64))]
fn main() {}
