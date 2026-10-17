//! A drive's Properties section (`plans/NEW-FILEMANAGER.md` FM22): the volume
//! mounted at a `/Storage` entry, as the mount table reports it, and how busy
//! the device serving it has lately been.
//!
//! Pure, like [`crate::properties`]: the file manager reads the mount table
//! and the ungated service counters off its loop and hands each reading here,
//! and this keeps what the section states. It holds no authority and reads
//! nothing. Its sizes, rates, medium and health words are `lib/procinfo`'s and
//! its volume identity `lib/path`'s, so a volume reads the same here as in
//! `df`, the Switchboard and Settings.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::blkio::BlkIoCounters;
use tairix_abi::driver::filesystem::VolumeStats;
use tairix_abi::sysinfo::{
    MountAvailability, MountRecord, VolumeHealth, VolumeIoStatsRecord, MOUNT_VOLUME_ID_LEN,
    VOLUME_ID_UNPUBLISHED,
};
use tairix_controls::{DuplexRates, RateTrace};
use tairix_path::VolumeId;
use tairix_procinfo::display::format_bytes;
use tairix_procinfo::list::field_lossy;
use tairix_procinfo::rate::rate_per_sec;
use tairix_procinfo::{
    availability_name, medium_name, render_options, volume_health_name, VolumeBytes,
};

/// One reading of a drive: what is mounted at its mount point now, and the
/// serving device's cumulative counters, taken at one moment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriveSample {
    /// The mount record at the drive's mount point, or `None` when nothing
    /// is mounted there any more.
    pub mount: Option<MountRecord>,
    /// The serving device's service counters, or `None` for a volume that is
    /// not block-backed or a service that did not answer.
    pub service: Option<VolumeIoStatsRecord>,
    /// When the reading was taken, on the monotonic clock, in nanoseconds.
    pub at_ns: u64,
}

/// What a newly adopted [`DriveSample`] changed, so its window repaints the
/// parts that moved and no others.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum DriveChange {
    /// Nothing the section shows.
    #[default]
    Nothing,
    /// What the section states or which panels it has: its layout moved, so
    /// the whole section repaints.
    Layout,
    /// Panels in place, some of which changed.
    Panels(DrivePanels),
}

/// The panels a change in place repaints.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DrivePanels {
    /// How much of the volume is used.
    pub capacity: bool,
    /// The activity trace or its rates.
    pub activity: bool,
    /// A fact's figure: the facts panel.
    pub figures: bool,
}

impl DriveChange {
    /// Whether anything changed at all.
    #[must_use]
    pub const fn any(self) -> bool {
        !matches!(self, Self::Nothing)
    }

    /// The change that repaints `panels`, or nothing when none moved.
    #[must_use]
    pub fn of(panels: DrivePanels) -> Self {
        if panels == DrivePanels::default() {
            Self::Nothing
        } else {
            Self::Panels(panels)
        }
    }
}

/// What the drive's section shows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Drive {
    mount: Mount,
    device: Option<String>,
    /// The counters the next reading's rates delta against: the serving
    /// device, its counters, and when they were read.
    previous: Option<(u64, BlkIoCounters, u64)>,
    rates: DuplexRates,
    latest: (Option<u64>, Option<u64>),
    /// What the section lists, derived when a reading moves it rather than on
    /// every paint.
    facts: Vec<(&'static str, String)>,
}

/// Whether the volume has been read, and what the reading found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Mount {
    #[default]
    Reading,
    Unreadable,
    Gone,
    Mounted(Box<MountRecord>),
}

