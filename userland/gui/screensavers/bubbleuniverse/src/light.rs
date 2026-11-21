//! Where the light falls: every point drawn as a soft dot, gathered with its
//! glow, and shown through the tone curve a band of rows at a time.
//!
//! A frame's spots are sorted into the strips of [`STRIP_ROWS`] rows their
//! dots reach. Each band of a paint then draws a strip's dots into light of
//! its own and tones those rows straight away, while the strip is still in
//! cache, so the frame's light is never held whole and no two bands share a
//! byte. A strip's light is held a dot's width wider than its rows and
//! columns on every side, so a dot is drawn whole, its taps unrolled, and
//! what falls past the strip is never shown.

use alloc::vec::Vec;
use core::array::from_fn;
use core::ops::Range;

use tairix_geometry::Rect;
use tairix_parallel::JobRunner;
use tairix_raster::{DitherRow, Pixel, RowBand, Surface};
use tairix_util::fallible;

use crate::glow::Glow;
use crate::tone::Tone;

/// The fraction bits a spot's position is held in.
pub(crate) const SUB_SHIFT: u32 = 8;

/// One pixel, in a position's fraction.
pub(crate) const SUB: i32 = 1 << SUB_SHIFT;

/// A pixel's centre, from its top left corner.
const HALF: i32 = SUB / 2;

/// The widest a dot is drawn, in whole pixels either way of its centre: the
/// size an 8K screen asks for. A larger screen's dots are drawn no wider,
/// each still shedding the light its size is worth.
pub(crate) const MAX_RADIUS: u32 = 4;

/// The most pixels a dot covers along an axis.
const MAX_TAPS: usize = 2 * MAX_RADIUS as usize;

/// The rows a band draws at once: at least as many as the tallest dot
/// covers, so no dot reaches more than two strips.
const STRIP_ROWS: i32 = 16;

const _: () = assert!(
    STRIP_ROWS as usize >= MAX_TAPS,
    "a dot reaches at most two strips"
);

/// The fewest pixels worth handing another core to draw.
const PAINT_GRAIN: usize = 16_384;

/// One point of light as a frame draws it: where its dot is centred, in
/// 256ths of a pixel on the screen, and the light the dot spreads about it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Spot {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) light: [u16; 3],
}

/// The dot each point is drawn as: a tent `radius` whole pixels each way.
/// Its taps along an axis weigh `256·radius²` in all wherever its centre
/// falls between pixels, so a moving dot neither brightens nor dims.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dot {
    radius: u32,
}

impl Dot {
    /// The dot `radius` pixels each way, held to `1..=MAX_RADIUS`.
    pub(crate) fn new(radius: u32) -> Self {
        Self {
            radius: radius.clamp(1, MAX_RADIUS),
        }
    }

    /// Its radius in whole pixels.
    pub(crate) const fn radius(self) -> u32 {
        self.radius
    }

    /// The light it sheds over all the pixels it covers, as a multiple of its
    /// spot's: `radius⁴`, its two axes' weights over 256², of which its peak
    /// takes `radius²`.
    pub(crate) const fn energy(self) -> u64 {
        let r = self.radius as u64;
        r * r * r * r
    }

    /// The pixels along an axis it covers.
    const fn taps(self) -> i32 {
        2 * self.radius.cast_signed()
    }

    /// The first pixel along an axis it lights, centred `at`.
    #[inline]
    fn first(self, at: i32) -> i32 {
        ((at - HALF - self.radius.cast_signed() * SUB) >> SUB_SHIFT) + 1
    }
}

/// A tent `TAPS` pixels across, centred `at` along an axis: the first pixel
/// it lights, and its height at each from there, in 256ths of a pixel.
#[inline]
fn tent<const TAPS: usize>(at: i32) -> (i32, [u32; TAPS]) {
    let reach = i32::try_from(TAPS).unwrap_or(0) * HALF;
    let first = ((at - HALF - reach) >> SUB_SHIFT) + 1;
    let offset = first * SUB + HALF - at;
    let heights = from_fn(|tap| {
        let distance = (offset + i32::try_from(tap).unwrap_or(0) * SUB).abs();
        (reach - distance).max(0).unsigned_abs()
    });
    (first, heights)
}

