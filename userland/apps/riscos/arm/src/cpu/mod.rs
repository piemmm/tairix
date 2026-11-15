//! The processor: its registers and banks, its step through memory, and the
//! exceptions it takes.
//!
//! A step takes a pending interrupt, fetches the next word through the bus,
//! finds its decoding in the page's memo and executes it. A `SWI`, a host
//! call, a wait and anything unmodelled stop the run so the session can
//! answer; every other exception is taken architecturally, through the
//! profile's vectors.

mod alu;
mod execute;
mod memo;
mod memory;

use crate::bus::Bus;
use crate::cp15::{Cp15, Fault};
use crate::isa::Feature;
use crate::profile::{Profile, Unpredictable};
use crate::psr::{self, Bank, Mode};
use crate::reg::Reg;

use memo::{Decoded, Memo};

/// An exception.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Exception {
    /// Reset.
    Reset,
    /// An undefined instruction.
    Undefined,
    /// A software interrupt.
    Swi,
    /// An instruction fetch the bus refused, or a breakpoint.
    PrefetchAbort,
    /// A data access the bus refused, or a misaligned one.
    DataAbort,
    /// An interrupt.
    Irq,
    /// A fast interrupt.
    Fiq,
}

impl Exception {
    /// The vector's offset from the profile's base.
    #[must_use]
    pub const fn offset(self) -> u32 {
        match self {
            Self::Reset => 0x00,
            Self::Undefined => 0x04,
            Self::Swi => 0x08,
            Self::PrefetchAbort => 0x0C,
            Self::DataAbort => 0x10,
            Self::Irq => 0x18,
            Self::Fiq => 0x1C,
        }
    }

    /// The mode the exception enters. Every profile here takes exceptions
    /// in the 32-bit modes, as the SA-110 does.
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self {
            Self::Reset | Self::Swi => Mode::Supervisor,
            Self::Undefined => Mode::Undefined,
            Self::PrefetchAbort | Self::DataAbort => Mode::Abort,
            Self::Irq => Mode::Irq,
            Self::Fiq => Mode::Fiq,
        }
    }
}

/// Something the processor does not model, named so the front end can say
/// what the program reached.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Unmodelled {
    /// A system control register: CP15's `opc1, CRn, CRm, opc2`.
    System {
        /// The register.
        register: [u8; 4],
        /// Whether the access was a write.
        write: bool,
    },
    /// Thumb state, which RO10 brings.
    Thumb,
}

/// Why a run stopped.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Stop {
    /// The instruction budget is spent.
    Budget,
    /// A `SWI` whose condition passed. The PC is past it; the session
    /// performs it, or enters the vector with [`Cpu::enter`].
    Swi {
        /// Its comment field.
        number: u32,
    },
    /// A `UDF` on a host page. The PC is past it.
    HostCall {
        /// Its immediate: the host routine.
        number: u32,
    },
    /// A wait for an interrupt with none pending. The PC is past it.
    WaitForInterrupt,
    /// A wait for an event with none signalled. The PC is past it.
    WaitForEvent,
    /// The instruction at the PC reached something this processor does not
    /// model; it has not executed.
    Unmodelled(Unmodelled),
}

/// A run's outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    /// Why it stopped.
    pub stop: Stop,
    /// Instructions executed, condition failed or passed, the stopping one
    /// included when it executed.
    pub executed: u64,
}

/// The processor's registers as the hardware holds them: every bank, the
/// PSRs and the address of the next instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct State {
    /// R0 to R14 as User and System mode see them.
    pub user: [u32; 15],
    /// FIQ mode's R8 to R14.
    pub fiq: [u32; 7],
    /// R13 and R14 of IRQ, Supervisor, Abort and Undefined mode.
    pub banked: [[u32; 2]; 4],
    /// The address of the next instruction.
    pub pc: u32,
    /// The CPSR.
    pub cpsr: u32,
    /// The SPSRs of FIQ, IRQ, Supervisor, Abort and Undefined mode.
    pub spsr: [u32; 5],
}

