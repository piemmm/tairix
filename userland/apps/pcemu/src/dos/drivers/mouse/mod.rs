//! `MOUSE.COM`: the INT 33h mouse driver, a program that stays resident for
//! a Microsoft serial mouse on the first COM port one answers on. Its file
//! is a stub — the installer's loop, the INT 33h handler and the port's
//! interrupt handler, the last two far-calling the ROM with DS their own
//! segment, where everything the driver keeps lies.
//!
//! It answers the Microsoft mouse functions `00h`–`24h` as version 6.26
//! does: the cursor in every text and graphics mode the BIOS sets, motion
//! as mickeys and as a position within the range set, presses and releases
//! counted, event handlers, sensitivity and the double-speed threshold, its
//! state saved and restored, and the driver disabled and enabled.

mod cursor;
mod functions;

use super::Driver;
use crate::asm::{Asm, AsmError, Cond, Reg16};
use crate::bios::bda;
use crate::bios::guest::{Guest, Poll};
use crate::bios::layout::{ask_until_done, SEGMENT as ROM};
use crate::cpu::{Bus, Seg};
use crate::dos::engine::{Far, Saved};
use crate::dos::rom::entry;
use crate::ports::mouse::HostPointer;
use crate::ports::serial::{ier, lsr, mcr, reg, DLAB};
use crate::time::Nanos;
use alloc::vec;
use alloc::vec::Vec;

/// Where things lie in the program's memory, from its PSP.
mod at {
    /// The installer, where the program starts.
    pub const START: u16 = crate::dos::memory::psp::LEN;
    /// The INT 33h handler.
    pub const INT33: u16 = 0x140;
    /// The port's interrupt handler.
    pub const IRQ: u16 = 0x150;
    /// The port's base.
    pub const PORT: u16 = 0x160;
    /// The interrupt line it raises.
    pub const LINE: u16 = 0x162;
    /// Whether the driver serves its port; function `1Fh` stops it.
    pub const ENABLED: u16 = 0x163;
    /// Whether the display's registers read back, as the VGA's do.
    pub const READABLE: u16 = 0x164;
    /// What the installer found, as [`super::outcome`] numbers it.
    pub const OUTCOME: u16 = 0x165;
    /// The port the installer is trying, and its step there.
    pub const TRYING: u16 = 0x166;
    pub const STEP: u16 = 0x167;
    /// The vectors the driver replaced.
    pub const OLD_33: u16 = 0x168;
    pub const OLD_IRQ: u16 = 0x16C;
    /// When the installer's wait ends.
    pub const DEADLINE: u16 = 0x170;
    /// A packet's bytes as they arrive, and how many have.
    pub const PACKET: u16 = 0x178;
    pub const PACKET_LEN: u16 = 0x17B;
    /// Set while an event handler runs.
    pub const BUSY: u16 = 0x17C;
    /// The tick speed is measured over, and the mickeys moved in it.
    pub const SPEED_TICK: u16 = 0x17E;
    pub const SPEED_MICKEYS: u16 = 0x180;
    /// What the cursor drawn covers, as [`super::cursor`] keeps it.
    pub const DRAWN: u16 = 0x182;
    pub const DRAWN_MODE: u16 = 0x183;
    pub const DRAWN_X: u16 = 0x184;
    pub const DRAWN_Y: u16 = 0x186;
    pub const DRAWN_PAGE: u16 = 0x188;
    /// What the drawn cursor covers.
    pub const BACKGROUND: u16 = 0x190;
    pub const BACKGROUND_LEN: u16 = 0x100;
    /// The state functions `15h`–`17h` save and restore.
    pub const STATE: u16 = BACKGROUND + BACKGROUND_LEN;
    /// The end of what stays resident.
    pub const END: u16 = STATE + super::state::LEN;
}

