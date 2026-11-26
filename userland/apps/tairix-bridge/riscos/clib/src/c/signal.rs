//! `signal.h` and `assert.h`: each signal's handler as `signal` sets it and
//! `raise` calls it, and the procedures the C library gives the library
//! kernel, turning a trap or an escape into its signal — with the default
//! actions as RISC OS 5.30's take them: a message and a postmortem
//! backtrace on `stderr`, then the program's end.
//!
//! A handler is entered as a continuation below a frame record of the
//! library's, so one that never returns — a `longjmp` — leaves nothing of
//! the library behind; the one that does returns through a routine that
//! goes on as the delivery asked.

use alloc::vec::Vec;

use tairix_riscos_kernel::error::exception;
use tairix_riscos_kernel::os::{quietly, Fail, Os, Regs, Unwind};
use tairix_riscos_kernel::Registers;

use super::backtrace::{self, Heading};
use super::errno::{self, ESIGNUM};
use super::report;
use super::statics::{at, SIGNAL_COUNT};
use crate::chunk;
use crate::kernel::frame::{code_address, returning, SAVED_FP, SAVED_LR, SAVED_SP};
use crate::kernel::os::last_error_text;
use crate::kernel::statics::{at as kernel_at, Statics};
use crate::kernel::trap::{self, DECLINED, HANDLED, READ_FAIL, WRITE_FAIL};
use crate::kernel::{context, template, Routine};
use crate::module::{message, message_with};

pub(crate) const SIGABRT: u32 = 1;
pub(crate) const SIGFPE: u32 = 2;
pub(crate) const SIGILL: u32 = 3;
pub(crate) const SIGINT: u32 = 4;
pub(crate) const SIGSEGV: u32 = 5;
pub(crate) const SIGTERM: u32 = 6;
pub(crate) const SIGSTAK: u32 = 7;
pub(crate) const SIGOSERROR: u32 = 10;

/// The handlers' marker values, as RISC OS 5.30 has them.
pub(crate) const SIG_DFL: u32 = u32::MAX;
pub(crate) const SIG_ERR: u32 = SIG_DFL - 1;
pub(crate) const SIG_IGN: u32 = SIG_DFL - 2;

/// The code an escape is searched for by.
const ESCAPE: u32 = trap::ESCAPE;

/// The signal a trap's code is: an arithmetic one for a division by nought
/// or a floating-point error, an illegal instruction, a bad address, an
/// unextendable stack, or else an OS error.
pub(crate) const fn signal_of(code: u32) -> u32 {
    match code {
        trap::DIVIDE_BY_ZERO | exception::FLOATING_POINT..=exception::FLOATING_POINT_LAST => SIGFPE,
        exception::UNDEFINED_INSTRUCTION
        | exception::PREFETCH_ABORT
        | exception::BRANCH_THROUGH_ZERO => SIGILL,
        exception::DATA_ABORT | exception::ADDRESS_EXCEPTION | READ_FAIL | WRITE_FAIL => SIGSEGV,
        trap::STACK_OVERFLOW => SIGSTAK,
        _ => SIGOSERROR,
    }
}

/// The statics field holding `sig`'s handler; none for a signal there is
/// not.
const fn slot(sig: u32) -> Option<u32> {
    if sig >= 1 && sig <= SIGNAL_COUNT {
        Some(at::SIGNALS + 4 * (sig - 1))
    } else {
        None
    }
}

/// Every signal's handler the default, as the library starts.
pub(crate) fn start(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    for sig in 1..=SIGNAL_COUNT {
        if let Some(field) = slot(sig) {
            statics.write(os, field, SIG_DFL)?;
        }
    }
    Ok(())
}

/// `signal(sig, func)`: the handler it replaces; `SIG_ERR` for a signal
/// there is not, `errno` untouched as RISC OS 5.30 leaves it.
pub(super) fn signal(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let Some(field) = slot(regs.r[0]) else {
        regs.r[0] = SIG_ERR;
        return Ok(());
    };
    let previous = statics.read(os, field)?;
    statics.write(os, field, regs.r[1])?;
    regs.r[0] = previous;
    Ok(())
}

/// Where a handler's return goes on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Then {
    /// `raise` answers nought to its caller.
    Raise,
    /// `abort`'s signal handled: the program still ends.
    Abort,
    /// The trap `code`, whose registers are at `set`, ends the program with
    /// a backtrace from them.
    Trap { code: u32, set: u32 },
    /// The escape is handled: the library kernel goes on.
    Event,
}

