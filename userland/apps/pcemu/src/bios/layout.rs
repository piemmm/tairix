//! The system ROM: its stubs at the addresses PC software calls, its tables
//! where software reads them, and the model byte, date and checksum.

use super::{call, BuildError};
use crate::asm::{Asm, Cond, Reg16, Reg8};
use crate::board::BoardConfig;
use crate::chips::ppi::Wiring;
use crate::cpu::Seg;
use crate::storage::fdc::{DriveKind, Interface};
use crate::video::font;
use alloc::vec;
use alloc::vec::Vec;

/// Where the system ROM sits.
pub const SYSTEM_BASE: u32 = (SEGMENT as u32) << 4;
/// Its segment.
pub const SEGMENT: u16 = 0xF000;
const SIZE: usize = 0x1_0000;
/// The first segment an adapter's option ROM may begin at.
pub const ADAPTER_ROMS: u16 = 0xC000;
/// Where an EGA's or VGA's ROM sits: the first adapter ROM, which POST
/// finds before any other, so the display is up for theirs.
pub const VIDEO_ROM: u16 = ADAPTER_ROMS;
/// The word an option ROM begins with, its bytes `55h AAh`.
pub const OPTION_ROM_SIGNATURE: u16 = 0xAA55;
/// Where INT 19h reads a disk's first sector to, in segment 0, and starts it.
pub const BOOTSTRAP: u16 = 0x7C00;

/// The entry points PC software calls by address.
pub mod entry {
    pub const POST: u16 = 0xE05B;
    pub const NMI: u16 = 0xE2C3;
    pub const BOOT: u16 = 0xE6F2;
    pub const SERIAL: u16 = 0xE739;
    pub const KEYBOARD: u16 = 0xE82E;
    pub const KEYBOARD_IRQ: u16 = 0xE987;
    pub const DISKETTE: u16 = 0xEC59;
    pub const FLOPPY_IRQ: u16 = 0xEF57;
    pub const PRINTER: u16 = 0xEFD2;
    pub const VIDEO: u16 = 0xF065;
    pub const MEMORY: u16 = 0xF841;
    pub const EQUIPMENT: u16 = 0xF84D;
    pub const SYSTEM: u16 = 0xF859;
    pub const CLOCK: u16 = 0xFE6E;
    pub const TICK: u16 = 0xFEA5;
    /// Unused hardware interrupts: an end of interrupt and nothing else.
    pub const EOI: u16 = 0xFF23;
    /// Unused software interrupts.
    pub const IRET: u16 = 0xFF53;
    pub const PRINT_SCREEN: u16 = 0xFF54;
    /// Where the CPU starts.
    pub const RESET: u16 = 0xFFF0;
    /// INT 18h, where the PC's cassette BASIC began.
    pub const NO_BOOT: u16 = 0x6000;
    /// INT 67h, behind the expanded memory manager's device header at
    /// the segment's start.
    pub const EMS: u16 = 0x0012;
    /// Where a map-and-call's routine returns to.
    pub const EMS_RETURN: u16 = 0x0050;
    /// The device header's strategy and interrupt routines: DOS never
    /// calls them, as the header is in no chain.
    pub const EMS_DEVICE: u16 = 0x0060;
    /// Where a stopped machine stays.
    pub const HALT: u16 = 0x6020;
}

/// Tables PC software reads by address.
pub mod table {
    /// The diskette parameters INT 1Eh points at.
    pub const DISKETTE: u16 = 0xEFC7;
    /// The video parameters INT 1Dh points at.
    pub const VIDEO: u16 = 0xF0A4;
    /// The 8×8 font's first 128 characters, for the CGA's graphics modes.
    pub const FONT: u16 = 0xFA6E;
    /// Its last 128, which INT 1Fh points at.
    pub const FONT_HIGH: u16 = 0x6800;
    /// The floppy adapter, high-density or not, then the four drives' types
    /// as INT 13h numbers them: the firmware's own record of the machine,
    /// which an XT has no CMOS to keep.
    pub const DRIVES: u16 = 0x6080;
    /// Each medium's diskette parameters.
    pub const MEDIA: u16 = 0x60A0;
    /// The clock card's port, or zero without one: the firmware's record of
    /// a card an XT's own BIOS knew nothing of.
    pub const CLOCK: u16 = 0x60F0;
    /// The expanded memory board: its registers' port, its frame's segment
    /// and its pages; zeros without one.
    pub const EMS: u16 = 0x60F4;
    /// The firmware's date, as software reads it.
    pub const DATE: u16 = 0xFFF5;
    /// The model byte: `FFh` for the PC, `FEh` for the XT.
    pub const MODEL: u16 = 0xFFFE;
}