impl Drive {
    /// A drive nothing has been read about yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mount: Mount::Reading,
            device: None,
            previous: None,
            rates: DuplexRates::new(),
            latest: (None, None),
            facts: Vec::new(),
        }
    }

    /// Take `sample` in, answering what it changed.
    ///
    /// Rates need both ends of an interval on one device, so a first reading,
    /// a reading with no counters, and a reading served by a different device
    /// than the last each yield no rate and add no point — never a delta
    /// across a gap or between two devices — and a device other than the
    /// last's starts its trace afresh rather than continuing another's.
    pub fn adopt(&mut self, sample: &DriveSample) -> DriveChange {
        let mount = sample
            .mount
            .map_or(Mount::Gone, |record| Mount::Mounted(Box::new(record)));
        let device = sample
            .service
            .map(|service| String::from(service.device().as_str()))
            .filter(|name| !name.is_empty());
        let now = sample
            .service
            .map(|service| (service.dev(), service.counters(), sample.at_ns));
        let latest = match (now, self.previous) {
            (Some((dev, counters, at)), Some((was_dev, was, then))) if dev == was_dev => {
                let elapsed = at.checked_sub(then);
                (
                    rate_per_sec(counters.read_bytes, was.read_bytes, elapsed),
                    rate_per_sec(counters.write_bytes, was.write_bytes, elapsed),
                )
            }
            _ => (None, None),
        };
        let same_device = now.map(|(dev, ..)| dev) == self.previous.map(|(dev, ..)| dev);
        let restarted = !same_device && !self.rates.is_empty();
        if !same_device {
            self.rates = DuplexRates::new();
        }
        let capacity = usage(&mount) != usage(&self.mount);
        let facts = !same_facts(&mount, &self.mount)
            || device != self.device
            || now.is_some() != self.previous.is_some()
            || capacity_of(&mount).is_some() != self.capacity().is_some();
        let figures = !facts && files_free(&mount) != files_free(&self.mount);
        let activity = latest != (None, None) || latest != self.latest || restarted;
        self.rates.record(latest.0.zip(latest.1));
        self.latest = latest;
        self.previous = now;
        self.mount = mount;
        self.device = device;
        if facts || figures {
            self.facts = self.stated_facts();
        }
        if facts {
            DriveChange::Layout
        } else {
            DriveChange::of(DrivePanels {
                capacity,
                activity,
                figures,
            })
        }
    }

    /// Forget the trace: the window has stopped reading the drive, so the
    /// reading it takes next starts a new series rather than plotting one
    /// interval across the pause.
    pub fn pause(&mut self) {
        self.previous = None;
        self.rates = DuplexRates::new();
        self.latest = (None, None);
    }

    /// Whether a reading has landed.
    #[must_use]
    pub const fn is_read(&self) -> bool {
        !matches!(self.mount, Mount::Reading)
    }

    /// The mount record, once read and while the volume is still mounted.
    #[must_use]
    pub const fn record(&self) -> Option<&MountRecord> {
        match &self.mount {
            Mount::Mounted(record) => Some(record),
            Mount::Reading | Mount::Unreadable | Mount::Gone => None,
        }
    }

    /// What the volume holds, or `None` before a reading, once it is gone,
    /// or for a format that tracks no fixed capacity.
    #[must_use]
    pub fn capacity(&self) -> Option<VolumeBytes> {
        self.record()
            .and_then(|record| VolumeBytes::of(&record.usage()))
    }

    /// The volume's banded health.
    #[must_use]
    pub fn health(&self) -> Option<VolumeHealth> {
        self.record().map(|record| record.availability().health())
    }

    /// The latest read and write rates, in bytes per second, where the last
    /// interval measured them.
    #[must_use]
    pub const fn rates(&self) -> (Option<u64>, Option<u64>) {
        self.latest
    }

    /// The recent rates scaled for plotting.
    #[must_use]
    pub fn trace(&self) -> RateTrace {
        self.rates.trace()
    }

    /// Whether the volume's device reports its service at all: a volume that
    /// is not block-backed has no activity to trace.
    #[must_use]
    pub const fn is_served(&self) -> bool {
        self.previous.is_some()
    }

    /// What the drive is and how big, as the one line an identity band
    /// carries beneath its name, or `None` before a reading or once the
    /// volume is gone.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let record = self.record()?;
        let medium = medium_name(record.medium());
        let mut summary = String::with_capacity(medium.len() + 16);
        let mut letters = medium.chars();
        if let Some(first) = letters.next() {
            summary.extend(first.to_uppercase());
            summary.push_str(letters.as_str());
        }
        if let Some(bytes) = self.capacity() {
            summary.push_str(" · ");
            summary.push_str(&format_bytes(bytes.total));
        }
        Some(summary)
    }

    /// Record that the mount table could not be read, answering what that
    /// changed: the section states it rather than showing a reading it no
    /// longer has, and the next reading starts its rates afresh.
    pub fn unreadable(&mut self) -> DriveChange {
        let facts = self.mount != Mount::Unreadable;
        self.mount = Mount::Unreadable;
        self.device = None;
        self.pause();
        self.facts.clear();
        if facts {
            DriveChange::Layout
        } else {
            DriveChange::Nothing
        }
    }

    /// Whether the last reading could not be taken.
    #[must_use]
    pub const fn is_unreadable(&self) -> bool {
        matches!(self.mount, Mount::Unreadable)
    }

    /// The facts the section lists, label then value, in reading order.
    #[must_use]
    pub fn facts(&self) -> &[(&'static str, String)] {
        &self.facts
    }

    /// The facts the current reading states.
    fn stated_facts(&self) -> Vec<(&'static str, String)> {
        let Some(record) = self.record() else {
            return Vec::new();
        };
        let mut facts = Vec::with_capacity(DRIVE_FACTS_MAX);
        facts.push(("Health", health_word(record.availability())));
        facts.push(("Medium", String::from(medium_name(record.medium()))));
        facts.push(("Filesystem", field_lossy(record.fstype_bytes())));
        facts.push(("Mounted at", field_lossy(record.target_bytes())));
        if !record.source_bytes().is_empty() {
            facts.push(("Source", field_lossy(record.source_bytes())));
        }
        if let Some(device) = &self.device {
            facts.push(("Device", device.clone()));
        }
        facts.push(("Options", render_options(record.flags())));
        let usage = record.usage();
        if usage.block_size > 0 {
            facts.push(("Block size", format_bytes(u64::from(usage.block_size))));
        }
        if let Some(files) = files(&usage) {
            facts.push(("Files", files));
        }
        if usage.in_reserve {
            facts.push(("Space", String::from(IN_RESERVE)));
        }
        if let Some(id) = volume_id(record.volume_id()) {
            facts.push(("Volume ID", id));
        }
        facts
    }
}

