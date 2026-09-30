//! The value-control family: [`Slider`] and [`Progress`] (spec §11.6–§11.7).
//!
//! Both are *measured* controls whose value is a validated fraction in permille
//! (`0..=1000`). A [`Slider`] is interactive — its thumb runs along a rail, its
//! value track fills from the start to the thumb, drag and keyboard update the
//! visual value immediately while the change commits through the owning model
//! — while [`Progress`] is a read-only instrument trace of
//! known, working, indeterminate, complete, or failed work. Both resolve every
//! colour/metric/radius from the active [`Theme`] and [`Scale`] and round their
//! plates through the shared drawing core the button and selector families use,
//! so nothing here restates a recipe those families already own.
//!
//! A measured track is drawn as a thin instrument line — the theme's
//! `measured_thickness` for a slider's groove, the slightly broader
//! `progress_thickness` for a trace the user only reads, centred in whatever row
//! the owner lays the control out in — never a control-height plate, so a
//! progress bar reads as an instrument and a slider as a groove rather than a
//! block.

use alloc::format;
use alloc::string::String;

use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Ring, RingInk, Surface};
use tairix_theme::{TextRole, Theme};

use crate::damage;
use crate::paint::{
    centred_text_y, clamp_permille, inset, measured_thickness, paint_bead, paint_filled_circle,
    paint_plate, paint_run, plate_border, progress_thickness, resolve_bead, resolve_frame,
    resolve_mark, resolve_rail, role_font, run_width, surface_rect, to_i32, withheld, PlateStyle,
    FULL,
};
use crate::state::{
    ActivityState, ControlDisposition, ControlRole, ControlState, PointerState, RecoveryState,
    RenderInvariant,
};

/// The outcome of interacting with a [`Slider`].
///
/// A slider updates its own displayed value immediately so a drag reads
/// smoothly, but the authoritative change still commits through the owning
/// model: the owner receives the requested value and applies
/// it (calling [`Slider::set_value`] to confirm, or a different value to
/// reject/clamp it).
///
/// The two variants are the *live* value and the *settled* one, and the
/// distinction is load-bearing: durable work — writing a setting, publishing a
/// document, telling another process — is done on [`Settled`] alone. Doing it
/// on every [`SetValue`] means one write per pointer-motion sample of a drag,
/// which is the coupling the charter forbids (`plans/GUI-CONTROLS-DESIGN.md`).
///
/// [`Settled`]: Self::Settled
/// [`SetValue`]: Self::SetValue
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SliderAction {
    /// The slider requests its value become `permille` (`0..=1000`) while the
    /// interaction continues: apply it live, and nothing more.
    SetValue {
        /// The requested new value, in permille.
        permille: u16,
    },
    /// The interaction finished at `permille` — a released drag, a track
    /// click, or a key step. This is where the owner acts durably.
    Settled {
        /// The value the interaction settled on, in permille.
        permille: u16,
    },
}

/// The resolved geometry of a slider within its bounds.
struct SliderLayout {
    /// The surface x of the knob-centre travel origin (value `0`).
    track_x0: u32,
    /// The travel span in pixels the knob centre moves across (value
    /// `0..=1000` maps onto `0..=travel`).
    travel: u32,
    /// The knob's diameter.
    knob_d: u32,
    /// How far the knob's focus ring stands off it: its gap and its width.
    ring_reach: u32,
    /// The groove the knob rides: its left edge and its width.
    groove_x: u32,
    groove_w: u32,
    /// Where the end labels start, when there is room to draw them.
    labels: Option<(u32, u32)>,
    /// The whole control's surface-y origin.
    y: u32,
    /// The whole control's height.
    h: u32,
}

impl SliderLayout {
    /// The knob-centre x for a permille value.
    fn centre_for(&self, permille: u16) -> u32 {
        let v = u64::from(clamp_permille(permille));
        let along = u64::from(self.travel) * v / u64::from(FULL);
        self.track_x0 + u32::try_from(along).unwrap_or(self.travel)
    }

