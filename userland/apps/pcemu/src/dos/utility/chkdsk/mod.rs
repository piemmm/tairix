//! `CHKDSK [d:][[path]name] [/F] [/V]`: a disk's FAT and directories checked
//! against each other as DOS 4.0's `CHKDSK` checks them, with its messages.
//! The FAT is read whole through INT 25h beside a map of a byte a cluster,
//! the tree walked and each chain marked; what is wrong is said and, with
//! `/F`, put right through INT 26h, the FAT written back once at the end.
//! Where DOS 4.0's code is at odds with what it plainly means — a drive
//! written as a number, a count of garbage, a flag `/V` should not change —
//! the meaning is followed.

mod cells;
mod extents;
mod lost;
mod tree;

pub use extents::ChkdskExtents;
pub use lost::ChkdskLost;
pub use tree::{ChkdskTree, FixEntry, Tell};

use super::{carried as failed, disk_ioctl, figure_line, refuse, space, HeldFat, Refusal, Sectors};
use crate::dos::data::dpb;
use crate::dos::device::generic::{self, parameters};
use crate::dos::engine::Saved;
use crate::dos::fs::critical;
use crate::dos::ioctl::drive_bits;
use crate::dos::message::{self, decimal};
use crate::dos::program::{
    answer, handler_entry, say, say_error, Flow, Frame, Id, Int, Line, Program, Returned, Routine,
    ENDED,
};
use crate::dos::shell::dates::{two_digits, Writing};
use crate::dos::shell::{asciz_at, letter, parse, set_asciz};
use crate::frames::{locals, take, Far, Fault, Outcome};
use alloc::vec::Vec;
use cells::{Held, Volume};
use tairix_abi::time::DosTime;
use tairix_fat::boot::at as boot_at;

/// Where `CHKDSK`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// Its [`super::switch`]es, the drive checked and the user's own, from
    /// `0`, what the run has found or done, and the code it ends with.
    pub const SWITCHES: u16 = STATE;
    pub const DRIVE: u16 = STATE + 0x01;
    pub const USER_DRIVE: u16 = STATE + 0x02;
    pub const FLAGS: u16 = STATE + 0x03;
    pub const CODE: u16 = STATE + 0x04;
    /// The walk's depth.
    pub const DEPTH: u16 = STATE + 0x05;
    /// The paragraph past the program's memory as it started.
    pub const TOP: u16 = STATE + 0x08;
    /// The volume, from its DPB.
    pub const SECTOR_BYTES: u16 = STATE + 0x0A;
    pub const CLUSTER_SECTORS: u16 = STATE + 0x0C;
    pub const FATS: u16 = STATE + 0x0D;
    pub const MAX_CLUSTER: u16 = STATE + 0x0E;
    pub const FAT_SECTORS: u16 = STATE + 0x10;
    pub const FIRST_FAT: u16 = STATE + 0x12;
    pub const FIRST_DATA: u16 = STATE + 0x14;
    pub const ROOT_SECTOR: u16 = STATE + 0x16;
    pub const ROOT_ENTRIES: u16 = STATE + 0x18;
    /// The disk's bytes, as function `36h` counts them.
    pub const DISK_BYTES: u16 = STATE + 0x1A;
    /// The block the walk's levels, the FAT and the map lie in, and the
    /// FAT's and the map's segments.
    pub const BLOCK: u16 = STATE + 0x1E;
    pub const FAT: u16 = STATE + 0x20;
    pub const MAP: u16 = STATE + 0x22;
    /// The FAT copy being read or written, the copies that would not be
    /// written, and its sectors moved.
    pub const COPY: u16 = STATE + 0x24;
    pub const FAILED_COPIES: u16 = STATE + 0x25;
    pub const MOVED: u16 = STATE + 0x26;
    /// The INT 24h handler before `CHKDSK`'s own.
    pub const OLD_24: u16 = STATE + 0x28;
    /// The sector in [`BUFFER`], `FFFFFFFFh` for none.
    pub const BUFFERED: u16 = STATE + 0x2C;
    /// Lost chains being made files: the root entry looked at, the next
    /// chain's head, the chains left, and the stamp they are made with.
    pub const ROOT_INDEX: u16 = STATE + 0x30;
    pub const HEAD: u16 = STATE + 0x32;
    pub const LEFT: u16 = STATE + 0x34;
    pub const STAMP: u16 = STATE + 0x38;
    /// The [`super::count`]s, a dword each.
    pub const COUNTS: u16 = STATE + 0x3C;
    /// The name the next file made of a lost chain may take.
    pub const ORPHAN: u16 = STATE + 0x70;
    /// The line a question is answered on: its room, its length, the text.
    pub const LINE: u16 = STATE + 0x7C;
    pub const LINE_ROOM: u8 = 15;
    /// A name to say or to change directory to, ASCIZ.
    pub const NAME: u16 = STATE + 0x90;
    /// The DTA: a search's entry, or a find's.
    pub const DTA: u16 = STATE + 0xA0;
    /// An FCB: the label's search, or the file named's.
    pub const FCB: u16 = STATE + 0xD0;
    /// INT 25h's and 26h's request.
    pub const PACKET: u16 = STATE + 0xFC;
    /// A path said: `X:\`, the current directory and a name.
    pub const PATH: u16 = STATE + 0x110;
    pub const PATH_LEN: u16 = 0x60;
    /// The user's directory on the drive, from its `\`.
    pub const USER_DIR: u16 = STATE + 0x170;
    /// The file named on the command line.
    pub const SPEC: u16 = STATE + 0x1B4;
    pub const SPEC_LEN: u16 = 0x80;
    /// The country's information, the medium's ID, the device's
    /// parameters, and the path a recovered file's name is tried as.
    pub const INFO: u16 = STATE + 0x234;
    pub const MEDIA: u16 = STATE + 0x256;
    pub const PARAMS: u16 = STATE + 0x270;
    pub const PROBE: u16 = STATE + 0x2A0;
    /// The root's path, and its parent's.
    pub const ROOT: u16 = STATE + 0x2B4;
    pub const UP: u16 = STATE + 0x2B6;
    /// A sector of the disk.
    pub const BUFFER: u16 = STATE + 0x2C0;
    pub const BUFFER_LEN: u16 = 0x200;
    /// The report, as it is written.
    pub const REPORT: u16 = BUFFER + BUFFER_LEN;
    pub const REPORT_LEN: u16 = 0x300;
    /// Bytes of `CHKDSK`'s state.
    pub const LEN: u16 = REPORT + REPORT_LEN - STATE;

    const _: () = assert!(3 + crate::dos::data::cds::PATH_LEN <= PATH_LEN);
    const _: () = assert!(PATH + PATH_LEN <= USER_DIR);
    const _: () = assert!(USER_DIR + 1 + crate::dos::data::cds::PATH_LEN <= SPEC);
    const _: () = assert!(SPEC + SPEC_LEN <= INFO);
    const _: () = assert!(INFO as usize + crate::dos::country::INFO <= MEDIA as usize);
    const _: () = assert!(MEDIA + crate::dos::device::generic::media::LEN <= PARAMS);
    const _: () = assert!(DTA + 0x2B <= FCB);
    const _: () = assert!(FCB + 0x2C <= PACKET);
}

