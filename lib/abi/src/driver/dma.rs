//! Owned DMA region ABI types (`abi-v1`).
//!
//! The host↔driver seam for DMA-able memory. These types live in `lib/abi`
//! because every driver-class trait a host implements names them, and a host
//! depending on a driver crate would invert the dependency direction.
//! `lib/virtio` re-exports them beside its virtio-specific `BounceBuffer`, and
//! its tests (`lib/virtio/src/dma.rs`, which may allocate) exercise them
//! through that re-export.
//!
//! No allocation: the crate-wide `no_std` and no-allocation invariants
//! documented in `lib/abi`'s crate-root rustdoc are preserved.

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::DriverError;

/// Host-side facility that mints owned, device-visible DMA regions.
///
/// This is the bus-neutral DMA-allocation seam, a sibling of
/// [`MmioMapper`](super::mmio::MmioMapper): any driver that has to hand the
/// hardware a physically-addressable buffer — a bus driver staging an xHCI
/// device-context / transfer ring, a virtio driver staging a split
/// virtqueue — obtains a [`DmaSlab`] through it. It lives in `lib/abi` so the
/// host accessor [`DriverHost::dma_host`](super::DriverHost::dma_host) can
/// name it without inverting the dependency direction, and
/// it is *separate from* virtio so a non-virtio bus driver never has to reach
/// through a virtio-shaped trait to allocate DMA. [`VirtioHost`] extends it
/// (`VirtioHost: DmaHost`) so a virtio host is also a DMA host without
/// duplicating the allocation contract.
///
/// [`VirtioHost`]: super::virtio::VirtioHost
pub trait DmaHost {
    /// Allocate a contiguous, device-visible, zero-initialised DMA region.
    ///
    /// The returned [`DmaSlab`] is owned by the caller; it carries the pool
    /// id and slot it was minted from so the host's drop path can reclaim
    /// the slot. The bytes are zero-initialised so a driver can publish the
    /// slab to a device without first clearing leftover bytes from another
    /// transaction (defence in depth zero-on-free).
    ///
    /// # Errors
    ///
    /// * [`DriverError::BufferTooSmall`] if `size == 0`.
    /// * [`DriverError::OutOfMemory`] if the memory cannot be had: the host's
    ///   frames or the driver's DMA budget are exhausted.
    /// * [`DriverError::LengthOutOfRange`] if `size` is more than the host can
    ///   carve as one region.
    /// * [`DriverError::PermissionDenied`] if the calling task is missing the
    ///   capability the host enforces at allocation time (the kernel host
    ///   gates on [`CapabilityId::MEM_DMA`](crate::CapabilityId::MEM_DMA)).
    ///
    /// # Capabilities
    ///
    /// None directly at the trait level; the host enforces its own DMA-pool
    /// quota and per-task capability check at allocation time ("per-process heaps" + "fail closed").
    fn alloc_dma_zeroed(&self, size: usize) -> Result<DmaSlab, DriverError>;

    /// The device has been reset and can no longer reach DMA memory an earlier
    /// instance of this driver handed it, so that memory may leave the
    /// quarantine it was held in when that instance ended
    /// (`plans/OPEN-DEFECTS.md` D167).
    ///
    /// A driver calls this once, as soon as its bring-up has reset the device.
    /// It is best effort by design: a refusal only leaves the memory held,
    /// which costs memory and never safety, and the kernel records every
    /// decision. A host whose DMA memory cannot outlive the process using it
    /// — an in-kernel host, a test mock — has nothing to release.
    fn device_quiesced(&self);

    /// Hold every later region to what a device driving `reach` reaches,
    /// never wider than the host's own bound: a driver states it once its
    /// bring-up has read the device's address width, and the narrowest
    /// statement stands.
    ///
    /// The default honours only the full reach, for a host that cannot place
    /// a region below a bound, so a driver whose device reaches less fails
    /// rather than hand it memory past its reach.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where the host cannot honour the bound.
    fn narrow_dma_reach(&self, reach: DmaReach) -> Result<(), DriverError> {
        if reach == DmaReach::FULL {
            Ok(())
        } else {
            Err(DriverError::Unsupported)
        }
    }
}

/// The address bits a DMA master drives, from one to 64.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DmaReach(u32);

