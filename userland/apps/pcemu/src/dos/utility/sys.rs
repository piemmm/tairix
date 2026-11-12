//! `SYS [d:][path] d:`: a disk made able to start TAIRiX DOS — the boot
//! record that finds the kernel in the emulator's ROM written over its own,
//! its BPB kept, and `COMMAND.COM` copied to its root — with DOS 4.0's
//! `SYS`'s checks and messages, every one on the standard error. As DOS 5's
//! `SYS` does, the command interpreter is part of the system; TAIRiX DOS's
//! kernel is in the ROM, so there are no other system files to place.
//! Without a source the system comes from the current drive, which is then
//! not to be the target; one not found on a removable disk is asked for.

use super::{absolute, carried, refuse, Refusal, Sectors};
use crate::dos::device::generic::media;
use crate::dos::ioctl::drive_bits;
use crate::dos::program::{
    say_error, Flow, Frame, Id, Int, Key, Program, Returned, Routine, ENDED,
};
use crate::dos::shell::copy::{error, CopyFile};
use crate::dos::shell::{self, asciz_at, letter, parse, set_asciz};
use crate::frames::{locals, take, Far, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::boot::{Bpb, Kind, BOOT_SECTOR};
use tairix_fat::dir::attr;
use tairix_fat::format::{FIXED_DRIVE, FIXED_MEDIA};

/// Where `SYS`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The target drive and the source's, from `0`, and whether a source
    /// was named.
    pub const TARGET: u16 = STATE;
    pub const SOURCE_DRIVE: u16 = STATE + 0x01;
    pub const NAMED: u16 = STATE + 0x02;
    /// The new `COMMAND.COM`'s size, and the old one's.
    pub const SIZE: u16 = STATE + 0x04;
    pub const OLD_SIZE: u16 = STATE + 0x08;
    /// The source's `COMMAND.COM` and the target's, ASCIZ.
    pub const FROM: u16 = STATE + 0x10;
    pub const FROM_LEN: u16 = 0x50;
    pub const TO: u16 = STATE + 0x60;
    pub const TO_LEN: u16 = 0x10;
    /// The DTA, the medium's ID, the label's search FCB and INT 25h's and
    /// 26h's request.
    pub const DTA: u16 = STATE + 0x70;
    pub const MEDIA: u16 = STATE + 0xA0;
    pub const FCB: u16 = STATE + 0xC0;
    pub const PACKET: u16 = STATE + 0xF0;
    /// The time the serial number a disk without one is given is made from.
    pub const CLOCK: u16 = STATE + 0xFC;
    /// What the copy and the boot record pass through.
    pub const BUFFER: u16 = STATE + 0x100;
    pub const BUFFER_LEN: u16 = 0x800;
    /// Bytes of `SYS`'s state.
    pub const LEN: u16 = BUFFER + BUFFER_LEN - STATE;
}

mod text {
    pub use crate::dos::message::{BAD_DRIVE, NEW_LINE, PRESS_ANY_KEY, TRANSFERRED};
    pub const DEFAULT: &[u8] = b"Cannot specify default drive\r\n";
    pub const NETWORK: &[u8] = b"Cannot SYS a network drive\r\n";
    pub const SUBST: &[u8] = b"Cannot SYS a SUBSTed or ASSIGNed drive\r\n";
    pub const NO_SYSTEM: &[u8] = b"No system on default drive\r\n";
    pub const NOT_FOUND: &[u8] = b"Invalid path or System files not found\r\n";
    pub const NO_ROOM: &[u8] = b"No room for system on destination disk\r\n";
    pub const WRITE_FAILED: &[u8] = b"Write failure, diskette unusable\r\n";
}

/// What a system file is searched for as: hidden, system or read-only too.
const SYSTEM_FILE: u16 = (attr::READ_ONLY | attr::HIDDEN | attr::SYSTEM) as u16;

/// What `SYS`'s command line names: the source, if any, and the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub source: Option<&'a [u8]>,
    pub target: u8,
}

/// The drive a word names, if it is `X:` alone.
fn drive_alone(word: &[u8]) -> Option<u8> {
    match word {
        [letter, b':'] if letter.is_ascii_alphabetic() => Some(letter.to_ascii_uppercase() - b'A'),
        _ => None,
    }
}

/// `SYS`'s command line, as DOS 4.0's parser reads it: a target drive,
/// after a source drive or path.
///
/// # Errors
///
/// What DOS's parser says of a line it cannot take.
pub fn request(tail: &[u8]) -> Result<Request<'_>, Refusal<'_>> {
    let words = parse::arguments(tail);
    if let Some(switch) = words.iter().find(|word| word.starts_with(b"/")) {
        return Err(Refusal::Switch(switch));
    }
    match words.as_slice() {
        [] => Err(Refusal::Missing),
        [target] => drive_alone(target)
            .map(|target| Request {
                source: None,
                target,
            })
            .ok_or(Refusal::Missing),
        [source, target] => drive_alone(target)
            .map(|target| Request {
                source: Some(source),
                target,
            })
            .ok_or(Refusal::Format(target)),
        [_, _, extra, ..] => Err(Refusal::TooMany(extra)),
    }
}

