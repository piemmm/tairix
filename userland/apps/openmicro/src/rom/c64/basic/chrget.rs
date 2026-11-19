//! CHRGET, the routine BASIC copies to zero page at `$73` that steps
//! TXTPTR through the text: past spaces, Z set at a statement's end, C
//! clear at a digit.

use super::ws::zp;
use crate::rom::asm::{abs, imm, zp as z, Asm, AsmError};

/// TXTPTR as the cold start leaves it, until NEW sets it: the original's.
const FIRST_TXTPTR: u16 = 0xEA60;

/// CHRGET's bytes as they run from `$73`, TXTPTR the operand of CHRGOT's
/// load.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn code() -> Result<alloc::vec::Vec<u8>, AsmError> {
    let mut a = Asm::new(zp::CHRGET, usize::from(zp::RNDX - zp::CHRGET));
    a.here("step");
    a.inc(z(zp::TXTPTR));
    a.bne("read");
    a.inc(z(zp::TXTPTR + 1));
    a.here("read");
    a.lda(abs(FIRST_TXTPTR));
    a.cmp(imm(b':'));
    a.bcs("done");
    a.cmp(imm(b' '));
    a.beq("step");
    a.sec();
    a.sbc(imm(b'0'));
    a.sec();
    a.sbc(imm(0xD0));
    a.here("done");
    a.rts();
    Ok(a.finish()?.bytes)
}
