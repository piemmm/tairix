//! [`IrqTable`] — kernel IRQ binding table and per-handle ready
//! flag.
//!
//! See the crate-level docs for the design rationale; this module
//! is the implementation.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use tairix_abi::sysinfo::{IrqRecord, IRQ_FLAG_QUARANTINED};
use tairix_abi::IrqHandle;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_kernel_sec::ProcessId;
use tairix_sync::{OnceCell, RwLock};

use crate::error::{IrqError, MaskError};

/// One row in [`IrqTable`].
///
/// Public so kernel/core's `irq_release` audit emission can read
/// the bound owner; otherwise opaque.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IrqEntry {
    /// Handle minted at [`IrqTable::bind`] time.
    pub handle: IrqHandle,
    /// The **process** that owns this binding.
    ///
    /// Process-scoped, not per-thread: a driver's interrupt binding is a
    /// process resource like an open file, so any thread of the owning
    /// process may wait on it and the binding outlives the particular thread
    /// that made it. The forgery check in [`IrqTable::try_wait_step`] compares
    /// against this, so a *different* process can never wait on the line.
    pub owner: ProcessId,
    /// Architecture-defined IRQ line. Stable for the lifetime of
    /// the binding.
    pub line: u32,
}

/// Controller-mask seam.
///
/// The production [`IrqTable::fire`] path calls
/// [`Self::mask`] before it advances the line's fire count — the
/// load-bearing safety property of the user-space IRQ contract
/// (`docs/src/security/irq.md`). Architecture ports without a
/// programmable controller return [`MaskError::Unsupported`].
pub trait IrqController {
    /// Mask `line` at the controller. Must complete before
    /// [`IrqTable::fire`] advances the line's fire count.
    ///
    /// # Errors
    ///
    /// * [`MaskError::Unsupported`] if the architecture has no
    ///   programmable interrupt controller wired in this build.
    /// * [`MaskError::OutOfRange`] if `line` exceeds the
    ///   controller's addressable range.
    fn mask(&self, line: u32) -> Result<(), MaskError>;

    /// Route `line` to a waiting CPU and unmask it so the next device
    /// interrupt on it is delivered.
    ///
    /// [`IrqTable::fire`] masks a line before a waiter observes the wake
    /// (mask-before-wake, `docs/src/security/irq.md`), so once a driver has
    /// drained the completion the line must be re-enabled for the next one.
    /// A user-space interrupt-driven driver cannot touch the controller, so
    /// the `irq_wait` park path re-arms the bound line through this method on
    /// the driver's behalf (no ambient hardware access). It
    /// is idempotent: re-routing an already-routed line and clearing an
    /// already-clear mask are both no-ops.
    ///
    /// The default is a no-op for controllers without a programmable unmask
    /// (placeholders, mask-only test doubles, or ports with no interrupt-driven
    /// user-space driver consumer yet, no interface ahead of a caller).
    /// The aarch64 `GicIrqController` overrides it to route the line to the
    /// boot CPU and clear its enable bit, which is what the user-space
    /// virtio-input keyboard driver's `irq_wait` park path drives.
    ///
    /// # Errors
    ///
    /// * [`MaskError::Unsupported`] if the architecture has no programmable
    ///   controller wired in this build.
    /// * [`MaskError::OutOfRange`] if `line` exceeds the controller's range.
    fn rearm(&self, line: u32) -> Result<(), MaskError> {
        let _ = line;
        Ok(())
    }

    /// Make `line` signal by `trigger` before it is first unmasked. The
    /// default is a controller whose lines all signal by level.
    ///
    /// # Errors
    ///
    /// * [`MaskError::Unsupported`] for a trigger the controller cannot give
    ///   the line now.
    /// * [`MaskError::OutOfRange`] if `line` exceeds the controller's range.
    fn set_trigger(&self, line: u32, trigger: Trigger) -> Result<(), MaskError> {
        let _ = line;
        match trigger {
            Trigger::Level => Ok(()),
            Trigger::Edge => Err(MaskError::Unsupported),
        }
    }
}

/// How a wired line signals its device's interrupt.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Trigger {
    /// Asserted until the device is serviced.
    Level,
    /// By a pulse, which the controller must latch while the line is masked.
    Edge,
}

/// Outcome of one [`IrqTable::try_wait_step`] poll.
///
/// The syscall handler runs the polling loop, mapping each
/// outcome to the documented stable `Errno`:
///
/// * [`Self::Ready`] → `Ok(())`
/// * [`Self::Continue`] → park the caller until the line fires or its
///   deadline passes, then poll again.
/// * [`Self::TimedOut`] → `Err(Errno::TimedOut)`.
/// * [`Self::NotFound`] → `Err(Errno::NotFound)` — handle was
///   not minted for the caller, or was released between two
///   polls (e.g. the task exited and `release_for` ran).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WaitStep {
    /// The line has fired since the binding last took a fire, which it
    /// has now taken. The waiter must observe `Ok(())`.
    Ready,
    /// No fire yet; the deadline has not yet been reached. The
    /// waiter should yield and retry.
    Continue,
    /// `now_ns >= deadline_ns` without a fire.
    TimedOut,
    /// The handle does not belong to the caller (forged), or the
    /// binding has been released since the last poll.
    NotFound,
    /// The bound line was **quarantined**: it fired far faster than any
    /// real device serviced through the user-space round-trip could, so
    /// [`IrqTable::fire`] disabled it (kept it masked, stopped delivering
    /// wakes) to stop it monopolising a CPU. A terminal, fail-closed
    /// outcome: the waiter must surface an error rather than re-arm and
    /// re-park, since re-arming would immediately re-storm. The binding is
    /// cleared for the line's next [`IrqTable::bind`].
    Quarantined,
}

/// Outcome of an [`IrqTable::bind`] success path.
///
/// A separate type rather than a bare `IrqHandle` so the syscall
/// handler's audit record can name the bound line explicitly
/// (without re-reading the table).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BindOutcome {
    /// Newly minted opaque handle.
    pub handle: IrqHandle,
    /// Line the binding is recorded against.
    pub line: u32,
}

/// Outcome of [`IrqTable::fire`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FireOutcome {
    /// The line had a binding; its fire count advanced.
    /// Mask-before-wake was honoured.
    Marked,
    /// The line has no binding (a stray interrupt from a line no
    /// driver claims). The mask write still happened so the
    /// stray edge does not re-fire; ready was not touched.
    Stray,
    /// The line fired past its rate budget within the accounting window and
    /// has been **quarantined**: the mask write happened (so it stays
    /// contained) but no waiter is told of the fire — a
    /// quarantined line delivers no further wakes until it is rebound. This
    /// is the runaway-interrupt safety net: a device (or user-space driver)
    /// that re-asserts a line as fast as the kernel can mask/wake it would
    /// otherwise peg a CPU, so the kernel disables the line and lets its
    /// waiter fail closed rather than spin. The dispatch caller emits the
    /// audit record; the parked waiter observes [`WaitStep::Quarantined`].
    Quarantined,
}

/// Placeholder [`IrqController`] for architecture ports without a
/// programmable interrupt controller wired in this build.
///
/// Every call to [`Self::mask`] returns [`MaskError::Unsupported`],
/// which [`IrqTable::fire`] translates to
/// [`IrqError::ArchUnsupported`] and the syscall handler in turn
/// translates to `Errno::NotImplemented`. The kernel binary's
/// startup audit log records one event naming the architecture
/// before installing the unsupported controller, per the
/// failure-mode table in `docs/src/security/irq.md`.
#[derive(Copy, Clone, Debug, Default)]
pub struct UnsupportedController;

impl IrqController for UnsupportedController {
    fn mask(&self, _line: u32) -> Result<(), MaskError> {
        Err(MaskError::Unsupported)
    }
}

/// Shared `'static` [`UnsupportedController`] suitable as the default
/// controller reference handed back from
/// `tairix_kernel_core::KernelArch::irq_routing` on architectures
/// or boot paths that have not yet installed a real controller.
///
/// Exposed as a `pub static` (not a `const`) so callers can take a
/// `&'static (dyn IrqController + Send + Sync)` reference without
/// risking the `clippy::declare_interior_mutable_const` footgun. The
/// unit-like type has no interior mutability, so the lint does not
/// fire here, but the `static` form keeps the address stable and
/// allows the type-erased reference to round-trip through the
/// `tairix_kernel_core` handover without surprise. It is an immutable
/// static, not global mutable state.
pub static UNSUPPORTED_CONTROLLER: UnsupportedController = UnsupportedController;

/// Outcome of [`IrqTable::release_for`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReleaseOutcome {
    /// Number of bindings the call dropped.
    pub released: usize,
}

