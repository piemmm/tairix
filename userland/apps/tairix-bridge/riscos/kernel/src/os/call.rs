//! Calls into guest code, and the run that drives the processor through
//! them.
//!
//! A call saves the processor, enters the code in a privileged mode with the
//! return address on the host pages, and runs until the code comes back
//! there; the processor is then restored and the code's registers handed
//! back. The innermost run catches a return, so a call made from inside a
//! call returns to its own caller. Control that leaves for good unwinds
//! every call to the outermost run, which then loads where it went.

use tairix_riscos_arm::cpu::{Exception, State, Stop, Unmodelled};
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;

use super::{Ending, Os, Outcome, Pulse, Regs};
use crate::host::Routine;
use crate::memory::Bits;

/// The work, in instructions, between words with the host.
const SLICE: u64 = 1 << 20;
/// What a step of a SWI's own loop weighs toward the host's word: a slice
/// is some sixty-five thousand steps.
const STEP: u64 = 16;

/// The most calls in progress at once: guest code's, and built-in routines'
/// the host's own Rust called. A call past it is refused as the supervisor
/// stack overflowing, which is how RISC OS meets a nest too deep; it bounds
/// the host's own stack, so it is a containment bound, not a capacity.
pub const MAX_DEPTH: u32 = 48;

/// The supervisor bank's index in [`State::banked`].
const SUPERVISOR: usize = 1;
/// The IRQ bank's.
const IRQ: usize = 0;

/// Where guest code is called: its address and the profile it was loaded
/// under.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Target {
    /// The address.
    pub address: u32,
    /// The profile.
    pub bits: Bits,
}

/// The mode a call's code is entered in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Entry {
    /// Supervisor mode, interrupts as the caller had them.
    Supervisor,
    /// Supervisor mode with interrupts disabled.
    SupervisorNoIrq,
    /// Supervisor mode with interrupts enabled.
    SupervisorIrq,
    /// IRQ mode with interrupts disabled.
    Irq,
    /// User mode with interrupts enabled, its stack pointer the one given,
    /// as a transient utility is entered.
    User(u32),
}

impl Entry {
    /// The interrupt mask the entry enters with, from the caller's `cpsr`.
    #[must_use]
    pub const fn masked(self, cpsr: u32) -> u32 {
        match self {
            Self::Supervisor => cpsr & psr::I,
            Self::SupervisorIrq | Self::User(_) => 0,
            Self::SupervisorNoIrq | Self::Irq => psr::I,
        }
    }
}

/// How a call's code came back.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Returned {
    /// Through the return address it was given.
    Plain,
    /// A vector claimant passed the call on.
    PassedOn,
    /// A vector claimant claimed the call, returning through the address its
    /// caller stacked.
    Claimed,
}

/// Control passing to guest code that never returns to the code running:
/// the processor state the outermost run loads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transfer {
    /// The profile it runs under.
    pub bits: Bits,
    /// Its registers.
    pub state: State,
}

/// What a context waits for before it can run again.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Wait {
    /// A key, or the clock reaching `until`: it waits in `OS_ReadC`,
    /// `OS_ReadLine` or a timed `INKEY`. The SWI is performed again when one
    /// comes.
    Input {
        /// When it stops waiting regardless, in monotonic centiseconds.
        until: Option<u64>,
    },
    /// The clock reaching `until` and nothing else: it waits in a SWI such
    /// as `OS_Byte` 19, which is performed again then.
    Clock {
        /// When, in monotonic centiseconds.
        until: u64,
    },
}

/// How a context's program ended.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ended {
    /// It reached the default exit handler with this return code.
    Exit {
        /// `Sys$ReturnCode`.
        code: u32,
    },
    /// An error reached the default error handler.
    Error {
        /// The error's number.
        number: u32,
        /// Its block's address in guest memory.
        block: u32,
    },
    /// The kernel could not carry the context on: its state was refused.
    Fault,
    /// The program reset the machine.
    Reset {
        /// A hard reset, which starts more afresh than a soft one.
        hard: bool,
        /// The configured boot option is to be taken the other way.
        reverse_boot: bool,
    },
}

