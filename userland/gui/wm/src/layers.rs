//! The display's hardware planes: windows on layers its engine composites,
//! and the pointer on its cursor plane.
//!
//! A layered present shows a *base* — the frame, holding the desktop and every
//! window below the lowest layer, composited in software as ever — and above
//! it the windows' layers, each in two buffers the display allocated. A layer
//! is one of three kinds:
//!
//! * **Free** — a window baked alone over nothing: content, furniture, corner
//!   coverage and shadow. The engine blends whatever of it is not opaque, and
//!   moving it is a new destination, not a composite.
//! * **Frost** — beneath a frosted window's free layer, opaque, what its own
//!   pixels are blended over: its retained field, the blur over a rectangle
//!   larger than the window, with the window's shape mixed at its corners, of
//!   which the engine shows the window's rectangle. It is baked from the field,
//!   so a move inside the field re-bakes only the corners.
//! * **Flattened** — a window translucent as a whole, baked as the software
//!   composite of its footprint over everything beneath it, the window
//!   included, opaque. It is re-baked wherever anything beneath it changes.
//!
//! The back buffer holds at each pixel the composite the frame or layer
//! showing it needs: every window below the lowest layer, within a flattened
//! footprint every window up to that one, and at a frost's corners every window
//! below its own and that window's frost. These composite zones never overlap
//! and never cross a frosted window of the base, so no composite is cut
//! through a frost. The base's damage excludes every zone, which hides it, so
//! the display is never handed pixels it cannot show and no zone is encoded
//! into the frame; a zone that moves off them, or a layer given back, marks
//! them first.
//!
//! A layer's two buffers alternate: the engine shows one while what changed is
//! baked into the other, so it never reads a buffer being written. A frame
//! with nothing new presents nothing.

use alloc::vec::Vec;

use tairix_abi::driver::display::{
    AccelCaps, AccelLayer, BufferId, DamageRect, MAX_ACCEL_LAYERS, MAX_DAMAGE_RECTS,
};
use tairix_abi::DriverError;
use tairix_display::{damage_list, Base, ChannelOrder, LayerPresenter, Presenter};
use tairix_inline::ArrayVec;
use tairix_parallel::JobRunner;

use super::{
    compose_row, encode_segment, resolve_chrome, Compositor, Planned, Presentation, RowLayers,
    RowRound, RowTargets, Scanout, ShownBy, MIN_PARALLEL_BAND_PX,
};
use crate::chrome::WindowChrome;
use crate::color::{DitherRow, Pixel};
use crate::frost::{self, FrostField};
use crate::geometry::{Point, Rect, Region};
use crate::pointer::{PointerRoute, Sprite, SpriteRun, MAX_OWED, MAX_SPRITES};
use crate::shadow::ShadowKit;
use crate::surface::{blend_run, Surface};
use crate::window::{RowLayer, Window, WindowId};

/// How a window's layer is baked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Role {
    /// The window alone over nothing.
    Free,
    /// Everything beneath the window and the window, composited and opaque.
    Flattened,
    /// Everything beneath the window's rectangle with its frost applied,
    /// opaque, beneath the window's own free layer.
    Frost,
}

impl Role {
    /// Whether the layer is baked from the back buffer's composite, which is
    /// opaque across it, rather than from its window alone.
    const fn is_composite(self) -> bool {
        matches!(self, Self::Flattened | Self::Frost)
    }
}

/// How deep the frame composites a part of the screen.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Depth {
    /// Every window below this z-index.
    Below(usize),
    /// Every window below this z-index, and that window's frost.
    Frost(usize),
}

/// What a layer shows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Source {
    Window(WindowId, Role),
    Pointer,
    /// The desktop composed smaller than its output, which the engine scales
    /// onto it.
    Desktop,
}

/// What a layered present shows beneath its window layers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Shape {
    /// The frame, as the whole output.
    Based,
    /// Nothing: the one window layer covers the output.
    Promoted,
    /// The desktop's own layer, scaled onto the output.
    Desktop,
}

/// One of a layer's two buffers and what it lacks, in the layer's own pixels.
struct Held {
    id: BufferId,
    stale: Region,
}

/// The least side the pointer's layer is given: a cursor alone fits.
const POINTER_LAYER_MIN: u32 = 64;

/// The side of the pointer's buffer a length of it is cut from: a power of
/// two, so a pointer growing across a gesture is given a few buffers rather
/// than one a frame.
const fn pointer_side(length: u32) -> u32 {
    let length = if length < POINTER_LAYER_MIN {
        POINTER_LAYER_MIN
    } else {
        length
    };
    match length.checked_next_power_of_two() {
        Some(side) => side,
        None => u32::MAX,
    }
}

/// The buffers the pointer's layer at `bounds` is cut from, or `None` past
/// what `caps` lets a layer be.
fn pointer_capacity(bounds: Rect, caps: &AccelCaps) -> Option<(u32, u32)> {
    let side = |length: u32, max: u32| (length <= max).then(|| pointer_side(length).min(max));
    Some((
        side(bounds.width, caps.max_width_px)?,
        side(bounds.height, caps.max_height_px)?,
    ))
}

/// What the display refused, each kept until what was asked for changes, so
/// a refusal costs one request rather than one a frame.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Refusals {
    /// The engine refused a stack it can never take: the output stays on the
    /// composited path until its mode changes.
    stack: bool,
    /// The pointer shape the cursor plane refused.
    plane: Option<u64>,
    /// The window, at the size, the engine could not give a layer: its
    /// buffers, or room in a stack it composites at once.
    window: Option<(WindowId, (u32, u32))>,
    /// The frosted window, at its rectangle's size, the engine could not give
    /// its frost's buffers: it is flattened instead.
    frost: Option<(WindowId, (u32, u32))>,
    /// The buffers the engine could not give the pointer's layer.
    pointer: Option<(u32, u32)>,
    /// The size the engine could not give the scaled desktop's layer.
    desktop: Option<(u32, u32)>,
    /// The display holds no frame of its own to draw a frame into: one is
    /// composed into the compositor's own and handed over instead, until a
    /// new sink arrives.
    pub(super) drawn: bool,
}

impl Refusals {
    /// Nothing refused.
    pub(super) const NONE: Self = Self {
        stack: false,
        plane: None,
        window: None,
        frost: None,
        pointer: None,
        desktop: None,
        drawn: false,
    };
}

/// A layer the engine shows.
struct Layer {
    source: Source,
    /// Where the layer's pixels lie on screen.
    at: Rect,
    /// What of `at` the engine shows: all of it, but for a frost layer, which
    /// holds its window's whole field and shows the window's rectangle of it.
    shows: Rect,
    /// The corners a frost layer's buffers hold its window's shape mixed at,
    /// in screen coordinates.
    corners: [Rect; 4],
    /// Its buffers' size: a window's own, or what the pointer's is cut from.
    capacity: (u32, u32),
    held: [Held; 2],
    /// The buffer the engine shows, once one was presented.
    shown: Option<usize>,
    /// The buffer baked for the next present, while it is not yet shown.
    staged: Option<usize>,
    /// Where the layer was last presented.
    presented_at: Option<Point>,
    /// The pointer overlay's shape a pointer layer holds.
    shape: Option<u64>,
}

impl Layer {
    /// A layer of `source` at `at` showing `shows` of it, both buffers of
    /// `capacity` taken from `engine`.
    fn new(
        engine: &mut dyn LayerPresenter,
        source: Source,
        (at, shows): (Rect, Rect),
        capacity: (u32, u32),
    ) -> Result<Self, DriverError> {
        let (width, height) = capacity;
        let first = engine.alloc(width, height)?;
        let second = engine.alloc(width, height).inspect_err(|_| {
            let _ = engine.free(first);
        })?;
        let all_stale = |id| {
            let mut stale = Region::new();
            stale.add(Rect::new(0, 0, at.width, at.height));
            Held { id, stale }
        };
        Ok(Self {
            source,
            at,
            shows,
            corners: [Rect::EMPTY; 4],
            capacity,
            held: [all_stale(first), all_stale(second)],
            shown: None,
            staged: None,
            presented_at: None,
            shape: None,
        })
    }

    const fn window(&self) -> Option<WindowId> {
        match self.source {
            Source::Window(id, _) => Some(id),
            Source::Pointer | Source::Desktop => None,
        }
    }

