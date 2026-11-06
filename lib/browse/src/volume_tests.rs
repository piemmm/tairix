use alloc::string::{String, ToString};
use alloc::vec;

use tairix_abi::blkio::{BlkMedium, BlkTransport};
use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
use tairix_abi::fs::FileKind;
use tairix_abi::sysinfo::{
    MountAvailability, MountRecord, MountVolumeState, VolumeClass, VOLUME_ID_UNPUBLISHED,
};
use tairix_icon::{disk_icon, IconKind};

use super::{
    catalog_shortcuts, drive_entry, locate, reference, shortcuts, shown_on_desktop, DesktopDrives,
    DriveClass, Located,
};
use crate::entry::{resolve_target, DriveMark, Entry, EntryKind, LinkTarget, Listed};
use crate::mount::volume_root;
use crate::places::{Volume, VolumeFacts};
use crate::vfs::LinkInfo;

/// The identity the stick's content carries.
const STICK: [u8; 16] = [7; 16];

/// An attached volume at `target` of `class`, published under `key` with the
/// content identity `identity`, shown to its reader.
fn volume(target: &str, class: VolumeClass, (key, identity): ([u8; 16], [u8; 16])) -> Volume {
    let label = target.rsplit('/').find(|part| !part.is_empty());
    Volume {
        label: String::from(label.unwrap_or(target)),
        target: String::from(target),
        medium: None,
        facts: VolumeFacts {
            filesystem: String::from("fat32"),
            title: String::new(),
            class,
            free: None,
            availability: MountAvailability::Available,
            owner: None,
            volume_id: key,
            identity,
            read_only: false,
            titled: false,
            title_writes: false,
        },
        offered: true,
    }
}

/// The stick, attached first, at `/Storage/USB`.
fn stick() -> Volume {
    volume("/Storage/USB", VolumeClass::Removable, (STICK, STICK))
}

#[test]
fn a_shortcut_names_a_volume_by_its_root_and_nothing_else() {
    assert_eq!(reference(&volume_root(STICK)), Some(STICK));
    assert_eq!(reference(&alloc::format!("{}x", volume_root(STICK))), None);
    assert_eq!(reference("Home:/"), None);
    assert_eq!(reference("/Storage/USB"), None);
    assert_eq!(reference(&volume_root(VOLUME_ID_UNPUBLISHED)), None);
}

/// A link storing a volume's root is a shortcut to that volume whatever a walk
/// made of it, drawn as the generic drive until the mount table places it.
#[test]
fn a_link_to_a_volume_s_root_is_a_volume_shortcut() {
    for kind in [None, Some(FileKind::Directory)] {
        let link = LinkInfo {
            target: volume_root(STICK),
            kind,
        };
        assert_eq!(
            EntryKind::for_listing(FileKind::Symlink, "USB", Some(&link)),
            EntryKind::Link(LinkTarget::Volume)
        );
    }
    let shortcut = Entry::new(
        "USB",
        EntryKind::Link(LinkTarget::Volume),
        0,
        tairix_abi::time::Time64::UNIX_EPOCH,
    );
    assert_eq!(shortcut.drive(), Some(IconKind::Disk));
    assert_eq!(shortcut.kind().resolved(), None);
    assert!(!shortcut.is_directory());
}

#[test]
fn locate_answers_present_absent_or_withheld() {
    let target = volume_root(STICK);
    let volumes = [stick()];
    assert_eq!(locate(&target, &volumes), Located::Present(&volumes[0]));
    assert_eq!(locate(&target, &[]), Located::Absent);
    assert_eq!(locate("/Storage/USB", &volumes), Located::Absent);

    // A twin carries the identity under a key of its own: alone it is the
    // medium, beside the attachment published under the identity it is not.
    let twin = volume("/Storage/USB_2", VolumeClass::Removable, ([9; 16], STICK));
    let twins = [twin.clone(), stick()];
    assert_eq!(locate(&target, &twins), Located::Present(&twins[1]));
    let alone = [twin];
    assert_eq!(locate(&target, &alone), Located::Present(&alone[0]));

    let mut private = stick();
    private.offered = false;
    assert_eq!(locate(&target, &[private.clone()]), Located::Withheld);
    private.facts.availability = MountAvailability::UnavailableLost;
    assert_eq!(
        locate(&target, &[private]),
        Located::Absent,
        "its medium went"
    );
}