/// The state's fields, from [`at::STATE`].
mod state {
    /// The position, and the mickeys short of the next unit each way.
    pub const X: u16 = 0x00;
    pub const Y: u16 = 0x02;
    pub const FRACTION_X: u16 = 0x04;
    pub const FRACTION_Y: u16 = 0x06;
    /// The mickeys moved since function `0Bh` last read them.
    pub const MOTION_X: u16 = 0x08;
    pub const MOTION_Y: u16 = 0x0A;
    /// The buttons held.
    pub const BUTTONS: u16 = 0x0C;
    /// The cursor shows while this count is zero.
    pub const VISIBLE: u16 = 0x0E;
    /// Each button's presses since last asked, and where the last was:
    /// a count and a position, the left button's first.
    pub const PRESSES: u16 = 0x10;
    pub const RELEASES: u16 = 0x1C;
    /// The range the position keeps to.
    pub const MIN_X: u16 = 0x28;
    pub const MAX_X: u16 = 0x2A;
    pub const MIN_Y: u16 = 0x2C;
    pub const MAX_Y: u16 = 0x2E;
    /// The graphics cursor's hot spot and masks.
    pub const HOT_X: u16 = 0x30;
    pub const HOT_Y: u16 = 0x32;
    pub const SCREEN_MASK: u16 = 0x34;
    pub const CURSOR_MASK: u16 = 0x54;
    /// The text cursor: the software one's masks or the hardware one's
    /// scan lines.
    pub const TEXT_KIND: u16 = 0x74;
    pub const TEXT_FIRST: u16 = 0x76;
    pub const TEXT_SECOND: u16 = 0x78;
    /// The event handler and the events it is called for.
    pub const EVENT_MASK: u16 = 0x7A;
    pub const HANDLER: u16 = 0x7C;
    /// The alternate handlers: three of a mask, with its shift keys, and a
    /// handler.
    pub const ALTERNATES: u16 = 0x80;
    pub const ALTERNATE_LEN: u16 = 6;
    /// Mickeys a unit of eight virtual pixels takes each way.
    pub const RATIO_X: u16 = 0x92;
    pub const RATIO_Y: u16 = 0x94;
    /// The speed, in mickeys a second, past which motion is doubled.
    pub const THRESHOLD: u16 = 0x96;
    /// The horizontal, vertical and threshold sensitivities.
    pub const SENSITIVITY: u16 = 0x98;
    /// The display page the cursor is drawn on.
    pub const PAGE: u16 = 0x9E;
    /// Whether a region the cursor hides in is set, and the region.
    pub const EXCLUDING: u16 = 0xA0;
    pub const EXCLUSION: u16 = 0xA2;
    /// Light-pen emulation, and the language messages are given in.
    pub const LIGHT_PEN: u16 = 0xAA;
    pub const LANGUAGE: u16 = 0xAC;
    pub const LEN: u16 = 0xAE;
}

/// The events a handler is called for.
mod event {
    pub const MOVED: u16 = 0x01;
    pub const LEFT_DOWN: u16 = 0x02;
    pub const LEFT_UP: u16 = 0x04;
    pub const RIGHT_DOWN: u16 = 0x08;
    pub const RIGHT_UP: u16 = 0x10;
    /// Every event.
    pub const ALL: u16 = 0x1F;
    /// An alternate handler's shift keys: Shift, Ctrl and Alt.
    pub const KEYS: u16 = 0xE0;
}

/// What the installer found.
pub mod outcome {
    pub const INSTALLED: u8 = 1;
    pub const ALREADY: u8 = 2;
    pub const ABSENT: u8 = 3;
}

/// The installer's steps at a port.
mod step {
    pub const POWER_OFF: u8 = 0;
    pub const OFF: u8 = 1;
    pub const LISTEN: u8 = 2;
}

/// How the driver sets the port: seven data bits, no parity, one stop
/// bit, at 1200 bits a second from the 1.8432 MHz clock; and the bytes
/// taken in one interrupt at most, a 16550's FIFO.
mod line {
    pub const SEVEN_N1: u8 = 0x02;
    pub const DIVISOR: u8 = 96;
    pub const BURST: usize = 16;
}

/// The version the driver reports, and the kind of mouse.
const VERSION: u16 = 0x0626;
const SERIAL: u8 = 2;
/// The buttons it reports.
const BUTTONS: u16 = 2;
/// A powered-down mouse's rest before it is powered up again, and how long
/// it is given to say `M`: its wake time and a character, with room.
const OFF_TIME: Nanos = 20_000_000;
const ANSWER_TIME: Nanos = 100_000_000;
/// An `IRET` instruction, which an unused vector points at.
const IRET: u8 = 0xCF;
/// The defaults function `00h` restores.
const RATIO_X: u16 = 8;
const RATIO_Y: u16 = 16;
const THRESHOLD: u16 = 64;
const SENSITIVITY: u16 = 50;
/// The text cursor's default masks: the character kept, its colours
/// swapped.
const TEXT_AND: u16 = 0x77FF;
const TEXT_XOR: u16 = 0x7700;
/// The virtual screen's width in every mode.
const WIDTH: u16 = 640;

