//! Turning an application's pixels into the bytes a **window** frame holds,
//! and back.
//!
//! A window channel carries an app's pixels in one `shm`-granted frame laid out
//! by a [`DisplayMode`], exactly as the display service's scan-out frame is:
//! **premultiplied**, in the channel order the mode names, so the frame holds
//! the very pixels the app composed and neither side divides. Both directions
//! live here, once, over the same [`ChannelOrder`] the scan-out path resolves,
//! so nothing anywhere holds a second opinion about which byte is red.
//!
//! * [`encode`] — the app's side: a [`Surface`] out into the frame it granted
//!   the session, a byte-order copy.
//! * [`decode`] — the desktop's side: a presented frame in to the compositor's
//!   own window surface, every colour channel clamped to its alpha because the
//!   bytes are the client's, reporting the sub-rectangle whose pixels the
//!   conversion actually *changed*.
//! * [`lay_opaque`] — the desktop's side for a picture that may not show what
//!   it covers, a screensaver's: every pixel laid opaque, at copy speed, and
//!   nothing compared, because the frame's own damage already bounds it.
//!
//! # Why only one side is given a real [`JobRunner`]
//!
//! Both directions are row-independent by construction — a row reads and writes
//! only its own pixels — so both are expressed over a runner. In practice the
//! desktop passes a real pool and an app passes [`tairix_parallel::SERIAL`], and
//! the asymmetry is principled rather than an oversight: an app decides how much
//! it presents and should present only what it changed, while the session must
//! accept whatever damage the app declares and cannot make it smaller.
//! Spreading the pass it cannot bound is what keeps one app's whole-window
//! repaint off the desktop's critical path.
//!
//! # Fail closed
//!
//! Every index either direction will use is validated **before** the first
//! write, so a malformed or hostile geometry refuses the whole conversion
//! rather than leaving half a window converted. A format with no channel order
//! here is refused, never guessed: guessing renders the window in false colour.
//!
//! A conversion covers the surface's full extent; an active clip
//! ([`Surface::with_clip`]) narrows which pixels it reaches, which is the
//! caller's own choice rather than something this module second-guesses.

use core::ops::Range;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayMode};
use tairix_abi::Errno;
use tairix_geometry::{Rect, Region};
use tairix_parallel::JobRunner;
use tairix_raster::{RowBand, Surface};

use crate::scanout::ChannelOrder;

/// Bytes one pixel occupies in a window frame. The two orders this crate knows
/// are both four-byte, and a mode claiming otherwise is refused.
const PIXEL_BYTES: usize = 4;

/// Pixels below which a conversion is not worth splitting across cores.
///
/// A band carrying fewer than this pays more in dispatch than it saves, so a
/// small repaint — the common case once an app clips its damage — runs on the
/// calling thread with no atomics and no syscall. Four times the compositor's
/// grain, because a pixel here is a copy and a clamp where a composited pixel
/// is a blend.
const MIN_PARALLEL_BAND_PX: usize = 64 * 1024;

/// The validated shape of one conversion: the resolved channel order, the row
/// stride, and the damage rectangle's own bounds.
///
/// Resolved once per call so neither direction repeats the arithmetic per row,
/// and so the whole request is refused before a pixel moves.
struct Shape {
    order: ChannelOrder,
    stride: usize,
    /// First surface row of the rectangle.
    top: u32,
    /// One past the last surface row.
    bottom: u32,
    /// First surface column of the rectangle.
    left: u32,
    /// Columns the rectangle spans.
    columns: usize,
    /// Byte offset of the rectangle's first column within a frame row.
    first_byte: usize,
    /// Bytes the rectangle spans within a frame row.
    row_bytes: usize,
}

