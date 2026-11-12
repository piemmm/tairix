//! INT 14h and 17h: the serial and parallel ports' services, as the PC BIOS
//! gives them. A wait polls the port with interrupts enabled until the
//! timeout the data area keeps for it, counted in units of about half a
//! second: 65,536 of the PC BIOS's status reads at 4.77 MHz.

use super::guest::{Guest, Poll};
use super::{bda, printer_status, serial_status};
use crate::asm::{Reg16, Reg8};
use crate::ports::parallel::{control, reg as lpt, status};
use crate::ports::serial::{lsr, mcr, msr, reg as com, DLAB};
use crate::time::Nanos;

/// INT 17h's state on its stub's stack: when its wait ends.
pub const PRINTER_STATE: u8 = 8;
/// INT 14h's: the step reached, then when its wait ends.
pub const SERIAL_STATE: u8 = 9;

const TIMEOUT_UNIT: Nanos = 500_000_000;
/// How long INT 17h holds Init low: the PC BIOS's delay loop at 4.77 MHz.
const INIT_PULSE: Nanos = 3_800_000;
/// INT 14h's eight rates, 110 to 9600 bits a second, as divisors.
const DIVISORS: [u16; 8] = [
    0x0417, 0x0300, 0x0180, 0x00C0, 0x0060, 0x0030, 0x0018, 0x000C,
];

/// INT 14h's waits: a send waits for DSR and CTS, then for the holding
/// register; a receive for DSR, then for a character.
mod step {
    pub const SEND_READY: u8 = 0;
    pub const SEND_EMPTY: u8 = 1;
    pub const RECEIVE_READY: u8 = 2;
    pub const RECEIVE_DATA: u8 = 3;
}

/// A timeout of `units` from the data area; 0 counts as 256, as the PC
/// BIOS's loop counts it.
fn timeout(units: u8) -> Nanos {
    let units = if units == 0 { 256 } else { u64::from(units) };
    units * TIMEOUT_UNIT
}

fn deadline(g: &mut Guest<'_>, at: u16) -> Nanos {
    let mut bytes = [0; 8];
    for (n, byte) in (at..).zip(&mut bytes) {
        *byte = g.local(n);
    }
    Nanos::from_le_bytes(bytes)
}

fn set_deadline(g: &mut Guest<'_>, at: u16, deadline: Nanos) {
    for (n, byte) in (at..).zip(deadline.to_le_bytes()) {
        g.set_local(n, byte);
    }
}

/// The port DX names among the `slots` the data area lists at `table`,
/// with its number, if the POST found one there.
fn listed(g: &mut Guest<'_>, table: u16, slots: u16) -> Option<(u16, u16)> {
    let n = g.r16(Reg16::Dx);
    if n >= slots {
        return None;
    }
    let base = g.bda16(table + 2 * n);
    (base != 0).then_some((n, base))
}

/// INT 17h: prints AL, initialises the printer or reports its status, on
/// the printer DX names; one the POST did not find changes nothing.
pub fn printer(g: &mut Guest<'_>) {
    let Some((n, base)) = listed(g, bda::LPT_PORTS, 3) else {
        g.done(true);
        return;
    };
    match g.r8(Reg8::Ah) {
        0x00 => {
            let byte = g.r8(Reg8::Al);
            g.output(base.wrapping_add(lpt::DATA), byte);
            let units = g.bda8(bda::LPT_TIMEOUT + n);
            let deadline = g.now() + timeout(units);
            set_deadline(g, 0, deadline);
            print_when_ready(g, base);
        }
        0x01 => {
            g.output(base.wrapping_add(lpt::CONTROL), control::SELECT_IN);
            g.spend(INIT_PULSE);
            g.output(
                base.wrapping_add(lpt::CONTROL),
                control::INIT | control::SELECT_IN,
            );
            report_printer(g, base);
        }
        0x02 => report_printer(g, base),
        _ => g.done(true),
    }
}

/// After an interrupt in INT 17h's wait.
pub fn printer_step(g: &mut Guest<'_>) {
    match listed(g, bda::LPT_PORTS, 3) {
        Some((_, base)) => print_when_ready(g, base),
        None => g.done(true),
    }
}

/// Strobes the byte in once the printer is no longer busy.
fn print_when_ready(g: &mut Guest<'_>, base: u16) {
    let deadline = deadline(g, 0);
    let mut raw = 0;
    let ready = g.poll(deadline, |g| {
        raw = g.input(base.wrapping_add(lpt::STATUS));
        raw & status::NOT_BUSY != 0
    });
    match ready {
        Poll::Ready => {
            let selected = control::INIT | control::SELECT_IN;
            g.output(base.wrapping_add(lpt::CONTROL), selected | control::STROBE);
            g.output(base.wrapping_add(lpt::CONTROL), selected);
            report_printer(g, base);
        }
        Poll::Expired => {
            g.set_r8(
                Reg8::Ah,
                ((raw & !status::UNUSED) | printer_status::TIMED_OUT) ^ printer_status::INVERTED,
            );
            g.done(true);
        }
        Poll::Interrupted => g.done(false),
        Poll::Yielded => g.resume(),
    }
}

