//! Walking the painted surface, over the scene beneath it, into the screen's
//! scan-out frame.
//!
//! The shared half of this — how long a frame is, which byte order a format
//! wants, and whether a rectangle is a sub-region or the whole screen — lives
//! in `lib/display`, and blending one layer over another is `lib/raster`'s
//! one span composite. What is here is only the loop that walks one surface
//! and the scene under it into one frame at the mode's stride, which is
//! genuinely different work from a compositor blending many windows.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageRect, DisplayMode};
use tairix_cursor::PlacedCursor;
use tairix_display::{scanout_len, sub_screen_damage, ChannelOrder};
use tairix_geometry::Rect;
use tairix_raster::{blend_span, DitherRow, Pixel, Surface};

/// Bytes the channel encoder writes per pixel.
const PIXEL_BYTES: usize = 4;

/// What a composition asks the display to present.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Present {
    /// Nothing changed, so nothing is presented.
    Nothing,
    /// Present this sub-region of the frame.
    Region(DamageRect),
    /// Present the whole frame.
    Whole,
}

impl Present {
    /// One present covering both this and `other`, so a drain that composed
    /// several changes hands the display one call rather than one per
    /// record.
    ///
    /// Two regions become the rectangle containing both, which `mode`
    /// resolves back to a whole present once it covers the screen — the one
    /// definition of that question. A region whose origin will not convert
    /// becomes the whole frame: presenting more than changed is wasteful,
    /// presenting less shows stale pixels.
    #[must_use]
    pub fn merged(self, other: Self, mode: &DisplayMode) -> Self {
        match (self, other) {
            (Self::Nothing, present) | (present, Self::Nothing) => present,
            (Self::Whole, _) | (_, Self::Whole) => Self::Whole,
            (Self::Region(mine), Self::Region(theirs)) => {
                let (Some(mine), Some(theirs)) = (rect_of(mine), rect_of(theirs)) else {
                    return Self::Whole;
                };
                match sub_screen_damage(&mine.union(&theirs), mode) {
                    Some(region) => Self::Region(region),
                    None => Self::Whole,
                }
            }
        }
    }
}

/// `damage` as a geometry rectangle, or `None` when its origin lies beyond
/// the signed coordinate space a rectangle is expressed in.
pub(crate) fn rect_of(damage: DamageRect) -> Option<Rect> {
    Some(Rect::new(
        i32::try_from(damage.x).ok()?,
        i32::try_from(damage.y).ok()?,
        damage.width_px,
        damage.height_px,
    ))
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
    /// One scanline of the surface laid over its scene, reused row after row.
    row: Vec<Pixel>,
}

