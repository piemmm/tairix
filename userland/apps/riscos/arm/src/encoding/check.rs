//! The UNPREDICTABLE rules both instruction sets state alike, decided by an
//! instruction's operands alone.

use crate::instruction::BitfieldOp;

/// Whether a `CPS` is UNPREDICTABLE: a mode given without the change, A, I
/// and F named without an enable or disable or not named with one, or the
/// reserved `imod`.
pub(crate) const fn change_state(imod: u8, change_mode: bool, interrupts: u8, mode: u8) -> bool {
    let changes_interrupts = imod & 2 != 0;
    (mode != 0 && !change_mode)
        || (changes_interrupts && interrupts == 0)
        || (!changes_interrupts && interrupts != 0)
        || (imod == 0 && !change_mode)
        || imod == 1
}

/// Whether a bitfield operation is UNPREDICTABLE: an extraction reaching past
/// bit 31, or an insertion or clear whose top bit is below its bottom.
pub(crate) const fn bitfield(op: BitfieldOp, lsb: u8, top: u8) -> bool {
    match op {
        BitfieldOp::Sbfx | BitfieldOp::Ubfx => lsb as u32 + top as u32 > 31,
        BitfieldOp::Bfi | BitfieldOp::Bfc => top < lsb,
    }
}
