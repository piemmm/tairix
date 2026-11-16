//! Format on the true drive, this tree's KERNAL and BASIC driving it: the
//! disk each `N` leaves, the error it reports, and the tracks it lays out
//! are the original DOS's with the same disk in the same drive.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::disk::{offset, Disk};
use super::writing::{commands, shown, STATUS};
use super::{played, surface};
use crate::machine::c64::drive::Unit;
use crate::machine::c64::Machine;
use crate::media::c1541::layout::{bam, DIRECTORY_TRACK, FIRST_DIRECTORY_SECTOR, PADDING, TRACKS};
use crate::media::c1541::{d64, gcr, Surface};

/// A command sent on the error channel, the error shown once it is done.
fn command(text: &str) -> String {
    format!("8 X$=\"{text}\"|9 OPEN15,8,15,X$:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN 8|")
}

/// The machine after `keys` with `disk` in the drive, `status` shown.
fn after(disk: Surface, keys: &str, status: &str, scene: &str) -> Machine {
    let mut machine = played(disk, None, keys);
    shown(scene, &mut machine, status);
    machine
}

/// The drive's disk as a D64 image.
fn image_of(machine: &Machine) -> Vec<u8> {
    machine
        .drive(Unit::Eight)
        .and_then(|drive| drive.disk())
        .and_then(|disk| d64::encode(disk, TRACKS))
        .expect("the disk encodes as a D64")
}

/// The commands' disk quickly formatted as `name`: its blocks kept, its BAM
/// and first directory sector made afresh with its own ID.
fn quickly(name: &[u8]) -> Vec<u8> {
    let mut image = commands().image();
    let fresh = Disk::new(name, *b"CO").image();
    for sector in [0, FIRST_DIRECTORY_SECTOR] {
        let at = offset(DIRECTORY_TRACK, sector);
        image[at..at + d64::SECTOR_BYTES].copy_from_slice(&fresh[at..at + d64::SECTOR_BYTES]);
    }
    image
}

#[test]
fn a_blank_disk_is_formatted_as_the_original_formats_it() {
    let machine = after(
        Surface::blank(),
        &command("N0:TEST,AB"),
        " 0 OK 0  0",
        "a blank disk",
    );
    assert_eq!(
        image_of(&machine),
        d64::formatted(b"TEST", *b"AB"),
        "the disk formatted"
    );
    let surface = machine
        .drive(Unit::Eight)
        .and_then(|drive| drive.disk())
        .expect("a disk");
    // Track 18 is left to its BAM's and directory's writes after.
    for track in [1, 17, 19, 24, 25, 30, 31, 35] {
        assert_laid_out(surface, track);
    }
}

/// Track `track` as the original lays it out: in sector order, each a SYNC
/// of five bytes, the header, nine bytes of gap, a SYNC and the data block,
/// then the zone's gap, the first sector's SYNC a byte longer and the last
/// gap the rest of the revolution.
fn assert_laid_out(surface: &Surface, track: u8) {
    let ring = surface
        .track(usize::from(track - 1) * 2)
        .expect("the track is recorded");
    let bits = ring.len();
    let bit = |at: usize| ring.bit(at % bits);
    let mut syncs = Vec::new();
    // From outside any SYNC, round once.
    let origin = (0..bits)
        .find(|&at| !bit(at))
        .expect("the track holds a zero");
    let mut at = origin;
    while at < origin + bits {
        if (0..10).all(|ahead| bit(at + ahead)) {
            let start = at;
            while bit(at) {
                at += 1;
            }
            syncs.push((start, at - start, at));
        } else {
            at += 1;
        }
    }
    let sectors = usize::from(d64::sectors(track));
    assert_eq!(syncs.len(), sectors * 2, "track {track}'s SYNCs");
    let decode = |from: usize, bytes: usize| -> Vec<u8> {
        let raw: Vec<u8> = (0..bytes * 10 / 8)
            .map(|byte| {
                (0..8).fold(0, |value, b| {
                    value << 1 | u8::from(bit(from + byte * 8 + b))
                })
            })
            .collect();
        gcr::decode(&raw).unwrap_or_default()
    };
    let gaps = [8, 11, 18, 8][usize::from(d64::zone(track))];
    let mut tails = 0;
    let mut long_syncs = 0;
    for (index, &(start, ones, data)) in syncs.iter().enumerate() {
        let next =
            syncs[(index + 1) % syncs.len()].0 + if index + 1 == syncs.len() { bits } else { 0 };
        let mark = decode(data, 4).first().copied();
        let ending = if mark == Some(0x08) {
            data + 80
        } else {
            data + 2600
        };
        // A gap's byte less the bit its SYNC begins with.
        let gap = (next - ending + 1) / 8;
        match mark {
            Some(0x08) => assert_eq!(gap, 9, "track {track}: the gap after a header at {start}"),
            Some(0x07) if gap == gaps => {}
            Some(0x07) => tails += 1,
            other => panic!("track {track}: a SYNC at {start} before {other:?}"),
        }
        // Five bytes and the gap's last bit, or the first sector's a byte
        // more after up to a byte the head held from reading when it began.
        match ones {
            41 => {}
            48..=56 => long_syncs += 1,
            _ => panic!("track {track}: a SYNC of {ones} ones at {start}"),
        }
    }
    assert_eq!(tails, 1, "track {track}'s tail");
    assert_eq!(long_syncs, 1, "track {track}'s first sector's SYNC");
}

