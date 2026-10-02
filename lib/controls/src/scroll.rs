//! The orientation-independent scroll geometry engine.
//!
//! A scrollbar exposes one viewport's position within a larger content, and
//! the design language requires exactly one implementation of that behaviour
//! shared by the window-manager root viewport and by nested application
//! content, across both axes. This module is that one implementation: it
//! turns a validated [`ScrollRange`] into a draggable thumb ([`ScrollGeometry`])
//! and maps pointer, wheel, and keyboard input back to a clamped offset
//! ([`ScrollModel`]).
//!
//! # Units
//!
//! A scrolling view counts in **physical pixels**: its content is laid out at
//! its natural size and the viewport is a window onto it that can rest at any
//! pixel, the way a desktop scroll view behaves. Content extent, viewport
//! extent, offset, and both steps share that unit ([`ScrollModel::in_pixels`]),
//! and [`ScrollView`] is the one mapping between the unscrolled layout and the
//! window, so a partly scrolled-off item is drawn whole and cut by the
//! viewport's edge rather than squeezed into what is left of it.
//! [`ScrollGeometry`] works in the *track length* instead — the pixels the
//! thumb travels along — because thumb size and position are a rendering
//! concern: the range says "where am I in the content", the geometry says
//! "where is the thumb on screen".
//!
//! # Fail-closed
//!
//! Invalid, overflowing, or stale range data normalises to a non-draggable,
//! zero-offset scrollbar rather than producing out-of-bounds geometry. Every
//! division is guarded by a non-zero denominator, so no path panics.

use core::ops::Range;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_geometry::{to_i32, Point, Rect, Region};
use tairix_input::InputEvent;
use tairix_raster::Surface;

/// How far one wheel detent scrolls a view, in logical pixels: about three
/// lines of body text.
///
/// One distance for every view rather than a count of each view's own rows,
/// so a detent moves a list of tall cards and a column of prose alike. The
/// seat has already accelerated a fast spin into more than one detent's
/// worth of scroll units.
pub const WHEEL_STEP: u32 = 48;

/// How long a press is held before it starts repeating, in nanoseconds.
///
/// The one cadence every press-and-hold stepping control is paced by
/// ([`crate::scrollbar::ScrollBar::repeat`],
/// [`crate::toolbar::Toolbar::repeat`]), so two controls in one window cannot
/// step at different rates. The owner arms its one-shot timer on this and on
/// [`REPEAT_INTERVAL_NS`] thereafter; nothing here polls.
///
/// Long enough that a deliberate single step is never mistaken for a hold.
pub const REPEAT_DELAY_NS: u64 = 400_000_000;

/// How long between repeats once a held press has started stepping, in
/// nanoseconds (see [`REPEAT_DELAY_NS`]).
pub const REPEAT_INTERVAL_NS: u64 = 60_000_000;

/// Which axis a scrollbar lays out along.
///
/// The behaviour is identical on both axes; orientation only decides how the
/// owning viewport maps the computed one-dimensional [`ThumbSpan`] onto a
/// screen rectangle. Keeping it a single parameter is what lets one engine
/// serve the vertical and horizontal bars without a duplicated recipe.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ScrollOrientation {
    /// Scrolls the viewport's vertical offset; the thumb moves top to bottom.
    Vertical,
    /// Scrolls the viewport's horizontal offset; the thumb moves along the
    /// logical start-to-end axis.
    Horizontal,
}

/// A validated scroll range: how much content there is, how much of it the
/// viewport shows, and where the viewport currently sits.
///
/// The three quantities share the owning viewport's logical scroll unit. A
/// `ScrollRange` is always **normalised**: the offset never exceeds
/// [`max_offset`](ScrollRange::max_offset), and a viewport that covers its
/// content (or a degenerate zero-size viewport) pins the offset to zero. The
/// fields are private precisely so this invariant cannot be violated by
/// constructing an out-of-range value directly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScrollRange {
    content_extent: u64,
    viewport_extent: u64,
    offset: u64,
}

impl ScrollRange {
    /// An empty range: no content, no viewport, no offset. Not scrollable.
    pub const EMPTY: Self = Self {
        content_extent: 0,
        viewport_extent: 0,
        offset: 0,
    };

