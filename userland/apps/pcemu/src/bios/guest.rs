//! The machine as a host call sees it: the CPU's registers, and memory and
//! ports reached at the moment the CPU has reached.

use super::bda;
use crate::asm::{Reg16, Reg8};
use crate::cpu::{flag, linear, Bus, Regs, Seg};
use crate::machine::Machine;
use crate::ports::mouse::HostPointer;
use crate::time::Nanos;

/// The first gap between a poll's reads, about a turn of the PC BIOS's
/// status loop at 4.77 MHz, and the longest the gap doubles to.
const POLL_FIRST: Nanos = 8_000;
const POLL_LONGEST: Nanos = 1_000_000;

/// How a poll ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    /// What it waited for came.
    Ready,
    /// An interrupt waits for the CPU: the stub takes it and the poll goes
    /// on.
    Interrupted,
    /// The deadline passed first.
    Expired,
    /// The run the service is made in came to its end first: the stub asks
    /// again as the machine runs on, and the poll goes on.
    Yielded,
}

pub struct Guest<'a> {
    machine: &'a mut Machine,
    /// Where the run the service is made in ends: no poll passes it, so no
    /// service keeps the machine from its owner however long it waits.
    until: Nanos,
}

impl<'a> Guest<'a> {
    /// The guest for a service made outside any run.
    pub fn new(machine: &'a mut Machine) -> Self {
        Self::within(machine, Nanos::MAX)
    }

    /// The guest for a service made in a run that ends at `until`.
    pub fn within(machine: &'a mut Machine, until: Nanos) -> Self {
        Self { machine, until }
    }

    fn at(&self) -> u64 {
        self.machine.cpu().cycles()
    }

    pub fn now(&self) -> Nanos {
        self.machine.now()
    }

    pub fn regs(&self) -> &Regs {
        self.machine.cpu().regs()
    }

    pub fn regs_mut(&mut self) -> &mut Regs {
        self.machine.cpu_mut().regs_mut()
    }

    pub fn r8(&self, reg: Reg8) -> u8 {
        self.regs().get8(reg as u8)
    }

    pub fn set_r8(&mut self, reg: Reg8, value: u8) {
        self.regs_mut().set8(reg as u8, value);
    }

    pub fn r16(&self, reg: Reg16) -> u16 {
        self.regs().get16(reg as u8)
    }

    pub fn set_r16(&mut self, reg: Reg16, value: u16) {
        self.regs_mut().set16(reg as u8, value);
    }

    pub fn seg(&self, seg: Seg) -> u16 {
        self.regs().sreg(seg)
    }

    pub fn read8(&mut self, segment: u16, offset: u16) -> u8 {
        let at = self.at();
        self.machine.board_mut().read(linear(segment, offset), at)
    }

    pub fn write8(&mut self, segment: u16, offset: u16, value: u8) {
        let at = self.at();
        self.machine
            .board_mut()
            .write(linear(segment, offset), value, at);
    }

    /// A word, its high byte wrapping within the segment as the CPU's does.
    pub fn read16(&mut self, segment: u16, offset: u16) -> u16 {
        let low = self.read8(segment, offset);
        crate::bits::word(low, self.read8(segment, offset.wrapping_add(1)))
    }

    pub fn write16(&mut self, segment: u16, offset: u16, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.write8(segment, offset, low);
        self.write8(segment, offset.wrapping_add(1), high);
    }

    pub fn input(&mut self, port: u16) -> u8 {
        let at = self.at();
        self.machine.board_mut().input(port, at)
    }

    pub fn output(&mut self, port: u16, value: u8) {
        let at = self.at();
        self.machine.board_mut().output(port, value, at);
    }

    /// A word to a 16-bit port, as the 80186's control block takes it.
    pub fn output_word(&mut self, port: u16, value: u16) {
        let at = self.at();
        self.machine.board_mut().output_word(port, value, at);
    }

    /// Lets `span` of emulated time pass inside the service, as firmware
    /// polling a controller spends it; the devices run on meanwhile.
    pub fn spend(&mut self, span: Nanos) {
        let t = self.now() + span;
        let cycles = self.machine.board().cycles_at(t);
        self.machine.cpu_mut().idle_until(cycles);
        self.machine.board_mut().advance_to(t);
    }

    /// Reads a device until `ready` holds, as firmware polling it with
    /// interrupts enabled does: stopping at `deadline`, as soon as an
    /// interrupt waits, or where the run ends. Its reads come further apart
    /// the longer it waits, and never pass the next instant a device could
    /// interrupt.
    pub fn poll(&mut self, deadline: Nanos, mut ready: impl FnMut(&mut Self) -> bool) -> Poll {
        let mut gap = POLL_FIRST;
        loop {
            if ready(self) {
                return Poll::Ready;
            }
            let now = self.now();
            if now >= deadline {
                return Poll::Expired;
            }
            if self.machine.board().intr() {
                return Poll::Interrupted;
            }
            if now >= self.until {
                return Poll::Yielded;
            }
            let mut until = (now + gap).min(deadline).min(self.until);
            if let Some(event) = self.machine.board_mut().next_event() {
                until = until.min(event.max(now + 1));
            }
            self.spend(until - now);
            gap = (gap * 2).min(POLL_LONGEST);
        }
    }

