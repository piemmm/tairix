use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::time::Time64;
use tairix_icon::IconKind;

use super::{entry_cmp, sort_entries, SortDirection, SortKey, SortMode};
use crate::entry::{DriveMark, Entry, EntryKind};
use crate::places::VolumeFacts;

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
            free,
            availability,
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
    }
}

#[test]
fn volumes_order_by_the_space_they_have_free_one_keeping_none_first() {
    let mut entries = volumes();
    sort_entries(&mut entries, ascending(SortKey::Free));
    assert_eq!(names(&entries), ["Camera", "Media", "Backup", "Archive"]);
}

#[test]
fn volumes_order_by_their_filesystem_then_by_name() {
    let mut entries = volumes();
    sort_entries(&mut entries, ascending(SortKey::Filesystem));
    assert_eq!(names(&entries), ["Archive", "Backup", "Camera", "Media"]);
}

#[test]
fn volumes_order_healthiest_first_and_reversed_on_a_second_press() {
    let mut entries = volumes();
    sort_entries(&mut entries, ascending(SortKey::Health));
    assert_eq!(names(&entries), ["Archive", "Backup", "Camera", "Media"]);
    let reversed = ascending(SortKey::Health).pressed(SortKey::Health);
    assert_eq!(reversed.direction, SortDirection::Descending);
    sort_entries(&mut entries, reversed);
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
    };
    expected.sort_by(|a, b| entry_cmp(a, b, mode));
    sort_entries(&mut entries, mode);
    assert_eq!(names(&entries), names(&expected));
}
