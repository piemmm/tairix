//! The shared separable box blur.
//!
//! One definition serves every frosted surface: a control's own soft
//! highlight, and the cells of the half-resolution blur a window's backdrop is
//! frosted with ([`Surface::frost_halved_from`]). A horizontal pass then a
//! vertical one, each carrying a running sum, so the cost is the region's area
//! rather than its area times the radius.
//!
//! Every channel — alpha included — is averaged. Averaging premultiplied
//! channels is the correct operation on premultiplied data: the result is
//! the same convex combination of the contributing colours that compositing
//! them would give, so the `colour <= alpha` invariant survives and no halo
//! appears around a translucent edge.
//!
//! Edges replicate: a sample past the region's edge takes the edge pixel.
//! Every output therefore averages exactly `2 * radius + 1` samples, which
//! keeps the divisor constant across the pass and leaves a uniform field
//! exactly unchanged.

use core::ops::Range;

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::color::{mix, Pixel};
use crate::dither::DitherRow;
use crate::surface::{band_rows, RowBand, Surface};

#[path = "halved.rs"]
mod halved;

pub use halved::halved_reach;
#[cfg(any(test, feature = "test-util"))]
pub use halved::halved_reference;

/// Blur `region` in place: a dense, row-major, premultiplied
/// `width`×`height` block of pixels, blurred by a separable box blur of
/// `radius` physical pixels using `aux` as the intermediate buffer.
///
/// `aux` is supplied by the caller — so a blur costs no allocation on the
/// frame path. A caller frosting a rectangle of a surface takes
/// [`Surface::frost_region`] or [`Surface::frost_halved_from`] instead, which
/// own the shape-weighted mix back into it.
///
/// Nothing is blurred, and `region` is left exactly as it was, when the
/// radius is `0` (the effect is disabled), when either dimension is `0`, or
/// when `region` or `aux` is shorter than `width * height`: a caller that
/// mis-sizes a buffer gets the unblurred backdrop, never a partly-blurred
/// or out-of-bounds one.
pub fn box_blur(
    region: &mut [Pixel],
    width: usize,
    height: usize,
    radius: usize,
    aux: &mut [Pixel],
) {
    blur_block(region, width, height, radius, aux);
}

/// Blur `coverage` in place: a dense, row-major `width`×`height` block of
/// 8-bit coverage, by the same separable box blur [`box_blur`] gives pixels,
/// using `aux` as the intermediate buffer.
///
/// For a soft shape drawn in one colour — a text shadow — where only the
/// coverage spreads, so one channel is averaged where a pixel would carry
/// four. The refusals are [`box_blur`]'s.
pub fn box_blur_coverage(
    coverage: &mut [u8],
    width: usize,
    height: usize,
    radius: usize,
    aux: &mut [u8],
) {
    blur_block(coverage, width, height, radius, aux);
}

/// How many box passes [`soften_coverage`] runs: three are within a few
/// percent of a Gaussian, where one draws a visibly square halo.
pub const SOFTEN_PASSES: u32 = 3;

/// Soften `coverage` in place into a near-Gaussian falloff: [`SOFTEN_PASSES`]
/// passes of [`box_blur_coverage`] at `radius`, so it reaches
/// `radius * SOFTEN_PASSES` from where it started.
///
/// The one recipe for a soft shadow cast by a shape drawn in one colour — a
/// text run's, a pointer's. The refusals are [`box_blur`]'s.
pub fn soften_coverage(
    coverage: &mut [u8],
    width: usize,
    height: usize,
    radius: usize,
    aux: &mut [u8],
) {
    for _ in 0..SOFTEN_PASSES {
        box_blur_coverage(coverage, width, height, radius, aux);
    }
}

/// How a picture casts its soft shadow: its coverage dropped `drop_x` right
/// and `drop_y` down and softened by [`soften_coverage`] at `radius`, all in
/// physical pixels.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct ShadowCast {
    /// How far right of the picture the shadow falls.
    pub drop_x: u32,
    /// How far below the picture the shadow falls.
    pub drop_y: u32,
    /// The radius of each softening pass.
    pub radius: u32,
}

impl ShadowCast {
    /// How far the softened shadow spreads from where it was dropped.
    #[must_use]
    pub const fn reach(self) -> u32 {
        self.radius.saturating_mul(SOFTEN_PASSES)
    }

    /// The size of the mask [`cast_shadow`] casts from a `width` × `height`
    /// picture: the picture, dropped and spread by the reach either side.
    /// `None` where that overflows.
    #[must_use]
    pub fn extent(self, width: u32, height: u32) -> Option<(u32, u32)> {
        let reach = self.reach();
        let span = |side: u32, drop: u32| {
            reach
                .saturating_sub(drop)
                .checked_add(side)?
                .checked_add(drop)?
                .checked_add(reach)
        };
        Some((span(width, self.drop_x)?, span(height, self.drop_y)?))
    }

    /// Where a [`cast_shadow`] mask's origin lies from the picture's: up and
    /// left by the reach, less the drop.
    #[must_use]
    pub fn origin(self) -> (i32, i32) {
        let back =
            |drop: u32| i32::try_from(self.reach().saturating_sub(drop)).map_or(i32::MIN, |at| -at);
        (back(self.drop_x), back(self.drop_y))
    }
}

/// The soft shadow `picture` casts: a mask whose alpha is the shadow's
/// strength, to composite tinted ([`Surface::blit_tinted`]), and where the
/// mask's origin lies relative to the picture's. `None` for a cast that
/// softens nothing, or memory that cannot be had.
///
/// The one recipe for a shadow cast from a picture's own coverage: a pointer's
/// and an icon's are both this.
#[must_use]
pub fn cast_shadow(picture: &Surface, cast: ShadowCast) -> Option<(Surface, (i32, i32))> {
    if cast.radius == 0 {
        return None;
    }
    let reach = cast.reach();
    let left = reach.saturating_sub(cast.drop_x);
    let top = reach.saturating_sub(cast.drop_y);
    let (width, height) = cast.extent(picture.width(), picture.height())?;
    let across = usize::try_from(width).ok()?;
    let down = usize::try_from(height).ok()?;
    let count = across.checked_mul(down)?;
    let mut levels = fallible::filled(count, 0u8)?;
    let mut aux = fallible::filled(count, 0u8)?;
    let cast_x = usize::try_from(left.checked_add(cast.drop_x)?).ok()?;
    let cast_y = usize::try_from(top.checked_add(cast.drop_y)?).ok()?;
    let row_len = usize::try_from(picture.width()).ok()?;
    if row_len > 0 {
        for (row, source) in picture.pixels().chunks_exact(row_len).enumerate() {
            let start = cast_y
                .checked_add(row)?
                .checked_mul(across)?
                .checked_add(cast_x)?;
            let target = levels.get_mut(start..start.checked_add(row_len)?)?;
            for (level, pixel) in target.iter_mut().zip(source) {
                *level = pixel.a;
            }
        }
    }
    soften_coverage(
        &mut levels,
        across,
        down,
        usize::try_from(cast.radius).ok()?,
        &mut aux,
    );
    let mut mask = Surface::new(width, height)?;
    for (pixel, &level) in mask.pixels_mut().iter_mut().zip(&levels) {
        *pixel = Pixel {
            r: level,
            g: level,
            b: level,
            a: level,
        };
    }
    Some((mask, cast.origin()))
}

/// [`box_blur`] over any sample the window can average.
fn blur_block<S: Sample>(
    region: &mut [S],
    width: usize,
    height: usize,
    radius: usize,
    aux: &mut [S],
) {
    let Some(count) = width.checked_mul(height) else {
        return;
    };
    if radius == 0 || count == 0 || region.len() < count || aux.len() < count {
        return;
    }
    // The window is the same size for every output of both passes, so its
    // divisor is resolved here rather than per pixel.
    let recip = Reciprocal::new(radius.saturating_mul(2).saturating_add(1));
    for y in 0..height {
        let Some((src, dst)) = row_pair(region, aux, y, width) else {
            return;
        };
        S::line(src, dst, width, radius, recip, 0..width);
    }
    for x in 0..width {
        let Some((src, dst)) = column_pair(aux, region, x, count) else {
            return;
        };
        blur_span::<S, S::Window>(src, dst, width, height, radius, recip, 0..height);
    }
}

