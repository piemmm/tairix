//! FAT32 driver unit tests against an in-memory image.
//!
//! The test image is a hand-built, specification-shaped FAT32 volume
//! held in a fixed array, driven through a [`MockBlock`] device:
//!
//! ```text
//! /
//! ├── HELLO.TXT        (one cluster)
//! └── SUB/             (one cluster; carries `.`/`..`)
//!     └── DEEP.BIN      (two clusters — exercises chain following)
//! ```

extern crate std;

extern crate alloc;

use super::*;
use alloc::vec::Vec;
use tairix_abi::driver::block::BlockGeometry;
use tairix_abi::driver::filesystem::clock::TestClock;
use tairix_abi::driver::filesystem::listing::{first_listed, listed};
use tairix_abi::DriverKind;

// What a test's creations ask for, which FAT has nowhere to keep.
const FILE_RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);
const DIR_RECORD: NodeSecurity = NodeSecurity::new(0o755, 0, 0);
const LINK_RECORD: NodeSecurity = NodeSecurity::new(0o777, 0, 0);

const SECTOR_SIZE: usize = 512;
const SECTOR_COUNT: u32 = 16;
const IMG_LEN: usize = SECTOR_SIZE * (SECTOR_COUNT as usize);

const HELLO_BODY: &[u8] = b"Hello, TAIRiX FAT32!\n";
const DEEP_LEN: usize = 700;

const CLUSTER_ROOT: u32 = 2;
const CLUSTER_HELLO: u32 = 3;
const CLUSTER_SUB: u32 = 4;
const CLUSTER_DEEP_FIRST: u32 = 5;
const CLUSTER_DEEP_SECOND: u32 = 6;
const CLUSTER_LONG: u32 = 7;

/// Body of the long-named file `Greetings Café.txt`.
const LONG_BODY: &[u8] = b"long name body\n";

/// UTF-16 code units of `Greetings Café.txt` (the `é` is U+00E9), the
/// long name carried by the `GREETI~1.TXT` short entry in `SUB/`.
const LONG_NAME_UNITS: [u16; 18] = [
    0x0047, 0x0072, 0x0065, 0x0065, 0x0074, 0x0069, 0x006E, 0x0067, 0x0073, 0x0020, 0x0043, 0x0061,
    0x0066, 0x00E9, 0x002E, 0x0074, 0x0078, 0x0074,
];

/// The expected UTF-8 reconstruction of [`LONG_NAME_UNITS`].
const LONG_NAME_UTF8: &[u8] = b"Greetings Caf\xC3\xA9.txt";

/// The 8.3 short-name alias backing the long name.
fn long_short_name() -> [u8; 11] {
    short_name(b"GREETI~1", b"TXT")
}

