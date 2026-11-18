//! Environment handlers (`plans/RISCOS-EMULATOR.md` §4.5): the seventeen
//! slots `OS_ChangeEnvironment` sets, the kernel's defaults for them, and
//! what enters them — an error, an exit, an exception, a breakpoint.
//!
//! A slot holds an address (or a value), the R12 its code is given and a
//! buffer, as RISC OS 5's table does, and the profile of the code that set
//! it, which its code runs under. A slot is never set to zero: zero asks
//! for it to be left as it is.

use tairix_riscos_arm::cpu::State;
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;

use crate::error::Error;
use crate::memory::Bits;
use crate::os::{Ended, Entry, Fail, Os, Regs, Transfer, Unwind};
use crate::swi::os::fs_control::START_APPLICATION;
use crate::vectors;

/// The handlers there are.
pub const HANDLERS: u32 = 17;

/// The memory limit: a value, the top of the current program's memory.
pub const MEMORY_LIMIT: u32 = 0;
/// The undefined instruction handler.
pub const UNDEFINED: u32 = 1;
/// The prefetch abort handler.
pub const PREFETCH_ABORT: u32 = 2;
/// The data abort handler.
pub const DATA_ABORT: u32 = 3;
/// The address exception handler, which no processor here takes.
pub const ADDRESS_EXCEPTION: u32 = 4;
/// The error handler.
pub const ERROR: u32 = 6;
/// The legacy callback handler.
pub const CALLBACK: u32 = 7;
/// The breakpoint handler.
pub const BREAKPOINT: u32 = 8;
/// The escape handler.
pub const ESCAPE: u32 = 9;
/// The event handler.
pub const EVENT: u32 = 10;
/// The exit handler.
pub const EXIT: u32 = 11;
/// The unused SWI handler.
pub const UNUSED_SWI: u32 = 12;
/// The exception dump area: a value.
pub const EXCEPTION_DUMP: u32 = 13;
/// The application space size: a value.
pub const APPLICATION_SPACE: u32 = 14;
/// The current active object: a value.
pub const CURRENT_ACTIVE_OBJECT: u32 = 15;
/// The `UpCall` handler.
pub const UPCALL: u32 = 16;

/// Which of R1, R2 and R3 a handler's slot holds.
const fn fields(handler: u32) -> [bool; 3] {
    match handler {
        ERROR | CALLBACK | BREAKPOINT => [true, true, true],
        ESCAPE | EVENT | EXIT | UNUSED_SWI | UPCALL => [true, true, false],
        5 => [false, false, false],
        _ => [true, false, false],
    }
}

/// "Environment handler number out of range".
const BAD_ENV_NUMBER: Error = Error {
    number: 0x1B0,
    token: "BadHandlerNumber",
};
/// "Return code limit exceeded": a negative code, read unsigned, past any.
pub(crate) const RC_EXCEEDED: Error = Error {
    number: 0x1E2,
    token: "ReturnCodeOverLimit",
};
/// "SWI &%0 returned an invalid error pointer".
const BAD_ERROR_POINTER: Error = Error {
    number: 0x116,
    token: "InvalidErrorPointer",
};
/// "SWI &%0 not known".
pub(crate) const NO_SUCH_SWI: Error = Error {
    number: 0x1E6,
    token: "UnknownSwiNumber",
};

/// The `ABEX` word `OS_Exit` reads a return code beside.
const ABEX: u32 = 0x5845_4241;
/// The return code limit `Sys$RCLimit` starts at.
const RC_LIMIT: u32 = 256;
/// The error buffer's size, as the PRM has it: the PC, then the error block.
pub(crate) const ERROR_BUFFER: u32 = 256;
/// The longest text the error buffer holds: what the PC, the error's number
/// and the text's zero leave.
const ERROR_TEXT: usize = ERROR_BUFFER as usize - 9;
/// The register dump's size: R0 to R15 and the PSR, the most it holds.
pub(crate) const DUMP: u32 = 17 * 4;

/// A register dump as a handler of `bits` reads one, and its length in
/// words: R0 to R14, then R15. A 26-bit dump's R15 holds the PSR beside the
/// PC; a 32-bit one's has no room, so the CPSR follows it.
pub(crate) fn dump_words(
    registers: &[u32; 15],
    pc: u32,
    cpsr: u32,
    bits: Bits,
) -> ([u32; 17], usize) {
    let mut words = [0; 17];
    words[..15].copy_from_slice(registers);
    match bits {
        Bits::Bit26 => {
            words[15] = (pc & psr::R15_PC) | psr::r15_psr(cpsr);
            (words, 16)
        }
        Bits::Bit32 => {
            words[15] = pc;
            words[16] = cpsr;
            (words, 17)
        }
    }
}
/// The command line `OS_GetEnv` points at.
pub(crate) const COMMAND_LINE: u32 = 1024;

