extern crate std;

use std::collections::{BTreeMap, BTreeSet};
use std::format;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemRead, FilesystemStats, FilesystemWrite, NodeId, NodeKind,
};
use tairix_abi::DriverError;

use super::*;
use crate::htree::Level;
use crate::tests::{checksummed, SizedBlock, ONE_GROUP_SECTORS, TEST_UUID};
use crate::{
    le16, node_inode, put_le32, Child, COMPAT_DIR_INDEX, FT_REG, INODE_FLAGS, INODE_FLAG_INDEX,
    SB_FLAGS_OFFSET, SB_FLAG_SIGNED_HASH, SUPERBLOCK_OFFSET,
};

/// The format's `half_md4` hash version.
const HALF_MD4: u8 = 1;

fn small(value: usize) -> u16 {
    u16::try_from(value).expect("small")
}

/// A formatted volume that indexes directories, `flags` recording how it
/// reads name bytes.
fn volume(flags: u32) -> Ext4<SizedBlock> {
    let fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 1024, TEST_UUID).expect("format");
    let mut device = fs.into_block();
    let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("small");
    let compat = le32(&device.data, sb + 0x5C) | COMPAT_DIR_INDEX;
    put_le32(&mut device.data, sb + 0x5C, compat);
    put_le32(&mut device.data, sb + SB_FLAGS_OFFSET, flags);
    Ext4::open(device).expect("remount")
}

/// The checksum the format gives the index whose count sits at `at` in
/// `block`, with where its tail lies. Written apart from the driver's, so a
/// wrong region there cannot also pass here.
fn dx_tail(block: &[u8], at: usize, seed: u32) -> (usize, u32) {
    let limit = usize::from(le16(block, at));
    let count = usize::from(le16(block, at + 2));
    let tail = at + limit * 8;
    let crc = tairix_crc32c::update(seed, &block[..at + count * 8]);
    let crc = tairix_crc32c::update(crc, &block[tail..tail + 4]);
    (tail, tairix_crc32c::update(crc, &[0; 4]))
}

/// Rebuild directory `ino`, which names nothing yet, as an indexed one: its
/// block 0 a root of hash `version` naming one empty leaf, block 1. Laid out
/// from the format, not by the driver's index code.
fn index_directory<B: Block>(fs: &mut Ext4<B>, ino: u32, version: u8) {
    let bs = fs.layout.block_size as usize;
    let dir = fs.read_inode(ino).expect("inode");
    let seed = fs.dir_seed(ino, &dir);
    let mut leaf = vec![0u8; bs];
    put_le16(&mut leaf, 4, small(fs.dir_data_end()));
    if let Some(seed) = seed {
        Ext4::<B>::seal_leaf(seed, &mut leaf).expect("seal");
    }
    let mut blocks = 1;
    fs.append_dir_block(ino, &mut blocks, &leaf).expect("grow");
    let phys = fs.map_block(&dir, 0).expect("map").expect("block 0");
    let mut root = vec![0u8; bs];
    fs.read_fs_block(phys, &mut root).expect("read");
    root[24..].fill(0);
    put_le16(&mut root, 16, small(bs - 12));
    root[28] = version;
    root[29] = 8;
    let limit = (bs - 32 - if seed.is_some() { 8 } else { 0 }) / 8;
    put_le16(&mut root, 32, small(limit));
    put_le16(&mut root, 34, 1);
    put_le32(&mut root, 36, 1);
    if let Some(seed) = seed {
        let (tail, crc) = dx_tail(&root, 32, seed);
        put_le32(&mut root, tail + 4, crc);
    }
    fs.write_fs_block(phys, &root).expect("write");
    set_index_flag(fs, ino, true);
}

fn set_index_flag<B: Block>(fs: &mut Ext4<B>, ino: u32, on: bool) {
    let mut raw = [0u8; MAX_BLOCK_SIZE as usize];
    fs.read_inode_raw(ino, &mut raw).expect("raw");
    let flags = le32(&raw, INODE_FLAGS);
    let flags = if on {
        flags | INODE_FLAG_INDEX
    } else {
        flags & !INODE_FLAG_INDEX
    };
    put_le32(&mut raw, INODE_FLAGS, flags);
    fs.write_inode_raw(ino, &mut raw).expect("write raw");
}

