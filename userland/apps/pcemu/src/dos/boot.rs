//! Starting TAIRiX DOS: the code a volume's boot record carries, and the
//! boot the ROM's loader runs — the kernel's data laid out in guest memory,
//! the built-in devices initialised through their headers, `CONFIG.SYS`
//! read and obeyed, its `DEVICE=` drivers loaded and linked into the chain,
//! its `INSTALL=` programs run, and the shell started, and started again
//! whenever it ends.

use super::char::CharIo;
use super::data::{
    boot, cds, dpb, driver, level, sda, sft_block, sysvars, unit, Builtin, BOOT, BOOT_BUFFER_COUNT,
    BUFFER, CDS, DOS_PSP, DRIVES, END, INTERRUPT_JUMP, NUL, NUL_JUMPS, SEGMENT, SFT, SFT_ENTRY,
    SFT_FIRST, SFT_HEADER, STRATEGY_JUMP, SYSVARS, UNIT, UNITS,
};
use super::device::{self, attribute, command, field, header, status, Device};
use super::engine::{self, locals, Far, Flow, Frame, Id, Kernel, Outcome, Routine, Saved};
use super::exec::{mode, Exec};
use super::file::{access, Close, Open};
use super::fs::{cds_at, dpb_at, FREE};
use super::memory::{mcb, psp, strategy};
use super::rom::{self, entry};
use super::stacks;
use crate::asm::{Asm, AsmError, Cond, Reg16, Reg8};
use crate::bios::layout::{entry as bios, BOOTSTRAP, SEGMENT as ROM};
use crate::cpu::{flag, Seg};
use alloc::vec::Vec;
use tairix_fat::boot::{at, Bpb, BOOT_SECTOR, SIGNATURE};
use tairix_partition::mbr::{self, MBR_SECTOR_LEN, PARTITION_ENTRY_LEN, PARTITION_TABLE_OFFSET};

/// The OEM name a TAIRiX DOS boot record carries.
pub const OEM: &[u8; 8] = b"TAIRIX50";

/// What a boot record off this machine says.
const NO_ROM: &[u8] = b"This disk starts TAIRiX DOS, which needs PCEmulator's ROM.\r\nPress a key to try again.\r\n\0";
/// What the boot record of a disk without the system says, as DOS's does.
const NO_SYSTEM: &[u8] =
    b"\r\nNon-System disk or disk error\r\nReplace and press any key when ready\r\n\0";

/// The volume boot record of a TAIRiX DOS system disk whose volume `bpb`
/// describes: it finds the kernel's signature in the system ROM and jumps
/// to its loader, the boot drive in DL; off this machine it says so and
/// waits for a key to try the next boot.
///
/// # Errors
///
/// [`AsmError`] should its code not assemble, a defect of its own.
pub fn boot_record(bpb: &Bpb) -> Result<[u8; BOOT_SECTOR], AsmError> {
    Ok(tairix_fat::format::boot_sector(
        bpb,
        OEM,
        &volume_code(true)?,
    ))
}

/// The volume boot record of a disk formatted without the system: it says
/// the disk starts nothing and waits for a key to try the next boot.
///
/// # Errors
///
/// [`AsmError`] should its code not assemble, a defect of its own.
pub fn non_system_record(bpb: &Bpb) -> Result<[u8; BOOT_SECTOR], AsmError> {
    Ok(tairix_fat::format::boot_sector(
        bpb,
        OEM,
        &volume_code(false)?,
    ))
}

/// What a master boot record says when the disk cannot be started, as
/// DOS's does.
mod master {
    pub const INVALID: &[u8] = b"Invalid partition table\0";
    pub const LOAD_FAILED: &[u8] = b"Error loading operating system\0";
    pub const MISSING: &[u8] = b"Missing operating system\0";
}

/// Where a master boot record moves itself, leaving `BOOTSTRAP` to the
/// boot record it loads.
const RELOCATED: u16 = 0x0600;
/// Reads of the active partition's boot record before the disk is given up.
const LOAD_TRIES: u16 = 5;

/// A fixed disk's master boot record with an empty partition table: code
/// that starts the active partition as DOS's does. It moves itself out of
/// the way, finds the one entry marked active with every other entry's
/// flag zero, reads that partition's boot record with five tries, a reset
/// between each, and starts it with DS:SI at its entry and DL the drive the
/// entry's flag names. With no entry active it takes INT 18h; a table, a
/// read or a record it cannot start is said, and the machine waits idle
/// for a restart.
///
/// # Errors
///
/// [`AsmError`] should its code not assemble or not fit before the table,
/// a defect of its own.
pub fn master_boot_record() -> Result<[u8; MBR_SECTOR_LEN], AsmError> {
    let table = RELOCATED + u16::try_from(PARTITION_TABLE_OFFSET).map_err(|_| AsmError::TooLong)?;
    let entry_len = i8::try_from(PARTITION_ENTRY_LEN).map_err(|_| AsmError::TooLong)?;
    let entries = u8::try_from(mbr::MAX_PRIMARY_PARTITIONS).map_err(|_| AsmError::TooLong)?;
    let signature_at = BOOTSTRAP + u16::try_from(at::SIGNATURE).map_err(|_| AsmError::TooLong)?;
    let mut asm = Asm::new(RELOCATED);
    let [moved, scan, active, rest, invalid, say, hold, load, read, loaded] =
        core::array::from_fn(|_| asm.label());
    let [invalid_text, failed_text, missing_text] = core::array::from_fn(|_| asm.label());
    asm.cli();
    asm.zero(Reg16::Ax);
    asm.mov_to_seg(Seg::Ss, Reg16::Ax);
    asm.mov_imm16(Reg16::Sp, BOOTSTRAP);
    asm.mov16(Reg16::Si, Reg16::Sp);
    asm.mov_to_seg(Seg::Es, Reg16::Ax);
    asm.mov_to_seg(Seg::Ds, Reg16::Ax);
    asm.sti();
    asm.cld();
    asm.mov_imm16(Reg16::Di, RELOCATED);
    asm.mov_imm16(
        Reg16::Cx,
        u16::try_from(MBR_SECTOR_LEN / 2).map_err(|_| AsmError::TooLong)?,
    );
    asm.rep_movsw();
    asm.jmp_far_label(0, &moved);
    asm.bind(&moved);
    asm.mov_imm16(Reg16::Si, table);
    asm.mov_imm8(Reg8::Bl, entries);
    asm.bind(&scan);
    asm.cmp_at_si(mbr::ACTIVE);
    asm.jcc(Cond::E, &active);
    asm.cmp_at_si(0);
    asm.jcc(Cond::Ne, &invalid);
    asm.add_imm8(Reg16::Si, entry_len);
    asm.dec8(Reg8::Bl);
    asm.jcc(Cond::Ne, &scan);
    asm.int(0x18);
    asm.bind(&active);
    asm.mov_from_si(Reg16::Dx, 0);
    asm.mov_from_si(Reg16::Cx, 2);
    asm.mov16(Reg16::Bp, Reg16::Si);
    asm.bind(&rest);
    asm.add_imm8(Reg16::Si, entry_len);
    asm.dec8(Reg8::Bl);
    asm.jcc(Cond::E, &load);
    asm.cmp_at_si(0);
    asm.jcc(Cond::E, &rest);
    asm.bind(&invalid);
    asm.mov_label(Reg16::Si, &invalid_text);
    asm.bind(&say);
    rom::teletype(&mut asm);
    asm.bind(&hold);
    asm.hlt();
    asm.jmp_short(&hold);
    asm.bind(&load);
    asm.mov_imm16(Reg16::Di, LOAD_TRIES);
    asm.bind(&read);
    asm.mov_imm16(Reg16::Bx, BOOTSTRAP);
    asm.mov_imm16(Reg16::Ax, 0x0201);
    asm.push(Reg16::Di);
    asm.int(0x13);
    asm.pop(Reg16::Di);
    asm.jcc(Cond::Ae, &loaded);
    asm.zero(Reg16::Ax);
    asm.int(0x13);
    asm.dec16(Reg16::Di);
    asm.jcc(Cond::Ne, &read);
    asm.mov_label(Reg16::Si, &failed_text);
    asm.jmp_short(&say);
    asm.bind(&loaded);
    asm.mov_label(Reg16::Si, &missing_text);
    asm.cmp_mem16(signature_at, u16::from_le_bytes(SIGNATURE));
    asm.jcc(Cond::Ne, &say);
    asm.mov16(Reg16::Si, Reg16::Bp);
    asm.jmp_far(0, BOOTSTRAP);
    asm.bind(&invalid_text);
    asm.bytes(master::INVALID);
    asm.bind(&failed_text);
    asm.bytes(master::LOAD_FAILED);
    asm.bind(&missing_text);
    asm.bytes(master::MISSING);
    let code = asm.finish()?;
    if code.len() > PARTITION_TABLE_OFFSET {
        return Err(AsmError::TooLong);
    }
    let mut sector = [0u8; MBR_SECTOR_LEN];
    sector[..code.len()].copy_from_slice(&code);
    sector[MBR_SECTOR_LEN - SIGNATURE.len()..].copy_from_slice(&SIGNATURE);
    Ok(sector)
}

