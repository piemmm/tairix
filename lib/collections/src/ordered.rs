//! [`OrderedMap`]: an ordered map, a B-tree whose nodes live in arenas.
//!
//! A node holds up to eleven entries in key order; every node but the root
//! holds at least five, and every leaf lies at one depth, so a lookup reaches
//! one node a level and compares at most five keys in each. A leaf holds its
//! entries, its parent and a free-list link; an interior node holds the same
//! and, inline, one child more than it has entries. Leaves and interior nodes
//! live in an arena each, addressed by `u32` index — the tree's height says
//! which arena an index names — and held in chunks of at most 16 KiB, so
//! growth allocates a chunk and moves no node. A freed node is chained into
//! its arena's free list, so a removal never allocates and an insertion takes
//! a freed node before its arena grows.
//!
//! An insertion counts the leaves and interior nodes its splits will take and
//! takes them before it changes anything, so a refused allocation leaves the
//! map as it was; [`OrderedMap::try_reserve`] takes the worst case of a run of
//! insertions up front in both arenas, after which none of them can fail.
//!
//! A mutable walk cannot borrow an arena's nodes in key order without
//! `unsafe`, so it moves each node it opens to an end of the stretch of its
//! arena it has not opened, re-pointing whatever named the nodes it moved:
//! the tree is whole after every step, and the walk costs what it visits.

use core::borrow::Borrow;
use core::fmt;
use core::iter::FusedIterator;
use core::mem::{self, MaybeUninit};
use core::ops::{Bound, RangeBounds};
use core::ptr;

use tairix_inline::ArrayVec;

use crate::arena::{fit_both, reserve_both, Arena, Block, Linked, NIL};
use crate::TryReserveError;

/// The minimum degree: a node holds at most `2B - 1` entries, and every node
/// but the root at least `B - 1`.
const B: usize = 6;
/// Entries a node holds at most.
const CAP: usize = 2 * B - 1;
/// Entries every node but the root holds at least.
const MIN: usize = B - 1;
/// Children an interior node holds at most.
const EDGES: usize = CAP + 1;
/// The tallest tree whose leaves `u32` indices can name: fourteen levels take
/// more than four billion.
const MAX_HEIGHT: usize = 13;
/// Nodes a mutable walk holds open at most: the paths down to both its ends.
const PATHS: usize = 2 * MAX_HEIGHT;

/// A leaf, and what every node holds.
#[derive(Clone)]
struct Leaf<K, V> {
    keys: ArrayVec<K, CAP>,
    vals: ArrayVec<V, CAP>,
    parent: u32,
    /// The freed node after this one, while this one is freed.
    next_free: u32,
}

/// An interior node: what every node holds, and its children inline.
#[derive(Clone)]
struct Interior<K, V> {
    node: Leaf<K, V>,
    /// One more than the keys.
    edges: ArrayVec<u32, EDGES>,
}

impl<K, V> Leaf<K, V> {
    /// The entry at `at`.
    fn entry(&self, at: usize) -> Option<(&K, &V)> {
        Some((self.keys.get(at)?, self.vals.get(at)?))
    }

    /// Where `key` is held, or where it would be inserted.
    fn search<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.keys.binary_search_by(|probe| probe.borrow().cmp(key))
    }

    /// Put an entry at `at`; the fill bounds leave room for it.
    fn insert(&mut self, at: usize, key: K, value: V) {
        put(&mut self.keys, at, key);
        put(&mut self.vals, at, value);
    }

    /// Append an entry; the fill bounds leave room for it.
    fn push(&mut self, key: K, value: V) {
        push(&mut self.keys, key);
        push(&mut self.vals, value);
    }

    /// Take out the entry at `at`.
    fn remove(&mut self, at: usize) -> Option<(K, V)> {
        self.keys.remove(at).zip(self.vals.remove(at))
    }

    /// Take out the last entry.
    fn pop(&mut self) -> Option<(K, V)> {
        self.keys.pop().zip(self.vals.pop())
    }

    /// Move the upper entries of this full node into the empty `upper`,
    /// answering the median left between the halves.
    fn split_into(&mut self, upper: &mut Self) -> Option<(K, V)> {
        while upper.keys.len() < MIN {
            let (key, value) = self.pop()?;
            upper.push(key, value);
        }
        upper.keys.reverse();
        upper.vals.reverse();
        self.pop()
    }

    /// Append the separator `(key, value)`, then every entry of `upper`,
    /// emptying it.
    fn absorb(&mut self, (key, value): (K, V), upper: &mut Self) {
        self.push(key, value);
        for key in mem::take(&mut upper.keys) {
            push(&mut self.keys, key);
        }
        for value in mem::take(&mut upper.vals) {
            push(&mut self.vals, value);
        }
    }

    /// Hand the separator at `sep` down to the front of `high`, taking the
    /// last entry of `low` up in its place.
    fn rotate_right(&mut self, sep: usize, low: &mut Self, high: &mut Self) -> Option<()> {
        let (key, value) = self.keys.get_mut(sep).zip(self.vals.get_mut(sep))?;
        let (key_up, value_up) = low.pop()?;
        let key = mem::replace(key, key_up);
        let value = mem::replace(value, value_up);
        high.insert(0, key, value);
        Some(())
    }

    /// Hand the separator at `sep` down to the end of `low`, taking the first
    /// entry of `high` up in its place.
    fn rotate_left(&mut self, sep: usize, low: &mut Self, high: &mut Self) -> Option<()> {
        let (key, value) = self.keys.get_mut(sep).zip(self.vals.get_mut(sep))?;
        let (key_up, value_up) = high.remove(0)?;
        let key = mem::replace(key, key_up);
        let value = mem::replace(value, value_up);
        low.push(key, value);
        Some(())
    }

    /// Empty the node for its free list.
    fn clear(&mut self) {
        self.keys.clear();
        self.vals.clear();
        self.parent = NIL;
    }
}

impl<K, V> Interior<K, V> {
    /// Where `child` sits among the children.
    fn place(&self, child: u32) -> Option<usize> {
        self.edges.iter().position(|&edge| edge == child)
    }

    /// Move the upper half of this full node, children with it, into the
    /// empty `upper`, answering the median left between the halves.
    fn split_into(&mut self, upper: &mut Self) -> Option<(K, V)> {
        while upper.edges.len() <= MIN {
            let edge = self.edges.pop()?;
            push(&mut upper.edges, edge);
        }
        upper.edges.reverse();
        self.node.split_into(&mut upper.node)
    }

    /// Append the separator, then every entry and child of `upper`, emptying
    /// it.
    fn absorb(&mut self, separator: (K, V), upper: &mut Self) {
        self.node.absorb(separator, &mut upper.node);
        for edge in mem::take(&mut upper.edges) {
            push(&mut self.edges, edge);
        }
    }

    /// Take out separator `sep` and the child after it.
    fn take_separator(&mut self, sep: usize) -> Option<(K, V)> {
        let separator = self.node.remove(sep)?;
        self.edges.remove(sep + 1);
        Some(separator)
    }

    /// [`Leaf::rotate_right`] between interior children: the last child of
    /// `low` moves with its entry, and is answered.
    fn rotate_right(&mut self, sep: usize, low: &mut Self, high: &mut Self) -> Option<u32> {
        self.node.rotate_right(sep, &mut low.node, &mut high.node)?;
        let edge = low.edges.pop()?;
        put(&mut high.edges, 0, edge);
        Some(edge)
    }

    /// [`Leaf::rotate_left`] between interior children: the first child of
    /// `high` moves with its entry, and is answered.
    fn rotate_left(&mut self, sep: usize, low: &mut Self, high: &mut Self) -> Option<u32> {
        self.node.rotate_left(sep, &mut low.node, &mut high.node)?;
        let edge = high.edges.remove(0)?;
        push(&mut low.edges, edge);
        Some(edge)
    }
}

impl<K, V> Linked for Leaf<K, V> {
    fn link(&self) -> u32 {
        self.next_free
    }

    fn set_link(&mut self, link: u32) {
        self.next_free = link;
    }

    fn write_empty(slot: &mut MaybeUninit<Self>) {
        let node = slot.as_mut_ptr();
        // SAFETY: every place lies within `slot`'s own aligned, writable
        // storage and is written, never read or dropped: the two vectors are
        // made empty where they lie and both links written, so the node is
        // whole once this returns.
        unsafe {
            let keys = ptr::addr_of_mut!((*node).keys).cast::<MaybeUninit<ArrayVec<K, CAP>>>();
            ArrayVec::init_empty(&mut *keys);
            let vals = ptr::addr_of_mut!((*node).vals).cast::<MaybeUninit<ArrayVec<V, CAP>>>();
            ArrayVec::init_empty(&mut *vals);
            ptr::addr_of_mut!((*node).parent).write(NIL);
            ptr::addr_of_mut!((*node).next_free).write(NIL);
        }
    }
}

