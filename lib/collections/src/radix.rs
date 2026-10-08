//! [`RadixTree`]: a sparse `u64`-keyed index of 64-way nodes, six key bits
//! a level, as tall as its largest key needs.
//!
//! Every point operation descends one path from the root, so its cost is the
//! tree's height — eleven nodes at most — whatever the number of keys. A walk
//! resumes from the path it last took, so a whole walk reaches each node it
//! passes a bounded number of times. Each node keeps a bitmap of its occupied
//! slots and, per tag, one of the slots holding a tagged entry or a subtree
//! holding one, so a successor or predecessor search, and a walk of only the
//! tagged entries, skip empty and untagged subtrees a word at a time.
//!
//! Nodes live in two arenas, interior and leaf, a freed node chained into its
//! arena's free list, so a removal never allocates and an insertion takes a
//! freed node before the arena grows; [`RadixTree::try_shrink_to_fit`] gives
//! the freed ones back. [`RadixTree::try_reserve_key`] makes the one
//! insertion that follows it unable to fail, so a holder changing several
//! trees at once can make every allocation before changing any.

use alloc::vec::Vec;

use crate::TryReserveError;

/// Key bits a level resolves.
const BITS: u32 = 6;
const FANOUT: usize = 1 << BITS;
const SLOT: u64 = FANOUT as u64 - 1;
/// The height that holds every `u64` key.
const MAX_HEIGHT: usize = u64::BITS.div_ceil(BITS) as usize;
const NIL: u32 = u32::MAX;
/// A freed node's link past the last of its chain: a live node's link is
/// [`NIL`], so a freed one is told from it.
const FREE_END: u32 = NIL - 1;

struct Inner<const TAGS: usize> {
    children: [u32; FANOUT],
    /// Bit `i`: slot `i` holds a subtree.
    present: u64,
    /// Bit `i` of word `t`: slot `i`'s subtree holds an entry tagged `t`.
    tags: [u64; TAGS],
    /// The freed node after this one, while this one is freed.
    next_free: u32,
}

struct Leaf<V, const TAGS: usize> {
    values: [Option<V>; FANOUT],
    present: u64,
    tags: [u64; TAGS],
    next_free: u32,
}

/// A node an arena chains through its link while it is freed.
trait Linked {
    fn link(&self) -> u32;
    fn set_link(&mut self, link: u32);
}

impl<const TAGS: usize> Linked for Inner<TAGS> {
    fn link(&self) -> u32 {
        self.next_free
    }

    fn set_link(&mut self, link: u32) {
        self.next_free = link;
    }
}

impl<V, const TAGS: usize> Linked for Leaf<V, TAGS> {
    fn link(&self) -> u32 {
        self.next_free
    }

    fn set_link(&mut self, link: u32) {
        self.next_free = link;
    }
}

impl<const TAGS: usize> Inner<TAGS> {
    const fn empty() -> Self {
        Self {
            children: [NIL; FANOUT],
            present: 0,
            tags: [0; TAGS],
            next_free: NIL,
        }
    }
}

impl<V, const TAGS: usize> Leaf<V, TAGS> {
    fn empty() -> Self {
        Self {
            values: core::array::from_fn(|_| None),
            present: 0,
            tags: [0; TAGS],
            next_free: NIL,
        }
    }
}

/// A node arena and the chain of its freed slots.
struct Arena<N> {
    nodes: Vec<N>,
    free: u32,
    free_count: usize,
}

impl<N: Linked> Arena<N> {
    const fn new() -> Self {
        Self {
            nodes: Vec::new(),
            free: NIL,
            free_count: 0,
        }
    }

    /// Make room for `count` more nodes, freed ones counting, every one
    /// indexable by a `u32` below the links' sentinels.
    fn reserve(&mut self, count: usize) -> Result<(), TryReserveError> {
        let short = count.saturating_sub(self.free_count);
        if self.nodes.len().saturating_add(short) > FREE_END as usize {
            return Err(TryReserveError::CapacityOverflow);
        }
        if short > self.nodes.capacity() - self.nodes.len() {
            self.nodes
                .try_reserve(short)
                .map_err(|_| TryReserveError::AllocFailed)?;
        }
        Ok(())
    }

