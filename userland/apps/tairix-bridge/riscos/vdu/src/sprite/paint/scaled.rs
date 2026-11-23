//! SpriteExtend's scaled plots, `OS_SpriteOp` 50, 51, 52 and 65, pixel for
//! pixel: a sprite of `w` by `h` scaled by `mul / div` covers `w * mul / div`
//! by `h * mul / div` pixels from its bottom left, each taking the source
//! pixel whose span reaches past its far edge. 52 paints the sprite, 65 tiles
//! it over the graphics window, 50 plots the background colour under its
//! mask, and 51 the foreground under a character's glyph.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::output::workspace;

use tairix_vdu::font::Glyph;
use tairix_vdu::mode::repeated;

use super::super::header::Sprite;
use super::super::NO_MEMORY;
use super::colours::{
    alpha_blended, blends, converted_first, mapped, Conversion, Converted, Palettised,
};
use super::source::{Checked, Source};
use super::{Columns, Destination, Mapping, PlotFlags, Scale};
use crate::blendtable;
use crate::canvas::Block;
use crate::changed::Rect;
use crate::colour::{combine, Direct};
use crate::inverse;
use crate::palette::EIGHT_BIT_COLOURS;
use crate::raster::Point;
use crate::state;

/// A sprite of `height` rows scaled from `at`.
struct Scaled {
    at: Point,
    scale: Scale,
    height: u32,
    columns: Columns,
    row: u32,
}

impl Scaled {
    fn new(at: Point, scale: Scale, height: u32) -> Self {
        Self {
            at,
            scale,
            height,
            columns: scale.columns_from(0),
            row: 0,
        }
    }
}

impl Mapping for Scaled {
    fn row(&mut self, y: i32, left: i32, right: i32) -> Option<(i32, i32)> {
        self.row = self.height - 1 - self.scale.row((y - self.at.1).cast_unsigned());
        self.columns = self.scale.columns_from((left - self.at.0).cast_unsigned());
        Some((left, right))
    }

    fn next(&mut self) -> (u32, u32) {
        (self.columns.next().unwrap_or(u32::MAX), self.row)
    }
}

/// A sprite scaled to `size`, repeated across and down from `at`.
struct Tiled {
    tile: Scaled,
    size: (i64, i64),
    /// The next column's place across its tile.
    across: i64,
}

impl Mapping for Tiled {
    fn row(&mut self, y: i32, left: i32, right: i32) -> Option<(i32, i32)> {
        let j = (i64::from(y) - i64::from(self.tile.at.1)).rem_euclid(self.size.1);
        let j = u32::try_from(j).ok()?;
        self.tile.row = self.tile.height - 1 - self.tile.scale.row(j);
        self.across = (i64::from(left) - i64::from(self.tile.at.0)).rem_euclid(self.size.0);
        self.tile.columns = self
            .tile
            .scale
            .columns_from(u32::try_from(self.across).unwrap_or(0));
        Some((left, right))
    }

    fn next(&mut self) -> (u32, u32) {
        if self.across == self.size.0 {
            self.across = 0;
            self.tile.columns = self.tile.scale.columns_from(0);
        }
        self.across += 1;
        (self.tile.columns.next().unwrap_or(u32::MAX), self.tile.row)
    }
}

/// `OS_SpriteOp` 52: the sprite at `at`, scaled by the factors at R6 and
/// turned into the destination's pixels by R7's table.
pub(crate) fn put_scaled(
    os: &mut Os<'_>,
    sprite: &Sprite,
    at: Point,
    flags: PlotFlags,
    scale: u32,
    table: u32,
) -> Result<(), Fail> {
    let Some(Prepared {
        checked,
        scale,
        size: (width, height),
        destination,
    }) = prepared(os, sprite, scale)?
    else {
        return Ok(());
    };
    if destination.visible(at, width, height).is_none() {
        return Ok(());
    }
    let (mut source, converted, destination) =
        converted_first(os, &checked, &destination, flags, table)?;
    let Some(bounds) = destination
        .visible(at, width, height)
        .filter(|_| converted.draws())
    else {
        return Ok(());
    };
    if !mapped(&checked.pixels, flags) {
        let rows = scale.rows_shown(checked.height, at.1, bounds);
        checked.read_rows(os, &mut source, rows)?;
    }
    let mut mapping = Scaled::new(at, scale, source.height);
    paint(
        os,
        &source,
        &destination,
        &converted,
        flags,
        bounds,
        &mut mapping,
    )
}

