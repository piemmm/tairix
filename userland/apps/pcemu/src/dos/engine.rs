//! How a call in progress runs: a stack of frames in the swappable data
//! area, each one routine's phase and locals, run in Rust until a routine
//! needs guest code, then resumed by the host call guest code returns into.
//!
//! A *root* is a call made to the kernel — INT 21h, 25h, 26h, 2Fh, the
//! boot — and the frames above it are the routines it has called. A call made
//! while another waits is a root nested on top; each root runs guest code on
//! the stack of its level. Every frame is decoded from guest memory and
//! checked, so a program that writes garbage over the frames fails its call;
//! it cannot make the kernel misbehave.

use super::data::{level, sda, DEPTH, FRAME, LEVEL, LEVELS, LEVEL_BASE, SDA, SEGMENT};
use super::table;
pub use super::table::Id;
use crate::asm::Reg16;
use crate::bios::guest::Guest;
use crate::cpu::{flag, Seg};
use crate::frames::{self, decode, encode, Context};
pub(crate) use crate::frames::{locals, take};
pub use crate::frames::{words, Far, Fault, Locals, Outcome, GENERAL_FAILURE};

/// A kernel routine: a step the engine runs each time the routine resumes.
pub trait Routine: Locals {
    /// Its number.
    const ID: Id;
    /// Runs on from `phase`, which it advances; `child` is the outcome of
    /// the routine it last called.
    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow;
}

/// What a kernel routine's step asks for next.
pub type Flow = frames::Flow<Id>;
/// A kernel frame, decoded.
pub type Frame = frames::Frame<Id>;

impl Frame {
    /// A frame starting `routine`.
    pub fn of<R: Routine>(routine: &R) -> Self {
        Self::new(R::ID, routine)
    }
}

/// Where a returning guest call lands: the ROM's step entry.
const PAD: (u16, u16) = (crate::bios::layout::SEGMENT, super::rom::entry::STEP);

/// The kernel as a routine sees it: guest memory and the CPU, the frames,
/// and the guest calls it can make.
pub struct Kernel<'g, 'm> {
    /// The machine.
    pub g: &'g mut Guest<'m>,
}

impl<'g, 'm> Kernel<'g, 'm> {
    /// The kernel over `g`.
    pub fn new(g: &'g mut Guest<'m>) -> Self {
        Self { g }
    }

    /// A byte of the kernel's segment.
    pub fn get8(&mut self, offset: u16) -> u8 {
        self.g.read8(SEGMENT, offset)
    }

    /// Sets a byte of the kernel's segment.
    pub fn set8(&mut self, offset: u16, value: u8) {
        self.g.write8(SEGMENT, offset, value);
    }

    /// A word of the kernel's segment.
    pub fn get16(&mut self, offset: u16) -> u16 {
        self.g.read16(SEGMENT, offset)
    }

    /// Sets a word of the kernel's segment.
    pub fn set16(&mut self, offset: u16, value: u16) {
        self.g.write16(SEGMENT, offset, value);
    }

    /// A double word of the kernel's segment.
    pub fn get32(&mut self, offset: u16) -> u32 {
        let low = self.get16(offset);
        u32::from(low) | u32::from(self.get16(offset.wrapping_add(2))) << 16
    }

    /// Sets a double word of the kernel's segment.
    pub fn set32(&mut self, offset: u16, value: u32) {
        let (low, high) = words(value);
        self.set16(offset, low);
        self.set16(offset.wrapping_add(2), high);
    }

    /// A far pointer in the kernel's segment.
    pub fn get_far(&mut self, offset: u16) -> Far {
        let low = self.get16(offset);
        Far::new(self.get16(offset.wrapping_add(2)), low)
    }

    /// Sets a far pointer in the kernel's segment.
    pub fn set_far(&mut self, offset: u16, pointer: Far) {
        self.set16(offset, pointer.offset);
        self.set16(offset.wrapping_add(2), pointer.segment);
    }

    /// A word at `pointer`.
    pub fn read16_at(&mut self, pointer: Far) -> u16 {
        self.g.read16(pointer.segment, pointer.offset)
    }

    /// Sets a word at `pointer`.
    pub fn write16_at(&mut self, pointer: Far, value: u16) {
        self.g.write16(pointer.segment, pointer.offset, value);
    }

    /// A far pointer at `pointer`.
    pub fn far_at(&mut self, pointer: Far) -> Far {
        let offset = self.read16_at(pointer);
        Far::new(self.read16_at(pointer.add(2)), offset)
    }

    /// A field of the swappable data area.
    pub fn sda8(&mut self, field: u16) -> u8 {
        self.get8(SDA + field)
    }

    /// Sets a field of the swappable data area.
    pub fn set_sda8(&mut self, field: u16, value: u8) {
        self.set8(SDA + field, value);
    }

