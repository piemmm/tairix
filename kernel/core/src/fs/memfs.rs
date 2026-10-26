//! A shared in-memory `FilesystemRead` + `FilesystemWrite` +
//! `FilesystemSecurity` driver for the `fs` module's tests.
//!
//! `RwMockFs` is a small allocation-backed filesystem implementing the ABI's
//! mutation model — name mutations by `(dir, name)`, byte writes by node —
//! standing in for a block-backed `drivers/filesystem/*` driver
//! (`kernel/core` may not depend on `drivers/*`). It is the one definition
//! shared by the delegation tests (`delegate_tests.rs`) and the
//! mounted-service tests (`mounted_tests.rs`), so neither carries its own
//! copy.
//!
//! A node is born with the record its create hands over. The fixtures create
//! theirs with [`ADMIN_RECORD`], so a test varies only the node it cares
//! about.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use core::ops::Bound;

use tairix_abi::driver::filesystem::{
    AllocationClass, DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs,
    FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite,
    NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned,
    VolumeHost, VolumeStats,
};
use tairix_abi::driver::{DriverError, DriverHandle};
use tairix_abi::SeekRegion;
use tairix_collections::RadixTree;
use tairix_fsmeta::{AttrFlags, AttrSet};

/// The uid the fixtures' records and the root are owned by.
pub(crate) const ADMIN_UID: u32 = 1;
/// The gid the fixtures' records and the root are owned by.
pub(crate) const ADMIN_GID: u32 = 1;
/// The record the fixtures create their nodes with: admin-owned and
/// world-traversable.
pub(crate) const ADMIN_RECORD: NodeSecurity = NodeSecurity::new(0o755, ADMIN_UID, ADMIN_GID);
/// The record of a fixture node the system user owns, world-traversable, as
/// an image lays its skeleton down.
#[cfg(test)]
pub(crate) const SYSTEM_RECORD: NodeSecurity = NodeSecurity::new(0o755, 0, 0);

/// One node of the in-memory tree.
enum RwNode {
    /// A directory: child name → node index.
    Dir(BTreeMap<String, usize>),
    /// A regular file: its byte contents.
    File(Vec<u8>),
    /// A symbolic link: its stored target, verbatim and unresolved.
    Link(Vec<u8>),
}

/// What gives a file a new number, beyond a create and a reused number.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Renumbering {
    /// Nothing: a node keeps its number for as long as it lives.
    Never,
    /// Every byte write, as ADFS renumbers an object whose data moves.
    OnWrite,
    /// A rename, as a FAT file's number is where its entry lies.
    OnRename,
}

/// An in-memory read/write filesystem with a per-node security record.
pub struct RwMockFs {
    nodes: Vec<RwNode>,
    sec: Vec<NodeSecurity>,
    /// How many directory entries name each node, parallel to `nodes`.
    ///
    /// This tree holds no `.`/`..` entries, so the count is exactly the
    /// names that reach the node and a directory is not given POSIX's
    /// extra two. The root, which no directory holds, counts as the one
    /// name its mount point provides.
    nlink: Vec<u32>,
    /// The directory each node was made in or last moved to, parallel to
    /// `nodes`: what [`FilesystemRead::parent`] answers for a directory, which
    /// has the one name. The root's is itself.
    parent: Vec<usize>,
    /// Per-node extended-attribute set, parallel to `nodes`/`sec`,
    /// validated through the one `lib/fsmeta` definition.
    attrs: Vec<AttrSet>,
    /// How many holds [`FilesystemRead::pin`] has on each held node: a node
    /// kept past its last name, and a number never given to another node.
    held: RadixTree<u32>,
    /// Folding by default, so the cache's and the watch's folding paths stay
    /// exercised; [`Self::with_exact_names`] stands in for an exact format.
    name_matching: NameMatching,
    /// Bumped by every name added or removed, so a listing cursor names a
    /// place in one version of a directory and no other.
    generation: u32,
    /// The numbers of freed nodes, which a create takes again lowest first,
    /// as FAT's clusters and ext4's inodes are; `None` keeps every number for
    /// good.
    free: Option<BTreeSet<usize>>,
    /// Run on entry to every lookup, so a test can check what its caller
    /// holds while inside the driver.
    lookup_probe: Option<Box<dyn Fn() + Send>>,
    /// Run as the driver is dropped, so a test can check what its caller
    /// holds then.
    drop_probe: Option<Box<dyn FnOnce() + Send>>,
    /// What every security store answers instead of storing, so a test can
    /// stand in for a format that holds no record.
    security_refusal: Option<DriverError>,
    /// What every pin answers instead of holding the node, so a test can
    /// stand in for a driver that refuses one.
    pin_refusal: Option<DriverError>,
    /// What every unpin answers instead of letting the hold go.
    unpin_refusal: Option<DriverError>,
    /// How long ids are declared to last.
    node_identity: NodeIdentity,
    /// What, if anything, gives a file a new number.
    renumbering: Renumbering,
    /// How far above its id each node's reported number lies.
    ino_bias: u64,
    /// Run on entry to every space-accounting read, so a test can count them.
    stats_probe: Option<Box<dyn Fn() + Send>>,
    /// The class the kernel set for the operations that follow.
    class: AllocationClass,
    /// Nodes whose ownership the format stands in for rather than records,
    /// as an ownerless format's are.
    unrecorded: BTreeSet<usize>,
    /// Each file's holes, parallel to `nodes`, so a seek reports real holes
    /// and a wrapper that answers "all data" itself is caught.
    holes: Vec<Holes>,
    /// The stamps every node reports.
    times: NodeTimes,
    /// What every byte read answers instead of reading, so a test can stand
    /// in for a faulting device.
    read_refusal: Option<DriverError>,
    /// The name every listed entry carries instead of its own, so a test can
    /// stand in for a corrupt directory.
    listed_as: Option<&'static [u8]>,
    /// Whether a listing hands every entry the cursor it started at.
    stuck_cursor: bool,
    /// Whether the format keeps links.
    links: bool,
    /// What every name mutation and byte write answers instead of making it,
    /// so a test can observe what the VFS makes of a driver's own refusal.
    mutation_refusal: Option<DriverError>,
}

