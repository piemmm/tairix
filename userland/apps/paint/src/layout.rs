//! The painter window's geometry: the one function every painter and every
//! hit-test agrees on.
//!
//! ```text
//! +----------------------------------------------------------------------+
//! | Brush  Size [ 4 ] px  [x] Smooth edges                 [-][+][F][1]|#|
//! +-------+----------------------------------------+---+---------------+
//! |x Tools_|                                        | ^ |x Colour______-|
//! | S  P  |               the picture              | | | [#][#] which  |
//! |=B  A  |                                        |   | [plane  ]|H|  |
//! | E  C  |                                        |   | H S V  R G B  |
//! | :  :  +----------------------------------------+---+               |
//! |       |========================================|   |x Layers______-|
//! |       +--------------------------------------------+ :             |
//! |       | [][][][][][][][][][][][][][][][][]     [^] |               |
//! +-------+--------------------------------------------+---------------+
//! | (12, 34) #ff00aa   640 × 480, 256 colours         1 of 3: sprite  100% |
//! +----------------------------------------------------------------------+
//! ```
//!
//! The tool-controls bar runs across the top, the view's commands at its
//! end; a dock runs down each side holding the panes the arrangement puts
//! there, each a mini title band above its body; the palette strip lies
//! beneath the canvas and its bars, the control that floats it at its end.
//!
//! A dock seats every band first, then each body at its least, then grows the
//! bodies towards what shows them whole, the pane opened last first. A body
//! given less than that scrolls: its own parts, or, laid out whole, by a bar
//! the dock gives it down its right. The dock itself never scrolls.
//!
//! Every extent comes from the theme's metrics at the desktop scale and the
//! faces' own measures. Bands are claimed from the edges inward, so however
//! small the window only the canvas gives up room; a region with no room is
//! an empty rectangle, which every painter and hit-test treats as absent.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::WindowSizing;
use tairix_controls::{
    NumberFace, ScrollBar, ScrollOrientation, StatusBand, TitleBar, TitleBarCommands,
};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::SpriteName;
use tairix_inline::ArrayVec;
use tairix_theme::{TextRole, Theme};

use crate::canvas::MAX_SIDE;
use crate::document::MAX_ENTRIES;
use crate::layers_pane::{self, LayerMeasures};
use crate::pane::{Arrangement, Docked, PaneKind, Side};
use crate::render::{write_position, write_sprite, write_zoom};
use crate::tool_controls::Placement;
use crate::viewport::{Zoom, ZOOMS};

/// The client area a new window opens at, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (900, 640);

/// The colour dock's width, in logical pixels: room for the picker with its
/// fields stacked beneath its plane.
const DOCK_WIDTH: u32 = 232;

/// The current-colour wells' band, in logical pixels.
const WELLS_HEIGHT: u32 = 52;

/// The least canvas the smallest window keeps, in logical pixels.
const MIN_CANVAS: u32 = 96;

/// The least a palette well is across, in logical pixels: what a palette too
/// long for its strip is set at, scrolled along it.
const LEAST_WELL: u32 = 14;

/// The most a palette well is across, in logical pixels: what a short
/// palette is set at in its strip, and every well of a floating one.
const MOST_WELL: u32 = 24;

/// The fewest columns a floating palette sets its wells in.
const LEAST_COLUMNS: usize = 4;

/// What the layout's text is set in.
#[derive(Copy, Clone, Debug)]
pub struct Faces {
    /// Captions: the wells', a panel's, the layers'.
    pub caption: BitmapFont,
    /// A tool setting's label and unit.
    pub label: BitmapFont,
    /// The name of the tool heading the tool-controls bar.
    pub heading: BitmapFont,
}

impl Faces {
    /// The faces `theme` sets the window's text in at `scale`.
    #[must_use]
    pub fn of(theme: &Theme, scale: Scale) -> Self {
        let face = |role| BitmapFont::for_role(theme.fonts(), role, scale);
        Self {
            caption: face(TextRole::Caption),
            label: face(TextRole::Body),
            heading: face(TextRole::SectionHeader),
        }
    }
}

/// What the layout is resolved from beyond the window's size and faces.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Needs {
    /// The wells the palette holds.
    pub wells: usize,
    /// Whether the last of them is the clear ink rather than an entry.
    pub clear: bool,
    /// The colour picker's height across the colour pane's inner width
    /// ([`Layout::dock_inner_width`]).
    pub picker: u32,
    /// How tall the colour pane's buttons and choices stand.
    pub colour_controls: u32,
    /// How tall the recent colours stand.
    pub recents: u32,
    /// How broad the tool box's tools are.
    pub tool_box: u32,
    /// How long the tool box is with every tool seated.
    pub tool_box_length: u32,
    /// How long the tool box is with one line of its tools seated.
    pub tool_box_least: u32,
    /// What the Layers pane is laid out from.
    pub layers: LayerMeasures,
    /// How tall the Adjustments and the Effects panes' bodies stand, in
    /// [`PaneKind::OPERATIONS`] order, the margin about them aside.
    pub operations: [BodyHeights; 2],
    /// How long the view strip's commands are.
    pub view_strip: u32,
    /// The rows the top band holds for the tool-controls bar across
    /// [`Layout::controls_width`].
    pub bar_rows: u32,
    /// The client size each floating pane's tool window was resized to, in
    /// physical pixels, by [`PaneKind::index`]; `None` where it opens at its
    /// own.
    pub chosen: [Option<(u32, u32)>; PaneKind::ALL.len()],
}

/// What the smallest window is floored on beyond the faces.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Floor {
    /// The least width every tool's tool-controls bar seats each of its
    /// settings in.
    pub controls: u32,
    /// How long the view strip is: it is given all its commands.
    pub view_strip: u32,
    /// How broad the tool box is, and the shortest it still shows a tool at.
    pub tool_box: (u32, u32),
    /// The most wells a palette strip holds.
    pub wells: usize,
}

