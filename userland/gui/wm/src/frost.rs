//! The frosted backdrop of a backdrop-blurred window, and the bounded,
//! pressure-governed cache that retains its blur between frames.
//!
//! # What a frost is
//!
//! A frosted window's pixels are blended over its frost: the windows beneath
//! it composed as though none of them were frosted, blurred at half resolution
//! on cells anchored at the screen's top-left with samples taken past the
//! window's edges as far as the screen's own, and mixed back by the window's
//! shape — the blur wherever the shape covers, the backdrop outside its
//! corners. The blur at a screen pixel is therefore a function of
//! the windows beneath and the radius alone: not of where the window lies, not
//! of its shape, and not of any other window's frost.
//!
//! # What is retained
//!
//! The blur itself, before the mix: a [`FrostField`] over a screen rectangle
//! holding the window's, and the part of it that is stale. A frost is laid
//! from the field — copied where the shape covers fully, mixed at the corners
//! over what was composed beneath them — so a window moving inside its field,
//! or changing its shape, blurs nothing. A change beneath stales the field
//! within the frost's reach of the change, and only that much is blurred
//! again.
//!
//! A window's first move gives its field a margin of the blur's radius about
//! its rectangle, and a move past it shifts the field, keeping what it still
//! holds, so a drag blurs a slab each time it crosses the margin rather than
//! its border every sample.

use core::mem::size_of;
use core::ops::Range;

use alloc::vec::Vec;

use tairix_hash::BuildFastHash;
use tairix_log::Sink;
use tairix_parallel::JobRunner;
use tairix_raster::{BlurScratch, Blurring};
use tairix_reclaim::{stacked_ui_cache, CachedBytes, PressureGauge, ReclaimCache};

use crate::color::Pixel;
use crate::geometry::{Rect, Region};
use crate::surface::{self, Surface};
use crate::window::{WindowId, WindowShape};

/// The most separate rectangles one frost blurs or is seen through at once:
/// the slabs a shifted field enters, or the parts of a damaged rectangle the
/// frosts above leave seen. More are taken as the rectangle bounding them.
pub(crate) const FROST_BANDS: usize = 8;

/// How far a moved window's field reaches past its rectangle on every side:
/// the blur's radius. The ring a drag's first move blurs is then about the
/// border every sample blurred when a frost replicated at its window's edges,
/// and a slab entered later costs no more.
const fn margin(radius_px: u32) -> u32 {
    radius_px
}

/// Worst-case per-entry bookkeeping the cache charges on top of a field's
/// payload: the LRU/index tick and charged-size fields (`u64` + `usize`), this
/// cache's small share of its two `BTreeMap`s' node overhead, and the
/// [`FrostField`] header held inline. The [`WindowId`] key is one `u64`,
/// already covered here.
const ENTRY_METADATA_BYTES: usize = 128;

/// What a field's stale region may hold on the heap: its two rectangle
/// buffers, each as long as combining a budget's worth of rectangles with one
/// more can briefly make it. Charged as part of every field, so the charge
/// stays fixed while the region changes.
const STALE_HEAP_BYTES: usize = 2 * (4 * (FROST_BANDS + 2)).next_power_of_two() * size_of::<Rect>();

/// The epoch a retained [`FrostField`] is valid for: this output's scale (in
/// percent) paired with the screen extent fields are blurred within.
///
/// The scale is caught per entry through the physical radius, so the epoch is
/// not what keeps a stale blur off the screen. It is what stops a superseded
/// one *staying charged*: a density or mode change invalidates every window's
/// field at once, and releasing them together gives the budget back to the
/// fields that are still live.
///
/// The theme is deliberately absent, unlike [`ChromeEpoch`](crate::ChromeEpoch).
/// A palette change repaints the layers beneath and marks them, which stales
/// the fields that read them.
pub type FrostEpoch = (u32, u32, u32);

