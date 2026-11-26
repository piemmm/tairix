//! An APCS frame record: the saved PC at its frame pointer, then the saved
//! LR, SP and FP below it.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::Os;

/// Each saved word's distance below the frame pointer.
pub(crate) const SAVED_PC: u32 = 0;
pub(crate) const SAVED_LR: u32 = 4;
pub(crate) const SAVED_SP: u32 = 8;
pub(crate) const SAVED_FP: u32 = 12;

/// The word at `address` as the caller could read it, none where it could
/// not: a frame that cannot be read is corrupt.
pub(crate) fn readable(os: &mut Os<'_>, address: u32) -> Option<u32> {
    os.as_caller().peek_word(address)
}

/// Where a saved return goes on, and the flags it leaves: a 26-bit one
/// restores the caller's, a 32-bit one keeps `flags`.
pub(crate) const fn returning(bits: Bits, lr: u32, flags: u32) -> (u32, u32) {
    match bits {
        Bits::Bit26 => (lr & psr::R15_PC, lr & psr::NZCV),
        Bits::Bit32 => (lr, flags),
    }
}

/// A saved PC or return as an address: a 26-bit one without its flags.
pub(crate) const fn code_address(bits: Bits, pc: u32) -> u32 {
    match bits {
        Bits::Bit26 => pc & psr::R15_PC,
        Bits::Bit32 => pc,
    }
}

#[cfg(test)]
mod tests {
    use tairix_riscos_kernel::memory::Bits;

    use super::{code_address, returning};

    #[test]
    fn a_26_bit_return_restores_the_caller_s_flags_and_a_32_bit_one_keeps_them() {
        assert_eq!(
            returning(Bits::Bit26, 0xA000_8004, 0),
            (0x8004, 0xA000_0000)
        );
        assert_eq!(
            returning(Bits::Bit32, 0xA000_8004, 0x2000_0000),
            (0xA000_8004, 0x2000_0000)
        );
        assert_eq!(code_address(Bits::Bit26, 0xFC00_8007), 0x8004);
        assert_eq!(code_address(Bits::Bit32, 0xFC00_8007), 0xFC00_8007);
    }
}
