//! ext4 driver unit tests against a hand-built in-memory image.
//!
//! The image is a specification-shaped ext4 volume held in a fixed
//! array (the crate is `no_std`, so the tests stay allocation-free),
//! driven through a [`SizedBlock`] device. Block size is 1024, one block
//! group, 128-byte inodes, the `filetype` feature on:
//!
//! ```text
//! /                       (inode 2,  extent-mapped directory)
//! ├── hello.txt           (inode 11, extent-mapped regular file)
//! ├── classic.bin         (inode 12, block-mapped: direct + holes +
//! │                        single indirect)
//! └── sub/                (inode 13, extent-mapped directory)
//!     └── deep.bin         (inode 14, extent-mapped regular file)
//! ```

extern crate alloc;

use super::*;
use alloc::vec;
use alloc::vec::Vec;
use tairix_abi::driver::block::BlockGeometry;
use tairix_abi::driver::filesystem::{SecurityAcl, SecuritySubject};

use crate::extent::EXTENT_MAGIC;
use crate::superblock::{EXT_MAGIC, INCOMPAT_FILETYPE, INCOMPAT_INLINE_DATA};
use tairix_abi::driver::filesystem::clock::TestClock;
use tairix_abi::driver::filesystem::listing::{first_listed, listed};
use tairix_abi::DriverKind;

// The records nodes are created with where a test does not examine the record.
pub(crate) const FILE_RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);
pub(crate) const DIR_RECORD: NodeSecurity = NodeSecurity::new(0o755, 0, 0);
const LINK_RECORD: NodeSecurity = NodeSecurity::new(0o777, 0, 0);

const FS_BLOCK: usize = 1024;
const FS_BLOCK_COUNT: usize = 40;
const IMG_LEN: usize = FS_BLOCK * FS_BLOCK_COUNT;

const DEV_SECTOR: usize = 512;
const DEV_SECTOR_COUNT: u64 = (IMG_LEN / DEV_SECTOR) as u64;

const BLOCK_BITMAP_BLOCK: usize = 3;
const INODE_BITMAP_BLOCK: usize = 4;
const INODE_TABLE_BLOCK: usize = 5;
const INODES_PER_GROUP: u32 = 16;
const INODE_SIZE: usize = 128;

/// The first data block that starts the run of free space the write
/// tests allocate from (blocks `0..=14` are metadata or planted data).
const FIRST_FREE_BLOCK: usize = 15;
/// Number of free data blocks the fixture leaves (`15..40`).
const FREE_BLOCKS: usize = FS_BLOCK_COUNT - FIRST_FREE_BLOCK;
/// Inodes `1..=14` are in use (reserved + the four planted files /
/// directories); inodes `15` and `16` are free.
const FREE_INODES: usize = 2;

const ROOT_DATA_BLOCK: u32 = 7;
const HELLO_DATA_BLOCK: u32 = 8;
const SUB_DATA_BLOCK: u32 = 9;
const DEEP_DATA_BLOCK: u32 = 10;

const HELLO_BODY: &[u8] = b"Hello from ext4 via extents!\n";
const DEEP_BODY: &[u8] = b"deep file body in a subdirectory\n";

/// Owner of `hello.txt`. Both ids span the low (`i_uid`/`i_gid`) and
/// high (osd2 `l_i_*_high`) halves so the combined decode is exercised.
const HELLO_UID: u32 = 0x0001_2345;
const HELLO_GID: u32 = 0x0002_6789;

/// The classic-mapped file spans 13 logical blocks. Only logical blocks
/// 0, 1 (direct pointers) and 12 (reached through the single-indirect
/// block) carry data; every other logical block is a sparse hole
/// (pointer 0) and reads back as zeros.
const CLASSIC_BLOCKS: usize = 13;
const CLASSIC_LEN: usize = CLASSIC_BLOCKS * FS_BLOCK;
const CLASSIC_DIRECT_0: u32 = 11;
const CLASSIC_DIRECT_1: u32 = 12;
const CLASSIC_INDIRECT_BLOCK: u32 = 13;
const CLASSIC_LOGICAL_12_BLOCK: u32 = 14;

fn u32c(value: usize) -> u32 {
    u32::try_from(value).expect("value fits in u32")
}

fn u16c(value: usize) -> u16 {
    u16::try_from(value).expect("value fits in u16")
}

fn u8c(value: usize) -> u8 {
    u8::try_from(value).expect("value fits in u8")
}

fn set_le16(img: &mut [u8], off: usize, value: u16) {
    img[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn set_le32(img: &mut [u8], off: usize, value: u32) {
    img[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

/// The `i_links_count` a freshly written fixture inode carries: a directory
/// has its own `.` beside its name in the parent, everything else has the one
/// name. A real volume always records this, and the unlink path reads it to
/// decide when to free, so the fixture records it too.
fn default_links(mode: u16) -> u16 {
    if mode & S_IFMT == S_IFDIR {
        2
    } else {
        1
    }
}

fn inode_offset(ino: u32) -> usize {
    INODE_TABLE_BLOCK * FS_BLOCK + (ino as usize - 1) * INODE_SIZE
}

/// Set an inode's owner, splitting each id into its low half
/// (`i_uid`/`i_gid`) and osd2 high half (`l_i_uid_high`/`l_i_gid_high`).
fn set_owner(img: &mut [u8], ino: u32, uid: u32, gid: u32) {
    let base = inode_offset(ino);
    set_le16(img, base + 0x02, (uid & 0xFFFF) as u16);
    set_le16(img, base + 0x78, (uid >> 16) as u16);
    set_le16(img, base + 0x18, (gid & 0xFFFF) as u16);
    set_le16(img, base + 0x7A, (gid >> 16) as u16);
}

fn block_offset(block: u32) -> usize {
    block as usize * FS_BLOCK
}

/// Deterministic byte for the classic file at an absolute file offset.
fn classic_byte(offset: usize) -> u8 {
    u8c(offset % 251)
}

/// Whether the classic file's logical block carries data (the others
/// are sparse holes).
fn classic_present(logical: usize) -> bool {
    matches!(logical, 0 | 1 | 12)
}

/// Physical block backing a present classic logical block.
fn classic_phys(logical: usize) -> u32 {
    match logical {
        0 => CLASSIC_DIRECT_0,
        1 => CLASSIC_DIRECT_1,
        12 => CLASSIC_LOGICAL_12_BLOCK,
        _ => 0,
    }
}

/// Write an inode's common fields plus an extent map covering
/// `extents` — each `(logical_start, len_blocks, physical_start)`.
fn write_extent_inode(img: &mut [u8], ino: u32, mode: u16, size: u32, extents: &[(u32, u16, u32)]) {
    let base = inode_offset(ino);
    set_le16(img, base, mode);
    set_le32(img, base + 0x04, size);
    set_le32(img, base + 0x20, INODE_FLAG_EXTENTS);
    set_le16(img, base + 0x1A, default_links(mode));

    let ib = base + I_BLOCK_OFFSET;
    set_le16(img, ib, EXTENT_MAGIC);
    set_le16(img, ib + 2, u16c(extents.len()));
    set_le16(img, ib + 4, 4);
    set_le16(img, ib + 6, 0);
    set_le32(img, ib + 8, 0);
    for (i, &(logical, len, phys)) in extents.iter().enumerate() {
        let e = ib + 12 + i * 12;
        set_le32(img, e, logical);
        set_le16(img, e + 4, len);
        set_le16(img, e + 6, 0); // ee_start_hi: all test blocks fit in 32 bits
        set_le32(img, e + 8, phys);
    }
}

/// Write an inode with the classic block map: up to 12 direct pointers
/// and one single-indirect pointer.
fn write_classic_inode(
    img: &mut [u8],
    ino: u32,
    mode: u16,
    size: u32,
    direct: &[u32; 12],
    single_indirect: u32,
) {
    let base = inode_offset(ino);
    set_le16(img, base, mode);
    set_le32(img, base + 0x04, size);
    set_le32(img, base + 0x20, 0);
    set_le16(img, base + 0x1A, default_links(mode));

    let ib = base + I_BLOCK_OFFSET;
    for (i, &ptr) in direct.iter().enumerate() {
        set_le32(img, ib + i * 4, ptr);
    }
    set_le32(img, ib + 12 * 4, single_indirect);
}

/// Append a directory entry into `block` at `pos`, returning the next
/// write position. When `fill_to_end` the entry's `rec_len` covers the
/// rest of the block (the on-disk convention for the final entry).
fn put_dirent(
    block: &mut [u8],
    pos: usize,
    ino: u32,
    name: &[u8],
    file_type: u8,
    fill_to_end: bool,
) -> usize {
    let needed = (DIRENT_HEADER + name.len()).div_ceil(4) * 4;
    let rec_len = if fill_to_end { FS_BLOCK - pos } else { needed };
    set_le32(block, pos, ino);
    set_le16(block, pos + 4, u16c(rec_len));
    block[pos + 6] = u8c(name.len());
    block[pos + 7] = file_type;
    block[pos + DIRENT_HEADER..pos + DIRENT_HEADER + name.len()].copy_from_slice(name);
    pos + rec_len
}

/// Write the superblock, the single group descriptor, and the block /
/// inode bitmaps that the write path's allocator consumes.
fn write_volume_metadata(img: &mut [u8]) {
    // --- Superblock at byte 1024 (block 1). ---
    let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("offset fits");
    set_le32(img, sb, INODES_PER_GROUP); // s_inodes_count
    set_le32(img, sb + 0x04, u32c(FS_BLOCK_COUNT)); // s_blocks_count_lo
    set_le32(img, sb + 0x0C, u32c(FREE_BLOCKS)); // s_free_blocks_count_lo
    set_le32(img, sb + 0x10, u32c(FREE_INODES)); // s_free_inodes_count
    set_le32(img, sb + 0x14, 1); // s_first_data_block (1024-byte blocks)
    set_le32(img, sb + 0x18, 0); // s_log_block_size -> 1024
    set_le32(img, sb + 0x20, u32c(FS_BLOCK_COUNT)); // s_blocks_per_group
    set_le32(img, sb + 0x24, u32c(FS_BLOCK_COUNT)); // s_clusters_per_group
    set_le32(img, sb + 0x28, INODES_PER_GROUP); // s_inodes_per_group
    set_le16(img, sb + 0x38, EXT_MAGIC); // s_magic
    set_le16(img, sb + 0x3A, 1); // s_state: clean
    set_le32(img, sb + 0x4C, 1); // s_rev_level (dynamic)
    set_le32(img, sb + 0x54, 11); // s_first_ino
    set_le16(img, sb + 0x58, u16c(INODE_SIZE)); // s_inode_size
    set_le32(img, sb + 0x60, INCOMPAT_FILETYPE); // s_feature_incompat

    // --- Group descriptor 0 at block 2. ---
    let gd = 2 * FS_BLOCK;
    set_le32(img, gd, u32c(BLOCK_BITMAP_BLOCK)); // bg_block_bitmap_lo
    set_le32(img, gd + 0x04, u32c(INODE_BITMAP_BLOCK)); // bg_inode_bitmap_lo
    set_le32(img, gd + 0x08, u32c(INODE_TABLE_BLOCK)); // bg_inode_table_lo
    set_le16(img, gd + 0x0C, u16c(FREE_BLOCKS)); // bg_free_blocks_count_lo
    set_le16(img, gd + 0x0E, u16c(FREE_INODES)); // bg_free_inodes_count_lo
    set_le16(img, gd + 0x10, 2); // bg_used_dirs_count_lo (root + sub)

    // --- Block bitmap (block 3): blocks 1..=14 used, 15..=39 free.
    //     Bit `b` represents block `s_first_data_block + b`, i.e. b + 1. ---
    let bbm = block_offset(u32c(BLOCK_BITMAP_BLOCK));
    for block in 1..FIRST_FREE_BLOCK {
        let bit = block - 1;
        img[bbm + bit / 8] |= 1 << (bit % 8);
    }

    // --- Inode bitmap (block 4): inodes 1..=14 used, 15..=16 free. ---
    let ibm = block_offset(u32c(INODE_BITMAP_BLOCK));
    for ino in 1..=14 {
        let bit = ino - 1;
        img[ibm + bit / 8] |= 1 << (bit % 8);
    }
}

/// Build the in-memory ext4 image described in the module docs.
fn build_image() -> Vec<u8> {
    let mut img = vec![0u8; IMG_LEN];
    write_volume_metadata(&mut img);

    // --- Root directory (inode 2), one extent-mapped block. ---
    write_extent_inode(
        &mut img,
        ROOT_INODE,
        S_IFDIR | 0o755,
        u32c(FS_BLOCK),
        &[(0, 1, ROOT_DATA_BLOCK)],
    );
    {
        let off = block_offset(ROOT_DATA_BLOCK);
        let block = &mut img[off..off + FS_BLOCK];
        let mut pos = put_dirent(block, 0, ROOT_INODE, b".", FT_DIR, false);
        pos = put_dirent(block, pos, ROOT_INODE, b"..", FT_DIR, false);
        pos = put_dirent(block, pos, 11, b"hello.txt", FT_REG, false);
        pos = put_dirent(block, pos, 12, b"classic.bin", FT_REG, false);
        let _ = put_dirent(block, pos, 13, b"sub", FT_DIR, true);
    }

    // --- hello.txt (inode 11), one extent-mapped block. ---
    write_extent_inode(
        &mut img,
        11,
        S_IFREG | 0o644,
        u32c(HELLO_BODY.len()),
        &[(0, 1, HELLO_DATA_BLOCK)],
    );
    set_owner(&mut img, 11, HELLO_UID, HELLO_GID);
    {
        let off = block_offset(HELLO_DATA_BLOCK);
        img[off..off + HELLO_BODY.len()].copy_from_slice(HELLO_BODY);
    }

    // --- sub/ (inode 13), one extent-mapped block. ---
    write_extent_inode(
        &mut img,
        13,
        S_IFDIR | 0o755,
        u32c(FS_BLOCK),
        &[(0, 1, SUB_DATA_BLOCK)],
    );
    {
        let off = block_offset(SUB_DATA_BLOCK);
        let block = &mut img[off..off + FS_BLOCK];
        let mut pos = put_dirent(block, 0, 13, b".", FT_DIR, false);
        pos = put_dirent(block, pos, ROOT_INODE, b"..", FT_DIR, false);
        let _ = put_dirent(block, pos, 14, b"deep.bin", FT_REG, true);
    }

    // --- sub/deep.bin (inode 14), one extent-mapped block. ---
    write_extent_inode(
        &mut img,
        14,
        S_IFREG | 0o600,
        u32c(DEEP_BODY.len()),
        &[(0, 1, DEEP_DATA_BLOCK)],
    );
    {
        let off = block_offset(DEEP_DATA_BLOCK);
        img[off..off + DEEP_BODY.len()].copy_from_slice(DEEP_BODY);
    }

    // --- classic.bin (inode 12): block-mapped direct + holes + single
    //     indirect. Direct pointers map logical 0 and 1; logical 2..=11
    //     are holes (pointer 0). Logical 12 is reached via the indirect
    //     block, exercising the direct/indirect boundary. ---
    let mut direct = [0u32; 12];
    direct[0] = CLASSIC_DIRECT_0;
    direct[1] = CLASSIC_DIRECT_1;
    write_classic_inode(
        &mut img,
        12,
        S_IFREG | 0o644,
        u32c(CLASSIC_LEN),
        &direct,
        CLASSIC_INDIRECT_BLOCK,
    );
    // The indirect block's first pointer maps logical block 12.
    set_le32(
        &mut img,
        block_offset(CLASSIC_INDIRECT_BLOCK),
        CLASSIC_LOGICAL_12_BLOCK,
    );
    // Fill each present data block with the deterministic pattern.
    for logical in 0..CLASSIC_BLOCKS {
        if !classic_present(logical) {
            continue;
        }
        let off = block_offset(classic_phys(logical));
        for i in 0..FS_BLOCK {
            img[off + i] = classic_byte(logical * FS_BLOCK + i);
        }
    }

    img
}

/// Mock driver host modelling the load-time `CAP_DRV_LOAD` grant.
struct MockHost {
    drv_load: bool,
}

impl DriverHost for MockHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        matches!(cap, CapabilityId::DRV_LOAD if self.drv_load)
    }

    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

fn mount() -> Ext4<SizedBlock> {
    Ext4::open(SizedBlock::over(build_image())).expect("image is a valid ext4 volume")
}

#[test]
fn register_requires_drv_load() {
    assert!(register(&MockHost { drv_load: true }).is_ok());
    assert_eq!(
        register(&MockHost { drv_load: false }),
        Err(DriverError::PermissionDenied)
    );
}

#[test]
fn open_rejects_bad_magic() {
    let mut data = build_image();
    let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("offset fits");
    set_le16(&mut data, sb + 0x38, 0x1234);
    assert_eq!(
        Ext4::open(SizedBlock::over(data)).err(),
        Some(DriverError::BadMagic)
    );
}

#[test]
fn root_is_a_directory() {
    let mut fs = mount();
    let info = fs.node_info(fs.root()).expect("info");
    assert_eq!(info.kind, NodeKind::Directory);
    assert_eq!(info.size, 0);
}

#[test]
fn root_lists_its_entries_in_on_disk_order() {
    let mut fs = mount();
    let root = fs.root();
    let entries = listed(&mut fs, root, 0, &[]).expect("lists");
    let names: Vec<&[u8]> = entries.iter().map(|(_, name)| name.as_slice()).collect();
    // `.` and `..` are not surfaced, and iteration terminates.
    assert_eq!(names, [&b"hello.txt"[..], b"classic.bin", b"sub"]);
    let kinds: Vec<NodeKind> = entries.iter().map(|(entry, _)| entry.info.kind).collect();
    assert_eq!(
        kinds,
        [
            NodeKind::RegularFile,
            NodeKind::RegularFile,
            NodeKind::Directory
        ]
    );

    // Resuming from each entry's cursor walks the same listing one entry at
    // a time.
    let mut chained = Vec::new();
    let mut cursor = 0;
    while let Some((entry, name)) = first_listed(&mut fs, root, cursor, &[]).expect("lists") {
        chained.push(name);
        cursor = entry.next_cursor;
    }
    assert_eq!(chained, names);
}

/// Entries present throughout a listing are listed exactly once across
/// calls, while the directory has a name removed (its record folds into its
/// neighbour) and another added (it splits a record's slack).
#[test]
fn a_listing_resumed_across_changes_lists_each_lasting_entry_once() {
    let mut fs = mount();
    let root = fs.root();
    let (first, name) = first_listed(&mut fs, root, 0, &[])
        .expect("lists")
        .expect("an entry");
    assert_eq!(name, b"hello.txt");
    fs.remove(root, b"classic.bin").expect("remove");
    fs.create(root, b"late.dat", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let rest: Vec<Vec<u8>> = listed(&mut fs, root, first.next_cursor, &name)
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert!(
        rest.iter().any(|name| name == b"sub"),
        "a lasting entry is listed"
    );
    assert!(
        !rest
            .iter()
            .any(|name| name == b"hello.txt" || name == b"classic.bin"),
        "nothing already listed or removed comes back: {rest:?}"
    );
}

#[test]
fn lookup_and_read_an_extent_mapped_file() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("found");
    let info = fs.node_info(file).expect("info");
    assert_eq!(info.kind, NodeKind::RegularFile);
    assert_eq!(info.size, HELLO_BODY.len() as u64);

    let mut buf = [0u8; 64];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], HELLO_BODY);

    // A read at EOF yields zero bytes.
    assert_eq!(fs.read_at(file, info.size, &mut buf), Ok(0));
    // A mid-file offset returns the trailing bytes.
    let n = fs.read_at(file, 7, &mut buf).expect("read");
    assert_eq!(&buf[..n], &HELLO_BODY[7..]);
}

#[test]
fn lookup_missing_child_is_not_found() {
    let mut fs = mount();
    assert_eq!(fs.lookup(fs.root(), b"nope"), Err(DriverError::NotFound));
}

#[test]
fn traverses_into_a_subdirectory() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");
    assert_eq!(fs.node_info(sub).expect("info").kind, NodeKind::Directory);

    let deep = fs.lookup(sub, b"deep.bin").expect("deep");
    let mut buf = [0u8; 64];
    let n = fs.read_at(deep, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], DEEP_BODY);
}

