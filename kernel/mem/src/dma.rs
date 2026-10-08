//! Per-process-heap DMA allocator.
//!
//! the charter requires every kernel-side DMA facility to:
//!
//! 1. Carve allocations out of the calling process's heap, never a
//!    global pool;
//! 2. Place guard pages around the slab so a buffer over-run faults
//!    instead of silently corrupting a neighbour;
//! 3. Zero every byte on free, because any DMA buffer may have held
//!    credentials, key material, or capability tokens at some point in
//!    its lifetime;
//! 4. Report exhaustion through a [`Result`] — allocation failure is
//!    never a panic.
//!
//! This module owns the architecture-neutral half of that contract. It
//! composes the existing [`FrameAllocator`] (for contiguous-by-physical
//! frame blocks) with an [`AddressSpace<P>`] (for the per-process
//! virtual-address window). The capability check itself lives in
//! `kernel/sec::dma`, since `kernel/mem` deliberately depends on
//! neither `tairix-abi` nor `tairix-caps` (see `kernel/mem/Cargo.toml`).
//!
//! # Returned shape
//!
//! [`DmaPool::alloc`] hands back a [`DmaBuffer`] carrying:
//!
//! * `virt` — the page-aligned virtual address inside the pool's
//!   window, suitable for handing to the driver's CPU-side code;
//! * `phys` — the page-aligned physical address of the same first
//!   byte, for the kernel's own view of the frames;
//! * `device_addr` — the address the device reaches that byte at: an
//!   IOVA in its node's domain when a translation unit stands between
//!   them ([`DmaTranslator`]), else the physical address;
//! * `len` — the *backing* length in bytes. A pool's carve is one block
//!   rounded up to the next power-of-two pages, because the frame
//!   allocator is a buddy allocator and only guarantees physical
//!   contiguity inside a single order. A process's translated carve
//!   ([`DmaWindowMap::alloc_into`]) is whole pages over scattered
//!   blocks. Drivers consume `len`, not the original request —
//!   analogous to `Vec::capacity` vs the constructor argument.
//!
//! # Guard model
//!
//! Each live allocation occupies *(2 + `data_pages`)* consecutive slots
//! in the pool's virtual window: a leading guard slot, the data
//! slots, then a trailing guard slot. Guard slots are intentionally
//! **left unmapped** in the [`AddressSpace`] so that the MMU faults on
//! a register-block over-run instead of letting it reach a
//! neighbouring allocation, exactly mirroring the convention used by
//! [`crate::slab`].
//!
//! # CPU access
//!
//! A driver's CPU-side code and the device both touch the buffer's
//! *physical frames*. The CPU reaches them through the kernel's direct
//! physical map ([`crate::phys::PhysMap`]): [`DmaPool::bytes`],
//! [`DmaPool::bytes_mut`], and [`DmaPool::slot_base`] translate the
//! buffer's `phys` into a pointer, so the bytes the driver reads are
//! exactly the bytes the device wrote — not a disconnected copy.
//!
//! # Zero-on-free
//!
//! [`DmaPool::free`] zeroes every byte of the data region through the
//! `zeroize` crate's volatile clear **before** the frames are returned to the
//! [`FrameAllocator`]. The driver may not see leftover bytes in a
//! later allocation, and a forensic dump of free physical memory
//! cannot recover the credentials the buffer once held.

use alloc::vec::Vec;
use core::fmt;
use core::ptr::NonNull;

use tairix_abi::DmaCoherence;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use zeroize::Zeroize;

use crate::error::AllocError;
use crate::frame::{
    Chunks, Frame, FrameAllocator, FrameBlock, MemoryClass, PhysAddr, MAX_ORDER, PAGE_SHIFT,
    PAGE_SIZE,
};
use crate::phys::PhysMap;
use crate::ptr::slice_within;
use crate::retire::{Retire, Unpublished};
use crate::vmm::{AddressSpace, MapFlags, Page, PageTable, PageTableError, VirtAddr};

/// Guard slots bracketing a carve's data pages in the virtual window, one
/// either side.
const GUARD_SLOTS: usize = 2;

/// The buddy order a carve of `requested` bytes takes — its page count
/// rounded up to a power of two, so one buddy block satisfies it — or `None`
/// for zero bytes or past [`MAX_ORDER`].
const fn carve_order(requested: usize) -> Option<u32> {
    if requested == 0 {
        return None;
    }
    let order = requested
        .div_ceil(PAGE_SIZE)
        .next_power_of_two()
        .trailing_zeros();
    if order > MAX_ORDER {
        None
    } else {
        Some(order)
    }
}

/// The virtual-window slots one carve of `requested` bytes occupies: its
/// power-of-two data pages and a guard either side, so a pool can be sized
/// for exactly the carves it will serve. `None` for a carve the pool refuses.
#[must_use]
pub const fn window_slots(requested: usize) -> Option<usize> {
    match carve_order(requested) {
        Some(order) => Some((1 << order) + GUARD_SLOTS),
        None => None,
    }
}

/// Zero the `pages`-page block at `start` through the direct map and clean
/// it to memory, so neither the CPU's caches nor a device read it back.
///
/// # Errors
///
/// [`DmaError::DirectMap`] when the direct map does not reach the block.
fn scrub_block(phys: &dyn PhysMap, start: Frame, pages: usize) -> Result<(), DmaError> {
    let len = pages * PAGE_SIZE;
    let ptr = phys
        .translate(start.start(), len)
        .ok_or(DmaError::DirectMap)?;
    // SAFETY: `translate` returned `len` bytes of the block, which its caller
    // holds and no mapping, CPU or snapshot can still reach.
    unsafe { slice_within(ptr.as_ptr(), len, 0, len) }
        .ok_or(DmaError::DirectMap)?
        .zeroize();
    phys.clean_invalidate(start.start(), len);
    Ok(())
}

/// Scrub every block of `blocks` through the direct map, which no mapping,
/// CPU or device, may still reach.
///
/// # Errors
///
/// [`DmaError::DirectMap`] when the direct map does not reach a block: none
/// of them may then be freed.
pub fn scrub_blocks(phys: &dyn PhysMap, blocks: &[FrameBlock]) -> Result<(), DmaError> {
    blocks
        .iter()
        .try_for_each(|block| scrub_block(phys, block.frame, 1 << block.order))
}

/// Free every block of `blocks`, scrubbed already, to `frames`.
///
/// # Errors
///
/// [`DmaError::Alloc`] for a block the allocator refuses; the rest are freed.
pub fn free_blocks(frames: &FrameAllocator, blocks: &[FrameBlock]) -> Result<(), DmaError> {
    blocks
        .iter()
        .map(|block| frames.free_order(block.frame, block.order))
        .fold(Ok(()), Result::and)
        .map_err(DmaError::Alloc)
}

