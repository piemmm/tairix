//! `'static` RAM backing for host tests, accountable to the UB oracle.
//!
//! [`FramePages`](crate::framepages::FramePages),
//! [`FrameTableSource`](crate::pagetables::FrameTableSource),
//! [`SlotWindow`](crate::kvslots::SlotWindow) and
//! [`LiveSpace`](crate::live::LiveSpace) all borrow `&'static` because
//! production holds those pieces in kernel globals, so a test cannot own them
//! on its stack. A leaked `Box` is indistinguishable from a real leak to the
//! interpreter, so each piece lives in a cell that stays reachable for the
//! whole run instead.

use alloc::vec::Vec;

use tairix_sync::{Once, SpinLock};

use crate::bootinfo::{BootMemoryMap, MemoryRegion, RegionKind};
use crate::dma::{DeviceTranslation, DmaCustody, DmaError};
use crate::error::AllocError;
use crate::frame::{Chunks, FrameBlock};
use crate::frame::{FrameAllocator, PhysAddr, PAGE_SIZE};
use crate::phys::SimPhysMap;

/// A frame pool and the simulated direct map that addresses exactly its
/// RAM.
///
/// Reach one through [`frame_backing!`], which gives every test site a cell
/// of its own so no two concurrently-running tests share a pool.
pub(crate) struct FrameBacking {
    frames: Once<FrameAllocator>,
    sim: Once<SimPhysMap>,
}

impl FrameBacking {
    pub(crate) const fn new() -> Self {
        Self {
            frames: Once::new(),
            sim: Once::new(),
        }
    }

    /// `pages` frames of usable RAM based at `base`, addressed by a
    /// direct map over exactly those bytes.
    pub(crate) fn build(
        &'static self,
        base: u64,
        pages: usize,
    ) -> (&'static FrameAllocator, &'static SimPhysMap) {
        let mut map = BootMemoryMap::new();
        map.push(MemoryRegion {
            start: PhysAddr::new(base),
            length: (pages * PAGE_SIZE) as u64,
            kind: RegionKind::Usable,
        });
        let frames = self
            .frames
            .call_once_infallible(|| FrameAllocator::new(&map).expect("allocator over the window"))
            .expect("a fresh cell");
        let sim = self
            .sim
            .call_once_infallible(|| SimPhysMap::new(PhysAddr::new(base), pages * PAGE_SIZE))
            .expect("a fresh cell");
        (frames, sim)
    }
}

/// Build a [`FrameBacking`] in a cell of this expansion's own.
macro_rules! frame_backing {
    ($base:expr, $pages:expr) => {{
        static CELL: crate::test_fixture::FrameBacking = crate::test_fixture::FrameBacking::new();
        CELL.build($base, $pages)
    }};
}

pub(crate) use frame_backing;

/// What a [`RecordingCustody`] was handed.
pub(crate) struct CustodyRecord {
    /// Reservations neither spent by a hold nor returned.
    pub(crate) reserved: usize,
    /// Every reservation accepted.
    pub(crate) reservations: usize,
    /// Every block held, with the node and generation it came under.
    pub(crate) held: Vec<(u32, u64, FrameBlock)>,
}

/// A [`DmaCustody`] that records what it is given, optionally refusing every
/// reservation.
pub(crate) struct RecordingCustody {
    refuse: bool,
    record: SpinLock<CustodyRecord>,
}

impl RecordingCustody {
    pub(crate) const fn new(refuse: bool) -> Self {
        Self {
            refuse,
            record: SpinLock::new(CustodyRecord {
                reserved: 0,
                reservations: 0,
                held: Vec::new(),
            }),
        }
    }

    /// Run `f` over the record.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&CustodyRecord) -> R) -> R {
        f(&self.record.lock())
    }
}

impl DmaCustody for RecordingCustody {
    fn reserve(&self, _node: u32) -> Result<(), DmaError> {
        if self.refuse {
            return Err(DmaError::Alloc(AllocError::OutOfMemory));
        }
        let mut record = self.record.lock();
        record.reserved += 1;
        record.reservations += 1;
        Ok(())
    }

    fn unreserve(&self, _node: u32) {
        self.record.lock().reserved -= 1;
    }

    fn hold(&self, node: u32, generation: u64, block: FrameBlock) {
        let mut record = self.record.lock();
        record.reserved -= 1;
        record.held.push((node, generation, block));
    }
}

/// A [`RecordingCustody`] in a cell of this expansion's own; `refusing`
/// builds one that refuses every reservation.
macro_rules! custody {
    () => {{
        static CUSTODY: crate::test_fixture::RecordingCustody =
            crate::test_fixture::RecordingCustody::new(false);
        &CUSTODY
    }};
    (refusing) => {{
        static CUSTODY: crate::test_fixture::RecordingCustody =
            crate::test_fixture::RecordingCustody::new(true);
        &CUSTODY
    }};
}

pub(crate) use custody;

