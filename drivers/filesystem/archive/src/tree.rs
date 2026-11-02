//! The member tree (`plans/ARCHIVES.md` §3): one node per member and per
//! directory, numbered for the attachment's life, each held as the kernel's
//! pins require, with what the archive records of it and where its data is.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::driver::filesystem::{NameMatching, NodeKind, NodeTimes};
use tairix_abi::DriverError;
use tairix_fsmeta::preset::acorn;

use crate::data::Data;

/// The root directory's number.
pub(crate) const ROOT: u64 = 1;

/// The most members an archive this driver opens holds: a fixed bound on
/// what a stranger's index can make it hold, not a capacity.
pub(crate) const MAX_MEMBERS: usize = 1 << 20;

/// The deepest a directory nests: a fixed bound on what a stranger's paths
/// make the driver walk, held to on every way a directory is made or moved.
pub(crate) const MAX_DEPTH: usize = 256;

/// What a node is.
pub(crate) enum Body {
    /// A directory: its entries, by name folded as the archive compares
    /// names, each to its node.
    Dir(BTreeMap<Vec<u8>, u64>),
    /// A file and where its bytes are.
    File(Data),
    /// A symbolic link and its target, as stored.
    Link(Vec<u8>),
}

/// What an archive records of a member beyond its bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Meta {
    /// Its permission bits.
    pub(crate) mode: u32,
    /// The owner and group it records, where it records any.
    pub(crate) owner: Option<(u32, u32)>,
    /// Its times.
    pub(crate) times: NodeTimes,
    /// Its RISC OS load, exec and attribute words.
    pub(crate) acorn: Option<acorn::Object>,
}

/// Where a member nothing has changed since it was read lies in the archive:
/// what a writer copies rather than encodes again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Kept {
    /// Its whole record — header, data and trailer — in the archive.
    pub(crate) record: Range<u64>,
    /// The format's own notes on it (a zip member's central record).
    pub(crate) notes: Vec<u8>,
}

/// One node.
pub(crate) struct Node {
    /// The directory holding its name; the root its own.
    pub(crate) parent: u64,
    /// Its name there, as served.
    pub(crate) name: Vec<u8>,
    pub(crate) body: Body,
    pub(crate) meta: Meta,
    /// Whether a name reaches it.
    pub(crate) named: bool,
    /// The kernel's holds on it.
    pub(crate) pins: u32,
    /// Its record in the archive while nothing about it has changed.
    pub(crate) kept: Option<Kept>,
    /// What its format records of it and the tree does not hold, which no
    /// change takes away: what a record spelled afresh keeps.
    pub(crate) notes: Vec<u8>,
    /// Its content generation.
    pub(crate) content_gen: u64,
    /// The order it was read in, or made in: the order a writer keeps.
    pub(crate) order: u64,
    /// The names on its path from the root.
    pub(crate) depth: usize,
}

impl Node {
    /// What kind of node it is.
    pub(crate) const fn kind(&self) -> NodeKind {
        match self.body {
            Body::Dir(_) => NodeKind::Directory,
            Body::File(_) => NodeKind::RegularFile,
            Body::Link(_) => NodeKind::Symlink,
        }
    }
}

/// The member tree.
pub(crate) struct Tree {
    nodes: BTreeMap<u64, Node>,
    next: u64,
    next_gen: u64,
    next_order: u64,
    matching: NameMatching,
}