/// A boot record's code: with `system`, the jump to the loader the ROM's
/// signature promises, its message for a machine without it.
fn volume_code(system: bool) -> Result<Vec<u8>, AsmError> {
    let origin = BOOTSTRAP + u16::try_from(at::CODE).unwrap_or(0);
    let mut asm = Asm::new(origin);
    let signature = asm.label();
    let message = asm.label();
    asm.cli();
    asm.zero(Reg16::Ax);
    asm.mov_to_seg(Seg::Ds, Reg16::Ax);
    asm.mov_to_seg(Seg::Ss, Reg16::Ax);
    asm.mov_imm16(Reg16::Sp, BOOTSTRAP);
    asm.sti();
    asm.cld();
    if system {
        let missing = asm.label();
        asm.mov_imm16(Reg16::Ax, ROM);
        asm.mov_to_seg(Seg::Es, Reg16::Ax);
        asm.mov_label(Reg16::Si, &signature);
        asm.mov_imm16(Reg16::Di, entry::SIGNATURE);
        asm.mov_imm16(Reg16::Cx, u16::try_from(rom::SIGNATURE.len()).unwrap_or(0));
        asm.repe_cmpsb();
        asm.jcc(Cond::Ne, &missing);
        asm.jmp_far(ROM, entry::LOADER);
        asm.bind(&missing);
    }
    asm.mov_label(Reg16::Si, &message);
    rom::teletype(&mut asm);
    asm.zero(Reg16::Ax);
    asm.int(0x16);
    asm.int(0x19);
    if system {
        asm.bind(&signature);
        asm.bytes(rom::SIGNATURE);
    }
    asm.bind(&message);
    asm.bytes(if system { NO_ROM } else { NO_SYSTEM });
    asm.finish()
}

/// How far the boot has gone, as [`boot::STAGE`] keeps it.
mod stage {
    /// The built-in devices are being initialised.
    pub const DEVICES: u8 = 0;
    /// `CONFIG.SYS` is being opened.
    pub const OPEN: u8 = 1;
    /// Its settings are being taken.
    pub const SETTINGS: u8 = 2;
    /// Its `DEVICE=` drivers are being loaded.
    pub const DRIVERS: u8 = 3;
    /// Its `INSTALL=` programs are being run.
    pub const INSTALLS: u8 = 4;
    /// The shell is being started.
    pub const SHELL: u8 = 5;
    /// The shell could not be started: its name is asked for.
    pub const ASK: u8 = 6;
}

/// The default settings: those `CONFIG.SYS` changes.
mod default {
    pub const FILES: u8 = 8;
    pub const BUFFERS: u8 = 15;
    pub const LOOKAHEAD: u8 = 1;
    pub const FCBS: u8 = 4;
    pub const LASTDRIVE: u8 = 5;
}

/// The most of `CONFIG.SYS` the boot reads.
const CONFIG_MAX: u16 = 0xFFF0;

/// The ROM loader was reached, the BIOS drive in DL: the kernel's data is
/// laid out and the boot begins.
pub fn start(k: &mut Kernel<'_, '_>) {
    let drive = k.g.r8(Reg8::Dl);
    lay_out(k);
    k.set8(BOOT + boot::BIOS_DRIVE, drive);
    k.set8(BOOT + boot::STAGE, stage::DEVICES);
    enter(k);
}

/// A program the boot ran has ended — an `INSTALL=` program, or the shell:
/// the boot goes on from where it was.
pub fn returned(k: &mut Kernel<'_, '_>) {
    if k.get8(BOOT + boot::STAGE) == stage::INSTALLS {
        advance_line(k);
    }
    enter(k);
}

/// Begins the boot's root: its caller's registers are the frame a program
/// it runs ends into, which returns to [`entry::BOOT_RETURN`].
fn enter(k: &mut Kernel<'_, '_>) {
    let frame = BOOT + boot::FRAME;
    k.g.write_bytes(SEGMENT, frame, &[0; 24]);
    k.set16(frame + Saved::Ds as u16 * 2, SEGMENT);
    k.set16(frame + Saved::Es as u16 * 2, SEGMENT);
    k.set16(frame + Saved::Ip as u16 * 2, entry::BOOT_RETURN);
    k.set16(frame + Saved::Cs as u16 * 2, ROM);
    k.set16(frame + Saved::Flags as u16 * 2, flag::IF | 0x0002);
    k.g.write16(dos_psp(), psp::STACK, frame);
    k.g.write16(dos_psp(), psp::STACK + 2, SEGMENT);
    k.g.set_stack(SEGMENT, frame);
    k.g.regs_mut().set_flag(flag::IF, true);
    let root = Frame::of(&Boot {
        device: 0,
        file: 0,
        header: Far::default(),
        load: 0,
        kept: 0,
    });
    engine::enter(k, &root, 0);
}

/// The kernel's PSP's segment.
const fn dos_psp() -> u16 {
    SEGMENT + DOS_PSP / 16
}

fn sysvar(field: i16) -> u16 {
    u16::try_from(i32::from(SYSVARS) + i32::from(field)).unwrap_or(0)
}

/// A far jump to `ROM:offset`, as the built-in headers' entries are.
fn far_jump(offset: u16) -> [u8; 5] {
    let [low, high] = offset.to_le_bytes();
    let [seg_low, seg_high] = ROM.to_le_bytes();
    [0xEA, low, high, seg_low, seg_high]
}