/// Errors specific to [`DmaPool`].
///
/// Distinct from the bare [`AllocError`] because a DMA pool can fail in
/// ways the bare allocators cannot: it has its own virtual-address
/// window, its own guard-slot bookkeeping, and a constraint on the
/// caller's pointer at `free` time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DmaError {
    /// The underlying frame or metadata allocator failed.
    Alloc(AllocError),
    /// The page-table layer rejected a map/unmap operation.
    PageTable(PageTableError),
    /// `free` was called with a [`DmaBuffer`] whose `virt` is not the
    /// start of a live allocation in this pool.
    UnknownBuffer,
    /// A buffer's physical frames fall outside the kernel's direct
    /// physical map, so the CPU cannot reach them. Indicates a
    /// mis-sized [`crate::phys::PhysMap`] for the platform; the pool
    /// fails closed rather than synthesising a pointer.
    DirectMap,
    /// The pool was constructed with a request that the allocator
    /// cannot satisfy (e.g. zero capacity, or a virtual base not
    /// page-aligned).
    InvalidPoolConfig,
    /// The requested allocation is larger than the maximum buddy
    /// order the underlying [`FrameAllocator`] supports.
    SizeUnsupported,
    /// The buffer spans several frame blocks, so no one physical base
    /// names it.
    Discontiguous,
    /// The caller asked for a zero-length allocation. Zero-sized DMA
    /// regions are rejected on purpose: a successful return value
    /// would be indistinguishable from a one-byte allocation and is
    /// almost always a bug at the call site.
    ZeroSize,
    /// The carve named a different [`DmaCustodian`] from the one the space is
    /// already bound to. A space's DMA memory has one custodian for its life,
    /// so its teardown has exactly one place to surrender it to.
    CustodianMismatch,
    /// No custody can take this space's DMA memory at teardown, so it may
    /// not carve any.
    NoCustody,
    /// The carve names a hardware-tree node that has left the tree: no driver
    /// of it may hand its device more memory.
    DeviceGone,
    /// The device's translation unit refused to map the carve.
    Translation,
    /// The device's translation unit did not confirm that the device lost its
    /// reach to a block, so the block stays out of reuse for good.
    Unconfirmed,
    /// The device belongs to a kernel driver: no process may carve for it.
    KernelOwned,
    /// Another device the fabric cannot keep apart from this one has a live
    /// owner, which holds their isolation group.
    GroupBusy,
}

impl From<AllocError> for DmaError {
    fn from(e: AllocError) -> Self {
        Self::Alloc(e)
    }
}

impl From<PageTableError> for DmaError {
    fn from(e: PageTableError) -> Self {
        Self::PageTable(e)
    }
}

impl fmt::Display for DmaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Alloc(e) => write!(f, "dma alloc: {e}"),
            Self::PageTable(e) => write!(f, "dma page-table: {e:?}"),
            Self::UnknownBuffer => f.write_str("dma buffer not from this pool"),
            Self::DirectMap => f.write_str("dma buffer outside the direct physical map"),
            Self::InvalidPoolConfig => f.write_str("dma pool config invalid"),
            Self::SizeUnsupported => f.write_str("dma request exceeds max buddy order"),
            Self::Discontiguous => f.write_str("dma buffer spans several frame blocks"),
            Self::ZeroSize => f.write_str("zero-sized dma allocation is not permitted"),
            Self::CustodianMismatch => {
                f.write_str("dma carve names a custodian the space is not bound to")
            }
            Self::NoCustody => f.write_str("no custody can take this space's dma memory"),
            Self::DeviceGone => f.write_str("dma carve names a node that has left the tree"),
            Self::Translation => f.write_str("dma translation unit refused the carve"),
            Self::Unconfirmed => f.write_str("dma translation unit did not confirm an unmap"),
            Self::KernelOwned => f.write_str("dma carve names a device a kernel driver owns"),
            Self::GroupBusy => {
                f.write_str("dma carve names a device whose isolation group another owner holds")
            }
        }
    }
}

/// Custody of DMA memory whose device may outlive the owner that carved it
/// (`plans/OPEN-DEFECTS.md` D167, D225).
///
/// A device no translation unit confines keeps the bus addresses it was
/// handed, so memory carved for it must not return to the allocator just
/// because its owner died. The custodian holds it until the device is proven
/// quiet. A translated carve reaches custody only when its unit could not
/// confirm the device lost its reach ([`DeviceTranslation`]), or a block of it
/// could not be scrubbed or freed.
///
/// Room to hold a block is reserved when the block is carved, so the
/// surrender that ends an owner's life can neither fail nor allocate: the
/// owner may be dying under the very memory pressure it is about to relieve.
pub trait DmaCustody: Sync {
    /// Reserve room to take one more block carved for hardware-tree `node`.
    ///
    /// Called before every carve. Each reservation is returned by exactly one
    /// [`Self::unreserve`] or spent by holding its carve: once for a carve of
    /// one block, block by block for a translated carve, whose custody keeps
    /// no count.
    ///
    /// # Errors
    ///
    /// [`DmaError::Alloc`] when the room cannot be made,
    /// [`DmaError::DeviceGone`] when `node` has left the hardware tree, by
    /// either kind of removal, and [`DmaError::NoCustody`] where no custody
    /// is wired. The carve is then refused, since nothing could take its
    /// memory at teardown.
    fn reserve(&self, node: u32) -> Result<(), DmaError>;

    /// Return a reservation whose block can no longer reach custody: freed
    /// (or its release abandoned) while its owner lived, or never carved.
    fn unreserve(&self, node: u32);

    /// Take `block`, carved for `node` by the driver instance admitted as
    /// `generation`, from an owner being torn down, spending one
    /// reservation.
    ///
    /// A carve of one block is held once. One of several blocks is only ever
    /// a translated device's, whose custody is its translation and keeps no
    /// count ([`DmaCustodian::translated`]), so it is held block by block; a
    /// custody that counts never sees one.
    ///
    /// The block's frames stay allocated and are mapped nowhere; the
    /// custodian alone decides when they return to the allocator. It never
    /// fails and never allocates: a block no reservation stands behind is
    /// leaked, never freed.
    fn hold(&self, node: u32, generation: u64, block: FrameBlock);
}

/// How a device behind a translation unit reaches memory carved for it: by
/// an IOVA in the domain of its node's owner (`plans/IOMMU.md`).
pub trait DeviceTranslation: Sync {
    /// Map `blocks`, largest first, back to back into the domain of `node`'s
    /// owner admitted as `generation`, at one IOVA ending at most at `limit`
    /// (`0` for the domain's reach), and return the IOVA.
    ///
    /// # Errors
    ///
    /// [`DmaError::Alloc`] when the domain has no room, [`DmaError::Translation`]
    /// when the unit refuses (a node whose earlier end it could not confirm
    /// included), [`DmaError::DeviceGone`] for an owner whose domain was
    /// revoked or a node that has left the tree, [`DmaError::KernelOwned`] for
    /// a node a kernel driver owns, [`DmaError::GroupBusy`] for a node whose
    /// isolation group another node's live owner holds, and
    /// [`DmaError::Unconfirmed`] when a
    /// refused map could not be confirmed gone — the block must then never be
    /// reused.
    fn map(
        &self,
        node: u32,
        generation: u64,
        blocks: &[FrameBlock],
        limit: u64,
    ) -> Result<u64, DmaError>;

