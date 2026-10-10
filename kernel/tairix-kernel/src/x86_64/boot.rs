//! Bare-metal boot pipeline for the x86_64 `tairix-kernel` binary.
//!
//! [`boot`] is the single entry point. It is called from each
//! binary's `extern "C" fn kernel_main(boot_info: u64)` after the
//! arch crate's [`tairix_arch_x86_64::entry`] trampoline has validated
//! the boot magic (multiboot2 or PVH) and recorded the protocol. It
//! performs the BSP bring-up
//! sequence the prompt for Stage 3a (c7-bin) lays out — boot info →
//! ACPI/MADT → `BootMemoryMap`; `X86_64Arch::new`; per-CPU
//! `percpu::init` → `preempt::init_local_preempt` →
//! `syscall_entry::init_local_syscalls`; install the fail-closed
//! syscall-dispatch callback **before** `syscall` is enabled — and
//! then hands a fully-validated [`tairix_kernel_core::BootInfo`] to
//! [`tairix_kernel_core::kernel_main`].
//!
//! # SAFETY-INVARIANTs
//!
//! Each step of [`boot`] is the unsafe shim into one of the
//! architecture port's audited primitives. The invariants the arch
//! crate documents on those primitives are upheld here:
//!
//! * `percpu::init(0)` runs exactly once with interrupts disabled
//!   (the boot trampoline leaves `IF` clear, and we never `sti`
//!   ourselves — `kernel_core::kernel_main` halts at the end of
//!   `BootCompleted`).
//! * `set_dispatch_callback` is invoked **before**
//!   `init_local_syscalls`, satisfying the trampoline's "callback
//!   installed before `syscall` is enabled" requirement (see
//!   `tairix_arch_x86_64::syscall_entry` rustdoc and).
//! * `init_local_preempt` and `init_local_syscalls` run with
//!   `cpu_index = 0` on the BSP after `percpu::init(0)`, satisfying their
//!   per-call SAFETY contracts.
//! * The boot-info pointer is dereferenced only through the audited
//!   `bootinfo::BootData::load` validator, which bounds every slice
//!   before parsing (the multiboot2 `total_size`, the PVH stated
//!   entry count).
//!
//! # No `unwrap` / `expect` / `panic!` in production paths
//!
//! the charter forbids panics in production paths. Every fallible
//! step inside [`boot`] returns a [`BootError`]; the outer function
//! reports the failure through the log sink and halts the CPU
//! forever via [`tairix_arch_x86_64::kernel_arch::halt`]. The CPU
//! never returns to the trampoline (the boot stub assumes
//! `kernel_main` does not return — `boot.s` SAFETY-INVARIANT 7).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::sysinfo::{DmaUnitFamily, DmaUnitState};
use tairix_abi::SYSCALL_MAX_ARGS;
use tairix_arch_api::fault;
use tairix_arch_x86_64::acpi::{self, MadtEntry};
use tairix_arch_x86_64::apic::{IoApic, Lapic, LocalApic, VolatileIoApicMmio};
use tairix_arch_x86_64::apic_timer::{self, Calibration, PolledPit, Rdtsc};
use tairix_arch_x86_64::bootinfo::BootData;
use tairix_arch_x86_64::bootmemory;
use tairix_arch_x86_64::gdt::PerCpuGdt;
use tairix_arch_x86_64::irq as arch_irq;
use tairix_arch_x86_64::kernel_arch::{halt as arch_halt, X86_64Arch, X86_64ArchStorage};
use tairix_arch_x86_64::paging;
use tairix_arch_x86_64::{percpu, preempt, syscall_entry};
use tairix_kernel_core::boot_audit_ring::{
    boot_audit_clock, BootAuditRing, BOOT_AUDIT_RING_CAPACITY,
};
use tairix_kernel_core::{kernel_main, BootInfo, InitSpawn, IrqRouting};
use tairix_kernel_irq::IrqController;
use tairix_kernel_mem::{BootMemoryMap, MemoryRegion, PhysAddr, RegionKind};
use tairix_kernel_sched_api::SchedulerConfig;
use tairix_log::{Event, EventId, Field, Level, Sink, TeeSink};

use tairix_arch_x86_64::irqmask::RflagsIrqControl;
use tairix_arch_x86_64::serial::SERIAL_SINK;

use crate::x86_64::arch_wrapper::BinArch;
use crate::x86_64::dispatch::{
    production_dispatch, production_user_fault, production_user_fault_terminate, DISPATCH_SLOT,
};
use crate::x86_64::init_spawn::X86_64_INIT_SPAWN;
use crate::x86_64::ioapic_controller::IoApicController;
use crate::x86_64::serial_sink::COM1_CONSOLES;

/// `IA32_EFER` MSR number and its No-Execute-Enable bit (bit 11). Enabling
/// `NXE` lets the W^X No-Execute leaf bit the process-image builder sets on
/// data/rodata pages and the user stack be honoured rather than treated as a
/// reserved bit that faults the page-table walk.
const IA32_EFER: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;

// --- BSP boot configuration ----------------------------------------

/// LAPIC-timer period programmed during BSP bring-up.
///
/// 1 ms matches the value the existing `scheduler_stress_qemu` test
/// uses; consistency removes one source of "why is QEMU TCG behaving
/// differently here?" noise from the boot test (no
/// flaky tests, no avoidable jitter). The timer is armed but no
/// callback is installed, so each tick is a no-op except for the EOI
/// — see `tairix_arch_x86_64::preempt::tairix_arch_x86_64_timer_dispatch`.
const PREEMPT_PERIOD_US: u32 = 1_000;

/// PIT calibration window. 10 ms is the universally-attested PIT
/// calibration period (the channel-2 reload fits in 16 bits up to
/// ~54 ms).
const PREEMPT_CALIBRATION_WINDOW_US: u32 = 10_000;

/// Per-CPU kernel-stack size in bytes.
///
/// 64 KiB matches the BSP bootstrap stack in `kernel/arch/x86_64::boot.s`.
/// The stack hosts the kernel side of a `syscall` transition (frame
/// layout in `syscall_entry::syscall_entry_stub`) and, in the QEMU
/// integration verticals, a full device-bring-up scenario driven
/// synchronously on the boot thread — including a filesystem `open`
/// that stages whole blocks through on-stack scratch buffers. The
/// earlier 16 KiB was marginal for that nested path; 64 KiB gives ample
/// headroom.
const KERNEL_STACK_BYTES: usize = 64 * 1024;

/// Number of logical CPUs the production `tairix-kernel` boot path
/// brings up. It runs **single-CPU** (it never drives the
/// `SecondaryBringup` HAL method — that handshake is proven by the QEMU
/// verticals), so every per-CPU backing here is sized to one slot
/// (capacity matches the machine the caller actually
/// drives, not a baked-in `MAX_CPUS` ceiling). A future AP-bring-up
/// commit sizes this from the-discovered MADT processor count.
const BOOT_CPUS: usize = 1;

/// 16-byte-aligned kernel-stack slot. Matches the System V AMD64
/// ABI's 16-byte stack-alignment requirement at function entry.
#[repr(C, align(16))]
struct KernelStack([u8; KERNEL_STACK_BYTES]);

/// Per-CPU kernel stack pool, sized to the [`BOOT_CPUS`] this binary
/// brings up (the BSP). A future AP-bring-up commit sizes it from the
/// discovered CPU count rather than re-introducing a fixed ceiling.
///
/// This is the only `static mut` in the bin crate, justified in `README.md`
/// as the per-CPU bootstrap-stack arena. Access is exclusively through
/// [`kernel_stack_top`], which derives a disjoint pointer per `cpu_index`.
static mut KERNEL_STACKS: [KernelStack; BOOT_CPUS] =
    [const { KernelStack([0; KERNEL_STACK_BYTES]) }; BOOT_CPUS];

/// Per-CPU GDT/IDT/IST arena the arch crate's [`percpu`] entry points
/// index, sized to [`BOOT_CPUS`] and published once by [`try_boot`]
/// before [`percpu::init`].
static PER_CPU_STORAGE: percpu::PerCpuStorage<BOOT_CPUS> = percpu::PerCpuStorage::new();

/// Per-CPU `syscall`-entry TLS arena, sized to [`BOOT_CPUS`] and published
/// once by [`try_boot`] before [`syscall_entry::init_local_syscalls`].
static SYSCALL_TLS_STORAGE: syscall_entry::SyscallTlsStorage<BOOT_CPUS> =
    syscall_entry::SyscallTlsStorage::new();

/// One byte past the top of `KERNEL_STACKS[cpu_index]`.
///
/// `cpu_index < BOOT_CPUS` is the caller's responsibility; [`boot`]
/// satisfies that statically (it only calls with `0`).
fn kernel_stack_top(cpu_index: usize) -> u64 {
    debug_assert!(cpu_index < BOOT_CPUS);
    // SAFETY: `cpu_index < BOOT_CPUS` per the debug assert above
    // (production callers in this module guarantee the bound at the
    // call site too); `addr_of` reads the static's address without
    // creating a Rust reference.
    let base = unsafe { core::ptr::addr_of!(KERNEL_STACKS[cpu_index]) } as u64;
    base + core::mem::size_of::<KernelStack>() as u64
}

// --- Errors --------------------------------------------------------

/// Failure modes of [`boot`].
///
/// Stored as a single `enum` rather than a heap-allocated message
/// string so the boot log emits a stable, machine-readable `cause`
/// field.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BootError {
    /// The boot-info record (multiboot2 or PVH) at the loader-supplied
    /// address could not be parsed.
    BootInfoParse,
    /// The multiboot2 record contains no memory-map tag (BIOS path)
    /// and no UEFI memory-map tag. (A PVH record without a memory map
    /// is already rejected by [`BootData::load`].)
    NoMemoryMap,
    /// The loader published no RSDP, or the RSDP bytes failed
    /// [`acpi::Rsdp::validate`] — either way ACPI discovery cannot
    /// proceed.
    NoRsdp,
    /// No MADT was found by walking the (X|R)SDT.
    NoMadt,
    /// The MADT bytes failed [`acpi::Madt::parse`].
    BadMadt,
    /// No enabled-Processor-Local-APIC entry covered the BSP — every
    /// Multiboot2-published firmware does, so this is a fatal
    /// discovery defect.
    BspLapicMissing,
    /// [`percpu::PerCpuStorage::register`] refused the per-CPU arena
    /// (already registered — a boot-path defect).
    PercpuStorageRegister,
    /// `percpu::init` rejected the BSP.
    PercpuInit,
    /// [`syscall_entry::SyscallTlsStorage::register`] refused the per-CPU
    /// syscall-TLS arena (already registered — a boot-path defect).
    SyscallTlsStorageRegister,
    /// LAPIC-timer calibration against the PIT failed.
    TimerCalibration,
    /// `preempt::init_local_preempt` rejected the BSP.
    PreemptInit,
    /// `syscall_entry::init_local_syscalls` rejected the BSP.
    SyscallInit,
    /// `X86_64Arch::new` rejected the BSP triple.
    ArchInit,
    /// `BootInfo::new`/`validate` rejected the assembled hand-off.
    BootInfoInvalid,
    /// MADT advertised no IO-APIC. Every PCAT/UEFI platform TAIRiX
    /// supports publishes at least one; the absence is a fatal
    /// discovery defect.
    NoIoApic,
    /// `percpu::install_vector` rejected the external-IRQ IDT install.
    /// Surfaces a defect in the per-CPU bootstrap latch or an
    /// out-of-range vector.
    IrqIdtInstall,
    /// The IO-APICs' bookkeeping could not be had.
    IoApicUnrecorded,
    /// The boot CPU's APIC id is one its APIC's mode cannot name as a
    /// CPU's: past the eight bits xAPIC names, or x2APIC's broadcast id.
    BspApicIdUnsupported,
    /// [`fault::set_user_fault_resolver`] refused the production user-fault
    /// resolver (a resolver was already installed). The single-entry
    /// bring-up runs once per boot, so a second occupant is a boot-path
    /// defect — two bring-up attempts, or a caller that installed its own
    /// resolver before booting — and the boot refuses rather than running
    /// with an unpredictable fault path.
    UserFaultResolverInstall,
    /// [`fault::set_user_fault_terminator`] refused the production
    /// user-fault terminator (one was already installed). Same
    /// single-publish contract as the resolver above: without a terminator
    /// a ring-3 exception the port cannot resolve would park the CPU
    /// instead of killing the offending task, so the boot refuses rather
    /// than running with an unpredictable fault path.
    UserFaultTerminatorInstall,
    /// More than one CPU was about to be brought up on a part whose
    /// CPUID does not advertise an Invariant TSC. `RDTSC` is the
    /// x86_64 monotonic clock source, and without the invariant
    /// guarantee it may run at a P-state-dependent rate or drift
    /// between cores, so a migrated task could observe time going
    /// backwards. Rather than silently trust the contract the boot
    /// path fails closed. A single-CPU
    /// boot is unaffected: one TSC is self-monotonic.
    TscNotInvariant,
    /// The direct physical map could not be installed over the discovered
    /// RAM — no usable run below the boot trampoline's identity window
    /// could host its page tables, or the arch install refused the
    /// request. Every kernel path that reaches a frame by pointer would
    /// then fail closed while the allocator kept handing out frames, so
    /// the boot refuses rather than running on RAM it cannot reach.
    DirectMapInstall,
    /// The kernel heap could not hold a copy of the command line, which the
    /// loader left in memory the frame allocator reuses.
    CommandLineCopy,
}

