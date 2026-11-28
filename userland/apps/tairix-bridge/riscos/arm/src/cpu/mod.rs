//! The processor: its registers and banks, its step through memory, and the
//! exceptions it takes.
//!
//! A step takes a pending interrupt, fetches the next instruction through
//! the bus, finds its decoding in the page's memo and executes it: an A32
//! word, or in Thumb state one or two halfwords under the `IT` block's
//! condition. A `SWI`, a host call, a wait and anything unmodelled stop the
//! run so the session can answer; every other exception is taken
//! architecturally, through the profile's vectors.

mod alu;
mod context;
mod execute;
mod fpa;
mod memo;
mod memory;
mod simdfp;

pub use context::Context;

use crate::bus::{Abort, Bus};
use crate::cond::Condition;
use crate::cp15::{Cp15, Fault};
use crate::fpa::Fpa;
use crate::isa::Feature;
use crate::profile::{Profile, Unpredictable};
use crate::psr::{self, Bank, Mode};
use crate::reg::Reg;
use crate::t32::{self, Position};
use crate::vfp::Vfp;
use crate::word::low16;

use memo::{Decoded, Memo, Thumb};

/// R15, the register a transfer to which branches or writes the flags.
const PC: Reg = Reg::PC;

/// The most bytes one of a processor's `memo_pages` holds: a page decoded in
/// each instruction set.
pub const MEMO_PAGE_BYTES: usize = Memo::<u32, 2>::PAGE_BYTES + Memo::<Thumb, 1>::PAGE_BYTES;

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
    /// An FPA instruction met an exception its trap is enabled for, where
    /// FPEmulator raises its error. The PC is past it; it wrote nothing but
    /// the flags of exceptions it raised first and, as FPEmulator's do, a
    /// load's written-back base.
    FloatingPoint {
        /// The exception.
        exception: crate::fpa::Exception,
    },
}

/// A run's outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    /// Why it stopped.
    pub stop: Stop,
    /// Instructions executed, condition failed or passed, the stopping one
    /// included when it executed. A fetch that took a prefetch abort counts
    /// as one, so a run of aborts ends at the budget too.
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
    /// Whether the instruction executing wrote the IT state, by `IT`, an
    /// exception or an exception return, so the state is not advanced past
    /// it.
    it_written: bool,
    cp15: Cp15,
    fpa: Fpa,
    /// Whether the floating point system has a context for the FPA's
    /// instructions to work in.
    fpa_active: bool,
    vfp: Vfp,
    memo: Memo<u32, 2>,
    thumb: Memo<Thumb, 1>,
}

