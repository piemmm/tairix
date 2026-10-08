//! Pure geometry of the scrolling item views.
//!
//! [`ListView`] (a column of full-width rows) and [`GridView`] (a wrapped grid
//! of icon tiles) are the one definition of *where* each entry is laid out and
//! *which* entries show for a given scroll offset — the single source both the
//! renderer ([`render`](mod@crate::render)) and the pointer hit-test
//! ([`entry_index_at`](crate::render::entry_index_at)) consume, so a click can
//! never resolve to a different item than the one the user saw. [`ViewLayout`]
//! is the dispatch that lets a caller treat the two uniformly without
//! branching on the browser's [`ViewMode`].
//!
//! Each view is a fixed-height header (the toolbar) above the scrolling item
//! area. The items are laid out at their natural size, unscrolled, and shown
//! through a [`ScrollView`]: the offset is a distance in pixels along the
//! scroll axis, so a view rests at any pixel and an item the viewport's edge
//! crosses is drawn whole and cut by it. The offset is owned by the
//! [`Browser`] and clamped here through the shared [`ScrollRange`];
//! [`reveal`](ListView::reveal) is the one rule that keeps the selection on
//! screen.
//!
//! The grid is deliberately not a *file manager* grid: a [`GridFlow`] chooses
//! whether tiles wrap along a row from the leading edge (the manager's
//! scrolling view) or down a column from the leading or the trailing edge (the
//! desktop's icons, which grow a new column across as they fill, from whichever
//! edge the user arranged them at). All three are the same cell maths and the
//! same hit-test, so the desktop needs no second grid.
//!
//! All arithmetic saturates and every accessor is total: a degenerate viewport
//! or a zero cell size simply shows no items, never a panic.
//!
//! [`Browser`]: crate::Browser
//! [`ViewMode`]: crate::ViewMode

use core::ops::Range;

use alloc::vec::Vec;

use tairix_controls::scroll::{ScrollModel, ScrollOrientation, ScrollRange, ScrollView};
use tairix_geometry::{GridFill, GridRun, Point, Rect};

/// Which of the two item views the browser is showing.
///
/// The two views share one selection cursor, one scroll offset, and one
/// listing; only the geometry (a column of full-width rows vs. a wrapped grid
/// of tiles) differs. Switching mode is a pure toggle on the
/// [`Browser`](crate::Browser) — it never re-reads the directory or moves the
/// selection to a different entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum ViewMode {
    /// A vertical column of full-width rows (the default).
    #[default]
    List,
    /// A wrapped grid of icon tiles.
    Grid,
}

impl ViewMode {
    /// The other view — the mode the list/grid toggle switches to.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::List => Self::Grid,
            Self::Grid => Self::List,
        }
    }
}

/// The layout of the scrolling entry list within a content viewport.
///
/// Constructed per paint/hit-test from the viewport, the row height the caller
/// renders with, the header height reserved above the list, and the number of
/// entries. It holds no selection or scroll state of its own: both are passed
/// to the accessors that need them, keeping the browser their single owner.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ListView {
    viewport: Rect,
    row_height: u32,
    header_height: u32,
    entry_count: usize,
}

impl ListView {
    /// Lay a list of `entry_count` rows of `row_height` pixels out below a
    /// `header_height`-pixel header within `viewport`.
    #[must_use]
    pub const fn new(
        viewport: Rect,
        row_height: u32,
        header_height: u32,
        entry_count: usize,
    ) -> Self {
        Self {
            viewport,
            row_height,
            header_height,
            entry_count,
        }
    }

    /// The top of the entry list, in view-local pixels: the header height,
    /// clamped so it never exceeds the viewport.
    #[must_use]
    pub fn list_top(&self) -> u32 {
        self.header_height.min(self.viewport.height)
    }

    /// The height in pixels available to the entry list below the header.
    #[must_use]
    pub fn list_height(&self) -> u32 {
        self.viewport.height.saturating_sub(self.list_top())
    }

    /// The rectangle the rows scroll through: the viewport below the header.
    #[must_use]
    pub fn list_area(&self) -> Rect {
        below_header(self.viewport, self.list_top())
    }

    /// The height every row takes together.
    #[must_use]
    pub fn content_height(&self) -> u64 {
        to_u64(self.entry_count).saturating_mul(u64::from(self.row_height))
    }

    /// The clamped scroll window for the desired `offset`, in pixels: every
    /// row's height against the list area's. The clamp is the shared
    /// [`ScrollRange`] normalisation, so the offset can never exceed what the
    /// content allows.
    #[must_use]
    pub fn scroll_range(&self, offset: u64) -> ScrollRange {
        ScrollRange::new(self.content_height(), u64::from(self.list_height()), offset)
    }

    /// The scroll model the drawn bar and the wheel move the list through,
    /// stepping a row a line.
    #[must_use]
    pub fn scroll_model(&self, offset: u64) -> ScrollModel {
        ScrollModel::in_pixels(self.scroll_range(offset), u64::from(self.row_height))
    }

    /// The list area, scrolled to the clamped `offset`: what the rows are
    /// painted through and hit-tested against.
    #[must_use]
    pub fn view(&self, offset: u64) -> ScrollView {
        ScrollView::new(
            ScrollOrientation::Vertical,
            self.list_area(),
            self.scroll_range(offset).offset(),
        )
    }

