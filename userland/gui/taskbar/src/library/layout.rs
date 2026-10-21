//! Where everything in the launcher is drawn: the panel, its search field,
//! sidebar, scrolled content, and details strip, laid out once so painting
//! and hit-testing read one geometry.

use core::ops::Range;

use alloc::vec::Vec;

use tairix_controls::block::title_height;
use tairix_controls::{ListRow, ScrollOrientation, ScrollView, Tabs, TextField, TileLayout};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, GridFill, GridRun, Point, Rect, Scale};
use tairix_theme::{TextRole, Theme};

use super::chrome_panel;
use super::view::{Section, SectionKind};
use crate::edge::Edge;
use crate::layout::BarLayout;
use crate::popover::{outward, panel_origin, probe_chrome, room_beside};

/// The panel's size in logical pixels at the reference density: room for the
/// sidebar beside six tiles a row, and short enough to open above a bar on a
/// screen 768 pixels tall. A smaller screen gets what it has room for.
pub(crate) const PANEL_WIDTH: u32 = 720;

/// See [`PANEL_WIDTH`].
pub(crate) const PANEL_HEIGHT: u32 = 560;

/// The sidebar's width, in logical pixels.
pub(crate) const SIDEBAR_WIDTH: u32 = 184;

/// The narrowest the content pane may stand beside the sidebar, in logical
/// pixels; a panel that cannot spare it drops the sidebar, and Home still
/// lists every program.
pub(crate) const MIN_CONTENT_WIDTH: u32 = 300;

/// A tile's cell width, in logical pixels: a picture with a two-line name
/// beneath it.
pub(crate) const TILE_WIDTH: u32 = 104;

/// The side a tile's picture is drawn at, in logical pixels.
pub(crate) const TILE_PICTURE: u32 = 48;

/// How many lines a tile's name may wrap over.
pub(crate) const TILE_NAME_LINES: u32 = 2;

/// Where one program sits in the content: its section and its place there.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ItemAt {
    /// The section, as an index into the shown sections.
    pub section: usize,
    /// The program's place in that section.
    pub item: usize,
}

/// One section as laid out, unscrolled: its heading, and the rows its
/// programs stand in beneath it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SectionLayout {
    /// What the section holds.
    pub kind: SectionKind,
    /// The heading's rectangle.
    pub heading: Rect,
    /// How many of its programs are laid out — fewer than it holds where it
    /// shows one row.
    pub count: usize,
    /// The top of its first row.
    top: i32,
    /// Its rows, down from `top`.
    rows: GridRun,
}

/// The computed geometry of the launcher, in screen space.
///
/// Every content rectangle is laid out *unscrolled* — as if the viewport
/// showed the content's top — and shown through [`view`](Self::view). All
/// arithmetic saturates: a pathological screen or scale clips regions, never
/// wraps them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryLayout {
    /// The whole panel, at rest.
    pub panel: Rect,
    /// The corner radius the panel's chrome is drawn with, which the window
    /// manager also takes the silhouette from.
    pub corner_radius: u32,
    /// The point the panel's anchor notch points back at: the centre of the
    /// Library button.
    pub anchor: Point,
    /// The unit step pointing away from the bar, the direction the panel
    /// rises in as it opens.
    pub outward: (i32, i32),
    /// The search field.
    pub search: Rect,
    /// The sidebar, or `None` where the panel has no room for it.
    pub sidebar: Option<Rect>,
    /// The hairline between the sidebar and the content.
    pub divider: Rect,
    /// The content's viewport, scrollbar gutter excluded.
    pub viewport: Rect,
    /// How far the content is scrolled up through the viewport, in pixels.
    pub offset: u32,
    /// The content's whole height.
    pub content_height: u32,
    /// The scrollbar, when the content overflows the viewport.
    pub scrollbar: Option<Rect>,
    /// The details strip at the foot of the panel, beneath its hairline.
    pub footer: Rect,
    /// The hairline over the details strip.
    pub footer_rule: Rect,
    /// The sections, laid out unscrolled.
    pub sections: Vec<SectionLayout>,
    /// How a tile divides its cell.
    pub tile: TileLayout,
    /// How many tiles a row holds.
    pub columns: usize,
    /// The tiles' columns across the content, from `left`.
    cells: GridRun,
    /// The content's left edge.
    left: i32,
    /// The content's width.
    width: u32,
}

