//! Physical frame allocator — buddy + bitmap hybrid.
//!
//! # Layers
//!
//! - **Bitmap.** One bit per architectural page-frame across the whole
//!   physical address range described by the [`BootMemoryMap`].
//!   `0 = free`, `1 = allocated or reserved or non-existent`. The bitmap
//!   is the source of truth for "is this frame currently handed out?",
//!   so a double-free or a free of a reserved frame is detected
//!   immediately and reported as
//!   [`AllocError::InvariantViolation`].
//!
//! - **Zones.** The usable span is cut where the DMA windows discovery
//!   reports stop reaching, and at 4 GiB, so the memory a device that
//!   reaches less needs is kept for it: ordinary requests take the highest
//!   zone first and leave each lower zone a reserve. See
//!   [`FrameAllocator::with_dma_reaches`].
//!
//! - **Buddy free lists.** For every zone and order `0..=MAX_ORDER` we keep
//!   a [`tairix_inline::IntrusiveList`] of free blocks of exactly that
//!   order, threaded through a per-frame `links` array indexed by starting
//!   frame. Splits push two half-blocks to `order - 1`; merges pop a buddy
//!   at `order` and push the parent at `order + 1`. The bitmap is consulted
//!   on every merge to refuse merging across reserved boundaries (this is
//!   the "hybrid" part — reserved frames look identical to allocated
//!   frames at the bitmap level, so the buddy never reaches across them).
//!
//! # No dependency on the kernel heap
//!
//! The per-order free lists are **intrusive**: their links live in the
//! `links`/`tags` arrays this allocator owns, both sized once from
//! the frame count at construction. Allocation and freeing therefore
//! touch no other allocator — critically, they never call the global
//! kernel heap. This is what lets the kernel heap grow by drawing frames
//! from here without re-entering itself (a heap that fed itself through
//! a page allocator whose free lists allocated *from that heap* would
//! deadlock under its own lock). The only heap use is the one-time
//! `links`/`tags`/`bitmap` construction, before the heap is under
//! load. Each free block occupies at most one node (its start frame),
//! so the per-frame overhead is a fixed `2 * usize + 1` byte — far
//! leaner than a per-frame descriptor, and proportional to the RAM the
//! machine actually has.
//!
//! # Concurrency
//!
//! The allocator's state is wrapped in
//! [`tairix_sync::SpinLock`] at the [`FrameAllocator`] level.
//! Internal helpers operate on `&mut FrameAllocatorState` and are
//! oblivious to locking.
//!
//! # Result-returning OOM
//!
//! `alloc` / `alloc_order` return [`AllocError::OutOfMemory`]; the
//! allocator never panics on resource exhaustion.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use tairix_collections::SmallVec;
use tairix_inline::intrusive::{IntrusiveList, Link};
use tairix_reclaim::RESERVE_DIVISOR;
use tairix_sync::SpinLock;

use crate::bootinfo::{BootMemoryMap, RegionKind};
use crate::error::AllocError;

/// The one physical-RAM class vocabulary, shared with the reporting ABI so a
/// charged class and a reported class can never be spelled differently.
pub use tairix_abi::{MemoryClass, MEMORY_CLASS_COUNT};

/// Page-frame size in bytes and its bit-shift: the one system granule, shared
/// with the mapping ABI and both heaps.
pub use tairix_abi::{PAGE_SHIFT, PAGE_SIZE};

/// Maximum buddy order supported by this allocator.
///
/// Order `n` blocks span `2^n` frames, i.e. `4 KiB * 2^n` bytes.
/// `MAX_ORDER = 13` ⇒ the largest atomically-allocatable block is
/// `4 KiB << 13 = 32 MiB`. It bounds one thing only: how much
/// *physically contiguous* memory a single draw can yield — a genuine
/// hardware-shaped limit (a device's DMA window, a coarse page-table
/// block), not a capacity anything scales against. A caller needing more
/// memory than this asks for several blocks and maps them into one virtual
/// window, exactly as the growable kernel heap does
/// (`plans/FIX-KHEAP.md`), so no consumer's size limit is derived from this
/// order.
pub const MAX_ORDER: u32 = 13;

// [`RESERVE_DIVISOR`](tairix_reclaim::RESERVE_DIVISOR) is the one
// definition shared with the memory-pressure policy (`tairix_reclaim`),
// so the frame allocator's user-commit floor and the pressure band's
// critical floor can never diverge.

/// Type alias for "number of frames", used to keep call-site arithmetic
/// distinct from frame-index arithmetic.
pub type FrameCount = usize;

/// A naturally aligned block of `2^order` frames, as the buddy allocator
/// hands one out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameBlock {
    /// Its first frame.
    pub frame: Frame,
    /// Its buddy order.
    pub order: u32,
}

impl FrameBlock {
    /// Bytes the block spans.
    #[must_use]
    pub fn len(self) -> usize {
        PAGE_SIZE << self.order
    }

    /// Always `false`: a block spans at least one page.
    #[must_use]
    pub fn is_empty(self) -> bool {
        false
    }
}

/// The blocks of one [`FrameAllocator::alloc_chunks_user`] draw, largest
/// first; a draw of one block allocates no list.
pub type Chunks = SmallVec<FrameBlock, 1>;

/// Physical byte address.
///
/// A thin newtype around `u64` so kernel APIs cannot accidentally pass a
/// physical address where a virtual one is expected (or vice versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysAddr(u64);

impl PhysAddr {
    /// Wrap a raw `u64` as a physical address.
    #[must_use]
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    /// The numeric value.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Frame index this address falls into.
    ///
    /// Truncating: `0..PAGE_SIZE` ↦ frame 0.
    #[must_use]
    pub const fn frame_index(self) -> u64 {
        self.0 >> PAGE_SHIFT
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}

/// A single page-frame, identified by its 0-based index in physical RAM.
///
/// `Frame(n)` covers the byte range `[n << PAGE_SHIFT, (n+1) << PAGE_SHIFT)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Frame(pub usize);

impl Frame {
    /// Starting physical address of this frame.
    #[must_use]
    pub fn start(self) -> PhysAddr {
        PhysAddr::new((self.0 as u64) << PAGE_SHIFT)
    }

    /// The frame containing `addr` (truncating).
    #[must_use]
    pub fn containing(addr: PhysAddr) -> Self {
        Self((addr.as_u64() >> PAGE_SHIFT) as usize)
    }
}

// ---------------------------------------------------------------------------
// State (private)
// ---------------------------------------------------------------------------

/// Nibble sentinel for "absent": not a registered free-block head in the low
/// nibble, no charged class in the high one. A real order is `0..=MAX_ORDER`
/// (≤ 13) and a real class `0..6`, so `0xF` collides with neither.
const NIBBLE_NONE: u8 = 0xF;

/// One frame's bookkeeping byte: the free-list order it heads while free, and
/// the [`MemoryClass`] it is charged to while allocated.
///
/// The two never coexist — a frame is either part of a free block or handed
/// out — so they share the byte the allocator already loads and stores on
/// every allocate and free. Packing them costs no extra memory, where a
/// parallel per-frame class array would.
///
/// The order is meaningful only on a free block's *head*; the class is stamped
/// on **every** frame of an allocated block, because that is the granularity a
/// free happens at: the kernel window releases a multi-frame region one page
/// at a time as its page tables give the frames back, so a head-only charge
/// could not tell which class an interior frame belonged to. Carrying it per
/// frame is what makes a free self-describing — no caller names a class to
/// give memory back, so none can mis-attribute one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct FrameTag(u8);

impl FrameTag {
    /// Neither a registered free head nor a charged allocation: an interior
    /// frame of a block, or one not yet populated.
    const UNTRACKED: Self = Self(NIBBLE_NONE << 4 | NIBBLE_NONE);

    /// The head of a free block registered at `order`.
    ///
    /// `order` is `0..=MAX_ORDER` by construction at every call site; a value
    /// past the nibble would alias the sentinel, so it is masked rather than
    /// truncated into a different order.
    fn free_head(order: u32) -> Self {
        let low = u8::try_from(order).unwrap_or(NIBBLE_NONE) & NIBBLE_NONE;
        Self(NIBBLE_NONE << 4 | low)
    }

    /// An allocated block's head, charged to `class`.
    fn allocated(class: MemoryClass) -> Self {
        // `MEMORY_CLASS_COUNT` is 6, so every discriminant fits the nibble
        // below the sentinel.
        let high = u8::try_from(class.index()).unwrap_or(NIBBLE_NONE) & NIBBLE_NONE;
        Self(high << 4 | NIBBLE_NONE)
    }

    /// The order this frame heads a free block at, or `None` when it heads
    /// none.
    fn free_order(self) -> Option<u32> {
        let low = self.0 & NIBBLE_NONE;
        (low != NIBBLE_NONE).then_some(u32::from(low))
    }

    /// The class this frame's block is charged to, or `None` when it is not a
    /// charged allocation head.
    fn class(self) -> Option<MemoryClass> {
        let high = usize::from(self.0 >> 4);
        MemoryClass::ALL.get(high).copied()
    }
}

// The largest order must stay clear of the nibble sentinel, or a free head at
// `MAX_ORDER` would read back as "not a head".
const _: () = assert!(MAX_ORDER < NIBBLE_NONE as u32);
// Likewise every class discriminant, or the last class would read back as
// "uncharged" and its free would find no counter to discharge.
const _: () = assert!(MEMORY_CLASS_COUNT < NIBBLE_NONE as usize);

/// One physical address zone: the frames from `start` to the next zone's,
/// and their free blocks.
///
/// A device that reaches only part of memory is served from the zones below
/// its limit, so a zone below the highest keeps back a reserve the requests a
/// higher zone could serve may not take. No block straddles two zones: every
/// boundary is aligned to the largest block, and buddies merge within one.
struct Zone {
    start: usize,
    free_lists: [IntrusiveList; MAX_ORDER as usize + 1],
    free_frames: usize,
    reserve: usize,
}

impl Zone {
    const fn new(start: usize) -> Self {
        Self {
            start,
            free_lists: [const { IntrusiveList::new() }; MAX_ORDER as usize + 1],
            free_frames: 0,
            reserve: 0,
        }
    }
}

/// The zone holding `frame`; the lowest for a frame below every zone.
fn zone_of(zones: &[Zone], frame: usize) -> usize {
    zones
        .iter()
        .rposition(|zone| zone.start <= frame)
        .unwrap_or(0)
}

/// The share of the usable RAM above a zone that it keeps back: Linux's
/// default `lowmem_reserve_ratio`.
const ZONE_RESERVE_RATIO: usize = 256;

/// The most of its own frames a zone keeps back, as a share, so a small low
/// zone still serves ordinary requests once the zones above it are drained.
const ZONE_RESERVE_MAX_SHARE: usize = 4;

/// The limit of a device that can address only 32 bits.
const DMA32_LIMIT: u64 = 1 << 32;

/// The zone boundaries the usable `runs` are cut at, ascending: each limit a
/// DMA reach in `reaches` ends at, and 4 GiB, where usable RAM lies both
/// within the reach below the limit and at or above it. A PCI function a bus
/// driver finds after boot may reach only 32 bits without the boot tree
/// saying so, which is why 4 GiB is always a boundary where RAM spans it.
/// Each limit is aligned down to the largest block.
fn zone_boundaries(runs: &[(usize, usize)], reaches: &[core::ops::Range<u64>]) -> Vec<usize> {
    let has_usable = |lo: usize, hi: usize| runs.iter().any(|&(a, b)| a < hi && lo < b);
    let block = 1usize << MAX_ORDER;
    let mut boundaries: Vec<usize> = reaches
        .iter()
        .cloned()
        .chain(core::iter::once(0..DMA32_LIMIT))
        .filter_map(|reach| {
            let limit = usize::try_from(reach.end >> PAGE_SHIFT).ok()?;
            let low = usize::try_from(reach.start >> PAGE_SHIFT).ok()?;
            let boundary = limit / block * block;
            (boundary > 0 && has_usable(low, boundary) && has_usable(boundary, usize::MAX))
                .then_some(boundary)
        })
        .collect();
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
}

/// Internal, lock-free state of the frame allocator.
///
/// `FrameAllocatorState` is what the [`SpinLock`] in [`FrameAllocator`]
/// guards. Splitting it out makes the locking discipline explicit and
/// lets the unit tests exercise the algorithm without spinlock noise.
///
/// The per-order free lists are intrusive (see the module docs): a heap
/// allocation never occurs on the allocate/free paths, only on the
/// one-time construction of `bitmap`/`links`/`tags`.
struct FrameAllocatorState {
    /// Address-space extent, in frames: the frame count from physical zero
    /// to the highest mapped address. Reported by [`FrameAllocator::total_frames`]
    /// as a diagnostic; it is **not** the size of any per-frame array — the
    /// bitmap and the intrusive arrays are all sized to the *usable* span
    /// (`span`) and indexed from `base_frame`.
    total_frames: usize,
    /// Lowest usable frame index — the base every per-frame array
    /// (`bitmap`, `links`, `tags`) is indexed from. A frame `f` maps to
    /// slot `f - base_frame`. Sizing the arrays to the *usable* frame span
    /// rather than to the whole address-space extent keeps the per-frame
    /// bookkeeping proportional to the RAM the machine actually has, so a
    /// map whose usable window sits at a very high physical address does not
    /// cost arrays indexed from frame zero.
    base_frame: usize,
    /// Number of frames the base-relative arrays cover: `hi - base_frame`,
    /// where `hi` is one past the highest usable frame. A frame outside
    /// `[base_frame, base_frame + span)` is not represented and is treated
    /// as "used" by [`Self::bit`] (it is reserved, a hole, or non-existent).
    span: usize,
    /// Bit set = "frame is allocated, reserved, or non-existent", one bit
    /// per frame of the usable span (indexed by `frame - base_frame`).
    bitmap: Vec<u64>,
    /// Intrusive free-list links, one per frame of the usable span (indexed
    /// by `frame - base_frame`). Only a free block's start frame holds live
    /// links; every other frame's link reads back unlinked.
    links: Vec<Link>,
    /// Per-frame bookkeeping byte, indexed by `frame - base_frame`: which
    /// free list a registered head is on (so an arbitrary buddy is found and
    /// unlinked in O(1) without scanning), or which [`MemoryClass`] an
    /// allocated block's head is charged to.
    tags: Vec<FrameTag>,
    /// The address zones, ascending and covering the usable span, each with
    /// its own free blocks of each order keyed by slot (`frame - base_frame`).
    zones: Vec<Zone>,
    /// Cached count of free frames (sum over the free lists of 2^order).
    free_frames: usize,
    /// Number of whole frames inside `Usable` boot-map regions — the RAM
    /// the allocator can ever hand out. Excludes reserved regions and
    /// physical-address holes (MMIO windows, the space below the RAM base),
    /// so `usable_frames - free_frames` is real allocation, fixed after
    /// construction.
    usable_frames: usize,
    /// Kernel reserve floor, in frames (`usable_frames / RESERVE_DIVISOR`).
    /// A *user* commit ([`FrameAllocator::alloc_user`] /
    /// [`FrameAllocator::alloc_order_user`]) is refused when it would drop
    /// `free_frames` below this, so the kernel always keeps headroom to make
    /// progress (heap growth, page-table build, fault service).
    /// Kernel-internal allocations draw the whole pool. Fixed after
    /// construction.
    reserve_frames: usize,
    /// Frames of anonymous/stack user memory that are *committed* (a
    /// successful `mem_map`/stack reservation promised the caller they can
    /// be touched) but not yet resident (never faulted in). Every such page
    /// is guaranteed a frame: a commit is admitted only while
    /// `free_frames >= reserve_frames + committed_frames + request`, and an
    /// eager user allocation may not draw the free pool below
    /// `reserve_frames + committed_frames`. The count therefore reserves
    /// physical headroom *up front* so a first touch of committed memory
    /// can never fail — the deterministic, no-overcommit refusal happens at
    /// reservation time (`Errno::OutOfMemory`) instead of as a fault-time
    /// kill. Decremented as a committed page faults in
    /// ([`FrameAllocator::alloc_user_committed`]) or its reservation is
    /// released ([`FrameAllocator::uncommit`]).
    committed_frames: usize,
    /// Frames charged to each [`MemoryClass`], indexed by discriminant.
    ///
    /// Maintained inside the lock the allocate/free paths already hold, so
    /// the partition costs one `usize` add per call and no second lock — and
    /// a snapshot reads the whole and its parts from one instant, which is
    /// what makes `usable == free + Σ class` true of the figures a reader
    /// sees rather than only of the allocator's internals.
    class_frames: [FrameCount; MEMORY_CLASS_COUNT],
}

impl FrameAllocatorState {
    /// Frames a user draw or commit may take without dropping the pool below
    /// the kernel reserve and every page committed but not yet resident.
    fn user_headroom(&self) -> usize {
        self.free_frames
            .saturating_sub(self.reserve_frames.saturating_add(self.committed_frames))
    }

