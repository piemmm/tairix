//! `Font_Paint` and `Font_Caret`, as RISC OS 5.30's Font Manager paints: a
//! string walked as the measuring calls walk it, each character's glyph
//! drawn with its origin at the pixel its pen is in, the colours changing as
//! the string changes them, an underline under the characters it is set
//! for, and a rub-out box filled in the background behind them all.
//!
//! A glyph is drawn at its font's resolution, a pixel to a pixel of the
//! screen: in sixteen levels of coverage through the colour table up to
//! `FontMax2` and `FontMax3` points, in whole pixels above `FontMax2` or
//! where the colours give no anti-aliasing, and above `FontMax3`, kept no
//! longer, filled as Draw fills its outline.

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_raster::scan::FillRule;
use tairix_riscos_draw::pixels::{fill_interior, Point};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::font::{code, flag};
use tairix_riscos_vdu::render::{self, Levels, Surface, WHOLE};

use crate::bitmaps::{self, Held, Kind, Raster, Scalable};
use crate::buffer::{Corners, Recorder};
use crate::colours::{self, Colours, Table};
use crate::glyphs::{self, rounded, Bitmap, Placing, Reach, Refused, Scratch, Traced};
use crate::grid::{self, clamped, exactly, Grid, DRAW_UNITS, QUARTERS};
use crate::layout::{self, Limit, Placed, Request, Visit};
use crate::module::{error, raise, with};
use crate::outlines::{Depth, PLACEMENTS};
use crate::resolve::Font;
use crate::settings;
use crate::store::Drawing;
use crate::transform::Transform;
use crate::units;
use crate::x90y45;

/// `Font_Paint`'s own flags, beside those it shares with `Font_ScanString`.
mod paint_flag {
    /// The text is justified to the graphics cursor.
    pub(super) const JUSTIFY: u32 = 1 << 0;
    /// A rub-out box is filled behind it.
    pub(super) const RUB_OUT: u32 = 1 << 1;
    /// The glyphs blend with what is on the screen.
    pub(super) const BLEND: u32 = 1 << 11;
    /// The blend takes the screen's alpha or supremacy into account.
    pub(super) const SUPREMACY: u32 = 1 << 14;
    /// The bits RISC OS 5.30 refuses.
    pub(super) const RESERVED: u32 = !0x7FFF;
}

/// Where in a paint's block its rub-out box's corners are.
const RUB_OUT_WORDS: u32 = 16;

/// `Font_Paint`.
pub(crate) fn paint(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = regs.r[2];
    let block = flags & flag::BLOCK != 0;
    let reserved = flags & paint_flag::RESERVED != 0
        || (block && flags & (flag::OS_UNITS | paint_flag::JUSTIFY) != 0);
    if reserved {
        return Err(raise(os, error::RESERVED, &[]));
    }
    let surface = render::surface(os);
    let supremacy = flags & paint_flag::BLEND != 0 && flags & paint_flag::SUPREMACY != 0;
    if supremacy && !surface.is_some_and(|s| s.tagged) {
        return Err(raise(os, error::NO_SUPREMACY, &[]));
    }
    let (font, mut request) = layout::requested(os, regs)?;
    let (start_colours, font_max, output) = with(os, |state| {
        (state.current, state.settings.font_max, state.output)
    })?;
    let colours = Colours {
        font: font.handle,
        ..start_colours
    };
    let geometry = settings::grid(os)?.map(|grid| Geometry::of(grid, regs, flags));
    if flags & paint_flag::JUSTIFY != 0 {
        if let (Some(surface), Some(geometry)) = (&surface, &geometry) {
            request.space = justified(os, &request, &font, colours, geometry, surface)?;
        }
    }
    if let Some(output) = output {
        let rub_out = if flags & paint_flag::RUB_OUT == 0 {
            None
        } else {
            rub_out_corners(os, regs, block, geometry.as_ref(), surface.as_ref())?
        };
        let mut recorder = Recorder::new(os, geometry, output, &colours, rub_out)?;
        let walked = layout::walk(os, &request, font, colours, &mut recorder);
        let end = recorder.finish(os)?;
        with(os, |state| {
            if let Some(output) = &mut state.output {
                output.at = end;
            }
        })?;
        let walked = walked?;
        return with(os, |state| state.current = walked.stop.colours);
    }
    let table = colours::paint_table(os, &colours)?;
    let mut painter = Painter {
        surface,
        geometry,
        table,
        aliased: colours.offset != 0,
        blend: (flags & paint_flag::BLEND != 0).then(|| blended_towards(&colours)),
        supremacy,
        font_max: Sizes::of(font_max),
        underline: None,
        scratch: Scratch::default(),
    };
    if flags & paint_flag::RUB_OUT != 0 {
        painter.rub_out(os, regs, block)?;
    }
    let walked = layout::walk(os, &request, font, colours, &mut painter)?;
    with(os, |state| state.current = walked.stop.colours)
}

