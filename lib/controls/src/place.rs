//! The place chooser (spec §11.47): a search field over a list of places,
//! each row naming the place, where it is, and a reading its owner keeps
//! current — the local time and offset there now.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{
    ClickKind, DoubleClickTracker, InputEvent, Key, Modifiers, NamedKey, PointerButton,
};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};
use tairix_util::utf8::folded;

use crate::button::{Button, ButtonContent};
use crate::damage;
use crate::decision::{Dialog, DialogAction};
use crate::paint::{
    foreground, paint_row, paint_run, role_font, text_plate_height, withheld, ChromeLayer,
};
use crate::scroll::{ScrollModel, ScrollOrientation, ScrollRange, ScrollView};
use crate::scrollbar::{ScrollAction, ScrollBar};
use crate::state::{ControlRole, ControlState, PointerState, RenderInvariant, SelectionState};
use crate::text::{SearchField, TextAction};

/// What the list says when the query matches no place.
pub const NO_MATCH: &str = "No place matches.";

/// One place the chooser offers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceRow {
    /// The place's own name: its city.
    pub name: String,
    /// Where it is: its region, and whatever tells it from its neighbours.
    pub region: String,
    /// The reading its owner keeps current, drawn at the row's end.
    pub reading: String,
    /// Matched by the search but never drawn, such as a zone's IANA name.
    pub key: String,
}

/// What a [`PlaceChooser`] asks of its owner.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PlaceAction {
    /// The highlight moved to this row of the owner's list.
    Moved {
        /// Its index in the rows the chooser was built with.
        row: usize,
    },
    /// This row was chosen: Enter on it, or a double-click.
    Chose {
        /// Its index in the rows the chooser was built with.
        row: usize,
    },
    /// The query changed and the list now shows only what it matches.
    Searched,
    /// Escape with no query to clear: the owner closes the chooser.
    Cancelled,
}

/// When a press landed, and the desktop's interval for pairing two presses on
/// one row into a double-click.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PressTiming {
    /// The monotonic instant of the event, in nanoseconds.
    pub now_ns: u64,
    /// The double-click interval the desktop publishes.
    pub double_click: Duration64,
}

/// What one part of the chooser made of an event.
enum Routed {
    /// It was not the part's alone: the next part sees it too.
    Passed,
    /// It was the part's alone, answering this.
    Taken(Option<PlaceAction>),
}

/// Where and how the chooser is drawn, handed to the parts of an event.
struct Ctx<'a> {
    bounds: Rect,
    scale: Scale,
    theme: &'a Theme,
}

/// The chooser's layout within its bounds, one for painting, hitting and
/// damage alike.
#[derive(Copy, Clone)]
struct Geom {
    search: Rect,
    /// The rows' viewport, short of the scrollbar's gutter.
    list: Rect,
    bar: Option<Rect>,
    row: u32,
}

impl Geom {
    fn view(&self, offset: u32) -> ScrollView {
        ScrollView::new(ScrollOrientation::Vertical, self.list, u64::from(offset))
    }

    /// The `index`th shown row's rectangle in the list's unscrolled layout.
    fn row_rect(&self, index: usize) -> Rect {
        let down = u32::try_from(index)
            .unwrap_or(u32::MAX)
            .saturating_mul(self.row);
        Rect::new(
            self.list.left(),
            self.list.top().saturating_add(to_i32(down)),
            self.list.width,
            self.row,
        )
    }

    /// The list and its gutter: what a change to the whole list repaints.
    fn report_list(&self, damage: &mut Region) {
        damage.add(self.list);
        if let Some(bar) = self.bar {
            damage.add(bar);
        }
    }
}

