//! The fault vertical behind an `amd-iommu`: [`vertical`].

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
extern crate alloc;

mod vertical;

/// QEMU's AMD-Vi model writes a refused access's direction into event record
/// bits the specification gives other meanings, so the family reads none.
#[cfg(itest_x86_64)]
const DIRECTION_RECORDED: bool = false;

#[cfg(not(itest_x86_64))]
fn main() {}