/// Where a frame is drawn: its whole square about the bubble, and the part of
/// it on the screen, both in screen pixels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Placed {
    pub(crate) frame: Rect,
    pub(crate) painted: Rect,
}

/// How the painted rows fall into strips: the rows, how many strips they
/// make, and the dot whose rows decide which strips a spot reaches.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    top: i32,
    bottom: i32,
    count: usize,
    dot: Dot,
}

impl Layout {
    /// The strips the `painted` rows make for `dot`s, as many of them as
    /// `most` allows.
    fn new(painted: Rect, dot: Dot, most: usize) -> Self {
        let count = usize::try_from(painted.height.div_ceil(STRIP_ROWS.unsigned_abs()))
            .unwrap_or(0)
            .min(most);
        let rows = i32::try_from(count).unwrap_or(0) * STRIP_ROWS;
        Self {
            top: painted.top(),
            bottom: painted.bottom().min(painted.top() + rows),
            count,
            dot,
        }
    }

    /// The strips rows `rows` reach.
    #[inline]
    fn reached(self, rows: Range<i32>) -> Range<usize> {
        let (first, last) = (rows.start.max(self.top), rows.end.min(self.bottom));
        if first >= last {
            return 0..0;
        }
        let strip = |row: i32| usize::try_from((row - self.top) / STRIP_ROWS).unwrap_or(0);
        strip(first)..(strip(last - 1) + 1).min(self.count)
    }

    /// The strips `spot`'s dot reaches.
    #[inline]
    fn of(self, spot: &Spot) -> Range<usize> {
        let first = self.dot.first(spot.y);
        self.reached(first..first + self.dot.taps())
    }

    /// The painted rows strip `strip` holds.
    fn rows(self, strip: usize) -> Range<i32> {
        let top = self.top + i32::try_from(strip).unwrap_or(0) * STRIP_ROWS;
        top..(top + STRIP_ROWS).min(self.bottom)
    }
}

/// A frame's spots sorted by the strips they reach, a share for each job that
/// traced them: each job sorts its own, and a strip's spots are each job's in
/// turn, so no two jobs touch one byte and the order a strip draws its spots
/// in is the order they were traced in, whichever job ran first.
pub(crate) struct Bins {
    layout: Layout,
    /// The most strips a frame makes, and so the stride of `starts`.
    most: usize,
    /// For each job, where each strip's spots start in its share of `spots`,
    /// and after the last strip its end.
    starts: Vec<u32>,
    /// Room for each job's spots twice over, as a dot reaches at most two
    /// strips.
    spots: Vec<Spot>,
    /// The spots one job traces.
    per_job: usize,
}

/// One job's part of the [`Bins`]: where it sorts the spots it traced.
pub(crate) struct Share<'a> {
    layout: Layout,
    starts: &'a mut [u32],
    spots: &'a mut [Spot],
}

impl Bins {
    /// Bins for `jobs` jobs tracing `per_job` spots each, over frames `rows`
    /// pixels tall drawn as `dot`s; `None` when the heap will not hold them.
    pub(crate) fn new(jobs: usize, per_job: usize, rows: u32, dot: Dot) -> Option<Self> {
        let most = usize::try_from(rows.div_ceil(STRIP_ROWS.unsigned_abs())).ok()?;
        Some(Self {
            layout: Layout::new(Rect::EMPTY, dot, most),
            most,
            starts: fallible::filled(jobs.checked_mul(most + 1)?, 0)?,
            spots: fallible::filled(jobs.checked_mul(per_job)?.checked_mul(2)?, Spot::default())?,
            per_job,
        })
    }

    /// Lay the strips out over the `painted` rows, and hand each job its
    /// share to sort what it traces into.
    pub(crate) fn shares(&mut self, painted: Rect) -> impl Iterator<Item = Share<'_>> {
        self.layout = Layout::new(painted, self.layout.dot, self.most);
        let layout = self.layout;
        self.starts
            .chunks_exact_mut(self.most + 1)
            .zip(self.spots.chunks_exact_mut(2 * self.per_job))
            .map(move |(starts, spots)| Share {
                layout,
                starts,
                spots,
            })
    }

    /// The spots whose dots reach strip `strip`, job by job.
    fn spots(&self, strip: usize) -> impl Iterator<Item = &[Spot]> {
        self.starts
            .chunks_exact(self.most + 1)
            .zip(self.spots.chunks_exact(2 * self.per_job))
            .map(move |(starts, spots)| {
                let bound = |index: usize| {
                    starts
                        .get(index)
                        .map_or(0, |&at| usize::try_from(at).unwrap_or(0))
                };
                spots.get(bound(strip)..bound(strip + 1)).unwrap_or(&[])
            })
    }
}

