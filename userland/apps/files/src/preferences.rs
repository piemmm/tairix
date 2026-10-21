//! The file manager's own settings: what a new window opens with, and how
//! every window orders, names and pictures what it lists, and asks before
//! moving to the Trash. Kept in its private app-data store by the shared
//! closed-registry engine and edited in its settings window
//! ([`crate::settings`]).

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::home::{HOME_DESKTOP_DIR, HOME_FILES_DIR};
use tairix_abi::STORAGE_CATALOG;
use tairix_appconf::{as_bool, bool_text, overwrite, Live, Registry};
use tairix_browse::{
    Folders, Presentation, SortDirection, SortKey, SortMode, ToolbarBand, ViewMode,
    MANAGER_PRESENTATION, MANAGER_VIEW_MODE,
};

use crate::chrome::Chrome;

/// Where a window asked to open on no folder in particular opens.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Start {
    /// The user's own files.
    #[default]
    Home,
    /// The desktop's folder.
    Desktop,
    /// The attached storage.
    Storage,
}

impl Start {
    /// Every place, in the order a choice lists them.
    pub const ALL: [Self; 3] = [Self::Home, Self::Desktop, Self::Storage];

    /// What a choice calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Desktop => "Desktop",
            Self::Storage => "Storage",
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Desktop => "desktop",
            Self::Storage => "storage",
        }
    }

    /// The folder it is for a user whose home is `home`, or `None` for one in
    /// a home the user has none of.
    #[must_use]
    pub fn place(self, home: Option<&[String]>) -> Option<Vec<String>> {
        let in_home = |leaf: &str| {
            home.map(|home| {
                let mut place = home.to_vec();
                place.push(String::from(leaf));
                place
            })
        };
        match self {
            Self::Home => in_home(HOME_FILES_DIR),
            Self::Desktop => in_home(HOME_DESKTOP_DIR),
            Self::Storage => Some(alloc::vec![String::from(STORAGE_CATALOG)]),
        }
    }
}

/// The views a new window may open in, in the order a choice lists them.
pub const VIEWS: [ViewMode; 3] = ViewMode::ALL;

/// The orders a new window may open sorted by: a folder's own columns.
pub const SORT_KEYS: [SortKey; 4] = [
    SortKey::Name,
    SortKey::Kind,
    SortKey::Size,
    SortKey::Modified,
];

/// Every setting the file manager keeps.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Preferences {
    /// Where a window asked for no folder opens.
    pub start: Start,
    /// The view a new window opens in.
    pub view: ViewMode,
    /// The order a new window opens sorted in, and where every window puts
    /// its folders.
    pub sort: SortMode,
    /// The chrome a new window opens showing.
    pub chrome: Chrome,
    /// How every window presents what it lists.
    pub presentation: Presentation,
    /// Whether moving items to the Trash asks first.
    pub confirm_trash: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            start: Start::Home,
            view: MANAGER_VIEW_MODE,
            sort: SortMode::default_order(),
            chrome: Chrome::HIDDEN,
            presentation: MANAGER_PRESENTATION,
            confirm_trash: true,
        }
    }
}

impl Preferences {
    /// The order a window already open is to keep with these settings: its
    /// own key and direction, and where these put folders.
    #[must_use]
    pub const fn reorder(&self, order: SortMode) -> SortMode {
        SortMode {
            folders: self.sort.folders,
            ..order
        }
    }
}

/// One key the file manager keeps a setting under.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PrefKey {
    /// `window.start`.
    Start,
    /// `window.view`.
    View,
    /// `window.sort-by`.
    SortBy,
    /// `window.order`.
    Order,
    /// `window.places`.
    Places,
    /// `window.toolbar`.
    Toolbar,
    /// `order.folders-first`.
    FoldersFirst,
    /// `names.hide-app-endings`.
    HideAppEndings,
    /// `names.hide-known-endings`.
    HideKnownEndings,
    /// `names.hidden-items`.
    HiddenItems,
    /// `icons.thumbnails`.
    Thumbnails,
    /// `icons.folder-contents`.
    FolderContents,
    /// `trash.confirm`.
    ConfirmTrash,
}