/// The fewest pixels a piece of a frost carries before it is worth handing to
/// another core.
///
/// A piece is a share of one pass over its own pixels, which at this size is
/// hundreds of microseconds even on a slow core — several times what a
/// dispatch's wake and park syscalls cost. Below it the frost runs on the
/// calling thread with no atomics, so a small frosted control costs what it
/// always did.
const MIN_PARALLEL_FROST_PX: usize = 8_192;

/// The share of the height a scratch is [reserved](BlurScratch::reserve) for
/// that one strip of the vertical pass covers.
///
/// The strip is the one buffer that follows a frost's height, so a quarter
/// holds it to a quarter of the plane the frost reads, at the price of three
/// more hand-offs for a frost as tall as the screen.
const STRIP_FRACTION: u32 = 4;

/// The working memory a frost runs in.
///
/// None of it grows with a frost's area: a horizontal-pass line per
/// participant, a running vertical sum per output column, the strip vertical
/// averages wait in before they are mixed back — a fraction of the
/// rectangle's height — the band and piece lists, and for a half-resolution
/// frost its plane of cells and the spread's lines.
/// [`Surface::frost_halved_from`] and [`Surface::blur_halved_from`] read their
/// backdrop from a plane their caller holds, so a caller that
/// [reserves](Self::reserve_halved) the scratch once frosts every frame
/// without allocating and is never refused. [`Surface::frost_region`] frosts
/// in place, so it also keeps here the copy of the backdrop it reads.
#[derive(Default)]
pub struct BlurScratch {
    plane: Option<Surface>,
    lines: Vec<Pixel>,
    sums: Vec<PixelSum>,
    strip: Vec<Pixel>,
    bands: Vec<Band>,
    pieces: Vec<Piece>,
    /// A half-resolution blur's cells, and the cells each of its bands is
    /// spread from.
    reduced: Option<Surface>,
    cells: Vec<(Range<u32>, Range<u32>)>,
    /// A row of cells taken between two rows of them, per participant.
    spread: Vec<u16>,
}

impl BlurScratch {
    /// An empty scratch, which allocates on its first frost and not before.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            plane: None,
            lines: Vec::new(),
            sums: Vec::new(),
            strip: Vec::new(),
            bands: Vec::new(),
            pieces: Vec::new(),
            reduced: None,
            cells: Vec::new(),
            spread: Vec::new(),
        }
    }

    /// Reserve what a full-resolution blur of up to `bands` bands of a
    /// rectangle no wider than `width` and no taller than `height` needs when
    /// `runner` spreads it, reporting whether it could: the in-place
    /// [`Surface::frost_region`], and the cells of a half-resolution frost,
    /// which [`reserve_halved`](Self::reserve_halved) reserves through this.
    ///
    /// A caller that frosts every frame reserves once, for its output, so no
    /// frost it runs grows the scratch on its frame path or finds it refused.
    /// A frost spreads only as widely as the scratch holds lines and pieces
    /// for, so a refusal here — which keeps what was reserved before — costs
    /// parallelism, never the frost.
    pub fn reserve(
        &mut self,
        width: u32,
        height: u32,
        bands: usize,
        runner: &dyn JobRunner,
    ) -> bool {
        let wide = usize::try_from(width).unwrap_or(usize::MAX);
        let strip_rows = usize::try_from(height.div_ceil(STRIP_FRACTION))
            .unwrap_or(usize::MAX)
            .max(bands)
            .max(1);
        let pieces = tairix_parallel::bands(runner, usize::MAX, 1).saturating_mul(bands.max(1));
        let (Some(lines), Some(sums), Some(strip)) = (
            wide.checked_mul(runner.width().max(1)),
            wide.checked_mul(bands.max(1)),
            wide.checked_mul(strip_rows),
        ) else {
            return false;
        };
        fallible::grow_to(&mut self.lines, lines, Pixel::TRANSPARENT)
            && fallible::grow_to(&mut self.sums, sums, PixelSum::default())
            && fallible::grow_to(&mut self.strip, strip, Pixel::TRANSPARENT)
            && room(&mut self.bands, bands.max(1))
            && room(&mut self.pieces, pieces)
    }

    /// Overwrite everything the scratch holds — what its last frosts read and
    /// wrote is a picture of whatever they frosted — keeping its reservation.
    ///
    /// Volatile, like a reclaimed surface's wipe, so the stores are not
    /// optimised away for memory nothing reads again.
    pub fn wipe(&mut self) {
        for plane in [self.plane.as_mut(), self.reduced.as_mut()]
            .into_iter()
            .flatten()
        {
            tairix_util::secret::wipe_with(plane.pixels_mut(), Pixel::TRANSPARENT);
        }
        tairix_util::secret::wipe_with(&mut self.lines, Pixel::TRANSPARENT);
        tairix_util::secret::wipe_with(&mut self.sums, PixelSum::default());
        tairix_util::secret::wipe_with(&mut self.strip, Pixel::TRANSPARENT);
        tairix_util::secret::wipe_with(&mut self.spread, 0);
    }
}

/// Clear `list` and make room in it for `count` entries without allocating
/// again.
fn room<T>(list: &mut Vec<T>, count: usize) -> bool {
    list.clear();
    list.try_reserve(count).is_ok()
}

