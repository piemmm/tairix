//! The file manager's settings window: one page of grouped settings — what a
//! new window opens with, how every window orders and names what it lists,
//! its icons, and the Trash — with *Restore defaults* last, built from the
//! shared form controls and scrolled when the page outgrows its window.
//!
//! It edits a copy of the settings in force and asks the application for every
//! change, which applies it to every window and writes it off the loop; what
//! the store then says comes back through [`SettingsWindow::adopt`]. Every
//! control on the page is a choice or a switch, so each change settles as it
//! is made.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_browse::{Folders, ScrollColumn, SortDirection, SortKey, ViewMode};
use tairix_controls::scroll::{ScrollModel, ScrollRange};
use tairix_controls::{
    stack, Button, ButtonContent, ComboBox, ControlRole, FieldAction, FieldControl, FieldGroup,
    FieldGroupAction, FieldLayout, FieldRow, Keystroke, Toggle,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::preferences::{Preferences, Start, SORT_KEYS, VIEWS};

/// The window's title.
pub const TITLE: &str = "Files Settings";

/// The page's width as the window opens, in logical pixels.
const PAGE_WIDTH: u32 = 520;

/// How far one wheel line scrolls the page, in logical pixels.
const LINE: u32 = 24;

/// One row of the page.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Setting {
    Start,
    View,
    SortBy,
    Order,
    Rail,
    Toolbar,
    FoldersFirst,
    HideAppEndings,
    HideKnownEndings,
    HiddenItems,
    Thumbnails,
    FolderContents,
    ConfirmTrash,
    Restore,
}

/// The page's groups, top to bottom: each its caption and its rows.
const PAGE: [(&str, &[Setting]); 5] = [
    (
        "NEW WINDOWS",
        &[
            Setting::Start,
            Setting::View,
            Setting::SortBy,
            Setting::Order,
            Setting::Rail,
            Setting::Toolbar,
        ],
    ),
    (
        "LISTINGS",
        &[
            Setting::FoldersFirst,
            Setting::HideAppEndings,
            Setting::HideKnownEndings,
            Setting::HiddenItems,
        ],
    ),
    ("ICONS", &[Setting::Thumbnails, Setting::FolderContents]),
    ("TRASH", &[Setting::ConfirmTrash]),
    ("DEFAULTS", &[Setting::Restore]),
];

/// The group a refusal to save or restore is stated beneath.
const DEFAULTS: usize = PAGE.len() - 1;

/// What a row's control shows.
enum Shown {
    /// A choice, at this index of its list.
    Choice(usize),
    /// A switch, on or off.
    Switch(bool),
    /// A command, which shows nothing of the settings.
    Command,
}

impl Setting {
    /// What the row's control shows of `settings`.
    fn shown(self, settings: &Preferences) -> Shown {
        match self {
            Self::Start => Shown::Choice(index_of(&Start::ALL, &settings.start)),
            Self::View => Shown::Choice(index_of(&VIEWS, &settings.view)),
            Self::SortBy => Shown::Choice(index_of(&SORT_KEYS, &settings.sort.key)),
            Self::Order => Shown::Choice(index_of(&SortDirection::ALL, &settings.sort.direction)),
            Self::Rail => Shown::Switch(settings.chrome.rail),
            Self::Toolbar => Shown::Switch(settings.chrome.toolbar.is_shown()),
            Self::FoldersFirst => Shown::Switch(settings.sort.folders == Folders::First),
            Self::HideAppEndings => Shown::Switch(settings.presentation.endings.applications),
            Self::HideKnownEndings => Shown::Switch(settings.presentation.endings.known_types),
            Self::HiddenItems => Shown::Switch(settings.presentation.hidden_items),
            Self::Thumbnails => Shown::Switch(settings.presentation.pictures.thumbnails),
            Self::FolderContents => Shown::Switch(settings.presentation.pictures.folder_contents),
            Self::ConfirmTrash => Shown::Switch(settings.confirm_trash),
            Self::Restore => Shown::Command,
        }
    }

