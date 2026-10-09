//! The colour picker: one colour picked on a view, its fields in one model,
//! its hexadecimal notation and, where it has one, its opacity.
//!
//! The parts, each its own stop for the keyboard:
//!
//! - the **view** ([`PickerView`]): the *square* — saturation across and value
//!   up at the colour's hue, the **hue strip** beside it, red at both ends; the
//!   *wheel* — the hue round a **ring**, saturation and value in a **triangle**
//!   within it, its pure-hue corner turned to the hue; or the *sliders* — a
//!   **track** per channel of the model, each drawn as that channel sweeps with
//!   the rest held;
//! - the **opacity strip**, over a checker, where the colour has an alpha;
//! - the **swatch**: the colour, and beside it the earlier colour an owner
//!   names, which a press takes back;
//! - the **hex field**: `#rrggbb`, or `#rrggbbaa` while translucent;
//! - **number fields** for the model's channels ([`ColourModel`]) — RGB, HSV,
//!   HSL, CMYK, Lab, `LCh` or grey — and A (`0..=255`).
//!
//! The picker holds the colour as hue, saturation and value, so a colour
//! dragged to grey or to black keeps the hue and saturation it showed, and
//! holds the model's values as typed, so editing one channel keeps the others
//! as shown. A Lab or `LCh` value outside sRGB is clipped to it and the swatch
//! is marked so. The view and model are the owner's to keep. Wide bounds put
//! the fields beside the view and narrow ones beneath it; bounds too short
//! for everything give up the number fields first, then the hex row, and
//! never the view.
//!
//! Like a slider, it shows a change at once, reports
//! [`PickerOutcome::Edited`] for each live one and [`PickerOutcome::Settled`]
//! once an interaction ends; durable work belongs on the settle.

use core::cell::Cell;

use tairix_colour::{parse_hex, Fraction, Hsv, Hue, Rgba};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_inline::ArrayString;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Pixel, Ring, RingInk, Surface};
use tairix_theme::{TextRole, Theme};
use tairix_util::mathf;

use crate::checker::Checker;
use crate::colour_model::{ColourModel, PickerView, MOST_CHANNELS};
use crate::number::{NumberAction, NumberField};
use crate::paint::{
    foreground, paint_bead, paint_filled_circle, paint_run, plate_border, resolve_bead, role_font,
    surface_rect, withheld, BeadShape,
};
use crate::state::{ControlDisposition, ControlState, RenderInvariant, ValidationState};
use crate::text::{owner_chord, TextAction, TextField};

/// The breadth of the hue and opacity strips, in logical pixels.
const STRIP: u32 = 16;

/// The least side the plane is drawn at, in logical pixels.
const PLANE_MIN: u32 = 96;

/// The tallest the plane grows when the picker is stacked, in logical pixels.
const PLANE_MAX: u32 = 192;

/// The plane marker's radius, in logical pixels.
const MARKER: u32 = 5;

/// The hex field's length: `#` and eight digits.
const HEX_LEN: usize = 9;

/// The alpha field's place among the number fields, after the model's.
const ALPHA: usize = MOST_CHANNELS;

/// The number fields: the most channels a model has, and the alpha.
const FIELDS: usize = MOST_CHANNELS + 1;

/// The ring's breadth, in thousandths of the wheel's radius.
const RING_BREADTH: u32 = 160;

/// What feeding an event to a [`ColourPicker`] concluded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PickerOutcome {
    /// Not the picker's.
    Ignored,
    /// Taken, with the colour unmoved: the focus, a caret, a refused key.
    Taken,
    /// The colour became this while the interaction continues — a drag
    /// sample, a digit typed. Apply it live, and nothing more.
    Edited(Rgba),
    /// The interaction finished on this colour: a released or abandoned
    /// drag, a key step, a field committed, the earlier colour taken back.
    /// This is where the owner acts durably.
    Settled(Rgba),
}

/// A part the keyboard can rest on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Part {
    /// The square's plane.
    Plane,
    /// The square's hue strip.
    Hue,
    /// The wheel's hue ring.
    Ring,
    /// The wheel's saturation and value triangle.
    Triangle,
    /// The slider of channel `n` of the model.
    Track(usize),
    Alpha,
    Earlier,
    Hex,
    /// Field `n`: a channel of the model, or [`ALPHA`].
    Number(usize),
}

/// The most parts a picker shows: four tracks, the opacity strip, the
/// earlier colour, the hex field and five number fields.
const MOST_PARTS: usize = 12;

/// The parts a picker shows, in the order Tab walks them.
#[derive(Copy, Clone, Debug)]
struct Parts {
    parts: [Part; MOST_PARTS],
    count: usize,
}

impl Parts {
    fn as_slice(&self) -> &[Part] {
        &self.parts[..self.count]
    }

    fn push(&mut self, part: Part) {
        if let Some(slot) = self.parts.get_mut(self.count) {
            *slot = part;
            self.count += 1;
        }
    }
}

/// The readouts a change of colour rewrote, for the damage they owe.
#[derive(Copy, Clone, Debug, Default)]
struct Rewritten {
    numbers: [bool; FIELDS],
    hex: bool,
    tracks: bool,
}

/// The lengths a layout is built from, at one scale and theme.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Measures {
    gap: u32,
    small: u32,
    row: u32,
    border: u32,
    strip: u32,
    marker: u32,
    plane_min: u32,
    plane_max: u32,
    label: u32,
    field: u32,
    unit: u32,
    hex: u32,
}

impl Measures {
    /// A number column: label, field and unit.
    fn column(self) -> u32 {
        self.label + self.small + self.field + self.small + self.unit
    }

    fn swatch(self) -> u32 {
        self.row * 2
    }

    /// The block holding the hex row and the number fields.
    fn block_width(self) -> u32 {
        (self.column() * 2 + self.gap).max(self.swatch() + self.gap + self.hex)
    }

    fn block_height(self, rows: u32) -> u32 {
        self.row + self.gap + self.row * rows + self.gap * rows.saturating_sub(1)
    }

    /// The height of `tracks` stacked sliders.
    fn tracks_height(self, tracks: u32) -> u32 {
        self.row * tracks + self.gap * tracks.saturating_sub(1)
    }
}

/// What the measures depend on: the scale and the faces and metrics the
/// theme gives.
type MeasureKey = (Scale, BitmapFont, BitmapFont, u32, u32, u32, u32);

/// The measures last taken, kept so a pointer sample does not measure text
/// again. It compares equal to any other, as nothing drawn reads it.
#[derive(Clone, Debug, Default)]
struct MeasureCache(Cell<Option<(MeasureKey, Measures)>>);

impl PartialEq for MeasureCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for MeasureCache {}

/// The wheel's geometry: its centre and radii, in surface pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Wheel {
    cx: f64,
    cy: f64,
    outer: f64,
    inner: f64,
}

impl Wheel {
    /// The wheel filling the square `area`.
    fn in_area(area: Rect) -> Self {
        let side = f64::from(area.width.min(area.height));
        let outer = (side / 2.0 - 1.0).max(1.0);
        Self {
            cx: f64::from(area.left()) + side / 2.0,
            cy: f64::from(area.top()) + side / 2.0,
            outer,
            inner: outer * (1.0 - f64::from(RING_BREADTH) / 1000.0),
        }
    }

    /// The triangle's corners for `hue`: the pure hue, white and black, each
    /// a third of a turn on from the last, inset from the ring.
    fn corners(self, hue: Hue) -> [(f64, f64); 3] {
        let reach = (self.inner - 2.0).max(1.0);
        let theta = degrees_of(hue).to_radians();
        let third = core::f64::consts::TAU / 3.0;
        [0.0, 1.0, 2.0].map(|turns| {
            let angle = theta + third * turns;
            (
                self.cx + reach * mathf::cos(angle),
                self.cy - reach * mathf::sin(angle),
            )
        })
    }

    /// How far from the centre `(x, y)` lies.
    fn radius_at(self, x: f64, y: f64) -> f64 {
        mathf::hypot(x - self.cx, y - self.cy)
    }

    /// The hue at the angle `(x, y)` lies at about the centre: red to the
    /// right, round against the clock.
    fn hue_at(self, x: f64, y: f64) -> Hue {
        Hue::from_degrees_f64(mathf::atan2(self.cy - y, x - self.cx).to_degrees())
    }

    /// The saturation and value `(x, y)` names in the triangle for `hue`,
    /// held to the triangle; the saturation kept from `near` at black.
    fn sv_at(self, hue: Hue, at: (f64, f64), near: Fraction) -> (Fraction, Fraction) {
        // Outside, held to the nearest edge by dropping what is negative.
        let (pure, white, black) = barycentric(self.corners(hue), at);
        let [pure, white, black] = [pure, white, black].map(|weight| mathf::fmax(weight, 0.0));
        let sum = pure + white + black;
        if sum <= f64::EPSILON {
            return (near, Fraction::NONE);
        }
        let value = (pure + white) / sum;
        let saturation = if value <= f64::EPSILON {
            near
        } else {
            Fraction::from_f64(pure / sum / value)
        };
        (saturation, Fraction::from_f64(value))
    }

    /// Where saturation and value `hsv` stands in the triangle.
    fn point_of(self, hsv: Hsv) -> (f64, f64) {
        let corners = self.corners(hsv.hue);
        let (saturation, value) = (fraction_f64(hsv.saturation), fraction_f64(hsv.value));
        let weights = [saturation * value, value * (1.0 - saturation), 1.0 - value];
        let along = |axis: fn((f64, f64)) -> f64| {
            weights
                .iter()
                .zip(corners)
                .map(|(weight, corner)| weight * axis(corner))
                .sum()
        };
        (along(|corner| corner.0), along(|corner| corner.1))
    }
}