fn set_le16(img: &mut [u8], offset: usize, value: u16) {
    img[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn set_le32(img: &mut [u8], offset: usize, value: u32) {
    img[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn set_fat(img: &mut [u8], cluster: usize, value: u32) {
    set_le32(img, SECTOR_SIZE + cluster * 4, value);
}

/// Build an 11-byte 8.3 short name from base and extension fields.
fn short_name(base: &[u8], ext: &[u8]) -> [u8; 11] {
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base);
    out[8..8 + ext.len()].copy_from_slice(ext);
    out
}

/// Byte offset of a cluster's data. In this image cluster `n` lives at
/// absolute sector `n` (reserved + FAT occupy sectors 0 and 1).
fn cluster_offset(cluster: u32) -> usize {
    (cluster as usize) * SECTOR_SIZE
}

fn write_entry(img: &mut [u8], offset: usize, name: &[u8; 11], attr: u8, cluster: u32, size: u32) {
    img[offset..offset + 11].copy_from_slice(name);
    img[offset + 11] = attr;
    let cluster_bytes = cluster.to_le_bytes();
    img[offset + 20..offset + 22].copy_from_slice(&cluster_bytes[2..4]);
    img[offset + 26..offset + 28].copy_from_slice(&cluster_bytes[0..2]);
    set_le32(img, offset + 28, size);
}

fn deep_byte(index: usize) -> u8 {
    u8::try_from(index % 256).expect("index % 256 fits in u8")
}

/// Write one 32-byte long-name directory entry.
fn write_lfn_entry(
    img: &mut [u8],
    offset: usize,
    order: u8,
    checksum: u8,
    slots: &[u16; LFN_UNITS_PER_ENTRY],
) {
    img[offset] = order;
    img[offset + 11] = ATTR_LONG_NAME;
    img[offset + 12] = 0;
    img[offset + 13] = checksum;
    for (k, &char_offset) in LFN_CHAR_OFFSETS.iter().enumerate() {
        set_le16(img, offset + char_offset, slots[k]);
    }
}

/// Write a complete long-name set (physical entries in descending
/// sequence, the first flagged [`LFN_LAST_FLAG`]) followed by its 8.3
/// short entry, returning the number of directory bytes consumed.
///
/// `checksum` is written into every fragment; passing a value other
/// than the short-name checksum models a corrupt set so the fall-back
/// to the 8.3 name can be tested.
#[allow(clippy::too_many_arguments)]
fn write_long_named_entry(
    img: &mut [u8],
    offset: usize,
    units: &[u16],
    short: &[u8; 11],
    checksum: u8,
    attr: u8,
    cluster: u32,
    size: u32,
) -> usize {
    let frag_count = units.len().div_ceil(LFN_UNITS_PER_ENTRY);
    for phys in 0..frag_count {
        let seq = frag_count - phys;
        let mut order = u8::try_from(seq).expect("sequence fits in u8");
        if phys == 0 {
            order |= LFN_LAST_FLAG;
        }
        let base = (seq - 1) * LFN_UNITS_PER_ENTRY;
        let mut slots = [0xFFFFu16; LFN_UNITS_PER_ENTRY];
        for (k, slot) in slots.iter_mut().enumerate() {
            let idx = base + k;
            *slot = match idx.cmp(&units.len()) {
                core::cmp::Ordering::Less => units[idx],
                core::cmp::Ordering::Equal => 0x0000,
                core::cmp::Ordering::Greater => 0xFFFF,
            };
        }
        write_lfn_entry(img, offset + phys * DIR_ENTRY_LEN, order, checksum, &slots);
    }
    write_entry(
        img,
        offset + frag_count * DIR_ENTRY_LEN,
        short,
        attr,
        cluster,
        size,
    );
    (frag_count + 1) * DIR_ENTRY_LEN
}

/// Byte offset of the long-named file's first directory entry in `SUB/`
/// (after `.`, `..`, and `DEEP.BIN`).
fn long_entry_offset() -> usize {
    cluster_offset(CLUSTER_SUB) + 3 * DIR_ENTRY_LEN
}

/// Construct the in-memory FAT32 image described in the module docs.
fn build_image() -> [u8; IMG_LEN] {
    let mut img = [0u8; IMG_LEN];

    // BIOS parameter block (boot sector).
    set_le16(&mut img, 11, 512); // bytes per sector
    img[13] = 1; // sectors per cluster
    set_le16(&mut img, 14, 1); // reserved sectors
    img[16] = 1; // number of FATs
    set_le16(&mut img, 17, 0); // root entry count (0 for FAT32)
    set_le16(&mut img, 22, 0); // 16-bit FAT size (0 for FAT32)
    set_le32(&mut img, 32, SECTOR_COUNT); // total sectors (32-bit)
    set_le32(&mut img, 36, 1); // 32-bit FAT size (sectors)
    set_le32(&mut img, 44, CLUSTER_ROOT); // root cluster
    img[510] = 0x55;
    img[511] = 0xAA;

    // File allocation table (single FAT in sector 1).
    let eoc = 0x0FFF_FFFF;
    set_fat(&mut img, 0, 0x0FFF_FFF8); // media descriptor
    set_fat(&mut img, 1, eoc);
    set_fat(&mut img, CLUSTER_ROOT as usize, eoc);
    set_fat(&mut img, CLUSTER_HELLO as usize, eoc);
    set_fat(&mut img, CLUSTER_SUB as usize, eoc);
    set_fat(&mut img, CLUSTER_DEEP_FIRST as usize, CLUSTER_DEEP_SECOND);
    set_fat(&mut img, CLUSTER_DEEP_SECOND as usize, eoc);

    // Root directory (cluster 2).
    let root = cluster_offset(CLUSTER_ROOT);
    let hello_size = u32::try_from(HELLO_BODY.len()).expect("body fits in u32");
    write_entry(
        &mut img,
        root,
        &short_name(b"HELLO", b"TXT"),
        0x20,
        CLUSTER_HELLO,
        hello_size,
    );
    write_entry(
        &mut img,
        root + DIR_ENTRY_LEN,
        &short_name(b"SUB", b""),
        ATTR_DIRECTORY,
        CLUSTER_SUB,
        0,
    );

    // HELLO.TXT contents (cluster 3).
    let hello = cluster_offset(CLUSTER_HELLO);
    img[hello..hello + HELLO_BODY.len()].copy_from_slice(HELLO_BODY);

    // SUB directory (cluster 4): `.`, `..`, then DEEP.BIN.
    let sub = cluster_offset(CLUSTER_SUB);
    write_entry(
        &mut img,
        sub,
        &short_name(b".", b""),
        ATTR_DIRECTORY,
        CLUSTER_SUB,
        0,
    );
    write_entry(
        &mut img,
        sub + DIR_ENTRY_LEN,
        &short_name(b"..", b""),
        ATTR_DIRECTORY,
        0,
        0,
    );
    let deep_size = u32::try_from(DEEP_LEN).expect("deep fits in u32");
    write_entry(
        &mut img,
        sub + 2 * DIR_ENTRY_LEN,
        &short_name(b"DEEP", b"BIN"),
        0x20,
        CLUSTER_DEEP_FIRST,
        deep_size,
    );

    // `Greetings Café.txt` (`GREETI~1.TXT`): a two-fragment long name.
    let long_short = long_short_name();
    let long_size = u32::try_from(LONG_BODY.len()).expect("body fits in u32");
    write_long_named_entry(
        &mut img,
        sub + 3 * DIR_ENTRY_LEN,
        &LONG_NAME_UNITS,
        &long_short,
        short_name_checksum(&long_short),
        0x20,
        CLUSTER_LONG,
        long_size,
    );
    set_fat(&mut img, CLUSTER_LONG as usize, eoc);
    let long_body = cluster_offset(CLUSTER_LONG);
    img[long_body..long_body + LONG_BODY.len()].copy_from_slice(LONG_BODY);

    // DEEP.BIN contents span clusters 5 and 6.
    let first = cluster_offset(CLUSTER_DEEP_FIRST);
    for i in 0..SECTOR_SIZE {
        img[first + i] = deep_byte(i);
    }
    let second = cluster_offset(CLUSTER_DEEP_SECOND);
    for i in SECTOR_SIZE..DEEP_LEN {
        img[second + (i - SECTOR_SIZE)] = deep_byte(i);
    }

    img
}

/// A fixed-size in-memory [`Block`] device over a FAT32 image.
struct MockBlock {
    data: [u8; IMG_LEN],
}

impl MockBlock {
    fn span(lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
        if len == 0 || !len.is_multiple_of(SECTOR_SIZE) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba)
            .map_err(|_| DriverError::LengthOutOfRange)?
            .saturating_mul(SECTOR_SIZE);
        let end = start
            .checked_add(len)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > IMG_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok((start, end))
    }
}

impl Block for MockBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: 512,
            block_count: u64::from(SECTOR_COUNT),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let (start, end) = Self::span(lba, buf.len())?;
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let (start, end) = Self::span(lba, buf.len())?;
        self.data[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
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

fn mount() -> Fat32<MockBlock> {
    Fat32::open(MockBlock {
        data: build_image(),
    })
    .expect("image is a valid FAT32 volume")
}

/// The first cluster of the directory `dir` names.
fn first_cluster(dir: NodeId) -> u32 {
    cluster_of(dir).expect("a directory")
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
fn open_reports_directory_root() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(fs.node_info(root).expect("info").kind, NodeKind::Directory);
}

#[test]
fn root_directory_lists_its_entries_in_order() {
    let mut fs = mount();
    let root = fs.root();
    let entries = listed(&mut fs, root, 0, &[]).expect("lists");
    let names: Vec<&[u8]> = entries.iter().map(|(_, name)| name.as_slice()).collect();
    assert_eq!(names, [&b"HELLO.TXT"[..], b"SUB"]);
    assert_eq!(entries[0].0.info.kind, NodeKind::RegularFile);
    assert_eq!(entries[1].0.info.kind, NodeKind::Directory);

    // Resuming from each entry's cursor walks the same listing.
    let mut chained = Vec::new();
    let mut cursor = 0;
    while let Some((entry, name)) = first_listed(&mut fs, root, cursor, &[]).expect("lists") {
        chained.push(name);
        cursor = entry.next_cursor;
    }
    assert_eq!(chained, names);
}

/// A cursor is a position in the directory it is applied to: one naming
/// another directory's cluster, as a stale or forged one would, never lists
/// that directory.
#[test]
fn a_cursor_never_reaches_outside_its_directory() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");
    let foreign = (u64::from(CLUSTER_ROOT) << 32) | 1;
    let names: Vec<Vec<u8>> = listed(&mut fs, sub, foreign, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert!(
        !names
            .iter()
            .any(|name| name == b"HELLO.TXT" || name == b"SUB"),
        "the root's entries were listed through the subdirectory: {names:?}"
    );
}

/// Fill the slots of `cluster` after its first `used` with deleted entries,
/// so a walk finds no end-of-directory mark inside it.
fn fill_with_deleted(img: &mut [u8], cluster: u32, used: usize) {
    let base = cluster_offset(cluster);
    for slot in used..SECTOR_SIZE / DIR_ENTRY_LEN {
        img[base + slot * DIR_ENTRY_LEN] = DELETED_ENTRY;
    }
}

/// A directory whose chain loops back on itself, or leads off the volume,
/// fails its listing closed instead of walking forever; so does the parent
/// that reports the directory's allocation.
#[test]
fn a_corrupt_directory_chain_fails_its_listing_closed() {
    let mut img = build_image();
    fill_with_deleted(&mut img, CLUSTER_SUB, 6);
    set_fat(&mut img, CLUSTER_SUB as usize, CLUSTER_SUB);
    let mut fs = Fat32::open(MockBlock { data: img }).expect("valid volume");
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");
    assert_eq!(listed(&mut fs, sub, 0, &[]), Err(DriverError::DeviceFault));
    assert_eq!(listed(&mut fs, root, 0, &[]), Err(DriverError::DeviceFault));

    let mut img = build_image();
    fill_with_deleted(&mut img, CLUSTER_SUB, 6);
    set_fat(&mut img, CLUSTER_SUB as usize, 16);
    let mut fs = Fat32::open(MockBlock { data: img }).expect("valid volume");
    let sub = fs.lookup(fs.root(), b"sub").expect("subdir");
    assert_eq!(listed(&mut fs, sub, 0, &[]), Err(DriverError::DeviceFault));
}

/// A file's allocation is the clusters its size needs, so listing never
/// walks a file's chain: a file whose chain loops is listed all the same.
#[test]
fn a_files_allocation_comes_from_its_size() {
    let mut img = build_image();
    set_fat(&mut img, CLUSTER_HELLO as usize, CLUSTER_HELLO);
    let mut fs = Fat32::open(MockBlock { data: img }).expect("valid volume");
    let root = fs.root();
    let entries = listed(&mut fs, root, 0, &[]).expect("lists");
    let (hello, _) = &entries[0];
    assert_eq!(hello.info.allocated, SECTOR_SIZE as u64);
}

/// A long name past the VFS name limit is listed and reached by its short
/// alias, so neither the file nor its directory becomes unreachable.
#[test]
fn a_long_name_past_the_name_limit_is_listed_by_its_alias() {
    let mut fs = mount();
    let root = fs.root();
    // Eighty-six three-byte characters: 258 bytes of UTF-8.
    let name: Vec<u8> = core::iter::repeat_n("\u{4E2D}", 86)
        .collect::<alloc::string::String>()
        .into_bytes();
    assert!(name.len() > FS_NAME_MAX);
    fs.create(root, &name, NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let entries = listed(&mut fs, root, 0, &[]).expect("lists");
    let (_, alias) = entries.last().expect("the new entry");
    assert!(alias.len() <= 12, "listed by its 8.3 alias: {alias:?}");
    assert!(fs.lookup(root, alias).is_ok(), "the alias resolves");
}

/// A long name that would hide, reorder or stack over what it says is listed
/// exactly as the volume holds it — the driver's job is fidelity — and the
/// display policy every surface draws names through escapes each such
/// character.
#[test]
fn a_hostile_long_name_is_listed_exactly_and_shown_escaped() {
    let mut fs = mount();
    let root = fs.root();
    let name = "invoice\u{202e}fdp.exe\u{200b}e\u{301}\u{301}";
    fs.create(root, name.as_bytes(), NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let entries = listed(&mut fs, root, 0, &[]).expect("lists");
    let (_, listed_name) = entries.last().expect("the new entry");
    assert_eq!(listed_name.as_slice(), name.as_bytes());
    let listed_name = core::str::from_utf8(listed_name).expect("UTF-8");
    assert_eq!(
        tairix_util::utf8::shown(listed_name),
        "invoice\u{fffd}fdp.exe\u{fffd}e\u{301}\u{fffd}"
    );
}

/// A [`MockBlock`] that records the first block of every read.
struct CountingBlock {
    inner: MockBlock,
    read_at: Vec<u64>,
}

impl Block for CountingBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.inner.geometry()
    }
    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.read_at.push(lba);
        self.inner.read_blocks(lba, buf)
    }
    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.inner.write_blocks(lba, buf)
    }
    fn flush(&mut self) -> Result<(), DriverError> {
        self.inner.flush()
    }
}

/// One listing reads each directory block once, not once per slot.
#[test]
fn a_listing_reads_each_directory_block_once() {
    let mut fs = Fat32::open(CountingBlock {
        inner: MockBlock {
            data: build_image(),
        },
        read_at: Vec::new(),
    })
    .expect("valid volume");
    let sub = fs.lookup(fs.root(), b"sub").expect("subdir");
    fs.dev.device_mut().read_at.clear();
    assert_eq!(listed(&mut fs, sub, 0, &[]).expect("lists").len(), 2);
    let sub_block = u64::from(CLUSTER_SUB);
    let reads = fs
        .dev
        .device()
        .read_at
        .iter()
        .filter(|at| **at == sub_block)
        .count();
    assert_eq!(reads, 1, "the directory's block is read once");
}

/// The 8.3 alias of every live entry of the directory at `cluster`, in slot
/// order.
fn aliases<B: Block>(fs: &mut Fat32<B>, cluster: u32) -> Vec<[u8; 11]> {
    let mut walk = DirCursor::at(cluster);
    let mut out = Vec::new();
    while let Some(entry) = fs.next_entry(&mut walk).expect("walks") {
        let mut raw = [0u8; DIR_ENTRY_LEN];
        fs.dev.read(entry.short_offset, &mut raw).expect("reads");
        out.push(raw[..11].try_into().expect("eleven bytes"));
    }
    out
}

/// A new entry's alias takes the lowest `~N` tail no live alias holds, and
/// placing it reads the directory once rather than once per tail tried.
#[test]
fn an_alias_takes_the_lowest_free_tail_in_one_pass() {
    let mut fs = Fat32::open(CountingBlock {
        inner: MockBlock {
            data: build_image(),
        },
        read_at: Vec::new(),
    })
    .expect("valid volume");
    let root = fs.root();
    for name in [b"Report 1.txt", b"Report 2.txt", b"Report 3.txt"] {
        fs.create(root, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
    }
    fs.remove(root, b"Report 2.txt").expect("remove");
    fs.dev.device_mut().read_at.clear();
    fs.create(root, b"Report 4.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let root_block = u64::from(CLUSTER_ROOT);
    let reads = fs
        .dev
        .device()
        .read_at
        .iter()
        .filter(|at| **at == root_block)
        .count();
    // The lookup, the placement, and one read for each of its two slots'
    // partial-block writes.
    assert_eq!(reads, 4, "the directory was read per slot or per tail");
    assert!(
        aliases(&mut fs, CLUSTER_ROOT).contains(&short_name(b"REPORT~2", b"TXT")),
        "the freed tail is taken again"
    );
}

/// Removing an entry whose slots run over a cluster boundary deletes every
/// one of them, leaving no long-name fragment behind.
#[test]
fn removing_an_entry_across_clusters_deletes_every_slot() {
    let mut fs = mount();
    let root = fs.root();
    // Three slots each, from slot 2: the fifth takes slots 14, 15 and 16, the
    // last in a cluster the create chains on.
    let names: [&[u8]; 5] = [
        b"Straddling one.txt",
        b"Straddling two.txt",
        b"Straddling six.txt",
        b"Straddling ten.txt",
        b"Straddling far.txt",
    ];
    for name in names {
        fs.create(root, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
    }
    fs.remove(root, names[4]).expect("remove");
    assert_eq!(fs.lookup(root, names[4]), Err(DriverError::NotFound));
    let listed_names: Vec<Vec<u8>> = listed(&mut fs, root, 0, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(listed_names.len(), 6, "{listed_names:?}");
    let mut walk = DirCursor::at(CLUSTER_ROOT);
    for _ in 0..17 {
        let (at, raw) = fs.next_slot(&mut walk).expect("walks").expect("a slot");
        if at.slot >= 14 {
            assert_eq!(raw[0], DELETED_ENTRY, "slot {} survived", at.slot);
        }
    }
}

/// A create whose end-of-entries mark would move behind a broken chain link
/// fails before writing, so a refused create leaves no entry behind.
#[test]
fn a_create_that_cannot_move_the_end_mark_writes_nothing() {
    let mut img = build_image();
    set_fat(&mut img, CLUSTER_ROOT as usize, 0x0FFF_FFF7);
    let mut fs = Fat32::open(MockBlock { data: img }).expect("valid volume");
    let root = fs.root();
    // Thirteen long-name fragments and a short entry fill slots 2 to 15, so
    // the mark goes to slot 16, past the root's broken link.
    let name = [b'a'; 160];
    assert_eq!(
        fs.create(root, &name, NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(fs.lookup(root, &name), Err(DriverError::NotFound));
}

/// A directory whose `..` links run in a circle, or name something that is
/// not a parent, never reaches the root, so moving a directory beneath it
/// fails closed instead of walking forever.
#[test]
fn a_broken_parent_chain_fails_a_directory_move_closed() {
    let dotdot = |dir: NodeId| cluster_offset(first_cluster(dir)) + DIR_ENTRY_LEN;
    let small = |dir: NodeId| u16::try_from(first_cluster(dir)).expect("a small cluster");

    let mut fs = mount();
    let root = fs.root();
    let a = fs
        .create(root, b"A", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    let b = fs
        .create(root, b"B", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    set_le16(&mut fs.dev.device_mut().data, dotdot(a) + 26, small(b));
    set_le16(&mut fs.dev.device_mut().data, dotdot(b) + 26, small(a));
    assert_eq!(
        fs.rename(root, b"SUB", a, b"SUB"),
        Err(DriverError::DeviceFault)
    );
    assert!(fs.lookup(root, b"SUB").is_ok(), "the refused move moved");

    let mut fs = mount();
    let root = fs.root();
    let a = fs
        .create(root, b"A", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    set_le16(&mut fs.dev.device_mut().data, dotdot(a) + 26, 0xFFFF);
    assert_eq!(
        fs.rename(root, b"SUB", a, b"SUB"),
        Err(DriverError::DeviceFault)
    );

    let mut fs = mount();
    let root = fs.root();
    let a = fs
        .create(root, b"A", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    let at = dotdot(a);
    fs.dev.device_mut().data[at..at + 11].copy_from_slice(&short_name(b"OTHER", b""));
    assert_eq!(
        fs.rename(root, b"SUB", a, b"SUB"),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn lookup_is_case_insensitive_and_reads_file_contents() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"hello.txt").expect("found");
    let info = fs.node_info(file).expect("info");
    assert_eq!(info.kind, NodeKind::RegularFile);
    assert_eq!(info.size, u64::try_from(HELLO_BODY.len()).unwrap());

    let mut buf = [0u8; 64];
    let read = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], HELLO_BODY);
}

#[test]
fn read_follows_the_cluster_chain_across_clusters() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");
    assert_eq!(fs.node_info(sub).expect("info").kind, NodeKind::Directory);

    let deep = fs.lookup(sub, b"deep.bin").expect("deep");
    assert_eq!(fs.node_info(deep).expect("info").size, DEEP_LEN as u64);

    let mut buf = [0u8; DEEP_LEN];
    let read = fs.read_at(deep, 0, &mut buf).expect("read");
    assert_eq!(read, DEEP_LEN);
    for (i, byte) in buf.iter().enumerate() {
        assert_eq!(*byte, deep_byte(i));
    }
}

#[test]
fn read_window_straddling_a_cluster_boundary() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");
    let deep = fs.lookup(sub, b"deep.bin").expect("deep");

    let mut window = [0u8; 20];
    let read = fs.read_at(deep, 502, &mut window).expect("read");
    assert_eq!(read, 20);
    for (k, byte) in window.iter().enumerate() {
        assert_eq!(*byte, deep_byte(502 + k));
    }
}

#[test]
fn read_at_or_past_eof_yields_zero() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"HELLO.TXT").expect("found");
    let size = u64::try_from(HELLO_BODY.len()).unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(file, size, &mut buf), Ok(0));
    assert_eq!(fs.read_at(file, size + 100, &mut buf), Ok(0));
}

#[test]
fn read_at_with_offset_returns_tail() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"HELLO.TXT").expect("found");
    let mut buf = [0u8; 5];
    let read = fs.read_at(file, 7, &mut buf).expect("read");
    assert_eq!(&buf[..read], &HELLO_BODY[7..7 + read]);
}

#[test]
fn lookup_missing_name_is_not_found() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(fs.lookup(root, b"NOPE.TXT"), Err(DriverError::NotFound));
}

/// The number `stat` reports is the node's id itself, for a file as for a
/// directory, by node as in a listing.
#[test]
fn every_nodes_number_is_its_id() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(fs.node_info(root).expect("info").ino, root.raw());
    for (entry, _) in listed(&mut fs, root, 0, &[]).expect("lists") {
        assert_eq!(entry.info.ino, entry.node.raw());
        assert_eq!(
            fs.node_info(entry.node).expect("info").ino,
            entry.node.raw()
        );
    }
}

