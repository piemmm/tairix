//! The routine `HLineAddr` hands out: a horizontal line from R0 to R2 on
//! row R1, in pixels from the screen's bottom left, clipped to the graphics
//! window, in R3's colour — nothing, the foreground, inverted, the
//! background, or the colour block R3 points at. Every register is kept.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::canvas::Block;
use crate::plot::{ink_block, Ink};
use crate::spans::plot;

/// The routine.
pub(crate) fn routine(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let block = match regs.r[3] {
        code @ 0..=3 => match Ink::of(code) {
            Some(ink) => ink_block(os, ink)?,
            None => return Ok(()),
        },
        at => Block::read_at(os, at)?,
    };
    let (left, y, right) = (
        regs.r[0].cast_signed(),
        regs.r[1].cast_signed(),
        regs.r[2].cast_signed(),
    );
    plot(os, [(y, left.min(right), left.max(right))], &block)
}
