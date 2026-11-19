//! Objects placed as `FileCore` places them, held to the saves RISC OS 5 was
//! recorded making on a 256 KiB RAM disc, a shared file's move to another
//! directory, what holds a byte of the disc, defects, and compaction.

use alloc::vec::Vec;

use tairix_fsmeta::preset::acorn::ATTR_DIRECTORY;

use super::{Backing, Defect, Holder, Recorded, Volume, EMPTY_OBJECT};
use crate::dir::{FixedFormat, Object};
use crate::format::{format_volume, lay_out, new_map_record, BootRecord, Variant};
use crate::identify::{identify, Layout};
use crate::newmap::NewMap;
use crate::record::{
    zone_check, MapCopy, DISC_RECORD_SIZE, FRAG_BAD, FRAG_ROOT, ZONE_CHECK_BYTE, ZONE_HEADER_SIZE,
};
use crate::testkit::MemDisc;
use crate::{Error, Fault, Structure};

fn ram_disc(size: u64) -> Volume<MemDisc> {
    laid_out(Variant::RamDisc, size, BootRecord::Blank)
}

/// A `variant` disc of `size` bytes, laid out and opened.
fn laid_out(variant: Variant, size: u64, boot: BootRecord) -> Volume<MemDisc> {
    let record = new_map_record(variant, size).expect("a record");
    let mut disc = MemDisc::new(usize::try_from(size).expect("a test size"));
    let record = lay_out(&mut disc, record, boot, b"$").expect("laid out");
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

/// The root's file `name` deleted.
fn delete(volume: &mut Volume<MemDisc>, name: &[u8]) {
    let root = volume.root_addr();
    let (index, object) = volume.dir_lookup(root, name).expect("read").expect("there");
    volume.dir_remove_at(root, index).expect("removed");
    volume
        .release_maybe_shared(object.indaddr, object.size, root)
        .expect("let go");
}

#[test]
fn what_holds_a_byte_is_the_fragment_an_entry_names() {
    let mut volume = ram_disc(0x4_0000);
    let root = volume.root_addr();
    let a = save(&mut volume, root, b"A", 0x800, false);
    let (_, entry) = volume.dir_lookup(root, b"A").expect("read").expect("A");
    let holder = volume.holder_at(0x1400).expect("read");
    assert_eq!(holder, Holder::Fragment(a >> 8));
    assert!(holder.holds(&entry));
    assert_eq!(
        volume.holder_at(0).expect("read"),
        Holder::Fragment(2),
        "the map"
    );
    assert_eq!(volume.holder_at(0x2000).expect("read"), Holder::Free);
    assert_eq!(
        volume.holder_at(0x4_0000).expect("read"),
        Holder::Defect,
        "past the disc"
    );
    assert_eq!(
        volume.holder_at(0xFFFF_FFFF).expect("read"),
        Holder::Free,
        "past the map"
    );
}

#[test]
fn a_defect_is_mapped_out_of_free_space_as_risc_os_5_maps_one() {
    for (first, defect, lost) in [
        (0x800, 0x1400, 0x800),
        (0xC00, 0x1000, 0xC00),
        (0xC00, 0x1800, 0xC00),
        (0x1800, 0x1800, 0x800),
    ] {
        let mut volume = ram_disc(0x4_0000);
        let root = volume.root_addr();
        save(&mut volume, root, b"A", first, false);
        save(&mut volume, root, b"B", 0x800, false);
        delete(&mut volume, b"A");
        let before = free(&mut volume);
        volume.add_defect(defect).expect("mapped out");
        assert_eq!(
            before - free(&mut volume),
            lost,
            "{defect:#X} after {first:#X}"
        );
        assert_eq!(volume.holder_at(defect).expect("read"), Holder::Defect);
        volume.add_defect(defect).expect("again");
        assert_eq!(before - free(&mut volume), lost, "a repeat changes nothing");
    }
}

#[test]
fn a_disc_without_a_boot_block_lists_its_maps_defects_a_sector_at_a_time() {
    let mut volume = ram_disc(0x4_0000);
    volume.add_defect(0x2_0001).expect("mapped out");
    let mut listed = Vec::new();
    volume
        .defects(|defect| {
            listed.push(defect);
            listed.len() < 4
        })
        .expect("listed");
    let at = |address| Defect {
        address,
        recorded: Recorded::Map,
    };
    assert_eq!(
        listed,
        [at(0x2_0000), at(0x2_0400), at(0x4_0000), at(0x4_0400)]
    );
}

#[test]
fn a_boot_block_lists_its_defects_and_refuses_one_in_use() {
    let mut volume = ram_disc(0x10_0000);
    let root = volume.root_addr();
    let before = free(&mut volume);
    for address in [0x9_0400, 0x8_0000, 0x10_0000] {
        volume.add_defect(address).expect("recorded");
    }
    assert_eq!(
        before - free(&mut volume),
        0x1000,
        "two mapped out, one past the disc"
    );
    let file = save(&mut volume, root, b"F", 0x2000, false);
    let (inside, _) = volume.locate(file, 0x400).expect("placed");
    assert_eq!(
        volume.add_defect(inside),
        Err(Error::Fault(Fault::DefectInUse))
    );
    assert_eq!(
        volume.add_defect(0x2000_0000),
        Err(Error::Fault(Fault::BadDefect))
    );
    let mut listed = Vec::new();
    volume
        .defects(|defect| {
            listed.push((defect.address, defect.recorded));
            true
        })
        .expect("listed");
    assert_eq!(
        listed,
        [
            (0x8_0000, Recorded::First),
            (0x9_0400, Recorded::First),
            (0x10_0000, Recorded::First)
        ]
    );
}

/// A small generator for the layouts compaction is tried on.
fn next(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    *seed >> 33
}

#[test]
fn compaction_keeps_every_file_and_ends_where_another_would_move_nothing() {
    for mut seed in 0..64u64 {
        let mut volume = ram_disc(0x4_0000);
        let root = volume.root_addr();
        let mut kept = Vec::new();
        for n in 0..12u8 {
            let name = [b'A' + n];
            let size = 0x200 * (1 + u32::try_from(next(&mut seed) % 12).expect("small"));
            let indaddr = save(&mut volume, root, &name, size, false);
            volume.object_write(indaddr, 0, &[n; 16]).expect("written");
            kept.push((name, n));
        }
        kept.retain(|&(name, _)| {
            let gone = next(&mut seed).is_multiple_of(3);
            if gone {
                delete(&mut volume, &name);
            }
            !gone
        });
        let before = free(&mut volume);
        volume.compact().expect("compacted");
        assert_eq!(
            free(&mut volume),
            before,
            "seed {seed}: no space made or lost"
        );
        assert_eq!(volume.compact(), Ok(false), "seed {seed}: settled");
        for (name, n) in kept {
            let (_, object) = volume.dir_lookup(root, &name).expect("read").expect("kept");
            let mut read = [0u8; 16];
            volume
                .object_read(object.indaddr, 0, &mut read)
                .expect("read");
            assert_eq!(read, [n; 16], "seed {seed}: {}", char::from(name[0]));
        }
    }
}

#[test]
fn a_big_directory_keeps_its_names_and_disc_addresses_in_entry_order() {
    let mut volume = ram_disc(0x4_0000);
    let root = volume.root_addr();
    let dir = save(&mut volume, root, b"Ab", 0x800, true);
    for name in [&b"One"[..], b"Two", b"Three"] {
        save(&mut volume, dir, name, 16, false);
    }
    let mut bytes = [0u8; 0x800];
    volume.object_read(dir, 0, &mut bytes).expect("read");
    let heap = 0x20 + 3 * 28;
    assert_eq!(
        &bytes[heap..heap + 16],
        b"One\rThree\r\0\0Two\r",
        "in entry order"
    );
    let addresses: Vec<u32> = (0..3)
        .map(|n| {
            let at = 0x20 + n * 28 + 12;
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        })
        .collect();
    let table: Vec<u32> = bytes[0x800 - 8 - 12..0x800 - 8]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&w| u32::from_le_bytes(w))
        .collect();
    assert_eq!(table, addresses, "each entry's disc address below the tail");
}

