//! The default disk: the drive the controller takes, one active partition
//! over what the BIOS offers, sparse, and a machine started from it at the
//! `C:\>` prompt with the utilities on its path.

use super::*;
use crate::dos::harness::{machine_of, screen, settle, typed};
use crate::machine::{Machine, Template};
use alloc::boxed::Box;
use tairix_partition::mbr::PART_TYPE_FAT16_SMALL;

fn sector(disk: &mut SparseDisk, n: u64) -> [u8; SECTOR] {
    let mut out = [0u8; SECTOR];
    disk.read(n, &mut out).unwrap();
    out
}

#[test]
fn the_disk_is_one_active_partition_over_the_drive_the_bios_offers() {
    let mut disk = default_disk(Recording::Rll, 0x1991_0611).unwrap();
    assert_eq!(disk.kind, ST_238R);
    assert_eq!(
        disk.device.sectors(),
        615 * 4 * 26,
        "every cylinder of the drive"
    );
    let record = sector(&mut disk.device, 0);
    assert_eq!(&record[510..], &[0x55, 0xAA]);
    let entry = &record
        [mbr::PARTITION_TABLE_OFFSET..mbr::PARTITION_TABLE_OFFSET + mbr::PARTITION_ENTRY_LEN];
    assert_eq!(
        (entry[0], entry[4]),
        (0x80, PART_TYPE_FAT16_SMALL),
        "active, FAT16 under 32 MiB"
    );
    let start = u32::from_le_bytes(entry[8..12].try_into().unwrap());
    let sectors = u32::from_le_bytes(entry[12..16].try_into().unwrap());
    assert_eq!(
        (start, start + sectors),
        (26, 614 * 4 * 26),
        "from the second track to the cylinder the BIOS keeps"
    );
    let boot = sector(&mut disk.device, 26);
    let bpb = Bpb::parse(&boot).unwrap();
    assert_eq!(
        (bpb.kind, bpb.media, bpb.hidden_sectors, bpb.serial),
        (Kind::Fat16, format::FIXED_MEDIA, 26, Some(0x1991_0611))
    );
    assert!(
        disk.device.held() < 400,
        "only what it holds kept: {} sectors",
        disk.device.held()
    );
    assert_eq!(
        (released().date, released().time),
        (0x16CB, 0x2800),
        "1991-06-11 05:00"
    );
    let mfm = default_disk(Recording::Mfm, 1).unwrap();
    assert_eq!((mfm.kind, mfm.device.sectors()), (ST_4038, 733 * 5 * 17));
}

/// A machine of `template` with the default disk for its controller,
/// started and at its prompt.
fn started(template: Template) -> Machine {
    let config = template.config();
    let recording = config.fixed.unwrap().recording;
    let disk = default_disk(recording, 0x2026_1111).unwrap();
    let mut m = machine_of(&config, [Some(disk.kind), None]);
    m.board_mut()
        .attach_fixed(0, disk.kind, Box::new(disk.device), disk.low_level)
        .unwrap();
    settle(&mut m);
    m
}

#[test]
fn a_machine_starts_from_its_default_disk_at_the_c_prompt() {
    for template in [Template::TurboXt, Template::IbmXt] {
        let mut m = started(template);
        let text = screen(&m);
        assert!(
            text.contains("TAIRiX Mouse driver installed"),
            "{template:?}:\n{text}"
        );
        assert!(text.trim_end().ends_with("C:\\>"), "{template:?}:\n{text}");
        typed(&mut m, "cls");
        typed(&mut m, "dir /w");
        let text = screen(&m);
        assert!(
            text.contains("Volume in drive C is TAIRIX DOS"),
            "{template:?}:\n{text}"
        );
        assert!(
            text.contains("COMMAND.COM") && text.contains("[DOS]"),
            "{template:?}:\n{text}"
        );
        typed(&mut m, "cls");
        typed(&mut m, "chkdsk");
        let text = screen(&m);
        assert!(
            text.contains("bytes total disk space") && !text.contains("lost"),
            "{template:?}: a sound volume:\n{text}"
        );
        typed(&mut m, "cls");
        typed(&mut m, "mem");
        assert!(
            screen(&m).contains("bytes total conventional memory"),
            "{template:?}: a utility found on the path:\n{}",
            screen(&m)
        );
    }
}

#[test]
fn the_default_disks_fat16_volume_takes_writes_and_stays_sound() {
    let mut m = started(Template::Workstation186);
    for line in [
        "md c:\\work",
        "copy c:\\dos\\*.* c:\\work",
        "copy c:\\work\\edit.com c:\\work\\again.com",
        "del c:\\work\\more.com",
    ] {
        typed(&mut m, line);
    }
    typed(&mut m, "cls");
    typed(&mut m, "dir c:\\work");
    let text = screen(&m);
    assert!(
        text.contains("AGAIN    COM") && !text.contains("MORE     COM"),
        "{text}"
    );
    typed(&mut m, "cls");
    typed(&mut m, "chkdsk c:");
    let text = screen(&m);
    assert!(
        text.contains("bytes total disk space") && !text.contains("lost") && !text.contains("rror"),
        "{text}"
    );
}