/// How tall a pane's body stands: the least its dock seats it at before it
/// grows another pane, what shows it whole, which it is never given more
/// than, and who scrolls it between.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BodyHeights {
    /// The least it is seated at.
    pub least: u32,
    /// The height that shows it whole.
    pub natural: u32,
    /// Who scrolls it when it is given less.
    pub fit: Fit,
}

impl BodyHeights {
    /// The same heights with `by` more about the body.
    const fn grown(self, by: u32) -> Self {
        Self {
            least: self.least.saturating_add(by),
            natural: self.natural.saturating_add(by),
            fit: self.fit,
        }
    }
}

/// Who scrolls a pane's body given less than its natural height.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Fit {
    /// The body lays itself out in the height it is given, scrolling its own
    /// parts: a tool box's lines, a list's rows.
    #[default]
    Own,
    /// The dock lays the body out whole and scrolls it by a bar down its
    /// right ([`PaneSlot::content`], [`PaneSlot::bar`]).
    Scrolled,
}

/// One pane as laid out: the plate it is drawn on, its band, and its body.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PaneSlot {
    /// Which pane.
    pub kind: PaneKind,
    /// The plate the pane is drawn on: its band and its body.
    pub frame: Rect,
    /// The mini title band across the plate's top; empty for a floating
    /// pane, whose tool window's band is the window manager's.
    pub header: Rect,
    /// What the pane holds, beneath the band; empty when it is rolled up or
    /// the dock had no room left for it.
    pub body: Rect,
    /// Where the body's content is laid out: the body itself, or, for a body
    /// the dock scrolls given less than it shows whole at, the whole of it
    /// from the body's top beside the bar, unscrolled.
    pub content: Rect,
    /// The bar the dock scrolls the body by, down its right; empty while the
    /// body shows whole or scrolls its own parts.
    pub bar: Rect,
    /// How the pane's tool window may be sized while it floats; fixed for a
    /// docked pane.
    pub sizing: WindowSizing,
}

impl PaneSlot {
    /// The part of the body its content shows through: all of it but the
    /// bar.
    #[must_use]
    pub fn viewport(&self) -> Rect {
        Rect::new(
            self.body.left(),
            self.body.top(),
            self.body.width.saturating_sub(self.bar.width),
            self.body.height,
        )
    }
}

/// A dock as laid out: its band down one edge, and its panes top down.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DockLayout {
    /// The band down the window's edge; empty when the dock holds nothing.
    pub rect: Rect,
    /// Its panes, top down.
    pub panes: Vec<PaneSlot>,
}

/// The window's resolved geometry: by default every part empty, as for a
/// window not yet laid out.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Layout {
    window: Rect,
    top: Rect,
    controls: Rect,
    view_strip: Rect,
    docks: [DockLayout; 2],
    floating: Vec<PaneSlot>,
    tool_box: Rect,
    tools: Rect,
    dock: Rect,
    wells: Rect,
    colour_controls: Rect,
    picker: Rect,
    recents: Rect,
    layers: layers_pane::Placed,
    operations: [Rect; 2],
    operation_settings: [Rect; 2],
    canvas: Rect,
    bands: CanvasBands,
    status: Rect,
    position: Rect,
    message: Rect,
    sprite: Rect,
    zoom: Rect,
    bar: Placement,
    number_face: NumberFace,
}

impl Layout {
    /// The geometry of a `width`×`height` client area holding what `needs`
    /// states; the tool-controls bar is placed into it by
    /// [`seat_bar`](Self::seat_bar).
    #[must_use]
    pub fn for_window(
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        (needs, panes): (Needs, &Arrangement),
    ) -> Self {
        let gap = gap(theme, scale);
        let bar = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        let window = Rect::new(0, 0, width, height);
        let mut rest = window;
        let (top, controls, view_strip) =
            top_band(&mut rest, (needs.bar_rows, needs.view_strip), theme, scale);
        let band = StatusBand::new(theme, scale);
        let status = rest.take_bottom(band.height());
        let (position, message, sprite, zoom) = status_slots(status, band);
        let header = TitleBar::height_of(TitleBarCommands::Pane, scale, theme);
        let measures = PaneMeasures {
            gap,
            header,
            dock: scale.scale_length(DOCK_WIDTH),
            wells: scale.scale_length(WELLS_HEIGHT),
            palette_well: well_sides(scale).1,
            bar: (bar, ScrollBar::least_length(bar, scale, theme)),
        };
        let seated =
            Side::BOTH.map(|side| measures.seat(rest.height, panes.docked(side), panes, needs));
        let left = rest.take_left(
            measures
                .dock_width(panes.docked(Side::Left), &seated[0], needs)
                .min(rest.width / 2),
        );
        let right = rest.take_right(
            measures
                .dock_width(panes.docked(Side::Right), &seated[1], needs)
                .min(rest.width / 2),
        );
        let docks = [
            measures.stack(left, panes.docked(Side::Left), &seated[0], needs),
            measures.stack(right, panes.docked(Side::Right), &seated[1], needs),
        ];
        let floating = measures.float(window, panes, needs);
        let slot = |kind| {
            docks
                .iter()
                .flat_map(|dock| &dock.panes)
                .chain(&floating)
                .find(|slot| slot.kind == kind)
                .copied()
        };
        let body = |kind| slot(kind).map_or(Rect::EMPTY, |slot| slot.body);
        let content = |kind| slot(kind).map_or(Rect::EMPTY, |slot| slot.content);
        let inset = |area: Rect| {
            if area.is_empty() {
                Rect::EMPTY
            } else {
                area.inset(gap / 2)
            }
        };
        let tool_box = body(PaneKind::Tools);
        let dock = body(PaneKind::Colour);
        let colour = ColourSlots::of(content(PaneKind::Colour), gap, measures.wells, needs);
        let layers = layers_pane::place(inset(body(PaneKind::Layers)), needs.layers, (gap, bar));
        let operations = PaneKind::OPERATIONS.map(&body);
        // Laid out whole from the body's top where the dock scrolls the body.
        let operation_settings = PaneKind::OPERATIONS.map(|kind| inset(content(kind)));
        let strip = if panes.in_strip() { needs.wells } else { 0 };
        let mut bands = CanvasBands::claim(&mut rest, strip, (gap, bar), scale);
        let floated = body(PaneKind::Palette);
        if !floated.is_empty() {
            bands.float(floated, (gap, bar), measures.palette_well, needs.wells);
        }
        Self {
            window,
            top,
            controls,
            view_strip,
            docks,
            floating,
            tool_box,
            tools: tool_box.inset(gap / 2),
            dock,
            wells: colour.wells,
            colour_controls: colour.controls,
            picker: colour.picker,
            recents: colour.recents,
            layers,
            operations,
            operation_settings,
            canvas: rest,
            bands,
            status,
            position,
            message,
            sprite,
            zoom,
            bar: Placement::default(),
            number_face: NumberFace::of(scale, theme),
        }
    }

