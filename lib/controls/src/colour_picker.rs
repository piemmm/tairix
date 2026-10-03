//! The colour picker: one colour edited by its hue, saturation and value, its
//! red, green and blue, its hexadecimal notation and, where it has one, its
//! opacity.
//!
//! The parts, each its own stop for the keyboard:
//!
//! - the **plane**: saturation across, value up, at the colour's hue;
//! - the **hue strip** beside it, red at both ends;
//! - the **opacity strip**, over a checker, where the colour has an alpha;
//! - the **swatch**: the colour, and beside it the earlier colour an owner
//!   names, which a press takes back;
//! - the **hex field**: `#rrggbb`, or `#rrggbbaa` while translucent;
//! - **number fields** for H (°), S and V (%), and R, G, B and A
//!   (`0..=255`).
//!
//! The picker holds the colour as hue, saturation and value, so a colour
//! dragged to grey or to black keeps the hue and saturation it showed. Wide
//! bounds put the fields beside the plane and narrow ones beneath it; bounds
//! too short for everything give up the number fields first, then the hex
//! row, and never the plane.
//!
//! Like a slider, it shows a change at once, reports
//! [`PickerOutcome::Edited`] for each live one and [`PickerOutcome::Settled`]
//! once an interaction ends; durable work belongs on the settle.

use core::cell::Cell;

use tairix_colour::{parse_hex, Fraction, Hsv, Hue, Rgb, Rgba};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_inline::ArrayString;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Ring, RingInk, Surface};
use tairix_theme::{TextRole, Theme};

use crate::checker::Checker;
use crate::number::{NumberAction, NumberField};
use crate::paint::{
    foreground, paint_bead, paint_filled_circle, paint_run, plate_border, resolve_bead, role_font,
    surface_rect, withheld,
};
use crate::state::{ControlDisposition, ControlState, RenderInvariant, ValidationState};
use crate::text::{TextAction, TextField};

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

/// One coordinate a number field edits, in the order Tab walks them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Component {
    Hue,
    Saturation,
    Value,
    Red,
    Green,
    Blue,
    Alpha,
}

const COMPONENTS: [Component; 7] = [
    Component::Hue,
    Component::Saturation,
    Component::Value,
    Component::Red,
    Component::Green,
    Component::Blue,
    Component::Alpha,
];

impl Component {
    const fn index(self) -> usize {
        self as usize
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Hue => "H",
            Self::Saturation => "S",
            Self::Value => "V",
            Self::Red => "R",
            Self::Green => "G",
            Self::Blue => "B",
            Self::Alpha => "A",
        }
    }

    const fn unit(self) -> &'static str {
        match self {
            Self::Hue => "°",
            Self::Saturation | Self::Value => "%",
            _ => "",
        }
    }

    /// Where the field sits in the grid: H, S and V down the first column,
    /// R, G, B and A down the second.
    const fn cell(self) -> (u32, u32) {
        match self {
            Self::Hue => (0, 0),
            Self::Saturation => (0, 1),
            Self::Value => (0, 2),
            Self::Red => (1, 0),
            Self::Green => (1, 1),
            Self::Blue => (1, 2),
            Self::Alpha => (1, 3),
        }
    }

    fn field(self) -> NumberField {
        match self {
            Self::Hue => NumberField::new(0, 0, 359).with_steps(1, 15),
            Self::Saturation | Self::Value => NumberField::new(0, 0, 100).with_steps(1, 10),
            _ => NumberField::new(0, 0, 255).with_steps(1, 16),
        }
    }
}

/// A part the keyboard can rest on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Part {
    Plane,
    Hue,
    Alpha,
    Earlier,
    Hex,
    Number(Component),
}

