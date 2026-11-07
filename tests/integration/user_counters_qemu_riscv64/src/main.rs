//! QEMU integration test: a hart the kernel arms gives U-mode none of its
//! counters.
//!
//! `scounteren` is UNSPECIFIED out of reset, where QEMU resets it closed, so
//! the run opens it itself, re-arms the hart, and reads it back closed.
//!
//! Any failure is reported to QEMU as a distinct, non-zero exit code.

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod kernel {
    use core::fmt::Write as _;
    use core::panic::PanicInfo;

    use tairix_arch_riscv64::serial::SbiWriter;
    use tairix_arch_riscv64::trap::install_trap_vector;
    use tairix_arch_riscv64::{handle_panic_via_serial, qemu_exit};
    use tairix_itest_finisher::fail_point;

    /// The cycle, time and retired-instruction counters, open to U-mode.
    const SCOUNTEREN_OPEN: u64 = 0b111;

    #[panic_handler]
    fn user_counters_qemu_riscv64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    fn scounteren() -> u64 {
        let open: u64;
        // SAFETY: an S-mode CSR read with no side effect.
        unsafe {
            core::arch::asm!("csrr {open}, scounteren", open = out(reg) open, options(nomem, nostack));
        }
        open
    }

    #[no_mangle]
    pub extern "C" fn kernel_main(_hartid: u64, _dtb: u64) -> ! {
        let mut w = SbiWriter;
        // SAFETY: opening the counters only matters to U-mode, which this
        // kernel never enters.
        unsafe {
            core::arch::asm!("csrw scounteren, {open}", open = in(reg) SCOUNTEREN_OPEN, options(nomem, nostack));
        }
        if scounteren() != SCOUNTEREN_OPEN {
            let _ = writeln!(w, "[user-counters] FAILED: scounteren would not open");
            qemu_exit::exit_failure(fail_point!(1));
        }
        // SAFETY: on this hart, on its stack and in S-mode, before any task
        // owes it registers or any user code can `ecall`.
        unsafe { install_trap_vector() };
        match scounteren() {
            0 => {
                let _ = writeln!(w, "[user-counters] riscv64 counters closed to U-mode");
                qemu_exit::exit_success()
            }
            open => {
                let _ = writeln!(w, "[user-counters] FAILED: scounteren={open:#x}");
                qemu_exit::exit_failure(fail_point!(2))
            }
        }
    }
}

// Host stub. The crate is *only* meaningful on the bare-metal riscv64
// target; on the host a no-op `main` keeps `cargo build` / `cargo test`
// against the host triple working for IDE indexing and `cargo xtask ci`.
#[cfg(not(itest_riscv64))]
fn main() {}