/// The program's file: the installer, then the handlers' stubs and room for
/// everything the driver keeps.
///
/// # Errors
///
/// [`AsmError`] should its code not assemble, a defect of its own.
pub fn image() -> Result<Vec<u8>, AsmError> {
    let mut image = vec![0u8; usize::from(at::END - at::START)];
    for (offset, code) in [
        (at::START, installer()?),
        (at::INT33, handler(entry::MOUSE)?),
        (at::IRQ, handler(entry::MOUSE_IRQ)?),
    ] {
        let start = usize::from(offset - at::START);
        image[start..start + code.len()].copy_from_slice(&code);
    }
    Ok(image)
}

/// Asks the ROM whether INT 33h may be called, calls it to find a driver
/// there, then has the ROM look for the mouse, halting until the next
/// interrupt while it waits; says what it found, and stays or ends.
fn installer() -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(at::START);
    let search = asm.label();
    asm.call_far(ROM, entry::MOUSE_LOOK);
    asm.jcc(Cond::Ne, &search);
    asm.int(0x33);
    asm.bind(&search);
    ask_until_done(
        &mut asm,
        |asm| asm.call_far(ROM, entry::MOUSE_PROBE),
        |asm| asm.call_far(ROM, entry::MOUSE_PROBE_STEP),
    );
    asm.sti();
    asm.int(0x21);
    asm.push_seg(Seg::Cs);
    asm.pop_seg(Seg::Ds);
    asm.call_far(ROM, entry::MOUSE_KEEP);
    asm.int(0x21);
    asm.finish()
}

/// What tells the emulator's own driver from any other: the code of its two
/// handlers, where INT 33h and its port's interrupt reach it.
#[derive(Clone, Debug)]
pub struct Follower {
    int33: Vec<u8>,
    irq: Vec<u8>,
}

impl Follower {
    /// The driver's handlers, as a guest's memory holds them.
    ///
    /// # Errors
    ///
    /// [`AsmError`] should the handlers not assemble, a defect of the
    /// driver's own.
    pub fn new() -> Result<Self, AsmError> {
        Ok(Self {
            int33: handler(entry::MOUSE)?,
            irq: handler(entry::MOUSE_IRQ)?,
        })
    }

    /// Whether the guest's mouse is read through the emulator's own driver,
    /// installed and serving its port, INT 33h and the port's interrupt both
    /// reaching it: then it may follow the host's pointer.
    #[must_use]
    pub fn follows(&self, board: &crate::board::Board) -> bool {
        let word = |at: u32| u16::from_le_bytes([board.peek(at), board.peek(at + 1)]);
        let vector = |n: u32| (word(n * 4 + 2), word(n * 4));
        let (own, offset) = vector(0x33);
        let base = u32::from(own) << 4;
        let byte = |field: u16| board.peek(base + u32::from(field));
        let holds = |at: u16, code: &[u8]| (at..).zip(code).all(|(field, &b)| byte(field) == b);
        let line = byte(at::LINE);
        offset == at::INT33
            && holds(at::INT33, &self.int33)
            && byte(at::ENABLED) != 0
            && line < 8
            && vector(8 + u32::from(line)) == (own, at::IRQ)
            && holds(at::IRQ, &self.irq)
    }
}

/// An interrupt handler that far-calls the ROM's `target` with DS its own
/// segment.
fn handler(target: u16) -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(0);
    asm.push_seg(Seg::Ds);
    asm.push_seg(Seg::Cs);
    asm.pop_seg(Seg::Ds);
    asm.call_far(ROM, target);
    asm.pop_seg(Seg::Ds);
    asm.iret();
    asm.finish()
}

/// The driver's own memory, as a host call reaches it.
struct Mouse<'a, 'g> {
    g: &'a mut Guest<'g>,
    own: u16,
}

impl Mouse<'_, '_> {
    fn get8(&mut self, field: u16) -> u8 {
        self.g.read8(self.own, field)
    }

