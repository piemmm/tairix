//! The desktop: the user's own `Desktop` folder shown as a column of icons
//! down the screen edge their settings name (`plans/NEW-TASKBAR.md` T7,
//! `plans/PINBOARD.md`).
//!
//! The desktop is a *directory view*, not a new kind of surface. It lists the
//! user's `Desktop` folder through the same [`DirectorySource`] seam the
//! trusted file picker uses, under the session's own identity; it orders the
//! listing with the shared [`sort_entries`]; it classifies each child with the
//! shared content-type registry; and it lays its tiles out with the shared
//! [`GridView`], differing from the file manager's grid only in its
//! [`GridFlow`] and [`GridFill`] — the desktop's column hugs the edge the
//! user's arrangement names, grows a new column inward as it fills, and keeps
//! a fixed pitch so an icon does not drift when the work area changes size.
//! There is no second grid, no second sort, and no second classifier anywhere
//! in this module.
//!
//! # The pinboard settings live here
//!
//! The desktop owns the user's [`DesktopSettings`] — the wallpaper and its
//! fit, the backdrop colour, the icon arrangement, and the sort order — as the
//! single copy inside the session: the shell reads them from the desktop
//! rather than holding a second set that could drift from the one the icons
//! are actually laid out by. An edit arrives through
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
//! The column follows its folder through a directory watch
//! (`docs/src/filesystem/watch.md`): whatever program changes it, the
//! embedder drains the report off the loop and hands the changed entries to
//! [`Desktop::apply_changes`], which merges them in place and repaints only
//! the cells they moved. The folder is read whole only at bring-up, when the
//! watch asks for a rescan, after an action of the session's own, and on the
//! user's Refresh.
//!
//! The model holds no authority: it *names* what should happen
//! ([`DesktopAction`]) and the embedder — which holds the spawn and
//! filesystem capabilities — carries it out.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_browse::render::{grid_metrics, grid_tile};
use tairix_browse::{
    applications_for, entry_icon_request, media_for_entry, merge_changes, sort_entries,
    suggest_new_dir_name, AppAssociation, DirectorySource, Entry, EntryChange, EntryKind, GridFlow,
    GridView, LinkTarget, Listing, SortDirection, SortKey, SortMode,
};
use tairix_controls::state::{ControlState, FocusState, PointerState, SelectionState};
use tairix_controls::IconTile;
use tairix_geometry::{GridFill, Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind, IconRequest, Landed};
use tairix_proglib::{Catalog, EntryId};
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_wallpaper::{DesktopSettings, IconFlow, IconSort};
use tairix_wm::{ClickKind, DoubleClickTracker, Key, NamedKey, PointerButton};

use crate::library::catalogued;
use crate::pinboard::PinboardCommand;

/// The inset, in logical pixels at the reference density, between the work
/// area's edges and the first icon.
///
/// A deliberate, fixed piece of visual spacing — not a capacity — so the
/// trailing column does not touch the screen edge and the top icon clears the
/// work area's top edge.
pub const DESKTOP_MARGIN: u32 = 8;

/// Where the desktop's icon grid rests: its anchored edge, always. The desktop
/// does not scroll; its icons stay where the user arranged them.
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
    /// Adopt these settings: persist them to the user's own store and hand
    /// them back through [`Desktop::apply_settings`], which reports the work
    /// the edit actually implies.
    ///
    /// The model names the new settings; it does not apply them itself, so
    /// there is exactly one place settings are adopted and the persisted
    /// document and the live desktop can never drift apart.
    AdoptSettings(DesktopSettings),
    /// Open the settings surface where the desktop picture is chosen,
    /// which is an installed application the embedder resolves and launches
    /// (the model knows no bundle paths).
    ChangeBackground,
    /// The gesture was refused. The line is complete and newline-terminated,
    /// ready for `stderr`: a refused action always says why rather than
    /// failing silently.
    Refuse(String),
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
    /// The appearance, contrast, density or motion changed: the embedder
    /// must re-theme the desktop and republish it, because every open
    /// application draws its own pixels and would otherwise be left in the
    /// appearance the user has just stopped asking for.
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
    /// icons' arrangement or order, or the theme or scale they are drawn at —
    /// so it must be repainted whole. Most of the settings document moves no
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
}