/// The path of `COMMAND.COM` in the source `source` names: a drive's root,
/// or a directory.
#[must_use]
pub fn source_path(source: &[u8]) -> Vec<u8> {
    let mut path = source.to_vec();
    if drive_alone(source).is_none() && path.last() == Some(&b'\\') {
        path.pop();
    }
    path.push(b'\\');
    path.extend_from_slice(shell::FILE);
    path
}

/// The serial number DOS 4.0's `SYS` gives a disk that has none: the time
/// function `2Ch` gave, its words doubled.
#[must_use]
pub fn serial_of(cx: u16, dx: u16) -> u32 {
    u32::from(cx.wrapping_add(cx)) << 16 | u32::from(dx.wrapping_add(dx))
}

locals! {
    /// `SYS`'s main routine.
    pub struct Sys {}
}

mod phase {
    pub const SAID: u8 = 1;
    pub const CURRENT: u8 = 2;
    pub const ATTRIBUTES: u8 = 3;
    pub const REMOVABLE: u8 = 4;
    pub const TRUENAME: u8 = 5;
    pub const DTA_SET: u8 = 6;
    pub const FOUND: u8 = 7;
    pub const SOURCE_REMOVABLE: u8 = 8;
    pub const ASKED: u8 = 9;
    pub const PRESSED: u8 = 10;
    pub const MEDIA_ID: u8 = 11;
    pub const OLD_FOUND: u8 = 12;
    pub const SPACE: u8 = 13;
    pub const PLAIN: u8 = 14;
    pub const COPIED: u8 = 15;
    pub const BOOT_READ: u8 = 16;
    pub const CLOCK: u8 = 17;
    pub const LABEL: u8 = 18;
    pub const BOOT_WRITTEN: u8 = 19;
    pub const MEDIA_SET: u8 = 20;
}

impl Sys {
    fn say(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        *phase = phase::SAID;
        Flow::Call(say_error(p, words))
    }

    /// Says that the source holds no system to be had: its named path, or
    /// the current drive.
    fn missing(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let words = if p.get8(at::NAMED) != 0 {
            text::NOT_FOUND
        } else {
            text::NO_SYSTEM
        };
        Self::say(p, phase, words)
    }

    /// Function `ax` of the target drive, named from `1` in BL and DL.
    fn target_call(p: &mut Program<'_, '_>, ax: u16) -> Frame {
        let drive = u16::from(p.get8(at::TARGET)) + 1;
        Frame::of(&Int {
            bx: drive,
            dx: drive,
            ..Int::dos(p.own, ax)
        })
    }

    /// The target's medium ID read, or with `set` written, through the
    /// record at [`at::MEDIA`].
    fn media_call(p: &mut Program<'_, '_>, set: bool) -> Frame {
        let drive = u16::from(p.get8(at::TARGET)) + 1;
        Frame::of(&Int {
            bx: drive,
            dx: at::MEDIA,
            ..Int::dos(p.own, if set { 0x6901 } else { 0x6900 })
        })
    }

    fn find(p: &mut Program<'_, '_>, phase: &mut u8, path: u16, then: u8) -> Flow {
        *phase = then;
        Flow::Call(Frame::of(&Int {
            cx: SYSTEM_FILE,
            dx: path,
            ..Int::dos(p.own, 0x4E00)
        }))
    }

    /// The size the last find found.
    fn found_size(p: &mut Program<'_, '_>) -> u32 {
        p.get32(at::DTA + 0x1A)
    }

    /// The sectors' call reading or writing the target's boot record.
    fn boot(p: &mut Program<'_, '_>, write: bool) -> Frame {
        let drive = p.get8(at::TARGET);
        let buffer = Far::new(p.own, at::BUFFER);
        absolute(
            p,
            at::PACKET,
            write,
            Sectors {
                drive,
                first: 0,
                count: 1,
                buffer,
            },
        )
    }

    /// The boot record read back as the target holds it.
    fn bpb(p: &mut Program<'_, '_>) -> Option<Bpb> {
        let mut boot = [0u8; BOOT_SECTOR];
        p.g.read_bytes(p.own, at::BUFFER, &mut boot);
        Bpb::parse(&boot)
    }

