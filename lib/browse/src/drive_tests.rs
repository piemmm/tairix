//! Unit tests for the drive model: a reading becomes the section's facts,
//! capacity and activity; each change is reported as the part it moved; and a
//! rate is only ever the delta of one device across one measured interval.

use alloc::string::String;
use alloc::vec::Vec;

use alloc::boxed::Box;
use tairix_abi::blkio::{BlkDeviceName, BlkIoCounters, BlkMedium, BlkTransport};
use tairix_abi::driver::filesystem::{MountFlags, VolumeStats, VolumeTitle};
use tairix_abi::sysinfo::{
    MountAvailability, MountRecord, MountVolumeState, VolumeHealth, VolumeIoStatsRecord,
};
use tairix_geometry::{Rect, Region};
use tairix_input::{Key, Modifiers, NamedKey};

use super::{Drive, DriveChange, DrivePanels, DriveSample, TitleKeyed};
use crate::mount::{MountAct, Titling};
use crate::places::Retitler;

/// An `NVMe` namespace.
const NVME: BlkMedium = BlkMedium::of_transport(BlkTransport::Nvme);

const SECOND: u64 = 1_000_000_000;

/// A volume mounted at `/Storage/Backup`, `free` of its 1000 4 KiB blocks
/// unallocated.
fn record(free: u64, availability: MountAvailability) -> MountRecord {
    MountRecord::new(
        b"nvme0p2",
        b"/Storage/Backup",
        b"arxfs",
        MountFlags::NOEXEC,
        MountVolumeState {
            usage: VolumeStats {
                block_size: 4096,
                in_reserve: false,
                total_blocks: 1000,
                free_blocks: free,
                avail_blocks: free,
                files: 65_536,
                files_free: 1_234,
            },
            availability,
            medium: Some(NVME),
        },
        [0x5f; 16],
    )
    .expect("a valid record")
}

/// The device `dev`'s counters, having read and written `read` and `written`.
fn service(dev: u64, read: u64, written: u64) -> VolumeIoStatsRecord {
    VolumeIoStatsRecord::new(
        [0x5f; 16],
        dev,
        BlkIoCounters {
            read_bytes: read,
            write_bytes: written,
            ..BlkIoCounters::default()
        },
        BlkDeviceName::new("Samsung-970"),
    )
}

fn sample(free: u64, service: Option<VolumeIoStatsRecord>, at: u64) -> DriveSample {
    DriveSample {
        mount: Some(record(free, MountAvailability::Available)),
        service,
        at_ns: at,
    }
}

/// The fixed disk's volume at `/Storage/Backup` read at `at`, recording
/// `title` in a format that takes one.
fn titled_sample(title: &str, at: u64) -> DriveSample {
    DriveSample {
        mount: Some(
            record(10, MountAvailability::Available)
                .with_title(VolumeTitle::new(title).expect("a title"))
                .with_titled(true),
        ),
        service: None,
        at_ns: at,
    }
}

/// `key` fed to `drive`'s *Title* field.
fn keyed(drive: &mut Drive, key: Key) -> TitleKeyed {
    drive.title_key(
        (key, Modifiers::default()),
        Rect::new(0, 0, 200, 40),
        &mut Region::new(),
    )
}

/// A retitle moves what the section states, so the window lays it out again
/// and its *Title* field follows.
#[test]
fn a_retitle_moves_the_layout() {
    let mut drive = Drive::new().acting_for(Some(1000));
    assert_eq!(
        drive.adopt(&titled_sample("Backup", 0)),
        DriveChange::Layout
    );
    assert_eq!(drive.title().text(), "Backup");
    assert_eq!(
        drive.adopt(&titled_sample("Backup", SECOND)),
        DriveChange::Nothing
    );
    assert_eq!(
        drive.adopt(&titled_sample("Photos", 2 * SECOND)),
        DriveChange::Layout,
        "a title is a fact the section states"
    );
    assert_eq!(drive.title().text(), "Photos");
}

