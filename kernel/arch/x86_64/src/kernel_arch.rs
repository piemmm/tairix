//! Stage 3a (c7-arch): the x86_64 `SchedulerArch` implementation.
//!
//! `X86_64Arch` is the concrete handle the architecture-neutral
//! kernel reaches for through the Arch HAL
//! [`tairix_arch_api::SchedulerArch`]. It is the
//! only production implementation of that trait inside the workspace
//! (the host-side `TestArch` shipped by `kernel/sched` is
//! feature-gated to `test-arch`).
//!
//! # Surface
//!
//! - `X86_64Arch::new` — validates a `(boot_cpu_id, boot_apic_id,
//!   cpu_to_lapic)` triple parsed from the ACPI MADT and returns the
//!   handle ready to be wrapped in `Arc`.
//! - `SchedulerArch::current_cpu` — on bare metal, reads the LAPIC
//!   ID register and looks it up in the handle's
//!   [`crate::cpumap::ApicMap`]; on host builds, returns the
//!   boot CPU's dense `CpuId` so host tests of the scheduler remain
//!   deterministic (no flaky tests).
//! - `SchedulerArch::ticks_now` — on bare metal, reads `RDTSC` (the
//!   invariant TSC modern x86_64 CPUs expose; QEMU advertises it); on
//!   host builds, returns a monotonically-increasing per-instance
//!   counter so `SchedulerArch`'s "monotonically non-decreasing"
//!   contract holds in tests too.
//! - `SchedulerArch::send_ipi` — on bare metal, issues a directed
//!   IPI through the calling CPU's [`crate::apic::LocalApic`]; on host
//!   builds, records the IPI in an in-instance counter so host tests can
//!   assert preemption was requested.
//! - `halt` — a free function that masks interrupts and parks the
//!   CPU forever on `hlt`. The companion `tairix-kernel` bin crate
//!   (Stage 3a (c7-bin)) uses it to satisfy
//!   `tairix_kernel_core::KernelArch::halt`. The trait impl lives in
//!   the bin crate because pulling `tairix-kernel-core` into the arch
//!   crate would transitively force a `#[global_allocator]` into the
//!   two pre-existing freestanding Stage-2 QEMU test bins — see the
//!   note in `kernel/arch/x86_64/Cargo.toml`.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

use tairix_arch_api::{
    CoreClass, CpuId, CpuMask, CrossCpuTlbShootdown, SchedulerArch, SecondaryBringup, SmpError,
};

use crate::cpumap::{ApicMap, Unmappable, NO_LAPIC, VACANT};
use crate::hybrid;

/// Caller-owned, `&'static` per-CPU backing for an [`X86_64Arch`] handle
/// (per-CPU bookkeeping is sized by the caller from
/// discovered hardware, never a fixed `const` ceiling baked into the
/// arch crate).
///
/// The const parameter `N` is the number of logical-CPU slots the
/// constructing caller sizes for its machine: a single-CPU vertical uses
/// `X86_64ArchStorage<1>`, and a multi-core boot path sizes `N` from the
/// ACPI MADT processor count. The arch crate stays allocator-free
/// (watch-out — no `alloc` in a bare-metal arch crate,
/// which would force a bump heap onto the allocator-free Stage-2 QEMU
/// bins), so the caller provides the storage as a `static` (allocator-free
/// bins) or a leaked allocation (allocator-having callers); [`X86_64Arch`]
/// borrows it as `&'static` slices.
#[derive(Debug)]
pub struct X86_64ArchStorage<const N: usize> {
    /// Forward map: dense `CpuId` index → LAPIC ID, [`NO_LAPIC`] for an
    /// unpopulated slot. Written once by the constructor through the
    /// shared `&'static` borrow (atomically, so no `&'static mut` is
    /// needed) and read-only thereafter.
    cpu_to_lapic: [AtomicU32; N],

    /// Reverse map: the slots of the handle's [`ApicMap`].
    by_apic: [AtomicU64; N],

    /// What each CPU owes the TLB shootdown in flight
    /// (`crate::tlb_shootdown`).
    // Only bare metal has a second CPU to shoot down.
    #[cfg_attr(not(all(target_arch = "x86_64", target_os = "none")), allow(dead_code))]
    shootdown: [AtomicU8; N],

    /// Host-only IPI accounting — incremented on every `send_ipi` with
    /// an in-range, mapped target. Bare-metal builds never touch it.
    #[cfg_attr(all(target_arch = "x86_64", target_os = "none"), allow(dead_code))]
    host_ipi_count: [AtomicU64; N],

    /// Static [`CoreClass`] of each CPU, indexed by dense [`CpuId`];
    /// initialised to [`CoreClass::Performance`] (a homogeneous machine).
    core_classes: [AtomicU8; N],
}

