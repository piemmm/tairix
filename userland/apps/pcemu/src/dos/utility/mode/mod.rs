//! `MODE`: the display's mode and shift, the console's columns, lines and
//! typing rate, a printer's columns, lines and retry, a printer rerouted to
//! a serial port, a serial port's rate and framing, and each device's
//! status, as DOS 4's `MODE` does them. Retry, rerouting and a shifted
//! screen are kept by a resident part: the first `MODE` to need one stays
//! resident with INT 17h and INT 14h hooked, and leaves the address of its
//! tables at `0000:0530`, where a later `MODE` finds them to change.

pub mod parse;
mod ports;
mod resident;
mod video;

pub use parse::{request, Adjust, Device, Display, Framing, Request};
pub use ports::{ModePrinter, ModeReroute, ModeSerial};
pub use resident::{Printer17, Serial14};
pub use video::{ModeAnsi, ModeDisplay, ModeDisplays, ModeShift, ModeVideo};

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;

use super::number;
use crate::bios::bda;
use crate::dos::device::generic::display;
use crate::dos::memory::{mcb, psp};
use crate::dos::message::decimal;
use crate::dos::program::{self, say, Flow, Frame, Id, Int, Program, Routine};
use crate::frames::{locals, take, Far, Outcome};
use alloc::vec::Vec;

/// Where `MODE`'s state lies in its memory: first the resident part's
/// tables, which stay with the copy that stays resident.
pub mod at {
    use crate::dos::device::generic::display;
    use crate::dos::program::at::STATE;

    /// The handlers the resident part chains to.
    pub const OLD_17: u16 = STATE;
    pub const OLD_14: u16 = STATE + 0x04;
    /// The serial port each printer is rerouted to, from `1`; `0` for none.
    pub const REROUTE: u16 = STATE + 0x08;
    /// Each printer's and serial port's retry, as [`super::retry::kept`]
    /// keeps it.
    pub const LPT_RETRY: u16 = STATE + 0x0B;
    pub const COM_RETRY: u16 = STATE + 0x0E;
    /// The bytes cleared when a copy becomes the resident part.
    pub const TABLES_LEN: u16 = 0x0A;
    /// The video parameter table, the ROM's copied for a shift to change.
    pub const VIDEO_TABLE: u16 = STATE + 0x12;
    pub const VIDEO_TABLE_LEN: u16 = crate::bios::layout::VIDEO_PARAMETERS_LEN;
    /// Whether this copy stays resident, and whether it has said an error.
    pub const STAYS: u16 = VIDEO_TABLE + VIDEO_TABLE_LEN;
    pub const FAILED: u16 = STAYS + 1;
    /// A loop's count: the device a status has reached, the handle closed.
    pub const STEP: u16 = STAYS + 2;
    /// The display information `ANSI.SYS` gives and takes.
    pub const PACKET: u16 = STAYS + 4;
    /// `CON`, opened to reach it.
    pub const NAME: u16 = PACKET + display::LEN;
    /// The BIOS's video state, as INT 10h function `1Bh` gives it.
    pub const VIDEO_STATE: u16 = NAME + 4;
    pub const VIDEO_STATE_LEN: u16 = 0x40;
    /// Bytes of `MODE`'s state.
    pub const LEN: u16 = VIDEO_STATE + VIDEO_STATE_LEN - STATE;

    const _: () = assert!(COM_RETRY + 4 - REROUTE == TABLES_LEN);
    const _: () = assert!(COM_RETRY + 4 <= VIDEO_TABLE);
}

/// What a printer or serial port that timed out answers: its retry as
/// typed, and as the tables keep it.
pub mod retry {
    /// The time-out passed on as the BIOS gave it.
    pub const NONE: u8 = 0;
    /// A status that says the device is in error, busy, or ready.
    pub const ERROR: u8 = b'E';
    pub const BUSY: u8 = b'B';
    pub const READY: u8 = b'R';
    /// The call made again until the device takes it.
    pub const ALWAYS: u8 = b'P';

