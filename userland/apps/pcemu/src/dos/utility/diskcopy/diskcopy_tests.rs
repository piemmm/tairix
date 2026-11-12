//! `DISKCOPY`: its command line, then copies run at the shell's prompt —
//! between two drives, through one drive the disks take turns in, of a
//! disk without a BPB, past a bad track and a drive put right — and the
//! drives it refuses.

use super::*;
use crate::bios::run_until;
use crate::board::BoardConfig;
use crate::dos::formats::Diskette;
use crate::dos::harness::{
    control, diskette, diskette_bytes, floppy, image_of, protected_floppy, screen, type_ahead,
    unreliable_floppy, File, C, SECOND,
};
use crate::dos::utility::tests::{run, with_utilities, with_utilities_on};
use crate::machine::{Machine, Template};
use alloc::string::String;
use alloc::vec;
use tairix_fat::boot::at::SERIAL;

#[test]
fn diskcopy_reads_two_drives_and_one_switch() {
    assert_eq!(request(b""), Ok(Request::default()));
    assert_eq!(
        request(b" a: b:"),
        Ok(Request {
            source: Some(0),
            target: Some(1),
            one_side: false
        })
    );
    assert_eq!(
        request(b" B:/1"),
        Ok(Request {
            source: Some(1),
            target: None,
            one_side: true
        })
    );
    assert_eq!(request(b" a: b: c:"), Err(Refusal::TooMany(b"c:")));
    assert_eq!(request(b" a:x"), Err(Refusal::Format(b"a:x")));
    assert_eq!(request(b" a: /1 /1"), Err(Refusal::Switch(b"/1")));
    assert_eq!(request(b" a: /v"), Err(Refusal::Switch(b"/v")));
}

#[test]
fn a_copys_serial_is_the_date_and_time_added_word_by_word_the_other_way_round() {
    assert_eq!(copy::serial((1988, 0x060E), (0x0D05, 0x1E3C)), 0x14C9_244A);
}

/// A 360 KiB volume holding a file, its serial `serial`.
fn source(serial: u32) -> Vec<u8> {
    image_of(
        &Diskette::K360.volume(serial),
        &[File {
            name: b"WORDS   TXT",
            bytes: b"one two three\r\n",
        }],
    )
}

/// Presses `keys` and runs while the copy goes on, until the machine waits
/// for a key again; the screen then.
fn pressed(m: &mut Machine, keys: &str) -> String {
    type_ahead(m, keys);
    let deadline = m.now() + 10 * 60 * SECOND;
    loop {
        let now = m.now();
        run_until(m, now + SECOND / 20);
        if crate::dos::waits_for_key(m) {
            return String::from(screen(m).trim_end());
        }
        assert!(
            m.now() < deadline,
            "the copy never asked again:\n{}",
            screen(m)
        );
    }
}

/// Whether the last command left an error level of `code` or more.
fn at_least(m: &mut Machine, code: u8) -> bool {
    let line = alloc::format!("if errorlevel {code} echo AT LEAST {code}");
    run(m, &line).contains(&alloc::format!("\nAT LEAST {code}\n"))
}

/// The error level the last command left.
fn code(m: &mut Machine) -> u8 {
    (1..=4).rev().find(|&n| at_least(m, n)).unwrap_or(0)
}

/// Whether `copy` is `original` but for the serial number in its boot
/// record, and that one new.
fn copied_with_new_serial(copy: &[u8], original: &[u8]) -> u32 {
    assert_eq!(copy.len(), original.len());
    assert_eq!(copy[..SERIAL], original[..SERIAL]);
    assert_eq!(copy[SERIAL + 4..], original[SERIAL + 4..]);
    let given = u32::from_le_bytes(copy[SERIAL..SERIAL + 4].try_into().unwrap());
    assert_ne!(
        given,
        u32::from_le_bytes(original[SERIAL..SERIAL + 4].try_into().unwrap()),
        "the copy gets a serial of its own"
    );
    given
}

#[test]
fn a_diskette_is_copied_onto_a_blank_one_formatted_as_it_is_written() {
    let original = source(0x1234_5678);
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    let text = run(&mut m, "diskcopy a: b:");
    assert!(text.contains("\nInsert SOURCE diskette in drive A:\n\nInsert TARGET diskette in drive B:\n\nPress any key to continue . . .\n"), "{text}");
    m.board_mut()
        .insert_floppy(0, floppy(original.clone()))
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(
        text.contains("\nCopying 40 tracks\n9 Sectors/Track, 2 Side(s)\n"),
        "{text}"
    );
    assert!(text.contains("\nFormatting while copying\n"), "{text}");
    assert!(text.ends_with("Copy another diskette (Y/N)?"), "{text}");
    pressed(&mut m, "n");
    let copy = diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap());
    let given = copied_with_new_serial(&copy, &original);
    let told = alloc::format!(
        "\nVolume Serial Number is {:04X}-{:04X}\n",
        given >> 16,
        given & 0xFFFF
    );
    assert!(screen(&m).contains(&told), "{}", screen(&m));
    assert_eq!(code(&mut m), exit::COPIED, "{}", screen(&m));
}

