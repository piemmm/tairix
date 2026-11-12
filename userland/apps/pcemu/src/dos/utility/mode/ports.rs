//! `MODE`'s printers and serial ports: a printer's columns, lines and
//! retry, its output rerouted, and a serial port's rate, framing and retry.

use super::{
    at, fail, found, name, port, requested, reroute_line, retry, retry_said, table, tables, text,
};
use super::{Device, Framing, ModeResident, Request};
use crate::bios::bda;
use crate::dos::char::ESC;
use crate::dos::program::{say, Flow, Frame, Id, Int, Program, Routine};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

/// The status bits that, after a byte is printed, say the printer did not
/// take it: not busy, out of paper, an I/O error, or a time-out.
const NOT_TAKEN: u8 = 0xA9;
const DC2: u8 = 0x12;
const SI: u8 = 0x0F;

/// Printer `lpt` sent back to its own port: the resident part's rerouting
/// of it ended, and a port word `MODE` set for a rerouted printer the BIOS
/// had not found cleared again.
fn unroute(p: &mut Program<'_, '_>, lpt: u8) {
    if let Some(segment) = tables(p) {
        p.g.write8(segment, at::REROUTE + u16::from(lpt), 0);
    }
    let word = bda::LPT_PORTS + u16::from(lpt) * 2;
    if p.g.read16(bda::SEGMENT, word) == 1 {
        p.g.write16(bda::SEGMENT, word, 0);
    }
}

/// The bytes that set a printer to `columns` and `lines` an inch.
fn control(columns: Option<u8>, lines: Option<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    match columns {
        Some(80) => out.push(DC2),
        Some(132) => out.push(SI),
        _ => {}
    }
    match lines {
        Some(6) => out.extend_from_slice(&[ESC, b'2']),
        Some(8) => out.extend_from_slice(&[ESC, b'0']),
        _ => {}
    }
    out
}

locals! {
    /// A printer set: sent back to its own port, its columns and lines set
    /// by the bytes it takes for them, then its retry. `MODE LPTn` alone
    /// does the first and last of these, with no retry.
    pub struct ModePrinter {
        sent: u8,
        error: bool,
    }
}

mod printer {
    pub const UNROUTED: u8 = 1;
    pub const SENDING: u8 = 2;
    pub const YELLED: u8 = 3;
    pub const SET: u8 = 4;
    pub const RESIDENT: u8 = 5;
    pub const DONE: u8 = 6;
}

/// A printer request's parts: the printer, its columns and lines, and its
/// retry; and whether it only sends the printer back to its port.
struct Asked {
    lpt: u8,
    columns: Option<u8>,
    lines: Option<u8>,
    retry: Option<u8>,
    only_unroute: bool,
}

impl ModePrinter {
    fn asked(p: &mut Program<'_, '_>) -> Option<Asked> {
        match requested(p)? {
            Request::Printer {
                lpt,
                columns,
                lines,
                retry,
            } => Some(Asked {
                lpt,
                columns,
                lines,
                retry,
                only_unroute: false,
            }),
            Request::Reroute { lpt, com: None } => Some(Asked {
                lpt,
                columns: None,
                lines: None,
                retry: Some(retry::NONE),
                only_unroute: true,
            }),
            _ => None,
        }
    }

    /// The next of the control bytes, or, all sent, what was set said.
    fn send(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, asked: &Asked) -> Flow {
        let bytes = control(asked.columns, asked.lines);
        if let Some(&byte) = bytes.get(usize::from(self.sent)) {
            self.sent += 1;
            *phase = printer::SENDING;
            return Flow::Call(Frame::of(&Int {
                dx: u16::from(asked.lpt),
                ..Int::bios(p.own, 0x17, u16::from(byte))
            }));
        }
        let mut said = Vec::new();
        if !self.error {
            let printer = name(Device::Printer(asked.lpt));
            match asked.columns {
                Some(80) => said.extend([&b"\r\n"[..], &printer, b": set for 80\r\n"].concat()),
                Some(132) => said.extend([&b"\r\n"[..], &printer, b": set for 132\r\n"].concat()),
                _ => {}
            }
            if asked.lines.is_some() {
                said.extend_from_slice(b"\r\nPrinter lines per inch set\r\n");
            }
        }
        *phase = printer::SET;
        if said.is_empty() {
            return self.step(p, phase, Ok(0));
        }
        Flow::Call(say(p, &said))
    }

