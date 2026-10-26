use super::*;

use crate::bytes::{put_le16, put_le32};

/// A valid superblock for `groups` groups of 8192 blocks of 1 KiB, 2048
/// inodes each, 256-byte inodes.
fn superblock(groups: u32) -> [u8; SUPERBLOCK_LEN] {
    let mut sb = [0u8; SUPERBLOCK_LEN];
    put_le32(&mut sb, SB_INODES_COUNT, groups * 2048);
    put_le32(&mut sb, SB_BLOCKS_LO, groups * 8192 + 1);
    put_le32(&mut sb, SB_FIRST_DATA_BLOCK, 1);
    put_le32(&mut sb, SB_BLOCKS_PER_GROUP, 8192);
    put_le32(&mut sb, SB_CLUSTERS_PER_GROUP, 8192);
    put_le32(&mut sb, SB_INODES_PER_GROUP, 2048);
    put_le16(&mut sb, SB_MAGIC, EXT_MAGIC);
    put_le16(&mut sb, SB_STATE, STATE_VALID);
    put_le32(&mut sb, SB_REV_LEVEL, 1);
    put_le32(&mut sb, SB_FIRST_INO, 11);
    put_le16(&mut sb, SB_INODE_SIZE, 256);
    put_le32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        INCOMPAT_FILETYPE | INCOMPAT_EXTENTS,
    );
    put_le32(&mut sb, SB_FEATURE_RO_COMPAT, RO_COMPAT_SPARSE_SUPER);
    sb[SB_UUID..SB_UUID + 16].copy_from_slice(&[0x42; 16]);
    sb
}

const BIG: u64 = 1 << 40;

#[test]
fn a_plain_ext4_superblock_parses_writable() {
    let layout = parse(&superblock(4), BIG).expect("parses");
    assert_eq!(layout.block_size, 1024);
    assert_eq!(layout.group_count, 4);
    assert_eq!(layout.inodes_count, 8192);
    assert!(layout.extents && layout.filetype && layout.sparse_super);
    assert_eq!(layout.no_write, None);
    assert_eq!(
        layout.csum_seed,
        tairix_crc32c::update(0xFFFF_FFFF, &[0x42; 16])
    );
}

#[test]
fn structural_damage_is_bad_magic() {
    let mut sb = superblock(1);
    put_le16(&mut sb, SB_MAGIC, 0x1234);
    assert_eq!(parse(&sb, BIG).err(), Some(DriverError::BadMagic));
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_INODES_COUNT, 7);
    assert_eq!(
        parse(&sb, BIG).err(),
        Some(DriverError::BadMagic),
        "inodes disagree with groups"
    );
    assert_eq!(
        parse(&superblock(1), 1024).err(),
        Some(DriverError::BadMagic),
        "a volume larger than its device"
    );
    assert_eq!(
        parse(&superblock(1)[..512], BIG).err(),
        Some(DriverError::BadMagic)
    );
}

#[test]
fn an_incompatible_feature_the_reader_lacks_refuses_the_mount() {
    for unknown in [0x8000_0000u32, INCOMPAT_COMPRESSION, INCOMPAT_JOURNAL_DEV] {
        let mut sb = superblock(1);
        put_le32(&mut sb, SB_FEATURE_INCOMPAT, INCOMPAT_FILETYPE | unknown);
        assert_eq!(
            parse(&sb, BIG).err(),
            Some(DriverError::Unsupported),
            "{unknown:#x}"
        );
    }
}