    pub fn bda8(&mut self, offset: u16) -> u8 {
        self.read8(bda::SEGMENT, offset)
    }

    pub fn set_bda8(&mut self, offset: u16, value: u8) {
        self.write8(bda::SEGMENT, offset, value);
    }

    /// Where the host's pointer stands while the emulator's own mouse driver
    /// follows it.
    pub fn host_pointer(&self) -> Option<HostPointer> {
        self.machine.board().host_pointer()
    }

    pub fn bda16(&mut self, offset: u16) -> u16 {
        self.read16(bda::SEGMENT, offset)
    }

    pub fn set_bda16(&mut self, offset: u16, value: u16) {
        self.write16(bda::SEGMENT, offset, value);
    }

    /// A byte of the state the stub reserved at the top of the stack.
    pub fn local(&mut self, index: u16) -> u8 {
        let (ss, sp) = (self.seg(Seg::Ss), self.r16(Reg16::Sp));
        self.read8(ss, sp.wrapping_add(index))
    }

    pub fn set_local(&mut self, index: u16, value: u8) {
        let (ss, sp) = (self.seg(Seg::Ss), self.r16(Reg16::Sp));
        self.write8(ss, sp.wrapping_add(index), value);
    }

    /// Sets or clears `mask` in the FLAGS the stub's IRET restores, `frame`
    /// bytes above the stack pointer.
    pub fn set_returned_flag(&mut self, frame: u16, mask: u16, on: bool) {
        let (ss, sp) = (self.seg(Seg::Ss), self.r16(Reg16::Sp));
        let at = sp.wrapping_add(frame);
        let flags = self.read16(ss, at);
        self.write16(ss, at, if on { flags | mask } else { flags & !mask });
    }

    /// Tells the stub whether the service is done (ZF set) or waits for an
    /// interrupt (ZF clear).
    pub fn done(&mut self, done: bool) {
        let regs = self.regs_mut();
        regs.set_flag(flag::ZF, done);
        regs.set_flag(flag::CF, false);
    }

    /// Tells the stub the run ended before the service did: it asks again
    /// as the machine runs on, with no interrupt waited for (ZF and CF
    /// both).
    pub fn resume(&mut self) {
        let regs = self.regs_mut();
        regs.set_flag(flag::ZF, false);
        regs.set_flag(flag::CF, true);
    }

    /// Continues the CPU at `segment:offset`.
    pub fn jump(&mut self, segment: u16, offset: u16) {
        let regs = self.regs_mut();
        regs.set_sreg(Seg::Cs, segment);
        regs.ip = offset;
    }

    /// Pushes `value` on the CPU's stack.
    pub fn push(&mut self, value: u16) {
        let sp = self.r16(Reg16::Sp).wrapping_sub(2);
        self.set_r16(Reg16::Sp, sp);
        let ss = self.seg(Seg::Ss);
        self.write16(ss, sp, value);
    }

    /// Calls `target` as a far CALL from `back` would: its RETF continues at
    /// `back`.
    pub fn far_call(&mut self, target: (u16, u16), back: (u16, u16)) {
        self.push(back.0);
        self.push(back.1);
        self.jump(target.0, target.1);
    }

    /// Takes INT `vector` as the instruction at `back` would: the handler's
    /// IRET continues at `back`.
    pub fn interrupt(&mut self, vector: u8, back: (u16, u16)) {
        self.machine.enters_interrupt(vector);
        let at = u16::from(vector) * 4;
        let (offset, segment) = (self.read16(0, at), self.read16(0, at + 2));
        self.interrupt_at((segment, offset), back);
    }

    /// Takes an interrupt to `target` as INT takes one through a vector:
    /// the flags and `back` pushed, IF and TF cleared.
    pub fn interrupt_at(&mut self, target: (u16, u16), back: (u16, u16)) {
        let flags = self.regs().flags;
        self.push(flags);
        self.push(back.0);
        self.push(back.1);
        self.regs_mut().set_flag(flag::IF | flag::TF, false);
        self.jump(target.0, target.1);
    }

    /// Sets SS:SP.
    pub fn set_stack(&mut self, segment: u16, pointer: u16) {
        self.regs_mut().set_sreg(Seg::Ss, segment);
        self.set_r16(Reg16::Sp, pointer);
    }

    /// `out.len()` bytes from `segment:offset`, wrapping within the segment.
    pub fn read_bytes(&mut self, segment: u16, offset: u16, out: &mut [u8]) {
        let mut at = offset;
        for byte in out {
            *byte = self.read8(segment, at);
            at = at.wrapping_add(1);
        }
    }

    /// Writes `data` at `segment:offset`, wrapping within the segment.
    pub fn write_bytes(&mut self, segment: u16, offset: u16, data: &[u8]) {
        let mut at = offset;
        for &byte in data {
            self.write8(segment, at, byte);
            at = at.wrapping_add(1);
        }
    }

    pub fn machine(&mut self) -> &mut Machine {
        self.machine
    }
}
