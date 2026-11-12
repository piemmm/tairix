//! Programs: loading one — a COM image whole, an MZ image relocated, its
//! minimum and maximum allocation honoured — with its PSP and environment,
//! starting it, and ending it: its handles closed, its memory freed, its
//! vectors put back, and its parent's EXEC call answered.

use super::data::{level, sda, sft, SEGMENT};
use super::engine::{
    self, locals, take, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine, Saved,
};
use super::file::{access, Close, Open, Read, CLOSED};
use super::fs::{error, written_back, FREE};
use super::memory::psp;
use crate::asm::Reg16;
use crate::cpu::{flag, Seg};

/// EXEC's modes.
pub mod mode {
    /// Load and run.
    pub const RUN: u8 = 0x00;
    /// Load, and hand back where it would start.
    pub const LOAD: u8 = 0x01;
    /// Load an overlay at a given segment.
    pub const OVERLAY: u8 = 0x03;
    /// Sets the state a program about to be run is to start in; the
    /// kernel keeps none, so it is accepted as done.
    pub const SET_STATE: u8 = 0x05;
}

/// How a process ended, as function `4Dh` reports it in AH.
pub mod ending {
    /// It ended itself.
    pub const NORMAL: u8 = 0;
    /// Ctrl+C ended it.
    pub const BREAK: u8 = 1;
    /// An INT 24h handler's abort ended it.
    pub const CRITICAL: u8 = 2;
    /// It stays resident.
    pub const RESIDENT: u8 = 3;
}

/// An MZ header's fields.
mod mz {
    pub const LAST_PAGE: usize = 0x02;
    pub const PAGES: usize = 0x04;
    pub const RELOCATIONS: usize = 0x06;
    pub const HEADER: usize = 0x08;
    pub const MIN_ALLOC: usize = 0x0A;
    pub const MAX_ALLOC: usize = 0x0C;
    pub const SS: usize = 0x0E;
    pub const SP: usize = 0x10;
    pub const IP: usize = 0x14;
    pub const CS: usize = 0x16;
    pub const TABLE: usize = 0x18;
    /// Bytes of the header read.
    pub const LEN: usize = 0x1C;
}

/// An executable's shape, decoded from its first bytes and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    /// Whether it is an MZ image.
    pub mz: bool,
    /// Where the image starts in the file.
    pub start: u32,
    /// Its bytes.
    pub size: u32,
    /// Relocations, and where their table lies in the file.
    pub relocations: u16,
    /// Where the relocation table lies.
    pub table: u16,
    /// Paragraphs needed beyond the image, and wanted.
    pub min_alloc: u16,
    /// Paragraphs wanted beyond the image.
    pub max_alloc: u16,
    /// The entry, relative to the load segment.
    pub cs: u16,
    /// The entry's offset.
    pub ip: u16,
    /// The stack, relative to the load segment.
    pub ss: u16,
    /// The stack pointer.
    pub sp: u16,
}

/// The largest COM image: what fits a segment after its PSP, less a word of
/// stack.
const COM_MAX: u32 = 0xFF00 - 2;