/// Why a [`State`] was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BadState {
    /// The CPSR names a mode the profile does not have.
    Mode,
}

/// The two interrupt lines, held at their CPSR mask bits' positions.
const LINES: u32 = psr::I | psr::F;

/// An ARM processor.
pub struct Cpu {
    profile: &'static Profile,
    /// R0 to R14 as the current mode sees them.
    r: [u32; 15],
    /// The address of the instruction executing, or next to.
    pc: u32,
    /// Where execution continues after the instruction executing.
    next: u32,
    cpsr: u32,
    mode: Mode,
    spsr: [u32; 5],
    /// Each bank's R13 and R14 while another bank is current.
    stashed: [[u32; 2]; 6],
    /// The R8 to R12 not current: User's in FIQ mode, FIQ's otherwise.
    stashed_high: [u32; 5],
    /// The interrupt lines asserted, at the I and F bits' positions.
    lines: u32,
    /// The event register `WFE` waits on.
    event: bool,
    /// The address an exclusive load marked, if any.
    exclusive: Option<u32>,
    cp15: Cp15,
    memo: Memo,
}

impl Cpu {
    /// A processor of `profile` at reset, keeping at most `memo_pages`
    /// pages of decoded instructions.
    #[must_use]
    pub fn new(profile: &'static Profile, memo_pages: usize) -> Self {
        let mut cpu = Self {
            profile,
            r: [0; 15],
            pc: 0,
            next: 0,
            cpsr: Mode::Supervisor.bits() | psr::I | psr::F,
            mode: Mode::Supervisor,
            spsr: [0; 5],
            stashed: [[0; 2]; 6],
            stashed_high: [0; 5],
            lines: 0,
            event: false,
            exclusive: None,
            cp15: Cp15::new(profile.system),
            memo: Memo::new(memo_pages, profile),
        };
        cpu.reset();
        cpu
    }

    /// Resets the processor: Supervisor mode, interrupts masked, at the
    /// reset vector. Registers other than the PC and CPSR keep their
    /// values, as on hardware, and the interrupt lines stay as the devices
    /// driving them hold them.
    pub fn reset(&mut self) {
        self.cp15 = Cp15::new(self.profile.system);
        self.take(Exception::Reset, self.pc);
        self.pc = self.next;
        self.event = false;
    }