impl BootError {
    /// Stable cause string for audit records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BootInfoParse => "boot_info_parse",
            Self::NoMemoryMap => "no_memory_map",
            Self::NoRsdp => "no_rsdp",
            Self::NoMadt => "no_madt",
            Self::BadMadt => "bad_madt",
            Self::BspLapicMissing => "bsp_lapic_missing",
            Self::PercpuStorageRegister => "percpu_storage_register_failed",
            Self::PercpuInit => "percpu_init_failed",
            Self::SyscallTlsStorageRegister => "syscall_tls_storage_register_failed",
            Self::TimerCalibration => "timer_calibration_failed",
            Self::PreemptInit => "preempt_init_failed",
            Self::SyscallInit => "syscall_init_failed",
            Self::ArchInit => "arch_init_failed",
            Self::BootInfoInvalid => "bootinfo_invalid",
            Self::NoIoApic => "no_io_apic",
            Self::IrqIdtInstall => "irq_idt_install_failed",
            Self::IoApicUnrecorded => "io_apic_unrecorded",
            Self::BspApicIdUnsupported => "bsp_apic_id_unsupported",
            Self::UserFaultResolverInstall => "user_fault_resolver_install_failed",
            Self::UserFaultTerminatorInstall => "user_fault_terminator_install_failed",
            Self::TscNotInvariant => "tsc_not_invariant",
            Self::DirectMapInstall => "direct_map_install_failed",
            Self::CommandLineCopy => "command_line_copy_failed",
        }
    }
}

/// Audit event the boot pipeline emits on failure. Kept separate from
/// the `kernel/core` audit catalogue because the failure happens
/// *before* `kernel_core::kernel_main` is ever entered (and therefore
/// before its phase events have any meaning).
///
/// `EventId(4099)` sits in the `4000..5000` range owned by `kernel/core`
/// (per `lib/log`'s subsystem ranges) but at the top of the range so
/// it cannot collide with any phase-numbered event. The id is part of
/// the audit contract with external consumers and may not be renumbered.
const KERNEL_BOOT_INIT_FAILED: EventId = EventId(4099);

/// Security-relevant boot decision: whether the BSP's CPUID advertises
/// an Invariant TSC. Logged on every boot so the
/// TSC contract is recorded rather than silently assumed. Sits in the
/// `kernel/core`-owned `4000..5000` range, just below
/// [`KERNEL_BOOT_INIT_FAILED`]; the id is part of the audit contract
/// and may not be renumbered.
const KERNEL_BOOT_TSC_INVARIANCE: EventId = EventId(4098);

// --- Retained boot audit log ----------------------------------------

/// The retained, tail-able in-memory boot audit ring for this port.
///
/// Composed into the boot audit channel through [`AUDIT_SINK`], so every
/// audit record the kernel emits from the earliest boot onward is teed into
/// it and can be read back non-destructively — the store the pre-boot
/// Supervisor's `log` command tails (`plans/NEW-SUPERVISOR.md`). It is
/// guarded by the port's one interrupt-masking primitive, so a record copy
/// masks this CPU's interrupts for its short, allocation-free duration and the
/// ring is safe to write from an interrupt handler that logs. It stamps each
/// record with the kernel's monotonic since-boot clock ([`boot_audit_clock`]).
pub static BOOT_AUDIT_RING: BootAuditRing<BOOT_AUDIT_RING_CAPACITY, RflagsIrqControl> =
    BootAuditRing::new(boot_audit_clock);

/// The production boot **audit** channel: a fan-out delivering each record to
/// both the COM1 serial console ([`SERIAL_SINK`]) and the retained
/// [`BOOT_AUDIT_RING`].
///
/// `main.rs` passes this as [`BootInfo`]'s audit sink for the production
/// binary; the QEMU boot verticals substitute their own audit sink through
/// [`boot`] directly, so retaining the trail is a production-only wiring and
/// never disturbs a test's audit interception.
pub static AUDIT_SINK: TeeSink<'static, 2> = TeeSink::new([&SERIAL_SINK, &BOOT_AUDIT_RING]);

// --- The boot entry -------------------------------------------------

/// Boot the kernel on the BSP and forward to
/// [`tairix_kernel_core::kernel_main`].
///
/// `log_sink` and `audit_sink` are the `&'static` sinks installed in
/// [`tairix_kernel_core::BootInfo`]: the production binary uses a
/// COM1-backed sink for both; the QEMU integration test substitutes
/// the audit sink with one that flips the QEMU `isa-debug-exit`
/// device on `AuditEvent::BootCompleted`.
///
/// `log_level` is the initial global log filter `kernel_main` installs
/// (`BootInfo::log_level`). Production passes [`Level::Info`]; an audit
/// observer vertical that must see the `Debug`-level allow records (e.g.
/// `SyscallInvoked`, `EventId(5000)`) passes [`Level::Debug`].
///
/// Returns the bottom type. On every failure the function logs one
/// [`KERNEL_BOOT_INIT_FAILED`] record (with the stable cause string
/// from [`BootError::as_str`]) and parks the CPU forever via
/// [`tairix_arch_x86_64::kernel_arch::halt`] (fail
/// closed, no silent reset).
///
/// Installs the production PID 1 spawn seam; see [`boot_with_init`] to supply
/// a different one, and for the `boot_info` SAFETY-INVARIANT.
pub fn boot(
    boot_info: u64,
    heap: &'static tairix_kalloc::FreeListAllocator,
    log_sink: &'static (dyn Sink + Sync),
    audit_sink: &'static (dyn Sink + Sync),
    log_level: Level,
) -> ! {
    boot_with_init(
        boot_info,
        heap,
        log_sink,
        audit_sink,
        log_level,
        &X86_64_INIT_SPAWN,
    )
}

/// Boot as [`boot`], but with a caller-chosen PID 1 spawn seam `init`.
///
/// [`boot`] passes the production [`X86_64_INIT_SPAWN`]; a QEMU vertical that
/// must admit its own in-kernel service before the dispatch loop (the DMA-fault
/// driver) passes an [`X86_64InitSpawn`](crate::x86_64::init_spawn::X86_64InitSpawn)
/// built with its own pre-dispatch. Every other caller uses [`boot`] and is
/// unaffected by this seam.
///
/// # SAFETY-INVARIANT
///
/// `boot_info` must be the verbatim 64-bit pointer the arch crate's boot
/// trampoline received in `%ebx`, in the identity-mapped 0..4 GiB window
/// (`boot.s` SAFETY-INVARIANT 7), exactly as [`boot`] requires.
pub fn boot_with_init(
    boot_info: u64,
    heap: &'static tairix_kalloc::FreeListAllocator,
    log_sink: &'static (dyn Sink + Sync),
    audit_sink: &'static (dyn Sink + Sync),
    log_level: Level,
    init: &'static dyn InitSpawn,
) -> ! {
    // Make every kernel heap's lock interrupt-safe before anything can be
    // interrupted while holding one: install this port's per-CPU `RFLAGS.IF`
    // mask/restore (`cli`/`sti`). Done at boot entry — before any interrupt
    // is enabled and before any AP is started — so an interrupt can never
    // fire on a CPU mid-allocation and reenter the allocator, spinning
    // forever on the lock its own interrupted mainline holds (a single-CPU
    // self-deadlock). One install covers every core and every heap the binary
    // holds: the hooks mask the *current* CPU's interrupts and are read by
    // the allocator itself, so a heap the boot handover never names is
    // covered too.
    tairix_kalloc::install_irq_control(
        crate::x86_64::com1_rx::kalloc_irq_disable,
        crate::x86_64::com1_rx::kalloc_irq_restore,
    );
    // A masked CPU cannot take the shootdown IPI, so it acknowledges from its
    // spin instead: this is the one port whose cross-CPU TLB invalidation
    // needs a software acknowledge, and the lock above is what masks the
    // kernel heap's teardown. Installed here, with the mask itself and before
    // any interrupt is enabled or any AP started, so no shootdown can be in
    // flight while a spinning CPU still has no way to answer it.
    tairix_sync::spinwait::install_service(tairix_arch_x86_64::tlb_shootdown::serve_pending);
    match try_boot(boot_info, heap, log_sink, audit_sink, log_level, init) {
        Ok(boot_info) => kernel_main(boot_info),
        Err(err) => {
            log_init_failure(log_sink, err);
            arch_halt()
        }
    }
}

fn log_tsc_invariance(sink: &(dyn Sink + Sync), invariant: bool) {
    // Record the decision on every boot so the TSC contract is audited,
    // not silently trusted. A part that advertises
    // the invariant flag logs at Info; one that does not logs at Warn,
    // because a later SMP bring-up on it is refused (`try_boot`).
    let (level, message) = if invariant {
        (Level::Info, "tsc invariance validated")
    } else {
        (
            Level::Warn,
            "tsc not invariant; single-cpu boot proceeds, smp gated",
        )
    };
    tairix_log::log(
        sink,
        &Event {
            level,
            id: KERNEL_BOOT_TSC_INVARIANCE,
            message,
            fields: &[Field {
                key: "invariant_tsc",
                value: tairix_log::FieldValue::Str(if invariant { "true" } else { "false" }),
            }],
        },
    );
}

fn log_init_failure(sink: &(dyn Sink + Sync), err: BootError) {
    tairix_log::log(
        sink,
        &Event {
            level: Level::Error,
            id: KERNEL_BOOT_INIT_FAILED,
            message: "kernel boot init failed",
            fields: &[Field {
                key: "cause",
                value: tairix_log::FieldValue::Str(err.as_str()),
            }],
        },
    );
}

/// Board facts the shared BSP bring-up ([`bring_up_bsp`]) discovered and
/// hands back to its caller.
///
/// The bring-up owns every set-once install (per-CPU GDT/IDT/IST arena,
/// syscall TLS arena, dispatch callback, user-fault resolver, LAPIC timer,
/// IO-APIC routing); the caller owns what varies per composition — the
/// arch handle(s) it builds from these facts and whatever it wires into the
/// [`DISPATCH_SLOT`] the installed callbacks resolve through. The production
/// pipeline assembles a [`BootInfo`] and enters `kernel_main` (which
/// publishes the production hook into the slot); a QEMU test chassis
/// composes the same facts with its own scheduler and installs its own
/// production-typed hook into the same slot instead — without forking any
/// of this bring-up.
pub struct BspBringUp {
    /// The BSP's LAPIC id, verified present and enabled in the MADT.
    pub bsp_lapic_id: u32,
    /// Dense-CpuId→LAPIC map with only the BSP populated — the one
    /// definition both the production arch handle and a chassis's handles
    /// are built from (single-CPU bring-up; an AP bring-up re-sizes it).
    pub cpu_to_lapic: [Option<u32>; 1],
    /// LAPIC-timer/TSC calibration measured against the PIT; the unit input
    /// to [`BinArch`]'s `monotonic_ns`.
    pub calibration: Calibration,
    /// The kernel command line the loader passed; empty where it passed none
    /// that could be read.
    pub command_line: &'static str,
    /// The firmware memory map with the running kernel image reserved and
    /// the kthread-stack guard arena carved out.
    pub memory_map: BootMemoryMap,
    /// Installed RAM in bytes: the firmware map's usable-RAM total taken
    /// before the kernel-image and guard-arena carves — the figure the
    /// ungated `boot_facts_get` syscall reports.
    pub installed_memory_bytes: u64,
    /// The MADT-discovered IO-APIC routing, every pin programmed masked.
    pub irq_routing: IrqRouting,
    /// The discovered hardware tree: the ACPI platform inventory plus the
    /// enumerated virtio-PCI block/network nodes. The production [`try_boot`]
    /// hands it to the boot record, which moves it into
    /// [`crate::hwtree_store::HW_TREE`]; a QEMU chassis that composes its own
    /// boot ignores it.
    pub tree: Vec<tairix_abi::HwNode>,
    /// The translation units firmware described in a malformed table, where
    /// it did, and whether the administrator chose to publish the functions
    /// they would have confined untranslated.
    pub malformed_units: Option<tairix_kernel_core::iommu::MalformedUnits>,
}

