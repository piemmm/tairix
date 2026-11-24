//! `OS_ChangedBox`: the box around every pixel the drivers have changed
//! since it was last reset, kept in zero page at `&165C` — a flag word whose
//! bit 0 enables it, then the left, bottom, right and top pixels — where
//! software reads it and modules widen it themselves.

use tairix_riscos_kernel::memory::layout::HIGH_ZERO_PAGE;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::output::workspace;

use crate::mode::Format;

/// A box of pixels from the screen's bottom left, inclusive at every edge.
pub(crate) use tairix_riscos_kernel::mouse::Bounds as Rect;

/// Where the box is.
const BOX: u32 = HIGH_ZERO_PAGE + workspace::CHANGED_BOX;
/// The flag word's enabling bit.
const ENABLED: u32 = 1;
/// A box covering nothing, which any pixel widens to itself.
const EMPTY: [u32; 4] = [0x7FFF_FFFF, 0x7FFF_FFFF, 0x8000_0000, 0x8000_0000];

/// Widens the box over `rect`, while the box is enabled.
pub(crate) fn cover(os: &mut Os<'_>, rect: Rect) -> Result<(), Fail> {
    let (words, at) = os.page_words_mut(BOX)?;
    if words[at] & ENABLED == 0 {
        return Ok(());
    }
    let mut widen = |i: usize, value: i32, smaller: bool| {
        let old = words[at + i].cast_signed();
        if (smaller && value < old) || (!smaller && value > old) {
            words[at + i] = value.cast_unsigned();
        }
    };
    widen(1, rect.left, true);
    widen(2, rect.bottom, true);
    widen(3, rect.right, false);
    widen(4, rect.top, false);
    Ok(())
}

/// Widens the box over the whole screen.
pub(crate) fn cover_screen(os: &mut Os<'_>, format: &Format) -> Result<(), Fail> {
    cover(
        os,
        Rect {
            left: 0,
            bottom: 0,
            right: (format.width() - 1).cast_signed(),
            top: (format.height() - 1).cast_signed(),
        },
    )
}

/// The flag word and box as a mode change leaves them: disabled and empty.
pub(crate) const RESET: [u32; 5] = [0, EMPTY[0], EMPTY[1], EMPTY[2], EMPTY[3]];

/// The box disabled and emptied, as a mode change leaves it.
pub(crate) fn reset(os: &mut Os<'_>) -> Result<(), Fail> {
    set_words(os, RESET)
}

fn empty(os: &mut Os<'_>) -> Result<(), Fail> {
    os.write_words(BOX + 4, &EMPTY)
}

/// The flag word and the box, as zero page holds them.
pub(crate) fn words(os: &mut Os<'_>) -> Result<[u32; 5], Fail> {
    let mut words = [0; 5];
    os.read_words(BOX, &mut words)?;
    Ok(words)
}

/// Sets the flag word and the box.
pub(crate) fn set_words(os: &mut Os<'_>, words: [u32; 5]) -> Result<(), Fail> {
    os.write_words(BOX, &words)
}

/// `OS_ChangedBox`: R0 0 disables the box, 1 enables it, 2 empties it, any
/// other leaves it; R0 the flag word as it was, R1 the box's address as the
/// caller's zero page has it.
pub(crate) fn swi(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flag = os.read_word(BOX)?;
    match regs.r[0] {
        0 => os.write_word(BOX, flag & !ENABLED)?,
        1 => os.write_word(BOX, flag | ENABLED)?,
        2 => empty(os)?,
        _ => {}
    }
    regs.r[0] = flag;
    regs.r[1] = os.vector_base() + workspace::CHANGED_BOX;
    Ok(())
}