/// The name a built-in device's header carries, and its attributes.
const fn builtin(device: Builtin) -> (&'static [u8; 8], u16) {
    use attribute::{
        CHARACTER, CLOCK, FAST, GENERIC, OPEN_CLOSE, QUERY, STDIN, STDOUT, UNTIL_BUSY,
    };
    match device {
        Builtin::Con => (device::CON, CHARACTER | FAST | STDOUT | STDIN),
        Builtin::Aux => (b"AUX     ", CHARACTER),
        Builtin::Prn => (b"PRN     ", CHARACTER | UNTIL_BUSY | GENERIC | QUERY),
        Builtin::Clock => (b"CLOCK$  ", CHARACTER | CLOCK),
        Builtin::Block => (&[0; 8], OPEN_CLOSE | GENERIC | QUERY | STDOUT),
        Builtin::Com1 => (b"COM1    ", CHARACTER),
        Builtin::Lpt1 => (b"LPT1    ", CHARACTER | UNTIL_BUSY | GENERIC | QUERY),
        Builtin::Lpt2 => (b"LPT2    ", CHARACTER | UNTIL_BUSY | GENERIC | QUERY),
        Builtin::Lpt3 => (b"LPT3    ", CHARACTER | UNTIL_BUSY | GENERIC | QUERY),
        Builtin::Com2 => (b"COM2    ", CHARACTER),
        Builtin::Com3 => (b"COM3    ", CHARACTER),
        Builtin::Com4 => (b"COM4    ", CHARACTER),
    }
}

/// Lays out the kernel's data: the device chain, the List of Lists, the
/// swappable data area, the first SFT block, the drives' tables, the
/// vectors, the kernel's PSP, and the arena with the boot buffers in it.
fn lay_out(k: &mut Kernel<'_, '_>) {
    let zeros = [0u8; 0x100];
    for at in (0..END).step_by(zeros.len()) {
        k.g.write_bytes(
            SEGMENT,
            at,
            &zeros[..usize::from(END - at).min(zeros.len())],
        );
    }
    let chain = Builtin::ALL;
    for (n, device) in chain.into_iter().enumerate() {
        let at = device.header();
        let next = chain
            .get(n + 1)
            .map_or(Far::new(0xFFFF, 0xFFFF), |d| Far::new(SEGMENT, d.header()));
        let (name, attributes) = builtin(device);
        k.set_far(at + header::NEXT, next);
        k.set16(at + header::ATTRIBUTES, attributes);
        k.set16(at + header::STRATEGY, at + STRATEGY_JUMP);
        k.set16(at + header::INTERRUPT, at + INTERRUPT_JUMP);
        k.g.write_bytes(SEGMENT, at + header::NAME, name);
        k.g.write_bytes(SEGMENT, at + STRATEGY_JUMP, &far_jump(entry::STRATEGY));
        k.g.write_bytes(
            SEGMENT,
            at + INTERRUPT_JUMP,
            &far_jump(rom::driver_entry(device)),
        );
    }
    k.set_far(NUL + header::NEXT, Far::new(SEGMENT, Builtin::Con.header()));
    k.set16(
        NUL + header::ATTRIBUTES,
        attribute::CHARACTER | attribute::NUL,
    );
    k.set16(NUL + header::STRATEGY, NUL_JUMPS);
    k.set16(NUL + header::INTERRUPT, NUL_JUMPS);
    k.g.write_bytes(SEGMENT, NUL + header::NAME, b"NUL     ");
    k.g.write_bytes(SEGMENT, NUL_JUMPS, &far_jump(entry::NUL));

    let top = memory_top(k);
    let arena = SEGMENT + END / 16;
    k.set16(sysvar(sysvars::SHARE_RETRIES), 3);
    k.set16(sysvar(sysvars::SHARE_DELAY), 1);
    k.set16(sysvar(sysvars::FIRST_MCB), arena);
    k.set_far(sysvar(sysvars::FIRST_DPB), Far::new(0xFFFF, 0xFFFF));
    k.set_far(sysvar(sysvars::FIRST_SFT), Far::new(SEGMENT, SFT));
    k.set_far(
        sysvar(sysvars::CLOCK),
        Far::new(SEGMENT, Builtin::Clock.header()),
    );
    k.set_far(
        sysvar(sysvars::CON),
        Far::new(SEGMENT, Builtin::Con.header()),
    );
    k.set16(sysvar(sysvars::MAX_SECTOR), super::data::SECTOR_MAX);
    k.set_far(
        sysvar(sysvars::BUFFER_INFO),
        Far::new(SEGMENT, driver::BUFFERS),
    );
    k.set_far(sysvar(sysvars::CDS), Far::new(SEGMENT, CDS));
    k.set_far(sysvar(sysvars::FCBS), Far::new(0xFFFF, 0xFFFF));
    k.set8(sysvar(sysvars::LASTDRIVE), default::LASTDRIVE);
    // A boot gives the shared folders their drives again once its drivers
    // have taken theirs.
    super::redirect::take_back(k);
    k.set_far(sysvar(sysvars::SETVER), Far::default());
    k.set16(sysvar(sysvars::BUFFERS), BOOT_BUFFER_COUNT);
    k.set16(sysvar(sysvars::UNREAD_CON), driver::CON_LINE);
    k.set8(sysvar(sysvars::JOINED), 0);
    k.set8(sysvar(sysvars::DWORD_MOVES), 0);
    k.set16(sysvar(sysvars::EXTENDED_KIB), 0);

    k.set_sda8(sda::ERROR_DRIVE, 0xFF);
    k.set_sda8(sda::SWITCH, b'/');
    k.set_sda16(sda::PSP, dos_psp());
    k.set_dta(Far::new(dos_psp(), psp::TAIL));
    k.set16(driver::COUNTRY, super::country::COUNTRIES[0].code);

    k.set_far(SFT, Far::new(0xFFFF, 0xFFFF));
    k.set16(SFT + 4, SFT_FIRST);

    for drive in 0..DRIVES {
        let at = cds_at(drive);
        k.g.write_bytes(SEGMENT, at + cds::PATH, &[b'A' + drive, b':', b'\\', 0]);
        k.set16(at + cds::ROOT, 2);
        k.set16(at + cds::ID_HIGH, 0);
        k.set16(at + cds::USER_WORD, 0);
        k.set_far(dpb_at(drive) + dpb::NEXT, Far::new(0xFFFF, 0xFFFF));
    }

    vectors(k);
    k.g.write_bytes(arena, mcb::KIND, &[mcb::LAST]);
    k.g.write16(arena, mcb::OWNER, mcb::FREE);
    k.g.write16(arena, mcb::SIZE, top.saturating_sub(arena + 1));
    k.create_psp(dos_psp(), top, dos_psp(), false);
    let paragraphs = BOOT_BUFFER_COUNT * BUFFER / 16;
    let buffers = k
        .allocate(paragraphs, mcb::DOS, strategy::FIRST)
        .unwrap_or(arena + 1);
    k.name_block(buffers, b"SD");
    k.set16(BOOT + boot::BOOT_BUFFERS, buffers);
    use_buffers(k, buffers, BOOT_BUFFER_COUNT);
    k.set8(BOOT + boot::FILES, default::FILES);
    k.set8(BOOT + boot::BUFFERS, default::BUFFERS);
    k.set8(BOOT + boot::LOOKAHEAD, default::LOOKAHEAD);
    k.set8(BOOT + boot::FCBS, default::FCBS);
    k.set8(BOOT + boot::LASTDRIVE, default::LASTDRIVE);
}

/// The segment past conventional memory, as the BIOS counts it.
fn memory_top(k: &mut Kernel<'_, '_>) -> u16 {
    let kib =
        k.g.read16(crate::bios::bda::SEGMENT, crate::bios::bda::MEMORY_KIB)
            .min(640);
    kib * 64
}

