//! QEMU integration test: the aarch64 boot entered at EL2, as the Raspberry
//! Pi firmware enters it.
//!
//! The board implements EL2 and enters the image there, so the trampoline
//! must hand the boot CPU to EL1 with the EL2 controls it owns written: the
//! kernel runs at EL1 and holds every PMU event counter the core implements,
//! which an `MDCR_EL2.HPMN` of zero would withhold.
//!
//! Any failure is reported to QEMU as a distinct, non-zero exit code.

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::fmt::Write as _;
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::coreclock::pmu_available;
    use tairix_arch_aarch64::el2::PMCR_EL0_N_SHIFT;
    use tairix_arch_aarch64::serial::ConsoleWriter;
    use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit};
    use tairix_itest_finisher::fail_point;

    /// `ID_AA64PFR0_EL1.EL2` (bits `[11:8]`): non-zero where the core
    /// implements EL2.
    const PFR0_EL2_SHIFT: u32 = 8;

    #[panic_handler]
    fn el2_entry_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// What the hand-off left EL1 to read.
    struct Observed {
        /// The exception level the kernel runs at.
        el: u64,
        /// `ID_AA64PFR0_EL1.EL2`: non-zero where the core implements EL2.
        el2: u64,
        /// Whether the core implements the architected PMU.
        pmu: bool,
        /// `PMCR_EL0.N` as EL1 reads it: the event counters EL2 left it.
        counters: u64,
    }

    fn observe() -> Observed {
        let (el, pfr0): (u64, u64);
        // SAFETY: reads of the current exception level and a feature-ID
        // register, both architected and readable at EL1 with no side effect.
        unsafe {
            core::arch::asm!(
                "mrs {el}, CurrentEL",
                "mrs {pfr0}, id_aa64pfr0_el1",
                el = out(reg) el,
                pfr0 = out(reg) pfr0,
                options(nomem, nostack, preserves_flags),
            );
        }
        let pmu = pmu_available();
        let mut pmcr = 0;
        if pmu {
            // SAFETY: the core implements the architected PMU (checked
            // above), whose control register EL1 reads with no side effect
            // unless EL2 traps it, which is what this vertical would show.
            unsafe {
                core::arch::asm!(
                    "mrs {pmcr}, pmcr_el0",
                    pmcr = out(reg) pmcr,
                    options(nomem, nostack, preserves_flags),
                );
            }
        }
        Observed {
            el: (el >> 2) & 0b11,
            el2: (pfr0 >> PFR0_EL2_SHIFT) & 0xF,
            pmu,
            counters: (pmcr >> PMCR_EL0_N_SHIFT) & 0x1F,
        }
    }

    #[no_mangle]
    pub extern "C" fn kernel_main(_tree: u64) -> ! {
        let mut w = ConsoleWriter;
        let seen = observe();
        let failure: Option<(&str, NonZeroU16)> = if seen.el2 == 0 {
            Some((
                "the board implements no EL2, so the run entered at EL1",
                fail_point!(1),
            ))
        } else if seen.el != 1 {
            Some(("the kernel does not run at EL1", fail_point!(2)))
        } else if !seen.pmu {
            Some((
                "the core implements no architected PMU to check",
                fail_point!(3),
            ))
        } else if seen.counters == 0 {
            Some(("EL1 holds none of the PMU's event counters", fail_point!(4)))
        } else {
            None
        };
        match failure {
            None => {
                let _ = writeln!(
                    w,
                    "[el2-entry] entered at EL2, running at EL1 with {} event counters",
                    seen.counters
                );
                qemu_exit::exit_success()
            }
            Some((why, code)) => {
                let _ = writeln!(w, "[el2-entry] FAILED: {why}");
                qemu_exit::exit_failure(code)
            }
        }
    }
}

// Host stub. The crate is *only* meaningful on the bare-metal aarch64
// target; on the host a no-op `main` keeps `cargo build` / `cargo test`
// against the host triple working for IDE indexing and `cargo xtask ci`.
#[cfg(not(itest_aarch64))]
fn main() {}
