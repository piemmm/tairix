//! `tests/SECURITY.md` §5 `copy_from_user` hardware fault fix-up
//! (aarch64) QEMU integration test: a same-EL data abort taken inside
//! the guarded user-copy window is redirected to the copy's fix-up and
//! surfaces as an error — the CPU keeps running instead of halting.
//!
//! ## Why this exists
//!
//! The kernel's validated user-copy path (`kernel/mem::uaccess`) proves
//! every page before it moves a byte, so a mid-copy hardware fault means
//! that proof was violated underneath it. The per-port fault window
//! (`tairix_arch_aarch64::uaccess`) is the backstop that turns such a
//! fault into an error return; this vertical proves the whole mechanism
//! live on the `virt` board: real EL1 data abort → vector table →
//! frame-ELR rewrite → fix-up return.
//!
//! ## What this test asserts
//!
//! 1. Build a stage-1 `AddressSpace` identity-mapping the low 2 GiB and
//!    activate it. A virtual page *beyond* the identity map
//!    (`UNMAPPED_VA`, 3 GiB) has no translation.
//! 2. `exceptions::init_vectors` points `VBAR_EL1` at the vector table
//!    **and arms the Arch HAL guarded-copy slot** — the one chokepoint
//!    pairing the recovery with the vectors.
//! 3. The shared `tairix_arch_api::uaccess::conformance` checks pass:
//!    an intact copy moves its bytes exactly; a copy reading the
//!    unmapped page returns the fault error; a copy writing it returns
//!    the fault error — each taking a *real* hardware data abort whose
//!    redirect lets execution continue to the next check.
//! 4. On a CPU offering PAN (the enrolment runs `cortex-a76`), the boot held
//!    the CPU to it: `PSTATE.PAN` is set and translation came on with
//!    `SCTLR_EL1.SPAN` clear, so a guarded copy reading a mapped EL0 page
//!    through its user address faults while the same bytes copy through the
//!    kernel's own mapping of them.
//! 5. The fatal `fault` handler reports FAILURE: reaching it means the
//!    window redirect did not absorb the deliberate abort.
//!
//! ## How it differs from a production kernel
//!
//! It links only the `tairix-arch-aarch64` port and supplies its own
//! `kernel_main`. The QEMU-exit shortcut lives in this dedicated bin,
//! never behind a Cargo feature on the arch crate (fail closed).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;

    use tairix_arch_aarch64::paging::{self, AddressSpace, PageTablePool};
    use tairix_arch_aarch64::protection;
    use tairix_arch_aarch64::{
        enable_fp_el1, exceptions, handle_panic_via_serial, qemu_exit, SERIAL_SINK,
    };
    use tairix_arch_api::fatal::KernelFault;
    use tairix_arch_api::mmu::{AddressSpace as _, PageFlags};
    use tairix_arch_api::uaccess::conformance::{self, reach_into, Reach, Verdict};
    use tairix_itest_finisher::fail_point;
    use tairix_log::{log, Event, EventId, Field, Level};

    /// Number of GiB the space identity-maps (device MMIO + RAM). The
    /// kernel image and stack live in the Normal RAM gigapage (GiB 1).
    const IDENTITY_GIB: usize = 2;

    /// A page-aligned virtual address beyond the identity map (3 GiB):
    /// inside the TTBR0 span but with no translation, so any access to it
    /// raises a synchronous same-EL data abort.
    const UNMAPPED_VA: u64 = 3 * (1 << 30);

    /// Stable audit-event ids for the QEMU transcript.
    const UA_TEST_START: EventId = EventId(4320);
    const UA_TEST_PASS: EventId = EventId(4321);
    const UA_TEST_FAIL: EventId = EventId(4322);

    /// Semihosting failure codes, distinct per failure site so a failing
    /// run's exit status pinpoints the broken invariant.
    const FAIL_SETUP: NonZeroU16 = fail_point!(1);
    const FAIL_FATAL_FAULT: NonZeroU16 = fail_point!(2);
    const FAIL_NOT_INSTALLED: NonZeroU16 = fail_point!(3);
    const FAIL_INTACT_COPY: NonZeroU16 = fail_point!(4);
    const FAIL_FAULT_NOT_REPORTED: NonZeroU16 = fail_point!(5);
    const FAIL_PAN_NOT_HELD: NonZeroU16 = fail_point!(6);
    const FAIL_USER_PAGE_READ: NonZeroU16 = fail_point!(7);
    const FAIL_KERNEL_MAPPING_READ: NonZeroU16 = fail_point!(8);
    const FAIL_STACK_PROBE: NonZeroU16 = fail_point!(9);

    /// An EL0 address beyond the identity map that the space maps
    /// [`PROBE_PAGE`] at.
    const USER_PROBE_VA: u64 = 4 * (1 << 30);

    /// `PSTATE.PAN`, as the `PAN` register reads it.
    const PSTATE_PAN: u64 = 1 << 22;

    /// `SCTLR_EL1.SPAN`: set, an exception taken to EL1 leaves PAN alone.
    const SCTLR_SPAN: u64 = 1 << 23;

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

    /// The fatal synchronous-exception handler: reaching it means an abort
    /// escaped the guarded-copy window redirect (or something else
    /// faulted) — a closed failure either way.
    fn on_fault(trap: KernelFault) -> ! {
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Error,
                id: UA_TEST_FAIL,
                message: "aarch64 uaccess-fault test: abort escaped the copy window redirect",
                fields: &[
                    Field {
                        key: "esr",
                        value: trap.syndrome.map_or(
                            tairix_log::FieldValue::Null,
                            tairix_log::FieldValue::UnsignedInt,
                        ),
                    },
                    Field {
                        key: "far",
                        value: trap.address.map_or(
                            tairix_log::FieldValue::Null,
                            tairix_log::FieldValue::UnsignedInt,
                        ),
                    },
                    Field {
                        key: "elr",
                        value: tairix_log::FieldValue::UnsignedInt(trap.pc),
                    },
                ],
            },
        );
        qemu_exit::exit_failure(FAIL_FATAL_FAULT);
    }

    /// Forward to the shared aarch64 panic bridge (parks the CPU; the run
    /// then times out and the harness reports the failure).
    #[panic_handler]
    fn tairix_uaccess_fault_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Log a setup failure and report it to QEMU. Never returns.
    fn fail(what: &'static str, code: NonZeroU16) -> ! {
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Error,
                id: UA_TEST_FAIL,
                message: "aarch64 uaccess-fault test: failed",
                fields: &[Field {
                    key: "stage",
                    value: tairix_log::FieldValue::Str(what),
                }],
            },
        );
        qemu_exit::exit_failure(code);
    }

    /// The boot held the CPU to PAN, so EL1 reaches the probe page only
    /// through the kernel's own mapping of it: a guarded copy reading it by
    /// its EL0 address faults. Returns only if all of that holds.
    fn check_pan(probe: u64) {
        if !protection::pan_in_force() {
            fail("PAN not in force on a CPU offering it", FAIL_PAN_NOT_HELD);
        }
        let (pan, sctlr): (u64, u64);
        // SAFETY: the CPU offers PAN, so its register exists; both reads
        // change nothing.
        unsafe {
            core::arch::asm!(
                "mrs {pan}, S3_0_C4_C2_3",
                "mrs {sctlr}, sctlr_el1",
                pan = out(reg) pan,
                sctlr = out(reg) sctlr,
                options(nomem, nostack, preserves_flags),
            );
        }
        if pan & PSTATE_PAN == 0 || sctlr & SCTLR_SPAN != 0 {
            fail("PAN not set, or SPAN left set", FAIL_PAN_NOT_HELD);
        }
        // The watchdog's stack-link probe: EL1 may read its own stack, and
        // neither an EL0 page nor an unmapped one.
        let on_stack = 0u64;
        if !paging::el1_may_read(core::ptr::addr_of!(on_stack) as u64)
            || paging::el1_may_read(USER_PROBE_VA)
            || paging::el1_may_read(UNMAPPED_VA)
        {
            fail("the stack-link probe misjudged a page", FAIL_STACK_PROBE);
        }
        // SAFETY: `USER_PROBE_VA` maps the probe page as an EL0 page and the
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
    /// calls (via `tairix_arch_aarch64_main`).
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        // SAFETY: runs once on the boot CPU before any FP/SIMD
        // instruction executes (see `enable_fp_el1`).
        unsafe {
            enable_fp_el1();
        }

        note(
            Level::Info,
            UA_TEST_START,
            "aarch64 uaccess-fault test: faulting inside the guarded copy window",
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

        // Install the fatal handler (the FAILURE reporter), then the
        // vector table — which also arms the Arch HAL guarded-copy slot,
        // the pairing under test.
        if tairix_arch_api::fault::set_fault_handler(on_fault).is_err() {
            fail("set_fault_handler", FAIL_SETUP);
        }
        // SAFETY: called once on the boot CPU with a stack established;
        // no interrupt source is armed, so only the deliberate synchronous
        // aborts below reach the vectors.
        unsafe {
            exceptions::init_vectors();
        }

        // Switch to the space (enables the MMU). It identity-maps this
        // code, the stack, and the device MMIO, so execution continues.
        // SAFETY: the space identity-maps `pc`, `sp`, and MMIO (RAM
        // Normal, device-0 Device) per `new_identity_gigapages`.
        unsafe {
            space.activate();
        }

        // Drive the shared checks: positive control, then a real abort on
        // the read side and the write side of the window.
        let mut scratch = [0u8; 64];
        // SAFETY: `UNMAPPED_VA` lies beyond the identity space just
        // activated, so the page is genuinely unmapped and the deliberate
        // aborts are absorbed by the guarded copy window armed above.
        let verdict = unsafe { conformance::run(UNMAPPED_VA as *mut u8, &mut scratch) };
        match verdict {
            Verdict::Pass => {
                check_pan(probe);
                note(
                    Level::Info,
                    UA_TEST_PASS,
                    "aarch64 uaccess-fault test: in-window aborts surfaced as errors",
                );
                qemu_exit::exit_success();
            }
            Verdict::NotInstalled => fail("guarded copy not installed", FAIL_NOT_INSTALLED),
            Verdict::IntactCopyBroken => fail("intact copy broken", FAIL_INTACT_COPY),
            Verdict::FaultNotReported => {
                fail("abort not reported as an error", FAIL_FAULT_NOT_REPORTED)
            }
        }
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