/// What the command line asks for.
pub mod switch {
    pub const FIX: u8 = 0x01;
    pub const VERBOSE: u8 = 0x02;
    /// A file is named, whose pieces are counted.
    pub const NAMED: u8 = 0x04;
}

/// What the run has found or done.
pub mod flag {
    /// The errors found have been said to be left unwritten.
    pub const ERRORS_SAID: u8 = 0x01;
    /// The FAT in memory differs from the disk's.
    pub const DIRTY: u8 = 0x02;
    /// Part of the tree went unwalked: no lost cluster is freed or made a
    /// file, as it may be that part's.
    pub const TRUNCATED: u8 = 0x04;
    pub const SECOND_PASS: u8 = 0x08;
    pub const FAT_READ: u8 = 0x10;
    /// The lost chains were made files on the disk.
    pub const RECOVERED: u8 = 0x20;
}

/// The counts the report gives, each kept as a dword.
pub mod count {
    pub const DIRECTORIES: u16 = 0;
    pub const DIRECTORY_CLUSTERS: u16 = 1;
    pub const FILES: u16 = 2;
    pub const FILE_CLUSTERS: u16 = 3;
    pub const HIDDEN: u16 = 4;
    pub const HIDDEN_CLUSTERS: u16 = 5;
    pub const BAD: u16 = 6;
    pub const LOST: u16 = 7;
    pub const CHAINS: u16 = 8;
    /// Lost clusters left as they were, neither freed nor made files.
    pub const UNRECOVERED: u16 = 9;
    pub const CROSSINGS: u16 = 10;
    pub const LEN: u16 = 11;
}

/// `CHKDSK`'s handlers.
pub mod handler {
    /// INT 23h, the work abandoned for the main routine to end it, writing
    /// nothing more.
    pub const BREAK: u16 = 0;
    pub const CRITICAL: u16 = 1;
}

/// What `CHKDSK` says, as DOS 4.0's says it.
pub mod text {
    pub use crate::dos::message::{BAD_DRIVE, MEMORY, NEW_LINE};
    pub const NETWORK: &[u8] = b"Cannot CHKDSK a network drive\r\n";
    pub const SUBST: &[u8] = b"Cannot CHKDSK a SUBSTed or ASSIGNed drive\r\n";
    pub const MEDIA: &[u8] = b"Invalid media type\r\n";
    pub const NO_ROOT: &[u8] = b"   Cannot CHDIR to root\r\n";
    pub const ERRORS: &[u8] = b"Errors found, F parameter not specified\r\nCorrections will not be written to disk\r\n\r\n";
    pub const CANNOT_GO_ON: &[u8] = b"   Processing cannot continue ";
    pub const FAT_BAD: &[u8] = b"   File allocation table bad";
    pub const WRITING_FAT: &[u8] = b"   Disk error writing FAT ";
    pub const UNRECOVERABLE: &[u8] = b"Unrecoverable error in directory\r\n";
    pub const WRITE_FAULT: &[u8] = b"Write fault error\r\n";
    pub const READ_FAULT: &[u8] = b"Read fault error\r\n";
}

const BYTES_IN: &[u8] = b" bytes in ";

/// The questions `CHKDSK` asks, each on its handle.
pub mod question {
    pub const LOST: u8 = 0;
    pub const CONVERT: u8 = 1;
    pub const NON_DOS: u8 = 2;
}

const QUESTIONS: [(&[u8], u16); 3] = [
    (b"Convert lost chains to files (Y/N)?", 1),
    (b"Convert directory to file (Y/N)?", 1),
    (b"Probable non-DOS disk\r\nContinue (Y/N)?", 2),
];

/// The code `CHKDSK` ends with when it could not go on.
const BAD_EXIT: u8 = 0xFF;
/// The drive the command line leaves to be the current one.
const CURRENT: u8 = 0xFF;
/// A drive the network serves or shares, as function `4409h` says; and one
/// `SUBST` or `ASSIGN` made.
/// The boot record `RAMDRIVE` makes, whose driver takes no generic IOCTL,
/// as DOS's `CHKDSK` knows it.
const RAMDRIVE: &[u8; 8] = b"RDV 1.20";
/// Levels of the walk, as deep as a path can name.
pub const LEVELS: u8 = crate::dos::program::walk::DEPTH;
/// Bytes of a level: its search FCB, its directory's first cluster and its
/// parent's, and what was found wrong there.
pub mod level {
    pub const FCB: u16 = 0x00;
    pub const SELF: u16 = 0x2C;
    pub const PARENT: u16 = 0x2E;
    /// The directory's invalid entry has been said.
    pub const SAID: u16 = 0x30;
    /// Its `.` or `..` could not be put right: its parent asks to make it a
    /// file.
    pub const DOTS_BAD: u16 = 0x31;
    pub const LEN: u16 = 0x32;
}

/// What `CHKDSK`'s command line names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    /// The drive, from `0`; none, the current one.
    pub drive: Option<u8>,
    /// The file whose pieces are counted.
    pub spec: Option<&'a [u8]>,
    pub switches: u8,
}

/// The drive a word names first, if it begins `X:`.
fn drive_of(word: &[u8]) -> Option<u8> {
    match word {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => {
            Some(letter.to_ascii_uppercase() - b'A')
        }
        _ => None,
    }
}

