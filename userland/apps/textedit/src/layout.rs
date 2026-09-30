//! The editor window's geometry: the one function every painter and every
//! hit-test agrees on.
//!
//! ```text
//! +------------------------------------------------------------+
//! | [find.........] [replace.......] Aa Word Hex  < > Rep All x |  (open)
//! +-----+------------------------------------------------+-----+
//! |   12| the text grid                                  |  ^  |
//! |   13|                                                |  |  |
//! +-----+------------------------------------------------+-----+
//! |     |================================================|     |
//! +------------------------------------------------------------+
//! | Ln 12, Col 5       2 problems      Rust  Text  LF  Tab  INS |
//! +------------------------------------------------------------+
//! ```
//!
//! Every extent comes from the theme's metrics at the desktop scale and the
//! faces' own measures. The window has no menu bar: its menus open on a
//! secondary press anywhere in it. The bands are claimed from the edges
//! inward, so however small the window only the grid gives up room; a region
//! with no room is an empty rectangle, which every painter and hit-test treats
//! as absent.

use tairix_controls::{Button, TextField};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Scale};
use tairix_theme::Theme;

/// The find bar's buttons after its two fields, in order.
pub const FIND_BUTTONS: [&str; 8] = [
    "Aa", "Word", "Hex", "\u{2191}", "\u{2193}", "Replace", "All", "\u{00d7}",
];

/// The status band's fields that answer a click, right to left from the
/// band's end: what each opens is decided by the view.
pub const STATUS_FIELDS: usize = 5;

/// The widest text each clickable status field shows, right to left: the
/// field is measured once from it, so the band does not shift as it changes.
const STATUS_WIDEST: [&str; STATUS_FIELDS] =
    ["OVR", "Spaces: 16", "CRLF", "Text", "System configuration"];

/// The widest the caret position reads, so the band does not shift as it
/// changes.
const POSITION_WIDEST: &str = "Ln 99999999, Col 99999";

/// The shortest a find field is worth drawing, in logical pixels.
const MIN_FIELD: u32 = 64;

/// Gutter columns beyond the widest line number: a marker, then a gap.
const GUTTER_SPARE_COLUMNS: u32 = 2;

/// The client area a new window opens at, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (760, 520);

/// Grid rows the smallest window still shows.
const MIN_ROWS: u32 = 3;

/// Character cells across the smallest window, its gutter among them.
const MIN_COLUMNS: u32 = 24;

/// The window's resolved geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    window: Rect,
    find: Rect,
    find_field: Rect,
    replace_field: Rect,
    find_buttons: [Rect; FIND_BUTTONS.len()],
    gutter: Rect,
    grid: Rect,
    vertical_bar: Rect,
    horizontal_bar: Rect,
    corner: Rect,
    status: Rect,
    position: Rect,
    message: Rect,
    status_fields: [Rect; STATUS_FIELDS],
    cell: (u32, u32),
    gutter_digits: u32,
}

/// What a layout is resolved from, beyond the window's size.
#[derive(Copy, Clone, Debug)]
pub struct Faces {
    /// The face the grid is set in.
    pub grid: BitmapFont,
    /// The face the status band is set in.
    pub status: BitmapFont,
}

impl Layout {
    /// The geometry of a `width`×`height` client area, with the find bar open
    /// when `find` and a gutter wide enough for `gutter_digits` digits
    /// (none in the hex view).
    #[must_use]
    pub fn for_window(
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
        find: bool,
        gutter_digits: u32,
    ) -> Self {
        let metrics = theme.metrics();
        let gap = scale.scale_length(metrics.control_gap).max(1);
        let bar = scale.scale_length(metrics.scrollbar_breadth).max(1);
        let cell = (
            faces.grid.cell_width().max(1),
            faces.grid.line_height().max(1),
        );
        let window = Rect::new(0, 0, width, height);
        let mut rest = window;

        let button_row = Button::height(scale, theme);
        let status = take_bottom(&mut rest, status_height(faces, gap));
        let (position, message, status_fields) = status_slots(status, faces.status, gap);

        let (find_band, find_field, replace_field, find_buttons) = if find {
            let row = TextField::height(scale, theme).max(button_row);
            let band = take_top(&mut rest, row + gap * 2);
            let (find_field, replace_field, buttons) = find_slots(band, row, gap, scale, theme);
            (band, find_field, replace_field, buttons)
        } else {
            (
                Rect::EMPTY,
                Rect::EMPTY,
                Rect::EMPTY,
                [Rect::EMPTY; FIND_BUTTONS.len()],
            )
        };

        let under = take_bottom(&mut rest, bar);
        let vertical_bar = take_right(&mut rest, bar);
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
        let gutter = if gutter_digits == 0 {
            Rect::EMPTY
        } else {
            take_left(
                &mut rest,
                (gutter_digits + GUTTER_SPARE_COLUMNS) * cell.0 + gap,
            )
        };
        let horizontal_bar = Rect::new(rest.left(), under.top(), rest.width, under.height);
        Self {
            window,
            find: find_band,
            find_field,
            replace_field,
            find_buttons,
            gutter,
            grid: rest,
            vertical_bar,
            horizontal_bar,
            corner,
            status,
            position,
            message,
            status_fields,
            cell,
            gutter_digits,
        }
    }

