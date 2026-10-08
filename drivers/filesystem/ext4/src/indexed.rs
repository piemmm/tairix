//! Hash-indexed directories, read and written through their index: names
//! found by hash, listings in hash order, and new names placed in the leaf
//! their hash selects, with leaves split and the index grown as they fill.
//!
//! Records move between an indexed directory's blocks when a leaf is packed or
//! split, so a listing runs in `(hash, name)` order and resumes just past the
//! name it last returned, which no move disturbs. Its cursor is a keyed digest
//! of that name: the same place in every listing, and never `0`.
//!
//! A change writes each new block first, then the inode that maps it, then the
//! index naming it, and the block it came from last. Stopped part-way, an
//! entry is left in two leaves, never in none, and every reader here passes
//! over an entry its leaf's index range does not cover.

use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{DirEntry, DirVisit, NodeId};
use tairix_abi::DriverError;
use tairix_hash::{HashSeed, SipHash13};

use super::htree::{self, Entries, EntriesMut, Level, NameHash, CONTINUED};
use super::{
    align4, le32, put_le16, u16_of, Child, Ext4, Inode, Located, DIRENT_HEADER, INODE_FLAG_INDEX,
    MAX_BLOCK_SIZE, MAX_NAME_LEN,
};

/// Most live entries one leaf holds: no record is shorter than twelve bytes.
const MAX_LEAF_ENTRIES: usize = MAX_BLOCK_SIZE as usize / 12;

/// Most leaves one hash's run may span. Equal hashes filling more than a leaf
/// come only from a crafted volume, and every entry of a run costs a read of
/// all its leaves, so a longer run is refused rather than listed.
const MAX_RUN_LEAVES: usize = 16;

/// The bit every indexed listing's cursor carries, so none is `0`, the start.
const INDEXED_CURSOR: u64 = 1 << 63;

/// An indexed directory this driver walks: how its names hash, how many
/// interior levels its index has, its root block, how many blocks it holds,
/// and its blocks' checksum seed on a `metadata_csum` volume.
pub(super) struct Indexed {
    hash: NameHash,
    levels: u8,
    root: u64,
    blocks: u64,
    seed: Option<u32>,
}

/// How a directory is read and written.
pub(super) enum IndexUse {
    /// Every block is a leaf.
    Linear,
    /// Through its index.
    Indexed(Indexed),
    /// Through an index this driver cannot use, for the reason given: read
    /// linearly, and never given a new name.
    Unusable(DriverError),
}

/// A leaf an index leads to, with the hash of its index entry and of the
/// next one, between which its names' hashes lie.
#[derive(Copy, Clone)]
struct Leaf {
    logical: u32,
    lower: u32,
    upper: Option<u32>,
}

impl Leaf {
    /// Whether a name hashing to `hash` belongs here. Hashes are even, so an
    /// odd (continued) upper bound also admits its even value, whose run
    /// carries on into the next leaf.
    fn holds(&self, hash: u32) -> bool {
        hash >= self.lower & !CONTINUED && self.upper.is_none_or(|upper| hash < upper)
    }

    /// The hash whose run continues into the next leaf.
    fn continued(&self) -> Option<u32> {
        self.upper
            .filter(|upper| upper & CONTINUED != 0)
            .map(|upper| upper & !CONTINUED)
    }
}

/// An interior node a walk stands in: where it lives, the entry taken, and
/// the root's hashes either side of it.
#[derive(Copy, Clone)]
struct NodeAt {
    phys: u64,
    at: usize,
    lower: u32,
    upper: Option<u32>,
}

/// Where a walk over an indexed directory's leaves stands. The lowest index
/// block on its way — the root, or the node it stands in — is kept in the
/// buffer it is walked with.
#[derive(Copy, Clone)]
struct Walk {
    root_at: usize,
    root_full: bool,
    node: Option<NodeAt>,
}

impl Walk {
    /// The leaf the walk stands at, from `index`, the block it was walked
    /// with.
    fn leaf(&self, index: &[u8]) -> Leaf {
        match self.node {
            None => {
                let root = Entries::of(index, Level::Root);
                Leaf {
                    logical: root.child(self.root_at),
                    lower: root.hash(self.root_at),
                    upper: (self.root_at + 1 < root.count()).then(|| root.hash(self.root_at + 1)),
                }
            }
            Some(node) => {
                let entries = Entries::of(index, Level::Node);
                Leaf {
                    logical: entries.child(node.at),
                    lower: if node.at == 0 {
                        node.lower
                    } else {
                        entries.hash(node.at)
                    },
                    upper: if node.at + 1 < entries.count() {
                        Some(entries.hash(node.at + 1))
                    } else {
                        node.upper
                    },
                }
            }
        }
    }

