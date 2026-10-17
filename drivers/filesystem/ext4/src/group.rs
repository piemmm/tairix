//! Block groups: descriptors, bitmaps, and the allocators over them.
//!
//! A group a lazily-initialised volume never used carries `BLOCK_UNINIT` or
//! `INODE_UNINIT` and no bitmap on disk; its bitmap is derived from the
//! layout the first time something is allocated there, checked against the
//! descriptor's free count, and only then made real — a derivation that
//! disagrees leaves the group unused rather than risk handing out a block
//! something owns.
//!
//! Blocks a transaction frees are released only when it commits: until then
//! the committed state still names them, so reusing one for file data —
//! written in place ahead of the commit — could overwrite contents a crash
//! would bring back.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::AllocationClass;
use tairix_abi::DriverError;

use crate::bytes::{
    crc16, crc32c_zeroed, high32, le16, le32, low16, low32, put_le16, put_le32, u32_le_truncate,
};
use crate::superblock::{self, SB_FREE_INODES};
use crate::Ext4;

/// `bg_flags`.
pub(crate) const BG_INODE_UNINIT: u16 = 0x0001;
pub(crate) const BG_BLOCK_UNINIT: u16 = 0x0002;

const GD_CHECKSUM: usize = 0x1E;
/// The part of a group descriptor this driver decodes: a `64bit` one's
/// second half holds the high words, and anything past it is padding.
const DECODED_DESC: usize = 64;

/// A group descriptor's checksum, taken over its bytes as they arrive.
#[derive(Copy, Clone)]
enum DescCrc {
    /// The volume keeps none.
    Unsummed,
    /// `gdt_csum`'s crc16, which leaves the checksum field out.
    Crc16(u16),
    /// `metadata_csum`'s crc32c, which counts the checksum field as zero.
    Crc32c(u32),
}

impl DescCrc {
    /// The descriptor's first bytes `head`, the checksum field among them.
    fn head(self, head: &[u8]) -> Self {
        match self {
            Self::Crc32c(c) => Self::Crc32c(crc32c_zeroed::<2>(c, head, GD_CHECKSUM)),
            other => other
                .update(head.get(..GD_CHECKSUM).unwrap_or(head))
                .update(head.get(GD_CHECKSUM + 2..).unwrap_or(&[])),
        }
    }

    /// More of the descriptor, past its checksum field.
    fn update(self, bytes: &[u8]) -> Self {
        match self {
            Self::Unsummed => self,
            Self::Crc16(c) => Self::Crc16(crc16(c, bytes)),
            Self::Crc32c(c) => Self::Crc32c(tairix_crc32c::update(c, bytes)),
        }
    }

    /// The checksum the descriptor should carry, or `None` where none is kept.
    fn finish(self) -> Option<u16> {
        match self {
            Self::Unsummed => None,
            Self::Crc16(c) => Some(c),
            Self::Crc32c(c) => Some(low16(c)),
        }
    }
}

/// The longest run one allocation hands out: the longest initialised extent.
pub(crate) const MAX_RUN: u32 = 32_768;

/// One group's descriptor, both halves of every wide field joined.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GroupDesc {
    pub(crate) block_bitmap: u64,
    pub(crate) inode_bitmap: u64,
    pub(crate) inode_table: u64,
    pub(crate) free_blocks: u32,
    pub(crate) free_inodes: u32,
    pub(crate) used_dirs: u32,
    pub(crate) flags: u16,
    pub(crate) itable_unused: u32,
    pub(crate) block_bitmap_csum: u32,
    pub(crate) inode_bitmap_csum: u32,
}

impl GroupDesc {
    fn decode(raw: &[u8], wide: bool) -> Self {
        let w16 = |lo: usize, hi: usize| {
            u32::from(le16(raw, lo))
                | if wide {
                    u32::from(le16(raw, hi)) << 16
                } else {
                    0
                }
        };
        let w32 = |lo: usize, hi: usize| {
            u64::from(le32(raw, lo))
                | if wide {
                    u64::from(le32(raw, hi)) << 32
                } else {
                    0
                }
        };
        Self {
            block_bitmap: w32(0x00, 0x20),
            inode_bitmap: w32(0x04, 0x24),
            inode_table: w32(0x08, 0x28),
            free_blocks: w16(0x0C, 0x2C),
            free_inodes: w16(0x0E, 0x2E),
            used_dirs: w16(0x10, 0x30),
            flags: le16(raw, 0x12),
            itable_unused: w16(0x1C, 0x32),
            block_bitmap_csum: w16(0x18, 0x38),
            inode_bitmap_csum: w16(0x1A, 0x3A),
        }
    }

