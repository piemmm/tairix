extern crate std;

use std::vec::Vec;

use super::*;

/// The seed `debugfs dx_hash -s aa8a555b-f111-dd3a-e021-9aa07e8373d7` hashes
/// under, as the superblock stores it: four little-endian words.
const FIXTURE_SEED: [u32; 4] = [0x5B55_8AAA, 0x3ADD_11F1, 0xA09A_21E0, 0xD773_837E];

/// The names the vectors hash: chunk edges at 16 and 32 bytes either side,
/// a name of several chunks, and bytes a signed `char` reads as negative.
fn vector_names() -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = [
        &b"a"[..],
        b"abcd",
        b"abcde",
        b"document.txt",
        b"sixteen_bytes_ok",
        b"seventeen_bytes_x",
    ]
    .iter()
    .map(|name| name.to_vec())
    .collect();
    names.push(std::vec![b'a'; 31]);
    names.push(std::vec![b'b'; 32]);
    names.push(std::vec![b'c'; 33]);
    names.push(b"forty_byte_name_forty_byte_name_forty_bx".to_vec());
    names.push(std::vec![b'x'; 100]);
    names.push(b"caf\xc3\xa9".to_vec());
    names.push(b"\xff\x80\x7f".to_vec());
    names.push(b"file_0123".to_vec());
    names.push(Vec::new());
    names
}

/// `debugfs dx_hash` (e2fsprogs) for each of [`vector_names`] under versions
/// 0 to 5, as `(major, minor)`, as the MIT-licensed rust-fs-ext4 test suite
/// records them.
const ZERO_SEED_VECTORS: [[(u32, u32); 6]; 15] = [
    [
        (0xE74B_53E2, 0),
        (0xD5FA_7D7A, 0xACB4_8187),
        (0x6D0E_A4C0, 0xC189_22DF),
        (0xE74B_53E2, 0),
        (0xD5FA_7D7A, 0xACB4_8187),
        (0x6D0E_A4C0, 0xC189_22DF),
    ],
    [
        (0xFAFA_23CA, 0),
        (0xAD75_57A8, 0xB1DA_437C),
        (0x5A24_112E, 0x9544_2076),
        (0xFAFA_23CA, 0),
        (0xAD75_57A8, 0xB1DA_437C),
        (0x5A24_112E, 0x9544_2076),
    ],
    [
        (0x2297_902C, 0),
        (0x5821_840E, 0x1CE8_A82C),
        (0x6937_ED68, 0xB66B_D0F1),
        (0x2297_902C, 0),
        (0x5821_840E, 0x1CE8_A82C),
        (0x6937_ED68, 0xB66B_D0F1),
    ],
    [
        (0x976C_8B3A, 0),
        (0xAEC4_DABA, 0xE3CC_0BB9),
        (0xFB93_2DDA, 0x8E54_5387),
        (0x976C_8B3A, 0),
        (0xAEC4_DABA, 0xE3CC_0BB9),
        (0xFB93_2DDA, 0x8E54_5387),
    ],
    [
        (0x7ABB_BB02, 0),
        (0xC847_64E0, 0xF5A5_AA7F),
        (0x1F0A_5D00, 0x18FB_30A1),
        (0x7ABB_BB02, 0),
        (0xC847_64E0, 0xF5A5_AA7F),
        (0x1F0A_5D00, 0x18FB_30A1),
    ],
    [
        (0x4294_E10A, 0),
        (0x8F73_9670, 0x7B0B_FB17),
        (0xF350_332E, 0xB9EE_CF7F),
        (0x4294_E10A, 0),
        (0x8F73_9670, 0x7B0B_FB17),
        (0xF350_332E, 0xB9EE_CF7F),
    ],
    [
        (0x8625_91B4, 0),
        (0x93A0_250C, 0xAE40_7DAE),
        (0x3DF9_B80A, 0x1110_A323),
        (0x8625_91B4, 0),
        (0x93A0_250C, 0xAE40_7DAE),
        (0x3DF9_B80A, 0x1110_A323),
    ],
    [
        (0x3604_06FC, 0),
        (0x8BE1_9AFE, 0x3D80_276E),
        (0xCE9B_05FA, 0x7AC0_E97E),
        (0x3604_06FC, 0),
        (0x8BE1_9AFE, 0x3D80_276E),
        (0xCE9B_05FA, 0x7AC0_E97E),
    ],
    [
        (0xC67A_2FC4, 0),
        (0x3C7D_AC74, 0xE7A3_AD67),
        (0xB754_07F8, 0x2323_8490),
        (0xC67A_2FC4, 0),
        (0x3C7D_AC74, 0xE7A3_AD67),
        (0xB754_07F8, 0x2323_8490),
    ],
    [
        (0xA103_8B36, 0),
        (0x3A08_9372, 0x0E5D_629D),
        (0x386F_5A9A, 0x27A2_706C),
        (0xA103_8B36, 0),
        (0x3A08_9372, 0x0E5D_629D),
        (0x386F_5A9A, 0x27A2_706C),
    ],
    [
        (0xF985_A9F6, 0),
        (0x23AA_98AC, 0x9A97_4DF1),
        (0x7353_AB0E, 0xCECC_2718),
        (0xF985_A9F6, 0),
        (0x23AA_98AC, 0x9A97_4DF1),
        (0x7353_AB0E, 0xCECC_2718),
    ],
    [
        (0x96CA_5A2C, 0),
        (0xFB9C_5E5C, 0x0573_E8B8),
        (0x1058_42EA, 0xFB91_65CA),
        (0x6DDE_4230, 0),
        (0x9D72_AED6, 0xF613_8C6A),
        (0x6621_F032, 0xF866_99C6),
    ],
    [
        (0xB6A1_B8CC, 0),
        (0x337F_F96A, 0x4ECD_4AC0),
        (0x6BA3_8152, 0x1FED_68FC),
        (0xB32A_9ECC, 0),
        (0xF435_CE8C, 0x2D0B_3C11),
        (0x4907_E268, 0xDC81_D4B9),
    ],
    [
        (0xC08D_0086, 0),
        (0x2FB4_51EE, 0x9E96_C067),
        (0x92D7_5966, 0xE00D_31E3),
        (0xC08D_0086, 0),
        (0x2FB4_51EE, 0x9E96_C067),
        (0x92D7_5966, 0xE00D_31E3),
    ],
    [
        (0x2547_FC5A, 0),
        (0xEFCD_AB88, 0x98BA_DCFE),
        (0x6745_2300, 0xEFCD_AB89),
        (0x2547_FC5A, 0),
        (0xEFCD_AB88, 0x98BA_DCFE),
        (0x6745_2300, 0xEFCD_AB89),
    ],
];