#[test]
fn reads_a_classic_block_mapped_file_across_holes_and_indirect() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"classic.bin").expect("found");
    assert_eq!(fs.node_info(file).expect("info").size, CLASSIC_LEN as u64);

    // Read the whole file in one call and check every byte: the present
    // blocks carry the deterministic pattern, the holes read as zeros.
    let mut buf = [0u8; CLASSIC_LEN];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(n, CLASSIC_LEN);
    for (offset, byte) in buf.iter().enumerate() {
        let logical = offset / FS_BLOCK;
        let expected = if classic_present(logical) {
            classic_byte(offset)
        } else {
            0
        };
        assert_eq!(*byte, expected, "mismatch at offset {offset}");
    }
}

#[test]
fn classic_read_spanning_the_indirect_boundary() {
    let mut fs = mount();
    let file = fs.lookup(fs.root(), b"classic.bin").expect("found");
    // Straddle the direct/indirect boundary: the tail of logical block
    // 11 (a hole) into logical block 12 (present, behind the
    // single-indirect pointer).
    let start = 12 * FS_BLOCK - 4;
    let mut buf = [0u8; 8];
    let n = fs.read_at(file, start as u64, &mut buf).expect("read");
    assert_eq!(n, 8);
    for (i, byte) in buf.iter().enumerate() {
        let offset = start + i;
        let expected = if classic_present(offset / FS_BLOCK) {
            classic_byte(offset)
        } else {
            0
        };
        assert_eq!(*byte, expected);
    }
}

#[test]
fn read_at_on_a_directory_is_unsupported() {
    let mut fs = mount();
    let mut buf = [0u8; 16];
    assert_eq!(
        fs.read_at(fs.root(), 0, &mut buf),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn lookup_in_a_regular_file_is_unsupported() {
    let mut fs = mount();
    let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
    assert_eq!(fs.lookup(file, b"x"), Err(DriverError::Unsupported));
}

#[test]
fn node_id_none_is_not_found() {
    let mut fs = mount();
    assert_eq!(fs.node_info(NodeId::NONE), Err(DriverError::NotFound));
}

#[test]
fn into_block_returns_the_underlying_device() {
    let fs = mount();
    let dev = fs.into_block().expect("commit");
    assert_eq!(
        dev.geometry().expect("geometry").block_count,
        DEV_SECTOR_COUNT
    );
}

#[test]
fn security_reports_a_files_mode_and_owner() {
    let mut fs = mount();
    let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
    let sec = fs.security(file).expect("security");
    // The mode is the low 12 bits; the directory/file type bits are stripped.
    assert_eq!(sec.mode, 0o644);
    // uid/gid recombine the low half with the osd2 high half.
    assert_eq!(sec.uid, HELLO_UID);
    assert_eq!(sec.gid, HELLO_GID);
    // ext4 stores no inline capability gate and no inline ACL here.
    assert_eq!(sec.required_cap, None);
    assert!(sec.acl().is_empty());
}

#[test]
fn security_reports_a_directorys_record() {
    let mut fs = mount();
    let sec = fs.security(fs.root()).expect("security");
    assert_eq!(sec.mode, 0o755);
    // The root inode in the fixture leaves the owner at the default 0/0.
    assert_eq!(sec.uid, 0);
    assert_eq!(sec.gid, 0);
}

#[test]
fn security_of_an_absent_node_is_not_found() {
    let mut fs = mount();
    assert_eq!(fs.security(NodeId::NONE), Err(DriverError::NotFound));
}

#[test]
fn a_created_node_takes_the_owner_group_and_mode_of_its_record() {
    // Wide enough to need both halves of each split id.
    let (uid, gid) = (0x0001_03E8, 0x0002_0064);
    let mut fs = mount();
    let root = fs.root();
    let mut file_record = NodeSecurity::new(0o640, uid, gid);
    // Neither has a place in an ext4 inode, so the node is born without them.
    file_record.required_cap = Some(CapabilityId::AUDIT_READ);
    file_record
        .push_acl(SecurityAcl {
            subject: SecuritySubject::User(7),
            perms: 0b100,
        })
        .expect("an entry");
    fs.create(root, b"mine.txt", NodeKind::RegularFile, &file_record)
        .expect("create");
    fs.create(
        root,
        b"mine",
        NodeKind::Directory,
        &NodeSecurity::new(0o2750, uid, gid),
    )
    .expect("mkdir");

    let mut fs = remount(fs);
    let root = fs.root();
    for (name, kind, mode) in [
        (&b"mine.txt"[..], NodeKind::RegularFile, 0o640),
        (b"mine", NodeKind::Directory, 0o2750),
    ] {
        let node = fs.lookup(root, name).expect("found after remount");
        assert_eq!(fs.node_info(node).expect("stat").kind, kind);
        assert_eq!(
            fs.security(node),
            Ok(NodeSecurity::new(mode, uid, gid)),
            "{name:?}"
        );
    }
}

// --- Write surface (`FilesystemWrite`). ---

/// Re-open the volume backing `fs`, exercising the persistence of any
/// writes through a fresh mount.
fn remount(fs: Ext4<SizedBlock>) -> Ext4<SizedBlock> {
    Ext4::open(fs.into_block().expect("commit")).expect("re-open the mutated image")
}

#[test]
fn create_and_write_a_regular_file_round_trips() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs
        .create(root, b"new.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(fs.node_info(file).expect("info").size, 0);
    assert_eq!(fs.lookup(root, b"new.txt"), Ok(file));

    // A payload spanning two filesystem blocks.
    let mut payload = [0u8; FS_BLOCK + 500];
    let mut next = 0u8;
    for b in &mut payload {
        *b = next;
        next = next.wrapping_add(1);
    }
    let n = fs.write_at(file, 0, &payload).expect("write");
    assert_eq!(n, payload.len());
    assert_eq!(fs.node_info(file).expect("info").size, payload.len() as u64);

    let mut fs = remount(fs);
    let file = fs
        .lookup(fs.root(), b"new.txt")
        .expect("found after remount");
    let mut buf = [0u8; FS_BLOCK + 500];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(n, payload.len());
    assert_eq!(buf, payload);
}

#[test]
fn create_then_appears_in_directory_listing() {
    let mut fs = mount();
    let root = fs.root();
    fs.create(root, b"zeta.dat", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let entries = listed(&mut fs, root, 0, &[]).expect("read_dir");
    let (entry, _) = entries
        .iter()
        .find(|(_, name)| name == b"zeta.dat")
        .expect("the created file is listed");
    assert_eq!(entry.info.kind, NodeKind::RegularFile);
}

#[test]
fn create_rejects_a_duplicate_name() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"hello.txt", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::AlreadyExists)
    );
}