/// An [`IrqTable::set_observer`] call was rejected because an observer was
/// already installed. The hook is set-once at boot; a second install is a
/// defect, not a runtime condition.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ObserverAlreadyInstalled;

/// A passive observer notified on every interrupt dispatch.
///
/// [`IrqTable::fire`] calls [`Self::on_irq`] at its entry for **every**
/// interrupt arrival — bound *and* stray — before the controller mask and the
/// fire count's advance. The kernel installs one implementation whose only job is
/// to feed the interrupt-arrival *timing* into the kernel entropy pool
/// (`lib/rng`), turning the physically-unpredictable inter-arrival intervals
/// of real devices into an independent entropy input.
///
/// # Contract
///
/// * It runs in **interrupt context**: it must be wait-free and must never
///   block, take a lock, allocate, or panic.
/// * It must **not** influence the mask-before-wake path — it is purely
///   observational, so a slow or absent observer can never weaken the IRQ
///   security contract (`docs/src/security/irq.md`).
/// * The [`Sync`] supertrait lets a `&'static dyn IrqDispatchObserver` be
///   shared across CPUs, which every SMP IRQ path requires.
pub trait IrqDispatchObserver: Sync {
    /// Notify the observer that `line` fired. See the trait contract.
    fn on_irq(&self, line: u32);
}

/// Monotonic-clock seam the runaway-interrupt safety net reads.
///
/// [`IrqTable::fire`] runs in interrupt context and must stay wait-free, so
/// the rate-accounting clock is injected as a set-once, lock-free hook (like
/// [`IrqDispatchObserver`]) rather than threaded through every architecture's
/// dispatch call site. `kernel/irq` keeps no architecture dependency: the
/// kernel binary installs one whose `now_ns` reads the arch monotonic timer.
/// Until a clock is installed, rate accounting is inert (a line is never
/// quarantined), so a host test or an early-boot fire behaves exactly as
/// before.
///
/// # Contract
///
/// * It runs in **interrupt context**: it must be wait-free and must never
///   block, take a lock, allocate, or panic.
/// * `now_ns` must be non-decreasing on a given CPU; the accounting tolerates
///   cross-CPU skew (it is a coarse safety net, not a precise meter).
/// * The [`Sync`] supertrait lets a `&'static dyn MonotonicClock` be shared
///   across CPUs, which every SMP IRQ path requires.
pub trait MonotonicClock: Sync {
    /// Current kernel monotonic time in nanoseconds.
    fn now_ns(&self) -> u64;
}

/// Length of the sliding window over which a line's fires are counted, in
/// nanoseconds (1 second). Chosen with [`STORM_FIRE_BUDGET`] so the trip
/// threshold is a sustained *rate* no correctly-serviced device reaches.
pub(crate) const STORM_WINDOW_NS: u64 = 1_000_000_000;

/// Maximum fires one line may take within a [`STORM_WINDOW_NS`] window before
/// it is quarantined.
///
/// This is a **safety net against a runaway line**, not a throughput cap.
/// Under mask-before-wake a bound line can only re-fire *after* its
/// user-space driver drains the completion and the kernel re-arms it, so
/// every legitimate interrupt costs a full user-space round-trip (syscall →
/// driver work → syscall). No real device driven that way sustains anywhere
/// near 100 000 interrupts per second on one line; a line that does is
/// re-asserting with no work being done (a wedged controller, a
/// never-quiesced source, or a hostile device), i.e. exactly the storm that
/// would otherwise peg a CPU. The budget is deliberately generous so a busy
/// but healthy line never trips, mirroring Linux's `note_interrupt`
/// spurious-IRQ disable in spirit while fitting this kernel's userspace-IRQ
/// model.
pub(crate) const STORM_FIRE_BUDGET: u32 = 100_000;

/// Kernel IRQ table.
///
/// One per running kernel, owned by `KernelState`. Bindings live behind a
/// writer-preference [`RwLock`]; everything [`IrqTable::fire`] touches is a
/// per-line atomic, so the interrupt path never takes the lock.
///
/// A line may be shared — a wired PCI INTx pin raised by several functions —
/// so each binding is woken by every fire of its line. The line stays masked
/// from a fire until every sharer has taken it and come back to wait: a
/// level-triggered line one sharer's device still holds re-asserts as soon as
/// it is unmasked, so unmasking early would wake the others for nothing until
/// that sharer serviced its device.
pub struct IrqTable {
    inner: RwLock<Inner>,
    max_line: u32,
    /// Set-once, lock-free-read hook notified on every [`IrqTable::fire`]
    /// (see [`IrqDispatchObserver`]); a no-op while empty.
    observer: OnceCell<&'static dyn IrqDispatchObserver>,
    /// Set-once, lock-free-read clock the runaway-interrupt safety net reads
    /// (see [`MonotonicClock`]); while empty no line is quarantined.
    clock: OnceCell<&'static dyn MonotonicClock>,
    /// Per-line start of the current fire-accounting window (arch monotonic
    /// ns), paired with [`Self::storm_fire_count`].
    storm_window_start_ns: Vec<AtomicU64>,
    /// Per-line count of fires in the current window; past
    /// [`STORM_FIRE_BUDGET`] the line is quarantined.
    storm_fire_count: Vec<AtomicU32>,
    /// Per-line sticky quarantine: [`IrqTable::fire`] stops delivering and
    /// nothing re-arms the line until its last binding goes.
    quarantined: Vec<AtomicBool>,
    /// Per-line count of every edge the line has taken since boot, strays
    /// and storm fires included, never reset. It is also the sequence a
    /// binding is woken by: one is ready while the count has moved past the
    /// last value it took.
    fires: Vec<AtomicU64>,
    /// Per-line "it has a binding" flags, written under the `Inner` write
    /// lock and read lock-free by [`IrqTable::fire`].
    bound: Vec<AtomicBool>,
}

