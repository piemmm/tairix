//! The inspector down a window's right: the selection's style, the layers,
//! the selection's place, alignment and arrangement, and a shape's and
//! text's own measures.
//!
//! The inspector holds its controls and answers [`Change`]s; it changes
//! nothing itself. A change arrives live while a control is being moved and
//! once more, settled, when it stops: the view shows the live one and lands
//! the settled one as a step. A tab is laid out as though the panel were
//! tall enough to hold it, and one that is not scrolls, by the wheel or by
//! the bar beside it.

use alloc::vec::Vec;

use tairix_bezier::{Bounds, FillRule};
use tairix_colour::Rgba;
use tairix_controls::{
    fill_area, ColourPicker, NumberAction, NumberField, PickerOutcome, ScrollAction, ScrollBar,
    ScrollModel, ScrollOrientation, ScrollRange, ScrollView,
};
use tairix_drawing::{ArrowShape, Cap, Drawing, Join, NodePath, Paint, Style, TextAlign};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::edit::Align;
use crate::layout::{px, Faces, Layout};

/// A row of controls, in logical pixels.
const ROW: u32 = 26;

/// The margin about the panel's rows, in logical pixels.
const INSET: u32 = 8;

/// The width of a number field's label, in logical pixels.
const LABEL: u32 = 84;

/// The width of a layer's show-or-hide button, in logical pixels.
const EYE: u32 = 64;

/// The largest measure a field holds, in hundredths: twenty million drawing
/// pixels, near as far as a field's whole number reaches.
const MOST_HUNDREDTHS: i32 = 2_000_000_000;

/// The inspector's tabs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tab {
    /// Fill and stroke.
    Style,
    /// The layers.
    Layers,
    /// Place, alignment and arrangement.
    Object,
    /// A shape's own measures, and text's face and setting.
    Shape,
}

impl Tab {
    const ALL: [Self; 4] = [Self::Style, Self::Layers, Self::Object, Self::Shape];

    const fn label(self) -> &'static str {
        match self {
            Self::Style => "Style",
            Self::Layers => "Layers",
            Self::Object => "Object",
            Self::Shape => "Shape",
        }
    }
}

/// The families text is offered in.
pub const FAMILIES: [(&str, &str); 3] = [
    ("Sans", "sans-serif"),
    ("Serif", "serif"),
    ("Mono", "monospace"),
];

/// Which paint the colour picker edits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// The fill.
    Fill,
    /// The stroke.
    Stroke,
}

/// Dash patterns offered, in stroke widths.
pub const DASHES: [&[f64]; 4] = [&[], &[4.0, 2.0], &[0.0, 2.0], &[4.0, 2.0, 0.0, 2.0]];

/// What each of [`DASHES`] is called.
const DASH_NAMES: [&str; 4] = ["Solid", "Dash", "Dot", "Dash-dot"];

/// Arrowheads offered, in the order a button steps through them.
pub const HEADS: [ArrowShape; 5] = [
    ArrowShape::None,
    ArrowShape::Triangle,
    ArrowShape::Arrow,
    ArrowShape::Circle,
    ArrowShape::Diamond,
];

/// What the start's and the end's button say for each of [`HEADS`].
const HEAD_NAMES: [[&str; 5]; 2] = [
    [
        "Start: none",
        "Start: triangle",
        "Start: arrow",
        "Start: circle",
        "Start: diamond",
    ],
    [
        "End: none",
        "End: triangle",
        "End: arrow",
        "End: circle",
        "End: diamond",
    ],
];

const JOINS: [(&str, Join); 3] = [
    ("Mitre", Join::Miter),
    ("Round", Join::Round),
    ("Bevel", Join::Bevel),
];

const CAPS: [(&str, Cap); 3] = [
    ("Butt", Cap::Butt),
    ("Round", Cap::Round),
    ("Square", Cap::Square),
];

const TEXT_ALIGNS: [(&str, TextAlign); 3] = [
    ("Left", TextAlign::Start),
    ("Centre", TextAlign::Middle),
    ("Right", TextAlign::End),
];

/// What a change in the inspector asks of the view.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Paint the target with this, live or settled.
    Paint {
        /// Which paint.
        target: Target,
        /// What with.
        paint: Paint,
        /// Whether the change has settled.
        settled: bool,
    },
    /// Stroke this wide, in drawing pixels.
    Width {
        /// The width.
        width: f64,
        /// Whether the change has settled.
        settled: bool,
    },
    /// Join corners so.
    Join(Join),
    /// End strokes so.
    Cap(Cap),
    /// Dash strokes with the pattern at this index of [`DASHES`].
    Dash(usize),
    /// Put this arrowhead at the start, or the end when `end`.
    Head {
        /// Which end.
        end: bool,
        /// Which arrowhead.
        shape: ArrowShape,
    },
    /// Composite the selection at this opacity.
    Opacity {
        /// The opacity, 0 to 1.
        opacity: f64,
        /// Whether the change has settled.
        settled: bool,
    },
    /// Fill by the even-odd rule, or the non-zero when false.
    EvenOdd(bool),
    /// A layer command.
    Layer(LayerCommand),
    /// Set one measure of the selection's bounds, in drawing pixels, the
    /// others kept: left, top, width or height, by index.
    Place {
        /// Which measure.
        index: usize,
        /// Its value.
        value: f64,
    },
    /// Line the selection up.
    Align(Align),
    /// Spread the selection evenly, down when true.
    Distribute(bool),
    /// Round a rectangle's corners by this, in drawing pixels.
    Corners(f64),
    /// Give a star this many points.
    Points(u32),
    /// Give a star this inner radius, of its outer.
    Inner(f64),
    /// Make a star a polygon, or a star again.
    Polygon(bool),
    /// Set text in the family of this CSS name.
    Family(&'static str),
    /// Set text this large, in drawing pixels.
    Size(f64),
    /// Set text bold, or not.
    Bold(bool),
    /// Set text italic, or not.
    Italic(bool),
    /// Align text's lines so on its origin.
    TextAlign(TextAlign),
}