impl Shape {
    /// Validate `damage` against `mode` and a surface of `size`, and against
    /// `frame_len` bytes of frame.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a format with no channel order, a pixel size
    /// other than four bytes, a rectangle reaching outside the surface, a row
    /// span wider than the stride, a frame too short for the rows the rectangle
    /// names, or any arithmetic that would overflow.
    fn resolve(
        mode: &DisplayMode,
        size: (u32, u32),
        damage: DamageRect,
        frame_len: usize,
    ) -> Result<Self, Errno> {
        let order = ChannelOrder::for_format(mode.format).ok_or(Errno::OutOfRange)?;
        if mode.format.bytes_per_pixel() as usize != PIXEL_BYTES {
            return Err(Errno::OutOfRange);
        }
        let right = damage
            .x
            .checked_add(damage.width_px)
            .ok_or(Errno::OutOfRange)?;
        let bottom = damage
            .y
            .checked_add(damage.height_px)
            .ok_or(Errno::OutOfRange)?;
        if right > size.0 || bottom > size.1 {
            return Err(Errno::OutOfRange);
        }
        let stride = mode.stride_bytes as usize;
        let columns = damage.width_px as usize;
        let first_byte = (damage.x as usize)
            .checked_mul(PIXEL_BYTES)
            .ok_or(Errno::OutOfRange)?;
        let row_bytes = columns.checked_mul(PIXEL_BYTES).ok_or(Errno::OutOfRange)?;
        if first_byte.checked_add(row_bytes).ok_or(Errno::OutOfRange)? > stride {
            return Err(Errno::OutOfRange);
        }
        // The band split chunks whole frame rows, so every row the rectangle
        // names has to be there. A window frame is `stride × height` and the
        // rectangle is inside the surface's height, so this holds for every
        // well-formed present and refuses every other.
        if (bottom as usize)
            .checked_mul(stride)
            .ok_or(Errno::OutOfRange)?
            > frame_len
        {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            order,
            stride,
            top: damage.y,
            bottom,
            left: damage.x,
            columns,
            first_byte,
            row_bytes,
        })
    }

    /// Rows the rectangle spans.
    fn rows(&self) -> usize {
        usize::try_from(self.bottom.saturating_sub(self.top)).unwrap_or(0)
    }

    /// Columns as the extent a row borrow is asked for.
    fn width(&self) -> u32 {
        u32::try_from(self.columns).unwrap_or(u32::MAX)
    }

    /// How many rows each band carries.
    ///
    /// The grain is a pixel budget expressed in this rectangle's own rows, so a
    /// narrow rectangle needs many rows to reach it and a wide one needs few —
    /// the rule the compositor splits a composite by.
    fn rows_per_band(&self, runner: &dyn JobRunner) -> u32 {
        let rows = self.rows();
        let count = tairix_parallel::bands(
            runner,
            rows,
            MIN_PARALLEL_BAND_PX.div_ceil(self.columns.max(1)),
        );
        tairix_raster::band_rows(rows, count)
    }

    /// Byte range of surface row `y`'s span, within a frame slice whose first
    /// row is `base_row`.
    fn frame_span(&self, base_row: u32, y: u32) -> Option<Range<usize>> {
        let local = usize::try_from(y.checked_sub(base_row)?).ok()?;
        let start = local
            .checked_mul(self.stride)?
            .checked_add(self.first_byte)?;
        Some(start..start.checked_add(self.row_bytes)?)
    }

    /// The frame bytes of the rows the rectangle names.
    fn region<'a>(&self, frame: &'a mut [u8]) -> Option<&'a mut [u8]> {
        let start = (self.top as usize).checked_mul(self.stride)?;
        let end = (self.bottom as usize).checked_mul(self.stride)?;
        frame.get_mut(start..end)
    }
}

/// The damage rectangle a client-space `rect` names, clipped to `mode`'s
/// surface, or `None` when nothing of it survives the clip.
///
/// An app that presents what it repainted holds that as a [`Rect`] in its own
/// coordinates — the very rectangles its controls reported — while the protocol
/// carries a [`DamageRect`] the session refuses outside the surface. Clipping is
/// therefore the app's own fail-closed step, with one definition here, beneath
/// every presenter, rather than a copy per app, and `None` says there is nothing to present at all.
#[must_use]
pub fn damage_in(mode: &DisplayMode, rect: Rect) -> Option<DamageRect> {
    let clipped = rect.intersection(&Rect::new(0, 0, mode.width_px, mode.height_px));
    let (x, y) = clipped.surface_origin()?;
    Some(DamageRect {
        x,
        y,
        width_px: clipped.width,
        height_px: clipped.height,
    })
}

