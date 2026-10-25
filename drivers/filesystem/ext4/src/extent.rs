//! The ext4 extent tree: lookup, insertion with neighbour merging, node
//! splits and depth growth, tail truncation, and unwritten-extent conversion.
//!
//! The tree is rooted in the inode's 60-byte `i_block` (a header and four
//! entries) and grows through blocks, each a header, `eh_max` entries, and on
//! a `metadata_csum` volume a checksum tail. Index entries are keyed by the
//! first logical block their subtree covers; leaf extents are sorted,
//! disjoint, and at most 32 768 blocks long (32 767 when unwritten).
//!
//! Block I/O, allocation and checksums belong to the filesystem, reached
//! through [`NodeStore`], so the algorithms are tested here over a mock.

use alloc::vec::Vec;

use tairix_abi::DriverError;

use crate::bytes::{
    high32, le16, le32, low16, low32, put_le16 as put16, put_le32 as put32, scratch, u16_of,
};

/// Magic every extent-tree node header starts with.
pub(crate) const EXTENT_MAGIC: u16 = 0xF30A;
/// Bytes of a node header, and of every entry.
const HEADER: usize = 12;
const ENTRY: usize = 12;
/// Bytes of the inline root in `i_block`.
pub(crate) const ROOT_LEN: usize = 60;
/// Entries the inline root holds.
pub(crate) const ROOT_CAPACITY: usize = (ROOT_LEN - HEADER) / ENTRY;
/// The deepest tree the format allows.
const MAX_DEPTH: u16 = 5;
/// The logical blocks an extent tree maps, exclusive: its 32-bit space less
/// the last block, so an extent's end (`ee_block + ee_len`) fits 32 bits.
pub(crate) const LOGICAL_LIMIT: u64 = u32::MAX as u64;
/// Longest initialised extent; an `ee_len` above it marks an unwritten one.
const INIT_MAX: u32 = 32_768;
/// Longest unwritten extent.
const UNWRITTEN_MAX: u32 = INIT_MAX - 1;

/// What a logical block resolves to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Mapping {
    /// `len` blocks from `phys` back logical blocks from the one asked about.
    /// An unwritten run reads as zeros.
    Mapped {
        phys: u64,
        len: u64,
        unwritten: bool,
    },
    /// Nothing maps the next `len` blocks; `None` when nothing maps any block
    /// from here on.
    Hole { len: Option<u64> },
}

/// One extent: `len` logical blocks from `logical` stored from `phys`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Extent {
    pub(crate) logical: u32,
    pub(crate) phys: u64,
    pub(crate) len: u32,
    pub(crate) unwritten: bool,
}

impl Extent {
    fn end(&self) -> u64 {
        u64::from(self.logical) + u64::from(self.len)
    }

    fn max_len(&self) -> u32 {
        if self.unwritten {
            UNWRITTEN_MAX
        } else {
            INIT_MAX
        }
    }
}