    /// Seat the tool-controls bar as `placement` places it in
    /// [`controls`](Self::controls).
    pub fn seat_bar(&mut self, placement: Placement) {
        self.bar = placement;
    }

    /// The width the tool-controls bar is laid out across in a window
    /// `width` wide whose view strip is `view_strip` long.
    #[must_use]
    pub fn controls_width(width: u32, view_strip: u32, theme: &Theme, scale: Scale) -> u32 {
        width.saturating_sub(gap(theme, scale) * 3 + view_strip)
    }

    /// The width a dock-wide pane's content is laid out across.
    #[must_use]
    pub fn dock_inner_width(theme: &Theme, scale: Scale) -> u32 {
        scale
            .scale_length(DOCK_WIDTH)
            .saturating_sub(gap(theme, scale) * 2)
    }

    /// The smallest client area worth laying out: every tool's bar with
    /// each of its settings seated, in the most rows `bar_rows` says any bar
    /// takes across the width it is given, beside the whole view strip; and
    /// around a canvas of a few dozen pixels the tool box showing a tool, the
    /// dock at its width beside the bar its bodies scroll by — held to the
    /// least they need, they do — and the largest palette strip.
    #[must_use]
    pub fn min_size(
        theme: &Theme,
        scale: Scale,
        floor: Floor,
        bar_rows: impl FnOnce(u32) -> u32,
    ) -> (u32, u32) {
        let gap = gap(theme, scale);
        let bar = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        let canvas = scale.scale_length(MIN_CANVAS);
        let dock = scale.scale_length(DOCK_WIDTH);
        let header = TitleBar::height_of(TitleBarCommands::Pane, scale, theme);
        let (tool_box, tool_box_least) = floor.tool_box;
        let left = tool_box + gap * 2;
        // The dock is never wider than the canvas's side of the window.
        let width = (left + (canvas + bar).max(dock) + dock + bar)
            .max(floor.controls + floor.view_strip + gap * 3);
        let middle = width - left - dock - bar;
        let across = middle.saturating_sub(gap * 2);
        let palette = palette_row(floor.wells, across, (gap, bar), scale).band(gap);
        let rows = bar_rows(Self::controls_width(width, floor.view_strip, theme, scale));
        let height = top_height(rows, strip_height(theme, scale), gap)
            + StatusBand::new(theme, scale).height()
            + (canvas + bar + palette)
                .max(header + tool_box_least + gap * 2)
                .max(header + scale.scale_length(WELLS_HEIGHT) * 2 + gap);
        (width, height)
    }

    /// The whole client area.
    #[must_use]
    pub const fn window(&self) -> Rect {
        self.window
    }

    /// Report every pixel the window and its floating panes draw.
    pub fn damage_all(&self, damage: &mut Region) {
        damage.add(self.window);
        for slot in &self.floating {
            damage.add(slot.frame);
        }
    }

    /// The floating panes, each in the rectangle of the drawing its tool
    /// window shows.
    #[must_use]
    pub fn floating(&self) -> &[PaneSlot] {
        &self.floating
    }

    /// Where a form is laid out in the drawing for its dialog window: below
    /// the window's own area and every floating pane, overlapping none.
    #[must_use]
    pub fn dialog_origin(&self) -> Point {
        let below = self
            .floating
            .iter()
            .map(|slot| slot.frame.bottom())
            .fold(self.window.bottom(), i32::max);
        Point::new(self.window.left(), below)
    }

    /// The window pane `kind`'s open lists are held within: its own tool
    /// window's rectangle when it floats, the client area otherwise.
    #[must_use]
    pub fn pane_window(&self, kind: PaneKind) -> Rect {
        self.floating
            .iter()
            .find(|slot| slot.kind == kind)
            .map_or(self.window, |slot| slot.frame)
    }

    /// The band across the top: the tool-controls bar and the view strip.
    #[must_use]
    pub const fn top(&self) -> Rect {
        self.top
    }

    /// Where the tool-controls bar is laid out.
    #[must_use]
    pub const fn controls(&self) -> Rect {
        self.controls
    }

    /// The tool-controls bar's parts, as placed.
    #[must_use]
    pub const fn bar(&self) -> &Placement {
        &self.bar
    }

    /// Where the view strip's commands are seated.
    #[must_use]
    pub const fn view_strip(&self) -> Rect {
        self.view_strip
    }

    /// The dock down `side`.
    #[must_use]
    pub const fn dock_on(&self, side: Side) -> &DockLayout {
        &self.docks[side.index()]
    }

