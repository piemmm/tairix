use crate::cpu::{gpr, Bus, Regs};
use crate::dos::formats::Diskette;
use crate::dos::harness::*;
use crate::dos::volume;
use crate::machine::Template;
use alloc::vec;
use tairix_fat::boot::{Bpb, Kind, DOS_BPB};
use tairix_fat::stamp::Stamp;

const PACKET: u16 = DATA;
const TRACK: u16 = DATA + 0x80;

/// Generic IOCTL `function` of category `08h` on drive `drive`, from 1, with
/// `packet` at DS:DX.
fn ioctl(dos: &mut Dos, drive: u16, function: u16, packet: &[u8]) -> Regs {
    dos.put(PACKET, packet);
    dos.call(|r| {
        r.gpr[gpr::AX] = 0x440D;
        r.gpr[gpr::BX] = drive;
        r.gpr[gpr::CX] = 0x0800 | function;
        r.gpr[gpr::DX] = PACKET;
    })
}

/// A device parameter block with special functions `special` and `bpb`.
fn parameters(special: u8, bpb: &[u8; DOS_BPB]) -> [u8; 0x28] {
    let mut packet = [0u8; 0x28];
    packet[0] = special;
    packet[7..7 + DOS_BPB].copy_from_slice(bpb);
    packet
}

fn set(dos: &mut Dos, drive: u16, special: u8, bpb: &[u8; DOS_BPB]) {
    let regs = ioctl(dos, drive, 0x40, &parameters(special, bpb));
    assert!(!carry(&regs), "{regs:X?}");
}

/// What Get Device Parameters answers for `drive`: its type, attributes,
/// cylinders and BPB.
fn got(dos: &mut Dos, drive: u16, special: u8) -> (u8, u16, u16, [u8; DOS_BPB]) {
    let regs = ioctl(dos, drive, 0x60, &[special]);
    assert!(!carry(&regs), "{regs:X?}");
    let packet = dos.get(PACKET, 0x20);
    let mut bpb = [0u8; DOS_BPB];
    bpb.copy_from_slice(&packet[7..7 + DOS_BPB]);
    let word = |at: usize| u16::from_le_bytes([packet[at], packet[at + 1]]);
    (packet[1], word(2), word(4), bpb)
}

/// The clusters function `36h` says `drive`'s volume has.
fn clusters(dos: &mut Dos, drive: u16) -> u16 {
    let regs = dos.call(|r| {
        r.gpr[gpr::AX] = 0x3600;
        r.gpr[gpr::DX] = drive;
    });
    regs.gpr[gpr::DX]
}

/// A format status call's answer for `drive`.
fn status_of(dos: &mut Dos, drive: u16) -> u8 {
    let regs = ioctl(dos, drive, 0x42, &[1, 0, 0, 0, 0, 0, 0]);
    assert!(!carry(&regs), "{regs:X?}");
    dos.get(PACKET, 1)[0]
}

#[test]
fn a_formatter_sets_the_bpb_dos_builds_until_it_sets_the_drives_again() {
    let mut dos = Dos::boot(&[]);
    assert_eq!(
        got(&mut dos, 1, 0),
        (0, 0, 40, Diskette::K360.bpb()),
        "a 360 KiB drive's own"
    );
    assert_eq!(clusters(&mut dos, 1), 354);
    set(&mut dos, 1, 1, &Diskette::K180.bpb());
    assert_eq!(
        got(&mut dos, 1, 1).3,
        Diskette::K180.bpb(),
        "the medium's is the one set"
    );
    assert_eq!(
        clusters(&mut dos, 1),
        351,
        "DOS takes the medium for one of it, reading nothing"
    );
    assert_eq!(
        got(&mut dos, 1, 0).3,
        Diskette::K360.bpb(),
        "the drive's own is kept"
    );
    set(&mut dos, 1, 0, &Diskette::K360.bpb());
    assert_eq!(clusters(&mut dos, 1), 354, "the medium is read again");
    assert_eq!(got(&mut dos, 1, 1).3, Diskette::K360.bpb());
}

#[test]
fn parameters_no_drive_could_take_are_refused_whole() {
    let mut dos = Dos::boot(&[]);
    let mut long = parameters(0, &Diskette::K180.bpb());
    long[0x26] = 64;
    let refused = ioctl(&mut dos, 1, 0x40, &long);
    assert!(
        carry(&refused) && ax(&refused) == 0x1F,
        "a general failure: {refused:X?}"
    );
    assert_eq!(
        got(&mut dos, 1, 0).3,
        Diskette::K360.bpb(),
        "nothing of it was taken"
    );
    let mut odd = Diskette::K360.bpb();
    odd[0..2].copy_from_slice(&1024u16.to_le_bytes());
    let refused = ioctl(&mut dos, 1, 0x40, &parameters(1, &odd));
    assert!(
        carry(&refused) && ax(&refused) == 0x1F,
        "sectors of 1 KiB: {refused:X?}"
    );
    assert_eq!(clusters(&mut dos, 1), 354, "the medium is as it was");
}

