//! `DISKCOPY [d: [d:]] [/1]`: a diskette copied track by track onto another,
//! as DOS 4.0's `DISKCOPY` copies one, with its checks, prompts and
//! messages: as many cylinders a pass as memory holds, the target formatted
//! as it is written where its format is not the source's, and the copy
//! given a serial number of its own. Two letters of one drive, or one
//! letter named twice, copy through that drive, the disks swapped between
//! passes.
//!
//! Where DOS 4.0's code is at odds with what it plainly means, the meaning
//! is followed: 5¼-inch and 3½-inch drives are told apart whatever their
//! density, a disk's cylinders are its sectors over those of a cylinder,
//! and it ends with the codes DOS 5 documents rather than DOS 4.0's, which
//! end a failed copy with `0`.

mod check;
mod copy;

pub use check::{DiskCopyFormat, DiskCopyProbe, DiskCopySource, DiskCopyTarget};
pub use copy::{DiskCopyDisk, DiskCopyPass, DiskCopyTrack};

use super::{disk_ioctl, refuse, Refusal};
use crate::dos::device::failed;
use crate::dos::device::generic::parameters::{self, kind};
use crate::dos::device::generic::{self, track};
use crate::dos::ioctl::drive_bits;
use crate::dos::message::named;
use crate::dos::program::{
    answer, handler_entry, say, Flow, Frame, Id, Int, Key, Program, Returned, Routine, ENDED,
};
use crate::dos::shell::handlers::device_words;
use crate::dos::shell::{letter, parse};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

/// Where `DISKCOPY`'s state lies in its memory.
pub mod at {
    use crate::dos::device::generic::parameters;
    use crate::dos::program::at::STATE;

    /// The source and target drives, from 0; the code it ends with;
    /// [`super::flag`].
    pub const SOURCE: u16 = STATE;
    pub const TARGET: u16 = STATE + 0x01;
    pub const STATUS: u16 = STATE + 0x02;
    pub const FLAGS: u16 = STATE + 0x03;
    /// The letters that owned the drives, as function `440Eh` names them.
    pub const SOURCE_OWNER: u16 = STATE + 0x04;
    pub const TARGET_OWNER: u16 = STATE + 0x05;
    /// The disk copied: its last cylinder, sectors a track, last side and
    /// bytes a sector.
    pub const LAST_TRACK: u16 = STATE + 0x06;
    pub const SECTORS: u16 = STATE + 0x07;
    pub const LAST_SIDE: u16 = STATE + 0x08;
    pub const SECTOR_BYTES: u16 = STATE + 0x0A;
    /// The cylinders reached, the next read and the next written, and the
    /// side.
    pub const TO_READ: u16 = STATE + 0x0C;
    pub const TO_WRITE: u16 = STATE + 0x0D;
    pub const SIDE: u16 = STATE + 0x0E;
    /// The buffer's segment and paragraphs, a cylinder's paragraphs, and
    /// where the next track goes.
    pub const BUFFER: u16 = STATE + 0x10;
    pub const ROOM: u16 = STATE + 0x12;
    pub const CYLINDER: u16 = STATE + 0x14;
    pub const PLACE: u16 = STATE + 0x16;
    /// Tracks not copied, and the serial number the copy was given.
    pub const ERRORS: u16 = STATE + 0x18;
    pub const SERIAL: u16 = STATE + 0x1A;
    /// The track a format of the target has reached.
    pub const FORMAT_TRACK: u16 = STATE + 0x1E;
    pub const FORMAT_SIDE: u16 = STATE + 0x1F;
    /// The key answering whether another is copied, or what the input's end
    /// gave instead.
    pub const KEY: u16 = STATE + 0x20;
    /// The drives' parameters as they were, the source's medium's, and the
    /// target's as it is set.
    pub const SOURCE_DRIVE: u16 = STATE + 0x22;
    pub const TARGET_DRIVE: u16 = SOURCE_DRIVE + parameters::LAYOUT;
    pub const SOURCE_MEDIUM: u16 = TARGET_DRIVE + parameters::LAYOUT;
    pub const TARGET_MEDIUM: u16 = SOURCE_MEDIUM + parameters::LEN;
    /// A track request, and a sector read to tell an old disk's format by.
    pub const PACKET: u16 = TARGET_MEDIUM + parameters::LEN;
    pub const SECTOR: u16 = PACKET + 0x10;
    /// Bytes of `DISKCOPY`'s state.
    pub const LEN: u16 = SECTOR + 0x200 - STATE;
}