#[test]
fn create_in_a_regular_file_is_unsupported() {
    let mut fs = mount();
    let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
    assert_eq!(
        fs.create(file, b"x", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn create_rejects_an_invalid_name() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        fs.create(root, b"a/b", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        fs.create(root, b"..", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn write_past_eof_leaves_a_sparse_hole() {
    let mut fs = mount();
    let root = fs.root();
    let sparse_bin = fs
        .create(root, b"sparse.bin", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let tail = b"TAIL";
    let n = fs.write_at(sparse_bin, 2000, tail).expect("write");
    assert_eq!(n, tail.len());

    let file = fs.lookup(root, b"sparse.bin").expect("found");
    assert_eq!(
        fs.node_info(file).expect("info").size,
        2000 + tail.len() as u64
    );
    let mut buf = [0u8; 2000 + 4];
    let read = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(read, buf.len());
    assert!(
        buf[..2000].iter().all(|&b| b == 0),
        "the gap reads as zeros"
    );
    assert_eq!(&buf[2000..], tail);
}

#[test]
fn truncate_shrink_then_grow() {
    let mut fs = mount();
    let root = fs.root();
    let trunc_bin = fs
        .create(root, b"trunc.bin", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let payload = [0xABu8; 3 * FS_BLOCK];
    fs.write_at(trunc_bin, 0, &payload).expect("write");

    // Shrink to mid-first-block: the freed tail blocks return to the pool.
    fs.truncate(trunc_bin, 100).expect("shrink");
    let file = fs.lookup(root, b"trunc.bin").expect("found");
    assert_eq!(fs.node_info(file).expect("info").size, 100);
    let mut buf = [0u8; 200];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(n, 100);
    assert!(buf[..100].iter().all(|&b| b == 0xAB));

    // Grow back: the extension reads as zeros (sparse).
    fs.truncate(file, FS_BLOCK as u64).expect("grow");
    let mut big = [0xFFu8; FS_BLOCK];
    let n = fs.read_at(file, 0, &mut big).expect("read");
    assert_eq!(n, FS_BLOCK);
    assert!(big[..100].iter().all(|&b| b == 0xAB));
    assert!(big[100..].iter().all(|&b| b == 0), "grown region is zero");
}

/// Logical blocks written far enough apart to stay distinct extents, so
/// the inline four-slot extent root overflows and converts to a depth-1
/// tree (`hello.txt`'s planted extent already occupies the first slot,
/// so the fifth write here is the one that forces the conversion).
const GROWTH_LOGICALS: [u64; 5] = [100, 200, 300, 400, 500];

/// A deterministic, non-zero fill byte for the block at `logical`.
fn growth_marker(logical: u64) -> u8 {
    u8::try_from(logical / 100).expect("marker index fits") * 17 + 3
}

/// Sparsely write one distinct block at each [`GROWTH_LOGICALS`] offset of
/// `hello.txt`, forcing its depth-0 inline root to grow into a depth-1
/// extent tree.
fn grow_hello_into_depth1(fs: &mut Ext4<SizedBlock>) {
    let root = fs.root();
    let hello_txt = fs.lookup(root, b"hello.txt").expect("lookup");
    for &logical in &GROWTH_LOGICALS {
        let block = [growth_marker(logical); FS_BLOCK];
        let n = fs
            .write_at(hello_txt, logical * FS_BLOCK as u64, &block)
            .expect("sparse write grows the extent tree");
        assert_eq!(n, FS_BLOCK);
    }
}

#[test]
fn an_extent_file_grows_into_a_depth1_tree() {
    let mut fs = mount();
    grow_hello_into_depth1(&mut fs);

    // Re-open to prove the converted tree persists on disk.
    let mut fs = remount(fs);
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("found");

    // The original first-block contents survive the conversion.
    let mut head = [0u8; 64];
    let n = fs.read_at(file, 0, &mut head).expect("read head");
    assert_eq!(n, head.len());
    assert_eq!(&head[..HELLO_BODY.len()], HELLO_BODY);

    // Every sparsely-written block reads back its marker...
    for &logical in &GROWTH_LOGICALS {
        let mut buf = [0u8; FS_BLOCK];
        let n = fs
            .read_at(file, logical * FS_BLOCK as u64, &mut buf)
            .expect("read a grown block");
        assert_eq!(n, FS_BLOCK);
        assert!(
            buf.iter().all(|&b| b == growth_marker(logical)),
            "logical block {logical} reads back its marker"
        );
    }

    // ...and a gap between them is a sparse hole of zeros.
    let mut hole = [0xFFu8; FS_BLOCK];
    let n = fs
        .read_at(file, 150 * FS_BLOCK as u64, &mut hole)
        .expect("read hole");
    assert_eq!(n, FS_BLOCK);
    assert!(hole.iter().all(|&b| b == 0), "the gap reads as zeros");
}

#[test]
fn truncating_a_depth1_extent_file_to_zero_frees_its_tree() {
    let mut fs = mount();
    grow_hello_into_depth1(&mut fs);
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("found");
    fs.truncate(file, 0).expect("truncate to zero");
    assert_eq!(fs.node_info(file).expect("info").size, 0);

    // The freed leaf + data blocks return to the pool: re-growing into a
    // fresh depth-1 tree succeeds and round-trips across a remount.
    grow_hello_into_depth1(&mut fs);
    let mut fs = remount(fs);
    let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
    for &logical in &GROWTH_LOGICALS {
        let mut buf = [0u8; FS_BLOCK];
        let n = fs
            .read_at(file, logical * FS_BLOCK as u64, &mut buf)
            .expect("read a regrown block");
        assert_eq!(n, FS_BLOCK);
        assert!(buf.iter().all(|&b| b == growth_marker(logical)));
    }
}

#[test]
fn removing_a_depth1_extent_file_frees_it() {
    let mut fs = mount();
    grow_hello_into_depth1(&mut fs);
    let root = fs.root();
    fs.remove(root, b"hello.txt")
        .expect("remove a depth-1 file");
    assert_eq!(fs.lookup(root, b"hello.txt"), Err(DriverError::NotFound));

    // The inode and all of its data + leaf blocks are reusable afterwards.
    let fresh_txt = fs
        .create(root, b"fresh.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create reuses the freed metadata");
    fs.write_at(fresh_txt, 0, b"ok").expect("write");
    let mut fs = remount(fs);
    let file = fs.lookup(fs.root(), b"fresh.txt").expect("found");
    let mut buf = [0u8; 8];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"ok");
}

#[test]
fn create_a_directory_with_dot_and_dotdot() {
    let mut fs = mount();
    let root = fs.root();
    let dir = fs
        .create(root, b"newdir", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    assert_eq!(fs.node_info(dir).expect("info").kind, NodeKind::Directory);

    // A fresh directory lists no children (`.`/`..` are skipped).
    assert_eq!(listed(&mut fs, dir, 0, &[]), Ok(Vec::new()));

    // It accepts a child, which then resolves and lists.
    let child = fs
        .create(dir, b"inner.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(fs.lookup(dir, b"inner.txt"), Ok(child));

    let mut fs = remount(fs);
    let dir = fs.lookup(fs.root(), b"newdir").expect("dir after remount");
    assert!(fs.lookup(dir, b"inner.txt").is_ok());
}

#[test]
fn remove_a_file_frees_its_inode_for_reuse() {
    let mut fs = mount();
    let root = fs.root();
    fs.remove(root, b"hello.txt").expect("remove");
    assert_eq!(fs.lookup(root, b"hello.txt"), Err(DriverError::NotFound));

    // The freed inode and blocks are reusable: creating + writing succeeds
    // and round-trips across a remount.
    let again_txt = fs
        .create(root, b"again.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create reuses freed metadata");
    let body = b"reused";
    fs.write_at(again_txt, 0, body).expect("write");

    let mut fs = remount(fs);
    let file = fs.lookup(fs.root(), b"again.txt").expect("found");
    let mut buf = [0u8; 16];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], body);
}

#[test]
fn remove_a_non_empty_directory_is_busy() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(fs.remove(root, b"sub"), Err(DriverError::DirectoryNotEmpty));
}

#[test]
fn remove_an_emptied_directory() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("sub");
    fs.remove(sub, b"deep.bin").expect("empty the dir");
    fs.remove(root, b"sub").expect("remove the now-empty dir");
    assert_eq!(fs.lookup(root, b"sub"), Err(DriverError::NotFound));
}

#[test]
fn parent_answers_a_directorys_dot_dot() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("sub");
    assert_eq!(fs.parent(sub), Ok(root));
    assert_eq!(fs.parent(root), Ok(root), "the root answers itself");
    let inner = fs
        .create(sub, b"inner", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    assert_eq!(fs.parent(inner), Ok(sub));
    let deep = fs.lookup(sub, b"deep.bin").expect("deep.bin");
    assert_eq!(fs.parent(deep), Err(DriverError::Unsupported));

    fs.remove(sub, b"inner").expect("rmdir");
    assert_eq!(fs.parent(inner), Err(DriverError::NotFound));
}

#[test]
fn write_to_a_directory_is_unsupported() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("lookup");
    assert_eq!(fs.write_at(sub, 0, b"x"), Err(DriverError::Unsupported));
}

#[test]
fn a_write_through_an_id_naming_no_live_file_is_not_found() {
    // A freed inode keeps its file type, so only its link count tells a stale
    // id from a live file.
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("found");
    fs.remove(root, b"hello.txt").expect("remove");
    let free = fs.stats().expect("stats").free_blocks;
    assert_eq!(fs.write_at(file, 0, b"stale"), Err(DriverError::NotFound));
    assert_eq!(fs.truncate(file, 4096), Err(DriverError::NotFound));
    assert_eq!(
        fs.stats().expect("stats").free_blocks,
        free,
        "the freed inode took no block"
    );
    assert_eq!(
        fs.write_at(NodeId::NONE, 0, b"x"),
        Err(DriverError::NotFound)
    );
}

#[test]
fn mutation_is_refused_on_an_unsupported_feature_set() {
    // `inline_data` is a feature the read path serves and the write path
    // does not maintain, so the volume mounts read-only.
    let mut data = build_image();
    let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("offset fits");
    set_le32(
        &mut data,
        sb + 0x60,
        INCOMPAT_FILETYPE | INCOMPAT_INLINE_DATA,
    );
    let mut fs = Ext4::open(SizedBlock::over(data)).expect("opens read-only");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
    let hello = fs.lookup(fs.root(), b"hello.txt").expect("still readable");
    assert_eq!(
        fs.create(fs.root(), b"x", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::ReadOnly)
    );
    assert_eq!(fs.write_at(hello, 0, b"x"), Err(DriverError::ReadOnly));
    assert_eq!(
        fs.remove(fs.root(), b"hello.txt"),
        Err(DriverError::ReadOnly)
    );
}

#[test]
fn create_exhausts_the_free_inodes() {
    let mut fs = mount();
    let root = fs.root();
    // The fixture leaves exactly two free inodes (15, 16).
    fs.create(root, b"one", NodeKind::RegularFile, &FILE_RECORD)
        .expect("first");
    fs.create(root, b"two", NodeKind::RegularFile, &FILE_RECORD)
        .expect("second");
    assert_eq!(
        fs.create(root, b"three", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::NoSpace)
    );
}

// --- POSIX access ACLs, on volumes e2fsprogs 1.47.0 made. ---

/// `mke2fs -t ext4 -b 1024 -I 128 -O metadata_csum,^has_journal`, then
/// `debugfs` writes `/f` (uid and gid 1000, mode 0664) and sets
/// `u::rw-,u:1000:rw-,g::r--,g:2000:r--,m::rw-,o::r--`: with no room in
/// the inode, the ACL lives in an external attribute block.
const ACL_BLOCK: &[u8] = include_bytes!("../tests/fixtures/acl_block.sparse");
/// The same with 256-byte inodes, so the ACL lives in the inode body.
const ACL_IBODY: &[u8] = include_bytes!("../tests/fixtures/acl_ibody.sparse");

fn acl_volume(fixture: &[u8]) -> (Ext4<SizedBlock>, NodeId) {
    let mut fs = Ext4::open(SizedBlock::over(sparse(fixture))).expect("an e2fsprogs volume");
    let file = fs.lookup(fs.root(), b"f").expect("f");
    (fs, file)
}

fn named(user: u32, user_perms: u8, group: u32, group_perms: u8) -> [SecurityAcl; 2] {
    [
        SecurityAcl {
            subject: SecuritySubject::User(user),
            perms: user_perms,
        },
        SecurityAcl {
            subject: SecuritySubject::Group(group),
            perms: group_perms,
        },
    ]
}

#[test]
fn an_acl_is_decoded_from_an_external_block_and_from_the_inode_body() {
    for fixture in [ACL_BLOCK, ACL_IBODY] {
        let (mut fs, file) = acl_volume(fixture);
        let sec = fs.security(file).expect("security");
        assert_eq!((sec.uid, sec.gid), (1000, 1000));
        assert_eq!(sec.mode, 0o664, "the owner, mask and other triads");
        assert_eq!(
            sec.acl_model,
            AclModel::Posix {
                group_obj: 0o4,
                mask: 0o6
            }
        );
        assert_eq!(sec.acl(), &named(1000, 0o6, 2000, 0o4));
    }
}

#[test]
fn a_mode_change_moves_the_acl_mask_and_keeps_its_entries() {
    for fixture in [ACL_BLOCK, ACL_IBODY] {
        let (mut fs, file) = acl_volume(fixture);
        let mut sec = fs.security(file).expect("security");
        sec.mode = 0o640;
        fs.set_security(file, sec).expect("chmod");
        let mut fs = remount(fs);
        let file = fs.lookup(fs.root(), b"f").expect("f");
        let after = fs.security(file).expect("a resealed record");
        assert_eq!(after.mode, 0o640);
        assert_eq!(
            after.acl_model,
            AclModel::Posix {
                group_obj: 0o4,
                mask: 0o4
            }
        );
        assert_eq!(
            after.acl(),
            &named(1000, 0o6, 2000, 0o4),
            "named entries untouched"
        );
    }
}

#[test]
fn an_owner_change_is_stored_and_an_acl_edit_refused() {
    let (mut fs, file) = acl_volume(ACL_BLOCK);
    let mut sec = fs.security(file).expect("security");
    sec.uid = 4242;
    sec.gid = 77;
    fs.set_security(file, sec).expect("chown");
    let after = fs.security(file).expect("security");
    assert_eq!((after.uid, after.gid), (4242, 77));

    let mut widened = after;
    widened.acl_model = AclModel::Posix {
        group_obj: 0o7,
        mask: 0o6,
    };
    assert_eq!(
        fs.set_security(file, widened),
        Err(DriverError::Unsupported)
    );
    let mut gated = after;
    gated.required_cap = Some(CapabilityId::DRV_LOAD);
    assert_eq!(fs.set_security(file, gated), Err(DriverError::Unsupported));
    assert_eq!(
        fs.security(file),
        Ok(after),
        "a refused record changes nothing"
    );
}

#[test]
fn a_damaged_attribute_block_fails_closed() {
    let mut image = sparse(ACL_BLOCK);
    // Block 33 is `/f`'s attribute block.
    image[33 * 1024] ^= 0xFF;
    let mut fs = Ext4::open(SizedBlock::over(image)).expect("opens");
    let file = fs.lookup(fs.root(), b"f").expect("f");
    assert_eq!(fs.security(file), Err(DriverError::DeviceFault));
}

// --- First-party formatter (`Ext4::format`). ---

/// The fixed volume identity the formatter tests stamp (tests need a
/// deterministic value; production callers mint one from the kernel RNG).
pub(crate) const TEST_UUID: [u8; 16] = [
    0xB7, 0xF2, 0xE4, 0xE6, 0x8D, 0x7A, 0x4E, 0xF8, 0xA1, 0x3E, 0xD3, 0xB8, 0x4D, 0x4E, 0x80, 0x01,
];

#[test]
fn format_refuses_the_nil_uuid() {
    assert_eq!(
        Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, [0u8; 16]).err(),
        Some(DriverError::OutOfRange)
    );
}

/// Logical-block (sector) size of the configurable in-memory device the
/// formatter tests run against.
const FMT_SECTOR: usize = 512;

/// 8 MiB in 512-byte sectors: a single block group at the 1024-byte
/// filesystem block size the formatter picks for sub-64-MiB volumes.
pub(crate) const ONE_GROUP_SECTORS: u64 = (8 * 1024 * 1024 / FMT_SECTOR) as u64;

/// 16 MiB in 512-byte sectors: two block groups at the 1024-byte block
/// size (`blocks_per_group = 8 * 1024 = 8192` blocks = 8 MiB each).
const TWO_GROUP_SECTORS: u64 = (16 * 1024 * 1024 / FMT_SECTOR) as u64;

/// The real `mke2fs` volume carrying `metadata_csum`, whose superblock hashes
/// names as signed bytes under a random seed.
const META_CSUM: &[u8] = include_bytes!("../tests/fixtures/metadata_csum.img");

/// The checksummed `mke2fs` volume.
pub(crate) fn checksummed() -> Ext4<SizedBlock> {
    Ext4::open(SizedBlock::over(META_CSUM.to_vec())).expect("mount")
}

/// A `Vec`-backed [`Block`] device of 512-byte sectors that counts its
/// flushes and, armed, fails every write after a budget: the one test
/// device every module's tests drive.
pub(crate) struct SizedBlock {
    pub(crate) data: Vec<u8>,
    pub(crate) flushes: u32,
    /// Writes the device still accepts before refusing every later one.
    pub(crate) writes_left: Option<u32>,
}

impl SizedBlock {
    /// A zeroed device of `sectors` sectors.
    pub(crate) fn new(sectors: u64) -> Self {
        let len = usize::try_from(sectors).expect("fits") * FMT_SECTOR;
        Self::over(vec![0u8; len])
    }

    /// A device holding `data`.
    pub(crate) fn over(data: Vec<u8>) -> Self {
        Self {
            data,
            flushes: 0,
            writes_left: None,
        }
    }

    fn span(&self, lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
        if len == 0 || !len.is_multiple_of(FMT_SECTOR) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba)
            .map_err(|_| DriverError::LengthOutOfRange)?
            .saturating_mul(FMT_SECTOR);
        let end = start
            .checked_add(len)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.data.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok((start, end))
    }
}

impl Block for SizedBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: u32c(FMT_SECTOR),
            block_count: (self.data.len() / FMT_SECTOR) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        if let Some(left) = self.writes_left.as_mut() {
            if *left == 0 {
                return Err(DriverError::DeviceFault);
            }
            *left -= 1;
        }
        self.data[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.flushes += 1;
        Ok(())
    }
}

#[path = "../tests/support/sparse.rs"]
mod sparse_fixture;
pub(crate) use sparse_fixture::sparse;

/// A three-byte file name `f<NN>` for the inode-exhaustion test.
fn nth_name(i: usize) -> [u8; 3] {
    [b'f', b'0' + u8c(i / 10), b'0' + u8c(i % 10)]
}

/// Per-file size cap when filling the data region, in bytes. Kept well
/// below the classic block-map reach (12 direct + one single-indirect
/// block, i.e. ~268 KiB at the 1024-byte block size), since
/// [`FilesystemWrite::create`] lays files down with the classic map.
const FILL_FILE_CAP: u64 = 256 * 1024;

/// Fill the volume's data region by creating bounded-size files until an
/// allocation reports [`DriverError::NoSpace`], returning the total
/// number of data bytes successfully written. Any other error is a
/// driver defect and panics the test.
fn fill_to_no_space(fs: &mut Ext4<SizedBlock>, root: NodeId) -> u64 {
    let chunk = [0xABu8; 4096];
    let mut total = 0u64;
    let mut idx = 0usize;
    'outer: loop {
        let name = nth_name(idx);
        let file = match fs.create(root, &name, NodeKind::RegularFile, &FILE_RECORD) {
            Ok(file) => file,
            Err(DriverError::NoSpace) => break,
            Err(e) => panic!("unexpected create error: {e:?}"),
        };
        idx += 1;
        assert!(idx < 100, "exhausted test file names before filling");
        let mut size = 0u64;
        while size < FILL_FILE_CAP {
            match fs.write_at(file, size, &chunk) {
                Ok(n) => {
                    size += n as u64;
                    total += n as u64;
                }
                Err(DriverError::NoSpace) => break 'outer,
                Err(e) => panic!("unexpected write error: {e:?}"),
            }
        }
    }
    total
}

#[test]
fn format_rejects_a_device_too_small_for_one_group() {
    // 1 MiB cannot host even a single 8-MiB (1024-byte block) group.
    let sectors = 1024 * 1024 / FMT_SECTOR as u64;
    assert_eq!(
        Ext4::format(SizedBlock::new(sectors), 64, TEST_UUID).err(),
        Some(DriverError::OutOfRange)
    );
}

#[test]
fn format_rejects_a_zero_inode_budget() {
    assert_eq!(
        Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 0, TEST_UUID).err(),
        Some(DriverError::OutOfRange)
    );
}

#[test]
fn format_produces_a_mountable_empty_volume() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    assert_eq!(fs.node_info(root).expect("info").kind, NodeKind::Directory);
    // A freshly formatted root has no children (only `.`/`..`, which the
    // reader does not surface).
    assert_eq!(listed(&mut fs, root, 0, &[]), Ok(Vec::new()));
}

#[test]
fn format_create_write_read_roundtrips_across_a_remount() {
    let body = b"a fresh ext4 volume, formatted in TAIRiX\n";
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let hello_txt = fs
        .create(root, b"hello.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(fs.write_at(hello_txt, 0, body), Ok(body.len()));

    // Remount from the same device and read the file back.
    let mut fs = Ext4::open(fs.into_block().expect("commit")).expect("reopen");
    let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
    assert_eq!(fs.node_info(file).expect("info").size, body.len() as u64);
    let mut buf = [0u8; 64];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], body);
}