impl Then {
    const fn routine(self) -> Routine {
        match self {
            Self::Raise => Routine::RaiseReturned,
            Self::Abort => Routine::AbortReturned,
            Self::Trap { .. } => Routine::TrapSignalReturned,
            Self::Event => Routine::EventSignalReturned,
        }
    }
}

/// The frame record a handler is entered below: the caller's FP, SP and
/// return, then a library address past a frame-storing instruction; for a
/// trap, its code and register set below them.
const RECORD_WORDS: u32 = 4;
const TRAP_WORDS: u32 = 2;

/// `sig` delivered as `raise` delivers it, from the routine whose caller's
/// registers are `caller`: ignored, its default action taken, or its handler
/// entered — reset to the default first. What the routine answers, where it
/// returns at all.
fn deliver(os: &mut Os<'_>, statics: Statics, sig: u32, then: Then) -> Result<Option<u32>, Fail> {
    let Some(field) = slot(sig) else {
        return Ok(Some(DECLINED));
    };
    match statics.read(os, field)? {
        SIG_IGN => handled(os, statics, then),
        SIG_DFL => {
            default_action(os, statics, sig, then)?;
            Ok(None)
        }
        handler => {
            statics.write(os, field, SIG_DFL)?;
            enter(os, sig, handler, then)?;
            Ok(None)
        }
    }
}

/// The handler at `handler` entered with `sig` below the library's frame
/// record, its return the routine `then` names.
fn enter(os: &mut Os<'_>, sig: u32, handler: u32, then: Then) -> Result<(), Fail> {
    let mut registers = os.registers();
    let sp = registers.r[13];
    let record = sp.wrapping_sub(4 * RECORD_WORDS);
    let pc = template(os)?.wrapping_add(chunk::RECORD_PC);
    os.as_caller()
        .write_words(record, &[registers.r[11], sp, registers.r[14], pc])?;
    let mut base = record;
    if let Then::Trap { code, set } = then {
        base = record.wrapping_sub(4 * TRAP_WORDS);
        os.as_caller().write_words(base, &[code, set])?;
    }
    registers.r[0] = sig;
    registers.r[11] = sp.wrapping_sub(4);
    registers.r[13] = base;
    registers.r[14] = context::routine(os, then.routine())?;
    registers.r[15] = code_address(os.caller_bits(), handler);
    os.leave_with(registers);
    Ok(())
}

/// What follows a signal handled, its handler returned or the signal
/// ignored: the answer the delivering routine gives, where it gives one.
fn handled(os: &mut Os<'_>, statics: Statics, then: Then) -> Result<Option<u32>, Fail> {
    match then {
        Then::Raise => Ok(Some(0)),
        Then::Event => Ok(Some(HANDLED)),
        Then::Abort => {
            default_action(os, statics, SIGABRT, Then::Raise)?;
            Ok(None)
        }
        Then::Trap { code, set } => {
            trap_backtrace(os, statics, code, set)?;
            report::end_failing(os, statics)?;
            Ok(None)
        }
    }
}

/// The backtrace from the trap `code`'s registers at `set`, none for a set
/// of nought.
fn trap_backtrace(os: &mut Os<'_>, statics: Statics, code: u32, set: u32) -> Result<(), Fail> {
    let (pc, fp, address) = if set == 0 {
        (0, 0, 0)
    } else {
        let mut words = [0; 16];
        os.as_caller().read_words(set, &mut words)?;
        (
            code_address(os.caller_bits(), words[15]),
            words[11],
            words[0],
        )
    };
    let heading = match code {
        READ_FAIL | WRITE_FAIL => Heading::Address(address),
        _ => Heading::Postmortem,
    };
    backtrace::write(os, statics, heading, pc, fp)
}

/// `sig`'s default action: its message on `stderr` — the last error's text
/// where the signal is that error's — and a backtrace, then the program's
/// end. An unextendable stack ends it silently, an interrupt without the
/// backtrace.
fn default_action(os: &mut Os<'_>, statics: Statics, sig: u32, then: Then) -> Result<(), Fail> {
    let Some(text) = default_message(os, statics, sig)? else {
        return report::end_failing(os, statics);
    };
    report::to_stderr(os, statics, &[&b"\n"[..], &text, b"\n"].concat())?;
    if sig != SIGINT {
        match then {
            Then::Trap { code, set } => trap_backtrace(os, statics, code, set)?,
            Then::Raise | Then::Abort | Then::Event => {
                let caller = os.registers();
                let pc = code_address(os.caller_bits(), caller.r[14]);
                backtrace::write(os, statics, Heading::Postmortem, pc, caller.r[11])?;
            }
        }
    }
    report::end_failing(os, statics)
}