/// A new indexed directory `name` under the root, and a file every name
/// added to it can stand for.
fn indexed_dir<B: Block>(fs: &mut Ext4<B>, name: &[u8]) -> (NodeId, u32, u32) {
    let root = fs.root();
    let dir = fs.create(root, name, NodeKind::Directory).expect("mkdir");
    let ino = node_inode(dir).expect("inode");
    index_directory(fs, ino, HALF_MD4);
    let mut file_name = b"target-".to_vec();
    file_name.extend_from_slice(name);
    let file = fs
        .create(root, &file_name, NodeKind::RegularFile)
        .expect("file");
    (dir, ino, node_inode(file).expect("inode"))
}

/// Add each of `names` to directory `ino`, naming inode `target`.
fn add<B: Block>(fs: &mut Ext4<B>, ino: u32, target: u32, names: &[Vec<u8>]) {
    for name in names {
        fs.insert_child(
            ino,
            Child {
                name,
                ino: target,
                file_type: FT_REG,
            },
        )
        .expect("insert");
    }
}

/// `count` names from `first` on, of lengths running up to `longest` bytes.
fn names(first: usize, count: usize, longest: usize) -> Vec<Vec<u8>> {
    (first..first + count)
        .map(|n| {
            let mut name = format!("n{n:05}-").into_bytes();
            name.resize(8 + n % (longest - 7), b"abcdefghijklmnopqrstuvwxyz"[n % 26]);
            name
        })
        .collect()
}

/// [`names`], every one `len` bytes long.
fn long_names(first: usize, count: usize, len: usize) -> Vec<Vec<u8>> {
    names(first, count, 8)
        .into_iter()
        .map(|mut name| {
            name.resize(len, b'z');
            name
        })
        .collect()
}

/// Every name `dir` lists, read `batch` at a time and resumed as the VFS
/// does — from each entry's cursor, after its name — with `between` run
/// before every batch after the first. Answers the names and their cursors.
fn list_with<B: Block>(
    fs: &mut Ext4<B>,
    dir: NodeId,
    batch: usize,
    mut between: impl FnMut(&mut Ext4<B>),
) -> Vec<(Vec<u8>, u64)> {
    let (mut cursor, mut after) = (0u64, Vec::new());
    let mut out = Vec::new();
    loop {
        let resume = after.clone();
        let mut taken = 0;
        fs.read_dir(dir, cursor, &resume, &mut |entry, name| {
            if taken == batch {
                return DirVisit::Stop;
            }
            out.push((name.to_vec(), entry.next_cursor));
            cursor = entry.next_cursor;
            after = name.to_vec();
            taken += 1;
            DirVisit::Take
        })
        .expect("list");
        if taken < batch {
            return out;
        }
        between(fs);
    }
}

