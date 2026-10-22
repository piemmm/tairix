//! Recognition of every supported format through the probe's one entry
//! point, over in-memory devices.

// Test images are a few hundred KiB, so every size fits the narrower field
// it is compared with.
#![allow(clippy::cast_possible_truncation)]

#[path = "support/images.rs"]
mod images;

use images::{device, iso, udf_image, Image};
use tairix_abi::driver::block::DiscLayout;
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::{
    fingerprint, iso9660, recognise, udf, Evidence, ProbedVolume, Recognised, ARXFS_RING_BLOCKS,
    FAT_ATTR_LONG_NAME, FAT_ATTR_VOLUME_ID, FAT_DELETED_ENTRY, FAT_DIR_ENTRY_LEN,
};

fn recognise_with(
    bytes: Vec<u8>,
    block_size: u32,
    layout: Option<DiscLayout>,
) -> Option<Recognised> {
    recognise(&mut device(bytes, block_size, layout)).expect("no device fault")
}

fn recognised(bytes: &[u8]) -> Option<ProbedVolume> {
    match recognise_with(bytes.to_vec(), 512, None)? {
        Recognised::Volume(volume) => Some(volume),
        Recognised::RaidMember(_) => None,
    }
}

fn fstype(bytes: &[u8]) -> Option<VolumeFsType> {
    recognised(bytes).map(|volume| volume.fstype)
}

fn udf_volume(image: Vec<u8>, block_size: u32) -> Option<udf::Volume> {
    udf::identify(&mut device(image, block_size, None), None).expect("no fault")
}

#[test]
fn a_raid_member_is_recognised_as_one_and_never_as_a_volume() {
    let uuid = [0x5A; 16];
    assert_eq!(
        recognise_with(images::raid_member_head(uuid), 512, None),
        Some(Recognised::RaidMember(uuid))
    );
    let mut corrupt = images::raid_member_head([0x9C; 16]);
    corrupt[20] ^= 0xFF;
    assert_eq!(
        recognise_with(corrupt, 512, None),
        None,
        "a member whose CRC fails is neither a member nor a volume"
    );
}

#[test]
fn recognises_fat32_with_identity_label_and_evidence() {
    let mut head = images::fat32_boot(*b"SRLN", b"MYDISK     ");
    head[48..50].copy_from_slice(&1u16.to_le_bytes());
    let probed = recognised(&head).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::Fat32);
    assert_eq!(probed.label(), b"MYDISK");
    let mut expected = [0u8; 16];
    expected[..4].copy_from_slice(b"SRLN");
    expected[4..15].copy_from_slice(b"MYDISK     ");
    expected[15] = 0xF3;
    assert_eq!(probed.identity, expected);
    assert_eq!(
        probed.evidence,
        Some(Evidence {
            offset: 0,
            len: 1024
        })
    );
    assert_eq!(probed.span, None);
}

#[test]
fn a_fat_label_placeholder_is_no_label() {
    let probed = recognised(&images::fat32_boot(*b"SRLN", b"NO NAME    ")).expect("matches");
    assert_eq!(probed.label(), b"");
    let blank = recognised(&images::fat32_boot(*b"SRLN", b"           ")).expect("matches");
    assert_eq!(blank.label(), b"");
}

#[test]
fn the_root_directory_label_outranks_the_boot_sector_copy() {
    let mut deleted = images::dir_entry(b"OLDLABEL   ", FAT_ATTR_VOLUME_ID);
    deleted[0] = FAT_DELETED_ENTRY;
    let image = images::fat32_with_root(
        b"NO NAME    ",
        images::FAT32_ROOT,
        &[
            deleted,
            images::dir_entry(b"LONGNAMEPRT", FAT_ATTR_LONG_NAME),
            images::dir_entry(b"README  TXT", 0x20),
            images::dir_entry(b"HOLIDAYS   ", FAT_ATTR_VOLUME_ID | 0x20),
        ],
    );
    let probed = recognised(&image).expect("matches");
    assert_eq!(probed.label(), b"HOLIDAYS");
    let boot_only = recognised(&images::fat32_boot(*b"SRLN", b"NO NAME    ")).expect("matches");
    assert_eq!(
        probed.identity, boot_only.identity,
        "a label is not an identity"
    );
}

