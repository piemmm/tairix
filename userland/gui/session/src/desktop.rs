//! The desktop: the user's own `Desktop` folder shown as icons on the screen,
//! each standing where the user put it (`plans/NEW-TASKBAR.md` T16,
//! `plans/PINBOARD.md`).
//!
//! The desktop is a *directory view*, not a new kind of surface. It lists the
//! user's `Desktop` folder through the same [`DirectorySource`] seam the
//! trusted file picker uses, under the session's own identity; it orders the
//! listing with the shared [`sort_entries`]; it names each child as a
//! file-manager window does, less the endings the file manager's settings
//! hide ([`Desktop::hide_endings`]), and leaves out a hidden item as such a
//! window does ([`Desktop::show_hidden_items`]); it classifies each child with the shared
//! content-type registry; and its cells are the shared [`GridView`]'s,
//! differing from the file manager's grid only in its [`GridFlow`] and
//! [`GridFill`] — they run down from the edge the user's arrangement names and
//! keep a fixed pitch, so a cell does not drift when the work area changes
//! size. There is no second grid, no second sort, and no second classifier
//! anywhere in this module.
//!
//! # Where an icon stands
//!
//! An icon the user put somewhere — carried across the desktop, dropped or
//! saved onto it, or tidied — stands exactly there, over another if that is
//! where it was put, and the session keeps the place in its private scope
//! ([`Arrangement`]). Every other icon takes the next free cell in listing
//! order and keeps it while the field stands; another field, order or
//! arrangement flows them again, and nothing about them is stored. A place
//! the field no longer holds whole is shown on the free cell nearest it and
//! kept as it was. Each icon is a rectangle of its own, the one put down last
//! on top, so a press and a repaint test them from the topmost down, and only
//! the icons the field shows are ever walked.
//!
//! # The pinboard settings live here
//!
//! The desktop owns the user's [`DesktopSettings`] — the wallpaper and its
//! fit, the backdrop colour, the icon arrangement, the sort order and the
//! grid — as the single copy inside the session: the shell reads them from
//! the desktop rather than holding a second set that could drift from the one
//! the icons are actually laid out by. An edit arrives through
//! [`Desktop::apply_settings`], which reports exactly the work it implies, so
//! changing the sort order does not decode a wallpaper and changing the
//! wallpaper does not re-read the folder.
//!
//! What the desktop *owns* is the behaviour a folder-on-the-screen needs: a
//! selection, keyboard navigation while it holds focus, and activation; a
//! pointer merely over an icon changes nothing about it. Each of those reuses
//! the pure engine that already decides it —
//! [`DoubleClickTracker`] for "is this the second click?" — so the desktop can
//! never disagree with the file manager about what a gesture means.
//!
//! # Shortcuts point *into* the desktop, never out of it
//!
//! The program library's row menu asks this folder for a shortcut
//! ([`Desktop::shortcut_to`]) and never the other way round. A `.app`
//! directory a user drops on their own `Desktop` is a directory *shaped* like
//! an application, not a catalogued one, so the desktop is not a source a
//! launcher can be populated from; the library's rows are catalogued entries
//! by construction (see [`tairix_taskbar::LibraryPopup`]) and are what a
//! shortcut is made from.
//!
//! # Following the folder
//!
//! The icons follow their folder through a directory watch
//! (`docs/src/filesystem/watch.md`): whatever program changes it, the
//! embedder drains the report off the loop and hands the changed entries to
//! [`Desktop::apply_changes`], which merges them in place and repaints only
//! the icons they moved. A file renamed keeps its place and one deleted gives
//! it up. The folder is read whole only at bring-up, when the watch asks for a
//! rescan, after an action of the session's own, on the user's Refresh, and,
//! where no watch follows it, as the mount table moves.
//!
//! # Drives
//!
//! A shortcut to a volume — a link storing the volume's root by its identity,
//! which the file manager makes where a volume is dropped from `/Storage` — is
//! drawn as the drive its volume is on while the mount table shows the volume
//! attached, and faded while not; opening it opens the volume as the storage
//! catalog does, or says it is not there. The desktop also draws a drive for
//! each attached volume of a class the file manager's settings name
//! ([`DesktopDrives`]), but for one a shortcut already stands for: drawn, never
//! written, each named by its volume's root, which no listed name can be, and
//! keeping its place under that volume ([`PlaceKey::of_volume`]). The mount
//! table is read off the loop as its notices arrive, one read under way and one
//! more owed at most ([`Desktop::take_mount_read`]).
//!
//! The model holds no authority: it *names* what should happen
//! ([`DesktopAction`]) and the embedder — which holds the spawn and
//! filesystem capabilities — carries it out.

use alloc::borrow::Cow;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;

use tairix_abi::driver::filesystem::VolumeTitle;
use tairix_abi::sysinfo::VOLUME_ID_LEN;
use tairix_abi::window_ipc::{
    AuthorisedOutcome, DocumentName, DragItems, DragPicture, LandingOutcome, LandingStep,
    SystemTask, DRAG_SHOWN_MAX, DROP_LANDINGS_MAX, PICK_MAX_OPEN,
};
use tairix_abi::{Errno, FileId};
use tairix_browse::chrome::REASON_NOT_EJECTABLE;
use tairix_browse::mount::{
    title_refusal, MountAct, MountDone, ADMINISTRATOR_BUSY, ADMINISTRATOR_FAILED,
    ADMINISTRATOR_UNASKED,
};
use tairix_browse::places::REASON_TITLE_ABSENT;
use tairix_browse::render::{entry_parts, tile_core, Tiles};
use tairix_browse::Selection;
use tairix_browse::{
    applications_for, entry_icon, merge_changes, resolve_occupancy, sort_entries, volume,
    AppAssociation, DesktopDrives, DirectorySource, Entry, EntryChange, EntryKind,
    FilerPreferences, GridFlow, GridView, HiddenEndings, LinkTarget, Listing, NewEntry, Order,
    Pictures, SortKey, SortMode, Volume,
};
use tairix_controls::state::{ControlState, FocusState, PointerState, SelectionState};
use tairix_controls::{ChainModel, IconTile, LabelHalo};
use tairix_geometry::{to_i32, GridFill, GridRun, Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind, IconRequest, Landed};
use tairix_input::Modifiers;
use tairix_proglib::{Catalog, EntryId};
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_wallpaper::{DesktopSettings, IconFlow, IconSort};
use tairix_wm::{past_drag_slop, ClickKind, DoubleClickTracker, Key, NamedKey, PointerButton};

use crate::drag::{DesktopCargo, DesktopItem};
use crate::launcher::Kept;
use crate::library::catalogued;
use crate::pinboard::{
    Arrangement, ArrangementEdit, ArrangementFull, ArrangementMemory, Over, PinboardCommand, Place,
    PlaceKey, Placed, TidyOrder, MAX_PLACED, PLACE_UNITS, REASON_DRIVE_NOT_TRASHED, REASON_SEVERAL,
};

/// The inset, in logical pixels at the reference density, between the work
/// area's edges and the first icon.
///
/// A deliberate, fixed piece of visual spacing — not a capacity — so the
/// trailing column does not touch the screen edge and the top icon clears the
/// work area's top edge.
pub const DESKTOP_MARGIN: u32 = 8;

/// Where the desktop's cells rest: their anchored edge, always. The desktop
/// does not scroll.
const DESKTOP_SCROLL: u64 = 0;

/// The most icons drawn by the picture their drag carried while their own is
/// produced: as many as the drops a window lands at once show. A containment
/// bound, not a capacity: past it the oldest is drawn by its own picture
/// alone, long since produced.
const STAND_INS_MAX: usize = DROP_LANDINGS_MAX * DRAG_SHOWN_MAX;

/// What activating a desktop icon means, resolved by the model and carried
/// out by the embedder (which holds the spawn capability).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopActivation {
    /// Open the file manager showing the directory at this absolute path.
    OpenFolder {
        /// The absolute path of the directory to show.
        path: String,
    },
    /// Launch an application: the absolute path of its `Run` binary, the name
    /// to report it by, and the document the user opened with it, if any.
    Launch {
        /// Absolute path of the bundle's `Run` entry-point binary.
        run_path: String,
        /// Display name for the launch record and any diagnosis.
        label: String,
        /// The document to open, if this launch came from a plain file.
        document: Option<LaunchDocument>,
    },
    /// Mount the disk image at this absolute path for the user, and show the
    /// volume it holds.
    MountImage {
        /// The absolute path of the image, or of the shortcut naming it.
        path: String,
    },
}

/// A document the desktop opens for the application it launches.
///
/// The session opens it and hands the application the descriptor, because an
/// application that requests no filesystem capability cannot act on a path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchDocument {
    /// Absolute path of the file, or of the shortcut naming it.
    pub path: String,
    /// Whether the application's signed manifest claims to edit what it
    /// opens, so the document is opened read-write where the user may write
    /// it.
    pub edits: bool,
}

/// A settings change the desktop's backdrop menu asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingsEdit {
    /// Order the icons this way.
    Sort(IconSort),
    /// Flow the icons from this corner.
    Icons(IconFlow),
}

impl SettingsEdit {
    /// `settings` with this edit made.
    #[must_use]
    pub fn applied_to(self, mut settings: DesktopSettings) -> DesktopSettings {
        match self {
            Self::Sort(sort) => settings.sort = sort,
            Self::Icons(icons) => settings.icons = icons,
        }
        settings
    }

    /// Whether `settings` already say what this edit would.
    #[must_use]
    pub fn is_in(self, settings: &DesktopSettings) -> bool {
        match self {
            Self::Sort(sort) => settings.sort == sort,
            Self::Icons(icons) => settings.icons == icons,
        }
    }
}

/// What one desktop gesture asks the session to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopAction {
    /// Carry out this activation.
    Activate(DesktopActivation),
    /// Create a directory at this absolute path.
    ///
    /// The name is already chosen — through the shared new-directory naming
    /// the file manager uses, over the listing the desktop is showing — so the
    /// embedder only holds the filesystem capability and makes the directory.
    CreateFolder {
        /// Absolute path of the directory to create.
        path: String,
    },
    /// Create a symbolic link — a desktop shortcut — at `link`, storing
    /// `target` verbatim.
    ///
    /// The name is already spelled and already validated against the one
    /// shared name rule ([`Desktop::shortcut_to`]), so the embedder only holds
    /// the filesystem capability and makes the link. The target is stored as
    /// *data* and never resolved here: a shortcut whose bundle is later
    /// removed dangles honestly rather than being prevented at creation.
    CreateShortcut {
        /// Absolute path of the link to create, inside the desktop folder.
        link: String,
        /// The path the link stores, exactly as it was given.
        target: String,
    },
    /// Make this edit: the embedder makes it over the newest settings it has
    /// asked to publish, persists them to the user's own store and hands them
    /// back through [`Desktop::apply_settings`], which reports the work the
    /// edit actually implies.
    ///
    /// The model names the change, not a document: one built over the
    /// settings it last adopted would undo a change still on its way to the
    /// store. Nor does it apply the change itself, so there is exactly one
    /// place settings are adopted and the persisted document and the live
    /// desktop can never drift apart.
    EditSettings(SettingsEdit),
    /// Open the settings surface where the desktop picture is chosen,
    /// which is an installed application the embedder resolves and launches
    /// (the model knows no bundle paths).
    ChangeBackground,
    /// Eject or retitle the volume a drive the desktop draws stands for: the
    /// embedder carries the act out off its loop and states what it came to.
    Volume(MountAct),
    /// The gesture was refused, and why: a refused action always says so
    /// rather than failing silently.
    Refuse(GestureRefusal),
}

/// A refused gesture, worded once for the alert the user reads and the line
/// `stderr` records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GestureRefusal {
    headline: String,
    detail: String,
}

impl GestureRefusal {
    /// The refusal `headline`, explained by `detail`.
    #[must_use]
    pub fn new(headline: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            headline: headline.into(),
            detail: detail.into(),
        }
    }

    /// What was refused.
    #[must_use]
    pub fn headline(&self) -> &str {
        &self.headline
    }

    /// Why.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// The refusal as one newline-terminated line for `stderr`.
    #[must_use]
    pub fn line(&self) -> String {
        format!("desktop: {}: {}\n", self.headline, self.detail)
    }
}

/// The work a change to the *backdrop* implies: the wallpaper, and the icons
/// standing on it.
///
/// Each field names one piece of work the edit asks for, so a change of
/// arrangement does not cost a wallpaper decode. A new sort order or backdrop
/// colour is neither: the desktop re-sorts the icons it shows as it adopts the
/// settings, and either shows through the [`PinboardChange::layer`] repaint.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BackdropWork {
    /// The icon arrangement moved: the grid must be laid out again before the
    /// next paint or hit-test.
    pub relayout: bool,
    /// The wallpaper image or its fit changed: the embedder must prepare the
    /// screen-sized wallpaper surface again and hand it to the shell.
    pub wallpaper: bool,
}

/// The work a change to how the desktop is *drawn* implies.
///
/// Separate from [`BackdropWork`] because it is a different embedder's job:
/// the backdrop work is the desktop layer's alone, while these reach the
/// theme registry, the output's density, and every application on the
/// screen.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AppearanceWork {
    /// The appearance, contrast, density, motion or text changed: the
    /// embedder must re-theme the desktop and republish it, because every
    /// open application draws its own pixels and would otherwise be left in
    /// the appearance the user has just stopped asking for.
    pub theme: bool,
    /// The UI scale changed: the embedder must rescale the output and
    /// republish, so every logical length on the desktop and in every
    /// application resolves at the new density.
    pub scale: bool,
    /// The cursor set, the pointer size or its shadow changed: the embedder
    /// must re-render the pointer.
    ///
    /// Separate from `scale`, which also moves the pointer's pixel side,
    /// because the two are answered in different places: a scale change
    /// rescales the whole output and the pointer follows, while this is the
    /// pointer alone and nothing else on screen moves.
    pub cursor: bool,
}

impl AppearanceWork {
    /// Whether anything the desktop is drawn with moved.
    #[must_use]
    pub const fn any(self) -> bool {
        self.theme || self.scale || self.cursor
    }
}

/// The work a settings edit implies.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PinboardChange {
    /// The desktop layer is drawn differently — its backdrop colour, the
    /// icons' arrangement or order, or the theme or scale they are drawn at,
    /// though never the motion, which moves no resting pixel — so it must be
    /// repainted whole. Most of the settings document moves no
    /// pixel of it: the seat's input, the pointer aids and the idle policy are
    /// all adopted elsewhere, and a new wallpaper repaints the layer when it
    /// lands.
    pub layer: bool,
    /// What the backdrop and its icons owe.
    pub backdrop: BackdropWork,
    /// What the desktop's appearance owes.
    pub appearance: AppearanceWork,
    /// The notification policy changed: what is already showing must be
    /// held to the new one.
    pub notifications: bool,
    /// The region or the zone changed: the clocks must be read in the new
    /// ones.
    pub clock: bool,
}

/// One icon the desktop shows, as the paint and the artwork landing see it.
struct ShownIcon<'a> {
    index: usize,
    entry: &'a Entry,
    /// Where the tile draws, strictly inside.
    bounds: Rect,
    kind: IconKind,
    request: IconRequest<'a>,
    /// The pixel side the picture resolves at.
    side: u32,
}

/// The outcome of one desktop command: whether every icon may have moved, and
/// what (if anything) the session must now do.
///
/// What the gesture *changed on screen* is not here. Every gesture takes a
/// [`Region`] sink and adds the icons it altered to it, so the embedder
/// repaints those rather than the whole desktop layer: the desktop is the
/// bottom layer, and marking all of it recomposites every window above it and
/// throws away every frosted backdrop over it — a screenful of work to move
/// one highlight.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DesktopOutcome {
    /// The command moved the icons as a whole — it listed the folder again on
    /// the spot and found it changed, or tidied every icon — so the whole
    /// layer is repainted. A re-list read elsewhere lands through
    /// [`Desktop::resume`] instead.
    pub rearranged: bool,
    /// The action the gesture asks for, if any.
    pub action: Option<DesktopAction>,
}

impl DesktopOutcome {
    /// The gesture asks for nothing.
    #[must_use]
    pub const fn ignored() -> Self {
        Self {
            rearranged: false,
            action: None,
        }
    }

    /// The gesture asks for `action`.
    #[must_use]
    pub const fn acting(action: DesktopAction) -> Self {
        Self {
            rearranged: false,
            action: Some(action),
        }
    }
}

/// What a drag over the desktop would leave there if it were let go: the
/// folder it lands in, the name it is led by, and, over the bare desktop,
/// how each item shown was drawn and where it was held from the press.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LandingOffer {
    folder: String,
    lead: String,
    /// The pictures shown, each with where it stood from the press, the
    /// lead's first; `None` over a folder icon, whose contents the desktop
    /// does not show.
    shown: Option<Vec<DragPicture>>,
}

impl LandingOffer {
    /// What a drag of `items` would leave in `folder`: standing where it is
    /// let go when that is the bare desktop, `bare`, and inside the folder
    /// icon there otherwise.
    #[must_use]
    pub fn of(items: &DragItems, folder: String, bare: bool) -> Self {
        Self {
            folder,
            lead: items.first().as_str().to_string(),
            shown: bare.then(|| items.pictures().copied().collect()),
        }
    }
}

/// What one step of a drop asks of the embedder, which holds the artwork.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LandingWork {
    /// Produce nothing pictured from at or under this path until it is
    /// released: an item is being written there.
    pub hold: Option<String>,
    /// Picture again what was held under this path: its item is done.
    pub release: Option<String>,
}

/// A drop onto the desktop its source is carrying out, or a save the session
/// made there, until the last name it owes arrives.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Landing {
    /// The window its drag began in and the report it was dropped at; `None`
    /// for a save, which no source steps.
    source: Option<(u64, u32)>,
    /// The absolute path of the folder it lands in.
    folder: String,
    /// Where it stands what it makes on the bare desktop; `None` inside a
    /// folder icon.
    places: Option<Places>,
    /// How its drag drew each item it showed, the lead's first.
    pictures: Vec<DragPicture>,
    /// The item being written.
    making: Option<Making>,
    /// Items made whose names have yet to be listed, each with its item.
    owed: Vec<(String, u32)>,
    /// Whether its source has ended it, so nothing more is made.
    ended: bool,
}

/// Where a drop onto the bare desktop stands what it makes.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Places {
    /// Where each item shown stands, in the arrangement it was carried in.
    shown: Vec<Place>,
    /// Where it was let go: each further item takes the free cell nearest.
    dropped: Place,
    /// The cell each further item was given, so one placed again keeps it.
    further: Vec<(u32, Place)>,
}

/// The item a drop is writing.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Making {
    item: u32,
    name: String,
    /// Its icon arrived while it was written, and already stands.
    stands: bool,
}

