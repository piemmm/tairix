//! Retained live user address space — the production target the
//! `mem_map` / `mmio_map` syscall producers mutate (`plans/PI.md` P10
//! chunk 5d-0-ii (b′); the `plans/SPAWN.md` `SP5b` production follow-on).
//!
//! Post-spawn an address space was previously captured only as an immutable
//! [`FrozenAddressSpace`] snapshot — enough
//! for the read-only user-memory copy path, but the live arch
//! [`AddressSpace<P>`] was *dropped*. `mem_map` / `mmio_map` need the
//! *running* space to stay **mutable** so a process can grow its own heap or
//! a driver can map a granted device window into its own address space.
//!
//! This module is that retained, mutable space, behind one object-safe
//! boundary so `kernel/core` can hold it without naming a concrete
//! page-table backend `P`:
//!
//! * [`LiveUserSpace`] — the object-safe, mutating operations the producers
//!   reach (anonymous map/unmap; device-window map). `Send` so the boxed
//!   space can move into the process's shared, spin-locked `ProcessSpace`
//!   (`kernel/core`), whose threads may run on any CPU; it is deliberately
//!   **not** `Sync`, because that lock — not an ambient one-task-per-space
//!   assumption — is what makes the `&mut` its methods take non-aliasing.
//! * [`LiveSpace`] — the generic concrete implementation over a port's
//!   [`PageTable`] backend `P`, the kernel direct map `M`, the kernel
//!   [`FrameAllocator`] (anonymous frames), and an [`MmioWindowMap`]
//!   (device windows). It composes the already-audited
//!   [`map_anonymous`] / [`unmap_anonymous`] and
//!   [`MmioWindowMap`] mechanisms — there is no second mapping path
//!   (one definition each).
//!
//! The capability posture (`mem_map` is unprivileged, the `mmio_map` grant
//! is owner-checked) and the placement of a
//! non-`FIXED` anonymous region belong to the higher-level `kernel/core`
//! producer that calls these — this layer knows only the page table, the
//! direct map, the frame allocator, and the device-window allocator.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};

use tairix_collections::{OrderedSet, RangeSet};
use tairix_inline::ArrayVec;
use tairix_log::Sink;
use tairix_reclaim::MemoryPressure;
use tairix_sync::SpinLock;

use crate::anon::{
    detach_anonymous, map_anonymous, map_errno, page_at, unmap_anonymous, zero_frame, AnonError,
};
use crate::anon_window::AnonWindowMap;
use crate::coldscan::{ColdPageScanner, ColdScanError};
use crate::dma::{CarveDevice, DmaCustodian, DmaError, DmaWindowMap};
use crate::error::AllocError;
use crate::filemap::{map_file_page, unmap_file_region};
use crate::frame::{Frame, FrameAllocator, MemoryClass, PAGE_SIZE};
use crate::mmio::{MmioError, MmioWindowMap, SharedMemory};
use crate::phys::PhysMap;
use crate::pressure::ramzip_scan_budget;
use crate::ramzip::{
    FaultError, PageCandidate, Ramzip, RamzipFaultOutcome, RamzipReclaimSummary, VmContext,
    WarmOutcome, COMPRESS_BATCH,
};
use crate::retire::{Retire, Retiring, SpaceTlb, TlbContext, Unpublished};
use crate::vmm::{
    AddressSpace, FrozenAddressSpace, MapFlags, Page, PageTable, PageTableError, VirtAddr,
};

/// What an installed code page is mapped with: read and execute, never write.
pub const CODE_FLAGS: MapFlags = MapFlags::READ.union(MapFlags::EXEC).union(MapFlags::USER);

/// Why a [`LiveUserSpace`] operation failed.
///
/// A faithful union of the two underlying mechanisms' errors so the
/// `kernel/core` producer can fold each onto a stable `Errno` without this
/// layer knowing the ABI error type (`kernel/mem` names
/// no `lib/abi` error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveSpaceError {
    /// An anonymous map/unmap failed (OOM, not-mapped, misaligned, …).
    Anon(AnonError),
    /// A device-window map failed (no virtual slot, page-table refusal, …).
    Mmio(MmioError),
    /// A DMA carve failed (no contiguous block, addressing-limit exceeded,
    /// no virtual slot, …).
    Dma(DmaError),
}

impl From<AnonError> for LiveSpaceError {
    fn from(err: AnonError) -> Self {
        Self::Anon(err)
    }
}

impl From<MmioError> for LiveSpaceError {
    fn from(err: MmioError) -> Self {
        Self::Mmio(err)
    }
}

impl From<DmaError> for LiveSpaceError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
}

/// A live DMA buffer the [`LiveUserSpace::alloc_dma`] carve returns.
///
/// `cpu_va` is the base **user virtual address** the driver's CPU accesses
/// go through; `device_addr` is where the device reaches the buffer: an IOVA
/// in its node's domain when a translation unit stands between them, else the
/// physical base of its one backing block, which the `kernel/core` producer
/// may still rebase through an inbound viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaMapping {
    /// Base user virtual address of the mapped, guard-bracketed buffer.
    pub cpu_va: u64,
    /// Where the device reaches the buffer's first byte.
    pub device_addr: u64,
    /// Backing length in bytes: the request rounded up to a power-of-two
    /// page count, or for a translated device to whole pages.
    pub len: usize,
}

/// The object-safe, mutating view of a process's live address space.
///
/// Held by `kernel/core` as a `Box<dyn LiveUserSpace + Send>` inside the
/// process's shared, spin-locked `ProcessSpace`, reached from a running
/// thread's syscall path through the per-CPU publication. Every method takes
/// `&mut self`; that lock is what makes the access exclusive.
pub trait LiveUserSpace: Send {
    /// The space's standing with the TLBs, for the dispatcher to keep
    /// current: an unmap reaches the CPUs running it at once, and the rest
    /// catch up on its generation when they return.
    fn tlb_context(&self) -> Arc<TlbContext>;

    /// Map `page_count` fresh, zeroed `RW|USER` pages at the page-aligned
    /// `base_va` into this space, returning `base_va` on success.
    ///
    /// The placement (the value of `base_va`) is the producer's decision;
    /// this is the `FIXED` map mechanism. The pages are zeroed before they
    /// are user-visible and are never executable;
    /// a part-way failure unwinds every page already mapped, leaving the
    /// space unchanged.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] carrying the precise
    /// [`AnonError`] (zero length, misalignment,
    /// overflow, OOM, …).
    fn map_anonymous(&mut self, base_va: u64, page_count: u64) -> Result<u64, LiveSpaceError>;

    /// Map `page_count` fresh, zeroed `RW|USER` pages at a **kernel-chosen**
    /// base — the non-`FIXED` `mem_map` placement (`plans/PI.md` 5d-0-ii (c))
    /// — returning the base virtual address the space placed them at.
    ///
    /// The placement is drawn from this task's anonymous heap window so a
    /// second non-`FIXED` request never overlaps the first, the program
    /// image, its stack, or a granted device window. The pages obey the same
    /// W^X / zero-on-map / all-or-nothing contract as [`Self::map_anonymous`];
    /// the reserved range is released back to the window on a mapping failure
    /// so a failed call consumes no address space.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::OutOfMemory`] when the heap
    /// window or the frame allocator is exhausted (deterministic OOM),
    /// or the precise placement/map error otherwise.
    fn map_anonymous_placed(&mut self, page_count: u64) -> Result<u64, LiveSpaceError>;

    /// Release the `page_count`-page region based at `base_va`. Each frame
    /// is zeroed and returned to the allocator only once no CPU and no
    /// `retire` view can reach it (zero on free). The whole range is
    /// validated mapped before any page is torn down (fail closed on a bad
    /// range).
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] (e.g. [`AnonError::NotMapped`] when the
    /// range is not one this space mapped).
    fn unmap_anonymous(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
    ) -> Result<(), LiveSpaceError>;

    /// Reserve `page_count` pages of *address space* for a demand-paged
    /// **anonymous** mapping at a **kernel-chosen** base (the non-`FIXED`
    /// `mem_map`), returning that base. No page-table entry is written and
    /// no frame is drawn: the region is backed one zeroed `RW|USER` page at
    /// a time by [`Self::map_anonymous`] from the kernel's anonymous fault
    /// path, so reserving a large region costs nothing until it is touched
    /// — a huge `mem_map` never zeroes and commits thousands of pages in
    /// one non-preemptible syscall.
    ///
    /// The placement is drawn from this task's anonymous heap window so a
    /// second reservation never overlaps the first, the program image, its
    /// stack, or a granted device window. Anonymous pages must all be
    /// backable, so the heap window's span is clamped to physical RAM.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::OutOfMemory`] when the heap
    /// window is exhausted, or the precise placement error otherwise.
    fn reserve_anonymous(&mut self, page_count: u64) -> Result<u64, LiveSpaceError>;

    /// Reserve `page_count` pages of *address space* at a kernel-chosen base
    /// for a demand-paged anonymous region whose physical headroom the caller
    /// takes **incrementally**, one growth step at a time
    /// ([`Self::commit_anonymous`]) — the thread-stack shape.
    ///
    /// The placement is [`Self::reserve_anonymous`]'s; what differs is the
    /// no-overcommit bargain. A `mem_map` reservation commits every page up
    /// front so a first touch can never fail, which is right for a region whose
    /// pages a program means to use. A *stack* is a span whose depth is unknown
    /// and mostly untouched, so committing its whole extent would charge a
    /// thread the RAM of its worst case — a process's first thread's growth room
    /// carries no commitment for exactly that reason, and a thread's must not
    /// differ. Growth therefore takes headroom as it descends and fails closed
    /// there, and the release credits back nothing (a page that never faulted in
    /// was never committed).
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::OutOfMemory`] when the heap
    /// window is exhausted or the region's records cannot be allocated, or
    /// the precise placement error otherwise.
    fn reserve_anonymous_growable(&mut self, page_count: u64) -> Result<u64, LiveSpaceError>;

    /// Reserve exactly the `page_count`-page anonymous region based at the
    /// page-aligned `base_va` (the `FIXED` `mem_map`), returning `base_va`.
    /// Like [`Self::reserve_anonymous`] this commits no frame and writes no
    /// page-table entry; the pages fault in one at a time. The caller owns
    /// the placement, so a `base_va` that later collides with resident
    /// memory fails closed when its first page faults in.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::ZeroLength`] for a zero page
    /// count, [`AnonError::Unaligned`] for a misaligned base.
    fn reserve_anonymous_at(
        &mut self,
        base_va: u64,
        page_count: u64,
    ) -> Result<u64, LiveSpaceError>;

    /// Reserve physical headroom for `page_count` demand-paged anonymous
    /// pages whose *address space is already reserved* — the stack-growth
    /// case, where the growth room was carved at spawn and only the
    /// no-overcommit commitment is taken here, one growth step at a time,
    /// immediately before the pages are faulted in by [`Self::map_anonymous`].
    ///
    /// This is the commitment half of [`Self::reserve_anonymous`] without the
    /// address-space placement (the stack span owns the address space). The
    /// committed pages join this space's outstanding tally and are consumed
    /// as they fault in or released on teardown, exactly like a `mem_map`
    /// reservation, so stack growth is bounded by the same no-overcommit
    /// budget and fails closed rather than killing the task on first touch.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] ([`AnonError::OutOfMemory`]) when the
    /// commitment cannot be admitted without overcommitting.
    fn commit_anonymous(&mut self, page_count: u64) -> Result<(), LiveSpaceError>;

    /// Reserve `page_count` pages of *address space* for a demand-paged,
    /// read-only file mapping at a **kernel-chosen** base, returning that
    /// base. No page table entry is written and no frame is drawn: the
    /// region is backed one page at a time by [`Self::map_file_page_at`]
    /// from the kernel's fault path, so reserving a huge region costs
    /// nothing until it is touched.
    ///
    /// The placement is drawn from this task's file-mapping window —
    /// deliberately separate from the anonymous heap window, whose span is
    /// clamped to physical RAM (anonymous pages must all be backable); a
    /// file mapping is bounded only by address space.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::OutOfMemory`] when the file
    /// window is exhausted (or absent on a degenerate configuration with no
    /// address space left above the heap window), or the precise
    /// placement error otherwise.
    fn reserve_file_region(&mut self, page_count: u64) -> Result<u64, LiveSpaceError>;

    /// Make the single page at the page-aligned `va` resident inside a
    /// region previously returned by [`Self::reserve_file_region`],
    /// carrying `contents` (at most one page; a short slice is the page
    /// straddling end-of-file, zero-filled past it). The page is mapped
    /// read-only and never executable.
    ///
    /// `va` must lie inside a *live* reserved file region: an address in
    /// the window but outside every reservation is refused (fail closed) —
    /// the fault path never backs address space the task did not map.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::NotMapped`] when `va` is not
    /// covered by a live file region, [`AnonError::Map`] when the page is
    /// already resident (a benign fault race; the caller retries the
    /// access), or the precise frame/fill error otherwise.
    fn map_file_page_at(&mut self, va: u64, contents: &[u8]) -> Result<(), LiveSpaceError>;

    /// Release the whole file region based at `base_va` (`page_count`
    /// pages, exactly as reserved), sparsely unmapping the pages fault
    /// history made resident — each frame zeroed on free once no CPU and no
    /// `retire` view can reach it — and returning the reservation to the
    /// file window. Returns the number of pages that were resident.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::NotMapped`] when
    /// `(base_va, page_count)` is not a live file region of this space
    /// (fail closed: nothing is torn down).
    fn release_file_region(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
    ) -> Result<u64, LiveSpaceError>;

    /// Map `len` bytes of device physical memory beginning at `phys_base`
    /// into this space, returning the kernel-chosen base user virtual
    /// address of the new, guard-bracketed, caching-disabled,
    /// non-executable window.
    ///
    /// The producer has already resolved and validated the grant the window
    /// comes from (owner-checked, kind, length);
    /// this only performs the page-table mechanism.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Mmio`] carrying the precise
    /// [`MmioError`] (no free virtual slot,
    /// page-table refusal, …).
    fn map_device_window(&mut self, phys_base: u64, len: usize) -> Result<u64, LiveSpaceError>;

    /// Map `len` bytes of a **linear framebuffer** beginning at `phys_base`
    /// into this space, returning the kernel-chosen base user virtual address
    /// of the new, guard-bracketed, non-executable window.
    ///
    /// Identical guard-bracketed slot mechanism as [`Self::map_device_window`],
    /// but the scan-out pages are mapped non-cacheable **Normal** memory
    /// instead of Device-strongly-ordered: a framebuffer is bulk pixel memory
    /// the CPU fills a whole frame at a time and the display engine reads back
    /// as a DMA master, so the slow per-access Device ordering registers need
    /// is exactly wrong for it (a full-frame blit of Device memory costs tens
    /// of seconds under emulation). The producer has already resolved and
    /// validated the framebuffer grant the window comes from (owner-checked,
    /// kind, length); this only performs the page-table mechanism.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Mmio`] carrying the precise [`MmioError`] (no free
    /// virtual slot, page-table refusal, …).
    fn map_framebuffer_window(&mut self, phys_base: u64, len: usize)
        -> Result<u64, LiveSpaceError>;

    /// Map a linear framebuffer backed by ordinary coherent RAM as
    /// guard-bracketed, non-executable, write-back memory.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Mmio`] carrying the precise [`MmioError`].
    fn map_writeback_framebuffer_window(
        &mut self,
        phys_base: u64,
        len: usize,
    ) -> Result<u64, LiveSpaceError>;

    /// Unmap every device and framebuffer window whose physical span `keep`
    /// refuses, handing each released window's page-aligned base and page
    /// count to `unmapped` once its entries are gone from the page table and
    /// from every CPU's TLB.
    ///
    /// The frames are the device's, so nothing is freed.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Mmio`] from the first unmap that fails; the windows
    /// before it are released.
    fn retain_device_windows(
        &mut self,
        keep: &mut dyn FnMut(u64, u64) -> bool,
        unmapped: &mut dyn FnMut(u64, u64),
    ) -> Result<(), LiveSpaceError>;

    /// Carve a zeroed, coherent DMA buffer of `len` bytes into this space,
    /// returning its CPU virtual base and the base its device reaches it at
    /// ([`DmaMapping`]): one physically contiguous block for an untranslated
    /// device, its pages wherever frames are free for one a translation unit
    /// confines.
    ///
    /// The buffer is mapped `RW|USER`, never executable, guard-bracketed, and
    /// zeroed before it is user-visible. When `addr_limit` is non-zero the
    /// device reaches it wholly below it (the granted device addressing
    /// constraint) — its frames for an untranslated device, its IOVA for a
    /// translated one — or the request is refused; `addr_limit == 0`
    /// declares no constraint.
    ///
    /// The producer has already resolved and validated the grant the buffer
    /// is bounded by (owner-checked, kind, length); this only performs the
    /// carve and page-table mechanism.
    ///
    /// Every carve names its `custodian` and first reserves room in its
    /// custody for the block's surrender, returned if the carve fails. The
    /// space takes the first carve's custodian as its own and refuses any
    /// other, so its teardown has one place to surrender to: when dropped it
    /// surrenders every buffer still live there rather than to the
    /// allocator, because the device may outlive the driver.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Dma`] carrying the precise [`DmaError`]: zero
    /// length, an untranslated carve past the max buddy order, no block below
    /// the limit ([`AllocError::OutOfRange`] when no RAM lies below it), frames the machine cannot spare past the
    /// kernel reserve ([`DmaError::Alloc`]), no virtual slot, a page-table or
    /// direct-map failure, the translation's refusal to map the carve
    /// ([`DeviceTranslation::map`](crate::dma::DeviceTranslation::map)), a
    /// custody that refused the reservation ([`DmaError::DeviceGone`],
    /// [`DmaError::NoCustody`], [`DmaError::Alloc`]), or a custodian other
    /// than the space's own ([`DmaError::CustodianMismatch`]).
    fn alloc_dma(
        &mut self,
        len: usize,
        addr_limit: u64,
        custodian: DmaCustodian,
    ) -> Result<DmaMapping, LiveSpaceError>;

    /// Release the DMA buffer whose CPU virtual base is `cpu_va`, zeroing
    /// every backing byte (zero-on-free) once no CPU and no `retire` view can
    /// reach it, before its frames return to the allocator — the symmetric
    /// free for [`Self::alloc_dma`].
    ///
    /// A long-running driver that issues many transfers reclaims each
    /// request's buffers through this rather than leaking frames until it
    /// exits. Only `cpu_va` is taken from the caller; the buffer's extent is
    /// the allocator's own authoritative record, so a `cpu_va` that is not the
    /// base of a live carve in this space fails closed (covering a forged,
    /// stale, or double free) without releasing anything.
    ///
    /// Reports the byte length released. The block's custody reservation is
    /// returned whether or not its release completes, since once its record
    /// is gone it can never be surrendered.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Dma`] — [`DmaError::UnknownBuffer`] when `cpu_va` is
    /// not the base of a live DMA carve of this space,
    /// [`DmaError::Unconfirmed`] when the unit could not confirm a translated
    /// carve unmapped (its blocks are then never reused), or the direct-map,
    /// page-table, or allocator refusal that stopped the release part-way; a
    /// block that is not returned to the allocator stays allocated.
    fn free_dma(&mut self, cpu_va: u64, retire: &mut dyn Retire) -> Result<usize, LiveSpaceError>;

    /// Map an existing, kernel-owned **shared-memory region** whose backing
    /// is a *list* of physically-contiguous chunks (`(phys_base, pages)`)
    /// into this space as one contiguous `RW|USER` (never executable),
    /// guard-bracketed window mapped as `memory` says, returning the
    /// kernel-chosen base user virtual address.
    ///
    /// A region larger than the frame allocator's single-block ceiling is
    /// backed by several blocks the shared-region registry allocated, and this
    /// maps them back-to-back in virtual space so the process sees one flat
    /// buffer (the display frame ring). The frames belong to the registry,
    /// which zeroed them on allocation and frees them only when the owner and
    /// every grantee have released the region; this installs page-table
    /// entries only, released by [`Self::unmap_shared`] or a space drop
    /// without freeing the frames.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Mmio`] carrying the precise [`MmioError`] (an empty
    /// or malformed chunk list, no free virtual run, a page-table refusal) —
    /// the shared mapping reuses the guarded-window mechanism.
    fn map_shared_chunks(
        &mut self,
        chunks: &[(u64, u64)],
        memory: SharedMemory,
    ) -> Result<u64, LiveSpaceError>;

    /// Whether the shared window has a free run for a region of `pages`
    /// now, so a caller can refuse before drawing frames
    /// [`Self::map_shared_chunks`] would have nowhere to map.
    fn shared_room(&self, pages: u64) -> bool;

    /// Release the shared-region mapping based at `base_va` from this space,
    /// tearing down only its page-table entries (the registry owns the
    /// frames). `len` is advisory \u2014 the allocator releases exactly the pages
    /// it recorded for `base_va`.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Mmio`] \u2014 [`MmioError::UnknownRegion`] if `base_va`
    /// does not name a live shared mapping of this space (fail closed), or a
    /// page-table error.
    fn unmap_shared(&mut self, base_va: u64, len: usize) -> Result<(), LiveSpaceError>;

    /// Take the `page_count` pages from `base_va`, anonymous memory of this
    /// space every page of which is resident, out of it with what they hold:
    /// each frame is appended to `into`, for the space it moves to, once no
    /// CPU and no `retire` view can reach it here.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::NotMapped`] for a range that
    /// is not wholly this space's resident anonymous memory, and
    /// [`AnonError::OutOfMemory`] when `into` cannot be given the room, taking
    /// nothing; the page table's refusal part-way, the frames already taken
    /// zeroed and freed. `into` is as it was after every refusal.
    fn detach_anonymous(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
        into: &mut Vec<Frame>,
    ) -> Result<(), LiveSpaceError>;

    /// Place `page_count` pages in this space's code window, mapping nothing,
    /// and answer the base [`Self::map_code`] installs code another space
    /// wrote at.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::ZeroLength`] for no pages, or
    /// [`AnonError::OutOfMemory`] when the code window (absent on a
    /// degenerate configuration) has no room.
    fn reserve_code(&mut self, page_count: u64) -> Result<u64, LiveSpaceError>;

    /// Map `frames`, code another space wrote, with [`CODE_FLAGS`] over the
    /// whole of the empty placement [`Self::reserve_code`] answered `base`
    /// for.
    ///
    /// Every CPU's instruction fetch is made to see the frames before any is
    /// mapped, and the mapping reaches every CPU the space runs on before
    /// this returns. The frames are this space's from the call on: a refusal
    /// zeroes and frees them and gives the placement back.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::NotMapped`] when `base` and
    /// the frames do not name exactly such a placement,
    /// [`AnonError::OutOfMemory`] when the page tables are exhausted,
    /// [`AnonError::InstructionFence`] when not every CPU's instruction fetch
    /// could be fenced past the code, or the precise map error otherwise.
    fn map_code(&mut self, base: u64, frames: &[Frame]) -> Result<(), LiveSpaceError>;

    /// Release the `page_count` pages of the code window from `base_va`, each
    /// installed frame zeroed and freed once no CPU and no `retire` view can
    /// reach it, and answer how many were installed.
    ///
    /// # Errors
    ///
    /// [`LiveSpaceError::Anon`] — [`AnonError::NotMapped`] for a range that
    /// is not wholly placed in the code window, releasing nothing.
    fn release_code(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
    ) -> Result<u64, LiveSpaceError>;

    /// Snapshot this space's current live mappings into a `Send + Sync`
    /// [`FrozenAddressSpace`], the form the kernel-wide address-space
    /// registry holds for the user-memory copy path.
    ///
    /// A live space grows and shrinks as a task maps its own heap
    /// ([`Self::map_anonymous`] / [`Self::map_anonymous_placed`]), unmaps it
    /// ([`Self::unmap_anonymous`]), or a driver maps a granted device window
    /// or DMA buffer. The registry's snapshot must be re-frozen after every
    /// such mutation, or the copy path (`copy_in` / `copy_out`) would walk a
    /// stale snapshot that cannot see memory the task mapped after spawn —
    /// the exact defect [`FrozenAddressSpace`]'s own docs warn against. This
    /// is the object-safe seam `kernel/core` calls to produce that fresh
    /// snapshot without naming the concrete page-table backend `P`
    /// (one freeze definition).
    ///
    /// # Errors
    ///
    /// [`AllocError::OutOfMemory`] when the snapshot cannot be allocated.
    fn freeze(&self) -> Result<FrozenAddressSpace, AllocError>;

    /// Whether the page holding `va` is mapped to allow a user read, or a
    /// `write`, discarding this CPU's cached view of it when it is: a fault on
    /// such a page was taken by a CPU that cached the page's absence before
    /// another CPU mapped it, as a port may, and the access only needs to
    /// retry.
    fn settle_spurious(&mut self, va: u64, write: bool) -> bool;

    /// Translate a single `page` to the `(frame, flags)` the backend
    /// resolves it to, or `None` when it is not mapped.
    ///
    /// The O(1) counterpart to [`Self::freeze`]: after the demand-fault
    /// resolver backs one page it reads that page's mapping through here and
    /// applies it to the registry snapshot as a single-page delta, instead of
    /// re-freezing the whole space per fault (which is O(N²) over a large
    /// mapping). Object-safe so `kernel/core` needs no concrete backend `P`.
    fn translate_page(&self, page: Page) -> Option<(Frame, MapFlags)>;

    /// Restore a compressed anonymous page on a demand fault at user
    /// virtual address `va`, through the process-global [`Ramzip`]
    /// tier.
    ///
    /// The fault resolver calls this **before** the plain anonymous
    /// handler: a compressed page is *reserved* anonymous memory, so
    /// the anonymous handler would otherwise re-zero it and destroy the
    /// compressed contents. [`RamzipFaultOutcome::NoEntry`] means no
    /// compressed entry existed (fall through to the anonymous
    /// handler); [`RamzipFaultOutcome::Handled`] means the page is now
    /// resident (retry the instruction);
    /// [`RamzipFaultOutcome::Fatal`] means the entry was unrecoverable
    /// (fail closed, no plaintext, escalate the fault). `sink` records
    /// authentication/corruption failures.
    fn ramzip_fault_in(
        &mut self,
        tier: &SpinLock<Ramzip>,
        va: u64,
        sink: &dyn Sink,
    ) -> RamzipFaultOutcome;

    /// Compress up to `want` of this space's cold anonymous pages out
    /// into the global [`Ramzip`] tier under memory pressure, and
    /// report what the sweep did.
    ///
    /// The candidate set is this space's resident *placed anonymous*
    /// pages (its heap window); the owned second-chance
    /// [`ColdPageScanner`] decides which are cold (fails closed,
    /// reclaiming nothing, on a backend with no referenced bit), and
    /// the tier's own gates (pressure handoff, caps, per-task share,
    /// decompression floor, eligibility) decide which cold pages are
    /// admitted. `template` carries the task-level attributes the
    /// caller knows (pinned / sensitive / latency-critical); a pinned
    /// task yields nothing. `reclaimable_residue` is the cheaper-cache
    /// residue the tier waits to drain first. Each page leaves `retire`'s
    /// view before it is read, and returns to it if the tier refuses it.
    // Each argument is a distinct input of the sweep.
    #[allow(clippy::too_many_arguments)]
    fn ramzip_reclaim(
        &mut self,
        tier: &SpinLock<Ramzip>,
        pressure: &MemoryPressure,
        reclaimable_residue: usize,
        want: usize,
        template: PageCandidate,
        sink: &dyn Sink,
        retire: &mut dyn Retire,
    ) -> RamzipReclaimSummary;

    /// Opportunistically restore compressed entries adjacent to the
    /// page just faulted in at `va`, through the global [`Ramzip`]
    /// tier, returning how many pages were brought back.
    ///
    /// The fault resolver calls this **after** a successful
    /// [`Self::ramzip_fault_in`]: that demand fault recorded the
    /// locality evidence this consumes. The tier restores only
    /// same-space neighbours within its cluster radius that were
    /// sealed close in time to the faulted entry, and only while free
    /// memory is comfortably above the warm-up watermark with the
    /// decompression floor protected — so clustering never runs under
    /// pressure and never draws down a reserve. It is best-effort: a
    /// restore failure never propagates (the original fault already
    /// succeeded), and `sink` records any authentication/corruption
    /// loss. The caller republishes the restored pages to the registry
    /// snapshot.
    fn ramzip_cluster(
        &mut self,
        tier: &SpinLock<Ramzip>,
        pressure: &MemoryPressure,
        va: u64,
        sink: &dyn Sink,
    ) -> usize;

    /// Run one bounded, opportunistic warm-up step over this space's
    /// compressed entries, returning how many pages were restored.
    ///
    /// The move-only background-warm optimisation: it restores up to a
    /// fixed batch of entries near recent demand faults, only while
    /// free memory is comfortably above the warm-up watermark
    /// (re-checked before every page, so it stops the instant pressure
    /// returns) and never touching the decompression reserve. A tier
    /// with no fault-locality evidence restores nothing — keeping cold
    /// pages compressed is the design. There is no timer here: the
    /// resolver drives one step when a compressed page is faulted back
    /// and memory is comfortable, so the cost is charged to the
    /// resuming task rather than a background daemon. The caller
    /// republishes the restored pages.
    fn ramzip_warm(
        &mut self,
        tier: &SpinLock<Ramzip>,
        pressure: &MemoryPressure,
        sink: &dyn Sink,
    ) -> usize;
}