#[test]
fn an_absent_or_withheld_volume_is_refused_in_its_own_words() {
    let present = [stick()];
    assert_eq!(Located::Present(&present[0]).present(), Some(&present[0]));
    assert_eq!(Located::Present(&present[0]).refusal("USB"), None);
    assert_eq!(Located::Absent.present(), None);
    let (headline, reason) = Located::Absent.refusal("USB").expect("refused");
    assert_eq!(headline, "\u{201c}USB\u{201d} is not present");
    assert_eq!(reason, "The drive it stands for is not attached.");
    let (headline, _) = Located::Withheld.refusal("USB").expect("refused");
    assert_eq!(headline, "\u{201c}USB\u{201d} is not available");
}

/// A drop of volumes makes a shortcut to each by the identity its content
/// carries — a twin's own key is never recorded — and refuses one that is no
/// attached volume's own root.
#[test]
fn a_volume_drop_makes_a_shortcut_to_each_by_its_identity() {
    let marked = |name: &str, facts: VolumeFacts| {
        let mut entry = Entry::directory(name);
        entry.set_drive(Some(&DriveMark {
            icon: IconKind::Disk,
            facts,
        }));
        entry
    };
    let twin = volume("/Storage/USB_2", VolumeClass::Removable, ([9; 16], STICK));
    let dropped = [marked("USB", stick().facts), marked("USB_2", twin.facts)];
    assert_eq!(
        shortcuts(&dropped),
        Some(vec![
            (String::from("USB"), volume_root(STICK)),
            (String::from("USB_2"), volume_root(STICK)),
        ])
    );
    let bound = [marked("USB", stick().facts), Entry::directory("Bound")];
    assert_eq!(shortcuts(&bound), None, "no volume's own root");
}

/// Volumes dropped by their places in the storage catalog make the shortcuts
/// the catalog's own entries make; anything else, or a volume no longer
/// attached there, makes none.
#[test]
fn volumes_dropped_by_their_catalog_places_make_the_same_shortcuts() {
    let at = |path: &[&str]| Listed {
        path: path.iter().map(|part| String::from(*part)).collect(),
        id: tairix_abi::fs::FileId::NONE,
    };
    let twin = volume("/Storage/USB_2", VolumeClass::Removable, ([9; 16], STICK));
    let volumes = [stick(), twin];
    assert_eq!(
        catalog_shortcuts(
            &[at(&["Storage", "USB"]), at(&["Storage", "USB_2"])],
            &volumes
        ),
        Some(vec![
            (String::from("USB"), volume_root(STICK)),
            (String::from("USB_2"), volume_root(STICK)),
        ])
    );
    assert_eq!(
        catalog_shortcuts(&[at(&["Storage", "USB"]), at(&["Users", "ada"])], &volumes),
        None,
        "a file is no volume"
    );
    assert_eq!(
        catalog_shortcuts(&[at(&["Storage", "Gone"])], &volumes),
        None,
        "nor is a place nothing is attached at"
    );
    let unpublished = volume(
        "/Storage/Odd",
        VolumeClass::Removable,
        ([5; 16], VOLUME_ID_UNPUBLISHED),
    );
    assert_eq!(
        catalog_shortcuts(&[at(&["Storage", "Odd"])], &[unpublished]),
        None,
        "nor one whose content carries no identity"
    );
}

#[test]
fn each_drive_class_is_shown_or_not_on_its_own() {
    for class in DriveClass::ALL {
        let drives = DesktopDrives::NONE.with(class, true);
        for other in DriveClass::ALL {
            assert_eq!(drives.shows(other), other == class, "{class:?}, {other:?}");
        }
        assert_eq!(drives.with(class, false), DesktopDrives::NONE);
    }
}

