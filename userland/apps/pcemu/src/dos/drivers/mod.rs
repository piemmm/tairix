//! The built-in device drivers, as guest code reached through their headers:
//! each interrupt entry saves the caller's registers, keeps
//! [`rom::DRIVER_LOCALS`] bytes on the stack, and makes BIOS calls that
//! return to the drivers' landing pad until the request is done.
//!
//! The strategy entry every built-in device shares records the request; the
//! interrupt entry copies the record into its own locals before anything
//! else, so a request made while it waits — a TSR's, from a BIOS call's
//! handler — cannot disturb it.

mod ansi;
mod ansi_ioctl;
mod block;
mod block_ioctl;
mod clock;
mod con;
mod mouse;
mod serial;

pub use ansi::image as ansi_image;
pub use mouse::image as mouse_image;
pub use mouse::{
    keep as mouse_keep, look as mouse_look, probe as mouse_probe, probe_step as mouse_probe_step,
    Follower as MouseFollower,
};

pub use block::{int13, units_of, Unit};

use super::data::{driver, Builtin, SEGMENT};
use super::device::{command, error, field, status};
use super::engine::{Far, Saved};
use super::rom;
use crate::asm::Reg16;
use crate::bios::guest::Guest;
use crate::cpu::Seg;

/// A built-in driver's locals, as offsets from SS:SP.
mod local {
    /// The request.
    pub const REQUEST: u16 = 0;
    /// Which built-in device.
    pub const DEVICE: u16 = 4;
    /// Where the request has reached.
    pub const PHASE: u16 = 5;
    /// Units moved so far.
    pub const DONE: u16 = 6;
    /// Units the BIOS call in flight moves.
    pub const CHUNK: u16 = 8;
    /// Tries left for it.
    pub const TRIES: u16 = 10;
    /// The driver's own.
    pub const SCRATCH: u16 = 12;
}

/// A driver's view of the machine while it serves a request.
pub struct Driver<'g, 'm> {
    g: &'g mut Guest<'m>,
}

impl Driver<'_, '_> {
    fn local8(&mut self, at: u16) -> u8 {
        self.g.local(at)
    }

    fn set_local8(&mut self, at: u16, value: u8) {
        self.g.set_local(at, value);
    }

    fn local16(&mut self, at: u16) -> u16 {
        u16::from_le_bytes([self.g.local(at), self.g.local(at + 1)])
    }

    fn set_local16(&mut self, at: u16, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.g.set_local(at, low);
        self.g.set_local(at + 1, high);
    }

    fn local32(&mut self, at: u16) -> u32 {
        u32::from(self.local16(at)) | u32::from(self.local16(at + 2)) << 16
    }

    fn set_local32(&mut self, at: u16, value: u32) {
        let (low, high) = super::engine::words(value);
        self.set_local16(at, low);
        self.set_local16(at + 2, high);
    }

    fn request(&mut self) -> Far {
        let offset = self.local16(local::REQUEST);
        Far::new(self.local16(local::REQUEST + 2), offset)
    }

    /// A byte of the request.
    fn get8(&mut self, field: u16) -> u8 {
        let at = self.request().add(field);
        self.g.read8(at.segment, at.offset)
    }

    fn set8(&mut self, field: u16, value: u8) {
        let at = self.request().add(field);
        self.g.write8(at.segment, at.offset, value);
    }

    fn get16(&mut self, field: u16) -> u16 {
        let at = self.request().add(field);
        self.g.read16(at.segment, at.offset)
    }

    fn set16(&mut self, field: u16, value: u16) {
        let at = self.request().add(field);
        self.g.write16(at.segment, at.offset, value);
    }

    fn get_far(&mut self, field: u16) -> Far {
        let offset = self.get16(field);
        Far::new(self.get16(field + 2), offset)
    }

    fn set_far(&mut self, field: u16, pointer: Far) {
        self.set16(field, pointer.offset);
        self.set16(field + 2, pointer.segment);
    }

    fn phase(&mut self) -> u8 {
        self.local8(local::PHASE)
    }

    fn set_phase(&mut self, phase: u8) {
        self.set_local8(local::PHASE, phase);
    }

    /// Ends the request: done, failed with `code` when given.
    fn finish(&mut self, code: Option<u8>) {
        let word = match code {
            None => status::DONE,
            Some(code) => status::DONE | status::ERROR | u16::from(code),
        };
        self.set16(field::STATUS, word);
    }

    /// Ends the request done, its busy bit `busy`.
    fn finish_busy(&mut self, busy: bool) {
        let word = if busy {
            status::DONE | status::BUSY
        } else {
            status::DONE
        };
        self.set16(field::STATUS, word);
    }

    /// A loaded driver's own segment, which its stub passed in DS.
    fn instance(&mut self) -> u16 {
        self.local16(local::SCRATCH)
    }

    /// A byte of the loaded driver's own memory.
    fn own8(&mut self, field: u16) -> u8 {
        let segment = self.instance();
        self.g.read8(segment, field)
    }

    fn set_own8(&mut self, field: u16, value: u8) {
        let segment = self.instance();
        self.g.write8(segment, field, value);
    }

    fn own16(&mut self, field: u16) -> u16 {
        let segment = self.instance();
        self.g.read16(segment, field)
    }

    fn set_own16(&mut self, field: u16, value: u16) {
        let segment = self.instance();
        self.g.write16(segment, field, value);
    }