impl Surface {
    /// Frost `[x, x+w) × [y, y+h)`: blur the pixels already there by
    /// `radius` and mix the blurred copy back over them at each pixel's
    /// `coverage` — `255` takes the blurred pixel, `0` keeps the original,
    /// and the values between are the weighted mix.
    ///
    /// This is the desktop's one frosted glass in place: a control frosting
    /// what it is drawn over. Weighting the mix rather than clipping it is
    /// what lets a rounded shape fade from frosted to untouched across its own
    /// arc instead of showing a square edge.
    ///
    /// `coverage` is asked about a pixel's position relative to the
    /// rectangle's **own** top-left, so a caller whose rectangle the surface
    /// edge or the clip window cuts short still reads the whole shape rather
    /// than re-fitting it to what survives.
    ///
    /// A partial coverage is a translucent field over a picture, so the mix
    /// rounds through the surface's own ordered dither: a frosted bar over a
    /// smooth wallpaper keeps the wallpaper's gradient instead of stepping it
    /// into plateaus. Full coverage takes the blurred pixel exactly and no
    /// coverage leaves the destination exactly, at every bias.
    ///
    /// The frost is confined to what the surface bounds and the active clip
    /// window admit and reads only the pixels it may write: samples past
    /// that edge replicate it, so the effect can neither pull a neighbour's
    /// pixels in nor mark a pixel outside. Unlike a window's frost
    /// ([`frost_halved_from`]) it is taken at full resolution: a control's
    /// plate is small and drawn once a repaint, so halving it would change the
    /// picture for nothing a frame pays.
    ///
    /// The surface is left exactly as it was, never partly frosted, when
    /// `radius` is `0` (the effect is disabled), when the rectangle is empty
    /// or lands nowhere the surface admits, or when the scratch could not be
    /// grown.
    ///
    /// [`frost_halved_from`]: Self::frost_halved_from
    #[expect(
        clippy::too_many_arguments,
        reason = "the rectangle is spelled as the four scalars every other \
                  Surface primitive takes, plus the blur radius, the caller's \
                  reused scratch, where its pieces run, and the shape being \
                  frosted"
    )]
    pub fn frost_region(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        radius: u32,
        scratch: &mut BlurScratch,
        runner: &dyn JobRunner,
        coverage: impl Fn(u32, u32) -> u8 + Sync,
    ) {
        if radius == 0 {
            return;
        }
        let Some((columns, rows)) = self.admitted(x, y, w, h) else {
            return;
        };
        let (width, height) = (
            columns.end.saturating_sub(columns.start),
            rows.end.saturating_sub(rows.start),
        );
        // The plane only ever receives the horizontal pass, so one already the
        // right size is reused as it stands rather than cleared.
        let plane = match scratch.plane.take() {
            Some(plane) if plane.width() == width && plane.height() == height => Some(plane),
            Some(plane) => plane.reshaped(width, height),
            None => Self::new(width, height),
        };
        let Some(mut plane) = plane else {
            return;
        };
        // A refusal keeps what was reserved before, which costs parallelism
        // and strip height, never the frost.
        let _ = scratch.reserve(width, height, 1, runner);
        let whole = (columns.clone(), rows.clone());
        let blurring = Blurring {
            rect: (x, y, w, h),
            bands: core::slice::from_ref(&whole),
            radius,
        };
        let origin = (columns.start, rows.start);
        let coverage =
            |column: u32, row: u32| coverage(column.saturating_sub(x), row.saturating_sub(y));
        let _ = self.frost_in_place((&mut plane, origin), &blurring, scratch, runner, &coverage);
        scratch.plane = Some(plane);
    }

    /// Write the full-resolution blur `blurring` describes, unmixed, into this
    /// surface, whose top-left lies at `at` of `plane` — past its top or left
    /// edge or not — reading the backdrop from `plane` alone, and report
    /// whether it was written.
    ///
    /// The bands and the rectangle are `plane`'s coordinates, and each band is
    /// written exactly as the whole rectangle's blur writes it, samples
    /// replicating at the rectangle's edges, so bands side by side meet without
    /// a seam. Bands may overlap: what two cover is written twice, the same
    /// both times. What of a band this surface does not reach is left out. The
    /// horizontal pass is written into `plane`, so what the plane holds there
    /// afterwards is unspecified.
    ///
    /// A radius of `0` copies the backdrop. `false` is a blur the scratch could
    /// not be grown for, and leaves this surface untouched.
    fn blur_from(
        &mut self,
        at: (i32, i32),
        plane: &mut Self,
        blurring: &Blurring<'_>,
        scratch: &mut BlurScratch,
        runner: &dyn JobRunner,
    ) -> bool {
        let Blurring {
            rect: (x, y, w, h),
            bands,
            radius,
        } = *blurring;
        let Some(admitted) = plane.admitted(x, y, w, h) else {
            return true;
        };
        let at = Offset { x: at.0, y: at.1 };
        let reach = at.reach(self);
        let lay = Lay::<fn(u32, u32) -> u8>::Field(at);
        if radius == 0 {
            let within = (clamp(&reach.0, &admitted.0), clamp(&reach.1, &admitted.1));
            lay_unblurred(self, &lay, plane, confined(bands, &within));
            return true;
        }
        let frost = Frost::new(admitted, radius);
        let Some(work) = frost.prepare(bands, &reach, scratch) else {
            return false;
        };
        if work.bands.is_empty() {
            return true;
        }
        frost.run(self, &lay, (plane, (0, 0)), work, runner)
    }

    /// Mix `source`, whose top-left lies at `at` of this surface — past its top
    /// or left edge or not — over `[x, x+w) × [y, y+h)` at each pixel's
    /// `coverage`, asked at this surface's coordinates: `255` takes the source
    /// pixel, `0` keeps this surface's, and the values between are the weighted
    /// mix, rounded at the surface's ordered dither.
    ///
    /// This is a frost's shape-weighted mix taken from a blur retained by
    /// [`blur_halved_from`](Self::blur_halved_from), so the two together write
    /// what [`frost_halved_from`](Self::frost_halved_from) writes. Pixels of
    /// the rectangle the source does not reach, or the clip window does not
    /// admit, are left as they are.
    pub fn mix_from(
        &mut self,
        source: &Self,
        at: (i32, i32),
        (x, y, w, h): (u32, u32, u32, u32),
        coverage: impl Fn(u32, u32) -> u8,
    ) {
        let at = Offset { x: at.0, y: at.1 };
        let (reach_cols, reach_rows) = at.reach(source);
        let cols = clamp(&(x..x.saturating_add(w)), &reach_cols);
        let rows = clamp(&(y..y.saturating_add(h)), &reach_rows);
        let Ok(wide) = u32::try_from(cols.len()) else {
            return;
        };
        for row in rows {
            let Some((first, dst)) = self.row_span_mut(row, cols.start, wide) else {
                continue;
            };
            let source_at = at.column_in(first).zip(at.row_in(row));
            let Some((column, source_row)) = source_at else {
                continue;
            };
            let wide = u32::try_from(dst.len()).unwrap_or(u32::MAX);
            let Some((_, src)) = source.row_span(source_row, column, wide) else {
                continue;
            };
            mix_covered(dst, src, (first, row), &coverage);
        }
    }

    /// Frost `blurring`'s bands of this surface from the pixels it holds
    /// itself, writing the horizontal pass into `plane`, whose top-left sits at
    /// the given point of this surface, and report whether it was written.
    fn frost_in_place(
        &mut self,
        (plane, origin): Plane<'_>,
        blurring: &Blurring<'_>,
        scratch: &mut BlurScratch,
        runner: &dyn JobRunner,
        coverage: &(impl Fn(u32, u32) -> u8 + Sync),
    ) -> bool {
        let Blurring {
            rect: (x, y, w, h),
            bands,
            radius,
        } = *blurring;
        if radius == 0 {
            return true;
        }
        let Some(admitted) = self.admitted(x, y, w, h) else {
            return true;
        };
        let reach = admitted.clone();
        let frost = Frost::new(admitted, radius);
        let Some(work) = frost.prepare(bands, &reach, scratch) else {
            return false;
        };
        if work.bands.is_empty() {
            return true;
        }
        frost.run(self, &Lay::Mix(coverage), (plane, origin), work, runner)
    }
}

/// The parts of `bands` inside `within`, leaving out those that cover nothing.
fn confined<'a>(
    bands: &'a [(Range<u32>, Range<u32>)],
    within: &'a (Range<u32>, Range<u32>),
) -> impl Iterator<Item = (Range<u32>, Range<u32>)> + Clone + Sync + 'a {
    bands.iter().filter_map(move |(cols, rows)| {
        let (cols, rows) = (clamp(cols, &within.0), clamp(rows, &within.1));
        (!cols.is_empty() && !rows.is_empty()).then_some((cols, rows))
    })
}

/// Lay the backdrop `plane` holds over `bands` into `target` as `lay` says,
/// unblurred: the blur of radius `0`.
fn lay_unblurred<C: Fn(u32, u32) -> u8>(
    target: &mut Surface,
    lay: &Lay<'_, C>,
    plane: &Surface,
    bands: impl Iterator<Item = (Range<u32>, Range<u32>)>,
) {
    let at = lay.at();
    for (cols, rows) in bands {
        let Ok(wide) = u32::try_from(cols.len()) else {
            continue;
        };
        for row in rows {
            let Some((first, src)) = plane.row_span(row, cols.start, wide) else {
                continue;
            };
            let Some((column, target_row)) = at.column_in(first).zip(at.row_in(row)) else {
                continue;
            };
            let wide = u32::try_from(src.len()).unwrap_or(u32::MAX);
            let Some((start, dst)) = target.row_span_mut(target_row, column, wide) else {
                continue;
            };
            // A clip window that cut the span's leading columns advances the
            // backdrop by as much.
            let skip = usize::try_from(start.saturating_sub(column)).unwrap_or(usize::MAX);
            let (Some(src), Some(backdrop_column)) = (src.get(skip..), at.column_of(start)) else {
                continue;
            };
            lay.span(dst, src, (backdrop_column, row));
        }
    }
}

/// Where one surface's top-left lies in the coordinates of another, which may
/// be past that one's top or left edge.
#[derive(Copy, Clone)]
struct Offset {
    x: i32,
    y: i32,
}

impl Offset {
    /// The column of this surface at column `x` of the other, or `None` where
    /// that lies before this surface's left edge.
    fn column_in(self, x: u32) -> Option<u32> {
        u32::try_from(i64::from(x) - i64::from(self.x)).ok()
    }

    /// The row of this surface at row `y` of the other, or `None` where that
    /// lies above this surface's top edge.
    fn row_in(self, y: u32) -> Option<u32> {
        u32::try_from(i64::from(y) - i64::from(self.y)).ok()
    }

    /// The column of the other surface at column `x` of this one.
    fn column_of(self, x: u32) -> Option<u32> {
        u32::try_from(i64::from(x) + i64::from(self.x)).ok()
    }

    /// The row of the other surface at row `y` of this one.
    fn row_of(self, y: u32) -> Option<u32> {
        u32::try_from(i64::from(y) + i64::from(self.y)).ok()
    }

    /// The columns and rows of the other surface `surface` covers.
    fn reach(self, surface: &Surface) -> (Range<u32>, Range<u32>) {
        let span = |at: i32, extent: u32| {
            let start = u32::try_from(at.max(0)).unwrap_or(0);
            let end = u32::try_from((i64::from(at) + i64::from(extent)).max(0)).unwrap_or(u32::MAX);
            start..end.max(start)
        };
        (
            span(self.x, surface.width()),
            span(self.y, surface.height()),
        )
    }
}

/// `range` confined to `within`, empty where they do not meet.
fn clamp(range: &Range<u32>, within: &Range<u32>) -> Range<u32> {
    let start = range.start.clamp(within.start, within.end);
    start..range.end.clamp(start, within.end)
}