/// The handlers an error the error handler cannot take returns to their
/// defaults.
pub(crate) const ERROR_HANDLERS: [u32; 3] = [ERROR, ESCAPE, UPCALL];
/// The handlers a program's end returns to their defaults.
pub(crate) const EXIT_HANDLERS: [u32; 9] = [
    EVENT,
    CALLBACK,
    EXIT,
    UNDEFINED,
    PREFETCH_ABORT,
    DATA_ABORT,
    ADDRESS_EXCEPTION,
    BREAKPOINT,
    UNUSED_SWI,
];

/// A handler's slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Slot {
    /// The address, or for a value slot, the value.
    pub(crate) address: u32,
    /// The R12 its code is given.
    pub(crate) workspace: u32,
    /// Its buffer.
    pub(crate) buffer: u32,
    /// The profile its code runs under.
    pub(crate) bits: Bits,
}

/// The kernel's buffers in guest memory the environment hands out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Buffers {
    /// The default error buffer, `ERROR_BUFFER` bytes.
    pub(crate) error: u32,
    /// The default register dump, `DUMP` bytes.
    pub(crate) dump: u32,
    /// `OS_GetEnv`'s command line, `COMMAND_LINE` bytes.
    pub(crate) command: u32,
    /// `OS_GetEnv`'s start time, five bytes.
    pub(crate) start: u32,
}

/// The environment: every handler's slot and the defaults they return to.
pub(crate) struct Environment {
    slots: [Slot; HANDLERS as usize],
    defaults: [Slot; HANDLERS as usize],
    /// The buffers it hands out.
    pub(crate) buffers: Buffers,
    /// `Sys$ReturnCode`.
    pub(crate) return_code: u32,
    /// `Sys$RCLimit`.
    pub(crate) rc_limit: u32,
}

impl Environment {
    /// The environment at reset: every slot its default, the defaults'
    /// code at `routine` (each handler's address on the host pages), the
    /// memory limit and application space `limit`.
    pub(crate) fn new(buffers: Buffers, routine: impl Fn(u32) -> u32, limit: u32) -> Self {
        let mut defaults = [Slot {
            address: 0,
            workspace: 0,
            buffer: 0,
            bits: Bits::Bit32,
        }; HANDLERS as usize];
        for (handler, slot) in (0..).zip(defaults.iter_mut()) {
            slot.address = match handler {
                MEMORY_LIMIT | 5 | EXCEPTION_DUMP | APPLICATION_SPACE | CURRENT_ACTIVE_OBJECT => 0,
                _ => routine(handler),
            };
            slot.buffer = match handler {
                ERROR | ESCAPE => buffers.error,
                CALLBACK | BREAKPOINT => buffers.dump,
                _ => 0,
            };
        }
        let mut slots = defaults;
        slots[MEMORY_LIMIT as usize].address = limit;
        slots[APPLICATION_SPACE as usize].address = limit;
        slots[EXCEPTION_DUMP as usize].address = buffers.dump;
        Self {
            slots,
            defaults,
            buffers,
            return_code: 0,
            rc_limit: RC_LIMIT,
        }
    }

    /// A handler's slot.
    pub(crate) const fn slot(&self, handler: u32) -> Slot {
        self.slots[handler as usize]
    }

    /// Application space's top made `top`: the memory limit and the
    /// application space handler both.
    pub(crate) const fn set_application_top(&mut self, top: u32) {
        self.slots[MEMORY_LIMIT as usize].address = top;
        self.slots[APPLICATION_SPACE as usize].address = top;
    }

    /// The current active object.
    pub(crate) const fn active_object(&self) -> u32 {
        self.slots[CURRENT_ACTIVE_OBJECT as usize].address
    }

    /// Sets the current active object.
    pub(crate) const fn set_active_object(&mut self, address: u32) {
        self.slots[CURRENT_ACTIVE_OBJECT as usize].address = address;
    }

    /// Sets `handler`'s fields to those of `new` that are not zero, from code
    /// of `bits`; its fields as they were.
    fn change(&mut self, handler: u32, new: [u32; 3], bits: Bits) -> [u32; 3] {
        let slot = &mut self.slots[handler as usize];
        let old = [slot.address, slot.workspace, slot.buffer];
        let held = fields(handler);
        let mut out = new;
        for (i, (field, value)) in [&mut slot.address, &mut slot.workspace, &mut slot.buffer]
            .into_iter()
            .zip(new)
            .enumerate()
        {
            if held[i] {
                out[i] = old[i];
                if value != 0 {
                    *field = value;
                    if i == 0 {
                        slot.bits = bits;
                    }
                }
            }
        }
        out
    }