#[test]
fn a_small_file_never_shares_the_maps_own_fragment() {
    for (variant, size, boot) in [
        (Variant::E, 0xC_8000, BootRecord::Blank),
        (Variant::F, 0x19_0000, BootRecord::Whole),
    ] {
        let mut volume = laid_out(variant, size, boot);
        let root = volume.root_addr();
        assert_eq!(root >> 8, FRAG_ROOT, "{variant:?}: the root shares it");
        let small = save(&mut volume, root, b"Small", 100, false);
        assert_ne!(small >> 8, FRAG_ROOT, "{variant:?}");
        volume
            .object_write(small, 0, &[0xA5; 100])
            .expect("written");
        let Backing::New { map, .. } = &volume.backing else {
            panic!("{variant:?} has a new map");
        };
        let record = map.record;
        NewMap::open(&mut volume.disc, record).expect("the map as it was");
        volume.defects(|_| true).expect("the boot block as it was");
    }
}

#[test]
fn an_address_in_the_discs_own_structures_names_no_object() {
    let refused = Err(Error::Fault(Fault::Broken(Structure::Directory)));
    let (defects, map) = (FRAG_BAD << 8, FRAG_ROOT << 8);
    let mut volume = laid_out(Variant::F, 0x19_0000, BootRecord::Whole);
    let root = volume.root_addr();
    let before = volume.disc.0.clone();
    let mut byte = [0u8; 1];
    assert_eq!(volume.object_write(defects | 1, 0, &[0]), refused);
    assert_eq!(volume.object_read(map, 0, &mut byte), refused, "the map's");
    assert_eq!(
        volume.object_write(map | 1, 0, &[0]),
        refused,
        "the map, ahead of the root"
    );
    let Backing::New { map: engine, .. } = &volume.backing else {
        panic!("a new map");
    };
    let whole = engine
        .object_allocated_bytes(&mut volume.disc, FRAG_ROOT)
        .expect("its size");
    let last = whole - engine.record.share_base(root) - 1;
    assert_eq!(
        volume.object_write(root, last, &[0]),
        refused,
        "the boot area, past the root's piece"
    );
    assert_eq!(volume.free(map | 1, 100), refused, "what shares the root's");
    assert_eq!(volume.free(defects, 0x800), refused);
    assert_eq!(volume.shrink_object(map, 0x1000, 0), refused);
    assert_eq!(
        volume.grow_object(defects, 0x800, 0x1000, root).err(),
        refused.err()
    );
    assert!(volume.disc.0 == before, "nothing changed");
    volume
        .object_read(root, 0, &mut byte)
        .expect("the root itself");

    let mut disc = MemDisc::new(0xA_0000);
    format_volume(&mut disc, Variant::L).expect("formatted");
    let Some(Layout::OldMap { root_sector, .. }) = identify(&mut disc).expect("read") else {
        panic!("an old map");
    };
    let mut volume = Volume::open(
        disc,
        Backing::Old {
            format: FixedFormat::Old,
            root_sector,
        },
    )
    .expect("its root");
    assert_eq!(volume.object_write(1, 0, &[0]), refused, "the map");
    assert_eq!(
        volume.free(root_sector + 1, 0x100),
        refused,
        "the root's own"
    );
    volume
        .object_read(0, 0, &mut [])
        .expect("an empty object's empty read touches nothing");
    volume
        .object_read(root_sector, 0, &mut byte)
        .expect("the root itself");
}