    /// Where pane `kind` is laid out, docked or floating, if it is shown.
    #[must_use]
    pub fn pane(&self, kind: PaneKind) -> Option<&PaneSlot> {
        self.panes()
            .chain(&self.floating)
            .find(|slot| slot.kind == kind)
    }

    /// Every docked pane laid out, the left dock's first, each dock's top
    /// down.
    pub fn panes(&self) -> impl Iterator<Item = &PaneSlot> {
        self.docks.iter().flat_map(|dock| &dock.panes)
    }

    /// The tool pane's body; empty where it is hidden or rolled up.
    #[must_use]
    pub const fn tool_box(&self) -> Rect {
        self.tool_box
    }

    /// Where the tool box's tools are seated.
    #[must_use]
    pub const fn tools(&self) -> Rect {
        self.tools
    }

    /// The colour pane's body; empty where it is hidden or rolled up. Its
    /// parts — the wells, the choices, the picker and the recent colours —
    /// are laid out whole in the pane's own layout, which its bar scrolls
    /// through the body.
    #[must_use]
    pub const fn dock(&self) -> Rect {
        self.dock
    }

    /// The Layers pane as laid out: empty where it is hidden or rolled up.
    #[must_use]
    pub const fn layers(&self) -> &layers_pane::Placed {
        &self.layers
    }

    /// The Layers pane's body; empty where it is hidden or rolled up.
    #[must_use]
    pub fn layers_body(&self) -> Rect {
        self.pane(PaneKind::Layers)
            .map_or(Rect::EMPTY, |slot| slot.body)
    }

    /// The body of `kind`, the Adjustments or the Effects pane: empty where
    /// it is hidden or rolled up, and for any other pane.
    #[must_use]
    pub fn operation(&self, kind: PaneKind) -> Rect {
        kind.operation()
            .map_or(Rect::EMPTY, |at| self.operations[at])
    }

    /// The bodies of the Adjustments and the Effects panes, in that order.
    #[must_use]
    pub const fn operations(&self) -> [Rect; 2] {
        self.operations
    }

    /// Where the settings of `kind`, the Adjustments or the Effects pane, are
    /// laid out: within its body, or whole from its top where the dock
    /// scrolls the body; empty for any other pane.
    #[must_use]
    pub fn operation_settings(&self, kind: PaneKind) -> Rect {
        kind.operation()
            .map_or(Rect::EMPTY, |at| self.operation_settings[at])
    }

    /// The current-colour wells, atop the dock, in the colour pane's own
    /// layout.
    #[must_use]
    pub const fn wells(&self) -> Rect {
        self.wells
    }

    /// The colour pane's buttons and choices, beneath the wells.
    #[must_use]
    pub const fn colour_controls(&self) -> Rect {
        self.colour_controls
    }

    /// The colour picker, beneath the colour pane's choices.
    #[must_use]
    pub const fn picker(&self) -> Rect {
        self.picker
    }

    /// The recent colours, beneath the picker.
    #[must_use]
    pub const fn recents(&self) -> Rect {
        self.recents
    }

    /// The primary colour's well, overlapping the secondary's.
    #[must_use]
    pub fn primary_well(&self) -> Rect {
        let side = self.well_side();
        Rect::new(self.wells.left(), self.wells.top(), side, side)
    }

    /// The secondary colour's well, behind and below-right of the primary's.
    #[must_use]
    pub fn secondary_well(&self) -> Rect {
        let side = self.well_side();
        let offset = i32::try_from(side / 2).unwrap_or(0);
        Rect::new(
            self.wells.left() + offset,
            self.wells.top() + offset,
            side,
            side,
        )
        .intersection(&self.wells)
    }

    /// Where the colour being edited is named.
    #[must_use]
    pub fn well_caption(&self) -> Rect {
        let side = self.well_side();
        let used = side + side / 2;
        let left = self.wells.left() + i32::try_from(used + side / 4).unwrap_or(0);
        Rect::new(
            left,
            self.wells.top(),
            self.wells.width.saturating_sub(used + side / 4),
            self.wells.height,
        )
    }

    fn well_side(&self) -> u32 {
        (self.wells.height * 2 / 3).min(self.wells.width / 3)
    }

    /// Where the picture is shown.
    #[must_use]
    pub const fn canvas(&self) -> Rect {
        self.canvas
    }

    /// The vertical scrollbar.
    #[must_use]
    pub const fn vertical_bar(&self) -> Rect {
        self.bands.vertical_bar
    }

    /// The horizontal scrollbar.
    #[must_use]
    pub const fn horizontal_bar(&self) -> Rect {
        self.bands.horizontal_bar
    }

    /// The square where the two bars meet.
    #[must_use]
    pub const fn corner(&self) -> Rect {
        self.bands.corner
    }

    /// Where the palette is: its strip beneath the canvas, or its tool
    /// window's rectangle while it floats; empty while it is hidden.
    #[must_use]
    pub const fn palette(&self) -> Rect {
        self.bands.palette
    }

    /// Every well of the palette, unscrolled: a row longer than the strip, or
    /// a floating palette's grid taller than its window, where it overflows.
    #[must_use]
    pub const fn swatches(&self) -> Rect {
        self.bands.swatches
    }

    /// The part of the palette the wells show in.
    #[must_use]
    pub const fn wells_view(&self) -> Rect {
        self.bands.wells_view
    }

    /// The bar the wells scroll by, empty while every well shows.
    #[must_use]
    pub const fn palette_bar(&self) -> Rect {
        self.bands.palette_bar
    }

    /// The control at the strip's right end that floats the palette, empty
    /// unless the palette lies in its strip.
    #[must_use]
    pub const fn pop_out(&self) -> Rect {
        self.bands.pop_out
    }

