//! What a front end shows of a session's screen: the bank on display and
//! its geometry, its pixels as colours through the palette in its flash
//! state now, the lines written since last asked, and the text cursor and
//! pointer to draw over it. The cursor is drawn over the screen rather than
//! into its memory, so a program reading its screen never finds it there.

use alloc::vec::Vec;

use tairix_collections::BitVec;
use tairix_riscos_arm::mmu::GRANULE_SIZE;
use tairix_riscos_kernel::os::{len32, Fail, Os};

use crate::mode::flag;
use crate::screen::AREA;
use crate::state::read;

/// The screen on display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// A pixel is `1 << x_eig` OS units across: with `y_eig`, the aspect
    /// the front end scales it to.
    pub x_eig: u32,
    /// And `1 << y_eig` up.
    pub y_eig: u32,
    /// The border's colour, as `&RRGGBB`.
    pub border: u32,
    /// The screen is blanked: show it black.
    pub blanked: bool,
}

/// The text cursor to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Its cell's left pixel, from the screen's left.
    pub x: u32,
    /// Its cell's top line, from the screen's top.
    pub y: u32,
    /// The pixels across it covers.
    pub width: u32,
    /// The lines of the cell it covers.
    pub lines: core::ops::RangeInclusive<u32>,
    /// It flashes rather than standing steady.
    pub flashing: bool,
}

/// The pointer to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerView {
    /// Its pixels across.
    pub width: u32,
    /// Its pixels down.
    pub height: u32,
    /// Its colours 1 to 3, as `&RRGGBB`.
    pub colours: [u32; 3],
    /// The screen pixel its active point is at, from the screen's top left.
    pub at: (i32, i32),
    /// Its active point within it.
    pub active: (u32, u32),
}

/// The screen on display.
pub fn view(os: &mut Os<'_>) -> Option<View> {
    read(os, |vdu, _| {
        let f = &vdu.screen.format;
        View {
            width: f.width() << (f.log2_bpc() - f.log2_bpp()),
            height: f.height(),
            x_eig: f.x_eig(),
            y_eig: f.y_eig(),
            border: rgb(vdu.palette.border[usize::from(vdu.palette.second)] >> 8),
            blanked: vdu.palette.blanked,
        }
    })
}

/// `&BBGGRR` as `&RRGGBB`.
const fn rgb(bbggrr: u32) -> u32 {
    ((bbggrr & 0xFF) << 16) | (bbggrr & 0xFF00) | ((bbggrr >> 16) & 0xFF)
}

/// Line `row` from the top of the screen on display, a colour as `&RRGGBB`
/// for each of its pixels.
///
/// # Errors
///
/// The abort reading the screen's memory gives.
pub fn line(os: &mut Os<'_>, row: u32, out: &mut Vec<u32>) -> Result<(), Fail> {
    out.clear();
    read(os, |vdu, os| {
        let format = vdu.screen.format;
        if row >= format.height() {
            return Ok(());
        }
        let stored = format.width() as usize;
        let doubled = 1usize << (format.log2_bpc() - format.log2_bpp());
        if out.try_reserve_exact(stored * doubled).is_err() {
            return Err(os.no_memory());
        }
        vdu.screen
            .shown()
            .pixels(os, row, 0, format.width() - 1, out)?;
        out.resize(stored * doubled, 0);
        let bits = 1u32 << format.log2_bpp();
        let mask = u32::MAX >> (32 - bits);
        let direct = format.direct().then(|| Direct::of(&format));
        let palette = &vdu.palette;
        let shown = |pixel: u32| {
            direct.map_or_else(
                || rgb(palette.shown(pixel)),
                |direct| direct.rgb(palette, pixel),
            )
        };
        for i in (0..stored).rev() {
            let unit = out[i];
            for (k, shift) in (0..doubled).zip((0u32..).step_by(bits as usize)) {
                out[i * doubled + k] = shown((unit >> shift) & mask);
            }
        }
        Ok(())
    })
    .unwrap_or(Ok(()))
}

/// How a direct-colour pixel holds its red, green and blue, which order
/// they come in, and how many bits each takes.
#[derive(Clone, Copy)]
struct Direct {
    swapped: bool,
    layout: Layout,
}

#[derive(Clone, Copy)]
enum Layout {
    /// Four bits each.
    Twelve,
    /// Five, six and five.
    Sixteen,
    /// Five each.
    Fifteen,
    /// Eight each.
    TwentyFour,
}

impl Direct {
    fn of(format: &crate::mode::Format) -> Self {
        let layout = match (format.log2_bpp(), format.ncolour()) {
            (4, 0xFFF) => Layout::Twelve,
            (4, _) if format.has(flag::FULL_PALETTE) => Layout::Sixteen,
            (4, _) => Layout::Fifteen,
            _ => Layout::TwentyFour,
        };
        Self {
            swapped: format.has(flag::RGB),
            layout,
        }
    }