/// The filesystem services the tree needs.
pub(crate) trait NodeStore {
    /// Bytes of a non-root node block.
    fn node_bytes(&self) -> usize;
    /// Entries a non-root node may hold (after any checksum tail).
    fn node_capacity(&self) -> usize;
    /// Read node `block`, verifying its checksum tail.
    fn read_node(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError>;
    /// Write node `block`, stamping its checksum tail.
    fn write_node(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError>;
    /// Allocate one node block near `goal`.
    fn alloc_node(&mut self, goal: u64) -> Result<u64, DriverError>;
    /// Release a node block the tree no longer names.
    fn free_node(&mut self, block: u64);
    /// Release `len` data blocks from `start` the tree no longer maps.
    fn free_data(&mut self, start: u64, len: u64);
}

fn count(buf: &[u8]) -> usize {
    usize::from(le16(buf, 2))
}

fn set_count(buf: &mut [u8], n: usize) -> Result<(), DriverError> {
    put16(
        buf,
        2,
        u16::try_from(n).map_err(|_| DriverError::DeviceFault)?,
    );
    Ok(())
}

fn max_entries(buf: &[u8]) -> usize {
    usize::from(le16(buf, 4))
}

fn depth(buf: &[u8]) -> u16 {
    le16(buf, 6)
}

fn at(i: usize) -> usize {
    HEADER + i * ENTRY
}

fn leaf_entry(buf: &[u8], i: usize) -> Extent {
    let off = at(i);
    let raw = le16(buf, off + 4);
    let (len, unwritten) = if u32::from(raw) > INIT_MAX {
        (u32::from(raw) - INIT_MAX, true)
    } else {
        (u32::from(raw), false)
    };
    Extent {
        logical: le32(buf, off),
        phys: (u64::from(le16(buf, off + 6)) << 32) | u64::from(le32(buf, off + 8)),
        len,
        unwritten,
    }
}

fn put_leaf_entry(buf: &mut [u8], i: usize, e: Extent) -> Result<(), DriverError> {
    if e.len == 0 || e.len > e.max_len() || e.phys >> 48 != 0 {
        return Err(DriverError::DeviceFault);
    }
    let off = at(i);
    put32(buf, off, e.logical);
    let raw = if e.unwritten { e.len + INIT_MAX } else { e.len };
    put16(
        buf,
        off + 4,
        u16::try_from(raw).map_err(|_| DriverError::DeviceFault)?,
    );
    put16(buf, off + 6, low16(high32(e.phys)));
    put32(buf, off + 8, low32(e.phys));
    Ok(())
}

fn index_key(buf: &[u8], i: usize) -> u32 {
    le32(buf, at(i))
}

fn index_child(buf: &[u8], i: usize) -> u64 {
    let off = at(i);
    (u64::from(le16(buf, off + 8)) << 32) | u64::from(le32(buf, off + 4))
}

fn put_index_entry(buf: &mut [u8], i: usize, key: u32, child: u64) -> Result<(), DriverError> {
    if child >> 48 != 0 {
        return Err(DriverError::DeviceFault);
    }
    let off = at(i);
    put32(buf, off, key);
    put32(buf, off + 4, low32(child));
    put16(buf, off + 8, low16(high32(child)));
    put16(buf, off + 10, 0);
    Ok(())
}

/// The first logical block a node's entries name, or `None` when empty.
fn first_key(buf: &[u8]) -> Option<u32> {
    (count(buf) > 0).then(|| le32(buf, at(0)))
}

/// Open up entry slot `i`, moving entries `i..` one slot right.
fn open_slot(buf: &mut [u8], i: usize) -> Result<(), DriverError> {
    let n = count(buf);
    if n >= max_entries(buf) || i > n {
        return Err(DriverError::DeviceFault);
    }
    buf.copy_within(at(i)..at(n), at(i + 1));
    set_count(buf, n + 1)
}

/// Close entry slot `i`, moving entries `i + 1..` one slot left.
fn close_slot(buf: &mut [u8], i: usize) -> Result<(), DriverError> {
    let n = count(buf);
    if i >= n {
        return Err(DriverError::DeviceFault);
    }
    buf.copy_within(at(i + 1)..at(n), at(i));
    buf[at(n - 1)..at(n)].fill(0);
    set_count(buf, n - 1)
}

/// Check a node's header against where it sits: the magic, a count within
/// its maximum, a maximum within what its space holds and above zero, an
/// index naming at least one child, and the depth its parent expects.
fn check_node(buf: &[u8], capacity: usize, want_depth: Option<u16>) -> Result<(), DriverError> {
    let sound = buf.len() >= HEADER
        && le16(buf, 0) == EXTENT_MAGIC
        && max_entries(buf) > 0
        && (depth(buf) == 0 || count(buf) > 0)
        && max_entries(buf) <= capacity
        && HEADER + max_entries(buf) * ENTRY <= buf.len()
        && count(buf) <= max_entries(buf)
        && depth(buf) <= MAX_DEPTH
        && want_depth.is_none_or(|want| depth(buf) == want);
    if sound {
        Ok(())
    } else {
        Err(DriverError::DeviceFault)
    }
}

/// An empty depth-0 root, as a new inode's `i_block` holds it.
pub(crate) fn init_root(root: &mut [u8]) -> Result<(), DriverError> {
    let root = root
        .get_mut(..ROOT_LEN)
        .ok_or(DriverError::BufferTooSmall)?;
    root.fill(0);
    put16(root, 0, EXTENT_MAGIC);
    put16(root, 4, u16_of(ROOT_CAPACITY)?);
    Ok(())
}

fn check_root(root: &[u8]) -> Result<(), DriverError> {
    check_node(root, ROOT_CAPACITY, None)
}

/// The inline root, copied into a buffer with room for `capacity` bytes, so
/// a walk can reuse it for the nodes below without reallocating.
fn root_copy(root: &[u8], capacity: usize) -> Result<Vec<u8>, DriverError> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(capacity.max(ROOT_LEN))
        .map_err(|_| DriverError::NoSpace)?;
    buf.extend_from_slice(root.get(..ROOT_LEN).ok_or(DriverError::DeviceFault)?);
    Ok(buf)
}

/// Index of the last entry whose key is at or below `logical`.
fn last_at_or_below(buf: &[u8], logical: u32) -> Option<usize> {
    let (mut lo, mut hi) = (0usize, count(buf));
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if le32(buf, at(mid)) <= logical {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo.checked_sub(1)
}

/// Resolve logical block `logical`.
pub(crate) fn lookup<S: NodeStore>(
    root: &[u8],
    store: &mut S,
    logical: u32,
) -> Result<Mapping, DriverError> {
    check_root(root)?;
    let mut node = root_copy(root, store.node_bytes())?;
    // The first logical block past this node's range, from the parent's
    // next key; `None` when nothing bounds it.
    let mut bound: Option<u64> = None;
    loop {
        if depth(&node) == 0 {
            let at_or_below = last_at_or_below(&node, logical);
            if let Some(i) = at_or_below {
                let e = leaf_entry(&node, i);
                if u64::from(logical) < e.end() {
                    let into = u64::from(logical - e.logical);
                    return Ok(Mapping::Mapped {
                        phys: e.phys + into,
                        len: u64::from(e.len) - into,
                        unwritten: e.unwritten,
                    });
                }
            }
            let next = at_or_below.map_or(0, |i| i + 1);
            let next_start = if next < count(&node) {
                Some(u64::from(leaf_entry(&node, next).logical))
            } else {
                bound
            };
            return Ok(Mapping::Hole {
                len: next_start.map(|start| start - u64::from(logical)),
            });
        }
        let child_depth = depth(&node) - 1;
        let Some(i) = last_at_or_below(&node, logical) else {
            let first = first_key(&node).map(u64::from);
            return Ok(Mapping::Hole {
                len: first.or(bound).map(|start| start - u64::from(logical)),
            });
        };
        if i + 1 < count(&node) {
            bound = Some(u64::from(index_key(&node, i + 1)));
        }
        let child = index_child(&node, i);
        node.clear();
        node.resize(store.node_bytes(), 0);
        store.read_node(child, &mut node)?;
        check_node(&node, store.node_capacity(), Some(child_depth))?;
    }
}

/// One node on the path from the root to a leaf.
struct Level {
    /// `None` for the inline root.
    block: Option<u64>,
    buf: Vec<u8>,
    /// The entry the path took through this node.
    pos: usize,
    /// Whether this node changed and must be written back.
    dirty: bool,
}

/// The path to the leaf that holds, or would hold, `logical`.
fn descend<S: NodeStore>(
    root: &[u8],
    store: &mut S,
    logical: u32,
) -> Result<Vec<Level>, DriverError> {
    check_root(root)?;
    let mut path = Vec::new();
    path.try_reserve_exact(usize::from(MAX_DEPTH) + 1)
        .map_err(|_| DriverError::NoSpace)?;
    let mut buf = root_copy(root, ROOT_LEN)?;
    let mut block = None;
    loop {
        let pos = last_at_or_below(&buf, logical).unwrap_or(0);
        let leaf = depth(&buf) == 0;
        let child = (!leaf && count(&buf) > 0).then(|| (index_child(&buf, pos), depth(&buf) - 1));
        path.push(Level {
            block,
            buf,
            pos,
            dirty: false,
        });
        let Some((next, child_depth)) = child else {
            return Ok(path);
        };
        let mut node = scratch(store.node_bytes())?;
        store.read_node(next, &mut node)?;
        check_node(&node, store.node_capacity(), Some(child_depth))?;
        buf = node;
        block = Some(next);
    }
}

/// Write back every changed node of `path`, the root into `root`.
fn write_back<S: NodeStore>(
    root: &mut [u8],
    store: &mut S,
    path: &mut [Level],
) -> Result<(), DriverError> {
    for level in path.iter_mut().rev().filter(|level| level.dirty) {
        match level.block {
            Some(block) => store.write_node(block, &mut level.buf)?,
            None => root[..ROOT_LEN].copy_from_slice(&level.buf[..ROOT_LEN]),
        }
        level.dirty = false;
    }
    Ok(())
}

/// Lower the keys on the path above `from` that must cover a node whose
/// first key became `key`.
fn fix_keys(path: &mut [Level], from: usize, key: u32) -> Result<(), DriverError> {
    for k in (0..from).rev() {
        let pos = path[k].pos;
        if index_key(&path[k].buf, pos) == key {
            break;
        }
        let child = index_child(&path[k].buf, pos);
        put_index_entry(&mut path[k].buf, pos, key, child)?;
        path[k].dirty = true;
        if pos != 0 {
            break;
        }
    }
    Ok(())
}

/// Map `e`, whose logical range nothing currently maps, merging it into an
/// adjacent extent in its leaf when it continues one.
pub(crate) fn insert<S: NodeStore>(
    root: &mut [u8],
    store: &mut S,
    e: Extent,
) -> Result<(), DriverError> {
    if e.len == 0 || e.len > e.max_len() || e.end() > LOGICAL_LIMIT {
        return Err(DriverError::LengthOutOfRange);
    }
    loop {
        let mut path = descend(root, store, e.logical)?;
        let leaf = path.len() - 1;
        if try_merge(root, store, &mut path, e)? {
            return Ok(());
        }
        let buf = &path[leaf].buf;
        if count(buf) < max_entries(buf) {
            let pos = match last_at_or_below(buf, e.logical) {
                Some(i) => i + 1,
                None => 0,
            };
            open_slot(&mut path[leaf].buf, pos)?;
            put_leaf_entry(&mut path[leaf].buf, pos, e)?;
            path[leaf].dirty = true;
            if pos == 0 {
                fix_keys(&mut path, leaf, e.logical)?;
            }
            return write_back(root, store, &mut path);
        }
        make_room(root, store, &mut path, leaf, e.logical)?;
    }
}

/// Extend the leaf neighbour `e` continues, or that continues `e`.
fn try_merge<S: NodeStore>(
    root: &mut [u8],
    store: &mut S,
    path: &mut [Level],
    e: Extent,
) -> Result<bool, DriverError> {
    let leaf = path.len() - 1;
    let n = count(&path[leaf].buf);
    let left = last_at_or_below(&path[leaf].buf, e.logical);
    if let Some(i) = left {
        let mut l = leaf_entry(&path[leaf].buf, i);
        if l.end() == u64::from(e.logical)
            && l.phys + u64::from(l.len) == e.phys
            && l.unwritten == e.unwritten
            && l.len + e.len <= l.max_len()
        {
            l.len += e.len;
            if i + 1 < n {
                let r = leaf_entry(&path[leaf].buf, i + 1);
                if u64::from(r.logical) == l.end()
                    && l.phys + u64::from(l.len) == r.phys
                    && r.unwritten == l.unwritten
                    && l.len + r.len <= l.max_len()
                {
                    l.len += r.len;
                    close_slot(&mut path[leaf].buf, i + 1)?;
                }
            }
            put_leaf_entry(&mut path[leaf].buf, i, l)?;
            path[leaf].dirty = true;
            write_back(root, store, path)?;
            return Ok(true);
        }
    }
    let right = left.map_or(0, |i| i + 1);
    if right < n {
        let mut r = leaf_entry(&path[leaf].buf, right);
        if e.end() == u64::from(r.logical)
            && e.phys + u64::from(e.len) == r.phys
            && r.unwritten == e.unwritten
            && r.len + e.len <= r.max_len()
        {
            r.logical = e.logical;
            r.phys = e.phys;
            r.len += e.len;
            put_leaf_entry(&mut path[leaf].buf, right, r)?;
            path[leaf].dirty = true;
            if right == 0 {
                fix_keys(path, leaf, e.logical)?;
            }
            write_back(root, store, path)?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Make room in the full node at `level` of `path`: split it into its
/// parent, making room in the parent first, or grow the tree a level when it
/// is the root. The caller re-descends afterwards.
fn make_room<S: NodeStore>(
    root: &mut [u8],
    store: &mut S,
    path: &mut Vec<Level>,
    level: usize,
    logical: u32,
) -> Result<(), DriverError> {
    if level == 0 {
        return grow(root, store, path);
    }
    let parent = level - 1;
    if count(&path[parent].buf) >= max_entries(&path[parent].buf) {
        return make_room(root, store, path, parent, logical);
    }
    let goal = path[level].block.unwrap_or(0);
    let fresh = store.alloc_node(goal)?;
    let n = count(&path[level].buf);
    // A node filled by appends keeps all but its last entry, so a sequential
    // writer's nodes stay full rather than half empty.
    let cut = if last_at_or_below(&path[level].buf, logical) == Some(n - 1) {
        n - 1
    } else {
        n / 2
    };
    let mut other = scratch(store.node_bytes())?;
    put16(&mut other, 0, EXTENT_MAGIC);
    put16(
        &mut other,
        4,
        u16::try_from(store.node_capacity()).map_err(|_| DriverError::DeviceFault)?,
    );
    put16(&mut other, 6, depth(&path[level].buf));
    let moved = n - cut;
    other[at(0)..at(moved)].copy_from_slice(&path[level].buf[at(cut)..at(n)]);
    set_count(&mut other, moved)?;
    path[level].buf[at(cut)..at(n)].fill(0);
    set_count(&mut path[level].buf, cut)?;
    let key = first_key(&other).ok_or(DriverError::DeviceFault)?;
    let slot = path[parent].pos + 1;
    open_slot(&mut path[parent].buf, slot)?;
    put_index_entry(&mut path[parent].buf, slot, key, fresh)?;
    path[parent].dirty = true;
    path[level].dirty = true;
    store.write_node(fresh, &mut other)?;
    write_back(root, store, path)
}

/// Push the full root's entries down into a new block, leaving the root an
/// index of that one child.
fn grow<S: NodeStore>(
    root: &mut [u8],
    store: &mut S,
    path: &mut [Level],
) -> Result<(), DriverError> {
    let old = &path[0].buf;
    let d = depth(old);
    if d >= MAX_DEPTH {
        return Err(DriverError::NoSpace);
    }
    let n = count(old);
    if store.node_capacity() < ROOT_CAPACITY || HEADER + ROOT_CAPACITY * ENTRY > store.node_bytes()
    {
        return Err(DriverError::DeviceFault);
    }
    let goal = match (d, n) {
        (_, 0) => 0,
        (0, _) => leaf_entry(old, 0).phys,
        _ => index_child(old, 0),
    };
    let fresh = store.alloc_node(goal)?;
    let mut child = scratch(store.node_bytes())?;
    put16(&mut child, 0, EXTENT_MAGIC);
    put16(
        &mut child,
        4,
        u16::try_from(store.node_capacity()).map_err(|_| DriverError::DeviceFault)?,
    );
    put16(&mut child, 6, d);
    child[at(0)..at(n)].copy_from_slice(&old[at(0)..at(n)]);
    set_count(&mut child, n)?;
    let key = first_key(&child).unwrap_or(0);
    store.write_node(fresh, &mut child)?;
    let mut new_root = [0u8; ROOT_LEN];
    put16(&mut new_root, 0, EXTENT_MAGIC);
    put16(&mut new_root, 4, u16_of(ROOT_CAPACITY)?);
    put16(&mut new_root, 6, d + 1);
    set_count(&mut new_root, 1)?;
    put_index_entry(&mut new_root, 0, key, fresh)?;
    root[..ROOT_LEN].copy_from_slice(&new_root);
    Ok(())
}

/// What one bounded truncation pass did.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Truncated {
    /// Data blocks released.
    pub(crate) data: u64,
    /// Tree blocks released.
    pub(crate) nodes: u64,
    /// Whether nothing at or past the cut remains mapped.
    pub(crate) done: bool,
}

/// Release every mapping at or past logical block `from`, working from the
/// end of the file and stopping after `budget` extents or nodes, so a huge
/// truncation spans transactions. Each pass leaves a sound tree that maps
/// a prefix of what it mapped before.
pub(crate) fn truncate<S: NodeStore>(
    root: &mut [u8],
    store: &mut S,
    from: u64,
    budget: u32,
) -> Result<Truncated, DriverError> {
    check_root(root)?;
    let mut out = Truncated::default();
    let mut left = budget.max(1);
    let mut node = root_copy(root, ROOT_LEN)?;
    let finished = trim(store, &mut node, from, &mut left, &mut out)?;
    if count(&node) == 0 {
        init_root(&mut node)?;
    }
    root[..ROOT_LEN].copy_from_slice(&node[..ROOT_LEN]);
    out.done = finished;
    Ok(out)
}

/// Trim one node to `from`; answers whether nothing at or past it remains.
fn trim<S: NodeStore>(
    store: &mut S,
    buf: &mut [u8],
    from: u64,
    left: &mut u32,
    out: &mut Truncated,
) -> Result<bool, DriverError> {
    if depth(buf) == 0 {
        while let Some(i) = count(buf).checked_sub(1) {
            if *left == 0 {
                return Ok(false);
            }
            let mut e = leaf_entry(buf, i);
            if u64::from(e.logical) >= from {
                store.free_data(e.phys, u64::from(e.len));
                out.data += u64::from(e.len);
                close_slot(buf, i)?;
                *left -= 1;
            } else if e.end() > from {
                let keep = u32::try_from(from - u64::from(e.logical))
                    .map_err(|_| DriverError::DeviceFault)?;
                let gone = u64::from(e.len - keep);
                store.free_data(e.phys + u64::from(keep), gone);
                out.data += gone;
                e.len = keep;
                put_leaf_entry(buf, i, e)?;
                *left -= 1;
                return Ok(true);
            } else {
                return Ok(true);
            }
        }
        return Ok(true);
    }
    let child_depth = depth(buf) - 1;
    while let Some(i) = count(buf).checked_sub(1) {
        if *left == 0 {
            return Ok(false);
        }
        let key = u64::from(index_key(buf, i));
        let child = index_child(buf, i);
        let mut node = scratch(store.node_bytes())?;
        store.read_node(child, &mut node)?;
        check_node(&node, store.node_capacity(), Some(child_depth))?;
        let finished = trim(store, &mut node, from, left, out)?;
        if count(&node) == 0 {
            store.free_node(child);
            out.nodes += 1;
            close_slot(buf, i)?;
        } else {
            store.write_node(child, &mut node)?;
        }
        if !finished {
            return Ok(false);
        }
        if key < from {
            return Ok(true);
        }
    }
    Ok(true)
}

/// Mark `len` blocks from `logical`, which lie inside one unwritten extent,
/// written, splitting that extent around them.
pub(crate) fn mark_written<S: NodeStore>(
    root: &mut [u8],
    store: &mut S,
    logical: u32,
    len: u32,
) -> Result<(), DriverError> {
    let mut path = descend(root, store, logical)?;
    let leaf = path.len() - 1;
    let i = last_at_or_below(&path[leaf].buf, logical).ok_or(DriverError::DeviceFault)?;
    let e = leaf_entry(&path[leaf].buf, i);
    let end = u64::from(logical) + u64::from(len);
    if !e.unwritten || len == 0 || u64::from(logical) < u64::from(e.logical) || end > e.end() {
        return Err(DriverError::DeviceFault);
    }
    let head = logical - e.logical;
    let tail = u32::try_from(e.end() - end).map_err(|_| DriverError::DeviceFault)?;
    let written = Extent {
        logical,
        phys: e.phys + u64::from(head),
        len,
        unwritten: false,
    };
    // The entry keeps whichever piece starts where it does, so its slot and
    // the keys above it stand; the other pieces go in as fresh mappings.
    let kept = if head > 0 {
        Extent { len: head, ..e }
    } else {
        written
    };
    put_leaf_entry(&mut path[leaf].buf, i, kept)?;
    path[leaf].dirty = true;
    write_back(root, store, &mut path)?;
    if head > 0 {
        insert(root, store, written)?;
    }
    if tail > 0 {
        insert(
            root,
            store,
            Extent {
                logical: u32::try_from(end).map_err(|_| DriverError::DeviceFault)?,
                phys: written.phys + u64::from(len),
                len: tail,
                unwritten: true,
            },
        )?;
    }
    Ok(())
}

/// The physical block just past the last extent at or before `logical`,
/// the natural place to continue a file; `None` when nothing precedes it.
pub(crate) fn goal_after<S: NodeStore>(
    root: &[u8],
    store: &mut S,
    logical: u32,
) -> Result<Option<u64>, DriverError> {
    let path = descend(root, store, logical)?;
    let leaf = &path[path.len() - 1].buf;
    // The extent's logical-to-physical offset carries on, so a sparse file
    // keeps its blocks where a dense one would have them.
    Ok(last_at_or_below(leaf, logical).map(|i| {
        let e = leaf_entry(leaf, i);
        e.phys + u64::from(logical - e.logical)
    }))
}

#[cfg(test)]
#[path = "extent_tests.rs"]
mod tests;
