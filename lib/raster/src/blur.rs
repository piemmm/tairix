//! The shared separable box blur.
//!
//! One definition serves every frosted surface: the compositor's backdrop
//! blur (a window's rectangle frosted before its own translucent pixels
//! blend over it) and a control's own soft highlight. A horizontal pass then
//! a vertical one, each carrying a running sum, so the cost is the region's
//! area rather than its area times the radius.
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
use crate::surface::{RowBand, Surface};

/// Blur `region` in place: a dense, row-major, premultiplied
/// `width`×`height` block of pixels, blurred by a separable box blur of
/// `radius` physical pixels using `aux` as the intermediate buffer.
///
/// `aux` is supplied by the caller — so a blur costs no allocation on the
/// frame path. A caller frosting a rectangle of a surface takes
/// [`Surface::frost_region`] or [`Surface::frost_from`] instead, which own the
/// shape-weighted mix back into it.
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
        blur_span(src, dst, 1, width, radius, recip, 0..width);
    }
    for x in 0..width {
        let Some((src, dst)) = column_pair(aux, region, x, count) else {
            return;
        };
        blur_span(src, dst, width, height, radius, recip, 0..height);
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
/// averages wait in before they are mixed back, and the band and piece lists.
/// [`Surface::frost_from`] reads its backdrop from a plane its caller holds, so
/// a caller that [reserves](Self::reserve) the scratch once frosts every frame
/// without allocating and is never refused. [`Surface::frost_region`] frosts in
/// place, so it also keeps here the copy of the backdrop it reads.
#[derive(Default)]
pub struct BlurScratch {
    plane: Option<Surface>,
    lines: Vec<Pixel>,
    sums: Vec<PixelSum>,
    strip: Vec<Pixel>,
    bands: Vec<Band>,
    pieces: Vec<Piece>,
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
        }
    }

    /// Reserve what a [`Surface::frost_from`] of up to `bands` bands of a
    /// rectangle no wider than `width` and no taller than `height` needs when
    /// `runner` spreads it, reporting whether it could.
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
        if let Some(plane) = self.plane.as_mut() {
            tairix_util::secret::wipe_with(plane.pixels_mut(), Pixel::TRANSPARENT);
        }
        tairix_util::secret::wipe_with(&mut self.lines, Pixel::TRANSPARENT);
        tairix_util::secret::wipe_with(&mut self.sums, PixelSum::default());
        tairix_util::secret::wipe_with(&mut self.strip, Pixel::TRANSPARENT);
    }

    /// Grow the scratch to what an in-place frost of a `width`×`height`
    /// rectangle needs on `runner`: its whole vertical pass in one strip.
    fn fit(&mut self, width: usize, height: usize, runner: &dyn JobRunner) -> bool {
        let (Some(lines), Some(strip)) = (
            width.checked_mul(runner.width().max(1)),
            width.checked_mul(height),
        ) else {
            return false;
        };
        let pieces = tairix_parallel::bands(runner, usize::MAX, 1);
        fallible::grow_to(&mut self.lines, lines, Pixel::TRANSPARENT)
            && fallible::grow_to(&mut self.sums, width, PixelSum::default())
            && fallible::grow_to(&mut self.strip, strip, Pixel::TRANSPARENT)
            && room(&mut self.pieces, pieces)
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
    /// pixels in nor mark a pixel outside. This is [`frost_from`] with the
    /// surface itself as the whole backdrop and a plane held in `scratch`, so
    /// the two cannot round, replicate, weight, or dither differently.
    ///
    /// The surface is left exactly as it was, never partly frosted, when
    /// `radius` is `0` (the effect is disabled), when the rectangle is empty
    /// or lands nowhere the surface admits, or when the scratch could not be
    /// grown.
    ///
    /// [`frost_from`]: Self::frost_from
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
        let (Ok(wide), Ok(tall)) = (usize::try_from(width), usize::try_from(height)) else {
            return;
        };
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
        let held = (columns.clone(), rows.clone());
        let frost = Frost::new(x, y, (columns.clone(), rows.clone()), &held, radius);
        let origin = (columns.start, rows.start);
        if scratch.fit(wide, tall, runner) {
            let BlurScratch {
                lines,
                sums,
                strip,
                bands,
                pieces,
                ..
            } = scratch;
            bands.clear();
            if fallible::reserve(bands, 1) {
                bands.push(Band {
                    cols: 0..wide,
                    rows: 0..tall,
                });
                let work = Work {
                    lines,
                    sums,
                    strip,
                    pieces,
                };
                let _ = frost.run(self, (&mut plane, origin), bands, work, runner, &coverage);
            }
        }
        scratch.plane = Some(plane);
    }

    /// Write the frost `frosting` describes into this surface, reading the
    /// backdrop from this surface within [`held`](Frosting::held) and from
    /// `plane` — addressed in this surface's coordinates — elsewhere, and report
    /// whether it was written.
    ///
    /// Each band is written exactly as [`frost_region`](Self::frost_region)
    /// writes it frosting the whole rectangle: samples replicate at the
    /// rectangle's edges and coverage is read at its own coordinates, so bands
    /// side by side meet without a seam. Nothing outside the bands is written,
    /// and the backdrop is read only within `radius` of them; this surface must
    /// hold it over the bands themselves, which a partial coverage mixes over.
    /// The horizontal pass is written into `plane`, so what the plane holds
    /// there afterwards is unspecified.
    ///
    /// A scratch [reserved](BlurScratch::reserve) for the rectangle is used as
    /// it stands. `false` is a frost the scratch could not be grown for, and
    /// leaves this surface untouched; a radius of `0`, or bands covering
    /// nothing, frost nothing and answer `true`.
    pub fn frost_from(
        &mut self,
        plane: &mut Self,
        frosting: &Frosting<'_>,
        scratch: &mut BlurScratch,
        runner: &dyn JobRunner,
        coverage: impl Fn(u32, u32) -> u8 + Sync,
    ) -> bool {
        let Frosting {
            rect: (x, y, w, h),
            ref held,
            bands,
            radius,
        } = *frosting;
        if radius == 0 {
            return true;
        }
        let Some(admitted) = self.admitted(x, y, w, h) else {
            return true;
        };
        let frost = Frost::new(x, y, admitted, held, radius);
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
            let Some(band) = frost.local(cols, rows) else {
                continue;
            };
            if !fallible::reserve(written, 1) {
                return false;
            }
            columns = columns.saturating_add(band.cols.len());
            written.push(band);
        }
        if written.is_empty() {
            return true;
        }
        let width = frost.width();
        if !(fallible::grow_to(lines, width, Pixel::TRANSPARENT)
            && fallible::grow_to(sums, columns, PixelSum::default())
            && fallible::grow_to(strip, width, Pixel::TRANSPARENT))
        {
            return false;
        }
        let work = Work {
            lines,
            sums,
            strip,
            pieces,
        };
        frost.run(self, (plane, (0, 0)), written, work, runner, &coverage)
    }
}

