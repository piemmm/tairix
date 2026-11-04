use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::sysinfo::{MountAvailability, RemovedWrites, VolumeRemovalRecord};
use tairix_browse::{Places, Volume, VolumeFacts};

use super::{
    departed, disconnected, duplicates, holding, named, recovering, unreadable, Removals, Standing,
};

fn volume(label: &str, id: u8, availability: MountAvailability) -> Volume {
    Volume {
        label: String::from(label),
        target: alloc::format!("/Storage/{label}"),
        medium: None,
        facts: VolumeFacts {
            filesystem: String::from("fat32"),
            title: String::new(),
            class: tairix_abi::sysinfo::VolumeClass::Local,
            free: None,
            availability,
            owner: None,
            volume_id: [id; 16],
            read_only: false,
        },
        offered: matches!(
            availability,
            MountAvailability::Available
                | MountAvailability::Degraded
                | MountAvailability::Recovering
        ),
    }
}

fn removal(sequence: u64, id: u8, writes: RemovedWrites) -> VolumeRemovalRecord {
    VolumeRemovalRecord::new(sequence, [id; 16], writes).expect("a removal")
}

fn components(path: &[&str]) -> Vec<String> {
    path.iter().map(|part| String::from(*part)).collect()
}

/// What happened before the file manager looked is never stated; after that,
/// each removal is stated once, and a read that failed changes nothing.
#[test]
fn each_removal_is_taken_in_once_and_none_from_before_the_first_read() {
    let mut removals = Removals::default();
    let before = [
        removal(1, 1, RemovedWrites::None),
        removal(2, 2, RemovedWrites::None),
    ];
    assert!(
        removals.take_in(Some(&before)).is_empty(),
        "before it looked"
    );
    assert!(
        removals.take_in(None).is_empty(),
        "an unread record says nothing"
    );
    let later = [
        removal(2, 2, RemovedWrites::None),
        removal(3, 3, RemovedWrites::Held),
        removal(4, 4, RemovedWrites::None),
    ];
    let new: Vec<u64> = removals
        .take_in(Some(&later))
        .iter()
        .map(VolumeRemovalRecord::sequence)
        .collect();
    assert_eq!(new, [3, 4]);
    assert!(removals.take_in(Some(&later)).is_empty(), "stated once");
}

/// A first read that lists nothing still marks where the file manager began.
#[test]
fn an_empty_first_read_still_marks_where_the_count_began() {
    let mut removals = Removals::default();
    assert!(removals.take_in(Some(&[])).is_empty());
    let first = [removal(1, 1, RemovedWrites::Lost)];
    assert_eq!(removals.take_in(Some(&first)).len(), 1);
}

/// A removed volume is named as the rail showed it, or as the mount table
/// still lists one that kept its writes; one shown nowhere goes unnamed.
#[test]
fn a_removal_is_named_as_the_user_saw_it() {
    let before = Places::new(&[], &[volume("USB1", 1, MountAvailability::Available)]);
    let after = [volume("Backup", 2, MountAvailability::UnavailableDirty)];
    let removed = [
        removal(1, 1, RemovedWrites::None),
        removal(2, 2, RemovedWrites::Held),
        removal(3, 3, RemovedWrites::None),
    ];
    assert_eq!(
        named(&removed, &before, &after),
        [
            ("USB1", RemovedWrites::None),
            ("Backup", RemovedWrites::Held)
        ]
    );
}

/// One alert names every volume disconnected together and says the worst of
/// what became of their writes; a loss is an error, the rest warnings.
#[test]
fn one_alert_names_every_volume_and_the_worst_of_their_writes() {
    assert_eq!(disconnected(&[]), None);
    let one = disconnected(&[("USB1", RemovedWrites::None)]).expect("an alert");
    assert_eq!(
        one.headline,
        "\u{201c}USB1\u{201d} was disconnected without being ejected"
    );
    assert_eq!(one.reason, "nothing that was being written to it was lost");
    assert!(!one.lost);
    let three = disconnected(&[
        ("A", RemovedWrites::None),
        ("B", RemovedWrites::Lost),
        ("C", RemovedWrites::Held),
    ])
    .expect("an alert");
    assert_eq!(
        three.headline,
        "\u{201c}A\u{201d}, \u{201c}B\u{201d} and \u{201c}C\u{201d} were disconnected without being \
         ejected"
    );
    assert_eq!(
        three.reason,
        "some of what was being written to them was lost"
    );
    assert!(three.lost);
    let held =
        disconnected(&[("A", RemovedWrites::None), ("B", RemovedWrites::Held)]).expect("an alert");
    assert_eq!(
        held.reason,
        "what was still being written to them is kept until they are connected again"
    );
}

/// What hides in a volume's name cannot make the alert read as another's.
#[test]
fn a_volume_is_named_as_shown() {
    let alert = disconnected(&[("a\u{202e}b", RemovedWrites::None)]).expect("an alert");
    assert!(alert.headline.starts_with("\u{201c}a\u{fffd}b\u{201d}"));
}