/// The barycentric weights of `(x, y)` in triangle `corners`.
fn barycentric(corners: [(f64, f64); 3], (x, y): (f64, f64)) -> (f64, f64, f64) {
    let [(x0, y0), (x1, y1), (x2, y2)] = corners;
    let area = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2);
    if area.abs() <= f64::EPSILON {
        return (0.0, 0.0, 1.0);
    }
    let a = ((y1 - y2) * (x - x2) + (x2 - x1) * (y - y2)) / area;
    let b = ((y2 - y0) * (x - x2) + (x0 - x2) * (y - y2)) / area;
    (a, b, 1.0 - a - b)
}

fn fraction_f64(fraction: Fraction) -> f64 {
    f64::from(fraction.raw()) / f64::from(u16::MAX)
}

fn degrees_of(hue: Hue) -> f64 {
    f64::from(hue.steps()) * 360.0 / f64::from(Hue::TURN)
}

/// The picker's resolved geometry: one layout serves drawing, hit-testing and
/// the damage each change reports. An absent part is [`Rect::EMPTY`].
#[derive(Clone, Debug)]
struct Layout {
    measures: Measures,
    view: PickerView,
    /// The view's own area: the square's plane, the wheel's square, or the
    /// sliders' stack.
    plane: Rect,
    hue: Rect,
    tracks: [Rect; MOST_CHANNELS],
    track_labels: [Rect; MOST_CHANNELS],
    alpha: Rect,
    alpha_label: Rect,
    earlier: Rect,
    now: Rect,
    hex: Rect,
    numbers: [Rect; FIELDS],
    labels: [Rect; FIELDS],
    units: [Rect; FIELDS],
}

impl Layout {
    fn rect_of(&self, part: Part) -> Rect {
        match part {
            Part::Plane | Part::Ring | Part::Triangle => self.plane,
            Part::Hue => self.hue,
            Part::Track(index) => self.tracks.get(index).copied().unwrap_or(Rect::EMPTY),
            Part::Alpha => self.alpha,
            Part::Earlier => self.earlier,
            Part::Hex => self.hex,
            Part::Number(index) => self.numbers.get(index).copied().unwrap_or(Rect::EMPTY),
        }
    }

    /// The part `point` lies on, of those `parts` names; on the wheel, the
    /// ring or the triangle by where it lies in the square.
    fn part_at(&self, point: Point, parts: &Parts, hue: Hue) -> Option<Part> {
        let found = parts
            .as_slice()
            .iter()
            .copied()
            .find(|&part| self.rect_of(part).contains(point))?;
        if self.view != PickerView::Wheel || !matches!(found, Part::Ring | Part::Triangle) {
            return Some(found);
        }
        let wheel = Wheel::in_area(self.inner(Part::Ring));
        let at = (f64::from(point.x), f64::from(point.y));
        let radius = wheel.radius_at(at.0, at.1);
        if radius >= wheel.inner && radius <= wheel.outer + 1.0 {
            return Some(Part::Ring);
        }
        let (pure, white, black) = barycentric(wheel.corners(hue), at);
        (pure >= 0.0 && white >= 0.0 && black >= 0.0).then_some(Part::Triangle)
    }

    /// A part's drawing area, inside the rim it is outlined with.
    fn inner(&self, part: Part) -> Rect {
        self.rect_of(part).inset(self.measures.border)
    }

    /// Where the plane marker for `hsv` is drawn, within the plane.
    fn marker(&self, hsv: Hsv) -> Rect {
        let (cx, cy) = marker_centre(self.inner(Part::Plane), hsv);
        let reach = self.measures.marker + 1;
        let side = reach * 2 + 1;
        Rect::new(cx - to_i32(reach), cy - to_i32(reach), side, side).intersection(&self.plane)
    }

    fn report(&self, rewritten: Rewritten, damage: &mut Region) {
        for (rect, changed) in self.numbers.iter().zip(rewritten.numbers) {
            if changed {
                damage.add(*rect);
            }
        }
        if rewritten.hex {
            damage.add(self.hex);
        }
        if rewritten.tracks {
            for track in self.tracks {
                damage.add(track);
            }
            damage.add(self.alpha);
        }
    }
}

/// A colour picker.
///
/// Equal pickers draw the same pixels: the drag latch, the pointer, the
/// settled baseline and the remembered measures are bookkeeping no render
/// path reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColourPicker {
    hsv: Hsv,
    alpha: u8,
    opacity: bool,
    earlier: Option<Rgba>,
    view: PickerView,
    model: ColourModel,
    /// The model's values as last shown or typed.
    typed: [i32; MOST_CHANNELS],
    /// Whether the values typed lay outside sRGB, and the colour is their
    /// nearest in it.
    clipped: bool,
    state: ControlState,
    part: Part,
    hex: TextField,
    numbers: [NumberField; FIELDS],
    /// The colour the last interaction settled on: what an abandoned drag
    /// returns to, and what a settle is measured against.
    settled: RenderInvariant<Rgba>,
    /// Whether the colour moved live since the last settle.
    live: RenderInvariant<bool>,
    drag: RenderInvariant<Option<Part>>,
    pointer: RenderInvariant<Point>,
    measured: MeasureCache,
}

impl ColourPicker {
    /// A picker showing `colour` opaque on the square, its fields in RGB,
    /// with no earlier colour beside it.
    #[must_use]
    pub fn new(colour: Rgba) -> Self {
        let mut picker = Self {
            hsv: Hsv::default(),
            alpha: u8::MAX,
            opacity: false,
            earlier: None,
            view: PickerView::Square,
            model: ColourModel::Rgb,
            typed: [0; MOST_CHANNELS],
            clipped: false,
            state: ControlState::idle(),
            part: Part::Plane,
            hex: TextField::new().with_max_len(HEX_LEN),
            numbers: fields_of(ColourModel::Rgb),
            settled: RenderInvariant::new(colour),
            live: RenderInvariant::new(false),
            drag: RenderInvariant::new(None),
            pointer: RenderInvariant::new(Point::ORIGIN),
            measured: MeasureCache::default(),
        };
        picker.set_colour(colour);
        picker
    }

    /// This picker editing the colour's opacity too.
    #[must_use]
    pub fn with_opacity(mut self, opacity: bool) -> Self {
        self.set_opacity(opacity);
        self
    }

    /// This picker picking on `view`.
    #[must_use]
    pub fn with_view(mut self, view: PickerView) -> Self {
        self.set_view(view);
        self
    }

    /// This picker showing its fields in `model`.
    #[must_use]
    pub fn with_model(mut self, model: ColourModel) -> Self {
        self.set_model(model);
        self
    }

    /// The colour shown: opaque unless the picker edits opacity.
    #[must_use]
    pub fn colour(&self) -> Rgba {
        let alpha = if self.opacity { self.alpha } else { u8::MAX };
        self.hsv.to_rgb().with_alpha(alpha)
    }

    /// Show `colour`, keeping the hue and saturation a grey or black has none
    /// of, and settle there, without reporting: the owner commits it and
    /// reports the repaint. Typing the picker held is replaced; a drag in
    /// progress carries on from the pointer.
    pub fn set_colour(&mut self, colour: Rgba) {
        self.hsv = Hsv::from_rgb(colour.without_alpha(), self.hsv);
        self.alpha = if self.opacity { colour.a } else { u8::MAX };
        self.typed = self.typed_from_colour();
        self.clipped = false;
        *self.settled = self.colour();
        *self.live = false;
        self.show_readouts(None);
    }

    /// The view the colour is picked on.
    #[must_use]
    pub const fn view(&self) -> PickerView {
        self.view
    }

    /// Pick on `view`. The owner keeps the choice and reports the repaint.
    pub fn set_view(&mut self, view: PickerView) {
        if view == self.view {
            return;
        }
        self.view = view;
        *self.drag = None;
        if !self.parts().as_slice().contains(&self.part) {
            self.part = self
                .parts()
                .as_slice()
                .first()
                .copied()
                .unwrap_or(Part::Hex);
            self.sync_children();
        }
    }

    /// The model the fields show the colour in.
    #[must_use]
    pub const fn model(&self) -> ColourModel {
        self.model
    }

    /// Show the fields in `model`. The owner keeps the choice and reports the
    /// repaint.
    pub fn set_model(&mut self, model: ColourModel) {
        if model == self.model {
            return;
        }
        let alpha = self.numbers[ALPHA].clone();
        self.model = model;
        self.numbers = fields_of(model);
        self.numbers[ALPHA] = alpha;
        self.typed = self.typed_from_colour();
        self.clipped = false;
        *self.drag = None;
        if !self.parts().as_slice().contains(&self.part) {
            self.part = self
                .parts()
                .as_slice()
                .first()
                .copied()
                .unwrap_or(Part::Hex);
        }
        self.sync_children();
        self.show_readouts(None);
    }

    /// Whether the values typed in a Lab or `LCh` field lay outside sRGB, the
    /// colour shown being their nearest in it.
    #[must_use]
    pub const fn clipped(&self) -> bool {
        self.clipped
    }

    /// Whether the picker edits opacity.
    #[must_use]
    pub const fn has_opacity(&self) -> bool {
        self.opacity
    }

    /// Edit opacity or not; without it the colour is opaque. The owner
    /// reports the repaint.
    pub fn set_opacity(&mut self, opacity: bool) {
        self.opacity = opacity;
        if !opacity {
            self.alpha = u8::MAX;
            if matches!(self.part, Part::Alpha | Part::Number(ALPHA)) {
                self.part = self
                    .parts()
                    .as_slice()
                    .first()
                    .copied()
                    .unwrap_or(Part::Hex);
            }
        }
        *self.settled = self.colour();
        self.sync_children();
        self.show_readouts(None);
    }