    /// Take the device's reach to the carve mapped at `iova` away, and confirm
    /// it gone. A domain already revoked took it away already.
    ///
    /// # Errors
    ///
    /// [`DmaError::Unconfirmed`] when the unit cannot confirm it: the carve's
    /// blocks must then never be reused.
    fn unmap(&self, node: u32, generation: u64, iova: u64) -> Result<(), DmaError>;

    /// Take the device's reach to the carve mapped at `iova` out of the
    /// domain's tables and take its `blocks`, to be scrubbed and freed once an
    /// invalidation confirms the carve gone — one confirming every carve that
    /// waits beside it. Answers whether it took them: `blocks` it declined,
    /// left as they were, the caller confirms gone with [`Self::unmap`].
    ///
    /// # Errors
    ///
    /// [`DmaError::Unconfirmed`] when the unit could not take the reach away:
    /// `blocks` must then never be reused.
    fn defer_free(
        &self,
        node: u32,
        generation: u64,
        iova: u64,
        blocks: &mut Chunks,
    ) -> Result<bool, DmaError> {
        let _ = (node, generation, iova, blocks);
        Ok(false)
    }

    /// End the domain of `node`'s owner admitted as `generation`, if it still
    /// stands: the device stops mastering and loses every carve at once,
    /// under one confirmed invalidation, so each later [`unmap`](Self::unmap)
    /// of the owner's carves answers without waiting on the unit. An end the
    /// unit cannot confirm leaves those unmaps unconfirmed.
    fn end(&self, node: u32, generation: u64);
}

/// The domain a translated carve maps into: the owner it belongs to and the
/// facility holding its domain.
#[derive(Clone, Copy)]
pub struct DmaTranslator {
    /// Hardware-tree node of the device.
    pub node: u32,
    /// Admission generation of the node's owner.
    pub generation: u64,
    /// The facility holding the owner's domain.
    pub domains: &'static dyn DeviceTranslation,
    /// The exclusive physical address the device's unit can name up to, which
    /// its carves' frames lie wholly below.
    pub output_limit: u64,
}

impl DmaTranslator {
    /// The frame ceiling [`Self::output_limit`] puts on a carve, if any.
    fn ceiling(&self) -> Option<PhysAddr> {
        (self.output_limit != u64::MAX).then_some(PhysAddr::new(self.output_limit))
    }

    fn map(&self, blocks: &[FrameBlock], limit: u64) -> Result<u64, DmaError> {
        self.domains.map(self.node, self.generation, blocks, limit)
    }

    fn unmap(&self, iova: u64) -> Result<(), DmaError> {
        self.domains.unmap(self.node, self.generation, iova)
    }

    fn defer_free(&self, iova: u64, blocks: &mut Chunks) -> Result<bool, DmaError> {
        self.domains
            .defer_free(self.node, self.generation, iova, blocks)
    }
}

impl fmt::Debug for DmaTranslator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaTranslator")
            .field("node", &self.node)
            .field("generation", &self.generation)
            .field("output_limit", &self.output_limit)
            .finish_non_exhaustive()
    }
}

/// The custodian an address space's DMA memory is surrendered to, and the
/// device it was carved for.
///
/// A translated device's custody is its translation itself, which keeps what
/// it cannot confirm gone and counts no reservation: the only constructors
/// pair them so, which is what lets a carve of several blocks hold each.
#[derive(Clone, Copy)]
pub struct DmaCustodian {
    /// Hardware-tree node the carving driver was loaded for.
    pub node: u32,
    /// The carving driver instance's admission generation for that node.
    pub generation: u64,
    custody: &'static dyn DmaCustody,
    translation: Option<&'static dyn DeviceTranslation>,
    /// For a translated device, its unit's [`DmaTranslator::output_limit`].
    output_limit: u64,
    coherence: DmaCoherence,
}

impl DmaCustodian {
    /// The custodian of a device that reaches its carves at their physical
    /// address, as `coherence` says: `custody` takes what teardown cannot
    /// release.
    #[must_use]
    pub fn untranslated(
        node: u32,
        generation: u64,
        custody: &'static dyn DmaCustody,
        coherence: DmaCoherence,
    ) -> Self {
        Self {
            node,
            generation,
            custody,
            translation: None,
            output_limit: u64::MAX,
            coherence,
        }
    }

    /// The custodian of a device that reaches its carves through the domain
    /// `translation` holds, which is also their custody, as `coherence` says,
    /// from frames wholly below `output_limit`, the exclusive physical address
    /// its unit can name up to.
    #[must_use]
    pub fn translated<T: DeviceTranslation + DmaCustody>(
        node: u32,
        generation: u64,
        translation: &'static T,
        output_limit: u64,
        coherence: DmaCoherence,
    ) -> Self {
        Self {
            node,
            generation,
            custody: translation,
            translation: Some(translation),
            output_limit,
            coherence,
        }
    }

    /// How the device's accesses meet the CPU's caches, which decides how
    /// its carves are mapped.
    #[must_use]
    pub const fn coherence(&self) -> DmaCoherence {
        self.coherence
    }

    /// Where the memory goes at teardown.
    #[must_use]
    pub fn custody(&self) -> &'static dyn DmaCustody {
        self.custody
    }

    /// The facility whose domain the device reaches carves through, or
    /// [`None`] for a device that reaches them at their physical address.
    #[must_use]
    pub fn translation(&self) -> Option<&'static dyn DeviceTranslation> {
        self.translation
    }

    /// Whether `self` and `other` name the same driver instance, custody and
    /// translation.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        let same_translation = match (self.translation, other.translation) {
            (None, None) => true,
            (Some(mine), Some(theirs)) => core::ptr::addr_eq(mine, theirs),
            _ => false,
        };
        self.node == other.node
            && self.generation == other.generation
            && core::ptr::addr_eq(self.custody, other.custody)
            && same_translation
            && self.coherence == other.coherence
    }

    /// For a translated device, the exclusive physical address its unit can
    /// name up to.
    #[must_use]
    pub const fn output_limit(&self) -> u64 {
        self.output_limit
    }

    /// The translator a carve for this custodian maps through, if its device
    /// is translated.
    #[must_use]
    pub fn translator(&self) -> Option<DmaTranslator> {
        self.translation.map(|domains| DmaTranslator {
            node: self.node,
            generation: self.generation,
            domains,
            output_limit: self.output_limit,
        })
    }
}

impl fmt::Debug for DmaCustodian {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaCustodian")
            .field("node", &self.node)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// A live DMA region handed out by [`DmaPool::alloc`].
///
/// `DmaBuffer` is **not** [`Drop`]: a forgotten buffer must be a hard
/// error, not a silent leak through a destructor that cannot reach the
/// originating pool. Callers free explicitly via [`DmaPool::free`].
///
/// The struct is `Copy + Clone` because a buffer descriptor is purely
/// addressing data; the kernel side keeps the *single* live record in
/// [`DmaPool`] and refuses a second free of the same `virt` address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBuffer {
    virt: VirtAddr,
    phys: PhysAddr,
    device: u64,
    len: usize,
}

