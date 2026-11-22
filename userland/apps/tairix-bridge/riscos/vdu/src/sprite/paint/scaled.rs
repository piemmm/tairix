//! SpriteExtend's scaled plots, `OS_SpriteOp` 50, 51, 52 and 65, pixel for
//! pixel: a sprite of `w` by `h` scaled by `mul / div` covers `w * mul / div`
//! by `h * mul / div` pixels from its bottom left, each taking the source
//! pixel whose span reaches past its far edge. 52 paints the sprite, 65 tiles
//! it over the graphics window, 50 plots the background colour under its
//! mask, and 51 the foreground under a character's glyph.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::output::workspace;

use super::super::header::Sprite;
use super::super::NO_MEMORY;
use super::blit::combine;
use super::colours::{alpha_blended, blends, Conversion, Converted, Palettised};
use super::source::Source;
use super::{changed, read, Destination, Mapping, PlotFlags, Scale};
use crate::blendtable;
use crate::canvas::Block;
use crate::changed::Rect;
use crate::colour::Direct;
use crate::inverse;
use crate::palette::EIGHT_BIT_COLOURS;
use crate::raster::Point;
use crate::state;

/// A sprite of `height` rows scaled from `at`.
struct Scaled {
    at: Point,
    scale: Scale,
    height: u32,
    column: u32,
    row: u32,
}

impl Scaled {
    const fn new(at: Point, scale: Scale, height: u32) -> Self {
        Self {
            at,
            scale,
            height,
            column: 0,
            row: 0,
        }
    }
}

impl Mapping for Scaled {
    fn row(&mut self, y: i32, left: i32, right: i32) -> Option<(i32, i32)> {
        self.row = self.height - 1 - self.scale.row((y - self.at.1).cast_unsigned());
        self.column = (left - self.at.0).cast_unsigned();
        Some((left, right))
    }

    fn next(&mut self) -> (u32, u32) {
        let column = self.scale.column(self.column);
        self.column += 1;
        (column, self.row)
    }
}

/// A sprite scaled to `size`, repeated across and down from `at`.
struct Tiled {
    tile: Scaled,
    size: (i64, i64),
    x: i32,
}

impl Mapping for Tiled {
    fn row(&mut self, y: i32, left: i32, right: i32) -> Option<(i32, i32)> {
        let j = (i64::from(y) - i64::from(self.tile.at.1)).rem_euclid(self.size.1);
        let j = u32::try_from(j).ok()?;
        self.tile.row = self.tile.height - 1 - self.tile.scale.row(j);
        self.x = left;
        Some((left, right))
    }

    fn next(&mut self) -> (u32, u32) {
        let i = (i64::from(self.x) - i64::from(self.tile.at.0)).rem_euclid(self.size.0);
        self.x += 1;
        (
            self.tile.scale.column(u32::try_from(i).unwrap_or(0)),
            self.tile.row,
        )
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
    let source = read(os, sprite)?;
    let Some(scale) = Scale::read(os, scale)? else {
        return Ok(());
    };
    let destination = Destination::now(os)?;
    let converted = Converted::of(os, &source, &destination.format, flags, table)?;
    let (width, height) = scale.size(source.width, source.height);
    let Some(bounds) = destination.visible(at, width, height) else {
        return Ok(());
    };
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
    let source = read(os, sprite)?;
    let Some(scale) = Scale::read(os, scale)? else {
        return Ok(());
    };
    let destination = Destination::now(os)?;
    let converted = Converted::of(os, &source, &destination.format, flags, table)?;
    let (width, height) = scale.size(source.width, source.height);
    if width == 0 || height == 0 {
        return Ok(());
    }
    let mut mapping = Tiled {
        tile: Scaled::new(at, scale, source.height),
        size: (i64::from(width), i64::from(height)),
        x: 0,
    };
    let bounds = destination.window;
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

/// A decoded picture of `width` by `height` RGBA pixels, the top row first,
/// plotted from `at` scaled by `scale`, through the colour mapping
/// descriptor at `mapping` where one is given.
pub(crate) fn put_picture(
    os: &mut Os<'_>,
    (width, height): (u32, u32),
    rgba: &[u8],
    at: Point,
    scale: Scale,
    flags: PlotFlags,
    mapping: Option<u32>,
) -> Result<(), Fail> {
    let source = Source::of_rgba(os, width, height, rgba)?;
    let destination = Destination::now(os)?;
    let converted = Converted::of_picture(os, &source, &destination.format, mapping)?;
    let (across, down) = scale.size(width, height);
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
    let source = read(os, sprite)?;
    let Some(scale) = Scale::read(os, scale)? else {
        return Ok(());
    };
    let (width, height) = scale.size(source.width, source.height);
    let destination = Destination::now(os)?;
    let Some(bounds) = destination.visible(at, width, height) else {
        return Ok(());
    };
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
    let (width, height) = scale.size(GLYPH_SIDE, GLYPH_SIDE);
    let destination = Destination::now(os)?;
    let Some(bounds) = destination.visible(at, width, height) else {
        return Ok(());
    };
    let mut mapping = Scaled::new(at, scale, GLYPH_SIDE);
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

/// The pixels across and down a soft-font character's cell.
const GLYPH_SIDE: u32 = 8;

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
    changed(os, bounds)
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
            let alpha = if flags.masked() {
                u32::from(source.alpha(sx, sy))
            } else {
                0xFF
            };
            if alpha == 0 {
                continue;
            }
            let colour = source.colour(sx, sy);
            let drawn = match &meeting {
                Meeting::Combined => conversion
                    .pixel(colour)
                    .map(|pixel| combine(flags.action(), *slot, pixel) & mask),
                Meeting::Channels(blending) => {
                    blending.pixel(conversion, colour, *slot, weight(alpha, opacity))
                }
                Meeting::Palette(blending) => conversion
                    .pixel(colour)
                    .and_then(|pixel| blending.pixel(pixel, *slot, alpha, opacity)),
            };
            if let Some(drawn) = drawn {
                *slot = drawn;
            }
        }
        canvas.put_pixels(os, row, left.cast_unsigned(), &line)?;
    }
    changed(os, bounds)
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
            return Self::Channels(ChannelBlend {
                from,
                to: Direct::of(format),
            });
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
                let [s, d] = [over[c], under[c]].map(|v| u32::from(v >> 3));
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
}

impl ChannelBlend {
    /// Source pixel `colour`, through `conversion` where it must be, drawn on
    /// `under` with the source's share `weight` of 65536; none where the
    /// conversion draws nothing.
    fn pixel(&self, conversion: &Conversion, colour: u32, under: u32, weight: u32) -> Option<u32> {
        let (over, from) = match self.from {
            Some(from) => (colour, from),
            None => (conversion.pixel(colour)?, self.to),
        };
        let (s, source_bits) = (from.channels(over), from.channel_bits());
        let (d, bits) = (self.to.channels(under), self.to.channel_bits());
        Some(self.to.pack_channels(core::array::from_fn(|c| {
            channel(s[c], source_bits[c], d[c], bits[c], weight)
        })))
    }
}

/// The bits of a five-bit channel.
const FIVE_BITS: u32 = 5;

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