/// Bring the BSP and its board up: per-CPU tables, the dedicated `#PF`
/// entry + fault-windowed user copy, NXE, the park root, LAPIC + timer
/// calibration, the firmware memory map (kernel image reserved, guard
/// arena carved), the MADT walk, the production syscall-dispatch callback
/// and user-fault resolver (both resolving through [`DISPATCH_SLOT`]),
/// `syscall`/TSS entry, and the IO-APIC routing (all pins masked).
///
/// This is the single, composable board bring-up every freestanding x86_64
/// kernel binary runs — the production [`boot`] pipeline and the QEMU
/// integration chassis alike — so the bring-up ordering and its set-once
/// installs are never forked. Runs exactly once per boot; every set-once
/// install fails closed with a typed [`BootError`] on a second attempt.
///
/// # SAFETY-INVARIANT
///
/// `boot_info` must be the verbatim 64-bit pointer the arch crate's boot
/// trampoline received (the [`boot`] contract): the record and every table
/// it points at sit in the identity-mapped 0..4 GiB window, and interrupts
/// are disabled (`IF=0`) for the whole call.
pub fn bring_up_bsp(
    boot_info: u64,
    log_sink: &'static (dyn Sink + Sync),
) -> Result<BspBringUp, BootError> {
    // 1. Per-CPU init (BSP).
    //
    //    Publish the caller-owned per-CPU GDT/IDT/IST arena before the
    //    first `percpu::init`, so the arch crate indexes a runtime-sized
    //    slice rather than a baked-in `MAX_CPUS` arena. `register` is set-once and `boot` runs once, so a second
    //    publish is a boot-path defect that fails closed.
    PER_CPU_STORAGE
        .register()
        .map_err(|_| BootError::PercpuStorageRegister)?;

    // SAFETY: This is the BSP, called exactly once. The boot
    // trampoline (`boot.s`) leaves `IF=0` so interrupts remain
    // disabled, satisfying `percpu::init`'s SAFETY contract.
    unsafe { percpu::init(0).map_err(|_| BootError::PercpuInit)? };

    // 1b. Arm the guarded user copy the `#PF` entry redirects into and
    //     publish the one fatal policy before anything further can fault.
    install_fault_entries();

    // 1c. Enable `IA32_EFER.NXE` so the W^X No-Execute leaf bit the
    //     process-image builder sets on a ring-3 program's data/rodata
    //     pages and its stack is honoured. Without it,
    //     bit 63 is reserved and the first non-executable user mapping the
    //     `init` spawn seam builds would fault the page-table walk. Enabling
    //     it on the BSP before any user image is built is the production W^X
    //     contract; it preserves `SCE`/`LME`/`LMA` the boot trampoline set.
    //
    // SAFETY: BSP after `percpu::init(0)`; interrupts disabled. The
    // read-modify-write only sets bit 11, leaving every other `IA32_EFER`
    // bit (long-mode enable/active, syscall enable) intact.
    unsafe {
        enable_nxe();
    }

    // 1d. Publish the trampoline's `CR3` tables as the park root a CPU
    //     re-installs whenever it leaves a user space's root (task
    //     suspend, address-space teardown). x86_64 keeps running on the
    //     `boot.s` tables rather than switching to a Rust-built kernel
    //     space, so — unlike aarch64/riscv64, where the boot `switch()`
    //     publishes — the boot path records the active root explicitly,
    //     before any process space exists to claim the set-once slot.
    //     (The paging module exists only on the bare-metal target.)
    #[cfg(all(freestanding, kernel_isa = "x86_64"))]
    tairix_arch_x86_64::paging::publish_boot_park_root();

    // 1e. Boot-info parsing and the direct physical map — before anything
    //     reaches a register block or a frame by pointer. The trampoline
    //     identity-maps only what must be addressed physically, so the LAPIC
    //     register writes the steps below make, and every frame the kernel
    //     later touches, resolve through this map or not at all. The tables
    //     it needs are carved out of the firmware map first, so the frame
    //     allocator never hands them out.
    //
    // SAFETY: `boot_info` is the verbatim trampoline pointer (the
    // documented invariant of [`boot`]); the blob and every table it
    // points at sit in the identity-mapped 0..4 GiB window (`boot.s`
    // SAFETY-INVARIANT 4).
    let boot_data = unsafe { BootData::load(boot_info) }.map_err(|_| BootError::BootInfoParse)?;
    // SAFETY: same contract — the loader's command line lies in the window
    // its record does.
    let command_line = kept(unsafe { boot_data.command_line() }.unwrap_or(""))?;
    let (mut memory_map, installed_memory_bytes) = build_memory_map(&boot_data)?;
    let direct_map_gib = crate::mem_map::direct_map_gib(
        &memory_map,
        paging::BOOT_IDENTITY_GIB,
        paging::MAX_PHYSMAP_GIB,
    );
    install_direct_physical_map(&mut memory_map, direct_map_gib)?;
    crate::mem_map::log_direct_map(
        log_sink,
        paging::physmap_gigapages(),
        paging::gigapages_supported(),
    );

    // 2. Software-enable the BSP LAPIC and read its ID. An APIC firmware
    //    left in x2APIC mode stays in it: leaving it means disabling the
    //    APIC, and its MMIO window answers nothing.
    if tairix_arch_x86_64::apic::in_x2apic() {
        // SAFETY: ring 0, interrupts masked, before anything reaches the
        // APIC and before any other CPU starts; the CPU is in x2APIC mode,
        // so it has it.
        unsafe { tairix_arch_x86_64::apic::enter_x2apic() };
    }
    let mut lapic = Lapic::new(LocalApic);
    lapic.software_enable(0xFF);
    let bsp_lapic_id = lapic.id();
    if !tairix_arch_x86_64::apic::names_cpu(bsp_lapic_id, tairix_arch_x86_64::apic::x2apic()) {
        return Err(BootError::BspApicIdUnsupported);
    }

    // 3. Calibrate the LAPIC timer against the PIT. The same window
    //    samples RDTSC so the resulting `Calibration::tsc_per_second`
    //    is the unit input to `BinArch::monotonic_ns` (the production
    //    `clock_get` syscall path, Stage 2.7 follow-up (f3)).
    let mut pit = PolledPit;
    let mut tsc = Rdtsc;
    let calibration = apic_timer::calibrate(
        &mut lapic,
        &mut pit,
        &mut tsc,
        PREEMPT_CALIBRATION_WINDOW_US,
        PREEMPT_PERIOD_US,
    )
    .map_err(|_| BootError::TimerCalibration)?;

    // 4. The RSDP, from the boot info parsed in step 1e.
    //
    // SAFETY: same identity-window contract as the `BootData::load`
    // above — the RSDP the loader published sits below 4 GiB.
    let rsdp = unsafe { boot_data.validated_rsdp() }.ok_or(BootError::NoRsdp)?;

    // 5. MADT walk → BSP LAPIC verification.
    //
    // SAFETY: `rsdp` was validated above; its XSDT/RSDT pointers came
    // from firmware-published tables in the identity-mapped 0..4 GiB
    // window (`boot.s` SAFETY-INVARIANT 4).
    let madt_bytes = unsafe { acpi::locate_madt(&rsdp) }.ok_or(BootError::NoMadt)?;
    let madt = acpi::Madt::parse(madt_bytes).map_err(|_| BootError::BadMadt)?;
    verify_bsp_present(&madt, bsp_lapic_id)?;

    // 6. Build the `cpu_to_lapic` map with **only** the BSP populated.
    //    Production `tairix-kernel` runs single-CPU (it never calls the
    //    `SecondaryBringup` HAL method — that handshake is proven by the
    //    QEMU verticals), so the arch handle's per-CPU bookkeeping is
    //    sized to one slot (capacity matches the
    //    machine the caller actually drives, no global `MAX_CPUS`
    //    ceiling baked into the arch crate). The per-CPU kernel-stack
    //    pool keeps its own `MAX_CPUS` secondary-bring-up bound.
    let cpu_to_lapic: [Option<u32>; 1] = [Some(bsp_lapic_id)];

    // 6a. Validate the TSC before trusting `RDTSC` as the cross-CPU
    //     monotonic clock source. The contract is recorded on every
    //     boot rather than silently assumed. A
    //     single-CPU boot proceeds regardless — one TSC is inherently
    //     self-monotonic — but the day this pipeline brings up a
    //     second CPU on a part without an Invariant TSC, it fails
    //     closed instead of risking a non-monotonic `clock_get`. The CPUID probe lives in the arch crate.
    let invariant_tsc = tairix_arch_x86_64::tsc::detect_invariant_tsc();
    log_tsc_invariance(log_sink, invariant_tsc);
    let active_cpu_count = cpu_to_lapic.iter().filter(|slot| slot.is_some()).count();
    if !invariant_tsc && active_cpu_count > 1 {
        return Err(BootError::TscNotInvariant);
    }

    // 7. Install the production syscall-dispatch callback **before**
    //    `init_local_syscalls` enables `syscall` on any CPU. The
    //    ordering matters per `syscall_entry` rustdoc — the trampoline
    //    fail-closes if it fires with no callback installed.
    //
    //    Stage 2.7 follow-up (f5). `production_dispatch` reads the
    //    `DISPATCH_SLOT` static (whose hook `kernel_main` publishes
    //    during the `Syscall` init phase between Sched and Ipc) and
    //    forwards every syscall through the resident `DispatchHook`.
    //    If a syscall fires before the slot is published, or if the
    //    hook signals `NoCallerContext`, the callback halts the CPU
    //    forever — the same fail-closed posture the (c7-bin) commit
    //    shipped, now coexisting with the live dispatcher.
    syscall_entry::set_dispatch_callback(production_dispatch);

    // Arm the task-latency watchdog beside the dispatcher it observes: an
    // interactive surface that declares a frame budget then gets an overrun
    // report naming the blocking call and the user code that stalled. The
    // layout is this port's own frame-pointer convention, the same constant
    // its fault-path backtrace uses. Debug image only; a shippable image
    // installs no observer and the entry hook costs one relaxed load.
    #[cfg(feature = "watchdog-diagnostics")]
    tairix_kernel_core::latency::install(tairix_arch_x86_64::backtrace::Backtracer::LAYOUT);
    // Demand-paged file mappings and stack growth resolve their ring-3
    // `#PF`s through the same resident hook; install the resolver beside
    // the dispatch callback so both are in place before user space exists.
    // This single-entry bring-up installs exactly once; a second occupant
    // is a boot-path defect and refuses the boot with a typed, logged
    // cause rather than running with an unpredictable fault path.
    if fault::set_user_fault_resolver(production_user_fault).is_err() {
        return Err(BootError::UserFaultResolverInstall);
    }
    // Beside the resolver, install the terminator the exception entries use
    // for a ring-3 exception they cannot resolve (a wild jump's
    // instruction-fetch `#PF`, an invalid opcode, a general-protection
    // violation): it kills the offending task and keeps the CPU alive, so
    // one task's bad instruction can never park a core. Installed once,
    // before user space; a second occupant refuses the boot rather than
    // running with an unpredictable fault path.
    if fault::set_user_fault_terminator(production_user_fault_terminate).is_err() {
        return Err(BootError::UserFaultTerminatorInstall);
    }

    // 7b. Publish the caller-owned per-CPU syscall-TLS arena before
    //     `init_local_syscalls` (which writes this CPU's slot and points
    //     `IA32_KERNEL_GS_BASE` at it). Runtime-sized, set-once, fails
    //     closed on a second publish.
    SYSCALL_TLS_STORAGE
        .register()
        .map_err(|_| BootError::SyscallTlsStorageRegister)?;

    // 8. Install the LAPIC timer ISR + program the period. No timer
    //    callback is registered: the timer ISR's null-callback branch
    //    issues the EOI and returns, which is exactly what we want
    //    until the scheduler dispatch loop lands in Stage 2.7.
    //
    // SAFETY: this is the BSP whose `percpu::init(0)` ran above,
    // interrupts are disabled, and `lapic` is the BSP's LAPIC because
    // it was constructed from the architectural LAPIC base.
    unsafe {
        preempt::init_local_preempt(0, &mut lapic, calibration)
            .map_err(|_| BootError::PreemptInit)?;
    }

    // 10. Enable `syscall`/`sysret` on the BSP, with both ring-3 entry
    //     stacks — `syscall`'s and `TSS.RSP0`, which a ring-3 exception or
    //     interrupt loads — on the per-CPU kernel stack. The callback is
    //     already installed (step 7).
    let sel = PerCpuGdt::selectors();
    // `STAR[63:48]` is the "sysret user base"; on `sysretq` long mode
    // the CPU loads `CS = base + 16`, `SS = base + 8`. See
    // `syscall_entry::encode_star` rustdoc.
    let sysret_user_base = sel.user_cs - 16;
    // SAFETY: BSP after `percpu::init(0)`; interrupts disabled; the
    // kernel stack top is one byte past a 16-byte-aligned backing region
    // mapped in every address space; the dispatch callback was installed
    // above.
    unsafe {
        syscall_entry::init_local_syscalls(0, sel.kernel_cs, sysret_user_base, kernel_stack_top(0))
            .map_err(|_| BootError::SyscallInit)?;
    }

    // 10b. Stage 4.D Item 2-tail.2: discover every IO-APIC the MADT
    //      advertises, allocate one external-IRQ vector per pin,
    //      install the per-pin IDT entry, populate the arch crate's
    //      lock-free routing table, and program the redirection entry
    //      `masked = true`. The driver-host side (Item 2-tail.3,
    //      out of scope here) will later unmask each line through the
    //      controller's `program_pin` re-publish path when a driver
    //      binds to the GSI.
    let irq_routing = discover_and_program_io_apics(&madt, bsp_lapic_id, log_sink)?;

    // Discover the ACPI platform inventory (root, enabled CPUs, and the I/O
    // APICs) plus the enumerated virtio-PCI devices, which `try_boot`'s boot
    // record publishes to the authoritative `HW_TREE`, so the `hw_tree_read`
    // / `hw_tree_wait` syscalls expose the real x86_64 hardware to user space
    // — the sibling of the riscv64/aarch64 device-tree seed.
    //
    // Ordered **after** `discover_and_program_io_apics` deliberately: an
    // interrupt-driven virtio-PCI function (a NIC, a keyboard) is a
    // message-signalled-interrupt device, and the probe routes each one's
    // MSI-X into a kernel-allocated vector (`crate::x86_64::msi::allocate` +
    // the function's MSI-X table), which requires the vectors
    // `discover_and_program_io_apics` installs (`vectors::install`). The
    // enumerator programs the device and grants the driver the routed MSI
    // line, exactly as the `MsiAllocation` contract describes a bus driver
    // wiring a function — so a user-space driver only `irq_bind`s the line
    // and never touches PCI config or the MSI-X BAR (the kernel owns
    // interrupt routing).
    //
    // SAFETY: `madt_bytes` and `rsdp` were validated above; their tables
    // (and the MCFG the ECAM branch reads) sit in the identity-mapped
    // 0..4 GiB window (`boot.s` SAFETY-INVARIANT 4), and the ECAM window is
    // re-validated against that window before it is mapped.
    let (tree, malformed_units) =
        unsafe { seed_hardware_tree(madt_bytes, &rsdp, &boot_data, command_line, log_sink) };

    Ok(BspBringUp {
        bsp_lapic_id,
        cpu_to_lapic,
        calibration,
        command_line,
        memory_map,
        installed_memory_bytes,
        irq_routing,
        tree,
        malformed_units,
    })
}