fn list<B: Block>(fs: &mut Ext4<B>, dir: NodeId, batch: usize) -> Vec<Vec<u8>> {
    list_with(fs, dir, batch, |_| {})
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// What [`check_tree`] found.
struct Tree {
    levels: u8,
    root_count: usize,
    leaves: usize,
    names: BTreeSet<Vec<u8>>,
    /// Names sitting in a leaf whose range does not cover their hash.
    strays: BTreeSet<Vec<u8>>,
}

/// The entries of the index whose count sits at `at` in `data`, checked: its
/// limit, count, order, and on a checksummed volume its tail.
fn index_entries(data: &[u8], at: usize, seed: Option<u32>) -> Vec<(u32, u32)> {
    let count = usize::from(le16(data, at + 2));
    let limit = usize::from(le16(data, at));
    assert_eq!(
        limit,
        (data.len() - at - if seed.is_some() { 8 } else { 0 }) / 8
    );
    assert!(count >= 1 && count <= limit);
    if let Some(seed) = seed {
        let (tail, crc) = dx_tail(data, at, seed);
        assert_eq!(le32(data, tail + 4), crc, "index tail");
    }
    let entries: Vec<(u32, u32)> = (0..count)
        .map(|n| {
            let hash = if n == 0 { 0 } else { le32(data, at + n * 8) };
            (hash, le32(data, at + n * 8 + 4))
        })
        .collect();
    assert!(
        entries.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "index out of order"
    );
    entries
}

/// One leaf, checked: its tail on a checksummed volume, its records within
/// its data area (`end`), and each name sorted into `names` when the leaf's
/// range `[lower, upper)` covers its hash, else into `strays`.
fn check_leaf_names(
    data: &[u8],
    end: usize,
    seed: Option<u32>,
    hash: &NameHash,
    (lower, upper): (u32, Option<u32>),
    (names, strays): (&mut BTreeSet<Vec<u8>>, &mut BTreeSet<Vec<u8>>),
) {
    let bs = data.len();
    if let Some(seed) = seed {
        assert_eq!(
            le32(data, bs - 4),
            tairix_crc32c::update(seed, &data[..bs - 12]),
            "leaf tail"
        );
        assert_eq!(le16(data, bs - 8), 12);
    }
    let mut pos = 0;
    while pos < end {
        let rec_len = usize::from(le16(data, pos + 4));
        assert!(
            rec_len >= 12 && pos + rec_len <= end,
            "record past the data area"
        );
        if le32(data, pos) != 0 {
            let name = data[pos + 8..pos + 8 + usize::from(data[pos + 6])].to_vec();
            let h = hash.of(&name).expect("short");
            if h >= lower & !CONTINUED && upper.is_none_or(|upper| h < upper) {
                assert!(names.insert(name.clone()), "{name:?} twice");
            } else {
                strays.insert(name);
            }
        }
        pos += rec_len;
    }
}

/// Check directory `ino`'s index from the format alone: each index block's
/// shape, order and tail, each leaf named once and sealed, and each name in
/// a leaf whose range covers it.
fn check_tree<B: Block>(fs: &mut Ext4<B>, ino: u32) -> Tree {
    let bs = fs.layout.block_size as usize;
    let dir = fs.read_inode(ino).expect("inode");
    let seed = fs.dir_seed(ino, &dir);
    let blocks = dir.size / u64::from(fs.layout.block_size);
    let block = |fs: &mut Ext4<B>, logical: u32| -> Vec<u8> {
        assert!(
            u64::from(logical) < blocks,
            "block {logical} outside the directory"
        );
        let phys = fs
            .map_block(&dir, u64::from(logical))
            .expect("map")
            .expect("mapped");
        let mut data = vec![0u8; bs];
        fs.read_fs_block(phys, &mut data).expect("read");
        data
    };
    let root = block(fs, 0);
    let levels = root[30];
    let hash = NameHash::new(root[28], fs.layout.hash_signed, fs.layout.hash_seed).expect("hash");
    let top = index_entries(&root, 32, seed);
    // Each leaf with the bounds its index entries give it.
    let mut leaves: Vec<(u32, u32, Option<u32>)> = Vec::new();
    let mut nodes = 0;
    for (n, &(lower, child)) in top.iter().enumerate() {
        let upper = top.get(n + 1).map(|entry| entry.0);
        if levels == 0 {
            leaves.push((child, lower, upper));
            continue;
        }
        nodes += 1;
        let node = block(fs, child);
        assert_eq!((le32(&node, 0), usize::from(le16(&node, 4))), (0, bs));
        let entries = index_entries(&node, 8, seed);
        for (m, &(hash_m, leaf)) in entries.iter().enumerate() {
            let below = if m == 0 { lower } else { hash_m };
            let above = entries.get(m + 1).map(|entry| entry.0).or(upper);
            leaves.push((leaf, below, above));
        }
    }
    let end = fs.dir_data_end();
    let (mut names, mut strays) = (BTreeSet::new(), BTreeSet::new());
    let mut visited = BTreeSet::new();
    for &(logical, lower, upper) in &leaves {
        assert!(
            logical > 0 && visited.insert(logical),
            "leaf {logical} named twice"
        );
        let data = block(fs, logical);
        check_leaf_names(
            &data,
            end,
            seed,
            &hash,
            (lower, upper),
            (&mut names, &mut strays),
        );
    }
    assert_eq!(
        blocks,
        1 + nodes + leaves.len() as u64,
        "every block accounted for"
    );
    Tree {
        levels,
        root_count: top.len(),
        leaves: leaves.len(),
        names,
        strays,
    }
}

#[test]
fn names_split_leaves_and_each_is_found_and_listed_once() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"big");
    let added = names(0, 400, 60);
    add(&mut fs, ino, target, &added);
    let tree = check_tree(&mut fs, ino);
    let want: BTreeSet<Vec<u8>> = added.iter().cloned().collect();
    assert_eq!(tree.names, want);
    assert!(tree.strays.is_empty());
    assert!(tree.root_count > 10, "the leaf split many times");
    for name in &added {
        assert_eq!(
            fs.lookup(dir, name),
            Ok(NodeId::from_raw(u64::from(target)))
        );
    }
    assert_eq!(fs.lookup(dir, b"absent"), Err(DriverError::NotFound));
    let listed = list(&mut fs, dir, 7);
    assert_eq!(listed.len(), added.len(), "no name twice");
    assert_eq!(listed.into_iter().collect::<BTreeSet<_>>(), want);
    // And the same after a remount, with nothing cached.
    let mut fs = Ext4::open(fs.into_block()).expect("remount");
    assert_eq!(check_tree(&mut fs, ino).names, want);
}