#[test]
fn a_directory_without_a_label_entry_keeps_the_boot_label() {
    let after_end = images::fat32_with_root(
        b"MYDISK     ",
        images::FAT32_ROOT,
        &[
            images::dir_entry(b"README  TXT", 0x20),
            [0u8; FAT_DIR_ENTRY_LEN],
            images::dir_entry(b"STALE      ", FAT_ATTR_VOLUME_ID),
        ],
    );
    let placeholder = images::fat32_with_root(
        b"MYDISK     ",
        images::FAT32_ROOT,
        &[images::dir_entry(b"NO NAME    ", FAT_ATTR_VOLUME_ID)],
    );
    for image in [after_end, placeholder] {
        assert_eq!(recognised(&image).expect("matches").label(), b"MYDISK");
    }
}

#[test]
fn the_root_directory_is_found_where_the_parameter_block_places_it() {
    let mut image = images::fat32_with_root(
        b"MYDISK     ",
        images::FAT32_ROOT + 3 * 8 * 512,
        &[images::dir_entry(b"MOVED      ", FAT_ATTR_VOLUME_ID)],
    );
    assert_eq!(
        recognised(&image).expect("matches").label(),
        b"MYDISK",
        "cluster 2 is empty"
    );
    image[44..48].copy_from_slice(&5u32.to_le_bytes());
    assert_eq!(
        recognised(&image).expect("matches").label(),
        b"MOVED",
        "three clusters of eight sectors past cluster 2"
    );
}

#[test]
fn a_label_keeps_its_interior_bytes_and_loses_its_padding() {
    let probed = recognised(&images::ext4_head([7u8; 16], b"A B\0 \0")).expect("matches");
    assert_eq!(probed.label(), b"A B");
    let blank = recognised(&images::ext4_head([7u8; 16], b"     ")).expect("matches");
    assert_eq!(blank.label(), b"");
}

#[test]
fn a_root_directory_beyond_the_extent_leaves_the_boot_label() {
    let probed = recognised(&images::fat32_boot(*b"SRLN", b"MYDISK     ")).expect("matches");
    assert_eq!(probed.label(), b"MYDISK");
}

#[test]
fn fat32_without_extended_signature_has_tag_only_identity_and_no_label() {
    let mut head = images::fat32_boot([0; 4], b"           ");
    head[66] = 0;
    let probed = recognised(&head).expect("matches");
    assert_eq!(probed.label(), b"");
    let mut expected = [0u8; 16];
    expected[15] = 0xF3;
    assert_eq!(probed.identity, expected);
}

#[test]
fn fat32_structural_lies_are_refused() {
    for corrupt in [
        &mut |h: &mut [u8]| h[510] = 0,
        &mut |h: &mut [u8]| h[82..87].copy_from_slice(b"FAT16"),
        &mut |h: &mut [u8]| h[11..13].copy_from_slice(&513u16.to_le_bytes()),
        &mut |h: &mut [u8]| h[13] = 3,
        &mut |h: &mut [u8]| h[16] = 3,
        &mut |h: &mut [u8]| h[36..40].copy_from_slice(&0u32.to_le_bytes()),
        &mut |h: &mut [u8]| h[44..48].copy_from_slice(&1u32.to_le_bytes()),
    ] as [&mut dyn FnMut(&mut [u8]); 7]
    {
        let mut head = images::fat32_boot(*b"SRLN", b"MYDISK     ");
        corrupt(&mut head);
        assert_eq!(fstype(&head), None);
    }
}

#[test]
fn a_lying_fsinfo_pointer_leaves_no_evidence() {
    let fat = images::fat32_boot(*b"SRLN", b"MYDISK     ");
    assert_eq!(
        recognised(&fat).expect("matches").evidence,
        None,
        "no FSInfo pointer"
    );
    let mut lying = fat;
    lying[48..50].copy_from_slice(&32u16.to_le_bytes()); // == reserved
    assert_eq!(recognised(&lying).expect("matches").evidence, None);
}

#[test]
fn recognises_ext4_with_uuid_label_and_its_superblock_as_evidence() {
    let uuid = [7u8; 16];
    let probed = recognised(&images::ext4_head(uuid, b"backup\0\0")).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::Ext4);
    assert_eq!(probed.identity, uuid);
    assert_eq!(probed.label(), b"backup");
    assert_eq!(
        probed.evidence,
        Some(Evidence {
            offset: 0,
            len: 2048
        })
    );
}