/// The message `sig`'s default action writes: the last error's text for an
/// arithmetic or OS error signal, and for a bad address or instruction whose
/// error it is; none for an unextendable stack.
fn default_message(os: &mut Os<'_>, statics: Statics, sig: u32) -> Result<Option<Vec<u8>>, Fail> {
    let last = statics.read(os, kernel_at::LAST_ERROR)?;
    Ok(Some(match sig {
        SIGSTAK => return Ok(None),
        SIGFPE | SIGOSERROR => last_error_text(os, statics)?,
        SIGSEGV | SIGILL if signal_of(last) == sig => last_error_text(os, statics)?,
        SIGSEGV => message(os, "SignalAddress")?,
        SIGILL => message(os, "SignalIllegal")?,
        SIGABRT => message(os, "SignalAbort")?,
        SIGINT => message(os, "SignalInterrupt")?,
        SIGTERM => message(os, "SignalTerminate")?,
        _ => message_with(
            os,
            "SignalUnknown",
            &[tairix_riscos_kernel::Decimal::of(sig.cast_signed()).as_bytes()],
        )?,
    }))
}

/// The delivering routine's answer, where it gives one, put in R0.
fn answer(regs: &mut Regs, delivered: Option<u32>) {
    if let Some(value) = delivered {
        regs.r[0] = value;
    }
}

/// `raise(sig)`: nought once delivered; `ESIGNUM` for a signal there is
/// not, `errno` set to it.
pub(super) fn raise(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let sig = regs.r[0];
    if slot(sig).is_none() {
        errno::set(os, statics, ESIGNUM)?;
        regs.r[0] = ESIGNUM;
        return Ok(());
    }
    let delivered = deliver(os, statics, sig, Then::Raise)?;
    answer(regs, delivered);
    Ok(())
}

/// `abort()`: `SIGABRT` raised, the program ended even where its handler
/// returns.
pub(super) fn abort(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let delivered = deliver(os, statics, SIGABRT, Then::Abort)?;
    answer(regs, delivered);
    Ok(())
}

/// `__assert(reason, file, line)`: the failure on `stderr`, then `abort`.
pub(super) fn assert(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let [reason, file] = [regs.r[0], regs.r[1]].map(|at| assertion_text(os, at));
    let line = tairix_riscos_kernel::Decimal::of(regs.r[2].cast_signed());
    let named: [&[u8]; 3] = [&reason?, &file?, line.as_bytes()];
    report_assertion(os, statics, "AssertFailed", &named)?;
    abort(os, regs)
}

/// `__assert2(reason, function, file, line)`: as `__assert`, the function
/// named too.
pub(crate) fn assert2(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let [reason, function, file] =
        [regs.r[0], regs.r[1], regs.r[2]].map(|at| assertion_text(os, at));
    let line = tairix_riscos_kernel::Decimal::of(regs.r[3].cast_signed());
    let named: [&[u8]; 4] = [&reason?, &function?, &file?, line.as_bytes()];
    report_assertion(os, statics, "AssertFailedIn", &named)?;
    abort(os, regs)
}

/// The longest reason, function or file name an assertion writes.
const TEXT_MOST: usize = 1024;

/// The text an assertion names at `at`, cut to the most it writes; none
/// where the host has no room for it, so a failed assertion aborts all the
/// same. Memory that refuses the read aborts as the read would.
fn assertion_text(os: &mut Os<'_>, at: u32) -> Result<Vec<u8>, Unwind> {
    match os.as_caller().read_until(at, TEXT_MOST, |b| b == 0) {
        Ok((text, _)) => Ok(text),
        Err(Fail::Error(_)) => Ok(Vec::new()),
        Err(Fail::Unwind(unwind)) => Err(unwind),
    }
}

/// A failed assertion's report, its message `token` with `named` in it, on
/// `stderr`: let go where it cannot be made.
fn report_assertion(
    os: &mut Os<'_>,
    statics: Statics,
    token: &str,
    named: &[&[u8]],
) -> Result<(), Unwind> {
    let text = message_with(os, token, named);
    quietly(text.and_then(|text| report::to_stderr(os, statics, &[&text[..], b"\n"].concat())))
}

