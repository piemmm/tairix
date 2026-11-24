//! What a module drawing its own shapes and glyphs reaches of the VDU
//! drivers, as RISC OS's Draw module and Font Manager reach them: where
//! output goes, runs of pixels plotted, levels of coverage painted in a
//! table's pixels, and a logical colour set through `PaletteV`.

use tairix_riscos_kernel::mouse::Bounds;
use tairix_riscos_kernel::os::{Fail, Os};

use alloc::vec::Vec;

use crate::canvas::{Block, Canvas};
use crate::changed::{cover, Rect};
use crate::colour::{replicate, Direct};
use crate::palette::{kind, set_through};
use crate::plot::{ink_block, widen, Ink};
use crate::sprite::alpha::widened;
use crate::sprite::Palettised;
use crate::state::{read, with, Vdu};

/// What a renderer needs of where output goes: the screen, or the sprite
/// output is switched to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Each axis's eigen factor: an OS unit's share of a pixel, as a shift.
    pub eig: (u32, u32),
    /// The graphics origin, in OS units from the bottom left.
    pub origin: (i32, i32),
    /// The graphics window, in pixels, inclusive.
    pub window: Bounds,
    /// The graphics cursor, then the two points it visited before, in
    /// pixels.
    pub cursors: [(i32, i32); 3],
    /// The bits a pixel takes, as a power of two.
    pub log2_bpp: u32,
    /// Whether its pixels keep a byte or a nibble beside their colour, as
    /// 16M and 4K colours' do, for an alpha or supremacy value.
    pub tagged: bool,
    /// Whether its pixels are their colours rather than a palette's
    /// entries.
    pub direct: bool,
}

/// Where output goes now; `None` where it cannot show graphics.
pub fn surface(os: &mut Os<'_>) -> Option<Surface> {
    read(os, |vdu, _| {
        let format = vdu.format();
        let graphics = &vdu.graphics;
        format.graphic().then(|| Surface {
            eig: (format.x_eig(), format.y_eig()),
            origin: graphics.origin,
            window: graphics.window,
            cursors: [graphics.cursor_pixels, graphics.old, graphics.older],
            log2_bpp: format.log2_bpp(),
            tagged: format.direct() && Direct::of(format).tagged(),
            direct: format.direct(),
        })
    })
    .flatten()
}

/// Plots `spans`, each a row and its first and last column in pixels, in
/// the graphics foreground, within the graphics window.
///
/// # Errors
///
/// The abort a write to the screen takes.
pub fn plot_foreground(os: &mut Os<'_>, spans: &[(i32, i32, i32)]) -> Result<(), Fail> {
    let block = ink_block(os, Ink::Foreground)?;
    plot(os, spans.iter().copied(), &block)
}

/// Plots `spans` in the pixel `value`, stored, or where `invert`
/// exclusive-ored onto what is there, within the graphics window.
///
/// # Errors
///
/// The abort a write to the screen takes.
pub fn plot_value(
    os: &mut Os<'_>,
    spans: &[(i32, i32, i32)],
    value: u32,
    invert: bool,
) -> Result<(), Fail> {
    with(os, |vdu, os| {
        if !vdu.format().graphic() {
            return Ok(());
        }
        let word = replicate(vdu.format(), value);
        let block = if invert {
            Block::solid(0, word)
        } else {
            Block::solid(u32::MAX, !word)
        };
        vdu.draw_spans(os, spans.iter().copied(), &block)
    })
}

/// Plots `spans` with `block`, where output can show graphics.
pub(crate) fn plot(
    os: &mut Os<'_>,
    spans: impl IntoIterator<Item = (i32, i32, i32)>,
    block: &Block,
) -> Result<(), Fail> {
    with(os, |vdu, os| {
        if !vdu.format().graphic() {
            return Ok(());
        }
        vdu.draw_spans(os, spans, block)
    })
}

/// Levels of coverage, from nought for none to fifteen for whole, over a
/// rectangle of pixels, row after row from the top.
#[derive(Clone, Copy, Debug)]
pub struct Levels<'a> {
    /// The rectangle's leftmost column, in pixels from the left.
    pub left: i32,
    /// Its top row, in pixels from the bottom.
    pub top: i32,
    /// Its pixels a row.
    pub width: usize,
    /// Each pixel's level.
    pub levels: &'a [u8],
}

/// The highest level, a pixel wholly covered.
pub const WHOLE: u8 = 15;

