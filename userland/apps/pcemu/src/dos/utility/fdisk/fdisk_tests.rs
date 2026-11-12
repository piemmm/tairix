//! `FDISK`: its command line and the room its screens take, then each run
//! at the shell's prompt on a PC/XT whose fixed disks are read back as it
//! writes them.

use super::table::{Disk, Shape};
use super::*;
use crate::dos::harness::{
    diskette, fixed_disks, screen, settle, type_ahead, typed, Fixed, FIXED_DISK,
};
use crate::dos::utility::tests::{run, with_utilities, with_utilities_in};
use crate::dos::volume;
use crate::machine::Machine;
use crate::storage::{BlockDevice, MediaError};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use core::cell::RefCell;
use tairix_fat::boot::Bpb;
use tairix_partition::mbr::{self, Entry, MBR_SECTOR_LEN};

/// A disk the test reads back as the machine writes it.
#[derive(Clone)]
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Shared {
    fn sector(&self, lba: u32) -> [u8; MBR_SECTOR_LEN] {
        let at = usize::try_from(lba).unwrap() * MBR_SECTOR_LEN;
        self.0.borrow()[at..at + MBR_SECTOR_LEN].try_into().unwrap()
    }

    /// The entries its master boot record holds.
    fn entries(&self) -> Vec<Entry> {
        mbr::entries(&self.sector(0)).unwrap().as_slice().to_vec()
    }

    /// The logical drives its extended partition from sector `base` chains,
    /// each its first sector and its sectors.
    fn chain(&self, base: u32) -> Vec<(u32, u32)> {
        let mut drives = Vec::new();
        let mut at = base;
        loop {
            let record = mbr::ebr(&self.sector(at)).unwrap();
            if let Some(drive) = record.logical {
                drives.push((at + drive.start_lba, drive.sectors));
            }
            match record.next {
                Some(next) => at = base + next,
                None => return drives,
            }
        }
    }
}

impl BlockDevice for Shared {
    fn sectors(&self) -> u64 {
        u64::try_from(self.0.borrow().len() / MBR_SECTOR_LEN).unwrap()
    }

    fn read_only(&self) -> bool {
        false
    }

    fn read(&mut self, sector: u64, out: &mut [u8; MBR_SECTOR_LEN]) -> Result<(), MediaError> {
        *out = self.sector(u32::try_from(sector).unwrap());
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8; MBR_SECTOR_LEN]) -> Result<(), MediaError> {
        let at = usize::try_from(sector).unwrap() * MBR_SECTOR_LEN;
        self.0.borrow_mut()[at..at + MBR_SECTOR_LEN].copy_from_slice(data);
        Ok(())
    }
}

/// The test fixed disk as `FDISK` sees it: 63 cylinders of two 17-sector
/// tracks, a cylinder 34 sectors, the drive's last cylinder the BIOS keeps
/// back.
const SHAPE: Shape = Shape {
    cylinders: 63,
    heads: 2,
    sectors: 17,
};
const CYLINDER: u32 = 34;

/// The test fixed disk, nothing written on it.
fn blank() -> Vec<u8> {
    vec![0; usize::from(FIXED_DISK.cylinders) * usize::try_from(CYLINDER).unwrap() * MBR_SECTOR_LEN]
}

/// The test fixed disk with the tables `make` makes written, as `FDISK`
/// writes them.
fn partitioned(make: impl FnOnce(&mut Disk)) -> Vec<u8> {
    let base = crate::dos::boot::master_boot_record().unwrap();
    let mut tables = Disk::read(SHAPE, &[0; MBR_SECTOR_LEN], &base);
    make(&mut tables);
    let mut image = blank();
    for (lba, sector) in tables.writes() {
        let at = usize::try_from(lba).unwrap() * MBR_SECTOR_LEN;
        image[at..at + MBR_SECTOR_LEN].copy_from_slice(&sector);
    }
    image
}

/// A volume labelled `label` laid in `image`'s partition of `sectors`
/// sectors from `start`.
fn formatted(image: &mut [u8], start: u32, sectors: u32, label: &[u8; 11]) {
    let bpb =
        Bpb::from_dos(&crate::dos::formats::partition(start, sectors, 2, 17).unwrap()).unwrap();
    let bytes = volume::volume(&bpb, Some(*label), &[], tairix_fat::stamp::Stamp::NONE).unwrap();
    let at = usize::try_from(start).unwrap() * MBR_SECTOR_LEN;
    image[at..at + bytes.len()].copy_from_slice(&bytes);
}