impl core::fmt::Debug for IrqTable {
    /// Reports only lock-free fields: it must not take the `Inner` lock (a
    /// `fire`-context or parked-waiter deadlock hazard) and never reveals
    /// bindings. The observer is shown by presence only.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IrqTable")
            .field("max_line", &self.max_line)
            .field(
                "observer_installed",
                &matches!(self.observer.get(), Ok(Some(_))),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Inner {
    /// Monotonically incrementing source of fresh [`IrqHandle`]
    /// values. Starts at 1 because [`IrqHandle::INVALID`] is 0.
    next_handle: u64,
    /// Each line's bindings, by line: a hardware interrupt arrives addressed
    /// by line, not by handle.
    lines: Vec<Line>,
    /// `handle → line`, so a handle's binding is found without a scan.
    by_handle: HashMap<u64, u32, BuildFastHash>,
}

/// One line's bindings, in bind order.
#[derive(Debug, Default)]
struct Line {
    sharers: Vec<Binding>,
    /// Bound by a kernel service, which no other binding may join: a sharer
    /// that never came back to wait would hold the line masked, and the
    /// service's own wait has no deadline to notice.
    exclusive: bool,
}

impl Inner {
    fn binding(&self, handle: IrqHandle) -> Option<&Binding> {
        let line = self.by_handle.get(&handle.as_u64())?;
        self.lines
            .get(*line as usize)?
            .sharers
            .iter()
            .find(|binding| binding.entry.handle == handle)
    }

    /// Every binding, in ascending line order and then bind order.
    fn bindings(&self) -> impl Iterator<Item = &Binding> {
        self.lines.iter().flat_map(|line| &line.sharers)
    }

    /// `handle`'s binding where `caller` owns it: the forgery check every
    /// handle-keyed operation makes before it acts.
    fn owned(&self, handle: IrqHandle, caller: ProcessId) -> Option<&Binding> {
        self.binding(handle)
            .filter(|binding| binding.entry.owner == caller)
    }
}

/// One owner's binding of a line.
#[derive(Debug)]
struct Binding {
    entry: IrqEntry,
    /// The line's fire count when this binding last took a fire.
    taken: AtomicU64,
    /// The line's fire count when this binding last came back to wait with
    /// nothing left to take.
    armed: AtomicU64,
}

impl IrqTable {
    /// Construct an empty IRQ table.
    ///
    /// `max_line` is the inclusive upper bound on the architecture
    /// port's IRQ-line identifier space — on x86_64 this is the
    /// IO-APIC's `max_redirection_entry`; on architectures whose
    /// production [`IrqController::mask`] returns
    /// [`MaskError::Unsupported`], pass `0` so every `bind` call
    /// fails-fast with [`IrqError::LineOutOfRange`] before any
    /// state is touched (fail closed).
    #[must_use]
    pub fn new(max_line: u32) -> Self {
        // One slot per addressable line (`0..=max_line`); `bind` rejects any
        // line above `max_line`, so every bound line indexes a valid slot.
        let slots = (max_line as usize).saturating_add(1);
        Self {
            inner: RwLock::new(Inner {
                next_handle: 1,
                lines: (0..slots).map(|_| Line::default()).collect(),
                by_handle: HashMap::with_hasher(BuildFastHash::new()),
            }),
            max_line,
            observer: OnceCell::new(),
            clock: OnceCell::new(),
            storm_window_start_ns: (0..slots).map(|_| AtomicU64::new(0)).collect(),
            storm_fire_count: (0..slots).map(|_| AtomicU32::new(0)).collect(),
            quarantined: (0..slots).map(|_| AtomicBool::new(false)).collect(),
            fires: (0..slots).map(|_| AtomicU64::new(0)).collect(),
            bound: (0..slots).map(|_| AtomicBool::new(false)).collect(),
        }
    }

    /// Install the set-once monotonic clock the runaway-interrupt safety net
    /// reads (see [`MonotonicClock`]).
    ///
    /// Called **exactly once** at boot, after the arch timer is discovered.
    /// Until it is installed, [`IrqTable::fire`] performs no rate accounting
    /// and no line is ever quarantined, so an early-boot interrupt behaves
    /// exactly as before.
    ///
    /// # Errors
    ///
    /// [`ObserverAlreadyInstalled`] if a clock is already installed — a
    /// second install is a defect (set-once), not a runtime condition.
    pub fn set_clock(
        &self,
        clock: &'static dyn MonotonicClock,
    ) -> Result<(), ObserverAlreadyInstalled> {
        self.clock.set(clock).map_err(|_| ObserverAlreadyInstalled)
    }

    /// Install the set-once interrupt-dispatch observer (see
    /// [`IrqDispatchObserver`]).
    ///
    /// Called **exactly once** at boot, after the arch entropy source is
    /// available, to feed interrupt-arrival timing into the kernel entropy
    /// pool. The observer reference outlives the running kernel (the kernel
    /// leaks it, like the table itself).
    ///
    /// # Errors
    ///
    /// [`ObserverAlreadyInstalled`] if an observer is already installed — a
    /// second install is a defect (set-once), not a runtime condition.
    pub fn set_observer(
        &self,
        observer: &'static dyn IrqDispatchObserver,
    ) -> Result<(), ObserverAlreadyInstalled> {
        self.observer
            .set(observer)
            .map_err(|_| ObserverAlreadyInstalled)
    }

    /// Inclusive upper bound on accepted line numbers.
    #[must_use]
    pub fn max_line(&self) -> u32 {
        self.max_line
    }

    /// Number of bindings currently recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().by_handle.len()
    }

    /// `true` iff there are no recorded bindings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.read().by_handle.is_empty()
    }

    /// Bind `line` to `owner`, minting a fresh [`IrqHandle`]. The line may
    /// already be bound to other owners, which then share it, unless a kernel
    /// service holds it ([`Self::bind_exclusive`]).
    ///
    /// # Errors
    ///
    /// * [`IrqError::LineOutOfRange`] if `line > self.max_line()`.
    /// * [`IrqError::LineAlreadyBound`] if `owner` already binds `line`, or a
    ///   kernel service holds it.
    /// * [`IrqError::Exhausted`] if the binding cannot be recorded.
    pub fn bind(&self, line: u32, owner: ProcessId) -> Result<BindOutcome, IrqError> {
        self.bind_as(line, owner, false)
    }

    /// Bind `line` to the kernel service `owner` alone: no other binding may
    /// join it while the service holds it, and the service takes no line
    /// another already binds.
    ///
    /// # Errors
    ///
    /// As [`Self::bind`], and [`IrqError::LineAlreadyBound`] for a line
    /// anything else binds.
    pub fn bind_exclusive(&self, line: u32, owner: ProcessId) -> Result<BindOutcome, IrqError> {
        self.bind_as(line, owner, true)
    }

    fn bind_as(
        &self,
        line: u32,
        owner: ProcessId,
        exclusive: bool,
    ) -> Result<BindOutcome, IrqError> {
        if line > self.max_line {
            return Err(IrqError::LineOutOfRange);
        }
        let slot = line as usize;
        let mut g = self.inner.write();
        let Inner {
            next_handle,
            lines,
            by_handle,
        } = &mut *g;
        let held = lines.get_mut(slot).ok_or(IrqError::LineOutOfRange)?;
        if held.exclusive
            || (exclusive && !held.sharers.is_empty())
            || held
                .sharers
                .iter()
                .any(|binding| binding.entry.owner == owner)
        {
            return Err(IrqError::LineAlreadyBound);
        }
        let raw = *next_handle;
        held.sharers
            .try_reserve(1)
            .map_err(|_| IrqError::Exhausted)?;
        by_handle
            .try_insert(raw, line)
            .map_err(|_| IrqError::Exhausted)?;
        if held.sharers.is_empty() {
            // The first binding starts the line clean, recovering one a
            // previous owner's storm quarantined.
            self.storm_window_start_ns[slot].store(0, Ordering::SeqCst);
            self.storm_fire_count[slot].store(0, Ordering::SeqCst);
            self.quarantined[slot].store(false, Ordering::SeqCst);
        }
        held.exclusive = exclusive;
        let handle = IrqHandle::from_raw(raw);
        // Fires before the bind are not this binding's to take.
        let fires = self.fires[slot].load(Ordering::SeqCst);
        held.sharers.push(Binding {
            entry: IrqEntry {
                handle,
                owner,
                line,
            },
            taken: AtomicU64::new(fires),
            armed: AtomicU64::new(fires),
        });
        // `next_handle` starts at 1 and is monotonic; saturating at
        // `u64::MAX` is a fail-closed limit, never a wrap onto a live handle.
        *next_handle = raw.saturating_add(1);
        self.bound[slot].store(true, Ordering::SeqCst);
        Ok(BindOutcome { handle, line })
    }

    /// Inspect the binding for `handle` on behalf of `caller`, returning the
    /// next step the waiter takes.
    ///
    /// `now_ns` and `deadline_ns` come from `KernelArch::monotonic_ns`; the
    /// waiter computes the deadline once and passes it on every poll.
    ///
    /// # Ordering
    ///
    /// 1. An unknown handle, or one another task owns, is
    ///    [`WaitStep::NotFound`]: identify before any transition.
    /// 2. A quarantined line is [`WaitStep::Quarantined`] (terminal, fail
    ///    closed); it never delivers, so this masks no real fire.
    /// 3. A fire the binding has not taken is [`WaitStep::Ready`], and is
    ///    taken. It wins over a near-simultaneous deadline: the wake-up did
    ///    happen.
    /// 4. `now_ns >= deadline_ns` is [`WaitStep::TimedOut`].
    /// 5. Otherwise [`WaitStep::Continue`].
    #[must_use]
    pub fn try_wait_step(
        &self,
        handle: IrqHandle,
        caller: ProcessId,
        now_ns: u64,
        deadline_ns: u64,
    ) -> WaitStep {
        // A read guard only: `fire` takes no lock, so the same-CPU completion
        // interrupt cannot deadlock against a parked waiter holding it.
        let g = self.inner.read();
        let Some(binding) = g.owned(handle, caller) else {
            return WaitStep::NotFound;
        };
        let slot = binding.entry.line as usize;
        if self.quarantined[slot].load(Ordering::Acquire) {
            return WaitStep::Quarantined;
        }
        // `fire` advances the count after the controller mask, so taking it
        // here observes the mask (mask-before-wake).
        let fires = self.fires[slot].load(Ordering::SeqCst);
        if binding.taken.swap(fires, Ordering::SeqCst) != fires {
            return WaitStep::Ready;
        }
        if now_ns >= deadline_ns {
            return WaitStep::TimedOut;
        }
        WaitStep::Continue
    }

    /// Record that `caller`, waiting on `handle` with nothing left to take,
    /// is back for the next fire, and unmask the line through `controller`
    /// once every sharer is. [`None`] if the handle is unknown or another
    /// task's.
    ///
    /// The waiter calls this before each park: a user-space driver holds no
    /// controller access, so the kernel re-arms on its behalf. A binding with
    /// a fire it has not taken re-arms nothing — it is about to wake — and a
    /// quarantined line is never re-armed.
    pub fn rearm(
        &self,
        handle: IrqHandle,
        caller: ProcessId,
        controller: &dyn IrqController,
    ) -> Option<Result<(), MaskError>> {
        let g = self.inner.read();
        let binding = g.owned(handle, caller)?;
        let line = binding.entry.line;
        let fires = self.fires[line as usize].load(Ordering::SeqCst);
        if binding.taken.load(Ordering::SeqCst) != fires {
            return Some(Ok(()));
        }
        binding.armed.store(fires, Ordering::SeqCst);
        Some(self.unmask_if_armed(&g, line, fires, controller))
    }