#[test]
fn a_feature_the_writer_lacks_mounts_read_only() {
    let mut sb = superblock(1);
    put_le32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        INCOMPAT_FILETYPE | INCOMPAT_INLINE_DATA,
    );
    assert_eq!(
        parse(&sb, BIG).map(|l| l.no_write),
        Ok(Some(NoWrite::Feature))
    );
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_FEATURE_RO_COMPAT, 0x0200);
    assert_eq!(
        parse(&sb, BIG).map(|l| l.no_write),
        Ok(Some(NoWrite::Feature)),
        "bigalloc"
    );
    let mut sb = superblock(1);
    put_le16(&mut sb, SB_STATE, STATE_VALID | STATE_ERROR);
    assert_eq!(
        parse(&sb, BIG).map(|l| l.no_write),
        Ok(Some(NoWrite::Errors))
    );
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_CREATOR_OS, OS_HURD);
    assert_eq!(parse(&sb, BIG).map(|l| l.no_write), Ok(Some(NoWrite::Hurd)));
}

#[test]
fn a_metadata_checksum_is_verified_and_its_seed_honoured() {
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_FEATURE_RO_COMPAT, RO_COMPAT_METADATA_CSUM);
    sb[SB_CHECKSUM_TYPE] = CHECKSUM_CRC32C;
    stamp(&mut sb);
    assert!(parse(&sb, BIG).is_ok());
    sb[SB_UUID] ^= 1;
    assert_eq!(parse(&sb, BIG).err(), Some(DriverError::DeviceFault));

    let mut sb = superblock(1);
    put_le32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        INCOMPAT_FILETYPE | INCOMPAT_CSUM_SEED,
    );
    put_le32(&mut sb, SB_FEATURE_RO_COMPAT, RO_COMPAT_METADATA_CSUM);
    put_le32(&mut sb, SB_CHECKSUM_SEED, 0xDEAD_BEEF);
    sb[SB_CHECKSUM_TYPE] = CHECKSUM_CRC32C;
    stamp(&mut sb);
    let layout = parse(&sb, BIG).expect("a recorded seed is accepted");
    assert_eq!(
        layout.csum_seed, 0xDEAD_BEEF,
        "the seed, not the uuid's checksum"
    );
    assert_eq!(layout.no_write, None);
}

#[test]
fn an_external_journal_is_refused() {
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_FEATURE_COMPAT, COMPAT_HAS_JOURNAL);
    put_le32(&mut sb, SB_JOURNAL_INUM, 8);
    put_le32(&mut sb, SB_JOURNAL_DEV, 0x0801);
    assert_eq!(parse(&sb, BIG).err(), Some(DriverError::Unsupported));
}

#[test]
fn a_64bit_volume_reads_both_halves_of_its_counts() {
    let mut sb = superblock(1);
    put_le32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        INCOMPAT_FILETYPE | INCOMPAT_64BIT,
    );
    put_le16(&mut sb, SB_DESC_SIZE, 64);
    put_le32(&mut sb, SB_FREE_BLOCKS_LO, 5);
    put_le32(&mut sb, SB_FREE_BLOCKS_HI, 2);
    put_le32(&mut sb, SB_R_BLOCKS_HI, 1);
    let layout = parse(&sb, BIG).expect("parses");
    assert!(layout.wide_desc());
    assert_eq!(free_blocks(&sb, true), (2 << 32) | 5);
    assert_eq!(reserved_blocks(&sb, true), 1 << 32);
    assert_eq!(
        free_blocks(&sb, false),
        5,
        "a narrow volume has no high half"
    );
    let mut narrow = superblock(1);
    assert_eq!(
        set_free_blocks(&mut narrow, false, 1 << 32),
        Err(DriverError::DeviceFault)
    );
    set_free_blocks(&mut sb, true, (7 << 32) | 9).expect("wide");
    assert_eq!(free_blocks(&sb, true), (7 << 32) | 9);
}

#[test]
fn sparse_super_keeps_backups_in_powers_of_three_five_and_seven() {
    let layout = parse(&superblock(64), BIG).expect("parses");
    let backups: alloc::vec::Vec<u64> = (0..64).filter(|&g| layout.has_super(g)).collect();
    assert_eq!(backups, [0, 1, 3, 5, 7, 9, 25, 27, 49]);
    let mut sb = superblock(64);
    put_le32(&mut sb, SB_FEATURE_RO_COMPAT, 0);
    let dense = parse(&sb, BIG).expect("parses");
    assert!(
        (0..64).all(|g| dense.has_super(g)),
        "every group without sparse_super"
    );
}