/// The executable whose first bytes are `head` and size `size`, or `None`
/// for a file no program is: an MZ header whose image lies beyond the file
/// or exceeds a megabyte, or a COM file too big for a segment.
#[must_use]
pub fn image(head: &[u8; mz::LEN], size: u32) -> Option<Image> {
    let word = |at: usize| u16::from_le_bytes([head[at], head[at + 1]]);
    if &head[..2] != b"MZ" && &head[..2] != b"ZM" {
        if size > COM_MAX {
            return None;
        }
        return Some(Image {
            mz: false,
            start: 0,
            size,
            relocations: 0,
            table: 0,
            min_alloc: 0,
            max_alloc: 0xFFFF,
            cs: 0,
            ip: psp::LEN,
            ss: 0,
            sp: 0xFFFE,
        });
    }
    let pages = u32::from(word(mz::PAGES));
    let last = u32::from(word(mz::LAST_PAGE) & 0x1FF);
    let total = if last == 0 {
        pages * 512
    } else {
        pages.checked_sub(1)? * 512 + last
    };
    let start = u32::from(word(mz::HEADER)) * 16;
    let image_size = total.checked_sub(start)?;
    if total > size.saturating_add(512) || image_size > 0xA_0000 {
        return None;
    }
    Some(Image {
        mz: true,
        start,
        size: image_size.min(size.saturating_sub(start)),
        relocations: word(mz::RELOCATIONS),
        table: word(mz::TABLE),
        min_alloc: word(mz::MIN_ALLOC),
        max_alloc: word(mz::MAX_ALLOC),
        cs: word(mz::CS),
        ip: word(mz::IP),
        ss: word(mz::SS),
        sp: word(mz::SP),
    })
}

/// Paragraphs `bytes` take.
const fn paragraphs(bytes: u32) -> u32 {
    bytes.div_ceil(16)
}

locals! {
    /// EXEC (function `4Bh`): loads the running call's path in `mode`, the
    /// parameter block at `block`.
    pub struct Exec {
        mode: u8,
        block: Far,
        file: u8,
        environment: u16,
        psp: u16,
        load: u16,
        done: u32,
        relocated: u16,
        code: u16,
    }
}

const OPENED: u8 = 1;
const HEAD_READ: u8 = 2;
const LOADED: u8 = 4;
const TABLE_READ: u8 = 5;
const CLOSED_FILE: u8 = 6;
/// A process has ended and every buffer is being written back.
const WRITTEN_BACK: u8 = 7;
const FAILED: u8 = 7;

impl Routine for Exec {
    const ID: Id = Id::Exec;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            OPENED => {
                self.file = u8::try_from(take!(child)).unwrap_or(CLOSED);
                *phase = HEAD_READ;
                let at = k.scratch(level::HEADER);
                k.g.write_bytes(SEGMENT, at, &[0; mz::LEN]);
                Flow::Call(Frame::of(&Read {
                    index: u16::from(self.file),
                    buffer: Far::new(SEGMENT, at),
                    count: u16::try_from(mz::LEN).unwrap_or(0),
                    done: 0,
                    steps: 0,
                }))
            }
            HEAD_READ => {
                if let Err(fault) = child {
                    return self.fail(k, phase, fault);
                }
                let Some(image) = self.image(k) else {
                    return self.fail(k, phase, Fault::Error(error::BAD_FORMAT));
                };
                if let Err(fault) = self.prepare(k, &image) {
                    return self.fail(k, phase, fault);
                }
                self.seek(k, image.start);
                self.done = 0;
                self.load_more(phase, &image)
            }
            LOADED => {
                let read = match child {
                    Ok(read) => read,
                    Err(fault) => return self.fail(k, phase, fault),
                };
                let Some(image) = self.image(k) else {
                    return self.fail(k, phase, Fault::Error(error::BAD_FORMAT));
                };
                self.done = self.done.saturating_add(read);
                if read != 0 && self.done < image.size {
                    return self.load_more(phase, &image);
                }
                if image.relocations > 0 {
                    self.relocated = 0;
                    self.seek(k, u32::from(image.table));
                    return self.read_table(k, phase, &image);
                }
                self.close(phase)
            }
            TABLE_READ => self.relocate(k, phase, child),
            CLOSED_FILE => {
                take!(child);
                self.start(k)
            }
            FAILED => {
                let fault = Fault::Error(self.code);
                if self.psp != 0 {
                    k.free_owned(self.psp);
                }
                if self.environment != 0 {
                    let _ = k.free(self.environment);
                }
                Flow::Return(Err(fault))
            }
            _ => {
                if !matches!(self.mode, mode::RUN | mode::LOAD | mode::OVERLAY) {
                    return Flow::Return(Err(Fault::Error(error::INVALID_FUNCTION)));
                }
                *phase = OPENED;
                Flow::Call(Frame::of(&Open {
                    mode: access::READ,
                    keep_handle: true,
                    index: 0,
                    handle: 0,
                    drive: 0,
                }))
            }
        }
    }
}