impl<const N: usize> X86_64ArchStorage<N> {
    /// A zeroed backing: every map slot is the `u32::MAX` (`NO_LAPIC`)
    /// unmapped sentinel, every IPI counter is `0`, and every core class
    /// is the homogeneous [`CoreClass::Performance`] default. `const` so
    /// the allocator-free bins can place it in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cpu_to_lapic: [const { AtomicU32::new(NO_LAPIC) }; N],
            by_apic: [const { AtomicU64::new(VACANT) }; N],
            shootdown: [const { AtomicU8::new(crate::tlb_shootdown::IDLE) }; N],
            host_ipi_count: [const { AtomicU64::new(0) }; N],
            core_classes: [const { AtomicU8::new(CoreClass::Performance.as_u8()) }; N],
        }
    }
}

impl<const N: usize> Default for X86_64ArchStorage<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Failure modes of [`X86_64Arch::new`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ArchInitError {
    /// `boot_cpu_id` was outside the caller-provided storage capacity.
    BootCpuOutOfRange,
    /// The slot `cpu_to_lapic[boot_cpu_id as usize]` was `None`,
    /// implying the caller forgot to populate the boot CPU's entry
    /// before constructing the handle.
    BootCpuMissingFromLapicMap,
    /// `cpu_to_lapic[boot_cpu_id as usize]` disagreed with the
    /// caller-supplied `boot_apic_id`.
    BootCpuLapicMismatch,
    /// A CPU's APIC id is one the APICs' mode cannot name: past the eight
    /// bits xAPIC names, or the x2APIC broadcast id.
    ApicIdUnsupported,
    /// Two CPUs claim one APIC id.
    ApicIdDuplicated,
    /// A handle built after the boot's names CPUs the boot's does not.
    MachineDiffers,
    /// A handle built after the boot's borrows the storage the boot's
    /// published.
    StorageInUse,
}

impl ArchInitError {
    /// Stable cause string for audit records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BootCpuOutOfRange => "boot_cpu_out_of_range",
            Self::BootCpuMissingFromLapicMap => "boot_cpu_missing_from_lapic_map",
            Self::BootCpuLapicMismatch => "boot_cpu_lapic_mismatch",
            Self::ApicIdUnsupported => "apic_id_unsupported",
            Self::ApicIdDuplicated => "apic_id_duplicated",
            Self::MachineDiffers => "machine_differs",
            Self::StorageInUse => "storage_in_use",
        }
    }
}

/// Architecture handle for the x86_64 Tier-1 port.
///
/// Constructed once by the kernel binary's `kernel_main` from the
/// ACPI MADT, wrapped in `Arc`, and shared between every CPU through
/// `kernel_core::BootInfo`. Stable for the lifetime of the kernel
/// image — there is no mutable internal state on the bare-metal path
/// (the host-only fields exist solely to support deterministic unit
/// tests, see the module docs).
#[derive(Debug)]
pub struct X86_64Arch {
    /// Forward mapping: dense `CpuId` index → LAPIC ID of that CPU,
    /// [`NO_LAPIC`] for an unpopulated slot. Borrowed from the caller's
    /// [`X86_64ArchStorage`]; its length is the caller's CPU count, so
    /// the handle imposes no compile-time CPU ceiling. Populated once at construction; never mutated thereafter.
    cpu_to_lapic: &'static [AtomicU32],

    /// Reverse mapping: the CPU each LAPIC ID names, built once at
    /// construction from the forward one.
    // Only bare metal has a local APIC to read the running CPU's id from.
    #[cfg_attr(
        not(any(test, all(target_arch = "x86_64", target_os = "none"))),
        allow(dead_code)
    )]
    apic_map: ApicMap<'static>,

    /// Dense `CpuId` of the boot processor.
    boot_cpu_id: CpuId,

    /// LAPIC ID of the boot processor — must equal the value stored in
    /// `cpu_to_lapic[boot_cpu_id as usize]`.
    boot_cpu_lapic_id: u32,

    /// Host-only monotonic counter backing [`SchedulerArch::ticks_now`].
    ///
    /// Production builds (`target_os = "none"`) read `RDTSC` instead
    /// and never touch this field; it is kept unconditionally to keep
    /// the struct layout identical between targets (no hacks).
    //
    // Allow `dead_code` only on the bare-metal target: on the host
    // target the field is read by `ticks_now`. The justification is
    // the struct-layout invariant called out above.
    #[cfg_attr(all(target_arch = "x86_64", target_os = "none"), allow(dead_code))]
    host_tick_counter: AtomicU64,

    /// Host-only IPI accounting — incremented on every `send_ipi`
    /// with an in-range target. Borrowed from the caller's
    /// [`X86_64ArchStorage`]; bare-metal builds never touch it.
    #[cfg_attr(all(target_arch = "x86_64", target_os = "none"), allow(dead_code))]
    host_ipi_count: &'static [AtomicU64],

    /// Host-only stray-IPI counter for out-of-range targets.
    #[cfg_attr(all(target_arch = "x86_64", target_os = "none"), allow(dead_code))]
    host_stray_ipi: AtomicU64,

    /// Static [`CoreClass`] of each CPU, indexed by dense [`CpuId`].
    ///
    /// Initialised to [`CoreClass::Performance`] (a homogeneous machine).
    /// Each CPU records the class it read from CPUID as it comes online
    /// via [`Self::record_core_class`]; the boot CPU's entry is recorded
    /// in [`Self::new`]. The scheduler reads the table through the
    /// [`SchedulerArch::core_class`] override so it can place background
    /// work on efficiency cores (static per-CPU
    /// identity discovered by the arch port). Borrowed from the caller's
    /// [`X86_64ArchStorage`].
    core_classes: &'static [AtomicU8],
}

