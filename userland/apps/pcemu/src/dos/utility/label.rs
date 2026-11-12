//! `LABEL [drive:][label]`: a disk's volume label made, changed or
//! deleted. Given none, it shows the volume's and asks for one, and an
//! empty answer offers to delete the label there is. The old label goes
//! through an FCB delete and the new one is made as a file with the label
//! attribute, as DOS's `LABEL` makes it, so the kernel puts it in the boot
//! record too.

use crate::dos::message;
use crate::dos::name::label_field;
use crate::dos::program::{answer_key, say, Flow, Frame, Id, Int, Key, Program, Routine};
use crate::dos::shell::internal::VolumeHeader;
use crate::dos::shell::{answered, letter, parse};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;
use tairix_fat::name::LABEL_LEN;

/// Where `LABEL`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The drive, from `0`, and whether it has a label.
    pub const DRIVE: u16 = STATE;
    pub const LABELLED: u16 = STATE + 0x01;
    /// The new label's field.
    pub const LABEL: u16 = STATE + 0x02;
    /// The DTA and scratch the volume's lines are read through.
    pub const DTA: u16 = STATE + 0x10;
    pub const SCRATCH: u16 = STATE + 0x40;
    /// The extended FCB the old label is deleted through.
    pub const FCB: u16 = STATE + 0x60;
    /// The path the new label is made as, ASCIZ.
    pub const PATH: u16 = STATE + 0x90;
    /// The line asked for: its room, what was typed, the text.
    pub const LINE: u16 = STATE + 0xB0;
    /// Bytes of `LABEL`'s state.
    pub const LEN: u16 = 0xC0;
}

const ASK_DELETE: &[u8] = b"\r\nDelete current volume label (Y/N)? ";
const INVALID: &[u8] = b"Invalid characters in volume label\r\n";
const NO_ENTRY: &[u8] = b"Cannot make directory entry\r\n";

/// What `LABEL`'s command line names: the drive, if any, and the label.
#[must_use]
pub fn request(tail: &[u8]) -> (Option<u8>, Vec<u8>) {
    let text = parse::trim(tail);
    match text {
        [drive, b':', rest @ ..] if drive.is_ascii_alphabetic() => (
            Some(drive.to_ascii_uppercase() - b'A'),
            parse::trim(rest).to_vec(),
        ),
        _ => (None, text.to_vec()),
    }
}

/// The field a typed label makes, cut to eleven bytes: none for one holding
/// a byte a label may not.
#[must_use]
pub fn field(label: &[u8]) -> Option<[u8; 11]> {
    let label = &label[..label.len().min(LABEL_LEN)];
    let refused = |b: &u8| b"*?/\\|.,;:+=[]<>\"".contains(b) || *b < b' ';
    if label.iter().any(refused) {
        return None;
    }
    let mut spelt = label[..label.len().min(8)].to_vec();
    if label.len() > 8 {
        spelt.push(b'.');
        spelt.extend_from_slice(&label[8..]);
    }
    label_field(&spelt)
}

locals! {
    /// `LABEL`'s main routine.
    pub struct Label {}
}

mod phase {
    pub const DRIVE: u8 = 1;
    pub const SHOWN: u8 = 2;
    pub const ASKED: u8 = 3;
    pub const TYPED: u8 = 4;
    pub const ASKED_DELETE: u8 = 5;
    pub const ANSWERED: u8 = 6;
    pub const DELETED: u8 = 7;
    pub const MADE: u8 = 8;
    pub const DONE: u8 = 9;
    pub const CHECKED: u8 = 10;
}

impl Label {
    /// Deletes the drive's label, if it has one: `then` follows.
    fn delete(p: &mut Program<'_, '_>, phase: &mut u8, then: u8) -> Flow {
        let mut fcb = [0u8; 0x2C];
        fcb[0] = 0xFF;
        fcb[6] = 0x08;
        fcb[7] = p.get8(at::DRIVE) + 1;
        fcb[8..19].fill(b'?');
        p.g.write_bytes(p.own, at::FCB, &fcb);
        *phase = then;
        Flow::Call(Frame::of(&Int {
            dx: at::FCB,
            ..Int::dos(p.own, 0x1300)
        }))
    }

    /// Makes the new label, the old one deleted, as a file in the drive's
    /// root with the label attribute.
    fn make(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut field = [b' '; 11];
        p.g.read_bytes(p.own, at::LABEL, &mut field);
        let (base, ext) = (parse::trim(&field[..8]), parse::trim(&field[8..]));
        let mut path = alloc::vec![letter(p.get8(at::DRIVE)), b':', b'\\'];
        path.extend_from_slice(base);
        if !ext.is_empty() {
            path.push(b'.');
            path.extend_from_slice(ext);
        }
        crate::dos::shell::set_asciz(p, at::PATH, &path, 0x20);
        *phase = phase::MADE;
        Flow::Call(Frame::of(&Int {
            cx: 0x08,
            dx: at::PATH,
            ..Int::dos(p.own, 0x5B00)
        }))
    }

