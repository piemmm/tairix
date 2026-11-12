//! `FORMAT d: [/S] [/V[:label]] [/B] [/1] [/4] [/8] [/T:tracks /N:sectors]
//! [/F:size]`: a disk laid out afresh as DOS 4.0's `FORMAT` lays it, with its
//! checks, prompts, messages, report and exit codes. Each track is formatted
//! and verified through the drive's generic IOCTL, a bad one's clusters
//! marked, then the boot record, FATs and root are written through INT 26h.
//!
//! `/S` places TAIRiX DOS's system — the boot record that finds the kernel
//! in the ROM, and `COMMAND.COM`, read before the disk is asked for so it may
//! come from the drive being formatted. `/B` is taken and reserves nothing,
//! as `SYS` places the system anywhere. Where DOS 4.0's arithmetic and its
//! readers disagree — the FAT kind at 4,085 clusters, a FAT12-sized table for
//! a custom format of more clusters — the volume gets what its readers take.

mod disk;
mod tracks;
mod write;

pub use disk::{FormatAsk, FormatDisk, FormatLabel};
pub use tracks::{FormatBad, FormatTracks};
pub use write::{FormatSource, FormatSystem, FormatWrite};

use super::label;
use super::{disk_ioctl, refuse, switch_parts, value_in, Refusal};
use crate::dos::device::generic::parameters::kind;
use crate::dos::device::generic::{self, parameters};
use crate::dos::device::{driver_code, failed};
use crate::dos::formats::{bpb_at, dword_at, partition, total_of, word_at, Diskette};
use crate::dos::ioctl::drive_bits;
use crate::dos::program::{
    handler_entry, say, say_error, Flow, Frame, Id, Int, Program, Returned, Routine,
};
use crate::dos::shell::handlers::device_words;
use crate::dos::shell::{letter, parse};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::boot::{Bpb, Kind, DOS_BPB};

/// Where `FORMAT`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The drive formatted, from 0; the code it ends with; [`super::flag`].
    pub const DRIVE: u16 = STATE;
    pub const STATUS: u16 = STATE + 0x01;
    pub const FLAGS: u16 = STATE + 0x02;
    /// The switches, as the checks leave them, and the original ones each
    /// disk starts from.
    pub const SWITCHES: u16 = STATE + 0x04;
    pub const ASKED: u16 = STATE + 0x06;
    /// `/T` and `/N`'s values, and `/F`'s size as its place in [`super::SIZES`].
    pub const TRACKS: u16 = STATE + 0x08;
    pub const SECTORS: u16 = STATE + 0x0A;
    pub const SIZE: u16 = STATE + 0x0C;
    /// The label given or typed, as a directory entry holds it.
    pub const LABEL: u16 = STATE + 0x10;
    /// The volume's serial number.
    pub const SERIAL: u16 = STATE + 0x1C;
    /// Where the data area starts, the volume's highest cluster, and its FAT.
    pub const START: u16 = STATE + 0x20;
    pub const MAX_CLUSTER: u16 = STATE + 0x24;
    pub const FAT: u16 = STATE + 0x26;
    pub const FAT_PARAGRAPHS: u16 = STATE + 0x28;
    /// The root directory as it is written, and a cluster's buffer for the
    /// sectors of a bad track of a fixed disk read again.
    pub const ROOT: u16 = STATE + 0x2A;
    pub const CLUSTER: u16 = STATE + 0x2C;
    /// The track the format has reached, the tracks left from it, and the
    /// disk's: the first, their count, those done, and the share shown.
    pub const CYLINDER: u16 = STATE + 0x30;
    pub const HEAD: u16 = STATE + 0x32;
    pub const LEFT: u16 = STATE + 0x34;
    pub const FIRST_CYLINDER: u16 = STATE + 0x38;
    pub const FIRST_HEAD: u16 = STATE + 0x3A;
    pub const TRACK_COUNT: u16 = STATE + 0x3C;
    pub const DONE: u16 = STATE + 0x40;
    pub const PERCENT: u16 = STATE + 0x44;
    /// `COMMAND.COM`, read for `/S`: its block, size, stamp and path, and
    /// the bytes the system takes on the disk.
    pub const SYSTEM: u16 = STATE + 0x46;
    pub const SYSTEM_SIZE: u16 = STATE + 0x48;
    pub const SYSTEM_TIME: u16 = STATE + 0x4C;
    pub const SYSTEM_DATE: u16 = STATE + 0x4E;
    pub const SYSTEM_BYTES: u16 = STATE + 0x50;
    pub const SYSTEM_PATH: u16 = STATE + 0x54;
    pub const PATH_LEN: u16 = 0x50;
    /// The device parameters the disk is formatted with, and those the
    /// drive had, put back at the end.
    pub const PARAMS: u16 = STATE + 0xA4;
    pub const SAVED: u16 = PARAMS + super::parameters::LEN;
    /// A track request, INT 25h's and 26h's, and a line typed.
    pub const PACKET: u16 = SAVED + 0x28;
    pub const ABSOLUTE: u16 = PACKET + 0x10;
    pub const LINE: u16 = ABSOLUTE + 0x10;
    pub const LINE_ROOM: u8 = 80;
    /// A search's DTA, an FCB, and a sector.
    pub const DTA: u16 = LINE + 0x54;
    pub const FCB: u16 = DTA + 0x30;
    pub const BUFFER: u16 = FCB + 0x30;
    /// Bytes of `FORMAT`'s state.
    pub const LEN: u16 = BUFFER + 0x200 - STATE;
}

