//! The pinboard's own model: the backdrop menu's rows — what the desktop's own
//! right-click offers, and how a chosen row is read back
//! (`plans/PINBOARD.md` §7) — and the record of where the user put their icons
//! (`plans/PINBOARD.md` §4).
//!
//! The desktop describes and the desktop's own menu service decides. This
//! module builds a [`ChainModel`] and reads a chosen row back through
//! [`PinboardCommand::from_item`]; the plate, the band, the placement, the
//! grab and the dismissal are the chain's (`plans/NEW-MENUS.md`), and nothing
//! here draws a menu pixel.
//!
//! Rows are built from one ordered [`PinboardCommand::ALL`] list and read back
//! against that same list, so a reordering cannot re-map what a row does and
//! the rows a gesture actually offers — `Open` only over an icon — cannot
//! shift the meaning of the rows around them.
//!
//! The menu holds no authority whatsoever. It *names* a command; the desktop
//! model resolves that command against its own state
//! ([`Desktop::command`](crate::Desktop::command)) and the session — which
//! holds the filesystem, spawn, and settings-store capabilities — carries the
//! resulting action out. Nothing here reads a directory, writes a store, or
//! launches anything.
//!
//! An [`Arrangement`] lives in the session's **private** app-data scope, one
//! key per icon the user placed: where a user keeps their files is theirs, and
//! the published scope would hand it to every application they run. It is read
//! and written off the loop through the same [`RecordMemory`] the launcher's
//! record is.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::VOLUME_ID_LEN;
use tairix_abi::window_ipc::{AppMenuItemId, AppMenuSymbol};
use tairix_abi::FileId;
use tairix_appdata::AppDataHost;
use tairix_controls::{ChainModel, ChainRow, ControlState, MenuItem};
use tairix_proglib::personal::FORGOTTEN;
use tairix_wallpaper::{DesktopSettings, IconFlow, IconSort};

use crate::launcher::{load_kept, Kept, RecordMemory};

/// The root plate's title: the surface the menu acts on, named as the user
/// knows it.
const PINBOARD_TITLE: &str = "Desktop";

/// The row the tidying orders are filed under.
const TIDY_UP_LABEL: &str = "Tidy Up";

/// Why the sort order already in force is not offered again.
const REASON_CURRENT_ORDER: &str = "already the listing order";

/// Why the arrangement already in force is not offered again.
const REASON_CURRENT_ARRANGEMENT: &str = "already the arrangement";

/// The order *Tidy Up* lays every icon out in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TidyOrder {
    /// By file name.
    Name,
    /// By modification date.
    Date,
    /// By file size.
    Size,
}

impl TidyOrder {
    /// The listing order this tidies in, so a tidy and a sort agree on what
    /// each ordering means.
    #[must_use]
    pub const fn sort(self) -> IconSort {
        match self {
            Self::Name => IconSort::Name,
            Self::Date => IconSort::Date,
            Self::Size => IconSort::Size,
        }
    }
}

/// What choosing a pinboard menu row asks for.
///
/// A closed set: every row of the menu names exactly one of these, and the
/// desktop model is the single place each is turned into a
/// [`DesktopAction`](crate::DesktopAction).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PinboardCommand {
    /// Open the icon the menu was opened on.
    Open,
    /// Eject the volume the drive the menu was opened on stands for.
    Eject,
    /// Retitle the volume the drive the menu was opened on stands for, with
    /// the title its field commits.
    Title,
    /// Move the icon the menu was opened on to the Trash: refused for a
    /// drive, which is drawn rather than stored.
    MoveToTrash,
    /// Create a new folder in the desktop folder.
    NewFolder,
    /// List the desktop folder in this order.
    SortBy(IconSort),
    /// Arrange the desktop icons from this edge.
    ArrangeFrom(IconFlow),
    /// Lay every icon out on the grid in this order, and keep each there.
    TidyUp(TidyOrder),
    /// List the desktop folder again now.
    Refresh,
    /// Show the desktop folder in the file manager.
    OpenDesktopFolder,
    /// Open the settings surface where the desktop picture is chosen.
    ChangeBackground,
}