/// What the state's flags record.
pub mod flag {
    /// A drive's own parameters were changed, and are put back.
    pub const SOURCE_SET: u8 = 0x01;
    pub const TARGET_SET: u8 = 0x02;
    /// The drives' owners were asked for, and are put back.
    pub const OWNERS: u8 = 0x04;
    /// The disks take turns in one drive.
    pub const SINGLE: u8 = 0x08;
    /// `/1`: the first side alone is copied.
    pub const ONE_SIDE: u8 = 0x10;
    /// The target is formatted as it is written.
    pub const FORMATTING: u8 = 0x20;
    /// The copy cannot go on.
    pub const FATAL: u8 = 0x40;
    /// The copy's boot record was given a serial number of its own.
    pub const SERIAL: u8 = 0x80;
}

/// The handlers `DISKCOPY` installs.
pub mod handler {
    /// INT 23h, the work abandoned for the main routine to end it, the
    /// drives put back as they were.
    pub const BREAK: u16 = 0;
}

/// The codes `DISKCOPY` ends with, as DOS 5 documents them.
pub mod exit {
    pub const COPIED: u8 = 0;
    pub const TRACKS_LOST: u8 = 1;
    pub const BREAK: u8 = 2;
    pub const FATAL: u8 = 3;
    pub const REFUSED: u8 = 4;
}

/// What `DISKCOPY` says, as DOS 4.0's does.
pub mod text {
    pub use crate::dos::message::{MEMORY, NEW_LINE, PRESS_ANY_KEY};
    pub const COMMAND_FORMAT: &[u8] =
        b"Do not specify filename(s)\r\nCommand Format: DISKCOPY d: d: [/1]\r\n";
    pub const BAD_DRIVE: &[u8] = b"\r\nInvalid drive specification\r\nSpecified drive does not exist\r\nor is non-removable\r\n";
    pub const NETWORK: &[u8] = b"\r\nCannot DISKCOPY to or from\r\na network drive\r\n";
    pub const FORMATTING: &[u8] = b"\r\nFormatting while copying\r\n";
    pub const INSERT_SOURCE: &[u8] = b"\r\nInsert SOURCE diskette in drive ";
    pub const INSERT_TARGET: &[u8] = b"\r\nInsert TARGET diskette in drive ";
    pub const CLOSE_DOOR: &[u8] =
        b"Make sure a diskette is inserted into\r\nthe drive and the door is closed\r\n";
    pub const MAY_BE_UNUSABLE: &[u8] = b"\r\nTarget diskette may be unusable\r\n";
    pub const ANOTHER: &[u8] = b"\r\nCopy another diskette (Y/N)? ";
    pub const INCOMPATIBLE: &[u8] = b"\r\nDrive types or diskette types\r\nnot compatible\r\n";
    pub const READ_ERROR: &[u8] = b"\r\nUnrecoverable read error on drive ";
    pub const WRITE_ERROR: &[u8] = b"\r\nUnrecoverable write error on drive ";
    pub const ENDED: &[u8] = b"\r\nCopy process ended\r\n";
    pub const BAD_SOURCE: &[u8] = b"\r\nSOURCE diskette bad or incompatible";
    pub const BAD_TARGET: &[u8] = b"\r\nTARGET diskette bad or incompatible";
}

