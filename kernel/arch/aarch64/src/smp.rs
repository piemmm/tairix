//! Multi-core (SMP) bring-up primitives for the aarch64 `virt` board.
//!
//! This module owns the architecture side of starting a secondary core
//! and recovering a running core's identity, mirroring riscv64's
//! [`crate::smp`] (the parity reference, `plans/WIRING.md` Stage W6):
//!
//! * `install_current_cpu_index` publishes the firmware-validated dense
//!   [`CpuId`](tairix_arch_api::CpuId) into the calling CPU's Arch HAL per-CPU word (`TPIDR_EL1`),
//!   and `current_cpu_index` reads it. IRQ, timer, scheduler-current, and
//!   continuation paths therefore index discovered-sized tables with dense
//!   identity rather than assuming a sparse/clustered `MPIDR_EL1` affinity is
//!   an array index. The `CpuId`↔`MPIDR` map itself lives in
//!   [`crate::kernel_arch::Aarch64Arch`].
//! * `set_secondary_entry` installs the set-once `extern "C"
//!   fn(CpuId) -> !` a freshly-started secondary core runs, and
//!   `start_secondary` asks PSCI to power on a parked core at the
//!   `smp.s` trampoline, which sets up that core's stack before invoking
//!   the installed entry. Storing a `fn` (not a closure) keeps the
//!   hand-off lock-free and free of a captured environment, exactly as
//!   [`crate::preempt`] does for the timer callback.
//!
//! # Why a set-once callback rather than an `extern` symbol
//!
//! The secondary trampoline must call *something* Rust-side, but binding
//! a mandatory `extern "C" fn secondary_main` would force every consumer
//! that links this crate (including the single-core boot pipeline and the
//! freestanding test bins) to define that symbol or fail to link. A
//! set-once callback (parking until installed) keeps secondary bring-up
//! opt-in without a Cargo feature gate (no hacks).
//!
//! # Bring-up methods
//!
//! The board's device tree declares how a parked secondary is started,
//! and the port implements both declared mechanisms:
//!
//! * **PSCI `CPU_ON`** (`start_secondary`) — the QEMU `virt` board and
//!   every PSCI-firmware platform — with the conduit (`hvc`/`smc`)
//!   discovered from the `/psci` node ([`crate::fdt`]). The firmware
//!   enters the started core at the `smp.s` PSCI trampoline with the
//!   dense id in `x0`.
//! * **Devicetree spin-table** (`start_secondary_spintable`) — the
//!   Raspberry Pi 4's stock firmware, whose `cpu@*` nodes declare
//!   `enable-method = "spin-table"` and a per-core `cpu-release-addr`.
//!   Releasing a core writes the spin-table trampoline's physical
//!   address to **both** release channels — the firmware's declared
//!   release word (for a core parked in the firmware stub) and the
//!   kernel's own `SECONDARY_KERNEL_RELEASE` word (for a core the
//!   firmware released straight into `boot.s`'s `_start`, which parks
//!   it polling that word) — publishes the released core's affinity in
//!   `SECONDARY_KERNEL_RELEASE_TARGET`, sweeps all three to the point of
//!   coherency, and signals `sev`. The kernel word is *shared*, so one
//!   `sev` wakes every core parked in `_start`; the target affinity is
//!   the gate that lets only the one core being released proceed while
//!   the rest re-park, so secondaries are brought up strictly one at a
//!   time (never a concurrent MMU-adopt / GIC-init race). A spin-table
//!   release carries no context register, so the released core recovers
//!   its dense id by matching its own `MPIDR_EL1` affinity against the
//!   table published by `register_secondary_affinities`; entry may be at
//!   EL2 (the Pi firmware hand-off), which the trampoline drops exactly
//!   as `_start` does (the shared `_el2_establish_and_drop` routine).
//!
//! A tree that declares neither mechanism leaves the handle without a
//! start method and every start fails closed rather than guessing.
//!
//! Either way the boot CPU then waits, bounded, for the started core's
//! answer ([`tairix_arch_api::StartAnswer`]): one that cannot hold the
//! protection in force says so and stops, rather than being read as a core
//! that never came up. The boot CPU gives its verdict back
//! ([`tairix_arch_api::StartVerdict`]), and a core runs only on a verdict
//! that took its join: one answering a start given up on stops. Each core
//! answers in its own slot of the stack pool, once its trampoline has turned
//! translation and its caches on.
//!
//! # Host testability
//!
//! `SecondaryStackPool`, `is_valid_cpu`, the callback slot, and the
//! `StartCpuError` decode build and are unit-tested on the host. The
//! `MPIDR_EL1` read, the PSCI call, and the secondary trampoline are
//! gated to the freestanding aarch64 target.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
use tairix_arch_api::StartAnswer;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
use tairix_arch_api::StartVerdict;
use tairix_arch_api::{CpuId, PerCpu, SmpError};

use crate::percpu_hal::PerCpuStorage;
use tairix_sync::FnCell;