    /// Store the fields this driver changes; the rest of `raw` stands.
    fn encode(&self, raw: &mut [u8], wide: bool) {
        let p16 = |raw: &mut [u8], lo: usize, hi: usize, v: u32| {
            put_le16(raw, lo, low16(v));
            if wide {
                put_le16(raw, hi, low16(v >> 16));
            }
        };
        let p32 = |raw: &mut [u8], lo: usize, hi: usize, v: u64| {
            put_le32(raw, lo, low32(v));
            if wide {
                put_le32(raw, hi, high32(v));
            }
        };
        p32(raw, 0x00, 0x20, self.block_bitmap);
        p32(raw, 0x04, 0x24, self.inode_bitmap);
        p32(raw, 0x08, 0x28, self.inode_table);
        p16(raw, 0x0C, 0x2C, self.free_blocks);
        p16(raw, 0x0E, 0x2E, self.free_inodes);
        p16(raw, 0x10, 0x30, self.used_dirs);
        put_le16(raw, 0x12, self.flags);
        p16(raw, 0x1C, 0x32, self.itable_unused);
        p16(raw, 0x18, 0x38, self.block_bitmap_csum);
        p16(raw, 0x1A, 0x3A, self.inode_bitmap_csum);
    }
}

/// The runs a transaction releases when it commits, in the order its
/// operations released them — an operation undone gives back its own — and
/// indexed by start, so a run released twice is found as it is asked for.
#[derive(Default)]
pub(crate) struct Releases {
    order: Vec<(u64, u64)>,
    by_start: BTreeMap<u64, u64>,
}

impl Releases {
    /// Room for one more run, so recording it cannot fail half-way.
    fn reserve(&mut self) -> Result<(), DriverError> {
        self.order.try_reserve(1).map_err(|_| DriverError::NoSpace)
    }

    fn push(&mut self, start: u64, len: u64) {
        self.order.push((start, len));
        self.by_start.insert(start, len);
    }

    /// Whether a run already released meets `start..end`. Released runs never
    /// meet, so only the last one starting below `end` can.
    pub(crate) fn overlaps(&self, start: u64, end: u64) -> bool {
        self.by_start
            .range(..end)
            .next_back()
            .is_some_and(|(&at, &len)| at + len > start)
    }