/// `CHKDSK`'s command line, as DOS 4.0's parser reads it: a drive, then a
/// file, or a file with its drive, and `/F` and `/V` each once.
///
/// # Errors
///
/// What DOS's parser says of a word it cannot take.
pub fn request(tail: &[u8]) -> Result<Request<'_>, Refusal<'_>> {
    let mut request = Request {
        drive: None,
        spec: None,
        switches: 0,
    };
    for word in parse::arguments(tail) {
        if let Some(switch) = word.strip_prefix(b"/") {
            let bit = match switch.to_ascii_uppercase().as_slice() {
                b"F" => switch::FIX,
                b"V" => switch::VERBOSE,
                _ => return Err(Refusal::Switch(word)),
            };
            if request.switches & bit != 0 {
                return Err(Refusal::Switch(word));
            }
            request.switches |= bit;
        } else if request.spec.is_some() {
            return Err(Refusal::TooMany(word));
        } else if request.drive.is_none() && word.len() == 2 && drive_of(word).is_some() {
            request.drive = drive_of(word);
        } else {
            if request.drive.is_none() {
                request.drive = drive_of(word);
            }
            request.spec = Some(word);
            request.switches |= switch::NAMED;
        }
    }
    Ok(request)
}

/// Whether a BPB is one DOS 4.0's `CHKDSK` checks: sectors of 128, 256 or
/// 512 bytes, one FAT or two, clusters a power of two to 128 sectors, a
/// FAT, and 2 to 512 root entries.
#[must_use]
pub fn checkable(bpb: &[u8]) -> bool {
    let word = |at: usize| {
        bpb.get(at..at + 2)
            .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
    };
    let (sector, cluster, fats, root, fat) = (
        word(0),
        bpb.get(2).copied().unwrap_or(0),
        bpb.get(5).copied().unwrap_or(0),
        word(6),
        word(11),
    );
    matches!(sector, 128 | 256 | 512)
        && matches!(fats, 1 | 2)
        && cluster.is_power_of_two()
        && fat != 0
        && (2..=512).contains(&root)
}

/// What the report says, from what the run counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub disk_bytes: u32,
    pub cluster_bytes: u32,
    pub clusters: u32,
    pub counts: [u32; count::LEN as usize],
    /// The lost chains were made files on the disk, not only counted.
    pub recovered: bool,
    pub memory: u32,
    pub free_memory: u32,
}

impl Totals {
    fn get(&self, which: u16) -> u32 {
        self.counts[usize::from(which)]
    }

    fn bytes(&self, clusters: u32) -> u32 {
        clusters.saturating_mul(self.cluster_bytes)
    }

    /// The clusters no file, directory, bad sector or lost chain holds.
    #[must_use]
    pub fn free(&self) -> u32 {
        [
            count::DIRECTORY_CLUSTERS,
            count::FILE_CLUSTERS,
            count::HIDDEN_CLUSTERS,
            count::BAD,
            count::LOST,
            count::UNRECOVERED,
        ]
        .iter()
        .fold(self.clusters, |left, &which| {
            left.saturating_sub(self.get(which))
        })
    }

    /// The report, each figure right-aligned in ten columns.
    #[must_use]
    pub fn report(&self) -> Vec<u8> {
        let mut out = Vec::new();
        figure_line(&mut out, self.disk_bytes, &[space::TOTAL]);
        let counted: [(u16, u16, &[u8]); 3] = [
            (count::HIDDEN_CLUSTERS, count::HIDDEN, b" hidden files"),
            (
                count::DIRECTORY_CLUSTERS,
                count::DIRECTORIES,
                b" directories",
            ),
            (count::FILE_CLUSTERS, count::FILES, b" user files"),
        ];
        for (clusters, number, what) in counted {
            if self.get(clusters) != 0 {
                figure_line(
                    &mut out,
                    self.bytes(self.get(clusters)),
                    &[BYTES_IN, &decimal(self.get(number)), what],
                );
            }
        }
        if self.get(count::LOST) != 0 {
            let lead: &[u8] = if self.recovered {
                BYTES_IN
            } else {
                b" bytes would be in "
            };
            figure_line(
                &mut out,
                self.bytes(self.get(count::LOST)),
                &[lead, &decimal(self.get(count::CHAINS)), b" recovered files"],
            );
        }
        if self.get(count::BAD) != 0 {
            figure_line(&mut out, self.bytes(self.get(count::BAD)), &[space::BAD]);
        }
        figure_line(&mut out, self.bytes(self.free()), &[space::AVAILABLE]);
        out.extend_from_slice(text::NEW_LINE);
        figure_line(&mut out, self.cluster_bytes, &[space::UNIT]);
        figure_line(
            &mut out,
            self.clusters,
            &[b" total allocation units on disk"],
        );
        figure_line(
            &mut out,
            self.free(),
            &[b" available allocation units on disk"],
        );
        out.extend_from_slice(text::NEW_LINE);
        figure_line(&mut out, self.memory, &[b" total bytes memory"]);
        figure_line(&mut out, self.free_memory, &[b" bytes free"]);
        out
    }
}

/// The volume `CHKDSK` checks, as its state records it.
pub(crate) fn volume(p: &mut Program<'_, '_>) -> Volume {
    let max = p.get16(at::MAX_CLUSTER);
    Volume {
        kind: crate::dos::fs::kind_of(max),
        max,
    }
}

/// The FAT and map `CHKDSK` holds.
pub(crate) fn held<'p, 'a, 'b>(p: &'p mut Program<'a, 'b>) -> Held<'p, 'a, 'b> {
    let (segment, map) = (p.get16(at::FAT), p.get16(at::MAP));
    let fat = HeldFat {
        segment,
        kind: volume(p).kind,
    };
    Held { p, fat, map }
}

pub(crate) fn has(p: &mut Program<'_, '_>, bit: u8) -> bool {
    p.get8(at::FLAGS) & bit != 0
}

pub(crate) fn set(p: &mut Program<'_, '_>, bit: u8) {
    let flags = p.get8(at::FLAGS);
    p.set8(at::FLAGS, flags | bit);
}

pub(crate) fn switched(p: &mut Program<'_, '_>, bit: u8) -> bool {
    p.get8(at::SWITCHES) & bit != 0
}

/// Whether a fix may reach the disk: `/F` given, and the whole tree walked.
pub(crate) fn fixing(p: &mut Program<'_, '_>) -> bool {
    switched(p, switch::FIX) && !has(p, flag::TRUNCATED)
}

pub(crate) fn counted(p: &mut Program<'_, '_>, which: u16) -> u32 {
    p.get32(at::COUNTS + which * 4)
}

pub(crate) fn add(p: &mut Program<'_, '_>, which: u16, n: u32) {
    let was = counted(p, which);
    p.set32(at::COUNTS + which * 4, was.saturating_add(n));
}

pub(crate) fn set_count(p: &mut Program<'_, '_>, which: u16, n: u32) {
    p.set32(at::COUNTS + which * 4, n);
}

/// Bytes of a cluster.
pub(crate) fn cluster_bytes(p: &mut Program<'_, '_>) -> u32 {
    u32::from(p.get16(at::SECTOR_BYTES)) * u32::from(p.get8(at::CLUSTER_SECTORS))
}