/// As [`ZERO_SEED_VECTORS`], under [`FIXTURE_SEED`].
const FIXTURE_SEED_VECTORS: [[(u32, u32); 6]; 15] = [
    [
        (0xE74B_53E2, 0),
        (0xD1D4_380E, 0x832F_6DC9),
        (0x52EB_669E, 0x95D4_D440),
        (0xE74B_53E2, 0),
        (0xD1D4_380E, 0x832F_6DC9),
        (0x52EB_669E, 0x95D4_D440),
    ],
    [
        (0xFAFA_23CA, 0),
        (0x787A_A7F0, 0x830D_603C),
        (0x76D4_7D16, 0xFB56_BE08),
        (0xFAFA_23CA, 0),
        (0x787A_A7F0, 0x830D_603C),
        (0x76D4_7D16, 0xFB56_BE08),
    ],
    [
        (0x2297_902C, 0),
        (0x21E9_0E84, 0xB3C1_681D),
        (0x0FF5_72A8, 0xFD99_6CD0),
        (0x2297_902C, 0),
        (0x21E9_0E84, 0xB3C1_681D),
        (0x0FF5_72A8, 0xFD99_6CD0),
    ],
    [
        (0x976C_8B3A, 0),
        (0x0FD5_D204, 0x3D6F_CEF7),
        (0x56B5_6F02, 0x6421_9D93),
        (0x976C_8B3A, 0),
        (0x0FD5_D204, 0x3D6F_CEF7),
        (0x56B5_6F02, 0x6421_9D93),
    ],
    [
        (0x7ABB_BB02, 0),
        (0x6A4A_B3DC, 0x8834_951F),
        (0x3896_7632, 0x31D8_A5D1),
        (0x7ABB_BB02, 0),
        (0x6A4A_B3DC, 0x8834_951F),
        (0x3896_7632, 0x31D8_A5D1),
    ],
    [
        (0x4294_E10A, 0),
        (0x4C27_9658, 0xB64C_0A66),
        (0x087E_F02A, 0x4A13_8E96),
        (0x4294_E10A, 0),
        (0x4C27_9658, 0xB64C_0A66),
        (0x087E_F02A, 0x4A13_8E96),
    ],
    [
        (0x8625_91B4, 0),
        (0x5184_3964, 0x0B2B_4691),
        (0x8AC9_F4BA, 0xA34B_DB9C),
        (0x8625_91B4, 0),
        (0x5184_3964, 0x0B2B_4691),
        (0x8AC9_F4BA, 0xA34B_DB9C),
    ],
    [
        (0x3604_06FC, 0),
        (0x7D95_B0CA, 0x5C71_77D4),
        (0x1C35_B46C, 0x20AD_B9B7),
        (0x3604_06FC, 0),
        (0x7D95_B0CA, 0x5C71_77D4),
        (0x1C35_B46C, 0x20AD_B9B7),
    ],
    [
        (0xC67A_2FC4, 0),
        (0x52C5_D336, 0x154A_1811),
        (0xC058_CA6A, 0x2494_5D66),
        (0xC67A_2FC4, 0),
        (0x52C5_D336, 0x154A_1811),
        (0xC058_CA6A, 0x2494_5D66),
    ],
    [
        (0xA103_8B36, 0),
        (0x70A3_8F70, 0x5E9D_2F5A),
        (0xDD9B_BBC4, 0x7107_2E7D),
        (0xA103_8B36, 0),
        (0x70A3_8F70, 0x5E9D_2F5A),
        (0xDD9B_BBC4, 0x7107_2E7D),
    ],
    [
        (0xF985_A9F6, 0),
        (0x5AF2_3298, 0x7CA7_1B80),
        (0x9282_CEC6, 0x404E_7923),
        (0xF985_A9F6, 0),
        (0x5AF2_3298, 0x7CA7_1B80),
        (0x9282_CEC6, 0x404E_7923),
    ],
    [
        (0x96CA_5A2C, 0),
        (0xE180_DCAE, 0xF665_5F53),
        (0x24B9_9EA6, 0x6455_954A),
        (0x6DDE_4230, 0),
        (0x393B_D250, 0x38A5_D475),
        (0xE88D_26D0, 0xD513_5EBB),
    ],
    [
        (0xB6A1_B8CC, 0),
        (0x5307_6A54, 0x7118_7582),
        (0x0B2B_DA42, 0x67A6_1515),
        (0xB32A_9ECC, 0),
        (0x4185_6E60, 0x410E_A688),
        (0xAFC9_ECB2, 0x36EA_C684),
    ],
    [
        (0xC08D_0086, 0),
        (0xE83B_032A, 0x1936_9104),
        (0xB2F3_8B0E, 0x917D_5BB7),
        (0xC08D_0086, 0),
        (0xE83B_032A, 0x1936_9104),
        (0xB2F3_8B0E, 0x917D_5BB7),
    ],
    [
        (0x2547_FC5A, 0),
        (0x3ADD_11F0, 0xA09A_21E0),
        (0x5B55_8AAA, 0x3ADD_11F1),
        (0x2547_FC5A, 0),
        (0x3ADD_11F0, 0xA09A_21E0),
        (0x5B55_8AAA, 0x3ADD_11F1),
    ],
];