#[test]
fn stats_track_the_live_superblock_and_the_uuid_is_remount_stable() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let uuid = fs.volume_uuid();
    assert_ne!(uuid, [0u8; 16], "the formatter mints a non-nil s_uuid");

    let before = fs.stats().expect("stats");
    assert!(before.total_blocks > 0);
    assert!(before.free_blocks <= before.total_blocks);
    assert!(before.avail_blocks <= before.free_blocks);
    assert!(before.files >= 256);
    assert!(before.files_free < before.files);

    // Allocating a file consumes blocks and an inode.
    let root = fs.root();
    let stats_bin = fs
        .create(root, b"stats.bin", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let body = [0xA5u8; 8192];
    assert_eq!(fs.write_at(stats_bin, 0, &body), Ok(body.len()));
    let after = fs.stats().expect("stats");
    assert!(after.free_blocks < before.free_blocks);
    assert_eq!(after.files_free, before.files_free - 1);

    // Removing it restores both counts, and the identity survives a
    // remount of the same bytes.
    fs.remove(root, b"stats.bin").expect("remove");
    let restored = fs.stats().expect("stats");
    assert_eq!(restored.free_blocks, before.free_blocks);
    assert_eq!(restored.files_free, before.files_free);
    let fs = Ext4::open(fs.into_block().expect("commit")).expect("reopen");
    assert_eq!(fs.volume_uuid(), uuid);
}

#[test]
fn format_data_region_fills_to_no_space_then_recovers() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let written = fill_to_no_space(&mut fs, root);
    assert!(written > 0, "expected to write at least one block");

    // The full volume is still consistent: an early file reads back its
    // first block intact.
    let file = fs.lookup(root, &nth_name(0)).expect("found");
    let mut buf = [0u8; 4096];
    assert_eq!(fs.read_at(file, 0, &mut buf), Ok(4096));
    assert!(buf.iter().all(|&b| b == 0xAB));

    // Freeing space lets allocation resume (NoSpace is not terminal):
    // removing a file frees its blocks, after which a write succeeds.
    fs.remove(root, &nth_name(0)).expect("remove");
    let after = fs
        .create(root, b"after", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create after free");
    let chunk = [0x11u8; 4096];
    assert_eq!(fs.write_at(after, 0, &chunk), Ok(chunk.len()));
}

#[test]
fn format_inode_table_fills_to_no_space() {
    // 16 inodes per group, 10 reserved (1..=10) → exactly 6 usable.
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 16, TEST_UUID).expect("format");
    let root = fs.root();
    let mut created = 0usize;
    loop {
        let name = nth_name(created);
        match fs.create(root, &name, NodeKind::RegularFile, &FILE_RECORD) {
            Ok(_) => created += 1,
            Err(DriverError::NoSpace) => break,
            Err(e) => panic!("unexpected error exhausting inodes: {e:?}"),
        }
        assert!(created <= 64, "inode table never reported full");
    }
    assert_eq!(created, 6);
}

#[test]
fn format_spans_multiple_block_groups() {
    let mut fs = Ext4::format(SizedBlock::new(TWO_GROUP_SECTORS), 1024, TEST_UUID).expect("format");
    let root = fs.root();
    let written = fill_to_no_space(&mut fs, root);
    // One block group holds 8 MiB of data blocks; writing past that
    // proves the allocator crossed into the second block group.
    assert!(
        written > 8 * 1024 * 1024,
        "expected a multi-group fill, only wrote {written} bytes"
    );

    // Remount and confirm an early file survives, including a block in
    // the second group's address range.
    let mut fs = Ext4::open(fs.into_block().expect("commit")).expect("reopen");
    let file = fs.lookup(fs.root(), &nth_name(0)).expect("found");
    let mut buf = [0u8; 4096];
    assert_eq!(fs.read_at(file, 0, &mut buf), Ok(4096));
    assert!(buf.iter().all(|&b| b == 0xAB));
}

#[test]
fn rename_within_directory_preserves_contents() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let a_txt = fs
        .create(root, b"a.txt", NodeKind::RegularFile, &FILE_RECORD)
        .unwrap();
    fs.write_at(a_txt, 0, b"hello").unwrap();
    fs.rename(root, b"a.txt", root, b"b.txt").expect("rename");
    assert_eq!(fs.lookup(root, b"a.txt"), Err(DriverError::NotFound));
    let node = fs.lookup(root, b"b.txt").expect("dst");
    let mut buf = [0u8; 8];
    let n = fs.read_at(node, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello");
}

#[test]
fn rename_missing_source_is_not_found() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    assert_eq!(
        fs.rename(root, b"nope", root, b"x"),
        Err(DriverError::NotFound)
    );
}