impl DmaBuffer {
    /// CPU-side virtual address of the first byte of the region.
    ///
    /// Always page-aligned.
    #[must_use]
    pub fn virt(self) -> VirtAddr {
        self.virt
    }

    /// Physical address of the first byte of the region's frames.
    ///
    /// Always page-aligned.
    #[must_use]
    pub fn phys(self) -> PhysAddr {
        self.phys
    }

    /// The address the device reaches the first byte at: an IOVA in its
    /// domain when a unit translates it, else the physical address, which a
    /// bus window may still rebase.
    ///
    /// Always page-aligned.
    #[must_use]
    pub fn device_addr(self) -> u64 {
        self.device
    }

    /// Backing length in bytes: a power-of-two multiple of [`PAGE_SIZE`],
    /// or whole pages for a translated carve.
    #[must_use]
    pub fn len(self) -> usize {
        self.len
    }

    /// `true` if the buffer is zero bytes long. Reserved for the
    /// `clippy::len_without_is_empty` lint; cannot actually occur
    /// because [`DmaPool::alloc`] rejects zero-length requests.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Borrowed-space DMA window allocator — the single definition of the
/// guarded, zeroed DMA carve, virtually contiguous whatever its frames.
///
/// Owns only the *bookkeeping* (its virtual window base, the slot bitmap,
/// and the live-allocation records); the [`AddressSpace`], the
/// [`FrameAllocator`], and the [`PhysMap`] are **borrowed** per call. This
/// is the device-RAM analogue of [`crate::mmio::MmioWindowMap`]: it lets a
/// retained live address space ([`crate::live::LiveSpace`]) carve a DMA
/// buffer into a space it owns and lends, while the owning [`DmaPool`]
/// wrapper drives the same core over a space it owns outright — so the
/// carve logic has exactly one home.
pub struct DmaWindowMap {
    base: VirtAddr,
    capacity_pages: usize,
    /// Slot bookkeeping: `slot_used[i] == true` iff page `i` of the
    /// window is currently held by some live allocation (whether as a
    /// data slot or a guard slot). Grows lazily toward
    /// `capacity_pages`: a slot beyond the vector's length is free, so a
    /// task pays bookkeeping proportional to its peak concurrent carve,
    /// not the window's full reserved span — the same lazy-bookkeeping
    /// shape the device and shared windows use over their 1 GiB spans.
    slot_used: Vec<bool>,
    /// Live allocations keyed by `virt.as_u64()` (the first *data*
    /// page, i.e. the byte after the leading guard). Nothing reads the
    /// records in key order, so the unordered map's expected constant-time
    /// lookup is what the free and translate paths want. It is hashed
    /// unkeyed: the keys are this allocator's own page-aligned window
    /// addresses, and the window is private to one process, so a caller
    /// steering its own allocations can only lengthen its own probes.
    allocations: HashMap<u64, Record, BuildFastHash>,
}

/// Per-process DMA pool.
///
/// `DmaPool<P>` is generic over a [`PageTable`] implementation so
/// it can be exercised by `crate::HostPageTable` in unit tests and
/// driven by the architecture page-table types from `kernel/arch/*` in
/// production.
///
/// One pool is intended to live per process that holds
/// `CapabilityId::MEM_DMA`; the capability check itself lives in
/// `kernel/sec::dma` so this crate stays free of the `tairix-abi`
/// dependency. The guarded carve mechanism lives in [`DmaWindowMap`]
/// (shared with the `dma_alloc` syscall facility); this
/// type is the thin owning adapter over a space it owns outright.
pub struct DmaPool<'a, P: PageTable> {
    address_space: AddressSpace<P>,
    window: DmaWindowMap,
    /// Direct physical map used to reach a buffer's frames from the
    /// CPU. The same frames the device DMAs to are the bytes the
    /// driver reads/writes, so there is no disconnected copy: in
    /// production this is the boot identity map, in host tests a
    /// `SimPhysMap` standing in for physical RAM.
    phys: &'a dyn PhysMap,
    /// Borrowed frame allocator used to back the data slots.
    ///
    /// `FrameAllocator` is internally synchronised via a
    /// [`tairix_sync::SpinLock`], so multiple pools may share
    /// one. The borrow is explicit (not `'static`) so the host-side
    /// tests do not need to leak their allocators and a future
    /// per-process kernel layout can carve the global allocator into
    /// per-process slices.
    frames: &'a FrameAllocator,
    /// The domain the pool's device reaches its carves through, if a
    /// translation unit stands between them.
    translator: Option<DmaTranslator>,
    /// How the pool's device's accesses meet the CPU's caches.
    coherence: DmaCoherence,
}

/// Where a carve's device reaches it.
enum Reach {
    /// At the physical base of its one block.
    At(PhysAddr),
    /// Through the domain of its node's owner.
    Through(DmaTranslator),
}

/// What a live carve's device reaches it through.
#[derive(Clone, Copy, Debug)]
enum DeviceSide {
    /// Its physical address, until the driver's word that it is done.
    Physical,
    /// The domain `DmaTranslator` maps it into.
    Mapped(DmaTranslator),
    /// Nothing: its unit confirmed the device lost it, and the address may
    /// carry another carve since.
    Unmapped,
}

/// Per-live-allocation bookkeeping retained by the pool.
#[derive(Debug)]
struct Record {
    /// Index of the leading guard slot.
    leading_guard_slot: usize,
    /// Number of data slots: the blocks' pages together.
    data_pages: usize,
    /// One physically contiguous block, or — for a device a translation
    /// unit lets reach scattered frames — as many as the frames gave.
    blocks: Chunks,
    /// The address the device reaches the carve at.
    device_addr: u64,
    /// Whether, and how, the device still reaches it.
    device: DeviceSide,
}

impl DmaWindowMap {
    /// Construct a window allocator managing the virtual range
    /// `[base, base + capacity_pages * PAGE_SIZE)`.
    ///
    /// # Errors
    ///
    /// * [`DmaError::InvalidPoolConfig`] if `capacity_pages == 0`,
    ///   `base` is not page-aligned, or the window size overflows.
    pub fn new(base: VirtAddr, capacity_pages: usize) -> Result<Self, DmaError> {
        if capacity_pages == 0 || !base.is_page_aligned() {
            return Err(DmaError::InvalidPoolConfig);
        }
        // Reject a window whose byte span would overflow the slot
        // offset arithmetic in `virt_of_slot` before committing state.
        capacity_pages
            .checked_mul(PAGE_SIZE)
            .ok_or(DmaError::InvalidPoolConfig)?;
        Ok(Self {
            base,
            capacity_pages,
            slot_used: Vec::new(),
            allocations: HashMap::with_hasher(BuildFastHash::new()),
        })
    }

