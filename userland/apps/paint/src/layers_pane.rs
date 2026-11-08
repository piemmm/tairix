//! The Layers pane (`plans/PAINT.md`): the picture's layers top first, each
//! row its eye, its thumbnail, its name and its opacity, the row of the layer
//! painted on marked; above them that layer's opacity, typed or dragged, and
//! New, Duplicate, Delete and Merge down.
//!
//! [`place`] lays the pane out for its paint, its hit tests and the keyboard
//! alike. The rows scroll by a bar of their own where they overflow the pane,
//! the controls above them staying put. A press on a row works on its layer
//! and on its eye shows or hides the layer; dragged, the row moves its layer
//! through the stack, and let go on the name of the layer painted on it
//! renames that layer in place.

use alloc::vec;
use core::fmt::Write as _;
use core::ops::Range;

use tairix_colour::Fraction;
use tairix_controls::{
    fill_area, owner_chord, paint_focus_ring, paint_icon_slot, paint_run, ArtworkTone, Button,
    Checker, Keystroke, ScrollBar, ScrollModel, ScrollOrientation, ScrollRange, TextAction,
    TextField,
};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind, IconPicture, IconRequest};
use tairix_inline::ArrayString;
use tairix_input::{past_drag_slop, InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_sandbox::imageedit::MAX_LAYER_NAME;
use tairix_theme::Theme;

use crate::document::{Layer, MOST_LAYERS};
use crate::layout::{BodyHeights, Faces, Fit};
use crate::op::Parameter;
use crate::panel::{Panel, PanelEvent, PanelOutcome, Part, Plain};

/// A row's thumbnail side, in logical pixels.
const THUMB: u32 = 28;

/// The eye's side, in logical pixels.
const EYE: u32 = 16;

/// The room about a row's parts, in logical pixels.
const PAD: u32 = 3;

/// The rows the pane shows at the least, where there are as many.
const LEAST_ROWS: usize = 2;

/// The opacity of the layer painted on, in percent.
const OPACITY: Parameter = Parameter {
    label: "Opacity",
    least: 0,
    most: 100,
};

/// The panel's parts: the opacity, the buttons that add a layer, and those
/// that take one away.
const OPACITY_PART: usize = 0;
const ADD_PART: usize = 1;
const REMOVE_PART: usize = 2;

/// The widest a row's opacity reads.
const WIDEST_OPACITY: &str = "100%";

/// What an input to the Layers pane came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LayersOutcome {
    /// None of the pane's.
    Ignored,
    /// The pane's, asking nothing of the window: a hover, a scroll, typing.
    Taken,
    /// Paint on this layer.
    Select(usize),
    /// Show this layer, or hide it.
    Show(usize),
    /// Call the layer at this revision what the name being typed holds
    /// ([`LayersPane::typed_name`]): the layer the rename began on, wherever
    /// it now stands, and none once it has gone.
    Renamed(u64),
    /// Move a layer through the stack.
    Move {
        /// Where it stands.
        from: usize,
        /// Where it goes.
        to: usize,
    },
    /// Show the layer painted on at `percent`.
    Opacity {
        /// How much of it shows, in percent.
        percent: i32,
        /// Whether the interaction is over.
        settled: bool,
    },
    /// Add a clear layer.
    New,
    /// Copy the layer painted on.
    Duplicate,
    /// Take the layer painted on away.
    Delete,
    /// Lay the layer painted on onto the one beneath.
    MergeDown,
    /// The keyboard walked off the pane: past its last stop when `forward`.
    Left {
        /// Which way.
        forward: bool,
    },
}

/// What the pane is laid out from beyond its bounds.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct LayerMeasures {
    /// How tall its controls stand.
    pub panel: u32,
    /// A row's height.
    pub row: u32,
    /// How many rows it holds, one a layer.
    pub count: usize,
}

impl LayerMeasures {
    /// How tall the pane stands, its controls `gap` above the rows: its
    /// controls and two rows at the least, every row at the most. The rows
    /// scroll themselves between.
    #[must_use]
    pub fn heights(self, gap: u32) -> BodyHeights {
        let above = self.panel.saturating_add(gap);
        BodyHeights {
            least: above.saturating_add(rows_span(self.count.min(LEAST_ROWS), self.row)),
            natural: above.saturating_add(rows_span(self.count, self.row)),
            fit: Fit::Own,
        }
    }
}

/// The pane as laid out.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Placed {
    /// The whole pane.
    pub bounds: Rect,
    /// The opacity and the buttons.
    pub panel: Rect,
    /// Where the rows show.
    pub rows: Rect,
    /// The rows' bar, empty while every row shows.
    pub bar: Rect,
    /// A row's height.
    pub row: u32,
    /// How many rows there are, one a layer.
    pub count: usize,
}