    /// The bitmap slot for `frame`, or `None` when `frame` lies outside the
    /// represented usable span `[base_frame, base_frame + span)`.
    #[inline]
    fn bit_slot(&self, frame: usize) -> Option<usize> {
        frame
            .checked_sub(self.base_frame)
            .filter(|&i| i < self.span)
    }

    /// Is `frame` marked used? A frame outside the usable span is not
    /// represented in the bitmap and is treated as used (reserved, a
    /// physical-address hole, or non-existent) — so a buddy probe that
    /// walks off either end of the usable window never merges across it.
    fn bit(&self, frame: usize) -> bool {
        match self.bit_slot(frame) {
            Some(i) => (self.bitmap[i / 64] >> (i % 64)) & 1 == 1,
            None => true,
        }
    }
    /// Mark a usable-span frame used. Only ever called for a frame the
    /// caller has already confirmed is inside the span (an allocated block).
    fn set_bit(&mut self, frame: usize) {
        let i = frame - self.base_frame;
        self.bitmap[i / 64] |= 1u64 << (i % 64);
    }
    /// Mark a usable-span frame free. Only ever called for a frame inside
    /// the span (a usable run at construction, or a freed allocation).
    fn clear_bit(&mut self, frame: usize) {
        let i = frame - self.base_frame;
        self.bitmap[i / 64] &= !(1u64 << (i % 64));
    }

    /// Mark frames `[start, start + count)` as allocated/reserved.
    fn mark_range_used(&mut self, start: usize, count: usize) {
        for i in start..start + count {
            self.set_bit(i);
        }
    }

    /// Are all frames in `[start, start + (1<<order))` free in the bitmap?
    ///
    /// A block that reaches outside the represented usable span is never
    /// "free": those frames are not backed by the bitmap and are treated as
    /// used, so a buddy merge can never fuse a usable block with a
    /// reserved/hole region beyond the window (`bit` returns `true` for
    /// them, but the span check short-circuits before indexing).
    fn block_is_free(&self, start: usize, order: u32) -> bool {
        let n = 1usize << order;
        let Some(rel) = start.checked_sub(self.base_frame) else {
            return false;
        };
        if rel.checked_add(n).is_none_or(|e| e > self.span) {
            return false;
        }
        for i in start..start + n {
            if self.bit(i) {
                return false;
            }
        }
        true
    }

    /// Insert a maximally-aligned free block into the buddy lists.
    ///
    /// `start` must be an off-list, aligned block start of `order`; it
    /// becomes the new head of that order's intrusive list.
    ///
    /// # Errors
    ///
    /// [`AllocError::InvariantViolation`] when the block's slot is outside
    /// the usable span, or the list refuses it because it is already
    /// registered. Both mean this allocator's own bookkeeping has diverged,
    /// and refusing is what stops a corrupt list being handed out as free
    /// memory. [`AllocError::SizeUnsupported`] when `order` names no list.
    fn add_free_block(&mut self, start: usize, order: u32) -> Result<(), AllocError> {
        let slot = start
            .checked_sub(self.base_frame)
            .ok_or(AllocError::InvariantViolation)?;
        let z = zone_of(&self.zones, start);
        let zone = &mut self.zones[z];
        zone.free_lists
            .get_mut(order as usize)
            .ok_or(AllocError::SizeUnsupported)?
            .push_front(&mut self.links[..], slot)
            .map_err(|_| AllocError::InvariantViolation)?;
        zone.free_frames += 1usize << order;
        self.tags[slot] = FrameTag::free_head(order);
        self.free_frames += 1usize << order;
        Ok(())
    }

    /// Unlink the free block starting at `start` from `order`'s list.
    ///
    /// Returns `Ok(false)` (and changes nothing) when `start` is not a
    /// registered free-block head of exactly `order` — the caller then
    /// knows the buddy is split into smaller blocks and cannot merge.
    ///
    /// # Errors
    ///
    /// [`AllocError::InvariantViolation`] when the block is registered at
    /// this order yet the list will not splice it out, which is the
    /// bookkeeping divergence [`Self::add_free_block`] describes, and
    /// [`AllocError::SizeUnsupported`] when `order` names no list.
    fn remove_free_block(&mut self, start: usize, order: u32) -> Result<bool, AllocError> {
        // `start` may be any frame the merge logic probes, including one
        // below `base_frame`; resolve the slot fail-safe.
        let Some(slot) = start.checked_sub(self.base_frame) else {
            return Ok(false);
        };
        if self.tags.get(slot).copied().and_then(FrameTag::free_order) != Some(order) {
            return Ok(false);
        }
        let z = zone_of(&self.zones, start);
        let zone = &mut self.zones[z];
        zone.free_lists
            .get_mut(order as usize)
            .ok_or(AllocError::SizeUnsupported)?
            .unlink(&mut self.links[..], slot)
            .map_err(|_| AllocError::InvariantViolation)?;
        zone.free_frames -= 1usize << order;
        self.tags[slot] = FrameTag::UNTRACKED;
        self.free_frames -= 1usize << order;
        Ok(true)
    }

    /// Greedy insertion of every free run discovered in `[start, end)`:
    /// chop the run into maximally-aligned, maximally-sized buddy blocks.
    ///
    /// # Errors
    ///
    /// As [`Self::add_free_block`].
    fn populate_run(&mut self, mut start: usize, end: usize) -> Result<(), AllocError> {
        while start < end {
            // The largest order we can place at `start` is bounded by
            // (a) alignment of `start` and (b) the number of frames left
            // and (c) MAX_ORDER.
            let align_order = if start == 0 {
                MAX_ORDER
            } else {
                core::cmp::min(start.trailing_zeros(), MAX_ORDER)
            };
            let max_by_remaining = (end - start)
                .checked_ilog2()
                .map_or(0, |o| core::cmp::min(o, MAX_ORDER));
            let order = core::cmp::min(align_order, max_by_remaining);
            self.add_free_block(start, order)?;
            start += 1usize << order;
        }
        Ok(())
    }

    /// The lowest order at or above `order` zone `z` holds a free block of,
    /// where taking `2^order` frames leaves the zone its reserve when
    /// `guarded`.
    fn zone_source(&self, z: usize, order: u32, guarded: bool) -> Option<u32> {
        let zone = &self.zones[z];
        if guarded && zone.free_frames < zone.reserve.saturating_add(1usize << order) {
            return None;
        }
        (order..=MAX_ORDER).find(|&o| !zone.free_lists[o as usize].is_empty())
    }

    /// The order of zone `z`'s list to split and the order up to `order` of
    /// the largest block it can give, leaving its reserve when `guarded`.
    fn zone_largest(&self, z: usize, order: u32, guarded: bool) -> Option<(u32, u32)> {
        let zone = &self.zones[z];
        let held = (0..=MAX_ORDER)
            .rev()
            .find(|&o| !zone.free_lists[o as usize].is_empty())?;
        let spare = if guarded {
            zone.free_frames.checked_sub(zone.reserve)?
        } else {
            zone.free_frames
        };
        let got = held.min(order).min(spare.checked_ilog2()?);
        let from = (got..=MAX_ORDER).find(|&o| !zone.free_lists[o as usize].is_empty())?;
        Some((from, got))
    }

    /// Take the front block of zone `z`'s order-`from` list, split it down to
    /// a `2^order` block, and charge that.
    fn take_front(
        &mut self,
        z: usize,
        from: u32,
        order: u32,
        class: MemoryClass,
    ) -> Result<usize, AllocError> {
        // The lists are LIFO, so the front is the block most recently freed
        // or split at this order: deterministic, and O(1).
        let slot = self.zones[z].free_lists[from as usize]
            .front()
            .ok_or(AllocError::InvariantViolation)?;
        let start = self.base_frame + slot;
        if !self.remove_free_block(start, from)? {
            return Err(AllocError::InvariantViolation);
        }
        self.split_around(start, from, start, order)?;
        self.charge(start, order, class);
        Ok(start)
    }

    /// Take the highest free `2^order`-or-smaller block below `ceiling` in
    /// the zone it falls in, of exactly `order` when `exact`.
    fn take_highest_under(
        &mut self,
        order: u32,
        ceiling: usize,
        exact: bool,
        class: MemoryClass,
    ) -> Result<Option<(usize, u32)>, AllocError> {
        let floor = self.zones[zone_of(&self.zones, ceiling - 1)].start;
        let Some((start, got)) = self
            .largest_free_run_under(order, ceiling, floor)
            .filter(|&(_, got)| !exact || got == order)
        else {
            return Ok(None);
        };
        self.claim_free_run(start, got)?;
        self.charge(start, got, class);
        Ok(Some((start, got)))
    }

    /// Serve an ordinary request from the highest zone that has the block,
    /// a lower zone only while it keeps its reserve for the requests that
    /// can be served nowhere higher.
    fn alloc_order(&mut self, class: MemoryClass, order: u32) -> Result<usize, AllocError> {
        if order > MAX_ORDER {
            return Err(AllocError::SizeUnsupported);
        }
        let top = self.zones.len() - 1;
        for z in (0..=top).rev() {
            if let Some(from) = self.zone_source(z, order, z < top) {
                return self.take_front(z, from, order, class);
            }
        }
        Err(AllocError::OutOfMemory)
    }

    /// Carve a `2^order` block lying wholly below frame `ceiling`.
    ///
    /// The zone the ceiling falls in is the highest the request can use, so
    /// its reserve is the request's own: the highest block below the ceiling
    /// there, found in the bitmap because the lists are address-blind, so
    /// memory lower still stays for devices that reach less. A lower zone
    /// gives one only while it keeps its own reserve.
    fn alloc_order_under(
        &mut self,
        class: MemoryClass,
        order: u32,
        ceiling: usize,
    ) -> Result<usize, AllocError> {
        if order > MAX_ORDER {
            return Err(AllocError::SizeUnsupported);
        }
        if ceiling >= self.base_frame + self.span {
            return self.alloc_order(class, order);
        }
        if ceiling <= self.base_frame {
            return Err(AllocError::OutOfRange);
        }
        if let Some((start, _)) = self.take_highest_under(order, ceiling, true, class)? {
            return Ok(start);
        }
        for z in (0..zone_of(&self.zones, ceiling - 1)).rev() {
            if let Some(from) = self.zone_source(z, order, true) {
                return self.take_front(z, from, order, class);
            }
        }
        Err(AllocError::OutOfMemory)
    }

    /// The largest block of order up to `order` the pool holds, below frame
    /// `ceiling` where one is given, and its order: a fragmented pool steps
    /// the order down within one search rather than in one per order. The
    /// zones are drawn on as [`Self::alloc_order`] and
    /// [`Self::alloc_order_under`] draw on them.
    fn alloc_largest(
        &mut self,
        class: MemoryClass,
        order: u32,
        ceiling: Option<usize>,
    ) -> Result<(usize, u32), AllocError> {
        if order > MAX_ORDER {
            return Err(AllocError::SizeUnsupported);
        }
        let below = match ceiling.filter(|&ceiling| ceiling < self.base_frame + self.span) {
            None => self.zones.len(),
            Some(ceiling) if ceiling <= self.base_frame => return Err(AllocError::OutOfRange),
            Some(ceiling) => {
                if let Some(taken) = self.take_highest_under(order, ceiling, false, class)? {
                    return Ok(taken);
                }
                zone_of(&self.zones, ceiling - 1)
            }
        };
        let top = self.zones.len() - 1;
        for z in (0..below).rev() {
            if let Some((from, got)) = self.zone_largest(z, order, z < top) {
                return self
                    .take_front(z, from, got, class)
                    .map(|start| (start, got));
            }
        }
        Err(AllocError::OutOfMemory)
    }