/// Why control leaves the code running and never returns to it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Unwind {
    /// To the guest code the kernel's pending transfer names.
    Transfer,
    /// The context waits: the outermost SWI is performed again later.
    Wait(Wait),
    /// The host stopped the run.
    Stopped,
    /// The processor reached something it does not model.
    Unmodelled(Unmodelled),
    /// The program ended.
    Ended(Ended),
}

impl Os<'_> {
    /// The machine's memory pressure and the log a session's cache records
    /// to, as the session was made with them.
    #[must_use]
    pub fn cache_context(
        &self,
    ) -> (
        &'static (dyn tairix_reclaim::PressureGauge + 'static),
        &'static (dyn tairix_log::Sink + Sync),
    ) {
        (self.world.pressure, self.world.log)
    }

    /// The key a cache over keys the guest chooses hashes them under, so a
    /// guest cannot flood one with keys it knows collide.
    #[must_use]
    pub const fn cache_key(&self) -> tairix_hash::HashSeed {
        self.world.cache_key
    }

    /// The session's memory ceiling, in bytes.
    #[must_use]
    pub fn ceiling_bytes(&self) -> u64 {
        self.memory.ram()
    }

    /// The session's desktop font: the TAIRiX desktop's interface text.
    #[must_use]
    pub fn desktop_font(&self) -> crate::desktop::DesktopFont {
        self.world.desktop_font
    }

    /// `request` carried to the side holding the session's files, its reply
    /// appended to `reply`: whether the host carried it.
    pub fn relay(&mut self, request: &[u8], reply: &mut alloc::vec::Vec<u8>) -> bool {
        self.world.host.relay(request, reply)
    }

    /// `request` carried to the font service, its reply appended to
    /// `reply`: whether the host carried it.
    pub fn font_relay(&mut self, request: &[u8], reply: &mut alloc::vec::Vec<u8>) -> bool {
        self.world.host.font(request, reply)
    }

    /// Calls the guest code at `target`, entered as `entry` says with `regs`,
    /// R14 its return address. `regs` holds R0 to R12 and the flags as the
    /// code left them.
    pub fn call(
        &mut self,
        target: Target,
        regs: &mut Regs,
        entry: Entry,
    ) -> Result<Returned, Unwind> {
        self.call_with(target, regs, entry, Routine::Return, None)
    }

    /// Calls the code at `address` like a subroutine with `regs`, entered as
    /// `entry` says: a built-in module's routine as its Rust directly, other
    /// code through the processor. V set is its error.
    ///
    /// # Errors
    ///
    /// The error the code gives, or the unwind leaving it.
    pub fn call_routine(
        &mut self,
        address: u32,
        bits: Bits,
        entry: Entry,
        regs: &mut Regs,
    ) -> Result<(), crate::os::Fail> {
        if let Some(code) = self.builtin_routine_at(address) {
            if self.nest_full() {
                return Err(crate::os::Fail::Unwind(self.overflowed()));
            }
            self.world.routines_nested += 1;
            let done = code(self, regs);
            self.world.routines_nested -= 1;
            return done;
        }
        self.call(Target { address, bits }, regs, entry)?;
        if regs.flags & psr::V != 0 {
            return Err(crate::os::Fail::Error(regs.r[0]));
        }
        Ok(())
    }

    /// Whether [`MAX_DEPTH`] calls are in progress already.
    fn nest_full(&self) -> bool {
        self.world.depth + self.world.routines_nested >= MAX_DEPTH
    }

    /// The supervisor bank's stack pointer, which the next call enters with.
    pub(crate) fn supervisor_stack(&mut self) -> u32 {
        self.world.processor.state().banked[SUPERVISOR][0]
    }

    /// Sets the supervisor bank's stack pointer, as the next call enters
    /// with it.
    pub(crate) fn set_supervisor_stack(&mut self, sp: u32) {
        let bits = self.world.processor.bits();
        let mut state = self.world.processor.state();
        state.banked[SUPERVISOR][0] = sp;
        let _ = self.world.processor.load(bits, &state);
    }

    /// Runs `work` with `bytes` of the supervisor stack set aside below its
    /// pointer, at the word-aligned address `work` is given: what `work`
    /// writes there outlives the calls it makes, which enter below it, as a
    /// RISC OS module's stack frame does. A room the stack has not got is
    /// the overflow RISC OS meets.
    ///
    /// # Errors
    ///
    /// The overflow, or what `work` fails with.
    pub fn below_supervisor_stack<T>(
        &mut self,
        bytes: u32,
        work: impl FnOnce(&mut Self, u32) -> Result<T, crate::os::Fail>,
    ) -> Result<T, crate::os::Fail> {
        let saved = self.supervisor_stack();
        let room = bytes.checked_add(7).map(|b| b & !7);
        let frame = room.and_then(|room| saved.checked_sub(room));
        let Some(frame) = frame.filter(|&f| f >= self.memory.layout().stacks.supervisor.start)
        else {
            return Err(crate::os::Fail::Unwind(self.overflowed()));
        };
        self.set_supervisor_stack(frame);
        let done = work(self, frame);
        self.set_supervisor_stack(saved);
        done
    }

    /// Runs `work` with `text` and its terminator `end` below the supervisor
    /// stack, at the address `work` is given, as a name or line handed to a
    /// call that reads it in place.
    ///
    /// # Errors
    ///
    /// The stack's overflow, or what `work` fails with.
    pub fn with_text<T>(
        &mut self,
        text: &[u8],
        end: u8,
        work: impl FnOnce(&mut Self, u32) -> Result<T, crate::os::Fail>,
    ) -> Result<T, crate::os::Fail> {
        let length = crate::os::len32(text.len());
        self.below_supervisor_stack(length.saturating_add(1), |os, at| {
            os.write_bytes(at, text)?;
            os.write_byte(at.wrapping_add(length), end)?;
            work(os, at)
        })
    }

    /// Calls a vector claimant at `target` with `regs`: its return address
    /// passes the call on, and `stacked` — the address that claims it — is
    /// on the supervisor stack, which the call leaves as the claimant left
    /// it.
    pub(crate) fn call_claimant(
        &mut self,
        target: Target,
        regs: &mut Regs,
        entry: Entry,
        stack: u32,
    ) -> Result<(Returned, u32), Unwind> {
        let mut sp = stack;
        let returned = self.call_with(target, regs, entry, Routine::PassOn, Some(&mut sp))?;
        Ok((returned, sp))
    }

    fn call_with(
        &mut self,
        target: Target,
        regs: &mut Regs,
        entry: Entry,
        back: Routine,
        stack: Option<&mut u32>,
    ) -> Result<Returned, Unwind> {
        if self.nest_full() {
            return Err(self.overflowed());
        }
        let saved_bits = self.world.processor.bits();
        let saved = self.world.processor.state();
        let state = self.entry_state(&saved, target, regs, entry, back, stack.as_deref().copied());
        if self.world.processor.load(target.bits, &state).is_err() {
            return Err(Unwind::Ended(Ended::Fault));
        }
        self.world.guest_called = true;
        self.world.depth += 1;
        let reached = self.execute(true, None);
        self.world.depth -= 1;
        let cpu = self.world.processor.cpu();
        for (n, slot) in (0..).zip(regs.r.iter_mut()) {
            *slot = cpu.reg(Reg::field(n));
        }
        regs.flags = cpu.cpsr() & psr::NZCV;
        let sp = cpu.reg(Reg::SP);
        if let Some(stack) = stack {
            *stack = sp;
        }
        if self.world.processor.load(saved_bits, &saved).is_err() {
            return Err(Unwind::Ended(Ended::Fault));
        }
        match reached? {
            Some(returned) => Ok(returned),
            None => Err(Unwind::Ended(Ended::Fault)),
        }
    }

    /// The registers a call enters with: the caller's banks, the mode and
    /// interrupts `entry` asks for, R0 to R12 from `regs`, the stack the
    /// mode had or `stack`, and R14 the host page's `back`, in a 26-bit
    /// mode with the PSR beside it.
    fn entry_state(
        &self,
        saved: &State,
        target: Target,
        regs: &Regs,
        entry: Entry,
        back: Routine,
        stack: Option<u32>,
    ) -> State {
        let mut state = *saved;
        let (mode, banked) = match (entry, target.bits) {
            (Entry::User(_), Bits::Bit26) => (Mode::User26, None),
            (Entry::User(_), Bits::Bit32) => (Mode::User, None),
            (Entry::Irq, Bits::Bit26) => (Mode::Irq26, Some(IRQ)),
            (Entry::Irq, Bits::Bit32) => (Mode::Irq, Some(IRQ)),
            (_, Bits::Bit26) => (Mode::Supervisor26, Some(SUPERVISOR)),
            (_, Bits::Bit32) => (Mode::Supervisor, Some(SUPERVISOR)),
        };
        state.cpsr = mode.bits() | (regs.flags & psr::NZCV) | entry.masked(saved.cpsr);
        state.user[..13].copy_from_slice(&regs.r);
        let mut lr = self.world.routine_address(back);
        let mut pc = target.address;
        if target.bits == Bits::Bit26 {
            lr = (lr & psr::R15_PC) | psr::r15_psr(state.cpsr);
            pc &= psr::R15_PC;
        }
        match (banked, entry) {
            (Some(banked), _) => {
                state.banked[banked] = [stack.unwrap_or(saved.banked[banked][0]), lr];
            }
            (None, Entry::User(sp)) => [state.user[13], state.user[14]] = [sp, lr],
            (None, _) => {}
        }
        state.pc = pc;
        state
    }

    /// Runs the processor until a call's code returns, when `nested`, or
    /// until `budget` instructions have run at the outermost level. A SWI or
    /// host call it stops at is performed on the way.
    pub(crate) fn execute(
        &mut self,
        nested: bool,
        mut budget: Option<&mut u64>,
    ) -> Result<Option<Returned>, Unwind> {
        loop {
            if let Some(left) = budget.as_deref_mut() {
                *left = left.saturating_sub(core::mem::take(&mut self.world.charged));
            }
            let slice = budget.as_deref().map_or(SLICE, |&left| left.min(SLICE));
            if slice == 0 {
                return Ok(None);
            }
            let room = SLICE - self.world.since_word;
            let run = self
                .world
                .processor
                .cpu()
                .run(self.memory.bus(), slice.min(room));
            if let Some(left) = budget.as_deref_mut() {
                *left = left.saturating_sub(run.executed);
            }
            // Counted across every call, so neither a run of SWIs nor a
            // loop of short calls keeps the host from its word.
            self.world.since_word += run.executed.max(1);
            if self.world.since_word >= SLICE {
                self.world.since_word = 0;
                self.between_slices()?;
            }
            match run.stop {
                Stop::Budget => {}
                Stop::Swi { number } => self.swi(number)?,
                Stop::HostCall { number } => {
                    if let Some(returned) = self.host_call(number, nested)? {
                        return Ok(Some(returned));
                    }
                }
                Stop::WaitForInterrupt | Stop::WaitForEvent => self.idle()?,
                Stop::Unmodelled(unmodelled) => return Err(Unwind::Unmodelled(unmodelled)),
                Stop::FloatingPoint { exception } => self.floating_point(exception)?,
            }
        }
    }

    /// A step of a loop in a SWI's or a host routine's Rust that runs as long
    /// as the guest asks: counted with the processor's work toward the host's
    /// word and the context's run, so a guest asking for endless work can be
    /// stopped and shares the processor as one running instructions does.
    ///
    /// # Errors
    ///
    /// The host stopping the run.
    pub fn yield_point(&mut self) -> Result<(), Unwind> {
        self.world.charged = self.world.charged.saturating_add(STEP);
        self.world.since_word += STEP;
        if self.world.since_word < SLICE {
            return Ok(());
        }
        self.world.since_word = 0;
        self.between_slices()
    }

    /// Between slices: the clock's due work, and the host's word on
    /// carrying on.
    fn between_slices(&mut self) -> Result<(), Unwind> {
        self.tick();
        match self.world.host.pulse() {
            Pulse::Continue => Ok(()),
            Pulse::Stop => Err(Unwind::Stopped),
        }
    }

    /// The processor waits for an interrupt with none pending: the host
    /// waits for the clock's next deadline or input, so an idle guest costs
    /// nothing, and its waking is the interrupt.
    fn idle(&mut self) -> Result<(), Unwind> {
        let until = self.next_deadline();
        match self.world.host.wait(until, Ending::Clock) {
            Pulse::Continue => {
                self.world.processor.set_irq(true);
                Ok(())
            }
            Pulse::Stop => Err(Unwind::Stopped),
        }
    }

    /// A host call: a call's return, or a host routine performed.
    fn host_call(&mut self, number: u32, nested: bool) -> Result<Option<Returned>, Unwind> {
        let returned = match Routine::of(number) {
            Routine::Return => Returned::Plain,
            Routine::PassOn => Returned::PassedOn,
            Routine::Claimed => Returned::Claimed,
            routine => {
                self.routine(routine)?;
                return Ok(None);
            }
        };
        if nested {
            Ok(Some(returned))
        } else {
            // A return with no call to return from: the host page's word is
            // the undefined instruction it encodes.
            self.undefined_here();
            Ok(None)
        }
    }

    /// Takes the undefined instruction exception for the host page word just
    /// executed.
    pub(crate) fn undefined_here(&mut self) {
        let cpu = self.world.processor.cpu();
        let at = cpu.pc().wrapping_sub(4);
        cpu.enter(Exception::Undefined, at.wrapping_add(4));
    }

    /// Runs the current context for at most `budget` instructions, loading
    /// each transfer it unwinds to, one a host call left first.
    pub(crate) fn run(&mut self, budget: u64) -> Outcome {
        // Host work done between runs is not this run's.
        self.world.charged = 0;
        // The clock may have moved on while the host had the context.
        self.tick();
        if self.world.transfer.is_some() {
            if let Err(outcome) = self.load_transfer() {
                return outcome;
            }
        }
        let mut left = budget;
        loop {
            match self.execute(false, Some(&mut left)) {
                Ok(_) => return Outcome::Budget,
                Err(Unwind::Transfer) => {
                    if let Err(outcome) = self.load_transfer() {
                        return outcome;
                    }
                }
                Err(Unwind::Wait(wait)) => return Outcome::Waiting(wait),
                Err(Unwind::Stopped) => return Outcome::Stopped,
                Err(Unwind::Unmodelled(unmodelled)) => return Outcome::Unmodelled(unmodelled),
                Err(Unwind::Ended(ended)) => return Outcome::Ended(ended),
            }
        }
    }

    /// Lets the processor take the interrupt the clock asserted while the
    /// kernel waits within a SWI, as RISC OS's waiting SWIs enable
    /// interrupts: a call to the return routine with them enabled, which the
    /// processor reaches only once it has taken the interrupt.
    pub(crate) fn take_interrupt(&mut self) -> Result<(), Unwind> {
        if !self.world.processor.irq() || self.world.in_interrupt {
            return Ok(());
        }
        let target = Target {
            address: self.world.routine_address(Routine::Return),
            bits: self.world.processor.bits(),
        };
        self.call(target, &mut Regs::default(), Entry::SupervisorIrq)?;
        Ok(())
    }

    /// Loads the pending transfer: whatever SWI waited is abandoned.
    fn load_transfer(&mut self) -> Result<(), Outcome> {
        self.world.resume = None;
        self.world.continuation = None;
        let Some(transfer) = self.world.transfer.take() else {
            return Err(Outcome::Ended(Ended::Fault));
        };
        self.world
            .processor
            .load(transfer.bits, &transfer.state)
            .map_err(|_| Outcome::Ended(Ended::Fault))
    }

    /// Unwinds to `transfer`.
    pub(crate) fn transfer(&mut self, transfer: Transfer) -> Unwind {
        self.world.transfer = Some(transfer);
        Unwind::Transfer
    }
}