impl Placed {
    /// Row `display`, counted from the top, its rows scrolled `offset` up.
    #[must_use]
    pub fn row_rect(&self, display: usize, offset: u32) -> Rect {
        let down = i64::try_from(display)
            .unwrap_or(i64::MAX)
            .saturating_mul(i64::from(self.row));
        let top = i64::from(self.rows.top())
            .saturating_add(down)
            .saturating_sub(i64::from(offset));
        Rect::new(
            self.rows.left(),
            i32::try_from(top).unwrap_or(i32::MAX),
            self.rows.width,
            self.row,
        )
    }

    /// The row under `at`, counted from the top, where the rows show it.
    #[must_use]
    pub fn row_at(&self, at: Point, offset: u32) -> Option<usize> {
        if !self.rows.contains(at) {
            return None;
        }
        let down = u64::try_from(at.y.saturating_sub(self.rows.top()))
            .ok()?
            .saturating_add(u64::from(offset));
        let display = usize::try_from(down / u64::from(self.row.max(1))).ok()?;
        (display < self.count).then_some(display)
    }

    /// The gap between rows nearest `at`, counted from the top: where a row
    /// dragged there lands.
    #[must_use]
    pub fn gap_at(&self, at: Point, offset: u32) -> usize {
        let down = i64::from(at.y) - i64::from(self.rows.top())
            + i64::from(offset)
            + i64::from(self.row / 2);
        usize::try_from(down.max(0) / i64::from(self.row.max(1)))
            .map_or(self.count, |gap| gap.min(self.count))
    }

    /// The rows in view at `offset`, counted from the top.
    #[must_use]
    pub fn shown(&self, offset: u32) -> Range<usize> {
        let row = self.row.max(1);
        let first = usize::try_from(offset / row)
            .unwrap_or(usize::MAX)
            .min(self.count);
        let last = usize::try_from(offset.saturating_add(self.rows.height).div_ceil(row))
            .unwrap_or(usize::MAX)
            .min(self.count);
        first..last.max(first)
    }

    /// The row layer `layer` stands in, counted from the top.
    fn display_of(&self, layer: usize) -> Option<usize> {
        self.count.checked_sub(layer.checked_add(1)?)
    }
}

/// The pane laid out down `bounds` from `measures`, its controls a `gap`
/// above the rows and a bar `breadth` broad beside rows that overflow.
#[must_use]
pub fn place(bounds: Rect, measures: LayerMeasures, (gap, breadth): (u32, u32)) -> Placed {
    let mut rest = bounds;
    let panel = rest.take_top(measures.panel);
    let _ = rest.take_top(gap);
    let overflows = !rest.is_empty() && rows_span(measures.count, measures.row) > rest.height;
    let bar = if overflows {
        rest.take_right(breadth)
    } else {
        Rect::EMPTY
    };
    Placed {
        bounds,
        panel,
        rows: rest,
        bar,
        row: measures.row,
        count: measures.count,
    }
}

/// A row's thumbnail side at `scale`: the side its thumbnail is drawn at.
#[must_use]
pub fn thumb_side(scale: Scale) -> u32 {
    scale.scale_length(THUMB).max(1)
}

/// The pixels `count` rows `row` high span.
fn rows_span(count: usize, row: u32) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX).saturating_mul(row)
}

/// Where the layer holding `revision` stands among `layers`, while one does.
#[must_use]
pub(crate) fn layer_at(layers: &[Layer], revision: u64) -> Option<usize> {
    layers.iter().position(|layer| layer.revision == revision)
}

/// What `opacity` reads as, in percent.
fn percent_of(opacity: u8) -> i32 {
    i32::try_from(Fraction::from_byte(opacity).percent()).unwrap_or(OPACITY.most)
}

/// Where one row's parts are drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RowParts {
    /// The eye that shows or hides the layer.
    pub eye: Rect,
    /// Its thumbnail.
    pub thumb: Rect,
    /// Its name.
    pub name: Rect,
    /// Its opacity.
    pub opacity: Rect,
}