/// Arm the guarded user copy and publish the machine's one fatal policy.
///
/// `percpu::init` already routed every exception to an entry that reports
/// it and `#PF` to the resumable one. The fault-windowed user copy is armed
/// beside that entry, because its kernel-fault window check is what redirects
/// an in-window fault to the copy's fix-up. The fatal handler slot is filled
/// last; it is set-once and never overrides an occupant, so a QEMU vertical
/// that observes its own deliberate faults publishes ahead of `boot` and keeps
/// it.
fn install_fault_entries() {
    tairix_arch_x86_64::uaccess::install();
    let _ = crate::x86_64::panic_ctx::install_kernel_fault_handler();
}

fn try_boot(
    boot_info: u64,
    heap: &'static tairix_kalloc::FreeListAllocator,
    log_sink: &'static (dyn Sink + Sync),
    audit_sink: &'static (dyn Sink + Sync),
    log_level: Level,
    init: &'static dyn InitSpawn,
) -> Result<BootInfo<'static, BinArch>, BootError> {
    // The arch handle borrows its per-CPU bookkeeping from this
    // process-static backing; `boot` runs once, so a
    // single `static` is sound and needs no allocator.
    static ARCH_STORAGE: X86_64ArchStorage<1> = X86_64ArchStorage::new();

    // The shared BSP/board bring-up: per-CPU tables, `#PF` + user-copy
    // entries, NXE, park root, LAPIC calibration, memory map + guard
    // arena, MADT, dispatch callback + user-fault resolver, `syscall`/TSS
    // entry, IO-APIC routing.
    let board = bring_up_bsp(boot_info, log_sink)?;

    // No device tree to stash: the root bring-up re-resolves its transport
    // from PCI configuration space.
    crate::unlock_service::record_boot(0, board.tree, log_sink);

    let arch = X86_64Arch::new(&ARCH_STORAGE, 0, board.bsp_lapic_id, &board.cpu_to_lapic)
        .map_err(|_| BootError::ArchInit)?;
    let BspBringUp {
        calibration,
        command_line,
        memory_map,
        installed_memory_bytes,
        irq_routing,
        malformed_units,
        ..
    } = board;

    // Assemble the `BootInfo` and hand off to `kernel_core`.
    //
    // Build the `Arc<BinArch>` ahead of the `BootInfo::new` call so we
    // can publish the pointer into `panic_ctx::PANIC_ARCH_PTR` for the
    // panic-handler bridge. The `Arc` is kept alive by `BootInfo`'s
    // `arch` field (and re-cloned into `kernel_core`'s `KernelState`),
    // so the published pointer remains valid for the lifetime of the
    // running kernel.
    let arch_arc: Arc<BinArch> = Arc::new(BinArch::new(arch, calibration, irq_routing));
    // SAFETY: `arch_arc` is moved into `BootInfo` immediately below
    // (which `kernel_main` consumes and stores). `Arc::as_ptr` returns
    // a stable pointer for the lifetime of any clone of the `Arc`.
    unsafe {
        crate::x86_64::panic_ctx::publish_arch(Arc::as_ptr(&arch_arc));
    }
    // Publish a clone of the firmware memory map into the bin-crate's
    // set-once slot before it is moved into the `kernel_core` hand-off,
    // so a driver-bring-up observer can build a per-device DMA
    // `FrameAllocator` from the same firmware description without
    // re-borrowing the `pub(crate)` `KernelState`.
    crate::x86_64::arch_wrapper::publish_memory_map(&memory_map);

    let scheduler_config = SchedulerConfig::defaults_for(1);
    let boot_info: BootInfo<'static, BinArch> = BootInfo::new(
        /* boot_cpu       = */ 0,
        /* cpu_count      = */ 1,
        command_line,
        memory_map,
        scheduler_config,
        arch_arc,
        log_sink,
        audit_sink,
        log_level,
        // Stage 2.7 follow-up (f4): hand the bin-crate-owned slot to
        // `kernel_main`'s `Syscall` phase. The arch-level
        // `set_dispatch_callback` (step 7 above) is unchanged; this
        // is the *kernel-side* publication point for the eventual
        // production dispatch hook.
        &DISPATCH_SLOT,
        heap,
    )
    // The COM1 console list for the standard streams: `stream_write` on
    // fd 1/2/3 reaches the same serial line the log sink uses, so PID 1
    // `init`'s banner lands (`plans/PI.md` X3a). It is a
    // stream *backing*, not a program-facing device; the read half fails
    // closed (no COM1 RX drain is wired on this slice).
    .with_consoles(&COM1_CONSOLES)
    // Record the firmware-reported installed-RAM total so the core mints
    // the `boot_facts_get` machine summary from it.
    .with_installed_memory(installed_memory_bytes)
    // Hand the shared identity cell to the core: the sec phase
    // publishes the compiled-in system identity into it, so the
    // system/service accounts resolve (spawn-as-user, filesystem
    // groups) from first boot; a later encrypted-root unlock replaces
    // the held table with the merged system∪human table.
    .with_spawn_identity(&crate::root_mount::LATE_IDENTITY)
    // The PID 1 (`init`) spawn seam: after `BootCompleted`, `kernel_main`
    // builds `init`'s ring-3 image and drops into it as a resumable user
    // kthread (`plans/PI.md` X3a). `boot` passes the production seam; a QEMU
    // vertical may pass its own through `boot_with_init`.
    .with_init(init)
    // The runtime `spawn` producer + embedded-program registry
    // (`plans/PI.md` X3b): the `spawn` syscall resolves a path against the
    // registry and drives the producer to build a fresh, isolated child PML4,
    // so PID 1 `init` can launch the user's session concurrently — the
    // cross-port sibling of the aarch64 `boot_aarch64` wiring.
    .with_spawn(
        &crate::spawn_layout::PROGRAM_REGISTRY,
        &crate::x86_64::spawn_producer::X86_64_PROCESS_SPAWN,
    )
    // Serve the discovered hardware tree: the
    // `hw_tree_read` / `hw_tree_wait` syscalls read the one authoritative
    // `HW_TREE`, so the user-space device manager observes the same
    // inventory the kernel discovered (Design D).
    .with_hw_tree(&crate::hwtree_store::HW_TREE_SOURCE)
    // Install the on-disk application store (`plans/APPS.md` deliverable 8):
    // this port embeds no program rows, so every command app and service is
    // spawned from its verified `/System` store bundle. The storage bring-up
    // resolves the store's readiness latch on every outcome (mount installed
    // or given up), so a spawn racing the mount parks and always wakes.
    .with_app_store(&crate::app_store::APP_STORE)
    // Hand the syscall dispatch hook the shared set-once credential cell: the
    // in-kernel root-unlock kthread publishes the mounted root volume's
    // database into it once the operator's passphrase unlocks the encrypted
    // root. Until that install the cell fails every `users_db_read` closed, so
    // login refuses every attempt until a root is mounted.
    .with_users_db(&crate::root_mount::LATE_USERS_DB)
    .with_groups_db(&crate::root_mount::LATE_GROUPS_DB)
    .with_users_admin(&crate::root_mount::LATE_USERS_ADMIN)
    // Serve the `fs_*` syscalls through the production filesystem service: it
    // routes each operation through the secured VFS against the late-installed
    // read-only `/System` mount. The cell fails closed until the disk-owning
    // task publishes the `/System` window
    // (`system_mount::install_system_mount`), so wiring the hook here changes
    // no boot behaviour until that install lands.
    .with_filesystem(&crate::system_mount::FS_SERVICE)
    // Resolve `id::<volume-id>/…` paths against the volume forest the
    // mount/unlock tasks publish each mounted volume's stable identity into
    // (`plans/DEVICES.md` D3a). Fails closed `NotFound` until a volume is
    // published, so wiring it here changes no boot behaviour.
    .with_volumes(&crate::system_mount::VOLUME_FOREST)
    // Delegate runtime volume attach/detach (`plans/DEVICES.md` D3b) to the
    // production service; it fails closed `NotImplemented` until the mount
    // task wires its audit sink and pressure gauge.
    .with_volume_service(&crate::volume_service::VOLUME_SERVICE);
    let boot_info = match malformed_units {
        Some(units) => boot_info.with_malformed_units(units),
        None => boot_info,
    };
    boot_info
        .validate()
        .map_err(|_| BootError::BootInfoInvalid)?;

    // The caller forwards to `kernel_main`, which returns `!` and
    // never re-enters this function.
    Ok(boot_info)
}

