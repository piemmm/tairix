//! The handlers a client's library sets: an exception or an OS error taken
//! as a trap; an escape or event searched for among the languages at the
//! callback it asks for, each language's fast event procedure called at
//! the event itself; the program's own exit finalising it first; and a new
//! application's upcall finalising it and putting its parent's handlers
//! back, every upcall passed on.
//!
//! As RISC OS 5.30's do, events go to no handler but the languages'.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::error::exception;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{Fail, Os, Regs, Target};
use tairix_riscos_kernel::swi::{os as kos, X};
use tairix_riscos_kernel::{ExceptionKind, Registers};

use super::environment;
use super::exit::{self, Then};
use super::frame::{code_address, readable};
use super::language::{self, word};
use super::os::note;
use super::statics::{at, Statics};
use super::trap::{self, Cause, Entering};
use crate::stack_chunk;

/// The trap codes of the four exceptions, in their trampolines' order.
const EXCEPTION_CODES: [u32; 4] = [
    exception::UNDEFINED_INSTRUCTION,
    exception::PREFETCH_ABORT,
    exception::DATA_ABORT,
    exception::ADDRESS_EXCEPTION,
];
/// How far before an exception's return address the instruction lies.
const REWIND: [u32; 4] = [4, 4, 8, 4];
const KINDS: [ExceptionKind; 4] = [
    ExceptionKind::Undefined,
    ExceptionKind::PrefetchAbort,
    ExceptionKind::DataAbort,
    ExceptionKind::AddressException,
];
/// Where the trampoline pushed R12 and the exception's R14, on the
/// exception's stack.
const PUSHED_R12: u32 = 4 * 5;
const PUSHED_R14: u32 = 4 * 6;

/// Exception `n` taken through its trampoline: a trap at the instruction it
/// struck, entered in User mode.
fn exception(os: &mut Os<'_>, regs: &mut Regs, n: usize) -> Result<(), Fail> {
    let statics = Statics::of_base(regs.r[12]);
    let pushed = os.registers().r[13];
    let mut dumped = os.interrupted();
    dumped.r[12] = os.as_caller().read_word(pushed.wrapping_add(PUSHED_R12))?;
    let returned = os.as_caller().read_word(pushed.wrapping_add(PUSHED_R14))?;
    let pc = returned.wrapping_sub(REWIND[n]);
    dumped.r[15] = pc;
    if let Fail::Error(block) = os.exception_error(KINDS[n], pc) {
        note(os, statics, block)?;
    }
    trap::raise(
        os,
        Cause::Trap,
        EXCEPTION_CODES[n],
        dumped,
        Entering::Privileged,
    )
}

pub(super) fn undefined(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    exception(os, regs, 0)
}

pub(super) fn prefetch_abort(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    exception(os, regs, 1)
}

pub(super) fn data_abort(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    exception(os, regs, 2)
}

pub(super) fn address_exception(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    exception(os, regs, 3)
}

/// The error handler: an OS error a SWI without its X bit raised, taken as
/// a trap whose code is the error's number, at the SWI.
pub(super) fn error(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = Statics::of_base(regs.r[0]);
    let block = statics.field(at::LAST_ERROR);
    statics.write(os, at::ERROR_PENDING, 1)?;
    let code = os.as_caller().read_word(block)?;
    let mut dumped = os.registers();
    dumped.r[0] = statics.base();
    dumped.r[15] = statics.read(os, at::ERROR_BUFFER)?.wrapping_sub(4);
    trap::raise(os, Cause::Trap, code, dumped, Entering::User)
}

/// The Exit handler: the program's own `OS_Exit`, its languages finalised
/// first, then ended with the return code that call set, which the kernel
/// enters the handler with in R2.
pub(super) fn exit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = Statics::of_base(regs.r[12]);
    exit::finalise(os, statics, Then::Exited, regs.r[2])
}

/// What the next callback dispatches.
mod pending {
    pub(super) const ESCAPE: u32 = 1;
    pub(super) const EVENT: u32 = 2;
}
/// The escape handler's R11 bit saying the escape condition is set.
const ESCAPE_SET: u32 = 0x40;
/// What a handler leaves in R12 to ask the kernel for a callback.
const CALLBACK_WANTED: u32 = 1;
/// The event registers the fast procedures are given: R0 to R9.
pub(crate) const EVENT_WORDS: u32 = 10;
/// A callback's buffer: R0 to R15, then the CPSR for a 32-bit client.
pub(crate) const CALLBACK_WORDS: u32 = 17;

