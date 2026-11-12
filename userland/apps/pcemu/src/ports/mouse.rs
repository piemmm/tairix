//! The Microsoft serial mouse: two buttons, powered from its port's RTS and
//! DTR lines, reporting at 1200 bits a second in seven data bits. Raising
//! RTS powers it up, and it answers with `M`; it sends a three-byte packet
//! whenever it has moved or a button has changed, as fast as the line
//! carries them. Its counters saturate at a packet's reach, so movement
//! faster than the line can report is lost rather than sent late.

use super::serial::{Control, Format, Parity, Status, Stop};
use crate::snapshot::{restore_queue, save_queue, In, Out, State, Torn};
use crate::time::Nanos;
use alloc::collections::VecDeque;

/// 1200 bits a second, seven data bits, no parity, one stop bit.
pub const FORMAT: Format = Format {
    bits: 7,
    parity: Parity::None,
    stop: Stop::One,
    divisor: 96,
};

/// From power-up to the identifying byte.
const WAKE: Nanos = 10_000_000;
/// The byte a Microsoft mouse identifies itself with.
const IDENTITY: u8 = b'M';
/// The most bytes queued: a packet's three, the next made only once they are
/// sent.
const MOST_QUEUED: usize = 3;
/// The most one packet moves either way.
const REACH: i32 = 127;

/// Where the host's pointer stands over the displayed picture, as fractions
/// of it from its top left: `0` the first dot or line, `u16::MAX` the last.
///
/// No modelled hardware carries this. The emulator's own mouse driver,
/// running in its ROM, takes its position from it, so the guest's pointer
/// lies under the host's; every other driver moves by the mouse's packets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostPointer {
    /// Across.
    pub x: u16,
    /// Down.
    pub y: u16,
}

/// The buttons, as [`Mouse::motion`] takes them.
pub mod button {
    /// The left button.
    pub const LEFT: u8 = 0x01;
    /// The right button.
    pub const RIGHT: u8 = 0x02;
}

/// The mouse.
#[derive(Clone, Debug, Default)]
pub struct Mouse {
    powered: bool,
    /// Bytes waiting to go, the next not before `ready`.
    queue: VecDeque<u8>,
    ready: Nanos,
    /// Movement and buttons not yet sent.
    dx: i32,
    dy: i32,
    buttons: u8,
    sent_buttons: u8,
    /// A packet is owed though nothing it counts changed: the host's pointer
    /// moved less than a count.
    owed: bool,
}

impl Mouse {
    /// A mouse whose port has not powered it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            powered: false,
            queue: VecDeque::new(),
            ready: 0,
            dx: 0,
            dy: 0,
            buttons: 0,
            sent_buttons: 0,
            owed: false,
        }
    }

    /// The modem status the mouse drives: none.
    #[must_use]
    pub const fn status(&self) -> Status {
        Status {
            cts: false,
            dsr: false,
            ri: false,
            dcd: false,
        }
    }

    /// The port's modem lines at `t`: RTS rising with DTR up powers the
    /// mouse and it identifies itself; either falling powers it down.
    pub fn control(&mut self, control: Control, t: Nanos) {
        let powered = control.rts && control.dtr;
        if powered && !self.powered {
            self.queue.clear();
            self.queue.push_back(IDENTITY);
            self.ready = t + WAKE;
            self.dx = 0;
            self.dy = 0;
            self.sent_buttons = self.buttons;
            self.owed = false;
        } else if !powered {
            self.queue.clear();
        }
        self.powered = powered;
    }

    /// The mouse moves by `dx`, `dy` (down positive) with `buttons` held at
    /// `t`; `report` owes a packet even should nothing it counts change.
    pub fn motion(&mut self, dx: i32, dy: i32, buttons: u8, report: bool, t: Nanos) {
        self.dx = self.dx.saturating_add(dx).clamp(-REACH, REACH);
        self.dy = self.dy.saturating_add(dy).clamp(-REACH, REACH);
        self.buttons = buttons & (button::LEFT | button::RIGHT);
        self.owed |= report;
        if self.powered && self.queue.is_empty() {
            self.ready = self.ready.max(t);
            self.queue_packet();
        }
    }

    /// Queues a packet of what is unsent, if anything is.
    fn queue_packet(&mut self) {
        if self.dx == 0 && self.dy == 0 && self.buttons == self.sent_buttons && !self.owed {
            return;
        }
        self.owed = false;
        let [x] = i8::try_from(core::mem::take(&mut self.dx))
            .unwrap_or(0)
            .to_le_bytes();
        let [y] = i8::try_from(core::mem::take(&mut self.dy))
            .unwrap_or(0)
            .to_le_bytes();
        let left = u8::from(self.buttons & button::LEFT != 0);
        let right = u8::from(self.buttons & button::RIGHT != 0);
        self.queue.extend([
            0x40 | (left << 5) | (right << 4) | ((y >> 6) << 2) | (x >> 6),
            x & 0x3F,
            y & 0x3F,
        ]);
        self.sent_buttons = self.buttons;
    }

    /// When the next byte leaves.
    #[must_use]
    pub fn next_send(&self) -> Option<Nanos> {
        (!self.queue.is_empty()).then_some(self.ready)
    }

    /// The byte that leaves at `t`, its framing, and the next packet begun
    /// once a packet is out.
    pub fn send(&mut self, t: Nanos) -> Option<(u8, Format)> {
        let byte = self.queue.pop_front()?;
        self.ready = t + FORMAT.character_time();
        if self.queue.is_empty() {
            self.queue_packet();
        }
        Some((byte, FORMAT))
    }
}

impl State for Mouse {
    fn save(&self, out: &mut Out) {
        let Self {
            powered,
            queue,
            ready,
            dx,
            dy,
            buttons,
            sent_buttons,
            owed,
        } = self;
        powered.save(out);
        save_queue(queue, out);
        ready.save(out);
        dx.save(out);
        dy.save(out);
        buttons.save(out);
        sent_buttons.save(out);
        owed.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let Self {
            powered,
            queue,
            ready,
            dx,
            dy,
            buttons,
            sent_buttons,
            owed,
        } = self;
        powered.restore(input)?;
        restore_queue(queue, MOST_QUEUED, input)?;
        ready.restore(input)?;
        dx.restore(input)?;
        dy.restore(input)?;
        buttons.restore(input)?;
        sent_buttons.restore(input)?;
        owed.restore(input)
    }
}

crate::snapshot::state!(HostPointer { x, y });

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;
