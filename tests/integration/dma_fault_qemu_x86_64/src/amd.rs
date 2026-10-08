//! The fault vertical behind an `amd-iommu`: [`vertical`].

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
extern crate alloc;

mod vertical;

/// QEMU's AMD-Vi model writes a refused access's direction into event record
/// bits the specification gives other meanings, so a record never says it
/// wrote.
#[cfg(itest_x86_64)]
const DIRECTION_RECORDED: bool = false;

/// QEMU's AMD-Vi model, through 11.0, builds an event field's mask from the
/// field's start bit in the whole record rather than in its word, so the
/// address never lands and every record names address zero.
#[cfg(itest_x86_64)]
const ADDRESS_RECORDED: bool = false;

#[cfg(not(itest_x86_64))]
fn main() {}