    /// The way the palette's wells scroll: along its strip, or down a
    /// floating palette.
    #[must_use]
    pub const fn palette_axis(&self) -> ScrollOrientation {
        self.bands.axis
    }

    /// A palette well's side, in its strip or floating.
    #[must_use]
    pub const fn palette_well(&self) -> u32 {
        self.bands.well
    }

    /// The palette's wells to a row: every one in the strip, as many as a
    /// floating palette's width holds.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.bands.columns
    }

    /// The face the palette numbers its marked wells in.
    #[must_use]
    pub const fn number_face(&self) -> NumberFace {
        self.number_face
    }

    /// The status band.
    #[must_use]
    pub const fn status(&self) -> Rect {
        self.status
    }

    /// Where the status band states the size, depth and pointed-at pixel.
    #[must_use]
    pub const fn position(&self) -> Rect {
        self.position
    }

    /// Where it states a message.
    #[must_use]
    pub const fn message(&self) -> Rect {
        self.message
    }

    /// Where it states which sprite is showing.
    #[must_use]
    pub const fn sprite(&self) -> Rect {
        self.sprite
    }

    /// Where it states the zoom, which opens the zoom menu.
    #[must_use]
    pub const fn zoom(&self) -> Rect {
        self.zoom
    }
}

fn gap(theme: &Theme, scale: Scale) -> u32 {
    scale.scale_length(theme.metrics().control_gap).max(1)
}

/// One row of the top band, inside its gaps: a control's height.
fn strip_height(theme: &Theme, scale: Scale) -> u32 {
    tairix_controls::Button::height(scale, theme)
}

/// The band across the top of the window, taken from `rest`: the room the
/// tool-controls bar is laid out in, `bar_rows` rows deep, and the view strip
/// `view_strip` long at its trailing end — `(band, controls, view strip)`.
fn top_band(
    rest: &mut Rect,
    (bar_rows, view_strip): (u32, u32),
    theme: &Theme,
    scale: Scale,
) -> (Rect, Rect, Rect) {
    let gap = gap(theme, scale);
    let row = strip_height(theme, scale);
    let top = rest.take_top(top_height(bar_rows, row, gap));
    let mut across = top.inset(gap);
    let strip = view_strip.min(across.width);
    let strip_rect = Rect::new(
        across.right().saturating_sub_unsigned(strip),
        across.top(),
        strip,
        row.min(across.height),
    );
    let _ = across.take_right(view_strip + gap);
    (top, across, strip_rect)
}

/// The top band's height for `rows` rows of the bar, each `row` high, a gap
/// apart and a gap from either edge.
fn top_height(rows: u32, row: u32, gap: u32) -> u32 {
    let rows = rows.max(1);
    row * rows + gap * (rows - 1) + gap * 2
}

/// A palette well's least and most side at `scale`.
fn well_sides(scale: Scale) -> (u32, u32) {
    let least = scale.scale_length(LEAST_WELL).max(1);
    (least, scale.scale_length(MOST_WELL).max(least))
}

/// The palette's wells as the strip sets them out: one row, and the control
/// that floats them at its end.
struct PaletteRow {
    /// A well's side.
    side: u32,
    /// The row's whole length, which overflows the room it shows in when
    /// longer than it.
    length: u32,
    /// The room the row shows in, before the control.
    room: u32,
    /// The strip's inside: a well, and the bar beneath an overflowing row;
    /// the control is as tall, and square.
    inside: u32,
}

impl PaletteRow {
    /// The strip's height, a gap about its inside, or none for no wells.
    const fn band(&self, gap: u32) -> u32 {
        if self.inside == 0 {
            0
        } else {
            self.inside.saturating_add(gap.saturating_mul(2))
        }
    }
}

/// How the palette strip sets `wells` across its `across` pixels inside: in
/// one row, each well as broad as the row allows between [`LEAST_WELL`] and
/// [`MOST_WELL`] with the control at its end taken as one more, the row
/// longer than its room where even the least would not fit.
fn palette_row(wells: usize, across: u32, (gap, bar): (u32, u32), scale: Scale) -> PaletteRow {
    let (least, most) = well_sides(scale);
    let count = u32::try_from(wells).unwrap_or(u32::MAX);
    if count == 0 {
        return PaletteRow {
            side: 0,
            length: 0,
            room: 0,
            inside: 0,
        };
    }
    let side = (across.saturating_sub(gap) / count.saturating_add(1)).clamp(least, most);
    let length = side.saturating_mul(count);
    let overflows = length > across.saturating_sub(gap.saturating_add(side));
    let inside = side.saturating_add(if overflows { bar } else { 0 });
    PaletteRow {
        side,
        length,
        room: across.saturating_sub(gap.saturating_add(inside)),
        inside,
    }
}

/// A floating palette's wells as its tool window opens on them, each at its
/// most: a grid as near square as its entries make, never fewer than
/// [`LEAST_COLUMNS`] across where it has as many wells.
#[derive(Copy, Clone)]
struct PaletteGrid {
    width: u32,
    height: u32,
}

impl PaletteGrid {
    /// The grid `needs` opens on, each well `well` pixels across.
    fn of(needs: Needs, well: u32) -> Self {
        let entries = needs.wells.saturating_sub(usize::from(needs.clear));
        let root = entries.isqrt();
        let columns = (root + usize::from(root * root < entries))
            .max(LEAST_COLUMNS)
            .min(needs.wells.max(1));
        Self {
            width: wells_span(columns, well),
            height: wells_span(needs.wells.div_ceil(columns), well),
        }
    }
}

/// The length `count` wells `well` pixels across span in a line.
fn wells_span(count: usize, well: u32) -> u32 {
    u32::try_from(count)
        .unwrap_or(u32::MAX)
        .saturating_mul(well)
}