/// Mix `averaged` over `dst` pixel for pixel, `dst[0]` lying at column `x` of
/// row `y`: each at its own coverage, rounding at the row's ordered dither.
fn mix_covered(
    dst: &mut [Pixel],
    averaged: &[Pixel],
    (x, y): (u32, u32),
    coverage: &impl Fn(u32, u32) -> u8,
) {
    let dither = DitherRow::at(y);
    for ((dst, &src), column) in dst.iter_mut().zip(averaged).zip(x..) {
        *dst = mix(*dst, src, coverage(column, y), dither.bias(column));
    }
}

/// The blur a [`Surface::blur_halved_from`] writes or a
/// [`Surface::frost_halved_from`] mixes back, in the coordinates of the
/// surface it reads.
pub struct Blurring<'a> {
    /// The rectangle `(x, y, w, h)` the blur is a function of: samples past its
    /// edges replicate them.
    pub rect: (u32, u32, u32, u32),
    /// The parts of the rectangle written, which must not overlap.
    pub bands: &'a [(Range<u32>, Range<u32>)],
    /// How far the blur spreads, in pixels.
    pub radius: u32,
}

/// Where a frost's averages go once they are taken.
enum Lay<'c, C> {
    /// Mixed over the destination at each pixel's coverage, asked at the
    /// destination's coordinates. A [`Frost`] mixed back is taken in place,
    /// reading its backdrop from the destination.
    Mix(&'c C),
    /// Written as they are into a surface whose top-left lies at this point
    /// of the backdrop's coordinates.
    Field(Offset),
}

impl<C: Fn(u32, u32) -> u8> Lay<'_, C> {
    /// Where the target's top-left lies in the backdrop's coordinates.
    const fn at(&self) -> Offset {
        match self {
            Self::Mix(_) => Offset { x: 0, y: 0 },
            Self::Field(at) => *at,
        }
    }

    /// Lay `averaged` into `dst`, whose first pixel lies at `(x, y)` of the
    /// backdrop's coordinates.
    fn span(&self, dst: &mut [Pixel], averaged: &[Pixel], (x, y): (u32, u32)) {
        match self {
            Self::Mix(coverage) => mix_covered(dst, averaged, (x, y), coverage),
            Self::Field(_) => {
                let len = dst.len().min(averaged.len());
                if let (Some(dst), Some(averaged)) = (dst.get_mut(..len), averaged.get(..len)) {
                    dst.copy_from_slice(averaged);
                }
            }
        }
    }
}

/// The rectangle a frost is a function of: the columns and rows of it the
/// destination admits, and the blur it spreads by.
///
/// Replication is read from *this* rectangle whichever band of it a call
/// writes, which is what makes frosting a border equal frosting the whole and
/// keeping the middle.
struct Frost {
    columns: Range<u32>,
    rows: Range<u32>,
    radius: usize,
    recip: Reciprocal,
}

/// A part of a frosted rectangle, in that rectangle's own columns and rows.
#[derive(Clone)]
struct Band {
    cols: Range<usize>,
    rows: Range<usize>,
}

impl Band {
    fn is_empty(&self) -> bool {
        self.cols.is_empty() || self.rows.is_empty()
    }

    fn area(&self) -> usize {
        self.cols.len().saturating_mul(self.rows.len())
    }
}

/// One column share of a band's vertical pass.
#[derive(Clone)]
struct Piece {
    cols: Range<usize>,
    rows: Range<usize>,
}

/// The plane a frost reads its backdrop from, and the destination coordinate
/// of the plane's own top-left.
type Plane<'a> = (&'a mut Surface, (u32, u32));

/// The bands one frost writes and the scratch buffers it works in.
struct Work<'a> {
    bands: &'a [Band],
    lines: &'a mut [Pixel],
    sums: &'a mut [PixelSum],
    strip: &'a mut [Pixel],
    pieces: &'a mut Vec<Piece>,
}

/// One piece's share of a strip: the rows of it the piece covers, the running
/// sums it carries from strip to strip, and the block its averages go to.
struct Averaging<'a> {
    piece: &'a Piece,
    rows: Range<usize>,
    sums: &'a mut [PixelSum],
    block: &'a mut [Pixel],
}

impl Frost {
    fn new((columns, rows): (Range<u32>, Range<u32>), radius: u32) -> Self {
        let radius = usize::try_from(radius).unwrap_or(usize::MAX);
        Self {
            columns,
            rows,
            radius,
            recip: Reciprocal::new(radius.saturating_mul(2).saturating_add(1)),
        }
    }

    fn width(&self) -> usize {
        usize::try_from(self.columns.end.saturating_sub(self.columns.start)).unwrap_or(0)
    }

    fn height(&self) -> usize {
        usize::try_from(self.rows.end.saturating_sub(self.rows.start)).unwrap_or(0)
    }

    /// The destination columns `cols` and rows `rows` as a band of this
    /// rectangle, or `None` where they land outside it.
    fn local(&self, cols: &Range<u32>, rows: &Range<u32>) -> Option<Band> {
        let confine = |asked: &Range<u32>, within: &Range<u32>| {
            let kept = clamp(asked, within);
            let offset = |at: u32| usize::try_from(at.saturating_sub(within.start)).unwrap_or(0);
            offset(kept.start)..offset(kept.end)
        };
        let band = Band {
            cols: confine(cols, &self.columns),
            rows: confine(rows, &self.rows),
        };
        (!band.is_empty()).then_some(band)
    }