/// Whether a handle over `slots` naming `cpus` may stand beside the boot's,
/// whose published map is `boot`: the interrupt paths and the shootdown read
/// the boot's state with the later handle's dense ids, and rebuilding the
/// published map in place would shift it under their lookups.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
fn beside_boot(
    boot: ApicMap<'_>,
    slots: &[AtomicU64],
    cpus: impl IntoIterator<Item = (u32, u32)>,
) -> Result<(), ArchInitError> {
    if boot.is_backed_by(slots) {
        return Err(ArchInitError::StorageInUse);
    }
    if !boot.holds_exactly(cpus) {
        return Err(ArchInitError::MachineDiffers);
    }
    Ok(())
}

impl X86_64Arch {
    /// Build a validated arch handle.
    ///
    /// # Errors
    ///
    /// See [`ArchInitError`]. The constructor refuses to silently
    /// repair caller mistakes — fail closed.
    ///
    /// `storage` is the caller-owned, `&'static` per-CPU backing sized
    /// to the machine; the handle borrows its slices
    /// and imposes no compile-time CPU ceiling. `cpu_to_lapic` is the
    /// dense `CpuId` → LAPIC-ID map read from the ACPI MADT; entries
    /// beyond `storage`'s capacity `N` are ignored — the caller sizes
    /// `N` to its discovered processor count. An id may be any the APICs'
    /// mode names: x2APIC's 32 bits once the boot CPU runs in it.
    ///
    /// The first handle built is the boot's: on bare metal it publishes its
    /// reverse map for the interrupt paths' lookup and its per-CPU state for
    /// the TLB shootdown, before any other CPU starts. A later handle reads
    /// that state too, so it must name the boot's CPUs from storage of its
    /// own.
    pub fn new<const N: usize>(
        storage: &'static X86_64ArchStorage<N>,
        boot_cpu_id: CpuId,
        boot_cpu_lapic_id: u32,
        cpu_to_lapic: &[Option<u32>],
    ) -> Result<Self, ArchInitError> {
        let idx = usize::try_from(boot_cpu_id).map_err(|_| ArchInitError::BootCpuOutOfRange)?;
        if idx >= N {
            return Err(ArchInitError::BootCpuOutOfRange);
        }
        let recorded = cpu_to_lapic
            .get(idx)
            .copied()
            .flatten()
            .ok_or(ArchInitError::BootCpuMissingFromLapicMap)?;
        if recorded != boot_cpu_lapic_id {
            return Err(ArchInitError::BootCpuLapicMismatch);
        }
        let x2apic = crate::apic::x2apic();
        if cpu_to_lapic
            .iter()
            .flatten()
            .any(|&id| !crate::apic::names_cpu(id, x2apic))
        {
            return Err(ArchInitError::ApicIdUnsupported);
        }
        // An entry whose dense id exceeds capacity `N` is dropped from both
        // maps — fail closed, never index out of bounds.
        let held = cpu_to_lapic.iter().take(N);
        let cpus = || {
            held.clone()
                .enumerate()
                .filter_map(|(cpu, lapic)| Some((u32::try_from(cpu).ok()?, (*lapic)?)))
        };
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        if let Some(boot) = crate::cpumap::published() {
            beside_boot(boot, &storage.by_apic, cpus())?;
        }
        let apic_map =
            ApicMap::build(&storage.by_apic, cpus()).map_err(|refused| match refused {
                Unmappable::Duplicate => ArchInitError::ApicIdDuplicated,
                Unmappable::OutOfRange => ArchInitError::ApicIdUnsupported,
            })?;
        // Populate the caller's `&'static` map through the shared borrow
        // (atomic stores, so no `&'static mut` is needed).
        for (slot, dst) in held.zip(&storage.cpu_to_lapic) {
            if let Some(lapic) = slot {
                dst.store(*lapic, Ordering::Relaxed);
            }
        }
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            crate::cpumap::publish(apic_map);
            crate::tlb_shootdown::install(&storage.shootdown, &storage.cpu_to_lapic);
        }
        let this = Self {
            cpu_to_lapic: &storage.cpu_to_lapic,
            apic_map,
            boot_cpu_id,
            boot_cpu_lapic_id,
            host_tick_counter: AtomicU64::new(0),
            host_ipi_count: &storage.host_ipi_count,
            host_stray_ipi: AtomicU64::new(0),
            core_classes: &storage.core_classes,
        };
        // `new` runs on the boot processor, so CPUID here reflects the
        // boot core. Each application processor records its own class as
        // it comes online (`Self::record_core_class`).
        this.record_core_class(boot_cpu_id, hybrid::detect_current_core_class());
        Ok(this)
    }

    /// Record the [`CoreClass`] `cpu` detected for itself.
    ///
    /// Each CPU calls this once as it comes online, passing the value
    /// from [`crate::hybrid::detect_current_core_class`]. An out-of-range
    /// `cpu` is ignored — the table is bounded to the caller-provided
    /// storage length, so a stray call cannot corrupt memory
    /// (fail-closed).
    pub fn record_core_class(&self, cpu: CpuId, class: CoreClass) {
        if let Some(slot) = usize::try_from(cpu)
            .ok()
            .and_then(|idx| self.core_classes.get(idx))
        {
            slot.store(class.as_u8(), Ordering::Relaxed);
        }
    }

    /// Boot CPU's dense `CpuId`.
    #[must_use]
    pub const fn boot_cpu_id(&self) -> CpuId {
        self.boot_cpu_id
    }

    /// Boot CPU's LAPIC ID.
    #[must_use]
    pub const fn boot_cpu_lapic_id(&self) -> u32 {
        self.boot_cpu_lapic_id
    }

    /// LAPIC ID of `cpu`, or `None` for unallocated slots.
    #[must_use]
    pub fn lapic_id_of(&self, cpu: CpuId) -> Option<u32> {
        let idx = usize::try_from(cpu).ok()?;
        match self.cpu_to_lapic.get(idx)?.load(Ordering::Relaxed) {
            NO_LAPIC => None,
            raw => Some(raw),
        }
    }

    /// Drop `cpu`, which never came up, from the map: no shootdown waits on
    /// its acknowledge, and nothing starts it again.
    #[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
    fn forget_cpu(&self, cpu: CpuId) {
        if let Some(slot) = usize::try_from(cpu)
            .ok()
            .and_then(|idx| self.cpu_to_lapic.get(idx))
        {
            slot.store(NO_LAPIC, Ordering::Release);
        }
    }

    /// Host-test accessor: total IPIs dispatched to `target`.
    ///
    /// Only meaningful on host builds — on bare metal the counter is
    /// never written to (the real LAPIC absorbs the IPI). Exposed so
    /// the (c7-bin) follow-up's host-side smoke tests can assert
    /// scheduler-driven preemption requested the right CPU.
    #[must_use]
    #[cfg(any(test, not(target_os = "none")))]
    pub fn host_ipi_count(&self, target: CpuId) -> u64 {
        usize::try_from(target)
            .ok()
            .and_then(|idx| self.host_ipi_count.get(idx))
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
    }

    /// Host-test accessor: IPIs whose target was out of range.
    #[must_use]
    #[cfg(any(test, not(target_os = "none")))]
    pub fn host_stray_ipi_count(&self) -> u64 {
        self.host_stray_ipi.load(Ordering::Relaxed)
    }
}