impl Cpu {
    /// A processor of `profile` at reset, keeping at most `memo_pages`
    /// pages of each instruction set's decoded instructions.
    #[must_use]
    pub fn new(profile: &'static Profile, memo_pages: usize) -> Self {
        let thumb_zero = Thumb {
            halfwords: 0,
            position: Position::Outside,
        };
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
            it_written: false,
            cp15: Cp15::new(profile.system, profile.isa),
            fpa: Fpa::new(),
            fpa_active: true,
            vfp: Vfp::new(),
            memo: Memo::new(memo_pages, 0, Decoded::arm(0, profile)),
            thumb: Memo::new(memo_pages, thumb_zero, Decoded::thumb(thumb_zero, profile)),
        };
        cpu.reset();
        cpu
    }

    /// Resets the processor: Supervisor mode, interrupts masked, at the
    /// reset vector, with R14 and the SPSR of Supervisor mode written as an
    /// exception's, CP15 and FPEXC at their reset values and the exclusive
    /// monitor and event register clear. The other registers keep their
    /// values, as on hardware, and the interrupt lines stay as the devices
    /// driving them hold them.
    pub fn reset(&mut self) {
        self.cp15 = Cp15::new(self.profile.system, self.profile.isa);
        self.vfp.set_fpexc(0);
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
            return self.step_thumb(bus);
        }
        let pc = self.pc;
        self.next = self.advance(pc, 4);
        let fetched = match bus.fetch(pc, self.mode.privileged()) {
            Ok(fetched) => fetched,
            Err(abort) => {
                self.prefetch_abort(Fault::Bus(abort), pc, pc);
                self.pc = self.next;
                return Step::ran();
            }
        };
        let profile = self.profile;
        let decoded = self.memo.decoded(fetched.page, pc, fetched.word, |word| {
            Decoded::arm(word, profile)
        });
        let flow = self.dispatch(bus, decoded, fetched.host);
        self.finish(flow)
    }

    /// A step in Thumb state: one or two halfwords, under the condition the
    /// `IT` block gives them, which then advances past them.
    fn step_thumb<B: Bus>(&mut self, bus: &mut B) -> Step {
        let pc = self.pc;
        self.it_written = false;
        let (halfwords, page) = match self.fetch_thumb(bus, pc) {
            Ok(fetched) => fetched,
            Err((abort, address)) => {
                self.prefetch_abort(Fault::Bus(abort), address, pc);
                self.pc = self.next;
                return Step::ran();
            }
        };
        let profile = self.profile;
        let in_block = self.cpsr & (psr::IT_LOW | psr::IT_HIGH) != 0;
        let position = if in_block {
            Position::of(self.itstate())
        } else {
            Position::Outside
        };
        let decoded = self.thumb.decoded(
            page,
            pc,
            Thumb {
                halfwords,
                position,
            },
            |key| Decoded::thumb(key, profile),
        );
        // A host page's UDF calls the host only from A32 code.
        let flow = self.dispatch(bus, decoded, false);
        if in_block && !self.it_written && !matches!(flow, Flow::Refuse(_)) {
            self.advance_itstate();
        }
        self.finish(flow)
    }

    /// The instruction at `pc` in Thumb state, its halfwords packed as
    /// [`Thumb`] keys them, and the page of memory its first is in, with
    /// `next` set past it; or the abort and the address of the halfword the
    /// bus refused.
    fn fetch_thumb<B: Bus>(&mut self, bus: &mut B, pc: u32) -> Result<(u32, u32), (Abort, u32)> {
        let privileged = self.mode.privileged();
        let fetched = bus
            .fetch(pc & !3, privileged)
            .map_err(|abort| (abort, pc))?;
        let first = (fetched.word >> (8 * (pc & 2))) & 0xFFFF;
        if !t32::is_wide(low16(first)) {
            self.next = pc.wrapping_add(2);
            return Ok((first, fetched.page));
        }
        self.next = pc.wrapping_add(4);
        let second = if pc & 2 == 0 {
            fetched.word >> 16
        } else {
            let after = pc.wrapping_add(2);
            bus.fetch(after, privileged)
                .map_err(|abort| (abort, after))?
                .word
                & 0xFFFF
        };
        Ok((first | (second << 16), fetched.page))
    }

    /// Executes `decoded` if its condition holds, fetched from a host page
    /// when `host`.
    fn dispatch<B: Bus>(&mut self, bus: &mut B, decoded: Decoded, host: bool) -> Flow {
        match decoded {
            Decoded::Execute(instruction) if self.holds(instruction.cond) => {
                self.execute(bus, &instruction, host)
            }
            Decoded::Execute(_) => Flow::Next,
            Decoded::Undefined(cond) => {
                if self.holds(cond) {
                    self.undefined();
                }
                Flow::Next
            }
        }
    }

    /// Whether `cond` holds: `NV` never on a core where it means never, and
    /// always otherwise, where it marks an unconditional instruction.
    fn holds(&self, cond: Condition) -> bool {
        cond.passes(self.cpsr)
            || (cond == Condition::Nv && !self.profile.isa.has(Feature::ConditionNever))
    }

    /// Moves on past an instruction that did what `flow` says.
    const fn finish(&mut self, flow: Flow) -> Step {
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
    /// profile has, and the PC and IT state to what the CPSR's state can
    /// hold. The decoded pages are kept.
    pub fn set_state(&mut self, state: &State) -> Result<(), BadState> {
        let mode = self.valid_mode(state.cpsr).ok_or(BadState::Mode)?;
        self.mode = mode;
        self.cpsr = state.cpsr & self.profile.psr;
        self.spsr = state.spsr.map(|spsr| spsr & self.profile.psr);
        self.pc = state.pc;
        self.settle();
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
    /// the next instruction, as the current state can hold it.
    pub fn set_reg(&mut self, reg: Reg, value: u32) {
        match self.r.get_mut(reg.index()) {
            Some(slot) => *slot = value,
            None => self.pc = self.aligned_pc(value),
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

    /// The VFP and Advanced SIMD register file and its control registers.
    #[must_use]
    pub const fn vfp(&self) -> &Vfp {
        &self.vfp
    }

    /// The VFP and Advanced SIMD state, for a session to load or a test to
    /// set.
    pub fn vfp_mut(&mut self) -> &mut Vfp {
        &mut self.vfp
    }

    /// CPACR's grant of VFP and Advanced SIMD made `access`, as a privileged
    /// `MCR` writing both their fields makes it; a processor without them
    /// grants nothing.
    pub const fn grant_simdfp(&mut self, access: crate::vfp::Access) {
        self.cp15.set_simdfp_access(access.field());
    }

    /// Replaces the CPSR, switching banks if its mode changes, and holds the
    /// PC and IT state to what its state can hold; refused if it names a
    /// mode the profile does not have.
    pub fn set_cpsr(&mut self, cpsr: u32) -> Result<(), BadState> {
        let mode = self.valid_mode(cpsr).ok_or(BadState::Mode)?;
        self.switch_mode(mode);
        self.cpsr = cpsr & self.profile.psr;
        self.settle();
        Ok(())
    }

    /// Holds the PC and the IT state to what the CPSR's state can hold: no
    /// IT state outside Thumb, nor a reserved one.
    const fn settle(&mut self) {
        if !restorable_itstate(self.cpsr) {
            self.cpsr &= !(psr::IT_LOW | psr::IT_HIGH);
        }
        self.pc = self.aligned_pc(self.pc);
    }

    /// Replaces the condition flags N, Z, C and V with `flags`' own.
    pub const fn set_nzcv(&mut self, flags: u32) {
        self.cpsr = (self.cpsr & !psr::NZCV) | (flags & psr::NZCV);
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

    /// The system control register `opc1, CRn, CRm, opc2` as a privileged
    /// `MRC` reads it, if the processor has it: how an operating system
    /// learns the processor from its ID registers.
    #[must_use]
    pub fn system_register(&self, register: [u8; 4]) -> Option<u32> {
        match self.cp15.read(register, true) {
            crate::cp15::Outcome::Done(value) => Some(value),
            _ => None,
        }
    }

    /// The processor's caches at `level`, from zero, as its programmers'
    /// model describes them; none past the last.
    #[must_use]
    pub const fn cache(&self, level: u32) -> Option<crate::SplitCache> {
        self.cp15.cache(level)
    }

    /// The last data abort's fault status and address, as the system control
    /// coprocessor records them: what an abort handler reads to find the
    /// access that failed.
    #[must_use]
    pub const fn data_fault(&self) -> (u32, u32) {
        self.cp15.last_data_fault()
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
        self.thumb.release();
    }

    /// The bytes the decoded pages hold.
    #[must_use]
    pub fn memo_bytes(&self) -> usize {
        self.memo.bytes() + self.thumb.bytes()
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
        let mut cpsr = (self.cpsr & !mask) | (value & mask);
        if restore && !restorable_itstate(cpsr) {
            cpsr &= !(psr::IT_LOW | psr::IT_HIGH);
        }
        if let Some(mode) = self.valid_mode(cpsr) {
            self.switch_mode(mode);
        }
        self.cpsr = cpsr;
        // Into a 26-bit mode, the next instruction is within its space.
        self.next = self.aligned_pc(self.next);
        if restore {
            // An exception return is an event, as ARMv8's is.
            self.it_written = true;
            self.event = true;
        }
    }

    /// Writes the 26-bit PSR bits R15's layout holds in `r15`: the flags in
    /// User mode, and I, F and the mode too in a privileged one.
    fn write_r15_psr(&mut self, r15: u32) {
        if !self.mode.privileged() {
            self.set_nzcv(r15);
            return;
        }
        let cpsr = psr::from_r15(self.cpsr, r15);
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

    /// Restores the CPSR from `status`, as an exception return does; false
    /// if refused, the undefined instruction exception taken, for naming a
    /// mode the profile lacks.
    fn restore(&mut self, status: u32) -> bool {
        if self.valid_mode(status).is_none() && self.refuse() {
            return false;
        }
        self.write_cpsr(status, u32::MAX, true);
        true
    }

    /// Restores the CPSR from the SPSR: none in a mode without one, else
    /// whether it was restored or refused.
    fn restore_cpsr(&mut self) -> Option<bool> {
        self.spsr().map(|spsr| self.restore(spsr))
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
        self.it_written = true;
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
        let width = if self.cpsr & psr::T != 0 { 2 } else { 4 };
        self.take(Exception::Undefined, self.pc.wrapping_add(width));
    }

    /// Sets the IT state, which the instruction executing then keeps rather
    /// than advancing.
    fn set_itstate(&mut self, itstate: u8) {
        self.put_itstate(itstate);
        self.it_written = true;
    }

    const fn put_itstate(&mut self, itstate: u8) {
        let itstate = itstate as u32;
        self.cpsr = (self.cpsr & !(psr::IT_LOW | psr::IT_HIGH))
            | ((itstate & 3) << 25)
            | ((itstate >> 2) << 10);
    }

    const fn itstate(&self) -> u8 {
        psr::itstate(self.cpsr)
    }

    /// Moves the IT state on to the block's next instruction, or out of the
    /// block after its last.
    const fn advance_itstate(&mut self) {
        let itstate = self.itstate();
        if itstate.trailing_zeros() >= 3 {
            if itstate != 0 {
                self.put_itstate(0);
            }
        } else {
            self.put_itstate((itstate & 0xE0) | ((itstate << 1) & 0x1F));
        }
    }

    /// The prefetch abort `fault` at `address` takes for the instruction at
    /// `instruction`: the same address unless the second halfword of a
    /// 32-bit Thumb instruction faulted.
    fn prefetch_abort(&mut self, fault: Fault, address: u32, instruction: u32) {
        self.cp15.prefetch_fault(fault, address);
        self.take(Exception::PrefetchAbort, instruction.wrapping_add(4));
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

/// Whether an exception return may restore the IT state `cpsr` holds: never
/// to ARM state, nor a reserved value, as ARMv8 defines it.
const fn restorable_itstate(cpsr: u32) -> bool {
    let itstate = psr::itstate(cpsr);
    // A condition with no block left to give it.
    let reserved = itstate != 0 && itstate.trailing_zeros() >= 4;
    !reserved && (cpsr & psr::T != 0 || itstate == 0)
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