#[test]
fn a_target_of_the_sources_format_is_written_without_a_format() {
    let original = source(0x0101_0101);
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, floppy(source(0x0202_0202)))
        .unwrap();
    run(&mut m, "diskcopy a: b:");
    m.board_mut()
        .insert_floppy(0, floppy(original.clone()))
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(!text.contains("Formatting while copying"), "{text}");
    pressed(&mut m, "n");
    copied_with_new_serial(
        &diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap()),
        &original,
    );
}

/// The XT's one drive, as A: and B:, at its system diskette's prompt, with
/// `memory_kib` of memory.
fn one_drive(memory_kib: u16) -> Machine {
    let config = BoardConfig {
        memory_kib,
        ..Template::IbmXt.config()
    };
    with_utilities_on(&config, &diskette(), &[])
}

#[test]
fn two_letters_of_one_drive_copy_through_it_a_pass_at_a_time() {
    // So little memory holds the disk in several passes, each asking for
    // the disk it needs.
    let original = source(0x5555_AAAA);
    let mut m = one_drive(256);
    let mut text = String::from(run(&mut m, "diskcopy a: b:").trim_end());
    let (mut source, mut target) = (
        Some(floppy(original.clone())),
        Some(floppy(vec![0u8; 360 * 1024])),
    );
    let mut passes = 0;
    while !text.ends_with("Copy another diskette (Y/N)?") {
        let held = m.board_mut().eject_floppy(0);
        if text.ends_with("Insert SOURCE diskette in drive A:\n\nPress any key to continue . . .") {
            target = target.or(held);
            m.board_mut()
                .insert_floppy(0, source.take().unwrap())
                .unwrap();
            passes += 1;
        } else {
            assert!(
                text.ends_with(
                    "Insert TARGET diskette in drive A:\n\nPress any key to continue . . ."
                ),
                "{text}"
            );
            source = source.or(held);
            m.board_mut()
                .insert_floppy(0, target.take().unwrap())
                .unwrap();
        }
        text = pressed(&mut m, "\r");
    }
    assert!(passes > 1, "{passes} passes");
    pressed(&mut m, "n");
    copied_with_new_serial(
        &diskette_bytes(&mut m.board_mut().eject_floppy(0).unwrap()),
        &original,
    );
}

#[test]
fn a_disk_without_a_bpb_is_known_by_the_sectors_it_holds() {
    // Its fifteenth sector is not there: the ninth's track is all it has.
    let original: alloc::vec::Vec<u8> = (0..720u32)
        .flat_map(|lba| [u8::try_from(lba % 200).unwrap(); 512])
        .collect();
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    run(&mut m, "diskcopy a: b:");
    m.board_mut()
        .insert_floppy(0, floppy(original.clone()))
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(
        text.contains("\nCopying 40 tracks\n9 Sectors/Track, 2 Side(s)\n"),
        "{text}"
    );
    pressed(&mut m, "n");
    assert_eq!(
        diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap()),
        original,
        "copied whole, no serial to give"
    );
}

#[test]
fn a_track_that_cannot_be_read_is_said_and_the_copy_goes_on() {
    let original = source(0x0BAD_7EAC);
    let bad = u64::from(5u32 * 18);
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    run(&mut m, "diskcopy a: b:");
    m.board_mut()
        .insert_floppy(
            0,
            unreliable_floppy(original, move |sector, _| sector == bad),
        )
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(
        text.contains("\nUnrecoverable read error on drive A\nSide 0, track 5\n"),
        "{text}"
    );
    assert!(
        text.contains("\nTarget diskette may be unusable\n"),
        "{text}"
    );
    pressed(&mut m, "n");
    assert_eq!(code(&mut m), exit::TRACKS_LOST, "{}", screen(&m));
}

#[test]
fn a_drive_put_right_is_asked_for_and_the_copy_goes_on() {
    let original = source(0x0BEE_0BEE);
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, protected_floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    run(&mut m, "diskcopy a: b:");
    m.board_mut()
        .insert_floppy(0, floppy(original.clone()))
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(
        text.ends_with("\nWrite protect error\n\nPress any key to continue . . ."),
        "{text}"
    );
    m.board_mut().eject_floppy(1);
    let text = pressed(&mut m, "\r");
    assert!(text.ends_with("\nNot ready - B\nMake sure a diskette is inserted into\nthe drive and the door is closed\n\nPress any key to continue . . ."), "{text}");
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(text.ends_with("Copy another diskette (Y/N)?"), "{text}");
    pressed(&mut m, "n");
    copied_with_new_serial(
        &diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap()),
        &original,
    );
}