// --- SchedulerArch ----------------------------------------------------

impl SchedulerArch for X86_64Arch {
    fn current_cpu(&self) -> CpuId {
        // Every CPU the handle starts is in its map, so only the boot CPU
        // before the handle existed could read as unmapped.
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            self.apic_map
                .cpu_of(crate::apic::local_apic_id())
                .unwrap_or(self.boot_cpu_id)
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            self.boot_cpu_id
        }
    }

    fn ticks_now(&self) -> u64 {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            crate::tsc::read_tsc()
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            // Per-instance counter so parallel `cargo test` runs
            // don't share state. `fetch_add(1, Relaxed) + 1` is
            // monotonically non-decreasing.
            self.host_tick_counter.fetch_add(1, Ordering::Relaxed) + 1
        }
    }

    fn quantum_ticks(&self) -> u64 {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            crate::preempt::quantum_tsc()
        }
        // No LAPIC off the freestanding target, so no quantum is calibrated.
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            0
        }
    }

    fn core_class(&self, cpu: CpuId) -> CoreClass {
        // Out-of-range CPUs report the safe homogeneous default per the
        // Arch HAL contract; a stored byte is always a valid encoding
        // because `record_core_class` only writes `CoreClass::as_u8`.
        usize::try_from(cpu)
            .ok()
            .and_then(|idx| self.core_classes.get(idx))
            .map_or(CoreClass::Performance, |slot| {
                CoreClass::from_u8(slot.load(Ordering::Relaxed)).unwrap_or(CoreClass::Performance)
            })
    }

    fn send_ipi(&self, target: CpuId) {
        // Resolve the destination LAPIC ID first. Out-of-range or
        // unallocated targets are dropped rather than panicking — the
        // architecture-neutral contract documents `send_ipi` as
        // best-effort, and stray IPIs are recorded for tests on the
        // host path only.
        let Some(target_apic_id) = self.lapic_id_of(target) else {
            #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
            self.host_stray_ipi.fetch_add(1, Ordering::Relaxed);
            return;
        };

        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // Each CPU reaches its own local APIC, so senders on different
            // CPUs touch independent registers. Within a CPU the call is not
            // re-entrant: the kernel calls it from process context only. The
            // one exception is the fatal-report stop-the-world, which pokes
            // peers from exception context; an in-flight command it clobbers
            // belongs to a mainline never resumed, as the report halts this
            // CPU.
            let mut lapic = crate::apic::Lapic::new(crate::apic::LocalApic);
            lapic.send_ipi(
                target_apic_id,
                crate::apic::DeliveryMode::Fixed,
                crate::preempt::TIMER_VECTOR,
            );
        }

        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            // Host: count the IPI; ignore the resolved APIC ID.
            let _ = target_apic_id;
            // `lapic_id_of` already confirmed `target` maps to a CPU, so
            // the counter slot exists; an absent slot is dropped rather
            // than panicking. Bound by the borrowed
            // slice length, never a fixed ceiling.
            if let Some(counter) = usize::try_from(target)
                .ok()
                .and_then(|idx| self.host_ipi_count.get(idx))
            {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn set_preemption(&self, armed: bool) {
        // Tickless preemption: `armed` records the
        // running task's quantum deadline (the current TSC plus one quantum
        // in TSC ticks, the single stored copy); `!armed` clears it.
        // The deadline combiner then programs the single LAPIC-timer
        // one-shot to the *earlier* of this quantum and any pending
        // blocking-wait wakeup ([`Self::set_wakeup`]), so neither suppresses
        // the other. Off the freestanding target there is no LAPIC, so the
        // arming is inert.
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            let deadline = if armed {
                Some(
                    self.ticks_now()
                        .wrapping_add(crate::preempt::quantum_tsc().max(1)),
                )
            } else {
                None
            };
            crate::preempt::record_quantum_deadline(deadline);
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            let _ = armed;
        }
    }

    fn set_wakeup(&self, deadline_ns: Option<u64>) {
        // The timed half of the tickless one-shot: a
        // blocking wait with a finite timeout records its soonest waiter
        // deadline here so the parked waiter is woken on time even when the
        // CPU has no runnable task to preempt. Convert the absolute
        // monotonic-ns deadline to an absolute TSC tick against the
        // calibrated TSC rate (the same rate `monotonic_ns` reads ns *out*
        // of), then record it; the combiner rebases the chosen TSC
        // duration onto the LAPIC clock to arm the one-shot. Off the
        // freestanding target the arming is inert.
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            let deadline = deadline_ns
                .map(|ns| tairix_arch_api::wakeup::ns_to_ticks(ns, crate::preempt::tsc_hz()));
            crate::preempt::record_wakeup_deadline(deadline);
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            let _ = deadline_ns;
        }
    }
}