/// The frost a [`Surface::frost_from`] writes, in the destination's
/// coordinates.
pub struct Frosting<'a> {
    /// The rectangle `(x, y, w, h)` the blur is a function of: samples
    /// replicate at its edges and coverage is read from its top-left.
    pub rect: (u32, u32, u32, u32),
    /// The columns and rows of the rectangle where the destination already
    /// holds the backdrop; the plane holds it everywhere else the frost reads.
    pub held: (Range<u32>, Range<u32>),
    /// The parts of the rectangle written, which must not overlap.
    pub bands: &'a [(Range<u32>, Range<u32>)],
    /// How far the blur spreads, in pixels.
    pub radius: u32,
}

/// The rectangle a frost is a function of: the destination columns and rows
/// it admits, the origin its shape coverage is read from, and the blur it
/// spreads by.
///
/// Replication and coverage are read from *this* rectangle whichever band of
/// it a call writes, which is what makes frosting a border equal frosting the
/// whole and keeping the middle.
struct Frost {
    x: u32,
    y: u32,
    columns: Range<u32>,
    rows: Range<u32>,
    /// Where the destination holds the backdrop, in the rectangle's own
    /// columns and rows.
    held: Band,
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

/// The scratch buffers one frost works in.
struct Work<'a> {
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
    fn new(
        x: u32,
        y: u32,
        (columns, rows): (Range<u32>, Range<u32>),
        held: &(Range<u32>, Range<u32>),
        radius: u32,
    ) -> Self {
        let radius = usize::try_from(radius).unwrap_or(usize::MAX);
        let mut frost = Self {
            x,
            y,
            columns,
            rows,
            held: Band {
                cols: 0..0,
                rows: 0..0,
            },
            radius,
            recip: Reciprocal::new(radius.saturating_mul(2).saturating_add(1)),
        };
        if let Some(band) = frost.local(&held.0, &held.1) {
            frost.held = band;
        }
        frost
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
            let start = asked.start.clamp(within.start, within.end);
            let end = asked.end.clamp(start, within.end);
            let offset = |at: u32| usize::try_from(at.saturating_sub(within.start)).unwrap_or(0);
            offset(start)..offset(end)
        };
        let band = Band {
            cols: confine(cols, &self.columns),
            rows: confine(rows, &self.rows),
        };
        (!band.is_empty()).then_some(band)
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

