// Test volumes are a few MiB, so every size fits the narrower field it is
// compared with.
#![allow(clippy::cast_possible_truncation)]

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::listing::listed;
use tairix_abi::driver::filesystem::{
    AclModel, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, NodeId,
    NodeKind, NodeSecurity, SecurityAcl, SecuritySubject, Unpinned, WriteRefusal,
};
use tairix_abi::fs::SeekRegion;
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError};

use tairix_writeback::TestVolumeHost;

use super::{FormatOptions, Udf};

#[path = "../../ext4/tests/support/sparse.rs"]
mod sparse;

// The records nodes are created with where a test is not about them.
const FILE: NodeSecurity = NodeSecurity::new(0o644, 0, 0);
const DIR: NodeSecurity = NodeSecurity::new(0o755, 0, 0);
const LINK: NodeSecurity = NodeSecurity::new(0o777, 0, 0);

/// Every request this binary makes, metered per thread, so a test can have the
/// heap refuse a hold as an exhausted one would.
#[global_allocator]
static ALLOC: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

/// An in-memory disk that counts the requests it serves.
struct Disk {
    block_size: u32,
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
}

impl Disk {
    fn new(block_size: u32, bytes: Vec<u8>) -> Self {
        Self {
            block_size,
            bytes,
            reads: 0,
            writes: 0,
        }
    }
}

impl Block for Disk {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: (self.bytes.len() / self.block_size as usize) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.reads += 1;
        let at = lba as usize * self.block_size as usize;
        buf.copy_from_slice(
            self.bytes
                .get(at..at + buf.len())
                .ok_or(DriverError::OutOfRange)?,
        );
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.writes += 1;
        let at = lba as usize * self.block_size as usize;
        self.bytes
            .get_mut(at..at + buf.len())
            .ok_or(DriverError::OutOfRange)?
            .copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn options(revision: u16) -> FormatOptions<'static> {
    FormatOptions {
        label: "Archive",
        revision,
        serial: [1, 2, 3, 4, 5, 6, 7, 8],
        now: Time64::from_secs(1_700_000_000),
    }
}

fn formatted(block_size: u32, revision: u16) -> Udf<Disk> {
    Udf::format(
        Disk::new(block_size, vec![0u8; 8 << 20]),
        &options(revision),
    )
    .expect("formats")
}

fn remount(fs: Udf<Disk>) -> Udf<Disk> {
    Udf::open(fs.into_device()).expect("mounts")
}

fn names(fs: &mut Udf<Disk>, dir: NodeId) -> Vec<Vec<u8>> {
    listed(fs, dir, 0, &[])
        .expect("lists")
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

fn read_all(fs: &mut Udf<Disk>, node: NodeId) -> Vec<u8> {
    let size = fs.node_info(node).expect("info").size as usize;
    let mut out = vec![0u8; size];
    let mut done = 0;
    while done < size {
        let end = (done + 1500).min(size);
        let n = fs
            .read_at(node, done as u64, &mut out[done..end])
            .expect("reads");
        assert_ne!(n, 0);
        done += n;
    }
    out
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed))
        .collect()
}

#[test]
fn a_formatted_volume_mounts_empty_writable_and_recognisable() {
    for (block_size, revision) in [(512, 0x0201), (2048, 0x0150), (4096, 0x0102)] {
        let mut fs = formatted(block_size, revision);
        assert_eq!(fs.write_refusal(), None);
        let root = fs.root();
        assert!(names(&mut fs, root).is_empty());
        assert_eq!(fs.node_info(root).expect("root").kind, NodeKind::Directory);
        let stats = fs.stats().expect("stats");
        assert_eq!(stats.block_size, block_size);
        assert!(stats.free_blocks > 0 && stats.free_blocks < stats.total_blocks);
        let mut dev = tairix_bytedev::ByteDevice::new(fs.into_device()).expect("geometry");
        let Ok(Some(tairix_fsprobe::Recognised::Volume(volume))) =
            tairix_fsprobe::recognise(&mut dev)
        else {
            panic!("the probe recognises what the formatter laid out");
        };
        assert_eq!(volume.fstype, tairix_abi::volume::VolumeFsType::Udf);
        assert_eq!(volume.label(), b"Archive");
        assert!(
            volume.evidence.is_some(),
            "the integrity sequence is its evidence"
        );
    }
}

#[test]
fn files_and_directories_persist_across_a_remount() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let docs = fs
        .create(root, b"Documents", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let note = fs
        .create(
            docs,
            "Notes – café.txt".as_bytes(),
            NodeKind::RegularFile,
            &FILE,
        )
        .expect("create");
    let body = pattern(10_000, 3);
    fs.write_all(note, 0, &body).expect("writes");
    fs.create(root, b"empty", NodeKind::RegularFile, &FILE)
        .expect("create");
    assert_eq!(read_all(&mut fs, note), body);
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"Documents"[..], b"empty"]);
    let docs = fs.lookup(root, b"Documents").expect("found");
    assert_eq!(fs.node_info(docs).expect("info").nlink, 2);
    let note = fs
        .lookup(docs, "Notes – café.txt".as_bytes())
        .expect("found");
    assert_eq!(read_all(&mut fs, note), body);
    let root_links = fs.node_info(root).expect("root").nlink;
    assert_eq!(
        root_links, 3,
        "the root's own and its one child's parent record"
    );
    assert_eq!(
        fs.create(root, b"empty", NodeKind::RegularFile, &FILE),
        Err(DriverError::AlreadyExists)
    );
}

#[test]
fn a_heavily_fragmented_file_spills_into_allocation_extent_descriptors() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let a = fs
        .create(root, b"a", NodeKind::RegularFile, &FILE)
        .expect("create");
    let b = fs
        .create(root, b"b", NodeKind::RegularFile, &FILE)
        .expect("create");
    let mut expected = Vec::new();
    // Interleaving the two files' growth gives each one extent per block.
    for i in 0..120u8 {
        let block = vec![i; 512];
        fs.write_all(a, u64::from(i) * 512, &block)
            .expect("a grows");
        fs.write_all(b, u64::from(i) * 512, &[!i; 512])
            .expect("b grows");
        expected.extend_from_slice(&block);
    }
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    let a = fs.lookup(root, b"a").expect("found");
    assert_eq!(read_all(&mut fs, a), expected);
    // Removing both gives every block back.
    let before = fs.stats().expect("stats").free_blocks;
    fs.remove(root, b"a").expect("remove");
    fs.remove(root, b"b").expect("remove");
    fs.flush().expect("flush");
    let after = fs.stats().expect("stats").free_blocks;
    assert!(after >= before + 240, "{before} -> {after}");
}

