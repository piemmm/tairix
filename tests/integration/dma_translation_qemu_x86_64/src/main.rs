//! The MI0 vertical behind an `intel-iommu`: [`vertical`].

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

mod vertical;

#[cfg(not(itest_x86_64))]
fn main() {}
