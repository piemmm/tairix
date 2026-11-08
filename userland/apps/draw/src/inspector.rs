//! The inspector down a window's right: the selection's style, the layers,
//! the selection's place, alignment and arrangement, a shape's and text's own
//! measures, and the page.
//!
//! The inspector holds its controls and answers [`Change`]s; it changes
//! nothing itself. A change arrives live while a control is being moved and
//! once more, settled, when it stops: the view shows the live one and lands
//! the settled one as a step. A tab is laid out as though the panel were
//! tall enough to hold it, and one that is not scrolls, by the wheel or by
//! the bar beside it.

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_bezier::{Bounds, FillRule};
use tairix_colour::{Fraction, Hsv, Hue, Rgba};
use tairix_controls::{
    fill_area, ColourPicker, NumberAction, NumberField, PickerOutcome, ScrollAction, ScrollBar,
    ScrollModel, ScrollOrientation, ScrollRange, ScrollView, SwatchAction, SwatchGrid, SwatchMark,
    WellSpacing,
};
use tairix_drawing::limits::{
    MAX_BASELINE_SHIFT, MAX_RESOLUTION, MAX_STAR_POINTS, MAX_SUBDIVISIONS, MIN_STAR_POINTS,
};
use tairix_drawing::{
    ArrowShape, Cap, Drawing, DrawingError, Gradient, Join, NodePath, Page, Paint, Spread, Stop,
    Style, TextAlign, Unit,
};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_util::fallible;

use crate::edit::Align;
use crate::gradient::{self, Form};
use crate::layout::{px, Faces, Layout};
use crate::stops::Bar;
use tairix_drawing::PAPER_WHITE;

/// A row of controls, in logical pixels.
const ROW: u32 = 26;

/// The margin about the panel's rows, in logical pixels.
const INSET: u32 = 8;

/// The width of a number field's label, in logical pixels.
const LABEL: u32 = 84;

/// The width of a layer's show-or-hide button, in logical pixels.
const EYE: u32 = 64;

/// How many of the page's units share a row of buttons.
const UNITS_PER_ROW: usize = 3;

/// How many wells share a row of the swatch grid: a hue every thirty
/// degrees.
const SWATCH_COLUMNS: usize = 12;

/// The rows of the swatch palette after its greys, as saturation and value
/// in per cent: full colours, tints and shades.
const PALETTE_ROWS: [(u32, u32); 3] = [(100, 100), (40, 100), (100, 55)];

/// The largest angle a field holds, in hundredths of a degree: a half turn
/// either way.
const HALF_TURN: i32 = 18_000;

/// How far Shift and an arrow nudge a gradient's stop: a hundredth of its
/// length.
const STOP_NUDGE: f64 = 0.01;

/// How gradients may continue past their ends, as their buttons read.
const SPREADS: [(&str, Spread); 3] = [
    ("Pad", Spread::Pad),
    ("Reflect", Spread::Reflect),
    ("Repeat", Spread::Repeat),
];

/// The largest measure a field holds, in hundredths: twenty million drawing
/// pixels, near as far as a field's whole number reaches.
const MOST_HUNDREDTHS: i32 = 2_000_000_000;

/// The most a line height is, in hundredths of a percent of the size: a
/// hundred ems, what a text may hold.
const MOST_LINE_HEIGHT: i32 = 1_000_000;

/// How far Outset and Inset move an outline until it is set otherwise, in
/// drawing pixels.
pub(crate) const DEFAULT_OFFSET: f64 = 5.0;

/// How near Simplify keeps a path to itself until it is set otherwise, in
/// drawing pixels.
pub(crate) const DEFAULT_SMOOTHNESS: f64 = 1.0;

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
    /// The page's size, units and ground, and the grid.
    Page,
}

impl Tab {
    const ALL: [Self; 5] = [
        Self::Style,
        Self::Layers,
        Self::Object,
        Self::Shape,
        Self::Page,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Style => "Style",
            Self::Layers => "Layers",
            Self::Object => "Object",
            Self::Shape => "Shape",
            Self::Page => "Page",
        }
    }

    /// The colour picker the tab shows, if it shows one.
    const fn picker(self) -> Option<Picker> {
        match self {
            Self::Style => Some(Picker::Paint),
            Self::Page => Some(Picker::Ground),
            Self::Layers | Self::Object | Self::Shape => None,
        }
    }
}

/// One of the inspector's colour pickers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Picker {
    /// The Style tab's, for the paint in hand.
    Paint,
    /// The Page tab's, for the page's ground.
    Ground,
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

impl Target {
    /// The paint this is of `style`.
    #[must_use]
    pub const fn of(self, style: &Style) -> &Paint {
        match self {
            Self::Fill => &style.fill,
            Self::Stroke => &style.stroke.paint,
        }
    }

    /// The paint this is of `style`, to change.
    pub const fn of_mut(self, style: &mut Style) -> &mut Paint {
        match self {
            Self::Fill => &mut style.fill,
            Self::Stroke => &mut style.stroke.paint,
        }
    }
}

/// Dash patterns offered, in stroke widths.
pub const DASHES: [&[f64]; 4] = [&[], &[4.0, 2.0], &[0.0, 2.0], &[4.0, 2.0, 0.0, 2.0]];

/// The most entries one of [`DASHES`] has.
pub const MOST_DASHES: usize = {
    let mut most = 0;
    let mut at = 0;
    while at < DASHES.len() {
        if DASHES[at].len() > most {
            most = DASHES[at].len();
        }
        at += 1;
    }
    most
};

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
    /// A change to how text is set.
    Text(TextChange),
    /// Outset and Inset move outlines this far, in drawing pixels.
    Offset(f64),
    /// Simplify keeps paths this near themselves, in drawing pixels.
    Smoothness(f64),
    /// A change to the page or the grid.
    Page(PageChange),
    /// Paint the target with a gradient of this form, fitted to each object.
    Form {
        /// Which paint.
        target: Target,
        /// The form.
        form: Form,
    },
    /// Run the target's gradient through `stops`, continued past its ends
    /// by `spread`; an object painted otherwise takes a gradient of `form`.
    Stops {
        /// Which paint.
        target: Target,
        /// The stops, offsets ascending.
        stops: Vec<Stop>,
        /// How it continues past its ends.
        spread: Spread,
        /// The form a gradient made anew takes.
        form: Form,
        /// Whether the change has settled.
        settled: bool,
    },
    /// Turn the selection so it stands this many degrees anticlockwise.
    Rotation(f64),
    /// Keep this colour with the drawing.
    KeepSwatch(Rgba),
    /// Stop keeping the drawing's swatch at this index.
    DropSwatch(usize),
    /// What the inspector could not make, and why.
    Refused {
        /// What it was making.
        what: &'static str,
        /// Why it could not.
        error: DrawingError,
    },
}