    /// Returns `handlers` to their defaults.
    pub(crate) fn reinstate(&mut self, handlers: &[u32]) {
        for &handler in handlers {
            self.slots[handler as usize] = self.defaults[handler as usize];
        }
    }

    /// Whether `handler` holds its default code.
    pub(crate) const fn is_default(&self, handler: u32) -> bool {
        self.slots[handler as usize].address == self.defaults[handler as usize].address
    }
}

/// The text of `n` in hex, leading zeros dropped, as the kernel's error
/// messages show an address or number.
pub(crate) fn hex(n: u32) -> alloc::vec::Vec<u8> {
    let mut text = alloc::vec::Vec::new();
    let digits = (32 - n.leading_zeros()).div_ceil(4).max(1);
    if text.try_reserve(digits as usize).is_ok() {
        for shift in (0..digits).rev() {
            text.push(b"0123456789ABCDEF"[((n >> (4 * shift)) & 0xF) as usize]);
        }
    }
    text
}

/// The text of `n` as eight hex digits.
pub(crate) fn hex8(n: u32) -> [u8; 8] {
    let mut text = [0; 8];
    for (i, digit) in text.iter_mut().enumerate() {
        *digit = b"0123456789ABCDEF"[((n >> (28 - 4 * i)) & 0xF) as usize];
    }
    text
}