/// A layer command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LayerCommand {
    /// Make this the layer new objects go into.
    Choose(usize),
    /// Show or hide this layer.
    Visible(usize),
    /// Lock or unlock this layer.
    Lock(usize),
    /// A new layer above every other.
    New,
    /// Remove the layer in hand.
    Remove,
    /// Move the layer in hand up.
    Raise,
    /// Move the layer in hand down.
    Lower,
}

/// What the view is asked for by an input the inspector took: what letting
/// go of a field it had been typing into settled, then what the input itself
/// changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    released: Option<Change>,
    change: Option<Change>,
}

impl Changes {
    const NONE: Self = Self {
        released: None,
        change: None,
    };

    fn of(change: Option<Change>) -> Self {
        Self {
            released: None,
            change,
        }
    }
}

impl IntoIterator for Changes {
    type Item = Change;
    type IntoIter =
        core::iter::Chain<core::option::IntoIter<Change>, core::option::IntoIter<Change>>;

    fn into_iter(self) -> Self::IntoIter {
        self.released.into_iter().chain(self.change)
    }
}

/// What pressing a button does.
#[derive(Clone, Debug, PartialEq)]
enum Press {
    /// Edit this paint in the picker.
    Target(Target),
    /// Align to the page, or to the selection's bounds.
    ToPage(bool),
    /// Ask the view for this.
    Change(Change),
}

/// A button the inspector draws.
#[derive(Clone, Debug, PartialEq)]
struct Button<'a> {
    rect: Rect,
    label: &'a str,
    on: bool,
    press: Press,
}

/// A number field of the inspector.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Field {
    Width,
    Opacity,
    Place(usize),
    Corners,
    Points,
    Inner,
    Size,
}

/// What holds the inspector's keyboard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Focus {
    /// A number field.
    Field(Field),
    /// The colour picker, on whichever of its parts it last had.
    Picker,
}

impl Field {
    const fn label(self) -> &'static str {
        match self {
            Self::Width => "Stroke width",
            Self::Opacity => "Opacity %",
            Self::Place(0) => "X",
            Self::Place(1) => "Y",
            Self::Place(2) => "Width",
            Self::Place(_) => "Height",
            Self::Corners => "Corners",
            Self::Points => "Points",
            Self::Inner => "Inner %",
            Self::Size => "Text size",
        }
    }
}

fn change(change: Change) -> Press {
    Press::Change(change)
}

/// A tab's controls, laid a row at a time down the panel from its top as
/// though the panel were tall enough to hold them all.
struct Column<'s> {
    left: i32,
    width: u32,
    row: u32,
    label: u32,
    origin: i32,
    top: i32,
    margin: u32,
    fields: Vec<(Field, Rect)>,
    buttons: Vec<Button<'s>>,
    picker: Option<Rect>,
}

impl<'s> Column<'s> {
    /// An empty column down `panel` at `scale`.
    fn new(panel: Rect, scale: Scale) -> Self {
        let inset = scale.scale_length(INSET);
        let origin = panel.top().saturating_add(px(inset / 2));
        Self {
            left: panel.left().saturating_add(px(inset)),
            width: panel.width.saturating_sub(2 * inset),
            row: scale.scale_length(ROW),
            label: scale.scale_length(LABEL),
            origin: panel.top(),
            top: origin,
            margin: inset / 2,
            fields: Vec::new(),
            buttons: Vec::new(),
            picker: None,
        }
    }

    fn step(&self) -> i32 {
        px(self.row)
    }

    fn advance(&mut self, by: i32) {
        self.top = self.top.saturating_add(by);
    }

    /// Half a row of space.
    fn gap(&mut self) {
        self.advance(self.step() / 2);
    }

    /// A number field on a row of its own, its label before it.
    fn field(&mut self, field: Field) {
        let rect = Rect::new(
            self.left.saturating_add(px(self.label)),
            self.top,
            self.width.saturating_sub(self.label),
            self.row.saturating_sub(4),
        );
        self.fields.push((field, rect));
        self.advance(self.step());
    }

