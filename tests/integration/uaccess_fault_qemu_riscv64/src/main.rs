//! `tests/SECURITY.md` §5 `copy_from_user` hardware fault fix-up
//! (riscv64) QEMU integration test: a load/store page fault taken inside
//! the guarded user-copy window is redirected to the copy's fix-up and
//! surfaces as an error — the hart keeps running instead of halting.
//!
//! ## Why this exists
//!
//! The kernel's validated user-copy path (`kernel/mem::uaccess`) proves
//! every page before it moves a byte, so a mid-copy hardware fault means
//! that proof was violated underneath it. The per-port fault window
//! (`tairix_arch_riscv64::uaccess`) is the backstop that turns such a
//! fault into an error return; this vertical proves the whole mechanism
//! live on the `virt` board: real S-mode page fault → trap vector →
//! saved-`sepc` rewrite → fix-up return.
//!
//! ## What this test asserts
//!
//! 1. Build an Sv39 `AddressSpace` identity-mapping the low 4 GiB and
//!    activate it. A virtual page *beyond* the identity map
//!    (`UNMAPPED_VA`, 5 GiB) has no translation.
//! 2. `trap::install_trap_vector` (run before activation) points `stvec`
//!    at the S-mode vector **and arms the Arch HAL guarded-copy slot** —
//!    the one chokepoint pairing the recovery with the vector.
//! 3. The shared `tairix_arch_api::uaccess::conformance` checks pass:
//!    an intact copy moves its bytes exactly; a copy reading the
//!    unmapped page returns the fault error; a copy writing it returns
//!    the fault error — each taking a *real* hardware page fault whose
//!    redirect lets execution continue to the next check.
//! 4. Arming the vector cleared `sstatus.SUM`, so a guarded copy reading a
//!    mapped `U` page through its user address faults, while the same bytes
//!    copy through the kernel's own mapping of them.
//! 5. The fatal `fault` handler reports FAILURE: reaching it means the
//!    window redirect did not absorb the deliberate fault.
//!
//! ## How it differs from a production kernel
//!
//! It links only the `tairix-arch-riscv64` port and supplies its own
//! `kernel_main`. The QEMU-exit shortcut lives in this dedicated bin,
//! never behind a Cargo feature on the arch crate (fail closed).

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_riscv64)]
mod kernel {
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;

    use tairix_arch_api::fatal::KernelFault;
    use tairix_arch_api::mmu::{AddressSpace as _, PageFlags};
    use tairix_arch_api::uaccess::conformance::{self, reach_into, Reach, Verdict};
    use tairix_arch_riscv64::paging::{AddressSpace, PageTablePool};
    use tairix_arch_riscv64::{handle_panic_via_serial, qemu_exit, trap, SERIAL_SINK};
    use tairix_itest_finisher::fail_point;
    use tairix_log::{log, Event, EventId, Field, Level};

    /// Gigapages of identity map the space installs: `[0, 4 GiB)` covers
    /// the `virt` board's low MMIO and the 2 GiB RAM base at `0x8000_0000`
    /// where this kernel runs.
    const IDENTITY_GIB: usize = 4;

    /// A page-aligned virtual address beyond the identity map (5 GiB):
    /// canonical for Sv39 but with no translation, so any access to it
    /// raises a synchronous load/store page fault.
    const UNMAPPED_VA: u64 = 5 * (1 << 30);

    /// Stable audit-event ids for the QEMU transcript.
    const UA_TEST_START: EventId = EventId(4330);
    const UA_TEST_PASS: EventId = EventId(4331);
    const UA_TEST_FAIL: EventId = EventId(4332);

    /// `SiFive` Test failure codes, distinct per failure site so a failing
    /// run's exit status pinpoints the broken invariant.
    const FAIL_SETUP: NonZeroU16 = fail_point!(1);
    const FAIL_FATAL_FAULT: NonZeroU16 = fail_point!(2);
    const FAIL_NOT_INSTALLED: NonZeroU16 = fail_point!(3);
    const FAIL_INTACT_COPY: NonZeroU16 = fail_point!(4);
    const FAIL_FAULT_NOT_REPORTED: NonZeroU16 = fail_point!(5);
    const FAIL_SUM_SET: NonZeroU16 = fail_point!(6);
    const FAIL_USER_PAGE_READ: NonZeroU16 = fail_point!(7);
    const FAIL_KERNEL_MAPPING_READ: NonZeroU16 = fail_point!(8);

    /// A user address beyond the identity map that the space maps
    /// [`PROBE_PAGE`] at with the `U` bit.
    const USER_PROBE_VA: u64 = 6 * (1 << 30);

    /// `sstatus.SUM`, which lets S-mode load and store `U` pages.
    const SSTATUS_SUM: u64 = 1 << 18;

    /// The byte every word of [`PROBE_PAGE`] holds.
    const PROBE_BYTE: u8 = 0x5A;

    /// The page the probe reads by its user address and by the identity map,
    /// the kernel's own mapping of it. Never written, so a read proves which
    /// mapping reached it.
    #[repr(C, align(4096))]
    struct ProbePage([u8; 4096]);

    static PROBE_PAGE: ProbePage = ProbePage([PROBE_BYTE; 4096]);