/// A file's id is the place of its short entry, which holds its size,
/// first cluster and stamps.
#[test]
fn a_files_id_is_its_short_entrys_place() {
    let mut fs = mount();
    let sub = fs.lookup(fs.root(), b"SUB").expect("subdir");
    let short = long_entry_offset() + 2 * DIR_ENTRY_LEN;
    let id = file_node(short as u64);
    assert_eq!(fs.lookup(sub, LONG_NAME_UTF8), Ok(id));
    assert_eq!(fs.node_info(id).expect("info").size, LONG_BODY.len() as u64);
}

/// An id naming anything but a file's short entry in the data region names
/// nothing, so a forged id reaches no other structure of the volume.
#[test]
fn a_file_id_naming_anything_but_a_files_short_entry_names_nothing() {
    let mut fs = mount();
    let at = |byte: usize| file_node(byte as u64);
    let root = cluster_offset(CLUSTER_ROOT);
    let sub = cluster_offset(CLUSTER_SUB);
    for (what, forged) in [
        ("the boot sector", at(0)),
        ("a directory's entry", at(root + DIR_ENTRY_LEN)),
        ("a `.` entry", at(sub)),
        ("a long-name fragment", at(long_entry_offset())),
        ("the end of a directory", at(sub + 6 * DIR_ENTRY_LEN)),
        ("past the volume", at(IMG_LEN)),
        ("no slot at all", NodeId::from_raw(u64::MAX)),
    ] {
        assert_eq!(fs.node_info(forged), Err(DriverError::NotFound), "{what}");
        assert_eq!(
            fs.read_at(forged, 0, &mut [0u8; 4]),
            Err(DriverError::NotFound),
            "{what}"
        );
        assert_eq!(
            fs.write_at(forged, 0, b"x"),
            Err(DriverError::NotFound),
            "{what}"
        );
        assert_eq!(fs.truncate(forged, 0), Err(DriverError::NotFound), "{what}");
    }
}

/// Two empty files share every fact but the place of their entries, which
/// is what names each.
#[test]
fn two_empty_files_are_two_nodes() {
    let mut fs = mount();
    let root = fs.root();
    let a = fs
        .create(root, b"A.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let b = fs
        .create(root, b"B.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_ne!(a, b);
    assert_eq!(fs.lookup(root, b"a.txt"), Ok(a));
    assert_eq!(fs.lookup(root, b"b.txt"), Ok(b));
    fs.write_at(a, 0, b"only a").expect("write");
    assert_eq!(fs.node_info(b).expect("info").size, 0);
    assert_eq!(fs.read_at(b, 0, &mut [0u8; 8]), Ok(0));
}

/// A rename writes the file's entry afresh, so the file answers to the new
/// place's id and the old one names nothing.
#[test]
fn a_renamed_file_takes_a_new_id_and_its_old_one_names_nothing() {
    let mut fs = mount();
    let root = fs.root();
    let old = fs.lookup(root, b"HELLO.TXT").expect("found");
    fs.rename(root, b"HELLO.TXT", root, b"MOVED.TXT")
        .expect("rename");
    let new = fs.lookup(root, b"MOVED.TXT").expect("found");
    assert_ne!(new, old);
    assert_eq!(fs.node_info(old), Err(DriverError::NotFound));
    assert_eq!(fs.write_at(old, 0, b"x"), Err(DriverError::NotFound));
    let mut buf = [0u8; 64];
    let read = fs.read_at(new, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], HELLO_BODY);
}

/// A directory's parent is the one its `..` entry names, the root's is the
/// root, and a removed directory, whose cluster is freed, has none.
#[test]
fn a_directorys_parent_is_the_one_its_dot_dot_names() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(fs.parent(root), Ok(root));
    let sub = fs.lookup(root, b"SUB").expect("subdir");
    assert_eq!(fs.parent(sub), Ok(root));
    let inner = fs
        .create(sub, b"Inner", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    assert_eq!(fs.parent(inner), Ok(sub));
    let other = fs
        .create(root, b"Other", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    fs.rename(sub, b"Inner", other, b"Inner").expect("move");
    assert_eq!(
        fs.lookup(other, b"Inner"),
        Ok(inner),
        "a directory keeps its id"
    );
    assert_eq!(fs.parent(inner), Ok(other));
    let file = fs.lookup(root, b"HELLO.TXT").expect("found");
    assert_eq!(fs.parent(file), Err(DriverError::Unsupported));
    fs.remove(other, b"Inner").expect("rmdir");
    assert_eq!(fs.parent(inner), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(inner), Err(DriverError::NotFound));
    assert_eq!(fs.parent(NodeId::NONE), Err(DriverError::NotFound));
}

#[test]
fn directory_operations_on_a_file_are_unsupported() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"HELLO.TXT").expect("found");
    assert_eq!(fs.lookup(file, b"X"), Err(DriverError::Unsupported));
    assert_eq!(listed(&mut fs, file, 0, &[]), Err(DriverError::Unsupported));
}

#[test]
fn reading_a_directory_as_a_file_is_unsupported() {
    let mut fs = mount();
    let root = fs.root();
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(root, 0, &mut buf), Err(DriverError::Unsupported));
}

#[test]
fn open_rejects_a_bad_boot_signature() {
    let mut data = build_image();
    data[510] = 0;
    assert_eq!(
        Fat32::open(MockBlock { data }).err(),
        Some(DriverError::BadMagic)
    );
}

#[test]
fn open_rejects_a_non_fat32_volume() {
    let mut data = build_image();
    // A non-zero 16-bit root-entry count is the FAT12/FAT16 shape.
    data[17] = 16;
    assert_eq!(
        Fat32::open(MockBlock { data }).err(),
        Some(DriverError::BadMagic)
    );
}

#[test]
fn into_block_returns_the_backing_device() {
    let fs = mount();
    let block = fs.into_block();
    assert_eq!(block.geometry().expect("geo").block_count, 16);
}

#[test]
fn long_file_name_is_reconstructed_in_listing() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");
    // `.`/`..` are skipped, so the first entry is DEEP.BIN and the next is
    // the long-named file.
    let entries = listed(&mut fs, sub, 0, &[]).expect("lists");
    let names: Vec<&[u8]> = entries.iter().map(|(_, name)| name.as_slice()).collect();
    assert_eq!(names, [&b"DEEP.BIN"[..], LONG_NAME_UTF8]);
    assert_eq!(entries[1].0.info.kind, NodeKind::RegularFile);
}

#[test]
fn lookup_resolves_a_long_file_name_case_insensitively() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");

    let by_long = fs.lookup(sub, LONG_NAME_UTF8).expect("found by long name");
    // The ASCII portion folds case; the non-ASCII `é` byte is compared
    // verbatim.
    let by_mixed = fs
        .lookup(sub, b"greetings caf\xC3\xA9.TXT")
        .expect("found case-insensitively");
    assert_eq!(by_long, by_mixed);

    let mut buf = [0u8; 32];
    let read = fs.read_at(by_long, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], LONG_BODY);
}

#[test]
fn long_name_supersedes_its_short_alias() {
    // Each entry exposes a single name: when a valid long name is
    // present it is the entry's name, and the internal 8.3 alias is not
    // separately resolvable (the VFS namespace uses the long name).
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");

    assert!(fs.lookup(sub, LONG_NAME_UTF8).is_ok());
    assert_eq!(fs.lookup(sub, b"GREETI~1.TXT"), Err(DriverError::NotFound));
}

#[test]
fn corrupt_long_name_checksum_falls_back_to_short_name() {
    let mut data = build_image();
    // Overwrite the checksum byte (offset 13) of both long-name
    // fragments with a value that cannot match the short name.
    let first = long_entry_offset();
    data[first + 13] ^= 0xFF;
    data[first + DIR_ENTRY_LEN + 13] ^= 0xFF;

    let mut fs = Fat32::open(MockBlock { data }).expect("valid volume");
    let root = fs.root();
    let sub = fs.lookup(root, b"sub").expect("subdir");

    let entries = listed(&mut fs, sub, 0, &[]).expect("lists");
    assert_eq!(entries[1].1, b"GREETI~1.TXT");

    assert_eq!(fs.lookup(sub, LONG_NAME_UTF8), Err(DriverError::NotFound));
}

#[test]
fn short_name_checksum_matches_the_specification() {
    // The reference checksum of the 11-byte field "GREETI~1TXT".
    let short = short_name(b"GREETI~1", b"TXT");
    let mut expected = 0u8;
    for &byte in &short {
        expected = expected.rotate_right(1).wrapping_add(byte);
    }
    assert_eq!(short_name_checksum(&short), expected);
}

#[test]
fn decode_utf16le_handles_a_surrogate_pair() {
    // U+1F600 GRINNING FACE encodes as the surrogate pair D83D DE00.
    let units = [0xD83Du16, 0xDE00];
    let mut out = [0u8; 8];
    let len = decode_utf16le(&units, &mut out).expect("decoded");
    assert_eq!(&out[..len], b"\xF0\x9F\x98\x80");
}

#[test]
fn decode_utf16le_stops_at_the_terminator() {
    let units = [0x0041u16, 0x0042, 0x0000, 0xFFFF, 0x0043];
    let mut out = [0u8; 8];
    let len = decode_utf16le(&units, &mut out).expect("decoded");
    assert_eq!(&out[..len], b"AB");
}

#[test]
fn decode_utf16le_rejects_unpaired_surrogates() {
    let mut out = [0u8; 8];
    assert_eq!(decode_utf16le(&[0xD83Du16], &mut out), None);
    assert_eq!(decode_utf16le(&[0xD83Du16, 0x0041], &mut out), None);
    assert_eq!(decode_utf16le(&[0xDC00u16], &mut out), None);
}

#[test]
fn decode_utf16le_rejects_an_overflowing_output() {
    let units = [0x0041u16, 0x0042, 0x0043];
    let mut out = [0u8; 2];
    assert_eq!(decode_utf16le(&units, &mut out), None);
}