/// Per-secondary-core kernel stack size, in bytes (64 KiB).
///
/// This is a fixed *per-stack bound* (like the kthread kernel stack), not
/// a CPU-count capacity, so it is a constant and not subject to the
/// "no fixed ceiling" rule — that rule governs the *number* of cores, not
/// the size of one core's stack. The number of cores
/// the pool covers is the caller-sized `N` of [`SecondaryStackPool`].
pub const SECONDARY_STACK_BYTES: usize = 1 << 16;

/// Pool base address published to the `smp.s` trampoline (`0` until a
/// [`SecondaryStackPool`] is registered). Read by the trampoline by symbol
/// once translation is on; written once by [`SecondaryStackPool::register`].
///
/// The exported name is port-qualified like this crate's other exported
/// symbols: the riscv64 port publishes the same pool base to its own
/// trampoline, so an unmangled `SECONDARY_STACK_BASE` collides in any
/// build that links both ports.
#[export_name = "tairix_arch_aarch64_secondary_stack_base"]
#[used]
static SECONDARY_STACK_BASE: AtomicPtr<SecondaryStack> = AtomicPtr::new(core::ptr::null_mut());

/// Per-core slice stride (bytes) published to the `smp.s` trampoline
/// (`0` until a pool is registered). Read by the secondary stub by
/// symbol; written once by [`SecondaryStackPool::register`].
#[no_mangle]
#[used]
static SECONDARY_STACK_STRIDE: AtomicU64 = AtomicU64::new(0);

/// Number of logical CPUs the registered pool covers (`0` until a pool
/// is registered, so an unstarted system fails closed — every id is
/// invalid).
static SECONDARY_STACK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Set-once guard so a second [`SecondaryStackPool::register`] is refused
/// rather than silently re-pointing the trampoline at a different pool.
static SECONDARY_STACKS_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Failure mode of [`SecondaryStackPool::register`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SecondaryStackError {
    /// A pool was already registered; the slot is set-once per boot
    /// (no silent re-pointing of the live trampoline).
    AlreadyRegistered,
}

/// One secondary core's slot: its answer to the CPU that starts it, the
/// verdict it waits on, and its [`SECONDARY_STACK_BYTES`] kernel stack,
/// growing down from the slot's end. Aligned so that end, the stack top the
/// trampoline computes, is a valid stack pointer.
///
/// Public so an allocator-having boot path can size a **runtime** pool
/// to the discovered core count (a zeroed, leaked `[SecondaryStack]`
/// registered through [`register_secondary_stacks`]) instead of a
/// compile-time `SecondaryStackPool<N>` — the per-core stack count is
/// derived from discovery, never a baked-in ceiling.
#[repr(C, align(16))]
pub struct SecondaryStack {
    /// What the core answers ([`tairix_arch_api::StartAnswer::word`]).
    answer: AtomicU64,
    /// What the starting CPU makes of it ([`tairix_arch_api::StartVerdict`]).
    verdict: AtomicU64,
    /// The core's live stack, written through its stack pointer while the
    /// slot is shared.
    bytes: UnsafeCell<[u8; SECONDARY_STACK_BYTES]>,
}

// SAFETY: the answer and verdict are atomics, and the stack bytes are reached
// only by the core whose stack they are, through the stack pointer the
// trampoline derives from the published base, never through a reference to
// the slot. Sharing a slot between CPUs therefore races on nothing.
unsafe impl Sync for SecondaryStack {}

impl SecondaryStack {
    // A 64 KiB zero array is a deliberately large backing (a secondary
    // core's whole kernel stack), const-evaluated into a `static`
    // `SecondaryStackPool` or written directly into a zeroed heap
    // allocation — never materialised on a runtime stack — so the
    // large-array lint does not apply (a per-stack size is a fixed
    // bound, not a runtime stack allocation).
    #[allow(clippy::large_stack_arrays)]
    const fn new() -> Self {
        Self {
            answer: AtomicU64::new(0),
            verdict: AtomicU64::new(0),
            bytes: UnsafeCell::new([0u8; SECONDARY_STACK_BYTES]),
        }
    }
}

/// Caller-owned, `&'static` secondary-core stack pool, sized by the
/// constructing caller for its machine (the
/// secondary-bring-up stack count is derived from the-discovered core
/// count, never a fixed `const` ceiling baked into the arch crate).
///
/// The const parameter `N` is the number of logical CPUs the caller
/// intends to bring up: a single-CPU boot path needs no pool, a two-core
/// vertical uses `SecondaryStackPool<2>`, and a multi-core boot path sizes
/// `N` from the device-tree CPU count. The arch crate stays allocator-free
/// (watch-out — no `alloc` in a bare-metal arch crate,
/// which would force a heap into every freestanding bin that links it), so
/// the caller provides the storage as a `static` (allocator-free bins) or
/// a leaked allocation (allocator-having callers) and registers it through
/// [`SecondaryStackPool::register`].
///
/// Each secondary core `c` runs on `c`'s slot; the `smp.s` trampoline
/// computes the slot's end from the registered base and stride, so the pool
/// memory is reached only by the freshly-started core that owns it and, for
/// its answer and verdict, the CPU that starts it.
#[repr(C)]
pub struct SecondaryStackPool<const N: usize> {
    stacks: [SecondaryStack; N],
}

impl<const N: usize> SecondaryStackPool<N> {
    /// A zeroed pool of `N` per-core stacks. `const` so the
    /// allocator-free bins can place it in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stacks: [const { SecondaryStack::new() }; N],
        }
    }

    /// Publish this pool to the secondary-core trampoline and the
    /// [`is_valid_cpu`] bound, then return the covered CPU count `N`.
    ///
    /// Must be called on the boot core, exactly once, before any
    /// `start_secondary`; the `&'static` receiver keeps the pool alive for
    /// the kernel's lifetime.
    ///
    /// # Errors
    ///
    /// [`SecondaryStackError::AlreadyRegistered`] on the second publish
    /// (set-once per boot — never silently re-points the live trampoline).
    pub fn register(&'static self) -> Result<usize, SecondaryStackError> {
        register_secondary_stacks(&self.stacks)
    }
}

