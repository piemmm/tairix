//! `tests/SECURITY.md` §5 `copy_from_user` hardware fault fix-up
//! (x86_64) QEMU integration test: a kernel-mode `#PF` taken inside the
//! guarded user-copy window is redirected to the copy's fix-up and
//! surfaces as an error — the CPU keeps running instead of halting.
//!
//! ## Why this exists
//!
//! The kernel's validated user-copy path (`kernel/mem::uaccess`) proves
//! every page before it moves a byte, so a mid-copy hardware fault means
//! that proof was violated underneath it. The per-port fault window
//! (`tairix_arch_x86_64::uaccess`) is the backstop that turns such a
//! fault into an error return; this vertical proves the whole mechanism
//! live: real kernel-mode `#PF` → dedicated `#PF` entry → frame-`RIP`
//! rewrite → fix-up return.
//!
//! ## What this test asserts
//!
//! 1. The **production** boot pipeline (`tairix_kernel::boot`) installs
//!    the dedicated `#PF` entry *and* arms the Arch HAL guarded-copy
//!    slot — the pairing under test lives on the real boot path, not a
//!    test-only install.
//! 2. On `AuditEvent::BootCompleted`, the shared
//!    `tairix_arch_api::uaccess::conformance` checks pass: an intact
//!    copy moves its bytes exactly; a copy reading an unmapped canonical
//!    address returns the fault error; a copy writing it returns the
//!    fault error — each taking a *real* kernel-mode `#PF` whose
//!    redirect lets execution continue to the next check.
//! 3. The kernel holds the CPU to every supervisor protection the guest's
//!    model offers: `CR4.SMEP`, `CR4.SMAP` and `CR4.UMIP` are set, the boot
//!    recorded all three, and a guarded copy reading a mapped user page
//!    through its user address faults while the same bytes copy through the
//!    direct map — so SMAP is enforced, with `AC` clear, not merely enabled.
//! 4. The fatal `fault` observer reports FAILURE: reaching it means the
//!    window redirect did not absorb the deliberate fault.
//!
//! ## How it differs from the production `tairix-kernel` binary
//!
//! Only the audit Sink is replaced (to hook `BootCompleted`) and the
//! fault observer installed (to fail loud). The QEMU-exit shortcut lives
//! in this dedicated bin, never behind a Cargo feature on the kernel
//! (fail closed).

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`x86_64-tairix-none`) -----------------