/// The parts, in the order Tab walks them.
const PARTS: [Part; 12] = [
    Part::Plane,
    Part::Hue,
    Part::Alpha,
    Part::Earlier,
    Part::Hex,
    Part::Number(Component::Hue),
    Part::Number(Component::Saturation),
    Part::Number(Component::Value),
    Part::Number(Component::Red),
    Part::Number(Component::Green),
    Part::Number(Component::Blue),
    Part::Number(Component::Alpha),
];

/// The readouts a change of colour rewrote, for the damage they owe.
#[derive(Copy, Clone, Debug, Default)]
struct Rewritten {
    numbers: [bool; 7],
    hex: bool,
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

/// The picker's resolved geometry: one layout serves drawing, hit-testing and
/// the damage each change reports. An absent part is [`Rect::EMPTY`].
#[derive(Clone, Debug)]
struct Layout {
    measures: Measures,
    plane: Rect,
    hue: Rect,
    alpha: Rect,
    earlier: Rect,
    now: Rect,
    hex: Rect,
    numbers: [Rect; 7],
    labels: [Rect; 7],
    units: [Rect; 7],
}

impl Layout {
    fn rect_of(&self, part: Part) -> Rect {
        match part {
            Part::Plane => self.plane,
            Part::Hue => self.hue,
            Part::Alpha => self.alpha,
            Part::Earlier => self.earlier,
            Part::Hex => self.hex,
            Part::Number(component) => self.numbers[component.index()],
        }
    }