// ---------------------------------------------------------------------
// Write-path tests (`FilesystemWrite`).
//
// They mutate the in-memory image through the same `MockBlock` and then
// read the result back through the `FilesystemRead` surface, so a
// round-trip exercises both halves of the driver.
// ---------------------------------------------------------------------

#[test]
fn create_then_write_and_read_back_a_short_named_file() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs
        .create(root, b"NOTES.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");

    let payload = b"hello world";
    let written = fs.write_at(file, 0, payload).expect("write");
    assert_eq!(written, payload.len());

    let node = fs.lookup(root, b"notes.txt").expect("lookup");
    assert_eq!(node, file);
    assert_eq!(fs.node_info(node).expect("info").size, payload.len() as u64);
    let mut buf = [0u8; 32];
    let read = fs.read_at(node, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], payload);
}

#[test]
fn create_a_long_named_file_and_resolve_it() {
    let mut fs = mount();
    let root = fs.root();
    let name = b"My Long Document.txt";
    let file = fs
        .create(root, name, NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let body = b"long content";
    fs.write_at(file, 0, body).expect("write");

    // Resolvable by the exact long name and case-insensitively.
    let exact = fs.lookup(root, name).expect("exact");
    let folded = fs.lookup(root, b"MY LONG DOCUMENT.TXT").expect("folded");
    assert_eq!(exact, folded);

    let mut buf = [0u8; 32];
    let read = fs.read_at(exact, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], body);
}

#[test]
fn write_extends_across_a_cluster_boundary() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs
        .create(root, b"BIG.BIN", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");

    let mut payload = [0u8; 600];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = deep_byte(i);
    }
    assert_eq!(fs.write_at(file, 0, &payload), Ok(600));

    let node = fs.lookup(root, b"BIG.BIN").expect("lookup");
    assert_eq!(fs.node_info(node).expect("info").size, 600);
    let mut buf = [0u8; 600];
    assert_eq!(fs.read_at(node, 0, &mut buf), Ok(600));
    assert_eq!(buf, payload);
}

#[test]
fn write_past_end_zero_fills_the_gap() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs
        .create(root, b"SPARSE.BIN", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(fs.write_at(file, 5, b"XYZ"), Ok(3));

    let node = fs.lookup(root, b"SPARSE.BIN").expect("lookup");
    assert_eq!(fs.node_info(node).expect("info").size, 8);
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(node, 0, &mut buf), Ok(8));
    assert_eq!(&buf, b"\0\0\0\0\0XYZ");
}

#[test]
fn truncate_shrinks_and_grows_a_file() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs
        .create(root, b"T.BIN", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let mut payload = [0u8; 600];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = deep_byte(i);
    }
    fs.write_at(file, 0, &payload).expect("write");

    // Shrink to 100 bytes.
    fs.truncate(file, 100).expect("shrink");
    assert_eq!(fs.node_info(file).expect("info").size, 100);
    let mut head = [0u8; 100];
    assert_eq!(fs.read_at(file, 0, &mut head), Ok(100));
    assert_eq!(&head[..], &payload[..100]);

    // Grow to 300 bytes; the new tail reads back as zero.
    fs.truncate(file, 300).expect("grow");
    assert_eq!(fs.node_info(file).expect("info").size, 300);
    let mut all = [0xAAu8; 300];
    assert_eq!(fs.read_at(file, 0, &mut all), Ok(300));
    assert_eq!(&all[..100], &payload[..100]);
    assert!(all[100..].iter().all(|&b| b == 0));
}

#[test]
fn remove_unlinks_a_file_and_frees_the_name() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs
        .create(root, b"TMP.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(file, 0, b"bye").expect("write");
    fs.remove(root, b"TMP.TXT").expect("remove");
    assert_eq!(fs.lookup(root, b"TMP.TXT"), Err(DriverError::NotFound));

    // The name (and its slots) can be reused immediately.
    fs.create(root, b"TMP.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("recreate");
    assert!(fs.lookup(root, b"TMP.TXT").is_ok());
}

#[test]
fn mkdir_then_create_a_file_inside() {
    let mut fs = mount();
    let root = fs.root();
    let dir = fs
        .create(root, b"Docs", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    assert_eq!(fs.node_info(dir).expect("info").kind, NodeKind::Directory);

    // The directory is visible from the root.
    let resolved = fs.lookup(root, b"docs").expect("lookup dir");
    assert_eq!(resolved, dir);

    let inner = fs
        .create(dir, b"inner.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create inside");
    fs.write_at(inner, 0, b"nested").expect("write");

    let file = fs.lookup(dir, b"inner.txt").expect("lookup inside");
    assert_eq!(file, inner);
    let mut buf = [0u8; 16];
    let read = fs.read_at(file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], b"nested");
}

#[test]
fn creating_an_existing_name_is_refused_as_already_existing() {
    let mut fs = mount();
    let root = fs.root();
    fs.create(root, b"DUP.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(
        fs.create(root, b"DUP.TXT", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::AlreadyExists)
    );
}

#[test]
fn writing_to_a_directory_is_unsupported() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"SUB").expect("subdir");
    for dir in [root, sub] {
        assert_eq!(fs.write_at(dir, 0, b"x"), Err(DriverError::Unsupported));
        assert_eq!(fs.truncate(dir, 0), Err(DriverError::Unsupported));
    }
}

#[test]
fn removing_a_non_empty_directory_is_refused_as_not_empty() {
    let mut fs = mount();
    let root = fs.root();
    assert_eq!(fs.remove(root, b"SUB"), Err(DriverError::DirectoryNotEmpty));
}

#[test]
fn removing_an_empty_directory_succeeds() {
    let mut fs = mount();
    let root = fs.root();
    fs.create(root, b"Empty", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    fs.remove(root, b"Empty").expect("rmdir");
    assert_eq!(fs.lookup(root, b"Empty"), Err(DriverError::NotFound));
}

/// A removed file's id names nothing: every operation by it is refused
/// rather than reaching what the freed slot holds next.
#[test]
fn a_removed_files_id_names_nothing() {
    let mut fs = mount();
    let root = fs.root();
    let ghost = fs
        .create(root, b"GHOST.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(ghost, 0, b"boo").expect("write");
    fs.remove(root, b"GHOST.TXT").expect("remove");
    assert_eq!(fs.node_info(ghost), Err(DriverError::NotFound));
    assert_eq!(
        fs.read_at(ghost, 0, &mut [0u8; 4]),
        Err(DriverError::NotFound)
    );
    assert_eq!(fs.write_at(ghost, 0, b"x"), Err(DriverError::NotFound));
    assert_eq!(fs.truncate(ghost, 0), Err(DriverError::NotFound));
    assert_eq!(fs.security(ghost).map(|_| ()), Err(DriverError::NotFound));
    assert_eq!(fs.remove(root, b"GHOST.TXT"), Err(DriverError::NotFound));
}

/// A removed directory's id takes no entries once its cluster is free: a
/// creation or a move into it is refused rather than writing into free space.
#[test]
fn a_removed_directorys_id_takes_no_entries() {
    let mut fs = mount();
    let root = fs.root();
    let gone = fs
        .create(root, b"GONE", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    fs.remove(root, b"GONE").expect("rmdir");
    let free = free_clusters(&mut fs);
    assert_eq!(
        fs.create(gone, b"X.TXT", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.rename(root, b"HELLO.TXT", gone, b"HELLO.TXT"),
        Err(DriverError::NotFound)
    );
    assert_eq!(free_clusters(&mut fs), free);
    assert!(
        fs.lookup(root, b"HELLO.TXT").is_ok(),
        "the refused move moved"
    );
}

/// The volume's free clusters.
fn free_clusters<B: Block>(fs: &mut Fat32<B>) -> u64 {
    fs.stats().expect("stats").free_blocks
}

/// A pinned file is reported under its token, which lookups and listings
/// answer in place of its own id, while `stat` goes on reporting its own
/// number: the pin is why the id a lookup answers is not stable.
#[test]
fn a_pinned_file_is_reported_under_its_token() {
    let mut fs = mount();
    assert_eq!(fs.node_identity(), NodeIdentity::ContentDerived);
    let root = fs.root();
    let own = fs.lookup(root, b"HELLO.TXT").expect("found");
    let pinned = fs.pin(own).expect("pin");
    assert_ne!(pinned, own);
    assert_eq!(fs.lookup(root, b"hello.txt"), Ok(pinned));
    let entries = listed(&mut fs, root, 0, &[]).expect("lists");
    assert_eq!(entries[0].0.node, pinned);
    assert_eq!(entries[0].0.info.ino, own.raw());
    assert_eq!(fs.node_info(pinned).expect("info").ino, own.raw());
    let mut buf = [0u8; 64];
    let read = fs.read_at(pinned, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], HELLO_BODY);
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released));
    assert_eq!(fs.lookup(root, b"HELLO.TXT"), Ok(own));
}

/// Holds count: every hold of one node answers one id, whichever id it was
/// taken by, and each is released by one unpin. A directory is held under its
/// own id.
#[test]
fn holds_count_and_every_hold_answers_one_id() {
    let mut fs = mount();
    let root = fs.root();
    let own = fs.lookup(root, b"HELLO.TXT").expect("found");
    let pinned = fs.pin(own).expect("pin");
    assert_eq!(fs.pin(own), Ok(pinned));
    assert_eq!(fs.pin(pinned), Ok(pinned));
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Held));
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Held));
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released));
    assert_eq!(fs.unpin(pinned), Err(DriverError::NotFound));

    let sub = fs.lookup(root, b"SUB").expect("subdir");
    assert_eq!(fs.pin(sub), Ok(sub));
    assert_eq!(fs.pin(sub), Ok(sub));
    assert_eq!(fs.unpin(sub), Ok(Unpinned::Held));
    assert_eq!(fs.unpin(sub), Ok(Unpinned::Released));
}

/// A hold nothing took cannot be released, and nothing but a live node can
/// be held.
#[test]
fn an_unpin_with_no_hold_is_not_found() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"HELLO.TXT").expect("found");
    for never in [file, root, token_node(99), NodeId::NONE] {
        assert_eq!(fs.unpin(never), Err(DriverError::NotFound), "{never:?}");
    }
    let pinned = fs.pin(file).expect("pin");
    assert_eq!(
        fs.unpin(file),
        Err(DriverError::NotFound),
        "a hold is released by the id it answered"
    );
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released));
    for dead in [NodeId::NONE, file_node(0), token_node(99)] {
        assert_eq!(fs.pin(dead), Err(DriverError::NotFound), "{dead:?}");
    }
}

/// A removal deletes a pinned file's entry but keeps its chain: the file
/// reads, writes and truncates through its token, stats with no name and the
/// number its entry had, and holds its space until the last unpin frees it.
#[test]
fn a_pinned_file_outlives_its_name_until_its_last_unpin() {
    let mut fs = mount();
    let root = fs.root();
    let empty = free_clusters(&mut fs);
    let file = fs
        .create(root, b"KEPT.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(file, 0, &[7u8; 700]).expect("write");
    let pinned = fs.pin(file).expect("pin");
    let held = free_clusters(&mut fs);
    fs.remove(root, b"KEPT.TXT").expect("remove");
    assert_eq!(fs.lookup(root, b"KEPT.TXT"), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(file), Err(DriverError::NotFound));
    assert_eq!(free_clusters(&mut fs), held, "the chain is kept");

    let info = fs.node_info(pinned).expect("info");
    assert_eq!((info.nlink, info.size, info.ino), (0, 700, file.raw()));
    let mut body = [0u8; 700];
    assert_eq!(fs.read_at(pinned, 0, &mut body), Ok(700));
    assert!(body.iter().all(|&b| b == 7));
    fs.write_at(pinned, 700, &[9u8; 600]).expect("write");
    assert_eq!(fs.node_info(pinned).expect("info").size, 1300);
    let mut tail = [0u8; 600];
    assert_eq!(fs.read_at(pinned, 700, &mut tail), Ok(600));
    assert!(tail.iter().all(|&b| b == 9));
    assert!(free_clusters(&mut fs) < held);
    fs.truncate(pinned, 100).expect("truncate");
    assert_eq!(fs.node_info(pinned).expect("info").size, 100);
    assert_eq!(fs.security(pinned).map(|record| record.mode), Ok(0o644));

    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
    assert_eq!(
        free_clusters(&mut fs),
        empty,
        "the last unpin frees the chain"
    );
}

/// Once its last unpin has freed it, a nameless file is gone to every call.
#[test]
fn an_unpinned_nameless_file_is_not_found() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"HELLO.TXT").expect("found");
    let pinned = fs.pin(file).expect("pin");
    fs.remove(root, b"HELLO.TXT").expect("remove");
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
    assert_eq!(fs.node_info(pinned), Err(DriverError::NotFound));
    assert_eq!(
        fs.read_at(pinned, 0, &mut [0u8; 4]),
        Err(DriverError::NotFound)
    );
    assert_eq!(fs.security(pinned).map(|_| ()), Err(DriverError::NotFound));
    assert_eq!(fs.write_at(pinned, 0, b"x"), Err(DriverError::NotFound));
    assert_eq!(fs.truncate(pinned, 0), Err(DriverError::NotFound));
    assert_eq!(fs.pin(pinned), Err(DriverError::NotFound));
}