impl CrossCpuTlbShootdown for X86_64Arch {
    fn shootdown_page(&self, vaddr: u64) {
        self.shootdown_range(vaddr, 1);
    }

    fn shootdown_range(&self, start_vaddr: u64, page_count: usize) {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // Stream every mapped CPU straight to `shootdown`, which excludes
            // this CPU and invalidates it directly. The iterator walks the
            // caller-sized per-CPU map, so there is no fixed `MAX_CPUS`
            // scratch buffer and no allocation.
            let targets = (0..self.cpu_to_lapic.len()).filter_map(move |idx| {
                let cpu = CpuId::try_from(idx).ok()?;
                self.lapic_id_of(cpu).map(|_| cpu)
            });
            // One IPI round-trip covers the whole range: `invlpg` is
            // per-page but the acknowledge protocol is what costs, so a
            // large teardown must not pay it once per leaf.
            crate::tlb_shootdown::shootdown(start_vaddr, page_count, targets);
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            // Host: no second CPU and no TLB; the conformance vertical
            // only checks the call is total and panic-free.
            let _ = (start_vaddr, page_count);
        }
    }

    fn shootdown_user_range(&self, cpus: CpuMask<'_>, start_vaddr: u64, page_count: usize) {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            match cpus.reach(|cpu| self.lapic_id_of(cpu).map(|_| cpu)) {
                Some(targets) => {
                    crate::tlb_shootdown::shootdown_remote(start_vaddr, page_count, targets);
                }
                None => self.shootdown_range(start_vaddr, page_count),
            }
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            let _ = (cpus, start_vaddr, page_count);
        }
    }
}