    fn live(&self) -> usize {
        self.nodes.len() - self.free_count
    }

    /// A node, freed first, else `empty` pushed into room reserved for it.
    // A freed node is chained already empty, so taking one back resets
    // nothing but its link.
    fn take(&mut self, empty: impl FnOnce() -> N) -> u32 {
        if self.free != NIL {
            let index = self.free;
            let node = &mut self.nodes[index as usize];
            self.free = match node.link() {
                FREE_END => NIL,
                next => next,
            };
            node.set_link(NIL);
            self.free_count -= 1;
            return index;
        }
        self.nodes.push(empty());
        u32::try_from(self.nodes.len() - 1).unwrap_or(NIL)
    }

    /// Chain the empty node at `index` into the free list.
    fn free(&mut self, index: u32) {
        let link = if self.free == NIL {
            FREE_END
        } else {
            self.free
        };
        self.nodes[index as usize].set_link(link);
        self.free = index;
        self.free_count += 1;
    }

    /// Move the live node at `index`, where it lies at or past `live`, into
    /// the first freed slot below it from `hole` on, answering where it now
    /// is.
    fn relocate(&mut self, index: u32, live: usize, hole: &mut usize) -> u32 {
        if (index as usize) < live {
            return index;
        }
        while self.nodes[*hole].link() == NIL {
            *hole += 1;
        }
        self.nodes.swap(*hole, index as usize);
        let moved = u32::try_from(*hole).unwrap_or(NIL);
        *hole += 1;
        moved
    }

    /// Drop the `live`-onward nodes [`Self::relocate`] left freed, and give
    /// back the room they and any spare capacity took.
    fn fit(&mut self, live: usize) -> Result<(), TryReserveError> {
        self.nodes.truncate(live);
        self.free = NIL;
        self.free_count = 0;
        if self.nodes.capacity() > live {
            let mut fitted = Vec::new();
            fitted
                .try_reserve_exact(live)
                .map_err(|_| TryReserveError::AllocFailed)?;
            fitted.append(&mut self.nodes);
            self.nodes = fitted;
        }
        Ok(())
    }

    fn bytes(&self) -> usize {
        self.nodes.capacity() * core::mem::size_of::<N>()
    }
}

/// Which bitmap a walk follows: the occupied slots, or one tag's.
#[derive(Copy, Clone)]
enum Follow {
    Present,
    Tag(usize),
}

/// A sparse map from `u64` keys to `V`, with `TAGS` independent tag bits per
/// entry.
///
/// Lookup, insertion, removal, tagging and each step of a walk cost the
/// tree's height; nothing scans. A removal frees the nodes it empties and
/// lowers the tree when its largest key no longer needs the height, so the
/// nodes resident are those the live keys' paths hold.
pub struct RadixTree<V, const TAGS: usize = 0> {
    inner: Arena<Inner<TAGS>>,
    leaves: Arena<Leaf<V, TAGS>>,
    root: u32,
    /// Levels from the root to the leaves; zero for an empty tree.
    height: usize,
    len: usize,
}

impl<V, const TAGS: usize> Default for RadixTree<V, TAGS> {
    fn default() -> Self {
        Self::new()
    }
}

/// The slot `key` takes at `level`, leaves being level zero.
const fn slot(key: u64, level: usize) -> usize {
    ((key >> (BITS as usize * level)) & SLOT) as usize
}

/// The height a tree needs to hold `key`.
const fn height_for(key: u64) -> usize {
    let bits = u64::BITS - key.leading_zeros();
    if bits == 0 {
        1
    } else {
        bits.div_ceil(BITS) as usize
    }
}

/// The key bits at and below `level`: one subtree at `level + 1`.
const fn below(level: usize) -> u64 {
    let shift = BITS as usize * (level + 1);
    if shift >= u64::BITS as usize {
        u64::MAX
    } else {
        (1 << shift) - 1
    }
}