    /// The retry the tables keep for `letter`. Every retry makes a timed
    /// out call again; a continual one is the busy one, whose status
    /// passes the device's own on.
    #[must_use]
    pub const fn kept(letter: u8) -> u8 {
        if letter == ALWAYS {
            BUSY
        } else {
            letter
        }
    }
}

/// `MODE`'s handlers, by their stub's number.
pub mod handler {
    pub const PRINTER: u16 = 0;
    pub const SERIAL: u16 = 1;
    /// Ctrl+C while a copy becoming the resident part has a vector led to
    /// it: the work abandoned, the main routine staying resident.
    pub const BREAK: u16 = 2;
}

/// `MODE`'s messages, as DOS's are worded.
pub mod text {
    use alloc::vec::Vec;

    pub const ILLEGAL_DEVICE: &[u8] = b"\r\nIllegal device name\r\n";
    pub const MUST_COM: &[u8] = b"\r\nMust specify COM1, COM2, COM3 or COM4\r\n";
    pub const BAD_BAUD: &[u8] = b"\r\nInvalid baud rate specified\r\n";
    pub const NEED_BAUD: &[u8] = b"\r\nBaud rate required\r\n";
    pub const FIELDS: &[u8] = b"\r\nInvalid number of parameters\r\n";
    pub const RATE_DELAY: &[u8] = b"\r\nRATE and DELAY must be specified together\r\n";
    pub const NO_ANSI: &[u8] = b"\r\nANSI.SYS must be installed to perform requested function\r\n";
    pub const NO_FONT: &[u8] = b"\r\nRequired font not loaded\r\n";
    pub const PRINTER_ERROR: &[u8] = b"\r\nPrinter error\r\n";
    pub const RESIDENT: &[u8] = b"\r\nResident portion of MODE loaded\r\n";
    pub const NO_CODE_PAGE: &[u8] = b"Code page operation not supported on this device\r\n";

    /// What `MODE` says of `what`, a setting no machine here can make.
    #[must_use]
    pub fn unsupported(what: &[u8]) -> Vec<u8> {
        [
            &b"\r\nFunction not supported on this computer - "[..],
            &what.to_ascii_uppercase(),
            b"\r\n",
        ]
        .concat()
    }
}

/// `0000:0530`, where `MODE` keeps the address of its resident part's
/// tables.
const POINTER: u16 = 0x530;

/// The segment of the resident part's tables: another copy of `MODE`,
/// staying resident in a block of its own, that `0000:0530` leads to. What
/// a warm start or an ended program left there leads nowhere.
fn resident(p: &mut Program<'_, '_>) -> Option<u16> {
    let (offset, segment) = (p.g.read16(0, POINTER), p.g.read16(0, POINTER + 2));
    let ours = segment != 0
        && segment != p.own
        && offset == at::OLD_17
        && u16::from(p.g.read8(segment, program::at::NUMBER)) == number::MODE
        && p.g.read16(segment.wrapping_sub(1), mcb::OWNER) == segment;
    ours.then_some(segment)
}

/// Where the tables this run changes lie: the resident part's, or this
/// copy's once it is to stay.
fn tables(p: &mut Program<'_, '_>) -> Option<u16> {
    resident(p).or_else(|| (p.get8(at::STAYS) != 0).then_some(p.own))
}

/// A byte of the tables in force; `0`, none rerouted or retried, with no
/// resident part.
fn table(p: &mut Program<'_, '_>, at: u16) -> u8 {
    tables(p).map_or(0, |segment| p.g.read8(segment, at))
}

/// The run has said an error: `MODE` ends with code 1, as DOS's does.
fn fail(p: &mut Program<'_, '_>) {
    p.set8(at::FAILED, 1);
}

/// The request on `MODE`'s command line, read again by each part of it.
fn requested(p: &mut Program<'_, '_>) -> Option<Request> {
    let tail = p.tail();
    request(&tail).ok()
}