/// A volume gone from the table, or there but no longer offered, is left;
/// one still offered is not, and nor is a folder of the user's.
#[test]
fn the_places_left_are_those_of_volumes_no_longer_offered() {
    let home = components(&["Users", "ann"]);
    let before = Places::new(
        &home,
        &[
            volume("USB1", 1, MountAvailability::Available),
            volume("USB2", 2, MountAvailability::Available),
            volume("Stays", 3, MountAvailability::Available),
        ],
    );
    let after = [
        volume("USB2", 2, MountAvailability::UnavailableDirty),
        volume("Stays", 3, MountAvailability::Degraded),
    ];
    let mut left = departed(&before, &after);
    left.sort();
    assert_eq!(
        left,
        vec![
            components(&["Storage", "USB1"]),
            components(&["Storage", "USB2"])
        ]
    );
}

fn standing(at: Option<&[String]>, busy: bool, focused: bool, since: u64) -> Standing<'_> {
    Standing {
        at,
        busy,
        focused,
        chosen: false,
        since,
    }
}

fn chosen(at: Option<&[String]>, since: u64) -> Standing<'_> {
    Standing {
        chosen: true,
        ..standing(at, false, false, since)
    }
}

/// Of windows that climbed to one place the one there longest stays, unless
/// another holds the keyboard; windows elsewhere are left alone.
#[test]
fn one_window_is_left_at_each_place() {
    let storage = components(&["Storage"]);
    let home = components(&["Users", "ann"]);
    let windows = [
        standing(Some(&storage), false, false, 5),
        standing(Some(&storage), false, false, 2),
        standing(Some(&home), false, false, 1),
        standing(Some(&storage), false, false, 9),
    ];
    assert_eq!(
        duplicates(&windows).closing,
        [0, 3],
        "the one there since 2 stays"
    );
    let focused = [
        standing(Some(&storage), false, false, 2),
        standing(Some(&storage), false, true, 7),
    ];
    assert_eq!(duplicates(&focused).closing, [0], "the one in use stays");
    let network = [
        standing(None, false, false, 1),
        standing(None, false, false, 3),
    ];
    assert_eq!(
        duplicates(&network).closing,
        [1],
        "the network is one place too"
    );
}

/// A window the user came to a place in outlasts those that only climbed
/// there, and of two the user came in, the newer — in front — stays.
#[test]
fn a_place_the_user_came_to_keeps_the_window_they_came_in() {
    let storage = components(&["Storage"]);
    let climbed_first = [
        standing(Some(&storage), false, false, 1),
        chosen(Some(&storage), 5),
        standing(Some(&storage), false, false, 3),
    ];
    assert_eq!(duplicates(&climbed_first).closing, [0, 2]);
    let both_chosen = [chosen(Some(&storage), 1), chosen(Some(&storage), 8)];
    assert_eq!(
        duplicates(&both_chosen).closing,
        [0],
        "the newer is in front"
    );
}

/// A busy window is never closed: it stays beside the one kept, and the set
/// converges once it is idle.
#[test]
fn a_busy_window_is_never_closed() {
    let storage = components(&["Storage"]);
    let windows = [
        standing(Some(&storage), false, false, 1),
        standing(Some(&storage), true, false, 4),
        standing(Some(&storage), false, false, 6),
    ];
    assert_eq!(duplicates(&windows).closing, [2]);
    let both_busy = [
        standing(Some(&storage), true, false, 1),
        standing(Some(&storage), true, false, 2),
    ];
    let waiting = duplicates(&both_busy);
    assert!(waiting.closing.is_empty() && waiting.waiting);
    assert!(
        duplicates(&windows).waiting,
        "the busy one is weighed again"
    );
    let alone = [standing(Some(&storage), true, false, 1)];
    assert!(
        !duplicates(&alone).waiting,
        "a window alone waits on nothing"
    );
}

/// Folders still attached whose devices failed are stated once per alert,
/// every volume named.
#[test]
fn unreadable_volumes_are_named_together() {
    assert_eq!(unreadable(&[]), None);
    assert_eq!(
        unreadable(&["USB1"]),
        Some((
            String::from("\u{201c}USB1\u{201d} could not be read"),
            "its device failed"
        ))
    );
    assert_eq!(
        unreadable(&["A", "B"]),
        Some((
            String::from("\u{201c}A\u{201d} and \u{201c}B\u{201d} could not be read"),
            "their devices failed"
        ))
    );
}

/// A folder is held by the innermost volume shown above it, and a folder of
/// the user's by none.
#[test]
fn a_folder_is_held_by_the_volume_above_it() {
    let places = Places::new(
        &components(&["Users", "ann"]),
        &[volume("USB1", 1, MountAvailability::Available)],
    );
    let held = holding(&places, &components(&["Storage", "USB1", "Games"]));
    assert_eq!(held.map(tairix_browse::Place::label), Some("USB1"));
    assert!(!recovering(held.expect("held")));
    assert!(holding(&places, &components(&["Storage", "USB10"])).is_none());
    assert!(holding(&places, &components(&["Users", "ann", "Home"])).is_none());
    let coming_back = Places::new(&[], &[volume("USB1", 1, MountAvailability::Recovering)]);
    let row = holding(&coming_back, &components(&["Storage", "USB1"]));
    assert!(
        row.is_some_and(recovering),
        "a volume ridden out is still offered"
    );
}