/// The most facts [`Drive::facts`] lists.
pub const DRIVE_FACTS_MAX: usize = 11;

/// What a volume down to its reserve is said to be.
const IN_RESERVE: &str = "down to its reserve: ordinary writes may be refused";

/// The availability as the health fact reads it: the volume's banded health,
/// refined to its own state where that says more.
fn health_word(availability: MountAvailability) -> String {
    let band = volume_health_name(availability.health());
    let state = availability_name(availability);
    if availability == MountAvailability::Available || state.eq_ignore_ascii_case(band) {
        String::from(band)
    } else {
        format!("{band} \u{2014} {state}")
    }
}

/// The space accounting `mount` reports, if it is mounted.
fn usage(mount: &Mount) -> Option<VolumeStats> {
    match mount {
        Mount::Mounted(record) => Some(record.usage()),
        Mount::Reading | Mount::Unreadable | Mount::Gone => None,
    }
}

/// What `mount` holds, where its format tracks a fixed capacity.
fn capacity_of(mount: &Mount) -> Option<VolumeBytes> {
    usage(mount).and_then(|usage| VolumeBytes::of(&usage))
}

/// How many more files `mount` has room for, where its format counts them.
fn files_free(mount: &Mount) -> Option<u64> {
    usage(mount).map(|usage| usage.files_free)
}

/// Whether two readings lay the section out alike, setting the figures that
/// move with every write aside — free space and free files — which repaint
/// their own panel and never the whole section.
fn same_facts(a: &Mount, b: &Mount) -> bool {
    match (a, b) {
        (Mount::Mounted(a), Mount::Mounted(b)) => {
            a.flags() == b.flags()
                && a.availability() == b.availability()
                && a.medium() == b.medium()
                && a.volume_id() == b.volume_id()
                && a.source_bytes() == b.source_bytes()
                && a.target_bytes() == b.target_bytes()
                && a.fstype_bytes() == b.fstype_bytes()
                && a.usage().block_size == b.usage().block_size
                && a.usage().files == b.usage().files
                && a.usage().in_reserve == b.usage().in_reserve
        }
        (Mount::Reading, Mount::Reading)
        | (Mount::Unreadable, Mount::Unreadable)
        | (Mount::Gone, Mount::Gone) => true,
        _ => false,
    }
}

/// How many files the volume holds room for, where the format counts them.
fn files(usage: &VolumeStats) -> Option<String> {
    (usage.files > 0).then(|| {
        format!(
            "{} free of {}",
            group_thousands(usage.files_free.min(usage.files)),
            group_thousands(usage.files)
        )
    })
}

/// `count` with its thousands grouped, so a large figure reads at a glance.
fn group_thousands(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The volume identity as identities are read, or `None` for a volume that
/// published none.
fn volume_id(id: [u8; MOUNT_VOLUME_ID_LEN]) -> Option<String> {
    (id != VOLUME_ID_UNPUBLISHED).then(|| VolumeId::from_bytes(id).to_string())
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod tests;