impl PinboardCommand {
    /// Every command, in the order the menu lists them.
    ///
    /// A row's id is its position here, so the list fixes both the order the
    /// plate shows and the id an answer names — and a gesture that leaves
    /// `Open` out shifts neither.
    pub const ALL: [Self; 17] = [
        Self::Open,
        Self::Eject,
        Self::Title,
        Self::MoveToTrash,
        Self::NewFolder,
        Self::SortBy(IconSort::Name),
        Self::SortBy(IconSort::Kind),
        Self::SortBy(IconSort::Size),
        Self::SortBy(IconSort::Date),
        Self::ArrangeFrom(IconFlow::Leading),
        Self::ArrangeFrom(IconFlow::Trailing),
        Self::TidyUp(TidyOrder::Name),
        Self::TidyUp(TidyOrder::Date),
        Self::TidyUp(TidyOrder::Size),
        Self::Refresh,
        Self::OpenDesktopFolder,
        Self::ChangeBackground,
    ];

    /// The command the chosen row `item` names, or `None` for an id this menu
    /// never declared (fail closed — a command is never guessed at).
    ///
    /// The inverse of the numbering [`model`] gives each row, which is the
    /// shared one every command-list menu uses.
    #[must_use]
    pub fn from_item(item: AppMenuItemId) -> Option<Self> {
        Self::ALL.get(item.index()).copied()
    }