impl Exec {
    /// Patches the relocations just read into the image, which is refused
    /// for one outside what the program owns: that would patch memory not
    /// its own.
    fn relocate(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let read = match child {
            Ok(bytes) => u16::try_from(bytes / 4).unwrap_or(0),
            Err(fault) => return self.fail(k, phase, fault),
        };
        let Some(image) = self.image(k) else {
            return self.fail(k, phase, Fault::Error(error::BAD_FORMAT));
        };
        let at = k.scratch(level::PATH2);
        let factor = if self.mode == mode::OVERLAY {
            self.relocation_factor(k)
        } else {
            self.load
        };
        let owned = image.size + u32::from(image.min_alloc) * 16;
        for n in 0..read {
            let offset = k.get16(at + n * 4);
            let segment = k.get16(at + n * 4 + 2);
            if u32::from(segment) * 16 + u32::from(offset) + 2 > owned {
                return self.fail(k, phase, Fault::Error(error::BAD_FORMAT));
            }
            let place = self.load.wrapping_add(segment);
            let value = k.g.read16(place, offset).wrapping_add(factor);
            k.g.write16(place, offset, value);
        }
        self.relocated += read;
        if read == 0 || self.relocated >= image.relocations {
            return self.close(phase);
        }
        self.read_table(k, phase, &image)
    }

    /// The image the header in the scratch describes.
    fn image(&self, k: &mut Kernel<'_, '_>) -> Option<Image> {
        let at = k.scratch(level::HEADER);
        let mut head = [0u8; mz::LEN];
        k.g.read_bytes(SEGMENT, at, &mut head);
        let entry = k.sft(u16::from(self.file))?;
        let size = k.sft32(entry, sft::SIZE);
        image(&head, size)
    }

    fn seek(&self, k: &mut Kernel<'_, '_>, position: u32) {
        if let Some(entry) = k.sft(u16::from(self.file)) {
            k.set_sft32(entry, sft::POSITION, position);
        }
    }

    fn relocation_factor(&self, k: &mut Kernel<'_, '_>) -> u16 {
        k.read16_at(self.block.add(2))
    }