impl Registry for Preferences {
    type Key = PrefKey;
    const KEYS: &'static [PrefKey] = &[
        PrefKey::Start,
        PrefKey::View,
        PrefKey::SortBy,
        PrefKey::Order,
        PrefKey::Places,
        PrefKey::Toolbar,
        PrefKey::FoldersFirst,
        PrefKey::HideAppEndings,
        PrefKey::HideKnownEndings,
        PrefKey::HiddenItems,
        PrefKey::Thumbnails,
        PrefKey::FolderContents,
        PrefKey::ConfirmTrash,
    ];

    fn name(key: PrefKey) -> &'static str {
        match key {
            PrefKey::Start => "window.start",
            PrefKey::View => "window.view",
            PrefKey::SortBy => "window.sort-by",
            PrefKey::Order => "window.order",
            PrefKey::Places => "window.places",
            PrefKey::Toolbar => "window.toolbar",
            PrefKey::FoldersFirst => "order.folders-first",
            PrefKey::HideAppEndings => "names.hide-app-endings",
            PrefKey::HideKnownEndings => "names.hide-known-endings",
            PrefKey::HiddenItems => "names.hidden-items",
            PrefKey::Thumbnails => "icons.thumbnails",
            PrefKey::FolderContents => "icons.folder-contents",
            PrefKey::ConfirmTrash => "trash.confirm",
        }
    }

    fn read(&mut self, key: PrefKey, text: &str) -> bool {
        let flag = |slot: &mut bool| as_bool(text).map(|on| *slot = on).is_ok();
        match key {
            PrefKey::Start => token_of(Start::ALL, Start::token, text)
                .map(|start| self.start = start)
                .is_some(),
            PrefKey::View => token_of(VIEWS, view_token, text)
                .map(|view| self.view = view)
                .is_some(),
            PrefKey::SortBy => token_of(SORT_KEYS, key_token, text)
                .map(|by| self.sort.key = by)
                .is_some(),
            PrefKey::Order => token_of(SortDirection::ALL, direction_token, text)
                .map(|direction| self.sort.direction = direction)
                .is_some(),
            PrefKey::Places => flag(&mut self.chrome.rail),
            PrefKey::Toolbar => as_bool(text)
                .map(|shown| self.chrome.toolbar = band(shown))
                .is_ok(),
            PrefKey::FoldersFirst => as_bool(text)
                .map(|first| self.sort.folders = folders(first))
                .is_ok(),
            PrefKey::HideAppEndings => flag(&mut self.presentation.endings.applications),
            PrefKey::HideKnownEndings => flag(&mut self.presentation.endings.known_types),
            PrefKey::HiddenItems => flag(&mut self.presentation.hidden_items),
            PrefKey::Thumbnails => flag(&mut self.presentation.pictures.thumbnails),
            PrefKey::FolderContents => flag(&mut self.presentation.pictures.folder_contents),
            PrefKey::ConfirmTrash => flag(&mut self.confirm_trash),
        }
    }

    fn spell(&self, key: PrefKey, out: &mut String) -> bool {
        let text = match key {
            PrefKey::Start => self.start.token(),
            PrefKey::View => view_token(self.view),
            PrefKey::SortBy => key_token(self.sort.key),
            PrefKey::Order => direction_token(self.sort.direction),
            PrefKey::Places => bool_text(self.chrome.rail),
            PrefKey::Toolbar => bool_text(self.chrome.toolbar.is_shown()),
            PrefKey::FoldersFirst => bool_text(self.sort.folders == Folders::First),
            PrefKey::HideAppEndings => bool_text(self.presentation.endings.applications),
            PrefKey::HideKnownEndings => bool_text(self.presentation.endings.known_types),
            PrefKey::HiddenItems => bool_text(self.presentation.hidden_items),
            PrefKey::Thumbnails => bool_text(self.presentation.pictures.thumbnails),
            PrefKey::FolderContents => bool_text(self.presentation.pictures.folder_contents),
            PrefKey::ConfirmTrash => bool_text(self.confirm_trash),
        };
        out.push_str(text);
        true
    }
}

impl Live for Preferences {
    fn take(&mut self, other: &Self, key: PrefKey) -> bool {
        match key {
            PrefKey::Start => overwrite(&mut self.start, other.start),
            PrefKey::View => overwrite(&mut self.view, other.view),
            PrefKey::SortBy => overwrite(&mut self.sort.key, other.sort.key),
            PrefKey::Order => overwrite(&mut self.sort.direction, other.sort.direction),
            PrefKey::Places => overwrite(&mut self.chrome.rail, other.chrome.rail),
            PrefKey::Toolbar => overwrite(&mut self.chrome.toolbar, other.chrome.toolbar),
            PrefKey::FoldersFirst => overwrite(&mut self.sort.folders, other.sort.folders),
            PrefKey::HideAppEndings => overwrite(
                &mut self.presentation.endings.applications,
                other.presentation.endings.applications,
            ),
            PrefKey::HideKnownEndings => overwrite(
                &mut self.presentation.endings.known_types,
                other.presentation.endings.known_types,
            ),
            PrefKey::HiddenItems => overwrite(
                &mut self.presentation.hidden_items,
                other.presentation.hidden_items,
            ),
            PrefKey::Thumbnails => overwrite(
                &mut self.presentation.pictures.thumbnails,
                other.presentation.pictures.thumbnails,
            ),
            PrefKey::FolderContents => overwrite(
                &mut self.presentation.pictures.folder_contents,
                other.presentation.pictures.folder_contents,
            ),
            PrefKey::ConfirmTrash => overwrite(&mut self.confirm_trash, other.confirm_trash),
        }
    }
}

/// A view's spelling in the store.
const fn view_token(view: ViewMode) -> &'static str {
    match view {
        ViewMode::LargeIcons => "large-icons",
        ViewMode::SmallIcons => "small-icons",
        ViewMode::List => "list",
    }
}

/// A key's spelling in the store.
const fn key_token(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::Kind => "type",
        SortKey::Size => "size",
        SortKey::Modified => "modified",
        SortKey::Free => "free",
        SortKey::Filesystem => "filesystem",
        SortKey::Health => "health",
    }
}

/// A direction's spelling in the store.
const fn direction_token(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "ascending",
        SortDirection::Descending => "descending",
    }
}

/// The toolbar band a new window shows, `shown` or not.
const fn band(shown: bool) -> ToolbarBand {
    if shown {
        ToolbarBand::Shown
    } else {
        ToolbarBand::Hidden
    }
}

/// Where folders stand, `first` or among the files.
const fn folders(first: bool) -> Folders {
    if first {
        Folders::First
    } else {
        Folders::Among
    }
}

/// The one of `all` that `token` spells as `text`.
fn token_of<T: Copy, const N: usize>(
    all: [T; N],
    token: fn(T) -> &'static str,
    text: &str,
) -> Option<T> {
    all.into_iter().find(|&value| token(value) == text)
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
