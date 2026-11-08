//! An application's settings window: its categories down a sidebar, the one
//! chosen beside it as a column of form groups, and a footer of commands —
//! *Restore defaults* — with a line saying what the store last refused.
//!
//! The frame holds no setting. Its owner builds each category's groups from
//! the record it keeps, answers [`SettingsAction::Shown`] with
//! [`SettingsFrame::show`], and carries each row's action into that record.
//!
//! - **The keyboard walks three regions**: Tab and Shift+Tab go from the
//!   sidebar to the column to each command and round, and a number or colour
//!   row the cursor leaves settles. Escape with nothing to take back asks to
//!   close.
//! - **A primary press gives the keyboard** to the region it lands in.
//! - **The column scrolls** when it outgrows the window, by the wheel, its
//!   scrollbar, and the keyboard cursor, which is kept in view. The window is
//!   never laid out shorter than its sidebar, so the categories never do.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::IconArtwork;
use tairix_inline::ArrayVec;
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::button::{Button, ButtonAction};
use crate::column::{ColumnAction, ColumnPlace, FieldColumn};
use crate::damage;
use crate::form::{FieldGroup, FieldLayout};
use crate::paint::{paint_run, role_font};
use crate::scroll::{ScrollModel, ScrollOrientation, ScrollRange, ScrollView};
use crate::scrollbar::{ScrollAction, ScrollBar};
use crate::stack;
use crate::tabs::{Tab, Tabs, TabsAction, TabsOrientation};
use crate::text::Keystroke;

/// The size a settings window opens at, in logical pixels.
pub const SETTINGS_SIZE: (u32, u32) = (600, 440);

/// The least a settings window may be made, in logical pixels, before its
/// sidebar's own height is counted.
pub const SETTINGS_LEAST: (u32, u32) = (440, 320);

/// The sidebar's width, in logical pixels; never more than a third of the
/// window.
const SIDEBAR: u32 = 150;

/// What a settings window's input asks its owner for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsAction {
    /// The sidebar chose category `category`: the owner shows its groups with
    /// [`SettingsFrame::show`], which is what moves the sidebar to it, so a
    /// row the same event settled is still of the category on show.
    Shown {
        /// The category chosen, as the sidebar lists them.
        category: usize,
    },
    /// A row of the category shown asked for something.
    Field(ColumnAction),
    /// The footer's command `index` was activated.
    Command {
        /// The command, in the order the footer was given them.
        index: usize,
    },
    /// Escape, with nothing to take back: close the window.
    Close,
}

/// What one event asks: a row the keyboard left settling, then what the
/// event itself did.
pub type SettingsActions = ArrayVec<SettingsAction, 2>;

/// Where a settings window's regions stand, resolved once for a window of one
/// size so its paint and its hit test agree.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SettingsLayout {
    /// The whole window.
    pub window: Rect,
    /// The categories.
    pub sidebar: Rect,
    /// Where the column shows, its scrollbar beside it.
    pub view: Rect,
    /// The column's scrollbar, where the column outgrows the view.
    pub bar: Option<Rect>,
    /// The commands and the note.
    pub footer: Rect,
    /// The column's height in the view, never less than the view's: what it
    /// scrolls through.
    pub extent: u32,
}

/// Which region holds the keyboard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stop {
    Sidebar,
    Column,
    Command(usize),
}

/// A settings window's sidebar, column and footer.
#[derive(Clone, Debug)]
pub struct SettingsFrame {
    sidebar: Tabs,
    column: FieldColumn,
    bar: ScrollBar,
    commands: Vec<Button>,
    note: Option<String>,
    keyboard: Option<Stop>,
    pointer: Point,
}