    /// A row of buttons sharing the row's width: each its label, whether it
    /// is on, and what pressing it does.
    fn buttons<const N: usize>(&mut self, buttons: [(&'s str, bool, Press); N]) {
        let width = self.width / u32::try_from(N).unwrap_or(1).max(1);
        let mut left = self.left;
        for (label, on, press) in buttons {
            let rect = Rect::new(
                left,
                self.top,
                width.saturating_sub(2),
                self.row.saturating_sub(4),
            );
            self.buttons.push(Button {
                rect,
                label,
                on,
                press,
            });
            left = left.saturating_add(px(width));
        }
        self.advance(self.step());
    }

    /// The colour picker, as tall as `height` answers for the row's width.
    fn picker(&mut self, height: impl FnOnce(u32) -> u32) {
        self.gap();
        let rect = Rect::new(self.left, self.top, self.width, height(self.width));
        self.picker = Some(rect);
        self.advance(px(rect.height));
        self.gap();
    }

    /// The layers' rows, the topmost first as it is drawn: each its name,
    /// which chooses it, and its show-or-hide button. Only the rows within
    /// `seen`, the stretch of the column the panel shows, are laid out, so a
    /// drawing of any number of layers costs what the panel shows.
    fn layers(&mut self, shown: &Shown<'s>, eye: u32, seen: (i32, i32)) {
        let count = shown.layers.len();
        let step = self.step().max(1);
        let first_top = self.top;
        let row_of = |at: i32| usize::try_from(at.saturating_sub(first_top) / step).unwrap_or(0);
        let first = row_of(seen.0).min(count);
        let last = row_of(seen.1.saturating_add(step - 1)).min(count);
        let height = self.row.saturating_sub(4);
        for row in first..last {
            let index = count - 1 - row;
            let Some(layer) = shown.layers.get(index) else {
                continue;
            };
            let top = first_top
                .saturating_add(step.saturating_mul(i32::try_from(row).unwrap_or(i32::MAX)));
            let name = Rect::new(self.left, top, self.width.saturating_sub(eye + 4), height);
            let toggle = Rect::new(
                self.left.saturating_add(px(self.width.saturating_sub(eye))),
                top,
                eye,
                height,
            );
            let visible = layer.visible();
            self.buttons.push(Button {
                rect: name,
                label: layer.name().unwrap_or("Layer"),
                on: index == shown.layer,
                press: change(Change::Layer(LayerCommand::Choose(index))),
            });
            self.buttons.push(Button {
                rect: toggle,
                label: if visible { "Shown" } else { "Hidden" },
                on: visible,
                press: change(Change::Layer(LayerCommand::Visible(index))),
            });
        }
        let rows = i32::try_from(count).unwrap_or(i32::MAX);
        self.advance(step.saturating_mul(rows));
    }

    /// How far down from the panel's top the column reaches.
    fn extent(&self) -> u32 {
        let reach = self.top.saturating_sub(self.origin);
        u32::try_from(reach)
            .unwrap_or(0)
            .saturating_add(self.margin)
    }
}

/// Where the tab shown lays its controls: the column, the scroll it shows
/// through, and the bar beside it when the tab is taller than the panel.
struct Placed<'s> {
    column: Column<'s>,
    view: ScrollView,
    bar: Option<Rect>,
    model: ScrollModel,
}

impl Placed<'_> {
    /// The field `field` is laid at, in the column's own layout.
    fn field_rect(&self, field: Field) -> Option<Rect> {
        let (_, rect) = self.column.fields.iter().find(|(at, _)| *at == field)?;
        Some(*rect)
    }
}

/// A number field's value, in hundredths.
fn hundredths(value: f64) -> i32 {
    let scaled = tairix_util::mathf::round(value * 100.0);
    tairix_util::mathf::round_i32(scaled)
}

/// The paint `target` is of `style`.
fn paint_of(style: &Style, target: Target) -> &Paint {
    match target {
        Target::Fill => &style.fill,
        Target::Stroke => &style.stroke.paint,
    }
}

fn primary_press(event: &InputEvent) -> bool {
    matches!(
        event,
        InputEvent::PointerPressed {
            button: PointerButton::Primary
        }
    )
}

/// The inspector's state.
pub struct Inspector {
    tab: Tab,
    target: Target,
    picker: ColourPicker,
    width: NumberField,
    opacity: NumberField,
    place: [NumberField; 4],
    corners: NumberField,
    points: NumberField,
    inner: NumberField,
    size: NumberField,
    to_page: bool,
    /// What holds the keyboard, if anything does.
    focus: Option<Focus>,
    /// Whether the picker shows a colour live that has not settled, which
    /// the selection's own is not shown over.
    unsettled: bool,
    /// Where the pointer last was, in the window.
    pointer: Point,
    /// The panel's bar; its model's offset is how far the tab is scrolled.
    bar: ScrollBar,
}

impl Inspector {
    /// An inspector showing `style`.
    #[must_use]
    pub fn new(style: &Style) -> Self {
        let colour = match &style.fill {
            Paint::Colour(colour) => *colour,
            _ => Rgba::new(0, 0, 0, 255),
        };
        let measure = |value, least, most| {
            NumberField::new(value, least, most)
                .with_decimals(2)
                .with_steps(100, 1000)
        };
        let empty = ScrollModel::in_pixels(ScrollRange::EMPTY, 1);
        Self {
            tab: Tab::Style,
            target: Target::Fill,
            picker: ColourPicker::new(colour).with_opacity(true),
            width: NumberField::new(hundredths(style.stroke.width), 0, MOST_HUNDREDTHS)
                .with_decimals(2)
                .with_steps(25, 100),
            opacity: NumberField::new(100, 0, 100),
            place: core::array::from_fn(|_| measure(0, -MOST_HUNDREDTHS, MOST_HUNDREDTHS)),
            corners: measure(0, 0, MOST_HUNDREDTHS),
            points: NumberField::new(5, 3, 1000),
            inner: NumberField::new(50, 1, 100),
            size: measure(2400, 100, MOST_HUNDREDTHS),
            to_page: false,
            focus: None,
            unsettled: false,
            pointer: Point::ORIGIN,
            bar: ScrollBar::new(ScrollOrientation::Vertical, empty).without_edges(),
        }
    }