/// The device name a program finds an expanded memory manager by.
pub const EMS_DEVICE: &[u8; 8] = b"EMMXXXX0";

/// The diskette parameter table INT 1Eh points at: where each field lies,
/// for the BIOS that reads it and for DOS, which keeps a copy of its own.
pub mod diskette {
    pub const VECTOR: u8 = 0x1E;
    pub const LEN: u16 = 11;
    /// The two bytes of the controller's SPECIFY command.
    pub const SPECIFY: u16 = 0;
    /// The clock ticks the motor turns on after the last access.
    pub const MOTOR_OFF: u16 = 2;
    /// The sector size: 128 bytes shifted left by this code.
    pub const SIZE_CODE: u16 = 3;
    pub const LAST_SECTOR: u16 = 4;
    /// The gap a read or write leaves between sectors.
    pub const GAP: u16 = 5;
    /// The bytes a sector of size code 0 holds.
    pub const DATA_LENGTH: u16 = 6;
    /// The gap a format leaves between sectors.
    pub const FORMAT_GAP: u16 = 7;
    /// The byte a format fills each sector with.
    pub const FILL: u16 = 8;
    /// The eighths of a second the motor takes to start.
    pub const MOTOR_START: u16 = 10;
}

/// The diskette parameters: 6 ms steps, a 4 ms head load, the motor off two
/// seconds after the last access, nine 512-byte sectors, the IBM gaps, 25 ms
/// head settling and a half-second motor start.
pub const DISKETTE_PARAMETERS: [u8; diskette::LEN as usize] = [
    0xCF, 0x02, 0x25, 0x02, 0x09, 0x2A, 0xFF, 0x50, 0xF6, 0x19, 0x04,
];

/// The table INT 1Dh points at: the 6845's registers for the 40- and
/// 80-column text modes, the graphics modes and the monochrome mode. A
/// program may point INT 1Dh at a copy of these bytes of its own.
pub const VIDEO_PARAMETERS: [u8; VIDEO_PARAMETERS_LEN as usize] = video_parameters();
pub const VIDEO_PARAMETERS_LEN: u16 = 64;

const fn video_parameters() -> [u8; VIDEO_PARAMETERS_LEN as usize] {
    const BLOCKS: [[u8; 16]; 4] = [
        super::modes::CRTC_40,
        super::modes::CRTC_80,
        super::modes::CRTC_GRAPHICS,
        super::modes::CRTC_MONO,
    ];
    let mut out = [0; VIDEO_PARAMETERS_LEN as usize];
    let mut i = 0;
    while i < out.len() {
        out[i] = BLOCKS[i / 16][i % 16];
        i += 1;
    }
    out
}

/// The mode set's own tables, which follow the parameters in ROM as IBM's
/// do and are read there, never through INT 1Dh: each kind's buffer
/// length, every mode's columns and its mode register.
pub const BUFFER_LENGTHS: [u16; 4] = [0x0800, 0x1000, 0x4000, 0x4000];
pub const MODE_COLUMNS: [u8; 8] = [40, 40, 80, 80, 40, 40, 80, 80];
pub const MODE_REGISTERS: [u8; 8] = [0x2C, 0x28, 0x2D, 0x29, 0x2A, 0x2E, 0x1E, 0x29];

/// Those tables as the ROM holds them.
const VIDEO_MODES: [u8; 24] = {
    let mut out = [0; 24];
    let mut i = 0;
    while i < 4 {
        let [low, high] = BUFFER_LENGTHS[i].to_le_bytes();
        out[i * 2] = low;
        out[i * 2 + 1] = high;
        i += 1;
    }
    while i < 12 {
        out[i + 4] = MODE_COLUMNS[i - 4];
        out[i + 12] = MODE_REGISTERS[i - 4];
        i += 1;
    }
    out
};

/// A ROM image being laid out: pieces placed at fixed offsets, never over
/// each other.
pub struct Image {
    bytes: Vec<u8>,
    placed: Vec<(usize, usize)>,
}

impl Image {
    pub fn new(len: usize) -> Self {
        Self {
            bytes: vec![0xFF; len],
            placed: Vec::new(),
        }
    }

    pub fn place(&mut self, offset: u16, data: &[u8]) -> Result<(), BuildError> {
        let start = usize::from(offset);
        let end = start + data.len();
        if end > self.bytes.len() || self.placed.iter().any(|&(s, e)| start < e && s < end) {
            return Err(BuildError::Overlap(offset));
        }
        self.bytes[start..end].copy_from_slice(data);
        self.placed.push((start, end));
        Ok(())
    }

