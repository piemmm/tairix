//! A diskette copied: passes of as many cylinders as memory holds, each
//! read from the source and then written to the target, a side of a
//! cylinder a call.

use super::{
    at, exit, flag, has, ioctl, press, set_flag, soft, text, track_request, DiskCopyFormat,
    DiskCopyPress, DiskCopySource, DiskCopyTarget,
};
use crate::dos::device::generic::{self, parameters};
use crate::dos::message::{self, decimal};
use crate::dos::program::{say, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::letter;
use crate::frames::{locals, take, Far, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::boot::{at as boot_at, EXTENDED, EXTENDED_SERIAL_ONLY};

/// The serial number DOS 4.0's `DISKCOPY` gives a copy, from function
/// `2Ah`'s CX and DX and function `2Ch`'s: the words the other way round
/// from `FORMAT`'s.
#[must_use]
pub fn serial(date: (u16, u16), time: (u16, u16)) -> u32 {
    u32::from(date.0.wrapping_add(time.0)) << 16 | u32::from(date.1.wrapping_add(time.1))
}

/// Whether the boot record `boot` has a serial number to give the copy a
/// new one in: a FAT medium's, extended.
fn has_serial(boot: &[u8; 0x30]) -> bool {
    boot[boot_at::MEDIA] & 0xF0 == 0xF0
        && matches!(boot[boot_at::EXTENDED], EXTENDED | EXTENDED_SERIAL_ONLY)
}

locals! {
    /// One diskette copied, as DOS 4.0's `COPY` copies it: a pass of
    /// reading, then of writing, until the last cylinder is written or the
    /// copy cannot go on; the code it ends with.
    pub struct DiskCopyDisk {}
}

mod disk {
    pub const STARTED: u8 = 1;
    pub const READ: u8 = 2;
    pub const WRITTEN: u8 = 3;
    pub const SAID: u8 = 4;
}

impl DiskCopyDisk {
    fn read(phase: &mut u8) -> Flow {
        *phase = disk::READ;
        Flow::Call(Frame::of(&DiskCopyPass::reading()))
    }

    /// What the copy ends saying: that it ended, or what was lost and the
    /// serial number the copy was given.
    fn end(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let words = if has(p, flag::FATAL) {
            text::ENDED.to_vec()
        } else {
            let mut words = Vec::new();
            if p.get16(at::ERRORS) != 0 {
                words.extend_from_slice(text::MAY_BE_UNUSABLE);
            }
            if has(p, flag::SERIAL) {
                words.extend_from_slice(text::NEW_LINE);
                words.extend_from_slice(&message::serial_line(p.get32(at::SERIAL)));
            }
            words
        };
        *phase = disk::SAID;
        Flow::Call(say(p, &words))
    }
}

impl Routine for DiskCopyDisk {
    const ID: Id = Id::DiskCopyDisk;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                p.set16(at::ERRORS, 0);
                set_flag(p, flag::FORMATTING | flag::FATAL | flag::SERIAL, false);
                p.set8(at::TO_READ, 0);
                p.set8(at::TO_WRITE, 0);
                let bytes = p.get16(at::SOURCE_DRIVE + parameters::BPB);
                p.set16(at::SECTOR_BYTES, bytes);
                if has(p, flag::SINGLE) {
                    return Self::read(phase);
                }
                *phase = disk::STARTED;
                Flow::Call(Frame::of(&DiskCopyPress::of(press::BOTH, at::SOURCE)))
            }
            disk::STARTED => {
                take!(child);
                Self::read(phase)
            }
            disk::READ => {
                take!(child);
                if has(p, flag::FATAL) {
                    return Self::end(p, phase);
                }
                *phase = disk::WRITTEN;
                Flow::Call(Frame::of(&DiskCopyPass::writing()))
            }
            disk::WRITTEN => {
                take!(child);
                if has(p, flag::FATAL) || p.get8(at::TO_WRITE) > p.get8(at::LAST_TRACK) {
                    return Self::end(p, phase);
                }
                Self::read(phase)
            }
            _ => {
                take!(child);
                let code = if has(p, flag::FATAL) {
                    exit::FATAL
                } else if p.get16(at::ERRORS) != 0 {
                    exit::TRACKS_LOST
                } else {
                    exit::COPIED
                };
                Flow::Return(Ok(u32::from(code)))
            }
        }
    }
}

locals! {
    /// A pass over the cylinders memory holds, read from the source or
    /// written to the target: the disk asked for first when the two take
    /// turns in one drive, and the disk checked when the pass is the first.
    pub struct DiskCopyPass {
        write: bool,
    }
}