    /// The level of the block holding the walk's leaf entry.
    fn parent(&self) -> Level {
        if self.node.is_some() {
            Level::Node
        } else {
            Level::Root
        }
    }

    /// The position of the walk's leaf entry within its block.
    fn at(&self) -> usize {
        self.node.map_or(self.root_at, |node| node.at)
    }
}

/// One live entry of a leaf: its name's hash, where its record lies, and its
/// name's length.
#[derive(Copy, Clone, Default)]
struct Slot {
    hash: u32,
    at: u16,
    len: u8,
}

impl Slot {
    fn name(self, leaf: &[u8]) -> &[u8] {
        let start = usize::from(self.at) + DIRENT_HEADER;
        leaf.get(start..start + usize::from(self.len))
            .unwrap_or_default()
    }

    fn ino(self, leaf: &[u8]) -> u32 {
        le32(leaf, usize::from(self.at))
    }

    /// The bytes its record needs.
    fn size(self) -> usize {
        align4(DIRENT_HEADER + usize::from(self.len))
    }
}

/// A listing in progress: the entry it last returned, the leaf being read
/// and its entries, and where entries go.
struct Listing<'v> {
    past: Past,
    leaf: Vec<u8>,
    slots: Vec<Slot>,
    visit: &'v mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
}

/// Heap room for a leaf's worth of entries.
fn slot_scratch() -> Result<Vec<Slot>, DriverError> {
    tairix_util::fallible::filled(MAX_LEAF_ENTRIES, Slot::default()).ok_or(DriverError::NoSpace)
}

/// The entry a listing last returned: nothing yet, or its hash and name.
struct Past {
    hash: u32,
    name: [u8; MAX_NAME_LEN],
    len: Option<usize>,
}

impl Past {
    fn start() -> Self {
        Self {
            hash: 0,
            name: [0; MAX_NAME_LEN],
            len: None,
        }
    }

    /// Whether `(hash, name)` comes after the entry passed.
    fn before(&self, hash: u32, name: &[u8]) -> bool {
        self.len
            .is_none_or(|len| (self.hash, &self.name[..len]) < (hash, name))
    }

    fn pass(&mut self, hash: u32, name: &[u8]) -> Result<(), DriverError> {
        self.name
            .get_mut(..name.len())
            .ok_or(DriverError::DeviceFault)?
            .copy_from_slice(name);
        self.hash = hash;
        self.len = Some(name.len());
        Ok(())
    }
}

/// A leaf a new name is bound for: the walk that reached it, the leaf and
/// where it lives, and the name's hash.
#[derive(Copy, Clone)]
struct Target {
    walk: Walk,
    leaf: Leaf,
    phys: u64,
    hash: u32,
}

/// Where a full leaf is split: the first entry, in hash order, that moves to
/// the new leaf, the hash the new leaf starts at, and whether entries of that
/// hash stay behind too.
#[derive(Copy, Clone)]
struct Cut {
    index: usize,
    hash: u32,
    continued: bool,
}

/// Choose where `slots`, sorted by hash, split so the new name — hashing to
/// `child`, needing `needed` bytes — fits the half its hash falls in, with
/// `pinned` bytes staying in the old leaf whatever the cut. A cut that keeps a
/// hash's entries together is preferred; then the most even one.
fn choose_cut(
    slots: &[Slot],
    pinned: usize,
    child: u32,
    needed: usize,
    capacity: usize,
) -> Option<Cut> {
    let total: usize = slots.iter().copied().map(Slot::size).sum();
    let mut below = 0;
    let mut best: Option<(Cut, (bool, usize))> = None;
    for index in 1..slots.len() {
        below += slots[index - 1].size();
        let hash = slots[index].hash;
        let (low, high) = if child >= hash {
            (pinned + below, total - below + needed)
        } else {
            (pinned + below + needed, total - below)
        };
        if low > capacity || high > capacity {
            continue;
        }
        let continued = slots[index - 1].hash == hash;
        let score = (continued, low.abs_diff(high));
        if best.as_ref().is_none_or(|(_, kept)| score < *kept) {
            best = Some((
                Cut {
                    index,
                    hash,
                    continued,
                },
                score,
            ));
        }
    }
    best.map(|(cut, _)| cut)
}

/// The cursor resuming a listing just past `name`.
fn cursor_after(name: &[u8]) -> u64 {
    let seed = tairix_hash::published().unwrap_or(HashSeed::UNKEYED);
    INDEXED_CURSOR | SipHash13::hash_bytes(seed, name) >> 1
}