    /// A word of the swappable data area.
    pub fn sda16(&mut self, field: u16) -> u16 {
        self.get16(SDA + field)
    }

    /// Sets a word of the swappable data area.
    pub fn set_sda16(&mut self, field: u16, value: u16) {
        self.set16(SDA + field, value);
    }

    /// The level of the call running now, whose region its routines use.
    pub fn level(&mut self) -> u8 {
        self.sda8(sda::ROOTS).saturating_sub(1).min(LEVELS - 1)
    }

    /// Where the running call's scratch `field` lies in the kernel's segment.
    pub fn scratch(&mut self, field: u16) -> u16 {
        LEVEL_BASE + u16::from(self.level()) * LEVEL + field
    }

    /// The top of the running call's guest stack.
    fn stack_top(&mut self) -> u16 {
        LEVEL_BASE + (u16::from(self.level()) + 1) * LEVEL
    }

    /// Calls `target` far, on the running call's stack; it returns to the
    /// kernel.
    pub fn call_far(&mut self, target: Far) {
        let top = self.stack_top();
        self.g.set_stack(SEGMENT, top);
        self.g.far_call((target.segment, target.offset), PAD);
    }

    /// Takes INT `vector`, on the running call's stack; it returns to the
    /// kernel.
    pub fn call_int(&mut self, vector: u8) {
        let top = self.stack_top();
        self.g.set_stack(SEGMENT, top);
        self.g.interrupt(vector, PAD);
    }

    /// Takes INT `vector` on the caller's stack, its registers above, as
    /// INT 24h and INT 23h are taken.
    pub fn call_int_on_caller(&mut self, vector: u8) {
        let frame = self.caller_frame();
        self.g.set_stack(frame.segment, frame.offset);
        self.g.interrupt(vector, PAD);
    }

    /// Where the running call's caller's registers lie.
    pub fn caller_frame(&mut self) -> Far {
        let at = self.scratch(level::CALLER);
        self.get_far(at)
    }

    /// A register of the running call's caller, as saved at its entry.
    pub fn caller(&mut self, reg: Saved) -> u16 {
        let frame = self.caller_frame();
        self.read16_at(frame.add(reg as u16 * 2))
    }

    /// Sets a register the running call's caller gets back.
    pub fn set_caller(&mut self, reg: Saved, value: u16) {
        let frame = self.caller_frame();
        self.write16_at(frame.add(reg as u16 * 2), value);
    }

    /// A byte register of the caller: the high half when `high`.
    pub fn caller8(&mut self, reg: Saved, high: bool) -> u8 {
        let [low, hi] = self.caller(reg).to_le_bytes();
        if high {
            hi
        } else {
            low
        }
    }

    /// Sets a byte register of the caller: the high half when `high`.
    pub fn set_caller8(&mut self, reg: Saved, high: bool, value: u8) {
        let [low, hi] = self.caller(reg).to_le_bytes();
        let word = if high {
            u16::from_le_bytes([low, value])
        } else {
            u16::from_le_bytes([value, hi])
        };
        self.set_caller(reg, word);
    }

    /// Sets the carry the caller gets back.
    pub fn set_caller_carry(&mut self, on: bool) {
        let flags = self.caller(Saved::Flags);
        let flags = if on {
            flags | flag::CF
        } else {
            flags & !flag::CF
        };
        self.set_caller(Saved::Flags, flags);
    }
}

/// The registers an INT 21h stub saves, in the order they lie above SS:SP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "the registers' own names")]
pub enum Saved {
    Ax,
    Bx,
    Cx,
    Dx,
    Si,
    Di,
    Bp,
    Ds,
    Es,
    Ip,
    Cs,
    Flags,
}

fn frame_at(n: u8) -> u16 {
    SDA + sda::FRAMES + u16::from(n) * FRAME
}

/// Begins `root`, a call whose caller's registers lie at SS:SP and which
/// returns through the ROM's `exit` (`0` for one that never returns),
/// nested on whatever is in progress. With no level or frame left for it, the
/// call fails at once.
pub fn enter(k: &mut Kernel<'_, '_>, root: &Frame, exit: u16) {
    let roots = k.sda8(sda::ROOTS);
    let depth = k.sda8(sda::DEPTH);
    let caller = Far::new(k.g.seg(Seg::Ss), k.g.r16(Reg16::Sp));
    if roots >= LEVELS || depth >= DEPTH {
        fail(k, caller, exit);
        return;
    }
    k.set_sda8(sda::ROOTS, roots + 1);
    k.set_sda8(sda::DEPTH, depth + 1);
    let at = k.scratch(level::CALLER);
    k.set_far(at, caller);
    let at = k.scratch(level::EXIT);
    k.set16(at, exit);
    let at = k.scratch(level::ROOT);
    k.set8(at, depth);
    let at = k.scratch(level::CHECKED);
    k.set32(at, 0);
    let at = k.scratch(level::FAILED);
    k.set8(at, 0);
    let at = k.scratch(level::CANARY);
    k.set16(at, CANARY);
    k.store(depth, root);
    frames::run(k);
}

