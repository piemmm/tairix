use alloc::string::String;
use alloc::vec::Vec;

use tairix_browse::{DriveClass, FilerPreferences, Folders, SortKey, Start, ToolbarBand, ViewMode};
use tairix_controls::{FieldControl, SelectionState};

use super::FilesSetting;

/// Every settable offering a list of choices, in the order the pane lists
/// them.
const CHOICES: [FilesSetting; 14] = [
    FilesSetting::Start,
    FilesSetting::Places,
    FilesSetting::Toolbar,
    FilesSetting::View,
    FilesSetting::SortBy,
    FilesSetting::Order,
    FilesSetting::FoldersFirst,
    FilesSetting::HideAppEndings,
    FilesSetting::HideKnownEndings,
    FilesSetting::HiddenItems,
    FilesSetting::Thumbnails,
    FilesSetting::FolderContents,
    FilesSetting::ConfirmTrash,
    FilesSetting::UnmountWhenClosed,
];

/// Every kind of drive's box, in the order the pane lists them.
fn drives() -> [FilesSetting; 5] {
    DriveClass::ALL.map(FilesSetting::Desktop)
}

/// Every settable, the lists first.
fn every() -> impl Iterator<Item = FilesSetting> {
    CHOICES.into_iter().chain(drives())
}

/// What `setting`, which offers a list, offers over `settings`.
fn offered(setting: FilesSetting, settings: &FilerPreferences) -> (Vec<String>, usize) {
    setting.offer(settings).expect("a list of choices")
}

/// Every choice a row offers takes, reads back as held, and leaves every
/// other settable as it was.
#[test]
fn every_choice_a_row_offers_is_taken_and_read_back_alone() {
    let defaults = FilerPreferences::default();
    for setting in CHOICES {
        let (choices, _) = offered(setting, &defaults);
        assert!(choices.len() >= 2, "{setting:?} offers a choice");
        for index in 0..choices.len() {
            let mut settings = defaults;
            assert!(setting.adopt(index, &mut settings), "{setting:?} {index}");
            assert_eq!(offered(setting, &settings).1, index, "{setting:?} {index}");
            assert_eq!(settings.desktop, defaults.desktop, "{setting:?} {index}");
            for other in CHOICES.into_iter().filter(|other| *other != setting) {
                let shared = matches!(
                    (setting, other),
                    (FilesSetting::SortBy, FilesSetting::Order)
                        | (FilesSetting::Order, FilesSetting::SortBy)
                );
                if !shared {
                    assert_eq!(
                        offered(other, &settings).1,
                        offered(other, &defaults).1,
                        "{setting:?} moved {other:?}"
                    );
                }
            }
        }
    }
}

/// An index past what a row offers changes nothing: a routing defect cannot
/// post a setting the reader did not choose.
#[test]
fn a_choice_the_row_never_offered_changes_nothing() {
    for setting in CHOICES {
        let mut settings = FilerPreferences::default();
        let past = offered(setting, &settings).0.len();
        assert!(!setting.adopt(past, &mut settings), "{setting:?}");
        assert_eq!(settings, FilerPreferences::default(), "{setting:?}");
    }
}

/// The choices name what they set, in the order the file manager lists
/// them, so the pane reads as it does.
#[test]
fn the_choices_are_the_file_managers_own_words() {
    let defaults = FilerPreferences::default();
    assert_eq!(
        offered(FilesSetting::Start, &defaults).0,
        Start::ALL.map(Start::label).to_vec()
    );
    assert_eq!(
        offered(FilesSetting::View, &defaults).0,
        ViewMode::ALL.map(ViewMode::label).to_vec()
    );
    assert_eq!(
        offered(FilesSetting::SortBy, &defaults).0,
        ["Name", "Size", "Date modified", "Type"]
    );
    let mut settings = defaults;
    assert!(FilesSetting::Toolbar.adopt(0, &mut settings));
    assert_eq!(settings.toolbar, ToolbarBand::Shown);
    assert!(FilesSetting::FoldersFirst.adopt(1, &mut settings));
    assert_eq!(settings.sort.folders, Folders::Among);
    assert!(FilesSetting::SortBy.adopt(2, &mut settings));
    assert_eq!(settings.sort.key, SortKey::Modified);
}

/// A label is a search term, so each is spelled once and none collides with
/// another pane's words.
#[test]
fn each_label_is_its_own_search_term() {
    let labels: Vec<&str> = every().map(FilesSetting::label).collect();
    for setting in every() {
        let label = setting.label();
        assert_eq!(labels.iter().filter(|other| **other == label).count(), 1);
        let lower = label.to_ascii_lowercase();
        for word in ["keyboard", "dns", "caching", "bluetooth", "microphone"] {
            assert!(!lower.contains(word), "{label} reaches another pane");
        }
        assert!(!setting.description().is_empty(), "{setting:?}");
    }
}

/// With nothing read a row says so and offers no choice, so no apply is built
/// from values nobody read; once read it offers a list, or a box for a kind of
/// drive.
#[test]
fn a_row_with_nothing_read_offers_no_choice() {
    for setting in every() {
        let unread = setting.row(None);
        assert_eq!(unread.label(), setting.label());
        assert!(
            matches!(unread.control(), FieldControl::Unmeasured(_)),
            "{setting:?}"
        );
        let drawn = match setting.row(Some(&FilerPreferences::default())).control() {
            FieldControl::Combo(_) => Some(false),
            FieldControl::Checkbox(_) => Some(true),
            _ => None,
        };
        assert_eq!(
            drawn,
            Some(matches!(setting, FilesSetting::Desktop(_))),
            "{setting:?}"
        );
    }
}

/// Each kind of drive is a box of its own, ticked where the desktop shows it
/// — removable drives alone at first — which ticks or clears its kind alone
/// and takes no choice from a list; a list takes no tick.
#[test]
fn each_kind_of_drive_is_a_box_that_moves_its_kind_alone() {
    let defaults = FilerPreferences::default();
    for (setting, class) in drives().into_iter().zip(DriveClass::ALL) {
        assert_eq!(setting.label(), class.label());
        assert!(setting.offer(&defaults).is_none(), "{setting:?}");
        let row = setting.row(Some(&defaults));
        let FieldControl::Checkbox(shown) = row.control() else {
            panic!("{setting:?} draws a box");
        };
        assert_eq!(
            shown.selection(),
            SelectionState::of(class == DriveClass::Removable),
            "{setting:?}"
        );
        let mut untouched = defaults;
        assert!(!setting.adopt(0, &mut untouched), "{setting:?}");
        assert_eq!(untouched, defaults, "{setting:?}");
        for on in [true, false] {
            let mut settings = defaults;
            assert!(setting.tick(on, &mut settings), "{setting:?} {on}");
            assert_eq!(settings.desktop.shows(class), on, "{setting:?} {on}");
            *settings.desktop.setting_mut(class) = defaults.desktop.shows(class);
            assert_eq!(settings, defaults, "{setting:?} {on} moved another");
        }
    }
    for setting in CHOICES {
        let mut settings = defaults;
        assert!(!setting.tick(true, &mut settings), "{setting:?}");
        assert_eq!(settings, defaults, "{setting:?}");
    }
}