    /// The earlier colour shown beside the colour, if any.
    #[must_use]
    pub const fn earlier(&self) -> Option<Rgba> {
        self.earlier
    }

    /// Show `earlier` beside the colour — what it was before the user began
    /// on it — or nothing. The owner reports the repaint.
    pub fn set_earlier(&mut self, earlier: Option<Rgba>) {
        self.earlier = earlier;
        if earlier.is_none() && self.part == Part::Earlier {
            self.part = self
                .parts()
                .as_slice()
                .first()
                .copied()
                .unwrap_or(Part::Hex);
            self.sync_children();
        }
    }

    /// The picker's composed state; its fields take its enablement and
    /// authority, and the picker alone shows an authority bead.
    #[must_use]
    pub const fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the picker's composed state. The owner reports the repaint.
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
        self.sync_children();
    }

    /// Set the picker's keyboard focus, on the part it last had. An owner
    /// moving the focus away [`blur`](Self::blur)s it instead, so typing is
    /// not lost. The owner reports the repaint.
    pub fn set_focused(&mut self, focused: bool) {
        self.state.focus.focused = focused;
        self.sync_children();
    }

    /// Take the keyboard on the first part shown, or the last when the walk
    /// arrives backwards. The owner reports the repaint.
    pub fn enter_focus(&mut self, forward: bool, bounds: Rect, scale: Scale, theme: &Theme) {
        let layout = self.layout(bounds, scale, theme);
        let parts = self.parts();
        let mut shown = parts
            .as_slice()
            .iter()
            .copied()
            .filter(|&part| !layout.rect_of(part).is_empty());
        let part = if forward {
            shown.next()
        } else {
            shown.next_back()
        };
        self.part = part.unwrap_or(Part::Plane);
        self.set_focused(true);
    }

    /// Whether a press on the view or a strip is being dragged.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        matches!(*self.drag, Some(part) if is_dragged(part))
    }

    /// The least width the picker lays out in: the view and its strips, or
    /// the fields beneath them, whichever is wider.
    #[must_use]
    pub fn min_width(&self, scale: Scale, theme: &Theme) -> u32 {
        let measures = self.measures(scale, theme);
        (measures.plane_min + self.strips_width(measures)).max(measures.block_width())
    }

    /// The height the picker needs at `width` to show every part.
    #[must_use]
    pub fn measured_height(&self, width: u32, scale: Scale, theme: &Theme) -> u32 {
        let layout = self.layout(Rect::new(0, 0, width, u32::MAX / 4), scale, theme);
        [layout.plane, layout.alpha, layout.now]
            .into_iter()
            .chain(layout.numbers)
            .map(|rect| rect.bottom())
            .max()
            .and_then(|bottom| u32::try_from(bottom).ok())
            .unwrap_or(0)
    }

    /// Paint the picker into `surface` at `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let layout = self.layout(bounds, scale, theme);
        match self.view {
            PickerView::Square => {
                self.paint_plane(surface, &layout, theme);
                self.paint_hue(surface, &layout, theme);
            }
            PickerView::Wheel => self.paint_wheel(surface, &layout, theme),
            PickerView::Sliders => self.paint_tracks(surface, &layout, (scale, theme)),
        }
        self.paint_alpha(surface, &layout, (scale, theme));
        self.paint_swatch(surface, &layout, (scale, theme));
        if !layout.hex.is_empty() {
            self.hex.render(surface, layout.hex, scale, theme);
        }
        let caption = role_font(theme, scale, TextRole::Caption);
        let ink = foreground(theme, self.state.disposition());
        let channels = self.model.channels();
        for index in 0..FIELDS {
            if layout.numbers[index].is_empty() {
                continue;
            }
            self.numbers[index].render(surface, layout.numbers[index], scale, theme);
            let (label, unit) = channels
                .get(index)
                .map_or(("A", ""), |channel| (channel.label, channel.unit));
            paint_caption(surface, caption, label, layout.labels[index], ink);
            paint_caption(surface, caption, unit, layout.units[index], ink);
        }
        if self.state.focus.focused && is_ringed(self.part) {
            let rim = Color::from(theme.palette().rim_active);
            ring(
                surface,
                layout.rect_of(self.part),
                layout.measures.border * 2,
                rim,
            );
        }
        if let Some((colour, shape)) = resolve_bead(theme, self.state) {
            if let Some((x, y, w, _)) = surface_rect(bounds) {
                let size = scale.scale_length(theme.metrics().bead_size).max(3).min(w);
                paint_bead(surface, x + w - size, y, size, colour, shape);
            }
        }
    }

    /// Feed a pointer event. A primary press on the view or a strip starts a
    /// drag that motion carries and the release settles; a press and release
    /// on the earlier colour takes it back; a press on a field puts its caret
    /// there. A press moves the keyboard to the part it lands on, committing
    /// the field it leaves. The wheel steps a number field that has the
    /// keyboard and the pointer.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> PickerOutcome {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        if !self.state.is_actionable() {
            *self.drag = None;
            return PickerOutcome::Ignored;
        }
        let layout = self.layout(bounds, scale, theme);
        let style = (scale, theme);
        match event {
            InputEvent::PointerMoved { .. } => {
                if let Some(part) = (*self.drag).filter(|&part| is_dragged(part)) {
                    return self.drag_to(part, &layout, damage);
                }
                self.feed_fields(event, &layout, style, damage);
                PickerOutcome::Ignored
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => self.press(event, bounds, &layout, style, damage),
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => self.release(event, &layout, style, damage),
            InputEvent::PointerScrolled { .. } => {
                match layout.part_at(*self.pointer, &self.parts(), self.hsv.hue) {
                    Some(Part::Number(index)) => {
                        let rect = layout.numbers[index];
                        let stepped =
                            self.numbers[index].on_pointer(event, rect, scale, theme, damage);
                        self.number_outcome(index, stepped, &layout, damage)
                    }
                    _ => PickerOutcome::Ignored,
                }
            }
            _ => PickerOutcome::Ignored,
        }
    }

    /// Feed a key to a focused picker. Tab and Shift+Tab walk the parts,
    /// committing a field they leave, and answer [`PickerOutcome::Ignored`]
    /// past either end so the owner carries the focus on. On the plane and
    /// the triangle the arrows step saturation and value, on a strip, the
    /// ring or a slider they step it, Shift steps ten times as far, and Home
    /// and End go to an end. Enter or Space on the earlier colour takes it
    /// back. A field takes every key but Tab and a chord it has no use for,
    /// which are the owner's — Ctrl+A selects the field's text, Ctrl+S still
    /// saves. Escape abandons a drag, or takes back what a field was typed
    /// since the last settle, and is otherwise the owner's.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> PickerOutcome {
        if !self.state.focus.focused || !self.state.is_actionable() {
            return PickerOutcome::Ignored;
        }
        let layout = self.layout(bounds, scale, theme);
        if layout.rect_of(self.part).is_empty() || !self.parts().as_slice().contains(&self.part) {
            self.part = self
                .parts()
                .as_slice()
                .first()
                .copied()
                .unwrap_or(Part::Hex);
            self.sync_children();
        }
        match key {
            Key::Named(NamedKey::Tab) => return self.walk(!modifiers.shift, &layout, damage),
            Key::Named(NamedKey::Escape) if self.is_dragging() => {
                return self.abandon_drag(&layout, damage);
            }
            _ => {}
        }
        match self.part {
            part if is_dragged(part) => self.step(key, modifiers, &layout, damage),
            Part::Earlier => match key {
                Key::Named(NamedKey::Enter) | Key::Char(' ') => {
                    self.take_back_earlier(&layout, damage)
                }
                _ => PickerOutcome::Ignored,
            },
            Part::Hex | Part::Number(_) if owner_chord(key, modifiers) => PickerOutcome::Ignored,
            Part::Hex => self.hex_key(key, modifiers, &layout, damage),
            Part::Number(index) => {
                let rect = layout.numbers[index];
                let action = self.numbers[index].on_key(key, modifiers, rect, damage);
                if action.is_none() && key == Key::Named(NamedKey::Escape) {
                    return PickerOutcome::Ignored;
                }
                self.number_outcome(index, action, &layout, damage)
            }
            _ => PickerOutcome::Ignored,
        }
    }

    /// Commit a field the keyboard rests on, keeping the focus there: what an
    /// owner does before acting on the colour itself.
    pub fn commit(
        &mut self,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> PickerOutcome {
        let layout = self.layout(bounds, scale, theme);
        self.commit_part(&layout, damage)
    }

    /// Give up the keyboard as the owner moves it elsewhere, committing a
    /// field it held.
    pub fn blur(
        &mut self,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> PickerOutcome {
        let layout = self.layout(bounds, scale, theme);
        let committed = self.commit_part(&layout, damage);
        if self.state.focus.focused {
            damage.add(layout.rect_of(self.part));
        }
        self.set_focused(false);
        committed
    }

    /// End a drag where it stands, as anything that takes the pointer from
    /// the picker must.
    pub fn finish_drag(&mut self) -> PickerOutcome {
        if !self.is_dragging() {
            return PickerOutcome::Ignored;
        }
        *self.drag = None;
        self.settle()
    }

    /// The parts shown, in the order Tab walks them.
    fn parts(&self) -> Parts {
        let mut parts = Parts {
            parts: [Part::Hex; MOST_PARTS],
            count: 0,
        };
        let channels = self.model.channels().len();
        match self.view {
            PickerView::Square => {
                parts.push(Part::Plane);
                parts.push(Part::Hue);
            }
            PickerView::Wheel => {
                parts.push(Part::Ring);
                parts.push(Part::Triangle);
            }
            PickerView::Sliders => (0..channels).for_each(|index| parts.push(Part::Track(index))),
        }
        if self.opacity {
            parts.push(Part::Alpha);
        }
        if self.earlier.is_some() {
            parts.push(Part::Earlier);
        }
        parts.push(Part::Hex);
        (0..channels).for_each(|index| parts.push(Part::Number(index)));
        if self.opacity {
            parts.push(Part::Number(ALPHA));
        }
        parts
    }

    fn press(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        layout: &Layout,
        style: (Scale, &Theme),
        damage: &mut Region,
    ) -> PickerOutcome {
        let Some(part) = layout.part_at(*self.pointer, &self.parts(), self.hsv.hue) else {
            return if bounds.contains(*self.pointer) {
                PickerOutcome::Taken
            } else {
                PickerOutcome::Ignored
            };
        };
        let left = self.move_to(part, layout, damage);
        *self.drag = Some(part);
        let pressed = match part {
            part if is_dragged(part) => self.drag_to(part, layout, damage),
            Part::Hex | Part::Number(_) => {
                self.feed_field(part, event, layout, style, damage);
                PickerOutcome::Taken
            }
            _ => PickerOutcome::Taken,
        };
        // A colour still moving settles when the drag does, carrying the
        // field just left with it.
        match pressed {
            PickerOutcome::Edited(_) => pressed,
            _ => left,
        }
    }

    fn release(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        style: (Scale, &Theme),
        damage: &mut Region,
    ) -> PickerOutcome {
        match self.drag.take() {
            Some(part) if is_dragged(part) => self.settle(),
            Some(Part::Earlier)
                if layout.part_at(*self.pointer, &self.parts(), self.hsv.hue)
                    == Some(Part::Earlier) =>
            {
                self.take_back_earlier(layout, damage)
            }
            Some(part @ (Part::Hex | Part::Number(_))) => {
                self.feed_field(part, event, layout, style, damage);
                PickerOutcome::Taken
            }
            _ => PickerOutcome::Ignored,
        }
    }

    /// Pass a pointer event to the text field `part` names.
    fn feed_field(
        &mut self,
        part: Part,
        event: &InputEvent,
        layout: &Layout,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        match part {
            Part::Hex => {
                self.hex.on_pointer(event, layout.hex, scale, theme, damage);
            }
            Part::Number(index) => {
                if let (Some(field), Some(&rect)) =
                    (self.numbers.get_mut(index), layout.numbers.get(index))
                {
                    field.on_pointer(event, rect, scale, theme, damage);
                }
            }
            _ => {}
        }
    }

    /// Pass pointer motion to every text field, for its hover and selection.
    fn feed_fields(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        style: (Scale, &Theme),
        damage: &mut Region,
    ) {
        self.feed_field(Part::Hex, event, layout, style, damage);
        for index in 0..FIELDS {
            if !layout.numbers[index].is_empty() {
                self.feed_field(Part::Number(index), event, layout, style, damage);
            }
        }
    }

    /// Abandon a drag, returning the colour to where it began.
    fn abandon_drag(&mut self, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        *self.drag = None;
        let settled = *self.settled;
        let hsv = Hsv::from_rgb(settled.without_alpha(), self.hsv);
        self.set_coordinates(hsv, settled.a, None, false, layout, damage);
        self.settle()
    }

    /// Move the keyboard to `part`, committing a field it leaves.
    fn move_to(&mut self, part: Part, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        if part == self.part {
            return PickerOutcome::Taken;
        }
        let left = self.commit_part(layout, damage);
        if self.state.focus.focused {
            damage.add(layout.rect_of(self.part));
            damage.add(layout.rect_of(part));
        }
        self.part = part;
        self.sync_children();
        left
    }

    /// Tab to the next part shown, or back to the previous; `Ignored` past
    /// either end.
    fn walk(&mut self, forward: bool, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        let parts = self.parts();
        let parts = parts.as_slice();
        let at = parts
            .iter()
            .position(|&part| part == self.part)
            .unwrap_or(0);
        let shown = |part: &&Part| !layout.rect_of(**part).is_empty();
        let next = if forward {
            parts.iter().skip(at + 1).find(shown)
        } else {
            parts.iter().take(at).rev().find(shown)
        };
        match next {
            Some(&part) => self.move_to(part, layout, damage),
            None => PickerOutcome::Ignored,
        }
    }

    /// A key on the view or a strip: one step, settled.
    fn step(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        damage: &mut Region,
    ) -> PickerOutcome {
        let Key::Named(named) = key else {
            return PickerOutcome::Ignored;
        };
        if let Part::Track(index) = self.part {
            let Some(channel) = self.model.channels().get(index) else {
                return PickerOutcome::Ignored;
            };
            let value = self.typed[index];
            let line = if modifiers.shift { 10 } else { 1 };
            let to = match named {
                NamedKey::Left | NamedKey::Down => value - line,
                NamedKey::Right | NamedKey::Up => value + line,
                NamedKey::PageDown => value - channel.page,
                NamedKey::PageUp => value + channel.page,
                NamedKey::Home => channel.least,
                NamedKey::End => channel.most,
                _ => return PickerOutcome::Ignored,
            };
            return if self.apply_channel(index, to, Part::Track(index), layout, damage) {
                self.settle()
            } else {
                PickerOutcome::Taken
            };
        }
        let stepped = match self.part {
            Part::Plane | Part::Triangle => {
                plane_step(self.hsv, named, modifiers.shift).map(|hsv| (hsv, self.alpha))
            }
            Part::Hue | Part::Ring => hue_step(self.hsv.hue, named, modifiers.shift)
                .map(|hue| (Hsv { hue, ..self.hsv }, self.alpha)),
            Part::Alpha => {
                alpha_step(self.alpha, named, modifiers.shift).map(|alpha| (self.hsv, alpha))
            }
            _ => None,
        };
        let Some((hsv, alpha)) = stepped else {
            return PickerOutcome::Ignored;
        };
        if self.set_coordinates(hsv, alpha, None, false, layout, damage) {
            self.settle()
        } else {
            PickerOutcome::Taken
        }
    }

    /// A key in the hex field.
    fn hex_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        damage: &mut Region,
    ) -> PickerOutcome {
        match self.hex.on_key(key, modifiers, layout.hex, damage) {
            Some(TextAction::Edited) => {
                let read = read_hex(self.hex.text(), self.opacity);
                let state = self.hex.state();
                self.hex
                    .set_state(state.with_validation(ValidationState::of(read.is_some())));
                let Some(colour) = read else {
                    return PickerOutcome::Taken;
                };
                let hsv = Hsv::from_rgb(colour.without_alpha(), self.hsv);
                if self.set_coordinates(hsv, colour.a, Some(Part::Hex), false, layout, damage) {
                    *self.live = true;
                    PickerOutcome::Edited(self.colour())
                } else {
                    PickerOutcome::Taken
                }
            }
            Some(TextAction::Submitted) => self.commit_part(layout, damage),
            Some(TextAction::Cancelled) => {
                if !*self.live && self.hex.text() == self.hex_text().as_str() {
                    return PickerOutcome::Ignored;
                }
                let settled = *self.settled;
                let hsv = Hsv::from_rgb(settled.without_alpha(), self.hsv);
                self.set_coordinates(hsv, settled.a, None, false, layout, damage);
                if self.show_hex() {
                    damage.add(layout.hex);
                }
                self.settle()
            }
            None => PickerOutcome::Taken,
        }
    }

    /// What number field `index`'s action means for the colour.
    fn number_outcome(
        &mut self,
        index: usize,
        action: Option<NumberAction>,
        layout: &Layout,
        damage: &mut Region,
    ) -> PickerOutcome {
        match action {
            Some(NumberAction::Edited { value }) => {
                if self.apply_field(index, value, layout, damage) {
                    *self.live = true;
                    PickerOutcome::Edited(self.colour())
                } else {
                    PickerOutcome::Taken
                }
            }
            Some(NumberAction::Settled { value }) => {
                self.apply_field(index, value, layout, damage);
                self.settle()
            }
            None => PickerOutcome::Taken,
        }
    }

    /// Commit a field the keyboard is leaving.
    fn commit_part(&mut self, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        match self.part {
            Part::Hex => {
                if self.show_hex() {
                    damage.add(layout.hex);
                }
                self.settle()
            }
            Part::Number(index) => {
                let rect = layout.numbers[index];
                if let Some(NumberAction::Settled { value }) =
                    self.numbers[index].commit(rect, damage)
                {
                    self.apply_field(index, value, layout, damage);
                }
                self.settle()
            }
            _ => PickerOutcome::Taken,
        }
    }

    fn take_back_earlier(&mut self, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        let Some(earlier) = self.earlier else {
            return PickerOutcome::Ignored;
        };
        let hsv = Hsv::from_rgb(earlier.without_alpha(), self.hsv);
        self.set_coordinates(hsv, earlier.a, None, false, layout, damage);
        self.settle()
    }

    /// The drag on `part` carried to the pointer.
    fn drag_to(&mut self, part: Part, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        let area = layout.inner(part);
        let pointer = *self.pointer;
        let across = along(offset(pointer.x, area.left(), area.width), area.width);
        let down = along(offset(pointer.y, area.top(), area.height), area.height);
        let (x, y) = (f64::from(pointer.x), f64::from(pointer.y));
        let (hsv, alpha) = match part {
            Part::Plane => {
                let value = Fraction::from_raw(u16::MAX - down.raw());
                (
                    Hsv {
                        saturation: across,
                        value,
                        ..self.hsv
                    },
                    self.alpha,
                )
            }
            Part::Hue => (
                Hsv {
                    hue: hue_at(down),
                    ..self.hsv
                },
                self.alpha,
            ),
            Part::Ring => (
                Hsv {
                    hue: Wheel::in_area(area).hue_at(x, y),
                    ..self.hsv
                },
                self.alpha,
            ),
            Part::Triangle => {
                let (saturation, value) =
                    Wheel::in_area(area).sv_at(self.hsv.hue, (x, y), self.hsv.saturation);
                (
                    Hsv {
                        saturation,
                        value,
                        ..self.hsv
                    },
                    self.alpha,
                )
            }
            Part::Track(index) => {
                let Some(channel) = self.model.channels().get(index) else {
                    return PickerOutcome::Taken;
                };
                let span = i64::from(channel.most - channel.least);
                let value = i64::from(channel.least)
                    + (i64::from(across.raw()) * span + i64::from(u16::MAX) / 2)
                        / i64::from(u16::MAX);
                let value = i32::try_from(value).unwrap_or(channel.least);
                return if self.apply_channel(index, value, part, layout, damage) {
                    *self.live = true;
                    PickerOutcome::Edited(self.colour())
                } else {
                    PickerOutcome::Taken
                };
            }
            _ if self.view == PickerView::Sliders => (self.hsv, across.byte()),
            _ => (self.hsv, u8::MAX - down.byte()),
        };
        if self.set_coordinates(hsv, alpha, None, false, layout, damage) {
            *self.live = true;
            PickerOutcome::Edited(self.colour())
        } else {
            PickerOutcome::Taken
        }
    }

    /// Set field `index` — a channel of the model, or the alpha — to
    /// `value`, answering whether the colour moved.
    fn apply_field(
        &mut self,
        index: usize,
        value: i32,
        layout: &Layout,
        damage: &mut Region,
    ) -> bool {
        if index == ALPHA {
            let alpha = u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX);
            return self.set_coordinates(
                self.hsv,
                alpha,
                Some(Part::Number(ALPHA)),
                false,
                layout,
                damage,
            );
        }
        self.apply_channel(index, value, Part::Number(index), layout, damage)
    }

    /// Set channel `index` of the model to `value` through `typing`, the
    /// other channels held as shown; answers whether anything moved.
    fn apply_channel(
        &mut self,
        index: usize,
        value: i32,
        typing: Part,
        layout: &Layout,
        damage: &mut Region,
    ) -> bool {
        let Some(channel) = self.model.channels().get(index) else {
            return false;
        };
        let mut typed = self.typed;
        typed[index] = value.clamp(channel.least, channel.most);
        let (hsv, clipped) = match self.model {
            // The picker's own coordinates: only the channel edited moves, the
            // others keeping their full precision.
            ColourModel::Hsv => {
                let whole = u32::try_from(typed[index]).unwrap_or(0);
                let mut hsv = self.hsv;
                match index {
                    0 => hsv.hue = Hue::from_degrees(whole),
                    1 => hsv.saturation = Fraction::from_percent(whole),
                    _ => hsv.value = Fraction::from_percent(whole),
                }
                (hsv, false)
            }
            model => {
                let (rgb, clipped) = model.colour(typed);
                (Hsv::from_rgb(rgb, self.hsv), clipped)
            }
        };
        let held = typed != self.typed || clipped != self.clipped;
        if clipped != self.clipped {
            damage.add(layout.now);
        }
        self.typed = typed;
        self.clipped = clipped;
        let moved = self.set_coordinates(hsv, self.alpha, Some(typing), true, layout, damage);
        if held && !moved {
            layout.report(self.show_readouts(Some(typing)), damage);
        }
        moved || held
    }

    /// Move the colour to `hsv` at `alpha`, reporting each part whose drawing
    /// that changes and showing it in every readout but the one being typed
    /// in; with `held`, the model's values stay as typed rather than being
    /// read anew from the colour. Answers whether the colour moved.
    fn set_coordinates(
        &mut self,
        hsv: Hsv,
        alpha: u8,
        typing: Option<Part>,
        held: bool,
        layout: &Layout,
        damage: &mut Region,
    ) -> bool {
        let alpha = if self.opacity { alpha } else { u8::MAX };
        if (hsv, alpha) == (self.hsv, self.alpha) {
            return false;
        }
        match self.view {
            PickerView::Square => {
                if hsv.hue != self.hsv.hue {
                    damage.add(layout.plane);
                    damage.add(layout.hue);
                } else if (hsv.saturation, hsv.value) != (self.hsv.saturation, self.hsv.value) {
                    damage.add(layout.marker(self.hsv));
                    damage.add(layout.marker(hsv));
                }
            }
            PickerView::Wheel | PickerView::Sliders => damage.add(layout.plane),
        }
        damage.add(layout.alpha);
        damage.add(layout.now);
        self.hsv = hsv;
        self.alpha = alpha;
        if !held {
            self.typed = self.typed_from_colour();
            if self.clipped {
                self.clipped = false;
            }
        }
        layout.report(self.show_readouts(typing), damage);
        true
    }

    /// The model's values of the colour, the hue a grey has none of kept
    /// from those shown.
    fn typed_from_colour(&self) -> [i32; MOST_CHANNELS] {
        match self.model {
            ColourModel::Hsv => [
                i32::try_from(self.hsv.hue.degrees()).unwrap_or(0),
                i32::try_from(self.hsv.saturation.percent()).unwrap_or(0),
                i32::try_from(self.hsv.value.percent()).unwrap_or(0),
                0,
            ],
            model => model.values(self.hsv.to_rgb(), self.typed),
        }
    }

    /// End an interaction: `Settled` when the colour moved since the last
    /// settle, live or otherwise.
    fn settle(&mut self) -> PickerOutcome {
        let colour = self.colour();
        if !*self.live && colour == *self.settled {
            return PickerOutcome::Taken;
        }
        *self.settled = colour;
        *self.live = false;
        PickerOutcome::Settled(colour)
    }

    /// Show the colour in every readout but `typing`.
    fn show_readouts(&mut self, typing: Option<Part>) -> Rewritten {
        let mut rewritten = Rewritten {
            tracks: self.view == PickerView::Sliders,
            ..Rewritten::default()
        };
        let channels = self.model.channels().len();
        for index in 0..FIELDS {
            let value = if index == ALPHA {
                i32::from(self.alpha)
            } else if index < channels {
                self.typed[index]
            } else {
                continue;
            };
            if typing != Some(Part::Number(index)) && self.numbers[index].value() != value {
                self.numbers[index].set_value(value);
                rewritten.numbers[index] = true;
            }
        }
        rewritten.hex = typing != Some(Part::Hex) && self.show_hex();
        rewritten
    }

    /// Show the colour's notation in the hex field, answering whether that
    /// changed it.
    fn show_hex(&mut self) -> bool {
        let text = self.hex_text();
        let state = self.hex.state();
        if self.hex.text() == text.as_str() && state.validation == ValidationState::Valid {
            return false;
        }
        self.hex.set_text(text.as_str());
        self.hex
            .set_state(state.with_validation(ValidationState::Valid));
        true
    }

    /// The colour as `#rrggbb`, or `#rrggbbaa` while translucent.
    fn hex_text(&self) -> ArrayString<HEX_LEN> {
        use core::fmt::Write as _;
        let mut text = ArrayString::new();
        let _ = write!(text, "{}", self.colour().hex().hashed());
        text
    }

    /// Give the fields the picker's enablement and authority — enabled only
    /// while the picker may act, so a denial is marked once, by the picker —
    /// and the keyboard to the field it rests on.
    fn sync_children(&mut self) {
        let enabled = self.state.is_actionable();
        let focused = self.state.focus.focused;
        let part = self.part;
        let share = |own: ControlState, rests: bool| {
            let mut state = ControlState { enabled, ..own };
            state.focus.focused = focused && rests;
            state
        };
        self.hex
            .set_state(share(self.hex.state(), part == Part::Hex));
        for (index, field) in self.numbers.iter_mut().enumerate() {
            field.set_state(share(field.state(), part == Part::Number(index)));
        }
    }

    /// The width the strips beside the view take: the hue strip on the
    /// square, and the opacity strip where there is one; the sliders stack
    /// the opacity as a slider of its own.
    fn strips_width(&self, measures: Measures) -> u32 {
        let strips = u32::from(self.view == PickerView::Square)
            + u32::from(self.opacity && self.view != PickerView::Sliders);
        (measures.strip + measures.gap) * strips
    }

    /// The lengths the layout is built from, measured once for each scale
    /// and set of faces.
    fn measures(&self, scale: Scale, theme: &Theme) -> Measures {
        let caption = role_font(theme, scale, TextRole::Caption);
        let body = role_font(theme, scale, TextRole::Body);
        let metrics = theme.metrics();
        let key = (
            scale,
            caption,
            body,
            metrics.control_gap,
            metrics.control_inset,
            metrics.control_height,
            plate_border(theme, scale),
        );
        if let Some((held, measures)) = self.measured.0.get() {
            if held == key {
                return measures;
            }
        }
        let gap = scale.scale_length(metrics.control_gap).max(1);
        let border = plate_border(theme, scale).max(1);
        // Measured across every model, so switching one moves nothing.
        let channels = || {
            ColourModel::ALL
                .into_iter()
                .flat_map(|model| model.channels().iter())
        };
        let label = channels()
            .map(|channel| caption.text_width(channel.label))
            .fold(caption.text_width("A"), u32::max);
        let unit = channels()
            .map(|channel| caption.text_width(channel.unit))
            .max()
            .unwrap_or(0);
        let field = channels()
            .map(|channel| {
                NumberField::new(0, channel.least, channel.most)
                    .with_decimals(channel.places)
                    .preferred_width(scale, theme)
            })
            .fold(self.numbers[ALPHA].preferred_width(scale, theme), u32::max);
        let edge = border.saturating_add(scale.scale_length(metrics.control_inset));
        let measures = Measures {
            gap,
            small: (gap / 2).max(1),
            row: NumberField::height(scale, theme),
            border,
            strip: scale.scale_length(STRIP).max(1),
            marker: scale.scale_length(MARKER).max(2),
            plane_min: scale.scale_length(PLANE_MIN),
            plane_max: scale.scale_length(PLANE_MAX),
            label,
            field,
            unit,
            hex: body
                .text_width("#00000000")
                .saturating_add(edge * 2)
                .saturating_add(scale.scale_length(2)),
        };
        self.measured.0.set(Some((key, measures)));
        measures
    }

    /// The rows of the number grid: the model's channels and the alpha, two
    /// to a row.
    fn grid_rows(&self) -> u32 {
        let fields = self.model.channels().len() + usize::from(self.opacity);
        u32::try_from(fields.div_ceil(2)).unwrap_or(1)
    }

    /// The view's natural height across `width`: the square at three
    /// quarters of its width, the wheel as tall as wide, each held between
    /// the least and the most the plane is drawn at; the sliders as tall as
    /// their stack.
    fn view_height(&self, width: u32, m: Measures) -> u32 {
        match self.view {
            PickerView::Square => (width * 3 / 4).clamp(m.plane_min, m.plane_max),
            PickerView::Wheel => width.clamp(m.plane_min, m.plane_max),
            PickerView::Sliders => {
                let tracks = self.model.channels().len() + usize::from(self.opacity);
                m.tracks_height(u32::try_from(tracks).unwrap_or(1))
            }
        }
    }

    /// The picker laid out in `bounds`.
    fn layout(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Layout {
        let m = self.measures(scale, theme);
        let (block_w, block_h) = (m.block_width(), m.block_height(self.grid_rows()));
        let strips = self.strips_width(m);
        let room = bounds.height;
        let least = match self.view {
            PickerView::Sliders => self.view_height(bounds.width, m),
            _ => m.plane_min,
        };
        let beside = bounds.width >= m.plane_min + strips + m.gap + block_w;
        let (plane_w, plane_h, block, shown) = if beside {
            let plane_w = bounds.width - block_w - m.gap - strips;
            let block_x = bounds.left() + to_i32(bounds.width - block_w);
            let block = Rect::new(block_x, bounds.top(), block_w, block_h.min(room));
            let shown = (room >= m.row, room >= block_h);
            let natural = match self.view {
                PickerView::Sliders => self.view_height(plane_w, m),
                _ => block_h.max(m.plane_min),
            };
            (plane_w, natural.min(room), block, shown)
        } else {
            let plane_w = bounds.width.saturating_sub(strips);
            let natural = self.view_height(plane_w, m);
            let (full, hex_only) = (m.gap + block_h, m.gap + m.row);
            let (plane_h, shown) = if room >= natural + full {
                (natural, (true, true))
            } else if room >= least + full {
                (room - full, (true, true))
            } else if room >= least + hex_only {
                ((room - hex_only).min(natural), (true, false))
            } else {
                (room, (false, false))
            };
            let top = bounds.top() + to_i32(plane_h + m.gap);
            let block = Rect::new(
                bounds.left(),
                top,
                bounds.width,
                room.saturating_sub(plane_h + m.gap),
            );
            (plane_w, plane_h, block, shown)
        };
        let plane = match self.view {
            PickerView::Wheel => {
                let side = plane_w.min(plane_h);
                Rect::new(bounds.left(), bounds.top(), side, side)
            }
            _ => Rect::new(bounds.left(), bounds.top(), plane_w, plane_h),
        };
        let strip = |n: u32| {
            let x = plane.right() + to_i32(m.gap + n * (m.strip + m.gap));
            Rect::new(x, bounds.top(), m.strip, plane_h)
        };
        let mut layout = Layout {
            measures: m,
            view: self.view,
            plane,
            hue: Rect::EMPTY,
            tracks: [Rect::EMPTY; MOST_CHANNELS],
            track_labels: [Rect::EMPTY; MOST_CHANNELS],
            alpha: Rect::EMPTY,
            alpha_label: Rect::EMPTY,
            earlier: Rect::EMPTY,
            now: Rect::EMPTY,
            hex: Rect::EMPTY,
            numbers: [Rect::EMPTY; FIELDS],
            labels: [Rect::EMPTY; FIELDS],
            units: [Rect::EMPTY; FIELDS],
        };
        match self.view {
            PickerView::Square => {
                layout.hue = strip(0);
                if self.opacity {
                    layout.alpha = strip(1);
                }
            }
            PickerView::Wheel => {
                if self.opacity {
                    let x = plane.right() + to_i32(m.gap);
                    layout.alpha = Rect::new(x, bounds.top(), m.strip, plane.height);
                }
            }
            PickerView::Sliders => self.lay_tracks(&mut layout),
        }
        let (show_hex, show_grid) = shown;
        if show_hex {
            self.lay_hex_row(&mut layout, block);
        }
        if show_grid {
            self.lay_grid(&mut layout, block);
        }
        layout
    }

    /// The sliders down the view's area, each a row tall and captioned, the
    /// opacity last.
    fn lay_tracks(&self, layout: &mut Layout) {
        let m = layout.measures;
        let area = layout.plane;
        let mut top = area.top();
        let mut lay = |label: &mut Rect, track: &mut Rect| {
            if top.saturating_add_unsigned(m.row) > area.bottom() {
                return;
            }
            *label = Rect::new(area.left(), top, m.label, m.row);
            let x = area.left() + to_i32(m.label + m.small);
            *track = Rect::new(x, top, area.width.saturating_sub(m.label + m.small), m.row);
            top = top.saturating_add_unsigned(m.row + m.gap);
        };
        let channels = self.model.channels().len();
        for index in 0..channels {
            lay(&mut layout.track_labels[index], &mut layout.tracks[index]);
        }
        if self.opacity {
            lay(&mut layout.alpha_label, &mut layout.alpha);
        }
    }

    /// The swatch and the hex field across the top of `block`.
    fn lay_hex_row(&self, layout: &mut Layout, block: Rect) {
        let m = layout.measures;
        let swatch = Rect::new(block.left(), block.top(), m.swatch(), m.row);
        if self.earlier.is_some() {
            let half = swatch.width / 2;
            layout.earlier = Rect::new(swatch.left(), swatch.top(), half, m.row);
            layout.now = Rect::new(
                swatch.left() + to_i32(half),
                swatch.top(),
                swatch.width - half,
                m.row,
            );
        } else {
            layout.now = swatch;
        }
        let rest = block.width.saturating_sub(m.swatch() + m.gap);
        layout.hex = Rect::new(swatch.right() + to_i32(m.gap), block.top(), rest, m.row);
    }

    /// The number fields under the hex row of `block`: the model's channels
    /// and then the alpha, two to a row.
    fn lay_grid(&self, layout: &mut Layout, block: Rect) {
        let m = layout.measures;
        let top = block.top() + to_i32(m.row + m.gap);
        let channels = self.model.channels().len();
        let fields = (0..channels).chain(self.opacity.then_some(ALPHA));
        for (cell, index) in fields.enumerate() {
            let (column, row) = (
                u32::try_from(cell % 2).unwrap_or(0),
                u32::try_from(cell / 2).unwrap_or(0),
            );
            let x = block.left() + to_i32((m.column() + m.gap) * column);
            let y = top + to_i32((m.row + m.gap) * row);
            let field_x = x + to_i32(m.label + m.small);
            layout.labels[index] = Rect::new(x, y, m.label, m.row);
            layout.numbers[index] = Rect::new(field_x, y, m.field, m.row);
            layout.units[index] = Rect::new(field_x + to_i32(m.field + m.small), y, m.unit, m.row);
        }
    }

    fn paint_plane(&self, surface: &mut Surface, layout: &Layout, theme: &Theme) {
        let area = layout.inner(Part::Plane);
        let Some((x, y, w, h)) = surface_rect(area) else {
            return;
        };
        for row in 0..h {
            let value = Fraction::from_raw(u16::MAX - along(row, h).raw());
            let Some((first, span)) = surface.row_span_mut(y + row, x, w) else {
                continue;
            };
            for (column, pixel) in (first - x..).zip(span.iter_mut()) {
                let colour = Hsv::new(self.hsv.hue, along(column, w), value).to_rgb();
                *pixel = Color::from(colour).premultiply();
            }
        }
        let border = layout.measures.border;
        ring(
            surface,
            layout.plane,
            border,
            Color::from(theme.palette().rim),
        );
        let centre = marker_centre(area, self.hsv);
        paint_marker(
            surface,
            layout.plane,
            centre,
            layout.measures,
            self.hsv.to_rgb(),
        );
        self.veil(surface, layout.plane, theme);
    }

    fn paint_hue(&self, surface: &mut Surface, layout: &Layout, theme: &Theme) {
        let Some((x, y, w, h)) = surface_rect(layout.inner(Part::Hue)) else {
            return;
        };
        for row in 0..h {
            let pure = Hsv::new(hue_at(along(row, h)), Fraction::ALL, Fraction::ALL);
            let pixel = Color::from(pure.to_rgb()).premultiply();
            if let Some((_, span)) = surface.row_span_mut(y + row, x, w) {
                span.fill(pixel);
            }
        }
        let border = layout.measures.border;
        ring(
            surface,
            layout.hue,
            border,
            Color::from(theme.palette().rim),
        );
        let row = position(hue_fraction(self.hsv.hue), h);
        paint_strip_marker(surface, layout.hue, (x, y, w), row, border);
        self.veil(surface, layout.hue, theme);
    }

    /// The wheel: the hue ring and, within it, the triangle of saturation
    /// and value at the colour's hue, each edge smoothed by its coverage,
    /// with a marker on each.
    fn paint_wheel(&self, surface: &mut Surface, layout: &Layout, theme: &Theme) {
        let area = layout.inner(Part::Ring);
        let Some((left, top, width, height)) = surface_rect(area) else {
            return;
        };
        let wheel = Wheel::in_area(area);
        let corners = wheel.corners(self.hsv.hue);
        let altitude = 1.5 * (wheel.inner - 2.0).max(1.0);
        let ground = theme.palette().surface_raised;
        let flat = Color::from(ground).premultiply();
        for row in 0..height {
            let Some((first, span)) = surface.row_span_mut(top + row, left, width) else {
                continue;
            };
            let py = f64::from(top + row) + 0.5;
            for (column, pixel) in (first..).zip(span.iter_mut()) {
                let px = f64::from(column) + 0.5;
                let radius = wheel.radius_at(px, py);
                let ring_cover = mathf::clamp(wheel.outer + 0.5 - radius, 0.0, 1.0)
                    * mathf::clamp(radius - wheel.inner + 0.5, 0.0, 1.0);
                if ring_cover > 0.0 {
                    let pure = Hsv::new(wheel.hue_at(px, py), Fraction::ALL, Fraction::ALL);
                    *pixel = mixed(ground, pure.to_rgb().opaque(), ring_cover);
                    continue;
                }
                let (hw, ww, kw) = barycentric(corners, (px, py));
                let inside = mathf::fmin(hw, mathf::fmin(ww, kw)) * altitude;
                let cover = mathf::clamp(inside + 0.5, 0.0, 1.0);
                if cover <= 0.0 {
                    *pixel = flat;
                    continue;
                }
                let value = mathf::clamp(hw + ww, 0.0, 1.0);
                let saturation = if value <= f64::EPSILON {
                    0.0
                } else {
                    mathf::clamp(hw / value, 0.0, 1.0)
                };
                let colour = Hsv::new(
                    self.hsv.hue,
                    Fraction::from_f64(saturation),
                    Fraction::from_f64(value),
                );
                *pixel = mixed(ground, colour.to_rgb().opaque(), cover);
            }
        }
        let m = layout.measures;
        let theta = degrees_of(self.hsv.hue).to_radians();
        let mid = wheel.outer.midpoint(wheel.inner);
        let on_ring = (
            mathf::round_i32(wheel.cx + mid * mathf::cos(theta)),
            mathf::round_i32(wheel.cy - mid * mathf::sin(theta)),
        );
        let pure = Hsv::new(self.hsv.hue, Fraction::ALL, Fraction::ALL).to_rgb();
        paint_marker(surface, layout.plane, on_ring, m, pure);
        let (sx, sy) = wheel.point_of(self.hsv);
        let in_triangle = (mathf::round_i32(sx), mathf::round_i32(sy));
        paint_marker(surface, layout.plane, in_triangle, m, self.hsv.to_rgb());
        self.veil(surface, layout.plane, theme);
    }

    /// The sliders: each channel's groove swept through its range with the
    /// other channels held as typed, its caption beside it and a marker at
    /// its value.
    fn paint_tracks(
        &self,
        surface: &mut Surface,
        layout: &Layout,
        (scale, theme): (Scale, &Theme),
    ) {
        let caption = role_font(theme, scale, TextRole::Caption);
        let ink = foreground(theme, self.state.disposition());
        let border = layout.measures.border;
        for (index, channel) in self.model.channels().iter().enumerate() {
            let track = layout.tracks[index];
            paint_caption(
                surface,
                caption,
                channel.label,
                layout.track_labels[index],
                ink,
            );
            let Some((x, y, w, h)) = surface_rect(track.inset(border)) else {
                continue;
            };
            let span = i64::from(channel.most - channel.least);
            for column in 0..w {
                let along_track = along(column, w);
                let mut values = self.typed;
                values[index] = channel.least
                    + i32::try_from(i64::from(along_track.raw()) * span / i64::from(u16::MAX))
                        .unwrap_or(0);
                let rgb = match self.model {
                    ColourModel::Hsv => Hsv::new(
                        Hue::from_degrees(u32::try_from(values[0]).unwrap_or(0)),
                        Fraction::from_percent(u32::try_from(values[1]).unwrap_or(0)),
                        Fraction::from_percent(u32::try_from(values[2]).unwrap_or(0)),
                    )
                    .to_rgb(),
                    model => model.colour(values).0,
                };
                surface.fill_rect(x + column, y, 1, h, Color::from(rgb));
            }
            ring(surface, track, border, Color::from(theme.palette().rim));
            let at = u32::try_from(
                i64::from(self.typed[index] - channel.least) * i64::from(w.saturating_sub(1))
                    / span.max(1),
            )
            .unwrap_or(0);
            paint_column_marker(surface, track, (x, y, h), at, border);
            self.veil(surface, track, theme);
        }
    }

    fn paint_alpha(&self, surface: &mut Surface, layout: &Layout, (scale, theme): (Scale, &Theme)) {
        let Some((x, y, w, h)) = surface_rect(layout.inner(Part::Alpha)) else {
            return;
        };
        let across = self.view == PickerView::Sliders;
        let side = if across { h } else { w };
        Checker::new(theme, scale)
            .with_side(side.div_ceil(2))
            .paint(surface, x, y, w, h);
        let rgb = self.hsv.to_rgb();
        if across {
            for column in 0..w {
                let alpha = along(column, w).byte();
                surface.fill_round_rect(x + column, y, 1, h, 0, Color::from(rgb.with_alpha(alpha)));
            }
        } else {
            for row in 0..h {
                let alpha = u8::MAX - along(row, h).byte();
                surface.fill_round_rect(x, y + row, w, 1, 0, Color::from(rgb.with_alpha(alpha)));
            }
        }
        let border = layout.measures.border;
        ring(
            surface,
            layout.alpha,
            border,
            Color::from(theme.palette().rim),
        );
        if across {
            let caption = role_font(theme, scale, TextRole::Caption);
            let ink = foreground(theme, self.state.disposition());
            paint_caption(surface, caption, "A", layout.alpha_label, ink);
            let column = position(Fraction::from_byte(self.alpha), w);
            paint_column_marker(surface, layout.alpha, (x, y, h), column, border);
        } else {
            let row = position(Fraction::from_byte(u8::MAX - self.alpha), h);
            paint_strip_marker(surface, layout.alpha, (x, y, w), row, border);
        }
        self.veil(surface, layout.alpha, theme);
    }

    fn paint_swatch(
        &self,
        surface: &mut Surface,
        layout: &Layout,
        (scale, theme): (Scale, &Theme),
    ) {
        let earlier = self.earlier.unwrap_or(Rgba::TRANSPARENT);
        for (rect, colour) in [(layout.earlier, earlier), (layout.now, self.colour())] {
            let Some((x, y, w, h)) = surface_rect(rect).filter(|_| !rect.is_empty()) else {
                continue;
            };
            if !colour.is_opaque() {
                Checker::new(theme, scale)
                    .with_side(h.div_ceil(2))
                    .paint(surface, x, y, w, h);
            }
            surface.fill_round_rect(x, y, w, h, 0, Color::from(colour));
            ring(
                surface,
                rect,
                layout.measures.border,
                Color::from(theme.palette().rim),
            );
            self.veil(surface, rect, theme);
        }
        // A value past sRGB shows the nearest colour in it, and says so.
        if self.clipped {
            if let Some((x, y, w, _)) = surface_rect(layout.now) {
                let size = scale.scale_length(theme.metrics().bead_size).max(3).min(w);
                let warning = Color::from(theme.palette().warning);
                paint_bead(surface, x + w - size, y, size, warning, BeadShape::Diamond);
            }
        }
    }

    /// Half veil a part of a disabled picker, so it still shows what it holds.
    fn veil(&self, surface: &mut Surface, rect: Rect, theme: &Theme) {
        if self.state.disposition() != ControlDisposition::DisabledByState {
            return;
        }
        if let Some((x, y, w, h)) = surface_rect(rect) {
            let veil = Color::from(theme.palette().surface.with_alpha(128));
            surface.fill_round_rect(x, y, w, h, 0, veil);
        }
    }
}