impl Scanout {
    /// A frame for `mode`, or `None` when the mode cannot be scanned out.
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
            frame: vec![0u8; len],
            row: vec![Pixel::TRANSPARENT; width],
        })
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

    /// Copy `surface`, laid over `ground` where there is one, into the frame
    /// within `damage`, dimmed to `reveal`, with `cursor` over the top, and
    /// say what to present.
    ///
    /// `damage` is `None` for "the whole screen changed". A rectangle is
    /// clipped to the screen first, so one that lies partly or wholly outside
    /// copies what overlaps and asks for nothing when nothing does.
    ///
    /// The scene and the surface over it are separate layers so that either
    /// can change without the other being painted again: a frame of the scene
    /// re-composes the pixels it moved, and a keystroke the ones the surface
    /// repainted. A `ground` that is not the surface's size is not a layer of
    /// this screen, and is left out.
    ///
    /// The cursor is sampled here rather than drawn into `surface`, so the
    /// pixels behind it are never overwritten and one painted surface serves
    /// every position the pointer takes over it. The veil
    /// ([`AuthSurface::reveal`](tairix_greeter::AuthSurface::reveal)) is
    /// applied here for the same reason, and beneath the cursor exactly as a
    /// wash painted into the surface would have been: the pointer stays crisp
    /// over a screen that is dissolving.
    pub fn compose(
        &mut self,
        surface: &Surface,
        ground: Option<&Surface>,
        cursor: Option<&PlacedCursor>,
        damage: Option<Rect>,
        reveal: u8,
    ) -> Present {
        let screen = self.screen();
        let clip = match damage {
            Some(damage) => damage.intersection(&screen),
            None => screen,
        };
        if clip.is_empty() {
            return Present::Nothing;
        }
        let ground = ground.filter(|ground| {
            ground.width() == surface.width() && ground.height() == surface.height()
        });
        self.blit(surface, ground, cursor, clip, reveal);
        match sub_screen_damage(&clip, &self.mode) {
            Some(region) => Present::Region(region),
            None => Present::Whole,
        }
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

    /// Encode `clip`'s pixels from `surface` laid over `ground`, dimmed to
    /// `reveal` and blended under `cursor`, into the frame.
    ///
    /// `clip` is already inside the screen, and the frame is stride-shaped
    /// for that screen, so a pixel the surface does not have is skipped
    /// rather than faulted: a surface smaller than the screen leaves those
    /// bytes as they were.
    ///
    /// A row over a scene is the scene's own pixels with the surface's
    /// composited over them in the kept scratch row, so the two layers are
    /// never merged anywhere that outlives the row. Which image row the
    /// cursor draws from is resolved once per scanline, and a scanline it does
    /// not reach walks the plain copy, so the blend costs only the rows the
    /// pointer actually covers.
    fn blit(
        &mut self,
        surface: &Surface,
        ground: Option<&Surface>,
        cursor: Option<&PlacedCursor>,
        clip: Rect,
        reveal: u8,
    ) {
        let Self {
            mode,
            order,
            frame,
            row: scratch,
        } = self;
        let Ok(stride) = usize::try_from(mode.stride_bytes) else {
            return;
        };
        let Ok(surface_width) = usize::try_from(surface.width()) else {
            return;
        };
        let Ok(surface_height) = usize::try_from(surface.height()) else {
            return;
        };
        let Ok(left) = usize::try_from(clip.left()) else {
            return;
        };
        let Ok(top) = usize::try_from(clip.top()) else {
            return;
        };
        let columns = usize::try_from(clip.width)
            .unwrap_or(0)
            .min(surface_width.saturating_sub(left));
        let rows = usize::try_from(clip.height)
            .unwrap_or(0)
            .min(surface_height.saturating_sub(top));
        let order = *order;
        let first_col = u32::try_from(left).unwrap_or(0);
        let pixels = surface.pixels();
        let scene = ground.map(Surface::pixels);
        for row in 0..rows {
            let Some(y) = top.checked_add(row) else {
                break;
            };
            let Some(start) = y
                .checked_mul(surface_width)
                .and_then(|line| line.checked_add(left))
            else {
                continue;
            };
            let Some(span) = start.checked_add(columns).map(|end| start..end) else {
                continue;
            };
            let Some(painted) = pixels.get(span.clone()) else {
                continue;
            };
            // The veil dims a picture, so its rounding error is spread across
            // the pixels it covers rather than contouring a gradient — the same
            // dither, tiled from the surface's own coordinates, that painting
            // the field into the surface used.
            let dither = u32::try_from(y).map_or(DitherRow::NEAREST, DitherRow::at);
            let source = match (
                scene.and_then(|scene| scene.get(span)),
                scratch.get_mut(..columns),
            ) {
                (Some(under), Some(laid)) => {
                    laid.copy_from_slice(under);
                    blend_span(laid, painted, u8::MAX, dither, first_col);
                    &*laid
                }
                _ => painted,
            };
            let Some(target) = y
                .checked_mul(stride)
                .and_then(|line| line.checked_add(left.checked_mul(PIXEL_BYTES)?))
                .and_then(|start| {
                    let span = columns.checked_mul(PIXEL_BYTES)?;
                    frame.get_mut(start..start.checked_add(span)?)
                })
            else {
                continue;
            };
            let (slots, _) = target.as_chunks_mut::<PIXEL_BYTES>();
            let cursor_row = cursor
                .zip(i32::try_from(y).ok())
                .and_then(|(cursor, y)| cursor.local_row(y).map(|ly| (cursor, ly)));
            let Some((cursor, ly)) = cursor_row else {
                for ((slot, pixel), col) in slots.iter_mut().zip(source).zip(first_col..) {
                    *slot = order.encode(pixel.dimmed_biased(reveal, dither.bias(col)));
                }
                continue;
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
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
