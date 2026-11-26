//! `_kernel_system(command, chain)` as RISC OS 5.30's: the command run as a
//! sub-program, the caller copied out of its way to the top of memory under
//! a lowered memory limit, the library's own error, exit and upcall handlers
//! taking the sub-program's end and copying the caller back; or the program
//! replaced by the command, its handlers put back first and no language
//! finalised.
//!
//! The current active object is left pointing into the library afterwards,
//! as RISC OS 5.30 leaves it.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{os as kos, X};
use tairix_riscos_kernel::Registers;

use super::environment::{self, handler};
use super::exit;
use super::frame::returning;
use super::init;
use super::os::{note, OS_ERROR};
use super::statics::{at, Statics};
use super::{context, Routine};
use crate::module::{error, raise};

/// What the caller resumes with and where it lies while the sub-program
/// runs, from the statics' system block: R4 to R11, SP, the return and
/// the flags; the memory limit it had; the copy's base and size.
pub(crate) mod kept {
    pub(crate) const V_REGISTERS: u32 = 0;
    pub(crate) const SP: u32 = 32;
    pub(crate) const RETURN: u32 = 36;
    pub(crate) const FLAGS: u32 = 40;
    pub(crate) const LIMIT: u32 = 44;
    pub(crate) const COPY: u32 = 48;
    pub(crate) const SIZE: u32 = 52;
    /// The block's size.
    pub(crate) const BYTES: u32 = 56;
}
/// The variable registers kept: R4 to R11.
const V_FIRST: usize = 4;
const V_COUNT: usize = 8;
/// The copy's granularity.
const COPY_ALIGN: u32 = 16;

/// `_kernel_system(command, chain)`.
pub(crate) fn system(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let command = regs.r[0];
    if regs.r[1] != 0 {
        return chain(os, statics, command);
    }
    let image = statics.read(os, at::IMAGE_BASE)?;
    let top = statics.read(os, at::HEAP)?;
    let [limit, _, _] = environment::change(os, handler::MEMORY_LIMIT, [0; 3])?;
    let size = top
        .checked_sub(image)
        .and_then(|bytes| bytes.checked_next_multiple_of(COPY_ALIGN));
    let Some((size, copy)) = size
        .and_then(|size| Some((size, limit.checked_sub(size)?)))
        .filter(|&(_, copy)| copy >= top)
    else {
        let refused = raise(os, error::NO_MEMORY, &[]);
        return answer_refusal(os, statics, regs, refused);
    };
    keep(os, statics, limit, copy, size)?;
    os.as_caller().copy_bytes(image, copy, size)?;
    let moved = Statics::of_base(statics.base().wrapping_sub(image).wrapping_add(copy));
    environment::restore(os, statics)?;
    let exit_routine = context::routine(os, Routine::SystemExit)?;
    for (number, routine, buffer) in [
        (
            handler::ERROR,
            Routine::SystemError,
            moved.field(at::ERROR_BUFFER),
        ),
        (handler::EXIT, Routine::SystemExit, 0),
        (handler::UPCALL, Routine::SystemUpCall, 0),
    ] {
        let code = context::routine(os, routine)?;
        environment::change(os, number, [code, moved.base(), buffer])?;
    }
    environment::change(os, handler::MEMORY_LIMIT, [copy, 0, 0])?;
    environment::change(os, handler::CURRENT_ACTIVE_OBJECT, [exit_routine, 0, 0])?;
    let mut cli = Regs::default();
    cli.r[0] = command;
    let ran = match os.perform(X | kos::CLI, &mut cli) {
        Err(Fail::Unwind(started)) => return Err(Fail::Unwind(started)),
        ran => ran,
    };
    reinstate(os, statics, limit)?;
    match ran {
        Ok(()) => {
            regs.r[0] = 0;
            Ok(())
        }
        Err(refused) => answer_refusal(os, statics, regs, refused),
    }
}

