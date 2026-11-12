//! What `FORMAT` writes and reads whole: the new volume's boot record,
//! FATs and root, through INT 26h, the drive's own parameters put back
//! after; `COMMAND.COM`, read into memory before the disk is asked for and
//! written to it after.

use super::{
    at, exit, flag, has, over_progress, serial, set_parameters, text, volume_of, FormatAsk,
};
use crate::dos::boot::{boot_record, non_system_record};
use crate::dos::device::generic::parameters;
use crate::dos::program::{say, Flow, Frame, Id, Int, Program, Returned, Routine, ENDED};
use crate::dos::shell::{self, asciz_at, letter, program_variable, set_asciz};
use crate::dos::utility::{absolute, carried, Sectors};
use crate::frames::{locals, take, Far, Fault, Outcome};
use tairix_fat::boot::BOOT_SECTOR;
use tairix_fat::name::{label_field, NO_LABEL};

/// The sectors one INT 26h writes at most, and the paragraphs they take.
const CHUNK: u32 = 0x40;
const CHUNK_PARAGRAPHS: u16 = 0x40 * 32;
/// The bytes one read or write of the system's file moves at most.
const FILE_CHUNK: u32 = 0x8000;

/// What writing the volume came to.
pub mod written {
    pub const OK: u32 = 0;
    /// The boot record could not be written: the disk is unusable.
    pub const BOOT: u32 = 1;
    /// A FAT or the root could not be: `FORMAT` ends, having said so.
    pub const FATAL: u32 = 2;
}

/// The call writing piece `piece` of the run of `count` sectors from
/// `first` the memory from `segment` holds, if the run has one.
fn write_piece(
    p: &mut Program<'_, '_>,
    first: u32,
    count: u32,
    segment: u16,
    piece: u16,
) -> Option<Frame> {
    let from = u32::from(piece) * CHUNK;
    let left = count.checked_sub(from).filter(|&left| left > 0)?;
    let drive = p.get8(at::DRIVE);
    let buffer = Far::new(
        segment.wrapping_add(piece.wrapping_mul(CHUNK_PARAGRAPHS)),
        0,
    );
    let count = u16::try_from(left.min(CHUNK)).unwrap_or(0);
    Some(absolute(
        p,
        at::ABSOLUTE,
        true,
        Sectors {
            drive,
            first: first + from,
            count,
            buffer,
        },
    ))
}

locals! {
    /// The volume written: its boot record, with a serial number made
    /// from the date and time, each FAT, and the root; then the drive's own
    /// parameters put back and DOS's buffers flushed. A [`written`].
    pub struct FormatWrite {
        date_cx: u16,
        date_dx: u16,
        copy: u8,
        piece: u16,
    }
}

mod phase {
    pub const DATE: u8 = 1;
    pub const TIME: u8 = 2;
    pub const BOOT: u8 = 3;
    pub const FAT: u8 = 4;
    pub const ROOT: u8 = 5;
    pub const RESTORED: u8 = 6;
    pub const RESET: u8 = 7;
    pub const SAID: u8 = 8;
}

impl FormatWrite {
    /// The next piece of the FATs, or once they are written the root's.
    fn next_fat(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some(bpb) = volume_of(p) else {
            return Flow::Return(Ok(written::FATAL));
        };
        while self.copy < bpb.fats {
            let first = u32::from(bpb.reserved_sectors) + u32::from(self.copy) * bpb.fat_sectors;
            let fat = p.get16(at::FAT);
            if let Some(call) = write_piece(p, first, bpb.fat_sectors, fat, self.piece) {
                self.piece += 1;
                *phase = phase::FAT;
                return Flow::Call(call);
            }
            self.copy += 1;
            self.piece = 0;
        }
        self.next_root(p, phase)
    }

    fn next_root(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some(bpb) = volume_of(p) else {
            return Flow::Return(Ok(written::FATAL));
        };
        let first = u32::try_from(bpb.first_data_sector()).unwrap_or(0) - bpb.root_sectors();
        let root = p.get16(at::ROOT);
        if let Some(call) = write_piece(p, first, bpb.root_sectors(), root, self.piece) {
            self.piece += 1;
            *phase = phase::ROOT;
            return Flow::Call(call);
        }
        p.set16(at::SAVED + parameters::LAYOUT_ENTRIES, 0);
        *phase = phase::RESTORED;
        Flow::Call(set_parameters(p, at::SAVED, parameters::GOOD_LAYOUT))
    }

