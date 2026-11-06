use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::time::Time64;
use tairix_fsmeta::preset::acorn::Filetype;
use tairix_icon::IconKind;

use super::{entry_cmp, sort_entries, Folders, Order, SortDirection, SortKey, SortMode};
use crate::entry::{DriveMark, Entry, EntryKind};
use crate::places::VolumeFacts;
use crate::presentation::HiddenEndings;

/// The root of a volume called `name` carrying `filesystem`, with `free`
/// bytes to spare, serving as `availability` says.
fn volume_root(
    name: &str,
    filesystem: &str,
    free: Option<u64>,
    availability: MountAvailability,
) -> Entry {
    let mut root = Entry::directory(name);
    root.set_drive(Some(&DriveMark {
        icon: IconKind::Disk,
        facts: VolumeFacts {
            filesystem: String::from(filesystem),
            title: String::new(),
            class: tairix_abi::sysinfo::VolumeClass::Local,
            free,
            availability,
            owner: None,
            volume_id: [0; 16],
            identity: [0; 16],
            read_only: false,
            titled: false,
        },
    }));
    root
}

/// The volumes a storage catalog lists, out of every order the tests ask for.
fn volumes() -> Vec<Entry> {
    alloc::vec![
        volume_root("Media", "fat32", Some(5), MountAvailability::Degraded),
        volume_root("Archive", "arxfs", Some(900), MountAvailability::Available),
        volume_root("Camera", "ext4", None, MountAvailability::Recovering),
        volume_root("Backup", "arxfs", Some(40), MountAvailability::Available),
    ]
}

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(Entry::name).collect()
}

fn ascending(key: SortKey) -> SortMode {
    SortMode {
        key,
        direction: SortDirection::Ascending,
        ..SortMode::default_order()
    }
}

#[test]
fn folders_stand_first_unless_the_order_places_them_among_the_files() {
    let listed = || {
        alloc::vec![
            Entry::directory("b"),
            Entry::file("c"),
            Entry::directory("d"),
            Entry::file("a"),
        ]
    };
    let mut first = listed();
    sort_entries(&mut first, ascending(SortKey::Name).into());
    assert_eq!(names(&first), ["b", "d", "a", "c"]);
    let among = SortMode {
        folders: Folders::Among,
        ..ascending(SortKey::Name)
    };
    let mut mixed = listed();
    sort_entries(&mut mixed, among.into());
    assert_eq!(names(&mixed), ["a", "b", "c", "d"]);
    let reversed = SortMode {
        direction: SortDirection::Descending,
        ..among
    };
    sort_entries(&mut mixed, reversed.into());
    assert_eq!(names(&mixed), ["d", "c", "b", "a"]);
}

#[test]
fn a_header_press_and_the_sort_cycle_keep_where_folders_stand() {
    let among = SortMode {
        folders: Folders::Among,
        ..SortMode::default_order()
    };
    assert_eq!(among.pressed(SortKey::Size).folders, Folders::Among);
    assert_eq!(among.next().folders, Folders::Among);
    assert_eq!(SortMode::default_order().folders, Folders::First);
}

#[test]
fn volumes_order_by_the_space_they_have_free_one_keeping_none_first() {
    let mut entries = volumes();
    sort_entries(&mut entries, ascending(SortKey::Free).into());
    assert_eq!(names(&entries), ["Camera", "Media", "Backup", "Archive"]);
}

#[test]
fn volumes_order_by_their_filesystem_then_by_name() {
    let mut entries = volumes();
    sort_entries(&mut entries, ascending(SortKey::Filesystem).into());
    assert_eq!(names(&entries), ["Archive", "Backup", "Camera", "Media"]);
}

#[test]
fn volumes_order_healthiest_first_and_reversed_on_a_second_press() {
    let mut entries = volumes();
    sort_entries(&mut entries, ascending(SortKey::Health).into());
    assert_eq!(names(&entries), ["Archive", "Backup", "Camera", "Media"]);
    let reversed = ascending(SortKey::Health).pressed(SortKey::Health);
    assert_eq!(reversed.direction, SortDirection::Descending);
    sort_entries(&mut entries, reversed.into());
    // The name tiebreak does not turn round with the key.
    assert_eq!(names(&entries), ["Camera", "Media", "Archive", "Backup"]);
}