/// Publish `stacks` (one [`SecondaryStack`] slot per dense CPU id, slot
/// 0 belonging to the never-started boot CPU) to the secondary-core
/// trampoline and the [`is_valid_cpu`] bound, returning the covered CPU
/// count.
///
/// This is the runtime-sized registration path: an allocator-having
/// boot path leaks a zeroed `[SecondaryStack]` sized to the discovered
/// core count and registers it here; the `const`-generic
/// [`SecondaryStackPool::register`] delegates to the same set-once
/// body. Must be called on the boot core, exactly once, before any
/// `start_secondary`; the `&'static` slice lives for the kernel's lifetime.
/// A started core reads the globals once its MMU and caches are on, so it
/// sees the boot CPU's writes coherently.
///
/// # Errors
///
/// [`SecondaryStackError::AlreadyRegistered`] on the second publish
/// (set-once per boot — never silently re-points the live trampoline).
pub fn register_secondary_stacks(
    stacks: &'static [SecondaryStack],
) -> Result<usize, SecondaryStackError> {
    if SECONDARY_STACKS_REGISTERED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(SecondaryStackError::AlreadyRegistered);
    }
    SECONDARY_STACK_BASE.store(stacks.as_ptr().cast_mut(), Ordering::Release);
    SECONDARY_STACK_STRIDE.store(
        core::mem::size_of::<SecondaryStack>() as u64,
        Ordering::Release,
    );
    SECONDARY_STACK_COUNT.store(stacks.len(), Ordering::Release);
    Ok(stacks.len())
}

/// `cpu`'s slot in the registered pool, or `None` for an id outside it.
#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
fn slot(cpu: CpuId) -> Option<&'static SecondaryStack> {
    if !is_valid_cpu(cpu) {
        return None;
    }
    let base = SECONDARY_STACK_BASE.load(Ordering::Acquire);
    // SAFETY: `register_secondary_stacks` published `base`, the start of a
    // `&'static [SecondaryStack]`, ahead of its length. `cpu` is below that
    // length, so the slot is an element of a slice that lives as long as the
    // kernel, and its live stack is an `UnsafeCell` a shared reference may
    // span.
    Some(unsafe { &*base.add(cpu as usize) })
}

impl<const N: usize> Default for SecondaryStackPool<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// `true` iff `cpu` indexes a slot inside the registered secondary-stack
/// pool, so the `smp.s` trampoline selects a stack slice that lies inside
/// it. Returns `false` for every id until a [`SecondaryStackPool`] is
/// registered (fail closed).
#[must_use]
pub fn is_valid_cpu(cpu: CpuId) -> bool {
    (cpu as usize) < SECONDARY_STACK_COUNT.load(Ordering::Acquire)
}

#[cfg(test)]
fn reset_secondary_stacks_for_tests() {
    SECONDARY_STACKS_REGISTERED.store(false, Ordering::Release);
    SECONDARY_STACK_COUNT.store(0, Ordering::Release);
    SECONDARY_STACK_BASE.store(core::ptr::null_mut(), Ordering::Release);
    SECONDARY_STACK_STRIDE.store(0, Ordering::Release);
}

/// The secondary-core entry the trampoline runs, read without a lock.
/// Empty until [`set_secondary_entry`] installs it.
static SECONDARY_ENTRY_FN: FnCell<extern "C" fn(CpuId) -> !> = FnCell::empty();

/// Base address of the published dense-id → `MPIDR_EL1`-affinity table
/// (`0` until [`register_secondary_affinities`] publishes it). Read by
/// symbol by the `smp.s` spin-table trampoline once it is in the high half:
/// a released core carries no context register, so it linearly matches its
/// own affinity against this table to recover its dense [`CpuId`] (entry
/// `i` is dense id `i`'s affinity).
#[no_mangle]
#[used]
static SECONDARY_AFFINITY_BASE: AtomicU64 = AtomicU64::new(0);

/// Number of entries in the published affinity table (`0` until
/// published). Read by symbol by the `smp.s` spin-table trampoline; an
/// affinity not found within this bound parks the core (fail closed).
#[no_mangle]
#[used]
static SECONDARY_AFFINITY_COUNT: AtomicU64 = AtomicU64::new(0);