    /// The row's label, and the line beneath it where it needs one.
    const fn words(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Start => ("Open new windows at", None),
            Self::View => ("Show items as", None),
            Self::SortBy => ("Sort by", None),
            Self::Order => ("Order", None),
            Self::Rail => ("Show the places rail", None),
            Self::Toolbar => ("Show the toolbar", None),
            Self::FoldersFirst => (
                "Keep folders on top",
                Some("Folders come first whichever way a window is sorted."),
            ),
            Self::HideAppEndings => ("Hide the .app ending of applications", None),
            Self::HideKnownEndings => (
                "Hide the endings of known file types",
                Some("Renaming an item keeps its ending."),
            ),
            Self::HiddenItems => (
                "Show hidden items",
                Some("Items whose names begin with a dot."),
            ),
            Self::Thumbnails => ("Show pictures as their content", None),
            Self::FolderContents => ("Show what folders hold", None),
            Self::ConfirmTrash => (
                "Ask before moving items to the Trash",
                Some("Deleting for good always asks."),
            ),
            Self::Restore => ("Every setting", None),
        }
    }

    /// The choices a choice row lists.
    fn choices(self) -> Vec<String> {
        let labels: &[&str] = match self {
            Self::Start => &Start::ALL.map(Start::label),
            Self::View => &VIEWS.map(ViewMode::label),
            Self::SortBy => &SORT_KEYS.map(key_label),
            Self::Order => &SortDirection::ALL.map(SortDirection::label),
            _ => &[],
        };
        labels.iter().copied().map(String::from).collect()
    }

    /// The row drawn for this setting, showing `settings`.
    fn row(self, settings: &Preferences) -> FieldRow {
        let control = match self.shown(settings) {
            Shown::Choice(index) => {
                FieldControl::Combo(ComboBox::new(self.choices()).with_selected(index))
            }
            Shown::Switch(on) => FieldControl::Toggle(Toggle::new("", on)),
            Shown::Command => FieldControl::Button(Button::new(
                ButtonContent::Label(String::from("Restore defaults")),
                ControlRole::Neutral,
            )),
        };
        let (label, description) = self.words();
        let row = FieldRow::new(label, control);
        match description {
            Some(description) => row.with_description(description),
            None => row,
        }
    }

    /// `was` with what `action` on this row asked for, or `None` when it asks
    /// for nothing.
    fn edited(self, action: &FieldAction, was: &Preferences) -> Option<Preferences> {
        let mut now = *was;
        match (self, action) {
            (Self::Start, FieldAction::Selected { index }) => {
                now.start = *Start::ALL.get(*index)?;
            }
            (Self::View, FieldAction::Selected { index }) => {
                now.view = *VIEWS.get(*index)?;
            }
            (Self::SortBy, FieldAction::Selected { index }) => {
                now.sort.key = *SORT_KEYS.get(*index)?;
            }
            (Self::Order, FieldAction::Selected { index }) => {
                now.sort.direction = *SortDirection::ALL.get(*index)?;
            }
            (Self::Rail, FieldAction::Set { on }) => now.chrome.rail = *on,
            (Self::Toolbar, FieldAction::Set { on }) => {
                now.chrome.toolbar = if *on {
                    tairix_browse::ToolbarBand::Shown
                } else {
                    tairix_browse::ToolbarBand::Hidden
                };
            }
            (Self::FoldersFirst, FieldAction::Set { on }) => {
                now.sort.folders = if *on { Folders::First } else { Folders::Among };
            }
            (Self::HideAppEndings, FieldAction::Set { on }) => {
                now.presentation.endings.applications = *on;
            }
            (Self::HideKnownEndings, FieldAction::Set { on }) => {
                now.presentation.endings.known_types = *on;
            }
            (Self::HiddenItems, FieldAction::Set { on }) => now.presentation.hidden_items = *on,
            (Self::Thumbnails, FieldAction::Set { on }) => {
                now.presentation.pictures.thumbnails = *on;
            }
            (Self::FolderContents, FieldAction::Set { on }) => {
                now.presentation.pictures.folder_contents = *on;
            }
            (Self::ConfirmTrash, FieldAction::Set { on }) => now.confirm_trash = *on,
            _ => return None,
        }
        (now != *was).then_some(now)
    }
}

/// What a combo lists a folder's order by under.
const fn key_label(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "Name",
        SortKey::Kind => "Type",
        SortKey::Size => "Size",
        SortKey::Modified => "Date modified",
        SortKey::Free => "Free space",
        SortKey::Filesystem => "Filesystem",
        SortKey::Health => "Health",
        SortKey::Address => "Address",
        SortKey::Workgroup => "Workgroup",
    }
}

/// Where `value` stands in `all`, the first when it is not there.
fn index_of<T: PartialEq>(all: &[T], value: &T) -> usize {
    all.iter().position(|each| each == value).unwrap_or(0)
}

/// What the window's input asks of the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsRequest {
    /// The settings became `now` from `was`: apply them and keep them.
    Edit {
        /// Before.
        was: Preferences,
        /// After.
        now: Preferences,
    },
    /// Put every setting back to what the layers beneath the user's own say.
    Restore,
    /// Close the window.
    Close,
}