/// Check each name's hash against `vectors`, whose six columns are `debugfs`'s
/// versions 0 to 5: the three algorithms over signed bytes, then over
/// unsigned. Only the major hash places a name, so only it is compared.
fn check_vectors(seed: [u32; 4], vectors: &[[(u32, u32); 6]; 15]) {
    for (name, expected) in vector_names().iter().zip(vectors) {
        for (column, &(major, _)) in expected.iter().enumerate() {
            let stored = u8::try_from(column % 3).expect("small");
            let hash =
                NameHash::new(stored, Some(column < 3), seed).expect("a hash the driver computes");
            assert_eq!(hash.of(name), Some(major), "column {column} of {name:?}");
        }
    }
}

#[test]
fn the_hash_is_the_one_e2fsprogs_reports_unseeded() {
    check_vectors([0; 4], &ZERO_SEED_VECTORS);
}

#[test]
fn the_hash_is_the_one_e2fsprogs_reports_under_a_seed() {
    check_vectors(FIXTURE_SEED, &FIXTURE_SEED_VECTORS);
}

/// The superblock, not the root, says how a name's bytes are read: bytes
/// below 0x80 hash alike either way, the rest do not.
#[test]
fn a_volume_reads_name_bytes_as_its_superblock_says() {
    for stored in 0..3 {
        let signed = NameHash::new(stored, Some(true), [0; 4]).expect("computed");
        let unsigned = NameHash::new(stored, Some(false), [0; 4]).expect("computed");
        assert_eq!(signed.of(b"plain"), unsigned.of(b"plain"));
        assert_ne!(signed.of(b"caf\xc3\xa9"), unsigned.of(b"caf\xc3\xa9"));
    }
}

