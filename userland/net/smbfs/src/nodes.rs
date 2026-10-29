//! A mounted share's nodes: each a path on the server, named by an id the
//! kernel holds for as long as the node keeps its name.
//!
//! The server keeps no ids a client may name, so the id is this map's own,
//! and the path is what an operation reaches the node by. The server's own
//! file id, where it keeps one, is recorded beside the path, so a node another
//! writer replaced under the same name is found out and answered stale rather
//! than mistaken for the one the kernel holds.
//!
//! A path is found as the share compares names, ASCII letters alike in either
//! case, so two spellings of one name are one node.
//!
//! Only so many nodes are remembered: the least recently used is forgotten
//! first, and a later call naming it is answered stale. A node the kernel has
//! pinned — an open file, a working directory — is never forgotten while it
//! is.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NodeId, Unpinned};
use tairix_collections::{HashMap, LruMap};
use tairix_hash::BuildSipHash13;
use tairix_smb::file::name::SharePath;

/// The most unpinned nodes one mount remembers.
pub const NODES_MAX: usize = 16_384;

/// The node a share's root is named by.
pub const ROOT: NodeId = NodeId::from_raw(1);

/// One node: where it is, and what the server last said it was.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    /// Its path from the share's root, as the server last spelled it.
    pub path: SharePath,
    /// The server's id for it, or zero where none is known.
    pub file_id: u64,
    /// Whether it is a directory.
    pub directory: bool,
}

/// Why a node could not be named.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unnamed {
    /// No room could be had.
    OutOfMemory,
}

/// Whether `path` is `root` or lies beneath it, as the share compares names.
#[must_use]
pub fn within(path: &SharePath, root: &SharePath) -> bool {
    spelled_within(path.as_str(), root.as_str())
}

fn spelled_within(path: &str, root: &str) -> bool {
    let (path, root) = (path.as_bytes(), root.as_bytes());
    if root.is_empty() {
        return true;
    }
    path.len() >= root.len()
        && path[..root.len()].eq_ignore_ascii_case(root)
        && path.get(root.len()).is_none_or(|&next| next == b'\\')
}

/// Whether two paths name one node, as the share compares names.
#[must_use]
pub fn same(a: &SharePath, b: &SharePath) -> bool {
    a.as_str().eq_ignore_ascii_case(b.as_str())
}

/// A held node: the node, its holds, and whether it still has its name.
struct Held {
    node: Node,
    pins: u32,
    named: bool,
}

/// Every node one mount names.
pub struct Nodes {
    unpinned: LruMap<u64, Node, BuildSipHash13>,
    pinned: HashMap<u64, Held, BuildSipHash13>,
    /// Each named node's id, by its path folded to ASCII lower case.
    by_path: HashMap<String, u64, BuildSipHash13>,
    /// The last path folded for a lookup, kept for its room.
    folded: String,
    next: u64,
}

impl Nodes {
    /// A share's nodes, only its root named, indexed under `hasher` — keyed,
    /// since a hostile server chooses the names.
    #[must_use]
    pub fn new(hasher: &BuildSipHash13) -> Self {
        Self {
            unpinned: LruMap::with_hasher(*hasher),
            pinned: HashMap::with_hasher(*hasher),
            by_path: HashMap::with_hasher(*hasher),
            folded: String::new(),
            next: ROOT.raw() + 1,
        }
    }

    /// The node `id` names, if it is remembered: the root always is.
    pub fn get(&mut self, id: NodeId) -> Option<Node> {
        if id == ROOT {
            return Some(Node {
                path: SharePath::root(),
                file_id: 0,
                directory: true,
            });
        }
        if let Some(held) = self.pinned.get(&id.raw()) {
            return held.named.then(|| held.node.clone());
        }
        self.unpinned.get(&id.raw()).cloned()
    }

    /// Fold `path` into the lookup room.
    fn fold(&mut self, path: &SharePath) -> Result<(), Unnamed> {
        self.folded.clear();
        self.folded
            .try_reserve(path.as_str().len())
            .map_err(|_| Unnamed::OutOfMemory)?;
        self.folded.push_str(path.as_str());
        self.folded.make_ascii_lowercase();
        Ok(())
    }

    /// The id `path` is named by, if it is named.
    fn id_at(&mut self, path: &SharePath) -> Option<u64> {
        self.fold(path).ok()?;
        self.by_path.get(self.folded.as_str()).copied()
    }