/// INT 25h, or with `write` INT 26h, on the drive checked; a write makes
/// the sector buffered no longer the disk's.
pub(crate) fn absolute(
    p: &mut Program<'_, '_>,
    write: bool,
    first: u32,
    count: u16,
    buffer: Far,
) -> Frame {
    if write {
        p.set32(at::BUFFERED, u32::MAX);
    }
    let drive = p.get8(at::DRIVE);
    super::absolute(
        p,
        at::PACKET,
        write,
        Sectors {
            drive,
            first,
            count,
            buffer,
        },
    )
}

/// The call making `text` the start of the path a message names: `X:\`
/// and the current directory, read there.
pub(crate) fn current_directory(p: &mut Program<'_, '_>) -> Frame {
    let drive = p.get8(at::DRIVE);
    p.g.write_bytes(p.own, at::PATH, &[letter(drive), b':', b'\\', 0]);
    Frame::of(&Int {
        dx: u16::from(drive) + 1,
        si: at::PATH + 3,
        ..Int::dos(p.own, 0x4700)
    })
}

/// The path [`current_directory`] read, `name` after it.
pub(crate) fn path_with(p: &mut Program<'_, '_>, name: &[u8]) -> Vec<u8> {
    let mut path = asciz_at(p, at::PATH, at::PATH_LEN);
    if !name.is_empty() {
        if path.last() != Some(&b'\\') {
            path.push(b'\\');
        }
        path.extend_from_slice(name);
    }
    path
}

/// An entry's name as DOS spells it from its field.
pub(crate) fn spelt(field: &[u8; 11]) -> Vec<u8> {
    let mut spelt = [0u8; 13];
    let len = crate::dos::name::spell(field, &mut spelt);
    spelt[..len].to_vec()
}

/// Keeps the time function `2Ch` just gave in [`at::STAMP`], for [`now`].
pub(crate) fn keep_clock(p: &mut Program<'_, '_>) {
    let clock = u32::from(p.result(Returned::Cx)) << 16 | u32::from(p.result(Returned::Dx));
    p.set32(at::STAMP, clock);
}

/// The stamp an entry made now takes, the date in the high word: the date
/// function `2Ah` just gave in `cx` and `dx`, the time `2Ch` gave before it,
/// held in [`at::STAMP`]; `0` for a reading an entry cannot hold.
pub(crate) fn now(p: &mut Program<'_, '_>, cx: u16, dx: u16) -> u32 {
    let clock = p.get32(at::STAMP);
    let [day, month] = dx.to_le_bytes();
    let civil = tairix_abi::time::CivilTime {
        year: i64::from(cx),
        month: u32::from(month),
        day: u32::from(day),
        hour: clock >> 24,
        minute: clock >> 16 & 0xFF,
        second: clock >> 8 & 0xFF,
    };
    DosTime::of_civil(&civil).map_or(0, |stamp| {
        u32::from(stamp.date) << 16 | u32::from(stamp.time)
    })
}

/// A dword the report gives, the free memory and the total among them.
fn memory_bytes(paragraphs: u32) -> u32 {
    paragraphs.saturating_mul(16)
}

locals! {
    /// A question answered on a line: `1` for yes, `0` for no. An answer
    /// that is neither is asked again; an input that has run out answers
    /// nothing and ends `CHKDSK` as a break would.
    pub struct Ask {
        question: u8,
        ended: bool,
    }
}

impl Ask {
    #[must_use]
    pub const fn of(question: u8) -> Self {
        Self {
            question,
            ended: false,
        }
    }
}

mod asking {
    pub const ASKED: u8 = 1;
    pub const TYPED: u8 = 2;
    pub const SAID: u8 = 3;
    pub const ANSWERED: u8 = 4;
}

impl Routine for Ask {
    const ID: Id = Id::ChkdskAsk;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            asking::ASKED => {
                take!(child);
                p.set8(at::LINE, at::LINE_ROOM);
                *phase = asking::TYPED;
                Flow::Call(Frame::of(&Line::at(at::LINE)))
            }
            asking::TYPED => {
                self.ended = take!(child) == ENDED;
                *phase = asking::SAID;
                Flow::Call(say(p, text::NEW_LINE))
            }
            asking::SAID => {
                take!(child);
                if self.ended {
                    return Flow::Return(Err(Fault::Break));
                }
                if p.get8(at::LINE + 1) == 0 {
                    return self.ask(p, phase);
                }
                let typed = p.get8(at::LINE + 2);
                *phase = asking::ANSWERED;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(typed),
                    ..Int::dos(p.own, 0x6523)
                }))
            }
            asking::ANSWERED => match child {
                Ok(given @ (answer::YES | answer::NO)) => Flow::Return(Ok(given)),
                _ => self.ask(p, phase),
            },
            _ => self.ask(p, phase),
        }
    }
}

impl Ask {
    fn ask(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (words, handle) = QUESTIONS[usize::from(self.question).min(QUESTIONS.len() - 1)];
        *phase = asking::ASKED;
        Flow::Call(if handle == 2 {
            say_error(p, words)
        } else {
            say(p, words)
        })
    }
}

locals! {
    /// `CHKDSK`'s main routine.
    pub struct Chkdsk {}
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const RESET: u8 = 2;
    pub const CURRENT: u8 = 3;
    pub const ATTRIBUTES: u8 = 4;
    pub const PARAMETERS: u8 = 5;
    pub const BOOT: u8 = 6;
    pub const SPACE: u8 = 7;
    pub const TRUENAME: u8 = 8;
    pub const DPB: u8 = 9;
    pub const BREAK_SET: u8 = 10;
    pub const OLD_CRITICAL: u8 = 11;
    pub const CRITICAL_SET: u8 = 12;
    pub const DRIVE_SET: u8 = 13;
    pub const USER_DIR: u8 = 14;
    pub const AT_ROOT: u8 = 15;
    pub const SHRUNK: u8 = 16;
    pub const ALLOCATED: u8 = 17;
    pub const DTA_SET: u8 = 18;
    pub const LABEL: u8 = 19;
    pub const COUNTRY: u8 = 20;
    pub const LABEL_SAID: u8 = 21;
    pub const MEDIA_ID: u8 = 22;
    pub const SERIAL_SAID: u8 = 23;
    pub const FAT_READ: u8 = 24;
    pub const NON_DOS: u8 = 25;
    pub const WALKED: u8 = 26;
    pub const LOST: u8 = 27;
    pub const SECOND: u8 = 28;
    pub const SECOND_WALKED: u8 = 29;
    pub const REPORT_LINE: u8 = 30;
    pub const EXTENDED_BIOS: u8 = 31;
    pub const REPORTED: u8 = 32;
    pub const FAT_WRITTEN: u8 = 33;
    pub const WRITE_SAID: u8 = 34;
    pub const RESET_AGAIN: u8 = 35;
    pub const BACK: u8 = 36;
    pub const COUNTED: u8 = 37;
    pub const DONE: u8 = 38;
    pub const SAID_ENDING: u8 = 39;
}