/// `OS_SpriteOp` 65: as 52, the sprite repeated across and down from `at`
/// over the whole graphics window.
pub(crate) fn tile(
    os: &mut Os<'_>,
    sprite: &Sprite,
    at: Point,
    flags: PlotFlags,
    scale: u32,
    table: u32,
) -> Result<(), Fail> {
    let Some(Prepared {
        checked,
        scale,
        size: (width, height),
        destination,
    }) = prepared(os, sprite, scale)?
    else {
        return Ok(());
    };
    if width == 0 || height == 0 {
        return Ok(());
    }
    let (mut source, converted, destination) =
        converted_first(os, &checked, &destination, flags, table)?;
    if !converted.draws() {
        return Ok(());
    }
    if !mapped(&checked.pixels, flags) {
        let rows = tile_rows(scale, checked.height, at.1, height, destination.window);
        checked.read_rows(os, &mut source, rows)?;
    }
    let mut mapping = Tiled {
        tile: Scaled::new(at, scale, source.height),
        size: (i64::from(width), i64::from(height)),
        across: 0,
    };
    paint(
        os,
        &source,
        &destination,
        &converted,
        flags,
        destination.window,
        &mut mapping,
    )
}

/// A scaled plot of a sprite, before it reads any of its rows.
struct Prepared<'a> {
    checked: Checked<'a>,
    scale: Scale,
    /// The pixels across and down it covers.
    size: (u32, u32),
    destination: Destination,
}

/// A scaled plot of `sprite` by the factors at `scale`: none where a factor
/// of nought draws nothing.
fn prepared<'a>(
    os: &mut Os<'_>,
    sprite: &'a Sprite,
    scale: u32,
) -> Result<Option<Prepared<'a>>, Fail> {
    let checked = Source::check(os, sprite)?;
    let Some(scale) = Scale::read(os, scale)? else {
        return Ok(None);
    };
    let size = scale.size(checked.width, checked.height);
    Ok(Some(Prepared {
        checked,
        scale,
        size,
        destination: Destination::now(os)?,
    }))
}

/// The rows, from the top, of a sprite of `height` rows tiled from row `at_y`
/// in tiles `tile` high that the rows of `window` show: all of them where the
/// window holds a tile's edge.
fn tile_rows(scale: Scale, height: u32, at_y: i32, tile: u32, window: Rect) -> Range<u32> {
    let up = |y: i32| {
        u32::try_from((i64::from(y) - i64::from(at_y)).rem_euclid(i64::from(tile))).unwrap_or(0)
    };
    let (low, high) = (up(window.bottom), up(window.top));
    if window.top.abs_diff(window.bottom) >= tile || low > high {
        0..height
    } else {
        scale.rows_taken(height, low, high)
    }
}