/// The bands claimed about the canvas — the palette strip beneath, or where a
/// floating palette's tool window sets it out instead — and the scroll bars
/// and their corner.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CanvasBands {
    palette: Rect,
    swatches: Rect,
    wells_view: Rect,
    palette_bar: Rect,
    pop_out: Rect,
    columns: usize,
    axis: ScrollOrientation,
    well: u32,
    vertical_bar: Rect,
    horizontal_bar: Rect,
    corner: Rect,
}

impl Default for CanvasBands {
    /// No bands, for a window not yet laid out.
    fn default() -> Self {
        let none = Rect::default();
        Self {
            palette: none,
            swatches: none,
            wells_view: none,
            palette_bar: none,
            pop_out: none,
            columns: 0,
            axis: ScrollOrientation::Vertical,
            well: 0,
            vertical_bar: none,
            horizontal_bar: none,
            corner: none,
        }
    }
}

impl CanvasBands {
    /// Claim the palette strip of `wells` and the scroll bars `bar` broad
    /// from `rest`, leaving it the canvas.
    fn claim(rest: &mut Rect, wells: usize, (gap, bar): (u32, u32), scale: Scale) -> Self {
        let across = rest.width.saturating_sub(gap * 2);
        let row = palette_row(wells, across, (gap, bar), scale);
        let palette = rest.take_bottom(row.band(gap));
        let (left, top) = (
            palette.left().saturating_add_unsigned(gap),
            palette.top().saturating_add_unsigned(gap),
        );
        let swatches = Rect::new(left, top, row.length, row.side);
        let wells_view = Rect::new(left, top, row.length.min(row.room), row.side);
        let palette_bar = if row.length > row.room {
            Rect::new(left, top.saturating_add_unsigned(row.side), row.room, bar)
        } else {
            Rect::EMPTY
        };
        let pop_out = Rect::new(
            left.saturating_add_unsigned(row.room.saturating_add(gap)),
            top,
            row.inside,
            row.inside,
        )
        .intersection(&palette);
        let under = rest.take_bottom(bar);
        let vertical_bar = rest.take_right(bar);
        let corner = if under.is_empty() || vertical_bar.is_empty() {
            Rect::EMPTY
        } else {
            Rect::new(
                vertical_bar.left(),
                under.top(),
                vertical_bar.width,
                under.height,
            )
        };
        Self {
            palette,
            swatches,
            wells_view,
            palette_bar,
            pop_out,
            columns: wells.max(1),
            axis: ScrollOrientation::Horizontal,
            well: row.side,
            vertical_bar,
            horizontal_bar: Rect::new(rest.left(), under.top(), rest.width, under.height),
            corner,
        }
    }

    /// Set the palette out instead in its tool window's `frame`: `wells`
    /// wells `well` pixels across, as many to a row as the frame holds a gap
    /// in from its edge, scrolled down by a bar `bar` broad down its right
    /// where they overflow it.
    fn float(&mut self, frame: Rect, (gap, bar): (u32, u32), well: u32, wells: usize) {
        let room = frame.inset(gap);
        let columns_in = |width: u32| {
            usize::try_from(width / well.max(1))
                .unwrap_or(usize::MAX)
                .clamp(1, wells.max(1))
        };
        let rows_in = |columns: usize| wells_span(wells.div_ceil(columns), well);
        let overflows = rows_in(columns_in(room.width)) > room.height;
        let across = room.width.saturating_sub(if overflows { bar } else { 0 });
        let columns = columns_in(across);
        let swatches = Rect::new(
            room.left(),
            room.top(),
            wells_span(columns, well),
            rows_in(columns),
        );
        self.palette_bar = if overflows {
            let bar = bar.min(room.width);
            Rect::new(
                room.right().saturating_sub_unsigned(bar),
                room.top(),
                bar,
                room.height,
            )
        } else {
            Rect::EMPTY
        };
        self.palette = frame;
        self.swatches = swatches;
        self.wells_view = Rect::new(
            room.left(),
            room.top(),
            swatches.width.min(across),
            swatches.height.min(room.height),
        );
        self.columns = columns;
        self.axis = ScrollOrientation::Vertical;
        self.well = well;
    }
}

/// The colour pane's parts, top to bottom a gap apart: the wells, the
/// buttons and choices, the picker and the recent colours.
struct ColourSlots {
    wells: Rect,
    controls: Rect,
    picker: Rect,
    recents: Rect,
}

impl ColourSlots {
    /// The parts laid down `body`: one too short for them all gives the
    /// picker what is left before the recent colours, which it lays out by
    /// giving up its fields before its plane.
    fn of(body: Rect, gap: u32, wells: u32, needs: Needs) -> Self {
        if body.is_empty() {
            return Self {
                wells: Rect::EMPTY,
                controls: Rect::EMPTY,
                picker: Rect::EMPTY,
                recents: Rect::EMPTY,
            };
        }
        let mut rest = body.inset(gap / 2);
        let wells = rest.take_top(wells);
        let _ = rest.take_top(gap);
        let controls = rest.take_top(needs.colour_controls);
        let _ = rest.take_top(gap);
        // Whole, or not at all: a part of a row of wells is no row.
        let fits = rest.height >= needs.picker + gap + needs.recents;
        let recents = rest.take_bottom(if fits { needs.recents } else { 0 });
        let _ = rest.take_bottom(if recents.is_empty() { 0 } else { gap });
        let picker = rest.take_top(needs.picker);
        Self {
            wells,
            controls,
            picker,
            recents,
        }
    }
}

/// The lengths panes are laid out by, at one scale and theme.
#[derive(Copy, Clone, Debug)]
struct PaneMeasures {
    gap: u32,
    header: u32,
    dock: u32,
    wells: u32,
    palette_well: u32,
    /// The bars panes scroll by: how broad they are, and the least length one
    /// is drawn at.
    bar: (u32, u32),
}