/// What a layout is computed from beyond the content itself.
pub(crate) struct Place<'a> {
    pub(crate) edge: Edge,
    pub(crate) bar: &'a BarLayout,
    pub(crate) screen: (u32, u32),
    pub(crate) scale: Scale,
    pub(crate) theme: &'a Theme,
}

impl LibraryLayout {
    /// Lay `sections` out in a panel opened from `place`'s Library button,
    /// scrolled `scroll` pixels, with `sidebar` beside them when it fits.
    pub(crate) fn compute(
        place: &Place<'_>,
        sections: &[Section],
        sidebar: &Tabs,
        scroll: u64,
    ) -> Self {
        let Place { scale, theme, .. } = *place;
        let metrics = theme.metrics();
        let pad = scale.scale_length(metrics.control_gap);
        let frame = Frame::of(place);
        let panes = Panes::split(&frame, sidebar, scale, theme);

        let tile = TileLayout::new(TILE_NAME_LINES);
        let stack_in = |width: u32| Stacking::new(width, scale, theme, tile);
        let mut stacking = stack_in(panes.content_width);
        let mut content_height = stacking.height(sections);
        let scrollable = content_height > frame.body_height;
        let breadth = scale.scale_length(metrics.scrollbar_breadth);
        let gutter = if scrollable {
            breadth.saturating_add(pad)
        } else {
            0
        };
        if scrollable {
            stacking = stack_in(panes.content_width.saturating_sub(gutter));
            content_height = stacking.height(sections);
        }
        let viewport = Rect::new(
            panes.content_x,
            frame.body_top,
            panes.content_width.saturating_sub(gutter),
            frame.body_height,
        );
        let scrollbar = scrollable.then(|| {
            Rect::new(
                viewport.right().saturating_add(to_i32(pad)),
                frame.body_top,
                breadth,
                frame.body_height,
            )
        });
        let offset = u32::try_from(scroll)
            .unwrap_or(u32::MAX)
            .min(content_height.saturating_sub(frame.body_height));

        Self {
            panel: frame.panel,
            corner_radius: scale.scale_length(metrics.window_corner_radius),
            anchor: frame.anchor,
            outward: outward(place.edge),
            search: frame.search,
            sidebar: panes.sidebar,
            divider: panes.divider,
            viewport,
            offset,
            content_height,
            scrollbar,
            footer: frame.footer,
            footer_rule: frame.footer_rule,
            sections: stacking.lay(sections, viewport),
            tile,
            columns: stacking.columns(),
            cells: stacking.run,
            left: viewport.left(),
            width: stacking.width,
        }
    }

    /// The column the content and its scrollbar share, the gap between them
    /// included: what the content may draw across once a scrollbar comes or
    /// goes.
    #[must_use]
    pub fn content_column(&self) -> Rect {
        self.scrollbar.map_or(self.viewport, |bar| {
            let right = bar.right().max(self.viewport.right());
            Rect::new(
                self.viewport.left(),
                self.viewport.top(),
                u32::try_from(right.saturating_sub(self.viewport.left())).unwrap_or(0),
                self.viewport.height,
            )
        })
    }

    /// The viewport, scrolled to where the content is.
    #[must_use]
    pub fn view(&self) -> ScrollView {
        ScrollView::new(
            ScrollOrientation::Vertical,
            self.viewport,
            u64::from(self.offset),
        )
    }

    /// Where the program at `at` is laid out, unscrolled.
    #[must_use]
    pub fn laid(&self, at: ItemAt) -> Option<Rect> {
        self.rect_in(self.sections.get(at.section)?, at.item)
    }

    /// Where `section`'s program `item` is laid out, unscrolled.
    fn rect_in(&self, section: &SectionLayout, item: usize) -> Option<Rect> {
        if item >= section.count {
            return None;
        }
        let (row, x, width) = if section.kind.is_list() {
            (item, 0, self.width)
        } else {
            let columns = self.columns.max(1);
            let x = self.cells.offset(item % columns)?;
            (item / columns, x, self.cells.cell().min(self.width).max(1))
        };
        let y = section.rows.offset(row)?;
        Some(Rect::new(
            self.left.saturating_add(to_i32(x)),
            section.top.saturating_add(to_i32(y)),
            width,
            section.rows.cell(),
        ))
    }