    /// Build a normalised range from raw extents and a desired offset.
    ///
    /// The offset is clamped into `0..=max_offset`; when the viewport is not
    /// smaller than the content (or is zero), the range is not scrollable and
    /// the offset is forced to zero.
    #[must_use]
    pub fn new(content_extent: u64, viewport_extent: u64, offset: u64) -> Self {
        let mut range = Self {
            content_extent,
            viewport_extent,
            offset: 0,
        };
        range.offset = if range.is_scrollable() {
            offset.min(range.max_offset())
        } else {
            0
        };
        range
    }

    /// The total content extent in the viewport's logical scroll unit.
    #[must_use]
    pub const fn content_extent(&self) -> u64 {
        self.content_extent
    }

    /// The visible viewport extent in the same unit.
    #[must_use]
    pub const fn viewport_extent(&self) -> u64 {
        self.viewport_extent
    }

    /// The current, already-clamped offset.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// `true` when the content is larger than the viewport, so the bar has a
    /// draggable range. A zero-size viewport is never scrollable (fail-closed).
    #[must_use]
    pub const fn is_scrollable(&self) -> bool {
        self.viewport_extent > 0 && self.content_extent > self.viewport_extent
    }

    /// The largest valid offset: `content - viewport` when scrollable, else 0.
    #[must_use]
    pub const fn max_offset(&self) -> u64 {
        if self.is_scrollable() {
            self.content_extent - self.viewport_extent
        } else {
            0
        }
    }

    /// The same range moved to `offset`, re-clamped to stay valid.
    #[must_use]
    pub fn with_offset(self, offset: u64) -> Self {
        Self::new(self.content_extent, self.viewport_extent, offset)
    }

    /// The same viewport position re-expressed against new extents, keeping
    /// the current offset clamped into the new valid range.
    ///
    /// This is what a viewport calls when its content grows or shrinks: the
    /// offset is preserved where it still fits and clamped where it no longer
    /// does, never left dangling past the new end.
    #[must_use]
    pub fn resize(self, content_extent: u64, viewport_extent: u64) -> Self {
        Self::new(content_extent, viewport_extent, self.offset)
    }
}

impl Default for ScrollRange {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// A validated range plus the line-step and page-step distances an input
/// gesture moves, all in the same logical scroll unit.
///
/// The model is the single source of truth for the viewport offset: pointer,
/// wheel, and keyboard input all flow through its step methods, which return a
/// new model with a re-clamped offset. The control never keeps a private
/// offset separate from this.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScrollModel {
    range: ScrollRange,
    line_step: u64,
    page_step: u64,
}

impl ScrollModel {
    /// Build a model from a range and its step distances.
    ///
    /// A zero step is permitted and simply moves nothing, so a viewport that
    /// has not declared a step fails closed to "no movement" rather than to a
    /// guessed distance.
    #[must_use]
    pub fn new(range: ScrollRange, line_step: u64, page_step: u64) -> Self {
        Self {
            range,
            line_step,
            page_step,
        }
    }

    /// A model over a `range` counted in physical pixels, stepping `line`
    /// pixels a line — the view's own row or line of text — and a viewport
    /// less one line a page, so a page turn keeps the last line it showed in
    /// view.
    ///
    /// A zero line reads as one pixel: a pixel view always has a line to step.
    #[must_use]
    pub fn in_pixels(range: ScrollRange, line: u64) -> Self {
        let line = line.max(1);
        let page = range.viewport_extent().saturating_sub(line).max(line);
        Self::new(range, line, page)
    }

    /// This model scrolled the least distance that shows `len` units of the
    /// content from `start`: unmoved when they already show, their end at
    /// the viewport's end when they lie below it, and their start at its
    /// start when they lie above it or are taller than the viewport.
    ///
    /// What a keyboard cursor moving onto an item asks for, so the item it
    /// lands on is always one the reader can see.
    #[must_use]
    pub fn revealing(self, start: u64, len: u64) -> Self {
        let offset = self.offset();
        let seen = self.range.viewport_extent();
        let end = start.saturating_add(len);
        if start < offset || len >= seen {
            return self.scroll_to(start);
        }
        if end > offset.saturating_add(seen) {
            return self.scroll_to(end.saturating_sub(seen));
        }
        self
    }