#[test]
fn a_map_leaving_free_space_past_the_disc_is_refused() {
    let size = 0x4_0000;
    let laid = new_map_record(Variant::RamDisc, size).expect("a record");
    let mut disc = MemDisc::new(usize::try_from(size).expect("a test size"));
    let mut record = lay_out(&mut disc, laid, BootRecord::Blank, b"$").expect("laid out");
    record.disc_size -= 0x8000;
    for copy in [MapCopy::First, MapCopy::Second] {
        let at = usize::try_from(record.zone_offset(copy, 0)).expect("on the disc");
        let zone = &mut disc.0[at..at + 0x400];
        zone[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]
            .copy_from_slice(&record.encode());
        zone[ZONE_CHECK_BYTE] = zone_check(zone);
    }
    assert_eq!(
        NewMap::open(&mut disc, record).err(),
        Some(Error::Fault(Fault::Broken(Structure::Map)))
    );
}

#[test]
fn a_big_directory_name_padded_past_its_heap_is_not_removed() {
    let mut volume = ram_disc(0x4_0000);
    let root = volume.root_addr();
    save(&mut volume, root, b"Four", 16, false);
    let mut header = [0u8; 28];
    volume.object_read(root, 0, &mut header).expect("read");
    let size = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    // The name's four bytes, but not the return and padding after them.
    header[20..24].copy_from_slice(&4u32.to_le_bytes());
    volume.object_write(root, 0, &header).expect("written");
    volume
        .object_write(root, u64::from(size) - 1, &[0])
        .expect("its check byte left unchecked");
    let Backing::New { map, boot_block } = volume.backing else {
        panic!("a new map");
    };
    let mut volume = Volume::open(
        MemDisc(volume.disc.0.clone()),
        Backing::New { map, boot_block },
    )
    .expect("its root");
    assert_eq!(
        volume.dir_remove_at(root, 0),
        Err(Error::Fault(Fault::Broken(Structure::Directory)))
    );
}