    /// The run released last.
    fn last(&self) -> Option<(u64, u64)> {
        self.order.last().copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Keep only the first `len` runs released.
    pub(crate) fn truncate(&mut self, len: usize) {
        for (start, _) in self.order.drain(len.min(self.order.len())..) {
            self.by_start.remove(&start);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.order.clear();
        self.by_start.clear();
    }

    /// Every run, by start.
    fn into_sorted(self) -> impl Iterator<Item = (u64, u64)> {
        self.by_start.into_iter()
    }
}

/// What the allocators remember between calls, all of it advisory.
#[derive(Default)]
pub(crate) struct Hints {
    /// Where the last block allocation landed.
    pub(crate) block_group: u64,
    /// Where the next directory goes, so directories spread across groups.
    pub(crate) dir_group: u64,
    /// One bit per descriptor block, set when every group it describes was
    /// last seen with no free block, so a nearly full volume is not
    /// rescanned group by group.
    full: Vec<u64>,
}

impl Hints {
    /// Forget which descriptor blocks were seen full: undoing allocations
    /// may have freed blocks in any of them.
    pub(crate) fn forget_full(&mut self) {
        self.full.clear();
    }

    fn is_full(&self, gdt: u64) -> bool {
        usize::try_from(gdt / 64)
            .ok()
            .and_then(|word| self.full.get(word))
            .is_some_and(|word| word & (1 << (gdt % 64)) != 0)
    }

    fn set_full(&mut self, gdt: u64, full: bool) {
        let Ok(word) = usize::try_from(gdt / 64) else {
            return;
        };
        if word >= self.full.len() {
            if !full {
                return;
            }
            if self.full.try_reserve(word + 1 - self.full.len()).is_err() {
                return;
            }
            self.full.resize(word + 1, 0);
        }
        if full {
            self.full[word] |= 1 << (gdt % 64);
        } else {
            self.full[word] &= !(1 << (gdt % 64));
        }
    }
}

/// The first zero bit of `bits` at or after `from` and below `end`.
fn first_clear(bits: &[u8], from: u64, end: u64) -> Option<u64> {
    let mut bit = from;
    while bit < end {
        let byte = (bit / 8) as usize;
        if bit.is_multiple_of(8) && bit + 64 <= end {
            if let Some(word) = bits.get(byte..byte + 8) {
                let word = u64::from_le_bytes([
                    word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7],
                ]);
                if word == u64::MAX {
                    bit += 64;
                    continue;
                }
                return Some(bit + u64::from((!word).trailing_zeros()));
            }
        }
        if bits.get(byte).is_some_and(|b| b & (1 << (bit % 8)) == 0) {
            return Some(bit);
        }
        bit += 1;
    }
    None
}

fn test_bit(bits: &[u8], bit: u64) -> bool {
    bits.get((bit / 8) as usize)
        .is_some_and(|b| b & (1 << (bit % 8)) != 0)
}

fn set_bit(bits: &mut [u8], bit: u64) {
    if let Some(b) = bits.get_mut((bit / 8) as usize) {
        *b |= 1 << (bit % 8);
    }
}

fn clear_bit(bits: &mut [u8], bit: u64) {
    if let Some(b) = bits.get_mut((bit / 8) as usize) {
        *b &= !(1 << (bit % 8));
    }
}

impl<B: Block> Ext4<B> {
    /// Inode-table blocks each group carries.
    pub(crate) fn itable_blocks(&self) -> u64 {
        (u64::from(self.layout.inodes_per_group) * u64::from(self.layout.inode_size))
            .div_ceil(u64::from(self.layout.block_size))
    }

    /// Begin group `group`'s descriptor checksum, before any of its bytes.
    fn desc_crc(&self, group: u64) -> DescCrc {
        let group_le = u32_le_truncate(group);
        if self.layout.metadata_csum {
            DescCrc::Crc32c(tairix_crc32c::update(self.layout.csum_seed, &group_le))
        } else if self.layout.gdt_csum {
            DescCrc::Crc16(crc16(crc16(0xFFFF, &self.layout.uuid), &group_le))
        } else {
            DescCrc::Unsummed
        }
    }

    /// Group `group`'s descriptor, its checksum verified. A staged block
    /// still carries the descriptors of groups the transaction never
    /// touched as the device held them, so those are checked too.
    ///
    /// Only the first [`DECODED_DESC`] bytes are decoded; the rest of a wider
    /// descriptor is summed where it lies, never copied.
    pub(crate) fn group_desc(&mut self, group: u64) -> Result<GroupDesc, DriverError> {
        if group >= self.layout.group_count {
            return Err(DriverError::DeviceFault);
        }
        let (block, off) = self.layout.desc_location(group);
        let len = self.layout.desc_size as usize;
        let head_len = len.min(DECODED_DESC);
        let mut head = [0u8; DECODED_DESC];
        let crc = if let Some(staged) = self.dirty.staged(block) {
            let desc = staged.get(off..off + len).ok_or(DriverError::DeviceFault)?;
            head[..head_len].copy_from_slice(&desc[..head_len]);
            self.desc_crc(group)
                .head(&desc[..head_len])
                .update(&desc[head_len..])
        } else {
            let at = block
                .checked_mul(u64::from(self.layout.block_size))
                .and_then(|base| base.checked_add(off as u64))
                .ok_or(DriverError::DeviceFault)?;
            self.dev.read_bytes(at, &mut head[..head_len])?;
            let mut crc = self.desc_crc(group).head(&head[..head_len]);
            let mut piece = [0u8; DECODED_DESC];
            let mut read = head_len;
            while read < len {
                let n = (len - read).min(DECODED_DESC);
                self.dev.read_bytes(at + read as u64, &mut piece[..n])?;
                crc = crc.update(&piece[..n]);
                read += n;
            }
            crc
        };
        if crc
            .finish()
            .is_some_and(|sum| le16(&head, GD_CHECKSUM) != sum)
        {
            return Err(DriverError::DeviceFault);
        }
        let desc = GroupDesc::decode(&head[..head_len], self.layout.wide_desc());
        let sane = |block: u64| block > 0 && block < self.layout.blocks_count;
        if !sane(desc.block_bitmap) || !sane(desc.inode_bitmap) || !sane(desc.inode_table) {
            return Err(DriverError::DeviceFault);
        }
        Ok(desc)
    }