    /// The entries any part of which shows at `offset` — the one definition a
    /// renderer iterates, so it can never disagree with the hit-test about
    /// which rows are on screen.
    #[must_use]
    pub fn visible_range(&self, offset: u64) -> Range<usize> {
        self.view(offset).lines(self.row_height, self.entry_count)
    }

    /// The offset that shows the whole of row `selected` while moving the
    /// least: the clamped `offset` when it already shows, otherwise the
    /// nearest offset that brings it to the top or the bottom edge.
    #[must_use]
    pub fn reveal(&self, offset: u64, selected: Option<usize>) -> u64 {
        let model = self.scroll_model(offset);
        let Some(index) = selected.filter(|&index| index < self.entry_count) else {
            return model.offset();
        };
        let row = u64::from(self.row_height);
        model
            .revealing(to_u64(index).saturating_mul(row), row)
            .offset()
    }

    /// Where the entry at `index` is laid out, unscrolled, or `None` when there
    /// is no such entry (or no row height to lay it out with).
    #[must_use]
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        if self.row_height == 0 || index >= self.entry_count {
            return None;
        }
        let down = u32::try_from(index).ok()?.checked_mul(self.row_height)?;
        let area = self.list_area();
        Some(Rect::new(
            area.left(),
            area.top().checked_add_unsigned(down)?,
            area.width,
            self.row_height,
        ))
    }

    /// The part of the entry at `index` the window shows at `offset`, or `None`
    /// when none of it does.
    #[must_use]
    pub fn shown_rect(&self, offset: u64, index: usize) -> Option<Rect> {
        self.view(offset).to_window(self.row_rect(index)?)
    }

    /// The entry at window `point` for the desired `offset`, or `None` for the
    /// header, the empty space below the last entry, the scrollbar gutter, and
    /// anywhere else outside the list area.
    ///
    /// A row the viewport's edge cuts resolves on the part of it that shows.
    #[must_use]
    pub fn index_at(&self, offset: u64, point: Point) -> Option<usize> {
        if self.row_height == 0 {
            return None;
        }
        let at = self.view(offset).to_content(point)?;
        let down = u32::try_from(at.y.checked_sub(self.list_area().top())?).ok()?;
        let index = usize::try_from(down / self.row_height).ok()?;
        (index < self.entry_count).then_some(index)
    }

    /// The rows any part of which lies within `band`, a rectangle in layout
    /// coordinates.
    #[must_use]
    pub(crate) fn band_cells(&self, band: Rect) -> BandCells {
        let area = self.list_area();
        let rows = if overlaps(band.left(), band.width, area.left(), area.width) {
            let (from, extent) = along(band.top(), band.height, area.top());
            GridRun::fixed(self.entry_count, self.row_height, 0).shown(from, extent)
        } else {
            0..0
        };
        BandCells::new(rows, 0..1, 1, self.entry_count)
    }
}

/// The span `[lo, lo + len)` measured from `origin`, cut at `origin`: where it
/// starts and how far it reaches past `origin`.
fn along(lo: i32, len: u32, origin: i32) -> (u64, u64) {
    let start = i64::from(lo) - i64::from(origin);
    let end = start.saturating_add(i64::from(len));
    let from = start.max(0);
    let extent = end.saturating_sub(from).max(0);
    (
        u64::try_from(from).unwrap_or(0),
        u64::try_from(extent).unwrap_or(0),
    )
}

/// Whether `[lo, lo + len)` and `[other, other + other_len)` share a pixel.
fn overlaps(lo: i32, len: u32, other: i32, other_len: u32) -> bool {
    let (start, end) = (i64::from(lo), i64::from(lo) + i64::from(len));
    let (other_start, other_end) = (i64::from(other), i64::from(other) + i64::from(other_len));
    start < other_end && other_start < end
}

/// The entries a band touches: every slot of every line it reaches.
///
/// A list has one slot per line. Membership, and the difference between two
/// bands, are arithmetic over the two ranges, so neither walks the listing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BandCells {
    lines: Range<usize>,
    slots: Range<usize>,
    per_line: usize,
    count: usize,
}

impl BandCells {
    /// The cells of `lines` × `slots` in a layout of `per_line` slots a line
    /// holding `count` entries.
    fn new(lines: Range<usize>, slots: Range<usize>, per_line: usize, count: usize) -> Self {
        if lines.is_empty() || slots.is_empty() {
            return Self {
                per_line,
                count,
                ..Self::default()
            };
        }
        Self {
            lines,
            slots,
            per_line,
            count,
        }
    }

    /// Whether the band touches the entry at `index`.
    #[must_use]
    pub(crate) fn contains(&self, index: usize) -> bool {
        self.per_line != 0
            && index < self.count
            && self.lines.contains(&(index / self.per_line))
            && self.slots.contains(&(index % self.per_line))
    }

    /// Every entry the band touches, line by line.
    pub(crate) fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        cells(
            self.lines.clone(),
            self.slots.clone(),
            self.per_line,
            self.count,
        )
    }

    /// Visit every entry `self` touches and `other` does not.
    ///
    /// Two bands over one layout differ only along their edges, so only those
    /// cells are visited; bands over different layouts are compared entry by
    /// entry.
    pub(crate) fn each_not_in(&self, other: &Self, mut visit: impl FnMut(usize)) {
        if self.per_line != other.per_line || self.count != other.count {
            self.iter()
                .filter(|index| !other.contains(*index))
                .for_each(visit);
            return;
        }
        let (before, after) = outside(&self.lines, &other.lines);
        for lines in [before, after] {
            cells(lines, self.slots.clone(), self.per_line, self.count).for_each(&mut visit);
        }
        let shared = self.lines.start.max(other.lines.start)..self.lines.end.min(other.lines.end);
        let (left, right) = outside(&self.slots, &other.slots);
        for slots in [left, right] {
            cells(shared.clone(), slots, self.per_line, self.count).for_each(&mut visit);
        }
    }
}