    /// Allocate a DMA region of at least `requested` bytes into the borrowed
    /// `space`, one run of its virtual window, drawing its frames from
    /// `frames` and reaching them through `phys`.
    ///
    /// `addr_limit` is where the device's reach ends (`0` for no end). For an
    /// untranslated device one physically contiguous block is carved wholly
    /// below it, or the request is refused with the allocator's own error:
    /// `OutOfRange` when no RAM lies below the limit, `OutOfMemory` when none
    /// of it is free. For a device `translator` maps through, the frames are
    /// whatever blocks the allocator gives, however scattered, the carve is
    /// no larger than its pages, and the limit binds the IOVA instead.
    pub fn alloc_into<P: PageTable>(
        &mut self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        requested: usize,
        device: CarveDevice,
    ) -> Result<DmaBuffer, DmaError> {
        let carve = Carve {
            addr_limit: device.addr_limit,
            backing: device
                .translator
                .map_or(Backing::Contiguous(None), Backing::Scattered),
            coherence: device.coherence,
        };
        self.alloc_inner(space, frames, phys, requested, carve)
    }

    /// Free a previously-allocated DMA buffer from the borrowed `space`,
    /// zeroing every byte (zero-on-free) before the frames
    /// return to `frames`.
    ///
    /// # Errors
    ///
    /// As [`DmaPool::free`].
    pub fn free_from<P: PageTable>(
        &mut self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        buf: DmaBuffer,
    ) -> Result<(), DmaError> {
        self.free_inner(space, frames, phys, buf, &mut Unpublished)
    }

    /// Free the live allocation whose first data page is at `virt`, zeroing
    /// every byte (zero-on-free) before its frames return to `frames`.
    ///
    /// The symmetric free for the `dma_free` syscall, which keys a release on
    /// the CPU virtual base the carve returned (the driver holds no
    /// [`DmaBuffer`] descriptor across the syscall boundary). The record's own
    /// `phys`/`len` are authoritative; only `virt` is taken from the caller,
    /// so a `virt` that is not the base of a live carve fails closed with
    /// [`DmaError::UnknownBuffer`] (covering a forged, stale, or double free).
    ///
    /// Reports the byte length released. The pages leave `retire`'s view
    /// before the frames are scrubbed and freed.
    ///
    /// # Errors
    ///
    /// As [`DmaPool::free`]. A block whose pages could not all be cleared,
    /// whose device its unit could not confirm lost it, or whose frames could
    /// not be scrubbed, stays live and is surrendered at teardown; one the
    /// allocator refuses back is not.
    pub fn free_at<P: PageTable>(
        &mut self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        virt: VirtAddr,
        retire: &mut dyn Retire,
    ) -> Result<usize, DmaError> {
        let record = self
            .allocations
            .get(&virt.as_u64())
            .ok_or(DmaError::UnknownBuffer)?;
        let buf = DmaBuffer {
            virt,
            phys: record
                .blocks
                .first()
                .map_or(PhysAddr::new(0), |block| block.frame.start()),
            device: record.device_addr,
            len: record.data_pages * PAGE_SIZE,
        };
        let len = buf.len;
        self.free_inner(space, frames, phys, buf, retire)
            .map(|()| len)
    }

    /// Look up `buf`'s live record and return its `(physical base, byte
    /// length)`.
    ///
    /// # Errors
    ///
    /// [`DmaError::UnknownBuffer`] if `buf` is not a live allocation, and
    /// [`DmaError::Discontiguous`] for one backed by scattered blocks.
    pub fn live_frames(&self, buf: &DmaBuffer) -> Result<(PhysAddr, usize), DmaError> {
        let record = self
            .allocations
            .get(&buf.virt.as_u64())
            .ok_or(DmaError::UnknownBuffer)?;
        match record.blocks.as_slice() {
            [block] => Ok((block.frame.start(), record.data_pages * PAGE_SIZE)),
            _ => Err(DmaError::Discontiguous),
        }
    }

    /// Whether a live allocation's first data page is at `virt`.
    #[must_use]
    pub fn holds(&self, virt: VirtAddr) -> bool {
        self.allocations.contains_key(&virt.as_u64())
    }

    /// Number of live allocations.
    #[must_use]
    pub fn live(&self) -> usize {
        self.allocations.len()
    }

    /// Total pages in the window.
    #[must_use]
    pub fn capacity_pages(&self) -> usize {
        self.capacity_pages
    }

    /// Whether `addr` lies inside this allocator's virtual window.
    #[must_use]
    pub fn contains(&self, addr: VirtAddr) -> bool {
        let start = self.base.as_u64();
        // `new` proved the window's byte span representable.
        let span = (self.capacity_pages * PAGE_SIZE) as u64;
        addr.as_u64() >= start && addr.as_u64() - start < span
    }

    /// Release **every** live DMA buffer as its owner ends: each block's pages
    /// leave `space` and it is zeroed and cleaned to memory. A translated
    /// owner is ended first ([`DeviceTranslation::end`]). A block whose
    /// translated device is confirmed to have lost its reach returns to
    /// `frames`; every other block passes, still allocated, to the custodian,
    /// since its device may still master it.
    ///
    /// The zeroing scrubs what the buffers held at once and makes a control
    /// block a device fetches afterwards read as all zero — no transfer, no
    /// successor. For a block the custodian takes it is defence in depth: the
    /// hold is what keeps the frames from reuse. Allocation-free, because it
    /// runs on teardown.
    pub fn surrender_into<P: PageTable>(
        &mut self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        custodian: &DmaCustodian,
    ) {
        // One end stops the device before anything is released, and takes its
        // reach to every carve under one confirmed invalidation, not one each.
        if let Some(translation) = custodian.translation {
            translation.end(custodian.node, custodian.generation);
        }
        for record in self.allocations.values() {
            let first_data_slot = record.leading_guard_slot + 1;
            for i in 0..record.data_pages {
                if let Ok(page) = Page::from_addr(self.virt_of_slot(first_data_slot + i)) {
                    let _ = space.unmap(page);
                }
            }
            space.shoot_remote(
                self.virt_of_slot(first_data_slot).as_u64(),
                record.data_pages as u64,
            );
            let unreachable = match record.device {
                DeviceSide::Mapped(translator) => translator.unmap(record.device_addr).is_ok(),
                DeviceSide::Unmapped => true,
                DeviceSide::Physical => false,
            };
            // Only a translated carve spans several blocks, and a translation's
            // custody keeps no count, so holding each spends nothing it lacks.
            let mut released = unreachable;
            for &block in &record.blocks {
                let scrubbed = scrub_block(phys, block.frame, 1 << block.order).is_ok();
                if !(unreachable && scrubbed && frames.free_order(block.frame, block.order).is_ok())
                {
                    released = false;
                    custodian
                        .custody
                        .hold(custodian.node, custodian.generation, block);
                }
            }
            if released {
                custodian.custody.unreserve(custodian.node);
            }
        }
        self.allocations.clear();
        self.slot_used.clear();
    }

    /// Reserve the record slot for one further live allocation, so the carve
    /// below commits into space that is already there.
    fn reserve_record(&mut self) -> Result<(), DmaError> {
        self.allocations
            .try_reserve(1)
            .map_err(|_| DmaError::Alloc(AllocError::OutOfMemory))
    }

