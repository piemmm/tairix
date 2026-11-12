//! `FORMAT`: its command line and DOS 4.0's checks of it against the drive,
//! then each format run at the shell's prompt: diskettes in drive B:, and a
//! fixed disk's partition.

use super::*;
use crate::chips::keyboard::Scan;
use crate::dos::device::generic::parameters::kind;
use crate::dos::harness::{
    control, diskette, diskette_bytes, fixed_boot, fixed_image, fixed_volume, floppy,
    protected_floppy, screen, typed, File,
};
use crate::dos::volume;
use crate::machine::Machine;
use alloc::string::String;
use alloc::vec;

fn asked(tail: &[u8]) -> Request {
    request(tail).unwrap()
}

#[test]
fn format_reads_its_drive_and_switches() {
    let plain = asked(b" b:");
    assert_eq!(
        (plain.drive, plain.switches, plain.size, plain.label),
        (1, 0, None, None)
    );
    let all = asked(b" a: /s /1 /4 /8 /b /t:40 /n:9 /v:work");
    assert_eq!(
        all.switches,
        switch::S
            | switch::ONE
            | switch::FOUR
            | switch::EIGHT
            | switch::B
            | switch::T
            | switch::N
            | switch::V
    );
    assert_eq!((all.tracks, all.sectors), (40, 9));
    assert_eq!(all.label, Some(*b"WORK       "));
    assert_eq!(asked(b" a: /F:1.44MB").size, Some(Diskette::K1440));
    assert_eq!(asked(b" a: /f:360k").size, Some(Diskette::K360));
    assert_eq!(asked(b" a: /v").label, None, "the label is asked for");
    assert_eq!(request(b""), Err(Refused::Parse(Refusal::Missing)));
    assert_eq!(
        request(b" a: b:"),
        Err(Refused::Parse(Refusal::TooMany(b"b:")))
    );
    assert_eq!(
        request(b" a:x"),
        Err(Refused::Parse(Refusal::Format(b"a:x")))
    );
    assert_eq!(
        request(b" a: /x"),
        Err(Refused::Parse(Refusal::Switch(b"/x")))
    );
    assert_eq!(
        request(b" a: /s:3"),
        Err(Refused::Parse(Refusal::Switch(b"/s:3")))
    );
    assert_eq!(
        request(b" a: /t:2000"),
        Err(Refused::Parse(Refusal::Range(b"/t:2000")))
    );
    assert_eq!(
        request(b" a: /n:x"),
        Err(Refused::Parse(Refusal::Format(b"/n:x")))
    );
    assert_eq!(
        request(b" a: /f:100"),
        Err(Refused::Parse(Refusal::Value(b"/f:100")))
    );
    assert_eq!(request(b" a: /s /s"), Err(Refused::Twice));
    assert_eq!(request(b" a: /v:a*b"), Err(Refused::BadLabel));
}

fn drive(kind: u8, diskette: Diskette) -> Drive {
    Drive {
        kind,
        cylinders: diskette.cylinders(),
        bpb: diskette.bpb(),
    }
}

fn planned(kind: u8, diskette: Diskette, tail: &[u8]) -> Result<Plan, Problem> {
    let tail = [b" a:", tail].concat();
    plan(&drive(kind, diskette), &asked(&tail))
}

#[test]
fn a_double_density_drive_takes_the_formats_dos_1_and_2_made() {
    let dd = |tail: &[u8]| planned(kind::DD525, Diskette::K360, tail).map(|plan| plan.bpb);
    assert_eq!(dd(b""), Ok(Diskette::K360.bpb()));
    assert_eq!(dd(b" /1"), Ok(Diskette::K180.bpb()));
    assert_eq!(dd(b" /8"), Ok(Diskette::K320.bpb()));
    assert_eq!(dd(b" /1 /8"), Ok(Diskette::K160.bpb()));
    assert_eq!(
        dd(b" /4"),
        Ok(Diskette::K360.bpb()),
        "/4 means nothing on a drive of its own density"
    );
    assert_eq!(dd(b" /f:180"), Ok(Diskette::K180.bpb()));
    assert_eq!(dd(b" /f:720"), Err(Problem::Incompatible));
    assert!(
        planned(kind::DD525, Diskette::K360, b" /8")
            .unwrap()
            .old_dir,
        "DOS 1's directory"
    );
}