impl Chkdsk {
    /// Ends after saying `words` on the standard error: nothing was changed.
    fn stop(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        *phase = phase::REFUSED;
        Flow::Call(say_error(p, words))
    }

    /// `CHKDSK` cannot go on: `words` said on the standard error, it ends
    /// with its bad code, putting back the user's directory and drive.
    fn fatal(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        p.set8(at::CODE, BAD_EXIT);
        *phase = phase::SAID_ENDING;
        Flow::Call(say_error(p, words))
    }

    /// The user's directory and drive put back, the file named counted
    /// first when the FAT was read.
    fn finish(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::RESET_AGAIN;
        Flow::Call(Frame::of(&Int::dos(p.own, 0x0D00)))
    }

    /// Function `ax` of the drive checked, named from `1` in BL and DL.
    fn drive_call(p: &mut Program<'_, '_>, ax: u16) -> Frame {
        let drive = u16::from(p.get8(at::DRIVE)) + 1;
        Frame::of(&Int {
            bx: drive,
            dx: drive,
            ..Int::dos(p.own, ax)
        })
    }

    /// Takes the DPB function `32h` found, refusing one whose volume
    /// `CHKDSK` cannot hold or that does not hold together.
    fn take_dpb(p: &mut Program<'_, '_>) -> bool {
        let (segment, at) = (p.result(Returned::Ds), p.result(Returned::Bx));
        let byte = |p: &mut Program<'_, '_>, field: u16| p.g.read8(segment, at.wrapping_add(field));
        let word =
            |p: &mut Program<'_, '_>, field: u16| p.g.read16(segment, at.wrapping_add(field));
        let sector = word(p, dpb::SECTOR);
        let cluster = u16::from(byte(p, dpb::CLUSTER_MASK)) + 1;
        let max = word(p, dpb::MAX_CLUSTER);
        let fat_sectors = word(p, dpb::FAT_SECTORS);
        let fats = byte(p, dpb::FATS);
        let kind = crate::dos::fs::kind_of(max);
        let (end, width) = tairix_fat::table::span(kind, u32::from(max));
        let fat_bytes = u64::from(fat_sectors) * u64::from(sector);
        let shaped = matches!(sector, 128 | 256 | 512)
            && cluster.is_power_of_two()
            && cluster <= 128
            && (2..=u16::try_from(tairix_fat::boot::FAT32_CLUSTERS).unwrap_or(u16::MAX))
                .contains(&max)
            && (1..=2).contains(&fats)
            && end + width as u64 <= fat_bytes;
        if !shaped {
            return false;
        }
        p.set16(at::SECTOR_BYTES, sector);
        p.set8(at::CLUSTER_SECTORS, u8::try_from(cluster).unwrap_or(1));
        p.set16(at::MAX_CLUSTER, max);
        p.set16(at::FAT_SECTORS, fat_sectors);
        p.set8(at::FATS, fats);
        for (to, field) in [
            (at::FIRST_FAT, dpb::RESERVED),
            (at::FIRST_DATA, dpb::DATA),
            (at::ROOT_SECTOR, dpb::ROOT),
            (at::ROOT_ENTRIES, dpb::ROOT_ENTRIES),
        ] {
            let value = word(p, field);
            p.set16(to, value);
        }
        true
    }

    /// The paragraphs the block takes: the walk's levels, then the FAT,
    /// then the map, each from a paragraph.
    fn layout(p: &mut Program<'_, '_>) -> (u16, u16, u16) {
        let levels = (u32::from(LEVELS) * u32::from(level::LEN)).div_ceil(16);
        let fat = (u32::from(p.get16(at::FAT_SECTORS)) * u32::from(p.get16(at::SECTOR_BYTES)))
            .div_ceil(16);
        let map = (u32::from(p.get16(at::MAX_CLUSTER)) + 1).div_ceil(16);
        let to = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
        (to(levels), to(fat), to(levels + fat + map))
    }

    /// The label's line: its name, and when it was made, as the country
    /// writes it, from the search's entry.
    fn label_line(p: &mut Program<'_, '_>) -> Vec<u8> {
        let entry = at::DTA + 8;
        let mut name = [0u8; 11];
        p.g.read_bytes(p.own, entry, &mut name);
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        let stamp = DosTime {
            date: p.get16(entry + 0x18),
            time: p.get16(entry + 0x16),
        }
        .civil();
        let field = |value: u32| u8::try_from(value).unwrap_or(0);
        let writing = Writing::read(p, at::INFO);
        let date = writing.date(
            u16::try_from(stamp.year).unwrap_or(0),
            field(stamp.month),
            field(stamp.day),
            false,
        );
        let (hour, meridian) = writing.hour(field(stamp.hour));
        let mut line = [&b"\r\nVolume "[..], name, b" created ", &date, b" "].concat();
        line.extend_from_slice(&decimal(u32::from(hour)));
        line.push(writing.time_separator);
        line.extend_from_slice(&two_digits(u16::from(field(stamp.minute))));
        line.extend(meridian);
        line.extend_from_slice(text::NEW_LINE);
        line
    }

    /// The next piece of a FAT copy read or written: as many sectors as a
    /// segment holds, into the FAT's place.
    fn fat_piece(p: &mut Program<'_, '_>, write: bool) -> Frame {
        let (sector, sectors) = (p.get16(at::SECTOR_BYTES), p.get16(at::FAT_SECTORS));
        let moved = p.get16(at::MOVED);
        let piece = (sectors - moved).min(0xFFFF / sector);
        let copy = u32::from(p.get8(at::COPY));
        let first =
            u32::from(p.get16(at::FIRST_FAT)) + copy * u32::from(sectors) + u32::from(moved);
        let paragraphs = u16::try_from(u32::from(moved) * u32::from(sector) / 16).unwrap_or(0);
        let buffer = Far::new(p.get16(at::FAT).wrapping_add(paragraphs), 0);
        absolute(p, write, first, piece, buffer)
    }

