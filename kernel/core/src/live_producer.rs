//! Production `mem_map` / `mmio_map` producers over the calling process's
//! live address space (`plans/PI.md` P10 chunk 5d-0-ii (b′)).
//!
//! [`crate::memmap::MemMap`] and [`crate::devres::MmioMapFacility`] are the
//! object-safe seams the `mem_map` and `mmio_map` syscall handlers reach.
//! Before this module the only implementations were the fail-closed
//! `NULL_*` defaults, because no live, mutable address space was retained at
//! all (the spawn path froze the space into a read-only snapshot and dropped
//! the live one). Every call now routes through the kthread runtime
//! ([`crate::kthread::with_current_live_space`]) to the **caller's own**
//! space: a syscall handler runs on the CPU servicing the trap, on which the
//! calling thread is the one switched in, so its process's space is exactly
//! the per-CPU slot for [`SchedulerArch::current_cpu`], and
//! [`crate::ProcessSpace`]'s own lock serialises a sibling thread mutating
//! it from another core.
//!
//! Both producers are generic over the arch (`A: SchedulerArch`) and hold a
//! `&'static A`, mirroring [`crate::procwait::KernelProcessWait`], so
//! `kernel/core` reads the current CPU without naming a concrete port. A call
//! on a CPU with no published space (a task spawned without one) fails closed
//! with [`Errno::NotImplemented`] rather than touching another task's memory.

use alloc::vec::Vec;

use tairix_abi::{Errno, MapFlags};
use tairix_kernel_mem::{
    page_count_for, AllocError, AnonError, DmaCustodian, DmaError, Frame, FrameAllocator,
    FrameBlock, LiveSpaceError, MemoryClass, MmioError, PageTableError, PhysAddr, PhysMap, Retire,
    SharedMemory, MAX_ORDER, PAGE_SIZE,
};
use tairix_kernel_sched_api::SchedulerArch;

use crate::devres::{
    DmaAllocFacility, DmaBacking, DmaCarve, MmioMapFacility, MmioMemoryKind, SharedChunk,
    SharedMemFacility,
};
use crate::filemap::FileMap;
use crate::kthread::with_current_live_space;
use crate::memmap::MemMap;

/// Fold an [`AnonError`] onto a stable [`Errno`]:
/// allocator exhaustion is [`Errno::OutOfMemory`], a not-mapped range
/// is [`Errno::NotFound`] (fail closed), and a misalignment/overflow
/// is [`Errno::OutOfRange`].
fn anon_errno(err: AnonError) -> Errno {
    match err {
        AnonError::ZeroLength => Errno::LengthOutOfRange,
        AnonError::Unaligned | AnonError::Overflow => Errno::OutOfRange,
        AnonError::OutOfMemory => Errno::OutOfMemory,
        AnonError::NotMapped => Errno::NotFound,
        // `PhysUnmapped`, `Map(_)`, and any future (`#[non_exhaustive]`)
        // variant fold to the generic bad-address error, failing closed
        // rather than being silently dropped.
        _ => Errno::BadAddress,
    }
}

/// Fold an [`MmioError`] onto a stable [`Errno`]: no free virtual slot is
/// [`Errno::OutOfMemory`] (deterministic exhaustion), a malformed
/// region or mapper config is [`Errno::OutOfRange`], and a page-table or
/// direct-map failure is [`Errno::BadAddress`].
fn mmio_errno(err: MmioError) -> Errno {
    match err {
        MmioError::NoVirtualSpace => Errno::OutOfMemory,
        MmioError::InvalidRegion | MmioError::InvalidMapConfig => Errno::OutOfRange,
        MmioError::UnknownRegion => Errno::NotFound,
        // A port that cannot keep memory a non-snooping master shares out of
        // the caches.
        MmioError::PageTable(PageTableError::Unsupported) => Errno::NotSupported,
        // `PageTable`, `DirectMap`, and any future (`#[non_exhaustive]`)
        // kind fail closed to a generic bad-address error.
        _ => Errno::BadAddress,
    }
}

/// Fold a [`DmaError`] onto a stable [`Errno`]: the frame allocator's own
/// refusal folds as every allocation's does ([`AllocError::as_errno`]:
/// exhaustion is [`Errno::OutOfMemory`], an addressing limit no RAM lies
/// below [`Errno::OutOfRange`]); a request beyond the max buddy order is
/// [`Errno::OutOfRange`]; a zero-length request is
/// [`Errno::LengthOutOfRange`]; a port unable to map memory for a device that
/// does not snoop is [`Errno::NotSupported`]; and a not-reachable frame or
/// other page-table refusal is [`Errno::BadAddress`] (fail closed).
pub(crate) fn dma_errno(err: DmaError) -> Errno {
    match err {
        DmaError::Alloc(alloc) => alloc.as_errno(),
        DmaError::ZeroSize => Errno::LengthOutOfRange,
        DmaError::SizeUnsupported => Errno::OutOfRange,
        // No custody behind the carve is an inert quarantine, not a caller
        // error.
        DmaError::NoCustody => Errno::NotImplemented,
        DmaError::DeviceGone => Errno::DeviceOffline,
        DmaError::CustodianMismatch => Errno::PermissionDenied,
        DmaError::KernelOwned | DmaError::GroupBusy => Errno::Busy,
        DmaError::Translation | DmaError::Unconfirmed => Errno::DeviceFault,
        DmaError::PageTable(PageTableError::Unsupported) => Errno::NotSupported,
        // `PageTable`, `DirectMap`, `UnknownBuffer`, `InvalidPoolConfig`, and
        // any future (`#[non_exhaustive]`) variant fail closed to a generic
        // bad-address error.
        _ => Errno::BadAddress,
    }
}