    /// The row label.
    ///
    /// The arrangement rows are worded as the *directions on screen* a user
    /// sees, not as the settings vocabulary's leading/trailing edges, because
    /// a menu is read by someone looking at their icons.
    ///
    /// Public because a host-side observer resolves a drawn row by the text it
    /// shows, so the label it looks for is this one definition rather than a
    /// copy of the wording.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Eject => "Eject",
            Self::Title => "Title",
            Self::MoveToTrash => "Move to Trash",
            Self::NewFolder => "New Folder",
            Self::SortBy(IconSort::Name) => "Sort by Name",
            Self::SortBy(IconSort::Kind) => "Sort by Kind",
            Self::SortBy(IconSort::Size) => "Sort by Size",
            Self::SortBy(IconSort::Date) => "Sort by Date",
            Self::ArrangeFrom(IconFlow::Leading) => "Arrange from the Left",
            Self::ArrangeFrom(IconFlow::Trailing) => "Arrange from the Right",
            Self::TidyUp(TidyOrder::Name) => "By Name",
            Self::TidyUp(TidyOrder::Date) => "By Date",
            Self::TidyUp(TidyOrder::Size) => "By Size",
            Self::Refresh => "Refresh",
            Self::OpenDesktopFolder => "Open Desktop Folder",
            Self::ChangeBackground => "Change Background…",
        }
    }

    /// The function this command's row shows.
    const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::Open => AppMenuSymbol::Open,
            Self::Eject => AppMenuSymbol::Drive,
            Self::Title => AppMenuSymbol::Rename,
            Self::MoveToTrash => AppMenuSymbol::Trash,
            Self::NewFolder => AppMenuSymbol::NewFolder,
            Self::SortBy(IconSort::Name) | Self::TidyUp(TidyOrder::Name) => {
                AppMenuSymbol::SortByName
            }
            Self::SortBy(IconSort::Kind) => AppMenuSymbol::SortByKind,
            Self::SortBy(IconSort::Size) | Self::TidyUp(TidyOrder::Size) => {
                AppMenuSymbol::SortBySize
            }
            Self::SortBy(IconSort::Date) | Self::TidyUp(TidyOrder::Date) => {
                AppMenuSymbol::SortByDate
            }
            Self::ArrangeFrom(IconFlow::Leading) => AppMenuSymbol::ArrangeLeft,
            Self::ArrangeFrom(IconFlow::Trailing) => AppMenuSymbol::ArrangeRight,
            Self::Refresh => AppMenuSymbol::Refresh,
            Self::OpenDesktopFolder => AppMenuSymbol::OpenFolder,
            Self::ChangeBackground => AppMenuSymbol::Wallpaper,
        }
    }

    /// Whether this command begins a new group, drawing a divider above it.
    const fn opens_group(self) -> bool {
        matches!(
            self,
            Self::NewFolder
                | Self::SortBy(IconSort::Name)
                | Self::ArrangeFrom(IconFlow::Leading)
                | Self::Refresh
        )
    }

    /// Whether `settings` already has this command's effect in force.
    ///
    /// The sort orders and the arrangements are each a group of alternatives
    /// exactly one of which holds, so the one in force is the group's chosen
    /// member; every other command asks for an action rather than for a state,
    /// and none of them is ever already done.
    fn in_force(self, settings: &DesktopSettings) -> bool {
        match self {
            Self::SortBy(sort) => sort == settings.sort,
            Self::ArrangeFrom(flow) => flow == settings.icons,
            _ => false,
        }
    }

    /// Why choosing this command would change nothing, when it would.
    const fn already_reason(self) -> Option<&'static str> {
        match self {
            Self::SortBy(_) => Some(REASON_CURRENT_ORDER),
            Self::ArrangeFrom(_) => Some(REASON_CURRENT_ARRANGEMENT),
            _ => None,
        }
    }

    /// Whether this command is offered over `over`: opening asks for an icon,
    /// and ejecting, retitling or moving to the Trash for a drive the desktop
    /// draws; the rest are the backdrop's own.
    const fn offered(self, over: Over<'_>) -> bool {
        match self {
            Self::Open => !matches!(over, Over::Backdrop),
            Self::Eject | Self::Title | Self::MoveToTrash => matches!(over, Over::Drive { .. }),
            _ => true,
        }
    }

    /// Why this command cannot be chosen over `over`, where it is offered but
    /// refused: an eject or a retitle the viewer may not make, a drive, which
    /// is no file to move to the Trash, and an open of several icons at once.
    const fn refused(self, over: Over<'_>) -> Option<&'static str> {
        match (self, over) {
            (Self::Open, Over::Several) => Some(REASON_SEVERAL),
            (Self::Eject, Over::Drive { eject_refused, .. }) => eject_refused,
            (
                Self::Title,
                Over::Drive {
                    title: Err(why), ..
                },
            ) => Some(why),
            (Self::MoveToTrash, Over::Drive { .. }) => Some(REASON_DRIVE_NOT_TRASHED),
            _ => None,
        }
    }

    /// This command's row over `over`, answering `id`: a drive's *Title* opens
    /// a field holding its title, the caret at its end, whose commit answers
    /// `id`.
    fn row(self, id: AppMenuItemId, over: Over<'_>, item: MenuItem) -> ChainRow {
        match (self, over) {
            (
                Self::Title,
                Over::Drive {
                    title: Ok(title), ..
                },
            ) => {
                let end = title.len();
                ChainRow::entry((id, id), (title, end..end), self.symbol(), item)
                    .opened_for_its_field()
            }
            _ => ChainRow::item(id, self.symbol(), item),
        }
    }
}

/// What the backdrop menu is opened over.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Over<'a> {
    /// The bare backdrop.
    Backdrop,
    /// An icon standing for a file.
    Icon,
    /// Several icons selected together.
    Several,
    /// A drive the desktop draws for a volume.
    Drive {
        /// Why the viewer may not eject it, or `None` where they may.
        eject_refused: Option<&'static str>,
        /// The volume's title, which its *Title* row's field opens on, or why
        /// the viewer may not retitle it.
        title: Result<&'a str, &'static str>,
    },
}

