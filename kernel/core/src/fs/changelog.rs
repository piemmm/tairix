//! How a volume's cache wrapper reports each mutation to the volume's watch
//! table (`crate::fswatch`, `docs/src/filesystem/watch.md`).
//!
//! The driver surface is keyed `(dir, name)` for every namespace change,
//! which is exactly what a directory's journal records. Two facts it does not
//! carry are recovered from the bindings the wrapper itself resolved: which
//! entry names a directory in *its* parent (an entry added inside a subfolder
//! changes the subfolder's own entry), and which entry a node-keyed content
//! or metadata change was made through.
//!
//! A pinned node's place is kept for as long as it is pinned: the binding the
//! walk that pinned it resolved, moved by a rename and dropped by a removal,
//! so a descriptor held open for a day still reports its writes through its
//! name. Any other node's is the most recent of a short trail of bindings,
//! which an operation's own walk refreshes before it mutates. A node with
//! neither leaves that single report unmade rather than guessed.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::NodeId;
use tairix_collections::HashMap;
use tairix_hash::BuildSipHash13;
use zeroize::Zeroize;

use super::path::MAX_COMPONENT_LEN;
use crate::fswatch::{Claim, VolumeWatch};

/// How many recent bindings the trail keeps: enough for the two walks a
/// rename makes on any ordinarily deep path. A directory reached through more
/// distinct components than this since its own was resolved loses only the
/// report that its entry in its parent changed; its listing's own changes
/// never depend on the trail.
const TRAIL_LEN: usize = 16;

/// `dir/name` named `node` when the wrapper last saw it, and still does:
/// every mutation that could unbind it drops it. Bytes past `len` are always
/// zero, so wiping a binding costs its name's length rather than the slot's.
struct Binding {
    node: u64,
    dir: u64,
    /// When the binding was last resolved, so the oldest is the one to go.
    used: u64,
    len: usize,
    name: [u8; MAX_COMPONENT_LEN],
}

impl Binding {
    fn vacant() -> Self {
        Self {
            node: 0,
            dir: 0,
            used: 0,
            len: 0,
            name: [0; MAX_COMPONENT_LEN],
        }
    }

    fn name(&self) -> &[u8] {
        &self.name[..self.len]
    }

    fn wipe(&mut self) {
        self.name[..self.len].zeroize();
        self.len = 0;
    }

    /// Bind `dir/name` to `node` instead.
    fn set(&mut self, node: u64, dir: u64, name: &[u8]) {
        if name.len() > MAX_COMPONENT_LEN {
            return;
        }
        self.wipe();
        self.node = node;
        self.dir = dir;
        self.len = name.len();
        self.name[..name.len()].copy_from_slice(name);
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.wipe();
    }
}

/// The bindings, in slots reserved once: the trail never moves a name to a
/// new allocation, which would leave its bytes behind in the old one.
struct Trail {
    slots: Vec<Binding>,
    clock: u64,
}

impl Trail {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            clock: 0,
        }
    }

    fn record(&mut self, node: u64, dir: u64, name: &[u8]) {
        if name.len() > MAX_COMPONENT_LEN {
            return;
        }
        self.clock += 1;
        let used = self.clock;
        if let Some(binding) = self
            .slots
            .iter_mut()
            .find(|b| b.len != 0 && b.node == node && b.dir == dir && b.name() == name)
        {
            binding.used = used;
            return;
        }
        let at = if let Some(at) = self
            .slots
            .iter()
            .position(|b| b.len != 0 && b.node == node)
            .or_else(|| self.slots.iter().position(|b| b.len == 0))
        {
            at
        } else if self.slots.len() < TRAIL_LEN {
            if self.slots.capacity() == 0 && self.slots.try_reserve_exact(TRAIL_LEN).is_err() {
                return;
            }
            self.slots.push(Binding::vacant());
            self.slots.len() - 1
        } else {
            let Some(at) = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, b)| b.used)
                .map(|(at, _)| at)
            else {
                return;
            };
            at
        };
        let Some(binding) = self.slots.get_mut(at) else {
            return;
        };
        binding.set(node, dir, name);
        binding.used = used;
    }

    fn forget(&mut self, unbound: impl Fn(&Binding) -> bool) {
        for binding in &mut self.slots {
            if binding.len != 0 && unbound(binding) {
                binding.wipe();
                binding.node = 0;
            }
        }
    }

    fn binding_of(&self, node: u64) -> Option<(NodeId, &[u8])> {
        self.slots
            .iter()
            .find(|b| b.len != 0 && b.node == node)
            .map(|b| (NodeId::from_raw(b.dir), b.name()))
    }
}

