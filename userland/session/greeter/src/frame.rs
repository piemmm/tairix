//! Walking the painted surface, over the scene beneath it, into the screen's
//! scan-out frame.
//!
//! The shared half of this — how long a frame is, which byte order a format
//! wants, and whether a rectangle is a sub-region or the whole screen — lives
//! in `lib/display`, and blending one layer over another is `lib/raster`'s
//! one span composite. What is here is only the loop that walks one surface
//! and the scene under it into one frame at the mode's stride, which is
//! genuinely different work from a compositor blending many windows.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::driver::display::{DamageList, DisplayMode};
use tairix_cursor::PlacedCursor;
use tairix_display::{scanout_len, sub_screen_damage, ChannelOrder};
use tairix_geometry::Rect;
use tairix_parallel::JobRunner;
use tairix_raster::{blend_span, DitherRow, Pixel, Surface};
use tairix_util::fallible;

/// Bytes the channel encoder writes per pixel.
const PIXEL_BYTES: usize = 4;

/// What a composition asks the display to present.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Present {
    /// Nothing changed, so nothing is presented.
    Nothing,
    /// Present these disjoint sub-regions of the frame.
    Region(DamageList),
    /// Present the whole frame.
    Whole,
}

impl Present {
    /// One present covering both this and `other`, so a drain that composed
    /// several changes hands the display one call rather than one per
    /// record.
    ///
    /// Two regions keep their rectangles apart, as many as a present names
    /// ([`DamageList::fitted`] merging the pair that over-covers least beyond
    /// that), so far-apart changes — a ribbon's strips across the screen —
    /// present as what moved rather than as the box between them. A list
    /// that comes to one rectangle covering the screen is the whole frame,
    /// which `mode` decides — the one definition of that question.
    #[must_use]
    pub fn merged(self, other: Self, mode: &DisplayMode) -> Self {
        match (self, other) {
            (Self::Nothing, present) | (present, Self::Nothing) => present,
            (Self::Whole, _) | (_, Self::Whole) => Self::Whole,
            (Self::Region(mine), Self::Region(theirs)) => {
                let both = mine.rects().iter().chain(theirs.rects()).copied();
                match DamageList::fitted(both) {
                    Some(list) if !covers_screen(&list, mode) => Self::Region(list),
                    Some(_) => Self::Whole,
                    None => Self::Nothing,
                }
            }
        }
    }

    /// The present of `clip`, a non-empty rectangle inside the screen.
    fn of(clip: &Rect, mode: &DisplayMode) -> Self {
        sub_screen_damage(clip, mode)
            .and_then(|region| DamageList::new(&[region]).ok())
            .map_or(Self::Whole, Self::Region)
    }
}

/// Whether `list`, disjoint rectangles inside the screen of `mode`, covers
/// all of it.
fn covers_screen(list: &DamageList, mode: &DisplayMode) -> bool {
    let area = |width: u32, height: u32| u64::from(width) * u64::from(height);
    let covered: u64 = list
        .rects()
        .iter()
        .map(|rect| area(rect.width_px, rect.height_px))
        .sum();
    covered >= area(mode.width_px, mode.height_px)
}

/// `damage` as a geometry rectangle, or `None` when its origin lies beyond
/// the signed coordinate space a rectangle is expressed in.
#[cfg(test)]
pub(crate) fn rect_of(damage: tairix_abi::driver::display::DamageRect) -> Option<Rect> {
    Some(Rect::new(
        i32::try_from(damage.x).ok()?,
        i32::try_from(damage.y).ok()?,
        damage.width_px,
        damage.height_px,
    ))
}

/// Pixels a band of a compose must carry to be worth handing to another
/// participant: below it the hand-off costs more than the encode it spreads.
const COMPOSE_GRAIN: usize = 16_384;

/// How many pixels the test for a clear surface takes at a time.
const INK_RUN: usize = 16;

/// What a compose lays into the frame, from the top down.
#[derive(Copy, Clone)]
pub struct Layers<'a> {
    /// The pointer, sampled over the veil.
    pub cursor: Option<&'a PlacedCursor>,
    /// The painted surface.
    pub surface: &'a Surface,
    /// The scene under the surface, left out unless it is the surface's size.
    pub ground: Option<&'a Surface>,
    /// How far the veil has lifted: `u8::MAX` shows the screen undimmed.
    pub reveal: u8,
}