    /// The FAT is in: its ID checked, then the tree walked.
    fn fat_in(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        set(p, flag::FAT_READ);
        let fat = p.get16(at::FAT);
        let id = p.g.read8(fat, 0);
        if id < 0xF8 && id != 0xF0 {
            *phase = phase::NON_DOS;
            return Flow::Call(Frame::of(&Ask::of(question::NON_DOS)));
        }
        Self::walk(p, phase)
    }

    /// The map cleared, the first walk of the tree.
    fn walk(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let map = p.get16(at::MAP);
        let zeros = alloc::vec![0u8; usize::from(p.get16(at::MAX_CLUSTER)) + 1];
        p.g.write_bytes(map, 0, &zeros);
        *phase = phase::WALKED;
        Flow::Call(Frame::of(&ChkdskTree::default()))
    }

    /// The report written, after the lines it begins with.
    fn report(p: &mut Program<'_, '_>, phase: &mut u8, ebda_kib: u16) -> Flow {
        let top = u32::from(p.get16(at::TOP));
        let counts: [u32; count::LEN as usize] =
            core::array::from_fn(|n| counted(p, u16::try_from(n).unwrap_or(0)));
        let totals = Totals {
            disk_bytes: p.get32(at::DISK_BYTES),
            cluster_bytes: cluster_bytes(p),
            clusters: u32::from(p.get16(at::MAX_CLUSTER).saturating_sub(1)),
            counts,
            recovered: has(p, flag::RECOVERED),
            memory: memory_bytes(top + u32::from(ebda_kib) * 64),
            free_memory: memory_bytes(top.saturating_sub(u32::from(p.own))),
        };
        let report = totals.report();
        let len = report.len().min(usize::from(at::REPORT_LEN));
        p.g.write_bytes(p.own, at::REPORT, &report[..len]);
        *phase = phase::REPORTED;
        Flow::Call(Frame::of(&Int {
            bx: 1,
            cx: u16::try_from(len).unwrap_or(0),
            dx: at::REPORT,
            ..Int::dos(p.own, 0x4000)
        }))
    }

    /// The FAT written back, a copy at a time, if it changed and `/F` was
    /// given; else the user's directory and drive put back.
    fn write_fat(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if !(has(p, flag::DIRTY) && switched(p, switch::FIX)) {
            return Self::finish(p, phase);
        }
        p.set8(at::COPY, 0);
        p.set8(at::FAILED_COPIES, 0);
        p.set16(at::MOVED, 0);
        *phase = phase::FAT_WRITTEN;
        Flow::Call(Self::fat_piece(p, true))
    }

    /// On to the next FAT copy to write, or done with them.
    fn next_copy(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let copy = p.get8(at::COPY) + 1;
        if copy < p.get8(at::FATS) {
            p.set8(at::COPY, copy);
            p.set16(at::MOVED, 0);
            *phase = phase::FAT_WRITTEN;
            return Flow::Call(Self::fat_piece(p, true));
        }
        if p.get8(at::FAILED_COPIES) == p.get8(at::FATS) {
            return Self::fatal(p, phase, &[text::CANNOT_GO_ON, b".\r\n"].concat());
        }
        Self::finish(p, phase)
    }

    /// Broken into: nothing more is written, the user's directory and drive
    /// are put back, and it ends with its bad code.
    fn broken(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        p.set8(at::CODE, BAD_EXIT);
        let flags = p.get8(at::FLAGS);
        p.set8(at::FLAGS, flags & !flag::FAT_READ);
        Self::finish(p, phase)
    }
}