/// One icon the column shows, as the paint and the artwork landing see it.
struct ShownIcon<'a> {
    index: usize,
    entry: &'a Entry,
    /// The cell the tile draws strictly inside.
    bounds: Rect,
    kind: IconKind,
    request: IconRequest<'a>,
    /// The pixel side the picture resolves at.
    side: u32,
}

/// The outcome of one desktop command: whether it re-listed the folder, and
/// what (if anything) the session must now do.
///
/// What the gesture *changed on screen* is not here. Every gesture takes a
/// [`Region`] sink and adds the icon cells it altered to it, so the embedder
/// repaints those cells rather than the whole desktop layer: the desktop is
/// the bottom layer, and marking all of it recomposites every window above it
/// and throws away every frosted backdrop over it — a screenful of work to
/// move one highlight.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DesktopOutcome {
    /// The command re-listed the folder on the spot and its contents had
    /// changed, so the whole column moved. A re-list read elsewhere lands
    /// through [`Desktop::resume`] instead.
    pub relisted: bool,
    /// The action the gesture asks for, if any.
    pub action: Option<DesktopAction>,
}

impl DesktopOutcome {
    /// The gesture asks for nothing.
    #[must_use]
    pub const fn ignored() -> Self {
        Self {
            relisted: false,
            action: None,
        }
    }

    /// The gesture asks for `action`.
    #[must_use]
    pub const fn acting(action: DesktopAction) -> Self {
        Self {
            relisted: false,
            action: Some(action),
        }
    }
}

/// The desktop's icon column: the listing, what the pointer and keyboard are
/// doing to it, and the pure double-click engine it shares with the file
/// manager.
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
    selected: Option<usize>,
    hovered: Option<usize>,
    focused: bool,
    clicks: DoubleClickTracker,
    /// Whether a source that reads elsewhere still owes the listing the last
    /// [`relist`](Self::relist) asked for.
    listing_owed: bool,
}

