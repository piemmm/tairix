//! `AUX` and `COM1`–`COM4` through INT 14h, `PRN` and `LPT1`–`LPT3` through
//! INT 17h, a character a call.

use super::{local, unknown, Driver};
use crate::asm::{Reg16, Reg8};
use crate::bios::{printer_status, serial_status};
use crate::dos::device::{command, error, field};
use crate::ports::serial::lsr;

const CALL: u8 = 1;
const INITIALISED: u8 = 2;
/// What DOS sets each serial port to as it starts, as INT 14h's AH=00h
/// takes it: 2400 bits a second, no parity, one stop bit, eight data bits.
const RSINIT: u8 = 0xA3;

/// A serial port's request: `port` is INT 14h's number for it.
pub fn serve_com(d: &mut Driver<'_, '_>, port: u8) {
    match d.get8(field::COMMAND) {
        command::INIT if d.phase() == INITIALISED => d.finish(None),
        command::INIT => {
            d.g.set_r16(Reg16::Ax, u16::from(RSINIT));
            d.g.set_r16(Reg16::Dx, u16::from(port));
            d.set_phase(INITIALISED);
            d.bios(0x14);
        }
        command::OPEN | command::CLOSE | command::INPUT_FLUSH | command::OUTPUT_FLUSH => {
            d.finish(None);
        }
        command::INPUT => transfer(d, port, 0x14, 0x02),
        command::OUTPUT | command::OUTPUT_VERIFY => transfer(d, port, 0x14, 0x01),
        command::INPUT_STATUS => line_status(d, port, lsr::DATA_READY),
        command::OUTPUT_STATUS => line_status(d, port, lsr::THR_EMPTY),
        command::PEEK => d.finish_busy(true),
        _ => unknown(d),
    }
}

/// A parallel port's request: `port` is INT 17h's number for it.
pub fn serve_lpt(d: &mut Driver<'_, '_>, port: u8) {
    match d.get8(field::COMMAND) {
        command::INIT | command::OPEN | command::CLOSE | command::OUTPUT_FLUSH => d.finish(None),
        command::INPUT => {
            d.set16(field::COUNT, 0);
            d.finish(None);
        }
        command::OUTPUT | command::OUTPUT_VERIFY | command::OUTPUT_UNTIL_BUSY => {
            transfer(d, port, 0x17, 0x00);
        }
        command::OUTPUT_STATUS => printer_status(d, port),
        _ => unknown(d),
    }
}

/// Moves the request's bytes a BIOS call each: INT `vector` function
/// `function`, the port in DX.
fn transfer(d: &mut Driver<'_, '_>, port: u8, vector: u8, function: u8) {
    let count = d.get16(field::COUNT);
    let buffer = d.get_far(field::ADDRESS);
    if d.phase() == CALL {
        let done = d.local16(local::DONE);
        let ah = d.g.r8(Reg8::Ah);
        let failed = if vector == 0x14 {
            ah & serial_status::TIMED_OUT != 0
        } else {
            ah & (printer_status::TIMED_OUT | printer_status::ERROR) != 0
                || ah & printer_status::PAPER_OUT != 0
        };
        if failed {
            d.set16(field::COUNT, done);
            let code = if vector == 0x14 {
                if function == 0x02 {
                    error::READ_FAULT
                } else {
                    error::WRITE_FAULT
                }
            } else if ah & printer_status::PAPER_OUT != 0 {
                error::PAPER
            } else if ah & printer_status::TIMED_OUT != 0 {
                error::NOT_READY
            } else {
                error::WRITE_FAULT
            };
            d.finish(Some(code));
            return;
        }
        if function == 0x02 {
            let at = buffer.add(done);
            let byte = d.g.r8(Reg8::Al);
            d.g.write8(at.segment, at.offset, byte);
        }
        d.set_local16(local::DONE, done + 1);
    }
    let done = d.local16(local::DONE);
    if done >= count {
        d.finish(None);
        return;
    }
    let byte = if function == 0x02 {
        0
    } else {
        let at = buffer.add(done);
        d.g.read8(at.segment, at.offset)
    };
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([byte, function]));
    d.g.set_r16(Reg16::Dx, u16::from(port));
    d.set_phase(CALL);
    d.bios(vector);
}

/// Answers a status request from INT 14h's line status: busy until `bit`.
fn line_status(d: &mut Driver<'_, '_>, port: u8, bit: u8) {
    if d.phase() == CALL {
        let line = d.g.r8(Reg8::Ah);
        d.finish_busy(line & bit == 0 || line & serial_status::TIMED_OUT != 0);
        return;
    }
    d.g.set_r16(Reg16::Ax, 0x0300);
    d.g.set_r16(Reg16::Dx, u16::from(port));
    d.set_phase(CALL);
    d.bios(0x14);
}

/// Answers an output status request from INT 17h's status: busy unless the
/// printer is selected and ready.
fn printer_status(d: &mut Driver<'_, '_>, port: u8) {
    if d.phase() == CALL {
        let state = d.g.r8(Reg8::Ah);
        let ready = state & printer_status::READY != 0 && state & printer_status::SELECTED != 0;
        d.finish_busy(!ready);
        return;
    }
    d.g.set_r16(Reg16::Ax, 0x0200);
    d.g.set_r16(Reg16::Dx, u16::from(port));
    d.set_phase(CALL);
    d.bios(0x17);
}
