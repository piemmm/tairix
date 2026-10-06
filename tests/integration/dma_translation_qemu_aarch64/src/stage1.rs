//! The IOM15 vertical behind an `SMMUv3` that translates at stage 1 alone:
//! the `vertical` module's run.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod vertical;

/// The stage the run proves: the only stage the unit offers.
#[cfg(itest_aarch64)]
const STAGE: tairix_itest_translation_witness::Stage =
    tairix_itest_translation_witness::Stage::First;

#[cfg(itest_aarch64)]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/stage1_tree.rs"));
}

/// The symbol the arch crate's boot trampoline calls.
#[cfg(itest_aarch64)]
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    vertical::boot(tree::DTB_BLOB)
}

#[cfg(not(itest_aarch64))]
fn main() {}