/// One screen-shaped scan-out frame and the order its pixels go out in.
///
/// The frame is kept across repaints, so a redraw confined to the field or
/// the clock copies only those pixels and the rest of the screen stands as
/// it was.
pub struct Scanout {
    mode: DisplayMode,
    order: ChannelOrder,
    frame: Vec<u8>,
    /// A scanline for each band a compose is split into, in which the surface
    /// is laid over its scene a row at a time.
    rows: Vec<Pixel>,
    /// How many scanlines `rows` holds.
    rooms: usize,
}

impl Scanout {
    /// A frame for `mode`, or `None` when the mode cannot be scanned out or
    /// the allocator refuses its frame.
    ///
    /// A zero extent, an impossible stride, or a pixel format with no
    /// software encoding here are all refused rather than guessed: a wrong
    /// guess renders the whole screen in false colour, and there is nothing
    /// to draw on at all without a frame.
    #[must_use]
    pub fn new(mode: DisplayMode) -> Option<Self> {
        let order = ChannelOrder::for_format(mode.format)?;
        if usize::try_from(mode.format.bytes_per_pixel()).ok() != Some(PIXEL_BYTES) {
            return None;
        }
        let len = scanout_len(&mode)?;
        let width = usize::try_from(mode.width_px).ok()?;
        Some(Self {
            mode,
            order,
            frame: fallible::filled(len, 0u8)?,
            rows: fallible::filled(width, Pixel::TRANSPARENT)?,
            rooms: 1,
        })
    }

    /// Hold a scanline for each of `runner`'s participants, so a compose can
    /// be spread across all of them. A refusal keeps the scanlines held
    /// before, which costs parallelism and never the frame.
    pub fn spread_over(&mut self, runner: &dyn JobRunner) {
        let rooms = runner.width().min(index(self.mode.height_px));
        let Some(len) = index(self.mode.width_px).checked_mul(rooms) else {
            return;
        };
        if rooms > self.rooms && fallible::grow_to(&mut self.rows, len, Pixel::TRANSPARENT) {
            self.rooms = rooms;
        }
    }

    /// The mode this frame is shaped for.
    #[must_use]
    pub const fn mode(&self) -> &DisplayMode {
        &self.mode
    }

    /// The rectangle the frame covers, in the surface's own coordinates.
    #[must_use]
    pub const fn screen(&self) -> Rect {
        Rect::new(0, 0, self.mode.width_px, self.mode.height_px)
    }

    /// The frame's bytes, for the present call.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }

