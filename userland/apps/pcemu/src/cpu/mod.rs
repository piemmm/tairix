//! The processor: decode, execute, interrupts and timing.
//!
//! [`Cpu::step`] runs one unit — a prefix byte or an instruction — so every
//! call does bounded work however hostile the code. A repeated string
//! instruction yields between iterations to a pending interrupt or the
//! caller's cycle limit and resumes exactly where it stopped.

pub mod bus;
pub mod decode;
pub mod jit;
pub mod model;
pub mod regs;

mod alu;
mod exec;
mod i8080;
mod nec;
mod queue;
mod timing;

pub use bus::{Bus, Cause, OPEN_BUS};
pub use decode::Insn;
pub use model::{CpuModel, Family};
pub use regs::{flag, gpr, Regs, Seg};

use crate::snapshot::{restore_option, save_fitted, state, state_enum, In, Out, State, Torn};
use decode::{decode, Fetch, Op, Prefix, Rep};
use queue::Queue;

/// Why [`Cpu::run`] or [`Cpu::step`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The cycle limit was reached.
    Limit,
    /// The bus reported a change to the device schedule the limit was
    /// chosen from.
    Rescheduled,
    /// The CPU is halted with no interrupt it can take.
    Halted,
    /// The emulator's ROM made host call `n`; the CPU resumes after it.
    HostCall(u16),
    /// The bus stopped the CPU before an instruction ([`Bus::begins`]).
    Break,
}

/// The physical address `segment:offset` names; the 20-bit bus wraps.
#[must_use]
pub fn linear(segment: u16, offset: u16) -> u32 {
    ((u32::from(segment) << 4) + u32::from(offset)) & 0xF_FFFF
}

/// The prefixes gathered ahead of an instruction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Prefixes {
    segment: Option<Seg>,
    rep: Option<Rep>,
    lock: bool,
    count: u8,
    /// IP of the first prefix, where an exact restart re-decodes them all.
    first_ip: u16,
    /// IP of the last prefix, where the 8086 resumes an interrupted string
    /// instruction.
    last_ip: u16,
}

/// What an instruction asks of the step that ran it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    /// Continue with the next instruction in sequence.
    Next,
    /// Control transferred: the queue is empty.
    Branch,
    /// Enter interrupt `n` with the return address after the instruction.
    Trap(u8),
    /// Enter interrupt `n` with the return address at the instruction.
    Fault(u8),
    /// HLT.
    Halt,
    /// The instruction was a host call.
    Host(u16),
    /// A repeated string instruction yielded; IP is already where it resumes.
    Yield,
    /// WAIT found TEST high: the CPU idles until it reads low.
    Wait,
}

/// A WAIT idling until TEST reads low.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Wait {
    /// The cycle it began looking at TEST.
    began: u64,
    /// The WAIT's own address, where an interrupt taken meanwhile returns.
    ip: u16,
}

/// The bus interface unit's prefetch, kept for its timing: an instruction
/// waits until its bytes are fetched, and the queue fills one bus cycle
/// (four clocks) at a time while the execution unit leaves the bus idle.
#[derive(Clone, Copy, Debug, Default)]
struct Biu {
    /// Bytes fetched ahead and not yet consumed.
    fill: u8,
    /// Idle clocks already spent toward the next fetch.
    credit: u32,
    /// Bus cycles the execution unit has used in the current unit.
    bus_cycles: u32,
}

/// The NEC V-series execution mode, and whether its MD flag is unlocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Native mode with MD locked: no IRET or POPF can enter 8080 mode.
    Native,
    /// Native mode between BRKEM and RETEM, as in a handler CALLN or an
    /// interrupt entered from 8080 mode: an IRET that pops a clear MD
    /// returns to 8080 mode.
    NativeUnlocked,
    /// 8080 emulation (MD clear).
    Emulation,
}

/// Clocks one bus cycle takes.
pub(crate) const BUS_CYCLE: u32 = 4;

/// A modelled processor.
#[derive(Clone, Debug)]
pub struct Cpu {
    regs: Regs,
    model: CpuModel,
    cycles: u64,
    halted: bool,
    prefixes: Prefixes,
    inhibit: bool,
    queue: Queue,
    /// The 8086's last effective address, which a register-operand LEA
    /// exposes.
    ind: u16,
    /// The 8086's last operand transferred, which a register-operand LDS or
    /// LES exposes.
    opr: u16,
    mode: Mode,
    biu: Biu,
    /// The WAIT the CPU is idling in, if any.
    waiting: Option<Wait>,
    /// A repeated string instruction paused at the caller's cycle limit,
    /// which the program cannot see: where an interrupt taken before it
    /// resumes returns, as one taken inside it would.
    paused: Option<u16>,
}

