//! The controller alone on the drive: a job put in the queue as the
//! interface would, the drive run, and its return code, buffer and disk
//! read back.

use alloc::vec;
use alloc::vec::Vec;

use crate::machine::c64::drive::{Drive, Unit, CLOCK};
use crate::media::c1541::recording::job;
use crate::media::c1541::{d64, Surface};
use crate::rom::c1541::ws::{zp, BUFFERS};

/// The disk's ID, as the BAM keeps it.
const ID: [u8; 2] = *b"XY";

/// A byte of track `track`, sector `sector` on the test disk.
fn pattern(track: u8, sector: u8, at: usize) -> u8 {
    (usize::from(track) * 7 + usize::from(sector) * 13 + at).to_le_bytes()[0]
}

/// A 35-track image whose every sector holds its pattern, the BAM's ID
/// bytes set.
fn image() -> Vec<u8> {
    let mut image = vec![0; 174_848];
    let mut at = 0;
    for track in 1..=35 {
        for sector in 0..d64::sectors(track) {
            for (offset, byte) in image[at..at + 256].iter_mut().enumerate() {
                *byte = pattern(track, sector, offset);
            }
            if (track, sector) == (18, 0) {
                image[at + 0xA2..at + 0xA4].copy_from_slice(&ID);
            }
            at += 256;
        }
    }
    image
}

fn drive(disk: Surface) -> Drive {
    let dos = super::super::build().expect("the DOS assembles");
    let mut drive = Drive::new(&dos.bytes, Unit::Eight).expect("a 16 KiB DOS");
    drive.insert(disk);
    drive.run_until(CLOCK / 10);
    drive.set_ram(zp::DISK_ID, ID[0]);
    drive.set_ram(zp::DISK_ID + 1, ID[1]);
    drive
}

/// Job `code` for buffer `buffer` at `track` and `sector`, run to its end
/// within four seconds: its return code.
fn run_job(drive: &mut Drive, buffer: u8, code: u8, track: u8, sector: u8) -> u8 {
    let header = zp::HEADERS + 2 * u16::from(buffer);
    drive.set_ram(header, track);
    drive.set_ram(header + 1, sector);
    drive.set_ram(zp::JOBS + u16::from(buffer), code);
    let end = drive.cycles() + 4 * CLOCK;
    while drive.ram(zp::JOBS + u16::from(buffer)) & 0x80 != 0 {
        assert!(
            drive.cycles() < end,
            "job {code:02X} at {track}/{sector} finished"
        );
        drive.run_until(drive.cycles() + 1000);
    }
    drive.ram(zp::JOBS + u16::from(buffer))
}

fn buffer(drive: &Drive, buffer: u8) -> Vec<u8> {
    let base = BUFFERS + 0x100 * u16::from(buffer);
    (0..0x100).map(|at| drive.ram(base + at)).collect()
}

fn decoded() -> Surface {
    d64::decode(&image()).expect("a D64")
}

#[test]
fn a_read_fills_the_buffer_with_the_sector() {
    let mut drive = drive(decoded());
    for (track, sector) in [(1, 0), (18, 7), (35, 16), (17, 20)] {
        assert_eq!(
            run_job(&mut drive, 0, 0x80, track, sector),
            job::OK,
            "{track}/{sector}"
        );
        let expected: Vec<u8> = (0..256).map(|at| pattern(track, sector, at)).collect();
        assert_eq!(buffer(&drive, 0), expected, "{track}/{sector}");
    }
}

#[test]
fn a_sector_the_track_lacks_has_no_header() {
    let mut drive = drive(decoded());
    assert_eq!(run_job(&mut drive, 1, 0x80, 1, 21), job::NO_HEADER);
}

#[test]
fn another_disks_id_mismatches() {
    let mut drive = drive(decoded());
    drive.set_ram(zp::DISK_ID, b'Q');
    assert_eq!(run_job(&mut drive, 2, 0x80, 18, 0), job::ID_MISMATCH);
}

#[test]
fn an_unformatted_disk_has_no_sync() {
    let mut drive = drive(Surface::blank());
    assert_eq!(run_job(&mut drive, 0, 0x80, 18, 0), job::NO_SYNC);
}

#[test]
fn a_seek_learns_the_disks_id() {
    let mut drive = drive(decoded());
    drive.set_ram(zp::DISK_ID, 0);
    drive.set_ram(zp::DISK_ID + 1, 0);
    assert_eq!(run_job(&mut drive, 0, 0xB0, 18, 0), job::OK);
    assert_eq!([drive.ram(zp::DISK_ID), drive.ram(zp::DISK_ID + 1)], ID);
}

#[test]
fn a_write_replaces_the_sector_and_a_verify_then_matches() {
    let mut drive = drive(decoded());
    let data: Vec<u8> = (0..=255).map(|byte: u8| byte ^ 0x5A).collect();
    let base = BUFFERS + 0x100;
    for (at, &byte) in (0..).zip(&data) {
        drive.set_ram(base + at, byte);
    }
    assert_eq!(run_job(&mut drive, 1, 0x90, 2, 5), job::OK);
    assert_eq!(buffer(&drive, 1), data, "the buffer decoded back");
    assert_eq!(run_job(&mut drive, 1, 0xA0, 2, 5), job::OK, "verified");
    assert_eq!(run_job(&mut drive, 2, 0x80, 2, 5), job::OK);
    assert_eq!(buffer(&drive, 2), data, "read back");
    drive.set_ram(base, data[0] ^ 1);
    assert_eq!(
        run_job(&mut drive, 1, 0xA0, 2, 5),
        job::VERIFY,
        "a changed buffer differs"
    );
    let saved = d64::encode(drive.disk().expect("the disk"), 35).expect("still a D64");
    let at = (usize::from(d64::sectors(1)) + 5) * 256;
    assert_eq!(
        &saved[at..at + 256],
        &data[..],
        "the image holds the sector written"
    );
}

#[test]
fn a_protected_disk_refuses_a_write() {
    let mut disk = decoded();
    disk.write_protected = true;
    let mut drive = drive(disk);
    assert_eq!(run_job(&mut drive, 0, 0x90, 1, 0), job::WRITE_PROTECT);
}

#[test]
fn a_bump_rests_the_head_on_track_1() {
    let mut drive = drive(decoded());
    assert_eq!(run_job(&mut drive, 0, 0xC0, 0, 0), job::OK);
    assert_eq!(drive.half_track(), 0);
    assert_eq!(drive.ram(zp::HALF_TRACK), 0);
}