    /// Map the run's data pages, from `first_data_slot` on, onto `blocks` in
    /// order.
    ///
    /// # Errors
    ///
    /// [`DmaError::PageTable`] from the first page that cannot be mapped. The
    /// pages already mapped are unmapped and every block returned to `frames`
    /// before returning, so a partial map never survives.
    fn map_data_pages<P: PageTable>(
        &self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        first_data_slot: usize,
        blocks: &[FrameBlock],
        coherence: DmaCoherence,
    ) -> Result<(), DmaError> {
        // Coherent with the device, so neither side needs cache maintenance:
        // ordinary RAM for a device that snoops, uncached for one that does
        // not, which a port unable to map it so refuses.
        let data = MapFlags::READ | MapFlags::WRITE | MapFlags::USER | MapFlags::DMA;
        let flags = match coherence {
            DmaCoherence::Snooped => data,
            DmaCoherence::Unsnooped => data | MapFlags::DMA_COHERENT,
        };
        let mut mapped = 0;
        for block in blocks {
            for i in 0..1usize << block.order {
                let page = Page::from_addr(self.virt_of_slot(first_data_slot + mapped));
                if let Err(err) =
                    page.and_then(|page| space.map(page, Frame(block.frame.0 + i), flags))
                {
                    self.rollback_partial_map(
                        space,
                        frames,
                        phys,
                        first_data_slot,
                        mapped,
                        blocks,
                        true,
                    );
                    return Err(DmaError::PageTable(err));
                }
                mapped += 1;
            }
        }
        Ok(())
    }

    /// The borrowed-space carve shared by [`Self::alloc_into`] and
    /// [`DmaPool::alloc`] (the single definition).
    fn alloc_inner<P: PageTable>(
        &mut self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        requested: usize,
        carve: Carve,
    ) -> Result<DmaBuffer, DmaError> {
        let Carve {
            addr_limit,
            backing,
            coherence,
        } = carve;
        if requested == 0 {
            return Err(DmaError::ZeroSize);
        }
        let translator = backing.translator();
        let data_pages = match backing {
            Backing::Contiguous(_) => {
                1usize << carve_order(requested).ok_or(DmaError::SizeUnsupported)?
            }
            Backing::Scattered(_) => requested.div_ceil(PAGE_SIZE),
        };
        let block_pages = data_pages
            .checked_add(GUARD_SLOTS)
            .ok_or(DmaError::SizeUnsupported)?;
        let leading_guard_slot = self
            .find_free_run(block_pages)
            .ok_or(DmaError::Alloc(AllocError::OutOfMemory))?;
        let first_data_slot = leading_guard_slot + 1;
        let trailing_guard_slot = leading_guard_slot + 1 + data_pages;
        // Grow the lazy slot bookkeeping to cover the chosen run, and make
        // room for the record, before any frame is reserved or page mapped,
        // so a bookkeeping-heap refusal fails the request cleanly with
        // nothing to roll back.
        self.ensure_slots(trailing_guard_slot + 1)?;
        self.reserve_record()?;

        // An untranslated device is never handed memory past its reach; a
        // translated one reaches its frames through an IOVA its domain keeps
        // within reach, so only its unit's own reach bounds them.
        let (blocks, reach) = match backing {
            Backing::Contiguous(translator) => {
                let ceiling = match translator {
                    Some(translator) => translator.ceiling(),
                    None => (addr_limit != 0).then_some(PhysAddr::new(addr_limit)),
                };
                let order = data_pages.trailing_zeros();
                let frame = frames.alloc_order_under_user(MemoryClass::Dma, order, ceiling)?;
                let mut blocks = Chunks::new();
                // The first block sits inline, so the push allocates nothing.
                let _ = blocks.try_push(FrameBlock { frame, order });
                let reach = translator.map_or(Reach::At(frame.start()), Reach::Through);
                (blocks, reach)
            }
            Backing::Scattered(translator) => (
                frames.alloc_chunks_user(
                    MemoryClass::Dma,
                    data_pages as u64,
                    translator.ceiling(),
                )?,
                Reach::Through(translator),
            ),
        };
        // Every draw of a page or more is at least one block.
        let first = blocks
            .first()
            .map(|block| block.frame.start())
            .ok_or(DmaError::SizeUnsupported)?;

        // Scrubbed before it is mapped: once an entry exists, any thread of
        // the process can read what the frames held for their previous owner.
        if let Err(err) = blocks
            .iter()
            .try_for_each(|block| scrub_block(phys, block.frame, 1 << block.order))
        {
            frames.free_chunks(&blocks);
            return Err(err);
        }
        self.map_data_pages(space, frames, phys, first_data_slot, &blocks, coherence)?;
        let device_addr = match reach {
            Reach::At(base) => base.as_u64(),
            Reach::Through(translator) => match translator.map(&blocks, addr_limit) {
                Ok(iova) => iova,
                Err(err) => {
                    // A map the unit could not confirm gone may still be
                    // reachable, so its frames are never freed.
                    let release = err != DmaError::Unconfirmed;
                    self.rollback_partial_map(
                        space,
                        frames,
                        phys,
                        first_data_slot,
                        data_pages,
                        &blocks,
                        release,
                    );
                    return Err(err);
                }
            },
        };

        // Mark every slot — guard and data alike — as used so no
        // future allocation can overlap them.
        for s in leading_guard_slot..=trailing_guard_slot {
            self.slot_used[s] = true;
        }

        let virt = self.virt_of_slot(first_data_slot);
        let record = Record {
            leading_guard_slot,
            data_pages,
            blocks,
            device_addr,
            device: translator.map_or(DeviceSide::Physical, DeviceSide::Mapped),
        };
        // Room was reserved above, and the run was free, so the insert can
        // neither allocate nor displace another record.
        let _ = self.allocations.try_insert(virt.as_u64(), record);
        Ok(DmaBuffer {
            virt,
            phys: first,
            device: device_addr,
            len: data_pages * PAGE_SIZE,
        })
    }