/// The number fields for `model`: its channels' bounds and steps, the rest
/// unused, and the alpha.
fn fields_of(model: ColourModel) -> [NumberField; FIELDS] {
    let channels = model.channels();
    core::array::from_fn(|index| {
        if index == ALPHA {
            return NumberField::new(255, 0, 255).with_steps(1, 16);
        }
        channels.get(index).map_or_else(
            || NumberField::new(0, 0, 0),
            |channel| {
                NumberField::new(channel.least, channel.least, channel.most)
                    .with_steps(1, channel.page)
                    .with_decimals(channel.places)
            },
        )
    })
}

/// Whether a press on `part` drags it: the view's parts and the strips.
const fn is_dragged(part: Part) -> bool {
    matches!(
        part,
        Part::Plane | Part::Hue | Part::Ring | Part::Triangle | Part::Track(_) | Part::Alpha
    )
}

/// Whether the keyboard resting on `part` is shown by a ring about it, as a
/// field's own plate shows it otherwise.
const fn is_ringed(part: Part) -> bool {
    matches!(
        part,
        Part::Plane
            | Part::Hue
            | Part::Ring
            | Part::Triangle
            | Part::Track(_)
            | Part::Alpha
            | Part::Earlier
    )
}

/// `over` laid on the opaque `ground` at `cover`, as a pixel.
fn mixed(ground: Rgba, over: Rgba, cover: f64) -> Pixel {
    let permille = u16::try_from(mathf::round_i32(cover * 1000.0).clamp(0, 1000)).unwrap_or(1000);
    Color::from(ground.mix(over, permille)).premultiply()
}