/// The space each space character adds to justify the string to the
/// graphics cursor: what the distance across from the start to the cursor,
/// either way, exceeds the string's own width by, shared among its spaces.
/// Justification is across alone.
fn justified(
    os: &mut Os<'_>,
    request: &Request,
    font: &Font,
    colours: Colours,
    geometry: &Geometry,
    surface: &Surface,
) -> Result<(i32, i32), Fail> {
    let measuring = Request {
        split: Some(u32::from(code::SPACE)),
        limit: Limit::None,
        ..*request
    };
    let natural = layout::walk(
        os,
        &measuring,
        font.clone(),
        colours,
        &mut layout::Measuring,
    )?
    .stop;
    let spaces = i64::from(natural.count);
    if spaces == 0 {
        return Ok(request.space);
    }
    let cursor = geometry.millipoints(surface.cursors[0]);
    let share = |cursor: i64, start: i64, width: i32| {
        let extra = (cursor - start).abs() - i64::from(width);
        i32::try_from(extra.div_euclid(spaces)).unwrap_or(0)
    };
    Ok((
        request
            .space
            .0
            .wrapping_add(share(cursor.0, geometry.start.0, natural.pen.0)),
        request.space.1,
    ))
}

/// A rub-out box's corners in millipoints from the screen's origin, as a
/// paint into a buffer takes them: the block's own, or the edges of the two
/// pixels the graphics cursor visited, both in; none where output shows no
/// graphics to take a cursor from.
fn rub_out_corners(
    os: &mut Os<'_>,
    regs: &Regs,
    block: bool,
    geometry: Option<&Geometry>,
    surface: Option<&Surface>,
) -> Result<Option<Corners>, Fail> {
    if block {
        let words = rub_out_block(os, regs)?.map(i64::from);
        return Ok(Some(((words[0], words[1]), (words[2], words[3]))));
    }
    let (Some(geometry), Some(surface)) = (geometry, surface) else {
        return Ok(None);
    };
    let (low, high) = cursor_corners(regs, surface);
    let corner = |(x, y): (i32, i32)| geometry.millipoints((x, y));
    let (left, bottom) = (low.0.min(high.0), low.1.min(high.1));
    let (right, top) = (
        low.0.max(high.0).saturating_add(1),
        low.1.max(high.1).saturating_add(1),
    );
    Ok(Some((corner((left, bottom)), corner((right, top)))))
}

/// The rub-out box's corners a paint's block holds, in millipoints.
fn rub_out_block(os: &mut Os<'_>, regs: &Regs) -> Result<[i32; 4], Fail> {
    let mut words = [0i32; 4];
    for (i, word) in (0u32..).zip(words.iter_mut()) {
        *word = os
            .read_word(regs.r[5].wrapping_add(RUB_OUT_WORDS + 4 * i))?
            .cast_signed();
    }
    Ok(words)
}

