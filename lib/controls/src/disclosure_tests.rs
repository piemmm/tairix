//! Unit tests for the disclosure set — sections open and close independently,
//! from either starting posture — and for the tree keys' one step rule.

use crate::disclosure::{tree_step, DisclosureSet, TreeKey, TreeRow, TreeStep};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Section {
    General,
    Networking,
    Sharing,
}

const SECTIONS: [Section; 3] = [Section::General, Section::Networking, Section::Sharing];

#[test]
fn every_section_starts_in_the_sets_posture() {
    let closed = DisclosureSet::<Section>::closed();
    let open = DisclosureSet::<Section>::open();
    for section in SECTIONS {
        assert!(!closed.is_open(&section), "{section:?}");
        assert!(open.is_open(&section), "{section:?}");
    }
}

/// The rule the whole desktop keeps: opening a second section leaves the
/// first one open.
#[test]
fn opening_a_second_section_leaves_the_first_open() {
    let mut set = DisclosureSet::closed();
    assert!(set.set(Section::General, true));
    assert!(set.set(Section::Networking, true));
    assert!(set.is_open(&Section::General), "the first section closed");
    assert!(set.is_open(&Section::Networking));
    assert!(
        !set.is_open(&Section::Sharing),
        "an untouched section moved"
    );
}

#[test]
fn closing_one_section_leaves_the_others_as_they_were() {
    let mut set = DisclosureSet::open();
    assert!(set.set(Section::Networking, false));
    assert!(set.is_open(&Section::General));
    assert!(!set.is_open(&Section::Networking));
    assert!(set.is_open(&Section::Sharing));
}

#[test]
fn setting_a_section_to_its_own_posture_moves_nothing() {
    for mut set in [DisclosureSet::closed(), DisclosureSet::open()] {
        let before = set.is_open(&Section::General);
        assert!(
            !set.set(Section::General, before),
            "a no-op reported a move"
        );
        assert!(set.set(Section::General, !before));
        assert!(
            !set.set(Section::General, !before),
            "a repeat reported a move"
        );
        assert!(
            set.set(Section::General, before),
            "the way back moved nothing"
        );
        assert_eq!(set.is_open(&Section::General), before);
    }
}

#[test]
fn toggling_answers_the_posture_it_left_the_section_in() {
    for mut set in [DisclosureSet::closed(), DisclosureSet::open()] {
        let before = set.is_open(&Section::Sharing);
        assert_eq!(set.toggle(Section::Sharing), !before);
        assert_eq!(set.is_open(&Section::Sharing), !before);
        assert_eq!(set.toggle(Section::Sharing), before);
        assert_eq!(set.is_open(&Section::Sharing), before);
        assert_eq!(
            set,
            if before {
                DisclosureSet::open()
            } else {
                DisclosureSet::closed()
            },
            "two toggles leave nothing recorded"
        );
    }
}

#[test]
fn reset_puts_every_section_back_where_it_started() {
    let mut set = DisclosureSet::closed();
    set.set(Section::General, true);
    set.toggle(Section::Sharing);
    set.reset();
    assert_eq!(set, DisclosureSet::closed());
    for section in SECTIONS {
        assert!(!set.is_open(&section), "{section:?}");
    }
}

const fn row(disclosure: Option<bool>, nested: bool) -> TreeRow {
    TreeRow { disclosure, nested }
}

/// A closed section, an open one with two pages, an open one with none, and
/// a plain row.
const TREE: [TreeRow; 6] = [
    row(Some(false), false),
    row(Some(true), false),
    row(None, true),
    row(None, true),
    row(Some(true), false),
    row(None, false),
];

fn step(rows: &[TreeRow], current: usize, key: TreeKey) -> Option<TreeStep> {
    tree_step(rows, current, key, |row| *row)
}

#[test]
fn inward_on_a_closed_section_and_outward_on_an_open_one_ask_for_a_disclosure() {
    assert_eq!(
        step(&TREE, 0, TreeKey::Inward),
        Some(TreeStep::Disclose {
            index: 0,
            open: true
        })
    );
    assert_eq!(
        step(&TREE, 1, TreeKey::Outward),
        Some(TreeStep::Disclose {
            index: 1,
            open: false
        })
    );
}

#[test]
fn inward_on_an_open_section_steps_onto_its_first_page() {
    assert_eq!(step(&TREE, 1, TreeKey::Inward), Some(TreeStep::Move(2)));
}

#[test]
fn inward_on_an_open_section_with_no_pages_asks_nothing() {
    assert_eq!(step(&TREE, 4, TreeKey::Inward), None, "a plain row follows");
    let last = [row(Some(true), false)];
    assert_eq!(step(&last, 0, TreeKey::Inward), None, "nothing follows");
}

#[test]
fn outward_on_a_page_climbs_to_the_section_that_disclosed_it() {
    assert_eq!(step(&TREE, 2, TreeKey::Outward), Some(TreeStep::Move(1)));
    assert_eq!(step(&TREE, 3, TreeKey::Outward), Some(TreeStep::Move(1)));
}

/// A list whose rows are all pages — a flat list of search matches read as
/// nested — has no section to climb to.
#[test]
fn outward_on_a_page_with_no_section_above_asks_nothing() {
    let flat = [row(None, true), row(None, true)];
    assert_eq!(step(&flat, 1, TreeKey::Outward), None);
}

#[test]
fn the_keys_ask_nothing_where_they_mean_nothing() {
    for (current, key) in [
        (0, TreeKey::Outward),
        (2, TreeKey::Inward),
        (5, TreeKey::Inward),
        (5, TreeKey::Outward),
        (TREE.len(), TreeKey::Inward),
        (TREE.len(), TreeKey::Outward),
    ] {
        assert_eq!(step(&TREE, current, key), None, "{key:?} on {current}");
    }
    assert_eq!(step(&[], 0, TreeKey::Inward), None);
}