impl DmaReach {
    /// A master that drives every address bit.
    pub const FULL: Self = Self(u64::BITS);

    /// A master driving `N` address bits, the bound checked as the program is
    /// built.
    #[must_use]
    pub const fn of<const N: u32>() -> Self {
        const { assert!(N != 0 && N <= u64::BITS, "a DMA reach is one to 64 bits") };
        Self(N)
    }

    /// A master driving `bits` address bits, or `None` for no bits or more
    /// than 64.
    #[must_use]
    pub const fn new(bits: u32) -> Option<Self> {
        if bits == 0 || bits > u64::BITS {
            None
        } else {
            Some(Self(bits))
        }
    }

    /// The address bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The first address past the reach, or `None` for a master that drives
    /// every address bit.
    #[must_use]
    pub const fn end(self) -> Option<u64> {
        1u64.checked_shl(self.0)
    }
}

/// Stable identifier of a DMA pool.
///
/// The driver code is opaque to pool internals; the identifier
/// exists so a [`DmaSlab`] can be tied to the pool that minted it.
/// [`PoolId::MOCK`] is reserved for the in-process test host `lib/virtio`
/// ships behind its `mock` feature.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct PoolId(u64);

impl PoolId {
    /// Reserved identifier for the in-process test host, `lib/virtio`'s
    /// `MockHost`.
    pub const MOCK: Self = Self(0);

    /// Construct an identifier from its raw `u64`.
    ///
    /// Intended for the kernel host wiring (Stage 4.D Item 0); the
    /// value must be globally unique within the running system and
    /// must not be zero ([`PoolId::MOCK`] is reserved).
    #[must_use]
    pub const fn from_raw(id: u64) -> Self {
        Self(id)
    }

    /// Raw `u64` representation.
    #[must_use]
    pub const fn as_raw(self) -> u64 {
        self.0
    }

    /// Allocate a fresh, process-unique [`PoolId`] that is **not**
    /// [`PoolId::MOCK`].
    ///
    /// Intended for the kernel host wiring (Stage 4.D Item 0) and
    /// for unit tests that exercise multiple independent pools.
    #[must_use]
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Cache-maintenance shim a [`DmaSlab`] invokes to keep a **non-coherent**
/// DMA master and the CPU caches in sync.
///
/// `base` is the CPU-virtual address of the affected sub-range and `len`
/// its byte length. The shim must clean **and** invalidate that range to
/// the point of coherency (a `dc civac`-class operation plus a barrier),
/// so that bytes the CPU just wrote are visible to the device and bytes
/// the device just wrote are visible to the CPU. It performs cache
/// maintenance only — it never dereferences the range — so it is a safe
/// `fn`. A slab minted without one (coherent interconnect, or the
/// in-process mock host) skips maintenance entirely.
pub type SlabCoherencyFn = fn(base: *const u8, len: usize);

/// How a [`DmaSlab`] from a pool ended, which its [`SlabFreeFn`] is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlabEnd {
    /// The pool takes the region back.
    Released,
    /// The region was [withheld](DmaSlab::withhold): the pool frees none of
    /// it.
    Withheld,
}

/// Type-erased free shim called from [`DmaSlab::drop`].
///
/// * `pool` is the opaque pointer the slab was built with;
/// * `cpu` is the slab's CPU base pointer (the user virtual base the
///   allocator returned) — the key a syscall-backed pool (the user-space
///   `RtDriverHost`) frees the buffer by;
/// * `slot` and `len` are the slab's bookkeeping (a slot-bitmap pool such as
///   the in-kernel host frees by `slot`, ignoring `cpu`);
/// * `end` is whether the pool may take the region back.
///
/// # Safety
///
/// The shim is `unsafe` because [`DmaSlab::drop`] (the only caller)
/// must guarantee `pool` still points at the originating pool, which
/// the pool enforces by outliving every slab it minted.
pub type SlabFreeFn =
    unsafe fn(pool: *const (), cpu: NonNull<u8>, slot: usize, len: usize, end: SlabEnd);