/// A file's holes as `start → end` byte ranges: disjoint, never adjacent.
type Holes = BTreeMap<u64, u64>;

/// Add `[start, end)` to `holes`, merging what it touches.
fn hole_insert(holes: &mut Holes, mut start: u64, mut end: u64) {
    if start >= end {
        return;
    }
    let touching: Vec<(u64, u64)> = holes
        .range(..=end)
        .filter(|&(_, &e)| e >= start)
        .map(|(&s, &e)| (s, e))
        .collect();
    for (s, e) in touching {
        holes.remove(&s);
        start = start.min(s);
        end = end.max(e);
    }
    holes.insert(start, end);
}

/// Take `[start, end)` out of `holes`, splitting a hole it lands inside.
fn hole_remove(holes: &mut Holes, start: u64, end: u64) {
    if start >= end {
        return;
    }
    let overlapping: Vec<(u64, u64)> = holes
        .range(..end)
        .filter(|&(_, &e)| e > start)
        .map(|(&s, &e)| (s, e))
        .collect();
    for (s, e) in overlapping {
        holes.remove(&s);
        if s < start {
            holes.insert(s, start);
        }
        if e > end {
            holes.insert(end, e);
        }
    }
}

/// Where the next region of `region`'s kind begins at or after `offset` of a
/// `size`-byte file with `holes`.
fn hole_seek(holes: &Holes, offset: u64, size: u64, region: SeekRegion) -> Option<u64> {
    if offset >= size {
        return None;
    }
    let inside = holes
        .range(..=offset)
        .next_back()
        .filter(|&(_, &e)| e > offset)
        .map(|(_, &e)| e);
    match (region, inside) {
        // Already in a region of the kind asked for.
        (SeekRegion::Hole, Some(_)) | (SeekRegion::Data, None) => Some(offset),
        (SeekRegion::Hole, None) => Some(
            holes
                .range(offset..)
                .next()
                .map_or(size, |(&s, _)| s.min(size)),
        ),
        (SeekRegion::Data, Some(end)) => (end < size).then_some(end),
    }
}

/// The one name this mock keeps for the system, as a volume keeps its
/// reserve: creating it is refused unless the class the kernel set is
/// [`AllocationClass::System`], which is what makes a wrapper's forwarding of
/// the class observable
/// ([`super::wrapper_conformance::assert_allocation_class_forwards`]).
pub const SYSTEM_ONLY_NAME: &[u8] = b"system-only";

impl Drop for RwMockFs {
    fn drop(&mut self) {
        if let Some(probe) = self.drop_probe.take() {
            probe();
        }
    }
}

impl Default for RwMockFs {
    fn default() -> Self {
        Self::new()
    }
}

