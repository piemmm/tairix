//! The scrollbar renderer: the one orientation-parameterized [`ScrollBar`]
//! control (spec §11.28–§11.30). The `ScrollCorner` at a two-bar junction
//! (§11.31) is drawn by the window-manager furniture family.
//!
//! Vertical and horizontal scrollbars are a single behavioural component; the
//! [`ScrollOrientation`] only decides how the shared one-dimensional thumb math
//! ([`crate::scroll`]) maps onto the screen. This module is the *drawn* form of
//! that component: it lays a bar out into a decrement button, a track (before
//! thumb / thumb / after thumb), and an increment button; paints the recessed
//! Scroll Channel with the two end buttons and the thumb raised in it from the
//! active [`Theme`] and [`Scale`]; classifies a pointer position; and maps
//! pointer drag, track paging, end-button line steps, the wheel, and
//! arrow/page/home/end keys back to a clamped offset through the shared
//! [`ScrollModel`].
//!
//! The control does **not** keep a private offset beside the owning viewport:
//! it holds the viewport's own [`ScrollModel`] (refreshed by the owner through
//! [`ScrollBar::set_model`]), updates it immediately so a drag reads smoothly,
//! and reports the requested offset as a typed [`ScrollAction`] the owner
//! applies. Every result is clamped and every division guarded, so an empty,
//! non-scrollable, or degenerate bar simply does not move (fail closed).

use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{edge_weight, Theme};

use crate::damage;
use crate::paint::{
    authority_rgba, draw_outline, ground_fill, heavy_contrast, paint_chevron, plate_border,
    surface_rect, to_i32, withheld, ChevronDir, ChromeLayer,
};
use crate::scroll::{
    wheel_steps, HoldRepeat, ScrollGeometry, ScrollModel, ScrollOrientation, ThumbSpan, TrackHit,
    WHEEL_STEP,
};
use crate::state::{ControlDisposition, ControlState, PointerState, RenderInvariant};

/// The outcome of interacting with a [`ScrollBar`].
///
/// A scrollbar updates its own held model immediately so a drag reads smoothly,
/// but the authoritative offset still lives in the owning viewport: the owner
/// receives the requested offset and applies it to the real content, then
/// refreshes the bar with [`ScrollBar::set_model`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ScrollAction {
    /// The bar requests its offset become `offset`, in the model's logical
    /// scroll unit.
    ScrollTo {
        /// The requested new offset (already clamped to the valid range).
        offset: u64,
    },
}

/// Which part of a scrollbar a pointer position falls in.
///
/// The two end buttons perform one line step each; the track regions before
/// and after the thumb perform one page step each; the thumb starts a drag.
/// This is the one classification paint, hit testing, and input all share, so
/// the mapping cannot diverge between them (spec §11.28).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ScrollPart {
    /// The decrement end button — one line step toward the start.
    Decrement,
    /// The increment end button — one line step toward the end.
    Increment,
    /// The track before the thumb — one page step toward the start.
    TrackBefore,
    /// The thumb itself — the start of a drag.
    Thumb,
    /// The track after the thumb — one page step toward the end.
    TrackAfter,
    /// Not over the bar at all.
    Outside,
}

/// The model step a pressed `part` performs, or `None` for a part that presses
/// into no step (the thumb, which drags instead, and off the bar).
///
/// One mapping for the initial press and for every auto-repeat of it, so a held
/// button cannot repeat a different step from the one it started with.
fn step_for(part: ScrollPart) -> Option<fn(ScrollModel) -> ScrollModel> {
    match part {
        ScrollPart::Decrement => Some(ScrollModel::line_backward),
        ScrollPart::Increment => Some(ScrollModel::line_forward),
        ScrollPart::TrackBefore => Some(ScrollModel::page_backward),
        ScrollPart::TrackAfter => Some(ScrollModel::page_forward),
        ScrollPart::Thumb | ScrollPart::Outside => None,
    }
}