    /// The thin groove band — `(top y, height)` — for a measured track of
    /// `thickness` physical pixels, centred in the control and never taller
    /// than it.
    fn groove(&self, thickness: u32) -> (u32, u32) {
        let band = thickness.max(1).min(self.h);
        (self.y + (self.h - band) / 2, band)
    }

    /// The knob's top y: centred on the groove.
    fn knob_y(&self) -> u32 {
        self.y + (self.h - self.knob_d) / 2
    }

    /// The permille value a pointer at surface-x `px` implies, clamped.
    fn value_for(&self, px: i32) -> u16 {
        if self.travel == 0 {
            return 0;
        }
        let clamped = px.clamp(to_i32(self.track_x0), to_i32(self.track_x0 + self.travel));
        let along = u64::from(
            u32::try_from(clamped)
                .unwrap_or(0)
                .saturating_sub(self.track_x0),
        );
        let permille = along * u64::from(FULL) / u64::from(self.travel);
        clamp_permille(u16::try_from(permille).unwrap_or(FULL))
    }
}

/// Resolve a slider's geometry, or `None` if the control collapses.
///
/// The knob is the theme's size, centred on the groove whatever height the
/// owner seats the slider in, and never so large that it and its focus ring
/// leave the control. Its travel stops short of the ends by that reach, so the
/// ring never overhangs the control's edge. End labels take their width and a
/// gap at either end; a slot too narrow to leave a track between them draws
/// none.
fn slider_layout(
    bounds: Rect,
    ends: Option<&(String, String)>,
    scale: Scale,
    theme: &Theme,
) -> Option<SliderLayout> {
    let (x, y, w, h) = surface_rect(bounds)?;
    if w == 0 || h == 0 {
        return None;
    }
    let ring_reach = plate_border(theme, scale).saturating_mul(2);
    let knob_d = scale
        .scale_length(theme.metrics().slider_knob)
        .min(h.saturating_sub(ring_reach.saturating_mul(2)))
        .min(w)
        .max(1);
    let reach = knob_d.div_ceil(2).saturating_add(ring_reach);
    let right = x.saturating_add(w);
    let (left, right, labels) = ends
        .and_then(|(start, end)| {
            let font = role_font(theme, scale, TextRole::Caption);
            let gap = scale.scale_length(theme.metrics().control_gap);
            let left = x.checked_add(font.text_width(start))?.checked_add(gap)?;
            let end_w = font.text_width(end);
            let track_right = right.checked_sub(end_w.checked_add(gap)?)?;
            (track_right > left.saturating_add(reach.saturating_mul(2))).then_some((
                left,
                track_right,
                Some((x, right - end_w)),
            ))
        })
        .unwrap_or((x, right, None));
    let span = right - left;
    Some(SliderLayout {
        track_x0: left.saturating_add(reach.min(span / 2)),
        travel: span.saturating_sub(reach.saturating_mul(2)),
        knob_d,
        ring_reach,
        groove_x: left.saturating_add(ring_reach.min(span / 2)),
        groove_w: span.saturating_sub(ring_reach.saturating_mul(2)),
        labels,
        y,
        h,
    })
}

/// How large the knob's centre dot is, in percent of the knob, for the
/// pointer's look: it grows under a hovering pointer and tightens under a
/// press, so the knob answers the hand before it moves.
const fn dot_percent(pointer: PointerState) -> u32 {
    match pointer {
        PointerState::Hover | PointerState::DragTarget => 56,
        PointerState::Pressed | PointerState::DragSource => 34,
        PointerState::None => 44,
    }
}