impl PaneMeasures {
    /// How broad pane `kind`'s plate stands, with the margin about it.
    fn width(self, kind: PaneKind, needs: Needs) -> u32 {
        match kind {
            // The dock's margin and the box's own inset, half a gap each side
            // apiece, so a docked column fits its tools whatever the gap's
            // parity.
            PaneKind::Tools => needs.tool_box + 4 * (self.gap / 2),
            PaneKind::Colour | PaneKind::Layers | PaneKind::Adjustments | PaneKind::Effects => {
                self.dock
            }
            PaneKind::Palette => PaletteGrid::of(needs, self.palette_well).width + self.gap * 2,
        }
    }

    /// How tall pane `kind`'s body stands, its content and the margin about
    /// it: the least its dock seats it at, what shows it whole, and who
    /// scrolls what lies between.
    fn heights(self, kind: PaneKind, needs: Needs) -> BodyHeights {
        let gap = self.gap;
        match kind {
            // The tool box scrolls its own lines of tools.
            PaneKind::Tools => BodyHeights {
                least: needs.tool_box_least + gap * 2,
                natural: needs.tool_box_length + gap * 2,
                fit: Fit::Own,
            },
            // The wells and the choices seated, the picker and the recent
            // colours scrolled into view beneath them.
            PaneKind::Colour => BodyHeights {
                least: self.wells + needs.colour_controls + gap * 2,
                natural: self.wells
                    + needs.colour_controls
                    + needs.picker
                    + needs.recents
                    + gap * 4,
                fit: Fit::Scrolled,
            },
            PaneKind::Layers => needs.layers.heights(gap).grown(gap),
            PaneKind::Adjustments => needs.operations[0].grown(gap),
            PaneKind::Effects => needs.operations[1].grown(gap),
            PaneKind::Palette => {
                let whole = PaletteGrid::of(needs, self.palette_well).height + gap * 2;
                BodyHeights {
                    least: whole,
                    natural: whole,
                    fit: Fit::Own,
                }
            }
        }
    }

    /// The panes `panes` floats, each at the size its tool window was resized
    /// to where it may be and was, else at its own breadth and open height,
    /// in rectangles of the drawing right of `window`, one below the next:
    /// what each tool window shows, overlapping neither the window nor
    /// another.
    fn float(self, window: Rect, panes: &Arrangement, needs: Needs) -> Vec<PaneSlot> {
        let left = tairix_geometry::to_i32(window.width);
        let mut top = 0;
        panes
            .floating()
            .map(|kind| {
                let sizing = self.sizing(kind, needs);
                let (width, height) = match needs.chosen[kind.index()] {
                    Some(chosen) if sizing.resizable() => held_in(chosen, sizing),
                    _ => (
                        self.width(kind, needs),
                        self.heights(kind, needs).natural.max(1),
                    ),
                };
                let frame = Rect::new(left, top, width, height);
                top = top.saturating_add(tairix_geometry::to_i32(frame.height));
                let (content, bar) = self.content(kind, frame, needs);
                PaneSlot {
                    kind,
                    frame,
                    header: Rect::new(frame.left(), frame.top(), frame.width, 0),
                    body: frame,
                    content,
                    bar,
                    sizing,
                }
            })
            .collect()
    }

    /// How pane `kind`'s tool window may be sized while it floats: the
    /// palette as [`palette_sizing`](Self::palette_sizing) holds it; the
    /// Layers pane from its controls and two rows to every row, and from its
    /// own breadth to twice it; every other pane, and a palette of no wells,
    /// at its own size.
    fn sizing(self, kind: PaneKind, needs: Needs) -> WindowSizing {
        match kind {
            PaneKind::Palette if needs.wells > 0 => self.palette_sizing(needs),
            PaneKind::Layers => {
                let (width, heights) = (self.width(kind, needs), self.heights(kind, needs));
                WindowSizing::Resizable {
                    min_width_px: width,
                    min_height_px: heights.least,
                    max_width_px: width.saturating_mul(2),
                    max_height_px: heights.natural.max(heights.least),
                }
            }
            _ => WindowSizing::Fixed,
        }
    }

    /// A floating palette's range: between one column of its wells and one
    /// row of them, never shorter than a well or than its bar where it may
    /// show one, and never so narrow that a palette too short for its bar
    /// would need it.
    fn palette_sizing(self, needs: Needs) -> WindowSizing {
        let (well, inset) = (self.palette_well, self.gap * 2);
        let (bar, least) = self.bar;
        let down = well.max(least.min(PaletteGrid::of(needs, well).height));
        let rows = usize::try_from(down / well.max(1))
            .unwrap_or(usize::MAX)
            .max(1);
        let across = if down < least {
            wells_span(needs.wells.div_ceil(rows), well)
        } else if needs.wells > rows {
            well + bar
        } else {
            well
        };
        let most = wells_span(needs.wells, well).saturating_add(inset);
        WindowSizing::Resizable {
            min_width_px: across.saturating_add(inset),
            min_height_px: down.saturating_add(inset),
            max_width_px: most,
            max_height_px: most,
        }
    }