/// What the state's flags record.
pub mod flag {
    /// The drive's own parameters were saved, and are put back.
    pub const SAVED: u8 = 0x01;
    /// It is a fixed disk.
    pub const FIXED: u8 = 0x02;
    /// Its root directory's entries begin `E5h`, as DOS 1 reads them.
    pub const OLD_DIR: u8 = 0x04;
    /// The volume is being written: a break waits for the writing to end.
    pub const WRITING: u8 = 0x08;
    /// A single-sided format is being tried after the second side failed.
    pub const LAST_CHANCE: u8 = 0x10;
    /// A label was given with `/V`.
    pub const LABELLED: u8 = 0x20;
    /// The table is 16-bit.
    pub const BIG_FAT: u8 = 0x40;
}

/// The handlers `FORMAT` installs.
pub mod handler {
    pub const BREAK: u16 = 0;
}

/// The switches.
pub mod switch {
    pub const S: u16 = 0x0001;
    pub const ONE: u16 = 0x0002;
    pub const FOUR: u16 = 0x0004;
    pub const EIGHT: u16 = 0x0008;
    pub const B: u16 = 0x0010;
    pub const V: u16 = 0x0020;
    pub const T: u16 = 0x0040;
    pub const N: u16 = 0x0080;
    pub const F: u16 = 0x0100;
}

/// The codes `FORMAT` ends with.
pub mod exit {
    pub const OK: u8 = 0;
    pub const BREAK: u8 = 3;
    pub const FATAL: u8 = 4;
    pub const DECLINED: u8 = 5;
}

/// The sizes `/F` names, in [`Diskette`]'s order.
pub const SIZES: [Diskette; 7] = [
    Diskette::K160,
    Diskette::K180,
    Diskette::K320,
    Diskette::K360,
    Diskette::K720,
    Diskette::K1200,
    Diskette::K1440,
];

/// What `FORMAT` says, as DOS 4.0's does.
pub mod text {
    pub use crate::dos::message::{
        ASK_LABEL, BAD_DRIVE, BAD_PARAMETER, MEMORY, NEW_LINE, TRANSFERRED,
    };
    pub const INTERRUPTED: &[u8] = b"\r\n\n";
    pub const NETWORK: &[u8] = b"\rCannot FORMAT a network drive\r\n";
    pub const ASSIGNED: &[u8] = b"\rCannot format an ASSIGNed or SUBSTed drive. \r\n";
    pub const TWICE: &[u8] = b"\rSame parameter entered twice\r\n";
    pub const BAD_LABEL: &[u8] = b"Invalid Volume ID";
    pub const NOT_SUPPORTED: &[u8] = b"Format not supported on drive ";
    pub const DEVICE_PARAMETERS: &[u8] = b"\rInvalid device parameters from device driver\r\n";
    pub const INCOMPATIBLE: &[u8] = b"\rParameters not compatible\r\n";
    pub const FIXED_DISK: &[u8] = b"\rParameters not compatible\r\nwith fixed disk\r\n";
    pub const BY_DRIVE: &[u8] = b"\rParameters not supported by drive\r\n";
    pub const BOTH: &[u8] = b"\rMust enter both /T and /N parameters\r\n";
    pub const UNSUPPORTED: &[u8] = b"\rParameters not supported\r\n";
    pub const TERMINATED: &[u8] = b"Format terminated";
    pub const COMPLETE: &[u8] = b"Format complete";
    pub const PERCENT: &[u8] = b" percent of disk formatted";
    pub const RECOVER: &[u8] = b"Attempting to recover allocation unit ";
    pub const UNUSABLE: &[u8] = b"\rInvalid media or Track 0 bad - disk unusable\r\n";
    pub const NOT_SYSTEM: &[u8] = b"\rDisk unsuitable for system disk\r\n";
    pub const BOOT: &[u8] = b"Unable to write BOOT";
    pub const FAT: &[u8] = b"\rError writing FAT           \r\n";
    pub const DIRECTORY: &[u8] = b"\rError writing directory\r\n";
    pub const NO_SYSTEM: &[u8] = b"\rCannot find System Files\r\n";
    pub const INSERT: &[u8] = b"Insert new diskette for drive ";
    pub const INSERT_DOS: &[u8] = b"\rInsert DOS disk in drive ";
    pub const ENTER: &[u8] = b"\rand press ENTER when ready...";
    pub const CURRENT_LABEL: &[u8] = b"\rEnter current volume label for drive ";
    pub const WARNING: &[u8] = b"\r\nWARNING, ALL DATA ON NON-REMOVABLE DISK\r\nDRIVE ";
    pub const PROCEED: &[u8] = b": WILL BE LOST!\r\nProceed with Format (Y/N)?";
    pub const ANOTHER: &[u8] = b"\rFormat another (Y/N)?";
    pub const TABLE_READ: &[u8] = b"\rError reading partition table\r\n";
    pub const TABLE_WRITE: &[u8] = b"\rError writing partition table\r\n";
    pub const BAD_TABLE: &[u8] = b"\rBad Partition Table        \r\n";
}

/// `words` with spaces after them to `width`: a message long enough to
/// cover the progress line it is written over.
#[must_use]
pub fn padded(words: &[u8], width: usize) -> Vec<u8> {
    let mut out = words.to_vec();
    out.resize(width.max(words.len()), b' ');
    out
}

/// A message written over the progress line: a carriage return first, the
/// padding after.
#[must_use]
pub fn over_progress(words: &[u8], width: usize, end: &[u8]) -> Vec<u8> {
    [b"\r", &padded(words, width)[..], end].concat()
}

/// The width DOS 4.0's progress messages are padded to.
pub const PROGRESS_WIDTH: usize = 68;

/// What `FORMAT`'s command line names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub drive: u8,
    pub switches: u16,
    pub tracks: u16,
    pub sectors: u16,
    pub size: Option<Diskette>,
    pub label: Option<[u8; 11]>,
}

