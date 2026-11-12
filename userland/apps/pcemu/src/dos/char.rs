//! Characters: transfers through a character device's driver, the standard
//! handles functions `01h`–`0Ch` read and write, the console's cooked
//! output — Ctrl+C breaking, Ctrl+S pausing, Ctrl+P echoing to the printer,
//! tabs to the next eighth column — and DOS's line input with its template.
//! A console waiting for a key calls INT 28h and halts until the next
//! interrupt.

use super::data::{driver, level, sda, sft, sysvars, SEGMENT, SYSVARS};
use super::device::{attribute, command, field, status, Device};
use super::engine::{locals, take, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine};
use super::file::{info, Read, Write};
use super::fs::{critical, Critical};
use super::rom::entry;
use crate::asm::Reg8;
use tairix_abi::time::{CivilTime, DosTime};

/// Ctrl+C.
pub const CTRL_C: u8 = 0x03;
/// Ctrl+P.
const CTRL_P: u8 = 0x10;
/// Ctrl+S.
const CTRL_S: u8 = 0x13;
/// Ctrl+Z.
pub const CTRL_Z: u8 = 0x1A;
/// Ctrl+F, which line input passes over.
const CTRL_F: u8 = 0x06;
/// The keys and bytes that edit a line, and the bell.
pub const BACKSPACE: u8 = 0x08;
pub const TAB: u8 = 0x09;
pub const LF: u8 = 0x0A;
pub const CR: u8 = 0x0D;
pub const ESC: u8 = 0x1B;
pub const DEL: u8 = 0x7F;
pub const BELL: u8 = 0x07;

/// The scan codes the console gives after a zero for the keys with no
/// character of their own.
pub mod scan {
    pub const ESC: u8 = 0x01;
    pub const BACKSPACE: u8 = 0x0E;
    pub const TAB: u8 = 0x0F;
    pub const ENTER: u8 = 0x1C;
    pub const F1: u8 = 0x3B;
    pub const F2: u8 = 0x3C;
    pub const F3: u8 = 0x3D;
    pub const F4: u8 = 0x3E;
    pub const F5: u8 = 0x3F;
    pub const F6: u8 = 0x40;
    pub const F7: u8 = 0x41;
    pub const F8: u8 = 0x42;
    pub const F9: u8 = 0x43;
    pub const F10: u8 = 0x44;
    pub const HOME: u8 = 0x47;
    pub const UP: u8 = 0x48;
    pub const PAGE_UP: u8 = 0x49;
    pub const LEFT: u8 = 0x4B;
    pub const RIGHT: u8 = 0x4D;
    pub const END: u8 = 0x4F;
    pub const DOWN: u8 = 0x50;
    pub const PAGE_DOWN: u8 = 0x51;
    pub const INSERT: u8 = 0x52;
    pub const DELETE: u8 = 0x53;
    pub const ALT_F7: u8 = 0x6E;
    pub const ALT_F10: u8 = 0x71;
    pub const CTRL_LEFT: u8 = 0x73;
    pub const CTRL_RIGHT: u8 = 0x74;
    pub const CTRL_END: u8 = 0x75;
    pub const CTRL_PAGE_DOWN: u8 = 0x76;
    pub const CTRL_HOME: u8 = 0x77;
    pub const CTRL_PAGE_UP: u8 = 0x84;
}

/// Where F2 or F4 and then `key` reach in `template` from `from`: up to, not
/// including, the next `key` past the character at `from`.
#[must_use]
pub fn template_reach(template: &[u8], from: usize, key: u8) -> Option<usize> {
    let found = template
        .get(from..)?
        .iter()
        .skip(1)
        .position(|&byte| byte == key)?;
    Some(from + 1 + found)
}

/// The standard handles.
pub mod handle {
    /// Input.
    pub const INPUT: u16 = 0;
    /// Output.
    pub const OUTPUT: u16 = 1;
    /// The auxiliary device.
    pub const AUX: u16 = 3;
    /// The printer.
    pub const PRINTER: u16 = 4;
}

/// INT 1Bh: Ctrl+Break was pressed. The BIOS has put the zero word the
/// console reads as Ctrl+C into the keyboard buffer; the flag lets a call
/// that is not reading the console see it.
pub fn break_pressed(k: &mut Kernel<'_, '_>) {
    k.set_sda8(sda::BREAK_HIT, 1);
}

/// The active `CON` device's header.
fn console(k: &mut Kernel<'_, '_>) -> Far {
    let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::CON)).unwrap_or(0);
    k.get_far(at)
}

/// The active `CLOCK$` device's header.
fn clock(k: &mut Kernel<'_, '_>) -> Far {
    let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::CLOCK)).unwrap_or(0);
    k.get_far(at)
}

locals! {
    /// Moves `count` bytes between `buffer` and the character device at
    /// `device` with `command`, as one request, or, for a console written
    /// through INT 29h, a byte a call. Returns the bytes moved.
    pub struct CharIo {
        device: Far,
        command: u8,
        buffer: Far,
        count: u16,
        done: u16,
        status: u16,
    }
}

const CALLED: u8 = 1;
const FAST: u8 = 2;
const ANSWERED: u8 = 3;