/// `__ignore_signal_handler(sig)`: the signal ignored from now on, for a
/// stub that names it as a handler.
pub(super) fn ignore_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    if let Some(field) = slot(regs.r[0]) {
        statics.write(os, field, SIG_IGN)?;
    }
    Ok(())
}

/// `__default_signal_handler(sig)`: the signal's default action.
pub(super) fn default_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    default_action(os, statics, regs.r[0], Then::Raise)
}

/// `__error_signal_marker(sig)`: a marker an old stub compared handlers
/// with, doing nothing called.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn error_marker(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

/// The C library's trap procedure: the trap's signal raised where it has a
/// handler; declined where it has the default, for the uncaught procedure.
pub(super) fn trap_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (code, set) = (regs.r[0], regs.r[1]);
    let sig = signal_of(code);
    let delivered = match slot(sig) {
        Some(field) if statics.read(os, field)? != SIG_DFL => {
            deliver(os, statics, sig, Then::Trap { code, set })?
        }
        _ => Some(DECLINED),
    };
    answer(regs, delivered);
    Ok(())
}

/// The C library's uncaught trap procedure: the trap's signal raised, its
/// default action taken where it has the default.
pub(super) fn uncaught_trap_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (code, set) = (regs.r[0], regs.r[1]);
    let delivered = deliver(os, statics, signal_of(code), Then::Trap { code, set })?;
    answer(regs, delivered);
    Ok(())
}

/// The C library's event procedure: an escape raises `SIGINT` where it has
/// a handler; every other event, and an escape with the default, declined.
pub(super) fn event_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let delivered = match slot(SIGINT) {
        Some(field) if regs.r[0] == ESCAPE && statics.read(os, field)? != SIG_DFL => {
            deliver(os, statics, SIGINT, Then::Event)?
        }
        _ => Some(DECLINED),
    };
    answer(regs, delivered);
    Ok(())
}

/// The C library's unhandled event procedure: an escape raises `SIGINT`,
/// its default action taken where it has the default; every other event
/// declined.
pub(super) fn unhandled_event_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let delivered = if regs.r[0] == ESCAPE {
        deliver(os, statics, SIGINT, Then::Event)?
    } else {
        Some(DECLINED)
    };
    answer(regs, delivered);
    Ok(())
}

/// The registers the delivering routine's caller resumes with, from the
/// frame record the returning handler left FP at, R0 `answer`.
fn resumed(os: &mut Os<'_>, answer: u32) -> Result<Registers, Fail> {
    let mut registers = os.registers();
    let fp = registers.r[11];
    let lr = os.as_caller().read_word(fp.wrapping_sub(SAVED_LR))?;
    registers.r[0] = answer;
    registers.r[11] = os.as_caller().read_word(fp.wrapping_sub(SAVED_FP))?;
    registers.r[13] = os.as_caller().read_word(fp.wrapping_sub(SAVED_SP))?;
    registers.r[14] = lr;
    (registers.r[15], registers.flags) = returning(os.caller_bits(), lr, registers.flags);
    Ok(registers)
}

/// A handler `raise` entered has returned: `raise` answers nought.
pub(crate) fn raise_returned(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let registers = resumed(os, 0)?;
    os.leave_with(registers);
    Ok(())
}

/// A handler an escape's procedure entered has returned: the escape
/// handled.
pub(crate) fn event_signal_returned(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let registers = resumed(os, HANDLED)?;
    os.leave_with(registers);
    Ok(())
}

/// `abort`'s handler has returned: the program ends as `SIGABRT`'s default
/// ends it.
pub(crate) fn abort_returned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    default_action(os, statics, SIGABRT, Then::Raise)
}

/// A handler a trap's procedure entered has returned: a backtrace from the
/// trap, and the program's end.
pub(crate) fn trap_signal_returned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let fp = os.registers().r[11];
    let mut kept = [0; TRAP_WORDS as usize];
    os.as_caller()
        .read_words(fp.wrapping_sub(SAVED_FP + 4 * TRAP_WORDS), &mut kept)?;
    let [code, set] = kept;
    trap_backtrace(os, statics, code, set)?;
    report::end_failing(os, statics)
}

#[cfg(test)]
mod tests;