impl<K, V> Linked for Interior<K, V> {
    fn link(&self) -> u32 {
        self.node.link()
    }

    fn set_link(&mut self, link: u32) {
        self.node.set_link(link);
    }

    fn write_empty(slot: &mut MaybeUninit<Self>) {
        let node = slot.as_mut_ptr();
        // SAFETY: as for a leaf: the embedded leaf and the edges each lie
        // within `slot`'s storage and are made empty there, nothing read or
        // dropped, so the node is whole once this returns.
        unsafe {
            let leaf = ptr::addr_of_mut!((*node).node).cast::<MaybeUninit<Leaf<K, V>>>();
            Leaf::write_empty(&mut *leaf);
            let edges =
                ptr::addr_of_mut!((*node).edges).cast::<MaybeUninit<ArrayVec<u32, EDGES>>>();
            ArrayVec::init_empty(&mut *edges);
        }
    }
}

/// A gap between neighbouring entries, named by the one leaf edge it falls
/// on: before `keys[at]` of `leaf`, or past its last key.
#[derive(Copy, Clone, Eq, PartialEq)]
struct Gap {
    leaf: u32,
    at: usize,
}

impl Gap {
    /// Where a walk over nothing stands.
    const NONE: Self = Self { leaf: NIL, at: 0 };

    /// The gap one entry on, in the same leaf.
    const fn step(self) -> Self {
        Self {
            leaf: self.leaf,
            at: self.at + 1,
        }
    }
}

/// A place among a node's entries: the node, its level above the leaves —
/// which says which arena `node` indexes — and the index there.
#[derive(Copy, Clone)]
struct Spot {
    node: u32,
    level: usize,
    at: usize,
}

impl Spot {
    /// The place in its leaf just past `gap`.
    const fn after(gap: Gap) -> Self {
        Self {
            node: gap.leaf,
            level: 0,
            at: gap.at,
        }
    }
}

/// Where a key is held, or the gap in a leaf it would be inserted at.
enum Slot {
    Held(Spot),
    Vacant(Gap),
}