    /// Show what is selected in `drawing`, its bounds `bounds` — or `style`,
    /// what new objects are drawn in, when nothing is. A field being typed
    /// into, and a colour not yet settled, are left as they are.
    pub fn follow(
        &mut self,
        drawing: &Drawing,
        selection: &[NodePath],
        style: &Style,
        bounds: Bounds,
    ) {
        let node = selection.first().and_then(|path| drawing.node_at(path));
        let style = node
            .and_then(|node| node.style())
            .map_or(style, |style| &**style);
        if let (false, Paint::Colour(colour)) = (self.unsettled, paint_of(style, self.target)) {
            self.picker.set_colour(*colour);
        }
        self.show(Field::Width, hundredths(style.stroke.width));
        self.show(
            Field::Opacity,
            hundredths(node.map_or(1.0, |node| node.opacity())),
        );
        match node.map(|node| node.kind()) {
            Some(tairix_drawing::Kind::Shape {
                shape: tairix_drawing::Shape::Rect(rect),
                ..
            }) => self.show(Field::Corners, hundredths(rect.rx)),
            Some(tairix_drawing::Kind::Shape {
                shape: tairix_drawing::Shape::Star(star),
                ..
            }) => {
                self.show(Field::Points, i32::try_from(star.points).unwrap_or(5));
                self.show(Field::Inner, hundredths(star.inner_ratio));
            }
            Some(tairix_drawing::Kind::Text { text, .. }) => {
                self.show(Field::Size, hundredths(text.size));
            }
            _ => {}
        }
        if !bounds.is_empty() {
            let measures = [bounds.min.x, bounds.min.y, bounds.width(), bounds.height()];
            for (index, value) in measures.into_iter().enumerate() {
                self.show(Field::Place(index), hundredths(value));
            }
        }
    }

    /// Show `value` in `field`, unless it is being typed into.
    fn show(&mut self, field: Field, value: i32) {
        if self.focus != Some(Focus::Field(field)) {
            self.field(field).set_value(value);
        }
    }

    /// The tab shown.
    #[cfg(test)]
    pub(crate) const fn tab(&self) -> Tab {
        self.tab
    }

    /// Where the stroke width's field shows in the window.
    #[cfg(test)]
    pub(crate) fn stroke_width_at(
        &self,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        shown: &Shown<'_>,
    ) -> Option<Rect> {
        let placed = self.place(layout, scale, theme, shown);
        placed
            .field_rect(Field::Width)
            .and_then(|rect| placed.view.to_window(rect))
    }

    /// Whether alignment is to the page.
    #[must_use]
    pub const fn to_page(&self) -> bool {
        self.to_page
    }

    /// Whether a field holds the keyboard.
    #[must_use]
    pub const fn has_keyboard(&self) -> bool {
        self.focus.is_some()
    }

    /// Whether a drag the inspector began holds the pointer.
    #[must_use]
    pub fn holds_pointer(&self) -> bool {
        self.picker.is_dragging() || self.bar.is_pressing()
    }

    /// Whether the pointer was last seen over the inspector, so the move
    /// that takes it away is the inspector's too.
    #[must_use]
    pub fn was_over(&self, layout: &Layout) -> bool {
        layout.panel.contains(self.pointer) || layout.tabs.contains(self.pointer)
    }

    /// The tab strip's buttons.
    fn tab_rects(layout: &Layout) -> [(Tab, Rect); 4] {
        let tabs = layout.tabs;
        let width = tabs.width / 4;
        core::array::from_fn(|index| {
            let at = px(u32::try_from(index).unwrap_or(0) * width);
            let rect = Rect::new(
                tabs.left().saturating_add(at),
                tabs.top(),
                width,
                tabs.height,
            );
            (Tab::ALL[index], rect)
        })
    }

