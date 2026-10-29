//! What the front remembers of a share's nodes between calls: each node's
//! attributes and each directory's listing, for a second after the server
//! last said so — CIFS's `actimeo` default — so a burst of calls on the same
//! names reaches the server once. The front's own changes drop what they make
//! stale at once; another writer's are seen within the second.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::NodeId;
use tairix_abi::netattach::OfferId;
use tairix_abi::time::NANOS_PER_SEC;
use tairix_hash::BuildSipHash13;
use tairix_log::Sink;
use tairix_reclaim::{
    CacheBudget, CacheCandidate, CachedBytes, InvalidationSource, PressureGauge, RebuildCost,
    ReclaimCache, ReclaimClass, ReclaimOwner, ReclaimRule, Sensitivity, MAP_ENTRY_OVERHEAD,
};

use super::served::parent_of;
use super::Front;
use crate::host::Host;
use crate::wire::Stat;
use tairix_smb::file::name::SharePath;

/// How long what the server said of a node stands.
pub const FRESH_NS: u64 = NANOS_PER_SEC as u64;

/// The most entries a listing kept whole holds: past it, the listing is
/// read from the server each time.
pub const SNAPSHOT_MAX: usize = 4096;

/// A node, on one volume.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(super) struct NodeKey {
    offer: u64,
    node: u64,
}

impl NodeKey {
    pub(super) const fn of(offer: OfferId, node: NodeId) -> Self {
        Self {
            offer: offer.get(),
            node: node.raw(),
        }
    }
}

/// What the server said a node was, and when: under the lease on its
/// directory it was read under, if one, which keeps it standing past its
/// second until the lease breaks.
pub(super) struct Attr {
    pub(super) stat: Stat,
    at: u64,
    under: Option<(NodeId, [u8; 16])>,
}

impl CachedBytes for Attr {
    fn payload_bytes(&self) -> usize {
        0
    }

    // A stat holds no name and no byte of a file.
    fn wipe(&mut self) {}
}

/// A directory's whole listing, when it was read, and the lease on the
/// directory it was read under, if one.
pub(super) struct Snapshot {
    pub(super) entries: Vec<(String, Stat)>,
    at: u64,
    under: Option<[u8; 16]>,
}

impl CachedBytes for Snapshot {
    fn payload_bytes(&self) -> usize {
        self.entries.capacity() * core::mem::size_of::<(String, Stat)>()
            + self
                .entries
                .iter()
                .map(|(name, _)| name.capacity())
                .sum::<usize>()
    }

    fn wipe(&mut self) {
        for (name, _) in &mut self.entries {
            // Overwritten in place: the bytes are ASCII zeros, valid UTF-8.
            let len = name.len();
            name.clear();
            name.extend(core::iter::repeat_n('\0', len));
        }
    }
}

/// The attributes kept, by node.
pub(super) type Attrs = ReclaimCache<NodeKey, Attr, (), BuildSipHash13>;

/// The listings kept, by directory.
pub(super) type Snapshots = ReclaimCache<NodeKey, Snapshot, (), BuildSipHash13>;

/// The declaration both caches make: file metadata, dropped by the front's
/// own changes and ended by its second otherwise.
const fn metadata(entry: usize) -> CacheCandidate {
    CacheCandidate {
        class: Some(ReclaimClass::FsMetadata),
        owner: Some(ReclaimOwner::UserlandProcess("smbfs")),
        rebuild_cost: RebuildCost::Moderate,
        // Names and sizes of a user's files.
        sensitivity: Some(Sensitivity::UserData),
        invalidation: Some(InvalidationSource::SourceMutation),
        rule: Some(ReclaimRule::Drop),
        entry_metadata_bytes: entry,
    }
}

/// The attribute and listing caches, within `budget`.
pub(super) fn caches(
    budget: CacheBudget,
    pressure: &'static dyn PressureGauge,
    sink: &'static (dyn Sink + Sync),
    hasher: BuildSipHash13,
) -> (Attrs, Snapshots) {
    let key = core::mem::size_of::<NodeKey>() + MAP_ENTRY_OVERHEAD;
    (
        ReclaimCache::new(
            "smbfs attributes",
            metadata(key + core::mem::size_of::<Attr>()),
            budget,
            pressure,
            sink,
            hasher,
        ),
        ReclaimCache::new(
            "smbfs listings",
            metadata(key + core::mem::size_of::<Snapshot>()),
            budget,
            pressure,
            sink,
            hasher,
        ),
    )
}

