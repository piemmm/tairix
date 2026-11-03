//! Volumes the driver reads but does not write: images short of the disc
//! they hold, directories with entries left past their end marker, and maps
//! whose two copies disagree — and the second map copy every write keeps.

use super::*;
use std::sync::Arc;
use tairix_abi::driver::filesystem::WriteRefusal;
use tairix_abi::volume::VolumeFsType;
use tairix_fsprobe::adfs::{DiscRecord, MapCopy, OLD_SECTOR_SIZE};
use tairix_fsprobe::Recognised;

fn device(data: Vec<u8>) -> MemDisk {
    MemDisk {
        data,
        block_size: 512,
        reads: Arc::default(),
    }
}

fn reopened(data: Vec<u8>) -> Adfs<MemDisk> {
    Adfs::open(device(data)).expect("the volume opens")
}

fn variant_bytes(variant: AdfsVariant) -> usize {
    VARIANTS
        .iter()
        .find(|(listed, _)| *listed == variant)
        .map(|&(_, bytes)| bytes)
        .expect("a listed variant")
}

fn map_record(fs: &Adfs<MemDisk>) -> DiscRecord {
    match &fs.backing {
        Backing::New { map, .. } => map.record,
        Backing::Old { .. } => panic!("an old-map volume has no disc record"),
    }
}

fn zone_bytes(image: &[u8], record: &DiscRecord, copy: MapCopy, zone: u32) -> Vec<u8> {
    let at = usize::try_from(record.zone_offset(copy, zone)).expect("fits");
    let len = usize::try_from(record.sector_size()).expect("fits");
    image[at..at + len].to_vec()
}

fn root_dir_offset(fs: &mut Adfs<MemDisk>) -> usize {
    let root = fs.root_addr();
    let at = match &fs.backing {
        Backing::Old { root_sector, .. } => u64::from(*root_sector) * OLD_SECTOR_SIZE,
        Backing::New { map, .. } => map.locate(&mut fs.volume, root, 0).expect("root").0,
    };
    usize::try_from(at).expect("fits")
}

fn names(fs: &mut Adfs<MemDisk>, dir: NodeId) -> Vec<Vec<u8>> {
    list(fs, dir).into_iter().map(|(name, ..)| name).collect()
}

/// Rewrite the directory at `at` in `image`: the entries once at the `live`
/// slots first, then the end marker, then each `(slot, once_at)` of
/// `stranded` past it — sealed as a writer that saw only the live ones
/// seals it.
fn strand(
    image: &mut [u8],
    at: usize,
    format: FixedFormat,
    live: &[usize],
    stranded: &[(usize, usize)],
) {
    let size = format.size();
    let slot = |index: usize| dir::ENTRIES_OFFSET + index * dir::ENTRY_SIZE;
    let original = image[at..at + size].to_vec();
    let mut raw = [0u8; dir::NEW_DIR_SIZE];
    raw[..size].copy_from_slice(&original);
    raw[slot(0)..slot(format.capacity())].fill(0);
    let placed = (0..)
        .zip(live.iter().copied())
        .chain(stranded.iter().copied());
    for (to, from) in placed {
        raw[slot(to)..slot(to + 1)].copy_from_slice(&original[slot(from)..slot(from + 1)]);
    }
    let unsealed = dir::FixedDir {
        data: raw,
        format,
        stranded: false,
    };
    raw[size - 1] = unsealed.check_byte();
    image[at..at + size].copy_from_slice(&raw[..size]);
}

