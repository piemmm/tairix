//! First-party ext4 formatter (mkfs).
//!
//! [`write_volume`] lays a fresh, empty, unjournaled ext4 volume onto a
//! [`Block`] device for [`crate::Ext4::open`] to mount: `filetype`,
//! `extent` and `extra_isize`, 256-byte inodes (whose enlarged record holds
//! nanosecond stamps, a birth time and the range to 2446), 32-byte group
//! descriptors, and every group materialised, so a volume can be filled to
//! exhaustion. Only group 0 carries a superblock and descriptor table,
//! which `sparse_super2` with no backup groups declares.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;

use crate::bytes::{align4, put_le16, put_le32, u16_of, u32_of, usize_of};
use crate::extent::EXTENT_MAGIC;
use crate::io::Device;
use crate::superblock::{
    COMPAT_SPARSE_SUPER2, EXT_MAGIC, INCOMPAT_EXTENTS, INCOMPAT_FILETYPE, RO_COMPAT_EXTRA_ISIZE,
    SUPERBLOCK_LEN, SUPERBLOCK_OFFSET,
};
use crate::{
    DIRENT_HEADER, FT_DIR, INODE_FLAG_EXTENTS, I_BLOCK_OFFSET, MAX_BLOCK_SIZE, NEW_EXTRA_ISIZE,
    ROOT_INODE, S_IFDIR,
};

/// On-disk inode record size the formatter writes: the classic 128 bytes
/// and the enlarged region the extended timestamps live in.
const INODE_SIZE: u32 = 256;

/// Group-descriptor record size (the legacy 32-byte descriptor; the
/// formatter never sets the `64bit` feature).
const DESC_SIZE: u32 = 32;

/// Minimum inodes per block group: enough for the reserved inodes
/// (1..=10) plus a usable remainder.
const MIN_INODES_PER_GROUP: u32 = 16;

/// First inode available to user files; inodes 1..=10 are reserved
/// (`s_first_ino`), with the root directory at inode 2.
const FIRST_INODE: u32 = 11;

/// Block-size threshold: volumes at least this large are formatted with
/// 4096-byte blocks, smaller ones with 1024-byte blocks. A 4096-byte
/// block group spans `8 * 4096 * 4096` bytes (128 MiB), so a smaller
/// volume could not host even one whole group at that block size.
const LARGE_VOLUME_BYTES: u64 = 128 * 1024 * 1024;

/// Computed geometry of the volume the formatter is about to write.
struct Plan {
    block_size: u32,
    blocks_count: u64,
    first_data_block: u64,
    blocks_per_group: u64,
    group_count: u64,
    inodes_per_group: u32,
    gdt_blocks: u64,
    inode_table_blocks: u64,
}

impl Plan {
    /// Total inode count across every group.
    fn total_inodes(&self) -> u64 {
        u64::from(self.inodes_per_group) * self.group_count
    }

    /// Absolute block number of group `group`'s first block.
    fn group_start(&self, group: u64) -> u64 {
        self.first_data_block + group * self.blocks_per_group
    }

    /// Blocks group 0 reserves for the superblock and the group-
    /// descriptor table before its bitmaps/inode-table begin.
    fn group0_meta_head(&self) -> u64 {
        1 + self.gdt_blocks
    }
}

/// Choose the geometry for a device of `total_bytes`, hosting at least
/// `inode_count` inodes. Returns [`DriverError::OutOfRange`] when the
/// device is too small for one full group plus a data region.
fn plan(total_bytes: u64, inode_count: u32) -> Result<Plan, DriverError> {
    if inode_count == 0 {
        return Err(DriverError::OutOfRange);
    }
    let block_size: u32 = if total_bytes >= LARGE_VOLUME_BYTES {
        4096
    } else {
        1024
    };
    let bs = u64::from(block_size);
    let dev_blocks = total_bytes / bs;
    let blocks_per_group = bs * 8;
    let group_count = dev_blocks / blocks_per_group;
    if group_count == 0 {
        return Err(DriverError::OutOfRange);
    }
    // Use whole groups only, so the reader's `ceil(blocks_count / bpg)`
    // group count matches and no degenerate tail group appears.
    let blocks_count = group_count * blocks_per_group;
    let first_data_block: u64 = u64::from(block_size == 1024);

    let gdt_bytes = group_count * u64::from(DESC_SIZE);
    let gdt_blocks = gdt_bytes.div_ceil(bs);

    let ipg_min = inode_count_per_group(inode_count, group_count);
    let inodes_per_group = clamp_inodes_per_group(ipg_min, block_size)?;
    let inode_table_blocks = (u64::from(inodes_per_group) * u64::from(INODE_SIZE)).div_ceil(bs);

    let plan = Plan {
        block_size,
        blocks_count,
        first_data_block,
        blocks_per_group,
        group_count,
        inodes_per_group,
        gdt_blocks,
        inode_table_blocks,
    };
    validate(&plan)?;
    Ok(plan)
}