/// Where one icon stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Spot {
    /// Where the user put it.
    Placed(Place),
    /// The cell the flow gave it, in the field it was flowed in.
    Flowed(usize),
    /// Owed the next free cell, which the field may have none of.
    Owed,
}

/// The press on an icon, carried for as long as it is held.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Carry {
    /// The icon pressed, which leads the selection it carries.
    index: usize,
    /// Where the press began.
    from: Point,
    /// The press's offset from the tile's corner.
    grab: (i32, i32),
    /// Where the pressed tile stood as the press began.
    rest: Point,
    /// Where the pressed tile stands, once the press has travelled far enough
    /// to be a drag.
    at: Option<Point>,
}

impl Carry {
    /// How far the carried icons stand from where they rest.
    fn shift(&self) -> Option<(i32, i32)> {
        let at = self.at?;
        Some((
            at.x.saturating_sub(self.rest.x),
            at.y.saturating_sub(self.rest.y),
        ))
    }
}

/// A press on the desktop's icons that the seat carries on over a window:
/// what putting them down again needs if they are brought back and let go.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopLift {
    /// The pressed icon's name, which leads the selection carried.
    pub(crate) lead: String,
    /// The press's offset from the pressed tile's corner.
    pub(crate) grab: (i32, i32),
}

/// The names a reorder keeps the selection, the focus and a carried press on.
struct Held {
    /// The selected icons' names, sorted.
    selected: Vec<String>,
    focus: Option<String>,
    carried: Option<(String, Carry)>,
}

/// A band dragged out over the backdrop: every icon whose picture or name it
/// touches is selected as it grows, beside what the press kept.
#[derive(Debug)]
struct Band {
    /// Where the press began.
    anchor: Point,
    /// Where the pointer is.
    head: Point,
    /// Whether it has travelled past the drag slop, so it is drawn and selects.
    live: bool,
    /// The selection the press kept, which the band adds to.
    base: Selection,
    /// The icons the band touched when last swept.
    touched: Selection,
    /// What a sweep builds the next touch in, so a pointer sample allocates
    /// nothing once the band has grown to the listing.
    next: Selection,
}

impl Band {
    /// The rectangle the band covers, once it is drawn.
    fn rect(&self) -> Option<Rect> {
        self.live.then(|| Rect::spanning(self.anchor, self.head))
    }
}

/// An icon whose file left the listing, and where it stood.
struct Leaving {
    name: String,
    id: FileId,
    spot: Spot,
}

/// What the desktop knows of its folder's listing.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Listed {
    /// The entries are the folder as last read, not empty for want of a
    /// listing.
    read: bool,
    /// A source that reads elsewhere still owes the listing the last
    /// [`relist`](Desktop::relist) asked for.
    owed: bool,
}

/// The desktop's own question to an administrator, for an act on a drive the
/// user was refused for want of the authority: at most one, owed until the
/// loop reaches the prompt and then awaited.
#[derive(Default)]
enum Administer {
    #[default]
    Idle,
    /// To be asked: the act the user was refused.
    Owed(MountAct),
    /// Asked; the act, stated as refused should the task fail.
    Asked(MountAct),
}

/// What the desktop knows of the mount table: the volumes it last listed, and
/// whether a read of it is under way and another owed, so a burst of notices
/// costs one read in flight and one more at most.
#[derive(Default)]
struct Mounts {
    volumes: Vec<Volume>,
    reading: bool,
    owed: bool,
}

/// How far the record of where the user put their icons has come.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Record {
    /// Not yet read from the store.
    Unread,
    /// Read; a place kept for a file not listed has yet to be followed.
    Read,
    /// Read, and every place kept for a file not listed followed.
    Reconciled,
}

/// Where the icons stand in one field: the icon the flow stood on each cell,
/// and those the user put somewhere, the one put down first first, each with
/// the rectangle it is shown in.
#[derive(Clone, Debug, Default)]
struct Standing {
    cells: Vec<Option<usize>>,
    placed: Vec<(usize, Rect)>,
}

/// The halo each resting name was last drawn over, blurred once and kept for
/// as long as the name shown, the scale, the tile size and the theme it was
/// blurred for stand.
#[derive(Default)]
struct Halos {
    look: Option<(Scale, (u32, u32), Theme)>,
    kept: BTreeMap<String, Arc<LabelHalo>>,
}

impl Halos {
    /// Keep only the halos blurred at `scale` for a tile of `size` under
    /// `theme`.
    fn hold(&mut self, scale: Scale, size: (u32, u32), theme: &Theme) {
        let current = self
            .look
            .as_ref()
            .is_some_and(|(held, side, look)| *held == scale && *side == size && look == theme);
        if !current {
            self.kept.clear();
            self.look = Some((scale, size, theme.clone()));
        }
    }

    /// The halo `tile`, the icon named `name`, draws its name over: the one
    /// kept, else one blurred now and kept; `None` where none can be blurred.
    fn of(&mut self, name: &str, tile: &IconTile) -> Option<Arc<LabelHalo>> {
        if let Some(kept) = self.kept.get(name) {
            return Some(Arc::clone(kept));
        }
        let (scale, size, theme) = self.look.as_ref()?;
        let halo = Arc::new(tile.label_halo(*size, *scale, theme)?);
        self.kept.insert(name.to_string(), Arc::clone(&halo));
        Some(halo)
    }
}

/// The desktop's icons: the listing, where each stands, what the pointer and
/// keyboard are doing to them, and the pure double-click engine it shares with
/// the file manager.
///
/// `S` is the injected [`DirectorySource`] — the live VFS listing under the
/// session's own identity in production, an in-memory tree in tests.
pub struct Desktop<S: DirectorySource> {
    source: S,
    /// Root-first components of the user's `Desktop` folder.
    folder: Vec<String>,
    /// The user's pinboard settings. The desktop is their single owner inside
    /// the session: the shell reads them from here rather than keeping a
    /// second copy that could drift.
    settings: DesktopSettings,
    /// The endings names are shown without: those the file manager's settings
    /// hide, so an icon reads as it does in a file-manager window.
    endings: HiddenEndings,
    /// Whether an item whose name marks it hidden has an icon, as the file
    /// manager's settings say of its windows.
    hidden_items: bool,
    entries: Vec<Entry>,
    /// The folder's entries the settings leave out, as last read and since
    /// merged, so their places are followed and forgotten as an icon's are.
    left_out: Vec<Entry>,
    /// Where each entry's icon stands, by listing index.
    spots: Vec<Spot>,
    /// The icons the user put somewhere, by listing index, the one put down
    /// first first.
    stack: Vec<usize>,
    /// The field the icons were last settled in, and where they stand there.
    flowed_in: Option<GridView>,
    standing: Standing,
    /// Whether an icon may be owed a cell: one arrived, lost its place, or a
    /// cell came free.
    flow_owed: bool,
    /// The halo each resting name was last drawn over, kept by the paint.
    halos: Cell<Halos>,
    /// Where the user put their icons, as the session's record last stood
    /// with the changes made here since.
    arrangement: Arrangement,
    record: Record,
    listed: Listed,
    /// The changes made here to where the icons stand, for the session to
    /// keep.
    edits: Vec<ArrangementEdit>,
    /// A refusal no gesture was there to state.
    refused: Option<GestureRefusal>,
    /// What a drag now over the desktop would leave there.
    offer: Option<LandingOffer>,
    /// The drops landing here and the saves made here, the oldest first.
    landings: Vec<Landing>,
    /// The icons drawn, while their own picture is produced, by the one their
    /// drag carried, the oldest first.
    stand_ins: Vec<(String, DragPicture)>,
    /// Where each save dropped on the bare desktop stands what it makes, by
    /// the open attempt making it, the oldest first.
    saves: Vec<((u64, u64), Places)>,
    carry: Option<Carry>,
    selection: Selection,
    /// The icon the keyboard ring sits on and the arrows move from.
    focus: Option<usize>,
    /// The band a press on the backdrop drags out, selecting what it touches.
    band: Option<Band>,
    /// The icon a press inside a several-icon selection collapses it to, if
    /// let go without travelling.
    collapse: Option<usize>,
    /// The folder icon a carried drag would drop into, while its application
    /// accepts it there.
    drop_target: Option<usize>,
    /// Bumped whenever the icons change place, so an index into them names
    /// the same icon only at the revision it was taken at.
    revision: u64,
    focused: bool,
    clicks: DoubleClickTracker,
    mounts: Mounts,
    /// Which drives the desktop draws for attached volumes.
    drives: DesktopDrives,
    /// The user the desktop acts for, whose own volumes it offers to eject
    /// and retitle.
    viewer: Option<u32>,
    administer: Administer,
    /// The arrangement of icons whose settling was last reported.
    settled_at: Option<u64>,
}

/// Stable event id: a revealed frame has carried the desktop's icons, every
/// one drawn as it will stay — once for each arrangement of them.
///
/// The pinboard's sibling of [`APP_BAR_SETTLED`](crate::APP_BAR_SETTLED), for
/// the same reason: an icon's picture is read and decoded off the serve loop,
/// so a new icon is drawn as its built-in glyph a frame or more before its own
/// picture lands, and a reader comparing a later frame's pixels with the
/// desktop's has no honest earlier record to key on.
pub const PINBOARD_SETTLED: tairix_log::EventId = tairix_log::EventId(20_043);

/// The exact message [`PINBOARD_SETTLED`] is emitted with.
pub const PINBOARD_SETTLED_MESSAGE: &str = "desktop icons drawn as they will stay";

impl<S: DirectorySource> Desktop<S> {
    /// A desktop over `source` showing the folder named by the root-first
    /// `folder` components, on the default pinboard settings. Nothing is
    /// listed until [`relist`](Self::relist), and no icon has a place of its
    /// own until the record of where they were put arrives
    /// ([`adopt_arrangement`](Self::adopt_arrangement)).
    ///
    /// The settings an absent store document implies are the defaults, so a
    /// desktop is fully specified before the embedder has read anything; the
    /// user's own document arrives through
    /// [`apply_settings`](Self::apply_settings), and the endings the file
    /// manager's settings hide through [`hide_endings`](Self::hide_endings).
    #[must_use]
    pub fn new(source: S, folder: Vec<String>) -> Self {
        Self {
            source,
            folder,
            settings: DesktopSettings::default(),
            endings: FilerPreferences::default().presentation.endings,
            hidden_items: FilerPreferences::default().presentation.hidden_items,
            entries: Vec::new(),
            left_out: Vec::new(),
            spots: Vec::new(),
            stack: Vec::new(),
            flowed_in: None,
            standing: Standing::default(),
            flow_owed: false,
            halos: Cell::default(),
            arrangement: Arrangement::default(),
            record: Record::Unread,
            listed: Listed::default(),
            edits: Vec::new(),
            refused: None,
            offer: None,
            landings: Vec::new(),
            stand_ins: Vec::new(),
            saves: Vec::new(),
            carry: None,
            selection: Selection::new(),
            focus: None,
            band: None,
            collapse: None,
            drop_target: None,
            revision: 0,
            focused: false,
            clicks: DoubleClickTracker::new(),
            mounts: Mounts::default(),
            drives: FilerPreferences::default().desktop,
            viewer: None,
            administer: Administer::Idle,
            settled_at: None,
        }
    }

    /// This desktop acting for the user `uid`: the one whose own volumes it
    /// offers to eject and retitle.
    #[must_use]
    pub fn acting_for(mut self, uid: u32) -> Self {
        self.viewer = Some(uid);
        self
    }

    /// The folder's entries, in the shared listing order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The pinboard settings in force.
    #[must_use]
    pub const fn settings(&self) -> &DesktopSettings {
        &self.settings
    }

    /// The absolute path of the folder the desktop shows.
    #[must_use]
    pub fn folder_path(&self) -> String {
        tairix_browse::vfs::spell_absolute_path(&self.folder)
    }

    /// Root-first components of the folder the desktop shows.
    #[must_use]
    pub fn folder(&self) -> &[String] {
        &self.folder
    }

    /// What creating a shortcut to the catalogued program `entry` asks for.
    ///
    /// The program library's row menu is the one caller: a shortcut is a
    /// symbolic link in *this* folder, so the desktop — which owns the folder
    /// and its naming — decides the name and spells the link, and the embedder
    /// only makes it.
    ///
    /// The link takes the entry's **display name** and points at the bundle
    /// *directory* it launches, spelled from the root it lies in: the desktop
    /// is on another volume than the program stores, and a link never crosses
    /// into a volume beneath its own, so it names that root and the desktop
    /// reaches the bundle itself. That is what makes the shortcut read as an
    /// application on the desktop: bundle-ness is decided from the target's own
    /// leaf name, never from the link's, so `Chess` → `System:/Applications/chess.app`
    /// is an application while the link itself is just a name. The target is
    /// never resolved here, so a shortcut whose bundle is later removed
    /// dangles honestly rather than being prevented at creation.
    ///
    /// A display name is not automatically a file name: it must be one legal
    /// filesystem component under the same shared
    /// [`tairix_path::validate_file_name`] rule a typed folder or rename name
    /// obeys, so the desktop and the file manager can never disagree about what
    /// a name may be. A name that rule refuses — one carrying a `/` or a `:`,
    /// say — is [`DesktopAction::Refuse`]d with the rule's own reason rather
    /// than spelled into a path the create could only fail on.
    ///
    /// An existing name is **not** worked around: the link replaces nothing, so
    /// a name already taken is the kernel's own `AlreadyExists` at create time
    /// and is reported as the refusal it is. Picking a free name instead would
    /// silently make a *second*, differently-named shortcut for a user who
    /// already has one, and could only be decided against a listing that may
    /// already be stale.
    #[must_use]
    pub fn shortcut_to(&self, catalog: &Catalog, entry: &EntryId) -> DesktopAction {
        let Ok(chosen) = catalogued(catalog, entry) else {
            return DesktopAction::Refuse(GestureRefusal::new(
                "That program is no longer in the program library",
                format!("Library entry {entry} was removed or replaced since the menu opened."),
            ));
        };
        let name = chosen.name().as_str();
        if let Err(err) = tairix_path::validate_file_name(name) {
            return DesktopAction::Refuse(GestureRefusal::new(
                format!(
                    "\u{201c}{}\u{201d} cannot name a shortcut",
                    tairix_util::utf8::shown(name)
                ),
                format!("A shortcut is a file, and {err}."),
            ));
        }
        let bundle = chosen.bundle().to_string();
        DesktopAction::CreateShortcut {
            link: self.path_of(name),
            target: tairix_path::alias_spelling(&bundle).unwrap_or(bundle),
        }
    }

    /// The mount table moved: a read of it is owed.
    pub fn mounts_moved(&mut self) {
        self.mounts.owed = true;
    }

    /// Whether a read of the mount table is to be asked for now — one is owed
    /// and none is under way — which it is from here on.
    pub fn take_mount_read(&mut self) -> bool {
        let due = self.mounts.owed && !self.mounts.reading;
        if due {
            self.mounts.owed = false;
            self.mounts.reading = true;
        }
        due
    }

    /// A read asked for was refused before it began: it is owed again.
    pub fn mount_read_refused(&mut self) {
        self.mounts.reading = false;
        self.mounts.owed = true;
    }

    /// Adopt what a read of the mount table found — `None` for one that
    /// failed, which keeps the volumes last listed — adding to `damage` each
    /// icon it changed: a shortcut whose volume came or went, a drive shown or
    /// gone, and the icons those moved. Answers whether any did.
    pub fn adopt_mounts(
        &mut self,
        volumes: Option<Vec<Volume>>,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> bool {
        self.mounts.reading = false;
        match volumes {
            Some(volumes) if volumes != self.mounts.volumes => {
                self.mounts.volumes = volumes;
                self.marking(layout, damage, Self::follow_volumes)
            }
            _ => false,
        }
    }

    /// Draw drives for the attached volumes of the classes `drives` names,
    /// answering whether that changed the icons. A drive drawn or taken away
    /// flows again every icon no one placed, so the caller repaints the layer
    /// whole.
    pub fn show_drives(&mut self, drives: DesktopDrives) -> bool {
        if self.drives == drives {
            return false;
        }
        self.drives = drives;
        self.follow_volumes()
    }

    /// The backdrop menu a secondary press opens: over the icon
    /// [`context_press`](Self::context_press) selected when `on_icon`, which
    /// for a drive offers its eject and its title and refuses moving it to the
    /// Trash, and over the bare backdrop otherwise.
    #[must_use]
    pub fn menu(&self, on_icon: bool) -> ChainModel {
        let chosen = self.selected().and_then(|index| self.entries.get(index));
        let over = match chosen {
            None if on_icon && self.selection.len() > 1 => Over::Several,
            Some(entry) if on_icon && is_drive(entry) => Over::Drive {
                eject_refused: self.eject_of(entry).err(),
                title: self.title_of(entry),
            },
            Some(_) if on_icon => Over::Icon,
            _ => Over::Backdrop,
        };
        crate::pinboard::model(over, &self.settings)
    }

    /// Hold `refusal`, of something carried out for the desktop off its loop,
    /// for the session to state as it states the desktop's own.
    pub fn refuse(&mut self, refusal: GestureRefusal) {
        self.refused = Some(refusal);
    }

    /// Adopt `settings`, reporting the work the edit implies.
    ///
    /// `None` means `settings` were already in force: nothing changed and there
    /// is nothing to do. Otherwise the returned [`PinboardChange`] names the
    /// work the edit asks for, the layer's own repaint included, so the caller
    /// repaints, re-lays out or re-prepares the wallpaper only when the edit
    /// actually asks for it. A new order or arrangement flows again every icon
    /// the user did not put somewhere. The desktop applies nothing beyond its
    /// own state.
    pub fn apply_settings(&mut self, settings: DesktopSettings) -> Option<PinboardChange> {
        if settings == self.settings {
            return None;
        }
        let relayout = settings.icons != self.settings.icons;
        let resorted = settings.sort != self.settings.sort;
        let restyled = settings.appearance != self.settings.appearance
            || settings.contrast != self.settings.contrast
            || settings.density != self.settings.density
            || settings.text_family != self.settings.text_family
            || settings.text_size != self.settings.text_size;
        let appearance = AppearanceWork {
            theme: restyled || settings.motion != self.settings.motion,
            scale: settings.scale != self.settings.scale,
            cursor: settings.cursor_set != self.settings.cursor_set
                || settings.cursor_size != self.settings.cursor_size
                || settings.cursor_shadow != self.settings.cursor_shadow,
        };
        let change = PinboardChange {
            layer: relayout
                || resorted
                || restyled
                || appearance.scale
                || settings.backdrop != self.settings.backdrop,
            backdrop: BackdropWork {
                relayout,
                wallpaper: settings.wallpaper != self.settings.wallpaper
                    || settings.fit != self.settings.fit,
            },
            appearance,
            notifications: settings.notifications != self.settings.notifications,
            clock: settings.region != self.settings.region
                || settings.time_zone != self.settings.time_zone,
        };
        self.settings = settings;
        if resorted {
            self.resort();
        }
        if relayout || resorted {
            self.reflow(relayout);
        }
        Some(change)
    }

    /// Name the icons less `endings`, the endings the file manager's settings
    /// hide, answering whether that changed them. Any name may then read
    /// differently, so the caller repaints the layer whole, and the icons no
    /// one placed flow again in the order the names shown give them.
    pub fn hide_endings(&mut self, endings: HiddenEndings) -> bool {
        if self.endings == endings {
            return false;
        }
        self.endings = endings;
        self.halos.set(Halos::default());
        self.resort();
        self.reflow(false);
        true
    }

    /// Order the listing as the settings in force say, the selection and a
    /// carried press staying on the icons they were on.
    fn resort(&mut self) {
        let held = self.held();
        let order = self.order(self.settings.sort);
        sort_entries(&mut self.entries, order);
        self.follow(held);
    }

    /// Give an item whose name marks it hidden an icon only where `shown`, as
    /// the file manager's settings say, answering whether that changed the
    /// icons. The folder is not read again: what is left out is set aside,
    /// and each item shown again stands where it was put.
    pub fn show_hidden_items(&mut self, shown: bool) -> bool {
        if self.hidden_items == shown {
            return false;
        }
        self.hidden_items = shown;
        let (mut entries, left_out): (Vec<Entry>, Vec<Entry>) = self
            .entries
            .iter()
            .chain(&self.left_out)
            .cloned()
            .partition(|entry| self.lists(entry.name()));
        if entries.len() == self.entries.len() {
            return false;
        }
        sort_entries(&mut entries, self.order(self.settings.sort));
        self.replace(entries, left_out, false);
        true
    }

    /// Whether an entry named `name` has an icon: every one but a hidden item
    /// the settings leave out.
    fn lists(&self, name: &str) -> bool {
        self.hidden_items || !tairix_path::is_hidden_name(name)
    }

    /// The order `sort` sets, comparing the names the desktop shows.
    const fn order(&self, sort: IconSort) -> Order {
        Order {
            mode: sort_mode(sort),
            endings: self.endings,
        }
    }

    /// The selected icon, when exactly one is: what the verbs that act on one
    /// icon act on.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        (self.selection.len() == 1)
            .then(|| self.selection.iter().next())
            .flatten()
    }