impl RwMockFs {
    /// A fresh filesystem with an empty, admin-owned, world-traversable root.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: alloc::vec![RwNode::Dir(BTreeMap::new())],
            // Root is admin-owned and world-traversable by default, so a test
            // can vary just the node it cares about.
            sec: alloc::vec![NodeSecurity::new(0o755, ADMIN_UID, ADMIN_GID)],
            nlink: alloc::vec![1],
            parent: alloc::vec![0],
            generation: 0,
            attrs: alloc::vec![AttrSet::new()],
            held: RadixTree::new(),
            name_matching: NameMatching::AsciiCaseInsensitive,
            free: None,
            lookup_probe: None,
            drop_probe: None,
            security_refusal: None,
            pin_refusal: None,
            unpin_refusal: None,
            node_identity: NodeIdentity::Stable,
            renumbering: Renumbering::Never,
            ino_bias: 0,
            stats_probe: None,
            class: AllocationClass::Ordinary,
            unrecorded: BTreeSet::new(),
            holes: alloc::vec![Holes::new()],
            times: NodeTimes::default(),
            read_refusal: None,
            listed_as: None,
            stuck_cursor: false,
            links: true,
            mutation_refusal: None,
        }
    }

    /// Stand `node`'s record in for one the format does not keep, so
    /// [`FilesystemSecurity::records_ownership`] answers `false` for it.
    pub fn forget_ownership(&mut self, node: NodeId) {
        if let Ok(idx) = Self::index(node) {
            self.unrecorded.insert(idx);
        }
    }

    /// Match names byte-for-byte, as ARXFS and ext4 do.
    #[must_use]
    pub fn with_exact_names(mut self) -> Self {
        self.name_matching = NameMatching::Exact;
        self
    }

    /// Declare ids that may follow contents, as ADFS's do, so a layer that
    /// passes the declaration on is seen to. The ids themselves stay put.
    #[must_use]
    pub fn with_content_derived_ids(mut self) -> Self {
        self.node_identity = NodeIdentity::ContentDerived;
        self
    }

    /// Give an unheld file a new number at every byte write to it, declaring
    /// ids that follow contents, as ADFS does when an object's data moves.
    #[must_use]
    pub fn with_renumbering_writes(mut self) -> Self {
        self.renumbering = Renumbering::OnWrite;
        self.node_identity = NodeIdentity::ContentDerived;
        self
    }

    /// Give an unheld file a new number when it is renamed, as FAT names a
    /// file by where its entry lies; with reused numbers the next node made
    /// takes the old one.
    #[must_use]
    pub fn with_renumbering_renames(mut self) -> Self {
        self.renumbering = Renumbering::OnRename;
        self
    }

    /// Report each node's number `bias` above its id, so a layer that reports
    /// the id where the number belongs is caught.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_ino_bias(mut self, bias: u64) -> Self {
        self.ino_bias = bias;
        self
    }

    /// Give a freed node's number to the next node made, lowest first.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_reused_numbers(mut self) -> Self {
        self.free = Some(BTreeSet::new());
        self
    }

    /// Run `probe` as the driver is dropped.
    #[cfg(test)]
    pub(crate) fn on_drop(&mut self, probe: Box<dyn FnOnce() + Send>) {
        self.drop_probe = Some(probe);
    }

    /// Run `probe` on entry to every lookup.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_lookup_probe(mut self, probe: Box<dyn Fn() + Send>) -> Self {
        self.lookup_probe = Some(probe);
        self
    }

    /// Answer every security store with `refusal`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_security_refused(mut self, refusal: DriverError) -> Self {
        self.security_refusal = Some(refusal);
        self
    }

    /// Answer every pin from now on with `refusal`, or hold again for `None`.
    #[cfg(test)]
    pub(crate) fn refuse_pins(&mut self, refusal: Option<DriverError>) {
        self.pin_refusal = refusal;
    }

    /// Answer every unpin from now on with `refusal`, or let holds go again
    /// for `None`.
    #[cfg(test)]
    pub(crate) fn refuse_unpins(&mut self, refusal: Option<DriverError>) {
        self.unpin_refusal = refusal;
    }

    /// Run `probe` on entry to every space-accounting read.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_stats_probe(mut self, probe: Box<dyn Fn() + Send>) -> Self {
        self.stats_probe = Some(probe);
        self
    }

    /// Report `times` for every node.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_times(mut self, times: NodeTimes) -> Self {
        self.times = times;
        self
    }

    /// Answer every byte read with `refusal`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_reads_refused(mut self, refusal: DriverError) -> Self {
        self.read_refusal = Some(refusal);
        self
    }

    /// List every entry under `name`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_entries_listed_as(mut self, name: &'static [u8]) -> Self {
        self.listed_as = Some(name);
        self
    }

    /// Hand every listed entry the cursor its listing started at.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_stuck_cursor(mut self) -> Self {
        self.stuck_cursor = true;
        self
    }

    /// Keep no links, as FAT32 and ADFS keep none: making one, reading one
    /// and giving a node a second name are each unsupported.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn without_links(mut self) -> Self {
        self.links = false;
        self
    }

    /// Answer every name mutation and byte write with `refusal`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_mutations_refused(mut self, refusal: DriverError) -> Self {
        self.mutation_refusal = Some(refusal);
        self
    }

    /// Refuse a mutation the knobs say the volume refuses.
    fn mutable(&self) -> Result<(), DriverError> {
        self.mutation_refusal.map_or(Ok(()), Err)
    }

    /// Refuse a link operation on a format that keeps none.
    fn linkable(&self) -> Result<(), DriverError> {
        if self.links {
            Ok(())
        } else {
            Err(DriverError::Unsupported)
        }
    }

    /// Place a new node in directory `dir`, born with `record`, under a freed
    /// number where one is reused, else a fresh one.
    fn place(&mut self, node: RwNode, dir: usize, record: &NodeSecurity) -> usize {
        let sec = *record;
        if let Some(index) = self.free.as_mut().and_then(BTreeSet::pop_first) {
            self.nodes[index] = node;
            self.sec[index] = sec;
            self.unrecorded.remove(&index);
            self.holes[index] = Holes::new();
            self.attrs[index] = AttrSet::new();
            self.nlink[index] = 1;
            self.parent[index] = dir;
            return index;
        }
        self.nodes.push(node);
        self.sec.push(sec);
        self.holes.push(Holes::new());
        self.attrs.push(AttrSet::new());
        self.nlink.push(1);
        self.parent.push(dir);
        self.nodes.len() - 1
    }

    /// Overwrite the root directory's security record.
    #[cfg(test)]
    pub(crate) fn set_root_security(&mut self, sec: NodeSecurity) {
        self.sec[0] = sec;
    }

    /// Set `node`'s recorded name count, so a test can stand a node at the
    /// format's fixed ceiling without adding four billion names to reach it.
    #[cfg(test)]
    pub(crate) fn set_link_count(&mut self, node: NodeId, count: u32) {
        if let Ok(idx) = Self::index(node) {
            if let Some(slot) = self.nlink.get_mut(idx) {
                *slot = count;
            }
        }
    }

    fn index(node: NodeId) -> Result<usize, DriverError> {
        let raw = node.raw();
        if raw == 0 {
            return Err(DriverError::NotFound);
        }
        usize::try_from(raw - 1).map_err(|_| DriverError::NotFound)
    }

    /// The index of the node `node` names while a name or a hold reaches it.
    fn live(&self, node: NodeId) -> Result<usize, DriverError> {
        let idx = Self::index(node)?;
        match self.nlink.get(idx) {
            Some(&names) if names > 0 || self.held.contains_key(idx as u64) => Ok(idx),
            _ => Err(DriverError::NotFound),
        }
    }

    /// The index of the node `node` names while an entry still names it.
    fn named(&self, node: NodeId) -> Result<usize, DriverError> {
        let idx = Self::index(node)?;
        match self.nlink.get(idx) {
            Some(&names) if names > 0 => Ok(idx),
            _ => Err(DriverError::NotFound),
        }
    }

    /// The index of the directory `dir` while an entry still names it: a
    /// removed one takes no entries, held or not.
    fn named_dir(&self, dir: NodeId) -> Result<usize, DriverError> {
        let idx = self.named(dir)?;
        match self.nodes.get(idx) {
            Some(RwNode::Dir(_)) => Ok(idx),
            Some(RwNode::File(_) | RwNode::Link(_)) => Err(DriverError::Unsupported),
            None => Err(DriverError::NotFound),
        }
    }

    /// Free the node at `idx`, which no name and no hold reaches: its storage
    /// goes, and its number is the next node's where numbers are reused.
    fn release(&mut self, idx: usize) {
        self.nodes[idx] = RwNode::File(Vec::new());
        self.holes[idx] = Holes::new();
        self.attrs[idx] = AttrSet::new();
        self.unrecorded.remove(&idx);
        if let Some(free) = self.free.as_mut() {
            free.insert(idx);
        }
    }

    /// One name fewer reaches the node at `idx`; with none and no hold left,
    /// it is freed.
    fn drop_name(&mut self, idx: usize) {
        let Some(count) = self.nlink.get_mut(idx) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 && !self.held.contains_key(idx as u64) {
            self.release(idx);
        }
    }

    /// Move the node at `idx` to a fresh number, repointing every entry that
    /// named it, and free `idx` for the next node made where numbers are
    /// reused. The fresh number is never `idx`: the new place is taken before
    /// the old one is given up. A held node is never moved: its id names it
    /// until its last unpin.
    fn renumber(&mut self, idx: usize) {
        if self.held.contains_key(idx as u64) {
            return;
        }
        let fresh = self.nodes.len();
        let node = core::mem::replace(&mut self.nodes[idx], RwNode::File(Vec::new()));
        self.nodes.push(node);
        let sec = self.sec[idx];
        self.sec.push(sec);
        let holes = core::mem::take(&mut self.holes[idx]);
        self.holes.push(holes);
        let attrs = core::mem::replace(&mut self.attrs[idx], AttrSet::new());
        self.attrs.push(attrs);
        let names = core::mem::replace(&mut self.nlink[idx], 0);
        self.nlink.push(names);
        let parent = self.parent[idx];
        self.parent.push(parent);
        if self.unrecorded.remove(&idx) {
            self.unrecorded.insert(fresh);
        }
        for node in &mut self.nodes {
            if let RwNode::Dir(children) = node {
                for child in children.values_mut().filter(|child| **child == idx) {
                    *child = fresh;
                }
            }
        }
        if let Some(free) = self.free.as_mut() {
            free.insert(idx);
        }
    }

    /// A byte write to the file at `idx` landed, which moves it where writes
    /// renumber.
    fn written(&mut self, idx: usize) {
        if self.renumbering == Renumbering::OnWrite {
            self.renumber(idx);
        }
    }

    fn child_index(&self, dir: NodeId, name: &[u8]) -> Result<Option<usize>, DriverError> {
        let idx = self.named_dir(dir)?;
        let RwNode::Dir(children) = &self.nodes[idx] else {
            return Err(DriverError::Unsupported);
        };
        let needle = core::str::from_utf8(name).map_err(|_| DriverError::NotFound)?;
        Ok(self.entry(children, needle).map(|(_, node)| node))
    }

    /// The spelling directory `dir_idx` stores for the entry `name` matches.
    fn stored_name(&self, dir_idx: usize, name: &[u8]) -> Option<&[u8]> {
        let RwNode::Dir(children) = self.nodes.get(dir_idx)? else {
            return None;
        };
        let name = core::str::from_utf8(name).ok()?;
        self.entry(children, name)
            .map(|(stored, _)| stored.as_bytes())
    }

    /// The stored spelling and node of the entry in `children` that `name`
    /// matches: one map lookup when names are exact, a scan when they fold.
    fn entry<'a>(
        &self,
        children: &'a BTreeMap<String, usize>,
        name: &str,
    ) -> Option<(&'a str, usize)> {
        if self.name_matching == NameMatching::Exact {
            return children
                .get_key_value(name)
                .map(|(stored, &node)| (stored.as_str(), node));
        }
        children
            .iter()
            .find(|(stored, _)| {
                self.name_matching
                    .matches(stored.as_bytes(), name.as_bytes())
            })
            .map(|(stored, &node)| (stored.as_str(), node))
    }

    /// Take the entry named `name` out of directory `dir_idx`, returning the
    /// node it named, whose count of names is the caller's to settle.
    ///
    /// Keyed by name, not by node: one directory may hold two names for one
    /// node, and finding the entry by node index would drop whichever the
    /// map happened to yield first.
    fn detach_name(&mut self, dir_idx: usize, name: &str) -> Option<usize> {
        let matching = self.name_matching;
        let RwNode::Dir(children) = &mut self.nodes[dir_idx] else {
            return None;
        };
        let key = children
            .keys()
            .find(|k| matching.matches(k.as_bytes(), name.as_bytes()))
            .cloned()?;
        let child = children.remove(&key)?;
        self.generation = self.generation.wrapping_add(1);
        Some(child)
    }

    /// Whether `target_idx` is `root_idx` or anywhere within its subtree,
    /// used to refuse moving a directory into its own descendants.
    fn is_in_subtree(&self, root_idx: usize, target_idx: usize) -> bool {
        if root_idx == target_idx {
            return true;
        }
        if let Some(RwNode::Dir(children)) = self.nodes.get(root_idx) {
            for &child in children.values() {
                if self.is_in_subtree(child, target_idx) {
                    return true;
                }
            }
        }
        false
    }
}

