//! A picture drawn through an affine transform: the one transformed blit.
//!
//! Each destination pixel the transformed picture can touch maps its centre
//! back into the source and samples it bilinearly in premultiplied space.
//! Sampling premultiplied keeps a translucent edge's colour honest. Its edge
//! is drawn one of two ways ([`Edge`]): with everything outside the source
//! transparent, which anti-aliases a turned edge against whatever lies beneath
//! it and fades a magnified one over half a source pixel; or framed, the
//! picture covering exactly its own rectangle — sampled no further out than
//! its edge pixels, the rectangle's own edge anti-aliased by how much of each
//! pixel it covers — as a picture placed in a document is drawn.
//!
//! The source is any [`Picture`]: a [`Surface`], or a [`Mosaic`] of tiles
//! sampled across their seams as the one picture they make. A transform that
//! only scales and moves maps every column of a row to the same source column
//! and every row to the same source row, so its sampling positions are worked
//! out once per column and once per row rather than once per pixel; the
//! samples are the same ones the general path takes, to the bit.

use core::borrow::Borrow;
use core::ops::Range;

use alloc::vec::Vec;

use tairix_util::{fallible, mathf};

use crate::color::{blend_span, Pixel};
use crate::dither::DitherRow;
use crate::surface::Surface;
use tairix_bezier::Affine;

/// Columns a blit samples at once into a run on the stack, composited in one
/// span, whatever the width drawn.
const TAP_CHUNK: u32 = 256;

/// How a transformed picture's edge is drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Edge {
    /// Sampled against transparency past the source.
    Fading,
    /// Covering exactly the source's rectangle.
    Framed,
}

/// How a framed picture is sampled along one axis of the destination: the
/// source's extent on that axis, and how many source pixels one destination
/// pixel spans across it.
#[derive(Copy, Clone)]
struct Frame {
    extent: f64,
    per_pixel: f64,
}

impl Frame {
    /// Where a sample at source position `at` is taken, held within the
    /// centres of the edge pixels, and how much of its destination pixel the
    /// rectangle covers along this axis, out of 256.
    fn place(self, at: f64) -> (f64, u32) {
        let held = mathf::fmin(mathf::fmax(at, 0.5), mathf::fmax(self.extent - 0.5, 0.5));
        // Each edge's distance from the pixel's centre, in destination pixels.
        let low = at / self.per_pixel;
        let high = (self.extent - at) / self.per_pixel;
        let covered = mathf::fmin(low, 0.5) + mathf::fmin(high, 0.5);
        let share = mathf::round_i32(mathf::fmin(mathf::fmax(covered, 0.0), 1.0) * 256.0);
        (held, u32::try_from(share).unwrap_or(0))
    }
}

/// How a picture of `size` drawn through a transform of `inverse` is framed
/// along each destination axis, if its `edge` is framed.
fn frames((width, height): (u32, u32), inverse: Affine, edge: Edge) -> Option<(Frame, Frame)> {
    (edge == Edge::Framed).then(|| {
        (
            Frame {
                extent: f64::from(width),
                per_pixel: mathf::hypot(inverse.a, inverse.c),
            },
            Frame {
                extent: f64::from(height),
                per_pixel: mathf::hypot(inverse.b, inverse.d),
            },
        )
    })
}

/// A sample taken at `share` of its pixel's coverage out of 256 along each
/// axis: what a framed picture's edge pixels show of it.
fn covered(sample: Pixel, across: u32, down: u32) -> Pixel {
    if across == 256 && down == 256 {
        return sample;
    }
    let factor = (across * down * 255 + (1 << 15)) >> 16;
    sample.scale_alpha(u8::try_from(factor).unwrap_or(u8::MAX))
}

/// A picture the transformed blit samples: premultiplied pixels.
pub trait Picture {
    /// Its width and height in pixels.
    fn size(&self) -> (u32, u32);

    /// The pixel at `(x, y)`, transparent wherever the picture holds none.
    fn pixel(&self, x: u32, y: u32) -> Pixel;