impl Routine for CharIo {
    const ID: Id = Id::CharIo;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            CALLED => {
                let word = u16::try_from(take!(child)).unwrap_or(0);
                let moved = k.request16(field::COUNT);
                if word & status::ERROR == 0 {
                    return Flow::Return(Ok(u32::from(moved)));
                }
                self.status = word;
                let output = matches!(self.command, command::OUTPUT | command::OUTPUT_VERIFY);
                let writing = if output { critical::WRITING } else { 0 };
                let flags = critical::NOT_DISK
                    | critical::RETRY
                    | critical::IGNORE
                    | critical::FAIL
                    | writing;
                *phase = ANSWERED;
                return Flow::Call(Frame::of(&Critical {
                    flags,
                    drive: 0,
                    code: word & 0xFF,
                    header: self.device,
                    action: 0,
                }));
            }
            ANSWERED => {
                return match u8::try_from(take!(child)) {
                    Ok(critical::ACTION_RETRY) => {
                        *phase = 0;
                        self.step(k, phase, Ok(0))
                    }
                    Ok(critical::ACTION_IGNORE) => Flow::Return(Ok(u32::from(self.count))),
                    Ok(critical::ACTION_FAIL) => {
                        Flow::Return(Err(Fault::Error(super::device::extended(self.status))))
                    }
                    _ => Flow::Return(Err(Fault::Abort)),
                };
            }
            FAST => self.done += 1,
            _ => {
                let fast = k.attributes(self.device) & attribute::FAST != 0;
                if !(fast && self.command == command::OUTPUT) {
                    k.request(0, self.command);
                    k.set_request_far(field::ADDRESS, self.buffer);
                    k.set_request16(field::COUNT, self.count);
                    *phase = CALLED;
                    return Flow::Call(Frame::of(&Device {
                        header: self.device,
                    }));
                }
            }
        }
        if self.done >= self.count {
            return Flow::Return(Ok(u32::from(self.done)));
        }
        let at = self.buffer.add(self.done);
        let byte = k.g.read8(at.segment, at.offset);
        k.call_int(0x29);
        k.g.set_r8(Reg8::Al, byte);
        *phase = FAST;
        Flow::Wait
    }
}

locals! {
    /// Looks at the console for Ctrl+C, Ctrl+S and Ctrl+P, as DOS does
    /// around console output and, with `BREAK=ON`, every call: Ctrl+C breaks
    /// the call, Ctrl+S pauses until the next key, Ctrl+P toggles the
    /// printer echo.
    pub struct BreakCheck {
        paused: bool,
    }
}

const PEEKED: u8 = 1;
const TAKEN: u8 = 2;
const RESUMED: u8 = 3;

impl Routine for BreakCheck {
    const ID: Id = Id::Break;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let con = console(k);
        match *phase {
            PEEKED => {
                let word = u16::try_from(take!(child)).unwrap_or(0);
                if word & status::BUSY != 0 {
                    return Flow::Return(Ok(0));
                }
                let key = k.request8(field::MEDIA);
                if !matches!(key, CTRL_C | CTRL_S | CTRL_P) {
                    return Flow::Return(Ok(0));
                }
                *phase = TAKEN;
                Self::read_one(k, con)
            }
            TAKEN => {
                take!(child);
                let at = k.scratch(level::BYTE);
                let key = k.get8(at);
                match key {
                    CTRL_C => Flow::Return(Err(Fault::Break)),
                    CTRL_P => {
                        let echo = k.get8(driver::ECHO);
                        k.set8(driver::ECHO, u8::from(echo == 0));
                        Flow::Return(Ok(0))
                    }
                    _ => {
                        *phase = RESUMED;
                        Self::read_one(k, con)
                    }
                }
            }
            RESUMED => {
                take!(child);
                let at = k.scratch(level::BYTE);
                let key = k.get8(at);
                if key == CTRL_C {
                    return Flow::Return(Err(Fault::Break));
                }
                Flow::Return(Ok(0))
            }
            _ => {
                if k.sda8(sda::BREAK_HIT) != 0 {
                    k.set_sda8(sda::BREAK_HIT, 0);
                }
                k.request(0, command::PEEK);
                *phase = PEEKED;
                Flow::Call(Frame::of(&Device { header: con }))
            }
        }
    }
}

impl BreakCheck {
    /// Reads a key from the console into the scratch's name.
    fn read_one(k: &mut Kernel<'_, '_>, con: Far) -> Flow {
        let at = k.scratch(level::BYTE);
        Flow::Call(Frame::of(&CharIo {
            device: con,
            command: command::INPUT,
            buffer: Far::new(SEGMENT, at),
            count: 1,
            done: 0,
            status: 0,
        }))
    }
}

locals! {
    /// Waits for a key at the console: asking it, and while none is there,
    /// calling INT 28h and halting until the next interrupt.
    pub struct Idle {
        device: Far,
    }
}

const ASKED: u8 = 1;
const IDLED: u8 = 2;
const HALTED: u8 = 3;