/// The entries a band selects: every cell whose core it touches — the part
/// every tile's body holds — and, of the cells it touches only along its
/// edges, those whose body it touches.
///
/// The edge entries are at most the band's perimeter in cells, so membership
/// and the difference between two bands stay arithmetic plus a short sorted
/// list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BandMarks {
    core: BandCells,
    edge: Vec<usize>,
}

impl BandMarks {
    /// Rebuild these marks as `core` plus whichever of `cells` beyond it
    /// `touches` accepts, keeping the edge list's allocation.
    pub(crate) fn rebuild(
        &mut self,
        core: BandCells,
        cells: &BandCells,
        mut touches: impl FnMut(usize) -> bool,
    ) {
        self.edge.clear();
        cells.each_not_in(&core, |index| {
            if touches(index) {
                self.edge.push(index);
            }
        });
        self.edge.sort_unstable();
        self.core = core;
    }

    /// Whether the band selects the entry at `index`.
    #[must_use]
    pub(crate) fn contains(&self, index: usize) -> bool {
        self.core.contains(index) || self.edge.binary_search(&index).is_ok()
    }

    /// Visit every entry `self` selects and `other` does not.
    pub(crate) fn each_not_in(&self, other: &Self, mut visit: impl FnMut(usize)) {
        self.core.each_not_in(&other.core, |index| {
            if !other.contains(index) {
                visit(index);
            }
        });
        for &index in &self.edge {
            if !other.contains(index) {
                visit(index);
            }
        }
    }
}

/// The parts of `range` before and after `cut`.
fn outside(range: &Range<usize>, cut: &Range<usize>) -> (Range<usize>, Range<usize>) {
    let before_end = range.end.min(cut.start).max(range.start);
    let after_start = range.start.max(cut.end).min(range.end);
    (range.start..before_end, after_start..range.end)
}

/// The entries at `lines` × `slots` of a layout of `per_line` slots a line
/// holding `count` entries, line by line.
fn cells(
    lines: Range<usize>,
    slots: Range<usize>,
    per_line: usize,
    count: usize,
) -> impl Iterator<Item = usize> {
    lines.flat_map(move |line| {
        let first = line.checked_mul(per_line);
        slots
            .clone()
            .filter_map(move |slot| first?.checked_add(slot))
            .filter(move |index| *index < count)
    })
}

/// `viewport` less its top `header` pixels: where a view's items scroll.
fn below_header(viewport: Rect, header: u32) -> Rect {
    Rect::new(
        viewport.origin.x,
        viewport.origin.y.saturating_add_unsigned(header),
        viewport.width,
        viewport.height.saturating_sub(header),
    )
}

/// Lossless `usize` → `u64` on every supported target.
fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// The order a [`GridView`] fills its tiles in, and the edge its first tile
/// is anchored to.
///
/// The icon views in this system differ *only* in this: the file manager's
/// grid reads like text and scrolls vertically, while the desktop's icons
/// run down a column that hugs one screen edge and grows a new column
/// across. They therefore share one set of cell maths and one hit-test,
/// parameterised here, rather than a second grid written beside the first.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum GridFlow {
    /// Fill a row left-to-right from the leading edge, then wrap down onto
    /// the next row; the grid scrolls vertically. The file manager's grid.
    #[default]
    RowsFromLeading,
    /// Fill a column top-to-bottom from the leading edge, then start a new
    /// column one pitch further *across*; the grid scrolls horizontally. The
    /// desktop's icons when they are arranged from the leading edge.
    ColumnsFromLeading,
    /// Fill a column top-to-bottom, then start a new column one pitch
    /// *inward* from the trailing edge; the grid scrolls horizontally, inward
    /// from that edge. The desktop's icons when they are arranged from the
    /// trailing edge.
    ColumnsFromTrailing,
}

impl GridFlow {
    /// Whether tiles wrap down a column (rather than along a row) — the one
    /// place the flows' axis assignment is decided.
    const fn wraps_down_a_column(self) -> bool {
        matches!(self, Self::ColumnsFromLeading | Self::ColumnsFromTrailing)
    }

    /// Whether lines are measured inward from the viewport's trailing edge
    /// rather than out from its leading one — the one place the flows'
    /// anchoring differs. Only the trailing column mirrors; both other flows
    /// grow away from the leading edge.
    const fn anchors_to_the_trailing_edge(self) -> bool {
        matches!(self, Self::ColumnsFromTrailing)
    }
}

/// The pixel metrics of one grid tile: its size, and the gap between tiles.
///
/// Grouped rather than passed loose because the three are derived and consumed
/// together — `render::grid_metrics` measures all three from the theme's body
/// face at the desktop scale — and because three bare pixel counts in a
/// positional argument list are easy to transpose.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct GridMetrics {
    /// Tile width in pixels.
    pub cell_width: u32,
    /// Tile height in pixels.
    pub cell_height: u32,
    /// The uniform gap between neighbouring tiles, on both axes.
    pub gap: u32,
}

