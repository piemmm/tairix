//! Sprites taken from the screen, or the sprite output is switched to
//! (`OS_SpriteOp` 14 and 16): a rectangle of pixels, its corners inclusive
//! and in either order, made a sprite in the destination's own pixel form —
//! a numbered mode's number, its rows keeping the screen's word alignment as
//! left-hand wastage, else the mode word naming its pixels. Every pixel of
//! the rectangle outside the graphics window, the destination's edges
//! included, is the background colour.
//!
//! The bits of a row outside the rectangle are nought. RISC OS 5 leaves
//! values from beyond the rectangle in an old-format sprite's wastage, the
//! screen's pixels among them; here nothing outside it is copied.

use tairix_image::{SpriteName, SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_vdu::mode::repeated;

use super::area::{read_name, Area, Naming};
use super::create::{new_palette_entries, screen_palette, Shape};
use super::edit::set_field;
use super::format::{self, Pixels, BAD_SPRITE_MODE};
use super::header::{advanced, Sprite};
use super::{system, FULL_PALETTE, NO_DEEP_NEW_PALETTE, NO_ROOM};
use crate::canvas::Canvas;
use crate::changed::Rect;
use crate::colour::Colours;
use crate::output::Output;
use crate::raster::Point;
use crate::state;

/// `OS_SpriteOp` 14: the rectangle the last two graphics points bound.
pub(crate) fn from_cursors(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let corners = state::with(os, |vdu, _| {
        Ok((vdu.graphics.old, vdu.graphics.cursor_pixels))
    })?;
    take(os, naming, regs, corners)
}

/// `OS_SpriteOp` 16: the rectangle R4 to R7 bound, in OS units from the
/// origin.
pub(crate) fn from_rectangle(os: &mut Os<'_>, naming: Naming, regs: &mut Regs) -> Result<(), Fail> {
    let corners = state::with(os, |vdu, _| {
        Ok((
            vdu.pixels_of(regs.r[4].cast_signed(), regs.r[5].cast_signed()),
            vdu.pixels_of(regs.r[6].cast_signed(), regs.r[7].cast_signed()),
        ))
    })?;
    take(os, naming, regs, corners)
}

/// What the screen is as a sprite is taken from it.
struct Source {
    canvas: Canvas,
    window: Rect,
    pixels: Pixels,
    word: u32,
    background: u32,
    palette: u32,
}

/// Takes the rectangle `corners` bound as sprite R2, with the screen's
/// palette where R3 asks; one already of that name is deleted first.
fn take(
    os: &mut Os<'_>,
    naming: Naming,
    regs: &mut Regs,
    corners: (Point, Point),
) -> Result<(), Fail> {
    let area = Area::named(os, naming, regs.r[1])?;
    let name = read_name(os, regs.r[2])?.lowered();
    let grab = Grab::of(os, corners, regs.r[3] != 0)?;
    let sprite = grab.write(os, area, name)?;
    if area.system {
        system::select(os, Some(name));
    } else {
        regs.r[2] = sprite.at;
    }
    Ok(())
}

/// A rectangle of the destination as a sprite is about to be taken from it.
pub(crate) struct Grab {
    source: Source,
    shape: Shape,
    entries: u32,
    width: u32,
    height: u32,
    first_bit: u32,
    top_left: Point,
}

impl Grab {
    /// The rectangle `corners` bound, with the screen's palette where
    /// `palette` asks.
    ///
    /// # Errors
    ///
    /// "Bad mode" for a destination no sprite can hold, and "No palette for
    /// this depth" for a palette asked of a deep one.
    pub(crate) fn of(os: &mut Os<'_>, (a, b): (Point, Point), palette: bool) -> Result<Self, Fail> {
        let source = state::with(os, |vdu, _| {
            let format = *vdu.format();
            let (word, pixels) = match vdu.output {
                Output::Screen => {
                    let word = format::of_screen(vdu.screen.named, &format);
                    let pixels = match vdu.screen.named {
                        crate::screen::Named::Number(_) => Pixels::numbered(&format),
                        crate::screen::Named::Selector(_) => word
                            .and_then(|w| Pixels::stored(w).ok())
                            .unwrap_or_else(|| Pixels::numbered(&format)),
                    };
                    (word, pixels)
                }
                Output::Sprite { word, .. } => (
                    Some(word),
                    Pixels::stored(word).unwrap_or_else(|_| Pixels::numbered(&format)),
                ),
            };
            let back = Colours::gcol_pixel(&format, &vdu.colours.back) & format.pixel_mask();
            let palette = if format.full_palette() {
                FULL_PALETTE
            } else {
                new_palette_entries(&pixels)
            };
            Ok(word.map(|word| Source {
                canvas: vdu.canvas(),
                window: vdu.graphics.window,
                pixels,
                word,
                background: repeated(back, 1 << format.log2_bpp(), pixels.field_bits()),
                palette,
            }))
        })?;
        let Some(source) = source else {
            return Err(os.error(BAD_SPRITE_MODE));
        };
        let entries = if !palette {
            0
        } else if source.pixels.paletted() {
            source.palette
        } else {
            return Err(os.error(NO_DEEP_NEW_PALETTE));
        };
        let (left, right) = (a.0.min(b.0), a.0.max(b.0));
        let (bottom, top) = (a.1.min(b.1), a.1.max(b.1));
        let width = u32::try_from(i64::from(right) - i64::from(left) + 1).unwrap_or(u32::MAX);
        let height = u32::try_from(i64::from(top) - i64::from(bottom) + 1).unwrap_or(u32::MAX);
        let first_bit = if source.pixels.old_format {
            (left.cast_unsigned() << source.pixels.log2_field) & 31
        } else {
            0
        };
        let shape = Shape::new(&source.pixels, width, height, first_bit);
        Ok(Self {
            source,
            shape,
            entries,
            width,
            height,
            first_bit,
            top_left: (left, top),
        })
    }

    /// The bytes the sprite takes, if a sprite can.
    pub(crate) fn length(&self) -> Option<u32> {
        self.shape.length(self.entries * SPRITE_PALETTE_ENTRY_LEN)
    }

    /// Writes the sprite into `area` as `name`, one already of that name
    /// deleted first.
    ///
    /// # Errors
    ///
    /// "No room in the sprite area" where it cannot hold the sprite.
    pub(crate) fn write(
        &self,
        os: &mut Os<'_>,
        area: Area,
        name: SpriteName,
    ) -> Result<Sprite, Fail> {
        let screen = screen_palette(os, self.entries)?;
        if let Some(old) = area.find(os, &name)? {
            area.remove(os, &old)?;
        }
        let Some(length) = self.length() else {
            return Err(os.error(NO_ROOM));
        };
        let at = area.append(os, length, NO_ROOM)?;
        let palette = self.entries * SPRITE_PALETTE_ENTRY_LEN;
        let sprite = Sprite {
            at: area.address(at),
            next: length,
            name,
            words_less_one: self.shape.words.wrapping_sub(1),
            rows_less_one: self.height.wrapping_sub(1),
            first_bit: self.first_bit,
            last_bit: self.shape.last_bit,
            image: SPRITE_HEADER_LEN + palette,
            mask: SPRITE_HEADER_LEN + palette,
            mode: self.source.word,
        };
        sprite.write(os)?;
        os.write_bytes(sprite.address(SPRITE_HEADER_LEN), &screen)?;
        copy_rows(os, &self.source, &sprite, self.top_left, self.width)?;
        Ok(sprite)
    }
}

/// The pixels of a row's first word before its first pixel.
fn first_pixel(first_bit: u32, pixels: &Pixels) -> u32 {
    first_bit >> pixels.log2_field
}

/// Fills the sprite's rows, `width` pixels each, from the screen's rectangle
/// whose top left pixel is `(left, top)`, the background where the window
/// holds no screen pixel.
fn copy_rows(
    os: &mut Os<'_>,
    source: &Source,
    sprite: &Sprite,
    (left, top): Point,
    width: u32,
) -> Result<(), Fail> {
    let stride = sprite.stride();
    let mut background = super::edit::row_buffer(os, stride)?;
    let mut row = super::edit::row_buffer(os, stride)?;
    let field = source.pixels.field_bits();
    let first = first_pixel(sprite.first_bit, &source.pixels);
    for p in first..first.saturating_add(width) {
        set_field(&mut background, p, field, source.background);
    }
    let canvas = &source.canvas;
    let window = clipped(source.window, canvas);
    let right = i64::from(left) + i64::from(width) - 1;
    let (from, to) = (
        left.max(window.left),
        i32::try_from(right.min(i64::from(window.right))).unwrap_or(i32::MAX),
    );
    let mut shown = alloc::vec::Vec::new();
    for index in 0..sprite.rows() {
        let y = top.wrapping_sub(index.cast_signed());
        row.copy_from_slice(&background);
        if y >= window.bottom && y <= window.top && from <= to {
            shown.clear();
            canvas.pixels(
                os,
                canvas.row_of(y),
                from.cast_unsigned(),
                to.cast_unsigned(),
                &mut shown,
            )?;
            for (x, &pixel) in (from..=to).zip(&shown) {
                set_field(&mut row, first + (x - left).cast_unsigned(), field, pixel);
            }
        }
        os.write_bytes(
            advanced(sprite.address(sprite.image), stride * u64::from(index)),
            &row,
        )?;
    }
    Ok(())
}

/// The graphics window, cut to the screen.
fn clipped(window: Rect, canvas: &Canvas) -> Rect {
    Rect {
        left: window.left.max(0),
        bottom: window.bottom.max(0),
        right: window.right.min(canvas.width.cast_signed() - 1),
        top: window.top.min(canvas.height.cast_signed() - 1),
    }
}