impl Routine for Idle {
    const ID: Id = Id::Idle;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            ASKED => {
                let word = u16::try_from(take!(child)).unwrap_or(0);
                if word & status::BUSY == 0 {
                    return Flow::Return(Ok(0));
                }
                k.call_int(0x28);
                *phase = IDLED;
                Flow::Wait
            }
            IDLED => {
                k.call_far(Far::new(crate::bios::layout::SEGMENT, entry::IDLE));
                *phase = HALTED;
                Flow::Wait
            }
            _ => {
                k.request(0, command::INPUT_STATUS);
                *phase = ASKED;
                Flow::Call(Frame::of(&Device {
                    header: self.device,
                }))
            }
        }
    }
}

locals! {
    /// Writes `byte` to the standard output, cooked when that is the
    /// console: breaks and pauses looked for, a tab spread to the next
    /// eighth column, the column kept, the printer echo honoured.
    pub struct PutChar {
        byte: u8,
        spaces: u8,
        index: u8,
    }
}

const CHECKED: u8 = 1;
const WROTE: u8 = 2;
const ECHOED: u8 = 3;

impl Routine for PutChar {
    const ID: Id = Id::PutChar;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let (index, entry) = match k.handle(handle::OUTPUT) {
            Ok(found) => found,
            Err(fault) => return Flow::Return(Err(fault)),
        };
        let cooked = k
            .sft_device(entry)
            .is_some_and(|device| k.attributes(device) & attribute::STDOUT != 0)
            && k.sft16(entry, sft::INFO) & info::RAW == 0;
        match *phase {
            CHECKED => {
                take!(child);
            }
            WROTE => {
                take!(child);
                if cooked {
                    Self::track(k);
                }
                if self.spaces > 0 {
                    self.spaces -= 1;
                    return Self::write(k, phase, index, b' ');
                }
                if k.get8(driver::ECHO) != 0 {
                    if let Ok((printer, _)) = k.handle(handle::PRINTER) {
                        *phase = ECHOED;
                        self.index = printer;
                        let byte = Far::new(SEGMENT, k.scratch(level::BYTE) + 1);
                        k.set8(byte.offset, self.byte);
                        return Flow::Call(Frame::of(&Write {
                            index: u16::from(printer),
                            buffer: byte,
                            count: 1,
                            done: 0,
                            kept: 0,
                            next: 0,
                        }));
                    }
                }
                return Flow::Return(Ok(0));
            }
            ECHOED => {
                take!(child);
                return Flow::Return(Ok(0));
            }
            _ => {
                if cooked {
                    *phase = CHECKED;
                    return Flow::Call(Frame::of(&BreakCheck { paused: false }));
                }
            }
        }
        let byte = if cooked && self.byte == TAB {
            let column = k.get8(driver::COLUMN);
            self.spaces = 7 - column % 8;
            b' '
        } else {
            self.byte
        };
        Self::write(k, phase, index, byte)
    }
}

impl PutChar {
    fn write(k: &mut Kernel<'_, '_>, phase: &mut u8, index: u8, byte: u8) -> Flow {
        let at = k.scratch(level::BYTE);
        k.set8(at, byte);
        *phase = WROTE;
        Flow::Call(Frame::of(&Write {
            index: u16::from(index),
            buffer: Far::new(SEGMENT, at),
            count: 1,
            done: 0,
            kept: 0,
            next: 0,
        }))
    }

    /// Moves the console's column past the byte just written.
    fn track(k: &mut Kernel<'_, '_>) {
        let at = k.scratch(level::BYTE);
        let written = k.get8(at);
        let column = k.get8(driver::COLUMN);
        let next = match written {
            CR => 0,
            BACKSPACE => column.saturating_sub(1),
            byte if byte >= 0x20 => column.wrapping_add(1),
            _ => column,
        };
        k.set8(driver::COLUMN, next);
    }
}

locals! {
    /// Reads a byte from the standard input: from the console, waiting for
    /// it idle, Ctrl+C breaking when `cooked`, echoed to the standard
    /// output when `echo`; or from a file. With `wait` clear, a console with
    /// nothing waiting answers at once with none. Returns the byte, or
    /// `0x100` for none.
    pub struct GetChar {
        cooked: bool,
        echo: bool,
        wait: bool,
        byte: u8,
    }
}

/// What [`GetChar`] answers when nothing is waiting.
pub const NONE: u32 = 0x100;

const WAITED: u8 = 1;
const GOT: u8 = 2;
const PUT: u8 = 3;
const STATUS: u8 = 4;