    /// The icons selected.
    #[must_use]
    pub const fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Whether the desktop holds the keyboard (no window is focused).
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Tell the desktop whether it holds the keyboard, adding what that
    /// changed to `damage`.
    ///
    /// Only the focused icon wears the Focus Ring, so gaining or losing the
    /// keyboard changes that one icon and nothing else — and with nothing
    /// focused it changes no pixel at all. This is the click that moves
    /// focus between the desktop and a window, which is exactly when the
    /// screen must *not* be repainted wholesale.
    pub fn set_focused(&mut self, focused: bool, layout: &GridView, damage: &mut Region) {
        self.settle(layout);
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        self.mark_icon(layout, self.focus, damage);
    }

    /// Ask `want` for every shown icon's picture at the side it is drawn at,
    /// drawing nothing: what tells the artwork producer which pictures are on
    /// screen after a pass that repainted only some icons.
    pub fn want_shown_artwork(
        &self,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        mut want: impl FnMut(IconRequest<'_>, u32),
    ) {
        self.visit_icons(
            layout,
            scale,
            theme,
            |_| true,
            |icon| {
                want(icon.request, icon.side);
            },
        );
    }

    /// Add to `damage` every shown icon whose picture the `landed` decodes
    /// moved, with `layout`, `scale` and `theme` the ones the icons are
    /// painted at.
    ///
    /// A decode can only change the picture inside the tiles that draw through
    /// it, never the backdrop or another icon, so a batch that pictures none of
    /// them damages nothing.
    pub fn mark_artwork(
        &self,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        landed: &Landed,
        damage: &mut Region,
    ) {
        if landed.is_empty() {
            return;
        }
        self.visit_icons(
            layout,
            scale,
            theme,
            |_| true,
            |icon| {
                if landed.resolves(icon.request, icon.side) {
                    damage.add(icon.bounds);
                }
            },
        );
    }

    /// Text drawn provisionally has landed: forget each name's halo, blurred
    /// from what was drawn then, and add to `damage` every shown icon — all
    /// the text the layer draws, since a name is drawn inside its own tile and
    /// nowhere else.
    pub fn text_landed(&self, layout: &GridView, damage: &mut Region) {
        self.halos.set(Halos::default());
        for (_, rect) in self.shown(layout).bottom_up() {
            damage.add(rect);
        }
    }

    /// Add where the icon at `index` stands under `layout` to `damage`.
    ///
    /// The one place an icon's footprint is spelled: a tile draws strictly
    /// inside its own rectangle, so repainting that rectangle is the whole of
    /// repainting the icon. An icon the desktop does not show damages nothing.
    fn mark_icon(&self, layout: &GridView, index: Option<usize>, damage: &mut Region) {
        if let Some(rect) = index.and_then(|index| self.shown(layout).rect(index)) {
            damage.add(rect);
        }
    }

    /// Ask for a fresh listing of the folder now: the caller knows the folder
    /// must be read whole (bring-up, a rescan its watch asked for, or an
    /// action the session itself performed on it). Returns whether the shown
    /// set changed.
    ///
    /// A listing the source refuses leaves the desktop empty and selects
    /// nothing rather than showing a stale or guessed folder, and forgets no
    /// place the user put an icon in.
    ///
    /// A source that reads the folder elsewhere answers "not yet", and this
    /// changes nothing at all — the icons already on screen stay there, and the
    /// caller hands the answer over with [`resume`](Self::resume) on the wake
    /// that says the read finished. Blanking the icons while a read is in
    /// flight would make every re-list flicker.
    pub fn relist(&mut self) -> bool {
        let listed = self.source.refresh(&self.folder);
        self.adopt_listing(listed)
    }

    /// Adopt the listing a source that reads elsewhere owes, if one is owed.
    /// Returns whether the shown set changed.
    ///
    /// What the embedder calls on a wake that may mean the read finished. It
    /// never asks for a listing of its own: a wake shared with other work says
    /// nothing about the folder, and looking again on one would cost a
    /// directory read and a second wake per unrelated completion.
    pub fn resume(&mut self) -> bool {
        if !self.listed.owed {
            return false;
        }
        let listed = self.source.list(&self.folder);
        self.adopt_listing(listed)
    }

    /// Show what the source answered, noting whether it still owes an answer.
    ///
    /// An icon still listed stands where it stood; one that arrived takes its
    /// place as [`arrived`](Self::arrived) says.
    fn adopt_listing(&mut self, listed: Result<Listing, Errno>) -> bool {
        self.listed.owed = matches!(listed, Ok(Listing::Pending));
        let (mut entries, read) = match listed {
            Ok(Listing::Ready(entries)) => (entries, true),
            Ok(Listing::Pending) => return false,
            Err(_) => (Vec::new(), false),
        };
        self.listed.read = read;
        let left_out: Vec<Entry> = entries
            .extract_if(.., |entry| !self.lists(entry.name()))
            .collect();
        self.with_volumes(&mut entries);
        sort_entries(&mut entries, self.order(self.settings.sort));
        let unchanged = entries.len() == self.entries.len()
            && entries
                .iter()
                .zip(&self.entries)
                .all(|(a, b)| a.same_listing(b) && a.volume() == b.volume());
        if unchanged {
            self.set_aside(left_out, read);
        } else {
            self.replace(entries, left_out, read);
        }
        self.reconcile();
        if read {
            self.settle_landings();
        }
        !unchanged
    }

    /// Take `left_out` as the folder's entries set aside where no icon
    /// changed, following the places of those that went.
    fn set_aside(&mut self, left_out: Vec<Entry>, forget: bool) {
        let mut was: BTreeMap<&str, FileId> = self
            .left_out
            .iter()
            .map(|entry| (entry.name(), entry.id()))
            .collect();
        let mut arrivals = Vec::new();
        for entry in &left_out {
            if was.remove(entry.name()).is_none() {
                arrivals.push((entry.name().to_string(), entry.id()));
            }
        }
        let leaving: Vec<Leaving> = was
            .into_iter()
            .map(|(name, id)| self.leaving_aside(name, id))
            .collect();
        self.left_out = left_out;
        self.follow_leaving(&arrivals, leaving, forget, Vec::new(), Vec::new());
    }

    /// The entry named `name`, set aside, leaving: where the record keeps it,
    /// if anywhere.
    fn leaving_aside(&self, name: &str, id: FileId) -> Leaving {
        Leaving {
            name: name.to_string(),
            id,
            spot: self
                .arrangement
                .place_of(name)
                .map_or(Spot::Owed, |placed| Spot::Placed(placed.at)),
        }
    }

    /// Show `entries` in place of the icons shown, with `left_out` set aside:
    /// one still listed stands where it stood, one set aside before keeps the
    /// place the record holds for it, and one that arrived takes its place as
    /// [`arrived`](Self::arrived) says, the place of one gone forgotten where
    /// `forget` says its folder was read.
    fn replace(&mut self, entries: Vec<Entry>, left_out: Vec<Entry>, forget: bool) {
        let held = self.held();
        let was = core::mem::replace(&mut self.entries, entries);
        let was_aside = core::mem::replace(&mut self.left_out, left_out);
        let mut stood: BTreeMap<&str, (FileId, Spot)> = was
            .iter()
            .zip(&self.spots)
            .map(|(entry, spot)| (entry.name(), (entry.id(), *spot)))
            .collect();
        let mut aside: BTreeMap<&str, FileId> = was_aside
            .iter()
            .map(|entry| (entry.name(), entry.id()))
            .collect();
        let mut spots = Vec::with_capacity(self.entries.len());
        let mut arrivals = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if let Some((_, spot)) = stood.remove(entry.name()) {
                spots.push(spot);
            } else {
                spots.push(Spot::Owed);
                if aside.remove(entry.name()).is_none() {
                    arrivals.push(index);
                }
            }
        }
        let mut set_aside = Vec::new();
        for entry in &self.left_out {
            if stood.remove(entry.name()).is_none() && aside.remove(entry.name()).is_none() {
                set_aside.push((entry.name().to_string(), entry.id()));
            }
        }
        let mut leaving: Vec<Leaving> = stood
            .into_iter()
            .map(|(name, (id, spot))| Leaving {
                name: name.to_string(),
                id,
                spot,
            })
            .collect();
        leaving.extend(
            aside
                .into_iter()
                .map(|(name, id)| self.leaving_aside(name, id)),
        );
        self.spots = spots;
        self.revision = self.revision.wrapping_add(1);
        self.follow(held);
        self.arrived(&arrivals, &set_aside, leaving, forget);
    }

    /// Mark each shortcut to a volume among `entries`, the folder's own, as
    /// the drive its volume is on while the mount table shows it attached, and
    /// add the drives the desktop draws for the attached volumes.
    fn with_volumes(&self, entries: &mut Vec<Entry>) {
        for entry in entries
            .iter_mut()
            .filter(|entry| entry.kind() == EntryKind::Link(LinkTarget::Volume))
        {
            let mark = entry
                .target()
                .and_then(|target| volume::locate(target, &self.mounts.volumes).present())
                .map(volume::drive_mark);
            entry.set_drive(mark.as_ref());
        }
        let drives = self.drives_for(entries);
        entries.extend(drives);
    }

    /// The drives to draw for the attached volumes of the classes shown: one
    /// for each identity, the attachment a shortcut to it would open, and none
    /// for a volume a shortcut among `entries` already stands for.
    fn drives_for(&self, entries: &[Entry]) -> Vec<Entry> {
        let mut drawn: BTreeSet<[u8; VOLUME_ID_LEN]> = entries
            .iter()
            .filter(|entry| entry.kind() == EntryKind::Link(LinkTarget::Volume))
            .filter_map(|entry| entry.target().and_then(volume::reference))
            .collect();
        let volumes = &self.mounts.volumes;
        let mut drives = Vec::new();
        for shown in volumes
            .iter()
            .filter(|shown| volume::shown_on_desktop(shown, self.drives))
        {
            let root = tairix_browse::mount::volume_root(shown.facts.identity);
            let opens = volume::locate(&root, volumes).present() == Some(shown);
            if opens && drawn.insert(shown.facts.identity) {
                drives.extend(volume::drive_entry(shown));
            }
        }
        drives
    }

    /// Follow the volumes the mount table shows and the drives the settings
    /// draw, answering whether any icon changed. A drive that went keeps its
    /// place for when its volume returns.
    fn follow_volumes(&mut self) -> bool {
        let mut entries: Vec<Entry> = self
            .entries
            .iter()
            .filter(|entry| !is_drive(entry))
            .cloned()
            .collect();
        self.with_volumes(&mut entries);
        sort_entries(&mut entries, self.order(self.settings.sort));
        if entries == self.entries {
            return false;
        }
        let left_out = self.left_out.clone();
        self.replace(entries, left_out, false);
        true
    }

    /// The names of the icons the selection, the focus and a carried press
    /// are on, to find them again once the listing is reordered or replaced.
    fn held(&self) -> Held {
        let name = |index: usize| {
            self.entries
                .get(index)
                .map(|entry| entry.name().to_string())
        };
        let mut selected = Vec::new();
        // Without the room to name them the selection is let go rather than
        // aborting the session.
        if selected.try_reserve_exact(self.selection.len()).is_ok() {
            selected.extend(self.selection.iter().filter_map(name));
            selected.sort_unstable();
        }
        Held {
            selected,
            focus: self.focus.and_then(name),
            carried: self
                .carry
                .and_then(|carry| Some((name(carry.index)?, carry))),
        }
    }

    /// Put the selection, the focus and a carried press back on the icons
    /// `held` names, so none lands on a different icon under the user's
    /// pointer. The lit drop folder and a half-made double-click go with the
    /// order, since what they were taken against has moved.
    fn follow(&mut self, held: Held) {
        let find = |name: &str| self.entries.iter().position(|entry| entry.name() == name);
        self.selection.reset(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            let named = |name: &String| name.as_str().cmp(entry.name());
            if held.selected.binary_search_by(named).is_ok() {
                self.selection.insert(index);
            }
        }
        self.focus = held.focus.and_then(|name| find(&name));
        self.carry = held.carried.and_then(|(name, carry)| {
            Some(Carry {
                index: find(&name)?,
                ..carry
            })
        });
        self.collapse = None;
        self.drop_target = None;
        self.clicks.reset();
    }

    /// Flow every icon the user did not put somewhere again, in the order and
    /// from the edge the settings now name; `moved` says the field moved with
    /// them, so they settle once it is next laid out.
    fn reflow(&mut self, moved: bool) {
        self.spots = alloc::vec![Spot::Owed; self.entries.len()];
        if moved {
            self.flowed_in = None;
        }
        self.resolve_places();
    }

