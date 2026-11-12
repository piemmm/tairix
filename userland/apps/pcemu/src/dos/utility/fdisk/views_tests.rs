//! The screens drawn from the tables, read back as text.

use super::*;
use crate::dos::utility::fdisk::table::{Extent, Shape};
use alloc::format;
use alloc::string::String;
use tairix_partition::mbr::MBR_SECTOR_LEN;

/// A 64-cylinder disk with a primary partition of 16, active, and an
/// extended one of the rest holding drives of 24 and 24 cylinders.
fn divided() -> Disk {
    let base = crate::dos::boot::master_boot_record().unwrap();
    let shape = Shape {
        cylinders: 64,
        heads: 2,
        sectors: 17,
    };
    let mut disk = Disk::read(shape, &[0; MBR_SECTOR_LEN], &base);
    disk.make_primary(Extent { start: 0, end: 15 }, true)
        .unwrap();
    disk.make_extended(Extent { start: 16, end: 63 }).unwrap();
    disk.make_logical(Extent { start: 16, end: 39 }).unwrap();
    disk.make_logical(Extent { start: 40, end: 63 }).unwrap();
    disk.logical[0].label[..4].copy_from_slice(b"WORK");
    disk.logical[0].system.copy_from_slice(b"FAT12   ");
    disk
}

/// The text of `screen`'s calls on a blank screen, a line a row, blanks on
/// the right dropped.
fn shown(screen: &Screen) -> Vec<String> {
    let mut grid = [[b' '; 80]; 25];
    let ops = screen.encoded(usize::MAX);
    let mut at = 0;
    while ops[at] != 0 {
        match ops[at] {
            1 => {
                let (row, column, len) = (
                    usize::from(ops[at + 1]),
                    usize::from(ops[at + 2]),
                    usize::from(ops[at + 4]),
                );
                grid[row][column..column + len].copy_from_slice(&ops[at + 5..at + 5 + len]);
                at += 5 + len;
            }
            2 => {
                for line in &mut grid[usize::from(ops[at + 1])..=usize::from(ops[at + 3])] {
                    line[usize::from(ops[at + 2])..=usize::from(ops[at + 4])].fill(b' ');
                }
                at += 5;
            }
            _ => at += 1,
        }
    }
    grid.iter()
        .map(|row| String::from(String::from_utf8_lossy(row).trim_end()))
        .collect()
}

#[test]
fn numbers_print_as_c_prints_them_a_zero_as_nothing() {
    assert_eq!(number(0), *b"    ");
    assert_eq!(number(7), *b"   7");
    assert_eq!(number(1234), *b"1234");
    assert_eq!(share(0), *b"   %");
    assert_eq!(share(100), *b"100%");
}

#[test]
fn the_table_lists_each_partition_its_letter_status_kind_size_and_share() {
    let disks = [divided()];
    let mut screen = Screen::new();
    assert!(table(&mut screen, &disks, 0));
    let rows = shown(&screen);
    assert_eq!(
        rows[8],
        "    Partition Status   Type    Size in Mbytes   Percentage of Disk Used"
    );
    let field = |row: usize, from: usize, len: usize| {
        String::from(&format!("{:<80}", rows[row])[from..from + len])
    };
    assert_eq!(
        [
            field(9, 5, 2),
            field(9, 8, 1),
            field(9, 17, 1),
            field(9, 22, 7)
        ],
        ["C:", "1", "A", "PRI DOS"],
        "{rows:?}"
    );
    assert_eq!(
        [field(9, 36, 4), field(9, 48, 4)],
        ["    ", " 25%"],
        "under half a megabyte shows none"
    );
    assert_eq!(
        [
            field(10, 5, 2),
            field(10, 8, 1),
            field(10, 17, 1),
            field(10, 22, 7)
        ],
        ["  ", "2", " ", "EXT DOS"]
    );
    assert_eq!([field(10, 36, 4), field(10, 48, 4)], ["   1", " 75%"]);
    assert_eq!(rows[11], "");
    let mut empty = Screen::new();
    let blank = Disk::read(
        disks[0].shape,
        &[0; MBR_SECTOR_LEN],
        &crate::dos::boot::master_boot_record().unwrap(),
    );
    assert!(!table(&mut empty, &[blank], 0));
    assert_eq!(shown(&empty)[21], "    No partitions defined");
}

#[test]
fn the_drive_list_gives_each_drive_its_letter_label_size_system_and_share() {
    let disks = [divided()];
    let mut screen = Screen::new();
    drives(&mut screen, &disks, 0);
    let rows = shown(&screen);
    assert_eq!(rows[3], "Drv Volume Label  Mbytes  System  Usage");
    let field = |row: usize, from: usize, len: usize| {
        String::from(&format!("{:<80}", rows[row])[from..from + len])
    };
    assert_eq!(
        [
            field(4, 0, 2),
            field(4, 4, 11),
            field(4, 19, 4),
            field(4, 25, 8),
            field(4, 35, 4)
        ],
        ["D:", "WORK       ", "    ", "FAT12   ", " 50%"],
        "{rows:?}"
    );
    assert_eq!(
        [field(5, 0, 2), field(5, 4, 11), field(5, 35, 4)],
        ["E:", "           ", " 50%"]
    );
    let mut none = Screen::new();
    let mut empty = divided();
    empty.logical.clear();
    drives(&mut none, &[empty], 0);
    assert_eq!(shown(&none)[10], "    No logical drives defined");
}

#[test]
fn only_a_first_disk_with_partitions_and_none_active_is_warned_of() {
    let mut inactive = divided();
    inactive.record.0[446] = 0;
    let mut screen = Screen::new();
    main_menu(&mut screen, &[inactive.clone()], 0);
    assert!(shown(&screen)[20].contains("No partitions are set active"));
    assert_eq!(screen.input, Some((17, 19)));
    let mut active = Screen::new();
    main_menu(&mut active, &[divided()], 0);
    assert!(!shown(&active)[20].contains("set active"));
    let mut second = Screen::new();
    main_menu(&mut second, &[divided(), inactive], 1);
    assert!(
        !shown(&second)[20].contains("set active"),
        "a second disk starts nothing"
    );
    assert!(shown(&second)[14].contains("5. Select next fixed disk drive"));
}
