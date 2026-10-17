//! The ext family's three generations against real `mke2fs 1.47` images:
//! the generation each volume reports, and that writing an ext2 or ext3
//! volume leaves it one, still mountable by the drivers it was made for.
//!
//! The offsets are the published on-disk layout's, written out here rather
//! than borrowed from the driver, so the driver is checked against the format
//! and not against itself.

#[path = "support/mem_block.rs"]
mod mem_block;
#[path = "support/sparse.rs"]
mod sparse;

use mem_block::MemBlock;
use sparse::sparse;
use tairix_abi::driver::filesystem::{
    FilesystemRead, FilesystemSecurity, FilesystemWrite, NodeId, NodeKind,
};
use tairix_drv_fs_ext4::{Ext4, Generation};

const EXT2: &[u8] = include_bytes!("fixtures/ext2.sparse");
const EXT3: &[u8] = include_bytes!("fixtures/ext3.sparse");
const EXT2_REV0: &[u8] = include_bytes!("fixtures/ext2_rev0.sparse");
const EXT4: &[u8] = include_bytes!("fixtures/metadata_csum.img");
const EXT4_JOURNALED: &[u8] = include_bytes!("fixtures/journaled.sparse");

const SB: usize = 1024;
const SB_INODES_PER_GROUP: usize = SB + 0x28;
const SB_STATE: usize = SB + 0x3A;
const SB_REV_LEVEL: usize = SB + 0x4C;
const SB_FIRST_INO: usize = SB + 0x54;
const SB_INODE_SIZE: usize = SB + 0x58;
const SB_COMPAT: usize = SB + 0x5C;
const SB_INCOMPAT: usize = SB + 0x60;
const SB_RO_COMPAT: usize = SB + 0x64;
const SB_JOURNAL_INUM: usize = SB + 0xE0;

const STATE_VALID: u16 = 0x1;
const RO_COMPAT_LARGE_FILE: u32 = 0x2;

/// The block size of every image written here; its descriptor table is
/// block 2.
const BLOCK: usize = 1024;
const GDT: usize = 2 * BLOCK;
const DESC_INODE_TABLE: usize = GDT + 0x08;
/// `bg_flags` through `bg_checksum`: fields only ext4's layouts define,
/// reserved and zero on ext2 and ext3.
const DESC_EXT4_FIELDS: core::ops::Range<usize> = GDT + 0x12..GDT + 0x20;

const INODE_FLAGS: usize = 0x20;
const INODE_BLOCK: usize = 0x28;
const INODE_FLAG_EXTENTS: u32 = 0x8_0000;
/// `i_block[12]`, the single-indirect pointer of a block-mapped file.
const INODE_INDIRECT: usize = INODE_BLOCK + 12 * 4;
/// `i_file_acl_high` on a `64bit` volume, padding on ext2 and ext3, and the
/// high mode bits on one GNU Hurd made.
const INODE_FILE_ACL_HIGH: usize = 0x76;

const JOURNAL_SEQUENCE: usize = 0x18;
const JOURNAL_START: usize = 0x1C;
/// `s_feature_compat` through `s_feature_ro_compat`.
const JOURNAL_FEATURES: core::ops::Range<usize> = 0x24..0x30;

const NAME: &[u8] = b"mapped.bin";
/// More blocks than an inode's twelve direct pointers map.
const FILE_BLOCKS: usize = 20;

fn le16(image: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([image[at], image[at + 1]])
}

fn le32(image: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(image[at..at + 4].try_into().expect("four bytes"))
}

fn be32(image: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(image[at..at + 4].try_into().expect("four bytes"))
}

fn index(value: u32) -> usize {
    usize::try_from(value).expect("an offset in a small image")
}

/// The feature words, compatible, incompatible and read-only-compatible.
fn features(image: &[u8]) -> [u32; 3] {
    [SB_COMPAT, SB_INCOMPAT, SB_RO_COMPAT].map(|at| le32(image, at))
}

/// Where inode `ino`'s record lies in `image`, a volume of one group.
fn inode_at(image: &[u8], ino: u32) -> core::ops::Range<usize> {
    assert!(ino <= le32(image, SB_INODES_PER_GROUP), "one group");
    let size = index(u32::from(le16(image, SB_INODE_SIZE)));
    let at = index(le32(image, DESC_INODE_TABLE)) * BLOCK + index(ino - 1) * size;
    at..at + size
}

fn inode(image: &[u8], ino: u32) -> &[u8] {
    &image[inode_at(image, ino)]
}

/// The journal superblock of `image`, whose journal inode is block-mapped.
fn journal_superblock(image: &[u8]) -> &[u8] {
    let journal = inode(image, le32(image, SB_JOURNAL_INUM));
    let at = index(le32(journal, INODE_BLOCK)) * BLOCK;
    &image[at..at + BLOCK]
}

fn open(image: Vec<u8>) -> Ext4<MemBlock> {
    Ext4::open(MemBlock { data: image }).expect("an e2fsprogs volume mounts")
}

fn body() -> Vec<u8> {
    (0..FILE_BLOCKS * BLOCK)
        .map(|at| u8::try_from(at % 251).expect("below 251"))
        .collect()
}

fn read_whole(fs: &mut Ext4<MemBlock>, file: NodeId, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut done = 0;
    while done < len {
        let offset = u64::try_from(done).expect("fits");
        let read = fs.read_at(file, offset, &mut out[done..]).expect("read");
        assert_ne!(read, 0, "the file holds every byte written");
        done += read;
    }
    out
}

