//! The screens `FDISK` draws from its tables, as DOS 4.0's draws them: its
//! messages with the inserts each one's figures make.

use super::screen::{attr, Screen, LAST_COLUMN, LAST_ROW};
use super::table::{
    first_logical_letter, percent, primary_letter, Disk, Extent, Kind, Logical, Shape, LABEL_LEN,
    SYSTEM_LEN,
};
use super::text;
use crate::dos::formats::is_dos;
use alloc::vec::Vec;

/// `value` in four columns, as C's `%4.0d` prints it: a zero is none at all.
#[must_use]
pub fn number(value: u16) -> [u8; 4] {
    let mut out = [b' '; 4];
    let (mut rest, mut at) = (value, out.len());
    while rest > 0 && at > 0 {
        at -= 1;
        out[at] = b'0' + u8::try_from(rest % 10).unwrap_or(0);
        rest /= 10;
    }
    out
}

/// `value` percent in four columns, as `%3.0d%%` prints it.
#[must_use]
pub fn share(value: u16) -> [u8; 4] {
    let digits = number(value);
    [digits[1], digits[2], digits[3], b'%']
}

/// The digit naming disk `disk`, from 0, to the user.
#[must_use]
pub fn disk_digit(disk: usize) -> u8 {
    b'1'.saturating_add(u8::try_from(disk).unwrap_or(0))
}

/// The first screen's head: the system's version, and the program's name.
pub fn title(screen: &mut Screen) {
    screen.centred(0, crate::dos::shell::text::VERSION);
    screen.centred(1, text::PROGRAM);
}

/// The head of every screen past the first: the screen cleared, `title`,
/// the disk being set up, and how to go back.
pub fn heading(screen: &mut Screen, title: &str, disk: usize) {
    screen.clear(0, LAST_ROW);
    screen.draw(title, &[]);
    screen.draw(text::CURRENT_DISK, &[disk_digit(disk)]);
    screen.draw(text::RETURN, &[]);
}

/// The name a partition of `kind` is listed by.
#[must_use]
pub const fn kind_name(kind: Kind) -> &'static [u8; 7] {
    match kind {
        Kind::Dos => b"PRI DOS",
        Kind::Extended => b"EXT DOS",
        Kind::Xenix => b" XENIX ",
        Kind::PcIx => b" PC/IX ",
        Kind::Table => b" Table ",
        Kind::Other => b"Non-DOS",
    }
}

/// Disk `current`'s partitions listed at rows 8 to 12, the rows cleared
/// first, or that it has none: whether it has any.
pub fn table(screen: &mut Screen, disks: &[Disk], current: usize) -> bool {
    let Some(disk) = disks.get(current) else {
        return false;
    };
    let mut inserts = Vec::with_capacity(4 * 19);
    let listed = disk.listed();
    for (n, primary) in listed.iter().enumerate() {
        let letter = if primary.kind() == Kind::Dos {
            [primary_letter(disks, current), b':']
        } else {
            [b' '; 2]
        };
        let cylinders = primary.extent.map_or(0, Extent::cylinders);
        inserts.extend_from_slice(&letter);
        inserts.push(b'1'.saturating_add(u8::try_from(n).unwrap_or(0)));
        inserts.push(if primary.active { b'A' } else { b' ' });
        inserts.extend_from_slice(kind_name(primary.kind()));
        inserts.extend_from_slice(&number(disk.shape.mbytes(cylinders)));
        inserts.extend_from_slice(&share(percent(cylinders, disk.shape.cylinders)));
    }
    inserts.resize(4 * 19, b' ');
    screen.window(8, 0, 12, super::screen::LAST_COLUMN);
    if listed.is_empty() {
        screen.draw(text::NO_PARTITIONS, &[]);
        return false;
    }
    screen.draw(text::TABLE, &inserts);
    true
}

/// The disk's size, under its table.
pub fn disk_space(screen: &mut Screen, disk: &Disk) {
    screen.draw(
        text::DISK_SPACE,
        &number(disk.shape.mbytes(disk.shape.cylinders)),
    );
}

/// The largest partition `run` offers, in megabytes and as a share of
/// `whole` cylinders, said by `message`.
pub fn maximum(screen: &mut Screen, message: &str, shape: Shape, run: Extent, whole: u16) {
    let mut inserts = number(shape.mbytes(run.cylinders())).to_vec();
    inserts.extend_from_slice(&share(percent(run.cylinders(), whole)));
    screen.draw(message, &inserts);
}

/// The size of the disk's extended partition, under its drives' list.
pub fn extended_space(screen: &mut Screen, disk: &Disk) {
    let mbytes = disk
        .extended()
        .map_or(0, |extended| disk.shape.mbytes(extended.cylinders()));
    screen.draw(text::EXTENDED_SPACE, &number(mbytes));
}