/// The disc marking where a colour stands on a plane, a ring or a triangle:
/// dark, light and `colour` itself, so it reads over any colour.
fn paint_marker(
    surface: &mut Surface,
    within: Rect,
    (cx, cy): (i32, i32),
    m: Measures,
    colour: tairix_colour::Rgb,
) {
    let Some((px, py, pw, ph)) = surface_rect(within) else {
        return;
    };
    let border = m.border;
    surface.with_clip(px, py, pw, ph, |surface| {
        let disc = |surface: &mut Surface, radius: u32, ink: Color| {
            let left = u32::try_from(cx - to_i32(radius)).unwrap_or(0);
            let top = u32::try_from(cy - to_i32(radius)).unwrap_or(0);
            paint_filled_circle(surface, left, top, radius * 2 + 1, ink);
        };
        let radius = m.marker;
        disc(surface, radius, Color::rgba(0, 0, 0, 200));
        disc(
            surface,
            radius.saturating_sub(border),
            Color::rgb(255, 255, 255),
        );
        disc(
            surface,
            radius.saturating_sub(border * 2),
            Color::from(colour),
        );
    });
}

/// A slider's marker: a light bar edged dark down it at `column`.
fn paint_column_marker(
    surface: &mut Surface,
    track: Rect,
    (x, y, h): (u32, u32, u32),
    column: u32,
    border: u32,
) {
    let Some((sx, sy, sw, sh)) = surface_rect(track) else {
        return;
    };
    let centre = x + column;
    surface.with_clip(sx, sy, sw, sh, |surface| {
        let dark = border * 2;
        surface.fill_rect(
            centre.saturating_sub(dark),
            y,
            dark * 2 + 1,
            h,
            Color::rgb(0, 0, 0),
        );
        surface.fill_rect(
            centre.saturating_sub(border),
            y,
            border * 2 + 1,
            h,
            Color::rgb(255, 255, 255),
        );
    });
}