/// The layout of the wrapped icon grid within a content viewport.
///
/// Tiles of `cell_width`×`cell_height` pixels are laid out below a
/// `header_height`-pixel header, at least a `gap` apart on both axes. A line
/// holds as many *whole* tiles as the view's width (or, for a column, height)
/// fits; the lines follow one another along the scroll axis at their natural
/// pitch however many there are, and the view is a pixel window onto them. A
/// [`GridFlow`] picks which axis a line runs along and which edge the first
/// line is anchored to, and a [`GridFill`] decides what becomes of the space a
/// line has left over, so the file manager's spreading scrolling grid and the
/// desktop's fixed trailing-edge column are the same geometry with two
/// parameters changed. Like [`ListView`] it holds no selection or scroll state.
///
/// The offset is always the distance scrolled away from the anchored edge. A
/// trailing column grows toward the leading edge, so its lines are laid out in
/// a frame as long as all of them, anchored at that frame's trailing edge, and
/// [`view`](Self::view) rests on that edge at offset zero.
///
/// All arithmetic saturates and every accessor is total: a viewport too narrow
/// for even one tile across simply shows no tiles rather than panicking.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GridView {
    viewport: Rect,
    cell_width: u32,
    cell_height: u32,
    gap: u32,
    header_height: u32,
    entry_count: usize,
    flow: GridFlow,
    fill: GridFill,
}

impl GridView {
    /// Lay `entry_count` tiles of the given [`GridMetrics`] out below a
    /// `header_height`-pixel header within `viewport`, flowing as `flow`
    /// describes and spending each line's leftover space as `fill` says.
    #[must_use]
    pub const fn new(
        viewport: Rect,
        metrics: GridMetrics,
        header_height: u32,
        entry_count: usize,
        flow: GridFlow,
        fill: GridFill,
    ) -> Self {
        Self {
            viewport,
            cell_width: metrics.cell_width,
            cell_height: metrics.cell_height,
            gap: metrics.gap,
            header_height,
            entry_count,
            flow,
            fill,
        }
    }

    /// The top of the tile area, in view-local pixels.
    #[must_use]
    pub fn list_top(&self) -> u32 {
        self.header_height.min(self.viewport.height)
    }

    /// The height available to the tile area below the header.
    #[must_use]
    pub fn list_height(&self) -> u32 {
        self.viewport.height.saturating_sub(self.list_top())
    }

    /// The rectangle the tiles are shown through: the viewport below the
    /// header.
    ///
    /// The paint is confined to this, so the grid can never mark a pixel
    /// outside the region it was given — the chrome above it or the scrollbar
    /// gutter beside it — whatever a tile does inside its own rectangle.
    #[must_use]
    pub fn tile_area(&self) -> Rect {
        below_header(self.viewport, self.list_top())
    }

    /// The tiles one line holds and where along that line each of them sits —
    /// down a column for the desktop's flow, across a row for the file
    /// manager's.
    ///
    /// This is the axis the viewport bounds, so it is the one whose leftover
    /// space the [`GridFill`] policy spends, and only whole tiles are laid out
    /// along it.
    fn slot_run(&self) -> GridRun {
        let (extent, cell) = if self.flow.wraps_down_a_column() {
            (self.list_height(), self.cell_height)
        } else {
            (self.viewport.width, self.cell_width)
        };
        GridRun::new(extent, cell, self.gap, self.fill)
    }

    /// Every line the entries fill and where each of them sits along the
    /// scroll axis, from the anchored edge.
    fn line_run(&self) -> GridRun {
        let cell = if self.flow.wraps_down_a_column() {
            self.cell_width
        } else {
            self.cell_height
        };
        GridRun::fixed(self.lines_total(), cell, self.gap)
    }

    /// The axis the grid scrolls along.
    fn axis(&self) -> ScrollOrientation {
        if self.flow.wraps_down_a_column() {
            ScrollOrientation::Horizontal
        } else {
            ScrollOrientation::Vertical
        }
    }

    /// How much of the scroll axis the tile area shows.
    fn shown_extent(&self) -> u32 {
        if self.flow.wraps_down_a_column() {
            self.viewport.width
        } else {
            self.list_height()
        }
    }

    /// How many tiles one line holds — tiles down a column for the desktop's
    /// flow, tiles across a row for the file manager's.
    #[must_use]
    pub fn cells_per_line(&self) -> usize {
        self.slot_run().count()
    }

    /// The total number of lines the entries occupy (ceiling division by the
    /// tiles one line holds).
    #[must_use]
    pub fn lines_total(&self) -> usize {
        let per_line = self.cells_per_line();
        if per_line == 0 {
            return 0;
        }
        self.entry_count.div_ceil(per_line)
    }

    /// How far every line reaches along the scroll axis together, in pixels:
    /// the last line's far edge, or nothing for an empty grid.
    #[must_use]
    pub fn content_extent(&self) -> u64 {
        self.line_run().span()
    }

    /// The clamped scroll window for the desired `offset`, in pixels along the
    /// scroll axis, through the same [`ScrollRange`] normalisation the list
    /// uses.
    #[must_use]
    pub fn scroll_range(&self, offset: u64) -> ScrollRange {
        ScrollRange::new(
            self.content_extent(),
            u64::from(self.shown_extent()),
            offset,
        )
    }

    /// The scroll model the drawn bar and the wheel move the grid through,
    /// stepping one line and the gap after it a line.
    #[must_use]
    pub fn scroll_model(&self, offset: u64) -> ScrollModel {
        ScrollModel::in_pixels(
            self.scroll_range(offset),
            u64::from(self.line_run().stride()),
        )
    }