struct CpuFetch<'a, B: Bus> {
    cpu: &'a mut Cpu,
    bus: &'a mut B,
}

impl<B: Bus> Fetch for CpuFetch<'_, B> {
    fn fetch(&mut self) -> u8 {
        let addr = linear(self.cpu.regs.sreg(Seg::Cs), self.cpu.regs.ip);
        self.cpu.regs.ip = self.cpu.regs.ip.wrapping_add(1);
        match self.cpu.queue.take(addr) {
            Some(byte) => byte,
            None => self.bus.fetch(addr, self.cpu.cycles),
        }
    }
}

impl Cpu {
    /// A processor in its reset state.
    #[must_use]
    pub fn new(model: CpuModel) -> Self {
        let mut cpu = Self {
            regs: Regs::default(),
            model,
            cycles: 0,
            halted: false,
            prefixes: Prefixes::default(),
            inhibit: false,
            queue: Queue::new(model.queue_len()),
            ind: 0,
            opr: 0,
            mode: Mode::Native,
            biu: Biu::default(),
            waiting: None,
            paused: None,
        };
        cpu.reset();
        cpu
    }

    /// Asserts RESET: CS:IP to `FFFF:0000`, every other register and the
    /// queue cleared, interrupts disabled. The cycle count runs on.
    pub fn reset(&mut self) {
        self.regs = Regs {
            flags: self.normalize_flags(0, false),
            ..Regs::default()
        };
        self.regs.set_sreg(Seg::Cs, 0xFFFF);
        self.halted = false;
        self.prefixes = Prefixes::default();
        self.inhibit = false;
        self.queue.flush();
        self.mode = Mode::Native;
        self.biu = Biu::default();
        self.waiting = None;
        self.paused = None;
    }

    /// The model this CPU is.
    #[must_use]
    pub const fn model(&self) -> CpuModel {
        self.model
    }

    /// The registers.
    #[must_use]
    pub const fn regs(&self) -> &Regs {
        &self.regs
    }

    /// The registers, to change. A change to CS or IP empties the queue.
    pub fn regs_mut(&mut self) -> &mut Regs {
        self.queue.flush();
        &mut self.regs
    }

    /// Cycles executed since construction.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Whether the CPU is halted.
    #[must_use]
    pub const fn halted(&self) -> bool {
        self.halted
    }

    /// Whether the CPU is idling in a WAIT for TEST to read low.
    #[must_use]
    pub const fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// Whether a repeated string instruction paused at a cycle limit resumes
    /// at the next step: the boundary it is at begins no instruction.
    #[must_use]
    pub const fn resumes_string(&self) -> bool {
        self.paused.is_some()
    }

    /// Whether the NEC CPU is in 8080 emulation mode.
    #[must_use]
    pub fn in_8080_mode(&self) -> bool {
        self.mode == Mode::Emulation
    }

    /// Advances the cycle count to `cycles` without executing, as time passes
    /// while the CPU is halted.
    pub fn idle_until(&mut self, cycles: u64) {
        self.cycles = self.cycles.max(cycles);
    }

    /// Runs until the cycle count reaches `limit`, something stops the CPU,
    /// or the bus reports its schedule changed.
    pub fn run<B: Bus>(&mut self, bus: &mut B, limit: u64) -> Stop {
        while self.cycles < limit {
            if let Some(stop) = self.step(bus, limit) {
                return stop;
            }
            if bus.schedule_changed() {
                return Stop::Rescheduled;
            }
        }
        Stop::Limit
    }