    /// Merge the entries the folder's watch reported into the listing in
    /// place, adding to `damage` each icon whose place or look changed. The
    /// selection stays on the icon it named and goes with one whose file went,
    /// a file renamed keeps where it stood, and a merge the memory could not
    /// be had for re-lists the folder instead. Answers whether any icon moved.
    ///
    /// `layout` lays a desktop out as [`layout`](Self::layout) does, so the
    /// icons are measured on both sides of the merge.
    pub fn apply_changes(
        &mut self,
        changes: Vec<EntryChange>,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> bool {
        self.marking(layout, damage, |desktop| desktop.merge(changes))
    }

    /// Merge `changes` into the listing, each icon keeping where it stood and
    /// each arrival given its place, answering whether anything moved.
    fn merge(&mut self, changes: Vec<EntryChange>) -> bool {
        let (changes, aside): (Vec<EntryChange>, Vec<EntryChange>) = changes
            .into_iter()
            .partition(|change| self.lists(changed_name(change)));
        let mut leaving = self.leaving(&changes);
        let order = self.order(self.settings.sort);
        let merged = merge_changes(&mut self.entries, changes, order);
        // Without the memory to merge them, the folder is read again.
        let Some((placement, moved)) = merged else {
            return self.relist();
        };
        let (set_aside, went_aside) = self.merge_aside(aside);
        leaving.extend(went_aside);
        if !moved {
            self.follow_leaving(&set_aside, leaving, true, Vec::new(), Vec::new());
            return false;
        }
        let mut spots = alloc::vec![Spot::Owed; self.entries.len()];
        let mut kept = alloc::vec![false; self.entries.len()];
        for (before, spot) in self.spots.iter().enumerate() {
            let Some(after) = placement.place(before) else {
                continue;
            };
            if let (Some(slot), Some(seen)) = (spots.get_mut(after), kept.get_mut(after)) {
                *slot = *spot;
                *seen = true;
            }
        }
        let arrivals: Vec<usize> = (0..kept.len())
            .filter(|&index| kept.get(index) == Some(&false))
            .collect();
        self.spots = spots;
        self.revision = self.revision.wrapping_add(1);
        let count = self.entries.len();
        self.selection.remap(count, |index| placement.place(index));
        self.focus = self.focus.and_then(|index| placement.place(index));
        self.collapse = None;
        self.drop_target = self.drop_target.and_then(|index| placement.place(index));
        self.carry = self.carry.and_then(|carry| {
            Some(Carry {
                index: placement.place(carry.index)?,
                ..carry
            })
        });
        self.clicks.follow(|at| placement.place_subject(at));
        self.arrived(&arrivals, &set_aside, leaving, true);
        true
    }

    /// Make `changes`, each to an entry the settings leave out, to those set
    /// aside: answers the names newly set aside and what went from among them,
    /// the latest change to a name its word.
    fn merge_aside(&mut self, changes: Vec<EntryChange>) -> (Vec<(String, FileId)>, Vec<Leaving>) {
        let mut arrivals: Vec<(String, FileId)> = Vec::new();
        let mut leaving = Vec::new();
        for change in changes {
            let held = self
                .left_out
                .iter()
                .position(|entry| entry.name() == changed_name(&change));
            match (change, held) {
                (EntryChange::Upsert(entry), Some(at)) => {
                    if let Some(slot) = self.left_out.get_mut(at) {
                        *slot = entry;
                    }
                }
                (EntryChange::Upsert(entry), None) => {
                    let name = entry.name().to_string();
                    match leaving.iter().position(|gone: &Leaving| gone.name == name) {
                        Some(back) => drop(leaving.swap_remove(back)),
                        None => arrivals.push((name, entry.id())),
                    }
                    self.left_out.push(entry);
                }
                (EntryChange::Remove(name), Some(at)) => {
                    let gone = self.left_out.swap_remove(at);
                    match arrivals.iter().position(|(arrived, _)| *arrived == name) {
                        Some(new) => drop(arrivals.swap_remove(new)),
                        None => leaving.push(self.leaving_aside(&name, gone.id())),
                    }
                }
                (EntryChange::Remove(_), None) => {}
            }
        }
        (arrivals, leaving)
    }

    /// The icons whose files the latest word of `changes` takes out of the
    /// listing, with where each stood.
    fn leaving(&self, changes: &[EntryChange]) -> Vec<Leaving> {
        let mut gone: BTreeMap<&str, bool> = BTreeMap::new();
        for change in changes {
            match change {
                EntryChange::Remove(name) => gone.insert(name.as_str(), true),
                EntryChange::Upsert(entry) => gone.insert(entry.name(), false),
            };
        }
        if !gone.values().any(|&going| going) {
            return Vec::new();
        }
        self.entries
            .iter()
            .zip(&self.spots)
            .filter(|(entry, _)| gone.get(entry.name()) == Some(&true))
            .map(|(entry, spot)| Leaving {
                name: entry.name().to_string(),
                id: entry.id(),
                spot: *spot,
            })
            .collect()
    }

    /// Give each icon at `arrivals` its place: a file renamed keeps where it
    /// stood, one a drop or a save is waiting for stands where it was let go,
    /// and one the record keeps a place for stands there. A file renamed to
    /// one of `set_aside`, the names newly set aside, keeps its place under
    /// that name, and the place of a file gone is forgotten where `forget`
    /// says the folder was read.
    fn arrived(
        &mut self,
        arrivals: &[usize],
        set_aside: &[(String, FileId)],
        mut leaving: Vec<Leaving>,
        forget: bool,
    ) {
        let mut edits = Vec::new();
        let mut set = Vec::new();
        let mut order = self.arrangement.next_order();
        let mut taken = None;
        for &index in arrivals {
            let Some(entry) = self.entries.get(index) else {
                continue;
            };
            let (name, id) = (entry.name().to_string(), entry.id());
            let renamed = if id.is_none() {
                None
            } else {
                leaving.iter().position(|gone| gone.id == id)
            };
            if let Some(at) = renamed {
                let gone = leaving.swap_remove(at);
                if let Spot::Placed(_) = gone.spot {
                    let from = PlaceKey::of(&gone.name);
                    edits.push(ArrangementEdit::Renamed { from, to: name, id });
                } else if let Some(slot) = self.spots.get_mut(index) {
                    *slot = gone.spot;
                }
            } else if let Some(at) = self.landing_place(&name, &mut taken) {
                set.push((name, Some(Placed { id, at, order })));
                order = order.saturating_add(1);
            }
        }
        self.stand_ins
            .retain(|(name, _)| !leaving.iter().any(|gone| gone.name == *name));
        self.follow_leaving(set_aside, leaving, forget, edits, set);
        self.resolve_places();
        self.retire_landings();
    }

    /// Keep `edits` and the places `set` with the places of `leaving`
    /// followed: one renamed to a name of `set_aside` keeps its place under
    /// that name, and the rest are forgotten where `forget` says the folder
    /// was read.
    fn follow_leaving(
        &mut self,
        set_aside: &[(String, FileId)],
        mut leaving: Vec<Leaving>,
        forget: bool,
        mut edits: Vec<ArrangementEdit>,
        mut set: Vec<(String, Option<Placed>)>,
    ) {
        for (name, id) in set_aside.iter().filter(|(_, id)| !id.is_none()) {
            let Some(at) = leaving.iter().position(|gone| gone.id == *id) else {
                continue;
            };
            let gone = leaving.swap_remove(at);
            if let Spot::Placed(_) = gone.spot {
                let from = PlaceKey::of(&gone.name);
                edits.push(ArrangementEdit::Renamed {
                    from,
                    to: name.clone(),
                    id: *id,
                });
            }
        }
        if forget {
            for gone in leaving {
                if let Spot::Placed(_) = gone.spot {
                    set.push((gone.name, None));
                }
            }
        }
        if !set.is_empty() {
            edits.push(ArrangementEdit::Set(set));
        }
        self.keep(edits);
    }

    /// Make each of `edits` here, holding a refusal for the session to state,
    /// since no gesture of the user's is there to state it.
    fn keep(&mut self, edits: Vec<ArrangementEdit>) {
        for edit in edits {
            if self.record(edit).is_err() {
                self.refused = Some(full_refusal());
            }
        }
    }

    /// Where a drop stands the file named `name` that just arrived: the item
    /// it is writing under that name, or one it made that was still to be
    /// listed. A file no drop named is none of its own.
    fn landing_place(&mut self, name: &str, taken: &mut Option<Vec<bool>>) -> Option<Place> {
        let (at, item) = self
            .landings
            .iter_mut()
            .enumerate()
            .filter(|(_, landing)| landing.places.is_some())
            .find_map(|(at, landing)| {
                if let Some(making) = landing.making.as_mut().filter(|making| making.name == name) {
                    making.stands = true;
                    return Some((at, making.item));
                }
                let owed = landing.owed.iter().position(|(owed, _)| owed == name)?;
                Some((at, landing.owed.swap_remove(owed).1))
            })?;
        self.item_place(at, item, taken)
    }

    /// Where item `item` of the landing at `at` stands: an item shown where it
    /// was carried, any other on the free cell nearest where the drop was let
    /// go — claimed from `taken`, the cells of the field, built at the first
    /// claim — and kept for it.
    fn item_place(&mut self, at: usize, item: u32, taken: &mut Option<Vec<bool>>) -> Option<Place> {
        let grid = self.flowed_in?;
        let from = self.settings.icons;
        let places = self.landings.get(at)?.places.as_ref()?;
        let shown = usize::try_from(item)
            .ok()
            .and_then(|item| places.shown.get(item));
        let kept = places
            .further
            .iter()
            .find(|(further, _)| *further == item)
            .map(|(_, place)| place);
        if let Some(place) = shown.or(kept) {
            return Some(*place);
        }
        let field = Field::of(&grid)?;
        if taken.is_none() {
            *taken = field.taken(&self.standing, |_| false);
        }
        let dropped = field.wanted(places.dropped).origin;
        let corner = field.claim_nearest(dropped, taken.as_deref_mut()?)?;
        let place = field.place_of(corner, from);
        self.landings
            .get_mut(at)?
            .places
            .as_mut()?
            .further
            .push((item, place));
        Some(place)
    }

    /// Stand item `item` of the landing at `at`, made as `name`, where the
    /// drop put it when it is listed — moving one that had the name before —
    /// and owe it the place until it arrives when not. Answers whether an icon
    /// moved.
    fn stand_made(&mut self, at: usize, item: u32, name: &str) -> bool {
        let Some(id) = self
            .entries
            .iter()
            .find(|entry| entry.name() == name)
            .map(Entry::id)
        else {
            if let Some(landing) = self.landings.get_mut(at) {
                landing.owed.push((name.to_string(), item));
            }
            return false;
        };
        let mut taken = None;
        let Some(place) = self.item_place(at, item, &mut taken) else {
            return false;
        };
        let placed = Placed {
            id,
            at: place,
            order: self.arrangement.next_order(),
        };
        self.keep(alloc::vec![ArrangementEdit::Set(alloc::vec![(
            name.to_string(),
            Some(placed)
        )])]);
        self.resolve_places();
        true
    }

    /// Forget the landings with nothing left to land: ended, and owing no
    /// name.
    fn retire_landings(&mut self) {
        self.landings
            .retain(|landing| !landing.ended || !landing.owed.is_empty());
    }

    /// The folder was read whole: a name a drop made that it did not list is
    /// not coming, so the place owed it goes, with every drop that ended.
    fn settle_landings(&mut self) {
        for landing in &mut self.landings {
            landing.owed.clear();
        }
        self.retire_landings();
    }

    /// Make `edit` to the record here, for the session to keep.
    fn record(&mut self, edit: ArrangementEdit) -> Result<(), ArrangementFull> {
        if !self.arrangement.make(&edit)?.is_empty() {
            self.edits.push(edit);
        }
        Ok(())
    }

    /// Stand each icon the record keeps a place for there, stacked in the
    /// order they were put down, owe one whose place went the next free cell,
    /// and settle them again in the field they were last settled in.
    fn resolve_places(&mut self) {
        let mut stack = Vec::new();
        for (index, (spot, entry)) in self.spots.iter_mut().zip(&self.entries).enumerate() {
            match self.arrangement.place_of(entry.name()) {
                Some(placed) => {
                    *spot = Spot::Placed(placed.at);
                    stack.push((placed.order, index));
                }
                None if matches!(spot, Spot::Placed(_)) => *spot = Spot::Owed,
                None => {}
            }
        }
        // Two put down in one turn keep one stacking whatever the sort.
        let name = |index: usize| self.entries.get(index).map(Entry::name);
        stack.sort_unstable_by_key(|&(order, index)| (order, name(index)));
        self.stack = stack.into_iter().map(|(_, index)| index).collect();
        self.flow_owed = true;
        self.revision = self.revision.wrapping_add(1);
        if let Some(layout) = self.flowed_in {
            self.settle(&layout);
        }
    }

    /// Once the record has been read and the folder listed, follow each place
    /// kept for a file no longer listed: one renamed while the desktop was not
    /// looking keeps its place, and one gone is forgotten.
    fn reconcile(&mut self) {
        if self.record != Record::Read || !self.listed.read {
            return;
        }
        self.record = Record::Reconciled;
        let folder = || self.entries.iter().chain(&self.left_out);
        let listed: BTreeSet<PlaceKey> = folder().map(|entry| PlaceKey::of(entry.name())).collect();
        let by_file: BTreeMap<FileId, &Entry> = folder()
            .filter(|entry| !entry.id().is_none())
            .map(|entry| (entry.id(), entry))
            .collect();
        let mut renamed = Vec::new();
        let mut gone = Vec::new();
        for (key, placed) in self.arrangement.places() {
            // A drive's place is its volume's, kept whether or not it is
            // attached: it names no file to follow or forget.
            if key.is_volume() || listed.contains(&key) {
                continue;
            }
            let moved = by_file
                .get(&placed.id)
                .filter(|entry| self.arrangement.place_of(entry.name()).is_none());
            match moved {
                Some(entry) => renamed.push(ArrangementEdit::Renamed {
                    from: key,
                    to: entry.name().to_string(),
                    id: placed.id,
                }),
                None => gone.push(key),
            }
        }
        if !gone.is_empty() {
            renamed.push(ArrangementEdit::Forget(gone));
        }
        self.keep(renamed);
        self.resolve_places();
    }

    /// [`relist`](Self::relist), adding to `damage` each icon whose place or
    /// look the new listing changed rather than answering for the whole layer.
    pub fn relist_into(&mut self, layout: impl Fn(&Self) -> GridView, damage: &mut Region) -> bool {
        self.marking(layout, damage, Self::relist)
    }

    /// [`resume`](Self::resume), adding to `damage` each icon whose place or
    /// look the landed listing changed.
    ///
    /// A wake that owes the desktop no listing costs it nothing, rather than a
    /// snapshot of every shown icon to compare against.
    pub fn resume_into(&mut self, layout: impl Fn(&Self) -> GridView, damage: &mut Region) -> bool {
        self.listed.owed && self.marking(layout, damage, Self::resume)
    }

    /// Latch every folder cue the shown icons now have an answer for, asking
    /// about each shown folder not yet known, and add to `damage` each icon
    /// whose picture moved.
    ///
    /// Only the icons `layout` shows are asked about, so the cost is bounded
    /// by the screen rather than the folder. A source that probes elsewhere
    /// answers "not yet" without reading, so the embedder resolves again when
    /// its batch lands.
    pub fn resolve_occupancy(&mut self, layout: &GridView, damage: &mut Region) {
        self.settle(layout);
        let mut rects: Vec<Option<Rect>> = alloc::vec![None; self.entries.len()];
        for (index, rect) in self.shown(layout).bottom_up() {
            if let Some(slot) = rects.get_mut(index) {
                *slot = Some(rect);
            }
        }
        // Asked a run of shown icons at a time, so one leaf buffer serves each.
        let mut start = 0;
        for run in rects.chunk_by(|a, b| a.is_some() == b.is_some()) {
            let range = start..start + run.len();
            start = range.end;
            if run.first().is_some_and(Option::is_some) {
                resolve_occupancy(
                    &mut self.source,
                    &mut self.folder,
                    &mut self.entries,
                    range,
                    |moved| {
                        if let Some(&Some(rect)) = rects.get(moved) {
                            damage.add(rect);
                        }
                    },
                );
            }
        }
    }

    /// Whether the folder's listing follows it, so a change the session made
    /// there arrives as a reported change and needs no re-list.
    #[must_use]
    pub fn follows(&self) -> bool {
        self.source.follows(&self.folder)
    }

    /// Read the folder again where its listing does not follow it, adding to
    /// `damage` each icon whose place or look the fresh listing changed: what
    /// a change no watch reports needs. Answers whether any icon moved.
    pub fn relist_unfollowed(
        &mut self,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> bool {
        !self.follows() && self.relist_into(layout, damage)
    }

    /// Show a name the session made at absolute `path`: a folder it lies
    /// within is read again unless its listing follows it, as
    /// [`relist_unfollowed`](Self::relist_unfollowed) does. Answers whether
    /// any icon moved.
    pub fn show_made(
        &mut self,
        path: &str,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> bool {
        let within = tairix_browse::vfs::components_from_absolute_path(path)
            .is_ok_and(|made| made.len() > self.folder.len() && made.starts_with(&self.folder));
        within && self.relist_unfollowed(layout, damage)
    }

    /// Adopt `live` — where the user put their icons, as the store last
    /// answered with the changes it has yet to confirm — adding to `damage`
    /// each icon it moved. `read` says the store has been read, which is what
    /// lets a place kept for a file no longer listed be followed or forgotten.
    pub fn adopt_arrangement(
        &mut self,
        live: &Arrangement,
        read: bool,
        layout: &GridView,
        damage: &mut Region,
    ) {
        let first_read = read && self.record == Record::Unread;
        if *live == self.arrangement && !first_read {
            return;
        }
        self.marking(
            |_| *layout,
            damage,
            |desktop| {
                desktop.arrangement.clone_from(live);
                if first_read {
                    desktop.record = Record::Read;
                }
                desktop.resolve_places();
                desktop.reconcile();
                true
            },
        );
    }

    /// The changes made here to where the icons stand since this was last
    /// asked, for the session to keep.
    pub fn take_arrangement_edits(&mut self) -> Vec<ArrangementEdit> {
        core::mem::take(&mut self.edits)
    }

    /// Hand `memory` the changes made here, then adopt what it holds whenever
    /// that moved, adding to `damage` each icon it moved, with `layout` laying
    /// the desktop out as [`layout`](Self::layout) does. The changes go in
    /// first, so a store answer collected meanwhile cannot undo one, and one
    /// the record has no room for puts its icon back. Answers whether one had
    /// no room.
    pub fn keep_record(
        &mut self,
        memory: &mut ArrangementMemory,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> bool {
        let mut full = self.hand_edits(memory);
        if memory.take_moved() || full {
            let shown = layout(self);
            self.adopt_arrangement(memory.live(), memory.is_read(), &shown, damage);
            // What a first read let the desktop follow goes with them, and the
            // desktop already shows every edit it handed in.
            full |= self.hand_edits(memory);
            memory.take_moved();
            if full {
                self.adopt_arrangement(memory.live(), memory.is_read(), &shown, damage);
            }
        }
        full
    }

    /// Hand `memory` the changes made here, answering whether it had no room
    /// for one.
    fn hand_edits(&mut self, memory: &mut ArrangementMemory) -> bool {
        let mut full = false;
        for edit in self.take_arrangement_edits() {
            full |= memory.apply(edit).is_err();
        }
        full
    }

    /// What became of an eject or a retitle carried out for a drive: one the
    /// user was refused for want of an administrator's authority is owed to
    /// the prompt, as the file manager's is, and any other refusal held for
    /// the session to state.
    pub fn volume_done(&mut self, done: MountDone) {
        let listed = self
            .mounts
            .volumes
            .iter()
            .map(|volume| (&volume.facts, volume.label.as_str()));
        if done.for_an_administrator(listed).is_some() {
            if matches!(self.administer, Administer::Idle) {
                self.administer = Administer::Owed(done.act);
            } else {
                self.refuse_act(&done.act, ADMINISTRATOR_BUSY);
            }
            return;
        }
        if let Err(refusal) = done.shown() {
            self.refuse(GestureRefusal::new(refusal.headline, refusal.reason));
        }
    }

    /// The task an administrator is owed a question about, once, named from
    /// the mount table as it stands: the embedder asks its prompt for it now
    /// and says how that went. A drive gone meanwhile is asked about nothing.
    pub fn take_administrator_ask(&mut self) -> Option<SystemTask> {
        if !matches!(self.administer, Administer::Owed(_)) {
            return None;
        }
        let Administer::Owed(act) = core::mem::take(&mut self.administer) else {
            return None;
        };
        let refused = MountDone {
            act,
            answer: Err(Errno::PermissionDenied),
        };
        let listed = self
            .mounts
            .volumes
            .iter()
            .map(|volume| (&volume.facts, volume.label.as_str()));
        let task = refused.for_an_administrator(listed);
        self.administer = Administer::Asked(refused.act);
        if task.is_none() {
            self.administrator_unasked(false);
        }
        task
    }

    /// The prompt could not be asked: `busy` with another question, or not
    /// shown at all.
    pub fn administrator_unasked(&mut self, busy: bool) {
        if let Administer::Asked(act) = core::mem::take(&mut self.administer) {
            let reason = if busy {
                ADMINISTRATOR_BUSY
            } else {
                ADMINISTRATOR_UNASKED
            };
            self.refuse_act(&act, reason);
        }
    }

    /// The administrator's act ended with `outcome`: one that ran and failed
    /// is stated; one declined, or done, says nothing, the mount table showing
    /// what happened.
    pub fn administrator_answered(&mut self, outcome: AuthorisedOutcome) {
        let Administer::Asked(act) = core::mem::take(&mut self.administer) else {
            return;
        };
        if !matches!(
            outcome,
            AuthorisedOutcome::Ran { exit_code: 0 } | AuthorisedOutcome::Declined
        ) {
            self.refuse_act(&act, ADMINISTRATOR_FAILED);
        }
    }

    /// Hold `act`'s refusal for `reason`.
    fn refuse_act(&mut self, act: &MountAct, reason: &'static str) {
        let refusal = act.refused(reason);
        self.refuse(GestureRefusal::new(refusal.headline, refusal.reason));
    }

    /// A refusal no gesture was there to state, since this was last asked.
    pub fn take_refusal(&mut self) -> Option<GestureRefusal> {
        self.refused.take()
    }

    /// Note what a drag now over the desktop would leave there, or that it is
    /// over somewhere else.
    pub fn offer_landing(&mut self, offer: Option<LandingOffer>) {
        self.offer = offer;
    }

    /// Land what the offer standing describes as the drop `source` — the
    /// window its drag began in and the report it was dropped at — let go at
    /// `at`: on the bare desktop each item shown as far from `at` as its
    /// picture was held from the press, on the nearest free cells where the
    /// settings keep icons to the grid, and each further item on the free cell
    /// nearest `at`, as its source makes it
    /// ([`step_landing`](Self::step_landing)). Asked once the source has been
    /// told the drop landed here, so a drop refused or concluded elsewhere
    /// lands nothing.
    pub fn expect_drop(
        &mut self,
        source: (u64, u32),
        at: Point,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
    ) {
        let Some(offer) = self.offer.take() else {
            return;
        };
        self.settle(layout);
        let places = offer.shown.as_deref().and_then(|shown| {
            let offsets: Vec<(i32, i32)> = shown
                .iter()
                .map(|picture| picture.from.unwrap_or((0, 0)))
                .collect();
            self.drop_places(&offer.lead, &offsets, at, (layout, scale, theme))
        });
        let pictures = offer.shown.unwrap_or_default();
        // Drops that ended with names still to arrive are kept for them, at
        // most as many as one window lands at once.
        let settling = self.landings.iter().filter(|landing| landing.ended).count();
        if settling >= DROP_LANDINGS_MAX {
            if let Some(oldest) = self.landings.iter().position(|landing| landing.ended) {
                self.landings.remove(oldest);
            }
        }
        self.landings.push(Landing {
            source: Some(source),
            folder: offer.folder,
            places,
            pictures,
            making: None,
            owed: Vec::new(),
            ended: false,
        });
    }

    /// Hold where a save of the file asked for as `name` was let go on the
    /// bare desktop at `at`, for the open attempt `attempt` that makes it: the
    /// file it makes stands under the pointer ([`saved`](Self::saved)).
    pub fn expect_save(
        &mut self,
        attempt: (u64, u64),
        name: &str,
        at: Point,
        (layout, scale, theme): (&GridView, Scale, &Theme),
    ) {
        self.settle(layout);
        let Some(places) = self.drop_places(name, &[(0, 0)], at, (layout, scale, theme)) else {
            return;
        };
        // At most as many saves as one application may have picks open.
        if self.saves.len() >= PICK_MAX_OPEN {
            self.saves.remove(0);
        }
        self.saves.push((attempt, places));
    }

    /// The open `attempt` made its file at absolute `made`, or made none: a
    /// file made in this folder stands where its save was let go, adding to
    /// `damage` each icon that moved. Answers whether one did.
    pub fn saved(
        &mut self,
        attempt: (u64, u64),
        made: Option<&str>,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> bool {
        let Some(at) = self.saves.iter().position(|(held, _)| *held == attempt) else {
            return false;
        };
        let (_, places) = self.saves.remove(at);
        let folder = self.folder_path();
        let Some(name) = made
            .and_then(|made| made.strip_prefix(folder.as_str()))
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|name| !name.is_empty() && !name.contains('/'))
            .map(String::from)
        else {
            return false;
        };
        self.landings.push(Landing {
            source: None,
            folder,
            places: Some(places),
            pictures: Vec::new(),
            making: None,
            owed: Vec::new(),
            ended: true,
        });
        let at = self.landings.len() - 1;
        let moved = self.marking(layout, damage, |desktop| desktop.stand_made(at, 0, &name));
        self.retire_landings();
        moved
    }

    /// Take `step` of the drop `source` landing here, adding to `damage` each
    /// icon it moved, and answer what it asks of the embedder. An item made
    /// under a name stands where the drop put it — at once where its icon
    /// arrives while it is written, else once it was made or replaced what had
    /// the name, as soon as it is listed — and a skipped, merged or failed one
    /// moves nothing. The drop's end reads again a folder no watch follows.
    pub fn step_landing(
        &mut self,
        source: (u64, u32),
        step: &LandingStep,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> LandingWork {
        let Some(at) = self
            .landings
            .iter()
            .position(|landing| landing.source == Some(source))
        else {
            return LandingWork::default();
        };
        match *step {
            LandingStep::Making { item, name } => self.making(at, item, name.as_str()),
            LandingStep::Made { outcome, .. } => self.made(at, outcome, layout, damage),
            LandingStep::Ended => self.ended(at, layout, damage),
        }
    }

    /// Item `item` of the landing at `at` is about to be written as `name`:
    /// what is pictured from inside it is held until it is done, and its icon
    /// is drawn meanwhile by the picture its drag carried, where that is one
    /// of its own.
    fn making(&mut self, at: usize, item: u32, name: &str) -> LandingWork {
        let Some(landing) = self.landings.get_mut(at) else {
            return LandingWork::default();
        };
        landing.making = Some(Making {
            item,
            name: name.to_string(),
            stands: false,
        });
        let hold = Some(tairix_path::join(&landing.folder, name));
        let carried = usize::try_from(item)
            .ok()
            .and_then(|item| landing.pictures.get(item))
            .filter(|picture| picture.own.is_some())
            .copied();
        if let Some(picture) = carried.filter(|_| self.lists(name)) {
            self.stand_in(name, &picture);
        }
        LandingWork {
            hold,
            release: None,
        }
    }

    /// Draw the icon named `name` by `picture`, the one its drag carried,
    /// while its own is produced, forgetting the oldest past the bound.
    fn stand_in(&mut self, name: &str, picture: &DragPicture) {
        self.stand_ins.retain(|(held, _)| held.as_str() != name);
        if self.stand_ins.len() >= STAND_INS_MAX {
            self.stand_ins.remove(0);
        }
        self.stand_ins.push((name.to_string(), *picture));
    }

    /// The picture the drag that made the icon named `name` carried for it,
    /// which it is drawn by while its own is produced.
    fn stand_in_for(&self, name: &str) -> Option<&DragPicture> {
        self.stand_ins
            .iter()
            .find(|(held, _)| held.as_str() == name)
            .map(|(_, picture)| picture)
    }

    /// The item the landing at `at` was writing is done, as `outcome` says.
    fn made(
        &mut self,
        at: usize,
        outcome: LandingOutcome,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> LandingWork {
        let Some(landing) = self.landings.get_mut(at) else {
            return LandingWork::default();
        };
        let Some(made) = landing.making.take() else {
            return LandingWork::default();
        };
        let release = Some(tairix_path::join(&landing.folder, &made.name));
        let lands = matches!(outcome, LandingOutcome::Created | LandingOutcome::Replaced);
        if lands && landing.places.is_some() && !made.stands {
            self.marking(layout, damage, |desktop| {
                desktop.stand_made(at, made.item, &made.name)
            });
        }
        LandingWork {
            hold: None,
            release,
        }
    }

    /// The landing at `at` has ended: what it held is released, and a folder
    /// no watch follows is read for what it made.
    fn ended(
        &mut self,
        at: usize,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> LandingWork {
        let Some(landing) = self.landings.get_mut(at) else {
            return LandingWork::default();
        };
        let release = landing
            .making
            .take()
            .map(|made| tairix_path::join(&landing.folder, &made.name));
        landing.ended = true;
        self.retire_landings();
        self.relist_unfollowed(layout, damage);
        LandingWork {
            hold: None,
            release,
        }
    }

    /// Where a drop led by `lead` stands what it carries, let go at `at` in
    /// the field `layout` lays out: each item shown as far from `at` as its
    /// picture stood from the press by `offsets`, every offset held to the
    /// field and each tile to the nearest free cell where the settings keep
    /// icons to the grid; `None` for a field holding no cell.
    fn drop_places(
        &self,
        lead: &str,
        offsets: &[(i32, i32)],
        at: Point,
        (layout, scale, theme): (&GridView, Scale, &Theme),
    ) -> Option<Places> {
        let field = Field::of(layout)?;
        let tile = Rect::new(0, 0, field.cell.0, field.cell.1);
        let measured = Entry::file(lead.to_string());
        let held = entry_parts(Tiles::Large, (&measured, self.endings), tile, scale, theme)
            .and_then(|parts| parts.picture)
            .unwrap_or(tile)
            .center();
        let mut taken = if self.settings.align_to_grid {
            field.taken(&self.standing, |_| false)
        } else {
            None
        };
        // An application says where its pictures stood, and none stands
        // farther from the drop than the field reaches.
        let reach = (to_i32(field.area.width), to_i32(field.area.height));
        let bounded = |offset: i32, most: i32| offset.max(-most).min(most);
        let corner_of = |(dx, dy): (i32, i32)| {
            field.shown_at(Point::new(
                at.x.saturating_add(bounded(dx, reach.0))
                    .saturating_sub(held.x),
                at.y.saturating_add(bounded(dy, reach.1))
                    .saturating_sub(held.y),
            ))
        };
        let from = self.settings.icons;
        let shown = offsets
            .iter()
            .map(|&offset| {
                let corner = corner_of(offset);
                let corner = taken
                    .as_deref_mut()
                    .and_then(|taken| field.claim_nearest(corner, taken))
                    .unwrap_or(corner);
                field.place_of(corner, from)
            })
            .collect();
        Some(Places {
            shown,
            dropped: field.place_of(corner_of((0, 0)), from),
            further: Vec::new(),
        })
    }

    /// Stand the icons in `layout`'s field ([`Field::stand`]), each flowed
    /// icon keeping its cell where this is the field it was flowed in, and let
    /// go of the halo of every name the field no longer shows.
    fn settle(&mut self, layout: &GridView) {
        let same_field = self.flowed_in == Some(*layout);
        if same_field && !self.flow_owed {
            return;
        }
        let standing = match Field::of(layout) {
            Some(field) => field.stand(&self.spots, &self.stack, same_field),
            None => Some(Standing::default()),
        };
        if let Some(standing) = standing {
            for spot in &mut self.spots {
                if !matches!(spot, Spot::Placed(_)) {
                    *spot = Spot::Owed;
                }
            }
            for (cell, held) in standing.cells.iter().enumerate() {
                if let Some(spot) = held.and_then(|index| self.spots.get_mut(index)) {
                    *spot = Spot::Flowed(cell);
                }
            }
            self.standing = standing;
            self.flow_owed = false;
        } else {
            // Without the memory to stand them the icons are not shown until a
            // later settle finds it, and a cell of another field is none here.
            if !same_field {
                for spot in &mut self.spots {
                    if matches!(spot, Spot::Flowed(_)) {
                        *spot = Spot::Owed;
                    }
                }
            }
            self.standing = Standing::default();
            self.flow_owed = true;
        }
        let shown: BTreeSet<&str> = self
            .standing
            .cells
            .iter()
            .flatten()
            .chain(self.standing.placed.iter().map(|(index, _)| index))
            .filter_map(|&index| self.entries.get(index))
            .map(Entry::name)
            .collect();
        self.halos
            .get_mut()
            .kept
            .retain(|name, _| shown.contains(name.as_str()));
        self.flowed_in = Some(*layout);
        self.revision = self.revision.wrapping_add(1);
    }

    /// The icons as `layout` shows them: as they were settled there, or as
    /// they would stand in a field they have yet to be settled in.
    fn shown<'a>(&'a self, layout: &'a GridView) -> Shown<'a> {
        let standing = if self.flowed_in == Some(*layout) {
            Cow::Borrowed(&self.standing)
        } else {
            Cow::Owned(
                Field::of(layout)
                    .and_then(|field| field.stand(&self.spots, &self.stack, false))
                    .unwrap_or_default(),
            )
        };
        let lift = self.carry.and_then(|carry| {
            Some(Lift {
                lead: carry.index,
                with: self
                    .selection
                    .contains(carry.index)
                    .then_some(&self.selection),
                shift: carry.shift()?,
            })
        });
        Shown {
            standing,
            grid: layout,
            lift,
        }
    }

    /// Where the icon at `index` stands under `layout`, for a test that asks.
    #[cfg(test)]
    pub(crate) fn bounds_for_test(&self, layout: &GridView, index: usize) -> Option<Rect> {
        self.shown(layout).rect(index)
    }

    /// The halo kept for the name `name`, for a test that asks.
    #[cfg(test)]
    pub(crate) fn halo_for_test(&self, name: &str) -> Option<Arc<LabelHalo>> {
        let halos = self.halos.take();
        let kept = halos.kept.get(name).cloned();
        self.halos.set(halos);
        kept
    }

    /// Answer what `change` does to the desktop, adding to `damage` each icon
    /// shown before or after it whose place or look it changed: only the icons
    /// shown are compared, so the cost is bounded by the screen rather than by
    /// the folder.
    fn marking(
        &mut self,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
        change: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        let before = layout(self);
        self.settle(&before);
        // Found again by name, since a change may move every index.
        let mut was: Vec<(Rect, Entry, ControlState)> = self
            .shown(&before)
            .bottom_up()
            .filter_map(|(index, rect)| {
                let entry = self.entries.get(index)?;
                Some((rect, entry.clone(), self.icon_state(index)))
            })
            .collect();
        was.sort_unstable_by(|a, b| a.1.name().cmp(b.1.name()));
        if !change(self) {
            return false;
        }
        let after = layout(self);
        self.settle(&after);
        let mut seen = alloc::vec![false; was.len()];
        let shown = self.shown(&after);
        for (index, rect) in shown.bottom_up() {
            let Some(entry) = self.entries.get(index) else {
                continue;
            };
            let found = was
                .binary_search_by(|(_, held, _)| held.name().cmp(entry.name()))
                .ok()
                .and_then(|at| Some((at, was.get(at)?)));
            match found {
                Some((at, (from, held, state))) => {
                    if let Some(seen) = seen.get_mut(at) {
                        *seen = true;
                    }
                    if (*from, held, *state) != (rect, entry, self.icon_state(index)) {
                        damage.add(*from);
                        damage.add(rect);
                    }
                }
                None => damage.add(rect),
            }
        }
        for ((from, _, _), seen) in was.iter().zip(seen) {
            if !seen {
                damage.add(*from);
            }
        }
        true
    }

    /// The cells the desktop's icons stand in: every whole cell of the shared
    /// tile geometry the work area holds, inset by [`DESKTOP_MARGIN`] and
    /// numbered down from the edge the settings' arrangement names.
    ///
    /// `work_area` is the screen with the taskbar's band removed, so an icon
    /// can never be drawn under the bar or hit-tested through it.
    #[must_use]
    pub fn layout(&self, work_area: Rect, scale: Scale, theme: &Theme) -> GridView {
        let margin = scale.scale_length(DESKTOP_MARGIN);
        let viewport = Rect::new(
            work_area.origin.x.saturating_add_unsigned(margin),
            work_area.origin.y.saturating_add_unsigned(margin),
            work_area.width.saturating_sub(margin.saturating_mul(2)),
            work_area.height.saturating_sub(margin.saturating_mul(2)),
        );
        let metrics = Tiles::Large.metrics(scale, theme);
        // The field is fixed, not resizable: keeping the pitch anchored to the
        // edge the cells run from keeps a cell where the user last saw it
        // whatever the work area's exact extent is.
        let [down, across] = [
            (viewport.height, metrics.cell_height),
            (viewport.width, metrics.cell_width),
        ]
        .map(|(extent, cell)| {
            GridRun::new(extent, cell, metrics.gap, GridFill::FixedPitch).count()
        });
        GridView::new(
            viewport,
            metrics,
            0,
            down.saturating_mul(across),
            grid_flow(self.settings.icons),
            GridFill::FixedPitch,
        )
    }

    /// Paint the shown icons that fall inside `area` into `surface` through
    /// the shared icon tile, resolving each one's artwork from `artwork` at
    /// exactly the slot side the tile will draw it in.
    ///
    /// Only the icons the field shows are walked — the one on each cell and
    /// each the user put somewhere — and only their artwork is asked for, so a
    /// folder with more icons than fit costs no more than a screenful. `area`
    /// narrows that again to the rectangle being repainted, so moving a
    /// highlight costs the cells that changed rather than every icon on
    /// screen — a tile draws strictly inside its own cell, so a cell `area`
    /// misses has nothing in `area` to draw. An application bundle on the
    /// desktop names itself in its request, so it draws the icon it carries in
    /// its own `Resources/` rather than the generic bundle picture. An icon
    /// whose artwork the lookup declines falls back to the shared class glyph
    /// inside the tile, so a system with no `/System/Graphics` still shows a
    /// meaningful desktop.
    ///
    /// `ground` is the flat colour the icons stand on, or `None` over a
    /// picture: a resting name takes the ink that reads there, and a name on
    /// a state's plate keeps the theme's. A resting name's halo over a picture
    /// is blurred once and kept until the name shown, the scale, the theme or
    /// the tile size moves, so a repaint lays it down rather than blurring it.
    #[allow(clippy::too_many_arguments)] // The paint, the look it takes, and the ground under it.
    pub fn render(
        &self,
        surface: &mut Surface,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        artwork: &mut dyn IconArtwork,
        area: Rect,
        ground: Option<tairix_raster::Color>,
    ) {
        let shadow = IconTile::shadow_cast(scale, theme);
        let (ink, halo) = Self::resting_label(ground, scale);
        let mut halos = self.halos.take();
        if let (Some(_), Some(tile)) = (halo, layout.cell_rect(0)) {
            halos.hold(scale, (tile.width, tile.height), theme);
        } else {
            // A name on a flat ground keeps no halo.
            halos = Halos::default();
        }
        self.visit_icons(
            layout,
            scale,
            theme,
            |bounds| !bounds.intersection(&area).is_empty(),
            |icon| {
                let state = self.icon_state(icon.index);
                let mut tile = Tiles::Large
                    .tile(icon.entry, state, icon.kind, self.endings)
                    .with_faded(is_absent(icon.entry));
                if state == ControlState::idle() {
                    tile = tile.with_label_ink(ink);
                    if let Some(halo) = halo {
                        tile = tile.with_label_shadow(halo);
                        if let Some(kept) = halos.of(icon.entry.name(), &tile) {
                            tile = tile.with_label_halo(kept);
                        }
                    }
                }
                let art = artwork.shadowed(icon.request, icon.side, shadow);
                tile.render(surface, icon.bounds, scale, theme, art);
            },
        );
        self.halos.set(halos);
        // Confined to `area`: outside it the band is already drawn over
        // pixels this paint did not lay down again.
        if let Some(band) = self
            .band()
            .filter(|band| !band.intersection(&area).is_empty())
        {
            let left = u32::try_from(area.left()).unwrap_or(0);
            let top = u32::try_from(area.top()).unwrap_or(0);
            surface.with_clip(left, top, area.width, area.height, |surface| {
                tairix_browse::marquee::draw(surface, band, scale, theme);
            });
        }
    }

    /// How a resting name is drawn on `ground`: near-white over a dark halo on
    /// a picture, whose light under any one name the desktop cannot know, and
    /// on a flat colour in whichever of near-white or near-black contrasts
    /// more with it, which needs no halo.
    fn resting_label(
        ground: Option<tairix_raster::Color>,
        scale: Scale,
    ) -> (tairix_raster::Color, Option<tairix_font::TextShadow>) {
        use tairix_colour::{Rgb, Xyz};
        use tairix_raster::Color;
        const LIGHT: Rgb = Rgb::new(0xf6, 0xf7, 0xf8);
        const DARK: Rgb = Rgb::new(0x16, 0x18, 0x1a);
        // The ground luminance the two inks contrast with equally under WCAG
        // 2.1: a lighter ground takes the dark one.
        const EVEN: f64 = 0.19;
        let Some(flat) = ground else {
            let halo = tairix_font::TextShadow::new(Color::rgba(0, 0, 0, 200), scale);
            return (Color::from(LIGHT), Some(halo));
        };
        let lighter = Xyz::from_rgb(Rgb::new(flat.r, flat.g, flat.b)).y > EVEN;
        (Color::from(if lighter { DARK } else { LIGHT }), None)
    }

    /// Visit every shown icon whose rectangle `wanted` admits, bottom first.
    ///
    /// The paint and the artwork landing both walk the icons here, so the
    /// icons a landing repaints are exactly the ones the paint draws from it,
    /// and an icon standing over another is drawn after it.
    fn visit_icons(
        &self,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        wanted: impl Fn(Rect) -> bool,
        mut visit: impl FnMut(ShownIcon<'_>),
    ) {
        // Spelled once for the whole walk; an icon that names its own file
        // appends its leaf into this one buffer rather than allocating a path.
        let dir = tairix_browse::vfs::spell_absolute_path(&self.folder);
        let mut scratch = String::new();
        let shown = self.shown(layout);
        for (index, bounds) in shown.bottom_up().filter(|&(_, bounds)| wanted(bounds)) {
            let Some(entry) = self.entries.get(index) else {
                continue;
            };
            let (kind, request) =
                entry_icon(&dir, &self.folder, entry, Pictures::ALL, &mut scratch);
            let request = match self.stand_in_for(entry.name()) {
                Some(picture) => request.standing_in(IconRequest::from_carried(
                    IconKind::for_asset(picture.icon.as_str()),
                    picture.own.as_ref(),
                )),
                None => request,
            };
            visit(ShownIcon {
                index,
                entry,
                bounds,
                kind,
                side: Tiles::Large.layout().icon_side(bounds, scale, theme),
                request,
            });
        }
    }

    /// The topmost icon whose tile's picture or name — not the ground around
    /// and between them — covers screen position `at`.
    fn icon_at(&self, layout: &GridView, at: Point, scale: Scale, theme: &Theme) -> Option<usize> {
        let shown = self.shown(layout);
        let hit = shown
            .under(at)
            .find(|&(index, rect)| {
                let Some(entry) = self.entries.get(index) else {
                    return false;
                };
                rect.contains(at)
                    && entry_parts(Tiles::Large, (entry, self.endings), rect, scale, theme)
                        .is_some_and(|parts| parts.contains(at))
            })
            .map(|(index, _)| index);
        hit
    }

    /// The composed control state of the icon at `index`: selected, focused too
    /// while the desktop holds the keyboard, and lit as where a drag would
    /// drop.
    fn icon_state(&self, index: usize) -> ControlState {
        let mut state = ControlState::idle();
        if self.selection.contains(index) {
            state.selection = SelectionState::Selected;
            if self.focused && self.focus == Some(index) {
                state.focus = FocusState::FOCUSED;
            }
        }
        if self.drop_target == Some(index) {
            state.pointer = PointerState::DragTarget;
        }
        state
    }

    /// The folder icon a drop at screen position `at` lands in, or `None`
    /// for the desktop's own folder.
    #[must_use]
    pub fn drop_icon_at(
        &self,
        at: Point,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
    ) -> Option<usize> {
        self.icon_at(layout, at, scale, theme).filter(|&index| {
            self.entries
                .get(index)
                .is_some_and(|entry| entry.kind().resolved() == Some(EntryKind::Directory))
        })
    }

    /// The folder a drop on `icon` lands in: that folder icon's, or the
    /// desktop's own for `None` or an icon that is not a folder.
    #[must_use]
    pub fn drop_folder(&self, icon: Option<usize>) -> String {
        match icon.and_then(|index| self.entries.get(index)) {
            Some(entry) if entry.kind().resolved() == Some(EntryKind::Directory) => {
                self.path_of(entry.name())
            }
            _ => self.folder_path(),
        }
    }

    /// Report, once for each arrangement of the icons, that a revealed frame
    /// has carried the folder's every icon drawn as it will stay, which
    /// `drawn_as_kept` answers of this desktop: asked only while a report is
    /// owed, and never while the folder is unread or a listing is on its way.
    pub fn report_settled(
        &mut self,
        revealed: bool,
        drawn_as_kept: impl FnOnce(&Self) -> bool,
        report: impl FnOnce(),
    ) {
        let owed = self.settled_at != Some(self.revision);
        let listed = self.listed.read && !self.listed.owed;
        if !revealed || !owed || !listed || !drawn_as_kept(self) {
            return;
        }
        self.settled_at = Some(self.revision);
        report();
    }

    /// The revision of the icons' places [`drop_icon_at`](Self::drop_icon_at)
    /// indexes.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The icon lit as where a carried drag would drop.
    #[must_use]
    pub const fn drop_target(&self) -> Option<usize> {
        self.drop_target
    }

    /// Light `icon` as where a carried drag would drop, or none, adding the
    /// icons that changed to `damage`.
    pub fn set_drop_target(&mut self, icon: Option<usize>, layout: &GridView, damage: &mut Region) {
        self.settle(layout);
        if self.drop_target != icon {
            self.mark_icon(layout, self.drop_target, damage);
            self.mark_icon(layout, icon, damage);
            self.drop_target = icon;
        }
    }

    /// A primary press at screen position `at`, at monotonic time `now_ns`,
    /// with `modifiers` held.
    ///
    /// A press on an icon selects it alone and arms the double-click engine,
    /// so a second press on the same icon within the shared window activates
    /// it; one that is not the second of a pair may go on to carry it, and the
    /// rest of the selection with it ([`follow_press`](Self::follow_press)).
    /// Control toggles the icon in the selection and Shift adds it. A press
    /// inside a selection of several keeps it, so a drag carries them all, and
    /// collapses it to that icon on a release that did not travel. A press on
    /// empty desktop — the ground around an icon's picture and name included —
    /// clears the selection unless Control or Shift is held, and starts a band
    /// that selects what it touches as it is dragged out.
    #[allow(clippy::too_many_arguments)] // The press, the field it lands on, and the round's report.
    pub fn press(
        &mut self,
        at: Point,
        modifiers: Modifiers,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        now_ns: u64,
        apps: &[AppAssociation],
        damage: &mut Region,
    ) -> DesktopOutcome {
        self.settle(layout);
        self.carry = None;
        self.collapse = None;
        self.end_band(scale, damage);
        // Taking the keyboard puts the ring on the focus, so it is repainted
        // whether the press moves the selection or merely claims focus.
        self.focused = true;
        self.mark_icon(layout, self.focus, damage);
        let adding = modifiers.ctrl || modifiers.shift;
        let Some(index) = self.icon_at(layout, at, scale, theme) else {
            if !adding {
                self.clear_selection(layout, damage);
            }
            self.focus = None;
            self.band = self.selection.try_clone().map(|base| Band {
                anchor: at,
                head: at,
                live: false,
                base,
                touched: Selection::new(),
                next: Selection::new(),
            });
            return DesktopOutcome::ignored();
        };
        if modifiers.ctrl {
            self.selection.toggle(index);
        } else if modifiers.shift {
            self.selection.insert(index);
        } else if self.selection.contains(index) && self.selection.len() > 1 {
            self.collapse = Some(index);
        } else {
            self.clear_selection(layout, damage);
            self.selection.single(index);
        }
        self.focus = Some(index);
        self.mark_icon(layout, self.focus, damage);
        let subject = u64::try_from(index).unwrap_or(u64::MAX);
        if self.clicks.register(
            now_ns,
            subject,
            PointerButton::Primary,
            self.settings.double_click,
        ) == ClickKind::Double
            && !adding
        {
            self.collapse = None;
            return self.activate(index, apps);
        }
        if !self.selection.contains(index) {
            return DesktopOutcome::ignored();
        }
        let carry = self.shown(layout).rect(index).map(|rect| Carry {
            index,
            from: at,
            grab: (
                at.x.saturating_sub(rect.left()),
                at.y.saturating_sub(rect.top()),
            ),
            rest: Point::new(rect.left(), rect.top()),
            at: None,
        });
        self.carry = carry;
        DesktopOutcome::ignored()
    }

    /// Follow the press to `at`, the primary button still `held` or let go:
    /// a band is dragged out there, or the icons pressed are carried there
    /// while it is held and put down there once it is not. The seat applies a
    /// whole batch of input before any of it is routed, so the release may
    /// come with motion this has yet to follow.
    pub fn follow_press(
        &mut self,
        at: Point,
        held: bool,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> DesktopOutcome {
        if self.band.is_some() {
            self.drag_band(at, layout, scale, theme, damage);
            if !held {
                self.end_band(scale, damage);
            }
            return DesktopOutcome::ignored();
        }
        if self.carry.is_none() {
            return DesktopOutcome::ignored();
        }
        self.carry_to(at, layout, scale, damage);
        if held {
            return DesktopOutcome::ignored();
        }
        if let Some(index) = self
            .collapse
            .take()
            .filter(|_| self.carry_shift().is_none())
        {
            self.clear_selection(layout, damage);
            self.selection.single(index);
            self.mark_icon(layout, Some(index), damage);
        }
        self.put_down(layout, damage)
    }

    /// Whether the press on an icon is a drag, or becomes one with the
    /// pointer at `at`.
    #[must_use]
    pub fn carries_to(&self, at: Point, scale: Scale) -> bool {
        self.carry
            .is_some_and(|carry| carry.at.is_some() || past_drag_slop(carry.from, at, scale))
    }

    /// What the press carries once it is a drag, for the seat to carry over a
    /// window with the pointer at `pointer`: the icons the pressed one leads,
    /// the pressed one first, as a drag shows them and as a drop acts on them
    /// — a drive by its volume's place in the storage catalog. `None` until
    /// the press has travelled, or where the heap cannot describe them.
    #[must_use]
    pub fn carried_cargo(&self, pointer: Point, layout: &GridView) -> Option<DesktopCargo> {
        let carry = self.carry?;
        carry.at?;
        let shown = self.shown(layout);
        let count = shown.carried().count();
        let mut order = Vec::new();
        order.try_reserve_exact(count).ok()?;
        order.extend(shown.carried());
        let pressed = order.iter().position(|&(index, _)| index == carry.index)?;
        order.get_mut(..=pressed)?.rotate_right(1);
        let lead = self.entries.get(carry.index)?;
        let (name, hidden) = match lead.label() {
            Some(label) => (label, 0),
            None => (lead.name(), self.endings.hidden(lead).len()),
        };
        let name = DocumentName::new(name).ok()?;
        let dir = tairix_browse::vfs::spell_absolute_path(&self.folder);
        let mut scratch = String::new();
        let mut items = Vec::new();
        items.try_reserve_exact(count).ok()?;
        let mut pictures = Vec::new();
        pictures.try_reserve_exact(count.min(DRAG_SHOWN_MAX)).ok()?;
        for (index, rect) in order {
            let entry = self.entries.get(index)?;
            items.push(self.dropped_as(entry)?);
            if pictures.len() < DRAG_SHOWN_MAX {
                let centre = rect.center();
                let from = (centre.x - pointer.x, centre.y - pointer.y);
                pictures.push(tairix_browse::media::drag_picture(
                    (&dir, &self.folder),
                    entry,
                    Some(from),
                    &mut scratch,
                )?);
            }
        }
        let count = u32::try_from(items.len()).ok()?;
        let shown = DragItems::new(name, count, false, pictures)
            .ok()?
            .with_filetype(lead.filetype())
            .with_hidden_ending(hidden)
            .ok()?;
        let lift = DesktopLift {
            lead: lead.name().to_string(),
            grab: carry.grab,
        };
        Some(DesktopCargo { shown, items, lift })
    }

    /// What a drop of `entry` off the desktop acts on: the file in the
    /// desktop folder, or for a drive its attached volume where the storage
    /// catalog shows it; `None` for a drive whose volume is not attached.
    fn dropped_as(&self, entry: &Entry) -> Option<DesktopItem> {
        if !is_drive(entry) {
            return Some(DesktopItem {
                path: self.path_of(entry.name()),
                id: entry.id(),
            });
        }
        let target = entry.target()?;
        let present = volume::locate(target, &self.mounts.volumes).present()?;
        Some(DesktopItem {
            path: present.target.clone(),
            id: FileId::NONE,
        })
    }

    /// Give the press up to the seat, which carries its icons on over a
    /// window: they stand where they rest again, and are put down here again
    /// only through [`put_down_lift`](Self::put_down_lift).
    pub fn yield_carry(&mut self, layout: &GridView, damage: &mut Region) {
        self.mark_carried(layout, damage);
        if let Some(carry) = self.carry.as_mut() {
            carry.at = None;
        }
        self.mark_carried(layout, damage);
        self.carry = None;
        self.collapse = None;
    }

    /// The icons `lift` describes came back and were let go at `at`: put them
    /// down there, as a press carried to it would be, the pressed icon found
    /// by its name and set out from where it rests now, so a folder re-listed
    /// meanwhile moves nothing else.
    pub fn put_down_lift(
        &mut self,
        lift: &DesktopLift,
        at: Point,
        layout: &GridView,
        damage: &mut Region,
    ) -> DesktopOutcome {
        self.settle(layout);
        self.carry = None;
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.name() == lift.lead)
        else {
            return DesktopOutcome::ignored();
        };
        let (Some(rest), Some(field)) = (self.shown(layout).rect(index), Field::of(layout)) else {
            return DesktopOutcome::ignored();
        };
        let corner = Point::new(
            at.x.saturating_sub(lift.grab.0),
            at.y.saturating_sub(lift.grab.1),
        );
        self.carry = Some(Carry {
            index,
            from: at,
            grab: lift.grab,
            rest: Point::new(rest.left(), rest.top()),
            at: Some(field.shown_at(corner)),
        });
        self.put_down(layout, damage)
    }

    /// How far the press carries its icons, once it is a drag.
    fn carry_shift(&self) -> Option<(i32, i32)> {
        self.carry.as_ref().and_then(Carry::shift)
    }

    /// Repaint every selected icon and select none.
    fn clear_selection(&mut self, layout: &GridView, damage: &mut Region) {
        if self.selection.is_empty() {
            return;
        }
        self.mark_selected(layout, damage);
        self.selection.clear();
    }

    /// Add every selected icon's tile to `damage`, in one walk of the icons.
    fn mark_selected(&self, layout: &GridView, damage: &mut Region) {
        for (index, rect) in self.shown(layout).bottom_up() {
            if self.selection.contains(index) {
                damage.add(rect);
            }
        }
    }

    /// Drag the band's head to `at`: once past the drag slop it is drawn, and
    /// every icon it touches is selected beside what its press kept.
    fn drag_band(
        &mut self,
        at: Point,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let Some(band) = self.band.as_mut() else {
            return;
        };
        let before = band.rect();
        band.head = at;
        band.live |= past_drag_slop(band.anchor, at, scale);
        let after = band.rect();
        tairix_browse::marquee::report_band(before, after, scale, damage);
        if let Some(rect) = after.filter(|_| before != after) {
            self.sweep_band(rect, layout, scale, theme, damage);
        }
    }

    /// Mark every icon the band `rect` touches selected, beside what the
    /// press kept, and release one it touches no longer unless the press kept
    /// it, adding each icon whose mark changed to `damage`.
    fn sweep_band(
        &mut self,
        rect: Rect,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let Some(mut band) = self.band.take() else {
            return;
        };
        let mut next = core::mem::take(&mut band.next);
        next.reset(self.entries.len());
        self.touching(rect, layout, scale, theme, &mut next);
        for index in next.iter() {
            if !band.touched.contains(index) && !band.base.contains(index) {
                self.selection.insert(index);
            }
        }
        for index in band.touched.iter() {
            if !next.contains(index) && !band.base.contains(index) {
                self.selection.remove(index);
            }
        }
        for (index, tile) in self.shown(layout).bottom_up() {
            let moved = band.touched.contains(index) != next.contains(index);
            if moved && !band.base.contains(index) {
                damage.add(tile);
            }
        }
        band.next = core::mem::replace(&mut band.touched, next);
        self.band = Some(band);
    }

    /// Mark in `touched` every icon whose picture or name `band` touches.
    ///
    /// A flowed icon whose cell's picture the band covers is touched without
    /// laying out its name; only those along the band's edges are tested
    /// part by part, so a sweep costs the band's edges rather than its area.
    fn touching(
        &self,
        band: Rect,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        touched: &mut Selection,
    ) {
        let shown = self.shown(layout);
        let core = tile_core(Tiles::Large, scale, theme);
        let surely = layout.band_cells_within(band, core);
        let meets = |index: usize, tile: Rect| {
            self.entries.get(index).is_some_and(|entry| {
                entry_parts(Tiles::Large, (entry, self.endings), tile, scale, theme)
                    .is_some_and(|parts| parts.meets(&band))
            })
        };
        for cell in layout.band_cells(band).iter() {
            let Some(index) = shown.standing.cells.get(cell).copied().flatten() else {
                continue;
            };
            let hit = surely.contains(cell)
                || layout
                    .cell_rect(cell)
                    .is_some_and(|tile| meets(index, tile));
            if hit && !shown.lifted(index) {
                touched.insert(index);
            }
        }
        for (index, tile) in shown.placed() {
            if !tile.intersection(&band).is_empty() && meets(index, tile) {
                touched.insert(index);
            }
        }
    }

    /// End the band, its press's selection kept as it stands.
    fn end_band(&mut self, scale: Scale, damage: &mut Region) {
        if let Some(band) = self.band.take() {
            tairix_browse::marquee::report_band(band.rect(), None, scale, damage);
        }
    }

    /// Take back what the band selected: the selection is its press's again.
    fn cancel_band(&mut self, layout: &GridView, scale: Scale, damage: &mut Region) {
        let Some(band) = self.band.take() else {
            return;
        };
        tairix_browse::marquee::report_band(band.rect(), None, scale, damage);
        self.mark_selected(layout, damage);
        self.selection = band.base;
        self.mark_selected(layout, damage);
    }

    /// The band, while it is drawn.
    #[must_use]
    pub fn band(&self) -> Option<Rect> {
        self.band.as_ref().and_then(Band::rect)
    }

    /// The pointer moved to `at` while the press on an icon is held: once it
    /// has travelled past the drag slop the icon is carried with it, in memory
    /// alone, adding the ground it left and the ground it covers to `damage`.
    fn carry_to(&mut self, at: Point, layout: &GridView, scale: Scale, damage: &mut Region) {
        self.settle(layout);
        let Some(mut carry) = self.carry else {
            return;
        };
        if carry.at.is_none() && !past_drag_slop(carry.from, at, scale) {
            return;
        }
        let Some(field) = Field::of(layout) else {
            return;
        };
        let corner = field.shown_at(Point::new(
            at.x.saturating_sub(carry.grab.0),
            at.y.saturating_sub(carry.grab.1),
        ));
        if carry.at == Some(corner) {
            return;
        }
        self.mark_carried(layout, damage);
        carry.at = Some(corner);
        self.carry = Some(carry);
        self.clicks.reset();
        self.mark_carried(layout, damage);
    }

    /// Add the tiles of every icon the press carries, where they stand now,
    /// to `damage`.
    fn mark_carried(&self, layout: &GridView, damage: &mut Region) {
        let Some(carry) = self.carry else {
            return;
        };
        let with_selection = self.selection.contains(carry.index);
        for (index, rect) in self.shown(layout).bottom_up() {
            if index == carry.index || (with_selection && self.selection.contains(index)) {
                damage.add(rect);
            }
        }
    }

    /// The press on an icon ended. One that never travelled was a click; one
    /// that did puts the icon down where it was carried — on the nearest free
    /// cell, where the settings keep icons to the grid — over every other, and
    /// the record keeps it there. Adds to `damage` every icon that moved, an
    /// icon the field had no room for taking the cell it left among them.
    fn put_down(&mut self, layout: &GridView, damage: &mut Region) -> DesktopOutcome {
        let Some(carry) = self.carry else {
            return DesktopOutcome::ignored();
        };
        self.settle(layout);
        let (Some(field), Some(_)) = (Field::of(layout), carry.shift()) else {
            self.carry = None;
            return DesktopOutcome::ignored();
        };
        let shown = self.shown(layout);
        let mut carried = Vec::new();
        if carried
            .try_reserve_exact(self.selection.len().max(1))
            .is_err()
        {
            self.carry = None;
            return DesktopOutcome::acting(DesktopAction::Refuse(full_refusal()));
        }
        carried.extend(
            shown
                .carried()
                .map(|(index, rect)| (index, field.shown_at(Point::new(rect.left(), rect.top())))),
        );
        let lift = shown.lift;
        let mut taken = self
            .settings
            .align_to_grid
            .then(|| {
                field.taken(&self.standing, |index| {
                    lift.is_some_and(|l| l.carries(index))
                })
            })
            .flatten();
        let mut order = self.arrangement.next_order();
        let mut set = Vec::new();
        for (index, corner) in carried {
            let Some(entry) = self.entries.get(index) else {
                continue;
            };
            let settled = taken
                .as_mut()
                .and_then(|taken| field.claim_nearest(corner, taken))
                .unwrap_or(corner);
            let placed = Placed {
                id: entry.id(),
                at: field.place_of(settled, self.settings.icons),
                order,
            };
            order = order.saturating_add(1);
            set.push((entry.name().to_string(), Some(placed)));
        }
        let mut refused = false;
        self.marking(
            |_| *layout,
            damage,
            |desktop| {
                desktop.carry = None;
                refused = desktop.record(ArrangementEdit::Set(set)).is_err();
                desktop.resolve_places();
                true
            },
        );
        if refused {
            return DesktopOutcome::acting(DesktopAction::Refuse(full_refusal()));
        }
        DesktopOutcome::ignored()
    }

    /// A secondary (right) press at screen position `at`: the pinboard's
    /// context-menu gesture. Answers whether the press landed on an icon,
    /// which is the only thing that decides whether the menu offers `Open`.
    ///
    /// A press on an icon selects it, so the menu acts on the thing the user
    /// pointed at; a press on empty backdrop leaves the selection exactly as
    /// it was, because asking for the menu is not a way to lose a selection.
    /// The gesture claims no keyboard focus: the window manager does not move
    /// focus for a secondary press on the backdrop, and the desktop does not
    /// pretend otherwise.
    ///
    /// It names no [`DesktopAction`]: the menu is the seat's one chain, opened
    /// by the embedder that owns it, so the desktop model describes the rows
    /// and never asks for a surface.
    pub fn context_press(
        &mut self,
        at: Point,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        self.settle(layout);
        let on_icon = self.icon_at(layout, at, scale, theme);
        // A press inside the selection keeps it, so the menu acts on all of it.
        if let Some(index) = on_icon.filter(|&index| !self.selection.contains(index)) {
            self.clear_selection(layout, damage);
            self.selection.single(index);
            self.mark_icon(layout, self.focus, damage);
            self.focus = Some(index);
            self.mark_icon(layout, self.focus, damage);
        }
        on_icon.is_some()
    }

    /// Resolve one pinboard menu `command` against the desktop's own state,
    /// its icons laid out in `layout`.
    ///
    /// This is the single translation from a named command to a
    /// [`DesktopAction`]: `Open` resolves through the very same activation the
    /// double-click path uses, a sort or arrangement row names the settings the
    /// embedder is to adopt (never applying them behind its back), a new folder
    /// is named through the shared new-directory naming over the listing on
    /// screen, `Tidy Up` lays every icon out on the grid and keeps each there,
    /// and `Refresh` re-lists here and now. A command that asks for what is
    /// already in force changes nothing.
    pub fn command(
        &mut self,
        command: PinboardCommand,
        layout: &GridView,
        apps: &[AppAssociation],
    ) -> DesktopOutcome {
        match command {
            PinboardCommand::Open => self.activate_selection(apps),
            PinboardCommand::Eject => self.eject_selection(),
            // Its row is a field, whose text commits through
            // `retitle_selection`; chosen bare, it changes nothing.
            PinboardCommand::Title => DesktopOutcome::ignored(),
            PinboardCommand::MoveToTrash => self.trash_selection(),
            PinboardCommand::NewFolder => DesktopOutcome::acting(DesktopAction::CreateFolder {
                path: self.path_of(&NewEntry::Folder.suggest_name(&self.entries)),
            }),
            PinboardCommand::SortBy(sort) => self.edit(SettingsEdit::Sort(sort)),
            PinboardCommand::ArrangeFrom(icons) => self.edit(SettingsEdit::Icons(icons)),
            PinboardCommand::TidyUp(order) => self.tidy_up(order, layout),
            PinboardCommand::Refresh => DesktopOutcome {
                rearranged: self.relist(),
                action: None,
            },
            PinboardCommand::OpenDesktopFolder => {
                DesktopOutcome::acting(DesktopAction::Activate(DesktopActivation::OpenFolder {
                    path: self.folder_path(),
                }))
            }
            PinboardCommand::ChangeBackground => {
                DesktopOutcome::acting(DesktopAction::ChangeBackground)
            }
        }
    }

    /// Lay every icon out on `layout`'s cells in `order`, from the corner the
    /// arrangement names, and keep each where it now stands: one change to the
    /// record for the whole desktop. An icon past the cells, or past what the
    /// record keeps, gives up its place and flows after the others, onto a
    /// cell the tidy left free or off the field where none is.
    fn tidy_up(&mut self, order: TidyOrder, layout: &GridView) -> DesktopOutcome {
        self.settle(layout);
        let Some(field) = Field::of(layout) else {
            return DesktopOutcome::ignored();
        };
        let mut ordered = self.entries.clone();
        sort_entries(&mut ordered, self.order(order.sort()));
        let room = field.cells.min(MAX_PLACED);
        let (from, first) = (self.settings.icons, self.arrangement.next_order());
        let set = ordered
            .iter()
            .enumerate()
            .map(|(cell, entry)| {
                let turn = u32::try_from(cell).unwrap_or(u32::MAX);
                let at = field
                    .grid
                    .cell_rect(cell)
                    .filter(|_| cell < room)
                    .map(|rect| Placed {
                        id: entry.id(),
                        at: field.place_of(rect.origin, from),
                        order: first.saturating_add(turn),
                    });
                (entry.name().to_string(), at)
            })
            .collect();
        if self.record(ArrangementEdit::Set(set)).is_err() {
            return DesktopOutcome::acting(DesktopAction::Refuse(full_refusal()));
        }
        self.reflow(false);
        DesktopOutcome {
            rearranged: true,
            action: None,
        }
    }

    /// Eject the volume the selected drive stands for, or say why the user the
    /// desktop acts for may not.
    fn eject_selection(&self) -> DesktopOutcome {
        let Some(entry) = self.selected().and_then(|index| self.entries.get(index)) else {
            return DesktopOutcome::ignored();
        };
        DesktopOutcome::acting(match self.eject_of(entry) {
            Ok(act) => DesktopAction::Volume(act),
            Err(reason) => DesktopAction::Refuse(GestureRefusal::new(
                tairix_util::utf8::refusal_headline(self.endings.shown(entry), "ejected"),
                reason,
            )),
        })
    }

    /// Refuse to move the selected drive to the Trash, saying why: it is drawn
    /// for its volume, and nothing of it is stored on the desktop.
    fn trash_selection(&self) -> DesktopOutcome {
        let Some(entry) = self
            .selected()
            .and_then(|index| self.entries.get(index))
            .filter(|entry| is_drive(entry))
        else {
            return DesktopOutcome::ignored();
        };
        DesktopOutcome::acting(DesktopAction::Refuse(GestureRefusal::new(
            tairix_util::utf8::refusal_headline(self.endings.shown(entry), "moved to the Trash"),
            REASON_DRIVE_NOT_TRASHED,
        )))
    }

    /// The eject of the volume the drive `entry` stands for, or why the user
    /// the desktop acts for may not eject it.
    fn eject_of(&self, entry: &Entry) -> Result<MountAct, &'static str> {
        let facts = entry
            .volume()
            .filter(|_| is_drive(entry))
            .filter(|facts| self.viewer.is_some_and(|uid| facts.ejectable_by(uid)))
            .ok_or(REASON_NOT_EJECTABLE)?;
        Ok(MountAct::eject(facts, self.endings.shown(entry)))
    }

    /// Retitle the volume the selected drive stands for `text`, as its menu's
    /// field committed it, or say why the user the desktop acts for may not:
    /// the one check every surface offering a title makes
    /// ([`MountAct::retitle`]). The title the volume records asks nothing.
    pub fn retitle_selection(&self, text: &str) -> DesktopOutcome {
        let Some(entry) = self
            .selected()
            .and_then(|index| self.entries.get(index))
            .filter(|entry| is_drive(entry))
        else {
            return DesktopOutcome::ignored();
        };
        let name = self.endings.shown(entry);
        let retitled = entry.volume().map_or_else(
            || Err(title_refusal(name, REASON_TITLE_ABSENT)),
            |facts| MountAct::retitle(facts, name, self.viewer, text),
        );
        match retitled {
            Ok(Some(act)) => DesktopOutcome::acting(DesktopAction::Volume(act)),
            Ok(None) => DesktopOutcome::ignored(),
            Err(refusal) => DesktopOutcome::acting(DesktopAction::Refuse(GestureRefusal::new(
                refusal.headline,
                refusal.reason,
            ))),
        }
    }

    /// The title of the volume the drive `entry` stands for, which its menu's
    /// *Title* field opens on, or why the user the desktop acts for may not
    /// retitle it.
    fn title_of<'a>(&self, entry: &'a Entry) -> Result<&'a str, &'static str> {
        let facts = entry.volume().ok_or(REASON_TITLE_ABSENT)?;
        if let Some(why) = facts.retitling_by(self.viewer).refusal() {
            return Err(why);
        }
        // A recorded title no field could hold opens the field empty.
        Ok(if VolumeTitle::new(&facts.title).is_ok() {
            facts.title.as_str()
        } else {
            ""
        })
    }

    /// Name `edit` for the embedder to make, or change nothing when the
    /// settings in force already say it.
    fn edit(&self, edit: SettingsEdit) -> DesktopOutcome {
        if edit.is_in(&self.settings) {
            return DesktopOutcome::ignored();
        }
        DesktopOutcome::acting(DesktopAction::EditSettings(edit))
    }

    /// A key while the desktop holds the keyboard: an arrow moves the
    /// selection to the nearest icon that way, `Enter` activates it, and
    /// `Escape` clears it.
    ///
    /// A key the desktop has no meaning for changes nothing. Releases are
    /// ignored: every desktop key acts on the press.
    pub fn key(
        &mut self,
        key: Key,
        pressed: bool,
        layout: &GridView,
        apps: &[AppAssociation],
        damage: &mut Region,
    ) -> DesktopOutcome {
        if !pressed || !self.focused {
            return DesktopOutcome::ignored();
        }
        self.settle(layout);
        match key {
            Key::Named(NamedKey::Enter) => self.activate_selection(apps),
            Key::Named(NamedKey::Escape) if self.band.is_some() => {
                let scale = self.settings.scale;
                self.cancel_band(layout, scale, damage);
                DesktopOutcome::ignored()
            }
            Key::Named(NamedKey::Escape) => {
                self.clear_selection(layout, damage);
                let focus = self.focus.take();
                self.mark_icon(layout, focus, damage);
                DesktopOutcome::ignored()
            }
            Key::Named(named) => match toward(named) {
                Some(toward) => self.move_selection(toward, layout, damage),
                None => DesktopOutcome::ignored(),
            },
            Key::Char(_) => DesktopOutcome::ignored(),
        }
    }

    /// Move the selection to the nearest icon `toward` the selected one. With
    /// nothing selected, or the selection not shown, the first arrow selects
    /// the first icon shown, so the keyboard always has somewhere to start;
    /// with no icon that way, nothing moves.
    fn move_selection(
        &mut self,
        toward: (i64, i64),
        layout: &GridView,
        damage: &mut Region,
    ) -> DesktopOutcome {
        let next = {
            let shown = self.shown(layout);
            match self.focus.and_then(|at| Some((at, shown.rect(at)?))) {
                None => shown.bottom_up().map(|(index, _)| index).min(),
                Some(from) => nearest_toward(&shown, from, toward),
            }
        };
        let Some(next) = next.filter(|&next| self.selected() != Some(next)) else {
            return DesktopOutcome::ignored();
        };
        self.clear_selection(layout, damage);
        self.mark_icon(layout, self.focus, damage);
        self.selection.single(next);
        self.focus = Some(next);
        self.mark_icon(layout, self.focus, damage);
        DesktopOutcome::ignored()
    }

    /// Activate whatever is selected, if anything.
    ///
    /// The one definition of "open the selection", so the menu's `Open` row,
    /// the `Enter` key, and a double-click can never disagree about what
    /// opening an icon means.
    fn activate_selection(&self, apps: &[AppAssociation]) -> DesktopOutcome {
        if self.selection.len() > 1 {
            return DesktopOutcome::acting(DesktopAction::Refuse(GestureRefusal::new(
                String::from("Nothing was opened"),
                REASON_SEVERAL,
            )));
        }
        match self.selected() {
            Some(index) => self.activate(index, apps),
            None => DesktopOutcome::ignored(),
        }
    }

    /// Resolve what activating the icon at `index` means.
    ///
    /// A directory opens the file manager at it; an application bundle
    /// launches; a disk image is mounted, as the file manager mounts one; any
    /// other file launches the application the shared association model picks
    /// for it, opening the file. A file nothing is associated with is refused
    /// with a stated reason and nothing else happens.
    ///
    /// A **shortcut** — a symbolic link on the desktop — acts on what it
    /// names: a folder or a file is opened through the link (the kernel
    /// resolves the final link), while a bundle is launched by its *resolved*
    /// path, because the spawn gate parses an entry point as
    /// `…/<Name>.app/Run` and a shortcut named after the program is not that
    /// shape. A shortcut whose target names another root is reached at that
    /// target, since no walk follows it. A shortcut whose target has gone is
    /// refused with its reason, never launched blind. A drive, or a shortcut
    /// to a volume, opens the volume where the storage catalog shows it while
    /// it is attached, and says it is not there otherwise.
    fn activate(&self, index: usize, apps: &[AppAssociation]) -> DesktopOutcome {
        let Some(entry) = self.entries.get(index) else {
            return DesktopOutcome::ignored();
        };
        let path = entry.reached_by(&self.folder_path());
        let bundle_path = match entry.kind() {
            EntryKind::Link(_) => match entry.target() {
                Some(target) => tairix_browse::resolve_target(&self.folder_path(), target),
                None => {
                    return DesktopOutcome::acting(DesktopAction::Refuse(GestureRefusal::new(
                        format!(
                            "The shortcut \u{201c}{}\u{201d} names nothing",
                            tairix_util::utf8::shown(entry.name())
                        ),
                        "It holds no target to open.",
                    )));
                }
            },
            _ => path.clone(),
        };
        match entry.kind() {
            EntryKind::Link(LinkTarget::Volume) => self.open_volume(entry),
            EntryKind::Directory | EntryKind::Link(LinkTarget::Directory) => {
                DesktopOutcome::acting(DesktopAction::Activate(DesktopActivation::OpenFolder {
                    path,
                }))
            }
            EntryKind::Bundle | EntryKind::Link(LinkTarget::Bundle) => {
                DesktopOutcome::acting(DesktopAction::Activate(launch_of(
                    &bundle_path,
                    bundle_label(leaf_of(&bundle_path)),
                    None,
                )))
            }
            EntryKind::File | EntryKind::Link(LinkTarget::File)
                if tairix_browse::media_for_entry(entry, &self.folder).holds_a_volume() =>
            {
                DesktopOutcome::acting(DesktopAction::Activate(DesktopActivation::MountImage {
                    path,
                }))
            }
            EntryKind::File | EntryKind::Link(LinkTarget::File) => {
                match applications_for(entry.name(), entry.filetype(), apps).first() {
                    Some(app) => DesktopOutcome::acting(DesktopAction::Activate(launch_of(
                        app.bundle_path(),
                        app.name().to_string(),
                        Some(LaunchDocument {
                            path,
                            edits: app.writes_documents(),
                        }),
                    ))),
                    None => DesktopOutcome::acting(DesktopAction::Refuse(GestureRefusal::new(
                        format!(
                            "No installed application opens \u{201c}{}\u{201d}",
                            tairix_util::utf8::shown(entry.name())
                        ),
                        "No program installed here claims files of this kind.",
                    ))),
                }
            }
            // A shortcut whose target cannot be reached: reported, never
            // launched or opened on the chance that it works.
            EntryKind::Link(LinkTarget::Dangling) => {
                DesktopOutcome::acting(DesktopAction::Refuse(GestureRefusal::new(
                    format!(
                        "The shortcut \u{201c}{}\u{201d} points at something that is not there",
                        tairix_util::utf8::shown(entry.name())
                    ),
                    "What it named has been moved or deleted.",
                )))
            }
        }
    }

    /// What opening `entry`, a drive or a shortcut to a volume, asks for: the
    /// volume shown where the storage catalog shows it, as a double-click on
    /// its entry there shows it, or a refusal saying why it cannot be.
    fn open_volume(&self, entry: &Entry) -> DesktopOutcome {
        let located = volume::locate(entry.target().unwrap_or_default(), &self.mounts.volumes);
        if let Some(present) = located.present() {
            return DesktopOutcome::acting(DesktopAction::Activate(
                DesktopActivation::OpenFolder {
                    path: present.target.clone(),
                },
            ));
        }
        located.refusal(self.endings.shown(entry)).map_or_else(
            DesktopOutcome::ignored,
            |(headline, reason)| {
                DesktopOutcome::acting(DesktopAction::Refuse(GestureRefusal::new(headline, reason)))
            },
        )
    }

    /// The absolute path of the child called `name` inside the desktop folder.
    fn path_of(&self, name: &str) -> String {
        let mut path = tairix_browse::vfs::spell_absolute_path(&self.folder);
        tairix_path::push_name(&mut path, name);
        path
    }
}

/// The way an arrow key moves the selection on the screen, or `None` for a
/// key that is not an arrow.
const fn toward(key: NamedKey) -> Option<(i64, i64)> {
    match key {
        NamedKey::Left => Some((-1, 0)),
        NamedKey::Right => Some((1, 0)),
        NamedKey::Up => Some((0, -1)),
        NamedKey::Down => Some((0, 1)),
        _ => None,
    }
}

/// The icon nearest the one at `from`, which stands at `bounds`, in the
/// direction `toward`: the shortest way there, an icon off to the side
/// counting its sideways distance twice so the selection keeps to its row or
/// column. Icons standing exactly on one another are met first, in the order
/// they are drawn — upward for an arrow right or down, downward for one left
/// or up — so every one of a stack is reached.
fn nearest_toward(
    shown: &Shown<'_>,
    (from, bounds): (usize, Rect),
    (dx, dy): (i64, i64),
) -> Option<usize> {
    let origin = bounds.center();
    let rank = shown.bottom_up().position(|(index, _)| index == from)?;
    let upward = dx + dy > 0;
    shown
        .bottom_up()
        .enumerate()
        .filter(|&(_, (index, _))| index != from)
        .filter_map(|(at, (index, rect))| {
            let centre = rect.center();
            let x = i64::from(centre.x) - i64::from(origin.x);
            let y = i64::from(centre.y) - i64::from(origin.y);
            let along = x * dx + y * dy;
            let aside = (x * dy - y * dx).abs();
            let (ahead, gap) = if along == 0 && aside == 0 {
                ((at > rank) == upward, at.abs_diff(rank))
            } else {
                (along > 0, 0)
            };
            ahead.then_some((along + 2 * aside, gap, index))
        })
        .min()
        .map(|(_, _, index)| index)
}

/// The shared grid flow the settings' icon arrangement names.
const fn grid_flow(flow: IconFlow) -> GridFlow {
    match flow {
        IconFlow::Leading => GridFlow::ColumnsFromLeading,
        IconFlow::Trailing => GridFlow::ColumnsFromTrailing,
    }
}

/// The shared listing order the settings' icon sort names.
///
/// The two vocabularies meet in exactly this one function, and the settings
/// engine deliberately does not speak [`SortMode`] itself: that type belongs to
/// the file-browser engine, and a five-line configuration document that every
/// consumer of the user's pinboard store must parse has no business dragging
/// that engine's dependency weight in behind it. Bridging here keeps the
/// desktop ordering its listing through the single shared sort — there is still
/// no second sort — while the store stays a store.
const fn sort_mode(sort: IconSort) -> SortMode {
    let key = match sort {
        IconSort::Name => SortKey::Name,
        IconSort::Kind => SortKey::Kind,
        IconSort::Size => SortKey::Size,
        IconSort::Date => SortKey::Modified,
    };
    SortMode {
        key,
        ..SortMode::default_order()
    }
}

/// The launch activation for the bundle at `bundle`, reported as `label` and
/// optionally opening `document`. One spelling of "a bundle's entry point is
/// its `Run` binary", so the desktop's three launch paths cannot diverge.
fn launch_of(bundle: &str, label: String, document: Option<LaunchDocument>) -> DesktopActivation {
    DesktopActivation::Launch {
        run_path: tairix_appstore::entry_path(bundle),
        label,
        document,
    }
}

/// The final component of the absolute `path` — the name a resolved bundle is
/// reported by. The one shared spelling rule, through the browser engine that
/// already owns this app's path handling.
fn leaf_of(path: &str) -> &str {
    tairix_browse::leaf_name(path)
}

/// The name an application bundle is reported by: its directory name without
/// the bundle suffix.
fn bundle_label(name: &str) -> String {
    name.strip_suffix(tairix_abi::BUNDLE_SUFFIX)
        .unwrap_or(name)
        .to_string()
}

/// The refusal of a place the record has no room left to keep.
fn full_refusal() -> GestureRefusal {
    GestureRefusal::new(
        "The desktop cannot keep that icon where it was put",
        format!("It keeps where at most {MAX_PLACED} icons stand, and that many are kept already."),
    )
}

/// Whether `entry` is a drive the desktop draws for a volume rather than a name
/// its folder lists: one named by its volume's root, which no listed name can
/// be.
fn is_drive(entry: &Entry) -> bool {
    volume::reference(entry.name()).is_some()
}

/// The name `change` is to.
fn changed_name(change: &EntryChange) -> &str {
    match change {
        EntryChange::Upsert(entry) => entry.name(),
        EntryChange::Remove(name) => name,
    }
}

/// Whether `entry` is a shortcut to a volume the mount table does not show
/// attached, which is drawn faded.
fn is_absent(entry: &Entry) -> bool {
    entry.kind() == EntryKind::Link(LinkTarget::Volume) && entry.volume().is_none()
}

/// `units` of a pitch `pitch` pixels long, in pixels, to the nearest.
fn pixels_of(units: u32, pitch: u32) -> i32 {
    let pixels =
        (u64::from(units) * u64::from(pitch) + u64::from(PLACE_UNITS / 2)) / u64::from(PLACE_UNITS);
    i32::try_from(pixels).unwrap_or(i32::MAX)
}

/// `pixels` along a pitch `pitch` pixels long, in place units, to the nearest.
fn units_of(pixels: i32, pitch: u32) -> u32 {
    let pixels = u64::try_from(pixels).unwrap_or(0);
    let pitch = u64::from(pitch.max(1));
    let units = (pixels * u64::from(PLACE_UNITS) + pitch / 2) / pitch;
    u32::try_from(units).unwrap_or(u32::MAX)
}

/// The icons as one layout shows them.
struct Shown<'a> {
    standing: Cow<'a, Standing>,
    grid: &'a GridView,
    /// The icons a press carries, and how far from where they rest.
    lift: Option<Lift<'a>>,
}

/// What a press carries: the icon pressed, with the selection when it is in
/// it, each moved by one shift.
#[derive(Clone, Copy)]
struct Lift<'a> {
    lead: usize,
    with: Option<&'a Selection>,
    shift: (i32, i32),
}

impl Lift<'_> {
    fn carries(&self, index: usize) -> bool {
        index == self.lead || self.with.is_some_and(|selection| selection.contains(index))
    }
}

/// `rect` moved by `shift`.
fn shifted(rect: Rect, (dx, dy): (i32, i32)) -> Rect {
    Rect::new(
        rect.left().saturating_add(dx),
        rect.top().saturating_add(dy),
        rect.width,
        rect.height,
    )
}

impl Shown<'_> {
    /// Where the icon at `index` stands, if it is shown.
    fn rect(&self, index: usize) -> Option<Rect> {
        self.bottom_up()
            .find(|&(shown, _)| shown == index)
            .map(|(_, rect)| rect)
    }

    /// Whether the icon at `index` is carried, which stands apart.
    fn lifted(&self, index: usize) -> bool {
        self.lift.is_some_and(|lift| lift.carries(index))
    }

    /// Every icon where it rests, a carried one included.
    fn resting(&self) -> impl DoubleEndedIterator<Item = (usize, Rect)> + '_ {
        let flowed = self
            .standing
            .cells
            .iter()
            .enumerate()
            .filter_map(|(cell, held)| Some(((*held)?, self.grid.cell_rect(cell)?)));
        flowed.chain(self.standing.placed.iter().copied())
    }

    /// The carried icons where they are carried to.
    fn carried(&self) -> impl DoubleEndedIterator<Item = (usize, Rect)> + '_ {
        self.lift.into_iter().flat_map(move |lift| {
            self.resting()
                .filter(move |&(index, _)| lift.carries(index))
                .map(move |(index, rect)| (index, shifted(rect, lift.shift)))
        })
    }