/// A measured value control: a rail, a value track that fills to the knob, a
/// draggable knob, an optional bounded-cap marker, and optionally a fixed set
/// of stops and a label at either end (spec §11.6).
///
/// The active range uses the theme accent, or the semantic pressure colour for
/// a resource slider (a slider under a [`PressureState`](crate::PressureState)).
/// A denied slider keeps its value and shows an Authority Mark rather than
/// looking merely disabled (spec §13); a bounded slider shows a cap marker at
/// the constrained edge and cannot be dragged past it.
///
/// A slider with stops ([`with_stops`](Self::with_stops)) takes only their
/// values: a drag moves from stop to stop and a key steps one, and each stop
/// is marked on the track. End labels ([`with_ends`](Self::with_ends)) name
/// what the two ends mean — *Slow* and *Fast* — so a setting reads in words
/// rather than in the unit the setting is stored in.
///
/// Equal sliders draw the same pixels, so a host may use `==` as its repaint
/// gate: the role, visible state, value, steps, stops, ends, and cap all
/// compare. The pointer coordinate and the drag latch do not — no render path
/// reads either, and what a drag *shows* is the value it commits, which is
/// compared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slider {
    role: ControlRole,
    state: ControlState,
    value: u16,
    line_step: u16,
    page_step: u16,
    cap: Option<u16>,
    /// How many evenly spaced values the slider takes, or `0` for any.
    stops: u16,
    /// What the start and end of the track mean.
    ends: Option<(String, String)>,
    /// The last pointer position, mapped to a value on press and on each drag
    /// sample — hit-testing input, never a drawn property.
    pointer: RenderInvariant<Point>,
    /// Whether the knob is being dragged; the press *look* lives in
    /// `state.pointer` and the moved knob in `value`.
    dragging: RenderInvariant<bool>,
}

impl Slider {
    /// A neutral slider at `value` permille, with a 1% line step and a 10%
    /// page step (both settable).
    #[must_use]
    pub fn new(value: u16) -> Self {
        Self {
            role: ControlRole::Neutral,
            state: ControlState::idle(),
            value: clamp_permille(value),
            line_step: 10,
            page_step: 100,
            cap: None,
            stops: 0,
            ends: None,
            pointer: RenderInvariant::new(Point::ORIGIN),
            dragging: RenderInvariant::new(false),
        }
    }

    /// This slider with a non-default role (e.g. destructive or recovery).
    #[must_use]
    pub fn with_role(mut self, role: ControlRole) -> Self {
        self.role = role;
        self
    }

    /// This slider with the given line and page steps (permille), each clamped
    /// into `0..=1000`. A zero step moves nothing (fail closed, no guessed
    /// distance).
    #[must_use]
    pub fn with_steps(mut self, line_step: u16, page_step: u16) -> Self {
        self.line_step = clamp_permille(line_step);
        self.page_step = clamp_permille(page_step);
        self
    }

    /// This slider taking only `count` evenly spaced values, the first at the
    /// start and the last at the end, each marked on the track. Fewer than two
    /// stops is no stop at all: the slider takes any value.
    ///
    /// A key steps from one stop to the next, and the value is moved onto the
    /// nearest stop.
    #[must_use]
    pub fn with_stops(mut self, count: u16) -> Self {
        self.stops = if count >= 2 { count.min(FULL + 1) } else { 0 };
        self.value = self.snapped(self.value);
        self
    }

    /// This slider with `start` and `end` naming what the two ends of its
    /// track mean.
    #[must_use]
    pub fn with_ends(mut self, start: impl Into<String>, end: impl Into<String>) -> Self {
        self.ends = Some((start.into(), end.into()));
        self
    }

    /// This slider bounded to a maximum settable value (permille), shown as a
    /// cap marker; the value can neither be dragged nor stepped past it.
    #[must_use]
    pub fn with_cap(mut self, cap: u16) -> Self {
        let cap = clamp_permille(cap);
        self.cap = Some(cap);
        self.value = self.value.min(cap);
        self
    }

    /// The slider's current value, in permille.
    #[must_use]
    pub fn value(&self) -> u16 {
        self.value
    }

