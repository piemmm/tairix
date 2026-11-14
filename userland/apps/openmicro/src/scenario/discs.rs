//! The discs scenarios name, built here byte for byte as the ones the
//! originals were observed with.

use alloc::vec;
use alloc::vec::Vec;

/// A catalogue entry: name, directory, load and execution addresses,
/// length, first sector, and lock.
struct File {
    name: &'static [u8],
    dir: u8,
    load: u32,
    exec: u32,
    length: u32,
    start: u16,
    locked: bool,
}

/// An SSD image of `sectors` sectors titled `title`, its cycle number
/// `cycle`, holding `files` in catalogue order, with `data` at its offsets;
/// none past the 31 files a catalogue holds.
fn image(
    title: &[u8],
    sectors: u16,
    cycle: u8,
    files: &[File],
    data: &[(usize, &[u8])],
) -> Option<Vec<u8>> {
    let mut image = vec![0; usize::from(sectors) * 256];
    let (first, last) = title.split_at(title.len().min(8));
    image.get_mut(..first.len())?.copy_from_slice(first);
    image.get_mut(256..256 + last.len())?.copy_from_slice(last);
    image[0x104] = cycle;
    image[0x105] = u8::try_from(files.len() * 8)
        .ok()
        .filter(|&end| end < 0xF9)?;
    let [sectors_low, sectors_high] = sectors.to_le_bytes();
    image[0x106] = sectors_high & 3;
    image[0x107] = sectors_low;
    for (n, file) in files.iter().enumerate() {
        let at = 8 + 8 * n;
        let name = &mut image[at..at + 7];
        name.fill(b' ');
        name.get_mut(..file.name.len())?.copy_from_slice(file.name);
        image[at + 7] = file.dir | if file.locked { 0x80 } else { 0 };
        let top = |value: u32| value.to_le_bytes()[2] & 3;
        let [load_0, load_1, ..] = file.load.to_le_bytes();
        let [exec_0, exec_1, ..] = file.exec.to_le_bytes();
        let [length_0, length_1, ..] = file.length.to_le_bytes();
        let [start_0, start_1] = file.start.to_le_bytes();
        let info = 0x100 + at;
        image[info..info + 8].copy_from_slice(&[
            load_0,
            load_1,
            exec_0,
            exec_1,
            length_0,
            length_1,
            (top(file.exec) << 6) | (top(file.length) << 4) | (top(file.load) << 2) | (start_1 & 3),
            start_0,
        ]);
    }
    for &(at, bytes) in data {
        image.get_mut(at..at + bytes.len())?.copy_from_slice(bytes);
    }
    Some(image)
}

/// A file `name` of one sector at `start`.
const fn sector(name: &'static [u8], start: u16) -> File {
    File {
        name,
        dir: b'$',
        load: 0x1900,
        exec: 0x1900,
        length: 0x100,
        start,
        locked: false,
    }
}

/// File A of 288 sectors at `start`.
const fn big(start: u16) -> File {
    File {
        name: b"A",
        dir: b'$',
        load: 0x1900,
        exec: 0x1900,
        length: 0x12000,
        start,
        locked: false,
    }
}

/// The disc named `name`, if a scenario may name it.
#[must_use]
pub fn named(name: &str) -> Option<Vec<u8>> {
    match name {
        // Three files on an 80-track disc: LONGNAM with a BASIC program's
        // addresses in the I/O processor, FILE in directory B, and A
        // holding an empty BASIC program.
        "testdisc" => image(
            b"TESTDISC",
            800,
            3,
            &[
                File {
                    name: b"LONGNAM",
                    dir: b'$',
                    load: 0xFFFF_1900,
                    exec: 0xFFFF_8023,
                    length: 0x345,
                    start: 4,
                    locked: false,
                },
                File {
                    name: b"FILE",
                    dir: b'B',
                    load: 0x3000,
                    exec: 0x3010,
                    length: 0x100,
                    start: 3,
                    locked: false,
                },
                File {
                    name: b"A",
                    dir: b'$',
                    load: 0x1900,
                    exec: 0x1900,
                    length: 0x100,
                    start: 2,
                    locked: false,
                },
            ],
            &[(0x200, &[0x0D, 0xFF])],
        ),
        "blank80" => image(b"TESTDISC", 800, 0, &[], &[]),
        "blank40" => image(b"BLANK40", 400, 0, &[], &[]),
        "other80" => image(b"OTHER", 800, 0, &[], &[]),
        // A 40-track disc whose one file claims sectors past its end.
        "fault40" => image(
            b"SHORT",
            400,
            0,
            &[File {
                name: b"BIG",
                dir: b'$',
                load: 0x1900,
                exec: 0x1900,
                length: 0x100,
                start: 500,
                locked: false,
            }],
            &[],
        ),
        "full80" => image(b"FULL", 800, 0, &[], &[]),
        // One file filling all but twenty sectors.
        "near80" => image(
            b"NEAR",
            800,
            0,
            &[File {
                name: b"FILLER",
                dir: b'$',
                load: 0x1900,
                exec: 0x1900,
                length: 778 << 8,
                start: 2,
                locked: false,
            }],
            &[],
        ),
        // One sector of file at the catalogue's end, then a gap before a
        // second file, or the disc's end.
        "gap100" => image(b"GAP", 800, 0, &[sector(b"B", 102), sector(b"A", 2)], &[]),
        "gap300" => image(b"GAP", 800, 0, &[sector(b"B", 302), sector(b"A", 2)], &[]),
        "nogap" => image(b"SMALL", 800, 0, &[sector(b"B", 4), sector(b"A", 2)], &[]),
        // A file past 64 KiB, with room after it or a file 498 sectors on.
        "big" => image(b"BIG", 800, 0, &[big(2)], &[]),
        "bigcapped" => image(b"BIG", 800, 0, &[sector(b"B", 500), big(2)], &[]),
        _ => None,
    }
}