/// A search field over a list of places, narrowed as the reader types.
///
/// The reader types into the search field while Up and Down walk the
/// highlight through what the query matches, so the keyboard never leaves the
/// query; a row is chosen by Enter or by a double-click. Matching ignores case
/// and the accents a Latin-1 name carries, and a place whose name begins with
/// the query is listed before one that merely contains it. The rows are the
/// owner's, in its order, and each row's reading is the owner's to keep
/// current ([`set_reading`](Self::set_reading)).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceChooser {
    search: SearchField,
    rows: Vec<PlaceRow>,
    /// Each row's name, folded for matching.
    names: Vec<String>,
    /// Each row's region and key, folded for matching.
    rests: Vec<String>,
    /// The rows the query matches, as indices into `rows`, in the order shown.
    shown: Vec<usize>,
    /// The highlighted row, an index into `rows`; one of `shown` when set.
    highlight: Option<usize>,
    /// The row under the pointer, an index into `rows`.
    hovered: Option<usize>,
    /// How far the list is scrolled, in pixels.
    scroll: u32,
    /// Whether the highlight is to be scrolled into view once the bounds are
    /// known.
    reveal: bool,
    bar: ScrollBar,
    state: ControlState,
    pointer: RenderInvariant<Point>,
    clicks: RenderInvariant<DoubleClickTracker>,
    /// Whether the press under way began in the search field.
    pressing_search: RenderInvariant<bool>,
}