    /// Store `desc` as group `group`'s descriptor in its staged block, its
    /// checksum restamped over the whole descriptor.
    fn put_group_desc(&mut self, group: u64, desc: &GroupDesc) -> Result<(), DriverError> {
        let (block, off) = self.layout.desc_location(group);
        let len = self.layout.desc_size as usize;
        let wide = self.layout.wide_desc();
        let crc = self.desc_crc(group);
        let raw = self
            .meta_mut(block)?
            .get_mut(off..off + len)
            .ok_or(DriverError::DeviceFault)?;
        desc.encode(raw, wide);
        if let Some(sum) = crc.head(raw).finish() {
            put_le16(raw, GD_CHECKSUM, sum);
        }
        Ok(())
    }

    /// The checksum `bitmap`'s descriptor records: crc32c over its `bits`
    /// under `metadata_csum`, and zero where the descriptor reserves the
    /// field.
    fn bitmap_checksum(&self, bitmap: &[u8], bits: u32) -> u32 {
        if !self.layout.metadata_csum {
            return 0;
        }
        let bytes = (bits as usize).div_ceil(8).min(bitmap.len());
        tairix_crc32c::update(self.layout.csum_seed, &bitmap[..bytes])
    }

    /// Whether a bitmap read from the device matches the checksum its
    /// descriptor records.
    fn bitmap_sound(&self, bitmap: &[u8], bits: u32, recorded: u32) -> bool {
        if !self.layout.metadata_csum {
            return true;
        }
        let sum = self.bitmap_checksum(bitmap, bits);
        let mask = if self.layout.wide_desc() {
            u32::MAX
        } else {
            0xFFFF
        };
        sum & mask == recorded & mask
    }

    /// The bitmap an uninitialised group's blocks would have: its superblock
    /// backup and descriptor tables, and its own bitmaps and inode table
    /// where they lie inside it, with the bits past its end set. Answers
    /// whether that agrees with the free count its descriptor records.
    fn derive_block_bitmap(&self, group: u64, desc: &GroupDesc, bitmap: &mut [u8]) -> bool {
        bitmap.fill(0);
        let first = self.layout.group_first(group);
        let n = self.layout.blocks_in_group(group);
        for bit in 0..self.layout.group_overhead(group).min(n) {
            set_bit(bitmap, bit);
        }
        let own = [
            (desc.block_bitmap, 1),
            (desc.inode_bitmap, 1),
            (desc.inode_table, self.itable_blocks()),
        ];
        for (start, len) in own {
            for block in start..start + len {
                if (first..first + n).contains(&block) {
                    set_bit(bitmap, block - first);
                }
            }
        }
        for bit in n..8 * bitmap.len() as u64 {
            set_bit(bitmap, bit);
        }
        let free = (0..n).filter(|&bit| !test_bit(bitmap, bit)).count() as u64;
        free == u64::from(desc.free_blocks)
    }