/// `row`'s parts, left to right: the eye, the thumbnail, the name and the
/// opacity.
fn parts(row: Rect, faces: Faces, scale: Scale) -> RowParts {
    let pad = scale.scale_length(PAD);
    let mut rest = row.inset(pad);
    let mut square = |side: u32| {
        let side = side.min(rest.height);
        let column = rest.take_left(side);
        let _ = rest.take_left(pad);
        Rect::new(
            column.left(),
            column
                .top()
                .saturating_add_unsigned((column.height - side) / 2),
            column.width,
            side,
        )
    };
    let eye = square(scale.scale_length(EYE));
    let thumb = square(thumb_side(scale));
    let room = faces
        .caption
        .text_width(WIDEST_OPACITY)
        .saturating_add(pad)
        .min(rest.width);
    let opacity = rest.take_right(room);
    RowParts {
        eye,
        thumb,
        name: rest,
        opacity,
    }
}

/// Where the field a row's name is typed in stands: across the name, as tall
/// as a field, centred on the row.
fn field_rect(row: Rect, name: Rect, scale: Scale, theme: &Theme) -> Rect {
    let height = TextField::height(scale, theme).min(row.height);
    Rect::new(
        name.left(),
        row.top().saturating_add_unsigned((row.height - height) / 2),
        name.width,
        height,
    )
}

/// The mark drawn where a dragged row would land: a bar across the rows at
/// gap `gap`, counted from the top.
fn landing_mark(placed: &Placed, gap: usize, offset: u32, scale: Scale) -> Rect {
    let thickness = scale.scale_length(2).max(2);
    let y = placed
        .row_rect(gap, offset)
        .top()
        .saturating_sub(to_i32(thickness / 2));
    Rect::new(placed.rows.left(), y, placed.rows.width, thickness).intersection(&placed.rows)
}

/// What the panel's outcome comes to for the pane.
fn answered(outcome: PanelOutcome) -> LayersOutcome {
    match outcome {
        PanelOutcome::Ignored => LayersOutcome::Ignored,
        PanelOutcome::Taken | PanelOutcome::Custom { .. } => LayersOutcome::Taken,
        PanelOutcome::Left { forward } => LayersOutcome::Left { forward },
        PanelOutcome::Event(event) => match event {
            PanelEvent::Number {
                part: OPACITY_PART,
                value,
                settled,
                ..
            } => LayersOutcome::Opacity {
                percent: value,
                settled,
            },
            PanelEvent::Pressed {
                part: ADD_PART,
                button,
            } => {
                if button == 0 {
                    LayersOutcome::New
                } else {
                    LayersOutcome::Duplicate
                }
            }
            PanelEvent::Pressed {
                part: REMOVE_PART,
                button,
            } => {
                if button == 0 {
                    LayersOutcome::Delete
                } else {
                    LayersOutcome::MergeDown
                }
            }
            _ => LayersOutcome::Taken,
        },
    }
}

/// Offer part `part`'s two buttons as `enabled` says, answering whether one
/// changed.
fn enable(panel: &mut Panel, part: usize, enabled: [bool; 2]) -> bool {
    let Some(Part::Buttons(buttons)) = panel.part_mut(part) else {
        return false;
    };
    let mut changed = false;
    for (button, enabled) in buttons.iter_mut().zip(enabled) {
        let mut state = button.state();
        if state.enabled != enabled {
            state.enabled = enabled;
            button.set_state(state);
            changed = true;
        }
    }
    changed
}

/// A press held on a row.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Press {
    /// The layer it landed on.
    layer: usize,
    /// Where it landed.
    at: Point,
    /// Whether it landed on the name of the layer painted on, which it
    /// renames let go there.
    renames: bool,
    /// The gap, counted from the top, a drag would land the layer in once
    /// the press is a drag.
    landing: Option<usize>,
}

/// A layer being renamed in place.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Rename {
    /// The revision of the layer renamed, which no other layer holds: what
    /// the name lands on, so it lands on no other.
    revision: u64,
    field: TextField,
    /// Whether the rows had the keyboard, which they take back once the name
    /// is settled.
    from_rows: bool,
}

/// One row as it is drawn.
#[derive(Copy, Clone, Debug)]
struct Row {
    rect: Rect,
    /// Whether its layer is the one painted on.
    working: bool,
    /// Whether the rows have the keyboard on it.
    focused: bool,
}

/// The Layers pane.
#[derive(Clone, Debug)]
pub struct LayersPane {
    panel: Panel,
    bar: ScrollBar,
    pointer: Point,
    press: Option<Press>,
    rename: Option<Rename>,
    /// Whether the rows hold the keyboard.
    rows_focused: bool,
    /// Whether no worker has the picture: no rename begins while one has.
    idle: bool,
}

impl Default for LayersPane {
    fn default() -> Self {
        Self::new()
    }
}