/// A root names its algorithm, never the signedness: the unsigned twins are
/// the superblock's business, `SipHash` is not computed here, and a volume that
/// records no signedness cannot have its names placed.
#[test]
fn only_a_hash_the_driver_computes_places_names() {
    for stored in [3, 4, 5, 7, 0xFF] {
        assert_eq!(
            NameHash::new(stored, Some(true), [0; 4]).err(),
            Some(DriverError::DeviceFault)
        );
    }
    assert_eq!(
        NameHash::new(SIPHASH, Some(true), [0; 4]).err(),
        Some(DriverError::Unsupported)
    );
    assert_eq!(
        NameHash::new(1, None, [0; 4]).err(),
        Some(DriverError::Unsupported)
    );
}

#[test]
fn a_name_longer_than_the_format_allows_has_no_hash() {
    let hash = NameHash::new(1, Some(true), FIXTURE_SEED).expect("computed");
    assert!(hash.of(&[b'n'; MAX_NAME_LEN]).is_some());
    assert_eq!(hash.of(&[b'n'; MAX_NAME_LEN + 1]), None);
}

/// No name takes the end-of-directory position or a continuation bit.
#[test]
fn a_hash_is_even_and_never_the_end_of_the_directory() {
    let hash = NameHash::new(1, Some(true), FIXTURE_SEED).expect("computed");
    for index in 0..4096u32 {
        let name = std::format!("name-{index}");
        let placed = hash.of(name.as_bytes()).expect("short");
        assert_eq!(placed & CONTINUED, 0);
        assert_ne!(placed, END);
    }
}

/// A `size`-byte block holding an index of `level` with `count` entries,
/// entry `i` covering from hash `i * 0x100` and pointing at block `i + 10`.
fn index_block(size: usize, level: Level, count: usize, csum: bool) -> Vec<u8> {
    let mut block = std::vec![0u8; size];
    match level {
        Level::Node => put_le16(&mut block, 4, u16::try_from(size).expect("small")),
        Level::Root => {
            put_le16(&mut block, 4, 12);
            put_le16(&mut block, 16, u16::try_from(size - 12).expect("small"));
            block[ROOT_INFO + 4] = 1;
            block[ROOT_INFO + 5] = ROOT_INFO_LEN;
        }
    }
    let at = level.offset();
    put_le16(
        &mut block,
        at,
        u16::try_from(level.limit(size, csum)).expect("small"),
    );
    put_le16(&mut block, at + 2, u16::try_from(count).expect("small"));
    for index in 0..count {
        if index > 0 {
            put_le32(
                &mut block,
                at + index * ENTRY,
                u32::try_from(index * 0x100).expect("small"),
            );
        }
        put_le32(
            &mut block,
            at + index * ENTRY + 4,
            u32::try_from(index + 10).expect("small"),
        );
    }
    block
}