/// The kernel's own spin-table release word, polled MMU-off by the
/// `wfe` park loop in `boot.s`'s `_start` (the park every non-boot core
/// enters when firmware releases all cores straight to the kernel).
/// `0` parks; a non-zero value is the physical entry address a *released*
/// core branches to. Written only by `start_secondary_spintable`.
///
/// In `.data`, not `.bss`: a parked core reads it from the moment the
/// firmware loads the image, long before the boot CPU zeroes `.bss`, so it
/// must hold its initial value in the image itself.
///
/// This word gates *whether* a release is in progress; it does not name
/// *which* core — [`SECONDARY_KERNEL_RELEASE_TARGET`] does. Both cores
/// parked here poll both words: a woken core branches only when the
/// release word is non-zero **and** the target affinity is its own.
#[no_mangle]
#[used]
#[cfg_attr(
    all(target_arch = "aarch64", target_os = "none"),
    link_section = ".data.secondary_release"
)]
static SECONDARY_KERNEL_RELEASE: AtomicU64 = AtomicU64::new(0);

/// The `MPIDR_EL1` affinity (`Aff0`–`Aff2`, [`MPIDR_AFFINITY_MASK`]) of
/// the single secondary the boot CPU is releasing *right now*, polled
/// MMU-off alongside [`SECONDARY_KERNEL_RELEASE`] by the `boot.s` park
/// loop. Written only by `start_secondary_spintable`, immediately before
/// the release word, and swept to the point of coherency before the
/// `sev`.
///
/// This is what serialises secondary bring-up to one core at a time, and
/// it gates **both** release channels because both converge on the same
/// entry: the `boot.s` `_start` park loop (a core the firmware released
/// straight into the kernel) and the `smp.s`
/// `_start_secondary_spintable_aarch64` trampoline (a core the firmware
/// released from its own spin table). Each polls this word MMU-off and
/// proceeds only when it equals the core's own masked affinity; the rest
/// re-park. The gate lives at the trampoline too because a firmware
/// spin-table release must **not** be trusted to wake only one core — a
/// shared/aliased `cpu-release-addr`, or one `sev` waking every parked
/// core, can deliver several cores into the trampoline at once. Without
/// the gate they would race *all* released secondaries through the
/// concurrent MMU-adopt / GIC-init path (observed on a Raspberry Pi 4 as
/// the last-released core deterministically faulting mid-bring-up and
/// never checking in). The exact predicate is [`release_gate_open`]. The
/// all-ones initial value matches no real masked affinity, so the gate is
/// closed until a deliberate release opens it (fail closed); it is in the
/// image's `.data` for the release word's reason.
#[no_mangle]
#[used]
#[cfg_attr(
    all(target_arch = "aarch64", target_os = "none"),
    link_section = ".data.secondary_release"
)]
static SECONDARY_KERNEL_RELEASE_TARGET: AtomicU64 = AtomicU64::new(u64::MAX);

/// The per-core release-gate predicate the `boot.s` park loop and the
/// `smp.s` spin-table trampoline both implement in assembly: a woken
/// secondary may leave its `wfe` park and begin bring-up only when the
/// published release target (`SECONDARY_KERNEL_RELEASE_TARGET`) names
/// its own masked ([`MPIDR_AFFINITY_MASK`]) `MPIDR_EL1` affinity.
///
/// This is the single, host-tested definition of the gate decision — the
/// two assembly sites compile the same `cmp target, affinity` / `b.eq`
/// against it, so the rule can never drift between them. It is a pure
/// comparison with no ambient state, so it is exhaustively testable off
/// the metal even though the parked-core polling itself is not.
///
/// `target` is the value read from `SECONDARY_KERNEL_RELEASE_TARGET`
/// and `own_affinity` the core's masked affinity; the all-ones initial
/// target names no real affinity, so the gate stays shut until a
/// deliberate release opens it for exactly one core.
#[must_use]
pub const fn release_gate_open(target: u64, own_affinity: u64) -> bool {
    target == own_affinity
}

/// Set-once guard for [`register_secondary_affinities`].
static SECONDARY_AFFINITIES_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Failure mode of [`register_secondary_affinities`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SecondaryAffinityError {
    /// A table was already published; the slot is set-once per boot (no
    /// silent re-pointing of the live trampoline's identity source).
    AlreadyRegistered,
}

/// Publish the dense-id → affinity table the spin-table trampoline
/// recovers a released core's dense [`CpuId`] from (entry `i` is dense
/// id `i`'s `MPIDR_EL1` affinity, `Aff0`–`Aff2` masked), returning the
/// covered CPU count.
///
/// Must be called on the boot core, exactly once, before any spin-table
/// release; the `&'static` slice lives for the kernel's lifetime. The
/// released core reads the table once its MMU and caches are on.
///
/// # Errors
///
/// [`SecondaryAffinityError::AlreadyRegistered`] on the second publish.
pub fn register_secondary_affinities(
    affinities: &'static [u64],
) -> Result<usize, SecondaryAffinityError> {
    if SECONDARY_AFFINITIES_REGISTERED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(SecondaryAffinityError::AlreadyRegistered);
    }
    SECONDARY_AFFINITY_BASE.store(affinities.as_ptr() as u64, Ordering::Release);
    SECONDARY_AFFINITY_COUNT.store(affinities.len() as u64, Ordering::Release);
    Ok(affinities.len())
}

