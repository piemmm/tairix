//! The fixed-disk adapter's ROM at `C8000h`: INT 13h for drives 80h and
//! 81h through the controller's command block, DMA channel 3 and IRQ 5, with
//! the diskette service moved to INT 40h, and the drives' parameters, built
//! for the drives the machine has, where INT 41h and 46h point.

use super::disk_status as status;
use super::guest::Guest;
use super::layout::{waiting, Image};
use super::{bda, call, BuildError};
use crate::asm::{Cond, Reg16, Reg8};
use crate::chips::dma::channel_setup;
use crate::cpu::{flag, Seg};
use crate::storage::hdc;
use crate::storage::hdc::{completion, status as hw};
use crate::storage::hdc::{DiskType, Recording};
use alloc::vec::Vec;
use tairix_fat::format::FIXED_DRIVE;

/// Where the ROM sits.
pub const BASE: u32 = 0xC_8000;
pub const SEGMENT: u16 = 0xC800;
/// Its length in the 512-byte blocks its header counts.
const BLOCKS: u8 = 16;
const SIZE: usize = BLOCKS as usize * 512;
/// The ROM's pieces.
const INIT: u16 = 0x0003;
const SERVICE: u16 = 0x0100;
const IRQ: u16 = 0x0200;
/// The two drives' parameter tables.
const PARAMETERS: u16 = 0x0300;

/// The stub's state: the stage, then the tick a wait gives up at.
pub const STATE: u8 = 4;
const STAGE: u16 = 0;
const DEADLINE: u16 = 2;
const FLAGS: u16 = STATE as u16 + 4;

mod stage {
    pub const RUNNING: u8 = 1;
}

/// The adapter's ports.
const DATA: u16 = 0x320;
const STATUS: u16 = 0x321;
const SELECT: u16 = 0x322;
const MASK: u16 = 0x323;

/// The interrupt-came flag the IRQ 5 handler sets.
const INTERRUPTED: u16 = 0x8E;

/// A parameter table entry, as utilities read it: cylinders, heads, the
/// reduced-write-current and precompensation cylinders, the ECC burst, the
/// control byte's step option, the landing zone and the sectors a track.
fn entry(kind: DiskType, sectors: u8) -> [u8; 16] {
    let [cyl_low, cyl_high] = kind.cylinders.to_le_bytes();
    [
        cyl_low, cyl_high, kind.heads, cyl_low, cyl_high, cyl_low, cyl_high, 11, 0x05, 0, 0, 0,
        cyl_low, cyl_high, sectors, 0,
    ]
}

const fn sectors(recording: Recording) -> u8 {
    match recording {
        Recording::Mfm => 17,
        Recording::Rll => 26,
    }
}

/// The ROM for an adapter recording as `recording` with drives `drives`.
pub fn rom(recording: Recording, drives: [Option<DiskType>; 2]) -> Result<Vec<u8>, BuildError> {
    let mut rom = Image::new(SIZE);
    rom.place(0, &[0x55, 0xAA, BLOCKS])?;
    rom.code(INIT, |asm| {
        asm.host_call(call::FIXED_INIT);
        asm.retf();
    })?;
    rom.code(SERVICE, |asm| {
        let fixed = asm.label();
        asm.cmp_imm8(Reg8::Dl, FIXED_DRIVE);
        asm.jcc(Cond::Ae, &fixed);
        // A diskette's call goes to the service INT 40h now holds, and
        // returns with that service's flags.
        asm.sti();
        asm.int(0x40);
        asm.bytes(&[0xCA, 0x02, 0x00]);
        asm.bind(&fixed);
        waiting(asm, call::FIXED_START, call::FIXED_STEP, STATE);
    })?;
    rom.code(IRQ, |asm| {
        asm.host_call(call::FIXED_IRQ);
        asm.iret();
    })?;
    let mut tables = [0u8; 32];
    for (slot, kind) in tables.as_chunks_mut::<16>().0.iter_mut().zip(drives) {
        if let Some(kind) = kind {
            slot.copy_from_slice(&entry(kind, sectors(recording)));
        }
    }
    rom.place(PARAMETERS, &tables)?;
    Ok(rom.finish())
}