impl Share<'_> {
    /// Sort `traced` by the strips their dots reach.
    pub(crate) fn sort(&mut self, traced: &[Spot]) {
        let layout = self.layout;
        let Some(starts) = self.starts.get_mut(..=layout.count) else {
            return;
        };
        starts.fill(0);
        for spot in traced {
            for strip in layout.of(spot) {
                if let Some(count) = starts.get_mut(strip) {
                    *count += 1;
                }
            }
        }
        // Each strip's end, and past the last strip the end of them all.
        let mut end = 0;
        for start in starts.iter_mut() {
            end += *start;
            *start = end;
        }
        // Placed from the back, so each strip keeps the order its spots were
        // traced in and its end is counted down to its start.
        for spot in traced.iter().rev() {
            for strip in layout.of(spot) {
                let Some(next) = starts.get_mut(strip) else {
                    continue;
                };
                let Some(at) = next.checked_sub(1) else {
                    continue;
                };
                *next = at;
                if let Some(slot) = self
                    .spots
                    .get_mut(usize::try_from(at).unwrap_or(usize::MAX))
                {
                    *slot = *spot;
                }
            }
        }
    }
}

/// The working memory one band of a paint draws in: a strip's light, and the
/// glow its rows lie between.
struct Scratch {
    light: Vec<[u16; 3]>,
    glow: GlowRows,
}

/// Two neighbouring rows of glow cells read out at each painted pixel, and
/// the row of cells they were read from.
struct GlowRows {
    /// One row of cells, as they are read out.
    cells: Vec<[u16; 3]>,
    rows: [Vec<[u16; 3]>; 2],
    /// The row of cells `rows[0]` holds, if they hold any yet.
    held: Option<usize>,
}

impl GlowRows {
    /// Hold `glow`'s cell rows `row` and the one after it, read out from
    /// frame column `first`, reading only a row not already held.
    fn hold(&mut self, glow: &Glow, row: usize, first: usize) {
        match self.held {
            Some(held) if held == row => return,
            Some(held) if held + 1 == row => {
                self.rows.swap(0, 1);
                glow.row(row + 1, first, &mut self.cells, &mut self.rows[1]);
            }
            _ => {
                glow.row(row, first, &mut self.cells, &mut self.rows[0]);
                glow.row(row + 1, first, &mut self.cells, &mut self.rows[1]);
            }
        }
        self.held = Some(row);
    }
}

/// A frame's light: the bins its spots are sorted into, its glow and its
/// tone, and where its dots are drawn.
pub(crate) struct Light {
    bins: Bins,
    tone: Tone,
    glow: Option<Glow>,
    /// One scratch for each band a paint may split into, and at least one.
    scratch: Vec<Scratch>,
}

impl Light {
    /// The light of frames `size` pixels whose spots `bins` sorts, shown
    /// through `tone` with `glow` if it shines, in no more bands than
    /// `runner` takes; `None` when the heap will not hold it.
    pub(crate) fn new(
        size: (u32, u32),
        bins: Bins,
        (tone, glow): (Tone, Option<Glow>),
        runner: &dyn JobRunner,
    ) -> Option<Self> {
        let width = usize::try_from(size.0).ok()?;
        let strips = usize::try_from(size.1.div_ceil(STRIP_ROWS.unsigned_abs())).ok()?;
        let (cells, glow_width) = glow.as_ref().map_or((0, 0), |glow| (glow.across(), width));
        let bands = tairix_parallel::bands(runner, strips, 1).max(1);
        // A strip is held its dots' width wider on every side.
        let pad = 2 * usize::try_from(bins.layout.dot.taps()).ok()?;
        let strip_rows = usize::try_from(STRIP_ROWS).ok()? + pad;
        let strip_light = (width + pad).checked_mul(strip_rows)?;
        let mut scratch = Vec::new();
        if !fallible::reserve(&mut scratch, bands) {
            return None;
        }
        for _ in 0..bands {
            scratch.push(Scratch {
                light: fallible::filled(strip_light, [0; 3])?,
                glow: GlowRows {
                    cells: fallible::filled(cells, [0; 3])?,
                    rows: [
                        fallible::filled(glow_width, [0; 3])?,
                        fallible::filled(glow_width, [0; 3])?,
                    ],
                    held: None,
                },
            });
        }
        Some(Self {
            bins,
            tone,
            glow,
            scratch,
        })
    }