    /// Runs one unit: services a pending interrupt or executes one prefix or
    /// instruction. `limit` bounds a repeated string instruction. A CPU idle
    /// in a WAIT, as one halted, answers [`Stop::Halted`] until it may go on.
    pub fn step<B: Bus>(&mut self, bus: &mut B, limit: u64) -> Option<Stop> {
        if self.prefixes.count == 0 {
            if self.inhibit {
                self.inhibit = false;
            } else {
                self.service_interrupts(bus);
            }
        }
        if let Some(wait) = self.waiting {
            return (self.wait(bus, wait) == Flow::Wait).then_some(Stop::Halted);
        }
        if self.halted {
            // The V-series leaves HLT on INTR even with interrupts disabled,
            // without taking the interrupt.
            if bus.intr() && matches!(self.model.family(), Family::Nec) {
                self.halted = false;
            } else {
                return Some(Stop::Halted);
            }
        }
        if self.prefixes.count == 0 && bus.begins(self) {
            // This boundary's interrupts are taken: the step that resumes
            // must not take them again.
            self.inhibit = true;
            return Some(Stop::Break);
        }
        if self.mode == Mode::Emulation {
            return self.step_8080(bus);
        }
        self.paused = None;
        let ip = self.regs.ip;
        let host = bus.host_code(linear(self.regs.sreg(Seg::Cs), ip));
        let model = self.model;
        let insn = decode(model, host, &mut CpuFetch { cpu: self, bus });
        let stall = self.await_bytes(insn.head);
        self.tick(stall);
        if let Op::Prefix(prefix) = insn.op {
            self.add_prefix(prefix, ip);
            self.tick(2);
            self.prefetch_during(2);
            return None;
        }
        let began = self.cycles;
        let start = if self.prefixes.count == 0 {
            ip
        } else {
            self.prefixes.first_ip
        };
        self.queue
            .set_window(u32::from(self.regs.sreg(Seg::Cs)) << 4, self.regs.ip);
        let trap = self.regs.flag(flag::TF);
        let flow = self.execute(bus, &insn, ip, limit);
        let waits = bus.take_wait_states(self.cycles);
        self.tick(waits);
        let spent = u32::try_from(self.cycles - began).unwrap_or(u32::MAX);
        let late = self.overlap_operands(insn.len - insn.head, spent);
        self.tick(late);
        self.prefetch_during(spent + late);
        self.prefixes = Prefixes::default();
        if !matches!(flow, Flow::Next | Flow::Wait) {
            // A transfer's published cost includes the first fetch at its
            // target.
            self.biu.fill = if matches!(flow, Flow::Branch | Flow::Trap(_) | Flow::Fault(_)) {
                self.fetch_width()
            } else {
                0
            };
            self.biu.credit = 0;
        }
        match flow {
            Flow::Next => {}
            Flow::Branch | Flow::Yield => self.queue.flush(),
            Flow::Trap(vector) => self.interrupt(bus, vector),
            Flow::Fault(vector) => {
                self.regs.ip = start;
                self.interrupt(bus, vector);
            }
            Flow::Halt => {
                self.halted = true;
                return Some(Stop::Halted);
            }
            Flow::Wait => return Some(Stop::Halted),
            Flow::Host(id) => {
                self.queue.flush();
                return Some(Stop::HostCall(id));
            }
        }
        if trap && !self.inhibit && !matches!(flow, Flow::Fault(_)) {
            self.tick(timing::trap_entry(self.model));
            self.interrupt(bus, 1);
        }
        None
    }

    fn add_prefix(&mut self, prefix: Prefix, ip: u16) {
        let p = &mut self.prefixes;
        if p.count == 0 {
            p.first_ip = ip;
        }
        p.last_ip = ip;
        p.count = p.count.saturating_add(1);
        match prefix {
            Prefix::Segment(seg) => p.segment = Some(seg),
            Prefix::Rep(rep) => p.rep = Some(rep),
            Prefix::Lock => p.lock = true,
        }
    }

    fn service_interrupts<B: Bus>(&mut self, bus: &mut B) {
        if bus.nmi() {
            bus.acknowledge_nmi();
            self.interrupted();
            self.tick(timing::nmi_entry(self.model));
            self.enter(bus, 2, Cause::External);
        } else if self.regs.flag(flag::IF) && bus.intr() {
            let vector = bus.acknowledge();
            self.interrupted();
            self.tick(timing::intr_entry(self.model));
            self.enter(bus, vector, Cause::External);
        }
    }

    /// An interrupt ends a HLT, returns to a WAIT, and returns to a paused
    /// string instruction where one taken inside it would.
    fn interrupted(&mut self) {
        self.halted = false;
        if let Some(wait) = self.waiting.take() {
            self.regs.ip = wait.ip;
            self.queue.flush();
        }
        if let Some(ip) = self.paused.take() {
            self.regs.ip = ip;
        }
    }

    /// Enters interrupt `vector` as an instruction does: pushes FLAGS, CS and
    /// IP, clears IF and TF, and loads CS:IP from the vector table.
    pub fn interrupt<B: Bus>(&mut self, bus: &mut B, vector: u8) {
        self.enter(bus, vector, Cause::Program);
    }