/// Where the page is drawn in a window: the column its groups stack down, the
/// gutter its scroll bar stands in, and each group's layout.
struct Page {
    body: Rect,
    gutter: Rect,
    content: u32,
    groups: Vec<FieldLayout>,
}

impl Page {
    /// The scroll geometry the page implies at `offset`.
    fn model(&self, offset: u64, scale: Scale) -> ScrollModel {
        ScrollModel::in_pixels(
            ScrollRange::new(u64::from(self.content), u64::from(self.body.height), offset),
            u64::from(scale.scale_length(LINE)),
        )
    }
}

/// The settings window.
#[derive(Clone, Debug)]
pub struct SettingsWindow {
    groups: Vec<FieldGroup>,
    shown: Preferences,
    scroll: ScrollColumn,
}

impl SettingsWindow {
    /// The window showing `settings`, the keyboard on its first row.
    #[must_use]
    pub fn new(settings: &Preferences) -> Self {
        let mut groups: Vec<FieldGroup> = PAGE
            .iter()
            .map(|(caption, rows)| {
                FieldGroup::new(
                    *caption,
                    rows.iter().map(|setting| setting.row(settings)).collect(),
                )
            })
            .collect();
        if let Some(first) = groups.first_mut() {
            first.adopt_focus(Some(0));
        }
        Self {
            groups,
            shown: *settings,
            scroll: ScrollColumn::new(),
        }
    }

    /// The settings the window shows.
    #[must_use]
    pub const fn shown(&self) -> &Preferences {
        &self.shown
    }