    /// The tile area, scrolled to the clamped `offset`: what the tiles are
    /// painted through and hit-tested against.
    #[must_use]
    pub fn view(&self, offset: u64) -> ScrollView {
        let range = self.scroll_range(offset);
        // A trailing column's frame is scrolled from its far end, so resting
        // on its anchored edge is the frame's greatest shift.
        let shift = if self.flow.anchors_to_the_trailing_edge() {
            range.max_offset().saturating_sub(range.offset())
        } else {
            range.offset()
        };
        ScrollView::new(self.axis(), self.tile_area(), shift)
    }

    /// The entries any part of which shows at `offset` — the one definition a
    /// renderer iterates, so it never re-derives the wrap arithmetic and can
    /// never disagree with [`shown_rect`](Self::shown_rect) about which tiles
    /// are on screen.
    #[must_use]
    pub fn visible_range(&self, offset: u64) -> Range<usize> {
        let per_line = self.cells_per_line();
        if per_line == 0 {
            return 0..0;
        }
        let lines = self.line_run().shown(
            self.scroll_range(offset).offset(),
            u64::from(self.shown_extent()),
        );
        let start = lines.start.saturating_mul(per_line).min(self.entry_count);
        let end = lines.end.saturating_mul(per_line).min(self.entry_count);
        start..end.max(start)
    }

    /// The offset that shows the whole of the tile at `selected` while moving
    /// the least (its line is `selected / cells_per_line`).
    #[must_use]
    pub fn reveal(&self, offset: u64, selected: Option<usize>) -> u64 {
        let model = self.scroll_model(offset);
        let per_line = self.cells_per_line();
        let lines = self.line_run();
        let Some(start) = selected
            .filter(|&index| per_line != 0 && index < self.entry_count)
            .and_then(|index| lines.offset(index / per_line))
        else {
            return model.offset();
        };
        model
            .revealing(u64::from(start), u64::from(lines.cell()))
            .offset()
    }

    /// How wide the frame a trailing column is laid out in is: the tile area,
    /// or every line when they reach further.
    fn frame_width(&self) -> Option<u32> {
        let lines = u32::try_from(self.content_extent()).ok()?;
        Some(lines.max(self.viewport.width))
    }

    /// The offsets of the tile at line `line` and slot `slot` within it from
    /// the tile area's origin, unscrolled: `(x, y)`. The single place the
    /// flows' anchoring differs — the trailing-edge column measures `x` inward
    /// from its frame's right edge, so its first column hugs that edge
    /// whatever the width is.
    fn tile_offsets(&self, line: usize, slot: usize) -> Option<(u32, u32)> {
        let along_slot = self.slot_run().offset(slot)?;
        let along_line = self.line_run().offset(line)?;
        let (across, down) = if self.flow.wraps_down_a_column() {
            (along_line, along_slot)
        } else {
            (along_slot, along_line)
        };
        let x = if self.flow.anchors_to_the_trailing_edge() {
            self.frame_width()?
                .checked_sub(across.checked_add(self.cell_width)?)?
        } else {
            across
        };
        Some((x, down))
    }

    /// Where the tile at `index` is laid out, unscrolled, or `None` when there
    /// is no such tile (or no line to lay it out in).
    #[must_use]
    pub fn cell_rect(&self, index: usize) -> Option<Rect> {
        let per_line = self.cells_per_line();
        if per_line == 0 || index >= self.entry_count {
            return None;
        }
        let (x, y) = self.tile_offsets(index / per_line, index % per_line)?;
        let area = self.tile_area();
        Some(Rect::new(
            area.left().checked_add_unsigned(x)?,
            area.top().checked_add_unsigned(y)?,
            self.cell_width,
            self.cell_height,
        ))
    }

    /// The part of the tile at `index` the window shows at `offset`, or `None`
    /// when none of it does.
    #[must_use]
    pub fn shown_rect(&self, offset: u64, index: usize) -> Option<Rect> {
        self.view(offset).to_window(self.cell_rect(index)?)
    }

    /// The tile at window `point` for the desired `offset`, or `None` for the
    /// header, a margin or gap between tiles, the empty space past the last
    /// tile, and anywhere else outside the tile area.
    ///
    /// A tile the viewport's edge cuts resolves on the part of it that shows.
    #[must_use]
    pub fn index_at(&self, offset: u64, point: Point) -> Option<usize> {
        let slots = self.slot_run();
        let lines = self.line_run();
        let at = self.view(offset).to_content(point)?;
        let area = self.tile_area();
        let x = u32::try_from(at.x.checked_sub(area.left())?).ok()?;
        let down = u32::try_from(at.y.checked_sub(area.top())?).ok()?;
        // The trailing-edge column measures its columns inward from its
        // frame's right edge, exactly as it lays them out.
        let across = if self.flow.anchors_to_the_trailing_edge() {
            self.frame_width()?.checked_sub(x)?.checked_sub(1)?
        } else {
            x
        };
        let (along_line, along_slot) = if self.flow.wraps_down_a_column() {
            (across, down)
        } else {
            (down, across)
        };
        // Each run resolves only its own tiles, so a point in a margin, in a
        // gap, or past the last line resolves to nothing: a click can land
        // only on a tile the user actually saw.
        let line = lines.cell_at(along_line)?;
        let slot = slots.cell_at(along_slot)?;
        let index = line.checked_mul(slots.count())?.checked_add(slot)?;
        (index < self.entry_count).then_some(index)
    }