/// Drive `n`'s parameters from the ROM's table: cylinders, heads, sectors.
fn geometry(g: &mut Guest<'_>, n: u8) -> Option<(u16, u8, u8)> {
    let base = PARAMETERS + u16::from(n & 1) * 16;
    let cylinders = g.read16(SEGMENT, base);
    (cylinders != 0).then(|| {
        (
            cylinders,
            g.read8(SEGMENT, base + 2),
            g.read8(SEGMENT, base + 14),
        )
    })
}

/// Polls the controller, briefly, until its status shows `want` under
/// `mask`.
fn poll(g: &mut Guest<'_>, mask: u8, want: u8) -> bool {
    for _ in 0..500 {
        if g.input(STATUS) & mask == want {
            return true;
        }
        g.spend(10_000);
    }
    false
}

/// Selects the controller and hands it a command block.
fn command(g: &mut Guest<'_>, dcb: [u8; 6]) -> bool {
    g.output(SELECT, 0);
    dcb.iter().all(|&byte| {
        let ok = poll(
            g,
            hw::BUSY | hw::CONTROL | hw::IO | hw::REQ,
            hw::BUSY | hw::CONTROL | hw::REQ,
        );
        if ok {
            g.output(DATA, byte);
        }
        ok
    })
}

/// Bytes moved through the data port, as the controller asks for them.
fn send_data(g: &mut Guest<'_>, bytes: &[u8]) -> bool {
    bytes.iter().all(|&byte| {
        let ok = poll(g, hw::CONTROL | hw::IO | hw::REQ, hw::REQ);
        if ok {
            g.output(DATA, byte);
        }
        ok
    })
}

fn receive_data<const N: usize>(g: &mut Guest<'_>) -> Option<[u8; N]> {
    let mut out = [0; N];
    for byte in &mut out {
        if !poll(g, hw::CONTROL | hw::IO | hw::REQ, hw::IO | hw::REQ) {
            return None;
        }
        *byte = g.input(DATA);
    }
    Some(out)
}

/// Waits for the completion status by polling, and reads it.
fn completed(g: &mut Guest<'_>) -> Option<u8> {
    let all = hw::BUSY | hw::CONTROL | hw::IO | hw::REQ;
    poll(g, all, all).then(|| g.input(DATA))
}

/// Runs a command that moves no data and completes at once, polling, as
/// the adapter's initialisation does while interrupts are masked: whether
/// it succeeded.
fn immediate(g: &mut Guest<'_>, dcb: [u8; 6], data: &[u8]) -> bool {
    g.output(MASK, 0);
    command(g, dcb)
        && send_data(g, data)
        && completed(g).is_some_and(|status| status & completion::ERROR == 0)
}

/// The command block's drive and head byte, and its cylinder and sector.
fn address(g: &Guest<'_>) -> [u8; 3] {
    let (ch, cl, dh, dl) = (
        g.r8(Reg8::Ch),
        g.r8(Reg8::Cl),
        g.r8(Reg8::Dh),
        g.r8(Reg8::Dl),
    );
    [
        ((dl & 1) << 5) | (dh & 0x1F),
        (cl & 0xC0) | ((cl & 0x3F).wrapping_sub(1) & 0x3F),
        ch,
    ]
}

fn initialize(g: &mut Guest<'_>, n: u8) -> bool {
    let Some((cylinders, heads, _)) = geometry(g, n) else {
        return false;
    };
    let [high, low] = cylinders.to_be_bytes();
    let data = [high, low, heads, high, low, high, low, 11];
    immediate(g, [0x0C, n << 5, 0, 0, 0, 0], &data)
}