/// A listing runs in `(hash, name)` order, so a split moving names between
/// blocks is invisible to it.
#[test]
fn a_listing_is_in_hash_order() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"ordered");
    add(&mut fs, ino, target, &names(0, 300, 40));
    let hash = NameHash::new(HALF_MD4, fs.layout.hash_signed, fs.layout.hash_seed).expect("hash");
    let listed = list(&mut fs, dir, 1000);
    let keys: Vec<(u32, Vec<u8>)> = listed
        .iter()
        .map(|name| (hash.of(name).expect("short"), name.clone()))
        .collect();
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
}

/// Long names fill leaves fast: the root fills and moves down a level, then
/// the node under it fills and splits.
#[test]
fn a_full_root_moves_down_a_level_then_a_full_node_splits() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"deep");
    let mut added = Vec::new();
    let mut tree = check_tree(&mut fs, ino);
    while tree.levels == 0 || tree.root_count < 3 {
        assert!(
            added.len() < 1000,
            "the index grew a level and split a node by now"
        );
        let more = long_names(added.len(), 50, 200);
        add(&mut fs, ino, target, &more);
        added.extend(more);
        tree = check_tree(&mut fs, ino);
    }
    assert_eq!(tree.levels, 1);
    assert!(
        tree.leaves > Level::Root.limit(1024, false),
        "more leaves than one root names"
    );
    assert_eq!(tree.names, added.iter().cloned().collect());
    for name in &added {
        assert!(fs.lookup(dir, name).is_ok());
    }
    assert_eq!(list(&mut fs, dir, 13).len(), added.len());
}

/// Names added between batches split leaves under the listing, which still
/// hands over every name present throughout exactly once.
#[test]
fn a_listing_resumed_across_splits_lists_each_lasting_name_once() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"busy");
    let lasting = names(0, 150, 50);
    add(&mut fs, ino, target, &lasting);
    let mut next = 1000;
    let listed = list_with(&mut fs, dir, 4, |fs| {
        add(fs, ino, target, &names(next, 12, 50));
        next += 12;
    });
    let mut counts: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    for (name, _) in &listed {
        *counts.entry(name.clone()).or_default() += 1;
    }
    assert!(counts.values().all(|&count| count == 1), "no name twice");
    for name in &lasting {
        assert_eq!(counts.get(name), Some(&1), "{name:?}");
    }
    assert!(next > 1000 + 12 * 20, "names were added throughout");
}

/// A cursor is never the start and names the place after its own entry.
#[test]
fn an_indexed_cursor_is_never_the_start_and_differs_per_name() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"cursors");
    add(&mut fs, ino, target, &names(0, 100, 30));
    let listed = list_with(&mut fs, dir, 1000, |_| {});
    let cursors: BTreeSet<u64> = listed.iter().map(|(_, cursor)| *cursor).collect();
    assert_eq!(cursors.len(), listed.len());
    assert!(!cursors.contains(&0));
}

#[test]
fn removed_names_are_gone_and_an_emptied_directory_can_be_removed() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"shrinking");
    let added = names(0, 200, 40);
    add(&mut fs, ino, target, &added);
    for name in added.iter().step_by(2) {
        assert_eq!(fs.remove_dirent(ino, name), Ok(target));
    }
    for (n, name) in added.iter().enumerate() {
        assert_eq!(fs.lookup(dir, name).is_ok(), n % 2 == 1, "{name:?}");
    }
    assert_eq!(list(&mut fs, dir, 9).len(), added.len() / 2);
    assert_eq!(fs.dir_is_empty(ino), Ok(false));
    for name in added.iter().skip(1).step_by(2) {
        assert_eq!(fs.remove_dirent(ino, name), Ok(target));
    }
    assert_eq!(fs.remove_dirent(ino, &added[1]), Err(DriverError::NotFound));
    assert_eq!(fs.dir_is_empty(ino), Ok(true));
    assert!(list(&mut fs, dir, 9).is_empty());
    let root = fs.root();
    fs.remove(root, b"shrinking").expect("rmdir");
    assert_eq!(fs.lookup(root, b"shrinking"), Err(DriverError::NotFound));
}

