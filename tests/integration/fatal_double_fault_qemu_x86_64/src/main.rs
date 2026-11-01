//! `plans/OPEN-DEFECTS.md` D146 and D1590 on x86_64: an overrun of the boot
//! stack faults on the guard's first byte rather than rewriting the page
//! tables beneath it, and is reported rather than triple-faulting.
//!
//! The kernel moves `rsp` to the boot stack's lowest byte and pushes. The
//! kernel root maps the guard below it read-only, so the push faults;
//! delivering that fault needs the same stack, so the CPU escalates to `#DF`,
//! which the boot tables take on a stack of their own to the fatal path. The
//! harness passes the run only if it stopped on that report's record naming
//! vector 8 and the address the guard stopped.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::fmt::Write as _;
    use core::panic::PanicInfo;

    use tairix_arch_x86_64::panic::handle_panic_via_serial;
    use tairix_arch_x86_64::{qemu_exit, serial};

    extern "C" {
        /// The boot stack's lowest byte, through the kernel window.
        static boot_stack_bottom_high: u8;
    }

    #[panic_handler]
    fn fatal_double_fault_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Entry point for the freestanding kernel.
    #[no_mangle]
    pub extern "C" fn kernel_main(_boot_info: u64) -> ! {
        let mut com1 = serial::Serial::init(serial::COM1_BASE);
        // Taking the extern's address is a link-time constant; it is never
        // dereferenced here.
        let bottom = core::ptr::addr_of!(boot_stack_bottom_high) as u64;
        let _ = writeln!(
            com1,
            "[fatal_double_fault] x86_64: pushing below the boot stack at {bottom:#x}"
        );
        // SAFETY: moves `rsp` to the boot stack's lowest byte, pushes one word
        // into the guard below it and restores `rsp`. Under the kernel root
        // the guard is read-only, so the push faults and the CPU takes `#DF`
        // to the fatal path, which parks; only a guard that took the write
        // returns here, and no byte below `bottom` is live.
        unsafe {
            core::arch::asm!(
                "mov {saved}, rsp",
                "mov rsp, {bottom}",
                "push {saved}",
                "pop rsp",
                bottom = in(reg) bottom,
                saved = out(reg) _,
            );
        }
        let _ = writeln!(
            com1,
            "[fatal_double_fault] x86_64 FAILED: the boot stack's guard took a write"
        );
        qemu_exit::exit_failure()
    }
}

// Host stub. The crate is only meaningful on the bare-metal target.
#[cfg(not(itest_x86_64))]
fn main() {}