/// Paints `levels` within the graphics window, each level above nought
/// storing its entry of `table`, a pixel value; a level of nought leaves
/// its pixel as it was.
///
/// # Errors
///
/// The abort a write to the screen takes.
pub fn paint_levels(
    os: &mut Os<'_>,
    levels: &Levels<'_>,
    table: &[u32; WHOLE as usize + 1],
) -> Result<(), Fail> {
    with(os, |vdu, os| {
        if !vdu.format().graphic() || levels.width == 0 {
            return Ok(());
        }
        let drawn = paint_rows(vdu, os, levels, table)?;
        match drawn {
            Some(rect) => cover(os, rect),
            None => Ok(()),
        }
    })
}

/// Paints `levels` within the graphics window blended toward `foreground`,
/// `&BBGGRRSS`, as RISC OS 5's Font Manager blends: each level above nought
/// and below whole takes the colour its pixel shows a level and an eighth of
/// it in sixteenths of the way to the foreground, in a mode of 256 colours
/// at five bits a channel through the palette's 32K table, in a direct-colour
/// mode at each channel's own bits, its other bits kept; a whole level stores
/// the foreground. With `supremacy`, in a mode whose pixels keep a
/// supremacy value beside their colour, that value is the pixel's
/// transparency and `SS` the foreground's, and each pixel and its value are
/// composited. A mode of sixteen colours or fewer does not blend: there each
/// level stores its entry of `table`.
///
/// # Errors
///
/// The abort a read or write of the screen takes, or the RMA's refusal of
/// room for the palette's table.
pub fn blend_levels(
    os: &mut Os<'_>,
    levels: &Levels<'_>,
    table: &[u32; WHOLE as usize + 1],
    foreground: u32,
    supremacy: bool,
) -> Result<(), Fail> {
    let depth = read(os, |vdu, _| vdu.format().log2_bpp()).unwrap_or(0);
    if depth < EIGHT_BITS {
        return paint_levels(os, levels, table);
    }
    let palettised = Palettised::now(os)?;
    with(os, |vdu, os| {
        let format = vdu.format();
        if !format.graphic() || levels.width == 0 {
            return Ok(());
        }
        let towards = if let Some(palettised) = &palettised {
            Towards::palettised(palettised, foreground)
        } else {
            let direct = Direct::of(format);
            let tag = format.pixel_mask() & !direct.colour_bits();
            Towards::direct(direct, tag, foreground, supremacy)
        };
        let drawn = rows(vdu, levels, |canvas, y, left, row| {
            blend_row(os, canvas, y, left, row, &towards)
        })?;
        match drawn {
            Some(rect) => cover(os, rect),
            None => Ok(()),
        }
    })
}

/// The depth, as a power of two, below which a mode does not blend.
const EIGHT_BITS: u32 = 3;
/// What takes an eight-bit channel to the 32K table's five bits.
const FIVE_BIT_SHIFT: u32 = u8::BITS - Direct::FIFTEEN.channel_bits()[0];

/// The colour a blend moves each pixel toward, in the destination's form,
/// and the pixel a whole level stores.
enum Towards<'a> {
    /// A palette's colours at five bits a channel, found again in its 32K
    /// table.
    Palettised {
        palettised: &'a Palettised,
        foreground: [u32; 3],
        whole: u32,
    },
    /// A direct-colour pixel's channels at their own bits.
    Direct {
        direct: Direct,
        foreground: [u32; 3],
        whole: u32,
        supremacy: Option<Supremacy>,
    },
}

/// A blend that composites each pixel's supremacy value: the bits that hold
/// it, and the foreground's channels and transparency at eight bits.
struct Supremacy {
    tag: u32,
    foreground: [u32; 3],
    transparency: u32,
}

impl Supremacy {
    /// Pixel `screen` of `direct`'s, its supremacy value its transparency,
    /// composited under the foreground at `level`.
    fn composited(&self, direct: Direct, screen: u32, level: u8) -> u32 {
        let shift = self.tag.trailing_zeros();
        let width = self.tag.count_ones();
        let behind = u32::from(widened((screen & self.tag) >> shift, width));
        let shown = direct.unpack_replicated(screen);
        let coverage =
            (share(level) * (SHARES * SHARES - expanded(self.transparency)) + HALF) >> SHARE_BITS;
        let left = (behind * (SHARES - coverage) + SHARES / 2) >> SHARE_SHIFT;
        let covered = SHARES * coverage;
        let total = covered
            + (((SHARES * SHARES - expanded(behind)) * (SHARES * SHARES - covered) + HALF)
                >> SHARE_BITS);
        let weight = (SHARES * covered + total / 2)
            .checked_div(total)
            .unwrap_or(0);
        let mixed = [0, 1, 2]
            .map(|i| (self.foreground[i] * weight + shown[i] * (SHARES - weight)) >> SHARE_SHIFT);
        let colour = direct.pack(mixed.map(|channel| u8::try_from(channel).unwrap_or(u8::MAX)));
        colour | narrowed(left, width) << shift & self.tag
    }
}

