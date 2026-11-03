//! The file manager's settings: where a new window opens and what it shows,
//! how every window orders, names and pictures what it lists, whether moving
//! items to the Trash asks first, and whether a disk image or archive the user
//! opened is unmounted once the last window showing it closes.
//!
//! They are the user's desktop settings: the desktop session keeps them in its
//! own document beside its other settings, the Settings application edits them
//! there, and the file manager follows them.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::home::{HOME_DESKTOP_DIR, HOME_FILES_DIR};
use tairix_abi::STORAGE_CATALOG;
use tairix_appconf::{as_bool, bool_text, Registry};

use crate::chrome::ToolbarBand;
use crate::layout::ViewMode;
use crate::presentation::Presentation;
use crate::render::ListColumns;
use crate::sort::{Folders, SortDirection, SortKey, SortMode};
use crate::{MANAGER_PRESENTATION, MANAGER_TOOLBAR_BAND, MANAGER_VIEW_MODE};

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

/// The name of the Settings pane that edits these settings: what the file
/// manager hands over as its launch target.
///
/// Spelled once because the file manager names it and the settings
/// application resolves it against its own pane registry, and neither may
/// depend on the other. It confers nothing.
pub const SETTINGS_PANE: &str = "files";

/// The orders a new window may open sorted by: a folder's own columns, in
/// the order they stand.
pub const SORT_KEYS: [SortKey; 4] = ListColumns::Files.keys();

/// Every setting of the file manager.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FilerPreferences {
    /// Where a window asked for no folder opens.
    pub start: Start,
    /// The view a new window opens in, and that every window shows until it
    /// is given a view of its own.
    pub view: ViewMode,
    /// The order a new window opens sorted in, and where every window puts
    /// its folders.
    pub sort: SortMode,
    /// Whether a new window shows the places rail.
    pub places: bool,
    /// Whether a new window shows the command toolbar.
    pub toolbar: ToolbarBand,
    /// How every window presents what it lists.
    pub presentation: Presentation,
    /// Whether moving items to the Trash asks first.
    pub confirm_trash: bool,
    /// Whether a disk image or archive the user opened is unmounted once the
    /// last window showing it closes, where nothing else is using it.
    pub unmount_when_closed: bool,
}

impl Default for FilerPreferences {
    fn default() -> Self {
        Self {
            start: Start::Home,
            view: MANAGER_VIEW_MODE,
            sort: SortMode::default_order(),
            places: false,
            toolbar: MANAGER_TOOLBAR_BAND,
            presentation: MANAGER_PRESENTATION,
            confirm_trash: true,
            unmount_when_closed: true,
        }
    }
}

impl FilerPreferences {
    /// The order a window with an order of its own is to keep with these
    /// settings: its own key and direction, and where these put folders.
    #[must_use]
    pub const fn reorder(&self, order: SortMode) -> SortMode {
        SortMode {
            folders: self.sort.folders,
            ..order
        }
    }
}

/// One key of the file manager's settings, every one under `files.`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FilerKey {
    /// `files.start`.
    Start,
    /// `files.view`.
    View,
    /// `files.sort_by`.
    SortBy,
    /// `files.order`.
    Order,
    /// `files.folders_first`.
    FoldersFirst,
    /// `files.places`.
    Places,
    /// `files.toolbar`.
    Toolbar,
    /// `files.hide_app_endings`.
    HideAppEndings,
    /// `files.hide_known_endings`.
    HideKnownEndings,
    /// `files.hidden_items`.
    HiddenItems,
    /// `files.thumbnails`.
    Thumbnails,
    /// `files.folder_contents`.
    FolderContents,
    /// `files.confirm_trash`.
    ConfirmTrash,
    /// `files.unmount_when_closed`.
    UnmountWhenClosed,
}

impl FilerKey {
    /// Every key, in the order a document lists them.
    pub const ALL: [Self; 14] = [
        Self::Start,
        Self::View,
        Self::SortBy,
        Self::Order,
        Self::FoldersFirst,
        Self::Places,
        Self::Toolbar,
        Self::HideAppEndings,
        Self::HideKnownEndings,
        Self::HiddenItems,
        Self::Thumbnails,
        Self::FolderContents,
        Self::ConfirmTrash,
        Self::UnmountWhenClosed,
    ];