/// Why `FORMAT` cannot take its command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused<'a> {
    /// What DOS's parser says.
    Parse(Refusal<'a>),
    /// A switch given twice.
    Twice,
    /// A label `/V` gives that a label cannot be.
    BadLabel,
}

/// The size `/F`'s value names.
fn size_of(value: &[u8]) -> Option<Diskette> {
    let value = value.to_ascii_uppercase();
    let (number, unit) = match value.iter().position(|b| !b.is_ascii_digit() && *b != b'.') {
        Some(at) => value.split_at(at),
        None => (&value[..], &b""[..]),
    };
    let kib = matches!(unit, b"" | b"K" | b"KB");
    let mib = matches!(unit, b"" | b"M" | b"MB");
    match number {
        b"160" if kib => Some(Diskette::K160),
        b"180" if kib => Some(Diskette::K180),
        b"320" if kib => Some(Diskette::K320),
        b"360" if kib => Some(Diskette::K360),
        b"720" if kib => Some(Diskette::K720),
        b"1200" if kib => Some(Diskette::K1200),
        b"1.2" if mib => Some(Diskette::K1200),
        b"1440" if kib => Some(Diskette::K1440),
        b"1.44" if mib => Some(Diskette::K1440),
        _ => None,
    }
}

/// The field a label `/V` gives makes: a blank before it is refused, as a
/// typed one's are skipped.
fn label_given(text: &[u8]) -> Option<[u8; 11]> {
    if text.first().is_some_and(|b| matches!(b, b' ' | b'\t')) {
        return None;
    }
    let text = text
        .strip_prefix(b"\"")
        .and_then(|t| t.strip_suffix(b"\""))
        .unwrap_or(text);
    label::field(&text.to_ascii_uppercase())
}

/// `FORMAT`'s command line, as DOS 4.0's parser takes it: one drive, and
/// each switch once.
///
/// # Errors
///
/// What it says of a line it cannot take.
pub fn request(tail: &[u8]) -> Result<Request, Refused<'_>> {
    let mut request = Request {
        drive: 0,
        switches: 0,
        tracks: 0,
        sectors: 0,
        size: None,
        label: None,
    };
    let mut drive = None;
    for word in parse::arguments(tail) {
        let Some(switch) = word.strip_prefix(b"/") else {
            if drive.is_some() {
                return Err(Refused::Parse(Refusal::TooMany(word)));
            }
            match word {
                [letter, b':'] if letter.is_ascii_alphabetic() => {
                    drive = Some(letter.to_ascii_uppercase() - b'A');
                }
                _ => return Err(Refused::Parse(Refusal::Format(word))),
            }
            continue;
        };
        let (name, value) = switch_parts(switch);
        let bit = match (name.to_ascii_uppercase().as_slice(), value) {
            (b"S", None) => switch::S,
            (b"1", None) => switch::ONE,
            (b"4", None) => switch::FOUR,
            (b"8", None) => switch::EIGHT,
            (b"B", None) => switch::B,
            (b"V", _) => switch::V,
            (b"T", Some(_)) => switch::T,
            (b"N", Some(_)) => switch::N,
            (b"F", Some(_)) => switch::F,
            _ => return Err(Refused::Parse(Refusal::Switch(word))),
        };
        if request.switches & bit != 0 {
            return Err(Refused::Twice);
        }
        request.switches |= bit;
        match (bit, value) {
            (switch::T, Some(value)) => {
                request.tracks = value_in(value, 1..=1024, word).map_err(Refused::Parse)?;
            }
            (switch::N, Some(value)) => {
                request.sectors = value_in(value, 1..=64, word).map_err(Refused::Parse)?;
            }
            (switch::F, Some(value)) => {
                request.size = Some(size_of(value).ok_or(Refused::Parse(Refusal::Value(word)))?);
            }
            (switch::V, Some(value)) => {
                request.label = Some(label_given(value).ok_or(Refused::BadLabel)?);
            }
            _ => {}
        }
    }
    request.drive = drive.ok_or(Refused::Parse(Refusal::Missing))?;
    Ok(request)
}

/// What Get Device Parameters said of the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drive {
    pub kind: u8,
    pub cylinders: u16,
    pub bpb: [u8; DOS_BPB],
}

/// What a disk is formatted with: the parameters set for it, the switches
/// as the checks leave them, and whether its root is DOS 1's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub bpb: [u8; DOS_BPB],
    pub cylinders: u16,
    pub media_type: u8,
    pub switches: u16,
    pub old_dir: bool,
}

/// Why the switches cannot be had of the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    Incompatible,
    FixedDisk,
    ByDrive,
    /// A size asked of a drive of a type DOS 4.0 knows no sizes of.
    Unsupported,
    Both,
}

impl Problem {
    /// What `FORMAT` says of it.
    #[must_use]
    pub fn text(self) -> Vec<u8> {
        match self {
            Self::Incompatible => text::INCOMPATIBLE.to_vec(),
            Self::FixedDisk => text::FIXED_DISK.to_vec(),
            Self::ByDrive => text::BY_DRIVE.to_vec(),
            Self::Unsupported => [text::BY_DRIVE, text::INCOMPATIBLE].concat(),
            Self::Both => text::BOTH.to_vec(),
        }
    }
}