/// A PC/XT at its prompt with the utilities on its diskette, its fixed
/// disks holding `images`, the first's tracks `bad` marked bad; and the
/// disks, read back as written.
fn machine(images: Vec<Vec<u8>>, bad: &[u32]) -> (Machine, Vec<Shared>) {
    let disks: Vec<Shared> = images
        .into_iter()
        .map(|image| Shared(Rc::new(RefCell::new(image))))
        .collect();
    let fixed = disks
        .iter()
        .enumerate()
        .map(|(n, disk)| Fixed {
            kind: FIXED_DISK,
            device: Box::new(disk.clone()),
            bad: if n == 0 { bad.to_vec() } else { Vec::new() },
        })
        .collect();
    (
        with_utilities_in(fixed_disks(fixed), &diskette(), &[]),
        disks,
    )
}

/// Types `keys`, then lets the machine run until it waits for a key again.
fn keyed(m: &mut Machine, keys: &str) {
    type_ahead(m, keys);
    settle(m);
}

const ESC: &str = "\x1b";

/// Whether the last command left an error level of `code` or more.
fn at_least(m: &mut Machine, code: u8) -> bool {
    let line = alloc::format!("if errorlevel {code} echo AT LEAST {code}");
    run(m, &line).contains(&alloc::format!("\nAT LEAST {code}\n"))
}

#[test]
fn fdisk_reads_a_disk_sizes_and_quiet() {
    assert_eq!(request(b""), Ok(Request::default()));
    let all = Request {
        disk: Some(0),
        primary: Some(30),
        extended: Some(10),
        logical: Some(5),
        quiet: true,
    };
    assert_eq!(request(b" 1 /pri:30 /EXT:10 /log:5 /q"), Ok(all));
    assert_eq!(
        request(b" 2"),
        Ok(Request {
            disk: Some(1),
            ..Request::default()
        })
    );
    assert_eq!(
        request(b" /pri:1 /pri:2").unwrap().primary,
        Some(2),
        "named again, the later value"
    );
    assert_eq!(request(b" 3"), Err(Refusal::Range(b"3")));
    assert_eq!(request(b" x"), Err(Refusal::Format(b"x")));
    assert_eq!(request(b" 1 2"), Err(Refusal::TooMany(b"2")));
    assert_eq!(request(b" /pri"), Err(Refusal::Switch(b"/pri")));
    assert_eq!(request(b" /pri:0"), Err(Refusal::Range(b"/pri:0")));
    assert_eq!(request(b" /log:4001"), Err(Refusal::Range(b"/log:4001")));
    assert_eq!(request(b" /q:1"), Err(Refusal::Switch(b"/q:1")));
    assert_eq!(request(b" /x"), Err(Refusal::Switch(b"/x")));
}

#[test]
fn the_fullest_screens_fit_the_room_drawn_from() {
    let shape = Shape {
        cylinders: 1024,
        heads: 16,
        sectors: 63,
    };
    let base = crate::dos::boot::master_boot_record().unwrap();
    let mut disk = Disk::read(shape, &[0; MBR_SECTOR_LEN], &base);
    disk.make_primary(table::Extent { start: 0, end: 63 }, true)
        .unwrap();
    disk.make_extended(table::Extent {
        start: 64,
        end: 1023,
    })
    .unwrap();
    for n in 0..23 {
        disk.make_logical(table::Extent {
            start: 64 + n * 40,
            end: 103 + n * 40,
        })
        .unwrap();
    }
    for drive in &mut disk.logical {
        drive.label = *b"WWWWWWWWWWW";
        drive.system = *b"FAT16   ";
    }
    let disks = [disk.clone()];
    let mut screen = Screen::new();
    screen.clear(0, LAST_ROW);
    for message in [
        text::LOGICAL_TITLE,
        text::DELETE_LOGICAL_TITLE,
        text::RETURN,
        text::DELETE_LOGICAL_WARNING,
        text::ENTER_LABEL,
    ] {
        screen.draw(message, &[]);
    }
    views::drives(&mut screen, &disks, 0);
    views::extended_space(&mut screen, &disk);
    views::maximum(
        &mut screen,
        text::MAXIMUM_LOGICAL,
        shape,
        table::Extent {
            start: 64,
            end: 1023,
        },
        960,
    );
    input::size_question(&mut screen, input::prompt::LOGICAL, 100, true);
    screen.draw(text::LOGICAL_CREATED, &[]);
    for n in 0..23 {
        views::deleted(&mut screen, n);
    }
    let len = screen.encoded(usize::MAX).len();
    assert!(len <= usize::from(at::SCREEN_LEN), "{len} bytes of calls");
    let mut menu = Screen::new();
    menu.clear(0, LAST_ROW);
    views::title(&mut menu);
    views::table(&mut menu, &disks, 0);
    views::main_menu(&mut menu, &disks, 0);
    menu.draw(text::UNREADABLE, b"2");
    assert!(menu.encoded(usize::MAX).len() <= usize::from(at::SCREEN_LEN));
}