impl<V, const TAGS: usize> RadixTree<V, TAGS> {
    /// An empty tree, which allocates nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: Arena::new(),
            leaves: Arena::new(),
            root: NIL,
            height: 0,
            len: 0,
        }
    }

    /// Entries held.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it holds no entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Levels every lookup descends; zero while empty.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Interior and leaf nodes the live keys' paths hold.
    #[must_use]
    pub fn resident_nodes(&self) -> (usize, usize) {
        (self.inner.live(), self.leaves.live())
    }

    fn holds(&self, key: u64) -> bool {
        self.height >= MAX_HEIGHT || key >> (BITS as usize * self.height) == 0
    }

    /// The leaf holding `key`'s slot, where its path exists.
    fn leaf_of(&self, key: u64) -> Option<u32> {
        if self.height == 0 || !self.holds(key) {
            return None;
        }
        let mut node = self.root;
        for level in (1..self.height).rev() {
            node = self.inner.nodes[node as usize].children[slot(key, level)];
            if node == NIL {
                return None;
            }
        }
        Some(node)
    }

    /// The value at `key`.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<&V> {
        let leaf = self.leaf_of(key)?;
        self.leaves.nodes[leaf as usize].values[slot(key, 0)].as_ref()
    }

    /// The value at `key`, to change in place.
    pub fn get_mut(&mut self, key: u64) -> Option<&mut V> {
        let leaf = self.leaf_of(key)?;
        self.leaves.nodes[leaf as usize].values[slot(key, 0)].as_mut()
    }

    /// Whether `key` holds a value.
    #[must_use]
    pub fn contains_key(&self, key: u64) -> bool {
        self.get(key).is_some()
    }

    /// The interior and leaf nodes inserting `key` would add.
    fn missing(&self, key: u64) -> (usize, usize) {
        let wanted = height_for(key).max(self.height);
        if self.height == 0 {
            return (wanted - 1, 1);
        }
        // Each level the tree grows by is a new root, whose slot zero alone
        // leads on, to the old root.
        let grown = wanted - self.height;
        if grown > 0 && key >> (BITS as usize * self.height) != 0 {
            // A key needing the new height has a non-zero top slot: a new
            // subtree under the top root, every level below it new.
            return (grown + wanted - 2, 1);
        }
        match self.deepest(key) {
            (_, 0) => (grown, 0),
            (_, level) => (grown + level - 1, 1),
        }
    }

    /// The deepest node on `key`'s path and its level, in a non-empty tree
    /// tall enough to hold `key`.
    fn deepest(&self, key: u64) -> (u32, usize) {
        let mut node = self.root;
        for level in (1..self.height).rev() {
            let child = self.inner.nodes[node as usize].children[slot(key, level)];
            if child == NIL {
                return (node, level);
            }
            node = child;
        }
        (node, 0)
    }

    /// Make the next insertion of `key` unable to fail: every node it needs
    /// is allocated now, and nothing else is changed.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the nodes cannot be had; the tree is as it
    /// was.
    pub fn try_reserve_key(&mut self, key: u64) -> Result<(), TryReserveError> {
        let (inner, leaves) = self.missing(key);
        self.inner.reserve(inner)?;
        self.leaves.reserve(leaves)
    }

    /// Put `value` at `key`, answering the value it replaces, whose tags the
    /// key keeps.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when a node the key needs cannot be had; the tree
    /// is as it was.
    pub fn try_insert(&mut self, key: u64, value: V) -> Result<Option<V>, TryReserveError> {
        if self.height == 0 || !self.holds(key) {
            self.try_reserve_key(key)?;
            return Ok(self.insert_reserved(key, value));
        }
        let (node, level) = self.deepest(key);
        if level > 0 {
            self.inner.reserve(level - 1)?;
            self.leaves.reserve(1)?;
        }
        Ok(self.insert_below(node, level, key, value))
    }

    fn take_inner(&mut self) -> u32 {
        self.inner.take(Inner::empty)
    }

    fn take_leaf(&mut self) -> u32 {
        self.leaves.take(Leaf::empty)
    }

    /// Chain an interior node that leads nowhere into the free list.
    fn free_inner(&mut self, index: u32) {
        let node = &self.inner.nodes[index as usize];
        debug_assert!(node.present == 0 && node.tags.iter().all(|&word| word == 0));
        self.inner.free(index);
    }

    /// Chain a leaf that holds nothing into the free list.
    fn free_leaf(&mut self, index: u32) {
        let node = &self.leaves.nodes[index as usize];
        debug_assert!(node.present == 0 && node.tags.iter().all(|&word| word == 0));
        self.leaves.free(index);
    }

    /// Drop every entry and give back every node.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Bytes the tree's arenas hold, freed nodes and spare room included.
    #[must_use]
    pub fn allocated_bytes(&self) -> usize {
        self.inner.bytes() + self.leaves.bytes()
    }

    /// Give back every node a removal freed and any spare room, moving the
    /// live nodes to the front of their arenas: what the tree then holds is
    /// what its live keys' paths need.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the fitted arenas cannot be had; the tree is
    /// then compacted, every entry where it was, but holds the room it had.
    pub fn try_shrink_to_fit(&mut self) -> Result<(), TryReserveError> {
        if self.height == 0 {
            self.clear();
            return Ok(());
        }
        let live = (self.inner.live(), self.leaves.live());
        let mut holes = (0, 0);
        self.root = if self.height == 1 {
            self.leaves.relocate(self.root, live.1, &mut holes.1)
        } else {
            self.inner.relocate(self.root, live.0, &mut holes.0)
        };
        // Each interior node on the walk's path, root first, with the slots
        // still to visit.
        let mut stack = [(NIL, 0u64); MAX_HEIGHT];
        let mut depth = 0;
        if self.height > 1 {
            stack[0] = (self.root, self.inner.nodes[self.root as usize].present);
            depth = 1;
        }
        while depth > 0 {
            let (node, remaining) = stack[depth - 1];
            if remaining == 0 {
                depth -= 1;
                continue;
            }
            stack[depth - 1].1 = remaining & (remaining - 1);
            let index = remaining.trailing_zeros() as usize;
            let level = self.height - depth;
            let child = self.inner.nodes[node as usize].children[index];
            let moved = if level == 1 {
                self.leaves.relocate(child, live.1, &mut holes.1)
            } else {
                self.inner.relocate(child, live.0, &mut holes.0)
            };
            self.inner.nodes[node as usize].children[index] = moved;
            if level > 1 {
                stack[depth] = (moved, self.inner.nodes[moved as usize].present);
                depth += 1;
            }
        }
        self.inner.fit(live.0)?;
        self.leaves.fit(live.1)
    }

    /// Insert where [`Self::try_reserve_key`] made room for `key`.
    fn insert_reserved(&mut self, key: u64, value: V) -> Option<V> {
        let wanted = height_for(key).max(self.height);
        if self.height == 0 {
            self.root = if wanted == 1 {
                self.take_leaf()
            } else {
                self.take_inner()
            };
            self.height = wanted;
        }
        while self.height < wanted {
            let old = self.root;
            let tags = self.tags_of(old, self.height - 1);
            let root = self.take_inner();
            let node = &mut self.inner.nodes[root as usize];
            node.children[0] = old;
            node.present = 1;
            for (word, tagged) in node.tags.iter_mut().zip(tags) {
                *word = u64::from(tagged);
            }
            self.root = root;
            self.height += 1;
        }
        let (node, level) = self.deepest(key);
        self.insert_below(node, level, key, value)
    }

    /// Insert `key` below `node`, the deepest node of its path, at `level`.
    fn insert_below(&mut self, mut node: u32, level: usize, key: u64, value: V) -> Option<V> {
        for level in (1..=level).rev() {
            let fresh = if level == 1 {
                self.take_leaf()
            } else {
                self.take_inner()
            };
            let index = slot(key, level);
            let parent = &mut self.inner.nodes[node as usize];
            parent.children[index] = fresh;
            parent.present |= 1 << index;
            node = fresh;
        }
        let index = slot(key, 0);
        let leaf = &mut self.leaves.nodes[node as usize];
        let old = leaf.values[index].replace(value);
        if old.is_none() {
            leaf.present |= 1 << index;
            self.len += 1;
        }
        old
    }

    /// Whether the node at `level` holds an entry of each tag.
    fn tags_of(&self, node: u32, level: usize) -> [bool; TAGS] {
        let words = if level == 0 {
            self.leaves.nodes[node as usize].tags
        } else {
            self.inner.nodes[node as usize].tags
        };
        words.map(|word| word != 0)
    }

    /// The nodes from the root to `key`'s leaf, root first.
    fn path(&self, key: u64) -> Option<[u32; MAX_HEIGHT]> {
        if self.height == 0 || !self.holds(key) {
            return None;
        }
        let mut path = [NIL; MAX_HEIGHT];
        let mut node = self.root;
        for level in (1..self.height).rev() {
            path[level] = node;
            node = self.inner.nodes[node as usize].children[slot(key, level)];
            if node == NIL {
                return None;
            }
        }
        path[0] = node;
        Some(path)
    }

    /// Take the value at `key` out, with its tags, freeing every node it
    /// leaves empty and lowering the tree to what its largest key needs.
    pub fn remove(&mut self, key: u64) -> Option<V> {
        let path = self.path(key)?;
        let index = slot(key, 0);
        let leaf = &mut self.leaves.nodes[path[0] as usize];
        let value = leaf.values[index].take()?;
        leaf.present &= !(1 << index);
        for word in &mut leaf.tags {
            *word &= !(1 << index);
        }
        self.len -= 1;
        let mut emptied = leaf.present == 0;
        let mut tags = leaf.tags.map(|word| word != 0);
        if emptied {
            self.free_leaf(path[0]);
        }
        for (level, &at) in path.iter().enumerate().take(self.height).skip(1) {
            let index = slot(key, level);
            let node = &mut self.inner.nodes[at as usize];
            if emptied {
                node.children[index] = NIL;
                node.present &= !(1 << index);
            }
            for (word, tagged) in node.tags.iter_mut().zip(tags) {
                if !tagged {
                    *word &= !(1 << index);
                }
            }
            emptied = node.present == 0;
            tags = node.tags.map(|word| word != 0);
            if emptied {
                self.free_inner(at);
            }
        }
        if emptied {
            self.root = NIL;
            self.height = 0;
        }
        self.shrink();
        Some(value)
    }

    /// Lower the tree while its root leads only to slot zero.
    fn shrink(&mut self) {
        while self.height > 1 && self.inner.nodes[self.root as usize].present == 1 {
            let old = self.root;
            let node = &mut self.inner.nodes[old as usize];
            self.root = core::mem::replace(&mut node.children[0], NIL);
            node.present = 0;
            node.tags = [0; TAGS];
            self.free_inner(old);
            self.height -= 1;
        }
    }

    /// Tag the entry at `key` with `tag`, answering whether there is one to
    /// tag. A tag past `TAGS` tags nothing.
    pub fn set_tag(&mut self, key: u64, tag: usize) -> bool {
        if tag >= TAGS || !self.contains_key(key) {
            return false;
        }
        let Some(path) = self.path(key) else {
            return false;
        };
        self.leaves.nodes[path[0] as usize].tags[tag] |= 1 << slot(key, 0);
        for (level, &at) in path.iter().enumerate().take(self.height).skip(1) {
            self.inner.nodes[at as usize].tags[tag] |= 1 << slot(key, level);
        }
        true
    }

    /// Clear `tag` from the entry at `key`, answering whether it had it.
    pub fn clear_tag(&mut self, key: u64, tag: usize) -> bool {
        if !self.is_tagged(key, tag) {
            return false;
        }
        let Some(path) = self.path(key) else {
            return false;
        };
        let leaf = &mut self.leaves.nodes[path[0] as usize];
        leaf.tags[tag] &= !(1 << slot(key, 0));
        let mut cleared = leaf.tags[tag] == 0;
        for (level, &at) in path.iter().enumerate().take(self.height).skip(1) {
            if !cleared {
                break;
            }
            let node = &mut self.inner.nodes[at as usize];
            node.tags[tag] &= !(1 << slot(key, level));
            cleared = node.tags[tag] == 0;
        }
        true
    }

    /// Whether the entry at `key` carries `tag`.
    #[must_use]
    pub fn is_tagged(&self, key: u64, tag: usize) -> bool {
        tag < TAGS
            && self.leaf_of(key).is_some_and(|leaf| {
                self.leaves.nodes[leaf as usize].tags[tag] & 1 << slot(key, 0) != 0
            })
    }

    /// Whether any entry carries `tag`.
    #[must_use]
    pub fn any_tagged(&self, tag: usize) -> bool {
        tag < TAGS && self.height != 0 && self.tags_of(self.root, self.height - 1)[tag]
    }

    fn bits(&self, node: u32, level: usize, follow: Follow) -> u64 {
        match (follow, level) {
            (Follow::Present, 0) => self.leaves.nodes[node as usize].present,
            (Follow::Present, _) => self.inner.nodes[node as usize].present,
            (Follow::Tag(tag), 0) => self.leaves.nodes[node as usize].tags[tag],
            (Follow::Tag(tag), _) => self.inner.nodes[node as usize].tags[tag],
        }
    }

    /// The least key at or above `from` whose slot `follow`'s bitmaps mark,
    /// and its leaf, `path` left holding the nodes above it.
    fn seek_up(
        &self,
        from: u64,
        follow: Follow,
        path: &mut [u32; MAX_HEIGHT],
    ) -> Option<(u64, u32)> {
        if self.height == 0 || !self.holds(from) {
            return None;
        }
        self.seek_up_at(from, self.height - 1, self.root, follow, path)
    }

    /// The least marked key after `key`, which a walk last found in `leaf`,
    /// `path` holding the nodes above it: resumed from the deepest node `key`
    /// and its successor share.
    fn seek_after(
        &self,
        key: u64,
        leaf: u32,
        follow: Follow,
        path: &mut [u32; MAX_HEIGHT],
    ) -> Option<(u64, u32)> {
        let next = key.checked_add(1)?;
        let level = (0..self.height).find(|&level| slot(key, level) != FANOUT - 1)?;
        let node = if level == 0 { leaf } else { path[level] };
        self.seek_up_at(next, level, node, follow, path)
    }

    /// [`Self::seek_up`] from `node`, at `level` on `key`'s path, `path`
    /// holding the nodes above it.
    fn seek_up_at(
        &self,
        mut key: u64,
        mut level: usize,
        mut node: u32,
        follow: Follow,
        path: &mut [u32; MAX_HEIGHT],
    ) -> Option<(u64, u32)> {
        loop {
            let at = slot(key, level);
            let bits = self.bits(node, level, follow) & (u64::MAX << at);
            if bits == 0 {
                // Up to the nearest ancestor with a slot after this path's,
                // then on from that slot's first key.
                loop {
                    level += 1;
                    if level >= self.height {
                        return None;
                    }
                    if slot(key, level) != FANOUT - 1 {
                        break;
                    }
                }
                key = (key | below(level - 1)).checked_add(1)?;
                node = path[level];
                continue;
            }
            let found = bits.trailing_zeros() as usize;
            if found != at {
                let shift = BITS as usize * level;
                key = (key & !below(level)) | ((found as u64) << shift);
            }
            if level == 0 {
                return Some((key, node));
            }
            path[level] = node;
            node = self.inner.nodes[node as usize].children[found];
            level -= 1;
        }
    }

    /// The greatest key at or below `at` whose slot `follow`'s bitmaps mark,
    /// and its leaf.
    fn seek_down(&self, at: u64, follow: Follow) -> Option<(u64, u32)> {
        if self.height == 0 {
            return None;
        }
        let ceiling = below(self.height - 1);
        let mut path = [NIL; MAX_HEIGHT];
        let mut level = self.height - 1;
        let mut node = self.root;
        let mut key = at.min(ceiling);
        loop {
            let here = slot(key, level);
            let mask = if here == FANOUT - 1 {
                u64::MAX
            } else {
                (1 << (here + 1)) - 1
            };
            let bits = self.bits(node, level, follow) & mask;
            if bits == 0 {
                // Up to the nearest ancestor with a slot before this path's,
                // then on from that slot's last key.
                loop {
                    level += 1;
                    if level >= self.height {
                        return None;
                    }
                    if slot(key, level) != 0 {
                        break;
                    }
                }
                key = (key & !below(level - 1)).checked_sub(1)?;
                node = path[level];
                continue;
            }
            let found = (u64::BITS - 1 - bits.leading_zeros()) as usize;
            if found != here {
                let shift = BITS as usize * level;
                let last = if level == 0 { 0 } else { below(level - 1) };
                key = (key & !below(level)) | ((found as u64) << shift) | last;
            }
            if level == 0 {
                return Some((key, node));
            }
            path[level] = node;
            node = self.inner.nodes[node as usize].children[found];
            level -= 1;
        }
    }

    fn entry(&self, (key, leaf): (u64, u32)) -> Option<(u64, &V)> {
        let value = self.leaves.nodes[leaf as usize].values[slot(key, 0)].as_ref()?;
        Some((key, value))
    }

    /// The entry of the least key at or above `from`.
    #[must_use]
    pub fn next(&self, from: u64) -> Option<(u64, &V)> {
        self.entry(self.seek_up(from, Follow::Present, &mut [NIL; MAX_HEIGHT])?)
    }

    /// The entry of the greatest key at or below `at`.
    #[must_use]
    pub fn prev(&self, at: u64) -> Option<(u64, &V)> {
        self.entry(self.seek_down(at, Follow::Present)?)
    }

    /// The entry of the least key at or above `from` that carries `tag`.
    #[must_use]
    pub fn next_tagged(&self, from: u64, tag: usize) -> Option<(u64, &V)> {
        if tag >= TAGS {
            return None;
        }
        self.entry(self.seek_up(from, Follow::Tag(tag), &mut [NIL; MAX_HEIGHT])?)
    }

    /// Every entry from `from` up, in key order: a gang lookup is the first
    /// few of them.
    #[must_use]
    pub fn iter_from(&self, from: u64) -> Iter<'_, V, TAGS> {
        Iter::new(self, Some(from), Follow::Present)
    }

    /// Every entry from `from` up that carries `tag`, in key order.
    #[must_use]
    pub fn iter_tagged(&self, from: u64, tag: usize) -> Iter<'_, V, TAGS> {
        Iter::new(self, (tag < TAGS).then_some(from), Follow::Tag(tag))
    }

    /// Every entry, in key order.
    #[must_use]
    pub fn iter(&self) -> Iter<'_, V, TAGS> {
        self.iter_from(0)
    }
}