/// A separator's node and the two leaves either side of it.
type Around<'a, K, V> = (&'a mut Interior<K, V>, [&'a mut Leaf<K, V>; 2]);

/// An ordered map: a B-tree of up to eleven entries a node, every leaf at
/// one depth, so a lookup, an insertion and a removal each reach one node a
/// level and compare at most five keys in each.
///
/// Iteration is in key order, from either end. Lookup, iteration, a range
/// query and every removal allocate nothing. An insertion takes the nodes
/// its splits need before it changes anything, so a refused allocation
/// leaves the map as it was, and [`try_reserve`](Self::try_reserve) makes
/// room for a run of insertions up front. There is no `Index`
/// implementation and no infallible insertion: a subscript that panics on a
/// missing key, or an insertion that aborts for want of memory, has no place
/// in a kernel.
///
/// A range whose start lies past its end admits nothing rather than
/// panicking.
///
/// The map does not scrub what it frees: a holder of a key, credential, or
/// capability token stores a value type that zeroes itself on drop.
pub struct OrderedMap<K, V> {
    leaves: Arena<Leaf<K, V>>,
    interiors: Arena<Interior<K, V>>,
    root: u32,
    /// Levels from the root to the leaves; zero while empty.
    height: usize,
    len: usize,
}

impl<K, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> OrderedMap<K, V> {
    /// An empty map, which allocates nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            leaves: Arena::new(),
            interiors: Arena::new(),
            root: NIL,
            height: 0,
            len: 0,
        }
    }

    /// An empty map with room for `capacity` entries inserted in any order.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the room cannot be had.
    pub fn try_with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        let mut map = Self::new();
        map.try_reserve(capacity)?;
        Ok(map)
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

    /// Bytes the node arenas hold, freed nodes and spare room included.
    #[must_use]
    pub fn allocated_bytes(&self) -> usize {
        self.leaves.bytes() + self.interiors.bytes()
    }

    /// Make the next `additional` insertions unable to fail, however their
    /// keys fall: room in both arenas for the most nodes their splits could
    /// take is made now, and nothing else changes. Removals in between only
    /// add to it; a shrink or a clear gives it back.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the room cannot be had; the map is as it was.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        let entries = self
            .len
            .checked_add(additional)
            .ok_or(TryReserveError::CapacityOverflow)?;
        // An insertion takes a leaf at most, and an interior node a level.
        let leaves = most_leaves(entries)
            .saturating_sub(self.leaves.live())
            .min(additional);
        let interiors = most_interiors(entries)
            .saturating_sub(self.interiors.live())
            .min(additional.saturating_mul(tallest(entries)));
        reserve_both(&mut self.leaves, &mut self.interiors, (leaves, interiors))
    }

    /// Drop every entry and give back every node.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Give back every freed node and any spare room, moving the live nodes
    /// to the front of their arenas.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the fitted arenas cannot be had; the map is
    /// then compacted, every entry where it was, but holds the room it had.
    pub fn try_shrink_to_fit(&mut self) -> Result<(), TryReserveError> {
        let Some(top) = self.height.checked_sub(1) else {
            self.clear();
            return Ok(());
        };
        let live = (self.leaves.live(), self.interiors.live());
        let mut holes = (0, 0);
        self.root = if top == 0 {
            self.leaves.relocate(self.root, live.0, &mut holes.0)
        } else {
            self.interiors.relocate(self.root, live.1, &mut holes.1)
        };
        // The interior nodes on the walk's path, root first, each with the
        // child it moves next: a node moves before its children do.
        let mut path: ArrayVec<(u32, usize), MAX_HEIGHT> = ArrayVec::new();
        if top > 0 {
            push(&mut path, (self.root, 0));
        }
        while let Some(&(node, next)) = path.last() {
            let level = top + 1 - path.len();
            let Some(child) = self.child(node, next) else {
                path.pop();
                continue;
            };
            let moved = if level == 1 {
                self.leaves.relocate(child, live.0, &mut holes.0)
            } else {
                self.interiors.relocate(child, live.1, &mut holes.1)
            };
            if let Some(edge) = self
                .interiors
                .nodes
                .get_mut(node as usize)
                .and_then(|held| held.edges.get_mut(next))
            {
                *edge = moved;
            }
            self.adopt(node, moved, level - 1);
            if let Some(last) = path.last_mut() {
                last.1 += 1;
            }
            if level > 1 {
                push(&mut path, (moved, 0));
            }
        }
        fit_both(&mut self.leaves, &mut self.interiors, live)
    }

    /// The entry with the least key.
    #[must_use]
    pub fn first_key_value(&self) -> Option<(&K, &V)> {
        let (spot, _) = self.next_entry(self.before_all()?)?;
        self.entry(spot)
    }

    /// The entry with the greatest key.
    #[must_use]
    pub fn last_key_value(&self) -> Option<(&K, &V)> {
        let (spot, _) = self.prev_entry(self.after_all()?)?;
        self.entry(spot)
    }

    /// Take out the entry with the least key.
    pub fn pop_first(&mut self) -> Option<(K, V)> {
        let (spot, _) = self.next_entry(self.before_all()?)?;
        self.remove_at(spot)
    }

    /// Take out the entry with the greatest key.
    pub fn pop_last(&mut self) -> Option<(K, V)> {
        let (spot, _) = self.prev_entry(self.after_all()?)?;
        self.remove_at(spot)
    }

    /// Every entry, in key order.
    #[must_use]
    pub fn iter(&self) -> Iter<'_, K, V> {
        match self.before_all().zip(self.after_all()) {
            Some((front, back)) => Iter {
                map: self,
                front,
                back,
                remaining: self.len,
            },
            None => Iter {
                map: self,
                front: Gap::NONE,
                back: Gap::NONE,
                remaining: 0,
            },
        }
    }

    /// Every entry, in key order, its value borrowed to change in place.
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V> {
        let ends = self.before_all().zip(self.after_all());
        let remaining = if ends.is_some() { self.len } else { 0 };
        let walk = match ends {
            Some((front, back)) => Opened::new(self, front, back),
            None => Opened::empty(),
        };
        IterMut { walk, remaining }
    }

    /// Every key, in order.
    #[must_use]
    pub fn keys(&self) -> Keys<'_, K, V> {
        Keys { inner: self.iter() }
    }

    /// Every value, in key order.
    #[must_use]
    pub fn values(&self) -> Values<'_, K, V> {
        Values { inner: self.iter() }
    }

    /// Every value, in key order, borrowed to change in place.
    pub fn values_mut(&mut self) -> ValuesMut<'_, K, V> {
        ValuesMut {
            inner: self.iter_mut(),
        }
    }

    /// The leaves and interior nodes an insertion into `leaf` takes: one for
    /// each full node it splits on the way up, and a new root if the root
    /// splits too.
    fn splits_at(&self, leaf: u32) -> (usize, usize) {
        let (mut node, mut level, mut taken) = (leaf, 0, (0, 0));
        while let Some(held) = self.node(node, level) {
            if held.keys.len() < CAP {
                return taken;
            }
            if level == 0 {
                taken.0 += 1;
            } else {
                taken.1 += 1;
            }
            if held.parent == NIL {
                return (taken.0, taken.1 + 1);
            }
            (node, level) = (held.parent, level + 1);
        }
        taken
    }

    /// Insert at `gap`, splitting each full node on the way up into the nodes
    /// [`Self::splits_at`] counted, which are reserved.
    fn insert_at(&mut self, gap: Gap, key: K, value: V) {
        let mut spot = Spot::after(gap);
        let mut carried = (key, value, NIL);
        while let Some(held) = self.node(spot.node, spot.level) {
            if held.keys.len() < CAP {
                self.put_entry(spot, carried);
                self.len += 1;
                return;
            }
            let parent = held.parent;
            let Some((sibling, median)) = self.split(spot.node, spot.level) else {
                return;
            };
            // The carried entry joins the half it sorts into.
            if spot.at <= MIN {
                self.put_entry(spot, carried);
            } else {
                let upper = Spot {
                    node: sibling,
                    at: spot.at - MIN - 1,
                    ..spot
                };
                self.put_entry(upper, carried);
            }
            if parent == NIL {
                self.grow(spot.node, sibling, median);
                self.len += 1;
                return;
            }
            let Some(at) = self.place(parent, spot.node) else {
                return;
            };
            spot = Spot {
                node: parent,
                level: spot.level + 1,
                at,
            };
            carried = (median.0, median.1, sibling);
        }
    }

    /// Move the upper half of the full node at `index`, `level` levels up,
    /// into a sibling taken from its arena, answering the sibling and the
    /// median entry left between the halves.
    fn split(&mut self, index: u32, level: usize) -> Option<(u32, (K, V))> {
        if level == 0 {
            let sibling = self.leaves.take();
            let pair = [index as usize, sibling as usize];
            let [Some(lower), Some(upper)] = self.leaves.nodes.disjoint_mut(pair) else {
                return None;
            };
            return Some((sibling, lower.split_into(upper)?));
        }
        let sibling = self.interiors.take();
        let pair = [index as usize, sibling as usize];
        let [Some(lower), Some(upper)] = self.interiors.nodes.disjoint_mut(pair) else {
            return None;
        };
        let median = lower.split_into(upper)?;
        self.adopt_all(sibling, level);
        Some((sibling, median))
    }

    /// Put an entry at `spot`, in a node with room, with `right` the child
    /// after it where the node is interior.
    fn put_entry(&mut self, spot: Spot, (key, value, right): (K, V, u32)) {
        let Some(below) = spot.level.checked_sub(1) else {
            if let Some(leaf) = self.leaves.nodes.get_mut(spot.node as usize) {
                leaf.insert(spot.at, key, value);
            }
            return;
        };
        if let Some(held) = self.interiors.nodes.get_mut(spot.node as usize) {
            held.node.insert(spot.at, key, value);
            put(&mut held.edges, spot.at + 1, right);
        }
        self.adopt(spot.node, right, below);
    }

    /// A new root above `lower` and `upper`, holding the entry between them.
    fn grow(&mut self, lower: u32, upper: u32, (key, value): (K, V)) {
        let root = self.interiors.take();
        if let Some(held) = self.interiors.nodes.get_mut(root as usize) {
            held.node.push(key, value);
            push(&mut held.edges, lower);
            push(&mut held.edges, upper);
        }
        self.adopt_all(root, self.height);
        self.root = root;
        self.height += 1;
    }

    /// What every node holds, of the node at `index`, `level` levels above
    /// the leaves.
    fn node(&self, index: u32, level: usize) -> Option<&Leaf<K, V>> {
        if level == 0 {
            self.leaves.nodes.get(index as usize)
        } else {
            let held = self.interiors.nodes.get(index as usize)?;
            Some(&held.node)
        }
    }

    /// [`Self::node`], to change.
    fn node_mut(&mut self, index: u32, level: usize) -> Option<&mut Leaf<K, V>> {
        if level == 0 {
            self.leaves.nodes.get_mut(index as usize)
        } else {
            let held = self.interiors.nodes.get_mut(index as usize)?;
            Some(&mut held.node)
        }
    }

    /// Child `at` of the interior node at `index`.
    fn child(&self, index: u32, at: usize) -> Option<u32> {
        let held = self.interiors.nodes.get(index as usize)?;
        held.edges.get(at).copied()
    }

    /// Where `child` sits among the children of the interior node at `parent`.
    fn place(&self, parent: u32, child: u32) -> Option<usize> {
        self.interiors.nodes.get(parent as usize)?.place(child)
    }

    /// Make `child`, `level` levels up, name `parent` as its parent; `NIL`
    /// names no child.
    fn adopt(&mut self, parent: u32, child: u32, level: usize) {
        if let Some(node) = self.node_mut(child, level) {
            node.parent = parent;
        }
    }

    /// Make every child of the interior node at `index`, `level` levels up,
    /// name it as parent.
    fn adopt_all(&mut self, index: u32, level: usize) {
        let Some(below) = level.checked_sub(1) else {
            return;
        };
        let Some(held) = self.interiors.nodes.get(index as usize) else {
            return;
        };
        let edges = held.edges.clone();
        for &child in &edges {
            self.adopt(index, child, below);
        }
    }

    /// Take out the entry at `spot`, restoring the fill bound behind it.
    fn remove_at(&mut self, spot: Spot) -> Option<(K, V)> {
        // An interior entry trades places with its predecessor, the last
        // entry of the subtree before it, which leaves a leaf short.
        let (taken, short) = if let Some(below) = spot.level.checked_sub(1) {
            let left = self.child(spot.node, spot.at)?;
            let leaf = self.last_gap(left, below)?.leaf;
            let (key, value) = self.leaves.nodes.get_mut(leaf as usize)?.pop()?;
            let held = &mut self.interiors.nodes.get_mut(spot.node as usize)?.node;
            let key = mem::replace(held.keys.get_mut(spot.at)?, key);
            let value = mem::replace(held.vals.get_mut(spot.at)?, value);
            ((key, value), leaf)
        } else {
            let leaf = self.leaves.nodes.get_mut(spot.node as usize)?;
            (leaf.remove(spot.at)?, spot.node)
        };
        self.len = self.len.saturating_sub(1);
        self.rebalance(short);
        Some(taken)
    }

    /// Restore the fill bound at the leaf `short` after it lost an entry:
    /// borrow from a sibling that can spare one, else merge with one and carry
    /// the shortfall up; an emptied root gives way to its one child.
    fn rebalance(&mut self, short: u32) {
        let (mut node, mut level) = (short, 0);
        loop {
            let Some(held) = self.node(node, level) else {
                return;
            };
            if held.parent == NIL {
                self.lower_root();
                return;
            }
            if held.keys.len() >= MIN {
                return;
            }
            let parent = held.parent;
            let Some(at) = self.place(parent, node) else {
                return;
            };
            let fill = |child: Option<usize>| {
                let sibling = self.child(parent, child?)?;
                Some(self.node(sibling, level)?.keys.len())
            };
            let (left, right) = (fill(at.checked_sub(1)), fill(Some(at + 1)));
            if left.is_some_and(|len| len > MIN) {
                self.rotate_right(parent, at - 1, level);
                return;
            }
            if right.is_some_and(|len| len > MIN) {
                self.rotate_left(parent, at, level);
                return;
            }
            let separator = if left.is_some() { at - 1 } else { at };
            if !self.merge(parent, separator, level) {
                return;
            }
            (node, level) = (parent, level + 1);
        }
    }

    /// The children either side of separator `sep` of `parent`.
    fn beside(&self, parent: u32, sep: usize) -> Option<(u32, u32)> {
        Some((self.child(parent, sep)?, self.child(parent, sep + 1)?))
    }

    /// The interior node `parent` and its leaves `left` and `right`.
    fn leaves_of(&mut self, parent: u32, (left, right): (u32, u32)) -> Option<Around<'_, K, V>> {
        let above = self.interiors.nodes.get_mut(parent as usize)?;
        let pair = [left as usize, right as usize];
        let [Some(low), Some(high)] = self.leaves.nodes.disjoint_mut(pair) else {
            return None;
        };
        Some((above, [low, high]))
    }

    /// Move the last entry of the child before separator `sep` of `parent` up
    /// into it, and the separator down to the front of the child after it;
    /// the children are `level` levels up.
    fn rotate_right(&mut self, parent: u32, sep: usize, level: usize) {
        let Some((left, right)) = self.beside(parent, sep) else {
            return;
        };
        let Some(below) = level.checked_sub(1) else {
            if let Some((above, [low, high])) = self.leaves_of(parent, (left, right)) {
                above.node.rotate_right(sep, low, high);
            }
            return;
        };
        let indices = [parent as usize, left as usize, right as usize];
        let [Some(above), Some(low), Some(high)] = self.interiors.nodes.disjoint_mut(indices)
        else {
            return;
        };
        if let Some(edge) = above.rotate_right(sep, low, high) {
            self.adopt(right, edge, below);
        }
    }

    /// Move the first entry of the child after separator `sep` of `parent` up
    /// into it, and the separator down to the end of the child before it; the
    /// children are `level` levels up.
    fn rotate_left(&mut self, parent: u32, sep: usize, level: usize) {
        let Some((left, right)) = self.beside(parent, sep) else {
            return;
        };
        let Some(below) = level.checked_sub(1) else {
            if let Some((above, [low, high])) = self.leaves_of(parent, (left, right)) {
                above.node.rotate_left(sep, low, high);
            }
            return;
        };
        let indices = [parent as usize, left as usize, right as usize];
        let [Some(above), Some(low), Some(high)] = self.interiors.nodes.disjoint_mut(indices)
        else {
            return;
        };
        if let Some(edge) = above.rotate_left(sep, low, high) {
            self.adopt(left, edge, below);
        }
    }

    /// Fold separator `sep` of `parent` and the child after it into the child
    /// before it, freeing the emptied child; the children are `level` levels
    /// up. Answers whether it merged.
    fn merge(&mut self, parent: u32, sep: usize, level: usize) -> bool {
        let Some((left, right)) = self.beside(parent, sep) else {
            return false;
        };
        if level == 0 {
            let Some((above, [low, high])) = self.leaves_of(parent, (left, right)) else {
                return false;
            };
            let Some(separator) = above.take_separator(sep) else {
                return false;
            };
            low.absorb(separator, high);
            self.free_leaf(right);
            return true;
        }
        let indices = [parent as usize, left as usize, right as usize];
        let [Some(above), Some(low), Some(high)] = self.interiors.nodes.disjoint_mut(indices)
        else {
            return false;
        };
        let Some(separator) = above.take_separator(sep) else {
            return false;
        };
        low.absorb(separator, high);
        self.free_interior(right);
        self.adopt_all(left, level);
        true
    }

    /// Retire an emptied root: the tree empties, or the root's one child
    /// takes its place.
    fn lower_root(&mut self) {
        let root = self.root;
        let Some(below) = self.height.checked_sub(2) else {
            let emptied = self.leaves.nodes.get(root as usize);
            if emptied.is_some_and(|leaf| leaf.keys.is_empty()) {
                self.free_leaf(root);
                (self.root, self.height) = (NIL, 0);
            }
            return;
        };
        let Some(held) = self.interiors.nodes.get_mut(root as usize) else {
            return;
        };
        if !held.node.keys.is_empty() {
            return;
        }
        let child = held.edges.pop();
        self.free_interior(root);
        if let Some(child) = child {
            self.adopt(NIL, child, below);
            (self.root, self.height) = (child, below + 1);
        } else {
            (self.root, self.height) = (NIL, 0);
        }
    }

    /// Chain an emptied leaf into its free list.
    fn free_leaf(&mut self, index: u32) {
        let Some(leaf) = self.leaves.nodes.get_mut(index as usize) else {
            return;
        };
        debug_assert!(leaf.keys.is_empty(), "a freed leaf held entries");
        leaf.clear();
        self.leaves.free(index);
    }

    /// Chain an emptied interior node into its free list.
    fn free_interior(&mut self, index: u32) {
        let Some(held) = self.interiors.nodes.get_mut(index as usize) else {
            return;
        };
        debug_assert!(
            held.node.keys.is_empty() && held.edges.is_empty(),
            "a freed node held entries"
        );
        held.node.clear();
        held.edges.clear();
        self.interiors.free(index);
    }

    /// The places a descent to `gap` takes, root first: the child it enters
    /// at each interior level, then the gap's index in its leaf.
    fn path_to(&self, gap: Gap) -> Option<ArrayVec<usize, MAX_HEIGHT>> {
        let mut path: ArrayVec<usize, MAX_HEIGHT> = ArrayVec::new();
        path.try_push(gap.at).ok()?;
        let mut node = gap.leaf;
        let mut parent = self.leaves.nodes.get(node as usize)?.parent;
        while parent != NIL {
            path.try_push(self.place(parent, node)?).ok()?;
            node = parent;
            parent = self.interiors.nodes.get(node as usize)?.node.parent;
        }
        path.reverse();
        Some(path)
    }
}