#[test]
fn a_high_density_drive_formats_a_double_density_disk_at_its_own_cylinders() {
    let plan = planned(kind::HD525, Diskette::K1200, b" /4").unwrap();
    assert_eq!(
        (plan.bpb, plan.cylinders, plan.media_type),
        (Diskette::K360.bpb(), 40, 1)
    );
    assert_eq!(
        planned(kind::HD525, Diskette::K1200, b"").unwrap().bpb,
        Diskette::K1200.bpb()
    );
    assert_eq!(
        planned(kind::HD525, Diskette::K1200, b" /4 /1")
            .unwrap()
            .bpb,
        Diskette::K180.bpb()
    );
    assert_eq!(
        planned(kind::HD525, Diskette::K1200, b" /1"),
        Err(Problem::Incompatible),
        "/1 needs /4 there"
    );
    assert_eq!(
        planned(kind::HD525, Diskette::K1200, b" /f:360")
            .unwrap()
            .bpb,
        Diskette::K360.bpb()
    );
    let eight = planned(kind::HD525, Diskette::K1200, b" /t:40 /n:8").unwrap();
    assert_eq!(
        (eight.bpb, eight.old_dir),
        (Diskette::K320.bpb(), true),
        "a 320 KiB disk asked for by its tracks"
    );
}

#[test]
fn tracks_and_sectors_lay_a_format_of_their_own() {
    let hd = planned(kind::OTHER, Diskette::K1440, b" /t:80 /n:15").unwrap();
    let bpb = Bpb::from_dos(&hd.bpb).unwrap();
    assert_eq!(
        (
            bpb.total_sectors,
            bpb.sectors_per_track,
            bpb.media,
            bpb.fat_sectors
        ),
        (2400, 15, CUSTOM_MEDIA, 8)
    );
    assert_eq!(
        planned(kind::OTHER, Diskette::K1440, b" /f:720")
            .unwrap()
            .bpb,
        Diskette::K720.bpb()
    );
    let full = Bpb::from_dos(
        &planned(kind::OTHER, Diskette::K1440, b" /f:1440")
            .unwrap()
            .bpb,
    )
    .unwrap();
    assert_eq!(
        (full.total_sectors, full.fat_sectors, full.media),
        (2880, 9, 0xF0)
    );
    assert_eq!(
        planned(kind::DD35, Diskette::K720, b" /t:80 /n:9")
            .unwrap()
            .bpb,
        Diskette::K720.bpb()
    );
    let wide = Bpb::from_dos(
        &planned(kind::OTHER, Diskette::K1440, b" /t:80 /n:36")
            .unwrap()
            .bpb,
    )
    .unwrap();
    assert_eq!(wide.kind, Kind::Fat16, "more clusters than FAT12 counts");
    assert!(
        u64::from(wide.clusters()) + 2 <= u64::from(wide.fat_sectors) * 256,
        "a table that maps them"
    );
}

#[test]
fn switches_that_cannot_go_together_are_refused() {
    assert_eq!(
        planned(kind::DD525, Diskette::K360, b" /b /s"),
        Err(Problem::Incompatible)
    );
    assert_eq!(
        planned(kind::DD525, Diskette::K360, b" /v /8"),
        Err(Problem::Incompatible)
    );
    assert_eq!(
        planned(kind::DD525, Diskette::K360, b" /1 /t:40 /n:9"),
        Err(Problem::Incompatible)
    );
    assert_eq!(
        planned(kind::DD525, Diskette::K360, b" /n:9"),
        Err(Problem::Both)
    );
    assert_eq!(
        planned(kind::DD525, Diskette::K360, b" /f:360 /1"),
        Err(Problem::Incompatible)
    );
    assert_eq!(
        planned(kind::OTHER, Diskette::K1440, b" /4"),
        Err(Problem::ByDrive)
    );
    assert_eq!(
        planned(kind::DD35, Diskette::K720, b" /f:360"),
        Err(Problem::Incompatible)
    );
    let fixed = Drive {
        kind: kind::FIXED,
        cylinders: 64,
        bpb: crate::dos::formats::partition(17, 2159, 2, 17).unwrap(),
    };
    assert!(plan(&fixed, &asked(b" c: /s /v")).is_ok());
    assert_eq!(plan(&fixed, &asked(b" c: /1")), Err(Problem::FixedDisk));
    assert_eq!(plan(&fixed, &asked(b" c: /f:360")), Err(Problem::FixedDisk));
    assert_eq!(
        Problem::Unsupported.text(),
        b"\rParameters not supported by drive\r\n\rParameters not compatible\r\n"
    );
}