/// `text` read as a colour the way a person types one: spaces round it, the
/// `#` optional, any of CSS's four digit forms — those carrying an alpha
/// only where the picker edits opacity.
fn read_hex(text: &str, opacity: bool) -> Option<Rgba> {
    let digits = text.trim();
    let digits = digits.strip_prefix('#').unwrap_or(digits);
    let (colour, form) = parse_hex(digits)?;
    (opacity || !form.has_alpha()).then_some(colour)
}

/// The plane stepped by `key`: saturation across, value up and down, ten
/// percent a step with `far`.
fn plane_step(hsv: Hsv, key: NamedKey, far: bool) -> Option<Hsv> {
    let line = if far { 10 } else { 1 };
    let by = |fraction: Fraction, by: i64| {
        let stepped = (i64::from(fraction.percent()) + by).clamp(0, 100);
        Fraction::from_percent(u32::try_from(stepped).unwrap_or(0))
    };
    Some(match key {
        NamedKey::Left => Hsv {
            saturation: by(hsv.saturation, -line),
            ..hsv
        },
        NamedKey::Right => Hsv {
            saturation: by(hsv.saturation, line),
            ..hsv
        },
        NamedKey::Up => Hsv {
            value: by(hsv.value, line),
            ..hsv
        },
        NamedKey::Down => Hsv {
            value: by(hsv.value, -line),
            ..hsv
        },
        NamedKey::PageUp => Hsv {
            value: by(hsv.value, 10),
            ..hsv
        },
        NamedKey::PageDown => Hsv {
            value: by(hsv.value, -10),
            ..hsv
        },
        NamedKey::Home => Hsv {
            saturation: Fraction::NONE,
            ..hsv
        },
        NamedKey::End => Hsv {
            saturation: Fraction::ALL,
            ..hsv
        },
        _ => return None,
    })
}

