//! QEMU integration test: a CPU the kernel holds gives user code neither its
//! time-stamp counter nor its performance counters.
//!
//! Firmware may leave `CR4.TSD` clear and `CR4.PCE` set, opening both, so the
//! run opens them itself, has the port hold the CPU again, and reads both
//! back closed.
//!
//! Any failure is reported to QEMU through its debug-exit device.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::fmt::Write as _;
    use core::panic::PanicInfo;

    use tairix_arch_x86_64::protection::{hold, in_force, CR4_PCE, CR4_TSD};
    use tairix_arch_x86_64::{qemu_exit, serial};

    #[panic_handler]
    fn user_counters_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        tairix_arch_x86_64::panic::handle_panic_via_serial(info)
    }

    fn cr4() -> u64 {
        let cr4: u64;
        // SAFETY: reading `CR4` at CPL 0 has no side effect.
        unsafe {
            core::arch::asm!("mov {cr4}, cr4", cr4 = out(reg) cr4, options(nomem, nostack, preserves_flags));
        }
        cr4
    }

    #[no_mangle]
    pub extern "C" fn kernel_main(_multiboot_info: u64) -> ! {
        let mut com1 = serial::Serial::init(serial::COM1_BASE);
        // SAFETY: clearing `TSD` and setting `PCE` only let user code run
        // `rdtsc` and `rdpmc`, and this kernel never enters user mode.
        unsafe {
            core::arch::asm!(
                "mov {t}, cr4",
                "and {t}, {open_tsc}",
                "or {t}, {pce}",
                "mov cr4, {t}",
                t = out(reg) _,
                open_tsc = in(reg) !CR4_TSD,
                pce = in(reg) CR4_PCE,
                options(nomem, nostack, preserves_flags),
            );
        }
        if cr4() & (CR4_TSD | CR4_PCE) != CR4_PCE {
            let _ = writeln!(
                com1,
                "[user-counters] FAILED: CR4 would not open the counters"
            );
            qemu_exit::exit_failure();
        }
        // SAFETY: CPL 0 with interrupts disabled and `RFLAGS.AC` clear at
        // entry, the protections in force being those the boot CPU settled
        // from itself, and no user page mapped.
        unsafe { hold(in_force()) };
        let held = cr4();
        if held & (CR4_TSD | CR4_PCE) == CR4_TSD {
            let _ = writeln!(com1, "[user-counters] x86_64 counters closed to user code");
            qemu_exit::exit_success()
        } else {
            let _ = writeln!(com1, "[user-counters] FAILED: CR4={held:#x}");
            qemu_exit::exit_failure()
        }
    }
}

// Host stub. The crate is *only* meaningful on the bare-metal x86_64
// target; on the host a no-op `main` keeps `cargo build` / `cargo test`
// against the host triple working for IDE indexing and `cargo xtask ci`.
#[cfg(not(itest_x86_64))]
fn main() {}