    const fn role(&self) -> Option<Role> {
        match self.source {
            Source::Window(_, role) => Some(role),
            Source::Pointer | Source::Desktop => None,
        }
    }

    /// Whether the layer's buffers show `at` as they are: a window's exactly,
    /// the pointer's within them while they are not far outgrown.
    const fn fits(&self, at: Rect) -> bool {
        let (width, height) = self.capacity;
        match self.source {
            Source::Pointer => {
                at.width <= width
                    && at.height <= height
                    && width <= pointer_side(at.width).saturating_mul(4)
                    && height <= pointer_side(at.height).saturating_mul(4)
            }
            Source::Window(..) | Source::Desktop => width == at.width && height == at.height,
        }
    }

    fn free(self, engine: &mut dyn LayerPresenter) {
        for held in self.held {
            let _ = engine.free(held.id);
        }
    }

    /// The pixels the layer shows at screen rectangle `rect` changed.
    fn dirty(&mut self, rect: Rect) {
        let hit = rect.intersection(&self.at);
        if hit.is_empty() {
            return;
        }
        let local = Rect::new(
            hit.left().saturating_sub(self.at.left()),
            hit.top().saturating_sub(self.at.top()),
            hit.width,
            hit.height,
        );
        for held in &mut self.held {
            held.stale.add(local);
        }
    }

    /// [`dirty`](Self::dirty) for every pixel of `region`, worked out in
    /// `local`.
    fn dirty_region(&mut self, region: &Region, local: &mut Region) {
        local.clone_from(region);
        local.clip(self.at);
        local.translate(
            self.at.left().saturating_neg(),
            self.at.top().saturating_neg(),
        );
        for held in &mut self.held {
            held.stale.add_region(local);
        }
    }

    fn dirty_all(&mut self) {
        let whole = Rect::new(0, 0, self.at.width, self.at.height);
        for held in &mut self.held {
            held.stale.clear();
            held.stale.add(whole);
        }
    }

    /// The buffer the next present names.
    const fn current(&self) -> Option<usize> {
        match self.staged {
            Some(staged) => Some(staged),
            None => self.shown,
        }
    }

    /// The buffer to bake into before the next present; `None` when the one
    /// it names already holds everything. Never the buffer the engine shows.
    fn target(&self) -> Option<usize> {
        match self.current() {
            Some(current) if self.held[current].stale.is_empty() => None,
            _ => Some(self.shown.map_or(0, |shown| 1 - shown)),
        }
    }

    /// Whether a present would change what the engine shows of this layer.
    fn owes(&self) -> bool {
        self.target().is_some()
            || self.staged.is_some()
            || self.presented_at != Some(self.at.origin)
    }

    /// The layer as the engine is to show it, `output` the screen a desktop
    /// layer is scaled onto.
    fn as_accel(&self, output: (u32, u32)) -> Option<AccelLayer> {
        let held = self.held.get(self.current()?)?;
        let shows = self.shows.intersection(&self.at);
        let (dst_width_px, dst_height_px) = match self.source {
            Source::Desktop => output,
            Source::Window(..) | Source::Pointer => (shows.width, shows.height),
        };
        let offset = |from: i32, to: i32| u32::try_from(to.saturating_sub(from)).ok();
        Some(AccelLayer {
            buffer: held.id,
            src: DamageRect {
                x: offset(self.at.left(), shows.left())?,
                y: offset(self.at.top(), shows.top())?,
                width_px: shows.width,
                height_px: shows.height,
            },
            dst_x: shows.left(),
            dst_y: shows.top(),
            dst_width_px,
            dst_height_px,
            opacity: u8::MAX,
        })
    }

    /// The present naming this layer reached the display.
    fn presented(&mut self) {
        self.shown = self.current();
        self.staged = None;
        self.presented_at = Some(self.at.origin);
    }
}

/// A window's layer in a plan: how it is baked, what of the screen it shows,
/// and the rectangle its buffers hold — the same, but for a frost layer, which
/// holds its window's whole field.
#[derive(Copy, Clone)]
struct Laid {
    role: Role,
    shows: Rect,
    holds: Rect,
}

impl Laid {
    /// A layer holding exactly what it shows.
    const fn showing(role: Role, rect: Rect) -> Self {
        Self {
            role,
            shows: rect,
            holds: rect,
        }
    }
}

/// Which windows a present puts on layers, and what lies beneath them.
struct Plan {
    /// The z-index of the lowest window on a layer, below which every window
    /// is the base's.
    split: usize,
    /// The windows on layers, bottom to top, each with how it is laid.
    windows: ArrayVec<(WindowId, Laid), MAX_ACCEL_LAYERS>,
    shape: Shape,
}

/// The layers the last layered present left on the display.
pub(super) struct Stack {
    /// The window layers bottom to top, then the pointer's; or the desktop's
    /// alone.
    layers: Vec<Layer>,
    split: usize,
    shape: Shape,
    /// Damage the base owes while a promoted window covers it.
    hidden: Region,
    /// What of a frame's composed region the base shows, held for its buffer.
    shown: Region,
    /// A row a free layer is composed through before it is encoded.
    scratch: Vec<Pixel>,
    /// The layers' other vector, which each reconcile builds the next stack
    /// in, so a layered frame allocates none.
    spare: Vec<Layer>,
}

impl Stack {
    const fn new(split: usize) -> Self {
        Self {
            layers: Vec::new(),
            split,
            shape: Shape::Based,
            hidden: Region::new(),
            shown: Region::new(),
            scratch: Vec::new(),
            spare: Vec::new(),
        }
    }

    /// `composed` less every composite layer's rectangle: a composite layer is
    /// opaque across all of it, so the base beneath can never be seen there.
    /// A free layer hides nothing, since its content may be fully transparent
    /// anywhere.
    fn shown_of(&mut self, composed: &Region) -> &Region {
        let Self { layers, shown, .. } = self;
        shown.clone_from(composed);
        for layer in layers.iter() {
            if layer.role().is_some_and(Role::is_composite) {
                shown.subtract(layer.shows);
            }
        }
        shown
    }

    /// Whether `engine` holds every buffer, which a display since replaced
    /// does not.
    fn held_by(&self, engine: &dyn LayerPresenter) -> bool {
        self.layers
            .iter()
            .flat_map(|layer| &layer.held)
            .all(|held| engine.holds(held.id))
    }

    fn free(self, engine: &mut dyn LayerPresenter) {
        for layer in self.layers {
            layer.free(engine);
        }
    }
}

/// What the cursor plane was last given: the pointer's shape, where its hot
/// spot is, and where it is to go once [moved](Compositor::move_plane).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct PlaneShown {
    shape: u64,
    at: Point,
    due: Point,
}

/// What changed about a window a free layer shows.
#[derive(Copy, Clone)]
pub(super) enum Change<'a> {
    /// Its pixels, within a screen rectangle.
    Pixels(Rect),
    /// Its pixels, within a screen region.
    Region(&'a Region),
    /// Anything that may reach all of them.
    Whole,
    /// Its place, and nothing else.
    Moved,
}

