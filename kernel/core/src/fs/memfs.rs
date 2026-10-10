//! A shared in-memory `FilesystemRead` + `FilesystemWrite` +
//! `FilesystemSecurity` driver for the `fs` module's tests.
//!
//! `RwMockFs` is a small allocation-backed filesystem implementing the same
//! `(dir, name)` mutation model the ABI defines, standing in for a
//! block-backed `drivers/filesystem/*` driver (`kernel/core` may not depend
//! on `drivers/*`). It is the one definition shared by the delegation tests
//! (`delegate_tests.rs`) and the mounted-service tests (`mounted_tests.rs`),
//! so neither carries its own copy.
//!
//! By default a created node is owned by [`ADMIN_UID`]/[`ADMIN_GID`] mode
//! `0o755` (the delegation tests vary only the node they care about); a test
//! that needs a different creator uses [`RwMockFs::with_create_owner`].

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use core::ops::Bound;

use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs, FilesystemAttrsProvider,
    FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, NameMatching, NodeId,
    NodeInfo, NodeKind, NodeSecurity, NodeTimes, VolumeStats, WritebackHost,
};
use tairix_abi::driver::{DriverError, DriverHandle};
use tairix_fsmeta::{AttrFlags, AttrSet};

/// Default owner uid baked into a freshly created node's security record.
pub(crate) const ADMIN_UID: u32 = 1;
/// Default owning gid baked into a freshly created node's security record.
pub(crate) const ADMIN_GID: u32 = 1;