    fn set8(&mut self, field: u16, value: u8) {
        self.g.write8(self.own, field, value);
    }

    fn get16(&mut self, field: u16) -> u16 {
        self.g.read16(self.own, field)
    }

    fn set16(&mut self, field: u16, value: u16) {
        self.g.write16(self.own, field, value);
    }

    fn state(&mut self, field: u16) -> u16 {
        self.get16(at::STATE + field)
    }

    fn set_state(&mut self, field: u16, value: u16) {
        self.set16(at::STATE + field, value);
    }

    fn signed(&mut self, field: u16) -> i16 {
        i16::from_le_bytes(self.state(field).to_le_bytes())
    }

    fn set_signed(&mut self, field: u16, value: i16) {
        self.set_state(field, u16::from_le_bytes(value.to_le_bytes()));
    }

    fn far(&mut self, field: u16) -> Far {
        let offset = self.get16(field);
        Far::new(self.get16(field + 2), offset)
    }

    fn set_far(&mut self, field: u16, value: Far) {
        self.set16(field, value.offset);
        self.set16(field + 2, value.segment);
    }

    fn port(&mut self) -> u16 {
        self.get16(at::PORT)
    }

    /// The state a reset leaves: the cursor hidden at the screen's centre,
    /// the range the whole screen, the default shapes, ratios, threshold
    /// and sensitivity, no handlers, nothing counted.
    fn reset(&mut self) {
        self.erase();
        let buttons = self.state(state::BUTTONS);
        let blank = [0u8; state::LEN as usize];
        self.g.write_bytes(self.own, at::STATE, &blank);
        self.set_state(state::BUTTONS, buttons);
        self.set_signed(state::VISIBLE, -1);
        let height = self.view().height;
        self.set_state(state::MAX_X, WIDTH - 1);
        self.set_state(state::MAX_Y, height.saturating_sub(1));
        self.set_state(state::X, WIDTH / 2);
        self.set_state(state::Y, height / 2);
        for (n, (screen, cursor)) in (0u16..).zip(cursor::ARROW) {
            self.set_state(state::SCREEN_MASK + n * 2, screen);
            self.set_state(state::CURSOR_MASK + n * 2, cursor);
        }
        self.set_state(state::TEXT_FIRST, TEXT_AND);
        self.set_state(state::TEXT_SECOND, TEXT_XOR);
        self.set_state(state::RATIO_X, RATIO_X);
        self.set_state(state::RATIO_Y, RATIO_Y);
        self.set_state(state::THRESHOLD, THRESHOLD);
        for n in 0..3 {
            self.set_state(state::SENSITIVITY + n * 2, SENSITIVITY);
        }
        self.set_state(state::LIGHT_PEN, 1);
    }

    /// Moves to `x`, `y`, kept to the range, and redraws.
    fn place(&mut self, x: i16, y: i16) {
        let (low_x, high_x) = (self.signed(state::MIN_X), self.signed(state::MAX_X));
        let (low_y, high_y) = (self.signed(state::MIN_Y), self.signed(state::MAX_Y));
        self.set_signed(state::X, x.clamp(low_x, high_x.max(low_x)));
        self.set_signed(state::Y, y.clamp(low_y, high_y.max(low_y)));
        self.update();
    }

    /// Takes every byte the port has received, a packet at a time: the
    /// events they made.
    fn receive(&mut self) -> u16 {
        let base = self.port();
        let mut events = 0;
        for _ in 0..line::BURST {
            if self.g.input(base.wrapping_add(reg::LSR)) & lsr::DATA_READY == 0 {
                break;
            }
            let byte = self.g.input(base.wrapping_add(reg::DATA));
            if let Some(packet) = self.assemble(byte) {
                events |= self.apply(packet);
            }
        }
        events
    }

    /// Adds `byte` to the packet arriving, which its first byte's bit 6
    /// begins: the packet, once it is whole.
    fn assemble(&mut self, byte: u8) -> Option<[u8; 3]> {
        let len = if byte & 0x40 != 0 {
            0
        } else {
            self.get8(at::PACKET_LEN)
        };
        if !(byte & 0x40 != 0 || (1..3).contains(&len)) {
            return None;
        }
        self.set8(at::PACKET + u16::from(len), byte);
        if len < 2 {
            self.set8(at::PACKET_LEN, len + 1);
            return None;
        }
        self.set8(at::PACKET_LEN, 0);
        Some([self.get8(at::PACKET), self.get8(at::PACKET + 1), byte])
    }