impl DiskCopyPass {
    #[must_use]
    pub const fn reading() -> Self {
        Self { write: false }
    }

    #[must_use]
    pub const fn writing() -> Self {
        Self { write: true }
    }

    fn reached(self) -> u16 {
        if self.write {
            at::TO_WRITE
        } else {
            at::TO_READ
        }
    }

    /// The pass's cylinders from the first: checked first on the first pass.
    fn begin(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if p.get8(self.reached()) != 0 {
            return self.place(p, phase);
        }
        *phase = pass::CHECKED;
        Flow::Call(if self.write {
            Frame::of(&DiskCopyTarget::default())
        } else {
            Frame::of(&DiskCopySource::default())
        })
    }

    /// The source is known: a cylinder's paragraphs, and whether memory
    /// holds one.
    fn sized(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let sides = u32::from(p.get8(at::LAST_SIDE)) + 1;
        let sector = u32::from(p.get16(at::SECTOR_BYTES)).div_ceil(16);
        let cylinder = sides * u32::from(p.get8(at::SECTORS)) * sector;
        p.set16(at::CYLINDER, u16::try_from(cylinder).unwrap_or(u16::MAX));
        if u32::from(p.get16(at::ROOM)) < cylinder {
            set_flag(p, flag::FATAL, true);
            *phase = pass::DONE;
            return Flow::Call(say(p, &[text::NEW_LINE, text::MEMORY].concat()));
        }
        self.place(p, phase)
    }

    fn place(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let buffer = p.get16(at::BUFFER);
        p.set16(at::PLACE, buffer);
        self.next(p, phase)
    }

    /// The next cylinder, while the disk and the buffer have one more.
    fn next(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let end = u32::from(p.get16(at::BUFFER)) + u32::from(p.get16(at::ROOM));
        let after = u32::from(p.get16(at::PLACE)) + u32::from(p.get16(at::CYLINDER));
        if p.get8(self.reached()) > p.get8(at::LAST_TRACK) || after > end {
            return Flow::Return(Ok(0));
        }
        p.set8(at::SIDE, 0);
        self.side(phase)
    }

    fn side(self, phase: &mut u8) -> Flow {
        *phase = pass::TRACK;
        Flow::Call(Frame::of(&DiskCopyTrack::of(self.write)))
    }
}

mod pass {
    pub const ASKED: u8 = 1;
    pub const CHECKED: u8 = 2;
    pub const TRACK: u8 = 3;
    pub const DONE: u8 = 4;
}

impl Routine for DiskCopyPass {
    const ID: Id = Id::DiskCopyPass;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let this = *self;
        match *phase {
            0 => {
                if !has(p, flag::SINGLE) {
                    return this.begin(p, phase);
                }
                *phase = pass::ASKED;
                let (what, drive) = if this.write {
                    (press::TARGET, at::TARGET)
                } else {
                    (press::SOURCE, at::SOURCE)
                };
                Flow::Call(Frame::of(&DiskCopyPress::of(what, drive)))
            }
            pass::ASKED => {
                take!(child);
                this.begin(p, phase)
            }
            pass::CHECKED => {
                take!(child);
                if has(p, flag::FATAL) {
                    return Flow::Return(Ok(0));
                }
                if this.write {
                    return this.place(p, phase);
                }
                this.sized(p, phase)
            }
            pass::TRACK => {
                take!(child);
                if has(p, flag::FATAL) {
                    return Flow::Return(Ok(0));
                }
                let sides = u16::from(p.get8(at::LAST_SIDE)) + 1;
                let place = p.get16(at::PLACE) + p.get16(at::CYLINDER) / sides;
                p.set16(at::PLACE, place);
                let side = p.get8(at::SIDE) + 1;
                p.set8(at::SIDE, side);
                if side <= p.get8(at::LAST_SIDE) {
                    return this.side(phase);
                }
                let reached = p.get8(this.reached()) + 1;
                p.set8(this.reached(), reached);
                this.next(p, phase)
            }
            _ => Flow::Return(child.map(|_| 0)),
        }
    }
}

locals! {
    /// One side of a cylinder read into the buffer or written from it: a
    /// drive the user can put right asked about and the track tried again,
    /// a track that cannot be read said and counted, and a target that
    /// cannot be written formatted from there on, once, before the track is
    /// tried again. The copy of the first track gets a serial number of
    /// its own.
    pub struct DiskCopyTrack {
        write: bool,
        date_cx: u16,
        date_dx: u16,
    }
}

impl DiskCopyTrack {
    #[must_use]
    pub const fn of(write: bool) -> Self {
        Self {
            write,
            date_cx: 0,
            date_dx: 0,
        }
    }