/// Discover the platform hardware tree from the firmware ACPI tables and
/// the PCI bus: the boot seed [`crate::unlock_service::record_boot`]
/// publishes to the authoritative [`crate::hwtree_store::HW_TREE`] the
/// `hw_tree_read` / `hw_tree_wait` syscalls read, so user space observes the
/// same inventory the kernel discovered (Design D) — the x86_64 sibling of
/// the riscv64/aarch64 device-tree seed.
///
/// Two discovery sources feed **one** shared
/// [`crate::boot_hwtree::CollectingHwNodeSink`] (so no arch carries its own
/// collect-into-`Vec` logic) before the buffered tree is published:
///
/// 1. ACPI normalisation through the port's
///    [`tairix_arch_x86_64::platform::AcpiDiscovery`] — the root, every
///    enabled Local APIC as a CPU node, and the I/O APICs as
///    interrupt-controller nodes — from the already-validated `madt_bytes`,
///    plus the legacy CMOS clock as an `Rtc` node carrying its `0x70`/`0x71`
///    port pair, which no ACPI table enumerates.
/// 2. The virtio-PCI probe: enumerate configuration space and emit every
///    virtio-net function as a role-tagged network node
///    ([`crate::hwdiscovery::observe_virtio_pci_network_devices`]) and every
///    virtio-input function as a role-tagged input node
///    ([`crate::hwdiscovery::observe_virtio_pci_input_devices`]) the
///    two-process user-space drivers autoload against, plus every
///    virtio-blk function as a match-key-only storage node
///    ([`crate::hwdiscovery::observe_virtio_pci_block_devices`]) the
///    root-mount autoload binds the bootstrap-floor block driver from. The
///    interrupt line each network and input node carries is the function's
///    firmware-assigned PCI interrupt line (a *discovered* value, read from
///    configuration space, never a board constant); the block node carries
///    only its bind key, as its in-kernel bring-up re-resolves the
///    transport from configuration space itself.
///
/// Fail closed at every step: a malformed ACPI table, an ECAM window
/// outside the identity map, or an enumeration error each leave the
/// affected devices undiscovered and seed whatever *was* collected rather
/// than failing the boot. The collected tree is returned by value for the
/// boot record to move into the live inventory, beside the units of a
/// malformed DMAR, IVRS or VIOT: their PCI functions are withheld unless
/// `command_line` sets `iommu.malformed=unconfined`.
///
/// # Safety
///
/// `madt_bytes` and `rsdp` must lie in the boot trampoline's 0..4 GiB
/// identity-mapped window and stay unmodified for the kernel's lifetime
/// (the ACPI guarantee); the ECAM window `rsdp`'s MCFG references is
/// re-validated against that window before it is mapped.
unsafe fn seed_hardware_tree(
    madt_bytes: &[u8],
    rsdp: &acpi::Rsdp,
    firmware: &BootData<'_>,
    command_line: &str,
    log: &'static (dyn Sink + Sync),
) -> (
    Vec<tairix_abi::HwNode>,
    Option<tairix_kernel_core::iommu::MalformedUnits>,
) {
    use tairix_arch_api::PlatformDiscovery;
    use tairix_arch_x86_64::platform::AcpiDiscovery;

    let mut sink = crate::boot_hwtree::CollectingHwNodeSink::new();
    // A discovery error leaves the sink empty; seed whatever was collected.
    let _ = AcpiDiscovery::new(madt_bytes).discover(&mut sink);
    // SAFETY: forwarded — the caller pins the firmware tables into the
    // identity-mapped window.
    let table = unsafe { unit_table(rsdp, log) };
    let malformed = match table {
        UnitTable::Malformed(family) => Some(tairix_kernel_core::iommu::MalformedUnits {
            family,
            unconfined: tairix_kernel_core::bootinfo::command_line_value(
                command_line,
                MALFORMED_UNITS,
            ) == Some(DmaUnitState::Unconfined.name()),
        }),
        _ => None,
    };
    // SAFETY: forwarded — the caller pins the firmware tables (and the MCFG
    // they reference) into the identity-mapped window.
    let platform = unsafe { platform_registers(madt_bytes, rsdp, &table, sink.nodes()) };
    match platform.and_then(|platform| bar_apertures(firmware, platform)) {
        // SAFETY: forwarded — the caller pins the firmware tables (and the
        // MCFG the ECAM branch reads) into the identity-mapped window.
        Some(apertures) => unsafe { seed_pci(rsdp, &table, malformed, &apertures, &mut sink, log) },
        None => crate::pci_probe::log_discovery(
            log,
            Level::Error,
            "pci apertures unrecorded; none probed",
        ),
    }
    (sink.into_vec(), malformed)
}

/// Where a firmware-assigned BAR may decode: anywhere the physical address
/// space reaches but memory of any kind firmware describes — RAM, and RAM
/// firmware keeps for itself — the kernel image, and the `platform`'s own
/// registers, so no device aims the kernel's writes or a driver's mapping at
/// memory or at the devices the kernel itself drives.
fn bar_apertures(
    data: &BootData<'_>,
    mut forbidden: Vec<core::ops::Range<u64>>,
) -> Option<tairix_pci::Apertures> {
    let mut held = true;
    firmware_regions(data, &mut |region| {
        if let Some(memory) = region.memory() {
            held &= forbidden.try_reserve(1).is_ok();
            if held {
                forbidden.push(memory);
            }
        }
    })
    .ok()?;
    if !held {
        return None;
    }
    let (kernel_start, kernel_end) = kernel_image_phys_bounds();
    forbidden.try_reserve(1).ok()?;
    forbidden.push(kernel_start.as_u64()..kernel_end.as_u64());
    let mut windows = Vec::new();
    windows.try_reserve_exact(1).ok()?;
    windows.push(tairix_pci::Aperture::identity(
        0..paging::PHYSICAL_ADDRESS_LIMIT,
    ));
    Some(tairix_pci::Apertures::new(windows, forbidden))
}

/// The registers of the devices the platform itself is made of: every local
/// APIC and the message window, each device `platform` already holds (the
/// IO-APICs), each translation unit, and each ECAM region. [`None`] when they
/// cannot be held.
///
/// # Safety
///
/// `rsdp` and the MCFG it references must lie in the identity-mapped window.
unsafe fn platform_registers(
    madt_bytes: &[u8],
    rsdp: &acpi::Rsdp,
    table: &UnitTable<'_>,
    platform: &[tairix_abi::HwNode],
) -> Option<Vec<core::ops::Range<u64>>> {
    let mut windows = Vec::new();
    let mut held = true;
    let mut keep = |window: core::ops::Range<u64>| {
        held &= windows.try_reserve(1).is_ok();
        if held {
            windows.push(window);
        }
    };
    keep(tairix_kernel_iommu_api::MESSAGE_WINDOW);
    let page = |base: u64| base..base.saturating_add(tairix_kernel_mem::PAGE_SIZE as u64);
    if let Ok(madt) = acpi::Madt::parse(madt_bytes) {
        keep(page(u64::from(madt.lapic_address)));
        for entry in madt.entries() {
            if let MadtEntry::LocalApicAddressOverride { address } = entry {
                keep(page(address));
            }
        }
    }
    platform
        .iter()
        .flat_map(tairix_kernel_core::iommu::register_windows)
        .for_each(&mut keep);
    match table {
        UnitTable::Dmar(dmar) => dmar
            .units()
            .map(|unit| {
                unit.register_base()..unit.register_base().saturating_add(unit.register_len())
            })
            .for_each(&mut keep),
        UnitTable::Ivrs(ivrs) => ivrs
            .units()
            .map(|unit| {
                let base = unit.register_base();
                base..base.saturating_add(tairix_arch_x86_64::ivrs::UNIT_REGISTER_LEN)
            })
            .for_each(&mut keep),
        // A unit that is a PCI function is reached through its own BARs.
        UnitTable::Viot(viot) => viot
            .units()
            .filter_map(|unit| match unit {
                tairix_arch_x86_64::viot::ViotUnit::Mmio { base } => {
                    Some(base..base.saturating_add(tairix_kernel_iommu_virtio::MMIO_WINDOW))
                }
                tairix_arch_x86_64::viot::ViotUnit::Pci { .. } => None,
            })
            .for_each(&mut keep),
        UnitTable::Malformed(_) | UnitTable::None => {}
    }
    // SAFETY: forwarded — the caller pins the MCFG into the identity window.
    if let Some(mcfg) =
        unsafe { acpi::locate_mcfg(rsdp) }.and_then(|bytes| acpi::Mcfg::parse(bytes).ok())
    {
        for allocation in mcfg.allocations() {
            let base = allocation.window_base();
            keep(base..base.saturating_add(allocation.window_len()));
        }
    }
    held.then_some(windows)
}

/// The translation units firmware describes, in the table it describes them
/// in: a platform describes its units in one, the hardware's own before a
/// hypervisor's paravirtual one.
enum UnitTable<'a> {
    Dmar(tairix_arch_x86_64::dmar::Dmar<'a>),
    Ivrs(tairix_arch_x86_64::ivrs::Ivrs<'a>),
    Viot(tairix_arch_x86_64::viot::Viot<'a>),
    /// A table of units of this family is present but malformed: there are
    /// units, and none can be read.
    Malformed(DmaUnitFamily),
    None,
}

/// The command-line key whose value `unconfined` publishes the functions a
/// malformed table's units would have confined untranslated, rather than
/// withholding them.
const MALFORMED_UNITS: &str = "iommu.malformed";

/// The table describing the platform's translation units, if one is present;
/// a malformed one is refused whole and logged.
///
/// # Safety
///
/// `rsdp` and every table it references must lie in the identity-mapped
/// 0..4 GiB window, unmodified for the kernel's lifetime.
unsafe fn unit_table(rsdp: &acpi::Rsdp, log: &dyn Sink) -> UnitTable<'static> {
    let parsed = |table: Result<UnitTable<'static>, acpi::AcpiError>, family, malformed| {
        table.unwrap_or_else(|_| {
            crate::pci_probe::log_discovery(log, Level::Error, malformed);
            UnitTable::Malformed(family)
        })
    };
    // SAFETY: forwarded.
    if let Some(bytes) = unsafe { acpi::locate_dmar(rsdp) } {
        return parsed(
            tairix_arch_x86_64::dmar::Dmar::parse(bytes).map(UnitTable::Dmar),
            DmaUnitFamily::Vtd,
            "dmar table malformed",
        );
    }
    // SAFETY: forwarded.
    if let Some(bytes) = unsafe { acpi::locate_ivrs(rsdp) } {
        return parsed(
            tairix_arch_x86_64::ivrs::Ivrs::parse(bytes).map(UnitTable::Ivrs),
            DmaUnitFamily::AmdVi,
            "ivrs table malformed",
        );
    }
    // SAFETY: forwarded.
    match unsafe { acpi::locate_viot(rsdp) } {
        Some(bytes) => parsed(
            tairix_arch_x86_64::viot::Viot::parse(bytes).map(UnitTable::Viot),
            DmaUnitFamily::VirtioPci,
            "viot table malformed",
        ),
        None => UnitTable::None,
    }
}