/// The ROM's initialisation, called from the POST: the vectors, the
/// controller reset and each drive's characteristics.
pub fn init(g: &mut Guest<'_>) {
    let floppy = (g.read16(0, 0x13 * 4), g.read16(0, 0x13 * 4 + 2));
    for (n, segment, offset) in [
        (0x40, floppy.1, floppy.0),
        (0x13, SEGMENT, SERVICE),
        (0x0D, SEGMENT, IRQ),
        (0x41, SEGMENT, PARAMETERS),
        (0x46, SEGMENT, PARAMETERS + 16),
    ] {
        g.write16(0, n * 4, offset);
        g.write16(0, n * 4 + 2, segment);
    }
    g.output(STATUS, 0);
    let ready = (0..2u8)
        .filter(|&n| initialize(g, n) && immediate(g, [0x00, n << 5, 0, 0, 0, 0], &[]))
        .count();
    g.set_bda8(bda::FIXED_COUNT, u8::try_from(ready).unwrap_or(0));
    let mask = g.input(0x21) & !0x20;
    g.output(0x21, mask);
}

/// INT 0Dh: the controller has finished a command.
pub fn interrupt(g: &mut Guest<'_>) {
    g.set_bda8(INTERRUPTED, 0xFF);
    g.output(0x20, 0x20);
}

fn answer(g: &mut Guest<'_>, code: u8) {
    g.set_r8(Reg8::Ah, code);
    g.set_bda8(bda::FIXED_STATUS, code);
    g.set_returned_flag(FLAGS, flag::CF, code != 0);
    g.done(true);
}

/// The controller's sense, as INT 13h names it.
const fn translate(sense: u8) -> u8 {
    match sense & 0x3F {
        0x00 => 0,
        0x02 | 0x06 | 0x15 => status::SEEK,
        0x03 => status::WRITE_FAULT,
        0x04 => status::NOT_READY,
        0x10 | 0x11 => status::CRC,
        0x12 => status::NO_ADDRESS_MARK,
        0x14 => status::NOT_FOUND,
        0x18 => status::ECC_CORRECTED,
        0x19 => status::BAD_TRACK,
        0x20 | 0x21 => status::INVALID,
        _ => status::CONTROLLER,
    }
}

/// INT 13h's first call for a fixed disk.
pub fn start(g: &mut Guest<'_>) {
    let n = g.r8(Reg8::Dl) & 0x7F;
    if n >= g.bda8(bda::FIXED_COUNT) {
        return answer(g, status::TIMEOUT);
    }
    let unit = n << 5;
    let [head, sector, cylinder] = address(g);
    let count = g.r8(Reg8::Al);
    let (opcode, transfer): (u8, Option<(u8, u32)>) = match g.r8(Reg8::Ah) {
        0x00 | 0x0D => return reset(g),
        0x01 => {
            let last = g.bda8(bda::FIXED_STATUS);
            g.set_r8(Reg8::Al, last);
            return answer(g, last);
        }
        0x02 => (0x08, Some((0x47, 512))),
        0x03 => (0x0A, Some((0x4B, 512))),
        0x04 => (0x05, None),
        0x05 => (0x06, None),
        0x06 => (0x07, None),
        0x07 => (0x04, None),
        0x08 => return parameters(g, n),
        0x09 => {
            let all = (0..g.bda8(bda::FIXED_COUNT)).all(|d| initialize(g, d));
            return answer(g, if all { 0 } else { status::CONTROLLER });
        }
        0x0A => (0xE5, Some((0x47, 516))),
        0x0B => (0xE6, Some((0x4B, 516))),
        0x0C => (0x0B, None),
        0x0E => (0x0E, Some((0x47, 512))),
        0x0F => (0x0F, Some((0x4B, 512))),
        0x10 => (0x00, None),
        0x11 => (0x01, None),
        0x12 => (0xE0, None),
        0x13 => (0xE3, None),
        0x14 => (0xE4, None),
        0x15 => return disk_type(g, n),
        _ => return answer(g, status::INVALID),
    };
    let per_sector = matches!(opcode, 0x08 | 0x0A | 0xE5 | 0xE6);
    let blocks = if per_sector { count } else { 1 };
    let dcb = [opcode, unit | (head & 0x1F), sector, cylinder, count, 0x05];
    let mask = match transfer {
        Some((mode, bytes)) => {
            let total = u32::from(blocks) * bytes;
            let linear = (u32::from(g.seg(Seg::Es)) << 4) + u32::from(g.r16(Reg16::Bx));
            if total == 0 || (linear & 0xFFFF) + total > 0x1_0000 {
                return answer(
                    g,
                    if total == 0 {
                        status::INVALID
                    } else {
                        status::DMA_BOUNDARY
                    },
                );
            }
            let count = u16::try_from(total - 1).unwrap_or(0);
            for (port, value) in channel_setup(hdc::DMA_CHANNEL, mode, linear, count) {
                g.output(port, value);
            }
            0x03
        }
        None => 0x02,
    };
    g.set_bda8(INTERRUPTED, 0);
    g.output(MASK, mask);
    if !command(g, dcb) {
        return answer(g, status::CONTROLLER);
    }
    g.set_local(STAGE, stage::RUNNING);
    // A format of the whole drive runs for minutes; anything else seconds.
    let ticks: u16 = if opcode == 0x04 { 0xFFFF } else { 182 };
    let [low, high] = g.bda16(bda::TICKS).wrapping_add(ticks).to_le_bytes();
    g.set_local(DEADLINE, low);
    g.set_local(DEADLINE + 1, high);
    g.done(false);
}