#[test]
fn a_partition_holding_no_fat_volume_is_given_dos_s() {
    let mut none = crate::dos::formats::partition(17, 2159, 2, 17).unwrap();
    none[5] = 0;
    let fixed = Drive {
        kind: kind::FIXED,
        cylinders: 64,
        bpb: none,
    };
    let bpb = Bpb::from_dos(&plan(&fixed, &asked(b" c:")).unwrap().bpb).unwrap();
    assert_eq!(
        (bpb.fats, bpb.sectors_per_cluster, bpb.root_entries),
        (2, 8, 512)
    );
}

#[test]
fn the_serial_is_the_date_and_time_added_word_by_word() {
    assert_eq!(serial((1988, 0x060E), (0x0D05, 0x1E3C)), 0x244A_14C9);
    assert_eq!(percent(1, 80), 1);
    assert_eq!(percent(80, 80), 100);
    assert_eq!(percent(7, 0), 100, "a disk of no tracks is done");
}

/// A blank 360 KiB diskette: every byte zero, as a new one formats.
fn blank() -> Vec<u8> {
    vec![0u8; 360 * 1024]
}

/// A machine at its system diskette's prompt, `image` in B:.
fn with_target(image: Vec<u8>) -> Machine {
    let mut m = crate::dos::utility::tests::with_utilities(&[]);
    m.board_mut().insert_floppy(1, floppy(image)).unwrap();
    m
}

fn run(m: &mut Machine, line: &str) -> String {
    crate::dos::utility::tests::run(m, line)
}

fn taken_out(m: &mut Machine) -> Vec<u8> {
    diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap())
}

/// Whether the last command left an error level of `code` or more.
fn at_least(m: &mut Machine, code: u8) -> bool {
    let line = alloc::format!("if errorlevel {code} echo AT LEAST {code}");
    run(m, &line).contains(&alloc::format!("\nAT LEAST {code}\n"))
}

#[test]
fn a_blank_diskette_is_formatted_labelled_and_reported() {
    let mut m = with_target(blank());
    let text = run(&mut m, "format b:");
    assert!(
        text.contains("Insert new diskette for drive B:\nand press ENTER when ready..."),
        "{text}"
    );
    typed(&mut m, "");
    let text = screen(&m);
    assert!(text.contains("\nFormat complete"), "{text}");
    assert!(
        text.contains("\nVolume label (11 characters, ENTER for none)?"),
        "{text}"
    );
    typed(&mut m, "work");
    let text = screen(&m);
    assert!(
        text.contains("\n    362496 bytes total disk space\n    362496 bytes available on disk\n"),
        "{text}"
    );
    assert!(text.contains("\n      1024 bytes in each allocation unit\n       354 allocation units available on disk\n"), "{text}");
    assert!(text.contains("Volume Serial Number is "), "{text}");
    assert!(text.contains("\nFormat another (Y/N)?"), "{text}");
    typed(&mut m, "n");
    assert!(!at_least(&mut m, 1), "{}", screen(&m));
    assert!(
        run(&mut m, "dir b:").contains("Volume in drive B is WORK"),
        "{}",
        screen(&m)
    );
    let image = taken_out(&mut m);
    let boot: &[u8; 512] = image[..512].try_into().unwrap();
    let bpb = Bpb::parse(boot).unwrap();
    assert_eq!(
        (bpb.total_sectors, bpb.kind, bpb.label),
        (720, Kind::Fat12, Some(*b"WORK       "))
    );
    assert!(bpb.serial.is_some_and(|serial| serial != 0));
    assert!(
        image[..512].windows(13).any(|w| w == b"Non-System di"),
        "a disk without the system says so"
    );
}