    /// A register the entry saved, as its caller had it; what is set here
    /// the way out restores.
    fn saved(&mut self, reg: Saved) -> u16 {
        self.local16(u16::from(rom::DRIVER_LOCALS) + reg as u16 * 2)
    }

    fn set_saved(&mut self, reg: Saved, value: u16) {
        self.set_local16(u16::from(rom::DRIVER_LOCALS) + reg as u16 * 2, value);
    }

    /// Takes INT `vector`, which returns to the drivers' landing pad.
    fn bios(&mut self, vector: u8) {
        self.g.interrupt(
            vector,
            (crate::bios::layout::SEGMENT, rom::entry::DRIVER_STEP),
        );
    }
}

/// A built-in device's strategy entry: records ES:BX.
pub fn strategy(g: &mut Guest<'_>) {
    let pointer = Far::new(g.seg(Seg::Es), g.r16(Reg16::Bx));
    g.write16(SEGMENT, driver::REQUEST, pointer.offset);
    g.write16(SEGMENT, driver::REQUEST + 2, pointer.segment);
}

/// `NUL`: reads nothing, takes everything. Its strategy and interrupt entry
/// are one, so it answers whichever request ES:BX names when it is called.
pub fn nul(g: &mut Guest<'_>) {
    let request = Far::new(g.seg(Seg::Es), g.r16(Reg16::Bx));
    let command = g.read8(request.segment, request.offset.wrapping_add(field::COMMAND));
    if command == command::INPUT {
        g.write16(
            request.segment,
            request.offset.wrapping_add(field::COUNT),
            0,
        );
    }
    let at = request.offset.wrapping_add(field::STATUS);
    g.write16(request.segment, at, status::DONE);
}

/// What a driver frame serves: a built-in device, or a driver loaded from
/// the disk whose stub passed its own segment in DS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// A built-in device's request.
    Builtin(Builtin),
    /// A request to `ANSI.SYS`.
    Ansi,
    /// `ANSI.SYS`'s INT 29h.
    AnsiFast,
    /// INT 33h, to `MOUSE.COM`.
    Mouse,
    /// The mouse's port interrupted.
    MouseIrq,
}

/// The number [`local::DEVICE`] records a loaded driver's frame by, past
/// every built-in device's.
const LOADED: u8 = 0x40;

impl Served {
    const fn number(self) -> u8 {
        match self {
            Self::Builtin(device) => device as u8,
            Self::Ansi => LOADED,
            Self::AnsiFast => LOADED + 1,
            Self::Mouse => LOADED + 2,
            Self::MouseIrq => LOADED + 3,
        }
    }

    fn of(number: u8) -> Option<Self> {
        match number {
            LOADED => Some(Self::Ansi),
            n if n == LOADED + 1 => Some(Self::AnsiFast),
            n if n == LOADED + 2 => Some(Self::Mouse),
            n if n == LOADED + 3 => Some(Self::MouseIrq),
            n => Builtin::ALL
                .into_iter()
                .find(|b| *b as u8 == n)
                .map(Self::Builtin),
        }
    }

    /// Whether it serves a device request.
    const fn has_request(self) -> bool {
        matches!(self, Self::Builtin(_) | Self::Ansi)
    }
}

/// A driver's interrupt entry was called: the request it serves is taken
/// into its locals, and a loaded driver's own segment.
pub fn start(g: &mut Guest<'_>, served: Served) {
    let mut d = Driver { g };
    if served.has_request() {
        let offset = d.g.read16(SEGMENT, driver::REQUEST);
        let segment = d.g.read16(SEGMENT, driver::REQUEST + 2);
        d.set_local16(local::REQUEST, offset);
        d.set_local16(local::REQUEST + 2, segment);
    }
    if !matches!(served, Served::Builtin(_)) {
        let own = d.g.seg(crate::cpu::Seg::Ds);
        d.set_local16(local::SCRATCH, own);
    }
    d.set_local8(local::DEVICE, served.number());
    d.set_phase(0);
    d.set_local16(local::DONE, 0);
    run(&mut d, served);
}

/// A driver's BIOS call returned.
pub fn step(g: &mut Guest<'_>) {
    let mut d = Driver { g };
    let number = d.local8(local::DEVICE);
    let Some(served) = Served::of(number) else {
        return;
    };
    run(&mut d, served);
}

fn run(d: &mut Driver<'_, '_>, served: Served) {
    let device = match served {
        Served::Builtin(device) => device,
        Served::Ansi => return ansi::serve(d),
        Served::AnsiFast => return ansi::fast(d),
        Served::Mouse => return mouse::serve(d),
        Served::MouseIrq => return mouse::irq(d),
    };
    match device {
        Builtin::Con => con::serve(d),
        Builtin::Clock => clock::serve(d),
        Builtin::Block => block::serve(d),
        Builtin::Aux | Builtin::Com1 => serial::serve_com(d, 0),
        Builtin::Com2 => serial::serve_com(d, 1),
        Builtin::Com3 => serial::serve_com(d, 2),
        Builtin::Com4 => serial::serve_com(d, 3),
        Builtin::Prn | Builtin::Lpt1 => serial::serve_lpt(d, 0),
        Builtin::Lpt2 => serial::serve_lpt(d, 1),
        Builtin::Lpt3 => serial::serve_lpt(d, 2),
    }
}

/// The request's command, refusing one the driver does not know.
fn unknown(d: &mut Driver<'_, '_>) {
    d.finish(Some(error::UNKNOWN_COMMAND));
}