/// The kernel's interrupt vectors, and the CP/M entry's jump at `0:00C0`.
fn vectors(k: &mut Kernel<'_, '_>) {
    let set = |k: &mut Kernel<'_, '_>, vector: u16, offset: u16| {
        k.g.write16(0, vector * 4, offset);
        k.g.write16(0, vector * 4 + 2, ROM);
    };
    set(k, 0x20, entry::INT20);
    set(k, 0x21, entry::INT21);
    set(k, 0x22, entry::BOOT_RETURN);
    set(k, 0x23, entry::INT23);
    set(k, 0x24, entry::INT24);
    set(k, 0x25, entry::INT25);
    set(k, 0x26, entry::INT26);
    set(k, 0x27, entry::INT27);
    set(k, 0x29, entry::INT29);
    set(k, 0x2F, entry::INT2F);
    set(k, 0x1B, entry::INT1B);
    // A boot through INT 19h finds the vector DOS left.
    let bios13 = Far::new(k.g.read16(0, 0x13 * 4 + 2), k.g.read16(0, 0x13 * 4));
    if bios13 != Far::new(ROM, entry::INT13) {
        k.set_far(driver::INT13, bios13);
    }
    set(k, 0x13, entry::INT13);
    for vector in [0x28, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E] {
        set(k, vector, bios::IRET);
    }
    k.g.write_bytes(0, 0xC0, &far_jump(entry::CALL5));
}

/// Makes the `count` buffers at `first` the disk buffers, every one empty.
fn use_buffers(k: &mut Kernel<'_, '_>, first: u16, count: u16) {
    k.set16(driver::BUFFERS, first);
    k.set16(driver::BUFFERS + 2, count);
    k.set16(driver::BUFFER_LAST, 0);
    k.set_far(sysvar(sysvars::CURRENT_BUFFER), Far::new(first, 0));
    for n in 0..count {
        let segment = first + n * (BUFFER / 16);
        k.g.write_bytes(segment, 0, &[0; super::data::BUFFER_HEADER as usize]);
        k.g.write8(segment, super::data::buffer::DRIVE, FREE);
    }
}

/// The active console's header.
fn console(k: &mut Kernel<'_, '_>) -> Far {
    k.get_far(sysvar(sysvars::CON))
}

/// The bytes left of `CONFIG.SYS` from its next line, and that line.
fn next_line(k: &mut Kernel<'_, '_>) -> Option<(u16, Vec<u8>)> {
    let segment = k.get16(BOOT + boot::TEXT);
    let len = k.get16(BOOT + boot::TEXT_LEN);
    let at = k.get16(BOOT + boot::LINE);
    if segment == 0 || at >= len {
        return None;
    }
    let mut line = Vec::new();
    let mut end = at;
    while end < len {
        let byte = k.g.read8(segment, end);
        if byte == b'\n' || byte == super::char::CTRL_Z {
            break;
        }
        if byte != b'\r' {
            line.push(byte);
        }
        end += 1;
    }
    Some((at, line))
}

/// Moves on past the line [`next_line`] gives.
fn advance_line(k: &mut Kernel<'_, '_>) {
    let segment = k.get16(BOOT + boot::TEXT);
    let len = k.get16(BOOT + boot::TEXT_LEN);
    let mut at = k.get16(BOOT + boot::LINE);
    while at < len {
        let byte = k.g.read8(segment, at);
        at += 1;
        if byte == b'\n' {
            break;
        }
        if byte == super::char::CTRL_Z {
            at = len;
        }
    }
    k.set16(BOOT + boot::LINE, at);
    let number = k.get16(BOOT + boot::LINE_NUMBER).saturating_add(1);
    k.set16(BOOT + boot::LINE_NUMBER, number);
}

/// Goes back to `CONFIG.SYS`'s first line for another pass.
fn rewind(k: &mut Kernel<'_, '_>) {
    k.set16(BOOT + boot::LINE, 0);
    k.set16(BOOT + boot::LINE_NUMBER, 1);
}

/// A line of `CONFIG.SYS`, as the boot reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Directive<'a> {
    /// `BREAK=ON` or `OFF`.
    Break(bool),
    /// `BUFFERS=n[,m]`.
    Buffers(u8, u8),
    /// `FILES=n`.
    Files(u8),
    /// `FCBS=x[,y]`.
    Fcbs(u8, u8),
    /// `LASTDRIVE=x`, as a count of drives.
    LastDrive(u8),
    /// `STACKS=n,s`.
    Stacks(u8, u16),
    /// `COUNTRY=xxx[,yyy[,path]]`.
    Country(u16),
    /// `SHELL=path args`.
    Shell(&'a [u8]),
    /// `DEVICE=` or `DEVICEHIGH=`: the driver's path and arguments, and
    /// where they start in the line.
    Device(&'a [u8], usize),
    /// `INSTALL=path args`.
    Install(&'a [u8]),
    /// `DOS=`: the high memory area and upper memory blocks, which a PC or
    /// XT has none of; `true` when the line asks for either.
    Dos(bool),
    /// A blank line or a remark.
    Remark,
    /// A known command with a value it cannot take.
    Invalid,
    /// A command DOS does not know.
    Unknown,
}

/// What `line` of `CONFIG.SYS` says.
#[must_use]
pub fn directive(line: &[u8]) -> Directive<'_> {
    let blank = |b: &u8| *b == b' ' || *b == b'\t';
    let start = line.iter().position(|b| !blank(b)).unwrap_or(line.len());
    let line = &line[start..];
    if line.is_empty() || line[0] == b';' {
        return Directive::Remark;
    }
    let keyword_end = line
        .iter()
        .position(|&b| b == b'=' || blank(&b))
        .unwrap_or(line.len());
    let keyword = &line[..keyword_end];
    let mut at = keyword_end;
    while at < line.len() && blank(&line[at]) {
        at += 1;
    }
    if at < line.len() && line[at] == b'=' {
        at += 1;
    }
    while at < line.len() && blank(&line[at]) {
        at += 1;
    }
    let value_end = line
        .iter()
        .rposition(|b| !blank(b))
        .map_or(at, |end| (end + 1).max(at));
    let value = &line[at..value_end];
    let is = |name: &[u8]| keyword.eq_ignore_ascii_case(name);
    match () {
        () if is(b"REM") => Directive::Remark,
        () if is(b"BREAK") => match_on_off(value).map_or(Directive::Invalid, Directive::Break),
        () if is(b"BUFFERS") => match numbers(value) {
            Some((n, m)) if (1..=99).contains(&n) && m.is_none_or(|m| m <= 8) => {
                Directive::Buffers(small(n), m.map_or(default::LOOKAHEAD, small))
            }
            _ => Directive::Invalid,
        },
        () if is(b"FILES") => match numbers(value) {
            Some((n, None)) if (8..=255).contains(&n) => Directive::Files(small(n)),
            _ => Directive::Invalid,
        },
        () if is(b"FCBS") => match numbers(value) {
            Some((x, y)) if (1..=255).contains(&x) && y.is_none_or(|y| y <= x) => {
                Directive::Fcbs(small(x), y.map_or(0, small))
            }
            _ => Directive::Invalid,
        },
        () if is(b"LASTDRIVE") => match value {
            [letter] if letter.is_ascii_alphabetic() => {
                Directive::LastDrive(letter.to_ascii_uppercase() - b'A' + 1)
            }
            _ => Directive::Invalid,
        },
        () if is(b"STACKS") => match numbers(value) {
            Some((0, Some(0))) => Directive::Stacks(0, 0),
            Some((n, Some(s)))
                if u8::try_from(n).is_ok_and(|n| stacks::COUNTS.contains(&n))
                    && u16::try_from(s).is_ok_and(|s| stacks::SIZES.contains(&s)) =>
            {
                Directive::Stacks(small(n), u16::try_from(s).unwrap_or(0))
            }
            _ => Directive::Invalid,
        },
        () if is(b"COUNTRY") => {
            let mut parts = value.split(|&b| b == b',');
            let code = parts.next().and_then(decimal);
            let page = parts.next().filter(|p| !p.is_empty()).map(decimal);
            match (code, page) {
                (Some(code), None | Some(Some(437))) if (1..=999).contains(&code) => {
                    Directive::Country(u16::try_from(code).unwrap_or(0))
                }
                _ => Directive::Invalid,
            }
        }
        () if is(b"SHELL") => Directive::Shell(value),
        () if is(b"DEVICE") => Directive::Device(value, start + at),
        () if is(b"DEVICEHIGH") => {
            let (value, skipped) = without_size(value);
            Directive::Device(value, start + at + skipped)
        }
        () if is(b"INSTALL") => Directive::Install(value),
        () if is(b"DOS") => {
            let asks = value.split(|&b| b == b',').any(|part| {
                let part = trim(part);
                part.eq_ignore_ascii_case(b"HIGH") || part.eq_ignore_ascii_case(b"UMB")
            });
            Directive::Dos(asks)
        }
        () => Directive::Unknown,
    }
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&b| b != b' ' && b != b'\t')
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| b != b' ' && b != b'\t')
        .map_or(start, |e| e + 1);
    &bytes[start..end]
}