/// Resumes the call whose guest code has returned, its records first found
/// intact: guest code is all that moves the call's stack.
pub fn resume(k: &mut Kernel<'_, '_>) {
    if k.sda8(sda::DEPTH) == 0 {
        // Guest code came back to the kernel with no call waiting for it: a
        // program jumped to the landing pad. It stops where it stands.
        k.g.jump(
            crate::bios::layout::SEGMENT,
            crate::bios::layout::entry::HALT,
        );
        return;
    }
    let at = k.scratch(level::CANARY);
    if k.get16(at) != CANARY {
        // Nothing the call keeps can be trusted: the machine halts, as DOS
        // does when its stacks run out.
        let top = k.stack_top();
        k.g.set_stack(SEGMENT, top);
        k.g.jump(
            crate::bios::layout::SEGMENT,
            super::rom::entry::STACK_OVERFLOW,
        );
        return;
    }
    frames::run(k);
}

/// What [`level::CANARY`] holds while the call's stack keeps above it.
const CANARY: u16 = 0x5AA5;

/// Drops the running call's frames, root and all.
fn end_root(k: &mut Kernel<'_, '_>) {
    let roots = k.sda8(sda::ROOTS);
    if roots == 0 {
        return;
    }
    // The last call's end leaves no frame, whatever its root index says.
    let root = if roots == 1 {
        0
    } else {
        k.root_index().min(DEPTH)
    };
    k.set_sda8(sda::DEPTH, root);
    k.set_sda8(sda::ROOTS, roots - 1);
}

/// Fails the running call whose frames no longer decode: its caller gets
/// the general failure.
fn fail_running(k: &mut Kernel<'_, '_>) {
    let caller = k.caller_frame();
    let at = k.scratch(level::EXIT);
    let exit = k.get16(at);
    end_root(k);
    fail(k, caller, exit);
}

/// Returns to the caller at `caller` through `exit` with the general
/// failure, as a call the kernel could not hold.
fn fail(k: &mut Kernel<'_, '_>, caller: Far, exit: u16) {
    k.write16_at(caller.add(Saved::Ax as u16 * 2), GENERAL_FAILURE);
    let at = caller.add(Saved::Flags as u16 * 2);
    let flags = k.read16_at(at);
    k.write16_at(at, flags | flag::CF);
    k.g.regs_mut().set_flag(flag::CF, true);
    leave(k, caller, exit);
}

/// Hands the CPU back to the caller at `caller` through the ROM's `exit`;
/// a call with no exit halts the machine, as a boot with nothing to run
/// does.
fn leave(k: &mut Kernel<'_, '_>, caller: Far, exit: u16) {
    if exit == 0 {
        k.g.jump(
            crate::bios::layout::SEGMENT,
            crate::bios::layout::entry::HALT,
        );
        return;
    }
    k.g.set_stack(caller.segment, caller.offset);
    k.g.jump(crate::bios::layout::SEGMENT, exit);
}

/// Drops every call in progress: a program has ended, and nothing it was
/// waiting on goes on.
pub fn clear(k: &mut Kernel<'_, '_>) {
    k.set_sda8(sda::DEPTH, 0);
    k.set_sda8(sda::ROOTS, 0);
}

impl Context for Kernel<'_, '_> {
    type Id = Id;
    const DEPTH: u8 = DEPTH;

    fn depth(&mut self) -> u8 {
        self.sda8(sda::DEPTH)
    }

    fn set_depth(&mut self, depth: u8) {
        self.set_sda8(sda::DEPTH, depth);
    }

    fn root_index(&mut self) -> u8 {
        let at = self.scratch(level::ROOT);
        self.get8(at)
    }

    fn load(&mut self, n: u8) -> Option<Frame> {
        let mut raw = [0u8; FRAME as usize];
        self.g.read_bytes(SEGMENT, frame_at(n), &mut raw);
        decode(&raw)
    }

    fn store(&mut self, n: u8, frame: &Frame) {
        self.g.write_bytes(SEGMENT, frame_at(n), &encode(frame));
    }

    fn step(&mut self, frame: &mut Frame) -> Flow {
        table::step(self, frame)
    }

    /// The call's routines have set what the caller gets back.
    fn returned(&mut self, _outcome: Outcome) {
        let caller = self.caller_frame();
        let at = self.scratch(level::EXIT);
        let exit = self.get16(at);
        end_root(self);
        leave(self, caller, exit);
    }

    fn transferred(&mut self) {
        end_root(self);
    }

    fn broken(&mut self) {
        fail_running(self);
    }
}