    /// Group `group`'s block bitmap, staged and writable, made real if the
    /// group was uninitialised. `None` when it cannot be trusted.
    fn block_bitmap_mut(
        &mut self,
        group: u64,
        desc: &mut GroupDesc,
    ) -> Result<Option<&mut [u8]>, DriverError> {
        let block = desc.block_bitmap;
        let bits = self.layout.blocks_per_group;
        if desc.flags & BG_BLOCK_UNINIT != 0 {
            let mut derived = crate::bytes::scratch(self.layout.block_size as usize)?;
            if !self.derive_block_bitmap(group, desc, &mut derived) {
                return Ok(None);
            }
            desc.flags &= !BG_BLOCK_UNINIT;
            desc.block_bitmap_csum = self.bitmap_checksum(&derived, bits);
            self.put_meta(block, &derived)?;
        } else if self.dirty.staged(block).is_none() {
            let mut loaded = crate::bytes::scratch(self.layout.block_size as usize)?;
            self.dev.read_blocks(block, &mut loaded)?;
            if !self.bitmap_sound(&loaded, bits, desc.block_bitmap_csum) {
                return Err(DriverError::DeviceFault);
            }
            self.put_meta(block, &loaded)?;
        }
        self.meta_mut(block).map(Some)
    }

    /// Group `group`'s inode bitmap, staged and writable, made real if the
    /// group was uninitialised.
    fn inode_bitmap_mut(&mut self, desc: &mut GroupDesc) -> Result<Option<&mut [u8]>, DriverError> {
        let block = desc.inode_bitmap;
        let ipg = self.layout.inodes_per_group;
        if desc.flags & BG_INODE_UNINIT != 0 {
            if desc.free_inodes != ipg {
                return Ok(None);
            }
            let mut derived = crate::bytes::scratch(self.layout.block_size as usize)?;
            for bit in u64::from(ipg)..8 * derived.len() as u64 {
                set_bit(&mut derived, bit);
            }
            desc.flags &= !BG_INODE_UNINIT;
            desc.itable_unused = ipg;
            desc.inode_bitmap_csum = self.bitmap_checksum(&derived, ipg);
            self.put_meta(block, &derived)?;
        } else if self.dirty.staged(block).is_none() {
            let mut loaded = crate::bytes::scratch(self.layout.block_size as usize)?;
            self.dev.read_blocks(block, &mut loaded)?;
            if !self.bitmap_sound(&loaded, ipg, desc.inode_bitmap_csum) {
                return Err(DriverError::DeviceFault);
            }
            self.put_meta(block, &loaded)?;
        }
        self.meta_mut(block).map(Some)
    }

    /// Set the superblock's free blocks and inodes to what the group
    /// descriptors count. Every allocation keeps the descriptors exact; a
    /// crash may leave the superblock's totals behind them, which a checker
    /// treats as hints, and so does this.
    pub(crate) fn recount_free(&mut self) -> Result<(), DriverError> {
        let (mut blocks, mut inodes) = (0u64, 0u32);
        for group in 0..self.layout.group_count {
            let desc = self.group_desc(group)?;
            blocks = blocks
                .checked_add(u64::from(desc.free_blocks))
                .ok_or(DriverError::DeviceFault)?;
            inodes = inodes
                .checked_add(desc.free_inodes)
                .ok_or(DriverError::DeviceFault)?;
        }
        if blocks > self.layout.blocks_count || inodes > self.layout.inodes_count {
            return Err(DriverError::DeviceFault);
        }
        if blocks != self.free_blocks_count() {
            self.set_free_blocks_count(blocks)?;
        }
        put_le32(&mut self.sb, SB_FREE_INODES, inodes);
        Ok(())
    }

    /// The volume's free blocks, as the running transaction counts them.
    pub(crate) fn free_blocks_count(&self) -> u64 {
        superblock::free_blocks(&self.sb, self.layout.wide_desc())
    }

    fn set_free_blocks_count(&mut self, free: u64) -> Result<(), DriverError> {
        superblock::set_free_blocks(&mut self.sb, self.layout.wide_desc(), free)
    }

    /// Blocks an allocation of `class` may still draw on.
    fn available_blocks(&self, class: AllocationClass) -> u64 {
        let free = self.free_blocks_count();
        match class {
            AllocationClass::System => free,
            AllocationClass::Ordinary => free.saturating_sub(superblock::reserved_blocks(
                &self.sb,
                self.layout.wide_desc(),
            )),
        }
    }