/// A fresh attribute record.
pub(super) const fn attr(stat: Stat, now: u64, under: Option<(NodeId, [u8; 16])>) -> Attr {
    Attr {
        stat,
        at: now,
        under,
    }
}

/// Whether a record made at `at` still stands at `now`.
const fn fresh(at: u64, now: u64) -> bool {
    now.saturating_sub(at) < FRESH_NS
}

impl<H: Host, S: Sink + Clone> Front<H, S> {
    /// What the server said of `node` within the second, or under a lease
    /// on its directory that still holds, if it did.
    pub(super) fn fresh_attr(&mut self, now: u64, offer: OfferId, node: NodeId) -> Option<Stat> {
        let volume = self.volumes.get_mut(&offer.get())?;
        let attr = self.attrs.as_mut()?.find(&(), &NodeKey::of(offer, node))?;
        let leased = attr
            .under
            .filter(|&(dir, key)| volume.dir_lease(dir) == Some(key));
        if let Some((dir, _)) = leased {
            volume.used_dir_lease(dir, now);
        }
        (fresh(attr.at, now) || leased.is_some()).then_some(attr.stat)
    }

    /// Keep what the server said of `node` at `now`.
    pub(super) fn keep_attr(&mut self, now: u64, offer: OfferId, node: NodeId, stat: Stat) {
        if let Some(attrs) = self.attrs.as_mut() {
            attrs.retain(&(), NodeKey::of(offer, node), attr(stat, now, None));
        }
    }

    /// The node at `path` changed: forget what was kept of it, and of the
    /// directory holding it.
    pub(super) fn stale_path(&mut self, offer: OfferId, path: &SharePath) {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        let node = volume.nodes.id_of(path);
        let dir = parent_of(path)
            .ok()
            .and_then(|parent| volume.nodes.id_of(&parent));
        for node in [node, dir].into_iter().flatten() {
            self.forget_kept(offer, node);
        }
    }

    /// The node `node` changed: forget what was kept of it, and of the
    /// directory holding it.
    pub(super) fn stale_node(&mut self, offer: OfferId, node: NodeId) {
        let path = self
            .volumes
            .get_mut(&offer.get())
            .and_then(|volume| volume.nodes.get(node))
            .map(|found| found.path);
        match path {
            Some(path) => self.stale_path(offer, &path),
            None => self.forget_kept(offer, node),
        }
    }

    fn forget_kept(&mut self, offer: OfferId, node: NodeId) {
        let key = NodeKey::of(offer, node);
        if let Some(attrs) = self.attrs.as_mut() {
            attrs.invalidate(&key);
        }
        if let Some(snapshots) = self.snapshots.as_mut() {
            snapshots.invalidate(&key);
        }
    }

    /// The listing of `dir` read within the second, or under a lease on it
    /// that still holds, copied out, if one was.
    pub(super) fn kept_listing(
        &mut self,
        now: u64,
        offer: OfferId,
        dir: NodeId,
    ) -> Option<Vec<(String, Stat)>> {
        let lease = self.volumes.get(&offer.get())?.dir_lease(dir);
        let snapshot = self
            .snapshots
            .as_mut()?
            .find(&(), &NodeKey::of(offer, dir))?;
        let leased = snapshot.under.is_some() && snapshot.under == lease;
        if !fresh(snapshot.at, now) && !leased {
            return None;
        }
        if leased {
            if let Some(volume) = self.volumes.get_mut(&offer.get()) {
                volume.used_dir_lease(dir, now);
            }
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(snapshot.entries.len()).ok()?;
        for (name, stat) in &snapshot.entries {
            let mut held = String::new();
            held.try_reserve_exact(name.len()).ok()?;
            held.push_str(name);
            entries.push((held, *stat));
        }
        Some(entries)
    }

    /// Keep `dir`'s whole listing, read at `now` under the lease `under`, if
    /// one.
    pub(super) fn keep_listing(
        &mut self,
        now: u64,
        offer: OfferId,
        dir: NodeId,
        entries: Vec<(String, Stat)>,
        under: Option<[u8; 16]>,
    ) {
        if let Some(snapshots) = self.snapshots.as_mut() {
            let kept = Snapshot {
                entries,
                at: now,
                under,
            };
            snapshots.retain(&(), NodeKey::of(offer, dir), kept);
        }
    }
}

#[cfg(test)]
#[path = "meta_tests.rs"]
mod tests;