    /// The tiles any part of which lies within `band`, a rectangle in layout
    /// coordinates. A band lying only in a margin or a gap touches none.
    #[must_use]
    pub(crate) fn band_cells(&self, band: Rect) -> BandCells {
        self.band_cells_over(band, self.line_run(), self.slot_run())
    }

    /// The tiles whose `part` — a rectangle relative to a tile's own top-left
    /// corner — `band` touches.
    #[must_use]
    pub(crate) fn band_cells_within(&self, band: Rect, part: Rect) -> BandCells {
        let left = u32::try_from(part.left()).unwrap_or(0);
        let top = u32::try_from(part.top()).unwrap_or(0);
        // The trailing-edge column measures across inward from a tile's right
        // edge, exactly as it lays its tiles out.
        let across = if self.flow.anchors_to_the_trailing_edge() {
            self.cell_width
                .saturating_sub(left.saturating_add(part.width))
        } else {
            left
        };
        let ((line_at, line_len), (slot_at, slot_len)) = if self.flow.wraps_down_a_column() {
            ((across, part.width), (top, part.height))
        } else {
            ((top, part.height), (across, part.width))
        };
        self.band_cells_over(
            band,
            self.line_run().within(line_at, line_len),
            self.slot_run().within(slot_at, slot_len),
        )
    }

    /// The cells of `lines` × `slots` any part of which lies within `band`.
    fn band_cells_over(&self, band: Rect, lines: GridRun, slots: GridRun) -> BandCells {
        let area = self.tile_area();
        let (x_from, x_extent) = along(band.left(), band.width, area.left());
        let down = along(band.top(), band.height, area.top());
        // The trailing-edge column measures across inward from its frame's
        // right edge, exactly as it lays its tiles out.
        let across = if self.flow.anchors_to_the_trailing_edge() {
            let frame = u64::from(self.frame_width().unwrap_or(0));
            let end = frame.saturating_sub(x_from);
            let start = frame.saturating_sub(x_from.saturating_add(x_extent));
            (start, end - start)
        } else {
            (x_from, x_extent)
        };
        let (line_span, slot_span) = if self.flow.wraps_down_a_column() {
            (across, down)
        } else {
            (down, across)
        };
        BandCells::new(
            lines.shown(line_span.0, line_span.1),
            slots.shown(slot_span.0, slot_span.1),
            self.cells_per_line(),
            self.entry_count,
        )
    }

    /// Whether a growing offset moves the view toward the viewport's leading
    /// edge rather than away from it: the trailing column scrolls inward.
    const fn scrolls_toward_the_leading_edge(&self) -> bool {
        self.flow.anchors_to_the_trailing_edge()
    }
}

/// One of the two item-view geometries, chosen by the browser's [`ViewMode`].
///
/// This is the single dispatch both the renderer and the pointer hit-test go
/// through, so the list and the grid expose one scrolling/hit-testing contract
/// and a caller never has to branch on the mode itself.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ViewLayout {
    /// The full-width row list.
    List(ListView),
    /// The wrapped icon grid.
    Grid(GridView),
}

impl ViewLayout {
    /// The scroll model the drawn bar and the wheel move the active view
    /// through, at the desired `offset`.
    #[must_use]
    pub fn scroll_model(&self, offset: u64) -> ScrollModel {
        match self {
            Self::List(v) => v.scroll_model(offset),
            Self::Grid(v) => v.scroll_model(offset),
        }
    }

    /// The active view's item area, scrolled to the clamped `offset`.
    #[must_use]
    pub fn view(&self, offset: u64) -> ScrollView {
        match self {
            Self::List(v) => v.view(offset),
            Self::Grid(v) => v.view(offset),
        }
    }

    /// The offset that shows the whole of `selected` while moving the least.
    #[must_use]
    pub fn reveal(&self, offset: u64, selected: Option<usize>) -> u64 {
        match self {
            Self::List(v) => v.reveal(offset, selected),
            Self::Grid(v) => v.reveal(offset, selected),
        }
    }

    /// The index of the item at window `point` for the desired scroll
    /// `offset`.
    #[must_use]
    pub fn index_at(&self, offset: u64, point: Point) -> Option<usize> {
        match self {
            Self::List(v) => v.index_at(offset, point),
            Self::Grid(v) => v.index_at(offset, point),
        }
    }

    /// Where the item at `index` is laid out, unscrolled — the rectangle the
    /// renderer draws it in through [`view`](Self::view).
    #[must_use]
    pub fn layout_rect(&self, index: usize) -> Option<Rect> {
        match self {
            Self::List(v) => v.row_rect(index),
            Self::Grid(v) => v.cell_rect(index),
        }
    }

    /// The part of the item at `index` the window shows at `offset`, or `None`
    /// when the item is out of range or scrolled wholly out of view. The exact
    /// footprint [`index_at`](Self::index_at) resolves to that item, so a
    /// damage report names what the renderer painted.
    #[must_use]
    pub fn item_rect(&self, offset: u64, index: usize) -> Option<Rect> {
        match self {
            Self::List(v) => v.shown_rect(offset, index),
            Self::Grid(v) => v.shown_rect(offset, index),
        }
    }