/// Why a verb that acts on one icon was not carried out over several.
pub const REASON_SEVERAL: &str = "several icons are selected; open them one at a time";

/// Why a drive the desktop draws is not moved to the Trash.
pub const REASON_DRIVE_NOT_TRASHED: &str =
    "it stands for its drive and stores no file; Settings ▸ Files chooses which drives show";

/// The backdrop menu the desktop's own secondary press opens, with the rows
/// `over` and `settings` imply.
///
/// `over` is what the press landed on, which alone decides whether `Open` is
/// offered and, over a drive, `Eject`, `Title` — whose field opens on the
/// volume's title — and `Move to Trash`, each refused with its reason where it
/// cannot be chosen. The sort order and the arrangement in
/// force are shown as their group's chosen member and are non-actionable,
/// because choosing what already holds is a statement of where the desktop is
/// rather than a command — and the first row of a plate never opens a group,
/// since there is nothing above it to divide it from. The tidying orders sit on
/// a plate of their own, opened from *Tidy Up*.
#[must_use]
pub fn model(over: Over<'_>, settings: &DesktopSettings) -> ChainModel {
    let mut model = ChainModel::new(PINBOARD_TITLE);
    let mut tidy = None;
    for (index, command) in PinboardCommand::ALL.into_iter().enumerate() {
        if !command.offered(over) {
            continue;
        }
        let Some(id) = AppMenuItemId::for_index(index) else {
            continue;
        };
        let in_force = command.in_force(settings);
        let refused = command.refused(over);
        let mut item = crate::menu::marked(MenuItem::new(command.label()), in_force);
        if refused.is_some() {
            item = item.with_state(ControlState::disabled());
        }
        let mut row = command.row(id, over, item);
        let already = in_force.then(|| command.already_reason()).flatten();
        if let Some(why) = already.or(refused) {
            row = row.explained(why);
        }
        if matches!(command, PinboardCommand::TidyUp(_)) {
            let plate = *tidy.get_or_insert_with(|| {
                let parent = ChainRow::submenu(AppMenuSymbol::Grid, MenuItem::new(TIDY_UP_LABEL));
                model.push(parent.grouped())
            });
            row = row.under(plate);
        } else if command.opens_group() && !model.rows().is_empty() {
            row = row.grouped();
        }
        model.push(row);
    }
    model
}

/// The key segment every icon's place is stored under.
pub const ARRANGEMENT_PREFIX: &str = "pinboard";

/// The most icons whose place the record keeps.
///
/// A containment bound, not a capacity: the private scope holds a bounded
/// number of settings, shared with the launcher's record and the sound
/// choices, and half of them are the desktop's.
pub const MAX_PLACED: usize = tairix_appconf::MAX_SETTINGS / 2;

/// How many units one pitch of the icon grid holds.
///
/// A place is kept in the grid's own pitch rather than in pixels, so the
/// arrangement keeps its shape through a new interface scale, density or text
/// size; this fine, an icon lands on the very pixel it was put down on wherever
/// a pitch is narrower.
pub const PLACE_UNITS: u32 = 4096;

/// How many bytes a kept place is spelled from: where it stands, the edge it
/// is measured from and its turn, then its file.
const PLACED_BYTES: usize = 37;

/// Where on the desktop an icon the user placed stands: its tile's top corner
/// on the edge it is measured from, from the icon field's own there, in
/// [`PLACE_UNITS`] of the grid's pitch.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Place {
    /// How far across.
    pub across: u32,
    /// How far down.
    pub down: u32,
    /// The edge it is measured across from: the one the icons were arranged
    /// from when it was put there, so it stays against that edge however wide
    /// the field is.
    pub from: IconFlow,
}

