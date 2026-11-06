//! The places / devices sidebar model: the shortcut rail the file manager
//! draws down the left edge of its window.
//!
//! Two kinds of row share one list. The **user's own places** — their `Home`
//! and `Desktop` folders — are fixed: they are always offered, in one order,
//! because they are where a session's work lives. The **storage** rows are
//! whatever is attached right now — the storage catalog and each volume
//! beneath it — learned from the live mount table and each carrying the
//! storage medium it actually sits on (`tairix_icon::disk_icon` turns that
//! medium into the shipped drive artwork), so a USB stick never masquerades as
//! an internal disk. The system's own tree is no place: it is reached by
//! navigating, as any folder is.
//!
//! # Data in, no I/O
//!
//! [`Places::new`] is pure: the caller supplies the home directory's path
//! components and the volumes it has already learned about, and gets back an
//! ordered, validated, deduplicated list. Nothing here opens, stats, or lists
//! a directory — the model cannot be made to touch the filesystem, so it is
//! host-testable and can never smuggle an unchecked read past the app's own
//! capability-checked seam.
//!
//! # Fail closed on malformed input
//!
//! A volume arrives from the mount table, which reports what the machine has
//! mounted — including labels and targets this process did not author. A
//! volume whose target does not parse as an absolute path, whose label is
//! empty, over-long, or carries control characters, or which lands on a target
//! an earlier row already covers, is **dropped**. It is never guessed at,
//! repaired, or shown as a row that would navigate somewhere else.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::blkio::BlkMedium;
use tairix_abi::driver::filesystem::MountFlags;
use tairix_abi::fs::STORAGE_CATALOG;
use tairix_abi::home::{HOME_DESKTOP_DIR, HOME_FILES_DIR};
use tairix_abi::sysinfo::{
    MountAvailability, MountRecord, VolumeClass, VOLUME_ID_LEN, VOLUME_ID_UNPUBLISHED,
};
use tairix_icon::{disk_icon, IconKind};
use tairix_procinfo::list::field_lossy;
use tairix_procinfo::VolumeBytes;

use crate::column::ScrollColumn;
use crate::vfs::components_from_absolute_path;

/// The longest volume label a sidebar row will accept, in bytes.
///
/// A fixed fail-closed validation bound on text this process did not author,
/// not a capacity that scales with the machine: a label longer than this is a
/// malformed record, so the row is dropped rather than truncated into
/// something that reads like a different volume. Comfortably longer than any
/// legitimate volume name while staying far below the row width a rail can
/// draw.
pub const MAX_PLACE_LABEL: usize = 64;

/// The folders inside the user's home the rail offers, in rail order, each
/// labelled with its own name and drawn with its own picture.
const USER_FOLDERS: [(&str, PlaceKind, IconKind); 2] = [
    (HOME_FILES_DIR, PlaceKind::Home, IconKind::Home),
    (HOME_DESKTOP_DIR, PlaceKind::UserFolder, IconKind::Folder),
];

/// Every fixed row's label.
///
/// The rail derives its width by measuring the widest of these in the theme's
/// body face at the desktop scale, so every fixed row's label fits without
/// truncation at any UI density. A volume whose label is longer simply
/// truncates in its row, as any over-long label does.
pub const FIXED_LABELS: [&str; 2] = [USER_FOLDERS[0].0, USER_FOLDERS[1].0];

/// The places a bare open — one naming no location — tries in turn for the
/// user whose home is `home`: their `Home` folder, then the home itself. A
/// caller falls back to the root view when neither can be listed.
#[must_use]
pub fn bare_open_places(home: &[String]) -> [Vec<String>; 2] {
    let mut files = home.to_vec();
    files.push(String::from(HOME_FILES_DIR));
    [files, home.to_vec()]
}

/// What one sidebar row is, and therefore how it was learned.
///
/// The kind is what separates the always-offered user places from the rows
/// that exist only because something is mounted: the rail draws its
/// separation where the first [`PlaceKind::Volume`] row begins, and a caller
/// refreshing the mount table replaces exactly the volume rows.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PlaceKind {
    /// The user's own files: the `Home` folder in their home.
    Home,
    /// Another fixed folder inside the user's home (`Desktop`).
    UserFolder,
    /// The storage catalog or a volume attached beneath it, learned from the
    /// live mount table.
    Volume,
}