impl Routine for GetChar {
    const ID: Id = Id::GetChar;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let (index, entry) = match k.handle(handle::INPUT) {
            Ok(found) => found,
            Err(fault) => return Flow::Return(Err(fault)),
        };
        let device = k.sft_device(entry);
        match *phase {
            STATUS => {
                let word = u16::try_from(take!(child)).unwrap_or(0);
                if word & status::BUSY != 0 {
                    return Flow::Return(Ok(NONE));
                }
                Self::read(k, phase, index, device)
            }
            WAITED => {
                take!(child);
                Self::read(k, phase, index, device)
            }
            GOT => {
                let read = take!(child);
                if read == 0 {
                    return Flow::Return(Ok(u32::from(CTRL_Z)));
                }
                let at = k.scratch(level::BYTE);
                self.byte = k.get8(at);
                if self.cooked && self.byte == CTRL_C {
                    return Flow::Return(Err(Fault::Break));
                }
                if self.echo {
                    *phase = PUT;
                    return Flow::Call(Frame::of(&PutChar {
                        byte: self.byte,
                        spaces: 0,
                        index: 0,
                    }));
                }
                Flow::Return(Ok(u32::from(self.byte)))
            }
            PUT => {
                take!(child);
                Flow::Return(Ok(u32::from(self.byte)))
            }
            _ => match device {
                Some(device) if !self.wait => {
                    k.request(0, command::INPUT_STATUS);
                    *phase = STATUS;
                    Flow::Call(Frame::of(&Device { header: device }))
                }
                Some(device) if k.attributes(device) & attribute::STDIN != 0 => {
                    *phase = WAITED;
                    Flow::Call(Frame::of(&Idle { device }))
                }
                _ => Self::read(k, phase, index, device),
            },
        }
    }
}

impl GetChar {
    fn read(k: &mut Kernel<'_, '_>, phase: &mut u8, index: u8, device: Option<Far>) -> Flow {
        let buffer = Far::new(SEGMENT, k.scratch(level::BYTE));
        *phase = GOT;
        Flow::Call(byte_read(index, device, buffer))
    }
}

/// One byte of the standard input read into `buffer`: a device's through
/// its driver, so a console's key comes back as typed rather than through
/// the cooked line a handle read edits; a file's through handle `index`.
/// Returns the bytes read.
fn byte_read(index: u8, device: Option<Far>, buffer: Far) -> Frame {
    match device {
        Some(device) => Frame::of(&CharIo {
            device,
            command: command::INPUT,
            buffer,
            count: 1,
            done: 0,
            status: 0,
        }),
        None => Frame::of(&Read {
            index: u16::from(index),
            buffer,
            count: 1,
            done: 0,
            steps: 0,
        }),
    }
}

locals! {
    /// Function `0Bh`: whether a byte waits at the standard input, not
    /// taken: a console looked at for Ctrl+C first, a file's not at its end.
    /// Returns `1` when one does.
    pub struct InputStatus {}
}

const BREAK_LOOKED: u8 = 1;
const STATUS_ASKED: u8 = 2;

impl Routine for InputStatus {
    const ID: Id = Id::InputStatus;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let (_, entry) = match k.handle(handle::INPUT) {
            Ok(found) => found,
            Err(fault) => return Flow::Return(Err(fault)),
        };
        let Some(device) = k.sft_device(entry) else {
            let waiting = k.sft32(entry, sft::POSITION) < k.sft32(entry, sft::SIZE);
            return Flow::Return(Ok(u32::from(waiting)));
        };
        match *phase {
            STATUS_ASKED => {
                let word = u16::try_from(take!(child)).unwrap_or(0);
                Flow::Return(Ok(u32::from(word & status::BUSY == 0)))
            }
            BREAK_LOOKED => {
                take!(child);
                if k.get8(driver::CON_LINE + 1) < k.get8(driver::CON_LINE) {
                    return Flow::Return(Ok(1));
                }
                k.request(0, command::INPUT_STATUS);
                *phase = STATUS_ASKED;
                Flow::Call(Frame::of(&Device { header: device }))
            }
            _ => {
                if k.attributes(device) & attribute::STDIN != 0 {
                    *phase = BREAK_LOOKED;
                    return Flow::Call(Frame::of(&BreakCheck { paused: false }));
                }
                k.request(0, command::INPUT_STATUS);
                *phase = STATUS_ASKED;
                Flow::Call(Frame::of(&Device { header: device }))
            }
        }
    }
}

locals! {
    /// DOS's line input: keys read from the console at `device` into
    /// `buffer`, up to `count` bytes and the CR ending them, edited with the
    /// template — F1 or → a character of it, F2 up to one, F3 the rest, F4
    /// skipping to one, F5 the line as the new template, Del skipping one,
    /// Ins inserting, Esc cancelling, ← or BS deleting — echoed as they
    /// go, an LF before any other key dropped and one after it a new line on
    /// the screen only. Returns the bytes in the line, the CR included. When
    /// `entry` is set, the read is a cooked read of `CON` through that SFT
    /// entry, which takes a whole line into the kernel's and hands it over
    /// as asked. When `piped`, the keys are instead the bytes of a standard
    /// input that is no console, read as function `08h` reads them with no
    /// wait between them, and its end ends the line as a CR would.
    pub struct LineInput {
        device: Far,
        buffer: Far,
        count: u16,
        entry: Far,
        piped: bool,
    }
}

