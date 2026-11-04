//! The desktop: the user's own `Desktop` folder shown as icons on the screen,
//! each standing where the user put it (`plans/NEW-TASKBAR.md` T16,
//! `plans/PINBOARD.md`).
//!
//! The desktop is a *directory view*, not a new kind of surface. It lists the
//! user's `Desktop` folder through the same [`DirectorySource`] seam the
//! trusted file picker uses, under the session's own identity; it orders the
//! listing with the shared [`sort_entries`]; it classifies each child with the
//! shared content-type registry; and its cells are the shared [`GridView`]'s,
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
//! on top, so a press, a hover and a repaint test them from the topmost down,
//! and only the icons the field shows are ever walked.
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
//! What the desktop *owns* is the behaviour a folder-on-the-screen needs:
//! hover feedback, a selection, keyboard navigation while it holds focus, and
//! activation. Each of those reuses the pure engine that already decides it —
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

use tairix_abi::window_ipc::DragItems;
use tairix_abi::{Errno, FileId};
use tairix_browse::media::Ending;
use tairix_browse::render::{entry_parts, Tiles};
use tairix_browse::{
    applications_for, entry_icon, merge_changes, resolve_occupancy, sort_entries, AppAssociation,
    DirectorySource, Entry, EntryChange, EntryKind, GridFlow, GridView, HiddenEndings, LinkTarget,
    Listing, NewEntry, Pictures, SortKey, SortMode,
};
use tairix_controls::state::{ControlState, FocusState, PointerState, SelectionState};
use tairix_controls::{IconTile, LabelHalo};
use tairix_geometry::{to_i32, GridFill, GridRun, Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind, IconRequest, Landed};
use tairix_proglib::{Catalog, EntryId};
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_wallpaper::{DesktopSettings, IconFlow, IconSort};
use tairix_wm::{past_drag_slop, ClickKind, DoubleClickTracker, Key, NamedKey, PointerButton};

use crate::launcher::Kept;
use crate::library::catalogued;
use crate::pinboard::{
    Arrangement, ArrangementEdit, ArrangementFull, ArrangementMemory, PinboardCommand, Place,
    PlaceKey, Placed, TidyOrder, MAX_PLACED, PLACE_UNITS,
};

/// The inset, in logical pixels at the reference density, between the work
/// area's edges and the first icon.
///
/// A deliberate, fixed piece of visual spacing — not a capacity — so the
/// trailing column does not touch the screen edge and the top icon clears the
/// work area's top edge.
pub const DESKTOP_MARGIN: u32 = 8;

/// How long a drop or a save onto the desktop waits for its files once the
/// program making them has been told where they go: long enough for it to
/// make them. The file pressed is known by its name, a drop's others only by
/// arriving meanwhile, so the wait is held to what making them takes.
pub const LANDING_WAIT_NS: u64 = 10_000_000_000;

/// Where the desktop's cells rest: their anchored edge, always. The desktop
/// does not scroll.
const DESKTOP_SCROLL: u64 = 0;

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

/// What a drag or a save over the bare desktop would leave there if it were
/// let go: the names the first file may arrive under, how many files there
/// are, and where each one shown was held from the press.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LandingOffer {
    first: Vec<String>,
    count: u32,
    /// Where each picture shown stood from the press, the first item's first.
    offsets: Vec<(i32, i32)>,
}

impl LandingOffer {
    /// What a drag of `items` would leave.
    #[must_use]
    pub fn of(items: &DragItems) -> Self {
        Self {
            first: alloc::vec![items.first().as_str().to_string()],
            count: items.count(),
            offsets: items
                .pictures()
                .map(|picture| picture.from.unwrap_or((0, 0)))
                .collect(),
        }
    }

    /// What a save of the one file asked for as `name` leaves, which a volume
    /// that keeps no file type makes under `finished` instead.
    #[must_use]
    pub fn saved(name: &str, finished: Option<&str>) -> Self {
        let mut first = alloc::vec![name.to_string()];
        first.extend(
            finished
                .filter(|finished| *finished != name)
                .map(String::from),
        );
        Self {
            first,
            count: 1,
            offsets: alloc::vec![(0, 0)],
        }
    }
}

