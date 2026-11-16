//! Test disks laid out as the 1541 User's Guide documents: the BAM on the
//! directory track's first sector, the directory chained from its second
//! three sectors apart, and each file's blocks chained from its first, the
//! last naming its last byte. Entries and blocks may also be placed by
//! hand, to lay out what no DOS writes or what a DOS is expected to write.

use alloc::vec;
use alloc::vec::Vec;

use crate::media::c1541::d64;
use crate::media::c1541::layout::{
    bam, entry, kind, side, DIRECTORY_TRACK, LINK_BYTES, NAME_BYTES, PADDING, TRACKS,
};

/// The type byte of a closed file of `kind`.
#[must_use]
pub const fn closed(kind: u8) -> u8 {
    kind | kind::CLOSED
}

/// The directory's sectors in turn.
const DIRECTORY_SECTORS: [u8; 6] = [1, 4, 7, 10, 13, 16];
const ENTRIES_PER_SECTOR: usize = 8;
/// Data a block holds after its link.
const BLOCK_DATA: usize = d64::SECTOR_BYTES - LINK_BYTES as usize;
/// A 35-track image's bytes.
const IMAGE_BYTES: usize = 174_848;

/// Where track `track`, sector `sector` lies in the image.
pub fn offset(track: u8, sector: u8) -> usize {
    let before: usize = (1..track).map(|t| usize::from(d64::sectors(t))).sum();
    (before + usize::from(sector)) * d64::SECTOR_BYTES
}

fn offset_in_bam(offset: u8) -> usize {
    self::offset(DIRECTORY_TRACK, 0) + usize::from(offset)
}

/// `name` padded to a name's length with shifted spaces.
fn padded(name: &[u8]) -> [u8; NAME_BYTES as usize] {
    let mut out = [PADDING; NAME_BYTES as usize];
    out[..name.len()].copy_from_slice(name);
    out
}

/// A directory entry: its type byte, name, first block, the block a replace
/// in progress starts at, its length, and a relative file's first side
/// sector and record length.
struct Entry {
    kind: u8,
    name: [u8; NAME_BYTES as usize],
    first: (u8, u8),
    replacement: (u8, u8),
    blocks: u16,
    side: (u8, u8),
    length: u8,
}

/// A relative file as a DOS lays it out: its records of `length` bytes end
/// to end along `blocks`, each empty but for those `records` holds, by
/// their index from 0; `sides` listing the blocks; the last block linked to
/// `spare` where it opened a side sector, else at its last whole record's
/// last byte; its entry counting `counted` blocks.
pub struct Relative<'a> {
    pub name: &'a [u8],
    pub length: u8,
    pub blocks: &'a [(u8, u8)],
    pub sides: &'a [(u8, u8)],
    pub records: &'a [(usize, &'a [u8])],
    pub spare: Option<(u8, u8)>,
    pub counted: u16,
}

/// An empty record's first byte.
const EMPTY_RECORD: u8 = 0xFF;

/// A disk being laid out.
pub struct Disk {
    image: Vec<u8>,
    free: Vec<Vec<bool>>,
    filling: Vec<u8>,
    next: (u8, u8),
    entries: Vec<Entry>,
}

impl Disk {
    /// A formatted disk named `name` with `id`, its files to come.
    #[must_use]
    pub fn new(name: &[u8], id: [u8; 2]) -> Self {
        let mut disk = Self {
            image: vec![0; IMAGE_BYTES],
            free: (1..=TRACKS)
                .map(|track| vec![true; usize::from(d64::sectors(track))])
                .collect(),
            filling: Vec::new(),
            next: (1, 0),
            entries: Vec::new(),
        };
        disk.take((DIRECTORY_TRACK, 0));
        let bam = offset(DIRECTORY_TRACK, 0);
        disk.image[bam + usize::from(bam::VERSION_AT)] = bam::VERSION;
        disk.image[bam + usize::from(bam::NAME)..][..usize::from(NAME_BYTES)]
            .copy_from_slice(&padded(name));
        let id_at = bam + usize::from(bam::ID);
        disk.image[id_at - 2..id_at].fill(PADDING);
        disk.image[id_at..id_at + 2].copy_from_slice(&id);
        disk.image[id_at + 2] = PADDING;
        let type_at = bam + usize::from(bam::DOS_TYPE_AT);
        disk.image[type_at..type_at + 2].copy_from_slice(&bam::DOS_TYPE);
        disk.image[type_at + 2..bam + usize::from(bam::HEADER_END)].fill(PADDING);
        disk
    }