/// Inodes per group needed to cover `inode_count` total, at least
/// [`MIN_INODES_PER_GROUP`], rounded up to a multiple of 8 so each
/// group's inode bitmap starts on a byte boundary.
fn inode_count_per_group(inode_count: u32, group_count: u64) -> u32 {
    let per = u64::from(inode_count).div_ceil(group_count);
    let per = per.max(u64::from(MIN_INODES_PER_GROUP));
    let rounded = per.div_ceil(8) * 8;
    u32::try_from(rounded).unwrap_or(u32::MAX)
}

/// Cap inodes-per-group to the inode bitmap's capacity (`8 * block_size`
/// bits) and ensure group 0 still holds the reserved inodes plus the
/// root.
fn clamp_inodes_per_group(ipg: u32, block_size: u32) -> Result<u32, DriverError> {
    let cap = block_size.saturating_mul(8);
    let ipg = ipg.min(cap);
    if ipg < FIRST_INODE {
        return Err(DriverError::OutOfRange);
    }
    Ok(ipg)
}

/// Ensure group 0 has room for its metadata head, both bitmaps, the
/// inode table, and at least one data block (the root directory).
fn validate(plan: &Plan) -> Result<(), DriverError> {
    let overhead = plan.group0_meta_head() + 2 + plan.inode_table_blocks;
    if overhead + 1 > plan.blocks_per_group {
        return Err(DriverError::OutOfRange);
    }
    let last_start = plan.group_start(plan.group_count - 1);
    let last_overhead = if plan.group_count == 1 {
        overhead
    } else {
        2 + plan.inode_table_blocks
    };
    if last_start + last_overhead + 1 > plan.blocks_count {
        return Err(DriverError::OutOfRange);
    }
    Ok(())
}

/// Set bit `bit` in a bitmap buffer (ext4 bitmaps are little-endian by
/// bit: bit `b` lives in byte `b / 8` at position `b % 8`).
fn set_bit(buf: &mut [u8], bit: usize) {
    if let Some(slot) = buf.get_mut(bit / 8) {
        *slot |= 1u8 << (bit % 8);
    }
}

/// Count the clear (free) bits in `buf` over the half-open bit range
/// `0..bits`.
fn count_free_bits(buf: &[u8], bits: usize) -> usize {
    let mut free = 0usize;
    for bit in 0..bits {
        if buf
            .get(bit / 8)
            .is_some_and(|b| b & (1u8 << (bit % 8)) == 0)
        {
            free += 1;
        }
    }
    free
}