    /// Frost `bands` of `dest` from the backdrop `dest` and `plane` hold
    /// between them, reporting whether the scratch could hold the frost.
    ///
    /// The horizontal pass is written into the plane, which keeps the scratch
    /// free of anything the size of the rectangle. The vertical pass runs a
    /// strip at a time, each piece sliding a running sum per column down its
    /// band, primed once; a strip is mixed into `dest` once all of it is
    /// written, because its pieces are split by column and its mix by row.
    fn run(
        &self,
        dest: &mut Surface,
        (plane, origin): Plane<'_>,
        bands: &[Band],
        work: Work<'_>,
        runner: &dyn JobRunner,
        coverage: &(impl Fn(u32, u32) -> u8 + Sync),
    ) -> bool {
        let width = self.width();
        let Some(rows) = bands
            .iter()
            .map(|band| band.rows.clone())
            .reduce(|a, b| a.start.min(b.start)..a.end.max(b.end))
        else {
            return true;
        };
        let Work {
            lines,
            sums,
            strip,
            pieces,
        } = work;
        if width == 0 || lines.len() < width || strip.len() < width {
            return false;
        }
        let read = PlaneRead {
            frost: self,
            origin,
        };
        read.blur_lines(plane, bands, lines, runner, dest);
        let Some(columns) = divide(bands, pieces, runner) else {
            return false;
        };
        let Some(sums) = sums.get_mut(..columns) else {
            return false;
        };
        let plane: &Surface = plane;
        let per = strip.len() / width;
        let mut start = rows.start;
        while start < rows.end {
            let mut end = start.saturating_add(per).min(rows.end);
            // Disjoint bands never need more than the strip's rows times the
            // rectangle's width; bands that overlap are given fewer rows.
            while strip_need(pieces, &(start..end)) > strip.len() && end > start.saturating_add(1) {
                end = start.saturating_add((end - start) / 2);
            }
            if strip_need(pieces, &(start..end)) <= strip.len() {
                read.average_strip(plane, pieces, sums, strip, &(start..end), runner);
                self.mix_strip(dest, pieces, strip, &(start..end), runner, coverage);
            }
            start = end;
        }
        true
    }