/// Write `damage`'s pixels of `surface` into `frame` (laid out per `mode`) as
/// the premultiplied bytes a window frame holds.
///
/// This is what an app calls before it presents: it converts exactly the
/// rectangle it is about to declare, because that is the only rectangle the
/// session copies — everything outside it stays as the last frame left it.
///
/// # Errors
///
/// Any [`Errno`] the shape validation reports. Nothing is written on a refusal.
pub fn encode(
    surface: &Surface,
    frame: &mut [u8],
    mode: &DisplayMode,
    damage: DamageRect,
    runner: &dyn JobRunner,
) -> Result<(), Errno> {
    let shape = Shape::resolve(
        mode,
        (surface.width(), surface.height()),
        damage,
        frame.len(),
    )?;
    if shape.rows() == 0 || shape.columns == 0 {
        return Ok(());
    }
    let per_band = shape.rows_per_band(runner);
    let band_bytes = usize::try_from(per_band)
        .ok()
        .and_then(|rows| rows.checked_mul(shape.stride))
        .ok_or(Errno::OutOfRange)?;
    let top = shape.top;
    let region = shape.region(frame).ok_or(Errno::OutOfRange)?;
    let bands = region
        .chunks_mut(band_bytes)
        .enumerate()
        .map(|(index, bytes)| EncodeBand {
            base_row: top.saturating_add(
                u32::try_from(index)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(per_band),
            ),
            bytes,
        });
    tairix_parallel::for_each_drawn(runner, bands, &|mut band| {
        encode_band(&shape, surface, &mut band);
    });
    Ok(())
}

/// One band of an [`encode`]: the frame bytes of a contiguous run of rows, and
/// the first surface row they belong to.
struct EncodeBand<'a> {
    base_row: u32,
    bytes: &'a mut [u8],
}

/// Encode every row of one band.
fn encode_band(shape: &Shape, surface: &Surface, band: &mut EncodeBand<'_>) {
    let held = u32::try_from(band.bytes.len() / shape.stride.max(1)).unwrap_or(u32::MAX);
    let last = shape.bottom.min(band.base_row.saturating_add(held));
    for y in band.base_row..last {
        let (Some(span), Some((_, source))) = (
            shape.frame_span(band.base_row, y),
            surface.row_span(y, shape.left, shape.width()),
        ) else {
            continue;
        };
        let Some(target) = band.bytes.get_mut(span) else {
            continue;
        };
        let _ = shape.order.encode_run(source, target);
    }
}

/// Convert `damage`'s pixels of `frame` (laid out per `mode`) into `surface`,
/// returning the sub-rectangle of `damage` whose pixels the conversion actually
/// *changed* — [`Rect::EMPTY`] when the presented pixels were identical to the
/// ones already there.
///
/// The returned rectangle is the window's real damage, and it is what keeps a
/// repaint proportional to the change. An app generally cannot say which pixels
/// its own toolkit touched, so it presents whole-window damage; taking that
/// claim at face value would recomposite every pixel of the window for a hover
/// highlight a few rows tall. The comparison is exact — a pixel reported
/// unchanged carries the byte-identical value it already had — and costs one
/// extra read on a loop that already reads the frame and writes the surface.
///
/// # Errors
///
/// Any [`Errno`] the shape validation reports. Nothing is written on a refusal.
pub fn decode(
    frame: &[u8],
    surface: &mut Surface,
    mode: &DisplayMode,
    damage: DamageRect,
    runner: &dyn JobRunner,
) -> Result<Rect, Errno> {
    let shape = Shape::resolve(
        mode,
        (surface.width(), surface.height()),
        damage,
        frame.len(),
    )?;
    if shape.rows() == 0 || shape.columns == 0 {
        return Ok(Rect::EMPTY);
    }
    let per_band = shape.rows_per_band(runner);
    let bands = surface
        .row_bands_mut(shape.top..shape.bottom, per_band)
        .map(|rows| DecodeBand {
            rows,
            changed: Bounds::default(),
        });
    // The union of the bands' boxes: a merge that cannot depend on the order the
    // bands ran in, which is what makes the split invisible in the result.
    let changed = tairix_parallel::fold_drawn(
        runner,
        bands,
        Bounds::default(),
        &|mut band| {
            decode_band(&shape, frame, &mut band);
            band.changed
        },
        &|mut joined, band| {
            joined.merge(&band);
            joined
        },
    );
    Ok(changed.rect())
}