    /// The pixels at `(x, y)`, `(x + 1, y)`, `(x, y + 1)` and
    /// `(x + 1, y + 1)`, each transparent where the picture holds none: what
    /// one bilinear sample weighs. A picture overrides it to read the
    /// neighbourhood in one look where it lies whole inside.
    fn quad(&self, x: i64, y: i64) -> [Pixel; 4] {
        quad_by_pixel(self, x, y)
    }

    /// `out.len()` pixels of row `y` from column `x` on, each transparent
    /// where the picture holds none: the run a scaled blit reads a source row
    /// through, so a picture overrides it to copy what it holds in one move.
    fn row(&self, x: i64, y: i64, out: &mut [Pixel]) {
        let (width, height) = self.size();
        for (at, slot) in (x..).zip(out.iter_mut()) {
            *slot = match (u32::try_from(at), u32::try_from(y)) {
                (Ok(column), Ok(row)) if column < width && row < height => self.pixel(column, row),
                _ => Pixel::TRANSPARENT,
            };
        }
    }
}

/// The four pixels about `(x, y)` that [`Picture::quad`] answers, read one
/// at a time.
fn quad_by_pixel<P: Picture + ?Sized>(src: &P, x: i64, y: i64) -> [Pixel; 4] {
    let (width, height) = src.size();
    let at = |x: i64, y: i64| match (u32::try_from(x), u32::try_from(y)) {
        (Ok(x), Ok(y)) if x < width && y < height => src.pixel(x, y),
        _ => Pixel::TRANSPARENT,
    };
    [at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1)]
}

impl Picture for Surface {
    fn size(&self) -> (u32, u32) {
        (self.width(), self.height())
    }

    fn pixel(&self, x: u32, y: u32) -> Pixel {
        let index = u64::from(y) * u64::from(self.width()) + u64::from(x);
        usize::try_from(index)
            .ok()
            .and_then(|index| self.pixels().get(index).copied())
            .unwrap_or(Pixel::TRANSPARENT)
    }

    fn quad(&self, x: i64, y: i64) -> [Pixel; 4] {
        let width = i64::from(self.width());
        let inside = x >= 0 && y >= 0 && x + 1 < width && y + 1 < i64::from(self.height());
        let rows = inside
            .then(|| {
                let top = usize::try_from(y * width + x).ok()?;
                let below = top.checked_add(usize::try_from(width).ok()?)?;
                let pixels = self.pixels();
                Some((pixels.get(top..top + 2)?, pixels.get(below..below + 2)?))
            })
            .flatten();
        match rows {
            Some((&[top_left, top_right], &[below_left, below_right])) => {
                [top_left, top_right, below_left, below_right]
            }
            _ => quad_by_pixel(self, x, y),
        }
    }

    fn row(&self, x: i64, y: i64, out: &mut [Pixel]) {
        let (width, height) = (i64::from(self.width()), i64::from(self.height()));
        let len = i64::try_from(out.len()).unwrap_or(i64::MAX);
        let (from, to) = (x.max(0), x.saturating_add(len).min(width));
        let held = (0..height).contains(&y) && from < to;
        let (Ok(skip), Ok(count), Ok(start)) = (
            usize::try_from(from - x),
            usize::try_from(to - from),
            usize::try_from(y.saturating_mul(width).saturating_add(from)),
        ) else {
            return out.fill(Pixel::TRANSPARENT);
        };
        let copied = held
            && match (
                out.get_mut(skip..skip.saturating_add(count)),
                self.pixels().get(start..start.saturating_add(count)),
            ) {
                (Some(into), Some(from)) => {
                    into.copy_from_slice(from);
                    true
                }
                _ => false,
            };
        if copied {
            let (before, rest) = out.split_at_mut(skip);
            before.fill(Pixel::TRANSPARENT);
            if let Some(after) = rest.get_mut(count..) {
                after.fill(Pixel::TRANSPARENT);
            }
        } else {
            out.fill(Pixel::TRANSPARENT);
        }
    }
}

/// Equal square tiles laid edge to edge as one picture, row by row, a tile
/// absent wherever its place shows nothing: a tiled view's cache drawn as the
/// picture it is, so a bilinear sample straddling two tiles blends them as it
/// would within one, leaving no seam.
pub struct Mosaic<'t, T> {
    /// A tile's side is `1 << shift`.
    shift: u32,
    columns: u32,
    rows: u32,
    tiles: &'t [Option<T>],
}