/// The diskette's root entry `name` in `image`.
fn root_entry(image: &[u8], name: &[u8; 11]) -> Option<tairix_fat::dir::ShortEntry> {
    let bpb = Bpb::parse(image[..512].try_into().ok()?)?;
    let root = usize::try_from(bpb.root_start()).ok()?;
    let entries = image.get(root..root + usize::from(bpb.root_entries) * 32)?;
    entries
        .as_chunks::<32>()
        .0
        .iter()
        .map(tairix_fat::dir::ShortEntry::decode)
        .find(|entry| &entry.name == name)
}

#[test]
fn a_system_disk_takes_command_com_and_the_label_given() {
    let mut m = with_target(blank());
    run(&mut m, "format b: /s /v:system");
    typed(&mut m, "");
    let text = screen(&m);
    assert!(text.contains("\nSystem transferred\n"), "{text}");
    assert!(
        !text.contains("Volume label (11"),
        "the label was given: {text}"
    );
    assert!(text.contains(" bytes used by system\n"), "{text}");
    typed(&mut m, "n");
    let image = taken_out(&mut m);
    let shell = crate::dos::shell::image().unwrap();
    let entry = root_entry(&image, b"COMMAND COM").expect("COMMAND.COM");
    assert_eq!(entry.size, u32::try_from(shell.len()).unwrap());
    let bpb = Bpb::parse(image[..512].try_into().unwrap()).unwrap();
    assert_eq!(bpb.label, Some(*b"SYSTEM     "));
    assert!(
        image[..512]
            .windows(crate::dos::rom::SIGNATURE.len())
            .any(|w| w == crate::dos::rom::SIGNATURE),
        "the record that starts TAIRiX DOS"
    );
}