    /// Set the slider's value (e.g. after the owner commits a change), clamped
    /// into range and to any cap, and onto the nearest stop.
    pub fn set_value(&mut self, value: u16) {
        self.value = self.snapped(self.ceiling().min(clamp_permille(value)));
    }

    /// Which stop `permille` is at or nearest, counting from the start; `None`
    /// for a slider without stops.
    #[must_use]
    pub fn stop_of(&self, permille: u16) -> Option<u16> {
        let gaps = u32::from(self.stops.checked_sub(1).filter(|gaps| *gaps > 0)?);
        let full = u32::from(FULL);
        let nearest = (u32::from(clamp_permille(permille)) * gaps + full / 2) / full;
        u16::try_from(nearest).ok()
    }

    /// The value of stop `index`, counting from the start; `None` for a
    /// slider without stops or an index past its last.
    #[must_use]
    pub fn stop_value(&self, index: u16) -> Option<u16> {
        let gaps = u32::from(self.stops.checked_sub(1).filter(|gaps| *gaps > 0)?);
        let index = u32::from(index);
        (index <= gaps).then(|| u16::try_from(index * u32::from(FULL) / gaps).unwrap_or(FULL))
    }

    /// `permille` moved onto the nearest stop, or as it is without stops.
    fn snapped(&self, permille: u16) -> u16 {
        self.stop_of(permille)
            .and_then(|stop| self.stop_value(stop))
            .unwrap_or(permille)
    }

    /// The slider's role.
    #[must_use]
    pub fn role(&self) -> ControlRole {
        self.role
    }