    /// The bands of `bands` inside `within` this frost writes, and the scratch
    /// grown to write them, or `None` where it could not be grown.
    fn prepare<'s>(
        &self,
        bands: &[(Range<u32>, Range<u32>)],
        within: &(Range<u32>, Range<u32>),
        scratch: &'s mut BlurScratch,
    ) -> Option<Work<'s>> {
        let BlurScratch {
            lines,
            sums,
            strip,
            bands: written,
            pieces,
            ..
        } = scratch;
        written.clear();
        let mut columns = 0usize;
        for (cols, rows) in bands {
            let Some(band) = self.local(&clamp(cols, &within.0), &clamp(rows, &within.1)) else {
                continue;
            };
            if !fallible::reserve(written, 1) {
                return None;
            }
            columns = columns.saturating_add(band.cols.len());
            written.push(band);
        }
        let width = self.width();
        // A strip always holds a row of every band, which bands that overlap
        // can need more than the rectangle's width for.
        let grown = written.is_empty()
            || (fallible::grow_to(lines, width, Pixel::TRANSPARENT)
                && fallible::grow_to(sums, columns, PixelSum::default())
                && fallible::grow_to(strip, columns.max(width), Pixel::TRANSPARENT));
        grown.then_some(Work {
            bands: written,
            lines,
            sums,
            strip,
            pieces,
        })
    }

    /// The columns averaging `cols` horizontally reads: `radius` beyond them on
    /// both sides, or the rectangle's own edge.
    fn reach(&self, cols: &Range<usize>) -> Range<usize> {
        cols.start.saturating_sub(self.radius)
            ..cols.end.saturating_add(self.radius).min(self.width())
    }

    /// The rows `band`'s vertical pass reads: `radius` beyond it on both
    /// sides, or the rectangle's own edge, whichever comes first — exactly
    /// where the replication begins.
    fn pass_rows(&self, band: &Band) -> Range<usize> {
        band.rows.start.saturating_sub(self.radius)
            ..band.rows.end.saturating_add(self.radius).min(self.height())
    }

    /// The destination row of the rectangle's own row `row`.
    fn row(&self, row: usize) -> u32 {
        self.rows
            .start
            .saturating_add(u32::try_from(row).unwrap_or(u32::MAX))
    }

    /// The destination column of the rectangle's own column `column`.
    fn column(&self, column: usize) -> u32 {
        self.columns
            .start
            .saturating_add(u32::try_from(column).unwrap_or(u32::MAX))
    }

    /// Blur `work`'s bands from the backdrop — the destination itself for a
    /// frost mixed back, `plane` otherwise — laying each into `target` as `lay`
    /// says, and report whether the scratch could hold the blur.
    ///
    /// The horizontal pass is written into the plane, which keeps the scratch
    /// free of anything the size of the rectangle. The vertical pass runs a
    /// strip at a time, each piece sliding a running sum per column down its
    /// band, primed once; a strip is laid into the target once all of it is
    /// written, because its pieces are split by column and its laying by row.
    fn run<C: Fn(u32, u32) -> u8 + Sync>(
        &self,
        target: &mut Surface,
        lay: &Lay<'_, C>,
        (plane, origin): Plane<'_>,
        work: Work<'_>,
        runner: &dyn JobRunner,
    ) -> bool {
        let width = self.width();
        let Work {
            bands,
            lines,
            sums,
            strip,
            pieces,
        } = work;
        if bands.is_empty() {
            return true;
        }
        if width == 0 || lines.len() < width || strip.len() < width {
            return false;
        }
        let read = PlaneRead {
            frost: self,
            origin,
        };
        let held = matches!(lay, Lay::Mix(_)).then_some(&*target);
        read.blur_lines(plane, bands, lines, runner, held);
        let Some(columns) = divide(bands, pieces, runner) else {
            return false;
        };
        let Some(sums) = sums.get_mut(..columns) else {
            return false;
        };
        let plane: &Surface = plane;
        let per = strip.len() / width;
        let mut start = 0;
        // Strips keep to the runs of rows the pieces cover: rows between
        // distant bands hold nothing to average or lay.
        let spans = pieces.iter().map(|piece| piece.rows.clone());
        while let Some(run) = covered_run(&spans, start) {
            start = run.start;
            let mut end = start.saturating_add(per).min(run.end);
            // Disjoint bands never need more than the strip's rows times the
            // rectangle's width; bands that overlap are given fewer rows.
            while strip_need(pieces, &(start..end)) > strip.len() && end > start.saturating_add(1) {
                end = start.saturating_add((end - start) / 2);
            }
            if strip_need(pieces, &(start..end)) <= strip.len() {
                read.average_strip(plane, pieces, sums, strip, &(start..end), runner);
                self.lay_strip(target, lay, pieces, strip, &(start..end), runner);
            }
            start = end;
        }
        true
    }

    /// Lay every piece's averages for `rows` into `target`, split into bands
    /// of whole target rows across `runner`.
    fn lay_strip<C: Fn(u32, u32) -> u8 + Sync>(
        &self,
        target: &mut Surface,
        lay: &Lay<'_, C>,
        pieces: &[Piece],
        strip: &[Pixel],
        rows: &Range<usize>,
        runner: &dyn JobRunner,
    ) {
        let count = tairix_parallel::bands(
            runner,
            rows.len(),
            MIN_PARALLEL_FROST_PX.div_ceil(self.width().max(1)),
        );
        let per = band_rows(rows.len(), count);
        let at = lay.at();
        let target_row = |row: u32| at.row_in(row).unwrap_or(0);
        let bands = target.row_bands_mut(
            target_row(self.row(rows.start))..target_row(self.row(rows.end)),
            per,
        );
        tairix_parallel::for_each_drawn(runner, bands, &|mut band| {
            self.lay_rows(&mut band, lay, pieces, strip, rows);
        });
    }

    /// Lay, over the target rows `band` owns, what every piece covering them
    /// averaged there in the strip over `rows`.
    ///
    /// The blocks lie in the strip in piece order, each a piece's columns wide
    /// and as tall as the rows of the strip it covers, which is how
    /// [`average_strip`](PlaneRead::average_strip) laid them out.
    fn lay_rows<C: Fn(u32, u32) -> u8>(
        &self,
        band: &mut RowBand<'_>,
        lay: &Lay<'_, C>,
        pieces: &[Piece],
        strip: &[Pixel],
        rows: &Range<usize>,
    ) {
        let at = lay.at();
        for row in band.rows() {
            let Some(backdrop_row) = at.row_of(row) else {
                continue;
            };
            let Some(local) = backdrop_row
                .checked_sub(self.rows.start)
                .and_then(|local| usize::try_from(local).ok())
            else {
                continue;
            };
            let mut offset = 0usize;
            for piece in pieces {
                let covered = overlap(&piece.rows, rows);
                let width = piece.cols.len();
                if covered.contains(&local) {
                    let block =
                        offset.saturating_add((local - covered.start).saturating_mul(width));
                    let asked = self.column(piece.cols.start);
                    let spans = strip
                        .get(block..block.saturating_add(width))
                        .zip(at.column_in(asked));
                    if let Some((averaged, column)) = spans {
                        let wide = u32::try_from(width).unwrap_or(u32::MAX);
                        let laid = band.row_span_mut(row, column, wide);
                        let placed =
                            laid.and_then(|(first, dst)| Some((at.column_of(first)?, first, dst)));
                        if let Some((backdrop_column, first, dst)) = placed {
                            // A clip window that cut the span's leading columns
                            // advances the averages by as much.
                            let skip =
                                usize::try_from(first.saturating_sub(column)).unwrap_or(usize::MAX);
                            let averaged = averaged.get(skip..).unwrap_or_default();
                            lay.span(dst, averaged, (backdrop_column, backdrop_row));
                        }
                    }
                }
                offset = offset.saturating_add(width.saturating_mul(covered.len()));
            }
        }
    }
}

/// A frost's view of the plane it reads: the rectangle, and where the plane's
/// own top-left sits in the destination's coordinates.
struct PlaneRead<'a> {
    frost: &'a Frost,
    origin: (u32, u32),
}