#[test]
fn ext4_sanity_bounds_are_enforced() {
    for corrupt in [
        &mut |h: &mut [u8]| h[1024..1028].copy_from_slice(&0u32.to_le_bytes()),
        &mut |h: &mut [u8]| h[1028..1032].copy_from_slice(&0u32.to_le_bytes()),
        &mut |h: &mut [u8]| h[1024 + 0x18..1024 + 0x1C].copy_from_slice(&7u32.to_le_bytes()),
        &mut |h: &mut [u8]| h[1024 + 0x38] = 0,
    ] as [&mut dyn FnMut(&mut [u8]); 4]
    {
        let mut head = images::ext4_head([7u8; 16], b"backup");
        corrupt(&mut head);
        assert_eq!(fstype(&head), None);
    }
}

#[test]
fn recognises_arxfs_and_refuses_a_nil_uuid() {
    let uuid = [9u8; 16];
    let probed = recognised(&images::arxfs_head(uuid)).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::ARXFS);
    assert_eq!(probed.identity, uuid);
    assert_eq!(probed.label(), b"");

    assert_eq!(fstype(&images::arxfs_head([0u8; 16])), None);
    let mut wrong_type = images::arxfs_head(uuid);
    wrong_type[8..12].copy_from_slice(&3u32.to_le_bytes());
    assert_eq!(fstype(&wrong_type), None);
}

#[test]
fn the_arxfs_ring_is_sized_by_its_block_size() {
    for bs in [512u32, 4096] {
        let mut head = images::arxfs_head([9u8; 16]);
        head[128..132].copy_from_slice(&bs.to_le_bytes());
        assert_eq!(
            recognised(&head).expect("matches").evidence,
            Some(Evidence {
                offset: 0,
                len: ARXFS_RING_BLOCKS * u64::from(bs)
            })
        );
    }
    for bad in [0u32, 256, 768, 8192] {
        let mut head = images::arxfs_head([9u8; 16]);
        head[128..132].copy_from_slice(&bad.to_le_bytes());
        let probed = recognised(&head).expect("still the volume");
        assert_eq!(probed.evidence, None, "a lying block size declares nothing");
    }
}

#[test]
fn a_short_or_empty_extent_matches_nothing() {
    assert_eq!(recognise_with(Vec::new(), 512, None), None);
    assert_eq!(fstype(&[0u8; 512]), None);
}

#[test]
fn the_head_formats_resolve_most_specific_first() {
    let mut head = images::fat32_boot(*b"SRLN", b"MYDISK     ");
    let ext = images::ext4_head([7u8; 16], b"backup");
    head[1024..2048].copy_from_slice(&ext[1024..2048]);
    assert_eq!(fstype(&head), Some(VolumeFsType::Ext4));

    let arx = images::arxfs_head([9u8; 16]);
    head[..32].copy_from_slice(&arx[..32]);
    assert_eq!(fstype(&head), Some(VolumeFsType::ARXFS));
}

#[test]
fn fingerprint_is_deterministic_lowercase_and_distinct() {
    let a = fingerprint(&[0u8; 16]);
    assert_eq!(&a, b"00000000000000000000000000");
    let b = fingerprint(&[0xFF; 16]);
    assert!(b
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    assert_ne!(a, b);
    assert_eq!(fingerprint(&[0xFF; 16]), b);
    for c in *b"ilou" {
        assert!(!b.contains(&c));
    }
}

#[test]
fn fingerprint_encodes_every_identity_bit() {
    let x = [0u8; 16];
    let mut y = [0u8; 16];
    y[15] = 1;
    assert_ne!(fingerprint(&x), fingerprint(&y));
}

#[test]
fn an_iso9660_volume_is_identified_by_its_primary_descriptor() {
    let image = iso::image(&iso::Spec {
        volume_id: b"PHOTOS_2021",
        ..Default::default()
    });
    let probed = recognised(&image).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::Iso9660);
    assert_eq!(probed.label(), b"PHOTOS_2021");
    assert_eq!(
        probed.evidence, None,
        "never written, so nothing to compare"
    );
    assert_eq!(probed.span, Some(21 * 2048));
    let mut renamed = image.clone();
    renamed[19 * 2048 + 40] ^= 1;
    assert_eq!(
        recognised(&renamed).expect("matches").identity,
        probed.identity
    );
    let mut remastered = image;
    remastered[16 * 2048 + 41] = b'Q';
    assert_ne!(
        recognised(&remastered).expect("matches").identity,
        probed.identity
    );
}