#[test]
fn a_header_press_orders_by_its_column_ascending_and_a_second_reverses_it() {
    let start = SortMode::default_order();
    let by_size = start.pressed(SortKey::Size);
    assert_eq!(by_size, ascending(SortKey::Size));
    assert_eq!(
        by_size.pressed(SortKey::Size).direction,
        SortDirection::Descending
    );
    assert_eq!(
        by_size.pressed(SortKey::Size).pressed(SortKey::Size),
        by_size,
        "a third press turns it back"
    );
    assert_eq!(
        by_size.pressed(SortKey::Size).pressed(SortKey::Name),
        start,
        "another column starts ascending"
    );
}

/// Kind classifies each entry once and then moves the entries into place; the
/// order that leaves must be exactly the one the comparison defines.
#[test]
fn a_kind_sort_lands_every_entry_where_its_comparison_puts_it() {
    let extensions = ["txt", "png", "zip", "rs", "md", "jpeg", "toml", "gif"];
    let mut entries: Vec<Entry> = (0..200_u64)
        .map(|n| {
            let ext = extensions[usize::try_from(n * 7 % 8).expect("small")];
            let kind = match n % 11 {
                0 => EntryKind::Directory,
                1 => EntryKind::Bundle,
                _ => EntryKind::File,
            };
            Entry::new(
                alloc::format!("e{:03}.{ext}", (n * 37) % 200),
                kind,
                n,
                Time64::from_secs(0),
            )
        })
        .collect();
    let mut expected = entries.clone();
    let mode = SortMode {
        key: SortKey::Kind,
        direction: SortDirection::Descending,
        ..SortMode::default_order()
    };
    expected.sort_by(|a, b| entry_cmp(a, b, mode.into()));
    sort_entries(&mut entries, mode.into());
    assert_eq!(names(&entries), names(&expected));
}

/// A file its name does not type sorts by the type its recorded filetype
/// names.
#[test]
fn a_recorded_filetype_sorts_a_file_by_the_type_it_names() {
    let typed = |name: &str, raw: u16| {
        Entry::file(name).with_filetype(Filetype::new(raw).expect("twelve bits"))
    };
    let mut entries = alloc::vec![typed("a", 0xFFF), typed("b", 0xFF9), Entry::file("c")];
    let mode = SortMode {
        key: SortKey::Kind,
        direction: SortDirection::Ascending,
        ..SortMode::default_order()
    };
    sort_entries(&mut entries, mode.into());
    assert_eq!(
        names(&entries),
        ["c", "b", "a"],
        "Data, then Sprite, then Text"
    );
}

/// Names order as they are shown, the name the volume holds breaking a tie,
/// whichever the direction; classifying each entry once orders exactly as the
/// comparison does.
#[test]
fn names_order_as_shown_with_the_held_name_breaking_a_tie() {
    let bundle = |name: &str| Entry::new(name, EntryKind::Bundle, 0, Time64::UNIX_EPOCH);
    let listed = || {
        alloc::vec![
            Entry::file("Paint-old.txt"),
            bundle("Paint.app"),
            Entry::file("Ab-2"),
            Entry::file("Paint"),
            bundle("Ab.app"),
        ]
    };
    let shown = |direction| Order {
        mode: SortMode {
            direction,
            ..ascending(SortKey::Name)
        },
        endings: HiddenEndings {
            applications: true,
            known_types: false,
        },
    };
    let mut entries = listed();
    sort_entries(&mut entries, ascending(SortKey::Name).into());
    assert_eq!(
        names(&entries),
        ["Ab-2", "Ab.app", "Paint", "Paint-old.txt", "Paint.app"],
        "as the volume holds them"
    );
    sort_entries(&mut entries, shown(SortDirection::Ascending));
    assert_eq!(
        names(&entries),
        ["Ab.app", "Ab-2", "Paint", "Paint.app", "Paint-old.txt"]
    );
    sort_entries(&mut entries, shown(SortDirection::Descending));
    assert_eq!(
        names(&entries),
        ["Paint-old.txt", "Paint", "Paint.app", "Ab-2", "Ab.app"],
        "the held name's tiebreak does not turn round"
    );
    let order = shown(SortDirection::Ascending);
    let mut compared = listed();
    compared.sort_by(|a, b| entry_cmp(a, b, order));
    sort_entries(&mut entries, order);
    assert_eq!(names(&compared), names(&entries));
}
