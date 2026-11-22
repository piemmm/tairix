//! The pointer: its four shapes, defined by `OS_Word` 21,0 at two bits a
//! pixel, colour nought clear; the one `OS_Byte` 106 selects, and whether
//! it follows the mouse; and its position, `OS_Word` 21,5 and 21,6. The
//! front end draws it in the colours the palette gives the pointer.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::state::with;

/// The most bytes across and lines down a shape takes.
const MOST_WIDTH: u8 = 8;
const MOST_HEIGHT: u8 = 32;

/// A pointer shape.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Shape {
    /// Bytes across, four pixels each, the leftmost in the low bits.
    pub(crate) width: u8,
    pub(crate) height: u8,
    /// The pixel the pointer's position names.
    pub(crate) active: (u8, u8),
    /// Its lines, the top first.
    pub(crate) data: Vec<u8>,
}

/// The pointer the drivers keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pointer {
    pub(crate) shapes: [Shape; 4],
    /// The shape shown, 1 to 4, or none.
    pub(crate) selected: u8,
    /// It stays where set rather than following the mouse.
    pub(crate) unlinked: bool,
    /// Where it was set, in OS units from the screen's bottom left.
    pub(crate) position: (i32, i32),
}

impl Pointer {
    pub(crate) fn new() -> Self {
        Self {
            shapes: Default::default(),
            selected: 0,
            unlinked: false,
            position: (0, 0),
        }
    }

    /// Where it is, in OS units from the screen's bottom left.
    pub(crate) fn at(&self, os: &Os<'_>) -> (i32, i32) {
        if self.unlinked {
            self.position
        } else {
            os.mouse_position()
        }
    }
}

/// `OS_Byte` 106: R1's low seven bits the shape, its top bit the pointer
/// unlinked from the mouse; R1 the setting as it was.
pub(crate) fn select(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    with(os, |vdu, os| {
        let p = &mut vdu.pointer;
        let old = u32::from(p.selected) | if p.unlinked { 0x80 } else { 0 };
        let was_linked = !p.unlinked;
        p.selected = (regs.byte(1) & 0x7F).min(4);
        p.unlinked = regs.byte(1) & 0x80 != 0;
        if p.unlinked && was_linked {
            p.position = os.mouse_position();
        }
        regs.r[1] = old;
        Ok(())
    })
}

/// `OS_Word` 21's pointer reasons: 0 defines a shape, 5 sets the position
/// and 6 reads it, in OS units from the origin.
pub(crate) fn word_21(os: &mut Os<'_>, reason: u8, block: u32) -> Result<(), Fail> {
    match reason {
        0 => {
            let mut head = [0; 9];
            os.read_bytes(block.wrapping_add(1), &mut head)?;
            let [number, width, height, ax, ay, d0, d1, d2, d3] = head;
            if !(1..=4).contains(&number) || width > MOST_WIDTH || height > MOST_HEIGHT {
                return Ok(());
            }
            let data = os.read_vec(
                u32::from_le_bytes([d0, d1, d2, d3]),
                usize::from(width) * usize::from(height),
            )?;
            with(os, |vdu, _| {
                vdu.pointer.shapes[usize::from(number - 1)] = Shape {
                    width,
                    height,
                    active: (ax, ay),
                    data,
                };
                Ok(())
            })
        }
        5 => {
            let (x, y) = os.read_coordinates(block.wrapping_add(1))?;
            with(os, |vdu, _| {
                let origin = vdu.graphics.origin;
                vdu.pointer.position = (x + origin.0, y + origin.1);
                Ok(())
            })
        }
        6 => {
            let (x, y) = with(os, |vdu, os| {
                let (x, y) = vdu.pointer.at(os);
                Ok((x - vdu.graphics.origin.0, y - vdu.graphics.origin.1))
            })?;
            os.write_coordinates(block.wrapping_add(1), (x, y))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