impl SettingsFrame {
    /// A frame listing `categories`, showing the one at `shown` as `groups`,
    /// with `commands` along its foot.
    #[must_use]
    pub fn new(
        categories: Vec<Tab>,
        shown: usize,
        groups: Vec<FieldGroup>,
        commands: Vec<Button>,
    ) -> Self {
        let mut sidebar = Tabs::new(categories).with_orientation(TabsOrientation::Vertical);
        sidebar.adopt_selected(shown);
        Self {
            sidebar,
            column: FieldColumn::new(groups),
            bar: ScrollBar::new(ScrollOrientation::Vertical, unscrolled()),
            commands,
            note: None,
            keyboard: None,
            pointer: Point::new(-1, -1),
        }
    }

    /// Give the column the keyboard, its cursor on its first row, without
    /// reporting: for an owner presenting the window whole as it opens.
    pub fn focus_column(&mut self) {
        self.keyboard = Some(Stop::Column);
        self.column.set_focused(true);
    }

    /// The category on show.
    #[must_use]
    pub fn shown(&self) -> usize {
        self.sidebar.selected().unwrap_or(0)
    }

    /// The column of the category on show.
    #[must_use]
    pub const fn column(&self) -> &FieldColumn {
        &self.column
    }

    /// The column, for an owner restating a row in place; it reports what
    /// moved.
    pub fn column_mut(&mut self) -> &mut FieldColumn {
        &mut self.column
    }

    /// The note beneath the settings, if there is one.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Whether a row is part way through a gesture — a list open, a slider or
    /// the scrollbar held — which showing other groups would drop.
    #[must_use]
    pub fn holding(&self) -> bool {
        self.column.mid_gesture() || self.bar.is_pressing()
    }

    /// Show `groups` as category `category`, from their top. They measure as
    /// they will, so the owner lays the window out again.
    pub fn show(
        &mut self,
        category: usize,
        groups: Vec<FieldGroup>,
        layout: &SettingsLayout,
        damage: &mut Region,
    ) {
        self.sidebar.adopt_selected(category);
        damage.add(layout.sidebar);
        let keyboard = self.keyboard == Some(Stop::Column);
        self.column = FieldColumn::new(groups);
        if keyboard {
            self.column.set_focused(true);
        }
        self.bar.set_model(unscrolled());
        damage.add(layout.view);
    }

    /// Show `groups` in place of the category's own, built again from what
    /// changed, the keyboard cursor and the scroll kept where they were; the
    /// owner lays the window out again.
    pub fn rebuild(
        &mut self,
        groups: Vec<FieldGroup>,
        layout: &SettingsLayout,
        damage: &mut Region,
    ) {
        self.column.replace(groups);
        damage.add(layout.view);
    }

    /// Say `note` beneath the settings, or nothing, answering whether that
    /// changed what is said.
    pub fn say(
        &mut self,
        note: Option<String>,
        layout: &SettingsLayout,
        damage: &mut Region,
    ) -> bool {
        if note == self.note {
            return false;
        }
        self.note = note;
        damage.add(layout.footer);
        true
    }

    /// The layout of a `width`×`height` window.
    #[must_use]
    pub fn layout(&self, width: u32, height: u32, scale: Scale, theme: &Theme) -> SettingsLayout {
        let window = Rect::new(0, 0, width, height);
        let gap = stack::gap(scale, theme);
        let inner = window.inset(gap);
        let side = scale.scale_length(SIDEBAR).min(width / 3);
        let sidebar = Rect::new(inner.left(), inner.top(), side, inner.height);
        let left = sidebar.right().saturating_add(to_i32(gap));
        let across = u32::try_from(inner.right().saturating_sub(left)).unwrap_or(0);
        let footer_height = self.footer_height(scale, theme).min(inner.height);
        let footer = Rect::new(
            left,
            inner.bottom().saturating_sub(to_i32(footer_height)),
            across,
            footer_height,
        );
        let room = inner
            .height
            .saturating_sub(footer_height)
            .saturating_sub(if footer_height > 0 { gap } else { 0 });
        let mut view = Rect::new(left, inner.top(), across, room);
        let mut height = self.column.measured_height(across, scale, theme);
        let mut bar = None;
        if height > room {
            // A column too tall for the view gives up a bar's breadth, and
            // wraps into what is left.
            let breadth = scale
                .scale_length(theme.metrics().scrollbar_breadth)
                .max(1)
                .min(across);
            view.width = across - breadth;
            bar = Some(Rect::new(view.right(), view.top(), breadth, room));
            height = self.column.measured_height(view.width, scale, theme);
        }
        SettingsLayout {
            window,
            sidebar,
            view,
            bar,
            footer,
            extent: height.max(room),
        }
    }

