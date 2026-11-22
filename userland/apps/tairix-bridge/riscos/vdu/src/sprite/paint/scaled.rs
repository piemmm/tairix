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
use super::colours::{Conversion, NearestKept};
use super::source::{Source, Transparency};
use super::{changed, read, Destination, Mapping, PlotFlags, Scale};
use crate::canvas::Block;
use crate::changed::Rect;
use crate::colour::Direct;
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
    let conversion = Conversion::of(os, &source, &destination.format, flags, table)?;
    let (width, height) = scale.size(source.width, source.height);
    let Some(bounds) = destination.visible(at, width, height) else {
        return Ok(());
    };
    let mut mapping = Scaled::new(at, scale, source.height);
    paint(
        os,
        &source,
        &destination,
        &conversion,
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
    let conversion = Conversion::of(os, &source, &destination.format, flags, table)?;
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
        &conversion,
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
    let palette = destination_palette(os, &destination)?;
    let conversion = Conversion::of_picture(os, &source, &destination.format, &palette, mapping)?;
    let (across, down) = scale.size(width, height);
    let Some(bounds) = destination.visible(at, across, down) else {
        return Ok(());
    };
    let mut mapping = Scaled::new(at, scale, height);
    paint(
        os,
        &source,
        &destination,
        &conversion,
        flags,
        bounds,
        &mut mapping,
    )
}

/// The colour each pixel value of a palettised destination shows, none for
/// a deep one.
fn destination_palette(os: &mut Os<'_>, destination: &Destination) -> Result<Vec<[u8; 3]>, Fail> {
    if destination.format.direct() {
        return Ok(Vec::new());
    }
    let colours = 1u32 << destination.format.log2_bpp();
    state::with(os, |vdu, _| {
        Ok((0..colours)
            .map(|pixel| {
                let rgb = vdu.palette.shown(pixel);
                [rgb.r, rgb.g, rgb.b]
            })
            .collect())
    })
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
/// alpha, kept from by translucency, combined by the action. A palettised
/// destination blends the two pixels' colours and takes the palette's
/// nearest.
pub(crate) fn paint(
    os: &mut Os<'_>,
    source: &Source,
    destination: &Destination,
    conversion: &Conversion,
    flags: PlotFlags,
    bounds: Rect,
    mapping: &mut impl Mapping,
) -> Result<(), Fail> {
    if matches!(conversion, Conversion::Nothing) {
        return Ok(());
    }
    let format = destination.format;
    let canvas = destination.canvas;
    let mask = format.pixel_mask();
    let direct = format.direct().then(|| Direct::of(&format));
    let opacity = OPAQUE - flags.translucency();
    let blends = opacity != OPAQUE
        || (flags.masked()
            && matches!(
                source.transparency,
                Transparency::Wide | Transparency::Channel(_)
            ));
    let palette = if blends && direct.is_none() {
        destination_palette(os, destination)?
    } else {
        Vec::new()
    };
    let Some(mut nearest) = NearestKept::of(&palette) else {
        return Err(os.error(NO_MEMORY));
    };
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
            let alpha = effective_alpha(alpha, opacity);
            if alpha == 0 {
                continue;
            }
            let Some(pixel) = conversion.pixel(source.colour(sx, sy)) else {
                continue;
            };
            *slot = match (blends, direct) {
                (true, Some(direct)) => blend(direct, pixel, *slot, alpha),
                (true, None) => blend_in_palette(&mut nearest, pixel, *slot, alpha),
                _ => combine(flags.action(), *slot, pixel) & mask,
            };
        }
        canvas.put_pixels(os, row, left.cast_unsigned(), &line)?;
    }
    changed(os, bounds)
}

/// Full opacity: an alpha of 255 widened to 256 so a solid pixel blends
/// to itself.
const OPAQUE: u32 = 0x100;

/// `over` drawn over `under` with alpha `alpha` of 256, each channel's two
/// shares rounded down apart, as RISC OS 5 blends.
fn blend(direct: Direct, over: u32, under: u32, alpha: u32) -> u32 {
    let (s, d) = (
        direct.unpack_replicated(over),
        direct.unpack_replicated(under),
    );
    direct.pack(mixed(s, d, alpha))
}

/// As [`blend`], the two palettised pixels' colours from `nearest`'s
/// palette, the result the palette's nearest.
fn blend_in_palette(nearest: &mut NearestKept<'_>, over: u32, under: u32, alpha: u32) -> u32 {
    let colour = |pixel: u32| {
        nearest
            .palette()
            .get(pixel as usize)
            .map_or([0; 3], |rgb| rgb.map(u32::from))
    };
    let rgb = mixed(colour(over), colour(under), alpha);
    nearest.index(rgb)
}

/// Each channel of `s` over `d` with alpha `alpha` of 256.
fn mixed(s: [u32; 3], d: [u32; 3], alpha: u32) -> [u8; 3] {
    let mix =
        |s: u32, d: u32| (((s * alpha) >> 8) + ((d * (OPAQUE - alpha)) >> 8)).to_le_bytes()[0];
    [mix(s[0], d[0]), mix(s[1], d[1]), mix(s[2], d[2])]
}

/// A mask's alpha, 255 solid, widened to 256 and kept from by `opacity`,
/// as RISC OS 5 takes it.
const fn effective_alpha(alpha: u32, opacity: u32) -> u32 {
    ((alpha + (alpha >> 7)) * opacity) >> 8
}

#[cfg(test)]
mod tests;