impl<'t, T: Borrow<Surface>> Mosaic<'t, T> {
    /// `tiles`, row by row and `columns` across, each a square `side`
    /// pixels wide; `None` where `side` is not a power of two, `tiles` is not
    /// a whole number of rows, a tile present is another size, or the
    /// picture is too large to address.
    #[must_use]
    pub fn new(side: u32, columns: u32, tiles: &'t [Option<T>]) -> Option<Self> {
        if !side.is_power_of_two() || columns == 0 {
            return None;
        }
        let count = u32::try_from(tiles.len()).ok()?;
        if !count.is_multiple_of(columns) {
            return None;
        }
        let rows = count / columns;
        let shift = side.trailing_zeros();
        columns.checked_mul(side)?;
        rows.checked_mul(side)?;
        let fits = |tile: &Option<T>| {
            tile.as_ref()
                .is_none_or(|tile| tile.borrow().size() == (side, side))
        };
        tiles.iter().all(fits).then_some(Self {
            shift,
            columns,
            rows,
            tiles,
        })
    }
}

impl<T: Borrow<Surface>> Picture for Mosaic<'_, T> {
    fn size(&self) -> (u32, u32) {
        (self.columns << self.shift, self.rows << self.shift)
    }

    fn pixel(&self, x: u32, y: u32) -> Pixel {
        let mask = (1 << self.shift) - 1;
        self.tile(x, y)
            .map_or(Pixel::TRANSPARENT, |tile| tile.pixel(x & mask, y & mask))
    }

    fn quad(&self, x: i64, y: i64) -> [Pixel; 4] {
        let mask = (1_i64 << self.shift) - 1;
        let within_one = x >= 0 && y >= 0 && x & mask < mask && y & mask < mask;
        if within_one {
            let place = (u32::try_from(x), u32::try_from(y));
            return match place {
                (Ok(x), Ok(y)) => self.tile(x, y).map_or([Pixel::TRANSPARENT; 4], |tile| {
                    tile.quad(i64::from(x) & mask, i64::from(y) & mask)
                }),
                _ => [Pixel::TRANSPARENT; 4],
            };
        }
        quad_by_pixel(self, x, y)
    }

    fn row(&self, x: i64, y: i64, out: &mut [Pixel]) {
        let (width, height) = self.size();
        let (side, mask) = (1_i64 << self.shift, (1_i64 << self.shift) - 1);
        let mut at = x;
        let mut rest = out;
        while !rest.is_empty() {
            let len = i64::try_from(rest.len()).unwrap_or(i64::MAX);
            // The run to the next tile edge, or all of it off the picture.
            let run = if (0..i64::from(width)).contains(&at) {
                (side - (at & mask)).min(len)
            } else if at < 0 {
                (-at).min(len)
            } else {
                len
            };
            let (here, after) = rest.split_at_mut(usize::try_from(run).unwrap_or(rest.len()));
            let tile = match (u32::try_from(at), u32::try_from(y)) {
                (Ok(column), Ok(row)) if column < width && row < height => self.tile(column, row),
                _ => None,
            };
            match tile {
                Some(tile) => tile.row(at & mask, y & mask, here),
                None => here.fill(Pixel::TRANSPARENT),
            }
            at = at.saturating_add(run);
            rest = after;
        }
    }
}

impl<T: Borrow<Surface>> Mosaic<'_, T> {
    /// The tile holding `(x, y)`, if one is there.
    fn tile(&self, x: u32, y: u32) -> Option<&Surface> {
        let (column, row) = (x >> self.shift, y >> self.shift);
        if column >= self.columns || row >= self.rows {
            return None;
        }
        let index = usize::try_from(u64::from(row) * u64::from(self.columns) + u64::from(column));
        self.tiles.get(index.ok()?)?.as_ref().map(Borrow::borrow)
    }
}

impl Surface {
    /// Composite `src` over this surface at `strength` (`255` whole) through
    /// `transform`, which maps `src`'s pixel coordinates to this surface's,
    /// clipped to the surface and its clip window. A transform that collapses
    /// area draws nothing.
    pub fn blit_transformed<P: Picture + ?Sized>(
        &mut self,
        src: &P,
        transform: Affine,
        strength: u8,
    ) {
        self.blit_through(src, transform, strength, Edge::Fading);
    }