impl<K: Ord, V> OrderedMap<K, V> {
    /// The value at `key`.
    #[must_use]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get_key_value(key).map(|(_, value)| value)
    }

    /// The stored key equal to `key`, and its value.
    #[must_use]
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Slot::Held(spot) = self.slot_of(key)? else {
            return None;
        };
        self.entry(spot)
    }

    /// The value at `key`, to change in place.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Slot::Held(Spot { node, level, at }) = self.slot_of(key)? else {
            return None;
        };
        self.node_mut(node, level)?.vals.get_mut(at)
    }

    /// Whether `key` holds a value.
    #[must_use]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        matches!(self.slot_of(key), Some(Slot::Held(..)))
    }

    /// The entries whose keys `range` admits, in key order.
    #[must_use]
    pub fn range<Q, R>(&self, range: R) -> Range<'_, K, V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        R: RangeBounds<Q>,
    {
        let (front, back) = self.gaps_of(&range).unwrap_or((Gap::NONE, Gap::NONE));
        Range {
            map: self,
            front,
            back,
        }
    }

    /// The entries whose keys `range` admits, in key order, each value
    /// borrowed to change in place.
    pub fn range_mut<Q, R>(&mut self, range: R) -> RangeMut<'_, K, V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        R: RangeBounds<Q>,
    {
        let walk = match self.gaps_of(&range) {
            Some((front, back)) => Opened::new(self, front, back),
            None => Opened::empty(),
        };
        RangeMut { walk }
    }

    /// Put `value` at `key`, answering the value it replaces.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when a node its splits need cannot be had: the map
    /// is as it was, and `key` and `value` are dropped. Replacing the value
    /// of a key already held never fails.
    pub fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, TryReserveError> {
        if self.root == NIL {
            self.leaves.reserve(1)?;
            let root = self.leaves.take();
            if let Some(leaf) = self.leaves.nodes.get_mut(root as usize) {
                leaf.push(key, value);
            }
            (self.root, self.height, self.len) = (root, 1, 1);
            return Ok(None);
        }
        match self.slot_of(&key) {
            Some(Slot::Held(spot)) => {
                let held = self.node_mut(spot.node, spot.level);
                Ok(held
                    .and_then(|held| held.vals.get_mut(spot.at))
                    .map(|held| mem::replace(held, value)))
            }
            Some(Slot::Vacant(gap)) => {
                let needed = self.splits_at(gap.leaf);
                reserve_both(&mut self.leaves, &mut self.interiors, needed)?;
                self.insert_at(gap, key, value);
                Ok(None)
            }
            None => {
                debug_assert!(false, "a descent reached an index that names no node");
                Err(TryReserveError::AllocFailed)
            }
        }
    }

    /// Take out the value at `key`.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.remove_entry(key).map(|(_, value)| value)
    }

    /// Take out the entry at `key`, with its stored key.
    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Slot::Held(spot) = self.slot_of(key)? else {
            return None;
        };
        self.remove_at(spot)
    }

    /// Keep only the entries `keep` accepts, visited in key order: a kept
    /// entry costs a step of the walk, a dropped one a removal and a search
    /// back to where the walk stood.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        let mut gap = self.before_all();
        while let Some(found) = gap {
            let Some((spot, after)) = self.next_entry(found) else {
                return;
            };
            let Some(held) = self.node_mut(spot.node, spot.level) else {
                return;
            };
            let at = spot.at;
            let (Some(key), Some(value)) = (held.keys.get(at), held.vals.get_mut(at)) else {
                return;
            };
            if keep(key, value) {
                gap = Some(after);
            } else {
                let Some((key, _)) = self.remove_at(spot) else {
                    return;
                };
                gap = self.gap_at(Bound::Excluded(&key), false);
            }
        }
    }

    /// File the entry at `key` under `new` instead, which must sort after the
    /// entry before it and before the entry after it. Answers whether it did:
    /// a missing entry, or a key that would move it, changes nothing.
    pub(crate) fn rekey<Q>(&mut self, key: &Q, new: K) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some(Slot::Held(spot)) = self.slot_of(key) else {
            return false;
        };
        let Spot { node, level, at } = spot;
        // The gaps either side of the entry, past which its neighbours lie.
        let (before, after) = if let Some(child_level) = level.checked_sub(1) {
            let before = self
                .child(node, at)
                .and_then(|left| self.last_gap(left, child_level));
            let after = self
                .child(node, at + 1)
                .and_then(|right| self.first_gap(right, child_level));
            (before, after)
        } else {
            let gap = Gap { leaf: node, at };
            (Some(gap), Some(gap.step()))
        };
        let below = before
            .and_then(|gap| self.prev_entry(gap))
            .and_then(|(entry, _)| self.entry(entry));
        let above = after
            .and_then(|gap| self.next_entry(gap))
            .and_then(|(entry, _)| self.entry(entry));
        let after_below = below.is_none_or(|(below, _)| *below < new);
        let before_above = above.is_none_or(|(above, _)| new < *above);
        if !after_below || !before_above {
            return false;
        }
        match self
            .node_mut(node, level)
            .and_then(|held| held.keys.get_mut(at))
        {
            Some(held) => {
                *held = new;
                true
            }
            None => false,
        }
    }

    /// Where `key` is held, or the leaf gap it would be inserted at; `None`
    /// for an empty map.
    fn slot_of<Q>(&self, key: &Q) -> Option<Slot>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let interiors = self.interiors.nodes.view();
        let mut node = self.root;
        for level in (1..self.height).rev() {
            let held = interiors.get(node as usize)?;
            match held.node.search(key) {
                Ok(at) => return Some(Slot::Held(Spot { node, level, at })),
                Err(at) => node = *held.edges.get(at)?,
            }
        }
        match self.leaves.nodes.get(node as usize)?.search(key) {
            Ok(at) => Some(Slot::Held(Spot { node, level: 0, at })),
            Err(at) => Some(Slot::Vacant(Gap { leaf: node, at })),
        }
    }

    /// The gap a bound falls at: before the first entry a lower bound
    /// admits, or past the last an upper one does.
    fn gap_at<Q>(&self, bound: Bound<&Q>, upper: bool) -> Option<Gap>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        // A key equal to the bound lies before the gap when an upper bound
        // includes it or a lower one excludes it.
        let past_equal = matches!(bound, Bound::Included(_)) == upper;
        let position = |held: &Leaf<K, V>| match bound {
            Bound::Included(key) | Bound::Excluded(key) => match held.search(key) {
                Ok(at) if past_equal => at + 1,
                Ok(at) | Err(at) => at,
            },
            Bound::Unbounded if upper => held.keys.len(),
            Bound::Unbounded => 0,
        };
        let mut node = self.root;
        for _ in 1..self.height {
            let held = self.interiors.nodes.get(node as usize)?;
            node = *held.edges.get(position(&held.node))?;
        }
        let at = position(self.leaves.nodes.get(node as usize)?);
        Some(Gap { leaf: node, at })
    }

    /// The gaps a range's ends fall at, or `None` when its ends cross and it
    /// admits nothing.
    fn gaps_of<Q, R>(&self, range: &R) -> Option<(Gap, Gap)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        R: RangeBounds<Q>,
    {
        let (start, end) = (range.start_bound(), range.end_bound());
        if crossed(start, end) {
            return None;
        }
        Some((self.gap_at(start, false)?, self.gap_at(end, true)?))
    }
}

