//! x86_64 floating-point isolation vertical (`plans/OPEN-DEFECTS.md`
//! D359/D360/D362): two hardware-isolated ring-3 tasks fill the whole SSE/x87
//! register file and `MXCSR` with different patterns and timeshare one CPU,
//! and neither may see the other's values; a third task from the entry-hygiene
//! fixture proves first entry to user mode leaks no kernel register state; and
//! the kernel does floating point in the dispatch loop under a probe task's
//! hostile `MXCSR` (round-toward-zero, an unmasked exception) and must round to
//! nearest and take no `#XM`. Any shortfall flips `qemu_exit::exit_failure` or
//! times out, so the run fails loudly.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!(
    "tairix-test-fp-isolation-qemu-x86-64: the `test-hooks` Cargo feature is a \
     debug-only test affordance and must not be enabled in release builds. \
     See AGENTS.md §2.1 (no hacks) and §5.4.5 (fail closed)."
);

#[cfg(all(itest_x86_64, feature = "test-hooks"))]
mod kernel;

// --- Stub when the test-hooks feature is off ----------------------
#[cfg(all(itest_x86_64, not(feature = "test-hooks")))]
#[no_mangle]
pub extern "C" fn kernel_main(_multiboot_info: u64) -> ! {
    loop {
        // SAFETY: `cli; hlt` is a well-defined parked-CPU sequence on x86_64.
        unsafe {
            core::arch::asm!("cli; hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(itest_x86_64, not(feature = "test-hooks")))]
#[panic_handler]
fn panic_stub(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        // SAFETY: same as above.
        unsafe {
            core::arch::asm!("cli; hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_x86_64))]
fn main() {}