/// How the Shape tab asks text to be set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextChange {
    /// In the family of this CSS name.
    Family(&'static str),
    /// This large, in drawing pixels.
    Size(f64),
    /// Bold, or not.
    Bold(bool),
    /// Italic, or not.
    Italic(bool),
    /// Its lines aligned so on its origin.
    Align(TextAlign),
    /// This much extra space after each character, in drawing pixels.
    Spacing(f64),
    /// Raised this far off the baseline, in ems; lowered where negative.
    Raise(f64),
    /// Its lines this far apart, in ems of its size.
    LineHeight(f64),
    /// Its line starting this far along the path it is set along.
    Start(f64),
}

/// What the Page tab asks of the view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageChange {
    /// Make the page this wide, in its unit.
    Width(f64),
    /// Make the page this tall, in its unit.
    Height(f64),
    /// Lay the page out wider than it is tall, or the other way when false.
    Landscape(bool),
    /// Measure the page in this unit, its size unchanged.
    Unit(Unit),
    /// Space the grid's major lines this far apart, in the page's unit.
    GridSpacing(f64),
    /// Cut each side of the grid's major cells into this many.
    Subdivisions(u32),
    /// Make raster copies at this many pixels to the inch.
    Resolution(u32),
    /// Fill the page with this ground, or with none, live or settled.
    Ground {
        /// The ground.
        colour: Option<Rgba>,
        /// Whether the change has settled.
        settled: bool,
    },
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
    /// Continue the gradient past its ends so.
    Spread(Spread),
    /// Run the gradient the other way.
    Reverse,
    /// Remove the stop in hand.
    RemoveStop,
    /// Keep the colour in hand with the drawing.
    KeepSwatch,
    /// Stop keeping the swatch in hand.
    DropSwatch,
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
    Spacing,
    Raise,
    LineHeight,
    Start,
    Offset,
    Smoothness,
    PageWidth,
    PageHeight,
    GridSpacing,
    Subdivisions,
    StopAt,
    Angle,
    Rotation,
    Resolution,
}

/// What holds the inspector's keyboard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Focus {
    /// A number field.
    Field(Field),
    /// The colour picker of the tab shown, on whichever of its parts it
    /// last had.
    Picker,
    /// The gradient's stop bar.
    Stops,
}