impl PlaceChooser {
    /// A chooser over `rows`, in their order, with an empty query.
    #[must_use]
    pub fn new(rows: Vec<PlaceRow>) -> Self {
        let names = rows.iter().map(|row| folded(&row.name).collect()).collect();
        let rests = rows
            .iter()
            .map(|row| {
                folded(&row.region)
                    .chain(core::iter::once(' '))
                    .chain(folded(&row.key))
                    .collect()
            })
            .collect();
        let shown = (0..rows.len()).collect();
        Self {
            search: SearchField::new(),
            rows,
            names,
            rests,
            shown,
            highlight: None,
            hovered: None,
            scroll: 0,
            reveal: false,
            bar: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::new(0, 0, 0), 1),
            ),
            state: ControlState::idle(),
            pointer: RenderInvariant::new(Point::ORIGIN),
            clicks: RenderInvariant::new(DoubleClickTracker::new()),
            pressing_search: RenderInvariant::new(false),
        }
    }

    /// This chooser's search field showing `placeholder` while it is empty.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.search = self.search.with_placeholder(placeholder);
        self
    }

    /// This chooser with `row` highlighted and scrolled into view, such as
    /// the place already chosen. A row past the list highlights nothing.
    #[must_use]
    pub fn with_highlight(mut self, row: usize) -> Self {
        if row < self.rows.len() {
            self.highlight = Some(row);
            self.reveal = true;
        }
        self
    }

    /// The rows it was built with.
    #[must_use]
    pub fn rows(&self) -> &[PlaceRow] {
        &self.rows
    }

    /// The highlighted row, an index into [`rows`](Self::rows).
    #[must_use]
    pub const fn highlighted(&self) -> Option<usize> {
        self.highlight
    }

    /// The rows the query matches, as indices into [`rows`](Self::rows), in
    /// the order they are shown.
    #[must_use]
    pub fn shown(&self) -> &[usize] {
        &self.shown
    }

    /// The query.
    #[must_use]
    pub fn query(&self) -> &str {
        self.search.text()
    }

    /// The chooser's composed state.
    #[must_use]
    pub const fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the composed state; the search field and the scrollbar follow
    /// its enablement.
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
        self.search.set_state(state);
        self.bar
            .set_state(ControlState::idle().with_enabled(state.enabled));
    }

    /// Set whether it holds the keyboard, whose caret is the search field's.
    pub fn set_focused(&mut self, focused: bool) {
        self.state.focus.focused = focused;
        self.search.set_focused(focused);
    }

    /// Replace `row`'s reading, reporting the row where it is shown.
    pub fn set_reading(
        &mut self,
        row: usize,
        reading: impl Into<String>,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.set_readings(
            core::iter::once((row, reading.into())),
            bounds,
            scale,
            theme,
            damage,
        );
    }

    /// Replace each `(row, reading)` of `readings`, laid out once however
    /// many there are, reporting each row whose reading changed where it is
    /// shown.
    pub fn set_readings(
        &mut self,
        readings: impl IntoIterator<Item = (usize, String)>,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let geom = self.geom(bounds, scale, theme);
        for (row, reading) in readings {
            let Some(place) = self.rows.get_mut(row) else {
                continue;
            };
            if place.reading != reading {
                place.reading = reading;
                self.report_row(row, &geom, damage);
            }
        }
    }

    /// The height of one row: the name's line, and the region's beneath it.
    #[must_use]
    pub fn row_height(scale: Scale, theme: &Theme) -> u32 {
        let pad = scale.scale_length(theme.metrics().control_inset);
        role_font(theme, scale, TextRole::Body)
            .line_height()
            .saturating_add(role_font(theme, scale, TextRole::Caption).line_height())
            .saturating_add(pad.saturating_mul(2))
            .max(1)
    }

    /// The height that shows `rows` rows beneath the search field.
    #[must_use]
    pub fn height_for(rows: u32, scale: Scale, theme: &Theme) -> u32 {
        text_plate_height(theme, scale, TextRole::Body)
            .saturating_add(scale.scale_length(theme.metrics().control_gap))
            .saturating_add(Self::row_height(scale, theme).saturating_mul(rows))
    }

    fn geom(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Geom {
        let search_h = text_plate_height(theme, scale, TextRole::Body).min(bounds.height);
        let gap = scale.scale_length(theme.metrics().control_gap);
        let top = search_h.saturating_add(gap).min(bounds.height);
        let list_h = bounds.height - top;
        let list_top = bounds.top().saturating_add(to_i32(top));
        let row = Self::row_height(scale, theme);
        let content = self.content(row);
        let breadth = scale.scale_length(theme.metrics().scrollbar_breadth);
        let bar = (content > list_h && bounds.width > breadth.saturating_mul(2)).then(|| {
            Rect::new(
                bounds.right().saturating_sub(to_i32(breadth)),
                list_top,
                breadth,
                list_h,
            )
        });
        Geom {
            search: Rect::new(bounds.left(), bounds.top(), bounds.width, search_h),
            list: Rect::new(
                bounds.left(),
                list_top,
                bounds.width - bar.map_or(0, |bar| bar.width),
                list_h,
            ),
            bar,
            row,
        }
    }

    /// The height every shown row takes together.
    fn content(&self, row: u32) -> u32 {
        u32::try_from(self.shown.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(row)
    }

    /// The scroll model over `geom` at `offset`, clamped to what it can show.
    fn model(&self, geom: &Geom, offset: u32) -> ScrollModel {
        ScrollModel::in_pixels(
            ScrollRange::new(
                u64::from(self.content(geom.row)),
                u64::from(geom.list.height),
                u64::from(offset),
            ),
            u64::from(geom.row),
        )
    }

    /// The offset the list is drawn at: the highlight brought into view if
    /// that is owed, and never past the end.
    fn offset(&self, geom: &Geom) -> u32 {
        let model = self.model(geom, self.scroll);
        let model = match self.highlight.and_then(|row| self.position(row)) {
            Some(at) if self.reveal => {
                let start = u64::from(geom.row) * u64::try_from(at).unwrap_or(u64::MAX);
                model.revealing(start, u64::from(geom.row))
            }
            _ => model,
        };
        u32::try_from(model.offset()).unwrap_or(u32::MAX)
    }

    /// Settle the owed reveal now the bounds are known; answers the layout.
    fn settle(&mut self, bounds: Rect, scale: Scale, theme: &Theme) -> Geom {
        let geom = self.geom(bounds, scale, theme);
        self.scroll = self.offset(&geom);
        self.reveal = false;
        geom
    }

    /// Where `row` stands among those shown.
    fn position(&self, row: usize) -> Option<usize> {
        self.shown.iter().position(|&shown| shown == row)
    }

    /// The shown row under window `point`, an index into `rows`.
    fn row_at(&self, geom: &Geom, point: Point) -> Option<usize> {
        let inside = geom.view(self.offset(geom)).to_content(point)?;
        let down = u32::try_from(inside.y.saturating_sub(geom.list.top())).ok()?;
        self.shown
            .get(usize::try_from(down / geom.row).ok()?)
            .copied()
    }

    fn report_row(&self, row: usize, geom: &Geom, damage: &mut Region) {
        let Some(at) = self.position(row) else {
            return;
        };
        let mut layout = damage::sink();
        layout.add(geom.row_rect(at));
        geom.view(self.offset(geom)).report(&layout, damage);
    }

    /// Narrow the list to what the query matches: names that begin with it,
    /// then the rest, each in the owner's order. The highlight stays where it
    /// still shows and otherwise moves to the first match.
    fn refilter(&mut self) {
        let query: String = folded(self.search.text().trim()).collect();
        let leads = |row: &usize| self.names[*row].starts_with(&query);
        let holds = |row: &usize| {
            !leads(row) && (self.names[*row].contains(&query) || self.rests[*row].contains(&query))
        };
        let shown: Vec<usize> = (0..self.rows.len())
            .filter(leads)
            .chain((0..self.rows.len()).filter(holds))
            .collect();
        self.shown = shown;
        if self.highlight.and_then(|row| self.position(row)).is_none() {
            self.highlight = self.shown.first().copied();
        }
        self.scroll = 0;
        self.reveal = true;
        self.hovered = None;
    }

    /// The query was edited: narrow the list and repaint it.
    fn searched(&mut self, bounds: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        let before = self.geom(bounds, scale, theme);
        self.refilter();
        let after = self.settle(bounds, scale, theme);
        before.report_list(damage);
        after.report_list(damage);
    }

    /// Move the highlight to `row`, reporting both rows and bringing it into
    /// view; answers the move.
    fn highlight_row(
        &mut self,
        row: usize,
        geom: &Geom,
        damage: &mut Region,
    ) -> Option<PlaceAction> {
        if self.highlight == Some(row) {
            return None;
        }
        if let Some(old) = self.highlight.replace(row) {
            self.report_row(old, geom, damage);
        }
        self.reveal = true;
        let shown = self.offset(geom);
        self.reveal = false;
        if shown == self.scroll {
            self.report_row(row, geom, damage);
        } else {
            self.scroll = shown;
            geom.report_list(damage);
        }
        Some(PlaceAction::Moved { row })
    }

    /// Show `row` as the one under the pointer.
    fn hover(&mut self, row: Option<usize>, geom: &Geom, damage: &mut Region) {
        if self.hovered == row {
            return;
        }
        for changed in [self.hovered, row].into_iter().flatten() {
            self.report_row(changed, geom, damage);
        }
        self.hovered = row;
    }

    fn scroll_to(&mut self, geom: &Geom, offset: u64, damage: &mut Region) {
        let offset =
            u32::try_from(self.model(geom, 0).scroll_to(offset).offset()).unwrap_or(u32::MAX);
        if offset != self.scroll {
            self.scroll = offset;
            geom.report_list(damage);
        }
    }

    /// Feed a pointer event. Presses on one row pair into a double-click by
    /// the desktop's rule ([`DoubleClickTracker`]); the wheel scrolls the
    /// list wherever the pointer is over the chooser.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        timing: PressTiming,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PlaceAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        let ctx = Ctx {
            bounds,
            scale,
            theme,
        };
        let geom = self.settle(bounds, scale, theme);
        if let InputEvent::PointerScrolled { dx, dy } = event {
            self.wheel((*dx, *dy), &geom, &ctx, damage);
            return None;
        }
        if let Routed::Taken(answer) = self.search_pointer(event, &geom, &ctx, damage) {
            return answer;
        }
        if self.bar_pointer(event, &geom, &ctx, damage) || !self.state.is_actionable() {
            return None;
        }
        self.rows_pointer(event, timing, &geom, damage)
    }

    /// Scroll the list by a wheel's `delta` while the pointer is over it.
    fn wheel(&mut self, (dx, dy): (i32, i32), geom: &Geom, ctx: &Ctx<'_>, damage: &mut Region) {
        let point = *self.pointer;
        let Some(bar) = geom.bar.filter(|_| ctx.bounds.contains(point)) else {
            return;
        };
        self.bar.set_model(self.model(geom, self.scroll));
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.bar.wheel(dx, dy, ctx.scale, bar, damage)
        {
            self.scroll_to(geom, offset, damage);
            let under = self.row_at(geom, point);
            self.hover(under, geom, damage);
        }
    }

    /// The search field's share of `event`: its presses, its drag, and every
    /// move so its hover follows.
    fn search_pointer(
        &mut self,
        event: &InputEvent,
        geom: &Geom,
        ctx: &Ctx<'_>,
        damage: &mut Region,
    ) -> Routed {
        let moved = matches!(event, InputEvent::PointerMoved { .. });
        let on_search = geom.search.contains(*self.pointer);
        let held = *self.pressing_search;
        if !(on_search || held || moved) {
            return Routed::Passed;
        }
        if matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        ) {
            *self.pressing_search = on_search;
        }
        let edited = self
            .search
            .on_pointer(event, geom.search, ctx.scale, ctx.theme, damage);
        if matches!(
            event,
            InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled
        ) {
            *self.pressing_search = false;
        }
        if edited == Some(TextAction::Edited) {
            self.searched(ctx.bounds, ctx.scale, ctx.theme, damage);
            return Routed::Taken(Some(PlaceAction::Searched));
        }
        if (on_search || held) && !moved {
            self.clicks.reset();
            return Routed::Taken(None);
        }
        Routed::Passed
    }

    /// The scrollbar's share of `event`: its own parts and any drag it holds.
    /// Answers whether the event was the bar's alone.
    fn bar_pointer(
        &mut self,
        event: &InputEvent,
        geom: &Geom,
        ctx: &Ctx<'_>,
        damage: &mut Region,
    ) -> bool {
        let Some(bar) = geom.bar else {
            return false;
        };
        let moved = matches!(event, InputEvent::PointerMoved { .. });
        let on_bar = bar.contains(*self.pointer);
        if !(on_bar || self.bar.is_pressing() || moved) {
            return false;
        }
        self.bar.set_model(self.model(geom, self.scroll));
        if let Some(ScrollAction::ScrollTo { offset }) = self
            .bar
            .on_pointer(event, bar, ctx.scale, ctx.theme, damage)
        {
            self.scroll_to(geom, offset, damage);
        }
        if !(on_bar || self.bar.is_pressing()) {
            return false;
        }
        if !moved {
            self.clicks.reset();
        }
        self.hover(None, geom, damage);
        true
    }

    /// The rows' share of `event`: the hover, and a press that highlights a
    /// row or completes a double-click on it.
    fn rows_pointer(
        &mut self,
        event: &InputEvent,
        timing: PressTiming,
        geom: &Geom,
        damage: &mut Region,
    ) -> Option<PlaceAction> {
        let under = self.row_at(geom, *self.pointer);
        match event {
            InputEvent::PointerMoved { .. } => {
                self.hover(under, geom, damage);
                None
            }
            InputEvent::PointerPressed { button } => {
                let Some(row) = under else {
                    self.clicks.reset();
                    return None;
                };
                let paired = self.clicks.register(
                    timing.now_ns,
                    u64::try_from(row).unwrap_or(u64::MAX),
                    *button,
                    timing.double_click,
                );
                if *button != PointerButton::Primary {
                    return None;
                }
                let moved = self.highlight_row(row, geom, damage);
                if paired == ClickKind::Double {
                    return Some(PlaceAction::Chose { row });
                }
                moved
            }
            InputEvent::PointerCancelled => {
                self.hover(None, geom, damage);
                None
            }
            _ => None,
        }
    }

    /// The pointer left the chooser: whatever it lit goes out.
    pub fn pointer_left(&mut self, bounds: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        let geom = self.geom(bounds, scale, theme);
        self.hover(None, &geom, damage);
        let away = InputEvent::PointerMoved {
            to: Point::new(i32::MIN, i32::MIN),
        };
        let _ = self
            .search
            .on_pointer(&away, geom.search, scale, theme, damage);
        self.bar.set_model(self.model(&geom, self.scroll));
        if let Some(bar) = geom.bar {
            let _ = self.bar.on_pointer(&away, bar, scale, theme, damage);
        }
    }

    /// Feed a key. Up and Down walk the highlight through what is shown and
    /// the Page keys a viewport's worth, stopping at either end; Enter chooses
    /// the highlighted row; Escape clears the query, then cancels; Tab answers
    /// nothing, so the owner can move the keyboard on. Everything else edits
    /// the query.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PlaceAction> {
        if !self.state.focus.focused || !self.state.is_actionable() {
            return None;
        }
        let geom = self.settle(bounds, scale, theme);
        let at = self.highlight.and_then(|row| self.position(row));
        let page = usize::try_from(geom.list.height / geom.row)
            .unwrap_or(1)
            .max(1);
        let last = self.shown.len().saturating_sub(1);
        let target = match key {
            Key::Named(NamedKey::Down) => Some(at.map_or(0, |at| (at + 1).min(last))),
            Key::Named(NamedKey::Up) => Some(at.map_or(last, |at| at.saturating_sub(1))),
            Key::Named(NamedKey::PageDown) => Some(at.map_or(0, |at| (at + page).min(last))),
            Key::Named(NamedKey::PageUp) => Some(at.map_or(0, |at| at.saturating_sub(page))),
            Key::Named(NamedKey::Enter) => {
                return at.and(self.highlight).map(|row| PlaceAction::Chose { row });
            }
            Key::Named(NamedKey::Tab) => return None,
            Key::Named(NamedKey::Escape) if !self.search.has_query() => {
                return Some(PlaceAction::Cancelled);
            }
            _ => None,
        };
        if let Some(target) = target {
            let row = *self.shown.get(target)?;
            return self.highlight_row(row, &geom, damage);
        }
        match self.search.on_key(key, modifiers, geom.search, damage)? {
            TextAction::Edited => {
                self.searched(bounds, scale, theme, damage);
                Some(PlaceAction::Searched)
            }
            TextAction::Cancelled => Some(PlaceAction::Cancelled),
            TextAction::Submitted => None,
        }
    }

    /// Replace the query's selection with `text`, as a paste does.
    pub fn insert_text(
        &mut self,
        text: &str,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PlaceAction> {
        let geom = self.geom(bounds, scale, theme);
        self.search.insert_text(text, geom.search, damage)?;
        self.searched(bounds, scale, theme, damage);
        Some(PlaceAction::Searched)
    }

    /// Select the whole query, as a select-all chord does.
    pub fn select_all(&mut self, bounds: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        let geom = self.geom(bounds, scale, theme);
        self.search.select_all(geom.search, damage);
    }

    /// Paint the chooser into `surface` at `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let geom = self.geom(bounds, scale, theme);
        let offset = self.offset(&geom);
        self.search.render(surface, geom.search, scale, theme);
        let body = role_font(theme, scale, TextRole::Body);
        let caption = role_font(theme, scale, TextRole::Caption);
        let view = geom.view(offset);
        if self.shown.is_empty() {
            view.paint(surface, |surface| {
                let run = body.elide_to_width(NO_MATCH, geom.list.width);
                let x = geom.list.left().saturating_add(to_i32(
                    geom.list.width.saturating_sub(body.text_width(run.0)) / 2,
                ));
                let y = body.centred_top(geom.list.top(), geom.row);
                paint_run(
                    surface,
                    body,
                    run,
                    (x, y),
                    Color::from(theme.palette().on_surface_muted),
                    None,
                );
            });
        } else {
            view.paint(surface, |surface| {
                let first = usize::try_from(offset / geom.row).unwrap_or(usize::MAX);
                let visible = usize::try_from(geom.list.height.div_ceil(geom.row))
                    .unwrap_or(usize::MAX)
                    .saturating_add(1);
                for (at, &row) in self.shown.iter().enumerate().skip(first).take(visible) {
                    self.paint_place(
                        surface,
                        row,
                        geom.row_rect(at),
                        (body, caption),
                        (scale, theme),
                    );
                }
            });
        }
        if let Some(bar) = geom.bar {
            let mut drawn = self.bar;
            drawn.set_model(self.model(&geom, offset));
            drawn.render(surface, bar, scale, theme);
        }
    }

    fn paint_place(
        &self,
        surface: &mut Surface,
        row: usize,
        rect: Rect,
        (body, caption): (BitmapFont, BitmapFont),
        (scale, theme): (Scale, &Theme),
    ) {
        let Some(place) = self.rows.get(row) else {
            return;
        };
        let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
            return;
        };
        let state = ControlState::idle()
            .with_enabled(self.state.enabled)
            .with_selection(SelectionState::of(self.highlight == Some(row)))
            .with_pointer(if self.hovered == Some(row) && self.state.is_actionable() {
                PointerState::Hover
            } else {
                PointerState::None
            });
        let Some((cx, cy, cw, _)) = paint_row(
            surface,
            (x, y, rect.width, rect.height),
            scale,
            theme,
            state,
            ChromeLayer::Inlay,
        ) else {
            return;
        };
        let pad = scale.scale_length(theme.metrics().control_inset);
        let gap = scale.scale_length(theme.metrics().control_gap);
        let ink = foreground(theme, state.disposition());
        let muted = Color::from(theme.palette().on_surface_muted);
        let reading_w = body.text_width(&place.reading).min(cw / 2);
        let name_w = cw.saturating_sub(reading_w).saturating_sub(gap);
        let top = to_i32(cy.saturating_add(pad));
        let below = top.saturating_add(to_i32(body.line_height()));
        paint_run(
            surface,
            body,
            body.elide_to_width(&place.name, name_w),
            (to_i32(cx), top),
            ink,
            None,
        );
        paint_run(
            surface,
            caption,
            caption.elide_to_width(&place.region, cw),
            (to_i32(cx), below),
            muted,
            None,
        );
        paint_run(
            surface,
            body,
            body.elide_to_width(&place.reading, reading_w),
            (to_i32(cx.saturating_add(cw - reading_w)), top),
            muted,
            None,
        );
    }
}