/// `true` iff a dense-id → affinity table has been published for the
/// spin-table trampoline. Test/diagnostic observer.
#[must_use]
pub fn secondary_affinities_registered() -> bool {
    SECONDARY_AFFINITY_BASE.load(Ordering::Acquire) != 0
}

#[cfg(test)]
fn reset_secondary_affinities_for_tests() {
    SECONDARY_AFFINITIES_REGISTERED.store(false, Ordering::Release);
    SECONDARY_AFFINITY_BASE.store(0, Ordering::Release);
    SECONDARY_AFFINITY_COUNT.store(0, Ordering::Release);
}

/// Failure modes of [`set_secondary_entry`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SetEntryError {
    /// An entry was already installed; the slot is set-once per boot.
    AlreadyInstalled,
}

/// Failure modes of `start_secondary`.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StartCpuError {
    /// `cpu` was outside the registered [`SecondaryStackPool`]'s core
    /// count, so the trampoline would select a stack slice outside it.
    CpuIdOutOfRange,
    /// No secondary entry was installed via [`set_secondary_entry`];
    /// starting a core that would immediately park is refused so the
    /// failure is loud at the call site, not silent on the new core.
    NoEntryInstalled,
    /// No dense-id → affinity table was published via
    /// [`register_secondary_affinities`]; a spin-table-released core
    /// could not recover its dense id, so the release is refused at the
    /// call site rather than parking the core in the trampoline.
    NoAffinityTable,
    /// The firmware's release word is misaligned or lies where the direct
    /// map reaches no RAM, so no store could release the core.
    ReleaseWordUnreachable,
    /// The PSCI `CPU_ON` call returned an error (the core was already on,
    /// the MPIDR is invalid, the entry address was rejected, etc.); the
    /// payload is the raw PSCI status (`crate::psci::error`).
    Psci(i32),
    /// The core was started but never answered within the wait.
    StartTimedOut,
    /// The core answered that it does not offer PAN, which is in force, so
    /// it stopped rather than join.
    LacksPan,
}

impl StartCpuError {
    /// Stable cause string for audit records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CpuIdOutOfRange => "cpu_id_out_of_range",
            Self::NoEntryInstalled => "no_secondary_entry_installed",
            Self::NoAffinityTable => "no_secondary_affinity_table",
            Self::ReleaseWordUnreachable => "release_word_unreachable",
            Self::Psci(_) => "psci_cpu_on_failed",
            Self::StartTimedOut => "core_start_timed_out",
            Self::LacksPan => "core_lacks_pan",
        }
    }
}

impl From<StartCpuError> for SmpError {
    fn from(err: StartCpuError) -> Self {
        match err {
            StartCpuError::CpuIdOutOfRange | StartCpuError::ReleaseWordUnreachable => {
                Self::InvalidCpu
            }
            StartCpuError::NoEntryInstalled | StartCpuError::NoAffinityTable => Self::NotReady,
            StartCpuError::Psci(status) => Self::StartRejected(i64::from(status)),
            StartCpuError::StartTimedOut => Self::StartRejected(0),
            StartCpuError::LacksPan => Self::LacksProtection,
        }
    }
}

/// How long the boot CPU waits for a started core's answer: orders of
/// magnitude above a core's microsecond start, short enough that a dead core
/// does not stall the boot perceptibly.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub(crate) const START_ANSWER_BUDGET_NS: u64 = 500_000_000;

/// Wake every core waiting in `wfe`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn send_event() {
    // SAFETY: `sev` is a hint instruction with no operands or side effects
    // beyond waking `wfe` waiters.
    unsafe {
        core::arch::asm!("sev", options(nomem, nostack, preserves_flags));
    }
}

/// Complete this CPU's earlier stores before what follows — the firmware
/// call that starts a core, or the `sev` that wakes one — so the other CPU
/// reads them.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn complete_stores() {
    // SAFETY: `dsb ish` only waits for this CPU's earlier memory accesses to
    // complete in the inner-shareable domain.
    unsafe {
        core::arch::asm!("dsb ish", options(nostack, preserves_flags));
    }
}

/// Ready `slot` for its core's start: no answer and no verdict, both
/// complete before the core can read them.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn ready(slot: &SecondaryStack) {
    slot.answer.store(0, Ordering::Relaxed);
    slot.verdict.store(0, Ordering::Relaxed);
    complete_stores();
}

/// Settle the start of `cpu` on its `slot`: the answer it gives before
/// `out_of_time`, and the verdict on it given back, so a core answering
/// after the wait is told that its start was given up on.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn settle(
    slot: &SecondaryStack,
    cpu: CpuId,
    out_of_time: impl FnMut() -> bool,
) -> Result<(), StartCpuError> {
    let load = || slot.answer.load(Ordering::Acquire);
    let answer = tairix_arch_api::await_start_answer(load, cpu, out_of_time);
    give(slot, StartVerdict::on(answer), cpu);
    outcome(answer)
}