    /// Composite `src` over this surface at `strength` through `transform`
    /// as [`blit_transformed`](Self::blit_transformed) does, but covering
    /// exactly `src`'s rectangle: sampled no further out than its edge
    /// pixels, the rectangle's edge anti-aliased by how much of each pixel it
    /// covers. How a picture placed in a document is drawn, whatever its
    /// magnification.
    pub fn blit_framed<P: Picture + ?Sized>(&mut self, src: &P, transform: Affine, strength: u8) {
        self.blit_through(src, transform, strength, Edge::Framed);
    }

    fn blit_through<P: Picture + ?Sized>(
        &mut self,
        src: &P,
        transform: Affine,
        strength: u8,
        edge: Edge,
    ) {
        if strength == 0 {
            return;
        }
        let Some(inverse) = transform.invert() else {
            return;
        };
        let Some((x0, y0, x1, y1)) = reach(src.size(), transform, edge) else {
            return;
        };
        let Some((columns, rows)) = self.admitted(x0, y0, x1 - x0, y1 - y0) else {
            return;
        };
        let frames = frames(src.size(), inverse, edge);
        if inverse.b == 0.0 && inverse.c == 0.0 {
            self.blit_scaled(src, (inverse, strength, frames), columns, rows);
        } else {
            self.blit_turned(src, (inverse, strength, frames), columns, rows);
        }
    }

    /// The general path: every pixel maps its own centre back.
    fn blit_turned<P: Picture + ?Sized>(
        &mut self,
        src: &P,
        (inverse, strength, frames): (Affine, u8, Option<(Frame, Frame)>),
        columns: Range<u32>,
        rows: Range<u32>,
    ) {
        for y in rows {
            for (start, end) in chunks(columns.clone()) {
                let Some((first, span)) = self.row_span_mut(y, start, end - start) else {
                    continue;
                };
                let mut samples = [Pixel::TRANSPARENT; TAP_CHUNK as usize];
                for (x, slot) in (first..).zip(samples.iter_mut().take(span.len())) {
                    let (u, v) = inverse.apply((f64::from(x) + 0.5, f64::from(y) + 0.5));
                    *slot = match frames {
                        None => sample(src, tap(u - 0.5), tap(v - 0.5)),
                        Some((across, down)) => {
                            let ((u, wide), (v, tall)) = (across.place(u), down.place(v));
                            covered(sample(src, tap(u - 0.5), tap(v - 0.5)), wide, tall)
                        }
                    };
                }
                let len = span.len();
                blend_span(span, &samples[..len], strength, DitherRow::NEAREST, first);
            }
        }
    }