/// Build the one [`ReclaimCache`] a [`Compositor`](crate::Compositor) retains
/// frost fields in, classified through the shared desktop cache policy
/// (`tairix_reclaim::stacked_ui_cache`).
///
/// `seat` is the seat the output belongs to, `fb_bytes` the real output's
/// backing byte size, and `memory_bytes` the machine's physical memory (`0`
/// where it cannot be read). A field is a whole window's rectangle or more,
/// and overlapping frosted windows want a field each over the same pixels, so
/// the ceiling is the machine's share of its memory, never below one
/// screenful, and pressure takes it back to one screenful and then to the
/// reserve. A stack deeper than that is retained from the front until the
/// ceiling runs out, and the rest blur what each frame needs of their frosts
/// (`Compositor::grant_backdrops`). `pressure` and `sink` are the process's
/// live pressure gauge and audit sink. The embedder — the only party that
/// knows all of these — calls this once and hands the result to
/// [`Compositor::new`](crate::Compositor::new).
#[must_use]
pub fn frost_cache(
    seat: u64,
    fb_bytes: usize,
    memory_bytes: usize,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
) -> ReclaimCache<WindowId, FrostField, FrostEpoch, BuildFastHash> {
    stacked_ui_cache(
        "wm.frost",
        seat,
        fb_bytes,
        memory_bytes,
        ENTRY_METADATA_BYTES,
        pressure,
        sink,
        // Keyed by an identifier this compositor assigned itself, so the
        // fast unkeyed hash is correct and is named here.
        BuildFastHash::new(),
    )
}

/// The bytes a field over `rect` charges the cache: its pixels and its stale
/// region's allowance, which is what a [`FrostField`] owns and therefore what
/// the cache weighs.
///
/// Read before any field is built, so a frame can choose which windows retain
/// one against what the budget will actually hold.
pub(crate) fn field_bytes(rect: Rect) -> usize {
    let area = usize::try_from(rect.width)
        .ok()
        .zip(usize::try_from(rect.height).ok())
        .and_then(|(w, h)| w.checked_mul(h));
    area.and_then(|area| area.checked_mul(size_of::<Pixel>()))
        .and_then(|pixels| pixels.checked_add(STALE_HEAP_BYTES))
        .unwrap_or(usize::MAX)
}

/// Whether `outer` holds every pixel of `inner`.
fn holds_rect(outer: Rect, inner: Rect) -> bool {
    inner.is_empty() || outer.intersection(&inner) == inner
}

/// The disjoint parts of `rect` outside `cut`: at most a band above, one below,
/// and the two sides between.
fn outside(rect: Rect, cut: Rect) -> [Rect; 4] {
    let cut = rect.intersection(&cut);
    if cut.is_empty() {
        return [rect, Rect::EMPTY, Rect::EMPTY, Rect::EMPTY];
    }
    let span = |from: i32, to: i32| u32::try_from(to.saturating_sub(from)).unwrap_or(0);
    [
        Rect::new(
            rect.left(),
            rect.top(),
            rect.width,
            span(rect.top(), cut.top()),
        ),
        Rect::new(
            rect.left(),
            cut.bottom(),
            rect.width,
            span(cut.bottom(), rect.bottom()),
        ),
        Rect::new(
            rect.left(),
            cut.top(),
            span(rect.left(), cut.left()),
            cut.height,
        ),
        Rect::new(
            cut.right(),
            cut.top(),
            span(cut.right(), rect.right()),
            cut.height,
        ),
    ]
}

/// One window's retained blur: the blurred backdrop, unmixed, over a screen
/// rectangle holding the window's, and the part of it that is stale.
///
/// An opaque cache payload. Nothing outside the window manager reads its
/// pixels, so it exposes no accessors beyond the [`CachedBytes`] contract the
/// cache holding it requires.
pub struct FrostField {
    /// The screen rectangle the field covers, on screen or not.
    rect: Rect,
    /// The physical radius the pixels are blurred by.
    radius_px: u32,
    /// The part of `rect` whose pixels are not the current blur. Held to a
    /// budget, so it grows to its bounding box rather than without limit:
    /// staling more than changed costs a blur, never a wrong pixel.
    stale: Region,
    /// The blurred pixels, `rect`-sized, premultiplied, in row-major order.
    pixels: Surface,
}

impl CachedBytes for FrostField {
    /// The retained heap size of the blurred pixels, and the allowance for
    /// the stale region's.
    fn payload_bytes(&self) -> usize {
        self.pixels.payload_bytes().saturating_add(STALE_HEAP_BYTES)
    }

    /// Overwrite the blurred pixels, so a released entry leaves no readable
    /// image of the user's desktop behind in reusable heap.
    fn wipe(&mut self) {
        self.pixels.wipe();
    }
}