#[test]
fn sparse_super2_names_its_backup_groups() {
    let mut sb = superblock(16);
    put_le32(&mut sb, SB_FEATURE_COMPAT, COMPAT_SPARSE_SUPER2);
    put_le32(&mut sb, SB_BACKUP_BGS, 1);
    put_le32(&mut sb, SB_BACKUP_BGS + 4, 15);
    let layout = parse(&sb, BIG).expect("parses");
    let backups: alloc::vec::Vec<u64> = (0..16).filter(|&g| layout.has_super(g)).collect();
    assert_eq!(backups, [0, 1, 15]);
    put_le32(&mut sb, SB_BACKUP_BGS, 0);
    put_le32(&mut sb, SB_BACKUP_BGS + 4, 0);
    let none = parse(&sb, BIG).expect("parses");
    assert!((1..16).all(|g| !none.has_super(g)), "no backups at all");
}

#[test]
fn meta_bg_places_descriptors_in_each_meta_groups_leader() {
    // 1 KiB blocks hold 32 descriptors: meta-group `m` covers groups
    // 32m..32m+31.
    let mut sb = superblock(96);
    put_le32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        INCOMPAT_FILETYPE | INCOMPAT_META_BG,
    );
    put_le32(&mut sb, SB_FIRST_META_BG, 1);
    let layout = parse(&sb, BIG).expect("parses");
    assert_eq!(
        layout.desc_location(5),
        (2, 5 * 32),
        "below first_meta_bg: the table"
    );
    // Group 32 leads meta-group 1 and is not a sparse backup group, so its
    // descriptor block is its own first block.
    assert_eq!(layout.desc_location(33), (layout.group_first(32), 32));
    // Group 64 leads meta-group 2 and is not a backup either.
    assert_eq!(layout.desc_location(64), (layout.group_first(64), 0));
    // The leader, the next group and the last of the meta-group each carry
    // a copy of its descriptor block.
    assert_eq!(layout.group_overhead(32), 1);
    assert_eq!(layout.group_overhead(33), 1);
    assert_eq!(layout.group_overhead(63), 1);
    assert_eq!(layout.group_overhead(40), 0);
    // Group 1 is a sparse backup and still holds the traditional table's
    // first_meta_bg blocks.
    assert_eq!(layout.group_overhead(1), 2);
}

#[test]
fn the_last_group_is_short() {
    let mut sb = superblock(2);
    put_le32(&mut sb, SB_BLOCKS_LO, 8192 + 100 + 1);
    let layout = parse(&sb, BIG).expect("parses");
    assert_eq!(layout.blocks_in_group(0), 8192);
    assert_eq!(layout.blocks_in_group(1), 100);
    assert_eq!(layout.group_of(8192 + 50), 1);
}

#[test]
fn a_bigalloc_bitmap_counts_clusters_not_blocks() {
    // Sixteen-block clusters: 8192 clusters a group is 131072 blocks, more
    // than one bitmap block's bits, which only a cluster count may not be.
    let mut sb = superblock(1);
    put_le32(
        &mut sb,
        SB_FEATURE_RO_COMPAT,
        RO_COMPAT_SPARSE_SUPER | RO_COMPAT_BIGALLOC,
    );
    put_le32(&mut sb, SB_LOG_CLUSTER_SIZE, 4);
    put_le32(&mut sb, SB_BLOCKS_PER_GROUP, 8192 * 16);
    put_le32(&mut sb, SB_BLOCKS_LO, 8192 * 16 + 1);
    let layout = parse(&sb, BIG).expect("a bigalloc volume reads");
    assert_eq!(layout.cluster_size, 16 * 1024);
    assert_eq!(
        layout.no_write,
        Some(NoWrite::Feature),
        "and is not written"
    );
    put_le32(&mut sb, SB_CLUSTERS_PER_GROUP, 8192 * 16);
    assert_eq!(
        parse(&sb, BIG).err(),
        Some(DriverError::BadMagic),
        "clusters past a bitmap"
    );
}