impl<B: Block> Ext4<B> {
    /// How directory `dir_ino` is read and written, leaving an indexed one's
    /// root in `index`.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a root whose checksum fails: a block
    /// that is not what was written is never read past.
    pub(super) fn index_use(
        &mut self,
        dir_ino: u32,
        dir: &Inode,
        index: &mut [u8],
    ) -> Result<IndexUse, DriverError> {
        if !self.layout.dir_index || dir.flags & INODE_FLAG_INDEX == 0 {
            return Ok(IndexUse::Linear);
        }
        let seed = self.dir_seed(dir_ino, dir);
        let Some(root) = self.map_block(dir, 0)? else {
            return Ok(IndexUse::Unusable(DriverError::DeviceFault));
        };
        self.read_fs_block(root, index)?;
        let info = match htree::check_root(index, seed.is_some()) {
            Ok(info) => info,
            Err(err) => return Ok(IndexUse::Unusable(err)),
        };
        if seed.is_some_and(|seed| !htree::verify(index, Level::Root, seed)) {
            return Err(DriverError::DeviceFault);
        }
        if info.levels > htree::MAX_LEVELS {
            return Ok(IndexUse::Unusable(DriverError::Unsupported));
        }
        Ok(
            match NameHash::new(
                info.hash_version,
                self.layout.hash_signed,
                self.layout.hash_seed,
            ) {
                Ok(hash) => IndexUse::Indexed(Indexed {
                    hash,
                    levels: info.levels,
                    root,
                    blocks: dir.size.div_ceil(u64::from(self.layout.block_size)),
                    seed,
                }),
                Err(err) => IndexUse::Unusable(err),
            },
        )
    }