    /// The heights the open panes of `panes`, down a dock `height` tall,
    /// give their bodies: every band seated first, then each body its least,
    /// then each grown towards what shows it whole — in both rounds the pane
    /// `arrangement` says was opened last first, panes opened alike top first.
    fn seat(
        self,
        height: u32,
        panes: &[Docked],
        arrangement: &Arrangement,
        needs: Needs,
    ) -> [u32; PaneKind::ALL.len()] {
        let mut given = [0u32; PaneKind::ALL.len()];
        let Some(count) = u32::try_from(panes.len()).ok().filter(|&count| count > 0) else {
            return given;
        };
        let margin = self.gap / 2;
        let bands = self
            .header
            .saturating_mul(count)
            .saturating_add(margin.saturating_mul(count - 1));
        let mut room = height
            .saturating_sub(margin.saturating_mul(2))
            .saturating_sub(bands);
        let mut order: ArrayVec<(u64, usize, PaneKind), { PaneKind::ALL.len() }> = ArrayVec::new();
        for (at, docked) in panes.iter().enumerate() {
            if !docked.collapsed {
                // A dock holds fewer panes than there are kinds of pane.
                let _ = order.try_push((arrangement.opened(docked.kind), at, docked.kind));
            }
        }
        order.sort_unstable_by_key(|&(opened, at, _)| (core::cmp::Reverse(opened), at));
        for &(_, _, kind) in order.as_slice() {
            let share = self.heights(kind, needs).least.min(room);
            room -= share;
            given[kind.index()] = share;
        }
        for &(_, _, kind) in order.as_slice() {
            let grown = self
                .heights(kind, needs)
                .natural
                .saturating_sub(given[kind.index()])
                .min(room);
            room -= grown;
            given[kind.index()] += grown;
        }
        given
    }

    /// A dock's width: its broadest pane's, widened by a bar where a body the
    /// dock scrolls is given less than it shows whole at; none for a dock of
    /// none.
    fn dock_width(self, panes: &[Docked], given: &[u32; PaneKind::ALL.len()], needs: Needs) -> u32 {
        panes
            .iter()
            .map(|docked| {
                let heights = self.heights(docked.kind, needs);
                let body = given[docked.kind.index()];
                let scrolled = heights.fit == Fit::Scrolled && body > 0 && body < heights.natural;
                self.width(docked.kind, needs) + if scrolled { self.bar.0 } else { 0 }
            })
            .max()
            .unwrap_or(0)
    }

    /// `panes` stacked down `dock` top first, half a gap about each plate,
    /// each body as tall as `given` says and a pane given none its band
    /// alone.
    fn stack(
        self,
        dock: Rect,
        panes: &[Docked],
        given: &[u32; PaneKind::ALL.len()],
        needs: Needs,
    ) -> DockLayout {
        if dock.is_empty() || panes.is_empty() {
            return DockLayout {
                rect: dock,
                panes: Vec::new(),
            };
        }
        let margin = self.gap / 2;
        let mut rest = dock.inset(margin);
        let mut slots = Vec::with_capacity(panes.len());
        for docked in panes {
            let frame = rest.take_top(self.header.saturating_add(given[docked.kind.index()]));
            let header = Rect::new(
                frame.left(),
                frame.top(),
                frame.width,
                self.header.min(frame.height),
            );
            let body = Rect::new(
                frame.left(),
                header.bottom(),
                frame.width,
                frame.height.saturating_sub(header.height),
            );
            let body = if body.height == 0 { Rect::EMPTY } else { body };
            let (content, bar) = self.content(docked.kind, body, needs);
            slots.push(PaneSlot {
                kind: docked.kind,
                frame,
                header,
                body,
                content,
                bar,
                sizing: WindowSizing::Fixed,
            });
            let _ = rest.take_top(margin);
        }
        DockLayout {
            rect: dock,
            panes: slots,
        }
    }

    /// Where pane `kind`'s content is laid out in `body`, and the bar it is
    /// scrolled by: a body the dock scrolls, given less than it shows whole
    /// at, is laid out whole from its top beside a bar down its right; any
    /// other fills its body and has none.
    fn content(self, kind: PaneKind, body: Rect, needs: Needs) -> (Rect, Rect) {
        let heights = self.heights(kind, needs);
        if body.is_empty() || heights.fit == Fit::Own || heights.natural <= body.height {
            return (body, Rect::EMPTY);
        }
        let breadth = self.bar.0.min(body.width);
        let bar = Rect::new(
            body.right().saturating_sub_unsigned(breadth),
            body.top(),
            breadth,
            body.height,
        );
        let content = Rect::new(
            body.left(),
            body.top(),
            body.width - breadth,
            heights.natural,
        );
        (content, bar)
    }
}

/// `size` held inside `sizing`'s range, where a bound of `0` bounds nothing.
fn held_in((width, height): (u32, u32), sizing: WindowSizing) -> (u32, u32) {
    let hold = |value: u32, least: u32, most: u32| {
        value
            .max(least)
            .min(if most == 0 { u32::MAX } else { most })
    };
    (
        hold(width, sizing.min_width_px(), sizing.max_width_px()),
        hold(height, sizing.min_height_px(), sizing.max_height_px()),
    )
}

/// The status band's position slot, message slot, sprite slot and zoom
/// slot, left to right: each but the message as wide as the most it reads.
fn status_slots(status: Rect, band: StatusBand) -> (Rect, Rect, Rect, Rect) {
    let room = |text: &mut String| {
        let room = band.slot_width(text);
        text.clear();
        room
    };
    let mut text = String::new();
    let zoom = ZOOMS
        .iter()
        .map(|&rung| {
            write_zoom(&mut text, Zoom::of(rung).percent());
            room(&mut text)
        })
        .max()
        .unwrap_or(0);
    let widest_name = SpriteName::from_bytes(&[b'W'; SpriteName::MAX_LEN]);
    write_sprite(&mut text, MAX_ENTRIES, MAX_ENTRIES, widest_name.as_ref());
    let sprite = room(&mut text);
    let side = MAX_SIDE - 1;
    write_position(
        &mut text,
        (side, side),
        Some([u8::MAX, u8::MAX, u8::MAX, 0]),
    );
    let position = room(&mut text);
    let slots = band.slots(status, [position], [zoom, sprite]);
    let ([position], [zoom, sprite]) = (slots.leading, slots.trailing);
    (position, slots.message, sprite, zoom)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