#[test]
fn hard_links_share_a_node_and_its_count() {
    let mut fs = formatted(2048, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"original", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(file, 0, b"shared").expect("write");
    fs.link(root, b"second", file).expect("link");
    assert_eq!(fs.lookup(root, b"second"), Ok(file));
    assert_eq!(fs.node_info(file).expect("info").nlink, 2);
    fs.remove(root, b"original").expect("unlink one");
    assert_eq!(read_all(&mut fs, file), b"shared");
    assert_eq!(fs.node_info(file).expect("info").nlink, 1);
    let dir = fs
        .create(root, b"dir", NodeKind::Directory, &DIR)
        .expect("mkdir");
    assert_eq!(
        fs.link(root, b"dir-again", dir),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn symbolic_links_record_their_path_components() {
    let mut fs = formatted(2048, 0x0201);
    let root = fs.root();
    for target in [&b"/usr/share/../bin/sh"[..], b"../relative/./x", b"name"] {
        let name = alloc::format!("link{}", target.len());
        let link = fs
            .create_link(root, name.as_bytes(), target, &LINK)
            .expect("symlink");
        assert_eq!(fs.node_info(link).expect("info").kind, NodeKind::Symlink);
        let mut out = [0u8; 64];
        let len = fs.read_link(link, &mut out).expect("a link");
        assert_eq!(&out[..len], target);
    }
}

#[test]
fn truncation_and_punching_leave_holes_that_read_as_zeros() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"sparse", NodeKind::RegularFile, &FILE)
        .expect("create");
    let body = pattern(8 * 512, 9);
    fs.write_all(file, 0, &body).expect("write");
    fs.punch(file, 1024 + 100, 2048).expect("punch");
    let mut expected = body.clone();
    expected[1124..3172].fill(0);
    assert_eq!(read_all(&mut fs, file), expected);
    assert_eq!(fs.seek_region(file, 0, SeekRegion::Hole), Ok(Some(1536)));
    assert_eq!(fs.seek_region(file, 1536, SeekRegion::Data), Ok(Some(3072)));
    fs.truncate(file, 10_000).expect("grow");
    assert_eq!(fs.seek_region(file, 4096, SeekRegion::Hole), Ok(Some(4096)));
    expected.resize(10_000, 0);
    assert_eq!(read_all(&mut fs, file), expected);
    fs.truncate(file, 700).expect("shrink");
    fs.truncate(file, 900).expect("grow a little");
    let mut tail = expected[..700].to_vec();
    tail.resize(900, 0);
    assert_eq!(
        read_all(&mut fs, file),
        tail,
        "bytes past a shrink come back as zeros"
    );
    fs.write_all(file, 50_000, b"far")
        .expect("write past the end");
    assert_eq!(fs.seek_region(file, 900, SeekRegion::Data), Ok(Some(900)));
    assert_eq!(
        fs.seek_region(file, 1024, SeekRegion::Data),
        Ok(Some(50_000 - 50_000 % 512))
    );
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    let file = fs.lookup(root, b"sparse").expect("found");
    let all = read_all(&mut fs, file);
    assert_eq!(&all[50_000..], b"far");
    assert!(all[1024..50_000].iter().all(|&b| b == 0));
}

#[test]
fn removal_and_renaming_keep_directories_consistent() {
    let mut fs = formatted(2048, 0x0150);
    let root = fs.root();
    let a = fs
        .create(root, b"a", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let b = fs
        .create(root, b"b", NodeKind::Directory, &DIR)
        .expect("mkdir");
    fs.create(a, b"inner", NodeKind::RegularFile, &FILE)
        .expect("create");
    assert_eq!(fs.remove(root, b"a"), Err(DriverError::DirectoryNotEmpty));
    fs.rename(root, b"a", b, b"moved")
        .expect("move a directory");
    assert_eq!(names(&mut fs, root), [&b"b"[..]]);
    let moved = fs.lookup(b, b"moved").expect("found");
    assert_eq!(moved, a);
    assert_eq!(
        fs.rename(root, b"b", moved, b"loop"),
        Err(DriverError::DirectoryCycle)
    );
    fs.rename(moved, b"inner", root, b"outer")
        .expect("move a file");
    fs.create(root, b"target", NodeKind::RegularFile, &FILE)
        .expect("create");
    let outer = fs.lookup(root, b"outer").expect("found");
    fs.write_all(outer, 0, b"replacing").expect("write");
    fs.rename(root, b"outer", root, b"target").expect("replace");
    let target = fs.lookup(root, b"target").expect("found");
    assert_eq!(read_all(&mut fs, target), b"replacing");
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    let b = fs.lookup(root, b"b").expect("found");
    let moved = fs.lookup(b, b"moved").expect("found");
    assert!(names(&mut fs, moved).is_empty());
    assert_eq!(fs.node_info(b).expect("info").nlink, 3);
    fs.remove(b, b"moved").expect("now empty");
    assert_eq!(fs.node_info(b).expect("info").nlink, 2);
    assert_eq!(names(&mut fs, root), [&b"b"[..], b"target"]);
}

/// The number `stat` reports is the node's id, by node as in a listing.
#[test]
fn every_nodes_number_is_its_id() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    fs.create(root, b"dir", NodeKind::Directory, &DIR)
        .expect("mkdir");
    fs.create(root, b"file", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.create_link(root, b"link", b"file", &LINK)
        .expect("symlink");
    assert_eq!(fs.node_info(root).expect("info").ino, root.raw());
    for (entry, _) in listed(&mut fs, root, 0, &[]).expect("lists") {
        assert_eq!(entry.info.ino, entry.node.raw());
        assert_eq!(
            fs.node_info(entry.node).expect("info").ino,
            entry.node.raw()
        );
    }
}

/// A directory's parent is the one its parent descriptor names, the root's
/// is the root, and a removed directory, whose entry's block is freed, has
/// none.
#[test]
fn a_directorys_parent_is_the_one_its_parent_descriptor_names() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    assert_eq!(fs.parent(root), Ok(root));
    let outer = fs
        .create(root, b"outer", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let inner = fs
        .create(outer, b"inner", NodeKind::Directory, &DIR)
        .expect("mkdir");
    assert_eq!(fs.parent(outer), Ok(root));
    assert_eq!(fs.parent(inner), Ok(outer));
    let other = fs
        .create(root, b"other", NodeKind::Directory, &DIR)
        .expect("mkdir");
    fs.rename(outer, b"inner", other, b"inner").expect("move");
    assert_eq!(fs.parent(inner), Ok(other));
    let file = fs
        .create(other, b"file", NodeKind::RegularFile, &FILE)
        .expect("create");
    assert_eq!(fs.parent(file), Err(DriverError::Unsupported));
    fs.remove(other, b"inner").expect("rmdir");
    assert_eq!(fs.parent(inner), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(inner), Err(DriverError::NotFound));
    assert_eq!(fs.parent(NodeId::NONE), Err(DriverError::NotFound));
}

/// A removal frees a node's entry but leaves its bytes, which would still
/// read as the node: its id names nothing, and a write by it reaches
/// nothing.
#[test]
fn a_removed_files_id_names_nothing() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"gone", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(file, 0, &pattern(2000, 5)).expect("write");
    fs.remove(root, b"gone").expect("remove");
    let before = free(&mut fs);
    assert_eq!(fs.node_info(file), Err(DriverError::NotFound));
    assert_eq!(
        fs.read_at(file, 0, &mut [0u8; 8]),
        Err(DriverError::NotFound)
    );
    assert_eq!(fs.write_all(file, 0, b"stale"), Err(DriverError::NotFound));
    assert_eq!(fs.truncate(file, 0), Err(DriverError::NotFound));
    assert_eq!(fs.punch(file, 0, 512), Err(DriverError::NotFound));
    assert_eq!(fs.security(file), Err(DriverError::NotFound));
    assert_eq!(fs.pin(file), Err(DriverError::NotFound), "nothing to hold");
    assert_eq!(free(&mut fs), before);
    assert_eq!(
        fs.write_refusal(),
        None,
        "refused before anything was written"
    );
}

/// A held file outlives its last name: through the held id it still reads,
/// writes, truncates and takes changes to its record and attributes, it
/// counts no name, and its space comes back only at its last unpin.
#[test]
fn a_held_file_outlives_its_last_name() {
    use tairix_abi::driver::filesystem::FilesystemAttrs;
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let empty = free(&mut fs);
    let file = fs
        .create(root, b"open", NodeKind::RegularFile, &FILE)
        .expect("create");
    let body = pattern(3000, 7);
    fs.write_all(file, 0, &body).expect("write");
    assert_eq!(fs.pin(file), Ok(file), "the node's own id is its pin");
    let named = free(&mut fs);
    fs.remove(root, b"open").expect("remove");
    assert_eq!(free(&mut fs), named, "nothing is freed while it is held");
    assert_eq!(fs.lookup(root, b"open"), Err(DriverError::NotFound));
    assert_eq!(fs.node_info(file).expect("held").nlink, 0);
    assert_eq!(read_all(&mut fs, file), body);
    fs.write_all(file, 3000, b"more").expect("written");
    fs.truncate(file, 3002).expect("truncated");
    let mut expected = body;
    expected.extend_from_slice(b"mo");
    assert_eq!(read_all(&mut fs, file), expected);
    fs.set_security(file, NodeSecurity::new(0o600, 1000, 100))
        .expect("its record changes");
    assert_eq!(fs.security(file).expect("recorded").mode, 0o600);
    fs.set_attr(file, b"mac.type", b"TEXT")
        .expect("its attributes change");
    assert_eq!(
        attr(&mut fs, file, b"mac.type").as_deref(),
        Some(&b"TEXT"[..])
    );
    assert_eq!(fs.unpin(file), Ok(Unpinned::Freed));
    assert_eq!(free(&mut fs), empty, "the last unpin frees it");
    assert_eq!(fs.write_refusal(), None);
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    assert_eq!(free(&mut fs), empty);
}

/// Holds count, each answering the one id, and only a hold taken is
/// released; the last of them frees a node no name reaches.
#[test]
fn every_hold_answers_the_one_id_and_is_released_once() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let kept = fs
        .create(root, b"kept", NodeKind::RegularFile, &FILE)
        .expect("create");
    assert_eq!(fs.unpin(kept), Err(DriverError::NotFound), "never held");
    assert_eq!(fs.pin(kept), Ok(kept));
    assert_eq!(fs.pin(kept), Ok(kept));
    assert_eq!(fs.unpin(kept), Ok(Unpinned::Held));
    assert_eq!(fs.unpin(kept), Ok(Unpinned::Released));
    assert_eq!(fs.unpin(kept), Err(DriverError::NotFound));
    assert_eq!(fs.lookup(root, b"kept"), Ok(kept));
    assert_eq!(fs.pin(root), Ok(root));
    assert_eq!(fs.unpin(root), Ok(Unpinned::Released));
    assert_eq!(fs.pin(NodeId::NONE), Err(DriverError::NotFound));
    let shared = fs
        .create(root, b"shared", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(shared, 0, b"twice held").expect("write");
    fs.pin(shared).expect("pinned");
    fs.pin(shared).expect("pinned");
    fs.remove(root, b"shared").expect("remove");
    assert_eq!(fs.unpin(shared), Ok(Unpinned::Held));
    assert_eq!(read_all(&mut fs, shared), b"twice held");
    assert_eq!(fs.unpin(shared), Ok(Unpinned::Freed));
    assert_eq!(fs.node_info(shared), Err(DriverError::NotFound));
}

/// A hold the heap cannot record is refused, and nothing is held; a further
/// hold, and a release, need no memory.
#[test]
fn a_hold_the_heap_cannot_record_is_refused() {
    use tairix_fuzzseed::meter::refusing_above;
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &FILE)
        .expect("create");
    // Above a block, so the entry is still read and only the hold's own
    // index is refused.
    assert_eq!(
        refusing_above(1024, || fs.pin(file)),
        Err(DriverError::OutOfMemory)
    );
    assert_eq!(fs.unpin(file), Err(DriverError::NotFound), "nothing held");
    assert_eq!(fs.pin(file), Ok(file));
    assert_eq!(refusing_above(0, || fs.pin(file)), Ok(file));
    assert_eq!(refusing_above(0, || fs.unpin(file)), Ok(Unpinned::Held));
    assert_eq!(refusing_above(0, || fs.unpin(file)), Ok(Unpinned::Released));
}

/// A held node with a second name keeps it when the first goes.
#[test]
fn a_held_node_is_unnamed_only_by_its_last_name() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"one", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.link(root, b"two", file).expect("link");
    fs.pin(file).expect("pinned");
    fs.remove(root, b"one").expect("remove a name");
    assert_eq!(fs.node_info(file).expect("named").nlink, 1);
    fs.remove(root, b"two").expect("remove the other");
    assert_eq!(fs.node_info(file).expect("held").nlink, 0);
    assert_eq!(fs.unpin(file), Ok(Unpinned::Freed));
}

/// The name a held node lost names a new node of its own: the held node's
/// entry, and so its id, is not given to another while it is held.
#[test]
fn a_name_a_held_node_lost_names_another_node() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let old = fs
        .create(root, b"name", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(old, 0, b"old").expect("write");
    fs.pin(old).expect("pinned");
    fs.remove(root, b"name").expect("remove");
    let new = fs
        .create(root, b"name", NodeKind::RegularFile, &FILE)
        .expect("create again");
    assert_ne!(new, old);
    fs.write_all(new, 0, b"new").expect("write");
    assert_eq!(fs.lookup(root, b"name"), Ok(new));
    assert_eq!(read_all(&mut fs, old), b"old");
    assert_eq!(fs.unpin(old), Ok(Unpinned::Freed));
    assert_eq!(read_all(&mut fs, new), b"new");
}