    /// Page-table pool backing the address space (lives in `.bss`).
    static POOL: PageTablePool = PageTablePool::new();

    fn note(level: Level, id: EventId, message: &'static str) {
        log(
            &SERIAL_SINK,
            &Event {
                level,
                id,
                message,
                fields: &[],
            },
        );
    }

    /// The fatal synchronous-exception handler: reaching it means a fault
    /// escaped the guarded-copy window redirect (or something else
    /// faulted) — a closed failure either way.
    fn on_fault(_trap: KernelFault) -> ! {
        note(
            Level::Error,
            UA_TEST_FAIL,
            "riscv64 uaccess-fault test: fault escaped the copy window redirect",
        );
        qemu_exit::exit_failure(FAIL_FATAL_FAULT);
    }

    /// Forward to the shared riscv64 panic bridge (parks the hart; the run
    /// then times out and the harness reports the failure).
    #[panic_handler]
    fn tairix_uaccess_fault_riscv64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Log a setup failure and report it to QEMU. Never returns.
    fn fail(what: &'static str, code: NonZeroU16) -> ! {
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Error,
                id: UA_TEST_FAIL,
                message: "riscv64 uaccess-fault test: failed",
                fields: &[Field {
                    key: "stage",
                    value: tairix_log::FieldValue::Str(what),
                }],
            },
        );
        qemu_exit::exit_failure(code);
    }

    /// Arming the vector cleared `SUM`, so S-mode reaches the probe page
    /// only through the kernel's own mapping of it: a guarded copy reading it
    /// by its user address faults. Returns only if both hold.
    fn check_supervisor_reach(probe: u64) {
        let sstatus: u64;
        // SAFETY: reading `sstatus` in S-mode changes nothing.
        unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack)) };
        if sstatus & SSTATUS_SUM != 0 {
            fail("`SUM` set after arming the vector", FAIL_SUM_SET);
        }
        // SAFETY: `USER_PROBE_VA` maps the probe page as a `U` page and the
        // identity map is the kernel's own mapping of it.
        match unsafe { reach_into(USER_PROBE_VA, probe, PROBE_BYTE) } {
            Reach::OwnMappingOnly => {}
            Reach::UserAddressRead => fail(
                "a kernel read of a user page did not fault",
                FAIL_USER_PAGE_READ,
            ),
            Reach::OwnMappingUnread => fail(
                "the kernel's mapping did not read the page",
                FAIL_KERNEL_MAPPING_READ,
            ),
        }
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_riscv64_main`).
    #[no_mangle]
    pub extern "C" fn kernel_main(_hartid: u64, _dtb: u64) -> ! {
        note(
            Level::Info,
            UA_TEST_START,
            "riscv64 uaccess-fault test: faulting inside the guarded copy window",
        );

        // Build the identity space; `UNMAPPED_VA` lies beyond it.
        let Some(mut space) = AddressSpace::new_identity_gigapages(&POOL, IDENTITY_GIB) else {
            fail("identity map", FAIL_SETUP);
        };
        // The kernel runs identity-mapped, so the probe's address is its
        // physical one.
        let probe = core::ptr::addr_of!(PROBE_PAGE) as u64;
        if space
            .map_page(USER_PROBE_VA, probe, PageFlags::READ | PageFlags::USER)
            .is_err()
        {
            fail("probe page", FAIL_SETUP);
        }

        // Install the fatal handler (the FAILURE reporter), then the trap
        // vector — which also arms the Arch HAL guarded-copy slot, the
        // pairing under test.
        if tairix_arch_api::fault::set_fault_handler(on_fault).is_err() {
            fail("set_fault_handler", FAIL_SETUP);
        }
        // SAFETY: called once on the boot hart with a stack established;
        // no interrupt source is armed, so only the deliberate synchronous
        // page faults below reach the vector.
        unsafe {
            trap::install_trap_vector();
        }

        // Switch to the space (turns paging on). It identity-maps this
        // code, the stack, and the device MMIO, so execution continues.
        // SAFETY: the space identity-maps `pc`, `sp`, and MMIO per
        // `new_identity_gigapages`.
        unsafe {
            space.activate();
        }

        // Drive the shared checks: positive control, then a real fault on
        // the read side and the write side of the window.
        let mut scratch = [0u8; 64];
        // SAFETY: `UNMAPPED_VA` lies beyond the identity space just
        // activated, so the page is genuinely unmapped and the deliberate
        // page faults are absorbed by the guarded copy window armed above.
        let verdict = unsafe { conformance::run(UNMAPPED_VA as *mut u8, &mut scratch) };
        match verdict {
            Verdict::Pass => {
                check_supervisor_reach(probe);
                note(
                    Level::Info,
                    UA_TEST_PASS,
                    "riscv64 uaccess-fault test: in-window faults surfaced as errors",
                );
                qemu_exit::exit_success();
            }
            Verdict::NotInstalled => fail("guarded copy not installed", FAIL_NOT_INSTALLED),
            Verdict::IntactCopyBroken => fail("intact copy broken", FAIL_INTACT_COPY),
            Verdict::FaultNotReported => {
                fail("fault not reported as an error", FAIL_FAULT_NOT_REPORTED)
            }
        }
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_riscv64))]
fn main() {}