impl Os<'_> {
    /// The profile of the code running: a SWI's caller's.
    pub fn caller_bits(&mut self) -> Bits {
        self.world.processor.bits()
    }

    /// `OS_ChangeEnvironment`, through `ChangeEnvironmentV`.
    pub(crate) fn change_environment(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(
            vectors::CHANGE_ENVIRONMENT,
            regs,
            Entry::Supervisor,
            vectors::Carry::Caller,
        )
    }

    /// `ChangeEnvironmentV`'s default claimant: the slot changed.
    pub(crate) fn adjust_environment(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let handler = regs.r[0];
        if handler >= HANDLERS {
            return Err(self.error(BAD_ENV_NUMBER));
        }
        let bits = self.caller_bits();
        let old = self
            .world
            .env
            .change(handler, [regs.r[1], regs.r[2], regs.r[3]], bits);
        regs.r[1..4].copy_from_slice(&old);
        Ok(())
    }

    /// `OS_ReadDefaultHandler`.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn read_default_handler(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let handler = regs.r[0];
        if handler >= HANDLERS {
            regs.r[0] = 0;
            return Ok(());
        }
        let default = self.world.env.defaults[handler as usize];
        regs.r[1] = default.address;
        regs.r[2] = 0;
        regs.r[3] = default.buffer;
        Ok(())
    }

    /// Changes `handler`'s fields as `OS_ChangeEnvironment` would, through
    /// its vector, for the older SWIs that set several at once.
    fn change_one(&mut self, handler: u32, new: [u32; 3]) -> Result<[u32; 3], Fail> {
        let mut regs = Regs::new([handler, new[0], new[1], new[2], 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.change_environment(&mut regs)?;
        Ok([regs.r[1], regs.r[2], regs.r[3]])
    }

    /// `OS_Control`: the error handler, its buffer, the escape handler and
    /// the event handler.
    pub(crate) fn control(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let [error, _, buffer] = self.change_one(ERROR, [regs.r[0], 0, regs.r[1]])?;
        let [escape, ..] = self.change_one(ESCAPE, [regs.r[2], 0, 0])?;
        let [event, ..] = self.change_one(EVENT, [regs.r[3], 0, 0])?;
        regs.r[..4].copy_from_slice(&[error, buffer, escape, event]);
        Ok(())
    }

    /// `OS_SetEnv`: the exit handler, the memory limit and the four exception
    /// handlers.
    pub(crate) fn set_env(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let [exit, ..] = self.change_one(EXIT, [regs.r[0], 0, 0])?;
        let [limit, ..] = self.change_one(MEMORY_LIMIT, [regs.r[1], 0, 0])?;
        let mut exceptions = [0; 4];
        for (slot, (handler, new)) in exceptions.iter_mut().zip(
            [UNDEFINED, PREFETCH_ABORT, DATA_ABORT, ADDRESS_EXCEPTION]
                .into_iter()
                .zip(&regs.r[4..8]),
        ) {
            *slot = self.change_one(handler, [*new, 0, 0])?[0];
        }
        regs.r[0] = exit;
        regs.r[1] = limit;
        regs.r[2] = u32::try_from(self.memory.ram()).unwrap_or(u32::MAX);
        regs.r[3] = 0;
        regs.r[4..8].copy_from_slice(&exceptions);
        Ok(())
    }

    /// `OS_CallBack` and `OS_BreakCtrl`: a handler and its buffer, in R1 and
    /// R0.
    pub(crate) fn handler_and_buffer(&mut self, handler: u32, regs: &mut Regs) -> Result<(), Fail> {
        let [address, _, buffer] = self.change_one(handler, [regs.r[1], 0, regs.r[0]])?;
        regs.r[0] = buffer;
        regs.r[1] = address;
        Ok(())
    }

    /// `OS_UnusedSWI`: the unused SWI handler, and the R12 R2 brings along,
    /// as genuine RISC OS 5 takes it.
    pub(crate) fn unused_swi(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let [address, workspace, _] = self.change_one(UNUSED_SWI, [regs.r[0], regs.r[2], 0])?;
        regs.r[0] = address;
        regs.r[2] = workspace;
        Ok(())
    }

    /// `OS_GetEnv`.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn get_env(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let buffers = self.world.env.buffers;
        regs.r[0] = buffers.command;
        regs.r[1] = self.world.env.slot(MEMORY_LIMIT).address;
        regs.r[2] = buffers.start;
        Ok(())
    }

    /// `OS_WriteEnv`: the command line (to its first control character, cut
    /// to the buffer) and the start time, each unless zero.
    pub(crate) fn write_env(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let buffers = self.world.env.buffers;
        if regs.r[0] != 0 {
            let (line, _) = self.read_until(regs.r[0], COMMAND_LINE as usize - 1, |b| b < b' ')?;
            self.write_terminated(buffers.command, &line, 0, line.len() + 1)?;
        }
        if regs.r[1] != 0 {
            let mut time = [0; 5];
            self.read_bytes(regs.r[1], &mut time)?;
            self.write_bytes(buffers.start, &time)?;
        }
        Ok(())
    }

    /// Checks an error block a SWI returned: one below `&4000` or out of
    /// alignment is replaced by "returned an invalid error pointer", naming SWI
    /// `number`.
    pub(crate) fn checked_error(&mut self, number: u32, block: u32) -> Fail {
        if block >= 0x4000 && block.is_multiple_of(4) {
            return Fail::Error(block);
        }
        let name = hex(number & !crate::swi::X);
        self.error_with(BAD_ERROR_POINTER, &[&name])
    }

    /// An error from a SWI without X: `Service_Error`, then `ErrorV`. A claimant
    /// of `ErrorV` may swallow it, and the SWI then returns as though it had
    /// not failed; otherwise the error handler is entered.
    pub(crate) fn raise(&mut self, block: u32) -> Result<(), Unwind> {
        let mut regs = Regs::new([block, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.service(&mut regs)?;
        let mut regs = Regs::new([block, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.call_vector(vectors::ERROR, &mut regs, Entry::Supervisor)
    }

    /// `ErrorV`'s default claimant: the error copied into the error handler's
    /// buffer, the privileged stacks emptied, and the handler entered in
    /// User mode with its R12 in R0.
    pub(crate) fn enter_error_handler(&mut self, block: u32) -> Result<(), Fail> {
        if !self.error_handler_sound() {
            self.world.env.reinstate(&ERROR_HANDLERS);
        }
        let (pc, upper) = if let Some(frame) = self.world.frame {
            (frame.return_pc, frame.upper)
        } else {
            let state = self.world.processor.state();
            (state.pc, [state.user[10], state.user[11], state.user[12]])
        };
        let slot = self.world.env.slot(ERROR);
        let number = self.read_word(block)?;
        let (text, _) = self.read_until(block.wrapping_add(4), ERROR_TEXT, |b| b == 0)?;
        self.write_word(slot.buffer, pc)?;
        self.write_word(slot.buffer.wrapping_add(4), number)?;
        self.write_terminated(slot.buffer.wrapping_add(8), &text, 0, text.len() + 1)?;
        self.world.env.set_active_object(slot.address);
        let mut registers = [0; 13];
        registers[0] = slot.workspace;
        registers[10..13].copy_from_slice(&upper);
        let transfer = self.user_transfer(slot, registers);
        self.world.callbacks_first = true;
        Err(Fail::Unwind(self.transfer(transfer)))
    }

    /// Whether the error handler's code can be read and its buffer used: a
    /// handler that cannot is replaced by the defaults first.
    fn error_handler_sound(&mut self) -> bool {
        let slot = self.world.env.slot(ERROR);
        let readable = self.peek_word(slot.address & !3).is_some();
        let buffer = slot.buffer;
        readable
            && buffer.is_multiple_of(4)
            && self.writable(buffer, u64::from(buffer) + u64::from(ERROR_BUFFER))
    }

    /// A transfer into `slot`'s code in User mode, under its profile, with
    /// `registers` in R0 to R12 and the privileged stacks empty.
    fn user_transfer(&mut self, slot: Slot, registers: [u32; 13]) -> Transfer {
        self.user_entry(slot.bits, slot.address, |user| {
            user[..13].copy_from_slice(&registers);
        })
    }

    /// A transfer to `entry` in User mode with interrupts enabled, under
    /// `bits`' profile, the privileged stacks empty and the User registers
    /// as `registers` leaves the current ones.
    fn user_entry(
        &mut self,
        bits: Bits,
        entry: u32,
        registers: impl FnOnce(&mut [u32; 15]),
    ) -> Transfer {
        let mode = match bits {
            Bits::Bit26 => Mode::User26,
            Bits::Bit32 => Mode::User,
        };
        let state = self.world.processor.state();
        let mut state = self.emptied_stacks(state);
        registers(&mut state.user);
        state.cpsr = mode.bits();
        state.pc = if bits == Bits::Bit26 {
            entry & psr::R15_PC
        } else {
            entry
        };
        Transfer { bits, state }
    }

    /// `FSControl_StartApplication`: the filing system and the modules told
    /// that an application is starting, any of them able to refuse it, with
    /// `tail` its parameters, `owner` the currently active object and
    /// `name` the command it was started by.
    ///
    /// # Errors
    ///
    /// The refusal, or an access the names fail.
    pub fn start_application(&mut self, tail: u32, owner: u32, name: u32) -> Result<(), Fail> {
        let mut start = Regs::default();
        start.r[..4].copy_from_slice(&[START_APPLICATION, tail, owner, name]);
        self.perform(crate::swi::X | crate::swi::os::FS_CONTROL, &mut start)
    }

    /// The application started at `entry`, as `TaskControl_ResetStacks`
    /// leaves the kernel for one, its registers as `registers` sets them:
    /// the unwind that leaves for it.
    pub fn enter_application(
        &mut self,
        bits: Bits,
        entry: u32,
        registers: impl FnOnce(&mut [u32; 15]),
    ) -> Fail {
        self.reset_semaphores();
        let transfer = self.user_entry(bits, entry, registers);
        Fail::Unwind(self.transfer(transfer))
    }

    /// `state` with every privileged mode's stack empty, as the kernel
    /// leaves them when it flattens them.
    pub(crate) fn emptied_stacks(&mut self, mut state: State) -> State {
        let stacks = &self.memory.layout().stacks;
        state.banked[0][0] = stacks.irq.end;
        state.banked[1][0] = stacks.supervisor.end;
        state.banked[2][0] = stacks.abort.end;
        state.banked[3][0] = stacks.undefined.end;
        self.world.frame = None;
        state
    }

    /// `OS_GenerateError`: the block in R0 returned as the SWI's error.
    #[expect(clippy::unused_self, reason = "a SWI handler's signature")]
    pub(crate) fn generate_error(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        Err(Fail::Error(regs.r[0]))
    }

    /// `OS_Exit`: the return code `ABEX` gives, checked against
    /// `Sys$RCLimit`; past it, the error R0 names or the kernel's own.
    /// Otherwise the exit handler is entered.
    pub(crate) fn exit(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let code = if regs.r[1] == ABEX && regs.r[0].is_multiple_of(4) {
            regs.r[2]
        } else {
            0
        };
        self.world.env.return_code = code;
        if code > self.world.env.rc_limit {
            let block = regs.r[0];
            if block >= 0x4000
                && self.peek_word(block).is_some()
                && self.peek_word(block.wrapping_add(4)).is_some()
            {
                return Err(Fail::Error(block));
            }
            return Err(self.error(RC_EXCEEDED));
        }
        let slot = self.world.env.slot(EXIT);
        self.world.env.set_active_object(slot.address);
        let mut registers = [0; 13];
        registers[1] = regs.r[1];
        registers[2] = code;
        registers[12] = slot.workspace;
        let transfer = self.user_transfer(slot, registers);
        Err(Fail::Unwind(self.transfer(transfer)))
    }

    /// The exit handler's default: the program is over.
    fn default_exit(&mut self) -> Unwind {
        self.world.env.reinstate(&EXIT_HANDLERS);
        let code = self.world.processor.cpu().reg(Reg::field(2));
        Unwind::Ended(Ended::Exit { code })
    }

    /// The error handler's default: the error printed as RISC OS prints it,
    /// and the program over, its handlers returned as an exit returns them.
    fn default_error(&mut self) -> Unwind {
        self.world.env.reinstate(&EXIT_HANDLERS);
        let buffer = self.world.env.slot(ERROR).buffer;
        let number = self.peek_word(buffer.wrapping_add(4)).unwrap_or(0);
        let text = self
            .read_until(buffer.wrapping_add(8), ERROR_TEXT, |b| b == 0)
            .map(|(text, _)| text)
            .unwrap_or_default();
        let line = self.message_with(b"ErrorReport", &[&text, &hex(number)]);
        if let Ok(line) = line {
            if let Err(Fail::Unwind(unwind)) = self.write_line(&line) {
                return unwind;
            }
        }
        Unwind::Ended(Ended::Error {
            number,
            block: buffer.wrapping_add(4),
        })
    }

    /// `OS_BreakPt`: the User registers in the breakpoint buffer, its PC
    /// the break point's own address; the stacks emptied, and the
    /// breakpoint handler entered in Supervisor mode.
    pub(crate) fn break_point(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let slot = self.world.env.slot(BREAKPOINT);
        let state = self.world.processor.state();
        let mut user = state.user;
        user[..13].copy_from_slice(&regs.r);
        let (words, len) = dump_words(&user, self.world.swi_at, state.cpsr, slot.bits);
        for (i, &word) in (0..).zip(&words[..len]) {
            self.write_word(slot.buffer.wrapping_add(4 * i), word)?;
        }
        let mut state = self.emptied_stacks(state);
        let mode = match slot.bits {
            Bits::Bit26 => Mode::Supervisor26,
            Bits::Bit32 => Mode::Supervisor,
        };
        state.cpsr = mode.bits() | (state.cpsr & psr::NZCV);
        state.banked[1][1] = state.pc;
        state.user[12] = slot.workspace;
        state.pc = slot.address;
        let transfer = Transfer {
            bits: slot.bits,
            state,
        };
        Err(Fail::Unwind(self.transfer(transfer)))
    }

    /// The breakpoint handler's default: where it stopped printed, and an
    /// exit.
    fn default_breakpoint(&mut self) -> Result<(), Unwind> {
        let buffer = self.world.env.slot(BREAKPOINT).buffer;
        let pc = self.peek_word(buffer.wrapping_add(60)).unwrap_or(0);
        if let Ok(line) = self.message_with(b"BreakpointReached", &[&hex8(pc)]) {
            if let Err(Fail::Unwind(unwind)) = self.write_line(&line) {
                return Err(unwind);
            }
        }
        let mut regs = Regs::default();
        match self.exit(&mut regs) {
            Err(Fail::Unwind(unwind)) => Err(unwind),
            Err(Fail::Error(block)) => self.raise(block),
            Ok(()) => Ok(()),
        }
    }

    /// The legacy callback handler's default: the registers it saved are
    /// loaded back, resuming where the callback struck.
    fn default_callback(&mut self) -> Result<(), Unwind> {
        let buffer = self.world.processor.cpu().reg(Reg::LR);
        let dumped = self.world.env.slot(CALLBACK).bits;
        let len = match dumped {
            Bits::Bit26 => 16,
            Bits::Bit32 => 17,
        };
        let mut words = [0; 17];
        for (i, word) in (0..).zip(words[..len].iter_mut()) {
            *word = self
                .peek_word(buffer.wrapping_add(4 * i))
                .ok_or(Unwind::Ended(Ended::Fault))?;
        }
        let mut state = self.world.processor.state();
        state.user.copy_from_slice(&words[..15]);
        (state.pc, state.cpsr) = match dumped {
            Bits::Bit26 => (
                words[15] & psr::R15_PC,
                psr::from_r15(state.cpsr, words[15]),
            ),
            Bits::Bit32 => (words[15], words[16]),
        };
        let bits = if Mode::from_bits(state.cpsr).is_some_and(Mode::is_26bit) {
            Bits::Bit26
        } else {
            self.world.processor.bits()
        };
        Err(self.transfer(Transfer { bits, state }))
    }

    /// A host routine entered as a subroutine: `work` given the caller's R0
    /// to R12 and flags, which it answers in, V set and R0 the block if it
    /// fails, then a return through R14.
    pub(crate) fn as_subroutine(
        &mut self,
        work: impl FnOnce(&mut Self, &mut Regs) -> Result<(), Fail>,
    ) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let mut regs = Regs::default();
        for (n, slot) in (0..).zip(regs.r.iter_mut()) {
            *slot = cpu.reg(Reg::field(n));
        }
        regs.flags = cpu.cpsr() & psr::NZCV;
        let result = work(self, &mut regs);
        let mut flags = regs.flags & !psr::V;
        match result {
            Ok(()) => {}
            Err(Fail::Error(block)) => {
                regs.r[0] = block;
                flags |= psr::V;
            }
            Err(Fail::Unwind(unwind)) => return Err(unwind),
        }
        let cpu = self.world.processor.cpu();
        for (n, &value) in (0..).zip(&regs.r) {
            cpu.set_reg(Reg::field(n), value);
        }
        cpu.set_nzcv(flags);
        self.return_to_lr();
        Ok(())
    }

    /// Returns from a handler called like a subroutine, as `MOV PC, R14`.
    pub(crate) fn return_to_lr(&mut self) {
        let cpu = self.world.processor.cpu();
        let lr = cpu.reg(Reg::LR);
        let target = if cpu.mode().is_26bit() {
            lr & psr::R15_PC
        } else {
            lr
        };
        cpu.set_reg(Reg::PC, target);
    }

    /// A default handler's host routine, entered as its slot's code is.
    pub(crate) fn default_handler(&mut self, handler: u32) -> Result<(), Unwind> {
        match handler {
            UNDEFINED | PREFETCH_ABORT | DATA_ABORT | ADDRESS_EXCEPTION => {
                self.default_exception(handler)
            }
            ERROR => Err(self.default_error()),
            CALLBACK => self.default_callback(),
            BREAKPOINT => self.default_breakpoint(),
            ESCAPE | EVENT | UPCALL => {
                self.return_to_lr();
                Ok(())
            }
            EXIT => Err(self.default_exit()),
            UNUSED_SWI => {
                let number = self.world.processor.cpu().reg(Reg::field(11));
                let name = hex(number & !crate::swi::X);
                let block = match self.error_with(NO_SUCH_SWI, &[&name]) {
                    Fail::Error(block) => block,
                    Fail::Unwind(unwind) => return Err(unwind),
                };
                let cpu = self.world.processor.cpu();
                cpu.set_reg(Reg::field(0), block);
                cpu.set_nzcv(cpu.cpsr() | psr::V);
                self.return_to_lr();
                Ok(())
            }
            _ => {
                self.undefined_here();
                Ok(())
            }
        }
    }

    /// The exception handlers' default, entered in the exception's mode: the
    /// registers dumped, and the exception's error raised.
    fn default_exception(&mut self, handler: u32) -> Result<(), Unwind> {
        let state = self.world.processor.state();
        let mode = Mode::from_bits(state.cpsr).unwrap_or(Mode::Undefined);
        let bank = mode.bank();
        let saved = self.world.processor.cpu().spsr().unwrap_or(state.cpsr);
        let return_address = self.world.processor.cpu().reg(Reg::LR);
        let interrupted = Mode::from_bits(saved).unwrap_or(Mode::User);
        let mut registers = state.user;
        // The interrupted mode's R13 and R14, but where it shares the
        // exception's bank, whose R14 the exception took for its return.
        [registers[13], registers[14]] = match interrupted.bank() {
            tairix_riscos_arm::psr::Bank::User => [state.user[13], state.user[14]],
            tairix_riscos_arm::psr::Bank::Fiq => [state.fiq[5], state.fiq[6]],
            other if other == bank => [state.banked[other.index() - 2][0], 0],
            other => state.banked[other.index() - 2],
        };
        let slot = self.world.env.slot(EXCEPTION_DUMP);
        let (words, len) = dump_words(&registers, return_address, saved, slot.bits);
        let size = 4 * len as u64;
        let dump = if slot.address.is_multiple_of(4)
            && self.writable(slot.address, u64::from(slot.address) + size)
        {
            slot.address
        } else {
            let fallback = self.world.env.buffers.dump;
            self.world.env.slots[EXCEPTION_DUMP as usize].address = fallback;
            fallback
        };
        for (i, &word) in (0..).zip(&words[..len]) {
            let _ = self.poke_word(dump + 4 * i, word);
        }
        let thumb = saved & psr::T != 0;
        let (error, rewind) = match handler {
            UNDEFINED => (UNDEFINED_INSTRUCTION, if thumb { 2 } else { 4 }),
            PREFETCH_ABORT => (INSTRUCTION_ABORT, 4),
            DATA_ABORT => (DATA_ABORT_ERROR, 8),
            _ => (ADDRESS_EXCEPTION_ERROR, 0),
        };
        let at = hex8(return_address.wrapping_sub(rewind));
        let block = match self.error_with(error, &[&at]) {
            Fail::Error(block) => block,
            Fail::Unwind(unwind) => return Err(unwind),
        };
        let state = self.emptied_stacks(state);
        self.world.frame = Some(crate::os::Frame {
            swi: None,
            return_pc: return_address,
            upper: [words[10], words[11], words[12]],
        });
        let user = match interrupted {
            Mode::User26 | Mode::Fiq26 | Mode::Irq26 | Mode::Supervisor26 => Mode::Supervisor26,
            _ => Mode::Supervisor,
        };
        let mut state = state;
        state.cpsr = user.bits() | (saved & psr::NZCV);
        let bits = self.world.processor.bits();
        if self.world.processor.load(bits, &state).is_err() {
            return Err(Unwind::Ended(Ended::Fault));
        }
        let raised = self.raise(block);
        self.world.frame = None;
        raised?;
        // ErrorV swallowed it: carry on where the exception struck.
        let mut state = self.world.processor.state();
        state.cpsr = saved;
        state.pc = return_address.wrapping_sub(rewind);
        Err(self.transfer(Transfer {
            bits: self.world.processor.bits(),
            state,
        }))
    }

    /// The pending abort of an access the kernel made for the SWI at
    /// `world.swi_at`: taken as if the SWI's instruction had aborted on its
    /// data.
    pub(crate) fn aborted(&mut self, address: u32) -> Fail {
        let at = self.world.swi_at;
        let state = self.world.processor.state();
        self.world.last_abort = crate::os::Abort {
            pc: at,
            psr: state.cpsr,
            address,
        };
        let slot = self.world.env.slot(DATA_ABORT);
        let mut entered = state;
        let bits = if self.world.env.is_default(DATA_ABORT) {
            self.world.processor.bits()
        } else {
            slot.bits
        };
        entered.spsr[3] = state.cpsr;
        entered.banked[2][1] = at.wrapping_add(8);
        entered.cpsr = Mode::Abort.bits() | psr::I | (state.cpsr & psr::NZCV);
        entered.pc = slot.address;
        Fail::Unwind(self.transfer(Transfer {
            bits,
            state: entered,
        }))
    }

    /// The SWI's call nests past the bound: the supervisor stack overflows,
    /// which aborts at its guard page.
    pub(crate) fn overflowed(&mut self) -> Unwind {
        let guard = self.memory.layout().stacks.supervisor.start - 4;
        match self.aborted(guard) {
            Fail::Unwind(unwind) => unwind,
            Fail::Error(_) => Unwind::Ended(Ended::Fault),
        }
    }
}

impl Os<'_> {
    /// An FPA instruction met an exception its trap is enabled for: its
    /// error raised as FPEmulator raises it.
    pub(crate) fn floating_point(
        &mut self,
        exception: tairix_riscos_arm::fpa::Exception,
    ) -> Result<(), Unwind> {
        use tairix_riscos_arm::fpa::Exception as E;
        let (offset, token) = match exception {
            E::InvalidOperation => (0, "FloatingInvalidOperation"),
            E::Overflow => (1, "FloatingOverflow"),
            E::DivisionByZero => (2, "FloatingDivideByZero"),
            E::Underflow => (3, "FloatingUnderflow"),
            E::Inexact => (4, "FloatingInexact"),
        };
        let error = Error {
            number: FLOATING_POINT + offset,
            token,
        };
        match self.error(error) {
            Fail::Error(block) => self.raise(block),
            Fail::Unwind(unwind) => Err(unwind),
        }
    }
}

/// The floating-point errors' first number.
const FLOATING_POINT: u32 = 0x8000_0200;

/// "Undefined instruction executed at &%0".
const UNDEFINED_INSTRUCTION: Error = Error {
    number: 0x8000_0000,
    token: "UndefinedInstructionAt",
};
/// "Instruction fetch aborted at &%0".
const INSTRUCTION_ABORT: Error = Error {
    number: 0x8000_0001,
    token: "PrefetchAbortAt",
};
/// "Data transfer aborted at &%0".
const DATA_ABORT_ERROR: Error = Error {
    number: 0x8000_0002,
    token: "DataAbortAt",
};
/// "Address exception raised at &%0".
const ADDRESS_EXCEPTION_ERROR: Error = Error {
    number: 0x8000_0003,
    token: "AddressExceptionAt",
};