fn small(n: u32) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

fn match_on_off(value: &[u8]) -> Option<bool> {
    if value.eq_ignore_ascii_case(b"ON") {
        Some(true)
    } else if value.eq_ignore_ascii_case(b"OFF") {
        Some(false)
    } else {
        None
    }
}

/// A decimal number of at most five digits.
fn decimal(text: &[u8]) -> Option<u32> {
    let text = trim(text);
    if text.len() > 5 {
        return None;
    }
    crate::dos::shell::parse::number(text)
}

/// `n` or `n,m`.
fn numbers(value: &[u8]) -> Option<(u32, Option<u32>)> {
    let mut parts = value.split(|&b| b == b',');
    let first = decimal(parts.next()?)?;
    let second = match parts.next() {
        Some(part) => Some(decimal(part)?),
        None => None,
    };
    parts.next().is_none().then_some((first, second))
}

/// `DEVICEHIGH`'s value without its `SIZE=hhhh`, and the bytes skipped.
fn without_size(value: &[u8]) -> (&[u8], usize) {
    if !value
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"SIZE="))
    {
        return (value, 0);
    }
    let end = value[5..]
        .iter()
        .position(|&b| b == b' ' || b == b'\t')
        .map_or(value.len(), |n| n + 5);
    let start = value[end..]
        .iter()
        .position(|&b| b != b' ' && b != b'\t')
        .map_or(value.len(), |n| n + end);
    (&value[start..], start)
}

/// A line's first word, its path, and the rest after it.
fn split_command(value: &[u8]) -> (&[u8], &[u8]) {
    let end = value
        .iter()
        .position(|&b| b == b' ' || b == b'\t' || b == b'/')
        .unwrap_or(value.len());
    (&value[..end], &value[end..])
}

locals! {
    /// The boot. Its progress lives in the kernel's data, which the programs
    /// it runs leave as it was: the built-in devices initialised one by one
    /// (`device`), `CONFIG.SYS` read through SFT entry `file`, each
    /// `DEVICE=` driver loaded at `load` and each header in it (`header`)
    /// initialised and linked.
    pub struct Boot {
        device: u8,
        file: u8,
        header: Far,
        load: u16,
        kept: u16,
    }
}

/// The boot's phases, within each stage.
mod phase {
    pub const INITIALISED: u8 = 1;
    pub const SAID: u8 = 2;
    pub const OPENED: u8 = 3;
    pub const READ: u8 = 4;
    pub const CLOSED: u8 = 5;
    pub const SIZED: u8 = 6;
    pub const SIZE_CLOSED: u8 = 7;
    pub const LOADED: u8 = 8;
    pub const RAN: u8 = 10;
    pub const ASKED: u8 = 11;
    pub const ANSWERED: u8 = 12;
}

impl Routine for Boot {
    const ID: Id = Id::Boot;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match k.get8(BOOT + boot::STAGE) {
            stage::DEVICES => self.devices(k, phase, child),
            stage::OPEN => self.open_config(k, phase, child),
            stage::SETTINGS => self.settings(k, phase),
            stage::DRIVERS => self.drivers(k, phase, child),
            stage::INSTALLS => self.installs(k, phase, child),
            stage::SHELL => self.shell(k, phase, child),
            _ => self.ask(k, phase, child),
        }
    }
}

impl Boot {
    fn set_stage(k: &mut Kernel<'_, '_>, phase: &mut u8, next: u8) {
        k.set8(BOOT + boot::STAGE, next);
        *phase = 0;
    }

    /// Writes `text` to the console; the step goes on at `next`.
    fn say(k: &mut Kernel<'_, '_>, phase: &mut u8, next: u8, text: &[u8]) -> Flow {
        let room = usize::from(boot::LEN - boot::MESSAGE);
        let len = text.len().min(room);
        k.g.write_bytes(SEGMENT, BOOT + boot::MESSAGE, &text[..len]);
        let device = console(k);
        *phase = next;
        Flow::Call(Frame::of(&CharIo {
            device,
            command: command::OUTPUT,
            buffer: Far::new(SEGMENT, BOOT + boot::MESSAGE),
            count: u16::try_from(len).unwrap_or(0),
            done: 0,
            status: 0,
        }))
    }

    /// The built-in devices, initialised through their headers in the
    /// chain's order; the block driver's units become drives.
    fn devices(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == phase::INITIALISED {
            let device = Builtin::ALL[usize::from(self.device)];
            if device == Builtin::Block
                && child.is_ok_and(|word| word & u32::from(status::ERROR) == 0)
            {
                let units = k.request8(field::MEDIA);
                let array = k.request_far(field::COUNT);
                let header = Far::new(SEGMENT, device.header());
                Self::block_units(k, header, units, array);
            }
            self.device += 1;
        }
        if *phase == phase::SAID {
            Self::set_stage(k, phase, stage::OPEN);
            return self.open_config(k, phase, Ok(0));
        }
        let Some(&device) = Builtin::ALL.get(usize::from(self.device)) else {
            Self::drives_ready(k);
            return Self::say(k, phase, phase::SAID, b"\r\nStarting TAIRiX DOS...\r\n\r\n");
        };
        k.set8(BOOT + boot::MESSAGE, b'\r');
        k.set8(BOOT + boot::MESSAGE + 1, b'\n');
        Self::init(k, phase, Far::new(SEGMENT, BOOT + boot::MESSAGE));
        Flow::Call(Frame::of(&Device {
            header: Far::new(SEGMENT, device.header()),
        }))
    }

    /// Sets up an `INIT` request, its command line at `line`.
    fn init(k: &mut Kernel<'_, '_>, phase: &mut u8, line: Far) {
        k.request(0, command::INIT);
        k.set_request_far(field::ADDRESS, Far::default());
        k.set_request_far(field::COUNT, line);
        let next = k.get8(BOOT + boot::NEXT_DRIVE);
        k.set_request8(field::FIRST_DRIVE, next);
        *phase = phase::INITIALISED;
    }