    /// The icons the flow stood, cell by cell, the one carried aside.
    fn flowed(&self) -> impl DoubleEndedIterator<Item = (usize, Rect)> + '_ {
        self.standing
            .cells
            .iter()
            .enumerate()
            .filter_map(move |(cell, held)| {
                let index = held.filter(|&index| !self.lifted(index))?;
                Some((index, self.grid.cell_rect(cell)?))
            })
    }

    /// The icons the user put somewhere, the one put down first first, the one
    /// carried aside.
    fn placed(&self) -> impl DoubleEndedIterator<Item = (usize, Rect)> + '_ {
        self.standing
            .placed
            .iter()
            .copied()
            .filter(move |&(index, _)| !self.lifted(index))
    }

    /// Every icon in the order it is drawn: those the flow stood, then those
    /// the user put somewhere in the order they were put down, then those
    /// carried.
    fn bottom_up(&self) -> impl DoubleEndedIterator<Item = (usize, Rect)> + '_ {
        self.flowed().chain(self.placed()).chain(self.carried())
    }

    /// The icons that may cover `at`, the topmost first: those carried, those
    /// the user put somewhere, and the one the flow stood on the cell there, so
    /// a hit costs the icons placed and carried and no more.
    fn under(&self, at: Point) -> impl Iterator<Item = (usize, Rect)> + '_ {
        let flowed = self.grid.index_at(DESKTOP_SCROLL, at).and_then(|cell| {
            let index = self
                .standing
                .cells
                .get(cell)?
                .filter(|&index| !self.lifted(index))?;
            Some((index, self.grid.cell_rect(cell)?))
        });
        self.carried()
            .rev()
            .chain(self.placed().rev())
            .chain(flowed)
    }
}