/// The line editor's state, kept in its scratch beside the line itself.
mod edit {
    /// Where the line is built.
    pub const LINE: u16 = 0x00;
    /// Bytes in it.
    pub const LENGTH: u16 = 0x80;
    /// Where the template is read from next.
    pub const TEMPLATE_AT: u16 = 0x81;
    /// Insert mode.
    pub const INSERT: u16 = 0x82;
    /// What a pending F2 or F4 waits for: `2` or `4`.
    pub const PENDING: u16 = 0x83;
    /// The key read.
    pub const KEY: u16 = 0x84;
    /// Whether the key read was an extended one's scan code.
    pub const EXTENDED: u16 = 0x85;
    /// The part of the line still to be echoed: from, then to.
    pub const SHOW: u16 = 0x86;
    /// Set while the second byte of a `^X` is still to be echoed.
    pub const HALF: u16 = 0x88;
    /// The sequence echoed after that part, and how far it has gone.
    pub const TAIL: u16 = 0x89;
    /// Bytes the editor keeps.
    pub const LEN: u16 = 0x8B;

    const _: () = assert!(LEN <= super::level::LINE_LEN);
}

/// What the editor echoes after the part of the line it shows.
mod tail {
    pub const NONE: u8 = 0;
    pub const BELL: u8 = 1;
    pub const RUB: u8 = 2;
    pub const RUB_WIDE: u8 = 3;
    pub const CANCEL: u8 = 4;
    pub const TEMPLATE: u8 = 5;
    pub const END: u8 = 6;
    pub const LINE_BREAK: u8 = 7;
}

/// What rubs out the character before the cursor.
pub(crate) const RUB_OUT: &[u8] = b"\x08 \x08";

/// The sequences [`tail`] names.
const TAILS: [&[u8]; 8] = [
    b"",
    &[BELL],
    RUB_OUT,
    b"\x08\x08  \x08\x08",
    b"\\\r\n",
    b"@\r\n",
    b"\r",
    b"\r\n",
];

const KEY_READ: u8 = 4;
const ECHOING: u8 = 5;
const LINE_DONE: u8 = 6;
const IDLE_DONE: u8 = 7;
const SCAN_READ: u8 = 8;
const FIRST_READ: u8 = 9;
const FIRST_IDLE_DONE: u8 = 10;

impl Routine for LineInput {
    const ID: Id = Id::LineInput;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let base = k.scratch(level::LINE);
        match *phase {
            IDLE_DONE | FIRST_IDLE_DONE => {
                take!(child);
                let next = if *phase == FIRST_IDLE_DONE {
                    FIRST_READ
                } else {
                    KEY_READ
                };
                return self.read_key(k, phase, base, next);
            }
            KEY_READ | FIRST_READ => {
                let first = *phase == FIRST_READ;
                if self.piped && take!(child) == 0 {
                    return self.key(k, phase, base, CR);
                }
                let key = k.get8(base + edit::KEY);
                // So that a line read from a file with CR LF endings starts
                // at its own first byte.
                if first && key == LF {
                    return self.next_key(k, phase, base, false);
                }
                if key == 0 {
                    return self.read_key(k, phase, base, SCAN_READ);
                }
                k.set8(base + edit::EXTENDED, 0);
                return self.key(k, phase, base, key);
            }
            SCAN_READ => {
                if self.piped && take!(child) == 0 {
                    return self.key(k, phase, base, CR);
                }
                k.set8(base + edit::EXTENDED, 1);
                let scan = k.get8(base + edit::KEY);
                return self.extended(k, phase, base, scan);
            }
            ECHOING => {
                take!(child);
                return self.echoing(k, phase, base);
            }
            LINE_DONE => {
                take!(child);
                return self.finish(k);
            }
            _ => {
                if self.entry.segment != 0 {
                    if let Some(flow) = self.hand_over(k) {
                        return flow;
                    }
                }
                k.set8(base + edit::LENGTH, 0);
                k.set8(base + edit::TEMPLATE_AT, 0);
                k.set8(base + edit::INSERT, 0);
                k.set8(base + edit::PENDING, 0);
            }
        }
        self.next_key(k, phase, base, true)
    }
}

/// The pending mark the editor leaves once the line is ended.
const LINE_FINISHED: u8 = 0xFF;

impl LineInput {
    /// The most the line may hold, the CR included.
    fn room(&self) -> u8 {
        let limit = if self.entry.segment != 0 {
            128
        } else {
            self.count.min(128)
        };
        u8::try_from(limit.max(1)).unwrap_or(128)
    }

    fn read_key(&self, k: &mut Kernel<'_, '_>, phase: &mut u8, base: u16, next: u8) -> Flow {
        *phase = next;
        let buffer = Far::new(SEGMENT, base + edit::KEY);
        if !self.piped {
            return Flow::Call(byte_read(0, Some(self.device), buffer));
        }
        match k.handle(handle::INPUT) {
            Ok((index, entry)) => {
                let device = k.sft_device(entry);
                Flow::Call(byte_read(index, device, buffer))
            }
            Err(fault) => Flow::Return(Err(fault)),
        }
    }

    /// Waits for the next key, the line's `first`: idle at a console, read
    /// at once when piped.
    fn next_key(&self, k: &mut Kernel<'_, '_>, phase: &mut u8, base: u16, first: bool) -> Flow {
        if self.piped {
            return self.read_key(k, phase, base, if first { FIRST_READ } else { KEY_READ });
        }
        *phase = if first { FIRST_IDLE_DONE } else { IDLE_DONE };
        Flow::Call(Frame::of(&Idle {
            device: self.device,
        }))
    }