    fn part_at(&self, point: Point) -> Option<Part> {
        PARTS
            .into_iter()
            .find(|&part| self.rect_of(part).contains(point))
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
    state: ControlState,
    part: Part,
    hex: TextField,
    numbers: [NumberField; 7],
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
    /// A picker showing `colour` opaque, with no earlier colour beside it.
    #[must_use]
    pub fn new(colour: Rgba) -> Self {
        let mut picker = Self {
            hsv: Hsv::default(),
            alpha: u8::MAX,
            opacity: false,
            earlier: None,
            state: ControlState::idle(),
            part: Part::Plane,
            hex: TextField::new().with_max_len(HEX_LEN),
            numbers: COMPONENTS.map(Component::field),
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
        *self.settled = self.colour();
        *self.live = false;
        self.show_readouts(None);
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
            if matches!(self.part, Part::Alpha | Part::Number(Component::Alpha)) {
                self.part = Part::Plane;
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
            self.part = Part::Plane;
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
        let mut shown = PARTS
            .into_iter()
            .filter(|&part| !layout.rect_of(part).is_empty());
        let part = if forward {
            shown.next()
        } else {
            shown.next_back()
        };
        self.part = part.unwrap_or(Part::Plane);
        self.set_focused(true);
    }

    /// Whether a press on the plane or a strip is being dragged.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        matches!(*self.drag, Some(Part::Plane | Part::Hue | Part::Alpha))
    }

    /// The least width the picker lays out in: the plane and its strips, or
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
        [layout.plane, layout.now]
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
        self.paint_plane(surface, &layout, theme);
        self.paint_hue(surface, &layout, theme);
        self.paint_alpha(surface, &layout, (scale, theme));
        self.paint_swatch(surface, &layout, (scale, theme));
        if !layout.hex.is_empty() {
            self.hex.render(surface, layout.hex, scale, theme);
        }
        let caption = role_font(theme, scale, TextRole::Caption);
        let ink = foreground(theme, self.state.disposition());
        for component in COMPONENTS {
            let index = component.index();
            if !layout.numbers[index].is_empty() {
                self.numbers[index].render(surface, layout.numbers[index], scale, theme);
                paint_caption(
                    surface,
                    caption,
                    component.label(),
                    layout.labels[index],
                    ink,
                );
                paint_caption(surface, caption, component.unit(), layout.units[index], ink);
            }
        }
        let ringed = matches!(
            self.part,
            Part::Plane | Part::Hue | Part::Alpha | Part::Earlier
        );
        if self.state.focus.focused && ringed {
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

    /// Feed a pointer event. A primary press on the plane or a strip starts a
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
                if let Some(part @ (Part::Plane | Part::Hue | Part::Alpha)) = *self.drag {
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
            InputEvent::PointerScrolled { .. } => match layout.part_at(*self.pointer) {
                Some(Part::Number(component)) => {
                    let rect = layout.numbers[component.index()];
                    let field = &mut self.numbers[component.index()];
                    let stepped = field.on_pointer(event, rect, scale, theme, damage);
                    self.number_outcome(component, stepped, &layout, damage)
                }
                _ => PickerOutcome::Ignored,
            },
            _ => PickerOutcome::Ignored,
        }
    }

    /// Feed a key to a focused picker. Tab and Shift+Tab walk the parts,
    /// committing a field they leave, and answer [`PickerOutcome::Ignored`]
    /// past either end so the owner carries the focus on. On the plane the
    /// arrows step saturation and value, on a strip they step it, Shift
    /// steps ten times as far, and Home and End go to an end. Enter or Space
    /// on the earlier colour takes it back. A field takes every key but Tab
    /// and a chord it has no use for, which are the owner's — Ctrl+A selects
    /// the field's text, Ctrl+S still saves. Escape abandons a drag, or takes
    /// back what a field was typed since the last settle, and is otherwise
    /// the owner's.
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
        if layout.rect_of(self.part).is_empty() {
            self.part = Part::Plane;
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
            Part::Plane | Part::Hue | Part::Alpha => self.step(key, modifiers, &layout, damage),
            Part::Earlier => match key {
                Key::Named(NamedKey::Enter) | Key::Char(' ') => {
                    self.take_back_earlier(&layout, damage)
                }
                _ => PickerOutcome::Ignored,
            },
            Part::Hex | Part::Number(_) if owner_chord(key, modifiers) => PickerOutcome::Ignored,
            Part::Hex => self.hex_key(key, modifiers, &layout, damage),
            Part::Number(component) => {
                let rect = layout.numbers[component.index()];
                let action = self.numbers[component.index()].on_key(key, modifiers, rect, damage);
                if action.is_none() && key == Key::Named(NamedKey::Escape) {
                    return PickerOutcome::Ignored;
                }
                self.number_outcome(component, action, &layout, damage)
            }
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

    fn press(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        layout: &Layout,
        style: (Scale, &Theme),
        damage: &mut Region,
    ) -> PickerOutcome {
        let Some(part) = layout.part_at(*self.pointer) else {
            return if bounds.contains(*self.pointer) {
                PickerOutcome::Taken
            } else {
                PickerOutcome::Ignored
            };
        };
        let left = self.move_to(part, layout, damage);
        *self.drag = Some(part);
        let pressed = match part {
            Part::Plane | Part::Hue | Part::Alpha => self.drag_to(part, layout, damage),
            Part::Hex | Part::Number(_) => {
                self.feed_field(part, event, layout, style, damage);
                PickerOutcome::Taken
            }
            Part::Earlier => PickerOutcome::Taken,
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
            Some(Part::Plane | Part::Hue | Part::Alpha) => self.settle(),
            Some(Part::Earlier) if layout.part_at(*self.pointer) == Some(Part::Earlier) => {
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
            Part::Number(component) => {
                let rect = layout.numbers[component.index()];
                self.numbers[component.index()].on_pointer(event, rect, scale, theme, damage);
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
        for component in COMPONENTS {
            self.feed_field(Part::Number(component), event, layout, style, damage);
        }
    }

    /// Abandon a drag, returning the colour to where it began.
    fn abandon_drag(&mut self, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        *self.drag = None;
        let settled = *self.settled;
        let hsv = Hsv::from_rgb(settled.without_alpha(), self.hsv);
        self.set_coordinates(hsv, settled.a, None, layout, damage);
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
        let at = PARTS
            .iter()
            .position(|&part| part == self.part)
            .unwrap_or(0);
        let shown = |part: &&Part| !layout.rect_of(**part).is_empty();
        let next = if forward {
            PARTS.iter().skip(at + 1).find(shown)
        } else {
            PARTS.iter().take(at).rev().find(shown)
        };
        match next {
            Some(&part) => self.move_to(part, layout, damage),
            None => PickerOutcome::Ignored,
        }
    }

    /// A key on the plane or a strip: one step, settled.
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
        let stepped = match self.part {
            Part::Plane => {
                plane_step(self.hsv, named, modifiers.shift).map(|hsv| (hsv, self.alpha))
            }
            Part::Hue => hue_step(self.hsv.hue, named, modifiers.shift)
                .map(|hue| (Hsv { hue, ..self.hsv }, self.alpha)),
            Part::Alpha => {
                alpha_step(self.alpha, named, modifiers.shift).map(|alpha| (self.hsv, alpha))
            }
            _ => None,
        };
        let Some((hsv, alpha)) = stepped else {
            return PickerOutcome::Ignored;
        };
        if self.set_coordinates(hsv, alpha, None, layout, damage) {
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
                if self.set_coordinates(hsv, colour.a, Some(Part::Hex), layout, damage) {
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
                self.set_coordinates(hsv, settled.a, None, layout, damage);
                if self.show_hex() {
                    damage.add(layout.hex);
                }
                self.settle()
            }
            None => PickerOutcome::Taken,
        }
    }

    /// What a number field's action means for the colour.
    fn number_outcome(
        &mut self,
        component: Component,
        action: Option<NumberAction>,
        layout: &Layout,
        damage: &mut Region,
    ) -> PickerOutcome {
        match action {
            Some(NumberAction::Edited { value }) => {
                if self.apply_component(component, value, layout, damage) {
                    *self.live = true;
                    PickerOutcome::Edited(self.colour())
                } else {
                    PickerOutcome::Taken
                }
            }
            Some(NumberAction::Settled { value }) => {
                self.apply_component(component, value, layout, damage);
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
            Part::Number(component) => {
                let rect = layout.numbers[component.index()];
                if let Some(NumberAction::Settled { value }) =
                    self.numbers[component.index()].commit(rect, damage)
                {
                    self.apply_component(component, value, layout, damage);
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
        self.set_coordinates(hsv, earlier.a, None, layout, damage);
        self.settle()
    }

    /// The drag on `part` carried to the pointer.
    fn drag_to(&mut self, part: Part, layout: &Layout, damage: &mut Region) -> PickerOutcome {
        let area = layout.inner(part);
        let pointer = *self.pointer;
        let across = along(offset(pointer.x, area.left(), area.width), area.width);
        let down = along(offset(pointer.y, area.top(), area.height), area.height);
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
            _ => (self.hsv, u8::MAX - down.byte()),
        };
        if self.set_coordinates(hsv, alpha, None, layout, damage) {
            *self.live = true;
            PickerOutcome::Edited(self.colour())
        } else {
            PickerOutcome::Taken
        }
    }

    /// Set the coordinate `component` edits to `value`, answering whether
    /// the colour moved.
    fn apply_component(
        &mut self,
        component: Component,
        value: i32,
        layout: &Layout,
        damage: &mut Region,
    ) -> bool {
        let whole = u32::try_from(value).unwrap_or(0);
        let byte = u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX);
        let hsv = self.hsv;
        let rgb = hsv.to_rgb();
        let (hsv, alpha) = match component {
            Component::Hue => (
                Hsv {
                    hue: Hue::from_degrees(whole),
                    ..hsv
                },
                self.alpha,
            ),
            Component::Saturation => (
                Hsv {
                    saturation: Fraction::from_percent(whole),
                    ..hsv
                },
                self.alpha,
            ),
            Component::Value => (
                Hsv {
                    value: Fraction::from_percent(whole),
                    ..hsv
                },
                self.alpha,
            ),
            Component::Red => (Hsv::from_rgb(Rgb { r: byte, ..rgb }, hsv), self.alpha),
            Component::Green => (Hsv::from_rgb(Rgb { g: byte, ..rgb }, hsv), self.alpha),
            Component::Blue => (Hsv::from_rgb(Rgb { b: byte, ..rgb }, hsv), self.alpha),
            Component::Alpha => (hsv, byte),
        };
        self.set_coordinates(hsv, alpha, Some(Part::Number(component)), layout, damage)
    }

    /// Move the colour to `hsv` at `alpha`, reporting each part whose drawing
    /// that changes and showing it in every readout but the one being typed
    /// in; answer whether it moved.
    fn set_coordinates(
        &mut self,
        hsv: Hsv,
        alpha: u8,
        typing: Option<Part>,
        layout: &Layout,
        damage: &mut Region,
    ) -> bool {
        let alpha = if self.opacity { alpha } else { u8::MAX };
        if (hsv, alpha) == (self.hsv, self.alpha) {
            return false;
        }
        if hsv.hue != self.hsv.hue {
            damage.add(layout.plane);
            damage.add(layout.hue);
        } else if (hsv.saturation, hsv.value) != (self.hsv.saturation, self.hsv.value) {
            damage.add(layout.marker(self.hsv));
            damage.add(layout.marker(hsv));
        }
        damage.add(layout.alpha);
        damage.add(layout.now);
        self.hsv = hsv;
        self.alpha = alpha;
        layout.report(self.show_readouts(typing), damage);
        true
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
        let rgb = self.hsv.to_rgb();
        let values = [
            self.hsv.hue.degrees(),
            self.hsv.saturation.percent(),
            self.hsv.value.percent(),
            u32::from(rgb.r),
            u32::from(rgb.g),
            u32::from(rgb.b),
            u32::from(self.alpha),
        ];
        let mut rewritten = Rewritten::default();
        for component in COMPONENTS {
            let index = component.index();
            let value = i32::try_from(values[index]).unwrap_or(0);
            if typing != Some(Part::Number(component)) && self.numbers[index].value() != value {
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
        for component in COMPONENTS {
            let field = &mut self.numbers[component.index()];
            field.set_state(share(field.state(), part == Part::Number(component)));
        }
    }

    fn strips_width(&self, measures: Measures) -> u32 {
        let strips = if self.opacity { 2 } else { 1 };
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
        let widest = |text: fn(Component) -> &'static str| {
            COMPONENTS
                .into_iter()
                .map(|component| caption.text_width(text(component)))
                .max()
                .unwrap_or(0)
        };
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
            label: widest(Component::label),
            field: self
                .numbers
                .iter()
                .map(|field| field.preferred_width(scale, theme))
                .max()
                .unwrap_or(0),
            unit: widest(Component::unit),
            hex: body
                .text_width("#00000000")
                .saturating_add(edge * 2)
                .saturating_add(scale.scale_length(2)),
        };
        self.measured.0.set(Some((key, measures)));
        measures
    }

    /// The picker laid out in `bounds`.
    fn layout(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Layout {
        let m = self.measures(scale, theme);
        let rows = if self.opacity { 4 } else { 3 };
        let (block_w, block_h) = (m.block_width(), m.block_height(rows));
        let strips = self.strips_width(m);
        let room = bounds.height;
        let beside = bounds.width >= m.plane_min + strips + m.gap + block_w;
        let (plane_w, plane_h, block, shown) = if beside {
            let plane_w = bounds.width - block_w - m.gap - strips;
            let block_x = bounds.left() + to_i32(bounds.width - block_w);
            let block = Rect::new(block_x, bounds.top(), block_w, block_h.min(room));
            let shown = (room >= m.row, room >= block_h);
            (plane_w, block_h.max(m.plane_min).min(room), block, shown)
        } else {
            let plane_w = bounds.width.saturating_sub(strips);
            let natural = (plane_w * 3 / 4).clamp(m.plane_min, m.plane_max);
            let (full, hex_only) = (m.gap + block_h, m.gap + m.row);
            let (plane_h, shown) = if room >= natural + full {
                (natural, (true, true))
            } else if room >= m.plane_min + full {
                (room - full, (true, true))
            } else if room >= m.plane_min + hex_only {
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
        let plane = Rect::new(bounds.left(), bounds.top(), plane_w, plane_h);
        let strip = |n: u32| {
            let x = plane.right() + to_i32(m.gap + n * (m.strip + m.gap));
            Rect::new(x, bounds.top(), m.strip, plane_h)
        };
        let mut layout = Layout {
            measures: m,
            plane,
            hue: strip(0),
            alpha: if self.opacity { strip(1) } else { Rect::EMPTY },
            earlier: Rect::EMPTY,
            now: Rect::EMPTY,
            hex: Rect::EMPTY,
            numbers: [Rect::EMPTY; 7],
            labels: [Rect::EMPTY; 7],
            units: [Rect::EMPTY; 7],
        };
        let (show_hex, show_grid) = shown;
        if show_hex {
            self.lay_hex_row(&mut layout, block);
        }
        if show_grid {
            self.lay_grid(&mut layout, block);
        }
        layout
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

    /// The number fields in their grid under the hex row of `block`.
    fn lay_grid(&self, layout: &mut Layout, block: Rect) {
        let m = layout.measures;
        let top = block.top() + to_i32(m.row + m.gap);
        for component in COMPONENTS {
            if component == Component::Alpha && !self.opacity {
                continue;
            }
            let index = component.index();
            let (column, row) = component.cell();
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
        let (cx, cy) = marker_centre(area, self.hsv);
        if let Some((px, py, pw, ph)) = surface_rect(layout.plane) {
            surface.with_clip(px, py, pw, ph, |surface| {
                let disc = |surface: &mut Surface, radius: u32, colour: Color| {
                    let left = u32::try_from(cx - to_i32(radius)).unwrap_or(0);
                    let top = u32::try_from(cy - to_i32(radius)).unwrap_or(0);
                    paint_filled_circle(surface, left, top, radius * 2 + 1, colour);
                };
                let radius = layout.measures.marker;
                disc(surface, radius, Color::rgba(0, 0, 0, 200));
                disc(
                    surface,
                    radius.saturating_sub(border),
                    Color::rgb(255, 255, 255),
                );
                disc(
                    surface,
                    radius.saturating_sub(border * 2),
                    Color::from(self.hsv.to_rgb()),
                );
            });
        }
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

    fn paint_alpha(&self, surface: &mut Surface, layout: &Layout, (scale, theme): (Scale, &Theme)) {
        let Some((x, y, w, h)) = surface_rect(layout.inner(Part::Alpha)) else {
            return;
        };
        Checker::new(theme, scale)
            .with_side(w.div_ceil(2))
            .paint(surface, x, y, w, h);
        let rgb = self.hsv.to_rgb();
        for row in 0..h {
            let alpha = u8::MAX - along(row, h).byte();
            surface.fill_round_rect(x, y + row, w, 1, 0, Color::from(rgb.with_alpha(alpha)));
        }
        let border = layout.measures.border;
        ring(
            surface,
            layout.alpha,
            border,
            Color::from(theme.palette().rim),
        );
        let row = position(Fraction::from_byte(u8::MAX - self.alpha), h);
        paint_strip_marker(surface, layout.alpha, (x, y, w), row, border);
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

/// Whether `key` with `modifiers` is a chord a field has no use for — any
/// but Ctrl+A, which selects its text — so the owner's shortcut takes it.
fn owner_chord(key: Key, modifiers: Modifiers) -> bool {
    let chord = modifiers.ctrl || modifiers.alt || modifiers.meta;
    let select_all =
        modifiers.ctrl && !modifiers.alt && !modifiers.meta && matches!(key, Key::Char('a' | 'A'));
    chord && !select_all
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