#[test]
fn a_status_call_asks_the_bios_whether_it_formats_the_medium_set() {
    let mut dos = Dos::boot(&[]);
    set(&mut dos, 1, 1, &Diskette::K360.bpb());
    assert_eq!(
        status_of(&mut dos, 1),
        0,
        "a 360 KiB medium in its own drive"
    );
    let mut ten = Diskette::K360.bpb();
    ten[13] = 10;
    set(&mut dos, 1, 1, &ten);
    assert_eq!(
        status_of(&mut dos, 1),
        2,
        "no medium of ten sectors a track"
    );
    let config = Template::TurboXt.config();
    let shell = com(idle);
    let mut turbo = Dos::shell_of(bare_on(
        &config,
        image_of(
            &Diskette::K1440.volume(1),
            &[File {
                name: b"COMMAND COM",
                bytes: &shell,
            }],
        ),
    ));
    set(&mut turbo, 2, 1, &Diskette::K1200.bpb());
    assert_eq!(status_of(&mut turbo, 2), 3, "the 1.2 MiB drive is empty");
}

#[test]
fn a_track_is_formatted_with_its_mediums_sectors() {
    // The BIOS formats as many sectors as INT 1Eh's parameters say, nine on
    // every machine, so no high-density track could be formatted.
    let config = Template::TurboXt.config();
    let shell = com(idle);
    let mut dos = Dos::shell_of(bare_on(
        &config,
        image_of(
            &Diskette::K1440.volume(1),
            &[File {
                name: b"COMMAND COM",
                bytes: &shell,
            }],
        ),
    ));
    dos.m
        .board_mut()
        .insert_floppy(1, floppy(vec![0u8; 1200 * 1024]))
        .unwrap();
    let vector = dos.m.board().peek(0x78);
    set(&mut dos, 2, 1, &Diskette::K1200.bpb());
    let formatted = ioctl(&mut dos, 2, 0x42, &[0, 1, 0, 5, 0]);
    assert!(!carry(&formatted), "{formatted:X?}");
    assert_eq!(dos.m.board().peek(0x78), vector, "INT 1Eh is put back");
    let mut read = [0u8; 13];
    read[1] = 1;
    read[3] = 5;
    read[5] = 14;
    read[7] = 1;
    read[9..11].copy_from_slice(&TRACK.to_le_bytes());
    read[11..13].copy_from_slice(&dos.psp.to_le_bytes());
    let sector = ioctl(&mut dos, 2, 0x61, &read);
    assert!(!carry(&sector), "{sector:X?}");
    assert!(
        dos.get(TRACK, 512).iter().all(|&b| b == 0xF6),
        "the fifteenth sector holds the fill byte"
    );
}

#[test]
fn an_unformatted_partition_is_a_drive_with_the_bpb_dos_formats_it_with() {
    let mut dos = Dos::fixed(fixed_image(None));
    let (kind, attributes, cylinders, bpb) = got(&mut dos, 3, 0);
    assert_eq!(
        (kind, attributes, cylinders),
        (5, 1, 64),
        "a fixed disk, its 2,159 sectors on 64 cylinders"
    );
    let bpb = Bpb::from_dos(&bpb).unwrap();
    assert_eq!(
        (
            bpb.kind,
            bpb.sectors_per_cluster,
            bpb.root_entries,
            bpb.media,
            bpb.hidden_sectors,
            bpb.total_sectors
        ),
        (Kind::Fat12, 8, 512, 0xF8, PARTITION_START, 2159)
    );
    assert_eq!(
        status_of(&mut dos, 3),
        1,
        "a fixed disk takes no media type"
    );
}

/// The access flag Get Access Flag answers for `drive`.
fn access(dos: &mut Dos, drive: u16) -> u8 {
    let regs = ioctl(dos, drive, 0x67, &[0, 0xAA]);
    assert!(!carry(&regs), "{regs:X?}");
    dos.get(PACKET, 2)[1]
}

#[test]
fn a_partition_holding_no_volume_reads_as_unformatted_until_one_is_set() {
    let mut dos = Dos::fixed(fixed_image(None));
    assert_eq!(access(&mut dos, 3), 0, "no volume DOS can read");
    assert!(!carry(&ioctl(&mut dos, 3, 0x47, &[0, 1])));
    assert_eq!(
        access(&mut dos, 3),
        1,
        "a program's word lets it be reached"
    );
    assert!(!carry(&ioctl(&mut dos, 3, 0x47, &[0, 0])));
    assert_eq!(access(&mut dos, 3), 0);
    let bpb = got(&mut dos, 3, 0).3;
    set(&mut dos, 3, 1, &bpb);
    assert_eq!(
        access(&mut dos, 3),
        1,
        "a BPB set for the medium is learned"
    );
    let volume = volume::volume(&fixed_volume(0x2026_1110), None, &[], Stamp::NONE).unwrap();
    let mut formatted = Dos::fixed(fixed_image(Some(&volume)));
    assert_eq!(
        access(&mut formatted, 3),
        1,
        "a volume's partition is formatted"
    );
}