    /// The underlying validated range.
    #[must_use]
    pub const fn range(&self) -> ScrollRange {
        self.range
    }

    /// The current clamped offset.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.range.offset()
    }

    /// The line-step distance.
    #[must_use]
    pub const fn line_step(&self) -> u64 {
        self.line_step
    }

    /// The page-step distance.
    #[must_use]
    pub const fn page_step(&self) -> u64 {
        self.page_step
    }

    /// This model with the offset set to `offset`, re-clamped.
    #[must_use]
    pub fn scroll_to(self, offset: u64) -> Self {
        Self {
            range: self.range.with_offset(offset),
            ..self
        }
    }

    /// This model with the offset moved by a signed `delta`, saturating at the
    /// range bounds. A negative delta scrolls toward the start.
    #[must_use]
    pub fn scroll_by(self, delta: i64) -> Self {
        let current = i128::from(self.range.offset());
        let moved = current + i128::from(delta);
        let clamped = moved.clamp(0, i128::from(self.range.max_offset()));
        // `clamped` is within `0..=max_offset`, which fits a `u64`; the
        // fallback can never be reached but keeps the path panic-free.
        self.scroll_to(u64::try_from(clamped).unwrap_or(0))
    }

    /// Move one line toward the start.
    #[must_use]
    pub fn line_backward(self) -> Self {
        self.scroll_by(-signed_step(self.line_step))
    }

    /// Move one line toward the end.
    #[must_use]
    pub fn line_forward(self) -> Self {
        self.scroll_by(signed_step(self.line_step))
    }

    /// Move one page toward the start.
    #[must_use]
    pub fn page_backward(self) -> Self {
        self.scroll_by(-signed_step(self.page_step))
    }

    /// Move one page toward the end.
    #[must_use]
    pub fn page_forward(self) -> Self {
        self.scroll_by(signed_step(self.page_step))
    }

    /// Jump to the start of the content.
    #[must_use]
    pub fn to_start(self) -> Self {
        self.scroll_to(0)
    }

    /// Jump to the end of the content.
    #[must_use]
    pub fn to_end(self) -> Self {
        let end = self.range.max_offset();
        self.scroll_to(end)
    }

    /// Re-express the model against new content and viewport extents, keeping
    /// the current offset clamped and the step distances unchanged.
    #[must_use]
    pub fn resize(self, content_extent: u64, viewport_extent: u64) -> Self {
        Self {
            range: self.range.resize(content_extent, viewport_extent),
            ..self
        }
    }
}

/// A step distance as a non-negative `i64`, saturating a pathologically large
/// distance at `i64::MAX` so negation and addition stay in range.
fn signed_step(step: u64) -> i64 {
    i64::try_from(step).unwrap_or(i64::MAX)
}

/// The steps `units` of scroll move a view that moves `per_detent` steps a
/// wheel detent — pixels for a scroll view, whole tools for a strip —
/// carrying what is short of a whole step in `carry`.
///
/// The one conversion from the seat's scroll units, so a turn moves every
/// view in proportion to it. A reversal drops the carry, so a turn back is
/// never shortened by what the turn before it left over.
pub fn wheel_steps(units: i32, per_detent: u64, carry: &mut i64) -> i64 {
    if units == 0 {
        return 0;
    }
    if (*carry < 0) != (units < 0) {
        *carry = 0;
    }
    let per = i128::from(SCROLL_UNITS_PER_DETENT);
    let total = i128::from(*carry) + i128::from(units) * i128::from(signed_step(per_detent));
    let moved = total / per;
    *carry = i64::try_from(total - moved * per).unwrap_or(0);
    i64::try_from(moved).unwrap_or(if moved < 0 { i64::MIN } else { i64::MAX })
}

/// A viewport scrolled a pixel offset along one axis of its content: the one
/// mapping between the content's own layout and the window it shows through.
///
/// A scrolled view lays its content out **unscrolled** — from the viewport's
/// own origin, as if the viewport were long enough to hold all of it — so no
/// part of it ever sits at a negative coordinate. Painting, hit-testing, and
/// damage then translate through here: the paint is confined to the viewport
/// and shifted by the offset, a window point becomes a layout point, and a
/// layout rectangle becomes the part of the window it shows in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScrollView {
    orientation: ScrollOrientation,
    viewport: Rect,
    offset: u32,
}