#[test]
fn rename_across_directories_persists() {
    let dev = {
        let mut fs =
            Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
        let root = fs.root();
        let src = fs
            .create(root, b"src", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        let dst = fs
            .create(root, b"dst", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        let f_bin = fs
            .create(src, b"f.bin", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        fs.write_at(f_bin, 0, b"data").unwrap();
        fs.rename(src, b"f.bin", dst, b"g.bin").expect("move");
        fs.into_block().expect("commit")
    };
    let mut fs = Ext4::open(dev).expect("reopen");
    let root = fs.root();
    let src = fs.lookup(root, b"src").unwrap();
    let dst = fs.lookup(root, b"dst").unwrap();
    assert_eq!(fs.lookup(src, b"f.bin"), Err(DriverError::NotFound));
    let node = fs.lookup(dst, b"g.bin").expect("moved");
    let mut buf = [0u8; 8];
    let n = fs.read_at(node, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"data");
}

#[test]
fn rename_overwrites_existing_file() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let a_txt = fs
        .create(root, b"a.txt", NodeKind::RegularFile, &FILE_RECORD)
        .unwrap();
    fs.write_at(a_txt, 0, b"AAAA").unwrap();
    let b_txt = fs
        .create(root, b"b.txt", NodeKind::RegularFile, &FILE_RECORD)
        .unwrap();
    fs.write_at(b_txt, 0, b"BB").unwrap();
    fs.rename(root, b"a.txt", root, b"b.txt")
        .expect("overwrite");
    assert_eq!(fs.lookup(root, b"a.txt"), Err(DriverError::NotFound));
    let node = fs.lookup(root, b"b.txt").unwrap();
    let mut buf = [0u8; 8];
    let n = fs.read_at(node, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"AAAA");
}

#[test]
fn rename_refuses_kind_mismatch_and_nonempty_dir_target() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    fs.create(root, b"f.txt", NodeKind::RegularFile, &FILE_RECORD)
        .unwrap();
    fs.create(root, b"d", NodeKind::Directory, &DIR_RECORD)
        .unwrap();
    assert_eq!(
        fs.rename(root, b"f.txt", root, b"d"),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        fs.rename(root, b"d", root, b"f.txt"),
        Err(DriverError::Unsupported)
    );
    let d2 = fs
        .create(root, b"d2", NodeKind::Directory, &DIR_RECORD)
        .unwrap();
    fs.create(d2, b"child", NodeKind::RegularFile, &FILE_RECORD)
        .unwrap();
    assert_eq!(
        fs.rename(root, b"d", root, b"d2"),
        Err(DriverError::DirectoryNotEmpty)
    );
}

#[test]
fn rename_moves_a_directory_across_parents() {
    let dev = {
        let mut fs =
            Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
        let root = fs.root();
        let p1 = fs
            .create(root, b"p1", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        let p2 = fs
            .create(root, b"p2", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        let d = fs
            .create(p1, b"d", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        let leaf_bin = fs
            .create(d, b"leaf.bin", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        fs.write_at(leaf_bin, 0, b"x").unwrap();
        fs.rename(p1, b"d", p2, b"d").expect("move dir");
        fs.into_block().expect("commit")
    };
    let mut fs = Ext4::open(dev).expect("reopen");
    let root = fs.root();
    let p1 = fs.lookup(root, b"p1").unwrap();
    let p2 = fs.lookup(root, b"p2").unwrap();
    assert_eq!(fs.lookup(p1, b"d"), Err(DriverError::NotFound));
    let moved = fs.lookup(p2, b"d").expect("moved");
    let leaf = fs.lookup(moved, b"leaf.bin").expect("leaf intact");
    let mut buf = [0u8; 4];
    let n = fs.read_at(leaf, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"x");
}

#[test]
fn rename_refuses_moving_directory_into_its_subtree() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let a = fs
        .create(root, b"a", NodeKind::Directory, &DIR_RECORD)
        .unwrap();
    let b = fs
        .create(a, b"b", NodeKind::Directory, &DIR_RECORD)
        .unwrap();
    assert_eq!(
        fs.rename(root, b"a", b, b"a"),
        Err(DriverError::DirectoryCycle)
    );
    assert_eq!(
        fs.rename(root, b"a", a, b"x"),
        Err(DriverError::DirectoryCycle)
    );
}

#[test]
fn rename_rejects_bad_destination_name() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    fs.create(root, b"a.txt", NodeKind::RegularFile, &FILE_RECORD)
        .unwrap();
    assert_eq!(
        fs.rename(root, b"a.txt", root, b""),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        fs.rename(root, b"a.txt", root, b".."),
        Err(DriverError::LengthOutOfRange)
    );
}

// --- Allocated storage (`NodeInfo::allocated` from `i_blocks`). ---

#[test]
fn node_info_reports_allocation_from_i_blocks() {
    // 10 sectors in the low half plus 1 in the osd2 high half, which only a
    // `huge_file` volume counts.
    let mut img = build_image();
    let base = inode_offset(11);
    set_le32(&mut img, base + 0x1C, 10);
    set_le16(&mut img, base + 0x74, 1);
    let mut huge = img.clone();
    let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("offset fits");
    set_le32(&mut huge, sb + 0x64, superblock::RO_COMPAT_HUGE_FILE);
    for (image, sectors) in [(huge, (1u64 << 32) + 10), (img, 10)] {
        let mut fs = Ext4::open(SizedBlock::over(image)).expect("valid volume");
        let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
        assert_eq!(fs.node_info(file).expect("info").allocated, sectors * 512);
    }
}

#[test]
fn node_info_scales_huge_file_allocation_by_the_block_size() {
    let mut img = build_image();
    let base = inode_offset(11);
    set_le32(&mut img, base + 0x1C, 3);
    set_le32(
        &mut img,
        base + 0x20,
        INODE_FLAG_EXTENTS | INODE_FLAG_HUGE_FILE,
    );
    let mut fs = Ext4::open(SizedBlock::over(img)).expect("valid volume");
    let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
    let info = fs.node_info(file).expect("info");
    // The huge-file flag makes `i_blocks` count filesystem blocks.
    assert_eq!(info.allocated, 3 * FS_BLOCK as u64);
}

#[test]
fn blocks_high_half_never_leaks_into_file_acl() {
    // Regression: `l_i_blocks_high` (osd2 offset 0x74) was decoded as the
    // `i_file_acl` high half, sending the ACL reader to a bogus xattr
    // block. With `i_file_acl` zero, a non-zero blocks high half must
    // leave the security record ACL-free and readable.
    let mut img = build_image();
    set_le16(&mut img, inode_offset(11) + 0x74, 0x7FFF);
    let mut fs = Ext4::open(SizedBlock::over(img)).expect("valid volume");
    let file = fs.lookup(fs.root(), b"hello.txt").expect("found");
    let sec = fs.security(file).expect("security reads cleanly");
    assert_eq!(sec.mode, 0o644);
    assert!(sec.acl().is_empty());
}

#[test]
fn writing_grows_the_reported_allocation() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let file = fs
        .create(root, b"grow.bin", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(fs.node_info(file).expect("info").allocated, 0);
    let payload = [7u8; FS_BLOCK + 1];
    fs.write_at(file, 0, &payload).expect("write");
    // Two data blocks were allocated and recorded in `i_blocks`.
    assert_eq!(
        fs.node_info(file).expect("info").allocated,
        2 * FS_BLOCK as u64
    );
    fs.truncate(file, 1).expect("truncate");
    // The freed tail block leaves the surviving one accounted.
    assert_eq!(fs.node_info(file).expect("info").allocated, FS_BLOCK as u64);
}

#[test]
fn inode_time_classic_field_is_signed() {
    // Without an extra field the classic 32-bit seconds are signed: a
    // pre-1970 stamp decodes below zero, never as a huge unsigned value.
    assert_eq!(
        crate::decode_inode_time(0xFFFF_FFFF, None),
        Ok(Time64::from_secs(-1))
    );
    assert_eq!(
        crate::decode_inode_time(1_700_000_000, None),
        Ok(Time64::from_secs(1_700_000_000))
    );
}

#[test]
fn inode_time_extra_carries_the_epoch_and_the_nanoseconds() {
    assert_eq!(
        crate::decode_inode_time(5, Some(0b01)),
        Ok(Time64::from_secs((1 << 32) + 5))
    );
    // Without epoch bits the extra field still carries the nanoseconds.
    assert_eq!(
        crate::decode_inode_time(10, Some(500 << 2)),
        Time64::new(10, 500).map_err(|_| tairix_abi::DriverError::DeviceFault)
    );
}

#[test]
fn inode_time_corrupt_nanoseconds_fail_closed() {
    // A nanosecond count of a full second cannot come from a valid encode;
    // it is corruption and is refused, never clamped.
    assert_eq!(
        crate::decode_inode_time(0, Some(1_000_000_000 << 2)),
        Err(tairix_abi::DriverError::DeviceFault)
    );
}

#[test]
fn inode_time_decodes_every_epoch_of_the_extended_layout() {
    // The ext4 disk layout's table: each epoch adds 2^32 seconds to the
    // signed classic field, whichever way its top bit lies.
    for (base, epoch, secs) in [
        (0x8000_0000_u32, 0_u32, -(1_i64 << 31)),
        (0x7FFF_FFFF, 0, (1 << 31) - 1),
        (0x8000_0000, 1, 1 << 31),
        (0x0000_0000, 1, 1 << 32),
        (0x8000_0000, 2, (1 << 31) + (1 << 32)),
        (0x0000_0000, 2, 1 << 33),
        (0x8000_0000, 3, (1 << 31) + (1 << 33)),
        (0x7FFF_FFFF, 3, (1 << 31) - 1 + 3 * (1 << 32)),
    ] {
        assert_eq!(
            crate::decode_inode_time(base, Some(epoch)),
            Ok(Time64::from_secs(secs)),
            "epoch {epoch} over {base:#x}"
        );
    }
}

#[test]
fn a_stamp_after_2038_reads_as_its_own_year() {
    // 2040-01-01T00:00:00Z as the extra-epoch encoding stores it: the low
    // word, whose top bit is
    // set, and epoch 1. Read as epoch-over-unsigned, it came back as 2176.
    assert_eq!(
        crate::decode_inode_time(2_208_988_800, Some(1)),
        Ok(Time64::from_secs(2_208_988_800))
    );
}

#[test]
fn inode_time_round_trips_the_range_each_record_holds() {
    let max = (1_i64 << 31) - 1 + 3 * (1 << 32);
    for time in [
        Time64::from_secs(-(1 << 31)),
        Time64::new(-1, 999_999_999).expect("canonical"),
        Time64::UNIX_EPOCH,
        Time64::from_secs((1 << 31) - 1),
        Time64::from_secs(1 << 31),
        Time64::new(2_208_988_800, 123_456_789).expect("canonical"),
        Time64::from_secs(1 << 32),
        Time64::new(max, 999_999_999).expect("canonical"),
    ] {
        let (secs, extra) = crate::encode_inode_time(time, true).expect("in range");
        assert_eq!(crate::decode_inode_time(secs, Some(extra)), Ok(time));
    }
    assert_eq!(
        crate::encode_inode_time(Time64::from_secs(-(1 << 31) - 1), true),
        None
    );
    assert_eq!(
        crate::encode_inode_time(Time64::from_secs(max + 1), true),
        None
    );

    // A classic record keeps whole seconds and stops at 2038.
    let (secs, _) =
        crate::encode_inode_time(Time64::new((1 << 31) - 1, 7).expect("canonical"), false)
            .expect("in range");
    assert_eq!(
        crate::decode_inode_time(secs, None),
        Ok(Time64::from_secs((1 << 31) - 1))
    );
    assert_eq!(
        crate::encode_inode_time(Time64::from_secs(1 << 31), false),
        None
    );
}

/// `fs`, stamping `wall` from now on, or no time.
fn at<B: Block>(mut fs: Ext4<B>, wall: Option<Time64>) -> Ext4<B> {
    fs.set_volume_host(TestClock::volume(), TestClock::at(wall));
    fs
}

#[test]
fn each_operation_stamps_what_it_changed_at_the_hosts_time() {
    let t1 = Time64::new(2_208_988_800, 5).expect("canonical");
    let t2 = Time64::new(2_208_988_900, 6).expect("canonical");
    let t3 = Time64::new(2_208_989_000, 7).expect("canonical");
    let mut fs = at(
        Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format"),
        Some(t1),
    );
    let root = fs.root();
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let made = fs.node_info(file).expect("info").times;
    assert_eq!(
        (made.created, made.modified, made.changed, made.accessed),
        (t1, t1, t1, t1)
    );
    let parent = fs.node_info(root).expect("info").times;
    assert_eq!((parent.modified, parent.changed), (t1, t1));

    fs = at(fs, Some(t2));
    fs.write_at(file, 0, b"data").expect("write");
    let written = fs.node_info(file).expect("info").times;
    assert_eq!(
        (
            written.created,
            written.accessed,
            written.modified,
            written.changed
        ),
        (t1, t1, t2, t2)
    );

    fs = at(fs, Some(t3));
    fs.rename(root, b"f", root, b"g").expect("rename");
    let moved = fs.node_info(file).expect("info").times;
    assert_eq!(
        (moved.modified, moved.changed),
        (t2, t3),
        "a rename changes the name, never the contents"
    );
    assert_eq!(fs.node_info(root).expect("info").times.modified, t3);

    // The stamps are on the medium, not only in the handle.
    let mut fs = Ext4::open(fs.into_block().expect("commit")).expect("reopen");
    let file = fs.lookup(fs.root(), b"g").expect("found");
    let kept = fs.node_info(file).expect("info").times;
    assert_eq!((kept.created, kept.modified, kept.changed), (t1, t2, t3));
}

#[test]
fn with_no_time_to_give_ext4_records_no_stamp() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let times = fs.node_info(file).expect("info").times;
    assert_eq!(
        (times.created, times.modified, times.changed, times.accessed),
        (
            Time64::UNIX_EPOCH,
            Time64::UNIX_EPOCH,
            Time64::UNIX_EPOCH,
            Time64::UNIX_EPOCH
        )
    );

    // A time the record cannot hold is no stamp either, never a wrapped one.
    let mut fs = at(fs, Some(Time64::from_secs(1 << 40)));
    fs.write_at(file, 0, b"data").expect("write");
    assert_eq!(
        fs.node_info(file).expect("info").times.modified,
        Time64::UNIX_EPOCH
    );
}

#[test]
fn a_freed_inode_records_when_it_went_or_the_marker_when_that_is_unknown() {
    let gone = Time64::from_secs(1_900_000_000);
    let mut fs = at(
        Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format"),
        Some(gone),
    );
    let root = fs.root();
    let dated = fs
        .create(root, b"dated", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let undated = fs
        .create(root, b"undated", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.remove(root, b"dated").expect("remove");
    let mut fs = at(fs, None);
    fs.remove(root, b"undated").expect("remove");

    let mut raw = fs.inode_scratch().expect("scratch");
    for (node, dtime) in [(dated, 1_900_000_000), (undated, crate::DELETED_DTIME)] {
        fs.read_inode_raw(node_inode(node).expect("an inode"), &mut raw)
            .expect("read");
        assert_eq!(crate::le32(&raw, crate::INODE_DTIME), dtime);
    }
}

#[test]
fn a_classic_record_keeps_whole_seconds_and_no_birth_time() {
    let stamp = Time64::new(1_900_000_000, 999).expect("canonical");
    let mut fs = at(mount(), Some(stamp));
    let root = fs.root();
    let file = fs
        .create(root, b"classic", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let times = fs.node_info(file).expect("info").times;
    assert_eq!(times.modified, Time64::from_secs(1_900_000_000));
    assert_eq!(times.created, Time64::UNIX_EPOCH);
}

// ---------------------------------------------------------------------------
// Symbolic links: both on-disk spellings ext4 uses for a target.
// ---------------------------------------------------------------------------

/// Target of the planted fast (inline `i_block`) symlink: short enough to fit
/// the 60-byte array.
const FAST_LINK_TARGET: &[u8] = b"/System/Commands/ls.app";
/// Target of the planted slow (block-backed) symlink: longer than the
/// `i_block` array, so it could not be inline whatever the accounting says.
const SLOW_LINK_TARGET: &[u8] =
    b"/Users/someone/Documents/a/deliberately/long/path/that/cannot/fit/inline";
/// Free inode the fast symlink is planted at.
const FAST_LINK_INO: u32 = 15;
/// Free inode the slow symlink is planted at.
const SLOW_LINK_INO: u32 = 16;
/// Free data block holding the slow symlink's target.
const SLOW_LINK_BLOCK: u32 = 15;

/// Plant a **fast** symlink at `ino`: the target lives in the raw `i_block`
/// array and the inode allocates nothing, so `i_blocks` stays zero.
fn plant_fast_symlink(img: &mut [u8], ino: u32, target: &[u8]) {
    let base = inode_offset(ino);
    set_le16(img, base, S_IFLNK | 0o777);
    set_le16(img, base + 0x1A, default_links(S_IFLNK));
    set_le32(img, base + 0x04, u32c(target.len()));
    set_le32(img, base + 0x20, 0);
    let ib = base + I_BLOCK_OFFSET;
    img[ib..ib + target.len()].copy_from_slice(target);
}

/// Plant a **slow** symlink at `ino`: the target is ordinary extent-mapped
/// file data, accounted for in `i_blocks` like any other allocation.
fn plant_slow_symlink(img: &mut [u8], ino: u32, target: &[u8], data_block: u32) {
    write_extent_inode(
        img,
        ino,
        S_IFLNK | 0o777,
        u32c(target.len()),
        &[(0, 1, data_block)],
    );
    set_le32(
        img,
        inode_offset(ino) + INODE_BLOCKS_LO,
        u32c(FS_BLOCK / DEV_SECTOR),
    );
    let off = block_offset(data_block);
    img[off..off + target.len()].copy_from_slice(target);
}

fn mount_with_links() -> Ext4<SizedBlock> {
    let mut img = build_image();
    plant_fast_symlink(&mut img, FAST_LINK_INO, FAST_LINK_TARGET);
    plant_slow_symlink(&mut img, SLOW_LINK_INO, SLOW_LINK_TARGET, SLOW_LINK_BLOCK);
    Ext4::open(SizedBlock::over(img)).expect("image is a valid ext4 volume")
}

#[test]
fn a_fast_symlinks_target_is_read_from_the_inline_i_block() {
    let mut fs = mount_with_links();
    let node = NodeId::from_raw(u64::from(FAST_LINK_INO));
    let info = fs.node_info(node).expect("stat the fast link");
    assert_eq!(info.kind, NodeKind::Symlink);
    assert_eq!(info.size, FAST_LINK_TARGET.len() as u64);

    let mut out = [0u8; 128];
    assert_eq!(fs.read_link(node, &mut out), Ok(FAST_LINK_TARGET.len()));
    assert_eq!(&out[..FAST_LINK_TARGET.len()], FAST_LINK_TARGET);
}

#[test]
fn a_slow_symlinks_target_is_read_from_its_data_blocks() {
    let mut fs = mount_with_links();
    let node = NodeId::from_raw(u64::from(SLOW_LINK_INO));
    let info = fs.node_info(node).expect("stat the slow link");
    assert_eq!(info.kind, NodeKind::Symlink);
    assert_eq!(info.size, SLOW_LINK_TARGET.len() as u64);

    let mut out = [0u8; 128];
    assert_eq!(fs.read_link(node, &mut out), Ok(SLOW_LINK_TARGET.len()));
    assert_eq!(&out[..SLOW_LINK_TARGET.len()], SLOW_LINK_TARGET);
}

#[test]
fn a_short_target_with_allocated_blocks_is_read_as_a_slow_symlink() {
    // The `i_blocks` accounting is the discriminator when a target would fit
    // inline but is nevertheless block-backed: reading `i_block` would hand
    // back extent-header bytes instead of a path.
    let short: &[u8] = b"/tiny/target";
    let mut img = build_image();
    plant_slow_symlink(&mut img, FAST_LINK_INO, short, SLOW_LINK_BLOCK);
    let mut fs = Ext4::open(SizedBlock::over(img)).expect("valid volume");

    let mut out = [0u8; 64];
    let node = NodeId::from_raw(u64::from(FAST_LINK_INO));
    assert_eq!(fs.read_link(node, &mut out), Ok(short.len()));
    assert_eq!(&out[..short.len()], short);
}

#[test]
fn read_link_refuses_a_non_link_and_an_undersized_buffer() {
    let mut fs = mount_with_links();
    let mut out = [0u8; 128];
    // A regular file and a directory have no target.
    assert_eq!(
        fs.read_link(NodeId::from_raw(11), &mut out),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        fs.read_link(NodeId::from_raw(u64::from(ROOT_INODE)), &mut out),
        Err(DriverError::Unsupported)
    );
    // A buffer too small is refused, never truncated.
    let mut small = [0u8; 4];
    assert_eq!(
        fs.read_link(NodeId::from_raw(u64::from(FAST_LINK_INO)), &mut small),
        Err(DriverError::BufferTooSmall)
    );
    assert_eq!(small, [0u8; 4]);
}

#[test]
fn a_links_bytes_are_never_readable_and_a_link_is_never_creatable() {
    let mut fs = mount_with_links();
    let mut buf = [0u8; 16];
    // A link's content is a path, not a byte stream.
    assert_eq!(
        fs.read_at(NodeId::from_raw(u64::from(FAST_LINK_INO)), 0, &mut buf),
        Err(DriverError::Unsupported)
    );
    // This driver reads links but does not author them, so creation refuses
    // rather than substituting a regular file holding the target's text.
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"alias", NodeKind::Symlink, &LINK_RECORD),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        fs.create_link(root, b"alias", b"/target", &LINK_RECORD),
        Err(DriverError::Unsupported)
    );
    assert_eq!(fs.lookup(root, b"alias"), Err(DriverError::NotFound));
}

#[test]
fn an_inline_data_link_is_read_from_its_inode() {
    let mut img = build_image();
    plant_fast_symlink(&mut img, FAST_LINK_INO, FAST_LINK_TARGET);
    set_le32(
        &mut img,
        inode_offset(FAST_LINK_INO) + 0x20,
        INODE_FLAG_INLINE_DATA,
    );
    let mut on = img.clone();
    let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("offset fits");
    set_le32(&mut on, sb + 0x60, INCOMPAT_FILETYPE | INCOMPAT_INLINE_DATA);
    let mut fs = Ext4::open(SizedBlock::over(on)).expect("valid volume");
    let mut out = [0u8; 128];
    let node = NodeId::from_raw(u64::from(FAST_LINK_INO));
    assert_eq!(fs.read_link(node, &mut out), Ok(FAST_LINK_TARGET.len()));
    assert_eq!(&out[..FAST_LINK_TARGET.len()], FAST_LINK_TARGET);

    // Inline contents on a volume that never enabled them are damage.
    let mut fs = Ext4::open(SizedBlock::over(img)).expect("valid volume");
    assert_eq!(fs.read_link(node, &mut out), Err(DriverError::DeviceFault));
}

#[test]
fn a_link_with_no_target_is_refused_as_corrupt() {
    let mut img = build_image();
    plant_fast_symlink(&mut img, FAST_LINK_INO, FAST_LINK_TARGET);
    // A zero-length target is structurally impossible.
    set_le32(&mut img, inode_offset(FAST_LINK_INO) + 0x04, 0);
    let mut fs = Ext4::open(SizedBlock::over(img)).expect("valid volume");

    let mut out = [0u8; 128];
    assert_eq!(
        fs.read_link(NodeId::from_raw(u64::from(FAST_LINK_INO)), &mut out),
        Err(DriverError::DeviceFault)
    );
}

/// Plant a second directory entry in the root naming inode `ino`, and record
/// the higher `i_links_count` a real writer would have left behind — the
/// shape of any ext4 volume another writer has made a hard link on.
fn plant_second_name(img: &mut [u8], name: &[u8], ino: u32) {
    let base = inode_offset(ino);
    let links = u16::from_le_bytes([img[base + 0x1A], img[base + 0x1A + 1]]);
    set_le16(img, base + 0x1A, links + 1);

    // Re-lay the root block with the extra entry; the last one's `rec_len`
    // covers the rest of the block, as on disk.
    let off = block_offset(ROOT_DATA_BLOCK);
    let block = &mut img[off..off + FS_BLOCK];
    block.fill(0);
    let mut pos = put_dirent(block, 0, ROOT_INODE, b".", FT_DIR, false);
    pos = put_dirent(block, pos, ROOT_INODE, b"..", FT_DIR, false);
    pos = put_dirent(block, pos, 11, b"hello.txt", FT_REG, false);
    pos = put_dirent(block, pos, 12, b"classic.bin", FT_REG, false);
    pos = put_dirent(block, pos, 13, b"sub", FT_DIR, false);
    let _ = put_dirent(block, pos, ino, name, FT_REG, true);
}

#[test]
fn unlinking_one_name_of_a_foreign_hard_link_keeps_the_other_readable() {
    // This driver authors no hard links, but it writes volumes that hold
    // them. Freeing the inode on the first unlink would destroy data the
    // other name still reaches — silent corruption of a foreign volume.
    let mut img = build_image();
    plant_second_name(&mut img, b"hello.alias", 11);
    let mut fs = Ext4::open(SizedBlock::over(img)).expect("valid volume");
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("the file");
    assert_eq!(fs.node_info(file).expect("stat").nlink, 2);

    fs.remove(root, b"hello.txt").expect("drop one name");
    assert_eq!(fs.lookup(root, b"hello.txt"), Err(DriverError::NotFound));

    let alias = fs.lookup(root, b"hello.alias").expect("the other name");
    assert_eq!(fs.node_info(alias).expect("stat").nlink, 1);
    let mut buf = [0u8; 64];
    let n = fs.read_at(alias, 0, &mut buf).expect("still readable");
    assert_eq!(&buf[..n], HELLO_BODY);

    // And the data survives a remount: the inode was never freed.
    let mut fs = remount(fs);
    let alias = fs.lookup(fs.root(), b"hello.alias").expect("still there");
    let n = fs.read_at(alias, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], HELLO_BODY);
}

#[test]
fn unlinking_the_last_name_of_a_foreign_hard_link_frees_the_inode() {
    let mut img = build_image();
    plant_second_name(&mut img, b"hello.alias", 11);
    let mut fs = Ext4::open(SizedBlock::over(img)).expect("valid volume");
    let root = fs.root();
    fs.remove(root, b"hello.txt").expect("drop one name");
    fs.remove(root, b"hello.alias").expect("drop the last name");

    // The inode is free again: a fresh create reuses it and round-trips.
    let again_txt = fs
        .create(root, b"again.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create reuses the freed inode");
    fs.write_at(again_txt, 0, b"reused").expect("write");
    let mut fs = remount(fs);
    let file = fs.lookup(fs.root(), b"again.txt").expect("found");
    let mut buf = [0u8; 16];
    let n = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"reused");
}

#[test]
fn the_link_count_is_reported_from_the_volumes_own_record() {
    // Read, never derived: a directory carries its `.` and its name, a file
    // carries the one name, and a planted second name shows as two.
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(fs.node_info(root).expect("stat").nlink, 2);
    let file = fs.lookup(root, b"hello.txt").expect("the file");
    assert_eq!(fs.node_info(file).expect("stat").nlink, 1);
}

#[test]
fn ext4_reads_link_counts_but_authors_no_second_name() {
    // The same posture the driver already takes for `create_link`: it reports
    // what a foreign writer recorded and refuses to author one itself.
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("the file");
    assert_eq!(fs.link(root, b"alias", file), Err(DriverError::Unsupported));
    assert_eq!(fs.lookup(root, b"alias"), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(file).expect("stat").nlink, 1);
}

/// The medium a [`FailingWrites`] device serves, shared with the test: its
/// bytes as they stand, which a crash at any moment would leave, and the
/// sectors whose writes fail.
#[derive(Default)]
struct Medium {
    data: Vec<u8>,
    failing: Option<core::ops::Range<u64>>,
}

type Shared = alloc::rc::Rc<core::cell::RefCell<Medium>>;

/// A device of 512-byte sectors over a [`Medium`] the test holds too.
struct FailingWrites(Shared);

impl FailingWrites {
    /// A device over `data`, and the test's handle on its medium.
    fn over(data: Vec<u8>) -> (Self, Shared) {
        let medium = Shared::new(core::cell::RefCell::new(Medium {
            data,
            failing: None,
        }));
        (Self(alloc::rc::Rc::clone(&medium)), medium)
    }

    fn span(&self, lba: u64, len: usize) -> Result<core::ops::Range<usize>, DriverError> {
        let start = usize::try_from(lba).map_err(|_| DriverError::OutOfRange)? * FMT_SECTOR;
        let end = start + len;
        if !len.is_multiple_of(FMT_SECTOR) || end > self.0.borrow().data.len() {
            return Err(DriverError::OutOfRange);
        }
        Ok(start..end)
    }
}

impl Block for FailingWrites {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: u32c(FMT_SECTOR),
            block_count: (self.0.borrow().data.len() / FMT_SECTOR) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        buf.copy_from_slice(&self.0.borrow().data[span]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        let sectors = (buf.len() / FMT_SECTOR) as u64;
        let mut medium = self.0.borrow_mut();
        if medium
            .failing
            .as_ref()
            .is_some_and(|range| lba < range.end && range.start < lba + sectors)
        {
            return Err(DriverError::DeviceFault);
        }
        medium.data[span].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// The device sectors filesystem block `block` covers.
fn sectors_of<B: Block>(fs: &Ext4<B>, block: u64) -> core::ops::Range<u64> {
    let per = u64::from(fs.layout.block_size) / FMT_SECTOR as u64;
    block * per..(block + 1) * per
}

/// The table block holding inode `ino`.
fn inode_block<B: Block>(fs: &mut Ext4<B>, ino: u32) -> u64 {
    fs.inode_location(ino).expect("the record").0
}

/// What a power loss now would leave on `medium`, mounted.
fn crashed(medium: &Shared) -> Ext4<SizedBlock> {
    Ext4::open(SizedBlock::over(medium.borrow().data.clone())).expect("mounts after the crash")
}

/// The physical block holding logical block 0 of directory `dir`.
fn first_dir_block<B: Block>(fs: &mut Ext4<B>, dir: NodeId) -> u64 {
    let inode = fs
        .read_inode(node_inode(dir).expect("inode"))
        .expect("read");
    fs.map_block(&inode, 0).expect("map").expect("mapped")
}

/// A linear directory block changed under its checksum is refused, whether it
/// is searched or listed, rather than read past.
#[test]
fn a_directory_block_failing_its_checksum_is_refused() {
    let mut fs = checksummed();
    let root = fs.root();
    let found = fs.lookup(root, b"lost+found").expect("lookup");
    let phys = first_dir_block(&mut fs, root);
    let mut data = vec![0u8; fs.layout.block_size as usize];
    fs.read_fs_block(phys, &mut data).expect("read");
    data[0] ^= 1;
    fs.write_fs_block(phys, &data).expect("write");
    assert_eq!(
        fs.lookup(root, b"lost+found"),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        fs.read_dir(root, 0, &[], &mut |_, _| DirVisit::Take),
        Err(DriverError::DeviceFault)
    );
    data[0] ^= 1;
    fs.write_fs_block(phys, &data).expect("restore");
    assert_eq!(fs.lookup(root, b"lost+found"), Ok(found));
}

/// `mke2fs -t ext4 -b 1024 -J size=1 -O metadata_csum,^resize_inode` over
/// 8 MiB (e2fsprogs 1.47.0): one group, `64bit`, `flex_bg`, `dir_index`,
/// and a 1024-block internal journal.
const JOURNALED: &[u8] = include_bytes!("../tests/fixtures/journaled.sparse");

/// `mke2fs -t ext4 -b 1024 -J size=1 -O 64bit,^resize_inode -E desc_size=128`
/// over 8 MiB (e2fsprogs 1.47.0), with `debugfs` writing `hello.txt`: group
/// descriptors twice the 64 bytes this driver decodes.
const WIDE_DESC: &[u8] = include_bytes!("../tests/fixtures/wide_desc.sparse");

/// A descriptor wider than the part this driver decodes is read and restamped
/// whole — its checksum covers all of it — rather than overrunning a buffer.
#[test]
fn a_volume_of_128_byte_descriptors_reads_writes_and_remounts() {
    let mut fs = Ext4::open(SizedBlock::over(sparse(WIDE_DESC))).expect("mount");
    assert_eq!(fs.layout.desc_size, 128);
    let root = fs.root();
    let hello = fs.lookup(root, b"hello.txt").expect("lookup");
    let mut buf = [0u8; 32];
    let n = fs.read_at(hello, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"wide descriptors\n");
    let made_file = fs
        .create(root, b"made", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(made_file, 0, b"here").expect("write");

    let mut fs = Ext4::open(fs.into_block().expect("commit")).expect("remount");
    let root = fs.root();
    let made = fs.lookup(root, b"made").expect("lookup");
    let n = fs.read_at(made, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"here");
}

/// The sectors of the superblock, which an unjournaled publication marks
/// not clean before anything else.
fn superblock_sectors() -> core::ops::Range<u64> {
    let first = SUPERBLOCK_OFFSET / FMT_SECTOR as u64;
    first..first + (SUPERBLOCK_LEN / FMT_SECTOR) as u64
}

/// A commit that fails before anything reaches the medium as the new state
/// takes the operation back whole; no operation reported done is lost, so
/// the handle goes on accepting writes.
#[test]
fn a_commit_failing_before_the_medium_changes_takes_the_operation_back() {
    let fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let (device, medium) = FailingWrites::over(fs.into_block().expect("commit").data);
    let mut fs = Ext4::open(device).expect("mount");
    let root = fs.root();
    let victim = fs
        .create(root, b"victim", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(victim, 0, b"still here").expect("write");
    let before = fs.stats().expect("stats");
    medium.borrow_mut().failing = Some(superblock_sectors());
    assert_eq!(fs.remove(root, b"victim"), Err(DriverError::DeviceFault));
    medium.borrow_mut().failing = None;
    assert_eq!(fs.write_refusal(), None, "nothing acknowledged was lost");
    assert_eq!(fs.lookup(root, b"victim"), Ok(victim));
    assert_eq!(fs.stats().expect("stats"), before, "nothing was freed");
    let mut buf = [0u8; 16];
    let read = fs.read_at(victim, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], b"still here");
    fs.remove(root, b"victim")
        .expect("a later operation succeeds");
}

/// A commit torn part-way on an unjournaled volume stops the handle, and a
/// crash then leaves the volume marked not clean: it mounts read-only until
/// a checker has seen it.
#[test]
fn a_torn_commit_on_an_unjournaled_volume_leaves_it_for_a_checker() {
    let fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let (device, medium) = FailingWrites::over(fs.into_block().expect("commit").data);
    let mut fs = Ext4::open(device).expect("mount");
    let root = fs.root();
    let table = inode_block(&mut fs, ROOT_INODE);
    medium.borrow_mut().failing = Some(sectors_of(&fs, table));
    assert_eq!(
        fs.create(root, b"f", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::CommitFailed));
    assert_eq!(
        fs.create(root, b"g", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::ReadOnly),
        "a stopped handle takes no further operation"
    );
    let mut after = crashed(&medium);
    assert_eq!(after.write_refusal(), Some(WriteRefusal::NeedsCheck));
    assert!(after.node_info(after.root()).is_ok(), "reads go on");
    // The device recovers and the handle is closed: what it holds goes out
    // whole and the volume is clean again.
    medium.borrow_mut().failing = None;
    fs.flush().expect("published once the device takes it");
    let mut fixed = crashed(&medium);
    assert_eq!(fixed.write_refusal(), None);
    assert!(fixed.lookup(fixed.root(), b"f").is_ok());
}

/// A commit torn after its journal commit is durable is finished at the next
/// mount by replaying the journal: the operation is there whole, and the
/// volume is writable again.
#[test]
fn a_commit_torn_after_the_journal_holds_it_is_replayed_at_mount() {
    let (device, medium) = FailingWrites::over(sparse(JOURNALED));
    let mut fs = Ext4::open(device).expect("mount");
    assert!(fs.journal.is_some());
    let root = fs.root();
    let table = inode_block(&mut fs, ROOT_INODE);
    medium.borrow_mut().failing = Some(sectors_of(&fs, table));
    assert_eq!(
        fs.create(root, b"kept", NodeKind::Directory, &DIR_RECORD),
        Err(DriverError::DeviceFault),
        "the home write failed"
    );
    let at = usize::try_from(SUPERBLOCK_OFFSET).expect("small") + SB_FEATURE_INCOMPAT;
    assert_ne!(
        le32(&medium.borrow().data, at) & INCOMPAT_RECOVER,
        0,
        "needs recovery"
    );
    let mut fs = crashed(&medium);
    assert_eq!(fs.write_refusal(), None);
    let kept = fs.lookup(fs.root(), b"kept").expect("replayed whole");
    assert_eq!(fs.node_info(kept).expect("stat").kind, NodeKind::Directory);
    assert_eq!(
        fs.node_info(fs.root()).expect("stat").nlink,
        4,
        "., lost+found, kept"
    );
    fs.create(kept, b"inside", NodeKind::RegularFile, &FILE_RECORD)
        .expect("writable");
    let mut fs = remount(fs);
    assert!(fs.lookup(kept, b"inside").is_ok());
    let device = fs.into_block().expect("commit");
    assert_eq!(
        le32(&device.data, at) & INCOMPAT_RECOVER,
        0,
        "a quiet volume is clean"
    );
}

/// A replay that rewrites the superblock with another geometry leaves nothing
/// read before it standing: the volume is read afresh from what the replay
/// left, never served through buffers sized for the geometry it replaced.
#[test]
fn a_replay_that_changes_the_geometry_reads_the_volume_afresh() {
    use crate::superblock::{SB_BLOCKS_LO, SB_FIRST_DATA_BLOCK, SB_LOG_BLOCK_SIZE};

    let (device, medium) = FailingWrites::over(sparse(JOURNALED));
    let mut fs = Ext4::open(device).expect("mount");
    let root = fs.root();
    let sb_at = usize::try_from(SUPERBLOCK_OFFSET).expect("small");
    let mut home = medium.borrow().data[sb_at..sb_at + SUPERBLOCK_LEN].to_vec();
    // The same eight mebibytes as half as many two-kilobyte blocks, one
    // group still: a superblock that parses, logged by the driver itself.
    let blocks = le32(&fs.sb, SB_BLOCKS_LO);
    put_le32(&mut fs.sb, SB_LOG_BLOCK_SIZE, 1);
    put_le32(&mut fs.sb, SB_BLOCKS_LO, blocks / 2);
    put_le32(&mut fs.sb, SB_FIRST_DATA_BLOCK, 0);
    superblock::stamp(&mut fs.sb);
    let table = inode_block(&mut fs, ROOT_INODE);
    medium.borrow_mut().failing = Some(sectors_of(&fs, table));
    assert_eq!(
        fs.create(root, b"torn", NodeKind::Directory, &DIR_RECORD),
        Err(DriverError::DeviceFault),
        "the home write failed with the log committed"
    );
    // A device free to reorder home writes may land none of the new
    // superblock: the old one stands, marked for the replay that brings the
    // new one home.
    let incompat = le32(&home, SB_FEATURE_INCOMPAT) | INCOMPAT_RECOVER;
    put_le32(&mut home, SB_FEATURE_INCOMPAT, incompat);
    superblock::stamp(&mut home);
    let mut image = medium.borrow().data.clone();
    image[sb_at..sb_at + SUPERBLOCK_LEN].copy_from_slice(&home);
    // A volume laid out for one block size and described in another cannot
    // be used; what matters is that it is read whole in the geometry the
    // replay left, so every later operation answers rather than overruns a
    // buffer sized for the old one.
    let mut fs = Ext4::open(SizedBlock::over(image)).expect("mounts what the replay left");
    assert_eq!(fs.layout.block_size, 2048);
    // The journal read before the replay was mapped in the old block size;
    // read afresh, this one cannot be found in the new, so nothing is
    // written through a log the replay emptied.
    assert_ne!(fs.write_refusal(), None);
    let root = fs.root();
    let _ = fs.node_info(root);
    let _ = fs.lookup(root, b"lost+found");
    let _ = fs.create(root, b"after", NodeKind::RegularFile, &FILE_RECORD);
    let _ = fs.into_block();
}

/// A directory whose size promises far more blocks than it maps is walked over
/// what it maps: the hole past its last block ends the walk, rather than each
/// block it spans costing a lookup of its own.
#[test]
fn a_directory_sized_past_its_blocks_is_walked_over_what_it_maps() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    fs.create(root, b"kept", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    // A `largedir` directory's size has a high half: the largest it can
    // carry, 2^54 blocks of promise over the one block it has.
    fs.layout.largedir = true;
    let mut raw = fs.inode_scratch().expect("scratch");
    fs.read_inode_raw(ROOT_INODE, &mut raw).expect("read");
    file::set_size(&mut raw, u64::MAX);
    fs.write_inode_raw(ROOT_INODE, &mut raw).expect("write");

    assert_eq!(fs.lookup(root, b"absent"), Err(DriverError::NotFound));
    assert!(fs.lookup(root, b"kept").is_ok());
    let names: Vec<Vec<u8>> = listed(&mut fs, root, 0, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert!(names.iter().any(|name| name == b"kept"));
}

/// Two journal blocks stored in one physical block are no log: a file whose
/// runs overlap could present the same blocks round and round as a log of any
/// length.
#[test]
fn a_journal_whose_runs_share_blocks_is_refused() {
    assert!(JournalMap::new(vec![(0, 100, 10), (10, 120, 10)]).is_ok());
    assert_eq!(
        JournalMap::new(vec![(0, 100, 10), (10, 105, 10)]).err(),
        Some(DriverError::DeviceFault)
    );
    assert_eq!(
        JournalMap::new(vec![(0, 100, 10), (10, 100, 10)]).err(),
        Some(DriverError::DeviceFault)
    );
}

/// A crash may leave the superblock's free totals behind the group
/// descriptors, which every allocation keeps exact: a writable mount counts
/// them again, so a stale total neither refuses room that is there nor is
/// written back.
#[test]
fn stale_superblock_totals_are_recounted_at_mount() {
    use crate::superblock::{SB_FREE_BLOCKS_LO, SB_FREE_INODES};

    let image = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID)
        .expect("format")
        .into_block()
        .expect("commit")
        .data;
    let exact = Ext4::open(SizedBlock::over(image.clone()))
        .expect("mount")
        .stats()
        .expect("stats");
    let mut stale = image;
    let at = usize::try_from(SUPERBLOCK_OFFSET).expect("small");
    let sb = &mut stale[at..at + SUPERBLOCK_LEN];
    put_le32(sb, SB_FREE_BLOCKS_LO, 0);
    put_le32(sb, SB_FREE_INODES, 0);
    if le32(sb, SB_FEATURE_RO_COMPAT) & RO_COMPAT_METADATA_CSUM != 0 {
        superblock::stamp(sb);
    }
    let mut fs = Ext4::open(SizedBlock::over(stale)).expect("mount");
    let recounted = fs.stats().expect("stats");
    assert_eq!(recounted.free_blocks, exact.free_blocks);
    assert_eq!(recounted.files_free, exact.files_free);
    let root = fs.root();
    fs.create(root, b"room", NodeKind::RegularFile, &FILE_RECORD)
        .expect("the room the groups hold");
    let stats = remount(fs).stats().expect("stats");
    assert_eq!(
        stats.files_free,
        exact.files_free - 1,
        "and written back exact"
    );
}

/// Set `flags` among inode `ino`'s, as `chattr` does.
fn chattr<B: Block>(fs: &mut Ext4<B>, ino: u32, flags: u32) {
    let mut raw = fs.inode_scratch().expect("scratch");
    fs.read_inode_raw(ino, &mut raw).expect("read");
    let set = le32(&raw, INODE_FLAGS) | flags;
    put_le32(&mut raw, INODE_FLAGS, set);
    fs.write_inode_raw(ino, &mut raw).expect("write");
}

/// What `chattr +i` and `chattr +a` hold fixed holds against every caller: an
/// immutable file is never written, truncated, re-moded, renamed or removed,
/// and an append-only one only grows at its end.
#[test]
fn an_immutable_or_append_only_file_is_held_as_the_volume_says() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    for (name, flag) in [
        (&b"fixed"[..], INODE_FLAG_IMMUTABLE),
        (b"log", INODE_FLAG_APPEND),
    ] {
        let node = fs
            .create(root, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        fs.write_at(node, 0, b"first").expect("write");
        chattr(&mut fs, node_inode(node).expect("inode"), flag);
        let refused = Err(DriverError::PermissionDenied);
        assert_eq!(fs.write_at(node, 0, b"over"), refused.map(|()| 0));
        assert_eq!(fs.truncate(node, 0), refused);
        assert_eq!(fs.remove(root, name), refused);
        assert_eq!(fs.rename(root, name, root, b"moved"), refused);
        let mut security = fs.security(node).expect("security");
        security.mode = 0o600;
        assert_eq!(fs.set_security(node, security), refused);
        let appended = fs.write_at(node, 5, b"+more");
        if flag == INODE_FLAG_APPEND {
            assert_eq!(appended, Ok(5), "an append-only file grows at its end");
        } else {
            assert_eq!(appended, refused.map(|()| 0));
        }
    }
    let other = fs
        .create(root, b"other", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(
        fs.rename(root, b"other", root, b"fixed"),
        Err(DriverError::PermissionDenied),
        "a rename over a protected file would remove it"
    );
    assert!(fs.node_info(other).is_ok());
}

/// An immutable directory takes no new name and loses none; an append-only
/// one takes new names but loses none.
#[test]
fn an_immutable_or_append_only_directory_is_held_as_the_volume_says() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    for (name, flag) in [
        (&b"sealed"[..], INODE_FLAG_IMMUTABLE),
        (b"inbox", INODE_FLAG_APPEND),
    ] {
        let dir = fs
            .create(root, name, NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        fs.create(dir, b"kept", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        chattr(&mut fs, node_inode(dir).expect("inode"), flag);
        assert_eq!(fs.remove(dir, b"kept"), Err(DriverError::PermissionDenied));
        assert_eq!(
            fs.rename(dir, b"kept", root, b"out"),
            Err(DriverError::PermissionDenied)
        );
        let added = fs
            .create(dir, b"new", NodeKind::RegularFile, &FILE_RECORD)
            .map(|_| ());
        if flag == INODE_FLAG_APPEND {
            assert_eq!(added, Ok(()), "an append-only directory gains names");
        } else {
            assert_eq!(added, Err(DriverError::PermissionDenied));
        }
    }
}

/// An entry naming an inode no entry may — the journal, here — is damage:
/// following it would let a delete free the live log.
#[test]
fn an_entry_naming_the_journal_is_refused_not_followed() {
    let mut fs = Ext4::open(SizedBlock::over(sparse(JOURNALED))).expect("mount");
    let root = fs.root();
    fs.create(root, b"victim", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let journal = fs.layout.journal_inum.expect("a journal");
    let dir = fs.read_inode(ROOT_INODE).expect("root");
    let seed = fs.dir_seed(ROOT_INODE, &dir);
    let phys = first_dir_block(&mut fs, root);
    let mut block = fs.block_scratch().expect("scratch");
    fs.read_dir_block(seed, phys, &mut block).expect("read");
    let end = fs.dir_data_end();
    let at = fs
        .locate(&block[..end], b"victim")
        .expect("parses")
        .expect("named")
        .at;
    put_le32(&mut block, at, journal);
    fs.operation(|fs| fs.write_leaf(seed, phys, &mut block))
        .expect("rewrite");

    assert_eq!(fs.lookup(root, b"victim"), Err(DriverError::DeviceFault));
    assert!(listed(&mut fs, root, 0, &[]).is_err(), "nor listed");
    assert_eq!(fs.remove(root, b"victim"), Err(DriverError::DeviceFault));
    let fs = remount(fs);
    assert!(fs.journal.is_some(), "the log is intact");
}

/// A volume under multi-mount protection may be in another host's hands, so
/// a log it holds is not replayed here: the replay is a write.
#[test]
fn a_log_on_a_multi_mount_protected_volume_is_not_replayed() {
    let (device, medium) = FailingWrites::over(sparse(JOURNALED));
    let mut fs = Ext4::open(device).expect("mount");
    let root = fs.root();
    let table = inode_block(&mut fs, ROOT_INODE);
    medium.borrow_mut().failing = Some(sectors_of(&fs, table));
    assert!(fs
        .create(root, b"torn", NodeKind::Directory, &DIR_RECORD)
        .is_err());
    let mut image = medium.borrow().data.clone();
    let at = usize::try_from(SUPERBLOCK_OFFSET).expect("small");
    let sb = &mut image[at..at + SUPERBLOCK_LEN];
    let incompat = le32(sb, SB_FEATURE_INCOMPAT) | superblock::INCOMPAT_MMP;
    put_le32(sb, SB_FEATURE_INCOMPAT, incompat);
    superblock::stamp(sb);
    let (device, medium) = FailingWrites::over(image.clone());
    assert_eq!(Ext4::open(device).err(), Some(DriverError::Unsupported));
    assert!(medium.borrow().data == image, "nothing was written");
}

/// A truncate a crash interrupted is finished from the orphan chain at mount,
/// the tail of its last block zeroed as a truncate zeroes it: the bytes cut
/// never reappear when the file grows again.
#[test]
fn an_interrupted_truncate_finished_at_mount_zeroes_its_tail() {
    use crate::superblock::SB_LAST_ORPHAN;

    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let node = fs
        .create(root, b"cut", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(node, 0, &[0xAB; 2048]).expect("write");
    let ino = node_inode(node).expect("inode");
    // What a crash mid-truncate leaves: the size cut to 100 bytes and the
    // inode listed, its blocks and the tail of the first not yet cut.
    fs.operation(|fs| {
        let mut raw = fs.inode_scratch()?;
        fs.read_inode_raw(ino, &mut raw)?;
        file::set_size(&mut raw, 100);
        put_le32(&mut raw, INODE_DTIME, 0);
        fs.write_inode_raw(ino, &mut raw)?;
        put_le32(&mut fs.sb, SB_LAST_ORPHAN, ino);
        Ok(())
    })
    .expect("listed");

    let mut fs = remount(fs);
    assert_eq!(le32(&fs.sb, SB_LAST_ORPHAN), 0, "finished");
    fs.write_at(node, 1000, b"!").expect("grow");
    let mut buf = [0u8; 1001];
    let n = fs.read_at(node, 0, &mut buf).expect("read");
    assert_eq!(n, 1001);
    assert!(
        buf[100..1000].iter().all(|&b| b == 0),
        "the cut bytes stay cut"
    );
}

/// `mke2fs -t ext4 -b 1024 -g 512 -J size=1 -O ^resize_inode` over 16 MiB
/// (e2fsprogs 1.47.0): thirty-two groups of 512 blocks and a 1024-block
/// internal journal, so one large file spans many groups.
const MULTIGROUP: &[u8] = include_bytes!("../tests/fixtures/multigroup.sparse");

/// A rename over a large file lets the replaced file go across transactions;
/// a crash between them must find the rename whole — the name naming the
/// moved file, the old name gone — never the name removed and its
/// replacement not yet linked.
#[test]
fn a_rename_over_a_file_let_go_across_transactions_is_whole_after_a_crash() {
    let mut image = sparse(MULTIGROUP);
    // The smallest log this driver writes through, whose transactions are a
    // few dozen blocks: letting go of a file across sixteen groups publishes
    // part-way.
    let log = {
        let fs = Ext4::open_read_only(SizedBlock::over(image.clone())).expect("mount");
        fs.journal_map
            .as_ref()
            .and_then(|map| map.resolve(0))
            .expect("a log")
    };
    let at = usize::try_from(log).expect("small") * 1024 + crate::journal::SB_MAXLEN;
    image[at..at + 4].copy_from_slice(&260u32.to_be_bytes());

    let (device, medium) = FailingWrites::over(image);
    let mut fs = Ext4::open(device).expect("mount");
    let root = fs.root();
    for name in [&b"old"[..], b"new"] {
        fs.create(root, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
    }
    let old = fs.lookup(root, b"old").expect("lookup");
    let new_file = fs.lookup(root, b"new").expect("lookup");
    let chunk = vec![0xA5u8; 64 * 1024];
    for at in 0..128u64 {
        fs.write_at(old, at * chunk.len() as u64, &chunk)
            .expect("write");
    }
    fs.write_at(new_file, 0, b"moved here").expect("write");
    let dir_block = first_dir_block(&mut fs, root);
    medium.borrow_mut().failing = Some(sectors_of(&fs, dir_block));
    assert_eq!(
        fs.rename(root, b"new", root, b"old"),
        Err(DriverError::DeviceFault),
        "the first publication holding the directory tears"
    );

    let mut fs = crashed(&medium);
    let root = fs.root();
    let moved = fs
        .lookup(root, b"old")
        .expect("the name names the moved file");
    let mut buf = [0u8; 16];
    let n = fs.read_at(moved, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"moved here");
    assert_eq!(fs.lookup(root, b"new"), Err(DriverError::NotFound));
}

/// A create that finds no room for its name — the directory needs a block
/// and none is free — gives its inode back.
#[test]
fn a_create_that_cannot_place_its_name_gives_its_inode_back() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let mut long = [b'f'; 200];
    for n in 0..4u8 {
        long[0] = b'a' + n;
        fs.create(root, &long, NodeKind::RegularFile, &FILE_RECORD)
            .expect("fits in block 0");
    }
    while fs.alloc_blocks(0, group::MAX_RUN, u64::MAX).is_ok() {}
    let before = fs.stats().expect("stats");
    long[0] = b'z';
    assert_eq!(
        fs.create(root, &long, NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::NoSpace)
    );
    assert_eq!(fs.stats().expect("stats"), before, "the inode went back");
    assert_eq!(fs.lookup(root, &long), Err(DriverError::NotFound));
}

/// A moved entry is listed as the kind its inode is: a symbolic link stays
/// one, not a regular file.
#[test]
fn a_renamed_symlink_keeps_its_entry_type() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let link = fs
        .create(root, b"link", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let ino = node_inode(link).expect("inode");
    let mut raw = [0u8; MAX_BLOCK_SIZE as usize];
    fs.read_inode_raw(ino, &mut raw).expect("raw");
    put_le16(&mut raw, 0, S_IFLNK | 0o777);
    put_le32(&mut raw, 0x04, 4);
    raw[I_BLOCK_OFFSET..I_BLOCK_OFFSET + 4].copy_from_slice(b"dest");
    fs.write_inode_raw(ino, &mut raw).expect("raw");
    let dir = fs
        .create(root, b"dir", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    fs.rename(root, b"link", dir, b"moved").expect("rename");
    let mut block = [0u8; MAX_BLOCK_SIZE as usize];
    let phys = first_dir_block(&mut fs, dir);
    fs.read_fs_block(phys, &mut block).expect("read");
    let bs = fs.layout.block_size as usize;
    let found = fs
        .locate(&block[..bs], b"moved")
        .expect("parse")
        .expect("listed");
    assert_eq!(block[found.at + 7], 7, "a symbolic link's entry type");
    let mut out = [0u8; 8];
    let moved = fs.lookup(dir, b"moved").expect("found");
    assert_eq!(fs.read_link(moved, &mut out), Ok(4));
}

/// Without `largedir` a directory's size is its low word: a stray high word
/// neither stretches its scans nor moves where its next block goes.
#[test]
fn a_directory_size_ignores_its_high_word_without_largedir() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let dir = fs
        .create(root, b"dir", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    let ino = node_inode(dir).expect("inode");
    let mut raw = [0u8; MAX_BLOCK_SIZE as usize];
    fs.read_inode_raw(ino, &mut raw).expect("raw");
    put_le32(&mut raw, 0x6C, 0x8000);
    fs.write_inode_raw(ino, &mut raw).expect("raw");
    assert_eq!(
        fs.read_inode(ino).expect("inode").size,
        u64::from(fs.layout.block_size)
    );
    let mut long = [b'f'; 200];
    for n in 0..8u8 {
        long[0] = b'a' + n;
        fs.create(dir, &long, NodeKind::RegularFile, &FILE_RECORD)
            .expect("a second block appended");
    }
    let inode = fs.read_inode(ino).expect("inode");
    assert_eq!(inode.size, 2 * u64::from(fs.layout.block_size));
    assert_eq!(listed(&mut fs, dir, 0, &[]).expect("list").len(), 8);
}

/// A group's bitmaps are one block each, so a superblock claiming more
/// blocks or inodes per group than a block has bits names bitmaps no read
/// stays inside: the volume is refused rather than indexed past them.
#[test]
fn a_volume_whose_groups_outgrow_their_bitmaps_is_refused() {
    for field in [0x20usize, 0x28] {
        let mut img = build_image();
        let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("offset fits");
        set_le32(&mut img, sb + field, u32c(8 * FS_BLOCK + 1));
        assert_eq!(
            Ext4::open(SizedBlock::over(img)).err(),
            Some(DriverError::BadMagic),
            "superblock field {field:#x}"
        );
    }
}

#[test]
fn a_seek_finds_the_holes_of_a_block_mapped_file() {
    let mut fs = mount();
    let file = fs.lookup(fs.root(), b"classic.bin").expect("found");
    let bs = FS_BLOCK as u64;
    // Data in blocks 0, 1 and 12; holes between.
    assert_eq!(fs.seek_region(file, 0, SeekRegion::Data), Ok(Some(0)));
    assert_eq!(fs.seek_region(file, 0, SeekRegion::Hole), Ok(Some(2 * bs)));
    assert_eq!(
        fs.seek_region(file, 2 * bs, SeekRegion::Data),
        Ok(Some(12 * bs))
    );
    assert_eq!(
        fs.seek_region(file, 3 * bs + 7, SeekRegion::Hole),
        Ok(Some(3 * bs + 7))
    );
    // The end of the file begins the last hole; past it nothing is left.
    assert_eq!(
        fs.seek_region(file, 12 * bs, SeekRegion::Hole),
        Ok(Some(CLASSIC_LEN as u64))
    );
    assert_eq!(
        fs.seek_region(file, CLASSIC_LEN as u64, SeekRegion::Data),
        Ok(None)
    );
    let root = fs.root();
    assert_eq!(
        fs.seek_region(root, 0, SeekRegion::Data),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn a_seek_finds_a_hole_left_by_writing_past_the_end() {
    let mut fs = mount();
    let root = fs.root();
    let gap = fs
        .create(root, b"gap", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let bs = FS_BLOCK as u64;
    fs.write_all(gap, 0, &[7u8; FS_BLOCK]).expect("write head");
    fs.write_all(gap, 4 * bs, &[9u8; FS_BLOCK])
        .expect("write past a gap");
    let file = fs.lookup(root, b"gap").expect("found");
    assert_eq!(fs.seek_region(file, 0, SeekRegion::Hole), Ok(Some(bs)));
    assert_eq!(fs.seek_region(file, bs, SeekRegion::Data), Ok(Some(4 * bs)));
    assert_eq!(
        fs.seek_region(file, 4 * bs, SeekRegion::Hole),
        Ok(Some(5 * bs))
    );
    // ext4 keeps holes but this driver does not release a range mid-file, and
    // says so rather than writing zeros in its place.
    assert_eq!(fs.punch(file, 0, bs), Err(DriverError::Unsupported));
}

// --- Holds (`pin`, `unpin`, `release_pins`). ---

/// The orphan chain, head first, as the open transaction has it.
fn orphan_chain<B: Block>(fs: &mut Ext4<B>) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut raw = fs.inode_scratch().expect("scratch");
    let mut at = le32(&fs.sb, crate::superblock::SB_LAST_ORPHAN);
    while at != 0 {
        assert!(chain.len() < 64, "the chain loops: {chain:?}");
        chain.push(at);
        fs.read_inode_raw(at, &mut raw).expect("an orphan's record");
        at = le32(&raw, INODE_DTIME);
    }
    chain
}

/// A fresh one-group volume.
fn formatted() -> Ext4<SizedBlock> {
    Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format")
}

/// Create `name` in `dir` holding `body`, hold it, and unlink it.
fn held_and_unlinked<B: Block>(fs: &mut Ext4<B>, dir: NodeId, name: &[u8], body: &[u8]) -> NodeId {
    let file = fs
        .create(dir, name, NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_all(file, 0, body).expect("write");
    let pinned = fs.pin(file).expect("pin");
    assert_eq!(pinned, file, "the pinned id is the inode number");
    fs.remove(dir, name).expect("unlink");
    pinned
}

#[test]
fn a_held_file_keeps_its_blocks_past_its_last_link_until_its_last_unpin() {
    let mut fs = formatted();
    let root = fs.root();
    let before = fs.stats().expect("stats");
    let file = fs
        .create(root, b"open", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_all(file, 0, &[0x5A; 8192]).expect("write");
    let written = fs.stats().expect("stats");
    let pinned = fs.pin(file).expect("pin");
    fs.remove(root, b"open").expect("unlink");

    assert_eq!(fs.lookup(root, b"open"), Err(DriverError::NotFound));
    assert_eq!(
        fs.stats().expect("stats"),
        written,
        "nothing came back with the link"
    );
    let ino = node_inode(pinned).expect("an inode");
    assert_eq!(orphan_chain(&mut fs), vec![ino], "the chain records it");
    assert_eq!(fs.node_info(pinned).expect("stat").nlink, 0);
    let mut buf = [0u8; 8192];
    assert_eq!(fs.read_at(pinned, 0, &mut buf), Ok(8192));
    assert!(buf.iter().all(|&b| b == 0x5A), "and its data");

    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
    assert!(orphan_chain(&mut fs).is_empty());
    assert_eq!(
        fs.stats().expect("stats"),
        before,
        "the last unpin gave back every block and the inode"
    );
    assert_eq!(fs.node_info(pinned), Err(DriverError::NotFound));
}

#[test]
fn an_unlinked_held_file_takes_writes_truncates_and_record_changes() {
    let mut fs = formatted();
    let root = fs.root();
    let pinned = held_and_unlinked(&mut fs, root, b"open", &[0x5A; 3000]);
    fs.write_all(pinned, 0, b"still writable")
        .expect("write an unlinked held file");
    let mut buf = [0u8; 14];
    assert_eq!(fs.read_at(pinned, 0, &mut buf), Ok(14));
    assert_eq!(&buf, b"still writable");
    fs.truncate(pinned, 5).expect("truncate");
    assert_eq!(fs.node_info(pinned).expect("stat").size, 5);
    let record = NodeSecurity::new(0o600, 9, 9);
    fs.set_security(pinned, record).expect("change its record");
    assert_eq!(fs.security(pinned), Ok(record));
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
    assert_eq!(fs.security(pinned), Err(DriverError::NotFound));
}

#[test]
fn holds_count_and_only_the_last_unpin_lets_an_inode_go() {
    let mut fs = formatted();
    let root = fs.root();
    let file = fs
        .create(root, b"twice", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_all(file, 0, b"kept by the other hold")
        .expect("write");
    let first = fs.pin(file).expect("pin");
    let second = fs.pin(file).expect("pin again");
    assert_eq!(first, second, "every hold of one node answers one id");
    fs.remove(root, b"twice").expect("unlink");
    assert_eq!(fs.unpin(first), Ok(Unpinned::Held));
    let mut buf = [0u8; 22];
    assert_eq!(fs.read_at(second, 0, &mut buf), Ok(22));
    assert_eq!(&buf, b"kept by the other hold");
    assert_eq!(fs.unpin(second), Ok(Unpinned::Freed));
    assert_eq!(
        fs.unpin(second),
        Err(DriverError::NotFound),
        "no hold is left"
    );
    assert_eq!(fs.unpin(root), Err(DriverError::NotFound), "never held");
    assert_eq!(fs.pin(NodeId::NONE), Err(DriverError::NotFound));
}

#[test]
fn a_held_inode_keeps_its_id_and_its_data_through_renames() {
    let mut fs = formatted();
    let root = fs.root();
    let outer = fs
        .create(root, b"outer", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    let file = fs
        .create(outer, b"f", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_all(file, 0, b"follows its node").expect("write");
    let pinned = fs.pin(file).expect("pin");
    fs.rename(outer, b"f", outer, b"g")
        .expect("rename the node");
    fs.rename(root, b"outer", root, b"moved")
        .expect("rename its parent");
    let moved = fs.lookup(root, b"moved").expect("lookup");
    assert_eq!(moved, outer);
    assert_eq!(
        fs.lookup(moved, b"g"),
        Ok(pinned),
        "a lookup answers the pinned id"
    );
    let listing = listed(&mut fs, moved, 0, &[]).expect("list");
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].0.node, pinned, "and so does a listing");
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(pinned, 0, &mut buf), Ok(16));
    assert_eq!(&buf, b"follows its node");
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released), "it keeps a link");
    assert!(orphan_chain(&mut fs).is_empty());
}

#[test]
fn a_rename_over_a_held_file_keeps_it_until_its_last_unpin() {
    let mut fs = formatted();
    let root = fs.root();
    let old = fs
        .create(root, b"old", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_all(old, 0, &[0xA5; 3000]).expect("write");
    let replacement = fs
        .create(root, b"new", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_all(replacement, 0, b"replacement").expect("write");
    let pinned = fs.pin(old).expect("pin");
    fs.rename(root, b"new", root, b"old")
        .expect("rename over the held file");
    assert_eq!(fs.lookup(root, b"old"), Ok(replacement));
    assert_eq!(fs.node_info(pinned).expect("stat").nlink, 0);
    assert_eq!(
        orphan_chain(&mut fs),
        vec![node_inode(pinned).expect("an inode")],
        "the replaced file is listed in the rename's own operation"
    );
    let mut buf = [0u8; 3000];
    assert_eq!(fs.read_at(pinned, 0, &mut buf), Ok(3000));
    assert!(buf.iter().all(|&b| b == 0xA5), "and kept whole");
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
    assert_eq!(fs.node_info(pinned), Err(DriverError::NotFound));
    assert!(orphan_chain(&mut fs).is_empty());
}

#[test]
fn a_removed_held_directory_takes_no_entries_and_its_last_unpin_frees_it() {
    let mut fs = formatted();
    let root = fs.root();
    let dir = fs
        .create(root, b"gone", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let pinned = fs.pin(dir).expect("pin");
    let root_links = fs.node_info(root).expect("stat").nlink;
    fs.remove(root, b"gone").expect("rmdir");
    assert_eq!(
        fs.node_info(root).expect("stat").nlink,
        root_links - 1,
        "its `..` left the parent's count"
    );
    assert_eq!(fs.node_info(pinned).expect("stat").nlink, 0);
    assert_eq!(fs.lookup(pinned, b"x"), Err(DriverError::NotFound));
    assert_eq!(fs.parent(pinned), Err(DriverError::NotFound));
    assert_eq!(listed(&mut fs, pinned, 0, &[]), Ok(Vec::new()));
    assert_eq!(
        fs.create(pinned, b"x", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.create(pinned, b"d", NodeKind::Directory, &DIR_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.rename(root, b"f", pinned, b"f"),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.lookup(root, b"f"),
        Ok(file),
        "the refused rename moved nothing"
    );

    let held = fs.stats().expect("stats");
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
    let freed = fs.stats().expect("stats");
    assert_eq!(freed.free_blocks, held.free_blocks + 1, "its one block");
    assert_eq!(freed.files_free, held.files_free + 1, "and its inode");
    assert!(orphan_chain(&mut fs).is_empty());
}

#[test]
fn release_pins_frees_every_unlinked_held_inode() {
    let mut fs = formatted();
    let root = fs.root();
    let kept = fs
        .create(root, b"kept", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let unlinked = [
        held_and_unlinked(&mut fs, root, b"a", b"a"),
        held_and_unlinked(&mut fs, root, b"b", b"b"),
    ];
    fs.pin(kept).expect("pin");
    fs.pin(kept).expect("pin again");
    assert_eq!(orphan_chain(&mut fs).len(), 2);

    assert_eq!(fs.release_pins(), Ok(()));
    assert!(orphan_chain(&mut fs).is_empty(), "each unlinked one went");
    for node in unlinked {
        assert_eq!(fs.node_info(node), Err(DriverError::NotFound));
    }
    assert_eq!(
        fs.unpin(kept),
        Err(DriverError::NotFound),
        "every hold went"
    );
    assert_eq!(fs.lookup(root, b"kept"), Ok(kept), "a linked inode stays");
}

#[test]
fn an_unlinked_inode_no_hold_keeps_is_not_found_to_every_call() {
    // A freed inode keeps its type, and a stale id once read it (D1443).
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("found");
    fs.remove(root, b"hello.txt").expect("remove");
    let mut buf = [0u8; 16];
    assert_eq!(fs.node_info(file), Err(DriverError::NotFound));
    assert_eq!(fs.read_at(file, 0, &mut buf), Err(DriverError::NotFound));
    assert_eq!(
        fs.seek_region(file, 0, SeekRegion::Data),
        Err(DriverError::NotFound)
    );
    assert_eq!(fs.security(file), Err(DriverError::NotFound));
    assert_eq!(
        fs.set_security(file, FILE_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(fs.write_at(file, 0, b"stale"), Err(DriverError::NotFound));
    assert_eq!(
        fs.pin(file),
        Err(DriverError::NotFound),
        "an inode no link reaches cannot be held"
    );
}

#[test]
fn an_orphan_leaves_the_chain_from_wherever_it_sits() {
    let mut fs = formatted();
    let root = fs.root();
    let held: Vec<NodeId> = [&b"first"[..], b"second", b"third", b"fourth"]
        .into_iter()
        .map(|name| held_and_unlinked(&mut fs, root, name, name))
        .collect();
    let inos: Vec<u32> = held
        .iter()
        .map(|&node| node_inode(node).expect("an inode"))
        .collect();
    assert_eq!(
        orphan_chain(&mut fs),
        vec![inos[3], inos[2], inos[1], inos[0]],
        "each joins at the head"
    );
    // The middle, the tail and the head each leave without disturbing the rest.
    for (gone, left) in [
        (1, vec![inos[3], inos[2], inos[0]]),
        (0, vec![inos[3], inos[2]]),
        (3, vec![inos[2]]),
        (2, Vec::new()),
    ] {
        assert_eq!(fs.unpin(held[gone]), Ok(Unpinned::Freed));
        assert_eq!(orphan_chain(&mut fs), left);
    }
    assert!(
        fs.orphan_prev.is_empty(),
        "the index holds nothing the chain does not"
    );
}

#[test]
fn an_unpin_whose_commit_is_lost_leaves_the_chain_and_its_index_whole() {
    let (device, medium) = FailingWrites::over(formatted().into_block().expect("commit").data);
    let mut fs = Ext4::open(device).expect("mount");
    let root = fs.root();
    let held = [
        held_and_unlinked(&mut fs, root, b"tail", b"t"),
        held_and_unlinked(&mut fs, root, b"middle", b"m"),
        held_and_unlinked(&mut fs, root, b"head", b"h"),
    ];
    let inos = held.map(|node| node_inode(node).expect("an inode"));
    assert_eq!(orphan_chain(&mut fs), vec![inos[2], inos[1], inos[0]]);

    medium.borrow_mut().failing = Some(superblock_sectors());
    assert_eq!(
        fs.unpin(held[1]),
        Err(DriverError::DeviceFault),
        "the release's commit is lost"
    );
    medium.borrow_mut().failing = None;
    assert_eq!(fs.write_refusal(), None, "nothing acknowledged was lost");
    assert_eq!(
        orphan_chain(&mut fs),
        vec![inos[2], inos[1], inos[0]],
        "the chain is as it was"
    );
    assert_eq!(
        fs.unpin(held[0]),
        Ok(Unpinned::Freed),
        "the index still names the tail's predecessor"
    );
    assert_eq!(fs.unpin(held[2]), Ok(Unpinned::Freed));
    assert_eq!(
        orphan_chain(&mut fs),
        vec![inos[1]],
        "the orphan whose release was lost waits for the mount"
    );
    let mut fs = crashed(&medium);
    assert!(orphan_chain(&mut fs).is_empty());
}

#[test]
fn a_held_file_a_crash_leaves_unlinked_is_released_at_mount() {
    let (device, medium) = FailingWrites::over(sparse(JOURNALED));
    let mut fs = Ext4::open(device).expect("mount");
    let root = fs.root();
    let before = fs.stats().expect("stats");
    let pinned = held_and_unlinked(&mut fs, root, b"open", &[0x5A; 6000]);
    assert_eq!(
        orphan_chain(&mut fs),
        vec![node_inode(pinned).expect("an inode")]
    );

    // No hold outlives the driver: the mount finds only the chain.
    let mut fs = crashed(&medium);
    assert!(orphan_chain(&mut fs).is_empty(), "the mount released it");
    assert_eq!(
        fs.stats().expect("stats"),
        before,
        "and its blocks and inode came back"
    );
    assert_eq!(fs.lookup(fs.root(), b"open"), Err(DriverError::NotFound));
}

#[test]
fn a_truncate_of_an_unlinked_held_file_spanning_transactions_lists_it_once() {
    // The rename test's small log: cutting a file across sixteen groups
    // publishes part-way, which lists a cut not listed already.
    let mut image = sparse(MULTIGROUP);
    let log = {
        let fs = Ext4::open_read_only(SizedBlock::over(image.clone())).expect("mount");
        fs.journal_map
            .as_ref()
            .and_then(|map| map.resolve(0))
            .expect("a log")
    };
    let at = usize::try_from(log).expect("small") * 1024 + crate::journal::SB_MAXLEN;
    image[at..at + 4].copy_from_slice(&260u32.to_be_bytes());
    let mut fs = Ext4::open(SizedBlock::over(image)).expect("mount");
    let root = fs.root();
    let file = fs
        .create(root, b"big", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let chunk = vec![0xA5u8; 64 * 1024];
    for at in 0..128u64 {
        fs.write_all(file, at * chunk.len() as u64, &chunk)
            .expect("write");
    }
    let big = fs.pin(file).expect("pin");
    fs.remove(root, b"big").expect("unlink");
    let small = held_and_unlinked(&mut fs, root, b"small", b"s");
    let chain = vec![
        node_inode(small).expect("an inode"),
        node_inode(big).expect("an inode"),
    ];
    assert_eq!(orphan_chain(&mut fs), chain);

    fs.truncate(big, 0).expect("truncate");
    assert_eq!(orphan_chain(&mut fs), chain, "listed once, where it was");
    assert_eq!(
        fs.unpin(big),
        Ok(Unpinned::Freed),
        "and still found through the orphan before it"
    );
    assert_eq!(fs.unpin(small), Ok(Unpinned::Freed));
    assert!(orphan_chain(&mut fs).is_empty());
}

#[test]
fn the_served_driver_opens_as_the_kernel_says_and_names_its_generation() {
    use tairix_fsserve::{ServedFormat, WritebackBudget};
    let budget = WritebackBudget {
        backing: 1 << 30,
        gauge: &tairix_reclaim::Unpressured,
    };
    let image = || {
        Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID)
            .expect("format")
            .into_block()
            .expect("the volume closes")
    };
    let mut writable = Served::open(image(), false, budget).expect("opens writable");
    assert_eq!(writable.format, writable.fs.generation().format());
    assert_eq!(writable.write_refusal, None);
    assert_eq!(writable.offer().root, writable.fs.root());
    let root = writable.fs.root();
    writable
        .fs
        .create(root, b"made", NodeKind::RegularFile, &FILE_RECORD)
        .expect("a writable volume takes a new file");

    let mut read_only = Served::open(image(), true, budget).expect("opens read-only");
    assert_eq!(
        read_only.write_refusal, None,
        "a volume the kernel serves read-only is no refusal of the format's"
    );
    let root = read_only.fs.root();
    assert!(read_only
        .fs
        .create(root, b"made", NodeKind::RegularFile, &FILE_RECORD)
        .is_err());
}