#[test]
fn a_fixed_disk_reads_as_changed_once_its_parameters_are_set() {
    let volume = volume::volume(&fixed_volume(0x2026_1110), None, &[], Stamp::NONE).unwrap();
    let mut dos = Dos::fixed(fixed_image(Some(&volume)));
    let own = got(&mut dos, 3, 0).3;
    assert_eq!(
        own,
        fixed_volume(0x2026_1110).dos(),
        "the partition's BPB is the drive's"
    );
    let before = clusters(&mut dos, 3);
    let mut eight = own;
    eight[2] = 8;
    set(&mut dos, 3, 1, &eight);
    assert_eq!(clusters(&mut dos, 3), before / 8, "the BPB set is built");
    set(&mut dos, 3, 0, &own);
    assert_eq!(clusters(&mut dos, 3), before, "and the disk's own again");
}

#[test]
fn several_tracks_are_verified_in_one_call_but_none_formatted() {
    let shell = com(idle);
    let diskette = image_of(
        &diskette(),
        &[File {
            name: b"COMMAND COM",
            bytes: &shell,
        }],
    );
    let mut dos = Dos::shell_of(fixed_machine(fixed_image(None), &[4], diskette));
    let one = ioctl(&mut dos, 3, 0x62, &[0, 0, 0, 1, 0]);
    assert!(!carry(&one), "cylinder 1, head 0: {one:X?}");
    let three = ioctl(&mut dos, 3, 0x62, &[2, 0, 0, 1, 0, 3, 0]);
    assert!(
        carry(&three),
        "the third, cylinder 2's head 0, is bad: {three:X?}"
    );
    let two = ioctl(&mut dos, 3, 0x42, &[2, 0, 0, 1, 0, 2, 0]);
    assert!(!carry(&two), "a fixed disk's are verified: {two:X?}");
    let too_many = ioctl(&mut dos, 3, 0x62, &[2, 0, 0, 0, 0, 16, 0]);
    assert!(
        carry(&too_many) && ax(&too_many) == 0x16,
        "272 sectors in one call: {too_many:X?}"
    );
    let diskette = ioctl(&mut dos, 1, 0x42, &[2, 0, 0, 0, 0, 2, 0]);
    assert!(
        carry(&diskette) && ax(&diskette) == 0x16,
        "a diskette is formatted a track at a time: {diskette:X?}"
    );
}

#[test]
fn a_track_read_names_the_tracks_own_sectors_and_none_past_them() {
    // A sector past the track's last was read from the next head's track.
    let shell = com(idle);
    let mut dos = Dos::shell_of(bare_on(
        &Template::IbmPc.config(),
        image_of(
            &diskette(),
            &[File {
                name: b"COMMAND COM",
                bytes: &shell,
            }],
        ),
    ));
    let image: alloc::vec::Vec<u8> = (0..720u32)
        .flat_map(|lba| [u8::try_from(lba % 251).unwrap(); 512])
        .collect();
    dos.m.board_mut().insert_floppy(1, floppy(image)).unwrap();
    let psp = dos.psp;
    let read = |first: u16, count: u16| {
        let mut packet = [0u8; 13];
        packet[5..7].copy_from_slice(&first.to_le_bytes());
        packet[7..9].copy_from_slice(&count.to_le_bytes());
        packet[9..11].copy_from_slice(&TRACK.to_le_bytes());
        packet[11..13].copy_from_slice(&psp.to_le_bytes());
        packet
    };
    let ninth = ioctl(&mut dos, 2, 0x61, &read(8, 1));
    assert!(!carry(&ninth), "{ninth:X?}");
    assert!(
        dos.get(TRACK, 512).iter().all(|&b| b == 8),
        "the track's ninth sector"
    );
    let tenth = ioctl(&mut dos, 2, 0x61, &read(9, 1));
    assert!(
        carry(&tenth) && ax(&tenth) == crate::dos::device::failed::NOT_FOUND,
        "a nine-sector track has no tenth: {tenth:X?}"
    );
    let across = ioctl(&mut dos, 2, 0x61, &read(7, 4));
    assert!(carry(&across), "a run past the track's end: {across:X?}");
    let beyond = ioctl(&mut dos, 2, 0x61, &read(60, 4));
    assert!(
        carry(&beyond) && ax(&beyond) == crate::dos::device::failed::NOT_FOUND,
        "no CHS address names a 64th sector: {beyond:X?}"
    );
}