/// Fold a [`LiveSpaceError`] onto a stable [`Errno`].
pub(crate) fn live_errno(err: LiveSpaceError) -> Errno {
    match err {
        LiveSpaceError::Anon(anon) => anon_errno(anon),
        LiveSpaceError::Mmio(mmio) => mmio_errno(mmio),
        LiveSpaceError::Dma(dma) => dma_errno(dma),
        // `LiveSpaceError` is `#[non_exhaustive]`; fail closed.
        _ => Errno::BadAddress,
    }
}

/// The production anonymous-memory producer: maps/unmaps `RW` anonymous
/// pages in the **calling task's own** live address space (`plans/SPAWN.md`
/// `SP5b` production form).
pub struct LiveMemMap<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    arch: &'static A,
}

impl<A> LiveMemMap<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    /// Build the producer over the `'static` arch handle the CPU id is read
    /// from (the boot-leaked `KernelState` arch, exactly as
    /// [`crate::procwait::KernelProcessWait`]).
    #[must_use]
    pub const fn new(arch: &'static A) -> Self {
        Self { arch }
    }
}

impl<A> MemMap for LiveMemMap<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    fn reserve(&self, len: usize, flags: MapFlags, addr_hint: u64) -> Result<u64, Errno> {
        let page_count = page_count_for(len).map_err(anon_errno)?;
        let cpu = self.arch.current_cpu();
        // Reserve address space only — no frame, no page-table entry. The
        // pages fault in one at a time (`map` below, from the anonymous
        // fault path), so a large `mem_map` never zeroes and commits
        // thousands of pages in one non-preemptible syscall. `FIXED` names
        // its own base; a non-`FIXED` request draws a base from this task's
        // own heap window (never a base guessed here that might collide with
        // the image, stack, or a granted device window).
        if flags.is_fixed() {
            with_current_live_space(cpu, |space| {
                space.reserve_anonymous_at(addr_hint, page_count)
            })
        } else {
            with_current_live_space(cpu, |space| space.reserve_anonymous(page_count))
        }
        .ok_or(Errno::NotImplemented)?
        .map_err(live_errno)
    }

    fn commit(&self, pages: u64) -> Result<(), Errno> {
        let cpu = self.arch.current_cpu();
        // Reserve physical headroom for `pages` demand-paged pages whose
        // address space already exists (stack growth): commitment only, no
        // placement. Fails closed as a `Result` when the no-overcommit
        // budget cannot admit the growth, so the stack-fault path refuses
        // rather than killing the task on first touch.
        with_current_live_space(cpu, |space| space.commit_anonymous(pages))
            .ok_or(Errno::NotImplemented)?
            .map_err(live_errno)
    }

    fn map(&self, len: usize, flags: MapFlags, addr_hint: u64) -> Result<u64, Errno> {
        let page_count = page_count_for(len).map_err(anon_errno)?;
        let cpu = self.arch.current_cpu();
        // The single-page commit the anonymous and stack fault paths use to
        // back one reserved page with a fresh zeroed `RW|USER` frame. Always
        // `FIXED` (the faulting page's own base); a non-`FIXED` request asks
        // the live space's per-task heap-window allocator to choose a base
        // out of this task's own free user-VA.
        if flags.is_fixed() {
            with_current_live_space(cpu, |space| space.map_anonymous(addr_hint, page_count))
        } else {
            with_current_live_space(cpu, |space| space.map_anonymous_placed(page_count))
        }
        .ok_or(Errno::NotImplemented)?
        .map_err(live_errno)
    }

    fn unmap(&self, base: u64, len: usize, retire: &mut dyn Retire) -> Result<(), Errno> {
        let page_count = page_count_for(len).map_err(anon_errno)?;
        let cpu = self.arch.current_cpu();
        with_current_live_space(cpu, |space| space.unmap_anonymous(base, page_count, retire))
            .ok_or(Errno::NotImplemented)?
            .map_err(live_errno)
    }
}

/// The whole-page count a `len`-byte file mapping spans, rounded up.
///
/// The file-mapping length is 64-bit end to end (a mappable file may
/// exceed both `usize` and any 32-bit figure), so this is the `u64` form
/// of [`page_count_for`]; a zero length names nothing and fails closed.
fn file_page_count(len: u64) -> Result<u64, Errno> {
    if len == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(len.div_ceil(PAGE_SIZE as u64))
}

impl<A> FileMap for LiveMemMap<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    fn reserve(&self, len: u64) -> Result<u64, Errno> {
        let page_count = file_page_count(len)?;
        let cpu = self.arch.current_cpu();
        // Pure address-space reservation out of this task's own
        // file-mapping window; no frame moves until a fault lands.
        with_current_live_space(cpu, |space| space.reserve_file_region(page_count))
            .ok_or(Errno::NotImplemented)?
            .map_err(live_errno)
    }

    fn map_page(&self, va: u64, contents: &[u8]) -> Result<(), Errno> {
        let cpu = self.arch.current_cpu();
        // The live space refuses an address outside every reserved file
        // region (`NotFound` after folding), so the fault path can never
        // materialise memory the task did not map.
        with_current_live_space(cpu, |space| space.map_file_page_at(va, contents))
            .ok_or(Errno::NotImplemented)?
            .map_err(live_errno)
    }

    fn release(&self, base: u64, len: u64, retire: &mut dyn Retire) -> Result<u64, Errno> {
        let page_count = file_page_count(len)?;
        let cpu = self.arch.current_cpu();
        with_current_live_space(cpu, |space| {
            space.release_file_region(base, page_count, retire)
        })
        .ok_or(Errno::NotImplemented)?
        .map_err(live_errno)
    }
}