    /// The key's spelling in the document.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Start => "files.start",
            Self::View => "files.view",
            Self::SortBy => "files.sort_by",
            Self::Order => "files.order",
            Self::FoldersFirst => "files.folders_first",
            Self::Places => "files.places",
            Self::Toolbar => "files.toolbar",
            Self::HideAppEndings => "files.hide_app_endings",
            Self::HideKnownEndings => "files.hide_known_endings",
            Self::HiddenItems => "files.hidden_items",
            Self::Thumbnails => "files.thumbnails",
            Self::FolderContents => "files.folder_contents",
            Self::ConfirmTrash => "files.confirm_trash",
            Self::UnmountWhenClosed => "files.unmount_when_closed",
        }
    }
}

impl core::fmt::Display for FilerKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

impl Registry for FilerPreferences {
    type Key = FilerKey;
    const KEYS: &'static [FilerKey] = &FilerKey::ALL;

    fn name(key: FilerKey) -> &'static str {
        key.name()
    }

    fn read(&mut self, key: FilerKey, text: &str) -> bool {
        let flag = |slot: &mut bool| as_bool(text).map(|on| *slot = on).is_ok();
        match key {
            FilerKey::Start => token_of(Start::ALL, Start::token, text)
                .map(|start| self.start = start)
                .is_some(),
            FilerKey::View => token_of(ViewMode::ALL, view_token, text)
                .map(|view| self.view = view)
                .is_some(),
            FilerKey::SortBy => token_of(SORT_KEYS, key_token, text)
                .map(|by| self.sort.key = by)
                .is_some(),
            FilerKey::Order => token_of(SortDirection::ALL, direction_token, text)
                .map(|direction| self.sort.direction = direction)
                .is_some(),
            FilerKey::FoldersFirst => as_bool(text)
                .map(|first| self.sort.folders = Folders::first_if(first))
                .is_ok(),
            FilerKey::Places => flag(&mut self.places),
            FilerKey::Toolbar => as_bool(text)
                .map(|shown| self.toolbar = ToolbarBand::shown_if(shown))
                .is_ok(),
            FilerKey::HideAppEndings => flag(&mut self.presentation.endings.applications),
            FilerKey::HideKnownEndings => flag(&mut self.presentation.endings.known_types),
            FilerKey::HiddenItems => flag(&mut self.presentation.hidden_items),
            FilerKey::Thumbnails => flag(&mut self.presentation.pictures.thumbnails),
            FilerKey::FolderContents => flag(&mut self.presentation.pictures.folder_contents),
            FilerKey::ConfirmTrash => flag(&mut self.confirm_trash),
            FilerKey::UnmountWhenClosed => flag(&mut self.unmount_when_closed),
        }
    }

    fn spell(&self, key: FilerKey, out: &mut String) -> bool {
        let text = match key {
            FilerKey::Start => self.start.token(),
            FilerKey::View => view_token(self.view),
            FilerKey::SortBy => key_token(self.sort.key),
            FilerKey::Order => direction_token(self.sort.direction),
            FilerKey::FoldersFirst => bool_text(self.sort.folders == Folders::First),
            FilerKey::Places => bool_text(self.places),
            FilerKey::Toolbar => bool_text(self.toolbar.is_shown()),
            FilerKey::HideAppEndings => bool_text(self.presentation.endings.applications),
            FilerKey::HideKnownEndings => bool_text(self.presentation.endings.known_types),
            FilerKey::HiddenItems => bool_text(self.presentation.hidden_items),
            FilerKey::Thumbnails => bool_text(self.presentation.pictures.thumbnails),
            FilerKey::FolderContents => bool_text(self.presentation.pictures.folder_contents),
            FilerKey::ConfirmTrash => bool_text(self.confirm_trash),
            FilerKey::UnmountWhenClosed => bool_text(self.unmount_when_closed),
        };
        out.push_str(text);
        true
    }
}

/// A view's spelling in the document.
const fn view_token(view: ViewMode) -> &'static str {
    match view {
        ViewMode::LargeIcons => "large_icons",
        ViewMode::SmallIcons => "small_icons",
        ViewMode::List => "list",
    }
}

/// A key's spelling in the document; only [`SORT_KEYS`] are ever read.
const fn key_token(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::Kind => "kind",
        SortKey::Size => "size",
        SortKey::Modified => "modified",
        SortKey::Free => "free",
        SortKey::Filesystem => "filesystem",
        SortKey::Health => "health",
        SortKey::Address => "address",
        SortKey::Workgroup => "workgroup",
    }
}

/// A direction's spelling in the document.
const fn direction_token(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "ascending",
        SortDirection::Descending => "descending",
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
