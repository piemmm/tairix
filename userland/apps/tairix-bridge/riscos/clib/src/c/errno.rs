//! `errno.h`: the values the library sets `errno` to, numbered as RISC OS
//! 5.30 numbers them, and the setting.

use tairix_riscos_kernel::os::{Fail, Os};

use super::statics::at;
use crate::kernel::statics::Statics;

pub(crate) const EDOM: u32 = 1;
pub(crate) const ERANGE: u32 = 2;
pub(crate) const ESIGNUM: u32 = 3;
/// A multibyte sequence naming no character.
pub(crate) const EILSEQ: u32 = 4;
/// A file position past what the call can reach or give.
pub(crate) const EOVERFLOW: u32 = 5;
/// A file grown past the largest its filing system holds.
pub(crate) const EFBIG: u32 = 6;
/// A call the OS refused, an open or one of a stream's transfers.
pub(crate) const OS_REFUSED: u32 = u32::MAX;

/// `errno` made `value`.
pub(crate) fn set(os: &mut Os<'_>, statics: Statics, value: u32) -> Result<(), Fail> {
    statics.write(os, at::ERRNO, value)
}