    /// Assembles code at `offset` and places it there.
    pub fn code(&mut self, offset: u16, build: impl FnOnce(&mut Asm)) -> Result<(), BuildError> {
        let mut asm = Asm::new(offset);
        build(&mut asm);
        self.place(offset, &asm.finish()?)
    }

    /// The image, its last byte making every byte sum to zero.
    pub fn finish(mut self) -> Vec<u8> {
        let last = self.bytes.len() - 1;
        let sum = self.bytes[..last]
            .iter()
            .fold(0u8, |sum, &b| sum.wrapping_add(b));
        self.bytes[last] = sum.wrapping_neg();
        self.bytes
    }
}

/// A service that may wait: it runs with interrupts masked, and while it
/// asks to wait, its stub halts with them enabled until the next interrupt.
/// `state` bytes on the stack are the service's own.
pub fn waiting(asm: &mut Asm, start: u16, step: u16, state: u8) {
    if state > 0 {
        asm.reserve(state);
    }
    ask_until_done(asm, |asm| asm.host_call(start), |asm| asm.host_call(step));
    if state > 0 {
        asm.release(state);
    }
    asm.iret();
}

/// The loop a service that may wait is asked in: `ask` asks it with
/// interrupts masked, and while it waits (ZF clear) the CPU halts with them
/// enabled until the next interrupt, and `ask_again` asks again — at once,
/// with no interrupt waited for, when the machine's run ended before the
/// service did (CF set too).
pub fn ask_until_done(asm: &mut Asm, ask: impl FnOnce(&mut Asm), ask_again: impl FnOnce(&mut Asm)) {
    let check = asm.label();
    let again = asm.label();
    let done = asm.label();
    asm.cli();
    ask(asm);
    asm.bind(&check);
    asm.jcc(Cond::E, &done);
    asm.jcc(Cond::B, &again);
    // STI holds interrupts off for one more instruction, so one that came
    // while the service looked is taken inside HLT, which it then ends.
    asm.sti();
    asm.hlt();
    asm.cli();
    asm.bind(&again);
    ask_again(asm);
    asm.jmp_short(&check);
    asm.bind(&done);
}

/// A service answered in one call.
fn direct(asm: &mut Asm, service: u16) {
    asm.host_call(service);
    asm.iret();
}

/// The POST: its first part, a far call to each option ROM found, its last
/// part, then the bootstrap.
fn post(asm: &mut Asm) {
    let scan = asm.label();
    let done = asm.label();
    asm.cli();
    asm.host_call(call::POST);
    asm.bind(&scan);
    asm.host_call(call::OPTION_ROM);
    asm.jcc(Cond::E, &done);
    // A far call to the ROM's entry at ES:0003, returning to the scan.
    asm.push_seg(Seg::Cs);
    asm.mov_label(Reg16::Ax, &scan);
    asm.push(Reg16::Ax);
    asm.push_seg(Seg::Es);
    asm.mov_imm16(Reg16::Ax, 3);
    asm.push(Reg16::Ax);
    asm.retf();
    asm.bind(&done);
    asm.host_call(call::POST_DONE);
    asm.sti();
    asm.int(0x19);
}

/// INT 19h: an INT 13h call the service chooses, until one boots.
fn boot(asm: &mut Asm) {
    let next = asm.label();
    let none = asm.label();
    asm.sti();
    asm.zero(Reg16::Ax);
    asm.push(Reg16::Ax);
    asm.bind(&next);
    asm.host_call(call::BOOT);
    asm.jcc(Cond::E, &none);
    asm.int(0x13);
    asm.host_call(call::BOOT_RESULT);
    asm.jmp_short(&next);
    asm.bind(&none);
    asm.release(2);
    asm.int(0x18);
}