#[test]
fn the_desktop_shows_removable_drives_alone_by_default_and_the_root_as_the_system_drive() {
    let defaults = DesktopDrives::default();
    for class in DriveClass::ALL {
        assert_eq!(
            defaults.shows(class),
            class == DriveClass::Removable,
            "{class:?}"
        );
    }
    let root = volume("/", VolumeClass::Local, ([5; 16], [5; 16]));
    assert_eq!(DriveClass::of(&root), Some(DriveClass::System));
    for (class, drive) in [
        (VolumeClass::Local, DriveClass::Fixed),
        (VolumeClass::Removable, DriveClass::Removable),
        (VolumeClass::Network, DriveClass::Network),
        (VolumeClass::Image, DriveClass::Images),
    ] {
        let attached = volume("/Storage/X", class, (STICK, STICK));
        assert_eq!(DriveClass::of(&attached), Some(drive));
    }
    let system = volume("/System", VolumeClass::Local, ([3; 16], [3; 16]));
    assert_eq!(DriveClass::of(&system), None, "the system tree is no drive");
    assert!(shown_on_desktop(&stick(), defaults));
    assert!(!shown_on_desktop(&root, defaults));
    let mut everything = defaults;
    for class in DriveClass::ALL {
        *everything.setting_mut(class) = true;
    }
    assert!(shown_on_desktop(&root, everything));
    let mut withheld = stick();
    withheld.offered = false;
    assert!(
        !shown_on_desktop(&withheld, everything),
        "shown while present"
    );
}

#[test]
fn a_drive_is_labelled_by_its_title_else_its_catalog_name_and_named_by_its_root() {
    let mut named = stick();
    named.medium = Some(BlkMedium::of_transport(BlkTransport::Nvme));
    let drive = drive_entry(&named).expect("a published identity");
    assert_eq!(drive.name(), volume_root(STICK));
    assert_eq!(drive.target(), Some(volume_root(STICK).as_str()));
    assert_eq!(drive.label(), Some("USB"));
    assert_eq!(drive.kind(), EntryKind::Link(LinkTarget::Volume));
    assert_eq!(drive.drive(), Some(disk_icon(named.medium)));
    assert_eq!(drive.volume().map(|facts| facts.volume_id), Some(STICK));
    named.facts.title = String::from("Holiday");
    let titled = drive_entry(&named).expect("a published identity");
    assert_eq!(titled.label(), Some("Holiday"));
    let unpublished = volume("/Storage/RAM", VolumeClass::Local, ([0; 16], [0; 16]));
    assert_eq!(drive_entry(&unpublished), None);
}

/// The view's root is a volume of its own, labelled by itself, carrying the
/// identity its content does beside the key it is published under.
#[test]
fn the_root_is_a_volume_and_every_volume_carries_its_identity() {
    let state = MountVolumeState {
        usage: VolumeStats::default(),
        availability: MountAvailability::Available,
        medium: None,
    };
    let record = MountRecord::new(
        b"disk0",
        b"/",
        b"arxfs",
        MountFlags::default(),
        state,
        [5; 16],
    )
    .expect("a record")
    .with_identity(STICK);
    let root = Volume::from_mount(&record).expect("the root is a volume");
    assert_eq!(root.label, "/");
    assert_eq!(root.facts.volume_id, [5; 16]);
    assert_eq!(root.facts.identity, STICK);
    assert_eq!(DriveClass::of(&root), Some(DriveClass::System));
}

/// A link rooted elsewhere is reached at its target, in the view where that
/// root is a machine alias; any other link is reached through itself.
#[test]
fn a_link_rooted_elsewhere_is_reached_at_its_target() {
    assert_eq!(
        resolve_target("/Users/ada/Desktop", "System:/Applications/chess.app"),
        "/System/Applications/chess.app"
    );
    assert_eq!(resolve_target("/Users/ada/Desktop", "Home:/x"), "Home:/x");
    let rooted = Entry::new(
        "Edit",
        EntryKind::Link(LinkTarget::Bundle),
        0,
        tairix_abi::time::Time64::UNIX_EPOCH,
    );
    let link = "/Users/ada/Desktop/Edit".to_string();
    assert_eq!(
        rooted
            .clone()
            .with_target("System:/Applications/Edit.app")
            .reached_by("/Users/ada/Desktop"),
        "/System/Applications/Edit.app"
    );
    assert_eq!(
        rooted
            .with_target("/Users/ada/Documents")
            .reached_by("/Users/ada/Desktop"),
        link,
        "the kernel follows it"
    );
}