/// The resolved on-screen anatomy of one scrollbar: the two end buttons, the
/// track between them, and the thumb geometry over that track.
///
/// Every rectangle is in surface coordinates. The `track_origin` is the track's
/// near edge along the bar's long axis, so a pointer's along-axis distance is a
/// single subtraction regardless of orientation.
struct BarLayout {
    orientation: ScrollOrientation,
    bar: Rect,
    decrement: Rect,
    increment: Rect,
    track: Rect,
    track_origin: i32,
    breadth: u32,
    geometry: ScrollGeometry,
}

impl BarLayout {
    /// The pointer's distance along the track's long axis from the track start.
    fn along(&self, point: Point) -> i32 {
        let coord = match self.orientation {
            ScrollOrientation::Vertical => point.y,
            ScrollOrientation::Horizontal => point.x,
        };
        coord - self.track_origin
    }

    /// Classify a surface `point` into a [`ScrollPart`].
    fn part_at(&self, point: Point) -> ScrollPart {
        if self.decrement.contains(point) {
            return ScrollPart::Decrement;
        }
        if self.increment.contains(point) {
            return ScrollPart::Increment;
        }
        if self.track.contains(point) {
            let along = u32::try_from(self.along(point)).unwrap_or(0);
            return match self.geometry.hit(along) {
                TrackHit::BeforeThumb => ScrollPart::TrackBefore,
                TrackHit::Thumb => ScrollPart::Thumb,
                TrackHit::AfterThumb => ScrollPart::TrackAfter,
            };
        }
        ScrollPart::Outside
    }

    /// The rectangle [`part_at`](Self::part_at) classifies as `part`, or `None`
    /// for a part this layout draws no pixels of.
    fn part_rect(&self, part: ScrollPart) -> Option<Rect> {
        let ThumbSpan { start, length } = self.geometry.thumb();
        let track_len = match self.orientation {
            ScrollOrientation::Vertical => self.track.height,
            ScrollOrientation::Horizontal => self.track.width,
        };
        let end = start.saturating_add(length).min(track_len);
        let span = |from: u32, to: u32| {
            let length = to.saturating_sub(from);
            let near = self.track_origin + to_i32(from);
            match self.orientation {
                ScrollOrientation::Vertical => {
                    Rect::new(self.track.left(), near, self.track.width, length)
                }
                ScrollOrientation::Horizontal => {
                    Rect::new(near, self.track.top(), length, self.track.height)
                }
            }
        };
        let rect = match part {
            ScrollPart::Decrement => self.decrement,
            ScrollPart::Increment => self.increment,
            ScrollPart::TrackBefore => span(0, start.min(track_len)),
            ScrollPart::Thumb => span(start.min(track_len), end),
            ScrollPart::TrackAfter => span(end, track_len),
            ScrollPart::Outside => return None,
        };
        (!rect.is_empty()).then_some(rect)
    }

    /// How far a raised part — the thumb or an end button — stands in from
    /// the channel's walls, so it reads as sitting in the Scroll Channel.
    fn inset(&self) -> u32 {
        self.breadth / 6
    }

    /// The thumb's surface rectangle, inset from the channel walls on the
    /// short axis.
    fn thumb_rect(&self) -> Rect {
        let ThumbSpan { start, length } = self.geometry.thumb();
        let short = self
            .breadth
            .saturating_sub(self.inset().saturating_mul(2))
            .max(1);
        let off = self.breadth.saturating_sub(short) / 2;
        match self.orientation {
            ScrollOrientation::Vertical => Rect::new(
                self.track.left() + to_i32(off),
                self.track_origin + to_i32(start),
                short,
                length,
            ),
            ScrollOrientation::Horizontal => Rect::new(
                self.track_origin + to_i32(start),
                self.track.top() + to_i32(off),
                length,
                short,
            ),
        }
    }
}