    /// How many programs `section`'s row holds.
    fn per_row(&self, section: &SectionLayout) -> usize {
        if section.kind.is_list() {
            1
        } else {
            self.columns.max(1)
        }
    }

    /// The programs of `section` whose rows reach the unscrolled band from
    /// `top` to `bottom`.
    fn reaching(&self, section: &SectionLayout, top: i32, bottom: i32) -> Range<usize> {
        let top = top.max(section.top);
        if bottom <= top {
            return 0..0;
        }
        let from = u64::try_from(top.saturating_sub(section.top)).unwrap_or(0);
        let extent = u64::try_from(bottom.saturating_sub(top)).unwrap_or(0);
        let rows = section.rows.shown(from, extent);
        let per_row = self.per_row(section);
        let start = rows.start.saturating_mul(per_row).min(section.count);
        start
            ..rows
                .end
                .saturating_mul(per_row)
                .min(section.count)
                .max(start)
    }

    /// Every program of the section at `section`, unscrolled, in reading
    /// order.
    pub fn laid_in(&self, section: usize) -> impl Iterator<Item = (ItemAt, Rect)> + '_ {
        self.sections
            .get(section)
            .into_iter()
            .flat_map(move |laid| {
                (0..laid.count).filter_map(move |item| {
                    Some((ItemAt { section, item }, self.rect_in(laid, item)?))
                })
            })
    }

    /// The program whose cell holds the unscrolled content point `content`,
    /// and that cell.
    #[must_use]
    pub fn cell_at(&self, content: Point) -> Option<(ItemAt, Rect)> {
        self.sections
            .iter()
            .enumerate()
            .find_map(|(section, laid)| {
                let down = u32::try_from(content.y.checked_sub(laid.top)?).ok()?;
                let row = laid.rows.cell_at(down)?;
                let item = if laid.kind.is_list() {
                    row
                } else {
                    let across = u32::try_from(content.x.checked_sub(self.left)?).ok()?;
                    let column = self.cells.cell_at(across)?;
                    row.checked_mul(self.per_row(laid))?.checked_add(column)?
                };
                let rect = self.rect_in(laid, item)?;
                rect.contains(content)
                    .then_some((ItemAt { section, item }, rect))
            })
    }

    /// Where the program at `at` shows on screen, cut to the viewport, or
    /// `None` when none of it does.
    #[must_use]
    pub fn item_rect(&self, at: ItemAt) -> Option<Rect> {
        self.view().to_window(self.laid(at)?)
    }

    /// Every program the viewport shows any part of, in reading order, with
    /// its rectangle laid out unscrolled. Only the rows the viewport reaches
    /// are visited, however long the content.
    pub fn shown(&self) -> impl Iterator<Item = (ItemAt, Rect)> + '_ {
        let view = self.view();
        let top = self.viewport.top().saturating_add(to_i32(self.offset));
        let bottom = top.saturating_add(to_i32(self.viewport.height));
        self.sections
            .iter()
            .enumerate()
            .flat_map(move |(section, laid)| {
                self.reaching(laid, top, bottom).filter_map(move |item| {
                    Some((ItemAt { section, item }, self.rect_in(laid, item)?))
                })
            })
            .filter(move |(_, rect)| view.to_window(*rect).is_some())
    }

    /// Whether the program at `at` is laid out at all — a section showing one
    /// row lays out only what fits.
    #[must_use]
    pub fn holds(&self, at: ItemAt) -> bool {
        self.laid(at).is_some()
    }

    /// The first program laid out, if any.
    #[must_use]
    pub fn first(&self) -> Option<ItemAt> {
        self.sections
            .iter()
            .position(|section| section.count > 0)
            .map(|section| ItemAt { section, item: 0 })
    }

    /// The last program laid out, if any.
    #[must_use]
    pub fn last(&self) -> Option<ItemAt> {
        self.sections
            .iter()
            .enumerate()
            .rev()
            .find_map(|(section, laid)| {
                Some(ItemAt {
                    section,
                    item: laid.count.checked_sub(1)?,
                })
            })
    }

    /// The scroll offset that brings `at` wholly into view, moving as little
    /// as it can; its heading too where it is the first of its section.
    #[must_use]
    pub fn revealing(&self, at: ItemAt) -> Option<u64> {
        let rect = self.laid(at)?;
        let top = if at.item == 0 {
            self.sections.get(at.section)?.heading.top()
        } else {
            rect.top()
        };
        let top = u64::try_from(top.saturating_sub(self.viewport.top())).unwrap_or(0);
        let bottom = u64::try_from(rect.bottom().saturating_sub(self.viewport.top())).unwrap_or(0);
        let offset = u64::from(self.offset);
        let shown = u64::from(self.viewport.height);
        Some(if top < offset {
            top
        } else if bottom > offset.saturating_add(shown) {
            bottom.saturating_sub(shown)
        } else {
            offset
        })
    }

    /// Where the panel's window stands `rise` pixels from rest toward the bar.
    #[must_use]
    pub fn placed_at(&self, rise: i32) -> Point {
        let (x, y) = self.outward;
        Point::new(
            self.panel.left().saturating_sub(x.saturating_mul(rise)),
            self.panel.top().saturating_sub(y.saturating_mul(rise)),
        )
    }
}