/// What `DISKCOPY`'s command line names: the source and target drives,
/// from 0, and whether `/1` asks for the first side alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub source: Option<u8>,
    pub target: Option<u8>,
    pub one_side: bool,
}

/// `DISKCOPY`'s command line read as DOS's common parser reads it: two
/// drives at most, each a letter and a colon alone, and `/1` once.
///
/// # Errors
///
/// The [`Refusal`] the parser gives a line it cannot take.
pub fn request(tail: &[u8]) -> Result<Request, Refusal<'_>> {
    let mut request = Request::default();
    for word in parse::arguments(tail) {
        if word.first() == Some(&b'/') {
            if word == b"/1" && !request.one_side {
                request.one_side = true;
                continue;
            }
            return Err(Refusal::Switch(word));
        }
        let drive = match word {
            [letter, b':'] if letter.is_ascii_alphabetic() => letter.to_ascii_uppercase() - b'A',
            _ => return Err(Refusal::Format(word)),
        };
        match (request.source, request.target) {
            (None, _) => request.source = Some(drive),
            (Some(_), None) => request.target = Some(drive),
            (Some(_), Some(_)) => return Err(Refusal::TooMany(word)),
        }
    }
    Ok(request)
}

/// Whether a drive of device type `kind` takes 3½-inch disks.
const fn three_and_a_half(drive_type: u8) -> bool {
    matches!(drive_type, kind::DD35 | kind::OTHER)
}

/// A child's failure the routine goes on past, as DOS's does: not an
/// abort or a break, which end it.
pub(crate) fn past(child: Outcome) -> Result<(), Fault> {
    match child {
        Err(fault @ (Fault::Abort | Fault::Break)) => Err(fault),
        _ => Ok(()),
    }
}

pub(crate) fn has(p: &mut Program<'_, '_>, mask: u8) -> bool {
    p.get8(at::FLAGS) & mask != 0
}

pub(crate) fn set_flag(p: &mut Program<'_, '_>, mask: u8, on: bool) {
    let flags = p.get8(at::FLAGS);
    p.set8(at::FLAGS, if on { flags | mask } else { flags & !mask });
}

/// The generic IOCTL `function` on the drive at `drive` in the state, its
/// request at `packet`.
pub(crate) fn ioctl(p: &mut Program<'_, '_>, drive: u16, function: u8, packet: u16) -> Frame {
    let drive = p.get8(drive);
    disk_ioctl(p, drive, function, packet)
}

/// Sets the parameters the block at `at` holds on the drive at `drive`,
/// special functions `special` first.
pub(crate) fn set_parameters(p: &mut Program<'_, '_>, drive: u16, at: u16, special: u8) -> Frame {
    p.set8(at + parameters::SPECIAL, special);
    ioctl(p, drive, generic::SET_PARAMETERS, at)
}

/// Lays the track request for `count` sectors from `first` of the track at
/// `cylinder` and `head`, into or from `buffer`'s segment.
pub(crate) fn track_request(
    p: &mut Program<'_, '_>,
    head: u8,
    cylinder: u8,
    first: u16,
    count: u16,
    buffer: crate::frames::Far,
) {
    p.set8(at::PACKET + track::SPECIAL, 0);
    p.set16(at::PACKET + track::HEAD, u16::from(head));
    p.set16(at::PACKET + track::CYLINDER, u16::from(cylinder));
    p.set16(at::PACKET + track::FIRST, first);
    p.set16(at::PACKET + track::COUNT, count);
    p.set_far(at::PACKET + track::BUFFER, buffer);
}

/// What a press waits under: the disks asked for, or a drive's error.
pub mod press {
    pub const BOTH: u8 = 0;
    pub const SOURCE: u8 = 1;
    pub const TARGET: u8 = 2;
    pub const NOT_READY: u8 = 3;
    pub const WRITE_PROTECT: u8 = 4;
}