/// Probe every PCI segment the firmware describes and keep each as the
/// kernel's own: every virtio-blk function (match-key-only) and every
/// virtio-net, sound and input function (each with its resolved
/// configuration windows and interrupt line) is emitted into `sink`.
///
/// Configuration space is reached through ECAM (MMCONFIG) for every segment
/// the firmware's `MCFG` describes, falling back to PCI **mechanism #1**
/// (`0xCF8`/`0xCFC` port I/O), which reaches segment 0 only, where it
/// describes none. This is hardware-capability detection, not a
/// compatibility shim: a real UEFI/PCIe machine (and QEMU `q35`) exposes
/// ECAM, while the legacy `pc`/i440fx machine has none. Every segment feeds
/// the one shared probe ([`crate::pci_probe::probe`]), so there is one probe
/// definition and no path is dead.
///
/// # Safety
///
/// `rsdp` and the MCFG it references must lie in the boot trampoline's
/// 0..4 GiB identity-mapped window (each ECAM region is validated to lie
/// wholly within the direct map before it is mapped).
unsafe fn seed_pci(
    rsdp: &acpi::Rsdp,
    table: &UnitTable<'_>,
    malformed: Option<tairix_kernel_core::iommu::MalformedUnits>,
    apertures: &tairix_pci::Apertures,
    sink: &mut crate::boot_hwtree::CollectingHwNodeSink,
    log: &dyn Sink,
) {
    // SAFETY: forwarded — `rsdp` (and its MCFG) are identity-mapped per the
    // caller's contract.
    let segments = unsafe { pci_segments(rsdp, apertures, log) };
    let routes = core::cell::RefCell::new(Vec::new());
    let probe = |units: &mut dyn crate::pci_probe::UnitTopology| {
        let mut publish =
            |segment: crate::hwdiscovery::PciSegment,
             bus: &dyn crate::pci_host::HostBus,
             _topology: &tairix_pci::topology::Topology,
             functions: &[tairix_abi::driver::bus::BusDevice],
             dma: crate::hwdiscovery::DmaIdentity<'_>,
             sink: &mut crate::boot_hwtree::CollectingHwNodeSink| {
                let walk = crate::hwdiscovery::PciWalk {
                    segment,
                    functions,
                    bus,
                    registers: &crate::x86_64::registers::KernelRegisters,
                    dma,
                    coherence: tairix_arch_x86_64::DMA_COHERENCE,
                };
                // The virtio-blk node is match-key-only: the in-kernel floor
                // bring-up re-resolves its transport from configuration space and
                // routes its own MSI-X. Whatever was collected is seeded
                // regardless.
                let _ = crate::hwdiscovery::observe_virtio_pci_block_devices(&walk, sink, log);
                observe_interrupt_driven(bus, &walk, &routes, sink, log);
                // A virtio-iommu function raises its faults by message.
                crate::hwdiscovery::describe_virtio_units(
                    segment.number,
                    bus,
                    &|_| None,
                    sink.nodes_mut(),
                );
            };
        // The x86_64 port names no external-facing port of its own: ACPI
        // describes them in AML, which the kernel does not run, so only a
        // hot-plug capable slot marks one.
        crate::pci_probe::probe(
            segments,
            units,
            &|_segment, _address| false,
            &mut publish,
            sink,
            log,
        )
    };
    let (owned, ioapics, remapping, x2apic_opt_out) = match table {
        UnitTable::Dmar(dmar) => {
            let mut units = DmarUnits {
                dmar: Some(dmar),
                nodes: tairix_arch_x86_64::acpi::UnitNodes::default(),
                ioapics: Vec::new(),
            };
            let owned = probe(&mut units);
            let flags = dmar.flags();
            (
                owned,
                units.ioapics,
                flags.interrupt_remapping(),
                flags.x2apic_opt_out(),
            )
        }
        // AMD-Vi remaps interrupts wherever it translates, and IVRS asks the
        // OS to keep away from nothing.
        UnitTable::Ivrs(ivrs) => {
            let mut units = IvrsUnits {
                ivrs,
                nodes: tairix_arch_x86_64::acpi::UnitNodes::default(),
                ioapics: Vec::new(),
            };
            let owned = probe(&mut units);
            (owned, units.ioapics, true, false)
        }
        // A virtio-iommu remaps no interrupt.
        UnitTable::Viot(viot) => {
            let mut units = ViotUnits {
                viot,
                nodes: tairix_arch_x86_64::acpi::UnitNodes::default(),
            };
            (probe(&mut units), Vec::new(), false, false)
        }
        // Units no table can be read for may sit on any segment, so none of
        // its functions is published, unless the administrator chose
        // otherwise.
        UnitTable::Malformed(_) if malformed.is_none_or(|units| !units.unconfined) => (
            probe(&mut crate::pci_probe::Undescribed),
            Vec::new(),
            false,
            false,
        ),
        UnitTable::Malformed(_) | UnitTable::None => {
            let mut units = DmarUnits {
                dmar: None,
                nodes: tairix_arch_x86_64::acpi::UnitNodes::default(),
                ioapics: Vec::new(),
            };
            (probe(&mut units), Vec::new(), false, false)
        }
    };
    crate::x86_64::remapping::publish_boot_sources(crate::x86_64::remapping::BootSources {
        ioapics,
        routes: routes.into_inner(),
        remapping,
        x2apic_opt_out,
    });
    crate::pci_host::publish(
        owned,
        alloc::boxed::Box::new(crate::x86_64::registers::KernelRegisters),
        log,
    );
}

/// Every segment the firmware's `MCFG` describes, each reached through its
/// ECAM regions; with no usable MCFG, segment 0 through mechanism #1. A
/// segment one of whose regions does not lie wholly within the direct map is
/// left unprobed, logged: no window over it can be trusted to stay in bounds.
///
/// # Safety
///
/// `rsdp` must be a validated RSDP whose tables lie in the identity-mapped
/// 0..4 GiB window (forwarded from [`seed_pci`]).
unsafe fn pci_segments(
    rsdp: &acpi::Rsdp,
    apertures: &tairix_pci::Apertures,
    log: &dyn Sink,
) -> Vec<crate::pci_probe::ProbeSegment> {
    // SAFETY: forwarded — `rsdp` is identity-mapped per the caller.
    let mcfg = unsafe { acpi::locate_mcfg(rsdp) }.and_then(|bytes| {
        let parsed = acpi::Mcfg::parse(bytes).ok();
        if parsed.is_none() {
            crate::pci_probe::log_discovery(
                log,
                Level::Error,
                "mcfg table malformed; configuration space through mechanism one",
            );
        }
        parsed
    });
    let mut segments = Vec::new();
    if let Some(mcfg) = mcfg.filter(|mcfg| mcfg.allocations().next().is_some()) {
        let mut numbers: Vec<u16> = Vec::new();
        for allocation in mcfg.allocations() {
            if !numbers.contains(&allocation.segment()) && numbers.try_reserve(1).is_ok() {
                numbers.push(allocation.segment());
            }
        }
        numbers.sort_unstable();
        if segments.try_reserve_exact(numbers.len()).is_err() {
            crate::pci_probe::log_discovery(
                log,
                Level::Error,
                "pci segments unrecorded; none probed",
            );
            return segments;
        }
        for number in numbers {
            match ecam_segment(&mcfg, number, apertures.clone()) {
                Some(bus) => segments.push(crate::pci_probe::ProbeSegment {
                    number,
                    bus: Box::new(bus),
                }),
                None => crate::pci_probe::log_discovery(
                    log,
                    Level::Error,
                    "pci segment configuration region unmappable; segment unprobed",
                ),
            }
        }
        return segments;
    }
    if segments.try_reserve_exact(1).is_ok() {
        segments.push(crate::pci_probe::ProbeSegment {
            number: 0,
            bus: Box::new(tairix_pci::mechanism_one(
                tairix_arch_x86_64::pio::x86_port_io(),
                apertures.clone(),
            )),
        });
    }
    segments
}

/// An ECAM bus over every region `mcfg` gives segment `number`, or [`None`]
/// when one does not lie wholly within the live direct map (fail closed).
fn ecam_segment(
    mcfg: &acpi::Mcfg<'_>,
    number: u16,
    apertures: tairix_pci::Apertures,
) -> Option<impl crate::pci_probe::SegmentBus + 'static> {
    use tairix_abi::RegisterWindow;

    let mut regions = Vec::new();
    for allocation in mcfg.allocations().filter(|a| a.segment() == number) {
        let base = allocation.window_base();
        let window_len = allocation.window_len();
        let len = usize::try_from(window_len).ok()?;
        let ptr = crate::x86_64::registers::device_registers(base, window_len)?;
        // SAFETY: `base .. base + len` is a firmware-described ECAM region
        // (`acpi::Mcfg`, which refuses any two of one segment overlapping),
        // proven above to lie wholly within the live direct physical map, so
        // `ptr` is valid for `len` bytes for the kernel's lifetime. Config
        // space is reached only through the bounded `RegisterWindow`
        // accessors this window backs: by the boot probe alone, then only
        // under the kernel's PCI host lock.
        let window = unsafe { RegisterWindow::from_mapping(base, ptr, len) };
        regions.try_reserve(1).ok()?;
        regions.push(tairix_pci::EcamRegion::new(window, allocation.buses()));
    }
    Some(tairix_pci::mechanism_ecam(regions, apertures))
}

/// What a segment's walk says of the functions the DMAR's scopes name: the
/// bus ranges and aliases they are resolved through, read from the probe's
/// own walk rather than configuration space again.
struct Fabric<'t>(&'t tairix_pci::topology::Topology);

impl tairix_arch_x86_64::dmar::BridgeBuses for Fabric<'_> {
    fn bus_range(&self, bridge: tairix_arch_x86_64::dmar::SourceId) -> Option<(u8, u8)> {
        let address = tairix_abi::driver::pci::config_address(bridge.raw());
        let index = self.0.index_of(address)?;
        match self.0.functions()[index].header {
            tairix_pci::topology::Header::Bridge {
                secondary,
                subordinate,
                ..
            } => Some((secondary, subordinate)),
            tairix_pci::topology::Header::Endpoint => None,
        }
    }
}

impl tairix_arch_x86_64::dmar::DmaAliases for Fabric<'_> {
    fn aliases(
        &self,
        source: tairix_arch_x86_64::dmar::SourceId,
        visit: &mut dyn FnMut(tairix_arch_x86_64::dmar::SourceId),
    ) {
        let address = tairix_abi::driver::pci::config_address(source.raw());
        let Some(index) = self.0.index_of(address) else {
            return;
        };
        for alias in self.0.aliases(index) {
            visit(tairix_arch_x86_64::dmar::SourceId::at(
                tairix_abi::driver::pci::config_address(alias.requester),
            ));
        }
    }
}

impl tairix_arch_x86_64::dmar::Fabric for Fabric<'_> {
    fn untrusted(&self, source: tairix_arch_x86_64::dmar::SourceId) -> bool {
        self.0
            .index_of(tairix_abi::driver::pci::config_address(source.raw()))
            .is_some_and(|index| self.0.untrusted(index).is_some())
    }
}

impl tairix_arch_x86_64::ivrs::Walk for Fabric<'_> {
    fn functions(&self, visit: &mut dyn FnMut(tairix_arch_x86_64::dmar::SourceId)) {
        for function in self.0.functions() {
            visit(tairix_arch_x86_64::dmar::SourceId::from_raw(
                function.requester_id(),
            ));
        }
    }
}

/// Log what an emission of unit nodes could not place.
fn log_unit_nodes(log: &dyn Sink, nodes: tairix_arch_x86_64::acpi::UnitNodes) {
    if nodes.dropped != 0 {
        crate::pci_probe::log_discovery(
            log,
            Level::Warn,
            "firmware dma windows no unit's node can carry; those devices lose them",
        );
    }
    if nodes.untrusted != 0 {
        crate::pci_probe::log_discovery(
            log,
            Level::Warn,
            "firmware dma windows refused below external-facing ports",
        );
    }
}