    /// Enters interrupt `vector`, the bus told of it and why first.
    fn enter<B: Bus>(&mut self, bus: &mut B, vector: u8, cause: Cause) {
        bus.interrupts(vector, self, cause);
        let mut flags = self.regs.flags;
        if self.mode == Mode::Emulation {
            flags &= !flag::MD;
            self.mode = Mode::NativeUnlocked;
        }
        self.push(bus, flags);
        self.regs.flags &= !(flag::IF | flag::TF);
        self.regs.flags |= self.native_md();
        self.push(bus, self.regs.sreg(Seg::Cs));
        self.push(bus, self.regs.ip);
        let slot = u16::from(vector) * 4;
        let ip = self.read16_abs(bus, 0, slot);
        let cs = self.read16_abs(bus, 0, slot.wrapping_add(2));
        self.regs.ip = ip;
        self.regs.set_sreg(Seg::Cs, cs);
        self.queue.flush();
        self.biu.fill = self.fetch_width();
        self.biu.credit = 0;
    }

    /// The MD bit a native-mode FLAGS word carries.
    const fn native_md(&self) -> u16 {
        if matches!(self.model.family(), Family::Nec) {
            flag::MD
        } else {
            0
        }
    }

    /// FLAGS as the model stores `raw`: reserved bits forced, MD kept in
    /// native mode unless `restore_md` allows a popped clear MD to take.
    fn normalize_flags(&self, raw: u16, restore_md: bool) -> u16 {
        let base = (raw & flag::WRITABLE) | flag::RESERVED_ONE | flag::HIGH_ONES;
        if matches!(self.model.family(), Family::Nec) && restore_md && raw & flag::MD == 0 {
            base & !flag::MD
        } else {
            base
        }
    }

    fn tick(&mut self, cycles: u32) {
        self.cycles += u64::from(cycles);
    }

    /// Bytes one bus cycle fetches.
    const fn fetch_width(&self) -> u8 {
        if self.model.bus8() {
            1
        } else {
            2
        }
    }

    /// Takes `needed` instruction bytes from the queue, returning the clocks
    /// spent waiting for the ones not yet fetched.
    fn await_bytes(&mut self, needed: u8) -> u32 {
        if self.biu.fill >= needed {
            self.biu.fill -= needed;
            return 0;
        }
        let missing = needed - self.biu.fill;
        let width = self.fetch_width();
        let fetches = missing.div_ceil(width);
        let stall = (u32::from(fetches) * BUS_CYCLE).saturating_sub(self.biu.credit);
        self.biu.fill = fetches * width - missing;
        self.biu.credit = 0;
        stall
    }

    /// Takes an instruction's trailing operand bytes, which the BIU fetches
    /// while execution begins: returns the clocks execution then waits for
    /// those it could not fetch in `clocks` of its idle bus time.
    fn overlap_operands(&mut self, operands: u8, clocks: u32) -> u32 {
        if self.biu.fill >= operands {
            self.biu.fill -= operands;
            return 0;
        }
        let missing = operands - self.biu.fill;
        let width = self.fetch_width();
        let fetches = missing.div_ceil(width);
        let needed = u32::from(fetches) * BUS_CYCLE;
        let idle = clocks.saturating_sub(self.biu.bus_cycles * BUS_CYCLE) + self.biu.credit;
        self.biu.fill = fetches * width - missing;
        self.biu.credit = 0;
        self.biu.bus_cycles += u32::from(fetches);
        needed.saturating_sub(idle)
    }

    /// Lets the queue fill during `clocks` of execution, in the cycles the
    /// execution unit left the bus idle.
    fn prefetch_during(&mut self, clocks: u32) {
        let busy = self.biu.bus_cycles * BUS_CYCLE;
        self.biu.bus_cycles = 0;
        let len = self.queue.len();
        if self.biu.fill >= len {
            self.biu.credit = 0;
            return;
        }
        let idle = clocks.saturating_sub(busy) + self.biu.credit;
        let width = u32::from(self.fetch_width());
        let room = u32::from(len - self.biu.fill);
        let fetches = (idle / BUS_CYCLE).min(room.div_ceil(width));
        let filled = (u32::from(self.biu.fill) + fetches * width).min(u32::from(len));
        self.biu.fill = u8::try_from(filled).unwrap_or(len);
        self.biu.credit = if self.biu.fill >= len {
            0
        } else {
            idle - fetches * BUS_CYCLE
        };
    }

    /// Counts the bus cycles a transfer of `width` bytes at `offset` takes.
    fn bus_transfer(&mut self, word: bool, offset: u16) {
        let split = word && (self.model.bus8() || offset & 1 != 0);
        self.biu.bus_cycles += if split { 2 } else { 1 };
    }