    /// Copy every rectangle of `damage` from `layers` into the frame, in bands
    /// of rows spread across `runner`, and say what to present.
    ///
    /// Each rectangle is clipped to the screen first, so one that lies partly
    /// or wholly outside copies what overlaps and asks for nothing when
    /// nothing does, and one covering the screen copies it once whatever else
    /// `damage` names. A band copies its rows of every rectangle, so a frame's
    /// many narrow changes — a ribbon's strips — are spread as one region.
    ///
    /// The scene and the surface over it are separate layers so that either
    /// can change without the other being painted again: a frame of the scene
    /// re-composes the pixels it moved, and a keystroke the ones the surface
    /// repainted.
    ///
    /// The cursor is sampled here rather than drawn into the surface, so the
    /// pixels behind it are never overwritten and one painted surface serves
    /// every position the pointer takes over it. The veil
    /// ([`AuthSurface::reveal`](tairix_greeter::AuthSurface::reveal)) is
    /// applied here for the same reason, and beneath the cursor exactly as a
    /// wash painted into the surface would have been: the pointer stays crisp
    /// over a screen that is dissolving.
    pub fn compose<D>(&mut self, layers: Layers<'_>, damage: D, runner: &dyn JobRunner) -> Present
    where
        D: Iterator<Item = Rect> + Clone + Sync,
    {
        let screen = self.screen();
        let (mut present, mut bounds, mut pixels, mut whole) =
            (Present::Nothing, Rect::EMPTY, 0, false);
        let each = damage.clone();
        for clip in damage.map(|rect| rect.intersection(&screen)) {
            if clip.is_empty() {
                continue;
            }
            whole |= clip == screen;
            present = present.merged(Present::of(&clip, &self.mode), &self.mode);
            bounds = bounds.union(&clip);
            pixels = index(clip.width)
                .saturating_mul(index(clip.height))
                .saturating_add(pixels);
        }
        let Some((_, top)) = bounds.surface_origin().filter(|_| !bounds.is_empty()) else {
            return Present::Nothing;
        };
        let rows = top..top.saturating_add(bounds.height);
        let surface = layers.surface;
        let layers = Layers {
            ground: layers.ground.filter(|ground| {
                ground.width() == surface.width() && ground.height() == surface.height()
            }),
            ..layers
        };
        let Ok(stride) = usize::try_from(self.mode.stride_bytes) else {
            return present;
        };
        let (first, count) = (index(rows.start), rows.len());
        let Some(lines) = first
            .checked_mul(stride)
            .zip(first.saturating_add(count).checked_mul(stride))
            .and_then(|(from, to)| self.frame.get_mut(from..to))
        else {
            return present;
        };
        let pass = Pass {
            layers,
            order: self.order,
            screen,
            stride,
            whole,
        };
        let pieces = tairix_parallel::bands(runner, pixels, COMPOSE_GRAIN)
            .min(self.rooms)
            .min(count);
        let per = tairix_parallel::piece_len(count, pieces);
        let bands = lines
            .chunks_mut(per.saturating_mul(stride))
            .zip(self.rows.chunks_mut(index(screen.width)))
            .zip(rows.step_by(per));
        tairix_parallel::for_each_drawn(runner, bands, &|((lines, laid), top)| {
            pass.band(top, lines, laid, each.clone());
        });
        present
    }

    /// Fill the whole frame with black and present all of it.
    ///
    /// What a display that cannot switch itself off is left showing while the
    /// screen sleeps, and what one that can is left holding, so it wakes on
    /// black rather than on the screen it went dark over.
    pub fn blacken(&mut self) -> Present {
        let black = self.order.encode(Pixel {
            r: 0,
            g: 0,
            b: 0,
            a: u8::MAX,
        });
        let (Ok(stride), Ok(width)) = (
            usize::try_from(self.mode.stride_bytes),
            usize::try_from(self.mode.width_px),
        ) else {
            return Present::Nothing;
        };
        for line in self.frame.chunks_exact_mut(stride) {
            let (pixels, _) = line.as_chunks_mut::<PIXEL_BYTES>();
            for slot in pixels.iter_mut().take(width) {
                *slot = black;
            }
        }
        Present::Whole
    }
}

/// What every band of one compose shares.
struct Pass<'a> {
    layers: Layers<'a>,
    order: ChannelOrder,
    screen: Rect,
    stride: usize,
    /// Whether the damage covers the screen, so a band is copied whole once.
    whole: bool,
}