#[test]
fn a_hash_finds_the_entry_whose_range_holds_it() {
    let block = index_block(1024, Level::Node, 4, false);
    let entries = Entries::of(&block, Level::Node);
    assert_eq!(entries.find(0), 0);
    assert_eq!(entries.find(0xFE), 0);
    assert_eq!(entries.find(0x100), 1);
    assert_eq!(entries.find(0x2FE), 2);
    assert_eq!(entries.find(u32::MAX), 3);
    assert_eq!(entries.child(3), 13);
}

/// A continued entry's odd hash sends a probe for its even value to the
/// entry before, where the run of equal hashes starts.
#[test]
fn a_probe_for_a_continued_hash_lands_where_its_run_starts() {
    let mut block = index_block(1024, Level::Node, 3, false);
    put_le32(
        &mut block,
        Level::Node.offset() + 2 * ENTRY,
        0x100 | CONTINUED,
    );
    let entries = Entries::of(&block, Level::Node);
    assert_eq!(entries.find(0x100), 1);
    assert_eq!(entries.find(0x102), 2);
}

#[test]
fn an_insert_keeps_the_index_in_order_and_refuses_a_full_one() {
    let mut block = index_block(1024, Level::Node, 3, false);
    EntriesMut::of(&mut block, Level::Node)
        .insert(1, 0x180, 99)
        .expect("room");
    let entries = Entries::of(&block, Level::Node);
    assert_eq!(entries.count(), 4);
    assert_eq!((entries.hash(2), entries.child(2)), (0x180, 99));
    assert_eq!((entries.hash(3), entries.child(3)), (0x200, 12));
    check_node(&block, false).expect("still a valid node");

    let full = Level::Node.limit(1024, false);
    let mut block = index_block(1024, Level::Node, full, false);
    assert_eq!(
        EntriesMut::of(&mut block, Level::Node).insert(0, 1, 1),
        Err(DriverError::DeviceFault)
    );
    let mut block = index_block(1024, Level::Node, 3, false);
    assert_eq!(
        EntriesMut::of(&mut block, Level::Node).insert(3, 0x400, 1),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn splitting_an_index_hands_the_parent_its_lowest_hash() {
    let mut from = index_block(1024, Level::Node, 6, false);
    let mut to = std::vec![0u8; 1024];
    init_node(&mut to, false).expect("lays out");
    let lowest = EntriesMut::of(&mut from, Level::Node)
        .split_off(4, &mut EntriesMut::of(&mut to, Level::Node))
        .expect("moves");
    assert_eq!(lowest, 0x400);
    assert_eq!(Entries::of(&from, Level::Node).count(), 4);
    let moved = Entries::of(&to, Level::Node);
    assert_eq!(moved.count(), 2);
    assert_eq!(moved.child(0), 14);
    assert_eq!((moved.hash(1), moved.child(1)), (0x500, 15));
    check_node(&to, false).expect("a valid node");
    check_node(&from, false).expect("still a valid node");
}

/// A full root hands every entry down to a new node and points at it alone.
#[test]
fn pushing_a_root_down_keeps_every_entry() {
    let full = Level::Root.limit(1024, false);
    let mut root = index_block(1024, Level::Root, full, false);
    let mut node = std::vec![0u8; 1024];
    init_node(&mut node, false).expect("lays out");
    EntriesMut::of(&mut root, Level::Root)
        .push_down(&mut EntriesMut::of(&mut node, Level::Node), 77)
        .expect("moves");
    let top = Entries::of(&root, Level::Root);
    assert_eq!((top.count(), top.child(0)), (1, 77));
    let below = Entries::of(&node, Level::Node);
    assert_eq!(below.count(), full);
    for index in 0..full {
        assert_eq!(
            below.child(index),
            u32::try_from(index + 10).expect("small")
        );
        assert_eq!(
            below.hash(index),
            u32::try_from(index * 0x100).expect("small")
        );
    }
    assert!(!below.is_full());
}

#[test]
fn a_damaged_index_is_refused() {
    let mut block = index_block(1024, Level::Node, 3, true);
    seal(&mut block, Level::Node, 0x1234_5678);
    check_node(&block, true).expect("valid");
    assert!(verify(&block, Level::Node, 0x1234_5678));
    // A limit the block size and checksum tail do not give.
    let mut wrong = block.clone();
    put_le16(&mut wrong, Level::Node.offset(), 200);
    assert_eq!(check_node(&wrong, true), Err(DriverError::DeviceFault));
    // A tail-less limit on a volume that checksums.
    let wrong = index_block(1024, Level::Node, 3, false);
    assert_eq!(check_node(&wrong, true), Err(DriverError::DeviceFault));
    // Entries out of order.
    let mut wrong = block.clone();
    put_le32(&mut wrong, Level::Node.offset() + 2 * ENTRY, 0x10);
    assert_eq!(check_node(&wrong, true), Err(DriverError::DeviceFault));
    // No entries at all.
    let mut wrong = block.clone();
    put_le16(&mut wrong, Level::Node.offset() + 2, 0);
    assert_eq!(check_node(&wrong, true), Err(DriverError::DeviceFault));
    // A node's empty record must span its block.
    let mut wrong = block.clone();
    put_le16(&mut wrong, 4, 1012);
    assert_eq!(check_node(&wrong, true), Err(DriverError::DeviceFault));
    // An entry changed under its checksum keeps its shape but fails the tail.
    let mut wrong = block;
    put_le32(&mut wrong, Level::Node.offset() + ENTRY + 4, 99);
    check_node(&wrong, true).expect("still shaped as a node");
    assert!(!verify(&wrong, Level::Node, 0x1234_5678));
}

#[test]
fn a_root_is_checked_from_its_dot_entries_to_its_tail() {
    let mut block = index_block(1024, Level::Root, 2, true);
    set_levels(&mut block, 1);
    seal(&mut block, Level::Root, 7);
    let info = check_root(&block, true).expect("valid");
    assert_eq!((info.hash_version, info.levels), (1, 1));
    assert!(verify(&block, Level::Root, 7));
    for (offset, value) in [
        (4usize, 16u16),         // `.` not 12 bytes
        (16, 1000),              // `..` not reaching the block end
        (ROOT_INFO + 4, 0x0901), // info length other than 8
        (ROOT_INFO + 6, 0x0100), // an incompatible hash flag
    ] {
        let mut wrong = block.clone();
        put_le16(&mut wrong, offset, value);
        assert_eq!(
            check_root(&wrong, true).err(),
            Some(DriverError::DeviceFault),
            "{offset:#x}"
        );
    }
    let mut wrong = block.clone();
    put_le32(&mut wrong, ROOT_INFO, 1);
    assert_eq!(
        check_root(&wrong, true).err(),
        Some(DriverError::DeviceFault)
    );
    // The same root under another directory's seed.
    assert!(!verify(&block, Level::Root, 8));
}

/// The tail's checksum covers the entries in use and the reserved word, so a
/// change to either is caught and the slack past the count is not.
#[test]
fn the_tail_checksum_covers_the_entries_in_use() {
    let mut block = index_block(1024, Level::Node, 3, true);
    seal(&mut block, Level::Node, 0x1234_5678);
    let tail = tail_at(&block, Level::Node);
    let sealed = le32(&block, tail + 4);
    let mut slack = block.clone();
    slack[Level::Node.offset() + 5 * ENTRY] = 0xEE;
    assert_eq!(tail_checksum(&slack, Level::Node, 0x1234_5678), sealed);
    let mut reserved = block.clone();
    reserved[tail] = 1;
    assert_ne!(tail_checksum(&reserved, Level::Node, 0x1234_5678), sealed);
    let mut changed = block;
    put_le32(&mut changed, Level::Node.offset() + 2 * ENTRY + 4, 77);
    assert_ne!(tail_checksum(&changed, Level::Node, 0x1234_5678), sealed);
}