    /// Block `(track, sector)` used by hand.
    pub fn take(&mut self, (track, sector): (u8, u8)) {
        self.free[usize::from(track - 1)][usize::from(sector)] = false;
    }

    /// Block `at` free in the BAM, whatever it holds.
    pub fn release(&mut self, (track, sector): (u8, u8)) {
        self.free[usize::from(track - 1)][usize::from(sector)] = true;
    }

    /// Every block of `track` used, by nothing the directory names.
    pub fn occupy(&mut self, track: u8) {
        self.free[usize::from(track - 1)].fill(false);
    }

    /// The BAM recording `track` wholly used, as the original's records
    /// the track a file is filling.
    pub fn filling(&mut self, track: u8) {
        self.filling.push(track);
    }

    /// The next free block from track 1 up, the directory's track passed.
    fn next_block(&mut self) -> (u8, u8) {
        if self.next.0 == DIRECTORY_TRACK {
            self.next = (DIRECTORY_TRACK + 1, 0);
        }
        let block = self.next;
        self.next.1 += 1;
        if self.next.1 == d64::sectors(self.next.0) {
            self.next = (self.next.0 + 1, 0);
        }
        block
    }

    /// `bytes` laid out from the next free block: its first block and
    /// length in blocks. Nothing takes one block.
    pub fn chain(&mut self, bytes: &[u8]) -> ((u8, u8), u16) {
        let count = bytes.len().div_ceil(BLOCK_DATA).max(1);
        let blocks: Vec<(u8, u8)> = (0..count).map(|_| self.next_block()).collect();
        self.chain_along(&blocks, bytes)
    }

    /// `bytes` laid along `blocks` in turn, as a DOS chose them: the first
    /// block and the length in blocks.
    pub fn chain_along(&mut self, blocks: &[(u8, u8)], bytes: &[u8]) -> ((u8, u8), u16) {
        let parts: Vec<&[u8]> = if bytes.is_empty() {
            vec![&[]]
        } else {
            bytes.chunks(BLOCK_DATA).collect()
        };
        assert_eq!(
            parts.len(),
            blocks.len(),
            "a block for each part of the bytes"
        );
        for (index, data) in parts.iter().enumerate() {
            let link = blocks
                .get(index + 1)
                .copied()
                .unwrap_or((0, u8::try_from(data.len() + 1).unwrap_or(u8::MAX)));
            self.block(blocks[index], link, data);
        }
        (blocks[0], u16::try_from(blocks.len()).unwrap_or(u16::MAX))
    }

    /// A block placed by hand: its link and data.
    pub fn block(&mut self, at: (u8, u8), (track, sector): (u8, u8), data: &[u8]) {
        self.take(at);
        let block = offset(at.0, at.1);
        self.image[block] = track;
        self.image[block + 1] = sector;
        self.image[block + 2..block + 2 + data.len()].copy_from_slice(data);
    }

    /// An entry placed by hand.
    pub fn entry(&mut self, kind: u8, name: &[u8], first: (u8, u8), blocks: u16) {
        self.entries.push(Entry {
            kind,
            name: padded(name),
            first,
            replacement: (0, 0),
            blocks,
            side: (0, 0),
            length: 0,
        });
    }