    /// A packet's motion and buttons taken: the counters, the position and
    /// the cursor moved; the events it made.
    fn apply(&mut self, packet: [u8; 3]) -> u16 {
        let [high, low_x, low_y] = packet;
        let dx = i16::from(i8::from_le_bytes([((high & 0x03) << 6) | (low_x & 0x3F)]));
        let dy = i16::from(i8::from_le_bytes([((high & 0x0C) << 4) | (low_y & 0x3F)]));
        let buttons = u16::from(high & 0x20 != 0) | u16::from(high & 0x10 != 0) << 1;
        let (dx, dy) = self.accelerated(dx, dy);
        let mut events = 0;
        let counted = dx != 0 || dy != 0;
        if counted {
            for (field, delta) in [(state::MOTION_X, dx), (state::MOTION_Y, dy)] {
                let moved = self.signed(field).wrapping_add(delta);
                self.set_signed(field, moved);
            }
        }
        let placed = match self.g.host_pointer() {
            Some(at) => self.follow(at),
            None if counted => {
                let x = self.advance(state::X, state::FRACTION_X, state::RATIO_X, dx);
                let y = self.advance(state::Y, state::FRACTION_Y, state::RATIO_Y, dy);
                self.place(x, y);
                false
            }
            None => false,
        };
        if counted || placed {
            events |= event::MOVED;
        }
        let held = self.state(state::BUTTONS);
        for (button, (down, up)) in [
            (event::LEFT_DOWN, event::LEFT_UP),
            (event::RIGHT_DOWN, event::RIGHT_UP),
        ]
        .into_iter()
        .enumerate()
        {
            let bit = 1 << button;
            let (was, is) = (held & bit != 0, buttons & bit != 0);
            if was != is {
                events |= if is { down } else { up };
                self.count(if is { state::PRESSES } else { state::RELEASES }, button);
            }
        }
        self.set_state(state::BUTTONS, buttons);
        events
    }

    /// The position moved to where the host's pointer stands over the
    /// screen, kept to the range: whether it moved.
    fn follow(&mut self, at: HostPointer) -> bool {
        let height = self.view().height;
        let along = |fraction: u16, extent: u16| {
            i16::try_from((u32::from(fraction) * u32::from(extent)) >> 16).unwrap_or(i16::MAX)
        };
        let before = (self.signed(state::X), self.signed(state::Y));
        self.set_signed(state::FRACTION_X, 0);
        self.set_signed(state::FRACTION_Y, 0);
        self.place(along(at.x, WIDTH), along(at.y, height));
        (self.signed(state::X), self.signed(state::Y)) != before
    }

    /// Button `button`'s press or release counted where the cursor is.
    fn count(&mut self, table: u16, button: usize) {
        let at = table + u16::try_from(button).unwrap_or(0) * 6;
        let count = self.state(at).saturating_add(1);
        self.set_state(at, count);
        let (x, y) = (self.state(state::X), self.state(state::Y));
        self.set_state(at + 2, x);
        self.set_state(at + 4, y);
    }