/// `value` as a coordinate, saturating.
fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// What a [`PlaceDialog`] concluded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PlaceOutcome {
    /// This row was chosen, by Choose, Enter or a double-click.
    Chose {
        /// Its index in the rows the chooser was built with.
        row: usize,
    },
    /// The reader cancelled, by Cancel or Escape.
    Cancelled,
}

/// The index of a [`PlaceDialog`]'s Cancel action.
const CANCEL: usize = 0;

/// The index of a [`PlaceDialog`]'s Choose action.
const CHOOSE: usize = 1;

/// The width a [`PlaceDialog`]'s window opens at, in logical pixels.
const OPENING_WIDTH: u32 = 440;

/// How many places a [`PlaceDialog`]'s window opens showing.
const OPENING_PLACES: u32 = 8;

/// A [`PlaceChooser`] above Cancel and Choose: the client of the dialog
/// window an application asks for one place in, whose frame carries the
/// title.
///
/// The chooser keeps the keyboard, so Enter chooses and Escape clears the
/// query, then cancels; Choose is offered only while a place is highlighted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceDialog {
    dialog: Dialog,
    chooser: PlaceChooser,
}

impl PlaceDialog {
    /// The client of a dialog window titled `title` around `chooser`.
    #[must_use]
    pub fn new(title: impl Into<String>, chooser: PlaceChooser) -> Self {
        let mut chooser = chooser;
        chooser.set_focused(true);
        let mut dialog = Self {
            dialog: Dialog::new(title).in_window().with_actions(vec![
                Button::labelled("Cancel"),
                Button::new(
                    ButtonContent::Label(String::from("Choose")),
                    ControlRole::Recommended,
                ),
            ]),
            chooser,
        };
        dialog.offer_choose();
        dialog
    }