/// A window-manager or application scrollbar in either orientation (spec
/// §11.28–§11.30).
///
/// The bar is laid out into a decrement button, a track, and an increment
/// button; its thumb math comes from the shared [`crate::scroll`] engine so the
/// vertical and horizontal forms are one behaviour parameterised by
/// [`ScrollOrientation`]. It holds the owning viewport's [`ScrollModel`] (never
/// a private offset beside it) and reports every requested change as a typed
/// [`ScrollAction`]; a denied or disabled bar keeps its position and ignores
/// input rather than looking merely inert (spec §13).
///
/// A held end button or track press repeats on its own cadence: the owner
/// folds [`repeat_deadline`](Self::repeat_deadline) into its wait and calls
/// [`repeat`](Self::repeat) when it comes. A held step goes on only while the
/// pointer stays on the part it pressed, so a press on the track pages until
/// the thumb reaches the pointer and no further.
///
/// Equal bars draw the same pixels, so a host may use `==` as its repaint
/// gate. The visible interaction state compares: `dragging`/`held` brighten
/// the thumb, and an end control is lit while its press is live — held, with
/// the pointer on it — never merely because the pointer is over it. The part
/// under the pointer, the raw pointer coordinate, the drag anchor and the
/// repeat cadence do not: they are input bookkeeping no render path reads, so
/// crossing between parts with nothing held draws the same pixels and must not
/// force the host to repaint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScrollBar {
    orientation: ScrollOrientation,
    /// Whether the channel's long sides carry their edge.
    edged: bool,
    model: ScrollModel,
    state: ControlState,
    /// The part the pointer currently sits over, resolved when the pointer
    /// moves and after every step that moved the thumb under it.
    hover: RenderInvariant<ScrollPart>,
    /// The end control drawn lit: the held one, while the pointer is on it.
    lit: Option<ScrollPart>,
    /// The last pointer position, classified on the next press or re-mapped on
    /// the next drag move — hit-testing input, never a drawn property.
    pointer: RenderInvariant<Point>,
    dragging: bool,
    /// How far along the thumb the drag was grabbed, so the thumb keeps its
    /// offset under the pointer instead of jumping its start to it.
    anchor: RenderInvariant<i32>,
    held: Option<ScrollPart>,
    /// When the held press steps next.
    cadence: RenderInvariant<HoldRepeat>,
    /// Wheel scroll short of a whole pixel, carried to the next turn.
    wheel_carry: RenderInvariant<i64>,
}

impl ScrollBar {
    /// A scrollbar of the given orientation driven by `model`.
    #[must_use]
    pub fn new(orientation: ScrollOrientation, model: ScrollModel) -> Self {
        Self {
            orientation,
            edged: true,
            model,
            state: ControlState::idle(),
            hover: RenderInvariant::new(ScrollPart::Outside),
            lit: None,
            pointer: RenderInvariant::new(Point::ORIGIN),
            dragging: false,
            anchor: RenderInvariant::new(0),
            held: None,
            cadence: RenderInvariant::new(HoldRepeat::default()),
            wheel_carry: RenderInvariant::new(0),
        }
    }

    /// This bar with no edge along its channel: one embedded in a plate whose
    /// own rim already bounds it, where an edge standing mid-plate with open
    /// ends reads as a stray line.
    #[must_use]
    pub const fn without_edges(mut self) -> Self {
        self.edged = false;
        self
    }

    /// The bar's orientation.
    #[must_use]
    pub fn orientation(&self) -> ScrollOrientation {
        self.orientation
    }

    /// The bar's current scroll model (the owning viewport's model).
    #[must_use]
    pub fn model(&self) -> ScrollModel {
        self.model
    }

    /// Replace the scroll model — what the owner calls after it applies a
    /// [`ScrollAction`] or when its content or client size changes, so the bar
    /// never keeps an offset separate from the viewport.
    pub fn set_model(&mut self, model: ScrollModel) {
        self.model = model;
    }