    /// Allocate up to `want` consecutive blocks near `goal`, every one below
    /// `below`, answering the first and how many.
    pub(crate) fn alloc_blocks(
        &mut self,
        goal: u64,
        want: u32,
        below: u64,
    ) -> Result<(u64, u32), DriverError> {
        let available = self.available_blocks(self.class);
        let want = u64::from(want.clamp(1, MAX_RUN)).min(available);
        if want == 0 {
            self.note_reserve(true);
            return Err(DriverError::NoSpace);
        }
        let layout_first = self.layout.first_data_block;
        let goal = if (layout_first..self.layout.blocks_count).contains(&goal) {
            goal
        } else {
            self.layout
                .group_first(self.hints.block_group.min(self.layout.group_count - 1))
        };
        let start = self.layout.group_of(goal);
        let groups = self.layout.group_count;
        let per = self.layout.descs_per_block();
        let tables = groups.div_ceil(per);
        // Walk descriptor blocks from the goal's, so a block every group of
        // which was seen full can be passed over whole on the next search.
        for t in 0..=tables {
            let table = (start / per + t) % tables;
            let lo = table * per;
            let hi = (lo + per).min(groups);
            let first_group = if t == 0 { start } else { lo };
            let last_group = if t == tables { start } else { hi };
            if t > 0 && self.hints.is_full(table) {
                continue;
            }
            // Only a table every group of which was seen with nothing free
            // is marked full; one an allocation's ceiling cut short is not.
            let mut all_full = first_group == lo && last_group == hi;
            for group in first_group..last_group {
                if self.layout.group_first(group) >= below {
                    all_full = false;
                    break;
                }
                let mut desc = self.group_desc(group)?;
                if desc.free_blocks == 0 {
                    continue;
                }
                all_full = false;
                if let Some(run) = self.take_run(group, &mut desc, goal, want, below)? {
                    return Ok(run);
                }
            }
            if all_full {
                self.hints.set_full(table, true);
            }
        }
        Err(DriverError::NoSpace)
    }

    /// Take up to `want` consecutive free blocks from group `group`, starting
    /// at `goal` when it lies inside the group.
    fn take_run(
        &mut self,
        group: u64,
        desc: &mut GroupDesc,
        goal: u64,
        want: u64,
        below: u64,
    ) -> Result<Option<(u64, u32)>, DriverError> {
        let first = self.layout.group_first(group);
        let end = self
            .layout
            .blocks_in_group(group)
            .min(below.saturating_sub(first));
        let from = if self.layout.group_of(goal) == group {
            goal - first
        } else {
            0
        };
        let bits = self.layout.blocks_per_group;
        let found = {
            let Some(bitmap) = self.block_bitmap_mut(group, desc)? else {
                return Ok(None);
            };
            let hit =
                first_clear(bitmap, from, end).or_else(|| first_clear(bitmap, 0, from.min(end)));
            hit.map(|bit| {
                let mut len = 0u64;
                while len < want && bit + len < end && !test_bit(bitmap, bit + len) {
                    set_bit(bitmap, bit + len);
                    len += 1;
                }
                (bit, len)
            })
        };
        let Some((bit, len)) = found else {
            return Ok(None);
        };
        let staged = self
            .dirty
            .staged(desc.block_bitmap)
            .ok_or(DriverError::DeviceFault)?;
        desc.block_bitmap_csum = self.bitmap_checksum(staged, bits);
        let taken = u32::try_from(len).map_err(|_| DriverError::DeviceFault)?;
        desc.free_blocks = desc
            .free_blocks
            .checked_sub(taken)
            .ok_or(DriverError::DeviceFault)?;
        self.put_group_desc(group, desc)?;
        let free = self
            .free_blocks_count()
            .checked_sub(len)
            .ok_or(DriverError::DeviceFault)?;
        self.set_free_blocks_count(free)?;
        self.hints.block_group = group;
        Ok(Some((first + bit, taken)))
    }

    /// Release `len` blocks from `start` when the running transaction
    /// commits; their staged copies, if any, are dropped now.
    ///
    /// The release is checked here, inside the operation making it, as the
    /// commit will apply it: a commit that refused it would take every
    /// operation of the transaction with it.
    pub(crate) fn free_blocks(&mut self, start: u64, len: u64) -> Result<(), DriverError> {
        let end = start.checked_add(len).ok_or(DriverError::DeviceFault)?;
        if len == 0 || start < self.layout.first_data_block || end > self.layout.blocks_count {
            return Err(DriverError::DeviceFault);
        }
        if self.freed.overlaps(start, end) {
            return Err(DriverError::DeviceFault);
        }
        self.check_allocated(start, end)?;
        self.freed.reserve()?;
        self.freed_cost = self
            .freed_cost
            .saturating_add(self.release_cost(start, end));
        self.forget_meta(start, len);
        self.freed.push(start, len);
        Ok(())
    }