/// The generic concrete live address space retained per task.
///
/// Generic over the port's [`PageTable`] backend `P` and the kernel direct
/// map `M`, so it is constructed by the architecture spawn producer (which
/// names both) and stored behind [`LiveUserSpace`] by `kernel/core` (which
/// names neither). It owns:
///
/// * `space` — the live arch [`AddressSpace<P>`] (its page-table frames come
///   from the backend's own [`PageTableFrames`](tairix_arch_api::frames::PageTableFrames)
///   source, wired at spawn), and `cpus`, the CPUs it is active on;
/// * `physmap` — the kernel direct map used to zero anonymous frames on map
///   and on free;
/// * `frames` — the kernel [`FrameAllocator`] anonymous pages are drawn from
///   and returned to;
/// * `mmio` — the per-task guarded device-window virtual-address allocator;
/// * `anon` — the per-task placement allocator that chooses the base for a
///   non-`FIXED` anonymous mapping out of this task's heap window;
/// * `dma` — the per-task guarded DMA-buffer allocator that carves a
///   coherent buffer out of this task's DMA window,
///   and `dma_custodian`, the custody its buffers are surrendered to at
///   teardown (the first carve's, which every later carve must name);
/// * `shared` — the per-task guarded allocator that maps a kernel-owned
///   cross-process shared-memory region (cacheable RAM) into this task's
///   shared-memory window. It reuses the [`MmioWindowMap`] guarded-window
///   mechanism (one slot/guard definition) but maps cacheable, not
///   device-ordered, and owns no frames (the region's frames belong to the
///   shared-region registry);
/// * `file` — the per-task placement allocator for demand-paged file
///   mappings, over its own window above the heap window: reservations are
///   pure address space, backed one page at a time by the fault path
///   ([`crate::filemap`]). `None` on a degenerate configuration whose user
///   address space has no room above the heap window (file mapping then
///   fails closed as a deterministic OOM).
/// * `space_id` — the space's [`TlbContext`] id, which the process-global
///   [`Ramzip`] tier keys this space's compressed entries on;
/// * `scanner` — this space's own second-chance [`ColdPageScanner`], so
///   the reclaim clock hand rotates independently per space.
pub struct LiveSpace<P: PageTable, M: PhysMap> {
    space: AddressSpace<P>,
    /// `space`'s standing with the TLBs: the context its attached reach
    /// targets.
    context: Arc<TlbContext>,
    physmap: M,
    frames: &'static FrameAllocator,
    mmio: MmioWindowMap,
    anon: AnonWindowMap,
    dma: DmaWindowMap,
    dma_custodian: Option<DmaCustodian>,
    shared: MmioWindowMap,
    file: Option<AnonWindowMap>,
    /// Where code installed from another space is placed; none on a
    /// configuration with no address space for it.
    code: Option<AnonWindowMap>,
    space_id: u64,
    scanner: ColdPageScanner,
    /// Frames this space has committed for demand-paged anonymous memory
    /// (`mem_map` reservations and per-page stack growth) but which have
    /// not yet faulted in. Charged against the global no-overcommit budget
    /// ([`FrameAllocator::commit`]) at reservation, decremented as a page
    /// becomes resident or its reservation is released, and released in full
    /// on teardown ([`Drop`]) so a task that dies with untouched
    /// reservations returns exactly its outstanding commitment to the pool.
    committed_unbacked: u64,
    /// Bases of anonymous reservations whose physical headroom is taken one
    /// growth step at a time ([`LiveUserSpace::reserve_anonymous_growable`] —
    /// a thread stack) rather than in full at reservation time (`mem_map`).
    ///
    /// It is what a *release* consults: an up-front-committed region credits
    /// back the headroom of every page that never faulted in, whereas such a
    /// page of a growable region was never committed, so crediting it would
    /// hand the pool budget it never received and let the machine overcommit.
    growable: OrderedSet<u64>,
    /// Byte extents of the caller-placed (`FIXED`) anonymous reservations:
    /// outside the heap window, the only memory an anonymous release may
    /// tear down, so a release by extent never reaches a frame another kind
    /// of mapping owns.
    fixed: RangeSet<u64>,
}

/// Where the anonymous range a release names is held.
enum Held {
    /// Placed in the heap window.
    Placed,
    /// Within caller-placed reservations: the range's extent.
    Fixed(Range<u64>),
}

impl<P: PageTable, M: PhysMap> LiveSpace<P, M> {
    /// Check that the `page_count` pages from `base_va` are anonymous memory
    /// of this space, making the room their release's bookkeeping needs so
    /// that nothing after a page has gone can be refused.
    ///
    /// Pages rather than a whole placement, because the anonymous ABI
    /// releases what the caller names: a program that grew a region over
    /// several calls hands back the part of it that came free. A range in the
    /// file or code window is the wrong call, and outside the heap window only
    /// caller-placed reservations count, so nothing another kind of mapping
    /// owns is reached.
    fn prepare_anonymous(&mut self, base_va: u64, page_count: u64) -> Result<Held, LiveSpaceError> {
        if self.file.as_ref().is_some_and(|file| file.owns(base_va))
            || self.code.as_ref().is_some_and(|code| code.owns(base_va))
        {
            return Err(LiveSpaceError::Anon(AnonError::NotMapped));
        }
        let extent = fixed_extent(base_va, page_count)?;
        if self.anon.owns(base_va) {
            self.anon.prepare_release_pages(base_va, page_count)?;
            return Ok(Held::Placed);
        }
        if self.fixed.first_gap(extent.clone()).is_some() {
            return Err(LiveSpaceError::Anon(AnonError::NotMapped));
        }
        self.fixed
            .try_reserve(1)
            .map_err(|_| LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        Ok(Held::Fixed(extent))
    }

    /// Forget the range [`Self::prepare_anonymous`] admitted, now that its
    /// pages are gone, with `tier`'s compressed copies of them: one left
    /// behind would be restored into whatever is mapped there next, and its
    /// charge would stand until exit.
    fn forget_anonymous(
        &mut self,
        held: Held,
        base_va: u64,
        page_count: u64,
        tier: Option<&SpinLock<Ramzip>>,
    ) {
        match held {
            Held::Placed => {
                let released = self.anon.release_pages(base_va, page_count);
                debug_assert!(released.is_ok(), "a prepared release was refused");
            }
            Held::Fixed(extent) => {
                let removed = self.fixed.remove(extent);
                debug_assert!(removed.is_ok(), "a reserved split was refused");
            }
        }
        if let Some(tier) = tier {
            tier.lock()
                .purge_range(self.space_id, base_va / PAGE_SIZE as u64, page_count);
        }
    }

    /// [`LiveUserSpace::unmap_anonymous`] against `tier`.
    fn release_anonymous(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
        tier: Option<&SpinLock<Ramzip>>,
    ) -> Result<(), LiveSpaceError> {
        let held = self.prepare_anonymous(base_va, page_count)?;
        let frames = self.frames;
        // Count the frames actually reclaimed: a demand-paged region is
        // sparsely resident, so the resident count tells us how many of its
        // pages ever faulted in. The remainder are still committed-unbacked
        // reservations whose physical headroom must be returned to the pool.
        let mut resident = 0u64;
        unmap_anonymous(
            &mut self.space,
            &self.physmap,
            base_va,
            page_count,
            retire,
            &mut |frame| {
                resident = resident.saturating_add(1);
                let _ = frames.free(frame);
            },
        )?;
        // Release the reservation for every page that never became resident
        // (an eager, fully-resident region leaves nothing to release). The
        // resident pages already left the committed tally when they faulted
        // in; their frames were just freed above.
        //
        // A growable region's unbacked pages were never committed at all, so
        // crediting them would hand the pool budget it never received — the
        // machine would then believe it has more headroom than it does and
        // overcommit. Whatever such a region *did* commit was consumed by the
        // growth step that mapped it; anything an interrupted step left
        // outstanding is returned when the space is dropped.
        let unbacked = if self.growable.remove(&base_va) {
            0
        } else {
            page_count.saturating_sub(resident)
        };
        if unbacked > 0 {
            self.frames.uncommit(unbacked);
            self.committed_unbacked = self.committed_unbacked.saturating_sub(unbacked);
        }
        self.forget_anonymous(held, base_va, page_count, tier);
        Ok(())
    }

    /// Whether `extent`, the `page_count` pages from `base_va`, touches a
    /// window, a caller-placed reservation, or a mapped page of this space.
    fn overlaps_held(&self, base_va: u64, page_count: u64, extent: &Range<u64>) -> bool {
        let windows = [
            Some(self.mmio.span()),
            Some(self.anon.span()),
            Some(self.dma.span()),
            Some(self.shared.span()),
            self.file.as_ref().map(AnonWindowMap::span),
            self.code.as_ref().map(AnonWindowMap::span),
        ];
        if windows
            .into_iter()
            .flatten()
            .any(|window| window.start < extent.end && extent.start < window.end)
        {
            return true;
        }
        if self.fixed.first_overlap(extent.clone()).is_some() {
            return true;
        }
        let (Ok(first), Ok(last)) = (page_at(base_va, 0), page_at(base_va, page_count - 1)) else {
            return true;
        };
        self.space.next_live(first, last).is_some()
    }

    /// Retain `space` as a live, mutable user address space.
    ///
    /// `tlb` is how an entry this space clears is discarded on the other CPUs
    /// the space is active on. `physmap` is the kernel direct map (used to
    /// zero anonymous frames),
    /// `frames` the allocator anonymous pages come from,
    /// `[mmio_window_base, mmio_window_base + mmio_window_pages * PAGE_SIZE)`
    /// the virtual range device windows are mapped into (guard-bracketed by
    /// [`MmioWindowMap`]), and
    /// `[anon_window_base, anon_window_base + anon_window_pages * PAGE_SIZE)`
    /// the range non-`FIXED` anonymous mappings are placed into
    /// ([`AnonWindowMap`]), and
    /// `[dma_window_base, dma_window_base + dma_window_pages * PAGE_SIZE)` the
    /// range guarded DMA buffers are carved into ([`DmaWindowMap`]). All
    /// three windows must lie in the task's own free user virtual space,
    /// clear of each other, its image, and its stack.
    ///
    /// # Errors
    ///
    /// [`MmioError::InvalidMapConfig`] if any window is misconfigured
    /// (zero pages, a base that is not page-aligned, or a size that
    /// overflows the address space). The space is torn down first, so the
    /// frames and page tables it holds go back rather than with it.
    // Each argument is a *distinct* piece of the retained space the
    // architecture spawn producer threads explicitly — the live arch space,
    // the direct map, the frame allocator, and the three guarded windows'
    // `(base, pages)` pairs. Bundling the windows behind a one-use config
    // wrapper purely to satisfy the arg-count lint would be the wrapper
    // type the charter forbids; the explicit list is the clearer shape,
    // mirroring `KernelSyscallHandlers::new`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut space: AddressSpace<P>,
        tlb: SpaceTlb,
        physmap: M,
        frames: &'static FrameAllocator,
        mmio_window_base: VirtAddr,
        mmio_window_pages: usize,
        anon_window_base: VirtAddr,
        anon_window_pages: usize,
        dma_window_base: VirtAddr,
        dma_window_pages: usize,
        shared_window_base: VirtAddr,
        shared_window_pages: usize,
        file_window_base: VirtAddr,
        file_window_pages: usize,
        code_window_base: VirtAddr,
        code_window_pages: usize,
    ) -> Result<Self, MmioError> {
        // Every window is checked before the space is retained, each refusal
        // the one configuration fault the spawn producer handles. The
        // shared-memory window reuses the guarded-window mechanism in a range
        // of its own, so it never collides with the device window. The
        // file-mapping window alone may be empty: a degenerate configuration
        // with no address space left above the heap window still spawns, and
        // file mapping fails closed at reservation time.
        let windows = (
            MmioWindowMap::new(mmio_window_base, mmio_window_pages),
            AnonWindowMap::new(anon_window_base, anon_window_pages),
            DmaWindowMap::new(dma_window_base, dma_window_pages),
            MmioWindowMap::new(shared_window_base, shared_window_pages),
            if file_window_pages == 0 {
                Ok(None)
            } else {
                AnonWindowMap::new(file_window_base, file_window_pages).map(Some)
            },
            if code_window_pages == 0 {
                Ok(None)
            } else {
                AnonWindowMap::new(code_window_base, code_window_pages).map(Some)
            },
        );
        let (Ok(mmio), Ok(anon), Ok(dma), Ok(shared), Ok(file), Ok(code)) = windows else {
            // SAFETY: the space was handed over by value and nothing retains
            // it, so nothing can make it active again or reach into its
            // tables; every entry is flushed here before its frame goes, and a
            // CPU still holding its root (PID 1's seam builds into the active
            // space) is re-parked by the port's reclaim before the tables go.
            unsafe { release_space(&mut space, &physmap, frames, false, |_| false) };
            return Err(MmioError::InvalidMapConfig);
        };
        let context = Arc::clone(tlb.context());
        space.attach_tlb(tlb);
        Ok(Self {
            space_id: context.id(),
            space,
            context,
            physmap,
            frames,
            mmio,
            anon,
            dma,
            dma_custodian: None,
            shared,
            file,
            code,
            scanner: ColdPageScanner::new(),
            committed_unbacked: 0,
            growable: OrderedSet::new(),
            fixed: RangeSet::new(),
        })
    }

    /// This space's stable id in the global [`Ramzip`] tier
    /// (diagnostic / test observer).
    #[must_use]
    pub fn space_id(&self) -> u64 {
        self.space_id
    }

    /// Borrow the underlying live address space (e.g. to freeze a fresh
    /// snapshot for the read-only copy path after a mutation).
    #[must_use]
    pub fn space(&self) -> &AddressSpace<P> {
        &self.space
    }

    /// Whether `frames` fit exactly the empty code-window placement at `base`.
    fn is_empty_placement(&self, base: u64, frames: &[Frame]) -> bool {
        let Ok(pages) = u64::try_from(frames.len()) else {
            return false;
        };
        let placed = self
            .code
            .as_ref()
            .is_some_and(|code| code.validate(base, pages).is_ok());
        let (Ok(first), Ok(last)) = (page_at(base, 0), page_at(base, pages.saturating_sub(1)))
        else {
            return false;
        };
        placed && self.space.next_live(first, last).is_none()
    }

    /// Clean `frames` from the data caches and fence every CPU's instruction
    /// fetch past them.
    fn fence_code(&self, frames: &[Frame]) -> Result<(), LiveSpaceError> {
        for &frame in frames {
            self.physmap
                .sync_instruction_cache(frame.start(), PAGE_SIZE);
        }
        if self.physmap.fence_instruction_fetch() {
            Ok(())
        } else {
            Err(LiveSpaceError::Anon(AnonError::InstructionFence))
        }
    }

    /// Undo a [`LiveUserSpace::map_code`] of `frames` at `base` that mapped
    /// only the first `mapped`: those leave as an unmap takes them, reaching
    /// every CPU that may have seen them, the rest are discarded, and the
    /// placement goes back to the window.
    fn unwind_code(&mut self, base: u64, mapped: u64, frames: &[Frame]) {
        if mapped > 0 {
            let pool = self.frames;
            let _ = unmap_anonymous(
                &mut self.space,
                &self.physmap,
                base,
                mapped,
                &mut Unpublished,
                &mut |frame| {
                    let _ = pool.free(frame);
                },
            );
        }
        let rest = usize::try_from(mapped)
            .ok()
            .and_then(|mapped| frames.get(mapped..))
            .unwrap_or_default();
        self.discard(rest);
        if let Some(code) = self.code.as_mut() {
            let released = code.release(base, frames.len() as u64);
            debug_assert!(released.is_ok(), "a placement was refused its release");
        }
    }

    /// Zero and free `frames`, none of which any CPU can reach: one the
    /// direct map cannot reach is kept rather than released unscrubbed.
    fn discard(&self, frames: &[Frame]) {
        for &frame in frames {
            if zero_frame(&self.physmap, frame).is_ok() {
                let _ = self.frames.free(frame);
            }
        }
    }