    /// The one release behind [`Self::free_from`] and [`Self::free_at`].
    ///
    /// # Errors
    ///
    /// As [`DmaPool::free`].
    fn free_inner<P: PageTable>(
        &mut self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        buf: DmaBuffer,
        retire: &mut dyn Retire,
    ) -> Result<(), DmaError> {
        let key = buf.virt.as_u64();
        let record = self.allocations.get(&key).ok_or(DmaError::UnknownBuffer)?;
        let data_pages = record.data_pages;
        let first_data_slot = record.leading_guard_slot + 1;

        // A page already cleared is where the loop means to leave it.
        let mut unmapped = 0_u64;
        let cleared = (0..data_pages).try_for_each(|i| {
            let page = Page::from_addr(self.virt_of_slot(first_data_slot + i))?;
            match space.unmap(page) {
                Ok(_) | Err(PageTableError::NotMapped) => {
                    unmapped += 1;
                    Ok(())
                }
                Err(err) => Err(err),
            }
        });
        // Even after a failed page, and before any scrub: the pages cleared
        // are gone here, but a late store through another CPU or a snapshot
        // would survive into the next owner. Only they leave the views; the
        // rest are still mapped, and still the driver's.
        let base = self.virt_of_slot(first_data_slot).as_u64();
        space.shoot_remote(base, data_pages as u64);
        retire.retire(base, unmapped);
        // A page still mapped, a device that may still reach the carve, or a
        // block that cannot be scrubbed keeps its record, so teardown
        // surrenders the frames rather than leaking them. The device goes
        // before the scrub, so nothing it writes survives it.
        cleared?;
        let record = self
            .allocations
            .get_mut(&key)
            .ok_or(DmaError::UnknownBuffer)?;
        let mut deferred = false;
        if let DeviceSide::Mapped(translator) = record.device {
            deferred = translator.defer_free(record.device_addr, &mut record.blocks)?;
            if !deferred {
                translator.unmap(record.device_addr)?;
            }
            record.device = DeviceSide::Unmapped;
        }
        if !deferred {
            scrub_blocks(phys, &record.blocks)?;
        }

        let record = self
            .allocations
            .remove(&key)
            .ok_or(DmaError::UnknownBuffer)?;
        for s in record.leading_guard_slot..=record.leading_guard_slot + 1 + data_pages {
            self.slot_used[s] = false;
        }
        // Deferred blocks left with the translation, which frees them once
        // their unit confirms them gone.
        free_blocks(frames, &record.blocks)
    }

    fn virt_of_slot(&self, slot: usize) -> VirtAddr {
        VirtAddr::new(self.base.as_u64() + ((slot as u64) << PAGE_SHIFT))
    }

    /// Extend the lazy slot bookkeeping to cover at least `len` slots
    /// (never past `capacity_pages`), failing closed on bookkeeping-heap
    /// exhaustion rather than panicking.
    fn ensure_slots(&mut self, len: usize) -> Result<(), DmaError> {
        if len <= self.slot_used.len() {
            return Ok(());
        }
        debug_assert!(len <= self.capacity_pages);
        self.slot_used
            .try_reserve(len - self.slot_used.len())
            .map_err(|_| DmaError::Alloc(AllocError::OutOfMemory))?;
        self.slot_used.resize(len, false);
        Ok(())
    }

    /// First slot index of a run of `len` consecutive *unused* slots, or
    /// `None` if no such run exists. A slot beyond the lazily-grown
    /// bookkeeping is free, so the search is bounded by the window's
    /// structural capacity while costing only the peak slots ever used.
    fn find_free_run(&self, len: usize) -> Option<usize> {
        if len == 0 || len > self.capacity_pages {
            return None;
        }
        let mut start = 0;
        while start + len <= self.capacity_pages {
            let mut ok = true;
            for i in 0..len {
                if self.slot_used.get(start + i).copied().unwrap_or(false) {
                    start = start + i + 1;
                    ok = false;
                    break;
                }
            }
            if ok {
                return Some(start);
            }
        }
        None
    }

    /// Undo a carve whose first `mapped_so_far` pages were mapped: they were
    /// never published beyond the page table, but a sibling thread may have
    /// touched them on another CPU, so the blocks are freed only once no CPU
    /// can reach them, and scrubbed again — and only if `release` says no
    /// device can still reach them either. Errors are dropped — this is
    /// already the failure path — and a block that cannot be scrubbed is
    /// kept.
    // Each argument is a distinct piece of the carve being undone.
    #[allow(clippy::too_many_arguments)]
    fn rollback_partial_map<P: PageTable>(
        &self,
        space: &mut AddressSpace<P>,
        frames: &FrameAllocator,
        phys: &dyn PhysMap,
        first_data_slot: usize,
        mapped_so_far: usize,
        blocks: &[FrameBlock],
        release: bool,
    ) {
        for i in 0..mapped_so_far {
            let virt = self.virt_of_slot(first_data_slot + i);
            if let Ok(page) = Page::from_addr(virt) {
                let _ = space.unmap(page);
            }
        }
        space.shoot_remote(
            self.virt_of_slot(first_data_slot).as_u64(),
            mapped_so_far as u64,
        );
        if release {
            for block in blocks {
                if scrub_block(phys, block.frame, 1 << block.order).is_ok() {
                    let _ = frames.free_order(block.frame, block.order);
                }
            }
        }
    }
}

/// The device a carve is for, as its grant describes it.
#[derive(Clone, Copy)]
pub struct CarveDevice {
    /// Where the device's reach ends; `0` for no end.
    pub addr_limit: u64,
    /// The domain a translation unit maps the device through, if one does.
    pub translator: Option<DmaTranslator>,
    /// How the device's accesses meet the CPU's caches.
    pub coherence: DmaCoherence,
}

/// A carve as the window makes it: its device's reach, how its frames must
/// lie, and how they are mapped.
#[derive(Clone, Copy)]
struct Carve {
    addr_limit: u64,
    backing: Backing,
    coherence: DmaCoherence,
}

/// What a carve's frames must be.
#[derive(Clone, Copy)]
enum Backing {
    /// One physically contiguous block: for a device that addresses frames
    /// itself, and for a pool the kernel reaches through one slice.
    Contiguous(Option<DmaTranslator>),
    /// As many blocks as the frames give, for a device a translation unit
    /// lets reach scattered frames.
    Scattered(DmaTranslator),
}

impl Backing {
    const fn translator(self) -> Option<DmaTranslator> {
        match self {
            Self::Contiguous(translator) => translator,
            Self::Scattered(translator) => Some(translator),
        }
    }
}

impl<'a, P: PageTable> DmaPool<'a, P> {
    /// Construct a new pool managing the virtual range
    /// `[base, base + capacity_pages * PAGE_SIZE)`.
    ///
    /// The pool maps frames from `frames` into the supplied
    /// [`AddressSpace`]. The borrow of `frames` is explicit (rather
    /// than `'static`) so the host-side tests do not need to leak
    /// their allocators and so a future per-process kernel layout can
    /// hand a process a `&FrameAllocator` carved from the global pool.
    ///
    /// `phys` is the kernel's direct physical map; the pool reaches a
    /// buffer's frames through it so the CPU sees exactly the bytes
    /// the device DMAs to.
    ///
    /// # Errors
    ///
    /// * [`DmaError::InvalidPoolConfig`] if `capacity_pages == 0`,
    ///   `base` is not page-aligned, or the window size overflows.
    pub fn new(
        address_space: AddressSpace<P>,
        base: VirtAddr,
        capacity_pages: usize,
        frames: &'a FrameAllocator,
        phys: &'a dyn PhysMap,
        coherence: DmaCoherence,
    ) -> Result<Self, DmaError> {
        let window = DmaWindowMap::new(base, capacity_pages)?;
        Ok(Self {
            address_space,
            window,
            phys,
            frames,
            translator: None,
            coherence,
        })
    }