    /// Mix every piece's averages for `rows` back over `dest`, split into
    /// bands of whole destination rows across `runner`.
    fn mix_strip(
        &self,
        dest: &mut Surface,
        pieces: &[Piece],
        strip: &[Pixel],
        rows: &Range<usize>,
        runner: &dyn JobRunner,
        coverage: &(impl Fn(u32, u32) -> u8 + Sync),
    ) {
        let count = tairix_parallel::bands(
            runner,
            rows.len(),
            MIN_PARALLEL_FROST_PX.div_ceil(self.width().max(1)),
        );
        let per = u32::try_from(rows.len().div_ceil(count.max(1)))
            .unwrap_or(u32::MAX)
            .max(1);
        let mut bands = dest.row_bands_mut(self.row(rows.start)..self.row(rows.end), per);
        if count <= 1 {
            if let Some(mut only) = bands.next() {
                self.mix_rows(&mut only, pieces, strip, rows, coverage);
            }
            return;
        }
        let mut split: Vec<RowBand<'_>> = Vec::new();
        if !fallible::reserve(&mut split, count) {
            // Without room for every band the mix would be partial; run the
            // rows serially instead, which needs no split at all.
            for mut band in bands {
                self.mix_rows(&mut band, pieces, strip, rows, coverage);
            }
            return;
        }
        split.extend(bands);
        tairix_parallel::for_each(runner, &mut split, &|band| {
            self.mix_rows(band, pieces, strip, rows, coverage);
        });
    }

