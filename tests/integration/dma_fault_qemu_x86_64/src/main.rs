//! The fault vertical behind an `intel-iommu`: [`vertical`].

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
extern crate alloc;

mod vertical;

/// The unit records which way a refused access went.
#[cfg(itest_x86_64)]
const DIRECTION_RECORDED: bool = true;

#[cfg(not(itest_x86_64))]
fn main() {}