impl PlaneRead<'_> {
    /// The plane row holding the rectangle's own row `row`.
    fn row(&self, row: usize) -> u32 {
        self.frost.row(row).saturating_sub(self.origin.1)
    }

    /// The plane column holding the rectangle's own column `column`.
    fn column(&self, column: usize) -> u32 {
        self.frost.column(column).saturating_sub(self.origin.0)
    }

    /// Run the horizontal pass over every row a band's vertical pass reads,
    /// writing the averages back into `plane` over the band's own columns.
    ///
    /// Rows are independent, so they are split across `runner` in bands of the
    /// plane, each with a line of its own: a row's averages are all taken from
    /// the backdrop before any is written back, because bands side by side
    /// read one another's columns. Each run of rows the bands read is split on
    /// its own, so rows between distant bands take no participant's share.
    fn blur_lines(
        &self,
        plane: &mut Surface,
        bands: &[Band],
        lines: &mut [Pixel],
        runner: &dyn JobRunner,
        held_by: Option<&Surface>,
    ) {
        let width = self.frost.width().max(1);
        let reads = bands.iter().map(|band| self.frost.pass_rows(band));
        let mut start = 0;
        while let Some(run) = covered_run(&reads, start) {
            let count =
                tairix_parallel::bands(runner, run.len(), MIN_PARALLEL_FROST_PX.div_ceil(width))
                    .clamp(1, (lines.len() / width).max(1));
            let work = plane
                .row_bands_mut(
                    self.row(run.start)..self.row(run.end),
                    band_rows(run.len(), count),
                )
                .zip(lines.chunks_exact_mut(width));
            tairix_parallel::for_each_drawn(runner, work, &|(mut band, line)| {
                self.blur_band_lines(&mut band, line, bands, held_by);
            });
            start = run.end;
        }
    }

    /// The horizontal pass over the plane rows `rows` owns, a row at a time:
    /// each band's averages written into the plane over the band's columns.
    ///
    /// A frost in place averages each row straight from the destination
    /// `held_by`. Otherwise a row's backdrop is gathered into `line` first,
    /// since its averages are written into the very row of the plane they
    /// read.
    fn blur_band_lines(
        &self,
        rows: &mut RowBand<'_>,
        line: &mut [Pixel],
        bands: &[Band],
        held_by: Option<&Surface>,
    ) {
        let frost = self.frost;
        let width = frost.width();
        let (Ok(span), first) = (u32::try_from(width), self.column(0)) else {
            return;
        };
        for plane_row in rows.rows() {
            let Some(local) = plane_row
                .checked_add(self.origin.1)
                .and_then(|row| row.checked_sub(frost.rows.start))
                .and_then(|local| usize::try_from(local).ok())
            else {
                continue;
            };
            let reading = || {
                bands
                    .iter()
                    .filter(move |band| frost.pass_rows(band).contains(&local))
            };
            let Some(read) = reading().map(|band| frost.reach(&band.cols)).reduce(hull) else {
                continue;
            };
            let Some((start, target)) = rows.row_span_mut(plane_row, first, span) else {
                continue;
            };
            if start != first || target.len() != width {
                continue;
            }
            let source = if let Some(dest) = held_by {
                match dest.row_span(frost.row(local), frost.column(0), span) {
                    Some((at, row)) if at == frost.column(0) && row.len() == width => row,
                    _ => continue,
                }
            } else {
                let (Some(gathered), Some(backdrop)) =
                    (line.get_mut(read.clone()), target.get(read.clone()))
                else {
                    continue;
                };
                gathered.copy_from_slice(backdrop);
                &*line
            };
            for band in reading() {
                if let Some(out) = target.get_mut(band.cols.clone()) {
                    Pixel::line(
                        source,
                        out,
                        width,
                        frost.radius,
                        frost.recip,
                        band.cols.clone(),
                    );
                }
            }
        }
    }

    /// Take the vertical averages of `rows` for every piece covering any of
    /// them into its own block of `strip`, laid out in piece order.
    ///
    /// A piece's running sums carry from one strip to the next, so it is
    /// primed once, at its band's first row, however many strips its rows
    /// span.
    fn average_strip(
        &self,
        plane: &Surface,
        pieces: &[Piece],
        sums: &mut [PixelSum],
        strip: &mut [Pixel],
        rows: &Range<usize>,
        runner: &dyn JobRunner,
    ) {
        let covering = pieces
            .iter()
            .filter(|piece| !overlap(&piece.rows, rows).is_empty())
            .count();
        // One covering piece is all a strip of distant bands holds, and the
        // others answer nothing, so there is nothing to hand to another core.
        let runner = if covering > 1 {
            runner
        } else {
            &tairix_parallel::SERIAL
        };
        let (mut sums, mut strip) = (sums, strip);
        // Every piece's sums are split off in turn, since they lie in piece
        // order; only the pieces covering the strip are handed out.
        let jobs = pieces.iter().filter_map(move |piece| {
            let all_sums = core::mem::take(&mut sums);
            let (held, rest) = all_sums.split_at_mut(piece.cols.len().min(all_sums.len()));
            sums = rest;
            let covered = overlap(&piece.rows, rows);
            if covered.is_empty() {
                return None;
            }
            let all_strip = core::mem::take(&mut strip);
            let need = piece
                .cols
                .len()
                .saturating_mul(covered.len())
                .min(all_strip.len());
            let (block, rest) = all_strip.split_at_mut(need);
            strip = rest;
            Some(Averaging {
                piece,
                rows: covered,
                sums: held,
                block,
            })
        });
        tairix_parallel::for_each_drawn(runner, jobs, &|mut job| {
            self.average(plane, &mut job);
        });
    }

    /// One piece's averages over its rows of a strip: primed at its band's
    /// first row, then slid a row at a time — `blur_span`'s walk turned on its
    /// side, so every output is the very sum it would have taken.
    fn average(&self, plane: &Surface, job: &mut Averaging<'_>) {
        let Averaging {
            piece,
            rows,
            sums,
            block,
        } = job;
        let width = piece.cols.len();
        if width == 0 {
            return;
        }
        let ops = crate::accel::blur_for(self.frost.recip);
        for (row, out) in rows.clone().zip(block.chunks_exact_mut(width)) {
            if row == piece.rows.start {
                self.prime(plane, piece, row, sums, ops.add);
            }
            (ops.mean)(out, sums, self.frost.recip);
            if row.saturating_add(1) < piece.rows.end {
                self.slide(plane, piece, row, sums, ops.slide);
            }
        }
    }

    /// Set `sums` to the vertical window around the rectangle's own row
    /// `row`: `2 * radius + 1` rows, any past the rectangle's edges counted as
    /// copies of its first or last row — arithmetically, as `blur_span` counts
    /// them, so priming costs the rectangle's height at most whatever the
    /// radius.
    fn prime(
        &self,
        plane: &Surface,
        piece: &Piece,
        row: usize,
        sums: &mut [PixelSum],
        add: crate::accel::RowAdd,
    ) {
        sums.fill(PixelSum::default());
        let Some(last) = self.frost.height().checked_sub(1) else {
            return;
        };
        let radius = self.frost.radius;
        self.gather(plane, piece, (0, radius.saturating_sub(row)), sums, add);
        for sample in row.saturating_sub(radius)..=row.saturating_add(radius).min(last) {
            self.gather(plane, piece, (sample, 1), sums, add);
        }
        let past = row.saturating_add(radius).saturating_sub(last);
        self.gather(plane, piece, (last, past), sums, add);
    }

    /// Add `times` copies of the averages the rectangle's own row `row` holds
    /// over `piece`'s columns to `sums`, one copy through `add`.
    fn gather(
        &self,
        plane: &Surface,
        piece: &Piece,
        (row, times): (usize, usize),
        sums: &mut [PixelSum],
        add: crate::accel::RowAdd,
    ) {
        if times == 0 {
            return;
        }
        let Some(averaged) = self.averaged(plane, piece, row) else {
            return;
        };
        if times == 1 {
            add(sums, averaged);
        } else {
            for (sum, &pixel) in sums.iter_mut().zip(averaged) {
                sum.add_many(pixel, times);
            }
        }
    }

    /// Move `sums` from the window around the rectangle's own row `row` to
    /// the one around the row after it.
    fn slide(
        &self,
        plane: &Surface,
        piece: &Piece,
        row: usize,
        sums: &mut [PixelSum],
        kernel: crate::accel::RowSlide,
    ) {
        let Some(last) = self.frost.height().checked_sub(1) else {
            return;
        };
        let radius = self.frost.radius;
        let entering = row.saturating_add(radius).saturating_add(1).min(last);
        let leaving = row.saturating_sub(radius);
        let (Some(added), Some(removed)) = (
            self.averaged(plane, piece, entering),
            self.averaged(plane, piece, leaving),
        ) else {
            return;
        };
        kernel(sums, added, removed);
    }

    /// The horizontal averages the rectangle's own row `row` holds over
    /// `piece`'s columns, or `None` where the plane does not hold them.
    fn averaged<'p>(&self, plane: &'p Surface, piece: &Piece, row: usize) -> Option<&'p [Pixel]> {
        let first = self.column(piece.cols.start);
        let (start, averaged) =
            plane.row_span(self.row(row), first, u32::try_from(piece.cols.len()).ok()?)?;
        (start == first && averaged.len() == piece.cols.len()).then_some(averaged)
    }
}

/// The first run of rows at or after `from` that `spans` cover without a
/// gap, or `None` when they cover none.
fn covered_run(
    spans: &(impl Iterator<Item = Range<usize>> + Clone),
    from: usize,
) -> Option<Range<usize>> {
    let start = spans
        .clone()
        .filter(|rows| rows.end > from)
        .map(|rows| rows.start.max(from))
        .min()?;
    let mut end = start;
    while let Some(further) = spans
        .clone()
        .filter(|rows| rows.start <= end && rows.end > end)
        .map(|rows| rows.end)
        .max()
    {
        end = further;
    }
    Some(start..end)
}

/// The strip one pass over `rows` lays its pieces' averages out in: each
/// piece's columns times the rows of `rows` it covers.
fn strip_need(pieces: &[Piece], rows: &Range<usize>) -> usize {
    pieces
        .iter()
        .map(|piece| {
            piece
                .cols
                .len()
                .saturating_mul(overlap(&piece.rows, rows).len())
        })
        .fold(0, usize::saturating_add)
}

/// Fill `pieces` with each band divided across its columns into as many
/// pieces as `runner` is worth splitting it for and the list has room for,
/// returning how many running sums they hold between them, or `None` when the
/// list cannot take even a piece per band.
///
/// Dividing by columns is what keeps a piece independent: its sums slide down
/// its own columns, so no piece reads or writes another's. The list is never
/// grown past its reservation for a finer division, since how finely a band is
/// divided changes no pixel.
fn divide(bands: &[Band], pieces: &mut Vec<Piece>, runner: &dyn JobRunner) -> Option<usize> {
    pieces.clear();
    if pieces.capacity() < bands.len() && !fallible::reserve(pieces, bands.len()) {
        return None;
    }
    let mut columns = 0usize;
    for (at, band) in bands.iter().enumerate() {
        let later = bands.len() - at - 1;
        let spare = pieces
            .capacity()
            .saturating_sub(pieces.len())
            .saturating_sub(later)
            .max(1);
        let count =
            tairix_parallel::bands(runner, band.area(), MIN_PARALLEL_FROST_PX).clamp(1, spare);
        let per = tairix_parallel::piece_len(band.cols.len(), count);
        let mut start = band.cols.start;
        while start < band.cols.end {
            let end = start.saturating_add(per).min(band.cols.end);
            pieces.push(Piece {
                cols: start..end,
                rows: band.rows.clone(),
            });
            columns = columns.checked_add(end - start)?;
            start = end;
        }
    }
    Some(columns)
}