/// How many of the data area's `slots` ports at `table` the BIOS found.
fn found(p: &mut Program<'_, '_>, table: u16, slots: u8) -> u8 {
    let mut count = 0;
    for n in 0..u16::from(slots) {
        if p.g.read16(bda::SEGMENT, table + n * 2) != 0 {
            count += 1;
        }
    }
    count
}

/// Printer `lpt`'s or serial port `com`'s word in the data area.
fn port(p: &mut Program<'_, '_>, device: Device) -> u16 {
    match device {
        Device::Printer(lpt) => {
            p.g.read16(bda::SEGMENT, bda::LPT_PORTS + u16::from(lpt) * 2)
        }
        Device::Serial(com) => {
            p.g.read16(bda::SEGMENT, bda::COM_PORTS + u16::from(com) * 2)
        }
        Device::Console => 1,
    }
}

/// A device's name: `CON`, `LPTn` or `COMn`.
fn name(device: Device) -> Vec<u8> {
    match device {
        Device::Console => b"CON".to_vec(),
        Device::Printer(n) => alloc::vec![b'L', b'P', b'T', b'1' + n],
        Device::Serial(n) => alloc::vec![b'C', b'O', b'M', b'1' + n],
    }
}

/// The line saying where printer `lpt` sends its output.
fn reroute_line(lpt: u8, com: Option<u8>) -> Vec<u8> {
    let printer = name(Device::Printer(lpt));
    match com {
        Some(com) => [
            &printer[..],
            b": rerouted to ",
            &name(Device::Serial(com)),
            b":\r\n",
        ]
        .concat(),
        None => [&printer[..], b": not rerouted\r\n"].concat(),
    }
}

/// The line a printer's retry is told by when one is set.
fn retry_said(on: bool) -> &'static [u8] {
    if on {
        b"\r\nInfinite retry on parallel printer time-out\r\n"
    } else {
        b"\r\nNo retry on parallel printer time-out\r\n"
    }
}

/// Makes this copy the resident part: INT 17h and INT 14h led to its
/// handlers, the ones before kept to chain to, and its tables' address left
/// at `0000:0530`. Written straight into the vector table with no call
/// between them and the end, so a Ctrl+C can never end `MODE` with a vector
/// leading into memory it gave back.
fn hook(p: &mut Program<'_, '_>) {
    for (vector, old, n) in [
        (0x17u16, at::OLD_17, handler::PRINTER),
        (0x14, at::OLD_14, handler::SERIAL),
    ] {
        let entry = vector * 4;
        let before = Far::new(p.g.read16(0, entry + 2), p.g.read16(0, entry));
        p.set_far(old, before);
        p.g.write16(0, entry, program::handler_entry(n));
        p.g.write16(0, entry + 2, p.own);
    }
    p.g.write16(0, POINTER, at::OLD_17);
    p.g.write16(0, POINTER + 2, p.own);
}

locals! {
    /// `MODE`'s main routine: the request carried out, then, for a copy
    /// that became the resident part, the standard handles closed and the
    /// vectors hooked as it stays.
    pub struct Mode {}
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const DONE: u8 = 2;
    pub const CLOSING: u8 = 3;
}

impl Mode {
    fn code(p: &mut Program<'_, '_>) -> u8 {
        p.get8(at::FAILED).min(1)
    }
}