/// The details strip's face.
pub(crate) fn footer_font(theme: &Theme, scale: Scale) -> BitmapFont {
    BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale)
}

/// The panel and what runs across the whole of it: the search field at its
/// head, the details strip at its foot, and the body between them.
struct Frame {
    panel: Rect,
    anchor: Point,
    search: Rect,
    footer: Rect,
    footer_rule: Rect,
    inner_x: i32,
    inner_width: u32,
    body_top: i32,
    body_height: u32,
    border: u32,
    pad: u32,
}

impl Frame {
    fn of(place: &Place<'_>) -> Self {
        let Place {
            edge,
            bar,
            screen,
            scale,
            theme,
        } = *place;
        let metrics = theme.metrics();
        let pad = scale.scale_length(metrics.control_gap);
        let (room_width, room_height) = room_beside(edge, bar.bar, screen);
        let width = scale.scale_length(PANEL_WIDTH).min(room_width).max(1);
        let height = scale.scale_length(PANEL_HEIGHT).min(room_height).max(1);
        let library = bar.library;
        let origin = panel_origin(edge, bar.bar, library, width, height);
        let panel = Rect::new(origin.x, origin.y, width, height);
        let anchor = Point::new(
            library.left().saturating_add(to_i32(library.width / 2)),
            library.top().saturating_add(to_i32(library.height / 2)),
        );
        let chrome = probe_chrome(&chrome_panel(Point::ORIGIN), width, scale, theme);
        let inner_x = panel
            .left()
            .saturating_add(chrome.content_left)
            .saturating_add(to_i32(pad));
        let inner_width = chrome.content_width.saturating_sub(pad.saturating_mul(2));
        let content_top = panel.top().saturating_add(chrome.content_top);
        let content_bottom =
            content_top.saturating_add(to_i32(height.saturating_sub(chrome.overhead)));
        let search = Rect::new(
            inner_x,
            content_top.saturating_add(to_i32(pad)),
            inner_width,
            TextField::height(scale, theme),
        );
        let footer_height = footer_font(theme, scale).line_height().saturating_add(pad);
        let border = scale.scale_length(metrics.border_thickness).max(1);
        let footer_top = content_bottom
            .saturating_sub(to_i32(pad))
            .saturating_sub(to_i32(footer_height));
        let body_top = search.bottom().saturating_add(to_i32(pad));
        Self {
            panel,
            anchor,
            search,
            footer: Rect::new(inner_x, footer_top, inner_width, footer_height),
            footer_rule: Rect::new(inner_x, footer_top, inner_width, border),
            inner_x,
            inner_width,
            body_top,
            body_height: u32::try_from(
                footer_top
                    .saturating_sub(to_i32(pad))
                    .saturating_sub(body_top),
            )
            .unwrap_or(0),
            border,
            pad,
        }
    }
}