    /// The half-open range of entry indices the active view shows any part of
    /// at the desired scroll `offset`.
    #[must_use]
    pub fn visible_range(&self, offset: u64) -> Range<usize> {
        match self {
            Self::List(v) => v.visible_range(offset),
            Self::Grid(v) => v.visible_range(offset),
        }
    }

    /// The entries any part of which lies within `band`, a rectangle in
    /// layout coordinates.
    #[must_use]
    pub(crate) fn band_cells(&self, band: Rect) -> BandCells {
        match self {
            Self::List(v) => v.band_cells(band),
            Self::Grid(v) => v.band_cells(band),
        }
    }

    /// The entries whose `part` — relative to an item's own top-left — any of
    /// `band` touches. A list row is all body, so a list answers its cells.
    #[must_use]
    pub(crate) fn band_cells_within(&self, band: Rect, part: Rect) -> BandCells {
        match self {
            Self::List(v) => v.band_cells(band),
            Self::Grid(v) => v.band_cells_within(band, part),
        }
    }

    /// The axis the active view scrolls along.
    #[must_use]
    pub(crate) fn axis(&self) -> ScrollOrientation {
        match self {
            Self::List(_) => ScrollOrientation::Vertical,
            Self::Grid(v) => v.axis(),
        }
    }

    /// Whether a growing offset brings into view what lies toward the
    /// viewport's leading edge — true only of the trailing column, which
    /// scrolls inward.
    #[must_use]
    pub(crate) fn scrolls_toward_the_leading_edge(&self) -> bool {
        match self {
            Self::List(_) => false,
            Self::Grid(v) => v.scrolls_toward_the_leading_edge(),
        }
    }
}

/// Pure geometry of the places rail: the fixed-width column of shortcut rows
/// down the window's leading edge, and the hit-test that inverts it.
///
/// The rows are a stack of equal-height rows with one separator band inserted
/// where the mounted volumes begin, laid out at their natural size, unscrolled,
/// and shown through a [`ScrollView`]. A rail longer than the window scrolls
/// rather than dropping the rows past its end — a machine with many volumes
/// must reach every one of them — with a bar carved from its trailing edge
/// while it does. [`row_rect`](Self::row_rect), [`shown_row_rect`] and
/// [`index_at`](Self::index_at) read the one layout, so a click lands on the
/// row the user saw, on whatever part of it shows.
///
/// [`shown_row_rect`]: Self::shown_row_rect
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SidebarView {
    viewport: Rect,
    width: u32,
    row_height: u32,
    separator_height: u32,
    row_count: usize,
    volume_start: Option<usize>,
    /// How far the rows are scrolled, as asked: clamped wherever it is read.
    offset: u64,
    /// The breadth of the bar a rail longer than the window shows.
    breadth: u32,
}

impl SidebarView {
    /// Lay `row_count` rows of `row_height` pixels out down the leading
    /// `width` pixels of `viewport`, with a `separator_height` band before the
    /// row at `volume_start` (the first mounted volume; `None` when nothing is
    /// mounted and there is nothing to separate), scrolled `offset` pixels,
    /// with a bar `breadth` pixels wide while the rows outgrow `viewport`.
    #[must_use]
    pub const fn new(
        viewport: Rect,
        width: u32,
        (row_height, separator_height): (u32, u32),
        row_count: usize,
        volume_start: Option<usize>,
        (offset, breadth): (u64, u32),
    ) -> Self {
        Self {
            viewport,
            width,
            row_height,
            separator_height,
            row_count,
            volume_start,
            offset,
            breadth,
        }
    }