    /// Lay the strips out over the `painted` rows, and hand each tracing job
    /// its share of the bins to sort what it traces into.
    pub(crate) fn shares(&mut self, painted: Rect) -> impl Iterator<Item = Share<'_>> {
        self.bins.shares(painted)
    }

    /// Draw the spots the jobs sorted where `placed` puts the frame, over
    /// `surface`, in bands of rows across `runner`: every painted pixel, lit
    /// or not. The glow, if it shines, is gathered from `glowing` alone, a
    /// share of the spots that stands for them all.
    pub(crate) fn paint(
        &mut self,
        glowing: &[Spot],
        placed: &Placed,
        surface: &mut Surface,
        runner: &dyn JobRunner,
    ) {
        let Self {
            bins,
            tone,
            glow,
            scratch,
        } = self;
        let (frame, painted) = (placed.frame, placed.painted);
        if let Some(glow) = glow {
            glow.clear();
            let origin = (frame.left() * SUB, frame.top() * SUB);
            for spot in glowing {
                let within = (spot.x - origin.0, spot.y - origin.1);
                if let (Ok(x), Ok(y)) = (u32::try_from(within.0), u32::try_from(within.1)) {
                    glow.gather((x, y), spot.light);
                }
            }
            glow.spread(runner);
        }
        let Some((left, top)) = painted.surface_origin() else {
            return;
        };
        let drawn = Drawn {
            tone,
            glow: glow.as_ref(),
            bins: &*bins,
            frame,
            painted,
        };
        let most = scratch.len();
        let mut free = scratch.iter_mut();
        surface.paint_bands(
            top..top.saturating_add(painted.height),
            runner,
            (most, PAINT_GRAIN),
            |band| (band, free.next()),
            &|(band, scratch): &mut (RowBand<'_>, Option<&mut Scratch>)| {
                if let Some(scratch) = scratch {
                    drawn.band(band, scratch, left);
                }
            },
        );
    }
}

/// Everything a band of a paint reads.
struct Drawn<'a> {
    tone: &'a Tone,
    glow: Option<&'a Glow>,
    bins: &'a Bins,
    frame: Rect,
    painted: Rect,
}