    fn fatal(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        p.set8(at::STATUS, exit::FATAL);
        *phase = phase::SAID;
        Flow::Call(say(p, words))
    }
}

impl Routine for FormatWrite {
    const ID: Id = Id::FormatWrite;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::DATE => {
                take!(child);
                (self.date_cx, self.date_dx) = (p.result(Returned::Cx), p.result(Returned::Dx));
                *phase = phase::TIME;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2C00)))
            }
            phase::TIME => {
                take!(child);
                let time = (p.result(Returned::Cx), p.result(Returned::Dx));
                let serial = serial((self.date_cx, self.date_dx), time);
                p.set32(at::SERIAL, serial);
                let Some(mut bpb) = volume_of(p) else {
                    return Flow::Return(Ok(written::BOOT));
                };
                bpb.serial = Some(serial);
                bpb.label = Some(label_field(NO_LABEL));
                bpb.drive = if has(p, flag::FIXED) { 0x80 } else { 0 };
                let record = if super::switched(p, super::switch::S) {
                    boot_record(&bpb)
                } else {
                    non_system_record(&bpb)
                };
                let Ok(record) = record else {
                    return Flow::Return(Ok(written::BOOT));
                };
                p.g.write_bytes(p.own, at::BUFFER, &record[..BOOT_SECTOR]);
                let drive = p.get8(at::DRIVE);
                let buffer = Far::new(p.own, at::BUFFER);
                *phase = phase::BOOT;
                Flow::Call(absolute(
                    p,
                    at::ABSOLUTE,
                    true,
                    Sectors {
                        drive,
                        first: 0,
                        count: 1,
                        buffer,
                    },
                ))
            }
            phase::BOOT => {
                take!(child);
                if carried(p) {
                    *phase = phase::SAID;
                    p.set32(at::SERIAL, 0);
                    return Flow::Call(say(p, &over_progress(text::BOOT, 44, b"\r\n")));
                }
                self.next_fat(p, phase)
            }
            phase::FAT => {
                take!(child);
                if carried(p) {
                    return Self::fatal(p, phase, text::FAT);
                }
                self.next_fat(p, phase)
            }
            phase::ROOT => {
                take!(child);
                if carried(p) {
                    return Self::fatal(p, phase, text::DIRECTORY);
                }
                self.next_root(p, phase)
            }
            phase::RESTORED => {
                take!(child);
                *phase = phase::RESET;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0D00)))
            }
            phase::RESET => {
                take!(child);
                Flow::Return(Ok(written::OK))
            }
            phase::SAID => {
                take!(child);
                let failed = if p.get8(at::STATUS) == exit::FATAL {
                    written::FATAL
                } else {
                    written::BOOT
                };
                Flow::Return(Ok(failed))
            }
            _ => {
                *phase = phase::DATE;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2A00)))
            }
        }
    }
}

locals! {
    /// `COMMAND.COM` written to the new volume's root from the memory
    /// [`FormatSource`] read it into, its date and time kept: `1`, and the
    /// bytes it takes on the disk counted as the system's.
    pub struct FormatSystem {
        handle: u16,
        written: u32,
        count: u16,
    }
}

mod system {
    pub const CREATED: u8 = 1;
    pub const WRITTEN: u8 = 2;
    pub const STAMPED: u8 = 3;
    pub const CLOSED: u8 = 4;
    pub const ABANDONED: u8 = 5;
}

impl FormatSystem {
    fn next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let left = p.get32(at::SYSTEM_SIZE).saturating_sub(self.written);
        if left == 0 {
            *phase = system::STAMPED;
            let (time, date) = (p.get16(at::SYSTEM_TIME), p.get16(at::SYSTEM_DATE));
            return Flow::Call(Frame::of(&Int {
                bx: self.handle,
                cx: time,
                dx: date,
                ..Int::dos(p.own, 0x5701)
            }));
        }
        self.count = u16::try_from(left.min(FILE_CHUNK)).unwrap_or(0);
        let segment = p
            .get16(at::SYSTEM)
            .wrapping_add(u16::try_from(self.written / 16).unwrap_or(0));
        *phase = system::WRITTEN;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            cx: self.count,
            dx: 0,
            ds: segment,
            ..Int::dos(p.own, 0x4000)
        }))
    }
}