    /// The slider's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the slider's composed state (e.g. from a model update).
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
    }

    /// Set the slider's keyboard focus.
    pub fn set_focused(&mut self, focused: bool) {
        self.state.focus.focused = focused;
    }

    /// The highest value the slider may take: the cap if bounded, else full.
    fn ceiling(&self) -> u16 {
        self.cap.unwrap_or(FULL)
    }

    /// Request a new value, clamped to `0..=ceiling` and onto the nearest
    /// stop; returns the action if the value actually changed, updating the
    /// displayed value immediately and reporting `bounds` — the knob and the
    /// filled track both move with it.
    fn request(&mut self, value: u16, bounds: Rect, damage: &mut Region) -> Option<SliderAction> {
        let mut next = self.snapped(self.ceiling().min(clamp_permille(value)));
        if next > self.ceiling() {
            // A cap between two stops holds the value on the stop beneath it.
            next = self
                .stop_of(next)
                .and_then(|stop| stop.checked_sub(1))
                .and_then(|stop| self.stop_value(stop))
                .unwrap_or(self.ceiling());
        }
        damage::set(&mut self.value, next, bounds, damage)
            .then_some(SliderAction::SetValue { permille: next })
    }

    /// Paint the slider into `surface` at `bounds` for the active theme.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let Some(layout) = slider_layout(bounds, self.ends.as_ref(), scale, theme) else {
            return;
        };
        let palette = theme.palette();
        let border = plate_border(theme, scale);
        let (groove_y, groove_h) = layout.groove(measured_thickness(theme, scale));

        // The quiet groove the knob runs along, from where the knob rests at
        // one end to where it rests at the other.
        surface.fill_round_rect(
            layout.groove_x,
            groove_y,
            layout.groove_w,
            groove_h,
            groove_h / 2,
            Color::from(palette.scroll_track),
        );

        // The value track, filled from the start to the knob centre.
        let centre = layout.centre_for(self.value);
        let active = resolve_rail(theme, self.state)
            .unwrap_or_else(|| resolve_mark(theme, self.role, self.state));
        let active_w = centre.saturating_sub(layout.groove_x).max(groove_h);
        surface.fill_round_rect(
            layout.groove_x,
            groove_y,
            active_w,
            groove_h,
            groove_h / 2,
            active,
        );

        self.paint_stops(surface, &layout, (groove_y, groove_h), active, theme);

        // The bounded-cap marker at the constrained edge, if any.
        if let Some(cap) = self.cap {
            if cap < FULL {
                let cap_x = layout.centre_for(cap);
                let tick_w = border.max(2).min(layout.knob_d);
                surface.fill_rect(
                    cap_x.saturating_sub(tick_w / 2),
                    layout.y,
                    tick_w,
                    layout.h,
                    Color::from(palette.warning),
                );
            }
        }

        self.paint_knob(surface, &layout, centre, active, (scale, theme));
        self.paint_ends(surface, &layout, scale, theme);

        // The Signal Bead (denied lock / recovery / complete) at the top-right.
        if let Some((color, shape)) = resolve_bead(theme, self.state) {
            let (x, _, w, _) = surface_rect(bounds).unwrap_or_default();
            let size = scale
                .scale_length(theme.metrics().bead_size)
                .max(3)
                .min(w)
                .min(layout.h);
            paint_bead(surface, x + w - size, layout.y, size, color, shape);
        }
    }

    /// Mark each stop on the groove: a dot in the track's own colour where
    /// the groove is empty, and in the colour laid on the accent where it is
    /// filled, so a stop reads on either side of the knob.
    fn paint_stops(
        &self,
        surface: &mut Surface,
        layout: &SliderLayout,
        (groove_y, groove_h): (u32, u32),
        active: Color,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let dot = (groove_h / 2).max(1);
        let dot_y = groove_y + (groove_h - dot) / 2;
        let mut stop = 0;
        while let Some(value) = self.stop_value(stop) {
            let x = layout.centre_for(value).saturating_sub(dot / 2);
            let ink = if value <= self.value {
                Color::from(palette.on_accent)
            } else {
                active
            };
            paint_filled_circle(surface, x, dot_y, dot, ink);
            stop += 1;
        }
    }

    /// The knob at `centre`: a raised disc over a soft shadow, a dot of the
    /// track's colour at its heart, and, when focused, a ring standing clear
    /// of it.
    fn paint_knob(
        &self,
        surface: &mut Surface,
        layout: &SliderLayout,
        centre: u32,
        active: Color,
        (scale, theme): (Scale, &Theme),
    ) {
        let d = layout.knob_d;
        let (x, y) = (centre.saturating_sub(d / 2), layout.knob_y());
        let palette = theme.palette();
        let shade = palette.drop_shadow;
        let lift = scale.scale_length(1).max(1).min(layout.ring_reach);
        paint_filled_circle(
            surface,
            x,
            y + lift,
            d,
            Color::rgba(shade.r, shade.g, shade.b, shade.a / 2),
        );
        let frame = resolve_frame(theme, self.role, self.state);
        paint_plate(
            surface,
            (x, y, d, d),
            &PlateStyle {
                radius: d / 2,
                border: plate_border(theme, scale),
                plate: frame.plate,
                rim: frame.rim,
                focused: false,
                ring: Color::from(palette.rim_active),
            },
        );
        let dot = (d * dot_percent(self.state.pointer) / 100).max(1);
        paint_filled_circle(surface, x + (d - dot) / 2, y + (d - dot) / 2, dot, active);

        if frame.focused {
            let border = plate_border(theme, scale);
            let outer = d.div_ceil(2).saturating_add(layout.ring_reach);
            let across = outer.saturating_mul(2);
            let (cx, cy) = (x + d / 2, y + d / 2);
            surface.wash_ring(
                cx.saturating_sub(outer),
                cy.saturating_sub(outer),
                across,
                across,
                Ring::uniform(outer, border),
                RingInk::Solid(Color::from(palette.rim_active)),
            );
        }
    }

    /// The end labels, in the caption tone, centred on the groove.
    fn paint_ends(
        &self,
        surface: &mut Surface,
        layout: &SliderLayout,
        scale: Scale,
        theme: &Theme,
    ) {
        let (Some((start, end)), Some((start_x, end_x))) = (&self.ends, layout.labels) else {
            return;
        };
        let font = role_font(theme, scale, TextRole::Caption);
        let y = centred_text_y(font, layout.y, layout.h);
        let ink = Color::from(theme.palette().on_surface_muted);
        font.draw_text(surface, to_i32(start_x), y, start, ink);
        font.draw_text(surface, to_i32(end_x), y, end, ink);
    }

    /// Feed a pointer event; a press/drag over an actionable slider updates the
    /// value and reports it as
    /// [`SetValue`](SliderAction::SetValue), and the release that ends the drag
    /// reports [`Settled`](SliderAction::Settled). A denied, disabled, pending,
    /// or failed-closed slider ignores pointer input (fail closed). The slider
    /// reports `bounds` into `damage` when the event moved the knob or changed
    /// the pointer look; a sample that stays inside it reports nothing.
    ///
    /// A press on an end label takes the value to that end, as a press past
    /// the knob's travel does.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<SliderAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        let layout = slider_layout(bounds, self.ends.as_ref(), scale, theme)?;
        let inside = bounds.contains(*self.pointer);
        let hover_or_none = if inside {
            PointerState::Hover
        } else {
            PointerState::None
        };
        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                if inside && self.state.is_actionable() {
                    *self.dragging = true;
                    damage::set(
                        &mut self.state.pointer,
                        PointerState::Pressed,
                        bounds,
                        damage,
                    );
                    return self.request(layout.value_for(self.pointer.x), bounds, damage);
                }
                None
            }
            InputEvent::PointerMoved { .. } => {
                if *self.dragging {
                    self.request(layout.value_for(self.pointer.x), bounds, damage)
                } else {
                    damage::set(&mut self.state.pointer, hover_or_none, bounds, damage);
                    None
                }
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let dragged = core::mem::replace(&mut *self.dragging, false);
                damage::set(&mut self.state.pointer, hover_or_none, bounds, damage);
                // A release settles the *interaction*, so it reports even when
                // the last sample moved nothing — that is the one moment the
                // owner may act durably. A release that no press here started
                // settles nothing.
                dragged.then_some(SliderAction::Settled {
                    permille: self.value,
                })
            }
            _ => None,
        }
    }

    /// Feed a key event; arrows step by the line step, PageUp/PageDown by the
    /// page step — each one stop, on a slider with stops — and Home/End jump
    /// to the ends, on a focused, actionable slider. A step that moves the
    /// value reports `bounds` into `damage` and
    /// [`Settled`](SliderAction::Settled); one already at the end it steps
    /// toward reports nothing.
    pub fn on_key(&mut self, key: Key, bounds: Rect, damage: &mut Region) -> Option<SliderAction> {
        if !self.state.focus.focused || !self.state.is_actionable() {
            return None;
        }
        let held_stop = self.stop_of(self.value);
        let step = |by: u16, forward: bool| match held_stop {
            Some(at) => {
                let next = if forward {
                    at.saturating_add(1)
                } else {
                    at.saturating_sub(1)
                };
                self.stop_value(next).unwrap_or(self.value)
            }
            None if forward => self.value.saturating_add(by),
            None => self.value.saturating_sub(by),
        };
        let target = match key {
            Key::Named(NamedKey::Right | NamedKey::Up) => step(self.line_step, true),
            Key::Named(NamedKey::Left | NamedKey::Down) => step(self.line_step, false),
            Key::Named(NamedKey::PageUp) => step(self.page_step, true),
            Key::Named(NamedKey::PageDown) => step(self.page_step, false),
            Key::Named(NamedKey::Home) => 0,
            Key::Named(NamedKey::End) => FULL,
            _ => return None,
        };
        // One keystroke is a whole interaction, so a step that moves the value
        // settles it. A step already at the end it presses toward moves
        // nothing and reports nothing.
        self.request(target, bounds, damage)
            .map(|_| SliderAction::Settled {
                permille: self.value,
            })
    }
}