/// A held node keeps its id and its data through renames of it and of the
/// directory holding it, and lookups and listings answer that id.
#[test]
fn a_held_node_keeps_its_id_through_renames() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let outer = fs
        .create(root, b"outer", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let file = fs
        .create(outer, b"file", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(file, 0, b"kept").expect("write");
    let held = fs.pin(file).expect("pinned");
    let dir = fs.pin(outer).expect("pinned");
    fs.rename(outer, b"file", outer, b"renamed")
        .expect("rename");
    fs.rename(root, b"outer", root, b"moved")
        .expect("rename its directory");
    assert_eq!(fs.lookup(root, b"moved"), Ok(dir));
    assert_eq!(fs.lookup(dir, b"renamed"), Ok(held));
    let entries = listed(&mut fs, dir, 0, &[]).expect("lists");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.node, held);
    let other = fs
        .create(root, b"other", NodeKind::Directory, &DIR)
        .expect("mkdir");
    fs.rename(dir, b"renamed", other, b"across")
        .expect("move it");
    fs.rename(root, b"moved", other, b"inner")
        .expect("move its directory");
    assert_eq!(fs.lookup(other, b"across"), Ok(held));
    assert_eq!(fs.lookup(other, b"inner"), Ok(dir));
    assert_eq!(fs.parent(dir), Ok(other));
    assert_eq!(read_all(&mut fs, held), b"kept");
    assert_eq!(fs.unpin(held), Ok(Unpinned::Released));
    assert_eq!(fs.unpin(dir), Ok(Unpinned::Released));
}

/// A directory removed while held takes no entries, lists none and has no
/// parent, and its last unpin frees it.
#[test]
fn a_held_directory_removed_takes_no_entries() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let before = free(&mut fs);
    let gone = fs
        .create(root, b"gone", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.pin(gone).expect("pinned");
    fs.remove(root, b"gone").expect("rmdir");
    assert_eq!(fs.node_info(gone).expect("held").nlink, 0);
    assert_eq!(
        fs.node_info(root).expect("root").nlink,
        2,
        "its parent counts it no more"
    );
    let refused = [
        fs.create(gone, b"new", NodeKind::RegularFile, &FILE)
            .map(|_| ()),
        fs.create(gone, b"sub", NodeKind::Directory, &DIR)
            .map(|_| ()),
        fs.create_link(gone, b"link", b"file", &LINK).map(|_| ()),
        fs.link(gone, b"file", file),
        fs.rename(root, b"file", gone, b"file"),
        fs.lookup(gone, b"file").map(|_| ()),
        fs.parent(gone).map(|_| ()),
    ];
    for (at, outcome) in refused.into_iter().enumerate() {
        assert_eq!(outcome, Err(DriverError::NotFound), "{at}");
    }
    assert!(names(&mut fs, gone).is_empty());
    assert_eq!(
        fs.lookup(root, b"file"),
        Ok(file),
        "the refused rename moved nothing"
    );
    assert_eq!(fs.unpin(gone), Ok(Unpinned::Freed));
    assert_eq!(fs.node_info(gone), Err(DriverError::NotFound));
    fs.remove(root, b"file").expect("remove");
    assert_eq!(free(&mut fs), before);
    assert_eq!(fs.write_refusal(), None);
}

/// A rename over a held node takes its name but not the node, which gains
/// no name again and is freed at its last unpin.
#[test]
fn a_held_node_a_rename_replaces_is_kept_until_its_last_unpin() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let target = fs
        .create(root, b"target", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(target, 0, b"replaced").expect("write");
    let source = fs
        .create(root, b"source", NodeKind::RegularFile, &FILE)
        .expect("create");
    let old = fs
        .create(root, b"old", NodeKind::Directory, &DIR)
        .expect("mkdir");
    fs.create(root, b"new", NodeKind::Directory, &DIR)
        .expect("mkdir");
    fs.pin(target).expect("pinned");
    fs.pin(old).expect("pinned");
    let held = free(&mut fs);
    fs.rename(root, b"source", root, b"target")
        .expect("replace a file");
    fs.rename(root, b"new", root, b"old")
        .expect("replace a directory");
    assert_eq!(free(&mut fs), held, "nothing is freed while it is held");
    assert_eq!(fs.lookup(root, b"target"), Ok(source));
    assert_eq!(fs.node_info(target).expect("held").nlink, 0);
    assert_eq!(fs.node_info(old).expect("held").nlink, 0);
    assert_eq!(read_all(&mut fs, target), b"replaced");
    assert_eq!(fs.link(root, b"back", target), Err(DriverError::NotFound));
    assert_eq!(fs.lookup(root, b"back"), Err(DriverError::NotFound));
    assert_eq!(
        fs.create(old, b"x", NodeKind::RegularFile, &FILE),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.node_info(root).expect("root").nlink,
        3,
        "one child directory left"
    );
    assert_eq!(fs.unpin(target), Ok(Unpinned::Freed));
    assert_eq!(fs.unpin(old), Ok(Unpinned::Freed));
    assert_eq!(
        free(&mut fs),
        held + 4,
        "each one's entry, and the file's block and the directory's stream"
    );
    assert_eq!(fs.write_refusal(), None);
}

/// Releasing every hold frees each held node no name reaches, and leaves
/// the named ones as they were.
#[test]
fn releasing_every_hold_frees_the_nodes_no_name_reaches() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let kept = fs
        .create(root, b"kept", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(kept, 0, b"named").expect("write");
    let named = free(&mut fs);
    let mut held = Vec::new();
    for name in [&b"a"[..], b"b"] {
        let node = fs
            .create(root, name, NodeKind::RegularFile, &FILE)
            .expect("create");
        fs.write_all(node, 0, &pattern(1500, 1)).expect("write");
        held.push(fs.pin(node).expect("pinned"));
        fs.remove(root, name).expect("remove");
    }
    let dir = fs
        .create(root, b"dir", NodeKind::Directory, &DIR)
        .expect("mkdir");
    held.push(fs.pin(dir).expect("pinned"));
    fs.remove(root, b"dir").expect("rmdir");
    fs.pin(held[0]).expect("held twice");
    fs.pin(kept).expect("pinned");
    assert!(free(&mut fs) < named);
    assert_eq!(fs.release_pins(), Ok(()));
    assert_eq!(free(&mut fs), named);
    for node in held {
        assert_eq!(fs.node_info(node), Err(DriverError::NotFound));
        assert_eq!(fs.unpin(node), Err(DriverError::NotFound));
    }
    assert_eq!(fs.unpin(kept), Err(DriverError::NotFound), "its hold went");
    assert_eq!(read_all(&mut fs, kept), b"named");
    assert_eq!(fs.write_refusal(), None);
}

/// Once its last unpin frees it, a node no name and no hold reaches is
/// nothing to every call.
#[test]
fn a_node_its_last_unpin_freed_names_nothing() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"gone", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(file, 0, &pattern(2000, 5)).expect("write");
    fs.pin(file).expect("pinned");
    fs.remove(root, b"gone").expect("remove");
    assert_eq!(fs.unpin(file), Ok(Unpinned::Freed));
    assert_eq!(fs.node_info(file), Err(DriverError::NotFound));
    assert_eq!(
        fs.read_at(file, 0, &mut [0u8; 8]),
        Err(DriverError::NotFound)
    );
    assert_eq!(fs.security(file), Err(DriverError::NotFound));
    assert_eq!(fs.write_all(file, 0, b"stale"), Err(DriverError::NotFound));
    assert_eq!(fs.pin(file), Err(DriverError::NotFound));
}

/// UDF keeps no orphan record: a node still held when the volume is taken
/// away keeps its space, which nothing names, until the volume is checked.
#[test]
fn a_crash_leaves_a_held_nodes_space_allocated() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let empty = free(&mut fs);
    let file = fs
        .create(root, b"open", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(file, 0, &pattern(2000, 3)).expect("write");
    fs.pin(file).expect("pinned");
    fs.remove(root, b"open").expect("remove");
    let held = free(&mut fs);
    assert!(held < empty);
    // Taken from the device with the hold outstanding.
    let mut fs = remount(fs);
    assert_eq!(fs.write_refusal(), None, "the removal was published");
    assert_eq!(free(&mut fs), held);
    let root = fs.root();
    assert!(names(&mut fs, root).is_empty());
    assert_eq!(
        fs.unpin(file),
        Err(DriverError::NotFound),
        "no hold survives"
    );
}

#[test]
fn owners_and_modes_are_recorded() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(
            root,
            b"owned",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o640, 1000, 100),
        )
        .expect("create");
    fs.set_security(file, NodeSecurity::new(0o4750, 1000, 100))
        .expect("set");
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    let file = fs.lookup(root, b"owned").expect("found");
    let security = fs.security(file).expect("recorded");
    assert_eq!(
        (security.mode, security.uid, security.gid),
        (0o4750, 1000, 100)
    );
    assert!(fs.records_ownership(file).expect("node"));
}

/// A new node is born with the owner, group and mode it is created with;
/// what a file entry has no place for is not kept, and an owner left
/// unspecified reads as unrecorded.
#[test]
fn a_new_node_is_born_with_its_record() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let mut gated = NodeSecurity::new(0o600, 1002, 102);
    gated.required_cap = Some(CapabilityId::AUDIT_READ);
    gated
        .push_acl(SecurityAcl {
            subject: SecuritySubject::User(7),
            perms: 0b100,
        })
        .expect("an entry");
    let made = [
        NodeSecurity::new(0o2750, 1000, 100),
        NodeSecurity::new(0o4640, 1000, 100),
        NodeSecurity::new(0o777, 1001, 101),
        NodeSecurity::new(0o600, 1002, 102),
    ];
    let dir = fs
        .create(root, b"dir", NodeKind::Directory, &made[0])
        .expect("mkdir");
    let file = fs
        .create(dir, b"file", NodeKind::RegularFile, &made[1])
        .expect("create");
    let link = fs
        .create_link(dir, b"link", b"file", &made[2])
        .expect("symlink");
    let plain = fs
        .create(dir, b"plain", NodeKind::RegularFile, &gated)
        .expect("create");
    let unowned = fs
        .create(
            dir,
            b"unowned",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, u32::MAX, u32::MAX),
        )
        .expect("create");
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    for (node, recorded) in [dir, file, link, plain].into_iter().zip(made) {
        assert_eq!(fs.security(node), Ok(recorded));
        assert!(fs.records_ownership(node).expect("a node"));
    }
    assert!(!fs.records_ownership(unowned).expect("a node"));
}