/// What a drive's failure `code` is answered by when the user can put it
/// right — an empty drive, a write-protected disk — the press asking them
/// to; none for a failure of the disk itself.
pub(crate) const fn soft(code: u16) -> Option<u8> {
    match code {
        failed::NOT_READY => Some(press::NOT_READY),
        failed::WRITE_PROTECT => Some(press::WRITE_PROTECT),
        _ => None,
    }
}

locals! {
    /// What is said, then a key waited for: the disks to insert, or what
    /// the drive at `drive` in the state failed with.
    pub struct DiskCopyPress {
        what: u8,
        drive: u16,
    }
}

impl DiskCopyPress {
    #[must_use]
    pub const fn of(what: u8, drive: u16) -> Self {
        Self { what, drive }
    }

    fn words(self, p: &mut Program<'_, '_>) -> Vec<u8> {
        let source = [letter(p.get8(at::SOURCE)), b':'];
        let target = [letter(p.get8(at::TARGET)), b':'];
        let source_line = [text::INSERT_SOURCE, &source, text::NEW_LINE].concat();
        let target_line = [text::INSERT_TARGET, &target, text::NEW_LINE].concat();
        let failed = [letter(p.get8(self.drive))];
        let prompt: Vec<u8> = match self.what {
            press::BOTH => [source_line, target_line].concat(),
            press::SOURCE => source_line,
            press::TARGET => target_line,
            press::NOT_READY => {
                let words = device_words(usize::from(crate::dos::device::driver_code(
                    failed::NOT_READY,
                )));
                [text::NEW_LINE, &named(words, &failed), text::CLOSE_DOOR].concat()
            }
            _ => {
                let words = device_words(usize::from(crate::dos::device::driver_code(
                    failed::WRITE_PROTECT,
                )));
                [text::NEW_LINE, words, text::NEW_LINE].concat()
            }
        };
        [
            &prompt[..],
            text::NEW_LINE,
            text::PRESS_ANY_KEY,
            text::NEW_LINE,
        ]
        .concat()
    }
}

mod pressing {
    pub const SAID: u8 = 1;
    pub const KEYED: u8 = 2;
}

impl Routine for DiskCopyPress {
    const ID: Id = Id::DiskCopyPress;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            pressing::SAID => {
                take!(child);
                *phase = pressing::KEYED;
                Flow::Call(Frame::of(&Key::of(0x0C08)))
            }
            pressing::KEYED => Flow::Return(if take!(child) == ENDED {
                Err(Fault::Break)
            } else {
                Ok(0)
            }),
            _ => {
                *phase = pressing::SAID;
                let words = self.words(p);
                Flow::Call(say(p, &words))
            }
        }
    }
}

locals! {
    /// `DISKCOPY`'s main routine.
    pub struct DiskCopy {}
}

/// The main routine's phases: from `SHRUNK` to before `STOPPED_SAID` a
/// break ends the copy, the drives put back.
mod phase {
    pub const REFUSED_SAID: u8 = 1;
    pub const CURRENT: u8 = 2;
    pub const BREAK_SET: u8 = 3;
    pub const SHRUNK: u8 = 4;
    pub const SOURCE_LOCAL: u8 = 5;
    pub const TARGET_LOCAL: u8 = 6;
    pub const SOURCE_FIXED: u8 = 7;
    pub const TARGET_FIXED: u8 = 8;
    pub const SOURCE_OWNED: u8 = 9;
    pub const TARGET_OWNED: u8 = 10;
    pub const SOURCE_TAKEN: u8 = 11;
    pub const TARGET_TAKEN: u8 = 12;
    pub const OWNER_SEEN: u8 = 13;
    pub const GIVEN_BACK: u8 = 14;
    pub const SOURCE_ASKED: u8 = 15;
    pub const TARGET_ASKED: u8 = 16;
    pub const LARGEST: u8 = 17;
    pub const TAKEN: u8 = 18;
    pub const COPIED: u8 = 19;
    pub const ASKED: u8 = 20;
    pub const KEYED: u8 = 21;
    pub const ENDED_LINE: u8 = 22;
    pub const CHECKED: u8 = 23;
    pub const REASK: u8 = 24;
    pub const STOPPED_SAID: u8 = 25;
    pub const SOURCE_OWNER_BACK: u8 = 26;
    pub const TARGET_OWNER_BACK: u8 = 27;
    pub const SOURCE_BACK: u8 = 28;
    pub const TARGET_BACK: u8 = 29;
    pub const FORMAT_SAID: u8 = 30;
}