impl Drawn<'_> {
    /// Draw `band`, whose painted columns start at surface column `left`, in
    /// `scratch`, with the tent the dot's size makes.
    fn band(&self, band: &mut RowBand<'_>, scratch: &mut Scratch, left: u32) {
        match self.bins.layout.dot.radius() {
            1 => self.band_with::<2>(band, scratch, left),
            2 => self.band_with::<4>(band, scratch, left),
            3 => self.band_with::<6>(band, scratch, left),
            _ => self.band_with::<MAX_TAPS>(band, scratch, left),
        }
    }

    /// [`Self::band`] with dots `TAPS` pixels across: each strip's dots into
    /// its light, then its rows lit by the glow and toned.
    fn band_with<const TAPS: usize>(
        &self,
        band: &mut RowBand<'_>,
        scratch: &mut Scratch,
        left: u32,
    ) {
        let width = usize::try_from(self.painted.width).unwrap_or(0);
        let stride = width + 2 * TAPS;
        let pad = i32::try_from(TAPS).unwrap_or(0);
        let rows = band.rows();
        let (Ok(start), Ok(end)) = (i32::try_from(rows.start), i32::try_from(rows.end)) else {
            return;
        };
        let first = usize::try_from(frame_offset(left, self.frame.left())).unwrap_or(0);
        scratch.glow.held = None;
        for strip in self.bins.layout.reached(start..end) {
            let held = self.bins.layout.rows(strip);
            let rows = held.start.max(start)..held.end.min(end);
            let Some(light) = scratch.light.get_mut(..stride * (rows.len() + 2 * TAPS)) else {
                continue;
            };
            light.fill([0; 3]);
            let corner = (self.painted.left() - pad, rows.start - pad);
            for spots in self.bins.spots(strip) {
                for spot in spots {
                    splat::<TAPS>(spot, corner, stride, light);
                }
            }
            let shown = light.chunks_exact_mut(stride).skip(TAPS);
            for (y, light) in rows.zip(shown) {
                let (Ok(y), Some(light)) = (u32::try_from(y), light.get_mut(TAPS..TAPS + width))
                else {
                    continue;
                };
                if let Some(glow) = self.glow {
                    let between = glow.rows_about(frame_offset(y, self.frame.top()));
                    scratch.glow.hold(glow, between.cell(), first);
                    let [upper, lower] = &scratch.glow.rows;
                    brighten(light, (upper, lower), between.weight());
                }
                let Some((x, pixels)) = band.row_span_mut(y, left, self.painted.width) else {
                    continue;
                };
                let light = light
                    .get(usize::try_from(x.saturating_sub(left)).unwrap_or(0)..)
                    .unwrap_or(&[]);
                shade(pixels, light, self.tone, (DitherRow::at(y), x));
            }
        }
    }
}

/// Add to `light` the glow `weight` 256ths of the way from `upper` to
/// `lower`, channel by channel.
#[inline]
fn brighten(light: &mut [[u16; 3]], (upper, lower): (&[[u16; 3]], &[[u16; 3]]), weight: u32) {
    let (upper, lower) = (upper.as_flattened(), lower.as_flattened());
    for ((light, &upper), &lower) in light.as_flattened_mut().iter_mut().zip(upper).zip(lower) {
        let glow = (u32::from(upper) * (256 - weight) + u32::from(lower) * weight + 128) >> 8;
        *light = light.saturating_add(u16::try_from(glow).unwrap_or(u16::MAX));
    }
}

/// How far surface coordinate `at` lies past `origin`, a frame's edge it
/// never lies before.
fn frame_offset(at: u32, origin: i32) -> u32 {
    u32::try_from(i64::from(at) - i64::from(origin)).unwrap_or(0)
}

/// Draw `spot`'s tent, `TAPS` pixels each way, into `light`, `stride` pixels
/// a row from surface pixel `corner`; a dot that would reach past its rows'
/// ends is not drawn.
#[inline]
fn splat<const TAPS: usize>(
    spot: &Spot,
    corner: (i32, i32),
    stride: usize,
    light: &mut [[u16; 3]],
) {
    let (left, across) = tent::<TAPS>(spot.x);
    let (top, down) = tent::<TAPS>(spot.y);
    let (Ok(column), Ok(row)) = (
        usize::try_from(left - corner.0),
        usize::try_from(top - corner.1),
    ) else {
        return;
    };
    if column + TAPS > stride {
        return;
    }
    let shed = spot.light.map(u64::from);
    for (row, down) in (row..).zip(down) {
        let start = row * stride + column;
        let Some(cells) = light.get_mut(start..start + TAPS) else {
            return;
        };
        for (cell, across) in cells.iter_mut().zip(across) {
            let weight = u64::from(across * down);
            for (channel, shed) in cell.iter_mut().zip(shed) {
                let lit = (shed * weight + (1 << 15)) >> 16;
                *channel = channel.saturating_add(u16::try_from(lit).unwrap_or(u16::MAX));
            }
        }
    }
}

/// Tone `light` into `pixels`, the first at surface column `first`, dithered
/// by `dither`.
#[inline]
fn shade(pixels: &mut [Pixel], light: &[[u16; 3]], tone: &Tone, (dither, first): (DitherRow, u32)) {
    for ((pixel, light), x) in pixels.iter_mut().zip(light).zip(first..) {
        let bias = dither.bias(x);
        let level = |channel: usize| tone.level(light[channel], bias);
        *pixel = Pixel {
            r: level(0),
            g: level(1),
            b: level(2),
            a: u8::MAX,
        };
    }
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