/// [`decode`] for every rectangle of `damage`, handing each changed
/// sub-rectangle to `changed`. Every rectangle's shape is checked before a
/// pixel is written, so a refusal writes nothing, and the list's rectangles are
/// disjoint, so no pixel is converted twice.
///
/// # Errors
///
/// Any [`Errno`] the shape validation reports for any of the rectangles.
pub fn decode_list(
    frame: &[u8],
    surface: &mut Surface,
    mode: &DisplayMode,
    damage: &DamageList,
    runner: &dyn JobRunner,
    mut changed: impl FnMut(Rect),
) -> Result<(), Errno> {
    let size = (surface.width(), surface.height());
    for rect in damage.rects() {
        Shape::resolve(mode, size, *rect, frame.len())?;
    }
    for rect in damage.rects() {
        changed(decode(frame, surface, mode, *rect, runner)?);
    }
    Ok(())
}

/// Lay `damage`'s pixels of `frame` (laid out per `mode`) into `surface`, every
/// one opaque whatever alpha it carries, across `runner`.
///
/// For a picture that may not show what it covers. Its damage is the frame's
/// own and bounds the change, so nothing is compared: the conversion is a
/// byte-order copy of every pixel named.
///
/// # Errors
///
/// Any [`Errno`] the shape validation reports. Nothing is written on a refusal.
pub fn lay_opaque(
    frame: &[u8],
    surface: &mut Surface,
    mode: &DisplayMode,
    damage: DamageRect,
    runner: &dyn JobRunner,
) -> Result<(), Errno> {
    let shape = Shape::resolve(
        mode,
        (surface.width(), surface.height()),
        damage,
        frame.len(),
    )?;
    if shape.rows() == 0 || shape.columns == 0 {
        return Ok(());
    }
    let per_band = shape.rows_per_band(runner);
    tairix_parallel::for_each_drawn(
        runner,
        surface.row_bands_mut(shape.top..shape.bottom, per_band),
        &|mut band| lay_band(&shape, frame, &mut band),
    );
    Ok(())
}

/// [`lay_opaque`] for every pixel of `damage` inside `mode`'s surface, each
/// rectangle checked before a pixel is written, so a refusal writes nothing.
/// The rows the region spans are laid in one pass of bands across `runner`,
/// each band laying its own rows of every rectangle that reaches it.
///
/// # Errors
///
/// Any [`Errno`] the shape validation reports for any of the rectangles.
pub fn lay_opaque_region(
    frame: &[u8],
    surface: &mut Surface,
    mode: &DisplayMode,
    damage: &Region,
    runner: &dyn JobRunner,
) -> Result<(), Errno> {
    let size = (surface.width(), surface.height());
    let Some(lay) = RegionLay::plan((mode, size, frame.len()), damage, runner, |_| Ok(()))? else {
        return Ok(());
    };
    tairix_parallel::for_each_drawn(
        runner,
        surface.row_bands_mut(lay.rows.clone(), lay.per_band),
        &|mut band| {
            for shape in lay.reaching(&band.rows()) {
                lay_band(&shape, frame, &mut band);
            }
        },
    );
    Ok(())
}