    /// A relative file laid out, its blocks and the spare used.
    pub fn relative(&mut self, file: &Relative<'_>) {
        let length = usize::from(file.length);
        let mut data = vec![0; file.blocks.len() * BLOCK_DATA];
        for start in (0..data.len()).step_by(length) {
            data[start] = EMPTY_RECORD;
        }
        for &(index, bytes) in file.records {
            let at = index * length;
            data[at..at + length].fill(0);
            data[at..at + bytes.len()].copy_from_slice(bytes);
        }
        let whole = data.len() / length * length;
        let last = u8::try_from(whole + 1 - (file.blocks.len() - 1) * BLOCK_DATA)
            .expect("a last record ends in the last block");
        for (index, (&at, part)) in file.blocks.iter().zip(data.chunks(BLOCK_DATA)).enumerate() {
            let link = file
                .blocks
                .get(index + 1)
                .copied()
                .or(file.spare)
                .unwrap_or((0, last));
            self.block(at, link, part);
        }
        if let Some(spare) = file.spare {
            self.take(spare);
        }
        let table: Vec<u8> = file
            .sides
            .iter()
            .flat_map(|&(track, sector)| [track, sector])
            .collect();
        let field = |offset: u8| usize::from(offset - LINK_BYTES);
        for (number, (&at, listed)) in file
            .sides
            .iter()
            .zip(file.blocks.chunks(usize::from(side::LISTED)))
            .enumerate()
        {
            let mut bytes = vec![0; BLOCK_DATA];
            bytes[field(side::NUMBER)] = u8::try_from(number).expect("six side sectors at most");
            bytes[field(side::RECORD_LENGTH)] = file.length;
            bytes[field(side::TABLE)..][..table.len()].copy_from_slice(&table);
            for (place, &(track, sector)) in listed.iter().enumerate() {
                bytes[field(side::POINTERS) + 2 * place] = track;
                bytes[field(side::POINTERS) + 2 * place + 1] = sector;
            }
            let pointers_end = usize::from(side::POINTERS) + 2 * listed.len() - 1;
            let link = file.sides.get(number + 1).copied().unwrap_or((
                0,
                u8::try_from(pointers_end).expect("a list within its block"),
            ));
            self.block(at, link, &bytes);
        }
        self.entries.push(Entry {
            kind: closed(kind::REL),
            name: padded(file.name),
            first: file.blocks[0],
            replacement: (0, 0),
            blocks: file.counted,
            side: file.sides[0],
            length: file.length,
        });
    }

    fn named(&mut self, name: &[u8]) -> &mut Entry {
        let name = padded(name);
        self.entries
            .iter_mut()
            .find(|entry| entry.name == name)
            .expect("an entry of that name")
    }

    /// The entry named `name` given type byte `kind`.
    pub fn retype(&mut self, name: &[u8], kind: u8) {
        self.named(name).kind = kind;
    }

    /// The entry named `name` named `new`.
    pub fn rename(&mut self, name: &[u8], new: &[u8]) {
        self.named(name).name = padded(new);
    }

    /// The entry named `name` given the file at `first` of `blocks` blocks.
    pub fn refile(&mut self, name: &[u8], first: (u8, u8), blocks: u16) {
        let entry = self.named(name);
        entry.first = first;
        entry.blocks = blocks;
    }

    /// The entry named `name` given a length of `blocks`.
    pub fn counted(&mut self, name: &[u8], blocks: u16) {
        self.named(name).blocks = blocks;
    }

    /// The entry named `name` marked as being replaced by a file starting at
    /// `first`.
    pub fn replacing(&mut self, name: &[u8], first: (u8, u8)) {
        let entry = self.named(name);
        entry.kind |= kind::REPLACING;
        entry.replacement = first;
    }

    /// The BAM's byte at `offset`, set by hand.
    pub fn bam(&mut self, offset: u8, value: u8) {
        self.image[offset_in_bam(offset)] = value;
    }

    /// A closed file of `kind` holding `bytes`.
    pub fn file(&mut self, name: &[u8], kind: u8, bytes: &[u8]) {
        let (first, blocks) = self.chain(bytes);
        self.entry(closed(kind), name, first, blocks);
    }

