use tairix_abi::time::Time64;

use super::{HiddenEndings, Presentation};
use crate::entry::{Entry, EntryKind, LinkTarget};

const APPS: HiddenEndings = HiddenEndings {
    applications: true,
    known_types: false,
};

const KNOWN: HiddenEndings = HiddenEndings {
    applications: false,
    known_types: true,
};

fn bundle(name: &str) -> Entry {
    Entry::new(name, EntryKind::Bundle, 0, Time64::UNIX_EPOCH)
}

fn link(name: &str, target: LinkTarget) -> Entry {
    Entry::new(name, EntryKind::Link(target), 0, Time64::UNIX_EPOCH)
}

/// Every shown name and the ending it leaves off make up the name again.
fn assert_split(endings: HiddenEndings, entry: &Entry, shown: &str) {
    assert_eq!(endings.shown(entry), shown, "{:?}", entry.name());
    let mut whole = alloc::string::String::from(endings.shown(entry));
    whole.push_str(endings.hidden(entry));
    assert_eq!(whole, entry.name());
}

#[test]
fn an_application_is_shown_without_its_ending_only_when_asked() {
    assert_split(HiddenEndings::NONE, &bundle("Paint.app"), "Paint.app");
    assert_split(APPS, &bundle("Paint.app"), "Paint");
    assert_split(APPS, &bundle("Old Paint.APP"), "Old Paint");
    assert_split(KNOWN, &bundle("Paint.app"), "Paint.app");
}

#[test]
fn a_link_is_shown_by_what_it_names() {
    assert_split(APPS, &link("Paint.app", LinkTarget::Bundle), "Paint");
    assert_split(APPS, &link("Paint", LinkTarget::Bundle), "Paint");
    assert_split(KNOWN, &link("notes.txt", LinkTarget::File), "notes");
    assert_split(KNOWN, &link("notes.txt", LinkTarget::Dangling), "notes.txt");
}

#[test]
fn a_file_is_shown_without_an_ending_the_registry_knows() {
    assert_split(KNOWN, &Entry::file("notes.txt"), "notes");
    assert_split(KNOWN, &Entry::file("archive.tar.gz"), "archive.tar");
    assert_split(KNOWN, &Entry::file("picture,c85"), "picture");
    assert_split(
        KNOWN,
        &Entry::file("data.unknownending"),
        "data.unknownending",
    );
    assert_split(APPS, &Entry::file("notes.txt"), "notes.txt");
}

#[test]
fn a_folder_or_a_name_that_would_be_left_empty_is_shown_whole() {
    let both = HiddenEndings {
        applications: true,
        known_types: true,
    };
    assert_split(both, &Entry::directory("photos.png"), "photos.png");
    assert_split(both, &Entry::file(".txt"), ".txt");
    assert_split(both, &bundle(".app"), ".app");
}

#[test]
fn a_hidden_item_is_listed_only_when_asked() {
    let hiding = Presentation {
        hidden_items: false,
        ..Presentation::EXACT
    };
    assert!(Presentation::EXACT.lists(".profile"));
    assert!(!hiding.lists(".profile"));
    assert!(hiding.lists("notes.txt"));
    assert_eq!(Presentation::default(), Presentation::EXACT);
}
