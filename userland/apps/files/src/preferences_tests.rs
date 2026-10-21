use alloc::string::String;
use alloc::vec::Vec;

use tairix_appconf::{Document, Registry};
use tairix_browse::{Folders, SortDirection, SortKey, SortMode, ToolbarBand, ViewMode};

use super::{PrefKey, Preferences, Start};
use crate::chrome::Chrome;

/// Every setting moved off its default.
fn moved() -> Preferences {
    let mut moved = Preferences {
        start: Start::Storage,
        view: ViewMode::List,
        sort: SortMode {
            key: SortKey::Modified,
            direction: SortDirection::Descending,
            folders: Folders::Among,
        },
        chrome: Chrome {
            rail: true,
            toolbar: ToolbarBand::Shown,
        },
        confirm_trash: false,
        ..Preferences::default()
    };
    moved.presentation.endings.applications = false;
    moved.presentation.endings.known_types = true;
    moved.presentation.hidden_items = true;
    moved.presentation.pictures.thumbnails = false;
    moved.presentation.pictures.folder_contents = false;
    moved
}

#[test]
fn a_new_window_opens_on_large_icons_and_the_app_ending_is_hidden() {
    let defaults = Preferences::default();
    assert_eq!(defaults.start, Start::Home);
    assert_eq!(defaults.view, ViewMode::LargeIcons);
    assert_eq!(defaults.sort, SortMode::default_order());
    assert_eq!(defaults.chrome, Chrome::HIDDEN);
    assert!(defaults.presentation.endings.applications);
    assert!(!defaults.presentation.endings.known_types);
    assert!(!defaults.presentation.hidden_items);
    assert!(defaults.confirm_trash);
}

#[test]
fn every_setting_survives_its_spelling() {
    let moved = moved();
    let document = moved.document_of(Preferences::KEYS);
    assert_eq!(Preferences::load(&document), (moved, Vec::new()));
    assert_eq!(document.get("window.start"), Some("storage"));
    assert_eq!(document.get("window.sort-by"), Some("modified"));
    assert_eq!(document.get("order.folders-first"), Some("false"));
    assert_eq!(
        Preferences::load(&Preferences::default().document_of(Preferences::KEYS)),
        (Preferences::default(), Vec::new())
    );
}

#[test]
fn a_misspelt_value_costs_only_itself() {
    let document = Document::parse(
        "window.view = huge-icons\n\
         window.sort-by = free\n\
         names.hidden-items = true\n",
    )
    .expect("a small document");
    let (read, refused) = Preferences::load(&document);
    assert_eq!(refused, [PrefKey::View, PrefKey::SortBy]);
    assert_eq!(read.view, Preferences::default().view);
    assert_eq!(read.sort, Preferences::default().sort);
    assert!(read.presentation.hidden_items);
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
fn an_open_window_keeps_its_own_order_and_takes_where_folders_stand() {
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