/// A picture of `width` by `height` RGBA pixels, the top row first, which
/// `decode` gives only where some of it shows, plotted from `at` scaled by
/// `scale`, through the colour mapping descriptor at `mapping` where one is
/// given.
pub(crate) fn put_picture(
    os: &mut Os<'_>,
    (width, height): (u32, u32),
    decode: impl FnOnce(&mut Os<'_>) -> Result<Vec<u8>, Fail>,
    at: Point,
    scale: Scale,
    flags: PlotFlags,
    mapping: Option<u32>,
) -> Result<(), Fail> {
    let (across, down) = scale.size(width, height);
    let destination = Destination::now(os)?;
    if destination.visible(at, across, down).is_none() {
        return Ok(());
    }
    let rgba = decode(os)?;
    let source = Source::of_rgba(os, width, height, rgba)?;
    let (converted, destination) = Converted::of_picture(os, &source, &destination, mapping)?;
    let Some(bounds) = destination.visible(at, across, down) else {
        return Ok(());
    };
    let mut mapping = Scaled::new(at, scale, height);
    paint(
        os,
        &source,
        &destination,
        &converted,
        flags,
        bounds,
        &mut mapping,
    )
}

/// `OS_SpriteOp` 50: the background colour plotted at `at` under the
/// sprite's mask scaled by the factors at R6, solid where it has none.
pub(crate) fn plot_mask_scaled(
    os: &mut Os<'_>,
    sprite: &Sprite,
    at: Point,
    scale: u32,
) -> Result<(), Fail> {
    let Some(Prepared {
        checked,
        scale,
        size: (width, height),
        destination,
    }) = prepared(os, sprite, scale)?
    else {
        return Ok(());
    };
    let Some(bounds) = destination.visible(at, width, height) else {
        return Ok(());
    };
    let rows = scale.rows_shown(checked.height, at.1, bounds);
    let source = checked.read(os, rows)?;
    let mut mapping = Scaled::new(at, scale, source.height);
    let solid = |(x, row): (u32, u32)| source.alpha(x, row) != 0;
    plot_block(
        os,
        &destination,
        workspace::BACKGROUND,
        bounds,
        &mut mapping,
        solid,
    )
}

/// `OS_SpriteOp` 51: character `c` of the soft font in the foreground colour
/// at `at`, its eight by eight cell scaled by the factors at R6.
pub(crate) fn paint_character(os: &mut Os<'_>, c: u8, at: Point, scale: u32) -> Result<(), Fail> {
    let Some(scale) = Scale::read(os, scale)? else {
        return Ok(());
    };
    let glyph = state::with(os, |vdu, _| {
        Ok(vdu.font.glyph(c).copied().unwrap_or_default())
    })?;
    let (width, height) = scale.size(GLYPH_COLUMNS, GLYPH_ROWS);
    let destination = Destination::now(os)?;
    let Some(bounds) = destination.visible(at, width, height) else {
        return Ok(());
    };
    let mut mapping = Scaled::new(at, scale, GLYPH_ROWS);
    let solid = |(x, row): (u32, u32)| {
        glyph
            .get(row as usize)
            .is_some_and(|&line| (u32::from(line) << x) & 0x80 != 0)
    };
    plot_block(
        os,
        &destination,
        workspace::FOREGROUND,
        bounds,
        &mut mapping,
        solid,
    )
}

/// A soft-font character's rows, each a byte of its columns.
#[expect(clippy::cast_possible_truncation, reason = "eight rows")]
const GLYPH_ROWS: u32 = size_of::<Glyph>() as u32;
const GLYPH_COLUMNS: u32 = u8::BITS;

/// Plots the colour block at zero page `offset` over the pixels within
/// `bounds` whose source pixels `solid` says are.
pub(crate) fn plot_block(
    os: &mut Os<'_>,
    destination: &Destination,
    offset: u32,
    bounds: Rect,
    mapping: &mut impl Mapping,
    solid: impl Fn((u32, u32)) -> bool,
) -> Result<(), Fail> {
    let block = Block::read(os, offset)?;
    let canvas = destination.canvas;
    for y in bounds.bottom..=bounds.top {
        let Some((left, right)) = mapping.row(y, bounds.left, bounds.right) else {
            continue;
        };
        let row = canvas.row_of(y);
        let (ora, eor) = block.for_line(row);
        let mut run = None;
        for x in left..=right {
            match (solid(mapping.next()), run) {
                (true, None) => run = Some(x),
                (false, Some(start)) => {
                    canvas.span(
                        os,
                        row,
                        start.cast_unsigned(),
                        (x - 1).cast_unsigned(),
                        ora,
                        eor,
                    )?;
                    run = None;
                }
                _ => {}
            }
        }
        if let Some(start) = run {
            canvas.span(
                os,
                row,
                start.cast_unsigned(),
                right.cast_unsigned(),
                ora,
                eor,
            )?;
        }
    }
    crate::changed::cover(os, bounds)
}

/// Paints the pixels within `bounds` that `mapping` covers, each from its
/// source pixel by `flags`: converted, under the mask or blended by its
/// alpha, kept from by translucency, combined by the action.
pub(crate) fn paint(
    os: &mut Os<'_>,
    source: &Source,
    destination: &Destination,
    converted: &Converted,
    flags: PlotFlags,
    bounds: Rect,
    mapping: &mut impl Mapping,
) -> Result<(), Fail> {
    let conversion = &converted.conversion;
    if matches!(conversion, Conversion::Nothing) {
        return Ok(());
    }
    let format = destination.format;
    let canvas = destination.canvas;
    let mask = format.pixel_mask();
    // A mode doubling its pixels holds each value repeated across a field
    // twice its pixel's bits.
    let (bits, field) = (1 << format.log2_bpp(), 1 << format.log2_bpc());
    let opacity = OPAQUE - flags.translucency();
    let meeting = Meeting::of(
        source,
        &format,
        conversion,
        converted.palettised.as_ref(),
        flags,
    );
    let mut line = Vec::new();
    let width = usize::try_from(i64::from(bounds.right) - i64::from(bounds.left) + 1).unwrap_or(0);
    if line.try_reserve_exact(width).is_err() {
        return Err(os.error(NO_MEMORY));
    }
    for y in bounds.bottom..=bounds.top {
        let Some((left, right)) = mapping.row(y, bounds.left, bounds.right) else {
            continue;
        };
        let row = canvas.row_of(y);
        line.clear();
        canvas.pixels(
            os,
            row,
            left.cast_unsigned(),
            right.cast_unsigned(),
            &mut line,
        )?;
        for slot in &mut line {
            let (sx, sy) = mapping.next();
            let Some((alpha, colour)) = source.pixel(sx, sy, flags.masked()) else {
                continue;
            };
            let alpha = u32::from(alpha);
            let old = *slot & mask;
            let drawn = match &meeting {
                Meeting::Combined => conversion
                    .pixel(colour)
                    .map(|pixel| combine(flags.action(), old, pixel) & mask),
                Meeting::Channels(blending) => {
                    blending.pixel(conversion, colour, old, weight(alpha, opacity))
                }
                Meeting::Palette(blending) => conversion
                    .pixel(colour)
                    .and_then(|pixel| blending.pixel(pixel, old, alpha, opacity)),
            };
            if let Some(drawn) = drawn {
                *slot = repeated(drawn, bits, field);
            }
        }
        canvas.put_pixels(os, row, left.cast_unsigned(), &line)?;
    }
    crate::changed::cover(os, bounds)
}

/// How a plot's pixels meet the destination's.
enum Meeting<'a> {
    /// Each combined with the destination's pixel by the GCOL action.
    Combined,
    /// Blended into a direct-colour destination's channels.
    Channels(ChannelBlend),
    /// Blended onto a palettised destination.
    Palette(PaletteBlend<'a>),
}

impl<'a> Meeting<'a> {
    /// A plot of `source` by `flags` through `conversion` on a destination of
    /// `format`, whose colours `palettised` gives where it is palettised and
    /// blended on. A blend onto a destination ColourTrans reads as
    /// direct-colour is combined unblended.
    fn of(
        source: &Source,
        format: &crate::mode::Format,
        conversion: &Conversion,
        palettised: Option<&'a Palettised>,
        flags: PlotFlags,
    ) -> Self {
        if !blends(source, flags) {
            return Self::Combined;
        }
        if format.direct() {
            let from = match conversion {
                Conversion::Direct(from, _) => Some(*from),
                _ => None,
            };
            return Self::Channels(ChannelBlend::new(from, Direct::of(format)));
        }
        palettised.map_or(Self::Combined, |palettised| {
            Self::Palette(PaletteBlend {
                palettised,
                alpha: alpha_blended(source, flags),
                full: palettised.colours.len() == EIGHT_BIT_COLOURS,
            })
        })
    }
}

/// A blend onto a palettised destination: the colour of the source pixel's
/// colour number mixed with the destination pixel's, then matched in the
/// palette, as RISC OS 5 blends.
struct PaletteBlend<'a> {
    palettised: &'a Palettised,
    /// The sprite's alpha is blended, not only a translucency.
    alpha: bool,
    /// The palette has 256 colours: each mix is found in its 32K table.
    full: bool,
}