    /// The start and order of the highest aligned run of free frames lying
    /// wholly within frames `[floor, ceiling)` of the largest order up to
    /// `order` any holds.
    ///
    /// Walks maximal free runs downward a bitmap word at a time: one step per
    /// word and per free run it passes below the ceiling, stopping at the
    /// first run holding a block of `order`.
    fn largest_free_run_under(
        &self,
        order: u32,
        ceiling: usize,
        floor: usize,
    ) -> Option<(usize, u32)> {
        let mut best: Option<(usize, u32)> = None;
        let mut end = ceiling.checked_sub(self.base_frame)?.min(self.span);
        let bottom = floor.saturating_sub(self.base_frame);
        while let Some(last) = self.last_slot_before(end, |word| !word) {
            if last < bottom {
                break;
            }
            let first = self
                .last_slot_before(last, |word| word)
                .map_or(0, |used| used + 1)
                .max(bottom);
            let (lowest, past) = (self.base_frame + first, self.base_frame + last + 1);
            // A higher run already gave as large a block as a lower one can.
            let floor = best.map_or(0, |(_, got)| got + 1);
            let block = (floor..=order).rev().find_map(|o| {
                let n = 1usize << o;
                let start = (past / n).checked_sub(1)? * n;
                (start >= lowest).then_some((start, o))
            });
            if block.is_some() {
                best = block;
            }
            if best.is_some_and(|(_, got)| got == order) {
                break;
            }
            end = first;
        }
        best
    }

    /// The highest slot below `end` whose bit `select` keeps set: `!word`
    /// finds a free slot, `word` a used one.
    fn last_slot_before(&self, end: usize, select: impl Fn(u64) -> u64) -> Option<usize> {
        let mut word = end / 64;
        let mut below = (1u64 << (end % 64)) - 1;
        loop {
            if let Some(&bits) = self.bitmap.get(word) {
                let hits = select(bits) & below;
                if hits != 0 {
                    return Some(word * 64 + (63 - hits.leading_zeros() as usize));
                }
            }
            word = word.checked_sub(1)?;
            below = u64::MAX;
        }
    }

    /// Take the free, aligned `2^order` run at `start` off the free lists,
    /// splitting the free block that encloses it around it.
    ///
    /// Population and eager merging leave every aligned all-free run inside
    /// one free block, so a run no block encloses is bookkeeping that has
    /// diverged, refused with the lists untouched.
    fn claim_free_run(&mut self, start: usize, order: u32) -> Result<(), AllocError> {
        for enclosing in order..=MAX_ORDER {
            let head = start & !((1usize << enclosing) - 1);
            if self.remove_free_block(head, enclosing)? {
                return self.split_around(head, enclosing, start, order);
            }
        }
        Err(AllocError::InvariantViolation)
    }

    /// Split the just-unlinked free block at `head` of order `from` down to
    /// the `2^to` block at `keep`, returning every other part to the lists.
    fn split_around(
        &mut self,
        mut head: usize,
        from: u32,
        keep: usize,
        to: u32,
    ) -> Result<(), AllocError> {
        let mut cur = from;
        while cur > to {
            cur -= 1;
            let half = 1usize << cur;
            if keep >= head + half {
                self.add_free_block(head, cur)?;
                head += half;
            } else {
                self.add_free_block(head + half, cur)?;
            }
        }
        Ok(())
    }

    /// Mark the claimed `2^order` block at `start` used and charge it.
    fn charge(&mut self, start: usize, order: u32, class: MemoryClass) {
        let n = 1usize << order;
        self.mark_range_used(start, n);
        // Charge every frame of the block, so a free of any part of it reads
        // its own charge back rather than trusting a caller to restate one.
        let rel = start - self.base_frame;
        self.tags[rel..rel + n].fill(FrameTag::allocated(class));
        self.class_frames[class.index()] += n;
    }

    fn free_order(&mut self, start: usize, order: u32) -> Result<(), AllocError> {
        if order > MAX_ORDER {
            return Err(AllocError::SizeUnsupported);
        }
        let n = 1usize << order;
        // Range checks: a validly-freed block lies wholly inside the usable
        // span, since it was handed out from a usable run. Checking against
        // the span (not the address-space extent) keeps every subsequent
        // `clear_bit` in bounds.
        let Some(rel) = start.checked_sub(self.base_frame) else {
            return Err(AllocError::OutOfRange);
        };
        if rel.checked_add(n).is_none_or(|e| e > self.span) {
            return Err(AllocError::OutOfRange);
        }
        if start & (n - 1) != 0 {
            return Err(AllocError::InvariantViolation);
        }
        // Every frame must be allocated (bit=1) and charged to the same
        // class, which an allocated block's frames always are. The bitmap
        // cannot tell an allocated frame from a reserved one, but the public
        // free path only ever sees frames a prior alloc handed out. Both
        // checks run over the whole range before anything is mutated, so an
        // untagged or mixed-class range — bookkeeping that has diverged — is
        // refused like a double-free rather than drifting the class totals.
        let Some(class) = self.tags[rel].class() else {
            return Err(AllocError::InvariantViolation);
        };
        for i in start..start + n {
            if !self.bit(i) {
                return Err(AllocError::InvariantViolation);
            }
            if self.tags[i - self.base_frame].class() != Some(class) {
                return Err(AllocError::InvariantViolation);
            }
        }
        self.class_frames[class.index()] = self.class_frames[class.index()].saturating_sub(n);
        self.tags[rel..rel + n].fill(FrameTag::UNTRACKED);
        for i in start..start + n {
            self.clear_bit(i);
        }

        // Try to merge with successive buddies.
        let mut cur_start = start;
        let mut cur_order = order;
        while cur_order < MAX_ORDER {
            let buddy = cur_start ^ (1usize << cur_order);
            // Refuse to merge across the end of the usable window.
            let parent_start = cur_start & !(1usize << cur_order);
            let past_end = parent_start
                .checked_sub(self.base_frame)
                .and_then(|p| p.checked_add(1usize << (cur_order + 1)))
                .is_none_or(|e| e > self.span);
            if past_end {
                break;
            }
            // Refuse to merge if any frame in the buddy half is not
            // marked free in the bitmap (covers both reserved holes and
            // currently-allocated buddies).
            if !self.block_is_free(buddy, cur_order) {
                break;
            }
            if !self.remove_free_block(buddy, cur_order)? {
                // The buddy is bitmap-free but not registered at this
                // order — it must be split into smaller blocks below us,
                // so we cannot merge.
                break;
            }
            cur_start = parent_start;
            cur_order += 1;
        }
        self.add_free_block(cur_start, cur_order)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public FrameAllocator
// ---------------------------------------------------------------------------

/// Every physical-RAM figure the allocator accounts, taken at one instant.
///
/// `usable == free + Σ class` holds of a snapshot by construction, which is
/// what a reader needs to draw where the RAM went as a partition rather than
/// as four independently-sampled numbers that need not add up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FrameSnapshot {
    /// Frames of usable RAM the allocator manages, free or handed out.
    pub usable: FrameCount,
    /// Frames still available.
    pub free: FrameCount,
    /// Frames promised to committed-but-unbacked user pages.
    pub committed: FrameCount,
    /// Frames charged to each [`MemoryClass`], indexed by discriminant.
    pub class: [FrameCount; MEMORY_CLASS_COUNT],
}

impl FrameSnapshot {
    /// Frames charged across every class — the RAM genuinely handed out.
    #[must_use]
    pub fn charged(&self) -> FrameCount {
        self.class.iter().sum()
    }
}

/// Buddy + bitmap physical frame allocator.
///
/// Constructed once at boot from a [`BootMemoryMap`]; thereafter handed
/// to subsystems through a `&FrameAllocator`. All public methods are
/// safe and use a [`SpinLock`] internally.
pub struct FrameAllocator {
    inner: SpinLock<FrameAllocatorState>,
}

impl FrameAllocator {
    /// Build a frame allocator from `map`, zoned only where RAM spans 4 GiB
    /// ([`Self::with_dma_reaches`] with no reach).
    ///
    /// # Errors
    ///
    /// As [`Self::with_dma_reaches`].
    pub fn new(map: &BootMemoryMap) -> Result<Self, AllocError> {
        Self::with_dma_reaches(map, &[])
    }

    /// Build a frame allocator from `map`, its zones cut where the DMA
    /// `reaches` — the CPU-side ranges each device's DMA can address — stop,
    /// so memory a device that reaches less needs is kept back from requests
    /// any other memory serves.
    ///
    /// Validation performed:
    ///
    /// - No region's `start + length` overflows `u64`.
    /// - No two regions overlap.
    /// - Map describes at least one usable whole frame.
    ///
    /// Reserved regions are merged into the bitmap as "used" so the
    /// allocator will never hand them out. Usable regions are rounded
    /// *inward* to whole-frame boundaries before being inserted into the
    /// buddy free lists.
    ///
    /// The frame at physical address zero is never enrolled, even when the
    /// firmware map reports it usable: under an identity direct map its
    /// translation is the null pointer, which no [`NonNull`]-based consumer
    /// (the page-table frame source, the DMA pool, an MMIO window) can
    /// represent — and because the buddy lists hand out the lowest free
    /// index first, a frame 0 that a consumer draws, cannot use, and hands
    /// back would be re-drawn by every later request, wedging allocation
    /// permanently while `free_frames` still reports plenty. On PCs it is
    /// also the real-mode IVT/BDA page. It stays marked reserved, exactly
    /// like firmware-reserved RAM, and is excluded from
    /// [`Self::usable_frames`].
    ///
    /// [`NonNull`]: core::ptr::NonNull
    ///
    /// # Errors
    ///
    /// Returns [`AllocError::InvariantViolation`] if the map is
    /// malformed (overflow or overlap) and
    /// [`AllocError::OutOfMemory`] if the map contains no usable frame.
    pub fn with_dma_reaches(
        map: &BootMemoryMap,
        reaches: &[core::ops::Range<u64>],
    ) -> Result<Self, AllocError> {
        // 1. Determine total frame count.
        let hi = map
            .highest_address()
            .ok_or(AllocError::InvariantViolation)?
            .as_u64();
        // Round up to a whole frame.
        let total_bytes = hi
            .checked_add(PAGE_SIZE as u64 - 1)
            .ok_or(AllocError::InvariantViolation)?
            & !(PAGE_SIZE as u64 - 1);
        let total_frames: usize = (total_bytes >> PAGE_SHIFT)
            .try_into()
            .map_err(|_| AllocError::InvariantViolation)?;
        if total_frames == 0 {
            return Err(AllocError::OutOfMemory);
        }

        // A frame slot can never reach the indices the link encoding
        // reserves: `total_frames` is a byte count shifted down by
        // `PAGE_SHIFT`, so the largest slot is far below
        // `tairix_inline::intrusive::MAX_INDEX`, and the link array is
        // shorter still.

        // 2. Detect overlaps by sorting by start and scanning.
        let mut sorted: Vec<_> = map.regions().to_vec();
        sorted.sort_by_key(|r| r.start.as_u64());
        for win in sorted.windows(2) {
            let a_end = win[0].end().ok_or(AllocError::InvariantViolation)?;
            if a_end.as_u64() > win[1].start.as_u64() {
                return Err(AllocError::InvariantViolation);
            }
        }

        // 3. Pre-pass over the Usable regions: collect the inward-rounded
        //    frame runs (the zero page excluded) and the `[lo, hi)` span of
        //    usable frames. The intrusive free-list arrays are sized to that
        //    span and indexed from `lo`, so a usable window at a very high
        //    physical address costs an array proportional to the RAM it
        //    covers, not one indexed from frame zero.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        for r in &sorted {
            if r.kind != RegionKind::Usable {
                continue;
            }
            let Some(end) = r.end() else {
                return Err(AllocError::InvariantViolation);
            };
            // Inward rounding: first frame fully inside region, last frame
            // fully inside region (exclusive upper bound).
            let first = usize::try_from(r.start.as_u64().div_ceil(PAGE_SIZE as u64))
                .map_err(|_| AllocError::InvariantViolation)?
                // Never enroll the zero page: its identity translation is
                // the null pointer, so a consumer that draws it must hand it
                // back, and a re-issue of it would wedge allocation forever
                // (the CI-only spawn wedge this guards against). Reserved,
                // like firmware-reserved RAM.
                .max(1);
            let last_excl = usize::try_from(end.as_u64() / PAGE_SIZE as u64)
                .map_err(|_| AllocError::InvariantViolation)?;
            if first >= last_excl {
                continue;
            }
            lo = lo.min(first);
            hi = hi.max(last_excl);
            // Adjacent regions are one run, or the buddies at their seam
            // would be populated apart and never merge.
            match runs.last_mut() {
                Some((_, prev_end)) if *prev_end == first => *prev_end = last_excl,
                _ => runs.push((first, last_excl)),
            }
        }
        if runs.is_empty() {
            return Err(AllocError::OutOfMemory);
        }
        let base_frame = lo;
        let span = hi - lo;
        let boundaries = zone_boundaries(&runs, reaches);
        let zones: Vec<Zone> = core::iter::once(lo)
            .chain(boundaries)
            .map(Zone::new)
            .collect();

        // 4. Allocate the per-frame metadata, all sized to the *usable*
        //    span and indexed from `base_frame`: the bitmap (all "1" — any
        //    frame not subsequently marked usable stays reserved), the
        //    intrusive free-list link array, and the per-block order tag.
        //    Sizing every one to the usable span rather than the
        //    address-space extent keeps the bookkeeping proportional to the
        //    RAM the machine actually has, so a usable window at a very high
        //    physical address (or a huge sparse map) costs metadata for the
        //    RAM it covers, not for the address range it sits in. These are
        //    the allocator's *only* heap use, taken once here at
        //    construction; the allocate/free paths touch no allocator but
        //    this one (see the module docs), so the kernel heap can later
        //    grow by drawing frames from here without re-entering itself.
        let words = span.div_ceil(64);
        let bitmap = vec![u64::MAX; words];
        let links = vec![Link::UNLINKED; span];
        let tags = vec![FrameTag::UNTRACKED; span];
        let mut state = FrameAllocatorState {
            total_frames,
            base_frame,
            span,
            bitmap,
            links,
            tags,
            zones,
            free_frames: 0,
            usable_frames: 0,
            reserve_frames: 0,
            committed_frames: 0,
            class_frames: [0; MEMORY_CLASS_COUNT],
        };

        // 5. Build per-frame state from the collected runs: clear the
        //    bitmap bit of every usable frame and insert the run as buddy
        //    blocks. Reserved frames keep their "used" bit.
        for &(first, last_excl) in &runs {
            for i in first..last_excl {
                state.clear_bit(i);
            }
            state.usable_frames += last_excl - first;
            state.populate_run(first, last_excl)?;
        }

        // 6. Derive the kernel reserve floor from the discovered RAM (never
        //    a fixed constant): a user commit may not draw the free pool to
        //    or below it, so the kernel keeps headroom to make progress on a
        //    machine a greedy userland is starving.
        state.reserve_frames = state.usable_frames / RESERVE_DIVISOR;