fn report_printer(g: &mut Guest<'_>, base: u16) {
    let raw = g.input(base.wrapping_add(lpt::STATUS));
    g.set_r8(Reg8::Ah, (raw & !status::UNUSED) ^ printer_status::INVERTED);
    g.done(true);
}

/// INT 14h: sets a port's rate and framing, sends AL, receives into AL,
/// or reports its status, on the port DX names; one the POST did not find
/// changes nothing.
pub fn serial(g: &mut Guest<'_>) {
    let Some((n, base)) = listed(g, bda::COM_PORTS, 4) else {
        g.done(true);
        return;
    };
    match g.r8(Reg8::Ah) {
        0x00 => {
            let parameters = g.r8(Reg8::Al);
            let [low, high] = DIVISORS[usize::from(parameters >> 5)].to_le_bytes();
            g.output(base.wrapping_add(com::LCR), DLAB);
            g.output(base.wrapping_add(com::DATA), low);
            g.output(base.wrapping_add(com::IER), high);
            g.output(base.wrapping_add(com::LCR), parameters & 0x1F);
            g.output(base.wrapping_add(com::IER), 0);
            report_serial(g, base);
        }
        0x01 => {
            g.output(base.wrapping_add(com::MCR), mcr::DTR | mcr::RTS);
            begin_wait(g, n, step::SEND_READY);
            serial_wait(g, n, base);
        }
        0x02 => {
            g.output(base.wrapping_add(com::MCR), mcr::DTR);
            begin_wait(g, n, step::RECEIVE_READY);
            serial_wait(g, n, base);
        }
        0x03 => report_serial(g, base),
        _ => g.done(true),
    }
}

/// After an interrupt in one of INT 14h's waits.
pub fn serial_step(g: &mut Guest<'_>) {
    match listed(g, bda::COM_PORTS, 4) {
        Some((n, base)) => serial_wait(g, n, base),
        None => g.done(true),
    }
}

/// Starts `at`, its timeout counted afresh as each of the PC BIOS's waits
/// counts its own.
fn begin_wait(g: &mut Guest<'_>, n: u16, at: u8) {
    g.set_local(0, at);
    let units = g.bda8(bda::COM_TIMEOUT + n);
    let deadline = g.now() + timeout(units);
    set_deadline(g, 1, deadline);
}

fn serial_wait(g: &mut Guest<'_>, n: u16, base: u16) {
    loop {
        let at = g.local(0);
        let (register, wanted) = match at {
            step::SEND_READY => (com::MSR, msr::DSR | msr::CTS),
            step::SEND_EMPTY => (com::LSR, lsr::THR_EMPTY),
            step::RECEIVE_READY => (com::MSR, msr::DSR),
            step::RECEIVE_DATA => (com::LSR, lsr::DATA_READY),
            // The caller wrote over the stub's state: nothing is waited for.
            _ => break,
        };
        let deadline = deadline(g, 1);
        let mut status = 0;
        let ready = g.poll(deadline, |g| {
            status = g.input(base.wrapping_add(register));
            status & wanted == wanted
        });
        match (ready, at) {
            (Poll::Interrupted, _) => {
                g.done(false);
                return;
            }
            (Poll::Yielded, _) => {
                g.resume();
                return;
            }
            (Poll::Expired, _) => {
                g.set_r8(Reg8::Ah, status | serial_status::TIMED_OUT);
                break;
            }
            (Poll::Ready, step::SEND_READY | step::RECEIVE_READY) => begin_wait(g, n, at + 1),
            (Poll::Ready, step::SEND_EMPTY) => {
                let byte = g.r8(Reg8::Al);
                g.output(base.wrapping_add(com::DATA), byte);
                g.set_r8(Reg8::Ah, status);
                break;
            }
            (Poll::Ready, _) => {
                let byte = g.input(base.wrapping_add(com::DATA));
                g.set_r8(Reg8::Al, byte);
                g.set_r8(Reg8::Ah, status & lsr::ERRORS);
                break;
            }
        }
    }
    g.done(true);
}

fn report_serial(g: &mut Guest<'_>, base: u16) {
    let line = g.input(base.wrapping_add(com::LSR));
    let modem = g.input(base.wrapping_add(com::MSR));
    g.set_r8(Reg8::Ah, line);
    g.set_r8(Reg8::Al, modem);
    g.done(true);
}

#[cfg(test)]
#[path = "ports_tests.rs"]
mod tests;
