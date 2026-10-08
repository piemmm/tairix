//! TAIRiX **shared directory-browser engine** (`plans/APPWIN.md` AW5).
//!
//! The one navigation **model** plus themed **renderer** every directory
//! browser in the system composes: the `files.app` bundle's windowed file
//! manager and the desktop session's **trusted file picker**
//! (`plans/CAPABILITY_USE.md` CU6) both drive exactly this engine, so the
//! two views can never diverge in navigation semantics, listing policy, or
//! look. Both are driven through an injected [`DirectorySource`]:
//!
//! * [`Browser`] holds the current directory's path and entries and a
//!   selection cursor. Descending, climbing to the parent, and refreshing
//!   are transactional and fail closed: the new directory is listed
//!   *before* any state changes, so a refused or failing read leaves the
//!   browser exactly where it was.
//! * [`render_into()`] paints the toolbar and the (scrolling) entry list into
//!   a caller-owned `lib/raster` [`Surface`](tairix_raster::Surface) using the
//!   active theme's palette and the shared `lib/font` face — the same surface
//!   the compositor places and rounds.
//!
//! # No `/proc`, no fabrication
//!
//! TAIRiX has no `/proc` and no `/sys`. The browser shows exactly the
//! entries its [`DirectorySource`] returns — it never injects a synthetic
//! entry — and it makes no permission decision of its own: the check and
//! the path policy live in the VFS behind the source, under the identity
//! of whichever process composes the engine (the files app's own, the
//! session's for the picker). A directory the caller may not read surfaces
//! a [`BrowseError`] rather than a partial or guessed listing.
//!
//! # Module map
//!
//! * [`entry`] — the [`Entry`]/[`EntryKind`] listing vocabulary, including
//!   the [`LinkTarget`] a symbolic link resolves to and the
//!   [`resolve_target`] rule a relative one is reached through.
//! * [`activate`](mod@activate) — the [`Activation`] dispatch-by-kind decision
//!   (descend / launch a bundle / open a file) the manager and picker share.
//! * [`chrome`](mod@chrome) — the file-manager frame model: the [`ToolbarModel`]
//!   command enable/pressed state, the [`ContextMenuModel`] right-click command
//!   enable state, and the manager-only [`ManagerTool`]
//!   write-tool vocabulary (with the [`ManagerToolModel`] enable state) the
//!   read-only picker never composes.
//! * [`select`](mod@select) — the [`Selection`] multi-entry set (single /
//!   toggle / range / select-all) the management verbs act on.
//! * [`clipboard`](mod@clipboard) — the cut/copy [`Clipboard`] and
//!   [`plan_paste`] paste-target validation (`plans/NEW-FILEMANAGER.md` FM7).
//! * [`delete`](mod@delete) — the delete model: the [`DeletePlan`] naming what
//!   the Delete verb would remove, captured from the selection, and the
//!   [`DeleteWalk`] driven cursor that carries the recursive removal out
//!   depth-first, bounded and interruptible (`plans/NEW-FILEMANAGER.md` `FM7b`).
//! * [`execute`](mod@execute) — the pure paste-execution model: the
//!   [`paste_strategy`] move-vs-copy volume decision, the bounded, resumable
//!   [`CopyCursor`] streaming single-file copy, and the depth-first,
//!   depth-bounded [`CopyWalk`] recursive directory-copy cursor the management
//!   verbs run (`plans/NEW-FILEMANAGER.md` `FM7b`).
//! * [`media`](mod@media) — the one closed content-type [`MediaType`] registry
//!   the manager and picker share: it drives both the file-type
//!   [`IconKind`](tairix_icon::IconKind) glyph and the "Open With…" association
//!   vocabulary (a display hint, never authority), and names each type's
//!   broader type ([`MediaType::parent`]) so association matching can widen.
//! * [`open_with`](mod@open_with) — the type→bundle "Open With…" association
//!   model ([`applications_for`]) over the installed applications' declared
//!   associations, resolving a file's type through [`media`](mod@media) and
//!   matching along its subclass chain, most specific declaration first.
//! * [`sort`](mod@sort) — the [`SortMode`] and the one shared listing order.
//! * [`trash`](mod@trash) — the recoverable-delete model: the
//!   [`trash_strategy`] same-volume move-vs-unlink decision, the
//!   collision-safe [`trash_dest_path`] destination naming
//!   (`plans/NEW-FILEMANAGER.md` `FM10`), and the [`empty_trash_plan`]
//!   permanent empty-Trash model (`FM11`).
//! * [`error`] — [`BrowseError`], the fail-closed navigation outcomes.
//! * [`source`] — the [`DirectorySource`] seam.
//! * [`browser`] — the [`Browser`] navigation model.
//! * [`layout`](mod@layout) — the [`ListView`]/[`GridView`] item-view geometry
//!   and the [`ViewLayout`] dispatch (the one visible-window/item-rect/hit-test
//!   definition the renderer and the pointer hit-test share), plus [`ViewMode`],
//!   the [`GridFlow`] that also lays the desktop's trailing-edge icon column out
//!   of the very same grid, and the [`GridFill`](tairix_geometry::GridFill) policy that decides what a line
//!   does with the space it has left over.
//! * [`marquee`](mod@marquee) — the [`Marquee`] band dragged across the
//!   listing's ground, which selects every entry it touches as it grows and
//!   scrolls the listing while held at an end.
//! * [`format`](mod@format) — the size/date column formatting and the
//!   properties view's date-and-time spelling.
//! * [`properties`](mod@properties) — the [`Properties`] view model: the
//!   display-ready summary of a node's `fs_stat` metadata the file manager's
//!   Properties panel shows (`plans/NEW-FILEMANAGER.md` FM8).
//! * [`create`](mod@create) — the New ▸ model: what a new folder or blank
//!   document is first called ([`NewEntry`]) and whether a name may be created
//!   ([`CreateError`]/[`validate_new_entry_name`]).
//! * [`mode_edit`](mod@mode_edit) — the [`ModeError`]/[`validate_mode`]
//!   permission-change model committed through the `fs_set_mode` seam
//!   (`plans/NEW-FILEMANAGER.md` `FM8b`).
//! * [`owner_edit`](mod@owner_edit) — the [`OwnerError`]/[`validate_owner`]
//!   ownership-change model ([`OwnerChange`]) committed through the privileged
//!   `fs_set_owner` seam (`plans/NEW-FILEMANAGER.md` `FM8b`).
//! * [`progress`](mod@progress) — the [`ProgressModel`] progress + latched-cancel
//!   state of a long delete/copy the file manager drives interleaved with its
//!   event loop (`plans/NEW-FILEMANAGER.md` `FM7b`).
//! * [`places`](mod@places) — the places/devices rail model: the user's fixed
//!   shortcuts plus the mounted volumes, each carrying the storage medium it
//!   really sits on, validated and ordered without touching the filesystem.
//! * [`rename`](mod@rename) — the in-place [`RenameError`]/[`validate_new_name`]
//!   rename model the file manager's first write operation is built on.
//! * [`column`](mod@column) — the [`ScrollColumn`] every scrolling surface
//!   holds: its offset in pixels and the bar that draws it, moved by the wheel
//!   and the bar's own pointer routing through one definition.
//! * [`rowlist`](mod@rowlist) — the [`RowList`] cursor the *Open With…*
//!   chooser and the Properties window's attribute list share.
//! * [`render`](mod@render) — painting the current directory into a
//!   `Surface`.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`); the only dependencies are the audited
//! `lib/abi` ABI crate and the shared `lib/*` desktop libraries, so this
//! engine never links a kernel, driver, or window-manager crate. No
//! `unsafe`, and no `unwrap`/`expect`/`panic!` in production paths.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod activate;
pub mod browser;
pub mod chrome;
pub mod clipboard;
pub mod column;
pub mod create;
pub mod delete;
pub mod desk;
pub mod document;
pub mod entry;
pub mod error;
pub mod execute;
pub mod format;
pub mod layout;
pub mod marquee;
pub mod media;
pub mod mode_edit;
pub mod open_with;
pub mod owner_edit;
pub mod places;
pub mod probes;
pub mod progress;
pub mod properties;
pub mod rename;
pub mod render;
pub mod rowlist;
pub mod select;
pub mod sort;
pub mod source;
pub mod trash;
pub mod vfs;
pub mod watch;