impl Routine for FormatSystem {
    const ID: Id = Id::FormatSystem;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            system::CREATED => {
                self.handle = u16::try_from(take!(child)).unwrap_or(0);
                self.next(p, phase)
            }
            system::WRITTEN => {
                if child != Ok(u32::from(self.count)) {
                    *phase = system::ABANDONED;
                    return Flow::Call(Frame::of(&Int {
                        bx: self.handle,
                        ..Int::dos(p.own, 0x3E00)
                    }));
                }
                self.written += u32::from(self.count);
                self.next(p, phase)
            }
            system::STAMPED => {
                take!(child);
                *phase = system::CLOSED;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x3E00)
                }))
            }
            system::CLOSED => {
                take!(child);
                let cluster = volume_of(p).map_or(512, |bpb| {
                    u32::from(bpb.sectors_per_cluster) * u32::from(bpb.bytes_per_sector)
                });
                let bytes = self.written.div_ceil(cluster.max(1)) * cluster;
                p.set32(at::SYSTEM_BYTES, bytes);
                Flow::Return(Ok(1))
            }
            system::ABANDONED => {
                Flow::Return(Err(Fault::Error(crate::dos::fs::error::ACCESS_DENIED)))
            }
            _ => {
                let drive = letter(p.get8(at::DRIVE));
                p.g.write_bytes(
                    p.own,
                    at::BUFFER,
                    &[&[drive, b':', b'\\'][..], shell::FILE, b"\0"].concat(),
                );
                *phase = system::CREATED;
                Flow::Call(Frame::of(&Int {
                    cx: 0,
                    dx: at::BUFFER,
                    ..Int::dos(p.own, 0x3C00)
                }))
            }
        }
    }
}

locals! {
    /// `COMMAND.COM` read whole into memory of its own for `/S`, with its
    /// date and time: from the drive DOS started from, where `COMSPEC`
    /// names it there or in its root. One not found on a removable disk is
    /// asked for and looked for again; one not to be had says why, and
    /// fails.
    pub struct FormatSource {
        handle: u16,
        read: u32,
        count: u16,
    }
}

mod source {
    pub const BOOT: u8 = 1;
    pub const OPENED: u8 = 2;
    pub const REMOVABLE: u8 = 3;
    pub const ASKED: u8 = 4;
    pub const TYPED: u8 = 5;
    pub const PROMPTED: u8 = 6;
    pub const STAMP: u8 = 7;
    pub const SIZED: u8 = 8;
    pub const BACK: u8 = 9;
    pub const TAKEN: u8 = 10;
    pub const READ: u8 = 11;
    pub const CLOSED: u8 = 12;
    pub const SAID: u8 = 13;
}

impl FormatSource {
    fn open(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = source::OPENED;
        Flow::Call(Frame::of(&Int {
            dx: at::SYSTEM_PATH,
            ..Int::dos(p.own, 0x3D00)
        }))
    }

