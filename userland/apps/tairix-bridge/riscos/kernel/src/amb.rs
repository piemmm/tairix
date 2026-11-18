//! `OS_AMBControl` (`plans/RISCOS-EMULATOR.md` §4.5): RISC OS 5's Wimp's
//! own call for its tasks' memory, which no program is to use. The
//! emulator's Wimp keeps its tasks' memory itself, so a program is answered
//! only as genuine RISC OS 5 was seen to answer one: reason 9 succeeds and
//! changes nothing, and any other is refused as a reason the call does not
//! know, where genuine RISC OS 5 hangs or faults.

use crate::kheap::UNKNOWN_REASON;
use crate::os::{Fail, Os, Regs};

/// The one reason genuine RISC OS 5 was seen to answer a program.
const ANSWERED: u32 = 9;

impl Os<'_> {
    /// `OS_AMBControl`.
    pub(crate) fn amb_control(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if regs.r[0] == ANSWERED {
            Ok(())
        } else {
            Err(self.error(UNKNOWN_REASON))
        }
    }
}

#[cfg(test)]
mod tests;