    /// A block driver's `units`, their BPBs' near pointers at `array`, made
    /// drives from the next free letter.
    fn block_units(k: &mut Kernel<'_, '_>, header: Far, units: u8, array: Far) {
        let first = k.get8(BOOT + boot::NEXT_DRIVE);
        let units = units.min(DRIVES - first.min(DRIVES));
        for unit in 0..units {
            let pointer = Far::new(array.segment, k.read16_at(array.add(u16::from(unit) * 2)));
            let mut bpb = [0u8; tairix_fat::boot::DOS_BPB];
            k.g.read_bytes(pointer.segment, pointer.offset, &mut bpb);
            let drive = first + unit;
            if k.build_dpb(drive, unit, header, &bpb) {
                let at = dpb_at(drive);
                k.set8(at + dpb::ACCESSED, 0xFF);
                let entry = cds_at(drive);
                k.set16(entry + cds::FLAGS, cds::PHYSICAL);
                k.set_far(entry + cds::DPB, Far::new(SEGMENT, at));
                k.set16(entry + cds::CLUSTER, 0);
            }
        }
        k.g.write8(
            header.segment,
            header.offset.wrapping_add(header::NAME),
            units,
        );
        k.set8(BOOT + boot::NEXT_DRIVE, first + units);
        let count = k.get8(sysvar(sysvars::BLOCK_DEVICES)).saturating_add(units);
        k.set8(sysvar(sysvars::BLOCK_DEVICES), count);
        link_dpbs(k);
    }

    /// The built-ins are ready: the boot drive found among the drives.
    fn drives_ready(k: &mut Kernel<'_, '_>) {
        let bios = k.get8(BOOT + boot::BIOS_DRIVE);
        let units = super::drivers::units_of(k.g);
        let drive = (0..units)
            .find(|&n| k.get8(UNITS + u16::from(n) * UNIT + unit::DRIVE) == bios)
            .unwrap_or(0);
        k.set_sda8(sda::DRIVE, drive);
        k.set8(sysvar(sysvars::BOOT_DRIVE), drive + 1);
        let drives = k.get8(BOOT + boot::NEXT_DRIVE).max(default::LASTDRIVE);
        k.set8(sysvar(sysvars::LASTDRIVE), drives);
        let letter = b'A' + drive;
        let mut shell = Vec::new();
        shell.extend_from_slice(&[letter, b':', b'\\']);
        shell.extend_from_slice(b"COMMAND.COM ");
        shell.extend_from_slice(&[letter, b':', b'\\']);
        shell.extend_from_slice(b" /P");
        set_text(k, BOOT + boot::SHELL, &shell);
    }