#[test]
fn a_write_protected_diskette_is_not_formatted() {
    let mut m = crate::dos::utility::tests::with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, protected_floppy(blank()))
        .unwrap();
    run(&mut m, "format b:");
    typed(&mut m, "");
    let text = screen(&m);
    assert!(
        text.contains("\nWrite protect error\nFormat terminated"),
        "{text}"
    );
    assert!(text.contains("\nFormat another (Y/N)?"), "{text}");
    typed(&mut m, "n");
    assert!(
        at_least(&mut m, 4) && !at_least(&mut m, 5),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_break_at_the_prompt_ends_it_and_puts_the_drive_back() {
    let existing = crate::dos::harness::image(&[File {
        name: b"KEPT    TXT",
        bytes: b"kept",
    }]);
    let mut m = with_target(existing);
    run(&mut m, "format b: /1");
    control(&mut m, Scan::plain(0x2E));
    crate::dos::harness::settle(&mut m);
    assert!(
        at_least(&mut m, 3) && !at_least(&mut m, 4),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "dir b:");
    assert!(
        text.contains("KEPT     TXT") && text.contains("1 file(s)"),
        "the diskette is read with its own BPB again:\n{text}"
    );
}

#[test]
fn what_cannot_be_formatted_is_said() {
    let mut m = with_target(blank());
    assert!(
        run(&mut m, "format").contains("Required parameter missing"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "format b: /x").contains("Invalid switch - /X"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "format b: /f:100").contains("Parameter value not allowed - /F:100"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "format b: /v /v").contains("Same parameter entered twice"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "format b: /t:40");
    assert!(
        text.contains("Must enter both /T and /N parameters\nFormat terminated"),
        "{text}"
    );
    assert!(at_least(&mut m, 4), "{}", screen(&m));
    assert!(
        run(&mut m, "format q:").contains("Invalid drive specification"),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_dos_1_diskette_has_eight_sectors_a_track_and_no_label() {
    let mut m = with_target(vec![0u8; 320 * 1024]);
    run(&mut m, "format b: /8");
    typed(&mut m, "");
    let text = screen(&m);
    assert!(
        !text.contains("Volume label (11") && !text.contains("Serial Number"),
        "{text}"
    );
    assert!(
        text.contains("    322560 bytes total disk space"),
        "315 clusters of 1 KiB: {text}"
    );
    typed(&mut m, "n");
    let image = taken_out_320(&mut m);
    let bpb = Bpb::parse(image[..512].try_into().unwrap()).unwrap();
    assert_eq!(
        (bpb.total_sectors, bpb.sectors_per_track, bpb.media),
        (640, 8, 0xFF)
    );
    let root = usize::try_from(bpb.root_start()).unwrap();
    assert!(
        image[root..root + 112 * 32]
            .chunks(32)
            .all(|entry| entry[0] == 0xE5),
        "DOS 1's empty directory"
    );
}

/// The 320 KiB diskette in B:, as its drive reads it.
fn taken_out_320(m: &mut Machine) -> Vec<u8> {
    let mut floppy = m.board_mut().eject_floppy(1).unwrap();
    let mut image = vec![0u8; 640 * 512];
    for (lba, sector) in image.as_chunks_mut::<512>().0.iter_mut().enumerate() {
        let (cylinder, in_cylinder) = (lba / 16, lba % 16);
        floppy
            .read(
                u8::try_from(cylinder).unwrap(),
                u8::from(in_cylinder >= 8),
                in_cylinder % 8,
                sector,
            )
            .unwrap();
    }
    image
}

#[test]
fn a_double_density_diskette_is_formatted_in_a_high_density_drive() {
    let config = crate::machine::Template::TurboXt.config();
    let mut m =
        crate::dos::utility::tests::with_utilities_on(&config, &Diskette::K1440.volume(1), &[]);
    m.board_mut().insert_floppy(1, floppy(blank())).unwrap();
    run(&mut m, "format b: /4");
    typed(&mut m, "");
    typed(&mut m, "");
    let text = screen(&m);
    assert!(text.contains("    362496 bytes total disk space"), "{text}");
    typed(&mut m, "n");
    let image = taken_out(&mut m);
    let bpb = Bpb::parse(image[..512].try_into().unwrap()).unwrap();
    assert_eq!((bpb.total_sectors, bpb.media), (720, 0xFD));
}

#[test]
fn another_diskette_is_formatted_when_asked() {
    let mut m = with_target(blank());
    run(&mut m, "format b:");
    typed(&mut m, "");
    typed(&mut m, "first");
    m.board_mut().insert_floppy(1, floppy(blank())).unwrap();
    typed(&mut m, "y");
    assert!(
        screen(&m).contains("Insert new diskette for drive B:"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "");
    typed(&mut m, "second");
    typed(&mut m, "n");
    let image = taken_out(&mut m);
    assert_eq!(
        Bpb::parse(image[..512].try_into().unwrap()).unwrap().label,
        Some(*b"SECOND     ")
    );
}

/// A PC/XT at its prompt with the utilities on its diskette, its fixed
/// disk holding `image`, the tracks `bad` marked bad.
fn fixed_target(image: Vec<u8>, bad: &[u32]) -> Machine {
    crate::dos::utility::tests::with_utilities_in(fixed_boot(image, bad), &diskette(), &[])
}

#[test]
fn a_new_partition_is_formatted_once_the_warning_is_answered() {
    let mut m = fixed_target(fixed_image(None), &[]);
    let text = run(&mut m, "format c:");
    assert!(text.contains("WARNING, ALL DATA ON NON-REMOVABLE DISK\nDRIVE C: WILL BE LOST!\nProceed with Format (Y/N)?"), "{text}");
    typed(&mut m, "y");
    let text = screen(&m);
    assert!(text.contains("Format complete"), "{text}");
    typed(&mut m, "work");
    let text = screen(&m);
    assert!(
        text.contains(" bytes total disk space") && !text.contains("Format another"),
        "{text}"
    );
    assert!(!at_least(&mut m, 1), "{}", screen(&m));
    let text = run(&mut m, "dir c:");
    assert!(
        text.contains("Volume in drive C is WORK") && text.contains("File not found"),
        "{text}"
    );
}

#[test]
fn a_fixed_disk_kept_by_its_answer_is_left_as_it_was() {
    let volume = volume::volume(
        &fixed_volume(0x1111_2222),
        None,
        &[volume::File {
            path: "DATA.TXT",
            bytes: b"data",
            attributes: 0,
        }],
        tairix_fat::stamp::Stamp::NONE,
    )
    .unwrap();
    let mut m = fixed_target(fixed_image(Some(&volume)), &[]);
    run(&mut m, "format c:");
    typed(&mut m, "n");
    assert!(at_least(&mut m, 5), "{}", screen(&m));
    assert!(
        run(&mut m, "dir c:").contains("DATA     TXT"),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_labelled_fixed_disk_is_formatted_only_for_its_label() {
    let volume = volume::volume(
        &fixed_volume(0x1111_2222),
        Some(*b"MAIN DISK  "),
        &[],
        tairix_fat::stamp::Stamp::NONE,
    )
    .unwrap();
    let mut m = fixed_target(fixed_image(Some(&volume)), &[]);
    let text = run(&mut m, "format c:");
    assert!(
        text.contains("Enter current volume label for drive C:"),
        "{text}"
    );
    typed(&mut m, "other");
    let text = screen(&m);
    assert!(
        text.contains("Invalid Volume ID") && text.contains("Format terminated"),
        "{text}"
    );
    assert!(at_least(&mut m, 4), "{}", screen(&m));
    run(&mut m, "format c:");
    typed(&mut m, "main disk");
    assert!(
        screen(&m).contains("Proceed with Format (Y/N)?"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "n");
}

#[test]
fn a_fixed_disks_bad_track_costs_only_the_clusters_it_holds() {
    let mut m = fixed_target(fixed_image(None), &[40]);
    run(&mut m, "format c:");
    typed(&mut m, "y");
    let text = screen(&m);
    assert!(text.contains("Format complete"), "{text}");
    typed(&mut m, "");
    let text = screen(&m);
    assert!(text.contains(" bytes in bad sectors\n"), "{text}");
    assert!(!at_least(&mut m, 1), "{}", screen(&m));
}

#[test]
fn answers_from_a_file_are_taken_a_line_each_and_its_end_answers_no_more() {
    let answers: &[u8] = b"\r\nwork\r\n\r\nn\r\n";
    let mut m = crate::dos::utility::tests::with_utilities(&[("ANSWERS.TXT", answers)]);
    m.board_mut().insert_floppy(1, floppy(blank())).unwrap();
    let text = run(&mut m, "format b: <answers.txt");
    assert_eq!(
        text.matches("Format another (Y/N)?").count(),
        2,
        "a blank line is a line, and no answer:\n{text}"
    );
    assert!(!at_least(&mut m, 1), "{}", screen(&m));
    let image = taken_out(&mut m);
    assert_eq!(
        Bpb::parse(image[..512].try_into().unwrap()).unwrap().label,
        Some(*b"WORK       ")
    );
    m.board_mut().insert_floppy(1, floppy(blank())).unwrap();
    let text = run(&mut m, "format b: <nul");
    assert_eq!(text.matches("Format another (Y/N)?").count(), 1, "{text}");
    assert!(
        text.contains("\nFormat complete") && !at_least(&mut m, 1),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_system_that_cannot_be_asked_for_is_said_to_be_missing() {
    let mut m = with_target(blank());
    typed(&mut m, "set comspec=a:\\nowhere\\command.com");
    let text = run(&mut m, "format b: /s <nul");
    assert_eq!(
        text.matches("Insert DOS disk in drive A:").count(),
        1,
        "asked once:\n{text}"
    );
    assert!(text.contains("Cannot find System Files"), "{text}");
}
