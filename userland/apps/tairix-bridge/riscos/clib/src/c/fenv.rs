//! `fenv.h` over the FPA's status register, as RISC OS 5.30's library has
//! it: an exception is its cumulative flag's bit, `fenv_t` the register's
//! one word, and the rounding direction only ever to nearest, as the FPA
//! takes its rounding from each instruction rather than a mode. Raising an
//! exception is an operation raising it, which traps as the FPA would.

use tairix_riscos_arm::fpa::{fpsr, Exception};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::fpa::as_client;

/// The one rounding direction there is: to nearest.
const TO_NEAREST: u32 = 0;

/// `feclearexcept(excepts)`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(crate) fn feclearexcept(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let status = os.fpsr();
    os.set_fpsr(status & !(regs.r[0] & fpsr::FLAGS));
    regs.r[0] = 0;
    Ok(())
}

/// `fegetexceptflag(flagp, excepts)`: the flags asked, as one word.
pub(crate) fn fegetexceptflag(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = os.fpsr() & regs.r[1] & fpsr::FLAGS;
    os.as_caller().write_word(regs.r[0], flags)?;
    regs.r[0] = 0;
    Ok(())
}

/// The exceptions of `excepts` raised in the FPA's order as operations
/// raise them: each trapping where its trap is enabled, else flagged — an
/// overflow or an underflow inexact too, as the result it leaves is.
fn raise(os: &mut Os<'_>, excepts: u32) -> Result<(), Fail> {
    as_client(os, |unit| {
        for exception in Exception::ALL
            .into_iter()
            .filter(|exception| excepts & exception.flag() != 0)
        {
            unit.raise(exception)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// `feraiseexcept(excepts)`: what it was asked beyond the exceptions there
/// are, as RISC OS 5.30 answers.
pub(crate) fn feraiseexcept(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let excepts = regs.r[0];
    raise(os, excepts & fpsr::FLAGS)?;
    regs.r[0] = excepts & !fpsr::FLAGS;
    Ok(())
}

/// `fesetexceptflag(flagp, excepts)`: the flags asked made the word's,
/// trapping nothing.
pub(crate) fn fesetexceptflag(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (mask, flags) = (
        regs.r[1] & fpsr::FLAGS,
        os.as_caller().read_word(regs.r[0])?,
    );
    let status = os.fpsr();
    os.set_fpsr(status & !mask | flags & mask);
    regs.r[0] = 0;
    Ok(())
}

/// `fetestexcept(excepts)`: the flags asked that are set.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(crate) fn fetestexcept(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] &= os.fpsr() & fpsr::FLAGS;
    Ok(())
}

/// `fegetround()`: to nearest.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(crate) fn fegetround(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = TO_NEAREST;
    Ok(())
}

/// `fesetround(round)`: nothing changed, the direction answered, so only to
/// nearest succeeds.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(crate) fn fesetround(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

/// `fegetenv(envp)`: the FPSR stored, the pointer answered.
pub(crate) fn fegetenv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let status = os.fpsr();
    os.as_caller().write_word(regs.r[0], status)
}

/// `feholdexcept(envp)`: the FPSR stored, then all of it but its system ID
/// cleared — every trap, flag and control bit — as RISC OS 5.30 leaves it.
pub(crate) fn feholdexcept(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let status = os.fpsr();
    os.as_caller().write_word(regs.r[0], status)?;
    os.set_fpsr(status & !fpsr::WRITABLE);
    regs.r[0] = 0;
    Ok(())
}

/// `fesetenv(envp)`: the FPSR made the one stored.
pub(crate) fn fesetenv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let status = os.as_caller().read_word(regs.r[0])?;
    os.set_fpsr(status);
    regs.r[0] = 0;
    Ok(())
}

/// `feupdateenv(envp)`: the flags set now kept, the FPSR made the one
/// stored, then those flags raised again.
pub(crate) fn feupdateenv(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (held, status) = (
        os.fpsr() & fpsr::FLAGS,
        os.as_caller().read_word(regs.r[0])?,
    );
    os.set_fpsr(status);
    raise(os, held)?;
    regs.r[0] = 0;
    Ok(())
}

#[cfg(test)]
mod tests;
