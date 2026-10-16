//! Logical-to-physical mapping of a file, over whichever map its inode
//! carries, and the data path that reads and writes through it a run at a
//! time.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;

use crate::blockmap::{self, MapStore};
use crate::bytes::{le16, le32, put_le32};
use crate::extent::{self, Extent, Mapping, NodeStore, Truncated, ROOT_LEN};
use crate::{Ext4, INODE_FLAGS, INODE_FLAG_EXTENTS, I_BLOCK_OFFSET};

/// Bytes an extent-block checksum tail takes.
const EXTENT_TAIL: usize = 4;
/// The classic map's pointers are 32 bits wide.
const CLASSIC_LIMIT: u64 = 1 << 32;

/// The filesystem as one inode's map sees it: its blocks verified and
/// sealed under the inode's checksum seed, allocations steered towards
/// `goal`, and what was allocated and released counted for `i_blocks`.
pub(crate) struct Tree<'a, B: Block> {
    pub(crate) fs: &'a mut Ext4<B>,
    seed: u32,
    goal: u64,
    /// Tree blocks this access allocated.
    pub(crate) nodes: u64,
}

impl<'a, B: Block> Tree<'a, B> {
    pub(crate) fn new(fs: &'a mut Ext4<B>, ino: u32, raw: &[u8]) -> Self {
        let seed = fs.inode_csum_seed(ino, le32(raw, crate::inode::GENERATION));
        let ipg = u64::from(fs.layout.inodes_per_group);
        let goal = fs
            .layout
            .group_first(u64::from(ino.saturating_sub(1)) / ipg);
        Self {
            fs,
            seed,
            goal,
            nodes: 0,
        }
    }

    fn tail_at(buf: &[u8]) -> usize {
        12 + usize::from(le16(buf, 4)) * 12
    }
}

impl<B: Block> NodeStore for Tree<'_, B> {
    fn node_bytes(&self) -> usize {
        self.fs.layout.block_size as usize
    }

    fn node_capacity(&self) -> usize {
        (self.fs.layout.block_size as usize - 12) / 12
    }

    fn read_node(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let from_device = self.fs.read_meta(block, buf)?;
        if from_device && self.fs.layout.metadata_csum {
            let tail = Self::tail_at(buf);
            if tail + EXTENT_TAIL > buf.len()
                || le32(buf, tail) != tairix_crc32c::update(self.seed, &buf[..tail])
            {
                return Err(DriverError::DeviceFault);
            }
        }
        Ok(())
    }

    fn write_node(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        if self.fs.layout.metadata_csum {
            let tail = Self::tail_at(buf);
            if tail + EXTENT_TAIL > buf.len() {
                return Err(DriverError::DeviceFault);
            }
            let sum = tairix_crc32c::update(self.seed, &buf[..tail]);
            put_le32(buf, tail, sum);
        }
        self.fs.put_meta(block, buf)
    }

    fn alloc_node(&mut self, goal: u64) -> Result<u64, DriverError> {
        let goal = if goal == 0 { self.goal } else { goal };
        let (block, _) = self.fs.alloc_blocks(goal, 1, u64::MAX)?;
        self.nodes += 1;
        Ok(block)
    }

    fn free_node(&mut self, block: u64) {
        self.fs.release(block, 1);
    }

    fn free_data(&mut self, start: u64, len: u64) {
        self.fs.release(start, len);
    }
}

impl<B: Block> MapStore for Tree<'_, B> {
    fn ptrs_per_block(&self) -> u64 {
        u64::from(self.fs.layout.block_size / 4)
    }

    fn read_ptrs(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.fs.read_meta(block, buf).map(drop)
    }

    fn write_ptrs(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.fs.put_meta(block, buf)
    }

    fn alloc_data(&mut self, goal: u64, want: u32) -> Result<(u64, u32), DriverError> {
        let goal = if goal == 0 { self.goal } else { goal };
        self.fs.alloc_blocks(goal, want, CLASSIC_LIMIT)
    }

    fn alloc_ptr_block(&mut self, goal: u64) -> Result<u64, DriverError> {
        let goal = if goal == 0 { self.goal } else { goal };
        let (block, _) = self.fs.alloc_blocks(goal, 1, CLASSIC_LIMIT)?;
        self.nodes += 1;
        Ok(block)
    }

    fn free_data(&mut self, start: u64, len: u64) {
        self.fs.release(start, len);
    }

    fn free_ptr_block(&mut self, block: u64) {
        self.fs.release(block, 1);
    }
}