/// Give `cpu` the `verdict` on its start in its `slot`, and wake it from its
/// wait for one. Every start made gets one, since nothing else ends that
/// wait.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn give(slot: &SecondaryStack, verdict: StartVerdict, cpu: CpuId) {
    slot.verdict.store(verdict.word(cpu), Ordering::Release);
    complete_stores();
    send_event();
}

/// What the boot CPU makes of a core's `answer`: it joined, was refused for
/// lacking PAN, or gave no answer the boot CPU can read.
#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
fn outcome(answer: Option<StartAnswer>) -> Result<(), StartCpuError> {
    match answer {
        Some(StartAnswer::Joined) => Ok(()),
        Some(StartAnswer::Refused(lacking)) if lacking == crate::protection::PAN => {
            Err(StartCpuError::LacksPan)
        }
        Some(StartAnswer::Refused(_)) | None => Err(StartCpuError::StartTimedOut),
    }
}

/// Answer the boot CPU as `cpu` in its `slot`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn answer_boot_cpu(slot: &SecondaryStack, answer: StartAnswer, cpu: CpuId) {
    slot.answer.store(answer.word(cpu), Ordering::Release);
}

/// Whether the boot CPU took `cpu`'s join, the verdict read from its `slot`,
/// waiting in `wfe` for the event that gives it.
///
/// The wait is timed by the boot CPU alone, which gives every start a
/// verdict: a clock read here would be this core's own, unprogrammed on
/// boards that leave the rate to the device tree.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn taken(slot: &SecondaryStack, cpu: CpuId) -> bool {
    let wait_for_event = || {
        // SAFETY: `wfe` only suspends this core until an event, a `sev` or
        // an interrupt, and touches no register or memory.
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    };
    tairix_arch_api::await_start_verdict(
        || slot.verdict.load(Ordering::Acquire),
        cpu,
        wait_for_event,
    ) == StartVerdict::Taken
}

/// Install the entry a secondary core runs once started.
///
/// The function must be `-> !`: a secondary core has nowhere to return
/// to (the trampoline's only fallback is a `wfi` park). Encoding the
/// bottom type in the signature pins that at the call site.
///
/// # Errors
///
/// [`SetEntryError::AlreadyInstalled`] on the second publish.
pub fn set_secondary_entry(entry: extern "C" fn(CpuId) -> !) -> Result<(), SetEntryError> {
    if !SECONDARY_ENTRY_FN.claim(entry) {
        return Err(SetEntryError::AlreadyInstalled);
    }
    Ok(())
}

/// Address of the installed secondary entry (`0` if none).
/// Test/diagnostic observer.
#[must_use]
pub fn secondary_entry_addr() -> usize {
    SECONDARY_ENTRY_FN.addr() as usize
}

#[cfg(test)]
fn clear_secondary_entry_for_tests() {
    SECONDARY_ENTRY_FN.clear();
}

/// Mask isolating the affinity fields (`Aff0`–`Aff2`) of `MPIDR_EL1`.
/// The reserved bits (`RES1` at 31, `U` at 30, `MT` at 24) sit above the
/// `Aff2` byte and are excluded, so the masked value is the pure core
/// affinity the `virt` board assigns linearly.
pub const MPIDR_AFFINITY_MASK: u64 = 0x00FF_FFFF;

/// The Arch HAL per-CPU word used to hold the dense [`CpuId`] assigned from
/// the validated firmware topology.
static DENSE_CPU_ID: PerCpuStorage = PerCpuStorage::new();

/// Publish `cpu` as the calling core's dense identity.
///
/// The boot CPU publishes dense id zero after firmware topology validation;
/// each secondary publishes the id passed through the validated bring-up
/// conduit before enabling vectors, the GIC, or preemption. `TPIDR_EL1` is
/// banked per CPU, so migration cannot change another core's identity.
pub fn install_current_cpu_index(cpu: CpuId) {
    // SAFETY: this writes only the calling CPU's Arch HAL per-CPU word, and
    // `cpu` is the identity assigned to that same CPU by boot bring-up.
    unsafe {
        DENSE_CPU_ID.write_self_base(cpu as usize);
    }
}

/// Read the calling core's boot-published dense id.
///
/// Interrupt, preemption, scheduler-current, and continuation tables all use
/// this identity. Raw `MPIDR_EL1` affinities may be sparse or clustered and
/// therefore are never used directly as array indices.
#[must_use]
pub fn current_cpu_index() -> CpuId {
    CpuId::try_from(DENSE_CPU_ID.read_self_base()).unwrap_or(0)
}