/// A pinned file keeps its token through renames of it and of its directory
/// and a move to another: its pin follows its entry to each new place, while
/// its own id moves. A pinned directory keeps its own id, its first cluster.
#[test]
fn a_pinned_node_keeps_its_id_through_renames() {
    let mut fs = mount();
    let root = fs.root();
    let sub = fs.lookup(root, b"SUB").expect("subdir");
    let deep = fs.lookup(sub, b"DEEP.BIN").expect("deep");
    let pinned = fs.pin(deep).expect("pin");
    assert_eq!(fs.pin(sub), Ok(sub));
    fs.rename(sub, b"DEEP.BIN", sub, b"Renamed deep.bin")
        .expect("rename");
    fs.rename(root, b"SUB", root, b"Moved sub")
        .expect("rename the directory");
    assert_eq!(fs.lookup(root, b"Moved sub"), Ok(sub));
    assert_eq!(fs.lookup(sub, b"Renamed deep.bin"), Ok(pinned));
    let other = fs
        .create(root, b"OTHER", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    fs.rename(sub, b"Renamed deep.bin", other, b"DEEP.BIN")
        .expect("move");
    assert_eq!(fs.lookup(other, b"DEEP.BIN"), Ok(pinned));
    assert_eq!(
        fs.lookup(sub, b"Renamed deep.bin"),
        Err(DriverError::NotFound)
    );
    let mut body = [0u8; DEEP_LEN];
    assert_eq!(fs.read_at(pinned, 0, &mut body), Ok(DEEP_LEN));
    for (i, byte) in body.iter().enumerate() {
        assert_eq!(*byte, deep_byte(i));
    }
    fs.write_at(pinned, 0, b"rewritten").expect("write");
    let mut head = [0u8; 9];
    assert_eq!(fs.read_at(pinned, 0, &mut head), Ok(9));
    assert_eq!(&head, b"rewritten");
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released));
    assert_eq!(fs.unpin(sub), Ok(Unpinned::Released));
}

/// A rename replacing a pinned file deletes its entry as a removal does: the
/// pin keeps the replaced file's data until its last unpin frees it.
#[test]
fn a_rename_over_a_pinned_file_keeps_it_for_the_pin() {
    let mut fs = mount();
    let root = fs.root();
    let victim = fs
        .create(root, b"B.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(victim, 0, b"victim").expect("write");
    let mover = fs
        .create(root, b"A.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(mover, 0, b"mover").expect("write");
    let pinned = fs.pin(victim).expect("pin");
    let free = free_clusters(&mut fs);
    fs.rename(root, b"A.TXT", root, b"B.TXT").expect("replace");
    assert_eq!(free_clusters(&mut fs), free, "the replaced chain is kept");
    let mut buf = [0u8; 16];
    let read = fs.read_at(pinned, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], b"victim");
    assert_eq!(fs.node_info(pinned).expect("info").nlink, 0);
    let named = fs.lookup(root, b"B.TXT").expect("found");
    assert_ne!(named, pinned);
    let read = fs.read_at(named, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], b"mover");
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
    assert_eq!(free_clusters(&mut fs), free + 1);
}

/// A file made in a removed pinned file's place is another file: the pin
/// goes on reaching the removed one.
#[test]
fn a_file_made_in_a_removed_pinned_files_place_is_another() {
    let mut fs = mount();
    let root = fs.root();
    let old = fs.lookup(root, b"HELLO.TXT").expect("found");
    let pinned = fs.pin(old).expect("pin");
    fs.remove(root, b"HELLO.TXT").expect("remove");
    let new = fs
        .create(root, b"HELLO.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("recreate");
    fs.write_at(new, 0, b"new body").expect("write");
    assert_ne!(new, pinned);
    assert_eq!(fs.lookup(root, b"HELLO.TXT"), Ok(new));
    let mut buf = [0u8; 64];
    let read = fs.read_at(pinned, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], HELLO_BODY);
    let read = fs.read_at(new, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], b"new body");
}

