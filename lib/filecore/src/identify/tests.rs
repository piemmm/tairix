//! Identification as `FileCore` identifies: the boot block first, a bare
//! record next, the old map last, and a RAM disc's name read from its map.

use super::{identify, trimmed, Layout};
use crate::format::{format_volume, lay_out, new_map_record, BootRecord, Variant};
use crate::testkit::MemDisc;

#[test]
fn every_format_is_found_where_filecore_finds_it() {
    for (variant, bytes) in [
        (Variant::S, 160 * 1024),
        (Variant::D, 800 * 1024),
        (Variant::E, 800 * 1024),
        (Variant::F, 1600 * 1024),
        (Variant::HardDiscPlus, 8 << 20),
    ] {
        let mut disc = MemDisc::new(bytes);
        format_volume(&mut disc, variant).expect("formatted");
        let found = identify(&mut disc).expect("no fault").expect("found");
        match (variant.is_old_map(), found) {
            (
                true,
                Layout::OldMap {
                    large_directories, ..
                },
            ) => {
                assert_eq!(large_directories, variant == Variant::D, "{variant:?}");
            }
            (false, Layout::NewMap { boot, record }) => {
                assert_eq!(boot.is_some(), record.nzones > 1, "{variant:?}");
            }
            (_, found) => panic!("{variant:?} found as {found:?}"),
        }
    }
    assert_eq!(
        identify(&mut MemDisc::new(4096)),
        Ok(None),
        "nothing to find"
    );
    assert_eq!(
        identify(&mut MemDisc::new(64)),
        Ok(None),
        "too short to hold anything"
    );
}

#[test]
fn a_ram_disc_is_named_by_its_map_though_its_boot_block_names_nothing() {
    let mut record = new_map_record(Variant::RamDisc, 0x10_0000).expect("a record");
    record.disc_name = *b"RamDisc0\0\0";
    let mut disc = MemDisc::new(0x10_0000);
    let laid = lay_out(&mut disc, record, BootRecord::Blank).expect("laid out");
    let found = identify(&mut disc).expect("no fault").expect("found");
    let Layout::NewMap { record, boot } = &found else {
        panic!("a new map: {found:?}");
    };
    assert_eq!(*record, laid);
    let boot = boot.expect("found through its boot block");
    assert_eq!(
        (boot.root_size, boot.disc_id),
        (0, 0),
        "the boot block names nothing"
    );
    let (label, len) = found.label();
    assert_eq!(&label[..len], b"RamDisc0");
}

#[test]
fn a_name_ends_at_its_padding_or_the_first_byte_no_name_holds() {
    assert_eq!(trimmed(*b"Games     ").1, 5);
    assert_eq!(trimmed(*b"RamDisc0\0\0").1, 8);
    assert_eq!(trimmed(*b"Ab\x07cdefghi").1, 2);
    assert_eq!(trimmed([b' '; 10]).1, 0);
}