impl FilesystemRead for RwMockFs {
    fn root(&self) -> NodeId {
        NodeId::from_raw(1)
    }

    fn name_matching(&self) -> NameMatching {
        self.name_matching
    }

    fn node_identity(&self) -> NodeIdentity {
        self.node_identity
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        let idx = self.live(node)?;
        let names = self.nlink.get(idx).copied().ok_or(DriverError::NotFound)?;
        let ino = node.raw().saturating_add(self.ino_bias);
        // The in-RAM tree keeps no per-node stamps: every node reports the
        // one set it was given, by default the epoch, the documented "no
        // stamp" value, never a fabricated wall time.
        let times = self.times;
        match self.nodes.get(idx).ok_or(DriverError::NotFound)? {
            RwNode::Dir(_) => Ok(NodeInfo {
                kind: NodeKind::Directory,
                nlink: names,
                ino,
                size: 0,
                allocated: 0,
                times,
                content_gen: NodeInfo::NO_CONTENT_GEN,
            }),
            RwNode::File(data) => Ok(NodeInfo {
                kind: NodeKind::RegularFile,
                nlink: names,
                ino,
                size: data.len() as u64,
                // Heap-backed: the bytes held are the storage occupied.
                allocated: data.len() as u64,
                times,
                content_gen: NodeInfo::NO_CONTENT_GEN,
            }),
            RwNode::Link(target) => Ok(NodeInfo {
                kind: NodeKind::Symlink,
                nlink: names,
                ino,
                size: target.len() as u64,
                allocated: target.len() as u64,
                times,
                content_gen: NodeInfo::NO_CONTENT_GEN,
            }),
        }
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        if let Some(probe) = &self.lookup_probe {
            probe();
        }
        match self.child_index(dir, name)? {
            Some(i) => Ok(NodeId::from_raw(i as u64 + 1)),
            None => Err(DriverError::NotFound),
        }
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        let idx = self.named_dir(dir)?;
        let parent = self.parent.get(idx).ok_or(DriverError::NotFound)?;
        Ok(NodeId::from_raw(*parent as u64 + 1))
    }