impl SecondaryBringup for X86_64Arch {
    unsafe fn start_secondary(&self, cpu: CpuId) -> Result<(), SmpError> {
        // Fail closed before any hardware action: the boot CPU is
        // already running, and an unmapped dense id has no LAPIC to
        // target.
        if cpu == self.boot_cpu_id {
            return Err(SmpError::InvalidCpu);
        }
        let Some(target_apic_id) = self.lapic_id_of(cpu) else {
            return Err(SmpError::InvalidCpu);
        };

        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // SAFETY: the caller of this HAL method guarantees `.bss` is
            // zeroed (clear AP stack pool), the boot CPU's LAPIC is
            // software-enabled, the secondary entry is installed, and
            // `target_apic_id` names a real, parked AP distinct from the
            // caller — exactly `crate::smp::start_secondary`'s contract.
            match unsafe { crate::smp::start_secondary(target_apic_id, cpu) } {
                Ok(()) => Ok(()),
                Err(crate::smp::StartCpuError::CpuIdOutOfRange) => Err(SmpError::InvalidCpu),
                Err(crate::smp::StartCpuError::NoEntryInstalled) => Err(SmpError::NotReady),
                Err(crate::smp::StartCpuError::StartTimedOut) => {
                    self.forget_cpu(cpu);
                    Err(SmpError::StartRejected(0))
                }
            }
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            // Host: there is no INIT-SIPI-SIPI hardware. Mirror the
            // bare-metal precondition so the observable contract holds —
            // refuse when no secondary entry is installed, otherwise
            // report the (range-checked) request as accepted. The real
            // handshake is proven by the QEMU verticals.
            let _ = target_apic_id;
            if crate::smp::secondary_entry_addr() == 0 {
                return Err(SmpError::NotReady);
            }
            Ok(())
        }
    }
}

// --- Halt -------------------------------------------------------------

/// Mask interrupts and park the CPU forever.
///
/// The bin crate's `KernelArch::halt` impl forwards here. Kept as a
/// free function (not a method) so the bin crate can call it without
/// owning an [`X86_64Arch`] (e.g. from the panic handler before
/// `BootInfo` has been constructed).
///
/// # SAFETY-INVARIANT
///
/// This function never returns. The `!` return type encodes the
/// invariant at the type level.
pub fn halt() -> ! {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    {
        crate::reset::park_cpu()
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        // Host fallback: spin-wait forever. Host tests never invoke
        // this function — the compile-time `const _` assertion in the
        // test module proves the `-> !` signature without calling it
        // (no flaky tests, no host-side blocking).
        loop {
            core::hint::spin_loop();
        }
    }
}