        // 7. Each zone below the highest keeps back a share of the RAM above
        //    it, bounded by its own size; nothing is allocated yet, so a
        //    zone's free frames are its usable ones.
        let mut above = 0usize;
        for zone in state.zones.iter_mut().rev() {
            zone.reserve =
                (above / ZONE_RESERVE_RATIO).min(zone.free_frames / ZONE_RESERVE_MAX_SHARE);
            above += zone.free_frames;
        }

        Ok(Self {
            inner: SpinLock::new(state),
        })
    }

    /// Allocate a single frame, charged to `class`.
    ///
    /// # Errors
    ///
    /// [`AllocError::OutOfMemory`] if no frame is available.
    pub fn alloc(&self, class: MemoryClass) -> Result<Frame, AllocError> {
        self.alloc_order(class, 0)
    }

    /// Allocate `2^order` contiguous frames, aligned to that boundary and
    /// charged to `class`.
    ///
    /// This is the **kernel-internal** path: it draws the whole free pool,
    /// including the reserve, so the kernel can always make progress (grow
    /// its heap, build page tables, service a fault) even under user memory
    /// pressure. A *user* commit must use [`Self::alloc_order_user`].
    ///
    /// # Errors
    ///
    /// - [`AllocError::SizeUnsupported`] if `order > MAX_ORDER`.
    /// - [`AllocError::OutOfMemory`] if no block of any order ≥ `order`
    ///   is available.
    pub fn alloc_order(&self, class: MemoryClass, order: u32) -> Result<Frame, AllocError> {
        let mut g = self.inner.lock();
        g.alloc_order(class, order).map(Frame)
    }

    /// Allocate a single frame on behalf of **userland** (reserve-gated),
    /// charged to `class`.
    ///
    /// # Errors
    ///
    /// [`AllocError::OutOfMemory`] if satisfying it would drop the free
    /// pool below the kernel reserve, or if no frame is available.
    pub fn alloc_user(&self, class: MemoryClass) -> Result<Frame, AllocError> {
        self.alloc_order_user(class, 0)
    }

    /// Allocate `2^order` contiguous frames on behalf of **userland**,
    /// charged to `class`, refusing when the draw would drop the free pool
    /// below the kernel reserve ([`RESERVE_DIVISOR`]).
    ///
    /// A greedy user process therefore fails closed with
    /// [`AllocError::OutOfMemory`] while the kernel still has reserved
    /// headroom to grow its heap and make progress — rather than draining
    /// physical RAM to zero and wedging the kernel. Kernel-internal callers
    /// (including heap growth) use [`Self::alloc_order`] and may draw the
    /// reserve.
    ///
    /// # Errors
    ///
    /// - [`AllocError::SizeUnsupported`] if `order > MAX_ORDER`.
    /// - [`AllocError::OutOfMemory`] if the draw would breach the reserve,
    ///   or if no block of any order ≥ `order` is available.
    pub fn alloc_order_user(&self, class: MemoryClass, order: u32) -> Result<Frame, AllocError> {
        self.alloc_order_under_user(class, order, None)
    }

    /// [`Self::alloc_order_user`] for a device that reaches only part of
    /// RAM: every frame of the block lies below `ceiling` when one is given.
    ///
    /// The block is the highest one below the ceiling, so a device reaching
    /// less keeps the memory beneath it. A ceiling above every usable frame
    /// constrains nothing and costs nothing; one inside RAM costs a search
    /// of the bitmap below it, a step per word and per free run it passes.
    ///
    /// # Errors
    ///
    /// - [`AllocError::SizeUnsupported`] if `order > MAX_ORDER`.
    /// - [`AllocError::OutOfRange`] if no usable frame lies below the ceiling.
    /// - [`AllocError::OutOfMemory`] if the draw would breach the reserve, or
    ///   no free block of the order lies below the ceiling.
    pub fn alloc_order_under_user(
        &self,
        class: MemoryClass,
        order: u32,
        ceiling: Option<PhysAddr>,
    ) -> Result<Frame, AllocError> {
        if order > MAX_ORDER {
            return Err(AllocError::SizeUnsupported);
        }
        let n = 1usize << order;
        let mut g = self.inner.lock();
        // Checked before the carve, so an eager draw never takes, even for a
        // moment, a frame the kernel reserve or a committed page is owed.
        if n > g.user_headroom() {
            return Err(AllocError::OutOfMemory);
        }
        match ceiling {
            // A ceiling past the frame index space constrains nothing.
            Some(ceiling) => {
                let ceiling = usize::try_from(ceiling.frame_index()).unwrap_or(usize::MAX);
                g.alloc_order_under(class, order, ceiling)
            }
            None => g.alloc_order(class, order),
        }
        .map(Frame)
    }

    /// Reserve physical headroom for `pages` frames of anonymous/stack user
    /// memory that the caller promises the owning task it may later touch,
    /// **without** allocating any frame yet.
    ///
    /// This is the no-overcommit admission control for demand-paged user
    /// memory: the reservation is admitted only while the free pool can
    /// still hold every already-committed page, the kernel reserve, *and*
    /// this request at once (`free_frames >= reserve_frames +
    /// committed_frames + pages`). A first touch of a committed page is
    /// therefore guaranteed a frame ([`Self::alloc_user_committed`]) — the
    /// out-of-memory refusal happens here, deterministically, as a
    /// `Result`, never as a fault-time task kill under overcommit.
    ///
    /// The caller must later balance every admitted page with exactly one
    /// of a committed fault-in ([`Self::alloc_user_committed`], as the page
    /// becomes resident) or an [`Self::uncommit`] (as the reservation is
    /// released while still unbacked), so the count returns to zero when the
    /// region is gone.
    ///
    /// # Errors
    ///
    /// [`AllocError::OutOfMemory`] when the reservation cannot be admitted
    /// without breaching the kernel reserve or a prior commitment.
    pub fn commit(&self, pages: u64) -> Result<(), AllocError> {
        let Ok(pages) = usize::try_from(pages) else {
            // More pages than the address width can count is, a fortiori,
            // more than any machine's RAM: refuse closed.
            return Err(AllocError::OutOfMemory);
        };
        if pages == 0 {
            return Ok(());
        }
        let mut g = self.inner.lock();
        if pages > g.user_headroom() {
            return Err(AllocError::OutOfMemory);
        }
        g.committed_frames += pages;
        Ok(())
    }

    /// Release `pages` frames of a commitment made by [`Self::commit`] whose
    /// pages never became resident (an unbacked reservation being torn
    /// down). Saturating, so a double release or a miscount can never wrap
    /// the counter below zero and spuriously admit a later commit.
    pub fn uncommit(&self, pages: u64) {
        let pages = usize::try_from(pages).unwrap_or(usize::MAX);
        let mut g = self.inner.lock();
        g.committed_frames = g.committed_frames.saturating_sub(pages);
    }

    /// Allocate a single frame to make a previously [`committed`](Self::commit)
    /// user page resident (the demand-fault path of anonymous/stack memory).
    ///
    /// Unlike [`Self::alloc_user`] this draws the whole pool — the frame was
    /// already reserved by the matching [`Self::commit`], so the kernel
    /// reserve is not a barrier here — and it converts one committed page
    /// from reserved to resident (decrementing the committed count,
    /// [`Self::committed_frames`]). The
    /// commitment guarantees a frame is available, so this fails only on a
    /// genuine invariant breach.
    ///
    /// The frame is charged to [`MemoryClass::UserAnon`] with no parameter: a
    /// page committed ahead of its first touch is always anonymous or stack
    /// memory. A draw admitted and taken at once, which may be of any class,
    /// is [`Self::alloc_chunks_user`].
    ///
    /// # Errors
    ///
    /// [`AllocError::OutOfMemory`] if no frame is available — which, given a
    /// matching prior commit, indicates the commitment invariant was
    /// violated rather than ordinary pressure; the caller still fails closed.
    pub fn alloc_user_committed(&self) -> Result<Frame, AllocError> {
        self.alloc_order_committed(MemoryClass::UserAnon, 0, None)
            .map(|block| block.frame)
    }

    /// Draw a `2^order` block a prior [`Self::commit`] admitted, from the
    /// whole pool, turning that many committed pages into drawn ones under
    /// the same lock, so no observer ever counts them twice.
    fn alloc_order_committed(
        &self,
        class: MemoryClass,
        order: u32,
        ceiling: Option<usize>,
    ) -> Result<FrameBlock, AllocError> {
        let mut g = self.inner.lock();
        let (start, order) = g.alloc_largest(class, order, ceiling)?;
        g.committed_frames = g.committed_frames.saturating_sub(1 << order);
        Ok(FrameBlock {
            frame: Frame(start),
            order,
        })
    }

    /// Return a block [`Self::alloc_order_committed`] drew to the
    /// commitment it was drawn against; re-charged only on a successful free.
    fn free_order_committed(&self, frame: Frame, order: u32) -> Result<(), AllocError> {
        let mut g = self.inner.lock();
        g.free_order(frame.0, order)?;
        g.committed_frames += 1 << order;
        Ok(())
    }

    /// Return a frame taken by [`Self::alloc_user_committed`] **back to its
    /// committed-but-unbacked reservation** (the fault-in unwind path):
    /// free the frame and re-charge one committed page, so a first touch
    /// that allocated a frame but then failed to install its page-table
    /// entry leaves the commitment intact and the counter undrifted. The
    /// reservation is released later, exactly once, by [`Self::uncommit`]
    /// when the region is torn down.
    ///
    /// # Errors
    ///
    /// [`AllocError::InvariantViolation`] for a double-free or a free of an
    /// unowned/misaligned frame (as [`Self::free`]); the commitment is
    /// re-charged only on a successful free.
    pub fn free_committed(&self, frame: Frame) -> Result<(), AllocError> {
        self.free_order_committed(frame, 0)
    }

    /// Frames currently committed to demand-paged user memory but not yet
    /// resident — the reserved-but-untouched headroom
    /// [`Self::commit`]/[`Self::alloc_user_committed`]/[`Self::uncommit`]
    /// track. A diagnostic and the basis of the no-overcommit gate.
    #[must_use]
    pub fn committed_frames(&self) -> FrameCount {
        self.inner.lock().committed_frames
    }

    /// Free a frame previously returned by [`Self::alloc`], discharging the
    /// class it was charged to.
    ///
    /// The class is read from the block's own head, so no caller restates it
    /// and none can mis-attribute one.
    ///
    /// # Errors
    ///
    /// [`AllocError::InvariantViolation`] for double-free, free of an
    /// unowned frame, or free of a misaligned address.
    pub fn free(&self, frame: Frame) -> Result<(), AllocError> {
        self.free_order(frame, 0)
    }

    /// Free a `2^order` block previously returned by
    /// [`Self::alloc_order`].
    ///
    /// # Errors
    ///
    /// As for [`Self::free`], plus [`AllocError::SizeUnsupported`] for
    /// `order > MAX_ORDER`.
    pub fn free_order(&self, frame: Frame, order: u32) -> Result<(), AllocError> {
        let mut g = self.inner.lock();
        g.free_order(frame.0, order)
    }

    /// Allocate `pages` frames on behalf of **userland** as a *set* of
    /// physically-contiguous buddy chunks, each of order `≤` [`MAX_ORDER`],
    /// preferring the largest block that fits the remainder so a large
    /// request costs few chunks.
    ///
    /// This is the path a cross-process shared-memory region draws its
    /// backing from: a region larger than the single-block ceiling
    /// ([`MAX_ORDER`]) is satisfied by several blocks the caller then maps
    /// into one contiguous virtual window, so the region size is bounded by
    /// available RAM rather than a fixed order, and a device a translation
    /// unit lets reach scattered frames is given its carve the same way. A
    /// request that fits one block is a single chunk (the common small case),
    /// held inline. Each chunk is `(start_frame, order)`, no larger than the
    /// one before it, so laid end to end every chunk starts at a multiple of
    /// its own size.
    ///
    /// The draw is admitted first, exactly as [`Self::commit`] admits a
    /// reservation, so a request the machine cannot spare past the kernel
    /// reserve and every committed page is refused before it takes a frame.
    /// Each block then draws the admission down as it is taken, so a
    /// concurrent commit counts the request once, never once as promised and
    /// again as drawn. When a block of the preferred order is unavailable the
    /// search steps down one order at a time, so a fragmented pool is still
    /// satisfied.
    ///
    /// On any failure every chunk already taken is returned and the
    /// admission released before the error propagates, so a failed call
    /// leaves the free pool and the commitment as it found them.
    ///
    /// # Errors
    ///
    /// - [`AllocError::SizeUnsupported`] if `pages` is zero.
    /// - [`AllocError::OutOfRange`] if no usable frame lies below `ceiling`.
    /// - [`AllocError::OutOfMemory`] if the draw cannot be admitted, no free
    ///   frame lies below `ceiling`, or the chunk-list bookkeeping cannot be
    ///   grown.
    /// - [`AllocError::InvariantViolation`] if the allocator's books are
    ///   found inconsistent part-way.
    ///
    /// Every chunk is charged to `class`, and lies wholly below `ceiling`
    /// when one is given.
    pub fn alloc_chunks_user(
        &self,
        class: MemoryClass,
        pages: u64,
        ceiling: Option<PhysAddr>,
    ) -> Result<Chunks, AllocError> {
        if pages == 0 {
            return Err(AllocError::SizeUnsupported);
        }
        self.commit(pages)?;
        let ceiling =
            ceiling.map(|ceiling| usize::try_from(ceiling.frame_index()).unwrap_or(usize::MAX));
        Self::draw_chunks(
            pages,
            |order| self.alloc_order_committed(class, order, ceiling),
            |block| {
                let _ = self.free_order_committed(block.frame, block.order);
            },
        )
        .inspect_err(|_| self.uncommit(pages))
    }

    /// Draw `pages` frames through `draw`, which answers the largest block
    /// of order up to the one asked it can, as blocks largest first, each no
    /// larger than the one before it, handing every block already taken to
    /// `release` when a draw fails.
    fn draw_chunks(
        pages: u64,
        mut draw: impl FnMut(u32) -> Result<FrameBlock, AllocError>,
        mut release: impl FnMut(FrameBlock),
    ) -> Result<Chunks, AllocError> {
        let mut out = Chunks::new();
        let mut remaining = pages;
        // A block freed elsewhere between draws could otherwise be taken
        // larger than one before it.
        let mut ceiling = MAX_ORDER;
        while remaining > 0 {
            let pushed = draw(remaining.ilog2().min(ceiling)).and_then(|block| {
                out.try_push(block).map(|()| block).map_err(|_| {
                    release(block);
                    AllocError::OutOfMemory
                })
            });
            let block = match pushed {
                Ok(block) => block,
                Err(err) => {
                    out.iter().copied().for_each(&mut release);
                    return Err(err);
                }
            };
            remaining -= 1u64 << block.order;
            ceiling = block.order;
        }
        Ok(out)
    }