/// One node of the in-memory tree.
enum RwNode {
    /// A directory: child name → node index.
    Dir(BTreeMap<String, usize>),
    /// A regular file: its byte contents.
    File(Vec<u8>),
    /// A symbolic link: its stored target, verbatim and unresolved.
    Link(Vec<u8>),
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
    /// Per-node extended-attribute set, parallel to `nodes`/`sec`,
    /// validated through the one `lib/fsmeta` definition.
    attrs: Vec<AttrSet>,
    /// Security applied to a node created through [`FilesystemWrite::create`].
    create_uid: u32,
    create_gid: u32,
    create_mode: u32,
    /// Folding by default, so the cache's and the watch's folding paths stay
    /// exercised; [`Self::with_exact_names`] stands in for an exact format.
    name_matching: NameMatching,
    /// Bumped by every name added or removed, so a listing cursor names a
    /// place in one version of a directory and no other.
    generation: u32,
    /// The numbers of nodes whose last name went, which a create takes again
    /// lowest first, as FAT's clusters and ext4's inodes are; `None` keeps
    /// every number for good.
    free: Option<BTreeSet<usize>>,
    /// Run on entry to every lookup, so a test can check what its caller
    /// holds while inside the driver.
    lookup_probe: Option<Box<dyn Fn() + Send>>,
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
            generation: 0,
            attrs: alloc::vec![AttrSet::new()],
            create_uid: ADMIN_UID,
            create_gid: ADMIN_GID,
            create_mode: 0o755,
            name_matching: NameMatching::AsciiCaseInsensitive,
            free: None,
            lookup_probe: None,
        }
    }

    /// Match names byte-for-byte, as ARXFS and ext4 do.
    #[must_use]
    pub fn with_exact_names(mut self) -> Self {
        self.name_matching = NameMatching::Exact;
        self
    }

    /// Give a removed node's number to the next node made, lowest first.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_reused_numbers(mut self) -> Self {
        self.free = Some(BTreeSet::new());
        self
    }

    /// Run `probe` on entry to every lookup.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_lookup_probe(mut self, probe: Box<dyn Fn() + Send>) -> Self {
        self.lookup_probe = Some(probe);
        self
    }

    /// Place a new node, owned as a create makes it, under a freed number
    /// where one is reused, else a fresh one.
    fn place(&mut self, node: RwNode) -> usize {
        let sec = NodeSecurity::new(self.create_mode, self.create_uid, self.create_gid);
        if let Some(index) = self.free.as_mut().and_then(BTreeSet::pop_first) {
            self.nodes[index] = node;
            self.sec[index] = sec;
            self.attrs[index] = AttrSet::new();
            self.nlink[index] = 1;
            return index;
        }
        self.nodes.push(node);
        self.sec.push(sec);
        self.attrs.push(AttrSet::new());
        self.nlink.push(1);
        self.nodes.len() - 1
    }

    /// Set the owner/mode a node created through the write surface receives,
    /// so a test can create files the resolving principal then owns.
    #[cfg(test)]
    pub(crate) fn with_create_owner(mut self, uid: u32, gid: u32, mode: u32) -> Self {
        self.create_uid = uid;
        self.create_gid = gid;
        self.create_mode = mode;
        self
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

    fn child_index(&self, dir: NodeId, name: &[u8]) -> Result<Option<usize>, DriverError> {
        let idx = Self::index(dir)?;
        let RwNode::Dir(children) = self.nodes.get(idx).ok_or(DriverError::NotFound)? else {
            return Err(DriverError::Unsupported);
        };
        let needle = core::str::from_utf8(name).map_err(|_| DriverError::NotFound)?;
        for (k, &v) in children {
            if self.name_matching.matches(k.as_bytes(), needle.as_bytes()) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// The spelling directory `dir_idx` stores for the entry `name` matches.
    fn stored_name(&self, dir_idx: usize, name: &[u8]) -> Option<&[u8]> {
        let RwNode::Dir(children) = self.nodes.get(dir_idx)? else {
            return None;
        };
        children
            .keys()
            .find(|k| self.name_matching.matches(k.as_bytes(), name))
            .map(String::as_bytes)
    }

    /// Remove the entry named `name` from directory `dir_idx`, returning the
    /// node it named.
    ///
    /// Keyed by name, not by node: one directory may hold two names for one
    /// node, and finding the entry by node index would drop whichever the
    /// map happened to yield first.
    fn unlink_name(&mut self, dir_idx: usize, name: &str) -> Option<usize> {
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
        // A detached name is one fewer name; the node itself lives on for as
        // long as another entry reaches it.
        if let Some(count) = self.nlink.get_mut(child) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                if let Some(free) = self.free.as_mut() {
                    free.insert(child);
                }
            }
        }
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

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        let idx = Self::index(node)?;
        let names = self.nlink.get(idx).copied().ok_or(DriverError::NotFound)?;
        match self.nodes.get(idx).ok_or(DriverError::NotFound)? {
            // The in-RAM tree keeps no per-node stamps; the epoch is the
            // documented "no stamp" value, never a fabricated wall time.
            RwNode::Dir(_) => Ok(NodeInfo {
                kind: NodeKind::Directory,
                nlink: names,
                size: 0,
                allocated: 0,
                times: NodeTimes::default(),
                content_gen: NodeInfo::NO_CONTENT_GEN,
            }),
            RwNode::File(data) => Ok(NodeInfo {
                kind: NodeKind::RegularFile,
                nlink: names,
                size: data.len() as u64,
                // Heap-backed: the bytes held are the storage occupied.
                allocated: data.len() as u64,
                times: NodeTimes::default(),
                content_gen: NodeInfo::NO_CONTENT_GEN,
            }),
            RwNode::Link(target) => Ok(NodeInfo {
                kind: NodeKind::Symlink,
                nlink: names,
                size: target.len() as u64,
                allocated: target.len() as u64,
                times: NodeTimes::default(),
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

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        let idx = Self::index(file)?;
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
        let idx = Self::index(link)?;
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
        let idx = Self::index(dir)?;
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
            let entry = DirEntry {
                node,
                info: self.node_info(node)?,
                next_cursor: (u64::from(self.generation) << 32) | (at as u64 + 1),
            };
            if visit(&entry, name.as_bytes()) == DirVisit::Stop {
                break;
            }
        }
        Ok(())
    }
}

impl FilesystemWrite for RwMockFs {
    fn create(&mut self, dir: NodeId, name: &[u8], kind: NodeKind) -> Result<NodeId, DriverError> {
        if self.child_index(dir, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
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
        let new_index = self.place(node);
        let dir_idx = Self::index(dir)?;
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
    ) -> Result<NodeId, DriverError> {
        if self.child_index(dir, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        if target.is_empty() {
            return Err(DriverError::LengthOutOfRange);
        }
        let name = core::str::from_utf8(name)
            .map_err(|_| DriverError::LengthOutOfRange)?
            .to_string();
        let new_index = self.place(RwNode::Link(target.to_vec()));
        let dir_idx = Self::index(dir)?;
        if let RwNode::Dir(children) = &mut self.nodes[dir_idx] {
            children.insert(name, new_index);
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(NodeId::from_raw(new_index as u64 + 1))
    }

    fn link(&mut self, dir: NodeId, name: &[u8], node: NodeId) -> Result<(), DriverError> {
        if self.child_index(dir, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        let target = Self::index(node)?;
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

    fn write_at(
        &mut self,
        dir: NodeId,
        name: &[u8],
        offset: u64,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        let child = self.child_index(dir, name)?.ok_or(DriverError::NotFound)?;
        let RwNode::File(body) = &mut self.nodes[child] else {
            return Err(DriverError::Unsupported);
        };
        let start = usize::try_from(offset).map_err(|_| DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(data.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        if body.len() < end {
            body.resize(end, 0);
        }
        body[start..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn truncate(&mut self, dir: NodeId, name: &[u8], size: u64) -> Result<(), DriverError> {
        let child = self.child_index(dir, name)?.ok_or(DriverError::NotFound)?;
        let RwNode::File(body) = &mut self.nodes[child] else {
            return Err(DriverError::Unsupported);
        };
        let new = usize::try_from(size).map_err(|_| DriverError::LengthOutOfRange)?;
        body.resize(new, 0);
        Ok(())
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        let child = self.child_index(dir, name)?.ok_or(DriverError::NotFound)?;
        if let RwNode::Dir(children) = &self.nodes[child] {
            if !children.is_empty() {
                return Err(DriverError::DirectoryNotEmpty);
            }
        }
        let key = core::str::from_utf8(name).map_err(|_| DriverError::NotFound)?;
        let dir_idx = Self::index(dir)?;
        self.unlink_name(dir_idx, key);
        Ok(())
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        let src_idx = self
            .child_index(src_dir, src_name)?
            .ok_or(DriverError::NotFound)?;
        let dst_dir_idx = Self::index(dst_dir)?;
        if !matches!(self.nodes.get(dst_dir_idx), Some(RwNode::Dir(_))) {
            return Err(DriverError::Unsupported);
        }
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
                self.unlink_name(dst_dir_idx, &dst_key);
            }
            None => {}
        }

        // Detach the source name and attach the node under the new name; the
        // node keeps the name it had, moved rather than added.
        let src_key = core::str::from_utf8(src_name).map_err(|_| DriverError::NotFound)?;
        let src_dir_idx = Self::index(src_dir)?;
        self.unlink_name(src_dir_idx, src_key);
        if let RwNode::Dir(children) = &mut self.nodes[dst_dir_idx] {
            children.insert(dst_key, src_idx);
            self.generation = self.generation.wrapping_add(1);
        }
        if let Some(count) = self.nlink.get_mut(src_idx) {
            *count = count.saturating_add(1);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    /// Report the mount's state to the host the moment the timer arrives,
    /// exactly as a driver that defers durability does — which is what makes
    /// the forwarding of this method observable through the host alone
    /// ([`super::wrapper_conformance::assert_writeback_host_forwards`]).
    fn set_writeback_host(&mut self, volume: DriverHandle, host: &'static dyn WritebackHost) {
        host.writeback_due(volume, None);
    }
}

impl FilesystemAttrs for RwMockFs {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let idx = Self::index(node)?;
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
        let idx = Self::index(node)?;
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
        let idx = Self::index(node)?;
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
        let idx = Self::index(node)?;
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
        let idx = Self::index(node)?;
        self.sec.get(idx).copied().ok_or(DriverError::NotFound)
    }

    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        let idx = Self::index(node)?;
        match self.sec.get_mut(idx) {
            Some(stored) => {
                *stored = security;
                Ok(())
            }
            None => Err(DriverError::NotFound),
        }
    }
}

/// The mock's fixed allocation unit for its derived space accounting.
const MOCK_BLOCK_SIZE: u32 = 512;

/// The mock's fixed capacity, in [`MOCK_BLOCK_SIZE`] blocks.
const MOCK_TOTAL_BLOCKS: u64 = 4096;

impl FilesystemStats for RwMockFs {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
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
            total_blocks: MOCK_TOTAL_BLOCKS,
            free_blocks: free,
            avail_blocks: free,
            files: 0,
            files_free: 0,
        })
    }
}