    /// The window that shows the column in a view `view` wide and tall:
    /// the sidebar, the footer and the gaps around them added, and never
    /// shorter than the sidebar. For an owner sizing its window from what its
    /// rows hold.
    #[must_use]
    pub fn size_for(&self, (width, height): (u32, u32), scale: Scale, theme: &Theme) -> (u32, u32) {
        let gap = stack::gap(scale, theme);
        let footer = self.footer_height(scale, theme);
        let below = if footer > 0 {
            footer.saturating_add(gap)
        } else {
            0
        };
        let side = scale.scale_length(SIDEBAR);
        let across = width
            .saturating_add(side)
            .saturating_add(gap.saturating_mul(3));
        // The sidebar is never more than a third of the window.
        let across = across.max(side.saturating_mul(3));
        let down = height
            .saturating_add(below)
            .saturating_add(gap.saturating_mul(2));
        let sidebar = self
            .sidebar
            .measured_height(scale, theme)
            .saturating_add(gap.saturating_mul(2));
        (across, down.max(sidebar))
    }

    /// Where row `row` of group `group` shows in the window, cut to the view;
    /// `None` for a row it does not show. For an owner restating a row in
    /// place.
    #[must_use]
    pub fn row_rect(
        &self,
        (group, row): (usize, usize),
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let (place, view) = self.placed(layout, scale, theme);
        let field = self.group_layout(group, place)?;
        let rect = self
            .column
            .groups()
            .get(group)?
            .row_rect(row, field, scale, theme)?;
        view.to_window(rect)
    }

    /// Where the control of row `row` of group `group` shows in the window,
    /// cut to the view; `None` for one it does not show.
    #[must_use]
    pub fn control_rect(
        &self,
        (group, row): (usize, usize),
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let (place, view) = self.placed(layout, scale, theme);
        let laid = self.group_layout(group, place)?;
        let seated = self.column.groups().get(group)?;
        let bounds = seated.row_rect(row, laid, scale, theme)?;
        let control =
            seated
                .rows()
                .get(row)?
                .control_rect(FieldLayout { bounds, ..laid }, scale, theme)?;
        view.to_window(control)
    }

    /// Where group `group` is laid out in the column's own layout.
    fn group_layout(&self, group: usize, place: ColumnPlace<'_>) -> Option<FieldLayout> {
        self.column
            .layouts(place)
            .into_iter()
            .find_map(|(index, layout)| (index == group).then_some(layout))
    }

    /// The smallest window the frame can be laid out in: the least a settings
    /// window is, and never shorter than its sidebar.
    #[must_use]
    pub fn min_size(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let gap = stack::gap(scale, theme).saturating_mul(2);
        let sidebar = self
            .sidebar
            .measured_height(scale, theme)
            .saturating_add(gap);
        (
            scale.scale_length(SETTINGS_LEAST.0),
            scale.scale_length(SETTINGS_LEAST.1).max(sidebar),
        )
    }

    /// Bring the scroll into line with `layout`: the column's extent, the
    /// view it shows through, the offset held within them.
    pub fn settle(&mut self, layout: &SettingsLayout, damage: &mut Region) {
        let model = self.bar.model();
        let resized = model.resize(u64::from(layout.extent), u64::from(layout.view.height));
        if resized != model {
            self.bar.set_model(resized);
            damage.add(layout.view);
            if let Some(bar) = layout.bar {
                damage.add(bar);
            }
        }
    }