    /// Return every chunk of an [`Self::alloc_chunks_user`] draw no one has
    /// reached.
    pub fn free_chunks(&self, chunks: &[FrameBlock]) {
        for block in chunks {
            let _ = self.free_order(block.frame, block.order);
        }
    }

    /// Every accounting figure, read under **one** lock acquisition.
    ///
    /// A reporter that asked separately for the whole, the free pool and each
    /// class would get figures from different instants, and the partition
    /// `usable == free + Σ class` would not hold of what it printed even
    /// though it holds of the allocator. One acquisition is what makes the
    /// reported composition a genuine whole.
    #[must_use]
    pub fn snapshot(&self) -> FrameSnapshot {
        let g = self.inner.lock();
        FrameSnapshot {
            usable: g.usable_frames,
            free: g.free_frames,
            committed: g.committed_frames,
            class: g.class_frames,
        }
    }

    /// Number of frames the allocator can still hand out.
    #[must_use]
    pub fn free_frames(&self) -> FrameCount {
        self.inner.lock().free_frames
    }

    /// Total frames the allocator is aware of (usable + reserved + holes).
    ///
    /// This is an *address-space* extent (bitmap size), not a RAM size:
    /// it spans from physical address zero to the highest mapped address,
    /// including reserved regions and holes. For the amount of RAM the
    /// system actually has, use [`Self::usable_frames`].
    #[must_use]
    pub fn total_frames(&self) -> FrameCount {
        self.inner.lock().total_frames
    }

    /// Number of whole frames of usable RAM the allocator manages —
    /// frames inside `Usable` boot-map regions, whether currently free or
    /// handed out. Excludes reserved regions, physical-address holes, and
    /// the permanently reserved zero page (see [`Self::new`]), so
    /// `usable_frames() - free_frames()` is the memory genuinely in use.
    #[must_use]
    pub fn usable_frames(&self) -> FrameCount {
        self.inner.lock().usable_frames
    }

    /// The kernel reserve floor, in frames: the free-pool level a user
    /// commit ([`Self::alloc_user`] / [`Self::alloc_order_user`]) may not
    /// draw below. Derived from discovered RAM
    /// (`usable_frames / RESERVE_DIVISOR`), fixed after construction.
    #[must_use]
    pub fn reserve_frames(&self) -> FrameCount {
        self.inner.lock().reserve_frames
    }

    /// Invoke `f(base, len)` once for every maximal run of currently-**free**
    /// physical frames, in ascending address order — `base` is the run's
    /// physical start address and `len` its byte length.
    ///
    /// This is the single authority on which physical RAM is safe to
    /// overwrite. A frame is reported only when the allocator has neither
    /// handed it out nor reserved it: every in-use frame (the kernel image
    /// and page tables, the heap, DMA buffers, driver and userland memory),
    /// every reserved region, and every physical-address hole is *excluded*,
    /// because the bitmap marks all of them used.
    ///
    /// The one caller is the pre-boot Supervisor's whole-RAM `memtest`
    /// takeover, which must test only free RAM: writing an in-use frame — a
    /// DMA buffer a device still maps non-cacheably, say — races its owner
    /// and can wedge the machine. Sweeping the free set is the honest
    /// maximum-safe coverage, exactly as a running memtest86 cannot test its
    /// own resident working set.
    ///
    /// `f` runs under the allocator lock, so it must not re-enter the
    /// allocator; it is expected only to copy each `(base, len)` into a
    /// caller-owned buffer.
    pub fn for_each_free_region(&self, mut f: impl FnMut(PhysAddr, u64)) {
        let state = self.inner.lock();
        let base = state.base_frame;
        let span = state.span;
        // Absolute start frame of the free run currently being accumulated.
        let mut run_start: Option<usize> = None;
        let mut rel = 0usize;
        while rel < span {
            let word = state.bitmap[rel / 64];
            let end_rel = (rel + 64).min(span);
            if word == 0 {
                // Whole word free: extend (or open) the current run and skip
                // the rest of the word in one step.
                if run_start.is_none() {
                    run_start = Some(base + rel);
                }
            } else if word == u64::MAX {
                // Whole word used: close any open run at the word boundary.
                if let Some(s) = run_start.take() {
                    emit_free_run(&mut f, s, base + rel);
                }
            } else {
                // Mixed word: decide each frame individually.
                for r in rel..end_rel {
                    if (word >> (r % 64)) & 1 == 1 {
                        if let Some(s) = run_start.take() {
                            emit_free_run(&mut f, s, base + r);
                        }
                    } else if run_start.is_none() {
                        run_start = Some(base + r);
                    }
                }
            }
            rel = end_rel;
        }
        if let Some(s) = run_start {
            emit_free_run(&mut f, s, base + span);
        }
    }
}

/// Report the free frame run `[start_frame, end_frame)` to `f` as a physical
/// `(base, byte_len)` pair.
fn emit_free_run<F: FnMut(PhysAddr, u64)>(f: &mut F, start_frame: usize, end_frame: usize) {
    let base = PhysAddr::new((start_frame as u64) << PAGE_SHIFT);
    let len = ((end_frame - start_frame) as u64) << PAGE_SHIFT;
    f(base, len);
}

// SAFETY: All shared mutable state lives behind the internal `SpinLock`,
// which already implements `Send`/`Sync` for its protected payload. The
// allocator itself contains no other interior mutability.
unsafe impl Sync for FrameAllocator {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::bootinfo::MemoryRegion;

    extern crate std;
    use std::format;
    use std::vec::Vec;

    /// A map whose single usable region starts at frame 16 — clear of the
    /// permanently reserved zero page, and aligned for the largest order
    /// the merge test draws — so `usable_pages` frames enter the pool.
    fn small_map(usable_pages: usize) -> BootMemoryMap {
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new((16 * PAGE_SIZE) as u64),
            length: (usable_pages * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        m
    }

    #[test]
    fn page_size_matches_shift() {
        assert_eq!(1usize << PAGE_SHIFT, PAGE_SIZE);
    }

    #[test]
    fn phys_addr_frame_index_truncates() {
        assert_eq!(PhysAddr::new(0).frame_index(), 0);
        assert_eq!(PhysAddr::new(PAGE_SIZE as u64 - 1).frame_index(), 0);
        assert_eq!(PhysAddr::new(PAGE_SIZE as u64).frame_index(), 1);
    }

    #[test]
    fn frame_start_round_trip() {
        let f = Frame(7);
        assert_eq!(Frame::containing(f.start()), f);
    }

    #[test]
    fn new_rejects_empty_map() {
        let m = BootMemoryMap::new();
        assert_eq!(FrameAllocator::new(&m).err(), Some(AllocError::OutOfMemory));
    }

    #[test]
    fn new_rejects_overlap() {
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new(0),
            length: 0x4000,
            kind: RegionKind::Usable,
        });
        m.push(MemoryRegion {
            start: PhysAddr::new(0x2000),
            length: 0x4000,
            kind: RegionKind::Usable,
        });
        assert_eq!(
            FrameAllocator::new(&m).err(),
            Some(AllocError::InvariantViolation)
        );
    }

    #[test]
    fn alloc_then_free_returns_same_frame() {
        let m = small_map(8);
        let a = FrameAllocator::new(&m).unwrap();
        let f = a.alloc(MemoryClass::Kernel).unwrap();
        assert!(a.free_frames() < 8);
        a.free(f).unwrap();
        assert_eq!(a.free_frames(), 8);
    }

    #[test]
    fn re_registering_a_free_block_is_refused_rather_than_double_counted() {
        let m = small_map(8);
        let a = FrameAllocator::new(&m).unwrap();
        let mut state = a.inner.lock();

        // Slot 0 is the region's first frame, so the populate pass registered
        // it as a free-block head at whatever order it could place there.
        let order = state.tags[0]
            .free_order()
            .expect("populated as a free head");
        assert!(state.links[0].is_linked());
        let free = state.free_frames;
        let base = state.base_frame;

        assert_eq!(
            state.add_free_block(base, order),
            Err(AllocError::InvariantViolation)
        );
        assert_eq!(state.free_frames, free);
    }

    #[test]
    fn an_order_tag_without_a_link_refuses_the_unlink() {
        let m = small_map(8);
        let a = FrameAllocator::new(&m).unwrap();
        let mut state = a.inner.lock();

        // Tag a slot that is on no list as an order-0 head, so the tag and the
        // links disagree exactly as a stray write would leave them.
        let victim = state.span - 1;
        assert!(!state.links[victim].is_linked());
        state.tags[victim] = FrameTag::free_head(0);
        let frame = state.base_frame + victim;
        let free = state.free_frames;

        assert_eq!(
            state.remove_free_block(frame, 0),
            Err(AllocError::InvariantViolation)
        );
        assert_eq!(state.free_frames, free);
    }

    #[test]
    fn a_front_block_whose_order_tag_disagrees_refuses_the_allocation() {
        let m = small_map(8);
        let a = FrameAllocator::new(&m).unwrap();
        {
            let mut state = a.inner.lock();
            // The registered head still fronts its order's list, but its tag
            // now claims a different order, so no order can pop it. Before the
            // free lists refused this, the mismatch was a release-mode
            // `debug_assert` and the frame was handed out anyway.
            let wrong = state.tags[0].free_order().expect("a free head") + 1;
            assert!(wrong <= MAX_ORDER);
            state.tags[0] = FrameTag::free_head(wrong);
        }

        assert_eq!(
            a.alloc(MemoryClass::Kernel).err(),
            Some(AllocError::InvariantViolation)
        );
    }

    #[test]
    fn for_each_free_region_reports_only_currently_free_frames() {
        let m = small_map(8);
        let a = FrameAllocator::new(&m).unwrap();
        let base = (16 * PAGE_SIZE) as u64;
        let p = PAGE_SIZE as u64;

        // Every frame free: one contiguous run over the whole region.
        let mut runs = Vec::new();
        a.for_each_free_region(|b, l| runs.push((b.as_u64(), l)));
        assert_eq!(runs, std::vec![(base, 8 * p)]);

        // Hand out every frame, then return two non-adjacent ones (by address)
        // so the free set is two isolated single-frame runs and everything
        // handed out is excluded.
        let f: Vec<_> = (0..8)
            .map(|_| a.alloc(MemoryClass::Kernel).unwrap())
            .collect();
        for &k in &[1u64, 5] {
            let target = base + k * p;
            let fr = *f.iter().find(|fr| fr.start().as_u64() == target).unwrap();
            a.free(fr).unwrap();
        }
        runs.clear();
        a.for_each_free_region(|b, l| runs.push((b.as_u64(), l)));
        assert_eq!(runs, std::vec![(base + p, p), (base + 5 * p, p)]);
    }

    #[test]
    fn alloc_order_returns_aligned_block() {
        let m = small_map(16);
        let a = FrameAllocator::new(&m).unwrap();
        let blk = a.alloc_order(MemoryClass::Kernel, 3).unwrap(); // 8 frames
        assert_eq!(blk.0 & 7, 0);
        a.free_order(blk, 3).unwrap();
    }

    #[test]
    fn oom_when_exhausted() {
        let m = small_map(4);
        let a = FrameAllocator::new(&m).unwrap();
        let mut held = Vec::new();
        while let Ok(f) = a.alloc(MemoryClass::Kernel) {
            held.push(f);
        }
        assert_eq!(held.len(), 4);
        assert_eq!(
            a.alloc(MemoryClass::Kernel).err(),
            Some(AllocError::OutOfMemory)
        );
        for f in held {
            a.free(f).unwrap();
        }
        assert_eq!(a.free_frames(), 4);
    }

    #[test]
    fn order_too_large_is_unsupported() {
        let m = small_map(4);
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(
            a.alloc_order(MemoryClass::Kernel, MAX_ORDER + 1).err(),
            Some(AllocError::SizeUnsupported)
        );
    }

    /// Total frames across a chunk list.
    fn chunk_frames(chunks: &[FrameBlock]) -> u64 {
        chunks.iter().map(|block| 1u64 << block.order).sum()
    }

    #[test]
    fn alloc_chunks_rejects_zero() {
        let m = small_map(4);
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(
            a.alloc_chunks_user(MemoryClass::Kernel, 0, None).err(),
            Some(AllocError::SizeUnsupported)
        );
    }