impl<K: Clone, V: Clone> OrderedMap<K, V> {
    /// A copy of the map, node for node.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the copy's arenas cannot be had.
    pub fn try_clone(&self) -> Result<Self, TryReserveError> {
        Ok(Self {
            leaves: self.leaves.try_clone()?,
            interiors: self.interiors.try_clone()?,
            root: self.root,
            height: self.height,
            len: self.len,
        })
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for OrderedMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: PartialEq, V: PartialEq> PartialEq for OrderedMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<K: Eq, V: Eq> Eq for OrderedMap<K, V> {}

impl<K, V> OrderedMap<K, V> {
    /// The gap before every entry.
    fn before_all(&self) -> Option<Gap> {
        self.first_gap(self.root, self.height.checked_sub(1)?)
    }

    /// The gap past every entry.
    fn after_all(&self) -> Option<Gap> {
        self.last_gap(self.root, self.height.checked_sub(1)?)
    }

    /// The gap before the first entry of the subtree at `node`, `level`
    /// levels up.
    fn first_gap(&self, mut node: u32, level: usize) -> Option<Gap> {
        for _ in 0..level {
            node = *self.interiors.nodes.get(node as usize)?.edges.first()?;
        }
        self.leaves.nodes.get(node as usize)?;
        Some(Gap { leaf: node, at: 0 })
    }

    /// The gap past the last entry of the subtree at `node`, `level` levels
    /// up.
    fn last_gap(&self, mut node: u32, level: usize) -> Option<Gap> {
        for _ in 0..level {
            node = *self.interiors.nodes.get(node as usize)?.edges.last()?;
        }
        let at = self.leaves.nodes.get(node as usize)?.keys.len();
        Some(Gap { leaf: node, at })
    }

    /// The entry after `gap`, and the gap after it.
    fn next_entry(&self, gap: Gap) -> Option<(Spot, Gap)> {
        let leaf = self.leaves.nodes.get(gap.leaf as usize)?;
        if gap.at < leaf.keys.len() {
            return Some((Spot::after(gap), gap.step()));
        }
        let (mut child, mut node, mut level) = (gap.leaf, leaf.parent, 1);
        loop {
            let held = self.interiors.nodes.get(node as usize)?;
            let at = held.place(child)?;
            if at < held.node.keys.len() {
                let after = self.first_gap(*held.edges.get(at + 1)?, level - 1)?;
                return Some((Spot { node, level, at }, after));
            }
            (child, node, level) = (node, held.node.parent, level + 1);
        }
    }

    /// The entry before `gap`, and the gap before it.
    fn prev_entry(&self, gap: Gap) -> Option<(Spot, Gap)> {
        let leaf = self.leaves.nodes.get(gap.leaf as usize)?;
        if let Some(at) = gap.at.checked_sub(1) {
            let before = Gap { at, ..gap };
            return Some((Spot::after(before), before));
        }
        let (mut child, mut node, mut level) = (gap.leaf, leaf.parent, 1);
        loop {
            let held = self.interiors.nodes.get(node as usize)?;
            if let Some(at) = held.place(child)?.checked_sub(1) {
                let before = self.last_gap(*held.edges.get(at)?, level - 1)?;
                return Some((Spot { node, level, at }, before));
            }
            (child, node, level) = (node, held.node.parent, level + 1);
        }
    }

    /// The key and value at `spot`.
    fn entry(&self, spot: Spot) -> Option<(&K, &V)> {
        self.node(spot.node, spot.level)?.entry(spot.at)
    }
}

/// Whether two bounds admit nothing because they cross: a start past the end,
/// or at it with either end excluded.
fn crossed<Q: Ord + ?Sized>(start: Bound<&Q>, end: Bound<&Q>) -> bool {
    match (start, end) {
        (Bound::Included(low), Bound::Included(high)) => low > high,
        (
            Bound::Included(low) | Bound::Excluded(low),
            Bound::Included(high) | Bound::Excluded(high),
        ) => low >= high,
        _ => false,
    }
}

/// The most leaves a tree of `entries` entries can hold: every leaf at the
/// fill floor, and a separator between each two. A tree above one level holds
/// a root entry over two leaves at the floor, so fewer entries than that are
/// one leaf.
const fn most_leaves(entries: usize) -> usize {
    match entries {
        0 => 0,
        1..=TWO_FLOORS => 1,
        _ => entries.saturating_add(1) / B,
    }
}

/// Entries two nodes at the fill floor hold.
const TWO_FLOORS: usize = 2 * MIN;

/// The most interior nodes a tree of `entries` entries can hold: its
/// separators, one fewer than its leaves, spread at the fill floor over every
/// interior node but the root.
const fn most_interiors(entries: usize) -> usize {
    match most_leaves(entries) {
        0 | 1 => 0,
        leaves => 1 + (leaves - 2) / MIN,
    }
}

/// The tallest a tree of `entries` entries can stand: a root of one entry
/// above nodes at the fill floor, which `2 * B^(h - 1) - 1` entries fill to
/// height `h`.
fn tallest(entries: usize) -> usize {
    let (mut height, mut span) = (0, 1usize);
    while span
        .checked_mul(2)
        .is_some_and(|twice| twice - 1 <= entries)
    {
        height += 1;
        match span.checked_mul(B) {
            Some(next) => span = next,
            None => break,
        }
    }
    height
}

/// Append to one of a node's arrays, which the fill bounds keep from full.
fn push<T, const N: usize>(array: &mut ArrayVec<T, N>, item: T) {
    let pushed = array.try_push(item);
    debug_assert!(pushed.is_ok(), "a node passed its bound");
}

/// Insert into one of a node's arrays at `at`, as [`push`] appends.
fn put<T, const N: usize>(array: &mut ArrayVec<T, N>, at: usize, item: T) {
    let inserted = array.try_insert(at, item);
    debug_assert!(inserted.is_ok(), "a node passed its bound");
}

impl<'a, K, V> IntoIterator for &'a OrderedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut OrderedMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V> IntoIterator for OrderedMap<K, V> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        let front = self.before_all().map_or(NIL, |gap| gap.leaf);
        let back = self.after_all().map_or(NIL, |gap| gap.leaf);
        let remaining = self.len;
        IntoIter {
            map: self,
            front,
            back,
            remaining,
        }
    }
}