/// INT 09h: the scan code through INT 15h's intercept to the service, then
/// whatever it asks for — INT 1Bh, INT 05h, or a wait for a pause or a beep.
fn keyboard_irq(asm: &mut Asm) {
    let consumed = asm.label();
    let not_break = asm.label();
    let not_print = asm.label();
    let wait = asm.label();
    let out = asm.label();
    asm.sti();
    asm.push(Reg16::Ax);
    asm.reserve(super::keyboard::STATE);
    asm.in_al(0x60);
    asm.host_call(call::KEYBOARD_ACK);
    asm.mov_imm8(Reg8::Ah, 0x4F);
    asm.stc();
    asm.int(0x15);
    asm.jcc(Cond::Ae, &consumed);
    // The service ends the interrupt, then asks for what follows.
    asm.host_call(call::KEYBOARD_IRQ);
    asm.cmp_imm8(Reg8::Ah, 1);
    asm.jcc(Cond::Ne, &not_break);
    asm.int(0x1B);
    asm.jmp_short(&out);
    asm.bind(&not_break);
    asm.cmp_imm8(Reg8::Ah, 2);
    asm.jcc(Cond::Ne, &not_print);
    asm.int(0x05);
    asm.jmp_short(&out);
    asm.bind(&not_print);
    asm.cmp_imm8(Reg8::Ah, 3);
    asm.jcc(Cond::Ne, &out);
    asm.bind(&wait);
    asm.cli();
    asm.host_call(call::PAUSE);
    asm.jcc(Cond::E, &out);
    asm.sti();
    asm.hlt();
    asm.jmp_short(&wait);
    asm.bind(&consumed);
    end_of_interrupt(asm);
    asm.bind(&out);
    asm.release(super::keyboard::STATE);
    asm.pop(Reg16::Ax);
    asm.iret();
}

fn end_of_interrupt(asm: &mut Asm) {
    asm.mov_imm8(Reg8::Al, 0x20);
    asm.out_al(0x20);
}

/// INT 08h: the tick, then the user's INT 1Ch, then the end of interrupt.
fn tick(asm: &mut Asm) {
    asm.sti();
    asm.push(Reg16::Ax);
    asm.host_call(call::TICK);
    asm.int(0x1C);
    end_of_interrupt(asm);
    asm.pop(Reg16::Ax);
    asm.iret();
}

fn spurious(asm: &mut Asm) {
    asm.push(Reg16::Ax);
    end_of_interrupt(asm);
    asm.pop(Reg16::Ax);
    asm.iret();
}

/// INT 05h: an INT 17h call a character, the service choosing each; `start`
/// begins the print and chooses the rows.
pub fn print_screen(asm: &mut Asm, start: u16) {
    let next = asm.label();
    let done = asm.label();
    asm.sti();
    asm.push(Reg16::Ax);
    asm.push(Reg16::Dx);
    asm.reserve(super::system::PRINT_STATE);
    asm.host_call(start);
    asm.bind(&next);
    asm.jcc(Cond::E, &done);
    asm.int(0x17);
    asm.host_call(call::PRINT_SCREEN_NEXT);
    asm.jmp_short(&next);
    asm.bind(&done);
    asm.release(super::system::PRINT_STATE);
    asm.pop(Reg16::Dx);
    asm.pop(Reg16::Ax);
    asm.iret();
}

/// INT 18h: the message, a key, and the bootstrap again.
fn no_boot(asm: &mut Asm) {
    asm.sti();
    asm.host_call(call::NO_BOOT);
    asm.zero(Reg16::Ax);
    asm.int(0x16);
    asm.int(0x19);
}

fn halt(asm: &mut Asm) {
    let stop = asm.label();
    asm.cli();
    asm.bind(&stop);
    asm.hlt();
    asm.jmp_short(&stop);
}

/// The floppy adapter's kind and its drives' types, as [`table::DRIVES`]
/// holds them.
fn drives(config: &BoardConfig) -> [u8; 5] {
    let mut drives = [0u8; 5];
    if let Some(adapter) = config.floppy {
        drives[0] = u8::from(adapter.interface == Interface::HighDensity);
        for (slot, kind) in drives[1..].iter_mut().zip(adapter.drives) {
            *slot = match kind {
                None => 0,
                Some(DriveKind::Dd525) => 1,
                Some(DriveKind::Hd525) => 2,
                Some(DriveKind::Dd35) => 3,
                Some(DriveKind::Hd35) => 4,
            };
        }
    }
    drives
}