/// Files made and removed through the write surface keep their inodes
/// honest in an indexed directory.
#[test]
fn files_come_and_go_in_an_indexed_directory() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, _, _) = indexed_dir(&mut fs, b"files");
    let before = fs.stats().expect("stats");
    let made = names(0, 120, 30);
    for name in &made {
        fs.create(dir, name, NodeKind::RegularFile).expect("create");
    }
    assert_eq!(
        fs.create(dir, &made[7], NodeKind::RegularFile),
        Err(DriverError::AlreadyExists)
    );
    fs.create(dir, b"sub", NodeKind::Directory).expect("mkdir");
    fs.rename(dir, &made[3], dir, b"renamed").expect("rename");
    assert!(fs.lookup(dir, b"renamed").is_ok());
    assert_eq!(fs.lookup(dir, &made[3]), Err(DriverError::NotFound));
    for name in made.iter().filter(|name| *name != &made[3]) {
        fs.remove(dir, name).expect("remove");
    }
    fs.remove(dir, b"renamed").expect("remove");
    fs.remove(dir, b"sub").expect("rmdir");
    let after = fs.stats().expect("stats");
    assert_eq!(after.files_free, before.files_free, "every inode went back");
}

/// Two names of one hash whose run spans two leaves are both found, and
/// listed once each in name order, a resumed listing included.
#[test]
fn a_run_of_equal_hashes_is_found_and_listed_across_its_leaves() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"run");
    let hash = NameHash::new(HALF_MD4, fs.layout.hash_signed, fs.layout.hash_seed).expect("hash");
    let mut seen: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let (first, second) = (0u32..)
        .find_map(|n| {
            let name = format!("k{n}").into_bytes();
            let h = hash.of(&name).expect("short");
            match seen.insert(h, name.clone()) {
                Some(other) if other < name => Some((other, name)),
                Some(other) => Some((name, other)),
                None => None,
            }
        })
        .expect("a collision");
    let shared = hash.of(&first).expect("short");
    let below: Vec<Vec<u8>> = seen
        .range(..shared)
        .take(3)
        .map(|(_, name)| name.clone())
        .collect();
    let above: Vec<Vec<u8>> = seen
        .range(shared + 2..)
        .take(3)
        .map(|(_, name)| name.clone())
        .collect();

    // Leaf 1 holds the hashes below the run and its first name; a second
    // leaf, whose entry continues the run, holds its second and those above.
    let bs = fs.layout.block_size as usize;
    let end = fs.dir_data_end();
    let lay = |fs: &mut Ext4<SizedBlock>, entries: &[&Vec<u8>]| -> Vec<u8> {
        let mut leaf = vec![0u8; bs];
        let mut pos = 0;
        for (n, name) in entries.iter().enumerate() {
            let size = crate::align4(8 + name.len());
            let rec_len = if n + 1 == entries.len() {
                end - pos
            } else {
                size
            };
            fs.write_dirent(&mut leaf, pos, target, small(rec_len), name, FT_REG)
                .expect("lay");
            pos += size;
        }
        leaf
    };
    let mut one: Vec<&Vec<u8>> = below.iter().collect();
    one.push(&first);
    let mut two: Vec<&Vec<u8>> = vec![&second];
    two.extend(above.iter());
    let leaf_one = lay(&mut fs, &one);
    let leaf_two = lay(&mut fs, &two);
    let dir_inode = fs.read_inode(ino).expect("inode");
    let phys_one = fs.map_block(&dir_inode, 1).expect("map").expect("leaf");
    fs.write_fs_block(phys_one, &leaf_one).expect("write");
    let mut blocks = 2;
    fs.append_dir_block(ino, &mut blocks, &leaf_two)
        .expect("grow");
    let phys_root = fs.map_block(&dir_inode, 0).expect("map").expect("root");
    let mut root = vec![0u8; bs];
    fs.read_fs_block(phys_root, &mut root).expect("read");
    put_le16(&mut root, 34, 2);
    put_le32(&mut root, 40, shared | CONTINUED);
    put_le32(&mut root, 44, 2);
    fs.write_fs_block(phys_root, &root).expect("write");

    let tree = check_tree(&mut fs, ino);
    assert!(tree.strays.is_empty());
    for name in one.iter().chain(two.iter()) {
        assert_eq!(
            fs.lookup(dir, name),
            Ok(NodeId::from_raw(u64::from(target))),
            "{name:?}"
        );
    }
    let mut want: Vec<(u32, Vec<u8>)> = one
        .iter()
        .chain(two.iter())
        .map(|name| (hash.of(name).expect("short"), (*name).clone()))
        .collect();
    want.sort();
    let want: Vec<Vec<u8>> = want.into_iter().map(|(_, name)| name).collect();
    for batch in [1, 2, 3, 100] {
        assert_eq!(list(&mut fs, dir, batch), want, "batches of {batch}");
    }
}