pub use activate::{Activation, BundleIntent};
pub use browser::Browser;
pub use chrome::{
    apply_command, context_choice_from_item, context_menu, ContextChoice, ContextCommand,
    ContextMenuModel, ContextQuick, ManagerTool, ManagerToolModel, ToolbarBand, ToolbarCommand,
    ToolbarModel, CONTEXT_COMMANDS, MANAGER_TOOLS, TOOLBAR_COMMANDS,
};
pub use clipboard::{
    drop_operation, plan_paste, Clipboard, ClipboardOp, PasteError, PasteItem, PastePlan,
};
pub use column::ScrollColumn;
pub use create::{validate_new_entry_name, CreateError, NewEntry, NEW_FOLDER_BASE};
pub use delete::{
    DeleteAction, DeleteError, DeletePlan, DeleteTarget, DeleteWalk, MAX_DELETE_DEPTH,
};
pub use desk::{ListingDesk, ListingJob};
pub use entry::{
    is_bundle_name, resolve_target, Entry, EntryKind, LinkResolution, LinkTarget, Occupancy,
};
pub use error::BrowseError;
pub use execute::{
    paste_strategy, CopyAction, CopyChunk, CopyCursor, CopyError, CopyKind, CopyWalk,
    CopyWalkError, PasteStrategy, VolumeId, COPY_CHUNK_LEN, MAX_COPY_DEPTH,
};
pub use format::{format_date, format_datetime, format_size};
pub use layout::{GridFlow, GridMetrics, GridView, ListView, SidebarView, ViewLayout, ViewMode};
pub use marquee::Marquee;
pub use media::{
    entry_icon, folder_sample, icon_for_entry, media_for_entry, media_for_name, BlankDocument,
    Ending, Family, MediaType,
};
pub use mode_edit::{validate_mode, ModeError};
pub use open_with::{
    applications_for, association_from_manifest, blank_documents, quick_applications,
    AppAssociation, OpenWithCandidate, OpenWithChooser,
};
pub use owner_edit::{validate_owner, OwnerChange, OwnerError};
pub use places::{
    bare_open_places, Place, PlaceKind, Places, Volume, FIXED_LABELS, MAX_PLACE_LABEL,
};
pub use probes::Probes;
pub use progress::{ProgressModel, ProgressOp};
pub use properties::{Attribute, Attributes, Properties};
pub use rename::{rename_selection, validate_new_name, RenameError};
pub use render::{render_into, ManagerChrome};
pub use rowlist::RowList;
pub use select::Selection;
pub use sort::{sort_entries, SortDirection, SortKey, SortMode};
pub use source::{resolve_occupancy, DirectorySource, Listing, Probe};
pub use tairix_abi::window_ipc::WindowSizing;
use tairix_geometry::Scale;
/// The pointer button a consumer names when it reports a press. Re-exported
/// because it is part of this engine's own surface and should not need a
/// dependency of its own. The double-click rule those presses pair under is
/// `tairix_input`'s, not this engine's (`plans/NEW-FILEMANAGER.md` `FM12`).
pub use tairix_input::PointerButton;
/// The one shared path-spelling rules a consumer of this engine needs beside
/// it: the final component of a path, and the `parent`/`name` join. Re-exported
/// rather than re-implemented so a surface that spells a resolved link target
/// uses the same rule the engine does.
pub use tairix_path::{join as join_child, leaf_name};
use tairix_theme::{SurfaceGround, Theme};
pub use trash::{
    empty_trash_plan, trash_dest_path, trash_dir, trash_strategy, DeleteDisposition, TrashError,
    TrashStrategy, MAX_TRASH_NAME_ATTEMPTS,
};
#[cfg(feature = "rt")]
pub use vfs::RtLinkReader;
pub use vfs::{LinkInfo, LinkReader, NoLinks, NoProbe, VfsDirectorySource};
pub use watch::{
    merge_changes, EntryChange, Placement, Took, WatchUpdate, Watches, WATCH_BUFFER_LEN,
    WATCH_LATENCY_NS,
};
#[cfg(feature = "rt")]
pub use watch::{WatchedDirectory, WatchedListing};