/// A removed directory a pin holds keeps its cluster but takes no entries: a
/// lookup, a listing, its parent and a creation or move into it find nothing,
/// it stats with no name, and its last unpin frees its cluster.
#[test]
fn a_removed_pinned_directory_takes_no_entries() {
    let mut fs = mount();
    let root = fs.root();
    let empty = free_clusters(&mut fs);
    let dir = fs
        .create(root, b"GONE", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    assert_eq!(fs.pin(dir), Ok(dir));
    fs.remove(root, b"GONE").expect("rmdir");
    assert_eq!(free_clusters(&mut fs), empty - 1, "its cluster is kept");
    assert_eq!(fs.lookup(root, b"GONE"), Err(DriverError::NotFound));
    assert_eq!(
        fs.create(dir, b"NEW.TXT", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.create(dir, b"NEW", NodeKind::Directory, &DIR_RECORD),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.rename(root, b"HELLO.TXT", dir, b"HELLO.TXT"),
        Err(DriverError::NotFound)
    );
    let file = fs
        .lookup(root, b"HELLO.TXT")
        .expect("the refused move moved nothing");
    assert_eq!(
        fs.link(dir, b"ALIAS.TXT", file),
        Err(DriverError::Unsupported),
        "FAT has no links at all"
    );
    assert_eq!(fs.lookup(dir, b"ANY"), Err(DriverError::NotFound));
    assert_eq!(listed(&mut fs, dir, 0, &[]), Ok(Vec::new()));
    assert_eq!(fs.parent(dir), Err(DriverError::NotFound));
    let info = fs.node_info(dir).expect("info");
    assert_eq!((info.kind, info.nlink), (NodeKind::Directory, 0));
    assert_eq!(fs.unpin(dir), Ok(Unpinned::Freed));
    assert_eq!(free_clusters(&mut fs), empty);
    assert_eq!(fs.node_info(dir), Err(DriverError::NotFound));
}

/// Releasing every hold frees each nameless node held, and leaves a named one
/// as it was.
#[test]
fn release_pins_frees_every_nameless_node() {
    let mut fs = mount();
    let root = fs.root();
    let empty = free_clusters(&mut fs);
    let file = fs
        .create(root, b"A.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(file, 0, &[1u8; 600]).expect("write");
    let dir = fs
        .create(root, b"D", NodeKind::Directory, &DIR_RECORD)
        .expect("mkdir");
    let pinned = fs.pin(file).expect("pin");
    fs.pin(dir).expect("pin");
    let hello = fs.lookup(root, b"HELLO.TXT").expect("found");
    let named = fs.pin(hello).expect("pin");
    fs.remove(root, b"A.TXT").expect("remove");
    fs.remove(root, b"D").expect("rmdir");
    assert_eq!(free_clusters(&mut fs), empty - 3);
    assert_eq!(fs.release_pins(), Ok(()));
    assert_eq!(free_clusters(&mut fs), empty);
    for released in [pinned, dir, named] {
        assert_eq!(fs.unpin(released), Err(DriverError::NotFound));
    }
    assert_eq!(fs.node_info(pinned), Err(DriverError::NotFound));
    assert_eq!(fs.lookup(root, b"HELLO.TXT"), Ok(hello));
    let mut buf = [0u8; 64];
    let read = fs.read_at(hello, 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], HELLO_BODY);
}

/// No pin outlives the mount: a removed file still pinned when the volume
/// goes away leaves its chain allocated and no entry naming it, for the
/// format's own checker to reclaim.
#[test]
fn a_removed_pinned_files_chain_is_left_to_the_checker_when_the_mount_ends() {
    let mut fs = mount();
    let root = fs.root();
    let empty = free_clusters(&mut fs);
    let file = fs
        .create(root, b"OPEN.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(file, 0, &[5u8; 600]).expect("write");
    fs.pin(file).expect("pin");
    fs.remove(root, b"OPEN.TXT").expect("remove");
    let mut fs = Fat32::open(fs.into_block()).expect("remount");
    let root = fs.root();
    assert_eq!(free_clusters(&mut fs), empty - 2);
    let names: Vec<Vec<u8>> = listed(&mut fs, root, 0, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(names, [b"HELLO.TXT".to_vec(), b"SUB".to_vec()]);
}

#[test]
fn flush_is_a_synchronous_no_op() {
    let mut fs = mount();
    assert!(fs.flush().is_ok());
}

// --- Allocated storage (`NodeInfo::allocated` from the FAT chain). ---

#[test]
fn node_info_reports_the_chain_allocation() {
    let mut fs = mount();
    let root = fs.root();
    // One 512-byte cluster each for the root directory and hello.txt;
    // deep.bin chains two clusters.
    assert_eq!(fs.node_info(root).expect("info").allocated, 512);
    let hello = fs.lookup(root, b"hello.txt").expect("found");
    assert_eq!(fs.node_info(hello).expect("info").allocated, 512);
    let sub = fs.lookup(root, b"sub").expect("subdir");
    let deep = fs.lookup(sub, b"deep.bin").expect("deep");
    assert_eq!(fs.node_info(deep).expect("info").allocated, 1024);
}

#[test]
fn node_info_reports_no_allocation_for_an_empty_file() {
    let mut fs = mount();
    let root = fs.root();
    let file = fs
        .create(root, b"empty.txt", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    // A fresh file has no first cluster, so no chain and no allocation.
    assert_eq!(fs.node_info(file).expect("info").allocated, 0);
}

/// The caller-minted BPB volume serial the format tests lay volumes down
/// with; production callers mint a fresh one per volume.
const TEST_SERIAL: u32 = 0x1234_5678;

/// Tests for [`Fat32::format`]: laying down a genuine, mountable FAT32
/// volume on a fresh device, sized large enough to be a real FAT32
/// filesystem, and the data-exhaustion (`NoSpace`) extreme. These use a
/// heap-backed device, so they live behind `std` like the round-trip
/// fixtures above.
mod format {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// A heap-backed [`Block`] device of `block_count` 512-byte sectors,
    /// starting all-zero like a freshly attached disk.
    struct VecBlock {
        store: Vec<u8>,
    }

    impl VecBlock {
        fn new(block_count: u64) -> Self {
            let len = usize::try_from(block_count * SECTOR_SIZE as u64).expect("fits usize");
            Self {
                store: vec![0u8; len],
            }
        }

        fn span(&self, lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
            if len == 0 || !len.is_multiple_of(SECTOR_SIZE) {
                return Err(DriverError::BufferTooSmall);
            }
            let start = usize::try_from(lba)
                .ok()
                .and_then(|l| l.checked_mul(SECTOR_SIZE))
                .ok_or(DriverError::LengthOutOfRange)?;
            let end = start
                .checked_add(len)
                .ok_or(DriverError::LengthOutOfRange)?;
            if end > self.store.len() {
                return Err(DriverError::LengthOutOfRange);
            }
            Ok((start, end))
        }
    }

    impl Block for VecBlock {
        fn geometry(&self) -> Result<BlockGeometry, DriverError> {
            Ok(BlockGeometry {
                block_size: u32::try_from(SECTOR_SIZE).expect("512 fits u32"),
                block_count: (self.store.len() / SECTOR_SIZE) as u64,
            })
        }

        fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            let (start, end) = self.span(lba, buf.len())?;
            buf.copy_from_slice(&self.store[start..end]);
            Ok(())
        }

        fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
            let (start, end) = self.span(lba, buf.len())?;
            self.store[start..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    /// 64 MiB in 512-byte sectors — large enough to be a genuine FAT32
    /// volume (more than `MIN_FAT32_CLUSTERS` clusters) yet cheap to fill.
    const SECTORS_64MIB: u64 = (64 << 20) / 512;

    #[test]
    fn format_produces_a_mountable_empty_volume() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        assert_eq!(
            fs.node_info(root).expect("root info").kind,
            NodeKind::Directory
        );
        // A freshly formatted root directory is empty.
        assert_eq!(listed(&mut fs, root, 0, &[]), Ok(Vec::new()));
    }

    #[test]
    fn format_lays_out_fsinfo_and_the_backup_boot_pair() {
        // Regression: the formatter used to leave BPB offsets 48/50 zero
        // and write no FSInfo structure at all — a format-conformance
        // defect that also left a TAIRiX-formatted volume without the
        // mutation-evidence window the verified re-insert path compares
        // (`plans/DEVICES.md` D4c).
        let fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let mut image = fs.into_block();
        let recognised = tairix_fsprobe::recognise(
            &mut tairix_bytedev::ByteDevice::new(&mut image).expect("geometry"),
        );
        let boot = &image.store[..512];
        let fsinfo_sector = u16::from_le_bytes([boot[48], boot[49]]) as usize;
        let backup_sector = u16::from_le_bytes([boot[50], boot[51]]) as usize;
        assert_eq!(fsinfo_sector, 1);
        assert_eq!(backup_sector, 6);

        // Both FSInfo copies carry the structure's three signatures and
        // the documented "unknown" hints (this driver counts free space
        // from the FAT itself).
        for base in [fsinfo_sector * 512, (backup_sector + 1) * 512] {
            let info = &image.store[base..base + 512];
            assert_eq!(&info[0..4], &0x4161_5252u32.to_le_bytes());
            assert_eq!(&info[484..488], &0x6141_7272u32.to_le_bytes());
            assert_eq!(&info[488..492], &0xFFFF_FFFFu32.to_le_bytes());
            assert_eq!(&info[492..496], &0xFFFF_FFFFu32.to_le_bytes());
            assert_eq!(&info[508..512], &0xAA55_0000u32.to_le_bytes());
        }
        // The backup boot sector is byte-identical to the primary.
        assert_eq!(
            &image.store[backup_sector * 512..backup_sector * 512 + 512],
            boot
        );
        // The formatted volume declares an evidence window covering the boot
        // sector through FSInfo.
        let Ok(Some(tairix_fsprobe::Recognised::Volume(volume))) = recognised else {
            panic!("a formatted volume is recognised");
        };
        assert_eq!(
            volume.evidence,
            Some(tairix_fsprobe::Evidence {
                offset: 0,
                len: ((fsinfo_sector as u64) + 1) * 512
            })
        );
    }

    #[test]
    fn format_then_reopen_round_trips_files_and_directories() {
        let dev = {
            let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
            let root = fs.root();
            let file = fs
                .create(root, b"Notes.txt", NodeKind::RegularFile, &FILE_RECORD)
                .expect("create file");
            fs.write_at(file, 0, b"formatted on TAIRiX")
                .expect("write file");
            let dir = fs
                .create(root, b"Folder", NodeKind::Directory, &DIR_RECORD)
                .expect("mkdir");
            let nested = fs
                .create(dir, b"inner.bin", NodeKind::RegularFile, &FILE_RECORD)
                .expect("create nested");
            fs.write_at(nested, 0, b"nested body")
                .expect("write nested");
            fs.into_block()
        };

        // Re-mount the very bytes the formatter and writes produced.
        let mut fs = Fat32::open(dev).expect("reopen formatted volume");
        let root = fs.root();
        let file = fs.lookup(root, b"Notes.txt").expect("file present");
        let mut buf = [0u8; 32];
        let n = fs.read_at(file, 0, &mut buf).expect("read file");
        assert_eq!(&buf[..n], b"formatted on TAIRiX");

        let dir = fs.lookup(root, b"Folder").expect("dir present");
        let nested = fs.lookup(dir, b"inner.bin").expect("nested present");
        let n = fs.read_at(nested, 0, &mut buf).expect("read nested");
        assert_eq!(&buf[..n], b"nested body");
    }

    #[test]
    fn stats_track_allocation_and_identity_is_remount_stable() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let identity = fs.volume_identity();
        assert_ne!(
            identity, [0u8; 16],
            "the tag byte keeps the identity non-nil"
        );

        let before = fs.stats().expect("stats");
        assert!(before.total_blocks > 0);
        assert!(before.free_blocks <= before.total_blocks);
        assert_eq!(before.avail_blocks, before.free_blocks);
        // FAT32 has no inode table; the zero pair reports that honestly.
        assert_eq!((before.files, before.files_free), (0, 0));

        // Allocating a multi-cluster body drops the maintained count;
        // removing it restores the exact figure.
        let root = fs.root();
        let file = fs
            .create(root, b"stats.bin", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let body = std::vec![0x5Au8; 40_000];
        assert_eq!(fs.write_at(file, 0, &body), Ok(body.len()));
        let after = fs.stats().expect("stats");
        assert!(after.free_blocks < before.free_blocks);
        fs.remove(root, b"stats.bin").expect("remove");
        let restored = fs.stats().expect("stats");
        assert_eq!(restored.free_blocks, before.free_blocks);

        // A fresh open's full FAT scan agrees with the maintained count,
        // and the identity survives the remount.
        let mut fs = Fat32::open(fs.into_block()).expect("reopen");
        assert_eq!(fs.stats().expect("stats").free_blocks, before.free_blocks);
        assert_eq!(fs.volume_identity(), identity);
    }

    #[test]
    fn security_is_uniform_and_stores_are_refused() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        fs.create(root, b"file.txt", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let file = fs.lookup(root, b"file.txt").expect("lookup");

        // The format stores no owner model: one uniform, restrictive,
        // system-owned record per kind, never fabricated per-file
        // ownership.
        let dir_sec = fs.security(root).expect("dir security");
        assert_eq!((dir_sec.mode, dir_sec.uid, dir_sec.gid), (0o755, 0, 0));
        assert!(dir_sec.required_cap.is_none());
        let file_sec = fs.security(file).expect("file security");
        assert_eq!((file_sec.mode, file_sec.uid, file_sec.gid), (0o644, 0, 0));

        // A store would be silently lossy, so it is refused whole.
        assert_eq!(
            fs.set_security(file, NodeSecurity::new(0o600, 1000, 100)),
            Err(DriverError::Unsupported)
        );

        // The record stands in for one the format does not keep, so the
        // mount policy may map it; a missing node still refuses.
        assert_eq!(fs.records_ownership(root), Ok(false));
        assert_eq!(fs.records_ownership(file), Ok(false));
        assert_eq!(
            fs.records_ownership(NodeId::NONE),
            Err(DriverError::NotFound)
        );
    }

    /// FAT keeps no owner or mode, so the record a node is created with has
    /// nowhere to go: the node reports the record every node does.
    #[test]
    fn a_new_node_reports_the_record_every_node_does() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let asked = NodeSecurity::new(0o600, 1000, 100);
        let file = fs
            .create(root, b"made.txt", NodeKind::RegularFile, &asked)
            .expect("create");
        let dir = fs
            .create(root, b"made", NodeKind::Directory, &asked)
            .expect("mkdir");
        let sec = fs.security(file).expect("security");
        assert_eq!((sec.mode, sec.uid, sec.gid), (0o644, 0, 0));
        let sec = fs.security(dir).expect("security");
        assert_eq!((sec.mode, sec.uid, sec.gid), (0o755, 0, 0));
        assert_eq!(fs.node_info(NodeId::NONE), Err(DriverError::NotFound));
        assert_eq!(fs.security(NodeId::NONE), Err(DriverError::NotFound));
        let mut buf = [0u8; 4];
        assert_eq!(
            fs.read_at(NodeId::NONE, 0, &mut buf),
            Err(DriverError::NotFound)
        );
    }

    /// A file's id is its entry's place, which its writes and truncations
    /// rewrite where it lies: the id lasts through them.
    #[test]
    fn a_files_id_lasts_through_its_writes_and_truncations() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let file = fs
            .create(root, b"grows.txt", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        fs.write_at(file, 0, &vec![7u8; 100_000]).expect("write");
        assert_eq!(fs.lookup(root, b"grows.txt"), Ok(file));
        assert_eq!(fs.node_info(file).expect("info").size, 100_000);
        fs.truncate(file, 5).expect("shrink");
        fs.truncate(file, 0).expect("empty");
        fs.write_at(file, 3, b"again").expect("write");
        assert_eq!(fs.lookup(root, b"grows.txt"), Ok(file));
        let info = fs.node_info(file).expect("info");
        assert_eq!((info.size, info.ino), (8, file.raw()));
    }

    #[test]
    fn filling_a_formatted_volume_reports_no_space() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let big = fs
            .create(root, b"BIG.DAT", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        // Append 1 MiB at a time until the data region is exhausted. The
        // first failure must be NoSpace (the volume is full), never a
        // DeviceFault, and the volume must fill well before any absurd size.
        let chunk = vec![0xCDu8; 1 << 20];
        let mut offset = 0u64;
        let mut result = Ok(0usize);
        while offset < (128 << 20) {
            result = fs.write_at(big, offset, &chunk);
            match result {
                Ok(n) => offset += n as u64,
                Err(_) => break,
            }
        }
        assert_eq!(result, Err(DriverError::NoSpace));
        // Something substantial was stored before the volume filled.
        assert!(
            offset >= (32 << 20),
            "only {offset} bytes written before full"
        );
    }

    /// Aliases are found past the first window of tails one walk examines.
    #[test]
    fn an_alias_past_a_thousand_held_tails_is_found() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        for n in 1..=1025u32 {
            let name = std::format!("Report {n}.txt");
            fs.create(root, name.as_bytes(), NodeKind::RegularFile, &FILE_RECORD)
                .expect("create");
        }
        let root_cluster = first_cluster(root);
        assert_eq!(
            aliases(&mut fs, root_cluster).last(),
            Some(&short_name(b"REP~1025", b"TXT"))
        );
    }

    /// A directory holds no more than the specification's 65,536 slots: a
    /// create that would grow it past them is refused, the directory stays
    /// listable, and freed slots are taken again.
    #[test]
    fn a_full_directory_refuses_a_new_name_and_reuses_freed_slots() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let dir = fs
            .create(root, b"FULL", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let mut pos = SlotPos {
            cluster: first_cluster(dir),
            intra: 2 * DIR_ENTRY_LEN as u64,
            slot: 2,
        };
        let name = |slot: u64| short_name(std::format!("F{slot:07X}").as_bytes(), b"BIN");
        while pos.slot < u64::from(MAX_DIR_SLOTS) {
            let entry = Fat32::<VecBlock>::build_short_entry(
                &name(pos.slot),
                0x20,
                0,
                0,
                EntryStamps::NONE,
            );
            fs.put_slot(&mut pos, &entry).expect("fill");
        }
        assert_eq!(
            fs.create(dir, b"one.txt", NodeKind::RegularFile, &FILE_RECORD),
            Err(DriverError::NoSpace)
        );
        assert_eq!(listed(&mut fs, dir, 0, &[]).expect("lists").len(), 65_534);

        fs.remove(dir, b"F0000010.BIN").expect("remove");
        fs.remove(dir, b"F0000011.BIN").expect("remove");
        fs.create(dir, b"one.txt", NodeKind::RegularFile, &FILE_RECORD)
            .expect("the freed slots hold it");
        assert!(fs.lookup(dir, b"one.txt").is_ok());
    }

    #[test]
    fn rename_within_directory_preserves_contents() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let a = fs
            .create(root, b"A.TXT", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        fs.write_at(a, 0, b"hello").unwrap();
        fs.rename(root, b"A.TXT", root, b"B.TXT").expect("rename");
        assert_eq!(fs.lookup(root, b"A.TXT"), Err(DriverError::NotFound));
        let node = fs.lookup(root, b"B.TXT").expect("dst");
        let mut buf = [0u8; 8];
        let n = fs.read_at(node, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    /// FAT matches names ignoring case, so `foo.txt` names `Foo.txt`'s own
    /// entry: the rename re-spells it rather than reporting success while
    /// changing nothing, and the data stays put.
    #[test]
    fn a_case_only_rename_respells_the_entry_and_keeps_its_data() {
        let dev = {
            let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
            let root = fs.root();
            let foo = fs
                .create(root, b"Foo.txt", NodeKind::RegularFile, &FILE_RECORD)
                .unwrap();
            fs.write_at(foo, 0, b"hello").unwrap();
            fs.rename(root, b"Foo.txt", root, b"foo.txt")
                .expect("re-spell");
            fs.into_block()
        };
        let mut fs = Fat32::open(dev).expect("reopen");
        let root = fs.root();
        let names: Vec<Vec<u8>> = listed(&mut fs, root, 0, &[])
            .expect("lists")
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, [b"foo.txt".to_vec()]);
        let node = fs.lookup(root, b"foo.txt").expect("found");
        let mut buf = [0u8; 8];
        let n = fs.read_at(node, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn a_rename_to_the_name_already_stored_changes_nothing() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        fs.create(root, b"Same.txt", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        let before = fs
            .find_child(first_cluster(root), b"Same.txt")
            .unwrap()
            .unwrap();
        fs.rename(root, b"Same.txt", root, b"Same.txt")
            .expect("no-op");
        let after = fs
            .find_child(first_cluster(root), b"Same.txt")
            .unwrap()
            .unwrap();
        assert_eq!(before.short_offset, after.short_offset);
    }

    /// A rename rewrites the entry, which is where FAT keeps a file's stamps:
    /// they travel with it rather than resetting to "no stamp".
    #[test]
    fn a_rename_keeps_the_entry_s_stamps() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let sub = fs
            .create(root, b"SUB", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        fs.create(root, b"old.txt", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        let entry = fs
            .find_child(first_cluster(root), b"old.txt")
            .unwrap()
            .unwrap();
        // 2024-03-15 10:20:30 created, accessed 2024-03-16, written 2025-01-02 03:04:06.
        let created = [0x64, 0x8F, 0x52, 0x6F, 0x58, 0x70, 0x58];
        let written = [0x83, 0x18, 0x22, 0x5A];
        fs.dev.write(entry.short_offset + 13, &created).unwrap();
        fs.dev.write(entry.short_offset + 22, &written).unwrap();

        fs.rename(root, b"old.txt", root, b"new.txt")
            .expect("rename");
        let moved = fs
            .find_child(first_cluster(root), b"new.txt")
            .unwrap()
            .unwrap();
        assert_eq!(moved.stamps.created, created);
        assert_eq!(moved.stamps.written, written);
        fs.rename(root, b"new.txt", sub, b"New.TXT").expect("move");
        let moved = fs
            .find_child(first_cluster(sub), b"New.TXT")
            .unwrap()
            .unwrap();
        assert_eq!(moved.stamps.created, created);
        assert_eq!(moved.stamps.written, written);
        assert_ne!(moved.times.modified, Time64::UNIX_EPOCH);
    }

    #[test]
    fn rename_missing_source_is_not_found() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        assert_eq!(
            fs.rename(root, b"X", root, b"Y"),
            Err(DriverError::NotFound)
        );
    }

    #[test]
    fn rename_across_directories_persists() {
        let dev = {
            let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
            let root = fs.root();
            let src = fs
                .create(root, b"SRC", NodeKind::Directory, &DIR_RECORD)
                .unwrap();
            let dst = fs
                .create(root, b"DST", NodeKind::Directory, &DIR_RECORD)
                .unwrap();
            let f = fs
                .create(src, b"F.BIN", NodeKind::RegularFile, &FILE_RECORD)
                .unwrap();
            fs.write_at(f, 0, b"data").unwrap();
            fs.rename(src, b"F.BIN", dst, b"G.BIN").expect("move");
            fs.into_block()
        };
        let mut fs = Fat32::open(dev).expect("reopen");
        let root = fs.root();
        let src = fs.lookup(root, b"SRC").unwrap();
        let dst = fs.lookup(root, b"DST").unwrap();
        assert_eq!(fs.lookup(src, b"F.BIN"), Err(DriverError::NotFound));
        let node = fs.lookup(dst, b"G.BIN").expect("moved");
        let mut buf = [0u8; 8];
        let n = fs.read_at(node, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[test]
    fn rename_overwrites_existing_file() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let a = fs
            .create(root, b"A.TXT", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        fs.write_at(a, 0, b"AAAA").unwrap();
        let b = fs
            .create(root, b"B.TXT", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        fs.write_at(b, 0, b"BB").unwrap();
        fs.rename(root, b"A.TXT", root, b"B.TXT")
            .expect("overwrite");
        assert_eq!(fs.lookup(root, b"A.TXT"), Err(DriverError::NotFound));
        let node = fs.lookup(root, b"B.TXT").unwrap();
        let mut buf = [0u8; 8];
        let n = fs.read_at(node, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"AAAA");
    }

    #[test]
    fn rename_refuses_kind_mismatch_and_nonempty_dir_target() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        fs.create(root, b"F.TXT", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        fs.create(root, b"D", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        assert_eq!(
            fs.rename(root, b"F.TXT", root, b"D"),
            Err(DriverError::Unsupported)
        );
        assert_eq!(
            fs.rename(root, b"D", root, b"F.TXT"),
            Err(DriverError::Unsupported)
        );
        let d2 = fs
            .create(root, b"D2", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        fs.create(d2, b"CHILD", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        assert_eq!(
            fs.rename(root, b"D", root, b"D2"),
            Err(DriverError::DirectoryNotEmpty)
        );
    }

    #[test]
    fn rename_moves_a_directory_across_parents() {
        let dev = {
            let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
            let root = fs.root();
            let p1 = fs
                .create(root, b"P1", NodeKind::Directory, &DIR_RECORD)
                .unwrap();
            let p2 = fs
                .create(root, b"P2", NodeKind::Directory, &DIR_RECORD)
                .unwrap();
            let d = fs
                .create(p1, b"D", NodeKind::Directory, &DIR_RECORD)
                .unwrap();
            let leaf = fs
                .create(d, b"LEAF.BIN", NodeKind::RegularFile, &FILE_RECORD)
                .unwrap();
            fs.write_at(leaf, 0, b"x").unwrap();
            fs.rename(p1, b"D", p2, b"D").expect("move dir");
            fs.into_block()
        };
        let mut fs = Fat32::open(dev).expect("reopen");
        let root = fs.root();
        let p1 = fs.lookup(root, b"P1").unwrap();
        let p2 = fs.lookup(root, b"P2").unwrap();
        assert_eq!(fs.lookup(p1, b"D"), Err(DriverError::NotFound));
        let moved = fs.lookup(p2, b"D").expect("moved");
        let leaf = fs.lookup(moved, b"LEAF.BIN").expect("leaf intact");
        let mut buf = [0u8; 4];
        let n = fs.read_at(leaf, 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"x");
    }

    #[test]
    fn rename_refuses_moving_directory_into_its_subtree() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let a = fs
            .create(root, b"A", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        let b = fs
            .create(a, b"B", NodeKind::Directory, &DIR_RECORD)
            .unwrap();
        assert_eq!(
            fs.rename(root, b"A", b, b"A"),
            Err(DriverError::DirectoryCycle)
        );
        assert_eq!(
            fs.rename(root, b"A", a, b"X"),
            Err(DriverError::DirectoryCycle)
        );
    }

    #[test]
    fn rename_rejects_bad_destination_name() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        fs.create(root, b"A.TXT", NodeKind::RegularFile, &FILE_RECORD)
            .unwrap();
        assert_eq!(
            fs.rename(root, b"A.TXT", root, b""),
            Err(DriverError::LengthOutOfRange)
        );
        assert_eq!(
            fs.rename(root, b"A.TXT", root, b".."),
            Err(DriverError::Unsupported)
        );
    }

    #[test]
    fn format_rejects_a_device_too_small_for_fat32() {
        // 8 MiB is far below the FAT32 minimum cluster count.
        let too_small = (8 << 20) / 512;
        assert_eq!(
            Fat32::format(VecBlock::new(too_small), TEST_SERIAL).map(|_| ()),
            Err(DriverError::OutOfRange)
        );
    }

    #[test]
    fn format_refuses_a_zero_serial() {
        // The all-zero serial is the "none recorded" value: laying it
        // down would give every fresh volume one shared identity.
        assert_eq!(
            Fat32::format(VecBlock::new(SECTORS_64MIB), 0).map(|_| ()),
            Err(DriverError::OutOfRange)
        );
    }

    #[test]
    fn distinct_serials_yield_distinct_identities() {
        let a = Fat32::format(VecBlock::new(SECTORS_64MIB), 0x0000_0001).expect("format a");
        let b = Fat32::format(VecBlock::new(SECTORS_64MIB), 0x0000_0002).expect("format b");
        assert_ne!(
            a.volume_identity(),
            b.volume_identity(),
            "the caller-minted serial distinguishes otherwise identical volumes"
        );
    }

    /// `fs`, stamping `wall` from now on, or no time.
    fn at<B: Block>(mut fs: Fat32<B>, wall: Option<Time64>) -> Fat32<B> {
        fs.set_volume_host(TestClock::volume(), TestClock::at(wall));
        fs
    }

    #[test]
    fn each_operation_stamps_what_it_changed_at_the_hosts_time() {
        // An even second, which the two-second write field holds exactly.
        let later = Time64::from_secs(stamp_instant().secs() + 101);
        let mut fs = at(
            Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format"),
            Some(stamp_instant()),
        );
        let root = fs.root();
        let file = fs
            .create(root, b"f.txt", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let made = fs
            .find_child(first_cluster(root), b"f.txt")
            .expect("read")
            .expect("found")
            .times;
        assert_eq!(
            made.created,
            Time64::new(1_792_067_697, 120_000_000).expect("canonical")
        );
        assert_eq!(made.modified, Time64::from_secs(1_792_067_696));
        assert_eq!(
            made.accessed,
            Time64::from_secs(1_792_022_400),
            "the access date, at midnight"
        );

        let mut fs = at(fs, Some(later));
        fs.write_at(file, 0, b"data").expect("write");
        let written = fs
            .find_child(first_cluster(root), b"f.txt")
            .expect("read")
            .expect("found");
        assert_eq!(written.times.modified, later);
        assert_eq!(written.times.created, made.created);
        assert_eq!(written.size, 4, "the stamp's write carried the size too");

        let dir = fs
            .create(root, b"sub", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let dot = fs
            .find_child(first_cluster(root), b"sub")
            .expect("read")
            .expect("found");
        let mut raw = [0u8; 32];
        fs.dev
            .read(fs.cluster_byte(first_cluster(dir)), &mut raw)
            .expect("read");
        assert_eq!(
            crate::EntryStamps::of(&raw),
            dot.stamps,
            "a new directory's `.` is stamped as its entry is"
        );
    }

    /// A file's stamps live in its entry, which its id reaches: a stat by
    /// node reports what a listing does.
    #[test]
    fn a_files_stat_by_node_reports_its_entrys_stamps() {
        let mut fs = at(
            Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format"),
            Some(stamp_instant()),
        );
        let root = fs.root();
        let file = fs
            .create(root, b"f.txt", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let stat = fs.node_info(file).expect("info");
        assert_eq!(stat.times.modified, Time64::from_secs(1_792_067_696));
        assert_eq!(
            stat.times.created,
            Time64::new(1_792_067_697, 120_000_000).expect("canonical")
        );
        let entries = listed(&mut fs, root, 0, &[]).expect("lists");
        assert_eq!(entries[0].0.node, file);
        assert_eq!(entries[0].0.info, stat);
    }

    /// A directory's stamps live in its entry in the parent its `..` names:
    /// a stat by node reports what a listing of that parent does, at any
    /// depth, and the root, which no entry names, reports none.
    #[test]
    fn a_directorys_stat_by_node_reports_its_entrys_stamps() {
        let mut fs = at(
            Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format"),
            Some(stamp_instant()),
        );
        let root = fs.root();
        let outer = fs
            .create(root, b"outer", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let mut fs = at(fs, Some(Time64::from_secs(stamp_instant().secs() + 3600)));
        fs.create(outer, b"spacer.txt", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let inner = fs
            .create(outer, b"inner", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        for (dir, parent) in [(outer, root), (inner, outer)] {
            let stat = fs.node_info(dir).expect("info");
            assert_ne!(stat.times.modified, Time64::UNIX_EPOCH);
            let listing = listed(&mut fs, parent, 0, &[]).expect("lists");
            let entry = listing
                .iter()
                .find(|(entry, _)| entry.node == dir)
                .expect("listed");
            assert_eq!(entry.0.info, stat);
        }
        assert_ne!(
            fs.node_info(outer).expect("info").times,
            fs.node_info(inner).expect("info").times
        );
        assert_eq!(
            fs.node_info(root).expect("info").times,
            NodeTimes::default()
        );
    }

    /// A damaged `..` leaves a directory's stamps out of reach, never its
    /// stat: the directory still stats and lists, and only a climb fails.
    #[test]
    fn a_directory_with_a_damaged_dotdot_stats_without_stamps() {
        let mut fs = at(
            Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format"),
            Some(stamp_instant()),
        );
        let root = fs.root();
        let dir = fs
            .create(root, b"sub", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let dotdot = fs.cluster_byte(first_cluster(dir)) + 32;
        fs.dev.write(dotdot, &[b'X'; 11]).expect("damage");
        let stat = fs.node_info(dir).expect("stats");
        assert_eq!(stat.kind, NodeKind::Directory);
        assert_eq!(stat.times, NodeTimes::default());
        assert_eq!(fs.parent(dir), Err(DriverError::DeviceFault));
    }

    #[test]
    fn with_no_time_to_give_fat32_records_no_date() {
        let mut fs = Fat32::format(VecBlock::new(SECTORS_64MIB), TEST_SERIAL).expect("format");
        let root = fs.root();
        let file = fs
            .create(root, b"f.txt", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let mut fs = at(fs, Some(Time64::from_secs(4_500_000_000)));
        fs.write_at(file, 0, b"data").expect("write");
        let entry = fs
            .find_child(first_cluster(root), b"f.txt")
            .expect("read")
            .expect("found");
        assert_eq!(entry.stamps, crate::EntryStamps::NONE);
        assert_eq!(
            (
                entry.times.created,
                entry.times.modified,
                entry.times.accessed
            ),
            (Time64::UNIX_EPOCH, Time64::UNIX_EPOCH, Time64::UNIX_EPOCH)
        );
    }
}

/// Pack a DOS date word from its calendar fields.
fn dos_date(year: u16, month: u16, day: u16) -> u16 {
    ((year - 1980) << 9) | (month << 5) | day
}

/// Pack a DOS time word from its clock fields.
fn dos_time(hour: u16, minute: u16, second: u16) -> u16 {
    (hour << 11) | (minute << 5) | (second / 2)
}

#[test]
fn dos_datetime_start_of_epoch_decodes() {
    // 1980-01-01 00:00:00 UTC, the earliest FAT timestamp.
    assert_eq!(
        crate::dos_datetime_to_time64(dos_date(1980, 1, 1), dos_time(0, 0, 0)),
        Time64::from_secs(315_532_800)
    );
}

#[test]
fn dos_datetime_leap_day_decodes() {
    // 2020-02-29 00:00:00 UTC — a valid leap day.
    assert_eq!(
        crate::dos_datetime_to_time64(dos_date(2020, 2, 29), dos_time(0, 0, 0)),
        Time64::from_secs(1_582_934_400)
    );
}

#[test]
fn dos_datetime_end_of_range_decodes_past_2038() {
    // 2107-12-31 23:59:58 UTC, the last representable FAT instant — far
    // beyond the 32-bit 2038 boundary, so the wide decode must not wrap.
    assert_eq!(
        crate::dos_datetime_to_time64(dos_date(2107, 12, 31), dos_time(23, 59, 58)),
        Time64::from_secs(4_354_819_198)
    );
}

#[test]
fn dos_datetime_undecodable_fields_report_no_stamp() {
    let ok_time = dos_time(12, 0, 0);
    let ok_date = dos_date(2024, 6, 15);
    // Month 0 and 13, day 0, a non-leap February 29th, and hour 24 are not
    // calendar instants; each reports the "no stamp" epoch, never a guess.
    assert_eq!(
        crate::dos_datetime_to_time64((44 << 9) | 15, ok_time),
        Time64::UNIX_EPOCH
    );
    assert_eq!(
        crate::dos_datetime_to_time64((44 << 9) | (13 << 5) | 15, ok_time),
        Time64::UNIX_EPOCH
    );
    assert_eq!(
        crate::dos_datetime_to_time64(dos_date(2024, 6, 0), ok_time),
        Time64::UNIX_EPOCH
    );
    assert_eq!(
        crate::dos_datetime_to_time64(dos_date(2023, 2, 29), ok_time),
        Time64::UNIX_EPOCH
    );
    assert_eq!(
        crate::dos_datetime_to_time64(ok_date, 24 << 11),
        Time64::UNIX_EPOCH
    );
}

/// 2026-10-15T12:34:57.123456789Z.
fn stamp_instant() -> Time64 {
    Time64::new(1_792_067_697, 123_456_789).expect("canonical")
}

#[test]
fn a_dos_stamp_holds_the_creation_to_the_hundredth_and_the_write_to_two_seconds() {
    let dos = crate::DosStamp::of(stamp_instant()).expect("in range");
    assert_eq!(dos.date, dos_date(2026, 10, 15));
    assert_eq!(dos.time, dos_time(12, 34, 56));
    assert_eq!(dos.centis, 112);
    assert_eq!(
        crate::dos_creation_to_time64(dos.date, dos.time, dos.centis),
        Time64::new(1_792_067_697, 120_000_000).expect("canonical")
    );
    assert_eq!(
        crate::dos_datetime_to_time64(dos.date, dos.time),
        Time64::from_secs(1_792_067_696)
    );
}

#[test]
fn a_dos_stamp_covers_1980_to_2107_and_nothing_either_side() {
    let at = |year, month, day, hour, minute, second| {
        CivilTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
        .to_time64()
        .expect("a calendar instant")
    };
    assert_eq!(crate::DosStamp::of(at(1979, 12, 31, 23, 59, 59)), None);
    assert!(crate::DosStamp::of(at(1980, 1, 1, 0, 0, 0)).is_some());
    assert!(crate::DosStamp::of(at(2107, 12, 31, 23, 59, 59)).is_some());
    assert_eq!(crate::DosStamp::of(at(2108, 1, 1, 0, 0, 0)), None);
}

#[test]
fn creation_hundredths_past_two_seconds_are_no_stamp() {
    let (date, time) = (dos_date(2024, 6, 15), dos_time(12, 0, 0));
    assert_eq!(
        crate::dos_creation_to_time64(date, time, 199),
        Time64::new(
            crate::dos_datetime_to_time64(date, time).secs() + 1,
            990_000_000
        )
        .expect("canonical")
    );
    assert_eq!(
        crate::dos_creation_to_time64(date, time, 200),
        Time64::UNIX_EPOCH
    );
}

#[test]
fn fat_has_no_second_name_for_a_file_and_reports_the_one_it_has() {
    // FAT has no link object of either kind: a directory entry *is* the
    // file's identity, so a second name would be a second file. Both link
    // calls refuse rather than approximating one, and every node reports the
    // single name the walk used.
    let mut fs = mount();
    let root = fs.root();
    let file = fs.lookup(root, b"HELLO.TXT").expect("found");
    assert_eq!(
        fs.link(root, b"ALIAS.TXT", file),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        fs.create_link(root, b"ALIAS.TXT", b"/HELLO.TXT", &LINK_RECORD),
        Err(DriverError::Unsupported)
    );
    assert_eq!(fs.lookup(root, b"ALIAS.TXT"), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(file).expect("stat").nlink, 1);
    assert_eq!(fs.node_info(root).expect("stat").nlink, 1);
}

/// The image's FAT occupies the sector after the boot sector.
const FAT_LBA: u64 = 1;

/// Reading a directory one entry per batch walks its cluster chain a small
/// multiple of one whole listing's walk, not once from its start per batch.
#[test]
fn a_listing_read_a_batch_at_a_time_walks_its_chain_once() {
    let mut fs = Fat32::open(CountingBlock {
        inner: MockBlock {
            data: build_image(),
        },
        read_at: Vec::new(),
    })
    .expect("valid volume");
    let root = fs.root();
    let big = fs
        .create(root, b"BIG", NodeKind::Directory, &DIR_RECORD)
        .expect("dir");
    for i in 0..40 {
        fs.create(
            big,
            alloc::format!("F{i:02}.TXT").as_bytes(),
            NodeKind::RegularFile,
            &FILE_RECORD,
        )
        .expect("file");
    }
    let fat_reads = |fs: &Fat32<CountingBlock>| {
        fs.dev
            .device()
            .read_at
            .iter()
            .filter(|at| **at == FAT_LBA)
            .count()
    };
    fs.dev.device_mut().read_at.clear();
    assert_eq!(listed(&mut fs, big, 0, &[]).expect("lists").len(), 40);
    let whole = fat_reads(&fs);
    fs.dev.device_mut().read_at.clear();
    let mut cursor = 0;
    let mut batches = 0;
    while let Some((entry, _)) = first_listed(&mut fs, big, cursor, &[]).expect("lists") {
        cursor = entry.next_cursor;
        batches += 1;
    }
    assert_eq!(batches, 40);
    let one_by_one = fat_reads(&fs);
    assert!(
        one_by_one <= 3 * whole,
        "{one_by_one} chain reads a batch at a time against {whole} for the whole listing"
    );
}

/// An entry written across the slot a batch last took was not there when
/// the listing passed it, so the next batch skips it, rather than listing it
/// by its 8.3 alias, and goes on to what follows.
#[test]
fn a_listing_resumed_past_an_entry_written_across_its_place_skips_it() {
    let mut fs = mount();
    let root = fs.root();
    let dir = fs
        .create(root, b"MID", NodeKind::Directory, &DIR_RECORD)
        .expect("dir");
    for name in [&b"A.TXT"[..], b"B.TXT", b"C.TXT"] {
        fs.create(dir, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("file");
    }
    let (first, name) = first_listed(&mut fs, dir, 0, &[])
        .expect("lists")
        .expect("an entry");
    assert_eq!(name, b"A.TXT");
    fs.remove(dir, b"A.TXT").expect("remove");
    fs.remove(dir, b"B.TXT").expect("remove");
    // Two long-name fragments and a short entry: the lowest free run, which
    // runs through A's old short entry to the slot the cursor names.
    fs.create(
        dir,
        b"two fragments.txt",
        NodeKind::RegularFile,
        &FILE_RECORD,
    )
    .expect("file");
    let rest: Vec<Vec<u8>> = listed(&mut fs, dir, first.next_cursor, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(rest, [b"C.TXT".to_vec()]);
}

/// A listing resumed across changes hands over every entry present for the
/// whole of it exactly once, whatever was added or removed between batches.
#[test]
fn a_listing_resumed_across_changes_lists_each_lasting_entry_once() {
    let mut fs = mount();
    let root = fs.root();
    let dir = fs
        .create(root, b"CHG", NodeKind::Directory, &DIR_RECORD)
        .expect("dir");
    for name in [&b"A.TXT"[..], b"B.TXT", b"C.TXT", b"D.TXT"] {
        fs.create(dir, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("file");
    }
    let mut taken = Vec::new();
    let mut cursor = 0;
    for _ in 0..2 {
        let (entry, name) = first_listed(&mut fs, dir, cursor, &[])
            .expect("lists")
            .expect("an entry");
        taken.push(name);
        cursor = entry.next_cursor;
    }
    fs.remove(dir, b"B.TXT").expect("remove a listed entry");
    fs.create(dir, b"E.TXT", NodeKind::RegularFile, &FILE_RECORD)
        .expect("file");
    taken.extend(
        listed(&mut fs, dir, cursor, &[])
            .expect("lists")
            .into_iter()
            .map(|(_, name)| name),
    );
    for lasting in [&b"A.TXT"[..], b"C.TXT", b"D.TXT"] {
        assert_eq!(
            taken
                .iter()
                .filter(|name| name.as_slice() == lasting)
                .count(),
            1,
            "{lasting:?} in {taken:?}"
        );
    }
    assert!(
        taken
            .iter()
            .filter(|name| name.as_slice() == b"E.TXT")
            .count()
            <= 1
    );
}