/// Synthetic leaves, `(hash, record bytes)` a slot.
fn slots(entries: &[(u32, usize)]) -> Vec<Slot> {
    entries
        .iter()
        .map(|&(hash, size)| Slot {
            hash,
            at: 0,
            len: u8::try_from(size - 8).expect("small"),
        })
        .collect()
}

#[test]
fn a_cut_keeps_equal_hashes_together_when_it_can_and_balances_the_halves() {
    // Even halves would part the two 0x40s; keeping them together costs a
    // little balance.
    let leaf = slots(&[
        (0x10, 200),
        (0x20, 200),
        (0x40, 200),
        (0x40, 200),
        (0x60, 200),
    ]);
    let cut = choose_cut(&leaf, 0, 0x08, 12, 1012).expect("a cut");
    assert!(!cut.continued);
    assert!(cut.hash == 0x40 || cut.hash == 0x60);
    // Every name one hash: only a continued cut exists, taken mid-way.
    let same = slots(&[(0x40, 200); 5]);
    let cut = choose_cut(&same, 0, 0x40, 200, 1012).expect("a cut");
    assert!(cut.continued && cut.hash == 0x40);
    assert!((2..=3).contains(&cut.index));
    // Entries pinned in the old leaf count against its half: the even cut
    // would overfill it, so the cut moves down.
    let leaf = slots(&[(0x10, 260), (0x20, 260), (0x30, 260), (0x40, 220)]);
    assert_eq!(
        choose_cut(&leaf, 0, 0x05, 12, 1012).expect("a cut").index,
        2
    );
    assert_eq!(
        choose_cut(&leaf, 600, 0x05, 12, 1012).expect("a cut").index,
        1
    );
    // A leaf of one entry cannot be cut at all.
    assert!(choose_cut(&slots(&[(0x10, 12)]), 0, 0x10, 12, 1012).is_none());
}

/// A split stopped after its index was written leaves the moved names in the
/// old leaf too, outside its range: readers pass over them, and a later
/// split keeps them where they are.
#[test]
fn names_left_outside_a_leafs_range_are_passed_over() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"torn");
    let added = names(0, 60, 40);
    add(&mut fs, ino, target, &added);
    let hash = NameHash::new(HALF_MD4, fs.layout.hash_signed, fs.layout.hash_seed).expect("hash");
    let bs = fs.layout.block_size as usize;
    let dir_inode = fs.read_inode(ino).expect("inode");
    let mut root = vec![0u8; bs];
    let root_phys = fs.map_block(&dir_inode, 0).expect("map").expect("root");
    fs.read_fs_block(root_phys, &mut root).expect("read");
    assert!(le16(&root, 34) >= 2, "a split happened");
    // A name the second leaf's range covers, copied into the first leaf.
    let boundary = le32(&root, 40) & !CONTINUED;
    let beyond = (le16(&root, 34) > 2).then(|| le32(&root, 48));
    let stray = added
        .iter()
        .find(|name| {
            let h = hash.of(name).expect("short");
            h >= boundary && beyond.is_none_or(|upper| h < upper)
        })
        .expect("a name past the boundary")
        .clone();
    let first_leaf = fs
        .map_block(&dir_inode, u64::from(le32(&root, 36)))
        .expect("map")
        .expect("leaf");
    let mut leaf = vec![0u8; bs];
    fs.read_fs_block(first_leaf, &mut leaf).expect("read");
    let needed = crate::align4(8 + stray.len());
    let end = fs.dir_data_end();
    assert!(fs
        .place_in_block(&mut leaf[..end], needed, target, &stray, FT_REG)
        .expect("place"));
    fs.write_fs_block(first_leaf, &leaf).expect("write");

    assert!(check_tree(&mut fs, ino).strays.contains(&stray));
    let listed = list(&mut fs, dir, 5);
    assert_eq!(listed.iter().filter(|name| **name == stray).count(), 1);
    assert_eq!(listed.len(), added.len());
    assert!(fs.lookup(dir, &stray).is_ok());
    // Names bound for the first leaf still go in, splitting it around the
    // stray, which stays outside every range rather than being lost.
    let more: Vec<Vec<u8>> = names(5000, 400, 40)
        .into_iter()
        .filter(|name| hash.of(name).expect("short") < boundary)
        .take(60)
        .collect();
    add(&mut fs, ino, target, &more);
    let tree = check_tree(&mut fs, ino);
    assert!(tree.strays.contains(&stray));
    assert_eq!(list(&mut fs, dir, 5).len(), added.len() + more.len());
}