/// One icon's place as the record keeps it: where it stands, its turn among
/// the icons put down, and the file it was, by which a rename made while the
/// desktop was not looking is followed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Placed {
    /// The file the icon stood for when it was put there.
    pub id: FileId,
    /// Where it stands.
    pub at: Place,
    /// Its turn in the order icons were put down: one put down later stands
    /// over it.
    pub order: u32,
}

impl Placed {
    /// The value the store keeps this place as.
    fn spelled(self) -> String {
        let mut bytes = Vec::with_capacity(PLACED_BYTES);
        bytes.extend_from_slice(&self.at.across.to_be_bytes());
        bytes.extend_from_slice(&self.at.down.to_be_bytes());
        bytes.push(match self.at.from {
            IconFlow::Leading => 0,
            IconFlow::Trailing => 1,
        });
        bytes.extend_from_slice(&self.order.to_be_bytes());
        bytes.extend_from_slice(&self.id.volume);
        bytes.extend_from_slice(&self.id.node.to_be_bytes());
        tairix_util::hex::encode(&bytes)
    }

    /// The place `value` spells, or `None` for any other value.
    fn read(value: &str) -> Option<Self> {
        let mut bytes = [0u8; PLACED_BYTES];
        if !tairix_util::hex::decode_into(value, &mut bytes) {
            return None;
        }
        let (at, rest) = bytes.split_at(8);
        let (across, down) = at.split_at(4);
        let (&edge, rest) = rest.split_first()?;
        let (order, file) = rest.split_at(4);
        let (volume, node) = file.split_at(16);
        let from = match edge {
            0 => IconFlow::Leading,
            1 => IconFlow::Trailing,
            _ => return None,
        };
        Some(Self {
            id: FileId {
                volume: volume.try_into().ok()?,
                node: u64::from_be_bytes(node.try_into().ok()?),
            },
            at: Place {
                across: u32::from_be_bytes(across.try_into().ok()?),
                down: u32::from_be_bytes(down.try_into().ok()?),
                from,
            },
            order: u32::from_be_bytes(order.try_into().ok()?),
        })
    }
}

/// An edit refused because the record keeps [`MAX_PLACED`] places already.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ArrangementFull;

/// The key an icon's place is kept under: the fingerprint of a file's name,
/// since the key grammar spells no file name, or the identity of the volume a
/// drive stands for.
///
/// Unkeyed, because a key must read the same in every session; two names
/// sharing one cost each other no more than a remembered place.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum PlaceKey {
    /// A file's, by the fingerprint of its name.
    Name(u64),
    /// A drive's, by its volume's identity, so the place outlives a new title,
    /// a new catalog name and the volume being away.
    Volume([u8; VOLUME_ID_LEN]),
}

impl PlaceKey {
    /// The key the icon named `name` keeps its place under: a drive, named by
    /// its volume's root, under that volume.
    #[must_use]
    pub fn of(name: &str) -> Self {
        match tairix_browse::volume::reference(name) {
            Some(identity) => Self::of_volume(identity),
            None => Self::Name(tairix_hash::FastHash::hash_bytes(0, name.as_bytes())),
        }
    }

    /// The key the drive of the volume whose content carries `identity` keeps
    /// its place under.
    #[must_use]
    pub const fn of_volume(identity: [u8; VOLUME_ID_LEN]) -> Self {
        Self::Volume(identity)
    }

    /// Whether this is a drive's key, which names no file to follow.
    #[must_use]
    pub const fn is_volume(self) -> bool {
        matches!(self, Self::Volume(_))
    }

    /// The store key this is spelled as.
    #[must_use]
    pub fn spelled(self) -> String {
        match self {
            Self::Name(print) => format!("{ARRANGEMENT_PREFIX}.{print:016x}"),
            Self::Volume(identity) => format!(
                "{ARRANGEMENT_PREFIX}.{VOLUME_SEGMENT}.{}",
                tairix_util::hex::encode(&identity)
            ),
        }
    }