    /// The smallest client area worth laying out: the bands around a grid of
    /// a few rows and columns.
    #[must_use]
    pub fn min_size(theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        let metrics = theme.metrics();
        let gap = scale.scale_length(metrics.control_gap).max(1);
        let bar = scale.scale_length(metrics.scrollbar_breadth).max(1);
        let columns = faces.grid.cell_width().max(1) * MIN_COLUMNS;
        let rows = faces.grid.line_height().max(1) * MIN_ROWS;
        (columns + bar, status_height(faces, gap) + bar + rows)
    }

    /// How many digits the gutter was sized for; none in the hex view.
    #[must_use]
    pub const fn gutter_digits(&self) -> u32 {
        self.gutter_digits
    }

    /// The whole client area.
    #[must_use]
    pub const fn window(&self) -> Rect {
        self.window
    }

    /// The find bar, empty when it is closed.
    #[must_use]
    pub const fn find(&self) -> Rect {
        self.find
    }

    /// The find field.
    #[must_use]
    pub const fn find_field(&self) -> Rect {
        self.find_field
    }

    /// The replace field.
    #[must_use]
    pub const fn replace_field(&self) -> Rect {
        self.replace_field
    }

    /// Each find-bar button, in [`FIND_BUTTONS`] order.
    #[must_use]
    pub const fn find_buttons(&self) -> &[Rect; FIND_BUTTONS.len()] {
        &self.find_buttons
    }

    /// The line-number gutter, empty in the hex view.
    #[must_use]
    pub const fn gutter(&self) -> Rect {
        self.gutter
    }