impl Compositor {
    /// Take a change to window `id` as its free layer's own, damaging only the
    /// composite zones above it, which read it, and its own frost where a move
    /// or a whole change shifts it. Answers whether `id` has a free layer; any
    /// other window's change is the caller's to mark.
    pub(super) fn route_to_layer(
        &mut self,
        id: WindowId,
        change: Change<'_>,
        footprints: [Rect; 2],
    ) -> bool {
        let screen = self.screen_rect();
        let Some(stack) = self.layers.as_mut() else {
            return false;
        };
        let Some(at) = stack
            .layers
            .iter()
            .position(|layer| layer.source == Source::Window(id, Role::Free))
        else {
            return false;
        };
        let reach = self.shadow.reach();
        let placed = self
            .windows
            .iter()
            .find(|window| window.id() == id)
            .map(|window| {
                (
                    window.footprint(reach),
                    window.bounds().intersection(&screen),
                )
            });
        let footprint = placed.map(|(footprint, _)| footprint);
        let mut flattened: ArrayVec<Rect, MAX_ACCEL_LAYERS> = ArrayVec::new();
        for above in stack.layers.iter().skip(at.saturating_add(1)) {
            if above.role() == Some(Role::Flattened) {
                let _ = flattened.try_push(above.shows);
            }
        }
        let frost = stack
            .layers
            .iter()
            .find(|layer| layer.source == Source::Window(id, Role::Frost))
            .map(|layer| layer.shows);
        if let Some(layer) = stack.layers.get_mut(at) {
            match change {
                Change::Pixels(rect) => layer.dirty(rect),
                Change::Region(region) => layer.dirty_region(region, &mut self.routed),
                Change::Whole => layer.dirty_all(),
                Change::Moved => {}
            }
            // A layer of the wrong size is replaced by the next plan.
            if let Some(footprint) = footprint.filter(|footprint| layer.fits(*footprint)) {
                layer.at.origin = footprint.origin;
            }
        }
        // Every frost above reads this window's pixels, whatever layer it
        // rides, and a flattened footprint above composes them in.
        let from = self.index_of(id).map_or(0, |index| index.saturating_add(1));
        if let Change::Region(region) = change {
            let mut hit = core::mem::take(&mut self.routed);
            for &rect in region.rects() {
                self.stale_frosts_from(rect, from);
            }
            for zone in flattened {
                hit.clone_from(region);
                hit.clip(zone);
                self.damage.add_region(&hit);
            }
            self.routed = hit;
            return true;
        }
        for rect in footprints {
            self.stale_frosts_from(rect, from);
            for zone in &flattened {
                self.damage.add(rect.intersection(zone));
            }
        }
        // The frost's layer shows where its window now lies, its corners
        // composed there, and leaves the base beneath where it lay.
        if let (Some(was), Some((_, now)), Change::Moved | Change::Whole) = (frost, placed, change)
        {
            self.damage.add(was);
            self.damage.add(now);
        }
        true
    }

    /// Stale every free layer whole: the theme or density its window's
    /// furniture and shadow are baked from changed, which no window's own
    /// damage reports.
    pub(super) fn restyle_free_layers(&mut self) {
        for layer in self.layers.iter_mut().flat_map(|stack| &mut stack.layers) {
            if layer.role() == Some(Role::Free) {
                layer.dirty_all();
            }
        }
    }

    /// Whether the layers owe the display a present that the base's damage
    /// does not already ask for.
    pub(super) fn layers_owe(&self) -> bool {
        let Some(stack) = &self.layers else {
            return false;
        };
        let shape = self.pointer.shape();
        let bounds = self.pointer.sprite_bounds();
        let wants_pointer = self.pointer.route() == PointerRoute::Layer && bounds.is_some();
        let has_pointer = stack
            .layers
            .iter()
            .any(|layer| layer.source == Source::Pointer);
        wants_pointer != has_pointer
            || stack.layers.iter().any(|layer| match layer.source {
                Source::Pointer => {
                    layer.owes()
                        || layer.shape != Some(shape)
                        || bounds.is_none_or(|bounds| bounds != layer.at)
                }
                Source::Window(..) | Source::Desktop => layer.owes(),
            })
    }

    /// The depth each pixel is composited to this frame: below `.0` on the
    /// base, and each composite zone's own depth within it.
    pub(super) fn layer_depths(&self) -> (usize, ArrayVec<(Rect, Depth, usize), MAX_ACCEL_LAYERS>) {
        let mut zones = ArrayVec::new();
        let Some(stack) = self
            .layers
            .as_ref()
            .filter(|stack| stack.shape == Shape::Based)
        else {
            return (usize::MAX, zones);
        };
        for (at, layer) in stack.layers.iter().enumerate() {
            let Source::Window(id, role) = layer.source else {
                continue;
            };
            let Some(index) = self.index_of(id) else {
                continue;
            };
            let depth = match role {
                Role::Free => continue,
                Role::Flattened => Depth::Below(index.saturating_add(1)),
                Role::Frost => Depth::Frost(index),
            };
            let _ = zones.try_push((layer.shows, depth, at));
        }
        (stack.split, zones)
    }

    /// Whether the engine scales the desktop onto the output, so the frame
    /// is not resampled.
    pub(super) fn scaled_in_hardware(&self) -> bool {
        self.layers
            .as_ref()
            .is_some_and(|stack| stack.shape == Shape::Desktop)
    }

    /// Give the layers back, owing the frame everything they showed: the next
    /// composite draws those windows and the pointer into it again. `engine`
    /// is the display that allocated them, or `None` where it is gone.
    pub(super) fn leave_layers(&mut self, engine: Option<&mut dyn LayerPresenter>) {
        // No layer carries the pointer from here, whether or not one did.
        if self.pointer.route() == PointerRoute::Layer {
            self.pointer.set_route(PointerRoute::Frame);
        }
        let Some(mut stack) = self.layers.take() else {
            return;
        };
        let screen = self.screen_rect();
        let scaled = stack.shape == Shape::Desktop;
        for layer in &stack.layers {
            self.mark_left(layer, screen);
        }
        self.damage.add_region(&stack.hidden);
        stack.hidden.clear();
        if scaled {
            // The frame was never resampled while the engine scaled.
            self.damage.add(screen);
        }
        if let Some(engine) = engine {
            stack.free(engine);
        }
    }

    /// Owe the frame what `layer` showed now that it is the frame's again:
    /// where the layer lay, and where its window now lies, which a resize
    /// the layer never followed may have moved.
    fn mark_left(&mut self, layer: &Layer, screen: Rect) {
        self.damage.add(layer.shows.intersection(&screen));
        if let Some(id) = layer.window() {
            let reach = self.shadow.reach();
            if let Some(window) = self.windows.iter().find(|window| window.id() == id) {
                self.damage
                    .add(window.footprint(reach).intersection(&screen));
            }
        }
    }

    /// Forget layers `presenter` does not hold: the display was replaced, and
    /// the buffers died with the configuration that allocated them, so the
    /// new one is owed everything they showed.
    pub(super) fn drop_layers_of_another(&mut self, presenter: &mut dyn Presenter) {
        let held = match (self.layers.as_ref(), presenter.layers()) {
            (None, _) => true,
            (Some(stack), Some(engine)) => stack.held_by(engine),
            (Some(stack), None) => stack.layers.is_empty(),
        };
        if !held {
            self.leave_layers(None);
            self.owe_output();
        }
    }

    /// Whether this frame may go to `presenter`'s layer engine at all.
    pub(super) fn layers_wanted(&self, presenter: &mut dyn Presenter) -> bool {
        if self.reveal != u8::MAX || self.refused.stack {
            return false;
        }
        presenter.layers().is_some_and(|engine| {
            let caps = engine.caps();
            caps.max_layers >= 2 && (self.scaled.is_none() || caps.hw_scaling)
        })
    }

