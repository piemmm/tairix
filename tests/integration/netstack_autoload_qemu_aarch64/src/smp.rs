//! The netstack-autoload vertical on a four-CPU machine (`plans/FIX-SLEEPLOCK.md`
//! S6): the `vertical` module's run, over a tree describing all four CPUs, so
//! the boot starts every one of them.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod vertical;

#[cfg(itest_aarch64)]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/smp_tree.rs"));
}

/// The symbol the arch crate's boot trampoline calls: the four-CPU machine.
#[cfg(itest_aarch64)]
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    vertical::boot(tree::DTB_BLOB)
}

#[cfg(not(itest_aarch64))]
fn main() {}