// --- Tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Each test owns a distinct function-local `static` backing — the
    // same allocator-free `&'static`-storage pattern the bare-metal
    // verticals use — so no two handles alias one
    // another's per-CPU bookkeeping under the parallel test runner
    // (no flaky shared state). Each test constructs
    // exactly one handle, so a single local `static` per test suffices.

    #[test]
    fn new_accepts_consistent_mapping() {
        static S: X86_64ArchStorage<3> = X86_64ArchStorage::new();
        let arch =
            X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).expect("valid construction");
        assert_eq!(arch.boot_cpu_id(), 0);
        assert_eq!(arch.boot_cpu_lapic_id(), 0xA0);
        assert_eq!(arch.lapic_id_of(0), Some(0xA0));
        assert_eq!(arch.lapic_id_of(1), Some(0xA1));
        assert_eq!(arch.lapic_id_of(2), None);
    }

    /// A handle built after the boot's names the boot's CPUs from storage of
    /// its own, or is refused.
    #[test]
    fn a_later_handle_names_the_boot_s_machine_from_its_own_storage() {
        static BOOT: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        static LATER: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let boot = X86_64Arch::new(&BOOT, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        let same = [(0, 0xA0), (1, 0xA1)];
        assert_eq!(beside_boot(boot.apic_map, &LATER.by_apic, same), Ok(()));
        assert_eq!(
            beside_boot(boot.apic_map, &BOOT.by_apic, same),
            Err(ArchInitError::StorageInUse)
        );
        for other in [
            &[(0, 0xA0)][..],
            &[(0, 0xA0), (1, 0xA2)],
            &[(1, 0xA0), (0, 0xA1)],
        ] {
            assert_eq!(
                beside_boot(boot.apic_map, &LATER.by_apic, other.iter().copied()),
                Err(ArchInitError::MachineDiffers),
                "{other:?}"
            );
        }
    }

    #[test]
    fn new_rejects_boot_cpu_out_of_range() {
        // Boot CPU id equal to the storage capacity is one past the last
        // valid slot — fail closed, no fixed `MAX_CPUS`.
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let err = X86_64Arch::new(&S, 2, 0, &[Some(0)]).unwrap_err();
        assert_eq!(err, ArchInitError::BootCpuOutOfRange);
        assert_eq!(err.as_str(), "boot_cpu_out_of_range");
    }

    #[test]
    fn new_rejects_missing_boot_cpu_slot() {
        static S: X86_64ArchStorage<4> = X86_64ArchStorage::new();
        let err = X86_64Arch::new(&S, 2, 0, &[Some(0)]).unwrap_err();
        assert_eq!(err, ArchInitError::BootCpuMissingFromLapicMap);
        assert_eq!(err.as_str(), "boot_cpu_missing_from_lapic_map");
    }

    #[test]
    fn new_rejects_lapic_id_mismatch() {
        static S: X86_64ArchStorage<1> = X86_64ArchStorage::new();
        let err = X86_64Arch::new(&S, 0, 0xAA, &[Some(0xBB)]).unwrap_err();
        assert_eq!(err, ArchInitError::BootCpuLapicMismatch);
        assert_eq!(err.as_str(), "boot_cpu_lapic_mismatch");
    }

    #[test]
    fn current_cpu_on_host_returns_boot_cpu_id() {
        static S: X86_64ArchStorage<4> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 3, 0xC3, &[None, None, None, Some(0xC3)]).unwrap();
        assert_eq!(arch.current_cpu(), 3);
    }

    #[test]
    fn ticks_now_is_monotonic_on_host() {
        static S: X86_64ArchStorage<1> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0, &[Some(0)]).unwrap();
        let a = arch.ticks_now();
        let b = arch.ticks_now();
        let c = arch.ticks_now();
        assert!(b > a, "expected b > a, got a={a} b={b}");
        assert!(c > b, "expected c > b, got b={b} c={c}");
    }

    #[test]
    fn send_ipi_records_in_range_target_on_host() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        arch.send_ipi(1);
        arch.send_ipi(1);
        arch.send_ipi(0);
        assert_eq!(arch.host_ipi_count(1), 2);
        assert_eq!(arch.host_ipi_count(0), 1);
        assert_eq!(arch.host_stray_ipi_count(), 0);
    }

    #[test]
    fn send_ipi_drops_unmapped_target_into_stray_counter() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0, &[Some(0)]).unwrap();
        // CPU 5 is out of range for this 2-slot storage — `lapic_id_of`
        // returns None and the stray counter ticks.
        arch.send_ipi(5);
        // CPU u32::MAX is out of range — `usize::try_from` succeeds
        // (u32 → usize) on 64-bit hosts but the index is OOB, so
        // `lapic_id_of` returns None and the stray counter ticks.
        arch.send_ipi(u32::MAX);
        assert_eq!(arch.host_stray_ipi_count(), 2);
        assert_eq!(arch.host_ipi_count(5), 0);
    }

    #[test]
    fn core_class_defaults_to_performance_then_tracks_recorded_class() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        // Every CPU starts as a performance core (homogeneous default);
        // on the host the boot-CPU detection also yields Performance.
        assert_eq!(arch.core_class(0), CoreClass::Performance);
        assert_eq!(arch.core_class(1), CoreClass::Performance);
        // Recording an efficiency core is reflected by the override; the
        // scheduler reads exactly this through `SchedulerArch`.
        arch.record_core_class(1, CoreClass::Efficiency);
        assert_eq!(arch.core_class(1), CoreClass::Efficiency);
        assert_eq!(arch.core_class(0), CoreClass::Performance);
        // Out-of-range CPUs report the safe default and do not panic.
        assert_eq!(arch.core_class(u32::MAX), CoreClass::Performance);
        arch.record_core_class(u32::MAX, CoreClass::Efficiency); // no-op
        assert_eq!(arch.core_class(u32::MAX), CoreClass::Performance);
    }

    /// / W0: the port passes the shared Arch HAL conformance
    /// vertical over its real `SchedulerArch`, `SideChannel`, and
    /// `MemoryTags` handles (`plans/WIRING.md` Stage W0).
    #[test]
    fn passes_arch_hal_conformance_suite() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        // One enabled Local APIC + one I/O APIC, the minimal MADT the
        // discovery suite needs. The entry bytes are a
        // fixed array (LocalApic 8 bytes + IoApic 12 bytes) so the test
        // names no allocator type.
        let entries: [u8; 20] = [
            0, 8, 0, 0, 1, 0, 0, 0, // LocalApic: uid 0, apic 0, enabled
            1, 12, 2, 0, 0x00, 0x00, 0xC0, 0xFE, 0, 0, 0, 0, // IoApic @0xFEC00000
        ];
        let madt = crate::acpi::tests::build_madt(0xFEE0_0000, 0x1, &entries);
        let discovery = crate::platform::AcpiDiscovery::new(&madt);
        tairix_arch_api::conformance::run_all(
            &arch,
            &crate::sidechannel::SideChannel::new(),
            &crate::memtag::MemoryTags::new(),
            &discovery,
            &crate::percpu_hal::PerCpuStorage::new(),
            &crate::cpufeatures::CpuFeatureDetect::new(),
        );
        // The cycle-counter slice has its own vertical (it takes no
        // scheduler/discovery handle).
        tairix_arch_api::cpucycles::conformance::run_all(
            &crate::cpufeatures::CpuCycleCounter::new(),
        );
    }

    /// / W6: the port passes the cross-CPU TLB-shootdown
    /// conformance vertical over its real `X86_64Arch` handle. On the
    /// host there is no second CPU and no TLB, so the vertical asserts
    /// the observable half — the call is total and panic-free for any
    /// address. The real IPI + acknowledge round-trip is proven by
    /// `cross_cpu_tlb_shootdown_qemu_x86_64`.
    #[test]
    fn passes_cross_cpu_tlb_shootdown_conformance() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        tairix_arch_api::xtlb::conformance::run_all(&arch, 0x10_0000_0000);
        let erased: &dyn CrossCpuTlbShootdown = &arch;
        tairix_arch_api::xtlb::conformance::run_all(erased, 0x10_0000_0000);
    }

    /// / W14: the port passes the secondary-bring-up conformance
    /// vertical over its real `X86_64Arch` handle. On the host there is
    /// no INIT-SIPI-SIPI hardware, so the vertical asserts the observable
    /// half — starting an unstartable id fails closed and never panics.
    /// The real handshake is proven by the multi-core QEMU verticals
    /// (`scheduler_stress_qemu`, `ipi_smp_qemu_x86_64`,
    /// `cross_cpu_tlb_shootdown_qemu_x86_64`).
    #[test]
    fn passes_secondary_bringup_conformance() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        tairix_arch_api::smp::conformance::run_all(&arch, CpuId::MAX);
        let erased: &dyn SecondaryBringup = &arch;
        tairix_arch_api::smp::conformance::run_all(erased, CpuId::MAX);
    }

    /// In xAPIC mode, which the host's APICs read as, a CPU map naming an
    /// id xAPIC cannot address is refused whole; x2APIC ids are
    /// `names_cpu`'s, tested beside it.
    #[test]
    fn an_apic_id_xapic_cannot_name_is_refused() {
        static STORAGE: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        assert_eq!(
            X86_64Arch::new(&STORAGE, 0, 0, &[Some(0), Some(0x100)]).err(),
            Some(ArchInitError::ApicIdUnsupported)
        );
        assert_eq!(
            X86_64Arch::new(&STORAGE, 0, 0xFF, &[Some(0xFF)]).err(),
            Some(ArchInitError::ApicIdUnsupported),
            "the broadcast id is no CPU's"
        );
    }

    /// Two CPUs claiming one APIC id leave the reverse map ambiguous, so the
    /// map is refused whole.
    #[test]
    fn two_cpus_claiming_one_apic_id_are_refused() {
        static STORAGE: X86_64ArchStorage<3> = X86_64ArchStorage::new();
        assert_eq!(
            X86_64Arch::new(&STORAGE, 0, 4, &[Some(4), Some(9), Some(4)]).err(),
            Some(ArchInitError::ApicIdDuplicated)
        );
    }

    /// The handle maps every CPU of its storage back from its APIC id, and
    /// a CPU past the storage's capacity from neither side.
    #[test]
    fn every_held_cpu_is_mapped_back_from_its_apic_id() {
        static STORAGE: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&STORAGE, 0, 0x30, &[Some(0x30), Some(0x07), Some(0x55)])
            .expect("a valid map");
        assert_eq!(arch.apic_map.cpu_of(0x30), Some(0));
        assert_eq!(arch.apic_map.cpu_of(0x07), Some(1));
        assert_eq!(arch.apic_map.cpu_of(0x55), None, "past the storage");
        assert_eq!(arch.lapic_id_of(2), None);
    }

    /// The boot CPU and any unmapped dense id are refused before any
    /// INIT-SIPI-SIPI action. The set-once secondary-entry slot is shared
    /// with `crate::smp`'s tests, so the accepted path is exercised there.
    #[test]
    fn start_secondary_rejects_boot_and_unmapped_ids() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        // SAFETY: every call below is refused before any hardware
        // action, so the test takes no platform action and touches no
        // shared global state.
        unsafe {
            // Boot CPU: already running.
            assert_eq!(arch.start_secondary(0), Err(SmpError::InvalidCpu));
            // Unmapped dense id.
            assert_eq!(arch.start_secondary(5), Err(SmpError::InvalidCpu));
            assert_eq!(arch.start_secondary(u32::MAX), Err(SmpError::InvalidCpu));
        }
    }

    /// A CPU whose start timed out is forgotten: it is no shootdown's target
    /// and is never started again.
    #[test]
    fn a_cpu_that_never_came_up_is_forgotten() {
        static S: X86_64ArchStorage<2> = X86_64ArchStorage::new();
        let arch = X86_64Arch::new(&S, 0, 0xA0, &[Some(0xA0), Some(0xA1)]).unwrap();
        arch.forget_cpu(1);
        assert_eq!(arch.lapic_id_of(1), None);
        assert_eq!(arch.lapic_id_of(0), Some(0xA0));
        // SAFETY: refused before any hardware action.
        assert_eq!(
            unsafe { arch.start_secondary(1) },
            Err(SmpError::InvalidCpu)
        );
    }

    /// Compile-time proof that [`halt`] has the `-> !` signature
    /// required by `KernelArch::halt`. Calling it would block the
    /// test runner; coercing the function pointer is enough to
    /// surface a mismatched return type at build time
    /// (encode the invariant in the type system).
    const _HALT_RETURNS_NEVER: fn() -> ! = halt;

    /// Compile-time proof that `X86_64Arch` implements
    /// [`SchedulerArch`]. If the impl ever regresses, this line
    /// fails to type-check.
    const _IS_SCHED_ARCH: fn(&X86_64Arch) -> u32 = <X86_64Arch as SchedulerArch>::current_cpu;
}