impl PaletteBlend<'_> {
    /// Colour number `over` drawn on `under` at alpha `alpha` of 255 and
    /// `opacity` of 256; none where nothing is drawn.
    ///
    /// With 256 colours a translucency mixes the two colours as `BlendTable`
    /// does and an alpha mixes their five-bit forms, each found in the 32K
    /// table. With fewer, an alpha is taken in steps of 32, and each mix
    /// takes the palette's nearest colour, as `BlendTable` matches.
    fn pixel(&self, over: u32, under: u32, alpha: u32, opacity: u32) -> Option<u32> {
        let (over, under) = (self.palettised.colour(over), self.palettised.colour(under));
        if self.full {
            let weight = weight(alpha, opacity);
            if !self.alpha {
                let mix = blendtable::mix(over, under, OPAQUE - (weight >> 8));
                return Some(self.palettised.number(mix));
            }
            let (over, under) = (over.to_array(), under.to_array());
            let mix = core::array::from_fn(|c| {
                let [s, d] = [over[c], under[c]].map(|v| u32::from(v) >> (u8::BITS - FIVE_BITS));
                channel(s, FIVE_BITS, d, FIVE_BITS, weight).to_le_bytes()[0]
            });
            return Some(self.palettised.number_of_cell(inverse::cell(mix)));
        }
        let share = if self.alpha {
            ((alpha * opacity) >> 8) & !ALPHA_STEP_LOW
        } else {
            opacity
        };
        if share == 0 {
            return None;
        }
        let mix = blendtable::mix(over, under, OPAQUE - share);
        Some(u32::from(blendtable::nearest_last(
            &self.palettised.colours,
            mix,
        )))
    }
}