    /// The key the store key `key` spells exactly as [`spelled`](Self::spelled)
    /// does, or `None` for any other.
    fn read(key: &str) -> Option<Self> {
        let print = key.strip_prefix(ARRANGEMENT_PREFIX)?.strip_prefix('.')?;
        if let Some(identity) = print
            .strip_prefix(VOLUME_SEGMENT)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            let mut bytes = [0u8; VOLUME_ID_LEN];
            return (lowercase_hex(identity)
                && tairix_util::hex::decode_into(identity, &mut bytes))
            .then_some(Self::Volume(bytes));
        }
        let mut bytes = [0u8; 8];
        (lowercase_hex(print) && tairix_util::hex::decode_into(print, &mut bytes))
            .then(|| Self::Name(u64::from_be_bytes(bytes)))
    }
}

/// The segment a drive's place is kept under, after the record's own.
const VOLUME_SEGMENT: &str = "volume";

/// Whether `spelled` is lowercase hexadecimal digits alone, as every key the
/// record writes is.
fn lowercase_hex(spelled: &str) -> bool {
    spelled
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// One change to where the user put their icons.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArrangementEdit {
    /// Keep each named icon at its place, or forget where it stood for `None`.
    Set(Vec<(String, Option<Placed>)>),
    /// The icon kept under `from` is the file named `to` now.
    Renamed {
        /// The key it was kept under.
        from: PlaceKey,
        /// Its name now.
        to: String,
        /// The file it is.
        id: FileId,
    },
    /// Forget the places kept under these keys.
    Forget(Vec<PlaceKey>),
}

/// Where the user put their desktop icons: each place kept, by the key its
/// icon's name keeps it under.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Arrangement {
    placed: BTreeMap<PlaceKey, Placed>,
}

impl Arrangement {
    /// Where the icon named `name` stands, if the user put it somewhere.
    #[must_use]
    pub fn place_of(&self, name: &str) -> Option<Placed> {
        self.placed.get(&PlaceKey::of(name)).copied()
    }