impl FrostField {
    /// The rectangle the field of a window at `window` blurred by `radius_px`
    /// covers, given the rectangle `held` of the field it has now.
    ///
    /// A field holding the window, and reaching no further than twice the
    /// margin past it, stays where it is. A window with no field yet is given
    /// one over its own rectangle alone; once it leaves that, its field is the
    /// margin about wherever it then lies.
    pub(crate) fn wanted(held: Option<Rect>, window: Rect, radius_px: u32) -> Rect {
        let margin = margin(radius_px);
        match held {
            Some(held)
                if holds_rect(held, window)
                    && holds_rect(window.outset(margin.saturating_mul(2)), held) =>
            {
                held
            }
            Some(_) => window.outset(margin),
            None => window,
        }
    }

    /// A field over `rect` blurred by `radius_px` holding nothing current yet,
    /// or `None` where its pixels cannot be had.
    pub(crate) fn new(rect: Rect, radius_px: u32) -> Option<Self> {
        let pixels = Surface::new(rect.width, rect.height)?;
        let mut stale = Region::with_budget(FROST_BANDS);
        stale.add(rect);
        Some(Self {
            rect,
            radius_px,
            stale,
            pixels,
        })
    }

    /// A field over `rect` holding what this one holds current there, or
    /// `None` where its pixels cannot be had.
    ///
    /// The blur at a screen pixel does not depend on the field it is kept in,
    /// so a field grown or shrunk around a window keeps what the old one held
    /// rather than blurring it again.
    pub(crate) fn regrown(&self, rect: Rect) -> Option<Self> {
        let mut grown = Self::new(rect, self.radius_px)?;
        let shared = self.rect.intersection(&rect);
        grown.copy_from(self, shared);
        grown.stale.subtract(shared);
        for &stale in self.stale.rects() {
            grown.stale(stale);
        }
        Some(grown)
    }

    /// Move this field to `rect`, the size it already is, keeping what it holds
    /// current there; `false`, changing nothing, for a `rect` of another size.
    ///
    /// The pixels are moved within the buffer already retained, so a drag that
    /// crosses its margin frees and requests nothing.
    pub(crate) fn shift_to(&mut self, rect: Rect) -> bool {
        if (rect.width, rect.height) != (self.rect.width, self.rect.height) {
            return false;
        }
        let shared = self.rect.intersection(&rect);
        let source = |at: Rect| {
            (
                u32::try_from(shared.left().saturating_sub(at.left())).unwrap_or(0),
                u32::try_from(shared.top().saturating_sub(at.top())).unwrap_or(0),
            )
        };
        let (from, to) = (source(self.rect), source(rect));
        let width = self.pixels.width();
        move_block(
            self.pixels.pixels_mut(),
            width,
            (from, to),
            (shared.width, shared.height),
        );
        self.stale.clip(shared);
        for entered in outside(rect, shared) {
            self.stale.add(entered);
        }
        self.rect = rect;
        true
    }

    /// The screen rectangle the field covers.
    pub(crate) const fn rect(&self) -> Rect {
        self.rect
    }

    /// The physical radius the field is blurred by.
    pub(crate) const fn radius_px(&self) -> u32 {
        self.radius_px
    }

    /// Stale whatever of `area` the field covers.
    pub(crate) fn stale(&mut self, area: Rect) {
        self.stale.add(area.intersection(&self.rect));
    }

    /// Whether the field holds the current blur at every pixel of `area`.
    pub(crate) fn holds(&self, area: Rect) -> bool {
        holds_rect(self.rect, area) && !self.stale.intersects(area)
    }

    /// The bounding box of what of the field on `screen` is stale.
    pub(crate) fn stale_on(&self, screen: Rect) -> Rect {
        self.stale.bounds().intersection(&screen)
    }

    /// The stale parts of the field on `screen`, as the spans of the screen's
    /// surface they cover, in `spans`.
    pub(crate) fn stale_spans(&self, screen: Rect, spans: &mut Vec<(Range<u32>, Range<u32>)>) {
        spans.clear();
        for &stale in self.stale.rects() {
            if let Some(span) = surface_span(stale.intersection(&screen)) {
                spans.push(span);
            }
        }
    }