/// The system ROM for a machine built from `config`.
/// The expanded memory manager's device header, stubs and the record of
/// its board.
fn expanded_memory(rom: &mut Image, board: crate::board::EmsConfig) -> Result<(), BuildError> {
    let mut header = [0u8; 18];
    header[..4].copy_from_slice(&[0xFF; 4]);
    header[4..6].copy_from_slice(&0xC000u16.to_le_bytes());
    header[6..8].copy_from_slice(&entry::EMS_DEVICE.to_le_bytes());
    header[8..10].copy_from_slice(&entry::EMS_DEVICE.to_le_bytes());
    header[10..].copy_from_slice(EMS_DEVICE);
    rom.place(0, &header)?;
    rom.code(entry::EMS, |asm| {
        waiting(asm, call::EMS, call::EMS_STEP, super::ems::STATE);
    })?;
    rom.code(entry::EMS_RETURN, |asm| direct(asm, call::EMS_RETURN))?;
    rom.code(entry::EMS_DEVICE, Asm::retf)?;
    let frame = u16::try_from(board.frame >> 4).unwrap_or(0);
    let mut record = [0u8; 6];
    record[..2].copy_from_slice(&board.port.to_le_bytes());
    record[2..4].copy_from_slice(&frame.to_le_bytes());
    record[4..].copy_from_slice(&board.pages.to_le_bytes());
    rom.place(table::EMS, &record)
}

pub fn system_rom(config: &BoardConfig) -> Result<Vec<u8>, BuildError> {
    let mut rom = Image::new(SIZE);
    rom.place(
        0xE000,
        b"TAIRiX PC BIOS: first-party firmware for the PC and XT",
    )?;
    rom.code(entry::POST, post)?;
    rom.code(entry::NMI, |asm| direct(asm, call::NMI))?;
    rom.code(entry::BOOT, boot)?;
    rom.code(entry::SERIAL, |asm| {
        waiting(
            asm,
            call::SERIAL,
            call::SERIAL_STEP,
            super::ports::SERIAL_STATE,
        );
    })?;
    rom.code(entry::KEYBOARD, |asm| {
        waiting(asm, call::KEYBOARD, call::KEYBOARD, 0);
    })?;
    rom.code(entry::KEYBOARD_IRQ, keyboard_irq)?;
    rom.code(entry::DISKETTE, |asm| {
        waiting(
            asm,
            call::DISKETTE_START,
            call::DISKETTE_STEP,
            super::floppy::STATE,
        );
    })?;
    rom.place(table::DRIVES, &drives(config))?;
    rom.place(table::MEDIA, super::floppy::MEDIA_PARAMETERS.as_flattened())?;
    rom.code(entry::FLOPPY_IRQ, |asm| direct(asm, call::FLOPPY_IRQ))?;
    rom.place(table::DISKETTE, &DISKETTE_PARAMETERS)?;
    rom.code(entry::PRINTER, |asm| {
        waiting(
            asm,
            call::PRINTER,
            call::PRINTER_STEP,
            super::ports::PRINTER_STATE,
        );
    })?;
    rom.code(entry::VIDEO, |asm| {
        waiting(asm, call::VIDEO, call::VIDEO_WAIT, super::video::STATE);
    })?;
    rom.place(table::VIDEO, &VIDEO_PARAMETERS)?;
    rom.place(table::VIDEO + VIDEO_PARAMETERS_LEN, &VIDEO_MODES)?;
    rom.code(entry::MEMORY, |asm| direct(asm, call::MEMORY))?;
    rom.code(entry::EQUIPMENT, |asm| direct(asm, call::EQUIPMENT))?;
    rom.code(entry::SYSTEM, |asm| direct(asm, call::SYSTEM))?;
    let glyphs: Vec<u8> = (0..=255u8)
        .flat_map(|code| *font::CGA.glyph(code))
        .collect();
    rom.place(table::FONT, &glyphs[..1024])?;
    rom.place(table::FONT_HIGH, &glyphs[1024..])?;
    rom.code(entry::CLOCK, |asm| direct(asm, call::CLOCK))?;
    let clock = if config.ports.clock {
        crate::ports::clock::PORT
    } else {
        0
    };
    rom.place(table::CLOCK, &clock.to_le_bytes())?;
    match config.ems {
        Some(ems) => expanded_memory(&mut rom, ems)?,
        None => rom.place(table::EMS, &[0; 6])?,
    }
    rom.code(entry::TICK, tick)?;
    rom.code(entry::EOI, spurious)?;
    rom.code(entry::IRET, Asm::iret)?;
    rom.code(entry::PRINT_SCREEN, |asm| {
        print_screen(asm, call::PRINT_SCREEN);
    })?;
    rom.code(entry::NO_BOOT, no_boot)?;
    rom.code(entry::HALT, halt)?;
    rom.code(entry::RESET, |asm| asm.jmp_far(SEGMENT, entry::POST))?;
    rom.place(table::DATE, b"11/09/26")?;
    let id = match config.wiring {
        Wiring::Pc => 0xFF,
        Wiring::Xt => 0xFE,
    };
    rom.place(table::MODEL, &[id])?;
    crate::dos::rom::place(&mut rom)?;
    Ok(rom.finish())
}
