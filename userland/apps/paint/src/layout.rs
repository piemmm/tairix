//! The painter window's geometry: the one function every painter and every
//! hit-test agrees on.
//!
//! ```text
//! +----------------------------------------------------------------------+
//! | Brush  Size [ 4 ] px  [x] Smooth edges                 [-][+][F][1]|#|
//! +----+-------------------------------------------+---+---------------+
//! | S  |                                           | ^ | [#][#] which  |
//! | P  |               the picture                 | | | [plane  ]|H|  |
//! |=B  |                                           |   | [#] #ff00aa   |
//! | :  |                                           |   | H S V  R G B  |
//! |    +-------------------------------------------+---+               |
//! |    |===========================================|   |               |
//! |    +-----------------------------------------------+               |
//! |    | [][][][][][][][][][][][][][][][][]            |               |
//! +----+-----------------------------------------------+---------------+
//! | (12, 34) #ff00aa   640 × 480, 256 colours         1 of 3: sprite  100% |
//! +----------------------------------------------------------------------+
//! ```
//!
//! The tool-controls bar runs across the top, the view's commands at its
//! end; the tool box runs down the left and the colour dock down the right;
//! the palette strip lies beneath the canvas and its bars.
//!
//! Every extent comes from the theme's metrics at the desktop scale and the
//! faces' own measures. Bands are claimed from the edges inward, so however
//! small the window only the canvas gives up room; a region with no room is
//! an empty rectangle, which every painter and hit-test treats as absent.

use alloc::string::String;

use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_image::SpriteName;
use tairix_theme::{TextRole, Theme};

use crate::canvas::MAX_SIDE;
use crate::document::MAX_ENTRIES;
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

/// The least a palette well is across, in logical pixels: what a 256-colour
/// palette is set at in a few rows.
const LEAST_WELL: u32 = 14;

/// The most a palette well is across, in logical pixels: what a palette of a
/// few colours is set at in one row.
const MOST_WELL: u32 = 24;

/// What the layout's text is set in.
#[derive(Copy, Clone, Debug)]
pub struct Faces {
    /// The status band and the wells' caption.
    pub status: BitmapFont,
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
            status: face(TextRole::Caption),
            label: face(TextRole::Body),
            heading: face(TextRole::SectionHeader),
        }
    }
}

/// What the layout is resolved from beyond the window's size and faces.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Needs {
    /// The wells the palette strip holds.
    pub wells: usize,
    /// The colour picker's height across the dock's inner width
    /// ([`Layout::dock_inner_width`]).
    pub picker: u32,
    /// How broad the tool box's tools are.
    pub tool_box: u32,
    /// How long the view strip's commands are.
    pub view_strip: u32,
    /// The rows the top band holds for the tool-controls bar across
    /// [`Layout::controls_width`].
    pub bar_rows: u32,
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