/// Owned, device-visible DMA region.
///
/// # Invariants
///
/// * `device_addr` is the device-visible base address that, when programmed
///   into a virtio descriptor's `addr` field, points at the same
///   bytes as `ptr[0]`: an IOVA behind a translation unit, never assumed
///   physical.
/// * `ptr` is a non-null, aligned pointer to a buffer of exactly
///   `len` bytes. Disjointness with every other live slab from the
///   same pool is witnessed by the pool's slot bitmap (one slot ↔
///   one slab).
/// * If `free_fn` is `Some`, dropping the slab calls it exactly once, and
///   the pool reclaims the slot unless the slab was [withheld]. If
///   `free_fn` is `None` (a leaked slab), drop is a no-op and the bytes
///   leak (the leak contract).
///
/// [withheld]: Self::withhold
#[derive(Debug)]
pub struct DmaSlab {
    device_addr: u64,
    ptr: NonNull<u8>,
    len: usize,
    pool_id: PoolId,
    slot: usize,
    pool_ptr: *const (),
    free_fn: Option<SlabFreeFn>,
    withheld: bool,
    coherency: Option<SlabCoherencyFn>,
}

// SAFETY: A `DmaSlab` is a tagged pointer to a disjoint range of
// pool storage. `NonNull<u8>` and `*const ()` are `!Send` by default
// because the compiler does not know whether the pointee permits
// cross-thread access. We assert `Send` because (i) the pool's slot
// bitmap guarantees only this slab observes its byte range; (ii) the
// pool implementations in `tairix-kernel-mem` are themselves `Send`
// (their internal storage is behind a per-process address space);
// and (iii) the in-process mock host's slab holds a count of its own
// thread-safe storage, so whichever thread drops it last frees it.
// No `Sync`: the inner bytes are mutably aliased through
// `as_bytes_mut` and concurrent access through two threads would
// race.
unsafe impl Send for DmaSlab {}

impl DmaSlab {
    /// Construct a [`DmaSlab`] whose drop is a no-op.
    ///
    /// Used by unit tests that wrap leaked or borrowed storage for the
    /// duration of the test function.
    ///
    /// # Safety
    ///
    /// * `ptr` must point at a buffer of exactly `len` bytes that
    ///   remains valid for the entire lifetime of the returned slab
    ///   and is not aliased by any other live reference.
    /// * `device_addr` must be the device-visible base address of `ptr[0]`.
    #[must_use]
    pub unsafe fn from_leaked(
        device_addr: u64,
        ptr: NonNull<u8>,
        len: usize,
        pool_id: PoolId,
        slot: usize,
    ) -> Self {
        Self {
            device_addr,
            ptr,
            len,
            pool_id,
            slot,
            pool_ptr: core::ptr::null(),
            free_fn: None,
            withheld: false,
            coherency: None,
        }
    }

    /// Construct a [`DmaSlab`] that reclaims its slot via `free_fn`
    /// on drop.
    ///
    /// Intended for the kernel host (Stage 4.D Item 0).
    ///
    /// # Safety
    ///
    /// * `ptr` must point at a buffer of exactly `len` bytes whose
    ///   disjointness is witnessed by the pool's slot bitmap (one
    ///   slot ↔ one slab).
    /// * `pool_ptr` must remain valid until `free_fn` is invoked
    ///   from [`Self::drop`]; the pool must outlive the slab.
    /// * `device_addr` must be the device-visible base address of `ptr[0]`.
    #[must_use]
    pub unsafe fn from_pool(
        device_addr: u64,
        ptr: NonNull<u8>,
        len: usize,
        pool_id: PoolId,
        slot: usize,
        pool_ptr: *const (),
        free_fn: SlabFreeFn,
    ) -> Self {
        Self {
            device_addr,
            ptr,
            len,
            pool_id,
            slot,
            pool_ptr,
            free_fn: Some(free_fn),
            withheld: false,
            coherency: None,
        }
    }

    /// Attach a [`SlabCoherencyFn`] for a **non-coherent** DMA master.
    ///
    /// On an interconnect where the device does not snoop the CPU caches
    /// (e.g. the BCM2711 PCIe root complex), the host wires the arch cache
    /// clean/invalidate primitive here so [`Self::sync_range`] can bracket
    /// every CPU-side publish/consume. Without it [`Self::sync_range`] is a
    /// no-op, the correct behaviour for a coherent interconnect or the
    /// in-process mock host.
    #[must_use]
    pub fn with_coherency(mut self, coherency: SlabCoherencyFn) -> Self {
        self.coherency = Some(coherency);
        self
    }