/// The two pixels the graphics cursor visited that bound a rub-out box: the
/// two before the one justification takes, where it takes one.
fn cursor_corners(regs: &Regs, surface: &Surface) -> ((i32, i32), (i32, i32)) {
    if regs.r[2] & paint_flag::JUSTIFY != 0 {
        (surface.cursors[2], surface.cursors[1])
    } else {
        (surface.cursors[1], surface.cursors[0])
    }
}

/// Where a paint's millipoints fall: the screen's grid, and the pen's start
/// in millipoints from the screen's origin. A paint's coordinates are the
/// screen's own: the graphics origin does not move them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Geometry {
    pub(crate) grid: Grid,
    pub(crate) start: (i64, i64),
}

impl Geometry {
    /// The paint `regs` asks for on `grid`: its start in OS units floored to
    /// the pixel they are in where `flags` say so, else in millipoints.
    pub(crate) fn of(grid: Grid, regs: &Regs, flags: u32) -> Self {
        let (x, y) = (regs.r[3].cast_signed(), regs.r[4].cast_signed());
        let start = if flags & flag::OS_UNITS != 0 {
            grid.os_units((x, y))
        } else {
            (i64::from(x), i64::from(y))
        };
        Self { grid, start }
    }

    /// The point `at` millipoints from the pen's start, from the screen's
    /// origin.
    pub(crate) const fn placed(&self, at: (i64, i64)) -> (i64, i64) {
        (
            self.start.0.saturating_add(at.0),
            self.start.1.saturating_add(at.1),
        )
    }

    /// The point `at` millipoints from the pen's start in pixels,
    /// unrounded.
    fn exact(&self, at: (i64, i64)) -> (f64, f64) {
        let (x, y) = self.placed(at);
        (self.grid.across.exact(x), self.grid.up.exact(y))
    }

    /// The quarter of a pixel each way the pen `at` millipoints from its
    /// start is in: where a glyph is put.
    pub(crate) fn quarters(&self, at: (i64, i64)) -> (i64, i64) {
        self.grid.quarters(self.placed(at))
    }

    /// The pixel nearest the point `at` millipoints from the screen's
    /// origin, halves up: a rub-out box's corner.
    fn nearest(&self, (x, y): (i32, i32)) -> (i32, i32) {
        (
            clamped(self.grid.across.nearest(i64::from(x))),
            clamped(self.grid.up.nearest(i64::from(y))),
        )
    }

    /// The pixel `(x, y)`'s bottom left, in millipoints from the screen's
    /// origin.
    fn millipoints(&self, (x, y): (i32, i32)) -> (i64, i64) {
        (
            self.grid.across.millipoints(i64::from(x)),
            self.grid.up.millipoints(i64::from(y)),
        )
    }
}

/// The `FontMax` values a glyph's size is held against, in points: up to
/// `bitmaps` scaled from a bitmap font where it has one, up to
/// `anti_aliased` anti-aliased, up to `kept` drawn as a bitmap and kept,
/// and up to `across` and `up` placed at a quarter of a pixel.
#[derive(Clone, Copy, Debug)]
struct Sizes {
    bitmaps: f64,
    anti_aliased: f64,
    kept: f64,
    across: f64,
    up: f64,
}

impl Sizes {
    fn of(font_max: [u32; 6]) -> Self {
        Self {
            bitmaps: f64::from(font_max[1]),
            anti_aliased: f64::from(font_max[2]),
            kept: f64::from(font_max[3]),
            across: f64::from(font_max[4]),
            up: f64::from(font_max[5]),
        }
    }
}

/// What a glyph is drawn from.
enum Plan {
    /// Its outline, anti-aliased or in whole pixels.
    Outline { aliased: bool },
    /// A bitmap file of its size, at four bits a pixel or one.
    Sized(Rc<Held>, Raster),
    /// A size of the `x90y45` file.
    Old(Rc<Held>, x90y45::Size),
    /// Bitmaps of another size, scaled.
    Scaled(Scalable),
    /// Nothing: a font of bitmaps alone with none to give.
    Nothing,
}

