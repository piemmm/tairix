//! A store just below the boot stack lands in its guard, which the kernel
//! root maps read-only. The fault cannot be taken on the stack that overran —
//! its frame would land in the guard and fault again — so the EL1 vector
//! takes it on the reserve stack, and the report names the overrun
//! (`boot_stack_guard=faulted`) with the interrupted stack pointer.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::fmt::Write as _;
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::serial::ConsoleWriter;
    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit};

    extern "C" {
        /// The boot stack's lowest byte.
        static __boot_stack_bottom: u8;
    }

    #[panic_handler]
    fn fatal_stack_overrun_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Entry point for the freestanding kernel.
    #[no_mangle]
    pub extern "C" fn kernel_main(_tree: u64) -> ! {
        // Taking the extern's address is a link-time constant; it is never
        // dereferenced here.
        let bottom = core::ptr::addr_of!(__boot_stack_bottom) as u64;
        let _ = writeln!(
            ConsoleWriter,
            "[fatal_stack_overrun] aarch64: storing below the boot stack at {bottom:#x}"
        );
        // SAFETY: moves `sp` to the boot stack's lowest byte, stores one pair
        // into the guard below it and restores `sp`. The guard is read-only,
        // so the store faults and the CPU takes the fatal path, which parks;
        // only a guard that took the write returns here, and no byte below
        // `bottom` is live.
        unsafe {
            core::arch::asm!(
                "mov {saved}, sp",
                "mov sp, {bottom}",
                "stp {saved}, {saved}, [sp, #-16]!",
                "mov sp, {saved}",
                bottom = in(reg) bottom,
                saved = out(reg) _,
            );
        }
        let _ = writeln!(
            ConsoleWriter,
            "[fatal_stack_overrun] aarch64 FAILED: the boot stack's guard took a write"
        );
        qemu_exit::exit_failure(core::num::NonZeroU16::MIN)
    }
}

// Host stub. The crate is only meaningful on the bare-metal target.
#[cfg(not(itest_aarch64))]
fn main() {}