impl ScrollView {
    /// `viewport` scrolled `offset` pixels along `orientation`.
    ///
    /// An offset past any drawable surface saturates rather than wrapping.
    #[must_use]
    pub fn new(orientation: ScrollOrientation, viewport: Rect, offset: u64) -> Self {
        Self {
            orientation,
            viewport,
            offset: u32::try_from(offset).unwrap_or(u32::MAX),
        }
    }

    /// The window rectangle the content shows through.
    #[must_use]
    pub const fn viewport(&self) -> Rect {
        self.viewport
    }

    /// How far the content is scrolled, in pixels.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// The same scroll confined to `window` rather than the viewport: what
    /// hangs out of the viewport — an open choice list over the window's
    /// other bands — is painted, hit and reported through this.
    ///
    /// Only for what holds the pointer until it resolves: nothing the
    /// viewport hides may be reached through it.
    #[must_use]
    pub const fn confined_to(self, window: Rect) -> Self {
        Self {
            viewport: window,
            ..self
        }
    }

    /// Run `paint` in the content's own layout, its writes confined to the
    /// viewport and shifted by the offset.
    ///
    /// An item wholly outside the viewport is admitted nowhere, so a paint
    /// that tests [`Surface::admits`] before composing pays nothing for it.
    pub fn paint(&self, surface: &mut Surface, paint: impl FnOnce(&mut Surface)) {
        let (Ok(x), Ok(y)) = (
            u32::try_from(self.viewport.left()),
            u32::try_from(self.viewport.top()),
        ) else {
            return;
        };
        let (dx, dy) = self.shift();
        surface.with_clip(x, y, self.viewport.width, self.viewport.height, |clipped| {
            clipped.with_origin(dx, dy, paint);
        });
    }

    /// The layout point shown at window `point`, or `None` when the point
    /// lies outside the viewport.
    #[must_use]
    pub fn to_content(&self, point: Point) -> Option<Point> {
        self.viewport.contains(point).then(|| self.in_layout(point))
    }

    /// `event` with the position a pointer move carries mapped into the
    /// layout; every other event unchanged.
    ///
    /// A pointer outside the viewport keeps its place across the scrolling
    /// axis and stands just before the content's start along it, outside
    /// every item: a control fed it sees the pointer leave and can never hover
    /// or arm what the reader cannot see, while a drag across that axis — a
    /// slider in a scrolling column — keeps following the pointer.
    #[must_use]
    pub fn event_in_layout(&self, event: &InputEvent) -> InputEvent {
        match *event {
            InputEvent::PointerMoved { to } => InputEvent::PointerMoved {
                to: self.to_content(to).unwrap_or_else(|| self.before_start(to)),
            },
            other => other,
        }
    }

    /// The layout point under window `point`, wherever it lies.
    fn in_layout(&self, point: Point) -> Point {
        let (dx, dy) = self.shift();
        Point::new(
            point.x.saturating_add(to_i32(dx)),
            point.y.saturating_add(to_i32(dy)),
        )
    }

    /// Where a pointer outside the viewport stands in the layout: one pixel
    /// before the content's start along the scrolling axis, where nothing laid
    /// out from the viewport's origin reaches. The offset shifts only that
    /// axis, so the other is already the layout's.
    fn before_start(&self, point: Point) -> Point {
        match self.orientation {
            ScrollOrientation::Vertical => {
                Point::new(point.x, self.viewport.top().saturating_sub(1))
            }
            ScrollOrientation::Horizontal => {
                Point::new(self.viewport.left().saturating_sub(1), point.y)
            }
        }
    }

    /// The part of the window layout rectangle `rect` shows in, or `None`
    /// when none of it does.
    #[must_use]
    pub fn to_window(&self, rect: Rect) -> Option<Rect> {
        let (dx, dy) = self.shift();
        let shifted = Rect::new(
            rect.left().saturating_sub(to_i32(dx)),
            rect.top().saturating_sub(to_i32(dy)),
            rect.width,
            rect.height,
        );
        let shown = shifted.intersection(&self.viewport);
        (!shown.is_empty()).then_some(shown)
    }