/// The bits below an alpha's step of 32, which a destination of fewer than
/// 256 colours drops.
const ALPHA_STEP_LOW: u32 = 31;

/// Full opacity, a blend's whole: an alpha of 255 widened to 256 so a solid
/// pixel blends to itself.
const OPAQUE: u32 = blendtable::WHOLE;

/// A blend into a direct-colour destination's channels.
struct ChannelBlend {
    /// The source's own pixel form, where the blend takes its pixels as they
    /// are; else it takes them turned into the destination's.
    from: Option<Direct>,
    to: Direct,
    /// The bits each channel of the pixels blended over takes, and each of
    /// the destination's: the same for every pixel of a plot.
    over_bits: [u32; 3],
    bits: [u32; 3],
}

impl ChannelBlend {
    const fn new(from: Option<Direct>, to: Direct) -> Self {
        let over = match from {
            Some(from) => from,
            None => to,
        };
        Self {
            from,
            to,
            over_bits: over.channel_bits(),
            bits: to.channel_bits(),
        }
    }

    /// Source pixel `colour`, through `conversion` where it must be, drawn on
    /// `under` with the source's share `weight` of 65536; none where the
    /// conversion draws nothing.
    fn pixel(&self, conversion: &Conversion, colour: u32, under: u32, weight: u32) -> Option<u32> {
        let (over, from) = match self.from {
            Some(from) => (colour, from),
            None => (conversion.pixel(colour)?, self.to),
        };
        let (s, d) = (from.channels(over), self.to.channels(under));
        Some(self.to.pack_channels(core::array::from_fn(|c| {
            channel(s[c], self.over_bits[c], d[c], self.bits[c], weight)
        })))
    }
}

/// The bits of a channel of a 32K table's colours: a 1:5:5:5 pixel's.
const FIVE_BITS: u32 = Direct::FIFTEEN.channel_bits()[0];

/// A source's share of a blend in 65536ths: its alpha, 255 solid, widened to
/// 256 and kept from by `opacity` of 256.
const fn weight(alpha: u32, opacity: u32) -> u32 {
    (alpha + (alpha >> 7)) * opacity
}

/// Channel `s` of `source_bits` over channel `d` of `bits`, the source's
/// share `weight` of 65536, each share rounded down apart, as RISC OS 5
/// blends: an eight-bit source's share cut to 256ths, a narrower one's kept
/// whole, its bits repeated below themselves where the destination's channel
/// is wider.
const fn channel(s: u32, source_bits: u32, d: u32, bits: u32, weight: u32) -> u32 {
    if source_bits >= 8 {
        let share = weight >> 8;
        return ((s * share) >> (8 + source_bits).saturating_sub(bits))
            + ((d * (OPAQUE - share)) >> 8);
    }
    let (s, source_bits) = if source_bits < bits {
        ((s << source_bits) | s, 2 * source_bits)
    } else {
        (s, source_bits)
    };
    ((s * weight) >> (16 + source_bits).saturating_sub(bits))
        + ((d * (WHOLE_WEIGHT - weight)) >> 16)
}

/// A blend's whole in 65536ths, as [`weight`] counts.
const WHOLE_WEIGHT: u32 = OPAQUE * OPAQUE;

#[cfg(test)]
mod tests;