    /// Read index block `phys`, of `level`, into `block`, checked.
    fn read_index(
        &mut self,
        ix: &Indexed,
        phys: u64,
        level: Level,
        block: &mut [u8],
    ) -> Result<(), DriverError> {
        self.read_fs_block(phys, block)?;
        match level {
            Level::Root => htree::check_root(block, ix.seed.is_some()).map(drop)?,
            Level::Node => htree::check_node(block, ix.seed.is_some())?,
        }
        if ix
            .seed
            .is_some_and(|seed| !htree::verify(block, level, seed))
        {
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    }

    /// Seal index block `block`, of `level`, and write it to `phys`.
    fn write_index(
        &mut self,
        ix: &Indexed,
        phys: u64,
        level: Level,
        block: &mut [u8],
    ) -> Result<(), DriverError> {
        if let Some(seed) = ix.seed {
            htree::seal(block, level, seed);
        }
        self.write_fs_block(phys, block)
    }

    /// Write `block`, the directory's root changed in place, sealed.
    pub(super) fn rewrite_root(
        &mut self,
        ix: &Indexed,
        block: &mut [u8],
    ) -> Result<(), DriverError> {
        self.write_index(ix, ix.root, Level::Root, block)
    }

    /// Read leaf `phys` into `block`, its tail checked.
    fn read_leaf(&mut self, ix: &Indexed, phys: u64, block: &mut [u8]) -> Result<(), DriverError> {
        self.read_fs_block(phys, block)?;
        match ix.seed {
            Some(seed) => Self::check_leaf(seed, block),
            None => Ok(()),
        }
    }

    /// The block behind logical block `logical`, which an index names: never
    /// the root, never past the directory's end, never a hole.
    fn indexed_block(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        logical: u32,
    ) -> Result<u64, DriverError> {
        if logical == 0 || u64::from(logical) >= ix.blocks {
            return Err(DriverError::DeviceFault);
        }
        self.map_block(dir, u64::from(logical))?
            .ok_or(DriverError::DeviceFault)
    }

    /// Walk from the root in `index` to the leaf whose range holds `hash`,
    /// the first of its run, leaving the lowest index block passed in
    /// `index`.
    fn walk_to(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        hash: u32,
        index: &mut [u8],
    ) -> Result<Walk, DriverError> {
        let root = Entries::of(index, Level::Root);
        let root_at = root.find(hash);
        let root_full = root.is_full();
        if ix.levels == 0 {
            return Ok(Walk {
                root_at,
                root_full,
                node: None,
            });
        }
        let logical = root.child(root_at);
        let lower = root.hash(root_at);
        let upper = (root_at + 1 < root.count()).then(|| root.hash(root_at + 1));
        let phys = self.indexed_block(dir, ix, logical)?;
        self.read_index(ix, phys, Level::Node, index)?;
        let at = Entries::of(index, Level::Node).find(hash);
        Ok(Walk {
            root_at,
            root_full,
            node: Some(NodeAt {
                phys,
                at,
                lower,
                upper,
            }),
        })
    }

    /// Step `walk` to the next leaf in hash order; `false` past the last.
    fn advance(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        walk: &mut Walk,
        index: &mut [u8],
    ) -> Result<bool, DriverError> {
        let Some(node) = walk.node else {
            if walk.root_at + 1 >= Entries::of(index, Level::Root).count() {
                return Ok(false);
            }
            walk.root_at += 1;
            return Ok(true);
        };
        if node.at + 1 < Entries::of(index, Level::Node).count() {
            walk.node = Some(NodeAt {
                at: node.at + 1,
                ..node
            });
            return Ok(true);
        }
        self.read_index(ix, ix.root, Level::Root, index)?;
        let root = Entries::of(index, Level::Root);
        let root_at = walk.root_at + 1;
        if root_at >= root.count() {
            return Ok(false);
        }
        let logical = root.child(root_at);
        let lower = root.hash(root_at);
        let upper = (root_at + 1 < root.count()).then(|| root.hash(root_at + 1));
        let phys = self.indexed_block(dir, ix, logical)?;
        self.read_index(ix, phys, Level::Node, index)?;
        walk.root_at = root_at;
        walk.node = Some(NodeAt {
            phys,
            at: 0,
            lower,
            upper,
        });
        Ok(true)
    }

    /// The live entries of leaf `block`, each with its hash, into `slots`;
    /// how many there are.
    fn leaf_slots(
        &self,
        ix: &Indexed,
        block: &[u8],
        slots: &mut [Slot],
    ) -> Result<usize, DriverError> {
        let end = self.dir_data_end();
        let data = block.get(..end).ok_or(DriverError::DeviceFault)?;
        let mut count = 0;
        let mut pos = 0;
        while pos + DIRENT_HEADER <= end {
            let record = self.record_at(data, pos)?;
            if record.ino != 0 {
                // A leaf names children only: anything else under a live
                // inode is damage.
                let name = record
                    .child_name(data, pos)
                    .ok_or(DriverError::DeviceFault)?;
                *slots.get_mut(count).ok_or(DriverError::DeviceFault)? = Slot {
                    hash: ix.hash.of(name).ok_or(DriverError::DeviceFault)?,
                    at: u16::try_from(pos).map_err(|_| DriverError::DeviceFault)?,
                    len: u8::try_from(name.len()).map_err(|_| DriverError::DeviceFault)?,
                };
                count += 1;
            }
            pos += record.rec_len;
        }
        Ok(count)
    }

    /// The record named `name` in indexed directory `dir`, with the leaf
    /// holding it, walked with `index` and read into `leaf`.
    fn locate_indexed(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        name: &[u8],
        index: &mut [u8],
        leaf: &mut [u8],
    ) -> Result<Option<(u64, Located)>, DriverError> {
        let Some(hash) = ix.hash.of(name) else {
            return Ok(None);
        };
        let end = self.dir_data_end();
        let mut walk = self.walk_to(dir, ix, hash, index)?;
        for _ in 0..MAX_RUN_LEAVES {
            let at = walk.leaf(index);
            let phys = self.indexed_block(dir, ix, at.logical)?;
            self.read_leaf(ix, phys, leaf)?;
            if let Some(found) =
                self.locate(leaf.get(..end).ok_or(DriverError::DeviceFault)?, name)?
            {
                return Ok(Some((phys, found)));
            }
            if at.continued() != Some(hash) || !self.advance(dir, ix, &mut walk, index)? {
                return Ok(None);
            }
        }
        Err(DriverError::DeviceFault)
    }

    /// The inode indexed directory `dir` names `name`, its root in `index`.
    pub(super) fn find_indexed(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        name: &[u8],
        index: &mut [u8],
    ) -> Result<Option<u32>, DriverError> {
        let mut leaf = self.block_scratch()?;
        let bs = self.layout.block_size as usize;
        Ok(self
            .locate_indexed(dir, ix, name, index, &mut leaf[..bs])?
            .map(|(_, found)| found.ino))
    }

    /// Remove `name` from indexed directory `dir`, its root in `index`,
    /// answering the inode it named.
    pub(super) fn remove_indexed(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        name: &[u8],
        index: &mut [u8],
    ) -> Result<u32, DriverError> {
        let mut leaf = self.block_scratch()?;
        let bs = self.layout.block_size as usize;
        let (phys, found) = self
            .locate_indexed(dir, ix, name, index, &mut leaf[..bs])?
            .ok_or(DriverError::NotFound)?;
        self.unlink_record(&mut leaf[..bs], &found)?;
        self.write_leaf(ix.seed, phys, &mut leaf[..bs])?;
        Ok(found.ino)
    }

    /// Whether indexed directory `dir`, its root in `index`, names no child.
    pub(super) fn indexed_is_empty(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        index: &mut [u8],
    ) -> Result<bool, DriverError> {
        let mut leaf = self.block_scratch()?;
        let bs = self.layout.block_size as usize;
        let end = self.dir_data_end();
        let mut walk = self.walk_to(dir, ix, 0, index)?;
        loop {
            let phys = self.indexed_block(dir, ix, walk.leaf(index).logical)?;
            self.read_leaf(ix, phys, &mut leaf[..bs])?;
            let data = leaf.get(..end).ok_or(DriverError::DeviceFault)?;
            let mut pos = 0;
            while pos + DIRENT_HEADER <= end {
                let record = self.record_at(data, pos)?;
                if record.ino != 0 {
                    return Ok(false);
                }
                pos += record.rec_len;
            }
            if !self.advance(dir, ix, &mut walk, index)? {
                return Ok(true);
            }
        }
    }

    /// Hand `visit` the entry naming inode `ino` as `name`.
    fn hand_over(
        &mut self,
        ino: u32,
        name: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<DirVisit, DriverError> {
        let child = self.read_inode(ino)?;
        let entry = DirEntry {
            node: NodeId::from_raw(u64::from(ino)),
            info: self.inode_info(&child)?,
            next_cursor: cursor_after(name),
        };
        Ok(visit(&entry, name))
    }

    /// Hand `visit` the children of indexed directory `dir`, its root in
    /// `index`, in `(hash, name)` order from just past `after`, the name the
    /// listing last returned (empty at the start).
    pub(super) fn list_indexed(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        after: &[u8],
        index: &mut [u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        let mut listing = Listing {
            past: Past::start(),
            leaf: self.block_scratch()?,
            slots: slot_scratch()?,
            visit,
        };
        if !after.is_empty() {
            let hash = ix.hash.of(after).ok_or(DriverError::DeviceFault)?;
            listing.past.pass(hash, after)?;
        }
        let bs = self.layout.block_size as usize;
        let mut walk = self.walk_to(dir, ix, listing.past.hash, index)?;
        loop {
            let at = walk.leaf(index);
            let phys = self.indexed_block(dir, ix, at.logical)?;
            self.read_leaf(ix, phys, &mut listing.leaf[..bs])?;
            let found = self.leaf_slots(ix, &listing.leaf[..bs], &mut listing.slots)?;
            let mut live = 0;
            for n in 0..found {
                let slot = listing.slots[n];
                if at.holds(slot.hash) && listing.past.before(slot.hash, slot.name(&listing.leaf)) {
                    listing.slots[live] = slot;
                    live += 1;
                }
            }
            let leaf = &listing.leaf;
            listing.slots[..live].sort_unstable_by(|a, b| {
                a.hash
                    .cmp(&b.hash)
                    .then_with(|| a.name(leaf).cmp(b.name(leaf)))
            });
            let run = at.continued();
            for n in 0..live {
                let slot = listing.slots[n];
                if Some(slot.hash) == run {
                    break;
                }
                let name = slot.name(&listing.leaf);
                if self.hand_over(slot.ino(&listing.leaf), name, listing.visit)? == DirVisit::Stop {
                    return Ok(());
                }
                listing.past.pass(slot.hash, name)?;
            }
            if let Some(hash) = run {
                if self.list_run(dir, ix, hash, &mut walk, index, &mut listing)? == DirVisit::Stop {
                    return Ok(());
                }
                // The walk now stands at the run's last leaf, whose names
                // past the run come next.
                continue;
            }
            if !self.advance(dir, ix, &mut walk, index)? {
                return Ok(());
            }
        }
    }

    /// Hand the listing the entries hashing to `hash` past the one it last
    /// returned, in name order: a run starting in the leaf `walk` stands at
    /// and continuing through the leaves after it, where the walk is left.
    fn list_run(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        hash: u32,
        walk: &mut Walk,
        index: &mut [u8],
        listing: &mut Listing<'_>,
    ) -> Result<DirVisit, DriverError> {
        let bs = self.layout.block_size as usize;
        let mut run = [0u64; MAX_RUN_LEAVES];
        let mut len = 0;
        loop {
            let at = walk.leaf(index);
            *run.get_mut(len).ok_or(DriverError::DeviceFault)? =
                self.indexed_block(dir, ix, at.logical)?;
            len += 1;
            if at.continued() != Some(hash) {
                break;
            }
            // An entry that continues a run always has one after it.
            if !self.advance(dir, ix, walk, index)? {
                return Err(DriverError::DeviceFault);
            }
        }
        let mut best_name = [0u8; MAX_NAME_LEN];
        loop {
            let mut best: Option<(u32, usize)> = None;
            for &phys in &run[..len] {
                self.read_leaf(ix, phys, &mut listing.leaf[..bs])?;
                let found = self.leaf_slots(ix, &listing.leaf[..bs], &mut listing.slots)?;
                for slot in &listing.slots[..found] {
                    let name = slot.name(&listing.leaf);
                    if slot.hash == hash
                        && listing.past.before(hash, name)
                        && best.is_none_or(|(_, kept)| name < &best_name[..kept])
                    {
                        best_name[..name.len()].copy_from_slice(name);
                        best = Some((slot.ino(&listing.leaf), name.len()));
                    }
                }
            }
            let Some((ino, kept)) = best else {
                return Ok(DirVisit::Take);
            };
            if self.hand_over(ino, &best_name[..kept], listing.visit)? == DirVisit::Stop {
                return Ok(DirVisit::Stop);
            }
            listing.past.pass(hash, &best_name[..kept])?;
        }
    }

    /// Add `child` to indexed directory `dir_ino`, its root in `index`: into
    /// the leaf its hash selects, packed or split when full, the index grown
    /// to name a new leaf.
    pub(super) fn insert_indexed(
        &mut self,
        dir_ino: u32,
        dir: &Inode,
        mut ix: Indexed,
        child: Child<'_>,
        index: &mut [u8],
    ) -> Result<(), DriverError> {
        let hash = ix
            .hash
            .of(child.name)
            .ok_or(DriverError::LengthOutOfRange)?;
        let walk = self.walk_to(dir, &ix, hash, index)?;
        let leaf = walk.leaf(index);
        let phys = self.indexed_block(dir, &ix, leaf.logical)?;
        let parent_full = Entries::of(index, walk.parent()).is_full();
        if self.place_in_leaf(&ix, phys, child)? {
            return Ok(());
        }
        let walk = if parent_full {
            self.make_index_room(dir_ino, &mut ix, walk)?
        } else {
            walk
        };
        // Growing the index may have mapped blocks the inode read before
        // does not show.
        let dir = self.read_inode(dir_ino)?;
        let target = Target {
            walk,
            leaf,
            phys,
            hash,
        };
        self.split_leaf(dir_ino, &dir, &mut ix, target, child)
    }

    /// Add `child` to leaf `phys` when it has room, packing its records first
    /// when only scattered free space would fit it; `false` when it is full.
    fn place_in_leaf(
        &mut self,
        ix: &Indexed,
        phys: u64,
        child: Child<'_>,
    ) -> Result<bool, DriverError> {
        let bs = self.layout.block_size as usize;
        let end = self.dir_data_end();
        let needed = align4(DIRENT_HEADER + child.name.len());
        let mut leaf = self.block_scratch()?;
        self.read_leaf(ix, phys, &mut leaf[..bs])?;
        if !self.place_in_block(
            &mut leaf[..end],
            needed,
            child.ino,
            child.name,
            child.file_type,
        )? {
            let mut scratch = self.block_scratch()?;
            scratch[..end].copy_from_slice(&leaf[..end]);
            let mut live = tairix_util::fallible::filled(MAX_LEAF_ENTRIES, 0u16)
                .ok_or(DriverError::NoSpace)?;
            let (count, used) = self.live_records(&scratch[..end], &mut live)?;
            if used + needed > end {
                return Ok(false);
            }
            self.lay_records(
                &scratch[..end],
                live[..count].iter().copied(),
                &mut leaf[..end],
            )?;
            if !self.place_in_block(
                &mut leaf[..end],
                needed,
                child.ino,
                child.name,
                child.file_type,
            )? {
                return Err(DriverError::DeviceFault);
            }
        }
        self.write_leaf(ix.seed, phys, &mut leaf[..bs])?;
        Ok(true)
    }

    /// The offsets of `block`'s live records into `live`, with how many there
    /// are and the bytes they need.
    fn live_records(&self, block: &[u8], live: &mut [u16]) -> Result<(usize, usize), DriverError> {
        let (mut count, mut used, mut pos) = (0, 0, 0);
        while pos + DIRENT_HEADER <= block.len() {
            let record = self.record_at(block, pos)?;
            if record.ino != 0 {
                if record.name_len == 0 || DIRENT_HEADER + record.name_len > record.rec_len {
                    return Err(DriverError::DeviceFault);
                }
                *live.get_mut(count).ok_or(DriverError::DeviceFault)? =
                    u16::try_from(pos).map_err(|_| DriverError::DeviceFault)?;
                count += 1;
                used += align4(DIRENT_HEADER + record.name_len);
            }
            pos += record.rec_len;
        }
        Ok((count, used))
    }

    /// Lay the records of `src` at `records` one after another into `dst`, a
    /// leaf's data area, the last spanning to its end.
    fn lay_records(
        &self,
        src: &[u8],
        records: impl Iterator<Item = u16>,
        dst: &mut [u8],
    ) -> Result<(), DriverError> {
        dst.fill(0);
        let (mut pos, mut last) = (0, None);
        for at in records.map(usize::from) {
            let record = self.record_at(src, at)?;
            let used = DIRENT_HEADER + record.name_len;
            let size = align4(used);
            dst.get_mut(pos..pos + used)
                .ok_or(DriverError::DeviceFault)?
                .copy_from_slice(src.get(at..at + used).ok_or(DriverError::DeviceFault)?);
            put_le16(dst, pos + 4, u16_of(size)?);
            last = Some(pos);
            pos += size;
        }
        match last {
            Some(at) => put_le16(dst, at + 4, u16_of(dst.len() - at)?),
            None => put_le16(dst, 4, u16_of(dst.len())?),
        }
        Ok(())
    }

    /// Give the leaf entry `walk` stands at room for a sibling: a full root
    /// moves its entries down to a new node and points at that alone, and a
    /// full node gives its upper half to a new one. Answers where the walk now
    /// stands.
    fn make_index_room(
        &mut self,
        dir_ino: u32,
        ix: &mut Indexed,
        walk: Walk,
    ) -> Result<Walk, DriverError> {
        let bs = self.layout.block_size as usize;
        let csum = ix.seed.is_some();
        let mut parent = self.block_scratch()?;
        let mut fresh = self.block_scratch()?;
        let Some(node) = walk.node else {
            self.read_index(ix, ix.root, Level::Root, &mut parent[..bs])?;
            htree::init_node(&mut fresh[..bs], csum)?;
            let logical = u32::try_from(ix.blocks).map_err(|_| DriverError::NoSpace)?;
            EntriesMut::of(&mut parent[..bs], Level::Root)
                .push_down(&mut EntriesMut::of(&mut fresh[..bs], Level::Node), logical)?;
            if let Some(seed) = ix.seed {
                htree::seal(&mut fresh[..bs], Level::Node, seed);
            }
            let phys = self.append_dir_block(dir_ino, &mut ix.blocks, &fresh[..bs])?;
            htree::set_levels(&mut parent[..bs], 1);
            self.write_index(ix, ix.root, Level::Root, &mut parent[..bs])?;
            ix.levels = 1;
            return Ok(Walk {
                root_at: 0,
                root_full: false,
                node: Some(NodeAt {
                    phys,
                    at: walk.root_at,
                    lower: 0,
                    upper: None,
                }),
            });
        };
        if walk.root_full {
            // Both levels are full: the most a two-level index holds.
            return Err(DriverError::NoSpace);
        }
        self.read_index(ix, node.phys, Level::Node, &mut parent[..bs])?;
        let entries = Entries::of(&parent[..bs], Level::Node);
        // A node whose entries stray past the root's range for it was left
        // half-split; splitting it again would compound that.
        let count = entries.count();
        if (count > 1 && entries.hash(1) < node.lower)
            || node
                .upper
                .is_some_and(|upper| entries.hash(count - 1) > upper)
        {
            return Err(DriverError::DeviceFault);
        }
        let split = count / 2;
        htree::init_node(&mut fresh[..bs], csum)?;
        let lowest = EntriesMut::of(&mut parent[..bs], Level::Node)
            .split_off(split, &mut EntriesMut::of(&mut fresh[..bs], Level::Node))?;
        if let Some(seed) = ix.seed {
            htree::seal(&mut fresh[..bs], Level::Node, seed);
        }
        let logical = u32::try_from(ix.blocks).map_err(|_| DriverError::NoSpace)?;
        let phys = self.append_dir_block(dir_ino, &mut ix.blocks, &fresh[..bs])?;
        self.read_index(ix, ix.root, Level::Root, &mut fresh[..bs])?;
        EntriesMut::of(&mut fresh[..bs], Level::Root).insert(walk.root_at, lowest, logical)?;
        self.write_index(ix, ix.root, Level::Root, &mut fresh[..bs])?;
        self.write_index(ix, node.phys, Level::Node, &mut parent[..bs])?;
        Ok(if node.at >= split {
            Walk {
                root_at: walk.root_at + 1,
                root_full: false,
                node: Some(NodeAt {
                    phys,
                    at: node.at - split,
                    lower: lowest,
                    upper: node.upper,
                }),
            }
        } else {
            Walk {
                root_full: false,
                node: Some(NodeAt {
                    upper: Some(lowest),
                    ..node
                }),
                ..walk
            }
        })
    }

    /// Split `target`'s full leaf and add `child` to the half its hash falls
    /// in. Entries outside the leaf's range stay where they are.
    fn split_leaf(
        &mut self,
        dir_ino: u32,
        dir: &Inode,
        ix: &mut Indexed,
        target: Target,
        child: Child<'_>,
    ) -> Result<(), DriverError> {
        let Target {
            walk,
            leaf,
            phys,
            hash,
        } = target;
        let bs = self.layout.block_size as usize;
        let end = self.dir_data_end();
        let mut old = self.block_scratch()?;
        let mut fresh = self.block_scratch()?;
        let mut slots = slot_scratch()?;
        self.read_leaf(ix, phys, &mut old[..bs])?;
        let found = self.leaf_slots(ix, &old[..bs], &mut slots)?;
        let mut placed = 0;
        for n in 0..found {
            if leaf.holds(slots[n].hash) {
                slots.swap(placed, n);
                placed += 1;
            }
        }
        let pinned: usize = slots[placed..found].iter().copied().map(Slot::size).sum();
        slots[..placed].sort_unstable_by_key(|slot| slot.hash);
        let needed = align4(DIRENT_HEADER + child.name.len());
        let cut = choose_cut(&slots[..placed], pinned, hash, needed, end)
            .ok_or(DriverError::DeviceFault)?;
        if cut.continued {
            self.read_index(ix, ix.root, Level::Root, &mut fresh[..bs])?;
            if self.run_leaves(dir, ix, cut.hash, &mut fresh[..bs])? >= MAX_RUN_LEAVES {
                return Err(DriverError::NoSpace);
            }
        }
        let upward = hash >= cut.hash;

        self.lay_records(
            &old[..end],
            slots[cut.index..placed].iter().map(|slot| slot.at),
            &mut fresh[..end],
        )?;
        if upward
            && !self.place_in_block(
                &mut fresh[..end],
                needed,
                child.ino,
                child.name,
                child.file_type,
            )?
        {
            return Err(DriverError::DeviceFault);
        }
        if let Some(seed) = ix.seed {
            Self::seal_leaf(seed, &mut fresh[..bs])?;
        }
        let logical = u32::try_from(ix.blocks).map_err(|_| DriverError::NoSpace)?;
        self.append_dir_block(dir_ino, &mut ix.blocks, &fresh[..bs])?;

        let (parent_phys, level) = match walk.node {
            Some(node) => (node.phys, Level::Node),
            None => (ix.root, Level::Root),
        };
        self.read_index(ix, parent_phys, level, &mut fresh[..bs])?;
        EntriesMut::of(&mut fresh[..bs], level).insert(
            walk.at(),
            cut.hash | if cut.continued { CONTINUED } else { 0 },
            logical,
        )?;
        self.write_index(ix, parent_phys, level, &mut fresh[..bs])?;

        let kept = slots[..cut.index]
            .iter()
            .chain(&slots[placed..found])
            .map(|slot| slot.at);
        self.lay_records(&old[..end], kept, &mut fresh[..end])?;
        if !upward
            && !self.place_in_block(
                &mut fresh[..end],
                needed,
                child.ino,
                child.name,
                child.file_type,
            )?
        {
            return Err(DriverError::DeviceFault);
        }
        self.write_leaf(ix.seed, phys, &mut fresh[..bs])
    }

    /// How many leaves the run of `hash` spans, walked with `index`, which
    /// holds the root; at least one, the leaf the hash falls in.
    fn run_leaves(
        &mut self,
        dir: &Inode,
        ix: &Indexed,
        hash: u32,
        index: &mut [u8],
    ) -> Result<usize, DriverError> {
        let mut walk = self.walk_to(dir, ix, hash, index)?;
        let mut leaves = 1;
        while leaves < MAX_RUN_LEAVES && walk.leaf(index).continued() == Some(hash) {
            if !self.advance(dir, ix, &mut walk, index)? {
                return Err(DriverError::DeviceFault);
            }
            leaves += 1;
        }
        Ok(leaves)
    }
}

#[cfg(test)]
#[path = "indexed_tests.rs"]
mod tests;