    /// Opens `CONFIG.SYS` on the boot drive and reads it into a block of its
    /// own; without one the defaults stand.
    fn open_config(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::OPENED => {
                let Ok(index) = child else {
                    return self.settings_done(k, phase);
                };
                self.file = u8::try_from(index).unwrap_or(0);
                let Some(entry) = k.sft(u16::from(self.file)) else {
                    return self.settings_done(k, phase);
                };
                let size = u16::try_from(
                    k.sft32(entry, super::data::sft::SIZE)
                        .min(u32::from(CONFIG_MAX)),
                )
                .unwrap_or(0);
                let rule = k.strategy();
                let Ok(segment) = k.allocate(size.div_ceil(16).max(1), mcb::DOS, rule) else {
                    return self.close_config(phase);
                };
                k.name_block(segment, b"SD");
                k.set16(BOOT + boot::TEXT, segment);
                *phase = phase::READ;
                Flow::Call(Frame::of(&super::file::Read {
                    index: u16::from(self.file),
                    buffer: Far::new(segment, 0),
                    count: size,
                    done: 0,
                    steps: 0,
                }))
            }
            phase::READ => {
                let read = u16::try_from(child.unwrap_or(0)).unwrap_or(0);
                k.set16(BOOT + boot::TEXT_LEN, read);
                self.close_config(phase)
            }
            phase::CLOSED => {
                rewind(k);
                Self::set_stage(k, phase, stage::SETTINGS);
                self.settings(k, phase)
            }
            _ => {
                let drive = k.current_drive();
                let mut path = Vec::from(&b"A:\\CONFIG.SYS"[..]);
                path[0] = b'A' + drive;
                k.set_path(level::PATH, &path);
                *phase = phase::OPENED;
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

    fn close_config(&self, phase: &mut u8) -> Flow {
        *phase = phase::CLOSED;
        Flow::Call(Frame::of(&Close {
            handle: 0,
            index: u16::from(self.file),
            by_index: true,
        }))
    }

    /// `CONFIG.SYS`'s settings, a line at a time; a bad line's message
    /// written before the next is taken.
    fn settings(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        if *phase == phase::SAID {
            advance_line(k);
            *phase = 0;
        }
        while let Some((_, line)) = next_line(k) {
            let message: Option<&[u8]> = match directive(&line) {
                Directive::Break(on) => {
                    k.set_sda8(sda::BREAK, u8::from(on));
                    None
                }
                Directive::Buffers(n, m) => {
                    k.set8(BOOT + boot::BUFFERS, n);
                    k.set8(BOOT + boot::LOOKAHEAD, m);
                    None
                }
                Directive::Files(n) => {
                    k.set8(BOOT + boot::FILES, n);
                    None
                }
                Directive::Fcbs(x, y) => {
                    k.set8(BOOT + boot::FCBS, x);
                    k.set8(BOOT + boot::PROTECTED, y);
                    None
                }
                Directive::LastDrive(n) => {
                    k.set8(BOOT + boot::LASTDRIVE, n);
                    None
                }
                Directive::Stacks(n, s) => {
                    k.set8(BOOT + boot::STACKS, n);
                    k.set16(BOOT + boot::STACK_SIZE, s);
                    None
                }
                Directive::Country(code) => {
                    (!k.set_country(code)).then_some(&b"Invalid country code\r\n"[..])
                }
                Directive::Shell(value) => {
                    set_text(k, BOOT + boot::SHELL, value);
                    None
                }
                Directive::Dos(true) => Some(&b"HMA not available: Loading DOS low\r\n"[..]),
                Directive::Device(..)
                | Directive::Install(_)
                | Directive::Dos(false)
                | Directive::Remark => None,
                Directive::Invalid => Some(&b"Invalid value in CONFIG.SYS\r\n"[..]),
                Directive::Unknown => Some(&b"Unrecognized command in CONFIG.SYS\r\n"[..]),
            };
            if let Some(text) = message {
                let mut text = Vec::from(text);
                name_line(&mut text, k.get16(BOOT + boot::LINE_NUMBER));
                return Self::say(k, phase, phase::SAID, &text);
            }
            advance_line(k);
        }
        self.settings_done(k, phase)
    }

    /// The settings taken: the tables they size made, the boot buffers
    /// given back, and the drivers' pass begun.
    fn settings_done(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        apply(k);
        rewind(k);
        Self::set_stage(k, phase, stage::DRIVERS);
        self.drivers(k, phase, Ok(0))
    }

    /// `CONFIG.SYS`'s `DEVICE=` drivers, in order: each file sized, loaded
    /// as an overlay into the largest free block, every header in it
    /// initialised and linked, and the block cut to what they keep.
    fn drivers(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::SIZED => {
                let Ok(index) = child else {
                    return Self::bad_driver(k, phase);
                };
                self.file = u8::try_from(index).unwrap_or(0);
                let size = k
                    .sft(u16::from(self.file))
                    .map_or(u32::MAX, |entry| k.sft32(entry, super::data::sft::SIZE));
                let largest = match k.allocate(0xFFFF, mcb::DOS, strategy::FIRST) {
                    Err(super::memory::Refusal::Short(largest)) => largest,
                    _ => 0,
                };
                self.load = 0;
                if u32::from(largest) * 16 > size + 16 {
                    if let Ok(segment) = k.allocate(largest, mcb::DOS, strategy::FIRST) {
                        self.load = segment;
                    }
                }
                *phase = phase::SIZE_CLOSED;
                Flow::Call(Frame::of(&Close {
                    handle: 0,
                    index: u16::from(self.file),
                    by_index: true,
                }))
            }
            phase::SIZE_CLOSED => {
                if self.load == 0 {
                    return Self::say(
                        k,
                        phase,
                        phase::SAID,
                        b"Insufficient memory for device driver\r\n",
                    );
                }
                let block = BOOT + boot::BLOCK;
                k.set16(block, self.load);
                k.set16(block + 2, self.load);
                *phase = phase::LOADED;
                Flow::Call(Frame::of(&Exec {
                    mode: mode::OVERLAY,
                    block: Far::new(SEGMENT, block),
                    file: 0,
                    environment: 0,
                    psp: 0,
                    load: 0,
                    done: 0,
                    relocated: 0,
                    code: 0,
                }))
            }
            phase::LOADED => {
                if child.is_err() {
                    let _ = k.free(self.load);
                    return Self::bad_driver(k, phase);
                }
                self.header = Far::new(self.load, 0);
                self.kept = 0;
                self.init_header(k, phase)
            }
            phase::INITIALISED => self.header_ready(k, phase, child),
            phase::SAID => {
                advance_line(k);
                *phase = 0;
                self.drivers(k, phase, Ok(0))
            }
            _ => {
                while let Some((_, line)) = next_line(k) {
                    if let Directive::Device(value, _) = directive(&line) {
                        let (path, _) = split_command(value);
                        k.set_path(level::PATH, path);
                        *phase = phase::SIZED;
                        return Flow::Call(Frame::of(&Open {
                            mode: access::READ,
                            keep_handle: true,
                            index: 0,
                            handle: 0,
                            drive: 0,
                        }));
                    }
                    advance_line(k);
                }
                standard_handles(k);
                rewind(k);
                Self::set_stage(k, phase, stage::INSTALLS);
                self.installs(k, phase, Ok(0))
            }
        }
    }

    /// Sends `INIT` to the header the loaded driver is at, its command line
    /// the `DEVICE=` line's value as `CONFIG.SYS` holds it.
    fn init_header(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let segment = k.get16(BOOT + boot::TEXT);
        let at = k.get16(BOOT + boot::LINE);
        let line = next_line(k).map(|(_, line)| line).unwrap_or_default();
        let start = match directive(&line) {
            Directive::Device(_, start) => u16::try_from(start).unwrap_or(0),
            _ => 0,
        };
        Self::init(k, phase, Far::new(segment, at.wrapping_add(start)));
        Flow::Call(Frame::of(&Device {
            header: self.header,
        }))
    }

    /// A header of the loaded driver answered `INIT`: linked in when it
    /// took, its units made drives; then the next header in the file, or the
    /// block cut to what the drivers keep.
    fn header_ready(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let word =
            u16::try_from(child.unwrap_or(u32::from(status::ERROR))).unwrap_or(status::ERROR);
        let next = k.far_at(self.header.add(header::NEXT));
        let end = k.request_far(field::ADDRESS);
        let kept = u32::from(end.segment.wrapping_sub(self.load)) * 16 + u32::from(end.offset);
        let took = word & status::ERROR == 0 && kept > u32::from(self.header.offset);
        if took {
            let attributes = k.attributes(self.header);
            if attributes & attribute::CHARACTER == 0 {
                let units = k.request8(field::MEDIA);
                let array = k.request_far(field::COUNT);
                Self::block_units(k, self.header, units, array);
            } else if attributes & (attribute::STDIN | attribute::STDOUT) != 0 {
                k.set_far(sysvar(sysvars::CON), self.header);
            }
            if attributes & attribute::CLOCK != 0 {
                k.set_far(sysvar(sysvars::CLOCK), self.header);
            }
            let first = k.get_far(NUL + header::NEXT);
            k.write16_at(self.header.add(header::NEXT), first.offset);
            k.write16_at(self.header.add(header::NEXT + 2), first.segment);
            k.set_far(NUL + header::NEXT, self.header);
            self.kept = self
                .kept
                .max(u16::try_from(kept.div_ceil(16)).unwrap_or(u16::MAX));
        }
        if next.offset != 0xFFFF && next.offset > self.header.offset {
            self.header = Far::new(self.load, next.offset);
            return self.init_header(k, phase);
        }
        let paragraphs = self.kept;
        if paragraphs == 0 {
            let _ = k.free(self.load);
            return Self::bad_driver(k, phase);
        }
        let _ = k.resize(self.load, paragraphs);
        let (path, len) = k.path(level::PATH);
        k.name_block(self.load, &program_name(&path[..len]));
        advance_line(k);
        *phase = 0;
        self.drivers(k, phase, Ok(0))
    }

    fn bad_driver(k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let (path, len) = k.path(level::PATH);
        let mut text = Vec::from(&b"Bad or missing "[..]);
        text.extend_from_slice(&path[..len]);
        text.extend_from_slice(b"\r\n");
        name_line(&mut text, k.get16(BOOT + boot::LINE_NUMBER));
        Self::say(k, phase, phase::SAID, &text)
    }

    /// `CONFIG.SYS`'s `INSTALL=` programs, each run to its end — a resident
    /// one stays — before the next.
    fn installs(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == phase::RAN {
            if child.is_err() {
                return Self::bad_driver(k, phase);
            }
            return Flow::Transfer;
        }
        if *phase == phase::SAID {
            advance_line(k);
            *phase = 0;
        }
        while let Some((_, line)) = next_line(k) {
            if let Directive::Install(value) = directive(&line) {
                let (path, tail) = split_command(value);
                k.set_path(level::PATH, path);
                set_tail(k, tail);
                return Self::run(k, phase);
            }
            advance_line(k);
        }
        super::redirect::assign(k);
        Self::set_stage(k, phase, stage::SHELL);
        self.shell(k, phase, Ok(0))
    }

    /// Runs the program the running call's path names, its command tail
    /// the one the boot keeps; it ends into the boot.
    fn run(k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let block = BOOT + boot::BLOCK;
        k.set16(block, 0);
        k.set_far(block + 2, Far::new(SEGMENT, BOOT + boot::TAIL));
        k.set_far(block + 6, Far::new(SEGMENT, BOOT + boot::FCB));
        k.set_far(block + 10, Far::new(SEGMENT, BOOT + boot::FCB));
        k.set8(BOOT + boot::FCB, 0);
        k.g.write_bytes(SEGMENT, BOOT + boot::FCB + 1, &[b' '; 11]);
        k.set_sda16(sda::PSP, dos_psp());
        *phase = phase::RAN;
        Flow::Call(Frame::of(&Exec {
            mode: mode::RUN,
            block: Far::new(SEGMENT, block),
            file: 0,
            environment: 0,
            psp: 0,
            load: 0,
            done: 0,
            relocated: 0,
            code: 0,
        }))
    }

    /// Starts the shell; one that cannot be started has its name asked for.
    fn shell(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == phase::RAN {
            if child.is_ok() {
                return Flow::Transfer;
            }
            Self::set_stage(k, phase, stage::ASK);
            return self.ask(k, phase, Ok(0));
        }
        let mut line = [0u8; 128];
        k.g.read_bytes(SEGMENT, BOOT + boot::SHELL, &mut line);
        let len = line.iter().position(|&b| b == 0).unwrap_or(line.len());
        let (path, tail) = split_command(&line[..len]);
        k.set_path(level::PATH, path);
        set_tail(k, tail);
        Self::run(k, phase)
    }

    /// Asks for the shell's name, then starts it.
    fn ask(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::ASKED => {
                let buffer = BOOT + boot::TAIL;
                k.set8(buffer, 120);
                *phase = phase::ANSWERED;
                let device = console(k);
                Flow::Call(Frame::of(&super::char::LineInput {
                    device,
                    buffer: Far::new(SEGMENT, buffer + 2),
                    count: 120,
                    entry: Far::default(),
                    piped: false,
                }))
            }
            phase::ANSWERED => {
                let typed = usize::try_from(child.unwrap_or(0)).unwrap_or(0).saturating_sub(1);
                let mut line = [0u8; 120];
                k.g.read_bytes(SEGMENT, BOOT + boot::TAIL + 2, &mut line);
                let line = trim(&line[..typed.min(line.len())]).to_vec();
                set_text(k, BOOT + boot::SHELL, &line);
                Self::set_stage(k, phase, stage::SHELL);
                self.shell(k, phase, Ok(0))
            }
            _ => Self::say(
                k,
                phase,
                phase::ASKED,
                b"\r\nBad or missing Command Interpreter\r\nEnter correct name of Command Interpreter (eg, C:\\COMMAND.COM)\r\n",
            ),
        }
    }
}