#[test]
fn a_joliet_tree_is_mounted_and_labelled_in_ucs2() {
    let image = iso::image(&iso::Spec {
        volume_id: b"BACKUP",
        joliet_id: Some("Ümlaut Backup"),
        ..Default::default()
    });
    let probed = recognised(&image).expect("matches");
    assert_eq!(probed.label(), "Ümlaut Backup".as_bytes());
    let mut dev = ByteDevice::new(Image {
        block_size: 2048,
        bytes: image,
        layout: None,
    })
    .expect("geometry");
    let volume = iso9660::identify(&mut dev, None)
        .expect("no fault")
        .expect("iso");
    assert_eq!(volume.tree, iso9660::Tree::Joliet);
    assert_eq!(volume.joliet_level, 3);
}

#[test]
fn rock_ridge_or_risc_os_records_keep_the_primary_tree() {
    for (rock_ridge, archimedes) in [(true, false), (false, true)] {
        let image = iso::image(&iso::Spec {
            volume_id: b"PRIMARY",
            joliet_id: Some("Joliet"),
            rock_ridge,
            archimedes,
            ..Default::default()
        });
        let mut dev = ByteDevice::new(Image {
            block_size: 2048,
            bytes: image,
            layout: None,
        })
        .expect("geometry");
        let volume = iso9660::identify(&mut dev, None)
            .expect("no fault")
            .expect("iso");
        assert_eq!(volume.tree, iso9660::Tree::Primary);
        assert_eq!(volume.system_use.rock_ridge, rock_ridge);
        assert_eq!(volume.system_use.archimedes, archimedes);
        assert_eq!(volume.label().1, b"PRIMARY".len());
    }
}

#[test]
fn a_broken_joliet_root_falls_back_to_the_primary_tree() {
    let mut image = iso::image(&iso::Spec {
        volume_id: b"PRIMARY",
        joliet_id: Some("Joliet"),
        ..Default::default()
    });
    image[20 * 2048 + 33] = 7; // the Joliet root's `.` is no longer `.`
    assert_eq!(recognised(&image).expect("matches").label(), b"PRIMARY");
}

#[test]
fn high_sierra_is_recognised_in_its_own_layout() {
    let image = iso::image(&iso::Spec {
        volume_id: b"HSFDISC",
        high_sierra: true,
        ..Default::default()
    });
    let probed = recognised(&image).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::Iso9660);
    assert_eq!(probed.label(), b"HSFDISC");
}

#[test]
fn a_multisession_disc_is_read_from_its_last_session() {
    let mut image = iso::image(&iso::Spec {
        volume_id: b"FIRST",
        ..Default::default()
    });
    let second = iso::image(&iso::Spec {
        volume_id: b"SECOND",
        session: 100,
        ..Default::default()
    });
    image.resize(second.len(), 0);
    image[100 * 2048..].copy_from_slice(&second[100 * 2048..]);
    let layout = DiscLayout {
        last_session_start: 100,
        last_recorded: None,
    };
    let Some(Recognised::Volume(last)) = recognise_with(image.clone(), 2048, Some(layout)) else {
        panic!("the last session is a volume");
    };
    assert_eq!(last.label(), b"SECOND");
    let Some(Recognised::Volume(first)) = recognise_with(image, 2048, None) else {
        panic!("the first session is a volume");
    };
    assert_eq!(first.label(), b"FIRST");
    assert_ne!(first.identity, last.identity);
}

#[test]
fn iso9660_structural_lies_are_refused() {
    let good = iso::image(&iso::Spec {
        volume_id: b"GOOD",
        ..Default::default()
    });
    for corrupt in [
        &mut |i: &mut [u8]| i[16 * 2048 + 128..16 * 2048 + 132].copy_from_slice(&[0, 16, 16, 0]),
        &mut |i: &mut [u8]| i[16 * 2048 + 80..16 * 2048 + 88].fill(0),
        &mut |i: &mut [u8]| i[16 * 2048 + 84] ^= 1,
        &mut |i: &mut [u8]| i[16 * 2048 + 156 + 25] = 0,
        &mut |i: &mut [u8]| i[19 * 2048 + 33] = 1,
        &mut |i: &mut [u8]| i[16 * 2048] = 2,
    ] as [&mut dyn FnMut(&mut [u8]); 6]
    {
        let mut image = good.clone();
        corrupt(&mut image);
        assert_eq!(fstype(&image), None);
    }
}

