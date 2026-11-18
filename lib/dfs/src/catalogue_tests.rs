use super::*;

fn file(name: &[u8], dir: u8, start: u16, length: u32) -> Entry {
    Entry {
        name: crate::name::spelled(name).unwrap(),
        dir,
        locked: false,
        load: 0x3_1900,
        exec: 0x3_8023,
        length,
        start,
    }
}

fn written(catalogue: &Catalogue) -> [u8; CATALOGUE_LEN] {
    let mut bytes = [0xEE; CATALOGUE_LEN];
    catalogue.write(&mut bytes);
    bytes
}

#[test]
fn a_blank_catalogue_is_the_one_dfs_formats() {
    let bytes = written(&Catalogue::blank(800));
    assert_eq!(&bytes[..8], b"        ");
    assert_eq!(&bytes[256..260], b"    ");
    assert_eq!(bytes[256 + usize::from(END)], 0);
    assert_eq!(bytes[256 + usize::from(OPTION)], 3);
    assert_eq!(bytes[256 + usize::from(SECTORS)], 0x20);
    assert!(bytes[8..256].iter().all(|&byte| byte == 0));
    assert_eq!(Catalogue::read(&bytes), Some(Catalogue::blank(800)));
}

#[test]
fn an_entry_s_eighteen_bit_fields_round_trip_through_its_mixed_byte() {
    let entry = Entry {
        name: *b"PROG   ",
        dir: b'B',
        locked: true,
        load: 0x2_3456,
        exec: 0x1_789A,
        length: 0x3_BCDE,
        start: 0x2F1,
    };
    let (names, info) = entry.encode();
    assert_eq!(&names, b"PROG   \xC2");
    assert_eq!(
        info,
        [0x56, 0x34, 0x9A, 0x78, 0xDE, 0xBC, 0b01_11_10_10, 0xF1]
    );
    assert_eq!(Entry::decode(&names, &info), entry);
}

#[test]
fn a_catalogue_round_trips_its_title_cycle_option_and_entries() {
    let mut catalogue = Catalogue::blank(400);
    catalogue.title = *b"GAMES DISC 2";
    catalogue.cycle = 0x47;
    catalogue.boot = 3;
    let files = [file(b"B", b'$', 20, 0x1000), file(b"!BOOT", b'$', 2, 13)];
    catalogue.set_files(&files).unwrap();
    let bytes = written(&catalogue);
    assert_eq!(&bytes[..8], b"GAMES DI");
    assert_eq!(&bytes[256..260], b"SC 2");
    assert_eq!(bytes[256 + usize::from(OPTION)], 0x31);
    assert_eq!(bytes[256 + usize::from(END)], 16);
    let read = Catalogue::read(&bytes).unwrap();
    assert_eq!(read, catalogue);
    assert_eq!(read.files(), &files);
}

#[test]
fn a_catalogue_holds_at_most_thirty_one_files() {
    let mut catalogue = Catalogue::blank(800);
    let files: [Entry; 32] = core::array::from_fn(|n| {
        let start = 2 + u16::try_from(n).unwrap();
        file(b"F", b'$', start, 1)
    });
    assert_eq!(catalogue.set_files(&files), None);
    assert!(catalogue.files().is_empty());
    catalogue.set_files(&files[..31]).unwrap();
    let bytes = written(&catalogue);
    assert_eq!(bytes[256 + usize::from(END)], FULL_END);
    assert_eq!(Catalogue::read(&bytes).unwrap().files().len(), 31);
}

