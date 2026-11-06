use alloc::string::String;
use alloc::vec::Vec;

use tairix_appconf::{Document, Registry};

use super::{FilerKey, FilerPreferences, Start, SORT_KEYS};
use crate::chrome::ToolbarBand;
use crate::layout::ViewMode;
use crate::render::ListColumns;
use crate::sort::{Folders, SortDirection, SortKey, SortMode};
use crate::volume::{DesktopDrives, DriveClass};

/// Every setting moved off its default.
fn moved() -> FilerPreferences {
    let mut moved = FilerPreferences {
        start: Start::Storage,
        view: ViewMode::List,
        sort: SortMode {
            key: SortKey::Modified,
            direction: SortDirection::Descending,
            folders: Folders::Among,
        },
        places: true,
        toolbar: ToolbarBand::Shown,
        confirm_trash: false,
        unmount_when_closed: false,
        desktop: DriveClass::ALL
            .into_iter()
            .fold(DesktopDrives::NONE, |drives, class| {
                drives.with(class, class != DriveClass::Removable)
            }),
        ..FilerPreferences::default()
    };
    moved.presentation.endings.applications = false;
    moved.presentation.endings.known_types = true;
    moved.presentation.hidden_items = true;
    moved.presentation.pictures.thumbnails = false;
    moved.presentation.pictures.folder_contents = false;
    moved
}

#[test]
fn a_new_window_opens_on_large_icons_and_an_opened_image_is_unmounted_when_closed() {
    let defaults = FilerPreferences::default();
    assert_eq!(defaults.start, Start::Home);
    assert_eq!(defaults.view, ViewMode::LargeIcons);
    assert_eq!(defaults.sort, SortMode::default_order());
    assert!(!defaults.places);
    assert_eq!(defaults.toolbar, ToolbarBand::Hidden);
    assert!(defaults.presentation.endings.applications);
    assert!(!defaults.presentation.endings.known_types);
    assert!(!defaults.presentation.hidden_items);
    assert!(defaults.confirm_trash);
    assert!(defaults.unmount_when_closed);
    assert_eq!(
        defaults.desktop,
        DesktopDrives::NONE.with(DriveClass::Removable, true),
        "removable drives alone are shown on the desktop"
    );
}

#[test]
fn every_setting_survives_its_spelling() {
    let moved = moved();
    let document = moved.document_of(FilerPreferences::KEYS);
    assert_eq!(FilerPreferences::load(&document), (moved, Vec::new()));
    assert_eq!(document.get("files.start"), Some("storage"));
    assert_eq!(document.get("files.view"), Some("list"));
    assert_eq!(document.get("files.sort_by"), Some("modified"));
    assert_eq!(document.get("files.folders_first"), Some("false"));
    assert_eq!(document.get("files.unmount_when_closed"), Some("false"));
    assert_eq!(document.get("files.desktop.system"), Some("true"));
    assert_eq!(document.get("files.desktop.removable"), Some("false"));
    assert_eq!(
        FilerPreferences::load(&FilerPreferences::default().document_of(FilerPreferences::KEYS)),
        (FilerPreferences::default(), Vec::new())
    );
}

#[test]
fn every_key_is_the_file_managers_own() {
    for key in FilerKey::ALL {
        assert!(key.name().starts_with("files."), "{key}");
        assert_eq!(
            FilerKey::ALL
                .iter()
                .filter(|other| other.name() == key.name())
                .count(),
            1,
            "{key} is spelled once"
        );
    }
}

#[test]
fn a_misspelt_value_costs_only_itself() {
    let document = Document::parse(
        "files.view = huge_icons\n\
         files.sort_by = free\n\
         files.hidden_items = true\n",
    )
    .expect("a small document");
    let (read, refused) = FilerPreferences::load(&document);
    assert_eq!(refused, [FilerKey::View, FilerKey::SortBy]);
    assert_eq!(read.view, FilerPreferences::default().view);
    assert_eq!(read.sort, FilerPreferences::default().sort);
    assert!(read.presentation.hidden_items);
}

#[test]
fn a_document_of_other_settings_reads_as_the_defaults() {
    let document = Document::parse("appearance = dark\nwindow.view = list\n").expect("parses");
    assert_eq!(
        FilerPreferences::load(&document),
        (FilerPreferences::default(), Vec::new()),
        "keys outside `files.` are another registry's"
    );
}

#[test]
fn each_start_names_its_own_folder() {
    let home: Vec<String> = ["Users", "ann"].map(String::from).to_vec();
    let spelled = |start: Start| start.place(Some(&home));
    assert_eq!(
        spelled(Start::Home),
        Some(["Users", "ann", "Home"].map(String::from).to_vec())
    );
    assert_eq!(
        spelled(Start::Desktop),
        Some(["Users", "ann", "Desktop"].map(String::from).to_vec())
    );
    assert_eq!(
        spelled(Start::Storage),
        Some(alloc::vec![String::from("Storage")])
    );
    assert_eq!(Start::Home.place(None), None);
    assert!(Start::Storage.place(None).is_some());
}

#[test]
fn a_window_with_an_order_of_its_own_keeps_it_and_takes_where_folders_stand() {
    let settings = moved();
    let own = SortMode {
        key: SortKey::Size,
        direction: SortDirection::Ascending,
        folders: Folders::First,
    };
    assert_eq!(
        settings.reorder(own),
        SortMode {
            folders: Folders::Among,
            ..own
        }
    );
}

#[test]
fn a_new_window_is_offered_the_orders_a_folders_columns_list() {
    assert_eq!(SORT_KEYS, ListColumns::Files.keys());
}