impl Field {
    const fn label(self) -> &'static str {
        match self {
            Self::Width => "Stroke width",
            Self::Opacity => "Opacity %",
            Self::Place(0) => "X",
            Self::Place(1) => "Y",
            Self::Place(2) | Self::PageWidth => "Width",
            Self::Place(_) | Self::PageHeight => "Height",
            Self::Corners => "Corners",
            Self::Points => "Points",
            Self::Inner => "Inner %",
            Self::Size => "Text size",
            Self::Spacing => "Letter spacing",
            Self::Raise => "Raise %",
            Self::LineHeight => "Line height %",
            Self::Start => "Along path",
            Self::Offset => "Offset by",
            Self::Smoothness => "Smoothness",
            Self::GridSpacing => "Grid spacing",
            Self::Subdivisions => "Subdivisions",
            Self::StopAt => "Stop at %",
            Self::Angle => "Angle \u{b0}",
            Self::Rotation => "Rotation \u{b0}",
            Self::Resolution => "Copy dpi",
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
    stops: Option<Rect>,
    swatches: Option<Rect>,
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
            stops: None,
            swatches: None,
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
    fn buttons<I>(&mut self, buttons: I)
    where
        I: IntoIterator<Item = (&'s str, bool, Press)>,
        I::IntoIter: ExactSizeIterator,
    {
        let buttons = buttons.into_iter();
        let width = self.width / u32::try_from(buttons.len()).unwrap_or(1).max(1);
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

    /// The gradient's stop bar, a row and a half tall.
    fn stop_bar(&mut self) {
        let rect = Rect::new(self.left, self.top, self.width, self.row * 3 / 2);
        self.stops = Some(rect);
        self.advance(px(rect.height));
        self.gap();
    }

    /// The swatch grid, as tall as `height` answers for the row's width.
    fn swatch_grid(&mut self, height: impl FnOnce(u32) -> u32) {
        let rect = Rect::new(self.left, self.top, self.width, height(self.width));
        self.swatches = Some(rect);
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

/// The swatches every drawing is offered: a row of greys from black to
/// white, then a hue every thirty degrees in full colour, tint and shade.
fn palette() -> Option<Vec<Rgba>> {
    let mut colours = Vec::new();
    if !fallible::reserve(&mut colours, (1 + PALETTE_ROWS.len()) * SWATCH_COLUMNS) {
        return None;
    }
    let last = SWATCH_COLUMNS - 1;
    for step in 0..SWATCH_COLUMNS {
        let level = u8::try_from(step * usize::from(u8::MAX) / last).unwrap_or(u8::MAX);
        colours.push(Rgba::new(level, level, level, u8::MAX));
    }
    for (saturation, value) in PALETTE_ROWS {
        for step in 0..SWATCH_COLUMNS {
            let degrees = u32::try_from(step * 360 / SWATCH_COLUMNS).unwrap_or(0);
            let hsv = Hsv::new(
                Hue::from_degrees(degrees),
                Fraction::from_percent(saturation),
                Fraction::from_percent(value),
            );
            colours.push(hsv.to_rgb().opaque());
        }
    }
    Some(colours)
}

/// A field holding a length in hundredths, from `least` to `most`, stepping
/// a whole unit at a time.
pub(crate) fn measure(value: i32, least: i32, most: i32) -> NumberField {
    NumberField::new(value, least, most)
        .with_decimals(2)
        .with_steps(100, 1000)
}

/// A number field's value, in hundredths.
pub(crate) fn hundredths(value: f64) -> i32 {
    let scaled = tairix_util::mathf::round(value * 100.0);
    tairix_util::mathf::round_i32(scaled)
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
    spacing: NumberField,
    raise: NumberField,
    line_height: NumberField,
    start: NumberField,
    offset: NumberField,
    smoothness: NumberField,
    page: [NumberField; 2],
    grid: NumberField,
    subdivisions: NumberField,
    ground: ColourPicker,
    stop_at: NumberField,
    angle: NumberField,
    rotation: NumberField,
    resolution: NumberField,
    /// The gradient the target is painted with, where it is one: the first
    /// selected object's, or the one new objects are drawn in.
    ramp: Option<Arc<Gradient>>,
    /// The stop of `ramp` in hand.
    stop: usize,
    /// Whether the stop in hand is being dragged along the bar.
    dragging_stop: bool,
    swatches: SwatchGrid,
    /// The swatches every drawing is offered, the grid's first wells.
    palette: Vec<Rgba>,
    /// The drawing's own swatches the grid shows after the palette, or
    /// nothing while the grid could not be brought up to date.
    kept: Option<Vec<Rgba>>,
    to_page: bool,
    /// What holds the keyboard, if anything does.
    focus: Option<Focus>,
    /// The picker showing a colour live that has not settled, which the
    /// drawing's own is not shown over.
    unsettled: Option<Picker>,
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
        let empty = ScrollModel::in_pixels(ScrollRange::EMPTY, 1);
        let palette = palette().unwrap_or_default();
        let wells = fallible::collected(palette.len(), palette.iter().map(|&c| Color::from(c)))
            .unwrap_or_default();
        let turn = || {
            NumberField::new(0, -HALF_TURN, HALF_TURN)
                .with_decimals(2)
                .with_steps(100, 1500)
        };
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
            points: NumberField::new(5, to_i32(MIN_STAR_POINTS), to_i32(MAX_STAR_POINTS)),
            inner: NumberField::new(50, 1, 100),
            size: measure(2400, 100, MOST_HUNDREDTHS),
            spacing: measure(0, -MOST_HUNDREDTHS, MOST_HUNDREDTHS),
            raise: {
                // What a run may raise its characters by, in hundredths of a
                // percent of its size.
                let most = hundredths(MAX_BASELINE_SHIFT * 100.0);
                measure(0, -most, most)
            },
            line_height: measure(
                hundredths(tairix_drawing::DEFAULT_LINE_HEIGHT * 100.0),
                0,
                MOST_LINE_HEIGHT,
            ),
            start: measure(0, -MOST_HUNDREDTHS, MOST_HUNDREDTHS),
            offset: measure(hundredths(DEFAULT_OFFSET), 1, MOST_HUNDREDTHS),
            smoothness: measure(hundredths(DEFAULT_SMOOTHNESS), 1, MOST_HUNDREDTHS),
            page: core::array::from_fn(|_| measure(1, 1, MOST_HUNDREDTHS)),
            grid: measure(1, 1, MOST_HUNDREDTHS),
            subdivisions: NumberField::new(1, 1, to_i32(MAX_SUBDIVISIONS)),
            ground: ColourPicker::new(PAPER_WHITE).with_opacity(true),
            stop_at: measure(0, 0, 10_000),
            angle: turn(),
            rotation: turn(),
            resolution: NumberField::new(
                to_i32(tairix_drawing::PIXELS_PER_INCH),
                1,
                to_i32(MAX_RESOLUTION),
            ),
            ramp: None,
            stop: 0,
            dragging_stop: false,
            swatches: SwatchGrid::new(SWATCH_COLUMNS, wells).with_spacing(WellSpacing::Packed),
            palette,
            kept: Some(Vec::new()),
            to_page: false,
            focus: None,
            unsettled: None,
            pointer: Point::ORIGIN,
            bar: ScrollBar::new(ScrollOrientation::Vertical, empty).without_edges(),
        }
    }

    /// Show what is selected in `drawing`, its bounds `bounds` — or `style`,
    /// what new objects are drawn in, when nothing is; of a text being typed,
    /// how the character `typed` begins with is set. A field being typed
    /// into, and a colour not yet settled, are left as they are.
    pub fn follow(
        &mut self,
        drawing: &Drawing,
        selection: &[NodePath],
        style: &Style,
        (bounds, typed): (Bounds, Option<Range<usize>>),
    ) {
        let node = selection.first().and_then(|path| drawing.node_at(path));
        let own = node
            .and_then(|node| node.style())
            .map_or(style, |style| &**style);
        let painted = typed_style(own, node.map(Arc::as_ref), typed.clone());
        let style = &*painted;
        self.ramp = match self.target.of(style) {
            Paint::Gradient(gradient) => Some(gradient.clone()),
            Paint::None | Paint::Colour(_) => None,
        };
        let in_hand = self.follow_ramp().or(match self.target.of(style) {
            Paint::Colour(colour) => Some(*colour),
            Paint::None | Paint::Gradient(_) => None,
        });
        if let (Some(colour), true) = (in_hand, self.unsettled != Some(Picker::Paint)) {
            self.picker.set_colour(colour);
        }
        self.follow_swatches(drawing.swatches(), in_hand);
        self.show(
            Field::Rotation,
            hundredths(crate::edit::angle_of(drawing, selection)),
        );
        self.follow_page(drawing);
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
                let face = text.face(typed.and_then(|range| text.run_for(range)));
                self.show(Field::Size, hundredths(face.size));
                self.show(Field::Spacing, hundredths(face.spacing));
                self.show(Field::Raise, hundredths(face.rise / face.size * 100.0));
                match &text.baseline {
                    tairix_drawing::Baseline::Lines { line_height, .. } => {
                        self.show(Field::LineHeight, hundredths(line_height * 100.0));
                    }
                    tairix_drawing::Baseline::Path(along) => {
                        self.show(Field::Start, hundredths(along.start));
                    }
                }
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

    /// Show the gradient in hand's stop in hand, kept among its stops, and
    /// its angle; answers the stop's colour.
    fn follow_ramp(&mut self) -> Option<Rgba> {
        let ramp = self.ramp.clone()?;
        self.stop = self.stop.min(ramp.stops.len().saturating_sub(1));
        let stop = *ramp.stops.get(self.stop)?;
        self.show(Field::StopAt, hundredths(stop.offset * 100.0));
        if let Form::Linear(angle) = gradient::form_of(&ramp) {
            self.show(Field::Angle, hundredths(angle));
        }
        Some(stop.colour)
    }

    /// Show the palette and then `kept`, the drawing's swatches, in the
    /// grid, the well holding `in_hand` marked.
    fn follow_swatches(&mut self, kept: &[Rgba], in_hand: Option<Rgba>) {
        if self.kept.as_deref() != Some(kept) {
            self.kept = None;
            let count = self.palette.len() + kept.len();
            let wells = fallible::collected(
                count,
                self.palette
                    .iter()
                    .chain(kept)
                    .map(|&colour| Color::from(colour)),
            );
            if let (Some(wells), Some(copy)) = (wells, fallible::copied(kept)) {
                self.swatches.adopt_colours(SWATCH_COLUMNS, wells);
                self.kept = Some(copy);
            }
        }
        let holds = |at: usize, colour: Rgba| self.swatches.colour(at) == Some(Color::from(colour));
        // The well chosen keeps the mark while it holds the colour in hand,
        // though an earlier well holds the same colour.
        let marked = in_hand.and_then(|colour| {
            let chosen = self.swatches.selected().filter(|&at| holds(at, colour));
            chosen.or_else(|| (0..self.swatches.len()).find(|&at| holds(at, colour)))
        });
        self.swatches.adopt_selected(marked);
    }

    /// Show `drawing`'s page and grid in the Page tab.
    fn follow_page(&mut self, drawing: &Drawing) {
        let page = drawing.page();
        let grid = drawing.grid();
        self.show(Field::PageWidth, hundredths(page.width));
        self.show(Field::PageHeight, hundredths(page.height));
        self.show(
            Field::GridSpacing,
            hundredths(grid.spacing / page.unit.pixels()),
        );
        self.show(Field::Subdivisions, to_i32(grid.subdivisions));
        self.show(Field::Resolution, to_i32(drawing.resolution()));
        if let (Some(ground), false) = (page.background, self.unsettled == Some(Picker::Ground)) {
            self.ground.set_colour(ground);
        }
    }

    /// The colour the Page tab's picker holds: the ground a page is given.
    #[must_use]
    pub fn ground_colour(&self) -> Rgba {
        self.ground.colour()
    }

    /// Show `tab`, scrolled to its top. What was under way in the tab shown
    /// has been landed, so nothing is let go of but the keyboard.
    pub fn show_tab(&mut self, tab: Tab) {
        if tab == self.tab {
            return;
        }
        if let Some(Focus::Field(field)) = self.focus.take() {
            self.field(field).set_focused(false);
        }
        self.unfocus_pickers();
        self.tab = tab;
        self.bar.set_model(self.bar.model().scroll_to(0));
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

    /// Which paint the Style tab edits.
    #[must_use]
    pub const fn target(&self) -> Target {
        self.target
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
        self.picker.is_dragging()
            || self.ground.is_dragging()
            || self.dragging_stop
            || self.bar.is_pressing()
    }

    /// Whether the pointer was last seen over the inspector, so the move
    /// that takes it away is the inspector's too.
    #[must_use]
    pub fn was_over(&self, layout: &Layout) -> bool {
        layout.panel.contains(self.pointer) || layout.tabs.contains(self.pointer)
    }

    /// The tab strip's buttons.
    fn tab_rects(layout: &Layout) -> [(Tab, Rect); Tab::ALL.len()] {
        let tabs = layout.tabs;
        let width = tabs.width / u32::try_from(Tab::ALL.len()).unwrap_or(1);
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
            Tab::Style => self.style_controls(&mut column, scale, theme, &shown.style),
            Tab::Layers => layer_controls(&mut column, scale, shown, seen),
            Tab::Object => self.object_controls(&mut column),
            Tab::Shape => shape_controls(&mut column, shown),
            Tab::Page => self.page_controls(&mut column, scale, theme, &shown.page),
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
        let target = self.target;
        let paint = target.of(style);
        let form = match paint {
            Paint::Gradient(gradient) => Some(gradient::form_of(gradient)),
            Paint::None | Paint::Colour(_) => None,
        };
        let solid = |paint| {
            change(Change::Paint {
                target,
                paint,
                settled: true,
            })
        };
        let shaped = |form| change(Change::Form { target, form });
        let angle = match form {
            Some(Form::Linear(angle)) => angle,
            Some(Form::Radial) | None => 0.0,
        };
        column.buttons([
            ("None", matches!(paint, Paint::None), solid(Paint::None)),
            (
                "Colour",
                matches!(paint, Paint::Colour(_)),
                solid(Paint::Colour(self.picker.colour())),
            ),
            (
                "Linear",
                matches!(form, Some(Form::Linear(_))),
                shaped(Form::Linear(angle)),
            ),
            ("Radial", form == Some(Form::Radial), shaped(Form::Radial)),
        ]);
        if let (Paint::Gradient(ramp), Some(form)) = (paint, form) {
            gradient_controls(column, ramp, form);
        }
        column.picker(|width| self.picker.measured_height(width, scale, theme));
        column.swatch_grid(|width| self.swatches.height_for_width(width));
        column.buttons([
            ("Add swatch", false, Press::KeepSwatch),
            ("Remove swatch", false, Press::DropSwatch),
        ]);
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
        column.field(Field::Rotation);
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

    fn page_controls(&self, column: &mut Column<'_>, scale: Scale, theme: &Theme, page: &Page) {
        column.field(Field::PageWidth);
        column.field(Field::PageHeight);
        let landscape = page.width > page.height;
        let paged = |change| Press::Change(Change::Page(change));
        column.buttons([
            ("Portrait", !landscape, paged(PageChange::Landscape(false))),
            ("Landscape", landscape, paged(PageChange::Landscape(true))),
        ]);
        for row in Unit::ALL.chunks(UNITS_PER_ROW) {
            column.buttons(row.iter().map(|&unit| {
                (
                    unit.symbol(),
                    page.unit == unit,
                    paged(PageChange::Unit(unit)),
                )
            }));
        }
        column.gap();
        column.field(Field::GridSpacing);
        column.field(Field::Subdivisions);
        column.field(Field::Resolution);
        column.gap();
        let ground = |colour| {
            paged(PageChange::Ground {
                colour,
                settled: true,
            })
        };
        column.buttons([
            ("None", page.background.is_none(), ground(None)),
            (
                "Colour",
                page.background.is_some(),
                ground(Some(self.ground.colour())),
            ),
        ]);
        column.picker(|width| self.ground.measured_height(width, scale, theme));
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
            Field::Spacing => &mut self.spacing,
            Field::Raise => &mut self.raise,
            Field::LineHeight => &mut self.line_height,
            Field::Start => &mut self.start,
            Field::Offset => &mut self.offset,
            Field::Smoothness => &mut self.smoothness,
            Field::PageWidth => &mut self.page[0],
            Field::PageHeight => &mut self.page[1],
            Field::GridSpacing => &mut self.grid,
            Field::Subdivisions => &mut self.subdivisions,
            Field::StopAt => &mut self.stop_at,
            Field::Angle => &mut self.angle,
            Field::Rotation => &mut self.rotation,
            Field::Resolution => &mut self.resolution,
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
            Field::Spacing => &self.spacing,
            Field::Raise => &self.raise,
            Field::LineHeight => &self.line_height,
            Field::Start => &self.start,
            Field::Offset => &self.offset,
            Field::Smoothness => &self.smoothness,
            Field::PageWidth => &self.page[0],
            Field::PageHeight => &self.page[1],
            Field::GridSpacing => &self.grid,
            Field::Subdivisions => &self.subdivisions,
            Field::StopAt => &self.stop_at,
            Field::Angle => &self.angle,
            Field::Rotation => &self.rotation,
            Field::Resolution => &self.resolution,
        }
    }

    /// The colour picker `which` is.
    fn picker_mut(&mut self, which: Picker) -> &mut ColourPicker {
        match which {
            Picker::Paint => &mut self.picker,
            Picker::Ground => &mut self.ground,
        }
    }

    fn unfocus_pickers(&mut self) {
        self.picker.set_focused(false);
        self.ground.set_focused(false);
    }

    fn picker_shown(&self, which: Picker) -> &ColourPicker {
        match which {
            Picker::Paint => &self.picker,
            Picker::Ground => &self.ground,
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
        let laid_picker = self.tab.picker().zip(placed.column.picker);
        if let Some((which, picker)) =
            laid_picker.filter(|(which, _)| self.picker_shown(*which).is_dragging())
        {
            // A drag holds the pointer wherever it goes until it lets go.
            let held = placed
                .view
                .confined_to(layout.window)
                .event_in_layout(event);
            let answer = self.picker_answer(which, &held, picker, (scale, theme), &mut moved);
            placed.view.report(&moved, damage);
            return Changes::of(answer);
        }
        if self.dragging_stop {
            return self.drag_stop(event, layout, placed.column.stops, damage);
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
        if let Some(changes) = self.wells_and_stops(
            &placed,
            (layout, scale, theme),
            (event, &mapped),
            local,
            damage,
        ) {
            return changes;
        }
        if let InputEvent::PointerScrolled { dx, dy } = *event {
            return self.wheel(
                &placed,
                (layout, scale, theme),
                (&mapped, dx, dy),
                local,
                damage,
            );
        }
        if let Some((which, picker)) = laid_picker.filter(|(_, rect)| inside(*rect)) {
            let mut released = None;
            if primary_press(event) && self.focus != Some(Focus::Picker) {
                released = self.let_go(&placed, (layout, scale, theme), damage);
                self.focus = Some(Focus::Picker);
                self.picker_mut(which).set_focused(true);
                damage.add(layout.panel);
            }
            let change = self.picker_answer(which, &mapped, picker, (scale, theme), &mut moved);
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
            let change = action.and_then(|action| self.field_change(field, action));
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

    /// A wheel turned by `dx` and `dy` over the panel, the pointer at `local`
    /// in the column's layout: the field holding the keyboard steps when it
    /// is under the pointer, and the panel scrolls otherwise.
    fn wheel(
        &mut self,
        placed: &Placed<'_>,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        (mapped, dx, dy): (&InputEvent, i32, i32),
        local: Option<Point>,
        damage: &mut Region,
    ) -> Changes {
        let focused = match self.focus {
            Some(Focus::Field(field)) => placed.field_rect(field).map(|rect| (field, rect)),
            _ => None,
        };
        let under = |rect: Rect| local.is_some_and(|point| rect.contains(point));
        if let Some((field, rect)) = focused.filter(|(_, rect)| under(*rect)) {
            let mut moved = Region::new();
            let action = self
                .field(field)
                .on_pointer(mapped, rect, scale, theme, &mut moved);
            placed.view.report(&moved, damage);
            return Changes::of(action.and_then(|action| self.field_change(field, action)));
        }
        if let Some(bar) = placed.bar.filter(|_| layout.panel.contains(self.pointer)) {
            if let Some(ScrollAction::ScrollTo { offset }) =
                self.bar.wheel(dx, dy, scale, bar, damage)
            {
                self.scroll(placed.model, offset, layout, damage);
            }
        }
        Changes::NONE
    }

    /// What the swatch grid and the stop bar make of `event`, `mapped` into
    /// the column's layout, the pointer at `local` there: the grid sees every
    /// event, so a press it took and a release elsewhere never leave it
    /// armed, and a press on the stop bar is the bar's.
    fn wells_and_stops(
        &mut self,
        placed: &Placed<'_>,
        context: (&Layout, Scale, &Theme),
        (event, mapped): (&InputEvent, &InputEvent),
        local: Option<Point>,
        damage: &mut Region,
    ) -> Option<Changes> {
        if let Some(wells) = placed.column.swatches {
            let mut moved = Region::new();
            let action = self
                .swatches
                .on_pointer(mapped, wells, SwatchMark::Primary, &mut moved);
            placed.view.report(&moved, damage);
            if let Some(action) = action {
                return Some(Changes::of(self.swatch_chosen(action)));
            }
        }
        let (bar, point) = placed
            .column
            .stops
            .zip(local)
            .filter(|(bar, point)| primary_press(event) && bar.contains(*point))?;
        Some(self.press_stops(placed, context, bar, point, damage))
    }

    /// The stop being dragged follows the pointer wherever it goes and lands
    /// where it is let go; `bar` is where the stop bar is laid, if it still
    /// is.
    fn drag_stop(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        bar: Option<Rect>,
        damage: &mut Region,
    ) -> Changes {
        let settled = matches!(
            event,
            InputEvent::PointerReleased {
                button: PointerButton::Primary
            } | InputEvent::PointerCancelled
        );
        if settled {
            self.dragging_stop = false;
        } else if !matches!(event, InputEvent::PointerMoved { .. }) {
            return Changes::NONE;
        }
        let Some(bar) = bar else {
            self.dragging_stop = false;
            return Changes::NONE;
        };
        damage.add(layout.panel);
        let (stop, offset) = (self.stop, Bar::of(bar).offset_at(self.pointer.x));
        Changes::of(self.restop("Move stop", settled, None, |stops| {
            gradient::moved(stops, stop, offset).map(Some)
        }))
    }

    /// A press on the stop bar laid at `bar`, at `point` in the column's
    /// layout: on a handle it takes that stop in hand and starts its drag, on
    /// the strip it adds a stop there.
    fn press_stops(
        &mut self,
        placed: &Placed<'_>,
        (layout, scale, theme): (&Layout, Scale, &Theme),
        bar: Rect,
        point: Point,
        damage: &mut Region,
    ) -> Changes {
        let mut released = None;
        if self.focus != Some(Focus::Stops) {
            released = self.let_go(placed, (layout, scale, theme), damage);
            self.focus = Some(Focus::Stops);
        }
        damage.add(layout.panel);
        let Some(ramp) = self.ramp.clone() else {
            return Changes {
                released,
                change: None,
            };
        };
        let laid = Bar::of(bar);
        if let Some(index) = laid.handle_at(&ramp.stops, self.stop, point) {
            self.choose_stop(index);
            self.dragging_stop = true;
            return Changes {
                released,
                change: None,
            };
        }
        if !laid.on_strip(point) {
            return Changes {
                released,
                change: None,
            };
        }
        let change = match gradient::added(&ramp.stops, laid.offset_at(point.x)) {
            Ok((stops, at)) => {
                self.stop = at;
                Some(Change::Stops {
                    target: self.target,
                    stops,
                    spread: ramp.spread,
                    form: gradient::form_of(&ramp),
                    settled: true,
                })
            }
            Err(error) => Some(Change::Refused {
                what: "Add stop",
                error,
            }),
        };
        Changes { released, change }
    }

    /// Take the gradient in hand's stop at `index` in hand, showing its
    /// colour and place.
    fn choose_stop(&mut self, index: usize) {
        let Some(stop) = self
            .ramp
            .as_ref()
            .and_then(|ramp| ramp.stops.get(index))
            .copied()
        else {
            return;
        };
        self.stop = index;
        self.picker.set_colour(stop.colour);
        self.show(Field::StopAt, hundredths(stop.offset * 100.0));
    }

    /// What choosing a swatch asks: the colour in hand, or the stop in
    /// hand's, made the well's.
    fn swatch_chosen(&mut self, action: SwatchAction) -> Option<Change> {
        let SwatchAction::Selected { index, .. } = action;
        let Color { r, g, b, a } = self.swatches.colour(index)?;
        let colour = Rgba::new(r, g, b, a);
        self.picker.set_colour(colour);
        if self.ramp.is_some() {
            let stop = self.stop;
            return self.restop("Stop colour", true, None, |stops| {
                gradient::recoloured(stops, stop, colour).map(Some)
            });
        }
        Some(Change::Paint {
            target: self.target,
            paint: Paint::Colour(colour),
            settled: true,
        })
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
                self.stop = 0;
                self.ramp = match target.of(&shown.style) {
                    Paint::Gradient(gradient) => Some(gradient.clone()),
                    Paint::None | Paint::Colour(_) => None,
                };
                let in_hand = self.follow_ramp().or(match target.of(&shown.style) {
                    Paint::Colour(colour) => Some(*colour),
                    Paint::None | Paint::Gradient(_) => None,
                });
                if let Some(colour) = in_hand {
                    self.picker.set_colour(colour);
                }
                None
            }
            Press::ToPage(to_page) => {
                self.to_page = *to_page;
                None
            }
            Press::Change(change) => Some(change.clone()),
            Press::Spread(spread) => self.restop("Spread", true, Some(*spread), |stops| {
                fallible::copied(stops)
                    .ok_or(DrawingError::OutOfMemory)
                    .map(Some)
            }),
            Press::Reverse => self.restop("Reverse gradient", true, None, |stops| {
                gradient::reversed(stops).map(Some)
            }),
            Press::RemoveStop => {
                let stop = self.stop;
                self.restop("Remove stop", true, None, |stops| {
                    gradient::removed(stops, stop)
                })
            }
            Press::KeepSwatch => Some(Change::KeepSwatch(self.picker.colour())),
            Press::DropSwatch => {
                let kept = self.kept.as_ref()?;
                let at = self.swatches.selected()?.checked_sub(self.palette.len())?;
                (at < kept.len()).then_some(Change::DropSwatch(at))
            }
        }
    }

    /// What running the gradient in hand through the stops `alter` makes of
    /// its own asks of the view, continued by `spread` or as it is; nothing
    /// where there is no gradient in hand or `alter` makes nothing.
    fn restop(
        &self,
        what: &'static str,
        settled: bool,
        spread: Option<Spread>,
        alter: impl FnOnce(&[Stop]) -> Result<Option<Vec<Stop>>, DrawingError>,
    ) -> Option<Change> {
        let ramp = self.ramp.as_ref()?;
        match alter(&ramp.stops) {
            Ok(Some(stops)) => Some(Change::Stops {
                target: self.target,
                stops,
                spread: spread.unwrap_or(ramp.spread),
                form: gradient::form_of(ramp),
                settled,
            }),
            Ok(None) => None,
            Err(error) => Some(Change::Refused { what, error }),
        }
    }

    /// What the picker `which`, laid at `bounds`, answers `event` with.
    fn picker_answer(
        &mut self,
        which: Picker,
        event: &InputEvent,
        bounds: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Change> {
        let outcome = self
            .picker_mut(which)
            .on_pointer(event, bounds, scale, theme, damage);
        self.picker_change(which, outcome)
    }

    /// What the picker `which`'s `outcome` changes, kept track of so a colour
    /// shown live is not shown over until it settles.
    fn picker_change(&mut self, which: Picker, outcome: PickerOutcome) -> Option<Change> {
        let (colour, settled) = match outcome {
            PickerOutcome::Edited(colour) => (colour, false),
            PickerOutcome::Settled(colour) => (colour, true),
            PickerOutcome::Ignored | PickerOutcome::Taken => return None,
        };
        self.unsettled = (!settled).then_some(which);
        match which {
            Picker::Paint if self.ramp.is_some() => {
                let stop = self.stop;
                self.restop("Stop colour", settled, None, |stops| {
                    gradient::recoloured(stops, stop, colour).map(Some)
                })
            }
            Picker::Paint => Some(Change::Paint {
                target: self.target,
                paint: Paint::Colour(colour),
                settled,
            }),
            Picker::Ground => Some(Change::Page(PageChange::Ground {
                colour: Some(colour),
                settled,
            })),
        }
    }

    /// What a number field's action changes.
    fn field_change(&self, field: Field, action: NumberAction) -> Option<Change> {
        let (value, settled) = (action.value(), action.is_settled());
        if !settled && !matches!(field, Field::Width | Field::Opacity) {
            return None;
        }
        let measure = f64::from(value) / 100.0;
        Some(match field {
            Field::Points => Change::Points(u32::try_from(value).unwrap_or(MIN_STAR_POINTS)),
            Field::Corners => Change::Corners(measure),
            Field::Inner => Change::Inner(measure),
            Field::Size => Change::Text(TextChange::Size(measure)),
            Field::Spacing => Change::Text(TextChange::Spacing(measure)),
            Field::Raise => Change::Text(TextChange::Raise(measure / 100.0)),
            Field::LineHeight => Change::Text(TextChange::LineHeight(measure / 100.0)),
            Field::Start => Change::Text(TextChange::Start(measure)),
            Field::Offset => Change::Offset(measure),
            Field::Smoothness => Change::Smoothness(measure),
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
            Field::PageWidth => Change::Page(PageChange::Width(measure)),
            Field::PageHeight => Change::Page(PageChange::Height(measure)),
            Field::GridSpacing => Change::Page(PageChange::GridSpacing(measure)),
            Field::Subdivisions => {
                Change::Page(PageChange::Subdivisions(u32::try_from(value).unwrap_or(1)))
            }
            Field::StopAt => {
                let stop = self.stop;
                return self.restop("Move stop", true, None, |stops| {
                    gradient::moved(stops, stop, measure / 100.0).map(Some)
                });
            }
            Field::Angle => Change::Form {
                target: self.target,
                form: Form::Linear(measure),
            },
            Field::Rotation => Change::Rotation(measure),
            Field::Resolution => Change::Page(PageChange::Resolution(
                u32::try_from(value).unwrap_or(tairix_drawing::PIXELS_PER_INCH),
            )),
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
                let Some((which, rect)) = self.tab.picker().zip(placed.column.picker) else {
                    self.focus = None;
                    self.unfocus_pickers();
                    return Changes::NONE;
                };
                let outcome =
                    self.picker_mut(which)
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
                    outcome => Changes::of(self.picker_change(which, outcome)),
                }
            }
            Focus::Field(_) | Focus::Stops if tab => {
                self.walk(&placed, !modifiers.shift, (layout, scale, theme), damage)
            }
            Focus::Field(field) => {
                let Some(rect) = placed.field_rect(field) else {
                    return Changes::NONE;
                };
                let action = self.field(field).on_key(key, modifiers, rect, &mut moved);
                placed.view.report(&moved, damage);
                Changes::of(action.and_then(|action| self.field_change(field, action)))
            }
            Focus::Stops if key == Key::Named(NamedKey::Escape) => Changes {
                released: self.let_go(&placed, (layout, scale, theme), damage),
                change: None,
            },
            Focus::Stops => {
                damage.add(layout.panel);
                Changes::of(self.stops_key(key, modifiers))
            }
        }
    }

    /// What `key` does on the stop bar: Left and Right take the stop before
    /// or after in hand, or with Shift nudge the one in hand, and Delete or
    /// Backspace removes it.
    fn stops_key(&mut self, key: Key, modifiers: Modifiers) -> Option<Change> {
        let count = self.ramp.as_ref()?.stops.len();
        let (towards, nudge) = match key {
            Key::Named(NamedKey::Left) => (-1, -STOP_NUDGE),
            Key::Named(NamedKey::Right) => (1, STOP_NUDGE),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                let stop = self.stop;
                return self.restop("Remove stop", true, None, |stops| {
                    gradient::removed(stops, stop)
                });
            }
            _ => return None,
        };
        let stop = self.stop;
        if modifiers.shift {
            let offset = self.ramp.as_ref()?.stops.get(stop)?.offset + nudge;
            return self.restop("Move stop", true, None, |stops| {
                gradient::moved(stops, stop, offset).map(Some)
            });
        }
        let next = stop.checked_add_signed(towards).filter(|at| *at < count)?;
        self.choose_stop(next);
        None
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
        let (bar, picker) = (placed.column.stops, placed.column.picker);
        let picker_at = usize::from(bar.is_some());
        let first_field = picker_at + usize::from(picker.is_some());
        let count = fields.len() + first_field;
        let at = match from {
            Some(Focus::Field(field)) => fields
                .iter()
                .position(|(each, _)| *each == field)
                .map_or(0, |at| at + first_field),
            Some(Focus::Picker) => picker_at,
            Some(Focus::Stops) | None => 0,
        };
        let next = if forward {
            (at + 1) % count.max(1)
        } else {
            (at + count.max(1) - 1) % count.max(1)
        };
        let stop = match next.checked_sub(first_field) {
            None if next < picker_at => bar.inspect(|_| self.focus = Some(Focus::Stops)),
            None => self.tab.picker().zip(picker).map(|(which, rect)| {
                self.focus = Some(Focus::Picker);
                self.picker_mut(which)
                    .enter_focus(forward, rect, scale, theme);
                rect
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
            Focus::Stops => {
                damage.add(layout.panel);
                self.settle_stop_drag()
            }
            Focus::Picker => {
                damage.add(layout.panel);
                let Some(which) = self.tab.picker() else {
                    self.unfocus_pickers();
                    return None;
                };
                let dragged = self.picker_mut(which).finish_drag();
                let Some(rect) = placed.column.picker else {
                    self.picker_mut(which).set_focused(false);
                    return self.picker_change(which, dragged);
                };
                let mut moved = Region::new();
                let blurred = self.picker_mut(which).blur(rect, scale, theme, &mut moved);
                placed.view.report(&moved, damage);
                let settled = self.picker_change(which, blurred);
                self.picker_change(which, dragged).or(settled)
            }
        }
    }

    /// The stop being dragged landed where it stands.
    fn settle_stop_drag(&mut self) -> Option<Change> {
        if !core::mem::take(&mut self.dragging_stop) {
            return None;
        }
        self.restop("Move stop", true, None, |stops| {
            fallible::copied(stops)
                .ok_or(DrawingError::OutOfMemory)
                .map(Some)
        })
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
            Focus::Stops => {
                damage.add(layout.panel);
                self.settle_stop_drag()
            }
            Focus::Picker => {
                damage.add(layout.panel);
                let Some(which) = self.tab.picker() else {
                    self.focus = None;
                    self.unfocus_pickers();
                    return None;
                };
                let picker = self.picker_mut(which);
                let outcome = picker.finish_drag();
                picker.set_colour(picker.colour());
                let change = self.picker_change(which, outcome);
                self.unsettled = None;
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
        committed.and_then(|action| self.field_change(field, action))
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
            if let Some((which, picker)) = self.tab.picker().zip(placed.column.picker) {
                self.picker_shown(which)
                    .render(surface, picker, scale, theme);
            }
            if let (Some(bar), Paint::Gradient(ramp)) =
                (placed.column.stops, self.target.of(&shown.style))
            {
                let focused = self.focus == Some(Focus::Stops);
                Bar::of(bar).render(surface, (&ramp.stops, self.stop, focused), scale, theme);
            }
            if let Some(wells) = placed.column.swatches {
                self.swatches.render(surface, wells, scale, theme);
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

/// The rows a gradient of `form` is edited by: its stop bar, the stop in
/// hand's place, a linear one's angle, how it continues past its ends, and
/// reversing it or removing the stop in hand.
fn gradient_controls(column: &mut Column<'_>, ramp: &Gradient, form: Form) {
    column.stop_bar();
    column.field(Field::StopAt);
    if matches!(form, Form::Linear(_)) {
        column.field(Field::Angle);
    }
    column.buttons(
        SPREADS.map(|(label, spread)| (label, ramp.spread == spread, Press::Spread(spread))),
    );
    column.buttons([
        ("Reverse", false, Press::Reverse),
        ("Remove stop", false, Press::RemoveStop),
    ]);
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
    column.buttons(FAMILIES.map(|(label, family)| {
        (
            label,
            face.family == family,
            change(Change::Text(TextChange::Family(family))),
        )
    }));
    column.field(Field::Size);
    column.field(Field::Spacing);
    column.field(Field::Raise);
    column.field(if face.along {
        Field::Start
    } else {
        Field::LineHeight
    });
    column.buttons([
        (
            "Bold",
            face.bold,
            change(Change::Text(TextChange::Bold(!face.bold))),
        ),
        (
            "Italic",
            face.italic,
            change(Change::Text(TextChange::Italic(!face.italic))),
        ),
    ]);
    column.buttons(TEXT_ALIGNS.map(|(label, align)| {
        (
            label,
            face.align == align,
            change(Change::Text(TextChange::Align(align))),
        )
    }));
    column.gap();
    column.field(Field::Offset);
    column.field(Field::Smoothness);
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
    pub style: Cow<'a, Style>,
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
    /// The page.
    pub page: Page,
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
    /// Whether it is set along a path rather than in lines.
    pub along: bool,
}

/// `own`, the style of the object `node`, as a text being typed paints the
/// character `typed` begins with: with its run's paints in place of its own.
pub(crate) fn typed_style<'a>(
    own: &'a Style,
    node: Option<&tairix_drawing::Node>,
    typed: Option<Range<usize>>,
) -> Cow<'a, Style> {
    let run = node.zip(typed).and_then(|(node, range)| match node.kind() {
        tairix_drawing::Kind::Text { text, .. } => text
            .run_for(range)
            .and_then(|index| text.runs.get(index as usize))
            .map(|run| Arc::clone(&run.style)),
        _ => None,
    });
    match run {
        Some(run) if run.fill.is_some() || run.stroke.is_some() => Cow::Owned(run.painted(own)),
        _ => Cow::Borrowed(own),
    }
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
