//! `CON`: keys through INT 16h, characters through the BIOS teletype. An
//! extended key reads as a zero byte, then its scan code, which the driver
//! holds for the next read. Ctrl+Break, which the BIOS hands over as a zero
//! word, reads as Ctrl+C.

use super::{local, unknown, Driver};
use crate::asm::{Reg16, Reg8};
use crate::cpu::flag;
use crate::dos::char::CTRL_C;
use crate::dos::data::{driver, SEGMENT};
use crate::dos::device::{command, field};

const CALL: u8 = 1;
const CONSUME: u8 = 2;

/// A request to `CON`.
pub fn serve(d: &mut Driver<'_, '_>) {
    match d.get8(field::COMMAND) {
        command::INIT
        | command::OPEN
        | command::CLOSE
        | command::OUTPUT_STATUS
        | command::OUTPUT_FLUSH => d.finish(None),
        command::INPUT => input(d),
        command::PEEK => peek(d, true),
        command::INPUT_STATUS => peek(d, false),
        command::INPUT_FLUSH => flush(d),
        command::OUTPUT | command::OUTPUT_VERIFY => output(d),
        _ => unknown(d),
    }
}

/// The key read ahead and held, if one is.
fn held(d: &mut Driver<'_, '_>) -> Option<u8> {
    (d.g.read8(SEGMENT, driver::CON_AHEAD + 1) != 0).then(|| d.g.read8(SEGMENT, driver::CON_AHEAD))
}

fn hold(d: &mut Driver<'_, '_>, code: Option<u8>) {
    d.g.write8(SEGMENT, driver::CON_AHEAD, code.unwrap_or(0));
    d.g.write8(SEGMENT, driver::CON_AHEAD + 1, u8::from(code.is_some()));
}

/// The byte a key INT 16h handed over reads as, and the scan code an
/// extended key leaves held.
fn byte_of(key: u16) -> (u8, Option<u8>) {
    let [code, scan] = key.to_le_bytes();
    match (code, key) {
        (_, 0) => (CTRL_C, None),
        (0, _) => (0, Some(scan)),
        _ => (code, None),
    }
}

fn input(d: &mut Driver<'_, '_>) {
    let count = d.get16(field::COUNT);
    let buffer = d.get_far(field::ADDRESS);
    if d.phase() == CALL {
        let (byte, ahead) = byte_of(d.g.r16(Reg16::Ax));
        let done = d.local16(local::DONE);
        let at = buffer.add(done);
        d.g.write8(at.segment, at.offset, byte);
        d.set_local16(local::DONE, done + 1);
        hold(d, ahead);
    }
    loop {
        let done = d.local16(local::DONE);
        if done >= count {
            d.finish(None);
            return;
        }
        if let Some(code) = held(d) {
            let at = buffer.add(done);
            d.g.write8(at.segment, at.offset, code);
            d.set_local16(local::DONE, done + 1);
            hold(d, None);
            continue;
        }
        d.g.set_r8(Reg8::Ah, 0x00);
        d.set_phase(CALL);
        d.bios(0x16);
        return;
    }
}

/// A non-destructive read when `peek`, else an input status: busy while no
/// key waits.
fn peek(d: &mut Driver<'_, '_>, peek: bool) {
    if d.phase() == CALL {
        if d.g.regs().flag(flag::ZF) {
            d.finish_busy(true);
            return;
        }
        let (byte, _) = byte_of(d.g.r16(Reg16::Ax));
        if peek {
            d.set8(field::MEDIA, byte);
        }
        d.finish_busy(false);
        return;
    }
    if let Some(code) = held(d) {
        if peek {
            d.set8(field::MEDIA, code);
        }
        d.finish_busy(false);
        return;
    }
    d.g.set_r8(Reg8::Ah, 0x01);
    d.set_phase(CALL);
    d.bios(0x16);
}

/// Drops every key waiting, the held one included.
fn flush(d: &mut Driver<'_, '_>) {
    match d.phase() {
        CALL if !d.g.regs().flag(flag::ZF) => {
            d.g.set_r8(Reg8::Ah, 0x00);
            d.set_phase(CONSUME);
            d.bios(0x16);
        }
        CALL => d.finish(None),
        _ => {
            hold(d, None);
            d.g.set_r8(Reg8::Ah, 0x01);
            d.set_phase(CALL);
            d.bios(0x16);
        }
    }
}

fn output(d: &mut Driver<'_, '_>) {
    let count = d.get16(field::COUNT);
    let buffer = d.get_far(field::ADDRESS);
    let mut done = d.local16(local::DONE);
    if d.phase() == CALL {
        done += 1;
        d.set_local16(local::DONE, done);
    }
    if done >= count {
        d.finish(None);
        return;
    }
    let at = buffer.add(done);
    let byte = d.g.read8(at.segment, at.offset);
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([byte, 0x0E]));
    d.g.set_r16(Reg16::Bx, 0x0007);
    d.set_phase(CALL);
    d.bios(0x10);
}