    /// The pool, for a device that reaches its carves through `translator`'s
    /// domain: every later carve is mapped there, and unmapped from it before
    /// its frames are freed.
    #[must_use]
    pub fn translated(mut self, translator: DmaTranslator) -> Self {
        self.translator = Some(translator);
        self
    }

    /// Allocate a contiguous DMA region of at least `requested` bytes the
    /// device reaches below `addr_limit` (`0` declares no constraint): its
    /// frames lie below it, or, on a translated pool, its IOVA does.
    ///
    /// The returned buffer's `len` is `requested` rounded up to the
    /// next power-of-two multiple of [`PAGE_SIZE`].
    ///
    /// # Errors
    ///
    /// * [`DmaError::ZeroSize`] — `requested == 0`.
    /// * [`DmaError::SizeUnsupported`] — `requested` would round to a
    ///   buddy order exceeding [`MAX_ORDER`].
    /// * [`DmaError::Alloc`]`(`[`AllocError::OutOfMemory`]`)` — no
    ///   contiguous frame block of the requested order is free below the
    ///   limit, or no suitable run of unused slots exists in the virtual
    ///   window; `OutOfRange` when no RAM lies below the limit at all.
    /// * [`DmaError::PageTable`] — propagated from the
    ///   [`AddressSpace`] when a mapping operation fails.
    pub fn alloc(&mut self, requested: usize, addr_limit: u64) -> Result<DmaBuffer, DmaError> {
        // The pool hands its carves out as one slice of the direct map, so
        // each is one contiguous block, translated or not.
        self.window.alloc_inner(
            &mut self.address_space,
            self.frames,
            self.phys,
            requested,
            Carve {
                addr_limit,
                backing: Backing::Contiguous(self.translator),
                coherence: self.coherence,
            },
        )
    }

    /// Free a previously-allocated DMA buffer.
    ///
    /// Every byte of the data region is zeroed (via the audited
    /// `zeroize` crate's volatile clear) *before* the backing frames
    /// are returned to the [`FrameAllocator`], so neither a later
    /// allocation nor a forensic dump of free memory can recover the
    /// credentials the buffer once held. The clear
    /// runs through the direct map, i.e. on the same physical frames
    /// the device used.
    ///
    /// # Errors
    ///
    /// * [`DmaError::UnknownBuffer`] — the buffer's `virt` is not the
    ///   start of a live allocation in this pool (covers double-free
    ///   and cross-pool free).
    /// * [`DmaError::DirectMap`] — the buffer's frames are outside the
    ///   direct physical map, so the zero-on-free clear cannot run
    ///   (a platform-config bug).
    /// * [`DmaError::PageTable`] — a page [`AddressSpace::unmap`] could
    ///   not clear. A page already cleared is not an error.
    /// * [`DmaError::Unconfirmed`] — the buffer's translation unit could
    ///   not confirm its device lost the buffer.
    /// * [`DmaError::Alloc`] — the allocator refused the scrubbed block.
    ///
    /// A live buffer's range is shot down whatever becomes of it, and the
    /// pages it cleared are retired. Only an allocator refusal drops the
    /// record; after any error before it the block stays live in the pool,
    /// out of reuse, since a kernel pool has no custodian to surrender it
    /// to.
    pub fn free(&mut self, buf: DmaBuffer) -> Result<(), DmaError> {
        self.window
            .free_from(&mut self.address_space, self.frames, self.phys, buf)
    }

    /// Borrow the data bytes of `buf` mutably.
    ///
    /// The slice points at the buffer's physical frames through the
    /// direct map, so writes are seen by the device and vice versa.
    ///
    /// # Errors
    ///
    /// * [`DmaError::UnknownBuffer`] if `buf` is not a live allocation
    ///   of this pool.
    /// * [`DmaError::DirectMap`] if the frames are outside the direct
    ///   physical map.
    pub fn bytes_mut(&mut self, buf: DmaBuffer) -> Result<&mut [u8], DmaError> {
        let (phys, len) = self.window.live_frames(&buf)?;
        let ptr = self.phys.translate(phys, len).ok_or(DmaError::DirectMap)?;
        // SAFETY: `translate` returned a pointer to `len` bytes of this
        // buffer's frames; the slot bitmap proves no other live
        // allocation covers them and `&mut self` makes the borrow
        // exclusive for the returned slice's lifetime.
        unsafe { slice_within(ptr.as_ptr(), len, 0, len) }.ok_or(DmaError::DirectMap)
    }

    /// Raw, non-null base pointer to the data slots of `buf`.
    ///
    /// Companion to [`Self::bytes_mut`] that hands out only a
    /// pointer (no slice borrow), so a future user-space-driver
    /// host shim can mint an owned `DmaSlab` carrying the pointer
    /// independently of the pool's mutable borrow. The disjointness
    /// witness is the pool's slot bitmap: one slot ↔ one
    /// allocation, so the bytes covered by `[ptr, ptr + buf.len())`
    /// alias nothing else the pool has minted.
    ///
    /// # Safety-invariant
    ///
    /// The returned pointer is valid for reads and writes of
    /// `buf.len()` bytes until the buffer is freed via
    /// [`Self::free`]. The caller (a future Stage 4.D Item 0
    /// `KernelVirtioHost`) must ensure the slab carrying this
    /// pointer is dropped strictly before
    /// [`Self::free`] is called for the same `buf`.
    ///
    /// # Errors
    ///
    /// [`DmaError::UnknownBuffer`] if `buf` does not name a live
    /// allocation of this pool.
    pub fn slot_base(&self, buf: &DmaBuffer) -> Result<NonNull<u8>, DmaError> {
        let (phys, len) = self.window.live_frames(buf)?;
        // The pointer names the buffer's physical frames through the
        // direct map. The slot bitmap proves no other live allocation
        // covers `[ptr, ptr + len)`, and every write through it is
        // gated by the slot's exclusive owner (the `DmaSlab` that
        // records the same buffer).
        self.phys.translate(phys, len).ok_or(DmaError::DirectMap)
    }

    /// Borrow the data bytes of `buf` immutably.
    ///
    /// # Errors
    ///
    /// See [`Self::bytes_mut`].
    pub fn bytes(&self, buf: DmaBuffer) -> Result<&[u8], DmaError> {
        let (phys, len) = self.window.live_frames(&buf)?;
        let ptr = self.phys.translate(phys, len).ok_or(DmaError::DirectMap)?;
        // SAFETY: as `bytes_mut`, but the shared borrow of `self`
        // yields a shared slice; the pool holds the single live record
        // for these bytes so no `&mut` alias exists. The base pointer
        // already covers `len` bytes, so no further arithmetic occurs.
        Ok(unsafe { core::slice::from_raw_parts(ptr.as_ptr().cast_const(), len) })
    }

    /// Number of live allocations.
    #[must_use]
    pub fn live(&self) -> usize {
        self.window.live()
    }

    /// Total pages in the pool's virtual window.
    #[must_use]
    pub fn capacity_pages(&self) -> usize {
        self.window.capacity_pages()
    }
}

#[cfg(all(test, not(loom)))]
mod tests;