/// The part of `a` inside `b`, empty where they do not meet.
fn overlap(a: &Range<usize>, b: &Range<usize>) -> Range<usize> {
    let start = a.start.max(b.start);
    start..a.end.min(b.end).max(start)
}

/// The least range holding both `a` and `b`.
fn hull(a: Range<usize>, b: Range<usize>) -> Range<usize> {
    a.start.min(b.start)..a.end.max(b.end)
}

/// Row `y` of `src` and of `dst`, both `width` samples wide.
fn row_pair<'a, S>(
    src: &'a [S],
    dst: &'a mut [S],
    y: usize,
    width: usize,
) -> Option<(&'a [S], &'a mut [S])> {
    let start = y.checked_mul(width)?;
    let end = start.checked_add(width)?;
    Some((src.get(start..end)?, dst.get_mut(start..end)?))
}

/// Column `x` of `src` and of `dst`, each the `count`-sample block's tail
/// from that column on: the strided [`blur_span`] walks it a row at a time.
fn column_pair<'a, S>(
    src: &'a [S],
    dst: &'a mut [S],
    x: usize,
    count: usize,
) -> Option<(&'a [S], &'a mut [S])> {
    Some((src.get(x..count)?, dst.get_mut(x..count)?))
}

/// Average the samples of `src` with their neighbours within `radius`,
/// writing the outputs `out` names to `dst`. Both are walked with `stride`
/// pixels between consecutive samples, so one implementation serves the
/// horizontal pass (stride `1`) and the vertical one (stride `width`).
///
/// `src` is the whole line of `len` samples, indexed from its own start; `dst`
/// receives `out.len()` pixels from *its* start, so a caller taking part of a
/// line writes a buffer only that wide. `out` is confined to the line, and an
/// empty one writes nothing.
///
/// The window slides by adding the sample entering it and subtracting the
/// one leaving, so each output costs a constant amount of work whatever the
/// radius. Samples outside `0..len` replicate the nearest edge, which keeps
/// the divisor at `2 * radius + 1` for every output — constant for the whole
/// pass, which is why `recip` is resolved once by the caller. That
/// replication is read from the *line*, never from where `out` begins, which
/// is what makes a part of a line answer exactly as the whole of it does.
///
/// The output slot and the two samples the window trades are each monotone
/// along the line, so all three are walked as strided iterators and the
/// furthest offset any of them can reach is bounds-checked once here instead
/// of per sample. An iterator that runs out is exactly a clamped end, so the
/// replicated edge pixel stands in for it — which is why `src` is confined to
/// the line before the walk begins rather than trusted to end with it: a
/// caller passing a buffer with further pixels after the line (a band of a
/// larger scratch) would otherwise read one of those as a replicated edge.
pub(crate) fn blur_span<S: Copy, W: Window<S>>(
    src: &[S],
    dst: &mut [S],
    stride: usize,
    len: usize,
    radius: usize,
    recip: Reciprocal,
    out: Range<usize>,
) {
    let Some(last) = len.checked_sub(1) else {
        return;
    };
    let Some(last_offset) = last.checked_mul(stride) else {
        return;
    };
    let Some(src) = src.get(..=last_offset) else {
        return;
    };
    let (Some(&first), Some(&edge)) = (src.first(), src.get(last_offset)) else {
        return;
    };
    let from = out.start.min(len);
    let Some(count) = out
        .end
        .min(len)
        .checked_sub(from)
        .filter(|asked| *asked > 0)
    else {
        return;
    };
    if dst.len() <= (count - 1).saturating_mul(stride) {
        return;
    }

    // Prime the window over `from - radius ..= from + radius`. The replicated
    // ends are counted arithmetically rather than sample by sample, so priming
    // costs the line's length at most however wide the radius is.
    let (lead, trail) = (
        from.saturating_sub(radius),
        from.saturating_add(radius).min(last),
    );
    let mut sum = W::default();
    sum.add_many(first, radius.saturating_sub(from));
    for &sample in src
        .get(lead.saturating_mul(stride)..)
        .unwrap_or_default()
        .iter()
        .step_by(stride)
        .take(trail - lead + 1)
    {
        sum.add(sample);
    }
    sum.add_many(edge, from.saturating_add(radius).saturating_sub(last));

    let mut entering = src
        .get(trail.saturating_add(1).min(last).saturating_mul(stride)..)
        .unwrap_or_default()
        .iter()
        .step_by(stride);
    // The outputs whose trailing edge has not yet cleared the start of the
    // line, whose leaving sample is therefore the replicated first pixel.
    // Splitting the walk there costs that clamp nothing at all.
    let clamped = radius.saturating_add(1).saturating_sub(from).min(count);
    let mut leaving = src
        .get(
            from.saturating_add(clamped)
                .saturating_sub(radius)
                .saturating_mul(stride)..,
        )
        .unwrap_or_default()
        .iter()
        .step_by(stride);
    let mut slots = dst.iter_mut().step_by(stride).take(count);

    for slot in slots.by_ref().take(clamped) {
        *slot = sum.mean(recip);
        sum.add(*entering.next().unwrap_or(&edge));
        sum.sub(first);
    }
    for slot in slots {
        *slot = sum.mean(recip);
        sum.add(*entering.next().unwrap_or(&edge));
        sum.sub(*leaving.next().unwrap_or(&first));
    }
}

/// What the sliding window averages: a pixel's four premultiplied channels,
/// or one byte of coverage.
trait Sample: Copy {
    /// The running sums of the samples inside the window.
    type Window: Window<Self>;

    /// [`blur_span`] along a line, stride one: the pass a vector candidate
    /// can take.
    fn line(
        src: &[Self],
        dst: &mut [Self],
        len: usize,
        radius: usize,
        recip: Reciprocal,
        out: Range<usize>,
    ) {
        blur_span::<Self, Self::Window>(src, dst, 1, len, radius, recip, out);
    }
}

impl Sample for Pixel {
    type Window = PixelSum;

    fn line(
        src: &[Self],
        dst: &mut [Self],
        len: usize,
        radius: usize,
        recip: Reciprocal,
        out: Range<usize>,
    ) {
        (crate::accel::blur_for(recip).line)(src, dst, len, radius, recip, out);
    }
}

impl Sample for u8 {
    type Window = CoverageSum;
}

/// Add `row` into the column sums `sums`, pairwise: the portable reference.
pub(crate) fn add_portable(sums: &mut [PixelSum], row: &[Pixel]) {
    for (sum, &pixel) in sums.iter_mut().zip(row) {
        sum.add(pixel);
    }
}

/// Move the column sums `sums` a row on, adding `entering` and removing
/// `leaving`, pairwise: the portable reference.
pub(crate) fn slide_portable(sums: &mut [PixelSum], entering: &[Pixel], leaving: &[Pixel]) {
    for ((sum, &enter), &leave) in sums.iter_mut().zip(entering).zip(leaving) {
        sum.add(enter);
        sum.sub(leave);
    }
}

/// Write each column sum's mean over `recip` to `out`, pairwise: the
/// portable reference.
pub(crate) fn mean_portable(out: &mut [Pixel], sums: &[PixelSum], recip: Reciprocal) {
    for (slot, sum) in out.iter_mut().zip(sums) {
        *slot = sum.mean(recip);
    }
}

/// The portable horizontal pass over pixels: the reference the vector
/// candidates are verified against, and the tail they hand a divisor they
/// have no lanes for.
pub(crate) fn line_portable(
    src: &[Pixel],
    dst: &mut [Pixel],
    len: usize,
    radius: usize,
    recip: Reciprocal,
    out: Range<usize>,
) {
    blur_span::<Pixel, PixelSum>(src, dst, 1, len, radius, recip, out);
}

/// The running sums of the samples currently inside a sliding window.
///
/// A `u32` per channel is ample: a channel is at most 255 and the window
/// holds at most one screen dimension's worth of samples, so the sum cannot
/// approach the type's range for any radius a surface is drawn at. Every
/// operation saturates so that a caller passing an absurd radius — the
/// entry points are public and take any `usize` — gets a flattened region
/// rather than an arithmetic panic.
pub(crate) trait Window<S>: Copy + Default {
    /// Add `times` copies of `sample` to the window.
    fn add_many(&mut self, sample: S, times: usize);
    /// Add one copy of `sample` to the window.
    fn add(&mut self, sample: S);
    /// Remove one copy of `sample` from the window.
    fn sub(&mut self, sample: S);
    /// The window's mean over `recip`'s divisor, rounded to nearest.
    fn mean(self, recip: Reciprocal) -> S;
}