/// Opens the kernel's own handles every program inherits, once every
/// driver is in the chain: `CON` as input, output and error, then `AUX` and
/// `PRN`.
fn standard_handles(k: &mut Kernel<'_, '_>) {
    for (name, handles) in [
        (b"CON        ", 0..3u16),
        (b"AUX        ", 3..4),
        (b"PRN        ", 4..5),
    ] {
        let Some(device) = k.char_device(name) else {
            continue;
        };
        let Some((index, entry)) = k.free_sft() else {
            return;
        };
        k.open_device(entry, device, access::BOTH);
        k.set_sft16(
            entry,
            super::data::sft::HANDLES,
            u16::try_from(handles.len()).unwrap_or(1),
        );
        for handle in handles {
            k.set_handle(handle, index);
        }
    }
}

/// Links the drives' DPBs into the chain the List of Lists heads.
fn link_dpbs(k: &mut Kernel<'_, '_>) {
    let mut previous: Option<u16> = None;
    for drive in 0..DRIVES {
        if k.dpb(drive).is_none() {
            continue;
        }
        let at = dpb_at(drive);
        match previous {
            Some(before) => k.set_far(before + dpb::NEXT, Far::new(SEGMENT, at)),
            None => k.set_far(sysvar(sysvars::FIRST_DPB), Far::new(SEGMENT, at)),
        }
        k.set_far(at + dpb::NEXT, Far::new(0xFFFF, 0xFFFF));
        previous = Some(at);
    }
}

/// Makes the tables the settings size — the further SFT entries, the FCB
/// table, the disk buffers — in blocks of the arena, gives the boot buffers
/// back, and sets the drives `LASTDRIVE` asks for.
fn apply(k: &mut Kernel<'_, '_>) {
    let files = u16::from(k.get8(BOOT + boot::FILES));
    let rule = strategy::FIRST;
    if files > SFT_FIRST {
        let count = files - SFT_FIRST;
        if let Ok(segment) = k.allocate(sft_block(count).div_ceil(16), mcb::DOS, rule) {
            sft_table(k, segment, count);
            k.set_far(SFT, Far::new(segment, 0));
        }
    }
    let fcbs = u16::from(k.get8(BOOT + boot::FCBS));
    if let Ok(segment) = k.allocate(sft_block(fcbs).div_ceil(16), mcb::DOS, rule) {
        sft_table(k, segment, fcbs);
        k.set_far(sysvar(sysvars::FCBS), Far::new(segment, 0));
        let protected = u16::from(k.get8(BOOT + boot::PROTECTED)).min(fcbs);
        k.set16(sysvar(sysvars::PROTECTED_FCBS), protected);
    }
    let buffers = u16::from(k.get8(BOOT + boot::BUFFERS));
    if let Ok(segment) = k.allocate(buffers * BUFFER / 16, mcb::DOS, rule) {
        k.name_block(segment, b"SD");
        k.discard_buffers(FREE);
        use_buffers(k, segment, buffers);
        k.set16(sysvar(sysvars::BUFFERS), buffers);
        let boot_buffers = k.get16(BOOT + boot::BOOT_BUFFERS);
        let _ = k.free(boot_buffers);
    }
    let size = k.get16(BOOT + boot::STACK_SIZE);
    let count = k.get8(BOOT + boot::STACKS);
    stacks::install(k, count, size);
    let lookahead = u16::from(k.get8(BOOT + boot::LOOKAHEAD));
    k.set16(sysvar(sysvars::LOOKAHEAD), lookahead);
    let drives = k
        .get8(BOOT + boot::LASTDRIVE)
        .max(k.get8(BOOT + boot::NEXT_DRIVE))
        .clamp(1, DRIVES);
    k.set8(sysvar(sysvars::LASTDRIVE), drives);
}

/// Makes the block at `segment` an SFT block of `count` empty entries, the
/// last of its chain.
fn sft_table(k: &mut Kernel<'_, '_>, segment: u16, count: u16) {
    k.name_block(segment, b"SD");
    k.g.write16(segment, 0, 0xFFFF);
    k.g.write16(segment, 2, 0xFFFF);
    k.g.write16(segment, 4, count);
    let empty = [0u8; SFT_ENTRY as usize];
    for n in 0..count {
        k.g.write_bytes(segment, SFT_HEADER + n * SFT_ENTRY, &empty);
    }
}

/// Sets the ASCIZ text at `at` in the kernel's segment to `text`, cut to
/// 127 bytes.
fn set_text(k: &mut Kernel<'_, '_>, at: u16, text: &[u8]) {
    let len = text.len().min(127);
    k.g.write_bytes(SEGMENT, at, &text[..len]);
    k.set8(at + u16::try_from(len).unwrap_or(0), 0);
}

/// Sets the command tail the boot gives a program: its length, the text,
/// and a CR.
fn set_tail(k: &mut Kernel<'_, '_>, tail: &[u8]) {
    let len = tail.len().min(126);
    let at = BOOT + boot::TAIL;
    k.set8(at, u8::try_from(len).unwrap_or(0));
    k.g.write_bytes(SEGMENT, at + 1, &tail[..len]);
    k.set8(at + 1 + u16::try_from(len).unwrap_or(0), b'\r');
}

/// Ends `text` with the `CONFIG.SYS` line it is about.
fn name_line(text: &mut Vec<u8>, line: u16) {
    text.extend_from_slice(b"Error in CONFIG.SYS line ");
    text.extend(super::message::decimal(u32::from(line)));
    text.extend_from_slice(super::message::NEW_LINE);
}

/// The eight-byte name a driver's block carries: its file name's base.
fn program_name(path: &[u8]) -> [u8; 8] {
    let file = path
        .rsplit(|&b| b == b'\\' || b == b':')
        .next()
        .unwrap_or(path);
    let base = file.split(|&b| b == b'.').next().unwrap_or(file);
    let mut name = [0u8; 8];
    for (slot, &byte) in name.iter_mut().zip(base) {
        *slot = byte;
    }
    name
}

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;
