//! The volume's title: the name its root block records.

use super::*;
use tairix_abi::driver::filesystem::{FilesystemLabel, VolumeTitle};
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::{ProbedVolume, Recognised};

fn title(text: &str) -> VolumeTitle {
    VolumeTitle::new(text).expect("a title")
}

fn label_of(fs: &mut Affs<MemDisk>) -> String {
    String::from(fs.label().expect("a title").as_str())
}

/// The volume the probe reads from `data`.
fn probed(data: Vec<u8>) -> ProbedVolume {
    let disk = MemDisk {
        data,
        block_size: 512,
    };
    let mut dev = ByteDevice::new(disk).expect("a device");
    match tairix_fsprobe::recognise(&mut dev) {
        Ok(Some(Recognised::Volume(volume))) => volume,
        other => panic!("no AmigaDOS volume: {other:?}"),
    }
}

/// A title reads back from the mount, a remount the validator passes, and
/// the volume probe, on every variant.
#[test]
fn a_title_reads_back_from_the_mount_a_remount_and_the_probe() {
    for (dos_type, block_size, bytes) in VARIANTS {
        let mut fs = fresh(dos_type, block_size, bytes);
        fs.set_label(&title("Disque Été")).expect("retitled");
        assert_eq!(label_of(&mut fs), "Disque Été");
        let mut fs = check_fs(fs);
        assert_eq!(label_of(&mut fs), "Disque Été");
        let (data, _) = image(fs);
        assert_eq!(probed(data).label(), "Disque Été".as_bytes());
    }
}

/// Retitling alters the root: its date and the volume's alteration date
/// take the host's time, and the creation date AmigaDOS tells volumes apart
/// by is kept.
#[test]
fn a_title_restamps_the_root_and_keeps_its_creation_date() {
    let mut fs = floppy(1);
    let when = Time64::new(1_700_000_000, 0).expect("a time");
    fs.set_volume_host(TestClock::volume(), TestClock::at(Some(when)));
    fs.set_label(&title("Dated")).expect("retitled");
    let (data, bs) = image(fs);
    let at = Offsets::new(bs);
    let root = root_block((data.len() / bs) as u64) as usize;
    let block = &data[root * bs..(root + 1) * bs];
    let stamped = crate::stamp(Some(when));
    assert_ne!(stamped, DateStamp::default(), "a time a stamp holds");
    assert_eq!(DateStamp::read(block, at.volume_date()), stamped);
    assert_eq!(DateStamp::read(block, at.date()), stamped);
    assert_eq!(DateStamp::read(block, at.created()), created());
}

/// A retitle changes no part of the volume's identity, which its geometry and
/// its root's creation date make: a remount and the volume probe find the one
/// it had, on every variant.
#[test]
fn a_retitle_keeps_the_identity() {
    for (dos_type, block_size, bytes) in VARIANTS {
        let mut fs = fresh(dos_type, block_size, bytes);
        let identity = fs.layout.identity();
        fs.set_label(&title("Renamed")).expect("retitled");
        let fs = check_fs(fs);
        assert_eq!(fs.layout.identity(), identity, "DOS\\{dos_type}");
        let (data, _) = image(fs);
        assert_eq!(probed(data).identity, identity, "DOS\\{dos_type}");
    }
}

/// A name AmigaDOS cannot record is refused and nothing written: none, one
/// past 30 bytes, one with the `:` or `/` it addresses a volume and
/// separates a path with, and one with a character ISO 8859-1 lacks.
#[test]
fn a_title_the_volume_cannot_hold_is_refused_and_nothing_written() {
    let mut fs = floppy(1);
    let before = fs.disk.dev.device().data.clone();
    let long = "N".repeat(31);
    for (text, refusal) in [
        ("", DriverError::LengthOutOfRange),
        (long.as_str(), DriverError::LengthOutOfRange),
        ("Work:", DriverError::OutOfRange),
        ("Work/Play", DriverError::OutOfRange),
        ("Ωmega", DriverError::OutOfRange),
    ] {
        assert_eq!(fs.set_label(&title(text)), Err(refusal), "{text}");
    }
    assert_eq!(fs.disk.dev.device().data, before);
    assert_eq!(label_of(&mut fs), "Work");
    let thirty = "N".repeat(30);
    fs.set_label(&title(&thirty)).expect("thirty fit");
    assert_eq!(label_of(&mut fs), thirty);
}

/// A volume the driver does not write refuses a title: a directory-cache
/// volume, and one served read-only.
#[test]
fn a_volume_the_driver_does_not_write_refuses_a_title() {
    let (mut data, _) = image(floppy(1));
    data[3] = 4;
    let disk = MemDisk {
        data: data.clone(),
        block_size: 512,
    };
    let mut cached = Affs::open(disk).expect("opens");
    let refusal = cached.write_refusal();
    assert_eq!(refusal, Some(WriteRefusal::UnsupportedFeature));
    let refused = cached.set_label(&title("Other"));
    assert_eq!(refused, Err(DriverError::ReadOnly));
    assert_eq!(cached.disk.dev.device().data, data);

    let mut served = floppy(1);
    served.refuse_writes(WriteRefusal::OpenedReadOnly);
    let before = served.disk.dev.device().data.clone();
    let refused = served.set_label(&title("Other"));
    assert_eq!(refused, Err(DriverError::ReadOnly));
    assert_eq!(served.disk.dev.device().data, before);
    assert_eq!(label_of(&mut served), "Work");
}

/// A soft link names its own volume by name, so after a retitle the new
/// name resolves within the volume and the old one names another.
#[test]
fn a_soft_link_naming_the_volume_follows_its_title() {
    let mut fs = floppy(1);
    let root = fs.root();
    mkdir(&mut fs, root, "Docs");
    let fs = forge_link(fs, b"renamed", ST_SOFTLINK, 0, b"Archive:Docs");
    let mut fs = forge_link(fs, b"former", ST_SOFTLINK, 0, b"Work:Docs");
    fs.set_label(&title("Archive")).expect("retitled");
    let root = fs.root();
    let mut out = [0u8; 64];
    for (name, expected) in [(&b"renamed"[..], &b"/Docs"[..]), (b"former", b"Work:Docs")] {
        let link = fs.lookup(root, name).expect("found");
        let len = fs.read_link(link, &mut out).expect("read");
        assert_eq!(&out[..len], expected);
    }
}