/// Read the calling core's full `MPIDR_EL1` affinity (`Aff0`–`Aff2`,
/// [`MPIDR_AFFINITY_MASK`]) — the value a device tree's `/cpus/cpu@*`
/// `reg` carries, so the boot path can locate the running boot core
/// inside the discovered CPU list.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[must_use]
pub fn current_affinity() -> u64 {
    let mpidr: u64;
    // SAFETY: `mrs x, MPIDR_EL1` reads the multiprocessor-affinity
    // register; it is side-effect-free and readable at EL1.
    unsafe {
        core::arch::asm!("mrs {}, MPIDR_EL1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    mpidr & MPIDR_AFFINITY_MASK
}

/// Power on the parked secondary core `cpu` (whose firmware identity is
/// `target_mpidr`) at the `smp.s` trampoline, via PSCI `CPU_ON`, and wait
/// for its answer until `out_of_time`.
///
/// Validates the id against the stack pool and confirms an entry is
/// installed, then issues the PSCI call through the `method` conduit. On
/// success the target core runs the trampoline (which sets up its stack
/// and tail-calls the [`set_secondary_entry`] callback with `cpu`), once it
/// has answered that it holds the protection in force.
///
/// # Errors
///
/// See [`StartCpuError`]. The launcher fails closed
/// rather than assuming the core came up.
///
/// # Safety
///
/// Must be called from the boot core after `boot.s` has zeroed `.bss`
/// (so the secondary stack pool is clear) and after the secondary entry
/// is installed. `target_mpidr` must name a real, parked core distinct
/// from the caller, and `cpu` must be the dense id the rest of the kernel
/// uses for it.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn start_secondary(
    method: crate::fdt::PsciMethod,
    cpu: CpuId,
    target_mpidr: u64,
    out_of_time: impl FnMut() -> bool,
) -> Result<(), StartCpuError> {
    if !is_valid_cpu(cpu) {
        return Err(StartCpuError::CpuIdOutOfRange);
    }
    if secondary_entry_addr() == 0 {
        return Err(StartCpuError::NoEntryInstalled);
    }
    let Some(slot) = slot(cpu) else {
        return Err(StartCpuError::CpuIdOutOfRange);
    };
    let entry = crate::paging::image_phys(secondary_trampoline_addr() as u64);
    ready(slot);
    // SAFETY: `entry` is the physical address of the `smp.s` trampoline,
    // where a core starts with its MMU off; `cpu` is in range, and the
    // caller's contract guarantees `target_mpidr` names a real parked
    // core. `cpu` is handed back as the trampoline's `context_id`.
    let ret = unsafe { crate::psci::cpu_on(method, target_mpidr, entry, u64::from(cpu)) };
    if !ret.is_success() {
        // A core the firmware started regardless must not wait on for ever.
        give(slot, StartVerdict::Withdrawn, cpu);
        return Err(StartCpuError::Psci(ret.status));
    }
    settle(slot, cpu, out_of_time)
}

/// Release the **single** parked secondary core `cpu` (whose masked
/// `MPIDR_EL1` affinity is `target_affinity`) through the Devicetree
/// spin-table protocol: publish `target_affinity` as the released
/// target, write the spin-table trampoline's physical address to the
/// firmware-declared release word `release_addr` *and* to the kernel's
/// own `SECONDARY_KERNEL_RELEASE` park word, sweep all three to the
/// point of coherency, and signal `sev`.
///
/// Two release channels because the parked core's location depends on
/// the firmware: a core still spinning in the firmware stub polls its
/// own per-core `release_addr`, while a core the firmware released
/// straight into the kernel image polls the shared kernel word in
/// `boot.s`'s `_start` park loop. Whichever loop the core is in, it
/// branches to the same trampoline, which recovers its dense id from the
/// published affinity table.
///
/// **The release is one core at a time.** The firmware `release_addr` is
/// already per-core, but the kernel park word is *shared* — a single
/// `sev` wakes every core parked in `_start`. So the boot CPU also
/// publishes `target_affinity` into [`SECONDARY_KERNEL_RELEASE_TARGET`],
/// and the park loop only lets the core whose affinity matches proceed;
/// the rest re-park. This is a correctness requirement, not a nicety:
/// waking all secondaries at once would race them through the concurrent
/// MMU-adopt / GIC-init path simultaneously, which on a Raspberry Pi 4
/// intermittently faults the last-released core mid-bring-up so it never
/// checks in. The target is stored *before* the release word and swept
/// ahead of the `sev`, so a woken core that observes the open gate also
/// observes the correct target. A core not described in the affinity
/// table parks again in the trampoline (fail closed).
///
/// Validates the id against the stack pool and confirms the entry and
/// the affinity table are installed **before** any write, so a released
/// core can never wake into a half-published hand-off. Then waits for the
/// released core's answer until `out_of_time`, as a `CPU_ON` start does.
///
/// # Errors
///
/// See [`StartCpuError`]. The launcher fails closed rather than
/// assuming the core came up (a spin-table release has no firmware
/// status to inspect; a core that never answers is
/// [`StartCpuError::StartTimedOut`], and the system continues on the cores
/// that are online).
///
/// # Safety
///
/// Must be called from the boot core after the secondary entry and
/// affinity table are installed. `release_addr` must be the
/// firmware-declared `cpu-release-addr` word for `cpu` — RAM the kernel
/// image does not occupy, which the direct map must cover or the release
/// is refused — `cpu` must be the dense id the rest of the kernel uses for
/// the core parked on it, and `target_affinity` must be that core's
/// masked ([`MPIDR_AFFINITY_MASK`]) `MPIDR_EL1` affinity (the value the
/// park loop compares its own affinity against).
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub unsafe fn start_secondary_spintable(
    cpu: CpuId,
    release_addr: u64,
    target_affinity: u64,
    out_of_time: impl FnMut() -> bool,
) -> Result<(), StartCpuError> {
    if !is_valid_cpu(cpu) {
        return Err(StartCpuError::CpuIdOutOfRange);
    }
    if secondary_entry_addr() == 0 {
        return Err(StartCpuError::NoEntryInstalled);
    }
    if !secondary_affinities_registered() {
        return Err(StartCpuError::NoAffinityTable);
    }
    let Some(slot) = slot(cpu) else {
        return Err(StartCpuError::CpuIdOutOfRange);
    };
    let word = core::mem::size_of::<u64>() as u64;
    if !release_addr.is_multiple_of(word) || !crate::paging::physmap_covers(release_addr, word) {
        return Err(StartCpuError::ReleaseWordUnreachable);
    }
    let release = crate::paging::physmap_virt(release_addr);
    let entry = crate::paging::image_phys(spintable_trampoline_addr() as u64);
    ready(slot);
    // Name the core being released *before* opening the gate, so a core
    // that wakes on the `sev` and sees the release word non-zero also
    // sees the affinity it must match. Only the matching core proceeds;
    // the rest re-park (the park loop polls both words each iteration).
    SECONDARY_KERNEL_RELEASE_TARGET.store(target_affinity, Ordering::Release);
    // SAFETY: the caller's contract guarantees `release_addr` is the
    // firmware's declared release word — RAM outside the kernel image, which
    // the direct map was just checked to cover at `release` — so the volatile
    // store writes only the word the parked core polls.
    unsafe {
        core::ptr::write_volatile(release as *mut u64, entry);
    }
    SECONDARY_KERNEL_RELEASE.store(entry, Ordering::Release);
    // The parked cores poll with their MMU (and cache) off: push the
    // target, the firmware release word, and the kernel release word to
    // the point of coherency, then wake the `wfe` loops. The last
    // sweep's own `dsb sy` orders every store ahead of `sev`.
    crate::paging::clean_invalidate_range_to_poc(
        core::ptr::addr_of!(SECONDARY_KERNEL_RELEASE_TARGET) as u64,
        core::mem::size_of::<AtomicU64>() as u64,
    );
    crate::paging::clean_invalidate_range_to_poc(release, word);
    crate::paging::clean_invalidate_range_to_poc(
        core::ptr::addr_of!(SECONDARY_KERNEL_RELEASE) as u64,
        core::mem::size_of::<AtomicU64>() as u64,
    );
    send_event();
    settle(slot, cpu, out_of_time)
}

/// Address of the `_start_secondary_aarch64` trampoline published by
/// `smp.s`.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn secondary_trampoline_addr() -> usize {
    extern "C" {
        fn _start_secondary_aarch64();
    }
    _start_secondary_aarch64 as *const () as usize
}

