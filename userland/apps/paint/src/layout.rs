//! The painter window's geometry: the one function every painter and every
//! hit-test agrees on.
//!
//! ```text
//! +----------------------------------------------------------------------+
//! | [tools ....................] | [zoom] [grid]                         |
//! +-----------+--------------------------------------+---+--------------+
//! | palette   |                                      | ^ | [#][#] which |
//! | tool      |            the picture               | | | [plane  ]|H||
//! | settings  |                                      |   | [#] #ff00aa  |
//! |           |                                      |   | H S V  R G B |
//! +-----------+--------------------------------------+---+              |
//! |           |======================================|   |              |
//! +----------------------------------------------------------------------+
//! | 640 × 480, 256 colours  (12, 34) #ff00aa   sprite  100%               |
//! +----------------------------------------------------------------------+
//! ```
//!
//! The panel down the left holds the palette and the tool's settings; the
//! colour dock down the right holds the two colour wells and the picker
//! editing the one chosen.
//!
//! Every extent comes from the theme's metrics at the desktop scale and the
//! face's own measures. Bands are claimed from the edges inward, so however
//! small the window only the canvas gives up room; a region with no room is
//! an empty rectangle, which every painter and hit-test treats as absent.

use alloc::string::String;

use tairix_controls::Toolbar;
use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_image::SpriteName;
use tairix_theme::Theme;

use crate::canvas::MAX_SIDE;
use crate::document::MAX_ENTRIES;
use crate::render::{write_position, write_sprite, write_zoom};
use crate::viewport::{Zoom, ZOOMS};

/// The client area a new window opens at, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (900, 640);

/// The panel's width, in logical pixels: room for sixteen wells a row of a
/// full palette, each large enough to hit.
const PANEL_WIDTH: u32 = 200;

/// The colour dock's width, in logical pixels: room for the picker with its
/// fields stacked beneath its plane.
const DOCK_WIDTH: u32 = 232;

/// The current-colour wells' band, in logical pixels.
const WELLS_HEIGHT: u32 = 52;

/// The least canvas the smallest window keeps, in logical pixels.
const MIN_CANVAS: u32 = 96;

/// What the layout is resolved from beyond the window's size.
#[derive(Copy, Clone, Debug)]
pub struct Faces {
    /// The face the status band and the wells' captions are set in.
    pub status: BitmapFont,
}

/// How much of the panel and the dock their content needs, measured for
/// their inner widths ([`Layout::panel_inner_width`],
/// [`Layout::dock_inner_width`]).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PanelNeeds {
    /// The palette grid's height.
    pub swatches: u32,
    /// The tool settings' height.
    pub settings: u32,
    /// The colour picker's height.
    pub picker: u32,
}

/// The window's resolved geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    window: Rect,
    toolbar: Rect,
    tools: Rect,
    panel: Rect,
    swatches: Rect,
    settings: Rect,
    dock: Rect,
    wells: Rect,
    picker: Rect,
    canvas: Rect,
    vertical_bar: Rect,
    horizontal_bar: Rect,
    corner: Rect,
    status: Rect,
    position: Rect,
    message: Rect,
    sprite: Rect,
    zoom: Rect,
}

impl Layout {
    /// The geometry of a `width`×`height` client area whose panel and dock
    /// hold what `needs` states.
    #[must_use]
    pub fn for_window(
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
        needs: PanelNeeds,
    ) -> Self {
        let gap = gap(theme, scale);
        let bar = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        let window = Rect::new(0, 0, width, height);
        let mut rest = window;
        let strip_height = strip_height(theme, scale);
        let toolbar_band = rest.take_top(strip_height + gap * 2);
        let tools = toolbar_band.inset(gap);
        let status = rest.take_bottom(status_height(faces, gap));
        let (position, message, sprite, zoom) = status_slots(status, faces.status, gap);
        let panel_width = scale.scale_length(PANEL_WIDTH).min(rest.width / 2);
        let panel = rest.take_left(panel_width);
        let (swatches, settings) = panel_slots(panel, gap, needs);
        let dock = rest.take_right(scale.scale_length(DOCK_WIDTH).min(rest.width / 2));
        let (wells, picker) = dock_slots(dock, gap, scale, needs);
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
            toolbar: toolbar_band,
            tools,
            panel,
            swatches,
            settings,
            dock,
            wells,
            picker,
            canvas: rest,
            vertical_bar,
            horizontal_bar,
            corner,
            status,
            position,
            message,
            sprite,
            zoom,
        }
    }

    /// The width the panel's content is laid out across.
    #[must_use]
    pub fn panel_inner_width(theme: &Theme, scale: Scale) -> u32 {
        scale
            .scale_length(PANEL_WIDTH)
            .saturating_sub(gap(theme, scale) * 2)
    }

    /// The width the dock's content is laid out across.
    #[must_use]
    pub fn dock_inner_width(theme: &Theme, scale: Scale) -> u32 {
        scale
            .scale_length(DOCK_WIDTH)
            .saturating_sub(gap(theme, scale) * 2)
    }

    /// The smallest client area worth laying out: the bands around a canvas
    /// of a few dozen pixels, the toolbar able to show at least one tool.
    #[must_use]
    pub fn min_size(theme: &Theme, scale: Scale, faces: Faces, toolbar: &Toolbar) -> (u32, u32) {
        let gap = gap(theme, scale);
        let bar = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        let canvas = scale.scale_length(MIN_CANVAS);
        let width =
            (scale.scale_length(PANEL_WIDTH) + canvas + bar + scale.scale_length(DOCK_WIDTH))
                .max(toolbar.min_width(scale, theme) + gap * 2);
        let height = strip_height(theme, scale)
            + gap * 2
            + status_height(faces, gap)
            + canvas.max(scale.scale_length(WELLS_HEIGHT) * 2)
            + bar;
        (width, height)
    }

    /// The whole client area.
    #[must_use]
    pub const fn window(&self) -> Rect {
        self.window
    }

    /// The toolbar band.
    #[must_use]
    pub const fn toolbar(&self) -> Rect {
        self.toolbar
    }

    /// Where the toolbar's tools are seated.
    #[must_use]
    pub const fn tools(&self) -> Rect {
        self.tools
    }

    /// The panel beside the canvas.
    #[must_use]
    pub const fn panel(&self) -> Rect {
        self.panel
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

    /// The palette grid.
    #[must_use]
    pub const fn swatches(&self) -> Rect {
        self.swatches
    }

    /// The tool's settings.
    #[must_use]
    pub const fn settings(&self) -> Rect {
        self.settings
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

/// The toolbar's tool height: a button's.
fn strip_height(theme: &Theme, scale: Scale) -> u32 {
    tairix_controls::Button::height(scale, theme)
}

/// The status band's height: a line of its face and a gap above and below.
fn status_height(faces: Faces, gap: u32) -> u32 {
    faces.status.line_height().max(1) + gap * 2
}

/// The panel's palette and settings, top to bottom, a gap between.
fn panel_slots(panel: Rect, gap: u32, needs: PanelNeeds) -> (Rect, Rect) {
    let mut rest = panel.inset(gap);
    let swatches = rest.take_top(needs.swatches);
    let _ = rest.take_top(gap);
    let settings = rest.take_top(needs.settings);
    (swatches, settings)
}

/// The dock's wells and picker, top to bottom, a gap between: a dock too
/// short for the whole picker gives it what is left, which it lays out by
/// giving up its fields before its plane.
fn dock_slots(dock: Rect, gap: u32, scale: Scale, needs: PanelNeeds) -> (Rect, Rect) {
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