    fn next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let left = p.get32(at::SYSTEM_SIZE).saturating_sub(self.read);
        if left == 0 {
            *phase = source::CLOSED;
            return Flow::Call(Frame::of(&Int {
                bx: self.handle,
                ..Int::dos(p.own, 0x3E00)
            }));
        }
        self.count = u16::try_from(left.min(FILE_CHUNK)).unwrap_or(0);
        let segment = p
            .get16(at::SYSTEM)
            .wrapping_add(u16::try_from(self.read / 16).unwrap_or(0));
        *phase = source::READ;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            cx: self.count,
            dx: 0,
            ds: segment,
            ..Int::dos(p.own, 0x3F00)
        }))
    }

    /// What opening the system file answered: its stamp asked for, or, where
    /// it could not be opened, whether its drive's medium can be changed.
    fn opened(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match child {
            Ok(handle) => {
                self.handle = u16::try_from(handle).unwrap_or(0);
                *phase = source::STAMP;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x5700)
                }))
            }
            Err(Fault::Break) => Flow::Return(Err(Fault::Break)),
            Err(_) => {
                let drive = asciz_at(p, at::SYSTEM_PATH, at::PATH_LEN)
                    .first()
                    .map_or(0, |d| d.to_ascii_uppercase().wrapping_sub(b'A'));
                *phase = source::REMOVABLE;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(drive) + 1,
                    ..Int::dos(p.own, 0x4408)
                }))
            }
        }
    }

    /// A system disk asked for where the drive's medium can be changed:
    /// `child` is whether it is removable, `0` for yes.
    fn ask_for_system(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child != Ok(0) {
            return Self::fail(p, phase, text::NO_SYSTEM);
        }
        let drive = asciz_at(p, at::SYSTEM_PATH, at::PATH_LEN)
            .first()
            .map_or(b'A', u8::to_ascii_uppercase);
        *phase = source::ASKED;
        Flow::Call(say(
            p,
            &[text::INSERT_DOS, &[drive, b':'], b"\r\n", text::ENTER].concat(),
        ))
    }

    /// Says `words`, which end the reading: `FORMAT` cannot go on.
    fn fail(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        *phase = source::SAID;
        Flow::Call(say(p, words))
    }
}

impl Routine for FormatSource {
    const ID: Id = Id::FormatSource;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            source::BOOT => {
                take!(child);
                let boot = letter(p.result(Returned::Dx).to_le_bytes()[0].saturating_sub(1));
                let named = program_variable(p, b"COMSPEC")
                    .filter(|path| path.first().is_some_and(|d| d.to_ascii_uppercase() == boot));
                let path =
                    named.unwrap_or_else(|| [&[boot, b':', b'\\'][..], shell::FILE].concat());
                set_asciz(p, at::SYSTEM_PATH, &path, at::PATH_LEN);
                Self::open(p, phase)
            }
            source::OPENED => self.opened(p, phase, child),
            source::REMOVABLE => Self::ask_for_system(p, phase, child),
            source::ASKED => {
                take!(child);
                *phase = source::TYPED;
                Flow::Call(Frame::of(&FormatAsk::line()))
            }
            source::TYPED => {
                if take!(child) == ENDED {
                    return Self::fail(p, phase, text::NO_SYSTEM);
                }
                *phase = source::PROMPTED;
                Flow::Call(say(p, b"\r\n\r\n"))
            }
            source::PROMPTED => {
                take!(child);
                Self::open(p, phase)
            }
            source::STAMP => {
                take!(child);
                let (time, date) = (p.result(Returned::Cx), p.result(Returned::Dx));
                p.set16(at::SYSTEM_TIME, time);
                p.set16(at::SYSTEM_DATE, date);
                *phase = source::SIZED;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x4202)
                }))
            }
            source::SIZED => {
                take!(child);
                let size =
                    u32::from(p.result(Returned::Dx)) << 16 | u32::from(p.result(Returned::Ax));
                p.set32(at::SYSTEM_SIZE, size);
                *phase = source::BACK;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x4200)
                }))
            }
            source::BACK => {
                take!(child);
                let paragraphs = u16::try_from(p.get32(at::SYSTEM_SIZE).div_ceil(16))
                    .unwrap_or(u16::MAX)
                    .max(1);
                *phase = source::TAKEN;
                Flow::Call(Frame::of(&Int {
                    bx: paragraphs,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            source::TAKEN => match child {
                Ok(segment) => {
                    p.set16(at::SYSTEM, u16::try_from(segment).unwrap_or(0));
                    self.next(p, phase)
                }
                Err(Fault::Break) => Flow::Return(Err(Fault::Break)),
                Err(_) => Self::fail(p, phase, text::MEMORY),
            },
            source::READ => {
                let got = take!(child);
                self.read += got;
                if got < u32::from(self.count) {
                    p.set32(at::SYSTEM_SIZE, self.read);
                }
                self.next(p, phase)
            }
            source::CLOSED => {
                take!(child);
                Flow::Return(Ok(0))
            }
            source::SAID => {
                take!(child);
                Flow::Return(Err(Fault::Error(crate::dos::fs::error::FILE_NOT_FOUND)))
            }
            _ => {
                *phase = source::BOOT;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x3305)))
            }
        }
    }
}
