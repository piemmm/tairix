//! Ending a client: each language's finalisation procedure called in turn,
//! the handlers put back, and the program ended as the call asked — an exit
//! with the return code, an error for the parent, or the program's own exit
//! carried on. An exit while the procedures are being called skips them.

use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs, Target};
use tairix_riscos_kernel::swi::os::{self as kos, exit::ABEX};

use super::environment;
use super::init;
use super::language;
use super::statics::{at, Statics};
use super::trap::{self, Cause};
use super::{context, Routine};
use crate::c::stdio::stream;
use crate::stack_chunk;

/// What follows a finalisation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Then {
    /// `OS_Exit` with the return code `_kernel_setreturncode` gave.
    Exit,
    /// The error whose block the finalisation was started with, for the
    /// program's parent.
    Error,
    /// The program's own `OS_Exit`, carried on to its parent with the
    /// return code the finalisation was started with.
    Exited,
}

impl Then {
    const fn word(self) -> u32 {
        match self {
            Self::Exit => 0,
            Self::Error => 1,
            Self::Exited => 2,
        }
    }

    const fn of(word: u32) -> Self {
        match word {
            1 => Self::Error,
            2 => Self::Exited,
            _ => Self::Exit,
        }
    }
}

/// A procedure a new application's upcall finalises with runs this far
/// below the root chunk's top, as RISC OS 5.30's do.
const RETURNING_STACK_BELOW_TOP: u32 = 4;

/// `_kernel_exit()`.
pub(super) fn exit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    finalise(os, statics, Then::Exit, 0)
}

/// The run procedure has returned: the program ends as `_kernel_exit` ends
/// it.
pub(super) fn run_returned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    exit(os, regs)
}

/// `_kernel_setreturncode(code)`.
pub(super) fn setreturncode(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    statics.write(os, at::RETURN_CODE, regs.r[0])
}

/// `_kernel_exittraphandler()`: the trap, escape or event being handled
/// taken as over, its procedure having resumed the program itself.
pub(super) fn exittraphandler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    trap::abandon(os, statics, Cause::Trap)?;
    trap::abandon(os, statics, Cause::Event)
}

/// `_kernel_raise_error(error)`: the program finalised, then the error
/// raised for its parent.
pub(super) fn raise_error(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    finalise(os, statics, Then::Error, regs.r[0])
}

/// The finalisation procedures called in turn, then the program ended as
/// `then` says, `with` its error where it is one. Asked while one is under
/// way, the procedures are skipped.
pub(crate) fn finalise(
    os: &mut Os<'_>,
    statics: Statics,
    then: Then,
    with: u32,
) -> Result<(), Fail> {
    statics.write(os, at::FINAL_THEN, then.word())?;
    statics.write(os, at::FINAL_WITH, with)?;
    if statics.read(os, at::FINAL_NEXT)? != 0 {
        return ended(os, statics);
    }
    let first = statics.read(os, at::LANGUAGES)?;
    statics.write(os, at::FINAL_NEXT, first.max(1))?;
    next_final(os, statics)
}

/// Each finalisation procedure called and returned from in turn, on the
/// root chunk's top, as a new application's upcall calls them; skipped
/// while a finalisation is under way.
pub(crate) fn finalise_returning(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    if statics.read(os, at::FINAL_NEXT)? != 0 {
        return Ok(());
    }
    statics.write(os, at::FINAL_NEXT, 1)?;
    let sp = statics
        .read(os, at::EXTENSION_STACK)?
        .wrapping_sub(RETURNING_STACK_BELOW_TOP);
    let sl = stack_chunk::limit_of(statics.read(os, at::ROOT_CHUNK)?);
    let bits = os.caller_bits();
    let mut walk = language::languages(os, statics)?;
    while let Some(language) = walk.next(os)? {
        if let Some(procedure) = language.finalisation(os)? {
            let mut regs = Regs::default();
            regs.r[9] = statics.base();
            regs.r[10] = sl;
            os.call(
                Target {
                    address: procedure,
                    bits,
                },
                &mut regs,
                Entry::User(sp),
            )?;
        }
    }
    statics.write(os, at::FINAL_NEXT, 0)
}

/// A finalisation procedure has returned: the next one called.
pub(super) fn final_returned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    next_final(os, statics)
}

fn next_final(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    let next = statics.read(os, at::FINAL_NEXT)?;
    let mut walk = language::from(os, statics, next)?;
    while let Some(language) = walk.next(os)? {
        statics.write(os, at::FINAL_NEXT, walk.position().max(1))?;
        if let Some(procedure) = language.finalisation(os)? {
            let mut registers = init::entered(os, statics)?;
            registers.r[14] = context::routine(os, Routine::FinaliseReturned)?;
            registers.r[15] = procedure;
            os.leave_with(registers);
            return Ok(());
        }
    }
    statics.write(os, at::FINAL_NEXT, 0)?;
    ended(os, statics)
}

/// The handlers put back and the program ended as the finalisation was
/// asked to end it.
fn ended(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    environment::restore(os, statics)?;
    match Then::of(statics.read(os, at::FINAL_THEN)?) {
        Then::Error => {
            let block = statics.read(os, at::FINAL_WITH)?;
            generate(os, Fail::Error(block))
        }
        Then::Exit => exit_with_return_code(os, statics),
        Then::Exited => {
            let code = statics.read(os, at::FINAL_WITH)?;
            exit_with(os, code)
        }
    }
}

/// The program ended with no finalisation procedure called: the handlers
/// put back, then `OS_Exit` with its return code.
pub(crate) fn end_unfinalised(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    environment::restore(os, statics)?;
    exit_with_return_code(os, statics)
}

/// `OS_Exit` with the return code `_kernel_setreturncode` gave.
pub(crate) fn exit_with_return_code(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    let code = statics.read(os, at::RETURN_CODE)?;
    exit_with(os, code)
}

/// `OS_Exit` with the return code `code`.
fn exit_with(os: &mut Os<'_>, code: u32) -> Result<(), Fail> {
    let mut regs = stream::registers(&[0, ABEX, code]);
    os.perform(kos::EXIT, &mut regs)
}

/// `failed`'s error raised as `OS_GenerateError` raises one: to the error
/// handler in force. Where a claimant of the error vector swallows it, the
/// program ends, nothing left to go on with.
pub(crate) fn generate(os: &mut Os<'_>, failed: Fail) -> Result<(), Fail> {
    let block = match failed {
        Fail::Error(block) => block,
        unwind @ Fail::Unwind(_) => return Err(unwind),
    };
    os.raise(block)?;
    os.perform(kos::EXIT, &mut Regs::default())
}