/// A cluster field past what a `u32` shift can carry wrapped to a zero-sized
/// cluster, and a zero-block group then divided by zero: refused instead.
#[test]
fn a_cluster_or_group_of_no_blocks_is_refused_rather_than_divided_by() {
    for log_cluster in [22, 31, 17, u32::MAX] {
        let mut sb = superblock(1);
        put_le32(
            &mut sb,
            SB_FEATURE_RO_COMPAT,
            RO_COMPAT_SPARSE_SUPER | RO_COMPAT_BIGALLOC,
        );
        put_le32(&mut sb, SB_LOG_CLUSTER_SIZE, log_cluster);
        put_le32(&mut sb, SB_BLOCKS_PER_GROUP, 0);
        put_le32(&mut sb, SB_CLUSTERS_PER_GROUP, 1);
        assert_eq!(
            parse(&sb, BIG).err(),
            Some(DriverError::BadMagic),
            "{log_cluster}"
        );
    }
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_BLOCKS_PER_GROUP, 0);
    assert_eq!(parse(&sb, BIG).err(), Some(DriverError::BadMagic));
}

/// Without `bigalloc` the cluster field is not a cluster size.
#[test]
fn a_stray_cluster_field_without_bigalloc_is_ignored() {
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_LOG_CLUSTER_SIZE, 4);
    let layout = parse(&sb, BIG).expect("parses");
    assert_eq!(layout.cluster_size, layout.block_size);
}

/// A `64bit` descriptor carries the high words of its block numbers in its
/// second half, so one shorter than 64 bytes is refused, and one wider than
/// the 64 this driver decodes is still read whole.
#[test]
fn a_64bit_descriptor_is_between_64_and_1024_bytes() {
    let wide = |desc_size: u16| {
        let mut sb = superblock(1);
        put_le32(
            &mut sb,
            SB_FEATURE_INCOMPAT,
            INCOMPAT_FILETYPE | INCOMPAT_64BIT,
        );
        put_le16(&mut sb, SB_DESC_SIZE, desc_size);
        parse(&sb, BIG)
    };
    for refused in [0, 32, 48, 2048] {
        assert_eq!(
            wide(refused).err(),
            Some(DriverError::BadMagic),
            "{refused}"
        );
    }
    for accepted in [64, 128, 1024] {
        assert_eq!(
            wide(accepted).expect("parses").desc_size,
            u32::from(accepted)
        );
    }
}

/// A device reporting a capacity past what the inode count can reach once
/// multiplied out is refused, not overflowed.
#[test]
fn an_inode_count_that_overflows_its_groups_is_refused() {
    let mut sb = superblock(1);
    put_le32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        INCOMPAT_FILETYPE | INCOMPAT_64BIT,
    );
    put_le16(&mut sb, SB_DESC_SIZE, 64);
    put_le32(&mut sb, SB_BLOCKS_PER_GROUP, 1);
    put_le32(&mut sb, SB_CLUSTERS_PER_GROUP, 1);
    // 2^54 - 1 one-block groups fit a 2^64-byte device, but 2048 inodes
    // each do not fit a count.
    put_le32(&mut sb, SB_BLOCKS_LO, u32::MAX);
    put_le32(&mut sb, SB_BLOCKS_HI, (1 << 22) - 1);
    assert_eq!(parse(&sb, u64::MAX).err(), Some(DriverError::BadMagic));
}

