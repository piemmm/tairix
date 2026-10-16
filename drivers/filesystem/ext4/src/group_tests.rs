use super::*;

use alloc::vec;

use crate::tests::{sparse, SizedBlock};

/// `mke2fs -t ext4 -b 1024 -N 2048 -O metadata_csum,^has_journal,^resize_inode
/// -E lazy_itable_init=1` over 64 MiB (e2fsprogs 1.47.0): eight groups of
/// 8192 blocks, every bitmap in group 0 (`flex_bg`), groups 1 to 6 left
/// `BLOCK_UNINIT` and 1 to 7 `INODE_UNINIT`, backups in groups 1, 3, 5, 7.
const UNINIT: &[u8] = include_bytes!("../tests/fixtures/uninit.sparse");

fn volume() -> Ext4<SizedBlock> {
    Ext4::open(SizedBlock::over(sparse(UNINIT))).expect("an e2fsprogs volume")
}

#[test]
fn an_uninitialised_group_is_derived_checked_and_made_real() {
    let mut fs = volume();
    let before = fs.group_desc(2).expect("descriptor");
    assert_ne!(before.flags & BG_BLOCK_UNINIT, 0);
    let goal = fs.layout.group_first(2) + 10;
    let (first, len) = fs
        .alloc_blocks(goal, 5, u64::MAX)
        .expect("space in group 2");
    assert_eq!((first, len), (goal, 5), "at the goal, in one run");
    let after = fs.group_desc(2).expect("descriptor");
    assert_eq!(after.flags & BG_BLOCK_UNINIT, 0, "made real");
    assert_eq!(after.free_blocks, before.free_blocks - 5);
    let mut bitmap = vec![0u8; 1024];
    fs.read_meta(after.block_bitmap, &mut bitmap)
        .expect("staged");
    let set: alloc::vec::Vec<u64> = (0..8192).filter(|&bit| test_bit(&bitmap, bit)).collect();
    assert_eq!(
        set,
        [10, 11, 12, 13, 14],
        "a group with no overhead derives empty"
    );
}

#[test]
fn a_backup_groups_derived_bitmap_carries_its_superblock_copy() {
    let mut fs = volume();
    let mut desc = fs.group_desc(3).expect("descriptor");
    let mut bitmap = vec![0u8; 1024];
    assert!(
        fs.derive_block_bitmap(3, &desc, &mut bitmap),
        "agrees with mke2fs"
    );
    assert!(
        test_bit(&bitmap, 0) && test_bit(&bitmap, 1),
        "superblock and table copy"
    );
    assert!(!test_bit(&bitmap, 2));
    desc.free_blocks += 1;
    assert!(
        !fs.derive_block_bitmap(3, &desc, &mut bitmap),
        "a count that disagrees"
    );
}

#[test]
fn a_group_whose_derivation_disagrees_is_left_unused() {
    let mut fs = volume();
    let mut desc = fs.group_desc(4).expect("descriptor");
    desc.free_blocks -= 1;
    fs.put_group_desc(4, &desc).expect("staged");
    let goal = fs.layout.group_first(4);
    let (first, _) = fs.alloc_blocks(goal, 1, u64::MAX).expect("space elsewhere");
    assert_eq!(fs.layout.group_of(first), 5, "group 4 is passed over");
    let still = fs.group_desc(4).expect("descriptor");
    assert_ne!(still.flags & BG_BLOCK_UNINIT, 0, "and stays as it was");
}

#[test]
fn freed_blocks_return_only_when_their_transaction_commits() {
    let mut fs = volume();
    let goal = fs.layout.group_first(0) + 2000;
    let (first, len) = fs.alloc_blocks(goal, 4, u64::MAX).expect("allocated");
    let free = fs.free_blocks_count();
    fs.free_blocks(first, u64::from(len)).expect("deferred");
    assert_eq!(fs.free_blocks_count(), free, "not yet free");
    let (again, _) = fs.alloc_blocks(goal, 4, u64::MAX).expect("allocated");
    assert!(
        again >= first + u64::from(len),
        "the freed run is not handed out"
    );
    fs.commit(true).expect("commit");
    assert_eq!(
        fs.free_blocks_count(),
        free,
        "the freed four back, the new four out"
    );
    let (reused, _) = fs.alloc_blocks(goal, 4, u64::MAX).expect("allocated");
    assert_eq!(reused, first, "committed, the run is free again");
}

#[test]
fn a_release_is_costed_once_per_group_it_opens() {
    let mut fs = volume();
    let base = fs.layout.group_first(1);
    for (at, len) in [
        (base + 100, 4),
        (base + 200, 4),
        (base + 8190, 2),
        (base + 8192, 2),
    ] {
        assert_eq!(
            fs.alloc_blocks(at, len, u64::MAX).expect("allocated"),
            (at, len)
        );
    }
    fs.free_blocks(base + 100, 4).expect("one group");
    assert_eq!(fs.freed_cost, 2, "a bitmap and a descriptor");
    fs.free_blocks(base + 200, 4).expect("the same group");
    assert_eq!(fs.freed_cost, 2, "already counted");
    fs.free_blocks(base + 8192 - 2, 4)
        .expect("into the next group");
    assert_eq!(fs.freed_cost, 4, "only the group it opens");
    fs.apply_frees().expect("applied");
    assert_eq!(fs.freed_cost, 0, "applying spends the estimate");
}