    /// Finds the memory the program takes and builds its PSP and
    /// environment; an overlay takes neither.
    fn prepare(&mut self, k: &mut Kernel<'_, '_>, image: &Image) -> Result<(), Fault> {
        if self.mode == mode::OVERLAY {
            self.load = k.read16_at(self.block);
            return Ok(());
        }
        let parent = k.psp();
        let source = match k.read16_at(self.block) {
            0 => k.g.read16(parent, psp::ENVIRONMENT),
            segment => segment,
        };
        let mut environment = if source == 0 {
            alloc::vec![0, 0]
        } else {
            k.environment(source)
                .ok_or(Fault::Error(error::BAD_ENVIRONMENT))?
        };
        let (path, len) = k.path(level::PATH);
        environment.extend_from_slice(&1u16.to_le_bytes());
        environment.extend_from_slice(&path[..len]);
        environment.push(0);
        let env_paragraphs = u16::try_from(paragraphs(
            u32::try_from(environment.len()).unwrap_or(u32::MAX),
        ))
        .map_err(|_| Fault::Error(error::BAD_ENVIRONMENT))?;
        let rule = k.strategy();
        self.environment = k
            .allocate(env_paragraphs, parent, rule)
            .map_err(|r| Fault::Error(r.error()))?;
        k.g.write_bytes(self.environment, 0, &environment);
        let image_paragraphs = paragraphs(image.size);
        let needed = 0x10 + image_paragraphs + u32::from(image.min_alloc);
        let wanted = (0x10 + image_paragraphs + u32::from(image.max_alloc)).min(0xFFFF);
        let needed = u16::try_from(needed).map_err(|_| Fault::Error(error::NO_MEMORY))?;
        let largest = match k.allocate(0xFFFF, parent, rule) {
            Err(super::memory::Refusal::Short(largest)) => largest,
            Ok(segment) => {
                let _ = k.free(segment);
                0xFFFF
            }
            Err(refusal) => return Err(Fault::Error(refusal.error())),
        };
        if largest < needed {
            return Err(Fault::Error(error::NO_MEMORY));
        }
        let size = u16::try_from(wanted)
            .unwrap_or(u16::MAX)
            .min(largest)
            .max(needed);
        let segment = k
            .allocate(size, parent, rule)
            .map_err(|r| Fault::Error(r.error()))?;
        self.psp = segment;
        k.g.write16(
            self.environment.wrapping_sub(1),
            super::memory::mcb::OWNER,
            segment,
        );
        k.g.write16(segment.wrapping_sub(1), super::memory::mcb::OWNER, segment);
        let name = program_name(&path[..len]);
        k.name_block(segment, &name);
        k.name_block(self.environment, &name);
        let top = segment.wrapping_add(size);
        k.create_psp(segment, top, parent, true);
        k.g.write16(segment, psp::ENVIRONMENT, self.environment);
        self.load = if image.mz && image.max_alloc == 0 && image.min_alloc == 0 {
            top.wrapping_sub(u16::try_from(image_paragraphs).unwrap_or(0))
        } else {
            segment.wrapping_add(0x10)
        };
        Ok(())
    }

    fn read_table(&self, k: &mut Kernel<'_, '_>, phase: &mut u8, image: &Image) -> Flow {
        let left = image.relocations - self.relocated;
        let count = left.min(32) * 4;
        let at = k.scratch(level::PATH2);
        *phase = TABLE_READ;
        Flow::Call(Frame::of(&Read {
            index: u16::from(self.file),
            buffer: Far::new(SEGMENT, at),
            count,
            done: 0,
            steps: 0,
        }))
    }

    /// Reads the next run of the image into memory.
    fn load_more(&self, phase: &mut u8, image: &Image) -> Flow {
        let chunk = (image.size - self.done).min(0x8000);
        let segment = self
            .load
            .wrapping_add(u16::try_from(self.done >> 4).unwrap_or(0));
        let offset = u16::try_from(self.done & 0xF).unwrap_or(0);
        *phase = LOADED;
        Flow::Call(Frame::of(&Read {
            index: u16::from(self.file),
            buffer: Far::new(segment, offset),
            count: u16::try_from(chunk).unwrap_or(0x8000),
            done: 0,
            steps: 0,
        }))
    }

    /// Closes the file EXEC opened for itself.
    fn close(&self, phase: &mut u8) -> Flow {
        *phase = CLOSED_FILE;
        Flow::Call(Frame::of(&Close {
            handle: 0,
            index: u16::from(self.file),
            by_index: true,
        }))
    }

    fn fail(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, fault: Fault) -> Flow {
        if let Some(entry) = k.sft(u16::from(self.file)) {
            k.set_sft16(entry, sft::HANDLES, 0);
        }
        let Fault::Error(code) = fault else {
            return Flow::Return(Err(fault));
        };
        self.code = code;
        *phase = FAILED;
        self.step(k, phase, Ok(0))
    }