    fn seek_region(
        &mut self,
        file: NodeId,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        let idx = self.live(file)?;
        let RwNode::File(data) = self.nodes.get(idx).ok_or(DriverError::NotFound)? else {
            return Err(DriverError::Unsupported);
        };
        Ok(hole_seek(
            &self.holes[idx],
            offset,
            data.len() as u64,
            region,
        ))
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        if let Some(refusal) = self.read_refusal {
            return Err(refusal);
        }
        let idx = self.live(file)?;
        let RwNode::File(data) = self.nodes.get(idx).ok_or(DriverError::NotFound)? else {
            return Err(DriverError::Unsupported);
        };
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        if start >= data.len() {
            return Ok(0);
        }
        let n = core::cmp::min(buf.len(), data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        self.linkable()?;
        let idx = self.live(link)?;
        let RwNode::Link(target) = self.nodes.get(idx).ok_or(DriverError::NotFound)? else {
            return Err(DriverError::Unsupported);
        };
        if out.len() < target.len() {
            // The whole target or nothing: a truncated path would resolve
            // somewhere else entirely.
            return Err(DriverError::BufferTooSmall);
        }
        out[..target.len()].copy_from_slice(target);
        Ok(target.len())
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let idx = self.live(dir)?;
        let RwNode::Dir(children) = self.nodes.get(idx).ok_or(DriverError::NotFound)? else {
            return Err(DriverError::Unsupported);
        };
        // Names are kept in order, so a listing resumes just past the one it
        // last took, a place no insertion or removal moves. The cursor adds
        // the generation and position only so that each names one version
        // of the directory.
        let rest: Vec<(usize, String, usize)> = if cursor == 0 {
            children
                .iter()
                .enumerate()
                .map(|(at, (name, &child))| (at, name.clone(), child))
                .collect()
        } else {
            let Ok(after) = core::str::from_utf8(after) else {
                return Ok(());
            };
            let skipped = children
                .range::<str, _>((Bound::Unbounded, Bound::Included(after)))
                .count();
            children
                .range::<str, _>((Bound::Excluded(after), Bound::Unbounded))
                .enumerate()
                .map(|(at, (name, &child))| (skipped + at, name.clone(), child))
                .collect()
        };
        for (at, name, child) in rest {
            let node = NodeId::from_raw(child as u64 + 1);
            let next_cursor = if self.stuck_cursor {
                cursor
            } else {
                (u64::from(self.generation) << 32) | (at as u64 + 1)
            };
            let entry = DirEntry {
                node,
                info: self.node_info(node)?,
                next_cursor,
            };
            let listed = self.listed_as.unwrap_or(name.as_bytes());
            if visit(&entry, listed) == DirVisit::Stop {
                break;
            }
        }
        Ok(())
    }

    /// The id a node is reported under is its id throughout, so a pin answers
    /// it unchanged.
    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        if let Some(refusal) = self.pin_refusal {
            return Err(refusal);
        }
        let key = self.live(node)? as u64;
        if let Some(holds) = self.held.get_mut(key) {
            *holds = holds.checked_add(1).ok_or(DriverError::OutOfMemory)?;
        } else {
            self.held
                .try_insert(key, 1)
                .map_err(|_| DriverError::OutOfMemory)?;
        }
        Ok(node)
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        if let Some(refusal) = self.unpin_refusal {
            return Err(refusal);
        }
        let idx = Self::index(pinned)?;
        let holds = self.held.get_mut(idx as u64).ok_or(DriverError::NotFound)?;
        *holds = holds.saturating_sub(1);
        if *holds > 0 {
            return Ok(Unpinned::Held);
        }
        self.held.remove(idx as u64);
        if self.nlink.get(idx).is_some_and(|&names| names > 0) {
            return Ok(Unpinned::Released);
        }
        self.release(idx);
        Ok(Unpinned::Freed)
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        let held = core::mem::take(&mut self.held);
        for (key, _) in &held {
            let Ok(idx) = usize::try_from(key) else {
                continue;
            };
            if self.nlink.get(idx).is_some_and(|&names| names == 0) {
                self.release(idx);
            }
        }
        Ok(())
    }
}