/// A file entry has no place for ACL entries or a capability gate, so a
/// record carrying either is refused before anything is written.
#[test]
fn a_record_a_file_entry_cannot_hold_is_refused_whole() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let recorded = NodeSecurity::new(0o640, 1000, 100);
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &recorded)
        .expect("create");
    fs.flush().expect("flush");
    let mut gated = NodeSecurity::new(0o600, 1000, 100);
    gated.required_cap = Some(CapabilityId::AUDIT_READ);
    let mut with_acl = NodeSecurity::new(0o600, 1000, 100);
    with_acl
        .push_acl(SecurityAcl {
            subject: SecuritySubject::Group(5),
            perms: 0b110,
        })
        .expect("an entry");
    let mut posix = NodeSecurity::new(0o600, 1000, 100);
    posix.acl_model = AclModel::Posix {
        group_obj: 0b100,
        mask: 0b100,
    };
    let writes = fs.dev.device().writes;
    for refused in [gated, with_acl, posix] {
        assert_eq!(
            fs.set_security(file, refused),
            Err(DriverError::Unsupported)
        );
    }
    assert_eq!(fs.dev.device().writes, writes, "nothing was written");
    assert_eq!(fs.security(file), Ok(recorded));
}

/// A name for a node this system has no kind for, such as a pipe another
/// system made, is no name: it is not removed or renamed, and a rename over
/// it is refused rather than replace what no lookup showed.
#[test]
fn a_name_for_a_node_of_no_kind_is_left_alone() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let pipe = fs
        .create(root, b"pipe", NodeKind::RegularFile, &FILE)
        .expect("create");
    let file = fs
        .create(root, b"file", NodeKind::RegularFile, &FILE)
        .expect("create");
    // File type 9, a FIFO.
    let mut fs = patched(fs, pipe, |block| block[27] = 9);
    let root = fs.root();
    let before = free(&mut fs);
    assert_eq!(fs.lookup(root, b"pipe"), Err(DriverError::NotFound));
    assert_eq!(fs.remove(root, b"pipe"), Err(DriverError::NotFound));
    assert_eq!(
        fs.rename(root, b"pipe", root, b"moved"),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        fs.rename(root, b"file", root, b"pipe"),
        Err(DriverError::Unsupported)
    );
    assert_eq!(fs.lookup(root, b"file"), Ok(file));
    assert_eq!(
        fs.create(root, b"pipe", NodeKind::RegularFile, &FILE),
        Err(DriverError::AlreadyExists),
        "the name is still taken"
    );
    assert_eq!(free(&mut fs), before);
    assert_eq!(fs.write_refusal(), None);
}

/// `fs` given a host whose clock lets a change wait for its write-back
/// window.
fn hosted(mut fs: Udf<Disk>) -> (Udf<Disk>, &'static TestVolumeHost) {
    let host = TestVolumeHost::leaked(1_000);
    fs.set_volume_host(TestVolumeHost::volume(), host);
    (fs, host)
}

#[test]
fn the_integrity_descriptor_is_open_while_a_change_waits_for_its_window() {
    let (mut fs, host) = hosted(formatted(512, 0x0201));
    let root = fs.root();
    fs.create(root, b"pending", NodeKind::RegularFile, &FILE)
        .expect("create");
    assert!(
        host.reported().is_some_and(|due| due > 1_000),
        "the open change names its deadline"
    );
    // Taken from the device unpublished: a volume left mid-change.
    let crashed = Udf::open(fs.into_device()).expect("mounts");
    assert_eq!(crashed.write_refusal(), Some(WriteRefusal::NotClosed));
    let (mut fs, host) = hosted(formatted(512, 0x0201));
    let root = fs.root();
    let done = fs
        .create(root, b"done", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(done, 0, &pattern(3000, 1)).expect("write");
    let free = fs.stats().expect("stats").free_blocks;
    fs.flush().expect("flush");
    assert_eq!(host.reported(), None, "published, nothing open");
    let mut fs = remount(fs);
    assert_eq!(fs.write_refusal(), None);
    assert_eq!(
        fs.stats().expect("stats").free_blocks,
        free,
        "the recorded free count is kept"
    );
}

#[test]
fn a_change_ending_past_its_window_publishes_the_volume() {
    let (mut fs, host) = hosted(formatted(512, 0x0201));
    let root = fs.root();
    fs.create(root, b"first", NodeKind::RegularFile, &FILE)
        .expect("create");
    let due = host.reported().expect("open");
    host.set_now(due);
    fs.create(root, b"second", NodeKind::RegularFile, &FILE)
        .expect("create");
    assert_eq!(host.reported(), None, "published as the late change ended");
    let mut fs = Udf::open(fs.into_device()).expect("mounts");
    assert_eq!(fs.write_refusal(), None);
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"first"[..], b"second"]);
}

#[test]
fn without_a_host_every_change_is_published_as_it_ends() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    fs.create(root, b"kept", NodeKind::RegularFile, &FILE)
        .expect("create");
    // Taken from the device unflushed, and closed all the same.
    let mut fs = Udf::open(fs.into_device()).expect("mounts");
    assert_eq!(fs.write_refusal(), None);
    let root = fs.root();
    fs.lookup(root, b"kept").expect("published");
}

#[test]
fn an_embedded_file_is_moved_into_blocks_when_written() {
    let mut fs = formatted(2048, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"tiny", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.flush().expect("flush");
    // Rewrite the entry as one recording its bytes inline, as other
    // writers record small files.
    let addr = super::addr_of(file).expect("an address");
    let mut probe = remount(fs);
    let mut block = probe.resolve(addr).expect("entry").block;
    let sector = probe.partitions.sector(addr).expect("a sector") as usize;
    let mut disk = probe.into_device();
    let ad = super::entry::EFE_FIXED;
    block[34] = (block[34] & !0b111) | 3;
    block[ad..ad + 5].copy_from_slice(b"hello");
    block[56..64].copy_from_slice(&5u64.to_le_bytes());
    block[64..72].copy_from_slice(&5u64.to_le_bytes());
    block[72..80].fill(0);
    block[212..216].copy_from_slice(&5u32.to_le_bytes());
    let tag = tairix_fsprobe::udf::Tag::checked(&block).unwrap_or(tairix_fsprobe::udf::Tag {
        id: tairix_fsprobe::udf::TAG_EFE,
        version: 3,
        serial: 0,
        location: addr.block,
    });
    tag.seal(&mut block, ad + 5 - 16).expect("seal");
    disk.bytes[sector * 2048..(sector + 1) * 2048].copy_from_slice(&block);
    let mut fs = Udf::open(disk).expect("mounts");
    let root = fs.root();
    let file = fs.lookup(root, b"tiny").expect("found");
    assert_eq!(read_all(&mut fs, file), b"hello");
    fs.write_all(file, 5, b", world")
        .expect("grows out of its entry");
    assert_eq!(read_all(&mut fs, file), b"hello, world");
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    let file = fs.lookup(root, b"tiny").expect("found");
    assert_eq!(read_all(&mut fs, file), b"hello, world");
}

#[test]
fn names_this_system_cannot_hold_are_refused_on_create() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    for name in [&b""[..], b".", b"..", b"a/b", b"nul\0", &[0xFF, 0xFE]] {
        assert_eq!(
            fs.create(root, name, NodeKind::RegularFile, &FILE),
            Err(DriverError::OutOfRange),
            "{name:?}"
        );
    }
}

#[test]
fn a_full_volume_refuses_space_cleanly() {
    let mut fs =
        Udf::format(Disk::new(512, vec![0u8; 512 * 400]), &options(0x0201)).expect("formats");
    let root = fs.root();
    let big = fs
        .create(root, b"big", NodeKind::RegularFile, &FILE)
        .expect("create");
    let free = fs.stats().expect("stats").free_blocks;
    // A write that cannot be given all its blocks takes none of them.
    assert_eq!(
        fs.write_all(big, 0, &vec![7u8; 512 * 400]),
        Err(DriverError::NoSpace)
    );
    assert_eq!(fs.stats().expect("stats").free_blocks, free);
    assert_eq!(fs.node_info(big).expect("info").size, 0);
    let mut written = 0u64;
    while fs.write_all(big, written, &[7u8; 512]).is_ok() {
        written += 512;
    }
    assert!(written > 0);
    assert_eq!(fs.stats().expect("stats").free_blocks, 0);
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    assert_eq!(fs.write_refusal(), None);
    assert_eq!(fs.stats().expect("stats").free_blocks, 0);
    let root = fs.root();
    let big = fs.lookup(root, b"big").expect("found");
    assert_eq!(read_all(&mut fs, big), vec![7u8; written as usize]);
}

#[test]
fn a_disc_mastered_as_a_bridge_is_read_and_refused_writes() {
    let image = sparse::sparse(include_bytes!("../../iso9660/tests/fixtures/bridge.sparse"));
    let mut fs = Udf::open(Disk::new(2048, image)).expect("mounts");
    // genisoimage marks its UDF write-protected as well as read-only.
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::WriteProtected));
    let root = fs.root();
    let listing = names(&mut fs, root);
    assert!(
        listing.iter().any(|name| name == b"README.TXT"),
        "{listing:?}"
    );
    let readme = fs.lookup(root, b"README.TXT").expect("found");
    assert_eq!(read_all(&mut fs, readme), b"hello\n");
    let many = fs.lookup(root, b"many").expect("found");
    assert_eq!(names(&mut fs, many).len(), 30);
    assert_eq!(
        fs.create(root, b"x", NodeKind::RegularFile, &FILE),
        Err(DriverError::ReadOnly)
    );
}