    /// The program is loaded: an overlay returns; a load hands back its
    /// entry and stack; a run starts it.
    fn start(&self, k: &mut Kernel<'_, '_>) -> Flow {
        if self.mode == mode::OVERLAY {
            return Flow::Return(Ok(0));
        }
        let Some(image) = self.image(k) else {
            return Flow::Return(Err(Fault::Error(error::BAD_FORMAT)));
        };
        let child = self.psp;
        let tail = k.far_at(self.block.add(2));
        let mut bytes = [0u8; 128];
        k.g.read_bytes(tail.segment, tail.offset, &mut bytes);
        let length = usize::from(bytes[0]).min(126);
        k.g.write_bytes(child, psp::TAIL, &bytes[..=length]);
        k.g.write8(
            child,
            psp::TAIL + 1 + u16::try_from(length).unwrap_or(0),
            0x0D,
        );
        let mut validity = [0u8; 2];
        for (n, (field, to)) in [(6u16, psp::FCB1), (10, psp::FCB2)].into_iter().enumerate() {
            let fcb = k.far_at(self.block.add(field));
            let mut copy = [0u8; 16];
            k.g.read_bytes(fcb.segment, fcb.offset, &mut copy);
            k.g.write_bytes(child, to, &copy);
            let drive = copy[0];
            let valid = drive == 0 || k.is_drive(drive - 1);
            validity[n] = if valid { 0 } else { 0xFF };
        }
        let return_ip = k.caller(Saved::Ip);
        let return_cs = k.caller(Saved::Cs);
        k.g.write16(child, psp::TERMINATE, return_ip);
        k.g.write16(child, psp::TERMINATE + 2, return_cs);
        k.g.write16(0, 0x22 * 4, return_ip);
        k.g.write16(0, 0x22 * 4 + 2, return_cs);
        let (cs, ip, ss, sp) = if image.mz {
            (
                self.load.wrapping_add(image.cs),
                image.ip,
                self.load.wrapping_add(image.ss),
                image.sp,
            )
        } else {
            let top = k.g.read16(child, psp::TOP);
            let room = u32::from(top.wrapping_sub(child)) * 16;
            let sp = if room >= 0x1_0000 {
                0xFFFE
            } else {
                u16::try_from(room - 2).unwrap_or(0xFFFE)
            };
            k.g.write16(child, sp, 0);
            (child, psp::LEN, child, sp)
        };
        if self.mode == mode::LOAD {
            let sp = sp.wrapping_sub(2);
            k.g.write16(ss, sp, u16::from_le_bytes(validity));
            k.write16_at(self.block.add(0x0E), sp);
            k.write16_at(self.block.add(0x10), ss);
            k.write16_at(self.block.add(0x12), ip);
            k.write16_at(self.block.add(0x14), cs);
            k.set_sda16(sda::PSP, child);
            k.set_dta(Far::new(child, psp::TAIL));
            return Flow::Return(Ok(0));
        }
        k.set_sda16(sda::PSP, child);
        k.set_dta(Far::new(child, psp::TAIL));
        leave_dos(k);
        let g = &mut *k.g;
        let regs = g.regs_mut();
        regs.set_sreg(Seg::Cs, cs);
        regs.ip = ip;
        regs.set_sreg(Seg::Ss, ss);
        regs.set_sreg(Seg::Ds, child);
        regs.set_sreg(Seg::Es, child);
        regs.set_flag(flag::IF, true);
        regs.set_flag(flag::DF, false);
        g.set_r16(Reg16::Sp, sp);
        g.set_r16(Reg16::Ax, u16::from_le_bytes(validity));
        g.set_r16(Reg16::Bx, 0);
        g.set_r16(Reg16::Cx, 0x00FF);
        g.set_r16(Reg16::Dx, child);
        g.set_r16(Reg16::Si, ip);
        g.set_r16(Reg16::Di, sp);
        g.set_r16(Reg16::Bp, 0x091E);
        Flow::Transfer
    }
}

/// The eight-byte name a program's blocks carry: its file name's base.
fn program_name(path: &[u8]) -> [u8; 8] {
    let file = path.rsplit(|&b| b == b'\\').next().unwrap_or(path);
    let base = file.split(|&b| b == b'.').next().unwrap_or(file);
    let mut name = [0u8; 8];
    for (slot, &byte) in name.iter_mut().zip(base) {
        *slot = byte;
    }
    name
}