impl Routine for Mode {
    const ID: Id = Id::Mode;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                p.set8(at::STAYS, 0);
                p.set8(at::FAILED, 0);
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(problem) => {
                        *phase = phase::REFUSED;
                        let text = problem.text();
                        return Flow::Call(say(p, &text));
                    }
                };
                *phase = phase::DONE;
                Flow::Call(match request {
                    Request::Status(_) => Frame::of(&ModeStatus {}),
                    Request::CodePage => {
                        fail(p);
                        say(p, text::NO_CODE_PAGE)
                    }
                    Request::Display { .. } | Request::Console { .. } => {
                        Frame::of(&ModeVideo::default())
                    }
                    Request::Printer { .. } | Request::Reroute { com: None, .. } => {
                        Frame::of(&ModePrinter::default())
                    }
                    Request::Reroute { .. } => Frame::of(&ModeReroute {}),
                    Request::Serial { .. } => Frame::of(&ModeSerial {}),
                })
            }
            phase::REFUSED => Flow::Return(Ok(1)),
            // A copy whose vectors are to lead to it stays however its work
            // ended, Ctrl+C included: what led to it already must stay
            // valid.
            phase::DONE => {
                if p.get8(at::STAYS) == 0 {
                    return Flow::Return(child.map(|_| u32::from(Self::code(p))));
                }
                if child.is_err() {
                    fail(p);
                }
                p.set16(at::STEP, 0);
                *phase = phase::CLOSING;
                self.step(p, phase, Ok(0))
            }
            _ => {
                let handle = p.get16(at::STEP);
                if handle < 5 {
                    p.set16(at::STEP, handle + 1);
                    return Flow::Call(Frame::of(&Int {
                        bx: handle,
                        ..Int::dos(p.own, 0x3E00)
                    }));
                }
                hook(p);
                let (code, paragraphs) = (Self::code(p), p.paragraphs());
                p.stay_resident(code, paragraphs)
            }
        }
    }
}

locals! {
    /// The resident part found, or this copy made it: its tables cleared,
    /// its environment given back and the message said; its vectors are
    /// hooked as `MODE` ends. Gives the tables' segment.
    pub struct ModeResident {}
}

mod resident_phase {
    pub const FREED: u8 = 1;
    pub const SAID: u8 = 2;
}

impl Routine for ModeResident {
    const ID: Id = Id::ModeResident;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                if let Some(segment) = tables(p) {
                    return Flow::Return(Ok(u32::from(segment)));
                }
                p.g.write_bytes(p.own, at::REROUTE, &[0; at::TABLES_LEN as usize]);
                p.set8(at::STAYS, 1);
                let environment = p.get16(psp::ENVIRONMENT);
                p.set16(psp::ENVIRONMENT, 0);
                *phase = resident_phase::FREED;
                if environment == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    es: environment,
                    ..Int::dos(p.own, 0x4900)
                }))
            }
            // A block DOS would not give back is no reason to keep nothing.
            resident_phase::FREED => {
                *phase = resident_phase::SAID;
                Flow::Call(say(p, text::RESIDENT))
            }
            _ => {
                take!(child);
                Flow::Return(Ok(u32::from(p.own)))
            }
        }
    }
}

/// The devices `MODE` alone shows: the three printers whether or not the
/// BIOS found them, the console, then the serial ports it found.
fn every(p: &mut Program<'_, '_>) -> Vec<Device> {
    let serial = found(p, bda::COM_PORTS, 4);
    (0..3)
        .map(Device::Printer)
        .chain([Device::Console])
        .chain((0..serial).map(Device::Serial))
        .collect()
}

/// A status's heading: the device's name, underlined.
fn heading(device: Device) -> Vec<u8> {
    let under: &[u8] = if device == Device::Console {
        b"----"
    } else {
        b"-----"
    };
    [
        &b"\r\nStatus for device "[..],
        &name(device),
        b":\r\n",
        &[b'-'; 18],
        under,
        b"\r\n",
    ]
    .concat()
}

fn retry_line(kept: u8) -> Vec<u8> {
    let shown: &[u8] = match kept {
        retry::ERROR => b"E",
        retry::BUSY => b"B",
        retry::READY => b"R",
        _ => b"NONE",
    };
    [&b"RETRY="[..], shown, b"\r\n"].concat()
}

