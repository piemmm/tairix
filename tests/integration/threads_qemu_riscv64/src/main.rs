//! `T3b-u` QEMU integration test: prove lightweight threads end to end on
//! the riscv64 `virt` board (`plans/THREADS.md`; `docs/src/architecture/threads.md`).
//!
//! The test kernel and the program roles it drives are described in
//! `kernel.rs`.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!(
    "tairix-test-threads-qemu-riscv64: the `test-hooks` Cargo feature is a \
     debug-only test affordance and must not be enabled in release builds. \
     See AGENTS.md §2.1 (no hacks) and §5.4.5 (fail closed)."
);

#[cfg(all(itest_riscv64, feature = "test-hooks"))]
extern crate alloc;

#[cfg(all(itest_riscv64, feature = "test-hooks"))]
mod kernel;

// --- Stub when the test-hooks feature is off ----------------------
#[cfg(all(itest_riscv64, not(feature = "test-hooks")))]
#[no_mangle]
pub extern "C" fn kernel_main(_hartid: u64, _dtb: u64) -> ! {
    loop {
        // SAFETY: `wfi` is a well-defined parked-hart hint on riscv64.
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(itest_riscv64, not(feature = "test-hooks")))]
#[panic_handler]
fn panic_stub(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        // SAFETY: same as above.
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_riscv64))]
fn main() {}