/// Window content width of a browser view, in logical pixels at the
/// reference density, resolved through the desktop's own scale — the one
/// definition
/// the files app's `Run` binary and the session's trusted picker size
/// their windows with, and the QEMU vertical's host-side scan-out
/// assertion measures against (`plans/APPWIN.md` AW3/AW5).
pub const WIN_WIDTH: u32 = 480;

/// Window content height of a browser view, in logical pixels (see
/// [`WIN_WIDTH`]); the window is resizable, so a user may grow it further.
pub const WIN_HEIGHT: u32 = 480;

/// The smallest client width, in logical pixels, a *listing* still reads at.
///
/// The floor a window declares is the larger of this and what its own
/// command toolbar needs ([`win_floor_width`]); below that the shared toolbar
/// would have to scroll, and a browser view rebuilds its strip per frame so
/// it holds no offset to scroll with.
const MIN_LISTING_WIDTH: u32 = 240;

/// The smallest client height a Properties window declares, in logical
/// pixels: its identity band and section strip, with a body beneath them that
/// scrolls.
const PROPERTIES_MIN_HEIGHT: u32 = 160;

/// The narrowest client a file-manager window declares at `scale`: the larger
/// of what a listing still reads at and what the command toolbar needs.
///
/// The floor the window manager holds an interactive resize to. The app never
/// re-imposes it — it lays out at whatever size it is given, and the content
/// clips gracefully below its natural size. It is derived rather than
/// hand-picked, because the toolbar's own tools are what set it and a denser
/// theme or a larger scale changes what they need. Measuring it builds the
/// toolbar, so a window derives it when its scale or theme moves, not per
/// frame. The ABI field is *physical*, so the logical floor is resolved here.
#[must_use]
pub fn win_floor_width(scale: Scale, theme: &Theme) -> u32 {
    scale
        .scale_length(MIN_LISTING_WIDTH)
        .max(render::toolbar_natural_width(scale, theme))
}