/// A glyph to draw from its outline: its font, its matrix, the glyph, the
/// quarter of a pixel its pen is in, its size in points across and up, and
/// the dots an inch it is drawn at.
struct Drawn<'a> {
    font: &'a Font,
    transform: &'a Transform,
    glyph: u32,
    quarters: (i64, i64),
    size: (f64, f64),
    resolution: (i32, i32),
}

/// A paint under way.
struct Painter {
    /// Where output goes; none where it shows no graphics.
    surface: Option<Surface>,
    /// Where its millipoints fall; none where the scale gives no pixels.
    geometry: Option<Geometry>,
    table: Table,
    /// Whether the colours anti-alias.
    aliased: bool,
    /// The colour `&BBGGRRSS` the levels blend toward what the screen shows,
    /// where they blend, and whether with the screen's supremacy.
    blend: Option<u32>,
    supremacy: bool,
    font_max: Sizes,
    /// The underline while one is set: its top and its bottom, in
    /// millipoints from the pen's start.
    underline: Option<(i64, i64)>,
    scratch: Scratch,
}

impl Painter {
    /// The rub-out box filled in the background colour: the block's
    /// corners, bottom left in and top right out, or the two points the
    /// graphics cursor visited before the one justification takes, both in.
    fn rub_out(&self, os: &mut Os<'_>, regs: &Regs, block: bool) -> Result<(), Fail> {
        let (Some(surface), Some(geometry)) = (self.surface, self.geometry) else {
            return Ok(());
        };
        let (low, high) = if block {
            let words = rub_out_block(os, regs)?;
            let high = geometry.nearest((words[2], words[3]));
            (
                geometry.nearest((words[0], words[1])),
                (high.0.saturating_sub(1), high.1.saturating_sub(1)),
            )
        } else {
            cursor_corners(regs, &surface)
        };
        let (left, right) = (low.0.min(high.0), low.0.max(high.0));
        let window = surface.window;
        let bottom = low.1.min(high.1).max(window.bottom);
        let top = low.1.max(high.1).min(window.top);
        let mut spans = Vec::new();
        let rows = usize::try_from(i64::from(top) - i64::from(bottom) + 1).unwrap_or(0);
        if spans.try_reserve(rows).is_err() {
            return Err(os.no_memory());
        }
        spans.extend((bottom..=top).map(|row| (row, left, right)));
        render::plot_value(os, &spans, self.table[0], false)
    }

    /// The glyph `placed` names drawn: its origin at the pixel its pen is
    /// in, from a bitmap file or its outline as the cache policy picks.
    fn glyph(
        &mut self,
        os: &mut Os<'_>,
        surface: &Surface,
        font: &Font,
        transform: &Transform,
        placed: &Placed,
    ) -> Result<(), Fail> {
        let (Some(glyph), Some(geometry)) = (placed.glyph, self.geometry) else {
            return Ok(());
        };
        let quarters = geometry.quarters((i64::from(placed.origin.0), i64::from(placed.origin.1)));
        let (across, up) = transform.scales();
        let size = (
            f64::from(font.points.0) / f64::from(units::SIXTEENTHS) * across,
            f64::from(font.points.1) / f64::from(units::SIXTEENTHS) * up,
        );
        let resolution = geometry.grid.resolution(font.resolution);
        match self.plan(os, font, transform, size, resolution)? {
            Plan::Outline { aliased } => {
                let drawn = Drawn {
                    font,
                    transform,
                    glyph,
                    quarters,
                    size,
                    resolution,
                };
                self.outline(os, surface, &drawn, aliased)
            }
            Plan::Sized(held, raster) => {
                let Held::Sized(bitmaps) = &*held else {
                    return Ok(());
                };
                let (x, across) = placement(bitmaps.placements.0, quarters.0);
                let (y, up) = placement(bitmaps.placements.1, quarters.1);
                let drawing = file_drawing(font, glyph, resolution, (across, up), raster);
                self.paint_file(os, drawing, (x, y), || bitmaps.glyph(glyph, (across, up)))
            }
            Plan::Old(held, size) => {
                let Held::Old(sizes) = &*held else {
                    return Ok(());
                };
                let drawing = file_drawing(font, glyph, resolution, (0, 0), Raster::Old);
                self.paint_file(os, drawing, pixel_of(quarters), || {
                    sizes.glyph(&size, glyph)
                })
            }
            Plan::Scaled(scalable) => {
                let drawing = file_drawing(font, glyph, resolution, (0, 0), Raster::Scaled);
                self.paint_file(os, drawing, pixel_of(quarters), || scalable.glyph(glyph))
            }
            Plan::Nothing => Ok(()),
        }
    }

