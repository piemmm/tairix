//! The library kernel's small answers: the current stack chunk, the
//! language a PC lies in, and the caller's IRQ state, which only a
//! privileged caller may change — from User mode the calls change nothing,
//! as RISC OS 5.30's do.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::context;
use super::language;
use crate::stack_chunk;

/// `_kernel_current_stack_chunk()`: the chunk SL lies in.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn current_stack_chunk(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = stack_chunk::of_limit(regs.r[10]);
    Ok(())
}

/// `_kernel_language(pc)`: the name of the language whose code holds `pc`,
/// nought where none does.
pub(super) fn language(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    regs.r[0] = match language::holding(os, statics, regs.r[0])? {
        Some(found) => found.name(os)?,
        None => 0,
    };
    Ok(())
}

/// `_kernel_irqs_on()`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn irqs_on(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.set_caller_irqs_disabled(false);
    Ok(())
}

/// `_kernel_irqs_off()`.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn irqs_off(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.set_caller_irqs_disabled(true);
    Ok(())
}

/// `_kernel_irqs_disabled()`: the caller's I bit, nought where IRQs are
/// enabled.
#[expect(clippy::unnecessary_wraps, reason = "an entry's signature")]
pub(super) fn irqs_disabled(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = if os.caller_irqs_disabled() { psr::I } else { 0 };
    Ok(())
}