/// A recorded name, wiped before its allocation is released: names are
/// decrypted user data.
struct PlaceName(Box<[u8]>);

impl PlaceName {
    fn copy(name: &[u8]) -> Option<Self> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(name.len()).ok()?;
        bytes.extend_from_slice(name);
        Some(Self(bytes.into_boxed_slice()))
    }
}

impl Drop for PlaceName {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Where a pinned node's name lies: the entry `name` of directory `dir`.
struct PinnedPlace {
    dir: u64,
    name: PlaceName,
}

impl PinnedPlace {
    fn is(&self, dir: u64, name: &[u8]) -> bool {
        self.dir == dir && *self.name.0 == *name
    }
}

/// A volume's claimed watch table and what attributes the changes its driver
/// surface leaves anonymous. Dropping it lets the table go.
pub(crate) struct ChangeLog {
    claim: Claim,
    trail: Trail,
    /// The binding the latest walk resolved, kept whether or not anything is
    /// watched: the place a pin taken after it is given.
    last: Binding,
    /// Each pinned node's place, by pinned id; bounded by the live pins.
    places: HashMap<u64, PinnedPlace, BuildSipHash13>,
}

impl ChangeLog {
    pub(crate) fn new(claim: Claim) -> Self {
        Self {
            claim,
            trail: Trail::new(),
            last: Binding::vacant(),
            // Pinned ids are the driver's, not chosen by a user, but the
            // table hashes under the per-boot key as the watch table does.
            places: HashMap::with_hasher(
                BuildSipHash13::keyed().unwrap_or(BuildSipHash13::UNKEYED),
            ),
        }
    }

    fn table(&self) -> &VolumeWatch {
        self.claim.table()
    }

    /// `dir/name` resolved to `node`. Trailed only while something on the
    /// volume is watched; unbinding is tracked always, so nothing kept goes
    /// stale.
    pub(crate) fn resolved(&mut self, node: NodeId, dir: NodeId, name: &[u8]) {
        self.last.set(node.raw(), dir.raw(), name);
        if self.table().active() {
            self.trail.record(node.raw(), dir.raw(), name);
        }
    }

    /// `node`, which the latest walk resolved, was pinned as `pinned`: its
    /// place is kept until its last unpin. A pin the latest walk did not
    /// reach keeps the place already kept, if any.
    pub(crate) fn pinned(&mut self, node: NodeId, pinned: NodeId) {
        if self.last.len == 0 || self.last.node != node.raw() {
            return;
        }
        let Some(name) = PlaceName::copy(self.last.name()) else {
            return;
        };
        let place = PinnedPlace {
            dir: self.last.dir,
            name,
        };
        // A place that cannot be kept leaves the node's reports to the trail.
        let _ = self.places.try_insert(pinned.raw(), place);
    }

    /// The last hold of `pinned` went.
    pub(crate) fn unpinned(&mut self, pinned: NodeId) {
        self.places.remove(&pinned.raw());
    }

    /// Every hold went.
    pub(crate) fn released_all(&mut self) {
        self.places.clear();
    }

    /// Where `node`'s name lies, as far as the wrapper knows.
    fn place_of(&self, node: u64) -> Option<(NodeId, &[u8])> {
        match self.places.get(&node) {
            Some(place) => Some((NodeId::from_raw(place.dir), &place.name.0[..])),
            None => self.trail.binding_of(node),
        }
    }

    fn within(&self, dir: NodeId) -> Option<(NodeId, &[u8])> {
        self.place_of(dir.raw())
    }

    /// Forget every binding `unbound` says a mutation undid.
    fn forget(&mut self, unbound: impl Fn(&Binding) -> bool) {
        if self.last.len != 0 && unbound(&self.last) {
            self.last.wipe();
        }
        self.trail.forget(unbound);
    }

    /// `node`'s kept place no longer holds it, if it was `dir/name`.
    fn unplace(&mut self, node: NodeId, dir: u64, name: &[u8]) {
        if self
            .places
            .get(&node.raw())
            .is_some_and(|place| place.is(dir, name))
        {
            self.places.remove(&node.raw());
        }
    }

