//! The sectors the worker has fetched, kept in the one reclaimable cache so
//! the machine's memory pressure takes them back first.
//!
//! The cache is an accelerator only: a sector it does not hold is fetched
//! again. It holds whole blocks of [`BLOCK_SECTORS`], keyed by the medium and
//! the block, hashed under a key the front-end drew, because the guest chooses
//! every sector it reads.

use crate::storage::SECTOR;
use alloc::vec::Vec;
use core::mem::size_of;
use tairix_hash::{BuildSipHash13, HashSeed};
use tairix_log::Sink;
use tairix_reclaim::{
    CacheBudget, CacheCandidate, CachedBytes, InvalidationSource, PressureGauge, RebuildCost,
    ReclaimCache, ReclaimClass, ReclaimOwner, ReclaimRule, Sensitivity, MAP_ENTRY_OVERHEAD,
};

/// Bytes a block holds: a page of eight sectors.
const BLOCK_BYTES: usize = 8 * SECTOR;

/// Sectors a block holds.
pub const BLOCK_SECTORS: u64 = (BLOCK_BYTES / SECTOR) as u64;

/// A block: the medium, and its block number on it.
type Key = (u16, u64);

/// A block's sectors: all of them, or as many as the medium has past the
/// block's start.
struct Block(Vec<u8>);

impl CachedBytes for Block {
    fn payload_bytes(&self) -> usize {
        self.0.len()
    }

    fn wipe(&mut self) {
        self.0.fill(0);
    }
}

/// The cache's declaration: clean data its medium holds, one bounded read to
/// rebuild, the guest's own and so wiped when let go, changed only by the
/// worker's own writes through it.
const CANDIDATE: CacheCandidate = CacheCandidate {
    class: Some(ReclaimClass::CleanFileData),
    owner: Some(ReclaimOwner::UserlandProcess("pcemu-worker")),
    rebuild_cost: RebuildCost::Cheap,
    sensitivity: Some(Sensitivity::UserData),
    invalidation: Some(InvalidationSource::SourceMutation),
    rule: Some(ReclaimRule::Drop),
    entry_metadata_bytes: MAP_ENTRY_OVERHEAD + size_of::<Key>() + size_of::<Block>(),
};

/// The sectors held.
pub struct SectorCache {
    blocks: ReclaimCache<Key, Block, (), BuildSipHash13>,
}

impl SectorCache {
    /// An empty cache of `budget`, its keys hashed under `seed`.
    pub fn new(
        budget: CacheBudget,
        seed: HashSeed,
        pressure: &'static (dyn PressureGauge + 'static),
        sink: &'static (dyn Sink + Sync),
    ) -> Self {
        let blocks = ReclaimCache::new(
            "pcemu.sectors",
            CANDIDATE,
            budget,
            pressure,
            sink,
            BuildSipHash13::with_seed(seed),
        );
        Self { blocks }
    }

    /// Copy sector `sector` of `medium` into `out`, answering whether the
    /// cache held it.
    pub fn read(&mut self, medium: u16, sector: u64, out: &mut [u8; SECTOR]) -> bool {
        let at = (sector % BLOCK_SECTORS) as usize * SECTOR;
        let held = self.blocks.find(&(), &(medium, sector / BLOCK_SECTORS));
        match held.and_then(|block| block.0.get(at..at + SECTOR)) {
            Some(bytes) => {
                out.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }

    /// Keep the run of sectors `bytes` holds from `first`, which begins on a
    /// block and ends on one or at the medium's end.
    pub fn keep(&mut self, medium: u16, first: u64, bytes: &[u8]) {
        for (n, chunk) in (first / BLOCK_SECTORS..).zip(bytes.chunks(BLOCK_BYTES)) {
            let mut block = Vec::new();
            if block.try_reserve_exact(chunk.len()).is_err() {
                return;
            }
            block.extend_from_slice(chunk);
            self.blocks.retain(&(), (medium, n), Block(block));
        }
    }

    /// Sector `sector` of `medium` now holds `data`.
    pub fn written(&mut self, medium: u16, sector: u64, data: &[u8; SECTOR]) {
        let at = (sector % BLOCK_SECTORS) as usize * SECTOR;
        self.blocks
            .renew(&(), &(medium, sector / BLOCK_SECTORS), |block| {
                block
                    .0
                    .get_mut(at..at + SECTOR)
                    .map(|bytes| bytes.copy_from_slice(data))
                    .is_some()
            });
    }

    /// Forget sector `sector` of `medium`, whose medium may not hold what the
    /// cache does.
    pub fn forget(&mut self, medium: u16, sector: u64) {
        self.blocks.invalidate(&(medium, sector / BLOCK_SECTORS));
    }

    /// Hold no more than `budget` from now on.
    pub fn rebudget(&mut self, budget: CacheBudget) {
        self.blocks.rebudget(budget);
    }

    /// Give back what the pressure band now asks for.
    pub fn enforce_pressure(&mut self) {
        self.blocks.enforce_pressure();
    }

    /// Bytes charged, payload and bookkeeping.
    #[cfg(test)]
    pub fn charged_bytes(&self) -> usize {
        self.blocks.charged_bytes()
    }
}
