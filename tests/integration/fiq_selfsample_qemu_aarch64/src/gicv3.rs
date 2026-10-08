//! The FIQ masked-section self-sample on a GICv3 `virt` board: the cadence a
//! Group 0 interrupt of the boot CPU's redistributor, acknowledged through
//! `ICC_IAR0_EL1` (`plans/IOMMU.md` IOM18.1).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!(
    "tairix-test-fiq-selfsample-qemu-aarch64: the `test-hooks` Cargo feature is a \
     debug-only test affordance and must not be enabled in release builds. \
     See AGENTS.md §2.1 (no hacks) and §5.4 (fail closed)."
);

#[cfg(all(itest_aarch64, feature = "test-hooks"))]
#[path = "kernel.rs"]
mod kernel;

/// The GICv3 `virt` board's tree.
#[cfg(all(itest_aarch64, feature = "test-hooks"))]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/gicv3_tree.rs"));
}

#[cfg(all(itest_aarch64, not(feature = "test-hooks")))]
#[path = "stub.rs"]
mod stub;

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