/// A read-only instrument trace of known, working, indeterminate, complete, or
/// failed work (spec §11.7).
///
/// Progress is *not* decoration and runs no idle loop: its appearance is
/// driven entirely by the [`ControlState::activity`] its owner sets and, for an
/// indeterminate trace, by a [`phase`](Progress::set_phase) the owner advances
/// on job-progress events. Known progress shows a stable percentage; a failed
/// job shows a recovery rim and a concise reason; an indeterminate trace is a
/// bounded moving segment that renders statically under reduced motion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Progress {
    role: ControlRole,
    state: ControlState,
    phase: u16,
    label: Option<String>,
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress {
    /// An idle neutral progress trace.
    #[must_use]
    pub fn new() -> Self {
        Self {
            role: ControlRole::Neutral,
            state: ControlState::idle(),
            phase: 0,
            label: None,
        }
    }

    /// This trace with a non-default role.
    #[must_use]
    pub fn with_role(mut self, role: ControlRole) -> Self {
        self.role = role;
        self
    }

    /// This trace with a caption (a value/throughput note, or a failure
    /// reason). The reason is concise user-facing text, never a secret.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The trace's composed state.
    #[must_use]
    pub fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the trace's composed state (e.g. from a model update).
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
    }

    /// Advance the indeterminate animation phase (permille around the track).
    /// The owner calls this from a job-progress event, never an idle loop.
    pub fn set_phase(&mut self, phase: u16) {
        self.phase = clamp_permille(phase);
    }

    /// Whether the trace's linked object has failed or needs recovery.
    fn is_failed(&self) -> bool {
        self.state.disposition() == ControlDisposition::FailedClosed
            || self.state.recovery != RecoveryState::None
    }

    /// The thin trace band within `bounds`: the theme's progress thickness,
    /// never taller than the bounds, centred when the caption cannot fit
    /// beside it and top-aligned when it can.
    ///
    /// The trace is an instrument line, not a filled block: its height comes
    /// from the theme's progress-trace thickness token — a touch broader than a
    /// slider's groove, because a read-only fill has no thumb to mark it — so a
    /// caller that hands it a tall row gets a thin bar and a captioned row
    /// rather than a slab.
    fn band(
        rect: (u32, u32, u32, u32),
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
    ) -> (u32, u32) {
        let (_, y, _, h) = rect;
        let band = progress_thickness(theme, scale).min(h).max(1);
        let spare = h - band;
        if spare >= font.glyph_height() {
            (y, band)
        } else {
            (y + spare / 2, band)
        }
    }

    /// Paint the trace into `surface` at `bounds` for the active theme.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let font = role_font(theme, scale, TextRole::Body);
        let Some((x, y, w, h)) = surface_rect(bounds) else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        let palette = theme.palette();
        let metrics = theme.metrics();
        let border = plate_border(theme, scale);
        let (band_y, band_h) = Self::band((x, y, w, h), scale, theme, font);
        let radius = scale
            .scale_length(metrics.control_corner_radius)
            .min(band_h / 2);
        let failed = self.is_failed();
        let frame = resolve_frame(theme, self.role, self.state);
        let rim = if failed {
            Color::from(palette.recovery)
        } else {
            frame.rim
        };

        paint_plate(
            surface,
            (x, band_y, w, band_h),
            &PlateStyle {
                radius,
                border,
                plate: Color::from(palette.scroll_track),
                rim,
                focused: false,
                ring: Color::from(palette.rim_active),
            },
        );

        let inner_radius = radius.saturating_sub(border);
        if let Some((ix, iy, iw, ih)) = inset(x, band_y, w, band_h, border) {
            self.paint_fill(surface, (ix, iy, iw, ih), inner_radius, theme, failed);
        }

        if let Some((color, shape)) = resolve_bead(theme, self.state) {
            let size = scale
                .scale_length(metrics.bead_size)
                .max(3)
                .min(w)
                .min(band_h);
            paint_bead(surface, x + w - border - size, band_y, size, color, shape);
        }

        self.paint_caption(surface, (x, y, w, h), (band_y, band_h), scale, theme, font);
    }

    /// Paint the value fill for the current activity within the inner area.
    fn paint_fill(
        &self,
        surface: &mut Surface,
        inner: (u32, u32, u32, u32),
        inner_radius: u32,
        theme: &Theme,
        failed: bool,
    ) {
        let (ix, iy, iw, ih) = inner;
        if iw == 0 || ih == 0 || failed {
            return;
        }
        let palette = theme.palette();
        let accent = resolve_rail(theme, self.state)
            .unwrap_or_else(|| resolve_mark(theme, self.role, self.state));
        match self.state.activity {
            ActivityState::Progress(v) => {
                let fill_w =
                    u32::try_from(u64::from(iw) * u64::from(v.permille()) / u64::from(FULL))
                        .unwrap_or(iw);
                if fill_w > 0 {
                    surface.fill_round_rect(ix, iy, fill_w, ih, inner_radius, accent);
                }
            }
            ActivityState::Working | ActivityState::Indeterminate => {
                let seg_w = (iw / 4).max(1);
                let travel = iw.saturating_sub(seg_w);
                let pos = if theme.motion().reduced_motion() {
                    travel / 2
                } else {
                    u32::try_from(u64::from(travel) * u64::from(self.phase) / u64::from(FULL))
                        .unwrap_or(travel)
                };
                surface.fill_round_rect(ix + pos, iy, seg_w, ih, inner_radius, accent);
            }
            ActivityState::Complete => {
                surface.fill_round_rect(ix, iy, iw, ih, inner_radius, Color::from(palette.success));
            }
            ActivityState::Idle => {}
        }
    }

    /// Paint the caption: a percentage for known progress, else the reason /
    /// note label.
    ///
    /// It sits in the height left over below the thin trace band when there is
    /// room for a full glyph, and is centred across the band otherwise, so a
    /// caption never overprints the value fill it describes.
    fn paint_caption(
        &self,
        surface: &mut Surface,
        rect: (u32, u32, u32, u32),
        band: (u32, u32),
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
    ) {
        let (x, y, w, h) = rect;
        let (band_y, band_h) = band;
        let failed = self.is_failed();
        let border = plate_border(theme, scale);
        let pad = scale.scale_length(theme.metrics().control_inset);
        let edge = border.saturating_add(pad);
        let avail = w.saturating_sub(edge.saturating_mul(2));
        if avail == 0 {
            return;
        }
        let palette = theme.palette();
        let percent;
        let (text, color) = if failed {
            match &self.label {
                Some(reason) => (reason.as_str(), Color::from(palette.recovery)),
                None => return,
            }
        } else if let ActivityState::Progress(v) = self.state.activity {
            let pct = ((u32::from(v.permille()) + 5) / 10).min(100);
            percent = format!("{pct}%");
            (percent.as_str(), Color::from(palette.on_surface))
        } else {
            match &self.label {
                Some(note) => (note.as_str(), Color::from(palette.on_surface)),
                None => return,
            }
        };
        let run = font.elide_to_width(text, avail);
        let width = run_width(font, run);
        let glyph_h = font.glyph_height();
        let cx = to_i32(x) + to_i32(w) / 2;
        let below = band_y + band_h;
        let spare = (y + h).saturating_sub(below);
        let text_y = if spare >= glyph_h {
            to_i32(below) + to_i32(spare - glyph_h) / 2
        } else {
            to_i32(band_y) + (to_i32(band_h) - to_i32(glyph_h)) / 2
        };
        paint_run(
            surface,
            font,
            run,
            (cx - to_i32(width) / 2, text_y),
            color,
            None,
        );
    }
}