/// A level's share of a blend in sixteenths: the level and an eighth of it.
fn share(level: u8) -> u32 {
    u32::from(level) + u32::from(level >> 3)
}

/// A value of eight bits taken to 256ths: 255 is the whole.
const fn expanded(value: u32) -> u32 {
    value + (value >> 7)
}

/// A value of eight bits narrowed to a field of `width` bits.
fn narrowed(value: u32, width: u32) -> u32 {
    value >> 8u32.saturating_sub(width)
}

impl<'a> Towards<'a> {
    /// Toward `foreground`, `&BBGGRRSS`, among `palettised`'s colours.
    fn palettised(palettised: &'a Palettised, foreground: u32) -> Self {
        let [_, red, green, blue] = foreground.to_le_bytes();
        let cell = Direct::FIFTEEN.pack([red, green, blue]);
        Self::Palettised {
            palettised,
            foreground: Direct::FIFTEEN.channels(cell),
            whole: palettised.number_of_cell(cell as usize),
        }
    }

    /// Toward `foreground`, `&BBGGRRSS`, in `direct`'s pixels, whose `tag`
    /// bits keep a supremacy value a blend with `supremacy` composites.
    fn direct(direct: Direct, tag: u32, foreground: u32, supremacy: bool) -> Self {
        let [supreme, red, green, blue] = foreground.to_le_bytes();
        let whole = direct.pack([red, green, blue]);
        Self::Direct {
            direct,
            foreground: direct.channels(whole),
            whole,
            supremacy: (supremacy && tag != 0).then_some(Supremacy {
                tag,
                foreground: [red, green, blue].map(u32::from),
                transparency: u32::from(supreme),
            }),
        }
    }

    /// Pixel `screen` blended toward the foreground at `level`.
    fn blended(&self, screen: u32, level: u8) -> u32 {
        match self {
            Self::Direct {
                direct,
                supremacy: Some(supremacy),
                ..
            } => supremacy.composited(*direct, screen, level),
            Self::Palettised { whole, .. } | Self::Direct { whole, .. } if level >= WHOLE => *whole,
            Self::Palettised {
                palettised,
                foreground,
                ..
            } => {
                let shown = palettised
                    .colour(screen)
                    .to_array()
                    .map(|c| u32::from(c) >> FIVE_BIT_SHIFT);
                let five = [0, 1, 2].map(|i| mixed(shown[i], foreground[i], level));
                palettised.number_of_cell(Direct::FIFTEEN.pack_channels(five) as usize)
            }
            Self::Direct {
                direct, foreground, ..
            } => {
                let shown = direct.channels(screen);
                let channels = [0, 1, 2].map(|i| mixed(shown[i], foreground[i], level));
                screen & !direct.colour_bits() | direct.pack_channels(channels)
            }
        }
    }
}

/// Channel `shown` blended toward `wanted` at `level`.
fn mixed(shown: u32, wanted: u32, level: u8) -> u32 {
    let share = share(level);
    (shown * (SHARES - share) + wanted * share) >> SHARE_SHIFT
}

/// The sixteenths a blend's share is counted in, and 256ths' rounding.
const SHARES: u32 = 16;
const SHARE_SHIFT: u32 = 4;
const SHARE_BITS: u32 = 8;
const HALF: u32 = 1 << (SHARE_BITS - 1);

/// The runs of `row`'s levels above nought blended toward the foreground from
/// column `left` of the line `y` up; whether any was.
fn blend_row(
    os: &mut Os<'_>,
    canvas: &Canvas,
    y: i32,
    left: i32,
    row: &[u8],
    towards: &Towards<'_>,
) -> Result<bool, Fail> {
    let line = canvas.row_of(y);
    let mut shown = Vec::new();
    let mut values = [0u32; RUN];
    each_run(row, left, |column, levels| {
        let last = column + u32::try_from(levels.len() - 1).unwrap_or(0);
        shown.clear();
        canvas.pixels(os, line, column, last, &mut shown)?;
        for ((value, &screen), &level) in values.iter_mut().zip(&shown).zip(levels) {
            *value = towards.blended(screen, level.min(WHOLE));
        }
        canvas.put_pixels(os, line, column, &values[..levels.len()])
    })
}

