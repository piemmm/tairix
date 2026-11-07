//! QEMU integration test: a CPU the kernel holds gives EL0 none of its
//! counters.
//!
//! `CNTKCTL_EL1`, `PMUSERENR_EL0` and `MDSCR_EL1.TDCC` are UNKNOWN out of
//! reset on silicon, where QEMU resets them closed, so the run opens all three
//! itself, has the port hold the CPU again, and reads them back closed.
//!
//! Any failure is reported to QEMU as a distinct, non-zero exit code.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::fmt::Write as _;
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::coreclock::pmu_available;
    use tairix_arch_aarch64::serial::ConsoleWriter;
    use tairix_arch_aarch64::{handle_panic_via_serial, protection, qemu_exit};
    use tairix_itest_finisher::fail_point;

    /// Every EL0 grant `CNTKCTL_EL1` holds: both counters, the event stream
    /// and both timers.
    const CNTKCTL_OPEN: u64 = 0b11 | (1 << 2) | (0b11 << 8);

    /// Every EL0 grant `PMUSERENR_EL0` holds: `EN`, `SW`, `CR` and `ER`.
    const PMUSERENR_OPEN: u64 = 0b1111;

    #[panic_handler]
    fn user_counters_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// `(CNTKCTL_EL1, PMUSERENR_EL0, MDSCR_EL1)`.
    fn controls() -> (u64, u64, u64) {
        let (timer, monitors, debug): (u64, u64, u64);
        // SAFETY: the core implements the architected PMU (checked first by
        // the caller), and all three registers read at EL1 with no side
        // effect.
        unsafe {
            core::arch::asm!(
                "mrs {timer}, cntkctl_el1",
                "mrs {monitors}, pmuserenr_el0",
                "mrs {debug}, mdscr_el1",
                timer = out(reg) timer,
                monitors = out(reg) monitors,
                debug = out(reg) debug,
                options(nomem, nostack, preserves_flags),
            );
        }
        (timer, monitors, debug)
    }

    fn open_to_el0() {
        // SAFETY: the core implements the architected PMU (checked first by
        // the caller); opening all three only matters to EL0, which this
        // kernel never enters, and a zero `MDSCR_EL1` enables no debug event.
        unsafe {
            core::arch::asm!(
                "msr cntkctl_el1, {timer}",
                "msr pmuserenr_el0, {monitors}",
                "msr mdscr_el1, xzr",
                "isb",
                timer = in(reg) CNTKCTL_OPEN,
                monitors = in(reg) PMUSERENR_OPEN,
                options(nomem, nostack, preserves_flags),
            );
        }
    }

    #[no_mangle]
    pub extern "C" fn kernel_main(_tree: u64) -> ! {
        let mut w = ConsoleWriter;
        if !pmu_available() {
            let _ = writeln!(w, "[user-counters] FAILED: no architected PMU to close");
            qemu_exit::exit_failure(fail_point!(1));
        }
        open_to_el0();
        if controls() != (CNTKCTL_OPEN, PMUSERENR_OPEN, 0) {
            let _ = writeln!(w, "[user-counters] FAILED: the controls would not open");
            qemu_exit::exit_failure(fail_point!(2));
        }
        // SAFETY: at EL1 on the boot CPU, before it reaches any user page.
        if !unsafe { protection::hold() } {
            let _ = writeln!(w, "[user-counters] FAILED: the CPU refused its hold");
            qemu_exit::exit_failure(fail_point!(3));
        }
        match controls() {
            (0, 0, protection::MDSCR_TDCC) => {
                let _ = writeln!(w, "[user-counters] aarch64 counters closed to EL0");
                qemu_exit::exit_success()
            }
            (timer, monitors, debug) => {
                let _ = writeln!(
                    w,
                    "[user-counters] FAILED: CNTKCTL_EL1={timer:#x} PMUSERENR_EL0={monitors:#x} \
                     MDSCR_EL1={debug:#x}"
                );
                qemu_exit::exit_failure(fail_point!(4))
            }
        }
    }
}

// Host stub. The crate is *only* meaningful on the bare-metal aarch64
// target; on the host a no-op `main` keeps `cargo build` / `cargo test`
// against the host triple working for IDE indexing and `cargo xtask ci`.
#[cfg(not(itest_aarch64))]
fn main() {}