    /// The tab shown, laid out in `layout`'s panel and scrolled as the bar
    /// says, the bar's place set aside where the tab overflows the panel.
    fn place<'s>(
        &self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        shown: &'s Shown<'s>,
    ) -> Placed<'s> {
        let panel = layout.panel;
        let mut viewport = panel;
        // Measured first with nothing in view, which lays out no row of a
        // long list, then laid out for the stretch the scroll shows.
        let unseen = (0, 0);
        let mut extent = self.column(viewport, scale, theme, shown, unseen).extent();
        let mut bar = None;
        if extent > panel.height {
            let breadth = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
            bar = Some(viewport.take_right(breadth.min(viewport.width)));
            extent = self.column(viewport, scale, theme, shown, unseen).extent();
        }
        let offset = self.bar.model().offset();
        let range = ScrollRange::new(u64::from(extent), u64::from(viewport.height), offset);
        let model = ScrollModel::in_pixels(range, u64::from(scale.scale_length(ROW)));
        let view = ScrollView::new(ScrollOrientation::Vertical, viewport, model.offset());
        let top = viewport
            .top()
            .saturating_add(i32::try_from(model.offset()).unwrap_or(i32::MAX));
        let seen = (top, top.saturating_add(px(viewport.height)));
        let column = self.column(viewport, scale, theme, shown, seen);
        Placed {
            column,
            view,
            bar,
            model,
        }
    }

    /// The tab shown, laid down `panel`, laying out only the rows of a long
    /// list within `seen`.
    fn column<'s>(
        &self,
        panel: Rect,
        scale: Scale,
        theme: &Theme,
        shown: &'s Shown<'s>,
        seen: (i32, i32),
    ) -> Column<'s> {
        let mut column = Column::new(panel, scale);
        match self.tab {
            Tab::Style => self.style_controls(&mut column, scale, theme, shown.style),
            Tab::Layers => layer_controls(&mut column, scale, shown, seen),
            Tab::Object => self.object_controls(&mut column),
            Tab::Shape => shape_controls(&mut column, shown),
        }
        column
    }

    fn style_controls(&self, column: &mut Column<'_>, scale: Scale, theme: &Theme, style: &Style) {
        column.buttons([
            (
                "Fill",
                self.target == Target::Fill,
                Press::Target(Target::Fill),
            ),
            (
                "Stroke",
                self.target == Target::Stroke,
                Press::Target(Target::Stroke),
            ),
        ]);
        let none = matches!(paint_of(style, self.target), Paint::None);
        let paint = |paint| {
            change(Change::Paint {
                target: self.target,
                paint,
                settled: true,
            })
        };
        column.buttons([
            ("None", none, paint(Paint::None)),
            ("Colour", !none, paint(Paint::Colour(self.picker.colour()))),
        ]);
        column.picker(|width| self.picker.measured_height(width, scale, theme));
        column.field(Field::Width);
        let stroke = &style.stroke;
        column.buttons(
            JOINS.map(|(label, join)| (label, stroke.join == join, change(Change::Join(join)))),
        );
        column
            .buttons(CAPS.map(|(label, cap)| (label, stroke.cap == cap, change(Change::Cap(cap)))));
        let dashed = dash_of(stroke);
        column.buttons(core::array::from_fn::<_, 4, _>(|index| {
            (
                DASH_NAMES[index],
                dashed == Some(index),
                change(Change::Dash(index)),
            )
        }));
        for (end, names) in [false, true].into_iter().zip(HEAD_NAMES) {
            let head = if end { stroke.end } else { stroke.start };
            let at = HEADS
                .iter()
                .position(|&shape| shape == head.shape)
                .unwrap_or(0);
            let shape = HEADS[(at + 1) % HEADS.len()];
            let on = head.shape != ArrowShape::None;
            column.buttons([(names[at], on, change(Change::Head { end, shape }))]);
        }
        let rule = style.fill_rule;
        column.buttons([
            (
                "Non-zero",
                rule == FillRule::NonZero,
                change(Change::EvenOdd(false)),
            ),
            (
                "Even-odd",
                rule == FillRule::EvenOdd,
                change(Change::EvenOdd(true)),
            ),
        ]);
        column.field(Field::Opacity);
    }

    fn object_controls(&self, column: &mut Column<'_>) {
        for index in 0..4 {
            column.field(Field::Place(index));
        }
        column.gap();
        let align = |align| change(Change::Align(align));
        column.buttons([
            ("Left", false, align(Align::Left)),
            ("Centre", false, align(Align::Centre)),
            ("Right", false, align(Align::Right)),
        ]);
        column.buttons([
            ("Top", false, align(Align::Top)),
            ("Middle", false, align(Align::Middle)),
            ("Bottom", false, align(Align::Bottom)),
        ]);
        column.buttons([(
            "Align to the page",
            self.to_page,
            Press::ToPage(!self.to_page),
        )]);
        column.gap();
        column.buttons([
            ("Spread across", false, change(Change::Distribute(false))),
            ("Spread down", false, change(Change::Distribute(true))),
        ]);
    }

    /// The number field `field` is.
    fn field(&mut self, field: Field) -> &mut NumberField {
        match field {
            Field::Width => &mut self.width,
            Field::Opacity => &mut self.opacity,
            Field::Place(index) => &mut self.place[index.min(3)],
            Field::Corners => &mut self.corners,
            Field::Points => &mut self.points,
            Field::Inner => &mut self.inner,
            Field::Size => &mut self.size,
        }
    }

    fn field_shown(&self, field: Field) -> &NumberField {
        match field {
            Field::Width => &self.width,
            Field::Opacity => &self.opacity,
            Field::Place(index) => &self.place[index.min(3)],
            Field::Corners => &self.corners,
            Field::Points => &self.points,
            Field::Inner => &self.inner,
            Field::Size => &self.size,
        }
    }

    /// Scroll the tab to `offset` within `model`.
    fn scroll(&mut self, model: ScrollModel, offset: u64, layout: &Layout, damage: &mut Region) {
        self.bar.set_model(model.scroll_to(offset));
        damage.add(layout.panel);
    }

    /// Feed a pointer event over the inspector, answering what it changes.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        shown: &Shown<'_>,
        damage: &mut Region,
    ) -> Changes {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        let placed = self.place(layout, scale, theme, shown);
        self.bar.set_model(placed.model);
        let mut moved = Region::new();
        if let Some(picker) = placed.column.picker.filter(|_| self.picker.is_dragging()) {
            // A drag holds the pointer wherever it goes until it lets go.
            let held = placed
                .view
                .confined_to(layout.window)
                .event_in_layout(event);
            let answer = self.picker_answer(&held, picker, (scale, theme), &mut moved);
            placed.view.report(&moved, damage);
            return Changes::of(answer);
        }
        if let Some(bar) = placed.bar {
            if let Some(ScrollAction::ScrollTo { offset }) =
                self.bar.on_pointer(event, bar, scale, theme, damage)
            {
                self.scroll(placed.model, offset, layout, damage);
            }
            if bar.contains(self.pointer) || self.bar.is_pressing() {
                return Changes::NONE;
            }
        }
        let local = placed.view.to_content(self.pointer);
        let inside = |rect: Rect| local.is_some_and(|point| rect.contains(point));
        let mapped = placed.view.event_in_layout(event);
        if let InputEvent::PointerScrolled { dx, dy } = *event {
            let focused = match self.focus {
                Some(Focus::Field(field)) => placed.field_rect(field).map(|rect| (field, rect)),
                _ => None,
            };
            if let Some((field, rect)) = focused.filter(|(_, rect)| inside(*rect)) {
                let action = self
                    .field(field)
                    .on_pointer(&mapped, rect, scale, theme, &mut moved);
                placed.view.report(&moved, damage);
                return Changes::of(action.and_then(|action| Self::field_change(field, action)));
            }
            if let Some(bar) = placed.bar.filter(|_| layout.panel.contains(self.pointer)) {
                if let Some(ScrollAction::ScrollTo { offset }) =
                    self.bar.wheel(dx, dy, scale, bar, damage)
                {
                    self.scroll(placed.model, offset, layout, damage);
                }
            }
            return Changes::NONE;
        }
        if let Some(picker) = placed.column.picker.filter(|rect| inside(*rect)) {
            let mut released = None;
            if primary_press(event) && self.focus != Some(Focus::Picker) {
                released = self.let_go(&placed, (layout, scale, theme), damage);
                self.focus = Some(Focus::Picker);
                self.picker.set_focused(true);
                damage.add(layout.panel);
            }
            let change = self.picker_answer(&mapped, picker, (scale, theme), &mut moved);
            placed.view.report(&moved, damage);
            return Changes { released, change };
        }
        for &(field, rect) in &placed.column.fields {
            let over = inside(rect);
            let focused = self.focus == Some(Focus::Field(field));
            if !over && !focused {
                continue;
            }
            let mut released = None;
            if over && primary_press(event) && !focused {
                released = self.let_go(&placed, (layout, scale, theme), damage);
                self.focus = Some(Focus::Field(field));
                self.field(field).set_focused(true);
            }
            let action = self
                .field(field)
                .on_pointer(&mapped, rect, scale, theme, &mut moved);
            placed.view.report(&moved, damage);
            let change = action.and_then(|action| Self::field_change(field, action));
            if over || change.is_some() || released.is_some() {
                return Changes { released, change };
            }
        }
        if !primary_press(event) {
            return Changes::NONE;
        }
        let released = self.let_go(&placed, (layout, scale, theme), damage);
        let change = self.press(&placed, layout, local, shown, damage);
        Changes { released, change }
    }

    /// What a press, at `local` in the column's layout, changes.
    fn press(
        &mut self,
        placed: &Placed<'_>,
        layout: &Layout,
        local: Option<Point>,
        shown: &Shown<'_>,
        damage: &mut Region,
    ) -> Option<Change> {
        let tab = Self::tab_rects(layout)
            .into_iter()
            .find(|(_, rect)| rect.contains(self.pointer));
        if let Some((tab, _)) = tab {
            if tab != self.tab {
                self.tab = tab;
                self.bar.set_model(placed.model.scroll_to(0));
            }
            damage.add(layout.tabs);
            damage.add(layout.panel);
            return None;
        }
        let local = local?;
        let button = placed
            .column
            .buttons
            .iter()
            .find(|button| button.rect.contains(local))?;
        damage.add(layout.panel);
        match &button.press {
            Press::Target(target) => {
                self.target = *target;
                if let Paint::Colour(colour) = paint_of(shown.style, *target) {
                    self.picker.set_colour(*colour);
                }
                None
            }
            Press::ToPage(to_page) => {
                self.to_page = *to_page;
                None
            }
            Press::Change(change) => Some(change.clone()),
        }
    }

    /// What the picker, laid at `bounds`, answers `event` with.
    fn picker_answer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Change> {
        let outcome = self.picker.on_pointer(event, bounds, scale, theme, damage);
        self.picker_change(outcome)
    }

    /// What the picker's `outcome` changes, kept track of so a colour shown
    /// live is not shown over until it settles.
    fn picker_change(&mut self, outcome: PickerOutcome) -> Option<Change> {
        let (colour, settled) = match outcome {
            PickerOutcome::Edited(colour) => (colour, false),
            PickerOutcome::Settled(colour) => (colour, true),
            PickerOutcome::Ignored | PickerOutcome::Taken => return None,
        };
        self.unsettled = !settled;
        Some(Change::Paint {
            target: self.target,
            paint: Paint::Colour(colour),
            settled,
        })
    }

    /// What a number field's action changes.
    fn field_change(field: Field, action: NumberAction) -> Option<Change> {
        let (value, settled) = match action {
            NumberAction::Edited { value } => (value, false),
            NumberAction::Settled { value } => (value, true),
        };
        if !settled && !matches!(field, Field::Width | Field::Opacity) {
            return None;
        }
        let measure = f64::from(value) / 100.0;
        Some(match field {
            Field::Points => Change::Points(u32::try_from(value).unwrap_or(5)),
            Field::Corners => Change::Corners(measure),
            Field::Inner => Change::Inner(measure),
            Field::Size => Change::Size(measure),
            Field::Width => Change::Width {
                width: measure,
                settled,
            },
            Field::Opacity => Change::Opacity {
                opacity: measure,
                settled,
            },
            Field::Place(index) => Change::Place {
                index,
                value: measure,
            },
        })
    }

    /// Feed a key to what holds the keyboard: Tab, or Shift and Tab, past the
    /// picker's last part or from a field, moves it on to the next, or the
    /// one before; Escape the picker does not want lets go of it.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        shown: &Shown<'_>,
        damage: &mut Region,
    ) -> Changes {
        let Some(focus) = self.focus else {
            return Changes::NONE;
        };
        let placed = self.place(layout, scale, theme, shown);
        let tab = key == Key::Named(NamedKey::Tab);
        let mut moved = Region::new();
        match focus {
            Focus::Picker => {
                let Some(rect) = placed.column.picker else {
                    self.focus = None;
                    self.picker.set_focused(false);
                    return Changes::NONE;
                };
                let outcome = self
                    .picker
                    .on_key(key, modifiers, rect, (scale, theme), &mut moved);
                placed.view.report(&moved, damage);
                match outcome {
                    PickerOutcome::Ignored if tab => {
                        self.walk(&placed, !modifiers.shift, (layout, scale, theme), damage)
                    }
                    PickerOutcome::Ignored if key == Key::Named(NamedKey::Escape) => Changes {
                        released: self.let_go(&placed, (layout, scale, theme), damage),
                        change: None,
                    },
                    outcome => Changes::of(self.picker_change(outcome)),
                }
            }
            Focus::Field(_) if tab => {
                self.walk(&placed, !modifiers.shift, (layout, scale, theme), damage)
            }
            Focus::Field(field) => {
                let Some(rect) = placed.field_rect(field) else {
                    return Changes::NONE;
                };
                let action = self.field(field).on_key(key, modifiers, rect, &mut moved);
                placed.view.report(&moved, damage);
                Changes::of(action.and_then(|action| Self::field_change(field, action)))
            }
        }
    }

    /// Move the keyboard from what holds it to the next of the picker and
    /// the fields, or the one before when not `forward`, committing what it
    /// leaves, and scroll the panel to show it.
    fn walk(
        &mut self,
        placed: &Placed<'_>,
        forward: bool,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        damage: &mut Region,
    ) -> Changes {
        let from = self.focus;
        let released = self.let_go(placed, (layout, scale, theme), damage);
        let fields = &placed.column.fields;
        let picker = placed.column.picker;
        let first_field = usize::from(picker.is_some());
        let count = fields.len() + first_field;
        let at = match from {
            Some(Focus::Field(field)) => fields
                .iter()
                .position(|(each, _)| *each == field)
                .map_or(0, |at| at + first_field),
            _ => 0,
        };
        let next = if forward {
            (at + 1) % count.max(1)
        } else {
            (at + count.max(1) - 1) % count.max(1)
        };
        let stop = match next.checked_sub(first_field) {
            None => picker.inspect(|&rect| {
                self.focus = Some(Focus::Picker);
                self.picker.enter_focus(forward, rect, scale, theme);
            }),
            Some(index) => fields.get(index).map(|&(field, rect)| {
                self.focus = Some(Focus::Field(field));
                self.field(field).set_focused(true);
                rect
            }),
        };
        if let Some(rect) = stop {
            let start = u64::try_from(rect.top().saturating_sub(layout.panel.top())).unwrap_or(0);
            self.bar
                .set_model(placed.model.revealing(start, u64::from(rect.height)));
            damage.add(layout.panel);
        }
        Changes {
            released,
            change: None,
        }
    }

    /// Let go of the keyboard, committing what was typed into a field or the
    /// picker and ending a colour drag, answering what that changes.
    fn let_go(
        &mut self,
        placed: &Placed<'_>,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Change> {
        match self.focus.take()? {
            Focus::Field(field) => self.release_field(field, layout, damage),
            Focus::Picker => {
                damage.add(layout.panel);
                let dragged = self.picker.finish_drag();
                let Some(rect) = placed.column.picker else {
                    self.picker.set_focused(false);
                    return self.picker_change(dragged);
                };
                let mut moved = Region::new();
                let blurred = self.picker.blur(rect, scale, theme, &mut moved);
                placed.view.report(&moved, damage);
                let settled = self.picker_change(blurred);
                self.picker_change(dragged).or(settled)
            }
        }
    }

    /// Let go of the keyboard for a chord or a press elsewhere, answering what
    /// committing what was typed changes.
    pub fn release_keyboard(
        &mut self,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        shown: &Shown<'_>,
        damage: &mut Region,
    ) -> Option<Change> {
        self.focus?;
        let placed = self.place(layout, scale, theme, shown);
        self.let_go(&placed, (layout, scale, theme), damage)
    }

    /// Settle what the controls hold, for work that must see it landed: what
    /// was typed into a field is committed and the field let go, and a colour
    /// being dragged is let go where it stands. A colour typed into the
    /// picker was shown as it was typed, and lands with what is shown, so the
    /// picker settles where it stands and keeps the keyboard.
    pub fn settle_edits(&mut self, layout: &Layout, damage: &mut Region) -> Option<Change> {
        match self.focus? {
            Focus::Field(field) => {
                self.focus = None;
                self.release_field(field, layout, damage)
            }
            Focus::Picker => {
                damage.add(layout.panel);
                let outcome = self.picker.finish_drag();
                let change = self.picker_change(outcome);
                self.picker.set_colour(self.picker.colour());
                self.unsettled = false;
                change
            }
        }
    }

    /// Let go of `field`, answering what committing what was typed into it
    /// changes.
    fn release_field(
        &mut self,
        field: Field,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<Change> {
        let control = self.field(field);
        control.set_focused(false);
        let committed = control.commit(layout.panel, damage);
        damage.add(layout.panel);
        committed.and_then(|action| Self::field_change(field, action))
    }

    /// When a held press on the panel's bar next steps, if one is held.
    pub fn repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.bar.repeat_deadline(now_ns)
    }

    /// Step a held press on the panel's bar, if its step is due at `now_ns`.
    pub fn repeat(
        &mut self,
        now_ns: u64,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        shown: &Shown<'_>,
        damage: &mut Region,
    ) {
        if !self.bar.repeat_due(now_ns) {
            return;
        }
        let placed = self.place(layout, scale, theme, shown);
        self.bar.set_model(placed.model);
        let Some(bar) = placed.bar else {
            self.bar.let_go();
            return;
        };
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.bar.repeat(now_ns, bar, scale, theme, damage)
        {
            self.scroll(placed.model, offset, layout, damage);
        }
    }

    /// Paint the inspector.
    pub fn render(
        &self,
        surface: &mut Surface,
        (layout, scale, theme, faces): (&Layout, Scale, &Theme, Faces),
        shown: &Shown<'_>,
    ) {
        let palette = theme.palette();
        fill_area(surface, layout.tabs, Color::from(palette.surface_raised));
        fill_area(surface, layout.panel, Color::from(palette.surface_raised));
        for (tab, rect) in Self::tab_rects(layout) {
            if tab == self.tab {
                fill_area(surface, rect, Color::from(palette.surface_selected));
            }
            text_in(
                surface,
                rect,
                tab.label(),
                faces,
                Color::from(palette.on_surface),
            );
        }
        let placed = self.place(layout, scale, theme, shown);
        if let Some(bar) = placed.bar {
            let mut shown_bar = self.bar;
            shown_bar.set_model(placed.model);
            shown_bar.render(surface, bar, scale, theme);
        }
        let label = scale.scale_length(LABEL);
        placed.view.paint(surface, |surface| {
            if let Some(picker) = placed.column.picker {
                self.picker.render(surface, picker, scale, theme);
            }
            for &(field, rect) in &placed.column.fields {
                let named = Rect::new(
                    rect.left().saturating_sub(px(label)),
                    rect.top(),
                    label,
                    rect.height,
                );
                text_in(
                    surface,
                    named,
                    field.label(),
                    faces,
                    Color::from(palette.on_surface_muted),
                );
                self.field_shown(field).render(surface, rect, scale, theme);
            }
            for button in &placed.column.buttons {
                let fill = if button.on {
                    palette.surface_selected
                } else {
                    palette.surface
                };
                fill_area(surface, button.rect, Color::from(fill));
                text_in(
                    surface,
                    button.rect,
                    button.label,
                    faces,
                    Color::from(palette.on_surface),
                );
            }
        });
    }
}