/// The production MMIO-map facility: maps a validated, **granted** device
/// window into the calling driver task's own live address space
/// (`plans/PI.md` P10 chunk 5d-0). The handler has already resolved and
/// owner-checked the grant; this performs only
/// the page-table mechanism, guard-bracketed and caching-disabled.
pub struct LiveMmioMap<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    arch: &'static A,
}

impl<A> LiveMmioMap<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    /// Build the producer over the `'static` arch handle.
    #[must_use]
    pub const fn new(arch: &'static A) -> Self {
        Self { arch }
    }
}

impl<A> MmioMapFacility for LiveMmioMap<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    fn map_window(&self, phys_base: u64, len: usize, kind: MmioMemoryKind) -> Result<u64, Errno> {
        let cpu = self.arch.current_cpu();
        with_current_live_space(cpu, |space| match kind {
            MmioMemoryKind::Device => space.map_device_window(phys_base, len),
            MmioMemoryKind::FramebufferWriteBack => {
                space.map_writeback_framebuffer_window(phys_base, len)
            }
            MmioMemoryKind::FramebufferWriteCombine => space.map_framebuffer_window(phys_base, len),
        })
        .ok_or(Errno::NotImplemented)?
        .map_err(live_errno)
    }
}

/// The production DMA-alloc facility: carves a coherent, guard-bracketed DMA
/// buffer into the calling driver task's own live address space
/// (`plans/PI.md` P10 chunk 5d-0). The handler has already resolved and
/// owner-checked the grant and validated its DMA constraint; this performs only the carve mechanism, bounded by the
/// grant's `addr_limit`.
pub struct LiveDmaAlloc<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    arch: &'static A,
}

impl<A> LiveDmaAlloc<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    /// Build the producer over the `'static` arch handle.
    #[must_use]
    pub const fn new(arch: &'static A) -> Self {
        Self { arch }
    }
}

impl<A> DmaAllocFacility for LiveDmaAlloc<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    fn alloc(
        &self,
        len: usize,
        addr_limit: u64,
        custodian: DmaCustodian,
    ) -> Result<DmaCarve, Errno> {
        let cpu = self.arch.current_cpu();
        with_current_live_space(cpu, |space| space.alloc_dma(len, addr_limit, custodian))
            .ok_or(Errno::NotImplemented)?
            .map(|mapping| DmaCarve {
                cpu_va: mapping.cpu_va,
                device_addr: mapping.device_addr,
                len: mapping.len as u64,
            })
            .map_err(live_errno)
    }

    fn free(&self, cpu_va: u64, retire: &mut dyn Retire) -> Result<usize, Errno> {
        let cpu = self.arch.current_cpu();
        with_current_live_space(cpu, |space| space.free_dma(cpu_va, retire))
            .ok_or(Errno::NotImplemented)?
            .map_err(live_errno)
    }
}

/// The production shared-memory facility: allocates, zeroes, maps, and frees
/// cross-process shared-memory regions over the kernel frame allocator and
/// the calling task's own live address space (`plans/USB.md`).
///
/// `arch` is read for the current CPU (the slot the calling task's live
/// space the *mapping* lands in is published on, exactly like
/// [`LiveMmioMap`]); `frames` is the kernel allocator the region's
/// physically-contiguous backing is drawn from and returned to; `physmap` is
/// the kernel direct map the region's frames are scrubbed through on
/// allocation and on free. Scrubbing through the direct map (not a user
/// mapping) is what makes the last-reference free's zero-on-free hold even
/// when the task whose teardown drops it is a kernel thread with no live
/// address space (a hot-removed driver torn down by the device manager).
pub struct LiveSharedMem<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    arch: &'static A,
    frames: &'static FrameAllocator,
    physmap: &'static (dyn PhysMap + Sync),
}

impl<A> LiveSharedMem<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    /// Build the producer over the `'static` arch handle, the kernel frame
    /// allocator, and the kernel direct physical map.
    ///
    /// `physmap` is the kernel-privileged view of all RAM (identity on
    /// aarch64 / riscv64, higher-half on x86_64); the facility scrubs a
    /// region's frames through it on allocation and on free, independent of
    /// any user mapping, so the zero-on-free guarantee holds even when the
    /// task whose teardown frees the region's last reference is a kernel
    /// thread with no live address space (a driver-store unload).
    #[must_use]
    pub const fn new(
        arch: &'static A,
        frames: &'static FrameAllocator,
        physmap: &'static (dyn PhysMap + Sync),
    ) -> Self {
        Self {
            arch,
            frames,
            physmap,
        }
    }

    /// Scrub `pages` frames beginning at `phys_base` through the kernel
    /// direct map, cleaning a coherent region's zeros to memory so neither the
    /// device nor a coherent mapping reads past them and no dirty line is
    /// written back over the frames later. Whether they were: frames the map
    /// cannot reach keep what they held, so no one may be let read them.
    #[must_use]
    fn scrub(&self, phys_base: u64, pages: u64, memory: SharedMemory) -> bool {
        let Some(len) = usize::try_from(pages)
            .ok()
            .and_then(|p| p.checked_mul(PAGE_SIZE))
        else {
            return false;
        };
        let Some(ptr) = self.physmap.translate(PhysAddr::new(phys_base), len) else {
            return false;
        };
        // SAFETY: `translate` returned a pointer valid for `len` bytes of the
        // kernel direct map. The frames are the region's own backing, owned by
        // the registry and not mapped writable anywhere else at scrub time
        // (allocation has not yet handed them out / free has dropped the last
        // mapping), so no concurrent access aliases them.
        unsafe {
            core::ptr::write_bytes(ptr.as_ptr(), 0, len);
        }
        if memory == SharedMemory::DmaCoherent {
            self.physmap.clean_invalidate(PhysAddr::new(phys_base), len);
        }
        true
    }

    /// A scrubbed backing of `pages` frames as blocks largest first, each
    /// drawn as large as the free frames allow; [`Errno::BadAddress`] where
    /// the direct map cannot reach one to scrub it.
    fn chunks(
        &self,
        class: MemoryClass,
        pages: u64,
        memory: SharedMemory,
        ceiling: Option<PhysAddr>,
    ) -> Result<Vec<SharedChunk>, Errno> {
        if pages == 0 {
            return Err(Errno::LengthOutOfRange);
        }
        let blocks = self
            .frames
            .alloc_chunks_user(class, pages, ceiling)
            .map_err(AllocError::as_errno)?;
        let mut chunks: Vec<SharedChunk> = Vec::new();
        if chunks.try_reserve_exact(blocks.len()).is_err() {
            self.frames.free_chunks(&blocks);
            return Err(Errno::OutOfMemory);
        }
        chunks.extend(
            blocks
                .iter()
                .map(|&FrameBlock { frame, order }| SharedChunk {
                    phys_base: frame.start().as_u64(),
                    order,
                    pages: 1u64 << order,
                }),
        );
        // Before any mapping can show the frames' previous contents.
        if !chunks
            .iter()
            .all(|chunk| self.scrub(chunk.phys_base, chunk.pages, memory))
        {
            self.frames.free_chunks(&blocks);
            return Err(Errno::BadAddress);
        }
        Ok(chunks)
    }
}