#[test]
fn what_no_dfs_disc_holds_is_not_believed() {
    let mut catalogue = Catalogue::blank(800);
    catalogue.set_files(&[file(b"OK", b'$', 2, 300)]).unwrap();
    let good = written(&catalogue);
    assert!(Catalogue::read(&good).is_some());
    let refused = |change: &dyn Fn(&mut [u8; CATALOGUE_LEN])| {
        let mut bytes = good;
        change(&mut bytes);
        Catalogue::read(&bytes)
    };
    // An end that is not a whole number of entries.
    assert_eq!(refused(&|b| b[256 + usize::from(END)] = 9), None);
    // Option bits DFS 1.20 leaves clear.
    assert_eq!(refused(&|b| b[256 + usize::from(OPTION)] |= 0x40), None);
    assert_eq!(refused(&|b| b[256 + usize::from(OPTION)] |= 0x04), None);
    // A side too small for its own catalogue.
    assert_eq!(
        refused(&|b| {
            b[256 + usize::from(OPTION)] = 0;
            b[256 + usize::from(SECTORS)] = 1;
        }),
        None
    );
    // A title holding a control code.
    assert_eq!(refused(&|b| b[3] = 0x0D), None);
    // A name beginning with its padding, or holding a control code.
    assert_eq!(refused(&|b| b[8] = b' '), None);
    assert_eq!(refused(&|b| b[9] = 0x07), None);
    // A directory that is a space.
    assert_eq!(refused(&|b| b[15] = b' '), None);
    // A file in the catalogue's sectors, or past the side's end.
    assert_eq!(refused(&|b| b[256 + 15] = 1), None);
    assert_eq!(
        refused(&|b| {
            b[256 + 14] |= 3;
            b[256 + 15] = 0xFF;
        }),
        None
    );
    // A title of teletext codes and a name whose top bits are set are DFS's.
    assert!(refused(&|b| b[0] = 0x81).is_some());
    assert!(refused(&|b| b[8] |= 0x80).is_some());
}

#[test]
fn the_cycle_steps_in_bcd_and_wraps() {
    assert_eq!(next_cycle(0x00), 0x01);
    assert_eq!(next_cycle(0x09), 0x10);
    assert_eq!(next_cycle(0x47), 0x48);
    assert_eq!(next_cycle(0x99), 0x00);
}

#[test]
fn a_file_takes_its_length_in_whole_sectors() {
    assert_eq!(file(b"E", b'$', 2, 0).sectors(), 0);
    assert_eq!(file(b"E", b'$', 2, 1).sectors(), 1);
    assert_eq!(file(b"E", b'$', 2, 256).sectors(), 1);
    assert_eq!(file(b"E", b'$', 2, 257).sectors(), 2);
    assert_eq!(file(b"E", b'$', 2, MAX_FIELD).sectors(), 1024);
    assert_eq!(file(b"E", b'$', 7, 513).end(), 10);
}

#[test]
fn an_address_field_widens_to_the_os_address_and_back() {
    assert_eq!(address(0x1900), 0x1900);
    assert_eq!(address(0x2_1900), 0x2_1900);
    assert_eq!(address(0x3_1900), 0xFFFF_1900);
    assert_eq!(field(0xFFFF_1900), Some(0x3_1900));
    assert_eq!(field(0x2_FFFF), Some(0x2_FFFF));
    for refused in [0x3_0000, 0x00FF_1900, 0xFFFE_0000, 0x8000_0000] {
        assert_eq!(field(refused), None, "{refused:#x}");
    }
    for value in [0, 0x1900, 0xFFFF, 0x1_0000, 0x2_ABCD, 0x3_0000, MAX_FIELD] {
        assert_eq!(field(address(value)), Some(value), "{value:#x}");
    }
}

#[test]
fn catalogues_of_the_same_entries_are_equal_whatever_they_held_before() {
    let files = [file(b"A", b'$', 2, 10), file(b"B", b'$', 3, 10)];
    let mut shrunk = Catalogue::blank(400);
    shrunk.set_files(&files).unwrap();
    shrunk.set_files(&files[..1]).unwrap();
    let mut fresh = Catalogue::blank(400);
    fresh.set_files(&files[..1]).unwrap();
    assert_eq!(shrunk, fresh);
}