    /// Report what a control reported in the content's layout, `layout`, as
    /// the window rectangles it shows in, adding them to `damage`.
    ///
    /// A control laid out unscrolled reports its changes where it drew them;
    /// the window repaints where they landed, and a change the viewport shows
    /// none of costs no repaint at all.
    pub fn report(&self, layout: &Region, damage: &mut Region) {
        for rect in layout.rects() {
            if let Some(shown) = self.to_window(*rect) {
                damage.add(shown);
            }
        }
    }

    /// Which of `count` lines `pitch` pixels apart, laid out from the
    /// viewport's own start along its axis, the viewport shows any part of.
    ///
    /// The one answer to "which rows do I paint" for a list of even rows, so
    /// a list can lay out only what shows however long it is.
    #[must_use]
    pub fn lines(&self, pitch: u32, count: usize) -> Range<usize> {
        let extent = match self.orientation {
            ScrollOrientation::Vertical => self.viewport.height,
            ScrollOrientation::Horizontal => self.viewport.width,
        };
        if pitch == 0 || extent == 0 {
            return 0..0;
        }
        let line = |at: u32| usize::try_from(at / pitch).unwrap_or(usize::MAX).min(count);
        let first = line(self.offset);
        let last = line(self.offset.saturating_add(extent - 1))
            .saturating_add(1)
            .min(count);
        first..last.max(first)
    }

    /// The span of the layout's scrolling axis the viewport shows.
    #[must_use]
    pub fn shown(&self) -> Range<i32> {
        let (start, extent) = match self.orientation {
            ScrollOrientation::Vertical => (self.viewport.top(), self.viewport.height),
            ScrollOrientation::Horizontal => (self.viewport.left(), self.viewport.width),
        };
        let start = start.saturating_add(to_i32(self.offset));
        start..start.saturating_add(to_i32(extent))
    }

    /// The offset as a shift of the paint's two axes.
    fn shift(&self) -> (u32, u32) {
        match self.orientation {
            ScrollOrientation::Vertical => (0, self.offset),
            ScrollOrientation::Horizontal => (self.offset, 0),
        }
    }
}

/// A one-dimensional thumb: its start and length along the track, in physical
/// track pixels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ThumbSpan {
    /// Distance from the track start to the thumb's near edge, in pixels.
    pub start: u32,
    /// The thumb's length along the track, in pixels.
    pub length: u32,
}

/// Which part of the track a coordinate falls in.
///
/// End buttons (decrement/increment) are laid out by the owning viewport with
/// its own theme metrics and are *not* part of the track this engine measures;
/// [`ScrollGeometry`] is given only the track span between them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TrackHit {
    /// The track region before the thumb — a page step toward the start.
    BeforeThumb,
    /// The thumb itself — the start of a drag.
    Thumb,
    /// The track region after the thumb — a page step toward the end.
    AfterThumb,
}

/// The painted geometry of a scrollbar thumb for a given track length.
///
/// Constructed from a [`ScrollRange`], the physical `track_len` between the end
/// controls, and the theme's minimum thumb length. All results are clamped to
/// the track, and a non-scrollable or zero-travel range is reported as
/// non-[`draggable`](ScrollGeometry::draggable) with a zero-offset thumb.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScrollGeometry {
    range: ScrollRange,
    track_len: u32,
    min_thumb: u32,
}

impl ScrollGeometry {
    /// Build the geometry for a track of `track_len` physical pixels, with a
    /// theme-supplied `min_thumb` minimum thumb length.
    #[must_use]
    pub fn new(range: ScrollRange, track_len: u32, min_thumb: u32) -> Self {
        Self {
            range,
            track_len,
            min_thumb,
        }
    }

    /// The range this geometry was built from.
    #[must_use]
    pub const fn range(&self) -> ScrollRange {
        self.range
    }

    /// The full track length in pixels.
    #[must_use]
    pub const fn track_len(&self) -> u32 {
        self.track_len
    }

