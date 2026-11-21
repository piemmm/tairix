//! The routine `HLineAddr` hands out: a horizontal line from R0 to R2 on
//! row R1, in pixels from the screen's bottom left, clipped to the graphics
//! window, in R3's colour — nothing, the foreground, inverted, the
//! background, or the colour block R3 points at. Every register is kept.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::output::workspace;

use crate::canvas::Block;
use crate::changed::{cover, Rect};
use crate::state::with;

/// The routine.
pub(crate) fn routine(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let block = match regs.r[3] {
        0 => return Ok(()),
        1 => Block::read(os, workspace::FOREGROUND)?,
        2 => Block::solid(0, u32::MAX),
        3 => Block::read(os, workspace::BACKGROUND)?,
        at => {
            let mut words = [0u32; 16];
            for (i, word) in (0u32..).zip(words.iter_mut()) {
                *word = os.read_word(at.wrapping_add(4 * i))?;
            }
            let y_offset = os.read_word(
                tairix_riscos_kernel::memory::layout::HIGH_ZERO_PAGE + workspace::ECF_Y_OFFSET,
            )?;
            Block {
                rows: core::array::from_fn(|r| [words[2 * r], words[2 * r + 1]]),
                y_offset,
            }
        }
    };
    let (left, y, right) = (
        regs.r[0].cast_signed(),
        regs.r[1].cast_signed(),
        regs.r[2].cast_signed(),
    );
    with(os, |vdu, os| {
        let w = vdu.graphics.window;
        let (l, r) = (left.min(right).max(w.left), left.max(right).min(w.right));
        if y < w.bottom || y > w.top || l > r || !vdu.screen.format.graphic() {
            return Ok(());
        }
        let canvas = vdu.screen.canvas();
        crate::plot::span(os, &canvas, y, l, r, &block)?;
        cover(
            os,
            Rect {
                left: l,
                bottom: y,
                right: r,
                top: y,
            },
        )
    })
}