    /// Present through the display's layer engine, answering whether it did.
    /// `false` leaves the frame for the caller to present, the layers given
    /// back.
    ///
    /// A stack the engine cannot composite at once loses its lowest window to
    /// the base, which is remembered as that window's refusal, and the frame
    /// is planned again with the layers still held: one fewer each pass.
    ///
    /// # Errors
    ///
    /// The engine's refusal, other than of a stack it can never take: that
    /// keeps the output on the composited path instead.
    pub(super) fn present_layered(
        &mut self,
        presenter: &mut dyn Presenter,
    ) -> Result<bool, DriverError> {
        let Some(engine) = presenter.layers() else {
            self.leave_layers(None);
            return Ok(false);
        };
        let caps = engine.caps();
        let pointer_layer =
            self.pointer.route() == PointerRoute::Layer && self.pointer.sprite_bounds().is_some();
        for _ in 0..=MAX_ACCEL_LAYERS {
            let Some(plan) = self.plan_layers(&caps, pointer_layer) else {
                break;
            };
            // A frost layer is baked from its field, so a window whose field
            // cannot be had where the plan lays it is flattened instead.
            if let Some(refused) = self.fit_frost_fields(&plan) {
                self.refused.frost = Some(refused);
                continue;
            }
            if !self.reconcile(engine, &caps, &plan, pointer_layer) {
                break;
            }
            match self.present_plan(engine, &plan) {
                Ok(()) => return Ok(true),
                // What could not be had for this frame may be had for the
                // next: the frame carries the scene this once.
                Err(DriverError::OutOfMemory) => break,
                Err(DriverError::OutOfRange) if plan.shape != Shape::Desktop => {
                    let Some((id, at)) = self.lowest_layered_window() else {
                        self.refused.stack = true;
                        break;
                    };
                    self.refused.window = Some((id, (at.width, at.height)));
                }
                Err(
                    DriverError::LengthOutOfRange
                    | DriverError::Unsupported
                    | DriverError::OutOfRange,
                ) => {
                    // A stack this engine will never take: the frame carries
                    // the scene from now on.
                    self.refused.stack = true;
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        self.leave_layers(Some(engine));
        Ok(false)
    }

    /// The lowest window on a layer, and where its layer lies.
    fn lowest_layered_window(&self) -> Option<(WindowId, Rect)> {
        self.layers
            .as_ref()?
            .layers
            .iter()
            .find_map(|layer| Some((layer.window()?, layer.at)))
    }

    /// Compose what this frame changed, bake it into the layers `plan` was
    /// reconciled to and present them, over the base where the plan has one.
    /// A refused present leaves what it composited owed to the next.
    ///
    /// # Errors
    ///
    /// The engine's refusal, or [`DriverError::OutOfMemory`] where a new
    /// layer could not be baked, which would leave its window out of the
    /// frame.
    fn present_plan(
        &mut self,
        engine: &mut dyn LayerPresenter,
        plan: &Plan,
    ) -> Result<(), DriverError> {
        let mut encoded = core::mem::take(&mut self.composed);
        encoded.clear();
        let planned = if plan.shape == Shape::Promoted {
            self.hide_base_damage();
            None
        } else {
            Some(self.plan_composite(&mut encoded))
        };
        let mut changed = core::mem::take(&mut self.undelivered);
        changed.clip(self.output_rect());
        // What was owed is in the back buffer already; what the plan composes
        // reaches a flattened layer as it is composed.
        self.dirty_flattened(&changed);
        changed.add_region(&encoded);
        if let Some(planned) = &planned {
            self.compose_zones(planned, Some(&mut *engine));
            // Beneath no base, the desktop's layer is baked from the back
            // buffer, which must hold the whole plan first.
            if plan.shape != Shape::Based {
                self.compose_heap(planned);
            }
        }
        let shown = self.bake_and_show(engine, plan, planned.as_ref(), (&encoded, &changed));
        self.composed = encoded;
        if shown.is_ok() {
            changed.clear();
        }
        self.undelivered = changed;
        shown
    }

    /// Bake every layer's changed pixels and show the stack, over the base
    /// `planned` plans where the plan has one.
    fn bake_and_show(
        &mut self,
        engine: &mut dyn LayerPresenter,
        plan: &Plan,
        planned: Option<&Planned>,
        (encoded, changed): (&Region, &Region),
    ) -> Result<(), DriverError> {
        let based = planned.filter(|_| plan.shape == Shape::Based);
        let layers = self.bake_composited(engine, changed).and_then(|()| {
            self.bake_free(engine);
            self.bake_pointer(engine);
            self.accel_layers()
        });
        let layers = match layers {
            Ok(layers) => layers,
            Err(err) => {
                // Composed all the same, for the frame that carries the scene
                // instead.
                if let Some(planned) = based {
                    self.compose_frame(planned, None);
                }
                return Err(err);
            }
        };
        self.stats.bump_present();
        match based {
            Some(planned) => self.present_based(engine, planned, (encoded, changed), &layers)?,
            None => engine.present_layers(None, &layers)?,
        }
        self.stats
            .set_layers(layers.len().saturating_add(usize::from(based.is_some())));
        for layer in self.layers.iter_mut().flat_map(|stack| &mut stack.layers) {
            layer.presented();
        }
        self.presented = Some(match (plan.shape, plan.windows.last()) {
            (Shape::Promoted, Some(&(id, _))) => Presentation::Promoted(id),
            _ => Presentation::Layered,
        });
        Ok(())
    }

    /// The layers as the engine is to show them, or
    /// [`DriverError::OutOfMemory`] where one has no buffer baked, which would
    /// leave its window out of the frame.
    fn accel_layers(&self) -> Result<ArrayVec<AccelLayer, MAX_ACCEL_LAYERS>, DriverError> {
        let output = (self.mode.width_px, self.mode.height_px);
        let mut layers = ArrayVec::new();
        for layer in self.layers.iter().flat_map(|stack| &stack.layers) {
            let accel = layer.as_accel(output).ok_or(DriverError::OutOfMemory)?;
            let _ = layers.try_push(accel);
        }
        Ok(layers)
    }

    /// Show `layers` over the base `planned` plans, of which `changed` changed
    /// and its composite encodes `encoded`: drawn straight into the display's
    /// own back frame where it holds one, and otherwise composed into the
    /// compositor's own frame and handed over. Whatever the engine does, the
    /// base is composed.
    fn present_based(
        &mut self,
        engine: &mut dyn LayerPresenter,
        planned: &Planned,
        (encoded, changed): (&Region, &Region),
        layers: &[AccelLayer],
    ) -> Result<(), DriverError> {
        let mut shown = self.layers.as_mut().map_or_else(Region::new, |stack| {
            stack.shown_of(changed);
            core::mem::take(&mut stack.shown)
        });
        let result = self.draw_base(engine, planned, (encoded, &shown), layers);
        shown.clear();
        if let Some(stack) = self.layers.as_mut() {
            stack.shown = shown;
        }
        result
    }

    /// Show `layers` over the base `planned` plans, of which `shown` is what
    /// the base shows of what changed: drawn into the display's own back
    /// frame unless it holds none, and otherwise composed into the
    /// compositor's own frame and handed over.
    fn draw_base(
        &mut self,
        engine: &mut dyn LayerPresenter,
        planned: &Planned,
        (encoded, shown): (&Region, &Region),
        layers: &[AccelLayer],
    ) -> Result<(), DriverError> {
        if !self.refused.drawn {
            let mut handed = false;
            let drawn = {
                let mut lay = |bytes: &mut [u8], stride: u32, refresh: &Region| {
                    handed = true;
                    self.draw_frame(planned, encoded, (bytes, stride, refresh));
                    Ok(())
                };
                let base = Base::Drawn {
                    damage: shown,
                    draw: &mut lay,
                };
                engine.present_layers(Some(base), layers)
            };
            match (drawn, handed) {
                (result, true) => {
                    self.heap_behind = true;
                    return result;
                }
                (Err(DriverError::Unsupported), false) => self.refused.drawn = true,
                (result, false) => {
                    self.compose_frame(planned, None);
                    return result;
                }
            }
        }
        self.compose_heap(planned);
        let whole = [DamageRect::full(&self.mode)];
        let mut list = [whole[0]; MAX_DAMAGE_RECTS];
        let damage = if shown.is_empty() {
            &[][..]
        } else {
            damage_list(shown, &self.mode, &mut list).unwrap_or(&whole)
        };
        let base = Base::Frame {
            frame: &self.frame,
            damage,
        };
        engine.present_layers(Some(base), layers)
    }

    /// Compose `piece` of the composite layer at `layer` in the stack to
    /// `depth`. A flattened layer's piece is staled in both its buffers and
    /// encoded straight into the one it shows next, which `engine` holds; a
    /// frost layer's is left in the back buffer, which its corners are baked
    /// from.
    pub(super) fn compose_zone_piece(
        &mut self,
        (piece, depth, layer): (Rect, Depth, usize),
        planned: &Planned,
        hits: &mut Vec<usize>,
        engine: Option<&mut (dyn LayerPresenter + '_)>,
    ) {
        let shown_by = ShownBy::Layer(depth);
        let laid = match depth {
            Depth::Below(_) => self
                .layers
                .as_mut()
                .and_then(|stack| stack.layers.get_mut(layer))
                .and_then(|laid| {
                    laid.dirty(piece);
                    let target = laid.target()?;
                    Some((target, laid.held.get(target)?.id, laid.at))
                }),
            Depth::Frost(_) => None,
        };
        let held = match (laid, engine) {
            (Some((target, id, at)), Some(engine)) => {
                engine.pixels(id).map(|pixels| (target, at, pixels))
            }
            _ => None,
        };
        let Some((target, at, (bytes, stride))) = held else {
            self.recompose_piece(piece, shown_by, planned, hits, None);
            return;
        };
        let mut scanout = Scanout {
            bytes,
            stride: usize::try_from(stride).unwrap_or(usize::MAX),
            origin: at.origin,
            reveal: u8::MAX,
        };
        self.recompose_piece(piece, shown_by, planned, hits, Some(&mut scanout));
        let local = Rect::new(
            piece.left().saturating_sub(at.left()),
            piece.top().saturating_sub(at.top()),
            piece.width,
            piece.height,
        );
        let held = self
            .layers
            .as_mut()
            .and_then(|stack| stack.layers.get_mut(layer))
            .and_then(|laid| laid.held.get_mut(target));
        if let Some(held) = held {
            held.stale.subtract(local);
        }
    }

    /// Stale what of `region` each flattened layer holds, for its bake to
    /// encode from the back buffer.
    fn dirty_flattened(&mut self, region: &Region) {
        let Self { layers, routed, .. } = self;
        for layer in layers.iter_mut().flat_map(|stack| &mut stack.layers) {
            if layer.role() == Some(Role::Flattened) {
                layer.dirty_region(region, routed);
            }
        }
    }

    /// Which windows this frame puts on layers, or `None` when the scene is
    /// not one the engine can take.
    fn plan_layers(&self, caps: &AccelCaps, pointer_layer: bool) -> Option<Plan> {
        let fits = |rect: Rect| {
            !rect.is_empty() && rect.width <= caps.max_width_px && rect.height <= caps.max_height_px
        };
        if self.scaled.is_some() {
            let screen = self.screen_rect();
            let refused = self.refused.desktop == Some((screen.width, screen.height));
            return (fits(screen) && fits(self.output_rect()) && !refused).then(|| Plan {
                split: self.windows.len(),
                windows: ArrayVec::new(),
                shape: Shape::Desktop,
            });
        }
        if pointer_layer {
            let capacity = self
                .pointer
                .sprite_bounds()
                .and_then(|bounds| pointer_capacity(bounds, caps))?;
            if self.refused.pointer == Some(capacity) {
                return None;
            }
        }
        let refused = |window: &Window, at: Rect| {
            self.refused.window == Some((window.id(), (at.width, at.height)))
        };
        let room = usize::try_from(caps.max_layers)
            .unwrap_or(usize::MAX)
            .min(MAX_ACCEL_LAYERS)
            .checked_sub(1 + usize::from(pointer_layer))
            .filter(|room| *room > 0)?;
        let reach = self.shadow.reach();
        let screen = self.screen_rect();
        if let Some(cover) = self.fullscreen_cover() {
            let footprint = cover.footprint(reach);
            if Self::promotable(cover) && fits(footprint) && !refused(cover, footprint) {
                let mut windows = ArrayVec::new();
                let _ = windows.try_push((cover.id(), Laid::showing(Role::Free, footprint)));
                return Some(Plan {
                    split: self.index_of(cover.id())?,
                    windows,
                    shape: Shape::Promoted,
                });
            }
        }
        let mut chosen: ArrayVec<(usize, Laid), MAX_ACCEL_LAYERS> = ArrayVec::new();
        'windows: for (index, window) in self.windows.iter().enumerate().rev() {
            if !window.is_visible() {
                continue;
            }
            let left = room.saturating_sub(chosen.len());
            let mut laid = self.laid(window, (reach, screen), left);
            // Planes the display could not give buffers, or a field larger than
            // a layer may be, leave the window flattened, as one layer.
            let frost_refused = self.refused.frost.is_some()
                && self.refused.frost == self.frost_refusal(window.id());
            if laid.len() > 1
                && laid.iter().any(|laid| {
                    refused(window, laid.holds)
                        || (laid.role == Role::Frost && (frost_refused || !fits(laid.holds)))
                })
            {
                laid = self.laid(window, (reach, screen), 1);
            }
            if laid.len() > left {
                break;
            }
            for laid in &laid {
                let overlaps_zone = laid.role.is_composite()
                    && chosen.iter().any(|(_, other)| {
                        other.role.is_composite()
                            && !other.shows.intersection(&laid.shows).is_empty()
                    });
                if !fits(laid.holds) || overlaps_zone || refused(window, laid.holds) {
                    break 'windows;
                }
            }
            for laid in laid {
                let _ = chosen.try_push((index, laid));
            }
        }
        self.trim_crossings(&mut chosen, screen);
        let split = chosen.last().map(|&(index, _)| index)?;
        let mut windows = ArrayVec::new();
        for &(index, laid) in chosen.iter().rev() {
            let _ = windows.try_push((self.windows.get(index)?.id(), laid));
        }
        Some(Plan {
            split,
            windows,
            shape: Shape::Based,
        })
    }

    /// Leave out of `chosen` every window whose composite zone crosses a
    /// blurred window of the base, and everything beneath it: that blur's
    /// composite would otherwise be cut through. A window's layers go
    /// together, so its pixels never go on a layer without their frost.
    fn trim_crossings(&self, chosen: &mut ArrayVec<(usize, Laid), MAX_ACCEL_LAYERS>, screen: Rect) {
        while let Some(crossing) = {
            let split = chosen.last().map_or(0, |&(index, _)| index);
            chosen.iter().position(|(_, laid)| {
                laid.role.is_composite()
                    && self.windows.iter().take(split).any(|below| {
                        let blurred = below.bounds().intersection(&screen);
                        let inside = blurred.intersection(&laid.shows);
                        below.is_visible()
                            && below.blur_radius() > 0
                            && !inside.is_empty()
                            && inside != blurred
                    })
            })
        } {
            let window = chosen.get(crossing).map(|&(index, _)| index);
            let first = chosen
                .iter()
                .position(|&(index, _)| Some(index) == window)
                .unwrap_or(crossing);
            chosen.truncate(first);
        }
    }

    /// The layers `window` would be laid on, top first, where `left` remain;
    /// `reach` is the shadow's and `screen` the output's rectangle.
    ///
    /// A window that reads no backdrop is alone over nothing, its translucency
    /// the engine's to blend whether or not that blend dithers (`plans/GPU.md`
    /// GD3). A frosted one whose blur is retained is its own pixels over its
    /// frost where two layers remain, the frost's buffers holding its whole
    /// field, so a move inside the field rewrites only its corners (GD9); a
    /// window translucent as a whole, a frosted one given a single layer, and
    /// one whose blur is not retained are composited with what lies beneath.
    fn laid(
        &self,
        window: &Window,
        (reach, screen): (u32, Rect),
        left: usize,
    ) -> ArrayVec<Laid, 2> {
        let footprint = window.footprint(reach);
        let shows = window.bounds().intersection(&screen);
        let whole = window.opacity() == u8::MAX;
        let mut laid = ArrayVec::new();
        // Wholly off screen, a frosted window has no frost to show.
        if !window.reads_backdrop() || (whole && shows.is_empty()) {
            let _ = laid.try_push(Laid::showing(Role::Free, footprint));
        } else if whole && window.is_retained() && left >= 2 {
            let _ = laid.try_push(Laid::showing(Role::Free, footprint));
            let _ = laid.try_push(Laid {
                role: Role::Frost,
                shows,
                holds: self.wanted_field(window),
            });
        } else {
            let _ = laid.try_push(Laid::showing(
                Role::Flattened,
                footprint.intersection(&screen),
            ));
        }
        laid
    }

    /// Whether `window` may be the one layer of a promoted present, which has
    /// nothing beneath it: only where every pixel it shows is opaque.
    fn promotable(window: &Window) -> bool {
        !window.reads_backdrop() && window.is_opaque() == Some(true)
    }

    /// Bring the stack to `plan`, damaging the base wherever a window joined
    /// or left it or a footprint's depth changed.
    ///
    /// Buffers are taken from the top down — the pointer's, then each window
    /// from the highest — so a window the display cannot give buffers joins
    /// the base with everything beneath it, the layers above it are kept, and
    /// the refusal is remembered until that window's size changes. `false`
    /// when not one window's layer, or the pointer's, could be had.
    fn reconcile(
        &mut self,
        engine: &mut dyn LayerPresenter,
        caps: &AccelCaps,
        plan: &Plan,
        pointer_layer: bool,
    ) -> bool {
        let screen = self.screen_rect();
        let mut stack = self
            .layers
            .take()
            .unwrap_or_else(|| Stack::new(self.windows.len()));
        let mut kept = core::mem::take(&mut stack.spare);
        kept.clear();
        let pointer = pointer_layer
            .then(|| self.pointer.sprite_bounds())
            .flatten()
            .and_then(|bounds| {
                let capacity = pointer_capacity(bounds, caps)?;
                Some((Source::Pointer, Laid::showing(Role::Free, bounds), capacity))
            });
        let desktop = (plan.shape == Shape::Desktop).then(|| {
            let at = self.screen_rect();
            (
                Source::Desktop,
                Laid::showing(Role::Free, at),
                (at.width, at.height),
            )
        });
        let windows = plan.windows.iter().rev().map(|&(id, laid)| {
            let capacity = (laid.holds.width, laid.holds.height);
            (Source::Window(id, laid.role), laid, capacity)
        });
        let mut placed = false;
        let mut whole = kept
            .try_reserve(plan.windows.len().saturating_add(1))
            .is_ok();
        for (source, laid, capacity) in pointer.into_iter().chain(desktop).chain(windows) {
            if !whole {
                break;
            }
            let Laid {
                holds: at, shows, ..
            } = laid;
            let held = stack
                .layers
                .iter()
                .position(|layer| layer.source == source && layer.fits(at));
            let layer = if let Some(position) = held {
                let mut layer = stack.layers.swap_remove(position);
                if layer.at.origin != at.origin
                    && matches!(source, Source::Window(_, role) if role.is_composite())
                {
                    layer.dirty_all();
                }
                layer.at = at;
                layer.shows = shows;
                layer
            } else if let Ok(layer) = Layer::new(engine, source, (at, shows), capacity) {
                if let Source::Window(..) = source {
                    // The window leaves the base, or its footprint starts to
                    // be composited to another depth.
                    self.damage.add(shows.intersection(&screen));
                }
                layer
            } else {
                if let Source::Window(id, _) = source {
                    // A window's layers are had together or not at all, so its
                    // pixels never go on a layer without their frost.
                    while let Some(at) = kept.iter().position(|layer| layer.window() == Some(id)) {
                        stack.layers.push(kept.remove(at));
                    }
                    placed = kept
                        .iter()
                        .any(|layer| matches!(layer.source, Source::Window(..) | Source::Desktop));
                }
                self.refuse(source, capacity);
                // A window refused buffers takes everything beneath it into
                // the base; anything else leaves no stack to show.
                whole =
                    placed && matches!(source, Source::Window(..)) && plan.shape == Shape::Based;
                break;
            };
            placed |= matches!(layer.source, Source::Window(..) | Source::Desktop);
            kept.push(layer);
        }
        if !whole || !placed {
            stack.layers.append(&mut kept);
            stack.spare = kept;
            self.layers = Some(stack);
            return false;
        }
        // What no longer has a layer of its own joins the base.
        for layer in stack.layers.drain(..) {
            self.mark_left(&layer, screen);
            layer.free(engine);
        }
        if stack.shape == Shape::Promoted && plan.shape != Shape::Promoted {
            self.damage.add_region(&stack.hidden);
            stack.hidden.clear();
        }
        // Taken top down; the stack holds them bottom up, the pointer's last.
        kept.reverse();
        stack.split = match plan.shape {
            Shape::Based => kept
                .iter()
                .find_map(Layer::window)
                .and_then(|id| self.index_of(id))
                .unwrap_or(plan.split),
            Shape::Promoted | Shape::Desktop => plan.split,
        };
        stack.spare = core::mem::replace(&mut stack.layers, kept);
        stack.shape = plan.shape;
        self.layers = Some(stack);
        true
    }

    /// Remember that the engine could not give `source` buffers of
    /// `capacity`, so it is not asked again until what is asked for changes.
    fn refuse(&mut self, source: Source, capacity: (u32, u32)) {
        match source {
            Source::Window(id, Role::Frost) => self.refused.frost = self.frost_refusal(id),
            Source::Window(id, _) => self.refused.window = Some((id, capacity)),
            Source::Pointer => self.refused.pointer = Some(capacity),
            Source::Desktop => self.refused.desktop = Some(capacity),
        }
    }

    /// The refusal of a frost layer to the window `id`, kept until the
    /// window's rectangle changes size: its field's, which a move can change,
    /// would have it asked again every time.
    fn frost_refusal(&self, id: WindowId) -> Option<(WindowId, (u32, u32))> {
        let window = self.windows.iter().find(|window| window.id() == id)?;
        let bounds = window.bounds();
        Some((id, (bounds.width, bounds.height)))
    }

    /// Hold the base's damage while a promoted window covers it, so a frame
    /// nothing can see composites nothing and the desktop settles.
    fn hide_base_damage(&mut self) {
        let screen = self.screen_rect();
        let mut settled: ArrayVec<Rect, MAX_OWED> = ArrayVec::new();
        self.pointer.settle(|rect| {
            let _ = settled.try_push(rect);
        });
        let mut owed = core::mem::take(&mut self.damage);
        let mut undelivered = core::mem::take(&mut self.undelivered);
        let mut rescan = core::mem::take(&mut self.scanout);
        if let Some(stack) = self.layers.as_mut() {
            for owing in [&mut owed, &mut undelivered, &mut rescan] {
                owing.clip(screen);
                stack.hidden.add_region(owing);
            }
            for rect in &settled {
                stack.hidden.add(rect.intersection(&screen));
            }
        }
        owed.clear();
        undelivered.clear();
        rescan.clear();
        self.damage = owed;
        self.undelivered = undelivered;
        self.scanout = rescan;
    }

    /// Bake every composite layer's changed pixels: a flattened footprint and
    /// the scaled desktop from the back buffer `composed` was composed into,
    /// and a frost layer from its window's field, its corners from the back
    /// buffer.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] where a frost layer's field is no longer
    /// retained, which leaves nothing to bake it from: the frame carries the
    /// scene instead.
    fn bake_composited(
        &mut self,
        engine: &mut dyn LayerPresenter,
        composed: &Region,
    ) -> Result<(), DriverError> {
        let epoch = self.frost_epoch();
        let Self {
            layers,
            back,
            order,
            runner,
            routed,
            frost,
            windows,
            ..
        } = self;
        for layer in layers.iter_mut().flat_map(|stack| &mut stack.layers) {
            match layer.source {
                Source::Window(id, Role::Frost) => {
                    let Some(window) = windows.iter().find(|window| window.id() == id) else {
                        continue;
                    };
                    let field = frost
                        .peek(&epoch, &id)
                        .filter(|field| field.rect() == layer.at)
                        .ok_or(DriverError::OutOfMemory)?;
                    bake_frost(
                        layer,
                        window,
                        field,
                        back,
                        composed,
                        engine,
                        (*order, *runner),
                    );
                }
                Source::Desktop => {
                    layer.dirty_region(composed, routed);
                    bake_stale(layer, back, engine, (*order, *runner));
                }
                // Staled, and its composed pixels encoded, as its zone was
                // composed.
                Source::Window(_, Role::Flattened) => {
                    bake_stale(layer, back, engine, (*order, *runner));
                }
                Source::Window(_, Role::Free) | Source::Pointer => {}
            }
        }
        Ok(())
    }

    /// Stale what a frost layer of the window `id` holds of `rect`, which its
    /// field has just blurred again.
    pub(super) fn dirty_frost_layer(&mut self, id: WindowId, rect: Rect) {
        for layer in self.layers.iter_mut().flat_map(|stack| &mut stack.layers) {
            if layer.source == Source::Window(id, Role::Frost) {
                layer.dirty(rect);
            }
        }
    }

    /// Bring the field of every window `plan` lays on a frost layer to the
    /// rectangle the layer holds, answering the first window, and the size,
    /// whose field could not be had there.
    fn fit_frost_fields(&mut self, plan: &Plan) -> Option<(WindowId, (u32, u32))> {
        let epoch = self.frost_epoch();
        for &(id, laid) in &plan.windows {
            if laid.role != Role::Frost {
                continue;
            }
            let Some(radius) = self
                .windows
                .iter()
                .find(|window| window.id() == id)
                .map(|window| self.blur_radius_px(window))
            else {
                continue;
            };
            let held = self
                .frost
                .peek(&epoch, &id)
                .filter(|field| field.radius_px() == radius)
                .map(FrostField::rect);
            if held != Some(laid.holds) {
                self.fit_field(id, radius, held, laid.holds);
            }
            let fitted = self
                .frost
                .peek(&epoch, &id)
                .is_some_and(|field| field.radius_px() == radius && field.rect() == laid.holds);
            if !fitted {
                return self.frost_refusal(id);
            }
        }
        None
    }

    /// Bake every free layer's changed pixels from its window alone.
    fn bake_free(&mut self, engine: &mut dyn LayerPresenter) {
        let mut wanted: ArrayVec<WindowId, MAX_ACCEL_LAYERS> = ArrayVec::new();
        for layer in self.layers.iter().flat_map(|stack| &stack.layers) {
            if let (Some(id), Some(Role::Free), Some(_)) =
                (layer.window(), layer.role(), layer.target())
            {
                let _ = wanted.try_push(id);
            }
        }
        if wanted.is_empty() {
            return;
        }
        let fallback = self.ensure_chrome(|window| wanted.contains(&window.id()));
        self.ensure_shadow_tiles();
        let epoch = self.chrome_epoch();
        let Self {
            layers,
            windows,
            chrome,
            shadow,
            order,
            ..
        } = self;
        let Some(stack) = layers.as_mut() else {
            return;
        };
        let Stack {
            layers: held_layers,
            scratch,
            ..
        } = stack;
        for layer in held_layers.iter_mut() {
            let (Some(id), Some(Role::Free), Some(target)) =
                (layer.window(), layer.role(), layer.target())
            else {
                continue;
            };
            let Some(window) = windows.iter().find(|window| window.id() == id) else {
                continue;
            };
            let Some((bytes, stride)) = engine.pixels(layer.held[target].id) else {
                continue;
            };
            let at = layer.at;
            let source = (window, resolve_chrome(chrome, &epoch, id, &fallback));
            let held = &mut layer.held[target];
            // A bake the scratch row could not be had for leaves the buffer
            // stale and unstaged, for the next frame to finish.
            if held.stale.rects().iter().all(|&local| {
                bake_window(source, shadow, at, local, (bytes, stride), *order, scratch)
            }) {
                held.stale.clear();
                layer.staged = Some(target);
            }
        }
    }

    /// Follow the pointer with its layer, baking the layer again only where
    /// the pointer's shape changed.
    fn bake_pointer(&mut self, engine: &mut dyn LayerPresenter) {
        let shape = self.pointer.shape();
        let Some(bounds) = self.pointer.sprite_bounds() else {
            return;
        };
        let sprites = self.pointer.sprites();
        let order = self.order;
        let Some(stack) = self.layers.as_mut() else {
            return;
        };
        let Stack {
            layers, scratch, ..
        } = stack;
        let Some(layer) = layers
            .iter_mut()
            .find(|layer| layer.source == Source::Pointer && layer.fits(bounds))
        else {
            return;
        };
        layer.at = bounds;
        if layer.shape != Some(shape) {
            layer.dirty_all();
            layer.shape = Some(shape);
        }
        let Some(target) = layer.target() else {
            return;
        };
        let Some((bytes, stride)) = engine.pixels(layer.held[target].id) else {
            return;
        };
        if bake_sprites(&sprites, bounds, (bytes, stride), order, scratch) {
            layer.held[target].stale.clear();
            layer.staged = Some(target);
        }
    }

    /// Give the pointer to the display's cursor plane where it can ride it,
    /// and take it back where it cannot. `layered` says whether the frame
    /// goes to a layer engine, where a pointer off the plane has a layer of
    /// its own when a plan can carry one.
    ///
    /// The plane is only told where the pointer is due, and goes there on
    /// [`move_plane`](Self::move_plane), so a frame carrying what the pointer
    /// drags can be handed over first; a new shape is shown where the old one
    /// stood, or where the pointer is when the plane showed nothing.
    ///
    /// A plane that refuses keeps the pointer off it until the pointer's
    /// shape changes: the frame shows it instead, and a lost seat is the
    /// present's to report. Nor does the plane carry it through a screen
    /// fade, which only the frame's pixels follow.
    pub(super) fn route_pointer(&mut self, presenter: &mut dyn Presenter, layered: bool) {
        let scaled = self.scaled.is_some();
        let faded = self.reveal != u8::MAX;
        let own_layer = layered
            && !scaled
            && self.pointer.sprite_bounds().is_some()
            && presenter
                .layers()
                .is_some_and(|engine| self.plan_layers(&engine.caps(), true).is_some());
        let off_plane = if own_layer {
            PointerRoute::Layer
        } else {
            PointerRoute::Frame
        };
        let Some(plane) = presenter.cursor() else {
            self.plane = None;
            self.pointer.set_route(off_plane);
            return;
        };
        let caps = plane.caps();
        let shape = self.pointer.shape();
        let cursor = (!scaled && !faded && self.refused.plane != Some(shape))
            .then(|| {
                self.pointer
                    .plane_cursor((caps.max_width_px, caps.max_height_px))
            })
            .flatten();
        let Some((image, at)) = cursor else {
            self.pointer.set_route(off_plane);
            if self.plane.take().is_some() {
                let _ = plane.hide();
            }
            return;
        };
        let stands = self.plane.map_or(at, |shown| shown.at);
        let fresh = self.plane.is_none_or(|shown| shown.shape != shape) || !plane.showing();
        let given = if fresh {
            let (width, height, hot) = (image.width(), image.height(), image.hotspot());
            encode_cursor(image.surface(), self.order, &mut self.plane_bytes)
                .ok_or(DriverError::OutOfMemory)
                .and_then(|stride| {
                    plane.show(
                        &self.plane_bytes,
                        width,
                        height,
                        stride,
                        u32::try_from(hot.x).unwrap_or(0),
                        u32::try_from(hot.y).unwrap_or(0),
                    )
                })
                .and_then(|()| plane.move_to(stands.x, stands.y))
        } else {
            Ok(())
        };
        if given.is_ok() {
            self.plane = Some(PlaneShown {
                shape,
                at: stands,
                due: at,
            });
            self.pointer.set_route(PointerRoute::Plane);
        } else {
            self.refused.plane = Some(shape);
            self.plane = None;
            self.pointer.set_route(off_plane);
            let _ = plane.hide();
        }
    }

    /// Bring the cursor plane to where the pointer was last routed. A plane
    /// that refuses the move gives the pointer back to the frame, which the
    /// next composite draws it into.
    pub(super) fn move_plane(&mut self, presenter: &mut dyn Presenter) {
        let Some(shown) = self.plane.filter(|shown| shown.at != shown.due) else {
            return;
        };
        let Some(plane) = presenter.cursor() else {
            self.plane = None;
            self.pointer.set_route(PointerRoute::Frame);
            return;
        };
        if plane.move_to(shown.due.x, shown.due.y).is_ok() {
            self.plane = Some(PlaneShown {
                at: shown.due,
                ..shown
            });
        } else {
            self.refused.plane = Some(shown.shape);
            self.plane = None;
            self.pointer.set_route(PointerRoute::Frame);
            let _ = plane.hide();
        }
    }

    /// Whether windows may go on layers with the pointer where it is: on the
    /// plane or a layer of its own, inside a desktop the engine scales, or
    /// not drawn at all — never in the frame beneath them.
    pub(super) fn pointer_allows_layers(&self) -> bool {
        self.scaled.is_some()
            || self.pointer.route() != PointerRoute::Frame
            || self.pointer.sprite_bounds().is_none()
    }
}

/// Encode a cursor image into `bytes` in the output's order, answering the
/// row stride, or `None` when the heap refuses the room.
fn encode_cursor(image: &Surface, order: ChannelOrder, bytes: &mut Vec<u8>) -> Option<u32> {
    let stride = image.width().checked_mul(4).filter(|stride| *stride != 0)?;
    let len = usize::try_from(u64::from(stride) * u64::from(image.height())).ok()?;
    bytes.clear();
    bytes.try_reserve(len).ok()?;
    bytes.resize(len, 0);
    for (y, row) in bytes.chunks_exact_mut(stride as usize).enumerate() {
        let Some((_, pixels)) = image.row_span(u32::try_from(y).ok()?, 0, image.width()) else {
            continue;
        };
        encode_segment(row, pixels, order, u8::MAX);
    }
    Some(stride)
}

/// Encode what the buffer `layer` shows next lacks from the back buffer
/// `back`, and stage it.
fn bake_stale(
    layer: &mut Layer,
    back: &Surface,
    engine: &mut dyn LayerPresenter,
    (order, runner): (ChannelOrder, &dyn JobRunner),
) {
    let Some(target) = layer.target() else {
        return;
    };
    let Some((bytes, stride)) = engine.pixels(layer.held[target].id) else {
        return;
    };
    let at = layer.at;
    let held = &mut layer.held[target];
    for &local in held.stale.rects() {
        encode_from(
            (back, Point::ORIGIN),
            at,
            local,
            (&mut *bytes, stride),
            order,
            runner,
        );
    }
    held.stale.clear();
    layer.staged = Some(target);
}

/// Encode the `local` part of a layer at `at` into its `bytes`, read from
/// `source`, whose top-left lies at `origin` on screen.
fn encode_from(
    (source, origin): (&Surface, Point),
    at: Rect,
    local: Rect,
    (bytes, stride): (&mut [u8], u32),
    order: ChannelOrder,
    runner: &dyn JobRunner,
) {
    let read = Rect::new(
        at.left()
            .saturating_add(local.left())
            .saturating_sub(origin.x),
        at.top()
            .saturating_add(local.top())
            .saturating_sub(origin.y),
        local.width,
        local.height,
    );
    let (Some((lx, ly)), Some((sx, sy))) = (local.surface_origin(), read.surface_origin()) else {
        return;
    };
    let (stride, rows, cols) = (stride as usize, local.height as usize, local.width as usize);
    let per_band = tairix_raster::band_rows(
        rows,
        tairix_parallel::bands(runner, rows, MIN_PARALLEL_BAND_PX.div_ceil(cols.max(1))),
    )
    .max(1) as usize;
    let (Some(region), Some(band_bytes)) = (
        bytes.get_mut(ly as usize * stride..),
        per_band.checked_mul(stride),
    ) else {
        return;
    };
    let (first_byte, row_bytes) = (lx as usize * 4, cols * 4);
    let pieces = region
        .chunks_mut(band_bytes.max(1))
        .take(rows.div_ceil(per_band))
        .enumerate();
    tairix_parallel::for_each_drawn(runner, pieces, &|(band, chunk): (usize, &mut [u8])| {
        for (step, row) in chunk.chunks_mut(stride.max(1)).enumerate() {
            let line = band * per_band + step;
            let Ok(y) = u32::try_from(line) else { break };
            if line >= rows {
                break;
            }
            let (Some((_, pixels)), Some(dst)) = (
                source.row_span(sy.saturating_add(y), sx, local.width),
                row.get_mut(first_byte..first_byte + row_bytes),
            ) else {
                continue;
            };
            encode_segment(dst, pixels, order, u8::MAX);
        }
    });
}

/// Bake what a frost `layer` of `window` lacks into the buffer it shows next:
/// the field everywhere, then the corners over it from the back buffer, where
/// the frame mixed the window's shape over what lies beneath them.
///
/// The corners the layer holds follow the window: where they left and where
/// they arrived are staled, and so is what `composed` changed beneath them.
fn bake_frost(
    layer: &mut Layer,
    window: &Window,
    field: &FrostField,
    back: &Surface,
    composed: &Region,
    engine: &mut dyn LayerPresenter,
    (order, runner): (ChannelOrder, &dyn JobRunner),
) {
    let corners = frost::corners(window.bounds(), window.shape())
        .map(|corner| corner.intersection(&layer.shows));
    if layer.corners != corners {
        for corner in layer.corners.into_iter().chain(corners) {
            layer.dirty(corner);
        }
        layer.corners = corners;
    }
    for &rect in composed.rects() {
        for corner in corners {
            layer.dirty(rect.intersection(&corner));
        }
    }
    let Some(target) = layer.target() else {
        return;
    };
    let Some((bytes, stride)) = engine.pixels(layer.held[target].id) else {
        return;
    };
    let at = layer.at;
    let held = &mut layer.held[target];
    for &local in held.stale.rects() {
        encode_from(
            (field.pixels(), at.origin),
            at,
            local,
            (&mut *bytes, stride),
            order,
            runner,
        );
        let screen = Rect::new(
            at.left().saturating_add(local.left()),
            at.top().saturating_add(local.top()),
            local.width,
            local.height,
        );
        for corner in corners {
            let piece = screen.intersection(&corner);
            if piece.is_empty() {
                continue;
            }
            let mixed = Rect::new(
                piece.left().saturating_sub(at.left()),
                piece.top().saturating_sub(at.top()),
                piece.width,
                piece.height,
            );
            encode_from(
                (back, Point::ORIGIN),
                at,
                mixed,
                (&mut *bytes, stride),
                order,
                runner,
            );
        }
    }
    held.stale.clear();
    layer.staged = Some(target);
}

/// Compose `window` alone over nothing for the `local` part of its layer at
/// `at`, and encode it into the layer's `bytes`; `false` when no scratch row
/// could be had.
fn bake_window(
    (window, chrome): (&Window, Option<&WindowChrome>),
    shadow: &ShadowKit,
    at: Rect,
    local: Rect,
    (bytes, stride): (&mut [u8], u32),
    order: ChannelOrder,
    scratch: &mut Vec<Pixel>,
) -> bool {
    let Some((lx, ly)) = local.surface_origin() else {
        return true;
    };
    let width = local.width as usize;
    if !reserve_row(scratch, width) {
        return false;
    }
    let stride = stride as usize;
    let first_x = at.left().saturating_add_unsigned(lx);
    for line in 0..local.height {
        let row = ly.saturating_add(line);
        let y = at.top().saturating_add_unsigned(row);
        let start = row as usize * stride + lx as usize * 4;
        let (Some(dst), Some(back)) = (
            bytes.get_mut(start..start + width * 4),
            scratch.get_mut(..width),
        ) else {
            continue;
        };
        let mut laid: ArrayVec<RowLayer<'_>, 2> = ArrayVec::new();
        if let Some(cast) = window.shadow_row(y, shadow) {
            let _ = laid.try_push(RowLayer::Shadow(cast));
        }
        if let Some(body) = window.row(y, chrome) {
            let _ = laid.try_push(RowLayer::Body(body));
        }
        let front = match laid.last() {
            Some(RowLayer::Body(body)) => Some(body),
            _ => None,
        };
        let layers = RowLayers {
            under: Some(Pixel::TRANSPARENT),
            desktop: None,
            windows: &laid,
            pointer: &[],
            front,
            overlays: &[],
        };
        let _work = compose_row(
            &layers,
            RowTargets {
                back,
                frame: Some(dst),
            },
            first_x,
            order,
            RowRound {
                reveal: u8::MAX,
                dither: DitherRow::at(y.cast_unsigned()),
            },
        );
    }
    true
}

/// Lay the pointer overlay's `sprites` over nothing into its layer at `at`;
/// `false` when no scratch row could be had.
fn bake_sprites(
    sprites: &[Sprite<'_>],
    at: Rect,
    (bytes, stride): (&mut [u8], u32),
    order: ChannelOrder,
    scratch: &mut Vec<Pixel>,
) -> bool {
    let width = at.width as usize;
    if !reserve_row(scratch, width) {
        return false;
    }
    let stride = stride as usize;
    for line in 0..at.height {
        let y = at.top().saturating_add_unsigned(line);
        let start = line as usize * stride;
        let (Some(dst), Some(row)) = (
            bytes.get_mut(start..start + width * 4),
            scratch.get_mut(..width),
        ) else {
            continue;
        };
        row.fill(Pixel::TRANSPARENT);
        let dither = DitherRow::at(y.cast_unsigned());
        let mut runs: ArrayVec<SpriteRun<'_>, MAX_SPRITES> = ArrayVec::new();
        for run in sprites.iter().filter_map(|sprite| sprite.run(y)) {
            let _ = runs.try_push(run);
        }
        for run in &runs {
            let _blended = blend_run(row, at.left(), run.pixels, run.left, run.opacity, dither);
        }
        encode_segment(dst, row, order, u8::MAX);
    }
    true
}

/// Grow `scratch` to hold a row of `width` pixels, answering whether it does.
fn reserve_row(scratch: &mut Vec<Pixel>, width: usize) -> bool {
    if scratch.len() >= width {
        return true;
    }
    if scratch.try_reserve(width - scratch.len()).is_err() {
        return false;
    }
    scratch.resize(width, Pixel::TRANSPARENT);
    true
}

#[cfg(test)]
#[path = "layers_tests.rs"]
mod tests;