    /// Unmask `line` if every binding of it is armed at `fires` and it is not
    /// quarantined.
    ///
    /// Sharers store their own `armed` and then read the others' with
    /// `SeqCst`, so of two racing to finish, at least one sees both.
    fn unmask_if_armed(
        &self,
        g: &Inner,
        line: u32,
        fires: u64,
        controller: &dyn IrqController,
    ) -> Result<(), MaskError> {
        let armed = g.lines.get(line as usize).is_some_and(|held| {
            !held.sharers.is_empty()
                && held
                    .sharers
                    .iter()
                    .all(|binding| binding.armed.load(Ordering::SeqCst) == fires)
        });
        if !armed || self.quarantined[line as usize].load(Ordering::Acquire) {
            return Ok(());
        }
        controller.rearm(line)
    }

    /// The line bound to `handle` for `caller`, or [`None`] if the handle is
    /// unknown or its binding is owned by another task.
    #[must_use]
    pub fn line_for(&self, handle: IrqHandle, caller: ProcessId) -> Option<u32> {
        self.inner
            .read()
            .owned(handle, caller)
            .map(|binding| binding.entry.line)
    }

    /// The task holding the earliest binding of `line`, or [`None`] if it
    /// is unbound.
    ///
    /// A read-only, owner-agnostic lookup the CPU-lockup watchdog uses to
    /// attribute a stuck line to a driver. It grants no authority.
    #[must_use]
    pub fn owner_of_line(&self, line: u32) -> Option<ProcessId> {
        self.inner
            .read()
            .lines
            .get(line as usize)?
            .sharers
            .first()
            .map(|binding| binding.entry.owner)
    }

    /// The wire-encoded IRQ-table page starting at record offset `first`, at
    /// most `max_records` whole [`IrqRecord`]s (`IntrospectDomain::Irqs`
    /// paging: an offset past the end returns the empty terminator).
    ///
    /// One record per binding, in ascending line order and then bind order:
    /// line id, the kernel-attested owning task, the line's fire count since
    /// boot, and its quarantine flag. Read-only; it grants no authority.
    #[must_use]
    pub fn records(&self, first: u64, max_records: usize) -> Vec<u8> {
        let skip = usize::try_from(first).unwrap_or(usize::MAX);
        let inner = self.inner.read();
        let mut out = Vec::new();
        for binding in inner.bindings().skip(skip).take(max_records) {
            let slot = binding.entry.line as usize;
            let flags = if self.quarantined[slot].load(Ordering::Acquire) {
                IRQ_FLAG_QUARANTINED
            } else {
                0
            };
            let record = IrqRecord {
                line: binding.entry.line,
                flags,
                owner: binding.entry.owner.0,
                count: self.fires[slot].load(Ordering::Relaxed),
            };
            out.extend_from_slice(&record.to_le_bytes());
        }
        out
    }

    /// Fire `line`: mask the controller, then advance the line's fire count
    /// every binding of it is woken by.
    ///
    /// **Mask-before-wake is the load-bearing invariant**
    /// (`docs/src/security/irq.md`): the controller-level mask is installed
    /// *before* a waiter can observe the fire, so the line cannot re-fire
    /// while its drivers drain their completions.
    ///
    /// # Errors
    ///
    /// * [`IrqError::ArchUnsupported`] if the controller's `mask`
    ///   returned [`MaskError::Unsupported`].
    /// * [`IrqError::LineOutOfRange`] if `controller.mask` returned
    ///   [`MaskError::OutOfRange`] — the arch port disagreed with
    ///   the table's `max_line`. A bug rather than a runtime
    ///   condition, but routed to a stable errno (fail closed, never panic).
    pub fn fire(&self, line: u32, controller: &dyn IrqController) -> Result<FireOutcome, IrqError> {
        // The entropy observer samples arrival timing first, as close to the
        // edge as possible; it is wait-free and touches nothing below.
        if let Ok(Some(observer)) = self.observer.get() {
            observer.on_irq(line);
        }
        controller.mask(line).map_err(|e| match e {
            MaskError::Unsupported => IrqError::ArchUnsupported,
            MaskError::OutOfRange => IrqError::LineOutOfRange,
        })?;
        // Interrupt context: only the lock-free per-line atomics. Taking
        // `Inner`'s lock here would deadlock a CPU whose parked task holds it.
        let slot = line as usize;
        let (Some(bound), Some(fires)) = (self.bound.get(slot), self.fires.get(slot)) else {
            return Ok(FireOutcome::Stray);
        };
        // `mask` fenced before returning; advancing the count after it is
        // what a waiter's `SeqCst` take pairs with.
        fires.fetch_add(1, Ordering::SeqCst);
        if !bound.load(Ordering::SeqCst) {
            // Contained by the mask; the caller may audit the stray.
            return Ok(FireOutcome::Stray);
        }
        // A line firing past its rate budget stays masked and stops being
        // re-armed, so a never-quiesced or hostile source cannot peg a CPU
        // through the mask/wake/re-arm cycle.
        if self.note_fire_and_quarantined(slot) {
            return Ok(FireOutcome::Quarantined);
        }
        Ok(FireOutcome::Marked)
    }

    /// Record one fire of `line` against its sliding-window rate budget and
    /// report whether the line is now (or already) **quarantined**.
    ///
    /// Wait-free, for [`Self::fire`]'s interrupt context; inert until a
    /// clock is installed. Cross-CPU races on the count only shift the trip
    /// point by a few fires, immaterial against [`STORM_FIRE_BUDGET`].
    fn note_fire_and_quarantined(&self, line: usize) -> bool {
        if self.quarantined[line].load(Ordering::Acquire) {
            return true;
        }
        let Ok(Some(clock)) = self.clock.get() else {
            return false;
        };
        let now = clock.now_ns();
        let start = self.storm_window_start_ns[line].load(Ordering::Relaxed);
        if now.saturating_sub(start) >= STORM_WINDOW_NS {
            self.storm_window_start_ns[line].store(now, Ordering::Relaxed);
            self.storm_fire_count[line].store(1, Ordering::Relaxed);
            return false;
        }
        let count = self.storm_fire_count[line]
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if count > STORM_FIRE_BUDGET {
            self.quarantined[line].store(true, Ordering::Release);
            return true;
        }
        false
    }

    /// Whether the line bound to `handle` for `caller` has been
    /// quarantined by the runaway-line safety net.
    ///
    /// Owner-checked like [`Self::line_for`]: a forged or foreign handle
    /// reports `false`. A pure read, safe from any context.
    #[must_use]
    pub fn is_quarantined(&self, handle: IrqHandle, caller: ProcessId) -> bool {
        self.line_for(handle, caller)
            .is_some_and(|line| self.quarantined[line as usize].load(Ordering::Acquire))
    }

    /// Drop every binding owned by `process`, unmasking through `controller`
    /// a shared line it was holding masked once its other sharers are armed.
    ///
    /// Idempotent. Scoped to the process, not to one thread: a binding is a
    /// process resource like an open file. A device that may still be
    /// raising a shared line is silenced before this, or the line re-asserts
    /// as soon as it is unmasked.
    pub fn release_for(
        &self,
        process: ProcessId,
        controller: &dyn IrqController,
    ) -> ReleaseOutcome {
        let mut g = self.inner.write();
        let mut released = 0;
        // A search per binding rather than a list of them: this runs when a
        // process ends, where an allocation must not be able to fail it.
        loop {
            let next = g
                .bindings()
                .find(|binding| binding.entry.owner == process)
                .map(|binding| binding.entry.handle);
            let Some(handle) = next else {
                break;
            };
            self.drop_binding(&mut g, handle, controller);
            released += 1;
        }
        ReleaseOutcome { released }
    }

    /// `owner`'s binding on the lowest line above `after`, or on the lowest
    /// line of all when `after` is [`None`] — a cursor over one owner's
    /// bindings that holds the table lock only for each step.
    #[must_use]
    pub fn next_binding_of(&self, owner: ProcessId, after: Option<u32>) -> Option<IrqEntry> {
        let from = match after {
            Some(line) => line.checked_add(1)?,
            None => 0,
        };
        self.inner
            .read()
            .lines
            .get(from as usize..)?
            .iter()
            .flat_map(|held| &held.sharers)
            .find(|binding| binding.entry.owner == owner)
            .map(|binding| binding.entry)
    }

