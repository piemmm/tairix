//! PC17 / BR26 QEMU integration test: prove code installation end to end on
//! the aarch64 `virt` board (`plans/PCEMU.md` PC17; `plans/BROWSER.md` BR26).
//!
//! The test kernel and the program roles it drives are described in
//! `kernel.rs`.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!(
    "tairix-test-code-install-qemu-aarch64: the `test-hooks` Cargo feature is a \
     debug-only test affordance and must not be enabled in release builds. \
     See AGENTS.md §2.1 (no hacks) and §5.4.5 (fail closed)."
);

#[cfg(all(itest_aarch64, feature = "test-hooks"))]
extern crate alloc;

#[cfg(all(itest_aarch64, feature = "test-hooks"))]
mod kernel;

#[cfg(all(itest_aarch64, not(feature = "test-hooks")))]
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    loop {
        // SAFETY: `wfe` is a well-defined parked-CPU hint on aarch64.
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(not(itest_aarch64))]
fn main() {}