    /// The path for an `inverse` that neither turns nor shears: a column's
    /// source column depends on it alone, and a row's source row likewise, so
    /// each is worked out once, and the two source rows a destination row
    /// weighs are read once for each run of columns, kept while the next
    /// destination row still weighs them.
    fn blit_scaled<P: Picture + ?Sized>(
        &mut self,
        src: &P,
        (inverse, strength, frames): (Affine, u8, Option<(Frame, Frame)>),
        columns: Range<u32>,
        rows: Range<u32>,
    ) {
        let (mut near, mut far) = (Vec::new(), Vec::new());
        for (start, end) in chunks(columns) {
            let mut taps = [(0_i64, 0_u32); TAP_CHUNK as usize];
            let mut wides = [256_u32; TAP_CHUNK as usize];
            for ((x, slot), wide) in (start..end).zip(taps.iter_mut()).zip(wides.iter_mut()) {
                let (u, _) = inverse.apply((f64::from(x) + 0.5, 0.5));
                let u = match frames {
                    None => u,
                    Some((across, _)) => {
                        let (held, share) = across.place(u);
                        *wide = share;
                        held
                    }
                };
                *slot = tap(u - 0.5);
            }
            let taps = taps.get(..(end - start) as usize).unwrap_or_default();
            let lefts = taps.iter().map(|&(left, _)| left);
            let (Some(lo), Some(hi)) = (lefts.clone().min(), lefts.max()) else {
                continue;
            };
            let span = usize::try_from(hi - lo + 2).unwrap_or(usize::MAX);
            let held = fallible::grow_to(&mut near, span, Pixel::TRANSPARENT)
                && fallible::grow_to(&mut far, span, Pixel::TRANSPARENT);
            if !held {
                let columns = (start, taps, &wides[..taps.len()]);
                self.blit_scaled_by_sample(src, (inverse, strength, frames), columns, rows.clone());
                continue;
            }
            let mut read: Option<i64> = None;
            for y in rows.clone() {
                let (v, tall) = framed_row(inverse, y, frames);
                let (top, wy) = tap(v - 0.5);
                match read {
                    Some(row) if row == top => {}
                    Some(row) if row + 1 == top => {
                        core::mem::swap(&mut near, &mut far);
                        src.row(lo, top + 1, &mut far[..span]);
                    }
                    Some(row) if row == top + 1 => {
                        core::mem::swap(&mut near, &mut far);
                        src.row(lo, top, &mut near[..span]);
                    }
                    _ => {
                        src.row(lo, top, &mut near[..span]);
                        src.row(lo, top + 1, &mut far[..span]);
                    }
                }
                read = Some(top);
                let Some((first, dst)) = self.row_span_mut(y, start, end - start) else {
                    continue;
                };
                let skipped = usize::try_from(first - start).unwrap_or(usize::MAX);
                let Some(taps) = taps.get(skipped..skipped.saturating_add(dst.len())) else {
                    continue;
                };
                let wides = &wides[skipped..skipped + taps.len()];
                let mut samples = [Pixel::TRANSPARENT; TAP_CHUNK as usize];
                for ((slot, &(left, wx)), &wide) in samples.iter_mut().zip(taps).zip(wides) {
                    let at = usize::try_from(left - lo).unwrap_or(usize::MAX);
                    let weighed = match (
                        near.get(at..at.saturating_add(2)),
                        far.get(at..at.saturating_add(2)),
                    ) {
                        (Some(&[top_left, top_right]), Some(&[below_left, below_right])) => {
                            weigh([top_left, top_right, below_left, below_right], wx, wy)
                        }
                        _ => Pixel::TRANSPARENT,
                    };
                    *slot = covered(weighed, wide, tall);
                }
                blend_span(
                    dst,
                    &samples[..taps.len()],
                    strength,
                    DitherRow::NEAREST,
                    first,
                );
            }
        }
    }

    /// The scaled path a sample at a time, for a run whose source rows the
    /// heap refused room to hold.
    fn blit_scaled_by_sample<P: Picture + ?Sized>(
        &mut self,
        src: &P,
        (inverse, strength, frames): (Affine, u8, Option<(Frame, Frame)>),
        (start, taps, wides): (u32, &[(i64, u32)], &[u32]),
        rows: Range<u32>,
    ) {
        let end = start.saturating_add(u32::try_from(taps.len()).unwrap_or(TAP_CHUNK));
        for y in rows {
            let (v, tall) = framed_row(inverse, y, frames);
            let row = tap(v - 0.5);
            let Some((first, dst)) = self.row_span_mut(y, start, end - start) else {
                continue;
            };
            let skipped = usize::try_from(first - start).unwrap_or(usize::MAX);
            let Some(taps) = taps.get(skipped..skipped.saturating_add(dst.len())) else {
                continue;
            };
            let wides = wides.get(skipped..skipped + taps.len()).unwrap_or_default();
            let mut samples = [Pixel::TRANSPARENT; TAP_CHUNK as usize];
            for ((slot, &column), &wide) in samples.iter_mut().zip(taps).zip(wides) {
                *slot = covered(sample(src, column, row), wide, tall);
            }
            blend_span(
                dst,
                &samples[..taps.len()],
                strength,
                DitherRow::NEAREST,
                first,
            );
        }
    }
}

/// Where destination row `y` samples its source along the vertical, and, for
/// a framed picture, how much of the row's pixels its rectangle covers.
fn framed_row(inverse: Affine, y: u32, frames: Option<(Frame, Frame)>) -> (f64, u32) {
    let (_, v) = inverse.apply((0.5, f64::from(y) + 0.5));
    match frames {
        None => (v, 256),
        Some((_, down)) => down.place(v),
    }
}