    /// The motion scaled by the sensitivities, and doubled while the mouse
    /// moves faster than the threshold: mickeys counted over each tick.
    fn accelerated(&mut self, dx: i16, dy: i16) -> (i16, i16) {
        let tick = self.g.bda16(bda::TICKS);
        let moved = dx.unsigned_abs().saturating_add(dy.unsigned_abs());
        let so_far = if self.get16(at::SPEED_TICK) == tick {
            self.get16(at::SPEED_MICKEYS)
        } else {
            0
        };
        self.set16(at::SPEED_TICK, tick);
        self.set16(at::SPEED_MICKEYS, so_far.saturating_add(moved));
        let sensitivity =
            |m: &mut Self, n: u16| i32::from(m.state(state::SENSITIVITY + n * 2).clamp(1, 100));
        let (horizontal, vertical, speed) = (
            sensitivity(self, 0),
            sensitivity(self, 1),
            sensitivity(self, 2),
        );
        // A tick is 1/18.2 s: the threshold scaled to one, with its own
        // sensitivity.
        let threshold = i32::from(self.state(state::THRESHOLD)) * 10 * i32::from(SENSITIVITY);
        let fast = i32::from(so_far.saturating_add(moved)) * 182 * speed > threshold;
        let factor = if fast { 2 } else { 1 };
        let scale = |delta: i16, by: i32| {
            let scaled = i32::from(delta) * by * factor / i32::from(SENSITIVITY);
            i16::try_from(scaled.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
        };
        (scale(dx, horizontal), scale(dy, vertical))
    }

    /// The position along one axis moved by `mickeys` at its ratio, the
    /// part short of a unit kept for the next move.
    fn advance(&mut self, position: u16, fraction: u16, ratio: u16, mickeys: i16) -> i16 {
        let ratio = i32::from(self.state(ratio).max(1));
        let total = i32::from(self.signed(fraction)) + i32::from(mickeys) * 8;
        let rest = i16::try_from(total % ratio).unwrap_or(0);
        self.set_signed(fraction, rest);
        let moved = i32::from(self.signed(position)) + total / ratio;
        i16::try_from(moved.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
    }

    /// The handler `events` call, and the events it is told of: an
    /// alternate whose shift keys are held, else the handler; none while
    /// one runs.
    fn handler_for(&mut self, events: u16) -> Option<(Far, u16)> {
        if events == 0 || self.get8(at::BUSY) != 0 {
            return None;
        }
        let shift = self.g.bda8(bda::SHIFT_FLAGS);
        let held = u16::from(shift & 0x03 != 0) << 5
            | u16::from(shift & 0x04 != 0) << 6
            | u16::from(shift & 0x08 != 0) << 7;
        for n in 0..3 {
            let at = at::STATE + state::ALTERNATES + n * state::ALTERNATE_LEN;
            let mask = self.get16(at);
            let handler = self.far(at + 2);
            if mask & event::KEYS != 0
                && mask & event::KEYS == held
                && events & mask & event::ALL != 0
                && handler != Far::default()
            {
                return Some((handler, events & mask & event::ALL));
            }
        }
        let mask = self.state(state::EVENT_MASK);
        let handler = self.far(at::STATE + state::HANDLER);
        (events & mask != 0 && handler != Far::default()).then_some((handler, events & mask))
    }

    /// Points the port's line at the driver and lets it interrupt.
    fn connect(&mut self) {
        let base = self.port();
        let line = self.get8(at::LINE) & 7;
        let vector = (0x08 + u16::from(line)) * 4;
        let old = Far::new(self.g.read16(0, vector + 2), self.g.read16(0, vector));
        self.set_far(at::OLD_IRQ, old);
        self.g.write16(0, vector, at::IRQ);
        self.g.write16(0, vector + 2, self.own);
        drain(self.g, base);
        self.g.output(base.wrapping_add(reg::IER), ier::RECEIVED);
        self.g
            .output(base.wrapping_add(reg::MCR), mcr::DTR | mcr::RTS | mcr::OUT2);
        let mask = self.g.input(0x21);
        self.g.output(0x21, mask & !(1 << line));
        self.set8(at::ENABLED, 1);
    }

    /// Stops the port interrupting and gives its vector back.
    fn disconnect(&mut self) {
        let base = self.port();
        let line = self.get8(at::LINE) & 7;
        self.g.output(base.wrapping_add(reg::IER), 0);
        self.g
            .output(base.wrapping_add(reg::MCR), mcr::DTR | mcr::RTS);
        let mask = self.g.input(0x21);
        self.g.output(0x21, mask | 1 << line);
        let old = self.far(at::OLD_IRQ);
        let vector = (0x08 + u16::from(line)) * 4;
        self.g.write16(0, vector, old.offset);
        self.g.write16(0, vector + 2, old.segment);
        self.set8(at::ENABLED, 0);
    }
}

/// Reads every byte the port holds, so none is taken for a packet's.
fn drain(g: &mut Guest<'_>, base: u16) {
    for _ in 0..line::BURST {
        if g.input(base.wrapping_add(reg::LSR)) & lsr::DATA_READY == 0 {
            return;
        }
        g.input(base.wrapping_add(reg::DATA));
    }
}

/// The installer's first call: whether INT 33h may be called to find a
/// driver there, ZF set when it may; AX zero either way.
pub fn look(g: &mut Guest<'_>) {
    let (offset, segment) = (g.read16(0, 0x33 * 4), g.read16(0, 0x33 * 4 + 2));
    let callable = (segment, offset) != (0, 0) && g.read8(segment, offset) != IRET;
    g.set_r16(Reg16::Ax, 0);
    g.done(callable);
}

/// The installer's next call, with what INT 33h answered in AX: a driver
/// there already, or the search for the mouse begun. ZF set once it ends.
pub fn probe(g: &mut Guest<'_>) {
    let own = g.seg(Seg::Ds);
    if g.r16(Reg16::Ax) == 0xFFFF {
        return finish(g, own, outcome::ALREADY);
    }
    g.write8(own, at::TRYING, 0);
    g.write8(own, at::STEP, step::POWER_OFF);
    search(g, own);
}

/// An interrupt came while the installer waited: the search goes on.
pub fn probe_step(g: &mut Guest<'_>) {
    let own = g.seg(Seg::Ds);
    search(g, own);
}

/// Powers each port's mouse down and up in turn and listens for its `M`:
/// the first that answers is the driver's.
fn search(g: &mut Guest<'_>, own: u16) {
    loop {
        let trying = g.read8(own, at::TRYING);
        if trying >= 4 {
            return finish(g, own, outcome::ABSENT);
        }
        let base = g.bda16(bda::COM_PORTS + u16::from(trying) * 2);
        if base == 0 {
            next_port(g, own);
            continue;
        }
        let register = |register: u16| base.wrapping_add(register);
        match g.read8(own, at::STEP) {
            step::POWER_OFF => {
                for (at, value) in [
                    (reg::LCR, DLAB),
                    (reg::DATA, line::DIVISOR),
                    (reg::IER, 0),
                    (reg::LCR, line::SEVEN_N1),
                    (reg::IER, 0),
                    (reg::MCR, 0),
                ] {
                    g.output(register(at), value);
                }
                drain(g, base);
                set_deadline(g, own, OFF_TIME);
                g.write8(own, at::STEP, step::OFF);
            }
            step::OFF => {
                let until = deadline(g, own);
                match g.poll(until, |_| false) {
                    Poll::Interrupted => return g.done(false),
                    Poll::Yielded => return g.resume(),
                    Poll::Ready | Poll::Expired => {}
                }
                drain(g, base);
                g.output(register(reg::MCR), mcr::DTR | mcr::RTS);
                set_deadline(g, own, ANSWER_TIME);
                g.write8(own, at::STEP, step::LISTEN);
            }
            _ => match listen(g, own, register(reg::LSR)) {
                Poll::Interrupted => return g.done(false),
                Poll::Yielded => return g.resume(),
                Poll::Expired => {
                    g.output(register(reg::MCR), 0);
                    next_port(g, own);
                }
                Poll::Ready => {
                    if g.input(register(reg::DATA)) == b'M' {
                        install(g, own, trying, base);
                        return finish(g, own, outcome::INSTALLED);
                    }
                }
            },
        }
    }
}

/// Waits for the port whose line status is at `status` to receive a byte.
fn listen(g: &mut Guest<'_>, own: u16, status: u16) -> Poll {
    let until = deadline(g, own);
    g.poll(until, |g| g.input(status) & lsr::DATA_READY != 0)
}

fn next_port(g: &mut Guest<'_>, own: u16) {
    let trying = g.read8(own, at::TRYING);
    g.write8(own, at::TRYING, trying.saturating_add(1));
    g.write8(own, at::STEP, step::POWER_OFF);
}

fn set_deadline(g: &mut Guest<'_>, own: u16, after: Nanos) {
    let at = g.now() + after;
    g.write_bytes(own, at::DEADLINE, &at.to_le_bytes());
}

fn deadline(g: &mut Guest<'_>, own: u16) -> Nanos {
    let mut bytes = [0u8; 8];
    g.read_bytes(own, at::DEADLINE, &mut bytes);
    Nanos::from_le_bytes(bytes)
}

/// The installer is done: what it found is kept for its last call, and its
/// message is in DS:DX for function `09h`.
fn finish(g: &mut Guest<'_>, own: u16, found: u8) {
    g.write8(own, at::OUTCOME, found);
    let message = match found {
        outcome::INSTALLED => entry::MOUSE_INSTALLED,
        outcome::ALREADY => entry::MOUSE_ALREADY,
        _ => entry::MOUSE_ABSENT,
    };
    g.set_r16(Reg16::Ax, 0x0900);
    g.regs_mut().set_sreg(Seg::Ds, ROM);
    g.set_r16(Reg16::Dx, message);
    g.done(true);
}

/// The mouse answered on COM port `trying`, at `base`: the driver takes
/// INT 33h and the port's line, and starts as reset.
fn install(g: &mut Guest<'_>, own: u16, trying: u8, base: u16) {
    let mut m = Mouse { g, own };
    m.set16(at::PORT, base);
    m.set8(at::LINE, if trying.is_multiple_of(2) { 4 } else { 3 });
    m.set8(at::PACKET_LEN, 0);
    m.set8(at::BUSY, 0);
    m.set8(at::DRAWN, cursor::drawn::NONE);
    let readable = cursor::readable_registers(m.g);
    m.set8(at::READABLE, u8::from(readable));
    let old = Far::new(m.g.read16(0, 0x33 * 4 + 2), m.g.read16(0, 0x33 * 4));
    m.set_far(at::OLD_33, old);
    m.g.write16(0, 0x33 * 4, at::INT33);
    m.g.write16(0, 0x33 * 4 + 2, own);
    m.set_state(state::BUTTONS, 0);
    m.reset();
    m.connect();
}

/// The installer's last call: function `31h` keeping the driver resident,
/// or `4Ch` ending without it.
pub fn keep(g: &mut Guest<'_>) {
    let own = g.seg(Seg::Ds);
    match g.read8(own, at::OUTCOME) {
        outcome::INSTALLED => {
            g.set_r16(Reg16::Ax, 0x3100);
            g.set_r16(Reg16::Dx, at::END.div_ceil(16));
        }
        outcome::ALREADY => g.set_r16(Reg16::Ax, 0x4C00),
        _ => g.set_r16(Reg16::Ax, 0x4C01),
    }
}

/// The registers a function answers in.
#[derive(Default)]
struct Answer {
    ax: Option<u16>,
    bx: Option<u16>,
    cx: Option<u16>,
    dx: Option<u16>,
    es: Option<u16>,
}

/// INT 33h.
pub fn serve(d: &mut Driver<'_, '_>) {
    let own = d.instance();
    let [ax, bx, cx, dx, si, di, es] = [
        Saved::Ax,
        Saved::Bx,
        Saved::Cx,
        Saved::Dx,
        Saved::Si,
        Saved::Di,
        Saved::Es,
    ]
    .map(|reg| d.saved(reg));
    let answer = Mouse { g: d.g, own }.function(ax, [bx, cx, dx, si, di], es);
    for (reg, value) in [
        (Saved::Ax, answer.ax),
        (Saved::Bx, answer.bx),
        (Saved::Cx, answer.cx),
        (Saved::Dx, answer.dx),
        (Saved::Es, answer.es),
    ] {
        if let Some(value) = value {
            d.set_saved(reg, value);
        }
    }
}

const CALLED: u8 = 1;

/// The port interrupted: its bytes taken, the interrupt ended, and the
/// handler the events call, if any, far-called with them.
pub fn irq(d: &mut Driver<'_, '_>) {
    let own = d.instance();
    if d.phase() == CALLED {
        d.g.write8(own, at::BUSY, 0);
        return;
    }
    let mut m = Mouse { g: d.g, own };
    let events = m.receive();
    m.g.output(0x20, 0x20);
    let Some((handler, condition)) = m.handler_for(events) else {
        return;
    };
    m.set8(at::BUSY, 1);
    let buttons = m.state(state::BUTTONS);
    let [x, y, mx, my] =
        [state::X, state::Y, state::MOTION_X, state::MOTION_Y].map(|field| m.state(field));
    for (reg, value) in [
        (Reg16::Ax, condition),
        (Reg16::Bx, buttons),
        (Reg16::Cx, x),
        (Reg16::Dx, y),
        (Reg16::Si, mx),
        (Reg16::Di, my),
    ] {
        m.g.set_r16(reg, value);
    }
    m.g.regs_mut().set_sreg(Seg::Ds, own);
    d.set_phase(CALLED);
    d.g.far_call((handler.segment, handler.offset), (ROM, entry::DRIVER_STEP));
}

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;