/// A flagged directory whose block 0 is no root keeps its leaves readable
/// but takes no new name, refused before anything changes.
#[test]
fn a_damaged_index_refuses_new_names_and_loses_nothing() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let root = fs.root();
    let dir = fs
        .create(root, b"damaged", NodeKind::Directory)
        .expect("mkdir");
    fs.create(dir, b"keep.txt", NodeKind::RegularFile)
        .expect("create");
    fs.create(root, b"hello.txt", NodeKind::RegularFile)
        .expect("create");
    let ino = node_inode(dir).expect("inode");
    set_index_flag(&mut fs, ino, true);
    let before = fs.stats().expect("stats");

    assert_eq!(
        fs.create(dir, b"new.txt", NodeKind::RegularFile),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        fs.rename(root, b"hello.txt", dir, b"moved.txt"),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        fs.rename(root, b"hello.txt", dir, b"keep.txt"),
        Err(DriverError::DeviceFault),
        "an existing destination is not dropped first"
    );
    assert!(fs.lookup(dir, b"keep.txt").is_ok());
    assert!(fs.lookup(root, b"hello.txt").is_ok());
    assert_eq!(list(&mut fs, dir, 4), vec![b"keep.txt".to_vec()]);
    assert_eq!(fs.stats().expect("stats"), before, "nothing was allocated");

    fs.remove(dir, b"keep.txt").expect("a name still leaves");
    assert_eq!(fs.lookup(dir, b"keep.txt"), Err(DriverError::NotFound));
}

/// A superblock naming no signedness leaves an index's hashes unknowable:
/// names are found and listed linearly, and none is added.
#[test]
fn an_index_on_a_volume_naming_no_signedness_takes_no_new_name() {
    let mut fs = volume(SB_FLAG_SIGNED_HASH);
    let (dir, ino, target) = indexed_dir(&mut fs, b"legacy");
    let added = names(0, 80, 30);
    add(&mut fs, ino, target, &added);
    let mut device = fs.into_block();
    let sb = usize::try_from(SUPERBLOCK_OFFSET).expect("small");
    put_le32(&mut device.data, sb + SB_FLAGS_OFFSET, 0);
    let mut fs = Ext4::open(device).expect("remount");
    assert_eq!(
        fs.create(dir, b"another", NodeKind::RegularFile),
        Err(DriverError::Unsupported)
    );
    for name in &added {
        assert!(fs.lookup(dir, name).is_ok());
    }
    assert_eq!(list(&mut fs, dir, 6).len(), added.len());
    assert_eq!(fs.remove_dirent(ino, &added[0]), Ok(target));
    assert_eq!(fs.lookup(dir, &added[0]), Err(DriverError::NotFound));
}

/// Without `dir_index` the flag means nothing: the directory is linear, and
/// the stale flag goes when a name is added.
#[test]
fn without_dir_index_a_flagged_directory_is_linear_and_loses_its_flag() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let root = fs.root();
    let dir = fs
        .create(root, b"plain", NodeKind::Directory)
        .expect("mkdir");
    let ino = node_inode(dir).expect("inode");
    set_index_flag(&mut fs, ino, true);
    fs.create(dir, b"file", NodeKind::RegularFile)
        .expect("create");
    assert_eq!(
        fs.read_inode(ino).expect("inode").flags & INODE_FLAG_INDEX,
        0
    );
    assert!(fs.lookup(dir, b"file").is_ok());
}

