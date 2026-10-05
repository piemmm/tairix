//! How a volume's cache wrapper reports each mutation to the volume's watch
//! table (`crate::fswatch`, `docs/src/filesystem/watch.md`).
//!
//! The driver surface is keyed `(dir, name)` for every namespace and content
//! change, which is exactly what a directory's journal records. Two facts it
//! does not carry are recovered from a short trail of the bindings the
//! wrapper itself resolved: which entry names a directory in *its* parent
//! (an entry added inside a subfolder changes the subfolder's own entry), and
//! which entry a node-keyed metadata change was made through. Every operation
//! walks its path through this wrapper before it mutates, so the binding is
//! there; one that is not leaves that single report unmade rather than
//! guessed.

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::NodeId;
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
    fn name(&self) -> &[u8] {
        &self.name[..self.len]
    }

    fn wipe(&mut self) {
        self.name[..self.len].zeroize();
        self.len = 0;
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
            self.slots.push(Binding {
                node,
                dir,
                used,
                len: 0,
                name: [0; MAX_COMPONENT_LEN],
            });
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
        binding.wipe();
        binding.node = node;
        binding.dir = dir;
        binding.used = used;
        binding.len = name.len();
        binding.name[..name.len()].copy_from_slice(name);
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

impl Drop for Trail {
    fn drop(&mut self) {
        for binding in &mut self.slots {
            binding.wipe();
        }
    }
}

/// A volume's claimed watch table and the trail that attributes what its
/// driver surface leaves anonymous. Dropping it lets the table go.
pub(crate) struct ChangeLog {
    claim: Claim,
    trail: Trail,
}

impl ChangeLog {
    pub(crate) fn new(claim: Claim) -> Self {
        Self {
            claim,
            trail: Trail::new(),
        }
    }

    fn table(&self) -> &VolumeWatch {
        self.claim.table()
    }

    /// Whether anything on the volume is watched.
    pub(crate) fn active(&self) -> bool {
        self.table().active()
    }

    /// `dir/name` resolved to `node`. Kept only while something on the volume
    /// is watched; unbinding is tracked always, so nothing kept goes stale.
    pub(crate) fn resolved(&mut self, node: NodeId, dir: NodeId, name: &[u8]) {
        if self.table().active() {
            self.trail.record(node.raw(), dir.raw(), name);
        }
    }

    fn within(&self, dir: NodeId) -> Option<(NodeId, &[u8])> {
        self.trail.binding_of(dir.raw())
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
        match self.trail.binding_of(node.raw()) {
            Some((was_dir, was_name)) => self.table().entry_changed(was_dir, was_name, Some(node)),
            None => self.table().node_changed(node),
        }
        self.added(dir, name, Some(node));
    }

    /// `dir/name` was removed; `victim` is what it named, when known.
    pub(crate) fn removed(&mut self, dir: NodeId, name: &[u8], victim: Option<NodeId>) {
        let dir_raw = dir.raw();
        self.trail.forget(|b| {
            (b.dir == dir_raw && b.name() == name) || victim.is_some_and(|v| v.raw() == b.node)
        });
        self.table().entries_changed(dir, name, self.within(dir));
        if let Some(victim) = victim {
            self.table().node_relocated(victim);
        }
    }

    /// `src_dir/src_name` moved to `dst_dir/dst_name`, replacing whatever
    /// `overwritten` named there.
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
        self.trail.forget(|b| {
            (b.dir == src_raw && b.name() == src_name)
                || (b.dir == dst_raw && b.name() == dst_name)
                || [moved, overwritten]
                    .into_iter()
                    .flatten()
                    .any(|n| n.raw() == b.node)
        });
        self.table()
            .entries_changed(src_dir, src_name, self.within(src_dir));
        if dst_raw != src_raw || dst_name != src_name {
            self.table()
                .entries_changed(dst_dir, dst_name, self.within(dst_dir));
        }
        for node in [moved, overwritten].into_iter().flatten() {
            self.table().node_relocated(node);
        }
        if let Some(moved) = moved {
            self.resolved(moved, dst_dir, dst_name);
        }
    }

    /// The contents of `dir/name` changed; `node` is what it names.
    pub(crate) fn written(&mut self, dir: NodeId, name: &[u8], node: Option<NodeId>) {
        self.table().entry_changed(dir, name, node);
    }

    /// A directory or symbolic link was renamed, removed or replaced.
    pub(crate) fn paths_moved(&self) {
        self.table().paths_moved();
    }

    /// A directory's security changed.
    pub(crate) fn access_moved(&self) {
        self.table().access_moved();
    }

    /// `node`'s metadata changed, through the entry the trail last resolved
    /// it under.
    pub(crate) fn metadata(&mut self, node: NodeId) {
        match self.trail.binding_of(node.raw()) {
            Some((dir, name)) => self.table().entry_changed(dir, name, Some(node)),
            None => self.table().node_changed(node),
        }
    }
}

#[cfg(test)]
#[path = "changelog_tests.rs"]
mod tests;
