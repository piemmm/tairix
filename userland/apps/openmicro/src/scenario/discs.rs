//! The discs scenarios name, built here byte for byte as the ones the
//! originals were observed with.

use alloc::vec;
use alloc::vec::Vec;

use tairix_dfs::catalogue::{
    self, Catalogue, Entry, CATALOGUE_LEN, MAX_FILES, NAME_LEN, TITLE_LEN,
};
use tairix_dfs::image::SECTOR_LEN;
use tairix_dfs::name::PAD;

/// A catalogue entry: name, directory, the load and execution addresses the
/// OS gives, length, first sector, and lock.
struct File {
    name: &'static [u8],
    dir: u8,
    load: u32,
    exec: u32,
    length: u32,
    start: u16,
    locked: bool,
}

impl File {
    /// The entry the catalogue holds it as; `None` for a name longer than
    /// one holds or an address no entry reads back as.
    fn entry(&self) -> Option<Entry> {
        let mut name = [PAD; NAME_LEN];
        name.get_mut(..self.name.len())?.copy_from_slice(self.name);
        Some(Entry {
            name,
            dir: self.dir,
            locked: self.locked,
            load: catalogue::field(self.load)?,
            exec: catalogue::field(self.exec)?,
            length: self.length,
            start: self.start,
        })
    }
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
    let mut catalogue = Catalogue::blank(sectors);
    catalogue.title = [0; TITLE_LEN];
    catalogue
        .title
        .get_mut(..title.len())?
        .copy_from_slice(title);
    catalogue.cycle = cycle;
    let mut entries = [Entry::NONE; MAX_FILES as usize];
    let held = entries.get_mut(..files.len())?;
    for (slot, file) in held.iter_mut().zip(files) {
        *slot = file.entry()?;
    }
    catalogue.set_files(held)?;
    let mut image = vec![0; usize::from(sectors) * SECTOR_LEN];
    catalogue.write(image.get_mut(..CATALOGUE_LEN)?.try_into().ok()?);
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

/// A file `name` loaded and entered at `address`, `length` long, at `start`.
const fn program(name: &'static [u8], address: u32, length: u32, start: u16) -> File {
    File {
        name,
        dir: b'$',
        load: address,
        exec: address,
        length,
        start,
        locked: false,
    }
}

/// Code that stores the A, X, Y and P it is entered with at &70 to &73.
const ENTRY_STATE: [u8; 11] = [
    0x85, 0x70, 0x86, 0x71, 0x84, 0x72, 0x08, 0x68, 0x85, 0x73, 0x60,
];
/// Code that prints `OK`.
const PRINT_OK: [u8; 11] = [
    0xA9, b'O', 0x20, 0xEE, 0xFF, 0xA9, b'K', 0x20, 0xEE, 0xFF, 0x60,
];
/// Two tracks' worth of bytes: a linear congruential generator's top byte,
/// so no run of them repeats another.
const NOISE: [u8; 0x800] = {
    let mut bytes = [0; 0x800];
    let mut x: u32 = 1;
    let mut i = 0;
    while i < bytes.len() {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        bytes[i] = x.to_le_bytes()[3];
        i += 1;
    }
    bytes
};

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
        // Files that land on what a load is decided from: X on the
        // catalogue, R and W, which crosses a track, on the DFS's own
        // workspace; K, of &55s, on a block put at &3000; and E, which
        // records the registers it is entered with.
        "lowload" => image(
            b"LOWLOAD",
            800,
            0,
            &[
                program(b"W", 0x1000, 0x800, 8),
                program(b"R", 0x1000, 0x200, 6),
                program(b"E", 0x3000, 0x100, 5),
                program(b"K", 0x3000, 0x100, 4),
                program(b"X", 0x0E00, 0x200, 2),
            ],
            &[
                (0x200, &[0xAA; 0x200]),
                (0x400, &[0x55; 0x100]),
                (0x500, &ENTRY_STATE),
                (0x600, &PRINT_OK),
                (0x800, &NOISE),
            ],
        ),
        _ => None,
    }
}