/// A release the commit could not apply — of blocks never in use, or of
/// blocks this transaction already releases — is refused as it is asked for,
/// failing that operation alone rather than the transaction at its commit.
#[test]
fn a_release_the_commit_would_refuse_is_refused_when_asked() {
    let mut fs = volume();
    let base = fs.layout.group_first(1);
    assert_eq!(
        fs.free_blocks(base + 100, 4),
        Err(DriverError::DeviceFault),
        "never in use"
    );
    let (first, len) = fs.alloc_blocks(base + 300, 4, u64::MAX).expect("allocated");
    fs.free_blocks(first, u64::from(len)).expect("released");
    assert_eq!(
        fs.free_blocks(first + 1, 1),
        Err(DriverError::DeviceFault),
        "already released"
    );
    fs.apply_frees()
        .expect("the release asked for once is applied");
}

#[test]
fn the_reserve_holds_back_ordinary_allocations() {
    let mut fs = volume();
    let free = fs.free_blocks_count();
    let wide = fs.layout.wide_desc();
    crate::bytes::put_le32(
        &mut fs.sb,
        superblock::SB_R_BLOCKS_LO,
        crate::bytes::low32(free),
    );
    if wide {
        crate::bytes::put_le32(
            &mut fs.sb,
            superblock::SB_R_BLOCKS_HI,
            crate::bytes::high32(free),
        );
    }
    assert_eq!(superblock::reserved_blocks(&fs.sb, wide), free);
    assert_eq!(fs.alloc_blocks(0, 1, u64::MAX), Err(DriverError::NoSpace));
    assert!(fs.in_reserve, "the crossing is noted");
    fs.class = AllocationClass::System;
    assert!(
        fs.alloc_blocks(0, 1, u64::MAX).is_ok(),
        "the system draws on it"
    );
}

#[test]
fn a_table_an_allocation_ceiling_cut_short_is_never_marked_full() {
    let mut fs = volume();
    let ceiling = fs.layout.group_first(1);
    while fs.alloc_blocks(0, MAX_RUN, ceiling).is_ok() {}
    assert!(
        !fs.hints.is_full(0),
        "groups above the ceiling were never looked at"
    );
    let (first, _) = fs
        .alloc_blocks(0, 1, u64::MAX)
        .expect("the rest of the volume");
    assert!(first >= ceiling);
}

#[test]
fn inodes_skip_the_reserved_ones_and_directories_spread() {
    let mut fs = volume();
    let file = fs.alloc_inode(2, false).expect("a file inode");
    assert!(file >= fs.layout.first_ino, "never a reserved inode");
    let dir = fs.alloc_inode(2, true).expect("a directory");
    let next = fs.alloc_inode(2, true).expect("another directory");
    let ipg = fs.layout.inodes_per_group;
    assert_ne!((dir - 1) / ipg, (next - 1) / ipg, "in different groups");
    let desc = fs
        .group_desc(u64::from((next - 1) / ipg))
        .expect("descriptor");
    assert_eq!(desc.flags & BG_INODE_UNINIT, 0, "the group was made real");
    assert_eq!(desc.used_dirs, 1);
    assert!(desc.itable_unused < ipg, "the used high-water moved");
}

#[test]
fn a_descriptor_carried_in_a_staged_block_is_still_verified() {
    let mut fs = volume();
    fs.alloc_blocks(0, 1, u64::MAX)
        .expect("stages group 0's descriptor block");
    let (block, off) = fs.layout.desc_location(5);
    assert!(
        fs.dirty.staged(block).is_some(),
        "group 5 shares the staged block"
    );
    fs.meta_mut(block).expect("staged")[off] ^= 1;
    assert_eq!(fs.group_desc(5).err(), Some(DriverError::DeviceFault));
}

#[test]
fn freeing_into_a_count_at_its_ceiling_fails_closed() {
    let mut fs = volume();
    let (first, len) = fs.alloc_blocks(0, 1, u64::MAX).expect("allocated");
    let group = fs.layout.group_of(first);
    let mut desc = fs.group_desc(group).expect("descriptor");
    desc.free_blocks = u32::MAX;
    fs.put_group_desc(group, &desc).expect("staged");
    fs.free_blocks(first, u64::from(len)).expect("deferred");
    assert_eq!(fs.apply_frees(), Err(DriverError::DeviceFault));
}

/// Inodes below `s_first_ino` are never handed out, even where groups are
/// too small for group 0 to hold them all.
#[test]
fn reserved_inodes_are_skipped_in_every_group_they_fall_in() {
    let mut fs = volume();
    fs.layout.first_ino = fs.layout.inodes_per_group + 3;
    let ino = fs.alloc_inode(crate::ROOT_INODE, false).expect("allocated");
    assert_eq!(ino, fs.layout.first_ino, "the first one past the reserved");
}