    /// Draw the window into `surface` as far as its clip admits.
    pub fn render(
        &self,
        surface: &mut Surface,
        layout: &SettingsLayout,
        (scale, theme): (Scale, &Theme),
        artwork: &mut dyn IconArtwork,
    ) {
        let palette = theme.palette();
        surface.fill(Color::from(palette.surface));
        let seam = layout
            .sidebar
            .right()
            .saturating_add(to_i32(stack::gap(scale, theme) / 2));
        if let (Ok(x), Ok(top)) = (u32::try_from(seam), u32::try_from(layout.window.top())) {
            surface.fill_rect(x, top, 1, layout.window.height, Color::from(palette.border));
        }
        self.sidebar
            .render(surface, layout.sidebar, scale, theme, artwork);
        let (place, view) = self.placed(layout, scale, theme);
        view.paint(surface, |surface| self.column.render(surface, place));
        if let Some(bar) = layout.bar {
            self.bar.render(surface, bar, scale, theme);
        }
        for (button, rect) in self
            .commands
            .iter()
            .zip(self.command_rects(layout, scale, theme))
        {
            button.render(surface, rect, scale, theme);
        }
        self.render_note(surface, layout, scale, theme);
        // An open list hangs over the footer and the bar as well as the rows.
        view.confined_to(layout.window)
            .paint(surface, |surface| self.column.render_popup(surface, place));
    }