impl<'t, V, const TAGS: usize> IntoIterator for &'t RadixTree<V, TAGS> {
    type Item = (u64, &'t V);
    type IntoIter = Iter<'t, V, TAGS>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An in-order walk of a [`RadixTree`]'s entries, or of those carrying one
/// tag. Each step resumes from the path the last one took, so the whole walk
/// reaches each node it passes a bounded number of times, and a step the
/// tree's height at most.
pub struct Iter<'t, V, const TAGS: usize> {
    tree: &'t RadixTree<V, TAGS>,
    at: Walked,
    follow: Follow,
    /// The nodes above the last entry found.
    path: [u32; MAX_HEIGHT],
}

/// Where a walk stands.
#[derive(Copy, Clone)]
enum Walked {
    /// It starts at this key.
    From(u64),
    /// It last found this key, in this leaf.
    Found(u64, u32),
    Done,
}

impl<'t, V, const TAGS: usize> Iter<'t, V, TAGS> {
    fn new(tree: &'t RadixTree<V, TAGS>, from: Option<u64>, follow: Follow) -> Self {
        Self {
            tree,
            at: from.map_or(Walked::Done, Walked::From),
            follow,
            path: [NIL; MAX_HEIGHT],
        }
    }
}

impl<'t, V, const TAGS: usize> Iterator for Iter<'t, V, TAGS> {
    type Item = (u64, &'t V);

    fn next(&mut self) -> Option<Self::Item> {
        let found = match self.at {
            Walked::From(from) => self.tree.seek_up(from, self.follow, &mut self.path),
            Walked::Found(key, leaf) => {
                self.tree.seek_after(key, leaf, self.follow, &mut self.path)
            }
            Walked::Done => None,
        };
        let Some(found) = found else {
            self.at = Walked::Done;
            return None;
        };
        self.at = Walked::Found(found.0, found.1);
        self.tree.entry(found)
    }
}

#[cfg(test)]
#[path = "radix_tests.rs"]
mod tests;