    /// What a glyph of `font` at `size` points, through `transform`, at
    /// `resolution` is drawn from: as the PRM orders it, where four bits a
    /// pixel can be painted, a bitmap file of the size, then a size of the
    /// `x90y45` file, then up to `FontMax2` and `FontMax3` the outline
    /// anti-aliased, or a bitmap scaled up to `FontMax1` and where there are
    /// no outlines, and past them a one-bit file of the size before the
    /// outline in whole pixels; where only one bit can be, the one-bit file,
    /// then the outline, then the bitmaps. A font found with a matrix, or
    /// painted through one that is not the identity's, is drawn from its
    /// outline alone.
    fn plan(
        &self,
        os: &mut Os<'_>,
        font: &Font,
        transform: &Transform,
        (width, height): (f64, f64),
        resolution: (i32, i32),
    ) -> Result<Plan, Fail> {
        let outlines = font.data.has_outlines();
        let four = self.aliased && !font.data.monochrome();
        let max = &self.font_max;
        let aliasable = height <= max.anti_aliased && height <= max.kept;
        let outline = |aliased: bool| {
            if outlines {
                Plan::Outline { aliased }
            } else {
                Plan::Nothing
            }
        };
        if font.transformed || !transform.unscaled() {
            return Ok(outline(four && aliasable));
        }
        let sized = |depth: Depth| Kind::Sized {
            depth,
            points: font.points,
            resolution,
        };
        let present = |os: &mut Os<'_>, depth: Depth| -> Result<Option<Plan>, Fail> {
            let held = bitmaps::file(os, font, sized(depth))?;
            let raster = match depth {
                Depth::Four => Raster::Four,
                Depth::One => Raster::One,
            };
            Ok(matches!(&*held, Held::Sized(_)).then(|| Plan::Sized(held, raster)))
        };
        let old = |os: &mut Os<'_>| -> Result<Option<Plan>, Fail> {
            let held = bitmaps::file(os, font, Kind::Old)?;
            let size = match &*held {
                Held::Old(file) => bitmaps::old_size(file, font, resolution),
                _ => None,
            };
            Ok(size.map(|size| Plan::Old(held, size)))
        };
        let scaled = |os: &mut Os<'_>| -> Result<Option<Plan>, Fail> {
            Ok(bitmaps::scalable(os, font, resolution)?.map(Plan::Scaled))
        };
        if four {
            if let Some(plan) = present(os, Depth::Four)? {
                return Ok(plan);
            }
            if let Some(plan) = old(os)? {
                return Ok(plan);
            }
            if aliasable {
                let subpixel = outlines && (width <= max.across || height <= max.up);
                if !subpixel && (height <= max.bitmaps || !outlines) {
                    if let Some(plan) = scaled(os)? {
                        return Ok(plan);
                    }
                }
                return Ok(outline(true));
            }
            if let Some(plan) = present(os, Depth::One)? {
                return Ok(plan);
            }
            return Ok(outline(false));
        }
        if let Some(plan) = present(os, Depth::One)? {
            return Ok(plan);
        }
        if outlines {
            return Ok(outline(false));
        }
        if let Some(plan) = present(os, Depth::Four)? {
            return Ok(plan);
        }
        if let Some(plan) = old(os)? {
            return Ok(plan);
        }
        Ok(scaled(os)?.unwrap_or(Plan::Nothing))
    }

    /// The levels `drawing` holds, decoded by `decode` where the glyph cache
    /// does not hold them, painted with their origin at the pixel `origin`.
    fn paint_file(
        &self,
        os: &mut Os<'_>,
        drawing: Drawing,
        origin: (i32, i32),
        decode: impl FnOnce() -> Result<Option<Bitmap>, Refused>,
    ) -> Result<(), Fail> {
        match with(os, |state| state.store.glyph(drawing, decode))? {
            Ok(Some(bitmap)) => self.levels(os, origin, &bitmap),
            Ok(None) => Ok(()),
            Err(Refused) => Err(os.no_memory()),
        }
    }

    /// `bitmap`'s levels painted through the colour table, its origin at the
    /// pixel `origin`.
    fn levels(&self, os: &mut Os<'_>, origin: (i32, i32), bitmap: &Bitmap) -> Result<(), Fail> {
        let levels = Levels {
            left: origin.0.saturating_add(bitmap.left),
            top: origin.1.saturating_add(bitmap.top),
            width: bitmap.width,
            levels: &bitmap.levels,
        };
        match self.blend {
            Some(foreground) => {
                render::blend_levels(os, &levels, &self.table, foreground, self.supremacy)
            }
            None => render::paint_levels(os, &levels, &self.table),
        }
    }

    /// The glyph `drawn` names drawn from its outline: anti-aliased where
    /// `aliased` and at a quarter of a pixel past its pen where its size
    /// places it so, else in whole pixels, and past `FontMax3` filled as
    /// Draw fills it.
    fn outline(
        &mut self,
        os: &mut Os<'_>,
        surface: &Surface,
        drawn: &Drawn<'_>,
        aliased: bool,
    ) -> Result<(), Fail> {
        let Some(outline) = drawn.font.data.outline(drawn.glyph) else {
            return Ok(());
        };
        let (width, height) = drawn.size;
        let origin = pixel_of(drawn.quarters);
        let quarter = |placed: bool, quarter: i64| placement(aliased && placed, quarter).1;
        let within = (
            quarter(width <= self.font_max.across, drawn.quarters.0),
            quarter(height <= self.font_max.up, drawn.quarters.1),
        );
        let phase = (
            i32::try_from(within.0).unwrap_or(0),
            i32::try_from(within.1).unwrap_or(0),
        );
        let placing = Placing {
            points: drawn.font.points,
            transform: *drawn.transform,
            resolution: drawn.resolution,
            shift: (grid::shift(within.0), grid::shift(within.1)),
        };
        let traced = glyphs::trace(&outline, &placing);
        if height > self.font_max.kept {
            return self.filled(os, surface, &traced, origin);
        }
        let rule = if drawn.font.data.non_zero() {
            FillRule::NonZero
        } else {
            FillRule::EvenOdd
        };
        let whole = !aliased;
        let Some(reach) = traced.reach() else {
            return Ok(());
        };
        let window = window_of(surface, origin);
        let bitmap: Result<Option<Rc<Bitmap>>, Refused> = if reach.area() <= window_area(surface) {
            let drawing = Drawing {
                master: drawn.font.serial,
                glyph: drawn.glyph,
                points: drawn.font.points,
                resolution: placing.resolution,
                matrix: drawn.transform.words(),
                phase,
                whole,
                raster: Raster::Outline,
            };
            let scratch = &mut self.scratch;
            with(os, |state| {
                state
                    .store
                    .glyph(drawing, || traced.levels(&reach, rule, whole, scratch))
            })?
        } else {
            match reach.within(&window) {
                Some(seen) => traced
                    .levels(&seen, rule, whole, &mut self.scratch)
                    .map(|drawn| drawn.map(Rc::new)),
                None => Ok(None),
            }
        };
        match bitmap {
            Ok(Some(bitmap)) => self.levels(os, origin, &bitmap),
            Ok(None) => Ok(()),
            Err(Refused) => Err(os.no_memory()),
        }
    }

    /// A glyph too big to anti-alias, filled as Draw fills its outline in
    /// the last colour.
    fn filled(
        &self,
        os: &mut Os<'_>,
        surface: &Surface,
        traced: &Traced,
        origin: (i32, i32),
    ) -> Result<(), Fail> {
        let units = |pixels: f64, origin_pixel: i32, graphics_origin: i32, eig: u32| {
            let screen = (f64::from(origin_pixel) + pixels) * f64::from(1u32 << eig.min(30));
            rounded((screen - f64::from(graphics_origin)) * exactly(DRAW_UNITS))
        };
        let path = traced.draw_path(&|(x, y)| {
            Point::new(
                units(x, origin.0, surface.origin.0, surface.eig.0),
                units(y, origin.1, surface.origin.1, surface.eig.1),
            )
        });
        let colour = self.table[usize::from(WHOLE)];
        fill_interior(os, &path, surface, &mut |os, spans| {
            render::plot_value(os, spans, colour, false)
        })
    }

    /// The underline under the pen's step from `from` to `to`.
    fn underlined(
        &mut self,
        os: &mut Os<'_>,
        surface: &Surface,
        from: i32,
        to: i32,
    ) -> Result<(), Fail> {
        let Some((top, bottom)) = self.underline else {
            return Ok(());
        };
        if top <= bottom || from == to {
            return Ok(());
        }
        let Some(geometry) = self.geometry else {
            return Ok(());
        };
        let corner = |x: i32, y: i64| geometry.exact((i64::from(x), y));
        let ((x0, y0), (x1, y1)) = (corner(from, bottom), corner(to, top));
        let bar = Traced::rectangle((x0.min(x1), y0), (x0.max(x1), y1));
        let Some(reach) = bar.reach() else {
            return Ok(());
        };
        let Some(seen) = reach.within(&window_of(surface, (0, 0))) else {
            return Ok(());
        };
        match bar.levels(&seen, FillRule::NonZero, false, &mut self.scratch) {
            Ok(Some(bitmap)) => self.levels(os, (0, 0), &bitmap),
            Ok(None) => Ok(()),
            Err(Refused) => Err(os.no_memory()),
        }
    }
}