/// Address of the `_start_secondary_spintable_aarch64` trampoline
/// published by `smp.s` — the argument-free entry a spin-table release
/// branches a parked core to.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn spintable_trampoline_addr() -> usize {
    extern "C" {
        fn _start_secondary_spintable_aarch64();
    }
    _start_secondary_spintable_aarch64 as *const () as usize
}

/// Rust side of the secondary trampoline.
///
/// `smp.s` jumps here, once per secondary core, on the kernel root with a
/// private stack. It leaves the identity page behind, holds the core to the
/// kernel's protection ([`crate::protection`]), answers the boot CPU
/// whether it could, and runs the installed [`set_secondary_entry`] callback
/// once the boot CPU took its join; a core that cannot hold it, whose start
/// was given up on, or one started with no callback installed (the
/// `start_secondary` guard makes that unreachable in practice), parks rather
/// than run, fail closed.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
#[no_mangle]
extern "C" fn tairix_arch_aarch64_secondary_main(cpu: CpuId) -> ! {
    // SAFETY: this core's first act, on its own stack with interrupts masked,
    // before any code the compiler may lower to FP/SIMD and before anything
    // that can fault, so a fault is reported rather than taken through the
    // vector the firmware left.
    unsafe { crate::exceptions::init_vectors() };
    install_current_cpu_index(cpu);
    // The identity page is only ever needed to cross into the high half.
    if !crate::paging::park_kernel_root(true) {
        crate::kernel_arch::halt_current_cpu();
    }
    // A core outside the pool was never started: its stack is the pool's.
    if let Some(slot) = slot(cpu) {
        // SAFETY: EL1 on a core fresh from the trampoline, whose low regime
        // maps nothing, so it reaches no user page; once held, translation is
        // enabled under what it holds.
        if unsafe { crate::protection::hold() } {
            // SAFETY: as above.
            unsafe { crate::paging::hold_translation_controls() };
            answer_boot_cpu(slot, StartAnswer::Joined, cpu);
            if taken(slot, cpu) {
                if let Some(entry) = SECONDARY_ENTRY_FN.load() {
                    entry(cpu);
                }
            }
        } else {
            answer_boot_cpu(slot, StartAnswer::Refused(crate::protection::PAN), cpu);
        }
    }
    crate::kernel_arch::halt_current_cpu()
}

#[cfg(test)]
#[path = "smp_tests.rs"]
mod tests;