    fn drive(self) -> u16 {
        if self.write {
            at::TARGET
        } else {
            at::SOURCE
        }
    }

    fn cylinder(self, p: &mut Program<'_, '_>) -> u8 {
        p.get8(if self.write {
            at::TO_WRITE
        } else {
            at::TO_READ
        })
    }

    fn attempt(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (side, cylinder, sectors) = (p.get8(at::SIDE), self.cylinder(p), p.get8(at::SECTORS));
        let place = p.get16(at::PLACE);
        track_request(p, side, cylinder, 0, u16::from(sectors), Far::new(place, 0));
        *phase = track::DONE;
        let function = if self.write {
            generic::WRITE_TRACK
        } else {
            generic::READ_TRACK
        };
        Flow::Call(ioctl(p, self.drive(), function, at::PACKET))
    }

    /// The track that could not be read or written, said: the drive, the
    /// side and the cylinder.
    fn lost(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let errors = p.get16(at::ERRORS).saturating_add(1);
        p.set16(at::ERRORS, errors);
        let lead = if self.write {
            text::WRITE_ERROR
        } else {
            text::READ_ERROR
        };
        let drive = letter(p.get8(self.drive()));
        let (side, cylinder) = (u32::from(p.get8(at::SIDE)), u32::from(self.cylinder(p)));
        let words = [
            lead,
            &[drive],
            text::NEW_LINE,
            b"Side ",
            &decimal(side),
            b", track ",
            &decimal(cylinder),
            text::NEW_LINE,
        ]
        .concat();
        *phase = track::SAID;
        Flow::Call(say(p, &words))
    }

    /// The first track read: its boot record given a new serial number,
    /// if it has one.
    fn first_read(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut boot = [0u8; 0x30];
        let place = p.get16(at::PLACE);
        p.g.read_bytes(place, 0, &mut boot);
        if !has_serial(&boot) {
            return Flow::Return(Ok(0));
        }
        *phase = track::DATE;
        Flow::Call(Frame::of(&Int::dos(p.own, 0x2A00)))
    }
}

mod track {
    pub const DONE: u8 = 1;
    pub const PRESSED: u8 = 2;
    pub const SAID: u8 = 3;
    pub const FORMAT_SAID: u8 = 4;
    pub const FORMATTED: u8 = 5;
    pub const DATE: u8 = 6;
    pub const TIME: u8 = 7;
}

impl Routine for DiskCopyTrack {
    const ID: Id = Id::DiskCopyTrack;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let this = *self;
        match *phase {
            track::DONE => match child {
                Ok(_) if !this.write && this.cylinder(p) == 0 && p.get8(at::SIDE) == 0 => {
                    Self::first_read(p, phase)
                }
                Ok(_) => Flow::Return(Ok(0)),
                Err(Fault::Error(code)) => {
                    if let Some(what) = soft(code) {
                        *phase = track::PRESSED;
                        return Flow::Call(Frame::of(&DiskCopyPress::of(what, this.drive())));
                    }
                    if !this.write || has(p, flag::FORMATTING) {
                        return this.lost(p, phase);
                    }
                    set_flag(p, flag::FORMATTING, true);
                    *phase = track::FORMAT_SAID;
                    Flow::Call(say(p, text::FORMATTING))
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            track::PRESSED => {
                take!(child);
                this.attempt(p, phase)
            }
            track::FORMAT_SAID => {
                take!(child);
                *phase = track::FORMATTED;
                Flow::Call(Frame::of(&DiskCopyFormat::default()))
            }
            track::FORMATTED => {
                take!(child);
                if has(p, flag::FATAL) {
                    return Flow::Return(Ok(0));
                }
                this.attempt(p, phase)
            }
            track::DATE => {
                take!(child);
                self.date_cx = p.result(Returned::Cx);
                self.date_dx = p.result(Returned::Dx);
                *phase = track::TIME;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2C00)))
            }
            track::TIME => {
                take!(child);
                let time = (p.result(Returned::Cx), p.result(Returned::Dx));
                let serial = serial((self.date_cx, self.date_dx), time);
                let place = p.get16(at::PLACE);
                p.g.write_bytes(
                    place,
                    u16::try_from(boot_at::SERIAL).unwrap_or(0),
                    &serial.to_le_bytes(),
                );
                p.set32(at::SERIAL, serial);
                set_flag(p, flag::SERIAL, true);
                Flow::Return(Ok(0))
            }
            track::SAID => Flow::Return(child.map(|_| 0)),
            _ => this.attempt(p, phase),
        }
    }
}
