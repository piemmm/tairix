//! Regular-file contents: writing a run at a time, cutting a file back, and
//! deleting one, each bounded so its transaction fits the bound and the
//! journal, the orphan chain keeping a cut that spans transactions whole.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_writeback::{RunWindow, RUN_BYTES};

use crate::bytes::{high32, le16, le32, low16, low32, put_le16, put_le32, scratch, usize_of};
use crate::extent::Mapping;
use crate::group::MAX_RUN;
use crate::superblock::{make_dynamic, RO_COMPAT_LARGE_FILE, SB_FEATURE_RO_COMPAT};
use crate::{
    put_stamps, Ext4, Layout, Stamp, INODE_BLOCKS_HI, INODE_BLOCKS_LO, INODE_DTIME, INODE_FLAGS,
    INODE_FLAG_HUGE_FILE, INODE_FLAG_INLINE_DATA, INODE_LINKS, INODE_SIZE_HI, INODE_SIZE_LO,
    I_BLOCK_LEN, S_IFDIR, S_IFLNK, S_IFMT, S_IFREG,
};

/// The most extents or indirect blocks one cutting pass releases.
const CUT_BUDGET: u32 = 256;
/// Blocks one released unit may stage, at worst: the bitmaps and
/// descriptors of the two groups a run can span, the node that named it,
/// and that node's own release.
const CUT_UNIT_COST: usize = 8;

/// A file's length as its record holds it. A directory's high word is a
/// length only with `largedir`.
pub(crate) fn inode_size_bytes(raw: &[u8], layout: &Layout) -> u64 {
    let lo = u64::from(le32(raw, INODE_SIZE_LO));
    let dir = le16(raw, 0) & S_IFMT == S_IFDIR;
    let hi = if dir && !layout.largedir {
        0
    } else {
        u64::from(le32(raw, INODE_SIZE_HI))
    };
    (hi << 32) | lo
}

pub(crate) fn set_size(raw: &mut [u8], size: u64) {
    put_le32(raw, INODE_SIZE_LO, low32(size));
    put_le32(raw, INODE_SIZE_HI, high32(size));
}

/// Allocated storage as `i_blocks` records it, in 512-byte sectors.
pub(crate) fn sectors(raw: &[u8], layout: &Layout) -> u64 {
    let hi = if layout.huge_file {
        u64::from(le16(raw, INODE_BLOCKS_HI)) << 32
    } else {
        0
    };
    let count = u64::from(le32(raw, INODE_BLOCKS_LO)) | hi;
    if le32(raw, INODE_FLAGS) & INODE_FLAG_HUGE_FILE != 0 {
        count.saturating_mul(u64::from(layout.block_size / 512))
    } else {
        count
    }
}

/// Record `count` 512-byte sectors in `i_blocks`, in the widest form the
/// volume allows.
fn set_sectors(raw: &mut [u8], layout: &Layout, count: u64) -> Result<(), DriverError> {
    let mut flags = le32(raw, INODE_FLAGS) & !INODE_FLAG_HUGE_FILE;
    let stored = if count >> 32 == 0 || (layout.huge_file && count >> 48 == 0) {
        count
    } else if layout.huge_file && (count / u64::from(layout.block_size / 512)) >> 48 == 0 {
        flags |= INODE_FLAG_HUGE_FILE;
        count / u64::from(layout.block_size / 512)
    } else {
        return Err(DriverError::NoSpace);
    };
    put_le32(raw, INODE_FLAGS, flags);
    put_le32(raw, INODE_BLOCKS_LO, low32(stored));
    if layout.huge_file {
        put_le16(raw, INODE_BLOCKS_HI, low16(high32(stored)));
    }
    Ok(())
}

/// Whether record `raw` is a symbolic link whose target `i_block` holds
/// inline: a fast link owns no data block, only an attribute block at most.
pub(crate) fn is_fast_symlink(raw: &[u8], layout: &Layout) -> bool {
    let xattr = if crate::xattr::block_of(raw, layout).is_some() {
        u64::from(layout.cluster_size / 512)
    } else {
        0
    };
    le16(raw, 0) & S_IFMT == S_IFLNK
        && le32(raw, INODE_FLAGS) & INODE_FLAG_INLINE_DATA == 0
        && inode_size_bytes(raw, layout) <= I_BLOCK_LEN as u64
        && sectors(raw, layout) <= xattr
}

/// Whether record `raw`'s `i_block` holds a block map. Inline data, a fast
/// link's target and a device's number live there instead, and none of them
/// may be read as block pointers.
pub(crate) fn has_block_map(raw: &[u8], layout: &Layout) -> bool {
    if le32(raw, INODE_FLAGS) & INODE_FLAG_INLINE_DATA != 0 {
        return false;
    }
    match le16(raw, 0) & S_IFMT {
        S_IFREG | S_IFDIR => true,
        S_IFLNK => !is_fast_symlink(raw, layout),
        _ => false,
    }
}