    /// `dir` gained the entry `name`, naming `node` when the driver said.
    pub(crate) fn added(&mut self, dir: NodeId, name: &[u8], node: Option<NodeId>) {
        self.table().entries_changed(dir, name, self.within(dir));
        if let Some(node) = node {
            self.resolved(node, dir, name);
        }
    }

    /// `dir/name` became a second name for `node`, whose link count the
    /// name it was reached through now shows changed.
    pub(crate) fn linked(&mut self, dir: NodeId, name: &[u8], node: NodeId) {
        match self.place_of(node.raw()) {
            Some((was_dir, was_name)) => self.table().entry_changed(was_dir, was_name, Some(node)),
            None => self.table().node_changed(node),
        }
        self.added(dir, name, Some(node));
    }

    /// `dir/name` was removed; `victim` is what it named, when known. The
    /// victim's watchers are woken to find it gone.
    pub(crate) fn removed(&mut self, dir: NodeId, name: &[u8], victim: Option<NodeId>) {
        let dir_raw = dir.raw();
        self.forget(|b| {
            (b.dir == dir_raw && b.name() == name) || victim.is_some_and(|v| v.raw() == b.node)
        });
        if let Some(victim) = victim {
            self.unplace(victim, dir_raw, name);
        }
        self.table().entries_changed(dir, name, self.within(dir));
        if let Some(victim) = victim {
            self.table().node_removed(victim);
        }
    }

    /// `src_dir/src_name` moved to `dst_dir/dst_name`, replacing whatever
    /// `overwritten` named there. A pinned `moved` keeps its place at its new
    /// name; a node moved keeps its watchers, and one replaced has its own
    /// woken to find it gone.
    pub(crate) fn renamed(
        &mut self,
        src: (NodeId, &[u8]),
        dst: (NodeId, &[u8]),
        moved: Option<NodeId>,
        overwritten: Option<NodeId>,
    ) {
        let (src_dir, src_name) = src;
        let (dst_dir, dst_name) = dst;
        let (src_raw, dst_raw) = (src_dir.raw(), dst_dir.raw());
        self.forget(|b| {
            (b.dir == src_raw && b.name() == src_name)
                || (b.dir == dst_raw && b.name() == dst_name)
                || [moved, overwritten]
                    .into_iter()
                    .flatten()
                    .any(|n| n.raw() == b.node)
        });
        if let Some(overwritten) = overwritten {
            self.unplace(overwritten, dst_raw, dst_name);
        }
        if let Some(moved) = moved {
            self.replace(moved, (src_raw, src_name), (dst_raw, dst_name));
        }
        self.table()
            .entries_changed(src_dir, src_name, self.within(src_dir));
        if dst_raw != src_raw || dst_name != src_name {
            self.table()
                .entries_changed(dst_dir, dst_name, self.within(dst_dir));
        }
        if let Some(overwritten) = overwritten {
            self.table().node_removed(overwritten);
        }
        if let Some(moved) = moved {
            self.table().node_changed(moved);
            self.resolved(moved, dst_dir, dst_name);
        }
    }

    /// Move `node`'s kept place from `from` to `to`, if it was `from`.
    fn replace(&mut self, node: NodeId, from: (u64, &[u8]), to: (u64, &[u8])) {
        let Some(place) = self.places.get_mut(&node.raw()) else {
            return;
        };
        if !place.is(from.0, from.1) {
            return;
        }
        match PlaceName::copy(to.1) {
            Some(name) => {
                place.dir = to.0;
                place.name = name;
            }
            None => {
                self.places.remove(&node.raw());
            }
        }
    }

    /// `node`'s contents or metadata changed, through the entry it is known
    /// by.
    pub(crate) fn changed(&mut self, node: NodeId) {
        match self.place_of(node.raw()) {
            Some((dir, name)) => self.table().entry_changed(dir, name, Some(node)),
            None => self.table().node_changed(node),
        }
    }

    /// `node`'s security changed: its entry changed in the directory holding
    /// it, and its own watchers rescan, since who may list what they watch
    /// may have changed with it.
    pub(crate) fn security_changed(&mut self, node: NodeId) {
        if let Some((dir, name)) = self.place_of(node.raw()) {
            self.table().entry_changed(dir, name, None);
        }
        // The rescan a lost name costs its watchers is the one a changed
        // right to list needs.
        self.table().node_removed(node);
    }
}

#[cfg(test)]
#[path = "changelog_tests.rs"]
mod tests;
