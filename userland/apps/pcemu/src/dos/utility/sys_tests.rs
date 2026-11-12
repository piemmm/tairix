//! `SYS`: its command line, then each transfer run at the shell's prompt
//! onto a diskette in drive B:.

use super::*;
use crate::dos::formats::Diskette;
use crate::dos::harness::{diskette_bytes, floppy, screen, typed};
use crate::dos::volume;
use crate::machine::Machine;
use alloc::string::String;
use tairix_fat::boot::at as boot_at;

#[test]
fn sys_reads_its_source_and_target() {
    assert_eq!(
        request(b" b:"),
        Ok(Request {
            source: None,
            target: 1
        })
    );
    assert_eq!(
        request(b" a: b:"),
        Ok(Request {
            source: Some(b"a:"),
            target: 1
        })
    );
    assert_eq!(
        request(b" c:\\dos\\ a:"),
        Ok(Request {
            source: Some(b"c:\\dos\\"),
            target: 0
        })
    );
    assert_eq!(request(b""), Err(Refusal::Missing));
    assert_eq!(
        request(b" c:\\dos"),
        Err(Refusal::Missing),
        "a path needs the target after it"
    );
    assert_eq!(request(b" a: b:x"), Err(Refusal::Format(b"b:x")));
    assert_eq!(request(b" a: b: c:"), Err(Refusal::TooMany(b"c:")));
    assert_eq!(request(b" b: /s"), Err(Refusal::Switch(b"/s")));
    assert_eq!(source_path(b"a:"), b"a:\\COMMAND.COM");
    assert_eq!(source_path(b"c:\\dos\\"), b"c:\\dos\\COMMAND.COM");
    assert_eq!(source_path(b"c:\\"), b"c:\\COMMAND.COM");
    assert_eq!(source_path(b""), b"\\COMMAND.COM");
    assert_eq!(
        serial_of(0x0D2A, 0x1F05),
        0x1A54_3E0A,
        "the time's words doubled"
    );
}

/// A machine at its system diskette's prompt, `image` in B:.
fn with_target(image: Vec<u8>) -> Machine {
    let mut m = crate::dos::utility::tests::with_utilities(&[]);
    m.board_mut().insert_floppy(1, floppy(image)).unwrap();
    m
}

/// The target diskettes' serial number, not the system diskette's.
const SERIAL: u32 = 0x2468_ACE0;

/// A diskette not yet able to start: another system's boot record over a
/// volume holding `files`.
fn foreign(files: &[volume::File<'_>]) -> Vec<u8> {
    let volume = Diskette::K360.volume(SERIAL);
    let mut image = volume::volume(
        &volume,
        Some(*b"DATA DISK  "),
        files,
        tairix_fat::stamp::Stamp::NONE,
    )
    .unwrap();
    image[boot_at::OEM..boot_at::OEM + 8].copy_from_slice(b"MSDOS5.0");
    image[boot_at::CODE..510].fill(0xF4);
    image
}

fn run(m: &mut Machine, line: &str) -> String {
    crate::dos::utility::tests::run(m, line)
}

fn taken_out(m: &mut Machine) -> Vec<u8> {
    diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap())
}