/// Add (or take away) `blocks` filesystem blocks to `i_blocks`.
pub(crate) fn adjust_blocks(
    raw: &mut [u8],
    layout: &Layout,
    blocks: i64,
) -> Result<(), DriverError> {
    let per = u64::from(layout.block_size / 512);
    let delta = blocks.unsigned_abs().saturating_mul(per);
    let now = sectors(raw, layout);
    let next = if blocks >= 0 {
        now.checked_add(delta).ok_or(DriverError::NoSpace)?
    } else {
        now.saturating_sub(delta)
    };
    set_sectors(raw, layout, next)
}

/// Lay `rest` into `run` from byte `within`, zeroing the remainder, and answer
/// how many bytes of it fit: a fresh block never carries what the medium held
/// before.
fn fill_run(run: &mut [u8], within: usize, rest: &[u8]) -> usize {
    run.fill(0);
    let take = (run.len() - within).min(rest.len());
    run[within..within + take].copy_from_slice(&rest[..take]);
    take
}

impl<B: Block> Ext4<B> {
    /// Write `data` at byte `offset` of regular file `ino` (record `raw`),
    /// returning how much was stored: all of it, or — when the transaction
    /// reaches its bound or a later run fails — every whole run before.
    pub(crate) fn write_file(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        offset: u64,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        let bs = u64::from(self.layout.block_size);
        let limit = self.map_limit(raw).saturating_mul(bs);
        if offset >= limit {
            return Err(DriverError::LengthOutOfRange);
        }
        let data = &data[..data
            .len()
            .min(usize::try_from(limit - offset).unwrap_or(usize::MAX))];
        let mut staging = RunWindow::default();
        let mut kept = scratch(raw.len())?;
        kept.copy_from_slice(raw);
        let mut written = 0usize;
        while written < data.len() {
            let run = self
                .write_run(ino, raw, offset, data, written, &mut staging)
                .and_then(|took| {
                    self.finish_write(ino, raw, offset + (written + took) as u64)?;
                    Ok(took)
                });
            match run {
                Ok(took) => {
                    written += took;
                    self.savepoint();
                    kept.copy_from_slice(raw);
                    if self.should_yield() {
                        break;
                    }
                }
                Err(_) if written > 0 => {
                    // What reached the file stands; the failed run goes,
                    // and the caller learns the count, as `write(2)` would.
                    self.undo_to_savepoint();
                    raw.copy_from_slice(&kept);
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        Ok(written)
    }

    /// Store the next run of `data` from `written`, answering its length.
    /// `staging` grows to the fresh blocks a run lays out whole, so writing
    /// over data already in place allocates nothing.
    fn write_run(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        offset: u64,
        data: &[u8],
        written: usize,
        staging: &mut RunWindow,
    ) -> Result<usize, DriverError> {
        let bs = u64::from(self.layout.block_size);
        let cursor = offset + written as u64;
        let logical = cursor / bs;
        let within = low32(cursor % bs) as usize;
        let rest = &data[written..];
        let blocks_wanted = (within + rest.len()).div_ceil(self.layout.block_size as usize) as u64;
        let room = RUN_BYTES as u64 / bs;
        match self.map_lookup(ino, raw, logical)? {
            Mapping::Mapped {
                phys,
                len,
                unwritten: false,
            } => {
                let mapped = usize::try_from(len.saturating_mul(bs))
                    .unwrap_or(usize::MAX)
                    .saturating_sub(within);
                let take = mapped.min(rest.len()).min(RUN_BYTES);
                self.write_data(phys, within as u64, &rest[..take])?;
                Ok(take)
            }
            Mapping::Mapped {
                phys,
                len,
                unwritten: true,
            } => {
                let blocks = len.min(blocks_wanted).min(room);
                let run = staging
                    .reserve(usize_of(blocks * bs)?)
                    .ok_or(DriverError::NoSpace)?;
                let take = fill_run(run, within, rest);
                self.write_data(phys, 0, run)?;
                let nodes = self.map_mark_written(
                    ino,
                    raw,
                    logical,
                    u32::try_from(blocks).map_err(|_| DriverError::DeviceFault)?,
                )?;
                crate::file::adjust_blocks(
                    raw,
                    &self.layout,
                    i64::try_from(nodes).map_err(|_| DriverError::DeviceFault)?,
                )?;
                Ok(take)
            }
            Mapping::Hole { len } => {
                let want = blocks_wanted.min(room).min(u64::from(MAX_RUN));
                let want = u32::try_from(want).map_err(|_| DriverError::DeviceFault)?;
                let got = self.map_alloc(ino, raw, logical, want, len)?;
                let run = staging
                    .reserve(usize_of(u64::from(got.len) * bs)?)
                    .ok_or(DriverError::NoSpace)?;
                let take = fill_run(run, within, rest);
                self.write_data(got.phys, 0, run)?;
                let added = i64::from(got.len)
                    + i64::try_from(got.nodes).map_err(|_| DriverError::DeviceFault)?;
                crate::file::adjust_blocks(raw, &self.layout, added)?;
                Ok(take)
            }
        }
    }

    /// Record a write that reached `end`: the size it grew to and the time.
    fn finish_write(&mut self, ino: u32, raw: &mut [u8], end: u64) -> Result<(), DriverError> {
        let size = inode_size_bytes(raw, &self.layout);
        if end > size {
            set_size(raw, end);
            self.note_size(end);
        }
        put_stamps(raw, self.now(), &[Stamp::Modify, Stamp::Change]);
        self.write_inode_raw(ino, raw)
    }

    /// Set regular file `ino`'s length to `size`, freeing what lies past it.
    /// A file with no link left is one a hold keeps, listed on the orphan chain
    /// already.
    pub(crate) fn truncate_file(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        size: u64,
    ) -> Result<(), DriverError> {
        let bs = u64::from(self.layout.block_size);
        let current = inode_size_bytes(raw, &self.layout);
        if size > self.map_limit(raw).saturating_mul(bs) {
            return Err(DriverError::LengthOutOfRange);
        }
        set_size(raw, size);
        put_stamps(raw, self.now(), &[Stamp::Modify, Stamp::Change]);
        if size < current {
            let listed = le16(raw, INODE_LINKS) == 0;
            self.cut_blocks(ino, raw, size.div_ceil(bs), listed)?;
            self.zero_tail(ino, raw, size)?;
        }
        self.note_size(size);
        self.write_inode_raw(ino, raw)
    }

    /// Mark the volume as holding a file past 2 GiB once one reaches `size`,
    /// which a driver older than `large_file` must know before it writes.
    fn note_size(&mut self, size: u64) {
        if size > i32::MAX as u64 {
            make_dynamic(&mut self.sb);
            let ro = le32(&self.sb, SB_FEATURE_RO_COMPAT) | RO_COMPAT_LARGE_FILE;
            put_le32(&mut self.sb, SB_FEATURE_RO_COMPAT, ro);
        }
    }

    /// Zero the bytes past `size` in the block holding it, so a later
    /// extension reads zeros rather than what was cut.
    pub(crate) fn zero_tail(&mut self, ino: u32, raw: &[u8], size: u64) -> Result<(), DriverError> {
        let bs = u64::from(self.layout.block_size);
        let within = size % bs;
        if within == 0 {
            return Ok(());
        }
        if let Mapping::Mapped {
            phys,
            unwritten: false,
            ..
        } = self.map_lookup(ino, raw, size / bs)?
        {
            let zeros = scratch(usize_of(bs - within)?)?;
            self.write_data(phys, within, &zeros)?;
        }
        Ok(())
    }

    /// Free inode `ino`'s mapping from logical block `keep` on, a bounded
    /// pass per transaction. A cut needing more than one goes on the orphan
    /// chain first unless `listed` says it already is, and comes off it once
    /// done.
    pub(crate) fn cut_blocks(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        keep: u64,
        listed: bool,
    ) -> Result<(), DriverError> {
        let mut added = false;
        loop {
            let budget = self.room().map_or(CUT_BUDGET, |room| {
                u32::try_from(room / CUT_UNIT_COST)
                    .map_or(CUT_BUDGET, |units| units.clamp(1, CUT_BUDGET))
            });
            let pass = self.map_truncate(ino, raw, keep, budget)?;
            let freed = pass.data + pass.nodes;
            adjust_blocks(
                raw,
                &self.layout,
                -i64::try_from(freed).map_err(|_| DriverError::DeviceFault)?,
            )?;
            self.write_inode_raw(ino, raw)?;
            if pass.done {
                break;
            }
            if self.should_yield() {
                // The cut spans transactions from here: a crash between them
                // must find the inode listed, to be finished at mount.
                if !listed && !added {
                    self.orphan_add(ino, raw)?;
                    added = true;
                }
                self.commit_midway()?;
            }
        }
        if added {
            self.orphan_remove(ino, raw)?;
            self.write_inode_raw(ino, raw)?;
        }
        Ok(())
    }

    /// Delete inode `ino`, whose last name is gone: free its blocks and its
    /// slot, marked deleted so a checker sees a freed inode.
    pub(crate) fn release_inode(
        &mut self,
        ino: u32,
        raw: &mut [u8],
        listed: bool,
    ) -> Result<(), DriverError> {
        let is_dir = le16(raw, 0) & S_IFMT == S_IFDIR;
        put_le16(raw, INODE_LINKS, 0);
        if has_block_map(raw, &self.layout) {
            self.cut_blocks(ino, raw, 0, listed)?;
        }
        self.release_xattr_block(raw)?;
        if listed {
            self.orphan_remove(ino, raw)?;
        }
        set_size(raw, 0);
        put_le32(raw, INODE_DTIME, self.deletion_time(self.now()));
        self.write_inode_raw(ino, raw)?;
        self.free_inode(ino, is_dir)
    }
}