    #[test]
    fn alloc_chunks_of_a_power_of_two_is_a_single_block() {
        let m = small_map(16);
        let a = FrameAllocator::new(&m).unwrap();
        // Four frames is one order-2 block: the common small case stays a
        // single chunk (so the USB URB-buffer path is unaffected).
        let chunks = a.alloc_chunks_user(MemoryClass::Kernel, 4, None).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].order, 2);
        assert_eq!(a.free_frames(), 16 - 4);
        for block in chunks {
            a.free_order(block.frame, block.order).unwrap();
        }
        assert_eq!(a.free_frames(), 16);
    }

    /// A ceilinged draw takes every chunk below it though frames above are
    /// free, and is refused where no usable frame lies below it.
    #[test]
    fn alloc_chunks_under_a_ceiling_lie_wholly_below_it() {
        let m = small_map(64);
        let a = FrameAllocator::new(&m).unwrap();
        let floor = (16 * PAGE_SIZE) as u64;
        let ceiling = PhysAddr::new(floor + (16 * PAGE_SIZE) as u64);
        let unbounded = a.alloc_chunks_user(MemoryClass::Dma, 6, None).unwrap();
        assert!(
            unbounded.iter().any(|block| block.frame.start() >= ceiling),
            "an unbounded draw reaches above the ceiling"
        );
        a.free_chunks(&unbounded);
        let free = a.free_frames();
        let chunks = a
            .alloc_chunks_user(MemoryClass::Dma, 6, Some(ceiling))
            .unwrap();
        assert_eq!(chunk_frames(&chunks), 6);
        for block in &chunks {
            let end = block.frame.start().as_u64() + (PAGE_SIZE << block.order) as u64;
            assert!(end <= ceiling.as_u64(), "a chunk ends at {end:#x}");
        }
        assert_eq!(
            a.alloc_chunks_user(MemoryClass::Dma, 1, Some(PhysAddr::new(floor)))
                .err(),
            Some(AllocError::OutOfRange)
        );
        a.free_chunks(&chunks);
        assert_eq!(a.free_frames(), free, "the refusal took and kept nothing");
    }

    const MIB: u64 = 1 << 20;

    /// Usable RAM from `start` to `end` MiB, in one region.
    fn map_between(start: u64, end: u64) -> BootMemoryMap {
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new(start * MIB),
            length: (end - start) * MIB,
            kind: RegionKind::Usable,
        });
        m
    }

    /// Frames in one 32 MiB largest block, the zone granule.
    const ZONE_GRANULE: usize = 1 << MAX_ORDER;

    /// A device reaching the lowest 64 MiB.
    const LOW_REACH: core::ops::Range<u64> = 0..64 * MIB;

    #[test]
    fn a_busy_pool_keeps_the_memory_below_a_dma_limit_for_the_carve_that_needs_it() {
        let map = map_between(32, 128);
        let ceiling = PhysAddr::new(64 * MIB);
        // Churn leaves freed low memory heading the address-blind lists; then
        // ordinary requests fill two thirds of the machine.
        let carve_after_churn = |a: &FrameAllocator| {
            let mut held = Vec::new();
            while let Ok(frame) = a.alloc(MemoryClass::Kernel) {
                held.push(frame);
            }
            held.sort_unstable_by_key(|frame| core::cmp::Reverse(frame.0));
            for frame in held {
                a.free(frame).unwrap();
            }
            for _ in 0..2 * ZONE_GRANULE {
                a.alloc(MemoryClass::Kernel).unwrap();
            }
            a.alloc_order_under_user(MemoryClass::Dma, 0, Some(ceiling))
        };
        let unzoned = FrameAllocator::new(&map).unwrap();
        assert_eq!(
            carve_after_churn(&unzoned).err(),
            Some(AllocError::OutOfMemory),
            "with one zone the low memory goes first"
        );
        let zoned = FrameAllocator::with_dma_reaches(&map, &[LOW_REACH]).unwrap();
        let frame = carve_after_churn(&zoned).expect("the zone below the limit is kept");
        assert!(frame.start() < ceiling);
    }

    #[test]
    fn ordinary_requests_take_the_highest_zone_first_and_a_lower_one_down_to_its_reserve() {
        let a = FrameAllocator::with_dma_reaches(&map_between(32, 128), &[LOW_REACH]).unwrap();
        for _ in 0..2 * ZONE_GRANULE {
            assert!(a.alloc(MemoryClass::Kernel).unwrap().start().as_u64() >= 64 * MIB);
        }
        let reserve = 2 * ZONE_GRANULE / ZONE_RESERVE_RATIO;
        for _ in 0..ZONE_GRANULE - reserve {
            assert!(a.alloc(MemoryClass::Kernel).unwrap().start().as_u64() < 64 * MIB);
        }
        assert_eq!(
            a.alloc(MemoryClass::Kernel).err(),
            Some(AllocError::OutOfMemory),
            "the reserve is not an ordinary request's"
        );
        assert_eq!(a.free_frames(), reserve);
    }

    #[test]
    fn a_carve_drains_its_own_zone_but_leaves_each_lower_zone_its_reserve() {
        let a = FrameAllocator::with_dma_reaches(&map_between(32, 160), &[LOW_REACH, 0..96 * MIB])
            .unwrap();
        let carve = |limit: u64| {
            a.alloc_order_under_user(MemoryClass::Dma, 0, Some(PhysAddr::new(limit * MIB)))
        };
        let lowest_reserve = 3 * ZONE_GRANULE / ZONE_RESERVE_RATIO;
        for _ in 0..2 * ZONE_GRANULE - lowest_reserve {
            assert!(carve(96).unwrap().start().as_u64() < 96 * MIB);
        }
        assert_eq!(carve(96).err(), Some(AllocError::OutOfMemory));
        for _ in 0..lowest_reserve {
            assert!(carve(64).unwrap().start().as_u64() < 64 * MIB);
        }
        assert_eq!(carve(64).err(), Some(AllocError::OutOfMemory));
    }

    #[test]
    fn a_zone_boundary_is_drawn_only_where_usable_ram_lies_on_both_sides_of_a_limit() {
        let bytes = |frames: usize| (frames * PAGE_SIZE) as u64;
        let runs = [(ZONE_GRANULE, 4 * ZONE_GRANULE)];
        assert_eq!(
            zone_boundaries(
                &runs,
                &[0..bytes(2 * ZONE_GRANULE + 1), 0..bytes(2 * ZONE_GRANULE),]
            ),
            [2 * ZONE_GRANULE],
            "aligned down to the largest block, and one boundary per limit"
        );
        assert!(
            zone_boundaries(
                &runs,
                &[
                    bytes(5 * ZONE_GRANULE)..bytes(6 * ZONE_GRANULE),
                    0..bytes(8 * ZONE_GRANULE),
                ]
            )
            .is_empty(),
            "a window holding no RAM, or a limit above it all, cuts nothing"
        );
        let four_gib = usize::try_from(DMA32_LIMIT >> PAGE_SHIFT).unwrap();
        assert_eq!(
            zone_boundaries(&[(ZONE_GRANULE, four_gib + ZONE_GRANULE)], &[]),
            [four_gib]
        );
        assert!(zone_boundaries(&runs, &[]).is_empty());
    }

    #[test]
    fn a_freed_block_returns_to_its_own_zone() {
        let a = FrameAllocator::with_dma_reaches(&map_between(32, 128), &[LOW_REACH]).unwrap();
        let zone_free = |a: &FrameAllocator| {
            a.inner
                .lock()
                .zones
                .iter()
                .map(|zone| zone.free_frames)
                .collect::<Vec<_>>()
        };
        let low = a
            .alloc_order_under_user(MemoryClass::Dma, 3, Some(PhysAddr::new(64 * MIB)))
            .unwrap();
        assert_eq!(zone_free(&a), [ZONE_GRANULE - 8, 2 * ZONE_GRANULE]);
        a.free_order(low, 3).unwrap();
        assert_eq!(zone_free(&a), [ZONE_GRANULE, 2 * ZONE_GRANULE]);
    }

    #[test]
    fn alloc_chunks_splits_a_non_power_of_two_into_descending_blocks() {
        let m = small_map(16);
        let a = FrameAllocator::new(&m).unwrap();
        // Six frames = one order-2 block (4) + one order-1 block (2), largest
        // first.
        let chunks = a.alloc_chunks_user(MemoryClass::Kernel, 6, None).unwrap();
        assert_eq!(chunk_frames(&chunks), 6);
        assert_eq!(
            chunks.iter().map(|block| block.order).collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(a.free_frames(), 16 - 6);
        for block in chunks {
            a.free_order(block.frame, block.order).unwrap();
        }
        assert_eq!(a.free_frames(), 16);
    }

    #[test]
    fn alloc_chunks_spans_more_than_one_max_order_block() {
        // A request larger than a single order-`MAX_ORDER` block must be
        // satisfied by several blocks — the whole point of the chunked
        // backing that removes the shared-region ceiling. Sized relative to
        // `MAX_ORDER` so it stays a genuine multi-block request whatever the
        // order ceiling is: one-and-a-half of the largest block.
        let max_block: usize = 1 << MAX_ORDER;
        let want_pages = max_block + max_block / 2;
        // Room for the kernel reserve on top of the draw.
        let total = want_pages + want_pages / 32;
        let m = small_map(total);
        let a = FrameAllocator::new(&m).unwrap();
        let want = want_pages as u64;
        let chunks = a
            .alloc_chunks_user(MemoryClass::Kernel, want, None)
            .unwrap();
        assert!(chunks.len() >= 2, "must span multiple blocks");
        assert!(chunks.iter().all(|block| block.order <= MAX_ORDER));
        assert_eq!(chunk_frames(&chunks), want);
        assert_eq!(a.free_frames(), total - want_pages);
        for block in chunks {
            a.free_order(block.frame, block.order).unwrap();
        }
        assert_eq!(a.free_frames(), total);
    }

    #[test]
    fn alloc_chunks_of_a_fragmented_pool_never_grows_a_chunk() {
        let m = small_map(16);
        let a = FrameAllocator::new(&m).unwrap();
        let singles: Vec<Frame> = (0..16)
            .map(|_| a.alloc(MemoryClass::Kernel).unwrap())
            .collect();
        // Free the first half whole and every other frame of the rest, so the
        // largest free block sits first and only singles follow it.
        for (i, &frame) in singles.iter().enumerate() {
            if i < 8 || i % 2 == 0 {
                a.free(frame).unwrap();
            }
        }
        let chunks = a.alloc_chunks_user(MemoryClass::Kernel, 11, None).unwrap();
        assert_eq!(chunk_frames(&chunks), 11);
        let orders: Vec<u32> = chunks.iter().map(|block| block.order).collect();
        assert_eq!(orders, [3, 0, 0, 0]);
        a.free_chunks(&chunks);
        assert_eq!(a.free_frames(), 12);
    }

    #[test]
    fn alloc_chunks_is_refused_before_it_takes_a_frame_the_machine_cannot_spare() {
        let a = FrameAllocator::new(&small_map(4 * RESERVE_DIVISOR)).unwrap();
        let reserve = a.reserve_frames() as u64;
        assert!(reserve > 0);
        a.commit(3).unwrap();
        let spare = 4 * RESERVE_DIVISOR as u64 - reserve - 3;
        assert_eq!(
            a.alloc_chunks_user(MemoryClass::Dma, spare + 1, None).err(),
            Some(AllocError::OutOfMemory)
        );
        assert_eq!(a.free_frames(), 4 * RESERVE_DIVISOR);
        assert_eq!(a.committed_frames(), 3);
        let chunks = a.alloc_chunks_user(MemoryClass::Dma, spare, None).unwrap();
        assert_eq!(chunk_frames(&chunks), spare);
        assert_eq!(a.committed_frames(), 3, "the draw spent its own admission");
        assert_eq!(a.commit(1), Err(AllocError::OutOfMemory));
        a.free_chunks(&chunks);
        a.uncommit(3);
        assert_eq!(a.free_frames(), 4 * RESERVE_DIVISOR);
        assert_eq!(a.committed_frames(), 0);
    }

    /// A concurrent commit sees an admitted draw counted once at every block,
    /// never once as promised and again as drawn.
    #[test]
    fn an_admitted_draw_is_counted_once_while_it_runs() {
        let a = FrameAllocator::new(&small_map(4 * RESERVE_DIVISOR)).unwrap();
        let headroom = |a: &FrameAllocator| {
            let figures = a.snapshot();
            figures.free - a.reserve_frames() - figures.committed
        };
        a.commit(7).unwrap();
        let before = headroom(&a);
        let mut seen = Vec::new();
        let chunks = FrameAllocator::draw_chunks(
            7,
            |order| {
                let drawn = a.alloc_order_committed(MemoryClass::Dma, order, None);
                seen.push(headroom(&a));
                drawn
            },
            |_| unreachable!("nothing fails"),
        )
        .unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(seen, [before; 3]);
        assert_eq!(a.committed_frames(), 0);
        a.free_chunks(&chunks);
    }

    /// A block freed elsewhere between draws never lets a later block grow
    /// past the one before it, so laid end to end each stays aligned to its
    /// own size.
    #[test]
    fn a_draw_never_takes_a_block_larger_than_the_one_before() {
        let mut next = 0;
        let mut draws = 0;
        let chunks = FrameAllocator::draw_chunks(
            7,
            |order| {
                draws += 1;
                // Only the first draw finds the pool short of large blocks.
                let order = if draws == 1 { order.min(1) } else { order };
                next += 1 << order;
                Ok(FrameBlock {
                    frame: Frame(next),
                    order,
                })
            },
            |_| unreachable!("nothing fails"),
        )
        .unwrap();
        let orders: Vec<u32> = chunks.iter().map(|block| block.order).collect();
        assert_eq!(orders, [1, 1, 1, 0]);
    }

    /// A draw that fails part-way hands back every block it took, and the
    /// committed draw's release re-charges what each block spent, so the
    /// admission's own release leaves the books as they started.
    #[test]
    fn a_failed_draw_returns_every_block_and_its_admission() {
        let a = FrameAllocator::new(&small_map(64)).unwrap();
        a.commit(7).unwrap();
        let mut draws = 0;
        let refused = FrameAllocator::draw_chunks(
            7,
            |order| {
                draws += 1;
                if draws == 3 {
                    return Err(AllocError::InvariantViolation);
                }
                a.alloc_order_committed(MemoryClass::Dma, order, None)
            },
            |block| a.free_order_committed(block.frame, block.order).unwrap(),
        );
        assert_eq!(refused.err(), Some(AllocError::InvariantViolation));
        assert_eq!(a.committed_frames(), 7);
        a.uncommit(7);
        assert_eq!(a.free_frames(), 64);
        assert_eq!(a.committed_frames(), 0);
    }

    #[test]
    fn double_free_detected() {
        let m = small_map(4);
        let a = FrameAllocator::new(&m).unwrap();
        let f = a.alloc(MemoryClass::Kernel).unwrap();
        a.free(f).unwrap();
        assert_eq!(a.free(f).err(), Some(AllocError::InvariantViolation));
    }

    #[test]
    fn free_out_of_range() {
        let m = small_map(4);
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(a.free(Frame(9999)).err(), Some(AllocError::OutOfRange));
    }

    #[test]
    fn free_misaligned_order_block() {
        let m = small_map(16);
        let a = FrameAllocator::new(&m).unwrap();
        // Frame 17 cannot be the start of an order-1 block (needs alignment 2).
        assert_eq!(
            a.free_order(Frame(17), 1).err(),
            Some(AllocError::InvariantViolation)
        );
    }

    #[test]
    fn reserved_region_not_handed_out() {
        let mut m = BootMemoryMap::new();
        // Frames 0..4 usable, 4..8 reserved, 8..16 usable.
        m.push(MemoryRegion {
            start: PhysAddr::new(0),
            length: (4 * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        m.push(MemoryRegion {
            start: PhysAddr::new((4 * PAGE_SIZE) as u64),
            length: (4 * PAGE_SIZE) as u64,
            kind: RegionKind::Reserved,
        });
        m.push(MemoryRegion {
            start: PhysAddr::new((8 * PAGE_SIZE) as u64),
            length: (8 * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        let a = FrameAllocator::new(&m).unwrap();
        let mut handed = Vec::new();
        while let Ok(f) = a.alloc(MemoryClass::Kernel) {
            // No frame in [4,8) — nor the reserved zero page — may ever be
            // handed out.
            assert!(!(4..8).contains(&f.0), "reserved frame {} handed out", f.0);
            assert_ne!(f.0, 0, "the zero page must never be handed out");
            handed.push(f);
        }
        assert_eq!(handed.len(), 11);
    }

    /// Regression: the zero page never enters circulation even when the
    /// firmware map reports it usable. Before the fix, frame 0's identity
    /// translation (the null pointer) made the page-table frame source hand
    /// it back, and the lowest-first buddy pop re-issued it to every later
    /// request — wedging `spawn` permanently with tens of thousands of
    /// frames still free (the CI-only spawn-session failure).
    #[test]
    fn zero_page_is_reserved_even_when_firmware_reports_it_usable() {
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new(0),
            length: (8 * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(a.usable_frames(), 7, "the zero page is not usable RAM");
        assert_eq!(a.free_frames(), 7);
        let mut handed = Vec::new();
        while let Ok(f) = a.alloc(MemoryClass::Kernel) {
            assert_ne!(f.0, 0, "the zero page must never be handed out");
            handed.push(f);
        }
        assert_eq!(handed.len(), 7);
    }

    /// A map whose only usable frame is the zero page holds no RAM the
    /// allocator may hand out, so construction fails closed.
    #[test]
    fn map_of_only_the_zero_page_has_no_usable_frame() {
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new(0),
            length: PAGE_SIZE as u64,
            kind: RegionKind::Usable,
        });
        assert_eq!(FrameAllocator::new(&m).err(), Some(AllocError::OutOfMemory));
    }

    #[test]
    fn usable_frames_excludes_holes_and_reserved() {
        // The QEMU-virt shape that broke the login screen's memory line:
        // RAM sits above a large MMIO hole, so the address-space extent
        // (total_frames) dwarfs the actual RAM. usable_frames must count
        // only the usable regions, and stay fixed across alloc/free.
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new((1024 * PAGE_SIZE) as u64),
            length: (4 * PAGE_SIZE) as u64,
            kind: RegionKind::Reserved,
        });
        m.push(MemoryRegion {
            start: PhysAddr::new((1028 * PAGE_SIZE) as u64),
            length: (16 * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(a.total_frames(), 1044);
        assert_eq!(a.usable_frames(), 16);
        assert_eq!(a.free_frames(), 16);
        let f = a.alloc(MemoryClass::Kernel).unwrap();
        assert_eq!(a.usable_frames(), 16);
        assert_eq!(a.free_frames(), 15);
        a.free(f).unwrap();
        assert_eq!(a.usable_frames(), 16);
        assert_eq!(a.free_frames(), 16);
    }

    #[test]
    fn usable_window_at_a_high_base_uses_span_sized_metadata() {
        // Regression: the intrusive free-list arrays are indexed from the
        // lowest usable frame, not from frame zero, so a usable window at a
        // high physical base costs metadata proportional to the window, not
        // to its address. A region of 32 frames based ~4 GiB up must build,
        // allocate, split, merge, and account exactly like a low-based one —
        // this exercises the `base_frame` offset (`slot`) on every path.
        let base = 1_000_000usize; // ~3.8 GiB up, far from frame 0
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new((base * PAGE_SIZE) as u64),
            length: (32 * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(a.usable_frames(), 32);
        assert_eq!(a.free_frames(), 32);

        // Order-2 blocks split from the population, then merge back.
        let b0 = a.alloc_order(MemoryClass::Kernel, 2).unwrap();
        let b1 = a.alloc_order(MemoryClass::Kernel, 2).unwrap();
        assert!(
            b0.0 >= base && b1.0 >= base,
            "blocks lie in the high window"
        );
        assert_eq!(b0.0 & 3, 0, "order-2 block is 4-frame aligned");
        a.free_order(b0, 2).unwrap();
        a.free_order(b1, 2).unwrap();
        assert_eq!(a.free_frames(), 32);

        // Drain to single frames (exercises the slot offset per frame),
        // then free them all back and coalesce to the largest block.
        let mut held = Vec::new();
        while let Ok(f) = a.alloc(MemoryClass::Kernel) {
            assert!(f.0 >= base && f.0 < base + 32);
            held.push(f);
        }
        assert_eq!(held.len(), 32);
        for f in held {
            a.free(f).unwrap();
        }
        assert_eq!(a.free_frames(), 32);
        let big = a.alloc_order(MemoryClass::Kernel, 5).unwrap(); // 32 frames
        assert_eq!(big.0, base, "the whole window coalesced back");
        a.free_order(big, 5).unwrap();
        assert_eq!(a.free_frames(), 32);
    }

    #[test]
    fn alloc_user_stops_at_reserve_but_kernel_may_draw_it() {
        // Enough usable RAM for a non-zero reserve
        // (`usable / RESERVE_DIVISOR`); `small_map` bases the window clear of
        // the zero page, so all `usable` frames are usable.
        let usable = 4 * RESERVE_DIVISOR;
        let m = small_map(usable);
        let a = FrameAllocator::new(&m).unwrap();
        let reserve = a.reserve_frames();
        assert_eq!(reserve, usable / RESERVE_DIVISOR);
        assert!(reserve > 0, "test needs a non-zero reserve");

        // User commits succeed until one more would drop the free pool below
        // the reserve, the same budget a commit is held to; the last success
        // leaves exactly `reserve`.
        let mut held = Vec::new();
        while let Ok(f) = a.alloc_user(MemoryClass::UserAnon) {
            held.push(f);
        }
        assert_eq!(a.free_frames(), reserve);
        assert_eq!(
            a.alloc_user(MemoryClass::UserAnon).err(),
            Some(AllocError::OutOfMemory)
        );

        // The kernel-internal path may draw into the reserve, all the way to
        // exhaustion — the kernel always keeps the ability to make progress.
        while let Ok(f) = a.alloc(MemoryClass::Kernel) {
            held.push(f);
        }
        assert_eq!(a.free_frames(), 0);

        for f in held {
            a.free(f).unwrap();
        }
        assert_eq!(a.free_frames(), usable);
    }

    #[test]
    fn commit_reserves_headroom_and_refuses_the_overcommit() {
        // A commit is admitted only while the free pool can still hold the
        // kernel reserve, every prior commitment, and this request at once.
        let usable = 4 * RESERVE_DIVISOR;
        let m = small_map(usable);
        let a = FrameAllocator::new(&m).unwrap();
        let reserve = a.reserve_frames();
        assert!(reserve > 0, "test needs a non-zero reserve");

        // The whole non-reserve pool may be committed, one page at a time,
        // but not one page more: the last admissible commit leaves exactly
        // `reserve` frames of guaranteed kernel headroom uncommitted.
        let committable = usable - reserve;
        for _ in 0..committable {
            a.commit(1).expect("within the no-overcommit budget");
        }
        assert_eq!(a.committed_frames(), committable);
        assert_eq!(
            a.commit(1).err(),
            Some(AllocError::OutOfMemory),
            "one page past the budget is refused as a Result, not overcommitted"
        );
        // A single bulk request for the whole budget behaves identically.
        a.uncommit(committable as u64);
        assert_eq!(a.committed_frames(), 0);
        a.commit(committable as u64)
            .expect("bulk commit of the budget");
        assert_eq!(
            a.commit(1).err(),
            Some(AllocError::OutOfMemory),
            "the budget is the budget however it is split"
        );
    }

    #[test]
    fn committed_pages_are_guaranteed_a_frame_and_eager_draws_respect_them() {
        // Every committed page can always be made resident, and an eager
        // user draw can never steal a committed page's reserved frame.
        let usable = 4 * RESERVE_DIVISOR;
        let m = small_map(usable);
        let a = FrameAllocator::new(&m).unwrap();
        let reserve = a.reserve_frames();
        let committable = usable - reserve;

        // Commit the entire non-reserve budget up front.
        a.commit(committable as u64).expect("commit the budget");
        // An eager user allocation must now be refused outright: every
        // non-reserve frame is promised to a committed page.
        assert_eq!(
            a.alloc_user(MemoryClass::UserAnon).err(),
            Some(AllocError::OutOfMemory),
            "an eager draw cannot dip into committed headroom"
        );
        // Yet every committed page can still be faulted in — the frames were
        // reserved for exactly this — until the commitment is exhausted.
        let mut held = Vec::new();
        for _ in 0..committable {
            held.push(
                a.alloc_user_committed()
                    .expect("committed page is guaranteed a frame"),
            );
        }
        assert_eq!(
            a.committed_frames(),
            0,
            "every commitment converted to residency"
        );
        assert_eq!(a.free_frames(), reserve, "only the kernel reserve remains");

        for f in held {
            a.free(f).unwrap();
        }
        assert_eq!(a.free_frames(), usable);
    }

    #[test]
    fn uncommit_is_saturating_and_releases_headroom() {
        let usable = 4 * RESERVE_DIVISOR;
        let m = small_map(usable);
        let a = FrameAllocator::new(&m).unwrap();
        a.commit(3).expect("commit three");
        assert_eq!(a.committed_frames(), 3);
        // An over-release cannot wrap the counter below zero.
        a.uncommit(10);
        assert_eq!(a.committed_frames(), 0);
        // A zero-page commit is a trivial success and changes nothing.
        a.commit(0).expect("zero commit");
        assert_eq!(a.committed_frames(), 0);
    }

    #[test]
    fn high_base_map_bitmap_is_proportional_to_ram_not_address() {
        // Regression: the bitmap, like the intrusive arrays, is sized to the
        // usable span and indexed from `base_frame`. A small usable window
        // sitting ~128 TiB up (huge-address territory) must cost bitmap
        // metadata proportional to the window (one word for 64 frames), not
        // to the physical address it sits at — indexed from frame zero that
        // bitmap alone would be ~4 GiB.
        let base = 128usize * 1024 * 1024 * 1024 * 1024 / PAGE_SIZE; // 128 TiB
        let frames = 64usize;
        let mut m = BootMemoryMap::new();
        m.push(MemoryRegion {
            start: PhysAddr::new((base * PAGE_SIZE) as u64),
            length: (frames * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(a.usable_frames(), frames);
        assert_eq!(a.free_frames(), frames);

        // Span-sized: 64 frames → one 64-bit word. Address-extent sizing
        // would be hundreds of millions of words.
        let words = a.inner.lock().bitmap.len();
        assert_eq!(words, frames.div_ceil(64));

        // `total_frames` still reports the address-space extent (a
        // diagnostic), proving the small bitmap is not merely a small map.
        assert!(a.total_frames() >= base);

        // Alloc/free still works correctly in the high window.
        let f = a.alloc(MemoryClass::Kernel).unwrap();
        assert!(f.0 >= base && f.0 < base + frames);
        a.free(f).unwrap();
        assert_eq!(a.free_frames(), frames);
        let big = a.alloc_order(MemoryClass::Kernel, 6).unwrap(); // 64 frames
        assert_eq!(big.0, base, "the whole window coalesced back");
        a.free_order(big, 6).unwrap();
        assert_eq!(a.free_frames(), frames);
    }

    #[test]
    fn intrusive_list_unlinks_a_middle_block_on_merge() {
        // Regression for the heap-independent intrusive free lists: a buddy
        // merge must be able to unlink a free block that is *not* the head
        // of its order's list (the doubly-linked `prev`/`next` splice). Set
        // up three order-0 free blocks, then free a fourth whose buddy is
        // the middle one, forcing a middle-of-list removal, and confirm the
        // whole window merges back to a single largest block with no leak
        // or double-count.
        let m = small_map(16);
        let a = FrameAllocator::new(&m).unwrap();
        // Drain to individual frames so every order-0 block is on the list.
        let mut frames = Vec::new();
        while let Ok(f) = a.alloc(MemoryClass::Kernel) {
            frames.push(f);
        }
        assert_eq!(frames.len(), 16);
        // Free odd-indexed frames first, then even ones: when an even
        // frame is later freed its odd buddy is already an interior list
        // node, so the merge must splice it out of the middle of the
        // order-0 list.
        for f in frames.iter().filter(|f| f.0 % 2 == 1) {
            a.free(*f).unwrap();
        }
        for f in frames.iter().filter(|f| f.0 % 2 == 0) {
            a.free(*f).unwrap();
        }
        assert_eq!(a.free_frames(), 16);
        // Fully coalesced: the largest single block the map allows is
        // available again.
        let big = a.alloc_order(MemoryClass::Kernel, 4).unwrap();
        assert_eq!(big.0 & 15, 0, "order-4 block is 16-frame aligned");
        a.free_order(big, 4).unwrap();
        assert_eq!(a.free_frames(), 16);
    }

    #[test]
    fn split_and_merge_round_trip() {
        let m = small_map(16);
        let a = FrameAllocator::new(&m).unwrap();
        // Take an order-2 block (4 frames). This forces a split if the
        // initial population coalesced higher.
        let b0 = a.alloc_order(MemoryClass::Kernel, 2).unwrap();
        let b1 = a.alloc_order(MemoryClass::Kernel, 2).unwrap();
        a.free_order(b0, 2).unwrap();
        a.free_order(b1, 2).unwrap();
        // Everything must merge back; we should once again be able to
        // satisfy the largest single allocation the map allows.
        let big = a.alloc_order(MemoryClass::Kernel, 4).unwrap(); // 16 frames
        a.free_order(big, 4).unwrap();
        assert_eq!(a.free_frames(), 16);
    }

    // Property tests: randomized alloc/free sequences uphold the
    // no-double-allocation and no-leak invariants.
    #[test]
    fn proptest_alloc_free_invariants() {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestRunner};

        let strat = proptest::collection::vec(any::<u8>(), 1..200);
        let mut runner = TestRunner::new(Config {
            cases: 64,
            ..Config::default()
        });
        runner
            .run(&strat, |ops| {
                let m = small_map(32);
                let a = FrameAllocator::new(&m).unwrap();
                let mut held: Vec<Frame> = Vec::new();
                let mut seen: std::collections::HashSet<usize> =
                    std::collections::HashSet::default();
                for op in ops {
                    if op % 2 == 0 || held.is_empty() {
                        if let Ok(f) = a.alloc(MemoryClass::Kernel) {
                            prop_assert!(seen.insert(f.0), "double alloc {}", f.0);
                            held.push(f);
                        }
                    } else {
                        let idx = (op as usize) % held.len();
                        let f = held.swap_remove(idx);
                        seen.remove(&f.0);
                        a.free(f).unwrap();
                    }
                }
                for f in held {
                    a.free(f).unwrap();
                }
                prop_assert_eq!(a.free_frames(), 32);
                Ok(())
            })
            .unwrap();
    }
    /// Every snapshot partitions the RAM: the whole is the free pool plus the
    /// per-class charges, at every point of an alloc/free/split/merge mix.
    fn assert_partitions(a: &FrameAllocator, usable: FrameCount) {
        let snap = a.snapshot();
        assert_eq!(snap.usable, usable);
        assert_eq!(
            snap.free + snap.charged(),
            snap.usable,
            "usable must be free plus the sum of the class charges"
        );
    }

    #[test]
    fn the_class_partition_holds_across_alloc_free_split_and_merge() {
        let m = small_map(32);
        let a = FrameAllocator::new(&m).unwrap();
        assert_partitions(&a, 32);

        // An order-2 draw splits a larger block; the whole block is charged.
        let block = a.alloc_order(MemoryClass::Dma, 2).unwrap();
        assert_eq!(a.snapshot().class[MemoryClass::Dma.index()], 4);
        assert_partitions(&a, 32);

        // Single frames of two further classes, so several counters are live
        // at once and no class can be reading another's frames.
        let anon = a.alloc(MemoryClass::UserAnon).unwrap();
        let table = a.alloc(MemoryClass::PageTable).unwrap();
        let snap = a.snapshot();
        assert_eq!(snap.class[MemoryClass::UserAnon.index()], 1);
        assert_eq!(snap.class[MemoryClass::PageTable.index()], 1);
        assert_eq!(snap.class[MemoryClass::Dma.index()], 4);
        assert_partitions(&a, 32);

        // Freeing merges buddies back up; each free discharges its own class.
        a.free(anon).unwrap();
        assert_eq!(a.snapshot().class[MemoryClass::UserAnon.index()], 0);
        assert_partitions(&a, 32);
        a.free_order(block, 2).unwrap();
        assert_eq!(a.snapshot().class[MemoryClass::Dma.index()], 0);
        assert_partitions(&a, 32);
        a.free(table).unwrap();
        let snap = a.snapshot();
        assert_eq!(snap.free, 32);
        assert_eq!(snap.charged(), 0);
    }

    #[test]
    fn a_multi_frame_block_may_be_freed_one_frame_at_a_time() {
        // The kernel window releases a region page by page as its page tables
        // give the frames back, so a block drawn at order 2 comes back as four
        // order-0 frees. Each must discharge the class the block was charged
        // to, or the partition drifts by the interior frames.
        let m = small_map(32);
        let a = FrameAllocator::new(&m).unwrap();
        let block = a.alloc_order(MemoryClass::Kernel, 2).unwrap();
        assert_eq!(a.snapshot().class[MemoryClass::Kernel.index()], 4);
        for offset in 0..4 {
            a.free(Frame(block.0 + offset)).expect("each frame frees");
            assert_partitions(&a, 32);
        }
        let snap = a.snapshot();
        assert_eq!(snap.class[MemoryClass::Kernel.index()], 0);
        assert_eq!(snap.free, 32);
    }

    #[test]
    fn a_free_discharges_the_class_its_alloc_charged() {
        // The caller names no class to give memory back, so the only thing
        // that can decide which counter falls is the block's own tag.
        let m = small_map(8);
        let a = FrameAllocator::new(&m).unwrap();
        let frame = a.alloc(MemoryClass::Compressed).unwrap();
        assert_eq!(a.snapshot().class[MemoryClass::Compressed.index()], 1);
        a.free(frame).unwrap();
        let snap = a.snapshot();
        assert_eq!(snap.class[MemoryClass::Compressed.index()], 0);
        for class in MemoryClass::ALL {
            assert_eq!(snap.class[class.index()], 0, "{}", class.name());
        }
    }

    #[test]
    fn freeing_a_block_this_allocator_never_charged_is_refused() {
        // An untagged head means the bookkeeping has diverged; discharging a
        // guessed class would drift the partition, so the free fails closed
        // with nothing mutated.
        let m = small_map(8);
        let a = FrameAllocator::new(&m).unwrap();
        let frame = a.alloc(MemoryClass::Kernel).unwrap();
        {
            let mut state = a.inner.lock();
            let slot = frame.0 - state.base_frame;
            state.tags[slot] = FrameTag::UNTRACKED;
        }
        assert_eq!(a.free(frame), Err(AllocError::InvariantViolation));
        assert_eq!(a.snapshot().class[MemoryClass::Kernel.index()], 1);
    }

    #[test]
    fn a_chunked_draw_charges_every_chunk_to_its_class() {
        let m = small_map(32);
        let a = FrameAllocator::new(&m).unwrap();
        // Not a power of two, so the draw is several blocks of descending
        // order rather than one.
        let chunks = a.alloc_chunks_user(MemoryClass::UserAnon, 7, None).unwrap();
        assert!(chunks.len() > 1);
        assert_eq!(a.snapshot().class[MemoryClass::UserAnon.index()], 7);
        assert_partitions(&a, 32);
        for block in chunks {
            a.free_order(block.frame, block.order).unwrap();
        }
        assert_eq!(a.snapshot().charged(), 0);
    }

    #[test]
    fn a_committed_fault_in_is_charged_as_anonymous_user_memory() {
        let m = small_map(64);
        let a = FrameAllocator::new(&m).unwrap();
        a.commit(1).unwrap();
        let frame = a.alloc_user_committed().unwrap();
        assert_eq!(a.snapshot().class[MemoryClass::UserAnon.index()], 1);
        assert_partitions(&a, 64);
        // The unwind path returns the frame to its reservation, so the class
        // is discharged and the commitment re-charged.
        a.free_committed(frame).unwrap();
        assert_eq!(a.snapshot().class[MemoryClass::UserAnon.index()], 0);
        assert_eq!(a.committed_frames(), 1);
    }

    #[test]
    fn the_packed_tag_round_trips_every_order_and_class() {
        for order in 0..=MAX_ORDER {
            let tag = FrameTag::free_head(order);
            assert_eq!(tag.free_order(), Some(order));
            assert_eq!(tag.class(), None, "a free block carries no class");
        }
        for class in MemoryClass::ALL {
            let tag = FrameTag::allocated(class);
            assert_eq!(tag.class(), Some(class));
            assert_eq!(tag.free_order(), None, "an allocation heads no list");
        }
        assert_eq!(FrameTag::UNTRACKED.free_order(), None);
        assert_eq!(FrameTag::UNTRACKED.class(), None);
    }

    #[test]
    fn a_ceiling_carve_takes_the_highest_free_block_below_it_not_the_list_front() {
        // Frames 16..80, and the lists offer the top of them first.
        let a = FrameAllocator::new(&small_map(64)).unwrap();
        let front = a.alloc_order(MemoryClass::Kernel, 0).unwrap();
        assert!(front.0 >= 48, "the list front lies above the ceiling");

        let one = a
            .alloc_order_under_user(MemoryClass::Dma, 0, Some(Frame(48).start()))
            .unwrap();
        assert_eq!(one, Frame(47));
        let four = a
            .alloc_order_under_user(MemoryClass::Dma, 2, Some(Frame(48).start()))
            .unwrap();
        assert_eq!(four, Frame(40), "44..48 holds the frame already taken");
        assert_eq!(a.snapshot().class[MemoryClass::Dma.index()], 5);
        assert_partitions(&a, 64);

        a.free_order(four, 2).unwrap();
        a.free(one).unwrap();
        a.free(front).unwrap();
        assert_eq!(a.free_frames(), 64);
        assert_eq!(
            a.alloc_order(MemoryClass::Kernel, 5),
            Ok(Frame(32)),
            "every split merged back"
        );
    }

    /// A draw under a ceiling a fragmented pool cannot meet at the order it
    /// asks takes the largest block below the ceiling there is, the highest
    /// of those first.
    #[test]
    fn a_ceiling_draw_on_a_fragmented_pool_takes_the_largest_block_below_it() {
        let a = FrameAllocator::new(&small_map(64)).unwrap();
        let mut held = Vec::new();
        while let Ok(frame) = a.alloc(MemoryClass::Kernel) {
            held.push(frame);
        }
        // Room above the ceiling, so the draw is admitted.
        let freed: Vec<usize> = [24, 25, 26, 27, 44, 46].into_iter().chain(60..80).collect();
        held.retain(|frame| !freed.contains(&frame.0));
        for &frame in &freed {
            a.free(Frame(frame)).unwrap();
        }
        let chunks = a
            .alloc_chunks_user(MemoryClass::Dma, 5, Some(Frame(48).start()))
            .unwrap();
        let taken: Vec<(usize, u32)> = chunks
            .iter()
            .map(|block| (block.frame.0, block.order))
            .collect();
        assert_eq!(taken, [(24, 2), (46, 0)]);
        a.free_chunks(&chunks);
        for frame in held {
            a.free(frame).unwrap();
        }
        assert_eq!(a.free_frames(), 64);
    }

    #[test]
    fn a_ceiling_carve_refuses_only_when_nothing_below_the_ceiling_is_free() {
        let a = FrameAllocator::new(&small_map(64)).unwrap();
        let low = a
            .alloc_order_under_user(MemoryClass::Dma, 4, Some(Frame(32).start()))
            .unwrap();
        assert_eq!(low, Frame(16));
        assert_eq!(
            a.alloc_order_under_user(MemoryClass::Dma, 0, Some(Frame(32).start())),
            Err(AllocError::OutOfMemory)
        );
        assert_eq!(a.free_frames(), 48, "memory above the ceiling is untouched");
        a.free_order(low, 4).unwrap();
        assert_eq!(
            a.alloc_order_under_user(MemoryClass::Dma, 0, Some(Frame(32).start())),
            Ok(Frame(31))
        );
    }

    #[test]
    fn a_ceiling_carve_fails_closed_on_every_request_it_cannot_honour() {
        let a = FrameAllocator::new(&small_map(64)).unwrap();
        for ceiling in [0, 10, 16] {
            assert_eq!(
                a.alloc_order_under_user(MemoryClass::Dma, 0, Some(Frame(ceiling).start())),
                Err(AllocError::OutOfRange),
                "no usable frame lies below frame {ceiling}"
            );
        }
        assert_eq!(
            a.alloc_order_under_user(MemoryClass::Dma, MAX_ORDER + 1, Some(Frame(48).start())),
            Err(AllocError::SizeUnsupported)
        );
        // A ceiling part-way into a frame excludes that frame.
        let unaligned = Some(PhysAddr::new(Frame(17).start().as_u64() + 1));
        assert_eq!(
            a.alloc_order_under_user(MemoryClass::Dma, 0, unaligned),
            Ok(Frame(16))
        );
        assert_eq!(
            a.alloc_order_under_user(MemoryClass::Dma, 0, unaligned),
            Err(AllocError::OutOfMemory)
        );
    }

    #[test]
    fn a_ceiling_above_every_usable_frame_is_an_ordinary_allocation() {
        let constrained = FrameAllocator::new(&small_map(64)).unwrap();
        let ordinary = FrameAllocator::new(&small_map(64)).unwrap();
        for ceiling in [None, Some(Frame(80).start()), Some(PhysAddr::new(u64::MAX))] {
            assert_eq!(
                constrained.alloc_order_under_user(MemoryClass::Kernel, 1, ceiling),
                ordinary.alloc_order(MemoryClass::Kernel, 1),
                "{ceiling:?}"
            );
        }
    }

    #[test]
    fn adjacent_boot_regions_populate_as_one_run() {
        // Populated apart, the halves of the order-5 block at frame 32 would
        // never merge, and no order-5 block would exist.
        let mut m = BootMemoryMap::new();
        for start in [32, 48] {
            m.push(MemoryRegion {
                start: Frame(start).start(),
                length: (16 * PAGE_SIZE) as u64,
                kind: RegionKind::Usable,
            });
        }
        let a = FrameAllocator::new(&m).unwrap();
        assert_eq!(a.alloc_order(MemoryClass::Kernel, 5), Ok(Frame(32)));
    }

    #[test]
    fn proptest_ceiling_carves_take_the_highest_free_block_or_prove_none() {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestRunner};

        const FIRST: usize = 16;
        const END: usize = 80;
        let strat = proptest::collection::vec((any::<u8>(), 0u32..=4, 0usize..=96), 1..160);
        let mut runner = TestRunner::new(Config {
            cases: 64,
            ..Config::default()
        });
        runner
            .run(&strat, |ops| {
                let a = FrameAllocator::new(&small_map(END - FIRST)).unwrap();
                let mut held: Vec<(Frame, u32)> = Vec::new();
                let mut used = [false; END];
                // The highest aligned all-free block below `ceiling`, by brute
                // force over the frames the model holds.
                let best = |used: &[bool; END], order: u32, ceiling: usize| {
                    let n = 1usize << order;
                    (FIRST.div_ceil(n) * n..=ceiling.min(END).saturating_sub(n))
                        .step_by(n)
                        .filter(|&s| used[s..s + n].iter().all(|u| !u))
                        .last()
                };
                for (op, order, ceiling) in ops {
                    if op % 3 == 2 && !held.is_empty() {
                        let (frame, order) = held.swap_remove(usize::from(op) % held.len());
                        used[frame.0..frame.0 + (1 << order)].fill(false);
                        a.free_order(frame, order).unwrap();
                        continue;
                    }
                    let got = if op % 3 == 0 {
                        a.alloc_order_under_user(
                            MemoryClass::Dma,
                            order,
                            Some(Frame(ceiling).start()),
                        )
                    } else {
                        a.alloc_order(MemoryClass::Kernel, order)
                    };
                    let n = 1usize << order;
                    let free = used[FIRST..].iter().filter(|&&taken| !taken).count();
                    if op % 3 == 0 && (free < n || free - n < a.reserve_frames()) {
                        prop_assert_eq!(got, Err(AllocError::OutOfMemory), "the reserve refuses");
                    } else if op % 3 == 0 && ceiling < END {
                        match (got, best(&used, order, ceiling)) {
                            (Ok(frame), Some(want)) => prop_assert_eq!(frame.0, want),
                            (Err(AllocError::OutOfRange), None) => prop_assert!(ceiling <= FIRST),
                            (Err(AllocError::OutOfMemory), None) => prop_assert!(ceiling > FIRST),
                            (got, want) => {
                                return Err(TestCaseError::fail(format!(
                                    "{got:?} against {want:?}"
                                )));
                            }
                        }
                    }
                    if let Ok(frame) = got {
                        prop_assert_eq!(frame.0 % n, 0);
                        prop_assert!(frame.0 >= FIRST && frame.0 + n <= END);
                        prop_assert!(used[frame.0..frame.0 + n].iter().all(|u| !u), "overlap");
                        used[frame.0..frame.0 + n].fill(true);
                        held.push((frame, order));
                    }
                }
                let held_frames: usize = held.iter().map(|&(_, order)| 1usize << order).sum();
                prop_assert_eq!(a.free_frames() + held_frames, END - FIRST);
                for (frame, order) in held {
                    a.free_order(frame, order).unwrap();
                }
                prop_assert_eq!(a.free_frames(), END - FIRST);
                Ok(())
            })
            .unwrap();
    }
}