    /// The boot record written: TAIRiX DOS's code with the target's BPB,
    /// its serial number and label those it has or is given.
    fn write_boot(
        p: &mut Program<'_, '_>,
        phase: &mut u8,
        serial: Option<u32>,
        label: Option<[u8; 11]>,
    ) -> Flow {
        let Some(mut bpb) = Self::bpb(p) else {
            return Self::say(p, phase, text::WRITE_FAILED);
        };
        bpb.serial = bpb.serial.or(serial);
        bpb.label = bpb.label.or(label);
        bpb.drive = if bpb.media == FIXED_MEDIA {
            FIXED_DRIVE
        } else {
            0
        };
        let Ok(boot) = crate::dos::boot::boot_record(&bpb) else {
            return Self::say(p, phase, text::WRITE_FAILED);
        };
        p.g.write_bytes(p.own, at::BUFFER, &boot);
        let mut media = [0u8; crate::dos::device::generic::media::LEN as usize];
        media[2..6].copy_from_slice(&bpb.serial.unwrap_or(0).to_le_bytes());
        media[6..17]
            .copy_from_slice(&boot[tairix_fat::boot::at::LABEL..tairix_fat::boot::at::LABEL + 11]);
        media[17..25].copy_from_slice(bpb.kind.type_name());
        p.g.write_bytes(p.own, at::MEDIA, &media);
        *phase = phase::BOOT_WRITTEN;
        Flow::Call(Self::boot(p, true))
    }
}

