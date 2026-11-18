//! The bytes HostFS has read from its discs, kept in the one reclaimable
//! cache so the machine's memory pressure takes them back first. It is an
//! accelerator only: a block it does not hold is read again. A file's blocks
//! are kept under the version of its data its volume names, so every open of
//! that version finds them and a changed file is never answered from them;
//! where the volume names none, or HostFS is changing the file, under a name
//! of the open's own, given afresh at each write.

use alloc::vec::Vec;
use core::mem::size_of;

use tairix_hash::{BuildSipHash13, HashSeed};

use super::protocol::Version;
use tairix_log::Sink;
use tairix_reclaim::{
    CacheBudget, CacheCandidate, CachedBytes, InvalidationSource, PressureGauge, RebuildCost,
    ReclaimCache, ReclaimClass, ReclaimOwner, ReclaimRule, Sensitivity, MAP_ENTRY_OVERHEAD,
};

/// Bytes a block holds: what one read across the relay fetches.
pub(super) const BLOCK: u32 = 4096;

/// What a file's blocks are kept under.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Name {
    /// The version of its data.
    Version(Version),
    /// An open's own name, minted by HostFS.
    Opened(u64),
}

/// A block: its file's name in the cache, and its number in the file.
type Key = (Name, u32);

/// A block's bytes: all of them, or as many as the file has past its start.
struct Block(Vec<u8>);

impl CachedBytes for Block {
    fn payload_bytes(&self) -> usize {
        self.0.len()
    }

    fn wipe(&mut self) {
        self.0.fill(0);
    }
}

/// The cache's declaration: clean data the discs hold, one read to rebuild,
/// the guest's own and so wiped when let go, changed only by HostFS's own
/// writes.
const CANDIDATE: CacheCandidate = CacheCandidate {
    class: Some(ReclaimClass::CleanFileData),
    owner: Some(ReclaimOwner::UserlandProcess("riscos-hostfs")),
    rebuild_cost: RebuildCost::Cheap,
    sensitivity: Some(Sensitivity::UserData),
    invalidation: Some(InvalidationSource::SourceMutation),
    rule: Some(ReclaimRule::Drop),
    entry_metadata_bytes: MAP_ENTRY_OVERHEAD + size_of::<Key>() + size_of::<Block>(),
};

/// The blocks held.
pub(super) struct ReadCache {
    blocks: ReclaimCache<Key, Block, (), BuildSipHash13>,
}

impl ReadCache {
    /// An empty cache, its budget a share of a session of `ceiling` bytes and
    /// its keys hashed under `seed`, since the guest chooses what it reads.
    pub(super) fn new(
        ceiling: u64,
        seed: [u8; 16],
        pressure: &'static (dyn PressureGauge + 'static),
        log: &'static (dyn Sink + Sync),
    ) -> Self {
        let budget = CacheBudget::from_backing(usize::try_from(ceiling).unwrap_or(usize::MAX));
        let blocks = ReclaimCache::new(
            "riscos.hostfs",
            CANDIDATE,
            budget,
            pressure,
            log,
            BuildSipHash13::with_seed(HashSeed::from_bytes(seed)),
        );
        Self { blocks }
    }

    /// At most `length` bytes of block `block` of the file named `name` from
    /// `within` into `out`, if the cache holds the block, fewer where the
    /// file ends: whether it did.
    pub(super) fn copy(
        &mut self,
        name: Name,
        block: u32,
        within: u32,
        length: u32,
        out: &mut Vec<u8>,
    ) -> bool {
        let Some(held) = self.blocks.find(&(), &(name, block)) else {
            return false;
        };
        let rest = held.0.get(within as usize..).unwrap_or_default();
        out.clear();
        out.extend_from_slice(&rest[..rest.len().min(length as usize)]);
        true
    }

    /// Block `block` of the file named `name` kept as a copy of `bytes`,
    /// where there is room to copy it.
    pub(super) fn keep(&mut self, name: Name, block: u32, bytes: &[u8]) {
        if let Some(bytes) = tairix_util::fallible::copied(bytes) {
            self.blocks.retain(&(), (name, block), Block(bytes));
        }
    }
}
