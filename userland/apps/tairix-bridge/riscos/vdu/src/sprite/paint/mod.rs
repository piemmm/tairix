//! Sprites drawn on the destination (`OS_SpriteOp` 28, 34, 48 to 52, 55, 56
//! and 65, `PLOT` &E8 to &EF), each by the path RISC OS 5 takes:
//!
//! - the kernel's plots, 28, 34, 48 and 49, copy the sprite's rows bit for
//!   bit, whatever the destination's depth, each mask pixel covering its
//!   image pixel's bits ([`blit`]);
//! - SpriteExtend's, 50 to 52, 55, 56 and 65, map pixels to pixels, scaled
//!   or transformed, each turned into the destination's form by a
//!   translation table, the sprite's palette or its colour channels
//!   ([`scaled`], [`transformed`], [`colours`]).
//!
//! Every plot is held to the graphics window, and reads each row it needs
//! once: a blit or a scaled plot only those it shows, chosen once any guest
//! code turning its colours has run; a plot through a colour mapping, a tile
//! whose window holds a tile's edge and a transformed plot all of them.

mod blit;
mod colours;
mod pointer;
mod scaled;
mod source;
mod transformed;

use core::ops::Range;

use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::swi::sprite_op::THROUGH_MASK;

use crate::canvas::Canvas;
use crate::changed::Rect;
use crate::mode::Format;
use crate::raster::Point;
use crate::state;

pub(crate) use blit::{plot_mask, plot_shape, put};
pub(crate) use colours::Palettised;
pub(crate) use pointer::set_pointer;
pub(crate) use scaled::{paint_character, plot_mask_scaled, put_picture, put_scaled, tile};
pub(crate) use transformed::{plot_mask_transformed, put_transformed, scaling_of};

/// A scaled or transformed plot's flags, R5: the action, the mask, the table
/// and its form, dithering, a colour mapping and translucency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlotFlags(pub(crate) u32);

impl PlotFlags {
    /// Where the translucency is held, a byte.
    pub(crate) const TRANSLUCENCY_SHIFT: u32 = 8;

    pub(crate) const fn action(self) -> u32 {
        self.0 & 7
    }

    pub(crate) const fn masked(self) -> bool {
        self.0 & THROUGH_MASK != 0
    }

    pub(crate) const fn table_ignorable(self) -> bool {
        self.0 & 0x10 != 0
    }

    pub(crate) const fn wide_table(self) -> bool {
        self.0 & 0x20 != 0
    }

    pub(crate) const fn mapping(self) -> bool {
        self.0 & 0x80 != 0
    }

    /// The share of each pixel kept away, 0 for none to 255 for all but a
    /// 256th.
    pub(crate) const fn translucency(self) -> u32 {
        (self.0 >> Self::TRANSLUCENCY_SHIFT) & 0xFF
    }
}

/// Scale factors: each pixel across becomes `mul.0 / div.0` pixels, each
/// down `mul.1 / div.1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scale {
    mul: (u32, u32),
    div: (u32, u32),
}

impl Scale {
    /// One to one.
    pub(crate) const UNIT: Self = Self {
        mul: (1, 1),
        div: (1, 1),
    };

    /// `mul.0 / div.0` across and `mul.1 / div.1` down; none where a factor
    /// is nought, as no plot divides by one.
    pub(crate) const fn of(mul: (u32, u32), div: (u32, u32)) -> Option<Self> {
        if mul.0 == 0 || mul.1 == 0 || div.0 == 0 || div.1 == 0 {
            None
        } else {
            Some(Self { mul, div })
        }
    }

    /// The factors at `address`, one to one where it is nought; any factor of
    /// nought draws nothing, as none.
    pub(crate) fn read(os: &mut Os<'_>, address: u32) -> Result<Option<Self>, Fail> {
        if address == 0 {
            return Ok(Some(Self::UNIT));
        }
        let mut words = [0u32; 4];
        os.read_words(address, &mut words)?;
        let [mx, my, dx, dy] = words;
        if words.contains(&0) || words.iter().any(|&w| w.cast_signed() < 0) {
            return Ok(None);
        }
        Ok(Some(Self {
            mul: (mx, my),
            div: (dx, dy),
        }))
    }

    /// The pixels `pixels` source pixels become along an axis.
    fn scaled(pixels: u32, mul: u32, div: u32) -> u32 {
        u32::try_from(u64::from(pixels) * u64::from(mul) / u64::from(div)).unwrap_or(u32::MAX)
    }

    /// The source pixel destination pixel `i` along an axis takes: the last
    /// whose span reaches past the pixel's far edge.
    fn source(i: u32, mul: u32, div: u32) -> u32 {
        let far = (u64::from(i) + 1) * u64::from(div);
        u32::try_from(far.div_ceil(u64::from(mul)) - 1).unwrap_or(u32::MAX)
    }