/// One row of the places rail: what it is called, what it looks like, and
/// where activating it navigates to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Place {
    label: String,
    icon: IconKind,
    components: Vec<String>,
    kind: PlaceKind,
    available: bool,
    /// What the volume is and how it is doing, for a storage row.
    facts: Option<VolumeFacts>,
}

impl Place {
    /// The row's display label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The icon the row draws: the shipped artwork for this kind when the
    /// system has it, and the built-in glyph otherwise.
    #[must_use]
    pub const fn icon(&self) -> IconKind {
        self.icon
    }

    /// The root-first path components activating the row navigates to.
    #[must_use]
    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// What kind of place this row is.
    #[must_use]
    pub const fn kind(&self) -> PlaceKind {
        self.kind
    }

    /// What the volume a storage row opens is and how it is doing, or `None`
    /// for a folder of the user's.
    #[must_use]
    pub const fn facts(&self) -> Option<&VolumeFacts> {
        self.facts.as_ref()
    }

    /// Whether the row can still be navigated to.
    ///
    /// Every row starts available: this model performs no I/O, so it cannot
    /// know in advance whether a target is listable. A row is marked
    /// unavailable only once a navigation to it has actually been refused
    /// ([`Places::set_unavailable`]), so the rail shows a place whose
    /// directory has gone as disabled rather than silently doing nothing each
    /// time it is clicked.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        self.available
    }
}

/// What a mounted volume is and how it is doing, as its mount record states:
/// what a listing says about the volume's root where another entry states its
/// size, modification and type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeFacts {
    /// The filesystem's name as its driver reports it, empty when none.
    pub filesystem: String,
    /// The volume's own title, the label its filesystem records: empty where
    /// none is set, or the mount table withholds it.
    pub title: String,
    /// What kind of volume it is: a device's, a network share, or one a
    /// disk-image file holds.
    pub class: VolumeClass,
    /// The bytes an ordinary write may still take, or `None` for a volume that
    /// keeps no accounting.
    pub free: Option<u64>,
    /// Whether the volume is serving, and how well.
    pub availability: MountAvailability,
    /// The principal it was attached for — a network share's or a disk
    /// image's — or `None` for one no one owns.
    pub owner: Option<u32>,
    /// The identity a detach names it by, the reserved unpublished identity
    /// for one it cannot name.
    pub volume_id: [u8; VOLUME_ID_LEN],
    /// The identity its content carries: its `volume_id` but for a twin's,
    /// published under a key of its own. What a shortcut to it records.
    pub identity: [u8; VOLUME_ID_LEN],
    /// Whether it is mounted read-only, so nothing can be made on it.
    pub read_only: bool,
    /// Whether its format takes a title a retitle can set, as the mount table
    /// says: never an archive's or a share's, nor on a record withheld from
    /// its reader.
    pub titled: bool,
    /// Whether a retitle may write its title now, as the mount table says: a
    /// pressed disc's stays as it was mastered, though an image on a stick
    /// whose files take no writes takes a title.
    pub title_writes: bool,
}

/// Who may eject a volume.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Ejector {
    /// The principal it was attached for: a disk image's, a share's.
    Owner(u32),
    /// The user at the machine: a device on a port it can be pulled from,
    /// which the kernel lets that user eject while nothing holds it open.
    AtTheSeat,
}

impl Ejector {
    /// Whether the principal `uid`, using this machine, is one this lets
    /// eject: the one a volume was attached for, or anyone at the seat. The
    /// one rule every surface offering an eject asks.
    #[must_use]
    pub const fn admits(self, uid: u32) -> bool {
        match self {
            Self::Owner(owner) => owner == uid,
            Self::AtTheSeat => true,
        }
    }
}

/// Who may retitle a volume, as the kernel decides it under the volume's
/// claim.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Retitler {
    /// The principal a disk image was attached for, and no one else.
    Owner(u32),
    /// The user at the machine where they may write its root, and an
    /// administrator: a removable device's volume.
    AtTheSeat,
    /// An administrator: a fixed device's volume.
    Administrator,
}

impl Retitler {
    /// Whether the principal `uid` may ask for the retitle: a disk image's
    /// owner, or anyone for a device's volume, whom the kernel refuses only
    /// for want of an authority an administrator holds.
    #[must_use]
    pub const fn admits(self, uid: u32) -> bool {
        match self {
            Self::Owner(owner) => owner == uid,
            Self::AtTheSeat | Self::Administrator => true,
        }
    }

