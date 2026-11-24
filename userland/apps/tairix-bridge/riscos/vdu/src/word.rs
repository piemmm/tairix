//! The `OS_Word` reasons the VDU drivers answer: a point's colour, a
//! character's or pattern's definition, the palette read and written, the
//! graphics cursors, the pointer, and the screen banks' bases.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::state::with;

/// The VDU drivers' `OS_Word` reasons.
pub(crate) fn reason(os: &mut Os<'_>, regs: &mut Regs) -> Option<Result<(), Fail>> {
    let block = regs.r[1];
    Some(match regs.r[0] {
        9 => read_point(os, block),
        10 => definition(os, block),
        11 => read_palette(os, block),
        12 => write_palette(os, block),
        13 => cursors(os, block),
        21 => {
            let reason = match os.read_byte(block) {
                Ok(reason) => reason,
                Err(fail) => return Some(Err(fail)),
            };
            if tairix_riscos_kernel::mouse::MOUSE_REASONS.contains(&reason) {
                return None;
            }
            crate::pointer::word_21(os, reason, block)
        }
        22 => bases(os, block),
        _ => return None,
    })
}

/// `OS_Word` 9: the logical colour at the block's point, in OS units from
/// the origin, at +4; `&FF` off the screen or where nothing is drawn.
fn read_point(os: &mut Os<'_>, block: u32) -> Result<(), Fail> {
    let (x, y) = os.read_coordinates(block)?;
    let colour = with(os, |vdu, os| vdu.colour_at(os, x, y))?;
    let value = colour.map_or(0xFF, |(c, _)| c.to_le_bytes()[0]);
    os.write_byte(block.wrapping_add(4), value)
}

/// `OS_Word` 10: the block's first byte names a character, whose eight
/// rows follow it; or 2 to 5 an ECF pattern's rows in the native form; or 6
/// the dot pattern's bytes.
fn definition(os: &mut Os<'_>, block: u32) -> Result<(), Fail> {
    let code = os.read_byte(block)?;
    let rows = with(os, |vdu, _| {
        Ok(match code {
            2..=5 => vdu.colours.patterns[usize::from(code - 2)],
            6 => vdu.colours.dot.to_be_bytes(),
            _ => vdu.font.glyph(code).copied().unwrap_or([0; 8]),
        })
    })?;
    os.write_bytes(block.wrapping_add(1), &rows)
}

/// `OS_Word` 11: the logical colour at +0 through `PaletteV`, in the flash
/// state shown now: how it was set at +1, red, green and blue at +2 to +4.
fn read_palette(os: &mut Os<'_>, block: u32) -> Result<(), Fail> {
    let logical = os.read_byte(block)?;
    let [first, second] = crate::palette::read_through(os, u32::from(logical))?;
    let in_second = with(os, |vdu, _| Ok(vdu.palette.second))?;
    let shown = if in_second { second } else { first };
    os.write_bytes(block.wrapping_add(1), &shown.to_le_bytes())
}

/// `OS_Word` 12: `VDU 19` from the block's five bytes.
fn write_palette(os: &mut Os<'_>, block: u32) -> Result<(), Fail> {
    let mut bytes = [0; 5];
    os.read_bytes(block, &mut bytes)?;
    crate::palette::write_through(os, bytes[0], bytes[1], [bytes[2], bytes[3], bytes[4]])
}

/// `OS_Word` 13: the graphics cursor before last and the last, in OS units
/// from the origin, sixteen bits each.
fn cursors(os: &mut Os<'_>, block: u32) -> Result<(), Fail> {
    let points = with(os, |vdu, _| {
        let old = vdu.units_of(vdu.graphics.old);
        Ok([old.0, old.1, vdu.graphics.cursor.0, vdu.graphics.cursor.1])
    })?;
    let mut bytes = [0; 8];
    for (chunk, value) in bytes.chunks_mut(2).zip(points) {
        chunk.copy_from_slice(&value.to_le_bytes()[..2]);
    }
    os.write_bytes(block, &bytes)
}

/// `OS_Word` 22: the bank the drivers write, the bank shown, or both, by
/// the block's first byte's bits 0 and 1, at the offset the next four give
/// from the screen's memory's start.
fn bases(os: &mut Os<'_>, block: u32) -> Result<(), Fail> {
    let kind = os.read_byte(block)?;
    let mut offset = [0; 4];
    os.read_bytes(block.wrapping_add(1), &mut offset)?;
    let offset = u32::from_le_bytes(offset);
    with(os, |vdu, _| {
        vdu.screen.set_base(kind & 1 != 0, kind & 2 != 0, offset);
        Ok(())
    })
}