/// The console's status: its columns and lines when `ANSI.SYS` told them,
/// then that it has no code pages.
fn console_status(info: Option<[u8; display::LEN as usize]>) -> Vec<u8> {
    let mut out = heading(Device::Console);
    if let Some(info) = info {
        let text = info[usize::from(display::MODE)] == display::TEXT;
        let word = |at: u16| u16::from_le_bytes([info[usize::from(at)], info[usize::from(at) + 1]]);
        let columns: &[u8] = match (text, word(display::COLUMNS)) {
            (false, _) => b"NONE",
            (true, 80) => b"80",
            (true, _) => b"40",
        };
        let lines = if text {
            decimal(u32::from(word(display::ROWS)))
        } else {
            b"NONE".to_vec()
        };
        out.extend([&b"COLUMNS="[..], columns, b"\r\nLINES=", &lines, b"\r\n"].concat());
    }
    out.extend_from_slice(text::NO_CODE_PAGE);
    out
}

locals! {
    /// One device's status, or every one's.
    pub struct ModeStatus {}
}

mod status {
    pub const NEXT: u8 = 1;
    pub const CHECKED: u8 = 2;
    pub const READ: u8 = 3;
}

impl ModeStatus {
    /// A printer's or serial port's status: a printer's rerouting whether or
    /// not the BIOS found it, its retry and code page if it did; a serial
    /// port's retry. `None` for a port the BIOS did not find.
    fn port_status(p: &mut Program<'_, '_>, device: Device) -> Option<Vec<u8>> {
        let mut out = heading(device);
        match device {
            Device::Printer(lpt) => {
                let com = table(p, at::REROUTE + u16::from(lpt));
                out.extend(reroute_line(lpt, com.checked_sub(1)));
                if found(p, bda::LPT_PORTS, 3) > lpt {
                    let kept = table(p, at::LPT_RETRY + u16::from(lpt));
                    out.extend(retry_line(kept));
                    out.extend_from_slice(text::NO_CODE_PAGE);
                    fail(p);
                }
            }
            Device::Serial(com) => {
                if found(p, bda::COM_PORTS, 4) <= com {
                    return None;
                }
                let kept = table(p, at::COM_RETRY + u16::from(com));
                out.extend(retry_line(kept));
            }
            Device::Console => {}
        }
        Some(out)
    }
}

impl Routine for ModeStatus {
    const ID: Id = Id::ModeStatus;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                p.set16(at::STEP, 0);
                *phase = status::NEXT;
                self.step(p, phase, Ok(0))
            }
            status::NEXT => {
                take!(child);
                let devices = match requested(p) {
                    Some(Request::Status(Some(device))) => alloc::vec![device],
                    _ => every(p),
                };
                let at = p.get16(at::STEP);
                let Some(&device) = devices.get(usize::from(at)) else {
                    return Flow::Return(Ok(0));
                };
                p.set16(at::STEP, at + 1);
                if device == Device::Console {
                    *phase = status::CHECKED;
                    return Flow::Call(Frame::of(&Int::bios(p.own, 0x2F, video::ANSI_CHECK)));
                }
                let Some(text) = Self::port_status(p, device) else {
                    fail(p);
                    return Flow::Call(say(p, text::ILLEGAL_DEVICE));
                };
                Flow::Call(say(p, &text))
            }
            status::CHECKED => {
                if video::ansi_answered(child) {
                    *phase = status::READ;
                    return Flow::Call(Frame::of(&ModeAnsi {
                        set: false,
                        ..ModeAnsi::default()
                    }));
                }
                *phase = status::NEXT;
                fail(p);
                let text = console_status(None);
                Flow::Call(say(p, &text))
            }
            _ => {
                *phase = status::NEXT;
                fail(p);
                let info = child.is_ok().then(|| {
                    let mut info = [0u8; display::LEN as usize];
                    p.g.read_bytes(p.own, at::PACKET, &mut info);
                    info
                });
                let text = console_status(info);
                Flow::Call(say(p, &text))
            }
        }
    }
}
