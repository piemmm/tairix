//! `setjmp.h`: the environment in RISC OS 5.30's 22-word `jmp_buf` — R4 to
//! R9, SL, FP, SP and the return, then F4 to F7 as `STFE` stores them — and
//! `longjmp` resuming it, out of a signal handler or a trap's too.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::kernel::context;
use crate::kernel::frame::returning;
use crate::kernel::trap::{self, Cause};

/// The integer registers kept, in their order, and the floating-point ones,
/// from F4.
const KEPT: [usize; REGISTERS as usize] = [4, 5, 6, 7, 8, 9, 10, 11, 13, 14];
const REGISTERS: u32 = 10;
const FIRST_FLOAT: u8 = 4;
const FLOATS: usize = 4;
/// Each floating-point register's words.
const FLOAT_WORDS: usize = 3;
const FLOATS_AT: u32 = 4 * REGISTERS;

/// `setjmp(env)`: nought.
pub(super) fn setjmp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let env = regs.r[0];
    let caller = os.registers();
    os.as_caller()
        .write_words(env, &KEPT.map(|n| caller.r[n]))?;
    let mut floats = [[0; FLOAT_WORDS]; FLOATS];
    for (n, words) in (FIRST_FLOAT..).zip(&mut floats) {
        match os.fpa_extended(n) {
            Ok(stored) => *words = stored,
            Err(exception) => return Ok(os.floating_point(exception)?),
        }
    }
    os.as_caller()
        .write_words(env.wrapping_add(FLOATS_AT), floats.as_flattened())?;
    regs.r[0] = 0;
    Ok(())
}

/// `longjmp(env, val)`: execution goes on as `setjmp` returning `val`, or
/// one for nought, any trap or event being handled taken as over.
pub(super) fn longjmp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (env, value) = (regs.r[0], regs.r[1]);
    let statics = context::statics_at(os, regs.r[10])?;
    let mut kept = [0; REGISTERS as usize];
    os.as_caller().read_words(env, &mut kept)?;
    let mut floats = [[0; FLOAT_WORDS]; FLOATS];
    os.as_caller()
        .read_words(env.wrapping_add(FLOATS_AT), floats.as_flattened_mut())?;
    for (n, &words) in (FIRST_FLOAT..).zip(&floats) {
        if let Err(exception) = os.set_fpa_extended(n, words) {
            return Ok(os.floating_point(exception)?);
        }
    }
    trap::abandon(os, statics, Cause::Trap)?;
    trap::abandon(os, statics, Cause::Event)?;
    let mut registers = os.registers();
    for (&n, &value) in KEPT.iter().zip(&kept) {
        registers.r[n] = value;
    }
    registers.r[0] = value.max(1);
    let bits = os.caller_bits();
    (registers.r[15], registers.flags) = returning(bits, registers.r[14], registers.flags);
    os.leave_with(registers);
    Ok(())
}

#[cfg(test)]
mod tests;