    /// The thumb length in pixels: proportional to the fraction of the content
    /// the viewport shows, never smaller than the theme minimum (bounded by the
    /// track), and never longer than the track. A non-scrollable range fills
    /// the whole track with a non-draggable thumb.
    #[must_use]
    pub fn thumb_length(&self) -> u32 {
        if self.track_len == 0 {
            return 0;
        }
        if !self.range.is_scrollable() {
            return self.track_len;
        }
        let track = u128::from(self.track_len);
        // viewport < content (scrollable), so this is strictly less than the
        // track length. `u128` keeps the product from overflowing for any
        // `u64` extents.
        let proportional = track * u128::from(self.range.viewport_extent())
            / u128::from(self.range.content_extent());
        let floor = u128::from(self.min_thumb).min(track).max(1);
        let length = proportional.max(floor).min(track);
        // `length <= track == track_len`, so this always fits.
        u32::try_from(length).unwrap_or(self.track_len)
    }

    /// The distance the thumb can travel: `track_len - thumb_length`.
    #[must_use]
    pub fn travel(&self) -> u32 {
        self.track_len.saturating_sub(self.thumb_length())
    }

    /// `true` when the thumb can be dragged: the range is scrollable and the
    /// thumb does not already fill the whole track.
    #[must_use]
    pub fn draggable(&self) -> bool {
        self.range.is_scrollable() && self.travel() > 0
    }

    /// The thumb's current span along the track, derived from the range offset.
    #[must_use]
    pub fn thumb(&self) -> ThumbSpan {
        let length = self.thumb_length();
        let start = if self.draggable() {
            let travel = u128::from(self.travel());
            let max_offset = u128::from(self.range.max_offset());
            // `draggable()` guarantees both `travel` and `max_offset` are > 0.
            let start = travel * u128::from(self.range.offset()) / max_offset;
            // `start <= travel == travel()`, so this always fits.
            u32::try_from(start).unwrap_or_else(|_| self.travel())
        } else {
            0
        };
        ThumbSpan { start, length }
    }

    /// Classify a track-relative coordinate (0 at the track start).
    #[must_use]
    pub fn hit(&self, pos: u32) -> TrackHit {
        let thumb = self.thumb();
        if pos < thumb.start {
            TrackHit::BeforeThumb
        } else if pos < thumb.start.saturating_add(thumb.length) {
            TrackHit::Thumb
        } else {
            TrackHit::AfterThumb
        }
    }

    /// The content offset a thumb whose near edge sits at `thumb_start` (in
    /// track pixels) represents. The inverse of [`thumb`](ScrollGeometry::thumb),
    /// rounded to the nearest offset and clamped to the valid range.
    #[must_use]
    pub fn offset_for_thumb_start(&self, thumb_start: u32) -> u64 {
        if !self.draggable() {
            return 0;
        }
        let travel = u128::from(self.travel());
        let clamped = u128::from(thumb_start).min(travel);
        let max_offset = u128::from(self.range.max_offset());
        // `travel > 0` (draggable); round to nearest so the extremes map to 0
        // and `max_offset` exactly. `u128` keeps the product in range.
        let offset = (clamped * max_offset + travel / 2) / travel;
        u64::try_from(offset.min(max_offset)).unwrap_or(self.range.max_offset())
    }

    /// The content offset implied by a thumb drag.
    ///
    /// `pointer_pos` is the pointer's current position along the track and
    /// `anchor` is the pointer-to-thumb-start distance captured when the drag
    /// began (`pointer_pos_at_start - thumb_start_at_start`). Preserving the
    /// anchor keeps the content from jumping when the drag begins, and the
    /// implied thumb start is clamped into the track before mapping, so a drag
    /// past either end pins to that end rather than overflowing.
    #[must_use]
    pub fn offset_for_drag(&self, pointer_pos: i32, anchor: i32) -> u64 {
        if !self.draggable() {
            return 0;
        }
        let travel = i64::from(self.travel());
        let thumb_start = i64::from(pointer_pos) - i64::from(anchor);
        let clamped = thumb_start.clamp(0, travel);
        // `clamped` is within `0..=travel == travel()`, so this always fits.
        self.offset_for_thumb_start(u32::try_from(clamped).unwrap_or_else(|_| self.travel()))
    }
}