    /// Echoes the line from byte `from` to its end, then the sequence
    /// `tail`; then reads the next key — or, when `last`, ends the line.
    fn echo(
        &self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        base: u16,
        from: u8,
        tail: u8,
        last: bool,
    ) -> Flow {
        let to = k.get8(base + edit::LENGTH);
        k.set8(base + edit::SHOW, from.min(to));
        k.set8(base + edit::SHOW + 1, to);
        k.set8(base + edit::HALF, 0);
        k.set8(base + edit::TAIL, tail);
        k.set8(base + edit::TAIL + 1, 0);
        if last {
            k.set8(base + edit::PENDING, LINE_FINISHED);
        }
        self.echoing(k, phase, base)
    }

    /// Echoes the next byte; with none left, ends the line or waits for the
    /// next key.
    fn echoing(&self, k: &mut Kernel<'_, '_>, phase: &mut u8, base: u16) -> Flow {
        if let Some(byte) = next_echo(k, base) {
            *phase = ECHOING;
            return Flow::Call(Frame::of(&PutChar {
                byte,
                spaces: 0,
                index: 0,
            }));
        }
        if k.get8(base + edit::PENDING) == LINE_FINISHED {
            return self.finish(k);
        }
        self.next_key(k, phase, base, false)
    }

    fn template(k: &mut Kernel<'_, '_>) -> ([u8; 128], usize) {
        let mut out = [0u8; 128];
        let len = usize::from(k.get8(driver::TEMPLATE)).min(127);
        k.g.read_bytes(SEGMENT, driver::TEMPLATE + 1, &mut out[..len]);
        (out, len)
    }

    fn line(k: &mut Kernel<'_, '_>, base: u16) -> ([u8; 128], usize) {
        let mut out = [0u8; 128];
        let len = usize::from(k.get8(base + edit::LENGTH)).min(127);
        k.g.read_bytes(SEGMENT, base + edit::LINE, &mut out[..len]);
        (out, len)
    }

    /// Appends `bytes` to the line as far as it has room, and echoes them;
    /// a byte past the room rings the bell instead.
    fn add(
        &self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        base: u16,
        bytes: &[u8],
        skip: usize,
    ) -> Flow {
        let (mut line, len) = Self::line(k, base);
        let room = usize::from(self.room()) - 1;
        let fits = bytes.len().min(room.saturating_sub(len));
        line[len..len + fits].copy_from_slice(&bytes[..fits]);
        k.g.write_bytes(SEGMENT, base + edit::LINE, &line[..len + fits]);
        k.set8(base + edit::LENGTH, u8::try_from(len + fits).unwrap_or(0));
        let at = k.get8(base + edit::TEMPLATE_AT);
        k.set8(
            base + edit::TEMPLATE_AT,
            at.saturating_add(u8::try_from(skip).unwrap_or(0)),
        );
        let tail = if fits < bytes.len() {
            tail::BELL
        } else {
            tail::NONE
        };
        self.echo(k, phase, base, u8::try_from(len).unwrap_or(0), tail, false)
    }

    fn key(&self, k: &mut Kernel<'_, '_>, phase: &mut u8, base: u16, key: u8) -> Flow {
        let pending = k.get8(base + edit::PENDING);
        if pending == 2 || pending == 4 {
            k.set8(base + edit::PENDING, 0);
            let (template, len) = Self::template(k);
            let from = usize::from(k.get8(base + edit::TEMPLATE_AT)).min(len);
            let Some(upto) = template_reach(&template[..len], from, key) else {
                return self.read_key(k, phase, base, KEY_READ);
            };
            if pending == 4 {
                k.set8(base + edit::TEMPLATE_AT, u8::try_from(upto).unwrap_or(0));
                return self.read_key(k, phase, base, KEY_READ);
            }
            return self.add(k, phase, base, &template[from..upto], upto - from);
        }
        match key {
            CR => {
                let (mut line, len) = Self::line(k, base);
                line[len] = CR;
                k.g.write_bytes(SEGMENT, base + edit::LINE, &line[..=len]);
                k.set8(base + edit::LENGTH, u8::try_from(len + 1).unwrap_or(0));
                k.set8(driver::TEMPLATE, u8::try_from(len).unwrap_or(0));
                k.g.write_bytes(SEGMENT, driver::TEMPLATE + 1, &line[..len]);
                self.echo(k, phase, base, u8::MAX, tail::END, true)
            }
            BACKSPACE | DEL => self.rub_out(k, phase, base),
            LF => self.echo(k, phase, base, u8::MAX, tail::LINE_BREAK, false),
            CTRL_F => self.next_key(k, phase, base, false),
            ESC => {
                k.set8(base + edit::LENGTH, 0);
                k.set8(base + edit::TEMPLATE_AT, 0);
                self.echo(k, phase, base, 0, tail::CANCEL, false)
            }
            CTRL_C => Flow::Return(Err(Fault::Break)),
            CTRL_P => {
                let echo = k.get8(driver::ECHO);
                k.set8(driver::ECHO, u8::from(echo == 0));
                self.read_key(k, phase, base, KEY_READ)
            }
            byte => {
                let skip = usize::from(k.get8(base + edit::INSERT) == 0);
                self.add(k, phase, base, &[byte], skip)
            }
        }
    }