    /// Whether the kernel holds `id`.
    #[must_use]
    pub fn is_pinned(&self, id: NodeId) -> bool {
        id == ROOT || self.pinned.contains_key(&id.raw())
    }

    /// The id `path` is named by, if it is named.
    pub fn id_of(&mut self, path: &SharePath) -> Option<NodeId> {
        if path.is_root() {
            return Some(ROOT);
        }
        self.id_at(path).map(NodeId::from_raw)
    }

    /// The node `path` names, if it is remembered.
    pub fn at(&mut self, path: &SharePath) -> Option<Node> {
        if path.is_root() {
            return self.get(ROOT);
        }
        let id = self.id_at(path)?;
        self.get(NodeId::from_raw(id))
    }

    fn recorded_mut(&mut self, id: u64) -> Option<&mut Node> {
        match self.pinned.get_mut(&id) {
            Some(held) => Some(&mut held.node),
            None => self.unpinned.peek_mut(&id),
        }
    }

    /// The id `path` is named by, naming it when it is new, and what the
    /// server says it now is.
    ///
    /// # Errors
    ///
    /// [`Unnamed::OutOfMemory`].
    pub fn name(
        &mut self,
        path: &SharePath,
        file_id: u64,
        directory: bool,
    ) -> Result<NodeId, Unnamed> {
        if path.is_root() {
            return Ok(ROOT);
        }
        if let Some(id) = self.id_at(path) {
            // Touched as the most recently used.
            let _ = self.unpinned.get(&id);
            if let Some(node) = self.recorded_mut(id) {
                if node.file_id == file_id || node.file_id == 0 || file_id == 0 {
                    node.file_id = node.file_id.max(file_id);
                    node.directory = directory;
                    return Ok(NodeId::from_raw(id));
                }
            }
            // Another node now has that name: the one the kernel held is
            // stale, and the name gets an id of its own.
            self.forget(id);
        }
        self.fold(path)?;
        let mut key = String::new();
        key.try_reserve_exact(self.folded.len())
            .map_err(|_| Unnamed::OutOfMemory)?;
        key.push_str(&self.folded);
        let node = Node {
            path: path.clone(),
            file_id,
            directory,
        };
        self.by_path
            .try_reserve(1)
            .map_err(|_| Unnamed::OutOfMemory)?;
        while self.unpinned.len() >= NODES_MAX {
            let Some((evicted, gone)) = self.unpinned.pop_lru() else {
                break;
            };
            self.unname(evicted, &gone.path);
        }
        self.unpinned
            .try_reserve(1)
            .map_err(|_| Unnamed::OutOfMemory)?;
        let id = self.next;
        self.unpinned
            .try_insert(id, node)
            .map_err(|_| Unnamed::OutOfMemory)?;
        if self.by_path.try_insert(key, id).is_err() {
            self.unpinned.remove(&id);
            return Err(Unnamed::OutOfMemory);
        }
        self.next += 1;
        Ok(NodeId::from_raw(id))
    }

    /// Let `path`'s name go, if it is still `id`'s.
    fn unname(&mut self, id: u64, path: &SharePath) {
        if self.id_at(path) == Some(id) {
            self.by_path.remove(self.folded.as_str());
        }
    }

    /// Forget the node `id` names: it was removed, or another took its name.
    /// A held one stays held, stale, until its last hold goes.
    pub fn forget(&mut self, id: u64) {
        let path = match self.pinned.get_mut(&id) {
            Some(held) => {
                held.named = false;
                held.node.path.clone()
            }
            None => match self.unpinned.remove(&id) {
                Some(node) => node.path,
                None => return,
            },
        };
        self.unname(id, &path);
    }

    /// Forget the node at `path`, if one is named there.
    pub fn forget_at(&mut self, path: &SharePath) {
        if let Some(id) = self.id_at(path) {
            self.forget(id);
        }
    }

