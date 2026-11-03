//! The file manager's settables: the Files pane's rows over
//! [`FilerPreferences`], which the desktop session keeps in its document
//! beside its own settings and adopts a change to through an apply of its
//! own.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_browse::{
    FilerPreferences, Folders, SortDirection, SortKey, Start, ToolbarBand, ViewMode, SORT_KEYS,
};
use tairix_controls::{ComboBox, FieldControl, FieldRow};

use crate::form::{pick, set, switch_label, SWITCH};
use crate::machine::UNREAD;

/// One settable of the file manager's settings.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FilesSetting {
    /// Where a window asked for no folder opens.
    Start,
    /// Whether a new window shows the places rail.
    Places,
    /// Whether a new window shows the command toolbar.
    Toolbar,
    /// The display every window shows its items in.
    View,
    /// The key every window orders its items by.
    SortBy,
    /// The direction every window orders its items in.
    Order,
    /// Whether folders stand before the files.
    FoldersFirst,
    /// Whether an application's `.app` ending is hidden.
    HideAppEndings,
    /// Whether a known type's ending is hidden.
    HideKnownEndings,
    /// Whether items whose names begin with a dot are listed.
    HiddenItems,
    /// Whether a picture is drawn as its own content.
    Thumbnails,
    /// Whether a folder is drawn as what it holds.
    FolderContents,
    /// Whether moving items to the Trash asks first.
    ConfirmTrash,
    /// Whether a disk image or archive the user opened is unmounted once its
    /// windows close.
    UnmountWhenClosed,
}

impl FilesSetting {
    /// The row's leading label, which is also its search term.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Start => "Open new windows at",
            Self::Places => "Show the places rail",
            Self::Toolbar => "Show the toolbar",
            Self::View => "Show items as",
            Self::SortBy => "Sort by",
            Self::Order => "Order",
            Self::FoldersFirst => "Keep folders on top",
            Self::HideAppEndings => "Hide the .app ending of applications",
            Self::HideKnownEndings => "Hide the endings of known file types",
            Self::HiddenItems => "Show hidden items",
            Self::Thumbnails => "Show pictures as their content",
            Self::FolderContents => "Show what folders hold",
            Self::ConfirmTrash => "Ask before moving items to the Trash",
            Self::UnmountWhenClosed => "Unmount disk images once closed",
        }
    }

    /// The sentence beneath the row's label.
    const fn description(self) -> &'static str {
        match self {
            Self::Start => "Where a window opens when it is asked for no folder in particular.",
            Self::Places => "Whether a new window shows your places and the attached storage.",
            Self::Toolbar => "Whether a new window shows the toolbar.",
            Self::View => "Every window shows its items so, unless you chose another in it.",
            Self::SortBy | Self::Order => {
                "Every window orders its items so, unless you sorted it yourself."
            }
            Self::FoldersFirst => "Folders come first whichever way a window is sorted.",
            Self::HideAppEndings => "An application is shown by its name alone.",
            Self::HideKnownEndings => "Renaming an item keeps its ending.",
            Self::HiddenItems => "Items whose names begin with a dot.",
            Self::Thumbnails => "A picture is drawn as what it shows, once it has been read.",
            Self::FolderContents => "A folder is drawn holding pictures of what is in it.",
            Self::ConfirmTrash => "Deleting for good always asks.",
            Self::UnmountWhenClosed => {
                "A disk image or archive you open is unmounted when its last window closes, \
                 unless something else is still using it. Turning this on also unmounts any \
                 you opened whose windows have already closed."
            }
        }
    }

    /// The labels this settable offers, and the index of the one `settings`
    /// hold, in the order [`adopt`](Self::adopt) takes them.
    fn offer(self, settings: &FilerPreferences) -> (Vec<String>, usize) {
        let switch = |on| pick(&SWITCH, on, switch_label);
        match self {
            Self::Start => pick(&Start::ALL, settings.start, Start::label),
            Self::Places => switch(settings.places),
            Self::Toolbar => switch(settings.toolbar.is_shown()),
            Self::View => pick(&ViewMode::ALL, settings.view, ViewMode::label),
            Self::SortBy => pick(&SORT_KEYS, settings.sort.key, SortKey::name),
            Self::Order => pick(
                &SortDirection::ALL,
                settings.sort.direction,
                SortDirection::label,
            ),
            Self::FoldersFirst => switch(settings.sort.folders == Folders::First),
            Self::HideAppEndings => switch(settings.presentation.endings.applications),
            Self::HideKnownEndings => switch(settings.presentation.endings.known_types),
            Self::HiddenItems => switch(settings.presentation.hidden_items),
            Self::Thumbnails => switch(settings.presentation.pictures.thumbnails),
            Self::FolderContents => switch(settings.presentation.pictures.folder_contents),
            Self::ConfirmTrash => switch(settings.confirm_trash),
            Self::UnmountWhenClosed => switch(settings.unmount_when_closed),
        }
    }

    /// The row this settable draws, showing what `settings` hold, or that
    /// none have been read: a choice offered over values nobody read would
    /// post them.
    pub(crate) fn row(self, settings: Option<&FilerPreferences>) -> FieldRow {
        let Some(settings) = settings else {
            return FieldRow::new(self.label(), FieldControl::Unmeasured(String::from(UNREAD)))
                .with_description(self.description());
        };
        let (choices, held) = self.offer(settings);
        let mut combo = ComboBox::new(choices);
        combo.set_selected(held);
        FieldRow::new(self.label(), FieldControl::Combo(combo)).with_description(self.description())
    }

    /// Take the choice at `index` into `settings`, answering whether the
    /// index named one. An index outside the list [`offer`](Self::offer)
    /// built changes nothing.
    pub(crate) fn adopt(self, index: usize, settings: &mut FilerPreferences) -> bool {
        let switched = |on: &mut bool| set(&SWITCH, index, on);
        let picked = SWITCH.get(index).copied();
        match self {
            Self::Start => set(&Start::ALL, index, &mut settings.start),
            Self::View => set(&ViewMode::ALL, index, &mut settings.view),
            Self::SortBy => set(&SORT_KEYS, index, &mut settings.sort.key),
            Self::Order => set(&SortDirection::ALL, index, &mut settings.sort.direction),
            Self::Places => switched(&mut settings.places),
            Self::Toolbar => picked
                .map(|shown| settings.toolbar = ToolbarBand::shown_if(shown))
                .is_some(),
            Self::FoldersFirst => picked
                .map(|first| settings.sort.folders = Folders::first_if(first))
                .is_some(),
            Self::HideAppEndings => switched(&mut settings.presentation.endings.applications),
            Self::HideKnownEndings => switched(&mut settings.presentation.endings.known_types),
            Self::HiddenItems => switched(&mut settings.presentation.hidden_items),
            Self::Thumbnails => switched(&mut settings.presentation.pictures.thumbnails),
            Self::FolderContents => switched(&mut settings.presentation.pictures.folder_contents),
            Self::ConfirmTrash => switched(&mut settings.confirm_trash),
            Self::UnmountWhenClosed => switched(&mut settings.unmount_when_closed),
        }
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