    /// Takes the last byte off the line, and off the screen: two columns for
    /// one shown as `^X`.
    fn rub_out(&self, k: &mut Kernel<'_, '_>, phase: &mut u8, base: u16) -> Flow {
        let (line, len) = Self::line(k, base);
        let Some(&last) = line[..len].last() else {
            return self.read_key(k, phase, base, KEY_READ);
        };
        k.set8(base + edit::LENGTH, u8::try_from(len - 1).unwrap_or(0));
        let at = k.get8(base + edit::TEMPLATE_AT);
        k.set8(base + edit::TEMPLATE_AT, at.saturating_sub(1));
        let rub = if last < 0x20 && last != TAB {
            tail::RUB_WIDE
        } else {
            tail::RUB
        };
        self.echo(k, phase, base, u8::MAX, rub, false)
    }

    fn extended(&self, k: &mut Kernel<'_, '_>, phase: &mut u8, base: u16, key: u8) -> Flow {
        use scan::{DELETE, F1, F2, F3, F4, F5, INSERT, LEFT, RIGHT};
        let (template, len) = Self::template(k);
        let from = usize::from(k.get8(base + edit::TEMPLATE_AT)).min(len);
        match key {
            F1 | RIGHT if from < len => self.add(k, phase, base, &template[from..=from], 1),
            F3 if from < len => self.add(k, phase, base, &template[from..len], len - from),
            F2 => {
                k.set8(base + edit::PENDING, 2);
                self.read_key(k, phase, base, KEY_READ)
            }
            F4 => {
                k.set8(base + edit::PENDING, 4);
                self.read_key(k, phase, base, KEY_READ)
            }
            F5 => {
                let (line, len) = Self::line(k, base);
                k.set8(driver::TEMPLATE, u8::try_from(len).unwrap_or(0));
                k.g.write_bytes(SEGMENT, driver::TEMPLATE + 1, &line[..len]);
                k.set8(base + edit::LENGTH, 0);
                k.set8(base + edit::TEMPLATE_AT, 0);
                self.echo(k, phase, base, 0, tail::TEMPLATE, false)
            }
            DELETE => {
                k.set8(
                    base + edit::TEMPLATE_AT,
                    u8::try_from((from + 1).min(len)).unwrap_or(0),
                );
                self.read_key(k, phase, base, KEY_READ)
            }
            INSERT => {
                let insert = k.get8(base + edit::INSERT);
                k.set8(base + edit::INSERT, u8::from(insert == 0));
                self.read_key(k, phase, base, KEY_READ)
            }
            LEFT => self.rub_out(k, phase, base),
            _ => self.read_key(k, phase, base, KEY_READ),
        }
    }

    /// The line is ended: for function `0Ah`, it is the caller's; for a
    /// cooked `CON` read, the kernel keeps it, with an LF, and hands over
    /// what was asked.
    fn finish(&self, k: &mut Kernel<'_, '_>) -> Flow {
        let base = k.scratch(level::LINE);
        let (line, len) = Self::line(k, base);
        if self.entry.segment == 0 {
            let copied = len.min(usize::from(self.count));
            k.g.write_bytes(self.buffer.segment, self.buffer.offset, &line[..copied]);
            return Flow::Return(Ok(u32::try_from(len).unwrap_or(0)));
        }
        let mut kept = [0u8; 130];
        kept[..len].copy_from_slice(&line[..len]);
        let total = (len + 1).min(129);
        kept[total - 1] = LF;
        k.set8(driver::CON_LINE, u8::try_from(total).unwrap_or(0));
        k.set8(driver::CON_LINE + 1, 0);
        k.g.write_bytes(SEGMENT, driver::CON_LINE + 2, &kept[..total]);
        if let Some(flow) = self.hand_over(k) {
            return flow;
        }
        Flow::Return(Ok(0))
    }

    /// Hands a cooked `CON` read what the kept line still holds, if
    /// anything: up to the count asked for, ending at its LF.
    fn hand_over(&self, k: &mut Kernel<'_, '_>) -> Option<Flow> {
        let total = k.get8(driver::CON_LINE);
        let next = k.get8(driver::CON_LINE + 1);
        if next >= total {
            return None;
        }
        let mut line = [0u8; 130];
        let len = usize::from(total).min(129);
        k.g.read_bytes(SEGMENT, driver::CON_LINE + 2, &mut line[..len]);
        let from = usize::from(next);
        let wanted = usize::from(self.count);
        let end = line[from..len]
            .iter()
            .position(|&b| b == LF)
            .map_or(len, |at| from + at + 1)
            .min(from + wanted);
        k.g.write_bytes(self.buffer.segment, self.buffer.offset, &line[from..end]);
        k.set8(driver::CON_LINE + 1, u8::try_from(end).unwrap_or(0));
        if line[from..end].first() == Some(&CTRL_Z) {
            let info = k.sft16(self.entry, sft::INFO);
            k.set_sft16(self.entry, sft::INFO, info & !info::NOT_EOF);
            return Some(Flow::Return(Ok(0)));
        }
        Some(Flow::Return(Ok(u32::try_from(end - from).unwrap_or(0))))
    }
}