    /// Whether a retitle the kernel refuses for want of the authority is an
    /// administrator's to make in its place: a device's volume's, never a
    /// disk image's.
    #[must_use]
    pub const fn administered(self) -> bool {
        !matches!(self, Self::Owner(_))
    }
}

/// How a principal may retitle a volume, as [`VolumeFacts::retitling_by`]
/// answers it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Retitling {
    /// They may ask the kernel, which decides under the volume's claim.
    Allowed(Retitler),
    /// It is theirs to retitle, but not as it is mounted now: why.
    Unavailable(&'static str),
    /// It is not theirs to retitle, or no one's: why.
    Refused(&'static str),
}

impl Retitling {
    /// Why the volume may not be retitled now, or `None` where it may.
    #[must_use]
    pub const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Allowed(_) => None,
            Self::Unavailable(why) | Self::Refused(why) => Some(why),
        }
    }
}

/// Why a network share is not retitled here.
pub const REASON_SHARE_TITLE: &str = "a network share's title is its server's";

/// Why a volume published under no identity is not retitled.
pub const REASON_UNNAMED_VOLUME: &str = "the system cannot name it";

/// Why another principal's disk image is not retitled.
pub const REASON_NOT_YOURS: &str = "only whoever mounted it titles it";

/// Why a viewer the kernel would not name retitles nothing.
pub const REASON_UNKNOWN_VIEWER: &str = "the system could not say who you are";

/// Why a volume whose format fixes its title, or keeps none, is not retitled.
pub const REASON_UNTITLED_FORMAT: &str = "its format does not let its title be changed";

/// Why a volume mounted read-only is not retitled.
pub const REASON_TITLE_READ_ONLY: &str = "it is mounted read-only";

/// Why a volume whose medium has gone is not retitled.
pub const REASON_TITLE_ABSENT: &str = "its disk is not there now";

impl VolumeFacts {
    /// Whether the principal `uid`, using this machine, may eject the volume.
    #[must_use]
    pub fn ejectable_by(&self, uid: u32) -> bool {
        self.ejector().is_some_and(|ejector| ejector.admits(uid))
    }

    /// Who may eject the volume, or `None` where no one may, or a detach
    /// could not name it.
    #[must_use]
    pub fn ejector(&self) -> Option<Ejector> {
        if self.volume_id == VOLUME_ID_UNPUBLISHED {
            return None;
        }
        match (self.owner, self.class) {
            (Some(owner), _) => Some(Ejector::Owner(owner)),
            (None, VolumeClass::Removable) => Some(Ejector::AtTheSeat),
            (None, _) => None,
        }
    }

    /// Who may retitle the volume, or why no one may: a disk image's owner,
    /// the user at the machine or an administrator for a removable device's
    /// volume, and an administrator for a fixed one's; never a share, whose
    /// title is its server's, an image no one owns, or a volume a retitle
    /// could not name.
    ///
    /// # Errors
    ///
    /// Why no one may, as the user is told it.
    pub fn retitler(&self) -> Result<Retitler, &'static str> {
        if self.class == VolumeClass::Network {
            return Err(REASON_SHARE_TITLE);
        }
        if self.volume_id == VOLUME_ID_UNPUBLISHED {
            return Err(REASON_UNNAMED_VOLUME);
        }
        match (self.owner, self.class) {
            (Some(owner), _) => Ok(Retitler::Owner(owner)),
            (None, VolumeClass::Image) => Err(REASON_NOT_YOURS),
            (None, VolumeClass::Removable) => Ok(Retitler::AtTheSeat),
            (None, _) => Ok(Retitler::Administrator),
        }
    }

    /// How the principal `viewer` may retitle the volume now: the one rule
    /// every surface offering a title asks — the icon bar's plate, a drive's
    /// Info window and the desktop's drive menu. Who may comes first, so a
    /// volume another principal owns is refused as theirs whatever its record
    /// withholds; then a format that takes a title, and a volume that takes
    /// writes to its title. `None`, a viewer the kernel would not name,
    /// retitles nothing.
    #[must_use]
    pub fn retitling_by(&self, viewer: Option<u32>) -> Retitling {
        let retitler = match self.retitler() {
            Ok(retitler) => retitler,
            Err(why) => return Retitling::Refused(why),
        };
        let Some(uid) = viewer else {
            return Retitling::Refused(REASON_UNKNOWN_VIEWER);
        };
        if !retitler.admits(uid) {
            return Retitling::Refused(REASON_NOT_YOURS);
        }
        if !self.titled {
            return Retitling::Refused(REASON_UNTITLED_FORMAT);
        }
        if crate::volume::gone(self.availability) {
            return Retitling::Unavailable(REASON_TITLE_ABSENT);
        }
        if !self.title_writes || self.availability == MountAvailability::RecoveryConflict {
            return Retitling::Unavailable(REASON_TITLE_READ_ONLY);
        }
        Retitling::Allowed(retitler)
    }
}