/// The Escape handler: the escape seen, and the languages' procedures asked
/// of the callback it asks for.
pub(super) fn escape(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[11] & ESCAPE_SET == 0 {
        return Ok(());
    }
    let statics = Statics::of_base(regs.r[12]);
    statics.write(os, at::ESCAPE_SEEN, 1)?;
    let waiting = statics.read(os, at::PENDING)?;
    statics.write(os, at::PENDING, waiting | pending::ESCAPE)?;
    regs.r[12] = CALLBACK_WANTED;
    Ok(())
}

/// The Event handler: each language's fast event procedure called now with
/// the event's R0 to R9, and the event dispatched at the callback it asks
/// for.
pub(super) fn event(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = Statics::of_base(regs.r[12]);
    let block = statics.field(at::EVENT_REGISTERS);
    for (n, &value) in (0..EVENT_WORDS).zip(&regs.r) {
        os.as_caller()
            .write_word(block.wrapping_add(4 * n), value)?;
    }
    let entry = os.event_entry();
    let bits = os.caller_bits();
    let mut walk = language::languages(os, statics)?;
    while let Some(found) = walk.next(os)? {
        if let Some(procedure) = found.procedure(os, word::FAST_EVENT)? {
            let mut fast = Regs::default();
            fast.r[0] = block;
            os.call(
                Target {
                    address: procedure,
                    bits,
                },
                &mut fast,
                entry,
            )?;
        }
    }
    statics.write(os, at::PENDING_EVENT, regs.r[0])?;
    let waiting = statics.read(os, at::PENDING)?;
    statics.write(os, at::PENDING, waiting | pending::EVENT)?;
    regs.r[12] = CALLBACK_WANTED;
    Ok(())
}

/// The callback handler: an escape — acknowledged first — or else an event
/// searched for among the languages from the User registers the callback
/// struck, where SL names a stack to run a procedure on and no search for
/// one is under way; otherwise those registers resumed. An event behind an
/// escape waits for the next callback.
pub(super) fn callback(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = Statics::of_base(regs.r[12]);
    let bits = os.caller_bits();
    let buffer = os.registers().r[14];
    let struck = struck(os, buffer, bits)?;
    let waiting = statics.read(os, at::PENDING)?;
    let dispatch = if waiting & pending::ESCAPE != 0 {
        let mut acknowledge = Regs::default();
        acknowledge.r[0] = tairix_riscos_kernel::input::ACKNOWLEDGE_ESCAPE;
        os.perform(X | kos::BYTE, &mut acknowledge)?;
        statics.write(os, at::PENDING, waiting & !pending::ESCAPE)?;
        if waiting & pending::EVENT != 0 {
            os.perform(X | kos::SET_CALLBACK, &mut Regs::default())?;
        }
        Some((Cause::Escape, trap::ESCAPE))
    } else if waiting & pending::EVENT != 0 {
        statics.write(os, at::PENDING, 0)?;
        Some((Cause::Event, statics.read(os, at::PENDING_EVENT)?))
    } else {
        None
    };
    if let Some((cause, code)) = dispatch {
        if has_stack(os, struck.r[10]) && !trap::searching(os, statics, cause)? {
            return trap::raise(os, cause, code, struck, Entering::Privileged);
        }
    }
    let mut user = [0; 15];
    user.copy_from_slice(&struck.r[..15]);
    Err(os.resume_in_user(bits, struck.r[15], user, struck.flags))
}

/// The User registers a callback struck, from its buffer: R0 to R15, the
/// PSR in R15 for a 26-bit client, in the word after for a 32-bit one.
fn struck(os: &mut Os<'_>, buffer: u32, bits: Bits) -> Result<Registers, Fail> {
    let mut registers = Registers::default();
    for (n, slot) in (0..).zip(registers.r.iter_mut()) {
        *slot = os.as_caller().read_word(buffer.wrapping_add(4 * n))?;
    }
    let status = match bits {
        Bits::Bit26 => registers.r[15],
        Bits::Bit32 => os
            .as_caller()
            .read_word(buffer.wrapping_add(4 * (CALLBACK_WORDS - 1)))?,
    };
    registers.r[15] = code_address(bits, registers.r[15]);
    registers.flags = status & psr::NZCV;
    Ok(registers)
}

/// Whether `sl` lies above a stack chunk a procedure can run on.
fn has_stack(os: &mut Os<'_>, sl: u32) -> bool {
    readable(os, stack_chunk::of_limit(sl)) == Some(stack_chunk::MARK)
}

/// The upcall handler: a new application's upcall finalising the program
/// and putting its parent's handlers back; every upcall passed on to the
/// handler in force before the library's.
pub(super) fn up_call(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = Statics::of_base(regs.r[12]);
    if regs.r[0] == kos::upcall::NEW_APPLICATION {
        exit::finalise_returning(os, statics)?;
        environment::restore(os, statics)?;
    }
    environment::pass_up_call(os, statics, regs)
}