    /// Mix back, over the destination rows `band` owns, what every piece
    /// covering them averaged there in the strip over `rows` — each pixel
    /// weighted by its own `coverage` and rounded at the surface's ordered
    /// dither.
    ///
    /// The blocks lie in the strip in piece order, each a piece's columns wide
    /// and as tall as the rows of the strip it covers, which is how
    /// [`average_strip`](PlaneRead::average_strip) laid them out.
    fn mix_rows(
        &self,
        band: &mut RowBand<'_>,
        pieces: &[Piece],
        strip: &[Pixel],
        rows: &Range<usize>,
        coverage: &impl Fn(u32, u32) -> u8,
    ) {
        for row in band.rows() {
            let Some(local) = row
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
                    let at = offset.saturating_add((local - covered.start).saturating_mul(width));
                    if let Some(averaged) = strip.get(at..at.saturating_add(width)) {
                        self.mix_span(band, row, piece, averaged, coverage);
                    }
                }
                offset = offset.saturating_add(width.saturating_mul(covered.len()));
            }
        }
    }

    /// Mix one piece's averages for destination row `row` over that row of
    /// `band`.
    fn mix_span(
        &self,
        band: &mut RowBand<'_>,
        row: u32,
        piece: &Piece,
        averaged: &[Pixel],
        coverage: &impl Fn(u32, u32) -> u8,
    ) {
        let asked = self.column(piece.cols.start);
        let wide = u32::try_from(piece.cols.len()).unwrap_or(u32::MAX);
        let Some((first, target)) = band.row_span_mut(row, asked, wide) else {
            return;
        };
        // A clip window that cut the span's leading columns advances the
        // averages by as much, so the two stay aligned.
        let skip = usize::try_from(first.saturating_sub(asked)).unwrap_or(usize::MAX);
        let (ly, dither) = (row.saturating_sub(self.y), DitherRow::at(row));
        for (((dst, &src), lx), column) in target
            .iter_mut()
            .zip(averaged.get(skip..).unwrap_or_default())
            .zip(first.saturating_sub(self.x)..)
            .zip(first..)
        {
            *dst = mix(*dst, src, coverage(lx, ly), dither.bias(column));
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
    /// read one another's columns.
    fn blur_lines(
        &self,
        plane: &mut Surface,
        bands: &[Band],
        lines: &mut [Pixel],
        runner: &dyn JobRunner,
        dest: &Surface,
    ) {
        let width = self.frost.width().max(1);
        let Some(span) = bands
            .iter()
            .map(|band| self.frost.pass_rows(band))
            .reduce(|a, b| a.start.min(b.start)..a.end.max(b.end))
        else {
            return;
        };
        let count =
            tairix_parallel::bands(runner, span.len(), MIN_PARALLEL_FROST_PX.div_ceil(width))
                .clamp(1, (lines.len() / width).max(1));
        let per = u32::try_from(span.len().div_ceil(count))
            .unwrap_or(u32::MAX)
            .max(1);
        let mut work = plane
            .row_bands_mut(self.row(span.start)..self.row(span.end), per)
            .zip(lines.chunks_exact_mut(width));
        if count <= 1 {
            if let Some((mut band, line)) = work.next() {
                self.blur_band_lines(&mut band, line, bands, dest);
            }
            return;
        }
        let mut split: Vec<(RowBand<'_>, &mut [Pixel])> = Vec::new();
        if !fallible::reserve(&mut split, count) {
            for (mut band, line) in work {
                self.blur_band_lines(&mut band, line, bands, dest);
            }
            return;
        }
        split.extend(work);
        tairix_parallel::for_each(runner, &mut split, &|(band, line)| {
            self.blur_band_lines(band, line, bands, dest);
        });
    }

    /// The horizontal pass over the plane rows `rows` owns, a row at a time:
    /// each band's averages written into the plane over the band's columns.
    ///
    /// A row the destination holds the whole of what its averages read is
    /// averaged straight from the destination. Any other row's backdrop is
    /// gathered into `line` first — the plane's own, with what the destination
    /// holds laid over it — since its averages are written into the very row of
    /// the plane they read.
    fn blur_band_lines(
        &self,
        rows: &mut RowBand<'_>,
        line: &mut [Pixel],
        bands: &[Band],
        dest: &Surface,
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
            let Some(read) = reading()
                .map(|band| frost.reach(&band.cols))
                .reduce(|a, b| a.start.min(b.start)..a.end.max(b.end))
            else {
                continue;
            };
            let Some((start, target)) = rows.row_span_mut(plane_row, first, span) else {
                continue;
            };
            if start != first || target.len() != width {
                continue;
            }
            let held = if frost.held.rows.contains(&local) {
                overlap(&frost.held.cols, &read)
            } else {
                0..0
            };
            let source = if held == read {
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
                if !held.is_empty() {
                    let wide = u32::try_from(held.len()).unwrap_or(u32::MAX);
                    let (Some(gathered), Some((_, backdrop))) = (
                        line.get_mut(held.clone()),
                        dest.row_span(frost.row(local), frost.column(held.start), wide),
                    ) else {
                        continue;
                    };
                    if gathered.len() != backdrop.len() {
                        continue;
                    }
                    gathered.copy_from_slice(backdrop);
                }
                &*line
            };
            for band in reading() {
                if let Some(out) = target.get_mut(band.cols.clone()) {
                    blur_span(
                        source,
                        out,
                        1,
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
        let mut jobs: Vec<Averaging<'_>> = Vec::new();
        let spread = fallible::reserve(&mut jobs, pieces.len());
        let (mut sums, mut strip) = (sums, strip);
        for piece in pieces {
            let all_sums = core::mem::take(&mut sums);
            let (held, rest) = all_sums.split_at_mut(piece.cols.len().min(all_sums.len()));
            sums = rest;
            let covered = overlap(&piece.rows, rows);
            if covered.is_empty() {
                continue;
            }
            let all_strip = core::mem::take(&mut strip);
            let need = piece
                .cols
                .len()
                .saturating_mul(covered.len())
                .min(all_strip.len());
            let (block, rest) = all_strip.split_at_mut(need);
            strip = rest;
            let mut job = Averaging {
                piece,
                rows: covered,
                sums: held,
                block,
            };
            if spread {
                jobs.push(job);
            } else {
                self.average(plane, &mut job);
            }
        }
        tairix_parallel::for_each(runner, &mut jobs, &|job| self.average(plane, job));
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
        for (row, out) in rows.clone().zip(block.chunks_exact_mut(width)) {
            if row == piece.rows.start {
                self.prime(plane, piece, row, sums);
            }
            for (slot, sum) in out.iter_mut().zip(sums.iter()) {
                *slot = sum.mean(self.frost.recip);
            }
            if row.saturating_add(1) < piece.rows.end {
                self.slide(plane, piece, row, sums);
            }
        }
    }

    /// Set `sums` to the vertical window around the rectangle's own row
    /// `row`: `2 * radius + 1` rows, any past the rectangle's edges counted as
    /// copies of its first or last row — arithmetically, as `blur_span` counts
    /// them, so priming costs the rectangle's height at most whatever the
    /// radius.
    fn prime(&self, plane: &Surface, piece: &Piece, row: usize, sums: &mut [PixelSum]) {
        sums.fill(PixelSum::default());
        let Some(last) = self.frost.height().checked_sub(1) else {
            return;
        };
        let radius = self.frost.radius;
        self.gather(plane, piece, 0, sums, radius.saturating_sub(row));
        for sample in row.saturating_sub(radius)..=row.saturating_add(radius).min(last) {
            self.gather(plane, piece, sample, sums, 1);
        }
        let past = row.saturating_add(radius).saturating_sub(last);
        self.gather(plane, piece, last, sums, past);
    }

    /// Add `times` copies of the averages the rectangle's own row `row` holds
    /// over `piece`'s columns to `sums`.
    fn gather(
        &self,
        plane: &Surface,
        piece: &Piece,
        row: usize,
        sums: &mut [PixelSum],
        times: usize,
    ) {
        if times == 0 {
            return;
        }
        let Some(averaged) = self.averaged(plane, piece, row) else {
            return;
        };
        if times == 1 {
            for (sum, &pixel) in sums.iter_mut().zip(averaged) {
                sum.add(pixel);
            }
        } else {
            for (sum, &pixel) in sums.iter_mut().zip(averaged) {
                sum.add_many(pixel, times);
            }
        }
    }

    /// Move `sums` from the window around the rectangle's own row `row` to
    /// the one around the row after it.
    fn slide(&self, plane: &Surface, piece: &Piece, row: usize, sums: &mut [PixelSum]) {
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
        for ((sum, &enter), &leave) in sums.iter_mut().zip(added).zip(removed) {
            sum.add(enter);
            sum.sub(leave);
        }
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
        let per = band.cols.len().div_ceil(count).max(1);
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
fn blur_span<S: Sample>(
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
    let mut sum = S::Window::default();
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
}

impl Sample for Pixel {
    type Window = PixelSum;
}

impl Sample for u8 {
    type Window = CoverageSum;
}

/// The running sums of the samples currently inside a sliding window.
///
/// A `u32` per channel is ample: a channel is at most 255 and the window
/// holds at most one screen dimension's worth of samples, so the sum cannot
/// approach the type's range for any radius a surface is drawn at. Every
/// operation saturates so that a caller passing an absurd radius — the
/// entry points are public and take any `usize` — gets a flattened region
/// rather than an arithmetic panic.
trait Window<S>: Copy + Default {
    /// Add `times` copies of `sample` to the window.
    fn add_many(&mut self, sample: S, times: usize);
    /// Add one copy of `sample` to the window.
    fn add(&mut self, sample: S);
    /// Remove one copy of `sample` from the window.
    fn sub(&mut self, sample: S);
    /// The window's mean over `recip`'s divisor, rounded to nearest.
    fn mean(self, recip: Reciprocal) -> S;
}

#[derive(Copy, Clone, Default)]
struct PixelSum {
    r: u32,
    g: u32,
    b: u32,
    a: u32,
}

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
enum Reciprocal {
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
    Multiply { m: u64, half: u32 },
    /// Divide, for a count past the range the multiply is exact over. No
    /// surface is drawn at such a radius; correctness does not depend on that.
    Divide { d: u32 },
}

impl Reciprocal {
    /// The divisor for a window of `count` samples. A count of zero would make
    /// no window, so it reads as one.
    fn new(count: usize) -> Self {
        let d = u32::try_from(count).unwrap_or(u32::MAX).max(1);
        if d <= RECIPROCAL_MAX_COUNT {
            let m = (1u64 << RECIPROCAL_SHIFT).div_ceil(u64::from(d));
            Self::Multiply { m, half: d / 2 }
        } else {
            Self::Divide { d }
        }
    }

    /// One channel's rounded mean, clamped to the channel range.
    #[inline]
    fn apply(self, sum: u32) -> u8 {
        let rounded = match self {
            Self::Multiply { m, half } => {
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