    /// Clean **and** invalidate the byte range `[offset, offset + len)` to
    /// the point of coherency through the attached [`SlabCoherencyFn`].
    ///
    /// The owner of DMA publication ordering on a non-coherent
    /// interconnect calls this **after** writing bytes the device will
    /// read (so they reach memory before the doorbell) and **before**
    /// reading bytes the device wrote (so the CPU does not see a stale
    /// cached copy). It is a no-op when no shim is attached, when `len` is
    /// zero, or — failing closed — when the range falls
    /// outside the region.
    pub fn sync_range(&self, offset: usize, len: usize) {
        let Some(maintain) = self.coherency else {
            return;
        };
        if len == 0 {
            return;
        }
        let Some(end) = offset.checked_add(len) else {
            return;
        };
        if end > self.len {
            return;
        }
        // SAFETY: `ptr` points at exactly `self.len` valid bytes by the
        // construction invariants, and `offset + len <= self.len`, so
        // `ptr + offset` addresses exactly `len` in-bounds bytes. The shim
        // performs cache maintenance over that range only; it never reads
        // or writes the bytes, so no aliasing rule is involved.
        let base = unsafe { self.ptr.as_ptr().add(offset).cast_const() };
        maintain(base, len);
    }

    /// Whether the device does not snoop the CPU caches for this region, so
    /// every publish and consume must be bracketed by [`Self::sync_range`].
    #[must_use]
    pub fn needs_cache_maintenance(&self) -> bool {
        self.coherency.is_some()
    }

    /// Device-visible base address of this region.
    #[must_use]
    pub fn device_addr(&self) -> u64 {
        self.device_addr
    }

    /// The device address of the `len` bytes at `offset` into this region,
    /// or [`None`] for a range that leaves it.
    #[must_use]
    pub fn device_addr_at(&self, offset: usize, len: usize) -> Option<u64> {
        if offset.checked_add(len)? > self.len {
            return None;
        }
        self.device_addr.checked_add(u64::try_from(offset).ok()?)
    }

    /// Byte length of this region.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// `true` iff the region is zero-length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Identifier of the pool that minted this slab.
    #[must_use]
    pub fn pool_id(&self) -> PoolId {
        self.pool_id
    }

    /// Never return this region to its pool: the device it was handed to may
    /// still master it, and nothing has proven otherwise.
    ///
    /// Dropping the slab afterwards tells its pool the region ended
    /// [withheld](SlabEnd::Withheld), and the pool frees none of it. In a
    /// user-space driver the region stays mapped until the process ends, when
    /// the kernel takes it into the DMA quarantine that frees it once the
    /// device is proven quiet.
    pub fn withhold(&mut self) {
        self.withheld = true;
    }

    /// Slot index within the originating pool.
    #[must_use]
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// Immutable byte view of the region (CPU-side).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `ptr` is non-null and points at exactly `len`
        // bytes by the construction invariants. Disjointness with
        // every other live slab from the same pool is witnessed by
        // the pool's slot bitmap (one slot ↔ one slab), so no
        // other live reference aliases these bytes.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr().cast_const(), self.len) }
    }

    /// Mutable byte view of the region (CPU-side).
    #[must_use]
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in [`Self::as_bytes`]. The `&mut self` borrow
        // upgrades exclusivity from "no other live `&` to these
        // bytes" (slot-bitmap disjointness across slabs) to "no
        // other live reference to these bytes" (this is the only
        // slab carrying this slot, and Rust's borrow checker
        // serialises every `&mut [u8]` derived from a given slab).
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for DmaSlab {
    fn drop(&mut self) {
        if let Some(f) = self.free_fn {
            let end = if self.withheld {
                SlabEnd::Withheld
            } else {
                SlabEnd::Released
            };
            // SAFETY: at construction the caller of `from_pool`
            // proved that `pool_ptr` outlives this slab and that
            // `(slot, len)` is the slab's exclusive slot in the
            // pool's bitmap. `self.ptr` is this slab's CPU base, the
            // key a syscall-backed pool frees by. `Drop::drop` runs
            // exactly once.
            unsafe { f(self.pool_ptr, self.ptr, self.slot, self.len, end) }
        }
    }
}
