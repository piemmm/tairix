//! Unit tests for the disclosure set: sections open and close independently,
//! from either starting posture.

use crate::disclosure::DisclosureSet;

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