    /// The node at `from` now lies at `to`, and every node beneath it with
    /// it: a rename keeps what the kernel holds. A node `to` named before is
    /// forgotten, as the rename replaced it.
    ///
    /// # Errors
    ///
    /// [`Unnamed::OutOfMemory`], having moved none.
    pub fn moved(&mut self, from: &SharePath, to: &SharePath) -> Result<(), Unnamed> {
        let Some(top) = self.id_at(from) else {
            // Nothing named there is held, so nothing moves.
            self.forget_at(to);
            return Ok(());
        };
        let beneath = self.recorded_mut(top).is_some_and(|node| node.directory);
        let mut moving = Vec::new();
        moving.try_reserve(1).map_err(|_| Unnamed::OutOfMemory)?;
        moving.push(top);
        if beneath {
            for (key, &id) in &self.by_path {
                if id != top && spelled_within(key, from.as_str()) {
                    moving.try_reserve(1).map_err(|_| Unnamed::OutOfMemory)?;
                    moving.push(id);
                }
            }
        }
        let mut renamed = Vec::new();
        renamed
            .try_reserve_exact(moving.len())
            .map_err(|_| Unnamed::OutOfMemory)?;
        for &id in &moving {
            let Some(node) = self.recorded_mut(id) else {
                continue;
            };
            // Folding keeps a path's length, so the rest begins where
            // `from` ends in either spelling.
            let rest = &node.path.as_str()[from.as_str().len()..];
            let mut text = String::new();
            text.try_reserve_exact(to.as_str().len() + rest.len())
                .map_err(|_| Unnamed::OutOfMemory)?;
            text.push_str(to.as_str());
            text.push_str(rest);
            let new = SharePath::parse(&text).map_err(|_| Unnamed::OutOfMemory)?;
            let mut key = text;
            key.make_ascii_lowercase();
            renamed.push((id, new, key));
        }
        self.by_path
            .try_reserve(renamed.len())
            .map_err(|_| Unnamed::OutOfMemory)?;
        if !same(from, to) {
            self.forget_at(to);
        }
        for (id, new, key) in renamed {
            if let Some(node) = self.recorded_mut(id) {
                let old = core::mem::replace(&mut node.path, new);
                self.unname(id, &old);
            }
            if let Some(&displaced) = self.by_path.get(key.as_str()) {
                if displaced != id {
                    self.forget(displaced);
                }
            }
            // Room was reserved for every name moved.
            let _ = self.by_path.try_insert(key, id);
        }
        Ok(())
    }

    /// Hold `id` so it is never forgotten while held.
    ///
    /// # Errors
    ///
    /// [`Unnamed::OutOfMemory`].
    pub fn pin(&mut self, id: NodeId) -> Result<bool, Unnamed> {
        if id == ROOT {
            return Ok(true);
        }
        if let Some(held) = self.pinned.get_mut(&id.raw()) {
            if !held.named {
                return Ok(false);
            }
            held.pins += 1;
            return Ok(true);
        }
        self.pinned
            .try_reserve(1)
            .map_err(|_| Unnamed::OutOfMemory)?;
        let Some(node) = self.unpinned.remove(&id.raw()) else {
            return Ok(false);
        };
        let held = Held {
            node,
            pins: 1,
            named: true,
        };
        // Room was reserved above.
        let _ = self.pinned.try_insert(id.raw(), held);
        Ok(true)
    }

    /// Let one hold on `id` go, answering what that left, or `None` for a
    /// node no pin held.
    pub fn unpin(&mut self, id: NodeId) -> Option<Unpinned> {
        if id == ROOT {
            return Some(Unpinned::Released);
        }
        let held = self.pinned.get_mut(&id.raw())?;
        held.pins -= 1;
        if held.pins > 0 {
            return Some(Unpinned::Held);
        }
        Some(self.unhold(id.raw()))
    }

    /// Let every hold go.
    pub fn release_pins(&mut self) {
        while let Some(&id) = self.pinned.keys().next() {
            self.unhold(id);
        }
    }

    /// Return the held node `id` to the remembered ones, or forget it when
    /// it no longer has a name.
    fn unhold(&mut self, id: u64) -> Unpinned {
        let Some(Held { node, named, .. }) = self.pinned.remove(&id) else {
            return Unpinned::Freed;
        };
        if !named || self.id_at(&node.path) != Some(id) {
            return Unpinned::Freed;
        }
        let path = node.path.clone();
        if self.unpinned.try_insert(id, node).is_err() {
            self.unname(id, &path);
            return Unpinned::Freed;
        }
        Unpinned::Released
    }
}

#[cfg(test)]
#[path = "nodes_tests.rs"]
mod tests;