/// Whether the inode record `raw` is extent-mapped.
pub(crate) fn uses_extents(raw: &[u8]) -> bool {
    le32(raw, INODE_FLAGS) & INODE_FLAG_EXTENTS != 0
}

fn iblock(raw: &[u8]) -> &[u8] {
    &raw[I_BLOCK_OFFSET..I_BLOCK_OFFSET + ROOT_LEN]
}

/// What one allocation mapped: where the data went, how many logical
/// blocks, and the tree blocks it took to reach them.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Mapped {
    pub(crate) phys: u64,
    pub(crate) len: u32,
    pub(crate) nodes: u64,
}

impl<B: Block> Ext4<B> {
    /// A freed block's release, its failure kept for the commit to report.
    ///
    /// A tree's release hooks cannot fail, so a refused run — one outside
    /// the volume — is held until the operation's end, which fails rather
    /// than leave the map naming freed space.
    pub(crate) fn release(&mut self, start: u64, len: u64) {
        if let Err(err) = self.free_blocks(start, len) {
            self.release_fault.get_or_insert(err);
        }
    }

    /// Resolve logical block `logical` of inode `ino`, whose record is
    /// `raw`.
    pub(crate) fn map_lookup(
        &mut self,
        ino: u32,
        raw: &[u8],
        logical: u64,
    ) -> Result<Mapping, DriverError> {
        let mut tree = Tree::new(self, ino, raw);
        if uses_extents(raw) {
            let Ok(logical) = u32::try_from(logical) else {
                return Ok(Mapping::Hole { len: None });
            };
            extent::lookup(iblock(raw), &mut tree, logical)
        } else {
            blockmap::lookup(iblock(raw), &mut tree, logical)
        }
    }

    /// Map up to `want` logical blocks from `logical`, a hole of `hole`
    /// blocks (`None`: unbounded), to fresh data.
    pub(crate) fn map_alloc(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        logical: u64,
        want: u32,
        hole: Option<u64>,
    ) -> Result<Mapped, DriverError> {
        let want = hole.map_or(want, |hole| {
            want.min(u32::try_from(hole).unwrap_or(u32::MAX))
        });
        let mut root = [0u8; ROOT_LEN];
        root.copy_from_slice(iblock(raw));
        let extents = uses_extents(raw);
        let mut tree = Tree::new(self, ino, raw);
        let mapped = if extents {
            let logical = u32::try_from(logical).map_err(|_| DriverError::LengthOutOfRange)?;
            let headroom = u64::from(u32::MAX) - u64::from(logical) + 1;
            let want = want.min(u32::try_from(headroom).unwrap_or(u32::MAX)).max(1);
            let goal = extent::goal_after(&root, &mut tree, logical)?.unwrap_or(tree.goal);
            let (phys, len) = tree.fs.alloc_blocks(goal, want, u64::MAX)?;
            let placed = extent::insert(
                &mut root,
                &mut tree,
                Extent {
                    logical,
                    phys,
                    len,
                    unwritten: false,
                },
            );
            if let Err(err) = placed {
                tree.fs.release(phys, u64::from(len));
                return Err(err);
            }
            Mapped {
                phys,
                len,
                nodes: tree.nodes,
            }
        } else {
            let got = blockmap::alloc(&mut root, &mut tree, logical, want, 0)?;
            Mapped {
                phys: got.phys,
                len: got.len,
                nodes: u64::from(got.meta),
            }
        };
        raw[I_BLOCK_OFFSET..I_BLOCK_OFFSET + ROOT_LEN].copy_from_slice(&root);
        Ok(mapped)
    }