/// The hue stepped by `key`, a degree a step, ten with `far`, round the
/// circle; Home and End are the strip's top and bottom.
fn hue_step(hue: Hue, key: NamedKey, far: bool) -> Option<Hue> {
    let degrees = i64::from(hue.degrees());
    let line = if far { 10 } else { 1 };
    let turned = match key {
        NamedKey::Up | NamedKey::Left => degrees - line,
        NamedKey::Down | NamedKey::Right => degrees + line,
        NamedKey::PageUp => degrees - 30,
        NamedKey::PageDown => degrees + 30,
        NamedKey::Home => 0,
        NamedKey::End => 359,
        _ => return None,
    };
    Some(Hue::from_degrees(
        u32::try_from(turned.rem_euclid(360)).unwrap_or(0),
    ))
}

/// The opacity stepped by `key`: one a step, sixteen with `far`; Home is
/// opaque, at the strip's top, and End clear.
fn alpha_step(alpha: u8, key: NamedKey, far: bool) -> Option<u8> {
    let alpha = i32::from(alpha);
    let line = if far { 16 } else { 1 };
    let moved = match key {
        NamedKey::Up | NamedKey::Right => alpha + line,
        NamedKey::Down | NamedKey::Left => alpha - line,
        NamedKey::PageUp => alpha + 16,
        NamedKey::PageDown => alpha - 16,
        NamedKey::Home => 255,
        NamedKey::End => 0,
        _ => return None,
    };
    u8::try_from(moved.clamp(0, 255)).ok()
}