#[cfg(itest_x86_64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicU32, Ordering};

    use tairix_arch_api::fatal::KernelFault;
    use tairix_arch_api::mmu::{AddressSpace as _, PageFlags};
    use tairix_arch_api::uaccess::conformance::{self, reach_into, Reach, Verdict};
    use tairix_arch_x86_64::paging::{self, AddressSpace, PageTablePool};
    use tairix_arch_x86_64::protection::{self, Protections};
    use tairix_arch_x86_64::qemu_exit;
    use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
    use tairix_kernel::KERNEL_BOOT_PROTECTIONS;
    use tairix_kernel::{
        boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink, SERIAL_SINK,
    };
    use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};

    /// A page-aligned canonical virtual address the production kernel
    /// space leaves unmapped (256 GiB: far beyond the low identity
    /// window and below the higher-half kernel window), so any access to
    /// it raises a kernel-mode not-present `#PF`.
    const UNMAPPED_VA: u64 = 0x40_0000_0000;

    /// `EventId` emitted when every boot init phase completed. Pinned by
    /// the `event_ids_are_unique` test in `kernel/core/src/audit.rs`.
    const BOOT_COMPLETED_EVENT_ID: EventId = EventId(4004);

    /// Stable audit-event ids for the QEMU transcript (clear of the
    /// `4000..5000` `kernel/core` boot range collisions used by other
    /// tests).
    const UA_TEST_START: EventId = EventId(4340);
    const UA_TEST_PASS: EventId = EventId(4341);
    const UA_TEST_FAIL: EventId = EventId(4342);

    /// Set once the conformance run has been driven so a duplicate
    /// `BootCompleted` cannot re-enter the test logic.
    static TEST_DRIVEN: AtomicU32 = AtomicU32::new(0);

    /// Set when the boot recorded every protection in force.
    static PROTECTIONS_RECORDED: AtomicU32 = AtomicU32::new(0);

    /// A user-half address the probe root maps [`PROBE_PAGE`] at.
    const USER_PROBE_VA: u64 = 1 << 32;

    /// The byte every word of [`PROBE_PAGE`] holds.
    const PROBE_BYTE: u8 = 0x5A;

    /// The page the SMAP probe reads by a user address and through the
    /// direct map. Never written, so a read proves which mapping reached it.
    #[repr(C, align(4096))]
    struct ProbePage([u8; 4096]);

    static PROBE_PAGE: ProbePage = ProbePage([PROBE_BYTE; 4096]);

    /// Page tables of the root the SMAP probe maps the page into.
    static PROBE_POOL: PageTablePool = PageTablePool::new();

    /// The global allocator's arena.
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and
    /// the allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

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

    /// Log a check failure and report it to QEMU. Never returns.
    fn fail(what: &'static str) -> ! {
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Error,
                id: UA_TEST_FAIL,
                message: "x86_64 uaccess-fault test: failed",
                fields: &[Field {
                    key: "stage",
                    value: tairix_log::FieldValue::Str(what),
                }],
            },
        );
        qemu_exit::exit_failure();
    }

    /// The fatal `#PF` observer: reaching it means a fault escaped the
    /// guarded-copy window redirect (or something else faulted) — a
    /// closed failure either way.
    fn on_fault(_trap: KernelFault) -> ! {
        fail("fault escaped the copy window redirect")
    }

    /// The guest's model offers SMEP, SMAP and UMIP, so the kernel holds the
    /// CPU to all three, and under SMAP a kernel read of a user page faults
    /// even inside the guarded copy, while the same bytes copy through the
    /// direct map.
    fn check_protections() -> Result<(), &'static str> {
        const HELD: u64 = (1 << 11) | (1 << 20) | (1 << 21);
        if protection::in_force() != Protections::ALL {
            return Err("the kernel does not hold every protection the guest offers");
        }
        let cr4: u64;
        // SAFETY: reading `CR4` at CPL 0 changes nothing.
        unsafe { core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack)) };
        if cr4 & HELD != HELD {
            return Err("CR4 does not hold SMEP, SMAP and UMIP");
        }
        if PROTECTIONS_RECORDED.load(Ordering::Acquire) == 0 {
            return Err("the boot did not record the protections in force");
        }
        let phys = core::ptr::addr_of!(PROBE_PAGE) as u64 - paging::KERNEL_VMA_BASE;
        let Some(mut root) = AddressSpace::new_process_root(&PROBE_POOL) else {
            return Err("no probe root");
        };
        if root
            .map_page(USER_PROBE_VA, phys, PageFlags::READ | PageFlags::USER)
            .is_err()
        {
            return Err("the probe page would not map");
        }
        // The user address must reach the page, so its fault can only be
        // SMAP's.
        if root
            .translate(USER_PROBE_VA)
            .is_none_or(|(to, flags)| to != phys || !flags.contains(PageFlags::USER))
        {
            return Err("the probe's user address does not reach the page");
        }
        // SAFETY: the root carries the kernel half, so this code, its stack
        // and every static stay mapped across the switch; the test ends on
        // this root.
        unsafe { root.switch() };
        // SAFETY: `USER_PROBE_VA` maps the probe page as a user page under
        // the root just switched to, and its direct-map alias is the kernel's
        // own mapping of it.
        match unsafe { reach_into(USER_PROBE_VA, paging::physmap_virt(phys), PROBE_BYTE) } {
            Reach::OwnMappingOnly => Ok(()),
            Reach::UserAddressRead => Err("a kernel read of a user page did not fault"),
            Reach::OwnMappingUnread => Err("the direct map did not read the page"),
        }
    }

    /// Whether `event` is the boot's record of every protection in force.
    fn records_every_protection(event: &Event<'_>) -> bool {
        event.id == KERNEL_BOOT_PROTECTIONS
            && event.level == Level::Info
            && ["smep", "smap", "umip"].iter().all(|key| {
                event
                    .fields
                    .iter()
                    .any(|field| field.key == *key && field.value == FieldValue::Bool(true))
            })
    }

    /// Drive the shared checks: positive control, then a real `#PF` on
    /// the read side and the write side of the window, then the
    /// supervisor protections. Never returns.
    fn run_checks() -> ! {
        note(
            Level::Info,
            UA_TEST_START,
            "x86_64 uaccess-fault test: faulting inside the guarded copy window",
        );
        let mut scratch = [0u8; 64];
        // SAFETY: `UNMAPPED_VA` has no translation in the boot address
        // space, so the page is genuinely unmapped and the deliberate
        // page faults are absorbed by the guarded copy window the boot
        // pipeline armed before `BOOT_COMPLETED` fired.
        let verdict = unsafe { conformance::run(UNMAPPED_VA as *mut u8, &mut scratch) };
        match verdict {
            Verdict::Pass => {
                if let Err(why) = check_protections() {
                    fail(why);
                }
                note(
                    Level::Info,
                    UA_TEST_PASS,
                    "x86_64 uaccess-fault test: in-window faults surfaced as errors",
                );
                qemu_exit::exit_success();
            }
            Verdict::NotInstalled => fail("guarded copy not installed"),
            Verdict::IntactCopyBroken => fail("intact copy broken"),
            Verdict::FaultNotReported => fail("fault not reported as an error"),
        }
    }

    /// Sink that forwards every event to the serial log (so the QEMU
    /// transcript captures the boot timeline), notes the boot's record of the
    /// protections in force, and, on the single [`BOOT_COMPLETED_EVENT_ID`],
    /// drives [`run_checks`]. The boot logs and audits through it alike.
    struct BootCompletedSink;

    impl Sink for BootCompletedSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);

            if records_every_protection(event) {
                PROTECTIONS_RECORDED.store(1, Ordering::Release);
            }
            if event.id == BOOT_COMPLETED_EVENT_ID
                && TEST_DRIVEN
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                run_checks();
            }
        }
    }

    static AUDIT_SINK: BootCompletedSink = BootCompletedSink;

    /// Forward to the shared bridge in `tairix_kernel`.
    #[panic_handler]
    fn tairix_uaccess_fault_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_kernel_core(info)
    }

    /// The symbol the arch crate's boot trampoline calls.
    #[no_mangle]
    pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
        // Claim the set-once fatal-fault slot before `boot` publishes the
        // production reporter into it: this vertical is the fail-loud
        // observer of its own deliberate in-window faults, so it owns the
        // machine's fatal policy for this image and must be first. The
        // production pipeline installs the dedicated `#PF` entry and arms
        // the guarded-copy slot itself.
        if tairix_arch_api::fault::set_fault_handler(on_fault).is_err() {
            fail("set_fault_handler");
        }
        boot(
            multiboot_info,
            &ALLOCATOR,
            &AUDIT_SINK,
            &AUDIT_SINK,
            tairix_log::Level::Info,
        )
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_x86_64))]
fn main() {}