    fn word_penalty(&mut self, offset: u16) {
        if self.model.bus8() || offset & 1 != 0 {
            self.tick(4);
        }
    }

    fn read8_abs<B: Bus>(&mut self, bus: &mut B, segment: u16, offset: u16) -> u8 {
        self.bus_transfer(false, offset);
        bus.read(linear(segment, offset), self.cycles)
    }

    fn read16_abs<B: Bus>(&mut self, bus: &mut B, segment: u16, offset: u16) -> u16 {
        self.word_penalty(offset);
        self.bus_transfer(true, offset);
        let lo = bus.read(linear(segment, offset), self.cycles);
        let hi = bus.read(linear(segment, offset.wrapping_add(1)), self.cycles);
        crate::bits::word(lo, hi)
    }

    fn store_byte<B: Bus>(&mut self, bus: &mut B, segment: u16, offset: u16, value: u8) {
        let addr = linear(segment, offset);
        if self.queue.covers(addr) {
            self.queue.hold(addr, bus.peek(addr));
        }
        bus.write(addr, value, self.cycles);
    }

    fn write8_abs<B: Bus>(&mut self, bus: &mut B, segment: u16, offset: u16, value: u8) {
        self.bus_transfer(false, offset);
        self.store_byte(bus, segment, offset, value);
    }

    fn write16_abs<B: Bus>(&mut self, bus: &mut B, segment: u16, offset: u16, value: u16) {
        self.word_penalty(offset);
        self.bus_transfer(true, offset);
        self.store_byte(bus, segment, offset, crate::bits::lo(value));
        self.store_byte(bus, segment, offset.wrapping_add(1), crate::bits::hi(value));
    }

    fn read8<B: Bus>(&mut self, bus: &mut B, seg: Seg, offset: u16) -> u8 {
        self.read8_abs(bus, self.regs.sreg(seg), offset)
    }

    fn read16<B: Bus>(&mut self, bus: &mut B, seg: Seg, offset: u16) -> u16 {
        self.read16_abs(bus, self.regs.sreg(seg), offset)
    }

    fn write8<B: Bus>(&mut self, bus: &mut B, seg: Seg, offset: u16, value: u8) {
        self.write8_abs(bus, self.regs.sreg(seg), offset, value);
    }

    fn write16<B: Bus>(&mut self, bus: &mut B, seg: Seg, offset: u16, value: u16) {
        self.write16_abs(bus, self.regs.sreg(seg), offset, value);
    }

    fn push<B: Bus>(&mut self, bus: &mut B, value: u16) {
        let sp = self.regs.sp().wrapping_sub(2);
        self.regs.gpr[gpr::SP] = sp;
        self.write16(bus, Seg::Ss, sp, value);
    }

    fn pop<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let sp = self.regs.sp();
        let value = self.read16(bus, Seg::Ss, sp);
        self.regs.gpr[gpr::SP] = sp.wrapping_add(2);
        value
    }
}

impl State for Prefixes {
    fn save(&self, out: &mut Out) {
        let Self {
            segment,
            rep,
            lock,
            count,
            first_ip,
            last_ip,
        } = self;
        save_fitted(segment.as_ref(), out);
        save_fitted(rep.as_ref(), out);
        lock.save(out);
        count.save(out);
        first_ip.save(out);
        last_ip.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let Self {
            segment,
            rep,
            lock,
            count,
            first_ip,
            last_ip,
        } = self;
        restore_option(segment, || Seg::Es, input)?;
        restore_option(rep, || Rep::Z, input)?;
        lock.restore(input)?;
        count.restore(input)?;
        first_ip.restore(input)?;
        last_ip.restore(input)
    }
}

state!(Biu {
    fill,
    credit,
    bus_cycles
});

state_enum!(Mode {
    0 => Native,
    1 => NativeUnlocked,
    2 => Emulation,
});

state!(Wait { began, ip });

state!(
    Cpu { regs, cycles, halted, prefixes, inhibit, queue, ind, opr, mode, biu, waiting, paused }
    as built { model }
    check |cpu: &Cpu| {
        // A queue no fuller than it is long, a BIU whose idle credit is short
        // of a bus cycle and whose count cannot overflow when clocked, and an
        // 8080 mode only a NEC part has.
        cpu.biu.fill <= cpu.queue.len()
            && cpu.biu.credit < BUS_CYCLE
            && u16::try_from(cpu.biu.bus_cycles).is_ok()
            && (cpu.mode == Mode::Native || cpu.model.family() == Family::Nec)
    }
);

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "behaviour_tests.rs"]
mod behaviour_tests;