    /// Blur `spans` of the field at half resolution from the backdrop `plane`
    /// holds around them, its cells anchored at `screen`'s top-left, and record
    /// them current, reporting whether the scratch could hold the blur.
    pub(crate) fn refresh(
        &mut self,
        plane: &mut Surface,
        screen: Rect,
        spans: &[(Range<u32>, Range<u32>)],
        scratch: &mut BlurScratch,
        runner: &dyn JobRunner,
    ) -> bool {
        let blurring = Blurring {
            rect: (0, 0, screen.width, screen.height),
            bands: spans,
            radius: self.radius_px,
        };
        let at = (self.rect.left(), self.rect.top());
        if !self
            .pixels
            .blur_halved_from(at, plane, &blurring, scratch, runner)
        {
            return false;
        }
        for (cols, rows) in spans {
            self.stale.subtract(span_rect(cols, rows));
        }
        true
    }

    /// Lay the frost of a window at `bounds` shaped by `shape` into `back`
    /// over `area`: mixed at the shape's coverage over what `back` holds within
    /// its corners, and, with `interior`, the blur copied wherever the shape
    /// covers fully.
    ///
    /// The caller has asked [`holds`](Self::holds) first; a pixel the field
    /// does not cover is left as it is.
    pub(crate) fn lay(
        &self,
        back: &mut Surface,
        area: Rect,
        (bounds, shape): (Rect, Option<WindowShape>),
        interior: bool,
    ) {
        let area = area.intersection(&bounds);
        if interior {
            for copied in self::interior(bounds, shape) {
                self.copy_into(back, area.intersection(&copied));
            }
        }
        let Some(shape) = shape else {
            return;
        };
        let coverage = |x: u32, y: u32| {
            let local = |at: u32, from: i32| u32::try_from(i64::from(at) - i64::from(from));
            match (local(x, bounds.left()), local(y, bounds.top())) {
                (Ok(lx), Ok(ly)) => shape.coverage(lx, ly),
                _ => 0,
            }
        };
        let at = (self.rect.left(), self.rect.top());
        for corner in corners(bounds, Some(shape)) {
            let piece = area.intersection(&corner);
            if let Some((cols, rows)) = surface_span(piece) {
                let extent = (cols.start, rows.start, piece.width, piece.height);
                back.mix_from(&self.pixels, at, extent, coverage);
            }
        }
    }

    /// The blurred pixels, the field's rectangle's own.
    pub(crate) const fn pixels(&self) -> &Surface {
        &self.pixels
    }

    /// Copy the part of the field inside `area` into `back`, replacing whatever
    /// is there: inside the shape's full coverage, the frost is the blur.
    fn copy_into(&self, back: &mut Surface, area: Rect) {
        let target = area.intersection(&self.rect);
        let (Ok(left), Ok(top)) = (u32::try_from(target.left()), u32::try_from(target.top()))
        else {
            return;
        };
        let local_x = u32::try_from(target.left().saturating_sub(self.rect.left())).unwrap_or(0);
        let local_y = u32::try_from(target.top().saturating_sub(self.rect.top())).unwrap_or(0);
        for row in 0..target.height {
            let source = surface::row(&self.pixels, local_y.saturating_add(row));
            let Some((start, destination)) =
                back.row_span_mut(top.saturating_add(row), left, target.width)
            else {
                continue;
            };
            // A clip window that cut the span's leading columns advances the
            // source by as much, so the two stay aligned.
            let lead = usize::try_from(local_x.saturating_add(start.saturating_sub(left)))
                .unwrap_or(usize::MAX);
            let Some(source) = source.get(lead..) else {
                continue;
            };
            let len = destination.len().min(source.len());
            if let (Some(destination), Some(source)) =
                (destination.get_mut(..len), source.get(..len))
            {
                destination.copy_from_slice(source);
            }
        }
    }

    /// Copy what `other` holds within `shared`, a part of both fields, into
    /// this one.
    fn copy_from(&mut self, other: &Self, shared: Rect) {
        let offset = |of: Rect| {
            (
                u32::try_from(shared.left().saturating_sub(of.left())).unwrap_or(0),
                u32::try_from(shared.top().saturating_sub(of.top())).unwrap_or(0),
            )
        };
        let (from, to) = (offset(other.rect), offset(self.rect));
        for row in 0..shared.height {
            let source = surface::row(&other.pixels, from.1.saturating_add(row));
            let Some((start, destination)) =
                self.pixels
                    .row_span_mut(to.1.saturating_add(row), to.0, shared.width)
            else {
                continue;
            };
            let lead = usize::try_from(from.0.saturating_add(start.saturating_sub(to.0)))
                .unwrap_or(usize::MAX);
            let Some(source) = source.get(lead..) else {
                continue;
            };
            let len = destination.len().min(source.len());
            if let (Some(destination), Some(source)) =
                (destination.get_mut(..len), source.get(..len))
            {
                destination.copy_from_slice(source);
            }
        }
    }
}