/// The cells a desktop layout holds and the pitch between them: what an icon's
/// place is measured in.
struct Field<'a> {
    grid: &'a GridView,
    /// Where the cells are shown, which every icon is kept wholly inside.
    area: Rect,
    /// A tile's width and height.
    cell: (u32, u32),
    /// The distance from one cell to the next, across and down.
    pitch: (u32, u32),
    /// How many cells the field holds.
    cells: usize,
}

impl<'a> Field<'a> {
    /// The field `grid` lays out, or `None` when it holds no cell at all.
    fn of(grid: &'a GridView) -> Option<Self> {
        let first = grid.cell_rect(0)?;
        let per_line = grid.cells_per_line();
        let down = grid
            .cell_rect(1)
            .filter(|_| per_line > 1)
            .map_or(first.height, |next| next.top().abs_diff(first.top()));
        let across = grid
            .cell_rect(per_line)
            .map_or(first.width, |next| next.left().abs_diff(first.left()));
        Some(Self {
            grid,
            area: grid.tile_area(),
            cell: (first.width, first.height),
            pitch: (across.max(1), down.max(1)),
            cells: per_line.saturating_mul(grid.lines_total()),
        })
    }

    /// The corner a tile with its corner at `corner` takes once it is shown
    /// wholly on the field.
    fn shown_at(&self, corner: Point) -> Point {
        Rect::new(corner.x, corner.y, self.cell.0, self.cell.1)
            .clamped_onto(self.area)
            .origin
    }