    /// Settle a fault's map of `page_count` pages from `base_va`: when another
    /// CPU mapped them first, this CPU may still cache the absence the fault
    /// walked, and would take the same fault again.
    fn settle_fault(
        &mut self,
        base_va: u64,
        page_count: u64,
        mapped: Result<(), AnonError>,
    ) -> Result<(), AnonError> {
        if mapped == Err(AnonError::Map(PageTableError::AlreadyMapped)) {
            for index in 0..page_count {
                if let Ok(page) = page_at(base_va, index) {
                    self.space.discard_stale(page);
                }
            }
        }
        mapped
    }
}

impl<P, M> LiveUserSpace for LiveSpace<P, M>
where
    P: PageTable + Send,
    M: PhysMap + Send,
{
    fn tlb_context(&self) -> Arc<TlbContext> {
        Arc::clone(&self.context)
    }

    fn map_anonymous(&mut self, base_va: u64, page_count: u64) -> Result<u64, LiveSpaceError> {
        // Copy the `'static` allocator handle out so the alloc/free closures
        // do not borrow `self` while `map_anonymous` borrows `self.space`
        // and `self.physmap`.
        let frames = self.frames;
        // This is the demand-fault path: every page here was reserved ahead
        // of time (a `mem_map` reservation, or a stack-growth commit) against
        // the no-overcommit budget, so it draws through the *committed* path
        // — guaranteed a frame, and not re-gated by the kernel reserve the
        // reservation already cleared. An unwound page returns to its
        // reservation (`free_committed`) so the commitment survives a
        // page-table-build failure without drifting the counter.
        let mapped = map_anonymous(
            &mut self.space,
            &self.physmap,
            base_va,
            page_count,
            || frames.alloc_user_committed().ok(),
            |frame| {
                let _ = frames.free_committed(frame);
            },
        );
        self.settle_fault(base_va, page_count, mapped)?;
        // The reserved pages are now resident: convert them from
        // committed-unbacked headroom to real residency (the per-page
        // `alloc_user_committed` already lowered the global counter; keep the
        // per-space tally in step so teardown releases exactly the pages that
        // never faulted in).
        self.committed_unbacked = self.committed_unbacked.saturating_sub(page_count);
        Ok(base_va)
    }

    fn map_anonymous_placed(&mut self, page_count: u64) -> Result<u64, LiveSpaceError> {
        // Choose a base out of this task's heap window first; no page table is
        // touched until the placement succeeds.
        let base_va = self.anon.allocate(page_count)?;
        let frames = self.frames;
        match map_anonymous(
            &mut self.space,
            &self.physmap,
            base_va,
            page_count,
            // Reserve-gated user commit (see `map_anonymous`).
            || frames.alloc_user(MemoryClass::UserAnon).ok(),
            |frame| {
                let _ = frames.free(frame);
            },
        ) {
            Ok(()) => Ok(base_va),
            Err(err) => {
                // The mapping failed (frame exhaustion, …): give the reserved
                // range back so a failed call consumes no address space. The range was just minted, so the
                // release matches and cannot fail; ignore its result rather
                // than panic on a path that already failed closed.
                let _ = self.anon.release(base_va, page_count);
                Err(err.into())
            }
        }
    }

    fn unmap_anonymous(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
    ) -> Result<(), LiveSpaceError> {
        self.release_anonymous(base_va, page_count, retire, crate::ramzip::global())
    }

    fn reserve_anonymous(&mut self, page_count: u64) -> Result<u64, LiveSpaceError> {
        // No-overcommit admission first: reserve physical headroom for every
        // page before handing back any address space, so a `mem_map` that
        // could not later be backed fails *here* as a deterministic
        // `OutOfMemory` rather than as a fault-time kill on first touch. Only
        // then place the region out of this task's heap window; no page-table
        // entry and no frame move until a fault lands, and the pages fault in
        // one at a time through `map_anonymous`.
        self.frames
            .commit(page_count)
            .map_err(|_| LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        match self.anon.allocate(page_count) {
            Ok(base_va) => {
                self.committed_unbacked = self.committed_unbacked.saturating_add(page_count);
                Ok(base_va)
            }
            Err(err) => {
                // The address-space placement failed: return the commitment
                // we just made so a failed reservation consumes no headroom.
                self.frames.uncommit(page_count);
                Err(err.into())
            }
        }
    }

    fn reserve_anonymous_growable(&mut self, page_count: u64) -> Result<u64, LiveSpaceError> {
        // Address space only: the caller commits as the region grows, so
        // nothing is charged against the no-overcommit budget here.
        let base_va = self.anon.allocate(page_count)?;
        if self.growable.try_insert(base_va).is_err() {
            // Just placed, so the release matches.
            let _ = self.anon.release(base_va, page_count);
            return Err(LiveSpaceError::Anon(AnonError::OutOfMemory));
        }
        Ok(base_va)
    }

    fn reserve_anonymous_at(
        &mut self,
        base_va: u64,
        page_count: u64,
    ) -> Result<u64, LiveSpaceError> {
        // A caller-placed (`FIXED`) reservation claims address space no
        // mapping, window or other reservation holds, so the release that
        // tears it down by extent can only reach pages the anonymous path
        // backed there. Validate first, before charging the commitment.
        let extent = fixed_extent(base_va, page_count)?;
        if self.overlaps_held(base_va, page_count, &extent) {
            return Err(LiveSpaceError::Anon(AnonError::Overlap));
        }
        self.fixed
            .try_reserve(1)
            .map_err(|_| LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        // No-overcommit admission (see `reserve_anonymous`): reserve physical
        // headroom for the whole extent up front, so a first touch is
        // guaranteed a frame.
        self.frames
            .commit(page_count)
            .map_err(|_| LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        if self.fixed.insert(extent).is_err() {
            self.frames.uncommit(page_count);
            return Err(LiveSpaceError::Anon(AnonError::OutOfMemory));
        }
        self.committed_unbacked = self.committed_unbacked.saturating_add(page_count);
        Ok(base_va)
    }

    fn commit_anonymous(&mut self, page_count: u64) -> Result<(), LiveSpaceError> {
        // Commitment only — the address space (the stack span's growth room)
        // is already reserved. Charge the no-overcommit budget and join the
        // pages to this space's outstanding tally; `map_anonymous` consumes
        // them as each faults in, and teardown releases any that never did.
        self.frames
            .commit(page_count)
            .map_err(|_| LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        self.committed_unbacked = self.committed_unbacked.saturating_add(page_count);
        Ok(())
    }

    fn reserve_file_region(&mut self, page_count: u64) -> Result<u64, LiveSpaceError> {
        // Pure placement: no page table entry and no frame until a fault
        // lands in the region. An absent window (degenerate configuration)
        // is the same deterministic refusal as an exhausted one.
        let file = self
            .file
            .as_mut()
            .ok_or(LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        Ok(file.allocate(page_count)?)
    }

    fn map_file_page_at(&mut self, va: u64, contents: &[u8]) -> Result<(), LiveSpaceError> {
        // Only an address inside a live reserved file region is ever
        // backed: the fault path must not be able to materialise memory
        // the task never mapped (fail closed before any frame is drawn).
        if !self.file.as_ref().is_some_and(|file| file.covers(va)) {
            return Err(LiveSpaceError::Anon(AnonError::NotMapped));
        }
        let frames = self.frames;
        let mapped = map_file_page(
            &mut self.space,
            &self.physmap,
            va,
            contents,
            || frames.alloc(MemoryClass::UserFile).ok(),
            |frame| {
                // The frame never became user-visible; returning it to the
                // allocator cannot fail meaningfully (best-effort, never a
                // panic).
                let _ = frames.free(frame);
            },
        );
        self.settle_fault(va, 1, mapped)?;
        Ok(())
    }

    fn release_file_region(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
    ) -> Result<u64, LiveSpaceError> {
        // The (base, extent) must name a live reservation exactly before
        // any teardown, so a bad pair fails closed without touching a
        // neighbour's pages; residency inside the region is fault history
        // and legitimately sparse.
        let file = self
            .file
            .as_mut()
            .ok_or(LiveSpaceError::Anon(AnonError::NotMapped))?;
        file.validate(base_va, page_count)?;
        let frames = self.frames;
        let resident = unmap_file_region(
            &mut self.space,
            &self.physmap,
            base_va,
            page_count,
            retire,
            &mut |frame| {
                let _ = frames.free(frame);
            },
        )?;
        // Validated above, so the release matches; ignore its result.
        let _ = file.release(base_va, page_count);
        Ok(resident)
    }

    fn map_device_window(&mut self, phys_base: u64, len: usize) -> Result<u64, LiveSpaceError> {
        let region = self.mmio.map_into(&mut self.space, phys_base, len)?;
        Ok(region.virt().as_u64())
    }

    fn map_framebuffer_window(
        &mut self,
        phys_base: u64,
        len: usize,
    ) -> Result<u64, LiveSpaceError> {
        let region = self
            .mmio
            .map_framebuffer_into(&mut self.space, phys_base, len)?;
        Ok(region.virt().as_u64())
    }

    fn map_writeback_framebuffer_window(
        &mut self,
        phys_base: u64,
        len: usize,
    ) -> Result<u64, LiveSpaceError> {
        let region = self
            .mmio
            .map_cacheable_into(&mut self.space, phys_base, len)?;
        Ok(region.virt().as_u64())
    }

    fn retain_device_windows(
        &mut self,
        keep: &mut dyn FnMut(u64, u64) -> bool,
        unmapped: &mut dyn FnMut(u64, u64),
    ) -> Result<(), LiveSpaceError> {
        let page_mask = PAGE_SIZE as u64 - 1;
        self.mmio
            .retain(&mut self.space, keep, |base, pages| {
                unmapped(base.as_u64() & !page_mask, pages as u64);
            })
            .map_err(LiveSpaceError::from)
    }

    fn alloc_dma(
        &mut self,
        len: usize,
        addr_limit: u64,
        custodian: DmaCustodian,
    ) -> Result<DmaMapping, LiveSpaceError> {
        if self
            .dma_custodian
            .is_some_and(|bound| !bound.same_as(&custodian))
        {
            return Err(DmaError::CustodianMismatch.into());
        }
        custodian.custody().reserve(custodian.node)?;
        self.dma_custodian = Some(custodian);
        let buf = self
            .dma
            .alloc_into(
                &mut self.space,
                self.frames,
                &self.physmap,
                len,
                CarveDevice {
                    addr_limit,
                    translator: custodian.translator(),
                    coherence: custodian.coherence(),
                },
            )
            .inspect_err(|_| custodian.custody().unreserve(custodian.node))?;
        Ok(DmaMapping {
            cpu_va: buf.virt().as_u64(),
            device_addr: buf.device_addr(),
            len: buf.len(),
        })
    }

    fn free_dma(&mut self, cpu_va: u64, retire: &mut dyn Retire) -> Result<usize, LiveSpaceError> {
        let virt = VirtAddr::new(cpu_va);
        let was_live = self.dma.holds(virt);
        let released = self
            .dma
            .free_at(&mut self.space, self.frames, &self.physmap, virt, retire);
        // A reservation goes back exactly when its record leaves: a block a
        // failed release kept is surrendered at teardown, which spends it then.
        if was_live && !self.dma.holds(virt) {
            if let Some(custodian) = self.dma_custodian {
                custodian.custody().unreserve(custodian.node);
            }
        }
        Ok(released?)
    }

    fn map_shared_chunks(
        &mut self,
        chunks: &[(u64, u64)],
        memory: SharedMemory,
    ) -> Result<u64, LiveSpaceError> {
        let region = self
            .shared
            .map_chunks_into(&mut self.space, chunks, memory)?;
        Ok(region.virt().as_u64())
    }

    fn shared_room(&self, pages: u64) -> bool {
        usize::try_from(pages).is_ok_and(|pages| self.shared.has_room(pages))
    }

    fn unmap_shared(&mut self, base_va: u64, _len: usize) -> Result<(), LiveSpaceError> {
        // Release exactly the pages the shared-window allocator recorded for
        // this base; `len` is advisory (the record is authoritative). The
        // frames are not freed here - they belong to the registry, which
        // frees them when the owner and every grantee have released the
        // region.
        self.shared
            .unmap_at(&mut self.space, VirtAddr::new(base_va))
            .map_err(LiveSpaceError::from)
    }

    fn detach_anonymous(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
        into: &mut Vec<Frame>,
    ) -> Result<(), LiveSpaceError> {
        let room = usize::try_from(page_count).map_err(|_| AnonError::Overflow)?;
        into.try_reserve(room)
            .map_err(|_| LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        let held = self.prepare_anonymous(base_va, page_count)?;
        let start = into.len();
        let detached = detach_anonymous(
            &mut self.space,
            &self.physmap,
            base_va,
            page_count,
            retire,
            &mut |frame| into.push(frame),
        );
        if let Err(err) = detached {
            self.discard(into.get(start..).unwrap_or_default());
            into.truncate(start);
            return Err(err.into());
        }
        self.forget_anonymous(held, base_va, page_count, crate::ramzip::global());
        Ok(())
    }

    fn reserve_code(&mut self, page_count: u64) -> Result<u64, LiveSpaceError> {
        let code = self
            .code
            .as_mut()
            .ok_or(LiveSpaceError::Anon(AnonError::OutOfMemory))?;
        Ok(code.allocate(page_count)?)
    }

    fn map_code(&mut self, base: u64, frames: &[Frame]) -> Result<(), LiveSpaceError> {
        if !self.is_empty_placement(base, frames) {
            self.discard(frames);
            return Err(LiveSpaceError::Anon(AnonError::NotMapped));
        }
        let prepared = self
            .space
            .reserve_records(frames.len())
            .map_err(|err| LiveSpaceError::Anon(map_errno(err)))
            .and_then(|()| self.fence_code(frames));
        if let Err(err) = prepared {
            self.unwind_code(base, 0, frames);
            return Err(err);
        }
        for (index, &frame) in (0u64..).zip(frames) {
            let mapped = page_at(base, index)
                .and_then(|page| self.space.map(page, frame, CODE_FLAGS).map_err(map_errno));
            if let Err(err) = mapped {
                self.unwind_code(base, index, frames);
                return Err(LiveSpaceError::Anon(err));
            }
        }
        self.space.publish_remote(base, frames.len() as u64);
        Ok(())
    }

    fn release_code(
        &mut self,
        base_va: u64,
        page_count: u64,
        retire: &mut dyn Retire,
    ) -> Result<u64, LiveSpaceError> {
        let code = self
            .code
            .as_mut()
            .filter(|code| code.owns(base_va))
            .ok_or(LiveSpaceError::Anon(AnonError::NotMapped))?;
        code.prepare_release_pages(base_va, page_count)?;
        let pool = self.frames;
        let mut installed = 0u64;
        unmap_anonymous(
            &mut self.space,
            &self.physmap,
            base_va,
            page_count,
            retire,
            &mut |frame| {
                installed += 1;
                let _ = pool.free(frame);
            },
        )?;
        let released = code.release_pages(base_va, page_count);
        debug_assert!(released.is_ok(), "a prepared release was refused");
        Ok(installed)
    }

    fn freeze(&self) -> Result<FrozenAddressSpace, AllocError> {
        // One freeze definition lives on `AddressSpace`; this only erases the
        // backend `P` for the registry.
        self.space.freeze()
    }

    fn settle_spurious(&mut self, va: u64, write: bool) -> bool {
        let Ok(page) = Page::from_addr(VirtAddr::new(va & !(PAGE_SIZE as u64 - 1))) else {
            return false;
        };
        let access = if write {
            MapFlags::WRITE
        } else {
            MapFlags::READ
        };
        let allowed = self
            .space
            .translate(page)
            .is_some_and(|(_, flags)| flags.contains(access | MapFlags::USER));
        if allowed {
            self.space.discard_stale(page);
        }
        allowed
    }

    fn translate_page(&self, page: Page) -> Option<(Frame, MapFlags)> {
        self.space.translate(page)
    }

    fn ramzip_fault_in(
        &mut self,
        tier: &SpinLock<Ramzip>,
        va: u64,
        sink: &dyn Sink,
    ) -> RamzipFaultOutcome {
        // Round down to the faulting page; a misaligned or out-of-range
        // address has no compressed entry (fall through).
        let Ok(page) = Page::from_addr(VirtAddr::new(va & !((PAGE_SIZE as u64) - 1))) else {
            return RamzipFaultOutcome::NoEntry;
        };
        let space_id = self.space_id;
        let mut tier = tier.lock();
        let mut ctx = VmContext {
            space_id,
            space: &mut self.space,
            physmap: &self.physmap,
            frames: self.frames,
            sink,
        };
        match tier.fault_in(&mut ctx, page) {
            Ok(()) => RamzipFaultOutcome::Handled,
            // No entry, or the page is already resident (a spurious or
            // raced fault): the anonymous handler takes it from here.
            Err(FaultError::NoEntry | FaultError::AlreadyMapped) => RamzipFaultOutcome::NoEntry,
            Err(other) => RamzipFaultOutcome::Fatal(other),
        }
    }

    fn ramzip_reclaim(
        &mut self,
        tier: &SpinLock<Ramzip>,
        pressure: &MemoryPressure,
        reclaimable_residue: usize,
        want: usize,
        template: PageCandidate,
        sink: &dyn Sink,
        retire: &mut dyn Retire,
    ) -> RamzipReclaimSummary {
        let Some(window) = page_span(&self.anon.span()).filter(|_| want > 0) else {
            return RamzipReclaimSummary::default();
        };
        // Only the heap window's pages are offered, and only those inside a
        // live placement: image, stack, file, device, DMA and shared pages
        // never are.
        let mut sweep = match self
            .scanner
            .sweep(&self.space, window, ramzip_scan_budget(want))
        {
            Ok(sweep) => sweep,
            Err(ColdScanError::Unsupported) => {
                return RamzipReclaimSummary {
                    access_tracking_unsupported: true,
                    ..RamzipReclaimSummary::default()
                };
            }
        };
        let anon = &self.anon;
        let space_id = self.space_id;
        let mut summary = RamzipReclaimSummary::default();
        loop {
            let asked = summary.compressed + summary.refused;
            let mut cold = ArrayVec::<Page, COMPRESS_BATCH>::new();
            sweep.collect(
                &mut self.space,
                |page| anon.covers(page.start().as_u64()),
                want.saturating_sub(asked),
                &mut cold,
            );
            if cold.is_empty() {
                break;
            }
            let mut ctx = VmContext {
                space_id,
                space: &mut self.space,
                physmap: &self.physmap,
                frames: self.frames,
                sink,
            };
            // The owning task is the space itself (one task per space), so
            // its tier task id is the space id.
            tier.lock().compress_out_batch(
                pressure,
                reclaimable_residue,
                (&mut ctx, &cold),
                space_id,
                &template,
                retire,
                &mut |_, outcome| match outcome {
                    Ok(()) => summary.compressed += 1,
                    Err(_) => summary.refused += 1,
                },
            );
        }
        summary.scanned = sweep.visited();
        summary
    }

    fn ramzip_cluster(
        &mut self,
        tier: &SpinLock<Ramzip>,
        pressure: &MemoryPressure,
        va: u64,
        sink: &dyn Sink,
    ) -> usize {
        // A misaligned or out-of-range address names no faulted page to
        // cluster around: restore nothing (fail closed).
        let Ok(page) = Page::from_addr(VirtAddr::new(va & !((PAGE_SIZE as u64) - 1))) else {
            return 0;
        };
        let space_id = self.space_id;
        let mut tier = tier.lock();
        let mut ctx = VmContext {
            space_id,
            space: &mut self.space,
            physmap: &self.physmap,
            frames: self.frames,
            sink,
        };
        tier.cluster_after_fault(pressure, &mut ctx, page)
    }

    fn ramzip_warm(
        &mut self,
        tier: &SpinLock<Ramzip>,
        pressure: &MemoryPressure,
        sink: &dyn Sink,
    ) -> usize {
        let space_id = self.space_id;
        let mut tier = tier.lock();
        let mut ctx = VmContext {
            space_id,
            space: &mut self.space,
            physmap: &self.physmap,
            frames: self.frames,
            sink,
        };
        match tier.warm_step(pressure, &mut ctx) {
            WarmOutcome::Restored(restored) => restored,
            // No candidate had locality evidence, or a gate closed
            // mid-step: nothing became resident either way.
            WarmOutcome::NothingToDo | WarmOutcome::Stopped => 0,
        }
    }
}

/// The pages of the byte extent `span`, or `None` when it holds none.
fn page_span(span: &Range<u64>) -> Option<RangeInclusive<Page>> {
    let last = span.end.checked_sub(PAGE_SIZE as u64)?;
    if last < span.start {
        return None;
    }
    let first = Page::from_addr(VirtAddr::new(span.start)).ok()?;
    let last = Page::from_addr(VirtAddr::new(last)).ok()?;
    Some(first..=last)
}

/// The byte extent of the `page_count` pages from `base_va`, checked.
fn fixed_extent(base_va: u64, page_count: u64) -> Result<Range<u64>, LiveSpaceError> {
    if page_count == 0 {
        return Err(LiveSpaceError::Anon(AnonError::ZeroLength));
    }
    if !base_va.is_multiple_of(PAGE_SIZE as u64) {
        return Err(LiveSpaceError::Anon(AnonError::Unaligned));
    }
    page_count
        .checked_mul(PAGE_SIZE as u64)
        .and_then(|len| base_va.checked_add(len))
        .map(|end| base_va..end)
        .ok_or(LiveSpaceError::Anon(AnonError::Overflow))
}

/// Clear every page `space` records, zeroing and releasing each frame `kept`
/// does not claim, then return the space's page-table frames: the one
/// teardown walk, for a live space's drop and for a space refused before one
/// was made.
///
/// Every page left is backed by a frame drawn from `frames`; it is zeroed, so
/// no dead process's bytes are recycled readable, and a frame the direct map
/// cannot reach is leaked rather than freed unscrubbed. The walk allocates
/// nothing, so a teardown under memory pressure cannot fail for want of the
/// memory it is about to return.
///
/// # Safety
///
/// `space` never becomes active again and nothing else reaches into its
/// tables; a CPU still holding its root is re-parked by the port's reclaim
/// before the tables go. When `quiet`, no CPU may still use a translation of
/// it, since its entries are then cleared unflushed.
unsafe fn release_space<P: PageTable>(
    space: &mut AddressSpace<P>,
    physmap: &dyn PhysMap,
    frames: &FrameAllocator,
    quiet: bool,
    kept: impl Fn(VirtAddr) -> bool,
) {
    let mut release = |frame| {
        let _ = frames.free(frame);
    };
    let mut unpublished = Unpublished;
    let reach = if quiet { None } else { space.tlb().cloned() };
    let mut retiring = Retiring::new(reach, &mut unpublished, physmap, &mut release);
    loop {
        let next = if quiet {
            // SAFETY: the caller's contract: no CPU uses a translation of the
            // space again.
            unsafe { space.clear_lowest() }
        } else {
            space.unmap_lowest()
        };
        let Some((page, cleared)) = next else {
            break;
        };
        // The page was recorded live, so the unmap can only fail on a
        // backend defect; declining to touch the frame is the only safe
        // recovery (never a panic).
        let Ok(frame) = cleared else {
            continue;
        };
        if !kept(page.start()) {
            retiring.hold(page.start().as_u64(), frame);
        }
    }
    drop(retiring);
    // SAFETY: the caller's contract: the space never becomes active again and
    // nothing else reaches into its tables.
    unsafe { space.reclaim_table_frames() };
}

impl<P: PageTable, M: PhysMap> Drop for LiveSpace<P, M> {
    /// Reclaim the dead task's **entire** memory footprint — the fix for
    /// the login/logout RAM leak (`plans/APPS.md` I2): before this, exit
    /// reclaimed the kernel stack, grants, endpoints, and shared regions,
    /// but every user frame (image, stack, startup block, anonymous heap)
    /// and every page-table frame leaked.
    fn drop(&mut self) {
        // 0. Purge this space's compressed entries from the global tier
        //    (if one is installed): a global pool keyed by space id must
        //    not keep a dead space's entries — nothing would ever fault
        //    them back in, so their RAM (sealed blobs) and ledger charge
        //    would leak. Freeing them here restores the frame/ledger
        //    balance the exit-cycle leak tests assert.
        if let Some(tier) = crate::ramzip::global() {
            tier.lock().purge_space(self.space_id);
        }

        // 1. Release every live DMA buffer: to the allocator only once its
        //    translated device has provably lost it, otherwise to the
        //    space's custodian. A space with no custodian never carved.
        if let Some(custodian) = self.dma_custodian.take() {
            self.dma
                .surrender_into(&mut self.space, self.frames, &self.physmap, &custodian);
        }

        // 2. Release every remaining tracked mapping, then the page tables.
        //    A page inside the device-window or shared-memory window is only
        //    *unmapped* — its frame belongs to a device (MMIO registers) or
        //    to the shared-region registry, which zeroes and frees region
        //    frames itself once the owner and every grantee have released
        //    them. A page left in the DMA window (an unmap that failed above)
        //    is unmapped likewise: its frame is the custodian's now.
        //
        //    A space no CPU runs needs no flush at all: a CPU that ran it
        //    keeps its entries only under a tag it discards before it gives
        //    the tag to another space, and this one never runs again.
        let quiet = self.context.is_idle();
        let (mmio, shared, dma) = (&self.mmio, &self.shared, &self.dma);
        // SAFETY: the owning task has exited and the dispatcher parked every
        // CPU that ran it off its root at switch-back (the port's reclaim
        // additionally re-parks defensively if the calling CPU still holds
        // this root), and when `quiet` no CPU runs the space and none can come
        // to, the last handle a thread could run it through being gone;
        // `self` is being dropped, so no other reference into the tables is
        // live.
        unsafe {
            release_space(&mut self.space, &self.physmap, self.frames, quiet, |va| {
                mmio.contains(va) || shared.contains(va) || dma.contains(va)
            });
        }

        // 3. Return this space's outstanding no-overcommit reservations —
        //    the pages a `mem_map`/stack commit reserved physical headroom
        //    for but which never faulted in — to the global budget. Their
        //    resident siblings already left the tally as they faulted in and
        //    had their frames freed in step 2, so a task that dies holding
        //    untouched reservations leaks no committed headroom.
        if self.committed_unbacked > 0 {
            self.frames.uncommit(self.committed_unbacked);
            self.committed_unbacked = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveSpace, LiveSpaceError, LiveUserSpace};
    use crate::anon::AnonError;
    use crate::dma::{DmaCustodian, DmaError};
    use crate::error::AllocError;
    use crate::frame::{FrameAllocator, MemoryClass, PhysAddr, PAGE_SIZE};
    use crate::mmio::SharedMemory;
    use crate::phys::SimPhysMap;
    use crate::retire::{SpaceTlb, TlbContext, Unpublished};
    use crate::test_fixture::{custody, frame_backing};
    use crate::uaccess::{copy_in, copy_out};
    use crate::vmm::{AddressSpace, HostPageTable, Page, VirtAddr};
    use tairix_sync::Once;

    extern crate std;
    use std::boxed::Box;
    use std::vec;

    /// A simulated physical window the frame allocator draws from and the
    /// direct map translates — frame 16 up, 256 KiB. Anonymous frames must be
    /// reachable through this map to be zeroed.
    const SIM_BASE: u64 = 16 * PAGE_SIZE as u64;
    const SIM_BYTES: usize = 64 * PAGE_SIZE;

    const SIM_PAGES: usize = SIM_BYTES / PAGE_SIZE;

    /// A `'static` frame allocator over the simulated usable window, plus the
    /// direct map over exactly those bytes. The live space holds the
    /// production `&'static FrameAllocator` shape (the kernel allocator is a
    /// boot global), so each expansion gets a cell of its own rather than a
    /// leaked box the interpreter could not tell from a real leak.
    macro_rules! backing {
        () => {{
            frame_backing!(SIM_BASE, SIM_PAGES)
        }};
    }

    fn sim() -> SimPhysMap {
        SimPhysMap::new(PhysAddr::new(SIM_BASE), SIM_BYTES)
    }

    /// A reach for a space no dispatcher runs: the host has no other CPU.
    fn host_tlb() -> SpaceTlb {
        SpaceTlb::new(TlbContext::new(1).expect("one word allocates"), None)
    }

    /// The node and admission generation the tests' carves are made for.
    const TEST_NODE: u32 = 7;
    const TEST_GENERATION: u64 = 3;

    fn custodian(custody: &'static crate::test_fixture::RecordingCustody) -> DmaCustodian {
        DmaCustodian::untranslated(
            TEST_NODE,
            TEST_GENERATION,
            custody,
            tairix_abi::DmaCoherence::Snooped,
        )
    }

    use crate::phys::PhysMap;

    /// A `PhysMap` view over one leaked, shared [`SimPhysMap`], so a test
    /// observes the same simulated physical memory the live space writes
    /// and scrubs through (each `sim()` owns disjoint storage).
    struct SharedSim(&'static SimPhysMap);
    impl PhysMap for SharedSim {
        fn translate(
            &self,
            phys: crate::frame::PhysAddr,
            len: usize,
        ) -> Option<core::ptr::NonNull<u8>> {
            self.0.translate(phys, len)
        }

        fn clean_invalidate(&self, phys: crate::frame::PhysAddr, len: usize) {
            self.0.clean_invalidate(phys, len);
        }

        fn sync_instruction_cache(&self, phys: crate::frame::PhysAddr, len: usize) {
            self.0.sync_instruction_cache(phys, len);
        }

        fn fence_instruction_fetch(&self) -> bool {
            self.0.fence_instruction_fetch()
        }
    }

    /// The user virtual window device mappings land in — far above the
    /// anonymous regions the tests map, on freshly walked tables.
    const MMIO_WINDOW_BASE: u64 = 0x8000_0000;
    const MMIO_WINDOW_PAGES: usize = 64;

    /// The user virtual window non-`FIXED` anonymous mappings are placed into
    /// — distinct from both the `FIXED` bases the tests choose (low, 0x4000)
    /// and the device window above.
    const ANON_WINDOW_BASE: u64 = 0xC000_0000;
    const ANON_WINDOW_PAGES: usize = 64;

    /// The user virtual window guarded DMA buffers are carved into — distinct
    /// from the device, anon, and `FIXED` regions above.
    const DMA_WINDOW_BASE: u64 = 0x1_0000_0000;
    const DMA_WINDOW_PAGES: usize = 64;

    /// The user virtual window shared-memory regions are mapped into —
    /// distinct from the device, anon, DMA, and `FIXED` regions above.
    const SHARED_WINDOW_BASE: u64 = 0x2_0000_0000;
    const SHARED_WINDOW_PAGES: usize = 64;

    /// The user virtual window demand-paged file mappings are reserved in —
    /// distinct from every window and `FIXED` region above.
    const FILE_WINDOW_BASE: u64 = 0x3_0000_0000;
    const FILE_WINDOW_PAGES: usize = 64;

    /// The user virtual window code from another space is installed in.
    const CODE_WINDOW_BASE: u64 = 0x4_0000_0000;
    const CODE_WINDOW_PAGES: usize = 64;

    macro_rules! live_space {
        () => {{
            let (frames, _simmap) = backing!();
            LiveSpace::new(
                AddressSpace::new(HostPageTable::new()),
                host_tlb(),
                sim(),
                frames,
                VirtAddr::new(MMIO_WINDOW_BASE),
                MMIO_WINDOW_PAGES,
                VirtAddr::new(ANON_WINDOW_BASE),
                ANON_WINDOW_PAGES,
                VirtAddr::new(DMA_WINDOW_BASE),
                DMA_WINDOW_PAGES,
                VirtAddr::new(SHARED_WINDOW_BASE),
                SHARED_WINDOW_PAGES,
                VirtAddr::new(FILE_WINDOW_BASE),
                FILE_WINDOW_PAGES,
                VirtAddr::new(0),
                0,
            )
            .expect("a page-aligned, non-zero window is valid")
        }};
    }

    #[test]
    fn map_anonymous_returns_the_fixed_base_and_maps_zeroed_user_pages() {
        let mut live = live_space!();
        let base = 0x4000;
        assert_eq!(live.map_anonymous(base, 3), Ok(base));
        assert_eq!(live.space().mapped_pages(), 3);

        // The pages are readable user memory and read back as zero (no stale
        // bytes are ever user-visible).
        let sim = sim();
        let mut buf = vec![0xAAu8; 3 * PAGE_SIZE];
        copy_in(live.space(), &sim, VirtAddr::new(base), &mut buf).expect("readable user range");
        assert!(buf.iter().all(|&b| b == 0), "anonymous pages are zeroed");
    }

    #[test]
    fn unmap_anonymous_releases_the_whole_region() {
        let mut live = live_space!();
        let base = 0x4000;
        live.reserve_anonymous_at(base, 4).expect("reserve");
        live.map_anonymous(base, 4).expect("map");
        assert_eq!(live.space().mapped_pages(), 4);
        live.unmap_anonymous(base, 4, &mut Unpublished)
            .expect("unmap");
        assert_eq!(live.space().mapped_pages(), 0);
    }

    #[test]
    fn unmap_of_a_sparsely_resident_range_reclaims_only_the_resident_pages() {
        let mut live = live_space!();
        let base = 0x4000;
        // A demand-paged region: two of the three pages ever faulted in (the
        // fault path backs one page at a time via `map_anonymous`), so the
        // third is a never-touched reservation page.
        live.reserve_anonymous_at(base, 3).expect("reserve");
        live.map_anonymous(base, 2).expect("map");
        // Releasing the whole three-page range reclaims the two resident
        // pages and skips the unbacked one — sparse residency is not an
        // error (the caller validates the reservation before it gets here).
        live.unmap_anonymous(base, 3, &mut Unpublished)
            .expect("sparse release");
        assert_eq!(live.space().mapped_pages(), 0);
    }

    /// A caller-placed reservation claims only address space nothing else
    /// holds, so the release that tears it down by extent never frees a frame
    /// another kind of mapping owns.
    #[test]
    fn a_fixed_reservation_over_held_memory_is_refused() {
        let mut live = live_space!();
        let page = PAGE_SIZE as u64;
        let overlap = Err(LiveSpaceError::Anon(AnonError::Overlap));
        live.reserve_anonymous_at(0x4000, 2)
            .expect("free address space");
        for (at, pages) in [(0x4000, 1), (0x5000, 4), (0x3000, 2)] {
            assert_eq!(live.reserve_anonymous_at(at, pages), overlap, "{at:#x}");
        }
        let image = 0x10_0000;
        live.map_anonymous(image, 1)
            .expect("a page mapped by another path");
        assert_eq!(live.reserve_anonymous_at(image - page, 2), overlap);
        for window in [
            MMIO_WINDOW_BASE,
            ANON_WINDOW_BASE,
            DMA_WINDOW_BASE,
            SHARED_WINDOW_BASE,
            FILE_WINDOW_BASE,
        ] {
            assert_eq!(live.reserve_anonymous_at(window, 1), overlap, "{window:#x}");
            assert_eq!(
                live.reserve_anonymous_at(window - page, 2),
                overlap,
                "{window:#x}"
            );
        }
        assert_eq!(
            live.reserve_anonymous_at(0x6000, 2),
            Ok(0x6000),
            "beside one is free"
        );
    }

    /// A heap in a region outside every window — as the spawn layout sets one
    /// aside between the heap window and the file window — grows by `FIXED`
    /// maps at its top, hands its top back, and grows again, every step
    /// granted.
    #[test]
    fn a_heap_between_the_windows_grows_and_trims_at_its_top() {
        let mut live = live_space!();
        let page = PAGE_SIZE as u64;
        let region = 0x2_8000_0000;
        assert!(region > SHARED_WINDOW_BASE + SHARED_WINDOW_PAGES as u64 * page);
        assert!(region + 16 * page < FILE_WINDOW_BASE);
        let mut top = region;
        for pages in [1, 2, 4] {
            assert_eq!(live.reserve_anonymous_at(top, pages), Ok(top));
            top += pages * page;
        }
        live.unmap_anonymous(top - 5 * page, 5, &mut Unpublished)
            .expect("the top handed back");
        top -= 5 * page;
        assert_eq!(live.reserve_anonymous_at(top, 3), Ok(top), "grown again");
        live.unmap_anonymous(region, 5, &mut Unpublished)
            .expect("all of it");
        assert_eq!(
            live.reserve_anonymous_at(region, 1),
            Ok(region),
            "the region is whole again"
        );
    }

    /// A release outside the heap window reaches only a caller-placed
    /// reservation held whole: anything else is refused before a page goes.
    #[test]
    fn an_unmap_outside_a_held_fixed_reservation_frees_nothing() {
        let mut live = live_space!();
        let image = 0x10_0000;
        live.map_anonymous(image, 2)
            .expect("pages mapped by another path");
        let not_mapped = Err(LiveSpaceError::Anon(AnonError::NotMapped));
        assert_eq!(live.unmap_anonymous(image, 2, &mut Unpublished), not_mapped);
        assert_eq!(live.space().mapped_pages(), 2, "nothing was torn down");
        live.reserve_anonymous_at(0x4000, 2).expect("reserved");
        assert_eq!(
            live.unmap_anonymous(0x4000, 3, &mut Unpublished),
            not_mapped
        );
        live.unmap_anonymous(0x4000, 1, &mut Unpublished)
            .expect("part of it");
        assert_eq!(
            live.unmap_anonymous(0x4000, 1, &mut Unpublished),
            not_mapped
        );
        live.unmap_anonymous(0x5000, 1, &mut Unpublished)
            .expect("the rest");
        assert_eq!(live.space().mapped_pages(), 2);
    }

    #[test]
    fn map_anonymous_rejects_a_misaligned_base() {
        let mut live = live_space!();
        assert_eq!(
            live.map_anonymous(0x4001, 1),
            Err(LiveSpaceError::Anon(AnonError::Unaligned))
        );
    }

    #[test]
    fn map_device_window_returns_a_base_in_the_guarded_window() {
        let mut live = live_space!();
        // A device window at an arbitrary physical base; the returned VA is
        // chosen by the guarded allocator inside the configured window.
        let va = live
            .map_device_window(0xFE98_0000, 0x4000)
            .expect("a free slot exists");
        assert!(
            va >= MMIO_WINDOW_BASE
                && va < MMIO_WINDOW_BASE + (MMIO_WINDOW_PAGES as u64) * PAGE_SIZE as u64,
            "device VA lies inside the configured window"
        );
        // A guard page precedes the data, so the first data page is never the
        // window base itself.
        assert!(
            va > MMIO_WINDOW_BASE,
            "a leading guard page precedes the data"
        );
    }

    #[test]
    fn a_refused_device_window_is_unmapped_and_reported_page_aligned() {
        let mut live = live_space!();
        let kept = live
            .map_device_window(0xFE98_0000, 0x1000)
            .expect("a free slot exists");
        let revoked = live
            .map_framebuffer_window(0xFEA0_0100, 0x2000)
            .expect("a free slot exists");
        let mut released = std::vec::Vec::new();
        live.retain_device_windows(
            &mut |phys, len| (phys, len) != (0xFEA0_0100, 0x2000),
            &mut |base, pages| released.push((base, pages)),
        )
        .expect("the unmap succeeds");

        let page_at = |va: u64| Page::from_addr(VirtAddr::new(va & !(PAGE_SIZE as u64 - 1)));
        let revoked_page = revoked & !(PAGE_SIZE as u64 - 1);
        assert_eq!(
            released,
            [(revoked_page, 3)],
            "an offset span spans three pages"
        );
        for page in 0..3 {
            let va = revoked_page + page * PAGE_SIZE as u64;
            assert_eq!(live.translate_page(page_at(va).unwrap()), None);
        }
        assert!(
            live.translate_page(page_at(kept).unwrap()).is_some(),
            "the kept window stays"
        );
    }

    #[test]
    fn map_device_window_rejects_zero_length() {
        let mut live = live_space!();
        assert!(matches!(
            live.map_device_window(0xFE98_0000, 0),
            Err(LiveSpaceError::Mmio(_))
        ));
    }

    #[test]
    fn map_anonymous_placed_chooses_a_base_in_the_heap_window_and_zeroes_it() {
        let mut live = live_space!();
        let base = live
            .map_anonymous_placed(2)
            .expect("the heap window has room");
        assert!(
            base >= ANON_WINDOW_BASE
                && base < ANON_WINDOW_BASE + (ANON_WINDOW_PAGES as u64) * PAGE_SIZE as u64,
            "a placed base lies inside the heap window"
        );
        assert_eq!(live.space().mapped_pages(), 2);

        // The placed pages are readable user memory and read back as zero.
        let sim = sim();
        let mut buf = vec![0xAAu8; 2 * PAGE_SIZE];
        copy_in(live.space(), &sim, VirtAddr::new(base), &mut buf).expect("readable user range");
        assert!(buf.iter().all(|&b| b == 0), "placed pages are zeroed");
    }

    #[test]
    fn map_anonymous_placed_does_not_overlap_a_prior_placement() {
        let mut live = live_space!();
        let a = live.map_anonymous_placed(3).expect("room");
        let b = live.map_anonymous_placed(2).expect("room");
        assert_ne!(a, b);
        assert!(
            b >= a + 3 * PAGE_SIZE as u64,
            "the second placement bumps past the first"
        );
        assert_eq!(live.space().mapped_pages(), 5);
    }

    #[test]
    fn unmap_releases_a_placement_for_reuse() {
        let mut live = live_space!();
        let a = live.map_anonymous_placed(4).expect("room");
        live.unmap_anonymous(a, 4, &mut Unpublished)
            .expect("placed region unmaps");
        assert_eq!(live.space().mapped_pages(), 0);
        // The freed heap range is reused by the next placement (the bump
        // cursor did not simply advance past it).
        let b = live.map_anonymous_placed(4).expect("room");
        assert_eq!(b, a, "the freed placement base is reused");
    }

    #[test]
    fn unmap_of_a_placed_range_releases_its_pages_and_no_others() {
        let mut live = live_space!();
        let a = live.map_anonymous_placed(3).expect("room");
        // Part of a placement releases just those pages: the anonymous ABI
        // releases what the caller names, so a region grown over several
        // calls can hand back the part that came free.
        live.unmap_anonymous(a, 2, &mut Unpublished)
            .expect("a held range releases");
        assert_eq!(live.space().mapped_pages(), 1, "only the named pages went");

        // A range running past what the window holds is refused whole,
        // before any teardown — the page it does not hold makes the whole
        // release fail closed.
        assert_eq!(
            live.unmap_anonymous(a + 2 * PAGE_SIZE as u64, 2, &mut Unpublished),
            Err(LiveSpaceError::Anon(AnonError::NotMapped))
        );
        assert_eq!(live.space().mapped_pages(), 1, "no partial teardown");

        // The remaining page releases, and the whole placement's space is
        // then available again.
        live.unmap_anonymous(a + 2 * PAGE_SIZE as u64, 1, &mut Unpublished)
            .expect("the last held page releases");
        assert_eq!(live.space().mapped_pages(), 0);
        assert_eq!(
            live.map_anonymous_placed(3).expect("room"),
            a,
            "the released placement base is reused"
        );
    }

    #[test]
    fn reserve_anonymous_places_without_committing_and_faults_in_on_map() {
        let mut live = live_space!();
        let base = live.reserve_anonymous(4).expect("the heap window has room");
        assert!(
            base >= ANON_WINDOW_BASE
                && base < ANON_WINDOW_BASE + (ANON_WINDOW_PAGES as u64) * PAGE_SIZE as u64,
            "a reserved base lies inside the heap window"
        );
        // A reservation commits nothing: no page is resident yet.
        assert_eq!(live.space().mapped_pages(), 0, "reserve maps no frame");
        // The fault path backs one covering page at a time.
        live.map_anonymous(base + PAGE_SIZE as u64, 1)
            .expect("fault in the second page");
        assert_eq!(live.space().mapped_pages(), 1, "one page faulted in");
        // Releasing the whole reservation reclaims the one resident page and
        // skips the three never-touched ones, and frees the placement.
        live.unmap_anonymous(base, 4, &mut Unpublished)
            .expect("sparse release");
        assert_eq!(live.space().mapped_pages(), 0);
        // The freed placement base is reusable.
        assert_eq!(live.reserve_anonymous(4), Ok(base));
    }

    #[test]
    fn reserve_anonymous_at_validates_shape_and_commits_nothing() {
        let mut live = live_space!();
        let base = 0x8000;
        assert_eq!(live.reserve_anonymous_at(base, 2), Ok(base));
        assert_eq!(live.space().mapped_pages(), 0, "FIXED reserve maps nothing");
        assert_eq!(
            live.reserve_anonymous_at(base, 0),
            Err(LiveSpaceError::Anon(AnonError::ZeroLength))
        );
        assert_eq!(
            live.reserve_anonymous_at(base + 1, 1),
            Err(LiveSpaceError::Anon(AnonError::Unaligned))
        );
    }

    #[test]
    fn reserve_charges_the_commit_budget_and_fault_in_converts_it() {
        // A `mem_map` reservation reserves physical headroom up front (the
        // no-overcommit guarantee), a fault-in converts one reserved page to
        // residency, and unmap returns the still-unbacked reservations — so
        // the committed tally is exactly balanced across the region's life.
        let (frames, simmap) = backing!();
        let (mut live, _sim) = live_over(frames, simmap);
        assert_eq!(frames.committed_frames(), 0);

        let base = live.reserve_anonymous(4).expect("heap window has room");
        assert_eq!(
            frames.committed_frames(),
            4,
            "reserve charges the whole extent up front"
        );
        assert_eq!(live.space().mapped_pages(), 0, "but commits no frame yet");

        // Fault one page in: it converts from reserved-unbacked to resident.
        live.map_anonymous(base + PAGE_SIZE as u64, 1)
            .expect("fault in one page");
        assert_eq!(
            frames.committed_frames(),
            3,
            "one reservation became resident"
        );
        assert_eq!(live.space().mapped_pages(), 1);

        // Unmap: the three never-touched reservations are released and the
        // one resident page is freed, returning the tally to zero.
        live.unmap_anonymous(base, 4, &mut Unpublished)
            .expect("sparse release");
        assert_eq!(frames.committed_frames(), 0, "every commitment released");
        assert_eq!(live.space().mapped_pages(), 0);
    }

    #[test]
    fn a_growable_reservation_charges_only_the_pages_it_grows_onto() {
        // A thread stack: address space up front, headroom as it descends. The
        // whole point is that reserving a large span costs no RAM — a thread
        // whose default stack is the process's whole `stack-bytes` bound must
        // not be billed eight megabytes to push one frame — and the release
        // must credit back only what was actually charged. Crediting the
        // untouched pages (the `mem_map` rule) would hand the pool budget it
        // never received and let the machine overcommit.
        let (frames, simmap) = backing!();
        let (mut live, _sim) = live_over(frames, simmap);
        // An untouched `mem_map` reservation held alive across the release, so
        // an over-credit is *visible*: the global tally saturates at zero, so a
        // release that credited the six never-charged pages would silently read
        // the same as a correct one without a standing commitment to eat into.
        let held = live.reserve_anonymous(4).expect("heap window has room");
        assert_eq!(frames.committed_frames(), 4);

        let base = live
            .reserve_anonymous_growable(8)
            .expect("heap window has room");
        assert_eq!(
            frames.committed_frames(),
            4,
            "an eight-page span costs no physical headroom of its own"
        );

        // Grow onto two pages, the commit-then-map pair the growth path uses.
        live.commit_anonymous(2).expect("headroom for two pages");
        assert_eq!(frames.committed_frames(), 6);
        for index in 0..2 {
            live.map_anonymous(base + index * PAGE_SIZE as u64, 1)
                .expect("back one page");
        }
        assert_eq!(
            frames.committed_frames(),
            4,
            "both commitments became residency"
        );
        assert_eq!(live.space().mapped_pages(), 2);

        // Release: the two resident frames go back, and the six pages that were
        // never committed credit nothing — the standing reservation is untouched.
        live.unmap_anonymous(base, 8, &mut Unpublished)
            .expect("sparse release");
        assert_eq!(
            frames.committed_frames(),
            4,
            "the release credited back headroom it never charged"
        );
        assert_eq!(live.space().mapped_pages(), 0);
        // The placement is reusable, so the window is not leaked either.
        assert_eq!(live.reserve_anonymous_growable(8), Ok(base));
        // The standing `mem_map` reservation still releases correctly: its own
        // four never-touched pages, and no more.
        live.unmap_anonymous(held, 4, &mut Unpublished)
            .expect("release the reservation");
        assert_eq!(frames.committed_frames(), 0);
    }

    #[test]
    fn a_growable_reservation_with_no_memory_for_its_mark_places_nothing() {
        let mut live = live_space!();
        // The placement's record is granted; the growable mark is refused.
        let refused =
            tairix_fuzzseed::meter::refusing_after(1, || live.reserve_anonymous_growable(8));
        assert_eq!(refused, Err(LiveSpaceError::Anon(AnonError::OutOfMemory)));
        assert_eq!(live.anon.live(), 0, "the placement is given back");
        assert!(live.growable.is_empty());
        let base = live
            .reserve_anonymous_growable(8)
            .expect("heap window has room");
        assert!(live.growable.contains(&base));
    }

    #[test]
    fn dropping_a_space_releases_its_untouched_reservations() {
        // A task that dies holding untouched `mem_map` reservations must
        // return that reserved headroom to the global budget, or a
        // long-running system would leak commitment on every short-lived
        // process that reserved but never touched memory.
        let (frames, simmap) = backing!();
        {
            let (mut live, _sim) = live_over(frames, simmap);
            live.reserve_anonymous(5).expect("heap window has room");
            assert_eq!(frames.committed_frames(), 5);
        }
        assert_eq!(
            frames.committed_frames(),
            0,
            "teardown released the untouched reservations"
        );
    }

    /// The live space's DMA path must route the post-zero cache maintenance
    /// through *its own* `PhysMap` on both allocation and free: the carve is
    /// zeroed through the cacheable direct-map alias while the owning task
    /// reaches the same frames through a non-cacheable mapping, so a live
    /// space built over a physmap whose `clean_invalidate` does nothing
    /// leaves dirty zero lines that are later written back over the task's
    /// device-shared rings (the xHCI regression). This pins the
    /// delegation the spawn producers rely on when they wire the
    /// cache-maintaining physmap into each task's live space.
    #[test]
    fn dma_alloc_and_free_clean_the_direct_map_alias_through_the_spaces_physmap() {
        use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

        struct Recorded {
            calls: AtomicUsize,
            last_phys: AtomicU64,
            last_len: AtomicUsize,
        }
        struct RecordingSim {
            inner: SimPhysMap,
            recorded: &'static Recorded,
        }
        impl PhysMap for RecordingSim {
            fn translate(
                &self,
                phys: crate::frame::PhysAddr,
                len: usize,
            ) -> Option<core::ptr::NonNull<u8>> {
                self.inner.translate(phys, len)
            }

            fn clean_invalidate(&self, phys: crate::frame::PhysAddr, len: usize) {
                self.recorded.calls.fetch_add(1, Ordering::Relaxed);
                self.recorded
                    .last_phys
                    .store(phys.as_u64(), Ordering::Relaxed);
                self.recorded.last_len.store(len, Ordering::Relaxed);
            }

            fn sync_instruction_cache(&self, phys: crate::frame::PhysAddr, len: usize) {
                self.inner.sync_instruction_cache(phys, len);
            }

            fn fence_instruction_fetch(&self) -> bool {
                self.inner.fence_instruction_fetch()
            }
        }

        static RECORDED: Once<Recorded> = Once::new();
        let (frames, _simmap) = backing!();
        let recorded = RECORDED
            .call_once_infallible(|| Recorded {
                calls: AtomicUsize::new(0),
                last_phys: AtomicU64::new(0),
                last_len: AtomicUsize::new(0),
            })
            .expect("a fresh cell");
        let mut live = LiveSpace::new(
            AddressSpace::new(HostPageTable::new()),
            host_tlb(),
            RecordingSim {
                inner: sim(),
                recorded,
            },
            frames,
            VirtAddr::new(MMIO_WINDOW_BASE),
            MMIO_WINDOW_PAGES,
            VirtAddr::new(ANON_WINDOW_BASE),
            ANON_WINDOW_PAGES,
            VirtAddr::new(DMA_WINDOW_BASE),
            DMA_WINDOW_PAGES,
            VirtAddr::new(SHARED_WINDOW_BASE),
            SHARED_WINDOW_PAGES,
            VirtAddr::new(FILE_WINDOW_BASE),
            FILE_WINDOW_PAGES,
            VirtAddr::new(0),
            0,
        )
        .expect("windows are valid");

        let mapping = live
            .alloc_dma(2 * PAGE_SIZE, 0, custodian(custody!()))
            .expect("a free block exists");
        assert_eq!(
            recorded.calls.load(Ordering::Relaxed),
            1,
            "alloc cleans the alias"
        );
        assert_eq!(
            recorded.last_phys.load(Ordering::Relaxed),
            mapping.device_addr
        );
        assert_eq!(recorded.last_len.load(Ordering::Relaxed), 2 * PAGE_SIZE);

        live.free_dma(mapping.cpu_va, &mut Unpublished)
            .expect("live carve frees");
        assert_eq!(
            recorded.calls.load(Ordering::Relaxed),
            2,
            "free cleans the alias"
        );
        assert_eq!(
            recorded.last_phys.load(Ordering::Relaxed),
            mapping.device_addr
        );
        assert_eq!(recorded.last_len.load(Ordering::Relaxed), 2 * PAGE_SIZE);
    }

    #[test]
    fn alloc_dma_refuses_a_limit_no_ram_lies_below() {
        let mut live = live_space!();
        assert_eq!(
            live.alloc_dma(PAGE_SIZE, SIM_BASE, custodian(custody!())),
            Err(LiveSpaceError::Dma(DmaError::Alloc(AllocError::OutOfRange)))
        );
        assert_eq!(
            live.space().mapped_pages(),
            0,
            "no buffer mapped on refusal"
        );
    }

    #[test]
    fn alloc_dma_carves_below_a_limit_inside_ram_whatever_the_free_lists_offer_first() {
        // The free lists offer the top of the window first, so a carve that
        // took their front block and refused one above the limit failed here
        // with half the window free beneath it.
        let mut live = live_space!();
        let limit = SIM_BASE + (SIM_BYTES / 2) as u64;
        let mapping = live
            .alloc_dma(2 * PAGE_SIZE, limit, custodian(custody!()))
            .expect("free RAM lies below the limit");
        assert!(mapping.device_addr >= SIM_BASE);
        assert!(mapping.device_addr + 2 * PAGE_SIZE as u64 <= limit);
    }

    #[test]
    fn alloc_dma_rejects_zero_length() {
        let mut live = live_space!();
        assert_eq!(
            live.alloc_dma(0, 0, custodian(custody!())),
            Err(LiveSpaceError::Dma(DmaError::ZeroSize))
        );
    }

    /// A live space over the shared sim map, so a test can observe the bytes
    /// its carves hold after the space is gone.
    macro_rules! shared_live_space {
        ($frames:expr, $simmap:expr) => {
            shared_live_space!($frames, $simmap, host_tlb())
        };
        ($frames:expr, $simmap:expr, $tlb:expr) => {
            shared_live_space!(map: SharedSim($simmap), $frames, $tlb)
        };
        (map: $map:expr, $frames:expr, $tlb:expr) => {
            shared_live_space!(map: $map, $frames, $tlb, code: CODE_WINDOW_PAGES)
        };
        (map: $map:expr, $frames:expr, $tlb:expr, code: $code_pages:expr) => {{
            LiveSpace::new(
                AddressSpace::new(HostPageTable::new()),
                $tlb,
                $map,
                $frames,
                VirtAddr::new(MMIO_WINDOW_BASE),
                MMIO_WINDOW_PAGES,
                VirtAddr::new(ANON_WINDOW_BASE),
                ANON_WINDOW_PAGES,
                VirtAddr::new(DMA_WINDOW_BASE),
                DMA_WINDOW_PAGES,
                VirtAddr::new(SHARED_WINDOW_BASE),
                SHARED_WINDOW_PAGES,
                VirtAddr::new(FILE_WINDOW_BASE),
                FILE_WINDOW_PAGES,
                VirtAddr::new(CODE_WINDOW_BASE),
                $code_pages,
            )
            .expect("windows are valid")
        }};
    }

    /// Whether every byte of `block` reads zero through `simmap`.
    fn block_is_zero(simmap: &SimPhysMap, block: crate::frame::FrameBlock) -> bool {
        phys_is_zero(simmap, block.frame.start(), block.len())
    }

    /// Whether the `len` simulated bytes at `phys` are all zero.
    fn phys_is_zero(simmap: &SimPhysMap, phys: PhysAddr, len: usize) -> bool {
        let ptr = simmap
            .translate(phys, len)
            .expect("range in the sim window");
        // SAFETY: the sim map proved the pointer valid for `len` bytes, and no
        // reference to them is live while the test reads.
        let bytes = unsafe { core::slice::from_raw_parts(ptr.as_ptr(), len) };
        bytes.iter().all(|&b| b == 0)
    }

    /// Set the `len` simulated bytes at `phys` to `byte`, as a device or a
    /// previous owner would have left them.
    fn fill_phys(simmap: &SimPhysMap, phys: PhysAddr, len: usize, byte: u8) {
        let ptr = simmap
            .translate(phys, len)
            .expect("range in the sim window");
        // SAFETY: the sim map proved the pointer valid for `len` bytes, and no
        // reference to them is live while the test writes.
        unsafe { core::ptr::write_bytes(ptr.as_ptr(), byte, len) };
    }

    #[test]
    fn alloc_dma_maps_a_zeroed_coherent_buffer_in_the_dma_window() {
        let (frames, simmap) = backing!();
        // Whatever a carve is drawn from starts dirty, so only a scrub reads
        // back as zero.
        fill_phys(simmap, PhysAddr::new(SIM_BASE), SIM_BYTES, 0xAA);
        let mut live = shared_live_space!(frames, simmap);
        let mapping = live
            .alloc_dma(2 * PAGE_SIZE, 0, custodian(custody!()))
            .expect("a free block exists");
        // The CPU VA lies inside the configured DMA window, past the leading
        // guard page.
        assert!(
            mapping.cpu_va > DMA_WINDOW_BASE
                && mapping.cpu_va < DMA_WINDOW_BASE + (DMA_WINDOW_PAGES as u64) * PAGE_SIZE as u64,
            "DMA VA lies inside the configured window, past the leading guard"
        );
        // The backing block is physically contiguous RAM drawn from the
        // allocator's window.
        assert!(mapping.device_addr >= SIM_BASE, "phys base is real RAM");
        // The frames the task and the device share hold no stale bytes.
        assert!(
            phys_is_zero(simmap, PhysAddr::new(mapping.device_addr), 2 * PAGE_SIZE),
            "DMA buffer is zeroed"
        );
    }

    #[test]
    fn dropping_the_live_space_surrenders_every_dma_block_to_its_custodian() {
        // A device may still master a dead driver's DMA memory, so teardown
        // must hand it to the custodian — allocated, unmapped, and scrubbed —
        // never back to the allocator.
        let (frames, simmap) = backing!();
        let held = custody!();
        let before = frames.free_frames();
        let (first, second);
        {
            let mut live = shared_live_space!(frames, simmap);
            first = live
                .alloc_dma(2 * PAGE_SIZE, 0, custodian(held))
                .expect("a free block exists");
            second = live
                .alloc_dma(PAGE_SIZE, 0, custodian(held))
                .expect("a second block");
            fill_phys(simmap, PhysAddr::new(first.device_addr), 64, 0xC3);
            held.with(|r| assert_eq!(r.reserved, 2, "each carve reserved its surrender"));
        }
        assert_eq!(
            frames.free_frames(),
            before - 3,
            "no surrendered frame returned to the allocator"
        );
        held.with(|record| {
            assert_eq!(record.reserved, 0, "each surrender spent its reservation");
            let mut bases: alloc::vec::Vec<u64> = record
                .held
                .iter()
                .map(|&(node, generation, block)| {
                    assert_eq!((node, generation), (TEST_NODE, TEST_GENERATION));
                    assert!(block_is_zero(simmap, block), "a held block is scrubbed");
                    block.frame.start().as_u64()
                })
                .collect();
            bases.sort_unstable();
            let mut expected = [first.device_addr, second.device_addr];
            expected.sort_unstable();
            assert_eq!(bases, expected, "exactly the live carves were surrendered");
            for &(_, _, block) in &record.held {
                frames
                    .free_order(block.frame, block.order)
                    .expect("hygiene: the test releases what it held");
            }
        });
        assert_eq!(frames.free_frames(), before);
    }

    fn translated_custodian(
        domains: &'static crate::test_fixture::RecordingTranslation,
    ) -> DmaCustodian {
        DmaCustodian::translated(
            TEST_NODE,
            TEST_GENERATION,
            domains,
            u64::MAX,
            tairix_abi::DmaCoherence::Snooped,
        )
    }

    #[test]
    fn teardown_ends_a_translated_owner_once_then_frees_every_carve() {
        let (frames, simmap) = backing!();
        let domains = crate::test_fixture::translation!();
        let before = frames.free_frames();
        {
            let mut live = shared_live_space!(frames, simmap);
            let carve = live
                .alloc_dma(2 * PAGE_SIZE, 0, translated_custodian(domains))
                .expect("carve");
            assert_eq!(carve.device_addr, crate::test_fixture::IOVA_BASE);
            live.alloc_dma(PAGE_SIZE, 0, translated_custodian(domains))
                .expect("a second carve");
        }
        assert_eq!(frames.free_frames(), before, "both blocks went back");
        domains.with(|record| {
            assert!(record.held.is_empty(), "nothing needed custody");
            assert_eq!(record.ended, [(TEST_NODE, TEST_GENERATION)]);
            assert!(
                record.unmapped.is_empty(),
                "ended first, so no carve waited on the unit"
            );
        });
    }

    #[test]
    fn teardown_holds_a_translated_carve_its_unit_could_not_confirm() {
        let (frames, simmap) = backing!();
        let domains = crate::test_fixture::translation!(unconfirmed);
        let before = frames.free_frames();
        {
            let mut live = shared_live_space!(frames, simmap);
            live.alloc_dma(PAGE_SIZE, 0, translated_custodian(domains))
                .expect("carve");
        }
        assert_eq!(frames.free_frames(), before - 1);
        domains.with(|record| {
            assert_eq!(record.ended, [(TEST_NODE, TEST_GENERATION)]);
            assert_eq!(record.held.len(), 1, "the device may still reach it");
            let (_, _, block) = record.held[0];
            frames
                .free_order(block.frame, block.order)
                .expect("hygiene: the test releases what it held");
        });
    }

    #[test]
    fn a_space_bound_to_one_translation_refuses_a_carve_under_another() {
        let (frames, simmap) = backing!();
        let held = custody!();
        let mut live = shared_live_space!(frames, simmap);
        live.alloc_dma(PAGE_SIZE, 0, custodian(held))
            .expect("carve");
        assert_eq!(
            live.alloc_dma(
                PAGE_SIZE,
                0,
                translated_custodian(crate::test_fixture::translation!())
            ),
            Err(LiveSpaceError::Dma(DmaError::CustodianMismatch))
        );
    }

    #[test]
    fn a_space_binds_one_custodian_for_its_life() {
        let mut live = live_space!();
        let held = custody!();
        let other = custody!();
        live.alloc_dma(PAGE_SIZE, 0, custodian(held))
            .expect("the first carve binds");
        live.alloc_dma(PAGE_SIZE, 0, custodian(held))
            .expect("the bound custodian carves again");
        assert_eq!(
            live.alloc_dma(PAGE_SIZE, 0, custodian(other)),
            Err(LiveSpaceError::Dma(DmaError::CustodianMismatch)),
            "a second custody is refused"
        );
        let mut next = custodian(held);
        next.generation += 1;
        assert_eq!(
            live.alloc_dma(PAGE_SIZE, 0, next),
            Err(LiveSpaceError::Dma(DmaError::CustodianMismatch)),
            "so is another driver instance's"
        );
        other.with(|r| assert_eq!(r.reservations, 0));
        held.with(|r| assert_eq!(r.reservations, 2, "a refused carve reserved nothing"));
    }

    #[test]
    fn a_refused_reservation_refuses_the_carve() {
        let (frames, simmap) = backing!();
        let before = frames.free_frames();
        let mut live = shared_live_space!(frames, simmap);
        assert_eq!(
            live.alloc_dma(PAGE_SIZE, 0, custodian(custody!(refusing))),
            Err(LiveSpaceError::Dma(DmaError::Alloc(
                crate::error::AllocError::OutOfMemory
            )))
        );
        assert_eq!(frames.free_frames(), before, "no frame was carved");
        assert_eq!(live.space().mapped_pages(), 0);
    }

    #[test]
    fn a_space_that_never_carved_surrenders_nothing() {
        let held = custody!();
        {
            let mut live = live_space!();
            live.map_anonymous(0x4000, 1).expect("an ordinary mapping");
        }
        held.with(|r| {
            assert_eq!(r.reservations, 0);
            assert!(r.held.is_empty());
        });
    }

    #[test]
    fn dropping_the_live_space_reclaims_the_whole_footprint() {
        // The I2 regression test (`plans/APPS.md`): a task's exit must
        // return every frame it owned — `FIXED` and placed anonymous alike —
        // while leaving registry-owned shared frames, device windows and the
        // DMA carve (the custodian's now) untouched, and scrub the freed
        // bytes.
        let (frames, simmap) = backing!();
        let held = custody!();
        let before = frames.free_frames();

        // A stand-in shared-region frame owned by "the registry", mapped
        // into the space but never owned by it.
        let region_frame = frames.alloc(MemoryClass::UserAnon).expect("region frame");
        let after_region = before - 1;

        let fixed_base: u64 = 0x4000;
        let fixed_phys;
        {
            let mut live = LiveSpace::new(
                AddressSpace::new(HostPageTable::new()),
                host_tlb(),
                SharedSim(simmap),
                frames,
                VirtAddr::new(MMIO_WINDOW_BASE),
                MMIO_WINDOW_PAGES,
                VirtAddr::new(ANON_WINDOW_BASE),
                ANON_WINDOW_PAGES,
                VirtAddr::new(DMA_WINDOW_BASE),
                DMA_WINDOW_PAGES,
                VirtAddr::new(SHARED_WINDOW_BASE),
                SHARED_WINDOW_PAGES,
                VirtAddr::new(FILE_WINDOW_BASE),
                FILE_WINDOW_PAGES,
                VirtAddr::new(0),
                0,
            )
            .expect("windows are valid");

            live.map_anonymous(fixed_base, 2).expect("fixed anon");
            live.map_anonymous_placed(3).expect("placed anon");
            live.alloc_dma(2 * PAGE_SIZE, 0, custodian(held))
                .expect("dma carve");
            live.map_device_window(0xFE98_0000, 0x2000)
                .expect("device window");
            live.map_shared_chunks(
                &[(region_frame.start().as_u64(), 1)],
                SharedMemory::Cacheable,
            )
            .expect("shared region");

            // A recognisable secret in a fixed anonymous page, so the
            // zero-on-free scrub below is observable.
            copy_out(
                live.space(),
                simmap,
                VirtAddr::new(fixed_base),
                &[0xA5u8; 32],
            )
            .expect("writable user range");
            fixed_phys = live
                .space()
                .translate(crate::vmm::Page::from_addr(VirtAddr::new(fixed_base)).expect("aligned"))
                .expect("mapped")
                .0;

            assert!(
                frames.free_frames() < after_region,
                "the mappings consumed frames"
            );
        }

        // Every frame the task owned returned to the allocator; the
        // registry-owned region frame did not (its lifecycle belongs to
        // the shared-region registry), and nor did the DMA carve.
        let dma_pages = held.with(|record| {
            assert_eq!(record.held.len(), 1, "the one carve was surrendered");
            record.held[0].2.len() / PAGE_SIZE
        });
        assert_eq!(
            frames.free_frames(),
            after_region - dma_pages,
            "exit reclaims the whole owned footprint and nothing else"
        );

        // The freed anonymous frame was scrubbed before it returned: the
        // dead task's bytes are unrecoverable through the direct map.
        let ptr = simmap
            .translate(fixed_phys.start(), PAGE_SIZE)
            .expect("frame in the sim window");
        // SAFETY: the sim map proved the pointer valid for a page; the
        // frame is free, so nothing else references it in this test.
        let bytes = unsafe { core::slice::from_raw_parts(ptr.as_ptr(), 32) };
        assert!(
            bytes.iter().all(|&b| b == 0),
            "freed frames are zeroed on teardown"
        );

        // Hygiene: return the registry frame and the held carve so the
        // allocator is whole.
        let _ = frames.free(region_frame);
        held.with(|record| {
            let (_, _, block) = record.held[0];
            frames
                .free_order(block.frame, block.order)
                .expect("the held carve frees");
        });
        assert_eq!(frames.free_frames(), before);
    }

    /// A window the live space refuses tears down the space it was handed,
    /// so every frame that space maps goes back, zeroed.
    ///
    /// Regression: the refusal dropped the space bare, which keeps its frames
    /// and page tables for good.
    #[test]
    fn a_refused_window_returns_the_frames_its_space_maps() {
        let (frames, simmap) = backing!();
        let before = frames.free_frames();
        let frame = frames.alloc(MemoryClass::UserAnon).expect("a free frame");
        fill_phys(simmap, frame.start(), PAGE_SIZE, 0xA5);
        let mut space = AddressSpace::new(HostPageTable::new());
        space
            .map(
                Page::from_addr(VirtAddr::new(0x4000)).expect("aligned"),
                frame,
                crate::vmm::MapFlags::READ | crate::vmm::MapFlags::USER,
            )
            .expect("maps");
        // A device window of no pages is the configuration fault.
        let refused = LiveSpace::new(
            space,
            host_tlb(),
            SharedSim(simmap),
            frames,
            VirtAddr::new(MMIO_WINDOW_BASE),
            0,
            VirtAddr::new(ANON_WINDOW_BASE),
            ANON_WINDOW_PAGES,
            VirtAddr::new(DMA_WINDOW_BASE),
            DMA_WINDOW_PAGES,
            VirtAddr::new(SHARED_WINDOW_BASE),
            SHARED_WINDOW_PAGES,
            VirtAddr::new(FILE_WINDOW_BASE),
            FILE_WINDOW_PAGES,
            VirtAddr::new(0),
            0,
        );
        assert!(matches!(
            refused,
            Err(crate::mmio::MmioError::InvalidMapConfig)
        ));
        assert_eq!(frames.free_frames(), before, "the space's frame went back");
        assert!(
            phys_is_zero(simmap, frame.start(), PAGE_SIZE),
            "and was zeroed first"
        );
    }

    #[test]
    fn free_dma_releases_a_carve_and_repeated_cycles_reclaim_fully() {
        // `free_dma` is the per-buffer release the `dma_free` syscall drives:
        // a long-running driver allocates and frees a DMA buffer every
        // transfer, so many alloc/free cycles must leave the frame allocator
        // exactly as full as it started — never marching upward (the leak the
        // syscall exists to close).
        let (frames, _simmap) = backing!();
        let before = frames.free_frames();
        let mut live = LiveSpace::new(
            AddressSpace::new(HostPageTable::new()),
            host_tlb(),
            sim(),
            frames,
            VirtAddr::new(MMIO_WINDOW_BASE),
            MMIO_WINDOW_PAGES,
            VirtAddr::new(ANON_WINDOW_BASE),
            ANON_WINDOW_PAGES,
            VirtAddr::new(DMA_WINDOW_BASE),
            DMA_WINDOW_PAGES,
            VirtAddr::new(SHARED_WINDOW_BASE),
            SHARED_WINDOW_PAGES,
            VirtAddr::new(FILE_WINDOW_BASE),
            FILE_WINDOW_PAGES,
            VirtAddr::new(0),
            0,
        )
        .expect("windows are valid");

        let held = custody!();
        for _ in 0..50 {
            let mapping = live
                .alloc_dma(2 * PAGE_SIZE, 0, custodian(held))
                .expect("a free block");
            assert!(frames.free_frames() < before, "the carve consumed frames");
            live.free_dma(mapping.cpu_va, &mut Unpublished)
                .expect("free by cpu base");
            assert_eq!(
                frames.free_frames(),
                before,
                "each free returns every frame — no leak across cycles"
            );
            assert_eq!(live.space().mapped_pages(), 0, "no data page left mapped");
            held.with(|r| assert_eq!(r.reserved, 0, "the free returned its reservation"));
        }
        // A free of an address that names no live carve fails closed.
        assert_eq!(
            live.free_dma(DMA_WINDOW_BASE + PAGE_SIZE as u64, &mut Unpublished),
            Err(LiveSpaceError::Dma(DmaError::UnknownBuffer))
        );
        held.with(|r| assert_eq!(r.reserved, 0, "a refused free returns nothing"));
    }

    #[test]
    fn a_failed_carve_returns_its_reservation() {
        let held = custody!();
        let mut live = live_space!();
        assert!(live.alloc_dma(0, 0, custodian(held)).is_err());
        held.with(|r| {
            assert_eq!(r.reservations, 1, "the carve reserved before it failed");
            assert_eq!(r.reserved, 0, "and gave the room back");
        });
    }

    #[test]
    fn a_free_that_fails_part_way_still_returns_its_reservation() {
        // The record goes before the release can fail, so the block will
        // never be surrendered: a reservation kept for it would stop its
        // node's custody from ever going idle.
        let (frames, simmap) = backing!();
        let held = custody!();
        let mut live = shared_live_space!(frames, simmap);
        let mapping = live
            .alloc_dma(PAGE_SIZE, 0, custodian(held))
            .expect("a free block exists");
        let frame = crate::frame::Frame::containing(PhysAddr::new(mapping.device_addr));
        frames
            .free_order(frame, 0)
            .expect("the block is freed behind the space's back");
        assert_eq!(
            live.free_dma(mapping.cpu_va, &mut Unpublished),
            Err(LiveSpaceError::Dma(DmaError::Alloc(
                AllocError::InvariantViolation
            ))),
            "the allocator refuses the second free"
        );
        held.with(|r| assert_eq!(r.reserved, 0, "the room went back"));
        assert_eq!(
            live.free_dma(mapping.cpu_va, &mut Unpublished),
            Err(LiveSpaceError::Dma(DmaError::UnknownBuffer)),
            "and a repeat returns nothing more"
        );
    }

    /// A [`SharedSim`] that can be told to stop reaching RAM, as a platform
    /// whose direct map misses a region does.
    struct RefusableSim {
        sim: &'static SimPhysMap,
        refusing: alloc::sync::Arc<core::sync::atomic::AtomicBool>,
    }

    impl PhysMap for RefusableSim {
        fn translate(
            &self,
            phys: crate::frame::PhysAddr,
            len: usize,
        ) -> Option<core::ptr::NonNull<u8>> {
            if self.refusing.load(core::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            self.sim.translate(phys, len)
        }

        fn clean_invalidate(&self, phys: crate::frame::PhysAddr, len: usize) {
            self.sim.clean_invalidate(phys, len);
        }

        fn sync_instruction_cache(&self, phys: crate::frame::PhysAddr, len: usize) {
            self.sim.sync_instruction_cache(phys, len);
        }

        fn fence_instruction_fetch(&self) -> bool {
            self.sim.fence_instruction_fetch()
        }
    }

    #[test]
    fn a_free_that_keeps_its_block_keeps_its_reservation_for_teardown() {
        // A block that cannot be scrubbed stays live, and the teardown that
        // surrenders it spends its reservation: returning the reservation at
        // the failed free as well would spend it twice.
        let (frames, simmap) = backing!();
        let held = custody!();
        let refusing = alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));
        let mut live = shared_live_space!(
            map: RefusableSim {
                sim: simmap,
                refusing: alloc::sync::Arc::clone(&refusing),
            },
            frames,
            host_tlb()
        );
        let mapping = live
            .alloc_dma(PAGE_SIZE, 0, custodian(held))
            .expect("a free block exists");
        refusing.store(true, core::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            live.free_dma(mapping.cpu_va, &mut Unpublished),
            Err(LiveSpaceError::Dma(DmaError::DirectMap)),
            "the scrub cannot reach the block"
        );
        held.with(|r| assert_eq!(r.reserved, 1, "the kept block still holds its room"));
        drop(live);
        held.with(|r| {
            assert_eq!(r.reserved, 0, "teardown spent the room");
            assert_eq!(r.held.len(), 1, "on the block it surrendered");
        });
    }

    #[test]
    fn map_shared_chunks_maps_a_region_in_its_window_and_unmaps_by_base() {
        let mut live = live_space!();
        // A real frame reachable through the sim direct map; the map only
        // installs page-table entries (the registry owns/zeroes the frames).
        let phys = SIM_BASE;
        let len = 2 * PAGE_SIZE;
        let base = live
            .map_shared_chunks(&[(phys, 2)], SharedMemory::Cacheable)
            .expect("maps the region");
        // The mapping lands inside the configured shared window, past the
        // leading guard page, and clear of the DMA window below it.
        assert!(
            base > SHARED_WINDOW_BASE
                && base < SHARED_WINDOW_BASE + (SHARED_WINDOW_PAGES as u64) * PAGE_SIZE as u64,
            "shared VA lies inside the configured window, past the leading guard"
        );
        // The region is read/write through the CPU mapping: a write is
        // visible on read-back through the same backing frames.
        let sim = sim();
        let payload = [0x5Au8; 16];
        copy_out(live.space(), &sim, VirtAddr::new(base), &payload).expect("writable user range");
        let mut back = [0u8; 16];
        copy_in(live.space(), &sim, VirtAddr::new(base), &mut back).expect("readable user range");
        assert_eq!(back, payload, "shared region round-trips reads and writes");
        // Unmapping by base tears down the page-table entries; the frames are
        // not freed here (the registry owns them).
        live.unmap_shared(base, len).expect("unmaps by base");
        assert_eq!(
            live.space().mapped_pages(),
            0,
            "the shared mapping's pages are gone after unmap"
        );
        // A double-unmap of the same base fails closed.
        assert!(
            live.unmap_shared(base, len).is_err(),
            "double-unmap of a shared region fails closed"
        );
    }

    #[test]
    fn map_shared_chunks_maps_a_multi_block_region_into_one_window() {
        let mut live = live_space!();
        // Two physically-disjoint blocks in the sim window become one flat
        // three-page shared window — the display frame ring a single buddy
        // block could not hold.
        let chunk_a = SIM_BASE; // 2 pages
        let chunk_b = SIM_BASE + 4 * PAGE_SIZE as u64; // 1 page, past chunk_a
        let base = live
            .map_shared_chunks(&[(chunk_a, 2), (chunk_b, 1)], SharedMemory::Cacheable)
            .expect("chunk list maps into one window");
        assert!(
            base > SHARED_WINDOW_BASE
                && base < SHARED_WINDOW_BASE + (SHARED_WINDOW_PAGES as u64) * PAGE_SIZE as u64,
            "shared VA lies inside the configured window, past the leading guard"
        );
        assert_eq!(live.space().mapped_pages(), 3);

        // The window is one flat buffer: a write into the page backed by the
        // *second* block round-trips, proving the blocks are contiguous in
        // virtual space and both reachable.
        let sim = sim();
        let payload = [0xA5u8; 16];
        let second_block_va = VirtAddr::new(base + 2 * PAGE_SIZE as u64);
        copy_out(live.space(), &sim, second_block_va, &payload).expect("writable user range");
        let mut back = [0u8; 16];
        copy_in(live.space(), &sim, second_block_va, &mut back).expect("readable user range");
        assert_eq!(
            back, payload,
            "second-block page round-trips through the window"
        );

        // Unmapping by base tears down all three pages; the frames belong to
        // the registry and are not freed here.
        live.unmap_shared(base, 3 * PAGE_SIZE)
            .expect("unmaps the whole window by base");
        assert_eq!(live.space().mapped_pages(), 0);
    }

    #[test]
    fn a_file_region_reserves_faults_reads_back_and_releases_sparsely() {
        let (frames, simmap) = backing!();
        let before = frames.free_frames();
        let (mut live, simmap) = live_over(frames, simmap);
        // Reserving draws pure address space: no frame moves.
        let base = live.reserve_file_region(4).expect("window has room");
        assert!(base >= FILE_WINDOW_BASE);
        assert_eq!(frames.free_frames(), before, "reservation costs no RAM");

        // Fault two of the four pages resident and read the bytes back
        // through the uaccess boundary.
        live.map_file_page_at(base + PAGE_SIZE as u64, &[0x11; 8])
            .expect("fault page 1");
        live.map_file_page_at(base + 3 * PAGE_SIZE as u64, &[0x33; 8])
            .expect("fault page 3");
        let mut buf = [0u8; 8];
        copy_in(
            live.space(),
            simmap,
            VirtAddr::new(base + PAGE_SIZE as u64),
            &mut buf,
        )
        .expect("resident page reads");
        assert_eq!(buf, [0x11; 8]);

        // Releasing reclaims exactly the two resident pages and returns
        // the reservation for reuse.
        assert_eq!(live.release_file_region(base, 4, &mut Unpublished), Ok(2));
        assert_eq!(frames.free_frames(), before, "all frames returned");
        assert_eq!(
            live.release_file_region(base, 4, &mut Unpublished),
            Err(LiveSpaceError::Anon(AnonError::NotMapped)),
            "a released region is gone"
        );
        let again = live.reserve_file_region(4).expect("slots were returned");
        assert_eq!(again, base, "first-fit reuses the released range");
    }

    #[test]
    fn a_fault_outside_any_reserved_file_region_is_refused() {
        let mut live = live_space!();
        // Nothing reserved: any window address is refused.
        assert_eq!(
            live.map_file_page_at(FILE_WINDOW_BASE, &[1]),
            Err(LiveSpaceError::Anon(AnonError::NotMapped))
        );
        // With a reservation, an address past its top is still refused.
        let base = live.reserve_file_region(2).expect("room");
        assert_eq!(
            live.map_file_page_at(base + 2 * PAGE_SIZE as u64, &[1]),
            Err(LiveSpaceError::Anon(AnonError::NotMapped))
        );
    }

    #[test]
    fn unmap_anonymous_refuses_a_file_region_base() {
        let mut live = live_space!();
        let base = live.reserve_file_region(2).expect("room");
        live.map_file_page_at(base, &[7]).expect("fault");
        // The anonymous release path must not tear down (or even inspect)
        // a file region — wrong syscall, fail closed.
        assert_eq!(
            live.unmap_anonymous(base, 2, &mut Unpublished),
            Err(LiveSpaceError::Anon(AnonError::NotMapped))
        );
        assert_eq!(live.release_file_region(base, 2, &mut Unpublished), Ok(1));
    }

    #[test]
    fn dropping_the_live_space_reclaims_resident_file_pages() {
        let (frames, simmap) = backing!();
        let before = frames.free_frames();
        {
            let (mut live, _sim) = live_over(frames, simmap);
            let base = live.reserve_file_region(8).expect("room");
            live.map_file_page_at(base, &[1]).expect("fault");
            live.map_file_page_at(base + 5 * PAGE_SIZE as u64, &[2])
                .expect("fault");
            assert!(frames.free_frames() < before);
        }
        // Teardown walked the live pages: both resident file pages (and the
        // page-table frames) came back.
        assert_eq!(frames.free_frames(), before);
    }

    /// A live space over the caller's own `'static` allocator handle and a
    /// shared simulated physical map (returned alongside), so a test can
    /// watch frame counts across the space's life and read the bytes the
    /// space wrote through the same storage.
    fn live_over(
        frames: &'static FrameAllocator,
        simmap: &'static SimPhysMap,
    ) -> (LiveSpace<HostPageTable, SharedSim>, &'static SimPhysMap) {
        (shared_live_space!(frames, simmap), simmap)
    }

    /// The live space must be `Send`: `kernel/core` moves it into the
    /// process's shared `ProcessSpace`, whose threads may run on any CPU.
    #[test]
    fn live_space_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<LiveSpace<HostPageTable, SimPhysMap>>();
        assert_send::<Box<dyn LiveUserSpace + Send>>();
    }

    /// Memory moved out of one space and code installed in another.
    mod code {
        use super::{fill_phys, host_tlb, phys_is_zero, SharedSim};
        use super::{
            ANON_WINDOW_BASE, ANON_WINDOW_PAGES, CODE_WINDOW_BASE, CODE_WINDOW_PAGES,
            DMA_WINDOW_BASE, DMA_WINDOW_PAGES, FILE_WINDOW_BASE, FILE_WINDOW_PAGES,
            MMIO_WINDOW_BASE, MMIO_WINDOW_PAGES, SHARED_WINDOW_BASE, SHARED_WINDOW_PAGES, SIM_BASE,
            SIM_PAGES,
        };
        use crate::anon::{page_at, AnonError};
        use crate::frame::{Frame, FrameAllocator, MemoryClass, PhysAddr, PAGE_SIZE};
        use crate::live::{LiveSpace, LiveSpaceError, LiveUserSpace, CODE_FLAGS};
        use crate::phys::{PhysMap, SimPhysMap};
        use crate::retire::Unpublished;
        use crate::test_fixture::frame_backing;
        use crate::uaccess::{copy_in, copy_out, UaccessError};
        use crate::vmm::{AddressSpace, HostPageTable, VirtAddr};
        use core::sync::atomic::{AtomicBool, Ordering};
        use tairix_sync::SpinLock;

        extern crate std;
        use std::vec;
        use std::vec::Vec;

        /// What the direct map was asked to do for code, in order.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Step {
            Synced(PhysAddr),
            Fenced,
        }

        /// A direct map over shared simulated RAM that records each
        /// instruction-cache sync and fence, a fence succeeding while
        /// `fences` holds.
        struct CodeSim {
            sim: &'static SimPhysMap,
            fences: AtomicBool,
            steps: SpinLock<Vec<Step>>,
        }

        impl CodeSim {
            fn new(sim: &'static SimPhysMap, fences: bool) -> Self {
                Self {
                    sim,
                    fences: AtomicBool::new(fences),
                    steps: SpinLock::new(Vec::new()),
                }
            }

            fn steps(&self) -> Vec<Step> {
                core::mem::take(&mut *self.steps.lock())
            }
        }

        impl PhysMap for CodeSim {
            fn translate(&self, phys: PhysAddr, len: usize) -> Option<core::ptr::NonNull<u8>> {
                self.sim.translate(phys, len)
            }

            fn clean_invalidate(&self, phys: PhysAddr, len: usize) {
                self.sim.clean_invalidate(phys, len);
            }

            fn sync_instruction_cache(&self, phys: PhysAddr, len: usize) {
                assert_eq!(len, PAGE_SIZE, "a whole page at a time");
                self.steps.lock().push(Step::Synced(phys));
            }

            fn fence_instruction_fetch(&self) -> bool {
                self.steps.lock().push(Step::Fenced);
                self.fences.load(Ordering::Relaxed)
            }
        }

        /// A frame holding `byte` throughout, as a compiler leaves one.
        fn written(frames: &FrameAllocator, simmap: &SimPhysMap, byte: u8) -> Frame {
            let frame = frames
                .alloc(MemoryClass::UserAnon)
                .expect("a frame is free");
            fill_phys(simmap, frame.start(), PAGE_SIZE, byte);
            frame
        }

        fn scrubbed(simmap: &SimPhysMap, frames: &[Frame]) -> bool {
            frames
                .iter()
                .all(|frame| phys_is_zero(simmap, frame.start(), PAGE_SIZE))
        }

        /// Place `frames` in `live`'s code window and install them there.
        fn install(live: &mut dyn LiveUserSpace, frames: &[Frame]) -> Result<u64, LiveSpaceError> {
            let base = live.reserve_code(frames.len() as u64)?;
            live.map_code(base, frames).map(|()| base)
        }

        #[test]
        fn code_maps_read_execute_at_its_window_once_cleaned_and_fenced() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(map: CodeSim::new(simmap, true), frames, host_tlb());
            let code = [written(frames, simmap, 0xC3), written(frames, simmap, 0x90)];

            let base = live.reserve_code(2).expect("places");
            assert_eq!(base, CODE_WINDOW_BASE);
            assert!(
                live.physmap.steps().is_empty(),
                "placing does no cache work"
            );
            assert_eq!(live.space().mapped_pages(), 0, "nor maps a page");
            live.map_code(base, &code).expect("installs");
            for (index, &frame) in (0u64..).zip(&code) {
                let page = page_at(base, index).expect("in range");
                assert_eq!(live.translate_page(page), Some((frame, CODE_FLAGS)));
            }
            assert_eq!(
                live.physmap.steps(),
                [
                    Step::Synced(code[0].start()),
                    Step::Synced(code[1].start()),
                    Step::Fenced
                ],
                "every page cleaned, then one fence"
            );

            let mut bytes = vec![0; 2 * PAGE_SIZE];
            copy_in(live.space(), simmap, VirtAddr::new(base), &mut bytes).expect("readable");
            assert!(bytes[..PAGE_SIZE].iter().all(|&byte| byte == 0xC3));
            assert!(bytes[PAGE_SIZE..].iter().all(|&byte| byte == 0x90));
            assert_eq!(
                copy_out(live.space(), simmap, VirtAddr::new(base), &[0]),
                Err(UaccessError::NotWritable)
            );
        }

        #[test]
        fn a_refused_fence_maps_nothing_scrubs_every_frame_and_gives_the_placement_back() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(map: CodeSim::new(simmap, false), frames, host_tlb());
            let code = [written(frames, simmap, 0xC3), written(frames, simmap, 0xC3)];
            let free = frames.free_frames();

            assert_eq!(
                install(&mut live, &code),
                Err(LiveSpaceError::Anon(AnonError::InstructionFence))
            );
            assert_eq!(live.space().mapped_pages(), 0);
            assert_eq!(frames.free_frames(), free + 2);
            assert!(scrubbed(simmap, &code));
            assert_eq!(live.reserve_code(1), Ok(CODE_WINDOW_BASE));
        }

        #[test]
        fn code_with_nowhere_to_go_is_refused_its_placement() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let map = CodeSim::new(simmap, true);
            let mut absent = shared_live_space!(map: map, frames, host_tlb(), code: 0);
            assert_eq!(
                absent.reserve_code(1),
                Err(LiveSpaceError::Anon(AnonError::OutOfMemory))
            );

            let map = CodeSim::new(simmap, true);
            let mut full = shared_live_space!(map: map, frames, host_tlb(), code: 1);
            assert_eq!(
                full.reserve_code(2),
                Err(LiveSpaceError::Anon(AnonError::OutOfMemory))
            );
            assert_eq!(
                full.reserve_code(0),
                Err(LiveSpaceError::Anon(AnonError::ZeroLength))
            );
            assert_eq!(full.reserve_code(1), Ok(CODE_WINDOW_BASE));
        }

        #[test]
        fn code_maps_only_over_the_whole_of_an_empty_placement() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(map: CodeSim::new(simmap, true), frames, host_tlb());
            let base = live.reserve_code(2).expect("places");
            let refused = Err(LiveSpaceError::Anon(AnonError::NotMapped));

            for (at, count) in [
                (base, 1),
                (base, 3),
                (base + PAGE_SIZE as u64, 1),
                (0x7000_0000, 2),
            ] {
                let code: Vec<Frame> = (0..count).map(|_| written(frames, simmap, 0xC3)).collect();
                let free = frames.free_frames();
                assert_eq!(live.map_code(at, &code), refused, "{count} at {at:#x}");
                assert_eq!(
                    frames.free_frames(),
                    free + code.len(),
                    "the frames went back"
                );
                assert!(scrubbed(simmap, &code));
            }
            assert!(
                live.physmap.steps().is_empty(),
                "no cache work for a refusal"
            );

            let code = [written(frames, simmap, 0xC3), written(frames, simmap, 0xC3)];
            live.map_code(base, &code)
                .expect("installs over the placement");
            let again = [written(frames, simmap, 0x90), written(frames, simmap, 0x90)];
            assert_eq!(
                live.map_code(base, &again),
                refused,
                "never over installed code"
            );
            assert!(scrubbed(simmap, &again));
            assert!(!scrubbed(simmap, &code), "the installed code is untouched");
        }

        #[test]
        fn released_code_is_scrubbed_and_freed_page_by_page_and_only_once() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(map: CodeSim::new(simmap, true), frames, host_tlb());
            let code = [written(frames, simmap, 0xC3), written(frames, simmap, 0x90)];
            let base = install(&mut live, &code).expect("installs");
            let free = frames.free_frames();

            assert_eq!(
                live.release_code(base, 3, &mut Unpublished),
                Err(LiveSpaceError::Anon(AnonError::NotMapped)),
                "a range past the code releases none of it"
            );
            assert_eq!(live.space().mapped_pages(), 2);

            let second = base + PAGE_SIZE as u64;
            assert_eq!(live.release_code(second, 1, &mut Unpublished), Ok(1));
            assert_eq!(frames.free_frames(), free + 1);
            assert!(scrubbed(simmap, &code[1..]));
            assert!(
                !scrubbed(simmap, &code[..1]),
                "the other page is code still"
            );
            assert_eq!(
                live.release_code(second, 1, &mut Unpublished),
                Err(LiveSpaceError::Anon(AnonError::NotMapped))
            );

            assert_eq!(live.release_code(base, 1, &mut Unpublished), Ok(1));
            assert_eq!(live.space().mapped_pages(), 0);
            assert_eq!(frames.free_frames(), free + 2);

            let empty = live.reserve_code(2).expect("places");
            assert_eq!(
                live.release_code(empty, 2, &mut Unpublished),
                Ok(0),
                "a placement nothing was installed at goes back holding nothing"
            );
            assert_eq!(live.reserve_code(2), Ok(empty));
        }

        #[test]
        fn anonymous_memory_and_installed_code_are_released_only_as_themselves() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(map: CodeSim::new(simmap, true), frames, host_tlb());
            let base = install(&mut live, &[written(frames, simmap, 0xC3)]).expect("installs");
            let refused = Err(LiveSpaceError::Anon(AnonError::NotMapped));
            assert_eq!(live.unmap_anonymous(base, 1, &mut Unpublished), refused);
            let mut moved = Vec::new();
            assert_eq!(
                live.detach_anonymous(base, 1, &mut Unpublished, &mut moved),
                refused
            );
            assert!(moved.is_empty(), "installed code never moves on");
            assert_eq!(
                live.reserve_anonymous_at(base, 1),
                Err(LiveSpaceError::Anon(AnonError::Overlap))
            );
            assert_eq!(live.space().mapped_pages(), 1);

            let anon = live.map_anonymous_placed(1).expect("places");
            assert_eq!(
                live.release_code(anon, 1, &mut Unpublished).map(drop),
                refused
            );
            assert_eq!(live.space().mapped_pages(), 2);
        }

        #[test]
        fn dropping_the_space_scrubs_and_frees_its_code() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let free = frames.free_frames();
            let code = [written(frames, simmap, 0xC3), written(frames, simmap, 0x90)];
            let mut live = shared_live_space!(map: CodeSim::new(simmap, true), frames, host_tlb());
            install(&mut live, &code).expect("installs");
            drop(live);
            assert_eq!(frames.free_frames(), free);
            assert!(scrubbed(simmap, &code));
        }

        #[test]
        fn detached_memory_leaves_with_its_bytes_and_installs_in_another_space() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut compiler = shared_live_space!(frames, simmap);
            let base = compiler.reserve_anonymous(3).expect("places");
            compiler.map_anonymous(base, 3).expect("backs");
            let pattern: Vec<u8> = (0..=u8::MAX).cycle().take(2 * PAGE_SIZE).collect();
            copy_out(compiler.space(), simmap, VirtAddr::new(base), &pattern).expect("writable");
            let free = frames.free_frames();

            let mut moved = Vec::new();
            compiler
                .detach_anonymous(base, 2, &mut Unpublished, &mut moved)
                .expect("detaches");
            assert_eq!(moved.len(), 2);
            assert_eq!(compiler.space().mapped_pages(), 1, "the third page stays");
            assert_eq!(frames.free_frames(), free, "no frame was freed");
            assert_eq!(frames.committed_frames(), 0);
            assert_eq!(
                compiler.detach_anonymous(base, 1, &mut Unpublished, &mut moved),
                Err(LiveSpaceError::Anon(AnonError::NotMapped))
            );
            assert_eq!(moved.len(), 2, "a refusal adds nothing");
            compiler
                .unmap_anonymous(base + 2 * PAGE_SIZE as u64, 1, &mut Unpublished)
                .expect("the page left behind still unmaps");

            let mut target =
                shared_live_space!(map: CodeSim::new(simmap, true), frames, host_tlb());
            let at = install(&mut target, &moved).expect("installs");
            let mut bytes = vec![0; 2 * PAGE_SIZE];
            copy_in(target.space(), simmap, VirtAddr::new(at), &mut bytes).expect("readable");
            assert_eq!(bytes, pattern);
        }

        #[test]
        fn a_range_not_wholly_resident_anonymous_memory_detaches_nothing() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(frames, simmap);
            let refused = Err(LiveSpaceError::Anon(AnonError::NotMapped));
            let mut never = Vec::new();

            let sparse = live.reserve_anonymous(2).expect("places");
            live.map_anonymous(sparse, 1).expect("backs the first page");
            assert_eq!(
                live.detach_anonymous(sparse, 2, &mut Unpublished, &mut never),
                refused
            );
            assert_eq!(live.space().mapped_pages(), 1);

            let file = live.reserve_file_region(1).expect("reserves");
            live.map_file_page_at(file, &[1]).expect("backs");
            assert_eq!(
                live.detach_anonymous(file, 1, &mut Unpublished, &mut never),
                refused
            );
            assert_eq!(
                live.detach_anonymous(0x7000_0000, 1, &mut Unpublished, &mut never),
                refused,
                "no reservation holds it"
            );
            assert!(never.is_empty(), "nothing moved");
        }

        #[test]
        fn a_caller_placed_range_detaches_and_gives_its_extent_back() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(frames, simmap);
            let base = 0x7000_0000;
            live.reserve_anonymous_at(base, 2).expect("reserves");
            live.map_anonymous(base, 2).expect("backs");

            let mut moved = Vec::new();
            live.detach_anonymous(base, 2, &mut Unpublished, &mut moved)
                .expect("detaches");
            assert_eq!(moved.len(), 2);
            assert_eq!(live.space().mapped_pages(), 0);
            assert_eq!(
                live.reserve_anonymous_at(base, 2),
                Ok(base),
                "the extent is free again"
            );
            for frame in moved {
                frames
                    .free(frame)
                    .expect("the frames are the caller's to free");
            }
        }
    }

    mod ramzip {
        use super::{host_tlb, SharedSim, ANON_WINDOW_BASE};
        use crate::frame::{FrameAllocator, MemoryClass, PAGE_SIZE};
        use crate::live::{LiveSpace, LiveUserSpace};
        use crate::phys::SimPhysMap;
        use crate::ramzip::{PageCandidate, Ramzip, RamzipCaps, RamzipFaultOutcome};
        use crate::retire::Unpublished;
        use crate::seal::{EntropySource, SealError};
        use crate::test_fixture::frame_backing;
        use crate::vmm::{AddressSpace, HostPageTable, VirtAddr};
        use tairix_log::{Event, Sink};
        use tairix_reclaim::{MemoryPressure, PressureBand};
        use tairix_sync::SpinLock;

        /// A larger machine than the shared 64-frame harness, so a test can
        /// press to a band *and* leave the tier room above the reserve.
        const FRAMES: usize = 512;

        struct NullSink;
        impl Sink for NullSink {
            fn write_event(&self, _event: &Event<'_>) {}
        }

        /// Deterministic counting entropy for the tier key/salt.
        struct CountingEntropy(u8);
        impl EntropySource for CountingEntropy {
            fn fill(&mut self, out: &mut [u8]) -> Result<(), SealError> {
                for byte in out.iter_mut() {
                    *byte = self.0;
                    self.0 = self.0.wrapping_add(1);
                }
                Ok(())
            }
        }

        /// A test tier keyed to a large machine (so caps never bind before
        /// the deliberately-exercised gate does).
        fn tier() -> SpinLock<Ramzip> {
            let mut entropy = CountingEntropy(1);
            SpinLock::new(
                Ramzip::new(RamzipCaps::from_physical(8 << 30), &mut entropy).expect("tier"),
            )
        }

        /// A live space over a fresh 512-frame allocator and the direct map
        /// over the same RAM (so `copy_in` observes the same storage). One
        /// cell per expansion, so no two concurrently-running tests share a
        /// pool.
        macro_rules! env {
            () => {{
                let (frames, simmap) = frame_backing!(0, FRAMES);
                build_env(frames, simmap)
            }};
        }

        fn build_env(
            frames: &'static FrameAllocator,
            simmap: &'static SimPhysMap,
        ) -> (
            LiveSpace<HostPageTable, SharedSim>,
            &'static FrameAllocator,
            MemoryPressure,
            &'static SimPhysMap,
        ) {
            let pressure = MemoryPressure::over(frames);
            let live = LiveSpace::new(
                AddressSpace::new(HostPageTable::new()),
                host_tlb(),
                SharedSim(simmap),
                frames,
                VirtAddr::new(0x8000_0000),
                64,
                VirtAddr::new(ANON_WINDOW_BASE),
                256,
                VirtAddr::new(0x1_0000_0000),
                64,
                VirtAddr::new(0x2_0000_0000),
                64,
                VirtAddr::new(0x3_0000_0000),
                64,
                VirtAddr::new(0),
                0,
            )
            .expect("windows valid");
            (live, frames, pressure, simmap)
        }

        /// Hold frames until the sampled band reaches `band`.
        fn press_to(frames: &FrameAllocator, pressure: &MemoryPressure, band: PressureBand) {
            let mut held = alloc::vec::Vec::new();
            let mut guard = 0;
            while pressure.sample() != band {
                held.push(frames.alloc(MemoryClass::UserAnon).expect("pressure frame"));
                guard += 1;
                assert!(guard <= FRAMES, "band {band:?} never reached");
            }
            // The frames stay drawn because nothing frees them: `Frame` is a
            // plain index with no drop glue, so the holding vector's own
            // memory is returned while the band stays where it was pressed.
            drop(held);
        }

        /// The tier entry count, read through the shared stats projection.
        fn entries(tier: &SpinLock<Ramzip>) -> u64 {
            crate::ramzip::stats_of(&tier.lock()).entries
        }

        /// Releasing an anonymous range purges the tier's copies of its
        /// pages: one left behind would be restored, old contents and all,
        /// into the next mapping placed there.
        #[test]
        fn a_released_range_leaves_no_compressed_copy_to_restore() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            let base = live.map_anonymous_placed(4).expect("place anon region");
            let kept = live.map_anonymous_placed(1).expect("a neighbour");
            press_to(frames, &pressure, PressureBand::Moderate);
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                5,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            assert_eq!(summary.compressed, 5);
            live.release_anonymous(base, 4, &mut Unpublished, Some(&tier))
                .expect("released");
            assert_eq!(entries(&tier), 1, "only the neighbour's copy is left");
            assert_eq!(live.reserve_anonymous(4), Ok(base), "placed where it was");
            assert_eq!(
                live.ramzip_fault_in(&tier, base, &sink),
                RamzipFaultOutcome::NoEntry
            );
            assert_eq!(
                live.ramzip_fault_in(&tier, kept, &sink),
                RamzipFaultOutcome::Handled
            );
        }

        #[test]
        fn reclaim_compresses_cold_placed_anonymous_pages_and_faults_them_back() {
            let (mut live, frames, pressure, simmap) = env!();
            let tier = tier();
            let sink = NullSink;

            // Six placed anonymous pages (zeroed, so highly compressible),
            // never accessed since mapping -> cold on the first scan.
            let base = live.map_anonymous_placed(6).expect("place anon region");
            assert_eq!(live.space().mapped_pages(), 6);

            press_to(frames, &pressure, PressureBand::Moderate);
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                6,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            assert_eq!(summary.scanned, 6);
            assert_eq!(summary.compressed, 6, "every cold page compressed");
            assert_eq!(summary.refused, 0);
            assert!(!summary.access_tracking_unsupported);
            assert_eq!(entries(&tier), 6, "six entries parked in the tier");
            assert_eq!(live.space().mapped_pages(), 0, "pages left resident RAM");

            // Fault every page back and confirm the restored bytes are the
            // original zeros (move-only: the entry is gone afterwards).
            for i in 0..6u64 {
                let va = base + i * PAGE_SIZE as u64;
                assert_eq!(
                    live.ramzip_fault_in(&tier, va, &sink),
                    RamzipFaultOutcome::Handled,
                    "page {i} restored"
                );
                let mut buf = [0xAAu8; PAGE_SIZE];
                crate::uaccess::copy_in(
                    live.space(),
                    &SharedSim(simmap),
                    VirtAddr::new(va),
                    &mut buf,
                )
                .expect("restored page is readable");
                assert!(buf.iter().all(|&b| b == 0), "page {i} restored to zeros");
            }
            assert_eq!(entries(&tier), 0, "move-only: tier emptied by fault-ins");
            assert_eq!(
                live.space().mapped_pages(),
                6,
                "all six pages resident again"
            );
        }

        #[test]
        fn reclaim_under_normal_pressure_compresses_nothing() {
            let (mut live, _frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            live.map_anonymous_placed(4).expect("place anon region");
            // No pressure: the handoff gate is closed, so every cold page is
            // refused by policy — nothing is compressed, but the sweep still
            // examined them.
            assert_eq!(pressure.sample(), PressureBand::Normal);
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                4,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            assert_eq!(summary.scanned, 4);
            assert_eq!(summary.compressed, 0);
            assert_eq!(summary.refused, 4);
            assert_eq!(entries(&tier), 0);
            assert_eq!(live.space().mapped_pages(), 4, "pages untouched");
        }

        #[test]
        fn only_placed_anonymous_pages_are_candidates() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            // A FIXED anonymous mapping outside every window is *not* a
            // reclaim candidate; only the placed-window region is.
            live.map_anonymous(0x4000, 2).expect("fixed map");
            live.map_anonymous_placed(3).expect("placed region");

            press_to(frames, &pressure, PressureBand::Moderate);
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                16,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            // Only the three placed pages were ever offered.
            assert_eq!(summary.scanned, 3);
            assert_eq!(summary.compressed, 3);
            // The two FIXED pages are still resident and untouched.
            assert!(live
                .space()
                .translate(crate::vmm::Page::from_addr(VirtAddr::new(0x4000)).unwrap())
                .is_some());
            assert!(live
                .space()
                .translate(crate::vmm::Page::from_addr(VirtAddr::new(0x5000)).unwrap())
                .is_some());
        }

        #[test]
        fn fault_in_with_no_entry_falls_through() {
            let (mut live, _frames, _pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            // An address the tier has no entry for: the resolver falls
            // through to the anonymous handler.
            assert_eq!(
                live.ramzip_fault_in(&tier, ANON_WINDOW_BASE, &sink),
                RamzipFaultOutcome::NoEntry
            );
        }

        #[test]
        fn reclaim_respects_the_want_budget() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            live.map_anonymous_placed(10).expect("place anon region");
            press_to(frames, &pressure, PressureBand::Moderate);
            // Ask for only 3 of the 10 cold pages.
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                3,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            assert_eq!(summary.compressed, 3, "budget honoured");
            assert_eq!(entries(&tier), 3);
            assert_eq!(live.space().mapped_pages(), 7, "seven pages stay resident");
        }

        /// A triggering fault visits a bounded step of the clock, not every
        /// resident page, and the next fault carries on from there: a page
        /// aged on the hand's first pass is reclaimed when it comes back.
        #[test]
        fn reclaim_visits_a_bounded_step_and_resumes_where_it_stopped() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            let pages = 128u64;
            let base = live.map_anonymous_placed(pages).expect("place anon region");
            for index in 0..pages {
                live.space
                    .table_mut_for_test()
                    .mark_accessed(base + index * PAGE_SIZE as u64);
            }
            press_to(frames, &pressure, PressureBand::Moderate);
            let want = 4;
            let step = crate::pressure::ramzip_scan_budget(want);
            let mut reclaim = || {
                live.ramzip_reclaim(
                    &tier,
                    &pressure,
                    0,
                    want,
                    PageCandidate::cold_anonymous(),
                    &sink,
                    &mut Unpublished,
                )
            };
            let revolution = usize::try_from(pages).expect("small").div_ceil(step);
            for sweep in 0..revolution {
                let summary = reclaim();
                assert_eq!(summary.scanned, step, "sweep {sweep} visited one step");
                assert_eq!(summary.compressed, 0, "sweep {sweep}: every page was hot");
            }
            let summary = reclaim();
            assert_eq!(
                summary.compressed, want,
                "aged pages are cold when the hand returns"
            );
            assert_eq!(
                summary.scanned, want,
                "and the first ones it meets are taken"
            );
            assert_eq!(entries(&tier), want as u64);
        }

        #[test]
        fn a_reclaim_and_fault_cycle_leaks_no_frames() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            let base = live.map_anonymous_placed(5).expect("place anon region");

            press_to(frames, &pressure, PressureBand::Moderate);
            let held_free = frames.free_frames();
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                5,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            assert_eq!(summary.compressed, 5);
            // Compression returned five frames to the allocator.
            assert_eq!(frames.free_frames(), held_free + 5);

            for i in 0..5u64 {
                assert_eq!(
                    live.ramzip_fault_in(&tier, base + i * PAGE_SIZE as u64, &sink),
                    RamzipFaultOutcome::Handled
                );
            }
            // Faulting all five back consumed exactly the five frames again:
            // free returns to the held level, and re-mapping restored the
            // pre-pressure resident set (the compressed round trip is frame
            // neutral).
            assert_eq!(frames.free_frames(), held_free);
            assert_eq!(entries(&tier), 0);
        }

        /// Hold frames until the sampled band reaches `band`, returning
        /// them so a test can free them again to relax back to `Normal`.
        /// The shared `press_to` leaks its frames; the clustering and
        /// warm-up paths must compress under pressure and then observe
        /// *comfortable* memory, so their tests need releasable pressure.
        fn press_holding(
            frames: &FrameAllocator,
            pressure: &MemoryPressure,
            band: PressureBand,
        ) -> alloc::vec::Vec<crate::frame::Frame> {
            let mut held = alloc::vec::Vec::new();
            let mut guard = 0;
            while pressure.sample() != band {
                held.push(frames.alloc(MemoryClass::UserAnon).expect("pressure frame"));
                guard += 1;
                assert!(guard <= FRAMES, "band {band:?} never reached");
            }
            held
        }

        /// Return every held pressure frame to the allocator, relaxing
        /// back toward `Normal`.
        fn release(frames: &FrameAllocator, held: alloc::vec::Vec<crate::frame::Frame>) {
            for frame in held {
                frames.free(frame).expect("return held frame");
            }
        }

        #[test]
        fn cluster_restores_contemporaneous_neighbours_after_a_comfortable_fault() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            // Six contiguous placed anonymous pages, all cold: compressed
            // together, so their seal times are contemporaneous.
            let base = live.map_anonymous_placed(6).expect("place anon region");
            let held = press_holding(frames, &pressure, PressureBand::Moderate);
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                6,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            assert_eq!(summary.compressed, 6);
            assert_eq!(entries(&tier), 6);

            // Relax so memory is comfortably free again — the warm gate.
            release(frames, held);
            assert_eq!(pressure.sample(), PressureBand::Normal);

            // Demand-fault the first page (records locality), then cluster
            // its neighbours: the exact production sequence the resolver
            // drives after a restore.
            assert_eq!(
                live.ramzip_fault_in(&tier, base, &sink),
                RamzipFaultOutcome::Handled
            );
            let restored = live.ramzip_cluster(&tier, &pressure, base, &sink);
            assert_eq!(restored, 5, "the five contemporaneous neighbours came back");
            assert_eq!(entries(&tier), 0, "fault + cluster emptied the tier");
            for i in 0..6u64 {
                let page = crate::vmm::Page::from_addr(VirtAddr::new(base + i * PAGE_SIZE as u64))
                    .unwrap();
                assert!(live.space().translate(page).is_some(), "page {i} resident");
            }
        }

        #[test]
        fn cluster_does_nothing_while_under_pressure() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            let base = live.map_anonymous_placed(4).expect("place anon region");
            // Stay under pressure: the held frames are never released.
            let _held = press_holding(frames, &pressure, PressureBand::Moderate);
            let summary = live.ramzip_reclaim(
                &tier,
                &pressure,
                0,
                4,
                PageCandidate::cold_anonymous(),
                &sink,
                &mut Unpublished,
            );
            assert_eq!(summary.compressed, 4);
            assert_eq!(
                live.ramzip_fault_in(&tier, base, &sink),
                RamzipFaultOutcome::Handled
            );
            // The warm gate is closed under pressure: clustering restores
            // nothing, and the other three pages stay compressed.
            assert_eq!(live.ramzip_cluster(&tier, &pressure, base, &sink), 0);
            assert_eq!(entries(&tier), 3);
        }

        #[test]
        fn warm_restores_near_recent_faults_only_with_evidence_and_comfort() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            let base = live.map_anonymous_placed(6).expect("place anon region");
            let held = press_holding(frames, &pressure, PressureBand::Moderate);
            assert_eq!(
                live.ramzip_reclaim(
                    &tier,
                    &pressure,
                    0,
                    6,
                    PageCandidate::cold_anonymous(),
                    &sink,
                    &mut Unpublished
                )
                .compressed,
                6
            );
            release(frames, held);
            assert_eq!(pressure.sample(), PressureBand::Normal);

            // No demand fault yet: warm-up has no locality evidence, so it
            // keeps every cold page compressed by design.
            assert_eq!(live.ramzip_warm(&tier, &pressure, &sink), 0);
            assert_eq!(entries(&tier), 6, "nothing warmed without evidence");

            // A demand fault provides evidence; the warm step brings the
            // neighbours back (batch budget covers all five).
            assert_eq!(
                live.ramzip_fault_in(&tier, base, &sink),
                RamzipFaultOutcome::Handled
            );
            let restored = live.ramzip_warm(&tier, &pressure, &sink);
            assert_eq!(restored, 5, "the five neighbours warmed back");
            assert_eq!(entries(&tier), 0);
        }

        #[test]
        fn warm_stops_immediately_under_pressure() {
            let (mut live, frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            let base = live.map_anonymous_placed(4).expect("place anon region");
            let _held = press_holding(frames, &pressure, PressureBand::Moderate);
            assert_eq!(
                live.ramzip_reclaim(
                    &tier,
                    &pressure,
                    0,
                    4,
                    PageCandidate::cold_anonymous(),
                    &sink,
                    &mut Unpublished
                )
                .compressed,
                4
            );
            assert_eq!(
                live.ramzip_fault_in(&tier, base, &sink),
                RamzipFaultOutcome::Handled
            );
            // Still under pressure: the warm gate is closed, so the step
            // stops immediately and the three others stay compressed.
            assert_eq!(live.ramzip_warm(&tier, &pressure, &sink), 0);
            assert_eq!(entries(&tier), 3);
        }

        #[test]
        fn cluster_and_warm_are_no_ops_on_an_empty_tier() {
            let (mut live, _frames, pressure, _simmap) = env!();
            let tier = tier();
            let sink = NullSink;
            // No entries and no faults: both restore nothing (fail closed)
            // and never panic — the resolver may call them after any fault.
            assert_eq!(
                live.ramzip_cluster(&tier, &pressure, ANON_WINDOW_BASE, &sink),
                0
            );
            assert_eq!(live.ramzip_warm(&tier, &pressure, &sink), 0);
            assert_eq!(entries(&tier), 0);
        }
    }

    /// Every release path hands a frame back only once no other CPU and no
    /// snapshot can still reach it.
    mod views {
        use super::{custodian, fill_phys, phys_is_zero, SharedSim};
        use super::{
            ANON_WINDOW_BASE, ANON_WINDOW_PAGES, CODE_WINDOW_BASE, CODE_WINDOW_PAGES,
            DMA_WINDOW_BASE, DMA_WINDOW_PAGES, FILE_WINDOW_BASE, FILE_WINDOW_PAGES,
            MMIO_WINDOW_BASE, MMIO_WINDOW_PAGES, SHARED_WINDOW_BASE, SHARED_WINDOW_PAGES, SIM_BASE,
            SIM_PAGES,
        };
        use crate::frame::{Frame, FrameAllocator, MemoryClass, PhysAddr, PAGE_SIZE};
        use crate::live::{LiveSpace, LiveUserSpace};
        use crate::phys::SimPhysMap;
        use crate::retire::{RecordedRemote, Retire, SpaceTlb, TlbContext, Unpublished};
        use crate::test_fixture::{custody, frame_backing};
        use crate::vmm::{AddressSpace, HostPageTable, MapFlags, Page, PageTable, VirtAddr};
        use tairix_arch_api::mmu::{
            AccessTracking, AddressSpace as HalAddressSpace, MapError, PageFlags,
        };
        use tairix_arch_api::tlb::TlbShootdown;

        extern crate std;
        use std::cell::{Cell, RefCell};
        use std::vec;
        use std::vec::Vec;

        /// What reached a view, in order.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Event {
            Remote(u64, usize),
            Retired(u64, u64),
            Restored(u64),
        }

        std::thread_local! {
            static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
            static LOCAL_FLUSHES: Cell<usize> = const { Cell::new(0) };
            static WALKS: Cell<usize> = const { Cell::new(0) };
        }

        fn events() -> Vec<Event> {
            EVENTS.with(|events| core::mem::take(&mut *events.borrow_mut()))
        }

        fn record_remote(start: u64, pages: usize) {
            EVENTS.with(|events| events.borrow_mut().push(Event::Remote(start, pages)));
        }

        /// The other CPUs' reach.
        static REMOTE: RecordedRemote = RecordedRemote(record_remote);

        /// A CPU other than the one each test runs on.
        const OTHER_CPU: u32 = 1;

        /// A reach whose space runs on another CPU too.
        fn shared_reach() -> SpaceTlb {
            let cpus = TlbContext::new(2).expect("one word allocates");
            cpus.enter(OTHER_CPU);
            SpaceTlb::new(cpus, Some(&REMOTE))
        }

        /// A snapshot that checks, the moment a page leaves it, that nothing
        /// that page mapped has been released yet.
        struct Snapshot<'a> {
            frames: &'static FrameAllocator,
            free_before: usize,
            still_held: &'a dyn Fn() -> bool,
        }

        impl Retire for Snapshot<'_> {
            fn retire(&mut self, base: u64, pages: u64) {
                assert_eq!(
                    self.frames.free_frames(),
                    self.free_before,
                    "no frame went back before the snapshot let go"
                );
                assert!((self.still_held)(), "released before the snapshot let go");
                EVENTS.with(|events| events.borrow_mut().push(Event::Retired(base, pages)));
            }
            fn restore(&mut self, page: Page, _frame: Frame, _flags: MapFlags) {
                EVENTS.with(|events| {
                    events
                        .borrow_mut()
                        .push(Event::Restored(page.start().as_u64()));
                });
            }
        }

        #[test]
        fn an_unmapped_anonymous_frame_is_freed_only_after_every_view_lets_go() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(frames, simmap, shared_reach());
            let base = 0x7000_0000;
            live.reserve_anonymous_at(base, 3).expect("reserves");
            live.map_anonymous(base, 3).expect("maps");
            fill_phys(simmap, PhysAddr::new(SIM_BASE), SIM_PAGES * PAGE_SIZE, 0x5A);
            let _ = events();

            let free_before = frames.free_frames();
            let mut snapshot = Snapshot {
                frames,
                free_before,
                still_held: &|| true,
            };
            live.unmap_anonymous(base, 3, &mut snapshot)
                .expect("unmaps");
            assert_eq!(
                events(),
                [Event::Remote(base, 3), Event::Retired(base, 3)],
                "the other CPU, then the snapshot, before any frame"
            );
            assert_eq!(frames.free_frames(), free_before + 3);
        }

        #[test]
        fn a_dma_buffer_is_scrubbed_and_freed_only_after_every_view_lets_go() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(frames, simmap, shared_reach());
            let mapping = live
                .alloc_dma(PAGE_SIZE, 0, custodian(custody!()))
                .expect("a free block exists");
            fill_phys(simmap, PhysAddr::new(mapping.device_addr), PAGE_SIZE, 0xC3);
            let _ = events();

            let phys = PhysAddr::new(mapping.device_addr);
            let dirty = || !phys_is_zero(simmap, phys, PAGE_SIZE);
            let mut snapshot = Snapshot {
                frames,
                free_before: frames.free_frames(),
                still_held: &dirty,
            };
            live.free_dma(mapping.cpu_va, &mut snapshot).expect("frees");
            assert_eq!(
                events(),
                [
                    Event::Remote(mapping.cpu_va, 1),
                    Event::Retired(mapping.cpu_va, 1)
                ]
            );
            assert!(
                phys_is_zero(simmap, phys, PAGE_SIZE),
                "scrubbed on the way out"
            );
        }

        #[test]
        fn a_file_region_frame_is_freed_only_after_every_view_lets_go() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(frames, simmap, shared_reach());
            let base = live.reserve_file_region(4).expect("reserves");
            live.map_file_page_at(base + PAGE_SIZE as u64, &[7; 16])
                .expect("backs one page");
            let _ = events();

            let free_before = frames.free_frames();
            let mut snapshot = Snapshot {
                frames,
                free_before,
                still_held: &|| true,
            };
            assert_eq!(live.release_file_region(base, 4, &mut snapshot), Ok(1));
            let page = base + PAGE_SIZE as u64;
            assert_eq!(events(), [Event::Remote(page, 1), Event::Retired(page, 1)]);
            assert_eq!(frames.free_frames(), free_before + 1);
        }

        #[test]
        fn a_detached_frame_moves_on_only_after_every_view_lets_go() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(frames, simmap, shared_reach());
            let base = live.map_anonymous_placed(2).expect("places");
            fill_phys(simmap, PhysAddr::new(SIM_BASE), SIM_PAGES * PAGE_SIZE, 0x5A);
            let _ = events();

            let mut moved = Vec::new();
            let mut snapshot = Snapshot {
                frames,
                free_before: frames.free_frames(),
                still_held: &|| true,
            };
            live.detach_anonymous(base, 2, &mut snapshot, &mut moved)
                .expect("detaches");
            assert_eq!(events(), [Event::Remote(base, 2), Event::Retired(base, 2)]);
            assert_eq!(moved.len(), 2);
            assert!(
                moved
                    .iter()
                    .all(|frame| !phys_is_zero(simmap, frame.start(), PAGE_SIZE)),
                "each frame moves on with its bytes"
            );
        }

        /// A frame holding code, as a compiler leaves one.
        fn code_frame(frames: &FrameAllocator, simmap: &SimPhysMap) -> Frame {
            let frame = frames
                .alloc(MemoryClass::UserAnon)
                .expect("a frame is free");
            fill_phys(simmap, frame.start(), PAGE_SIZE, 0xC3);
            frame
        }

        #[test]
        fn released_code_is_scrubbed_and_freed_only_after_every_view_lets_go() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = shared_live_space!(frames, simmap, shared_reach());
            let frame = code_frame(frames, simmap);
            let base = live.reserve_code(1).expect("places");
            live.map_code(base, &[frame]).expect("installs");
            let _ = events();

            let dirty = || !phys_is_zero(simmap, frame.start(), PAGE_SIZE);
            let mut snapshot = Snapshot {
                frames,
                free_before: frames.free_frames(),
                still_held: &dirty,
            };
            live.release_code(base, 1, &mut snapshot).expect("releases");
            assert_eq!(events(), [Event::Remote(base, 1), Event::Retired(base, 1)]);
            assert!(!dirty(), "scrubbed on the way out");
        }

        #[test]
        fn installed_code_is_published_to_the_other_cpus_of_a_port_that_caches_an_absence() {
            static CACHING: RecordedRemote<true> = RecordedRemote(record_remote);
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let cpus = TlbContext::new(2).expect("one word allocates");
            cpus.enter(OTHER_CPU);
            let mut caching =
                shared_live_space!(frames, simmap, SpaceTlb::new(cpus, Some(&CACHING)));
            let base = caching.reserve_code(1).expect("places");
            caching
                .map_code(base, &[code_frame(frames, simmap)])
                .expect("installs");
            assert_eq!(events(), [Event::Remote(base, 1)]);

            let mut plain = shared_live_space!(frames, simmap, shared_reach());
            let base = plain.reserve_code(1).expect("places");
            plain
                .map_code(base, &[code_frame(frames, simmap)])
                .expect("installs");
            assert!(events().is_empty(), "an absence is never cached there");
        }

        #[test]
        fn a_map_undone_part_way_reaches_the_other_cpu_before_its_frames_go_back() {
            // Fewer frames than the map asks for: it maps some, then unwinds.
            let (frames, simmap) = frame_backing!(SIM_BASE, 8);
            let mut live = shared_live_space!(frames, simmap, shared_reach());
            let free_before = frames.free_frames();
            assert!(live.map_anonymous_placed(64).is_err());
            let unwound = events();
            assert_eq!(unwound.len(), 1, "one shootdown for the unwound run");
            assert!(matches!(unwound[0], Event::Remote(base, _) if base >= ANON_WINDOW_BASE));
            assert_eq!(frames.free_frames(), free_before, "every frame came back");
        }

        /// A page table counting every flush its space issues, even past the
        /// space's drop.
        struct CountingTable(HostPageTable);

        impl HalAddressSpace for CountingTable {
            fn map_page(
                &mut self,
                vaddr: u64,
                paddr: u64,
                flags: PageFlags,
            ) -> Result<(), MapError> {
                self.0.map_page(vaddr, paddr, flags)
            }
            fn translate(&self, vaddr: u64) -> Option<(u64, PageFlags)> {
                WALKS.with(|walks| walks.set(walks.get() + 1));
                self.0.translate(vaddr)
            }
            fn unmap(&mut self, vaddr: u64) -> Result<u64, MapError> {
                self.0.unmap(vaddr)
            }
            fn root_phys(&self) -> u64 {
                self.0.root_phys()
            }
            fn access_tracking(&self) -> AccessTracking {
                self.0.access_tracking()
            }
            unsafe fn activate(&self) {}
        }

        impl TlbShootdown for CountingTable {
            fn flush_page(&mut self, _vaddr: u64) {
                LOCAL_FLUSHES.with(|flushes| flushes.set(flushes.get() + 1));
            }
        }

        /// A live space over `table`, `file_pages` of file window long.
        fn space_over<T: PageTable>(
            table: T,
            frames: &'static FrameAllocator,
            simmap: &'static SimPhysMap,
            tlb: SpaceTlb,
            file_pages: usize,
        ) -> LiveSpace<T, SharedSim> {
            LiveSpace::new(
                AddressSpace::new(table),
                tlb,
                SharedSim(simmap),
                frames,
                VirtAddr::new(MMIO_WINDOW_BASE),
                MMIO_WINDOW_PAGES,
                VirtAddr::new(ANON_WINDOW_BASE),
                ANON_WINDOW_PAGES,
                VirtAddr::new(DMA_WINDOW_BASE),
                DMA_WINDOW_PAGES,
                VirtAddr::new(SHARED_WINDOW_BASE),
                SHARED_WINDOW_PAGES,
                VirtAddr::new(FILE_WINDOW_BASE),
                file_pages,
                VirtAddr::new(CODE_WINDOW_BASE),
                CODE_WINDOW_PAGES,
            )
            .expect("windows are valid")
        }

        fn counting_space(
            frames: &'static FrameAllocator,
            simmap: &'static SimPhysMap,
            tlb: SpaceTlb,
            file_pages: usize,
        ) -> LiveSpace<CountingTable, SharedSim> {
            space_over(
                CountingTable(HostPageTable::new()),
                frames,
                simmap,
                tlb,
                file_pages,
            )
        }

        /// A page table that refuses to unmap the page at `refused`, as only
        /// a defective backend would.
        struct RefusingTable {
            table: HostPageTable,
            refused: u64,
        }

        impl HalAddressSpace for RefusingTable {
            fn map_page(
                &mut self,
                vaddr: u64,
                paddr: u64,
                flags: PageFlags,
            ) -> Result<(), MapError> {
                self.table.map_page(vaddr, paddr, flags)
            }
            fn translate(&self, vaddr: u64) -> Option<(u64, PageFlags)> {
                self.table.translate(vaddr)
            }
            fn unmap(&mut self, vaddr: u64) -> Result<u64, MapError> {
                if vaddr == self.refused {
                    return Err(MapError::Unsupported);
                }
                self.table.unmap(vaddr)
            }
            fn root_phys(&self) -> u64 {
                self.table.root_phys()
            }
            fn access_tracking(&self) -> AccessTracking {
                self.table.access_tracking()
            }
            unsafe fn activate(&self) {}
        }

        impl TlbShootdown for RefusingTable {
            fn flush_page(&mut self, vaddr: u64) {
                self.table.flush_page(vaddr);
            }
        }

        #[test]
        fn a_detach_refused_part_way_scrubs_and_frees_what_it_took_and_hands_over_nothing() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let base = ANON_WINDOW_BASE;
            let table = RefusingTable {
                table: HostPageTable::new(),
                refused: base + PAGE_SIZE as u64,
            };
            let mut live = space_over(table, frames, simmap, shared_reach(), FILE_WINDOW_PAGES);
            assert_eq!(live.map_anonymous_placed(2), Ok(base));
            fill_phys(simmap, PhysAddr::new(SIM_BASE), SIM_PAGES * PAGE_SIZE, 0x5A);
            let first = live
                .translate_page(Page::from_addr(VirtAddr::new(base)).expect("aligned"))
                .expect("mapped")
                .0;
            let free = frames.free_frames();

            let mut moved = vec![Frame(0)];
            assert!(live
                .detach_anonymous(base, 2, &mut Unpublished, &mut moved)
                .is_err());
            assert_eq!(moved, [Frame(0)], "what the caller held is all it holds");
            assert_eq!(
                frames.free_frames(),
                free + 1,
                "the page that left went back"
            );
            assert!(phys_is_zero(simmap, first.start(), PAGE_SIZE), "scrubbed");
        }

        #[test]
        fn a_fault_on_a_page_mapped_to_allow_it_is_spurious_and_discards_this_cpus_view() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = counting_space(frames, simmap, shared_reach(), FILE_WINDOW_PAGES);
            let data = live.map_anonymous_placed(1).expect("places");
            let code = live.reserve_code(1).expect("places");
            live.map_code(code, &[code_frame(frames, simmap)])
                .expect("installs");
            let flushes = LOCAL_FLUSHES.with(Cell::get);

            assert!(live.settle_spurious(data + 8, false), "a read of data");
            assert!(live.settle_spurious(data + 8, true), "a write to data");
            assert!(live.settle_spurious(code, false), "a read of code");
            assert_eq!(
                LOCAL_FLUSHES.with(Cell::get),
                flushes + 3,
                "this CPU's view is discarded each time"
            );

            assert!(
                !live.settle_spurious(code, true),
                "a write to code is no slip"
            );
            assert!(
                !live.settle_spurious(data + PAGE_SIZE as u64, false),
                "nor an access nothing is mapped for"
            );
            assert_eq!(LOCAL_FLUSHES.with(Cell::get), flushes + 3);
        }

        #[test]
        fn a_sparse_release_visits_and_retires_only_its_resident_pages() {
            // Four GiB of file window, two pages of it resident.
            const SPAN: usize = 1 << 20;
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = counting_space(frames, simmap, shared_reach(), SPAN);
            let base = live.reserve_file_region(SPAN as u64).expect("reserves");
            let last = base + (SPAN as u64 - 1) * PAGE_SIZE as u64;
            live.map_file_page_at(base, &[1])
                .expect("backs the first page");
            live.map_file_page_at(last, &[2])
                .expect("backs the last page");
            let _ = events();

            let walks = WALKS.with(Cell::get);
            let mut snapshot = Snapshot {
                frames,
                free_before: frames.free_frames(),
                still_held: &|| true,
            };
            assert_eq!(
                live.release_file_region(base, SPAN as u64, &mut snapshot),
                Ok(2)
            );
            assert_eq!(WALKS.with(Cell::get), walks, "no page of the gap is walked");
            assert_eq!(
                events(),
                [
                    Event::Remote(base, SPAN),
                    Event::Retired(base, 1),
                    Event::Retired(last, 1),
                ],
                "one shootdown spans the batch, and the snapshot loses only what was resident"
            );
        }

        #[test]
        fn a_fault_on_a_page_another_cpu_mapped_discards_the_stale_entry_here() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let mut live = counting_space(frames, simmap, shared_reach(), FILE_WINDOW_PAGES);
            let base = 0x7000_0000;
            live.reserve_anonymous_at(base, 1).expect("reserves");
            live.map_anonymous(base, 1)
                .expect("the first fault backs it");

            let (flushes, free, committed) = (
                LOCAL_FLUSHES.with(Cell::get),
                frames.free_frames(),
                frames.committed_frames(),
            );
            assert!(
                live.map_anonymous(base, 1).is_err(),
                "the second fault lost the race"
            );
            assert_eq!(
                LOCAL_FLUSHES.with(Cell::get),
                flushes + 1,
                "this CPU's cached absence is discarded"
            );
            assert_eq!(frames.free_frames(), free, "the loser draws no frame");
            assert_eq!(
                frames.committed_frames(),
                committed,
                "nor moves the commitment"
            );

            let file = live.reserve_file_region(1).expect("reserves");
            live.map_file_page_at(file, &[1])
                .expect("the first fault backs it");
            let flushes = LOCAL_FLUSHES.with(Cell::get);
            assert!(live.map_file_page_at(file, &[1]).is_err());
            assert_eq!(LOCAL_FLUSHES.with(Cell::get), flushes + 1);
        }

        #[test]
        fn a_space_active_nowhere_is_torn_down_without_a_single_flush() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let free_before = frames.free_frames();
            let live = {
                let cpus = TlbContext::new(2).expect("one word allocates");
                let mut live = counting_space(
                    frames,
                    simmap,
                    SpaceTlb::new(cpus, Some(&REMOTE)),
                    FILE_WINDOW_PAGES,
                );
                let base = 0x7000_0000;
                live.reserve_anonymous_at(base, 16).expect("reserves");
                live.map_anonymous(base, 16).expect("maps");
                live
            };
            let _ = events();
            let flushes = LOCAL_FLUSHES.with(Cell::get);
            drop(live);
            assert_eq!(LOCAL_FLUSHES.with(Cell::get), flushes, "no local flush");
            assert!(events().is_empty(), "no other CPU reached");
            assert_eq!(frames.free_frames(), free_before, "every frame came back");
        }

        #[test]
        fn a_space_still_active_elsewhere_is_shot_down_before_its_frames_go_back() {
            let (frames, simmap) = frame_backing!(SIM_BASE, SIM_PAGES);
            let base = 0x7000_0000;
            let mut live = counting_space(frames, simmap, shared_reach(), FILE_WINDOW_PAGES);
            live.reserve_anonymous_at(base, 4).expect("reserves");
            live.map_anonymous(base, 4).expect("maps");
            let _ = events();
            let flushes = LOCAL_FLUSHES.with(Cell::get);
            drop(live);
            assert_eq!(
                LOCAL_FLUSHES.with(Cell::get),
                flushes + 4,
                "each page flushed here"
            );
            assert_eq!(events(), [Event::Remote(base, 4)], "and on the other CPU");
        }
    }
}
