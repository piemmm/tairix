//! The volume's title: the logical volume identifier, with its copies in the
//! logical volume information and the file set descriptor, and the primary
//! volume descriptor's volume identifier, in both descriptor sequences.

use alloc::string::String;

use super::*;
use tairix_abi::driver::filesystem::{FilesystemLabel, VolumeTitle};
use tairix_fsprobe::udf::{
    cs0_chars, dstring, ExtentAd, Tag, TAG_FSD, TAG_IUVD, TAG_LVD, TAG_PD, TAG_PVD,
};
use tairix_fsprobe::{ProbedVolume, Recognised};

fn title(text: &str) -> VolumeTitle {
    VolumeTitle::new(text).expect("a title")
}

fn label_of(fs: &mut Udf<Disk>) -> String {
    String::from(fs.label().expect("a title").as_str())
}

/// The text the `dstring` `field` records.
fn text(field: &[u8]) -> String {
    cs0_chars(dstring(field)).map_or_else(String::new, |chars| {
        chars.map(|ch| ch.expect("a character")).collect()
    })
}

/// Every descriptor of `kind` in the two sequences the anchor at sector 256
/// names, each checked as recorded where it lies.
fn descriptors(image: &[u8], bs: usize, kind: u16) -> Vec<Vec<u8>> {
    let anchor = &image[256 * bs..257 * bs];
    let mut found = Vec::new();
    for at in [16, 24] {
        let extent = ExtentAd::at(anchor, at).expect("an extent");
        for index in 0..extent.length as usize / bs {
            let sector = extent.location as usize + index;
            let block = &image[sector * bs..(sector + 1) * bs];
            let Some(tag) = Tag::verify(block, sector as u32) else {
                break;
            };
            if tag.id == kind {
                found.push(block.to_vec());
            }
        }
    }
    found
}

/// The file set descriptor the logical volume names, checked as recorded.
fn file_set(image: &[u8], bs: usize) -> Vec<u8> {
    let word = |bytes: &[u8], at: usize| {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("a word")) as usize
    };
    let lvds = descriptors(image, bs, TAG_LVD);
    let partitions = descriptors(image, bs, TAG_PD);
    let block = word(&lvds[0], 252);
    let sector = word(&partitions[0], 188) + block;
    let fsd = &image[sector * bs..(sector + 1) * bs];
    let tag = Tag::verify(fsd, block as u32).expect("sealed");
    assert_eq!(tag.id, TAG_FSD);
    fsd.to_vec()
}

/// The volume the probe reads from `bytes`.
fn probed(bytes: Vec<u8>, block_size: u32) -> ProbedVolume {
    let disk = Disk::new(block_size, bytes);
    let mut dev = tairix_bytedev::ByteDevice::new(disk).expect("geometry");
    match tairix_fsprobe::recognise(&mut dev) {
        Ok(Some(Recognised::Volume(volume))) => volume,
        other => panic!("no UDF volume: {other:?}"),
    }
}

/// A title is recorded whole in the logical volume identifier of both
/// sequences' logical volume descriptors and logical volume information and
/// of the file set descriptor, each resealed, and reads back from the mount,
/// a remount and the probe.
#[test]
fn a_title_is_recorded_wherever_the_volume_names_itself() {
    for (block_size, revision) in [(512, 0x0201), (2048, 0x0150)] {
        let mut fs = formatted(block_size, revision);
        fs.set_label(&title("Family Photos")).expect("retitled");
        assert_eq!(label_of(&mut fs), "Family Photos");
        let mut fs = remount(fs);
        assert_eq!(fs.write_refusal(), None, "closed again");
        assert_eq!(label_of(&mut fs), "Family Photos");
        let image = fs.into_device().bytes;
        let bs = block_size as usize;
        for (kind, field) in [(TAG_PVD, 24..56), (TAG_LVD, 84..212), (TAG_IUVD, 116..244)] {
            let found = descriptors(&image, bs, kind);
            assert_eq!(found.len(), 2, "one in each sequence");
            for descriptor in found {
                assert_eq!(text(&descriptor[field.clone()]), "Family Photos");
            }
        }
        assert_eq!(text(&file_set(&image, bs)[112..240]), "Family Photos");
        assert_eq!(probed(image, block_size).label(), b"Family Photos");
    }
}

/// The volume identifier holds 30 eight-bit characters or 15 UTF-16 units,
/// so a longer title keeps its longest leading part there, in whole
/// characters and without a closing space, and is whole in the logical
/// volume identifier every reader takes it from — by a retitle and by the
/// formatter alike.
#[test]
fn a_title_past_the_volume_identifier_keeps_its_leading_part_there() {
    let wide = "中".repeat(20);
    let wide_leading = "中".repeat(15);
    for (whole, leading) in [
        (
            "Holiday pictures from the summer of 2025",
            "Holiday pictures from the summ",
        ),
        (
            "abcdefghijklmnopqrstuvwxyz123 tail",
            "abcdefghijklmnopqrstuvwxyz123",
        ),
        (wide.as_str(), wide_leading.as_str()),
    ] {
        let mut retitled = formatted(512, 0x0201);
        retitled.set_label(&title(whole)).expect("retitled");
        let laid_out = Udf::format(
            Disk::new(512, vec![0u8; 8 << 20]),
            &FormatOptions {
                label: whole,
                ..options(0x0201)
            },
        )
        .expect("formats");
        for mut fs in [remount(retitled), laid_out] {
            assert_eq!(label_of(&mut fs), whole);
            let image = fs.into_device().bytes;
            for pvd in descriptors(&image, 512, TAG_PVD) {
                assert_eq!(text(&pvd[24..56]), leading);
            }
        }
    }
}