/// [`lay_opaque_region`] into `target` rather than a surface: a display's own
/// frame, `stride` bytes a row in `order`, so a picture covering the output
/// reaches the memory the display shows with no copy between. Every pixel is
/// opaque whatever alpha it carries, and the region's rows are laid in bands
/// across `runner`.
///
/// # Errors
///
/// Any [`Errno`] the shape validation reports for any of the rectangles, and
/// [`Errno::OutOfRange`] for one `target` cannot hold. Nothing is written on a
/// refusal.
pub fn lay_opaque_into(
    frame: &[u8],
    mode: &DisplayMode,
    (target, stride, order): (&mut [u8], usize, ChannelOrder),
    damage: &Region,
    runner: &dyn JobRunner,
) -> Result<(), Errno> {
    let size = (mode.width_px, mode.height_px);
    let held = target.len();
    let fits = |shape: &Shape| {
        let last_row = usize::try_from(shape.bottom.saturating_sub(1)).ok();
        let reach = last_row
            .and_then(|row| row.checked_mul(stride))
            .and_then(|start| start.checked_add(shape.first_byte))
            .and_then(|start| start.checked_add(shape.row_bytes));
        if shape.first_byte + shape.row_bytes > stride || reach.is_none_or(|reach| reach > held) {
            return Err(Errno::OutOfRange);
        }
        Ok(())
    };
    let Some(lay) = RegionLay::plan((mode, size, frame.len()), damage, runner, fits)? else {
        return Ok(());
    };
    let band_bytes = usize::try_from(lay.per_band)
        .ok()
        .and_then(|rows| rows.checked_mul(stride))
        .ok_or(Errno::OutOfRange)?;
    let from = usize::try_from(lay.rows.start)
        .ok()
        .and_then(|top| top.checked_mul(stride));
    let Some(region) = from.and_then(|from| target.get_mut(from..)) else {
        return Err(Errno::OutOfRange);
    };
    let bands = region.chunks_mut(band_bytes).zip(
        lay.rows
            .clone()
            .step_by(usize::try_from(lay.per_band).unwrap_or(usize::MAX)),
    );
    tairix_parallel::for_each_drawn(runner, bands, &|(bytes, first)| {
        let within = first..first.saturating_add(lay.per_band).min(lay.rows.end);
        for shape in lay.reaching(&within) {
            for y in shape.top.max(within.start)..shape.bottom.min(within.end) {
                let local = usize::try_from(y - first).unwrap_or(usize::MAX);
                let at = local
                    .saturating_mul(stride)
                    .saturating_add(shape.first_byte);
                let (Some(source), Some(row)) = (
                    shape.frame_span(0, y).and_then(|span| frame.get(span)),
                    bytes.get_mut(at..at.saturating_add(shape.row_bytes)),
                ) else {
                    continue;
                };
                let _ = order.transcode_opaque_run(shape.order, source, row);
            }
        }
    });
    Ok(())
}

/// A region's rectangles validated for one lay before any pixel is written:
/// the rows they span inside the surface, and how many of them a band takes.
struct RegionLay<'a> {
    damage: &'a Region,
    mode: &'a DisplayMode,
    size: (u32, u32),
    frame_len: usize,
    rows: Range<u32>,
    per_band: u32,
}

impl<'a> RegionLay<'a> {
    /// Resolve every rectangle of `damage` inside `mode`'s surface of `size`
    /// against `frame_len` bytes of frame, each also passing `fits`, and size
    /// the bands across `runner` by the pixels a row averages; `None` for a
    /// region naming no pixel.
    fn plan(
        (mode, size, frame_len): (&'a DisplayMode, (u32, u32), usize),
        damage: &'a Region,
        runner: &dyn JobRunner,
        fits: impl Fn(&Shape) -> Result<(), Errno>,
    ) -> Result<Option<Self>, Errno> {
        let (mut top, mut bottom, mut pixels) = (u32::MAX, 0, 0_usize);
        for rect in damage
            .rects()
            .iter()
            .filter_map(|rect| damage_in(mode, *rect))
        {
            let shape = Shape::resolve(mode, size, rect, frame_len)?;
            fits(&shape)?;
            top = top.min(shape.top);
            bottom = bottom.max(shape.bottom);
            pixels = pixels.saturating_add(shape.rows().saturating_mul(shape.columns));
        }
        let Some(span) = bottom.checked_sub(top).filter(|span| *span != 0) else {
            return Ok(None);
        };
        let rows = usize::try_from(span).unwrap_or(usize::MAX);
        let per_row = (pixels / rows).max(1);
        let count = tairix_parallel::bands(runner, rows, MIN_PARALLEL_BAND_PX.div_ceil(per_row));
        Ok(Some(Self {
            damage,
            mode,
            size,
            frame_len,
            rows: top..bottom,
            per_band: tairix_raster::band_rows(rows, count).max(1),
        }))
    }

    /// The shapes of the rectangles reaching `rows`: one run of them, the
    /// region being band-ordered.
    fn reaching(&self, rows: &Range<u32>) -> impl Iterator<Item = Shape> + '_ {
        let (start, end) = (i64::from(rows.start), i64::from(rows.end));
        let rects = self.damage.rects();
        let first = rects.partition_point(|rect| i64::from(rect.bottom()) <= start);
        let last = rects.partition_point(|rect| i64::from(rect.top()) < end);
        rects
            .get(first..last)
            .unwrap_or_default()
            .iter()
            .filter_map(|rect| damage_in(self.mode, *rect))
            .filter_map(|rect| Shape::resolve(self.mode, self.size, rect, self.frame_len).ok())
    }
}