    /// `pixel`'s colour, as `&RRGGBB` through the gamma ramp.
    fn rgb(self, palette: &crate::palette::Palette, pixel: u32) -> u32 {
        let (r, g, b) = match self.layout {
            Layout::Twelve => (
                (pixel & 0xF) * 17,
                ((pixel >> 4) & 0xF) * 17,
                ((pixel >> 8) & 0xF) * 17,
            ),
            Layout::Sixteen => (
                (pixel & 0x1F) * 255 / 31,
                ((pixel >> 5) & 0x3F) * 255 / 63,
                ((pixel >> 11) & 0x1F) * 255 / 31,
            ),
            Layout::Fifteen => (
                (pixel & 0x1F) * 255 / 31,
                ((pixel >> 5) & 0x1F) * 255 / 31,
                ((pixel >> 10) & 0x1F) * 255 / 31,
            ),
            Layout::TwentyFour => (pixel & 0xFF, (pixel >> 8) & 0xFF, (pixel >> 16) & 0xFF),
        };
        let (r, b) = if self.swapped { (b, r) } else { (r, b) };
        let ramp =
            |v: u32, shift: u32| (palette.entries[(v & 0xFF) as usize][0] >> (8 + shift)) & 0xFF;
        (ramp(r, 0) << 16) | (ramp(g, 8) << 8) | ramp(b, 16)
    }
}

/// Hands `each` the first and last lines of the screen on display stored
/// into since last asked, a run at a time, from the top. Where there is no
/// room to sort the stores into lines, every line is given.
pub fn damage(os: &mut Os<'_>, mut each: impl FnMut(u32, u32)) {
    let Some((start, size, line, height)) = read(os, |vdu, _| {
        let s = &vdu.screen;
        (
            s.display_start,
            s.size,
            s.format.line_length(),
            s.format.height(),
        )
    }) else {
        return;
    };
    let mut lines = BitVec::try_with_len(height as usize, false).ok();
    os.take_display_damage(AREA, |offset| {
        let Some(lines) = lines.as_mut() else {
            return;
        };
        if size == 0 || line == 0 {
            return;
        }
        let from_start = (offset % size + size - start % size) % size;
        let first = from_start / line;
        let last = ((from_start + GRANULE_SIZE - 1) / line).min(height.saturating_sub(1));
        if first <= last {
            lines.set_range(first as usize..last as usize + 1, true);
        }
    });
    let Some(lines) = lines else {
        if height > 0 {
            each(0, height - 1);
        }
        return;
    };
    let mut from = 0;
    while let Some(first) = lines.next_one(from) {
        let end = lines.next_zero(first).unwrap_or(lines.len());
        each(len32(first), len32(end - 1));
        from = end;
    }
}

/// The text cursor, where one is to be drawn: shown, not removed, not in
/// VDU 5, its shape covering a line of its cell.
pub fn cursor(os: &mut Os<'_>) -> Option<Cursor> {
    read(os, |vdu, _| {
        let t = &vdu.text;
        if !t.cursor.shown || t.cursor.appearance == 1 || t.vdu5 || vdu.cursors_removed > 0 {
            return None;
        }
        let cell = vdu.screen.format.cell();
        let (column, row) = match t.editing {
            Some((x, y)) => t.cell(x, y),
            None => t.cursor_cell(),
        };
        let start = u32::from(t.cursor.start).min(cell.space_y - 1);
        let end = u32::from(t.cursor.end).min(cell.space_y - 1);
        (start <= end).then_some(Cursor {
            x: column * cell.size_x,
            y: row * cell.space_y,
            width: cell.size_x,
            lines: start..=end,
            flashing: t.cursor.appearance >= 2,
        })
    })
    .flatten()
}

/// The pointer, where a shape is selected, its pixels in `pixels` a row at
/// a time, each a colour 0 to 3, nought clear.
///
/// # Errors
///
/// "Not enough memory" where `pixels` cannot hold the shape.
pub fn pointer(os: &mut Os<'_>, pixels: &mut Vec<u8>) -> Result<Option<PointerView>, Fail> {
    pixels.clear();
    read(os, |vdu, os| {
        let p = &vdu.pointer;
        let Some(shape) = p
            .selected
            .checked_sub(1)
            .and_then(|n| p.shapes.get(usize::from(n)))
        else {
            return Ok(None);
        };
        if pixels.try_reserve_exact(4 * shape.data.len()).is_err() {
            return Err(os.no_memory());
        }
        pixels.extend(
            shape
                .data
                .iter()
                .flat_map(|byte| (0..4).map(move |p| (byte >> (2 * p)) & 3)),
        );
        let (x, y) = p.at(os);
        let f = &vdu.screen.format;
        let at = (
            x >> f.x_eig(),
            f.height().cast_signed() - 1 - (y >> f.y_eig()),
        );
        Ok(Some(PointerView {
            width: u32::from(shape.width) * 4,
            height: u32::from(shape.height),
            colours: vdu.palette.pointer.map(|[first, _]| rgb(first >> 8)),
            at,
            active: (u32::from(shape.active.0), u32::from(shape.active.1)),
        }))
    })
    .unwrap_or(Ok(None))
}