impl Visit for Painter {
    fn character(
        &mut self,
        os: &mut Os<'_>,
        font: &Font,
        transform: &Transform,
        placed: &Placed,
        _: &Colours,
    ) -> Result<(), Fail> {
        let Some(surface) = self.surface else {
            return Ok(());
        };
        self.glyph(os, &surface, font, transform, placed)?;
        self.underlined(os, &surface, placed.from.0, placed.to.0)
    }

    fn moved(&mut self, os: &mut Os<'_>, from: (i32, i32), to: (i32, i32)) -> Result<(), Fail> {
        let Some(surface) = self.surface else {
            return Ok(());
        };
        self.underlined(os, &surface, from.0, to.0)
    }

    fn underline(
        &mut self,
        _: &mut Os<'_>,
        font: &Font,
        at: (i32, i32),
        position: i32,
        thickness: u32,
    ) -> Result<(), Fail> {
        self.underline = Some(layout::underline_edges(font, at, position, thickness));
        Ok(())
    }

    fn colours(&mut self, os: &mut Os<'_>, colours: &mut Colours) -> Result<(), Fail> {
        colours::string_changed(os, colours)?;
        self.table = colours::paint_table(os, colours)?;
        self.aliased = colours.offset != 0;
        if self.blend.is_some() {
            self.blend = Some(blended_towards(colours));
        }
        Ok(())
    }
}