#[test]
fn a_used_disk_is_formatted_anew_and_quickly_as_the_original_does() {
    let full = after(
        surface(&commands().image()),
        &command("N0:NEW,XY"),
        " 0 OK 0  0",
        "a full format",
    );
    assert_eq!(
        image_of(&full),
        d64::formatted(b"NEW", *b"XY"),
        "the disk formatted anew"
    );
    let quick = after(
        surface(&commands().image()),
        &command("N0:NEW"),
        " 0 OK 0  0",
        "a quick format",
    );
    assert_eq!(
        image_of(&quick),
        quickly(b"NEW"),
        "the disk's BAM and directory made afresh"
    );
}

#[test]
fn names_and_ids_are_taken_as_the_original_takes_them() {
    for (text, name, id) in [
        ("N0:A,AB", &b"A"[..], *b"AB"),
        ("N0:ABCDEFGHIJKLMNOPQRS,ABC", b"ABCDEFGHIJKLMNOP", *b"AB"),
        ("N0:TEST,A", b"TEST", [b'A', 0]),
        ("N0:X,,AB", b"X", *b",A"),
    ] {
        let machine = after(
            surface(&commands().image()),
            &command(text),
            " 0 OK 0  0",
            text,
        );
        assert_eq!(image_of(&machine), d64::formatted(name, id), "{text}");
    }
}

#[test]
fn a_name_ending_where_it_starts_is_formatted_as_the_original_formats_it() {
    let bam_at = |at: u8| offset(DIRECTORY_TRACK, 0) + usize::from(at);
    let machine = after(
        surface(&commands().image()),
        &command("N0:X,"),
        " 0 OK 0  0",
        "nothing after the comma",
    );
    assert_eq!(
        image_of(&machine),
        quickly(b"X"),
        "nothing after the comma is quick"
    );
    let machine = after(
        surface(&commands().image()),
        &command("N0:"),
        " 0 OK 0  0",
        "no name",
    );
    let mut nameless = quickly(b"");
    nameless[bam_at(bam::NAME)] = 0;
    assert_eq!(image_of(&machine), nameless, "no name is a zero");
    // A comma first is written with a whole field's padding after it, so
    // the header's padding runs a byte further on.
    for (text, mut disk) in [
        ("N0:,", quickly(b",")),
        ("N0:,AB", d64::formatted(b",", *b"AB")),
    ] {
        disk[bam_at(bam::HEADER_END)] = PADDING;
        let machine = after(
            surface(&commands().image()),
            &command(text),
            " 0 OK 0  0",
            text,
        );
        assert_eq!(image_of(&machine), disk, "{text}");
    }
}

#[test]
fn formats_are_refused_as_the_original_refuses_them() {
    for (scene, protected, text, status) in [
        ("no colon", false, "NEW,AB", " 34 SYNTAX ERROR 0  0"),
        ("the second drive", false, "N1:X,AB", " 21 READ ERROR 0  0"),
        ("a protected disk", true, "N0:X,AB", " 21 READ ERROR 0  0"),
        (
            "a protected disk, quickly",
            true,
            "N0:X",
            " 26 WRITE PROTECT ON 18  0",
        ),
    ] {
        let mut disk = surface(&commands().image());
        disk.write_protected = protected;
        let machine = after(disk, &command(text), status, scene);
        assert_eq!(
            image_of(&machine),
            commands().image(),
            "{scene}: the disk untouched"
        );
    }
    after(
        Surface::blank(),
        &command("N0:X"),
        " 21 READ ERROR 18  0",
        "a blank disk, quickly",
    );
}

#[test]
fn a_format_gives_up_every_open_file_as_the_originals_does() {
    let keys = format!(
        "1 OPEN2,8,2,\"NEWF,S,W\":PRINT#2,\"X\"|2 OPEN15,8,15,\"N0:Q\":CLOSE2|RUN|~~{STATUS}RUN 9|"
    );
    let machine = after(
        surface(&commands().image()),
        &keys,
        " 0 OK 0  0",
        "a file open",
    );
    assert_eq!(
        image_of(&machine),
        quickly(b"Q"),
        "no file closed onto the formatted disk"
    );
}