    /// Where the tile standing at `place` would be, whether or not the field
    /// holds it whole.
    fn wanted(&self, place: Place) -> Rect {
        let across = pixels_of(place.across, self.pitch.0);
        let left = match place.from {
            IconFlow::Leading => self.area.left().saturating_add(across),
            IconFlow::Trailing => self
                .area
                .right()
                .saturating_sub(across)
                .saturating_sub_unsigned(self.cell.0),
        };
        Rect::new(
            left,
            self.area
                .top()
                .saturating_add(pixels_of(place.down, self.pitch.1)),
            self.cell.0,
            self.cell.1,
        )
    }

    /// Whether the field holds `rect` whole.
    fn holds(&self, rect: Rect) -> bool {
        self.area.intersection(&rect) == rect
    }

    /// The place a tile with its corner at `corner` is kept at once it is
    /// shown wholly on the field, measured across from the edge `from` names.
    fn place_of(&self, corner: Point, from: IconFlow) -> Place {
        let shown = Rect::new(corner.x, corner.y, self.cell.0, self.cell.1).clamped_onto(self.area);
        let across = match from {
            IconFlow::Leading => shown.left().saturating_sub(self.area.left()),
            IconFlow::Trailing => self.area.right().saturating_sub(shown.right()),
        };
        Place {
            across: units_of(across, self.pitch.0),
            down: units_of(shown.top().saturating_sub(self.area.top()), self.pitch.1),
            from,
        }
    }