/// The colour a blending paint moves the screen toward: the foreground of
/// the RGB colours set, black where none are.
fn blended_towards(colours: &Colours) -> u32 {
    colours.rgb.map_or(0, |(_, foreground, _)| foreground)
}

/// A bitmap file's glyph as the glyph cache keys it: at the sub-placement
/// `phase`, made from `raster`.
fn file_drawing(
    font: &Font,
    glyph: u32,
    resolution: (i32, i32),
    phase: (usize, usize),
    raster: Raster,
) -> Drawing {
    let index = |placement: usize| i32::try_from(placement).unwrap_or(0);
    Drawing {
        master: font.serial,
        glyph,
        points: font.points,
        resolution,
        matrix: Transform::IDENTITY.words(),
        phase: (index(phase.0), index(phase.1)),
        whole: raster == Raster::One,
        raster,
    }
}

/// The pixel each way the quarters of a pixel `quarters` are in.
fn pixel_of((x, y): (i64, i64)) -> (i32, i32) {
    (placement(false, x).0, placement(false, y).0)
}

/// The pixel along one axis a glyph put at quarter `quarter` of a pixel is
/// drawn from, and where a file holds them `placed`, the quarter of that
/// pixel it is put at.
fn placement(placed: bool, quarter: i64) -> (i32, usize) {
    let pixel = clamped(quarter.div_euclid(QUARTERS));
    let within = usize::try_from(quarter.rem_euclid(QUARTERS))
        .map_or(0, |within| within.min(PLACEMENTS - 1));
    (pixel, if placed { within } else { 0 })
}