impl DiskCopy {
    /// Says `words` and ends refused, whatever was taken over put back.
    fn refused(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        p.set8(at::STATUS, exit::REFUSED);
        *phase = phase::STOPPED_SAID;
        Flow::Call(say(p, words))
    }

    /// Function `subfunction` of `44h` on the drive at `drive` in the state.
    fn drive_call(p: &mut Program<'_, '_>, subfunction: u8, drive: u16) -> Flow {
        let drive = p.get8(drive);
        Flow::Call(Frame::of(&Int {
            bx: u16::from(drive) + 1,
            ..Int::dos(p.own, u16::from_le_bytes([subfunction, 0x44]))
        }))
    }

    /// The drive, from 0, that function `440Eh` answered owns the drive at
    /// `drive` in the state: that drive itself when it has one letter.
    fn owner(p: &mut Program<'_, '_>, child: Outcome, drive: u16) -> u8 {
        match child.map(|_| p.result(Returned::Ax).to_le_bytes()[0]) {
            Ok(owner @ 1..) => owner - 1,
            _ => p.get8(drive),
        }
    }

    /// The drive's own parameters asked for into `at`.
    fn ask_drive(p: &mut Program<'_, '_>, drive: u16, at: u16) -> Frame {
        p.set8(at + parameters::SPECIAL, 0);
        ioctl(p, drive, generic::GET_PARAMETERS, at)
    }

    /// A break while the drives are put back ends the program with its
    /// code once they are, not half put back.
    fn broken_in_restore(p: &mut Program<'_, '_>, child: Outcome) {
        if child == Err(Fault::Break) {
            p.set8(at::STATUS, exit::BREAK);
        }
    }

    fn copy(phase: &mut u8) -> Flow {
        *phase = phase::COPIED;
        Flow::Call(Frame::of(&DiskCopyDisk::default()))
    }

    /// The drives' owners and parameters put back, then the end.
    fn restore(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if has(p, flag::OWNERS) {
            set_flag(p, flag::OWNERS, false);
            *phase = phase::SOURCE_OWNER_BACK;
            return Self::drive_call(p, 0x0F, at::SOURCE_OWNER);
        }
        if has(p, flag::SOURCE_SET) {
            set_flag(p, flag::SOURCE_SET, false);
            p.set16(at::SOURCE_DRIVE + parameters::LAYOUT_ENTRIES, 0);
            *phase = phase::SOURCE_BACK;
            return Flow::Call(set_parameters(p, at::SOURCE, at::SOURCE_DRIVE, 0));
        }
        if has(p, flag::TARGET_SET) {
            set_flag(p, flag::TARGET_SET, false);
            p.set16(at::TARGET_DRIVE + parameters::LAYOUT_ENTRIES, 0);
            *phase = phase::TARGET_BACK;
            return Flow::Call(set_parameters(p, at::TARGET, at::TARGET_DRIVE, 0));
        }
        Flow::Return(Ok(u32::from(p.get8(at::STATUS))))
    }
}