    /// The window's extent at `scale`: the page's width, and the height that
    /// shows all of it.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let gutter = gutter_width(scale, theme);
        let body = scale.scale_length(PAGE_WIDTH);
        let plate = stack::plate_width(body, scale, theme);
        let column = FieldGroup::shared_column(&self.groups, plate, scale, theme);
        let content = stack::height(
            self.groups
                .iter()
                .map(|group| group.measured_height(plate, column, scale, theme)),
            scale,
            theme,
        );
        (body.saturating_add(gutter), content)
    }

    /// Lay the page out in `window`, scrolled no further than its end.
    fn page(&self, window: Rect, scale: Scale, theme: &Theme) -> Page {
        let gutter_width = gutter_width(scale, theme).min(window.width);
        let body = Rect::new(
            window.left(),
            window.top(),
            window.width - gutter_width,
            window.height,
        );
        let gutter = Rect::new(body.right(), window.top(), gutter_width, window.height);
        let plate = stack::plate_width(body.width, scale, theme);
        let column = FieldGroup::shared_column(&self.groups, plate, scale, theme);
        let heights: Vec<u32> = self
            .groups
            .iter()
            .map(|group| group.measured_height(plate, column, scale, theme))
            .collect();
        let content = stack::height(heights.iter().copied(), scale, theme);
        let offset = self.offset_within(content, body.height);
        let top = body
            .top()
            .saturating_sub(i32::try_from(offset).unwrap_or(i32::MAX));
        let column_rect = Rect::new(body.left(), top, body.width, content);
        let groups = stack::place(column_rect, self.groups.len(), scale, theme, |index| {
            heights.get(index).copied().unwrap_or(0)
        })
        .into_iter()
        .filter_map(|(index, rect)| {
            let layout = FieldLayout::new(rect, column).within(window);
            Some(self.groups.get(index)?.place_popup(layout, scale, theme))
        })
        .collect();
        Page {
            body,
            gutter,
            content,
            groups,
        }
    }

    /// The scroll offset clamped to a page of `content` pixels in a column
    /// `shown` pixels tall.
    fn offset_within(&self, content: u32, shown: u32) -> u64 {
        self.scroll
            .offset()
            .min(u64::from(content.saturating_sub(shown)))
    }

    /// Draw the window into `surface`, filling `window`.
    pub fn render(&self, surface: &mut Surface, window: Rect, scale: Scale, theme: &Theme) {
        surface.fill(Color::from(theme.palette().surface));
        let page = self.page(window, scale, theme);
        let body = page.body;
        let (x, y) = (
            u32::try_from(body.left()).unwrap_or(0),
            u32::try_from(body.top()).unwrap_or(0),
        );
        surface.with_clip(x, y, body.width, body.height, |surface| {
            for (group, layout) in self.groups.iter().zip(&page.groups) {
                group.render(surface, *layout, scale, theme);
            }
        });
        // The bar is drawn over the geometry this frame lays out, carrying the
        // column's own hover and drag.
        let mut bar = *self.scroll.scrollbar();
        bar.set_model(page.model(self.offset_within(page.content, body.height), scale));
        bar.render(surface, page.gutter, scale, theme);
        // A choice list hangs over the rest of the page, so it is drawn last.
        if let Some((group, layout)) = self.listing(&page) {
            group.render_popup(surface, layout.popup, scale, theme);
        }
    }

    /// The group whose choice list is open, with its layout.
    fn listing<'a>(&'a self, page: &'a Page) -> Option<(&'a FieldGroup, &'a FieldLayout)> {
        self.groups
            .iter()
            .zip(&page.groups)
            .find(|(group, _)| group.rows().iter().any(FieldRow::popup_open))
    }

    /// Feed a pointer `event` at `point`, answering what it asks of the
    /// application.
    ///
    /// The scroll bar takes what lands on it; an open choice list takes
    /// everything until it resolves; a primary press moves the keyboard to
    /// the row pressed and off every other group.
    pub fn on_pointer(
        &mut self,
        (point, event): (Point, &InputEvent),
        window: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<SettingsRequest> {
        let page = self.page(window, scale, theme);
        let listing = self
            .groups
            .iter()
            .position(|group| group.rows().iter().any(FieldRow::popup_open));
        if listing.is_none() {
            let model = page.model(self.offset_within(page.content, page.body.height), scale);
            let routed = self.scroll.route(
                model,
                (page.gutter, page.body),
                scale,
                theme,
                (point, event),
                damage,
            );
            if routed.is_some() {
                return None;
            }
        }
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: tairix_input::PointerButton::Primary
            }
        );
        let mut asked = None;
        for (index, layout) in page.groups.iter().enumerate() {
            if listing.is_some_and(|open| open != index) {
                continue;
            }
            if pressed && listing.is_none() && !layout.bounds.contains(point) {
                if let Some(group) = self.groups.get_mut(index) {
                    group.set_focus(None, *layout, scale, theme, damage);
                }
            }
            let Some(group) = self.groups.get_mut(index) else {
                continue;
            };
            if let Some(action) = group.on_pointer(event, *layout, scale, theme, damage) {
                asked = self.act(index, &action);
            }
        }
        asked
    }

    /// Feed a key press, answering what it asks of the application.
    ///
    /// Up and Down walk the rows from one group into the next, and Tab and
    /// Shift+Tab do the same, wrapping at the ends; the focused row's control
    /// takes every other key. Escape closes the window unless a choice list
    /// is open for it to close. The page scrolls the least that shows the row
    /// the keyboard is on.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        window: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<SettingsRequest> {
        let page = self.page(window, scale, theme);
        let open = self
            .groups
            .iter()
            .any(|group| group.rows().iter().any(FieldRow::popup_open));
        let focus = self.focus();
        let step = match stroke.key {
            _ if open => None,
            Key::Named(NamedKey::Escape) => return Some(SettingsRequest::Close),
            Key::Named(NamedKey::Tab) if stroke.modifiers.shift => Some((false, true)),
            Key::Named(NamedKey::Tab) => Some((true, true)),
            Key::Named(NamedKey::Down) if self.at_group_edge(focus, true) => Some((true, false)),
            Key::Named(NamedKey::Up) if self.at_group_edge(focus, false) => Some((false, false)),
            _ => None,
        };
        if let Some((forward, wrap)) = step {
            self.step_focus(focus, (forward, wrap), &page, (scale, theme), damage);
            self.reveal(window, (scale, theme), damage);
            return None;
        }
        let (index, _) = focus?;
        let layout = *page.groups.get(index)?;
        let action = self
            .groups
            .get_mut(index)?
            .on_key(stroke, layout, scale, theme, damage);
        self.reveal(window, (scale, theme), damage);
        self.act(index, &action?)
    }

    /// Scroll the page by a wheel turn of `(dx, dy)`, answering whether it
    /// moved.
    pub fn on_wheel(
        &mut self,
        delta: (i32, i32),
        window: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> bool {
        let page = self.page(window, scale, theme);
        let model = page.model(self.offset_within(page.content, page.body.height), scale);
        self.scroll
            .wheel(model, delta, scale, (page.gutter, page.body), damage)
    }

    /// When a held press on the scroll bar steps next, now it is `now`.
    pub fn due(&mut self, now: u64) -> Option<u64> {
        self.scroll.repeat_deadline(now)
    }

    /// Step a held press on the scroll bar if it is due at `now`, answering
    /// whether the page moved.
    pub fn step_scroll(
        &mut self,
        now: u64,
        window: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> bool {
        let page = self.page(window, scale, theme);
        let model = page.model(self.offset_within(page.content, page.body.height), scale);
        self.scroll
            .repeat(now, model, (page.gutter, page.body), (scale, theme), damage)
    }

    /// Show `settings`, as the store now holds them, reporting what moved.
    pub fn adopt(&mut self, settings: &Preferences, window: Rect, damage: &mut Region) {
        if self.shown == *settings {
            return;
        }
        for (group, (_, rows)) in self.groups.iter_mut().zip(PAGE) {
            for (row, setting) in group.rows_mut().iter_mut().zip(rows.iter()) {
                match (row.control_mut(), setting.shown(settings)) {
                    (FieldControl::Combo(combo), Shown::Choice(index)) => combo.set_selected(index),
                    (FieldControl::Toggle(toggle), Shown::Switch(on)) => toggle.set_on(on),
                    _ => {}
                }
            }
        }
        self.shown = *settings;
        damage.add(window);
    }

    /// State `said` beneath *Restore defaults*, or nothing, reporting what
    /// moved.
    pub fn state(&mut self, said: Option<String>, window: Rect, damage: &mut Region) {
        if let Some(group) = self.groups.get_mut(DEFAULTS) {
            if group.set_footnote(said) {
                damage.add(window);
            }
        }
    }

    /// Answer what `action` on a row of group `index` asks for, taking an edit
    /// as what the window now shows.
    fn act(&mut self, index: usize, action: &FieldGroupAction) -> Option<SettingsRequest> {
        let setting = *PAGE.get(index)?.1.get(action.row)?;
        if setting == Setting::Restore {
            return matches!(action.action, FieldAction::Activated)
                .then_some(SettingsRequest::Restore);
        }
        let was = self.shown;
        let now = setting.edited(&action.action, &was)?;
        self.shown = now;
        Some(SettingsRequest::Edit { was, now })
    }

    /// The group and row the keyboard is on.
    fn focus(&self) -> Option<(usize, usize)> {
        self.groups
            .iter()
            .enumerate()
            .find_map(|(index, group)| Some((index, group.focus()?)))
    }

    /// Whether the keyboard rests on the last row of its group (`forward`) or
    /// on its first, so the next step leaves the group.
    fn at_group_edge(&self, focus: Option<(usize, usize)>, forward: bool) -> bool {
        let Some((index, row)) = focus else {
            return true;
        };
        let rows = self.groups.get(index).map_or(0, FieldGroup::len);
        if forward {
            row + 1 >= rows
        } else {
            row == 0
        }
    }

    /// Move the keyboard one row on from `focus`, into the next group at its
    /// edge — wrapping past either end of the page only when `wrap`.
    fn step_focus(
        &mut self,
        focus: Option<(usize, usize)>,
        (forward, wrap): (bool, bool),
        page: &Page,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        let rows: Vec<usize> = self.groups.iter().map(FieldGroup::len).collect();
        let total: usize = rows.iter().sum();
        if total == 0 {
            return;
        }
        let flat = focus.map(|(group, row)| rows.iter().take(group).sum::<usize>() + row);
        let end = if forward { 0 } else { total - 1 };
        let next = match flat {
            Some(at) if forward && at + 1 < total => at + 1,
            Some(at) if !forward && at > 0 => at - 1,
            Some(at) if !wrap => at,
            _ => end,
        };
        let mut left = next;
        for (index, (group, layout)) in self.groups.iter_mut().zip(&page.groups).enumerate() {
            let len = rows.get(index).copied().unwrap_or(0);
            let row = (left < len).then_some(left);
            left = left.saturating_sub(len);
            if row.is_some() || group.focus().is_some() {
                group.set_focus(row, *layout, scale, theme, damage);
            }
        }
    }

    /// Scroll the least that shows the row the keyboard is on.
    fn reveal(&mut self, window: Rect, (scale, theme): (Scale, &Theme), damage: &mut Region) {
        let page = self.page(window, scale, theme);
        let offset = self.offset_within(page.content, page.body.height);
        let shown = self.focus().and_then(|(index, _)| {
            let layout = page.groups.get(index)?;
            self.groups.get(index)?.focus_rect(*layout, scale, theme)
        });
        let Some(shown) = shown else {
            return;
        };
        // The row's place down the unscrolled page.
        let top = u64::try_from(i64::from(shown.top()) - i64::from(page.body.top()))
            .unwrap_or(0)
            .saturating_add(offset);
        let model = page
            .model(offset, scale)
            .revealing(top, u64::from(shown.height));
        if self.scroll.set_offset(model.range().offset()) {
            damage.add(window);
        }
    }
}

/// The scroll bar's gutter width at `scale`.
fn gutter_width(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().scrollbar_breadth).max(1)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