/// `refused` noted and answered as an OS error, or its unwind carried on.
fn answer_refusal(
    os: &mut Os<'_>,
    statics: Statics,
    regs: &mut Regs,
    refused: Fail,
) -> Result<(), Fail> {
    let Fail::Error(block) = refused else {
        return Err(refused);
    };
    note(os, statics, block)?;
    regs.r[0] = OS_ERROR;
    Ok(())
}

/// The command run in the program's place, its handlers put back first:
/// past a command that returns, the program exits, no language finalised;
/// its error raised to the handlers put back.
fn chain(os: &mut Os<'_>, statics: Statics, command: u32) -> Result<(), Fail> {
    environment::restore(os, statics)?;
    let mut cli = Regs::default();
    cli.r[0] = command;
    if let Err(refused) = os.perform(kos::CLI, &mut cli) {
        return exit::generate(os, refused);
    }
    exit::exit_with_return_code(os, statics)
}

/// What the caller resumes with kept in the statics, which the copy takes
/// with it.
fn keep(os: &mut Os<'_>, statics: Statics, limit: u32, copy: u32, size: u32) -> Result<(), Fail> {
    let registers = os.registers();
    let block = at::SYSTEM;
    for (n, &value) in (0..).zip(&registers.r[V_FIRST..V_FIRST + V_COUNT]) {
        statics.write(os, block + kept::V_REGISTERS + 4 * n, value)?;
    }
    for (word, value) in [
        (kept::SP, registers.r[13]),
        (kept::RETURN, registers.r[14]),
        (kept::FLAGS, registers.flags),
        (kept::LIMIT, limit),
        (kept::COPY, copy),
        (kept::SIZE, size),
    ] {
        statics.write(os, block + word, value)?;
    }
    Ok(())
}

/// The caller's memory limit and the library's handlers back.
fn reinstate(os: &mut Os<'_>, statics: Statics, limit: u32) -> Result<(), Fail> {
    environment::change(os, handler::MEMORY_LIMIT, [limit, 0, 0])?;
    environment::restore(os, statics)?;
    init::set_handlers(os, statics)
}

/// The sub-program has ended, `moved` the statics' copy: the caller copied
/// back and reinstated, and resumed from `_kernel_system` with `answer`.
fn ended(os: &mut Os<'_>, moved: Statics, answer: u32) -> Result<(), Fail> {
    let block = at::SYSTEM;
    let copy = moved.read(os, block + kept::COPY)?;
    let size = moved.read(os, block + kept::SIZE)?;
    let image = moved.read(os, at::IMAGE_BASE)?;
    os.as_caller().copy_bytes(copy, image, size)?;
    let statics = Statics::of_base(moved.base().wrapping_sub(copy).wrapping_add(image));
    let limit = statics.read(os, block + kept::LIMIT)?;
    reinstate(os, statics, limit)?;
    let mut registers = Registers::default();
    for (n, slot) in (0..).zip(&mut registers.r[V_FIRST..V_FIRST + V_COUNT]) {
        *slot = statics.read(os, block + kept::V_REGISTERS + 4 * n)?;
    }
    registers.r[0] = answer;
    registers.r[13] = statics.read(os, block + kept::SP)?;
    let flags = statics.read(os, block + kept::FLAGS)?;
    let back = statics.read(os, block + kept::RETURN)?;
    (registers.r[15], registers.flags) = returning(os.caller_bits(), back, flags);
    os.leave_with(registers);
    Ok(())
}

/// The sub-program's Exit handler: its end, the caller answered nought.
pub(super) fn system_exit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    ended(os, Statics::of_base(regs.r[12]), 0)
}

/// The sub-program's error handler: its end, the error noted and the caller
/// answered as for an OS error.
pub(super) fn system_error(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let moved = Statics::of_base(regs.r[0]);
    moved.write(os, at::ERROR_PENDING, 1)?;
    ended(os, moved, OS_ERROR)
}

/// The sub-program's upcall handler: each upcall passed on to the handler
/// in force before the library's.
pub(super) fn system_up_call(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    environment::pass_up_call(os, Statics::of_base(regs.r[12]), regs)
}