/// The sizing a browser window showing `view` under `toolbar` declares at
/// `scale`: resizable, down to `width` — its [`win_floor_width`] — and to the
/// height that still shows one whole row of its listing
/// ([`render::listing_floor_height`]).
///
/// The height follows the view and the bands, so a window restates its floor
/// as either changes.
#[must_use]
pub fn browser_floor(
    width: u32,
    view: ViewMode,
    toolbar: ToolbarBand,
    scale: Scale,
    theme: &Theme,
) -> WindowSizing {
    sizing_of(
        width,
        render::listing_floor_height(view, toolbar, scale, theme),
        0,
    )
}

/// The sizing a Properties window declares at `scale`: resizable, down to the
/// manager's [`win_floor_width`] and a height that seats its identity band and
/// section strip over a body that scrolls.
#[must_use]
pub fn properties_sizing(scale: Scale, theme: &Theme) -> WindowSizing {
    sizing_of(
        win_floor_width(scale, theme),
        scale.scale_length(PROPERTIES_MIN_HEIGHT),
        0,
    )
}

/// `floor` — a window's [`browser_floor`] — with a height ceiling at the
/// `fitted` pixels its listing fills ([`render::fitted_height`]), so no drag,
/// maximize or shrinking listing leaves a blank band beneath the items. A
/// listing shorter than the floor is held to the floor.
#[must_use]
pub const fn fitted_sizing(floor: WindowSizing, fitted: u32) -> WindowSizing {
    let min_height_px = floor.min_height_px();
    let max_height_px = if fitted > min_height_px {
        fitted
    } else {
        min_height_px
    };
    sizing_of(floor.min_width_px(), min_height_px, max_height_px)
}