/// `fs` flushed, then remounted with the entry `node` names rewritten as
/// `edit` leaves its block, resealed over the areas its lengths then name.
fn patched(mut fs: Udf<Disk>, node: NodeId, edit: impl FnOnce(&mut [u8])) -> Udf<Disk> {
    fs.flush().expect("flush");
    let addr = super::addr_of(node).expect("an address");
    let mut block = fs.resolve(addr).expect("entry").block;
    let sector = fs.partitions.sector(addr).expect("a sector") as usize;
    let bs = fs.block_size as usize;
    let tag = tairix_fsprobe::udf::Tag::checked(&block).expect("sealed");
    edit(&mut block);
    let (fixed, lengths) = if tag.id == tairix_fsprobe::udf::TAG_EFE {
        (super::entry::EFE_FIXED, 208)
    } else {
        (super::entry::FE_FIXED, 168)
    };
    let word = |at: usize| {
        u32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]]) as usize
    };
    let areas = word(lengths) + word(lengths + 4);
    tag.seal(&mut block, fixed + areas - 16).expect("seal");
    let mut disk = fs.into_device();
    disk.bytes[sector * bs..(sector + 1) * bs].copy_from_slice(&block);
    Udf::open(disk).expect("mounts")
}

/// Rewrite an Extended File Entry's block as one recording `content` inline.
fn embed(block: &mut [u8], content: &[u8]) {
    let at = super::entry::EFE_FIXED;
    block[34] = (block[34] & !0b111) | 3;
    block[at..].fill(0);
    block[at..at + content.len()].copy_from_slice(content);
    block[56..64].copy_from_slice(&(content.len() as u64).to_le_bytes());
    block[64..72].copy_from_slice(&(content.len() as u64).to_le_bytes());
    block[72..80].fill(0);
    block[208..212].fill(0);
    block[212..216].copy_from_slice(&(content.len() as u32).to_le_bytes());
}

fn attr(fs: &mut Udf<Disk>, node: NodeId, key: &[u8]) -> Option<Vec<u8>> {
    use tairix_abi::driver::filesystem::FilesystemAttrs;
    let mut value = [0u8; 16];
    fs.get_attr(node, key, &mut value)
        .expect("reads")
        .map(|len| value[..len].to_vec())
}

fn attr_keys(fs: &mut Udf<Disk>, node: NodeId) -> Vec<Vec<u8>> {
    use tairix_abi::driver::filesystem::FilesystemAttrs;
    (0..)
        .map_while(|index| {
            let mut key = [0u8; 32];
            fs.list_attr(node, index, &mut key)
                .expect("lists")
                .map(|len| key[..len].to_vec())
        })
        .collect()
}

fn free(fs: &mut Udf<Disk>) -> u64 {
    fs.stats().expect("stats").free_blocks
}

#[test]
fn finder_information_is_kept_as_the_mac_attributes() {
    use tairix_abi::driver::filesystem::FilesystemAttrs;
    for (block_size, revision) in [(512, 0x0201), (2048, 0x0150)] {
        let mut fs = formatted(block_size, revision);
        let root = fs.root();
        let doc = fs
            .create(root, b"doc", NodeKind::RegularFile, &FILE)
            .expect("create");
        fs.write_all(doc, 0, b"Dear Sir,").expect("write");
        let folder = fs
            .create(root, b"folder", NodeKind::Directory, &DIR)
            .expect("mkdir");
        assert!(attr_keys(&mut fs, doc).is_empty());
        fs.set_attr(doc, b"mac.type", b"TEXT").expect("type");
        fs.set_attr(doc, b"mac.creator", b"ttxt").expect("creator");
        fs.set_attr(doc, b"mac.finderflags", &[0x01, 0x00])
            .expect("flags");
        fs.set_attr(folder, b"mac.finderflags", &[0x40, 0x00])
            .expect("a folder's flags");
        assert_eq!(
            fs.set_attr(folder, b"mac.type", b"TEXT"),
            Err(DriverError::Unsupported),
            "a folder has no type"
        );
        assert_eq!(
            fs.set_attr(doc, b"acorn.filetype", b"fff"),
            Err(DriverError::Unsupported)
        );
        assert_eq!(
            fs.set_attr(doc, b"mac.type", b"TOOLONG"),
            Err(DriverError::OutOfRange)
        );
        fs.remove_attr(doc, b"mac.creator").expect("removed");
        assert_eq!(
            fs.remove_attr(doc, b"mac.creator"),
            Err(DriverError::NotFound)
        );
        assert_eq!(
            fs.remove_attr(doc, b"user.note"),
            Err(DriverError::NotFound)
        );
        fs.flush().expect("flush");
        let mut fs = remount(fs);
        let root = fs.root();
        let doc = fs.lookup(root, b"doc").expect("found");
        let folder = fs.lookup(root, b"folder").expect("found");
        assert_eq!(
            attr(&mut fs, doc, b"mac.type").as_deref(),
            Some(&b"TEXT"[..]),
            "{block_size}"
        );
        assert_eq!(attr(&mut fs, doc, b"mac.creator"), None);
        assert_eq!(
            attr(&mut fs, doc, b"mac.finderflags").as_deref(),
            Some(&[0x01, 0x00][..])
        );
        assert_eq!(
            attr_keys(&mut fs, doc),
            [&b"mac.type"[..], b"mac.finderflags"]
        );
        assert_eq!(attr_keys(&mut fs, folder), [&b"mac.finderflags"[..]]);
        assert_eq!(
            read_all(&mut fs, doc),
            b"Dear Sir,",
            "the data is where it was"
        );
    }
}

#[test]
fn finder_information_makes_room_by_moving_what_follows_it() {
    use tairix_abi::driver::filesystem::FilesystemAttrs;
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let small = fs
        .create(root, b"small", NodeKind::RegularFile, &FILE)
        .expect("create");
    let full = fs
        .create(root, b"full", NodeKind::RegularFile, &FILE)
        .expect("create");
    let fs = patched(fs, small, |block| embed(block, b"hello"));
    let crowded = pattern(280, 3);
    let mut fs = patched(fs, full, |block| embed(block, &crowded));
    let root = fs.root();
    // A fragmented file whose descriptors fill its entry: the attribute
    // pushes them on into a continuation.
    let split = fs
        .create(root, b"split", NodeKind::RegularFile, &FILE)
        .expect("create");
    let gap = fs
        .create(root, b"gap", NodeKind::RegularFile, &FILE)
        .expect("create");
    let mut expected = Vec::new();
    for i in 0..37u8 {
        let block = vec![i; 512];
        fs.write_all(split, u64::from(i) * 512, &block)
            .expect("split");
        fs.write_all(gap, u64::from(i) * 512, &block).expect("gap");
        expected.extend_from_slice(&block);
    }
    for node in [small, full, split] {
        fs.set_attr(node, b"mac.type", b"BINA").expect("set");
    }
    let small_entry = fs
        .resolve(super::addr_of(small).expect("addr"))
        .expect("entry");
    assert_eq!(
        small_entry.entry.form,
        super::entry::AdForm::Embedded,
        "five bytes still fit beside it"
    );
    let full_entry = fs
        .resolve(super::addr_of(full).expect("addr"))
        .expect("entry");
    assert_eq!(
        full_entry.entry.form,
        super::entry::AdForm::Short,
        "the bytes moved into a block"
    );
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    for (node, content) in [(small, &b"hello"[..]), (full, &crowded), (split, &expected)] {
        assert_eq!(read_all(&mut fs, node), content);
        assert_eq!(
            attr(&mut fs, node, b"mac.type").as_deref(),
            Some(&b"BINA"[..])
        );
    }
}

#[test]
fn finder_information_in_an_attribute_file_is_read_and_rewritten_there() {
    use tairix_abi::driver::filesystem::FilesystemAttrs;
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let before = free(&mut fs);
    let target = fs
        .create(root, b"target", NodeKind::RegularFile, &FILE)
        .expect("create");
    let attributes = fs
        .create(root, b"attributes", NodeKind::RegularFile, &FILE)
        .expect("create");
    let at = super::addr_of(attributes).expect("addr").block;
    let header = tairix_fsprobe::udf::Tag {
        id: tairix_fsprobe::udf::TAG_EAHD,
        version: 3,
        serial: 0,
        location: at,
    };
    let finder = tairix_fsmeta::preset::mac::FinderInfo {
        file_type: *b"PICT",
        creator: *b"8BIM",
        flags: 0,
    };
    let content = super::ea::with_finder(&[], header, false, finder, 0x0201).expect("built");
    let fs = patched(fs, attributes, |block| {
        embed(block, &content);
        block[27] = super::entry::TYPE_ATTRIBUTES;
    });
    let mut fs = patched(fs, target, |block| {
        block[136..140].copy_from_slice(&(content.len() as u32).to_le_bytes());
        block[140..144].copy_from_slice(&at.to_le_bytes());
    });
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [&b"target"[..]],
        "an attribute file is no node of its own"
    );
    assert_eq!(
        attr(&mut fs, target, b"mac.creator").as_deref(),
        Some(&b"8BIM"[..])
    );
    fs.set_attr(target, b"mac.creator", b"ogle")
        .expect("rewritten");
    let entry = fs
        .resolve(super::addr_of(target).expect("addr"))
        .expect("entry");
    assert!(
        entry.entry.ea.is_empty(),
        "the attribute stays in the file that held it"
    );
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    assert_eq!(
        attr(&mut fs, target, b"mac.creator").as_deref(),
        Some(&b"ogle"[..])
    );
    assert_eq!(
        attr(&mut fs, target, b"mac.type").as_deref(),
        Some(&b"PICT"[..])
    );
    fs.remove(root, b"target").expect("removed");
    assert_eq!(
        free(&mut fs),
        before,
        "the attribute file goes with the node"
    );
}