/// The DOS logical drives disk `current` lists, with the letters DOS gives
/// them, none past `Z`.
#[must_use]
pub fn lettered(disks: &[Disk], current: usize) -> Vec<(u8, &Logical)> {
    let first = first_logical_letter(disks, current);
    let Some(disk) = disks.get(current) else {
        return Vec::new();
    };
    disk.drives()
        .filter(|drive| is_dos(drive.type_byte()))
        .enumerate()
        .map(|(n, drive)| {
            let letter = first.saturating_add(u8::try_from(n).unwrap_or(u8::MAX));
            (if letter > b'Z' { b' ' } else { letter }, drive)
        })
        .collect()
}

/// Disk `current`'s logical drives listed at rows 2 to 15, the rows cleared
/// first, twelve to a column, or that it has none.
pub fn drives(screen: &mut Screen, disks: &[Disk], current: usize) {
    let Some(disk) = disks.get(current) else {
        return;
    };
    let whole = disk.extended().map_or(0, Extent::cylinders);
    let listed = lettered(disks, current);
    let mut inserts = Vec::with_capacity(listed.len() * 29);
    for (letter, drive) in &listed {
        let cylinders = drive.extent().cylinders();
        inserts.push(*letter);
        inserts.push(if *letter == b' ' { b' ' } else { b':' });
        inserts.extend_from_slice(&padded::<LABEL_LEN>(&drive.label));
        inserts.extend_from_slice(&number(disk.shape.mbytes(cylinders)));
        inserts.extend_from_slice(&padded::<SYSTEM_LEN>(&drive.system));
        inserts.extend_from_slice(&share(percent(cylinders, whole)));
    }
    screen.clear(2, 15);
    if listed.is_empty() {
        screen.draw(text::NO_DRIVES, &[]);
        return;
    }
    for (from, block) in [
        (0, text::DRIVES_LEFT),
        (6, text::DRIVES_LEFT_MORE),
        (12, text::DRIVES_RIGHT),
        (18, text::DRIVES_RIGHT_MORE),
    ] {
        if listed.len() > from {
            screen.draw(block, &inserts[from * 29..]);
        }
    }
}

/// The `n`th drive [`drives`] listed, from 0, shown deleted: its letter
/// kept, the rest of its line saying so.
pub fn deleted(screen: &mut Screen, n: usize) {
    let (row, column, right) = if n < LISTED_A_COLUMN {
        (n, DELETED_LEFT, LEFT_EDGE)
    } else {
        (n - LISTED_A_COLUMN, DELETED_RIGHT, LAST_COLUMN)
    };
    let row = FIRST_DRIVE_ROW.saturating_add(u8::try_from(row).unwrap_or(u8::MAX));
    screen.window(row, column, row, right);
    screen.write(row, column, attr::HIGH, text::DRIVE_DELETED);
}

/// Where [`drives`] lists: the row its first drive is on, the drives a
/// column takes, and where a line's text after the letter begins and the
/// left column's ends.
const FIRST_DRIVE_ROW: u8 = 4;
const LISTED_A_COLUMN: usize = 12;
const DELETED_LEFT: u8 = 4;
const DELETED_RIGHT: u8 = 45;
const LEFT_EDGE: u8 = 39;

/// `field` as C's `%-N.Ns` prints it: up to its first NUL, blanks after.
fn padded<const N: usize>(field: &[u8; N]) -> [u8; N] {
    let mut out = [b' '; N];
    for (slot, &byte) in out
        .iter_mut()
        .zip(field.iter().take_while(|&&byte| byte != 0))
    {
        *slot = byte;
    }
    out
}

/// The main menu over whatever the screen shows: its options, the disk
/// being set up, the warning a first disk with partitions and none active
/// gets, and the fifth option with a second disk.
pub fn main_menu(screen: &mut Screen, disks: &[Disk], current: usize) {
    screen.draw(text::OPTIONS, &[]);
    screen.draw(text::CHOOSE, &[]);
    screen.draw(text::ENTER_CHOICE, &[]);
    screen.draw(text::CURRENT_DISK, &[disk_digit(current)]);
    screen.clear(20, 21);
    let unstartable = disks
        .get(current)
        .is_some_and(|disk| disk.primaries().next().is_some() && disk.active().is_none());
    if current == 0 && unstartable {
        screen.draw(text::NOTHING_ACTIVE, &[]);
    }
    if disks.len() > 1 {
        screen.draw(text::NEXT_DISK, &[]);
    }
}

#[cfg(test)]
#[path = "views_tests.rs"]
mod tests;