/// One mounted volume offered to [`Places::new`].
///
/// The caller reads these from the live mount table; the model validates them
/// rather than trusting them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Volume {
    /// The volume's display label.
    pub label: String,
    /// The absolute path the volume is mounted at, in the ordinary path
    /// spelling. A target that does not parse drops the volume.
    pub target: String,
    /// The storage medium the volume's backing device sits on, or `None` when
    /// the mount reports no medium (a synthetic mount, or a class this build
    /// does not know). `None` draws the generic drive icon — never a guessed
    /// medium.
    pub medium: Option<BlkMedium>,
    /// What the volume is and how it is doing.
    pub facts: VolumeFacts,
    /// Whether the rail offers it as a place to go. One this session cannot
    /// navigate to is still drawn as a drive wherever it is listed.
    pub offered: bool,
}

impl Volume {
    /// The volume `record` reports, labelled by its mount point's leaf — the
    /// view's root by `/` itself — or `None` for one whose target is not valid
    /// text or names no leaf. One
    /// this session cannot navigate to — another principal's private volume,
    /// withheld from it, or a mount whose device has gone — is not offered.
    ///
    /// A degraded, recovering or read-only conflicted volume still serves its
    /// data, so it is offered, its facts saying how it is doing.
    #[must_use]
    pub fn from_mount(record: &MountRecord) -> Option<Self> {
        let availability = record.availability();
        let target = core::str::from_utf8(record.target_bytes()).ok()?;
        let label = target
            .rsplit('/')
            .find(|part| !part.is_empty())
            .or_else(|| (target == "/").then_some(target))?;
        Some(Self {
            label: String::from(label),
            target: String::from(target),
            medium: record.medium(),
            offered: !record.is_withheld() && !crate::volume::gone(availability),
            facts: VolumeFacts {
                filesystem: field_lossy(record.fstype_bytes()),
                title: String::from(record.title().as_str()),
                class: record.class(),
                free: VolumeBytes::of(&record.usage()).map(|bytes| bytes.available),
                availability,
                owner: record.owner(),
                volume_id: record.volume_id(),
                identity: record.identity(),
                read_only: record.flags().bits() & MountFlags::READ_ONLY.bits() != 0,
                titled: record.titled(),
                title_writes: record.title_writes(),
            },
        })
    }
}

/// The ordered rail: the user's fixed places, then the attached storage.
///
/// The order is fixed and deterministic — `Home`, `Desktop`, then the storage
/// rows sorted by label — so the rail never reshuffles under the user between
/// two paints of the same state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Places {
    rows: Vec<Place>,
    /// The storage volumes not offered as rows, in target order: drives where
    /// they are listed, never places to go.
    unoffered: Vec<Place>,
    cursor: usize,
    focused: bool,
    hovered: Option<usize>,
    /// How far the rail is scrolled, and the bar it shows while its rows
    /// outgrow the window.
    scroll: ScrollColumn,
}