impl Routine for Chkdsk {
    const ID: Id = Id::Chkdsk;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's CHKDSK runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) {
            if *phase < phase::DRIVE_SET {
                return Flow::Return(Ok(u32::from(BAD_EXIT)));
            }
            if *phase < phase::RESET_AGAIN {
                return Self::broken(p, phase);
            }
        }
        match *phase {
            0 => {
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(refusal) => {
                        *phase = phase::REFUSED;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                p.set8(at::SWITCHES, request.switches);
                p.set8(at::DRIVE, request.drive.unwrap_or(CURRENT));
                set_asciz(p, at::SPEC, request.spec.unwrap_or_default(), at::SPEC_LEN);
                let top = p.g.read16(p.own, crate::dos::memory::psp::TOP);
                p.set16(at::TOP, top);
                p.set8(at::FLAGS, 0);
                p.set8(at::CODE, 0);
                for which in 0..count::LEN {
                    set_count(p, which, 0);
                }
                p.g.write_bytes(p.own, at::ROOT, b"\\\0..\0");
                p.g.write_bytes(p.own, at::ORPHAN, b"FILE0000CHK");
                p.set32(at::BUFFERED, u32::MAX);
                *phase = phase::RESET;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0D00)))
            }
            phase::RESET => {
                *phase = phase::CURRENT;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
            phase::CURRENT => {
                let current = take!(child).to_le_bytes()[0];
                p.set8(at::USER_DRIVE, current);
                if p.get8(at::DRIVE) == CURRENT {
                    p.set8(at::DRIVE, current);
                }
                *phase = phase::ATTRIBUTES;
                Flow::Call(Self::drive_call(p, 0x4409))
            }
            phase::ATTRIBUTES => {
                if child.is_err() {
                    return Self::stop(p, phase, text::BAD_DRIVE);
                }
                let attributes = p.result(Returned::Dx);
                if attributes & drive_bits::NETWORK != 0 {
                    return Self::stop(p, phase, text::NETWORK);
                }
                if attributes & drive_bits::SUBST != 0 {
                    return Self::stop(p, phase, text::SUBST);
                }
                p.set8(at::PARAMS + parameters::SPECIAL, parameters::MEDIUM_BPB);
                *phase = phase::PARAMETERS;
                let drive = p.get8(at::DRIVE);
                Flow::Call(disk_ioctl(p, drive, generic::GET_PARAMETERS, at::PARAMS))
            }
            phase::PARAMETERS => {
                if child.is_err() {
                    *phase = phase::BOOT;
                    return Flow::Call(absolute(p, false, 0, 1, Far::new(p.own, at::BUFFER)));
                }
                let mut bpb = [0u8; 13];
                p.g.read_bytes(p.own, at::PARAMS + parameters::BPB, &mut bpb);
                if !checkable(&bpb) {
                    return Self::stop(p, phase, text::MEDIA);
                }
                *phase = phase::SPACE;
                Flow::Call(Self::drive_call(p, 0x3600))
            }
            phase::BOOT => {
                if failed(p) {
                    return Self::stop(p, phase, text::BAD_DRIVE);
                }
                let mut boot = [0u8; 0x18];
                p.g.read_bytes(p.own, at::BUFFER, &mut boot);
                if &boot[boot_at::OEM..boot_at::BYTES_PER_SECTOR] != RAMDRIVE
                    || !checkable(&boot[boot_at::BYTES_PER_SECTOR..])
                {
                    return Self::stop(p, phase, text::MEDIA);
                }
                *phase = phase::SPACE;
                Flow::Call(Self::drive_call(p, 0x3600))
            }
            phase::SPACE => {
                let per_cluster = p.result(Returned::Ax);
                if per_cluster == 0xFFFF {
                    return Self::stop(p, phase, text::BAD_DRIVE);
                }
                let (sector, clusters) = (p.result(Returned::Cx), p.result(Returned::Dx));
                let bytes = u32::from(clusters)
                    .saturating_mul(u32::from(per_cluster))
                    .saturating_mul(u32::from(sector));
                p.set32(at::DISK_BYTES, bytes);
                let drive = p.get8(at::DRIVE);
                set_asciz(p, at::NAME, &[letter(drive), b':', b'\\'], 4);
                *phase = phase::TRUENAME;
                Flow::Call(Frame::of(&Int {
                    si: at::NAME,
                    di: at::BUFFER,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            phase::TRUENAME => {
                if child.is_err() {
                    return Self::stop(p, phase, text::BAD_DRIVE);
                }
                if p.get8(at::BUFFER) != p.get8(at::NAME) {
                    return Self::stop(p, phase, text::SUBST);
                }
                *phase = phase::DPB;
                Flow::Call(Self::drive_call(p, 0x3200))
            }
            phase::DPB => {
                if take!(child).to_le_bytes()[0] == 0xFF {
                    p.set8(at::CODE, BAD_EXIT);
                    return Self::stop(p, phase, text::BAD_DRIVE);
                }
                if !Self::take_dpb(p) {
                    return Self::stop(p, phase, text::MEDIA);
                }
                *phase = phase::BREAK_SET;
                Flow::Call(Frame::of(&Int {
                    dx: handler_entry(handler::BREAK),
                    ..Int::dos(p.own, 0x2523)
                }))
            }
            phase::BREAK_SET => {
                take!(child);
                *phase = phase::OLD_CRITICAL;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x3524)))
            }
            phase::OLD_CRITICAL => {
                take!(child);
                let old = Far::new(p.result(Returned::Es), p.result(Returned::Bx));
                p.set_far(at::OLD_24, old);
                *phase = phase::CRITICAL_SET;
                Flow::Call(Frame::of(&Int {
                    dx: handler_entry(handler::CRITICAL),
                    ..Int::dos(p.own, 0x2524)
                }))
            }
            phase::CRITICAL_SET => {
                take!(child);
                *phase = phase::DRIVE_SET;
                let drive = p.get8(at::DRIVE);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(drive),
                    ..Int::dos(p.own, 0x0E00)
                }))
            }
            phase::DRIVE_SET => {
                take!(child);
                p.g.write_bytes(p.own, at::USER_DIR, b"\\\0");
                *phase = phase::USER_DIR;
                let drive = p.get8(at::DRIVE);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(drive) + 1,
                    si: at::USER_DIR + 1,
                    ..Int::dos(p.own, 0x4700)
                }))
            }
            phase::USER_DIR => {
                if child.is_err() {
                    p.g.write_bytes(p.own, at::USER_DIR, b"\\\0");
                }
                *phase = phase::AT_ROOT;
                Flow::Call(Frame::of(&Int {
                    dx: at::ROOT,
                    ..Int::dos(p.own, 0x3B00)
                }))
            }
            phase::AT_ROOT => {
                if child.is_err() {
                    return Self::fatal(p, phase, text::NO_ROOT);
                }
                *phase = phase::SHRUNK;
                Flow::Call(p.shrink())
            }
            phase::SHRUNK => {
                take!(child);
                let (_, _, paragraphs) = Self::layout(p);
                *phase = phase::ALLOCATED;
                Flow::Call(Frame::of(&Int {
                    bx: paragraphs,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::ALLOCATED => {
                let Ok(block) = child else {
                    *phase = phase::SAID_ENDING;
                    return Flow::Call(say_error(p, text::MEMORY));
                };
                let block = u16::try_from(block).unwrap_or(0);
                let (levels, fat, _) = Self::layout(p);
                p.set16(at::BLOCK, block);
                p.set16(at::FAT, block.wrapping_add(levels));
                p.set16(at::MAP, block.wrapping_add(levels).wrapping_add(fat));
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::DTA_SET => {
                take!(child);
                let mut label = [0u8; 44];
                label[0] = 0xFF;
                label[6] = tairix_fat::dir::attr::VOLUME_ID;
                label[7] = p.get8(at::DRIVE) + 1;
                label[8..19].fill(b'?');
                p.g.write_bytes(p.own, at::FCB, &label);
                *phase = phase::LABEL;
                Flow::Call(Frame::of(&Int {
                    dx: at::FCB,
                    ..Int::dos(p.own, 0x1100)
                }))
            }
            phase::LABEL => {
                if take!(child).to_le_bytes()[0] != 0 {
                    *phase = phase::LABEL_SAID;
                    return self.step(p, phase, Ok(0));
                }
                *phase = phase::COUNTRY;
                Flow::Call(Frame::of(&Int {
                    dx: at::INFO,
                    ..Int::dos(p.own, 0x3800)
                }))
            }
            phase::COUNTRY => {
                if child.is_err() {
                    let us = crate::dos::country::COUNTRIES[0].info();
                    p.g.write_bytes(p.own, at::INFO, &us);
                }
                let line = Self::label_line(p);
                *phase = phase::LABEL_SAID;
                Flow::Call(say(p, &line))
            }
            phase::LABEL_SAID => {
                take!(child);
                *phase = phase::MEDIA_ID;
                let drive = p.get8(at::DRIVE);
                p.set16(at::MEDIA, 0);
                Flow::Call(disk_ioctl(p, drive, generic::GET_MEDIA_ID, at::MEDIA))
            }
            phase::MEDIA_ID => {
                p.set8(at::COPY, 0);
                p.set16(at::MOVED, 0);
                if child.is_err() {
                    *phase = phase::FAT_READ;
                    return Flow::Call(Self::fat_piece(p, false));
                }
                let serial = p.get32(at::MEDIA + 2);
                let line = message::serial_line(serial);
                *phase = phase::SERIAL_SAID;
                Flow::Call(say(p, &line))
            }
            phase::SERIAL_SAID => {
                take!(child);
                *phase = phase::FAT_READ;
                Flow::Call(Self::fat_piece(p, false))
            }
            phase::FAT_READ => {
                if failed(p) {
                    let copy = p.get8(at::COPY) + 1;
                    if copy < p.get8(at::FATS) {
                        p.set8(at::COPY, copy);
                        p.set16(at::MOVED, 0);
                        return Flow::Call(Self::fat_piece(p, false));
                    }
                    let drive = p.get8(at::DRIVE);
                    let words = [
                        text::CANNOT_GO_ON,
                        text::FAT_BAD,
                        b" ",
                        &[letter(drive)],
                        b".\r\n",
                    ]
                    .concat();
                    return Self::fatal(p, phase, &words);
                }
                let moved = p.get16(at::MOVED) + p.get16(at::PACKET + 4);
                p.set16(at::MOVED, moved);
                if moved < p.get16(at::FAT_SECTORS) {
                    return Flow::Call(Self::fat_piece(p, false));
                }
                Self::fat_in(p, phase)
            }
            phase::NON_DOS => {
                if take!(child) == 0 {
                    let flags = p.get8(at::FLAGS);
                    p.set8(at::FLAGS, flags & !flag::FAT_READ);
                    return Self::finish(p, phase);
                }
                Self::walk(p, phase)
            }
            phase::WALKED => {
                take!(child);
                let volume = volume(p);
                let (bad, lost) = cells::unreached(&mut held(p), volume);
                set_count(p, count::BAD, bad);
                set_count(p, count::LOST, lost);
                *phase = phase::LOST;
                if lost > 0 {
                    return Flow::Call(Frame::of(&ChkdskLost::default()));
                }
                self.step(p, phase, Ok(0))
            }
            phase::LOST => {
                take!(child);
                if counted(p, count::CROSSINGS) == 0 {
                    *phase = phase::REPORT_LINE;
                    return Flow::Call(say(p, text::NEW_LINE));
                }
                set(p, flag::SECOND_PASS);
                *phase = phase::SECOND;
                Flow::Call(say(p, text::NEW_LINE))
            }
            phase::SECOND => {
                take!(child);
                *phase = phase::SECOND_WALKED;
                Flow::Call(Frame::of(&ChkdskTree::default()))
            }
            phase::SECOND_WALKED => {
                take!(child);
                *phase = phase::REPORT_LINE;
                Flow::Call(say(p, text::NEW_LINE))
            }
            phase::REPORT_LINE => {
                take!(child);
                *phase = phase::EXTENDED_BIOS;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x15, 0xC100)))
            }
            phase::EXTENDED_BIOS => {
                let ebda = if child.is_ok() && !failed(p) {
                    let ebda = p.result(Returned::Es);
                    u16::from(p.g.read8(ebda, 0))
                } else {
                    0
                };
                Self::report(p, phase, ebda)
            }
            phase::REPORTED => {
                take!(child);
                Self::write_fat(p, phase)
            }
            phase::FAT_WRITTEN => {
                if failed(p) {
                    let failures = p.get8(at::FAILED_COPIES) + 1;
                    p.set8(at::FAILED_COPIES, failures);
                    let copy = p.get8(at::COPY) + 1;
                    let words =
                        [text::WRITING_FAT, &decimal(u32::from(copy)), text::NEW_LINE].concat();
                    *phase = phase::WRITE_SAID;
                    return Flow::Call(say_error(p, &words));
                }
                let moved = p.get16(at::MOVED) + p.get16(at::PACKET + 4);
                p.set16(at::MOVED, moved);
                if moved < p.get16(at::FAT_SECTORS) {
                    return Flow::Call(Self::fat_piece(p, true));
                }
                Self::next_copy(p, phase)
            }
            phase::WRITE_SAID => {
                take!(child);
                Self::next_copy(p, phase)
            }
            phase::RESET_AGAIN => {
                *phase = phase::BACK;
                Flow::Call(Frame::of(&Int {
                    dx: at::USER_DIR,
                    ..Int::dos(p.own, 0x3B00)
                }))
            }
            phase::BACK => {
                *phase = phase::COUNTED;
                if switched(p, switch::NAMED) && has(p, flag::FAT_READ) {
                    return Flow::Call(Frame::of(&ChkdskExtents::default()));
                }
                self.step(p, phase, Ok(0))
            }
            phase::COUNTED => {
                *phase = phase::DONE;
                let user = p.get8(at::USER_DRIVE);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(user),
                    ..Int::dos(p.own, 0x0E00)
                }))
            }
            phase::SAID_ENDING => Self::finish(p, phase),
            _ => Flow::Return(Ok(u32::from(p.get8(at::CODE)))),
        }
    }
}