/// Format a fresh ext4 volume onto `block`, sized from its geometry and
/// hosting at least `inode_count` inodes, stamped with the caller-minted
/// volume `uuid` (the reserved all-zero identity is refused — a volume
/// without an identity cannot be published), and hand the device back once
/// the volume is durable.
pub(crate) fn write_volume<B: Block>(
    block: B,
    inode_count: u32,
    uuid: [u8; 16],
) -> Result<B, DriverError> {
    if uuid == [0u8; 16] {
        return Err(DriverError::OutOfRange);
    }
    let mut dev = Device::new(block)?;
    let total_bytes = dev.bytes();
    let plan = plan(total_bytes, inode_count)?;
    dev.set_fs_block(plan.block_size);

    let mut free_blocks_total: u64 = 0;
    let mut free_inodes_total: u64 = 0;
    let bs = u64::from(plan.block_size);
    let bs_usize = usize_of(bs)?;
    let bpg = usize_of(plan.blocks_per_group)?;
    let itb = plan.inode_table_blocks;
    let ipg = usize_of(u64::from(plan.inodes_per_group))?;
    let reserved = usize_of(u64::from(FIRST_INODE - 1))?;

    // Zero the group-descriptor table region before stamping the live
    // descriptors, so unused slots and the block tail are clean.
    let gdt_start_block: u64 = if plan.block_size == 1024 { 2 } else { 1 };
    {
        let zero = [0u8; MAX_BLOCK_SIZE as usize];
        for b in 0..plan.gdt_blocks {
            dev.write_blocks(gdt_start_block + b, &zero[..bs_usize])?;
        }
    }

    for group in 0..plan.group_count {
        let group_start = plan.group_start(group);
        let meta_head = if group == 0 {
            plan.group0_meta_head()
        } else {
            0
        };
        let bbm_block = group_start + meta_head;
        let ibm_block = bbm_block + 1;
        let itable_block = ibm_block + 1;
        let first_data = itable_block + itb;
        // Relative-to-group bit of the first non-metadata block.
        let meta_bits = usize_of(first_data - group_start)?.min(bpg);

        // --- Block bitmap. ---
        let mut bbm = [0u8; MAX_BLOCK_SIZE as usize];
        for bit in 0..meta_bits {
            set_bit(&mut bbm, bit);
        }
        if group == 0 {
            // The root directory's single data block.
            set_bit(&mut bbm, meta_bits);
        }
        // Mark any in-bitmap bits that fall outside the volume as used.
        for bit in 0..bpg {
            if group_start + bit as u64 >= plan.blocks_count {
                set_bit(&mut bbm, bit);
            }
        }
        let group_free_blocks = count_free_bits(&bbm, bpg);
        free_blocks_total += group_free_blocks as u64;
        dev.write_blocks(bbm_block, &bbm[..bs_usize])?;

        // --- Inode bitmap. ---
        let mut ibm = [0u8; MAX_BLOCK_SIZE as usize];
        // Inodes beyond this group's count do not exist.
        for bit in ipg..(bs_usize * 8) {
            set_bit(&mut ibm, bit);
        }
        if group == 0 {
            // Reserved inodes 1..=10 (bits 0..=9), including the root.
            for bit in 0..reserved {
                set_bit(&mut ibm, bit);
            }
        }
        let group_free_inodes = count_free_bits(&ibm, ipg);
        free_inodes_total += group_free_inodes as u64;
        dev.write_blocks(ibm_block, &ibm[..bs_usize])?;

        // --- Inode table (zeroed; group 0 carries the root inode). ---
        let mut itable = [0u8; MAX_BLOCK_SIZE as usize];
        for b in 0..itb {
            for byte in &mut itable[..bs_usize] {
                *byte = 0;
            }
            if group == 0 && b == 0 {
                write_root_inode(&mut itable, plan.block_size, first_data)?;
            }
            dev.write_blocks(itable_block + b, &itable[..bs_usize])?;
        }

        // --- Group descriptor. ---
        let mut desc = [0u8; DESC_SIZE as usize];
        put_le32(&mut desc, 0x00, u32_of(bbm_block)?);
        put_le32(&mut desc, 0x04, u32_of(ibm_block)?);
        put_le32(&mut desc, 0x08, u32_of(itable_block)?);
        put_le16(&mut desc, 0x0C, u16_of(group_free_blocks)?);
        put_le16(&mut desc, 0x0E, u16_of(group_free_inodes)?);
        put_le16(&mut desc, 0x10, u16::from(group == 0));
        let desc_off = gdt_start_block * bs + group * u64::from(DESC_SIZE);
        dev.write_bytes(desc_off, &desc)?;

        // --- Root directory data block (group 0). ---
        if group == 0 {
            let mut root = [0u8; MAX_BLOCK_SIZE as usize];
            write_root_dir_block(&mut root, plan.block_size)?;
            dev.write_blocks(first_data, &root[..bs_usize])?;
        }
    }

    write_superblock(&mut dev, &plan, free_blocks_total, free_inodes_total, uuid)?;
    dev.flush()?;
    Ok(dev.into_inner())
}

/// Stamp the root directory inode (#2) into the first inode-table block
/// at its in-block offset (one inode record in).
fn write_root_inode(
    itable: &mut [u8],
    block_size: u32,
    root_data_block: u64,
) -> Result<(), DriverError> {
    let base = usize_of(u64::from(INODE_SIZE))?; // inode 2 sits at index 1
    put_le16(itable, base, S_IFDIR | 0o755);
    put_le32(itable, base + 0x04, block_size); // i_size_lo
    put_le16(itable, base + 0x1A, 2); // i_links_count ("." + child "..")
    put_le32(itable, base + 0x1C, block_size / 512); // i_blocks_lo (sectors)
    put_le32(itable, base + 0x20, INODE_FLAG_EXTENTS); // i_flags
    put_le16(itable, base + crate::inode::EXTRA_ISIZE, NEW_EXTRA_ISIZE);

    // Extent-tree root in i_block: header + one extent covering logical
    // block 0.
    let ib = base + I_BLOCK_OFFSET;
    put_le16(itable, ib, EXTENT_MAGIC);
    put_le16(itable, ib + 2, 1); // eh_entries
    put_le16(itable, ib + 4, 4); // eh_max
    put_le16(itable, ib + 6, 0); // eh_depth
    put_le32(itable, ib + 8, 0); // eh_generation
    put_le32(itable, ib + 12, 0); // ee_block
    put_le16(itable, ib + 16, 1); // ee_len
    put_le16(itable, ib + 18, 0); // ee_start_hi
    put_le32(itable, ib + 20, u32_of(root_data_block)?); // ee_start_lo
    Ok(())
}