    /// Release the binding `handle`, returning whether `owner` still held it,
    /// unmasking as [`Self::release_for`] does.
    ///
    /// A parked [`Self::try_wait_step`] on the handle reports
    /// [`WaitStep::NotFound`] from its next poll. Keyed by handle, so a line
    /// the owner has since rebound is not released by a stale caller.
    pub fn release_binding(
        &self,
        handle: IrqHandle,
        owner: ProcessId,
        controller: &dyn IrqController,
    ) -> bool {
        let mut g = self.inner.write();
        if g.owned(handle, owner).is_none() {
            return false;
        }
        self.drop_binding(&mut g, handle, controller);
        true
    }

    /// Remove `handle`'s binding. The last binding of a line resets its
    /// lock-free state, so a late edge is a stray and a later bind starts
    /// clean; a binding that was holding a shared line masked lets the
    /// others' re-arm through.
    fn drop_binding(&self, g: &mut Inner, handle: IrqHandle, controller: &dyn IrqController) {
        let Some(line) = g.by_handle.remove(&handle.as_u64()) else {
            return;
        };
        let slot = line as usize;
        let Some(held) = g.lines.get_mut(slot) else {
            return;
        };
        let Some(at) = held.sharers.iter().position(|b| b.entry.handle == handle) else {
            return;
        };
        let gone = held.sharers.remove(at);
        if held.sharers.is_empty() {
            held.exclusive = false;
            self.bound[slot].store(false, Ordering::SeqCst);
            self.quarantined[slot].store(false, Ordering::SeqCst);
            self.storm_fire_count[slot].store(0, Ordering::SeqCst);
            self.storm_window_start_ns[slot].store(0, Ordering::SeqCst);
            return;
        }
        let fires = self.fires[slot].load(Ordering::SeqCst);
        if gone.armed.load(Ordering::SeqCst) != fires {
            // Best-effort like every re-arm: a refusal leaves the line as it
            // was and each sharer's wait stays bounded by its deadline.
            let _ = self.unmask_if_armed(g, line, fires, controller);
        }
    }

    /// Snapshot of an entry by handle, for diagnostic / audit
    /// emission only. Returns `None` if the handle is unknown.
    #[must_use]
    pub fn lookup(&self, handle: IrqHandle) -> Option<IrqEntry> {
        self.inner
            .read()
            .binding(handle)
            .map(|binding| binding.entry)
    }