    /// The image, the directory written and the BAM counting what it takes.
    #[must_use]
    pub fn image(mut self) -> Vec<u8> {
        let sectors = self.entries.len().div_ceil(ENTRIES_PER_SECTOR).max(1);
        for (index, &sector) in DIRECTORY_SECTORS[..sectors].iter().enumerate() {
            self.take((DIRECTORY_TRACK, sector));
            let at = offset(DIRECTORY_TRACK, sector);
            let link = DIRECTORY_SECTORS[..sectors]
                .get(index + 1)
                .map_or((0, 0xFF), |&next| (DIRECTORY_TRACK, next));
            (self.image[at], self.image[at + 1]) = link;
            for (slot, found) in self
                .entries
                .iter()
                .skip(index * ENTRIES_PER_SECTOR)
                .take(ENTRIES_PER_SECTOR)
                .enumerate()
            {
                let place = at + slot * usize::from(entry::BYTES);
                self.image[place + usize::from(entry::TYPE)] = found.kind;
                self.image[place + usize::from(entry::TRACK)] = found.first.0;
                self.image[place + usize::from(entry::SECTOR)] = found.first.1;
                self.image[place + usize::from(entry::NAME)..][..usize::from(NAME_BYTES)]
                    .copy_from_slice(&found.name);
                (
                    self.image[place + usize::from(entry::REPLACEMENT)],
                    self.image[place + usize::from(entry::REPLACEMENT) + 1],
                ) = found.replacement;
                self.image[place + usize::from(entry::BLOCKS)..][..2]
                    .copy_from_slice(&found.blocks.to_le_bytes());
                (
                    self.image[place + usize::from(entry::SIDE)],
                    self.image[place + usize::from(entry::SIDE) + 1],
                ) = found.side;
                self.image[place + usize::from(entry::RECORD_LENGTH)] = found.length;
            }
        }
        let bam = offset(DIRECTORY_TRACK, 0);
        self.image[bam] = DIRECTORY_TRACK;
        self.image[bam + 1] = DIRECTORY_SECTORS[0];
        for (track, sectors) in (1..=TRACKS).zip(&self.free) {
            let at = bam + usize::from(bam::TRACK_ENTRY_BYTES) * usize::from(track);
            self.image[at..at + usize::from(bam::TRACK_ENTRY_BYTES)].fill(0);
            if self.filling.contains(&track) {
                continue;
            }
            self.image[at] =
                u8::try_from(sectors.iter().filter(|&&free| free).count()).unwrap_or(u8::MAX);
            for (sector, &free) in sectors.iter().enumerate() {
                if free {
                    self.image[at + 1 + sector / 8] |= 1 << (sector % 8);
                }
            }
        }
        self.image
    }
}

/// Chains no DOS writes, each a sequential file's: last blocks naming no
/// data byte, a lone block holding one byte, links and entries off the
/// disk; eight entries, so the directory's first sector is full.
#[must_use]
pub fn odd_chains() -> Disk {
    let mut disk = Disk::new(b"CHAINS", *b"CH");
    disk.block((2, 0), (0, 1), b"QQ");
    disk.block((2, 1), (0, 2), b"Z");
    disk.block((2, 2), (0, 3), b"YX");
    disk.block((2, 3), (2, 4), &[b'S'; 254]);
    disk.block((2, 4), (0, 2), b"T");
    disk.block((2, 5), (36, 0), &[b'D'; 254]);
    disk.block((2, 6), (1, 25), &[b'C'; 254]);
    for (name, first, blocks) in [
        (&b"EMPTY1"[..], (2, 0), 1),
        (b"ONEBYTE", (2, 1), 1),
        (b"TWOBYTE", (2, 2), 1),
        (b"TWOSEQ", (2, 3), 2),
        (b"BADTRACK", (2, 5), 2),
        (b"BADSECTOR", (2, 6), 2),
        (b"ENTRYBAD", (40, 0), 1),
        (b"ENTRYT0", (0, 0), 1),
    ] {
        disk.entry(closed(kind::SEQ), name, first, blocks);
    }
    disk
}

/// A program: its load address, then `body`.
#[must_use]
pub fn program(load: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = load.to_le_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

/// A program at `$0801` of a `PRINT "text"` line for each of `lines`, as
/// BASIC keeps what is typed at its prompt.
#[must_use]
pub fn basic(lines: &[(u16, &[u8])]) -> Vec<u8> {
    const START: u16 = 0x0801;
    const PRINT: u8 = 0x99;
    let mut body = Vec::new();
    for &(number, text) in lines {
        let mut line = number.to_le_bytes().to_vec();
        line.extend_from_slice(&[PRINT, b'"']);
        line.extend_from_slice(text);
        line.extend_from_slice(&[b'"', 0]);
        let next = START + u16::try_from(body.len() + 2 + line.len()).unwrap_or(0);
        body.extend_from_slice(&next.to_le_bytes());
        body.extend_from_slice(&line);
    }
    body.extend_from_slice(&[0, 0]);
    program(START, &body)
}

/// `10 PRINT "text"` at `$0801`.
#[must_use]
pub fn printing(text: &[u8]) -> Vec<u8> {
    basic(&[(10, text)])
}
