//! WIRING Stage W6 QEMU integration test: cross-CPU TLB shootdown on the
//! aarch64 `virt` board.
//!
//! ## What this test asserts
//!
//! The `tairix_arch_api::CrossCpuTlbShootdown` HAL slice requires that a
//! page-table edit on one CPU can be made visible on the others. On
//! aarch64 this needs no IPI: `Aarch64Arch::shootdown_page` issues the
//! *inner-shareable broadcast* `tlbi vaae1is` + `dsb ish`/`isb`, which the
//! hardware propagates to every PE in the inner-shareable domain. This
//! binary proves that path on a real two-core `virt` board:
//!
//! 1. The boot core starts core 1 via `smp::start_secondary` (the PSCI
//!    `CPU_ON` call); core 1 signals `READY` and idles, so the domain
//!    genuinely contains a second PE.
//! 2. The boot core drives `Aarch64Arch::shootdown_page` — the broadcast
//!    invalidation + barriers — and reaches the PASS finisher, proving the
//!    broadcast executes on a real multi-core machine without faulting.
//! 3. Both cores hold PAN: the enrolment runs `cortex-a76`, which offers it,
//!    and the port sets `PSTATE.PAN` on each core as it starts. Translation
//!    stays off throughout, where no page is EL0's, so the watchdog's
//!    stack-link probe still lets EL1 read its own stack.
//!
//! Unlike the `riscv64`/`x86_64` verticals there is no software acknowledge to
//! observe: the broadcast and its `dsb ish` completion are the hardware's
//! responsibility, exactly as the local `flush_page` (the same instruction)
//! is already exercised by `memory_isolation_qemu_aarch64`.
//!
//! A regression that fails to start the core or that faults in
//! `shootdown_page` never reaches the PASS write, so the run times out or
//! trips a failure finisher — the documented fail-loud behaviour.
//!
//! ## PSCI conduit
//!
//! As in `ipi_smp_qemu_aarch64`, the QEMU `virt` board's conduit is `hvc`
//! and QEMU's ELF `-kernel` boot hands no DTB pointer, so the test names
//! the board's known conduit directly; the runtime FDT-discovery path is
//! proven by W1's tests, not re-proved here.
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
    use core::sync::atomic::{AtomicU32, Ordering};

    use tairix_arch_aarch64::kernel_arch::read_cntfrq;
    use tairix_arch_aarch64::{
        fdt, handle_panic_via_serial, paging, protection, qemu_exit, smp, Aarch64Arch,
        Aarch64ArchStorage, SERIAL_SINK,
    };
    use tairix_arch_api::{CpuId, CrossCpuTlbShootdown, BOOT_CPU};
    use tairix_itest_finisher::fail_point;
    use tairix_log::{log, Event, EventId, Level};

    /// Dense id of the secondary core this test starts.
    const SECONDARY_CPU: CpuId = 1;

    /// `MPIDR_EL1` affinity QEMU assigns core 1 on the `virt` board (the
    /// linear core index).
    const SECONDARY_MPIDR: u64 = SECONDARY_CPU as u64;

    /// PSCI conduit on the QEMU `virt` board (no EL3 → `hvc`); see the
    /// module docs.
    const VIRT_PSCI_METHOD: fdt::PsciMethod = fdt::PsciMethod::Hvc;

    /// A representative page to invalidate. The exact address is
    /// immaterial — a TLB shootdown can only ever *over*-invalidate.
    const SHOOTDOWN_VADDR: u64 = 0x4020_0000;

    /// Stable audit-event ids for the QEMU transcript.
    const TEST_START: EventId = EventId(4264);
    const SECONDARY_UP: EventId = EventId(4265);
    const TEST_PASS: EventId = EventId(4266);

    /// Failure finisher code: the secondary core never came up.
    const FAIL_SECONDARY_START: NonZeroU16 = fail_point!(1);
    /// Failure finisher code: `CNTFRQ_EL0` reported a zero frequency.
    const FAIL_ZERO_FREQ: NonZeroU16 = fail_point!(3);
    /// Failure finisher code: a core does not hold PAN on a CPU offering it.
    const FAIL_PAN_NOT_HELD: NonZeroU16 = fail_point!(4);
    /// Failure finisher code: with translation off, the stack-link probe
    /// refused the core's own stack.
    const FAIL_STACK_PROBE: NonZeroU16 = fail_point!(5);

    /// `PSTATE.PAN`, as the `PAN` register reads it.
    const PSTATE_PAN: u64 = 1 << 22;

    /// Set to `1` by the secondary core when it found `PSTATE.PAN` set.
    static SECONDARY_PAN: AtomicU32 = AtomicU32::new(0);

    /// Set to `1` by the secondary core once it is up and idling, so the
    /// inner-shareable domain genuinely contains a second PE when the boot
    /// core broadcasts.
    static SECONDARY_READY: AtomicU32 = AtomicU32::new(0);

    /// Entry the secondary core runs (via the `smp.s` trampoline). It only
    /// needs to be a *running* PE in the inner-shareable domain for the
    /// broadcast to reach it, so it signals ready and idles — no GIC or
    /// vector setup is required.
    extern "C" fn secondary_entry(_cpu: CpuId) -> ! {
        if pan_set() {
            SECONDARY_PAN.store(1, Ordering::SeqCst);
        }
        SECONDARY_READY.store(1, Ordering::SeqCst);
        loop {
            // SAFETY: `wfi` is a wait-for-interrupt hint with no
            // architectural side effects.
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
            }
        }
    }

    /// Whether the calling core holds PAN; `false` while PAN is not in force,
    /// when the core may not even have the register.
    fn pan_set() -> bool {
        if !protection::pan_in_force() {
            return false;
        }
        let pan: u64;
        // SAFETY: PAN is in force, so every running core offers the register;
        // reading it changes nothing.
        unsafe {
            core::arch::asm!(
                "mrs {}, S3_0_C4_C2_3",
                out(reg) pan,
                options(nomem, nostack, preserves_flags),
            );
        }
        pan & PSTATE_PAN != 0
    }

    /// Forward to the shared aarch64 panic bridge (parks the core; the run
    /// then times out and the harness reports the failure).
    #[panic_handler]
    fn tairix_xtlb_aarch64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_serial(info)
    }

    /// Boot entry point — the symbol the arch crate's `boot.s` trampoline
    /// calls (via `tairix_arch_aarch64_main`).
    #[no_mangle]
    pub extern "C" fn kernel_main(_dtb: u64) -> ! {
        // Per-CPU bookkeeping backing for this two-core vertical.
        static ARCH_STORAGE: Aarch64ArchStorage<2> = Aarch64ArchStorage::new();
        // The secondary-core stack pool, sized to this two-core vertical: the
        // `smp.s` trampoline reads its published base and stride to seed the
        // started core's stack.
        static SECONDARY_STACKS: smp::SecondaryStackPool<2> = smp::SecondaryStackPool::new();

        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Info,
                id: TEST_START,
                message: "aarch64 cross-CPU TLB shootdown test: starting secondary core",
                fields: &[],
            },
        );

        let counter_hz = read_cntfrq();
        if counter_hz == 0 {
            qemu_exit::exit_failure(FAIL_ZERO_FREQ);
        }

        let arch = Aarch64Arch::with_cpus(
            &ARCH_STORAGE,
            BOOT_CPU,
            counter_hz,
            &[u64::from(BOOT_CPU), SECONDARY_MPIDR],
        );

        // Registered before any `CPU_ON`.
        if SECONDARY_STACKS.register().is_err() {
            qemu_exit::exit_failure(FAIL_SECONDARY_START);
        }

        if smp::set_secondary_entry(secondary_entry).is_err() {
            qemu_exit::exit_failure(FAIL_SECONDARY_START);
        }
        // SAFETY: called on the boot core after the secondary-stack pool
        // was registered (above) and the secondary entry was installed;
        // `SECONDARY_MPIDR` names a real, parked, distinct core.
        if unsafe { smp::start_secondary(VIRT_PSCI_METHOD, SECONDARY_CPU, SECONDARY_MPIDR) }
            .is_err()
        {
            qemu_exit::exit_failure(FAIL_SECONDARY_START);
        }

        // Wait until the secondary core is up, so the broadcast targets a
        // genuinely multi-PE inner-shareable domain.
        while SECONDARY_READY.load(Ordering::SeqCst) == 0 {
            core::hint::spin_loop();
        }
        if !pan_set() || SECONDARY_PAN.load(Ordering::SeqCst) == 0 {
            qemu_exit::exit_failure(FAIL_PAN_NOT_HELD);
        }
        let on_stack = 0u64;
        if !paging::el1_may_read(core::ptr::addr_of!(on_stack) as u64) {
            qemu_exit::exit_failure(FAIL_STACK_PROBE);
        }
        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Info,
                id: SECONDARY_UP,
                message: "aarch64 cross-CPU TLB shootdown test: secondary up, broadcasting",
                fields: &[],
            },
        );

        // Drive the real HAL entry point: the inner-shareable broadcast
        // `tlbi vaae1is` + `dsb ish`/`isb`. Reaching the next line proves
        // the broadcast executed on a real two-core machine without
        // faulting.
        arch.shootdown_page(SHOOTDOWN_VADDR);

        log(
            &SERIAL_SINK,
            &Event {
                level: Level::Info,
                id: TEST_PASS,
                message: "aarch64 cross-CPU TLB shootdown test: broadcast invalidation completed",
                fields: &[],
            },
        );
        qemu_exit::exit_success();
    }
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