#[test]
fn sys_makes_a_disk_that_starts_tairix_dos() {
    let mut m = with_target(foreign(&[volume::File {
        path: "KEPT.TXT",
        bytes: b"kept",
        attributes: 0,
    }]));
    assert!(
        run(&mut m, "sys b:").contains("\nSystem transferred\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "dir b:command.com").contains("COMMAND  COM"),
        "{}",
        screen(&m)
    );
    let image = taken_out(&mut m);
    let bpb = tairix_fat::boot::Bpb::parse(image[..512].try_into().unwrap()).unwrap();
    assert_eq!(bpb.serial, Some(SERIAL), "its serial number kept");
    assert_eq!(
        bpb.label,
        Some(*b"NO NAME    "),
        "the boot record's own label, not the root's"
    );
    assert_eq!(
        &image[..512],
        &crate::dos::boot::boot_record(&bpb).unwrap()[..],
        "TAIRiX DOS's boot record over its BPB"
    );
    let mut started =
        crate::dos::harness::bare_on(&crate::machine::Template::IbmPc.config(), image);
    crate::dos::harness::settle(&mut started);
    assert!(
        screen(&started).contains("Starting TAIRiX DOS...\n\nCurrent date is"),
        "the shell starts:\n{}",
        screen(&started)
    );
    typed(&mut started, "");
    typed(&mut started, "");
    typed(&mut started, "type kept.txt");
    assert!(screen(&started).contains("\nkept"), "{}", screen(&started));
}

#[test]
fn sys_says_what_it_cannot_do() {
    let mut m = with_target(foreign(&[]));
    assert!(
        run(&mut m, "sys a:").contains("\nCannot specify default drive\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "sys q:").contains("\nInvalid drive specification\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "sys a:\\nowhere b:")
            .contains("\nInsert system disk in drive A\nPress any key to continue . . .\n"),
        "a diskette without it is asked for:\n{}",
        screen(&m)
    );
    crate::dos::harness::control(&mut m, crate::dos::harness::C);
    crate::dos::harness::settle(&mut m);
    assert!(
        run(&mut m, "sys").contains("\nRequired parameter missing\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "sys q:\\ b:").contains("\nInvalid path or System files not found\n"),
        "a source no drive holds:\n{}",
        screen(&m)
    );
    let full = [volume::File {
        path: "FULL.DAT",
        bytes: &alloc::vec![0u8; 354 * 1024],
        attributes: 0,
    }];
    let mut m = with_target(foreign(&full));
    assert!(
        run(&mut m, "sys b:").contains("\nNo room for system on destination disk\n"),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_disk_dos_4_never_numbered_is_given_a_serial_and_its_label() {
    let mut image = foreign(&[]);
    image[boot_at::EXTENDED] = 0;
    let mut m = with_target(image);
    assert!(
        run(&mut m, "sys b:").contains("\nSystem transferred\n"),
        "{}",
        screen(&m)
    );
    let image = taken_out(&mut m);
    let bpb = tairix_fat::boot::Bpb::parse(image[..512].try_into().unwrap()).unwrap();
    assert!(bpb.serial.is_some_and(|serial| serial != 0));
    assert_eq!(bpb.label, Some(*b"DATA DISK  "), "the root's label");
}

#[test]
fn a_system_missing_from_a_diskette_is_asked_for() {
    let mut m = with_target(foreign(&[]));
    typed(&mut m, "b:");
    typed(&mut m, "cls");
    typed(&mut m, "a:sys a:");
    assert!(
        screen(&m).contains("\nInsert system disk in drive B\nPress any key to continue . . .\n"),
        "{}",
        screen(&m)
    );
    let system = diskette_bytes(&mut m.board_mut().eject_floppy(0).unwrap());
    let target = taken_out(&mut m);
    m.board_mut().insert_floppy(1, floppy(system)).unwrap();
    m.board_mut().insert_floppy(0, floppy(target)).unwrap();
    // These drives have no change line: DOS looks again only once two
    // seconds have passed since it last read them, as a hand would take.
    let now = m.now();
    crate::bios::run_until(&mut m, now + 3 * crate::dos::harness::SECOND);
    crate::dos::harness::type_ahead(&mut m, " ");
    crate::dos::harness::settle(&mut m);
    assert!(
        screen(&m).contains("\nSystem transferred\n"),
        "{}",
        screen(&m)
    );
}

#[test]
fn an_input_with_no_key_left_stops_the_asking_and_says_why() {
    let mut m = with_target(foreign(&[]));
    typed(&mut m, "cls");
    typed(&mut m, "sys a:\\nowhere b: <nul");
    let text = screen(&m);
    assert_eq!(
        text.matches("Insert system disk in drive A").count(),
        1,
        "asked once:\n{text}"
    );
    assert!(
        text.contains("\nInvalid path or System files not found\n"),
        "{text}"
    );
    typed(&mut m, "b:");
    typed(&mut m, "cls");
    typed(&mut m, "a:sys a: <nul");
    let text = screen(&m);
    assert_eq!(
        text.matches("Insert system disk in drive B").count(),
        1,
        "asked once:\n{text}"
    );
    assert!(text.contains("\nNo system on default drive\n"), "{text}");
}