/// The body split between the sidebar, where it fits, and the content.
struct Panes {
    sidebar: Option<Rect>,
    divider: Rect,
    content_x: i32,
    content_width: u32,
}

impl Panes {
    /// The sidebar stands beside the content only where both fit across the
    /// body and the sidebar's whole strip fits down it.
    fn split(frame: &Frame, sidebar: &Tabs, scale: Scale, theme: &Theme) -> Self {
        let pad = frame.pad;
        let width = scale.scale_length(SIDEBAR_WIDTH);
        let fits_beside = frame.inner_width
            >= width
                .saturating_add(pad)
                .saturating_add(scale.scale_length(MIN_CONTENT_WIDTH));
        let fits_down = sidebar.measured_height(scale, theme) <= frame.body_height;
        if !(fits_beside && fits_down) {
            return Self {
                sidebar: None,
                divider: Rect::EMPTY,
                content_x: frame.inner_x,
                content_width: frame.inner_width,
            };
        }
        let strip = Rect::new(frame.inner_x, frame.body_top, width, frame.body_height);
        Self {
            sidebar: Some(strip),
            divider: Rect::new(
                strip.right().saturating_add(to_i32(pad / 2)),
                frame.body_top,
                frame.border,
                frame.body_height,
            ),
            content_x: strip.right().saturating_add(to_i32(pad)),
            content_width: frame.inner_width.saturating_sub(width.saturating_add(pad)),
        }
    }
}

/// How sections stack down a content pane of one width: the shared grid of
/// tiles, the two-line rows, and the headings between them.
struct Stacking {
    run: GridRun,
    width: u32,
    tile_height: u32,
    row_height: u32,
    heading: u32,
    gap: u32,
}

impl Stacking {
    fn new(width: u32, scale: Scale, theme: &Theme, tile: TileLayout) -> Self {
        let gap = scale.scale_length(theme.metrics().control_gap);
        let cell = scale.scale_length(TILE_WIDTH).min(width.max(1));
        Self {
            run: GridRun::new(width, cell, gap, GridFill::Spread),
            width,
            tile_height: tile.height_for(scale.scale_length(TILE_PICTURE), scale, theme),
            row_height: ListRow::captioned_height(scale, theme),
            heading: title_height(scale, theme),
            gap,
        }
    }

    fn columns(&self) -> usize {
        self.run.count().max(1)
    }

    /// How many of `section`'s programs are laid out.
    fn shown(&self, section: &Section) -> usize {
        match section.kind {
            SectionKind::Frequent => section.items.len().min(self.columns()),
            _ => section.items.len(),
        }
    }

    /// The rows `section`'s programs stand in beneath its heading.
    fn rows(&self, section: &Section) -> GridRun {
        let shown = self.shown(section);
        if section.kind.is_list() {
            GridRun::fixed(shown, self.row_height, 0)
        } else {
            GridRun::fixed(shown.div_ceil(self.columns()), self.tile_height, self.gap)
        }
    }

    /// How tall `section`'s programs stand beneath its heading.
    fn body(&self, section: &Section) -> u32 {
        u32::try_from(self.rows(section).span()).unwrap_or(u32::MAX)
    }

    fn height(&self, sections: &[Section]) -> u32 {
        let stacked = sections.iter().fold(0u32, |total, section| {
            total
                .saturating_add(self.heading)
                .saturating_add(self.body(section))
                .saturating_add(self.gap.saturating_mul(2))
        });
        stacked.saturating_sub(self.gap.saturating_mul(2).min(stacked))
    }

    /// Stack `sections` down `viewport` from its top.
    fn lay(&self, sections: &[Section], viewport: Rect) -> Vec<SectionLayout> {
        let mut y = viewport.top();
        sections
            .iter()
            .map(|section| {
                let heading = Rect::new(viewport.left(), y, self.width, self.heading);
                let items_top = y.saturating_add(to_i32(self.heading));
                y = items_top
                    .saturating_add(to_i32(self.body(section)))
                    .saturating_add(to_i32(self.gap.saturating_mul(2)));
                SectionLayout {
                    kind: section.kind,
                    heading,
                    count: self.shown(section),
                    top: items_top,
                    rows: self.rows(section),
                }
            })
            .collect()
    }
}