/// Write a file past its inode's direct pointers onto `image`, read it back
/// from a fresh mount, and return the image as left and the file's inode.
fn write_mapped_file(image: Vec<u8>) -> (Vec<u8>, u32) {
    let mut fs = open(image);
    let root = fs.root();
    fs.create(root, NAME, NodeKind::RegularFile)
        .expect("create");
    fs.write_all(root, NAME, 0, &body()).expect("write");
    let mut fs = open(fs.into_block().expect("the volume closes").data);
    let root = fs.root();
    let file = fs.lookup(root, NAME).expect("the file is listed");
    assert_eq!(read_whole(&mut fs, file, body().len()), body());
    let ino = u32::try_from(file.raw()).expect("an inode number");
    (fs.into_block().expect("the volume closes").data, ino)
}

/// What an ext2 or ext3 driver needs of a volume this driver wrote: the
/// features it was made with, a block-mapped file, and descriptors with no
/// ext4 field filled in.
fn assert_left_as_made(pristine: &[u8], image: &[u8], ino: u32) {
    assert_eq!(features(image), features(pristine));
    assert_eq!(le16(image, SB_STATE) & STATE_VALID, STATE_VALID, "clean");
    let record = inode(image, ino);
    assert_eq!(le32(record, INODE_FLAGS) & INODE_FLAG_EXTENTS, 0);
    assert_ne!(
        le32(record, INODE_INDIRECT),
        0,
        "an indirect block maps the tail"
    );
    assert!(image[DESC_EXT4_FIELDS].iter().all(|&byte| byte == 0));
}

#[test]
fn each_volume_reports_the_generation_its_features_make_it() {
    for (image, generation) in [
        (sparse(EXT2), Generation::Ext2),
        (sparse(EXT2_REV0), Generation::Ext2),
        (sparse(EXT3), Generation::Ext3),
        (sparse(EXT4_JOURNALED), Generation::Ext4),
        (EXT4.to_vec(), Generation::Ext4),
    ] {
        assert_eq!(open(image).generation(), generation);
    }
}

#[test]
fn writing_an_ext2_volume_leaves_it_ext2() {
    let pristine = sparse(EXT2);
    let (image, ino) = write_mapped_file(pristine.clone());
    assert_left_as_made(&pristine, &image, ino);
    assert_eq!(open(image).generation(), Generation::Ext2);
}

#[test]
fn writing_an_ext3_volume_commits_through_its_journal_and_leaves_it_ext3() {
    let pristine = sparse(EXT3);
    let (image, ino) = write_mapped_file(pristine.clone());
    assert_left_as_made(&pristine, &image, ino);
    let (journal, made) = (journal_superblock(&image), journal_superblock(&pristine));
    assert_eq!(be32(journal, JOURNAL_START), 0, "nothing left to replay");
    assert!(
        be32(journal, JOURNAL_SEQUENCE) > be32(made, JOURNAL_SEQUENCE),
        "the write went through the log"
    );
    assert_eq!(
        journal[JOURNAL_FEATURES], made[JOURNAL_FEATURES],
        "the log keeps the format an ext3 driver replays"
    );
    assert_eq!(open(image).generation(), Generation::Ext3);
}

#[test]
fn a_file_past_two_gibibytes_raises_a_revision_0_volume_before_marking_it() {
    let mut pristine = sparse(EXT2_REV0);
    assert_eq!(le32(&pristine, SB_REV_LEVEL), 0);
    // Revision 0 defines nothing past `s_rev_level`: mke2fs fills these two
    // anyway, another maker need not, so the raise must write them.
    pristine[SB_FIRST_INO..SB_INODE_SIZE + 2].fill(0);
    let mut fs = open(pristine);
    let root = fs.root();
    fs.create(root, NAME, NodeKind::RegularFile)
        .expect("create");
    fs.truncate(root, NAME, 3 << 30).expect("a hole past 2 GiB");
    let image = fs.into_block().expect("the volume closes").data;
    assert_eq!(le32(&image, SB_REV_LEVEL), 1);
    assert_eq!(le32(&image, SB_FIRST_INO), 11);
    assert_eq!(le16(&image, SB_INODE_SIZE), 128);
    assert_eq!(features(&image), [0, 0, RO_COMPAT_LARGE_FILE]);
    let mut fs = open(image);
    assert_eq!(fs.generation(), Generation::Ext2);
    let root = fs.root();
    let file = fs.lookup(root, NAME).expect("the file is listed");
    assert_eq!(fs.node_info(file).expect("info").size, 3 << 30);
}

#[test]
fn an_attribute_block_number_has_no_high_half_without_64bit() {
    let mut fs = open(sparse(EXT2));
    let root = fs.root();
    let file = fs
        .create(root, NAME, NodeKind::RegularFile)
        .expect("create");
    let mut image = fs.into_block().expect("the volume closes").data;
    let ino = u32::try_from(file.raw()).expect("an inode number");
    let high = inode_at(&image, ino).start + INODE_FILE_ACL_HIGH;
    image[high..high + 2].copy_from_slice(&0xBEEF_u16.to_le_bytes());
    let mut fs = open(image);
    fs.security(file).expect("no attribute block to read");
    fs.remove(root, NAME).expect("none to release");
}