impl Routine for DiskCopy {
    const ID: Id = Id::DiskCopy;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's DISKCOPY runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) && *phase >= phase::SHRUNK && *phase < phase::STOPPED_SAID {
            p.set8(at::STATUS, exit::BREAK);
            return Self::restore(p, phase);
        }
        match *phase {
            0 => {
                p.set8(at::STATUS, exit::COPIED);
                p.set8(at::FLAGS, 0);
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(refusal) => {
                        p.set8(at::STATUS, exit::REFUSED);
                        *phase = phase::REFUSED_SAID;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                p.set8(at::SOURCE, request.source.unwrap_or(u8::MAX));
                p.set8(at::TARGET, request.target.unwrap_or(u8::MAX));
                set_flag(p, flag::ONE_SIDE, request.one_side);
                *phase = phase::CURRENT;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
            phase::REFUSED_SAID => {
                take!(child);
                *phase = phase::FORMAT_SAID;
                Flow::Call(say(p, text::COMMAND_FORMAT))
            }
            phase::CURRENT => {
                let current = take!(child).to_le_bytes()[0];
                if p.get8(at::SOURCE) == u8::MAX {
                    p.set8(at::SOURCE, current);
                }
                if p.get8(at::TARGET) == u8::MAX {
                    p.set8(at::TARGET, current);
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
                *phase = phase::SOURCE_LOCAL;
                Self::drive_call(p, 0x09, at::SOURCE)
            }
            phase::SOURCE_LOCAL | phase::TARGET_LOCAL => {
                if child.is_err() {
                    return Self::refused(p, phase, text::BAD_DRIVE);
                }
                if p.result(Returned::Dx) & drive_bits::NETWORK != 0 {
                    return Self::refused(p, phase, text::NETWORK);
                }
                if *phase == phase::SOURCE_LOCAL {
                    *phase = phase::TARGET_LOCAL;
                    return Self::drive_call(p, 0x09, at::TARGET);
                }
                *phase = phase::SOURCE_FIXED;
                Self::drive_call(p, 0x08, at::SOURCE)
            }
            phase::SOURCE_FIXED | phase::TARGET_FIXED => {
                if child.is_err() || p.result(Returned::Ax) != 0 {
                    return Self::refused(p, phase, text::BAD_DRIVE);
                }
                if *phase == phase::SOURCE_FIXED {
                    *phase = phase::TARGET_FIXED;
                    return Self::drive_call(p, 0x08, at::TARGET);
                }
                *phase = phase::SOURCE_OWNED;
                Self::drive_call(p, 0x0E, at::SOURCE)
            }
            phase::SOURCE_OWNED => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                let owner = Self::owner(p, child, at::SOURCE);
                p.set8(at::SOURCE_OWNER, owner);
                *phase = phase::TARGET_OWNED;
                Self::drive_call(p, 0x0E, at::TARGET)
            }
            phase::TARGET_OWNED => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                let owner = Self::owner(p, child, at::TARGET);
                p.set8(at::TARGET_OWNER, owner);
                set_flag(p, flag::OWNERS, true);
                *phase = phase::SOURCE_TAKEN;
                Self::drive_call(p, 0x0F, at::SOURCE)
            }
            phase::SOURCE_TAKEN => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                *phase = phase::TARGET_TAKEN;
                Self::drive_call(p, 0x0F, at::TARGET)
            }
            phase::TARGET_TAKEN => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                *phase = phase::OWNER_SEEN;
                Self::drive_call(p, 0x0E, at::SOURCE)
            }
            phase::OWNER_SEEN => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                let owner = Self::owner(p, child, at::SOURCE);
                let (source, target) = (p.get8(at::SOURCE), p.get8(at::TARGET));
                if owner != source {
                    set_flag(p, flag::SINGLE, true);
                    p.set8(at::TARGET, source);
                    *phase = phase::GIVEN_BACK;
                    return Self::drive_call(p, 0x0F, at::SOURCE);
                }
                set_flag(p, flag::SINGLE, source == target);
                *phase = phase::SOURCE_ASKED;
                Flow::Call(Self::ask_drive(p, at::SOURCE, at::SOURCE_DRIVE))
            }
            phase::GIVEN_BACK => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                *phase = phase::SOURCE_ASKED;
                Flow::Call(Self::ask_drive(p, at::SOURCE, at::SOURCE_DRIVE))
            }
            phase::SOURCE_ASKED | phase::TARGET_ASKED => {
                let block = if *phase == phase::SOURCE_ASKED {
                    at::SOURCE_DRIVE
                } else {
                    at::TARGET_DRIVE
                };
                if child.is_err()
                    || p.get16(block + parameters::ATTRIBUTES) & parameters::NON_REMOVABLE != 0
                {
                    return Self::refused(p, phase, text::BAD_DRIVE);
                }
                if *phase == phase::SOURCE_ASKED {
                    *phase = phase::TARGET_ASKED;
                    return Flow::Call(Self::ask_drive(p, at::TARGET, at::TARGET_DRIVE));
                }
                let source = p.get8(at::SOURCE_DRIVE + parameters::KIND);
                let target = p.get8(at::TARGET_DRIVE + parameters::KIND);
                if three_and_a_half(source) != three_and_a_half(target) {
                    return Self::refused(p, phase, text::INCOMPATIBLE);
                }
                *phase = phase::LARGEST;
                Flow::Call(Frame::of(&Int {
                    bx: 0xFFFF,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::LARGEST => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                let largest = p.result(Returned::Bx);
                p.set16(at::ROOM, largest);
                p.set16(at::BUFFER, 0);
                if largest == 0 {
                    return Self::copy(phase);
                }
                *phase = phase::TAKEN;
                Flow::Call(Frame::of(&Int {
                    bx: largest,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::TAKEN => {
                match child {
                    Ok(segment) => p.set16(at::BUFFER, u16::try_from(segment).unwrap_or(0)),
                    Err(Fault::Error(_)) => p.set16(at::ROOM, 0),
                    Err(fault) => return Flow::Return(Err(fault)),
                }
                Self::copy(phase)
            }
            phase::COPIED => {
                let code = take!(child);
                p.set8(at::STATUS, u8::try_from(code).unwrap_or(exit::FATAL));
                *phase = phase::ASKED;
                Flow::Call(say(p, text::ANOTHER))
            }
            phase::ASKED => {
                take!(child);
                *phase = phase::KEYED;
                Flow::Call(Frame::of(&Key::of(0x0C01)))
            }
            phase::KEYED => {
                let key = take!(child);
                p.set16(at::KEY, u16::try_from(key).unwrap_or(u16::MAX));
                *phase = phase::ENDED_LINE;
                Flow::Call(say(p, text::NEW_LINE))
            }
            phase::ENDED_LINE => {
                take!(child);
                let key = p.get16(at::KEY);
                if u32::from(key) >= ENDED {
                    return Self::restore(p, phase);
                }
                *phase = phase::CHECKED;
                Flow::Call(Frame::of(&Int {
                    dx: key,
                    ..Int::dos(p.own, 0x6523)
                }))
            }
            phase::CHECKED => match child {
                Ok(answer::YES) => Self::copy(phase),
                Ok(answer::NO) => Self::restore(p, phase),
                _ => {
                    *phase = phase::REASK;
                    Flow::Call(say(p, text::NEW_LINE))
                }
            },
            phase::REASK => {
                take!(child);
                *phase = phase::ASKED;
                Flow::Call(say(p, text::ANOTHER))
            }
            phase::FORMAT_SAID => {
                take!(child);
                Flow::Return(Ok(u32::from(exit::REFUSED)))
            }
            phase::SOURCE_OWNER_BACK => {
                Self::broken_in_restore(p, child);
                *phase = phase::TARGET_OWNER_BACK;
                Self::drive_call(p, 0x0F, at::TARGET_OWNER)
            }
            phase::STOPPED_SAID
            | phase::TARGET_OWNER_BACK
            | phase::SOURCE_BACK
            | phase::TARGET_BACK => {
                Self::broken_in_restore(p, child);
                Self::restore(p, phase)
            }
            _ => Self::restore(p, phase),
        }
    }
}

#[cfg(test)]
#[path = "diskcopy_tests.rs"]
mod tests;