/// A meta-group the volume ends inside keeps copies only at its leader and
/// the group after it, as `mke2fs` lays them: none in the volume's last group.
#[test]
fn a_meta_group_cut_short_has_no_third_descriptor_copy() {
    // Forty groups of 32-descriptor blocks: meta-group 1 is groups 32 to 39.
    let mut sb = superblock(40);
    put_le32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        INCOMPAT_FILETYPE | INCOMPAT_META_BG,
    );
    put_le32(&mut sb, SB_FIRST_META_BG, 1);
    let layout = parse(&sb, BIG).expect("parses");
    assert_eq!(layout.group_overhead(32), 1);
    assert_eq!(layout.group_overhead(33), 1);
    assert_eq!(layout.group_overhead(39), 0, "the volume's last group");
}

#[test]
fn a_volume_is_ext4_once_any_feature_past_ext3s_is_on() {
    let ext3_ro = RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE | RO_COMPAT_BTREE_DIR;
    let ext3_incompat = INCOMPAT_FILETYPE | INCOMPAT_RECOVER | INCOMPAT_META_BG;
    assert_eq!(
        Generation::of(COMPAT_DIR_INDEX, INCOMPAT_FILETYPE, ext3_ro),
        Generation::Ext2
    );
    assert_eq!(
        Generation::of(COMPAT_HAS_JOURNAL, ext3_incompat, ext3_ro),
        Generation::Ext3
    );
    assert_eq!(
        Generation::of(COMPAT_SPARSE_SUPER2 | COMPAT_ORPHAN_FILE, 0, 0),
        Generation::Ext2,
        "a compatible feature leaves an older driver mounting the volume"
    );
    for compat in [0, COMPAT_HAS_JOURNAL] {
        for incompat in [
            INCOMPAT_COMPRESSION,
            INCOMPAT_JOURNAL_DEV,
            INCOMPAT_EXTENTS,
            INCOMPAT_64BIT,
            INCOMPAT_FLEX_BG,
            INCOMPAT_INLINE_DATA,
        ] {
            assert_eq!(
                Generation::of(compat, INCOMPAT_FILETYPE | incompat, 0),
                Generation::Ext4,
                "{incompat:#x}"
            );
        }
        for ro in [
            RO_COMPAT_HUGE_FILE,
            RO_COMPAT_GDT_CSUM,
            RO_COMPAT_DIR_NLINK,
            RO_COMPAT_EXTRA_ISIZE,
            RO_COMPAT_METADATA_CSUM,
        ] {
            assert_eq!(
                Generation::of(compat, 0, RO_COMPAT_SPARSE_SUPER | ro),
                Generation::Ext4,
                "{ro:#x}"
            );
        }
    }
    assert_eq!(
        [Generation::Ext2, Generation::Ext3, Generation::Ext4]
            .map(|generation| generation.format().as_str()),
        ["ext2", "ext3", "ext4"]
    );
}

#[test]
fn a_revision_0_superblock_is_raised_with_the_values_revision_0_fixes() {
    let mut sb = superblock(1);
    put_le32(&mut sb, SB_REV_LEVEL, 0);
    put_le32(&mut sb, SB_FEATURE_INCOMPAT, 0);
    put_le32(&mut sb, SB_FEATURE_RO_COMPAT, 0);
    sb[SB_FIRST_INO..SB_INODE_SIZE + 2].fill(0);
    let layout = parse(&sb, BIG).expect("a revision-0 volume parses");
    assert_eq!((layout.inode_size, layout.first_ino), (128, 11));
    assert_eq!(layout.generation, Generation::Ext2);
    make_dynamic(&mut sb);
    assert_eq!(le32(&sb, SB_REV_LEVEL), 1);
    assert_eq!(le32(&sb, SB_FIRST_INO), 11);
    assert_eq!(le16(&sb, SB_INODE_SIZE), 128);
    let raised = parse(&sb, BIG).expect("the raised volume parses");
    assert_eq!((raised.inode_size, raised.first_ino), (128, 11));
    let before = sb;
    make_dynamic(&mut sb);
    assert_eq!(sb, before, "a dynamic superblock is left as it is");
}
