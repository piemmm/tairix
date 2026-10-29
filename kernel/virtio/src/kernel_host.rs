//! In-kernel [`VirtioHost`] backed by a per-process [`DmaPool`].
//!
//! The test-only `tairix_virtio::MockHost` satisfies
//! [`tairix_virtio::VirtioHost`] by leaking `Box<[u8]>` storage; the
//! [`KernelVirtioHost`] here satisfies the same trait but routes every
//! allocation through the capability-gated [`tairix_kernel_sec::alloc_dma`]
//! / [`tairix_kernel_sec::free_dma`] pair.
//!
//! The host owns a single [`tairix_kernel_mem::DmaPool`] (per-driver
//! pool "per-process heaps, never a global user
//! heap"). Allocations:
//!
//! 1. `kernel_sec::alloc_dma` performs the `CapabilityId::MEM_DMA`
//!    check and emits the `AuditEvent::DmaAllocated` / `…Denied`
//!    record. The pool zero-fills the data slots; the bytes are
//!    therefore safe to publish into a virtio descriptor without an
//!    extra scrub pass.
//! 2. The host obtains the raw, non-null base pointer through
//!    [`DmaPool::slot_base`] and mints a [`DmaSlab`] via
//!    [`DmaSlab::from_pool`]; the slab carries a generic free shim
//!    that re-enters the host on drop and routes the buffer back
//!    through `kernel_sec::free_dma`.
//! 3. The host records the live `(slot, DmaBuffer)` pair in an
//!    internal table so the drop-path shim — which receives only
//!    `(*const(), slot, len)` — can recover the originating
//!    [`DmaBuffer`] without further state in the slab.
//!
//! [`KernelVirtioHost::notify_wait`] blocks the calling driver task until
//! the device raises its interrupt line, through the kernel's [`LineWait`]
//! on the line its bus bring-up bound, so the kernel records how the wait
//! went exactly as it does for every device it drives itself.
//!
//! # Safety
//!
//! The unsafe construction site is [`DmaSlab::from_pool`]. Every
//! invariant required by that constructor is established here:
//!
//! * `pool_ptr` is `self as *const Self as *const ()`; the slab's
//!   lifetime is bounded by `'a`, which is the lifetime of the
//!   `KernelVirtioHost`. The borrow checker enforces that no slab
//!   minted by this host outlives the host's borrow.
//! * `ptr` is the result of [`DmaPool::slot_base`], which is
//!   documented to be a non-null, page-aligned pointer covering
//!   exactly `buf.len()` disjoint bytes (witnessed by the pool's
//!   slot bitmap — one slot ↔ one allocation).
//! * The free shim is monomorphised per `(P, S)`; the cast back from
//!   `*const ()` to `*const KernelVirtioHost<'_, P, S>` is the
//!   inverse of the cast performed at construction and therefore
//!   sound.

use alloc::collections::BTreeMap;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_abi::driver::CompletionSignal;
use tairix_abi::DriverError;
use tairix_kernel_irq::LineWait;
use tairix_kernel_mem::{DmaBuffer, DmaPool, PageTable};
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::dma::{alloc_dma, free_dma, DmaGateError};
use tairix_log::Sink;
use tairix_sync::SpinLock;

use tairix_virtio::{DmaHost, DmaSlab, PoolId, SlabEnd, VirtioHost};