impl<S: DirectorySource> Desktop<S> {
    /// A desktop over `source` showing the folder named by the root-first
    /// `folder` components, on the default pinboard settings. Nothing is
    /// listed until [`relist`](Self::relist).
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
            selected: None,
            hovered: None,
            focused: false,
            clicks: DoubleClickTracker::new(),
            listing_owed: false,
        }
    }

    /// The entries currently shown, in the shared listing order.
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
    /// leaf name, never from the link's, so `Chess` → `/Apps/chess.app` is an
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
        let chosen = match catalogued(catalog, entry) {
            Ok(chosen) => chosen,
            Err(reason) => return DesktopAction::Refuse(reason),
        };
        let name = chosen.name().as_str();
        if let Err(err) = tairix_path::validate_file_name(name) {
            return DesktopAction::Refuse(format!(
                "desktop: '{name}' cannot be a shortcut name ({err})\n"
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
    /// actually asks for it. The desktop applies nothing beyond its own state.
    pub fn apply_settings(&mut self, settings: DesktopSettings) -> Option<PinboardChange> {
        if settings == self.settings {
            return None;
        }
        let relayout = settings.icons != self.settings.icons;
        let resorted = settings.sort != self.settings.sort;
        let appearance = AppearanceWork {
            theme: settings.appearance != self.settings.appearance
                || settings.contrast != self.settings.contrast
                || settings.density != self.settings.density
                || settings.motion != self.settings.motion,
            scale: settings.scale != self.settings.scale,
            cursor: settings.cursor_set != self.settings.cursor_set
                || settings.cursor_size != self.settings.cursor_size
                || settings.cursor_shadow != self.settings.cursor_shadow,
        };
        let change = PinboardChange {
            layer: relayout
                || resorted
                || appearance.theme
                || appearance.scale
                || settings.backdrop != self.settings.backdrop,
            backdrop: BackdropWork {
                relayout,
                wallpaper: settings.wallpaper != self.settings.wallpaper
                    || settings.fit != self.settings.fit,
            },
            appearance,
            notifications: settings.notifications != self.settings.notifications,
        };
        self.settings = settings;
        if resorted {
            let chosen = self.selected_name();
            sort_entries(&mut self.entries, sort_mode(self.settings.sort));
            self.follow_name(chosen);
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
    /// keyboard changes that one cell and nothing else — and with nothing
    /// selected it changes no pixel at all. This is the click that moves
    /// focus between the desktop and a window, which is exactly when the
    /// screen must *not* be repainted wholesale.
    pub fn set_focused(&mut self, focused: bool, layout: &GridView, damage: &mut Region) {
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        Self::mark_cell(layout, self.selected, damage);
    }

    /// Add to `damage` the cell of every shown icon whose picture the
    /// `landed` decodes moved, with `layout`, `scale` and `theme` the ones the
    /// column is painted at.
    ///
    /// A decode can only change the picture inside the tiles that draw through
    /// it, never the backdrop or another icon, so a batch that pictures none of
    /// the column damages nothing.
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

    /// Add the part of the cell the icon at `index` occupies that shows to
    /// `damage`.
    ///
    /// The one place an icon's footprint is spelled: a tile draws strictly
    /// inside the cell the shared grid gives it, so repainting that rectangle
    /// is the whole of repainting the icon. An index the column does not
    /// currently show has no cell and damages nothing.
    fn mark_cell(layout: &GridView, index: Option<usize>, damage: &mut Region) {
        if let Some(rect) = index.and_then(|index| shown_whole(layout, index)) {
            damage.add(rect);
        }
    }

    /// Note that the pointer is somewhere other than the desktop (over a
    /// window, the taskbar, or one of its popovers), clearing the hover.
    pub fn pointer_left(&mut self, layout: &GridView, damage: &mut Region) {
        Self::mark_cell(layout, self.hovered.take(), damage);
    }

    /// Ask for a fresh listing of the folder now: the caller knows the folder
    /// must be read whole (bring-up, a rescan its watch asked for, or an
    /// action the session itself performed on it). Returns whether the shown
    /// set changed.
    ///
    /// A listing the source refuses leaves the desktop empty and selects
    /// nothing rather than showing a stale or guessed folder.
    ///
    /// A source that reads the folder elsewhere answers "not yet", and this
    /// changes nothing at all — the icons already on screen stay there, and the
    /// caller hands the answer over with [`resume`](Self::resume) on the wake
    /// that says the read finished. Blanking the column while a read is in
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
        if !self.listing_owed {
            return false;
        }
        let listed = self.source.list(&self.folder);
        self.adopt_listing(listed)
    }

    /// Show what the source answered, noting whether it still owes an answer.
    fn adopt_listing(&mut self, listed: Result<Listing, Errno>) -> bool {
        self.listing_owed = matches!(listed, Ok(Listing::Pending));
        let mut entries = match listed {
            Ok(Listing::Ready(entries)) => entries,
            Ok(Listing::Pending) => return false,
            Err(_) => Vec::new(),
        };
        sort_entries(&mut entries, sort_mode(self.settings.sort));
        if entries.len() == self.entries.len()
            && entries
                .iter()
                .zip(&self.entries)
                .all(|(a, b)| a.same_listing(b))
        {
            return false;
        }
        let chosen = self.selected_name();
        self.entries = entries;
        self.follow_name(chosen);
        true
    }

    fn selected_name(&self) -> Option<String> {
        self.selected
            .and_then(|index| self.entries.get(index))
            .map(|entry| entry.name().to_string())
    }

    /// Put the selection back on the icon named `chosen` after the column was
    /// reordered or replaced, so it never lands on a different icon under the
    /// user's pointer. The hover and a half-made double-click go with the
    /// order, since the cell under them may now show another icon.
    fn follow_name(&mut self, chosen: Option<String>) {
        self.selected = chosen.and_then(|name| {
            self.entries
                .iter()
                .position(|entry| entry.name() == name.as_str())
        });
        self.hovered = None;
        self.clicks.reset();
    }

    /// Merge the entries the folder's watch reported into the column in
    /// place, adding to `damage` each cell whose icon or highlight changed.
    /// The selection stays on the icon it named and goes with one whose file
    /// went, and a merge the memory could not be had for re-lists the folder
    /// instead. Answers whether any icon moved.
    ///
    /// `layout` lays a desktop out as [`layout`](Self::layout) does, so the
    /// cells are measured on both sides of the merge.
    pub fn apply_changes(
        &mut self,
        changes: Vec<EntryChange>,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
    ) -> bool {
        self.marking(layout, damage, |desktop| {
            let merged = merge_changes(
                &mut desktop.entries,
                changes,
                sort_mode(desktop.settings.sort),
            );
            // Without the memory to merge them, the folder is read again.
            let Some((placement, moved)) = merged else {
                return desktop.relist();
            };
            if !moved {
                return false;
            }
            desktop.selected = desktop.selected.and_then(|index| placement.place(index));
            // A hover is the pointer's cell, so it stays only while that cell
            // still shows the icon it did.
            desktop.hovered = desktop
                .hovered
                .filter(|&index| placement.place(index) == Some(index));
            desktop.clicks.follow(|at| placement.place_subject(at));
            true
        })
    }

    /// [`relist`](Self::relist), adding to `damage` each cell whose icon or
    /// highlight the new listing changed rather than answering for the whole
    /// layer.
    pub fn relist_into(&mut self, layout: impl Fn(&Self) -> GridView, damage: &mut Region) -> bool {
        self.marking(layout, damage, Self::relist)
    }

    /// [`resume`](Self::resume), adding to `damage` each cell whose icon or
    /// highlight the landed listing changed.
    ///
    /// A wake that owes the desktop no listing costs it nothing, rather than a
    /// snapshot of every shown icon to compare against.
    pub fn resume_into(&mut self, layout: impl Fn(&Self) -> GridView, damage: &mut Region) -> bool {
        self.listing_owed && self.marking(layout, damage, Self::resume)
    }

    /// Whether the folder's listing follows it, so a change the session made
    /// there arrives as a reported change and needs no re-list.
    #[must_use]
    pub fn follows(&self) -> bool {
        self.source.follows(&self.folder)
    }

    /// Answer what `change` does to the desktop, adding to `damage` each cell
    /// shown before or after it whose icon or highlight it changed: only the
    /// cells the column shows are compared, so the cost is bounded by the
    /// screen rather than by the folder.
    fn marking(
        &mut self,
        layout: impl Fn(&Self) -> GridView,
        damage: &mut Region,
        change: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        let before = layout(self);
        let shown = before.visible_range(DESKTOP_SCROLL);
        let was: Vec<(Entry, ControlState)> = shown
            .clone()
            .map_while(|index| {
                let entry = self.entries.get(index)?;
                Some((entry.clone(), self.icon_state(index)))
            })
            .collect();
        if !change(self) {
            return false;
        }
        let after = layout(self);
        let end = shown.end.max(after.visible_range(DESKTOP_SCROLL).end);
        for index in shown.start..end {
            let old = was.get(index - shown.start);
            let new = self
                .entries
                .get(index)
                .map(|entry| (entry, self.icon_state(index)));
            if old.map(|(entry, state)| (entry, *state)) != new {
                let cell = shown_whole(&after, index).or_else(|| shown_whole(&before, index));
                if let Some(rect) = cell {
                    damage.add(rect);
                }
            }
        }
        true
    }

    /// The grid the desktop's icons are laid out in: the shared tile geometry
    /// under the column flow the settings' arrangement names, inset from
    /// `work_area` by [`DESKTOP_MARGIN`].
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
        // The field is fixed, not resizable: keeping the pitch anchored to the
        // edge the icons hug means an icon stays where the user last saw it
        // whatever the work area's exact extent is, rather than drifting as the
        // file manager's spreading grid deliberately does.
        GridView::new(
            viewport,
            grid_metrics(scale, theme),
            0,
            self.entries.len(),
            grid_flow(self.settings.icons),
            GridFill::FixedPitch,
        )
    }

    /// Paint the visible icons that fall inside `area` into `surface` through
    /// the shared icon tile, resolving each one's artwork from `artwork` at
    /// exactly the slot side the tile will draw it in.
    ///
    /// Only the icons the column actually shows are painted and only their
    /// artwork is asked for, so a folder with more icons than fit costs
    /// nothing for the ones off screen. `area` narrows that again to the
    /// rectangle being repainted, so moving a highlight costs the cells that
    /// changed rather than every icon on screen — a tile draws strictly inside
    /// its own cell, so a cell `area` misses has nothing in `area` to draw.
    /// An application bundle on the desktop names itself in its request, so it
    /// draws the icon it carries in its own `Resources/` rather than the
    /// generic bundle picture. An icon whose artwork the lookup declines falls
    /// back to the shared class glyph inside the tile, so a system with no
    /// `/System/Graphics` still shows a meaningful desktop.
    pub fn render(
        &self,
        surface: &mut Surface,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        artwork: &mut dyn IconArtwork,
        area: Rect,
    ) {
        self.visit_icons(
            layout,
            scale,
            theme,
            |bounds| !bounds.intersection(&area).is_empty(),
            |icon| {
                let tile = grid_tile(icon.entry, self.icon_state(icon.index), icon.kind);
                let art = artwork.artwork(icon.request, icon.side);
                tile.render(surface, icon.bounds, scale, theme, art);
            },
        );
    }

    /// Visit every shown icon whose cell `wanted` admits.
    ///
    /// The paint and the artwork landing both walk the column here, so the
    /// icons a landing repaints are exactly the ones the paint draws from it.
    fn visit_icons(
        &self,
        layout: &GridView,
        scale: Scale,
        theme: &Theme,
        wanted: impl Fn(Rect) -> bool,
        mut visit: impl FnMut(ShownIcon<'_>),
    ) {
        // Spelled once for the whole walk; a bundle icon appends its own leaf
        // into this one buffer rather than allocating a path per tile.
        let dir = tairix_browse::vfs::spell_absolute_path(&self.folder);
        let mut bundle = String::new();
        for index in layout.visible_range(DESKTOP_SCROLL) {
            let Some(entry) = self.entries.get(index) else {
                break;
            };
            let Some(bounds) = shown_whole(layout, index).filter(|&bounds| wanted(bounds)) else {
                continue;
            };
            let kind = media_for_entry(entry, &self.folder).icon();
            visit(ShownIcon {
                index,
                entry,
                bounds,
                kind,
                side: IconTile::icon_side(bounds, scale, theme),
                request: entry_icon_request(&dir, entry, kind, &mut bundle),
            });
        }
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
        state
    }

    /// Pointer motion to screen position `at`, which drives the hover
    /// highlight.
    pub fn pointer_moved(&mut self, at: Point, layout: &GridView, damage: &mut Region) {
        let hovered = index_at(layout, at);
        if self.hovered != hovered {
            Self::mark_cell(layout, self.hovered, damage);
            Self::mark_cell(layout, hovered, damage);
            self.hovered = hovered;
        }
    }

    /// A primary press at screen position `at`, at monotonic time `now_ns`.
    ///
    /// A press on an icon selects it and arms the double-click engine, so a
    /// second press on the same icon within the shared window activates it. A
    /// press on empty desktop clears the selection.
    pub fn press(
        &mut self,
        at: Point,
        layout: &GridView,
        now_ns: u64,
        apps: &[AppAssociation],
        damage: &mut Region,
    ) -> DesktopOutcome {
        // Taking the keyboard puts the ring on whatever is selected, so the
        // old selection's cell is repainted whether the press moves the
        // selection or merely claims focus.
        self.focused = true;
        Self::mark_cell(layout, self.selected, damage);
        let Some(index) = index_at(layout, at) else {
            self.selected = None;
            return DesktopOutcome::ignored();
        };
        self.selected = Some(index);
        Self::mark_cell(layout, self.selected, damage);
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
    pub fn context_press(&mut self, at: Point, layout: &GridView, damage: &mut Region) -> bool {
        let on_icon = index_at(layout, at);
        if let Some(index) = on_icon {
            if self.selected != Some(index) {
                Self::mark_cell(layout, self.selected, damage);
                self.selected = Some(index);
                Self::mark_cell(layout, self.selected, damage);
            }
        }
        on_icon.is_some()
    }

    /// Resolve one pinboard menu `command` against the desktop's own state.
    ///
    /// This is the single translation from a named command to a
    /// [`DesktopAction`]: `Open` resolves through the very same activation the
    /// double-click path uses, a sort or arrangement row names the settings the
    /// embedder is to adopt (never applying them behind its back), a new folder
    /// is named through the shared new-directory naming over the listing on
    /// screen, and `Refresh` re-lists here and now. A command that asks for
    /// what is already in force changes nothing.
    pub fn command(&mut self, command: PinboardCommand, apps: &[AppAssociation]) -> DesktopOutcome {
        match command {
            PinboardCommand::Open => self.activate_selection(apps),
            PinboardCommand::NewFolder => DesktopOutcome::acting(DesktopAction::CreateFolder {
                path: self.path_of(&suggest_new_dir_name(&self.entries)),
            }),
            PinboardCommand::SortBy(sort) => self.adopt(DesktopSettings {
                sort,
                ..self.settings.clone()
            }),
            PinboardCommand::ArrangeFrom(icons) => self.adopt(DesktopSettings {
                icons,
                ..self.settings.clone()
            }),
            PinboardCommand::Refresh => DesktopOutcome {
                relisted: self.relist(),
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

    /// Name the settings edit `next` for the embedder to adopt, or change
    /// nothing when it asks for the settings already in force.
    fn adopt(&self, next: DesktopSettings) -> DesktopOutcome {
        if next == self.settings {
            return DesktopOutcome::ignored();
        }
        DesktopOutcome::acting(DesktopAction::AdoptSettings(next))
    }

    /// A key while the desktop holds the keyboard: the arrows move the
    /// selection, `Enter` activates it, and `Escape` clears it.
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
        match key {
            Key::Named(NamedKey::Enter) => self.activate_selection(apps),
            Key::Named(NamedKey::Escape) => {
                Self::mark_cell(layout, self.selected.take(), damage);
                DesktopOutcome::ignored()
            }
            Key::Named(named) => match Step::for_key(named, self.settings.icons) {
                Some(step) => self.move_selection(step, layout, damage),
                None => DesktopOutcome::ignored(),
            },
            Key::Char(_) => DesktopOutcome::ignored(),
        }
    }

    /// Move the selection one `step` along the listing, clamped to its ends.
    /// With nothing selected the first arrow selects the first icon, so the
    /// keyboard always has somewhere to start.
    fn move_selection(
        &mut self,
        step: Step,
        layout: &GridView,
        damage: &mut Region,
    ) -> DesktopOutcome {
        if self.entries.is_empty() {
            return DesktopOutcome::ignored();
        }
        let last = self.entries.len().saturating_sub(1);
        let next = match self.selected {
            None => 0,
            Some(current) => step.applied(current, layout.cells_per_line()).min(last),
        };
        if self.selected == Some(next) {
            return DesktopOutcome::ignored();
        }
        Self::mark_cell(layout, self.selected, damage);
        self.selected = Some(next);
        Self::mark_cell(layout, self.selected, damage);
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
    /// launches; a plain file launches the application the shared association
    /// model picks for it, opening the file. A file nothing is
    /// associated with is refused with a stated reason and nothing else
    /// happens.
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
                    return DesktopOutcome::acting(DesktopAction::Refuse(format!(
                        "desktop: the shortcut '{}' names nothing\n",
                        entry.name()
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
            EntryKind::File | EntryKind::Link(LinkTarget::File) => {
                match applications_for(entry.name(), apps).first() {
                    Some(app) => DesktopOutcome::acting(DesktopAction::Activate(launch_of(
                        app.bundle_path(),
                        app.name().to_string(),
                        Some(LaunchDocument {
                            path,
                            edits: app.writes_documents(),
                        }),
                    ))),
                    None => DesktopOutcome::acting(DesktopAction::Refuse(format!(
                        "desktop: no installed application opens '{}'\n",
                        entry.name()
                    ))),
                }
            }
            // A shortcut whose target cannot be reached: reported, never
            // launched or opened on the chance that it works.
            EntryKind::Link(LinkTarget::Dangling) => {
                DesktopOutcome::acting(DesktopAction::Refuse(format!(
                    "desktop: the shortcut '{}' points at something that is not there\n",
                    entry.name()
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

/// One arrow-key move over the icon column.
///
/// The desktop's icons flow *down* a column before wrapping, so up/down is one
/// icon while right/left is one whole column — and "one column" is however
/// many icons the live grid fits, never a number this module guesses.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Step {
    /// One icon further down the listing.
    NextIcon,
    /// One icon back up the listing.
    PreviousIcon,
    /// One whole column later in the listing.
    NextColumn,
    /// One whole column earlier in the listing.
    PreviousColumn,
}

impl Step {
    /// The step an arrow key asks for under the `flow` the icons are arranged
    /// in, or `None` when the key means nothing to the desktop.
    ///
    /// Which horizontal arrow runs *later* into the listing is a property of
    /// the arrangement, not a constant: columns grow rightward from the
    /// leading edge and leftward from the trailing one, so the mapping is read
    /// off the live arrangement. Otherwise one of the two arrangements would
    /// move the selection the opposite way to the icons the user can see.
    const fn for_key(key: NamedKey, flow: IconFlow) -> Option<Self> {
        let rightward_is_later = matches!(flow, IconFlow::Leading);
        match key {
            NamedKey::Down => Some(Self::NextIcon),
            NamedKey::Up => Some(Self::PreviousIcon),
            NamedKey::Right if rightward_is_later => Some(Self::NextColumn),
            NamedKey::Right => Some(Self::PreviousColumn),
            NamedKey::Left if rightward_is_later => Some(Self::PreviousColumn),
            NamedKey::Left => Some(Self::NextColumn),
            _ => None,
        }
    }

    /// This step applied to the icon at `current` in a column holding
    /// `per_column` icons, saturating at the start of the listing.
    fn applied(self, current: usize, per_column: usize) -> usize {
        let column = per_column.max(1);
        match self {
            Self::NextIcon => current.saturating_add(1),
            Self::PreviousIcon => current.saturating_sub(1),
            Self::NextColumn => current.saturating_add(column),
            Self::PreviousColumn => current.saturating_sub(column),
        }
    }
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
        direction: SortDirection::Ascending,
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

/// The icon at screen position `at`, through the shared grid hit-test.
fn index_at(layout: &GridView, at: Point) -> Option<usize> {
    let index = layout.index_at(DESKTOP_SCROLL, at)?;
    shown_whole(layout, index).map(|_| index)
}

/// Where the icon at `index` shows on screen, when the field shows it whole.
///
/// The desktop never scrolls, so a column its edge would cut could never be
/// brought into view: it is left out, like a tile a line cannot hold, rather
/// than drawn cut.
fn shown_whole(layout: &GridView, index: usize) -> Option<Rect> {
    let cell = layout.cell_rect(index)?;
    layout
        .shown_rect(DESKTOP_SCROLL, index)
        .filter(|shown| shown.width == cell.width && shown.height == cell.height)
}