/// The squares at the corners of a window at `bounds` where `shape` covers it
/// by less than full: none for a window with no shape or square corners.
pub(crate) fn corners(bounds: Rect, shape: Option<WindowShape>) -> [Rect; 4] {
    let reach = shape.map_or(0, WindowShape::corner_reach);
    if reach == 0 {
        return [Rect::EMPTY; 4];
    }
    let right = bounds.right().saturating_sub_unsigned(reach);
    let bottom = bounds.bottom().saturating_sub_unsigned(reach);
    let square = |x: i32, y: i32| Rect::new(x, y, reach, reach);
    [
        square(bounds.left(), bounds.top()),
        square(right, bounds.top()),
        square(bounds.left(), bottom),
        square(right, bottom),
    ]
}

/// What of a window at `bounds` its shape covers fully: its rectangle less the
/// corner squares, as the full-height middle and the sides between the
/// corners. The pieces are disjoint, so a frost laid piece by piece copies no
/// pixel twice.
pub(crate) fn interior(bounds: Rect, shape: Option<WindowShape>) -> [Rect; 3] {
    let reach = shape.map_or(0, WindowShape::corner_reach);
    if reach == 0 {
        return [bounds, Rect::EMPTY, Rect::EMPTY];
    }
    let inner = |extent: u32| extent.saturating_sub(reach.saturating_mul(2));
    let side = |x: i32| {
        Rect::new(
            x,
            bounds.top().saturating_add_unsigned(reach),
            reach,
            inner(bounds.height),
        )
    };
    [
        Rect::new(
            bounds.left().saturating_add_unsigned(reach),
            bounds.top(),
            inner(bounds.width),
            bounds.height,
        ),
        side(bounds.left()),
        side(bounds.right().saturating_sub_unsigned(reach)),
    ]
}

/// Move the `w` × `h` block of `pixels`, rows `width` long, from `from` to `to`,
/// rows taken in the order that reads each before anything is written over it.
fn move_block(
    pixels: &mut [Pixel],
    width: u32,
    (from, to): ((u32, u32), (u32, u32)),
    (w, h): (u32, u32),
) {
    let at = |(x, y): (u32, u32), row: u32| {
        let line = u64::from(y.saturating_add(row)) * u64::from(width);
        usize::try_from(line.saturating_add(u64::from(x))).ok()
    };
    let Ok(len) = usize::try_from(w) else {
        return;
    };
    let mut step = |row: u32| {
        if let (Some(source), Some(target)) = (at(from, row), at(to, row)) {
            let fits = source
                .checked_add(len)
                .is_some_and(|end| end <= pixels.len())
                && target
                    .checked_add(len)
                    .is_some_and(|end| end <= pixels.len());
            if fits {
                pixels.copy_within(source..source + len, target);
            }
        }
    };
    if to.1 <= from.1 {
        (0..h).for_each(&mut step);
    } else {
        (0..h).rev().for_each(&mut step);
    }
}

/// `rect`, on screen, as the column and row ranges a surface is addressed in,
/// or `None` for an empty rectangle or one reaching above or left of the
/// screen.
pub(crate) fn surface_span(rect: Rect) -> Option<(Range<u32>, Range<u32>)> {
    if rect.is_empty() {
        return None;
    }
    let (Ok(left), Ok(top)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
        return None;
    };
    Some((
        left..left.saturating_add(rect.width),
        top..top.saturating_add(rect.height),
    ))
}

/// The screen rectangle a surface's `cols` × `rows` cover.
pub(crate) fn span_rect(cols: &Range<u32>, rows: &Range<u32>) -> Rect {
    let at = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    Rect::new(
        at(cols.start),
        at(rows.start),
        cols.end.saturating_sub(cols.start),
        rows.end.saturating_sub(rows.start),
    )
}