/// Lay every row of one band opaque.
fn lay_band(shape: &Shape, frame: &[u8], band: &mut RowBand<'_>) {
    let rows = band.rows();
    for y in rows.start.max(shape.top)..rows.end.min(shape.bottom) {
        let (Some(span), Some((_, target))) = (
            shape.frame_span(0, y),
            band.row_span_mut(y, shape.left, shape.width()),
        ) else {
            continue;
        };
        if let Some(source) = frame.get(span) {
            let _ = shape.order.decode_opaque_run(source, target);
        }
    }
}

/// One band of a [`decode`]: the surface rows it owns, and the box of pixels it
/// changed within them.
struct DecodeBand<'a> {
    rows: RowBand<'a>,
    changed: Bounds,
}

/// Decode every row of one band, accumulating the pixels it changed.
fn decode_band(shape: &Shape, frame: &[u8], band: &mut DecodeBand<'_>) {
    let rows = band.rows.rows();
    for y in rows.start.max(shape.top)..rows.end.min(shape.bottom) {
        // Addressed from the frame's own first row: the frame is shared
        // read-only, so a band indexes it absolutely rather than through a
        // slice of its own.
        let (Some(span), Some((_, target))) = (
            shape.frame_span(0, y),
            band.rows.row_span_mut(y, shape.left, shape.width()),
        ) else {
            continue;
        };
        let Some(source) = frame.get(span) else {
            continue;
        };
        // One row address and one bounds check per row, not per pixel: this
        // loop runs over every damaged pixel of every application repaint.
        let (groups, _tail) = source.as_chunks::<PIXEL_BYTES>();
        for (index, (bytes, slot)) in groups.iter().zip(target).enumerate() {
            let pixel = shape.order.decode(*bytes);
            if *slot == pixel {
                continue;
            }
            *slot = pixel;
            if let Ok(step) = u32::try_from(index) {
                band.changed.include(shape.left.saturating_add(step), y);
            }
        }
    }
}

/// The bounding box of the pixels a conversion changed, accumulated as
/// inclusive edges so an untouched conversion stays distinguishable from one
/// that changed the single pixel at the origin.
#[derive(Copy, Clone, Default)]
struct Bounds {
    edges: Option<(u32, u32, u32, u32)>,
}

impl Bounds {
    /// Grow the box to cover the changed pixel `(x, y)`.
    fn include(&mut self, x: u32, y: u32) {
        self.edges = Some(match self.edges {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    }

    /// Grow the box to cover everything `other` covers.
    fn merge(&mut self, other: &Self) {
        let Some((x0, y0, x1, y1)) = other.edges else {
            return;
        };
        self.include(x0, y0);
        self.include(x1, y1);
    }

    /// The box as a rectangle, or [`Rect::EMPTY`] when nothing was changed.
    fn rect(&self) -> Rect {
        let Some((x0, y0, x1, y1)) = self.edges else {
            return Rect::EMPTY;
        };
        let (Ok(left), Ok(top)) = (i32::try_from(x0), i32::try_from(y0)) else {
            return Rect::EMPTY;
        };
        Rect::new(
            left,
            top,
            x1.saturating_sub(x0).saturating_add(1),
            y1.saturating_sub(y0).saturating_add(1),
        )
    }
}

#[cfg(test)]
#[path = "winframe_tests.rs"]
mod tests;
