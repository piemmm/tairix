//! A RISC OS member's words, read into its metadata and written back.

use tairix_abi::driver::filesystem::{NodeKind, NodeSecurity};
use tairix_abi::time::Time64;
use tairix_fsmeta::preset::acorn::{self, Filetype};

use super::{meta_of, words_of, Words};

/// 2000-01-01 00:00:00 UTC, in centiseconds since 1900.
const Y2K: u64 = 3_155_673_600 * 100;

fn typed(filetype: u32, centiseconds: u64) -> Words {
    Words {
        load: 0xFFF0_0000 | filetype << 8 | u32::try_from(centiseconds >> 32).unwrap(),
        exec: u32::try_from(centiseconds & 0xFFFF_FFFF).unwrap(),
        attr: 0x13,
    }
}

#[test]
fn a_typed_members_stamp_is_its_modification_time() {
    let meta = meta_of(
        Some(typed(0xFFF, Y2K + 50)),
        Some(Time64::from_secs(7)),
        NodeKind::RegularFile,
    );
    assert_eq!(
        meta.times.modified,
        Time64::new(946_684_800, 500_000_000).unwrap()
    );
    assert_eq!(
        meta.mode,
        NodeSecurity::unrecorded(NodeKind::RegularFile).mode
    );
    assert_eq!(meta.owner, None);
    let object = meta.acorn.unwrap();
    assert_eq!(object.attr, acorn::attr_from_fileswitch(0x13));
    let directory = meta_of(Some(typed(0xDDC, Y2K)), None, NodeKind::Directory);
    assert_ne!(directory.acorn.unwrap().attr & acorn::ATTR_DIRECTORY, 0);
}

#[test]
fn an_untyped_member_keeps_its_addresses_and_takes_its_time_elsewhere() {
    let words = Words {
        load: 0x0000_8000,
        exec: 0x0000_8000,
        attr: 0x03,
    };
    let meta = meta_of(
        Some(words),
        Some(Time64::from_secs(1_000_000_000)),
        NodeKind::RegularFile,
    );
    assert_eq!(meta.times.modified, Time64::from_secs(1_000_000_000));
    assert_eq!(words_of(&meta, Filetype::DATA), Ok(words));
    let none = meta_of(None, None, NodeKind::RegularFile);
    assert_eq!(none.acorn, None);
    assert_eq!(none.times.modified, Time64::UNIX_EPOCH);
}

#[test]
fn words_are_restamped_from_the_modification_time_or_typed_afresh() {
    let mut meta = meta_of(Some(typed(0xFEB, Y2K)), None, NodeKind::RegularFile);
    meta.times.modified = Time64::new(946_684_801, 250_000_000).unwrap();
    assert_eq!(words_of(&meta, Filetype::DATA), Ok(typed(0xFEB, Y2K + 125)));
    let mut fresh = meta_of(None, None, NodeKind::RegularFile);
    fresh.times.modified = Time64::new(946_684_800, 0).unwrap();
    let written = words_of(&fresh, Filetype::DATA).unwrap();
    assert_eq!(
        (written.load, written.exec),
        (typed(0xFFD, Y2K).load, typed(0xFFD, Y2K).exec)
    );
    assert_eq!(
        written.attr,
        acorn::attr_to_fileswitch(acorn::ATTR_OWNER_READ | acorn::ATTR_OWNER_WRITE)
    );
    // Before 1900 no stamp can hold it.
    fresh.times.modified = Time64::from_secs(-2_208_988_801);
    assert!(words_of(&fresh, Filetype::DATA).is_err());
}