/// The graphics window, in pixels from the pixel `origin`, its right and
/// top out.
fn window_of(surface: &Surface, origin: (i32, i32)) -> Reach {
    let window = surface.window;
    Reach {
        left: window.left.saturating_sub(origin.0),
        bottom: window.bottom.saturating_sub(origin.1),
        right: window.right.saturating_add(1).saturating_sub(origin.0),
        top: window.top.saturating_add(1).saturating_sub(origin.1),
    }
}

/// The graphics window's pixels: the most a kept glyph may cover.
fn window_area(surface: &Surface) -> u64 {
    window_of(surface, (0, 0)).area()
}

/// `Font_Caret`: R0 a colour exclusive-ored onto the screen, R1 a height in
/// OS units, R3 and R4 where, in OS units where R2's bit 4 says, else
/// millipoints: a bar a pixel wide up from the point, its top and bottom
/// rows a pixel either side of it instead.
pub(crate) fn caret(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(surface) = render::surface(os) else {
        return Ok(());
    };
    let Some(grid) = settings::grid(os)? else {
        return Ok(());
    };
    let (x, y) = pixel_of(Geometry::of(grid, regs, regs.r[2]).quarters((0, 0)));
    let rows = regs.r[1].cast_signed() >> surface.eig.1.min(31);
    if rows <= 0 {
        return Ok(());
    }
    let top = y.saturating_add(rows - 1);
    let window = surface.window;
    let columns = i64::from(window.left)..=i64::from(window.right);
    let (bar, ends) = (i64::from(x), [i64::from(x) - 1, i64::from(x) + 1]);
    let mut spans = Vec::new();
    for row in y.max(window.bottom)..=top.min(window.top) {
        let across = if row == y || row == top {
            &ends[..]
        } else {
            core::slice::from_ref(&bar)
        };
        for column in across.iter().filter(|column| columns.contains(column)) {
            let column = clamped(*column);
            spans.push((row, column, column));
        }
    }
    render::plot_value(os, &spans, regs.r[0], true)
}

#[cfg(test)]
mod file_tests;
#[cfg(test)]
mod tests;