/// The entries of an [`OrderedMap`], in key order.
pub struct Iter<'a, K, V> {
    map: &'a OrderedMap<K, V>,
    front: Gap,
    back: Gap,
    remaining: usize,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (spot, after) = self.map.next_entry(self.front)?;
        self.front = after;
        self.remaining -= 1;
        self.map.entry(spot)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> DoubleEndedIterator for Iter<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (spot, before) = self.map.prev_entry(self.back)?;
        self.back = before;
        self.remaining -= 1;
        self.map.entry(spot)
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {
    fn len(&self) -> usize {
        self.remaining
    }
}

impl<K, V> FusedIterator for Iter<'_, K, V> {}

/// The keys of an [`OrderedMap`], in order.
pub struct Keys<'a, K, V> {
    inner: Iter<'a, K, V>,
}

impl<'a, K, V> Iterator for Keys<'a, K, V> {
    type Item = &'a K;

    fn next(&mut self) -> Option<&'a K> {
        self.inner.next().map(|(key, _)| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> DoubleEndedIterator for Keys<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(key, _)| key)
    }
}

impl<K, V> ExactSizeIterator for Keys<'_, K, V> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V> FusedIterator for Keys<'_, K, V> {}

/// The values of an [`OrderedMap`], in key order.
pub struct Values<'a, K, V> {
    inner: Iter<'a, K, V>,
}

impl<'a, K, V> Iterator for Values<'a, K, V> {
    type Item = &'a V;

    fn next(&mut self) -> Option<&'a V> {
        self.inner.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> DoubleEndedIterator for Values<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, value)| value)
    }
}

impl<K, V> ExactSizeIterator for Values<'_, K, V> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V> FusedIterator for Values<'_, K, V> {}

/// The entries of an [`OrderedMap`] a range admits, in key order.
pub struct Range<'a, K, V> {
    map: &'a OrderedMap<K, V>,
    front: Gap,
    back: Gap,
}

impl<'a, K, V> Iterator for Range<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        let (spot, after) = self.map.next_entry(self.front)?;
        self.front = after;
        self.map.entry(spot)
    }
}

impl<K, V> DoubleEndedIterator for Range<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        let (spot, before) = self.map.prev_entry(self.back)?;
        self.back = before;
        self.map.entry(spot)
    }
}

impl<K, V> FusedIterator for Range<'_, K, V> {}

/// The entries of an [`OrderedMap`], in key order, each value borrowed to
/// change in place.
pub struct IterMut<'a, K, V> {
    walk: Opened<'a, K, V>,
    remaining: usize,
}

impl<'a, K, V> Iterator for IterMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let entry = self.walk.next_front()?;
        self.remaining -= 1;
        Some(entry)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> DoubleEndedIterator for IterMut<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let entry = self.walk.next_back()?;
        self.remaining -= 1;
        Some(entry)
    }
}

impl<K, V> ExactSizeIterator for IterMut<'_, K, V> {
    fn len(&self) -> usize {
        self.remaining
    }
}

impl<K, V> FusedIterator for IterMut<'_, K, V> {}

/// The values of an [`OrderedMap`], in key order, each borrowed to change in
/// place.
pub struct ValuesMut<'a, K, V> {
    inner: IterMut<'a, K, V>,
}

impl<'a, K, V> Iterator for ValuesMut<'a, K, V> {
    type Item = &'a mut V;

    fn next(&mut self) -> Option<&'a mut V> {
        self.inner.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> DoubleEndedIterator for ValuesMut<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, value)| value)
    }
}

impl<K, V> ExactSizeIterator for ValuesMut<'_, K, V> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V> FusedIterator for ValuesMut<'_, K, V> {}

/// The entries of an [`OrderedMap`] a range admits, in key order, each value
/// borrowed to change in place.
pub struct RangeMut<'a, K, V> {
    walk: Opened<'a, K, V>,
}

impl<'a, K, V> Iterator for RangeMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<Self::Item> {
        self.walk.next_front()
    }
}

impl<K, V> DoubleEndedIterator for RangeMut<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.walk.next_back()
    }
}

impl<K, V> FusedIterator for RangeMut<'_, K, V> {}

/// The entries of an [`OrderedMap`], taken out in key order.
pub struct IntoIter<K, V> {
    /// The map taken apart, which nothing else reads.
    map: OrderedMap<K, V>,
    /// The leaf the front takes from.
    front: u32,
    /// The leaf the back takes from.
    back: u32,
    remaining: usize,
}

impl<K, V> Iterator for IntoIter<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        if self.remaining == 0 {
            return None;
        }
        let leaf = self.map.leaves.nodes.get_mut(self.front as usize)?;
        if let Some(entry) = leaf.remove(0) {
            self.remaining -= 1;
            return Some(entry);
        }
        // Nothing reads the tree after this walk, so a drained node is
        // dropped from its parent rather than rebalanced.
        let (mut parent, mut level) = (leaf.parent, 1);
        loop {
            let above = self.map.interiors.nodes.get_mut(parent as usize)?;
            above.edges.remove(0);
            if let Some(entry) = above.node.remove(0) {
                let next = above.edges.first().copied();
                self.front = next
                    .and_then(|next| self.map.first_gap(next, level - 1))
                    .map_or(NIL, |gap| gap.leaf);
                self.remaining -= 1;
                return Some(entry);
            }
            (parent, level) = (above.node.parent, level + 1);
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> DoubleEndedIterator for IntoIter<K, V> {
    fn next_back(&mut self) -> Option<(K, V)> {
        if self.remaining == 0 {
            return None;
        }
        let leaf = self.map.leaves.nodes.get_mut(self.back as usize)?;
        if let Some(entry) = leaf.pop() {
            self.remaining -= 1;
            return Some(entry);
        }
        let (mut parent, mut level) = (leaf.parent, 1);
        loop {
            let above = self.map.interiors.nodes.get_mut(parent as usize)?;
            above.edges.pop();
            if let Some(entry) = above.node.pop() {
                let next = above.edges.last().copied();
                self.back = next
                    .and_then(|next| self.map.last_gap(next, level - 1))
                    .map_or(NIL, |gap| gap.leaf);
                self.remaining -= 1;
                return Some(entry);
            }
            (parent, level) = (above.node.parent, level + 1);
        }
    }
}

impl<K, V> ExactSizeIterator for IntoIter<K, V> {
    fn len(&self) -> usize {
        self.remaining
    }
}

impl<K, V> FusedIterator for IntoIter<K, V> {}

/// An opened node: its arena index, what every node holds, and its children.
type Opening<'a, K, V> = (u32, &'a mut Leaf<K, V>, &'a mut [u32]);

/// The parts of one node a mutable walk holds open.
struct Frame<'a, K, V> {
    /// The node's arena index.
    at: u32,
    /// Its level above the leaves, which says which arena `at` indexes.
    level: usize,
    keys: &'a [K],
    /// The values of `keys[lo..hi]`, not yet handed out.
    vals: &'a mut [V],
    edges: &'a mut [u32],
    /// The front's place: past `keys[..lo]`, in or past child `lo`.
    lo: usize,
    /// The back's place: before `keys[hi..]`, in or past child `hi`.
    hi: usize,
    /// Whether some of its children lie outside the walk: they stay unopened,
    /// so a swap may still move one after the frame is used up.
    fenced: bool,
}