#[test]
fn a_whole_blank_disk_made_one_active_partition_restarts_the_machine() {
    let (mut m, disks) = machine(vec![blank()], &[]);
    let text = run(&mut m, "fdisk");
    assert!(
        text.contains("TAIRiX DOS Version 5.00\n") && text.contains("Fixed Disk Setup Program\n"),
        "{text}"
    );
    assert!(
        text.contains("FDISK Options") && text.contains("Current fixed disk drive: 1"),
        "{text}"
    );
    assert!(
        text.contains("Enter choice: [1]") && !text.contains("5. Select next"),
        "one disk:\n{text}"
    );
    typed(&mut m, "1");
    let text = screen(&m);
    assert!(
        text.contains("Create DOS Partition or Logical DOS Drive")
            && !text.contains("1. Create DOS Partition"),
        "{text}"
    );
    typed(&mut m, "1");
    assert!(
        screen(&m).contains("and make the partition active (Y/N).....................? [Y]"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "");
    let text = screen(&m);
    assert!(
        text.contains("System will now restart")
            && text.contains("Insert DOS diskette in drive A:"),
        "{text}"
    );
    let whole = Entry {
        type_byte: table::NEW_TYPE,
        active: true,
        start_lba: 17,
        sectors: 63 * CYLINDER - 17,
    };
    assert_eq!(
        disks[0].entries(),
        [whole],
        "from cylinder 0's second track to the disk's end"
    );
    assert_eq!(
        disks[0].sector(0)[..0x1BE],
        crate::dos::boot::master_boot_record().unwrap()[..0x1BE],
        "the master boot record's code"
    );
    keyed(&mut m, " ");
    assert!(screen(&m).contains("A:\\>"), "restarted:\n{}", screen(&m));
}

#[test]
fn partitions_and_logical_drives_take_the_sizes_typed() {
    let (mut m, disks) = machine(vec![blank()], &[]);
    run(&mut m, "fdisk");
    typed(&mut m, "1");
    typed(&mut m, "1");
    typed(&mut m, "n");
    let text = screen(&m);
    assert!(text.contains("Total disk space is    1 Mbytes"), "{text}");
    assert!(
        text.contains("Maximum space available for partition is    1 Mbytes (100%)"),
        "{text}"
    );
    assert!(
        text.contains("create a Primary DOS Partition.................................: [   1]"),
        "{text}"
    );
    typed(&mut m, "50%");
    let text = screen(&m);
    assert!(
        text.contains("Primary DOS Partition created") && text.contains("C: 1             PRI DOS"),
        "{text}"
    );
    keyed(&mut m, ESC);
    typed(&mut m, "1");
    typed(&mut m, "2");
    assert!(
        screen(&m)
            .contains("create an Extended DOS Partition..............................: [   1]"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "");
    assert!(
        screen(&m).contains("Extended DOS Partition created"),
        "{}",
        screen(&m)
    );
    keyed(&mut m, ESC);
    let text = screen(&m);
    assert!(
        text.contains("Create Logical DOS Drive(s) in the Extended DOS Partition")
            && text.contains("No logical drives defined"),
        "{text}"
    );
    assert!(
        text.contains("Maximum space available for logical drive is    1 Mbytes (100%)"),
        "{text}"
    );
    typed(&mut m, "50%");
    let text = screen(&m);
    assert!(
        text.contains("Logical DOS Drive created") && text.contains("D:"),
        "{text}"
    );
    assert!(
        text.contains("(%)...[ 48%]"),
        "what is left, under half a megabyte, offered as a share:\n{text}"
    );
    typed(&mut m, "");
    let text = screen(&m);
    assert!(
        text.contains("All available space in the Extended DOS Partition") && text.contains("E:"),
        "{text}"
    );
    keyed(&mut m, ESC);
    keyed(&mut m, ESC);
    assert!(
        screen(&m).contains("System will now restart"),
        "{}",
        screen(&m)
    );
    let primary = Entry {
        type_byte: table::NEW_TYPE,
        active: false,
        start_lba: 17,
        sectors: 32 * CYLINDER - 17,
    };
    let extended = Entry {
        type_byte: mbr::PART_TYPE_EXTENDED_CHS,
        active: false,
        start_lba: 32 * CYLINDER,
        sectors: 31 * CYLINDER,
    };
    assert_eq!(disks[0].entries(), [primary, extended]);
    let drive =
        |cylinder: u32, cylinders: u32| (cylinder * CYLINDER + 17, cylinders * CYLINDER - 17);
    assert_eq!(
        disks[0].chain(32 * CYLINDER),
        [drive(32, 16), drive(48, 15)],
        "each after its record's track"
    );
    keyed(&mut m, " ");
}

#[test]
fn the_active_partition_is_chosen_from_the_table() {
    let image = partitioned(|disk| {
        disk.make_primary(table::Extent { start: 0, end: 31 }, false)
            .unwrap();
        disk.make_extended(table::Extent { start: 32, end: 47 })
            .unwrap();
    });
    let (mut m, disks) = machine(vec![image], &[]);
    let text = run(&mut m, "fdisk");
    assert!(
        text.contains("WARNING! No partitions are set active - disk 1 is not startable unless"),
        "{text}"
    );
    typed(&mut m, "2");
    let text = screen(&m);
    assert!(
        text.contains("C: 1             PRI DOS") && text.contains("2             EXT DOS"),
        "{text}"
    );
    typed(&mut m, "2");
    assert!(
        screen(&m)
            .contains("Partition selected (2) is not startable, active partition not changed."),
        "{}",
        screen(&m)
    );
    typed(&mut m, "1");
    let text = screen(&m);
    assert!(
        text.contains("Partition 1 made active") && text.contains("C: 1        A    PRI DOS"),
        "{text}"
    );
    keyed(&mut m, ESC);
    assert!(!screen(&m).contains("WARNING!"), "{}", screen(&m));
    keyed(&mut m, ESC);
    assert!(
        screen(&m).contains("A:\\>"),
        "no restart for the active partition alone:\n{}",
        screen(&m)
    );
    assert!(disks[0].entries()[0].active && !disks[0].entries()[1].active);
}

#[test]
fn a_logical_drive_is_deleted_for_its_label_and_the_rest_lettered_again() {
    let mut image = partitioned(|disk| {
        disk.make_primary(table::Extent { start: 0, end: 15 }, true)
            .unwrap();
        disk.make_extended(table::Extent { start: 16, end: 63 })
            .unwrap();
        disk.make_logical(table::Extent { start: 16, end: 39 })
            .unwrap();
        disk.make_logical(table::Extent { start: 40, end: 63 })
            .unwrap();
    });
    formatted(
        &mut image,
        16 * CYLINDER + 17,
        24 * CYLINDER - 17,
        b"WORK       ",
    );
    formatted(
        &mut image,
        40 * CYLINDER + 17,
        24 * CYLINDER - 17,
        b"PLAY       ",
    );
    let (mut m, disks) = machine(vec![image], &[]);
    run(&mut m, "fdisk");
    typed(&mut m, "3");
    typed(&mut m, "3");
    let text = screen(&m);
    assert!(
        text.contains("D:  WORK") && text.contains("E:  PLAY"),
        "{text}"
    );
    assert!(text.contains("FAT12"), "{text}");
    typed(&mut m, "d");
    typed(&mut m, "play");
    assert!(
        screen(&m).contains("Volume label does not match."),
        "{}",
        screen(&m)
    );
    typed(&mut m, "d");
    typed(&mut m, "work");
    typed(&mut m, "y");
    let text = screen(&m);
    assert!(
        text.contains("D:  Drive deleted") && text.contains("E:  PLAY"),
        "{text}"
    );
    typed(&mut m, "d");
    assert!(
        screen(&m).contains("Drive D: already deleted."),
        "{}",
        screen(&m)
    );
    keyed(&mut m, ESC);
    let text = screen(&m);
    assert!(
        text.contains("Drive letters have been changed or deleted") && text.contains("D:  PLAY"),
        "{text}"
    );
    keyed(&mut m, ESC);
    keyed(&mut m, ESC);
    assert!(
        screen(&m).contains("System will now restart"),
        "{}",
        screen(&m)
    );
    assert_eq!(
        disks[0].chain(16 * CYLINDER),
        [(40 * CYLINDER + 17, 24 * CYLINDER - 17)],
        "the first record left heading the chain"
    );
    keyed(&mut m, " ");
}

#[test]
fn an_unformatted_logical_drive_is_listed_as_unknown() {
    let image = partitioned(|disk| {
        disk.make_primary(table::Extent { start: 0, end: 31 }, true)
            .unwrap();
        disk.make_extended(table::Extent { start: 32, end: 63 })
            .unwrap();
        disk.make_logical(table::Extent { start: 32, end: 63 })
            .unwrap();
    });
    let (mut m, _) = machine(vec![image], &[]);
    run(&mut m, "fdisk");
    typed(&mut m, "4");
    let text = screen(&m);
    assert!(
        text.contains("Display Partition Information")
            && text.contains("The Extended DOS Partition contains Logical DOS Drives."),
        "{text}"
    );
    typed(&mut m, "");
    let text = screen(&m);
    assert!(
        text.contains("Display Logical DOS Drive Information")
            && text.contains("D:")
            && text.contains("UNKNOWN"),
        "{text}"
    );
    keyed(&mut m, ESC);
    keyed(&mut m, ESC);
    assert!(screen(&m).contains("A:\\>"), "{}", screen(&m));
}

#[test]
fn partitions_are_made_from_the_command_line_with_no_restart() {
    let (mut m, disks) = machine(vec![blank()], &[]);
    assert!(
        run(&mut m, "fdisk 1").contains("Invalid parameter"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fdisk /pri:1").contains("Invalid parameter"),
        "a size needs its disk:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fdisk 2 /pri:1").contains("Invalid parameter"),
        "one disk:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fdisk 3 /pri:1").contains("Parameter value not in allowed range - 3"),
        "{}",
        screen(&m)
    );
    assert!(
        disks[0].sector(0).iter().all(|&byte| byte == 0),
        "nothing written"
    );
    let text = run(&mut m, "fdisk 1 /pri:1 /ext:1 /q");
    assert!(
        text.contains("A:\\>fdisk 1 /pri:1 /ext:1 /q\n\nA:\\>"),
        "{text}"
    );
    assert!(!at_least(&mut m, 1), "{}", screen(&m));
    let whole = Entry {
        type_byte: table::NEW_TYPE,
        active: true,
        start_lba: 17,
        sectors: 63 * CYLINDER - 17,
    };
    assert_eq!(
        disks[0].entries(),
        [whole],
        "the primary partition took the disk, leaving none for the extended"
    );
    run(&mut m, "fdisk /q");
    keyed(&mut m, ESC);
    assert!(
        at_least(&mut m, 2),
        "/Q with nothing asked or changed:\n{}",
        screen(&m)
    );
}

#[test]
fn a_bad_track_in_a_partitions_system_area_moves_the_partition_past_it() {
    let (mut m, disks) = machine(vec![blank()], &[2]);
    run(&mut m, "fdisk 1 /pri:1");
    let moved = Entry {
        type_byte: table::NEW_TYPE,
        active: true,
        start_lba: 2 * CYLINDER,
        sectors: 61 * CYLINDER,
    };
    assert_eq!(
        disks[0].entries(),
        [moved],
        "the cylinder holding track 2 and those before it left out"
    );
}

#[test]
fn the_second_disk_is_set_up_through_the_fifth_choice() {
    let first = partitioned(|disk| {
        disk.make_primary(table::Extent { start: 0, end: 63 }, true)
            .unwrap();
    });
    let (mut m, disks) = machine(vec![first, blank()], &[]);
    let text = run(&mut m, "fdisk");
    assert!(
        text.contains("5. Select next fixed disk drive")
            && text.contains("Current fixed disk drive: 1"),
        "{text}"
    );
    typed(&mut m, "5");
    assert!(
        screen(&m).contains("Current fixed disk drive: 2"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "1");
    typed(&mut m, "2");
    typed(&mut m, "");
    assert!(
        screen(&m).contains("Extended DOS Partition created"),
        "an extended partition needs no primary on a second disk:\n{}",
        screen(&m)
    );
    keyed(&mut m, ESC);
    typed(&mut m, "");
    assert!(
        screen(&m).contains("D:"),
        "after the first disk's C:\n{}",
        screen(&m)
    );
    keyed(&mut m, ESC);
    keyed(&mut m, ESC);
    assert!(
        screen(&m).contains("System will now restart"),
        "{}",
        screen(&m)
    );
    let extended = Entry {
        type_byte: mbr::PART_TYPE_EXTENDED_CHS,
        active: false,
        start_lba: CYLINDER,
        sectors: 62 * CYLINDER,
    };
    assert_eq!(
        disks[1].entries(),
        [extended],
        "never on cylinder 0, which holds the master boot record"
    );
    keyed(&mut m, " ");
}

#[test]
fn a_machine_without_a_fixed_disk_is_told_so_once_the_screen_is_given_back() {
    let mut m = with_utilities(&[]);
    let text = run(&mut m, "fdisk");
    assert!(text.contains("No fixed disks present"), "{text}");
    assert!(!text.contains("FDISK Options"), "{text}");
}
