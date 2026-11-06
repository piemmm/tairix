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
use tairix_abi::driver::filesystem::{VolumeStats, VolumeTitle, VOLUME_TITLE_MAX};
use tairix_abi::sysinfo::{
    MountAvailability, MountRecord, VolumeHealth, VolumeIoStatsRecord, VOLUME_ID_LEN,
    VOLUME_ID_UNPUBLISHED,
};
use tairix_controls::text::{TextAction, TextField};
use tairix_controls::{DuplexRates, RateTrace};
use tairix_geometry::{Rect, Region};
use tairix_input::{Key, Modifiers, NamedKey};
use tairix_path::VolumeId;
use tairix_procinfo::display::format_bytes;
use tairix_procinfo::list::field_lossy;
use tairix_procinfo::rate::rate_per_sec;
use tairix_procinfo::{
    availability_name, medium_name, render_options, volume_health_name, VolumeBytes,
};

use crate::mount::MountAct;
use crate::places::{Retitling, Volume};

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

/// What the drive's section shows, its *Title* field among it.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// The volume as the reading states it, which a committed title is
    /// checked against.
    volume: Option<Volume>,
    /// The *Title* field: the title as last read, until it holds the
    /// keyboard.
    title: TextField,
    /// The user reading the drive, on whose authority its title is offered.
    viewer: Option<u32>,
}

impl Default for Drive {
    fn default() -> Self {
        Self::new()
    }
}

/// What a key on a drive's *Title* field asks of its window.
#[derive(Debug, Eq, PartialEq)]
pub enum TitleKeyed {
    /// The field does not hold the keyboard: the key is not its own.
    Ignored,
    /// The field took it, reporting what it repainted.
    Typed,
    /// The field gave the keyboard back: the section is repainted.
    Ended,
    /// The field committed this retitle, for the window to carry out off its
    /// loop, and gave the keyboard back.
    Retitle(MountAct),
    /// The commit was refused: the field says why, and keeps the keyboard.
    Refused,
}

/// What the *Title* field shows while it holds no title.
const NO_TITLE: &str = "No title";

/// The *Title* field holding `title`, editable where `titling` allows its
/// viewer and otherwise saying why not.
fn title_field(title: &str, titling: Option<Retitling>) -> TextField {
    let field = TextField::new()
        .with_text(title)
        .with_max_len(VOLUME_TITLE_MAX)
        .with_placeholder(NO_TITLE)
        .read_only(!matches!(titling, Some(Retitling::Allowed(_))));
    match titling.and_then(Retitling::refusal) {
        Some(why) => field.with_message(why),
        None => field,
    }
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
    /// A drive nothing has been read about yet, read for no one.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mount: Mount::Reading,
            device: None,
            previous: None,
            rates: DuplexRates::new(),
            latest: (None, None),
            facts: Vec::new(),
            volume: None,
            title: title_field("", None),
            viewer: None,
        }
    }

    /// This drive read for `viewer`, the user its *Title* field is offered
    /// to.
    #[must_use]
    pub fn acting_for(mut self, viewer: Option<u32>) -> Self {
        self.viewer = viewer;
        self
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
        if facts || self.title_stale() {
            self.refresh_title();
            return DriveChange::Layout;
        }
        DriveChange::of(DrivePanels {
            capacity,
            activity,
            figures,
        })
    }

    /// The *Title* field the section draws.
    #[must_use]
    pub const fn title(&self) -> &TextField {
        &self.title
    }

    /// Whether the *Title* field holds the keyboard.
    #[must_use]
    pub fn is_titling(&self) -> bool {
        self.title.state().focus.focused
    }

    /// Give the *Title* field the keyboard, answering whether it took it: one
    /// its viewer may not edit never does.
    pub fn begin_title(&mut self) -> bool {
        if self.title.is_read_only() || self.is_titling() {
            return false;
        }
        self.title.set_focused(true);
        true
    }

    /// Take the keyboard back from the *Title* field, the title as read put
    /// back in it, answering whether it held it.
    pub fn end_title(&mut self) -> bool {
        if !self.is_titling() {
            return false;
        }
        self.title.set_focused(false);
        self.refresh_title();
        true
    }

    /// Feed `key` to the *Title* field drawn at `bounds`, reporting what it
    /// repainted into `damage`, and answering what it asks of the window:
    /// `Return` commits the title through the one check every surface
    /// offering a title makes ([`MountAct::retitle`]), and `Escape` or `Tab`
    /// hands the keyboard back.
    pub fn title_key(
        &mut self,
        (key, modifiers): (Key, Modifiers),
        bounds: Rect,
        damage: &mut Region,
    ) -> TitleKeyed {
        if !self.is_titling() {
            return TitleKeyed::Ignored;
        }
        if key == Key::Named(NamedKey::Tab) {
            self.end_title();
            return TitleKeyed::Ended;
        }
        match self.title.on_key(key, modifiers, bounds, damage) {
            Some(TextAction::Submitted) => self.commit_title(),
            Some(TextAction::Cancelled) => {
                self.end_title();
                TitleKeyed::Ended
            }
            Some(TextAction::Edited) => {
                self.title.set_message(None);
                TitleKeyed::Typed
            }
            None => TitleKeyed::Typed,
        }
    }

    /// Commit what the *Title* field holds: the retitle it asks for, the field
    /// keeping the title until a reading says what the volume records; or,
    /// refused, the field saying why and keeping the keyboard.
    fn commit_title(&mut self) -> TitleKeyed {
        let Some(volume) = &self.volume else {
            self.end_title();
            return TitleKeyed::Ended;
        };
        match MountAct::retitle(&volume.facts, &volume.label, self.viewer, self.title.text()) {
            Ok(act) => {
                self.title.set_focused(false);
                act.map_or(TitleKeyed::Ended, TitleKeyed::Retitle)
            }
            Err(refusal) => {
                self.title.set_message(Some(String::from(refusal.reason)));
                TitleKeyed::Refused
            }
        }
    }

    /// Bring the *Title* field to the reading: the title as read, editable
    /// where the viewer may retitle the volume and otherwise saying why not.
    /// A field holding the keyboard keeps what was typed while the volume is
    /// still theirs to retitle.
    fn refresh_title(&mut self) {
        self.volume = self.record().and_then(Volume::from_mount);
        let titling = self
            .volume
            .as_ref()
            .map(|volume| volume.facts.retitling_by(self.viewer));
        if self.is_titling() && matches!(titling, Some(Retitling::Allowed(_))) {
            return;
        }
        let title = self
            .volume
            .as_ref()
            .map_or("", |volume| volume.facts.title.as_str());
        self.title = title_field(title, titling);
    }

    /// Whether the *Title* field, not holding the keyboard, shows other than
    /// the title as read: a commit the kernel refused, or one made and now
    /// read.
    fn title_stale(&self) -> bool {
        let read = self.record().map_or(VolumeTitle::EMPTY, MountRecord::title);
        !self.is_titling() && self.title.text() != read.as_str()
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
        self.refresh_title();
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
pub(crate) fn health_word(availability: MountAvailability) -> String {
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
                && a.title() == b.title()
                && a.titled() == b.titled()
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
fn volume_id(id: [u8; VOLUME_ID_LEN]) -> Option<String> {
    (id != VOLUME_ID_UNPUBLISHED).then(|| VolumeId::from_bytes(id).to_string())
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod tests;