#[test]
fn removing_a_node_frees_its_named_streams() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let before = free(&mut fs);
    let target = fs
        .create(root, b"target", NodeKind::RegularFile, &FILE)
        .expect("create");
    let streams = fs
        .create(root, b"streams", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let fork = fs
        .create(streams, b"fork", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(fork, 0, &[5u8; 700]).expect("write");
    let at = super::addr_of(streams).expect("addr").block;
    let fs = patched(fs, streams, |block| block[27] = super::entry::TYPE_STREAMS);
    let mut fs = patched(fs, target, |block| {
        block[152..156].copy_from_slice(&512u32.to_le_bytes());
        block[156..160].copy_from_slice(&at.to_le_bytes());
    });
    let root = fs.root();
    fs.remove(root, b"target").expect("removed");
    assert_eq!(
        free(&mut fs),
        before,
        "the streams and their directory go with the node"
    );
}

/// A volume holding `a`, its 37 three-block extents filling its entry's
/// descriptors, and no free block left.
fn crowded_and_full() -> (Udf<Disk>, Vec<u8>) {
    let mut fs =
        Udf::format(Disk::new(512, vec![0u8; 512 * 600]), &options(0x0201)).expect("formats");
    let root = fs.root();
    let a = fs
        .create(root, b"a", NodeKind::RegularFile, &FILE)
        .expect("create");
    let b = fs
        .create(root, b"b", NodeKind::RegularFile, &FILE)
        .expect("create");
    let fill = fs
        .create(root, b"fill", NodeKind::RegularFile, &FILE)
        .expect("create");
    let mut content = Vec::new();
    for i in 0..37u64 {
        let run = pattern(3 * 512, i as u8);
        fs.write_all(a, i * 3 * 512, &run).expect("a");
        fs.write_all(b, i * 512, &[1u8; 512]).expect("b");
        content.extend_from_slice(&run);
    }
    let entry = fs.resolve(super::addr_of(a).expect("addr")).expect("entry");
    assert_eq!(
        entry.entry.ad.len(),
        37 * 8,
        "every descriptor in the entry"
    );
    let mut at = 0;
    while fs.write_all(fill, at, &[2u8; 512]).is_ok() {
        at += 512;
    }
    assert_eq!(free(&mut fs), 0);
    (fs, content)
}

#[test]
fn a_change_the_volume_lacks_the_space_to_record_leaves_it_as_it_was() {
    let (mut fs, content) = crowded_and_full();
    let root = fs.root();
    let a = fs.lookup(root, b"a").expect("found");
    // Growing the file by a hole needs a continuation block.
    let size = content.len() as u64;
    assert_eq!(fs.truncate(a, size + 10 * 512), Err(DriverError::NoSpace));
    assert_eq!(fs.node_info(a).expect("info").size, size);
    assert_eq!(free(&mut fs), 0);
    let fill = fs.lookup(root, b"fill").expect("found");
    let filled = fs.node_info(fill).expect("info").size;
    fs.truncate(fill, filled - 512).expect("one block back");
    assert_eq!(free(&mut fs), 1);
    // A block of data past a gap needs that block and a continuation too.
    assert_eq!(
        fs.write_all(a, 200 * 512, &[9u8; 512]),
        Err(DriverError::NoSpace)
    );
    assert_eq!(free(&mut fs), 1, "the data block taken first went back");
    assert_eq!(fs.node_info(a).expect("info").size, size);
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    assert_eq!(read_all(&mut fs, a), content);
    assert_eq!(free(&mut fs), 1);
}

#[test]
fn a_full_volume_can_still_punch_a_hole() {
    let (mut fs, mut content) = crowded_and_full();
    let root = fs.root();
    let a = fs.lookup(root, b"a").expect("found");
    // The block the hole frees holds the continuation the split needs.
    fs.punch(a, 31 * 512, 512).expect("punched");
    content[31 * 512..32 * 512].fill(0);
    assert_eq!(free(&mut fs), 0);
    assert_eq!(read_all(&mut fs, a), content);
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    assert_eq!(read_all(&mut fs, a), content);
    assert_eq!(
        fs.seek_region(a, 0, SeekRegion::Hole).expect("seeks"),
        Some(31 * 512)
    );
}

#[test]
fn an_entry_with_no_room_for_a_descriptor_refuses_the_write() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let crammed = fs
        .create(root, b"crammed", NodeKind::RegularFile, &FILE)
        .expect("create");
    // Attributes leave four bytes, less than one descriptor.
    let attributes = (512 - super::entry::EFE_FIXED - 4) as u32;
    let mut fs = patched(fs, crammed, |block| {
        block[208..212].copy_from_slice(&attributes.to_le_bytes());
    });
    let before = free(&mut fs);
    assert_eq!(
        fs.write_all(crammed, 0, &[1u8; 100]),
        Err(DriverError::NoSpace)
    );
    assert_eq!(free(&mut fs), before);
    assert_eq!(fs.node_info(crammed).expect("info").size, 0);
}

#[test]
fn a_refused_write_leaves_the_node_it_names_untouched() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let link = fs
        .create_link(root, b"link", b"target", &LINK)
        .expect("link");
    let tiny = fs
        .create(root, b"tiny", NodeKind::RegularFile, &FILE)
        .expect("create");
    let fs = patched(fs, link, |block| {
        // One named component, its identifier compressed as OSTA CS0 8-bit.
        embed(block, &[5, 7, 0, 0, 8, b't', b'a', b'r', b'g', b'e', b't']);
        block[27] = super::entry::TYPE_SYMLINK;
    });
    let mut fs = patched(fs, tiny, |block| embed(block, b"tiny"));
    let root = fs.root();
    fs.link(root, b"tiny-again", tiny).expect("hard link");
    assert_eq!(fs.write_all(link, 0, b"x"), Err(DriverError::Unsupported));
    assert_eq!(fs.truncate(link, 0), Err(DriverError::Unsupported));
    let link_entry = fs
        .resolve(super::addr_of(link).expect("addr"))
        .expect("entry");
    assert_eq!(
        link_entry.entry.form,
        super::entry::AdForm::Embedded,
        "a refused write moves nothing"
    );
    let mut target = [0u8; 16];
    let len = fs.read_link(link, &mut target).expect("reads");
    assert_eq!(&target[..len], b"target");
    // Dropping one of two names changes only the count.
    fs.remove(root, b"tiny").expect("removed");
    let tiny_entry = fs
        .resolve(super::addr_of(tiny).expect("addr"))
        .expect("entry");
    assert_eq!(tiny_entry.entry.form, super::entry::AdForm::Embedded);
    assert_eq!(tiny_entry.entry.links, 1);
    assert_eq!(read_all(&mut fs, tiny), b"tiny");
}

#[test]
fn a_hole_longer_than_one_descriptor_records_is_split_across_several() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let vast = fs
        .create(root, b"vast", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(vast, 0, b"head").expect("write");
    let size = 3u64 << 30;
    fs.truncate(vast, size).expect("grows by a hole");
    let far = (5u64 << 29) + 7;
    fs.write_all(vast, far, b"tail")
        .expect("data far into the hole");
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    let file = fs.lookup(root, b"vast").expect("found");
    assert_eq!(fs.node_info(file).expect("info").size, size);
    let mut buf = [0u8; 4];
    fs.read_at(file, 0, &mut buf).expect("reads");
    assert_eq!(&buf, b"head");
    fs.read_at(file, far, &mut buf).expect("reads");
    assert_eq!(&buf, b"tail");
    fs.read_at(file, 2u64 << 30, &mut buf).expect("reads");
    assert_eq!(buf, [0; 4]);
    assert_eq!(fs.seek_region(file, 4, SeekRegion::Hole), Ok(Some(512)));
    assert_eq!(
        fs.seek_region(file, 512, SeekRegion::Data),
        Ok(Some(5u64 << 29))
    );
    // A hole more descriptors than a file may have would record is refused.
    assert_eq!(
        fs.truncate(file, u64::MAX / 2),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(fs.node_info(file).expect("info").size, size);
    assert_eq!(fs.write_refusal(), None);
}

#[test]
fn partition_maps_never_chain_into_a_cycle() {
    use super::layout::{Map, Partitions, Physical};
    let physical = Physical {
        number: 0,
        start: 64,
        length: 1000,
        access: 0,
        bitmap: tairix_fsprobe::udf::ShortAd {
            length: 0,
            position: 0,
        },
        table: tairix_fsprobe::udf::ShortAd {
            length: 0,
            position: 0,
        },
    };
    // Two write-once maps of one partition: each sits on the physical map,
    // never on the other, though the first is resolved before the second.
    let mut maps = vec![
        Map::Physical(physical),
        Map::Physical(physical),
        Map::Physical(physical),
    ];
    let numbers = [0u16, 0, 0];
    let awaiting = [1usize, 2];
    assert_eq!(
        super::beneath_of(&maps, &numbers, &awaiting, 0, true),
        Some(0)
    );
    maps[1] = Map::Virtual {
        beneath: 0,
        table: Vec::new(),
    };
    assert_eq!(
        super::beneath_of(&maps, &numbers, &awaiting, 0, true),
        Some(0)
    );
    // A metadata map sits on the write-once map of its number.
    assert_eq!(
        super::beneath_of(&maps, &numbers, &awaiting, 0, false),
        Some(1)
    );
    assert_eq!(
        super::beneath_of(&maps, &numbers, &awaiting, 7, true),
        None,
        "no map of that number"
    );
    // Maps that name each other lead nowhere, rather than round for ever.
    let cycle = Partitions {
        maps: vec![
            Map::Virtual {
                beneath: 1,
                table: Vec::new(),
            },
            Map::Metadata {
                beneath: 0,
                file: Vec::new(),
            },
        ],
    };
    assert!(cycle.physical(0).is_none());
}

/// Rewrite the single short descriptor of the empty file `node` to name
/// `blocks` blocks from `start`.
fn misplaced(fs: Udf<Disk>, node: NodeId, start: u32, blocks: u32) -> Udf<Disk> {
    patched(fs, node, |block| {
        let at = super::entry::EFE_FIXED;
        block[at..at + 4].copy_from_slice(&(blocks * 512).to_le_bytes());
        block[at + 4..at + 8].copy_from_slice(&start.to_le_bytes());
        block[212..216].copy_from_slice(&8u32.to_le_bytes());
        block[56..64].copy_from_slice(&u64::from(blocks * 512).to_le_bytes());
        block[64..72].copy_from_slice(&u64::from(blocks * 512).to_le_bytes());
    })
}

#[test]
fn a_file_naming_blocks_past_its_partition_is_refused_writes() {
    for start in [u32::MAX - 1, 20_000] {
        let mut fs = formatted(512, 0x0201);
        let root = fs.root();
        let file = fs
            .create(root, b"wild", NodeKind::RegularFile, &FILE)
            .expect("create");
        let mut fs = misplaced(fs, file, start, 2);
        let before = free(&mut fs);
        assert_eq!(
            fs.write_all(file, 1024, b"more"),
            Err(DriverError::DeviceFault),
            "{start}"
        );
        assert_eq!(fs.truncate(file, 0), Err(DriverError::DeviceFault));
        assert_eq!(fs.punch(file, 0, 512), Err(DriverError::DeviceFault));
        assert_eq!(free(&mut fs), before);
        assert_eq!(
            fs.write_refusal(),
            None,
            "refused before anything was written"
        );
    }
}

#[test]
fn embedded_bytes_reaching_past_their_area_are_refused() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"lies", NodeKind::RegularFile, &FILE)
        .expect("create");
    let mut fs = patched(fs, file, |block| {
        embed(block, b"tiny!");
        block[56..64].copy_from_slice(&(u64::MAX - 5).to_le_bytes());
    });
    let mut buf = [0u8; 4];
    assert_eq!(
        fs.read_at(file, u64::MAX - 10, &mut buf),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        fs.node_info(file).map(|info| info.size),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn unique_ids_skip_the_reserved_low_values_and_run_out_cleanly() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    fs.integrity.next_unique_id = 0x1_FFFF_FFFF;
    fs.integrity.files = u32::MAX;
    let file = fs
        .create(root, b"wraps", NodeKind::RegularFile, &FILE)
        .expect("create");
    let entry = fs
        .resolve(super::addr_of(file).expect("addr"))
        .expect("entry");
    assert_eq!(entry.entry.unique_id, 0x1_FFFF_FFFF);
    assert_eq!(
        fs.integrity.next_unique_id, 0x2_0000_0010,
        "the low half lifted past 15"
    );
    assert_eq!(fs.integrity.files, u32::MAX, "the count holds at its most");
    fs.integrity.next_unique_id = u64::MAX;
    let before = free(&mut fs);
    assert_eq!(
        fs.create(root, b"none", NodeKind::RegularFile, &FILE),
        Err(DriverError::NoSpace)
    );
    assert_eq!(free(&mut fs), before);
    assert_eq!(fs.lookup(root, b"none"), Err(DriverError::NotFound));
}