/// Files a drop or a save onto the desktop is to leave where it was let go.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Landing {
    /// The names the first file may arrive under, before its copy tells it
    /// apart from a name already taken.
    first: Vec<String>,
    /// Where each lands, the first item's first; the others take the rest in
    /// the order they arrive.
    places: Vec<Place>,
    /// Whether the first has arrived.
    led: bool,
    /// How many of the others are yet to arrive.
    owed: u32,
    /// How many of the others have arrived.
    others: usize,
    until_ns: u64,
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
    index: usize,
    /// Where the press began.
    from: Point,
    /// The press's offset from the tile's corner.
    grab: (i32, i32),
    /// Where the tile stands, once the press has travelled far enough to be a
    /// drag.
    at: Option<Point>,
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
/// as long as the scale, the tile size and the theme it was blurred for stand.
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
    entries: Vec<Entry>,
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
    /// What a drag now over the bare desktop would leave there.
    offer: Option<LandingOffer>,
    landing: Option<Landing>,
    carry: Option<Carry>,
    selected: Option<usize>,
    hovered: Option<usize>,
    /// The folder icon a carried drag would drop into, while its application
    /// accepts it there.
    drop_target: Option<usize>,
    /// Bumped whenever the icons change place, so an index into them names
    /// the same icon only at the revision it was taken at.
    revision: u64,
    focused: bool,
    clicks: DoubleClickTracker,
}

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
    /// [`apply_settings`](Self::apply_settings).
    #[must_use]
    pub fn new(source: S, folder: Vec<String>) -> Self {
        Self {
            source,
            folder,
            settings: DesktopSettings::default(),
            entries: Vec::new(),
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
            landing: None,
            carry: None,
            selected: None,
            hovered: None,
            drop_target: None,
            revision: 0,
            focused: false,
            clicks: DoubleClickTracker::new(),
        }
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
    /// *directory* it launches. That is what makes the shortcut read as an
    /// application on the desktop: bundle-ness is decided from the target's own
    /// leaf name, never from the link's, so `Chess` → `/System/Applications/chess.app` is an
    /// application while the link itself is just a name. The target is carried
    /// verbatim and never resolved here, so a shortcut whose bundle is later
    /// removed dangles honestly rather than being prevented at creation.
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
        DesktopAction::CreateShortcut {
            link: self.path_of(name),
            target: chosen.bundle().to_string(),
        }
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
            let held = self.held();
            sort_entries(&mut self.entries, sort_mode(self.settings.sort));
            self.follow(held);
        }
        if relayout || resorted {
            self.reflow(relayout);
        }
        Some(change)
    }

    /// The selected icon's index, if any.
    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The icon the pointer is over, if any.
    #[must_use]
    pub const fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Whether the desktop holds the keyboard (no window is focused).
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Tell the desktop whether it holds the keyboard, adding what that
    /// changed to `damage`.
    ///
    /// Only the selected icon wears the Focus Ring, so gaining or losing the
    /// keyboard changes that one icon and nothing else — and with nothing
    /// selected it changes no pixel at all. This is the click that moves
    /// focus between the desktop and a window, which is exactly when the
    /// screen must *not* be repainted wholesale.
    pub fn set_focused(&mut self, focused: bool, layout: &GridView, damage: &mut Region) {
        self.settle(layout);
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        self.mark_icon(layout, self.selected, damage);
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

    /// Note that the pointer is somewhere other than the desktop (over a
    /// window, the taskbar, or one of its popovers), clearing the hover.
    pub fn pointer_left(&mut self, layout: &GridView, damage: &mut Region) {
        let left = self.hovered.take();
        self.mark_icon(layout, left, damage);
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
        sort_entries(&mut entries, sort_mode(self.settings.sort));
        if entries.len() == self.entries.len()
            && entries
                .iter()
                .zip(&self.entries)
                .all(|(a, b)| a.same_listing(b))
        {
            self.reconcile();
            return false;
        }
        let held = self.held();
        let was = core::mem::replace(&mut self.entries, entries);
        let mut stood: BTreeMap<&str, (FileId, Spot)> = was
            .iter()
            .zip(&self.spots)
            .map(|(entry, spot)| (entry.name(), (entry.id(), *spot)))
            .collect();
        let mut spots = Vec::with_capacity(self.entries.len());
        let mut arrivals = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if let Some((_, spot)) = stood.remove(entry.name()) {
                spots.push(spot);
            } else {
                spots.push(Spot::Owed);
                arrivals.push(index);
            }
        }
        let leaving = stood
            .into_iter()
            .map(|(name, (id, spot))| Leaving {
                name: name.to_string(),
                id,
                spot,
            })
            .collect();
        self.spots = spots;
        self.revision = self.revision.wrapping_add(1);
        self.follow(held);
        self.arrived(&arrivals, leaving, read);
        self.reconcile();
        true
    }

    /// The names of the icons the selection and a carried press are on, to
    /// find them again once the listing is reordered or replaced.
    fn held(&self) -> (Option<String>, Option<(String, Carry)>) {
        let name = |index: usize| {
            self.entries
                .get(index)
                .map(|entry| entry.name().to_string())
        };
        (
            self.selected.and_then(name),
            self.carry
                .and_then(|carry| Some((name(carry.index)?, carry))),
        )
    }

    /// Put the selection and a carried press back on the icons `held` names,
    /// so neither lands on a different icon under the user's pointer. The
    /// hover, the lit drop folder and a half-made double-click go with the
    /// order, since what they were taken against has moved.
    fn follow(&mut self, (chosen, carried): (Option<String>, Option<(String, Carry)>)) {
        let find = |name: &str| self.entries.iter().position(|entry| entry.name() == name);
        self.selected = chosen.and_then(|name| find(&name));
        self.carry = carried.and_then(|(name, carry)| {
            Some(Carry {
                index: find(&name)?,
                ..carry
            })
        });
        self.hovered = None;
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
        let leaving = self.leaving(&changes);
        let merged = merge_changes(&mut self.entries, changes, sort_mode(self.settings.sort));
        // Without the memory to merge them, the folder is read again.
        let Some((placement, moved)) = merged else {
            return self.relist();
        };
        if !moved {
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
        self.selected = self.selected.and_then(|index| placement.place(index));
        self.drop_target = self.drop_target.and_then(|index| placement.place(index));
        self.hovered = self.hovered.and_then(|index| placement.place(index));
        self.carry = self.carry.and_then(|carry| {
            Some(Carry {
                index: placement.place(carry.index)?,
                ..carry
            })
        });
        self.clicks.follow(|at| placement.place_subject(at));
        self.arrived(&arrivals, leaving, true);
        true
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
    /// and one the record keeps a place for stands there. The place of a file
    /// gone is forgotten where `forget` says the folder was read.
    fn arrived(&mut self, arrivals: &[usize], mut leaving: Vec<Leaving>, forget: bool) {
        let mut edits = Vec::new();
        let mut set = Vec::new();
        let mut order = self.arrangement.next_order();
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
            } else if let Some(at) = self.landed(&name) {
                set.push((name, Some(Placed { id, at, order })));
                order = order.saturating_add(1);
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
        self.resolve_places();
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

    /// The place a drop or a save waiting on its files gives the arrived file
    /// named `name`, if one is waiting and the file is one of its own: the
    /// first by a name it was offered under or one its copy told apart, the
    /// others by arriving while any are owed.
    fn landed(&mut self, name: &str) -> Option<Place> {
        let landing = self.landing.as_mut()?;
        let at = if !landing.led && landing.first.iter().any(|offered| copied_as(name, offered)) {
            landing.led = true;
            0
        } else {
            landing.owed = landing.owed.checked_sub(1)?;
            landing.others = landing.others.saturating_add(1);
            landing.others
        };
        let place = landing.places.get(at).copied();
        if landing.led && landing.owed == 0 {
            self.landing = None;
        }
        place
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
        let listed: BTreeSet<PlaceKey> = self
            .entries
            .iter()
            .map(|entry| PlaceKey::of(entry.name()))
            .collect();
        let by_file: BTreeMap<FileId, usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.id().is_none())
            .map(|(index, entry)| (entry.id(), index))
            .collect();
        let mut renamed = Vec::new();
        let mut gone = Vec::new();
        for (key, placed) in self.arrangement.places() {
            if listed.contains(&key) {
                continue;
            }
            let moved = by_file
                .get(&placed.id)
                .and_then(|&index| self.entries.get(index))
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

    /// A refusal no gesture was there to state, since this was last asked.
    pub fn take_refusal(&mut self) -> Option<GestureRefusal> {
        self.refused.take()
    }

    /// Note what a drag or a save now over the bare desktop would leave
    /// there, or that it is over somewhere else.
    pub fn offer_landing(&mut self, offer: Option<LandingOffer>) {
        self.offer = offer;
    }

    /// Wait until `until_ns` for the files the offer standing describes, let
    /// go on the bare desktop at `at`: each picture's centre as far from `at`
    /// as it was held from the press, on the nearest free cells where the
    /// settings keep icons to the grid. Asked once the program making the
    /// files has been told they land here, so a drop refused or concluded
    /// elsewhere waits for nothing.
    pub fn expect_offered(
        &mut self,
        at: Point,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        until_ns: u64,
    ) {
        let Some(offer) = self.offer.take() else {
            return;
        };
        self.settle(layout);
        let Some(field) = Field::of(layout) else {
            return;
        };
        let tile = Rect::new(0, 0, field.cell.0, field.cell.1);
        let measured = Entry::file(offer.first.first().cloned().unwrap_or_default());
        let held = entry_parts(
            Tiles::Large,
            (&measured, HiddenEndings::NONE),
            tile,
            scale,
            theme,
        )
        .and_then(|parts| parts.picture)
        .unwrap_or(tile)
        .center();
        let mut taken = if self.settings.align_to_grid {
            field.taken(&self.standing, None)
        } else {
            None
        };
        // An application says where its pictures stood, and none stands
        // farther from the drop than the field reaches.
        let reach = (to_i32(field.area.width), to_i32(field.area.height));
        let bounded = |offset: i32, most: i32| offset.max(-most).min(most);
        let from = self.settings.icons;
        let places = offer
            .offsets
            .iter()
            .map(|&(dx, dy)| {
                let (dx, dy) = (bounded(dx, reach.0), bounded(dy, reach.1));
                let corner = field.shown_at(Point::new(
                    at.x.saturating_add(dx).saturating_sub(held.x),
                    at.y.saturating_add(dy).saturating_sub(held.y),
                ));
                let corner = taken
                    .as_deref_mut()
                    .and_then(|taken| field.claim_nearest(corner, taken))
                    .unwrap_or(corner);
                field.place_of(corner, from)
            })
            .collect();
        self.landing = Some(Landing {
            first: offer.first,
            places,
            led: false,
            owed: offer.count.saturating_sub(1),
            others: 0,
            until_ns,
        });
    }

    /// Stop waiting for the files of a drop or a save past its wait at
    /// `now_ns`: one that appears later takes the next free cell.
    pub fn expire_landing(&mut self, now_ns: u64) {
        if self
            .landing
            .as_ref()
            .is_some_and(|landing| now_ns >= landing.until_ns)
        {
            self.landing = None;
        }
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
        let carried = self.carry.and_then(|carry| {
            let (tile, at) = (layout.cell_rect(0)?, carry.at?);
            Some((carry.index, Rect::new(at.x, at.y, tile.width, tile.height)))
        });
        Shown {
            standing,
            grid: layout,
            carried,
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
    /// is blurred once and kept until its name, the scale, the theme or the
    /// tile size moves, so a repaint lays it down rather than blurring it.
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
                let mut tile = Tiles::Large.tile(icon.entry, state, icon.kind, HiddenEndings::NONE);
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
                    && entry_parts(
                        Tiles::Large,
                        (entry, HiddenEndings::NONE),
                        rect,
                        scale,
                        theme,
                    )
                    .is_some_and(|parts| parts.contains(at))
            })
            .map(|(index, _)| index);
        hit
    }

    /// The composed control state of the icon at `index`: selected, hovered,
    /// and — when the desktop holds the keyboard and this is the selection —
    /// focused.
    fn icon_state(&self, index: usize) -> ControlState {
        let mut state = ControlState::idle();
        if self.selected == Some(index) {
            state.selection = SelectionState::Selected;
            if self.focused {
                state.focus = FocusState::FOCUSED;
            }
        }
        if self.hovered == Some(index) {
            state.pointer = PointerState::Hover;
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

    /// Pointer motion to screen position `at`, which drives the hover
    /// highlight. An icon being carried takes the pointer's attention, so
    /// nothing under it lights.
    pub fn pointer_moved(
        &mut self,
        at: Point,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.settle(layout);
        if self.carry.is_some_and(|carry| carry.at.is_some()) {
            return;
        }
        let hovered = self.icon_at(layout, at, scale, theme);
        if self.hovered != hovered {
            self.mark_icon(layout, self.hovered, damage);
            self.mark_icon(layout, hovered, damage);
            self.hovered = hovered;
        }
    }

    /// A primary press at screen position `at`, at monotonic time `now_ns`.
    ///
    /// A press on an icon selects it and arms the double-click engine, so a
    /// second press on the same icon within the shared window activates it;
    /// one that is not the second of a pair may go on to carry the icon
    /// ([`follow_press`](Self::follow_press)). A press on empty desktop — the ground
    /// around an icon's picture and name included — clears the selection.
    #[allow(clippy::too_many_arguments)] // The press, the field it lands on, and the round's report.
    pub fn press(
        &mut self,
        at: Point,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        now_ns: u64,
        apps: &[AppAssociation],
        damage: &mut Region,
    ) -> DesktopOutcome {
        self.settle(layout);
        self.carry = None;
        // Taking the keyboard puts the ring on whatever is selected, so the
        // old selection is repainted whether the press moves the selection or
        // merely claims focus.
        self.focused = true;
        self.mark_icon(layout, self.selected, damage);
        let Some(index) = self.icon_at(layout, at, scale, theme) else {
            self.selected = None;
            return DesktopOutcome::ignored();
        };
        self.selected = Some(index);
        self.mark_icon(layout, self.selected, damage);
        let subject = u64::try_from(index).unwrap_or(u64::MAX);
        if self.clicks.register(
            now_ns,
            subject,
            PointerButton::Primary,
            self.settings.double_click,
        ) == ClickKind::Double
        {
            return self.activate(index, apps);
        }
        let carry = self.shown(layout).rect(index).map(|rect| Carry {
            index,
            from: at,
            grab: (
                at.x.saturating_sub(rect.left()),
                at.y.saturating_sub(rect.top()),
            ),
            at: None,
        });
        self.carry = carry;
        DesktopOutcome::ignored()
    }

    /// Follow the press on an icon to `at`, the primary button still `held`
    /// or let go: carried there while it is held, and put down there once it
    /// is not. The seat applies a whole batch of input before any of it is
    /// routed, so the release may come with motion this has yet to follow.
    pub fn follow_press(
        &mut self,
        at: Point,
        held: bool,
        layout: &GridView,
        scale: Scale,
        damage: &mut Region,
    ) -> DesktopOutcome {
        if self.carry.is_none() {
            return DesktopOutcome::ignored();
        }
        self.carry_to(at, layout, scale, damage);
        if held {
            return DesktopOutcome::ignored();
        }
        self.put_down(layout, damage)
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
        self.mark_icon(layout, Some(carry.index), damage);
        let left = self.hovered.take();
        self.mark_icon(layout, left, damage);
        carry.at = Some(corner);
        self.carry = Some(carry);
        self.clicks.reset();
        self.mark_icon(layout, Some(carry.index), damage);
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
        let field = Field::of(layout);
        let (Some(corner), Some(field), Some(entry)) =
            (carry.at, field, self.entries.get(carry.index))
        else {
            self.carry = None;
            return DesktopOutcome::ignored();
        };
        let settled = if self.settings.align_to_grid {
            field
                .taken(&self.standing, Some(carry.index))
                .and_then(|mut taken| field.claim_nearest(corner, &mut taken))
                .unwrap_or(corner)
        } else {
            corner
        };
        let placed = Placed {
            id: entry.id(),
            at: field.place_of(settled, self.settings.icons),
            order: self.arrangement.next_order(),
        };
        let set = alloc::vec![(entry.name().to_string(), Some(placed))];
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
        if let Some(index) = on_icon {
            if self.selected != Some(index) {
                self.mark_icon(layout, self.selected, damage);
                self.selected = Some(index);
                self.mark_icon(layout, self.selected, damage);
            }
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
        sort_entries(&mut ordered, sort_mode(order.sort()));
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
            Key::Named(NamedKey::Escape) => {
                let cleared = self.selected.take();
                self.mark_icon(layout, cleared, damage);
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
            match self.selected.and_then(|at| Some((at, shown.rect(at)?))) {
                None => shown.bottom_up().map(|(index, _)| index).min(),
                Some(from) => nearest_toward(&shown, from, toward),
            }
        };
        let Some(next) = next.filter(|&next| self.selected != Some(next)) else {
            return DesktopOutcome::ignored();
        };
        self.mark_icon(layout, self.selected, damage);
        self.selected = Some(next);
        self.mark_icon(layout, self.selected, damage);
        DesktopOutcome::ignored()
    }

    /// Activate whatever is selected, if anything.
    ///
    /// The one definition of "open the selection", so the menu's `Open` row,
    /// the `Enter` key, and a double-click can never disagree about what
    /// opening an icon means.
    fn activate_selection(&self, apps: &[AppAssociation]) -> DesktopOutcome {
        match self.selected {
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
    /// shape. A shortcut whose target has gone is refused with its reason,
    /// never launched blind.
    fn activate(&self, index: usize, apps: &[AppAssociation]) -> DesktopOutcome {
        let Some(entry) = self.entries.get(index) else {
            return DesktopOutcome::ignored();
        };
        let path = self.path_of(entry.name());
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

    /// The absolute path of the child called `name` inside the desktop folder.
    fn path_of(&self, name: &str) -> String {
        let mut path = tairix_browse::vfs::spell_absolute_path(&self.folder);
        tairix_browse::vfs::push_child(&mut path, name);
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

/// Whether a file arriving as `arrived` is the one offered as `offered`: the
/// name itself, the name told apart from one already taken by what follows
/// its stem (`Report 2.txt`, `Report (2).txt`), or the name finished with a
/// file type by a volume that keeps none.
fn copied_as(arrived: &str, offered: &str) -> bool {
    let (made, asked) = (Ending::of(arrived), Ending::of(offered));
    let from_stem = made
        .stem
        .strip_prefix(asked.stem)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '));
    from_stem
        && made.extension == asked.extension
        && (made.filetype == asked.filetype || asked.filetype.is_none())
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
    /// The icon a press carries, and where its tile now stands.
    carried: Option<(usize, Rect)>,
}

impl Shown<'_> {
    /// Where the icon at `index` stands, if it is shown.
    fn rect(&self, index: usize) -> Option<Rect> {
        self.bottom_up()
            .find(|&(shown, _)| shown == index)
            .map(|(_, rect)| rect)
    }

    /// Whether the icon at `index` is the one carried, which stands apart.
    fn lifted(&self, index: usize) -> bool {
        self.carried.is_some_and(|(carried, _)| carried == index)
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
    /// the user put somewhere in the order they were put down, then the one
    /// carried.
    fn bottom_up(&self) -> impl DoubleEndedIterator<Item = (usize, Rect)> + '_ {
        self.flowed().chain(self.placed()).chain(self.carried)
    }

    /// The icons that may cover `at`, the topmost first: the one carried, those
    /// the user put somewhere, and the one the flow stood on the cell there, so
    /// a hit costs the icons placed and no more.
    fn under(&self, at: Point) -> impl Iterator<Item = (usize, Rect)> + '_ {
        let flowed = self.grid.index_at(DESKTOP_SCROLL, at).and_then(|cell| {
            let index = self
                .standing
                .cells
                .get(cell)?
                .filter(|&index| !self.lifted(index))?;
            Some((index, self.grid.cell_rect(cell)?))
        });
        self.carried
            .into_iter()
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
    /// or by a placed tile reaching into one, the icon at `aside` apart;
    /// `None` without the memory to say.
    fn taken(&self, standing: &Standing, aside: Option<usize>) -> Option<Vec<bool>> {
        let mut taken = tairix_util::fallible::filled(self.cells, false)?;
        for (held, on) in taken.iter_mut().zip(&standing.cells) {
            *held = on.is_some_and(|index| Some(index) != aside);
        }
        for &(index, rect) in &standing.placed {
            if Some(index) != aside {
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
