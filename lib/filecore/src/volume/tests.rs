//! Objects placed as `FileCore` places them, held to the saves RISC OS 5 was
//! recorded making on a 256 KiB RAM disc, and a shared file's move to another
//! directory.

use tairix_fsmeta::preset::acorn::ATTR_DIRECTORY;

use super::{Backing, Volume, EMPTY_OBJECT};
use crate::dir::Object;
use crate::format::{lay_out, new_map_record, BootRecord, Variant};
use crate::newmap::NewMap;
use crate::testkit::MemDisc;

fn ram_disc(size: u64) -> Volume<MemDisc> {
    let record = new_map_record(Variant::RamDisc, size).expect("a record");
    let mut disc = MemDisc::new(usize::try_from(size).expect("a test size"));
    let record = lay_out(&mut disc, record, BootRecord::Blank).expect("laid out");
    let map = NewMap::open(&mut disc, record).expect("its map");
    Volume::open(
        disc,
        Backing::New {
            map,
            boot_block: record.nzones > 1,
        },
    )
    .expect("its root")
}

fn free(volume: &mut Volume<MemDisc>) -> u64 {
    let space = volume.space().expect("its space");
    u64::from(space.unit) * space.free
}

/// `name`, `size` bytes, placed and entered in the directory at `parent`.
fn save(volume: &mut Volume<MemDisc>, parent: u32, name: &[u8], size: u32, dir: bool) -> u32 {
    let indaddr = if dir {
        let (indaddr, bytes) = volume.new_directory(parent, name).expect("a directory");
        assert_eq!(bytes, size, "a new directory's size");
        indaddr
    } else {
        volume
            .place_object(parent, u64::from(size))
            .expect("placed")
    };
    let mut object = Object::named(name).expect("a name");
    object.size = size;
    object.indaddr = indaddr;
    if dir {
        object.attr = ATTR_DIRECTORY;
    }
    volume.dir_insert(parent, &object).expect("entered");
    indaddr
}

#[test]
fn small_files_share_fragments_as_risc_os_5_shared_them() {
    let mut volume = ram_disc(0x4_0000);
    let root = volume.root_addr();
    assert_eq!(free(&mut volume), 0x3_F000);
    for (name, size, dir, indaddr, after) in [
        (&b"A"[..], 100, false, 0x401, 0x3_E800),
        (b"B", 1000, false, 0x402, 0x3_E800),
        (b"C", 2000, false, 0x500, 0x3_E000),
        (b"D", 5000, false, 0x600, 0x3_CC00),
        (b"F", 1, false, 0x701, 0x3_C400),
        (b"G", 0, false, EMPTY_OBJECT, 0x3_C400),
        (b"E", 0x800, true, 0x800, 0x3_BC00),
    ] {
        assert_eq!(
            save(&mut volume, root, name, size, dir),
            indaddr,
            "{name:?}"
        );
        assert_eq!(free(&mut volume), after, "{name:?}");
    }
    assert!(
        volume.holds_nothing(EMPTY_OBJECT),
        "an empty file holds nothing"
    );
    assert_eq!(volume.object_allocated(0x401, 100).expect("A's"), 0x400);
    assert_eq!(volume.object_allocated(0x600, 5000).expect("D's"), 0x1400);
}

#[test]
fn a_shared_file_moved_to_another_directory_leaves_its_fragment_behind() {
    let mut volume = ram_disc(0x4_0000);
    let root = volume.root_addr();
    let a = save(&mut volume, root, b"A", 100, false);
    let b = save(&mut volume, root, b"B", 1000, false);
    let sub = save(&mut volume, root, b"Sub", 0x800, true);
    volume
        .object_write(a, 0, b"the first file")
        .expect("A written");
    volume
        .object_write(b, 0, b"the second file")
        .expect("B written");
    let (index, moving) = volume.dir_lookup(root, b"A").expect("read").expect("A");
    let moved = volume
        .copy_for_move(&moving, root, sub)
        .expect("copied")
        .expect("a shared file moves out of its fragment");
    assert_ne!(moved >> 8, a >> 8, "a fragment of its own directory's");
    volume.dir_remove_at(root, index).expect("left");
    let mut entry = moving;
    entry.indaddr = moved;
    volume.dir_insert(sub, &entry).expect("entered");
    volume
        .release_maybe_shared(a, moving.size, root)
        .expect("its old granules let go");
    let (_, staying) = volume.dir_lookup(root, b"B").expect("read").expect("B");
    assert_eq!(
        volume.copy_for_move(&staying, root, root),
        Ok(None),
        "no move, no copy"
    );
    let (index, gone) = volume.dir_lookup(root, b"B").expect("read").expect("B");
    volume
        .release_maybe_shared(gone.indaddr, gone.size, root)
        .expect("B let go");
    volume.dir_remove_at(root, index).expect("B removed");
    save(&mut volume, root, b"Fill", 0x3_0000, false);
    let mut read = [0u8; 14];
    volume.object_read(moved, 0, &mut read).expect("A read");
    assert_eq!(
        &read, b"the first file",
        "B's going freed nothing A still uses"
    );
}