/// `crowded_and_full`'s volume with one block given back.
fn one_block_free() -> Udf<Disk> {
    let (mut fs, _) = crowded_and_full();
    let root = fs.root();
    let fill = fs.lookup(root, b"fill").expect("found");
    let filled = fs.node_info(fill).expect("info").size;
    fs.truncate(fill, filled - 512).expect("one block back");
    assert_eq!(free(&mut fs), 1);
    fs
}

#[test]
fn a_node_whose_content_cannot_be_recorded_is_never_named() {
    let mut fs = one_block_free();
    let root = fs.root();
    // The entry takes the one block; the directory's parent descriptor, and
    // the link's path, need another.
    assert_eq!(
        fs.create(root, b"dir", NodeKind::Directory, &DIR),
        Err(DriverError::NoSpace)
    );
    assert_eq!(
        fs.create_link(root, b"link", b"somewhere", &LINK),
        Err(DriverError::NoSpace)
    );
    assert_eq!(fs.lookup(root, b"dir"), Err(DriverError::NotFound));
    assert_eq!(fs.lookup(root, b"link"), Err(DriverError::NotFound));
    assert_eq!(free(&mut fs), 1);
    assert_eq!(fs.write_refusal(), None);
    fs.create(root, b"file", NodeKind::RegularFile, &FILE)
        .expect("an empty file needs only its entry");
}

#[test]
fn a_directory_whose_parent_has_every_name_it_may_count_is_refused() {
    let fs = formatted(512, 0x0201);
    let root = fs.root();
    let mut fs = patched(fs, root, |block| {
        block[48..50].copy_from_slice(&u16::MAX.to_le_bytes());
    });
    let before = free(&mut fs);
    assert_eq!(
        fs.create(root, b"one-too-many", NodeKind::Directory, &DIR),
        Err(DriverError::TooManyLinks)
    );
    assert_eq!(fs.lookup(root, b"one-too-many"), Err(DriverError::NotFound));
    assert_eq!(free(&mut fs), before);
}

/// Names `root` until its stream fills its last block exactly, so the next
/// name it takes needs a block of its own; `prefix` keeps the names apart.
fn fill_root_block(fs: &mut Udf<Disk>, prefix: char) {
    let root = fs.root();
    for i in 0.. {
        let size = fs
            .resolve(super::addr_of(root).expect("addr"))
            .expect("root")
            .entry
            .size;
        let left = 512 - size % 512;
        if left == 512 {
            return;
        }
        // A descriptor is 38 bytes and its identifier, padded to four.
        let name = if left >= 88 {
            alloc::format!("{prefix}{i}")
        } else {
            core::iter::once(prefix)
                .chain(core::iter::repeat_n('z', left as usize - 40))
                .collect()
        };
        fs.create(root, name.as_bytes(), NodeKind::RegularFile, &FILE)
            .expect("create");
    }
}