    /// Feed a pointer event, reporting into `damage` what it redrew.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> SettingsActions {
        let mut asked = SettingsActions::new();
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        let listing = self.column.is_listing();
        if !listing
            && matches!(
                event,
                InputEvent::PointerPressed {
                    button: PointerButton::Primary
                }
            )
        {
            let region = self.region_at(self.pointer, layout, scale, theme);
            if region.is_some() && region != self.keyboard {
                // The column gives its own cursor to the row pressed, and
                // settles the one it leaves as it does.
                let into_column = region == Some(Stop::Column);
                if let Some(left) = self.release(!into_column, layout, scale, theme, damage) {
                    push(&mut asked, SettingsAction::Field(left));
                }
                self.keyboard = region.filter(|&stop| stop != Stop::Sidebar);
                if let Some(Stop::Command(index)) = self.keyboard {
                    self.focus_command(index, true, layout, scale, theme, damage);
                }
            }
        }
        // An open list or a held bar sees every event first, wherever it
        // falls; the rows a list hangs over see none of them.
        if let Some(acted) = self.column_pointer(event, layout, scale, theme, damage) {
            push(&mut asked, acted);
            return asked;
        }
        if listing {
            return asked;
        }
        if let Some(bar) = layout.bar {
            if let Some(ScrollAction::ScrollTo { .. }) =
                self.bar.on_pointer(event, bar, scale, theme, damage)
            {
                damage.add(layout.view);
            }
        }
        if let InputEvent::PointerScrolled { dx, dy } = *event {
            self.wheel(dx, dy, layout, scale, damage);
        }
        let mut seats = Seats::along(layout.footer, scale, theme);
        for (index, button) in self.commands.iter_mut().enumerate() {
            let rect = seats.seat(button.measured_width(scale, theme));
            if let Some(ButtonAction::Activated) = button.on_pointer(event, rect, damage) {
                push(&mut asked, SettingsAction::Command { index });
            }
        }
        if let Some(TabsAction::Selected { index }) =
            self.sidebar
                .on_pointer(event, layout.sidebar, scale, theme, damage)
        {
            if let Some(shown) = self.choose(index) {
                push(&mut asked, shown);
            }
        }
        asked
    }

    /// Feed a key press, reporting into `damage` what it redrew.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> SettingsActions {
        let mut asked = SettingsActions::new();
        let key = stroke.key;
        let listing = self.column.is_listing();
        if key == Key::Named(NamedKey::Tab) && !listing {
            let forward = !stroke.modifiers.shift;
            let next = match self.keyboard {
                None if forward => Stop::Sidebar,
                None => self.last_stop(),
                Some(stop) => self.next_stop(stop, forward),
            };
            if let Some(left) = self.enter(next, layout, scale, theme, damage) {
                push(&mut asked, SettingsAction::Field(left));
            }
            return asked;
        }
        let used = match self.keyboard {
            Some(Stop::Sidebar) => {
                if let Some(TabsAction::Selected { index }) =
                    self.sidebar
                        .on_key(key, layout.sidebar, scale, theme, damage)
                {
                    if let Some(shown) = self.choose(index) {
                        push(&mut asked, shown);
                    }
                }
                false
            }
            Some(Stop::Column) => {
                let (place, view) = self.placed(layout, scale, theme);
                let mut drew = damage::sink();
                let acted = self.column.on_key(stroke, place, &mut drew);
                let shown = if listing || self.column.is_listing() {
                    view.confined_to(layout.window)
                } else {
                    view
                };
                shown.report(&drew, damage);
                self.reveal_cursor(layout, scale, theme, damage);
                let used = acted.is_some() || listing;
                if let Some(acted) = acted {
                    push(&mut asked, SettingsAction::Field(acted));
                }
                used
            }
            Some(Stop::Command(index)) => {
                if let Some(ButtonAction::Activated) = self
                    .commands
                    .get_mut(index)
                    .and_then(|button| button.on_key(key))
                {
                    push(&mut asked, SettingsAction::Command { index });
                }
                false
            }
            None => false,
        };
        if key == Key::Named(NamedKey::Escape) && !used {
            push(&mut asked, SettingsAction::Close);
        }
        asked
    }

    /// When the held scrollbar next steps, or a row's control next changes
    /// on its own, the sooner of them.
    pub fn deadline_ns(&mut self, now_ns: u64) -> Option<u64> {
        let bar = self.bar.repeat_deadline(now_ns);
        bar.into_iter().chain(self.column.deadline_ns()).min()
    }

    /// Carry out what came due by `now_ns`, reporting what moved.
    pub fn advance(
        &mut self,
        now_ns: u64,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if let Some(bar) = layout.bar {
            if self.bar.repeat_due(now_ns)
                && self.bar.repeat(now_ns, bar, scale, theme, damage).is_some()
            {
                damage.add(layout.view);
            }
        }
        let (place, view) = self.placed(layout, scale, theme);
        let mut drew = damage::sink();
        self.column.advance(now_ns, place, &mut drew);
        view.report(&drew, damage);
    }

    /// The column's place in its own unscrolled layout, and the view it shows
    /// through.
    fn placed<'a>(
        &self,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &'a Theme,
    ) -> (ColumnPlace<'a>, ScrollView) {
        let offset = self.bar.model().offset();
        let view = ScrollView::new(ScrollOrientation::Vertical, layout.view, offset);
        let bounds = Rect::new(
            layout.view.left(),
            layout.view.top(),
            layout.view.width,
            layout.extent,
        );
        let shift = to_i32(view.offset());
        let window = layout.window;
        let viewport = Rect::new(
            window.left(),
            window.top().saturating_add(shift),
            window.width,
            window.height,
        );
        let place = ColumnPlace {
            bounds,
            viewport,
            scale,
            theme,
        };
        (place, view)
    }

    /// Feed `event` to the column through its view, answering what a row
    /// asked; `None` when the column did not take it.
    fn column_pointer(
        &mut self,
        event: &InputEvent,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<SettingsAction> {
        let listing = self.column.is_listing();
        let (place, view) = self.placed(layout, scale, theme);
        let hit = if listing {
            view.confined_to(layout.window)
        } else {
            view
        };
        let mut drew = damage::sink();
        let acted = self
            .column
            .on_pointer(&hit.event_in_layout(event), place, &mut drew);
        let shown = if listing || self.column.is_listing() {
            view.confined_to(layout.window)
        } else {
            view
        };
        shown.report(&drew, damage);
        acted.map(SettingsAction::Field)
    }

    /// Scroll the column by a wheel turn no row used, where the pointer is
    /// over it.
    fn wheel(
        &mut self,
        dx: i32,
        dy: i32,
        layout: &SettingsLayout,
        scale: Scale,
        damage: &mut Region,
    ) {
        let Some(bar) = layout.bar else {
            return;
        };
        if !layout.view.contains(self.pointer) && !bar.contains(self.pointer) {
            return;
        }
        if self.bar.wheel(dx, dy, scale, bar, damage).is_some() {
            damage.add(layout.view);
        }
    }

    /// Scroll so the keyboard cursor shows, where the column has one.
    fn reveal_cursor(
        &mut self,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let (place, _) = self.placed(layout, scale, theme);
        let Some(cursor) = self.column.cursor_reveal(place, layout.view.height) else {
            return;
        };
        let model = self.bar.model();
        let start = u64::try_from(cursor.top().saturating_sub(place.bounds.top())).unwrap_or(0);
        let revealed = model.revealing(start, u64::from(cursor.height));
        if revealed.offset() != model.offset() {
            self.bar.set_model(revealed);
            damage.add(layout.view);
            if let Some(bar) = layout.bar {
                damage.add(bar);
            }
        }
    }

    /// The request for category `index`'s groups, where it is not on show.
    fn choose(&self, index: usize) -> Option<SettingsAction> {
        (self.sidebar.selected() != Some(index))
            .then_some(SettingsAction::Shown { category: index })
    }

    /// Give `stop` the keyboard, answering the settle of a row the column
    /// left.
    fn enter(
        &mut self,
        stop: Stop,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<ColumnAction> {
        let left = self.release(true, layout, scale, theme, damage);
        match stop {
            Stop::Sidebar => {
                let current = self.sidebar.selected();
                self.sidebar
                    .set_current(current, layout.sidebar, scale, theme, damage);
            }
            Stop::Column => {
                self.column.set_focused(true);
                damage.add(layout.view);
                self.reveal_cursor(layout, scale, theme, damage);
            }
            Stop::Command(index) => self.focus_command(index, true, layout, scale, theme, damage),
        }
        self.keyboard = Some(stop);
        left
    }

    /// Take the keyboard from the region holding it, answering the settle of
    /// a row the column left. A press landing in the column leaves its cursor
    /// for the column itself to move, which is what `leave_column` declines.
    fn release(
        &mut self,
        leave_column: bool,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<ColumnAction> {
        match self.keyboard.take()? {
            Stop::Sidebar => {
                self.sidebar
                    .set_current(None, layout.sidebar, scale, theme, damage);
                None
            }
            Stop::Column if leave_column => {
                let (place, view) = self.placed(layout, scale, theme);
                let mut drew = damage::sink();
                let left = self.column.leave(place, &mut drew);
                view.report(&drew, damage);
                left
            }
            Stop::Column => None,
            Stop::Command(index) => {
                self.focus_command(index, false, layout, scale, theme, damage);
                None
            }
        }
    }

    fn focus_command(
        &mut self,
        index: usize,
        focused: bool,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let rect = self.command_rects(layout, scale, theme).nth(index);
        if let (Some(button), Some(rect)) = (self.commands.get_mut(index), rect) {
            button.set_focused(focused);
            damage.add(rect);
        }
    }

    /// The stop after `stop`, or before it when going back.
    fn next_stop(&self, stop: Stop, forward: bool) -> Stop {
        let commands = self.commands.len();
        match (stop, forward) {
            (Stop::Sidebar, true) | (Stop::Command(0), false) => Stop::Column,
            (Stop::Column, true) if commands > 0 => Stop::Command(0),
            (Stop::Command(index), true) if index + 1 < commands => Stop::Command(index + 1),
            (Stop::Column | Stop::Command(_), true) | (Stop::Column, false) => Stop::Sidebar,
            (Stop::Sidebar, false) => self.last_stop(),
            (Stop::Command(index), false) => Stop::Command(index - 1),
        }
    }

    /// The stop Shift+Tab reaches first: the last command, else the column.
    fn last_stop(&self) -> Stop {
        self.commands
            .len()
            .checked_sub(1)
            .map_or(Stop::Column, Stop::Command)
    }

    /// The region `at` falls in, if any takes the keyboard.
    fn region_at(
        &self,
        at: Point,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Stop> {
        if layout.sidebar.contains(at) {
            return Some(Stop::Sidebar);
        }
        if layout.view.contains(at) {
            return Some(Stop::Column);
        }
        self.command_rects(layout, scale, theme)
            .position(|rect| rect.contains(at))
            .map(Stop::Command)
    }

    /// Where each command stands along the footer, from its leading edge.
    fn command_rects<'a>(
        &'a self,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &'a Theme,
    ) -> impl Iterator<Item = Rect> + 'a {
        let mut seats = Seats::along(layout.footer, scale, theme);
        self.commands
            .iter()
            .map(move |button| seats.seat(button.measured_width(scale, theme)))
    }

    /// The footer's height: a command's, or a line of the note's.
    fn footer_height(&self, scale: Scale, theme: &Theme) -> u32 {
        if !self.commands.is_empty() {
            return Button::height(scale, theme);
        }
        self.note.as_ref().map_or(0, |_| {
            role_font(theme, scale, TextRole::Caption).line_height()
        })
    }

    /// The note, after the last command, cut to the footer's width.
    fn render_note(
        &self,
        surface: &mut Surface,
        layout: &SettingsLayout,
        scale: Scale,
        theme: &Theme,
    ) {
        let Some(note) = self.note.as_deref() else {
            return;
        };
        let gap = to_i32(stack::gap(scale, theme));
        let left = self
            .command_rects(layout, scale, theme)
            .last()
            .map_or(layout.footer.left(), |rect| {
                rect.right().saturating_add(gap)
            });
        let width = u32::try_from(layout.footer.right().saturating_sub(left)).unwrap_or(0);
        let face = role_font(theme, scale, TextRole::Caption);
        let run = face.elide_to_width(note, width);
        let top = layout.footer.top()
            + to_i32(layout.footer.height.saturating_sub(face.line_height()) / 2);
        let ink = Color::from(theme.palette().danger);
        paint_run(surface, face, run, (left, top), ink, None);
    }
}

/// The footer's commands seated one after another from its leading edge,
/// each at its own width and the button height, centred down the footer.
struct Seats {
    left: i32,
    top: i32,
    height: u32,
    gap: i32,
}

impl Seats {
    fn along(footer: Rect, scale: Scale, theme: &Theme) -> Self {
        let height = Button::height(scale, theme).min(footer.height);
        Self {
            left: footer.left(),
            top: footer.top() + to_i32(footer.height.saturating_sub(height) / 2),
            height,
            gap: to_i32(stack::gap(scale, theme)),
        }
    }

    /// The next command's rectangle, `width` wide.
    fn seat(&mut self, width: u32) -> Rect {
        let rect = Rect::new(self.left, self.top, width, self.height);
        self.left = self
            .left
            .saturating_add(to_i32(width))
            .saturating_add(self.gap);
        rect
    }
}

/// The scroll an unmeasured column starts at: nothing to scroll.
fn unscrolled() -> ScrollModel {
    ScrollModel::in_pixels(ScrollRange::EMPTY, 1)
}

/// Add `action` to `asked`, which holds every action one event can make.
fn push(asked: &mut SettingsActions, action: SettingsAction) {
    // A settle and the event's own: never more than two.
    let _ = asked.try_push(action);
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