impl Pass<'_> {
    /// Copy every rectangle of `damage` within the rows `lines` holds, the
    /// first of them row `top`, laying the surface over its scene in `laid`.
    fn band(
        &self,
        top: u32,
        lines: &mut [u8],
        laid: &mut [Pixel],
        damage: impl Iterator<Item = Rect>,
    ) {
        let (Ok(y), Ok(height)) = (i32::try_from(top), u32::try_from(lines.len() / self.stride))
        else {
            return;
        };
        let band = Rect::new(0, y, self.screen.width, height);
        if self.whole {
            self.rect(band, top, lines, laid);
            return;
        }
        for rect in damage {
            self.rect(rect.intersection(&band), top, lines, laid);
        }
    }

    /// Copy `clip`, inside the band whose first row is `top`, into `lines`.
    fn rect(&self, clip: Rect, top: u32, lines: &mut [u8], laid: &mut [Pixel]) {
        let Some((left, y)) = clip.surface_origin().filter(|_| !clip.is_empty()) else {
            return;
        };
        let Some(skipped) = y.checked_sub(top) else {
            return;
        };
        let columns = index(left)..index(left).saturating_add(index(clip.width));
        let rows = lines
            .chunks_exact_mut(self.stride)
            .skip(index(skipped))
            .take(index(clip.height));
        for (line, y) in rows.zip(index(y)..) {
            self.span(y, columns.clone(), line, laid);
        }
    }

    /// Encode `columns` of row `y` into `line`.
    ///
    /// A pixel the surface does not have is skipped rather than faulted: a
    /// surface smaller than the screen leaves those bytes as they were.
    ///
    /// A row over a scene is the scene's own pixels with the surface's
    /// composited over them in `laid`, so the two layers are never merged
    /// anywhere that outlives the row. Which image row the cursor draws from
    /// is resolved once per scanline, and a scanline it does not reach walks
    /// the plain copy, so the blend costs only the rows the pointer covers.
    fn span(&self, y: usize, columns: Range<usize>, line: &mut [u8], laid: &mut [Pixel]) {
        let Layers {
            cursor,
            surface,
            ground,
            reveal,
        } = self.layers;
        let surface_width = index(surface.width());
        let columns = columns.start..columns.end.min(surface_width);
        if columns.is_empty() || y >= index(surface.height()) {
            return;
        }
        let Some(start) = y.checked_mul(surface_width) else {
            return;
        };
        let span = start + columns.start..start + columns.end;
        let Some(painted) = surface.pixels().get(span.clone()) else {
            return;
        };
        // The veil dims a picture, so its rounding error is spread across
        // the pixels it covers rather than contouring a gradient — the same
        // dither, tiled from the surface's own coordinates, that painting
        // the field into the surface used.
        let dither = u32::try_from(y).map_or(DitherRow::NEAREST, DitherRow::at);
        let first_col = u32::try_from(columns.start).unwrap_or(0);
        let source = match (
            ground.and_then(|ground| ground.pixels().get(span)),
            laid.get_mut(..columns.len()),
        ) {
            (Some(under), Some(laid)) => match inked(painted) {
                // A clear pixel laid over another is the identity, and most of
                // a ribbon's strips lie wholly outside the column.
                None => under,
                Some(ink) => {
                    laid.copy_from_slice(under);
                    let at = first_col.saturating_add(u32::try_from(ink.start).unwrap_or(0));
                    if let (Some(laid), Some(painted)) =
                        (laid.get_mut(ink.clone()), painted.get(ink))
                    {
                        blend_span(laid, painted, u8::MAX, dither, at);
                    }
                    &*laid
                }
            },
            _ => painted,
        };
        let Some(target) = line.get_mut(columns.start * PIXEL_BYTES..columns.end * PIXEL_BYTES)
        else {
            return;
        };
        let cursor_row = cursor
            .zip(i32::try_from(y).ok())
            .and_then(|(cursor, y)| cursor.local_row(y).map(|ly| (cursor, ly)));
        let order = self.order;
        if cursor_row.is_none() && reveal == u8::MAX {
            let _ = order.encode_run(source, target);
            return;
        }
        let (slots, _) = target.as_chunks_mut::<PIXEL_BYTES>();
        let Some((cursor, ly)) = cursor_row else {
            for ((slot, pixel), col) in slots.iter_mut().zip(source).zip(first_col..) {
                *slot = order.encode(pixel.dimmed_biased(reveal, dither.bias(col)));
            }
            return;
        };
        for ((slot, pixel), col) in slots.iter_mut().zip(source).zip(first_col..) {
            let under = pixel.dimmed_biased(reveal, dither.bias(col));
            let painted = i32::try_from(col)
                .ok()
                .and_then(|x| cursor.sample_row(x, ly))
                .map_or(under, |sprite| sprite.over(under));
            *slot = order.encode(painted);
        }
    }
}

/// The part of `span` from the first run of [`INK_RUN`] pixels holding one
/// that is not wholly clear to the last such run, or `None` when every pixel
/// is clear.
///
/// A run is tested whole, with no early exit inside it, so the test is a
/// reduction the compiler vectorises: a clear row costs a fraction of
/// blending it.
fn inked(span: &[Pixel]) -> Option<Range<usize>> {
    let holds_ink = |run: &[Pixel]| {
        run.iter().fold(0, |any, pixel| {
            any | u32::from_ne_bytes([pixel.r, pixel.g, pixel.b, pixel.a])
        }) != 0
    };
    let first = span.chunks(INK_RUN).position(holds_ink)?;
    let last = span.rchunks(INK_RUN).position(holds_ink).unwrap_or(0);
    Some(first * INK_RUN..span.len() - last * INK_RUN)
}

/// A screen extent as an index, which every supported target's address
/// space holds.
fn index(extent: u32) -> usize {
    usize::try_from(extent).unwrap_or(usize::MAX)
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