    /// The rail's drawn width: the requested width, clamped so a window
    /// narrower than the rail is filled rather than overrun.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width.min(self.viewport.width)
    }

    /// The whole rail's rectangle — the band the content area is inset by.
    #[must_use]
    pub fn rail_rect(&self) -> Rect {
        Rect::new(
            self.viewport.origin.x,
            self.viewport.origin.y,
            self.width(),
            self.viewport.height,
        )
    }

    /// How tall every row and the separator are together.
    #[must_use]
    pub fn content_height(&self) -> u64 {
        let rows = u64::try_from(self.row_count)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::from(self.row_height));
        match self.volume_start {
            Some(first) if first < self.row_count => {
                rows.saturating_add(u64::from(self.separator_height))
            }
            _ => rows,
        }
    }

    /// The bar beside the rows, carved from the rail's trailing edge, or
    /// `None` while every row fits (or the rail is too narrow to carve one).
    #[must_use]
    pub fn bar_rect(&self) -> Option<Rect> {
        let rail = self.rail_rect();
        let scrolls = self.content_height() > u64::from(rail.height);
        (scrolls && self.breadth > 0 && self.breadth < rail.width).then(|| {
            Rect::new(
                rail.origin
                    .x
                    .saturating_add_unsigned(rail.width - self.breadth),
                rail.origin.y,
                self.breadth,
                rail.height,
            )
        })
    }

    /// Where the rows show: the rail less the bar beside them.
    #[must_use]
    pub fn rows_area(&self) -> Rect {
        let rail = self.rail_rect();
        let bar = self.bar_rect().map_or(0, |bar| bar.width);
        Rect::new(
            rail.origin.x,
            rail.origin.y,
            rail.width.saturating_sub(bar),
            rail.height,
        )
    }

    /// The scroll the bar and the wheel move the rows through, stepping a row a
    /// line.
    #[must_use]
    pub fn scroll_model(&self) -> ScrollModel {
        ScrollModel::in_pixels(
            ScrollRange::new(
                self.content_height(),
                u64::from(self.viewport.height),
                self.offset,
            ),
            u64::from(self.row_height),
        )
    }

    /// The rows' area, scrolled to the clamped offset: what the rows are
    /// painted through and hit-tested against.
    #[must_use]
    pub fn view(&self) -> ScrollView {
        ScrollView::new(
            ScrollOrientation::Vertical,
            self.rows_area(),
            self.scroll_model().offset(),
        )
    }

    /// The top of row `index` below the rail's top, including the separator
    /// band once the volumes begin.
    fn row_top(&self, index: usize) -> Option<u32> {
        let step = u32::try_from(index).ok()?;
        let base = self.row_height.checked_mul(step)?;
        match self.volume_start {
            Some(first) if index >= first => base.checked_add(self.separator_height),
            _ => Some(base),
        }
    }

    /// Where row `index` is laid out, unscrolled, or `None` when there is no
    /// such row.
    #[must_use]
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        if self.row_height == 0 || index >= self.row_count {
            return None;
        }
        let area = self.rows_area();
        Some(Rect::new(
            area.origin.x,
            area.origin.y.checked_add_unsigned(self.row_top(index)?)?,
            area.width,
            self.row_height,
        ))
    }

    /// What the window shows of row `index`, or `None` when none of it shows.
    #[must_use]
    pub fn shown_row_rect(&self, index: usize) -> Option<Rect> {
        self.view().to_window(self.row_rect(index)?)
    }

    /// The half-open range of rows the window shows any part of, found from
    /// the offset rather than by walking the rows, so a paint costs the rows
    /// it draws however many volumes are mounted.
    #[must_use]
    pub fn visible_range(&self) -> Range<usize> {
        if self.row_height == 0 {
            return 0..0;
        }
        let top = self.scroll_model().offset();
        let bottom = top.saturating_add(u64::from(self.rows_area().height));
        self.rows_ending_by(top)..self.rows_starting_before(bottom)
    }

    /// The rail's two runs of equal rows, each as its row count and the depth
    /// of its first row: the user's places from the top, then the volumes
    /// below the separator band.
    fn runs(&self) -> [(usize, u64); 2] {
        let first = self
            .volume_start
            .filter(|&first| first < self.row_count)
            .unwrap_or(self.row_count);
        let band = if first < self.row_count {
            u64::from(self.separator_height)
        } else {
            0
        };
        let volumes = to_u64(first)
            .saturating_mul(u64::from(self.row_height))
            .saturating_add(band);
        [(first, 0), (self.row_count - first, volumes)]
    }

    /// How many rows end at or above depth `y` below the rail's top.
    fn rows_ending_by(&self, y: u64) -> usize {
        let pitch = u64::from(self.row_height);
        self.runs()
            .into_iter()
            .map(|(rows, top)| {
                usize::try_from(y.saturating_sub(top) / pitch).map_or(rows, |whole| whole.min(rows))
            })
            .sum()
    }

    /// How many rows start above depth `y` below the rail's top.
    fn rows_starting_before(&self, y: u64) -> usize {
        let pitch = u64::from(self.row_height);
        self.runs()
            .into_iter()
            .map(|(rows, top)| {
                usize::try_from(y.saturating_sub(top).div_ceil(pitch))
                    .map_or(rows, |started| started.min(rows))
            })
            .sum()
    }

    /// Where the separator band between the user's own places and the mounted
    /// volumes is laid out, unscrolled, or `None` when nothing is mounted.
    #[must_use]
    pub fn separator_rect(&self) -> Option<Rect> {
        let first = self.volume_start?;
        if self.separator_height == 0 || first >= self.row_count {
            return None;
        }
        let top = self.row_height.checked_mul(u32::try_from(first).ok()?)?;
        let area = self.rows_area();
        Some(Rect::new(
            area.origin.x,
            area.origin.y.checked_add_unsigned(top)?,
            area.width,
            self.separator_height,
        ))
    }

    /// The row at window `point`, or `None` for a point outside the rows —
    /// above the rail, on its bar, in the separator band, or below the last
    /// row.
    ///
    /// A row the rail's edge cuts resolves on the part of it that shows.
    #[must_use]
    pub fn index_at(&self, point: Point) -> Option<usize> {
        if self.row_height == 0 {
            return None;
        }
        let at = self.view().to_content(point)?;
        let down = u32::try_from(at.y.checked_sub(self.rows_area().origin.y)?).ok()?;
        let index = match self.volume_start {
            Some(first) => {
                let split = self.row_height.checked_mul(u32::try_from(first).ok()?)?;
                if down < split {
                    usize::try_from(down / self.row_height).ok()?
                } else {
                    // Subtracting the band yields nothing for a point inside
                    // it, so the separation itself is never a row.
                    let below = down.checked_sub(split.checked_add(self.separator_height)?)?;
                    first.checked_add(usize::try_from(below / self.row_height).ok()?)?
                }
            }
            None => usize::try_from(down / self.row_height).ok()?,
        };
        (index < self.row_count).then_some(index)
    }

    /// The offset that shows the whole of row `index` while moving the least.
    #[must_use]
    pub fn reveal(&self, index: usize) -> u64 {
        let model = self.scroll_model();
        match self.row_top(index).filter(|_| index < self.row_count) {
            Some(top) => model
                .revealing(u64::from(top), u64::from(self.row_height))
                .offset(),
            None => model.offset(),
        }
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