    /// Refuse blocks `start..end` unless every one is allocated: none in a
    /// group never initialised, none already free.
    fn check_allocated(&mut self, start: u64, end: u64) -> Result<(), DriverError> {
        let mut at = start;
        while at < end {
            let group = self.layout.group_of(at);
            let first = self.layout.group_first(group);
            let stop = end.min(first + self.layout.blocks_in_group(group));
            let mut desc = self.group_desc(group)?;
            if desc.flags & BG_BLOCK_UNINIT != 0 {
                return Err(DriverError::DeviceFault);
            }
            let Some(bitmap) = self.block_bitmap_mut(group, &mut desc)? else {
                return Err(DriverError::DeviceFault);
            };
            if (at - first..stop - first).any(|bit| !test_bit(bitmap, bit)) {
                return Err(DriverError::DeviceFault);
            }
            at = stop;
        }
        Ok(())
    }

    /// Blocks releasing `start..end` will stage when the transaction commits:
    /// a bitmap and a descriptor per group it touches, less the group the
    /// previous release already ended in — a cut frees in file order, so
    /// neighbouring runs mostly share one.
    fn release_cost(&self, start: u64, end: u64) -> usize {
        let first = self.layout.group_of(start);
        let last = self.layout.group_of(end - 1);
        let shared = self
            .freed
            .last()
            .is_some_and(|(s, l)| self.layout.group_of(s + l - 1) == first);
        let groups = usize::try_from(last - first + 1).unwrap_or(usize::MAX);
        groups.saturating_sub(usize::from(shared)).saturating_mul(2)
    }

    /// Release every run the running transaction freed into the bitmaps and
    /// counts it is about to commit.
    pub(crate) fn apply_frees(&mut self) -> Result<(), DriverError> {
        let runs = core::mem::take(&mut self.freed).into_sorted();
        self.freed_cost = 0;
        let bits = self.layout.blocks_per_group;
        for (start, len) in runs {
            let mut at = start;
            let end = start + len;
            while at < end {
                let group = self.layout.group_of(at);
                let first = self.layout.group_first(group);
                let stop = end.min(first + self.layout.blocks_in_group(group));
                let mut desc = self.group_desc(group)?;
                if desc.flags & BG_BLOCK_UNINIT != 0 {
                    // Nothing in an uninitialised group was ever allocated.
                    return Err(DriverError::DeviceFault);
                }
                {
                    let Some(bitmap) = self.block_bitmap_mut(group, &mut desc)? else {
                        return Err(DriverError::DeviceFault);
                    };
                    for bit in at - first..stop - first {
                        if !test_bit(bitmap, bit) {
                            return Err(DriverError::DeviceFault);
                        }
                        clear_bit(bitmap, bit);
                    }
                }
                let staged = self
                    .dirty
                    .staged(desc.block_bitmap)
                    .ok_or(DriverError::DeviceFault)?;
                desc.block_bitmap_csum = self.bitmap_checksum(staged, bits);
                let n = u32::try_from(stop - at).map_err(|_| DriverError::DeviceFault)?;
                desc.free_blocks = desc
                    .free_blocks
                    .checked_add(n)
                    .ok_or(DriverError::DeviceFault)?;
                self.put_group_desc(group, &desc)?;
                let free = self
                    .free_blocks_count()
                    .checked_add(u64::from(n))
                    .ok_or(DriverError::DeviceFault)?;
                self.set_free_blocks_count(free)?;
                self.hints
                    .set_full(group / self.layout.descs_per_block(), false);
                at = stop;
            }
        }
        if self.available_blocks(AllocationClass::Ordinary) > 0 {
            self.note_reserve(false);
        }
        Ok(())
    }