impl Tree {
    /// An empty tree whose names compare as `matching` says, its root
    /// recorded as `meta`.
    pub(crate) fn new(matching: NameMatching, meta: Meta) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT,
            Node {
                parent: ROOT,
                name: Vec::new(),
                body: Body::Dir(BTreeMap::new()),
                meta,
                named: true,
                pins: 0,
                kept: None,
                notes: Vec::new(),
                content_gen: 1,
                order: 0,
                depth: 0,
            },
        );
        Self {
            nodes,
            next: ROOT + 1,
            next_gen: 2,
            next_order: 1,
            matching,
        }
    }

    /// How the tree compares names.
    pub(crate) const fn matching(&self) -> NameMatching {
        self.matching
    }

    /// How many nodes it holds.
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// `name` as the tree indexes it.
    fn key(&self, name: &[u8]) -> Result<Vec<u8>, DriverError> {
        let mut key = Vec::new();
        key.try_reserve_exact(name.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        match self.matching {
            NameMatching::AsciiCaseInsensitive => {
                key.extend(name.iter().map(u8::to_ascii_lowercase));
            }
            _ => key.extend_from_slice(name),
        }
        Ok(key)
    }

    /// The node `id`, or [`DriverError::NotFound`] for none.
    pub(crate) fn node(&self, id: u64) -> Result<&Node, DriverError> {
        self.nodes.get(&id).ok_or(DriverError::NotFound)
    }

    /// The node `id`, to change.
    pub(crate) fn node_mut(&mut self, id: u64) -> Result<&mut Node, DriverError> {
        self.nodes.get_mut(&id).ok_or(DriverError::NotFound)
    }

    /// Every node, in number order.
    pub(crate) fn nodes(&self) -> impl Iterator<Item = (u64, &Node)> {
        self.nodes.iter().map(|(&id, node)| (id, node))
    }

    /// The entries of the directory `dir` a name still reaches.
    pub(crate) fn entries(&self, dir: u64) -> Result<&BTreeMap<Vec<u8>, u64>, DriverError> {
        let node = self.node(dir)?;
        match &node.body {
            Body::Dir(_) if !node.named => Err(DriverError::NotFound),
            Body::Dir(entries) => Ok(entries),
            _ => Err(DriverError::Unsupported),
        }
    }

    fn entries_mut(&mut self, dir: u64) -> Result<&mut BTreeMap<Vec<u8>, u64>, DriverError> {
        self.entries(dir)?;
        match self.nodes.get_mut(&dir).map(|node| &mut node.body) {
            Some(Body::Dir(entries)) => Ok(entries),
            _ => Err(DriverError::Unsupported),
        }
    }

    /// The child `name` of `dir`.
    pub(crate) fn lookup(&self, dir: u64, name: &[u8]) -> Result<u64, DriverError> {
        let key = self.key(name)?;
        self.entries(dir)?
            .get(&key)
            .copied()
            .ok_or(DriverError::NotFound)
    }

    /// A fresh content generation.
    pub(crate) fn fresh_gen(&mut self) -> u64 {
        let gen = self.next_gen;
        self.next_gen += 1;
        gen
    }

    /// Give `dir` a new child `name` that is `body`, recorded as `meta`.
    ///
    /// # Errors
    ///
    /// [`DriverError::AlreadyExists`] for a name taken,
    /// [`DriverError::NoSpace`] past [`MAX_MEMBERS`],
    /// [`DriverError::LengthOutOfRange`] for a directory past [`MAX_DEPTH`],
    /// and the refusals of [`Self::entries`].
    pub(crate) fn add(
        &mut self,
        dir: u64,
        name: &[u8],
        body: Body,
        meta: Meta,
        kept: Option<Kept>,
    ) -> Result<u64, DriverError> {
        if self.nodes.len() >= MAX_MEMBERS {
            return Err(DriverError::NoSpace);
        }
        let key = self.key(name)?;
        let entries = self.entries(dir)?;
        if entries.contains_key(&key) {
            return Err(DriverError::AlreadyExists);
        }
        let depth = self.node(dir)?.depth + 1;
        if matches!(body, Body::Dir(_)) && depth > MAX_DEPTH {
            return Err(DriverError::LengthOutOfRange);
        }
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(name.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        owned.extend_from_slice(name);
        let id = self.next;
        let content_gen = self.fresh_gen();
        let order = self.next_order;
        self.next += 1;
        self.next_order += 1;
        self.nodes.insert(
            id,
            Node {
                parent: dir,
                name: owned,
                body,
                meta,
                named: true,
                pins: 0,
                kept,
                notes: Vec::new(),
                content_gen,
                order,
                depth,
            },
        );
        self.entries_mut(dir)?.insert(key, id);
        Ok(id)
    }

    /// The directory `parts` names under the root, made with `meta` where
    /// the archive names none, as a member path's directories are.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where a part names a file,
    /// [`DriverError::LengthOutOfRange`] past [`MAX_DEPTH`], and the refusals
    /// of [`Self::add`].
    pub(crate) fn directory(&mut self, parts: &[Vec<u8>], meta: &Meta) -> Result<u64, DriverError> {
        if parts.len() > MAX_DEPTH {
            return Err(DriverError::LengthOutOfRange);
        }
        let mut dir = ROOT;
        for part in parts {
            dir = match self.lookup(dir, part) {
                Ok(found) => match self.node(found)?.body {
                    Body::Dir(_) => found,
                    _ => return Err(DriverError::Unsupported),
                },
                Err(DriverError::NotFound) => {
                    self.add(dir, part, Body::Dir(BTreeMap::new()), meta.clone(), None)?
                }
                Err(err) => return Err(err),
            };
        }
        Ok(dir)
    }

    /// Take `name` out of `dir`, answering the node it named, which is kept
    /// while a hold keeps it and freed otherwise.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no such entry;
    /// [`DriverError::DirectoryNotEmpty`] for a directory holding entries.
    pub(crate) fn unlink(&mut self, dir: u64, name: &[u8]) -> Result<u64, DriverError> {
        let key = self.key(name)?;
        let id = self
            .entries(dir)?
            .get(&key)
            .copied()
            .ok_or(DriverError::NotFound)?;
        if matches!(&self.node(id)?.body, Body::Dir(entries) if !entries.is_empty()) {
            return Err(DriverError::DirectoryNotEmpty);
        }
        self.entries_mut(dir)?.remove(&key);
        self.unname(id);
        Ok(id)
    }

    /// Take away `id`'s name, freeing it unless a hold keeps it.
    fn unname(&mut self, id: u64) {
        let held = self.nodes.get(&id).is_some_and(|node| node.pins > 0);
        if held {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.named = false;
                node.kept = None;
            }
        } else {
            self.nodes.remove(&id);
        }
    }

    /// Move `src_name` of `src_dir` to `dst_name` of `dst_dir`, replacing
    /// what that names, answering the node a replacement took the name
    /// from.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no such source,
    /// [`DriverError::DirectoryCycle`] for a directory moved beneath itself,
    /// [`DriverError::LengthOutOfRange`] for one whose directories it would
    /// carry past [`MAX_DEPTH`], [`DriverError::DirectoryNotEmpty`] for a
    /// directory replacing one with entries, [`DriverError::Unsupported`] for
    /// a replacement of another kind.
    pub(crate) fn rename(
        &mut self,
        src_dir: u64,
        src_name: &[u8],
        dst_dir: u64,
        dst_name: &[u8],
    ) -> Result<Option<u64>, DriverError> {
        let src_key = self.key(src_name)?;
        let dst_key = self.key(dst_name)?;
        let id = self
            .entries(src_dir)?
            .get(&src_key)
            .copied()
            .ok_or(DriverError::NotFound)?;
        let replaced = self.entries(dst_dir)?.get(&dst_key).copied();
        let moving_dir = matches!(self.node(id)?.body, Body::Dir(_));
        if moving_dir && self.is_within(dst_dir, id)? {
            return Err(DriverError::DirectoryCycle);
        }
        let (from, to) = (self.node(id)?.depth, self.node(dst_dir)?.depth + 1);
        let beneath = self.beneath(id)?;
        let deepest = beneath
            .iter()
            .filter_map(|&node| self.nodes.get(&node))
            .filter(|node| matches!(node.body, Body::Dir(_)))
            .map(|node| node.depth)
            .max();
        if deepest.is_some_and(|deepest| (deepest + to).saturating_sub(from) > MAX_DEPTH) {
            return Err(DriverError::LengthOutOfRange);
        }
        if let Some(other) = replaced.filter(|&other| other != id) {
            match (&self.node(other)?.body, moving_dir) {
                (Body::Dir(entries), true) if !entries.is_empty() => {
                    return Err(DriverError::DirectoryNotEmpty)
                }
                (Body::Dir(_), true) => {}
                (Body::Dir(_), false) | (_, true) => return Err(DriverError::Unsupported),
                (_, false) => {}
            }
        }
        let mut name = Vec::new();
        name.try_reserve_exact(dst_name.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        name.extend_from_slice(dst_name);
        self.entries_mut(src_dir)?.remove(&src_key);
        let previous = self.entries_mut(dst_dir)?.insert(dst_key, id);
        self.node_mut(id)?.parent = dst_dir;
        self.node_mut(id)?.name = name;
        // Its path is part of every record beneath it.
        for node in beneath {
            if let Some(node) = self.nodes.get_mut(&node) {
                node.depth = (node.depth + to).saturating_sub(from);
                node.kept = None;
            }
        }
        Ok(match previous {
            Some(other) if other != id => {
                self.unname(other);
                Some(other)
            }
            _ => None,
        })
    }

    /// Whether `dir` is `ancestor` or lies beneath it.
    fn is_within(&self, mut dir: u64, ancestor: u64) -> Result<bool, DriverError> {
        for _ in 0..=MAX_DEPTH {
            if dir == ancestor {
                return Ok(true);
            }
            if dir == ROOT {
                return Ok(false);
            }
            dir = self.node(dir)?.parent;
        }
        Err(DriverError::DeviceFault)
    }

    /// `id` and every node beneath it.
    fn beneath(&self, id: u64) -> Result<Vec<u64>, DriverError> {
        let mut found = Vec::new();
        found.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
        found.push(id);
        // Every node is reached once, so the walk is bounded by the tree.
        let mut next = 0;
        while let Some(&at) = found.get(next) {
            next += 1;
            if let Body::Dir(entries) = &self.node(at)?.body {
                found
                    .try_reserve(entries.len())
                    .map_err(|_| DriverError::OutOfMemory)?;
                found.extend(entries.values().copied());
            }
        }
        Ok(found)
    }

    /// Add one hold of `id`.
    pub(crate) fn pin(&mut self, id: u64) -> Result<(), DriverError> {
        let node = self.node_mut(id)?;
        node.pins = node.pins.checked_add(1).ok_or(DriverError::OutOfMemory)?;
        Ok(())
    }

    /// Release one hold of `id`, answering whether it is now freed, or still
    /// held.
    pub(crate) fn unpin(&mut self, id: u64) -> Result<Unheld, DriverError> {
        let node = self.node_mut(id)?;
        if node.pins == 0 {
            return Err(DriverError::NotFound);
        }
        node.pins -= 1;
        if node.pins > 0 {
            return Ok(Unheld::Held);
        }
        if node.named {
            return Ok(Unheld::Released);
        }
        self.nodes.remove(&id);
        Ok(Unheld::Freed)
    }

    /// Release every hold, freeing what no name reaches.
    pub(crate) fn release_pins(&mut self) {
        self.nodes.retain(|_, node| {
            node.pins = 0;
            node.named
        });
    }

    /// The path of `id` from the root, as its names: what a writer spells a
    /// member's record from.
    pub(crate) fn path(&self, id: u64) -> Result<Vec<&[u8]>, DriverError> {
        let mut parts = Vec::new();
        let mut at = id;
        while at != ROOT {
            if parts.len() > MAX_DEPTH {
                return Err(DriverError::DeviceFault);
            }
            let node = self.node(at)?;
            parts.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
            parts.push(node.name.as_slice());
            at = node.parent;
        }
        parts.reverse();
        Ok(parts)
    }

    /// The named nodes beneath the root in the order a writer keeps: each
    /// directory before what it holds, members in the order they were read
    /// or made.
    pub(crate) fn in_order(&self) -> Result<Vec<u64>, DriverError> {
        let mut order = Vec::new();
        order
            .try_reserve_exact(self.nodes.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        order.extend(
            self.nodes
                .iter()
                .filter(|&(&id, node)| id != ROOT && node.named)
                .map(|(&id, node)| (node.order, id)),
        );
        order.sort_unstable();
        let mut placed = Vec::new();
        placed
            .try_reserve_exact(order.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        self.place_parents_first(order.iter().map(|&(_, id)| id), &mut placed)?;
        Ok(placed)
    }

    /// `order` with each node placed after the directories holding it.
    fn place_parents_first(
        &self,
        order: impl Iterator<Item = u64>,
        placed: &mut Vec<u64>,
    ) -> Result<(), DriverError> {
        let mut seen = alloc::collections::BTreeSet::new();
        seen.insert(ROOT);
        for id in order {
            let mut chain = Vec::new();
            let mut at = id;
            while !seen.contains(&at) {
                if chain.len() > MAX_DEPTH {
                    return Err(DriverError::DeviceFault);
                }
                chain.push(at);
                at = self.node(at)?.parent;
            }
            for &node in chain.iter().rev() {
                seen.insert(node);
                placed.push(node);
            }
        }
        Ok(())
    }
}

/// What releasing a hold left.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Unheld {
    /// Other holds remain.
    Held,
    /// The last hold; a name still reaches it.
    Released,
    /// The last hold of a node no name reached, now freed.
    Freed,
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