/// INT 13h's later calls: the command's interrupt, then its status and, on
/// an error, the controller's sense.
pub fn step(g: &mut Guest<'_>) {
    if g.bda8(INTERRUPTED) == 0 {
        let deadline = u16::from_le_bytes([g.local(DEADLINE), g.local(DEADLINE + 1)]);
        if g.bda16(bda::TICKS).wrapping_sub(deadline) < 0x8000 {
            return answer(g, status::TIMEOUT);
        }
        return g.done(false);
    }
    g.set_bda8(INTERRUPTED, 0);
    if g.input(DATA) & completion::ERROR == 0 {
        return answer(g, 0);
    }
    let unit = g.r8(Reg8::Dl) & 1;
    let code = request_sense(g, unit).map_or(status::CONTROLLER, translate);
    g.set_r8(Reg8::Al, 0);
    answer(g, code);
}

/// REQUEST SENSE through the data port; the first sense byte.
fn request_sense(g: &mut Guest<'_>, unit: u8) -> Option<u8> {
    g.output(MASK, 0);
    if !command(g, [0x03, unit << 5, 0, 0, 0, 0]) {
        return None;
    }
    let [code, ..] = receive_data::<4>(g)?;
    completed(g)?;
    Some(code)
}

/// AH 00h and 0Dh: resets the controller and gives every drive its
/// characteristics again.
fn reset(g: &mut Guest<'_>) {
    g.output(STATUS, 0);
    let all = (0..g.bda8(bda::FIXED_COUNT)).all(|n| initialize(g, n));
    answer(g, if all { 0 } else { status::RESET_FAILED });
}

/// AH 08h: the drive's last cylinder, kept back for diagnostics, its last
/// head and sector, and how many drives there are.
fn parameters(g: &mut Guest<'_>, n: u8) {
    let Some((cylinders, heads, sectors)) = geometry(g, n) else {
        return answer(g, status::INVALID);
    };
    let last = cylinders.saturating_sub(2);
    let [low, high] = last.to_le_bytes();
    let count = g.bda8(bda::FIXED_COUNT);
    g.set_r16(
        Reg16::Cx,
        u16::from_le_bytes([((high & 3) << 6) | (sectors & 0x3F), low]),
    );
    g.set_r16(
        Reg16::Dx,
        u16::from_le_bytes([count, heads.saturating_sub(1)]),
    );
    answer(g, 0);
}

/// AH 15h: a fixed disk, and its sectors.
fn disk_type(g: &mut Guest<'_>, n: u8) {
    let Some((cylinders, heads, sectors)) = geometry(g, n) else {
        return answer(g, status::INVALID);
    };
    let total = (u32::from(cylinders) * u32::from(heads) * u32::from(sectors)).to_le_bytes();
    g.set_r16(Reg16::Cx, u16::from_le_bytes([total[2], total[3]]));
    g.set_r16(Reg16::Dx, u16::from_le_bytes([total[0], total[1]]));
    g.set_r8(Reg8::Ah, 3);
    g.set_returned_flag(FLAGS, flag::CF, false);
    g.done(true);
}