#[test]
fn the_first_side_alone_is_copied_with_one() {
    let original = source(0x1111_2222);
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    run(&mut m, "diskcopy a: b: /1");
    m.board_mut()
        .insert_floppy(0, floppy(original.clone()))
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(
        text.contains("\nCopying 40 tracks\n9 Sectors/Track, 1 Side(s)\n"),
        "{text}"
    );
    pressed(&mut m, "n");
    let copy = diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap());
    for (n, (track, was)) in copy
        .chunks(9 * 512)
        .zip(original.chunks(9 * 512))
        .enumerate()
        .skip(1)
    {
        if n % 2 == 0 {
            assert_eq!(track, was, "cylinder {}'s first side", n / 2);
        } else {
            assert!(
                track.iter().all(|&b| b == 0),
                "cylinder {}'s second side is left",
                n / 2
            );
        }
    }
}

#[test]
fn drives_it_cannot_copy_between_are_refused() {
    let mut m = with_utilities_on(&Template::TurboXt.config(), &Diskette::K1440.volume(9), &[]);
    let text = run(&mut m, "diskcopy a: b:");
    assert!(
        text.contains("\nDrive types or diskette types\nnot compatible\n"),
        "{text}"
    );
    assert_eq!(code(&mut m), exit::REFUSED, "{}", screen(&m));
    let text = run(&mut m, "diskcopy a: d:");
    assert!(
        text.contains(
            "\nInvalid drive specification\nSpecified drive does not exist\nor is non-removable\n"
        ),
        "{text}"
    );
    assert_eq!(code(&mut m), exit::REFUSED, "{}", screen(&m));
    let text = run(&mut m, "diskcopy a:x");
    assert!(text.contains("Parameter format not correct - A:X\nDo not specify filename(s)\nCommand Format: DISKCOPY d: d: [/1]\n"), "{text}");
    assert_eq!(code(&mut m), exit::REFUSED, "{}", screen(&m));
}

#[test]
fn a_break_at_a_prompt_ends_it_with_its_code() {
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    run(&mut m, "diskcopy a: b:");
    control(&mut m, C);
    crate::dos::harness::settle(&mut m);
    assert_eq!(code(&mut m), exit::BREAK, "{}", screen(&m));
}

#[test]
fn a_source_it_cannot_read_ends_the_copy() {
    // Neither its boot record nor its eighth sector reads: no format to
    // find.
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    run(&mut m, "diskcopy a: b:");
    m.board_mut()
        .insert_floppy(
            0,
            unreliable_floppy(source(1), |sector, _| matches!(sector, 0 | 7)),
        )
        .unwrap();
    let text = pressed(&mut m, "\r");
    assert!(
        text.contains("\nSOURCE diskette bad or incompatible\nCopy process ended\n"),
        "{text}"
    );
    pressed(&mut m, "n");
    m.board_mut().insert_floppy(0, floppy(source(3))).unwrap();
    assert_eq!(code(&mut m), exit::FATAL, "{}", screen(&m));
}

#[test]
fn a_target_that_cannot_be_formatted_ends_the_copy() {
    // Its third cylinder's first track fails the verify after its format.
    let bad = u64::from(2u32 * 18);
    let mut m = with_utilities(&[]);
    m.board_mut()
        .insert_floppy(
            1,
            unreliable_floppy(vec![0u8; 360 * 1024], move |sector, _| sector == bad),
        )
        .unwrap();
    run(&mut m, "diskcopy a: b:");
    m.board_mut().insert_floppy(0, floppy(source(2))).unwrap();
    let text = pressed(&mut m, "\r");
    assert!(text.contains("\nFormatting while copying\n\nTARGET diskette bad or incompatible\nCopy process ended\n"), "{text}");
    pressed(&mut m, "n");
    assert_eq!(code(&mut m), exit::FATAL, "{}", screen(&m));
}

#[test]
fn an_input_that_has_ended_answers_no_more_prompts() {
    // Each press read a Ctrl+Z from the file's end, and the drive was asked
    // for again for ever.
    let mut m = with_utilities(&[("EMPTY.TXT", b""), ("KEY.TXT", b"x")]);
    let text = run(&mut m, "diskcopy a: b: <empty.txt");
    assert!(text.contains("Press any key to continue . . ."), "{text}");
    assert_eq!(code(&mut m), exit::BREAK, "{}", screen(&m));
    m.board_mut()
        .insert_floppy(1, floppy(vec![0u8; 360 * 1024]))
        .unwrap();
    type_ahead(&mut m, "diskcopy a: b: <key.txt\r");
    let text = pressed(&mut m, "");
    assert!(text.contains("Copy another diskette (Y/N)?"), "{text}");
    assert_eq!(code(&mut m), exit::COPIED, "{}", screen(&m));
}