fn set_word(bpb: &mut [u8; DOS_BPB], at: usize, value: u16) {
    bpb[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// A fixed disk's BPB that has no FATs, as a driver gives one for a
/// partition holding no FAT volume, made the one DOS formats it with.
#[must_use]
pub fn fat_volume(bpb: [u8; DOS_BPB]) -> [u8; DOS_BPB] {
    if bpb[bpb_at::FATS] != 0 {
        return bpb;
    }
    let hidden = dword_at(&bpb, bpb_at::HIDDEN);
    let [heads, ..] = word_at(&bpb, bpb_at::HEADS).to_le_bytes();
    let [sectors, ..] = word_at(&bpb, bpb_at::SECTORS).to_le_bytes();
    partition(hidden, total_of(&bpb), heads, sectors).unwrap_or(bpb)
}

/// The switches `/F`'s size makes for a drive of type `drive_type`, as DOS 4.0's
/// `FORMAT` turns a size into the switches that ask for it.
fn size_switches(drive_type: u8, size: Diskette) -> Result<(u16, Option<(u16, u16)>), Problem> {
    let low_density = |size| match size {
        Diskette::K160 => Some(switch::ONE | switch::EIGHT),
        Diskette::K180 => Some(switch::ONE),
        Diskette::K320 => Some(switch::EIGHT),
        Diskette::K360 => Some(0),
        _ => None,
    };
    match (drive_type, size) {
        (kind::FIXED, _) => Err(Problem::FixedDisk),
        (kind::DD525, _) => low_density(size)
            .map(|bits| (bits, None))
            .ok_or(Problem::Incompatible),
        (kind::HD525, Diskette::K1200) | (kind::DD35, Diskette::K720) => Ok((0, None)),
        (kind::HD525, _) => low_density(size)
            .map(|bits| (bits | switch::FOUR, None))
            .ok_or(Problem::Incompatible),
        (kind::OTHER, Diskette::K1440) => Ok((switch::T | switch::N, Some((80, 18)))),
        (kind::OTHER, Diskette::K720) => Ok((switch::T | switch::N, Some((80, 9)))),
        (kind::DD35 | kind::OTHER, _) => Err(Problem::Incompatible),
        _ => Err(Problem::Unsupported),
    }
}

/// The disk `request` asks for on `drive`: DOS 4.0's checks of the
/// switches against each other and the drive, and the parameters they ask
/// for — the drive's own, a 5¼" double-density format's for `/1`, `/4` and
/// `/8`, or `/T` and `/N`'s.
///
/// # Errors
///
/// The [`Problem`] the checks find.
pub fn plan(drive: &Drive, request: &Request) -> Result<Plan, Problem> {
    let mut switches = request.switches;
    let (mut tracks, mut sectors) = (request.tracks, request.sectors);
    if switches & switch::F != 0 {
        if switches & (switch::ONE | switch::EIGHT | switch::FOUR | switch::N | switch::T) != 0 {
            return Err(Problem::Incompatible);
        }
        let (bits, numbers) = size_switches(drive.kind, request.size.unwrap_or(Diskette::K360))?;
        switches = (switches | bits) & !switch::F;
        if let Some(numbers) = numbers {
            (tracks, sectors) = numbers;
        }
    }
    let has = |bits: u16| switches & bits == bits;
    let any = |bits: u16| switches & bits != 0;
    let clash = has(switch::B | switch::EIGHT | switch::V)
        || has(switch::B | switch::S)
        || any(switch::ONE | switch::EIGHT | switch::FOUR) && any(switch::T | switch::N)
        || has(switch::V | switch::EIGHT);
    if clash {
        return Err(Problem::Incompatible);
    }
    if any(switch::N) != any(switch::T) {
        return Err(Problem::Both);
    }
    let mut plan = Plan {
        bpb: drive.bpb,
        cylinders: drive.cylinders,
        media_type: 0,
        switches,
        old_dir: false,
    };
    if drive.kind == kind::FIXED {
        plan.bpb = fat_volume(plan.bpb);
        return if switches & !(switch::S | switch::V | switch::B) == 0 {
            Ok(plan)
        } else {
            Err(Problem::FixedDisk)
        };
    }
    if !matches!(drive.kind, kind::DD525 | kind::HD525) {
        if switches & !(switch::V | switch::S | switch::N | switch::T | switch::B) != 0 {
            return Err(Problem::ByDrive);
        }
        return Ok(custom(plan, drive.kind, tracks, sectors));
    }
    if drive.kind == kind::DD525 || plan.switches & (switch::FOUR | switch::ONE) == 0 {
        plan.switches &= !switch::FOUR;
        // A drive of one head formats its one side already.
        if word_at(&plan.bpb, bpb_at::HEADS) <= 1 && plan.switches & switch::EIGHT == 0 {
            plan.switches &= !switch::ONE;
        }
    } else if plan.switches & switch::FOUR == 0 && plan.switches & (switch::N | switch::T) == 0 {
        return Err(Problem::Incompatible);
    }
    let interesting = plan.switches & !(switch::V | switch::S | switch::B);
    if interesting == 0 {
        return Ok(plan);
    }
    if plan.switches & (switch::N | switch::T) != 0 {
        if plan.switches & !(switch::N | switch::T | switch::V | switch::S) != 0 {
            return Err(Problem::Incompatible);
        }
        let double_density = drive.kind == kind::HD525 && tracks == 40 && matches!(sectors, 8 | 9);
        if !double_density {
            return Ok(custom(plan, drive.kind, tracks, sectors));
        }
        if sectors == 8 {
            plan.switches |= switch::EIGHT;
        }
        plan.switches &= !(switch::N | switch::T);
    }
    if drive.kind == kind::HD525 {
        plan.media_type = 1;
        plan.cylinders = 40;
    }
    plan.switches |= switch::FOUR;
    plan.bpb = match (
        plan.switches & switch::ONE != 0,
        plan.switches & switch::EIGHT != 0,
    ) {
        (false, false) => Diskette::K360,
        (true, false) => Diskette::K180,
        (false, true) => Diskette::K320,
        (true, true) => Diskette::K160,
    }
    .bpb();
    plan.old_dir = plan.switches & switch::EIGHT != 0;
    Ok(plan)
}

/// The format `/T` and `/N` ask for, over the BPB the plan has: `/T`'s
/// cylinders of `/N`'s sectors, DOS 4.0's table for a 3½" disk of 80 of 9,
/// and otherwise its count of FAT sectors, of the entries the clusters need.
fn custom(mut plan: Plan, drive_type: u8, tracks: u16, sectors: u16) -> Plan {
    if plan.switches & (switch::N | switch::T) == 0 {
        return plan;
    }
    let sectors = if plan.switches & switch::N != 0 {
        sectors
    } else {
        word_at(&plan.bpb, bpb_at::SECTORS)
    };
    if plan.switches & switch::T != 0 {
        plan.cylinders = tracks;
    }
    set_word(&mut plan.bpb, bpb_at::SECTORS, sectors);
    if !matches!(drive_type, kind::DD525 | kind::HD525) && plan.cylinders == 80 && sectors == 9 {
        plan.bpb = Diskette::K720.bpb();
        return plan;
    }
    let total = u32::from(sectors)
        * u32::from(word_at(&plan.bpb, bpb_at::HEADS))
        * u32::from(plan.cylinders);
    if let Ok(small) = u16::try_from(total) {
        set_word(&mut plan.bpb, bpb_at::TOTAL, small);
        plan.bpb[bpb_at::BIG_TOTAL..].fill(0);
    } else {
        set_word(&mut plan.bpb, bpb_at::TOTAL, 0);
        plan.bpb[bpb_at::BIG_TOTAL..].copy_from_slice(&total.to_le_bytes());
    }
    plan.bpb[bpb_at::MEDIA] = CUSTOM_MEDIA;
    plan.media_type = 0;
    let clusters = total / u32::from(plan.bpb[bpb_at::SECTORS_PER_CLUSTER].max(1));
    for bits in [12, 16] {
        let fat = u16::try_from((clusters * bits / 8) / 512 + 1).unwrap_or(u16::MAX);
        set_word(&mut plan.bpb, bpb_at::FAT, fat);
        if Bpb::from_dos(&plan.bpb).is_none_or(|bpb| bpb.kind == Kind::Fat12) {
            break;
        }
    }
    plan
}

/// The media byte of a format of no standard size.
const CUSTOM_MEDIA: u8 = 0xF0;

/// The serial number DOS 4.0's `FORMAT` gives a volume, from function
/// `2Ah`'s CX and DX and function `2Ch`'s.
#[must_use]
pub fn serial(date: (u16, u16), time: (u16, u16)) -> u32 {
    u32::from(date.1.wrapping_add(time.1)) << 16 | u32::from(date.0.wrapping_add(time.0))
}

/// The share of `done` tracks of `count`, in percent.
#[must_use]
pub fn percent(done: u32, count: u32) -> u16 {
    u16::try_from(u64::from(done) * 100 / u64::from(count.max(1)))
        .unwrap_or(100)
        .min(100)
}

/// The device parameters' BPB and the fields beside it, as the program
/// holds them.
pub(crate) fn drive_of(p: &mut Program<'_, '_>, at: u16) -> Drive {
    let mut bpb = [0u8; DOS_BPB];
    p.g.read_bytes(p.own, at + parameters::BPB, &mut bpb);
    Drive {
        kind: p.get8(at + parameters::KIND),
        cylinders: p.get16(at + parameters::CYLINDERS),
        bpb,
    }
}

/// The volume the parameters formatted with describe.
pub(crate) fn volume_of(p: &mut Program<'_, '_>) -> Option<Bpb> {
    let mut bpb = [0u8; DOS_BPB];
    p.g.read_bytes(p.own, at::PARAMS + parameters::BPB, &mut bpb);
    Bpb::from_dos(&bpb)
}

pub(crate) fn has(p: &mut Program<'_, '_>, bit: u8) -> bool {
    p.get8(at::FLAGS) & bit != 0
}

pub(crate) fn set_flag(p: &mut Program<'_, '_>, bit: u8, on: bool) {
    let flags = p.get8(at::FLAGS);
    p.set8(at::FLAGS, if on { flags | bit } else { flags & !bit });
}

pub(crate) fn switched(p: &mut Program<'_, '_>, bit: u16) -> bool {
    p.get16(at::SWITCHES) & bit != 0
}

/// Generic IOCTL `function` of category `08h` on the drive formatted, the
/// request at `packet`.
pub(crate) fn ioctl(p: &mut Program<'_, '_>, function: u8, packet: u16) -> Frame {
    let drive = p.get8(at::DRIVE);
    disk_ioctl(p, drive, function, packet)
}

/// The drive's parameters set from the block at `at`, special functions
/// `special` first.
pub(crate) fn set_parameters(p: &mut Program<'_, '_>, at: u16, special: u8) -> Frame {
    p.set8(at + parameters::SPECIAL, special);
    ioctl(p, generic::SET_PARAMETERS, at)
}

/// A device error's words, from its extended code, with the lines DOS
/// 4.0's `FORMAT` puts before them.
pub(crate) fn device_error(code: u16) -> Vec<u8> {
    let words = device_words(usize::from(driver_code(code)));
    [b"\r\n\r\n", words, b"\r\n"].concat()
}

/// The device errors a bad track gives: a CRC, a sector not found, a write
/// or read fault, a general failure.
pub(crate) fn bad_track(code: u16) -> bool {
    matches!(
        code,
        failed::CRC
            | failed::NOT_FOUND
            | failed::WRITE_FAULT
            | failed::READ_FAULT
            | failed::GENERAL
    )
}

locals! {
    /// `FORMAT`'s main routine.
    pub struct Format {}
}

/// The main routine's phases: from `SHRUNK` to before `RESTORED` a break
/// ends the format, the drive put back.
mod phase {
    pub const SAID: u8 = 1;
    pub const ATTRIBUTES: u8 = 2;
    pub const TRUENAME: u8 = 3;
    pub const BREAK_SET: u8 = 4;
    pub const SHRUNK: u8 = 5;
    pub const PARAMETERS: u8 = 6;
    pub const BOOT_READ: u8 = 8;
    pub const DTA_SET: u8 = 9;
    pub const CURRENT: u8 = 10;
    pub const DRIVE_SET: u8 = 11;
    pub const SEARCHED: u8 = 12;
    pub const BACK: u8 = 13;
    pub const ASKED_LABEL: u8 = 14;
    pub const TYPED_LABEL: u8 = 15;
    pub const LABEL_SAID: u8 = 16;
    pub const READ_SYSTEM: u8 = 17;
    pub const FAT_TAKEN: u8 = 18;
    pub const ROOT_TAKEN: u8 = 19;
    pub const CLUSTER_TAKEN: u8 = 20;
    pub const FORMATTED: u8 = 21;
    pub const FAILED_SAID: u8 = 22;
    pub const ANOTHER: u8 = 23;
    pub const ANOTHER_SAID: u8 = 24;
    pub const REASKED: u8 = 25;
    pub const RESTORED: u8 = 26;
    pub const BROKEN_SAID: u8 = 27;
    pub const TERMINATED: u8 = 28;
}

impl Format {
    /// Says `words` and ends with the fatal code, the drive's parameters
    /// put back.
    fn fatal(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8], error: bool) -> Flow {
        p.set8(at::STATUS, exit::FATAL);
        *phase = phase::SAID;
        Flow::Call(if error {
            say_error(p, words)
        } else {
            say(p, words)
        })
    }

    /// Says the checks' problem, then that the format is terminated.
    fn terminated(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        p.set8(at::STATUS, exit::FATAL);
        *phase = phase::TERMINATED;
        Flow::Call(say(p, words))
    }

    /// Ends, the drive's own parameters put back if they were changed.
    fn end(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if !has(p, flag::SAVED) {
            return Flow::Return(Ok(u32::from(p.get8(at::STATUS))));
        }
        set_flag(p, flag::SAVED, false);
        p.set16(at::SAVED + parameters::LAYOUT_ENTRIES, 0);
        *phase = phase::RESTORED;
        Flow::Call(set_parameters(p, at::SAVED, parameters::GOOD_LAYOUT))
    }

    /// The plan the switches make of the drive: refused, or laid into the
    /// parameters, and for a fixed disk its label checked.
    fn planned(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let switches = p.get16(at::ASKED);
        if let Err(problem) = replan(p, switches) {
            return Self::terminated(p, phase, &problem.text());
        }
        if volume_of(p).is_none() {
            return Self::fatal(p, phase, text::DEVICE_PARAMETERS, false);
        }
        let fixed = drive_of(p, at::SAVED).kind == kind::FIXED;
        set_flag(p, flag::FIXED, fixed);
        if fixed {
            return disk::read_boot(p, phase, phase::BOOT_READ);
        }
        Self::after_checks(p, phase)
    }

    /// The checks passed: the system is read for `/S`, then the memory the
    /// format needs taken.
    fn after_checks(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if switched(p, switch::S) {
            *phase = phase::READ_SYSTEM;
            return Flow::Call(Frame::of(&FormatSource::default()));
        }
        Self::take_fat(p, phase)
    }

    fn take_fat(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some(bpb) = volume_of(p) else {
            return Self::fatal(p, phase, text::DEVICE_PARAMETERS, false);
        };
        let paragraphs = u16::try_from(bpb.fat_sectors.saturating_mul(32)).unwrap_or(u16::MAX);
        p.set16(at::FAT_PARAGRAPHS, paragraphs);
        *phase = phase::FAT_TAKEN;
        Flow::Call(Frame::of(&Int {
            bx: paragraphs,
            ..Int::dos(p.own, 0x4800)
        }))
    }

    /// The next disk is formatted, from the plan the first had.
    fn next_disk(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        set_flag(p, flag::LAST_CHANCE, false);
        let switches = p.get16(at::ASKED);
        if let Err(problem) = replan(p, switches) {
            return Self::terminated(p, phase, &problem.text());
        }
        *phase = phase::FORMATTED;
        Flow::Call(Frame::of(&FormatDisk::default()))
    }
}

/// Plans the disk from the drive's own parameters and `switches`, laying
/// the plan into the parameters set for it.
pub(crate) fn replan(p: &mut Program<'_, '_>, switches: u16) -> Result<(), Problem> {
    let drive = drive_of(p, at::SAVED);
    let request = Request {
        drive: p.get8(at::DRIVE),
        switches,
        tracks: p.get16(at::TRACKS),
        sectors: p.get16(at::SECTORS),
        size: SIZES.get(usize::from(p.get8(at::SIZE))).copied(),
        label: None,
    };
    let planned = plan(&drive, &request)?;
    lay_out(p, &planned);
    Ok(())
}

/// Lays `plan` into the parameters set for each disk: its BPB, cylinders
/// and media type, and a track layout of its sectors numbered from 1.
pub(crate) fn lay_out(p: &mut Program<'_, '_>, plan: &Plan) {
    p.g.write_bytes(p.own, at::PARAMS + parameters::BPB, &plan.bpb);
    p.set16(at::PARAMS + parameters::CYLINDERS, plan.cylinders);
    p.set8(at::PARAMS + parameters::MEDIA_TYPE, plan.media_type);
    p.set16(at::SWITCHES, plan.switches);
    set_flag(p, flag::OLD_DIR, plan.old_dir);
    let sectors = word_at(&plan.bpb, bpb_at::SECTORS).min(parameters::LAYOUT_MAX);
    p.set16(at::PARAMS + parameters::LAYOUT_ENTRIES, sectors);
    let bytes = word_at(&plan.bpb, 0);
    for n in 0..sectors {
        let entry = at::PARAMS + parameters::LAYOUT + n * 4;
        p.set16(entry, n + 1);
        p.set16(entry + 2, bytes);
    }
}

impl Routine for Format {
    const ID: Id = Id::Format;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's FORMAT runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) && *phase >= phase::SHRUNK && *phase < phase::RESTORED {
            p.set8(at::STATUS, exit::BREAK);
            *phase = phase::BROKEN_SAID;
            return Flow::Call(say(p, text::INTERRUPTED));
        }
        match *phase {
            0 => {
                p.set8(at::STATUS, exit::OK);
                p.set8(at::FLAGS, 0);
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(refused) => {
                        p.set8(at::STATUS, exit::FATAL);
                        *phase = phase::SAID;
                        return Flow::Call(match refused {
                            Refused::Parse(refusal) => refuse(p, b"", refusal),
                            Refused::Twice => say(p, text::TWICE),
                            Refused::BadLabel => {
                                say(p, &over_progress(text::BAD_LABEL, 44, b"\r\n"))
                            }
                        });
                    }
                };
                p.set8(at::DRIVE, request.drive);
                p.set16(at::SWITCHES, request.switches);
                p.set16(at::TRACKS, request.tracks);
                p.set16(at::SECTORS, request.sectors);
                let size = request
                    .size
                    .and_then(|size| SIZES.iter().position(|&s| s == size));
                p.set8(
                    at::SIZE,
                    size.and_then(|n| u8::try_from(n).ok()).unwrap_or(u8::MAX),
                );
                p.g.write_bytes(p.own, at::LABEL, &request.label.unwrap_or([b' '; 11]));
                set_flag(p, flag::LABELLED, request.label.is_some());
                p.set16(at::SYSTEM, 0);
                p.set16(at::FAT, 0);
                *phase = phase::ATTRIBUTES;
                let drive = u16::from(request.drive) + 1;
                Flow::Call(Frame::of(&Int {
                    bx: drive,
                    ..Int::dos(p.own, 0x4409)
                }))
            }
            phase::ATTRIBUTES => {
                if child.is_err() {
                    return Self::fatal(p, phase, text::BAD_DRIVE, true);
                }
                let attributes = p.result(Returned::Dx);
                if attributes & drive_bits::NETWORK != 0 {
                    return Self::fatal(p, phase, text::NETWORK, false);
                }
                if attributes & drive_bits::SUBST != 0 {
                    return Self::fatal(p, phase, text::ASSIGNED, false);
                }
                let drive = p.get8(at::DRIVE);
                p.g.write_bytes(p.own, at::BUFFER, &[letter(drive), b':', b'\\', 0]);
                *phase = phase::TRUENAME;
                Flow::Call(Frame::of(&Int {
                    si: at::BUFFER,
                    di: at::BUFFER + 0x10,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            phase::TRUENAME => {
                if child.is_err() || p.get8(at::BUFFER + 0x10) != letter(p.get8(at::DRIVE)) {
                    return Self::fatal(p, phase, text::ASSIGNED, false);
                }
                *phase = phase::BREAK_SET;
                Flow::Call(Frame::of(&Int {
                    dx: handler_entry(handler::BREAK),
                    ..Int::dos(p.own, 0x2523)
                }))
            }
            phase::BREAK_SET => {
                take!(child);
                *phase = phase::SHRUNK;
                Flow::Call(p.shrink())
            }
            phase::SHRUNK => {
                take!(child);
                p.set8(at::PARAMS + parameters::SPECIAL, 0);
                *phase = phase::PARAMETERS;
                Flow::Call(ioctl(p, generic::GET_PARAMETERS, at::PARAMS))
            }
            phase::PARAMETERS => {
                if child.is_err() {
                    let words = [
                        text::NOT_SUPPORTED,
                        &[letter(p.get8(at::DRIVE)), b':'],
                        b"\r\n",
                    ]
                    .concat();
                    return Self::fatal(p, phase, &[b"\r", &words[..]].concat(), false);
                }
                let mut saved = [0u8; 0x28];
                p.g.read_bytes(p.own, at::PARAMS, &mut saved);
                p.g.write_bytes(p.own, at::SAVED, &saved);
                set_flag(p, flag::SAVED, true);
                let switches = p.get16(at::SWITCHES);
                p.set16(at::ASKED, switches);
                Self::planned(p, phase)
            }
            phase::BOOT_READ => {
                if !disk::boot_valid(p, child) {
                    return Self::after_checks(p, phase);
                }
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::DTA_SET => {
                take!(child);
                *phase = phase::CURRENT;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
            phase::CURRENT => {
                let current = take!(child).to_le_bytes()[0];
                p.set8(at::BUFFER, current);
                *phase = phase::DRIVE_SET;
                let drive = p.get8(at::DRIVE);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(drive),
                    ..Int::dos(p.own, 0x0E00)
                }))
            }
            phase::DRIVE_SET => {
                take!(child);
                let mut fcb = [0u8; 0x2C];
                fcb[0] = 0xFF;
                fcb[6] = tairix_fat::dir::attr::VOLUME_ID;
                fcb[8..19].fill(b'?');
                p.g.write_bytes(p.own, at::FCB, &fcb);
                *phase = phase::SEARCHED;
                Flow::Call(Frame::of(&Int {
                    dx: at::FCB,
                    ..Int::dos(p.own, 0x1100)
                }))
            }
            phase::SEARCHED => {
                let found = take!(child).to_le_bytes()[0] == 0;
                p.set8(at::BUFFER + 1, u8::from(found));
                *phase = phase::BACK;
                let current = p.get8(at::BUFFER);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(current),
                    ..Int::dos(p.own, 0x0E00)
                }))
            }
            phase::BACK => {
                take!(child);
                if p.get8(at::BUFFER + 1) == 0 {
                    return Self::after_checks(p, phase);
                }
                let mut old = [0u8; 11];
                p.g.read_bytes(p.own, at::DTA + 8, &mut old);
                p.g.write_bytes(p.own, at::BUFFER + 0x20, &old);
                let words = [
                    text::CURRENT_LABEL,
                    &[letter(p.get8(at::DRIVE)), b':', b' '],
                ]
                .concat();
                *phase = phase::ASKED_LABEL;
                Flow::Call(say(p, &words))
            }
            phase::ASKED_LABEL => {
                take!(child);
                *phase = phase::TYPED_LABEL;
                Flow::Call(Frame::of(&FormatAsk::line()))
            }
            phase::TYPED_LABEL => {
                take!(child);
                let typed = disk::typed(p);
                let mut old = [0u8; 11];
                p.g.read_bytes(p.own, at::BUFFER + 0x20, &mut old);
                let mut answer = [b' '; 11];
                let typed = typed.to_ascii_uppercase();
                let len = typed.len().min(11);
                answer[..len].copy_from_slice(&typed[..len]);
                if answer == old && typed.len() <= 11 {
                    *phase = phase::LABEL_SAID;
                    return Flow::Call(say(p, text::NEW_LINE));
                }
                let words = [
                    text::NEW_LINE,
                    &over_progress(text::BAD_LABEL, 44, b"\r\n")[..],
                ]
                .concat();
                Self::terminated(p, phase, &words)
            }
            phase::LABEL_SAID => {
                take!(child);
                Self::after_checks(p, phase)
            }
            phase::READ_SYSTEM => match child {
                Ok(_) => Self::take_fat(p, phase),
                Err(Fault::Break) => Flow::Return(Err(Fault::Break)),
                Err(_) => {
                    p.set8(at::STATUS, exit::FATAL);
                    Self::end(p, phase)
                }
            },
            phase::FAT_TAKEN => {
                let Ok(segment) = child.map(|ax| u16::try_from(ax).unwrap_or(0)) else {
                    return Self::terminated(p, phase, b"");
                };
                p.set16(at::FAT, segment);
                let Some(bpb) = volume_of(p) else {
                    return Self::fatal(p, phase, text::DEVICE_PARAMETERS, false);
                };
                let paragraphs = u16::try_from(bpb.root_sectors() * 32)
                    .unwrap_or(u16::MAX)
                    .max(32);
                *phase = phase::ROOT_TAKEN;
                Flow::Call(Frame::of(&Int {
                    bx: paragraphs,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::ROOT_TAKEN => {
                let Ok(segment) = child.map(|ax| u16::try_from(ax).unwrap_or(0)) else {
                    return Self::terminated(p, phase, b"");
                };
                p.set16(at::ROOT, segment);
                let cluster = volume_of(p).map_or(1, |bpb| u16::from(bpb.sectors_per_cluster));
                *phase = phase::CLUSTER_TAKEN;
                Flow::Call(Frame::of(&Int {
                    bx: cluster * 32,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::CLUSTER_TAKEN => {
                let segment = child.map_or(0, |ax| u16::try_from(ax).unwrap_or(0));
                p.set16(at::CLUSTER, segment);
                Self::next_disk(p, phase)
            }
            phase::FORMATTED => match take!(child) {
                disk::outcome::FATAL => Self::end(p, phase),
                outcome => {
                    if outcome == disk::outcome::FAILED {
                        p.set8(at::STATUS, exit::FATAL);
                    }
                    if has(p, flag::FIXED) {
                        return Self::end(p, phase);
                    }
                    *phase = phase::ANOTHER;
                    Flow::Call(say(p, text::ANOTHER))
                }
            },
            phase::ANOTHER => {
                take!(child);
                *phase = phase::ANOTHER_SAID;
                Flow::Call(Frame::of(&FormatAsk::yes_no()))
            }
            phase::ANOTHER_SAID => match take!(child) {
                disk::answer::YES => {
                    *phase = phase::FAILED_SAID;
                    Flow::Call(say(p, b"\r\n\r\n"))
                }
                disk::answer::NO | disk::answer::ENDED => Self::end(p, phase),
                _ => {
                    *phase = phase::REASKED;
                    Flow::Call(say(p, b"\r\n"))
                }
            },
            phase::REASKED => {
                take!(child);
                *phase = phase::ANOTHER;
                Flow::Call(say(p, text::ANOTHER))
            }
            phase::FAILED_SAID => {
                take!(child);
                Self::next_disk(p, phase)
            }
            phase::TERMINATED => {
                take!(child);
                *phase = phase::SAID;
                Flow::Call(say(
                    p,
                    &over_progress(text::TERMINATED, PROGRESS_WIDTH, b"\r\n"),
                ))
            }
            phase::RESTORED => Flow::Return(Ok(u32::from(p.get8(at::STATUS)))),
            _ => Self::end(p, phase),
        }
    }
}

locals! {
    /// INT 23h while `FORMAT` runs: its work is abandoned for its main
    /// routine to end it, the drive's parameters put back; while the volume
    /// is written the break waits, as half a volume is no disk.
    pub struct FormatBreak {}
}

impl Routine for FormatBreak {
    const ID: Id = Id::FormatBreak;

    fn step(&mut self, p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        if has(p, flag::WRITING) {
            return Flow::Return(Ok(0));
        }
        p.abandon()
    }
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