/// The pixels one pass writes at most.
const RUN: usize = 64;

/// Each run of `row`'s levels above nought, at most [`RUN`] long, handed to
/// `run` with its first column, counted from column `left`; whether there
/// was any.
fn each_run(
    row: &[u8],
    left: i32,
    mut run: impl FnMut(u32, &[u8]) -> Result<(), Fail>,
) -> Result<bool, Fail> {
    let mut any = false;
    let mut x = 0;
    while x < row.len() {
        if row[x] == 0 {
            x += 1;
            continue;
        }
        let start = x;
        while x < row.len() && row[x] != 0 && x - start < RUN {
            x += 1;
        }
        let column = (left + i32::try_from(start).unwrap_or(0)).cast_unsigned();
        run(column, &row[start..x])?;
        any = true;
    }
    Ok(any)
}

/// Each row of `levels` within the window painted; the rectangle drawn on.
fn paint_rows(
    vdu: &Vdu,
    os: &mut Os<'_>,
    levels: &Levels<'_>,
    table: &[u32; WHOLE as usize + 1],
) -> Result<Option<Rect>, Fail> {
    let mask = vdu.format().pixel_mask();
    rows(vdu, levels, |canvas, y, left, seen| {
        paint_row(os, canvas, y, left, seen, table, mask)
    })
}

/// Each row of `levels` within the window, held to it, handed to `row` with
/// the canvas, its line and its first column; the rectangle drawn on, of the
/// rows `row` says it drew on.
fn rows(
    vdu: &Vdu,
    levels: &Levels<'_>,
    mut row: impl FnMut(&Canvas, i32, i32, &[u8]) -> Result<bool, Fail>,
) -> Result<Option<Rect>, Fail> {
    let window = vdu.graphics.window;
    let canvas = vdu.canvas();
    let width = i32::try_from(levels.width).unwrap_or(i32::MAX);
    let left = levels.left.max(window.left);
    let right = levels.left.saturating_add(width - 1).min(window.right);
    if left > right {
        return Ok(None);
    }
    let skip = usize::try_from(left - levels.left).unwrap_or(0);
    let take = usize::try_from(right - left + 1).unwrap_or(0);
    let mut drawn = None;
    let mut y = levels.top;
    for line in levels.levels.chunks(levels.width) {
        if (window.bottom..=window.top).contains(&y) {
            let seen = line
                .get(skip..)
                .map_or(&[][..], |r| &r[..take.min(r.len())]);
            if row(&canvas, y, left, seen)? {
                drawn = Some(widen(
                    drawn,
                    Rect {
                        left,
                        bottom: y,
                        right,
                        top: y,
                    },
                ));
            }
        }
        y = y.saturating_sub(1);
    }
    Ok(drawn)
}

/// The runs of `row`'s levels above nought written from column `left` of
/// the line `y` up; whether any was.
fn paint_row(
    os: &mut Os<'_>,
    canvas: &Canvas,
    y: i32,
    left: i32,
    row: &[u8],
    table: &[u32; WHOLE as usize + 1],
    mask: u32,
) -> Result<bool, Fail> {
    let line = canvas.row_of(y);
    let mut values = [0u32; RUN];
    each_run(row, left, |column, levels| {
        for (value, &level) in values.iter_mut().zip(levels) {
            *value = table[usize::from(level.min(WHOLE))] & mask;
        }
        canvas.put_pixels(os, line, column, &values[..levels.len()])
    })
}

/// Sets logical colour `logical`, both its flash states, to `colour`, a
/// word `&BBGGRR00`, through `PaletteV`, as `Font_SetPalette` sets the
/// colours it anti-aliases between in a mode of up to sixteen colours.
///
/// # Errors
///
/// What a claimant of the vector gives.
pub fn set_logical_colour(os: &mut Os<'_>, logical: u32, colour: u32) -> Result<(), Fail> {
    set_through(os, logical, kind::NORMAL, [colour; 2])
}

#[cfg(test)]
mod tests;