/// Each segment's walk as a [`Fabric`], or [`None`] when they cannot be
/// held.
fn fabrics<'t>(
    walks: &[(u16, &'t tairix_pci::topology::Topology)],
) -> Option<Vec<(u16, Fabric<'t>)>> {
    let mut fabrics = Vec::new();
    fabrics.try_reserve_exact(walks.len()).ok()?;
    fabrics.extend(walks.iter().map(|&(segment, walk)| (segment, Fabric(walk))));
    Some(fabrics)
}

/// The units an IVRS describes, as the shared probe reads them.
struct IvrsUnits<'i, 'a> {
    ivrs: &'i tairix_arch_x86_64::ivrs::Ivrs<'a>,
    /// What [`crate::pci_probe::UnitTopology::emit`] placed.
    nodes: tairix_arch_x86_64::acpi::UnitNodes,
    /// The I/O APICs the placed units name.
    ioapics: Vec<crate::x86_64::remapping::IoApicSource>,
}

impl crate::pci_probe::UnitTopology for IvrsUnits<'_, '_> {
    fn covers(&self, segment: u16) -> bool {
        self.ivrs.units().any(|unit| unit.segment() == segment)
    }

    fn emit(
        &mut self,
        walks: &[(u16, &tairix_pci::topology::Topology)],
        sink: &mut dyn tairix_arch_api::HwNodeSink,
        log: &dyn Sink,
    ) -> Result<(), crate::pci_probe::Unconfined> {
        let fabrics = fabrics(walks).ok_or(crate::pci_probe::Unconfined)?;
        let walk = |segment: u16| {
            fabrics
                .iter()
                .find(|(at, _)| *at == segment)
                .map(|(_, fabric)| fabric as &dyn tairix_arch_x86_64::ivrs::Walk)
        };
        let nodes = tairix_arch_x86_64::ivrs::emit_unit_nodes(
            self.ivrs,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            tairix_kernel_iommu_amdvi::COMPATIBLE,
            &walk,
            sink,
        )
        .map_err(|_| crate::pci_probe::Unconfined)?;
        log_unit_nodes(log, nodes);
        let mut unrecorded = false;
        tairix_arch_x86_64::ivrs::ioapic_sources(
            self.ivrs,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            nodes,
            &mut |id, unit, requester| {
                if self.ioapics.try_reserve(1).is_ok() {
                    self.ioapics.push(crate::x86_64::remapping::IoApicSource {
                        id,
                        unit,
                        requester,
                    });
                } else {
                    unrecorded = true;
                }
            },
        );
        if unrecorded {
            // A pin whose I/O APIC is unrecorded keeps the machine unremapped.
            crate::pci_probe::log_discovery(
                log,
                Level::Error,
                "io-apic remapping sources unrecorded",
            );
        }
        self.nodes = nodes;
        Ok(())
    }

    fn strands(&self, segment: u16) -> bool {
        self.nodes
            .strands(self.ivrs.units().map(|unit| unit.segment()), segment)
    }

    fn stream(
        &self,
        segment: u16,
        _walk: &tairix_pci::topology::Topology,
        requester: u16,
    ) -> Option<(u32, u32)> {
        let unit = tairix_arch_x86_64::ivrs::unit_node(
            self.ivrs,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            self.nodes,
            segment,
            requester,
        )?;
        // An AMD-Vi unit indexes its device table by requester id.
        Some((unit, u32::from(requester)))
    }

    fn firmware_alias(&self, segment: u16, requester: u16) -> Option<u16> {
        self.ivrs
            .units()
            .find(|unit| unit.segment() == segment && unit.covers(requester))?
            .alias_of(requester)
    }

    /// An AMD-Vi unit serves one segment and no platform master.
    fn contested(&self, _segment: u16, _unit: u32, _stream: u32) -> bool {
        false
    }
}

/// The units a VIOT describes, as the shared probe reads them.
struct ViotUnits<'v, 'a> {
    viot: &'v tairix_arch_x86_64::viot::Viot<'a>,
    /// What [`crate::pci_probe::UnitTopology::emit`] placed.
    nodes: tairix_arch_x86_64::acpi::UnitNodes,
}

impl crate::pci_probe::UnitTopology for ViotUnits<'_, '_> {
    fn covers(&self, segment: u16) -> bool {
        self.viot.covers(segment)
    }

    /// A virtio-iommu reports its reserved regions when probed, so firmware
    /// keeps no window through one.
    fn emit(
        &mut self,
        _walks: &[(u16, &tairix_pci::topology::Topology)],
        sink: &mut dyn tairix_arch_api::HwNodeSink,
        _log: &dyn Sink,
    ) -> Result<(), crate::pci_probe::Unconfined> {
        self.nodes = tairix_arch_x86_64::viot::emit_unit_nodes(
            self.viot,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            tairix_kernel_iommu_virtio::COMPATIBLE,
            tairix_virtio::transport_mmio::COMPATIBLE.as_bytes(),
            tairix_kernel_iommu_virtio::MMIO_WINDOW,
            sink,
        )
        .map_err(|_| crate::pci_probe::Unconfined)?;
        Ok(())
    }

    fn strands(&self, segment: u16) -> bool {
        self.viot.strands(self.nodes, segment)
    }

    fn stream(
        &self,
        segment: u16,
        _walk: &tairix_pci::topology::Topology,
        requester: u16,
    ) -> Option<(u32, u32)> {
        tairix_arch_x86_64::viot::endpoint(
            self.viot,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            self.nodes,
            segment,
            requester,
        )
    }

    /// A VIOT names each function's endpoint by its requester id alone.
    fn firmware_alias(&self, _segment: u16, _requester: u16) -> Option<u16> {
        None
    }

    fn contested(&self, segment: u16, unit: u32, stream: u32) -> bool {
        tairix_arch_x86_64::viot::contested(
            self.viot,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            segment,
            unit,
            stream,
        )
    }
}

/// The units a DMAR describes, as the shared probe reads them.
struct DmarUnits<'d, 'a> {
    dmar: Option<&'d tairix_arch_x86_64::dmar::Dmar<'a>>,
    /// What [`crate::pci_probe::UnitTopology::emit`] placed.
    nodes: tairix_arch_x86_64::acpi::UnitNodes,
    /// The I/O APICs the placed units' scopes name.
    ioapics: Vec<crate::x86_64::remapping::IoApicSource>,
}

impl crate::pci_probe::UnitTopology for DmarUnits<'_, '_> {
    /// A VT-d unit knows a function by its requester id and the walk's
    /// aliases alone.
    fn firmware_alias(&self, _segment: u16, _requester: u16) -> Option<u16> {
        None
    }

    /// A VT-d unit serves one segment and no platform master.
    fn contested(&self, _segment: u16, _unit: u32, _stream: u32) -> bool {
        false
    }

    fn covers(&self, segment: u16) -> bool {
        self.dmar
            .is_some_and(|dmar| dmar.units().any(|unit| unit.segment() == segment))
    }

    fn emit(
        &mut self,
        walks: &[(u16, &tairix_pci::topology::Topology)],
        sink: &mut dyn tairix_arch_api::HwNodeSink,
        log: &dyn Sink,
    ) -> Result<(), crate::pci_probe::Unconfined> {
        let Some(dmar) = self.dmar else {
            return Ok(());
        };
        let fabrics = fabrics(walks).ok_or(crate::pci_probe::Unconfined)?;
        let fabric = |segment: u16| {
            fabrics
                .iter()
                .find(|(at, _)| *at == segment)
                .map(|(_, fabric)| fabric as &dyn tairix_arch_x86_64::dmar::Fabric)
        };
        let Ok(nodes) = tairix_arch_x86_64::dmar::emit_unit_nodes(
            dmar,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            tairix_kernel_iommu_vtd::COMPATIBLE,
            &fabric,
            sink,
        ) else {
            return Err(crate::pci_probe::Unconfined);
        };
        log_unit_nodes(log, nodes);
        let mut unrecorded = false;
        tairix_arch_x86_64::dmar::ioapic_sources(
            dmar,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            nodes,
            &fabric,
            &mut |id, unit, source| {
                if self.ioapics.try_reserve(1).is_ok() {
                    self.ioapics.push(crate::x86_64::remapping::IoApicSource {
                        id,
                        unit,
                        requester: source.raw(),
                    });
                } else {
                    unrecorded = true;
                }
            },
        );
        if unrecorded {
            // A pin whose I/O APIC is unrecorded keeps the machine unremapped.
            crate::pci_probe::log_discovery(
                log,
                Level::Error,
                "io-apic remapping sources unrecorded",
            );
        }
        self.nodes = nodes;
        Ok(())
    }

    fn strands(&self, segment: u16) -> bool {
        self.dmar.is_some_and(|dmar| {
            self.nodes
                .strands(dmar.units().map(|unit| unit.segment()), segment)
        })
    }

    fn stream(
        &self,
        segment: u16,
        walk: &tairix_pci::topology::Topology,
        requester: u16,
    ) -> Option<(u32, u32)> {
        let unit = tairix_arch_x86_64::dmar::unit_node(
            self.dmar?,
            crate::hwtree_node_ids::IOMMU_UNIT_NODE_BASE_ID,
            self.nodes,
            segment,
            tairix_arch_x86_64::dmar::SourceId::at(tairix_abi::driver::pci::config_address(
                requester,
            )),
            &Fabric(walk),
        )?;
        // A VT-d unit knows every function by its requester id.
        Some((unit, u32::from(requester)))
    }
}

/// Discover the interrupt-driven PCI functions `walk` found — virtio-net,
/// sound and input, xHCI and HD Audio controllers — each with its MSI-X, or
/// else its MSI, routed by `bus` in
/// compatibility format to a vector of its own and recorded in `routes`, for
/// remapping to take over once the units are up. The function cannot master
/// yet, so nothing it raises is live before then.
fn observe_interrupt_driven(
    bus: &dyn crate::pci_host::HostBus,
    walk: &crate::hwdiscovery::PciWalk<'_>,
    routes: &core::cell::RefCell<Vec<crate::x86_64::remapping::PendingRoute>>,
    sink: &mut crate::boot_hwtree::CollectingHwNodeSink,
    log: &dyn Sink,
) {
    // A function that cannot be given a vector, a record or a programmed
    // message is left undiscovered (fail closed): a granted line that never
    // delivers would strand its driver parked forever.
    let route_irq = |bdf: u64| -> Option<crate::hwdiscovery::DeviceInterrupt> {
        let node = walk.segment.node_id(bdf)?;
        let mut routes = routes.try_borrow_mut().ok()?;
        routes.try_reserve(1).ok()?;
        let vector = crate::x86_64::msi::allocate().ok()?;
        let programmed = crate::x86_64::msi::compatibility_message(vector).is_some_and(|message| {
            crate::pci_host::route_message(
                bus,
                bdf,
                message,
                &crate::x86_64::registers::KernelRegisters,
            )
            .is_ok()
        });
        // A function left unpublished never masters, so it cannot raise the
        // vector even if its entry was written.
        if !programmed {
            crate::x86_64::msi::release(vector.line);
            return None;
        }
        routes.push(crate::x86_64::remapping::PendingRoute { node, vector });
        // The interrupt window is claimed before translation, so no domain
        // maps a doorbell.
        Some(crate::hwdiscovery::DeviceInterrupt::line(
            tairix_abi::HwResource::message_irq(
                u64::from(vector.line),
                crate::pci_host::MSIX_ENTRY,
            ),
        ))
    };
    // An enumeration error leaves that class undiscovered; whatever was
    // collected is seeded regardless.
    let _ = crate::hwdiscovery::observe_virtio_pci_network_devices(walk, &route_irq, sink, log);
    let _ = crate::hwdiscovery::observe_virtio_pci_audio_devices(walk, &route_irq, sink, log);
    let _ = crate::hwdiscovery::observe_virtio_pci_input_devices(walk, &route_irq, sink, log);
    for class in [
        crate::hwdiscovery::XHCI_CONTROLLERS,
        crate::hwdiscovery::HD_AUDIO_CONTROLLERS,
    ] {
        let _ = crate::hwdiscovery::observe_pci_class_functions(
            walk, bus, class, &route_irq, sink, log,
        );
    }
}

/// Enable the No-Execute-Enable bit in `IA32_EFER` on the current CPU.
///
/// # Safety
///
/// Must run in ring 0 with interrupts disabled (the BSP after
/// `percpu::init`). Performs a `rdmsr`/`wrmsr` read-modify-write that only
/// sets [`EFER_NXE`], preserving every other `IA32_EFER` bit.
unsafe fn enable_nxe() {
    use tairix_arch_x86_64::msr;

    // SAFETY: `IA32_EFER` is implemented on every long-mode CPU and the
    // caller runs in ring 0; writing it back with only `NXE` newly set is the
    // documented enable sequence, preserving `SCE`/`LME`/`LMA`.
    unsafe { msr::write(IA32_EFER, msr::read(IA32_EFER) | EFER_NXE) }
}

/// Widen the direct physical map to `[0, gib GiB)`, reserving the page
/// tables it needs out of `map` first.
///
/// A map that already covers `gib` — the boot trampoline's own floor, on a
/// machine whose RAM fits it — is left alone. Otherwise the tables are
/// carved below that floor, because that is what the widening can still
/// write through, and reserved so the frame allocator never hands them out
/// from under the live page tables.
fn install_direct_physical_map(map: &mut BootMemoryMap, gib: usize) -> Result<(), BootError> {
    if gib <= paging::BOOT_IDENTITY_GIB {
        return Ok(());
    }
    let tables = match paging::physmap_table_frames(gib) {
        0 => 0,
        pages => crate::mem_map::carve_frames_from_map(map, pages, paging::BOOT_IDENTITY_END)
            .ok_or(BootError::DirectMapInstall)?,
    };
    // SAFETY: this runs on the BSP before any secondary is brought up and
    // before the frame allocator exists, so no other CPU walks the live
    // PML4 and nothing else owns `tables` — the carve above reserved those
    // page-aligned frames out of the map for this use alone (and a part
    // with 1 GiB pages within one span needs none, so the run may be
    // empty).
    if unsafe { paging::install_boot_physmap(gib, tables) } {
        Ok(())
    } else {
        Err(BootError::DirectMapInstall)
    }
}

/// Build the canonical memory map from the firmware description, returning
/// it alongside the installed-RAM total — the usable-RAM byte sum taken
/// **before** the kernel-image reservation below drops that range from the
/// map (firmware `Reserved` regions span ACPI/MMIO, not RAM, so the usable
/// sum is the honest installed figure a PC firmware map can state).
/// `line`, copied out of the loader memory the frame allocator reuses.
fn kept(line: &str) -> Result<&'static str, BootError> {
    let mut copy = String::new();
    copy.try_reserve_exact(line.len())
        .map_err(|_| BootError::CommandLineCopy)?;
    copy.push_str(line);
    Ok(copy.leak())
}