locals! {
    /// INT 24h while `CHKDSK` runs: a disk's error fails the call at once,
    /// but a write-protected disk or an empty drive's, which go to the
    /// handler before it; aborted there, the user's drive is put back.
    pub struct ChkdskCritical {}
}

mod critical_phase {
    pub const ASKED: u8 = 1;
    pub const PUT_BACK: u8 = 2;
}

impl Routine for ChkdskCritical {
    const ID: Id = Id::ChkdskCritical;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let ax = p.caller(Saved::Ax);
        match *phase {
            0 => {
                let flags = ax.to_le_bytes()[1];
                let error = p.caller(Saved::Di).to_le_bytes()[0];
                let disk = flags & critical::NOT_DISK == 0;
                if disk
                    && !matches!(
                        error,
                        crate::dos::device::error::WRITE_PROTECT
                            | crate::dos::device::error::NOT_READY
                    )
                {
                    p.set_caller(
                        Saved::Ax,
                        u16::from_le_bytes([critical::ACTION_FAIL, flags]),
                    );
                    return Flow::Return(Ok(0));
                }
                *phase = critical_phase::ASKED;
                let old = p.get_far(at::OLD_24);
                Flow::Call(Frame::of(&Int::passed_on(p, 0x24, old, ax)))
            }
            critical_phase::ASKED => {
                take!(child);
                let answer = p.result(Returned::Ax).to_le_bytes()[0];
                p.set_caller(Saved::Ax, u16::from_le_bytes([answer, ax.to_le_bytes()[1]]));
                if answer != critical::ACTION_ABORT {
                    return Flow::Return(Ok(0));
                }
                *phase = critical_phase::PUT_BACK;
                let user = p.get8(at::USER_DRIVE);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(user),
                    ..Int::dos(p.own, 0x0E00)
                }))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

#[cfg(test)]
#[path = "chkdsk_tests.rs"]
mod tests;
