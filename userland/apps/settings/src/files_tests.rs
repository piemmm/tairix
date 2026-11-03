use alloc::vec::Vec;

use tairix_browse::{FilerPreferences, Folders, SortKey, Start, ToolbarBand, ViewMode};
use tairix_controls::FieldControl;

use super::FilesSetting;

/// Every settable, in the order the pane lists them.
const ALL: [FilesSetting; 14] = [
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

/// Every choice a row offers takes, reads back as held, and leaves every
/// other settable as it was.
#[test]
fn every_choice_a_row_offers_is_taken_and_read_back_alone() {
    for setting in ALL {
        let (choices, _) = setting.offer(&FilerPreferences::default());
        assert!(choices.len() >= 2, "{setting:?} offers a choice");
        for index in 0..choices.len() {
            let mut settings = FilerPreferences::default();
            assert!(setting.adopt(index, &mut settings), "{setting:?} {index}");
            assert_eq!(setting.offer(&settings).1, index, "{setting:?} {index}");
            for other in ALL.into_iter().filter(|other| *other != setting) {
                let shared = matches!(
                    (setting, other),
                    (FilesSetting::SortBy, FilesSetting::Order)
                        | (FilesSetting::Order, FilesSetting::SortBy)
                );
                if !shared {
                    assert_eq!(
                        other.offer(&settings).1,
                        other.offer(&FilerPreferences::default()).1,
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
    for setting in ALL {
        let mut settings = FilerPreferences::default();
        let past = setting.offer(&settings).0.len();
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
        FilesSetting::Start.offer(&defaults).0,
        Start::ALL.map(Start::label).to_vec()
    );
    assert_eq!(
        FilesSetting::View.offer(&defaults).0,
        ViewMode::ALL.map(ViewMode::label).to_vec()
    );
    assert_eq!(
        FilesSetting::SortBy.offer(&defaults).0,
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
    let labels: Vec<&str> = ALL.map(FilesSetting::label).to_vec();
    for setting in ALL {
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
/// from values nobody read.
#[test]
fn a_row_with_nothing_read_offers_no_choice() {
    for setting in ALL {
        let unread = setting.row(None);
        assert_eq!(unread.label(), setting.label());
        assert!(
            matches!(unread.control(), FieldControl::Unmeasured(_)),
            "{setting:?}"
        );
        assert!(
            matches!(
                setting.row(Some(&FilerPreferences::default())).control(),
                FieldControl::Combo(_)
            ),
            "{setting:?}"
        );
    }
}