    /// The size `width` by `height` pixels become.
    pub(crate) fn size(self, width: u32, height: u32) -> (u32, u32) {
        (
            Self::scaled(width, self.mul.0, self.div.0),
            Self::scaled(height, self.mul.1, self.div.1),
        )
    }

    /// The source column destination column `i` takes.
    pub(crate) fn column(self, i: u32) -> u32 {
        Self::source(i, self.mul.0, self.div.0)
    }

    /// The source columns destination columns from `i` on take, in turn.
    pub(crate) fn columns_from(self, i: u32) -> Columns {
        let (mul, div) = (u64::from(self.mul.0), u64::from(self.div.0));
        let far = (u64::from(i) + 1) * div;
        Columns {
            quotient: far / mul,
            remainder: far % mul,
            mul,
            step: (div / mul, div % mul),
        }
    }

    /// The source row, from the bottom, destination row `j` from the bottom
    /// takes.
    pub(crate) fn row(self, j: u32) -> u32 {
        Self::source(j, self.mul.1, self.div.1)
    }

    /// The rows, from the top, of a sprite of `height` rows plotted from row
    /// `at_y` that the destination's rows of `bounds` take.
    pub(crate) fn rows_shown(self, height: u32, at_y: i32, bounds: Rect) -> Range<u32> {
        self.rows_taken(
            height,
            bounds.bottom.abs_diff(at_y),
            bounds.top.abs_diff(at_y),
        )
    }

    /// The rows, from the top, of a sprite of `height` rows that destination
    /// rows `low..=high` up from its bottom take.
    pub(crate) fn rows_taken(self, height: u32, low: u32, high: u32) -> Range<u32> {
        height.saturating_sub(self.row(high).saturating_add(1))
            ..height.saturating_sub(self.row(low))
    }
}

/// The destination as a plot draws on it: its pixels, its memory and its
/// graphics window in pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Destination {
    pub(crate) format: Format,
    pub(crate) canvas: Canvas,
    pub(crate) window: Rect,
}

impl Destination {
    pub(crate) fn now(os: &mut Os<'_>) -> Result<Self, Fail> {
        state::with(os, |vdu, _| {
            Ok(Self {
                format: *vdu.format(),
                canvas: vdu.canvas(),
                window: vdu.graphics.window,
            })
        })
    }

    /// The rectangle from `(left, bottom)` of `width` by `height` pixels
    /// within the window: its columns and its rows, from the bottom, or none.
    pub(crate) fn visible(&self, (left, bottom): Point, width: u32, height: u32) -> Option<Rect> {
        let right = i64::from(left) + i64::from(width) - 1;
        let top = i64::from(bottom) + i64::from(height) - 1;
        let w = self.window;
        let rect = Rect {
            left: left.max(w.left),
            bottom: bottom.max(w.bottom),
            right: i32::try_from(right.min(i64::from(w.right))).ok()?,
            top: i32::try_from(top.min(i64::from(w.top))).ok()?,
        };
        (width > 0 && height > 0 && rect.left <= rect.right && rect.bottom <= rect.top)
            .then_some(rect)
    }
}

/// A point in OS units from the origin, in the destination's pixels.
pub(crate) fn at(os: &mut Os<'_>, x: u32, y: u32) -> Result<Point, Fail> {
    state::with(os, |vdu, _| {
        Ok(vdu.pixels_of(x.cast_signed(), y.cast_signed()))
    })
}

/// The graphics cursor, in pixels.
pub(crate) fn cursor(os: &mut Os<'_>) -> Result<Point, Fail> {
    state::with(os, |vdu, _| Ok(vdu.graphics.cursor_pixels))
}

/// The source columns successive destination columns take, as
/// [`Scale::column`] gives each: its far edge's span stepped on by a column's
/// share of the source each time, so a row costs no division a pixel.
pub(crate) struct Columns {
    quotient: u64,
    remainder: u64,
    mul: u64,
    step: (u64, u64),
}

impl Iterator for Columns {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let source = self.quotient + u64::from(self.remainder != 0) - 1;
        self.quotient += self.step.0;
        self.remainder += self.step.1;
        if self.remainder >= self.mul {
            self.remainder -= self.mul;
            self.quotient += 1;
        }
        Some(u32::try_from(source).unwrap_or(u32::MAX))
    }
}

/// Where each destination pixel of a scaled or transformed plot takes its
/// source pixel from, a row at a time.
pub(crate) trait Mapping {
    /// Begins row `y`: the columns of `left..=right` the plot covers on it,
    /// if any.
    fn row(&mut self, y: i32, left: i32, right: i32) -> Option<(i32, i32)>;

    /// The source pixel the row's next column takes, from its first: a
    /// column and a row from the top.
    fn next(&mut self) -> (u32, u32);
}