/// The client extent a file-manager window opens at over `browser`, and the
/// sizing it declares: `size` — the desktop's extent for a browser window,
/// [`WIN_WIDTH`] × [`WIN_HEIGHT`] at its density — shortened to the height
/// the listing fills and never below the floor of its view under the bands it
/// opens with ([`browser_floor`]), with the ceiling at that listing
/// ([`fitted_sizing`]).
///
/// The one opening rule, so the app and a host reconstruction of its window
/// cannot place it differently. A listing still being read opens at `size`
/// with no ceiling yet.
#[must_use]
pub fn manager_opening<S: DirectorySource>(
    browser: &Browser<S>,
    size: (u32, u32),
    scale: Scale,
    theme: &Theme,
) -> ((u32, u32), WindowSizing) {
    let (width, height) = size;
    let floor = browser_floor(
        win_floor_width(scale, theme),
        browser.view_mode(),
        MANAGER_TOOLBAR_BAND,
        scale,
        theme,
    );
    match render::fitted_height(browser, width, scale, theme, None, MANAGER_TOOLBAR_BAND) {
        Some(fitted) => {
            let least = floor.min_height_px().min(height);
            (
                (width, fitted.min(height).max(least)),
                fitted_sizing(floor, fitted),
            )
        }
        None => (size, floor),
    }
}

/// One spelling of the sizing variant, so [`browser_floor`],
/// [`properties_sizing`], [`fitted_sizing`] and [`WIN_RESIZABLE`] cannot state
/// different things about the decoration.
/// No width ceiling: a listing re-flows into every width it is given.
const fn sizing_of(min_width_px: u32, min_height_px: u32, max_height_px: u32) -> WindowSizing {
    WindowSizing::Resizable {
        min_width_px,
        min_height_px,
        max_width_px: 0,
        max_height_px,
    }
}

/// Whether a browser window is decorated resizable, which widens the
/// furniture band reserved around the client.
///
/// Derived from the one sizing constructor rather than stated a second time,
/// so the drawn window and the on-screen footprint a QEMU vertical
/// reconstructs cannot disagree; the range does not affect the decoration, so
/// any value answers it.
pub const WIN_RESIZABLE: bool = sizing_of(0, 0, 0).resizable();

/// The item view a **file-manager** window opens showing: icons, not rows.
///
/// The engine's own default is the list a read-only picker wants, so the
/// manager states its choice — and states it here, because where an item is
/// drawn depends on it: the app's opening browser and the QEMU vertical's
/// host-side reconstruction of a gesture into such a window read this one
/// value, so the tile a script aims at and the tile the renderer paints
/// cannot disagree.
pub const MANAGER_VIEW_MODE: ViewMode = ViewMode::Grid;

/// The command-toolbar band a **file-manager** window opens with: none.
///
/// The toolbar and the places rail are surfaces the user asks for rather than
/// fixed parts of the layout, so neither reserves any of the window until it
/// does — a plain window is the directory and nothing else. Read by the app's
/// opening chrome and by the same host-side reconstruction as
/// [`MANAGER_VIEW_MODE`], for the same reason.
pub const MANAGER_TOOLBAR_BAND: ToolbarBand = ToolbarBand::Hidden;

/// The ground a **file-manager** window is drawn on: the desktop's frosted
/// window glass. Read by the app and by the host-side reconstruction of its
/// window, like [`MANAGER_VIEW_MODE`].
pub const MANAGER_WINDOW_GROUND: SurfaceGround = SurfaceGround::Frosted;

/// The title a **file-manager** window puts on the context menu it asks the
/// desktop to draw ([`context_menu`]).
///
/// Stated here for the same reason as [`MANAGER_VIEW_MODE`]: the plate is
/// sized around its title, so the row a QEMU vertical's host-side
/// reconstruction aims at moves with this text. One definition keeps the row
/// the script clicks and the row the desktop draws in step.
pub const MANAGER_MENU_TITLE: &str = "Files";

/// The deepest directory nesting any of the file manager's recursive
/// component-path filesystem walks will descend, counted in root-first path
/// components.
///
/// A fixed fail-closed *bound*, not a hardware-scaled capacity: it caps how far
/// a recursive removal ([`DeleteWalk`]) or a recursive copy ([`CopyWalk`])
/// descends, so a pathological or adversarial tree can never make the traversal
/// recurse without limit. Both walks share this single definition rather than
/// each carrying their own copy of the value; a tree deeper than the bound is
/// refused rather than followed. Chosen far beyond any legitimate directory
/// depth while staying comfortably bounded.
pub(crate) const MAX_WALK_DEPTH: usize = 256;

#[cfg(test)]
mod tests;