#[test]
fn a_set_that_changes_format_part_way_is_refused() {
    let mut image = iso::image(&iso::Spec {
        volume_id: b"MIXED",
        ..Default::default()
    });
    let hsf = iso::image(&iso::Spec {
        volume_id: b"MIXED",
        high_sierra: true,
        ..Default::default()
    });
    image[17 * 2048..18 * 2048].copy_from_slice(&hsf[17 * 2048..18 * 2048]);
    assert_eq!(fstype(&image), None);
}

#[test]
fn the_descriptor_crc_meets_the_ecma_test_vector() {
    assert_eq!(udf::crc16(&[0x70, 0x6A, 0x77]), 0x3299);
    assert_eq!(udf::crc16(&[]), 0);
}

#[test]
fn a_sealed_tag_verifies_only_where_it_was_recorded() {
    let mut sector = vec![0u8; 512];
    sector[100] = 0xAB;
    let tag = udf::Tag {
        id: udf::TAG_PVD,
        version: 3,
        serial: 9,
        location: 42,
    };
    tag.seal(&mut sector, 496).expect("fits");
    assert_eq!(udf::Tag::verify(&sector, 42), Some(tag));
    assert_eq!(udf::Tag::verify(&sector, 43), None, "a moved descriptor");
    let mut flipped = sector.clone();
    flipped[100] ^= 1;
    assert_eq!(
        udf::Tag::verify(&flipped, 42),
        None,
        "the CRC covers the body"
    );
    let mut checksum = sector.clone();
    checksum[6] ^= 1;
    assert_eq!(
        udf::Tag::verify(&checksum, 42),
        None,
        "the checksum covers the tag"
    );
    assert_eq!(
        udf::Tag::verify(&sector[..200], 42),
        None,
        "the body must be present"
    );
    assert_eq!(
        tag.seal(&mut sector[..100], 496),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn a_udf_volume_is_recognised_with_its_label_evidence_and_span() {
    let image = udf_image::image(&udf_image::Spec::default());
    let probed = recognised(&image).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::Udf);
    assert_eq!(probed.label(), b"Archive");
    assert_eq!(
        probed.evidence,
        Some(Evidence {
            offset: u64::from(udf_image::places(2048).2) * 2048,
            len: 2 * 2048
        })
    );
    assert_eq!(probed.span, Some(600 * 2048));
    let volume = udf_volume(image, 2048).expect("udf");
    assert_eq!(volume.nsr, 3);
    assert_eq!(volume.logical().revision, 0x0260);
    assert_eq!(volume.partitions().len(), 1);
    assert_eq!(volume.partitions()[0].start, udf_image::PARTITION_START);
}

#[test]
fn a_udf_image_is_found_whatever_block_the_device_presents() {
    for (image_block, device_block) in [(2048, 512), (512, 2048), (4096, 512), (1024, 4096)] {
        let image = udf_image::image(&udf_image::Spec {
            block_size: image_block,
            blocks: 2400 * 2048 / image_block,
            ..Default::default()
        });
        let volume = udf_volume(image, device_block).expect("udf");
        assert_eq!(volume.block_size(), image_block as u32);
    }
}

#[test]
fn the_highest_sequence_number_prevails() {
    let spec = udf_image::Spec::default();
    let mut image = udf_image::image(&spec);
    // A second set later in the main sequence, over its terminator.
    let main = udf_image::places(2048).0;
    udf_image::sequence(&mut image, &spec, main + 3, 5, "Renamed");
    assert_eq!(recognised(&image).expect("matches").label(), b"Renamed");
}

#[test]
fn a_damaged_main_sequence_falls_back_to_the_reserve() {
    let mut image = udf_image::image(&udf_image::Spec::default());
    let main = udf_image::places(2048).0 as usize;
    image[main * 2048 + 200] ^= 0xFF;
    assert_eq!(recognised(&image).expect("matches").label(), b"Archive");
    let reserve = udf_image::places(2048).1 as usize;
    image[reserve * 2048 + 200] ^= 0xFF;
    assert_eq!(fstype(&image), None, "neither sequence holds a volume");
}

#[test]
fn udf_lies_are_refused() {
    let foreign = udf_image::image(&udf_image::Spec {
        domain: b"*Some Other Domain",
        ..Default::default()
    });
    assert_eq!(udf_volume(foreign, 2048), None, "a foreign domain");

    let mut no_anchor = udf_image::image(&udf_image::Spec::default());
    no_anchor[256 * 2048 + 20] ^= 1;
    let last = 599 * 2048;
    no_anchor[last + 20] ^= 1;
    assert_eq!(udf_volume(no_anchor, 2048), None, "no anchor verifies");

    let mut no_nsr = udf_image::image(&udf_image::Spec::default());
    no_nsr[32768 + 2048 + 1..32768 + 2048 + 6].copy_from_slice(b"NSR09");
    assert_eq!(udf_volume(no_nsr, 2048), None, "no NSR descriptor");

    let mut unterminated = udf_image::image(&udf_image::Spec::default());
    unterminated[32768 + 2 * 2048 + 1..32768 + 2 * 2048 + 6].fill(0);
    assert_eq!(
        udf_volume(unterminated, 2048),
        None,
        "no TEA closes the area"
    );
}

#[test]
fn a_bridge_disc_is_udf_unless_its_iso_tree_keeps_more() {
    let udf = udf_image::image(&udf_image::Spec {
        iso_first: true,
        ..Default::default()
    });
    let with_iso = |spec: &iso::Spec<'_>| {
        let mut image = udf.clone();
        let iso = iso::image(&iso::Spec {
            root_sector: 21,
            ..*spec
        });
        image[16 * 2048..17 * 2048].copy_from_slice(&iso[16 * 2048..17 * 2048]);
        image[21 * 2048..23 * 2048].copy_from_slice(&iso[21 * 2048..23 * 2048]);
        image
    };
    let plain = with_iso(&iso::Spec {
        volume_id: b"BRIDGE",
        ..Default::default()
    });
    assert_eq!(fstype(&plain), Some(VolumeFsType::Udf));
    for (rock_ridge, archimedes) in [(true, false), (false, true)] {
        let richer = with_iso(&iso::Spec {
            volume_id: b"BRIDGE",
            rock_ridge,
            archimedes,
            ..Default::default()
        });
        assert_eq!(fstype(&richer), Some(VolumeFsType::Iso9660));
    }
}