/// One window's four channel sums: `#[repr(C)]`, so a vector candidate
/// reads a run of them as lanes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub(crate) struct PixelSum {
    pub(crate) r: u32,
    pub(crate) g: u32,
    pub(crate) b: u32,
    pub(crate) a: u32,
}

const _: () = assert!(core::mem::size_of::<PixelSum>() == 16);

impl Window<Pixel> for PixelSum {
    fn add_many(&mut self, pixel: Pixel, times: usize) {
        let times = u32::try_from(times).unwrap_or(u32::MAX);
        let weighted = |channel: u8| u32::from(channel).saturating_mul(times);
        self.r = self.r.saturating_add(weighted(pixel.r));
        self.g = self.g.saturating_add(weighted(pixel.g));
        self.b = self.b.saturating_add(weighted(pixel.b));
        self.a = self.a.saturating_add(weighted(pixel.a));
    }

    fn add(&mut self, pixel: Pixel) {
        self.r = self.r.saturating_add(u32::from(pixel.r));
        self.g = self.g.saturating_add(u32::from(pixel.g));
        self.b = self.b.saturating_add(u32::from(pixel.b));
        self.a = self.a.saturating_add(u32::from(pixel.a));
    }

    fn sub(&mut self, pixel: Pixel) {
        self.r = self.r.saturating_sub(u32::from(pixel.r));
        self.g = self.g.saturating_sub(u32::from(pixel.g));
        self.b = self.b.saturating_sub(u32::from(pixel.b));
        self.a = self.a.saturating_sub(u32::from(pixel.a));
    }

    fn mean(self, recip: Reciprocal) -> Pixel {
        Pixel {
            r: recip.apply(self.r),
            g: recip.apply(self.g),
            b: recip.apply(self.b),
            a: recip.apply(self.a),
        }
    }
}

#[derive(Copy, Clone, Default)]
struct CoverageSum(u32);

impl Window<u8> for CoverageSum {
    fn add_many(&mut self, level: u8, times: usize) {
        let times = u32::try_from(times).unwrap_or(u32::MAX);
        self.0 = self
            .0
            .saturating_add(u32::from(level).saturating_mul(times));
    }

    fn add(&mut self, level: u8) {
        self.0 = self.0.saturating_add(u32::from(level));
    }

    fn sub(&mut self, level: u8) {
        self.0 = self.0.saturating_sub(u32::from(level));
    }

    fn mean(self, recip: Reciprocal) -> u8 {
        recip.apply(self.0)
    }
}

/// The fractional bits the reciprocal multiply is computed with.
const RECIPROCAL_SHIFT: u32 = 40;

/// The largest window the reciprocal multiply is exactly equal to the divide
/// for, and therefore the largest it is used at.
const RECIPROCAL_MAX_COUNT: u32 = 65_536;

/// How one window's mean is divided by its sample count.
///
/// The count is `2 * radius + 1` for every output of a pass, so it is resolved
/// once for the pass instead of dividing four times per pixel per pass — which
/// was the dominant cost of a frosted window.
#[derive(Copy, Clone)]
pub(crate) enum Reciprocal {
    /// Multiply by a fixed-point reciprocal, which for every window the blur
    /// is used at gives *exactly* the same answer as the divide.
    ///
    /// The target is `floor((n + d/2) / d)`, so with `n` the rounded numerator
    /// and `m = ceil(2^S / d)` the claim is `floor(n*m / 2^S) == floor(n/d)`.
    /// Write `n = k*d + s` with `s <= d-1`, and `e = m*d - 2^S` (so `e <= d-1`
    /// by construction). Then `n*m/2^S = n/d + n*e/(d*2^S)`, and the two floors
    /// agree exactly while `s*2^S + n*e < d*2^S`; since `s <= d-1` it suffices
    /// that `n*e < 2^S`.
    ///
    /// A window holds exactly `d` samples of at most 255 each, so `n < 256*d`,
    /// and `(256*d - 1) * (d - 1) < 2^40` holds for every `d` up to 65536 —
    /// which is why that is the cutoff, and why the product stays under `2^48`.
    /// `blur_tests` checks the condition for every count in range, and that a
    /// count above it genuinely breaks the proof, rather than leaving either
    /// argued.
    ///
    /// The product saturates so the answer stays a total function of its
    /// argument: a numerator from outside a window of `d` samples — which no
    /// blur produces — reads as fully bright rather than overflowing.
    Multiply {
        m: u64,
        half: u32,
        #[cfg(raster_lanes)]
        lanes: LaneDivisor,
    },
    /// Divide, for a count past the range the multiply is exact over. No
    /// surface is drawn at such a radius; correctness does not depend on that,
    /// and its windows take the portable kernels, whose sums saturate.
    Divide { d: u32 },
}

/// The same rounded mean in a form 32-bit vector lanes can take:
/// `floor((sum + half) * m / 2^shift)` with `m` below `2^32`.
///
/// Exact for every sum a window of `d` samples can hold, by the argument
/// above with `S = 8 + 2b` and `d <= 2^b`: then `n < 256*d` and `e < d` give
/// `n*e < 256*d*d <= 2^S`, and `m <= 2^S/d + 1 < 2^(9+b)`, so `m` fits a lane
/// for every count up to [`RECIPROCAL_MAX_COUNT`] and the product fits 64
/// bits. `blur_tests` checks every count at every boundary of the quotient.
#[cfg(raster_lanes)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaneDivisor {
    pub(crate) m: u32,
    pub(crate) shift: u32,
    pub(crate) half: u32,
}

#[cfg(raster_lanes)]
impl LaneDivisor {
    /// The lane form of division by `d`, for `d` in `1..=RECIPROCAL_MAX_COUNT`;
    /// a count outside it is held to it rather than overflowing the shift.
    fn new(d: u32) -> Self {
        let d = d.clamp(1, RECIPROCAL_MAX_COUNT);
        let bits = u32::BITS - (d - 1).leading_zeros();
        let shift = 8 + 2 * bits;
        let m = (1u64 << shift).div_ceil(u64::from(d));
        Self {
            m: u32::try_from(m).unwrap_or(u32::MAX),
            shift,
            half: d / 2,
        }
    }

    /// One channel's rounded mean, as a lane computes it.
    #[cfg(test)]
    pub(crate) fn apply(self, sum: u32) -> u32 {
        let n = u64::from(sum.wrapping_add(self.half));
        u32::try_from((n * u64::from(self.m)) >> self.shift).unwrap_or(u32::MAX)
    }
}

impl Reciprocal {
    /// The divisor for a window of `count` samples. A count of zero would make
    /// no window, so it reads as one.
    pub(crate) fn new(count: usize) -> Self {
        let d = u32::try_from(count).unwrap_or(u32::MAX).max(1);
        if d <= RECIPROCAL_MAX_COUNT {
            let m = (1u64 << RECIPROCAL_SHIFT).div_ceil(u64::from(d));
            Self::Multiply {
                m,
                half: d / 2,
                #[cfg(raster_lanes)]
                lanes: LaneDivisor::new(d),
            }
        } else {
            Self::Divide { d }
        }
    }

    /// Whether every sum a window of this count holds — at most 255 for each
    /// of its samples — fits a 32-bit lane: true wherever the multiply is.
    pub(crate) const fn sums_fit_lanes(self) -> bool {
        matches!(self, Self::Multiply { .. })
    }

    /// The lane form of this division, where the count admits one.
    #[cfg(raster_lanes)]
    pub(crate) const fn lanes(self) -> Option<LaneDivisor> {
        match self {
            Self::Multiply { lanes, .. } => Some(lanes),
            Self::Divide { .. } => None,
        }
    }

    /// One channel's rounded mean, clamped to the channel range.
    #[inline]
    pub(crate) fn apply(self, sum: u32) -> u8 {
        let rounded = match self {
            Self::Multiply { m, half, .. } => {
                u64::from(sum.saturating_add(half)).saturating_mul(m) >> RECIPROCAL_SHIFT
            }
            Self::Divide { d } => u64::from(sum.saturating_add(d / 2)) / u64::from(d),
        };
        u8::try_from(rounded.min(255)).unwrap_or(u8::MAX)
    }
}

#[cfg(test)]
#[path = "blur_tests.rs"]
mod tests;