    /// The grid the document is drawn in.
    #[must_use]
    pub const fn grid(&self) -> Rect {
        self.grid
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

    /// Where the status band states the caret's position.
    #[must_use]
    pub const fn position(&self) -> Rect {
        self.position
    }

    /// Where the status band states a message or the problems summary.
    #[must_use]
    pub const fn message(&self) -> Rect {
        self.message
    }

    /// The status band's clickable fields, right to left.
    #[must_use]
    pub const fn status_fields(&self) -> &[Rect; STATUS_FIELDS] {
        &self.status_fields
    }

    /// A grid cell's width and height.
    #[must_use]
    pub const fn cell(&self) -> (u32, u32) {
        self.cell
    }

    /// Whole rows the grid shows.
    #[must_use]
    pub const fn rows(&self) -> usize {
        (self.grid.height / self.cell.1) as usize
    }

    /// Whole columns the grid shows.
    #[must_use]
    pub const fn columns(&self) -> usize {
        (self.grid.width / self.cell.0) as usize
    }

    /// The gutter and the grid together: the rows' full breadth.
    fn body(&self) -> Rect {
        let left = if self.gutter.is_empty() {
            self.grid.left()
        } else {
            self.gutter.left()
        };
        Rect::new(
            left,
            self.grid.top(),
            self.gutter.width + self.grid.width,
            self.grid.height,
        )
    }

    /// The grid row, and the half-cell across it, under `point`; `None` when
    /// it lies outside the grid and the gutter.
    #[must_use]
    pub fn cell_at(&self, point: Point) -> Option<(usize, usize)> {
        self.body().contains(point).then(|| self.cell_near(point))
    }

    /// The grid row, and the half-cell across it, nearest `point` wherever it
    /// lies: what a drag past the grid's edge extends to.
    #[must_use]
    pub fn cell_near(&self, point: Point) -> (usize, usize) {
        let x = u32::try_from(point.x.saturating_sub(self.grid.left())).unwrap_or(0);
        let y = u32::try_from(point.y.saturating_sub(self.grid.top())).unwrap_or(0);
        (
            (y / self.cell.1) as usize,
            (x.saturating_mul(2) / self.cell.0) as usize,
        )
    }

    /// The pixel rectangle of grid row `row`, across the gutter and the grid.
    #[must_use]
    pub fn row_rect(&self, row: usize) -> Rect {
        let body = self.body();
        let offset = u32::try_from(row)
            .unwrap_or(u32::MAX)
            .saturating_mul(self.cell.1);
        Rect::new(
            body.left(),
            body.top().saturating_add_unsigned(offset),
            body.width,
            self.cell.1,
        )
        .intersection(&body)
    }
}

/// The status band's height: a line of its face and a gap above and below.
fn status_height(faces: Faces, gap: u32) -> u32 {
    faces.status.line_height().max(1) + gap * 2
}

fn take_top(rest: &mut Rect, height: u32) -> Rect {
    let height = height.min(rest.height);
    let band = Rect::new(rest.left(), rest.top(), rest.width, height);
    *rest = Rect::new(
        rest.left(),
        rest.top().saturating_add_unsigned(height),
        rest.width,
        rest.height - height,
    );
    band
}

fn take_bottom(rest: &mut Rect, height: u32) -> Rect {
    let height = height.min(rest.height);
    *rest = Rect::new(rest.left(), rest.top(), rest.width, rest.height - height);
    Rect::new(rest.left(), rest.bottom(), rest.width, height)
}

fn take_right(rest: &mut Rect, width: u32) -> Rect {
    let width = width.min(rest.width);
    *rest = Rect::new(rest.left(), rest.top(), rest.width - width, rest.height);
    Rect::new(rest.right(), rest.top(), width, rest.height)
}

fn take_left(rest: &mut Rect, width: u32) -> Rect {
    let width = width.min(rest.width);
    let band = Rect::new(rest.left(), rest.top(), width, rest.height);
    *rest = Rect::new(
        rest.left().saturating_add_unsigned(width),
        rest.top(),
        rest.width - width,
        rest.height,
    );
    band
}

/// The status band's position slot, message slot, and clickable fields.
fn status_slots(band: Rect, face: BitmapFont, gap: u32) -> (Rect, Rect, [Rect; STATUS_FIELDS]) {
    let inner = Rect::new(
        band.left().saturating_add_unsigned(gap),
        band.top(),
        band.width.saturating_sub(gap * 2),
        band.height,
    );
    let mut rest = inner;
    let mut fields = [Rect::EMPTY; STATUS_FIELDS];
    for (field, widest) in fields.iter_mut().zip(STATUS_WIDEST) {
        *field = take_right(&mut rest, face.text_width(widest) + gap * 2);
    }
    let position = take_left(&mut rest, face.text_width(POSITION_WIDEST) + gap * 2);
    (position, rest, fields)
}

/// The find bar's two fields and its buttons, the fields sharing what the
/// buttons leave.
fn find_slots(
    band: Rect,
    row: u32,
    gap: u32,
    scale: Scale,
    theme: &Theme,
) -> (Rect, Rect, [Rect; FIND_BUTTONS.len()]) {
    let top = band.top().saturating_add_unsigned(gap);
    let widths: [u32; FIND_BUTTONS.len()] =
        FIND_BUTTONS.map(|label| Button::labelled(label).measured_width(scale, theme));
    let buttons_wide: u32 = widths.iter().map(|wide| wide + gap).sum();
    let for_fields = band.width.saturating_sub(buttons_wide + gap * 3);
    let min_field = scale.scale_length(MIN_FIELD);
    let find_wide = (for_fields * 3 / 5).max(min_field.min(for_fields));
    let replace_wide = for_fields.saturating_sub(find_wide);
    let mut x = band.left().saturating_add_unsigned(gap);
    let find_field = Rect::new(x, top, find_wide, row).intersection(&band);
    x = x.saturating_add_unsigned(find_wide + gap);
    let replace_field = Rect::new(x, top, replace_wide, row).intersection(&band);
    x = x.saturating_add_unsigned(replace_wide + gap);
    let mut buttons = [Rect::EMPTY; FIND_BUTTONS.len()];
    for (slot, wide) in buttons.iter_mut().zip(widths) {
        *slot = Rect::new(x, top, wide, row).intersection(&band);
        x = x.saturating_add_unsigned(wide + gap);
    }
    (find_field, replace_field, buttons)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