    /// The printer's retry: one asked for makes the resident part; none
    /// asked for by position turns it off; none by keyword keeps it.
    fn retry(p: &mut Program<'_, '_>, phase: &mut u8, asked: &Asked) -> Flow {
        let on = match asked.retry {
            Some(retry::NONE) => {
                if let Some(segment) = tables(p) {
                    p.g.write8(segment, at::LPT_RETRY + u16::from(asked.lpt), retry::NONE);
                }
                false
            }
            Some(_) => {
                *phase = printer::RESIDENT;
                return Flow::Call(Frame::of(&ModeResident {}));
            }
            None => table(p, at::LPT_RETRY + u16::from(asked.lpt)) != retry::NONE,
        };
        *phase = printer::DONE;
        Flow::Call(say(p, retry_said(on)))
    }
}

impl Routine for ModePrinter {
    const ID: Id = Id::ModePrinter;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(asked) = Self::asked(p) else {
            return Flow::Return(Ok(0));
        };
        match *phase {
            0 => {
                unroute(p, asked.lpt);
                *phase = printer::UNROUTED;
                let text = [&b"\r\n"[..], &reroute_line(asked.lpt, None)].concat();
                Flow::Call(say(p, &text))
            }
            printer::UNROUTED => {
                take!(child);
                if asked.only_unroute {
                    if found(p, bda::LPT_PORTS, 3) <= asked.lpt {
                        return Flow::Return(Ok(0));
                    }
                    return Self::retry(p, phase, &asked);
                }
                if port(p, Device::Printer(asked.lpt)) == 0 {
                    fail(p);
                    *phase = printer::DONE;
                    return Flow::Call(say(p, text::ILLEGAL_DEVICE));
                }
                self.send(p, phase, &asked)
            }
            // A failure is said once, however many bytes it refuses.
            printer::SENDING => {
                let status = take!(child).to_le_bytes()[1];
                if status & NOT_TAKEN != 0 && !self.error {
                    self.error = true;
                    *phase = printer::YELLED;
                    return Flow::Call(say(p, text::PRINTER_ERROR));
                }
                self.send(p, phase, &asked)
            }
            printer::YELLED => {
                take!(child);
                self.send(p, phase, &asked)
            }
            printer::SET => {
                take!(child);
                Self::retry(p, phase, &asked)
            }
            printer::RESIDENT => {
                let segment = take!(child).to_le_bytes();
                let segment = u16::from_le_bytes([segment[0], segment[1]]);
                let kept = retry::kept(asked.retry.unwrap_or(retry::NONE));
                p.g.write8(segment, at::LPT_RETRY + u16::from(asked.lpt), kept);
                *phase = printer::DONE;
                Flow::Call(say(p, retry_said(true)))
            }
            _ => {
                take!(child);
                Flow::Return(Ok(0))
            }
        }
    }
}

locals! {
    /// A printer's output rerouted to a serial port the BIOS found, through
    /// the resident part. A printer the BIOS did not find is given a port
    /// word of `1`, so that BASIC opens it.
    pub struct ModeReroute {}
}

mod reroute {
    pub const RESIDENT: u8 = 1;
    pub const DONE: u8 = 2;
}

impl Routine for ModeReroute {
    const ID: Id = Id::ModeReroute;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(Request::Reroute {
            lpt,
            com: Some(com),
        }) = requested(p)
        else {
            return Flow::Return(Ok(0));
        };
        match *phase {
            0 => {
                if port(p, Device::Serial(com)) == 0 {
                    fail(p);
                    *phase = reroute::DONE;
                    return Flow::Call(say(p, text::ILLEGAL_DEVICE));
                }
                if port(p, Device::Printer(lpt)) == 0 {
                    p.g.write16(bda::SEGMENT, bda::LPT_PORTS + u16::from(lpt) * 2, 1);
                }
                *phase = reroute::RESIDENT;
                Flow::Call(Frame::of(&ModeResident {}))
            }
            reroute::RESIDENT => {
                let segment = take!(child).to_le_bytes();
                let segment = u16::from_le_bytes([segment[0], segment[1]]);
                p.g.write8(segment, at::REROUTE + u16::from(lpt), com + 1);
                *phase = reroute::DONE;
                let text = [&b"\r\n"[..], &reroute_line(lpt, Some(com))].concat();
                Flow::Call(say(p, &text))
            }
            _ => {
                take!(child);
                Flow::Return(Ok(0))
            }
        }
    }
}