    /// The profile.
    #[must_use]
    pub const fn profile(&self) -> &'static Profile {
        self.profile
    }

    /// Runs at most `budget` instructions.
    pub fn run<B: Bus>(&mut self, bus: &mut B, budget: u64) -> Run {
        let mut executed = 0;
        while executed < budget {
            let outcome = self.step(bus);
            if outcome.executed {
                executed += 1;
            }
            if let Some(stop) = outcome.stop {
                return Run { stop, executed };
            }
        }
        Run {
            stop: Stop::Budget,
            executed,
        }
    }

    fn step<B: Bus>(&mut self, bus: &mut B) -> Step {
        if self.lines & !self.cpsr & LINES != 0 {
            self.interrupt();
        }
        if self.cpsr & psr::T != 0 {
            return Step::stopped(Stop::Unmodelled(Unmodelled::Thumb), false);
        }
        let pc = self.pc;
        self.next = self.advance(pc, 4);
        let fetched = match bus.fetch(pc, self.mode.privileged()) {
            Ok(fetched) => fetched,
            Err(abort) => {
                self.prefetch_abort(Fault::Bus(abort), pc);
                self.pc = self.next;
                return Step::ran();
            }
        };
        let decoded = self.memo.decoded(pc, fetched.word, self.profile);
        let flow = match decoded {
            Decoded::Execute(instruction) if instruction.cond.passes(self.cpsr) => {
                self.execute(bus, &instruction, fetched.host)
            }
            Decoded::Execute(_) => Flow::Next,
            Decoded::Undefined(cond) => {
                let unconditional = cond == crate::cond::Condition::Nv
                    && !self.profile.isa.has(Feature::ConditionNever);
                if unconditional || cond.passes(self.cpsr) {
                    self.undefined();
                }
                Flow::Next
            }
        };
        match flow {
            Flow::Next => {
                self.pc = self.next;
                Step::ran()
            }
            Flow::Stop(stop) => {
                self.pc = self.next;
                Step::stopped(stop, true)
            }
            Flow::Refuse(stop) => Step::stopped(stop, false),
        }
    }

    /// `address` plus `bytes`, within the 26-bit address space in a 26-bit
    /// mode.
    const fn advance(&self, address: u32, bytes: u32) -> u32 {
        let next = address.wrapping_add(bytes);
        if self.mode.is_26bit() {
            next & psr::R15_PC
        } else {
            next
        }
    }

    /// The processor's registers.
    #[must_use]
    pub fn state(&self) -> State {
        let bank = self.mode.bank();
        let mut user = [0; 15];
        user[..8].copy_from_slice(&self.r[..8]);
        let mut fiq = [0; 7];
        if bank == Bank::Fiq {
            user[8..13].copy_from_slice(&self.stashed_high);
            fiq[..5].copy_from_slice(&self.r[8..13]);
        } else {
            user[8..13].copy_from_slice(&self.r[8..13]);
            fiq[..5].copy_from_slice(&self.stashed_high);
        }
        let pair = |b: Bank| -> [u32; 2] {
            if b == bank {
                [self.r[13], self.r[14]]
            } else {
                self.stashed[b.index()]
            }
        };
        [user[13], user[14]] = pair(Bank::User);
        [fiq[5], fiq[6]] = pair(Bank::Fiq);
        State {
            user,
            fiq,
            banked: [
                pair(Bank::Irq),
                pair(Bank::Supervisor),
                pair(Bank::Abort),
                pair(Bank::Undefined),
            ],
            pc: self.pc,
            cpsr: self.cpsr,
            spsr: self.spsr,
        }
    }

    /// Replaces the processor's registers, the PSRs held to the bits the
    /// profile has. The decoded pages are kept.
    pub fn set_state(&mut self, state: &State) -> Result<(), BadState> {
        let mode = self.valid_mode(state.cpsr).ok_or(BadState::Mode)?;
        self.mode = mode;
        self.cpsr = state.cpsr & self.profile.psr;
        self.spsr = state.spsr.map(|spsr| spsr & self.profile.psr);
        self.pc = state.pc;
        let bank = mode.bank();
        self.r[..8].copy_from_slice(&state.user[..8]);
        if bank == Bank::Fiq {
            self.r[8..13].copy_from_slice(&state.fiq[..5]);
            self.stashed_high.copy_from_slice(&state.user[8..13]);
        } else {
            self.r[8..13].copy_from_slice(&state.user[8..13]);
            self.stashed_high.copy_from_slice(&state.fiq[..5]);
        }
        self.stashed[Bank::User.index()] = [state.user[13], state.user[14]];
        self.stashed[Bank::Fiq.index()] = [state.fiq[5], state.fiq[6]];
        for (b, pair) in [Bank::Irq, Bank::Supervisor, Bank::Abort, Bank::Undefined]
            .into_iter()
            .zip(state.banked)
        {
            self.stashed[b.index()] = pair;
        }
        [self.r[13], self.r[14]] = self.stashed[bank.index()];
        self.exclusive = None;
        Ok(())
    }

    /// A register as the current mode sees it; R15 reads as the address of
    /// the next instruction.
    #[must_use]
    pub fn reg(&self, reg: Reg) -> u32 {
        match self.r.get(reg.index()) {
            Some(&value) => value,
            None => self.pc,
        }
    }

    /// Sets a register as the current mode sees it; R15 sets the address of
    /// the next instruction.
    pub fn set_reg(&mut self, reg: Reg, value: u32) {
        match self.r.get_mut(reg.index()) {
            Some(slot) => *slot = value,
            None => self.pc = value,
        }
    }

    /// The address of the next instruction.
    #[must_use]
    pub const fn pc(&self) -> u32 {
        self.pc
    }

    /// The CPSR.
    #[must_use]
    pub const fn cpsr(&self) -> u32 {
        self.cpsr
    }

    /// The current mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Replaces the CPSR, switching banks if its mode changes; refused if
    /// it names a mode the profile does not have.
    pub fn set_cpsr(&mut self, cpsr: u32) -> Result<(), BadState> {
        let mode = self.valid_mode(cpsr).ok_or(BadState::Mode)?;
        self.switch_mode(mode);
        self.cpsr = cpsr & self.profile.psr;
        Ok(())
    }

    /// The current mode's SPSR; User and System mode have none.
    #[must_use]
    pub fn spsr(&self) -> Option<u32> {
        self.mode.bank().spsr().map(|index| self.spsr[index])
    }

    /// Takes `exception`, leaving `return_address` in the exception mode's
    /// R14.
    pub fn enter(&mut self, exception: Exception, return_address: u32) {
        self.take(exception, return_address);
        self.pc = self.next;
    }

    /// Asserts or releases the interrupt line.
    pub fn set_irq(&mut self, asserted: bool) {
        self.set_line(psr::I, asserted);
    }

    /// Asserts or releases the fast interrupt line.
    pub fn set_fiq(&mut self, asserted: bool) {
        self.set_line(psr::F, asserted);
    }

    fn set_line(&mut self, line: u32, asserted: bool) {
        if asserted {
            self.lines |= line;
        } else {
            self.lines &= !line;
        }
    }

    /// Signals an event to a `WFE`.
    pub fn signal_event(&mut self) {
        self.event = true;
    }

    /// Clears the exclusive monitor, as a context switch must.
    pub fn clear_exclusive(&mut self) {
        self.exclusive = None;
    }

    /// Gives back the memory the decoded pages hold; they decode again as
    /// they run.
    pub fn release_memo(&mut self) {
        self.memo.release();
    }

    /// The bytes the decoded pages hold.
    #[must_use]
    pub fn memo_bytes(&self) -> usize {
        self.memo.bytes()
    }

    /// The mode `cpsr` names, if the profile has it.
    fn valid_mode(&self, cpsr: u32) -> Option<Mode> {
        Mode::from_bits(cpsr).filter(|mode| !mode.is_26bit() || self.profile.modes26())
    }

    /// Makes `mode` current, exchanging the banked registers; the CPSR's
    /// mode field is the caller's to write.
    fn switch_mode(&mut self, mode: Mode) {
        let (from, to) = (self.mode.bank(), mode.bank());
        self.mode = mode;
        if from == to {
            return;
        }
        self.stashed[from.index()] = [self.r[13], self.r[14]];
        [self.r[13], self.r[14]] = self.stashed[to.index()];
        if (from == Bank::Fiq) != (to == Bank::Fiq) {
            for (live, stashed) in self.r[8..13].iter_mut().zip(&mut self.stashed_high) {
                core::mem::swap(live, stashed);
            }
        }
    }

    /// Writes the CPSR bits `bytes` selects of `value` that the profile lets
    /// this write change: an `MSR`, or an exception return when `restore`.
    /// A mode the profile does not have leaves the mode as it was.
    fn write_cpsr(&mut self, value: u32, bytes: u32, restore: bool) {
        let writable = if restore {
            self.profile.psr_return
        } else if self.mode.privileged() {
            self.profile.psr_privileged
        } else {
            self.profile.psr_user
        };
        let mut mask = bytes & writable;
        if mask & psr::M != 0 && self.valid_mode(value).is_none() {
            mask &= !psr::M;
        }
        let cpsr = (self.cpsr & !mask) | (value & mask);
        if let Some(mode) = self.valid_mode(cpsr) {
            self.switch_mode(mode);
        }
        self.cpsr = cpsr;
    }

    /// Writes the 26-bit PSR bits R15's layout holds in `r15`: the flags in
    /// User mode, and I, F and the mode too in a privileged one.
    fn write_r15_psr(&mut self, r15: u32) {
        let cpsr = if self.mode.privileged() {
            psr::from_r15(self.cpsr, r15)
        } else {
            (self.cpsr & !psr::NZCV) | (r15 & psr::NZCV)
        };
        if let Some(mode) = self.valid_mode(cpsr) {
            self.switch_mode(mode);
            self.cpsr = cpsr;
        }
    }

    /// The SPSR's bytes `bytes` selects, written from `value`; false in a
    /// mode without one.
    fn write_spsr(&mut self, value: u32, bytes: u32) -> bool {
        let Some(index) = self.mode.bank().spsr() else {
            return false;
        };
        let mask = bytes & self.profile.psr;
        self.spsr[index] = (self.spsr[index] & !mask) | (value & mask);
        true
    }

    /// Restores the CPSR from the SPSR, as an exception return does; false
    /// in a mode without one.
    fn restore_cpsr(&mut self) -> bool {
        match self.spsr() {
            Some(spsr) => {
                self.write_cpsr(spsr, u32::MAX, true);
                true
            }
            None => false,
        }
    }

    fn take(&mut self, exception: Exception, return_address: u32) {
        let old = self.cpsr;
        let mode = exception.mode();
        self.switch_mode(mode);
        if let Some(index) = mode.bank().spsr() {
            self.spsr[index] = old;
        }
        self.r[14] = return_address;
        let mut cpsr = (old & !psr::M) | mode.bits() | psr::I;
        if matches!(exception, Exception::Reset | Exception::Fiq) {
            cpsr |= psr::F;
        }
        if self.profile.isa.has(Feature::V6) {
            if !matches!(exception, Exception::Undefined | Exception::Swi) {
                cpsr |= psr::A;
            }
            cpsr &= !(psr::IT_LOW | psr::IT_HIGH | psr::J | psr::E);
        }
        self.cpsr = cpsr & !psr::T & self.profile.psr;
        self.exclusive = None;
        self.next = self.profile.vectors.wrapping_add(exception.offset());
    }

    /// Takes the pending interrupt the CPSR does not mask, FIQ first.
    fn interrupt(&mut self) {
        let pending = self.lines & !self.cpsr;
        let exception = if pending & psr::F != 0 {
            Exception::Fiq
        } else {
            Exception::Irq
        };
        self.take(exception, self.pc.wrapping_add(4));
        self.pc = self.next;
    }

    fn undefined(&mut self) {
        self.take(Exception::Undefined, self.pc.wrapping_add(4));
    }

    fn prefetch_abort(&mut self, fault: Fault, address: u32) {
        self.cp15.prefetch_fault(fault, address);
        self.take(Exception::PrefetchAbort, address.wrapping_add(4));
    }

    fn data_abort(&mut self, fault: Fault, address: u32, write: bool) {
        self.cp15.data_fault(fault, address, write);
        self.take(Exception::DataAbort, self.pc.wrapping_add(8));
    }

    /// Whether an instruction whose effect the architecture leaves
    /// UNPREDICTABLE in this state is refused, having taken the undefined
    /// instruction exception; otherwise the caller carries on.
    fn refuse(&mut self) -> bool {
        let refused = self.profile.unpredictable == Unpredictable::Undefined;
        if refused {
            self.undefined();
        }
        refused
    }
}

/// What executing an instruction did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flow {
    /// It completed, or took an exception.
    Next,
    /// It completed and stops the run.
    Stop(Stop),
    /// It did not execute: the run stops at it.
    Refuse(Stop),
}

/// What a step did.
struct Step {
    executed: bool,
    stop: Option<Stop>,
}

impl Step {
    const fn ran() -> Self {
        Self {
            executed: true,
            stop: None,
        }
    }

    const fn stopped(stop: Stop, executed: bool) -> Self {
        Self {
            executed,
            stop: Some(stop),
        }
    }
}

#[cfg(test)]
mod tests;