impl LayersPane {
    /// The pane over a layer shown wholly, its rows unscrolled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            panel: Panel::new(vec![
                Part::number(OPACITY, OPACITY.most),
                Part::Buttons(vec![Button::labelled("New"), Button::labelled("Duplicate")]),
                Part::Buttons(vec![
                    Button::labelled("Delete"),
                    Button::labelled("Merge down"),
                ]),
            ]),
            bar: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::new(0, 0, 0), 1),
            ),
            pointer: Point::new(-1, -1),
            press: None,
            rename: None,
            rows_focused: false,
            idle: true,
        }
    }

    /// What the pane is laid out from across `width` over `count` layers.
    #[must_use]
    pub fn measures(
        &self,
        count: usize,
        width: u32,
        faces: Faces,
        scale: Scale,
        theme: &Theme,
    ) -> LayerMeasures {
        let row = thumb_side(scale)
            .max(faces.label.line_height())
            .saturating_add(scale.scale_length(PAD).saturating_mul(2));
        LayerMeasures {
            panel: self.panel.measured_height(width, faces, scale, theme),
            row,
            count,
        }
    }

    /// How far the rows are scrolled, in pixels.
    #[must_use]
    pub fn offset(&self) -> u32 {
        u32::try_from(self.bar.model().offset()).unwrap_or(u32::MAX)
    }

    /// Whether a press is held on the pane: on a row, a control or the bar.
    #[must_use]
    pub fn holding(&self) -> bool {
        self.press.is_some() || self.panel.holding() || self.bar.is_pressing()
    }

    /// Whether the pane has the keyboard: its controls, its rows, or a name
    /// being typed.
    #[must_use]
    pub fn has_focus(&self) -> bool {
        self.rows_focused || self.rename.is_some() || self.panel.focus().is_some()
    }

    /// The revision of the layer being renamed, while one is.
    #[must_use]
    pub fn renaming(&self) -> Option<u64> {
        self.rename.as_ref().map(|rename| rename.revision)
    }

    /// What the name being typed holds, while a layer is renamed.
    #[must_use]
    pub fn typed_name(&self) -> Option<&str> {
        self.rename.as_ref().map(|rename| rename.field.text())
    }

    /// Where layer `layer`'s row shows its parts, laid out as `placed`.
    #[cfg(test)]
    #[must_use]
    pub fn parts_of(
        &self,
        layer: usize,
        placed: &Placed,
        faces: Faces,
        scale: Scale,
    ) -> Option<RowParts> {
        let display = placed.display_of(layer)?;
        Some(parts(placed.row_rect(display, self.offset()), faces, scale))
    }

    /// Where the opacity is typed, laid out as `placed`.
    #[cfg(test)]
    #[must_use]
    pub fn opacity_field(
        &self,
        placed: &Placed,
        (faces, scale, theme): (Faces, Scale, &Theme),
    ) -> Option<Rect> {
        self.panel
            .place_of(OPACITY_PART, placed.panel, faces, scale, theme)
            .map(|part| part.controls[0])
    }

    /// Where layer `layer`'s row shows: empty where it does not.
    #[must_use]
    pub fn row_rect(&self, layer: usize, placed: &Placed) -> Rect {
        placed.display_of(layer).map_or(Rect::EMPTY, |display| {
            placed
                .row_rect(display, self.offset())
                .intersection(&placed.rows)
        })
    }

    /// Show the opacity of the layer painted on, and offer what may be done
    /// to the layers: nothing changes a palette picture's one layer, nor a
    /// picture a worker has, when no rename begins either. A rename whose
    /// layer no longer holds its revision is dropped. Answers whether what the
    /// controls draw changed.
    pub fn sync(
        &mut self,
        (layers, active): (&[Layer], usize),
        (colour, mergeable, idle): (bool, bool, bool),
    ) -> bool {
        if let Some(rename) = self
            .rename
            .take_if(|rename| layer_at(layers, rename.revision).is_none())
        {
            self.rows_focused = rename.from_rows;
        }
        self.idle = idle;
        let withheld = !(colour && idle);
        let mut changed = self.panel.withheld() != withheld;
        if changed {
            self.panel.set_withheld(withheld);
        }
        let percent = layers
            .get(active)
            .map_or(OPACITY.most, |layer| percent_of(layer.opacity));
        if let Some(part) = self.panel.part_mut(OPACITY_PART) {
            changed |= part.show(0, percent);
        }
        let room = layers.len() < MOST_LAYERS;
        changed |= enable(&mut self.panel, ADD_PART, [room, room]);
        changed |= enable(&mut self.panel, REMOVE_PART, [layers.len() > 1, mergeable]);
        changed
    }

    /// Bring the rows' bar into line with `placed`, the offset held within
    /// the rows; answers whether the rows moved.
    pub fn settle(&mut self, placed: &Placed) -> bool {
        let range = self.bar.model().range().resize(
            u64::from(rows_span(placed.count, placed.row)),
            u64::from(placed.rows.height),
        );
        let moved = range.offset() != self.bar.model().offset();
        self.bar
            .set_model(ScrollModel::in_pixels(range, u64::from(placed.row.max(1))));
        moved
    }

    /// Scroll the rows so layer `layer`'s shows, answering whether they moved.
    pub fn reveal(&mut self, layer: usize, placed: &Placed) -> bool {
        let Some(display) = placed.display_of(layer) else {
            return false;
        };
        let start = u64::from(rows_span(display, placed.row));
        let model = self.bar.model().revealing(start, u64::from(placed.row));
        let moved = model.offset() != self.bar.model().offset();
        if moved {
            self.bar.set_model(model);
        }
        moved
    }

    /// Begin renaming `held`, layer `layer`, in place, where no worker has the
    /// picture: its name in a field, all of it chosen, the field holding the
    /// keyboard.
    fn begin_rename(&mut self, layer: usize, held: &Layer, placed: &Placed, damage: &mut Region) {
        if !self.idle {
            return;
        }
        let name = held.name.as_str();
        let mut field = TextField::new()
            .with_text(name)
            .with_max_len(MAX_LAYER_NAME)
            .with_selection(0..name.len());
        field.set_focused(true);
        self.rename = Some(Rename {
            revision: held.revision,
            field,
            from_rows: core::mem::take(&mut self.rows_focused),
        });
        damage.add(self.row_rect(layer, placed));
    }

    /// Stop renaming: the rows have the keyboard again where they had it.
    pub fn end_rename(&mut self, layers: &[Layer], placed: &Placed, damage: &mut Region) {
        if let Some(row) = self.renamed_row(layers, placed) {
            damage.add(row.intersection(&placed.rows));
        }
        if let Some(rename) = self.rename.take() {
            self.rows_focused = rename.from_rows;
        }
    }

    /// Where the row of the layer being renamed lies, found by its revision,
    /// while one of `layers` still holds it.
    fn renamed_row(&self, layers: &[Layer], placed: &Placed) -> Option<Rect> {
        let layer = layer_at(layers, self.rename.as_ref()?.revision)?;
        Some(placed.row_rect(placed.display_of(layer)?, self.offset()))
    }

    /// Feed a pointer event to the pane laid out as `placed` over `layers`,
    /// painted on at `active`: a press the controls take takes the keyboard
    /// from the rows, so the keys after it reach what it landed on.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        placed: &Placed,
        (layers, active): (&[Layer], usize),
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> LayersOutcome {
        let (_, scale, theme) = look;
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if let Some(outcome) = self.rename_pointer(event, placed, layers, look, damage) {
            return outcome;
        }
        if self
            .bar
            .on_pointer(event, placed.bar, scale, theme, damage)
            .is_some()
        {
            damage.add(placed.rows);
            return LayersOutcome::Taken;
        }
        match self
            .panel
            .on_pointer(event, (placed.panel, placed.bounds), look, damage)
        {
            PanelOutcome::Ignored => {
                self.rows_pointer(event, placed, (layers, active), look, damage)
            }
            outcome => {
                if matches!(event, InputEvent::PointerPressed { .. })
                    && core::mem::take(&mut self.rows_focused)
                {
                    damage.add(placed.rows);
                }
                answered(outcome)
            }
        }
    }

    /// The pointer while a layer is renamed: the field takes what lands on
    /// it, and a press beside it settles the name.
    fn rename_pointer(
        &mut self,
        event: &InputEvent,
        placed: &Placed,
        layers: &[Layer],
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<LayersOutcome> {
        let row = self.renamed_row(layers, placed)?;
        let pointer = self.pointer;
        let rename = self.rename.as_mut()?;
        let field = field_rect(row, parts(row, faces, scale).name, scale, theme);
        let pressed = matches!(event, InputEvent::PointerPressed { .. });
        if pressed && !field.contains(pointer) {
            return Some(LayersOutcome::Renamed(rename.revision));
        }
        let typed = rename.field.on_pointer(event, field, scale, theme, damage);
        (typed.is_some() || pressed).then_some(LayersOutcome::Taken)
    }

    /// A key while a layer is renamed: the field takes it, Enter answering
    /// the name and Escape dropping it.
    fn rename_key(
        &mut self,
        stroke: Keystroke,
        placed: &Placed,
        layers: &[Layer],
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> LayersOutcome {
        let Some(row) = self.renamed_row(layers, placed) else {
            self.end_rename(layers, placed, damage);
            return LayersOutcome::Taken;
        };
        let field = field_rect(row, parts(row, faces, scale).name, scale, theme);
        let Some(rename) = self.rename.as_mut() else {
            return LayersOutcome::Taken;
        };
        let revision = rename.revision;
        match rename
            .field
            .on_key(stroke.key, stroke.modifiers, field, damage)
        {
            Some(TextAction::Submitted) => LayersOutcome::Renamed(revision),
            Some(TextAction::Cancelled) => {
                self.end_rename(layers, placed, damage);
                LayersOutcome::Taken
            }
            _ => LayersOutcome::Taken,
        }
    }

    /// The pointer on the rows: a press on an eye shows or hides its layer,
    /// one elsewhere on a row paints on its layer, and a press dragged moves
    /// its layer to the gap it is let go at.
    fn rows_pointer(
        &mut self,
        event: &InputEvent,
        placed: &Placed,
        (layers, active): (&[Layer], usize),
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> LayersOutcome {
        let (faces, scale, _) = look;
        let offset = self.offset();
        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                let Some(display) = placed.row_at(self.pointer, offset) else {
                    return LayersOutcome::Ignored;
                };
                let Some(layer) = placed.count.checked_sub(display + 1) else {
                    return LayersOutcome::Ignored;
                };
                let parts = parts(placed.row_rect(display, offset), faces, scale);
                if parts.eye.contains(self.pointer) {
                    return LayersOutcome::Show(layer);
                }
                self.press = Some(Press {
                    layer,
                    at: self.pointer,
                    renames: layer == active && parts.name.contains(self.pointer),
                    landing: None,
                });
                if layer == active {
                    LayersOutcome::Taken
                } else {
                    LayersOutcome::Select(layer)
                }
            }
            InputEvent::PointerMoved { .. } => self.drag_row(placed, offset, scale, damage),
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => self.release_row(placed, layers, look, damage),
            InputEvent::PointerCancelled => {
                let Some(press) = self.press.take() else {
                    return LayersOutcome::Ignored;
                };
                if let Some(gap) = press.landing {
                    damage.add(landing_mark(placed, gap, offset, scale));
                }
                LayersOutcome::Taken
            }
            _ => LayersOutcome::Ignored,
        }
    }

    /// A press held on a row moved: past the drag's slop, it marks the gap
    /// its layer would land in.
    fn drag_row(
        &mut self,
        placed: &Placed,
        offset: u32,
        scale: Scale,
        damage: &mut Region,
    ) -> LayersOutcome {
        let pointer = self.pointer;
        let Some(press) = self.press.as_mut() else {
            return LayersOutcome::Ignored;
        };
        if press.landing.is_none() && !past_drag_slop(press.at, pointer, scale) {
            return LayersOutcome::Taken;
        }
        let landing = Some(placed.gap_at(pointer, offset));
        if press.landing != landing {
            let was = core::mem::replace(&mut press.landing, landing);
            press.renames = false;
            for gap in [was, landing].into_iter().flatten() {
                damage.add(landing_mark(placed, gap, offset, scale));
            }
        }
        LayersOutcome::Taken
    }

    /// A press held on a row was let go: dragged, its layer moves to the gap
    /// marked; let go where it began, on the name of the layer painted on,
    /// that layer is renamed in place.
    fn release_row(
        &mut self,
        placed: &Placed,
        layers: &[Layer],
        (faces, scale, _): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> LayersOutcome {
        let Some(press) = self.press.take() else {
            return LayersOutcome::Ignored;
        };
        let offset = self.offset();
        let Some(display) = placed.display_of(press.layer) else {
            return LayersOutcome::Taken;
        };
        if let Some(gap) = press.landing {
            damage.add(landing_mark(placed, gap, offset, scale));
            let lands = if gap > display { gap - 1 } else { gap };
            let to = placed.count - 1 - lands.min(placed.count - 1);
            return if to == press.layer {
                LayersOutcome::Taken
            } else {
                LayersOutcome::Move {
                    from: press.layer,
                    to,
                }
            };
        }
        let name = parts(placed.row_rect(display, offset), faces, scale).name;
        if let (true, Some(layer)) = (
            press.renames && name.contains(self.pointer),
            layers.get(press.layer),
        ) {
            self.begin_rename(press.layer, layer, placed, damage);
        }
        LayersOutcome::Taken
    }

    /// Feed a key while the pane has the keyboard: the name being typed takes
    /// every key but the window's chords; the controls walk their stops onto
    /// the rows; on the rows Up and Down paint on the layer above or below,
    /// Home and End on the top or the bottom, Space shows or hides the layer
    /// painted on and Enter renames it.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        placed: &Placed,
        (layers, active): (&[Layer], usize),
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> LayersOutcome {
        if self.rename.is_some() {
            if owner_chord(stroke.key, stroke.modifiers) {
                return LayersOutcome::Ignored;
            }
            return self.rename_key(stroke, placed, layers, look, damage);
        }
        if self.rows_focused {
            return self.rows_key(stroke, placed, (layers, active), damage);
        }
        match self
            .panel
            .on_key(stroke, (placed.panel, placed.bounds), look, damage)
        {
            PanelOutcome::Left { forward: true } if placed.count > 0 => {
                self.rows_focused = true;
                damage.add(placed.rows);
                LayersOutcome::Taken
            }
            outcome => answered(outcome),
        }
    }

    /// A key while the rows have the keyboard.
    fn rows_key(
        &mut self,
        stroke: Keystroke,
        placed: &Placed,
        (layers, active): (&[Layer], usize),
        damage: &mut Region,
    ) -> LayersOutcome {
        let top = placed.count.saturating_sub(1);
        let to = match stroke.key {
            Key::Named(NamedKey::Up) => Some(active.saturating_add(1).min(top)),
            Key::Named(NamedKey::Down) => Some(active.saturating_sub(1)),
            Key::Named(NamedKey::Home) => Some(top),
            Key::Named(NamedKey::End) => Some(0),
            _ => None,
        };
        if let Some(to) = to {
            return if to == active {
                LayersOutcome::Taken
            } else {
                LayersOutcome::Select(to)
            };
        }
        match stroke.key {
            Key::Char(' ') => LayersOutcome::Show(active),
            Key::Named(NamedKey::Enter) => {
                if let Some(layer) = layers.get(active) {
                    self.begin_rename(active, layer, placed, damage);
                }
                LayersOutcome::Taken
            }
            Key::Named(NamedKey::Tab) => {
                self.rows_focused = false;
                damage.add(placed.rows);
                let back = stroke.modifiers.shift;
                if back && self.panel.enter_focus(false, placed.panel, damage) {
                    LayersOutcome::Taken
                } else {
                    LayersOutcome::Left { forward: !back }
                }
            }
            _ => LayersOutcome::Ignored,
        }
    }

    /// Give the pane the keyboard: its controls' first stop, or the rows when
    /// not `forward`, each giving way to the other where it has nothing to
    /// take it; `false` where neither has.
    pub fn enter_focus(&mut self, forward: bool, placed: &Placed, damage: &mut Region) -> bool {
        let rows = placed.count > 0 && !placed.rows.is_empty();
        if forward && self.panel.enter_focus(true, placed.panel, damage) {
            return true;
        }
        if rows {
            self.rows_focused = true;
            damage.add(placed.rows);
            return true;
        }
        !forward && self.panel.enter_focus(false, placed.panel, damage)
    }

    /// Take the keyboard from the pane, a name being typed answered to be
    /// settled, as is an opacity typed.
    pub fn blur(
        &mut self,
        placed: &Placed,
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> LayersOutcome {
        if core::mem::take(&mut self.rows_focused) {
            damage.add(placed.rows);
        }
        if let Some(rename) = &self.rename {
            return LayersOutcome::Renamed(rename.revision);
        }
        self.blur_panel(placed, look, damage)
    }

    /// Settle what the controls hold typed and take the keyboard from them.
    pub fn blur_panel(
        &mut self,
        placed: &Placed,
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> LayersOutcome {
        self.panel
            .blur(placed.panel, look, damage)
            .map_or(LayersOutcome::Ignored, |event| {
                answered(PanelOutcome::Event(event))
            })
    }

    /// Scroll the rows by a turn of the wheel, answering whether they moved.
    pub fn wheel(&mut self, dy: i32, placed: &Placed, scale: Scale, damage: &mut Region) -> bool {
        let scrolled = self.bar.wheel(0, dy, scale, placed.bar, damage).is_some();
        if scrolled {
            damage.add(placed.rows);
        }
        scrolled
    }

    /// When a press held on the rows' bar steps next.
    pub fn repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.bar.repeat_deadline(now_ns)
    }

    /// Step a press held on the rows' bar, where it has fallen due.
    pub fn repeat(
        &mut self,
        now_ns: u64,
        placed: &Placed,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if self
            .bar
            .repeat(now_ns, placed.bar, scale, theme, damage)
            .is_some()
        {
            damage.add(placed.rows);
        }
    }

    /// Paint the pane laid out as `placed` over `layers`, painted on at
    /// `active`: each row's thumbnail as `thumbs` finds it by its layer's
    /// revision, the picture glyph until one lands, and its eye through
    /// `artwork`.
    pub fn render<'a>(
        &self,
        surface: &mut Surface,
        placed: &Placed,
        (layers, active): (&[Layer], usize),
        thumbs: &dyn Fn(u64) -> Option<&'a Surface>,
        artwork: &mut dyn IconArtwork,
        look: (Faces, Scale, &Theme),
    ) {
        let (faces, scale, theme) = look;
        self.panel.render(surface, placed.panel, look, &Plain);
        if !placed.bar.is_empty() {
            self.bar.render(surface, placed.bar, scale, theme);
        }
        let (Ok(x), Ok(y)) = (
            u32::try_from(placed.rows.left()),
            u32::try_from(placed.rows.top()),
        ) else {
            return;
        };
        let offset = self.offset();
        surface.with_clip(x, y, placed.rows.width, placed.rows.height, |surface| {
            for display in placed.shown(offset) {
                let Some(at) = placed.count.checked_sub(display + 1) else {
                    continue;
                };
                let Some(layer) = layers.get(at) else {
                    continue;
                };
                let row = Row {
                    rect: placed.row_rect(display, offset),
                    working: at == active,
                    focused: self.rows_focused && at == active,
                };
                render_row(surface, row, layer, thumbs, artwork, look);
            }
            if let Some(gap) = self.press.and_then(|press| press.landing) {
                let mark = landing_mark(placed, gap, offset, scale);
                fill_area(surface, mark, Color::from(theme.palette().accent));
            }
            if let Some((rename, row)) = self.rename.as_ref().zip(self.renamed_row(layers, placed))
            {
                let field = field_rect(row, parts(row, faces, scale).name, scale, theme);
                rename.field.render(surface, field, scale, theme);
            }
        });
    }
}

/// One row: marked where its layer is painted on, then its eye, its
/// thumbnail over the checkerboard, its name and its opacity, muted while the
/// layer is hidden.
fn render_row<'a>(
    surface: &mut Surface,
    row: Row,
    layer: &Layer,
    thumbs: &dyn Fn(u64) -> Option<&'a Surface>,
    artwork: &mut dyn IconArtwork,
    (faces, scale, theme): (Faces, Scale, &Theme),
) {
    let palette = theme.palette();
    if row.working {
        fill_area(surface, row.rect, Color::from(palette.selection_fill));
    }
    let parts = parts(row.rect, faces, scale);
    let quiet = Color::from(palette.on_surface_muted);
    let ink = if layer.visible {
        Color::from(palette.on_surface)
    } else {
        quiet
    };
    let eye = if layer.visible {
        IconKind::Visible
    } else {
        IconKind::Hidden
    };
    if let Some((x, y)) = parts.eye.surface_origin() {
        let side = parts.eye.height;
        let picture = artwork.artwork(IconRequest::kind(eye), side);
        paint_icon_slot(surface, (x, y, side), eye, ink, picture, ArtworkTone::FULL);
    }
    if let Some((x, y)) = parts.thumb.surface_origin() {
        let side = parts.thumb.height;
        Checker::new(theme, scale).paint(surface, x, y, parts.thumb.width, side);
        let drawn = thumbs(layer.revision).map(IconPicture::coloured);
        paint_icon_slot(
            surface,
            (x, y, side),
            IconKind::Image,
            quiet,
            drawn,
            ArtworkTone::FULL,
        );
    }
    let name = faces.label;
    paint_run(
        surface,
        name,
        name.elide_to_width(&layer.name, parts.name.width),
        (
            parts.name.left(),
            name.centred_top(row.rect.top(), row.rect.height),
        ),
        ink,
        None,
    );
    let caption = faces.caption;
    let mut opacity = ArrayString::<8>::new();
    let _ = write!(opacity, "{}%", percent_of(layer.opacity));
    let width = to_i32(caption.text_width(opacity.as_str()));
    paint_run(
        surface,
        caption,
        (opacity.as_str(), false),
        (
            parts.opacity.right().saturating_sub(width),
            caption.centred_top(row.rect.top(), row.rect.height),
        ),
        ink,
        None,
    );
    if row.focused {
        if let Some((x, y)) = row.rect.surface_origin() {
            paint_focus_ring(
                surface,
                (x, y, row.rect.width, row.rect.height),
                scale,
                theme,
            );
        }
    }
}

#[cfg(test)]
#[path = "layers_pane_tests.rs"]
mod tests;