/// What a [`RecordingTranslation`] was asked to do.
pub(crate) struct TranslationRecord {
    /// Every mapping handed out: its IOVA, blocks and the limit asked for.
    pub(crate) mapped: Vec<(u64, Vec<FrameBlock>, u64)>,
    /// Every unmap confirmed by the unit: its IOVA.
    pub(crate) unmapped: Vec<u64>,
    /// Every owner ended: its node and generation.
    pub(crate) ended: Vec<(u32, u64)>,
    /// Every block kept as custody, with the node and generation it came
    /// under: a translation counts no reservation.
    pub(crate) held: Vec<(u32, u64, FrameBlock)>,
    /// Every carve whose free was deferred: its IOVA and the blocks taken.
    pub(crate) deferred: Vec<(u64, Vec<FrameBlock>)>,
}

/// A [`DeviceTranslation`] that hands out IOVAs from a fixed base and records
/// every call, refusing maps with `refuse_map`, leaving every unmap and end
/// unconfirmed when `unconfirmed_unmap`, and taking every free to defer when
/// `deferring`. Once an owner is ended its unmaps answer without reaching the
/// unit, as the facility's do.
pub(crate) struct RecordingTranslation {
    refuse_map: Option<DmaError>,
    unconfirmed_unmap: bool,
    deferring: bool,
    record: SpinLock<TranslationRecord>,
}

/// Where a [`RecordingTranslation`] starts handing out IOVAs: far from the
/// synthetic RAM, so an IOVA is never mistaken for a physical address.
pub(crate) const IOVA_BASE: u64 = 0x7F00_0000_0000;

impl RecordingTranslation {
    pub(crate) const fn new(refuse_map: Option<DmaError>, unconfirmed_unmap: bool) -> Self {
        Self::with_deferral(refuse_map, unconfirmed_unmap, false)
    }

    pub(crate) const fn with_deferral(
        refuse_map: Option<DmaError>,
        unconfirmed_unmap: bool,
        deferring: bool,
    ) -> Self {
        Self {
            refuse_map,
            unconfirmed_unmap,
            deferring,
            record: SpinLock::new(TranslationRecord {
                mapped: Vec::new(),
                unmapped: Vec::new(),
                ended: Vec::new(),
                held: Vec::new(),
                deferred: Vec::new(),
            }),
        }
    }

    /// Run `f` over the record.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&TranslationRecord) -> R) -> R {
        f(&self.record.lock())
    }
}

impl DeviceTranslation for RecordingTranslation {
    fn map(
        &self,
        _node: u32,
        _generation: u64,
        blocks: &[FrameBlock],
        limit: u64,
    ) -> Result<u64, DmaError> {
        if let Some(err) = self.refuse_map {
            return Err(err);
        }
        let mut record = self.record.lock();
        let iova = IOVA_BASE + (record.mapped.len() as u64) * (1 << 40);
        record.mapped.push((iova, blocks.to_vec(), limit));
        Ok(iova)
    }

    fn unmap(&self, node: u32, generation: u64, iova: u64) -> Result<(), DmaError> {
        if self.unconfirmed_unmap {
            return Err(DmaError::Unconfirmed);
        }
        let mut record = self.record.lock();
        if !record.ended.contains(&(node, generation)) {
            record.unmapped.push(iova);
        }
        Ok(())
    }

    fn defer_free(
        &self,
        _node: u32,
        _generation: u64,
        iova: u64,
        blocks: &mut Chunks,
    ) -> Result<bool, DmaError> {
        if !self.deferring {
            return Ok(false);
        }
        let taken = core::mem::take(blocks);
        self.record.lock().deferred.push((iova, taken.to_vec()));
        Ok(true)
    }

    fn end(&self, node: u32, generation: u64) {
        self.record.lock().ended.push((node, generation));
    }
}

impl DmaCustody for RecordingTranslation {
    fn reserve(&self, _node: u32) -> Result<(), DmaError> {
        Ok(())
    }

    fn unreserve(&self, _node: u32) {}

    fn hold(&self, node: u32, generation: u64, block: FrameBlock) {
        self.record.lock().held.push((node, generation, block));
    }
}

/// A [`RecordingTranslation`] in a cell of this expansion's own: `refusing(e)`
/// refuses every map with `e`, `unconfirmed` confirms no unmap.
macro_rules! translation {
    () => {{
        static TRANSLATION: crate::test_fixture::RecordingTranslation =
            crate::test_fixture::RecordingTranslation::new(None, false);
        &TRANSLATION
    }};
    (refusing $err:expr) => {{
        static TRANSLATION: crate::test_fixture::RecordingTranslation =
            crate::test_fixture::RecordingTranslation::new(Some($err), false);
        &TRANSLATION
    }};
    (unconfirmed) => {{
        static TRANSLATION: crate::test_fixture::RecordingTranslation =
            crate::test_fixture::RecordingTranslation::new(None, true);
        &TRANSLATION
    }};
    (deferring) => {{
        static TRANSLATION: crate::test_fixture::RecordingTranslation =
            crate::test_fixture::RecordingTranslation::with_deferral(None, false, true);
        &TRANSLATION
    }};
}

pub(crate) use translation;