    /// Whether `handle`'s binding has a fire it has not taken.
    ///
    /// Read-only: only [`Self::try_wait_step`] takes a fire. `false` for an
    /// unknown handle. Safe from any context, alongside an in-flight
    /// [`Self::fire`].
    #[must_use]
    pub fn ready_for(&self, handle: IrqHandle) -> bool {
        let g = self.inner.read();
        g.binding(handle).is_some_and(|binding| {
            self.fires[binding.entry.line as usize].load(Ordering::SeqCst)
                != binding.taken.load(Ordering::SeqCst)
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec::Vec;
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::{Cell, RefCell};

    extern crate std;

    std::thread_local! {
        /// Whether this thread's allocations are refused.
        static STARVED: Cell<bool> = const { Cell::new(false) };
    }

    /// The system allocator, refusing every allocation of a thread that
    /// asked to be starved, so a bind with no memory to record it in is seen
    /// refused rather than aborting the process.
    struct Starving;

    // SAFETY: every method forwards to `System`, which upholds the contract,
    // or returns null, which the contract allows for any request.
    unsafe impl GlobalAlloc for Starving {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if STARVED.with(Cell::get) {
                return core::ptr::null_mut();
            }
            // SAFETY: forwarded; the caller upholds the layout contract.
            unsafe { std::alloc::System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from `System` with `layout`, as no starved
            // allocation ever returned one.
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Starving = Starving;

    /// Run `f` with this thread's allocations refused.
    fn starved<T>(f: impl FnOnce() -> T) -> T {
        STARVED.with(|starved| starved.set(true));
        let out = f();
        STARVED.with(|starved| starved.set(false));
        out
    }

    #[test]
    fn a_bind_with_nothing_to_record_it_in_is_refused_and_leaves_no_trace() {
        let t = IrqTable::new(15);
        assert_eq!(
            starved(|| t.bind(9, ProcessId(1))),
            Err(IrqError::Exhausted)
        );
        assert!(t.is_empty());
        assert_eq!(t.owner_of_line(9), None);
        assert!(t.bind(9, ProcessId(1)).is_ok(), "and binds once it can");
    }

    #[test]
    fn a_kernel_service_s_line_takes_no_other_sharer() {
        let t = IrqTable::new(15);
        let service = t.bind_exclusive(7, ProcessId(0x5b6)).expect("binds");
        assert_eq!(
            t.bind(7, ProcessId(0x1_0000)),
            Err(IrqError::LineAlreadyBound),
            "a sharer that never waited would hold it masked"
        );
        assert!(t.release_binding(service.handle, ProcessId(0x5b6), &UnsupportedController));
        assert!(t.bind(7, ProcessId(0x1_0000)).is_ok(), "free once it goes");
        assert_eq!(
            t.bind_exclusive(7, ProcessId(0x5b6)),
            Err(IrqError::LineAlreadyBound),
            "and a service takes no line another binds"
        );
    }

    #[test]
    fn a_controller_naming_no_trigger_gives_every_line_a_level() {
        assert_eq!(UnsupportedController.set_trigger(4, Trigger::Level), Ok(()));
        assert_eq!(
            UnsupportedController.set_trigger(4, Trigger::Edge),
            Err(MaskError::Unsupported)
        );
    }

    /// Deterministic mock controller. Records the sequence of
    /// `mask(line)` calls so tests can assert ordering against
    /// table state changes (the mask-before-wake invariant).
    struct MockController {
        calls: RefCell<Vec<u32>>,
        rearms: RefCell<Vec<u32>>,
        unsupported: bool,
        out_of_range_above: Option<u32>,
    }

    impl MockController {
        fn ok() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                rearms: RefCell::new(Vec::new()),
                unsupported: false,
                out_of_range_above: None,
            }
        }

        fn unsupported() -> Self {
            Self {
                unsupported: true,
                ..Self::ok()
            }
        }

        fn with_max(max: u32) -> Self {
            Self {
                out_of_range_above: Some(max),
                ..Self::ok()
            }
        }

        fn calls(&self) -> Vec<u32> {
            self.calls.borrow().clone()
        }

        fn rearms(&self) -> Vec<u32> {
            self.rearms.borrow().clone()
        }
    }

    impl IrqController for MockController {
        fn mask(&self, line: u32) -> Result<(), MaskError> {
            if self.unsupported {
                return Err(MaskError::Unsupported);
            }
            if let Some(max) = self.out_of_range_above {
                if line > max {
                    return Err(MaskError::OutOfRange);
                }
            }
            self.calls.borrow_mut().push(line);
            Ok(())
        }

        fn rearm(&self, line: u32) -> Result<(), MaskError> {
            self.rearms.borrow_mut().push(line);
            Ok(())
        }
    }

    #[test]
    fn bind_mints_handle_and_records_owner() {
        let t = IrqTable::new(31);
        let out = t.bind(7, ProcessId(42)).expect("bind");
        assert_eq!(out.line, 7);
        assert_ne!(out.handle, IrqHandle::INVALID);
        let entry = t.lookup(out.handle).expect("present");
        assert_eq!(entry.line, 7);
        assert_eq!(entry.owner, ProcessId(42));
        assert!(!t.ready_for(out.handle));
    }

    #[test]
    fn owner_of_line_names_the_bound_task_and_is_none_when_unbound() {
        let t = IrqTable::new(31);
        // Unbound line: no owner.
        assert_eq!(t.owner_of_line(7), None);
        let _ = t.bind(7, ProcessId(42)).expect("bind");
        // Bound line: names the owner, without a caller check (any reader).
        assert_eq!(t.owner_of_line(7), Some(ProcessId(42)));
        // A different, still-unbound line stays None.
        assert_eq!(t.owner_of_line(8), None);
        // Releasing the owner's bindings makes the line unbound again.
        let _ = t.release_for(ProcessId(42), &MockController::ok());
        assert_eq!(t.owner_of_line(7), None);
    }

    #[test]
    fn records_report_bound_lines_with_counts_owners_and_paging() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let _ = t.bind(5, ProcessId(7)).expect("bind 5");
        let _ = t.bind(10, ProcessId(9)).expect("bind 10");
        // Fire line 5 three times, line 10 once; each record counts every
        // edge on its line.
        for _ in 0..3 {
            t.fire(5, &ctl).expect("fire 5");
        }
        t.fire(10, &ctl).expect("fire 10");

        // The whole table: two records in ascending line order.
        let blob = t.records(0, 16);
        assert_eq!(blob.len(), 2 * IrqRecord::WIRE_LEN);
        let first = IrqRecord::from_bytes(&blob[..IrqRecord::WIRE_LEN]).expect("decode 0");
        let second = IrqRecord::from_bytes(&blob[IrqRecord::WIRE_LEN..]).expect("decode 1");
        assert_eq!(first.line, 5);
        assert_eq!(first.owner, 7);
        assert_eq!(first.count, 3);
        assert!(!first.is_quarantined());
        assert_eq!(second.line, 10);
        assert_eq!(second.owner, 9);
        assert_eq!(second.count, 1);

        // Paging: skip the first record, then a window past the end
        // terminates empty.
        let page = t.records(1, 16);
        assert_eq!(page.len(), IrqRecord::WIRE_LEN);
        let only = IrqRecord::from_bytes(&page).expect("decode page");
        assert_eq!(only.line, 10);
        assert!(t.records(2, 16).is_empty());
        assert!(t.records(99, 16).is_empty());

        // A limit of one takes only the first record.
        let capped = t.records(0, 1);
        assert_eq!(capped.len(), IrqRecord::WIRE_LEN);
    }

    #[test]
    fn records_report_a_quarantined_line() {
        let t = IrqTable::new(31);
        t.set_clock(leak_clock(0)).expect("clock");
        let ctl = MockController::ok();
        let _ = t.bind(5, ProcessId(7)).expect("bind");
        // Drive the line past its storm budget within one window so it
        // quarantines; the record must carry the flag.
        for _ in 0..=STORM_FIRE_BUDGET {
            let _ = t.fire(5, &ctl);
        }
        let blob = t.records(0, 16);
        let record = IrqRecord::from_bytes(&blob[..IrqRecord::WIRE_LEN]).expect("decode");
        assert_eq!(record.line, 5);
        assert!(record.is_quarantined());
        // Every edge is still counted, quarantined or not.
        assert!(record.count >= u64::from(STORM_FIRE_BUDGET));
    }

    #[test]
    fn an_owner_binds_a_line_once_and_others_share_it() {
        let t = IrqTable::new(31);
        let _ = t.bind(7, ProcessId(1)).unwrap();
        assert_eq!(t.bind(7, ProcessId(1)), Err(IrqError::LineAlreadyBound));
        let _ = t
            .bind(7, ProcessId(2))
            .expect("a second owner shares the line");
        assert_eq!(t.len(), 2);
        assert_eq!(t.owner_of_line(7), Some(ProcessId(1)), "the earliest");
        let blob = t.records(0, 16);
        assert_eq!(blob.len(), 2 * IrqRecord::WIRE_LEN, "one record each");
        let second = IrqRecord::from_bytes(&blob[IrqRecord::WIRE_LEN..]).expect("decode");
        assert_eq!((second.line, second.owner), (7, 2));
    }

    #[test]
    fn a_shared_line_wakes_every_sharer_and_unmasks_once_all_are_back() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let a = t.bind(7, ProcessId(1)).unwrap().handle;
        let b = t.bind(7, ProcessId(2)).unwrap().handle;
        assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Marked));
        assert_eq!(t.try_wait_step(a, ProcessId(1), 0, 1), WaitStep::Ready);
        assert_eq!(t.try_wait_step(b, ProcessId(2), 0, 1), WaitStep::Ready);
        assert_eq!(t.try_wait_step(a, ProcessId(1), 0, 1), WaitStep::Continue);
        assert_eq!(t.rearm(a, ProcessId(1), &ctl), Some(Ok(())));
        assert!(ctl.rearms().is_empty(), "the other sharer is still busy");
        assert_eq!(t.try_wait_step(b, ProcessId(2), 0, 1), WaitStep::Continue);
        assert_eq!(t.rearm(b, ProcessId(2), &ctl), Some(Ok(())));
        assert_eq!(ctl.rearms(), std::vec![7]);
    }

    #[test]
    fn a_binding_with_a_fire_to_take_rearms_nothing() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let a = t.bind(7, ProcessId(1)).unwrap().handle;
        t.fire(7, &ctl).unwrap();
        assert_eq!(t.rearm(a, ProcessId(1), &ctl), Some(Ok(())));
        assert!(ctl.rearms().is_empty(), "it is about to wake");
        assert_eq!(t.try_wait_step(a, ProcessId(1), 0, 1), WaitStep::Ready);
        assert_eq!(t.rearm(a, ProcessId(1), &ctl), Some(Ok(())));
        assert_eq!(ctl.rearms(), std::vec![7]);
        assert_eq!(t.rearm(a, ProcessId(2), &ctl), None, "another task's");
    }

    #[test]
    fn a_late_sharer_takes_no_earlier_fire() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let a = t.bind(7, ProcessId(1)).unwrap().handle;
        t.fire(7, &ctl).unwrap();
        let b = t.bind(7, ProcessId(2)).unwrap().handle;
        assert_eq!(t.try_wait_step(b, ProcessId(2), 0, 1), WaitStep::Continue);
        assert_eq!(t.try_wait_step(a, ProcessId(1), 0, 1), WaitStep::Ready);
    }

    #[test]
    fn a_departing_sharer_that_held_the_line_masked_lets_the_others_through() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let a = t.bind(7, ProcessId(1)).unwrap().handle;
        let b = t.bind(7, ProcessId(2)).unwrap().handle;
        t.fire(7, &ctl).unwrap();
        assert_eq!(t.try_wait_step(a, ProcessId(1), 0, 1), WaitStep::Ready);
        let _ = t.rearm(a, ProcessId(1), &ctl);
        assert!(ctl.rearms().is_empty());
        assert!(t.release_binding(b, ProcessId(2), &ctl));
        assert_eq!(ctl.rearms(), std::vec![7], "the departed sharer owed it");
    }

    #[test]
    fn a_departing_sharer_that_owed_nothing_unmasks_nothing() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let a = t.bind(7, ProcessId(1)).unwrap().handle;
        let _ = t.bind(7, ProcessId(2)).unwrap();
        let _ = t.rearm(a, ProcessId(1), &ctl);
        assert_eq!(ctl.rearms(), std::vec![7], "both armed from the start");
        assert_eq!(t.release_for(ProcessId(2), &ctl).released, 1);
        assert_eq!(ctl.rearms(), std::vec![7]);
        assert_eq!(t.owner_of_line(7), Some(ProcessId(1)));
    }

    #[test]
    fn a_quarantined_line_is_never_rearmed() {
        let t = IrqTable::new(31);
        t.set_clock(leak_clock(0)).expect("clock");
        let ctl = MockController::ok();
        let a = t.bind(7, ProcessId(1)).unwrap().handle;
        for _ in 0..=STORM_FIRE_BUDGET {
            let _ = t.fire(7, &ctl);
        }
        let _ = t.try_wait_step(a, ProcessId(1), 0, 1);
        assert_eq!(t.rearm(a, ProcessId(1), &ctl), Some(Ok(())));
        assert!(ctl.rearms().is_empty());
    }

    #[test]
    fn bind_refuses_out_of_range_line() {
        let t = IrqTable::new(15);
        assert_eq!(t.bind(16, ProcessId(1)), Err(IrqError::LineOutOfRange));
        // Boundary case: max_line itself is accepted.
        let _ = t.bind(15, ProcessId(1)).expect("boundary accepted");
    }

    #[test]
    fn try_wait_step_returns_continue_when_no_ready_and_not_expired() {
        let t = IrqTable::new(31);
        let out = t.bind(7, ProcessId(42)).unwrap();
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(42), 0, 1_000),
            WaitStep::Continue
        );
    }

    #[test]
    fn try_wait_step_returns_ready_after_fire_and_consumes_flag() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let out = t.bind(7, ProcessId(42)).unwrap();
        assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Marked));
        // The first poll takes the fire.
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(42), 0, 1_000),
            WaitStep::Ready
        );
        // Second poll without another fire is Continue.
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(42), 0, 1_000),
            WaitStep::Continue
        );
    }

    #[test]
    fn try_wait_step_returns_timed_out_when_now_meets_deadline() {
        let t = IrqTable::new(31);
        let out = t.bind(7, ProcessId(42)).unwrap();
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(42), 1_000, 1_000),
            WaitStep::TimedOut
        );
    }

    #[test]
    fn try_wait_step_returns_not_found_on_forged_handle() {
        let t = IrqTable::new(31);
        // No bind: any handle is unknown.
        assert_eq!(
            t.try_wait_step(IrqHandle::from_raw(0xDEAD_BEEF), ProcessId(42), 0, 1_000),
            WaitStep::NotFound
        );
    }

    #[test]
    fn try_wait_step_returns_not_found_on_handle_minted_for_another_task() {
        let t = IrqTable::new(31);
        let out = t.bind(7, ProcessId(42)).unwrap();
        // Same handle, different caller — forgery defence.
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(99), 0, 1_000),
            WaitStep::NotFound
        );
    }

    #[test]
    fn ready_beats_timeout_in_a_tie() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let out = t.bind(7, ProcessId(42)).unwrap();
        t.fire(7, &ctl).unwrap();
        // The wake-up happened; even though `now == deadline` we
        // must surface `Ready` rather than `TimedOut`.
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(42), 1_000, 1_000),
            WaitStep::Ready
        );
    }

    #[test]
    fn fire_returns_stray_when_no_binding_but_still_masks() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Stray));
        // Mask still happened — the controller-level write is
        // load-bearing even for stray edges.
        assert_eq!(ctl.calls(), std::vec![7]);
    }

    #[test]
    fn fire_returns_arch_unsupported_when_controller_unsupported() {
        let t = IrqTable::new(31);
        let ctl = MockController::unsupported();
        let _ = t.bind(7, ProcessId(42)).unwrap();
        assert_eq!(t.fire(7, &ctl), Err(IrqError::ArchUnsupported));
    }

    #[test]
    fn fire_returns_out_of_range_when_controller_rejects_line() {
        let t = IrqTable::new(31);
        let ctl = MockController::with_max(5);
        assert_eq!(t.fire(7, &ctl), Err(IrqError::LineOutOfRange));
    }

    #[test]
    fn mask_is_observed_before_wake() {
        // The binding must not read as ready while the controller's mask is
        // still in flight.
        struct OrderingProbe<'a> {
            table: &'a IrqTable,
            handle: IrqHandle,
            observed_ready_during_mask: RefCell<Option<bool>>,
        }
        impl IrqController for OrderingProbe<'_> {
            fn mask(&self, _line: u32) -> Result<(), MaskError> {
                *self.observed_ready_during_mask.borrow_mut() =
                    Some(self.table.ready_for(self.handle));
                Ok(())
            }
        }
        let t = IrqTable::new(31);
        let handle = t.bind(7, ProcessId(42)).unwrap().handle;
        let probe = OrderingProbe {
            table: &t,
            handle,
            observed_ready_during_mask: RefCell::new(None),
        };
        t.fire(7, &probe).unwrap();
        assert_eq!(
            *probe.observed_ready_during_mask.borrow(),
            Some(false),
            "not ready while the mask is executing"
        );
        assert!(t.ready_for(handle));
    }

    #[test]
    fn release_for_evicts_bindings_and_returns_subsequent_wait_with_not_found() {
        let t = IrqTable::new(31);
        let a = t.bind(7, ProcessId(42)).unwrap();
        let b = t.bind(9, ProcessId(42)).unwrap();
        let c = t.bind(10, ProcessId(99)).unwrap();
        let ctl = MockController::ok();
        assert_eq!(t.release_for(ProcessId(42), &ctl).released, 2);
        // Releases are idempotent.
        assert_eq!(t.release_for(ProcessId(42), &ctl).released, 0);
        // 42's handles are now unknown.
        assert_eq!(
            t.try_wait_step(a.handle, ProcessId(42), 0, 1_000),
            WaitStep::NotFound
        );
        assert_eq!(
            t.try_wait_step(b.handle, ProcessId(42), 0, 1_000),
            WaitStep::NotFound
        );
        // 99's binding survives.
        assert_eq!(
            t.try_wait_step(c.handle, ProcessId(99), 0, 1_000),
            WaitStep::Continue
        );
    }

    #[test]
    fn the_owner_cursor_visits_only_the_owners_bindings_in_line_order() {
        let t = IrqTable::new(31);
        let low = t.bind(3, ProcessId(42)).unwrap();
        let _ = t.bind(5, ProcessId(99)).unwrap();
        let high = t.bind(9, ProcessId(42)).unwrap();

        let first = t.next_binding_of(ProcessId(42), None).unwrap();
        assert_eq!(first.handle, low.handle);
        let second = t.next_binding_of(ProcessId(42), Some(first.line)).unwrap();
        assert_eq!(second.handle, high.handle);
        assert_eq!(t.next_binding_of(ProcessId(42), Some(second.line)), None);
        assert_eq!(t.next_binding_of(ProcessId(42), Some(u32::MAX)), None);
    }

    #[test]
    fn a_released_binding_fails_its_wait_and_frees_its_line() {
        let t = IrqTable::new(31);
        let mine = t.bind(7, ProcessId(42)).unwrap();
        let other = t.bind(8, ProcessId(42)).unwrap();
        let controller = MockController::ok();

        assert!(
            !t.release_binding(mine.handle, ProcessId(99), &controller),
            "another owner releases nothing"
        );
        assert!(t.release_binding(mine.handle, ProcessId(42), &controller));
        assert!(
            !t.release_binding(mine.handle, ProcessId(42), &controller),
            "idempotent"
        );
        assert_eq!(
            t.try_wait_step(mine.handle, ProcessId(42), 0, 1_000),
            WaitStep::NotFound
        );
        assert_eq!(
            t.try_wait_step(other.handle, ProcessId(42), 0, 1_000),
            WaitStep::Continue,
            "only the named binding goes"
        );
        assert_eq!(t.fire(7, &controller), Ok(FireOutcome::Stray), "unbound");
        assert!(
            t.bind(7, ProcessId(42)).is_ok(),
            "its owner may bind it again"
        );
    }

    #[test]
    fn a_stale_handle_does_not_release_the_line_rebound_under_a_new_one() {
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let old = t.bind(7, ProcessId(42)).unwrap();
        assert!(t.release_binding(old.handle, ProcessId(42), &ctl));
        let new = t.bind(7, ProcessId(42)).unwrap();
        assert!(!t.release_binding(old.handle, ProcessId(42), &ctl));
        assert_eq!(
            t.try_wait_step(new.handle, ProcessId(42), 0, 1_000),
            WaitStep::Continue
        );
    }

    #[test]
    fn lookup_returns_none_for_unknown_handle() {
        let t = IrqTable::new(31);
        assert!(t.lookup(IrqHandle::from_raw(0xDEAD)).is_none());
    }

    #[test]
    fn len_and_is_empty_track_bindings() {
        let t = IrqTable::new(31);
        assert!(t.is_empty());
        let _ = t.bind(7, ProcessId(1)).unwrap();
        assert_eq!(t.len(), 1);
        assert!(!t.is_empty());
        let _ = t.bind(8, ProcessId(2)).unwrap();
        assert_eq!(t.len(), 2);
        let ctl = MockController::ok();
        t.release_for(ProcessId(1), &ctl);
        assert_eq!(t.len(), 1);
        t.release_for(ProcessId(2), &ctl);
        assert!(t.is_empty());
    }

    #[test]
    fn max_line_is_reported() {
        let t = IrqTable::new(23);
        assert_eq!(t.max_line(), 23);
    }

    #[test]
    fn handles_are_unique_across_rebinds() {
        let t = IrqTable::new(31);
        let a = t.bind(7, ProcessId(1)).unwrap();
        t.release_for(ProcessId(1), &MockController::ok());
        let b = t.bind(7, ProcessId(1)).unwrap();
        assert_ne!(a.handle, b.handle, "fresh bind must mint a fresh handle");
    }

    use core::sync::atomic::{AtomicU32, AtomicU64};

    /// Test observer: counts calls and remembers the last line, so a test can
    /// assert `fire` notified it. Interior-atomic so it is `Sync`, mirroring
    /// the production entropy observer's shape.
    struct CountingObserver {
        calls: AtomicU32,
        last_line: AtomicU64,
    }

    impl CountingObserver {
        fn new() -> Self {
            Self {
                calls: AtomicU32::new(0),
                last_line: AtomicU64::new(u64::MAX),
            }
        }
    }

    impl IrqDispatchObserver for CountingObserver {
        fn on_irq(&self, line: u32) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.last_line.store(u64::from(line), Ordering::Relaxed);
        }
    }

    #[test]
    fn observer_is_notified_on_every_fire_including_strays() {
        let t = IrqTable::new(31);
        let obs: &'static CountingObserver =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(CountingObserver::new()));
        t.set_observer(obs).expect("first install succeeds");
        let ctl = MockController::ok();
        // Bound line: fire notifies the observer.
        let _ = t.bind(7, ProcessId(1)).unwrap();
        t.fire(7, &ctl).expect("fire bound line");
        assert_eq!(obs.calls.load(Ordering::Relaxed), 1);
        assert_eq!(obs.last_line.load(Ordering::Relaxed), 7);
        // Stray (unbound) line: still an arrival, still fed to the observer.
        assert_eq!(t.fire(9, &ctl), Ok(FireOutcome::Stray));
        assert_eq!(obs.calls.load(Ordering::Relaxed), 2);
        assert_eq!(obs.last_line.load(Ordering::Relaxed), 9);
    }

    #[test]
    fn set_observer_is_set_once() {
        let t = IrqTable::new(31);
        let a: &'static CountingObserver =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(CountingObserver::new()));
        let b: &'static CountingObserver =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(CountingObserver::new()));
        assert_eq!(t.set_observer(a), Ok(()));
        assert_eq!(t.set_observer(b), Err(ObserverAlreadyInstalled));
    }

    #[test]
    fn fire_without_observer_is_a_noop() {
        // The observer is optional: a table with none installed fires exactly
        // as before (no panic, correct outcome).
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let _ = t.bind(3, ProcessId(1)).unwrap();
        assert_eq!(t.fire(3, &ctl), Ok(FireOutcome::Marked));
    }

    /// Test monotonic clock with a settable time, so the runaway-line
    /// rate accounting can be driven deterministically. Interior-atomic so
    /// it is `Sync`, mirroring the production clock's shape.
    struct MockClock {
        now: AtomicU64,
    }

    impl MockClock {
        fn at(now: u64) -> Self {
            Self {
                now: AtomicU64::new(now),
            }
        }
        fn set(&self, now: u64) {
            self.now.store(now, Ordering::Relaxed);
        }
    }

    impl MonotonicClock for MockClock {
        fn now_ns(&self) -> u64 {
            self.now.load(Ordering::Relaxed)
        }
    }

    fn leak_clock(now: u64) -> &'static MockClock {
        alloc::boxed::Box::leak(alloc::boxed::Box::new(MockClock::at(now)))
    }

    #[test]
    fn without_a_clock_a_line_is_never_quarantined() {
        // Rate accounting is inert until a clock is installed: even far past
        // the budget every fire is delivered exactly as before.
        let t = IrqTable::new(31);
        let ctl = MockController::ok();
        let out = t.bind(7, ProcessId(1)).unwrap();
        for _ in 0..(STORM_FIRE_BUDGET + 10) {
            assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Marked));
        }
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(1), 0, u64::MAX),
            WaitStep::Ready
        );
        assert!(!t.is_quarantined(out.handle, ProcessId(1)));
    }

    #[test]
    fn a_line_firing_past_its_budget_in_one_window_is_quarantined() {
        let t = IrqTable::new(31);
        t.set_clock(leak_clock(0)).expect("clock installs once");
        let ctl = MockController::ok();
        let out = t.bind(7, ProcessId(1)).unwrap();
        // Every fire lands at the same instant (one window). The budget-th
        // is still delivered; the one past it trips the quarantine.
        for _ in 0..STORM_FIRE_BUDGET {
            assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Marked));
        }
        assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Quarantined));
        // The tripping fire delivered no wake, and the waiter observes the
        // terminal, fail-closed quarantine rather than a ready or a spin.
        assert!(t.is_quarantined(out.handle, ProcessId(1)));
        assert_eq!(
            t.try_wait_step(out.handle, ProcessId(1), 0, u64::MAX),
            WaitStep::Quarantined
        );
        // A quarantined line keeps reporting quarantined and never re-delivers.
        assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Quarantined));
    }

    #[test]
    fn fires_spread_across_windows_never_trip_the_budget() {
        // A busy-but-healthy line: it fires the full budget, then the window
        // rolls over and the count resets, so it is never quarantined.
        let t = IrqTable::new(31);
        let clock = leak_clock(0);
        t.set_clock(clock).expect("clock installs once");
        let ctl = MockController::ok();
        let out = t.bind(7, ProcessId(1)).unwrap();
        for window in 0..4u64 {
            clock.set(window * STORM_WINDOW_NS);
            for _ in 0..STORM_FIRE_BUDGET {
                assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Marked));
                // Take each fire, as a driver servicing every interrupt does.
                let _ = t.try_wait_step(out.handle, ProcessId(1), 0, u64::MAX);
            }
        }
        assert!(!t.is_quarantined(out.handle, ProcessId(1)));
    }

    #[test]
    fn a_fresh_bind_clears_a_previous_quarantine() {
        let t = IrqTable::new(31);
        t.set_clock(leak_clock(0)).expect("clock installs once");
        let ctl = MockController::ok();
        let first = t.bind(7, ProcessId(1)).unwrap();
        for _ in 0..=STORM_FIRE_BUDGET {
            let _ = t.fire(7, &ctl);
        }
        assert!(t.is_quarantined(first.handle, ProcessId(1)));
        // The driver releases and rebinds the line to recover it.
        t.release_for(ProcessId(1), &ctl);
        let second = t.bind(7, ProcessId(1)).unwrap();
        assert!(!t.is_quarantined(second.handle, ProcessId(1)));
        assert_eq!(t.fire(7, &ctl), Ok(FireOutcome::Marked));
        assert_eq!(
            t.try_wait_step(second.handle, ProcessId(1), 0, u64::MAX),
            WaitStep::Ready
        );
    }

    #[test]
    fn is_quarantined_owner_checks_and_ignores_unknown_handles() {
        let t = IrqTable::new(31);
        t.set_clock(leak_clock(0)).expect("clock installs once");
        let ctl = MockController::ok();
        let out = t.bind(7, ProcessId(1)).unwrap();
        for _ in 0..=STORM_FIRE_BUDGET {
            let _ = t.fire(7, &ctl);
        }
        // Owner sees the quarantine; a foreign task and a forged handle do
        // not (identify before acting).
        assert!(t.is_quarantined(out.handle, ProcessId(1)));
        assert!(!t.is_quarantined(out.handle, ProcessId(2)));
        assert!(!t.is_quarantined(IrqHandle::from_raw(0xDEAD), ProcessId(1)));
    }

    #[test]
    fn set_clock_is_set_once() {
        let t = IrqTable::new(31);
        assert_eq!(t.set_clock(leak_clock(0)), Ok(()));
        assert_eq!(t.set_clock(leak_clock(0)), Err(ObserverAlreadyInstalled));
    }

    /// Two sharers coming back at once each store their own arm before they
    /// read the other's, so at least one of them unmasks the line: the
    /// store-buffering pairing in `unmask_if_armed`, run on real threads
    /// because loom cannot build the kernel crate graph.
    #[test]
    fn two_sharers_returning_at_once_never_both_leave_the_line_masked() {
        use std::sync::Arc;

        struct Counting(AtomicU64);
        impl IrqController for Counting {
            fn mask(&self, _line: u32) -> Result<(), MaskError> {
                Ok(())
            }
            fn rearm(&self, _line: u32) -> Result<(), MaskError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        const ROUNDS: u64 = 400_000;
        let table = Arc::new(IrqTable::new(7));
        let ctl = Arc::new(Counting(AtomicU64::new(0)));
        let owners = [ProcessId(1), ProcessId(2)];
        let handles = owners.map(|owner| table.bind(1, owner).unwrap().handle);
        let round = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicU64::new(0));
        let workers: Vec<_> = (0..2)
            .map(|i| {
                let (table, ctl, round, done) =
                    (table.clone(), ctl.clone(), round.clone(), done.clone());
                let (handle, owner) = (handles[i], owners[i]);
                std::thread::spawn(move || {
                    for r in 1..=ROUNDS {
                        while round.load(Ordering::Acquire) != r {
                            core::hint::spin_loop();
                        }
                        let _ = table.rearm(handle, owner, &*ctl);
                        done.fetch_add(1, Ordering::AcqRel);
                    }
                })
            })
            .collect();
        for r in 1..=ROUNDS {
            table.fire(1, &*ctl).unwrap();
            for (handle, owner) in handles.iter().zip(owners) {
                assert_eq!(table.try_wait_step(*handle, owner, 0, 1), WaitStep::Ready);
            }
            let before = ctl.0.load(Ordering::SeqCst);
            round.store(r, Ordering::Release);
            while done.load(Ordering::Acquire) != 2 * r {
                core::hint::spin_loop();
            }
            assert!(
                ctl.0.load(Ordering::SeqCst) > before,
                "round {r}: both sharers left the line masked"
            );
        }
        for worker in workers {
            worker.join().unwrap();
        }
    }
}
