//! The resident part's handlers, as DOS 4's `MODE` leaves them. INT 17h
//! sends a rerouted printer's bytes to its serial port and makes a timed
//! out printer call again; INT 14h makes a timed out serial call again.
//! Every call a handler does not take is chained to the handler before it,
//! with the caller's registers.

use super::{at, retry};
use crate::bios::bda;
use crate::dos::engine::Saved;
use crate::dos::program::{hand_back, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::frames::{locals, Far, Outcome};

/// Bit 7 of the data area's break flag: Ctrl+Break was pressed, which ends a
/// retry.
const BROKEN: u8 = 0x80;

/// A printer status's bits: busy cleared, selected, and a time-out.
mod lpt {
    pub const NOT_BUSY: u8 = 0x80;
    pub const TIMED_OUT: u8 = 0x01;
    /// What a retry of `E` and `R` say of a busy printer: busy, out of
    /// paper, an I/O error and a time-out; or not busy and selected.
    pub const ERROR: u8 = 0x29;
    pub const READY: u8 = 0x90;
    /// What a rerouted printer says of its serial port.
    pub const TAKEN: u8 = 0x90;
    pub const REFUSED: u8 = 0x09;
    pub const OFF: u8 = 0x29;
    pub const BUSY: u8 = 0x10;
}

/// A serial status's: the time-out, and the line and modem statuses a
/// retry of `E` and `R` give.
mod com {
    pub const TIMED_OUT: u8 = 0x80;
    pub const ERROR: u16 = 0x8E00;
    pub const READY: u16 = 0x6030;
    /// Clear to send and data set ready.
    pub const READY_LINES: u8 = 0x30;
}

fn broken(p: &mut Program<'_, '_>) -> bool {
    p.g.read8(bda::SEGMENT, bda::BREAK) & BROKEN != 0
}

/// Clears the break flag before a call, so that one pressed during it ends
/// the retry.
fn arm(p: &mut Program<'_, '_>) {
    let flag = p.g.read8(bda::SEGMENT, bda::BREAK);
    p.g.write8(bda::SEGMENT, bda::BREAK, flag & !BROKEN);
}

/// AX with `ah` given back and the caller's AL kept, as a rerouted
/// printer's status is.
fn status_ax(ah: u8, ax: u16) -> u16 {
    u16::from_le_bytes([ax.to_le_bytes()[0], ah])
}

locals! {
    /// INT 17h: the caller's request kept for a retry to make again.
    pub struct Printer17 {
        ax: u16,
        lpt: u16,
    }
}

mod printer {
    pub const SENT: u8 = 1;
    pub const LOOKED: u8 = 2;
    pub const RETRIED: u8 = 3;
    pub const CHAINED: u8 = 4;
}

impl Printer17 {
    fn old(p: &mut Program<'_, '_>) -> Far {
        p.get_far(at::OLD_17)
    }

    fn kept(p: &mut Program<'_, '_>, lpt: u16) -> u8 {
        p.get8(at::LPT_RETRY + lpt)
    }

    fn call(self, p: &mut Program<'_, '_>, phase: &mut u8, then: u8) -> Flow {
        *phase = then;
        let old = Self::old(p);
        Flow::Call(Frame::of(&Int::passed_on(p, 0x17, old, self.ax)))
    }

    /// A rerouted printer: initialising it is done at once; a byte is sent
    /// to its serial port and its status is the port's, through INT 14h.
    fn rerouted(self, p: &mut Program<'_, '_>, phase: &mut u8, com: u8) -> Flow {
        let [al, ah] = self.ax.to_le_bytes();
        let (function, then) = match ah {
            0x01 => {
                p.set_caller(Saved::Ax, status_ax(lpt::NOT_BUSY, self.ax));
                return Flow::Return(Ok(0));
            }
            0x00 => (0x01, printer::SENT),
            _ => (0x03, printer::LOOKED),
        };
        *phase = then;
        Flow::Call(Frame::of(&Int {
            dx: u16::from(com),
            ..Int::bios(p.own, 0x14, u16::from_le_bytes([al, function]))
        }))
    }
}

impl Routine for Printer17 {
    const ID: Id = Id::Printer17;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                self.ax = p.caller(Saved::Ax);
                self.lpt = p.caller(Saved::Dx);
                if self.lpt >= 3 {
                    return self.call(p, phase, printer::CHAINED);
                }
                let com = p.get8(at::REROUTE + self.lpt);
                if let Some(com) = com.checked_sub(1) {
                    return self.rerouted(p, phase, com);
                }
                if Self::kept(p, self.lpt) == retry::NONE {
                    return self.call(p, phase, printer::CHAINED);
                }
                arm(p);
                self.call(p, phase, printer::RETRIED)
            }
            printer::SENT => {
                let status = child.map_or(0xFF, |ax| ax.to_le_bytes()[1]);
                let ah = if status & com::TIMED_OUT != 0 {
                    lpt::REFUSED
                } else {
                    lpt::TAKEN
                };
                p.set_caller(Saved::Ax, status_ax(ah, self.ax));
                Flow::Return(Ok(0))
            }
            printer::LOOKED => {
                let lines = child.map_or(0, |ax| ax.to_le_bytes()[0]) & com::READY_LINES;
                let ah = match lines {
                    0 => lpt::OFF,
                    com::READY_LINES => lpt::READY,
                    _ => lpt::BUSY,
                };
                p.set_caller(Saved::Ax, status_ax(ah, self.ax));
                Flow::Return(Ok(0))
            }
            // A status call reports a busy printer as its retry says; any
            // other call is made again while it times out and Ctrl+Break is
            // not pressed.
            printer::RETRIED => {
                let status = p.result(Returned::Ax).to_le_bytes()[1];
                if self.ax.to_le_bytes()[1] == 0x02 {
                    hand_back(p);
                    let shown = match Self::kept(p, self.lpt) {
                        retry::ERROR => lpt::ERROR,
                        retry::READY => lpt::READY,
                        _ => status,
                    };
                    if status & lpt::NOT_BUSY == 0 {
                        let ax = p.result(Returned::Ax);
                        p.set_caller(Saved::Ax, status_ax(shown, ax));
                    }
                    return Flow::Return(Ok(0));
                }
                if status & lpt::TIMED_OUT != 0 && !broken(p) {
                    arm(p);
                    return self.call(p, phase, printer::RETRIED);
                }
                hand_back(p);
                Flow::Return(Ok(0))
            }
            _ => {
                hand_back(p);
                Flow::Return(Ok(0))
            }
        }
    }
}