/// `columns` in runs of at most [`TAP_CHUNK`], as `(start, end)`.
fn chunks(columns: Range<u32>) -> impl Iterator<Item = (u32, u32)> {
    columns
        .clone()
        .step_by(TAP_CHUNK as usize)
        .map(move |start| (start, columns.end.min(start.saturating_add(TAP_CHUNK))))
}

/// The destination pixels a picture of `size` drawn through `transform` can
/// touch, as `(x0, y0, x1, y1)` with the far edges exclusive, or `None` when
/// it touches none: the bounding box of the picture — grown, where its edge
/// fades, by the half pixel a bilinear edge blends into — mapped, so the edge
/// is whole at any scale, and widened by a pixel for rounding.
fn reach(
    (width, height): (u32, u32),
    transform: Affine,
    edge: Edge,
) -> Option<(u32, u32, u32, u32)> {
    let grown = match edge {
        Edge::Fading => 0.5,
        Edge::Framed => 0.0,
    };
    let (w, h) = (f64::from(width) + grown, f64::from(height) + grown);
    let corners =
        [(-grown, -grown), (w, -grown), (-grown, h), (w, h)].map(|corner| transform.apply(corner));
    let (mut left, mut top, mut right, mut bottom) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        left = mathf::fmin(left, x);
        top = mathf::fmin(top, y);
        right = mathf::fmax(right, x);
        bottom = mathf::fmax(bottom, y);
    }
    // A corner far off either side clamps to the edge rather than wrapping.
    let edge = |value: f64| u32::try_from(mathf::round_i32(value).max(0)).unwrap_or(0);
    let (x0, y0) = (
        edge(mathf::floor(left) - 1.0),
        edge(mathf::floor(top) - 1.0),
    );
    let (x1, y1) = (
        edge(mathf::ceil(right) + 1.0),
        edge(mathf::ceil(bottom) + 1.0),
    );
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// Where a sample at `at` — pixel `i`'s centre is `i` here — falls along one
/// axis: the pixel at or before it, and the weight out of 256 its successor
/// takes.
fn tap(at: f64) -> (i64, u32) {
    let floor = mathf::floor(at);
    let weight = u32::try_from(mathf::round_i32((at - floor) * 256.0).clamp(0, 256)).unwrap_or(0);
    (i64::from(mathf::round_i32(floor)), weight)
}

/// `src` sampled where `(left, wx)` and `(top, wy)` fall, by weighing the four
/// pixels around the point, everything outside `src` transparent.
fn sample<P: Picture + ?Sized>(src: &P, (left, wx): (i64, u32), (top, wy): (i64, u32)) -> Pixel {
    let (width, height) = src.size();
    if left < -1 || top < -1 || left >= i64::from(width) || top >= i64::from(height) {
        return Pixel::TRANSPARENT;
    }
    weigh(src.quad(left, top), wx, wy)
}

/// The four pixels about a sample, each weighted by how near it lies, their
/// weights summing to `1 << 16`, rounded to nearest. Two channels share a
/// word: a channel's sum stays below `1 << 24`, so neither carries into the
/// other.
fn weigh(quad: [Pixel; 4], wx: u32, wy: u32) -> Pixel {
    let weights = [
        (256 - wx) * (256 - wy),
        wx * (256 - wy),
        (256 - wx) * wy,
        wx * wy,
    ];
    let (mut red_blue, mut green_alpha) = (0_u64, 0_u64);
    for (pixel, weight) in quad.into_iter().zip(weights) {
        let weight = u64::from(weight);
        red_blue += (u64::from(pixel.r) | u64::from(pixel.b) << 32) * weight;
        green_alpha += (u64::from(pixel.g) | u64::from(pixel.a) << 32) * weight;
    }
    let channel =
        |sum: u64| u8::try_from(((sum & 0xffff_ffff) + (1 << 15)) >> 16).unwrap_or(u8::MAX);
    Pixel {
        r: channel(red_blue),
        g: channel(green_alpha),
        b: channel(red_blue >> 32),
        a: channel(green_alpha >> 32),
    }
}

#[cfg(test)]
#[path = "transformed_tests.rs"]
mod tests;