impl<A> SharedMemFacility for LiveSharedMem<A>
where
    A: SchedulerArch + Send + Sync + 'static,
{
    fn window_room(&self, pages: u64) -> Result<(), Errno> {
        let cpu = self.arch.current_cpu();
        match with_current_live_space(cpu, |space| space.shared_room(pages)) {
            Some(true) => Ok(()),
            Some(false) => Err(live_errno(LiveSpaceError::Mmio(MmioError::NoVirtualSpace))),
            None => Err(Errno::NotImplemented),
        }
    }

    fn alloc_region(&self, pages: u64) -> Result<Vec<SharedChunk>, Errno> {
        self.chunks(MemoryClass::UserAnon, pages, SharedMemory::Cacheable, None)
    }

    fn alloc_dma_region(&self, pages: u64, backing: DmaBacking) -> Result<Vec<SharedChunk>, Errno> {
        let limit = match backing {
            DmaBacking::Scattered { output_limit } => {
                let ceiling = (output_limit != u64::MAX).then_some(PhysAddr::new(output_limit));
                return self.chunks(MemoryClass::Dma, pages, SharedMemory::DmaCoherent, ceiling);
            }
            DmaBacking::Contiguous { limit } => limit,
        };
        let order = pages
            .checked_next_power_of_two()
            .filter(|_| pages != 0)
            .map(u64::trailing_zeros)
            .filter(|&order| order <= MAX_ORDER)
            .ok_or(Errno::LengthOutOfRange)?;
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(1)
            .map_err(|_| Errno::OutOfMemory)?;
        let ceiling = (limit != 0).then_some(PhysAddr::new(limit));
        let frame = self
            .frames
            .alloc_order_under_user(MemoryClass::Dma, order, ceiling)
            .map_err(AllocError::as_errno)?;
        let phys_base = frame.start().as_u64();
        let pages = 1u64 << order;
        if !self.scrub(phys_base, pages, SharedMemory::DmaCoherent) {
            let _ = self.frames.free_order(frame, order);
            return Err(Errno::BadAddress);
        }
        chunks.push(SharedChunk {
            phys_base,
            order,
            pages,
        });
        Ok(chunks)
    }

    fn map_region(&self, chunks: &[SharedChunk], memory: SharedMemory) -> Result<u64, Errno> {
        // Project the chunk list onto the `(phys_base, pages)` list the live
        // space maps into one contiguous virtual window.
        let mut list: Vec<(u64, u64)> = Vec::new();
        if list.try_reserve_exact(chunks.len()).is_err() {
            return Err(Errno::OutOfMemory);
        }
        for c in chunks {
            list.push((c.phys_base, c.pages));
        }
        let cpu = self.arch.current_cpu();
        with_current_live_space(cpu, |space| space.map_shared_chunks(&list, memory))
            .ok_or(Errno::NotImplemented)?
            .map_err(live_errno)
    }

    fn unmap_region(&self, base: u64, len: usize) -> Result<(), Errno> {
        let cpu = self.arch.current_cpu();
        with_current_live_space(cpu, |space| space.unmap_shared(base, len))
            .ok_or(Errno::NotImplemented)?
            .map_err(live_errno)
    }

    fn free_region(&self, chunks: &[SharedChunk], memory: SharedMemory) {
        // Zeroed through the direct map, so even a kernel-thread teardown with
        // no live address space frees nothing that still holds the region's
        // bytes: a block the map cannot reach stays allocated.
        for c in chunks {
            if self.scrub(c.phys_base, c.pages, memory) {
                let frame = Frame::containing(PhysAddr::new(c.phys_base));
                let _ = self.frames.free_order(frame, c.order);
            }
        }
    }

    fn surrender_region(&self, chunks: &[SharedChunk], custodian: &DmaCustodian) {
        for c in chunks {
            // Custody keeps the block allocated and scrubs it again before any
            // free, so what this zeroing buys is a device fetching zeros.
            let _ = self.scrub(c.phys_base, c.pages, SharedMemory::DmaCoherent);
            let block = FrameBlock {
                frame: Frame::containing(PhysAddr::new(c.phys_base)),
                order: c.order,
            };
            custodian
                .custody()
                .hold(custodian.node, custodian.generation, block);
        }
    }

    fn kernel_window(&self, chunks: &[SharedChunk], len: usize) -> Option<core::ptr::NonNull<u8>> {
        // Only a single-chunk region is physically contiguous, so one direct-
        // map translation covers the whole window; a multi-chunk region is
        // not contiguous and fails closed (no kernel consumer maps one).
        if let [chunk] = chunks {
            self.physmap.translate(PhysAddr::new(chunk.phys_base), len)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use alloc::sync::Arc;
    use std::boxed::Box;

    use tairix_kernel_mem::Unpublished;

    use crate::kthread::publish_live_space_for_test;
    use crate::procspace::ProcessSpace;
    use crate::test_arch::TestArch;
    use crate::test_live::{FakeLive, DMA_PHYS, FILE_BASE, FILE_RESIDENT, PLACED_BASE};

    /// A port that cannot map memory for a device that does not snoop says
    /// so, rather than reporting the carve's address bad.
    #[test]
    fn an_unmappable_memory_type_is_not_supported() {
        assert_eq!(
            dma_errno(DmaError::PageTable(PageTableError::Unsupported)),
            Errno::NotSupported
        );
        assert_eq!(
            mmio_errno(MmioError::PageTable(PageTableError::Unsupported)),
            Errno::NotSupported
        );
        assert_eq!(
            dma_errno(DmaError::PageTable(PageTableError::AlreadyMapped)),
            Errno::BadAddress
        );
    }

    /// A `TestArch` reporting `cpu`, leaked to the `'static` shape the
    /// producers hold (mirroring the boot-global arch handle).
    fn arch_at(cpu: u32) -> &'static TestArch {
        Box::leak(Box::new(TestArch::on_cpu(cpu)))
    }

    /// Wrap `fake` in the refcounted [`ProcessSpace`]
    /// [`publish_live_space_for_test`] publishes — the same shape a process's
    /// threads hold for their whole lives — returning the handle to publish and
    /// a raw pointer to inspect the recording after the producer call (the
    /// space's lock is released by then; single-threaded).
    ///
    /// The `FakeLive` stays boxed inside the space, so its address is stable
    /// for as long as the publication's guard holds the handle alive.
    fn shared_fake() -> (Arc<ProcessSpace>, *const FakeLive) {
        shared_fake_with(FakeLive::default())
    }

    fn shared_fake_with(fake: FakeLive) -> (Arc<ProcessSpace>, *const FakeLive) {
        let boxed = Box::new(fake);
        let ptr: *const FakeLive = &raw const *boxed;
        (Arc::new(ProcessSpace::for_test(boxed)), ptr)
    }

    const PAGE: usize = 4096;

    #[test]
    fn mem_map_routes_a_fixed_request_to_the_current_live_space() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        let base = 0x4000;
        let got = producer.map(2 * PAGE, MapFlags::FIXED, base);
        assert_eq!(got, Ok(base));
        // The producer rounded the byte length to a page count and forwarded
        // the FIXED base unchanged.
        // SAFETY: the producer's `&mut` has ended; single-threaded read.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.anon_maps, std::vec![(base, 2)]);
    }

    #[test]
    fn mem_map_unmap_routes_to_the_current_live_space() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        assert_eq!(producer.unmap(0x4000, PAGE, &mut Unpublished), Ok(()));
        // SAFETY: see above.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.anon_unmaps, std::vec![(0x4000, 1)]);
    }

    #[test]
    fn mem_map_non_fixed_routes_to_the_placement_allocator() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        // A non-`FIXED` request asks the live space to choose the base; the
        // `addr_hint` is ignored, and the placed base flows back unchanged.
        let got = producer.map(2 * PAGE, MapFlags::empty(), 0xDEAD_0000);
        assert_eq!(got, Ok(PLACED_BASE));
        // The producer routed to `map_anonymous_placed` (page count only),
        // never the `FIXED` `map_anonymous`.
        // SAFETY: the producer's `&mut` has ended; single-threaded read.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.anon_placed, std::vec![2]);
        assert!(recorded.anon_maps.is_empty());
    }

    #[test]
    fn mem_map_reserve_non_fixed_routes_to_the_placement_reservation() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        // A non-`FIXED` `mem_map` reserves address space only (no eager
        // commit); the placed base flows back unchanged.
        let got = MemMap::reserve(&producer, 2 * PAGE, MapFlags::empty(), 0xDEAD_0000);
        assert_eq!(got, Ok(PLACED_BASE));
        // SAFETY: the producer's `&mut` has ended; single-threaded read.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.anon_reserves, std::vec![2]);
        // A reservation never eagerly maps.
        assert!(recorded.anon_maps.is_empty());
        assert!(recorded.anon_placed.is_empty());
    }

    #[test]
    fn mem_map_reserve_fixed_routes_to_the_placed_reservation() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        let base = 0x4000;
        let got = MemMap::reserve(&producer, 2 * PAGE, MapFlags::FIXED, base);
        assert_eq!(got, Ok(base));
        // SAFETY: the producer's `&mut` has ended; single-threaded read.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.anon_reserves_at, std::vec![(base, 2)]);
        assert!(recorded.anon_maps.is_empty());
    }

    #[test]
    fn mem_map_with_no_published_space_fails_closed_for_a_non_fixed_request() {
        let cpu = crate::test_boot::claim_cpu();
        // No live space published on this CPU: a non-`FIXED` placement must
        // also fail closed rather than fabricating a base.
        let producer = LiveMemMap::new(arch_at(cpu));
        assert_eq!(
            producer.map(PAGE, MapFlags::empty(), 0),
            Err(Errno::NotImplemented)
        );
    }

    #[test]
    fn mem_map_with_no_published_space_fails_closed() {
        let cpu = crate::test_boot::claim_cpu();
        // No live space published on this CPU: the producer must not map
        // anything (a task spawned without a retained space).
        let producer = LiveMemMap::new(arch_at(cpu));
        assert_eq!(
            producer.map(PAGE, MapFlags::FIXED, 0x4000),
            Err(Errno::NotImplemented)
        );
    }

    #[test]
    fn mem_map_folds_an_out_of_memory_error() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, _ptr) = shared_fake_with(FakeLive {
            next: Some(LiveSpaceError::Anon(AnonError::OutOfMemory)),
            ..FakeLive::default()
        });
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        assert_eq!(
            producer.map(PAGE, MapFlags::FIXED, 0x4000),
            Err(Errno::OutOfMemory)
        );
    }

    #[test]
    fn file_map_reserve_routes_to_the_current_live_space() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        // The byte length rounds up to whole pages; the reserved base flows
        // back unchanged.
        assert_eq!(FileMap::reserve(&producer, PAGE as u64 + 1), Ok(FILE_BASE));
        // SAFETY: the producer's `&mut` has ended; single-threaded read.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.file_reserves, std::vec![2]);
    }

    #[test]
    fn file_map_page_and_release_route_to_the_current_live_space() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMemMap::new(arch_at(cpu));
        assert_eq!(producer.map_page(FILE_BASE, &[7; 12]), Ok(()));
        assert_eq!(
            producer.release(FILE_BASE, 4 * PAGE as u64, &mut Unpublished),
            Ok(FILE_RESIDENT)
        );
        // SAFETY: see above.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.file_page_maps, std::vec![(FILE_BASE, 12)]);
        assert_eq!(recorded.file_releases, std::vec![(FILE_BASE, 4)]);
    }

    #[test]
    fn file_map_with_no_published_space_fails_closed() {
        let cpu = crate::test_boot::claim_cpu();
        // No live space published on this CPU: every file-mapping operation
        // announces the inert interface rather than pretending anything was
        // reserved, backed, or freed. A zero length is refused before the
        // space is even consulted.
        let producer = LiveMemMap::new(arch_at(cpu));
        assert_eq!(
            FileMap::reserve(&producer, PAGE as u64),
            Err(Errno::NotImplemented)
        );
        assert_eq!(
            producer.map_page(0xF000_0000, &[1]),
            Err(Errno::NotImplemented)
        );
        assert_eq!(
            producer.release(0xF000_0000, PAGE as u64, &mut Unpublished),
            Err(Errno::NotImplemented)
        );
        assert_eq!(FileMap::reserve(&producer, 0), Err(Errno::LengthOutOfRange));
    }

    #[test]
    fn mmio_map_routes_a_granted_window_to_the_current_live_space() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMmioMap::new(arch_at(cpu));
        let va = producer.map_window(0xFE98_0000, 0x4000, MmioMemoryKind::Device);
        assert_eq!(va, Ok(0x9000_1000));
        // SAFETY: see above.
        let recorded = unsafe { &*ptr };
        assert_eq!(recorded.device_maps, std::vec![(0xFE98_0000, 0x4000)]);
        // A device window never takes either framebuffer path.
        assert!(recorded.framebuffer_maps.is_empty());
    }

    #[test]
    fn mmio_map_routes_a_write_combining_framebuffer_to_the_scanout_path() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMmioMap::new(arch_at(cpu));
        let va = producer.map_window(
            0x8000_0000,
            0x30_0000,
            MmioMemoryKind::FramebufferWriteCombine,
        );
        assert_eq!(va, Ok(0x9000_2000));
        // SAFETY: the producer's `&mut` has ended; single-threaded read.
        let recorded = unsafe { &*ptr };
        // A framebuffer grant takes the scan-out (Normal-NC) path, never the
        // strongly-ordered device path.
        assert_eq!(
            recorded.framebuffer_maps,
            std::vec![(0x8000_0000, 0x30_0000)]
        );
        assert!(recorded.device_maps.is_empty());
    }

    #[test]
    fn mmio_map_with_no_published_space_fails_closed() {
        let cpu = crate::test_boot::claim_cpu();
        let producer = LiveMmioMap::new(arch_at(cpu));
        assert_eq!(
            producer.map_window(0xFE98_0000, 0x4000, MmioMemoryKind::Device),
            Err(Errno::NotImplemented)
        );
    }

    #[test]
    fn mmio_map_folds_a_no_virtual_space_error() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, _ptr) = shared_fake_with(FakeLive {
            next: Some(LiveSpaceError::Mmio(MmioError::NoVirtualSpace)),
            ..FakeLive::default()
        });
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveMmioMap::new(arch_at(cpu));
        assert_eq!(
            producer.map_window(0xFE98_0000, 0x4000, MmioMemoryKind::Device),
            Err(Errno::OutOfMemory)
        );
    }

    #[test]
    fn dma_alloc_routes_a_carve_to_the_current_live_space() {
        let cpu = crate::test_boot::claim_cpu();
        let (fake, ptr) = shared_fake();
        let _guard = publish_live_space_for_test(cpu, fake);

        let producer = LiveDmaAlloc::new(arch_at(cpu));
        let carve = producer.alloc(2 * PAGE, 0x4000_0000, test_custodian());
        // The CPU VA, the physical-base-as-device-address and the backing
        // length flow back from the live space unchanged.
        assert_eq!(
            carve,
            Ok(DmaCarve {
                cpu_va: 0xD000_2000,
                device_addr: DMA_PHYS,
                len: 2 * PAGE as u64,
            })
        );
        // SAFETY: the producer's `&mut` has ended; single-threaded read.
        let recorded = unsafe { &*ptr };
        assert_eq!(
            recorded.dma_allocs,
            std::vec![(2 * PAGE, 0x4000_0000, TEST_NODE, TEST_GENERATION)],
            "the custodian reaches the space unchanged"
        );
    }

    #[test]
    fn a_carve_for_a_device_that_is_gone_is_refused_as_offline() {
        use tairix_kernel_mem::{
            BootMemoryMap, FrameAllocator, MemoryRegion, PhysAddr, RegionKind, SimPhysMap,
        };

        /// A tree that still holds only [`LIVE_NODE`].
        struct OneNodeTree;
        impl crate::hwtree::HwNodeLiveness for OneNodeTree {
            fn is_live(&self, node_id: u32) -> bool {
                node_id == LIVE_NODE
            }
        }
        const LIVE_NODE: u32 = TEST_NODE + 1;
        let cpu = crate::test_boot::claim_cpu();

        let base = PhysAddr::new(16 * PAGE as u64);
        let mut map = BootMemoryMap::new();
        map.push(MemoryRegion {
            kind: RegionKind::Usable,
            start: base,
            length: (16 * PAGE) as u64,
        });
        let frames: &'static FrameAllocator =
            Box::leak(Box::new(FrameAllocator::new(&map).expect("allocator")));
        let physmap: &'static SimPhysMap = Box::leak(Box::new(SimPhysMap::new(base, 16 * PAGE)));
        let quarantine: &'static crate::dmaquarantine::DmaQuarantine = Box::leak(Box::new(
            crate::dmaquarantine::DmaQuarantine::new(frames, physmap, &OneNodeTree),
        ));
        let _guard = publish_live_space_for_test(
            cpu,
            Arc::new(ProcessSpace::for_test(crate::procspace::host_test_space!())),
        );
        let producer = LiveDmaAlloc::new(arch_at(cpu));
        let custodian = |node| {
            DmaCustodian::untranslated(
                node,
                TEST_GENERATION,
                quarantine,
                tairix_abi::DmaCoherence::Snooped,
            )
        };

        assert_eq!(
            producer.alloc(PAGE, 0, custodian(TEST_NODE)),
            Err(Errno::DeviceOffline)
        );
        assert_eq!(quarantine.tracked_nodes(), 0, "no custody was opened");
        assert!(
            producer.alloc(PAGE, 0, custodian(LIVE_NODE)).is_ok(),
            "the same space carves for a node the tree still holds"
        );
    }

    /// A region whose frames the direct map cannot reach is never handed out,
    /// and a block of one is never freed still holding what it held.
    #[test]
    fn a_region_the_direct_map_cannot_scrub_is_refused_and_never_freed() {
        use tairix_kernel_mem::{
            BootMemoryMap, FrameAllocator, MemoryRegion, PhysAddr, RegionKind, SimPhysMap,
        };
        let base = PhysAddr::new(16 * PAGE as u64);
        let mut map = BootMemoryMap::new();
        map.push(MemoryRegion {
            kind: RegionKind::Usable,
            start: base,
            length: (16 * PAGE) as u64,
        });
        let frames: &'static FrameAllocator =
            Box::leak(Box::new(FrameAllocator::new(&map).expect("allocator")));
        // A direct map over other memory: none of these frames is in it.
        let elsewhere: &'static SimPhysMap = Box::leak(Box::new(SimPhysMap::new(
            PhysAddr::new(64 * PAGE as u64),
            PAGE,
        )));
        let reachable: &'static SimPhysMap = Box::leak(Box::new(SimPhysMap::new(base, 16 * PAGE)));
        let cpu = crate::test_boot::claim_cpu();
        let blind = LiveSharedMem::new(arch_at(cpu), frames, elsewhere);
        let free = frames.free_frames();
        assert_eq!(blind.alloc_region(3), Err(Errno::BadAddress));
        assert_eq!(
            blind.alloc_dma_region(
                3,
                DmaBacking::Scattered {
                    output_limit: u64::MAX
                }
            ),
            Err(Errno::BadAddress)
        );
        assert_eq!(
            blind.alloc_dma_region(2, DmaBacking::Contiguous { limit: 0 }),
            Err(Errno::BadAddress)
        );
        assert_eq!(frames.free_frames(), free, "nothing drawn stays out");

        let seeing = LiveSharedMem::new(arch_at(cpu), frames, reachable);
        let chunks = seeing.alloc_region(2).expect("reachable frames");
        blind.free_region(&chunks, SharedMemory::Cacheable);
        assert_eq!(
            frames.free_frames(),
            free - 2,
            "a block it could not scrub stays allocated"
        );
        seeing.free_region(&chunks, SharedMemory::Cacheable);
        assert_eq!(frames.free_frames(), free);
    }

    /// A region a process asks for is admitted against the kernel's reserve
    /// before a frame is drawn, so no request can take what the kernel keeps.
    #[test]
    fn a_region_is_refused_before_it_could_take_the_kernel_s_reserve() {
        use tairix_kernel_mem::{
            BootMemoryMap, FrameAllocator, MemoryRegion, PhysAddr, RegionKind, SimPhysMap,
        };
        const RAM_PAGES: usize = 256;
        let base = PhysAddr::new(16 * PAGE as u64);
        let mut map = BootMemoryMap::new();
        map.push(MemoryRegion {
            kind: RegionKind::Usable,
            start: base,
            length: (RAM_PAGES * PAGE) as u64,
        });
        let frames: &'static FrameAllocator =
            Box::leak(Box::new(FrameAllocator::new(&map).expect("allocator")));
        let physmap: &'static SimPhysMap =
            Box::leak(Box::new(SimPhysMap::new(base, RAM_PAGES * PAGE)));
        let shared = LiveSharedMem::new(arch_at(crate::test_boot::claim_cpu()), frames, physmap);
        let reserve = frames.reserve_frames();
        assert!(reserve > 0);
        let free = frames.free_frames();
        let spare = u64::try_from(free - reserve).unwrap();
        for draw in [
            shared.alloc_region(spare + 1),
            shared.alloc_dma_region(
                spare + 1,
                DmaBacking::Scattered {
                    output_limit: u64::MAX,
                },
            ),
            shared.alloc_dma_region(RAM_PAGES as u64, DmaBacking::Contiguous { limit: 0 }),
        ] {
            assert_eq!(draw, Err(Errno::OutOfMemory));
        }
        assert_eq!(frames.free_frames(), free);
        assert_eq!(frames.committed_frames(), 0, "every admission was returned");
        let chunks = shared
            .alloc_region(spare)
            .expect("what the machine can spare");
        assert_eq!(frames.free_frames(), reserve);
        assert_eq!(frames.committed_frames(), 0, "the draw spent its admission");
        shared.free_region(&chunks, SharedMemory::Cacheable);
        assert_eq!(frames.free_frames(), free);
    }

    /// A device region is scrubbed and charged to the DMA class, scattered
    /// below its unit's reach as the frames give or one block under a limit,
    /// and freed back whole.
    #[test]
    fn a_device_region_is_charged_to_dma_and_freed_back() {
        use tairix_kernel_mem::{
            BootMemoryMap, FrameAllocator, MemoryClass, MemoryRegion, PhysAddr, RegionKind,
            SimPhysMap,
        };
        let base = PhysAddr::new(16 * PAGE as u64);
        let mut map = BootMemoryMap::new();
        map.push(MemoryRegion {
            kind: RegionKind::Usable,
            start: base,
            length: (64 * PAGE) as u64,
        });
        let frames: &'static FrameAllocator =
            Box::leak(Box::new(FrameAllocator::new(&map).expect("allocator")));
        let physmap: &'static SimPhysMap = Box::leak(Box::new(SimPhysMap::new(base, 64 * PAGE)));
        let shared = LiveSharedMem::new(arch_at(crate::test_boot::claim_cpu()), frames, physmap);
        let dma = || frames.snapshot().class[MemoryClass::Dma as usize];
        let free = frames.free_frames();
        // Below what the device's unit can name, though frames above are free.
        let reach = base.as_u64() + (8 * PAGE) as u64;
        let scattered = shared
            .alloc_dma_region(
                3,
                DmaBacking::Scattered {
                    output_limit: reach,
                },
            )
            .expect("scattered");
        assert_eq!(scattered.iter().map(|chunk| chunk.pages).sum::<u64>(), 3);
        assert!(scattered
            .iter()
            .all(|chunk| chunk.phys_base + chunk.pages * PAGE as u64 <= reach));
        assert_eq!(dma(), 3);
        let limit = base.as_u64() + (64 * PAGE) as u64;
        let contiguous = shared
            .alloc_dma_region(3, DmaBacking::Contiguous { limit })
            .expect("contiguous");
        assert_eq!(contiguous.len(), 1, "one block, rounded to a buddy order");
        assert_eq!(contiguous[0].pages, 4);
        assert!(contiguous[0].phys_base + 4 * PAGE as u64 <= limit);
        assert_eq!(dma(), 7);
        shared.free_region(&scattered, SharedMemory::DmaCoherent);
        shared.free_region(&contiguous, SharedMemory::DmaCoherent);
        assert_eq!(dma(), 0);
        assert_eq!(frames.free_frames(), free);
    }

    #[test]
    fn dma_alloc_with_no_published_space_fails_closed() {
        let cpu = crate::test_boot::claim_cpu();
        let producer = LiveDmaAlloc::new(arch_at(cpu));
        assert_eq!(
            producer.alloc(PAGE, 0, test_custodian()),
            Err(Errno::NotImplemented)
        );
    }

    #[test]
    fn dma_alloc_folds_an_unreachable_or_exhausted_limit_as_the_allocator_does() {
        let cpu = crate::test_boot::claim_cpu();
        for (refusal, errno) in [
            (AllocError::OutOfRange, Errno::OutOfRange),
            (AllocError::OutOfMemory, Errno::OutOfMemory),
        ] {
            let (fake, _ptr) = shared_fake_with(FakeLive {
                next: Some(LiveSpaceError::Dma(DmaError::Alloc(refusal))),
                ..FakeLive::default()
            });
            let _guard = publish_live_space_for_test(cpu, fake);

            let producer = LiveDmaAlloc::new(arch_at(cpu));
            assert_eq!(
                producer.alloc(PAGE, 0x1000, test_custodian()),
                Err(errno),
                "{refusal:?}"
            );
        }
    }

    const TEST_NODE: u32 = 5;
    const TEST_GENERATION: u64 = 2;

    fn test_custodian() -> DmaCustodian {
        DmaCustodian::untranslated(
            TEST_NODE,
            TEST_GENERATION,
            &crate::devres::NULL_DMA_QUARANTINE,
            tairix_abi::DmaCoherence::Snooped,
        )
    }
}