locals! {
    /// INT 14h: the caller's request kept for a retry to make again.
    pub struct Serial14 {
        ax: u16,
        com: u16,
    }
}

mod serial {
    pub const RETRIED: u8 = 1;
    pub const CHAINED: u8 = 2;
}

impl Serial14 {
    fn call(self, p: &mut Program<'_, '_>, phase: &mut u8, then: u8) -> Flow {
        *phase = then;
        let old = p.get_far(at::OLD_14);
        Flow::Call(Frame::of(&Int::passed_on(p, 0x14, old, self.ax)))
    }
}

impl Routine for Serial14 {
    const ID: Id = Id::Serial14;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        match *phase {
            0 => {
                self.ax = p.caller(Saved::Ax);
                self.com = p.caller(Saved::Dx);
                if self.com >= 4 || p.get8(at::COM_RETRY + self.com) == retry::NONE {
                    return self.call(p, phase, serial::CHAINED);
                }
                arm(p);
                self.call(p, phase, serial::RETRIED)
            }
            // A status call reports the port as its retry says; any other
            // call is made again while it times out, and one Ctrl+Break
            // ends is given a time-out.
            serial::RETRIED => {
                hand_back(p);
                let ax = p.result(Returned::Ax);
                if self.ax.to_le_bytes()[1] == 0x03 {
                    match p.get8(at::COM_RETRY + self.com) {
                        retry::ERROR => p.set_caller(Saved::Ax, com::ERROR),
                        retry::READY => p.set_caller(Saved::Ax, com::READY),
                        _ => {}
                    }
                    return Flow::Return(Ok(0));
                }
                if broken(p) {
                    p.set_caller(Saved::Ax, ax | u16::from(com::TIMED_OUT) << 8);
                    return Flow::Return(Ok(0));
                }
                if ax.to_le_bytes()[1] & com::TIMED_OUT != 0 {
                    arm(p);
                    return self.call(p, phase, serial::RETRIED);
                }
                Flow::Return(Ok(0))
            }
            _ => {
                hand_back(p);
                Flow::Return(Ok(0))
            }
        }
    }
}