    /// Every place kept, by key.
    pub fn places(&self) -> impl Iterator<Item = (PlaceKey, Placed)> + '_ {
        self.placed.iter().map(|(key, placed)| (*key, *placed))
    }

    /// The turn an icon put down now takes: after every one kept.
    pub(crate) fn next_order(&self) -> u32 {
        self.placed
            .values()
            .map(|placed| placed.order.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    /// Keep or forget each named icon's place, refusing whole a set that would
    /// keep more than [`MAX_PLACED`], and answer the keys that changed.
    fn set(
        &mut self,
        items: &[(String, Option<Placed>)],
    ) -> Result<Vec<PlaceKey>, ArrangementFull> {
        let keyed: Vec<(PlaceKey, Option<Placed>)> = items
            .iter()
            .map(|(name, placed)| (PlaceKey::of(name), *placed))
            .collect();
        let mut kept = self.placed.len();
        for (key, placed) in &keyed {
            match (self.placed.contains_key(key), placed) {
                (false, Some(_)) => kept = kept.saturating_add(1),
                (true, None) => kept = kept.saturating_sub(1),
                _ => {}
            }
        }
        if kept > MAX_PLACED {
            return Err(ArrangementFull);
        }
        let mut changed = Vec::new();
        for (key, placed) in keyed {
            let was = match placed {
                Some(placed) => self.placed.insert(key, placed),
                None => self.placed.remove(&key),
            };
            if was != placed {
                changed.push(key);
            }
        }
        Ok(changed)
    }

    /// Keep the place kept under `from` under `to`'s key instead, as the file
    /// `id`, answering the keys that changed.
    fn renamed(&mut self, from: PlaceKey, to: &str, id: FileId) -> Vec<PlaceKey> {
        let Some(mut placed) = self.placed.remove(&from) else {
            return Vec::new();
        };
        placed.id = id;
        let key = PlaceKey::of(to);
        self.placed.insert(key, placed);
        if key == from {
            alloc::vec![key]
        } else {
            alloc::vec![from, key]
        }
    }
}

/// The record is read tolerantly: a place it cannot read is left out and
/// named, so one bad line costs one icon its place rather than every icon its
/// own.
impl Kept for Arrangement {
    type Edit = ArrangementEdit;
    type Full = ArrangementFull;
    const OVERFULL: ArrangementFull = ArrangementFull;

    fn read<'a>(settings: impl Iterator<Item = (&'a str, &'a str)>) -> (Self, Vec<String>) {
        let mut arrangement = Self::default();
        let mut refused = Vec::new();
        for (key, value) in settings {
            if !in_record(key) || value == FORGOTTEN {
                continue;
            }
            let room = arrangement.placed.len() < MAX_PLACED;
            match PlaceKey::read(key)
                .zip(Placed::read(value))
                .filter(|_| room)
            {
                Some((key, placed)) => {
                    arrangement.placed.insert(key, placed);
                }
                None => refused.push(String::from(key)),
            }
        }
        (arrangement, refused)
    }

    fn make(&mut self, edit: &ArrangementEdit) -> Result<Vec<String>, ArrangementFull> {
        let changed = match edit {
            ArrangementEdit::Set(items) => self.set(items)?,
            ArrangementEdit::Renamed { from, to, id } => self.renamed(*from, to, *id),
            ArrangementEdit::Forget(keys) => {
                let mut changed = Vec::new();
                for key in keys {
                    if self.placed.remove(key).is_some() {
                        changed.push(*key);
                    }
                }
                changed
            }
        };
        Ok(changed.into_iter().map(PlaceKey::spelled).collect())
    }

    fn kept(&self, key: &str) -> Option<String> {
        self.placed
            .get(&PlaceKey::read(key)?)
            .copied()
            .map(Placed::spelled)
    }

    fn take(&mut self, key: &str, value: Option<&str>) -> bool {
        if !in_record(key) {
            return true;
        }
        let Some(place_key) = PlaceKey::read(key) else {
            return false;
        };
        let Some(value) = value.filter(|value| *value != FORGOTTEN) else {
            self.placed.remove(&place_key);
            return true;
        };
        let room = self.placed.contains_key(&place_key) || self.placed.len() < MAX_PLACED;
        match Placed::read(value).filter(|_| room) {
            Some(placed) => {
                self.placed.insert(place_key, placed);
                true
            }
            None => false,
        }
    }
}

/// Whether `key` is one of the record's: under its prefix.
fn in_record(key: &str) -> bool {
    key.strip_prefix(ARRANGEMENT_PREFIX)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// What reading the arrangement produced: the record, or none when the store
/// could not be read, and a ready-to-print line for each place left out.
pub struct LoadedArrangement {
    /// The record, or `None` for a store that could not be read: never a
    /// stand-in a place could be stored over.
    pub arrangement: Option<Arrangement>,
    /// One `stderr` line per refused place or unreadable store.
    pub warnings: Vec<String>,
}

/// Read where the user put their icons out of this session's private scope.
pub fn load_arrangement(host: &mut dyn AppDataHost) -> LoadedArrangement {
    match load_kept::<Arrangement>(host) {
        Ok((arrangement, refused)) => LoadedArrangement {
            arrangement: Some(arrangement),
            warnings: refused
                .into_iter()
                .map(|key| format!("desktop: the icon place `{key}` is not one kept; left out\n"))
                .collect(),
        },
        Err(err) => LoadedArrangement {
            arrangement: None,
            warnings: alloc::vec![format!(
                "desktop: where the icons were put could not be read ({err:?}); they stand on \
                 the grid until it can\n"
            )],
        },
    }
}

/// Where the user put their icons, kept between the desktop and the store.
pub type ArrangementMemory = RecordMemory<Arrangement>;

#[cfg(test)]
#[path = "pinboard_tests.rs"]
mod tests;