impl Places {
    /// Build the rail from the user's `home` path components and the
    /// `volumes` the caller has learned about.
    ///
    /// The fixed user places come first, in their one order. They are listed
    /// whether or not their directories exist: this model performs no I/O, so
    /// it cannot know, and a user's Desktop shortcut is expected to be there
    /// even on the day the directory is missing (navigating to it then fails
    /// closed and says so, which is honest — a silently absent shortcut is
    /// not). They need a home to hang off, so an empty `home` drops them
    /// rather than spelling a row that navigates nowhere.
    ///
    /// The storage rows follow, sorted by label (stably, so equal labels keep
    /// the caller's order) after every malformed, foreign and duplicate entry
    /// has been dropped. A volume is rejected when its label is empty, longer
    /// than [`MAX_PLACE_LABEL`], or carries a control character; when its
    /// target does not parse as an absolute path; when it is neither the
    /// storage catalog nor directly beneath it — the root volume and the
    /// system tree's own mounts are not the user's storage; or when an
    /// already-accepted row already navigates to that same target. A volume
    /// not [offered](Volume::offered) is no row, but still marks its root a
    /// drive ([`crate::Browser::mark_volume_roots`]).
    #[must_use]
    pub fn new(home: &[String], volumes: &[Volume]) -> Self {
        let mut rows: Vec<Place> = Vec::new();
        if !home.is_empty() {
            for (leaf, kind, icon) in USER_FOLDERS {
                let mut components = home.to_vec();
                components.push(String::from(leaf));
                rows.push(Place {
                    label: String::from(leaf),
                    icon,
                    components,
                    kind,
                    available: true,
                    facts: None,
                });
            }
        }

        let (offered, unoffered): (Vec<&Volume>, Vec<&Volume>) =
            volumes.iter().partition(|volume| volume.offered);
        let mut accepted: Vec<(usize, Place)> = offered
            .into_iter()
            .filter_map(volume_row)
            .enumerate()
            .collect();
        // A duplicated target keeps its least label, then the first offered, so
        // the survivor depends only on the set of volumes and never on the order
        // the mount table happened to page them out in.
        accepted.sort_by(|(i, a), (j, b)| {
            a.components
                .cmp(&b.components)
                .then_with(|| a.label.cmp(&b.label))
                .then(i.cmp(j))
        });
        accepted.dedup_by(|(_, later), (_, kept)| later.components == kept.components);
        let fixed = |place: &Place| rows.iter().any(|row| row.components == place.components);
        accepted.retain(|(_, row)| !fixed(row));
        let mut unoffered: Vec<Place> = unoffered.into_iter().filter_map(volume_row).collect();
        unoffered.sort_by(|a, b| {
            a.components
                .cmp(&b.components)
                .then_with(|| a.label.cmp(&b.label))
        });
        unoffered.dedup_by(|later, kept| later.components == kept.components);
        // Searched while the offered rows are still in target order, so a
        // catalog of thousands of withheld volumes costs no pass per row.
        unoffered.retain(|place| {
            !fixed(place)
                && accepted
                    .binary_search_by(|(_, row)| row.components.cmp(&place.components))
                    .is_err()
        });
        accepted.sort_by(|(i, a), (j, b)| a.label.cmp(&b.label).then(i.cmp(j)));
        rows.extend(accepted.into_iter().map(|(_, row)| row));
        Self {
            rows,
            unoffered,
            cursor: 0,
            focused: false,
            hovered: None,
            scroll: ScrollColumn::new(),
        }
    }

    /// Every row, in rail order.
    #[must_use]
    pub fn rows(&self) -> &[Place] {
        &self.rows
    }

    /// The storage volumes listed but not offered as rows.
    pub(crate) fn unoffered(&self) -> &[Place] {
        &self.unoffered
    }

    /// Every volume listed, offered as a row or not, and the name it is shown
    /// by.
    pub fn volumes(&self) -> impl Iterator<Item = (&VolumeFacts, &str)> {
        self.rows
            .iter()
            .chain(&self.unoffered)
            .filter_map(|place| Some((place.facts()?, place.label())))
    }

    /// What the volume published as `volume_id` is and how it is doing, from
    /// its row or, for one the rail does not offer, its listing; `None` for
    /// one the mount table did not list.
    #[must_use]
    pub fn volume_facts(&self, volume_id: [u8; VOLUME_ID_LEN]) -> Option<&VolumeFacts> {
        self.rows
            .iter()
            .chain(&self.unoffered)
            .filter_map(Place::facts)
            .find(|facts| facts.volume_id == volume_id)
    }