fn build_memory_map(data: &BootData<'_>) -> Result<(BootMemoryMap, u64), BootError> {
    let mut map = BootMemoryMap::new();
    firmware_regions(data, &mut |region| {
        push_descriptor(&mut map, region.descriptor());
    })?;

    // The installed-RAM total: usable firmware bytes before the
    // kernel-image carve below (the carve drops the range from the map, so
    // it cannot be recovered afterwards).
    let installed_memory_bytes: u64 = map
        .regions()
        .iter()
        .filter(|region| region.kind == RegionKind::Usable)
        .fold(0u64, |acc, region| acc.saturating_add(region.length));

    // Reserve the running kernel image (boot trampoline through the end of
    // .bss, which includes the bump heap) out of the firmware-usable RAM.
    //
    // The loader (GRUB, or QEMU's PVH ELF loader) places this kernel in
    // memory the boot map reports as usable (`EfiLoaderData`/
    // `EfiConventionalMemory` on the UEFI path, plain RAM on the PVH
    // path). Without this carve-out the frame allocator eventually hands
    // out frames overlapping the running kernel's code and heap, and the
    // `spawn` image builder's zero-fill / page-table writes corrupt the live
    // kernel (`plans/PI.md` X4 follow-on). This is the x86_64 sibling of the
    // aarch64 `mem_map` `[ram_base, __kernel_end)` reservation (`plans/PI.md`
    // P6c-1) — without it nothing protected the kernel image.
    let (kstart, kend) = kernel_image_phys_bounds();
    map.reserve_range(kstart, kend);

    Ok((map, installed_memory_bytes))
}

/// Physical `[start, end)` bounds of the running kernel image — the boot
/// trampoline (`__boot_phys_start`, fixed at 1 MiB) through the end of `.bss`
/// (`__kernel_phys_end`, which the linker emits as a *physical* address; see
/// `kernel/arch/x86_64/linker.ld`). The bump heap lives in `.bss`, so the
/// range covers it too.
fn kernel_image_phys_bounds() -> (PhysAddr, PhysAddr) {
    // `__boot_phys_start` / `__kernel_phys_end` are absolute symbols the
    // linker script defines; only their *addresses* (i.e. their linked
    // values) are read here — they are never dereferenced, so taking the
    // address is safe.
    let start = core::ptr::addr_of!(__boot_phys_start) as u64;
    let end = core::ptr::addr_of!(__kernel_phys_end) as u64;
    (PhysAddr::new(start), PhysAddr::new(end))
}

extern "C" {
    /// Physical start of the kernel image (the boot trampoline at 1 MiB).
    /// Defined by `kernel/arch/x86_64/linker.ld`.
    static __boot_phys_start: u8;
    /// Physical one-past-the-end of the kernel image (end of `.bss`,
    /// including the bump heap). Defined by `kernel/arch/x86_64/linker.ld`
    /// as `. - KERNEL_VMA_BASE`, so its linked value is a physical address.
    static __kernel_phys_end: u8;
}

/// Visit every region of the memory map firmware gave: the UEFI map where the
/// loader passed one, else the BIOS one, or the PVH one.
fn firmware_regions(
    data: &BootData<'_>,
    visit: &mut dyn FnMut(bootmemory::FirmwareRegion),
) -> Result<(), BootError> {
    use bootmemory::FirmwareRegion;
    match data {
        BootData::Multiboot2(mb2) => {
            if let Some(uefi) = mb2.efi_memory_map() {
                uefi.entries().map(FirmwareRegion::Uefi).for_each(visit);
            } else if let Some(bios) = mb2.memory_map() {
                bios.entries().map(FirmwareRegion::Bios).for_each(visit);
            } else {
                return Err(BootError::NoMemoryMap);
            }
        }
        BootData::Pvh { memmap, .. } => memmap.entries().map(FirmwareRegion::Pvh).for_each(visit),
    }
    Ok(())
}

fn push_descriptor(map: &mut BootMemoryMap, desc: bootmemory::MemoryRegionDescriptor) {
    // Translate the arch-port mirror enum into the kernel/mem
    // canonical enum. `bootmemory`'s host-side round-trip test pins
    // the two enums together at compile time so a future drift fails
    // the build.
    let kind = match desc.kind {
        bootmemory::RegionKind::Usable => RegionKind::Usable,
        bootmemory::RegionKind::Reserved => RegionKind::Reserved,
    };
    map.push(MemoryRegion {
        start: PhysAddr::new(desc.start),
        length: desc.length,
        kind,
    });
}

/// The direct-map address of the IO-APIC register block at physical
/// `phys`, or [`None`] when the block lies outside the live direct
/// physical map: the caller skips an IO-APIC it could not reach.
fn io_apic_mmio_virt(phys: u32) -> Option<usize> {
    crate::x86_64::registers::device_registers(
        u64::from(phys),
        tairix_arch_x86_64::apic::IOAPIC_WINDOW_BYTES,
    )
    .map(|registers| registers.as_ptr() as usize)
}

/// Every IO-APIC the MADT advertises that the direct map reaches and whose
/// global system interrupts no other holds, each pin wired as the MADT's
/// overrides say.
fn discover_io_apics(
    madt: &acpi::Madt<'_>,
    log: &dyn Sink,
) -> Result<Vec<crate::x86_64::ioapic_controller::IoApicBlock<VolatileIoApicMmio>>, BootError> {
    use crate::x86_64::ioapic_controller::{gsis_free, IoApicBlock};
    use tairix_arch_x86_64::apic::PinWiring;

    let mut overrides: Vec<(u32, u16)> = Vec::new();
    for entry in madt.entries() {
        if let MadtEntry::InterruptSourceOverride { gsi, flags, .. } = entry {
            overrides
                .try_reserve(1)
                .map_err(|_| BootError::IoApicUnrecorded)?;
            overrides.push((gsi, flags));
        }
    }
    let wiring = |gsi: u32| {
        let flags = overrides
            .iter()
            .find(|(at, _)| *at == gsi)
            .map(|&(_, flags)| flags);
        PinWiring::of(gsi, flags)
    };

    let mut blocks: Vec<IoApicBlock<VolatileIoApicMmio>> = Vec::new();
    for entry in madt.entries() {
        let MadtEntry::IoApic {
            id,
            address,
            gsi_base,
        } = entry
        else {
            continue;
        };
        // A block the direct map does not reach is an unusable block: skip it
        // rather than dereference an address nothing maps. If that leaves
        // none, the caller fails closed below.
        let Some(mmio_virt) = io_apic_mmio_virt(address) else {
            continue;
        };
        // SAFETY: the IO-APIC register block MADT publishes sits at a
        // firmware-fixed physical frame, proven above to lie wholly within
        // the live direct physical map, so the pointer is valid for the
        // block for the kernel's lifetime; the controller serialises every
        // access to it.
        let mmio = unsafe { VolatileIoApicMmio::new(mmio_virt as *mut u32) };
        let mut ioapic = IoApic::new(mmio);
        let pin_count = (u32::from(ioapic.max_redirection_entry()) + 1)
            .min(tairix_arch_x86_64::apic::IOAPIC_ADDRESSABLE_PINS);
        // Two blocks claiming one GSI would leave it reaching either: refuse
        // the later block, as Linux does, masking every pin so none firmware
        // left armed raises a vector the pool hands another line.
        if !gsis_free(
            &blocks,
            gsi_base,
            pin_count,
            crate::x86_64::msi::MSI_LINE_BASE,
        ) {
            for pin in (0..pin_count).filter_map(|pin| u8::try_from(pin).ok()) {
                let low = ioapic.read_redirection_entry_low(pin);
                ioapic.write_redirection_low(
                    pin,
                    low | tairix_arch_x86_64::msr::halves(
                        tairix_arch_x86_64::apic::REDIRECTION_MASKED,
                    )
                    .0,
                );
            }
            crate::pci_probe::log_discovery(
                log,
                Level::Error,
                "io-apic global system interrupts conflict; block unused, masked",
            );
            continue;
        }
        let mut pins = Vec::new();
        pins.try_reserve_exact(pin_count as usize)
            .and_then(|()| blocks.try_reserve(1))
            .map_err(|_| BootError::IoApicUnrecorded)?;
        pins.extend((gsi_base..gsi_base + pin_count).map(wiring));
        blocks.push(IoApicBlock {
            id,
            gsi_base,
            ioapic,
            wiring: pins,
        });
    }
    if blocks.is_empty() {
        return Err(BootError::NoIoApic);
    }
    Ok(blocks)
}

/// Discover every IO-APIC the MADT advertises, mask every pin of the
/// production [`IoApicController`] over them, install every external vector
/// pins and messages draw from, and claim COM1's pin for the console.
///
/// No other pin takes a vector until its line is bound, so the IO-APICs may
/// carry any number of pins.
///
/// Returns the [`IrqRouting`] the caller stores in [`BinArch`].
///
/// # Failure modes
///
/// * [`BootError::NoIoApic`] if MADT advertises none.
/// * [`BootError::IoApicUnrecorded`] if the IO-APICs' bookkeeping cannot be
///   had.
/// * [`BootError::IrqIdtInstall`] if an external vector's IDT entry cannot be
///   installed — pathological, the BSP has finished `percpu::init` by this
///   point.
fn discover_and_program_io_apics(
    madt: &acpi::Madt<'_>,
    bsp_lapic_id: u32,
    log: &dyn Sink,
) -> Result<IrqRouting, BootError> {
    let blocks = discover_io_apics(madt, log)?;
    let controller = IoApicController::<_>::new(blocks).ok_or(BootError::IoApicUnrecorded)?;
    let controller: &'static IoApicController<VolatileIoApicMmio> = Box::leak(Box::new(controller));
    controller.quiesce();
    // The `irq_qemu_x86_64` and `ps2_input_qemu_x86_64` verticals reach the
    // typed controller through this slot.
    crate::x86_64::ioapic_controller::publish_typed(controller);
    let max_gsi = controller.last_gsi().ok_or(BootError::NoIoApic)?;

    let vectors =
        crate::x86_64::vectors::install(bsp_lapic_id).map_err(|_| BootError::IrqIdtInstall)?;
    let composite: &'static crate::x86_64::msi::CompositeIrqController<VolatileIoApicMmio> =
        Box::leak(Box::new(crate::x86_64::msi::CompositeIrqController::new(
            controller,
            vectors,
            arch_irq::global_routing(),
            &crate::x86_64::remapping::LIVE_PIN_REMAPPING,
        )));
    crate::x86_64::msi::publish_composite(composite);

    // COM1 is the legacy ISA IRQ 4, which the MADT may override onto another
    // GSI. Its pin is claimed before remapping is planned, so the plan
    // rewrites it with the rest; a GSI no pin owns leaves the console on its
    // poll-backed path (fail closed).
    let com1_gsi = resolve_com1_gsi(madt);
    if composite.activate(com1_gsi).is_ok() {
        crate::x86_64::com1_rx::set_com1_console_gsi(com1_gsi);
    }

    // The bind ceiling covers both the real GSIs and the MSI lines.
    Ok(IrqRouting {
        max_line: max_gsi.max(crate::x86_64::msi::LAST_MSI_LINE),
        controller: composite as &'static (dyn IrqController + Send + Sync),
    })
}

/// The legacy ISA interrupt line COM1 asserts on every x86 PC platform.
const COM1_ISA_IRQ: u8 = 4;

/// Resolve the GSI the COM1 (ISA IRQ 4) interrupt is routed to: an
/// Interrupt-Source-Override in the MADT remaps a legacy ISA IRQ to a GSI,
/// so honour an override for source 4; absent one, the ISA IRQ maps
/// identically (GSI 4), the ACPI default.
fn resolve_com1_gsi(madt: &acpi::Madt<'_>) -> u32 {
    for entry in madt.entries() {
        if let acpi::MadtEntry::InterruptSourceOverride { source, gsi, .. } = entry {
            if source == COM1_ISA_IRQ {
                return gsi;
            }
        }
    }
    u32::from(COM1_ISA_IRQ)
}

fn verify_bsp_present(madt: &acpi::Madt<'_>, bsp_lapic_id: u32) -> Result<(), BootError> {
    if madt.processors().any(|id| id == bsp_lapic_id) {
        Ok(())
    } else {
        Err(BootError::BspLapicMissing)
    }
}

// --- Compile-time invariants ---------------------------------------

// SAFETY-INVARIANT: the production dispatch callback exposes the
// type `syscall_entry::SyscallDispatchFn` expects. The dispatch
// module already pins this at compile time; we re-coerce here at the
// call-site to catch a regression at the `set_dispatch_callback`
// install rather than only in the dispatch module's own tests.
const _DISPATCH_CALLBACK_INSTALLABLE: syscall_entry::SyscallDispatchFn = production_dispatch;

// SAFETY-INVARIANT: a 16-KiB per-CPU stack is sufficient to hold a
// `[u64; SYSCALL_MAX_ARGS]` frame plus the kernel-side trampoline's
// own activation record many times over. Encode the lower bound
// here so a future shrink of `KERNEL_STACK_BYTES` fails the build
// before reaching QEMU.
const _KERNEL_STACK_FITS_AT_LEAST_ONE_FRAME: () = {
    assert!(KERNEL_STACK_BYTES >= SYSCALL_MAX_ARGS * core::mem::size_of::<u64>() * 16);
};