    /// The line typed read: a new label, or, where none was typed, whether to
    /// delete the old one asked.
    fn typed(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let typed = usize::from(p.get8(at::LINE + 1)).min(LABEL_LEN);
        let mut label = alloc::vec![0u8; typed];
        p.g.read_bytes(p.own, at::LINE + 2, &mut label);
        let label = parse::trim(&label).to_vec();
        if !label.is_empty() {
            return Self::take_label(p, phase, &label);
        }
        if p.get8(at::LABELLED) == 0 {
            *phase = phase::DONE;
            return Flow::Call(say(p, b"\r\n"));
        }
        *phase = phase::ASKED_DELETE;
        Flow::Call(say(p, ASK_DELETE))
    }

    /// The new label typed or given: refused, or made in place of the old.
    fn take_label(p: &mut Program<'_, '_>, phase: &mut u8, label: &[u8]) -> Flow {
        let Some(field) = field(&label.to_ascii_uppercase()) else {
            *phase = phase::SHOWN;
            return Flow::Call(say(p, INVALID));
        };
        p.g.write_bytes(p.own, at::LABEL, &field);
        Self::delete(p, phase, phase::DELETED)
    }
}

impl Routine for Label {
    const ID: Id = Id::Label;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            return Flow::Return(Ok(1));
        }
        match *phase {
            0 => {
                let tail = p.tail();
                *phase = phase::DRIVE;
                match request(&tail).0 {
                    Some(drive) => self.step(p, phase, Ok(u32::from(drive))),
                    None => Flow::Call(Frame::of(&Int::dos(p.own, 0x1900))),
                }
            }
            phase::DRIVE => {
                let drive = take!(child).to_le_bytes()[0];
                p.set8(at::DRIVE, drive);
                *phase = phase::CHECKED;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(drive) + 1,
                    ..Int::dos(p.own, 0x4409)
                }))
            }
            phase::CHECKED => {
                if child.is_err() {
                    *phase = phase::DONE;
                    return Flow::Call(say(p, crate::dos::shell::text::BAD_DRIVE));
                }
                let tail = p.tail();
                let (_, label) = request(&tail);
                if !label.is_empty() {
                    return Self::take_label(p, phase, &label);
                }
                let drive = p.get8(at::DRIVE);
                *phase = phase::SHOWN;
                Flow::Call(Frame::of(&VolumeHeader::of(drive, at::DTA, at::SCRATCH)))
            }
            phase::SHOWN => {
                match child {
                    Ok(labelled) if labelled <= 1 => {
                        p.set8(at::LABELLED, u8::try_from(labelled).unwrap_or(0));
                    }
                    Ok(_) => {}
                    Err(_) => return Flow::Return(Ok(1)),
                }
                *phase = phase::ASKED;
                Flow::Call(say(p, message::ASK_LABEL))
            }
            phase::ASKED => {
                take!(child);
                p.set8(at::LINE, u8::try_from(LABEL_LEN + 1).unwrap_or(0));
                *phase = phase::TYPED;
                Flow::Call(Frame::of(&Int {
                    dx: at::LINE,
                    ..Int::dos(p.own, 0x0A00)
                }))
            }
            phase::TYPED => {
                take!(child);
                Self::typed(p, phase)
            }
            phase::ASKED_DELETE => {
                take!(child);
                *phase = phase::ANSWERED;
                Flow::Call(Frame::of(&Key::of(0x0C01)))
            }
            // An input with no answer left answers no.
            phase::ANSWERED => match answer_key(take!(child)) {
                b'Y' => Self::delete(p, phase, phase::DONE),
                b'N' => {
                    *phase = phase::DONE;
                    Flow::Call(say(p, b"\r\n"))
                }
                _ => {
                    *phase = phase::ASKED_DELETE;
                    Flow::Call(say(p, ASK_DELETE))
                }
            },
            phase::DELETED => Self::make(p, phase),
            phase::MADE => {
                let Ok(handle) = child else {
                    *phase = phase::DONE;
                    return Flow::Call(say(p, NO_ENTRY));
                };
                *phase = phase::DONE;
                Flow::Call(Frame::of(&Int {
                    bx: u16::try_from(handle).unwrap_or(0),
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}