/// Capability-checked, [`DmaPool`]-backed [`VirtioHost`].
///
/// Generic over the page-table backend `P` (so the same code is
/// exercised by `kernel/mem::HostPageTable` in unit tests and by the
/// architecture page-table types in production) and the audit
/// [`Sink`] implementation `S`.
///
/// # Lifetime
///
/// `'a` bounds the pool's allocator borrow and the borrowed
/// [`TaskCapabilities`]. Every [`DmaSlab`] minted by this host
/// re-enters the host on drop, so by construction no slab can
/// outlive the host — Rust's borrow checker enforces this through
/// the `&'a self` borrow returned by [`Self::alloc_dma_zeroed`]:
/// the slab carries no lifetime in its type, but the slab's drop
/// would dereference `&self` if it ran after the host went away.
/// Every slab re-enters the host through its free shim on drop, so
/// all outstanding slabs must be dropped before the host (and the
/// [`DmaPool`] it owns) is dropped.
pub struct KernelVirtioHost<'a, P: PageTable, S: Sink + Sync + ?Sized> {
    /// Per-driver DMA pool, behind a [`SpinLock`] (not a `RefCell`) so the
    /// host is [`Sync`] and a `&'static` host can be shared across the
    /// tasks that share one device behind a [`crate`]-external block-sharing
    /// lock (disk access is a common, capability-checked
    /// service many tasks reach). The lock is effectively uncontended (the
    /// outer block-sharing layer already serialises every device op), so it
    /// costs nothing on the hot path.
    pool: SpinLock<DmaPool<'a, P>>,
    caller: &'a TaskCapabilities,
    audit: &'a S,
    id: PoolId,
    /// Monotonic slab-slot stamp. An [`AtomicUsize`] (not a `Cell`) so the
    /// host stays [`Sync`]; the increment is a single `fetch_add`.
    next_slot: AtomicUsize,
    /// Live `(slot → DmaBuffer)` table. The drop-path shim receives
    /// only `(*const(), slot, len)`; it recovers the originating
    /// [`DmaBuffer`] by removing the entry under `slot`. Behind a
    /// [`SpinLock`] for the same [`Sync`] reason as `pool`.
    live: SpinLock<BTreeMap<usize, DmaBuffer>>,
    /// The wait on the device's bound line, and the clock it is timed on.
    /// `Sync`, because a shared `&'static` host is reached from more than
    /// one task.
    line: &'a (dyn LineWait + Sync),
}

impl<'a, P: PageTable, S: Sink + Sync + ?Sized> KernelVirtioHost<'a, P, S> {
    /// Take ownership of a [`DmaPool`] behind a capability-checking
    /// host.
    ///
    /// The host owns the pool for its whole lifetime, so a bring-up hands
    /// each device a host of its own over a fresh pool. The pool's `'a`
    /// allocator borrow still bounds the host.
    ///
    /// `id` must be a fresh, process-unique [`PoolId`] (mint via
    /// [`PoolId::fresh`]). `caller` is the [`TaskCapabilities`] of
    /// the task that owns the per-process pool; every allocation
    /// and every drop-frees the buffer is audited against this
    /// capability set.
    ///
    /// `line` is the wait on the device's interrupt line, bound to the
    /// process that bound it, so the host can only wake on a line that
    /// process holds.
    #[must_use]
    pub fn new(
        pool: DmaPool<'a, P>,
        caller: &'a TaskCapabilities,
        audit: &'a S,
        id: PoolId,
        line: &'a (dyn LineWait + Sync),
    ) -> Self {
        Self {
            pool: SpinLock::new(pool),
            caller,
            audit,
            id,
            next_slot: AtomicUsize::new(0),
            live: SpinLock::new(BTreeMap::new()),
            line,
        }
    }

    /// Identifier this host stamps on every slab it mints.
    #[must_use]
    pub fn pool_id(&self) -> PoolId {
        self.id
    }

    /// Number of slabs this host currently has outstanding.
    ///
    /// Decremented exactly once per slab drop. Test-only consumers
    /// assert this returns zero after a transaction completes — the
    /// canonical leak check for the slab/free-shim wiring.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.live.lock().len()
    }
}