/// The kernel's `InDOS` count drops as a call leaves it.
pub fn leave_dos(k: &mut Kernel<'_, '_>) {
    let indos = k.sda8(sda::INDOS);
    k.set_sda8(sda::INDOS, indos.saturating_sub(1));
}

locals! {
    /// Ends the current process with `code` and how it ended: its handles
    /// closed, its memory freed — but for a resident one, which keeps
    /// `keep` paragraphs — its vectors put back, every buffer written back,
    /// and its parent's EXEC call answered at the terminate address.
    pub struct Terminate {
        code: u8,
        how: u8,
        keep: u16,
        handle: u16,
    }
}

impl Routine for Terminate {
    const ID: Id = Id::Terminate;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == WRITTEN_BACK {
            // An ending process cannot be failed, so what could not be
            // written stays in the buffers and the parent resumes.
            let _ = child;
            return Self::resume_parent(k);
        }
        let process = k.psp();
        if *phase == CLOSED_FILE {
            // A failed close leaves the handle closed; the process ends all
            // the same.
            let _ = child;
            self.handle += 1;
        }
        if self.how == ending::RESIDENT {
            let _ = k.resize(process, self.keep.max(6));
        } else {
            let (_, size) = k.jft(process);
            while self.handle < size {
                if k.handle(self.handle).is_ok() {
                    *phase = CLOSED_FILE;
                    return Flow::Call(Frame::of(&Close {
                        handle: self.handle,
                        index: 0,
                        by_index: false,
                    }));
                }
                self.handle += 1;
            }
            k.free_owned(process);
        }
        for (field, vector) in [
            (psp::TERMINATE, 0x22u16),
            (psp::BREAK, 0x23),
            (psp::CRITICAL, 0x24),
        ] {
            let offset = k.g.read16(process, field);
            let segment = k.g.read16(process, field + 2);
            k.g.write16(0, vector * 4, offset);
            k.g.write16(0, vector * 4 + 2, segment);
        }
        k.set_sda16(sda::RETURN_CODE, u16::from_le_bytes([self.code, self.how]));
        let parent = k.g.read16(process, psp::PARENT);
        k.set_sda16(sda::PSP, parent);
        k.set_dta(Far::new(parent, psp::TAIL));
        k.release_busy_buffers();
        // As inside a handler, so a buffer that cannot be written fails
        // without asking.
        k.set_sda8(sda::ERROR_MODE, 1);
        *phase = WRITTEN_BACK;
        written_back(FREE)
    }
}

impl Terminate {
    /// The parent resumes at the terminate address the ending restored to
    /// INT 22h, its EXEC call answered with the carry clear.
    fn resume_parent(k: &mut Kernel<'_, '_>) -> Flow {
        let parent = k.psp();
        let terminate = Far::new(k.g.read16(0, 0x22 * 4 + 2), k.g.read16(0, 0x22 * 4));
        let frame = Far::new(
            k.g.read16(parent, psp::STACK + 2),
            k.g.read16(parent, psp::STACK),
        );
        engine::clear(k);
        k.set_sda8(sda::INDOS, 0);
        k.set_sda8(sda::ERROR_MODE, 0);
        k.write16_at(frame.add(Saved::Ip as u16 * 2), terminate.offset);
        k.write16_at(frame.add(Saved::Cs as u16 * 2), terminate.segment);
        let flags_at = frame.add(Saved::Flags as u16 * 2);
        let flags = k.read16_at(flags_at);
        k.write16_at(flags_at, flags & !flag::CF);
        k.g.set_stack(frame.segment, frame.offset);
        k.g.jump(crate::bios::layout::SEGMENT, super::rom::entry::EXIT);
        Flow::Transfer
    }
}