impl<'a, K, V> Frame<'a, K, V> {
    /// The frame of `opened`, `level` levels up, with the ends at `lo` and
    /// `hi`.
    fn open(level: usize, opened: Opening<'a, K, V>, lo: usize, hi: usize) -> Option<Self> {
        let (at, Leaf { keys, vals, .. }, edges) = opened;
        let keys: &'a ArrayVec<K, CAP> = keys;
        Some(Self {
            at,
            level,
            keys: keys.as_slice(),
            vals: vals.as_mut_slice().get_mut(lo..hi)?,
            edges,
            lo,
            hi,
            fenced: lo > 0 || hi < keys.len(),
        })
    }

    /// Whether this is the frame of the interior node at arena index `at`.
    fn is_interior(&self, at: u32) -> bool {
        self.level > 0 && self.at == at
    }
}

/// The children of a used-up frame some of which lie outside the walk, under
/// the node's arena index and level.
struct Retired<'a> {
    at: u32,
    level: usize,
    edges: &'a mut [u32],
}

/// A mutable walk: the nodes it holds open, and the stretch of each arena it
/// has not opened, from either end of which it opens the next.
struct Opened<'a, K, V> {
    /// The unopened leaves.
    leaves: Block<'a, Leaf<K, V>>,
    /// The unopened interior nodes.
    interiors: Block<'a, Interior<K, V>>,
    /// The root's level.
    top: usize,
    /// The frames on both ends' paths, root first, then the front's own, top
    /// first, then the back's own, top first.
    frames: ArrayVec<Frame<'a, K, V>, PATHS>,
    shared: usize,
    front_own: usize,
    /// The children of used-up fenced frames: only the two ends' first paths
    /// are fenced, so they fit.
    retired: ArrayVec<Retired<'a>, PATHS>,
}

impl<'a, K, V> Opened<'a, K, V> {
    fn over(
        leaves: Block<'a, Leaf<K, V>>,
        interiors: Block<'a, Interior<K, V>>,
        top: usize,
    ) -> Self {
        Self {
            leaves,
            interiors,
            top,
            frames: ArrayVec::new(),
            shared: 0,
            front_own: 0,
            retired: ArrayVec::new(),
        }
    }

    /// Let go of a used-up own frame, keeping a fenced interior node's
    /// children reachable for the swaps that may still move them.
    fn retire(&mut self, frame: Option<Frame<'a, K, V>>) {
        let Some(frame) = frame.filter(|frame| frame.fenced && frame.level > 0) else {
            return;
        };
        let held = Retired {
            at: frame.at,
            level: frame.level,
            edges: frame.edges,
        };
        if self.retired.try_push(held).is_err() {
            // Past the bound a swap could orphan a child: end the walk instead.
            self.frames.clear();
            self.shared = 0;
            self.front_own = 0;
        }
    }

    /// A walk over nothing.
    fn empty() -> Self {
        Self::over(Block::empty(), Block::empty(), 0)
    }

    /// The walk of `map` from gap `front` to gap `back`, which is not before
    /// it, with the paths down to both opened.
    fn new(map: &'a mut OrderedMap<K, V>, front: Gap, back: Gap) -> Self {
        let paths = map.path_to(front).zip(map.path_to(back));
        let (Some((down, up)), Some(top)) = (paths, map.height.checked_sub(1)) else {
            return Self::empty();
        };
        let OrderedMap {
            leaves,
            interiors,
            root,
            ..
        } = map;
        let mut walk = Self::over(leaves.nodes.block(), interiors.nodes.block(), top);
        match walk.open_paths(root, &down, &up) {
            Some(()) => walk,
            None => Self::empty(),
        }
    }

    /// Open the root, the stretch of path both ends share, and each end's own
    /// path below it.
    fn open_paths(&mut self, root: &mut u32, down: &[usize], up: &[usize]) -> Option<()> {
        let mut opened = self.open(*root, self.top, NIL, End::Low)?;
        *root = opened.0;
        let mut depth = 0;
        loop {
            let level = self.top.checked_sub(depth)?;
            let (lo, hi) = (*down.get(depth)?, *up.get(depth)?);
            let frame = Frame::open(level, opened, lo, hi)?;
            let child = if lo == hi {
                frame.edges.get(lo).copied()
            } else {
                None
            };
            let parent = frame.at;
            self.frames.try_push(frame).ok()?;
            self.shared += 1;
            let Some(child) = child else {
                break;
            };
            opened = self.open(child, level.checked_sub(1)?, parent, End::Low)?;
            depth += 1;
        }
        self.open_front_path(down, depth)?;
        self.open_back_path(up, depth)
    }

    /// Open the front's own path, from the child it is in below the deepest
    /// shared frame down to its leaf.
    fn open_front_path(&mut self, down: &[usize], mut depth: usize) -> Option<()> {
        let deepest = self.frames.get(self.shared.checked_sub(1)?)?;
        let (mut parent, mut level) = (deepest.at, deepest.level);
        let mut child = deepest.edges.get(deepest.lo).copied();
        while let Some(edge) = child {
            depth += 1;
            level = level.checked_sub(1)?;
            let opened = self.open(edge, level, parent, End::Low)?;
            let (lo, hi) = (*down.get(depth)?, opened.1.keys.len());
            let frame = Frame::open(level, opened, lo, hi)?;
            (parent, child) = (frame.at, frame.edges.get(lo).copied());
            self.frames
                .try_insert(self.shared + self.front_own, frame)
                .ok()?;
            self.front_own += 1;
        }
        Some(())
    }

    /// Open the back's own path, from the child it is in below the deepest
    /// shared frame down to its leaf.
    fn open_back_path(&mut self, up: &[usize], mut depth: usize) -> Option<()> {
        let deepest = self.frames.get(self.shared.checked_sub(1)?)?;
        let (mut parent, mut level) = (deepest.at, deepest.level);
        let mut child = deepest.edges.get(deepest.hi).copied();
        while let Some(edge) = child {
            depth += 1;
            level = level.checked_sub(1)?;
            let opened = self.open(edge, level, parent, End::High)?;
            let hi = *up.get(depth)?;
            let frame = Frame::open(level, opened, 0, hi)?;
            (parent, child) = (frame.at, frame.edges.get(hi).copied());
            self.frames.try_push(frame).ok()?;
        }
        Some(())
    }

    fn back_own(&self) -> usize {
        self.frames
            .len()
            .saturating_sub(self.shared + self.front_own)
    }

    /// The frame the front stands in: its own deepest, else the deepest
    /// shared one.
    fn front_bottom(&self) -> Option<usize> {
        (self.shared + self.front_own).checked_sub(1)
    }

    /// The frame the back stands in: its own deepest, else the deepest
    /// shared one.
    fn back_bottom(&self) -> Option<usize> {
        if self.back_own() > 0 {
            self.frames.len().checked_sub(1)
        } else {
            self.shared.checked_sub(1)
        }
    }

    /// The next entry from the front.
    fn next_front(&mut self) -> Option<(&'a K, &'a mut V)> {
        loop {
            let bottom = self.front_bottom()?;
            let own = bottom >= self.shared;
            let frame = self.frames.get_mut(bottom)?;
            if frame.lo < frame.hi {
                let keys: &'a [K] = frame.keys;
                let key = keys.get(frame.lo)?;
                let (value, rest) = mem::take(&mut frame.vals).split_first_mut()?;
                frame.vals = rest;
                frame.lo += 1;
                if !frame.edges.is_empty() {
                    self.descend_front();
                }
                return Some((key, value));
            }
            if !own {
                return None;
            }
            let used = self.frames.remove(bottom);
            self.front_own -= 1;
            self.retire(used);
        }
    }

    /// The next entry from the back.
    fn next_back(&mut self) -> Option<(&'a K, &'a mut V)> {
        loop {
            let bottom = self.back_bottom()?;
            let own = bottom >= self.shared + self.front_own;
            let frame = self.frames.get_mut(bottom)?;
            if frame.lo < frame.hi {
                frame.hi -= 1;
                let keys: &'a [K] = frame.keys;
                let key = keys.get(frame.hi)?;
                let (value, rest) = mem::take(&mut frame.vals).split_last_mut()?;
                frame.vals = rest;
                if !frame.edges.is_empty() {
                    self.descend_back();
                }
                return Some((key, value));
            }
            if !own {
                return None;
            }
            let used = self.frames.pop();
            self.retire(used);
        }
    }