/// On a checksummed volume every block a split writes is sealed — the root
/// and leaves alike — and a block whose checksum fails is never read past.
#[test]
fn on_a_checksummed_volume_every_index_and_leaf_is_sealed() {
    let mut fs = checksummed();
    let (dir, ino, target) = indexed_dir(&mut fs, b"sealed");
    let added = long_names(0, 40, 100);
    add(&mut fs, ino, target, &added);
    let tree = check_tree(&mut fs, ino);
    assert!(tree.root_count > 3);
    assert_eq!(tree.names, added.iter().cloned().collect());
    for name in added.iter().step_by(3) {
        assert_eq!(fs.remove_dirent(ino, name), Ok(target));
    }
    check_tree(&mut fs, ino);
    assert_eq!(
        list(&mut fs, dir, 4).len(),
        added.len() - added.len().div_ceil(3)
    );

    // A leaf changed under its checksum fails closed.
    let bs = fs.layout.block_size as usize;
    let dir_inode = fs.read_inode(ino).expect("inode");
    let leaf = fs.map_block(&dir_inode, 1).expect("map").expect("leaf");
    let mut data = vec![0u8; bs];
    fs.read_fs_block(leaf, &mut data).expect("read");
    data[20] ^= 1;
    fs.write_fs_block(leaf, &data).expect("write");
    assert_eq!(
        fs.read_dir(dir, 0, &[], &mut |_, _| DirVisit::Take),
        Err(DriverError::DeviceFault)
    );
    data[20] ^= 1;
    fs.write_fs_block(leaf, &data).expect("restore");

    // A root changed under its checksum fails closed too, rather than
    // being read linearly.
    let root = fs.map_block(&dir_inode, 0).expect("map").expect("root");
    fs.read_fs_block(root, &mut data).expect("read");
    data[36] ^= 0x80;
    fs.write_fs_block(root, &data).expect("write");
    assert_eq!(fs.lookup(dir, &added[1]), Err(DriverError::DeviceFault));
}

/// Moving an indexed directory to a new parent rewrites the `..` in its root
/// and reseals the root's tail.
#[test]
fn moving_an_indexed_directory_reseals_its_root() {
    let mut fs = checksummed();
    let (dir, ino, target) = indexed_dir(&mut fs, b"mover");
    add(&mut fs, ino, target, &names(0, 20, 60));
    let root = fs.root();
    let parent = fs
        .create(root, b"home", NodeKind::Directory)
        .expect("mkdir");
    fs.rename(root, b"mover", parent, b"mover").expect("move");
    let parent_ino = node_inode(parent).expect("inode");
    assert_eq!(fs.dir_parent_ino(ino), Ok(parent_ino));
    check_tree(&mut fs, ino);
    assert_eq!(fs.lookup(parent, b"mover"), Ok(dir));
    assert_eq!(list(&mut fs, dir, 3).len(), 20);
}

/// A root sealed by the driver checks as the format defines it.
#[test]
fn the_driver_seals_an_index_tail_as_the_format_defines_it() {
    let mut fs = checksummed();
    let (_, ino, target) = indexed_dir(&mut fs, b"tails");
    add(&mut fs, ino, target, &long_names(0, 12, 200));
    let bs = fs.layout.block_size as usize;
    let dir_inode = fs.read_inode(ino).expect("inode");
    let seed = fs.dir_seed(ino, &dir_inode).expect("checksummed");
    let root = fs.map_block(&dir_inode, 0).expect("map").expect("root");
    let mut data = vec![0u8; bs];
    fs.read_fs_block(root, &mut data).expect("read");
    let (tail, crc) = dx_tail(&data, 32, seed);
    assert_eq!(le32(&data, tail + 4), crc);
    assert!(htree::verify(&data, Level::Root, seed));
}

/// The deepest path a directory takes — an indexed insert that packs or
/// splits a leaf, grows the index a level and allocates for it — runs within
/// a stack the size of the kernel's, which an operation holding its block
/// buffers on the stack overflows.
#[test]
fn indexed_growth_fits_a_kernel_sized_stack() {
    std::thread::Builder::new()
        .stack_size(32 * 1024)
        .spawn(|| {
            let mut fs = volume(SB_FLAG_SIGNED_HASH);
            let (dir, ino, target) = indexed_dir(&mut fs, b"stacked");
            let mut added = 0;
            while check_tree(&mut fs, ino).levels == 0 {
                add(&mut fs, ino, target, &long_names(added, 40, 200));
                added += 40;
            }
            fs.create(dir, b"one-more", NodeKind::RegularFile)
                .expect("create");
            assert!(fs.lookup(dir, b"one-more").is_ok());
            assert_eq!(list(&mut fs, dir, 64).len(), added + 1);
            fs.remove(dir, b"one-more").expect("remove");
        })
        .expect("spawn")
        .join()
        .expect("ran within the stack");
}