    /// Allocate an inode for a child of `parent`: a file beside its
    /// directory, a directory in the next group round, so trees spread.
    pub(crate) fn alloc_inode(&mut self, parent: u32, is_dir: bool) -> Result<u32, DriverError> {
        if crate::bytes::le32(&self.sb, SB_FREE_INODES) == 0 {
            return Err(DriverError::NoSpace);
        }
        let ipg = u64::from(self.layout.inodes_per_group);
        let groups = self.layout.group_count;
        let start = if is_dir {
            self.hints.dir_group % groups
        } else {
            u64::from(parent.saturating_sub(1)) / ipg % groups
        };
        let tracked = self.layout.metadata_csum || self.layout.gdt_csum;
        for k in 0..groups {
            let group = (start + k) % groups;
            let mut desc = self.group_desc(group)?;
            if desc.free_inodes == 0 {
                continue;
            }
            // Inodes below `s_first_ino` are the format's reserved ones, in
            // whichever groups they fall.
            let lowest = (u64::from(self.layout.first_ino) - 1)
                .saturating_sub(group * ipg)
                .min(ipg);
            let found = {
                let Some(bitmap) = self.inode_bitmap_mut(&mut desc)? else {
                    continue;
                };
                let hit = first_clear(bitmap, lowest, ipg);
                if let Some(bit) = hit {
                    set_bit(bitmap, bit);
                }
                hit
            };
            let Some(bit) = found else {
                continue;
            };
            let staged = self
                .dirty
                .staged(desc.inode_bitmap)
                .ok_or(DriverError::DeviceFault)?;
            desc.inode_bitmap_csum = self.bitmap_checksum(staged, self.layout.inodes_per_group);
            desc.free_inodes -= 1;
            if is_dir {
                desc.used_dirs = desc
                    .used_dirs
                    .checked_add(1)
                    .ok_or(DriverError::DeviceFault)?;
                self.hints.dir_group = group + 1;
            }
            if tracked {
                let unused =
                    u32::try_from(ipg - (bit + 1)).map_err(|_| DriverError::DeviceFault)?;
                desc.itable_unused = desc.itable_unused.min(unused);
            }
            self.put_group_desc(group, &desc)?;
            let free = crate::bytes::le32(&self.sb, SB_FREE_INODES) - 1;
            put_le32(&mut self.sb, SB_FREE_INODES, free);
            return u32::try_from(group * ipg + bit + 1).map_err(|_| DriverError::DeviceFault);
        }
        Err(DriverError::NoSpace)
    }

    /// Release inode `ino` now: an inode slot is metadata, so its reuse
    /// inside the transaction is journaled like any other change.
    pub(crate) fn free_inode(&mut self, ino: u32, is_dir: bool) -> Result<(), DriverError> {
        let ipg = u64::from(self.layout.inodes_per_group);
        let index = u64::from(ino.checked_sub(1).ok_or(DriverError::DeviceFault)?);
        let (group, bit) = (index / ipg, index % ipg);
        let mut desc = self.group_desc(group)?;
        if desc.flags & BG_INODE_UNINIT != 0 {
            return Err(DriverError::DeviceFault);
        }
        {
            let Some(bitmap) = self.inode_bitmap_mut(&mut desc)? else {
                return Err(DriverError::DeviceFault);
            };
            if !test_bit(bitmap, bit) {
                return Err(DriverError::DeviceFault);
            }
            clear_bit(bitmap, bit);
        }
        let staged = self
            .dirty
            .staged(desc.inode_bitmap)
            .ok_or(DriverError::DeviceFault)?;
        desc.inode_bitmap_csum = self.bitmap_checksum(staged, self.layout.inodes_per_group);
        desc.free_inodes = desc
            .free_inodes
            .checked_add(1)
            .ok_or(DriverError::DeviceFault)?;
        if is_dir {
            desc.used_dirs = desc.used_dirs.saturating_sub(1);
        }
        self.put_group_desc(group, &desc)?;
        let free = crate::bytes::le32(&self.sb, SB_FREE_INODES)
            .checked_add(1)
            .ok_or(DriverError::DeviceFault)?;
        put_le32(&mut self.sb, SB_FREE_INODES, free);
        Ok(())
    }
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