/// Entries packed at the end of a directory's table behind an empty prefix,
/// sealed over that prefix — a layout protected discs carry — are listed and
/// read, and the volume then refuses every write: writing the directory back
/// would vouch for entries no check byte covered.
#[test]
fn entries_left_past_an_end_marker_are_listed_and_the_volume_is_not_written() {
    for variant in [
        AdfsVariant::S,
        AdfsVariant::L,
        AdfsVariant::D,
        AdfsVariant::E,
        AdfsVariant::F,
        AdfsVariant::HardDisc,
    ] {
        let mut fs = fresh(variant, variant_bytes(variant));
        let root = FilesystemRead::root(&fs);
        for name in [b"Alpha".as_slice(), b"Beta", b"Gamma"] {
            make(&mut fs, root, name, name);
        }
        let format = fs.fixed_format();
        let at = root_dir_offset(&mut fs);
        let last = format.capacity() - 3;
        let mut image = fs.volume.into_inner().data;
        strand(
            &mut image,
            at,
            format,
            &[],
            &[(last, 0), (last + 1, 1), (last + 2, 2)],
        );

        let mut fs = reopened(image);
        assert_eq!(
            fs.write_refusal(),
            Some(WriteRefusal::UnsupportedFeature),
            "{variant:?}"
        );
        assert_eq!(
            names(&mut fs, root),
            [b"Alpha".to_vec(), b"Beta".to_vec(), b"Gamma".to_vec()],
            "{variant:?}"
        );
        let beta = fs.lookup(root, b"Beta").expect("found");
        assert_eq!(read_all(&mut fs, beta), b"Beta", "{variant:?}");
        assert_eq!(
            fs.create(root, b"Delta", NodeKind::RegularFile, &FILE_RECORD),
            Err(DriverError::ReadOnly),
            "{variant:?}"
        );
        assert_eq!(fs.write_at(beta, 0, b"b"), Err(DriverError::ReadOnly));
        assert_eq!(fs.truncate(beta, 0), Err(DriverError::ReadOnly));
        assert_eq!(fs.remove(root, b"Beta"), Err(DriverError::ReadOnly));
        assert_eq!(
            fs.rename(root, b"Beta", root, b"BETA"),
            Err(DriverError::ReadOnly),
            "a respelling is a write too"
        );
        assert_eq!(
            fs.rename(root, b"Beta", root, b"Omega"),
            Err(DriverError::ReadOnly)
        );
        assert_eq!(
            fs.set_attr(beta, b"acorn.filetype", b"fff"),
            Err(DriverError::ReadOnly)
        );
        assert_eq!(read_all(&mut fs, beta), b"Beta", "nothing was written");
    }
}

/// Entries past the end marker that would not continue the directory's
/// order are left out — the live ones still list — and the volume still
/// refuses writes, any of which could join them to the list.
#[test]
fn entries_past_an_end_marker_out_of_order_are_left_out_and_still_refuse_writes() {
    let mut fs = fresh(AdfsVariant::E, 800 * 1024);
    let root = FilesystemRead::root(&fs);
    for name in [b"Alpha".as_slice(), b"Beta", b"Gamma"] {
        make(&mut fs, root, name, name);
    }
    let format = fs.fixed_format();
    let at = root_dir_offset(&mut fs);
    let mut image = fs.volume.into_inner().data;
    // Beta stays live; Alpha would follow it out of order.
    strand(&mut image, at, format, &[1], &[(9, 0), (10, 2)]);

    let mut fs = reopened(image);
    assert_eq!(names(&mut fs, root), [b"Beta".to_vec()]);
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
    assert_eq!(
        fs.create(root, b"Delta", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::ReadOnly)
    );
}

/// An image short of the disc it holds — a dump missing its final, free
/// track — is recognised, listed and read, and never written: an allocation
/// could land past its end.
#[test]
fn an_image_short_of_its_disc_is_read_and_never_written() {
    for variant in [
        AdfsVariant::S,
        AdfsVariant::L,
        AdfsVariant::D,
        AdfsVariant::E,
        AdfsVariant::F,
    ] {
        let bytes = variant_bytes(variant);
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        make(&mut fs, root, b"Kept", b"kept bytes");
        let mut image = fs.volume.into_inner().data;
        image.truncate(bytes - 5 * 1024);

        let mut probed = ByteDevice::new(device(image.clone())).expect("a device");
        assert!(
            matches!(
                tairix_fsprobe::recognise(&mut probed),
                Ok(Some(Recognised::Volume(volume))) if volume.fstype == VolumeFsType::Adfs
            ),
            "{variant:?} is recognised"
        );
        let mut fs = reopened(image);
        assert_eq!(
            fs.write_refusal(),
            Some(WriteRefusal::Truncated),
            "{variant:?}"
        );
        let kept = fs.lookup(root, b"Kept").expect("listed");
        assert_eq!(read_all(&mut fs, kept), b"kept bytes", "{variant:?}");
        assert_eq!(
            fs.create(root, b"More", NodeKind::RegularFile, &FILE_RECORD),
            Err(DriverError::ReadOnly),
            "{variant:?}"
        );
        assert_eq!(fs.write_at(kept, 0, b"K"), Err(DriverError::ReadOnly));
    }
}