    /// The cells a tile at `rect` reaches into: each holds one of its corners.
    fn cells_of(&self, rect: Rect) -> impl Iterator<Item = usize> + '_ {
        let (right, bottom) = (
            rect.right().saturating_sub(1),
            rect.bottom().saturating_sub(1),
        );
        [
            (rect.left(), rect.top()),
            (right, rect.top()),
            (rect.left(), bottom),
            (right, bottom),
        ]
        .into_iter()
        .filter_map(move |(x, y)| self.grid.index_at(DESKTOP_SCROLL, Point::new(x, y)))
    }

    /// Mark every cell a tile at `rect` reaches into as taken.
    fn reach(&self, rect: Rect, taken: &mut [bool]) {
        for cell in self.cells_of(rect) {
            if let Some(held) = taken.get_mut(cell) {
                *held = true;
            }
        }
    }

    /// Which cells `standing` already holds, by an icon the flow stood there
    /// or by a placed tile reaching into one, the icons `aside` names apart;
    /// `None` without the memory to say.
    fn taken(&self, standing: &Standing, aside: impl Fn(usize) -> bool) -> Option<Vec<bool>> {
        let mut taken = tairix_util::fallible::filled(self.cells, false)?;
        for (held, on) in taken.iter_mut().zip(&standing.cells) {
            *held = on.is_some_and(|index| !aside(index));
        }
        for &(index, rect) in &standing.placed {
            if !aside(index) {
                self.reach(rect, &mut taken);
            }
        }
        Some(taken)
    }

    /// The cell nearest a tile with its corner at `corner`: the nearest free
    /// one, else, unless `free` insists, the nearest of all.
    fn nearest(&self, corner: Point, taken: &[bool], free: bool) -> Option<usize> {
        let distance = |cell: usize| {
            let origin = self.grid.cell_rect(cell)?.origin;
            let x = u64::from(origin.x.abs_diff(corner.x));
            let y = u64::from(origin.y.abs_diff(corner.y));
            // The square of a difference of two `i32`s fits; only the sum of
            // two can run past the range.
            Some(((x * x).saturating_add(y * y), cell))
        };
        let among = |only_free: bool| {
            (0..self.cells)
                .filter(|&cell| !only_free || taken.get(cell) == Some(&false))
                .filter_map(distance)
                .min()
        };
        match among(true) {
            Some((_, cell)) => Some(cell),
            None if free => None,
            None => among(false).map(|(_, cell)| cell),
        }
    }

    /// Take the free cell nearest a tile with its corner at `corner`, else the
    /// nearest of all, answering its corner.
    fn claim_nearest(&self, corner: Point, taken: &mut [bool]) -> Option<Point> {
        let cell = self.nearest(corner, taken, false)?;
        if let Some(held) = taken.get_mut(cell) {
            *held = true;
        }
        Some(self.grid.cell_rect(cell)?.origin)
    }

    /// Where `spots` stand in this field: each icon the user put somewhere
    /// where it was put, in the order `stack` puts them down — on the free
    /// cell nearest there where the field no longer holds it whole, and off
    /// the field with none free; then, where `kept` says this is the field the
    /// flow stood them in, each flowed icon on its own cell; then the rest, in
    /// listing order, on the free cells left until they run out. `None`
    /// without the memory to say.
    fn stand(&self, spots: &[Spot], stack: &[usize], kept: bool) -> Option<Standing> {
        let mut taken = tairix_util::fallible::filled(self.cells, false)?;
        let mut cells = tairix_util::fallible::filled(self.cells, None)?;
        let mut placed: Vec<(usize, Option<Rect>)> = Vec::new();
        placed.try_reserve_exact(stack.len()).ok()?;
        let mut moved = Vec::new();
        for &index in stack {
            let Some(&Spot::Placed(place)) = spots.get(index) else {
                continue;
            };
            let wanted = self.wanted(place);
            if self.holds(wanted) {
                self.reach(wanted, &mut taken);
                placed.push((index, Some(wanted)));
            } else {
                moved.push((placed.len(), self.shown_at(wanted.origin)));
                placed.push((index, None));
            }
        }
        if kept {
            for (index, spot) in spots.iter().enumerate() {
                if let Spot::Flowed(cell) = *spot {
                    if let (Some(held), Some(on)) = (taken.get_mut(cell), cells.get_mut(cell)) {
                        *held = true;
                        *on = Some(index);
                    }
                }
            }
        }
        for (at, corner) in moved {
            let Some(cell) = self.nearest(corner, &taken, true) else {
                continue;
            };
            if let Some(held) = taken.get_mut(cell) {
                *held = true;
            }
            if let Some(slot) = placed.get_mut(at) {
                slot.1 = self.grid.cell_rect(cell);
            }
        }
        let mut next = 0;
        for (index, spot) in spots.iter().enumerate() {
            let owed = match spot {
                Spot::Placed(_) => false,
                Spot::Flowed(_) => !kept,
                Spot::Owed => true,
            };
            if !owed {
                continue;
            }
            while taken.get(next) == Some(&true) {
                next += 1;
            }
            let (Some(held), Some(on)) = (taken.get_mut(next), cells.get_mut(next)) else {
                break;
            };
            *held = true;
            *on = Some(index);
            next += 1;
        }
        let placed = placed
            .into_iter()
            .filter_map(|(index, rect)| Some((index, rect?)))
            .collect();
        Some(Standing { cells, placed })
    }
}