/// The window's resolved geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    window: Rect,
    top: Rect,
    controls: Rect,
    view_strip: Rect,
    tool_box: Rect,
    tools: Rect,
    dock: Rect,
    wells: Rect,
    picker: Rect,
    canvas: Rect,
    vertical_bar: Rect,
    horizontal_bar: Rect,
    corner: Rect,
    palette: Rect,
    swatches: Rect,
    columns: usize,
    status: Rect,
    position: Rect,
    message: Rect,
    sprite: Rect,
    zoom: Rect,
    bar: Placement,
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
        faces: Faces,
        needs: Needs,
    ) -> Self {
        let gap = gap(theme, scale);
        let bar = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        let window = Rect::new(0, 0, width, height);
        let mut rest = window;
        let row = strip_height(theme, scale);
        let top = rest.take_top(top_height(needs.bar_rows, row, gap));
        let mut across = top.inset(gap);
        let view_strip = Rect::new(
            across
                .right()
                .saturating_sub_unsigned(needs.view_strip.min(across.width)),
            across.top(),
            needs.view_strip.min(across.width),
            row.min(across.height),
        );
        let _ = across.take_right(needs.view_strip + gap);
        let controls = across;
        let status = rest.take_bottom(status_height(faces, gap));
        let (position, message, sprite, zoom) = status_slots(status, faces.status, gap);
        let tool_box = rest.take_left((needs.tool_box + gap * 2).min(rest.width / 2));
        let dock = rest.take_right(scale.scale_length(DOCK_WIDTH).min(rest.width / 2));
        let (wells, picker) = dock_slots(dock, gap, scale, needs);
        let grid = palette_grid(needs.wells, rest.width.saturating_sub(gap * 2), scale);
        let palette = rest.take_bottom(if grid.height == 0 {
            0
        } else {
            grid.height + gap * 2
        });
        let swatches = Rect::new(
            palette.left().saturating_add_unsigned(gap),
            palette.top().saturating_add_unsigned(gap),
            grid.width,
            grid.height,
        )
        .intersection(&palette.inset(gap));
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
        let horizontal_bar = Rect::new(rest.left(), under.top(), rest.width, under.height);
        Self {
            window,
            top,
            controls,
            view_strip,
            tool_box,
            tools: tool_box.inset(gap),
            dock,
            wells,
            picker,
            canvas: rest,
            vertical_bar,
            horizontal_bar,
            corner,
            palette,
            swatches,
            columns: grid.columns,
            status,
            position,
            message,
            sprite,
            zoom,
            bar: Placement::default(),
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

    /// The width the dock's content is laid out across.
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
    /// dock at its width, and the largest palette strip.
    #[must_use]
    pub fn min_size(
        theme: &Theme,
        scale: Scale,
        faces: Faces,
        floor: Floor,
        bar_rows: impl FnOnce(u32) -> u32,
    ) -> (u32, u32) {
        let gap = gap(theme, scale);
        let bar = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        let canvas = scale.scale_length(MIN_CANVAS);
        let dock = scale.scale_length(DOCK_WIDTH);
        let (tool_box, tool_box_least) = floor.tool_box;
        let left = tool_box + gap * 2;
        // The dock is never wider than the canvas's side of the window.
        let width = (left + (canvas + bar).max(dock) + dock)
            .max(floor.controls + floor.view_strip + gap * 3);
        let middle = width - left - dock;
        let palette = palette_grid(floor.wells, middle.saturating_sub(gap * 2), scale).height;
        let rows = bar_rows(Self::controls_width(width, floor.view_strip, theme, scale));
        let height = top_height(rows, strip_height(theme, scale), gap)
            + status_height(faces, gap)
            + (canvas + bar + palette + gap * 2)
                .max(tool_box_least + gap * 2)
                .max(scale.scale_length(WELLS_HEIGHT) * 2);
        (width, height)
    }

    /// The whole client area.
    #[must_use]
    pub const fn window(&self) -> Rect {
        self.window
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

    /// The tool box's band down the left.
    #[must_use]
    pub const fn tool_box(&self) -> Rect {
        self.tool_box
    }

    /// Where the tool box's tools are seated.
    #[must_use]
    pub const fn tools(&self) -> Rect {
        self.tools
    }

    /// The colour dock on the canvas's other side.
    #[must_use]
    pub const fn dock(&self) -> Rect {
        self.dock
    }

    /// The current-colour wells, atop the dock.
    #[must_use]
    pub const fn wells(&self) -> Rect {
        self.wells
    }

    /// The colour picker, beneath the wells.
    #[must_use]
    pub const fn picker(&self) -> Rect {
        self.picker
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
        self.vertical_bar
    }

    /// The horizontal scrollbar.
    #[must_use]
    pub const fn horizontal_bar(&self) -> Rect {
        self.horizontal_bar
    }

    /// The square where the two bars meet.
    #[must_use]
    pub const fn corner(&self) -> Rect {
        self.corner
    }

    /// The palette strip's band beneath the canvas.
    #[must_use]
    pub const fn palette(&self) -> Rect {
        self.palette
    }

    /// Where the palette's wells are drawn.
    #[must_use]
    pub const fn swatches(&self) -> Rect {
        self.swatches
    }

    /// The palette's wells to a row.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
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

/// The top band's height for `rows` rows of the bar, each `row` high, a gap
/// apart and a gap from either edge.
fn top_height(rows: u32, row: u32, gap: u32) -> u32 {
    let rows = rows.max(1);
    row * rows + gap * (rows - 1) + gap * 2
}

/// The status band's height: a line of its face and a gap above and below.
fn status_height(faces: Faces, gap: u32) -> u32 {
    faces.status.line_height().max(1) + gap * 2
}

/// The palette's wells as the strip sets them out.
struct PaletteGrid {
    columns: usize,
    width: u32,
    height: u32,
}

/// How the palette strip sets `wells` across `width`: in as few rows as hold
/// every well at least [`LEAST_WELL`] across, the rows evened out, each well
/// as broad as its row allows up to [`MOST_WELL`].
fn palette_grid(wells: usize, width: u32, scale: Scale) -> PaletteGrid {
    let least = scale.scale_length(LEAST_WELL).max(1);
    let most = scale.scale_length(MOST_WELL).max(least);
    let count = u32::try_from(wells).unwrap_or(u32::MAX);
    if count == 0 || width < least {
        return PaletteGrid {
            columns: 1,
            width: 0,
            height: 0,
        };
    }
    let rows = count.div_ceil(width / least);
    let columns = count.div_ceil(rows);
    let side = (width / columns).min(most);
    PaletteGrid {
        columns: usize::try_from(columns).unwrap_or(1),
        width: columns.saturating_mul(side),
        height: rows.saturating_mul(side),
    }
}

/// The dock's wells and picker, top to bottom, a gap between: a dock too
/// short for the whole picker gives it what is left, which it lays out by
/// giving up its fields before its plane.
fn dock_slots(dock: Rect, gap: u32, scale: Scale, needs: Needs) -> (Rect, Rect) {
    let mut rest = dock.inset(gap);
    let wells = rest.take_top(scale.scale_length(WELLS_HEIGHT));
    let _ = rest.take_top(gap);
    let picker = rest.take_top(needs.picker);
    (wells, picker)
}

/// The status band's position slot, message slot, sprite slot and zoom
/// slot, left to right: each but the message as wide as the most it reads.
fn status_slots(band: Rect, face: BitmapFont, gap: u32) -> (Rect, Rect, Rect, Rect) {
    let room = |text: &mut String| {
        let room = face.text_width(text) + gap * 2;
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
    let mut rest = Rect::new(
        band.left().saturating_add_unsigned(gap),
        band.top(),
        band.width.saturating_sub(gap * 2),
        band.height,
    );
    let zoom = rest.take_right(zoom);
    let sprite = rest.take_right(sprite);
    let position = rest.take_left(position);
    (position, rest, sprite, zoom)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