    /// Release up to `budget` units of the mapping at or past logical block
    /// `from`, from the end of the file.
    pub(crate) fn map_truncate(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        from: u64,
        budget: u32,
    ) -> Result<Truncated, DriverError> {
        let mut root = [0u8; ROOT_LEN];
        root.copy_from_slice(iblock(raw));
        let extents = uses_extents(raw);
        let mut tree = Tree::new(self, ino, raw);
        let done = if extents {
            extent::truncate(&mut root, &mut tree, from, budget)?
        } else {
            blockmap::truncate(&mut root, &mut tree, from, budget)?
        };
        raw[I_BLOCK_OFFSET..I_BLOCK_OFFSET + ROOT_LEN].copy_from_slice(&root);
        Ok(done)
    }

    /// Mark `len` blocks from `logical`, inside one unwritten extent, as
    /// written; answers the tree blocks that took.
    pub(crate) fn map_mark_written(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        logical: u64,
        len: u32,
    ) -> Result<u64, DriverError> {
        let mut root = [0u8; ROOT_LEN];
        root.copy_from_slice(iblock(raw));
        let logical = u32::try_from(logical).map_err(|_| DriverError::DeviceFault)?;
        let mut tree = Tree::new(self, ino, raw);
        extent::mark_written(&mut root, &mut tree, logical, len)?;
        let nodes = tree.nodes;
        raw[I_BLOCK_OFFSET..I_BLOCK_OFFSET + ROOT_LEN].copy_from_slice(&root);
        Ok(nodes)
    }

    /// Read file data of inode `ino` (record `raw`, `size` bytes long) from
    /// byte `offset` into `buf`: one device request per mapped run, holes and
    /// unwritten extents read as zeros.
    pub(crate) fn read_file(
        &mut self,
        ino: u32,
        raw: &[u8],
        size: u64,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        if offset >= size {
            return Ok(0);
        }
        let want = usize::try_from((size - offset).min(buf.len() as u64))
            .map_err(|_| DriverError::DeviceFault)?;
        let bs = u64::from(self.layout.block_size);
        let mut done = 0usize;
        while done < want {
            let cursor = offset + done as u64;
            let logical = cursor / bs;
            let within = cursor % bs;
            let span = |blocks: Option<u64>| {
                let bytes = blocks
                    .and_then(|blocks| blocks.checked_mul(bs))
                    .map_or(u64::MAX, |bytes| bytes - within);
                usize::try_from(bytes)
                    .unwrap_or(usize::MAX)
                    .min(want - done)
            };
            match self.map_lookup(ino, raw, logical)? {
                Mapping::Mapped {
                    phys,
                    len,
                    unwritten: false,
                } => {
                    if phys
                        .checked_add(len)
                        .is_none_or(|end| end > self.layout.blocks_count)
                    {
                        return Err(DriverError::DeviceFault);
                    }
                    let take = span(Some(len));
                    let at = phys * bs + within;
                    self.read_data_bytes(at, &mut buf[done..done + take])?;
                    done += take;
                }
                Mapping::Mapped { len, .. } => {
                    let take = span(Some(len));
                    buf[done..done + take].fill(0);
                    done += take;
                }
                Mapping::Hole { len } => {
                    let take = span(len);
                    buf[done..done + take].fill(0);
                    done += take;
                }
            }
        }
        Ok(done)
    }

    /// The highest logical block inode record `raw`'s map can address,
    /// exclusive: the extent tree's, or the classic map's three indirect
    /// levels.
    pub(crate) fn map_limit(&self, raw: &[u8]) -> u64 {
        if uses_extents(raw) {
            crate::extent::LOGICAL_LIMIT
        } else {
            blockmap::limit(u64::from(self.layout.block_size / 4))
        }
    }
}