/// The next byte the editor echoes: the line's part still to show, a
/// control byte as `^X`, then its tail sequence.
fn next_echo(k: &mut Kernel<'_, '_>, base: u16) -> Option<u8> {
    let from = k.get8(base + edit::SHOW);
    let to = k.get8(base + edit::SHOW + 1).min(0x7F);
    if from < to {
        let byte = k.get8(base + edit::LINE + u16::from(from));
        if byte < 0x20 && byte != TAB {
            if k.get8(base + edit::HALF) == 0 {
                k.set8(base + edit::HALF, 1);
                return Some(b'^');
            }
            k.set8(base + edit::HALF, 0);
            k.set8(base + edit::SHOW, from + 1);
            return Some(byte + 0x40);
        }
        k.set8(base + edit::SHOW, from + 1);
        return Some(byte);
    }
    let sequence = TAILS.get(usize::from(k.get8(base + edit::TAIL)))?;
    let at = k.get8(base + edit::TAIL + 1);
    let byte = *sequence.get(usize::from(at))?;
    k.set8(base + edit::TAIL + 1, at + 1);
    Some(byte)
}

locals! {
    /// Reads `CLOCK$`: returns the date and time as a directory entry holds
    /// them, the date in the high word, the record left in the scratch's
    /// clock area; or, when `read` is clear, writes the record there to it.
    pub struct Clock {
        read: bool,
    }
}

impl Routine for Clock {
    const ID: Id = Id::Clock;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let record = k.scratch(level::CLOCK);
        if *phase == CALLED {
            let word = u16::try_from(take!(child)).unwrap_or(0);
            if word & status::ERROR != 0 {
                return Flow::Return(Err(Fault::Error(super::device::extended(word))));
            }
            if !self.read {
                return Flow::Return(Ok(0));
            }
            let mut bytes = [0u8; 6];
            k.g.read_bytes(SEGMENT, record, &mut bytes);
            return Flow::Return(Ok(stamp_of(bytes)));
        }
        let device = clock(k);
        k.request(
            0,
            if self.read {
                command::INPUT
            } else {
                command::OUTPUT
            },
        );
        k.set_request_far(field::ADDRESS, Far::new(SEGMENT, record));
        k.set_request16(field::COUNT, 6);
        *phase = CALLED;
        Flow::Call(Frame::of(&Device { header: device }))
    }
}

/// The date and time `CLOCK$`'s six bytes name, as a directory entry holds
/// them: the date in the high word, or `0`, no stamp, for a reading an entry
/// cannot hold.
#[must_use]
pub fn stamp_of(record: [u8; 6]) -> u32 {
    let [low, high, minutes, hours, _, seconds] = record;
    let (year, month, day) = civil_of(u16::from_le_bytes([low, high]));
    let civil = CivilTime {
        year: i64::from(year),
        month: u32::from(month),
        day: u32::from(day),
        hour: u32::from(hours),
        minute: u32::from(minutes),
        second: u32::from(seconds),
    };
    DosTime::of_civil(&civil).map_or(0, |stamp| {
        u32::from(stamp.date) << 16 | u32::from(stamp.time)
    })
}

/// The civil date `days` after 1980-01-01.
#[must_use]
pub fn civil_of(days: u16) -> (u16, u8, u8) {
    let epoch = tairix_abi::time::days_from_civil(1980, 1, 1).unwrap_or(0);
    let (year, month, day) = tairix_abi::time::civil_from_days(epoch + i64::from(days));
    (
        u16::try_from(year).unwrap_or(1980),
        u8::try_from(month).unwrap_or(1),
        u8::try_from(day).unwrap_or(1),
    )
}

/// Days since 1980-01-01 of a civil date, if DOS counts it.
#[must_use]
pub fn days_of(year: u16, month: u8, day: u8) -> Option<u16> {
    let epoch = tairix_abi::time::days_from_civil(1980, 1, 1)?;
    let date =
        tairix_abi::time::days_from_civil(i64::from(year), u32::from(month), u32::from(day))?;
    let civil = tairix_abi::time::civil_from_days(date);
    if civil != (i64::from(year), u32::from(month), u32::from(day)) {
        return None;
    }
    u16::try_from(date - epoch).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clocks_reading_is_stamped_as_an_entry_holds_it_or_not_at_all() {
        let days = days_of(2026, 11, 10).unwrap().to_le_bytes();
        let stamp = stamp_of([days[0], days[1], 45, 15, 0, 31]);
        assert_eq!(stamp, 0x5D6A_7DAF, "2026-11-10 15:45:30, to two seconds");
        let late = days_of(2107, 12, 31).unwrap() + 1;
        assert_eq!(
            stamp_of([late.to_le_bytes()[0], late.to_le_bytes()[1], 0, 0, 0, 0]),
            0,
            "2108 is past what an entry holds"
        );
        assert_eq!(
            stamp_of([days[0], days[1], 0, 99, 0, 0]),
            0,
            "an hour wider than its bits"
        );
    }
}