/// The map is written twice, so either copy stands for it: formatting and
/// every write since leave the two copies identical.
#[test]
fn every_write_keeps_both_map_copies_identical() {
    for variant in [
        AdfsVariant::E,
        AdfsVariant::EPlus,
        AdfsVariant::F,
        AdfsVariant::FPlus,
        AdfsVariant::HardDisc,
        AdfsVariant::HardDiscPlus,
    ] {
        let mut fs = fresh(variant, variant_bytes(variant));
        let root = FilesystemRead::root(&fs);
        make(&mut fs, root, b"Grown", &vec![7; 40_000]);
        let small = make(&mut fs, root, b"Small", b"x");
        fs.truncate(small, 9_000).expect("grows");
        fs.remove(root, b"Grown").expect("removed");
        let record = map_record(&fs);
        let image = fs.volume.into_inner().data;
        for zone in 0..u32::from(record.nzones) {
            assert_eq!(
                zone_bytes(&image, &record, MapCopy::First, zone),
                zone_bytes(&image, &record, MapCopy::Second, zone),
                "{variant:?} zone {zone}"
            );
        }
    }
}

/// A first copy with a zone failing its check is read past to the second,
/// as `FileCore` does, and writes made meanwhile reach both; with both
/// copies damaged the volume is refused.
#[test]
fn a_first_map_copy_failing_its_check_is_read_past_to_the_second() {
    for variant in [AdfsVariant::F, AdfsVariant::HardDisc] {
        let bytes = variant_bytes(variant);
        let record =
            mkfs::new_map_record(variant, u64::try_from(bytes).expect("fits")).expect("a record");
        let flip = |copy| usize::try_from(record.zone_offset(copy, 1)).expect("fits") + 0x40;
        let mut image = image_of(variant, bytes);
        image[flip(MapCopy::First)] ^= 0x01;

        let mut fs = reopened(image.clone());
        assert_eq!(fs.write_refusal(), None, "{variant:?}");
        let root = FilesystemRead::root(&fs);
        let file = fs.lookup(root, b"File").expect("listed");
        assert_eq!(read_all(&mut fs, file), b"survivor", "{variant:?}");
        let later_bytes = vec![3u8; 30_000];
        make(&mut fs, root, b"Later", &later_bytes);
        let mut fs = reopened(fs.volume.into_inner().data);
        let later = fs.lookup(root, b"Later").expect("kept");
        assert_eq!(read_all(&mut fs, later), later_bytes, "{variant:?}");

        image[flip(MapCopy::Second)] ^= 0x01;
        assert_rejected(variant, image, "both map copies damaged");
    }
}

/// Copies that both check yet differ are what some protected discs carry:
/// the first records an empty disc and the second the files. An object only
/// the second records is read through it, and the volume is never written,
/// which would allocate over what only the second records.
#[test]
fn map_copies_that_both_check_yet_differ_recover_objects_and_refuse_writes() {
    for variant in [AdfsVariant::E, AdfsVariant::F] {
        let mut fs = fresh(variant, variant_bytes(variant));
        let root = FilesystemRead::root(&fs);
        make(&mut fs, root, b"Before", b"before");
        let record = map_record(&fs);
        let sector = usize::try_from(record.sector_size()).expect("fits");
        let earlier: Vec<Vec<u8>> = (0..u32::from(record.nzones))
            .map(|zone| {
                let mut bytes = vec![0u8; sector];
                fs.volume
                    .read(record.zone_offset(MapCopy::First, zone), &mut bytes)
                    .expect("zone");
                bytes
            })
            .collect();
        let hidden = vec![9u8; 30_000];
        make(&mut fs, root, b"Hidden", &hidden);
        let mut image = fs.volume.into_inner().data;
        for (zone, bytes) in (0u32..).zip(&earlier) {
            let at = usize::try_from(record.zone_offset(MapCopy::First, zone)).expect("fits");
            image[at..at + sector].copy_from_slice(bytes);
        }

        let mut fs = reopened(image);
        assert_eq!(
            fs.write_refusal(),
            Some(WriteRefusal::UnsupportedFeature),
            "{variant:?}"
        );
        let found = fs.lookup(root, b"Hidden").expect("listed");
        assert_eq!(read_all(&mut fs, found), hidden, "{variant:?}");
        assert_eq!(
            fs.create(root, b"Over", NodeKind::RegularFile, &FILE_RECORD),
            Err(DriverError::ReadOnly),
            "{variant:?}"
        );
    }
}