    /// The bar's composed control state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the bar's composed state (e.g. an authority or pressure update).
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
    }

    /// Set the bar's keyboard focus.
    pub fn set_focused(&mut self, focused: bool) {
        self.state.focus.focused = focused;
    }

    /// The part currently held for press-and-hold auto-repeat, if any.
    #[must_use]
    pub fn held(&self) -> Option<ScrollPart> {
        self.held
    }

    /// Whether a primary press is currently captured on the bar — a thumb
    /// drag in progress or an end/track button held down. An owner uses this
    /// to decide that a subsequent move or release belongs to the bar (the
    /// interaction the press started) rather than to the content beneath it.
    #[must_use]
    pub fn is_pressing(&self) -> bool {
        self.dragging || self.held.is_some()
    }

    /// When the held press steps next, as a monotonic instant, or `None` when
    /// nothing held would move: no press, the pointer off the part it pressed,
    /// or the offset already at the end that part steps toward.
    ///
    /// The first step of a press just made is armed
    /// [`REPEAT_DELAY_NS`](crate::scroll::REPEAT_DELAY_NS) after `now_ns`, so
    /// the owner asks for this after every event it routes and folds the
    /// answer into its wait; nothing here polls.
    pub fn repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        let stepping = self.stepping();
        self.cadence.deadline(stepping, now_ns)
    }

    /// Whether the held press's next step is due at `now_ns`: what an owner
    /// asks before laying out what [`repeat`](Self::repeat) needs, so a wake
    /// with nothing due costs no layout. Arms a new press's first step as
    /// [`repeat_deadline`](Self::repeat_deadline) does.
    pub fn repeat_due(&mut self, now_ns: u64) -> bool {
        self.repeat_deadline(now_ns)
            .is_some_and(|due| due <= now_ns)
    }

    /// Step the held press once if its repeat is due at `now_ns`, for the bar
    /// drawn at `bounds`, returning the requested offset when it moved.
    ///
    /// Reports `bounds` into `damage` for a moved thumb, and re-reads the part
    /// under the pointer, so a track press stops paging when the thumb
    /// arrives beneath it.
    pub fn repeat(
        &mut self,
        now_ns: u64,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<ScrollAction> {
        if !self.stepping() || !self.cadence.fire(now_ns) {
            return None;
        }
        let step = step_for(self.held?)?;
        let action = self.apply(step, bounds, damage);
        self.rehover(bounds, scale, theme, damage);
        action
    }

    /// Classify a surface `point` into a [`ScrollPart`] for the bar drawn at
    /// `bounds`, using the same layout as paint and input (spec §11.28). Off a
    /// present bar, or when the bar is degenerate, this is [`ScrollPart::Outside`].
    #[must_use]
    pub fn part_at(&self, bounds: Rect, point: Point, scale: Scale, theme: &Theme) -> ScrollPart {
        match self.layout(bounds, scale, theme) {
            Some(layout) => layout.part_at(point),
            None => ScrollPart::Outside,
        }
    }

    /// The rectangle `part` occupies for the bar drawn at `bounds`, or `None`
    /// when the bar is degenerate or draws none of that part — a track run
    /// the thumb covers, or [`ScrollPart::Outside`].
    ///
    /// The forward mirror of [`part_at`](Self::part_at) over the same layout,
    /// so a caller that must aim *at* a part reads the rectangle input
    /// classifies rather than arithmetic of its own.
    #[must_use]
    pub fn part_rect(
        &self,
        part: ScrollPart,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        self.layout(bounds, scale, theme)?.part_rect(part)
    }

    /// The scroll geometry for the bar drawn at `bounds`, or `None` when the
    /// bar is degenerate. Exposed so an owner (or a test) can reason about the
    /// thumb without re-deriving the layout.
    #[must_use]
    pub fn geometry(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<ScrollGeometry> {
        self.layout(bounds, scale, theme).map(|l| l.geometry)
    }

    /// Whether the bar is drawn in its awake (brightened) form — being
    /// hovered, focused, dragged, or repeating an end/track press.
    fn is_active(&self) -> bool {
        self.dragging
            || self.held.is_some()
            || self.state.focus.focused
            || matches!(
                self.state.pointer,
                PointerState::Hover | PointerState::Pressed
            )
    }

    /// Whether a press on `part` would move the offset: the end buttons and
    /// the track regions toward an end the content does not already rest at.
    fn moves(&self, part: ScrollPart) -> bool {
        step_for(part).is_some_and(|step| step(self.model).offset() != self.model.offset())
    }

    /// Whether the held press is stepping: the pointer is still on the part
    /// it pressed, and that part would move the offset.
    fn stepping(&self) -> bool {
        self.held.is_some_and(|held| {
            held == *self.hover && self.state.is_actionable() && self.moves(held)
        })
    }

    /// Re-read the part under the pointer for the bar drawn at `bounds`, after
    /// a step moved the thumb beneath it.
    fn rehover(&mut self, bounds: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        if let Some(layout) = self.layout(bounds, scale, theme) {
            self.hover_on(layout.part_at(*self.pointer), bounds, damage);
        }
    }

    /// Take `part` as the one under the pointer, relighting the end control
    /// whose press that makes live or idle.
    fn hover_on(&mut self, part: ScrollPart, bounds: Rect, damage: &mut Region) {
        *self.hover = part;
        let live = self.held.filter(|held| {
            *held == part && matches!(held, ScrollPart::Decrement | ScrollPart::Increment)
        });
        damage::set(&mut self.lit, live, bounds, damage);
    }

    /// Apply `change` to the held model, returning the requested offset when it
    /// actually moved (fail closed: no movement means no action) and reporting
    /// `bounds` — a moved offset is a moved thumb.
    fn apply(
        &mut self,
        change: impl FnOnce(ScrollModel) -> ScrollModel,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<ScrollAction> {
        let next = change(self.model);
        let offset = next.offset();
        let moved = offset != self.model.offset();
        damage::set(&mut self.model, next, bounds, damage);
        moved.then_some(ScrollAction::ScrollTo { offset })
    }

    /// End a press: the drag and the held step stop where they are, and the
    /// bar takes `pointer` as its look.
    fn end_press(&mut self, pointer: PointerState, bounds: Rect, damage: &mut Region) {
        damage::set(&mut self.dragging, false, bounds, damage);
        damage::set(&mut self.held, None, bounds, damage);
        damage::set(&mut self.lit, None, bounds, damage);
        damage::set(&mut self.state.pointer, pointer, bounds, damage);
        self.cadence.release();
    }

    /// Feed a pointer event for the bar drawn at `bounds`.
    ///
    /// An end-button press steps one line, a track press pages one step, and a
    /// thumb press captures a drag whose pointer-to-thumb anchor is preserved so
    /// the content does not jump; a subsequent move re-maps and re-clamps the
    /// offset (so a mid-drag range change stays valid). A press toward an end
    /// the content already rests at presses nothing. A denied, disabled,
    /// pending, or failed-closed bar ignores presses (fail closed), and a
    /// release or a cancel always ends the press, whatever became of the bar.
    /// The bar reports `bounds` into `damage` when the event changed how it
    /// draws — a moved thumb, an end control lit or put out, the awake look
    /// arriving or leaving. It is the whole bar because that look is the whole
    /// bar: the thumb brightens wherever on the bar the pointer is.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<ScrollAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        match event {
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let over = if bounds.contains(*self.pointer) {
                    PointerState::Hover
                } else {
                    PointerState::None
                };
                self.end_press(over, bounds, damage);
                return None;
            }
            InputEvent::PointerCancelled => {
                self.end_press(PointerState::None, bounds, damage);
                *self.hover = ScrollPart::Outside;
                return None;
            }
            _ => {}
        }
        let layout = self.layout(bounds, scale, theme)?;
        let over_bar = if layout.bar.contains(*self.pointer) {
            PointerState::Hover
        } else {
            PointerState::None
        };
        match event {
            InputEvent::PointerMoved { .. } => {
                let part = layout.part_at(*self.pointer);
                self.hover_on(part, bounds, damage);
                if self.dragging {
                    let offset = layout
                        .geometry
                        .offset_for_drag(layout.along(*self.pointer), *self.anchor);
                    self.apply(|m| m.scroll_to(offset), bounds, damage)
                } else {
                    damage::set(&mut self.state.pointer, over_bar, bounds, damage);
                    None
                }
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                if !self.state.is_actionable() {
                    return None;
                }
                let part = layout.part_at(*self.pointer);
                if let Some(step) = step_for(part) {
                    if !self.moves(part) {
                        return None;
                    }
                    damage::set(&mut self.held, Some(part), bounds, damage);
                    self.hover_on(part, bounds, damage);
                    damage::set(
                        &mut self.state.pointer,
                        PointerState::Pressed,
                        bounds,
                        damage,
                    );
                    self.cadence.release();
                    let action = self.apply(step, bounds, damage);
                    self.rehover(bounds, scale, theme, damage);
                    return action;
                }
                if part == ScrollPart::Thumb && layout.geometry.draggable() {
                    let thumb_start = to_i32(layout.geometry.thumb().start);
                    *self.anchor = layout.along(*self.pointer) - thumb_start;
                    damage::set(&mut self.dragging, true, bounds, damage);
                    damage::set(
                        &mut self.state.pointer,
                        PointerState::Pressed,
                        bounds,
                        damage,
                    );
                }
                None
            }
            InputEvent::PointerScrolled { dx, dy } if over_bar == PointerState::Hover => {
                self.wheel(*dx, *dy, scale, bounds, damage)
            }
            _ => None,
        }
    }

    /// Feed a key event to a focused, actionable bar: the orientation's arrow
    /// keys step one line, Page Up/Down step one page, and Home/End jump to the
    /// range bounds (spec §11.28).
    pub fn on_key(&mut self, key: Key, bounds: Rect, damage: &mut Region) -> Option<ScrollAction> {
        if !self.state.focus.focused || !self.state.is_actionable() {
            return None;
        }
        let vertical = self.orientation == ScrollOrientation::Vertical;
        let step: fn(ScrollModel) -> ScrollModel = match key {
            Key::Named(NamedKey::PageUp) => ScrollModel::page_backward,
            Key::Named(NamedKey::PageDown) => ScrollModel::page_forward,
            Key::Named(NamedKey::Home) => ScrollModel::to_start,
            Key::Named(NamedKey::End) => ScrollModel::to_end,
            Key::Named(NamedKey::Up) if vertical => ScrollModel::line_backward,
            Key::Named(NamedKey::Down) if vertical => ScrollModel::line_forward,
            Key::Named(NamedKey::Left) if !vertical => ScrollModel::line_backward,
            Key::Named(NamedKey::Right) if !vertical => ScrollModel::line_forward,
            _ => return None,
        };
        self.apply(step, bounds, damage)
    }

    /// Apply wheel `dx`/`dy` scroll units along the bar's own axis —
    /// [`WHEEL_STEP`] logical pixels a detent at `scale` — returning the
    /// requested offset when it moved.
    ///
    /// `bounds` is the bar's own rectangle, reported as damage when the thumb
    /// moves; the owner reports the content it scrolled beside it. A denied
    /// or disabled bar ignores the wheel (fail closed).
    pub fn wheel(
        &mut self,
        dx: i32,
        dy: i32,
        scale: Scale,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<ScrollAction> {
        let units = match self.orientation {
            ScrollOrientation::Vertical => dy,
            ScrollOrientation::Horizontal => dx,
        };
        if units == 0 || !self.state.is_actionable() {
            return None;
        }
        let detent = u64::from(scale.scale_length(WHEEL_STEP).max(1));
        let pixels = wheel_steps(units, detent, &mut self.wheel_carry);
        if pixels == 0 {
            return None;
        }
        self.apply(|m| m.scroll_by(pixels), bounds, damage)
    }

    /// Resolve the bar's on-screen anatomy for `bounds`, or `None` when it
    /// collapses. The end buttons are breadth-square (bounded so the track
    /// keeps at least a third of the long axis); the minimum thumb length is the
    /// theme metric scaled to physical pixels.
    fn layout(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<BarLayout> {
        let (_, _, sw, sh) = surface_rect(bounds)?;
        if sw == 0 || sh == 0 {
            return None;
        }
        let min_thumb = scale.scale_length(theme.metrics().min_thumb_length).max(1);
        let (breadth, long) = match self.orientation {
            ScrollOrientation::Vertical => (sw, sh),
            ScrollOrientation::Horizontal => (sh, sw),
        };
        let button_len = breadth.min(long / 3);
        let track_len = long.saturating_sub(button_len.saturating_mul(2));
        let (decrement, increment, track, track_origin) = match self.orientation {
            ScrollOrientation::Vertical => (
                Rect::new(bounds.left(), bounds.top(), sw, button_len),
                Rect::new(
                    bounds.left(),
                    bounds.top() + to_i32(long - button_len),
                    sw,
                    button_len,
                ),
                Rect::new(
                    bounds.left(),
                    bounds.top() + to_i32(button_len),
                    sw,
                    track_len,
                ),
                bounds.top() + to_i32(button_len),
            ),
            ScrollOrientation::Horizontal => (
                Rect::new(bounds.left(), bounds.top(), button_len, sh),
                Rect::new(
                    bounds.left() + to_i32(long - button_len),
                    bounds.top(),
                    button_len,
                    sh,
                ),
                Rect::new(
                    bounds.left() + to_i32(button_len),
                    bounds.top(),
                    track_len,
                    sh,
                ),
                bounds.left() + to_i32(button_len),
            ),
        };
        Some(BarLayout {
            orientation: self.orientation,
            bar: Rect::new(bounds.left(), bounds.top(), sw, sh),
            decrement,
            increment,
            track,
            track_origin,
            breadth,
            geometry: ScrollGeometry::new(self.model.range(), track_len, min_thumb),
        })
    }

    /// Paint the bar into `surface` at `bounds` for the active theme.
    ///
    /// The Scroll Channel is recessed into the surface, edged along both long
    /// sides in the theme's frame colour, and the two end buttons and the
    /// thumb are raised in it alike: each end button a plate carrying its
    /// chevron across the whole button, the thumb a rounded bar. An end button
    /// whose step would move nothing — the content already at that end — draws
    /// no plate and a dimmed chevron, so it reads as having nothing that way.
    /// A hovered or held end button and an awake thumb take the neutral
    /// `scroll_thumb_active` grey, never the accent, and a focused bar draws a
    /// reactive outline distinct from a hover. Content changes are never
    /// animated — the thumb follows the offset immediately — so the bar is
    /// already reduced-motion correct.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let Some(layout) = self.layout(bounds, scale, theme) else {
            return;
        };
        let Some((bx, by, bw, bh)) = surface_rect(bounds) else {
            return;
        };
        let palette = theme.palette();
        // The channel is recessed *into* the surface rather than raised on it,
        // so on floating chrome it takes the ground's own weight and a bar down
        // a frosted popup is not an opaque strip.
        let channel = ground_fill(theme, palette.scroll_track, ChromeLayer::Inlay);
        surface.fill_rect(bx, by, bw, bh, Color::from(channel));
        let edge = Color::from(palette.frame);
        match (self.edged, self.orientation) {
            (false, _) => {}
            (true, ScrollOrientation::Vertical) => {
                let line = plate_border(theme, scale).min(bw / 2);
                surface.fill_rect(bx, by, line, bh, edge);
                surface.fill_rect(bx + bw - line, by, line, bh, edge);
            }
            (true, ScrollOrientation::Horizontal) => {
                let line = plate_border(theme, scale).min(bh / 2);
                surface.fill_rect(bx, by, bw, line, edge);
                surface.fill_rect(bx, by + bh - line, bw, line, edge);
            }
        }

        let (dec_dir, inc_dir) = match self.orientation {
            ScrollOrientation::Vertical => (ChevronDir::Up, ChevronDir::Down),
            ScrollOrientation::Horizontal => (ChevronDir::Left, ChevronDir::Right),
        };
        self.paint_end(surface, &layout, ScrollPart::Decrement, dec_dir, theme);
        self.paint_end(surface, &layout, ScrollPart::Increment, inc_dir, theme);

        let disposition = self.state.disposition();
        let disabled = matches!(disposition, ControlDisposition::DisabledByState);
        let denied = matches!(disposition, ControlDisposition::DeniedByAuthority);
        let draggable = layout.geometry.draggable();
        let thumb_color = if disabled {
            Color::from(palette.border)
        } else if denied {
            Color::from(authority_rgba(palette, self.state.authority))
        } else if draggable && self.is_active() {
            Color::from(palette.scroll_thumb_active)
        } else {
            Color::from(palette.scroll_thumb)
        };
        if let Some(thumb) = surface_rect(layout.thumb_rect()) {
            paint_raised(surface, thumb, layout.breadth / 2, thumb_color, theme);
        }

        // Focus ring around the whole bar, distinct from a hover brighten.
        if self.state.focus.focused {
            let thickness = edge_weight(theme.contrast());
            draw_outline(
                surface,
                bx,
                by,
                bw,
                bh,
                thickness,
                Color::from(palette.rim_active),
            );
        }
    }

    /// Paint end button `part` of `layout`, its chevron pointing `dir`.
    fn paint_end(
        &self,
        surface: &mut Surface,
        layout: &BarLayout,
        part: ScrollPart,
        dir: ChevronDir,
        theme: &Theme,
    ) {
        let rect = match part {
            ScrollPart::Decrement => layout.decrement,
            _ => layout.increment,
        };
        let palette = theme.palette();
        let live = self.state.is_actionable() && self.moves(part);
        let awake = live && self.lit == Some(part);
        if live {
            let inset = layout.inset();
            let plate = Rect::new(
                rect.left() + to_i32(inset),
                rect.top() + to_i32(inset),
                rect.width.saturating_sub(inset.saturating_mul(2)),
                rect.height.saturating_sub(inset.saturating_mul(2)),
            );
            let face = Color::from(if awake {
                palette.scroll_thumb_active
            } else {
                palette.scroll_thumb
            });
            if let Some(plate) = surface_rect(plate) {
                paint_raised(surface, plate, layout.breadth / 4, face, theme);
            }
        }
        let glyph = if live {
            palette.on_surface
        } else {
            palette.on_surface_muted
        };
        paint_chevron(surface, rect, dir, Color::from(glyph));
    }
}

/// Paint one raised part of the bar — the thumb or an end button — as a
/// rounded plate of `color` at most `radius` round, rimmed under a
/// high-contrast theme so it reads without relying on its brightness against
/// the channel alone.
fn paint_raised(
    surface: &mut Surface,
    (x, y, w, h): (u32, u32, u32, u32),
    radius: u32,
    color: Color,
    theme: &Theme,
) {
    if w == 0 || h == 0 {
        return;
    }
    let radius = radius.min(w / 2).min(h / 2);
    surface.fill_round_rect(x, y, w, h, radius, color);
    if heavy_contrast(theme) {
        draw_outline(
            surface,
            x,
            y,
            w,
            h,
            1,
            Color::from(theme.palette().on_surface),
        );
    }
}