/// A title the logical volume identifier cannot hold is refused and nothing
/// written: past 63 UTF-16 units once a character needs them, and closing
/// with a space, which readers take for padding.
#[test]
fn a_title_the_logical_volume_identifier_cannot_hold_is_refused() {
    let mut fs = formatted(512, 0x0201);
    let before = fs.dev.device().bytes.clone();
    let fits = alloc::format!("中{}", "x".repeat(62));
    let over = alloc::format!("中{}", "x".repeat(63));
    for (asked, refusal) in [
        (over.as_str(), DriverError::LengthOutOfRange),
        ("Photos ", DriverError::OutOfRange),
    ] {
        assert_eq!(fs.set_label(&title(asked)), Err(refusal), "{asked}");
    }
    assert_eq!(fs.dev.device().bytes, before);
    fs.set_label(&title(&fits)).expect("63 units fit");
    assert_eq!(label_of(&mut fs), fits);
    let narrow = "x".repeat(126);
    fs.set_label(&title(&narrow)).expect("126 characters fit");
    assert_eq!(label_of(&mut remount(fs)), narrow);
}

/// The longest titles the logical volume identifier holds are titles, though
/// their UTF-8 runs past their characters: 126 eight-bit characters, and 63
/// UTF-16 units.
#[test]
fn the_longest_titles_the_identifier_holds_are_titles() {
    let mut fs = formatted(512, 0x0201);
    for whole in ["é".repeat(126), "中".repeat(63)] {
        fs.set_label(&title(&whole)).expect("held whole");
        assert_eq!(label_of(&mut fs), whole);
    }
    assert_eq!(label_of(&mut remount(fs)), "中".repeat(63));
}

/// A retitle changes neither field the volume's identity is a digest of — the
/// volume set identifier and the recording time — so the mount, a remount and
/// the volume probe all find the identity it had.
#[test]
fn a_retitle_keeps_the_identity() {
    let mut fs = formatted(512, 0x0201);
    let identity = fs.volume_identity();
    fs.set_label(&title("Renamed")).expect("retitled");
    assert_eq!(fs.volume_identity(), identity);
    let fs = remount(fs);
    assert_eq!(fs.volume_identity(), identity);
    assert_eq!(probed(fs.into_device().bytes, 512).identity, identity);
}

/// The empty title clears both identifiers, and the volume reads untitled.
#[test]
fn the_empty_title_clears_the_identifiers() {
    let mut fs = formatted(2048, 0x0201);
    fs.set_label(&title("")).expect("cleared");
    let mut fs = remount(fs);
    assert_eq!(label_of(&mut fs), "");
    let image = fs.into_device().bytes;
    for (kind, field) in [(TAG_PVD, 24..56), (TAG_LVD, 84..212)] {
        for descriptor in descriptors(&image, 2048, kind) {
            assert!(descriptor[field.clone()].iter().all(|&byte| byte == 0));
        }
    }
    assert!(probed(image, 2048).label().is_empty());
}

/// A volume mounted read-only refuses a title and is left as it was.
#[test]
fn a_volume_mounted_read_only_refuses_a_title() {
    let image = sparse::sparse(include_bytes!(
        "../../../iso9660/tests/fixtures/bridge.sparse"
    ));
    let mut fs = Udf::open(Disk::new(2048, image.clone())).expect("mounts");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::WriteProtected));
    let named = fs.label();
    assert_eq!(fs.set_label(&title("Other")), Err(DriverError::ReadOnly));
    assert_eq!(fs.label(), named);
    assert_eq!(fs.into_device().bytes, image);
}

/// A volume the kernel serves read-only refuses a title before it reads or
/// writes anything, though the format would take one.
#[test]
fn a_volume_served_read_only_refuses_a_title_at_once() {
    use tairix_fsserve::{ServedFormat, WritebackBudget};
    let budget = WritebackBudget {
        backing: 1 << 30,
        gauge: &tairix_reclaim::Unpressured,
    };
    let image = formatted(512, 0x0201).into_device();
    let mut served = crate::Served::open(
        Disk::new(512, image.bytes.clone()),
        true,
        budget,
        &tairix_log::DiscardSink,
    )
    .expect("opens read-only");
    assert_eq!(served.write_refusal, None, "the format refuses nothing");
    assert_eq!(
        served.fs.write_refusal(),
        Some(WriteRefusal::OpenedReadOnly)
    );
    assert_eq!(
        served.fs.set_label(&title("Other")),
        Err(DriverError::ReadOnly)
    );
    assert_eq!(served.fs.into_device().bytes, image.bytes);
}

/// The volume identifier's share of a title is cut on a character, and
/// counts UTF-16 units once a character needs sixteen bits.
#[test]
fn the_leading_part_is_cut_in_whole_characters() {
    use crate::title::leading;
    let narrow = "é".repeat(40);
    assert_eq!(leading(&narrow, 32), "é".repeat(30));
    let widening = alloc::format!("{}中", "x".repeat(29));
    assert_eq!(leading(&widening, 32), "x".repeat(29));
    let astral = "😀".repeat(10);
    assert_eq!(leading(&astral, 32), "😀".repeat(7));
    let fits = "中".repeat(20);
    assert_eq!(leading(&fits, 128), fits);
}