/// The *Title* field commits through the one check: what was committed shows
/// until a reading says what the volume records, `Escape` puts the title as
/// read back, and a refused commit keeps the keyboard.
#[test]
fn the_title_field_commits_through_the_one_check() {
    let mut drive = Drive::new().acting_for(Some(1000));
    let _ = drive.adopt(&titled_sample("Backup", 0));
    assert_eq!(
        keyed(&mut drive, Key::Char('x')),
        TitleKeyed::Ignored,
        "not until it holds the keyboard"
    );
    assert!(drive.begin_title());
    assert!(!drive.begin_title(), "it holds it already");
    for ch in "s2".chars() {
        assert_eq!(keyed(&mut drive, Key::Char(ch)), TitleKeyed::Typed);
    }
    assert_eq!(
        keyed(&mut drive, Key::Named(NamedKey::Enter)),
        TitleKeyed::Retitle(MountAct::Retitle {
            volume_id: [0x5f; 16],
            name: alloc::string::String::from("Backup"),
            titling: Box::new(Titling {
                title: VolumeTitle::new("Backups2").expect("a title"),
                retitler: Retitler::Administrator,
            }),
        })
    );
    assert!(!drive.is_titling(), "a commit hands the keyboard back");
    assert_eq!(drive.title().text(), "Backups2", "until a reading says");
    assert_eq!(
        drive.adopt(&titled_sample("Backup", SECOND)),
        DriveChange::Layout,
        "a reading after a refused retitle puts the title back"
    );
    assert_eq!(drive.title().text(), "Backup");

    assert!(drive.begin_title());
    let _ = keyed(&mut drive, Key::Char('!'));
    assert_eq!(
        keyed(&mut drive, Key::Named(NamedKey::Escape)),
        TitleKeyed::Ended
    );
    assert_eq!(
        (drive.is_titling(), drive.title().text()),
        (false, "Backup")
    );
    assert!(drive.begin_title());
    assert_eq!(
        keyed(&mut drive, Key::Named(NamedKey::Enter)),
        TitleKeyed::Ended,
        "an unchanged title asks nothing"
    );

    assert!(drive.begin_title());
    for _ in 0..127 {
        let _ = keyed(&mut drive, Key::Char('\u{e9}'));
    }
    assert_eq!(
        keyed(&mut drive, Key::Named(NamedKey::Enter)),
        TitleKeyed::Refused,
        "longer than any disk records"
    );
    assert!(drive.is_titling(), "a refused commit keeps the keyboard");
    assert_eq!(
        keyed(&mut drive, Key::Named(NamedKey::Tab)),
        TitleKeyed::Ended
    );
}

/// A title its viewer may not change takes no keyboard: one read for no one,
/// and one whose format keeps no title.
#[test]
fn a_title_its_viewer_may_not_change_takes_no_keyboard() {
    let mut anonymous = Drive::new();
    let _ = anonymous.adopt(&titled_sample("Backup", 0));
    assert!(anonymous.title().is_read_only());
    assert!(!anonymous.begin_title());
    let mut untitled = Drive::new().acting_for(Some(1000));
    let _ = untitled.adopt(&sample(10, None, 0));
    assert!(untitled.title().is_read_only());
    assert!(!untitled.begin_title());
}

#[test]
fn an_unread_drive_states_nothing_and_a_gone_one_nothing_but_that() {
    let mut drive = Drive::new();
    assert!(!drive.is_read());
    assert!(drive.facts().is_empty());
    assert_eq!(drive.capacity(), None);

    let change = drive.adopt(&DriveSample {
        mount: None,
        service: None,
        at_ns: 0,
    });
    assert_eq!(
        change,
        DriveChange::Layout,
        "reading to gone moves the layout"
    );
    assert!(drive.is_read());
    assert!(drive.record().is_none());
    assert!(drive.facts().is_empty());
}

