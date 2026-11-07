//! Stage 3b QEMU integration test: the aarch64 MMU enforces memory
//! isolation between two stage-1 address spaces.
//!
//! ## What this test asserts
//!
//! The Stage-3 per-sub-stage checklist requires that "the
//! memory-isolation test passes" on each architecture — that the MMU,
//! not software, isolates one address space's frames from another. This binary exercises that on the aarch64 `virt`
//! board, end to end:
//!
//! 1. Build two process roots that disagree on a single page at
//!    `VICTIM_VA`: the *victim* maps it, the *attacker* does not. Neither
//!    carries the kernel, which runs from `TTBR1_EL1`.
//! 2. Install an EL1 vector table and a fault handler.
//! 3. Activate the *victim* and read `VICTIM_VA`, proving the page is
//!    mapped and activation loads a root; then activate the *attacker* and
//!    read it again.
//! 4. The MMU raises a translation (data abort) fault; the handler
//!    confirms it is an abort on exactly `VICTIM_VA` under the attacker and
//!    reports PASS through the ARM semihosting finisher. Any other outcome
//!    reports FAILURE.
//!
//! A regression that fails to isolate the page never faults, so the read
//! returns and the binary reports FAILURE explicitly (it does not hang).
//!
//! ## How it differs from a production kernel
//!
//! It links only the `tairix-arch-aarch64` port and supplies its own
//! `kernel_main`. The QEMU-exit shortcut lives in this dedicated bin,
//! never behind a Cargo feature on the arch crate (fail closed).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel {
    use core::num::NonZeroU16;
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_aarch64::paging::{self, AddressSpace, PageTablePool, PAGE_SIZE};
    use tairix_arch_aarch64::{exceptions, fault, handle_panic_via_serial, qemu_exit, SERIAL_SINK};
    use tairix_arch_api::fatal::KernelFault;
    use tairix_arch_api::mmu::{AddressSpace as _, PageFlags};
    use tairix_itest_finisher::fail_point;
    use tairix_log::{log, Event, EventId, Level};

    /// Virtual address the victim space maps and the attacker does not: a
    /// user-regime address, which only the active process root translates.
    const VICTIM_VA: u64 = 64 << 30;

    /// Stable audit-event ids for the QEMU transcript.
    const MISO_TEST_START: EventId = EventId(4230);
    const MISO_TEST_PASS: EventId = EventId(4231);
    const MISO_TEST_FAIL: EventId = EventId(4232);
    /// Failure finisher codes, distinct per failure site.
    const FAIL_NO_FAULT: NonZeroU16 = fail_point!(2);
    const FAIL_UNEXPECTED_FAULT: NonZeroU16 = fail_point!(3);
    const FAIL_SETUP: NonZeroU16 = fail_point!(4);
    /// The victim's own root did not read its page back.
    const FAIL_VICTIM_UNREAD: NonZeroU16 = fail_point!(5);

    /// Page-table pool backing both address spaces (lives in `.bss`).
    static POOL: PageTablePool = PageTablePool::new();

    /// What every byte of [`VICTIM_PAGE`] holds, so the victim's read proves it
    /// reached that page.
    const VICTIM_BYTE: u8 = 0x5A;

    /// Set just before the attacker's root is loaded: a fault before then is
    /// the victim's own read failing, not isolation.
    static ATTACKER_ACTIVE: AtomicBool = AtomicBool::new(false);

    /// A 4 KiB page the victim space maps at [`VICTIM_VA`]. The attacker never
    /// maps it.
    #[repr(C, align(4096))]
    struct VictimPage([u8; PAGE_SIZE]);
    static mut VICTIM_PAGE: VictimPage = VictimPage([VICTIM_BYTE; PAGE_SIZE]);

    /// The fault handler: confirm the trap is a data/instruction abort on
    /// exactly [`VICTIM_VA`] under the attacker's root, then report PASS.
    /// Anything else is a FAILURE. Never returns.
    fn on_fault(trap: KernelFault) -> ! {
        if ATTACKER_ACTIVE.load(Ordering::SeqCst)
            && trap.syndrome.is_some_and(fault::is_abort)
            && trap.address == Some(VICTIM_VA)
        {
            log(
                &SERIAL_SINK,
                &Event {
                    level: Level::Info,
                    id: MISO_TEST_PASS,
                    message: "aarch64 memory-isolation test: attacker faulted on victim page",
                    fields: &[],
                },
            );
            qemu_exit::exit_success();
        }
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Error,
                id: MISO_TEST_FAIL,
                message: "aarch64 memory-isolation test: unexpected fault",
                fields: &[],
            },
        );
        qemu_exit::exit_failure(FAIL_UNEXPECTED_FAULT);
    }

    /// Forward to the shared aarch64 panic bridge (parks the CPU; the run
    /// then times out and the harness reports the failure).
    #[panic_handler]
    fn tairix_memory_isolation_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s`
    /// trampoline calls (via `tairix_arch_aarch64_main`).
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Info,
                id: MISO_TEST_START,
                message: "aarch64 memory-isolation test: building address spaces",
                fields: &[],
            },
        );

        let victim_pa = paging::image_phys(core::ptr::addr_of!(VICTIM_PAGE) as u64);

        // Victim space: the one VICTIM_VA mapping.
        let mut victim = AddressSpace::new_user(&POOL).unwrap_or_else(|| fail("victim root"));
        // Install the victim mapping through the Arch HAL MMU surface
        // (`tairix_arch_api::mmu::AddressSpace::map_page`), the path
        // the architecture-neutral kernel uses (`plans/WIRING.md` W5b).
        victim
            .map_page(VICTIM_VA, victim_pa, PageFlags::READ | PageFlags::WRITE)
            .unwrap_or_else(|_| fail("victim map_page"));

        // Attacker space: nothing mapped — VICTIM_VA stays unmapped.
        let attacker = AddressSpace::new_user(&POOL).unwrap_or_else(|| fail("attacker root"));

        // Install the vector table and fault handler before either read so an
        // abort is routed to `on_fault`.
        tairix_arch_api::fault::set_fault_handler(on_fault)
            .unwrap_or_else(|_| fail("set_fault_handler"));
        // SAFETY: the boot CPU, a stack established, interrupts masked.
        unsafe {
            exceptions::init_vectors();
        }

        // SAFETY: the kernel runs from `TTBR1_EL1`, which loading a process
        // root leaves untouched.
        unsafe {
            victim.activate();
        }
        // SAFETY: the victim's root maps VICTIM_VA onto VICTIM_PAGE for EL1.
        if unsafe { core::ptr::read_volatile(VICTIM_VA as *const u8) } != VICTIM_BYTE {
            log(
                &SERIAL_SINK,
                &Event {
                    level: Level::Error,
                    id: MISO_TEST_FAIL,
                    message: "aarch64 memory-isolation test: victim root did not read its page",
                    fields: &[],
                },
            );
            qemu_exit::exit_failure(FAIL_VICTIM_UNREAD);
        }

        ATTACKER_ACTIVE.store(true, Ordering::SeqCst);
        // SAFETY: as the victim's activation above.
        unsafe {
            attacker.activate();
        }

        // Read the victim-only address. With the attacker space active
        // this must raise a translation fault → `on_fault` (which exits).
        // SAFETY: the access is *expected* to fault; if the MMU wrongly
        // permitted it the read is still of a valid pointer-sized region
        // we then report as a FAILURE below.
        let observed = unsafe { core::ptr::read_volatile(VICTIM_VA as *const u8) };

        // Reaching here means the attacker read the victim page without a
        // fault — isolation FAILED. Reference `observed` so the read is
        // not elided, and `victim` so it is not dropped before the switch.
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Error,
                id: MISO_TEST_FAIL,
                message: "aarch64 memory-isolation test: attacker read victim page (no fault)",
                fields: &[],
            },
        );
        let _ = (observed, victim.root_phys());
        qemu_exit::exit_failure(FAIL_NO_FAULT);
    }

    /// Log a setup failure and report it to QEMU. Never returns.
    fn fail(what: &str) -> ! {
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Error,
                id: MISO_TEST_FAIL,
                message: "aarch64 memory-isolation test: setup failed",
                fields: &[tairix_log::Field {
                    key: "stage",
                    value: tairix_log::FieldValue::Str(what),
                }],
            },
        );
        qemu_exit::exit_failure(FAIL_SETUP);
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