fn shape_controls(column: &mut Column<'_>, shown: &Shown<'_>) {
    column.field(Field::Corners);
    column.field(Field::Points);
    column.field(Field::Inner);
    column.buttons([
        ("Star", !shown.polygon, change(Change::Polygon(false))),
        ("Polygon", shown.polygon, change(Change::Polygon(true))),
    ]);
    column.gap();
    let face = &shown.face;
    column.buttons(
        FAMILIES
            .map(|(label, family)| (label, face.family == family, change(Change::Family(family)))),
    );
    column.field(Field::Size);
    column.buttons([
        ("Bold", face.bold, change(Change::Bold(!face.bold))),
        ("Italic", face.italic, change(Change::Italic(!face.italic))),
    ]);
    column.buttons(
        TEXT_ALIGNS
            .map(|(label, align)| (label, face.align == align, change(Change::TextAlign(align)))),
    );
}

/// The layer commands, then the layers, the topmost first as it is drawn.
fn layer_controls<'s>(
    column: &mut Column<'s>,
    scale: Scale,
    shown: &'s Shown<'s>,
    seen: (i32, i32),
) {
    let layer = |command| change(Change::Layer(command));
    column.buttons([
        ("New", false, layer(LayerCommand::New)),
        ("Remove", false, layer(LayerCommand::Remove)),
        ("Lock", shown.locked, layer(LayerCommand::Lock(shown.layer))),
    ]);
    column.buttons([
        ("Raise", false, layer(LayerCommand::Raise)),
        ("Lower", false, layer(LayerCommand::Lower)),
    ]);
    column.gap();
    column.layers(shown, scale.scale_length(EYE), seen);
}

