//! Discs as RISC OS writes them: small files sharing a fragment, which a move
//! to another directory may not take along, and empty files numbered as RISC
//! OS 5 numbers them, which are told apart by name.

use super::*;
use tairix_filecore::dir::Object;
use tairix_filecore::volume::EMPTY_OBJECT;

/// A file of `size` bytes named `name` in the directory at `parent`, placed
/// as `FileCore` places it — sharing a fragment where it is small — as RISC
/// OS would have written it.
fn placed(fs: &mut Adfs<MemDisk>, parent: u32, name: &[u8], size: u32, data: &[u8]) -> u32 {
    let indaddr = fs
        .volume
        .place_object(parent, u64::from(size))
        .expect("placed");
    if !data.is_empty() {
        fs.volume.object_write(indaddr, 0, data).expect("written");
    }
    let mut object = Object::named(name).expect("a name");
    object.size = size;
    object.indaddr = indaddr;
    fs.volume.dir_insert(parent, &object).expect("entered");
    indaddr
}

#[test]
fn a_shared_file_moved_away_keeps_its_bytes_when_its_sharers_go() {
    let mut fs = fresh(Variant::EPlus, 800 * 1024);
    let root = fs.root();
    let root_addr = fs.volume.root_addr();
    let first = placed(
        &mut fs,
        root_addr,
        b"First",
        40,
        b"the first file's own forty bytes, kept..",
    );
    let second = placed(&mut fs, root_addr, b"Second", 40, &[0x5A; 40]);
    assert_eq!(first >> 8, second >> 8, "the two share a fragment");
    fs.create(root, b"Sub", NodeKind::Directory, &DIR_RECORD)
        .expect("a directory");
    let sub = fs.lookup(root, b"Sub").expect("it resolves");
    fs.rename(root, b"First", sub, b"First").expect("moved");
    fs.remove(root, b"Second").expect("its sharer removed");
    let stats = fs.stats().expect("stats");
    let free = stats.free_blocks * u64::from(stats.block_size);
    let filler = make(
        &mut fs,
        root,
        b"Filler",
        &vec![0xC3; usize::try_from(free).expect("a test size") - 4096],
    );
    assert!(fs.node_info(filler).is_ok());
    let moved = fs.lookup(sub, b"First").expect("it moved");
    assert_eq!(
        read_all(&mut fs, moved),
        b"the first file's own forty bytes, kept..",
        "nothing it still used was handed out again"
    );
}

#[test]
fn empty_files_risc_os_5_numbered_alike_are_told_apart() {
    let mut fs = fresh(Variant::EPlus, 800 * 1024);
    let root = fs.root();
    let root_addr = fs.volume.root_addr();
    for name in [&b"Alpha"[..], b"Beta"] {
        assert_eq!(placed(&mut fs, root_addr, name, 0, b""), EMPTY_OBJECT);
    }
    let alpha = fs.lookup(root, b"Alpha").expect("Alpha");
    let beta = fs.lookup(root, b"Beta").expect("Beta");
    assert_ne!(alpha, beta, "two empty files are two nodes");
    assert_eq!(fs.node_info(alpha).expect("Alpha's").size, 0);
    assert_eq!(fs.read_at(beta, 0, &mut [0u8; 8]), Ok(0));
    fs.write_at(alpha, 0, b"now it holds bytes")
        .expect("written");
    let alpha = fs.lookup(root, b"Alpha").expect("Alpha again");
    assert_eq!(read_all(&mut fs, alpha), b"now it holds bytes");
    assert_eq!(fs.node_info(beta).expect("Beta's").size, 0);
}