impl Routine for Sys {
    const ID: Id = Id::Sys;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's SYS runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(refusal) => {
                        *phase = phase::SAID;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                p.set8(at::TARGET, request.target);
                p.set8(at::NAMED, u8::from(request.source.is_some()));
                set_asciz(
                    p,
                    at::FROM,
                    &source_path(request.source.unwrap_or(b"")),
                    at::FROM_LEN,
                );
                let to = [&[letter(request.target), b':', b'\\'][..], shell::FILE].concat();
                set_asciz(p, at::TO, &to, at::TO_LEN);
                *phase = phase::CURRENT;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
            phase::CURRENT => {
                let current = take!(child).to_le_bytes()[0];
                let from = asciz_at(p, at::FROM, at::FROM_LEN);
                let source = match from.as_slice() {
                    [letter, b':', ..] if p.get8(at::NAMED) != 0 => {
                        letter.to_ascii_uppercase().wrapping_sub(b'A')
                    }
                    _ => current,
                };
                p.set8(at::SOURCE_DRIVE, source);
                if p.get8(at::NAMED) == 0 {
                    if p.get8(at::TARGET) == current {
                        return Self::say(p, phase, text::DEFAULT);
                    }
                    let path = [&[letter(current), b':'][..], &from].concat();
                    set_asciz(p, at::FROM, &path, at::FROM_LEN);
                }
                *phase = phase::ATTRIBUTES;
                Flow::Call(Self::target_call(p, 0x4409))
            }
            phase::ATTRIBUTES => {
                if child.is_ok() && p.result(Returned::Dx) & drive_bits::NETWORK != 0 {
                    return Self::say(p, phase, text::NETWORK);
                }
                *phase = phase::REMOVABLE;
                Flow::Call(Self::target_call(p, 0x4408))
            }
            phase::REMOVABLE => {
                if child == Err(Fault::Error(crate::dos::fs::error::INVALID_DRIVE)) {
                    return Self::say(p, phase, text::BAD_DRIVE);
                }
                let target = p.get8(at::TARGET);
                set_asciz(p, at::DTA, &[letter(target), b':', b'\\'], 4);
                *phase = phase::TRUENAME;
                Flow::Call(Frame::of(&Int {
                    si: at::DTA,
                    di: at::BUFFER,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            phase::TRUENAME => {
                if child.is_err() {
                    return Self::say(p, phase, text::BAD_DRIVE);
                }
                if p.get8(at::BUFFER) != letter(p.get8(at::TARGET)) {
                    return Self::say(p, phase, text::SUBST);
                }
                *phase = phase::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            phase::DTA_SET => Self::find(p, phase, at::FROM, phase::FOUND),
            phase::PRESSED => {
                if take!(child) == ENDED {
                    return Self::missing(p, phase);
                }
                Self::find(p, phase, at::FROM, phase::FOUND)
            }
            phase::FOUND => {
                if child.is_ok() {
                    let size = Self::found_size(p);
                    p.set32(at::SIZE, size);
                    p.set16(at::MEDIA, 0);
                    *phase = phase::MEDIA_ID;
                    return Flow::Call(Self::media_call(p, false));
                }
                *phase = phase::SOURCE_REMOVABLE;
                let source = u16::from(p.get8(at::SOURCE_DRIVE)) + 1;
                Flow::Call(Frame::of(&Int {
                    bx: source,
                    ..Int::dos(p.own, 0x4408)
                }))
            }
            phase::SOURCE_REMOVABLE => {
                if child != Ok(0) {
                    return Self::missing(p, phase);
                }
                let drive = letter(p.get8(at::SOURCE_DRIVE));
                let words = [
                    &b"Insert system disk in drive "[..],
                    &[drive],
                    text::NEW_LINE,
                    text::PRESS_ANY_KEY,
                    text::NEW_LINE,
                ]
                .concat();
                *phase = phase::ASKED;
                Flow::Call(say_error(p, &words))
            }
            phase::ASKED => {
                take!(child);
                *phase = phase::PRESSED;
                Flow::Call(Frame::of(&Key::of(0x0C08)))
            }
            phase::MEDIA_ID => {
                if child.is_ok() {
                    let mut kind = [0u8; 8];
                    p.g.read_bytes(p.own, at::MEDIA + media::FILE_SYSTEM, &mut kind);
                    if ![Kind::Fat12, Kind::Fat16]
                        .iter()
                        .any(|named| named.type_name() == &kind)
                    {
                        let name = parse::trim(&kind);
                        let words =
                            [&b"Not able to SYS to "[..], name, b" file system\r\n"].concat();
                        return Self::say(p, phase, &words);
                    }
                }
                p.set32(at::OLD_SIZE, 0);
                Self::find(p, phase, at::TO, phase::OLD_FOUND)
            }
            phase::OLD_FOUND => {
                if child.is_ok() {
                    let size = Self::found_size(p);
                    p.set32(at::OLD_SIZE, size);
                }
                *phase = phase::SPACE;
                Flow::Call(Self::target_call(p, 0x3600))
            }
            phase::SPACE => {
                let cluster = u32::from(p.result(Returned::Ax)) * u32::from(p.result(Returned::Cx));
                if p.result(Returned::Ax) == 0xFFFF || cluster == 0 {
                    return Self::say(p, phase, text::BAD_DRIVE);
                }
                let free =
                    u32::from(p.result(Returned::Bx)) + p.get32(at::OLD_SIZE).div_ceil(cluster);
                if p.get32(at::SIZE).div_ceil(cluster) > free {
                    return Self::say(p, phase, text::NO_ROOM);
                }
                *phase = phase::PLAIN;
                Flow::Call(Frame::of(&Int {
                    dx: at::TO,
                    ..Int::dos(p.own, 0x4301)
                }))
            }
            phase::PLAIN => {
                *phase = phase::COPIED;
                Flow::Call(Frame::of(&CopyFile::between(
                    at::FROM,
                    at::TO,
                    at::BUFFER,
                    at::BUFFER_LEN,
                )))
            }
            phase::COPIED => match child {
                Ok(_) | Err(Fault::Error(error::SELF)) => {
                    *phase = phase::BOOT_READ;
                    Flow::Call(Self::boot(p, false))
                }
                Err(Fault::Error(error::FULL)) => Self::say(p, phase, text::NO_ROOM),
                Err(Fault::Error(_)) => Self::say(p, phase, text::WRITE_FAILED),
                Err(fault) => Flow::Return(Err(fault)),
            },
            phase::BOOT_READ => {
                if carried(p) {
                    return Self::say(p, phase, text::WRITE_FAILED);
                }
                if Self::bpb(p).is_some_and(|bpb| bpb.serial.is_some()) {
                    return Self::write_boot(p, phase, None, None);
                }
                *phase = phase::CLOCK;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2C00)))
            }
            phase::CLOCK => {
                take!(child);
                let serial = serial_of(p.result(Returned::Cx), p.result(Returned::Dx));
                p.set32(at::CLOCK, serial);
                let mut fcb = [0u8; 44];
                fcb[0] = 0xFF;
                fcb[6] = tairix_fat::dir::attr::VOLUME_ID;
                fcb[7] = p.get8(at::TARGET) + 1;
                fcb[8..19].fill(b'?');
                p.g.write_bytes(p.own, at::FCB, &fcb);
                *phase = phase::LABEL;
                Flow::Call(Frame::of(&Int {
                    dx: at::FCB,
                    ..Int::dos(p.own, 0x1100)
                }))
            }
            phase::LABEL => {
                let label = (take!(child).to_le_bytes()[0] == 0).then(|| {
                    let mut name = [0u8; 11];
                    p.g.read_bytes(p.own, at::DTA + 8, &mut name);
                    name
                });
                let serial = p.get32(at::CLOCK);
                Self::write_boot(p, phase, Some(serial), label)
            }
            phase::BOOT_WRITTEN => {
                if carried(p) {
                    return Self::say(p, phase, text::WRITE_FAILED);
                }
                *phase = phase::MEDIA_SET;
                Flow::Call(Self::media_call(p, true))
            }
            phase::MEDIA_SET => Self::say(p, phase, text::TRANSFERRED),
            _ => Flow::Return(Ok(0)),
        }
    }
}

#[cfg(test)]
#[path = "sys_tests.rs"]
mod tests;