/// What the inspector shows of the window.
pub struct Shown<'a> {
    /// The style shown.
    pub style: &'a Style,
    /// The layers, bottom first.
    pub layers: &'a tairix_drawing::Children,
    /// The layer in hand.
    pub layer: usize,
    /// Whether the layer in hand is locked.
    pub locked: bool,
    /// Whether new stars are polygons.
    pub polygon: bool,
    /// How text is set.
    pub face: Face<'a>,
}

/// How text is set, as the Shape tab shows it.
pub struct Face<'a> {
    /// The family, by its CSS name.
    pub family: &'a str,
    /// Whether bold.
    pub bold: bool,
    /// Whether italic.
    pub italic: bool,
    /// How its lines align.
    pub align: TextAlign,
}

/// Which of [`DASHES`] `stroke` is dashed with, if one.
fn dash_of(stroke: &tairix_drawing::Stroke) -> Option<usize> {
    let width = if stroke.width > 0.0 {
        stroke.width
    } else {
        1.0
    };
    DASHES.iter().position(|pattern| {
        pattern.len() == stroke.dashes.len()
            && pattern
                .iter()
                .zip(stroke.dashes.iter())
                .all(|(a, b)| tairix_util::mathf::fabs(a * width - b) < 1e-6)
    })
}

/// `text` set in the middle of `rect`, and kept within it.
pub fn text_in(surface: &mut Surface, rect: Rect, text: &str, faces: Faces, colour: Color) {
    let Some((left, top)) = rect.surface_origin() else {
        return;
    };
    let face = faces.label;
    let width = px(face.text_width(text));
    let height = px(face.line_height());
    let x = rect.left() + (px(rect.width) - width).max(0) / 2;
    let y = rect.top() + (px(rect.height) - height) / 2;
    surface.with_clip(left, top, rect.width, rect.height, |surface| {
        face.draw_text(surface, x, y, text, colour);
    });
}

#[cfg(test)]
#[path = "inspector_tests.rs"]
mod tests;