/// Write the empty root directory's data block: a "." and a ".." entry,
/// the latter filling the rest of the block.
fn write_root_dir_block(block: &mut [u8], block_size: u32) -> Result<(), DriverError> {
    let bs = usize_of(u64::from(block_size))?;
    // "." → root, rec_len 12.
    let dot_len = align4(DIRENT_HEADER + 1);
    put_le32(block, 0x00, ROOT_INODE);
    put_le16(block, 0x04, u16_of(dot_len)?);
    block[0x06] = 1; // name_len
    block[0x07] = FT_DIR;
    block[DIRENT_HEADER] = b'.';
    // ".." → root, rec_len covers the rest of the block.
    let pos = dot_len;
    put_le32(block, pos, ROOT_INODE);
    put_le16(block, pos + 0x04, u16_of(bs - pos)?);
    block[pos + 0x06] = 2; // name_len
    block[pos + 0x07] = FT_DIR;
    block[pos + DIRENT_HEADER] = b'.';
    block[pos + DIRENT_HEADER + 1] = b'.';
    Ok(())
}

/// Write the 1024-byte superblock at its fixed device offset.
fn write_superblock<B: Block>(
    dev: &mut Device<B>,
    plan: &Plan,
    free_blocks: u64,
    free_inodes: u64,
    uuid: [u8; 16],
) -> Result<(), DriverError> {
    let mut sb = [0u8; SUPERBLOCK_LEN];
    let log_block_size = plan.block_size.trailing_zeros() - 10; // log2(bs/1024)
    put_le32(&mut sb, 0x00, u32_of(plan.total_inodes())?); // s_inodes_count
    put_le32(&mut sb, 0x04, u32_of(plan.blocks_count)?); // s_blocks_count_lo
    put_le32(&mut sb, 0x0C, u32_of(free_blocks)?); // s_free_blocks_count_lo
    put_le32(&mut sb, 0x10, u32_of(free_inodes)?); // s_free_inodes_count
    put_le32(&mut sb, 0x14, u32_of(plan.first_data_block)?); // s_first_data_block
    put_le32(&mut sb, 0x18, log_block_size); // s_log_block_size
    put_le32(&mut sb, 0x1C, log_block_size); // s_log_cluster_size
    put_le32(&mut sb, 0x20, u32_of(plan.blocks_per_group)?); // s_blocks_per_group
    put_le32(&mut sb, 0x24, u32_of(plan.blocks_per_group)?); // s_clusters_per_group
    put_le32(&mut sb, 0x28, plan.inodes_per_group); // s_inodes_per_group
    put_le16(&mut sb, 0x38, EXT_MAGIC); // s_magic
    put_le16(&mut sb, 0x3A, 1); // s_state = clean
    put_le16(&mut sb, 0x3C, 1); // s_errors = continue
    put_le32(&mut sb, 0x4C, 1); // s_rev_level = dynamic
    put_le32(&mut sb, 0x54, FIRST_INODE); // s_first_ino
    put_le16(&mut sb, 0x58, u16_of(usize_of(u64::from(INODE_SIZE))?)?); // s_inode_size
                                                                        // No backup superblock or descriptor table: `s_backup_bgs` stays zero.
    put_le32(&mut sb, 0x5C, COMPAT_SPARSE_SUPER2); // s_feature_compat
    put_le32(&mut sb, 0x60, INCOMPAT_FILETYPE | INCOMPAT_EXTENTS); // s_feature_incompat
    put_le32(&mut sb, 0x64, RO_COMPAT_EXTRA_ISIZE); // s_feature_ro_compat
    sb[0x68..0x78].copy_from_slice(&uuid); // s_uuid
    put_le16(&mut sb, 0x15C, NEW_EXTRA_ISIZE); // s_min_extra_isize
    put_le16(&mut sb, 0x15E, NEW_EXTRA_ISIZE); // s_want_extra_isize
    dev.write_bytes(SUPERBLOCK_OFFSET, &sb)
}