#[test]
fn cs0_decodes_both_compressions_and_refuses_others() {
    let narrow: Vec<_> = udf::cs0_chars(&[8, b'A', 0xE9]).expect("known").collect();
    assert_eq!(narrow, [Ok('A'), Ok('é')]);
    let wide: Vec<_> = udf::cs0_chars(&[16, 0x00, b'Z', 0xD8, 0x3D, 0xDE, 0x00])
        .expect("known")
        .collect();
    assert_eq!(wide, [Ok('Z'), Ok('😀')]);
    let lone: Vec<_> = udf::cs0_chars(&[16, 0xD8, 0x00]).expect("known").collect();
    assert_eq!(lone, [Err(0xD800)]);
    assert!(udf::cs0_chars(&[7, b'x']).is_none());
    assert!(udf::cs0_chars(&[]).is_none());
    assert_eq!(udf::dstring(&[8, b'h', b'i', 0, 3]), &[8, b'h', b'i']);
    assert_eq!(
        udf::dstring(&[8, b'h', 200]),
        &[8, b'h'],
        "a lying length is clipped"
    );
}

#[test]
fn an_old_map_adfs_disc_is_recognised_last_with_its_name() {
    let probed = recognised(&images::adfs_old_map(b"Acorn     ")).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::Adfs);
    assert_eq!(probed.label(), b"Acorn");
    assert_eq!(probed.evidence, None, "no structure every writer rewrites");
    let mut unmarked = images::adfs_old_map(b"Acorn     ");
    unmarked[512 + 1..512 + 5].copy_from_slice(b"Hugh");
    assert_eq!(fstype(&unmarked), None);
}

#[test]
fn a_session_the_drive_places_beyond_any_byte_address_is_no_disc() {
    let layout = DiscLayout {
        last_session_start: u64::MAX / 2048,
        last_recorded: None,
    };
    assert!(recognise_with(vec![0u8; 64 * 2048], 2048, Some(layout)).is_none());
    let mut dev = device(vec![0u8; 64 * 2048], 2048, Some(layout));
    assert!(iso9660::identify(&mut dev, Some(layout))
        .expect("no device fault")
        .is_none());
}
