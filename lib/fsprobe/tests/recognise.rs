//! Recognition of every supported format through the probe's one entry
//! point, over in-memory devices.

// Test images are a few hundred KiB, so every size fits the narrower field
// it is compared with.
#![allow(clippy::cast_possible_truncation)]

#[path = "support/images.rs"]
mod images;

use images::{device, iso, udf_image, Image};
use tairix_abi::blkio::BlkForm;
use tairix_abi::driver::block::DiscLayout;
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::{
    adfs, affs, fingerprint, iso9660, recognise, udf, Evidence, ProbedVolume, Recognised,
    ARXFS_RING_BLOCKS, FAT_ATTR_LONG_NAME, FAT_ATTR_VOLUME_ID, FAT_DELETED_ENTRY,
    FAT_DIR_ENTRY_LEN,
};
use tairix_test_amiga_image::rdb_disk;

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
        Recognised::RaidMember(_) | Recognised::RigidDiskBlock => None,
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
    let mut relabelled = head.clone();
    relabelled[71..82].copy_from_slice(b"OTHER      ");
    let mut reserialled = head.clone();
    reserialled[67] ^= 1;
    let mut regeometried = head.clone();
    regeometried[36..40].copy_from_slice(&2048u32.to_le_bytes());
    let identity = |image: &[u8]| recognised(image).expect("matches").identity;
    assert_eq!(
        identity(&relabelled),
        probed.identity,
        "a label is no part of it"
    );
    assert_ne!(identity(&reserialled), probed.identity, "the serial is");
    assert_ne!(
        identity(&regeometried),
        probed.identity,
        "and so is the geometry"
    );
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
    let boot_only = recognised(&images::fat32_boot(*b"SRLN", b"MYDISK     ")).expect("matches");
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
fn fat32_without_extended_signature_is_identified_by_its_geometry_and_has_no_label() {
    let mut head = images::fat32_boot([0; 4], b"           ");
    head[66] = 0;
    let probed = recognised(&head).expect("matches");
    assert_eq!(probed.label(), b"");
    let mut undeclared = head.clone();
    undeclared[67..82].copy_from_slice(b"SRLNMYDISK     ");
    assert_eq!(
        recognised(&undeclared).expect("matches").identity,
        probed.identity,
        "bytes the signature does not declare are no part of it"
    );
    assert_ne!(probed.identity, [0u8; 16]);
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
        reads: 0,
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
            reads: 0,
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

/// A disc name is spelt in RISC OS's Latin-1, so its label is that text in
/// UTF-8: an accented letter and one of the characters RISC OS adds alike.
#[test]
fn an_adfs_disc_name_is_labelled_in_utf8() {
    let probed = recognised(&images::adfs_old_map(b"Caf\xE9 \x80    ")).expect("matches");
    assert_eq!(probed.label(), "Caf\u{e9} \u{20ac}".as_bytes());
}

/// An ADFS disc's identity is what formatting fixed, its name no part of it:
/// discs named apart share it, and another size of disc does not.
#[test]
fn an_adfs_disc_is_identified_without_its_name() {
    let acorn = recognised(&images::adfs_old_map(b"Acorn     ")).expect("matches");
    let retitled = recognised(&images::adfs_old_map(b"Renamed   ")).expect("matches");
    assert_eq!(acorn.identity, retitled.identity);
    let record = images::adfs_f_record();
    let named = adfs::Layout::NewMap {
        record,
        boot_block: true,
    };
    let renamed = adfs::Layout::NewMap {
        record: adfs::DiscRecord {
            disc_name: *b"Other     ",
            ..record
        },
        boot_block: true,
    };
    let smaller = adfs::Layout::NewMap {
        record: adfs::DiscRecord {
            disc_size: record.disc_size / 2,
            ..record
        },
        boot_block: true,
    };
    assert_eq!(renamed.identity(), named.identity());
    assert_ne!(smaller.identity(), named.identity());
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

#[test]
fn an_amiga_floppy_is_recognised_with_its_name_identity_and_root_as_evidence() {
    let image = images::affs_volume(0, 512, affs::DD_FLOPPY_BLOCKS, b"Workbench1.3");
    let probed = recognised(&image).expect("matches");
    assert_eq!(probed.fstype, VolumeFsType::Affs);
    assert_eq!(probed.label(), b"Workbench1.3");
    assert_eq!(probed.form, BlkForm::FloppyDisk);
    assert_eq!(
        probed.evidence,
        Some(Evidence {
            offset: 880 * 512,
            len: 512
        })
    );
    assert_eq!(probed.span, None);
    let mut dev = device(image.clone(), 512, None);
    let layout = affs::identify(&mut dev)
        .expect("no fault")
        .expect("a volume");
    assert_eq!(probed.identity, layout.identity());
    assert_eq!((layout.root, layout.blocks), (880, 1760));
    assert!(layout.bitmap_valid);
    let renamed = images::affs_volume(0, 512, affs::DD_FLOPPY_BLOCKS, b"Workbench1.2");
    assert_eq!(
        recognised(&renamed).expect("matches").identity,
        probed.identity,
        "a name is no part of the identity"
    );
    let mut reformatted = image;
    let root = &mut reformatted[880 * 512..881 * 512];
    affs::DateStamp {
        days: 9001,
        mins: 600,
        ticks: 25,
    }
    .write(root, affs::Offsets::new(512).created());
    affs::seal(root, affs::CHECKSUM);
    assert_ne!(
        recognised(&reformatted).expect("matches").identity,
        probed.identity,
        "a volume formatted another day is another"
    );
}

#[test]
fn a_latin1_volume_name_is_labelled_in_utf8() {
    let image = images::affs_volume(3, 512, affs::HD_FLOPPY_BLOCKS, b"Gr\xFC\xDFe");
    let probed = recognised(&image).expect("matches");
    assert_eq!(probed.label(), "Grüße".as_bytes());
    assert_eq!(probed.form, BlkForm::FloppyDisk);
}

#[test]
fn every_dos_type_is_an_amiga_volume_and_no_other_is() {
    for flags in 0..=7 {
        let image = images::affs_volume(flags, 512, 40, b"Work");
        assert_eq!(fstype(&image), Some(VolumeFsType::Affs), "DOS\\{flags}");
        let dos = affs::DosType::new(flags).expect("a type");
        assert_eq!(dos.is_fast(), flags & 1 == 1);
        assert_eq!(dos.is_international(), flags >= 2);
        assert_eq!(dos.has_dir_cache(), matches!(flags, 4 | 5));
        assert_eq!(dos.has_long_names(), flags >= 6);
    }
    let mut unknown = images::affs_volume(0, 512, 40, b"Work");
    unknown[3] = 8;
    assert_eq!(fstype(&unknown), None);
    let mut kickstart = images::affs_volume(0, 512, 40, b"Work");
    kickstart[..4].copy_from_slice(b"KICK");
    assert_eq!(fstype(&kickstart), None);
}

#[test]
fn a_volume_formatted_with_larger_blocks_is_found_at_its_own_middle() {
    for block_size in [1024, 4096, 32768] {
        let image = images::affs_volume(1, block_size, 64, b"HardFile");
        let probed = recognised(&image).expect("matches");
        assert_eq!(probed.fstype, VolumeFsType::Affs, "{block_size}");
        assert_eq!(probed.form, BlkForm::Unspecified);
        let layout = affs::identify(&mut device(image, 512, None))
            .expect("no fault")
            .expect("a volume");
        assert_eq!(layout.block_size as usize, block_size);
        assert_eq!(layout.root, 32);
    }
}

#[test]
fn the_root_the_boot_block_names_is_tried_when_the_middle_holds_none() {
    // A partition formatted with more reserved blocks than the boot pair keeps
    // its root off the middle; its boot block says where.
    let mut image = vec![0u8; 100 * 512];
    image[..4].copy_from_slice(b"DOS\x01");
    image[8..12].copy_from_slice(&52u32.to_be_bytes());
    images::write_affs_root(&mut image[52 * 512..53 * 512], b"Moved");
    assert_eq!(fstype(&image), Some(VolumeFsType::Affs));
    image[8..12].copy_from_slice(&100u32.to_be_bytes());
    assert_eq!(fstype(&image), None, "a pointer past the volume is no root");
}

/// A named corruption of one block.
type Lie = (&'static str, fn(&mut [u8]));

#[test]
fn amiga_root_lies_are_refused() {
    let good = images::affs_volume(1, 512, 40, b"Work");
    let root = 20 * 512;
    assert_eq!(fstype(&good), Some(VolumeFsType::Affs));
    let cases: [Lie; 7] = [
        ("not a header", |b| b[3] = 8),
        ("a header key", |b| b[7] = 1),
        ("a hash table of another block size", |b| b[15] = 128),
        ("a first data block", |b| b[19] = 1),
        ("not the root", |b| b[511] = 2),
        ("a name past its bound", |b| b[432] = 31),
        ("a broken checksum", |b| b[100] ^= 1),
    ];
    for (what, corrupt) in cases {
        let mut bad = good.clone();
        let block = &mut bad[root..root + 512];
        corrupt(block);
        if what != "a broken checksum" {
            affs::seal(block, affs::CHECKSUM);
        }
        assert_eq!(fstype(&bad), None, "{what}");
    }
    assert_eq!(fstype(&good[..3 * 512]), None, "too small for a root");
}

#[test]
fn acorn_and_amiga_floppies_are_never_taken_for_each_other() {
    let acorn = images::adfs_old_map(b"Acorn     ");
    let amiga = images::affs_volume(0, 512, affs::DD_FLOPPY_BLOCKS, b"Amiga");
    assert_eq!(fstype(&acorn), Some(VolumeFsType::Adfs));
    assert_eq!(fstype(&amiga), Some(VolumeFsType::Affs));
    assert!(affs::identify(&mut device(acorn, 512, None))
        .expect("no fault")
        .is_none());
    assert!(adfs::identify(&mut device(amiga, 512, None))
        .expect("no fault")
        .is_none());
}

#[test]
fn a_volume_reports_the_medium_its_format_was_laid_out_for() {
    assert_eq!(
        recognised(&images::adfs_old_map(b"Acorn     "))
            .expect("adfs")
            .form,
        BlkForm::FloppyDisk
    );
    assert_eq!(
        recognised(&images::affs_volume(1, 512, 40, b"Part"))
            .expect("affs")
            .form,
        BlkForm::Unspecified
    );
    let iso = iso::image(&iso::Spec {
        volume_id: b"DISC",
        ..Default::default()
    });
    assert_eq!(recognised(&iso).expect("iso").form, BlkForm::OpticalDisc);
    let disc = udf_image::image(&udf_image::Spec::default());
    let Some(Recognised::Volume(udf)) = recognise_with(disc, 2048, None) else {
        panic!("a udf volume");
    };
    assert_eq!(udf.form, BlkForm::OpticalDisc);
    let stick = udf_image::image(&udf_image::Spec {
        block_size: 512,
        blocks: 2400,
        ..Default::default()
    });
    assert_eq!(recognised(&stick).expect("udf").form, BlkForm::Unspecified);
    let head = images::fat32_boot(*b"SRLN", b"MYDISK     ");
    assert_eq!(recognised(&head).expect("fat32").form, BlkForm::Unspecified);
}

#[test]
fn a_rigid_disk_block_is_partitions_not_a_volume() {
    for at in [0, 15] {
        assert_eq!(
            recognise_with(rdb_disk(32, at), 512, None),
            Some(Recognised::RigidDiskBlock),
            "block {at}"
        );
    }
    let mut broken = rdb_disk(32, 0);
    broken[40] ^= 1;
    assert_eq!(recognise_with(broken, 512, None), None, "its sum fails");
    assert_eq!(
        recognise_with(rdb_disk(32, 16), 512, None),
        None,
        "past the blocks it is looked for in"
    );
}

/// The Rigid Disk Block is looked for in one read of its window, whatever the
/// device's block size, and in all of a device smaller than the window.
#[test]
fn the_rigid_disk_block_is_looked_for_in_one_read() {
    for block_size in [512, 2048, 4096] {
        for (disk, found) in [(vec![0u8; 64 * 1024], false), (rdb_disk(16, 15), true)] {
            let mut dev = device(disk, block_size, None);
            assert_eq!(affs::rigid_disk_block(&mut dev), Ok(found), "{block_size}");
            assert_eq!(
                dev.device().reads,
                1,
                "one transfer at {block_size}-byte blocks"
            );
        }
    }
    let mut small = device(rdb_disk(3, 2), 512, None);
    assert_eq!(affs::rigid_disk_block(&mut small), Ok(true));
    assert_eq!(small.device().reads, 1);
}

/// A sealed AmigaDOS block sums to zero whatever its checksum slot held, and
/// sealing it again changes nothing.
#[test]
fn a_sealed_amiga_block_sums_to_zero_whatever_its_checksum_slot_held() {
    for size in affs::BLOCK_SIZES {
        for at in [affs::BITMAP_CHECKSUM, 8, affs::CHECKSUM] {
            let mut block: Vec<u8> = (0..size as usize)
                .map(|i| (i.wrapping_mul(31) ^ (i >> 3)) as u8)
                .collect();
            affs::put_be32(&mut block, at, 0xDEAD_BEEF);
            affs::seal(&mut block, at);
            assert!(affs::sums_to_zero(&block), "{size} at {at}");
            let sealed = block.clone();
            affs::seal(&mut block, at);
            assert_eq!(block, sealed, "{size} at {at}");
        }
    }
}

/// A record `log2secsize` 9 and `idlen` 15 with `zone_spare` and `nzones`.
fn crafted_record(zone_spare: u16, nzones: u16) -> adfs::DiscRecord {
    let mut record = adfs::DiscRecord::parse(&[0; adfs::DISC_RECORD_SIZE]);
    record.log2secsize = 9;
    record.idlen = 15;
    record.log2bpmb = 7;
    record.zone_spare = zone_spare;
    record.nzones = nzones;
    record.disc_size = 1 << 20;
    record.root = 0x200;
    record
}

/// A record whose zone 0 cannot hold it is no volume: a driver given it
/// would compute where its map lies by subtracting past zero.
#[test]
fn a_disc_record_whose_first_zone_cannot_hold_it_is_no_adfs_volume() {
    for nzones in [2, 1] {
        let record = crafted_record(4048, nzones);
        assert_eq!(
            record.validate(),
            Err(DriverError::BadMagic),
            "{nzones} zones"
        );
        let image = images::adfs_boot_block(&record);
        assert_eq!(
            adfs::identify(&mut device(image.clone(), 512, None)),
            Ok(None)
        );
        assert_eq!(recognise_with(image, 512, None), None);
    }
    // Zone 0 holds the record and exactly one fragment of 16 bits.
    assert_eq!(crafted_record(3600, 2).validate(), Ok(()));
    assert_eq!(
        crafted_record(3601, 2).validate(),
        Err(DriverError::BadMagic)
    );
    // A spare smaller than the zone header puts every zone's stream past its
    // sector.
    for spare in [0, 31] {
        assert_eq!(
            crafted_record(spare, 2).validate(),
            Err(DriverError::BadMagic),
            "{spare}"
        );
    }
    assert_eq!(crafted_record(32, 2).validate(), Ok(()));
}

/// Every record the validator admits has a zone geometry: each zone's stream
/// inside its sector, zone 0's holding a fragment, and where every zone and
/// the map lie computed without overflow. The admitted boundary is pinned at
/// each sector size and id width, so a validator refusing everything fails.
#[test]
fn every_disc_record_validate_admits_has_a_zone_geometry() {
    let mut admitted = 0u64;
    for log2secsize in 8..=10u8 {
        let sector_bits = 8u32 << log2secsize;
        for format_version in [0, 1] {
            let max_idlen = if format_version == 0 { 16 } else { 19 };
            for idlen in log2secsize + 3..=max_idlen {
                let record = |zone_spare: u32, nzones, log2bpmb| {
                    let mut record = adfs::DiscRecord::parse(&[0; adfs::DISC_RECORD_SIZE]);
                    record.log2secsize = log2secsize;
                    record.format_version = format_version;
                    record.idlen = idlen;
                    record.zone_spare = u16::try_from(zone_spare).unwrap_or(u16::MAX);
                    record.nzones = nzones;
                    record.log2bpmb = log2bpmb;
                    record.root = 0x200;
                    record.disc_size = 1 << 20;
                    record
                };
                let fits = sector_bits - adfs::DISC_RECORD_BITS - 1 - u32::from(idlen);
                assert_eq!(
                    record(fits, 2, 6).validate(),
                    Ok(()),
                    "{log2secsize} {idlen}"
                );
                assert_eq!(
                    record(fits + 1, 2, 6).validate(),
                    Err(DriverError::BadMagic),
                    "{log2secsize} {idlen}"
                );
                for zone_spare in (0..=sector_bits).chain([u32::from(u16::MAX)]) {
                    for nzones in [1, 2, 3, 4, 255, 256, 511, 512, u16::MAX] {
                        for log2bpmb in [6, 16] {
                            let record = record(zone_spare, nzones, log2bpmb);
                            if record.validate().is_err() {
                                continue;
                            }
                            admitted += 1;
                            assert!(record.zone_stream_bits(0) > u32::from(idlen));
                            assert!(
                                adfs::ZONE_HEADER_SIZE_U32 * 8 + record.zone_bits() <= sector_bits
                            );
                            let _ = record.map_offset();
                            let nzones = u32::from(nzones);
                            for zone in [0, 1, nzones >> 1, nzones - 1, nzones] {
                                let _ = record.zone_start_block(zone);
                                let _ = record.zone_stream_bits(zone);
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(admitted > 0);
}
