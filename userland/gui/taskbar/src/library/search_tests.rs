//! The search's ranking, matching, and bounds.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::testkit::emphasis_runs;
use tairix_proglib::{BundlePath, DisplayName, EntryId, LibraryCategory, LibraryEntry};

use super::{search, terms, Indexed, MAX_TERMS, MAX_TERM_CHARS};

fn program(id: &str, name: &str, category: LibraryCategory, purpose: Option<&str>) -> Indexed {
    let entry = LibraryEntry::new(
        EntryId::new(id).expect("id"),
        DisplayName::new(name).expect("name"),
        BundlePath::new(&alloc::format!("/System/Applications/{id}.app")).expect("bundle"),
        category,
        None,
    );
    Indexed::new(&entry, purpose, crate::library::folder_label(category))
}

fn index() -> Vec<Indexed> {
    alloc::vec![
        program(
            "terminal",
            "Terminal",
            LibraryCategory::SystemTools,
            Some("A command line")
        ),
        program(
            "textedit",
            "TextEdit",
            LibraryCategory::Accessories,
            Some("Edit plain text")
        ),
        program("notes", "Notes", LibraryCategory::Accessories, None),
        program(
            "paint",
            "Paint",
            LibraryCategory::Graphics,
            Some("Draw and edit images")
        ),
        program(
            "view",
            "View",
            LibraryCategory::Graphics,
            Some("Look at images and documents")
        ),
        program("files", "File Manager", LibraryCategory::SystemTools, None),
        program("chess", "Chess", LibraryCategory::Games, None),
        program("cafe", "Café Racer", LibraryCategory::Games, None),
    ]
}

fn names(index: &[Indexed], query: &str) -> Vec<String> {
    names_weighted(index, query, &BTreeMap::new())
}

fn names_weighted(index: &[Indexed], query: &str, weights: &BTreeMap<EntryId, u32>) -> Vec<String> {
    search(index, query, weights)
        .into_iter()
        .map(|hit| String::from(index[hit.index].id().as_str()))
        .collect()
}

#[test]
fn a_name_that_begins_with_the_query_ranks_above_one_that_holds_it() {
    let index = index();
    assert_eq!(names(&index, "te"), ["terminal", "textedit", "notes"]);
    assert_eq!(names(&index, "TE"), names(&index, "te"), "case folds");
}

#[test]
fn a_word_start_inside_a_name_counts_camel_case_included() {
    let index = index();
    assert_eq!(names(&index, "edit")[0], "textedit");
    assert_eq!(names(&index, "manager"), ["files"]);
}

#[test]
fn initials_and_characters_in_order_find_a_program() {
    let index = index();
    assert_eq!(names(&index, "fm"), ["files"]);
    assert_eq!(names(&index, "tedt"), ["textedit"]);
}

#[test]
fn the_purpose_and_the_folder_find_what_the_name_does_not() {
    let index = index();
    assert_eq!(names(&index, "images"), ["paint", "view"]);
    assert_eq!(names(&index, "games"), ["cafe", "chess"]);
    // A purpose match never outranks a name match.
    assert_eq!(names(&index, "edit"), ["textedit", "paint"]);
}

#[test]
fn every_term_must_match_somewhere() {
    let index = index();
    assert_eq!(names(&index, "text edit"), ["textedit"]);
    assert!(names(&index, "text paint").is_empty());
    assert!(names(&index, "   ").is_empty(), "no terms, nothing found");
}

#[test]
fn accents_fold_and_emphasis_covers_the_whole_character() {
    let index = index();
    let hits = search(&index, "cafe", &BTreeMap::new());
    assert_eq!(index[hits[0].index].id().as_str(), "cafe");
    // "Café": the é is two bytes, and the emphasis spans both.
    assert_eq!(hits[0].emphasis, emphasis_runs(&[(0, 5)]));
}

#[test]
fn emphasis_marks_what_each_term_matched_in_the_name() {
    let index = index();
    let first = |query: &str| search(&index, query, &BTreeMap::new()).remove(0).emphasis;
    assert_eq!(first("text"), emphasis_runs(&[(0, 4)]));
    assert_eq!(first("tedt"), emphasis_runs(&[(0, 2), (5, 6), (7, 8)]));
    assert_eq!(first("file man"), emphasis_runs(&[(0, 4), (5, 8)]));
    assert!(first("images").is_empty(), "a purpose match marks no name");
}

#[test]
fn the_more_used_program_wins_a_tie() {
    let index = index();
    let mut weights = BTreeMap::new();
    weights.insert(EntryId::new("textedit").expect("id"), 5_000);
    assert_eq!(
        names_weighted(&index, "te", &weights),
        ["textedit", "terminal", "notes"]
    );
}

#[test]
fn a_query_is_read_within_its_bounds() {
    let many = "t ".repeat(100);
    assert_eq!(terms(&many).len(), MAX_TERMS);
    let long: String = core::iter::repeat_n('x', 1_000).collect();
    assert_eq!(terms(&long)[0].len(), MAX_TERM_CHARS);
    assert!(names(&index(), &long).is_empty());
}