/// Drop-path shim invoked by [`DmaSlab::drop`].
///
/// `pool` is the host pointer recorded at construction time
/// (cast through `*const ()`). `slot` is the slab's unique slot
/// index within this host; `len` is the slab's byte length and is
/// retained for symmetry with [`tairix_virtio::SlabFreeFn`] — the actual
/// free goes through [`free_dma`] which keys on the [`DmaBuffer`]
/// stored in [`KernelVirtioHost::live`].
///
/// # Safety
///
/// Mirrors the [`tairix_virtio::SlabFreeFn`] contract:
///
/// * `pool` must be the `*const ()` produced by
///   `KernelVirtioHost::alloc_dma_zeroed` for some slab carrying
///   `slot`. The originating host must still be live (the slab's
///   lifetime is bounded by the host's `'a`).
/// * `slot` must be the slot index recorded on the slab. Calling
///   with a stale or unknown slot is a logic bug; the shim is
///   defensive and silently returns when no entry is found (the
///   slab will then have been double-freed, which is `Drop`'s
///   responsibility to avoid — see [`DmaSlab::drop`] running exactly
///   once).
unsafe fn slab_free_shim<P: PageTable, S: Sink + Sync + ?Sized>(
    pool: *const (),
    _cpu: core::ptr::NonNull<u8>,
    slot: usize,
    _len: usize,
    end: SlabEnd,
) {
    if end == SlabEnd::Withheld {
        return;
    }
    // SAFETY: `pool` was produced at the matching `from_pool` call by
    // casting `&KernelVirtioHost<'_, P, S>` through `*const Self as
    // *const ()`. The slab's lifetime is bounded by the host's `'a`
    // borrow, so the dereference here observes a live host of the
    // same monomorphisation.
    let host: &KernelVirtioHost<'_, P, S> =
        unsafe { &*(pool.cast::<KernelVirtioHost<'_, P, S>>()) };
    let removed = host.live.lock().remove(&slot);
    if let Some(buf) = removed {
        // `Drop` cannot propagate errors; a refusal here means the
        // task lost `CAP_MEM_DMA` between alloc and free, which
        // `free_dma` records as a `DmaAllocDenied` audit event. The
        // pool keeps the slot reserved (the supervisor process is
        // expected to reclaim it). We deliberately do not retry —
        // the charter forbids retry-until-it-works.
        let _ = free_dma(&mut *host.pool.lock(), host.caller, buf, host.audit);
    }
}

impl<P: PageTable, S: Sink + Sync + ?Sized> DmaHost for KernelVirtioHost<'_, P, S> {
    fn alloc_dma_zeroed(&self, size: usize) -> Result<DmaSlab, DriverError> {
        if size == 0 {
            return Err(DriverError::BufferTooSmall);
        }
        let buf = {
            let mut pool = self.pool.lock();
            alloc_dma(&mut *pool, self.caller, size, 0, self.audit)
                .map_err(DmaGateError::as_driver_error)?
        };
        // `slot_base` returns the data-region base for the buffer.
        // It cannot fail for a buffer minted from this pool one
        // statement above, but the result is plumbed through to
        // surface any allocator-internal inconsistency as a
        // `DriverError` instead of a panic.
        let base: NonNull<u8> = if let Ok(p) = self.pool.lock().slot_base(&buf) {
            p
        } else {
            // Roll back the allocation rather than leak the buffer;
            // this is fail-closed. Cannot happen
            // in practice for a buffer minted one statement above,
            // but the recovery path keeps satisfied
            // without an `expect`.
            let _ = free_dma(&mut *self.pool.lock(), self.caller, buf, self.audit);
            return Err(DriverError::LengthOutOfRange);
        };
        // A single atomic stamp; `Relaxed` is sufficient because the slot is
        // only an opaque per-host key (the outer block-sharing lock already
        // serialises device ops, so there is no cross-slab ordering to
        // establish here).
        let slot = self.next_slot.fetch_add(1, Ordering::Relaxed);
        self.live.lock().insert(slot, buf);
        let device_addr = buf.device_addr();
        let len = buf.len();
        let pool_ptr: *const () = core::ptr::from_ref::<Self>(self).cast::<()>();
        // SAFETY: as discussed in the module-level comment —
        // (i) `base` is non-null and covers `len` disjoint bytes
        //     (witnessed by the pool's slot bitmap);
        // (ii) `pool_ptr` outlives the slab because the slab carries
        //     no lifetime in its type but the slab's drop shim
        //     dereferences `&self`, which the borrow checker
        //     enforces via the `&'a self` return path;
        // (iii) `device_addr` is the device-visible base of `base[0]`,
        //     produced by `DmaPool::alloc`: its IOVA on a translated pool.
        let slab = unsafe {
            DmaSlab::from_pool(
                device_addr,
                base,
                len,
                self.id,
                slot,
                pool_ptr,
                slab_free_shim::<P, S>,
            )
        };
        Ok(slab)
    }

    /// Nothing to release: this pool is the kernel's own and outlives any
    /// device instance that uses it.
    fn device_quiesced(&self) {}
}

impl<P: PageTable, S: Sink + Sync + ?Sized> VirtioHost for KernelVirtioHost<'_, P, S> {
    fn notify_wait(&self, _queue_index: u16, timeout_ns: u64) -> CompletionSignal {
        // One line serves every queue, so the driver re-scans every used ring
        // on a wake and the queue is no part of the wait. A finite budget
        // means a request is outstanding, which makes a quiet line a fault.
        let outcome = if timeout_ns == u64::MAX {
            self.line.await_event(timeout_ns)
        } else {
            self.line.await_completion(timeout_ns)
        };
        outcome.into()
    }

    fn now_ns(&self) -> u64 {
        self.line.now_ns()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicU32, AtomicU64};
    use tairix_abi::CapabilityId;
    use tairix_abi::IrqHandle;
    use tairix_caps::CapabilitySet;
    use tairix_kernel_irq::{
        block_until_ready, IrqController, IrqTable, IrqWaitAbort, IrqWaiter, MaskError, WaitOutcome,
    };
    use tairix_kernel_mem::{
        bootinfo::{BootMemoryMap, MemoryRegion, RegionKind},
        AddressSpace, FrameAllocator, HostPageTable, PhysAddr, SimPhysMap, VirtAddr, PAGE_SIZE,
    };
    use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
    use tairix_kernel_sec::identity::UserId;
    use tairix_log::{Event, Sink};

    /// Owner process id every fixture binds the device line against, waits
    /// on, and derives its caller with ([`task_with`]).
    const OWNER: ProcessId = ProcessId(99);

    /// Permissive controller so [`IrqTable::fire`] can mask and set
    /// the ready flag without an architecture port.
    struct OkController;
    impl IrqController for OkController {
        fn mask(&self, _line: u32) -> Result<(), MaskError> {
            Ok(())
        }
    }

    /// Deterministic [`LineWait`] for the host tests: the shared wait loop
    /// over `handle`, counting which wait it was asked for.
    ///
    /// `now_ns` advances one tick per park, so a caller's finite budget
    /// expires after that many parks — the silent-device path. When
    /// `fire_line` is set, the waiter fires that line on the
    /// `fire_after`-th park — the "device raises its line while the
    /// driver is parked" path. `parked_until` records the deadline the
    /// wait loop handed to the most recent park, so a test can assert the
    /// caller's budget reaches the park that has to honour it.
    struct TestWaiter<'a> {
        table: &'a IrqTable,
        handle: IrqHandle,
        controller: OkController,
        fire_line: Option<u32>,
        fire_after: u32,
        yields: AtomicU32,
        now: AtomicU64,
        parked_until: AtomicU64,
        completions: AtomicU32,
        events: AtomicU32,
    }

    impl<'a> TestWaiter<'a> {
        /// Waiter on `handle` that never fires.
        fn idle(table: &'a IrqTable, handle: IrqHandle) -> Self {
            Self {
                table,
                handle,
                controller: OkController,
                fire_line: None,
                fire_after: 0,
                yields: AtomicU32::new(0),
                now: AtomicU64::new(0),
                parked_until: AtomicU64::new(0),
                completions: AtomicU32::new(0),
                events: AtomicU32::new(0),
            }
        }

        /// Waiter on `handle` that fires `line` on the `after`-th
        /// cooperative yield.
        fn firing(table: &'a IrqTable, handle: IrqHandle, line: u32, after: u32) -> Self {
            Self {
                fire_line: Some(line),
                fire_after: after,
                ..Self::idle(table, handle)
            }
        }

        fn yields(&self) -> u32 {
            self.yields.load(Ordering::Relaxed)
        }

        fn parked_until(&self) -> u64 {
            self.parked_until.load(Ordering::Relaxed)
        }

        /// How many completion and event waits were asked for.
        fn waits(&self) -> (u32, u32) {
            (
                self.completions.load(Ordering::Relaxed),
                self.events.load(Ordering::Relaxed),
            )
        }
    }

    impl LineWait for TestWaiter<'_> {
        fn await_completion(&self, budget_ns: u64) -> WaitOutcome {
            self.completions.fetch_add(1, Ordering::Relaxed);
            block_until_ready(self.table, self.handle, OWNER, budget_ns, self)
        }

        fn await_event(&self, timeout_ns: u64) -> WaitOutcome {
            self.events.fetch_add(1, Ordering::Relaxed);
            block_until_ready(self.table, self.handle, OWNER, timeout_ns, self)
        }
    }

    impl IrqWaiter for TestWaiter<'_> {
        fn now_ns(&self) -> u64 {
            self.now.load(Ordering::Relaxed)
        }

        fn yield_now(&self, deadline_ns: u64) -> Result<(), IrqWaitAbort> {
            let n = self.yields.fetch_add(1, Ordering::Relaxed) + 1;
            self.parked_until.store(deadline_ns, Ordering::Relaxed);
            if let Some(line) = self.fire_line {
                if n == self.fire_after {
                    self.table.fire(line, &self.controller).expect("fire");
                }
            }
            self.now.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    /// Build a fresh IRQ table with the device line bound to
    /// [`OWNER`], returning the table and the minted handle.
    fn irq_binding(line: u32) -> (IrqTable, IrqHandle) {
        let table = IrqTable::new(31);
        let out = table.bind(line, OWNER).expect("bind device line");
        (table, out.handle)
    }

    /// Minimal in-memory [`Sink`] that records `(level, event-id)` for
    /// every event. Used in lieu of the kernel/sec `RecordingSink` to
    /// keep this crate from depending on `kernel/sec`'s test-only
    /// surface.
    struct Recorder {
        events: SpinLock<Vec<u32>>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                events: SpinLock::new(Vec::new()),
            }
        }
        fn ids(&self) -> Vec<u32> {
            self.events.lock().clone()
        }
    }

    impl Sink for Recorder {
        fn write_event(&self, event: &Event<'_>) {
            self.events.lock().push(event.id.0);
        }
    }

    fn small_map(usable_pages: usize) -> BootMemoryMap {
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            kind: RegionKind::Usable,
            start: PhysAddr::new(PAGE_SIZE as u64 * 16),
            length: (PAGE_SIZE * usable_pages) as u64,
        });
        m
    }

    /// Simulated physical RAM covering the frame allocator's region so
    /// the pool can reach a buffer's frames from the CPU.
    fn fresh_sim() -> SimPhysMap {
        SimPhysMap::new(PhysAddr::new(PAGE_SIZE as u64 * 16), 16 * PAGE_SIZE)
    }

    fn fresh_pool<'a>(
        frames: &'a FrameAllocator,
        sim: &'a SimPhysMap,
    ) -> DmaPool<'a, HostPageTable> {
        DmaPool::new(
            AddressSpace::new(HostPageTable::new()),
            VirtAddr::new(0x2000_0000),
            16,
            frames,
            sim,
            tairix_abi::DmaCoherence::Snooped,
        )
        .expect("pool constructs")
    }

    fn task_with(caps: &[CapabilityId], sink: &Recorder) -> TaskCapabilities {
        let mut set = CapabilitySet::empty();
        for c in caps {
            set.insert(*c);
        }
        TaskCapabilities::derive(ProcessId(99), UserId(1000), set, set, sink)
    }

    /// Event ID emitted by `kernel/sec::dma::alloc_dma` on a
    /// successful grant. Mirrored from `kernel/sec::audit` so the
    /// test asserts the observable contract without depending on
    /// the internal `AuditEvent` enum.
    const DMA_ALLOCATED_ID: u32 = 1030;
    const DMA_ALLOC_DENIED_ID: u32 = 1031;

    #[test]
    fn alloc_returns_zero_initialised_slab() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        let slab = host.alloc_dma_zeroed(PAGE_SIZE).expect("granted");
        assert_eq!(slab.len(), PAGE_SIZE);
        assert!(slab.as_bytes().iter().all(|b| *b == 0));
        assert_eq!(slab.pool_id(), host.pool_id());
        assert_ne!(host.pool_id(), PoolId::MOCK);
        // Exactly one `DmaAllocated` audit event so far.
        let dma_events: Vec<u32> = sink
            .ids()
            .into_iter()
            .filter(|id| *id == DMA_ALLOCATED_ID || *id == DMA_ALLOC_DENIED_ID)
            .collect();
        assert_eq!(dma_events, [DMA_ALLOCATED_ID]);
    }

    #[test]
    fn drop_routes_through_free_dma() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        {
            let slab = host.alloc_dma_zeroed(PAGE_SIZE).expect("granted");
            assert_eq!(host.outstanding(), 1);
            drop(slab);
        }
        assert_eq!(
            host.outstanding(),
            0,
            "the drop-path shim must remove the slot from the live table"
        );
        // After drop the pool must show zero live allocations — the
        // canonical witness that `kernel_sec::free_dma` ran.
        assert_eq!(host.pool.lock().live(), 0);
    }

    #[test]
    fn capability_missing_is_permission_denied() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        // No `MEM_DMA` capability: every allocation must fail closed.
        let caller = task_with(&[], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        let err = host.alloc_dma_zeroed(PAGE_SIZE).unwrap_err();
        assert!(matches!(err, DriverError::PermissionDenied));
        assert_eq!(host.outstanding(), 0);
        // The denial must have been audited.
        assert!(sink.ids().contains(&DMA_ALLOC_DENIED_ID));
        assert!(!sink.ids().contains(&DMA_ALLOCATED_ID));
    }

    #[test]
    fn zero_size_request_rejected_before_capability_check() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        let err = host.alloc_dma_zeroed(0).unwrap_err();
        assert!(matches!(err, DriverError::BufferTooSmall));
        assert_eq!(host.outstanding(), 0);
    }

    #[test]
    fn two_simultaneous_slabs_are_disjoint() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        let mut a = host.alloc_dma_zeroed(PAGE_SIZE).expect("first");
        let mut b = host.alloc_dma_zeroed(PAGE_SIZE).expect("second");
        // Distinct slot indices guarantee the slabs name disjoint
        // ranges of the pool's slot bitmap.
        assert_ne!(a.slot(), b.slot());
        // Distinct device addresses.
        assert_ne!(a.device_addr(), b.device_addr());
        a.as_bytes_mut().copy_from_slice(&[0xAA; PAGE_SIZE]);
        b.as_bytes_mut().copy_from_slice(&[0xBB; PAGE_SIZE]);
        // No cross-talk: each slab observes only the byte pattern it
        // wrote.
        assert!(a.as_bytes().iter().all(|byte| *byte == 0xAA));
        assert!(b.as_bytes().iter().all(|byte| *byte == 0xBB));
        assert_eq!(host.outstanding(), 2);
        drop(a);
        assert_eq!(host.outstanding(), 1);
        drop(b);
        assert_eq!(host.outstanding(), 0);
        assert_eq!(host.pool.lock().live(), 0);
    }

    /// `notify_wait` returns immediately when the bound line fired
    /// before the call (the device raised its interrupt while the
    /// driver was busy). The ready flag is consumed on the first
    /// poll — no cooperative yield is needed.
    #[test]
    fn notify_wait_returns_when_line_pre_fired() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        // Pre-fire the line through a permissive controller so the
        // ready flag is already set when `notify_wait` polls.
        irq.fire(4, &OkController).expect("pre-fire");
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        assert_eq!(host.notify_wait(0, 1_000), CompletionSignal::Fired);
        // No yield occurred: the pre-fired ready flag was consumed on
        // the first poll.
        assert_eq!(waiter.yields(), 0);
        // The ready flag was consumed exactly once: a second wait
        // would block (so we do not call it), but the binding is no
        // longer ready.
        assert!(
            !irq.ready_for(handle),
            "notify_wait must consume the ready flag"
        );
    }

    /// `notify_wait` blocks across cooperative yields until the
    /// device fires its line, then returns. The fire is injected on
    /// the third parked yield to model a device that completes after
    /// the driver has gone to sleep.
    #[test]
    fn notify_wait_blocks_until_line_fires() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::firing(&irq, handle, 4, 3);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        // A budget generous enough to outlast the three parks (the fake
        // clock ticks once per park), so the fire is what ends the wait.
        assert_eq!(host.notify_wait(0, 1_000), CompletionSignal::Fired);
        // The loop parked three times before the injected fire
        // released it.
        assert_eq!(waiter.yields(), 3);
        assert!(
            !irq.ready_for(handle),
            "notify_wait must consume the ready flag"
        );
    }

    /// Mask-before-wake: the controller mask is installed *before*
    /// `notify_wait` observes the wake-up. The probe controller
    /// records the entry's `ready` flag at the instant `mask` runs;
    /// it must still be `false`, proving the wake the driver sees is
    /// always preceded by a masked line
    /// (`docs/src/security/irq.md`).
    #[test]
    fn notify_wait_observes_mask_before_wake() {
        // `SpinLock`/atomics (not `Cell`) so `Probe`/`ProbeWaiter` are
        // `Sync`, as the host's line wait must be.
        struct Probe<'a> {
            table: &'a IrqTable,
            handle: IrqHandle,
            ready_during_mask: SpinLock<Option<bool>>,
        }
        impl IrqController for Probe<'_> {
            fn mask(&self, _line: u32) -> Result<(), MaskError> {
                *self.ready_during_mask.lock() = Some(self.table.ready_for(self.handle));
                Ok(())
            }
        }

        // A waiter that fires through the probe controller on the
        // first yield.
        struct ProbeWaiter<'a> {
            table: &'a IrqTable,
            probe: &'a Probe<'a>,
            line: u32,
            yields: AtomicU32,
        }
        impl IrqWaiter for ProbeWaiter<'_> {
            fn now_ns(&self) -> u64 {
                0
            }
            fn yield_now(&self, _deadline_ns: u64) -> Result<(), IrqWaitAbort> {
                if self.yields.fetch_add(1, Ordering::Relaxed) == 0 {
                    self.table.fire(self.line, self.probe).expect("fire");
                }
                Ok(())
            }
        }
        impl LineWait for ProbeWaiter<'_> {
            fn await_completion(&self, budget_ns: u64) -> WaitOutcome {
                block_until_ready(self.table, self.probe.handle, OWNER, budget_ns, self)
            }
            fn await_event(&self, timeout_ns: u64) -> WaitOutcome {
                block_until_ready(self.table, self.probe.handle, OWNER, timeout_ns, self)
            }
        }

        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let probe = Probe {
            table: &irq,
            handle,
            ready_during_mask: SpinLock::new(None),
        };
        let waiter = ProbeWaiter {
            table: &irq,
            probe: &probe,
            line: 4,
            yields: AtomicU32::new(0),
        };
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        assert_eq!(host.notify_wait(0, 1_000), CompletionSignal::Fired);
        assert_eq!(
            *probe.ready_during_mask.lock(),
            Some(false),
            "ready must still be false while the controller mask runs"
        );
        assert!(
            !irq.ready_for(handle),
            "notify_wait consumed the ready flag"
        );
    }

    /// `notify_wait` returns without hanging when the binding has been
    /// released (the driver task was torn down). The shared loop surfaces
    /// this as `NotFound`, which the host reports as silence: a driver
    /// cannot tell a vanished binding from a dead device, and neither is a
    /// reason to keep waiting.
    #[test]
    fn notify_wait_returns_when_binding_released() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        // Release every binding owned by the device task before the
        // wait runs.
        irq.release_for(OWNER, &OkController);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        assert_eq!(host.notify_wait(0, 1_000), CompletionSignal::TimedOut);
        assert!(irq.lookup(handle).is_none());
    }

    /// A device that never signals releases the caller at its budget and is
    /// reported as silence.
    ///
    /// This is the whole point of the budget. The waiting task holds the
    /// disk's lock for the duration of its request, so a wait that could
    /// not expire would not merely delay this caller: it would wedge every
    /// other user of that disk — at boot, the `/System` mount and the
    /// driver-store service, and with them the rest of the system — with no
    /// error anywhere to explain it.
    #[test]
    fn a_silent_device_times_out_instead_of_waiting_forever() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        // Never fires; the fake clock ticks once per park, so a 5 ns budget
        // expires after a bounded number of parks.
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        assert_eq!(host.notify_wait(0, 5), CompletionSignal::TimedOut);
        // The binding is untouched: a timed-out wait consumes nothing and
        // leaves the line bound for the next request.
        assert!(irq.lookup(handle).is_some());
    }

    /// The caller's budget reaches the park that must honour it.
    ///
    /// A park is releasable without the line firing only if it knows the
    /// deadline, so passing the budget down is what makes the wait bounded
    /// in fact rather than in intent.
    #[test]
    fn the_callers_budget_reaches_the_park() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        // The clock starts at 0, so the loop's absolute deadline is the
        // budget itself.
        assert_eq!(host.notify_wait(0, 9), CompletionSignal::TimedOut);
        assert_eq!(
            waiter.parked_until(),
            9,
            "the park must be told the deadline the wait is bounded by"
        );
    }

    /// A budget is a request outstanding, waited for as a completion, whose
    /// silence the kernel records; no budget is an unsolicited event.
    #[test]
    fn a_budget_waits_for_a_completion_and_no_budget_for_an_event() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        assert_eq!(host.notify_wait(0, 5), CompletionSignal::TimedOut);
        assert_eq!(waiter.waits(), (1, 0));
        irq.fire(4, &OkController).expect("fire");
        assert_eq!(host.notify_wait(0, u64::MAX), CompletionSignal::Fired);
        assert_eq!(waiter.waits(), (1, 1));
    }

    #[test]
    fn a_waits_clock_is_the_parks_own_clock() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        assert_eq!(host.now_ns(), 0);
        assert_eq!(host.notify_wait(0, 5), CompletionSignal::TimedOut);
        assert_eq!(
            host.now_ns(),
            waiter.now_ns(),
            "one clock for waits and deadlines"
        );
        assert!(host.now_ns() >= 5, "the timed-out wait spent its budget");
    }

    /// A carve the pool cannot back reaches the driver as exhaustion.
    #[test]
    fn a_carve_the_pool_cannot_back_is_exhaustion() {
        let frames = FrameAllocator::new(&small_map(16)).unwrap();
        let sim = fresh_sim();
        let pool = fresh_pool(&frames, &sim);
        let sink = Recorder::new();
        let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
        let (irq, handle) = irq_binding(4);
        let waiter = TestWaiter::idle(&irq, handle);
        let host = KernelVirtioHost::new(pool, &caller, &sink, PoolId::fresh(), &waiter);
        // The pool's frames hold sixteen pages.
        let err = host.alloc_dma_zeroed(PAGE_SIZE * 64).unwrap_err();
        assert_eq!(err, DriverError::OutOfMemory);
        assert_eq!(host.outstanding(), 0);
    }
}