impl FilesystemWrite for RwMockFs {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.mutable()?;
        if self.child_index(dir, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        if name == SYSTEM_ONLY_NAME && self.class != AllocationClass::System {
            return Err(DriverError::NoSpace);
        }
        let name = core::str::from_utf8(name)
            .map_err(|_| DriverError::LengthOutOfRange)?
            .to_string();
        let node = match kind {
            NodeKind::Directory => RwNode::Dir(BTreeMap::new()),
            NodeKind::RegularFile => RwNode::File(Vec::new()),
            // A link carries a target this call has nowhere to put.
            NodeKind::Symlink => return Err(DriverError::Unsupported),
        };
        let dir_idx = Self::index(dir)?;
        let new_index = self.place(node, dir_idx, record);
        if let RwNode::Dir(children) = &mut self.nodes[dir_idx] {
            children.insert(name, new_index);
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(NodeId::from_raw(new_index as u64 + 1))
    }

    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.mutable()?;
        self.linkable()?;
        if self.child_index(dir, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        if target.is_empty() {
            return Err(DriverError::LengthOutOfRange);
        }
        let name = core::str::from_utf8(name)
            .map_err(|_| DriverError::LengthOutOfRange)?
            .to_string();
        let dir_idx = Self::index(dir)?;
        let new_index = self.place(RwNode::Link(target.to_vec()), dir_idx, record);
        if let RwNode::Dir(children) = &mut self.nodes[dir_idx] {
            children.insert(name, new_index);
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(NodeId::from_raw(new_index as u64 + 1))
    }

    fn link(&mut self, dir: NodeId, name: &[u8], node: NodeId) -> Result<(), DriverError> {
        self.mutable()?;
        self.linkable()?;
        if self.child_index(dir, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        let target = self.named(node)?;
        // A second name for a directory would let the tree hold a cycle,
        // which the physical `..` walk depends on being impossible.
        match self.nodes.get(target).ok_or(DriverError::NotFound)? {
            RwNode::Dir(_) => return Err(DriverError::Unsupported),
            RwNode::File(_) | RwNode::Link(_) => {}
        }
        let count = self.nlink.get_mut(target).ok_or(DriverError::NotFound)?;
        // The stored count is a fixed-width field, so an overflow fails
        // closed rather than wrapping into a count that frees live data.
        *count = count.checked_add(1).ok_or(DriverError::TooManyLinks)?;
        let name = core::str::from_utf8(name)
            .map_err(|_| DriverError::LengthOutOfRange)?
            .to_string();
        let dir_idx = Self::index(dir)?;
        if let RwNode::Dir(children) = &mut self.nodes[dir_idx] {
            children.insert(name, target);
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(())
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        self.mutable()?;
        let child = self.live(file)?;
        let RwNode::File(body) = &mut self.nodes[child] else {
            return Err(DriverError::Unsupported);
        };
        let start = usize::try_from(offset).map_err(|_| DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(data.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        let old = body.len() as u64;
        if body.len() < start {
            body.resize(start, 0);
        }
        // Only a gap past the end is zero-filled; written bytes are copied
        // once, in bulk.
        let overlap = body.len().min(end) - start;
        body[start..start + overlap].copy_from_slice(&data[..overlap]);
        body.extend_from_slice(&data[overlap..]);
        // A gap a write leaves past the old end stores nothing, as a
        // hole-keeping format's does.
        hole_insert(&mut self.holes[child], old, offset);
        hole_remove(&mut self.holes[child], offset, end as u64);
        self.written(child);
        Ok(data.len())
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        self.mutable()?;
        let child = self.live(file)?;
        let RwNode::File(body) = &mut self.nodes[child] else {
            return Err(DriverError::Unsupported);
        };
        let new = usize::try_from(size).map_err(|_| DriverError::LengthOutOfRange)?;
        let old = body.len() as u64;
        body.resize(new, 0);
        let holes = &mut self.holes[child];
        if size > old {
            hole_insert(holes, old, size);
        } else {
            hole_remove(holes, size, u64::MAX);
        }
        self.written(child);
        Ok(())
    }

    fn punch(&mut self, file: NodeId, offset: u64, len: u64) -> Result<(), DriverError> {
        self.mutable()?;
        let child = self.live(file)?;
        let RwNode::File(body) = &mut self.nodes[child] else {
            return Err(DriverError::Unsupported);
        };
        let end = offset.checked_add(len).ok_or(DriverError::OutOfRange)?;
        let size = body.len() as u64;
        let (start, end) = (offset.min(size), end.min(size));
        let span = usize::try_from(start).map_err(|_| DriverError::LengthOutOfRange)?
            ..usize::try_from(end).map_err(|_| DriverError::LengthOutOfRange)?;
        body[span].fill(0);
        hole_insert(&mut self.holes[child], start, end);
        self.written(child);
        Ok(())
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.mutable()?;
        let child = self.child_index(dir, name)?.ok_or(DriverError::NotFound)?;
        if let RwNode::Dir(children) = &self.nodes[child] {
            if !children.is_empty() {
                return Err(DriverError::DirectoryNotEmpty);
            }
        }
        let key = core::str::from_utf8(name).map_err(|_| DriverError::NotFound)?;
        let dir_idx = Self::index(dir)?;
        if let Some(gone) = self.detach_name(dir_idx, key) {
            self.drop_name(gone);
        }
        Ok(())
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.mutable()?;
        let src_idx = self
            .child_index(src_dir, src_name)?
            .ok_or(DriverError::NotFound)?;
        let dst_dir_idx = self.named_dir(dst_dir)?;
        let dst_key = core::str::from_utf8(dst_name)
            .map_err(|_| DriverError::LengthOutOfRange)?
            .to_string();
        let moving_dir = matches!(self.nodes[src_idx], RwNode::Dir(_));

        // Refuse moving a directory into itself or its own subtree.
        if moving_dir && self.is_in_subtree(src_idx, dst_dir_idx) {
            return Err(DriverError::DirectoryCycle);
        }

        match self.child_index(dst_dir, dst_name)? {
            Some(dst_idx) if dst_idx == src_idx => {
                // Another name for the same node is left alone, as POSIX has
                // it; the source's own entry takes the spelling asked for.
                let own_entry =
                    src_dir == dst_dir && self.name_matching.matches(src_name, dst_name);
                if !own_entry || self.stored_name(dst_dir_idx, dst_name) == Some(dst_name) {
                    return Ok(());
                }
            }
            // Replace an existing destination of a compatible kind.
            Some(dst_idx) => {
                let dst_is_dir = matches!(self.nodes[dst_idx], RwNode::Dir(_));
                if dst_is_dir != moving_dir {
                    return Err(DriverError::Unsupported);
                }
                if let RwNode::Dir(children) = &self.nodes[dst_idx] {
                    if !children.is_empty() {
                        return Err(DriverError::DirectoryNotEmpty);
                    }
                }
                if let Some(replaced) = self.detach_name(dst_dir_idx, &dst_key) {
                    self.drop_name(replaced);
                }
            }
            None => {}
        }

        // The node keeps the name it had, moved rather than added.
        let src_key = core::str::from_utf8(src_name).map_err(|_| DriverError::NotFound)?;
        let src_dir_idx = Self::index(src_dir)?;
        self.detach_name(src_dir_idx, src_key);
        if let RwNode::Dir(children) = &mut self.nodes[dst_dir_idx] {
            children.insert(dst_key, src_idx);
            self.generation = self.generation.wrapping_add(1);
        }
        self.parent[src_idx] = dst_dir_idx;
        if self.renumbering == Renumbering::OnRename && !moving_dir {
            self.renumber(src_idx);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    /// Report the mount's state to the host the moment the timer arrives,
    /// exactly as a driver that defers durability does — which is what makes
    /// the forwarding of this method observable through the host alone
    /// ([`super::wrapper_conformance::assert_volume_host_forwards`]).
    fn set_volume_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        host.writeback_due(volume, None);
    }

    fn set_allocation_class(&mut self, class: AllocationClass) {
        self.class = class;
    }
}

impl FilesystemAttrs for RwMockFs {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let idx = self.live(node)?;
        let set = self.attrs.get(idx).ok_or(DriverError::NotFound)?;
        let Some(value) = set.get(key) else {
            return Ok(None);
        };
        let Some(out) = value_out.get_mut(..value.len()) else {
            return Err(DriverError::BufferTooSmall);
        };
        out.copy_from_slice(value);
        Ok(Some(value.len()))
    }

    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
        let idx = self.live(node)?;
        let set = self.attrs.get_mut(idx).ok_or(DriverError::NotFound)?;
        set.set(key, AttrFlags::empty(), value)
            .map_err(DriverError::from)
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let idx = self.live(node)?;
        let set = self.attrs.get(idx).ok_or(DriverError::NotFound)?;
        let Ok(index) = usize::try_from(index) else {
            return Ok(None);
        };
        let Some(entry) = set.iter().nth(index) else {
            return Ok(None);
        };
        let key = entry.key().as_bytes();
        let Some(out) = key_out.get_mut(..key.len()) else {
            return Err(DriverError::BufferTooSmall);
        };
        out.copy_from_slice(key);
        Ok(Some(key.len()))
    }

    fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError> {
        let idx = self.live(node)?;
        let set = self.attrs.get_mut(idx).ok_or(DriverError::NotFound)?;
        if set.remove(key) {
            Ok(())
        } else {
            Err(DriverError::NotFound)
        }
    }
}

impl FilesystemAttrsProvider for RwMockFs {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        Some(self)
    }
}

impl FilesystemSecurity for RwMockFs {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        let idx = self.live(node)?;
        self.sec.get(idx).copied().ok_or(DriverError::NotFound)
    }

    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        if let Some(refusal) = self.security_refusal {
            return Err(refusal);
        }
        let idx = self.live(node)?;
        match self.sec.get_mut(idx) {
            Some(stored) => {
                *stored = security;
                Ok(())
            }
            None => Err(DriverError::NotFound),
        }
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        let idx = self.live(node)?;
        Ok(!self.unrecorded.contains(&idx))
    }
}

/// The mock's fixed allocation unit for its derived space accounting.
const MOCK_BLOCK_SIZE: u32 = 512;

/// The mock's fixed capacity, in [`MOCK_BLOCK_SIZE`] blocks.
const MOCK_TOTAL_BLOCKS: u64 = 4096;

impl FilesystemStats for RwMockFs {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        if let Some(probe) = &self.stats_probe {
            probe();
        }
        // Derived from the live tree, not fabricated: each file occupies
        // whole blocks of its byte length, over a fixed test capacity, so a
        // test observes the free count move with real writes. There is no
        // reserve and no fixed inode table.
        let used: u64 = self
            .nodes
            .iter()
            .map(|node| match node {
                RwNode::File(bytes) | RwNode::Link(bytes) => {
                    (bytes.len() as u64).div_ceil(u64::from(MOCK_BLOCK_SIZE))
                }
                RwNode::Dir(_) => 0,
            })
            .sum();
        let free = MOCK_TOTAL_BLOCKS.saturating_sub(used);
        Ok(VolumeStats {
            block_size: MOCK_BLOCK_SIZE,
            in_reserve: false,
            total_blocks: MOCK_TOTAL_BLOCKS,
            free_blocks: free,
            avail_blocks: free,
            files: 0,
            files_free: 0,
        })
    }
}

#[cfg(test)]
#[path = "memfs_tests.rs"]
mod tests;