/// A volume whose root's stream fills its block, with no block free: `a`
/// and its second name `twin`, `b`, and `c` with its second name `c2`.
fn named_and_full() -> Udf<Disk> {
    let mut fs =
        Udf::format(Disk::new(512, vec![0u8; 512 * 400]), &options(0x0201)).expect("formats");
    let root = fs.root();
    let a = fs
        .create(root, b"a", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(a, 0, b"the a file").expect("write");
    fs.link(root, b"twin", a).expect("link");
    fs.create(root, b"b", NodeKind::RegularFile, &FILE)
        .expect("create");
    let c = fs
        .create(root, b"c", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.link(root, b"c2", c).expect("link");
    let fill = fs
        .create(root, b"fill", NodeKind::RegularFile, &FILE)
        .expect("create");
    fill_root_block(&mut fs, 'p');
    let mut at = 0;
    while fs.write_all(fill, at, &[2u8; 512]).is_ok() {
        at += 512;
    }
    assert_eq!(free(&mut fs), 0);
    fs
}

#[test]
fn a_link_the_directory_has_no_room_for_leaves_the_count_as_it_was() {
    let mut fs = named_and_full();
    let root = fs.root();
    let b = fs.lookup(root, b"b").expect("found");
    assert_eq!(fs.link(root, b"b2", b), Err(DriverError::NoSpace));
    assert_eq!(fs.node_info(b).expect("info").nlink, 1);
    assert_eq!(fs.lookup(root, b"b2"), Err(DriverError::NotFound));
    assert_eq!(fs.write_refusal(), None);
}

#[test]
fn a_rename_over_a_name_needs_no_room_in_the_destination() {
    let mut fs = named_and_full();
    let root = fs.root();
    let b = fs.lookup(root, b"b").expect("found");
    let c = fs.lookup(root, b"c").expect("found");
    // `c` has a second name, so replacing it frees no block.
    fs.rename(root, b"b", root, b"c").expect("renamed in place");
    assert_eq!(fs.lookup(root, b"c"), Ok(b));
    assert_eq!(fs.lookup(root, b"b"), Err(DriverError::NotFound));
    assert_eq!(fs.lookup(root, b"c2"), Ok(c));
    assert_eq!(fs.node_info(c).expect("info").nlink, 1);
    // Renaming one name of a node onto another changes nothing.
    let a = fs.lookup(root, b"a").expect("found");
    fs.rename(root, b"a", root, b"twin").expect("nothing to do");
    assert_eq!(fs.lookup(root, b"a"), Ok(a));
    assert_eq!(fs.lookup(root, b"twin"), Ok(a));
    assert_eq!(fs.node_info(a).expect("info").nlink, 2);
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    let root = fs.root();
    assert_eq!(read_all(&mut fs, a), b"the a file");
    assert_eq!(fs.lookup(root, b"c"), Ok(b));
}

/// Remount `fs` with the directory `dir` recording its descriptors inline
/// in its entry, each resealed to name the entry's block.
fn embedded_dir(mut fs: Udf<Disk>, dir: NodeId) -> Udf<Disk> {
    fs.flush().expect("flush");
    let addr = super::addr_of(dir).expect("addr");
    let node = fs.resolve(addr).expect("entry");
    let mapped = super::Mapped::of(fs.extents_of(&node).expect("extents")).expect("mapped");
    let mut content = vec![0u8; node.entry.size as usize];
    fs.read_content(&node, &mapped, 0, &mut content)
        .expect("reads");
    let mut at = 0;
    while at < content.len() {
        let len = super::dir::Fid::length(&content[at..]).expect("a descriptor");
        let fid = &mut content[at..at + len];
        let tag = tairix_fsprobe::udf::Tag::checked(fid).expect("sealed");
        tairix_fsprobe::udf::Tag {
            location: addr.block,
            ..tag
        }
        .seal(fid, len - 16)
        .expect("seal");
        at += len;
    }
    patched(fs, dir, |block| embed(block, &content))
}

#[test]
fn a_full_volume_still_removes_from_an_embedded_directory() {
    let (mut fs, _) = crowded_and_full();
    let root = fs.root();
    let fill = fs.lookup(root, b"fill").expect("found");
    let filled = fs.node_info(fill).expect("info").size;
    fs.truncate(fill, filled - 4 * 512)
        .expect("room for a directory");
    let dir = fs
        .create(root, b"inline", NodeKind::Directory, &DIR)
        .expect("mkdir");
    fs.create(dir, b"gone", NodeKind::RegularFile, &FILE)
        .expect("create");
    let mut fs = embedded_dir(fs, dir);
    let mut at = filled - 4 * 512;
    while fs.write_all(fill, at, &[2u8; 512]).is_ok() {
        at += 512;
    }
    assert_eq!(free(&mut fs), 0);
    fs.remove(dir, b"gone").expect("removed in place");
    assert_eq!(free(&mut fs), 1, "the removed entry's block");
    let entry = fs
        .resolve(super::addr_of(dir).expect("addr"))
        .expect("entry");
    assert_eq!(
        entry.entry.form,
        super::entry::AdForm::Embedded,
        "the directory took no block"
    );
    fs.flush().expect("flush");
    let mut fs = remount(fs);
    assert!(names(&mut fs, dir).is_empty());
}

#[test]
fn a_full_volume_punches_a_partial_hole_without_taking_a_block() {
    let (mut fs, mut content) = crowded_and_full();
    let root = fs.root();
    let a = fs.lookup(root, b"a").expect("found");
    fs.punch(a, 10 * 512, 3 * 512)
        .expect("a hole of whole blocks");
    content[10 * 512..13 * 512].fill(0);
    assert!(free(&mut fs) > 0, "the hole gave blocks back");
    let fill = fs.lookup(root, b"fill").expect("found");
    let mut at = fs.node_info(fill).expect("info").size;
    while fs.write_all(fill, at, &[2u8; 512]).is_ok() {
        at += 512;
    }
    assert_eq!(free(&mut fs), 0);
    // Both partial ends lie in the hole, which reads as zeros already.
    fs.punch(a, 10 * 512 + 100, 200).expect("nothing to zero");
    fs.punch(a, 9 * 512 + 500, 512)
        .expect("one end in data, one in the hole");
    content[9 * 512 + 500..10 * 512].fill(0);
    assert_eq!(free(&mut fs), 0);
    assert_eq!(read_all(&mut fs, a), content);
}

#[test]
fn a_sparing_table_is_read_no_larger_than_the_largest_accepted_and_sorted() {
    let mut fs = formatted(2048, 0x0201);
    let Some(super::layout::Map::Physical(physical)) = fs.partitions.maps.first().cloned() else {
        panic!("a formatted volume's one map is physical");
    };
    let sector = physical.start + physical.length - 40;
    let mut table = vec![0u8; 2048];
    table[17..35].copy_from_slice(b"*UDF Sparing Table");
    let entries: [(u32, u32); 4] = [(64, 9000), (0, 8000), (0xFFFF_FFF0, 0), (32, 7000)];
    table[48..50].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    for (index, (original, mapped)) in entries.iter().enumerate() {
        table[56 + index * 8..60 + index * 8].copy_from_slice(&original.to_le_bytes());
        table[60 + index * 8..64 + index * 8].copy_from_slice(&mapped.to_le_bytes());
    }
    let tag = tairix_fsprobe::udf::Tag {
        id: 0,
        version: 3,
        serial: 0,
        location: sector,
    };
    tag.seal(&mut table, 56 + entries.len() * 8 - 16)
        .expect("seal");
    let at = sector as usize * 2048;
    fs.dev.device_mut().bytes[at..at + 2048].copy_from_slice(&table);
    let mut map = [0u8; 64];
    map[42] = 1;
    map[44..48].copy_from_slice(&u32::MAX.to_le_bytes());
    map[48..52].copy_from_slice(&sector.to_le_bytes());
    let read = fs
        .sparing_table(&map, &physical)
        .expect("a table of the size its entries take");
    assert_eq!(read, [(0, 8000), (32, 7000), (64, 9000)]);
    let sparable = super::layout::Partitions {
        maps: vec![super::layout::Map::Sparable {
            physical,
            packet: 32,
            table: read,
        }],
    };
    let sector_of = |block| {
        sparable.sector(super::layout::LbAddr {
            block,
            partition: 0,
        })
    };
    assert_eq!(sector_of(33), Ok(7001));
    assert_eq!(sector_of(70), Ok(9006));
    assert_eq!(sector_of(100), Ok(u64::from(physical.start) + 100));
    assert_eq!(
        sparable.contiguous(
            super::layout::LbAddr {
                block: 20,
                partition: 0
            },
            100
        ),
        Ok(12),
        "as laid out up to the next packet remapped"
    );
}

#[test]
fn the_allocation_table_search_passes_over_a_candidate_it_cannot_read() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let vat = fs
        .create(root, b"vat", NodeKind::RegularFile, &FILE)
        .expect("create");
    let mut content = vec![0u8; 152];
    content[0..2].copy_from_slice(&152u16.to_le_bytes());
    for word in [10u32, 11, 12] {
        content.extend_from_slice(&word.to_le_bytes());
    }
    fs.write_all(vat, 0, &content).expect("write");
    let bad = fs
        .create(root, b"bad", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(bad, 0, &[1u8; 600]).expect("write");
    let fs = patched(fs, vat, |block| block[27] = super::entry::TYPE_VAT);
    let mut fs = patched(fs, bad, |block| {
        block[27] = super::entry::TYPE_VAT;
        let huge = (super::layout::MAX_VAT_ENTRIES as u64) * 4 + 1024;
        block[56..64].copy_from_slice(&huge.to_le_bytes());
    });
    let (vat_block, bad_block) = (
        super::addr_of(vat).expect("addr").block,
        super::addr_of(bad).expect("addr").block,
    );
    assert!(bad_block > vat_block && bad_block - vat_block < 32);
    assert_eq!(fs.find_vat(u64::from(bad_block), 0), Ok(vec![10, 11, 12]));
}

#[test]
fn whole_blocks_are_written_in_runs() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let run = fs
        .create(root, b"run", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.dev.device_mut().writes = 0;
    fs.write_all(run, 0, &pattern(128 * 512, 4)).expect("write");
    let writes = fs.dev.device().writes;
    assert!(writes < 16, "{writes} requests for 128 blocks");
}

#[test]
fn bytes_past_the_last_extent_read_as_the_hole_a_seek_reports() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let file = fs
        .create(root, b"short", NodeKind::RegularFile, &FILE)
        .expect("create");
    fs.write_all(file, 0, &[7u8; 512]).expect("write");
    let mut fs = patched(fs, file, |block| {
        block[56..64].copy_from_slice(&4096u64.to_le_bytes());
        block[64..72].copy_from_slice(&4096u64.to_le_bytes());
    });
    let mut expected = vec![7u8; 512];
    expected.resize(4096, 0);
    assert_eq!(read_all(&mut fs, file), expected);
    assert_eq!(fs.seek_region(file, 0, SeekRegion::Hole), Ok(Some(512)));
}

/// The descriptors of the directory `dir`, in order.
fn fids(fs: &mut Udf<Disk>, dir: NodeId) -> Vec<super::dir::Fid> {
    let node = fs
        .resolve(super::addr_of(dir).expect("addr"))
        .expect("entry");
    let mut reader = fs.dir_reader(&node).expect("a directory");
    let mut out = Vec::new();
    let mut at = 0;
    while let Some((fid, next)) = fs.next_fid(&node, &mut reader, at).expect("reads") {
        out.push(fid);
        at = next;
    }
    out
}

#[test]
fn a_new_directory_names_its_parent_by_the_parents_unique_id() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let outer = fs
        .create(root, b"outer", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let inner = fs
        .create(outer, b"inner", NodeKind::Directory, &DIR)
        .expect("mkdir");
    let outer_id = fs
        .resolve(super::addr_of(outer).expect("addr"))
        .expect("entry")
        .entry
        .unique_id;
    let inner_id = fs
        .resolve(super::addr_of(inner).expect("addr"))
        .expect("entry")
        .entry
        .unique_id;
    let listed = fids(&mut fs, inner);
    assert_eq!(
        listed[0].characteristics & super::dir::CHAR_PARENT,
        super::dir::CHAR_PARENT
    );
    assert_eq!(listed[0].unique(), outer_id & 0xFFFF_FFFF);
    let named = fids(&mut fs, outer);
    assert_eq!(named[1].unique(), inner_id & 0xFFFF_FFFF);
}

#[test]
fn a_descriptor_naming_another_block_or_covering_too_little_is_damage() {
    for edit in [0usize, 1] {
        let mut fs = formatted(512, 0x0201);
        let root = fs.root();
        let dir = fs
            .create(root, b"dir", NodeKind::Directory, &DIR)
            .expect("mkdir");
        fs.create(dir, b"child", NodeKind::RegularFile, &FILE)
            .expect("create");
        fs.flush().expect("flush");
        let node = fs
            .resolve(super::addr_of(dir).expect("addr"))
            .expect("entry");
        let extent = fs.extents_of(&node).expect("extents")[0];
        let sector = fs.partitions.sector(extent.start).expect("sector") as usize;
        let mut disk = fs.into_device();
        let fid = &mut disk.bytes[sector * 512 + 40..sector * 512 + 40 + 44];
        let tag = tairix_fsprobe::udf::Tag::checked(fid).expect("sealed");
        if edit == 0 {
            tairix_fsprobe::udf::Tag {
                location: tag.location + 1,
                ..tag
            }
            .seal(fid, 44 - 16)
            .expect("seal");
        } else {
            tag.seal(fid, 4).expect("seal");
        }
        let mut fs = Udf::open(disk).expect("mounts");
        assert_eq!(
            listed(&mut fs, dir, 0, &[]).map(|list| list.len()),
            Err(DriverError::DeviceFault),
            "{edit}"
        );
    }
}

#[test]
fn region_queries_reuse_the_extents_a_read_mapped() {
    let mut fs = formatted(512, 0x0201);
    let root = fs.root();
    let a = fs
        .create(root, b"a", NodeKind::RegularFile, &FILE)
        .expect("create");
    let b = fs
        .create(root, b"b", NodeKind::RegularFile, &FILE)
        .expect("create");
    for i in 0..120u64 {
        fs.write_all(a, i * 512, &[1u8; 512]).expect("a grows");
        fs.write_all(b, i * 512, &[2u8; 512]).expect("b grows");
    }
    fs.seek_region(a, 0, SeekRegion::Hole)
        .expect("maps the extents");
    fs.dev.device_mut().reads = 0;
    fs.seek_region(a, 512, SeekRegion::Data).expect("seeks");
    fs.seek_region(a, 1024, SeekRegion::Hole).expect("seeks");
    assert_eq!(
        fs.dev.device().reads,
        2,
        "the entry, once a query, and no continuation"
    );
}

#[test]
fn the_served_driver_states_a_refusal_only_for_a_volume_it_was_asked_to_write() {
    use tairix_fsserve::{ServedFormat, WritebackBudget};
    let budget = WritebackBudget {
        backing: 1 << 30,
        gauge: &tairix_reclaim::Unpressured,
    };
    let writable = crate::Served::open(formatted(512, 0x0201).into_device(), false, budget)
        .expect("opens writable");
    assert_eq!(writable.format, tairix_abi::volume::FormatName::Udf);
    assert_eq!(writable.write_refusal, None);
    let read_only = crate::Served::open(formatted(512, 0x0201).into_device(), true, budget)
        .expect("opens read-only");
    assert_eq!(read_only.write_refusal, None);
}