/// The hue `down` of the way down the strip: red at both ends.
fn hue_at(down: Fraction) -> Hue {
    let steps = u64::from(down.raw()) * u64::from(Hue::TURN) / u64::from(u16::MAX);
    Hue::from_steps(u32::try_from(steps).unwrap_or(0))
}

/// How far down the strip `hue` lies: the inverse of [`hue_at`].
fn hue_fraction(hue: Hue) -> Fraction {
    let raw = u64::from(hue.steps()) * u64::from(u16::MAX) / u64::from(Hue::TURN);
    Fraction::from_raw(u16::try_from(raw).unwrap_or(u16::MAX))
}

/// Pixel `offset` of `extent`, the first none and the last all.
fn along(offset: u32, extent: u32) -> Fraction {
    let last = u64::from(extent.saturating_sub(1).max(1));
    let raw = u64::from(offset.min(extent.saturating_sub(1))) * u64::from(u16::MAX) / last;
    Fraction::from_raw(u16::try_from(raw).unwrap_or(u16::MAX))
}

/// The pixel of `extent` that `fraction` lands on: the inverse of [`along`].
fn position(fraction: Fraction, extent: u32) -> u32 {
    let last = u64::from(extent.saturating_sub(1));
    let all = u64::from(u16::MAX);
    u32::try_from((u64::from(fraction.raw()) * last + all / 2) / all).unwrap_or(0)
}

/// How far into `extent` pixels from `start` the coordinate `at` lies, held
/// to the extent.
fn offset(at: i32, start: i32, extent: u32) -> u32 {
    let into = i64::from(at) - i64::from(start);
    u32::try_from(into.clamp(0, i64::from(extent.saturating_sub(1)))).unwrap_or(0)
}

/// Where the plane marker for `hsv` centres, within the plane's drawing area.
fn marker_centre(area: Rect, hsv: Hsv) -> (i32, i32) {
    let x = area.left() + to_i32(position(hsv.saturation, area.width));
    let down = area
        .height
        .saturating_sub(1)
        .saturating_sub(position(hsv.value, area.height));
    (x, area.top() + to_i32(down))
}

/// A strip's marker: a light bar edged dark across it at `row`, so it reads
/// over any colour.
fn paint_strip_marker(
    surface: &mut Surface,
    strip: Rect,
    (x, y, w): (u32, u32, u32),
    row: u32,
    border: u32,
) {
    let Some((sx, sy, sw, sh)) = surface_rect(strip) else {
        return;
    };
    let centre = y + row;
    surface.with_clip(sx, sy, sw, sh, |surface| {
        let dark = border * 2;
        surface.fill_rect(
            x,
            centre.saturating_sub(dark),
            w,
            dark * 2 + 1,
            Color::rgb(0, 0, 0),
        );
        let light = Color::rgb(255, 255, 255);
        surface.fill_rect(x, centre.saturating_sub(border), w, border * 2 + 1, light);
    });
}

/// A square ring of `thickness` just inside `rect`.
fn ring(surface: &mut Surface, rect: Rect, thickness: u32, colour: Color) {
    if rect.is_empty() {
        return;
    }
    if let Some((x, y, w, h)) = surface_rect(rect) {
        surface.wash_ring(
            x,
            y,
            w,
            h,
            Ring::uniform(0, thickness),
            RingInk::Solid(colour),
        );
    }
}

/// One line of caption text, centred down `rect`.
fn paint_caption(surface: &mut Surface, font: BitmapFont, text: &str, rect: Rect, ink: Color) {
    if text.is_empty() || rect.is_empty() {
        return;
    }
    let run = font.elide_to_width(text, rect.width);
    let top = rect.top() + to_i32(rect.height.saturating_sub(font.line_height()) / 2);
    paint_run(surface, font, run, (rect.left(), top), ink, None);
}

#[cfg(test)]
#[path = "colour_picker_tests.rs"]
mod tests;