    /// This dialog with `message` above the chooser.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.dialog = self.dialog.with_message(message);
        self
    }

    /// The title its window opens under.
    #[must_use]
    pub fn title(&self) -> &str {
        self.dialog.title()
    }

    /// The chooser.
    #[must_use]
    pub const fn chooser(&self) -> &PlaceChooser {
        &self.chooser
    }

    /// The chooser, for a reading its owner keeps current.
    pub fn chooser_mut(&mut self) -> &mut PlaceChooser {
        &mut self.chooser
    }

    /// Where the chooser is drawn within `bounds`.
    #[must_use]
    pub fn chooser_rect(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
        self.dialog.content_rect(bounds, scale, theme)
    }

    /// The height a dialog `width` pixels wide needs to show `rows` places.
    #[must_use]
    pub fn height_for(&self, rows: u32, width: u32, scale: Scale, theme: &Theme) -> u32 {
        self.dialog.height_for_content(
            PlaceChooser::height_for(rows, scale, theme),
            width,
            scale,
            theme,
        )
    }

    /// The extent its window opens at: wide enough for a place's name and its
    /// reading side by side, and tall enough for several places.
    #[must_use]
    pub fn opening_extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let width = scale.scale_length(OPENING_WIDTH);
        (width, self.height_for(OPENING_PLACES, width, scale, theme))
    }

    /// Paint the dialog and the chooser in it.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        self.dialog.render(surface, bounds, scale, theme);
        if let Some(content) = self.chooser_rect(bounds, scale, theme) {
            self.chooser.render(surface, content, scale, theme);
        }
    }

    /// Feed a pointer event: the actions first, then the chooser.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        timing: PressTiming,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PlaceOutcome> {
        match self.dialog.on_pointer(event, bounds, scale, theme, damage) {
            Some(DialogAction::ActionActivated { index: CANCEL }) => {
                return Some(PlaceOutcome::Cancelled);
            }
            Some(DialogAction::ActionActivated { index: CHOOSE }) => {
                return self
                    .chooser
                    .highlighted()
                    .map(|row| PlaceOutcome::Chose { row });
            }
            _ => {}
        }
        let content = self.chooser_rect(bounds, scale, theme)?;
        let answer = self
            .chooser
            .on_pointer(event, timing, content, scale, theme, damage);
        self.answer(answer, bounds, scale, theme, damage)
    }

    /// Feed a key to the chooser, which holds the keyboard.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PlaceOutcome> {
        let content = self.chooser_rect(bounds, scale, theme)?;
        let answer = self
            .chooser
            .on_key(key, modifiers, content, scale, theme, damage);
        self.answer(answer, bounds, scale, theme, damage)
    }

    /// The pointer left the dialog: whatever it lit goes out.
    pub fn pointer_left(&mut self, bounds: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        let away = InputEvent::PointerMoved {
            to: Point::new(i32::MIN, i32::MIN),
        };
        let _ = self.dialog.on_pointer(&away, bounds, scale, theme, damage);
        if let Some(content) = self.chooser_rect(bounds, scale, theme) {
            self.chooser.pointer_left(content, scale, theme, damage);
        }
    }

    fn answer(
        &mut self,
        answer: Option<PlaceAction>,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PlaceOutcome> {
        match answer? {
            PlaceAction::Chose { row } => Some(PlaceOutcome::Chose { row }),
            PlaceAction::Cancelled => Some(PlaceOutcome::Cancelled),
            PlaceAction::Moved { .. } | PlaceAction::Searched => {
                if self.offer_choose() {
                    if let Some(rect) = self.dialog.action_rects(bounds, scale, theme).get(CHOOSE) {
                        damage.add(*rect);
                    }
                }
                None
            }
        }
    }

    /// Offer Choose exactly while a place is highlighted; answers whether
    /// that changed.
    fn offer_choose(&mut self) -> bool {
        let offered = self.chooser.highlighted().is_some();
        let Some(choose) = self.dialog.actions_mut().get_mut(CHOOSE) else {
            return false;
        };
        let state = choose.state();
        if state.enabled == offered {
            return false;
        }
        choose.set_state(state.with_enabled(offered));
        true
    }
}