    /// Take the front into the child at its place in the frame it stands in,
    /// and on down that subtree's first children to a leaf.
    fn descend_front(&mut self) {
        while let Some(bottom) = self.front_bottom() {
            let Some(frame) = self.frames.get(bottom) else {
                return;
            };
            let Some(&child) = frame.edges.get(frame.lo) else {
                return;
            };
            let Some(level) = frame.level.checked_sub(1) else {
                return;
            };
            let parent = frame.at;
            if bottom + 1 == self.shared && frame.lo == frame.hi {
                // The child is the back's: the ends share it now, unless the
                // back has passed through it and nothing lies between them.
                if self.back_own() == 0 {
                    return;
                }
                self.shared += 1;
                continue;
            }
            let Some(opened) = self.open(child, level, parent, End::Low) else {
                return;
            };
            let hi = opened.1.keys.len();
            let Some(frame) = Frame::open(level, opened, 0, hi) else {
                return;
            };
            if self
                .frames
                .try_insert(self.shared + self.front_own, frame)
                .is_err()
            {
                return;
            }
            self.front_own += 1;
        }
    }

    /// Take the back into the child at its place in the frame it stands in,
    /// and on down that subtree's last children to a leaf.
    fn descend_back(&mut self) {
        while let Some(bottom) = self.back_bottom() {
            let Some(frame) = self.frames.get(bottom) else {
                return;
            };
            let Some(&child) = frame.edges.get(frame.hi) else {
                return;
            };
            let Some(level) = frame.level.checked_sub(1) else {
                return;
            };
            let parent = frame.at;
            if bottom + 1 == self.shared && frame.lo == frame.hi {
                // The child is the front's, shared now unless the front has
                // passed through it.
                if self.front_own == 0 {
                    return;
                }
                self.shared += 1;
                self.front_own -= 1;
                continue;
            }
            let Some(opened) = self.open(child, level, parent, End::High) else {
                return;
            };
            let hi = opened.1.keys.len();
            let Some(frame) = Frame::open(level, opened, 0, hi) else {
                return;
            };
            if self.frames.try_push(frame).is_err() {
                return;
            }
        }
    }

    /// Open `child`, `level` levels up and a child of the opened node
    /// `parent` (`NIL` for the root), from `end` of its arena's stretch.
    fn open(
        &mut self,
        child: u32,
        level: usize,
        parent: u32,
        end: End,
    ) -> Option<Opening<'a, K, V>> {
        let at = if level == 0 {
            end_of(&mut self.leaves, end)?
        } else {
            end_of(&mut self.interiors, end)?
        };
        if child != at {
            self.swap_in(child, at, level, parent)?;
        }
        if level == 0 {
            return Some((at, take_end(&mut self.leaves, end)?, Default::default()));
        }
        let Interior { node, edges } = take_end(&mut self.interiors, end)?;
        Some((at, node, edges.as_mut_slice()))
    }

    /// Swap the unopened node at `opening`, `level` levels up and a child of
    /// `parent`, with the unopened node at `end` of the same arena,
    /// re-pointing whatever named either.
    fn swap_in(&mut self, opening: u32, end: u32, level: usize, parent: u32) -> Option<()> {
        let in_leaves = level == 0;
        let displaced_parent = *self.parent_of(end, level)?;
        // Read while the displaced node's ancestors still name it.
        let displaced_level = if in_leaves { 0 } else { self.level_of(end)? };
        let opening_place = self.place_of(parent, opening);
        let displaced_place = self.place_of(displaced_parent, end);
        if in_leaves {
            swap_unopened(&mut self.leaves, opening, end)?;
        } else {
            swap_unopened(&mut self.interiors, opening, end)?;
        }
        if let Some(at) = opening_place {
            self.point(parent, at, end);
        }
        // A displaced interior node may be a child of the one that moved.
        let displaced_parent = if !in_leaves && displaced_parent == opening {
            end
        } else {
            displaced_parent
        };
        if let Some(at) = displaced_place {
            self.point(displaced_parent, at, opening);
        }
        if !in_leaves {
            self.adopt_local(end, level);
            self.adopt_local(opening, displaced_level);
        }
        Some(())
    }

    /// The level of the unopened interior node at `at`: one below its
    /// parent's, counted from its nearest opened ancestor or the root.
    fn level_of(&self, at: u32) -> Option<usize> {
        let (mut node, mut below) = (at, 0);
        while let Some(local) = unopened(&self.interiors, node) {
            let parent = self.interiors.get(local)?.node.parent;
            if parent == NIL {
                return self.top.checked_sub(below);
            }
            (node, below) = (parent, below + 1);
        }
        let level = match self.frames.iter().find(|frame| frame.is_interior(node)) {
            Some(frame) => frame.level,
            None => self.retired.iter().find(|held| held.at == node)?.level,
        };
        level.checked_sub(below)
    }

    /// The parent link of the unopened node at arena index `at`, `level`
    /// levels up.
    fn parent_of(&mut self, at: u32, level: usize) -> Option<&mut u32> {
        if level == 0 {
            let local = unopened(&self.leaves, at)?;
            Some(&mut self.leaves.get_mut(local)?.parent)
        } else {
            let local = unopened(&self.interiors, at)?;
            Some(&mut self.interiors.get_mut(local)?.node.parent)
        }
    }

    /// Set child `at` of the interior node at arena index `node` to `child`.
    fn point(&mut self, node: u32, at: usize, child: u32) {
        if let Some(edge) = self.edges_of(node).and_then(|edges| edges.get_mut(at)) {
            *edge = child;
        }
    }

    /// The children of the interior node at arena index `at`, opened or not.
    fn edges_of(&mut self, at: u32) -> Option<&mut [u32]> {
        if let Some(local) = unopened(&self.interiors, at) {
            return self
                .interiors
                .get_mut(local)
                .map(|held| held.edges.as_mut_slice());
        }
        let framed = self.frames.iter_mut().find(|frame| frame.is_interior(at));
        if let Some(frame) = framed {
            return Some(&mut *frame.edges);
        }
        self.retired
            .iter_mut()
            .find(|held| held.at == at)
            .map(|held| &mut *held.edges)
    }

    /// Where `child` sits among the children of `parent`, opened or not.
    fn place_of(&mut self, parent: u32, child: u32) -> Option<usize> {
        self.edges_of(parent)?
            .iter()
            .position(|&edge| edge == child)
    }

    /// Make every child of the unopened interior node at `at`, `level` levels
    /// up, name it as parent: an unopened node's children are all unopened.
    fn adopt_local(&mut self, at: u32, level: usize) {
        let Some(below) = level.checked_sub(1) else {
            return;
        };
        let Some(local) = unopened(&self.interiors, at) else {
            return;
        };
        let Some(edges) = self.interiors.get(local).map(|held| held.edges.clone()) else {
            return;
        };
        for &child in &edges {
            if let Some(parent) = self.parent_of(child, below) {
                *parent = at;
            }
        }
    }
}

/// An end of the stretch of an arena a mutable walk has not opened.
#[derive(Copy, Clone)]
enum End {
    Low,
    High,
}

/// Where arena index `at` lies in the unopened stretch `block`, if it does.
fn unopened<N>(block: &Block<'_, N>, at: u32) -> Option<usize> {
    (at as usize)
        .checked_sub(block.base())
        .filter(|&local| local < block.len())
}

/// Swap the unopened nodes at arena indices `first` and `second` of `block`.
fn swap_unopened<N>(block: &mut Block<'_, N>, first: u32, second: u32) -> Option<()> {
    let (from, to) = (unopened(block, first)?, unopened(block, second)?);
    block.swap(from, to);
    Some(())
}

/// Drop the freed nodes off `end` of `block`, answering the arena index of
/// the node left there.
fn end_of<N: Linked>(block: &mut Block<'_, N>, end: End) -> Option<u32> {
    match end {
        End::Low => {
            while block.first().is_some_and(|node| node.link() != NIL) {
                block.take_first()?;
            }
            u32::try_from(block.base()).ok()
        }
        End::High => {
            while block.last().is_some_and(|node| node.link() != NIL) {
                block.take_last()?;
            }
            u32::try_from(block.base() + block.len().checked_sub(1)?).ok()
        }
    }
}

/// Split the node at `end` off `block`, for the walk to hold.
fn take_end<'a, N>(block: &mut Block<'a, N>, end: End) -> Option<&'a mut N> {
    match end {
        End::Low => block.take_first(),
        End::High => block.take_last(),
    }
}

#[cfg(test)]
#[path = "ordered_tests.rs"]
mod tests;