/// The INT 14h initialisation byte for `framing`: the rate's index, the
/// parity, the stop bits and the data bits.
fn initialisation(framing: Framing) -> u8 {
    const RATES: [u32; 8] = [110, 150, 300, 600, 1200, 2400, 4800, 9600];
    let rate = RATES
        .iter()
        .position(|&r| r == framing.baud)
        .and_then(|n| u8::try_from(n).ok())
        .unwrap_or(7);
    let parity = match framing.parity {
        b'o' => 0x08,
        b'e' => 0x18,
        _ => 0x00,
    };
    let stop = if framing.stop > 10 { 0x04 } else { 0x00 };
    let data = framing.data.saturating_sub(5) & 0x03;
    rate << 5 | parity | stop | data
}

/// The line telling what a serial port was set to.
fn serial_said(com: u8, framing: Framing, shown: u8) -> Vec<u8> {
    let stop: &[u8] = if framing.stop > 10 { b"2" } else { b"1" };
    [
        &b"\r\n"[..],
        &name(Device::Serial(com)),
        b": ",
        &crate::dos::message::decimal(framing.baud),
        b",",
        &[framing.parity],
        b",",
        &[b'0' + framing.data],
        b",",
        stop,
        b",",
        &[shown],
        b"\r\n",
    ]
    .concat()
}

/// A retry as `MODE` shows it after a port's settings: its letter, or `-`
/// for none.
fn shown(letter: u8) -> u8 {
    if letter == retry::NONE {
        b'-'
    } else {
        letter.to_ascii_lowercase()
    }
}

locals! {
    /// A serial port set: its rate and framing through INT 14h, then its
    /// retry as a printer's is.
    pub struct ModeSerial {}
}

mod serial {
    pub const SET: u8 = 1;
    pub const RESIDENT: u8 = 2;
    pub const DONE: u8 = 3;
}

impl Routine for ModeSerial {
    const ID: Id = Id::ModeSerial;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(Request::Serial {
            com,
            framing,
            retry,
        }) = requested(p)
        else {
            return Flow::Return(Ok(0));
        };
        match *phase {
            0 => {
                if port(p, Device::Serial(com)) == 0 {
                    fail(p);
                    *phase = serial::DONE;
                    return Flow::Call(say(p, text::ILLEGAL_DEVICE));
                }
                *phase = serial::SET;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(com),
                    ..Int::bios(p.own, 0x14, u16::from(initialisation(framing)))
                }))
            }
            serial::SET => {
                take!(child);
                let letter = match retry {
                    Some(retry::NONE) => {
                        if let Some(segment) = tables(p) {
                            p.g.write8(segment, at::COM_RETRY + u16::from(com), retry::NONE);
                        }
                        retry::NONE
                    }
                    Some(_) => {
                        *phase = serial::RESIDENT;
                        return Flow::Call(Frame::of(&ModeResident {}));
                    }
                    None => table(p, at::COM_RETRY + u16::from(com)),
                };
                *phase = serial::DONE;
                let text = serial_said(com, framing, shown(letter));
                Flow::Call(say(p, &text))
            }
            serial::RESIDENT => {
                let segment = take!(child).to_le_bytes();
                let segment = u16::from_le_bytes([segment[0], segment[1]]);
                let letter = retry.unwrap_or(retry::NONE);
                p.g.write8(segment, at::COM_RETRY + u16::from(com), retry::kept(letter));
                *phase = serial::DONE;
                let text = serial_said(com, framing, shown(letter));
                Flow::Call(say(p, &text))
            }
            _ => {
                take!(child);
                Flow::Return(Ok(0))
            }
        }
    }
}