    /// Whether a file can be made in the folder `components` name: anywhere
    /// [`stores_files`](crate::vfs::stores_files) allows but on a volume the
    /// mount table lists read-only, the deepest listed volume holding the
    /// folder deciding.
    #[must_use]
    pub fn writable_at(&self, components: &[String]) -> bool {
        crate::vfs::stores_files(components)
            && !self
                .rows
                .iter()
                .chain(&self.unoffered)
                .filter(|place| place.facts.is_some() && components.starts_with(&place.components))
                .max_by_key(|place| place.components.len())
                .and_then(Place::facts)
                .is_some_and(|facts| facts.read_only)
    }

    /// How many rows the rail has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the rail has no rows at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The index of the first volume row — where the rail draws its
    /// separation between the user's own places and the mounted volumes — or
    /// `None` when nothing is mounted (there is then nothing to separate).
    #[must_use]
    pub fn volume_start(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.kind == PlaceKind::Volume)
    }

    /// The row that navigates to exactly `components`, or `None` when no row
    /// does.
    ///
    /// This is how the rail shows the browser's current location as selected:
    /// an exact match only, so standing inside a subdirectory of a place
    /// highlights nothing rather than claiming the user is at the place
    /// itself.
    #[must_use]
    pub fn index_of(&self, components: &[String]) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.components == components)
    }

    /// The row the rail's keyboard cursor is on.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Put the keyboard cursor on `index`, ignoring an index the rail does not
    /// have.
    pub fn set_cursor(&mut self, index: usize) {
        if index < self.rows.len() {
            self.cursor = index;
        }
    }

    /// Step the keyboard cursor by `delta` rows, clamping at both ends, and
    /// report whether it moved.
    ///
    /// Clamping rather than wrapping keeps a held arrow key from cycling the
    /// rail endlessly, and it never leaves the cursor on a row that is not
    /// there.
    pub fn move_cursor(&mut self, delta: i32) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let last = self.rows.len().saturating_sub(1);
        let moved = i64::from(delta).saturating_add(i64::try_from(self.cursor).unwrap_or(i64::MAX));
        let next = usize::try_from(moved.max(0)).unwrap_or(last).min(last);
        let changed = next != self.cursor;
        self.cursor = next;
        changed
    }

    /// Whether the rail currently owns the keyboard focus.
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Give the rail the keyboard focus, or take it away.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// The row the pointer is over, or `None` when it is elsewhere.
    #[must_use]
    pub const fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Record the row the pointer is over, reporting whether it changed (so a
    /// caller repaints only when the highlight actually moves).
    pub fn set_hovered(&mut self, index: Option<usize>) -> bool {
        let index = index.filter(|&i| i < self.rows.len());
        let changed = index != self.hovered;
        self.hovered = index;
        changed
    }

    /// How far the rail is scrolled, and its bar.
    #[must_use]
    pub const fn scroll(&self) -> &ScrollColumn {
        &self.scroll
    }

    /// The rail's scroll, for a caller moving it.
    pub fn scroll_mut(&mut self) -> &mut ScrollColumn {
        &mut self.scroll
    }

    /// Mark the row at `index` as one that could not be navigated to, so it
    /// reads as disabled until the rail is rebuilt.
    ///
    /// A place is never *assumed* unavailable — this is only ever called after
    /// a real navigation was refused, so the rail reports what the filesystem
    /// actually said rather than a guess.
    pub fn set_unavailable(&mut self, index: usize) {
        if let Some(row) = self.rows.get_mut(index) {
            row.available = false;
        }
    }
}

/// Validate one offered volume into a rail row, or `None` when it is
/// malformed.
fn volume_row(volume: &Volume) -> Option<Place> {
    if volume.label.is_empty() || volume.label.len() > MAX_PLACE_LABEL {
        return None;
    }
    if volume.label.chars().any(char::is_control) {
        return None;
    }
    // A target that does not start at the root is not a mount point: the
    // shared component parser tolerates a leading-slash-free spelling, so the
    // absoluteness is checked here rather than inferred.
    if !volume.target.starts_with('/') {
        return None;
    }
    let components = components_from_absolute_path(&volume.target).ok()?;
    let storage =
        matches!(components.as_slice(), [catalog] | [catalog, _] if catalog == STORAGE_CATALOG);
    if !storage {
        return None;
    }
    Some(Place {
        label: volume.label.clone(),
        icon: disk_icon(volume.medium),
        components,
        kind: PlaceKind::Volume,
        available: true,
        facts: Some(volume.facts.clone()),
    })
}