#[test]
fn a_mounted_volume_states_its_facts_in_the_shared_vocabulary() {
    let mut drive = Drive::new();
    drive.adopt(&sample(400, Some(service(9, 0, 0)), 0));
    let facts = drive.facts();
    let value = |label: &str| {
        facts
            .iter()
            .find(|(fact, _)| *fact == label)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(value("Health"), Some("Healthy"));
    assert_eq!(value("Medium"), Some("NVMe drive"));
    assert_eq!(value("Filesystem"), Some("arxfs"));
    assert_eq!(value("Mounted at"), Some("/Storage/Backup"));
    assert_eq!(value("Source"), Some("nvme0p2"));
    assert_eq!(value("Device"), Some("Samsung-970"));
    assert_eq!(value("Options"), Some("rw,noexec"));
    assert_eq!(value("Files"), Some("1,234 free of 65,536"));
    assert_eq!(
        value("Volume ID"),
        Some("5f5f5f5f-5f5f-5f5f-5f5f-5f5f5f5f5f5f")
    );
    assert_eq!(value("Space"), None, "not down to its reserve");
    assert_eq!(drive.health(), Some(VolumeHealth::Healthy));

    let bytes = drive.capacity().expect("a sized volume");
    assert_eq!(bytes.total, 4_096_000);
    assert_eq!(bytes.used_permille(), 600);
}

#[test]
fn an_unhealthy_volume_says_how() {
    let mut drive = Drive::new();
    drive.adopt(&DriveSample {
        mount: Some(record(400, MountAvailability::Degraded)),
        service: None,
        at_ns: 0,
    });
    assert_eq!(drive.facts()[0], ("Health", String::from("Degraded")));
    assert_eq!(drive.health(), Some(VolumeHealth::Degraded));
}

#[test]
fn a_rate_is_one_devices_delta_over_one_measured_interval() {
    let mut drive = Drive::new();
    let first = drive.adopt(&sample(400, Some(service(9, 1_000, 0)), 0));
    assert_eq!(drive.rates(), (None, None), "a first reading has no rate");
    assert!(!matches!(
        first,
        DriveChange::Panels(DrivePanels { activity: true, .. })
    ));
    assert!(drive.is_served());

    let second = drive.adopt(&sample(400, Some(service(9, 3_000, 500)), SECOND));
    assert_eq!(drive.rates(), (Some(2_000), Some(500)));
    assert_eq!(
        second,
        DriveChange::Panels(DrivePanels {
            capacity: false,
            activity: true,
            figures: false,
        })
    );
    assert_eq!(drive.trace().primary.len(), 1);

    // Another device behind the volume is a new series, never a delta, and
    // never a continuation of the last device's trace.
    let swapped = drive.adopt(&sample(400, Some(service(10, 9_000, 9_000)), 2 * SECOND));
    assert_eq!(drive.rates(), (None, None));
    assert!(drive.trace().is_empty(), "the trace starts afresh");
    assert!(
        matches!(
            swapped,
            DriveChange::Panels(DrivePanels { activity: true, .. })
        ),
        "the old device's trace is taken down"
    );

    // A reading without counters breaks the series too.
    drive.adopt(&sample(400, None, 3 * SECOND));
    drive.adopt(&sample(400, Some(service(10, 9_100, 9_000)), 4 * SECOND));
    assert_eq!(drive.rates(), (None, None));
}

#[test]
fn a_change_reports_only_the_part_it_moved() {
    let mut drive = Drive::new();
    drive.adopt(&sample(400, None, 0));

    let written = drive.adopt(&sample(350, None, SECOND));
    assert_eq!(
        written,
        DriveChange::Panels(DrivePanels {
            capacity: true,
            activity: false,
            figures: false,
        }),
        "a write moves the capacity panel alone"
    );

    let same = drive.adopt(&sample(350, None, 2 * SECOND));
    assert!(!same.any(), "an unchanged reading repaints nothing");

    let failing = drive.adopt(&DriveSample {
        mount: Some(record(350, MountAvailability::Recovering)),
        service: None,
        at_ns: 3 * SECOND,
    });
    assert_eq!(failing, DriveChange::Layout);
}

#[test]
fn a_format_that_counts_no_files_and_publishes_no_identity_omits_both() {
    let mut drive = Drive::new();
    let bare = MountRecord::new(
        b"",
        b"/Storage/Card",
        b"fat32",
        MountFlags::READ_ONLY,
        MountVolumeState {
            usage: VolumeStats {
                block_size: 512,
                in_reserve: true,
                total_blocks: 100,
                free_blocks: 0,
                avail_blocks: 0,
                files: 0,
                files_free: 0,
            },
            availability: MountAvailability::Available,
            medium: None,
        },
        [0; 16],
    )
    .expect("a valid record");
    drive.adopt(&DriveSample {
        mount: Some(bare),
        service: None,
        at_ns: 0,
    });
    let labels: Vec<&str> = drive.facts().iter().map(|(label, _)| *label).collect();
    assert_eq!(
        labels,
        [
            "Health",
            "Medium",
            "Filesystem",
            "Mounted at",
            "Options",
            "Block size",
            "Space"
        ]
    );
    assert_eq!(drive.facts()[1].1, "unclassified");
    assert!(!drive.is_served(), "no counters, no activity to trace");
}

#[test]
fn a_drive_sums_itself_up_in_one_line() {
    let mut drive = Drive::new();
    assert_eq!(drive.summary(), None);
    drive.adopt(&sample(400, None, 0));
    assert_eq!(drive.summary().as_deref(), Some("NVMe drive · 3.9 MiB"));
}

#[test]
fn an_unreadable_mount_table_is_stated_and_restarts_the_rates() {
    let mut drive = Drive::new();
    drive.adopt(&sample(400, Some(service(9, 0, 0)), 0));
    let change = drive.unreadable();
    assert_eq!(change, DriveChange::Layout);
    assert!(drive.is_unreadable() && drive.is_read());
    assert!(drive.facts().is_empty());
    assert!(!drive.is_served());
    assert!(
        !drive.unreadable().any(),
        "a second refusal changes nothing"
    );

    drive.adopt(&sample(400, Some(service(9, 5_000, 0)), 2 * SECOND));
    assert_eq!(drive.rates(), (None, None), "no delta across the refusal");
}

#[test]
fn a_file_created_repaints_the_facts_and_moves_nothing() {
    let mut drive = Drive::new();
    drive.adopt(&sample(400, None, 0));
    let mut fewer = record(400, MountAvailability::Available);
    let mut usage = fewer.usage();
    usage.files_free -= 1;
    fewer = MountRecord::new(
        b"nvme0p2",
        b"/Storage/Backup",
        b"arxfs",
        MountFlags::NOEXEC,
        MountVolumeState {
            usage,
            availability: MountAvailability::Available,
            medium: Some(NVME),
        },
        [0x5f; 16],
    )
    .expect("a valid record");
    let created = drive.adopt(&DriveSample {
        mount: Some(fewer),
        service: None,
        at_ns: SECOND,
    });
    assert_eq!(
        created,
        DriveChange::Panels(DrivePanels {
            capacity: true,
            activity: false,
            figures: true,
        })
    );
}

#[test]
fn gaining_or_losing_the_service_moves_the_layout() {
    let mut drive = Drive::new();
    drive.adopt(&sample(400, Some(service(9, 0, 0)), 0));
    let lost = drive.adopt(&sample(400, None, SECOND));
    assert_eq!(
        lost,
        DriveChange::Layout,
        "the activity panel goes, so the facts below it move"
    );
    let found = drive.adopt(&sample(400, Some(service(9, 0, 0)), 2 * SECOND));
    assert_eq!(found, DriveChange::Layout);
}

#[test]
fn a_paused_trace_starts_afresh_rather_than_spanning_the_pause() {
    let mut drive = Drive::new();
    drive.adopt(&sample(400, Some(service(9, 1_000, 0)), 0));
    drive.adopt(&sample(400, Some(service(9, 2_000, 0)), SECOND));
    assert_eq!(drive.trace().primary.len(), 1);
    drive.pause();
    assert!(drive.trace().is_empty());
    drive.adopt(&sample(400, Some(service(9, 600_000, 0)), 600 * SECOND));
    assert_eq!(drive.rates(), (None, None), "no rate across the pause");
    assert!(drive.trace().is_empty());
}
