//! The `files.app` bundle's `Run` entry point (`plans/APPWIN.md` AW3):
//! the windowed file browser, the first app served over the desktop
//! session's window channel.
//!
//! # One binary, two roles
//!
//! Launched plainly — from a shell, or by the desktop opening a folder — this
//! is an ordinary application: one window at the location it was given, and
//! the shared icon-bar menu convention on its slot. Closing every window
//! puts it away rather than ending it: the slot stays, a click there opens a
//! window at the user's home, and *Quit* is what ends the process.
//!
//! Launched by the desktop session with `tairix_window::DESKTOP_ROLE_SWITCH`
//! it is instead a **component of the desktop**
//! ([`Role::Desktop`](command::Role::Desktop)): it comes up with the session,
//! holds a permanent icon-bar slot offering the user's places and whatever is
//! mounted, opens a window per place the user chooses (bounded by the
//! session's per-client frame budget), and cannot be quit at all — its menu
//! carries neither an information row nor *Quit*. Only the session passes the
//! switch, so a second component can never appear.
//!
//! # What the program wires (and what stays in the libraries)
//!
//! Everything with behaviour worth testing lives in host-tested crates —
//! the shared browser engine and its validated path spelling
//! (`tairix_browse`), the themed listing renderer
//! (`tairix_browse::render`), and the window
//! channel's client half (`tairix_window`). This binary only composes
//! them over the live syscalls:
//!
//! * One `shm_create`d frame region **per window**, granted to the reserved
//!   window endpoint (the zero-copy surface the session maps once at create).
//! * One `port_bind`-bound event mailbox the app **parks** on through
//!   its wait-set — never a poll loop, and one mailbox for every window and
//!   the icon-bar slot alike. Every received event carries its sender's
//!   kernel-attested origin, and the app accepts only events from the session
//!   identity the (squat-protected) desktop-query reply named: no other
//!   process can feed it forged input (fail closed). Learning that identity
//!   from the *query* rather than from a create reply is what lets a
//!   component answer its slot before it owns any window.
//! * The `WindowClient` calls (create / present / close) over `ipc_call`
//!   and the `WindowEvents` typed wait over the parked source.
//! * The grid's icon artwork: the shared reclaim-governed decode cache
//!   (`tairix_icon::artwork`) bound to a bounded VFS read and to the
//!   sandboxed icon rasteriser. The artwork is untrusted input, so it is
//!   decoded by a capability-empty worker this binary re-enters itself as
//!   — never in this process — and a missing, over-long, or disbelieved
//!   asset falls back to the built-in glyph rather than a blank tile. The
//!   decode happens between turns of the event loop, never inside a paint
//!   ([`crate::icons`]), so a folder of picture-bearing bundles neither
//!   freezes the first frame nor stalls a scroll. The same wait-set carries
//!   a memory-pressure member, so the retained pixels are trimmed when the
//!   machine's band changes and released when the app ends.
//!
//! Keyboard navigation drives the browser (`Down`/`Up` select, `Enter`
//! activates the selection — it descends into a directory or launches a
//! selected `<Name>.app` bundle by spawning the bundle's own `Run` through
//! the ordinary signed app-load gate (asynchronously, with the launched
//! child reaped on the wait-set's any-child member so it is never left a
//! zombie), and a launch refusal is stated fail-loud on `stderr`;
//! `Backspace` goes up); `F2` renames the selected
//! item through an inline `lib/controls` text field, committing over
//! `fs_rename` on the reader thread under the user's own identity (a refusal
//! is stated in the field, never a silent failure or a fabricated success);
//! `Ctrl+X`/`Ctrl+C` capture the selection onto a cut/copy clipboard and
//! `Ctrl+V` pastes it into the current directory — a same-volume move is one
//! `fs_rename`, a cross-volume move is copy-then-delete, and a copy streams
//! the bytes in bounded chunks, all under the user's own identity and stopping
//! fail-loud on `stderr` at the first refusal; `Delete` asks first in an
//! alert of its own and, on confirm, removes the selection (recursively
//! for a folder) over the user's own `fs_unlink`s, stopping and stating the
//! reason on `stderr` on the first refusal; a `CloseRequested` from the
//! desktop closes that window, which ends an ordinary file manager cleanly
//! once it was its last. Every bring-up refusal exits fail-loud with a
//! reserved code and a stated reason on `stderr`.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy,
//! and fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

extern crate alloc;

pub mod appbar;
pub mod change;
pub mod chrome;
pub mod command;
pub mod deferred;
pub mod gesture;
pub mod icons;
pub mod listing;
pub mod location;
pub mod netbrowse;
pub mod notice;
pub mod operation;
pub mod operations;
pub mod preferences;
pub mod rename;
pub mod route;
pub mod settings;
pub mod sidebar;

#[cfg(test)]
mod route_tests;
#[cfg(test)]
mod test_fs;

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {

    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};

    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::time::Duration64;

    use crate::netbrowse::{
        self, Browse, Need, NetworkAnswer, NetworkAsk, NetworkLanded, NetworkReading,
    };
    use crate::route;
    use tairix_abi::discovery_ipc::DOORBELL_LEN;
    use tairix_abi::driver::display::{DamageList, DamageRect, DisplayMode, RefreshRate};
    use tairix_abi::fs::{
        FileId, FileKind, OpenFlags, FS_MODE_MASK, FS_NAME_MAX, FS_NEW_DIR_MODE, FS_NEW_FILE_MODE,
    };
    use tairix_abi::input::{
        KeyInput, KeyValue, Modifiers as AbiModifiers, NamedKeyCode, PointerButtonCode,
    };
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::window_ipc::{
        AlertOutcome, AlertSpec, DialogModality, DocumentName, DragAt, DragItems, DropOperation,
        DropSite, HandOverDocument, HandOverOutcome, MenuOutcome, PointerAction, WindowEvent,
        WindowRegion, WindowSizeState, WindowSizing,
    };
    use tairix_abi::{
        load_failure_reason, CapabilityId, Errno, FdWire, NoticeTopic, ProcId, RenameFlags,
        SpawnAttach, WaitFlags, WaitSetOp, WaitSourceKind, WaitStatus, APPINFO_WIRE_MAX,
        ORIGIN_WIRE_LEN, STDIN, STD_STREAM_COUNT, WAITSET_CHILD_ANY, WAIT_PID_ANY,
    };
    use tairix_appstore::{DirEntry as StoreDirEntry, StoreReader, Verdict};
    use tairix_browse::document;
    use tairix_browse::marquee::{self, report_band};
    use tairix_browse::render::{
        chooser_art, delete_alert, draw_open_with_chooser, draw_progress_window,
        draw_properties_window, draw_rename_field, drop_folder_at, manager_art, manager_tool_at,
        open_with_action_at, open_with_reveal, open_with_row_at, open_with_scroll_pointer,
        open_with_scroll_repeat, open_with_scroll_wheel, progress_bar_rect, progress_state,
        progress_window_extent, properties_art, properties_drive_damage, properties_reveal,
        properties_scroll_pointer, properties_scroll_repeat, properties_scroll_wheel, render_into,
        scroll_pointer, scroll_repeat, scroll_wheel, sidebar_scroll_repeat, AttrAction, Frame,
        Identity, OpenWithAction, OwnerField, PermsCursor, PermsPanel, PropertiesControls,
        PropertiesFrame, PropertiesStrip, PropertiesTab, PropertiesTarget, PropertiesView,
        SectionsShown, DELETE_CONFIRM_INDEX,
    };
    use tairix_browse::{
        applications_for, association_from_manifest, blank_documents, browser_floor,
        context_choice_from_item, context_menu, drop_operation, empty_trash_plan, fitted_sizing,
        manager_opening, network_context_menu, paste_strategy, plan_paste, quick_applications,
        trash_dest_path, trash_dir, trash_strategy, validate_new_name, win_floor_width, Activation,
        AppAssociation, Attribute, Attributes, BlankDocument, Browser, BundleIntent, Clipboard,
        ClipboardOp, ContextChoice, ContextCommand, ContextMenuModel, ContextQuick,
        DeleteDisposition, DeletePlan, DeleteTarget, DirectorySource, Drive, DriveChange,
        DriveSample, Entry, EntryKind, HiddenEndings, Listed, Listing, ListingDesk, ListingJob,
        Location, ManagerChrome, ManagerTool, ManagerToolModel, Marquee, NetworkItem, NewEntry,
        OpenWithCandidate, OpenWithChooser, OwnerChange, PasteItem, PasteStrategy, PendingEntry,
        PendingRename, Places, Probe, Probes, ProgressModel, ProgressOp, Properties, RenameError,
        RowList, RtLinkReader, ScrollColumn, SortMode, Subject, Survey, SurveyStep, Tally, Took,
        ToolbarBand, ToolbarCommand, TrashError, TrashStrategy, Volume, VolumeId, WatchUpdate,
        WatchedDirectory, Watches, MANAGER_MENU_TITLE, MANAGER_TOOLS, MANAGER_WINDOW_GROUND,
        WATCH_BUFFER_LEN, WIN_HEIGHT, WIN_WIDTH,
    };
    use tairix_controls::damage;
    use tairix_controls::text::{Keystroke, TextAction, TextField};
    use tairix_controls::value::Progress;
    use tairix_discovery::catalogue::Catalogue;
    use tairix_discovery::walk::OpenSession;
    use tairix_discovery::{DiscoveryError, RtTransport, Session};
    use tairix_geometry::{Point, Rect, Region, Scale};
    use tairix_icon::{
        artwork_cache, render_artwork, ArtworkDesk, ArtworkJob, ArtworkKey, ArtworkRasteriser,
        ArtworkReader, ArtworkResolver, IconKind, IconRequest, InlineArtwork, Landed, Resolved,
        ThumbnailStore, MAX_ARTWORK_BYTES,
    };
    use tairix_input::{ClickKind, DoubleClickTracker, Key, Modifiers, NamedKey, PointerButton};
    use tairix_procinfo::{IpcTransport, WalkStep};
    use tairix_raster::Surface;
    use tairix_rt::io::{Stderr, Write};
    use tairix_rt::PrivatePort;
    use tairix_sandbox::imagerender::{rasterise_icon, ImageRenderService};
    use tairix_sandbox::rt::{serve_stdio, worker_role, RtLauncher};
    use tairix_sandbox::ParserSandbox;
    use tairix_theme::motion::work_slice_ns;
    use tairix_theme::{Grounds, MotionTheme, Theme, ThemeRegistry};
    use tairix_window::app::{self, Wake, WindowPane};
    use tairix_window::notices::Raised;
    use tairix_window::{
        pointer_input_events, pointer_point, Desktop, EventDrain, EventError, EventMailbox,
        EventSource, Owed, Parked, Repaint, Target, WindowClient, WindowEvents, WindowTransport,
        EVENT_MAILBOX_CAPACITY,
    };

    use crate::appbar;
    use crate::change::Change;
    use crate::chrome::{Accelerator, Chrome, SelectionVerb};
    use crate::command::{self, Command, Ladder, Role, UsageError, USAGE};
    use crate::deferred::{
        DriveJob, DriveReading, DriveReads, FilesClient, FilesClients, PropertyJob, PropertyReads,
    };
    use crate::gesture::{
        self, bundle_intent, press_step, DragArm, Keyboard, NamePress, PressHit, PressHold,
        PressInput, PressStep, PrimaryPress, RenameArm, RenameCommit, SelectHow, SelectKeys,
    };
    use crate::icons::IconPipeline;
    use crate::listing::ViewMark;
    use crate::location::{leave_directory, location_title, retitle, Leave};
    use crate::notice::{self, next_alert, NextAlert, NoticeSource, Notices};
    use crate::operation::{
        change_refusal, completion_closes_at, confirmation_may_open, create_refusal,
        destination_refusal, empty_confirmation_may_open, item_refusal, mode_flip_allowed,
        operation_refusal, progress_control, progress_due, reach_refusal, trash_read_refusal,
        BarPace, ProgressControl,
    };
    use crate::operations::{run_slice, Ending, Finished, OperationDesk, Reading, Refusal, Step};
    use crate::preferences::Preferences;
    use crate::rename::RenameEdit;
    use crate::settings::{SettingsRequest, SettingsWindow, TITLE as SETTINGS_TITLE};
    use crate::sidebar::{self, press_point};
    use tairix_appdata::{Publication, PublishWorker};
    use tairix_fscopy::op::{
        Decision, Named, OpError, Operation as TreeOp, Progress as Advanced, Report, Taken, Verb,
    };
    use tairix_fscopy::Omissions;
    use tairix_rt::copy::WorkingDirectory;

    /// Exit code when the initial directory listing was refused (no
    /// filesystem reach, or a corrupt stream). A reserved, fail-closed
    /// value: the browser never shows a fabricated listing.
    const EXIT_NO_LISTING: i32 = 80;

    /// Exit code for a command line the program cannot act on: an unrecognised
    /// option, a second operand, or an argument vector that is not UTF-8. The
    /// conventional usage status the other command apps return, so a script
    /// sees the familiar value rather than one reserved to this app.
    const EXIT_USAGE: i32 = 2;

    /// The wait-set token of the any-child member: a bundle the file manager
    /// launched has exited, so it is reaped promptly (never left a zombie,
    /// and never a busy-poll — the member is drained the instant it wakes).
    const CHILD_TOKEN: u64 = app::FIRST_APP_TOKEN;

    /// The wait-set token of the reader's wake pipe: readable exactly when a
    /// directory listing, a batch of folder cues, or a bundle scan has come
    /// back, so the answer is adopted through the park the loop is already in
    /// rather than by polling for it.
    const READS_TOKEN: u64 = CHILD_TOKEN + 1;

    /// The wait-set token the mount-table notice arrives under: a volume
    /// attached or removed, which the places rail must re-read.
    const MOUNTS_TOKEN: u64 = READS_TOKEN + 1;

    /// The wait-set token the settings writer wakes the loop under: a write it
    /// carried out has an answer for the loop to adopt.
    const SETTINGS_TOKEN: u64 = MOUNTS_TOKEN + 1;

    /// The wait-set token the discovery service rings the network session's
    /// doorbell under: what the catalogue holds moved.
    const DOORBELL_TOKEN: u64 = SETTINGS_TOKEN + 1;

    /// Rings the doorbell holds before the loop drains it; the service rings
    /// once per change it queues, so a few say all a drain needs to know.
    const DOORBELL_CAPACITY: usize = 4;

    /// Each browser window's directory watch reports under this plus the
    /// number its listing consumer was minted as, so a wake names its window.
    const WATCH_TOKEN_BASE: u64 = DOORBELL_TOKEN + 1;

    /// The maximum digit count the owner/group id editor accepts — a `u32` id
    /// is at most ten decimal digits, so a longer entry cannot be a valid id.
    const OWNER_ID_MAX_DIGITS: usize = 10;

    /// The in-field hint shown when a typed owner/group id is not a
    /// well-formed, assignable `u32` (non-numeric, empty, out of range, or the
    /// reserved "unchanged" sentinel).
    const OWNER_ID_HINT: &str = "Enter a valid numeric id.";

    /// What a router that only answers "did anything change" concludes: it
    /// moved pixels it cannot name, so the window is drawn whole.
    ///
    /// Every path that replaces the listing, opens or closes an overlay, or
    /// re-derives the model is one of these — correct, and cheap enough at one
    /// window's size that describing it would buy nothing.
    const fn whole_if(changed: bool) -> Repaint {
        if changed {
            Repaint::Whole
        } else {
            Repaint::Nothing
        }
    }

    /// The name this program states its refusals under.
    const APP_NAME: &str = "files";

    /// Declare (or re-declare) this process's presence on the desktop's icon
    /// bar, as its `role` decides.
    ///
    /// An ordinary file manager makes the shared declaration — the
    /// session-drawn information row and *Quit*, with the session raising a
    /// window it has and asking for one when it has none. A component offers
    /// its places instead and neither of those rows ([`appbar`]).
    ///
    /// A refused declaration is an answer, not a death: the application says
    /// so and carries on with no slot of its own — a window it owns is still
    /// reachable through the slot the session derives from it. A component so
    /// refused has no slot and no window, which is a desktop without its file
    /// manager: stated loudly, and still not fatal.
    fn declare_app_bar(
        client: &mut WindowClient<app::RtWindowTransport>,
        endpoint: u64,
        role: Role,
        places: &Places,
    ) {
        let declared = match role {
            Role::Window => appbar::window_declaration(endpoint).map(|bar| (bar, 0)),
            Role::Desktop => appbar::component_declaration(endpoint, places),
        };
        let declared = declared.map(|(bar, skipped)| {
            if skipped > 0 {
                report_error(&alloc::format!(
                    "{skipped} place(s) do not fit the icon-bar menu and are not shown"
                ));
            }
            bar
        });
        if let Err(refused) = tairix_window::declare_app_bar(client, declared) {
            report_error(&alloc::format!("{refused}"));
        }
    }

    /// What one present writes through, threaded as one value: the channel
    /// half, the window's pane, the surface the frame is drawn in, and the
    /// title the session was last told. Bundling them keeps a frame
    /// inseparable from the window it belongs to.
    struct FrameTarget<'a, T: WindowTransport> {
        /// The app half of the window channel the present goes out over.
        client: &'a mut WindowClient<T>,
        /// The pane the frame is presented over, which names the window and
        /// the layout the frame is shaped as.
        pane: &'a mut WindowPane,
        /// The title the window currently carries, owned by the run so it
        /// outlives one frame: the location is only sent again when it moves.
        title: &'a mut String,
        /// The window-lifetime surface the frame is drawn in and copied from.
        surface: &'a mut Surface,
        /// The rectangles this frame redraws, each under its own clip, and
        /// presents.
        parts: DamageList,
    }

    /// One open browser window: everything a frame is drawn from, and nothing
    /// shared with another window.
    ///
    /// The process holds a list of these rather than one window's worth of
    /// locals, because a component's slot opens a window per place the user
    /// chooses and an empty list is a perfectly good state for it to be in
    /// (see [`Role`](command::Role)). What *is* shared sits outside: the
    /// artwork cache (one decode serves every window), the launched-bundle
    /// bookkeeping, and the desktop's own theme and density.
    struct OpenWindow {
        /// This window's channel-side state: its id, its shared frame region,
        /// and the layout both are shaped as.
        pane: WindowPane,
        /// The title the session was last told. Kept so a frame retitles only
        /// when what it names actually moves.
        title: String,
        /// The window-sized surface every frame is drawn into, held for the
        /// life of the window: allocating and zeroing one per present would be
        /// a whole-window pass of its own, and holding it is what makes a
        /// clipped repaint sound — every pixel outside the clip is the one
        /// already on screen.
        surface: Surface,
        /// What the rounds since its last present have left it owing.
        owed: Owed,
        /// What this window is showing.
        kind: WindowKind,
    }

    impl OpenWindow {
        /// The id of the popup this window currently holds, if any.
        ///
        /// A popup is a window in its own right, so the session addresses its
        /// events to the popup's own id; only the window that opened it knows
        /// the two belong together.
        fn popup_id(&self) -> Option<u64> {
            match &self.kind {
                WindowKind::Browser(browser) => browser
                    .overlays
                    .open_with
                    .as_ref()
                    .map(|chooser| chooser.pane.id()),
                WindowKind::Properties(_) | WindowKind::Progress(_) | WindowKind::Settings(_) => {
                    None
                }
            }
        }
    }

    /// What one of this process's windows is.
    ///
    /// The pane, surface and title above are every window's; this is the part
    /// that differs. One list of windows rather than one per kind, so the
    /// routing scan, the resize, the frame release, the present and the close
    /// are each written once.
    ///
    /// Every payload is boxed: they differ by a kilobyte, so a list sized for
    /// the largest would carry that per window of another kind.
    enum WindowKind {
        /// A directory listing.
        Browser(Box<BrowserWindow>),
        /// One node's Properties.
        Properties(Box<PropertiesWindow>),
        /// One operation's own progress.
        Progress(Box<ProgressWindow>),
        /// The file manager's own settings.
        Settings(Box<SettingsWindow>),
    }

    /// The file manager's own settings: the record in force and on screen, and
    /// the worker that writes it off the loop.
    struct Kept {
        publication: Publication<Preferences>,
        writer: alloc::sync::Arc<PublishWorker<Preferences>>,
    }

    impl Kept {
        /// The settings in force.
        fn live(&self) -> &Preferences {
            self.publication.live()
        }
    }

    /// An operation's progress window: modeless, a dialog of the folder window
    /// that started it, which it closes with, whose listing it reads again as
    /// it ends, and in which a refusal that stopped it is stated.
    struct ProgressWindow {
        /// The session's id for the folder window that started it.
        owner: u64,
        operation: Operation,
    }

    /// A browser window's own state.
    struct BrowserWindow {
        /// The listing this window shows.
        browser: Browser<DeferredSource>,
        /// The consumer its listings are read under, let go when it closes.
        listing: FilesClient,
        /// The overlays open over it.
        overlays: Overlays,
        /// This window's own copy of the places rail: the same shortcuts and
        /// volumes, but its own focus, cursor, and hover — one window's
        /// pointer must not highlight a row in another.
        places: Places,
        /// Which of its own chrome bands this window is showing. Per window
        /// like the rail above, so one window's chrome is not another's.
        chrome: Chrome,
        /// Where the pointer last was over this window, or `None` before it
        /// has been.
        pointer: Option<Point>,
        /// Whether it holds the keyboard, as its focus reports tell it.
        keyboard: Keyboard,
        /// This window's unanswered context-menu gesture, if one is up.
        ///
        /// The desktop mints one open id per gesture and never reuses it, so an
        /// answer that names anything else belongs to a gesture already settled
        /// and is not acted on.
        menu: Option<OpenMenuState>,
        /// What the window's height was last fitted to.
        fit: Fit,
        /// The directory shown has gone from its path: a reload that cannot
        /// list it climbs to its parent rather than showing what was there.
        lost: bool,
        /// A file was asked to be shown in this window: its entry is brought
        /// into view at the next present, once the listing has it.
        reveal_chosen: bool,
        /// Whether moving items to the Trash asks first, as the settings say.
        confirm_trash: bool,
        /// Settings that changed while a gesture held the listing, taken once
        /// it lets go.
        settings_owed: Option<Preferences>,
        /// The network answer this window last showed.
        network_seen: u64,
    }

    /// What a browser window's height was last fitted to: the window never
    /// stands taller than its listing, so no blank band opens beneath it.
    struct Fit {
        /// The location it was fitted to; moving to another fits it afresh, as
        /// a new window there would open.
        location: Location,
        /// The range the window manager was last told.
        sizing: WindowSizing,
        /// The window's narrowest client at the desktop's scale and theme,
        /// derived when either moves rather than per frame; the height floor
        /// follows the view and the bands, so it is derived at each fit.
        floor_width: u32,
        /// The height a new window opens at: the most a move to another folder
        /// grows this one to, unless its user made it taller.
        opening: u32,
        /// Whether the window is restored. Only a restored window is fitted
        /// afresh by the app; a maximized one is re-maximized under the ceiling
        /// by the window manager.
        restored: bool,
    }

    /// What a Properties window does with a write's answer.
    enum PropertiesWrite {
        /// A permission bit was flipped.
        Mode,
        /// The owner or group was set.
        Owner,
        /// The attribute named so was set, or removed.
        Attribute { shown: String, removing: bool },
    }

    impl PropertiesWrite {
        /// The title a refusal of this write is stated under.
        const fn refusal_title(&self) -> &'static str {
            match self {
                Self::Mode => "Permissions not changed",
                Self::Owner => "Owner not changed",
                Self::Attribute {
                    removing: false, ..
                } => "Attribute not set",
                Self::Attribute { removing: true, .. } => "Attribute not removed",
            }
        }
    }

    /// One node's Properties, in its own window.
    ///
    /// The node is held by *path*, not by a browser selection: several of these
    /// are open at once and the listing behind them moves on, so a window
    /// describes and writes to the node it was opened on and nothing else.
    struct PropertiesWindow {
        /// The node's absolute path, the one spelling every read and write of
        /// it uses.
        path: String,
        /// What the listing called it, which decides how it is opened to be
        /// described.
        kind: EntryKind,
        /// The spelling a link stores, shown as the alias field.
        target: Option<String>,
        /// The picture the identity band draws: the one the listing drew the
        /// node with, so it is pictured alike in both and does not change
        /// when the read lands.
        art: IconKind,
        /// What the read answered, or why there is nothing to show yet.
        state: PropertiesState,
        /// The attribute list's cursor.
        rows: RowList,
        /// How far the section on show is scrolled, and its bar. A section
        /// switch starts the new one at its top.
        scroll: ScrollColumn,
        /// The `key = value` attribute editor.
        editor: TextField,
        /// The open owning-id editor, when one is being typed into.
        owner: Option<OwnerEditor>,
        /// Which section is on show.
        tab: PropertiesTab,
        /// Where the Permissions section's keyboard cursor rests, if the
        /// section has taken the keyboard from the strip.
        perms: PermsCursor,
        /// The Permissions section as last composed, kept between events.
        section: PermsPanel,
        /// What the identity band says beneath the name. Held rather than
        /// formatted per frame, and refreshed when a read lands.
        detail: String,
        /// Whether the launching user holds `CAP_FS_CHOWN` — the one gate on
        /// offering the ownership control (read once at start-up).
        can_chown: bool,
        /// The write this window waits on, by ticket.
        writing: Option<(u64, PropertiesWrite)>,
        /// A write landed and the node is being read again: until it is, the
        /// window shows the node as it was, which no change is computed from.
        rereading: bool,
        /// The refused changes stated in the window, one at a time.
        notices: Notices,
        /// The sections the window offers.
        strip: PropertiesStrip,
        /// The drive a window opened on a volume root describes.
        drive: Option<DriveWatch>,
    }

    /// A drive window's drive, and when it is read next.
    struct DriveWatch {
        drive: Drive,
        /// The reading the drive is asked for by: its mount point, parsed
        /// once, when the window opened.
        job: DriveJob,
        /// When the next reading is asked for: only while the Drive section
        /// is on show and none is out.
        due: Option<u64>,
        /// A reading has been asked for and has not landed.
        asked: bool,
    }

    /// How often a drive window on show reads its drive again: the trace's
    /// one point a second, the rate a person reads a throughput at.
    const DRIVE_SAMPLE_NS: u64 = 1_000_000_000;

    /// What a Properties window has to show.
    enum PropertiesState {
        /// The read is in flight; the window says so rather than showing an
        /// empty or invented summary.
        Reading,
        /// What the read answered.
        Ready(Properties),
        /// The read was refused, with the reason to state.
        Refused(String),
    }

    impl PropertiesWindow {
        /// What the current state draws as.
        fn frame(&self) -> PropertiesFrame<'_> {
            match &self.state {
                PropertiesState::Reading => PropertiesFrame::Reading,
                PropertiesState::Ready(props) => PropertiesFrame::Ready(props),
                PropertiesState::Refused(reason) => PropertiesFrame::Refused(reason),
            }
        }

        /// The node's summary, once the read has landed.
        fn props(&self) -> Option<&Properties> {
            match &self.state {
                PropertiesState::Ready(props) => Some(props),
                PropertiesState::Reading | PropertiesState::Refused(_) => None,
            }
        }

        /// `change`, to be made to the node the window describes; `None`
        /// while it describes none.
        fn change(&self, change: Change) -> Option<WriteAct> {
            Some(WriteAct::Change {
                path: self.path.clone(),
                described: self.props()?.id(),
                change,
            })
        }

        /// Which section is on show, how far it is scrolled, and where each
        /// section's keyboard cursor rests.
        fn view(&self) -> PropertiesView {
            PropertiesView {
                strip: self.strip,
                tab: self.tab,
                scroll: self.scroll.offset(),
                cursor: self.rows.cursor(),
                perms: self.perms,
            }
        }

        /// The live controls the window draws — and so the ones its
        /// hit-test and keyboard resolve against, so both act on exactly
        /// what was painted.
        fn controls(&self) -> PropertiesControls<'_> {
            PropertiesControls {
                identity: self.identity(),
                can_chown: self.can_chown,
                owner: self.owner.as_ref().map(|ed| (ed.field, &ed.editor)),
                attribute: &self.editor,
                scrollbar: self.scroll.scrollbar(),
                perms: &self.section,
                drive: self.drive.as_ref().map(|watch| &watch.drive),
            }
        }

        /// What the identity band names and pictures: the node's own leaf
        /// name, what it is, and its picture.
        fn identity(&self) -> Identity<'_> {
            Identity {
                name: path_leaf(&self.path),
                detail: &self.detail,
                art: IconRequest::kind(self.art),
            }
        }

        /// The one-line summary the identity band carries beneath the name:
        /// what the node is, and how big, once the read can say.
        ///
        /// Held rather than formatted per frame: a paint re-deriving a string
        /// is work a frame does not owe.
        fn detail_for(kind: EntryKind, props: Option<&Properties>) -> String {
            match props {
                Some(props) => {
                    alloc::format!("{} · {}", props.kind_label(), props.size_display())
                }
                None => String::from(Properties::kind_label_for(kind)),
            }
        }

        /// The identity band's line as what the window now knows says it: a
        /// drive's medium and size, or a node's kind and size.
        fn refresh_detail(&mut self) {
            let drive = self.drive.as_ref().and_then(|watch| watch.drive.summary());
            self.detail = drive.unwrap_or_else(|| Self::detail_for(self.kind, self.props()));
        }

        /// The read to ask for, so a window refreshes through the same job it
        /// opened with.
        fn job(&self) -> PropertyJob {
            PropertyJob {
                path: self.path.clone(),
                kind: self.kind,
            }
        }

        /// What the sections are composed from besides the node.
        fn sections(&self) -> SectionsShown<'_> {
            sections_shown(
                self.can_chown,
                self.owner.as_ref(),
                &self.section,
                self.drive.as_ref(),
            )
        }

        /// Adopt a drive reading at `now`, answering what it changed, and
        /// ask for the next while the Drive section shows.
        fn adopt_drive(&mut self, answer: DriveReading, now: u64) -> DriveChange {
            let tab = self.tab;
            let Some(watch) = &mut self.drive else {
                return DriveChange::default();
            };
            watch.asked = false;
            let change = match answer {
                DriveReading::Read(sample) => watch.drive.adopt(&sample),
                DriveReading::Unreadable => watch.drive.unreadable(),
            };
            watch.due = (tab == PropertiesTab::Drive).then(|| now.saturating_add(DRIVE_SAMPLE_NS));
            change
        }

        /// When this window next needs the loop with no event to bring it, now
        /// it is `now`: its drive's next reading, or a held scrollbar press
        /// stepping its section.
        fn due(&mut self, now: u64) -> Option<u64> {
            let drive = self.drive.as_ref().and_then(|watch| watch.due);
            [drive, self.scroll.repeat_deadline(now)]
                .into_iter()
                .flatten()
                .min()
        }

        /// Step the section's held scrollbar press if it is due at `now`,
        /// answering whether the section moved.
        fn step_scroll(
            &mut self,
            (window, scale, theme): (Rect, Scale, &Theme),
            now: u64,
            damage: &mut Region,
        ) -> bool {
            let view = self.view();
            let PropertiesState::Ready(props) = &self.state else {
                return false;
            };
            let shown = sections_shown(
                self.can_chown,
                self.owner.as_ref(),
                &self.section,
                self.drive.as_ref(),
            );
            properties_scroll_repeat(
                &mut self.scroll,
                props,
                (view, shown),
                (window, scale, theme),
                now,
                damage,
            )
        }

        /// The reading to ask for now, if one is due at `now` and none is out.
        fn take_due_drive(&mut self, now: u64) -> Option<DriveJob> {
            let watch = self.drive.as_mut()?;
            if watch.asked || watch.due.is_none_or(|due| due > now) {
                return None;
            }
            watch.due = None;
            watch.asked = true;
            Some(watch.job.clone())
        }
    }

    impl OpenWindow {
        /// The listing this window shows, or `None` when it shows something
        /// else.
        fn browser(&mut self) -> Option<&mut BrowserWindow> {
            match &mut self.kind {
                WindowKind::Browser(win) => Some(win),
                WindowKind::Properties(_) | WindowKind::Progress(_) | WindowKind::Settings(_) => {
                    None
                }
            }
        }

        fn progress(&mut self) -> Option<&mut ProgressWindow> {
            match &mut self.kind {
                WindowKind::Progress(win) => Some(win),
                WindowKind::Browser(_) | WindowKind::Properties(_) | WindowKind::Settings(_) => {
                    None
                }
            }
        }

        /// Whether this is a listing of `location`, or one already on its way
        /// there.
        fn shows(&self, location: &Location) -> bool {
            match &self.kind {
                WindowKind::Browser(win) => lists(&win.browser, location),
                WindowKind::Properties(_) | WindowKind::Progress(_) | WindowKind::Settings(_) => {
                    false
                }
            }
        }

        /// Whether this window lists `folder`, with a file named `name` in it.
        fn lists_file(&self, folder: &[String], name: &str) -> bool {
            let WindowKind::Browser(win) = &self.kind else {
                return false;
            };
            win.browser.folder() == Some(folder)
                && win.browser.entries().iter().any(|entry| {
                    entry.name() == name && entry.kind().resolved() == Some(EntryKind::File)
                })
        }
    }

    /// Whether `browser` lists `location`, or is on its way there.
    fn lists(browser: &Browser<DeferredSource>, location: &Location) -> bool {
        browser.is_at(location) || browser.listing_target() == Some(location)
    }

    impl BrowserWindow {
        /// Record where `event`, addressed to this window, puts the pointer.
        ///
        /// Every pointer event and every wheel turn carries the position,
        /// whatever mode the window is in and whether or not it draws a rail,
        /// so the wheel is routed by where it turned, never by where the
        /// pointer was before.
        fn note_pointer(&mut self, event: &WindowEvent) {
            if let WindowEvent::Pointer { x, y, .. } | WindowEvent::Scrolled { x, y, .. } = event {
                self.pointer = Some(pointer_point(*x, *y));
            }
        }

        /// Whether a confirmation of `plan` may open: the window still shows
        /// the selection it plans, with nothing else open over it.
        fn awaits_confirmation_of(&self, plan: &DeletePlan) -> bool {
            confirmation_may_open(
                plan,
                self.browser.plan_delete().as_ref(),
                self.is_occupied(),
            )
        }

        /// Whether a confirmation to empty `trash` may open: the window still
        /// shows it, with nothing else open over it.
        fn awaits_empty_confirmation_of(&self, trash: &[String]) -> bool {
            self.browser
                .folder()
                .is_some_and(|shown| empty_confirmation_may_open(trash, shown, self.is_occupied()))
        }

        /// Whether anything is open over the listing that a confirmation
        /// would take keys meant for.
        fn is_occupied(&self) -> bool {
            let overlays = &self.overlays;
            self.listing_is_held()
                || overlays.delete.is_some()
                || overlays.open_with.is_some()
                || overlays.notices.is_showing()
        }

        /// Whether a gesture holds the entries it was opened on — a context
        /// menu, an inline rename, a drag — so a reported change waits for it
        /// to end rather than moving the listing under it.
        fn listing_is_held(&self) -> bool {
            self.menu.is_some()
                || self.overlays.rename.is_open()
                || self.overlays.drag.is_some()
                || self.overlays.carrying.is_some()
                || matches!(self.overlays.sweep, Some(Sweep::Live(_)))
        }

        /// Take `settings` — whether the Trash asks, where folders stand, how
        /// the listing names, lists and pictures its entries — answering
        /// whether what the window shows changed. What would move the entries
        /// waits while a gesture holds them, as a fresh listing does.
        fn adopt_settings(&mut self, settings: &Preferences) -> bool {
            self.confirm_trash = settings.confirm_trash;
            if self.listing_is_held() {
                self.settings_owed = Some(*settings);
                return false;
            }
            self.settings_owed = None;
            let order = settings.reorder(self.browser.sort_mode());
            let changed = order != self.browser.sort_mode()
                || settings.presentation != *self.browser.presentation();
            self.browser.set_sort_mode(order);
            if let Err(err) = self.browser.set_presentation(settings.presentation) {
                report_error(&alloc::format!("listing refused ({err})"));
            }
            changed
        }

        /// Step the listing under a held band if a step is due on a desktop
        /// refreshing at `refresh`, answering what it repainted.
        fn step_band(
            &mut self,
            canvas: Canvas<'_>,
            (now, refresh): (u64, RefreshRate),
            damage: &mut Region,
        ) -> Repaint {
            let Some(Sweep::Live(band)) = self.overlays.sweep.as_mut() else {
                return Repaint::Nothing;
            };
            let frame = Frame {
                scale: canvas.scale,
                theme: canvas.theme(),
                viewport: canvas.viewport(&self.places),
                toolbar: canvas.chrome.toolbar,
            };
            let mark = ViewMark::of(&self.browser, frame);
            let moved = band.step(&mut self.browser, frame, (now, refresh));
            Repaint::reported_if(moved && mark.report(&self.browser, frame, damage))
        }

        /// A listing this window asked for was refused: say so in the window,
        /// which stays where it was. Answers whether the window changed.
        /// Show `items`, the catalogue's latest, if this window shows the
        /// network or is on its way there: a navigation it waited on lands and
        /// owes the whole window, and an update repaints only the slots it
        /// altered.
        fn adopt_network(
            &mut self,
            items: &[NetworkItem],
            reads: &Reads,
            canvas: Canvas<'_>,
            damage: &mut Region,
        ) -> Repaint {
            let arriving = self.browser.awaits_network();
            let viewport = canvas.viewport(&self.places);
            let (scale, theme, toolbar) = (canvas.scale, canvas.theme(), canvas.chrome.toolbar);
            let before = tairix_browse::render::shown_listing(
                &self.browser,
                scale,
                theme,
                viewport,
                toolbar,
            );
            if !self.browser.show_network(items.to_vec()) {
                return Repaint::Nothing;
            }
            if arriving {
                // The folder left is followed no longer, and no click armed on
                // its entries names anything shown now.
                drop(reads.unwatch(self.listing));
                self.overlays.double_click.reset();
                self.overlays.rename_arm = None;
                return Repaint::Whole;
            }
            Repaint::reported_if(tairix_browse::render::listing_damage(
                &before,
                &self.browser,
                scale,
                theme,
                viewport,
                toolbar,
                damage,
            ))
        }

        /// Whether this window is to show answer `generation`: it wants the
        /// network, has not shown that answer or waits on one, and nothing
        /// holds its listing.
        fn owes_network(&self, generation: u64) -> bool {
            self.browser.wants_network()
                && (self.network_seen != generation || self.browser.awaits_network())
                && !self.listing_is_held()
        }

        /// The catalogue can no longer be read: a navigation to the network
        /// is refused where it started, and a network shown says it stopped,
        /// each stating `err`.
        fn network_failed(&mut self, err: DiscoveryError) -> Repaint {
            if !self.browser.wants_network() {
                return Repaint::Nothing;
            }
            let shown = self.browser.is_network();
            let changed = self.browser.stop_network();
            let title = if shown {
                netbrowse::STOPPED
            } else {
                netbrowse::NOT_SHOWN
            };
            report_error(&alloc::format!("{title}: {err}"));
            self.overlays
                .notices
                .refuse(NoticeSource::Network, title, netbrowse::refusal(err));
            if changed {
                Repaint::Whole
            } else {
                Repaint::Nothing
            }
        }

        fn listing_refused(&mut self, err: tairix_browse::BrowseError) -> bool {
            report_error(&alloc::format!("listing refused ({err})"));
            self.overlays.notices.refuse(
                NoticeSource::Listing,
                "Not listed",
                command::listing_refusal(err),
            );
            true
        }

        /// When this window next needs the loop with no event to bring it, now
        /// it is `now` on a desktop refreshing at `refresh`: the band held over
        /// it steps the listing, a clicked name opens, or a held scrollbar
        /// press steps its column.
        fn due(&mut self, now: u64, refresh: RefreshRate) -> Option<u64> {
            let band = match &self.overlays.sweep {
                Some(Sweep::Live(band)) => band.due(refresh),
                _ => None,
            };
            let rename = self.overlays.rename_arm.and_then(|arm| arm.due());
            let chooser = self
                .overlays
                .open_with
                .as_mut()
                .and_then(|overlay| overlay.chooser.scroll_repeat_deadline(now));
            [
                band,
                rename,
                chooser,
                self.browser.scroll_repeat_deadline(now),
                self.places.scroll_mut().repeat_deadline(now),
            ]
            .into_iter()
            .flatten()
            .min()
        }

        /// Step every held scrollbar press in this window due at `now` — the
        /// listing's, the rail's while it shows, the chooser's — reporting
        /// what it repainted here. The chooser presents in its own pane.
        fn step_scrollbars(
            &mut self,
            canvas: Canvas<'_>,
            now: u64,
            damage: &mut Region,
        ) -> Repaint {
            let (scale, theme, toolbar) = (canvas.scale, canvas.theme(), canvas.chrome.toolbar);
            let mut moved = scroll_repeat(
                &mut self.browser,
                (scale, theme),
                canvas.viewport(&self.places),
                toolbar,
                now,
                damage,
            );
            if canvas.chrome.rail(&self.places).is_some() {
                moved |= sidebar_scroll_repeat(
                    &mut self.places,
                    (canvas.window(), scale, theme),
                    toolbar,
                    now,
                    damage,
                );
            }
            if let Some(overlay) = self.overlays.open_with.as_mut() {
                let mode = *overlay.pane.mode();
                let viewport = Rect::new(0, 0, mode.width_px, mode.height_px);
                overlay.owed |= open_with_scroll_repeat(
                    &mut overlay.chooser,
                    (scale, theme),
                    viewport,
                    now,
                    &mut damage::sink(),
                );
            }
            Repaint::reported_if(moved)
        }

        /// Open the rename a click on a name armed once it is due at `now`,
        /// reporting what it repainted: the entry and its field, or the item
        /// area if bringing it into view scrolled. It opens only on the entry
        /// still chosen alone, with nothing holding the listing and no
        /// navigation under way.
        fn open_due_rename(
            &mut self,
            canvas: Canvas<'_>,
            now: u64,
            damage: &mut Region,
        ) -> Repaint {
            let Some(arm) = self.overlays.rename_arm.filter(|arm| arm.is_due(now)) else {
                return Repaint::Nothing;
            };
            self.overlays.rename_arm = None;
            let settled = !self.browser.listing_elsewhere();
            let free = !self.listing_is_held()
                && self.overlays.delete.is_none()
                && !self.overlays.notices.is_showing();
            if !settled || !free || self.browser.chosen_index() != Some(arm.index()) {
                return Repaint::Nothing;
            }
            let frame = Frame {
                scale: canvas.scale,
                theme: canvas.theme(),
                viewport: canvas.viewport(&self.places),
                toolbar: canvas.chrome.toolbar,
            };
            let mark = ViewMark::of(&self.browser, frame);
            let (opened, _) = begin_rename(
                &mut self.browser,
                &mut self.overlays.rename,
                frame.scale,
                frame.theme,
                frame.viewport,
                frame.toolbar,
            );
            if !opened {
                return Repaint::Nothing;
            }
            mark.report(&self.browser, frame, damage);
            let (scale, theme, viewport, toolbar) =
                (frame.scale, frame.theme, frame.viewport, frame.toolbar);
            let entry = tairix_browse::render::entry_rect(
                &self.browser,
                scale,
                theme,
                viewport,
                toolbar,
                arm.index(),
            );
            let field = tairix_browse::render::selection_name_rect(
                &self.browser,
                scale,
                theme,
                viewport,
                toolbar,
            );
            for rect in entry.into_iter().chain(field) {
                damage.add(rect);
            }
            Repaint::Reported
        }

        /// Open the rename New ▸ asked for once the listing shows the new
        /// entry, answering whether it opened. A focus moved elsewhere, a
        /// navigation away, a gesture holding the listing, or a rename still
        /// with the volume lets it go.
        fn rename_arrived(&mut self, canvas: Canvas<'_>) -> bool {
            let Some((dir, name)) = self.overlays.rename_on_arrival.as_ref() else {
                return false;
            };
            if self.browser.focus_pending() == Some(name.as_str()) {
                return false;
            }
            let arrived = self.browser.focus_pending().is_none()
                && self.browser.folder() == Some(dir.as_slice())
                && self.browser.chosen_name() == Some(name.as_str())
                && !self.listing_is_held();
            self.overlays.rename_on_arrival = None;
            arrived
                && begin_rename(
                    &mut self.browser,
                    &mut self.overlays.rename,
                    canvas.scale,
                    canvas.theme(),
                    canvas.viewport(&self.places),
                    canvas.chrome.toolbar,
                )
                .0
        }

        /// Apply what the window's watch reported, answering what it
        /// repainted. A change rewrites only the rows it altered; a rescan
        /// re-reads the folder whole; a folder gone from its path is re-read
        /// and, if it will not list, left for its parent. A navigation to
        /// somewhere else decides what the window shows next, so a report
        /// waits for it, as one does while a gesture holds the listing.
        fn follow_watch(
            &mut self,
            reads: &Reads,
            canvas: Canvas<'_>,
            damage: &mut Region,
        ) -> Repaint {
            if self.listing_is_held() || self.browser.listing_elsewhere() {
                return Repaint::Nothing;
            }
            match reads.take_watch_update(self.listing) {
                WatchUpdate::Quiet => Repaint::Nothing,
                WatchUpdate::Changes(changes) => self.merge(changes, reads, canvas, damage),
                WatchUpdate::Rescan => self.rescan(),
                WatchUpdate::Gone => {
                    drop(reads.unwatch(self.listing));
                    self.lost = true;
                    if self.browser.refresh().is_ok() {
                        return self.landed();
                    }
                    self.climb()
                }
            }
        }

        /// Read the folder again whole. A reload already under way may have
        /// begun before the change: asking again restamps it.
        fn rescan(&mut self) -> Repaint {
            match self.browser.refresh() {
                Ok(()) => self.landed(),
                Err(err) => {
                    self.listing_refused(err);
                    Repaint::Nothing
                }
            }
        }

        /// Merge reported `changes` into the listing, repainting only the rows
        /// they moved. A changed folder's cue is asked afresh, and a toolbar
        /// whose verbs the change enabled or disabled is repainted with it; a
        /// merge the memory could not be had for reads the folder again.
        fn merge(
            &mut self,
            changes: Vec<tairix_browse::EntryChange>,
            reads: &Reads,
            canvas: Canvas<'_>,
            damage: &mut Region,
        ) -> Repaint {
            let viewport = canvas.viewport(&self.places);
            let (scale, theme, toolbar) = (canvas.scale, canvas.theme(), canvas.chrome.toolbar);
            let cued = self
                .browser
                .folder()
                .is_some_and(|folder| reads.invalidate_probes(folder, &changes));
            let tools = manager_tool_model(&self.browser);
            let before = tairix_browse::render::shown_listing(
                &self.browser,
                scale,
                theme,
                viewport,
                toolbar,
            );
            let Ok(moved) = self.browser.apply_changes(changes) else {
                return self.rescan();
            };
            let marked = self.browser.mark_volume_roots(&self.places);
            let recued = cued && resolve_visible_occupancy(&mut self.browser, &self.places, canvas);
            if moved.is_none() && !recued && !marked {
                return Repaint::Nothing;
            }
            if let Some(placement) = &moved {
                self.overlays
                    .double_click
                    .follow(|at| placement.place_subject(at));
                self.overlays.rename_arm = self
                    .overlays
                    .rename_arm
                    .and_then(|arm| arm.follow(|at| placement.place(at)));
            }
            // The editor may scroll its row into view, and the verbs live in
            // the toolbar: neither is a row comparison.
            if self.rename_arrived(canvas) || manager_tool_model(&self.browser) != tools {
                return Repaint::Whole;
            }
            Repaint::reported_if(tairix_browse::render::listing_damage(
                &before,
                &self.browser,
                scale,
                theme,
                viewport,
                toolbar,
                damage,
            ))
        }

        /// What a navigation or reload just asked for repaints: nothing while
        /// its read is under way, since its landing repaints, and the whole
        /// window when it was read on the spot.
        fn landed(&mut self) -> Repaint {
            if self.browser.is_listing() {
                return Repaint::Nothing;
            }
            self.lost = false;
            self.browser.mark_volume_roots(&self.places);
            Repaint::Whole
        }

        /// The folder shown is gone from its path: leave it for the nearest
        /// one above that is still there, staying lost until a listing lands
        /// so a refusal that answers later climbs on.
        fn climb(&mut self) -> Repaint {
            match self.browser.climb() {
                Ok(true) => self.landed(),
                Ok(false) => {
                    self.lost = false;
                    Repaint::Nothing
                }
                Err(err) => {
                    self.lost = false;
                    self.listing_refused(err);
                    Repaint::Nothing
                }
            }
        }
    }

    /// One context-menu gesture in flight: the open the desktop minted for it,
    /// and what the rows it built were built *from*.
    ///
    /// The quick context is remembered rather than re-derived when the answer
    /// arrives, because an answer names a candidate by its position in the list
    /// the *rows* came from. Re-deriving it would read the program stores again
    /// and could rank them differently, so a chosen row would launch an
    /// application the user never saw.
    struct OpenMenuState {
        /// The session-minted id this gesture's one answer names.
        open_id: u64,
        /// The file the menu was opened on, by the same spelling every open
        /// uses — `None` where the selection was not a file with candidates.
        target: Option<PendingChooser>,
        /// The applications the "Open With…" submenu offered, in the order its
        /// ids number them.
        candidates: Vec<OpenWithCandidate>,
        /// The documents New ▸ offered, in the order its ids number them.
        documents: Vec<BlankDocument>,
        /// The endings the Rename row's field showed its name without, which
        /// a commit restores.
        endings: HiddenEndings,
    }

    /// Fold one round's conclusion about `win`, and the region it reported,
    /// into what the window owes. The turn paints it once its input is drained,
    /// so a burst of samples costs one frame.
    fn owe(win: &mut OpenWindow, repaint: Repaint, damage: &Region) {
        let mode = *win.pane.mode();
        win.owed.owe(&mode, repaint, damage);
    }

    /// Bring the network browse up to date: take what the reader answered
    /// into every window that wants the network, ask the reader for what the
    /// windows need now, and show a window that arrived meanwhile the answer
    /// already held. Answers whether anything moved.
    fn step_network(
        (windows, opening): (&mut [OpenWindow], &Opening),
        browse: &mut Browse,
        reads: &Reads,
        rung: &Cell<bool>,
        notices: &mut Notices,
        look: (&Theme, Scale),
    ) -> bool {
        let now = tairix_rt::clock_get();
        let mut moved = false;
        if let Some(answer) = reads.take_network() {
            let landed = browse.landed(answer, now);
            moved |= land_network(windows, opening, &landed, (reads, notices));
        }
        let mut need = Need {
            rung: rung.replace(false),
            ..Need::default()
        };
        let opening_browsers = opening.borrow();
        let browsers = windows
            .iter()
            .filter_map(|win| match &win.kind {
                WindowKind::Browser(state) => Some(&state.browser),
                _ => None,
            })
            .chain(opening_browsers.iter().map(|asked| &asked.browser));
        for browser in browsers {
            need.wanted |= browser.wants_network();
            need.waiting |= browser.awaits_network();
        }
        drop(opening_browsers);
        if let Some(ask) = browse.next(need, now) {
            if let Some(answer) = reads.want_network(ask) {
                let landed = browse.landed(answer, now);
                moved |= land_network(windows, opening, &landed, (reads, notices));
            }
        }
        if let Some(latest) = browse.latest() {
            moved |= land_items(windows, opening, latest, reads, look);
        }
        moved
    }

    /// Take a failed answer into every window that wants the network; a
    /// network window still opening is given up, stated once. Items are
    /// shown from what the browse holds ([`land_items`]).
    fn land_network(
        windows: &mut [OpenWindow],
        opening: &Opening,
        landed: &NetworkLanded,
        (reads, notices): (&Reads, &mut Notices),
    ) -> bool {
        match landed {
            NetworkLanded::Failed(err) => {
                let mut moved = false;
                for win in windows.iter_mut() {
                    let repaint = win
                        .browser()
                        .map_or(Repaint::Nothing, |state| state.network_failed(*err));
                    if repaint != Repaint::Nothing {
                        moved = true;
                        owe(win, repaint, &Region::new());
                    }
                }
                let mut opening = opening.borrow_mut();
                let mut given_up = false;
                let mut at = 0;
                while at < opening.len() {
                    if opening[at].browser.wants_network() {
                        reads.forget_listing(opening.remove(at).listing);
                        given_up = true;
                    } else {
                        at += 1;
                    }
                }
                if given_up {
                    report_error(&alloc::format!("{}: {err}", netbrowse::NOT_SHOWN));
                    notices.refuse(
                        NoticeSource::Network,
                        netbrowse::NOT_SHOWN,
                        netbrowse::refusal(*err),
                    );
                }
                moved || given_up
            }
            NetworkLanded::Items | NetworkLanded::Nothing => false,
        }
    }

    /// Show answer `generation`, `items`, in every window that wants the
    /// network and has not shown it, or waits on it, and in every network
    /// window still opening, which then opens. A window whose listing is held
    /// shows it once let go.
    fn land_items(
        windows: &mut [OpenWindow],
        opening: &Opening,
        (generation, items): (u64, &[NetworkItem]),
        reads: &Reads,
        (theme, scale): (&Theme, Scale),
    ) -> bool {
        let mut moved = false;
        for win in windows.iter_mut() {
            let mode = *win.pane.mode();
            let mut damage = Region::new();
            let repaint = match win.browser() {
                Some(state) if state.owes_network(generation) => {
                    let canvas = Canvas {
                        theme,
                        mode: &mode,
                        scale,
                        chrome: state.chrome,
                    };
                    let repaint = state.adopt_network(items, reads, canvas, &mut damage);
                    state.network_seen = generation;
                    repaint
                }
                _ => continue,
            };
            if repaint != Repaint::Nothing {
                moved = true;
                owe(win, repaint, &damage);
            }
        }
        for asked in opening.borrow_mut().iter_mut() {
            if asked.browser.awaits_network() {
                moved |= asked.browser.show_network(items.to_vec());
            }
        }
        moved
    }

    /// The doorbell the discovery service rings the network session on, a
    /// member of the event wait-set, or why none could be had.
    fn network_doorbell(set: u64) -> Result<PrivatePort, Errno> {
        let port = PrivatePort::bind(DOORBELL_LEN, DOORBELL_CAPACITY)?;
        match tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Port,
            port.id(),
            DOORBELL_TOKEN,
        ) {
            0 => Ok(port),
            refused => Err(Errno::from_syscall(refused)),
        }
    }

    /// Paint what `win` owes and present it.
    ///
    /// # Errors
    ///
    /// Whatever the present refuses; the caller treats a refused present as a
    /// lost channel and exits fail-loud.
    fn present_owed(
        win: &mut OpenWindow,
        client: &mut WindowClient<app::RtWindowTransport>,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        scale: Scale,
    ) -> Result<(), Errno> {
        if win.owed.is_clean() {
            // Nothing moved, so a released window stays released rather than
            // being re-attached to redraw pixels nobody can see.
            return Ok(());
        }
        // A region the session released, like a surface a fit re-allocated,
        // holds none of the pixels a partial present would leave standing.
        if fit_to_listing(win, client, grounds, scale) || win.pane.content_released() {
            win.owed.owe_whole();
        }
        bring_chosen_into_view(win, grounds, scale);
        let whole = win.owed.is_whole();
        let mode = *win.pane.mode();
        let Some(parts) = win.owed.take(&mode) else {
            return Ok(());
        };
        let mut target = FrameTarget {
            client,
            pane: &mut win.pane,
            title: &mut win.title,
            surface: &mut win.surface,
            parts,
        };
        match &mut win.kind {
            WindowKind::Browser(browser) => {
                if whole {
                    sidebar::follow_pointer(
                        &mut browser.places,
                        (
                            Rect::new(0, 0, mode.width_px, mode.height_px),
                            scale,
                            grounds.popups,
                        ),
                        browser.chrome,
                        browser.pointer,
                    );
                }
                present_frame(
                    &mut browser.browser,
                    &browser.overlays,
                    &browser.places,
                    browser.chrome,
                    grounds,
                    &mut target,
                    icons,
                    scale,
                )
            }
            WindowKind::Properties(props) => {
                present_properties(props, grounds.popups, &mut target, icons, scale)
            }
            WindowKind::Progress(progress) => {
                present_progress(progress, grounds.popups, &mut target, scale)
            }
            WindowKind::Settings(page) => {
                present_settings(page, grounds.popups, &mut target, scale)
            }
        }
    }

    /// Paint the settings window's client and present it.
    ///
    /// # Errors
    ///
    /// Whatever the present refuses.
    fn present_settings<T: WindowTransport>(
        page: &SettingsWindow,
        theme: &Theme,
        target: &mut FrameTarget<'_, T>,
        scale: Scale,
    ) -> Result<(), Errno> {
        let mode = *target.pane.mode();
        let window = Rect::new(0, 0, mode.width_px, mode.height_px);
        let parts = target.parts;
        let surface = &mut *target.surface;
        for part in parts.rects() {
            surface.with_clip(part.x, part.y, part.width_px, part.height_px, |surface| {
                page.render(surface, window, scale, theme);
            });
        }
        target.pane.present_list(target.client, surface, &parts)
    }

    /// Paint a progress window's client and present it, retitling it as the
    /// operation turns to cancelling.
    ///
    /// # Errors
    ///
    /// Whatever the present refuses.
    fn present_progress<T: WindowTransport>(
        win: &ProgressWindow,
        theme: &Theme,
        target: &mut FrameTarget<'_, T>,
        scale: Scale,
    ) -> Result<(), Errno> {
        let mode = *target.pane.mode();
        let client = Rect::new(0, 0, mode.width_px, mode.height_px);
        let (progress, trace) = (&win.operation.progress, &win.operation.trace);
        let bar = progress_bar_rect(client, scale, theme);
        // A frame of the bar's own motion lays out and draws nothing else.
        let bar_only = target.parts.rects().iter().all(|part| {
            let part = Rect::new(
                i32::try_from(part.x).unwrap_or(i32::MAX),
                i32::try_from(part.y).unwrap_or(i32::MAX),
                part.width_px,
                part.height_px,
            );
            part.intersection(&bar) == part
        });
        let surface = &mut *target.surface;
        for part in target.parts.rects() {
            surface.with_clip(part.x, part.y, part.width_px, part.height_px, |surface| {
                if bar_only {
                    trace.render(surface, bar, scale, theme);
                } else {
                    draw_progress_window(surface, (progress, trace), scale, theme, client);
                }
            });
        }
        target
            .pane
            .present_list(target.client, surface, &target.parts)?;
        // A refused retitle keeps the title remembered, so the next frame asks
        // again.
        let title = progress.title();
        if target.title.as_str() != title
            && target.client.set_title(target.pane.id(), title).is_ok()
        {
            target.title.clear();
            target.title.push_str(title);
        }
        Ok(())
    }

    /// Paint a Properties window's whole client and present it.
    ///
    /// The window's own title bar names the node, so the client is the
    /// identity band, the section strip and the selected section; the read
    /// that fills them happens on the reader, so this draws whatever has
    /// landed and the stated reading or refusal notice until it does.
    ///
    /// # Errors
    ///
    /// Whatever the present refuses.
    fn present_properties<T: WindowTransport>(
        win: &mut PropertiesWindow,
        theme: &Theme,
        target: &mut FrameTarget<'_, T>,
        icons: &RefCell<IconPipeline>,
        scale: Scale,
    ) -> Result<(), Errno> {
        let mode = *target.pane.mode();
        let window = Rect::new(0, 0, mode.width_px, mode.height_px);
        let controls = win.controls();
        let view = win.view();
        let frame = win.frame();
        let parts = target.parts;
        let surface = &mut *target.surface;
        {
            let mut pipeline = icons.borrow_mut();
            let mut source = pipeline.source();
            for part in parts.rects() {
                surface.with_clip(part.x, part.y, part.width_px, part.height_px, |surface| {
                    draw_properties_window(
                        surface,
                        frame,
                        view,
                        controls,
                        scale,
                        theme,
                        window,
                        &mut source,
                    );
                });
            }
        }
        target.pane.present_list(target.client, surface, &parts)
    }

    /// Repaint and present the whole of `win` now: a window's first frame.
    ///
    /// # Errors
    ///
    /// Whatever the present refuses.
    fn present_whole(
        win: &mut OpenWindow,
        client: &mut WindowClient<app::RtWindowTransport>,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        scale: Scale,
    ) -> Result<(), Errno> {
        win.owed.owe_whole();
        present_owed(win, client, grounds, icons, scale)
    }

    /// Paint every window that owes the screen, and every chooser popup over
    /// one, once: how a turn ends after its input is drained and what landed
    /// meanwhile is adopted.
    ///
    /// # Errors
    ///
    /// Whatever a window's present refuses. A chooser's refusal is said and
    /// costs nothing more: the window behind it is unaffected.
    fn present_owed_windows(
        windows: &mut [OpenWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        scale: Scale,
    ) -> Result<(), Errno> {
        for win in windows {
            present_owed(win, client, grounds, icons, scale)?;
            let mode = *win.pane.mode();
            let WindowKind::Browser(state) = &mut win.kind else {
                continue;
            };
            let Some(overlay) = state
                .overlays
                .open_with
                .as_mut()
                .filter(|overlay| overlay.owed)
            else {
                continue;
            };
            overlay.owed = false;
            let canvas = Canvas {
                theme: grounds.popups,
                mode: &mode,
                scale,
                chrome: state.chrome,
            };
            if present_chooser(overlay, client, canvas, icons).is_err() {
                report_error("the chooser present was refused");
            }
        }
        Ok(())
    }

    /// Open the browser window `landed` asked for, now that its first listing
    /// is in, at the height that listing fills, and paint it whole.
    ///
    /// The reason for a refusal is stated here, on the one fail-loud path, and
    /// the exit code it would warrant is handed back: a refused frame region
    /// and a refused window are different diagnoses a supervisor reads apart.
    /// A window nobody can see is worse than none, so one whose first frame is
    /// refused is taken down again.
    ///
    /// `places` is the process's rail; the window takes its own copy so its
    /// focus and hover are its own.
    ///
    /// # Errors
    ///
    /// The exit code naming what refused.
    fn open_window(
        client: &mut WindowClient<app::RtWindowTransport>,
        event_endpoint: u64,
        desktop: &Desktop,
        places: &Places,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        (landed, settings): (PendingWindow, &Preferences),
    ) -> Result<OpenWindow, i32> {
        let PendingWindow {
            mut browser,
            listing,
            ladder,
            ..
        } = landed;
        let shows = ladder
            .as_ref()
            .zip(browser.folder())
            .and_then(|(ladder, folder)| ladder.shows_in(folder))
            .map(String::from);
        if let Some(name) = &shows {
            browser.follow(name);
        }
        let opening = desktop.window_size(WIN_WIDTH, WIN_HEIGHT);
        let ((w, h), sizing) = manager_opening(&browser, opening, desktop.scale(), grounds.window);
        let mode = app::mode_for(w, h);
        // Allocated before the session is asked, so a window this app could
        // not draw into is never put on the desktop.
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            report_error("window surface refused; no window opened");
            return Err(app::EXIT_NO_WINDOW);
        };
        // The window opens carrying the location it shows, rather than a name
        // the first frame would have to replace.
        let title = location_title(&browser);
        let pane = match WindowPane::open(client, event_endpoint, &mode, &title, sizing) {
            Ok((pane, _)) => pane,
            Err(err) => {
                report_error(&alloc::format!("{err}; no window opened"));
                return Err(err.code());
            }
        };
        // Asked before the first present, so no frame is shown unfrosted.
        apply_backdrop(client, pane.id(), grounds);
        let location = browser.location();
        let mut overlays = initial_overlays();
        if let Some(reason) = ladder.as_ref().and_then(Ladder::moved) {
            overlays
                .notices
                .refuse(NoticeSource::Listing, OPENED_ELSEWHERE, &reason);
        }
        let mut win = OpenWindow {
            pane,
            title,
            surface,
            owed: Owed::new(),
            kind: WindowKind::Browser(Box::new(BrowserWindow {
                browser,
                listing,
                overlays,
                places: places.clone(),
                chrome: settings.chrome,
                pointer: None,
                keyboard: Keyboard::default(),
                menu: None,
                fit: Fit {
                    location,
                    sizing,
                    floor_width: win_floor_width(desktop.scale(), grounds.window),
                    opening: opening.1,
                    restored: true,
                },
                lost: false,
                reveal_chosen: shows.is_some(),
                confirm_trash: settings.confirm_trash,
                settings_owed: None,
                network_seen: 0,
            })),
        };
        if present_whole(&mut win, client, grounds, icons, desktop.scale()).is_err() {
            let _ = win.pane.close(client);
            report_error("the new window's first frame was refused; it was closed again");
            return Err(app::EXIT_CHANNEL_LOST);
        }
        Ok(win)
    }

    /// Ask the compositor for the blur `window`'s ground is drawn over. A
    /// refusal is stated, and the window keeps the blur it had.
    fn apply_backdrop(
        client: &mut WindowClient<app::RtWindowTransport>,
        window: u64,
        grounds: Grounds<'_>,
    ) {
        if let Err(err) = client.set_backdrop_blur(window, grounds.window.backdrop_blur()) {
            report_error(&alloc::format!("backdrop blur refused: {err}"));
        }
    }

    /// Hold `win`'s height to what its listing fills, so no blank band opens
    /// beneath the items, answering whether the window was resized.
    ///
    /// The range is restated whenever the listing's height or the floor of its
    /// view and bands moves, and the window manager holds the window to it —
    /// a drag in flight included. A folder the window has
    /// just moved to is fitted afresh, as a new window there would open —
    /// never taller than the listing, and grown back to the opening height
    /// unless its user made it taller still. A listing still being read
    /// changes nothing until it lands.
    fn fit_to_listing(
        win: &mut OpenWindow,
        client: &mut WindowClient<app::RtWindowTransport>,
        grounds: Grounds<'_>,
        scale: Scale,
    ) -> bool {
        let OpenWindow {
            pane,
            surface,
            kind: WindowKind::Browser(state),
            ..
        } = win
        else {
            return false;
        };
        let mode = *pane.mode();
        let Some(fitted) = tairix_browse::render::fitted_height(
            &state.browser,
            mode.width_px,
            scale,
            grounds.window,
            state.chrome.rail(&state.places),
            state.chrome.toolbar,
        ) else {
            return false;
        };
        let floor = browser_floor(
            state.fit.floor_width,
            state.browser.view_mode(),
            state.chrome.toolbar,
            scale,
            grounds.window,
        );
        let sizing = fitted_sizing(floor, fitted);
        let moved = !state.browser.is_at(&state.fit.location);
        if moved {
            state.fit.location = state.browser.location();
        }
        let mut resized = false;
        if moved && state.fit.restored {
            let height = sizing
                .max_height_px()
                .min(mode.height_px.max(state.fit.opening))
                .max(sizing.min_height_px());
            if height != mode.height_px {
                resized = pane.resize_with(client, &app::mode_for(mode.width_px, height), surface)
                    == app::Fitted::Adopted;
            }
        }
        if sizing != state.fit.sizing {
            match client.set_sizing(pane.id(), sizing) {
                Ok(()) => state.fit.sizing = sizing,
                Err(err) => report_error(&alloc::format!(
                    "the desktop refused this window's resize range ({err}); it keeps the \
                     range it had"
                )),
            }
        }
        resized
    }

    /// Carry every window over to a desktop change: the artwork cache is held
    /// to the screen as it now is, a browser window asks for its blur again and
    /// re-reads its floor and opening height at the new density, and each
    /// window — and any chooser over one — owes the whole of itself in the new
    /// theme.
    fn owe_desktop(
        windows: &mut [OpenWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        (desktop, icons): (&Desktop, &RefCell<IconPipeline>),
        grounds: Grounds<'_>,
    ) {
        icons.borrow_mut().rescale(screen_bytes(desktop));
        let opening = desktop.window_size(WIN_WIDTH, WIN_HEIGHT).1;
        let floor_width = win_floor_width(desktop.scale(), grounds.window);
        for win in windows {
            let id = win.pane.id();
            win.owed.owe_whole();
            if matches!(win.kind, WindowKind::Progress(_)) {
                let (width, height) = progress_window_extent(desktop.scale(), grounds.popups);
                resize_window(win, client, width, height);
            }
            if let Some(state) = win.browser() {
                state.fit.opening = opening;
                state.fit.floor_width = floor_width;
                apply_backdrop(client, id, grounds);
                if let Some(overlay) = state.overlays.open_with.as_mut() {
                    overlay.owed = true;
                }
            }
        }
    }

    /// Close the window at `index`, which never ends the process.
    ///
    /// Neither role *is* its windows: both keep an icon-bar slot with none
    /// open, so closing every window is the user putting the file manager
    /// away rather than quitting it. An ordinary one is ended by the *Quit*
    /// row its slot carries and a component by nothing at all — the
    /// desktop's own parts are not the user's to close.
    fn close_window(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &Reads,
    ) {
        if index >= windows.len() {
            return;
        }
        let mut closed = windows.remove(index);
        // A window's outstanding reads are dropped with it: an answer for a
        // window that has gone belongs to nobody.
        let id = closed.pane.id();
        match &mut closed.kind {
            WindowKind::Browser(win) => {
                reads.forget_listing(win.listing);
                // Its progress windows are its dialogs, which the desktop
                // closes with it: their operations stop with them.
                while let Some(at) = windows.iter().position(|win| {
                    matches!(&win.kind, WindowKind::Progress(progress) if progress.owner == id)
                }) {
                    let mut owned = windows.remove(at);
                    let owned_id = owned.pane.id();
                    if let Some(progress) = owned.progress() {
                        stop_operation(owned_id, &mut progress.operation, reads);
                    }
                }
            }
            WindowKind::Properties(_) => {
                reads.forget_properties(id);
                reads.forget_drive(id);
            }
            WindowKind::Progress(progress) => stop_operation(id, &mut progress.operation, reads),
            WindowKind::Settings(_) => {}
        }
        let _ = closed.pane.close(client);
    }

    /// Operation `id` stops with the window showing it: it never runs if it
    /// waits, stops at its next unit if it runs, and one carried out on this
    /// loop ends here, the file it was writing removed and the decodes held
    /// for what it was writing let run.
    fn stop_operation(id: u64, operation: &mut Operation, reads: &Reads) {
        reads.forget_operation(id);
        if let Some(job) = operation.here.as_mut() {
            job.abandon();
            job.release_hold(&mut reads.work.lock().artwork);
        }
    }

    /// A one-shot write a browser window asked for, and what its answer does.
    struct Writing {
        /// The call, until the loop hands it to the reader.
        act: Option<WriteAct>,
        /// Its ticket, once handed over.
        ticket: Option<u64>,
        /// What its answer does.
        then: BrowserWrite,
    }

    impl Writing {
        fn new(act: WriteAct, then: BrowserWrite) -> Self {
            Self {
                act: Some(act),
                ticket: None,
                then,
            }
        }
    }

    /// What a browser window does with a write's answer.
    enum BrowserWrite {
        /// New ▸ made this entry: on success it is selected and renamed.
        Entry(PendingEntry),
        /// Go to Trash made sure the Trash exists: on success it is shown.
        Trash(alloc::vec::Vec<String>),
        /// Delete asked whether the targets of this plan can go to Trash: the
        /// answer opens the confirmation worded for what will happen.
        Delete(DeletePlan),
        /// Empty Trash read what this Trash holds: the answer opens the
        /// confirmation to remove it.
        EmptyTrash(alloc::vec::Vec<String>),
    }

    impl BrowserWrite {
        /// The title a refusal of this write is stated under.
        const fn refusal_title(&self) -> &'static str {
            match self {
                Self::Entry(_) => NOT_MADE,
                Self::Trash(_) => NO_TRASH,
                Self::Delete(_) => NOT_DELETED,
                Self::EmptyTrash(_) => NOT_EMPTIED,
            }
        }
    }

    /// Bring the entry a file shown in `win` chose into view, against the size
    /// the window was just fitted to. One whose listing does not have it yet
    /// waits for one that does.
    fn bring_chosen_into_view(win: &mut OpenWindow, grounds: Grounds<'_>, scale: Scale) {
        let mode = *win.pane.mode();
        let Some(state) = win.browser() else {
            return;
        };
        if !state.reveal_chosen || state.browser.focus_pending().is_some() {
            return;
        }
        state.reveal_chosen = false;
        let frame = frame_of(state, &mode, grounds.window, scale);
        tairix_browse::render::reveal_selection(
            &mut state.browser,
            frame.scale,
            frame.theme,
            frame.viewport,
            frame.toolbar,
        );
        win.owed.owe_whole();
    }

    /// Select the entry `name` alone in `win`'s listing, to be brought into
    /// view at its next present.
    fn reveal_entry(win: &mut OpenWindow, name: &str) {
        if let Some(state) = win.browser() {
            state.browser.follow(name);
            state.reveal_chosen = true;
        }
        win.owed.owe_whole();
    }

    /// A browser window's frame geometry, as its listing is drawn.
    fn frame_of<'a>(
        state: &BrowserWindow,
        mode: &DisplayMode,
        theme: &'a Theme,
        scale: Scale,
    ) -> Frame<'a> {
        Frame {
            scale,
            theme,
            viewport: Canvas {
                theme,
                mode,
                scale,
                chrome: state.chrome,
            }
            .viewport(&state.places),
            toolbar: state.chrome.toolbar,
        }
    }

    /// Hand each browser window's newly asked write to the reader. With no
    /// reader the write was carried out here, and its answer is taken at once.
    fn submit_writes(windows: &mut [OpenWindow], reads: &Reads, theme: &Theme, scale: Scale) {
        for win in windows.iter_mut() {
            let mode = *win.pane.mode();
            let Some(state) = win.browser() else {
                continue;
            };
            let Some(act) = state
                .overlays
                .writing
                .as_mut()
                .and_then(|writing| writing.act.take())
            else {
                continue;
            };
            match reads.write(act) {
                WriteSubmit::Pending(ticket) => {
                    if let Some(writing) = state.overlays.writing.as_mut() {
                        writing.ticket = Some(ticket);
                    }
                }
                WriteSubmit::Done(answer) => {
                    let Some(writing) = state.overlays.writing.take() else {
                        continue;
                    };
                    let frame = frame_of(state, &mode, theme, scale);
                    if apply_browser_write(state, writing.then, answer, frame) {
                        win.owed.owe_whole();
                    }
                }
                WriteSubmit::Unsent => {
                    let Some(writing) = state.overlays.writing.take() else {
                        continue;
                    };
                    state.overlays.notices.refuse(
                        NoticeSource::Write,
                        writing.then.refusal_title(),
                        WRITES_WAITING,
                    );
                    win.owed.owe_whole();
                }
            }
        }
    }

    /// Act on the answer to a New ▸ entry, answering whether the window
    /// changed: the made entry is named in place, a refusal stated naming it.
    fn finish_new_entry(
        state: &mut BrowserWindow,
        pending: &tairix_browse::PendingEntry,
        made: Result<(), Errno>,
        frame: Frame<'_>,
    ) -> bool {
        match state.browser.finish_entry(pending, made) {
            Ok(true) if state.browser.focus_pending().is_some() => {
                state.overlays.rename_on_arrival = state
                    .browser
                    .folder()
                    .map(|folder| (folder.to_vec(), String::from(pending.name())));
                false
            }
            Ok(true) => {
                begin_rename(
                    &mut state.browser,
                    &mut state.overlays.rename,
                    frame.scale,
                    frame.theme,
                    frame.viewport,
                    frame.toolbar,
                )
                .0
            }
            Ok(false) => false,
            Err(err) => {
                let reason = match err {
                    tairix_browse::CreateError::Refused(errno) => {
                        item_refusal("make", &[pending.name()], create_refusal(errno))
                    }
                    other => {
                        alloc::format!("\u{201c}{}\u{201d}: {}", pending.name(), other.message())
                    }
                };
                report_error(&reason);
                state
                    .overlays
                    .notices
                    .refuse(NoticeSource::Write, NOT_MADE, &reason);
                true
            }
        }
    }

    /// Act on a browser window's write answer, answering whether the window
    /// changed.
    fn apply_browser_write(
        state: &mut BrowserWindow,
        then: BrowserWrite,
        answer: WriteAnswer,
        frame: Frame<'_>,
    ) -> bool {
        match then {
            BrowserWrite::Delete(_) => {
                let WriteAnswer::Planned(plan, moves) = answer else {
                    return false;
                };
                // Asked over a selection the user may have moved on from: a
                // confirmation opened over anything else would take a key
                // meant for that as its answer.
                if !state.awaits_confirmation_of(&plan) {
                    return false;
                }
                ask_delete(&mut state.overlays, plan, moves, state.confirm_trash);
                true
            }
            BrowserWrite::Entry(pending) => {
                finish_new_entry(state, &pending, answer.status(), frame)
            }
            BrowserWrite::EmptyTrash(trash) => {
                let WriteAnswer::TrashPlanned(planned) = answer else {
                    return false;
                };
                match planned {
                    Ok(Some(plan)) if state.awaits_empty_confirmation_of(&trash) => {
                        ask_delete(&mut state.overlays, plan, None, true);
                        true
                    }
                    Ok(_) => false,
                    Err(reason) => {
                        state
                            .overlays
                            .notices
                            .refuse(NoticeSource::Write, NOT_EMPTIED, reason);
                        true
                    }
                }
            }
            BrowserWrite::Trash(trash) => match answer.status() {
                Ok(()) => match state.browser.navigate_to(trash) {
                    Ok(true) => {
                        tairix_browse::render::reveal_selection(
                            &mut state.browser,
                            frame.scale,
                            frame.theme,
                            frame.viewport,
                            frame.toolbar,
                        );
                        true
                    }
                    Ok(false) => false,
                    Err(_) => {
                        let reason = "the Trash folder could not be opened";
                        state
                            .overlays
                            .notices
                            .refuse(NoticeSource::Write, NO_TRASH, reason);
                        true
                    }
                },
                Err(errno) => {
                    let reason = alloc::format!("the Trash folder is unavailable: {errno}");
                    state
                        .overlays
                        .notices
                        .refuse(NoticeSource::Write, NO_TRASH, &reason);
                    true
                }
            },
        }
    }

    /// What one operation came to this turn.
    enum Moved {
        /// It ended so.
        Ended(Finished),
        /// It has done more, or begun, to show now: copied out of the desk,
        /// which is let go before it is shown.
        Shown {
            reading: Reading,
            current: String,
            cancelling: bool,
            waiting: bool,
        },
        /// It completed, and its full bar has been on show long enough.
        Closed,
    }

    /// Hand each newly asked operation to the operations worker, and fold
    /// what the worker reports into the progress windows: a count, at most
    /// once a frame, and an operation's end.
    ///
    /// The desk is read in one critical section and acted on once the lock is
    /// let go, because ending an operation reads its folder's listing again
    /// through that same lock.
    fn adopt_operations(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &Reads,
        (motion, refresh): (MotionTheme, RefreshRate),
    ) {
        let now = tairix_rt::clock_get();
        let mut asked = false;
        let mut moved = alloc::vec::Vec::new();
        {
            let mut work = reads.work.lock();
            let desk = &mut work.operations;
            // Looked at from here, so a report landing while this runs still
            // owes a wake.
            desk.acknowledge();
            let mut holding = false;
            for win in windows.iter_mut() {
                let id = win.pane.id();
                let Some(progress) = win.progress() else {
                    continue;
                };
                let operation = &mut progress.operation;
                if let Some(closing) = operation.closing_ns {
                    if now >= closing {
                        moved.push((id, Moved::Closed));
                    }
                    continue;
                }
                if let Some(job) = operation.job.take() {
                    match desk.submit(id, job) {
                        Ok(()) => asked = true,
                        Err((job, Refusal::Stopping)) => operation.here = Some(job),
                        Err((_, Refusal::Busy | Refusal::Full)) => moved.push((
                            id,
                            Moved::Ended(Finished {
                                done: Tally::default(),
                                ending: Ending::Stopped(String::from(
                                    "too many operations are already waiting; try again once one finishes",
                                )),
                                omissions: Omissions::default(),
                            }),
                        )),
                    }
                    continue;
                }
                if let Some(finished) = desk.take(id) {
                    moved.push((id, Moved::Ended(finished)));
                    continue;
                }
                let Some(standing) = desk.standing(id) else {
                    continue;
                };
                let progress = &operation.progress;
                let shown = progress.is_cancel_requested();
                if !operation.differs(&standing.reading, standing.current)
                    && standing.waiting == progress.is_waiting()
                    && (shown || !standing.cancelling)
                {
                    // Nothing new by the frame: the next report wakes the loop.
                    operation.due_ns = None;
                    continue;
                }
                if progress_due(now, operation.shown_ns, refresh) {
                    moved.push((
                        id,
                        Moved::Shown {
                            reading: standing.reading,
                            current: String::from(standing.current),
                            cancelling: standing.cancelling,
                            waiting: standing.waiting,
                        },
                    ));
                } else {
                    operation.due_ns = Some(operation.shown_ns.saturating_add(refresh.frame_ns()));
                }
                holding = true;
            }
            // A count shown now, or held for the next frame, is next looked at
            // when that frame is due: no report owes a wake before it.
            if holding {
                desk.hold();
            }
        }
        if asked {
            reads.operation_signal.notify_one();
        }
        for (id, change) in moved {
            if let Some(index) = windows.iter().position(|win| win.pane.id() == id) {
                apply_moved(
                    windows,
                    index,
                    client,
                    reads,
                    change,
                    (now, motion, refresh),
                );
            }
        }
    }

    /// Act at `now_ns` on what the operation of the progress window at `index`
    /// came to.
    fn apply_moved(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &Reads,
        change: Moved,
        (now_ns, motion, refresh): (u64, MotionTheme, RefreshRate),
    ) {
        match change {
            Moved::Ended(finished) => {
                finish_operation(windows, index, client, reads, finished, (now_ns, motion));
            }
            Moved::Closed => close_window(windows, index, client, reads),
            Moved::Shown {
                reading,
                current,
                cancelling,
                waiting,
            } => {
                let win = &mut windows[index];
                let Some(progress) = win.progress() else {
                    return;
                };
                let operation = &mut progress.operation;
                operation.progress.set_waiting(waiting);
                operation.observe(reading, &current, now_ns);
                if cancelling {
                    operation.progress.request_cancel();
                }
                operation.shown_ns = now_ns;
                operation.due_ns = Some(now_ns.saturating_add(refresh.frame_ns()));
                win.owed.owe_whole();
            }
        }
    }

    /// End the operation the progress window at `index` shows, on its answer:
    /// the window closes, the folder that started it is read again where
    /// nothing follows it, so a partial run shows what actually remains, and
    /// a refusal is stated in that folder's window, and on `stderr` for the
    /// record.
    fn end_operation(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &Reads,
        finished: Finished,
    ) {
        let Some(progress) = windows.get_mut(index).and_then(OpenWindow::progress) else {
            return;
        };
        let (owner, stopped) = (progress.owner, progress.operation.progress.stopped_title());
        close_window(windows, index, client, reads);
        reread_owner(windows, owner, stopped, finished);
    }

    /// End the operation the progress window at `index` shows, on its answer
    /// at `now_ns`. One that completed in a window on show keeps the window
    /// until its bar has been seen full; its folder is read again at once
    /// either way.
    fn finish_operation(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &Reads,
        finished: Finished,
        (now_ns, motion): (u64, MotionTheme),
    ) {
        let Some(progress) = windows.get_mut(index).and_then(OpenWindow::progress) else {
            return;
        };
        if !matches!(finished.ending, Ending::Completed) || !progress.operation.bar.on_show() {
            end_operation(windows, index, client, reads, finished);
            return;
        }
        let (owner, stopped) = (progress.owner, progress.operation.progress.stopped_title());
        let whole = Reading {
            done: finished.done,
            total: Some(finished.done),
            ..Reading::default()
        };
        progress.operation.observe(whole, "", now_ns);
        progress.operation.closing_ns = Some(completion_closes_at(now_ns, motion));
        windows[index].owed.owe_whole();
        reread_owner(windows, owner, stopped, finished);
    }

    /// Read the folder window `owner` again once an operation it started has
    /// ended, so a partial run shows what actually remains, and state a
    /// refusal that stopped it, and what its copies could not keep, there and
    /// on `stderr` for the record.
    fn reread_owner(windows: &mut [OpenWindow], owner: u64, stopped: &str, finished: Finished) {
        let Finished {
            ending, omissions, ..
        } = finished;
        let unkept = omissions.report(APP_NAME).map(|report| report.summary);
        if let Some(summary) = &unkept {
            report_error(&alloc::format!("warning: {summary}"));
        }
        let Some(win) = windows.iter_mut().find(|win| win.pane.id() == owner) else {
            if let Ending::Stopped(reason) = &ending {
                report_error(reason);
            }
            return;
        };
        let Some(state) = win.browser() else {
            return;
        };
        if !state.browser.follows() {
            if let Err(err) = state.browser.refresh() {
                state.listing_refused(err);
            }
        }
        if let Ending::Stopped(reason) = ending {
            report_error(&reason);
            state
                .overlays
                .notices
                .refuse(NoticeSource::Operation, stopped, &reason);
        }
        if let Some(summary) = unkept {
            state
                .overlays
                .notices
                .warn(NoticeSource::Omission, NOT_ALL_KEPT, &summary);
        }
        win.owed.owe_whole();
    }

    /// The title of the notice a copy that left extended attributes behind is
    /// stated in.
    const NOT_ALL_KEPT: &str = "Not every attribute was kept";

    /// Give each operation a folder window asked for this round a progress
    /// window of its own — modeless, a dialog of that folder's window — and
    /// present it. One the desktop will not open is not run: an operation
    /// with no Cancel to press is not one to start, and the refusal is stated
    /// in the folder's window.
    fn open_started_operations(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        client: &mut WindowClient<app::RtWindowTransport>,
        (desktop, grounds, icons): (&Desktop, Grounds<'_>, &RefCell<IconPipeline>),
        event_endpoint: u64,
    ) {
        let mut index = 0;
        while index < windows.len() {
            let owner = windows[index].pane.id();
            let Some(operation) = windows[index]
                .browser()
                .and_then(|state| state.overlays.starting.take())
            else {
                index += 1;
                continue;
            };
            match open_progress(operation, owner, client, (desktop, grounds), event_endpoint) {
                Ok(mut win) => {
                    if present_whole(&mut win, client, grounds, icons, desktop.scale()).is_err() {
                        report_error("an operation's progress window could not be painted");
                    }
                    windows.push(win);
                }
                Err((title, why)) => {
                    report_error(&alloc::format!("{title} not begun: {why}"));
                    if let Some(state) = windows[index].browser() {
                        let reason = alloc::format!("its progress could not be shown ({why})");
                        state
                            .overlays
                            .notices
                            .refuse(NoticeSource::Operation, title, &reason);
                    }
                    windows[index].owed.owe_whole();
                }
            }
            index += 1;
        }
    }

    /// Open `operation`'s progress window as a modeless dialog of window
    /// `owner`, or say why it could not be, with what the operation is called.
    fn open_progress(
        operation: Operation,
        owner: u64,
        client: &mut WindowClient<app::RtWindowTransport>,
        (desktop, grounds): (&Desktop, Grounds<'_>),
        event_endpoint: u64,
    ) -> Result<OpenWindow, (&'static str, String)> {
        let title = operation.progress.title();
        let stopped = operation.progress.stopped_title();
        let (width, height) = progress_window_extent(desktop.scale(), grounds.popups);
        let mode = app::mode_for(width, height);
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            return Err((stopped, String::from("no memory to draw it")));
        };
        let Some(server) = client.session() else {
            return Err((stopped, String::from("the desktop session is unknown")));
        };
        let pane = WindowPane::open_dialog(
            client,
            (owner, server),
            event_endpoint,
            &mode,
            DialogModality::Modeless,
            title,
        )
        .map_err(|err| (stopped, alloc::format!("{err}")))?;
        Ok(OpenWindow {
            pane,
            title: String::from(title),
            surface,
            owed: Owed::new(),
            kind: WindowKind::Progress(Box::new(ProgressWindow { owner, operation })),
        })
    }

    /// Serve what falls due with no event to bring it, answering whether any
    /// window moved: a band held where its listing scrolls steps it, a click
    /// on a chosen name whose double-click interval has run out opens the
    /// rename it armed, a held scrollbar press steps its column, and a drive
    /// window on show asks for its next reading.
    fn serve_due(
        windows: &mut [OpenWindow],
        reads: &Reads,
        theme: &Theme,
        (scale, refresh): (Scale, RefreshRate),
    ) -> bool {
        let now = tairix_rt::clock_get();
        let mut moved = false;
        for win in windows {
            let mode = *win.pane.mode();
            let id = win.pane.id();
            let mut damage = Region::new();
            let repaint = match &mut win.kind {
                WindowKind::Browser(state) => {
                    let canvas = Canvas {
                        theme,
                        mode: &mode,
                        scale,
                        chrome: state.chrome,
                    };
                    let stepped = state.step_band(canvas, (now, refresh), &mut damage);
                    stepped
                        .merged(state.open_due_rename(canvas, now, &mut damage))
                        .merged(state.step_scrollbars(canvas, now, &mut damage))
                }
                WindowKind::Properties(props) => {
                    if let Some(job) = props.take_due_drive(now) {
                        reads.want_drive(id, job);
                    }
                    let window = Rect::new(0, 0, mode.width_px, mode.height_px);
                    Repaint::reported_if(props.step_scroll(
                        (window, scale, theme),
                        now,
                        &mut damage,
                    ))
                }
                // A reading is shown as it is adopted; what moves on the clock
                // is the bar, drawn alone when its frame falls due.
                WindowKind::Progress(progress) => {
                    let operation = &mut progress.operation;
                    if operation.bar.due(now) {
                        operation.trace.set_clock(now, theme.motion(), refresh);
                        operation.bar.drawn(now, operation.trace.next_frame_in());
                        let client = Rect::new(0, 0, mode.width_px, mode.height_px);
                        damage.add(progress_bar_rect(client, scale, theme));
                        Repaint::Reported
                    } else {
                        Repaint::Nothing
                    }
                }
                WindowKind::Settings(page) => {
                    let window = Rect::new(0, 0, mode.width_px, mode.height_px);
                    Repaint::reported_if(page.step_scroll(now, window, (scale, theme), &mut damage))
                }
            };
            // The bar's next frame is its own deadline, so drawing one never
            // keeps the loop turning.
            moved |= repaint != Repaint::Nothing && !matches!(win.kind, WindowKind::Progress(_));
            owe(win, repaint, &damage);
        }
        moved
    }

    /// Serve one event addressed to the progress window at `index`: its
    /// Cancel or Escape asks the operation to stop before its next unit — one
    /// not yet handed to the worker ends at once, having done nothing — and a
    /// redraw request is answered.
    fn serve_progress_event(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        (theme, scale): (&Theme, Scale),
        reads: &Reads,
        event: &WindowEvent,
    ) {
        let Some(win) = windows.get_mut(index) else {
            return;
        };
        let mode = *win.pane.mode();
        let id = win.pane.id();
        let client_rect = Rect::new(0, 0, mode.width_px, mode.height_px);
        let Some(progress) = win.progress() else {
            return;
        };
        match progress_control(client_rect, scale, theme, event) {
            // Done already: there is nothing left to cancel, so it closes.
            ProgressControl::Cancel if progress.operation.closing_ns.is_some() => {
                close_window(windows, index, client, reads);
            }
            ProgressControl::Cancel if progress.operation.job.is_some() => {
                let cancelled = Finished {
                    done: Tally::default(),
                    ending: Ending::Cancelled,
                    omissions: Omissions::default(),
                };
                end_operation(windows, index, client, reads, cancelled);
            }
            ProgressControl::Cancel => {
                progress.operation.progress.request_cancel();
                reads.cancel_operation(id);
                win.owed.owe_whole();
            }
            ProgressControl::Redraw => win.owed.owe_whole(),
            ProgressControl::Ignore => {}
        }
    }

    /// What routing an icon-bar event did.
    enum BarRouted {
        /// Not the icon bar's event at all; the caller resolves it against a
        /// window instead.
        NotMine,
        /// Handled; the process carries on.
        Handled,
        /// Handled, and the process owes this exit code.
        Ends(i32),
    }

    /// Route one icon-bar event — an event that names the *application* rather
    /// than a window.
    ///
    /// The slot outlives every window, and a component's slot is often all
    /// there is, so these are resolved before anything asks which window an
    /// event belongs to.
    fn route_app_bar_event(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        (opening, kept): (&Opening, &RefCell<Kept>),
        client: &mut WindowClient<app::RtWindowTransport>,
        places: &Places,
        reads: &alloc::sync::Arc<Reads>,
        (role, event_endpoint, surround): (Role, u64, Surround<'_>),
        event: &WindowEvent,
    ) -> BarRouted {
        let settings = *kept.borrow().live();
        match *event {
            WindowEvent::AppBarDefault => {
                // A window at the user's home — the readiest thing a file
                // manager can offer. A component takes every click; an
                // ordinary one is told only when it has no window left, and
                // this is the way back to one.
                open_more(windows, opening, client, reads, (None, &settings));
                BarRouted::Handled
            }
            WindowEvent::OpenRequested => {
                // A wake, not a path: the desktop queued at least one folder
                // for this instance, so drain until the queue answers empty.
                // One event may cover several, and another may arrive while
                // this drain is still running.
                drain_open_targets(windows, opening, client, reads, &settings);
                BarRouted::Handled
            }
            WindowEvent::AppBarMenu { item } if appbar::is_settings(item) => {
                open_settings(windows, client, surround, event_endpoint, kept);
                BarRouted::Handled
            }
            WindowEvent::AppBarMenu { item } if appbar::is_network(item) => {
                open_network(windows, opening, client, reads, &settings);
                BarRouted::Handled
            }
            // A component's rows are its places: open a window at the one
            // chosen. A stale row — the rail was re-read since the menu was
            // declared — names nothing and does nothing, rather than opening
            // somewhere the user did not point at.
            WindowEvent::AppBarMenu { item } if role == Role::Desktop => {
                let location = appbar::place_of(item)
                    .and_then(|index| places.rows().get(index))
                    .map(|place| place.components().to_vec());
                if location.is_some() {
                    open_more(windows, opening, client, reads, (location, &settings));
                }
                BarRouted::Handled
            }
            // An ordinary file manager declares the shared convention, whose
            // one command closes every window and ends the process. A row it
            // never declared names nothing.
            WindowEvent::AppBarMenu { item } => {
                if !tairix_window::is_quit(item) {
                    return BarRouted::Handled;
                }
                for win in windows.drain(..) {
                    let _ = win.pane.close(client);
                }
                BarRouted::Ends(0)
            }
            _ => BarRouted::NotMine,
        }
    }

    /// Serve `event` where it is no listing's: one addressed to a Properties,
    /// progress or settings window, or `Ctrl+,` asking for the settings in a
    /// listing's. Answers whether it was.
    fn route_dialog(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        (reads, kept, endpoint): (&Reads, &RefCell<Kept>, u64),
        surround: Surround<'_>,
        event: &WindowEvent,
    ) -> bool {
        let (desktop, grounds, _) = surround;
        let (scale, theme) = (desktop.scale(), grounds.popups);
        match windows.get(index).map(|win| &win.kind) {
            Some(WindowKind::Properties(_)) => {
                route_properties_event(windows, index, client, theme, reads, scale, event);
                return true;
            }
            Some(WindowKind::Progress(_)) => {
                serve_progress_event(windows, index, client, (theme, scale), reads, event);
                return true;
            }
            Some(WindowKind::Settings(_)) => {
                route_settings_event(windows, index, client, reads, kept, (scale, theme), event);
                return true;
            }
            Some(WindowKind::Browser(_)) | None => {}
        }
        let WindowEvent::Key {
            key: KeyInput::Pressed { key, modifiers },
            ..
        } = *event
        else {
            return false;
        };
        if !crate::chrome::opens_settings(key, modifiers) {
            return false;
        }
        open_settings(windows, client, surround, endpoint, kept);
        true
    }

    /// What a window is opened against: the desktop it appears on, the grounds
    /// it is drawn over, and the artwork it draws through.
    type Surround<'a> = (&'a Desktop, Grounds<'a>, &'a RefCell<IconPipeline>);

    /// Open the settings window on the settings in force, or bring the one
    /// already open forward: the settings are one record, so one window edits
    /// them.
    fn open_settings(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        client: &mut WindowClient<app::RtWindowTransport>,
        (desktop, grounds, icons): Surround<'_>,
        event_endpoint: u64,
        kept: &RefCell<Kept>,
    ) {
        if let Some(open) = windows
            .iter()
            .find(|win| matches!(win.kind, WindowKind::Settings(_)))
        {
            raise_showing(client, open.pane.id());
            return;
        }
        let editor = SettingsWindow::new(kept.borrow().live());
        let scale = desktop.scale();
        let screen = desktop.screen();
        let (width, height) = editor.extent(scale, grounds.popups);
        let (width, height) = (width.min(screen.width), height.min(screen.height));
        let mode = app::mode_for(width, height);
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            report_error("window surface refused; no settings window opened");
            return;
        };
        // As tall as the page and no taller, so no blank band opens beneath
        // it; shorter, and the page scrolls.
        let sizing = WindowSizing::Resizable {
            min_width_px: width,
            min_height_px: height.min(scale.scale_length(SETTINGS_MIN_HEIGHT)),
            max_width_px: 0,
            max_height_px: height,
        };
        let pane = match WindowPane::open(client, event_endpoint, &mode, SETTINGS_TITLE, sizing) {
            Ok((pane, _)) => pane,
            Err(err) => {
                report_error(&alloc::format!("{err}; no settings window opened"));
                return;
            }
        };
        let mut win = OpenWindow {
            pane,
            title: String::from(SETTINGS_TITLE),
            surface,
            owed: Owed::new(),
            kind: WindowKind::Settings(Box::new(editor)),
        };
        if present_whole(&mut win, client, grounds, icons, scale).is_err() {
            let _ = win.pane.close(client);
            report_error("the settings window could not be painted; it was closed again");
            return;
        }
        windows.push(win);
    }

    /// The least the settings window may be made, in logical pixels.
    const SETTINGS_MIN_HEIGHT: u32 = 240;

    /// Serve one event addressed to the settings window at `index`, carrying
    /// out what it asks for.
    fn route_settings_event(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &Reads,
        kept: &RefCell<Kept>,
        place: (Scale, &Theme),
        event: &WindowEvent,
    ) {
        let Some(win) = windows.get_mut(index) else {
            return;
        };
        let mode = *win.pane.mode();
        let window = Rect::new(0, 0, mode.width_px, mode.height_px);
        let mut damage = damage::sink();
        let WindowKind::Settings(page) = &mut win.kind else {
            return;
        };
        let asked = match event {
            WindowEvent::CloseRequested { .. } => Some(SettingsRequest::Close),
            WindowEvent::Key {
                key: KeyInput::Pressed { key, modifiers },
                ..
            } => {
                let (key, modifiers) = to_editor_key(*key, *modifiers);
                let stroke = Keystroke {
                    key,
                    modifiers,
                    at_ns: tairix_rt::clock_get(),
                };
                page.on_key(stroke, window, place, &mut damage)
            }
            WindowEvent::Pointer { x, y, action, .. } => {
                let point = pointer_point(*x, *y);
                let mut asked = None;
                for input in pointer_input_events(*action, point) {
                    asked = page
                        .on_pointer((point, &input), window, place, &mut damage)
                        .or(asked);
                }
                asked
            }
            WindowEvent::Scrolled { dx, dy, .. } => {
                page.on_wheel((*dx, *dy), window, place, &mut damage);
                None
            }
            _ => None,
        };
        owe(win, Repaint::Reported, &damage);
        match asked {
            Some(SettingsRequest::Close) => close_window(windows, index, client, reads),
            Some(SettingsRequest::Edit { was, now }) => {
                kept.borrow_mut().publication.edit(&was, &now);
                settle_settings(windows, kept, false);
            }
            Some(SettingsRequest::Restore) => settle_settings(windows, kept, true),
            None => {}
        }
    }

    /// Ask for the settings edited to be written — or, `restoring`, for the
    /// user's own to be removed — off the loop, and show every window what is
    /// in force meanwhile.
    fn settle_settings(windows: &mut [OpenWindow], kept: &RefCell<Kept>, restoring: bool) {
        {
            let mut kept = kept.borrow_mut();
            let Kept {
                publication,
                writer,
            } = &mut *kept;
            let job = if restoring {
                publication.restore()
            } else {
                publication.settle()
            };
            if let Some(job) = job {
                // Carried out at once with no worker to take it: the answer is
                // there to adopt below.
                let _ = writer.submit(job);
            }
        }
        adopt_settings(windows, kept);
    }

    /// Adopt what the settings store has answered, and show every window the
    /// settings then in force, answering whether any window changed.
    fn adopt_settings(windows: &mut [OpenWindow], kept: &RefCell<Kept>) -> bool {
        let mut kept = kept.borrow_mut();
        let Kept {
            publication,
            writer,
        } = &mut *kept;
        let mut refusals = Vec::new();
        tairix_appdata::collect_published(&**writer, publication, &mut refusals);
        for refusal in &refusals {
            app::report(APP_NAME, refusal);
        }
        let said = refusals.last().map(|refusal| alloc::format!("{refusal}"));
        let (was, now) = publication.take_pending(|was, now| (*was, *now));
        if was == now && said.is_none() {
            return false;
        }
        show_settings(windows, &now, said.as_deref());
        true
    }

    /// Show every window `settings`: each listing as they present it, and the
    /// settings window them with `said`, the store's last refusal.
    fn show_settings(windows: &mut [OpenWindow], settings: &Preferences, said: Option<&str>) {
        for win in windows {
            let mode = *win.pane.mode();
            let window = Rect::new(0, 0, mode.width_px, mode.height_px);
            let mut damage = damage::sink();
            let repaint = match &mut win.kind {
                WindowKind::Browser(state) => whole_if(state.adopt_settings(settings)),
                WindowKind::Settings(page) => {
                    page.adopt(settings, window, &mut damage);
                    page.state(said.map(String::from), window, &mut damage);
                    Repaint::Reported
                }
                WindowKind::Properties(_) | WindowKind::Progress(_) => Repaint::Nothing,
            };
            owe(win, repaint, &damage);
        }
    }

    /// See the settings' writes out before the program ends, so the last change
    /// is never dropped with the worker.
    fn see_out(kept: &RefCell<Kept>) {
        let mut kept = kept.borrow_mut();
        let Kept {
            publication,
            writer,
        } = &mut *kept;
        let mut refusals = Vec::new();
        tairix_appdata::see_out_published(&**writer, publication, &mut refusals);
        for refusal in refusals {
            app::report(APP_NAME, refusal);
        }
    }

    /// End the program with `code`, its settings' writes seen out first.
    fn ended(kept: &RefCell<Kept>, code: i32) -> i32 {
        see_out(kept);
        code
    }

    /// Route one event to whatever it names, answering the exit code the
    /// process owes or `None` to carry on.
    ///
    /// The icon bar's own outcomes are resolved first
    /// ([`route_app_bar_event`]). Everything else is window-scoped: an id no
    /// live window — *or popup a window holds* — carries is a window that has
    /// just closed, and the event has nowhere to land.
    ///
    /// A popup is a window of its own with its own id, so resolving ids
    /// against the window list alone dropped every key and every click
    /// delivered to the "Open With…" chooser, leaving it on screen and inert.
    /// A popup's events resolve to the window that owns it, which then routes
    /// them to the overlay.
    #[allow(clippy::too_many_arguments)] // The run's whole mutable state, threaded explicitly.
    fn route_event(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        opening: &Opening,
        client: &mut WindowClient<app::RtWindowTransport>,
        desktop: &mut Desktop,
        places: &mut Places,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        launcher: &RefCell<Launcher>,
        (reads, places_read): (&alloc::sync::Arc<Reads>, &PlacesRead),
        installed: &Installed,
        event_endpoint: u64,
        role: Role,
        can_chown: bool,
        kept: &RefCell<Kept>,
        event: &WindowEvent,
    ) -> Option<i32> {
        let how = (role, event_endpoint, (&*desktop, grounds, icons));
        match route_app_bar_event(windows, (opening, kept), client, places, reads, how, event) {
            BarRouted::Handled => return None,
            BarRouted::Ends(code) => return Some(code),
            BarRouted::NotMine => {}
        }
        // An alert answers the application; the window that asked is found by
        // the alert it holds.
        if let WindowEvent::AlertClosed { alert_id, outcome } = *event {
            if !launcher.borrow_mut().notices.answered(alert_id) {
                answer_alert(windows, alert_id, outcome);
            }
            return None;
        }
        // Where a drag is, and where it ended, may name a window other than the
        // one it began in, so both are answered where every window is.
        if matches!(
            event,
            WindowEvent::DragOver { .. } | WindowEvent::DragEnded { .. }
        ) {
            route_drag(
                windows,
                client,
                grounds.popups,
                launcher,
                desktop.scale(),
                event,
            );
            return None;
        }
        let window_id = event.window_id()?;
        let addressed: Vec<(u64, Option<u64>)> = windows
            .iter()
            .map(|win| (win.pane.id(), win.popup_id()))
            .collect();
        let (index, surface) = route::addressee(addressed, window_id)?;

        // An event addressed to a held popup is the popup's, not its owner's:
        // routing it on would resize the parent's surface, release the
        // parent's frames, or close the parent outright.
        if surface == route::Addressed::Popup {
            return route_popup_event(
                windows,
                index,
                client,
                desktop,
                launcher,
                grounds.popups,
                event,
            );
        }
        if let Some(progress) = windows[index].progress() {
            progress.operation.bar.observe(event);
        }

        if let WindowEvent::Resized {
            width_px,
            height_px,
            state,
            ..
        } = *event
        {
            if let Some(browser) = windows[index].browser() {
                browser.fit.restored = state == WindowSizeState::Restored;
            }
            resize_window(&mut windows[index], client, width_px, height_px);
            return None;
        }

        // Nobody can see the window, so the session gave its copy of the pixels
        // back and unmapped the region. Let go of this side too — the pages go
        // only when both do — and paint nothing: the redraw request that
        // follows the window being shown again is what re-attaches a fresh
        // region and fills it.
        if matches!(event, WindowEvent::ContentReleased { .. }) {
            windows[index].pane.release_frames();
            return None;
        }

        let surround = (&*desktop, grounds, icons);
        if route_dialog(
            windows,
            index,
            client,
            (reads, kept, event_endpoint),
            surround,
            event,
        ) {
            return None;
        }
        route_browser_event(
            windows,
            index,
            client,
            desktop,
            grounds,
            icons,
            launcher,
            (reads, places_read),
            installed,
            event_endpoint,
            can_chown,
            event,
        )
    }

    /// Answer the desktop's report of where this app's drag is, or act on
    /// where it ended.
    ///
    /// A report is answered with what a drop there would do — the one drop
    /// policy over the folder the point names — and the folder tile under the
    /// pointer is lit while the answer accepts it. A drop on one of this app's
    /// windows or on the desktop runs as a paste into that folder, in the
    /// window dropped on (or, on the desktop, the window the drag began in);
    /// a drop on an application opens the one file for it, exactly as its
    /// "Open With" row would.
    fn route_drag(
        windows: &mut [OpenWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        theme: &Theme,
        launcher: &RefCell<Launcher>,
        scale: Scale,
        event: &WindowEvent,
    ) {
        let Some(source) = windows.iter_mut().position(|win| {
            win.browser()
                .is_some_and(|state| state.overlays.carrying.is_some())
        }) else {
            return;
        };
        let source_id = windows[source].pane.id();
        match *event {
            WindowEvent::DragOver {
                window_id,
                serial,
                at,
                shift,
            } => {
                let (folder, tile) = match at {
                    DragAt::Window { x, y } => windows
                        .iter_mut()
                        .find(|win| win.pane.id() == window_id)
                        .and_then(|win| window_drop_folder(win, x, y, scale, theme))
                        .map_or((None, None), |(folder, tile)| {
                            (Some(folder), tile.map(|tile| (window_id, tile)))
                        }),
                    DragAt::Desktop => (desktop_drop_folder(client, source_id, serial), None),
                    DragAt::Nowhere => (None, None),
                };
                let verdict = windows[source]
                    .browser()
                    .and_then(|state| state.overlays.carrying.as_ref())
                    .zip(folder)
                    .and_then(|(carrying, folder)| {
                        drop_operation(&carrying.sources, &folder, shift)
                    })
                    .map(|op| match op {
                        ClipboardOp::Copy => DropOperation::Copy,
                        ClipboardOp::Cut => DropOperation::Move,
                    });
                light_drop_tile(windows, theme, scale, verdict.and(tile));
                // A drag that ended after this report was sent answers
                // nothing; its end is already on its way.
                match client.drag_verdict(source_id, serial, verdict) {
                    Ok(()) | Err(Errno::NotFound) => {}
                    Err(err) => {
                        report_error(&alloc::format!("the drag's answer was refused ({err})"));
                    }
                }
            }
            WindowEvent::DragEnded { site, .. } => {
                let carrying = windows[source]
                    .browser()
                    .and_then(|state| state.overlays.carrying.take());
                light_drop_tile(windows, theme, scale, None);
                let Some(carrying) = carrying else {
                    return;
                };
                match site {
                    DropSite::Nothing => {}
                    DropSite::Application => {
                        open_dropped_file(client, launcher, source_id, carrying.file);
                    }
                    DropSite::Window {
                        window_id,
                        x,
                        y,
                        operation,
                    } => {
                        let op = clipboard_op(operation);
                        let Some(target) =
                            windows.iter().position(|win| win.pane.id() == window_id)
                        else {
                            let reason = "the window it was dropped on has closed";
                            refuse_drop(&mut windows[source], op, reason);
                            return;
                        };
                        let Some((folder, _)) =
                            window_drop_folder(&mut windows[target], x, y, scale, theme)
                        else {
                            let reason = "nothing there takes a drop";
                            refuse_drop(&mut windows[target], op, reason);
                            return;
                        };
                        begin_drop(&mut windows[target], carrying.sources, &folder, op);
                    }
                    DropSite::Desktop { serial, operation } => {
                        let op = clipboard_op(operation);
                        let Some(folder) = desktop_drop_folder(client, source_id, serial) else {
                            let reason = "the desktop folder it was dropped on is gone";
                            refuse_drop(&mut windows[source], op, reason);
                            return;
                        };
                        begin_drop(&mut windows[source], carrying.sources, &folder, op);
                    }
                }
            }
            _ => {}
        }
    }

    /// The folder a drop at window-local `(x, y)` in `win` lands in, and the
    /// listed folder that stands for it ([`drop_folder_at`]).
    fn window_drop_folder(
        win: &mut OpenWindow,
        x: u32,
        y: u32,
        scale: Scale,
        theme: &Theme,
    ) -> Option<(Vec<String>, Option<usize>)> {
        let mode = *win.pane.mode();
        let state = win.browser()?;
        let canvas = Canvas {
            theme,
            mode: &mode,
            scale,
            chrome: state.chrome,
        };
        drop_folder_at(
            &state.browser,
            scale,
            theme,
            canvas.viewport(&state.places),
            canvas.chrome.toolbar,
            pointer_point(x, y),
        )
    }

    /// The desktop folder the drag report numbered `serial` named, read from
    /// the session; `None` once the session has named another.
    fn desktop_drop_folder(
        client: &mut WindowClient<app::RtWindowTransport>,
        source_id: u64,
        serial: u32,
    ) -> Option<Vec<String>> {
        let path = client.drag_spot(source_id, serial).ok()?;
        tairix_browse::vfs::components_from_absolute_path(&path).ok()
    }

    /// Light `tile` — a window and a listed entry — as where the drag would
    /// drop, and put out whatever was lit before, repainting only the tiles
    /// that changed.
    fn light_drop_tile(
        windows: &mut [OpenWindow],
        theme: &Theme,
        scale: Scale,
        tile: Option<(u64, usize)>,
    ) {
        for win in windows.iter_mut() {
            let id = win.pane.id();
            let mode = *win.pane.mode();
            let Some(state) = win.browser() else {
                continue;
            };
            let lit = tile.and_then(|(window, index)| (window == id).then_some(index));
            let was = state.browser.set_drop_mark(lit);
            if was == lit {
                continue;
            }
            let canvas = Canvas {
                theme,
                mode: &mode,
                scale,
                chrome: state.chrome,
            };
            let viewport = canvas.viewport(&state.places);
            let mut damage = Region::new();
            for index in [was, lit].into_iter().flatten() {
                if let Some(rect) = tairix_browse::render::entry_rect(
                    &state.browser,
                    scale,
                    theme,
                    viewport,
                    canvas.chrome.toolbar,
                    index,
                ) {
                    damage.add(rect);
                }
            }
            owe(win, Repaint::Reported, &damage);
        }
    }

    /// Open the one file a drag carried for the application it was dropped
    /// on, exactly as its "Open With" row would.
    fn open_dropped_file(
        client: &mut WindowClient<app::RtWindowTransport>,
        launcher: &RefCell<Launcher>,
        source_id: u64,
        file: Option<PendingChooser>,
    ) {
        let Some(file) = file else {
            return;
        };
        let target = match client.take_drop_target(source_id) {
            Ok(target) => target,
            Err(err) => {
                report_error(&alloc::format!("the drop was lost ({err})"));
                return;
            }
        };
        let Some(bundle) = tairix_appstore::bundle_of_entry(target.run_path.as_str()) else {
            report_error("the drop names no application");
            return;
        };
        launcher.borrow_mut().launch_viewer(
            client,
            bundle,
            target.writes_documents,
            &file.path,
            &file.name,
        );
    }

    /// The paste a drop's `operation` asks for.
    const fn clipboard_op(operation: DropOperation) -> ClipboardOp {
        match operation {
            DropOperation::Copy => ClipboardOp::Copy,
            DropOperation::Move => ClipboardOp::Cut,
        }
    }

    /// Run a drop of `sources` into `folder` in `win`, as the paste the
    /// pointer showed. The drop policy is asked again first, so a drop the
    /// folder no longer allows moves nothing.
    fn begin_drop(win: &mut OpenWindow, sources: Vec<Listed>, folder: &[String], op: ClipboardOp) {
        let refused = if drop_operation(&sources, folder, op == ClipboardOp::Cut) == Some(op) {
            let Some(state) = win.browser() else {
                return;
            };
            Clipboard::new(op, sources)
                .and_then(|clipboard| start_paste(&clipboard, folder, &mut state.overlays).err())
        } else {
            Some(String::from("that folder no longer takes the drop"))
        };
        if let Some(reason) = refused {
            refuse_drop(win, op, &reason);
        }
    }

    /// State in `win` why a drop moved nothing.
    fn refuse_drop(win: &mut OpenWindow, op: ClipboardOp, reason: &str) {
        let Some(state) = win.browser() else {
            report_error(reason);
            return;
        };
        refuse_paste(&mut state.overlays, op, reason);
        win.owed.owe_whole();
    }

    /// Route one event delivered to a window's own held popup — the
    /// "Open With…" chooser.
    ///
    /// The chooser owns the whole event: it is a window of its own, so its
    /// close request closes *it*, its released content releases *its* region,
    /// and its keys and clicks never reach the listing behind it.
    #[allow(clippy::too_many_arguments)] // The window, its popup, and the event.
    fn route_popup_event(
        windows: &mut [OpenWindow],
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        desktop: &mut Desktop,
        launcher: &RefCell<Launcher>,
        theme: &Theme,
        event: &WindowEvent,
    ) -> Option<i32> {
        let win = windows.get_mut(index)?;
        let mode = *win.pane.mode();
        let WindowKind::Browser(state) = &mut win.kind else {
            return None;
        };
        let canvas = Canvas {
            theme,
            mode: &mode,
            scale: desktop.scale(),
            chrome: state.chrome,
        };
        let mut damage = damage::sink();
        let repaint = apply_chooser_event(
            &mut state.overlays,
            launcher,
            client,
            canvas,
            desktop.info().double_click(),
            event,
            &mut damage,
        );
        // The popup paints itself; the window behind it owes nothing, so a
        // round that changed only the chooser presents only the chooser.
        if let Some(overlay) = state.overlays.open_with.as_mut() {
            overlay.owed |= repaint;
        }
        None
    }

    /// Route one event to the browser window at `index`.
    ///
    /// Its own path because a listing carries everything a Properties window
    /// does not: the rail, the chrome bands, the overlays, and the gestures
    /// that launch, copy, and open.
    #[allow(clippy::too_many_arguments)] // The run's whole mutable state, threaded explicitly.
    fn route_browser_event(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        desktop: &mut Desktop,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        launcher: &RefCell<Launcher>,
        (reads, places_read): (&alloc::sync::Arc<Reads>, &PlacesRead),
        installed: &Installed,
        event_endpoint: u64,
        can_chown: bool,
        event: &WindowEvent,
    ) -> Option<i32> {
        let win = windows.get_mut(index)?;
        let window_id = win.pane.id();
        let double_click = desktop.info().double_click();
        let mut keyboard_held = false;
        if let Some(state) = win.browser() {
            state.note_pointer(event);
            let (held, keyboard) = state.keyboard.step(event);
            (keyboard_held, state.keyboard) = (held, keyboard);
            // Stepped before the event is routed, so the press that arms a
            // rename is not taken for the press that lets it go.
            let to = state.pointer.unwrap_or(Point::new(0, 0));
            // The wait runs from the release on the clock presses are paired by.
            let at_ns = match *event {
                WindowEvent::Pointer { at_ns, .. } => at_ns,
                _ => tairix_rt::clock_get(),
            };
            state.overlays.rename_arm = state.overlays.rename_arm.and_then(|arm| {
                arm.step(
                    PressInput::of(event),
                    to,
                    at_ns,
                    double_click,
                    desktop.scale(),
                )
            });
        }
        // The chrome toggle is a window-level gesture like the refresh below,
        // not a listing key: it changes what the frame is laid out from, so it
        // is applied to the record before this round's canvas is built and the
        // whole window is repainted at the new layout.
        let chrome_toggled = chrome_toggle(win, event);
        let WindowKind::Browser(state) = &win.kind else {
            return None;
        };
        // The mode is copied rather than borrowed, so the round's canvas does
        // not hold the pane while the gesture below takes the window.
        let mode = *win.pane.mode();
        let canvas = Canvas {
            theme: grounds.popups,
            mode: &mode,
            scale: desktop.scale(),
            chrome: state.chrome,
        };
        // The user asked this window to re-read what is there, so the rail is
        // re-read with the listing. What is mounted comes from the System
        // Information service, so it is asked for rather than read on this
        // loop, and it lands for every window — and a component's slot menu,
        // which *is* that rail — the way a mount notice's answer does.
        if sidebar::is_refresh_request(
            &state.browser,
            canvas.scale,
            canvas.theme(),
            canvas.window(),
            canvas.chrome.toolbar,
            event,
        ) {
            if let Some(found) = reads.want_places() {
                *places_read.borrow_mut() = Some(found);
            }
        }
        // Exact rather than budgeted: a band's edges and the tiles it selects
        // are far apart, and a sink that degraded to their bounding box would
        // repaint the whole band where the window's account merges them least.
        let mut damage = Region::new();
        let popup = PopupLink::of(client, desktop, event_endpoint);
        let mut properties = None;
        let state = win.browser()?;
        let (repaint, close) = apply_event(
            &mut WindowState {
                browser: &mut state.browser,
                overlays: &mut state.overlays,
                places: &mut state.places,
                pointer: state.pointer,
                keyboard_held,
            },
            &mut Acts {
                menu: MenuLink {
                    client,
                    window: window_id,
                    open: &mut state.menu,
                },
                launcher,
                reads,
                installed,
                popup,
                icons,
                properties: &mut properties,
                double_click,
            },
            canvas,
            event,
            &mut damage,
        );
        if close {
            close_window(windows, index, client, reads);
            return None;
        }
        owe(win, repaint.merged(whole_if(chrome_toggled)), &damage);
        // The window this round asked for, opened once its own window's borrow
        // has ended.
        if let Some(request) = properties {
            open_properties(
                request,
                windows,
                client,
                desktop,
                grounds,
                icons,
                reads,
                event_endpoint,
                can_chown,
            );
        }
        None
    }

    /// Adopt the client size the window manager gave this window.
    ///
    /// Re-maps the frame region at the new size at once, so the input that
    /// follows is resolved against the layout it was aimed at, and owes the
    /// window whole: the browser lays out to the new viewport and a Properties
    /// window re-places its bands.
    ///
    /// The reported size is adopted exactly — the declared minimum is the
    /// window manager's to hold, and an app that pushed back here would fight
    /// the drag frame by frame. The fresh surface is allocated before the
    /// session is asked and adopted only once the pane has taken the new
    /// region, so a refusal at either step leaves the window drawable at the
    /// size it already had (fail closed). The surface holds none of the last
    /// frame's pixels, so the repaint that follows can only be a whole one.
    fn resize_window(
        win: &mut OpenWindow,
        client: &mut WindowClient<app::RtWindowTransport>,
        width_px: u32,
        height_px: u32,
    ) {
        let new_mode = app::mode_for(width_px, height_px);
        if win.pane.resize_with(client, &new_mode, &mut win.surface) == app::Fitted::Adopted {
            win.owed.owe_whole();
        }
    }

    /// Route one event to the Properties window at `index`.
    ///
    /// Its own path because a Properties window shares nothing with a listing
    /// but the pane, the surface and the title: no rail, no chrome, no
    /// overlays, and no listing to navigate.
    fn route_properties_event(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        index: usize,
        client: &mut WindowClient<app::RtWindowTransport>,
        theme: &Theme,
        reads: &Reads,
        scale: Scale,
        event: &WindowEvent,
    ) {
        let Some(win) = windows.get_mut(index) else {
            return;
        };
        let window_id = win.pane.id();
        let mode = *win.pane.mode();
        let mut damage = damage::sink();
        let WindowKind::Properties(props) = &mut win.kind else {
            return;
        };
        let (repaint, close) = apply_properties_event(
            props,
            window_id,
            reads,
            theme,
            scale,
            Rect::new(0, 0, mode.width_px, mode.height_px),
            event,
            &mut damage,
        );
        if close {
            close_window(windows, index, client, reads);
            return;
        }
        owe(win, repaint, &damage);
    }

    /// Apply the chrome toggle `event` names to `win`, answering whether a
    /// band moved.
    ///
    /// `F9` shows or hides the places rail and `Ctrl+F9` the command toolbar,
    /// whichever the settings opened the window with, so every command the
    /// toolbar carries — the view toggle, the sort cycle, the Trash tools —
    /// stays reachable while it is hidden.
    fn chrome_toggle(win: &mut OpenWindow, event: &WindowEvent) -> bool {
        let WindowEvent::Key {
            key: KeyInput::Pressed { key, modifiers },
            ..
        } = event
        else {
            return false;
        };
        let Some(state) = win.browser() else {
            return false;
        };
        match state.chrome.toggled_by(*key, *modifiers) {
            Some(chrome) => {
                state.chrome = chrome;
                true
            }
            None => false,
        }
    }

    /// Show `location` (the user's own files when `None`): bring forward the
    /// window already showing it, else open one more, stating why when it
    /// cannot be opened.
    ///
    /// A request arrives from the user — the slot, its menu, a folder opened
    /// on the desktop — so the session handed this process the activation a
    /// raise needs. A refused raise still shows the folder, in a new window.
    ///
    /// The window cap is this process's own resource bound, not a policy about
    /// how many folders a user may look at, so reaching it is stated rather
    /// than silently ignored.
    fn open_more(
        windows: &mut [OpenWindow],
        opening: &Opening,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &alloc::sync::Arc<Reads>,
        (location, settings): (Option<alloc::vec::Vec<String>>, &Preferences),
    ) {
        let ladder = ladder_for(location, settings);
        if let Some(wanted) = ladder.place() {
            let at = Location::Folder(wanted.to_vec());
            if let Some(open) = windows.iter().find(|win| win.shows(&at)) {
                if raise_showing(client, open.pane.id()) {
                    return;
                }
            } else if show_in_its_folder(windows, client, wanted) {
                return;
            } else if opening
                .borrow()
                .iter()
                .any(|asked| lists(&asked.browser, &at))
            {
                // Asked for already: it opens once its listing lands.
                return;
            }
        }
        // No count of its own: a window's mapped frame region is bounded by
        // the session's per-client frame budget and by this process's own
        // address-space limit, both derived from the machine and both refusing
        // with a stated reason. A hand-picked ceiling in front of them would
        // only refuse windows the machine could have given. A ladder with no
        // listable place was stated as each was refused.
        if let Some(asked) = open_browser(reads, ladder, settings) {
            opening.borrow_mut().push(asked);
        }
    }

    /// Show the network: bring forward the window already showing it, else
    /// open one, which lands with the catalogue's first answer. A refused
    /// raise still shows it, in a new window.
    fn open_network(
        windows: &mut [OpenWindow],
        opening: &Opening,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &alloc::sync::Arc<Reads>,
        settings: &Preferences,
    ) {
        if let Some(open) = windows.iter().find(|win| win.shows(&Location::Network)) {
            if raise_showing(client, open.pane.id()) {
                return;
            }
        } else if opening
            .borrow()
            .iter()
            .any(|asked| lists(&asked.browser, &Location::Network))
        {
            return;
        }
        let listing = reads.listing_client();
        let mut browser =
            Browser::open_network(DeferredSource(alloc::sync::Arc::clone(reads), listing));
        browser.set_view_mode(settings.view);
        browser.set_sort_mode(settings.sort);
        if let Err(err) = browser.set_presentation(settings.presentation) {
            report_error(&alloc::format!("listing refused ({err})"));
        }
        opening.borrow_mut().push(PendingWindow {
            browser,
            listing,
            ladder: None,
            vital: false,
        });
    }

    /// The places a window asked for `location` — or for none — may open at,
    /// as `settings` say a window asked for none opens.
    fn ladder_for(location: Option<alloc::vec::Vec<String>>, settings: &Preferences) -> Ladder {
        let home = home_components();
        let start = settings.start.place(home.as_deref());
        Ladder::new(location, home.as_deref(), start)
    }

    /// Show the file `path` names selected in a window already listing the
    /// folder holding it — what listing `path` would come to — bringing that
    /// window forward. Answers whether one did; a refused raise is stated,
    /// and the file is selected all the same.
    fn show_in_its_folder(
        windows: &mut [OpenWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        path: &[String],
    ) -> bool {
        let Some((name, folder)) = path.split_last() else {
            return false;
        };
        let Some(win) = windows.iter_mut().find(|win| win.lists_file(folder, name)) else {
            return false;
        };
        reveal_entry(win, name);
        if let Err(err) = client.activate_window(win.pane.id()) {
            report_error(&alloc::format!(
                "the window showing that file could not be brought forward ({err:?})"
            ));
        }
        true
    }

    /// Bring forward the window `id`, which already shows a folder asked for,
    /// rather than open a second; a refused raise is stated and answered
    /// `false`, so the folder still opens.
    fn raise_showing(client: &mut WindowClient<app::RtWindowTransport>, id: u64) -> bool {
        match client.activate_window(id) {
            Ok(()) => true,
            Err(err) => {
                report_error(&alloc::format!(
                    "the window showing that folder could not be brought forward ({err:?}); \
                     opening another"
                ));
                false
            }
        }
    }

    /// The title of the notice a window that could not open where it was
    /// asked states the move in.
    const OPENED_ELSEWHERE: &str = "Opened elsewhere";

    /// Drain every folder the desktop has queued for this instance, opening a
    /// window at each.
    ///
    /// How a *relaunch* that names a folder reaches this already-running
    /// instance: the desktop asks the running process to open it rather than
    /// starting a second. The path goes through the very same
    /// [`location_components`](crate::command::location_components) rule the
    /// command line's own starting location does, so a refused spelling is
    /// stated and skipped rather than opening a window somewhere else.
    ///
    /// A *document* target is not one of these: the file manager holds
    /// `CAP_FS_ACCESS` and opens what it is given by name, so a delegated
    /// descriptor is authority it has no use for. It is stated and skipped
    /// rather than silently dropped.
    fn drain_open_targets(
        windows: &mut [OpenWindow],
        opening: &Opening,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &alloc::sync::Arc<Reads>,
        settings: &Preferences,
    ) {
        loop {
            let path = match client.take_open_target() {
                Ok(Some(Target::Path(path))) => path,
                Ok(Some(Target::Document { name, .. })) => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{name} was handed over as a document; this is a file manager, \
                         which opens what it is given by name"
                        ),
                    );
                    continue;
                }
                // A window on a folder is what this opens; it has no panes
                // for a launch to name.
                Ok(Some(Target::Pane(pane))) => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{pane} was handed over, but this is a file manager and has no \
                         places of that name"
                        ),
                    );
                    continue;
                }
                Ok(None) => return,
                Err(err) => {
                    app::report(APP_NAME, format_args!("cannot take an open target: {err}"));
                    return;
                }
            };
            match crate::command::location_components(&path) {
                Ok(location) => {
                    open_more(windows, opening, client, reads, (Some(location), settings));
                }
                // Stated and skipped: the *next* target may well be fine, so
                // one bad spelling must not abandon the drain.
                Err(reason) => {
                    app::report(APP_NAME, reason);
                }
            }
        }
    }

    /// The file manager's launched children: the application bundles it
    /// spawned when the user activated a `<Name>.app`, tracked by PID so each
    /// is reaped when it exits (never left a zombie) and a load refusal is
    /// named in the fail-loud diagnosis.
    ///
    /// Launching is asynchronous: [`spawn`](tairix_rt::spawn) admits the child
    /// and returns its PID before the bundle's image is loaded, so a load
    /// refusal (a missing, unverified, or malformed bundle) surfaces later as
    /// the child's reserved `LOAD_*` exit status, reported by
    /// [`reap`](Self::reap) under the bundle name. The launched app is the
    /// user's own — it runs under the launching user's identity and receives
    /// only the manager's manifest set intersected with that user's grants, so
    /// launching grants no ambient authority.
    struct Launcher {
        /// PID → bundle leaf name, for every launched child not yet reaped.
        /// Bounded by the children in flight; an entry is removed when its
        /// child is reaped, so it never grows beyond that.
        in_flight: BTreeMap<u64, String>,
        /// Documents opened before any bundle scan landed, waiting for one to
        /// say which application opens each. At most [`DOCUMENT_OPENS_MAX`].
        awaiting: Vec<String>,
        /// The reader a document is opened on.
        reads: alloc::sync::Arc<Reads>,
        /// The program-store scan every open resolves its application from.
        installed: alloc::rc::Rc<Installed>,
        /// The opens and launches it could not carry out, each stated in an
        /// alert holding no window: by the time one is known, the window that
        /// asked may have moved on or closed.
        notices: Notices,
    }

    impl Launcher {
        /// An idle launcher with no children in flight, opening documents on
        /// `reads` for the applications `installed` declares.
        fn new(reads: alloc::sync::Arc<Reads>, installed: alloc::rc::Rc<Installed>) -> Self {
            Self {
                in_flight: BTreeMap::new(),
                awaiting: Vec::new(),
                reads,
                installed,
                notices: Notices::default(),
            }
        }

        /// State that the item `name` could not be opened, because of
        /// `reason`.
        fn not_opened(&mut self, name: &str, reason: &str) {
            self.notices
                .refuse(NoticeSource::Open, &notice::not_opened(name), reason);
        }

        /// Launch the application bundle whose directory is `bundle_path` (the
        /// validated absolute path the activation named, e.g. `/System/Applications/Notes.app`)
        /// by spawning its own `Run` binary through the ordinary signed
        /// app-load gate — never a private path.
        ///
        /// The spawn is admitted immediately and the child loads on its own
        /// task, so the event loop never blocks behind a load. A synchronous
        /// refusal (a stripped spawn capability or a malformed path, decided
        /// before any child exists) is stated fail-loud on `stderr` at once; a
        /// load refusal that only shows once the image is read surfaces later
        /// through [`reap`](Self::reap). Either way the file manager carries on
        /// — a refused launch is an answer, not a crash.
        fn launch(&mut self, client: &mut WindowClient<app::RtWindowTransport>, bundle_path: &str) {
            let label = bundle_leaf(bundle_path);
            let run_path = tairix_appstore::entry_path(bundle_path);
            // The desktop's single-instance funnel first: a bundle that
            // declares one instance and already has one should be *asked*
            // rather than started again, and only the desktop knows what is
            // running. Anything but "reached" — no instance, an instance that
            // could not be reached, a session with no funnel — spawns, so a
            // launch never silently does nothing.
            if matches!(
                client.hand_over_launch(&run_path, None),
                Ok(HandOverOutcome::Reached)
            ) {
                return;
            }
            let ret = tairix_rt::spawn(run_path.as_bytes());
            if ret < 0 {
                let reason =
                    alloc::format!("it could not be started: {}", Errno::from_syscall(ret));
                self.not_opened(&label, &reason);
                return;
            }
            #[allow(clippy::cast_sign_loss)] // `ret >= 0` in this branch; it is a PID.
            self.in_flight.insert(ret as u64, label);
        }

        /// Reap every child that has exited, naming a load refusal in the
        /// fail-loud diagnosis.
        ///
        /// Non-blocking and drained fully: the loop ends the instant no zombie
        /// remains (the non-blocking `wait` yields nothing once none is left),
        /// so a burst of exits is handled in one wake and nothing spins. A
        /// clean exit, or an ordinary non-zero exit outside the reserved
        /// `LOAD_*` band, is the launched app's own business and is not
        /// reported as a launch failure.
        fn reap(&mut self) {
            loop {
                let mut status = WaitStatus::Exited(0);
                let pid = tairix_rt::wait(WAIT_PID_ANY, &mut status, WaitFlags::NONBLOCK);
                if pid <= 0 {
                    // No child ready (WouldBlock) or none left: stop draining.
                    return;
                }
                #[allow(clippy::cast_sign_loss)] // guarded by `pid > 0`.
                let label = self.in_flight.remove(&(pid as u64));
                if let WaitStatus::Exited(code) = status {
                    if let Some(reason) = load_failure_reason(code) {
                        let name = label.as_deref().unwrap_or("an application");
                        report_error(&alloc::format!("{name} failed to launch: {reason}"));
                    }
                }
            }
        }

        /// Open the data file at the validated absolute path `file_path` in its
        /// associated viewer — the `OpenFile` half of activation.
        ///
        /// The associated application is resolved from the installed bundles'
        /// declared file-type associations (the reader's scan +
        /// [`applications_for`], keyed off the file's leaf name), never a
        /// hard-coded viewer path. The first bundle that claims the file's type
        /// is launched with the file handed to it on `STDIN` (see
        /// [`launch_viewer`](Self::launch_viewer)). When no installed
        /// application claims the type the refusal is stated fail-loud on
        /// `stderr` and nothing is launched — an honest answer, never a
        /// fabricated open.
        ///
        /// With no scan landed yet the document waits for one rather than
        /// walking the stores on this loop; an empty scan is still an answer,
        /// so a store with nothing installed says so once it has been read.
        fn open_file(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            file_path: &str,
        ) {
            if self.installed.borrow().is_some() {
                self.resolve(client, file_path);
                return;
            }
            if self.awaiting.len() >= DOCUMENT_OPENS_MAX {
                self.not_opened(
                    path_leaf(file_path),
                    "too many documents are already waiting to open",
                );
                return;
            }
            self.awaiting.push(String::from(file_path));
            if let Some(found) = self.reads.want_bundles() {
                *self.installed.borrow_mut() = Some(found);
                self.scan_landed(client);
            }
        }

        /// A bundle scan landed: open every document that was waiting for one.
        fn scan_landed(&mut self, client: &mut WindowClient<app::RtWindowTransport>) {
            for file_path in core::mem::take(&mut self.awaiting) {
                self.resolve(client, &file_path);
            }
        }

        /// Open `file_path` in the first installed application that claims it.
        fn resolve(&mut self, client: &mut WindowClient<app::RtWindowTransport>, file_path: &str) {
            let name = path_leaf(file_path);
            let chosen =
                applications_for(name, self.installed.borrow().as_deref().unwrap_or_default())
                    .first()
                    .map(|assoc| (String::from(assoc.bundle_path()), assoc.writes_documents()));
            match chosen {
                Some((bundle_path, edits)) => {
                    self.launch_viewer(client, &bundle_path, edits, file_path, name);
                }
                None => self.not_opened(name, "no installed application opens it"),
            }
        }

        /// Launch the bundle at `bundle_path`, handing it the file at
        /// `file_path` on `STDIN` — the inherited-document hand-off (the TAIRiX
        /// spelling of `viewer < file`, `plans/NEW-FILEMANAGER.md` `FM6b`).
        ///
        /// The file manager opens the file in its own table — read-write when
        /// the bundle's signed manifest says it `edits` and the user may write
        /// it, read-only otherwise — and wires that descriptor onto the
        /// child's `STDIN` slot ([`FdWire::Handle`]); the kernel clones the
        /// open description into the child owner-checked, capturing *this*
        /// process's attested identity onto the backing as it crosses over,
        /// so the application reaches the document with no filesystem
        /// capability of its own and there is no post-spawn channel or
        /// ordering race. The document-role token says it was handed a
        /// document, and whether it may write it, and the leaf name titles its
        /// window. The manager's own descriptor is
        /// closed regardless of the spawn outcome — the child holds its own
        /// counted clone. Launching is asynchronous and the child is reaped on
        /// the any-child wake exactly as [`launch`](Self::launch)'s children
        /// are; a refusal is stated fail-loud, never a fabricated open.
        ///
        /// The document is opened on the reader, and handed over when it has
        /// been ([`opened`](Self::opened)).
        fn launch_viewer(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            bundle_path: &str,
            edits: bool,
            file_path: &str,
            display_name: &str,
        ) {
            let job = DocumentOpen {
                bundle_path: String::from(bundle_path),
                edits,
                file_path: String::from(file_path),
                name: String::from(display_name),
            };
            if let Some(answer) = self.reads.open(job) {
                self.opened(client, answer);
            }
        }

        /// Hand a document the reader opened to the application it was opened
        /// for.
        fn opened(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            answer: DocumentOpened,
        ) {
            let DocumentOpened { job, result } = answer;
            let display_name = job.name.as_str();
            let document::Opened { file, writable } = match result {
                Ok(opened) => opened,
                Err(err) => {
                    let reason = alloc::format!("it could not be read: {err}");
                    self.not_opened(display_name, &reason);
                    return;
                }
            };
            let run_path = tairix_appstore::entry_path(&job.bundle_path);
            let label = bundle_leaf(&job.bundle_path);
            // The desktop's single-instance funnel first, handing the live
            // instance the document itself: the grant is minted from *this*
            // descriptor to the session, which relays it on, so the viewer
            // reads it under this manager's authority and never the session's.
            // Anything but "reached" spawns below, which still shows it.
            if hand_over(client, &run_path, file.fd(), writable, display_name) {
                return;
            }
            let mut wires = [FdWire::Inherit; STD_STREAM_COUNT];
            wires[STDIN as usize] = FdWire::Handle(file.fd());
            let attach = SpawnAttach {
                wires,
                ..SpawnAttach::INHERIT
            };
            let pid = tairix_rt::spawn_attached(
                run_path.as_bytes(),
                &attach,
                &[
                    run_path.as_bytes(),
                    document::role_arg(writable),
                    display_name.as_bytes(),
                ],
                &[],
            );
            // The child holds its own counted clone of the open description,
            // so the manager's copy goes either way.
            drop(file);
            if pid < 0 {
                let reason =
                    alloc::format!("{label} could not be started: {}", Errno::from_syscall(pid));
                self.not_opened(display_name, &reason);
                return;
            }
            #[allow(clippy::cast_sign_loss)] // `pid >= 0` in this branch; it is a PID.
            self.in_flight.insert(pid as u64, label);
        }
    }

    /// Offer the document open on `fd`, read-write when `writable`, to a live
    /// instance of the bundle whose entry binary is `run_path`, answering
    /// whether one took it.
    ///
    /// The delegation is minted from this manager's own descriptor to the
    /// *session*, which redeems it and hands the same authority on to the
    /// instance — so the viewer reads the document under this manager's
    /// captured identity and the session lends none of its own, larger reach.
    /// Every refusal answers `false` and leaves the caller to spawn, which
    /// still shows the document.
    fn hand_over(
        client: &mut WindowClient<app::RtWindowTransport>,
        run_path: &str,
        fd: u32,
        writable: bool,
        display_name: &str,
    ) -> bool {
        let Some(session) = client.session() else {
            return false;
        };
        let Ok(name) = DocumentName::new(display_name) else {
            return false;
        };
        let grant = tairix_rt::fd_grant(fd, document::grant_ceiling(writable), session);
        let Some(grant) = u64::try_from(grant).ok().filter(|&handle| handle != 0) else {
            return false;
        };
        matches!(
            client.hand_over_launch(
                run_path,
                Some(HandOverDocument {
                    name,
                    grant,
                    writable
                })
            ),
            Ok(HandOverOutcome::Reached)
        )
    }

    /// The last non-empty component of a `/`-separated path
    /// (`/System/Applications/Notes.app` → `Notes.app`, `/Users/me/notes.txt` → `notes.txt`) —
    /// the leaf the user already sees, carrying no path prefix. An empty or
    /// `/`-only path (which the validated activation paths never produce) falls
    /// back to the whole string rather than an empty name.
    fn path_leaf(path: &str) -> &str {
        path.rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or(path)
    }

    /// The bundle directory's leaf name (`/System/Applications/Notes.app` → `Notes.app`) — the
    /// label the fail-loud launch diagnosis names.
    fn bundle_leaf(bundle_path: &str) -> String {
        String::from(path_leaf(bundle_path))
    }

    /// The store-reading seam behind the shared walk: directory listings and
    /// one bundle's manifest, both under this process's own attested identity.
    ///
    /// Listings resolve through the browser's own entry decode, so a bundle
    /// reached by a symbolic link is still found; the manifest read is bounded
    /// by the shared manifest ceiling, so an unexpectedly huge file at a
    /// bundle's `AppInfo` path is refused rather than read without limit.
    struct RtStoreReader;

    impl StoreReader for RtStoreReader {
        fn list_dir(&self, path: &str) -> Result<Option<alloc::vec::Vec<StoreDirEntry>>, Errno> {
            let Ok(stream) = tairix_rt::read_dir_all(path.as_bytes()) else {
                // An absent or unreadable store root contributes nothing; the
                // walk carries on with the roots it can see.
                return Ok(None);
            };
            let Ok(entries) =
                tairix_browse::vfs::entries_from_dir_stream(path, &stream, &mut RtLinkReader)
            else {
                return Ok(None);
            };
            Ok(Some(
                entries
                    .iter()
                    .map(|entry| StoreDirEntry {
                        name: String::from(entry.name()),
                        directory: entry.is_directory_backed(),
                    })
                    .collect(),
            ))
        }

        fn read_appinfo(&self, bundle: &str) -> Result<Option<alloc::vec::Vec<u8>>, Errno> {
            Ok(tairix_rt::read_path_to_end(
                tairix_appstore::manifest_path(bundle).as_bytes(),
                APPINFO_WIRE_MAX,
            )
            .ok())
        }
    }

    /// The grid's [`ArtworkReader`]: one shipped icon asset read through the
    /// app's own capability-checked filesystem access, under its own identity
    /// and with no authority beyond it.
    ///
    /// The read stops one byte past the shared artwork ceiling, so an asset
    /// larger than it comes back over-long — detected rather than truncated
    /// into a decodable-looking one — and the shared cache refuses it before
    /// any decode; a missing or unreadable asset simply reads as `None`.
    /// Either way the tile falls back to its built-in glyph, so a tile is
    /// never blank.
    struct VfsArtworkReader;

    impl ArtworkReader for VfsArtworkReader {
        fn read(&mut self, path: &str) -> Option<alloc::vec::Vec<u8>> {
            tairix_rt::read_path_to_end(path.as_bytes(), MAX_ARTWORK_BYTES).ok()
        }

        fn open(&mut self, path: &str) -> Option<Box<dyn tairix_icon::ArtworkDocument + '_>> {
            tairix_icon::RtDocument::open(path)
                .map(|document| Box::new(document) as Box<dyn tairix_icon::ArtworkDocument>)
        }
    }

    /// The grid's [`ArtworkRasteriser`]: the decode runs in a
    /// minimum-capability sandbox worker, never in this process.
    ///
    /// Icon artwork is a file on a volume — untrusted input — so its bytes go
    /// to the shared icon-rasterisation service running in a capability-empty
    /// child this binary re-enters itself as, and only validated pixels come
    /// back. A refusing, crashed, or replaced worker reports `None`, which the
    /// tile draws as its built-in glyph.
    struct SandboxRasteriser {
        /// The parser-sandbox seam: one worker, started on the first decode and
        /// replaced by the seam if it ever fails.
        sandbox: ParserSandbox<RtLauncher, tairix_rt::LogSink>,
        /// Where a thumbnail decoded once per version of its file is kept.
        store: StoreSlot,
    }

    impl SandboxRasteriser {
        fn new() -> Self {
            Self {
                sandbox: ParserSandbox::new(
                    RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Documents),
                    tairix_rt::LogSink,
                ),
                store: StoreSlot::Unopened,
            }
        }
    }

    /// The blob in this application's own bulk store its thumbnails persist in.
    const THUMBNAIL_BLOB: &str = "thumbnails";

    /// The persistent thumbnail store (`plans/FILES-INTERACTION.md` FI25),
    /// opened on the first thumbnail, at the side that asks for it.
    enum StoreSlot {
        Unopened,
        Open(ThumbnailStore<tairix_rt::File>),
        /// Refused, and said once: thumbnails are decoded and not kept.
        Unavailable,
    }

    impl StoreSlot {
        /// The store, when it holds pictures `side` pixels square.
        fn at(&mut self, side: u32) -> Option<&mut ThumbnailStore<tairix_rt::File>> {
            if matches!(self, Self::Unopened) {
                *self = match open_thumbnail_store(side) {
                    Ok(store) => Self::Open(store),
                    Err(reason) => {
                        report_error(&alloc::format!("thumbnails are not kept: {reason}"));
                        Self::Unavailable
                    }
                };
            }
            match self {
                Self::Open(store) if store.side() == side => Some(store),
                Self::Open(_) | Self::Unopened | Self::Unavailable => None,
            }
        }
    }

    /// Open the thumbnail blob. A second instance shares it unlocked — a
    /// delegated descriptor takes no advisory lock — which the store survives
    /// because every slot checks itself.
    fn open_thumbnail_store(side: u32) -> Result<ThumbnailStore<tairix_rt::File>, String> {
        let handle = tairix_appdata::blobs::open(
            &mut tairix_appdata::RtHost,
            THUMBNAIL_BLOB,
            tairix_abi::appdata_ipc::BlobMode::ReadWrite,
        )
        .map_err(|err| alloc::format!("the store was refused ({err})"))?;
        let file = tairix_rt::File::from_delegation(handle).map_err(|err| {
            alloc::format!(
                "the store's descriptor was refused ({})",
                Errno::from_syscall(err)
            )
        })?;
        ThumbnailStore::open(
            file,
            side,
            tairix_sandbox::imagerender::THUMBNAIL_REVISION,
            tairix_abi::appdata_ipc::APPDATA_BULK_FILE_MAX_BYTES,
        )
        .ok_or_else(|| String::from("the store could not be laid out"))
    }

    impl ArtworkRasteriser for SandboxRasteriser {
        fn rasterise(&mut self, side: u32, bytes: &[u8]) -> Option<alloc::vec::Vec<u8>> {
            rasterise_icon(
                &mut self.sandbox,
                side,
                bytes,
                &mut tairix_font::ServiceFonts::new(),
            )
            .ok()
        }

        fn thumbnail(
            &mut self,
            side: u32,
            reading: tairix_icon::Reading,
            document: &mut dyn tairix_icon::ArtworkDocument,
        ) -> Option<tairix_icon::Fitted> {
            let sandbox = &mut self.sandbox;
            let mut decode = |document: &mut dyn tairix_icon::ArtworkDocument| {
                tairix_sandbox::imagerender::thumbnail(sandbox, side, reading, document).ok()
            };
            match self.store.at(side) {
                Some(store) => store.serve(reading, document, decode),
                None => decode(document),
            }
        }
    }

    /// Everything the file manager reads off its event loop, and the one worker
    /// that reads it: each is a read of somebody's disk, which on the loop
    /// froze the window for as long as that disk took.
    ///
    /// One worker serves them in one stated order, [`Reads::next_read`]'s: a
    /// document opened, a listing navigated to, a shown folder's changes, a
    /// Properties read, icon artwork, folder cues, thumbnails, the bundle scan,
    /// the places rail. Nothing starves: each set is finite, refilled only by the user
    /// asking again or, for a folder's changes, by a watch the kernel paces to
    /// one report a window per latency.
    struct Reads {
        work: tairix_rt::sync::Mutex<Work>,
        /// Signalled when a read is recorded, and on teardown.
        signal: tairix_rt::sync::Condvar,
        /// Signalled when an operation is asked for, and on teardown. Apart
        /// from `signal`, so a wake meant for one worker is never taken by the
        /// other.
        operation_signal: tairix_rt::sync::Condvar,
        wake: tairix_rt::sync::WorkerWake,
        /// The wait-set the loop parks in, which each window's watch joins.
        set: u64,
        /// The network session and its catalogue, held by whichever thread
        /// carries out a network job. A lock of its own, so the calls to the
        /// discovery service hold no other desk up.
        network: tairix_rt::sync::Mutex<Option<NetworkSession>>,
        /// The port the discovery service rings the session on, or why none
        /// could be bound.
        doorbell: Result<u64, Errno>,
        /// The session open now, for the loop's drain of the doorbell to tell
        /// the service's rings from anything else posted there.
        open_session: OpenSession,
    }

    /// The session the network catalogue is browsed through, and the
    /// catalogue it keeps.
    struct NetworkSession {
        session: Session<RtTransport>,
        catalogue: Catalogue,
    }

    /// The desks the worker serves, in one lock: they are read by the same
    /// thread and written by the same worker, so one lock is one ordering
    /// rather than three that could interleave.
    struct Work {
        /// The documents asked to be opened, each answered in turn.
        opens: tairix_util::defer::JobQueue<DocumentOpen, DocumentOpened>,
        /// The renames committed and not yet answered, and the last ticket.
        writes: tairix_util::defer::JobQueue<WriteJob, WriteDone>,
        write_tickets: u64,
        listings: ListingDesk<FilesClient>,
        /// Each browser window's watch of the directory it shows.
        watches: Watches<FilesClient, alloc::sync::Arc<WatchedDirectory>>,
        /// Where each browser window's listing consumer comes from.
        listing_clients: FilesClients,
        /// What the paints have asked to be decoded and what has come back.
        /// Only the desk crosses this lock: the cache that keeps a picture
        /// lends it as a borrow, which could not outlive a guard.
        artwork: ArtworkDesk,
        probes: Probes,
        /// What the open Properties windows have asked to be described and
        /// what has come back, keyed by window.
        properties: PropertyReads,
        /// The readings the drive windows have asked for, keyed by window.
        drives: DriveReads,
        bundles: tairix_util::defer::JobDesk<(), Vec<AppAssociation>>,
        /// The user's home components and the mounted volumes the places rail
        /// is built from, re-read when the kernel says the mount table moved.
        places: tairix_util::defer::JobDesk<(), (Vec<String>, Vec<Volume>)>,
        /// The network catalogue a window shows or is on its way to.
        network: tairix_util::defer::JobDesk<NetworkAsk, NetworkAnswer>,
        /// Set once the app is tearing down, so a parked worker leaves instead
        /// of looking for work. Its own flag rather than one desk's, so the
        /// worker's exit test does not depend on which desk happens to carry
        /// one.
        stopping: bool,
        /// No reader was granted: the loop reads, and drains the windows'
        /// watches through `scratch`, itself.
        alone: bool,
        scratch: Vec<u8>,
        /// The long operations the operations worker carries out.
        operations: OperationDesk<Job>,
    }

    /// State that a window's folder no longer follows its changes, and why.
    fn unfollowed(err: Errno) {
        report_error(&alloc::format!(
            "this folder will not refresh by itself ({err})"
        ));
    }

    /// The most operations the app holds waiting behind the one running.
    ///
    /// A containment bound on what one process queues against its disks, not
    /// a capacity: each window holds at most one, and one past it is refused
    /// with its reason.
    const OPERATIONS_MAX: usize = 16;

    /// The most documents the app holds for opening at once, counting those
    /// opened but not yet handed over.
    ///
    /// A containment bound on what clicks can queue behind a slow or failing
    /// disk, not a capacity: one past it is refused with its reason.
    const DOCUMENT_OPENS_MAX: usize = 16;

    /// A document the user asked to open in the application chosen for it.
    struct DocumentOpen {
        bundle_path: String,
        edits: bool,
        file_path: String,
        name: String,
    }

    /// A [`DocumentOpen`] the reader carried out, and what it came to. The
    /// document closes when this is dropped.
    struct DocumentOpened {
        job: DocumentOpen,
        result: Result<document::Opened, Errno>,
    }

    /// A one-shot call the reader makes for a window, answered by ticket: the
    /// window keeps its live state until the answer lands.
    struct WriteJob {
        ticket: u64,
        act: WriteAct,
    }

    /// What a [`WriteJob`] asks of the volume, under the user's own identity.
    enum WriteAct {
        /// Move `from` to `to`. The name was checked against a listing the
        /// volume may have moved on from, so the kernel refuses rather than
        /// replaces whatever holds `to` now.
        Rename { from: String, to: String },
        /// Make the folder at the path.
        MakeFolder(String),
        /// Make the empty file at the path, exclusively.
        MakeFile(String),
        /// Make a Properties window's change to the node at `path`, while it
        /// is the node `described`.
        Change {
            path: String,
            described: FileId,
            change: Change,
        },
        /// Make sure the Trash folder exists.
        EnsureTrash(Vec<String>),
        /// Decide whether removing the plan's targets can be a recoverable
        /// move to Trash.
        PlanTrash(DeletePlan),
        /// Read what the Trash folder holds now and plan its removal.
        PlanEmptyTrash(Vec<String>),
    }

    /// A [`WriteJob`] carried out, and what the volume said.
    struct WriteDone {
        ticket: u64,
        answer: WriteAnswer,
    }

    /// What a [`WriteAct`] came to.
    enum WriteAnswer {
        /// The call's own answer.
        Done(Result<(), Errno>),
        /// A delete plan, and the Trash moves it can be carried out as.
        Planned(DeletePlan, Option<TrashMoves>),
        /// The removal of what the Trash holds, `None` when it holds nothing,
        /// or why it cannot be emptied.
        TrashPlanned(Result<Option<DeletePlan>, &'static str>),
    }

    impl WriteAnswer {
        /// The call's own answer. A plan carries its own: what it found, or
        /// why it found nothing.
        fn status(&self) -> Result<(), Errno> {
            match self {
                Self::Done(result) => *result,
                Self::Planned(..) | Self::TrashPlanned(_) => Ok(()),
            }
        }
    }

    /// The source→destination renames a move to Trash is carried out as.
    type TrashMoves = alloc::vec::Vec<(Listed, alloc::vec::Vec<String>)>;

    /// The most one-shot writes the app holds in flight at once: a containment
    /// bound on what gestures can queue behind a slow or failing disk, not a
    /// capacity; one past it is refused with its reason.
    const WRITES_MAX: usize = 16;

    /// What handing a write on came to: carried out at once, with no reader
    /// to take it, awaiting the reader under its ticket, or not handed on at
    /// all because as many writes as the app lets wait already do.
    enum WriteSubmit {
        Done(WriteAnswer),
        Pending(u64),
        Unsent,
    }

    /// Why a write was never asked of the volume.
    const WRITES_WAITING: &str = "too many changes are already waiting; try again once they finish";

    /// Why a window's change waits for the one it is already making.
    const STILL_WRITING: &str =
        "the window is still making a change; try again once it has finished";

    /// Why a Properties change is refused before the window has read the node.
    const NOT_READ: &str = "the window has not read it yet";

    /// Carry `act` out on this thread.
    fn write_now(act: WriteAct) -> WriteAnswer {
        let status = |ret: i64| match ret {
            0 => Ok(()),
            ret => Err(Errno::from_syscall(ret)),
        };
        WriteAnswer::Done(match act {
            WriteAct::Rename { from, to } => status(tairix_rt::fs_rename(
                from.as_bytes(),
                to.as_bytes(),
                RenameFlags::NO_REPLACE,
            )),
            WriteAct::MakeFolder(path) => {
                status(tairix_rt::fs_mkdir(path.as_bytes(), FS_NEW_DIR_MODE))
            }
            WriteAct::MakeFile(path) => tairix_rt::create_new(path.as_bytes(), FS_NEW_FILE_MODE)
                .map(drop)
                .map_err(Errno::from_syscall),
            WriteAct::Change {
                path,
                described,
                change,
            } => crate::change::make(&WorkingDirectory, (&path, described), &change),
            WriteAct::EnsureTrash(trash) => ensure_trash_dir(&trash),
            WriteAct::PlanTrash(plan) => {
                let moves = plan_trash_moves(&plan);
                return WriteAnswer::Planned(plan, moves);
            }
            WriteAct::PlanEmptyTrash(trash) => {
                let planned = match listed_children(&trash) {
                    Ok(children) => {
                        empty_trash_plan(&trash, &children).map_err(TrashError::message)
                    }
                    Err(err) => Err(trash_read_refusal(err)),
                };
                return WriteAnswer::TrashPlanned(planned);
            }
        })
    }

    /// One unit of work the reader took.
    enum Read {
        /// Open a document the user is waiting to see.
        Open(DocumentOpen),
        /// Carry out a rename the user committed.
        Write(WriteJob),
        /// List this directory for the browser.
        List(ListingJob<FilesClient>),
        /// Drain a window's watch of the directory it shows.
        Drain(FilesClient, Vec<String>, alloc::sync::Arc<WatchedDirectory>),
        /// Read and decode one tile's icon artwork.
        Artwork(ArtworkJob),
        /// Probe these folders' occupancy as one batch.
        Probe(Vec<Vec<String>>),
        /// Describe one node for the Properties window named.
        Properties(u64, PropertyJob),
        /// Read the drive the window named describes.
        Drive(u64, DriveJob),
        /// Walk the program stores for their declared file associations.
        Bundles,
        /// Re-read the home components and the mounted volumes.
        Places,
        /// Collect from, or close, the network session.
        Network(NetworkAsk),
    }

    impl Reads {
        /// A desk over `wake`, with no worker yet, whose windows' watches
        /// report in `set` and whose network session rings `doorbell`.
        fn new(wake: tairix_rt::sync::WorkerWake, set: u64, doorbell: Result<u64, Errno>) -> Self {
            Self {
                work: tairix_rt::sync::Mutex::new(Work {
                    // A queue refused its room refuses every open, stating why.
                    opens: tairix_util::defer::JobQueue::with_capacity(DOCUMENT_OPENS_MAX)
                        .unwrap_or_default(),
                    writes: tairix_util::defer::JobQueue::with_capacity(WRITES_MAX)
                        .unwrap_or_default(),
                    write_tickets: 0,
                    listings: ListingDesk::new(),
                    watches: Watches::new(),
                    listing_clients: FilesClients::default(),
                    artwork: ArtworkDesk::new(),
                    probes: Probes::new(),
                    properties: PropertyReads::new(),
                    drives: DriveReads::new(),
                    bundles: tairix_util::defer::JobDesk::new(),
                    places: tairix_util::defer::JobDesk::new(),
                    network: tairix_util::defer::JobDesk::new(),
                    stopping: false,
                    alone: false,
                    scratch: Vec::new(),
                    operations: OperationDesk::new(OPERATIONS_MAX),
                }),
                signal: tairix_rt::sync::Condvar::new(),
                operation_signal: tairix_rt::sync::Condvar::new(),
                wake,
                set,
                network: tairix_rt::sync::Mutex::new(None),
                doorbell,
                open_session: OpenSession::default(),
            }
        }

        /// One worker's whole life: park until something is wanted, read it,
        /// deliver it, wake the loop.
        ///
        /// The decoder seams are built here and reused for every later decode,
        /// so no sandbox handle ever crosses a thread boundary.
        fn serve(&self) {
            let mut reader = VfsArtworkReader;
            let mut rasteriser = SandboxRasteriser::new();
            // Refused, each drain reads its changes a few at a time instead.
            let mut scratch =
                tairix_util::fallible::filled(WATCH_BUFFER_LEN, 0).unwrap_or_default();
            loop {
                let job = {
                    let mut work = self.work.lock();
                    loop {
                        if work.stopping {
                            return;
                        }
                        if let Some(job) = Self::next_read(&mut work) {
                            break job;
                        }
                        work = self.signal.wait(work);
                    }
                };
                // The reads themselves, with no lock held: these are the calls
                // that used to stall the window.
                let owed = match job {
                    Read::Open(job) => {
                        let result = document::open_for(job.file_path.as_bytes(), job.edits);
                        self.work
                            .lock()
                            .opens
                            .deliver(DocumentOpened { job, result })
                    }
                    Read::Write(job) => {
                        let answer = write_now(job.act);
                        self.work.lock().writes.deliver(WriteDone {
                            ticket: job.ticket,
                            answer,
                        })
                    }
                    Read::List(job) => {
                        let (client, target) = (job.client(), job.target().to_vec());
                        // A reload of the folder already watched reads through
                        // that watch, so its pacing carries on.
                        let reuse = self.work.lock().watches.relisting(client, &target);
                        let (listed, armed) = WatchedDirectory::read(&target, reuse, unfollowed);
                        let (owed, unwanted) = {
                            let mut work = self.work.lock();
                            let owed = work.listings.deliver(job, listed);
                            (owed, work.watches.offer_armed(owed, client, &target, armed))
                        };
                        drop(unwanted);
                        owed
                    }
                    Read::Drain(client, location, dir) => {
                        let update = dir.drain(&mut scratch);
                        self.work.lock().watches.deliver(client, &location, update)
                    }
                    Read::Artwork(job) => {
                        // The shared decode, so deferring it cannot change
                        // what a tile draws.
                        let artwork =
                            render_artwork(&mut reader, &mut rasteriser, &job.key, job.side);
                        self.work.lock().artwork.deliver(&job, artwork).wake()
                    }
                    Read::Probe(batch) => {
                        let answers = tairix_browse::vfs::probe_batch(
                            batch,
                            tairix_browse::vfs::probe_directory,
                        );
                        self.work.lock().probes.deliver(answers)
                    }
                    Read::Properties(window, job) => {
                        let described = describe_node(&job);
                        self.work.lock().properties.deliver(window, described)
                    }
                    Read::Drive(window, job) => {
                        let read = read_drive(&job);
                        self.work.lock().drives.deliver(window, read)
                    }
                    Read::Bundles => {
                        let found = scan_bundles();
                        self.work.lock().bundles.deliver(found)
                    }
                    Read::Places => {
                        let found = places_source();
                        self.work.lock().places.deliver(found)
                    }
                    Read::Network(ask) => {
                        let answer = self.browse_network(ask);
                        self.work.lock().network.deliver(answer)
                    }
                };
                if owed {
                    self.wake.nudge();
                }
            }
        }

        /// The next unit of work, in the stated order.
        fn next_read(work: &mut Work) -> Option<Read> {
            // A document the user opened is what they are waiting for.
            if let Some(job) = work.opens.next_job() {
                return Some(Read::Open(job));
            }
            // So is a write: its window waits on the answer.
            if let Some(job) = work.writes.next_job() {
                return Some(Read::Write(job));
            }
            if let Some(job) = work.listings.next_job() {
                return Some(Read::List(job));
            }
            // A window showing the network, or on its way there, waits on
            // the catalogue as a folder's on its listing.
            if let Some(ask) = work.network.next_job() {
                return Some(Read::Network(ask));
            }
            // A folder on screen that changed is next: what the user sees is
            // wrong until it lands.
            if let Some((client, location, dir)) = work.watches.next_drain() {
                return Some(Read::Drain(client, location, dir));
            }
            // A node the user asked to be described comes before the icon
            // decodes and folder cues: those are decoration a frame already
            // draws without, and a Properties window shows nothing until its
            // answer lands.
            if let Some((window, job)) = work.properties.next_job() {
                return Some(Read::Properties(window, job));
            }
            // A drive window's reading is watched live, so it goes before the
            // decoration below, which a frame already draws without.
            if let Some((window, job)) = work.drives.next_job() {
                return Some(Read::Drive(window, job));
            }
            if let Some(job) = work.artwork.next_job() {
                return Some(Read::Artwork(job));
            }
            if let Some(batch) = work.probes.next_batch() {
                return Some(Read::Probe(batch));
            }
            // A picture file's own content last of the decoration: its class
            // picture already stands in, and it costs a whole file's read.
            if let Some(job) = work.artwork.next_thumbnail() {
                return Some(Read::Artwork(job));
            }
            if work.bundles.next_job().is_some() {
                return Some(Read::Bundles);
            }
            work.places.next_job().map(|()| Read::Places)
        }

        /// Answer the browser's request for `components`, recording it if this
        /// desk does not already hold the answer.
        ///
        /// With no worker to answer it the directory is read on the calling
        /// thread instead, which is exactly what this app did before it had
        /// one: a recorded request nobody will serve would leave the window
        /// listing for ever, so the degradation is a real read, not a wait.
        ///
        /// A listing taken commits its window to the watch armed with it,
        /// whichever path took it, so a navigation answered on the spot is as
        /// live as one that waited.
        fn list(&self, client: FilesClient, components: &[String]) -> Result<Listing, Errno> {
            let mut took = Took::default();
            let listing = self.ask(client, components, |work| {
                let listing = work.listings.take(client, components);
                if matches!(listing, Ok(Listing::Ready(_))) {
                    took = work.watches.took(client, components);
                }
                listing
            });
            self.commit(client, took);
            listing
        }

        /// Join the watch `client` now reports on to the loop's wait-set,
        /// letting go of what it moved away from; a folder that cannot be
        /// followed says so and is unwatched.
        fn commit(&self, client: FilesClient, took: Took<alloc::sync::Arc<WatchedDirectory>>) {
            let token = WATCH_TOKEN_BASE.saturating_add(client.number());
            if let Err(err) = took.commit(self.set, token) {
                drop(self.unwatch(client));
                unfollowed(err);
            }
        }

        /// Record a fresh listing of `components` — one no read already under
        /// way may answer — degrading exactly as [`list`](Self::list) does.
        fn refresh(&self, client: FilesClient, components: &[String]) -> Result<Listing, Errno> {
            self.ask(client, components, |work| {
                work.listings.refresh(client, components);
                // The folder may just have changed, so what was probed of its
                // folders may have too.
                work.probes.invalidate_listing(components);
                Ok(Listing::Pending)
            })
        }

        /// The consumer a new browser window lists under.
        fn listing_client(&self) -> FilesClient {
            self.work.lock().listing_clients.mint()
        }

        /// Let a closed window's listing consumer go, with whatever it had
        /// asked for and the watch it held.
        fn forget_listing(&self, client: FilesClient) {
            let released = {
                let mut work = self.work.lock();
                work.listings.forget(client);
                work.watches.forget(client)
            };
            drop(released);
        }

        /// Whether a window's listing of `components` follows that folder.
        fn follows(&self, client: FilesClient, components: &[String]) -> bool {
            self.work.lock().watches.follows(client, components)
        }

        /// A window's watch reported a change: drain it off the loop, or here
        /// with no reader to. Whether it was drained here and left something
        /// for the loop to adopt now.
        fn want_drain(&self, client: FilesClient) -> bool {
            let mut work = self.work.lock();
            if !work.watches.want_drain(client) {
                return false;
            }
            if work.alone {
                let Work {
                    watches, scratch, ..
                } = &mut *work;
                return watches.drain_here(|dir| dir.drain(scratch));
            }
            drop(work);
            self.signal.notify_one();
            false
        }

        /// What a window's drains produced, handed over once.
        fn take_watch_update(&self, client: FilesClient) -> WatchUpdate {
            self.work.lock().watches.take_update(client)
        }

        /// The window stopped watching: the watch it held, to release.
        fn unwatch(&self, client: FilesClient) -> Option<alloc::sync::Arc<WatchedDirectory>> {
            self.work.lock().watches.unwatch(client)
        }

        /// The folders among `changes` in `dir` changed, so an answer about one
        /// is stale. Whether any was a folder, which owes the cues a resolve.
        fn invalidate_probes(
            &self,
            dir: &[String],
            changes: &[tairix_browse::EntryChange],
        ) -> bool {
            self.work.lock().probes.invalidate(dir, changes)
        }

        /// Put `client`'s listing request to the desk through `record`, waking
        /// the worker when it leaves the browser waiting; with no worker to,
        /// read it on the loop.
        fn ask(
            &self,
            client: FilesClient,
            components: &[String],
            record: impl FnOnce(&mut Work) -> Result<Listing, Errno>,
        ) -> Result<Listing, Errno> {
            let deferred = {
                let mut work = self.work.lock();
                if work.stopping {
                    None
                } else {
                    Some(record(&mut work))
                }
            };
            let Some(listing) = deferred else {
                return self.read_here(client, components);
            };
            if matches!(listing, Ok(Listing::Pending)) {
                self.signal.notify_one();
            }
            listing
        }

        /// Read `components` for `client` on the loop: armed and joined as the
        /// reader's read would be when the app has no reader, so the window
        /// still follows its folder, and read bare once it is tearing down.
        fn read_here(&self, client: FilesClient, components: &[String]) -> Result<Listing, Errno> {
            let read = {
                let mut work = self.work.lock();
                let Work { watches, alone, .. } = &mut *work;
                alone.then(|| {
                    watches.read_here(client, components, |reuse| {
                        WatchedDirectory::read(components, reuse, unfollowed)
                    })
                })
            };
            let Some((listed, took)) = read else {
                return read_directory(components).map(Listing::Ready);
            };
            self.commit(client, took);
            listed.map(Listing::Ready)
        }

        /// Answer a paint's miss on `key` at `side`, recording the decode if
        /// this desk has neither run it nor been asked for it already.
        ///
        /// Called from *inside a paint*, so it must never read: an undecoded
        /// icon is `Pending`, the tile draws its built-in glyph, and the
        /// pixels are drawn a frame later.
        fn resolve_artwork(&self, key: &ArtworkKey, side: u32) -> Resolved {
            let (answer, wanted) = {
                let mut work = self.work.lock();
                let answer = work.artwork.collect(key, side);
                (answer, work.artwork.has_work())
            };
            if wanted {
                self.signal.notify_one();
            }
            answer
        }

        /// Record `key` at `side` as wanted, without collecting an answer:
        /// what a draw site about to show an icon asks for, so the decode
        /// finishes before the frame that needs it.
        fn want_artwork(&self, key: &ArtworkKey, side: u32) {
            let wanted = {
                let mut work = self.work.lock();
                work.artwork.want(key, side);
                work.artwork.has_work()
            };
            if wanted {
                self.signal.notify_one();
            }
        }

        /// Note that the cache could not keep this decode, so nothing offers
        /// it again until the band that refused it moves.
        fn decline_artwork(&self, key: &ArtworkKey, side: u32) {
            self.work.lock().artwork.decline(key, side);
        }

        /// The band moved: offer the refused decodes again, answering whether
        /// there was one, for the windows drawn without it to ask again.
        fn retry_declined_artwork(&self) -> bool {
            self.work.lock().artwork.retry_declined()
        }

        /// Note that a window still shows what `key` at `side` is for, so the
        /// next sweep keeps it, starting nothing.
        fn keep_artwork(&self, key: &ArtworkKey, side: u32) {
            self.work.lock().artwork.keep(key, side);
        }

        /// Which decodes have landed since this was last asked: each window
        /// repaints the pictures drawn through them and no others.
        fn take_artwork_landed(&self) -> Landed {
            self.work.lock().artwork.take_landed()
        }

        /// Withdraw the thumbnails no window asked about in the pass just
        /// drawn.
        fn sweep_thumbnails(&self) {
            self.work.lock().artwork.sweep_thumbnails();
        }

        /// Answer the folder cue for `components`, recording the probe if this
        /// desk does not already hold the answer.
        ///
        /// This is called from *inside a paint*, so it must never read: an
        /// unknown cue is [`Probe::Pending`], the folder draws without it, and
        /// the answer is drawn a frame later. A desk with no worker answers
        /// that it does not probe, so every folder draws plain: a cue is
        /// decoration, not worth a directory read on the loop.
        fn probe(&self, components: &[String]) -> Result<Probe, Errno> {
            let (answer, recorded) = self.work.lock().probes.ask(components);
            // Only a folder this desk had not seen is worth a wake. Waking on
            // every ask would mean one `futex_wake` per folder per frame, for
            // work the worker has already been told about.
            if recorded {
                self.signal.notify_one();
            }
            answer
        }

        /// Ask for `job`'s node to be described for `window`, waking a worker
        /// for it, or describing it here when no reader will.
        ///
        /// The window shows that it is reading until the answer lands: a node
        /// is one `fs_stat` plus one call per attribute key, which on a
        /// contended volume is not a frame.
        fn want_properties(&self, window: u64, job: PropertyJob) {
            let mut work = self.work.lock();
            if work.alone {
                drop(work);
                let described = describe_node(&job);
                self.work.lock().properties.answer_here(window, described);
                return;
            }
            if work.properties.submit(window, job) {
                drop(work);
                self.signal.notify_one();
            }
        }

        /// Ask for `window`'s drive to be read, waking a worker for it, or
        /// reading it here when no reader will.
        ///
        /// A reading is two pages of the System Information service, a round
        /// trip the loop does not wait on.
        fn want_drive(&self, window: u64, job: DriveJob) {
            let mut work = self.work.lock();
            if work.alone {
                drop(work);
                let read = read_drive(&job);
                self.work.lock().drives.answer_here(window, read);
                return;
            }
            if work.drives.submit(window, job) {
                drop(work);
                self.signal.notify_one();
            }
        }

        /// Take `window`'s landed drive reading, if one has.
        fn take_drive(&self, window: u64) -> Option<DriveReading> {
            self.work.lock().drives.take(window)
        }

        /// Whether a drive reading has landed since this was last asked.
        fn take_drives_landed(&self) -> bool {
            self.work.lock().drives.take_landed()
        }

        /// Forget a closed window's drive reading.
        fn forget_drive(&self, window: u64) {
            self.work.lock().drives.forget(window);
        }

        /// Take `window`'s landed description, if one has.
        fn take_properties(&self, window: u64) -> Option<Result<Properties, Errno>> {
            self.work.lock().properties.take(window)
        }

        /// Whether a description has landed since this was last asked.
        fn take_properties_landed(&self) -> bool {
            self.work.lock().properties.take_landed()
        }

        /// Forget a closed window's read, so its answer is not held for a
        /// window that no longer exists.
        fn forget_properties(&self, window: u64) {
            self.work.lock().properties.forget(window);
        }

        /// Whether a probe batch has landed since this was last asked.
        ///
        /// The loop resolves the visible cues on a `true`, which is what turns
        /// a worker's answers into drawn icons: a delivery nothing adopts
        /// leaves every folder showing the cue it had before the probe.
        fn take_probes_landed(&self) -> bool {
            self.work.lock().probes.take_landed()
        }

        /// Drop the probes no window asked for in the pass just resolved over
        /// every window, waking a worker if a batch is left for it.
        fn sweep_probes(&self) {
            if self.work.lock().probes.sweep() {
                self.signal.notify_one();
            }
        }

        /// Ask for the program stores to be walked, answering with what they
        /// declare when there is no worker to walk them elsewhere.
        fn want_bundles(&self) -> Option<Vec<AppAssociation>> {
            let submitted = {
                let mut work = self.work.lock();
                if work.stopping {
                    drop(work);
                    return Some(scan_bundles());
                }
                work.bundles.submit(())
            };
            if submitted.wake {
                self.signal.notify_one();
            }
            None
        }

        /// Take a landed bundle scan, if one has.
        fn take_bundles(&self) -> Option<Vec<AppAssociation>> {
            self.work.lock().bundles.collect()
        }

        /// Ask for `job`'s document to be opened, answering with what that came
        /// to when it was not left for the worker: opened here for want of one,
        /// or refused because too many are waiting.
        fn open(&self, job: DocumentOpen) -> Option<DocumentOpened> {
            let refused = {
                let mut work = self.work.lock();
                if work.stopping {
                    drop(work);
                    let result = document::open_for(job.file_path.as_bytes(), job.edits);
                    return Some(DocumentOpened { job, result });
                }
                work.opens.submit(job).err()
            };
            if let Some(job) = refused {
                return Some(DocumentOpened {
                    job,
                    result: Err(Errno::LimitExceeded),
                });
            }
            self.signal.notify_one();
            None
        }

        /// Take the oldest document the reader opened, if one has.
        fn take_opened(&self) -> Option<DocumentOpened> {
            self.work.lock().opens.collect()
        }

        /// Hand `act` to the reader, or carry it out here when there is no
        /// reader to take it: a write is a filesystem call the loop must not
        /// wait on, but a recorded one nobody will answer would leave its
        /// window waiting for ever.
        fn write(&self, act: WriteAct) -> WriteSubmit {
            let mut work = self.work.lock();
            if work.stopping || work.alone {
                drop(work);
                return WriteSubmit::Done(write_now(act));
            }
            work.write_tickets = work.write_tickets.wrapping_add(1);
            let ticket = work.write_tickets;
            if work.writes.submit(WriteJob { ticket, act }).is_err() {
                return WriteSubmit::Unsent;
            }
            drop(work);
            self.signal.notify_one();
            WriteSubmit::Pending(ticket)
        }

        /// Take the oldest write the reader carried out, if one has.
        fn take_written(&self) -> Option<WriteDone> {
            self.work.lock().writes.collect()
        }

        /// Ask for the places to be re-read, answering with them directly when
        /// there is no worker to read them elsewhere.
        ///
        /// The mount table is read through the System Information service, so
        /// a synchronous read here would be an IPC round trip on the loop that
        /// owes the user a frame.
        fn want_places(&self) -> Option<(Vec<String>, Vec<Volume>)> {
            let submitted = {
                let mut work = self.work.lock();
                if work.stopping {
                    drop(work);
                    return Some(places_source());
                }
                work.places.submit(())
            };
            if submitted.wake {
                self.signal.notify_one();
            }
            None
        }

        /// Take a landed places read, if one has.
        fn take_places(&self) -> Option<(Vec<String>, Vec<Volume>)> {
            self.work.lock().places.collect()
        }

        /// Ask for `ask` of the network session, answering it directly when
        /// there is no worker to carry it out elsewhere: each is a call to the
        /// discovery service, which the loop that owes a frame must not wait
        /// on.
        fn want_network(&self, ask: NetworkAsk) -> Option<NetworkAnswer> {
            let submitted = {
                let mut work = self.work.lock();
                if work.stopping {
                    drop(work);
                    return Some(self.browse_network(ask));
                }
                work.network.submit(ask)
            };
            if submitted.wake {
                self.signal.notify_one();
            }
            None
        }

        /// Take a landed answer from the network session, if one has.
        fn take_network(&self) -> Option<NetworkAnswer> {
            self.work.lock().network.collect()
        }

        /// Carry `ask` out on the network session.
        fn browse_network(&self, ask: NetworkAsk) -> NetworkAnswer {
            let mut held = self.network.lock();
            match ask {
                NetworkAsk::Close => {
                    if let Some(open) = held.take() {
                        self.open_session.close();
                        // The service ends a session whose owner has gone, so
                        // a refused close leaves nothing here to act on.
                        let _ = open.session.close();
                    }
                    NetworkAnswer::Closed
                }
                NetworkAsk::Collect => NetworkAnswer::Read(self.collect_network(&mut held)),
            }
        }

        /// Take what the service holds for the session — opening one if none
        /// is held, and once more if the one held was lost — and bring the
        /// catalogue's questions in line with it.
        fn collect_network(
            &self,
            held: &mut Option<NetworkSession>,
        ) -> Result<NetworkReading, DiscoveryError> {
            let doorbell = self.doorbell.map_err(DiscoveryError::Transport)?;
            let lost = match held.as_mut() {
                Some(open) => drain_session(open).is_err(),
                None => true,
            };
            if lost {
                *held = None;
                self.open_session.close();
                let session = Session::open(RtTransport, doorbell)?;
                self.open_session.open(session.id());
                let mut opened = NetworkSession {
                    session,
                    catalogue: Catalogue::new(netbrowse::PACE_NS),
                };
                drain_session(&mut opened)?;
                *held = Some(opened);
            }
            let open = held.as_mut().ok_or(DiscoveryError::Unavailable)?;
            open.catalogue
                .reconcile(&mut open.session, tairix_rt::clock_get());
            let walk = open.catalogue.walk();
            if let Some(refused) = walk.refused() {
                return Err(*refused);
            }
            Ok(NetworkReading {
                unsettled: walk.stalled().is_some() || walk.deferred(),
                items: tairix_browse::network::items(open.catalogue.listing()),
            })
        }

        /// Drain the doorbell, answering whether the service rang the session
        /// open now: anything else posted there is dropped unread.
        fn drain_doorbell(&self) -> bool {
            let Ok(doorbell) = self.doorbell else {
                return false;
            };
            let mut frame = [0u8; DOORBELL_LEN];
            let mut sender = [0u8; ORIGIN_WIRE_LEN];
            let mut rung = false;
            while let Ok(len) = tairix_rt::ipc_recv(doorbell, &mut frame, &mut sender) {
                rung |= self.open_session.rung_by(&sender, &frame[..len]);
            }
            rung
        }

        /// Ask both workers to leave and wake them.
        fn stop(&self) {
            let mut work = self.work.lock();
            work.halt();
            work.watches.stop();
            work.operations.stop();
            work.alone = false;
            drop(work);
            self.signal.notify_all();
            self.operation_signal.notify_all();
        }

        /// No operations worker will answer: each operation is carried out
        /// where it is asked.
        fn operations_alone(&self) {
            self.work.lock().operations.stop();
        }

        /// Cancel operation `id`: the one its progress window shows.
        fn cancel_operation(&self, id: u64) {
            self.work.lock().operations.cancel(id);
        }

        /// Operation `id`'s progress window has closed: the operation stops
        /// and its answer is dropped.
        fn forget_operation(&self, id: u64) {
            self.work.lock().operations.forget(id);
        }

        /// The operations worker's whole life: park until an operation is
        /// asked for, carry it out a unit at a time, and answer it by its own
        /// id. A cancel is seen between units, never mid-unit.
        fn serve_operations(&self) {
            loop {
                let (_, mut job) = {
                    let mut work = self.work.lock();
                    loop {
                        if work.operations.stopping() {
                            return;
                        }
                        if let Some(next) = work.operations.next_job() {
                            break next;
                        }
                        work = self.operation_signal.wait(work);
                    }
                };
                let ending = loop {
                    let step = job.step();
                    let (owed, cancelled) = {
                        let mut work = self.work.lock();
                        let released = job.sync_hold(&mut work.artwork);
                        (
                            work.operations.report(job.reading(), &job.current) | released,
                            work.operations.cancelled(),
                        )
                    };
                    if owed {
                        self.wake.nudge();
                    }
                    match step {
                        Step::Working if cancelled => {
                            job.abandon();
                            break Ending::Cancelled;
                        }
                        Step::Working => {}
                        Step::Done => break Ending::Completed,
                        Step::Stopped(reason) => break Ending::Stopped(reason),
                    }
                };
                // Its files close before its answer lands.
                let owed = {
                    let mut work = self.work.lock();
                    let released = job.release_hold(&mut work.artwork);
                    let omissions = job.omissions();
                    drop(job);
                    work.operations.finish(ending, omissions) | released
                };
                if owed {
                    self.wake.nudge();
                }
            }
        }

        /// No reader will answer: every desk's work is done on the loop, and
        /// the windows' watches are kept, drained there too. Refused its
        /// scratch, each drain reads its changes a few at a time instead.
        fn alone(&self) {
            let mut work = self.work.lock();
            work.halt();
            work.alone = true;
            work.scratch = tairix_util::fallible::filled(WATCH_BUFFER_LEN, 0).unwrap_or_default();
        }
    }

    impl Work {
        /// Stop recording work on every desk but the watches, so what is asked
        /// is done where it is asked.
        fn halt(&mut self) {
            self.stopping = true;
            drop(self.opens.stop());
            self.listings.stop();
            // Overwrites every decode still held, so one user's rendered
            // pixels do not outlive their window in reusable heap.
            self.artwork.stop();
            self.probes.stop();
            self.properties.stop();
            self.drives.stop();
            self.bundles.stop();
            self.places.stop();
            self.network.stop();
        }
    }

    /// Take everything the service holds for `open`'s session into its
    /// catalogue.
    fn drain_session(open: &mut NetworkSession) -> Result<(), DiscoveryError> {
        let NetworkSession { session, catalogue } = open;
        while session.collect(&mut |entry| catalogue.apply(entry))? {}
        Ok(())
    }

    /// Stops the workers on every way out, so neither is left on a disk for
    /// windows that have gone.
    ///
    /// The reader is *detached* rather than joined: one mid-read of a slow disk
    /// would otherwise hold the teardown for as long as that disk takes, and it
    /// leaves at its next turn round its loop anyway. The operations worker is
    /// joined: it stops at its next unit, a bounded one, and only once it has
    /// does a copy it gave up remove the file it was writing.
    struct ReadsGuard {
        reads: alloc::sync::Arc<Reads>,
        operations: Option<tairix_rt::thread::JoinHandle<()>>,
    }

    impl Drop for ReadsGuard {
        fn drop(&mut self) {
            self.reads.stop();
            if let Some(worker) = self.operations.take() {
                let _ = worker.join();
            }
        }
    }

    /// Start the operations worker, stating a refusal once.
    ///
    /// Without it an operation runs on the event loop to its end: slower, and
    /// not cancellable, but never wrong.
    fn spawn_operations(
        reads: &alloc::sync::Arc<Reads>,
    ) -> Option<tairix_rt::thread::JoinHandle<()>> {
        let served = alloc::sync::Arc::clone(reads);
        match tairix_rt::thread::Thread::spawn(move || served.serve_operations()) {
            Ok(handle) => Some(handle),
            Err(err) => {
                report_error(&alloc::format!(
                    "no operations thread ({err:?}); copies and deletes run on the event loop"
                ));
                None
            }
        }
    }

    /// Start the reader, stating a refusal once.
    ///
    /// A kernel that will not grant the thread is not a failure: the reads move
    /// back onto the event loop, which is exactly where they used to be.
    fn spawn_reader(reads: &alloc::sync::Arc<Reads>) -> Option<tairix_rt::thread::JoinHandle<()>> {
        let served = alloc::sync::Arc::clone(reads);
        match tairix_rt::thread::Thread::spawn(move || served.serve()) {
            Ok(handle) => Some(handle),
            Err(err) => {
                report_error(&alloc::format!(
                    "no reader thread ({err:?}); directory listings happen on the event loop"
                ));
                None
            }
        }
    }

    /// The paint's artwork seam: whatever the reader has already decoded, and
    /// otherwise a recorded decode and the built-in glyph for this frame.
    ///
    /// Held by the pipeline behind a boxed trait object, which is why it owns
    /// its handle to the desk rather than borrowing one.
    struct DeferredArtwork(alloc::sync::Arc<Reads>);

    impl ArtworkResolver for DeferredArtwork {
        fn resolve(&mut self, key: &ArtworkKey, side: u32) -> Resolved {
            self.0.resolve_artwork(key, side)
        }

        fn prefetch(&mut self, key: &ArtworkKey, side: u32) {
            self.0.want_artwork(key, side);
        }

        fn declined(&mut self, key: &ArtworkKey, side: u32) {
            self.0.decline_artwork(key, side);
        }

        fn still_shown(&mut self, key: &ArtworkKey, side: u32) {
            self.0.keep_artwork(key, side);
        }
    }

    /// One browser window's directory seam: a [`DirectorySource`] that records
    /// a request under the window's own consumer and answers with whatever has
    /// come back for it.
    #[derive(Clone)]
    struct DeferredSource(alloc::sync::Arc<Reads>, FilesClient);

    impl DirectorySource for DeferredSource {
        fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
            self.0.list(self.1, components)
        }

        fn refresh(&mut self, components: &[String]) -> Result<Listing, Errno> {
            self.0.refresh(self.1, components)
        }

        fn follows(&self, components: &[String]) -> bool {
            self.0.follows(self.1, components)
        }

        fn has_children(&mut self, components: &[String]) -> Result<Probe, Errno> {
            self.0.probe(components)
        }
    }

    /// Read the directory named by root-first `components` under this app's own
    /// identity, through the same validated path spelling and stream decode the
    /// synchronous source uses.
    fn read_directory(components: &[String]) -> Result<Vec<Entry>, Errno> {
        let path = tairix_browse::vfs::absolute_path(components)?;
        let stream = list_directory(&path)?;
        tairix_browse::vfs::entries_from_dir_stream(&path, &stream, &mut RtLinkReader)
    }

    /// Describe the node `job` names: one `fs_stat` under the user's own
    /// identity, plus its visible extended attributes.
    ///
    /// A link is described as *itself*, through a resolve-only `NO_FOLLOW`
    /// handle: that is the only reading under which a link to a file can be
    /// described at all, and it is what makes the permission string honestly
    /// read `l` rather than the target's kind. `stat` needs only a live
    /// handle, not an access bit.
    fn describe_node(job: &PropertyJob) -> Result<Properties, Errno> {
        let flags = match job.kind {
            EntryKind::Link(_) => OpenFlags::NO_FOLLOW,
            EntryKind::File => OpenFlags::READ,
            EntryKind::Directory | EntryKind::Bundle => OpenFlags::DIRECTORY,
        };
        let node =
            tairix_rt::File::open(job.path.as_bytes(), flags).map_err(Errno::from_syscall)?;
        let stat = node.stat().map_err(Errno::from_syscall)?;
        Ok(Properties::from_stat(path_leaf(&job.path), job.kind, &stat)
            .with_attributes(read_attributes(&node)))
    }

    /// Read what is mounted at `job`'s mount point and its device's service
    /// counters, through the ungated mount table and service list.
    ///
    /// The counters are joined to the mount by its volume identity, so a
    /// volume that published none, or whose device reports no service, has
    /// none — never another volume's.
    fn read_drive(job: &DriveJob) -> DriveReading {
        let mut mount = None;
        let listed = tairix_procinfo::for_each_mount(&IpcTransport, |record| {
            let at = core::str::from_utf8(record.target_bytes())
                .ok()
                .and_then(|target| tairix_browse::vfs::components_from_absolute_path(target).ok());
            if at.as_deref() == Some(job.mount_point.as_slice()) {
                mount = Some(*record);
                return Ok(WalkStep::Stop);
            }
            Ok(WalkStep::Continue)
        });
        if listed.is_err() {
            return DriveReading::Unreadable;
        }
        let identity = mount
            .map(|record| record.volume_id())
            .filter(|id| *id != tairix_abi::sysinfo::VOLUME_ID_UNPUBLISHED);
        let mut service = None;
        if let Some(identity) = identity {
            // A refused service list leaves the reading without activity
            // rather than without the volume.
            let _ = tairix_procinfo::for_each_volume_service(&IpcTransport, |record| {
                if record.volume_id() == identity {
                    service = Some(*record);
                    return Ok(WalkStep::Stop);
                }
                Ok(WalkStep::Continue)
            });
        }
        DriveReading::Read(Box::new(DriveSample {
            mount,
            service,
            at_ns: tairix_rt::clock_get(),
        }))
    }

    /// The visible extended attributes of the node `node` holds.
    ///
    /// The kernel omits keys whose namespace the caller may not read, so this
    /// only ever sees what it may show. A volume that stores no attributes is
    /// stated as such rather than reported as a node with none, and a refused
    /// listing states its reason — an empty list would be a claim the reader
    /// could tell from neither.
    ///
    /// A value the node lost between the listing and the read is dropped
    /// rather than shown as empty: the key is gone, which is what the next
    /// read will say.
    fn read_attributes(node: &tairix_rt::File) -> Attributes {
        let keys = match node.attr_keys() {
            Ok(keys) => keys,
            Err(ret) => {
                return match Errno::from_syscall(ret) {
                    Errno::NotSupported => Attributes::Unsupported,
                    errno => Attributes::Refused(errno),
                }
            }
        };
        Attributes::Visible(
            keys.into_iter()
                .filter_map(|key| {
                    node.attr_value(key.as_bytes())
                        .ok()
                        .map(|value| Attribute::new(key, value))
                })
                .collect(),
        )
    }

    /// Adopt the desktop the session published, if the park said it moved,
    /// answering whether anything the windows draw from actually changed.
    ///
    /// A refused state is reported and the last good desktop stands, so every
    /// window keeps drawing correctly rather than at a nonsense density.
    fn adopt_desktop(
        desktop: &mut Desktop,
        themes: &mut ThemeRegistry,
        moved: &Cell<bool>,
    ) -> bool {
        if !moved.replace(false) {
            return false;
        }
        match app::adopt_desktop(desktop, themes) {
            Ok(changed) => changed,
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("could not adopt the desktop change: {err}"),
                );
                false
            }
        }
    }

    /// Walk the machine-wide program stores for the file types their bundles
    /// declare, under the file manager's own `CAP_FS_ACCESS` (never a
    /// compiled-in list).
    ///
    /// The walk is the shared one (`lib/appstore`), so this table, the program
    /// library's `rescan`, and the desktop's icon-bar identity index find the
    /// same bundles by the same rule. A declared type is a display *hint*: a
    /// bundle offered here is still launched through the ordinary signed load
    /// gate. Fail-closed per bundle: one whose manifest cannot be read, is
    /// over-long, or does not decode simply declares no types, so a corrupt
    /// bundle is never offered on a guess. A tree the shared walk refuses
    /// outright yields no candidates at all rather than a partial table.
    fn scan_bundles() -> Vec<AppAssociation> {
        let mut out = Vec::new();
        let scanned = tairix_appstore::walk(
            &RtStoreReader,
            &tairix_appstore::MACHINE_ROOTS,
            |bundle: tairix_appstore::Bundle<'_>| match association_from_manifest(
                bundle.path,
                bundle.header,
                bundle.manifest,
            ) {
                Some(assoc) => {
                    out.push(assoc);
                    Verdict::Accepted
                }
                None => Verdict::Refused,
            },
        );
        if scanned.is_err() {
            out.clear();
        }
        out
    }

    /// One frame of the output every window draws on: the least the artwork
    /// cache they all share may grow to. Zero for a screen whose frame no
    /// address could hold.
    fn screen_bytes(desktop: &Desktop) -> usize {
        app::region_bytes(
            &app::mode_for(desktop.screen_width_px(), desktop.screen_height_px()),
            app::FRAME_COUNT,
        )
        .unwrap_or(0)
    }

    /// Build the pipeline every window draws through, its cache free to grow
    /// to the share of `memory_bytes` and never held below `screen_bytes`
    /// ([`screen_bytes`]), since every window's icons share it.
    ///
    /// The cache is built through the one shared constructor with this app's
    /// real seat, output size, pressure gauge, and audit sink, so it is
    /// classified and budgeted by the same desktop policy the session's caches
    /// obey rather than by numbers picked here.
    ///
    /// With a reader thread the decode happens there and a paint that misses
    /// draws the built-in glyph until the pixels land; `deferred` false means
    /// the kernel granted none, so the read and the sandbox round trip happen
    /// in the paint, exactly as they used to.
    fn open_icons(
        (screen_bytes, memory_bytes): (usize, usize),
        reads: &alloc::sync::Arc<Reads>,
        deferred: bool,
    ) -> IconPipeline {
        // The reclaim bookkeeping's audit sink. The shared constructor takes a
        // `'static` borrow, and the runtime sink is a unit value that owns
        // nothing.
        static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;
        let cache = artwork_cache(
            "files.icon-artwork",
            SEAT_PRIMARY,
            (screen_bytes, memory_bytes),
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
        );
        // Decoded artwork is memory only this process can see, so the app says
        // what it holds; nothing outside it can sample the counters. A cache
        // declared unclassifiable has no ledger and holds nothing, so there is
        // simply nothing to report.
        if let Some(ledger) = cache.ledger() {
            tairix_rt::cachereport::register(ledger);
        }
        let resolver: alloc::boxed::Box<dyn ArtworkResolver> = if deferred {
            alloc::boxed::Box::new(DeferredArtwork(alloc::sync::Arc::clone(reads)))
        } else {
            alloc::boxed::Box::new(InlineArtwork::new(
                VfsArtworkReader,
                SandboxRasteriser::new(),
            ))
        };
        IconPipeline::new(cache, resolver)
    }

    /// The production [`EventSource`]: drain the app's own event
    /// mailbox, parking on the wait-set whenever it is empty, and accept
    /// only events whose kernel-attested sender is the desktop session
    /// named by the create reply — anything else is dropped (fail
    /// closed), so no other process can feed the app forged input.
    ///
    /// The same wait-set carries the any-child member ([`CHILD_TOKEN`]): a
    /// launched bundle exiting wakes the park, and the source reaps it in place
    /// before re-parking, so a child is never left a zombie and the wake never
    /// degrades into a busy-poll. The shared shell's memory-pressure member
    /// reports a [`Wake::PressureChanged`], and the retained artwork is trimmed
    /// there and then, so memory goes back when the machine asks for it rather
    /// than at whatever later moment the user next types. The reader's wake
    /// ([`READS_TOKEN`]) is the one member the source cannot serve itself: the
    /// answer is the loop's to adopt, so the wake is drained here and the wait
    /// ends without an event.
    struct RtEventSource<'a> {
        /// The app's own event mailbox, which authenticates every frame it
        /// hands over.
        mailbox: EventMailbox,
        /// The wait-set handle the app parks on.
        set: u64,
        /// The launched-bundle bookkeeping reaped on a [`CHILD_TOKEN`] wake,
        /// shared with the activation path that spawns.
        launcher: &'a RefCell<Launcher>,
        /// The grid's artwork pipeline, trimmed on a [`Wake::PressureChanged`],
        /// shared with the present path that draws through it.
        icons: &'a RefCell<IconPipeline>,
        /// The reader's wake, drained on a [`READS_TOKEN`] wake. Its readiness
        /// is a level peek, so leaving it undrained would report ready for
        /// ever and turn the park into a spin.
        reads: &'a Reads,
        /// Set when the park woke for a desktop change, cleared when the loop
        /// adopts it.
        desktop_moved: &'a Cell<bool>,
        /// The settings writer, whose wake is drained on a [`SETTINGS_TOKEN`]
        /// wake for the loop to adopt its answer.
        settings: &'a PublishWorker<Preferences>,
        /// Where a places read the mount notice asked for lands when no worker
        /// was there to read it.
        places_read: &'a PlacesRead,
        /// The soonest a window needs the loop with no event to bring it — a
        /// band's step, a clicked name opening — and the one deadline the park
        /// keeps for the loop.
        due: &'a Cell<Option<u64>>,
        /// Set when the discovery service rang the network session, cleared
        /// when the loop asks for what moved.
        network_rung: &'a Cell<bool>,
    }

    /// Places read on the loop's own thread because no worker was there to
    /// read them, adopted on the loop's next turn exactly as a reader's answer
    /// is — whether the mount notice or the refresh gesture asked.
    type PlacesRead = RefCell<Option<(Vec<String>, Vec<Volume>)>>;

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            // Park until the session's next delivery — or a launched bundle's
            // exit — wakes the wait-set, never a spin. A cache-report change
            // the rate limiter is holding back only ever *tightens* the park
            // to the moment it may be sent; with nothing pending the park
            // stays indefinite.
            let due = self.due.get();
            let loop_ns = due.map_or(u64::MAX, |due| {
                tairix_window::park::remaining_ns(due, tairix_rt::clock_get())
            });
            let timeout_ns = tairix_rt::cachereport::fold_wait_deadline_ns(loop_ns);
            let Some(wake) = app::park_for(self.set, timeout_ns)? else {
                // No member woke, so one of the two deadlines is due: the
                // held-back report, or the loop's own.
                tairix_rt::cachereport::publish_if_due();
                if due.is_some_and(|due| tairix_rt::clock_get() >= due) {
                    return Ok(Parked::Interrupted);
                }
                return Ok(Parked::Served);
            };
            match wake {
                // The reader answered. Draining is the whole of noticing it,
                // and the answer is the loop's to adopt, so the wait ends here
                // rather than parking again on a source that is still ready.
                Wake::App(READS_TOKEN) => {
                    self.reads.wake.drain();
                    return Ok(Parked::Interrupted);
                }
                // A settings write was carried out; its answer is the loop's to
                // adopt.
                Wake::App(SETTINGS_TOKEN) => {
                    self.settings.wake().drain();
                    return Ok(Parked::Interrupted);
                }
                // A child-exit wake reaps the exited bundle(s) in place, so a
                // launched app is never left a zombie and the ready child
                // member cannot spin the park (it is drained the instant it
                // fires).
                Wake::App(CHILD_TOKEN) => self.launcher.borrow_mut().reap(),
                // The machine's band moved: give back whatever the new band
                // says the decoded artwork may no longer keep, here at the
                // wake rather than at the next user input. A decode the old
                // band refused to keep is offered again, and the loop runs to
                // repaint what was drawn without it. The glyph cache this
                // window drew through gives back the same way.
                Wake::PressureChanged => {
                    self.icons.borrow_mut().trim();
                    let retried = self.reads.retry_declined_artwork();
                    tairix_font::trim_glyph_cache();
                    if retried {
                        return Ok(Parked::Interrupted);
                    }
                }
                // A volume was attached, re-backed, or removed. The rail is
                // re-read off the loop: what is mounted comes from the System
                // Information service, and a round trip here would stall the
                // frame this park owes.
                Wake::App(MOUNTS_TOKEN) => {
                    if let Some(found) = self.reads.want_places() {
                        *self.places_read.borrow_mut() = Some(found);
                    }
                    return Ok(Parked::Interrupted);
                }
                // The theme and density every window draws from moved.
                Wake::DesktopChanged => {
                    self.desktop_moved.set(true);
                    return Ok(Parked::Interrupted);
                }
                // What the network catalogue holds moved; what to read is
                // the loop's to ask the reader.
                Wake::App(DOORBELL_TOKEN) => {
                    if self.reads.drain_doorbell() {
                        self.network_rung.set(true);
                        return Ok(Parked::Interrupted);
                    }
                }
                // A folder a window shows changed. The drain is the reader's,
                // and its answer arrives as a reader wake for the loop to
                // adopt, so this park carries on — but with no reader it was
                // drained here, and is the loop's to adopt now.
                Wake::App(token) if token >= WATCH_TOKEN_BASE => {
                    let client = FilesClient::from_number(token - WATCH_TOKEN_BASE);
                    if self.reads.want_drain(client) {
                        return Ok(Parked::Interrupted);
                    }
                }
                Wake::Event | Wake::PressureUnchanged | Wake::App(_) => {}
            }
            Ok(Parked::Served)
        }
    }

    /// The open owner-id editor on the Properties overlay: which owning id is
    /// being edited and the inline [`TextField`] carrying the typed value.
    ///
    /// `None` unless the user (who must hold `CAP_FS_CHOWN`) clicked the uid or
    /// gid value to reassign it; the event loop threads it so the editor state
    /// and the painted control stay in step, exactly as `rename`/`properties`
    /// are threaded.
    struct OwnerEditor {
        /// Which owning id the editor commits.
        field: OwnerField,
        /// The inline numeric editor, pre-filled with the current id.
        editor: TextField,
    }

    /// The open delete-confirmation dialog and the plan it would carry out.
    ///
    /// `None` unless the user pressed `Delete` on a selection: the question
    /// put to them, and what answering yes removes. The plan is captured when
    /// the question is asked, so a listing change while it is up cannot move
    /// what the confirmed delete targets.
    struct DeleteConfirm {
        /// The question, until it is up, and then the alert asking it.
        ask: Ask,
        /// What the confirmed delete would remove, captured at open time.
        plan: DeletePlan,
        /// The recoverable move-to-Trash plan when the removal can be carried
        /// out as same-volume renames into the user's Trash
        /// (`plans/NEW-FILEMANAGER.md` `FM10`): the per-target
        /// source→destination renames, captured at open time so the dialog's
        /// "Move to Trash" wording and the confirmed action stay in step.
        /// `None` when Trash is unavailable or cross-volume, in which case the
        /// removal is the irreversible unlink and the dialog says so.
        trash_moves: Option<TrashMoves>,
    }

    /// A question a window asks: what it says until it is up, and then the
    /// alert asking it.
    enum Ask {
        Waiting(Box<AlertSpec>),
        Up(u64),
    }

    /// A long file operation a window asked for (`plans/NEW-FILEMANAGER.md`
    /// `FM7b`): its job until the loop hands it to the operations worker, and
    /// what its own progress window shows while it runs. The folder window
    /// goes on being used meanwhile.
    struct Operation {
        /// The job, until it is handed over.
        job: Option<Job>,
        /// The job carried out on this loop, a slice a turn, when no worker
        /// takes it.
        here: Option<Job>,
        /// What is done of what whole and a latched cancel, as its window
        /// shows them.
        progress: ProgressModel,
        /// The window's bar, clocked while it moves.
        trace: Progress,
        /// When its window last showed a new reading, so it moves once a frame.
        shown_ns: u64,
        /// A reading landed too soon after the last to show yet: its window is
        /// drawn again at this instant.
        due_ns: Option<u64>,
        /// When its bar is next drawn, and whether its window is on show.
        bar: BarPace,
        /// Once it has completed, when its window, its bar full, closes.
        closing_ns: Option<u64>,
    }

    impl Operation {
        fn new(job: Job, progress: ProgressModel) -> Self {
            let mut trace = Progress::new().uncaptioned();
            trace.set_state(progress_state(&progress));
            Self {
                job: Some(job),
                here: None,
                progress,
                trace,
                shown_ns: 0,
                due_ns: None,
                bar: BarPace::new(),
                closing_ns: None,
            }
        }

        /// Show `reading`, working on `current`, at `now_ns`.
        fn observe(&mut self, reading: Reading, current: &str, now_ns: u64) {
            self.progress.observe(reading, current, now_ns);
            let shown = self.trace.state();
            self.trace.set_state(progress_state(&self.progress));
            if self.trace.state() != shown {
                self.bar.restated();
            }
        }

        /// When its window is next due: a reading held for the next frame, the
        /// bar's own next frame, or its close once it has completed.
        fn due(&self) -> Option<u64> {
            [self.due_ns, self.bar.deadline(), self.closing_ns]
                .into_iter()
                .flatten()
                .min()
        }

        /// Whether `reading` and `current` say anything its window does not
        /// already show.
        fn differs(&self, reading: &Reading, current: &str) -> bool {
            self.progress.reading() != *reading || self.progress.current() != current
        }
    }

    /// A long file operation: the trees it counts first, where it cannot
    /// know its whole up front, and the walk that carries it out one bounded
    /// unit at a time ([`Job::step`]).
    struct Job {
        /// The count under way before the walk begins.
        survey: Option<Survey>,
        /// The whole, once counted; `None` where it could not be.
        total: Option<Tally>,
        walk: Walk,
        /// The item in hand, by its name.
        current: String,
        /// The tree the artwork desk was last told this job is writing
        /// ([`sync_hold`](Self::sync_hold)).
        held: Option<String>,
    }

    impl Job {
        /// A removal of `plan`'s targets and everything beneath them, counted
        /// first.
        fn delete(plan: &DeletePlan) -> Self {
            let roots = plan
                .targets()
                .iter()
                .map(|target| (target.path().to_vec(), target.is_directory(), 0));
            Self::counted(
                Survey::new(roots),
                Walk::Delete(Box::new(Removal::new(plan.clone()))),
            )
        }

        /// A move of `moves` to Trash: one rename each, so the whole is known.
        fn trash(moves: TrashMoves) -> Self {
            let items = u64::try_from(moves.len()).unwrap_or(u64::MAX);
            Self {
                survey: None,
                total: Some(Tally { items, bytes: 0 }),
                walk: Walk::Trash(Box::new(TrashRun::new(moves))),
                current: String::new(),
                held: None,
            }
        }

        /// A paste of its items, whose kinds and sizes the count reads first.
        fn paste(paste: Paste) -> Self {
            let roots = paste.items.iter().map(|item| item.source().to_vec());
            Self::counted(Survey::of(roots), Walk::Paste(Box::new(paste)))
        }

        fn counted(survey: Survey, walk: Walk) -> Self {
            Self {
                survey: Some(survey),
                total: None,
                walk,
                current: String::new(),
                held: None,
            }
        }

        /// What the walk says of itself: what is counted while it prepares,
        /// what is done of the whole once it works.
        fn reading(&self) -> Reading {
            match &self.survey {
                Some(survey) => Reading {
                    done: survey.found(),
                    preparing: true,
                    ..Reading::default()
                },
                None => Reading {
                    done: self.walk.done(),
                    total: self.total,
                    preparing: false,
                    removing: self.walk.removing(),
                },
            }
        }

        /// Hold off `desk`'s decodes of whatever this job is now writing, and
        /// release the tree it has finished, answering whether the release
        /// named anything for the windows to repaint.
        ///
        /// A copy writes a bundle's manifest and icon after the bundle itself
        /// appears, so a decode run meanwhile would picture, or refuse, the
        /// half-written bundle — and that answer would stand.
        fn sync_hold(&mut self, desk: &mut ArtworkDesk) -> bool {
            let writing = match &self.walk {
                Walk::Paste(paste) => paste.writing.as_deref(),
                Walk::Delete(_) | Walk::Trash(_) => None,
            };
            if self.held.as_deref() == writing {
                return false;
            }
            let released = self
                .held
                .take()
                .is_some_and(|tree| desk.release_under(&tree));
            if let Some(tree) = writing {
                desk.hold_under(tree);
                self.held = Some(String::from(tree));
            }
            released
        }

        /// The job has ended: release whatever it still held, answering
        /// whether that named anything for the windows to repaint.
        fn release_hold(&mut self, desk: &mut ArtworkDesk) -> bool {
            self.held
                .take()
                .is_some_and(|tree| desk.release_under(&tree))
        }
    }

    /// The walk a [`Job`] carries out.
    enum Walk {
        /// A removal of a captured plan, each target removed with everything
        /// beneath it by the shared engine, which removes a cross-volume move's
        /// originals too, so a delete and a move's cleanup can never diverge.
        Delete(Box<Removal>),
        /// A move/copy paste of a captured plan, driven by the [`Paste`] state
        /// machine, boxed for the operation it carries.
        Paste(Box<Paste>),
        /// A recoverable move to Trash (`plans/NEW-FILEMANAGER.md` `FM10`),
        /// driven by a [`TrashRun`]: one same-volume rename per target into
        /// the user's Trash directory, in place of an irreversible unlink.
        Trash(Box<TrashRun>),
    }

    /// The items of a removal or a paste, each carried out in turn by an
    /// operation of the shared engine, and what those finished did.
    #[derive(Default)]
    struct Turns {
        /// The next item to begin.
        next: usize,
        /// The item in hand: the source its refusal names, and its operation.
        in_hand: Option<(alloc::vec::Vec<String>, TreeOp<tairix_rt::Dir>)>,
        /// What the items finished did.
        finished: Tally,
        /// One copy buffer for every file, grown to at most a chunk by the
        /// thread doing the copy.
        buf: alloc::vec::Vec<u8>,
        /// The extended attributes the finished items' copies could not keep.
        omissions: Omissions,
    }

    impl Turns {
        /// Take `op`, carrying out the item at `source`, in hand.
        fn take(&mut self, source: &[String], op: TreeOp<tairix_rt::Dir>) {
            self.in_hand = Some((source.to_vec(), op));
        }

        /// One unit of the item in hand, a refusal stated with `verb`: once it
        /// ends, what it did joins what the items before it did, and the next
        /// unit begins the next item. `None` with no item in hand.
        fn unit(&mut self, verb: &str) -> Option<Step> {
            let (source, op) = self.in_hand.as_mut()?;
            let source = source.as_slice();
            let refusal =
                match op.advance(1, &mut self.buf) {
                    Advanced::Yielded => return Some(Step::Working),
                    Advanced::Done => op.report().kept.first().map(|kept| {
                        operation_refusal(verb, source, &OpError::Changed(kept.clone()))
                    }),
                    Advanced::Failed(error) => Some(operation_refusal(verb, source, &error)),
                    // An operation refusing taken names never asks.
                    Advanced::Paused => {
                        wind(op, &mut self.buf);
                        Some(item_refusal(verb, source, LOST_TRACK))
                    }
                };
            self.finished = with_done(self.finished, op.report());
            self.omissions.merge(op.omissions());
            self.in_hand = None;
            Some(refusal.map_or(Step::Working, Step::Stopped))
        }

        /// What the items finished and the one in hand have done.
        fn done(&self) -> Tally {
            match &self.in_hand {
                Some((_, op)) => with_done(self.finished, op.report()),
                None => self.finished,
            }
        }

        /// Whether the item in hand is a move removing the originals it carried.
        fn removing(&self) -> bool {
            self.in_hand.as_ref().is_some_and(|(_, op)| op.removing())
        }

        /// The operation ends part-way: what the item in hand made is settled,
        /// and a file it was writing removed rather than left half-copied.
        fn abandon(&mut self) {
            if let Some((_, op)) = self.in_hand.as_mut() {
                wind(op, &mut self.buf);
            }
        }

        /// The extended attributes the copies could not keep, the item in
        /// hand's among them.
        fn omissions(&self) -> Omissions {
            let mut omissions = self.omissions.clone();
            if let Some((_, op)) = &self.in_hand {
                omissions.merge(op.omissions());
            }
            omissions
        }
    }

    /// `tally` with what `report` says was done added.
    fn with_done(tally: Tally, report: &Report) -> Tally {
        Tally {
            items: tally
                .items
                .saturating_add(u64::try_from(report.done).unwrap_or(u64::MAX)),
            bytes: tally.bytes.saturating_add(report.bytes),
        }
    }

    /// Cancel `op` and let it settle what it made: its folders given their
    /// modes, a file part-written removed.
    fn wind(op: &mut TreeOp<tairix_rt::Dir>, buf: &mut alloc::vec::Vec<u8>) {
        op.resolve(Decision::Cancel);
        // Settling ends within the units it takes; a refusal on the way is moot
        // beside the stop already being stated.
        let _ = op.advance(usize::MAX, buf);
    }

    /// The name `path` ends in, in its folder opened through the rest of it,
    /// to hold the node `seen` ([`Named::seen`]).
    fn named(path: &[String], seen: FileId) -> Result<Named<tairix_rt::Dir>, Errno> {
        let spelled = tairix_browse::vfs::absolute_path(path)?;
        Ok(Named {
            seen,
            ..Named::parent(&WorkingDirectory, &spelled)?
        })
    }

    /// A removal of a captured plan's targets, each with everything beneath
    /// it.
    struct Removal {
        plan: DeletePlan,
        turns: Turns,
    }

    impl Removal {
        fn new(plan: DeletePlan) -> Self {
            Self {
                plan,
                turns: Turns::default(),
            }
        }

        /// One unit of the removal, the target in hand named in `current`. The
        /// first refusal stops it, leaving what was already removed removed.
        fn unit(&mut self, current: &mut String) -> Step {
            if let Some(step) = self.turns.unit("delete") {
                return step;
            }
            let Some(target) = self.plan.targets().get(self.turns.next) else {
                return Step::Done;
            };
            self.turns.next += 1;
            name_into(current, target.path());
            match named(target.path(), target.id()) {
                Ok(source) => {
                    self.turns.take(target.path(), TreeOp::remove(source));
                    Step::Working
                }
                Err(errno) => {
                    Step::Stopped(item_refusal("delete", target.path(), reach_refusal(errno)))
                }
            }
        }
    }

    /// One window's link to the desktop's menu service: the channel a chain is
    /// asked for over, the window the ask is scoped to, and the open id that
    /// window is waiting on an answer for.
    ///
    /// One value rather than three parameters threaded separately, because the
    /// exactly-once rule reads all three together: an answer is acted on only
    /// when it names the open *this* window minted, and the desktop never
    /// reuses an id, so anything else answers a gesture already settled.
    struct MenuLink<'a> {
        /// The window channel the open is sent over.
        client: &'a mut WindowClient<app::RtWindowTransport>,
        /// The session's id for the window the chain belongs to.
        window: u64,
        /// This window's unanswered menu gesture, if one is up.
        open: &'a mut Option<OpenMenuState>,
    }

    /// The window's own state one event round reads and may change.
    ///
    /// One value rather than parameters threaded separately, because every
    /// level of the router chain carries all of it: what the window is showing
    /// (the browser), what is layered over it (the overlays), the rail beside
    /// it (the places), and where the pointer last was over it.
    struct WindowState<'a, S: DirectorySource> {
        browser: &'a mut Browser<S>,
        overlays: &'a mut Overlays,
        places: &'a mut Places,
        pointer: Option<Point>,
        /// Whether the window held the keyboard before this round's event.
        keyboard_held: bool,
    }

    /// What one event round acts *through*, as against what it acts *on*.
    ///
    /// The desktop's menu chain and the app launcher: neither is window state,
    /// and both are carried at every level of the router chain, so they travel
    /// as one value for the reason [`WindowState`] does.
    struct Acts<'a> {
        menu: MenuLink<'a>,
        launcher: &'a RefCell<Launcher>,
        /// The reader every deferred read is asked for through.
        reads: &'a Reads,
        /// The program-store scan the quick offers are built from, held for
        /// the process: the stores are the same for every window, so one scan
        /// serves them all.
        installed: &'a Installed,
        /// What opening a popup of this window's own needs.
        popup: PopupLink,
        /// The artwork cache the chooser draws each candidate's own icon
        /// through, shared with every other surface this process paints.
        icons: &'a RefCell<IconPipeline>,
        /// Where a gesture records the Properties window it asked for.
        ///
        /// A window cannot be appended to the list the gesture's own window is
        /// borrowed from, so the node is resolved here — while the listing
        /// that named it is still in hand — and the window is opened once the
        /// round's borrow has ended.
        properties: &'a mut Option<PropertiesRequest>,
        /// The desktop's double-click interval, which a press on an entry is
        /// paired under.
        double_click: Duration64,
    }

    /// A Properties window a gesture asked for: the node it describes,
    /// resolved from the listing that named it.
    struct PropertiesRequest {
        /// The node's absolute path.
        path: String,
        /// What the listing called it.
        kind: EntryKind,
        /// The spelling a link stores, if it is one.
        target: Option<String>,
        /// The picture the listing drew it with.
        art: IconKind,
        /// Whether it is a volume root, described as its drive.
        drive: bool,
    }

    /// What a popup this app opens above one of its own windows is opened
    /// against: the seat's screen, the session it must be served by, and where
    /// the popup's events come back.
    ///
    /// A popup is sized against the *screen* rather than its parent — a chooser
    /// sized to its candidates must not be shrunk by a narrow window — and its
    /// events arrive on the same mailbox the parent's do, addressed by the
    /// popup's own window id.
    #[derive(Copy, Clone)]
    struct PopupLink {
        /// The seat's screen rectangle, at its own origin.
        screen: Rect,
        /// The serving session's attested identity, which every popup's
        /// create reply must match. Absent until the desktop has answered a
        /// query, so a popup asked for before then is refused rather than
        /// opened against an unattested server.
        server: Option<ProcId>,
        /// The app's own event mailbox endpoint.
        event_endpoint: u64,
    }

    impl PopupLink {
        /// The context a popup opened during this round is opened against.
        ///
        /// Read before the round borrows the client, because a popup's create
        /// reply is checked against the session that served its parent.
        fn of(
            client: &WindowClient<app::RtWindowTransport>,
            desktop: &Desktop,
            event_endpoint: u64,
        ) -> Self {
            Self {
                screen: desktop.screen(),
                server: client.session(),
                event_endpoint,
            }
        }
    }

    /// What an "Open With…" chooser needs once the bundle scan answers: the
    /// file it was asked about, by the same spelling every open uses.
    struct PendingChooser {
        /// The absolute path of the selected file.
        path: String,
        /// Its leaf name, which the candidate match is keyed off.
        name: String,
    }

    /// The transient overlay state layered over the browser view, threaded
    /// through the event loop so the painted overlays and the state they
    /// reflect stay in step. At most one of `rename`/`delete` is open at a
    /// time. Properties is not among them: it is a window of its own, so
    /// several nodes can be inspected at once and the listing stays usable
    /// while they are.
    struct Overlays {
        /// The in-place rename editor (`F2`), and the rename with the volume.
        rename: RenameEdit,
        /// The entry New ▸ made and the directory it made it in, whose rename
        /// opens once a listing there shows it.
        rename_on_arrival: Option<(Vec<String>, String)>,
        /// The delete-confirmation dialog, when open (`Delete`).
        delete: Option<DeleteConfirm>,
        /// The "Open With…" application chooser, when open (chosen from the
        /// context menu on a regular file).
        ///
        /// A chooser in its own **popup window**, not a menu and not an overlay
        /// drawn inside the listing: the candidate set is as long as the
        /// applications a user has installed, which no menu plate can promise
        /// to hold, and a panel sized to its own candidates cannot be one that
        /// borrows this window's extent. The right-click menu itself is the
        /// desktop's chain and appears nowhere here.
        open_with: Option<ChooserOverlay>,
        /// An operation this window asked for, until the loop gives it a
        /// progress window of its own.
        starting: Option<Operation>,
        /// The refusals stated in the window, one at a time until dismissed.
        notices: Notices,
        /// The one-shot write this window waits on.
        writing: Option<Writing>,
        /// The held cut/copy clipboard, captured by `Ctrl+X`/`Ctrl+C` and
        /// consumed by `Ctrl+V`. It lives in the app (not the browser), so it
        /// survives navigating to the paste target; a `Cut` is cleared once
        /// pasted (its sources have moved), a `Copy` is kept so it can be
        /// pasted again elsewhere.
        clipboard: Option<Clipboard>,
        /// The pointer double-click detector: a second quick primary press on
        /// the same item activates it, exactly as `Enter` does. It lives in the
        /// app (the engine is pointer-agnostic) and is reset whenever a press
        /// lands on chrome rather than an item, so a click through the toolbar
        /// or the places rail never pairs across it.
        double_click: DoubleClickTracker,
        /// A primary press on a selected entry that may yet become a drag.
        drag: Option<DragArm>,
        /// What this window handed the desktop to carry, until its drag ends.
        carrying: Option<Carrying>,
        /// The entry a press inside a multi-selection landed on, selected alone
        /// if the press is released without becoming a drag.
        collapse_on_release: Option<usize>,
        /// A click on the one selected entry's name, waiting to rename it.
        rename_arm: Option<RenameArm>,
        /// A press on the listing's ground, and the band a drag from it draws.
        sweep: Option<Sweep>,
    }

    /// A press on the listing's ground: armed until the pointer travels, then
    /// a band.
    enum Sweep {
        /// Pressed here.
        Armed(DragArm),
        /// The band being dragged, boxed: it carries the marks it selected,
        /// and a press that never travels should not pay for them.
        Live(Box<Band>),
    }

    /// A band, and the clock pacing the listing as it scrolls under it.
    struct Band {
        marquee: Marquee,
        /// When the listing last stepped, while the band is held where it
        /// scrolls.
        stepped_at: Option<u64>,
        /// Travel short of a whole pixel, in pixel-nanoseconds.
        carry: i128,
    }

    impl Band {
        /// Start or stop the clock as the head enters or leaves the strip where
        /// the listing scrolls.
        fn pace<S: DirectorySource>(&mut self, browser: &Browser<S>, frame: Frame<'_>, now: u64) {
            if marquee::autoscroll(browser, frame, &self.marquee) == 0 {
                self.stepped_at = None;
                self.carry = 0;
            } else if self.stepped_at.is_none() {
                self.stepped_at = Some(now);
            }
        }

        /// When the next step is due, a frame of `refresh` on, while the
        /// listing scrolls.
        fn due(&self, refresh: RefreshRate) -> Option<u64> {
            self.stepped_at
                .map(|at| at.saturating_add(refresh.frame_ns()))
        }

        /// Scroll the listing by the travel `now` owes and grow the band to
        /// where its head lies, answering whether the listing moved. The travel
        /// is the elapsed time at the band's speed, so a late wake loses none.
        fn step<S: DirectorySource>(
            &mut self,
            browser: &mut Browser<S>,
            frame: Frame<'_>,
            (now, refresh): (u64, RefreshRate),
        ) -> bool {
            let Some(since) = self
                .stepped_at
                .filter(|at| now >= at.saturating_add(refresh.frame_ns()))
            else {
                return false;
            };
            let speed = marquee::autoscroll(browser, frame, &self.marquee);
            self.carry += i128::from(speed) * i128::from(now - since);
            let whole = self.carry / 1_000_000_000;
            self.carry -= whole * 1_000_000_000;
            self.stepped_at = Some(now);
            let moved = i64::try_from(whole).is_ok_and(|delta| {
                delta != 0 && marquee::step(browser, frame, &mut self.marquee, delta)
            });
            self.pace(browser, frame, now);
            moved
        }
    }

    /// What a window handed the desktop to carry.
    struct Carrying {
        /// Every dragged entry as listed, in listing order.
        sources: Vec<Listed>,
        /// The one file a drop on an application opens, when the drag is one
        /// file.
        file: Option<PendingChooser>,
    }

    impl Carrying {
        /// The selection as a drag, and what the desktop is told of it; `None`
        /// with nothing selected.
        fn of<S: DirectorySource>(browser: &Browser<S>) -> Option<(Self, DragItems)> {
            let first = browser.selection().iter().next()?;
            let name = DocumentName::new(browser.entries().get(first)?.name()).ok()?;
            let sources = browser.listed_selection();
            let file = open_with_target(browser);
            let count = u32::try_from(sources.len()).ok()?;
            let items = DragItems::new(name, count, file.is_some()).ok()?;
            Some((Self { sources, file }, items))
        }
    }

    /// The "Open With…" chooser and the popup window it is drawn in.
    ///
    /// The surface is held for the popup's life for the same reason a window's
    /// is: allocating and zeroing one per present would be a whole-surface
    /// pass of its own.
    struct ChooserOverlay {
        /// What the user is picking from.
        chooser: OpenWithChooser,
        /// Pairs the presses that make a double-click an activation, keyed on
        /// the row — the same shared tracker the listing behind it uses, so a
        /// double-click means one thing on both surfaces.
        clicks: DoubleClickTracker,
        /// The popup's channel-side state.
        pane: WindowPane,
        /// The surface every frame of it is drawn into.
        surface: Surface,
        /// A round changed it since it was last presented.
        owed: bool,
    }

    impl Overlays {
        /// How a gesture asking for a one-shot change fares while one is still
        /// being made: `None` when none is, so it goes ahead. A repeat of the
        /// change in flight, as `repeats` judges it — a held key's repeats
        /// among them — is absorbed, answering `Some(false)`; any other is
        /// refused under `title`, answering `Some(true)` for its notice.
        fn while_writing(
            &mut self,
            title: &'static str,
            repeats: impl FnOnce(&BrowserWrite) -> bool,
        ) -> Option<bool> {
            let writing = self.writing.as_ref()?;
            if repeats(&writing.then) {
                return Some(false);
            }
            self.notices
                .refuse(NoticeSource::Write, title, STILL_WRITING);
            Some(true)
        }

        /// Put `next` in place of whatever chooser was open, closing the old
        /// popup.
        ///
        /// An assignment that found one already there would otherwise drop it
        /// without closing it, leaving a session-side popup window on screen
        /// with nothing owning it. Going through here makes that unable to
        /// happen by construction rather than by inspection.
        fn set_chooser(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            next: Option<ChooserOverlay>,
        ) {
            if let Some(previous) = core::mem::replace(&mut self.open_with, next) {
                let _ = previous.pane.close(client);
            }
        }
    }

    /// How the window is drawn at this moment: the active theme, the
    /// surface a frame is laid out for, and the desktop's UI density.
    ///
    /// One value rather than three parameters, because every consumer
    /// resolves the same geometry from them, and threading them separately
    /// is how a painted frame and the hit-test that must match it come to
    /// disagree. All three change together when the desktop does.
    #[derive(Copy, Clone)]
    struct Canvas<'a> {
        /// The active theme, as the desktop reports its appearance.
        theme: &'a Theme,
        /// The surface the current frame is laid out for.
        mode: &'a DisplayMode,
        /// The desktop's UI density.
        scale: Scale,
        /// The chrome bands this window is showing, so every measurement of
        /// the listing and every hit-test that inverts one read the same
        /// answer as the paint did.
        chrome: Chrome,
    }

    impl<'a> Canvas<'a> {
        /// The active theme.
        fn theme(&self) -> &'a Theme {
            self.theme
        }

        /// The whole window, as a rectangle at its own origin.
        fn window(&self) -> Rect {
            Rect::new(0, 0, self.mode.width_px, self.mode.height_px)
        }

        /// What the rail and toolbar leave of the window for the listing —
        /// the one inset the paint lays out in and every hit-test inverts, so
        /// a click lands on exactly the control the user saw.
        fn viewport(&self, places: &Places) -> Rect {
            tairix_browse::render::content_area(
                self.window(),
                self.scale,
                self.theme,
                self.chrome.rail(places),
                self.chrome.toolbar,
            )
        }
    }

    /// Latch every folder cue `browser`'s visible rows now have an answer for,
    /// reporting whether any of them moved.
    ///
    /// The one derivation of which rows are on screen, so the resolve a
    /// delivered probe batch drives and the resolve a paint performs ask about
    /// exactly the same folders.
    fn resolve_visible_occupancy<S: DirectorySource>(
        browser: &mut Browser<S>,
        places: &Places,
        canvas: Canvas<'_>,
    ) -> bool {
        let range = tairix_browse::render::visible_range(
            browser,
            canvas.scale,
            canvas.theme(),
            canvas.viewport(places),
            canvas.chrome.toolbar,
        );
        browser.resolve_occupancy(range)
    }

    /// The manager chrome a browser window draws its listing within: what the
    /// paint lays out, and what a landing walks.
    fn manager_chrome<'a, S: DirectorySource>(
        browser: &Browser<S>,
        places: &'a Places,
        chrome: Chrome,
        overlays: &Overlays,
    ) -> ManagerChrome<'a> {
        ManagerChrome {
            tools: MANAGER_TOOLS,
            tool_model: manager_tool_model(browser),
            sidebar: chrome.rail(places),
            toolbar: chrome.toolbar,
            renaming: overlays.rename.field().and(browser.chosen_index()),
        }
    }

    /// Mark, in every window, the pictures drawn through a decode `landed`
    /// answered, and tell the pipeline of every other picture each window
    /// shows, so the sweep after the pass withdraws only what no window shows.
    /// Answers whether any window owes a frame.
    ///
    /// A decode changes only the slots drawing through it, so a batch that
    /// pictures nothing a window shows costs that window nothing.
    fn adopt_artwork(
        windows: &mut [OpenWindow],
        icons: &RefCell<IconPipeline>,
        grounds: Grounds<'_>,
        scale: Scale,
        landed: &Landed,
    ) -> bool {
        let mut pipeline = icons.borrow_mut();
        let mut owed = false;
        for win in windows {
            let mode = *win.pane.mode();
            let window = Rect::new(0, 0, mode.width_px, mode.height_px);
            let mut damage = Region::new();
            match &mut win.kind {
                WindowKind::Browser(state) => {
                    let chrome = manager_chrome(
                        &state.browser,
                        &state.places,
                        state.chrome,
                        &state.overlays,
                    );
                    manager_art(
                        &state.browser,
                        (scale, grounds.window),
                        window,
                        &chrome,
                        &mut |art| {
                            if pipeline.moved_by(landed, art) {
                                damage.add(art.at);
                            }
                        },
                    );
                    if let Some(overlay) = state.overlays.open_with.as_mut() {
                        let shown = *overlay.pane.mode();
                        let viewport = Rect::new(0, 0, shown.width_px, shown.height_px);
                        let mut moved = false;
                        chooser_art(
                            &overlay.chooser,
                            (scale, grounds.popups),
                            viewport,
                            &mut |art| {
                                moved |= pipeline.moved_by(landed, art);
                            },
                        );
                        // The popup is presented whole.
                        overlay.owed |= moved;
                        owed |= moved;
                    }
                }
                WindowKind::Properties(props) => {
                    properties_art(
                        &props.controls().identity,
                        (scale, grounds.popups),
                        window,
                        &mut |art| {
                            if pipeline.moved_by(landed, art) {
                                damage.add(art.at);
                            }
                        },
                    );
                }
                WindowKind::Progress(_) | WindowKind::Settings(_) => {}
            }
            if !damage.is_empty() {
                owe(win, Repaint::Reported, &damage);
                owed = true;
            }
        }
        owed
    }

    /// Render the browser into the window's retained surface, once under each
    /// of `target.parts`, and present those rectangles.
    ///
    /// Clipping is sound because the surface lives for the life of the window:
    /// every pixel outside the clips is the one already on screen, and the
    /// single shared frame holds the same. A round that could not describe
    /// what it changed asks for the whole window instead.
    ///
    /// The window's title is the location it is showing, so the frame begins by
    /// retitling when — and only when — the browser has moved since the last
    /// one. A repaint that did not move sends nothing.
    ///
    /// The frame also resolves the folder-occupancy of exactly the entries it
    /// is about to draw, so an empty folder and a full one are drawn apart.
    /// The probe is bounded by the visible range, so a listing of
    /// a hundred thousand entries costs one directory read per row on screen,
    /// not per entry, and each answer — including a refusal — is remembered
    /// until the next listing.
    #[allow(clippy::too_many_arguments)] // The window's state, its chrome, and the frame target.
    fn present_frame<S, T>(
        browser: &mut Browser<S>,
        overlays: &Overlays,
        places: &Places,
        chrome: Chrome,
        grounds: Grounds<'_>,
        target: &mut FrameTarget<'_, T>,
        icons: &RefCell<IconPipeline>,
        scale: Scale,
    ) -> Result<(), Errno>
    where
        S: DirectorySource,
        T: WindowTransport,
    {
        let theme = grounds.popups;
        let rename = overlays.rename.field();
        let mode = *target.pane.mode();
        let window = Rect::new(0, 0, mode.width_px, mode.height_px);
        let toolbar = chrome.toolbar;
        let canvas = Canvas {
            theme,
            mode: &mode,
            scale,
            chrome,
        };
        // The rail owns the window's leading edge, so an overlay is placed in
        // what is left: the inset the pointer hit-tests resolve through.
        let viewport = canvas.viewport(places);
        resolve_visible_occupancy(browser, places, canvas);
        let browser = &*browser;
        // The title is the location, so it is sent only when the browser has
        // moved. A refused retitle leaves the remembered text alone rather than
        // claiming a title the session does not carry: the next frame retries.
        if let Some(title) = retitle(browser, target.title) {
            if target.client.set_title(target.pane.id(), &title).is_ok() {
                *target.title = title;
            }
        }
        // Each visible grid tile resolves its icon through the artwork
        // pipeline: the shipped raster master for the entry's content type,
        // read bounded and decoded in the sandbox once per (asset, pixel side)
        // and retained, falling back to the built-in glyph whenever no artwork
        // resolves. Only the tiles the grid actually draws are asked for, so
        // nothing scrolled out of view is ever decoded. The borrow is taken for
        // the render alone; the parked event source holds it only to trim.
        let parts = target.parts;
        let surface = &mut *target.surface;
        let drawn = manager_chrome(browser, places, chrome, overlays);
        let band = match overlays.sweep.as_ref() {
            Some(Sweep::Live(band)) => {
                let frame = Frame {
                    scale,
                    theme,
                    viewport,
                    toolbar,
                };
                marquee::band(browser, frame, &band.marquee)
            }
            Some(Sweep::Armed(_)) | None => None,
        };
        {
            let mut pipeline = icons.borrow_mut();
            let mut source = pipeline.source();
            for part in parts.rects() {
                surface.with_clip(part.x, part.y, part.width_px, part.height_px, |surface| {
                    render_into(
                        surface,
                        browser,
                        scale,
                        grounds.window,
                        window,
                        &drawn,
                        &mut source,
                    );
                    if let Some(shown) = band {
                        marquee::draw(surface, shown, scale, theme);
                    }
                    // In rename mode, overlay the inline editor on the selected
                    // item's *name* through the shared geometry the views draw
                    // it at, so the field covers what is being edited and not
                    // the icon or the columns beside it, and scrolls with its
                    // item.
                    if let Some(field) = rename {
                        draw_rename_field(surface, field, browser, scale, theme, viewport, toolbar);
                    }
                    // The "Open With…" chooser has its own popup window, the
                    // right-click menu and every question are the desktop's own
                    // windows, and each operation's progress is a window of its
                    // own, so none is drawn here.
                });
            }
        }
        target.pane.present_list(target.client, surface, &parts)
    }

    /// Apply one delivered event to the browser, reporting whether the
    /// listing changed (and must re-present) and whether the app should
    /// end (the desktop asked the window to close).
    ///
    /// `canvas` gives the reveal/scroll helpers the same scale and content
    /// viewport the renderer uses, so the drawn view, the selection reveal,
    /// and the wheel scroll all agree on the geometry; `launcher` is the
    /// launched-bundle bookkeeping an activation spawns through.
    fn apply_event<S: DirectorySource>(
        win: &mut WindowState<'_, S>,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let WindowState {
            browser,
            overlays,
            places,
            ..
        } = win;
        let theme = canvas.theme();
        let scale = canvas.scale;
        // Everything below the rail lays out in what the rail leaves, resolved
        // through the one shared inset the renderer paints with, so a click
        // lands on exactly the control the user saw.
        let toolbar = canvas.chrome.toolbar;
        let viewport = canvas.viewport(places);

        // A close request closes *this* window whatever mode it is in; an open
        // rename edit or properties overlay is simply abandoned (nothing was
        // written). Whether the process then ends is the caller's to decide —
        // an ordinary file manager is its window, a component is not.
        //
        // The icon bar's own outcomes never reach here: they name the
        // application rather than a window, so the run resolves them before it
        // resolves which window an event belongs to.
        if let WindowEvent::CloseRequested { .. } = event {
            return (Repaint::Nothing, true);
        }
        // The one answer the desktop owes an open. An id that names anything
        // else answers a gesture already settled, so acting on it would run a
        // stale command.
        if let WindowEvent::MenuClosed {
            open_id, outcome, ..
        } = *event
        {
            if acts.menu.open.as_ref().map(|open| open.open_id) != Some(open_id) {
                return (Repaint::Nothing, false);
            }
            // Taken, so a second answer for one gesture — or an answer acting
            // on a gesture whose own context has gone — reaches nothing.
            let Some(gesture) = acts.menu.open.take() else {
                return (Repaint::Nothing, false);
            };
            return apply_menu_outcome(
                browser, overlays, acts, &gesture, canvas, viewport, outcome, damage,
            );
        }

        // The "Open With…" chooser is not handled here. It is a window of its
        // own and takes the keyboard when it opens, so the session addresses
        // its events to the popup's own id and the run routes them there
        // (`route_popup_event`). Feeding this window's events to it would
        // resolve *this* window's coordinates against the popup's viewport,
        // which could land on the chooser's Open button and launch something
        // the user never picked.
        //
        if overlays.drag.is_some()
            && press_step(PressHold::Armed, PressInput::of(event)) == PressStep::EndAndRoute
        {
            overlays.drag = None;
            overlays.collapse_on_release = None;
        }

        // Rename mode: the inline editor owns the keyboard and the presses
        // inside it. A press outside commits the name, and acts as it would
        // with no editor open only when the name was unchanged.
        if overlays.rename.is_open() {
            let frame = Frame {
                scale,
                theme,
                viewport,
                toolbar,
            };
            return match rename_event(
                browser,
                &mut overlays.rename,
                acts.reads,
                frame,
                event,
                damage,
            ) {
                RenameRouting::Taken(repaint) => (repaint, false),
                RenameRouting::Committed(closed) => {
                    let (routed, close) = route_listing_event(win, acts, canvas, event, damage);
                    (routed.merged(closed), close)
                }
            };
        }
        route_listing_event(win, acts, canvas, event, damage)
    }

    /// What routing an event to the open rename editor did.
    enum RenameRouting {
        /// The editor took it, repainting as this says.
        Taken(Repaint),
        /// A press outside the editor closed it on an unchanged name,
        /// repainting as this says; the press routes on as though no editor
        /// had been open.
        Committed(Repaint),
    }

    /// Route one event to the open rename editor, reporting what it repainted
    /// into `damage`.
    ///
    /// Keys edit, submit, or cancel ([`apply_rename_key`]); a press inside the
    /// field places the caret and a drag from it selects. A press outside it
    /// commits the name, and acts only when nothing moved
    /// ([`RenameCommit::press_acts`]). Anything else leaves the edit untouched,
    /// as does everything while the committed name is with the volume.
    fn rename_event<S: DirectorySource>(
        browser: &mut Browser<S>,
        edit: &mut RenameEdit,
        reads: &Reads,
        frame: Frame<'_>,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> RenameRouting {
        match event {
            WindowEvent::Key {
                key: KeyInput::Pressed { key, modifiers },
                ..
            } => RenameRouting::Taken(apply_rename_key(
                browser, edit, reads, frame, *key, *modifiers, damage,
            )),
            WindowEvent::Pointer { x, y, action, .. } => {
                let point = pointer_point(*x, *y);
                let bounds = editor_rect(browser, frame).unwrap_or(Rect::EMPTY);
                if matches!(action, PointerAction::Pressed(_)) && !bounds.contains(point) {
                    let (commit, repaint) = commit_rename(browser, edit, reads, frame, damage);
                    return if commit.press_acts() {
                        RenameRouting::Committed(repaint)
                    } else {
                        RenameRouting::Taken(repaint)
                    };
                }
                let Some(field) = edit.editing() else {
                    return RenameRouting::Taken(Repaint::Nothing);
                };
                let mut moved = damage::sink();
                for input in pointer_input_events(*action, point) {
                    field.on_pointer(&input, bounds, frame.scale, frame.theme, &mut moved);
                }
                for rect in moved.rects() {
                    damage.add(*rect);
                }
                RenameRouting::Taken(Repaint::reported_if(!moved.is_empty()))
            }
            _ => RenameRouting::Taken(Repaint::Nothing),
        }
    }

    /// Route one event to the listing and the chrome around it: the band a
    /// press on the ground draws, the places rail, then the view itself.
    fn route_listing_event<S: DirectorySource>(
        win: &mut WindowState<'_, S>,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let WindowState {
            browser,
            overlays,
            places,
            pointer,
            ..
        } = win;
        let theme = canvas.theme();
        let scale = canvas.scale;
        let window = canvas.window();
        let toolbar = canvas.chrome.toolbar;
        let viewport = canvas.viewport(places);

        // A band holds the pointer from its press to its release, wherever the
        // pointer goes, so it is served before the rail could claim a motion.
        let frame = Frame {
            scale,
            theme,
            viewport,
            toolbar,
        };
        let ended = match apply_band_event(browser, overlays, frame, event, damage) {
            BandRouting::Taken(repaint, close) => return (repaint, close),
            BandRouting::Passed(ended) => ended,
        };

        // The rail owns the window's leading edge: its hover highlight tracks
        // every motion that reaches here, and it consumes the presses and keys
        // that belong to it. Whatever it does not consume routes to the view,
        // carrying any repaint the highlight alone owed. A window showing no
        // rail routes nothing to one: there is no drawn row for a press to
        // land on, and hit-testing a rail that was never painted would take
        // presses from the listing beneath it.
        let hover = if canvas.chrome.rail {
            Repaint::reported_if(sidebar::track_hover(
                places, scale, theme, window, toolbar, event, damage,
            ))
        } else {
            Repaint::Nothing
        }
        .merged(ended);
        if canvas.chrome.rail {
            if let Some(outcome) = sidebar::apply_event(
                browser, places, scale, theme, window, toolbar, *pointer, event, damage,
            ) {
                if let Some(reason) = &outcome.refused {
                    report_error(reason);
                    overlays
                        .notices
                        .refuse(NoticeSource::Listing, NOT_OPENED, reason);
                }
                return (outcome.repaint.merged(hover), false);
            }
        }

        let (repaint, close) = apply_nav_event(win, acts, canvas, viewport, event, damage);
        (repaint.merged(hover), close)
    }

    /// A key pressed in navigation mode, reporting whether the view changed
    /// and whether the window should close.
    ///
    /// An accelerator runs its toolbar control's own dispatch, so a key and a
    /// click cannot diverge, and a tool the model has disabled does nothing,
    /// as a press on it would. Alt+Enter opens a Properties window, a plain
    /// Enter activates the selection and Shift+Enter lists a bundle rather
    /// than running it — the keyboard spelling of the pointer's
    /// shift-double-click — Delete opens the delete confirmation,
    /// Ctrl+X/C/V drive the clipboard verbs, and Ctrl+A / Ctrl+Shift+A select
    /// every entry or none; every other key is the shared `apply_nav_key`'s.
    #[allow(clippy::too_many_arguments)] // The key, its context, and the round's report.
    fn apply_nav_press<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        viewport: Rect,
        key: KeyValue,
        modifiers: AbiModifiers,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let theme = canvas.theme();
        let scale = canvas.scale;
        let toolbar = canvas.chrome.toolbar;
        if let Some(accelerator) = Accelerator::of(key, modifiers) {
            return whole(match accelerator {
                Accelerator::Command(command) => {
                    apply_toolbar_command(browser, scale, theme, viewport, toolbar, command)
                }
                Accelerator::Tool(tool) if manager_tool_model(browser).is_enabled(tool) => {
                    apply_manager_tool(browser, overlays, tool)
                }
                Accelerator::Tool(_) => (false, false),
            });
        }
        if matches!(key, KeyValue::Named(NamedKeyCode::Enter)) && modifiers.alt {
            whole(ask_properties(browser, acts.properties))
        } else if matches!(key, KeyValue::Named(NamedKeyCode::Enter)) {
            let frame = Frame {
                scale,
                theme,
                viewport,
                toolbar,
            };
            let changed = activate(
                browser,
                overlays,
                acts,
                frame,
                bundle_intent(modifiers.shift),
            );
            (whole_if(changed), false)
        } else if matches!(key, KeyValue::Named(NamedKeyCode::Delete)) {
            whole(begin_delete(browser, overlays))
        } else if let Some(verb) = clipboard_verb(key, modifiers) {
            whole(apply_clipboard_verb(browser, overlays, verb))
        } else if let Some(verb) = SelectionVerb::of_key(key, modifiers) {
            let frame = Frame {
                scale,
                theme,
                viewport,
                toolbar,
            };
            apply_selection_verb(browser, frame, verb, damage)
        } else {
            apply_nav_key(
                browser,
                &mut overlays.rename,
                scale,
                theme,
                viewport,
                toolbar,
                key,
                damage,
            )
        }
    }

    /// Route one event in plain navigation mode — no overlay is open, and the
    /// places rail did not claim it — reporting whether the view changed and
    /// whether the window should close.
    ///
    /// `viewport` is the rail-inset content area the listing and the scrollbar
    /// occupy; the toolbar band spans the whole window, which the routers below
    /// read from `canvas`.
    fn apply_nav_event<S: DirectorySource>(
        win: &mut WindowState<'_, S>,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        viewport: Rect,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let WindowState {
            browser, overlays, ..
        } = win;
        let theme = canvas.theme();
        let scale = canvas.scale;
        let toolbar = canvas.chrome.toolbar;
        match event {
            WindowEvent::Key {
                key: KeyInput::Pressed { key, modifiers },
                ..
            } => apply_nav_press(browser, overlays, acts, canvas, viewport, *key, *modifiers, damage),
            // A wheel gesture the desktop forwarded, in scroll units (this
            // window owns its own content scrolling): the listing's bar moves
            // it a fixed distance a detent, carrying what is short of a pixel,
            // and clamps at both ends so a large or hostile turn cannot run
            // past the content. The selection is untouched; the bar and the
            // items it slid report themselves.
            WindowEvent::Scrolled { dx, dy, .. } => {
                let moved = scroll_wheel(
                    browser,
                    scale,
                    theme,
                    viewport,
                    toolbar,
                    (*dx, *dy),
                    damage,
                );
                if let Some(Sweep::Live(band)) = overlays.sweep.as_mut() {
                    let frame = Frame {
                        scale,
                        theme,
                        viewport,
                        toolbar,
                    };
                    let head = band.marquee.head();
                    marquee::sweep(browser, frame, &mut band.marquee, head);
                }
                (Repaint::reported_if(moved), false)
            }
            // A pointer event the desktop routed into this window's local
            // coordinates: routed by `apply_pointer`.
            WindowEvent::Pointer { .. } => {
                apply_pointer(win, acts, canvas, viewport, event, damage)
            }
            // A secondary press on the window's Close control asks to leave the
            // folder rather than the window: it climbs to the parent and closes
            // only at the top, where there is nothing left to leave. A parent
            // that cannot be listed keeps the window open and states which place
            // was refused.
            WindowEvent::AlternateCloseRequested { .. } => match leave_directory(browser) {
                Leave::Climbed => (Repaint::Whole, false),
                Leave::Closed => (Repaint::Nothing, true),
                Leave::Refused(reason) => {
                    report_error(&reason);
                    overlays
                        .notices
                        .refuse(NoticeSource::Listing, NOT_OPENED, &reason);
                    (Repaint::Nothing, false)
                }
            },
            // Focus changes and key releases repaint nothing. The browser
            // never requests a pick, so a pick conclusion is a session bug and
            // is ignored rather than acted on (an unredeemed delegation is
            // reclaimed by the kernel at exit).
            //
            // Minimized needs no action: the window manager hides the
            // window and keeps its taskbar entry; the browser renders on
            // demand, so there is nothing to pause. A `Resized` is handled by
            // the event loop itself (it re-maps the frame region before
            // `apply_event` is called), so it never reaches this match; it is
            // listed here only to keep the arm exhaustive. These are honest
            // no-ops, not deferred work.
            //
            // A redraw request is already answered by the typed wait, which
            // re-presents the last frame with full-window damage before
            // handing the event on. The browser's state has not changed, so
            // rendering it again would draw the same pixels at the cost of a
            // second present.
            //
            // A desktop change is adopted and, when anything actually moved,
            // repainted by the event loop itself (through `adopt_desktop`)
            // before `apply_event` is called; nothing here needs to react to
            // it a second time.
            // Both icon-bar events were resolved before this dispatch, by
            // the routing that owns the whole application's windows rather
            // than by one of them.
            //
            // A `MenuClosed` was resolved before this dispatch too, against the
            // open id the window is waiting on; one reaching here named no such
            // open and is a stale answer, dropped rather than acted on.
            WindowEvent::Key { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::MenuClosed { .. }
            // The layer-surface feeds and a tool window's moves address
            // surfaces this application never opens, so none can arrive here.
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::AlertClosed { .. }
            | WindowEvent::Authorised { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::CloseRequested { .. }
            | WindowEvent::Focus { .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::RedrawRequested { .. }
            | WindowEvent::ContentReleased { .. }
            | WindowEvent::Resized { .. }
            | WindowEvent::FilePicked { .. }
            | WindowEvent::PickCancelled { .. }
            | WindowEvent::FolderPicked { .. }
            // A drag's reports are answered where every window is.
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::PreviewRendered { .. }
            | WindowEvent::Pinch { .. }
            // An open target opens a *new* window rather than moving this
            // one, so it is answered where the window set is (`bar_routed`)
            // and repaints nothing here.
            | WindowEvent::OpenRequested
            => (Repaint::Nothing, false),
        }
    }

    /// The conclusion of a router that answers `(changed, close)` and cannot
    /// name what it changed.
    const fn whole(outcome: (bool, bool)) -> (Repaint, bool) {
        (whole_if(outcome.0), outcome.1)
    }

    /// The mounted volumes offered to the places rail, read from the live
    /// mount table through the shared System Information client, each as
    /// [`Volume::from_mount`] reads its record. A refused or failed query
    /// yields no volumes at all, so the rail falls back to the user's own
    /// places rather than guessing what is mounted. The rail itself then
    /// re-validates every row.
    fn mounted_volumes() -> alloc::vec::Vec<Volume> {
        let mut volumes = alloc::vec::Vec::new();
        let _ = tairix_procinfo::for_each_mount(&IpcTransport, |record| {
            volumes.extend(Volume::from_mount(record));
            Ok(WalkStep::Continue)
        });
        volumes
    }

    /// Everything the places rail is built from, read from the live system:
    /// the logged-in user's home and whatever is mounted right now.
    ///
    /// The one place the rail's inputs are gathered, so the first build and
    /// every later refresh read exactly the same sources.
    fn places_source() -> (alloc::vec::Vec<String>, alloc::vec::Vec<Volume>) {
        (home_components().unwrap_or_default(), mounted_volumes())
    }

    /// Route one pointer event in navigation mode, reporting whether the view
    /// changed (and must re-present).
    ///
    /// The layers are resolved in the order they overlap on screen. The
    /// right-edge scrollbar owns its gutter, so it gets first refusal on a
    /// primary press/drag/release — a click on the bar scrolls the listing
    /// instead of selecting an item beneath it, and it consumes only events
    /// that belong to it. A secondary-button press asks the desktop to open
    /// this window's context menu on the item under the pointer, and a primary
    /// press is routed by [`apply_primary_press`]. Every other pointer action
    /// is a no-op.
    ///
    /// `viewport` is the rail-inset content area; the toolbar band spans the
    /// whole window, which [`apply_primary_press`] reads from `canvas`.
    /// What routing a band made of one event.
    enum BandRouting {
        /// The band took it.
        Taken(Repaint, bool),
        /// Route it on, owing what ending the band repainted.
        Passed(Repaint),
    }

    /// Route an event to the band a press on the listing's ground armed, as
    /// [`press_step`] decides.
    fn apply_band_event<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        frame: Frame<'_>,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> BandRouting {
        let hold = match overlays.sweep {
            None => return BandRouting::Passed(Repaint::Nothing),
            Some(Sweep::Armed(_)) => PressHold::Armed,
            Some(Sweep::Live(_)) => PressHold::Dragging,
        };
        match press_step(hold, PressInput::of(event)) {
            PressStep::Grow => match (event, overlays.sweep.as_mut()) {
                (WindowEvent::Pointer { x, y, .. }, Some(sweep)) => BandRouting::Taken(
                    grow_band(browser, sweep, frame, pointer_point(*x, *y), damage),
                    false,
                ),
                _ => BandRouting::Passed(Repaint::Nothing),
            },
            PressStep::End => BandRouting::Taken(end_band(browser, overlays, frame, damage), false),
            PressStep::EndAndRoute => {
                BandRouting::Passed(end_band(browser, overlays, frame, damage))
            }
            PressStep::Cancel => {
                BandRouting::Taken(cancel_band(browser, overlays, frame, damage), false)
            }
            PressStep::Hold => BandRouting::Taken(Repaint::Nothing, false),
            PressStep::Route => BandRouting::Passed(Repaint::Nothing),
        }
    }

    /// End the band, keeping what it selected.
    fn end_band<S: DirectorySource>(
        browser: &Browser<S>,
        overlays: &mut Overlays,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> Repaint {
        match overlays.sweep.take() {
            Some(Sweep::Live(band)) => {
                report_band(
                    marquee::band(browser, frame, &band.marquee),
                    None,
                    frame.scale,
                    damage,
                );
                Repaint::Reported
            }
            Some(Sweep::Armed(_)) | None => Repaint::Nothing,
        }
    }

    /// Take back what the band selected, and end it.
    fn cancel_band<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> Repaint {
        let Some(Sweep::Live(band)) = overlays.sweep.take() else {
            return Repaint::Nothing;
        };
        let mark = ViewMark::of(browser, frame);
        let shown = marquee::band(browser, frame, &band.marquee);
        marquee::cancel(browser, band.marquee);
        report_band(shown, None, frame.scale, damage);
        mark.report(browser, frame, damage);
        Repaint::Reported
    }

    /// Move the band's head to `point` — beginning the band once an armed
    /// press has travelled — and report what it repainted.
    fn grow_band<S: DirectorySource>(
        browser: &mut Browser<S>,
        sweep: &mut Sweep,
        frame: Frame<'_>,
        point: Point,
        damage: &mut Region,
    ) -> Repaint {
        if let Sweep::Armed(arm) = *sweep {
            if !arm.travelled(point, frame.scale) {
                return Repaint::Nothing;
            }
            let Some(marquee) = marquee::begin(browser, frame, arm.at) else {
                return Repaint::Nothing;
            };
            *sweep = Sweep::Live(Box::new(Band {
                marquee,
                stepped_at: None,
                carry: 0,
            }));
        }
        let Sweep::Live(band) = sweep else {
            return Repaint::Nothing;
        };
        let mark = ViewMark::of(browser, frame);
        let before = marquee::band(browser, frame, &band.marquee);
        marquee::sweep(browser, frame, &mut band.marquee, point);
        let after = marquee::band(browser, frame, &band.marquee);
        report_band(before, after, frame.scale, damage);
        let marked = mark.report(browser, frame, damage);
        band.pace(browser, frame, tairix_rt::clock_get());
        Repaint::reported_if(before != after || marked)
    }

    fn apply_pointer<S: DirectorySource>(
        win: &mut WindowState<'_, S>,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        viewport: Rect,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let toolbar = canvas.chrome.toolbar;
        let keyboard_held = win.keyboard_held;
        let WindowState {
            browser, overlays, ..
        } = win;
        let theme = canvas.theme();
        let scale = canvas.scale;
        let WindowEvent::Pointer {
            x,
            y,
            action,
            modifiers,
            at_ns,
            ..
        } = event
        else {
            return (Repaint::Nothing, false);
        };
        let point = pointer_point(*x, *y);
        let frame = Frame {
            scale,
            theme,
            viewport,
            toolbar,
        };
        let mut scrolled = None;
        let mark = ViewMark::of(browser, frame);
        for input in pointer_input_events(*action, point) {
            if let Some(repaint) = scroll_pointer(
                browser, scale, theme, viewport, toolbar, point, &input, damage,
            ) {
                scrolled = Some(scrolled.unwrap_or(false) || repaint);
            }
        }
        if let Some(repaint) = scrolled {
            // The bar reported its own drawn state; an offset it actually
            // moved draws every entry somewhere new besides. A sample that
            // changed neither repaints nothing.
            let moved = mark.report(browser, frame, damage);
            return (Repaint::reported_if(repaint || moved), false);
        }
        if *action == PointerAction::Moved {
            let travelled = overlays.drag.take_if(|arm| arm.travelled(point, scale));
            if let Some((carrying, items)) = travelled.and_then(|_| Carrying::of(browser)) {
                // A drag the desktop will not carry stays the press it was.
                if acts.menu.client.begin_drag(acts.menu.window, items).is_ok() {
                    overlays.carrying = Some(carrying);
                    overlays.collapse_on_release = None;
                }
            }
            return (Repaint::Nothing, false);
        }
        if *action == PointerAction::Cancelled {
            // The press never completed, so it collapses no selection.
            overlays.drag = None;
            overlays.collapse_on_release = None;
            return (Repaint::Nothing, false);
        }
        if matches!(action, PointerAction::Released(_)) {
            overlays.drag = None;
            if let Some(index) = overlays.collapse_on_release.take() {
                let mark = ViewMark::of(browser, frame);
                let collapsed = browser.select(index).is_ok();
                return (
                    Repaint::reported_if(collapsed && mark.report(browser, frame, damage)),
                    false,
                );
            }
        }
        if let Some(point) = secondary_press_point(*action, *x, *y) {
            let hit = tairix_browse::render::entry_index_at(
                browser, scale, theme, viewport, toolbar, point,
            );
            return whole(open_context_menu(
                browser,
                overlays,
                &mut acts.menu,
                acts.reads,
                acts.installed,
                point,
                hit,
            ));
        }
        match press_point(*action, *x, *y) {
            Some(point) => apply_primary_press(
                browser,
                overlays,
                acts,
                canvas,
                viewport,
                (point, *modifiers, keyboard_held, *at_ns),
                damage,
            ),
            None => (Repaint::Nothing, false),
        }
    }

    /// Handle one key press in navigation mode (not renaming), reporting
    /// whether the view changed (it never asks the app to close). Mirrors
    /// [`apply_rename_key`]'s shape; the window's accelerators, Alt+Enter
    /// (Properties, which opens a window) and a plain Enter (activation, which
    /// needs the launcher) are handled by the caller, which owns the overlays,
    /// the window list and the launcher state.
    #[allow(clippy::too_many_arguments)] // The key, its context, and the round's report.
    fn apply_nav_key<S: DirectorySource>(
        browser: &mut Browser<S>,
        rename: &mut RenameEdit,
        scale: Scale,
        theme: &Theme,
        viewport: Rect,
        toolbar: ToolbarBand,
        key: KeyValue,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        match key {
            KeyValue::Named(NamedKeyCode::Down) => walk_selection(
                browser,
                scale,
                theme,
                viewport,
                toolbar,
                damage,
                Browser::select_next,
            ),
            KeyValue::Named(NamedKeyCode::Up) => walk_selection(
                browser,
                scale,
                theme,
                viewport,
                toolbar,
                damage,
                Browser::select_previous,
            ),
            KeyValue::Named(NamedKeyCode::Backspace) => {
                (whole_if(browser.go_up().unwrap_or(false)), false)
            }
            // F2 begins an in-place rename of the selected item; with nothing
            // selected (an empty directory) it is a no-op.
            KeyValue::Named(NamedKeyCode::F2) => whole(begin_rename(
                browser, rename, scale, theme, viewport, toolbar,
            )),
            _ => (Repaint::Nothing, false),
        }
    }

    /// Select every entry or none, reporting the entries whose mark changed.
    fn apply_selection_verb<S: DirectorySource>(
        browser: &mut Browser<S>,
        frame: Frame<'_>,
        verb: SelectionVerb,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let mark = ViewMark::of(browser, frame);
        match verb {
            SelectionVerb::All => browser.select_all(),
            SelectionVerb::Clear => browser.clear_selection(),
        }
        (
            Repaint::reported_if(mark.report(browser, frame, damage)),
            false,
        )
    }

    /// Move the listing's focus with `step`, keep it on screen, and report the
    /// entries the mark moved between — or the whole item area when keeping it
    /// on screen scrolled the view.
    fn walk_selection<S: DirectorySource>(
        browser: &mut Browser<S>,
        scale: Scale,
        theme: &Theme,
        viewport: Rect,
        toolbar: ToolbarBand,
        damage: &mut Region,
        step: fn(&mut Browser<S>),
    ) -> (Repaint, bool) {
        let frame = Frame {
            scale,
            theme,
            viewport,
            toolbar,
        };
        let mark = ViewMark::of(browser, frame);
        step(browser);
        tairix_browse::render::reveal_selection(browser, scale, theme, viewport, toolbar);
        (
            Repaint::reported_if(mark.report(browser, frame, damage)),
            false,
        )
    }

    /// Activate the chosen entry through the shared dispatch-by-kind
    /// [`Browser::activate_selected`], answering whether the listing changed.
    ///
    /// A directory the engine descended into reveals its selection; a bundle
    /// is launched and a file opened in its application ([`Launcher`]), both
    /// asynchronously, so the loop never waits on a load. A refusal is stated
    /// over the window, the listing as it was. `intent` says what the gesture
    /// meant for a bundle.
    fn activate<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        acts: &mut Acts<'_>,
        frame: Frame<'_>,
        intent: BundleIntent,
    ) -> bool {
        match browser.activate_selected(intent) {
            Ok(Activation::Descended) => {
                tairix_browse::render::reveal_selection(
                    browser,
                    frame.scale,
                    frame.theme,
                    frame.viewport,
                    frame.toolbar,
                );
                true
            }
            Ok(Activation::LaunchBundle { path }) => {
                acts.launcher.borrow_mut().launch(acts.menu.client, &path);
                false
            }
            Ok(Activation::OpenFile { path }) => {
                acts.launcher
                    .borrow_mut()
                    .open_file(acts.menu.client, &path);
                false
            }
            // Mounting is what opening an endpoint means, and this system
            // has no client for its shares: the refusal is the answer.
            Ok(Activation::MountEndpoint(endpoint)) => {
                report_error(&alloc::format!(
                    "{} could not be opened: {}",
                    endpoint.name(),
                    netbrowse::MOUNT_UNAVAILABLE
                ));
                overlays.notices.refuse(
                    NoticeSource::Open,
                    &notice::not_opened(endpoint.name()),
                    netbrowse::MOUNT_UNAVAILABLE,
                );
                false
            }
            // Nothing was chosen, so nothing was refused.
            Err(tairix_browse::BrowseError::NoSuchEntry) => false,
            Err(err) => {
                let name = browser
                    .chosen_name()
                    .or_else(|| browser.chosen_network_item().map(NetworkItem::label))
                    .unwrap_or_default();
                let reason = command::listing_refusal(err);
                report_error(&alloc::format!(
                    "{} could not be opened: {reason}",
                    tairix_util::utf8::shown(name)
                ));
                overlays
                    .notices
                    .refuse(NoticeSource::Open, &notice::not_opened(name), reason);
                false
            }
        }
    }

    /// Ask whether the selection's removal can be a recoverable move to Trash
    /// (`plans/NEW-FILEMANAGER.md` `FM10`): the reader decides, so the
    /// confirmation it then opens is worded for exactly what a confirmed
    /// delete will do. With nothing selected [`Browser::plan_delete`] yields no
    /// plan and this does nothing (fail closed). The plan is captured now, so a
    /// listing change while the dialog is up cannot move what it removes.
    fn begin_delete<S: DirectorySource>(
        browser: &Browser<S>,
        overlays: &mut Overlays,
    ) -> (bool, bool) {
        let Some(plan) = browser.plan_delete() else {
            return (false, false);
        };
        let repeats =
            |then: &BrowserWrite| matches!(then, BrowserWrite::Delete(asked) if *asked == plan);
        if let Some(changed) = overlays.while_writing(NOT_DELETED, repeats) {
            return (changed, false);
        }
        overlays.writing = Some(Writing::new(
            WriteAct::PlanTrash(plan.clone()),
            BrowserWrite::Delete(plan),
        ));
        (false, false)
    }

    /// Ask whether to remove `plan`, worded for the Trash moves it can be
    /// carried out as, or for the irreversible unlink without them. A move to
    /// the Trash is undone by taking the items back out, so it asks only when
    /// `confirm_trash` says to; removing for good always asks.
    fn ask_delete(
        overlays: &mut Overlays,
        plan: DeletePlan,
        trash_moves: Option<TrashMoves>,
        confirm_trash: bool,
    ) {
        if trash_moves.is_some() && !confirm_trash {
            start_removal(overlays, &plan, trash_moves);
            return;
        }
        let disposition = if trash_moves.is_some() {
            DeleteDisposition::Trash
        } else {
            DeleteDisposition::Permanent
        };
        let Some(question) = delete_alert(&plan, disposition) else {
            return;
        };
        overlays.delete = Some(DeleteConfirm {
            ask: Ask::Waiting(Box::new(question)),
            plan,
            trash_moves,
        });
    }

    /// Resolve, for a confirmed removal of `plan`, whether it can be carried
    /// out as a recoverable move to the user's Trash and, if so, the per-target
    /// source→destination rename plan (`plans/NEW-FILEMANAGER.md` `FM10`).
    ///
    /// Returns `Some(moves)` only when the removal is wholly recoverable:
    /// `HOME` resolves to a real per-user home, the `Library/Trash` subtree is
    /// ensured, and **every** target shares Trash's volume (so a single
    /// `fs_rename` carries each intact). Any other outcome — an absent or root
    /// `HOME`, a Trash directory that cannot be created or stat'd, a
    /// cross-volume target (a mounted volume under the current directory), or a
    /// name that cannot be given a collision-free home in Trash — returns
    /// `None`, and the removal falls back to the irreversible unlink (fail
    /// closed). Every call here is the user's own permission-checked I/O — no
    /// new capability, no ambient authority.
    fn plan_trash_moves(plan: &DeletePlan) -> Option<TrashMoves> {
        let home = home_components()?;
        // A root (empty) home has no per-user Trash; fall back to permanent.
        if home.is_empty() {
            return None;
        }
        let trash = trash_dir(&home);
        ensure_trash_dir(&trash).ok()?;
        let trash_vol = VolumeId::new(stat_node(&trash)?.id.volume);
        // The names already in Trash: a move must never overwrite a
        // previously-trashed item, and a second same-named target in this very
        // batch must not collide with the first, so each resolved leaf is
        // reserved as it is chosen.
        let mut taken: alloc::vec::Vec<String> = read_children(&trash)
            .ok()?
            .into_iter()
            .map(|(name, _, _)| name)
            .collect();
        let mut moves = alloc::vec::Vec::with_capacity(plan.len());
        for target in plan.targets() {
            let item_vol = VolumeId::new(stat_name(target.path())?.id.volume);
            if trash_strategy(item_vol, trash_vol) != TrashStrategy::Move {
                // A cross-volume target cannot be renamed into Trash; the whole
                // removal takes the permanent path so the dialog stays honest.
                return None;
            }
            let dest = trash_dest_path(&trash, target.name(), &taken).ok()?;
            if let Some(leaf) = dest.last() {
                taken.push(leaf.clone());
            }
            let source = Listed {
                path: target.path().to_vec(),
                id: target.id(),
            };
            moves.push((source, dest));
        }
        Some(moves)
    }

    /// The logged-in user's home directory as root-first path components, read
    /// from the `HOME` the session exported (the same source the trusted picker
    /// starts at). `None` when `HOME` is unset or not a valid absolute path
    /// (fail closed — the caller falls back to the permanent delete rather than
    /// guessing a home).
    fn home_components() -> Option<alloc::vec::Vec<String>> {
        let home = tairix_rt::env_var(b"HOME")?;
        let home = core::str::from_utf8(home).ok()?;
        tairix_browse::vfs::components_from_absolute_path(home).ok()
    }

    /// Ensure the `Library/Trash` directory exists, creating the `Library`
    /// parent then `Trash` under the user's own identity (`fs_mkdir` is
    /// idempotent here — an already-present directory is success). `home`
    /// itself is assumed to exist (it came from `HOME`). Returns `false`,
    /// fail closed, if either directory cannot be created and is not already a
    /// directory, so the trash move degrades to the permanent path.
    fn ensure_trash_dir(trash: &[String]) -> Result<(), Errno> {
        // Trash lives at `<home>/Library/Trash`; ensure the immediate `Library`
        // parent, then Trash itself.
        let Some((_, library)) = trash
            .split_last()
            .filter(|(_, library)| !library.is_empty())
        else {
            return Err(Errno::NotFound);
        };
        ensure_dir(library)?;
        ensure_dir(trash)
    }

    /// Ensure a single directory at `components` exists, under the user's own
    /// identity. `true` when it was created, or already exists as a directory;
    /// `false` (fail closed) when the path cannot be spelled, the create is
    /// refused, or the name exists as a non-directory.
    fn ensure_dir(components: &[String]) -> Result<(), Errno> {
        let spelled = tairix_browse::vfs::absolute_path(components)?;
        match tairix_rt::fs_mkdir(spelled.as_bytes(), FS_NEW_DIR_MODE) {
            0 => Ok(()),
            _ if matches!(stat_node(components), Some(stat) if stat.kind == FileKind::Directory) => {
                Ok(())
            }
            ret => Err(Errno::from_syscall(ret)),
        }
    }

    /// Put up every question and notice a window is waiting to ask, one alert
    /// a window at a time, the delete question first because the user has
    /// just asked for it. One the desktop refuses is stated on `stderr`
    /// instead, and a delete it would not ask about is not carried out.
    fn raise_alerts(
        windows: &mut [OpenWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        endpoint: u64,
        full: &mut bool,
    ) {
        for win in windows {
            if *full {
                return;
            }
            let window_id = win.pane.id();
            match &mut win.kind {
                WindowKind::Browser(state) => {
                    let overlays = &mut state.overlays;
                    let question = overlays
                        .delete
                        .as_ref()
                        .map(|confirm| matches!(confirm.ask, Ask::Up(_)));
                    match next_alert(question, overlays.notices.is_up()) {
                        NextAlert::Question => {
                            let Some(confirm) = overlays.delete.as_mut() else {
                                continue;
                            };
                            let Ask::Waiting(asked) = &confirm.ask else {
                                continue;
                            };
                            match client.open_alert(endpoint, Some(window_id), asked) {
                                Ok(alert) => confirm.ask = Ask::Up(alert),
                                Err(Errno::LimitExceeded) => {
                                    *full = true;
                                    return;
                                }
                                Err(err) => {
                                    report_error(&alloc::format!(
                                        "the delete question could not be asked ({err}); \
                                         nothing was removed"
                                    ));
                                    overlays.delete = None;
                                }
                            }
                        }
                        NextAlert::Notice => {
                            *full = raise_notice(
                                &mut overlays.notices,
                                client,
                                endpoint,
                                Some(window_id),
                            );
                        }
                        NextAlert::Nothing => {}
                    }
                }
                WindowKind::Properties(props) => {
                    *full = raise_notice(&mut props.notices, client, endpoint, Some(window_id));
                }
                // A refusal that stops an operation is its folder's to state.
                WindowKind::Progress(_) | WindowKind::Settings(_) => {}
            }
        }
    }

    /// Put up `notices`' next notice, holding `owner`, when none is up.
    fn raise_notice(
        notices: &mut Notices,
        client: &mut WindowClient<app::RtWindowTransport>,
        endpoint: u64,
        owner: Option<u64>,
    ) -> bool {
        loop {
            match notices.raise(client, endpoint, owner) {
                Raised::Nothing | Raised::Up => return false,
                Raised::Full => return true,
                Raised::Refused(err, notice) => report_error(&alloc::format!(
                    "{}: {} (the desktop would not show this: {err})",
                    notice.headline(),
                    notice.detail()
                )),
            }
        }
    }

    /// The answer to alert `alert_id`, given to whichever window asked it.
    fn answer_alert(windows: &mut [OpenWindow], alert_id: u64, outcome: AlertOutcome) {
        for win in windows {
            let removal = match &mut win.kind {
                WindowKind::Browser(state) => {
                    let overlays = &mut state.overlays;
                    if overlays.notices.answered(alert_id) {
                        return;
                    }
                    let asked = overlays.delete.as_ref().is_some_and(
                        |confirm| matches!(confirm.ask, Ask::Up(up) if up == alert_id),
                    );
                    if asked {
                        answer_delete(overlays, outcome);
                    }
                    asked
                }
                WindowKind::Properties(props) => {
                    if props.notices.answered(alert_id) {
                        return;
                    }
                    false
                }
                WindowKind::Progress(_) | WindowKind::Settings(_) => false,
            };
            if removal {
                win.owed.owe_whole();
                return;
            }
        }
    }

    /// The delete question's answer: the removal begins when the user chose
    /// it, and nothing changes otherwise. The plan was captured when the
    /// question was asked, so a listing change while it was up cannot move
    /// what the removal targets, and the Trash moves match its wording.
    fn answer_delete(overlays: &mut Overlays, outcome: AlertOutcome) {
        let Some(confirm) = overlays.delete.take() else {
            return;
        };
        let chosen = matches!(
            outcome,
            AlertOutcome::Chosen { index } if usize::from(index) == DELETE_CONFIRM_INDEX
        );
        if chosen {
            start_removal(overlays, &confirm.plan, confirm.trash_moves);
        }
    }

    /// Start removing `plan`: as moves to the Trash where `trash_moves` carry
    /// it, else for good.
    fn start_removal(overlays: &mut Overlays, plan: &DeletePlan, trash_moves: Option<TrashMoves>) {
        if overlays.starting.is_some() {
            overlays.notices.refuse(
                NoticeSource::Operation,
                "Nothing was removed",
                "the operation asked for just before it is still being started",
            );
            return;
        }
        let subject = subject_of(plan.targets().iter().map(DeleteTarget::path));
        overlays.starting = Some(match trash_moves {
            Some(moves) => Operation::new(
                Job::trash(moves),
                ProgressModel::new(ProgressOp::Trash, subject, None),
            ),
            None => Operation::new(
                Job::delete(plan),
                ProgressModel::new(ProgressOp::Delete, subject, None),
            ),
        });
    }

    /// What an operation on `paths` acts on, as its window's headline names
    /// it: the one item by its name, or how many.
    fn subject_of<'a>(paths: impl ExactSizeIterator<Item = &'a [String]>) -> Subject {
        let count = paths.len();
        let mut paths = paths;
        match (count, paths.next().and_then(<[String]>::last)) {
            (1, Some(name)) => Subject::One(name.clone()),
            _ => Subject::Many(count),
        }
    }

    /// What one unit of an operation came to.
    impl Job {
        /// Carry out one bounded unit: one listing or one look at an item
        /// while counting, then one removal, one rename, one directory made,
        /// or one copy chunk. Every unit is the user's own permission-checked
        /// call, so no new authority is used.
        fn step(&mut self) -> Step {
            let Some(survey) = self.survey.as_mut() else {
                return match &mut self.walk {
                    Walk::Delete(removal) => removal.unit(&mut self.current),
                    Walk::Paste(paste) => paste.unit(&mut self.current),
                    Walk::Trash(run) => run.unit(&mut self.current),
                };
            };
            let counting = match &mut self.walk {
                Walk::Delete(_) | Walk::Trash(_) => count_step(survey, None),
                Walk::Paste(paste) => match paste.landing() {
                    Ok(landing) => count_step(survey, Some(landing)),
                    Err(refusal) => return refusal,
                },
            };
            if let Some(next) = survey.next() {
                name_into(&mut self.current, next.path());
            }
            if counting.is_none() {
                self.total = survey.total();
                self.survey = None;
                self.current.clear();
            }
            Step::Working
        }

        /// The operation ends part-way: what the item in hand made is settled,
        /// and a file it was writing removed rather than left half-copied.
        fn abandon(&mut self) {
            match &mut self.walk {
                Walk::Delete(removal) => removal.turns.abandon(),
                Walk::Paste(paste) => paste.turns.abandon(),
                Walk::Trash(run) => run.turns.abandon(),
            }
        }

        /// The extended attributes its copies could not keep.
        fn omissions(&self) -> Omissions {
            match &self.walk {
                Walk::Paste(paste) => paste.turns.omissions(),
                Walk::Trash(run) => run.turns.omissions(),
                Walk::Delete(_) => Omissions::default(),
            }
        }
    }

    impl Walk {
        /// Whether a move across volumes is removing the originals of what it
        /// has carried.
        fn removing(&self) -> bool {
            match self {
                Walk::Paste(paste) => paste.turns.removing(),
                Walk::Trash(run) => run.turns.removing(),
                Walk::Delete(_) => false,
            }
        }

        /// What is done so far.
        fn done(&self) -> Tally {
            match self {
                Walk::Delete(removal) => removal.turns.done(),
                Walk::Paste(paste) => paste.turns.done(),
                Walk::Trash(run) => run.turns.done(),
            }
        }
    }

    /// Where a paste's items land and how: what a count needs to tell an item
    /// a move renames in place, which carries nothing, from one it copies.
    #[derive(Copy, Clone)]
    struct Landing {
        op: ClipboardOp,
        volume: VolumeId,
    }

    /// One unit of counting what an operation will touch: one look at an item
    /// whose kind is not yet known, or one listing. `landing` is a paste's.
    /// Answers `None` once nothing is left to count.
    fn count_step(survey: &mut Survey, landing: Option<Landing>) -> Option<()> {
        match survey.next()? {
            SurveyStep::Stat(path) => {
                let path = path.to_vec();
                let Some(stat) = stat_name(&path) else {
                    survey.unreadable();
                    return Some(());
                };
                let renamed = landing.is_some_and(|landing| {
                    paste_strategy(landing.op, VolumeId::new(stat.id.volume), landing.volume)
                        == PasteStrategy::Rename
                });
                if renamed {
                    survey.stated(false, 0);
                } else {
                    let bytes = if stat.kind == FileKind::Regular {
                        stat.size
                    } else {
                        0
                    };
                    survey.stated(stat.kind == FileKind::Directory, bytes);
                }
            }
            SurveyStep::List(path) => {
                let path = path.to_vec();
                match read_children(&path) {
                    Ok(children) => {
                        let counted: alloc::vec::Vec<(String, bool, u64)> = children
                            .into_iter()
                            .map(|(name, kind, size)| {
                                let bytes = if kind.is_directory_backed() || kind.is_link() {
                                    0
                                } else {
                                    size
                                };
                                (name, kind.is_directory_backed(), bytes)
                            })
                            .collect();
                        survey.listed(&counted);
                    }
                    Err(_) => survey.unreadable(),
                }
            }
        }
        Some(())
    }

    /// Put the last component of `path`, the name a reader knows it by, into
    /// `name`, reusing what it holds.
    fn name_into(name: &mut String, path: &[String]) {
        name.clear();
        if let Some(leaf) = path.last() {
            name.push_str(leaf);
        }
    }

    /// Carry each operation no worker took a slice further on this loop: a
    /// serve loop's slice of units on a desktop refreshing at `refresh`, so
    /// input is served between slices and Cancel still stops it at a unit's
    /// end. Answers whether one is still working, which keeps the loop
    /// turning.
    fn step_here_operations(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        client: &mut WindowClient<app::RtWindowTransport>,
        reads: &Reads,
        (motion, refresh): (MotionTheme, RefreshRate),
    ) -> bool {
        let mut working = false;
        let mut ended = alloc::vec::Vec::new();
        for win in windows.iter_mut() {
            let id = win.pane.id();
            let Some(progress) = win.progress() else {
                continue;
            };
            let operation = &mut progress.operation;
            let Some(mut job) = operation.here.take() else {
                continue;
            };
            let until = tairix_rt::clock_get().saturating_add(work_slice_ns(refresh));
            let cancelled = operation.progress.is_cancel_requested();
            let ending = run_slice(
                || job.step(),
                || cancelled,
                || tairix_rt::clock_get() >= until,
            );
            if ending == Some(Ending::Cancelled) {
                job.abandon();
            }
            // The paint runs on this thread too, so nothing was asked for
            // during the slice: syncing the hold after it still precedes every
            // ask of what the slice began writing.
            {
                let mut work = reads.work.lock();
                if ending.is_some() {
                    job.release_hold(&mut work.artwork);
                } else {
                    job.sync_hold(&mut work.artwork);
                }
            }
            let reading = job.reading();
            let counted = operation.differs(&reading, &job.current);
            operation.observe(reading, &job.current, tairix_rt::clock_get());
            if let Some(ending) = ending {
                ended.push((
                    id,
                    Finished {
                        done: reading.done,
                        ending,
                        omissions: job.omissions(),
                    },
                ));
            } else {
                operation.here = Some(job);
                working = true;
                if counted {
                    win.owed.owe_whole();
                }
            }
        }
        for (id, finished) in ended {
            if let Some(index) = windows.iter().position(|win| win.pane.id() == id) {
                let now = tairix_rt::clock_get();
                finish_operation(windows, index, client, reads, finished, (now, motion));
            }
        }
        working
    }

    /// A recoverable move to Trash in progress (`plans/NEW-FILEMANAGER.md`
    /// `FM10`): each target moved by an operation of the shared engine to the
    /// Trash name chosen free when the dialog opened — one rename on the
    /// Trash's volume, the item moving intact.
    struct TrashRun {
        /// The sources and the Trash names they take, in listing order.
        moves: TrashMoves,
        turns: Turns,
    }

    impl TrashRun {
        fn new(moves: TrashMoves) -> Self {
            Self {
                moves,
                turns: Turns::default(),
            }
        }

        /// One unit of the move, the target in hand named in `current`. The
        /// first refusal stops it, leaving what was already moved in Trash.
        fn unit(&mut self, current: &mut String) -> Step {
            if let Some(step) = self.turns.unit("move") {
                return step;
            }
            let Some((source, dest)) = self.moves.get(self.turns.next) else {
                return Step::Done;
            };
            self.turns.next += 1;
            name_into(current, &source.path);
            let named_source = match named(&source.path, source.id) {
                Ok(named_source) => named_source,
                Err(errno) => {
                    return Step::Stopped(item_refusal("move", &source.path, reach_refusal(errno)))
                }
            };
            let target = match named(dest, FileId::NONE) {
                Ok(target) => target,
                Err(errno) => {
                    return Step::Stopped(item_refusal(
                        "move",
                        &source.path,
                        destination_refusal(errno),
                    ))
                }
            };
            self.turns.take(
                &source.path,
                TreeOp::transfer(
                    Verb::Move,
                    named_source,
                    target,
                    Taken::Refuse,
                    tairix_rt::staging_name,
                ),
            );
            Step::Working
        }
    }

    /// Why an operation stopped at a state its own record says cannot arise:
    /// going on would act on a guess.
    const LOST_TRACK: &str =
        "the file manager lost track of its progress and stopped rather than guess";

    /// Read the children of the directory at `path`: each child's leaf name,
    /// the browser's own classification of it and its size, through the same
    /// capability-checked listing call and shared decode the browser navigates
    /// with, so the count and the Trash's plans see what is a directory, a
    /// file or a link exactly as the browser would. Where a link points is not
    /// read: a link is the leaf it is.
    fn read_children(path: &[String]) -> Result<alloc::vec::Vec<(String, EntryKind, u64)>, Errno> {
        Ok(listed_children(path)?
            .into_iter()
            .map(|entry| (entry.name().to_string(), entry.kind(), entry.size()))
            .collect())
    }

    /// The children of `path` as the browser lists them, each naming the node
    /// it was listed as, so emptying the Trash removes a link as the leaf it is
    /// and leaves a name that has come to hold another node.
    fn listed_children(path: &[String]) -> Result<alloc::vec::Vec<Entry>, Errno> {
        let spelled = tairix_browse::vfs::absolute_path(path)?;
        let stream = tairix_rt::read_dir_all(spelled.as_bytes()).map_err(Errno::from_syscall)?;
        tairix_browse::vfs::entries_from_dir_stream(
            &spelled,
            &stream,
            &mut tairix_browse::vfs::NoLinks,
        )
    }

    /// State a `files:`-prefixed diagnosis on `stderr` — the one fail-loud
    /// reporting path a whole-operation refusal (a too-deep tree, an
    /// unspellable path, a rejected paste plan, an internal step error) states
    /// its reason through, shared by the delete and paste drives.
    fn report_error(reason: &str) {
        app::report(APP_NAME, reason);
    }

    /// One clipboard verb the keyboard invokes in navigation mode.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum ClipboardVerb {
        /// `Ctrl+X`: capture the selection onto a move clipboard.
        Cut,
        /// `Ctrl+C`: capture the selection onto a copy clipboard.
        Copy,
        /// `Ctrl+V`: paste the held clipboard into the current directory.
        Paste,
    }

    /// Classify a navigation-mode key press as a clipboard verb, or `None`.
    ///
    /// `Ctrl+X`/`Ctrl+C`/`Ctrl+V` are the verbs; `Shift` must not be held (so
    /// `Ctrl+Shift+N`'s New Folder is never shadowed). The keyboard delivers
    /// the letter itself even with `Ctrl` held (the `Ctrl+Shift+N` precedent),
    /// and either case is accepted.
    fn clipboard_verb(key: KeyValue, modifiers: AbiModifiers) -> Option<ClipboardVerb> {
        let KeyValue::Char(ch) = key else {
            return None;
        };
        if !modifiers.ctrl || modifiers.shift || modifiers.alt {
            return None;
        }
        if ch.eq_ignore_ascii_case(&'x') {
            Some(ClipboardVerb::Cut)
        } else if ch.eq_ignore_ascii_case(&'c') {
            Some(ClipboardVerb::Copy)
        } else if ch.eq_ignore_ascii_case(&'v') {
            Some(ClipboardVerb::Paste)
        } else {
            None
        }
    }

    /// Apply a clipboard `verb`.
    ///
    /// `Cut`/`Copy` capture the current selection onto `clipboard` (a no-op
    /// with nothing selected — the verb is simply unavailable, fail closed);
    /// neither changes the visible view, so both repaint nothing. `Paste`
    /// carries the held clipboard into the current directory (`run_paste`),
    /// which re-lists and repaints.
    fn apply_clipboard_verb<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        verb: ClipboardVerb,
    ) -> (bool, bool) {
        match verb {
            ClipboardVerb::Cut => {
                if let Some(clip) = browser.clipboard(ClipboardOp::Cut) {
                    overlays.clipboard = Some(clip);
                }
                (false, false)
            }
            ClipboardVerb::Copy => {
                if let Some(clip) = browser.clipboard(ClipboardOp::Copy) {
                    overlays.clipboard = Some(clip);
                }
                (false, false)
            }
            ClipboardVerb::Paste => run_paste(browser, overlays),
        }
    }

    /// Paste the held clipboard into the current directory ([`start_paste`]).
    /// A `Cut` is consumed by initiating the paste (its sources are being
    /// moved); a `Copy`, or a paste that could not begin, keeps it.
    fn run_paste<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
    ) -> (bool, bool) {
        let Some(clip) = overlays.clipboard.take() else {
            return (false, false);
        };
        let op = clip.op();
        let Some(folder) = browser.folder() else {
            overlays.clipboard = Some(clip);
            refuse_paste(overlays, op, NOTHING_PASTES_ON_THE_NETWORK);
            return (true, false);
        };
        match start_paste(&clip, folder, overlays) {
            Ok(()) if op == ClipboardOp::Cut => {}
            Ok(()) => overlays.clipboard = Some(clip),
            Err(reason) => {
                overlays.clipboard = Some(clip);
                refuse_paste(overlays, op, &reason);
            }
        }
        (true, false)
    }

    /// Why nothing is pasted while the network is shown.
    const NOTHING_PASTES_ON_THE_NETWORK: &str =
        "the network holds no files to paste among; open a folder or a mounted share";

    /// State why a paste or a drop moved nothing, in the window and on
    /// `stderr` for the record.
    fn refuse_paste(overlays: &mut Overlays, op: ClipboardOp, reason: &str) {
        report_error(reason);
        let title = match op {
            ClipboardOp::Copy => "Not copied",
            ClipboardOp::Cut => "Not moved",
        };
        overlays.notices.refuse(NoticeSource::Paste, title, reason);
    }

    /// Ask for `clip` to be pasted into the folder `target` as an
    /// [`Operation`] of `overlays`' window, under the user's own identity —
    /// every step is the ordinary permission-checked write the user could
    /// perform themselves — or say why it cannot begin.
    ///
    /// It does not begin while the window waits on a one-shot change, whose
    /// answer would otherwise act on the listing it is planned from, nor
    /// while an operation asked for just before it is still being started.
    /// Pasting a folder into itself is refused outright ([`plan_paste`]), and
    /// the shared engine refuses it again from the folders themselves. Each
    /// item is one of the engine's operations: a move is one `fs_rename` on its
    /// own volume, and across volumes a copy after which what it copied goes;
    /// a copy always streams. The first refusal stops the paste, is stated in
    /// the window, and leaves whatever already landed in place rather than a
    /// fabricated success.
    fn start_paste(
        clip: &Clipboard,
        target: &[String],
        overlays: &mut Overlays,
    ) -> Result<(), String> {
        if overlays.starting.is_some() {
            return Err(String::from(
                "the operation asked for just before it is still being started",
            ));
        }
        if overlays.writing.is_some() {
            return Err(String::from(STILL_WRITING));
        }
        let plan = plan_paste(clip, target).map_err(|err| err.to_string())?;
        let op = match plan.op() {
            ClipboardOp::Copy => ProgressOp::Copy,
            ClipboardOp::Cut => ProgressOp::Move,
        };
        let subject = subject_of(plan.items().iter().map(PasteItem::source));
        let destination = target.last().cloned();
        let paste = Paste::new(plan.op(), target.to_vec(), plan.items().to_vec());
        overlays.starting = Some(Operation::new(
            Job::paste(paste),
            ProgressModel::new(op, subject, destination),
        ));
        Ok(())
    }

    /// A paste in progress: each item of a captured plan carried out by an
    /// operation of the shared engine, one bounded unit at a time, so the event
    /// loop stays responsive.
    ///
    /// The plan chose every destination name free, so a name taken since
    /// refuses the item rather than being replaced. Every step is the user's
    /// own permission-checked call — no new capability, no ambient authority —
    /// so the read-only picker never builds a [`Paste`].
    struct Paste {
        /// Whether the plan moves (`Cut`) or copies (`Copy`) its items.
        op: ClipboardOp,
        /// The paste target directory.
        dest: alloc::vec::Vec<String>,
        /// The target directory's volume once the count has read it: what
        /// tells an item a move renames, carrying nothing, from one it copies.
        dest_vol: Option<VolumeId>,
        /// The resolved plan items, carried out in order.
        items: alloc::vec::Vec<PasteItem>,
        turns: Turns,
        /// Where the item in hand is landing, spelled, until it is carried out
        /// ([`Job::sync_hold`]).
        writing: Option<String>,
    }

    impl Paste {
        /// A paste of `items` (in order) into the folder `dest`, nothing begun
        /// yet.
        fn new(
            op: ClipboardOp,
            dest: alloc::vec::Vec<String>,
            items: alloc::vec::Vec<PasteItem>,
        ) -> Self {
            Self {
                op,
                dest,
                dest_vol: None,
                items,
                turns: Turns::default(),
                writing: None,
            }
        }

        /// Where the items land and how, the destination read on first ask.
        fn landing(&mut self) -> Result<Landing, Step> {
            if let Some(volume) = self.dest_vol {
                return Ok(Landing {
                    op: self.op,
                    volume,
                });
            }
            let Some(stat) = stat_node(&self.dest) else {
                return Err(Step::Stopped(item_refusal(
                    "paste into",
                    &self.dest,
                    "it could not be read",
                )));
            };
            let volume = VolumeId::new(stat.id.volume);
            self.dest_vol = Some(volume);
            Ok(Landing {
                op: self.op,
                volume,
            })
        }

        /// One unit of the paste, its refusal stated naming the item, and the
        /// item in hand named in `current`.
        ///
        /// A `Cut` back into the item's own directory is already done; a
        /// `Copy` onto itself is refused rather than duplicating a file onto
        /// itself.
        fn unit(&mut self, current: &mut String) -> Step {
            let verb = match self.op {
                ClipboardOp::Copy => "copy",
                ClipboardOp::Cut => "move",
            };
            if let Some(step) = self.turns.unit(verb) {
                if self.turns.in_hand.is_none() {
                    self.writing = None;
                }
                return step;
            }
            let Some(item) = self.items.get(self.turns.next) else {
                return Step::Done;
            };
            self.turns.next += 1;
            name_into(current, item.source());
            if item.overwrites_source() {
                return match self.op {
                    ClipboardOp::Cut => {
                        // Done, as the count of the whole counted it.
                        self.turns.finished.items = self.turns.finished.items.saturating_add(1);
                        Step::Working
                    }
                    ClipboardOp::Copy => Step::Stopped(item_refusal(
                        verb,
                        item.source(),
                        "an item cannot be copied onto itself",
                    )),
                };
            }
            let source = match named(item.source(), item.source_id()) {
                Ok(source) => source,
                Err(errno) => {
                    return Step::Stopped(item_refusal(verb, item.source(), reach_refusal(errno)))
                }
            };
            // The plan chose the name free: nothing was listed there.
            let target = match named(item.dest(), FileId::NONE) {
                Ok(target) => target,
                Err(errno) => {
                    return Step::Stopped(item_refusal(
                        verb,
                        item.source(),
                        destination_refusal(errno),
                    ))
                }
            };
            let carried = match self.op {
                ClipboardOp::Copy => Verb::Copy,
                ClipboardOp::Cut => Verb::Move,
            };
            self.writing = Some(target.path.clone());
            self.turns.take(
                item.source(),
                TreeOp::transfer(
                    carried,
                    source,
                    target,
                    Taken::Refuse,
                    tairix_rt::staging_name,
                ),
            );
            Step::Working
        }
    }

    /// Read a node's structural metadata (kind + size + volume id) through a
    /// resolve-only handle — opened with neither read nor write — so a path of
    /// unknown kind (file or directory) can be stat'd without guessing a flag,
    /// under the user's own identity. `None` when the path cannot be spelled or
    /// the node cannot be stat'd (the caller reports, fail closed).
    fn stat_node(path: &[String]) -> Option<tairix_abi::fs::FileStat> {
        let spelled = tairix_browse::vfs::absolute_path(path).ok()?;
        tairix_rt::File::open(spelled.as_bytes(), OpenFlags::empty())
            .and_then(|file| file.stat())
            .ok()
    }

    /// Stat the node `path` **names**, without following a final symbolic
    /// link — the POSIX `lstat` reading.
    ///
    /// This is the question every verb that acts on a *name* must ask: a link
    /// is what gets copied, moved, or trashed, so its own kind and its own
    /// volume are what decide how. [`stat_node`] answers the other question —
    /// what the path leads to — which is what a *destination folder* is.
    fn stat_name(path: &[String]) -> Option<tairix_abi::fs::FileStat> {
        let spelled = tairix_browse::vfs::absolute_path(path).ok()?;
        tairix_rt::File::open(spelled.as_bytes(), OpenFlags::NO_FOLLOW)
            .and_then(|file| file.stat())
            .ok()
    }

    /// Route one primary-button press at window-local `point` in navigation
    /// mode, reporting whether the view changed (and must re-present).
    ///
    /// The press is resolved in the order the layers overlap on screen, each
    /// action the spelling of one user intent, never an escalation:
    ///
    /// * a **manager write tool** (New Folder, Go to Trash, Empty Trash) is
    ///   dispatched here because the write path needs the overlay state; its
    ///   enable state gates the hit-test, so a click on a disabled Empty Trash
    ///   resolves to nothing (fail closed);
    /// * an **item** is activated on a quick second press on that same item —
    ///   descend / launch a bundle / open a file, through the very same
    ///   [`activate`] dispatch a keyboard `Enter` drives, so pointer and
    ///   keyboard can never diverge — and merely selected on a first or lone
    ///   press. Held shift lists a bundle instead of running it, the pointer
    ///   spelling of `Shift+Enter`. The monotonic [`tairix_rt::clock_get`]
    ///   needs no capability, and the pairing rule lives once in the shared
    ///   engine ([`DoubleClickTracker`]);
    /// * anything else lands on the read-only **chrome** and routes through
    ///   [`apply_chrome_press`].
    ///
    /// A tool or chrome press is not an item, so it resets the double-click
    /// tracker: a click *through* the chrome and back onto the same item is
    /// never mistaken for a double-click of that item.
    ///
    /// `viewport` is the rail-inset content area the items occupy; the write
    /// tools sit on the toolbar band, which spans the whole window
    /// (`canvas.window()`).
    ///
    /// `press` is where it landed, the modifiers held, whether the window
    /// held the keyboard before it, and when its device reported it.
    fn apply_primary_press<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        viewport: Rect,
        (point, modifiers, keyboard_held, at_ns): (Point, AbiModifiers, bool, u64),
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let toolbar = canvas.chrome.toolbar;
        let theme = canvas.theme();
        let scale = canvas.scale;
        if let Some(tool) = manager_tool_at(
            browser,
            scale,
            theme,
            canvas.window(),
            toolbar,
            point,
            MANAGER_TOOLS,
            manager_tool_model(browser),
        ) {
            overlays.double_click.reset();
            return whole(apply_manager_tool(browser, overlays, tool));
        }
        if let Some(order) =
            tairix_browse::render::header_sort_at(browser, scale, theme, viewport, toolbar, point)
        {
            overlays.double_click.reset();
            return whole(reorder(browser, order, (scale, theme, viewport, toolbar)));
        }
        let frame = Frame {
            scale,
            theme,
            viewport,
            toolbar,
        };
        let hit =
            tairix_browse::render::entry_index_at(browser, scale, theme, viewport, toolbar, point);
        let place = match hit {
            Some(index) => PressHit::Item(index),
            None if tairix_browse::render::listing_area(
                browser, scale, theme, viewport, toolbar,
            )
            .contains(point) =>
            {
                PressHit::Empty
            }
            None => PressHit::Chrome,
        };
        let keys = SelectKeys {
            toggle: modifiers.ctrl,
            extend: modifiers.shift,
        };
        let in_selection =
            hit.is_some_and(|index| browser.is_selected(index) && browser.selection().len() > 1);
        overlays.collapse_on_release = None;
        overlays.sweep = None;
        match gesture::primary_press(
            &mut overlays.double_click,
            at_ns,
            place,
            keys,
            in_selection,
            acts.double_click,
        ) {
            // The pair's first press may have selected more than the item, so
            // the marks are reported from before it is chosen alone.
            PrimaryPress::Activate {
                index,
                intent,
                deselect,
            } => {
                let mark = ViewMark::of(browser, frame);
                let _ = browser.select(index);
                if activate(browser, overlays, acts, frame, intent) {
                    return (Repaint::Whole, false);
                }
                if deselect {
                    browser.clear_selection();
                }
                (
                    Repaint::reported_if(mark.report(browser, frame, damage)),
                    false,
                )
            }
            PrimaryPress::Select { index, how } => select_pressed(
                browser,
                overlays,
                frame,
                (index, how),
                (point, keyboard_held),
                damage,
            ),
            PrimaryPress::Empty { keep } => {
                let mark = ViewMark::of(browser, frame);
                if !keep {
                    browser.clear_selection();
                }
                overlays.drag = None;
                overlays.sweep = Some(Sweep::Armed(DragArm { at: point }));
                (
                    Repaint::reported_if(mark.report(browser, frame, damage)),
                    false,
                )
            }
            PrimaryPress::Chrome => whole(apply_chrome_press(browser, canvas, viewport, point)),
        }
    }

    /// Apply a lone press that selected the entry at `index` `how`: the
    /// selection changes, a drag may begin from it, and a click on the name of
    /// the entry already chosen arms its rename.
    ///
    /// A selection change repaints only the shown entries whose mark changed,
    /// so the round reports exactly those.
    fn select_pressed<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        frame: Frame<'_>,
        (index, how): (usize, SelectHow),
        (point, keyboard_held): (Point, bool),
        damage: &mut Region,
    ) -> (Repaint, bool) {
        overlays.rename_arm = RenameArm::on_press(
            index,
            point,
            NamePress {
                how,
                was_chosen: browser.chosen_index() == Some(index),
                on_name: tairix_browse::render::entry_name_target(
                    browser,
                    frame.scale,
                    frame.theme,
                    frame.viewport,
                    frame.toolbar,
                    index,
                )
                .is_some_and(|name| name.contains(point)),
                keyboard_held,
            },
        );
        let mark = ViewMark::of(browser, frame);
        let applied = match how {
            SelectHow::Single => browser.select(index),
            SelectHow::Toggle => browser.toggle_selection(index),
            SelectHow::Extend => browser.extend_selection_to(index),
            SelectHow::Hold => {
                overlays.collapse_on_release = Some(index);
                Ok(())
            }
        };
        let moved = mark.report(browser, frame, damage);
        overlays.drag = browser.is_selected(index).then_some(DragArm { at: point });
        (Repaint::reported_if(applied.is_ok() && moved), false)
    }

    /// Apply one primary press that landed on the read-only **chrome** (the
    /// toolbar), reporting whether the view changed.
    ///
    /// The caller ([`apply_primary_press`]) resolves manager write tools and
    /// item clicks first, so by the time a press reaches here it is neither. A
    /// click on a toolbar command runs it through the same shared dispatch the
    /// keyboard accelerators use; a click on empty space resolves to nothing
    /// and repaints nothing.
    ///
    /// The toolbar band spans the whole window (`canvas.window()`), so it is
    /// hit-tested against that; `viewport` is the rail-inset content area the
    /// command it runs acts within.
    fn apply_chrome_press<S: DirectorySource>(
        browser: &mut Browser<S>,
        canvas: Canvas<'_>,
        viewport: Rect,
        point: Point,
    ) -> (bool, bool) {
        let toolbar = canvas.chrome.toolbar;
        let theme = canvas.theme();
        let scale = canvas.scale;
        // A click on a toolbar command runs it through the same shared dispatch
        // the keyboard accelerators use; a disabled command resolves to nothing
        // (`toolbar_command_at` fails closed) and repaints nothing.
        if let Some(command) = tairix_browse::render::toolbar_command_at(
            browser,
            scale,
            theme,
            canvas.window(),
            toolbar,
            point,
        ) {
            return apply_toolbar_command(browser, scale, theme, viewport, toolbar, command);
        }
        (false, false)
    }

    /// The window-local [`Point`] of a **secondary**-button (right-click)
    /// press, or `None` for any other pointer action — the mirror of
    /// [`press_point`] for the button that opens the right-click context menu.
    fn secondary_press_point(action: PointerAction, x: u32, y: u32) -> Option<Point> {
        if action != PointerAction::Pressed(PointerButtonCode::Secondary) {
            return None;
        }
        Some(pointer_point(x, y))
    }

    /// Ask the desktop to open this window's context menu at window-local
    /// `point`, on the item `index` the caller's hit-test resolved (`None` for
    /// empty space or the chrome).
    ///
    /// An item outside the selection is selected first so the menu's commands
    /// act on what was clicked, while one inside it keeps the whole selection;
    /// a right-click on nothing clears the selection so the menu offers only
    /// the directory-scoped verbs. The rows are the shared
    /// [`context_menu`] declaration over the [`ContextMenuModel`] with this
    /// app's own held-clipboard state, so an inapplicable command is declared
    /// disabled with its reason rather than left out.
    ///
    /// The anchor is the client-local point the press was reported at, which is
    /// the only space this window can speak truthfully: it is never told where
    /// it sits on screen. The desktop titles, places, draws, grabs and
    /// dismisses; the answer arrives later as one `MenuClosed` naming the id
    /// minted here. A refusal is an answer — it is reported and the window
    /// carries on with no menu, never drawing one of its own.
    ///
    /// The press also breaks any half-finished primary pair, so a click either
    /// side of a right-click cannot read as a double-click.
    fn open_context_menu<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        menu: &mut MenuLink<'_>,
        reads: &Reads,
        installed: &Installed,
        point: Point,
        index: Option<usize>,
    ) -> (bool, bool) {
        overlays.double_click.reset();
        overlays.collapse_on_release = None;
        overlays.drag = None;
        overlays.sweep = None;
        match index {
            // A right-click inside the selection keeps it, so the menu's set
            // verbs act on everything the user had chosen.
            Some(index) if browser.is_selected(index) => {}
            Some(index) => {
                let _ = browser.select(index);
            }
            None => browser.clear_selection(),
        }
        // Keep the scan warm rather than reading here: the submenu is built
        // from the answer that has already landed, so a right-click performs
        // no I/O at all, and asking again now is what has the next one
        // current.
        adopt_bundles(reads, installed);
        let model = ContextMenuModel::for_browser(browser, overlays.clipboard.is_some());
        let target = open_with_target(browser);
        let held = installed.borrow();
        let ranked = target
            .as_ref()
            .map(|file| applications_for(&file.name, held.as_deref().unwrap_or_default()))
            .unwrap_or_default();
        let quick = quick_applications(&ranked);
        let documents = blank_documents(held.as_deref().unwrap_or_default());
        let built = if browser.is_network() {
            network_context_menu(browser, MANAGER_MENU_TITLE)
        } else {
            context_menu(
                model,
                MANAGER_MENU_TITLE,
                ContextQuick {
                    chosen: browser.chosen_entry(),
                    candidates: &quick,
                    documents: &documents,
                },
            )
        };
        let rows = match built {
            Ok(rows) => rows,
            Err(err) => {
                report_error(&alloc::format!("menu model refused ({err}); not shown"));
                return (true, false);
            }
        };
        let candidates: Vec<OpenWithCandidate> =
            quick.iter().map(|app| OpenWithCandidate::of(app)).collect();
        drop(held);
        let anchor = match WindowRegion::new(point.x, point.y, 0, 0) {
            Ok(anchor) => anchor,
            Err(err) => {
                report_error(&alloc::format!("menu anchor refused ({err}); not shown"));
                return (true, false);
            }
        };
        match menu.client.open_menu(menu.window, anchor, &rows) {
            Ok(open_id) => {
                *menu.open = Some(OpenMenuState {
                    open_id,
                    target,
                    candidates,
                    documents,
                    endings: browser.presentation().endings,
                });
            }
            Err(err) => report_error(&alloc::format!("menu refused ({err}); not shown")),
        }
        // The selection moved whether or not a chain came up, so the listing is
        // repainted either way.
        (true, false)
    }

    /// The installed applications' declared associations, once a bundle scan
    /// has answered: `None` until one has, so a scan that found nothing is an
    /// answer rather than a reason to walk the stores again.
    type Installed = RefCell<Option<Vec<AppAssociation>>>;

    /// Take whatever the bundle scan has answered into `installed`, and ask for
    /// it again so the next read is current.
    ///
    /// The scan walks three program stores and reads one manifest per installed
    /// application, which is far more than a frame's worth of work, so it runs
    /// on the reader and every consumer reads the answer that has already
    /// landed. A machine that granted no reader answers here, on this thread,
    /// exactly as it always did.
    fn adopt_bundles(reads: &Reads, installed: &Installed) {
        if let Some(found) = reads.take_bundles() {
            *installed.borrow_mut() = Some(found);
        }
        if let Some(found) = reads.want_bundles() {
            *installed.borrow_mut() = Some(found);
        }
    }

    /// The selected entry as an "Open With…" target — its absolute path and
    /// leaf name — or `None` when the selection is not a regular file, or
    /// cannot be named.
    ///
    /// A directory descends and a bundle launches itself, so neither has an
    /// application to choose; a selection that cannot be spelled is no target
    /// rather than a fabricated one (fail closed).
    fn open_with_target<S: DirectorySource>(browser: &Browser<S>) -> Option<PendingChooser> {
        let entry = browser.chosen_entry()?;
        if entry.kind().resolved() != Some(EntryKind::File) {
            return None;
        }
        let name = entry.name().to_string();
        let Some(Ok(path)) = browser.chosen_target_path() else {
            return None;
        };
        Some(PendingChooser { path, name })
    }

    /// Act on the one outcome the desktop owes this window's open.
    ///
    /// A chosen row runs its command's verb; a dismissal does nothing; a
    /// refusal is stated on `stderr` and the window carries on. A row id this
    /// window never declared names no command and is dropped (fail closed —
    /// an outcome is never guessed at).
    #[allow(clippy::too_many_arguments)] // The window's state, its geometry, the answer, and the round's report.
    fn apply_menu_outcome<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        acts: &mut Acts<'_>,
        gesture: &OpenMenuState,
        canvas: Canvas<'_>,
        viewport: Rect,
        outcome: MenuOutcome,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let scale = canvas.scale;
        let theme = canvas.theme();
        let toolbar = canvas.chrome.toolbar;
        let window = acts.menu.window;
        let (changed, close) = match outcome {
            MenuOutcome::Chosen(item) => match context_choice_from_item(item) {
                Some(ContextChoice::Command(command)) => {
                    return dispatch_context_command(
                        browser, overlays, acts, canvas, viewport, command, damage,
                    );
                }
                Some(ContextChoice::OpenWithCandidate(index)) => {
                    launch_candidate(gesture, index, acts.launcher, acts.menu.client)
                }
                Some(ContextChoice::NewFolder) => {
                    begin_new_entry(browser, overlays, NewEntry::Folder)
                }
                Some(ContextChoice::Display(mode)) => {
                    browser.set_view_mode(mode);
                    tairix_browse::render::reveal_selection(
                        browser, scale, theme, viewport, toolbar,
                    );
                    (true, false)
                }
                Some(ContextChoice::SortBy(key)) => {
                    let order = SortMode {
                        key,
                        ..browser.sort_mode()
                    };
                    reorder(browser, order, (scale, theme, viewport, toolbar))
                }
                Some(ContextChoice::SortIn(direction)) => {
                    let order = SortMode {
                        direction,
                        ..browser.sort_mode()
                    };
                    reorder(browser, order, (scale, theme, viewport, toolbar))
                }
                // A document the menu never offered makes nothing.
                Some(ContextChoice::NewDocument(index)) => match gesture.documents.get(index) {
                    Some(document) => {
                        begin_new_entry(browser, overlays, NewEntry::Document(*document))
                    }
                    None => (false, false),
                },
                // A commit answers `Entered`, so a *chosen* row that reads
                // back as one is an answer this menu never declared that way
                // (fail closed — an outcome is never guessed at).
                Some(ContextChoice::RenameCommit) | None => (false, false),
            },
            // The field's text is wider than the event frame, so the answer
            // names the field and the text is pulled. It renames through the
            // very path `F2` drives, so a name typed in the menu and one typed
            // in place cannot come to mean different things.
            MenuOutcome::Entered(item) => match context_choice_from_item(item) {
                Some(ContextChoice::RenameCommit) => {
                    let frame = Frame {
                        scale,
                        theme,
                        viewport,
                        toolbar,
                    };
                    let repaint = commit_menu_rename(
                        browser,
                        &mut overlays.rename,
                        acts.reads,
                        (acts.menu.client, window, gesture),
                        frame,
                        damage,
                    );
                    return (repaint, false);
                }
                _ => (false, false),
            },
            MenuOutcome::Dismissed => (false, false),
            MenuOutcome::Refused(reason) => {
                report_error(&alloc::format!("no menu was shown: {}", reason.describe()));
                (false, false)
            }
        };
        (whole_if(changed), close)
    }

    /// Order `browser`'s listing by `order`, keeping the selection in view.
    fn reorder<S: DirectorySource>(
        browser: &mut Browser<S>,
        order: SortMode,
        (scale, theme, viewport, toolbar): (Scale, &Theme, Rect, ToolbarBand),
    ) -> (bool, bool) {
        browser.set_sort_mode(order);
        tairix_browse::render::reveal_selection(browser, scale, theme, viewport, toolbar);
        (true, false)
    }

    /// Pull the name the user typed into the context menu's Rename field and
    /// rename the selection to it.
    ///
    /// The text is pulled rather than delivered because an event is one fixed
    /// frame and a name is wider than that; the session holds exactly one per
    /// window and hands it over once. Nothing held is an honest answer that
    /// renames nothing — a stale pull, or a commit the session could not keep
    /// — and the rename is prepared and handed on exactly as `F2`'s
    /// ([`commit_rename`]): the same permission-checked `fs_rename` under the
    /// user's own identity, off the loop, so a refusal is stated and the
    /// listing is untouched.
    fn commit_menu_rename<S: DirectorySource>(
        browser: &mut Browser<S>,
        edit: &mut RenameEdit,
        reads: &Reads,
        (client, window, gesture): (
            &mut WindowClient<app::RtWindowTransport>,
            u64,
            &OpenMenuState,
        ),
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> Repaint {
        let name = match client.take_menu_text(window, gesture.open_id) {
            Ok(Some(name)) => name,
            Ok(None) => return Repaint::Nothing,
            Err(err) => {
                report_error(&alloc::format!("typed name refused ({err})"));
                return Repaint::Nothing;
            }
        };
        if edit.in_flight() {
            edit.refuse(String::from("a rename is already being carried out"));
            return Repaint::Nothing;
        }
        let pending = match browser.prepare_rename(&name, gesture.endings) {
            Ok(pending) => pending,
            Err(RenameError::Unchanged) => return close_editor(browser, edit, frame, damage),
            Err(err) => {
                edit.refuse(rename_refusal(err));
                return Repaint::Nothing;
            }
        };
        submit_rename(browser, edit, reads, pending, frame, damage)
    }

    /// Hand the file the menu was opened on to the candidate application the
    /// submenu's chosen row names.
    ///
    /// The same [`Launcher::launch_viewer`] hand-off the chooser and the
    /// default open use: the file is opened read-only in this app's own table
    /// and wired onto the child's `STDIN`, so the application reads it holding
    /// no filesystem capability of its own. An index or a target the gesture
    /// does not carry opens nothing (fail closed).
    fn launch_candidate(
        gesture: &OpenMenuState,
        index: usize,
        launcher: &RefCell<Launcher>,
        client: &mut WindowClient<app::RtWindowTransport>,
    ) -> (bool, bool) {
        let Some(candidate) = gesture.candidates.get(index) else {
            return (false, false);
        };
        let Some(target) = gesture.target.as_ref() else {
            return (false, false);
        };
        launcher.borrow_mut().launch_viewer(
            client,
            candidate.bundle_path(),
            candidate.writes_documents(),
            &target.path,
            &target.name,
        );
        (false, false)
    }

    /// Run the verb a chosen [`ContextCommand`] names, over the exact same app
    /// paths the toolbar and keyboard drive, so the right-click menu can never
    /// diverge from them. Every verb is the user's own permission- checked
    /// action under their identity — the menu adds no authority.
    #[allow(clippy::too_many_arguments)] // The window's state, its geometry, the command, and the round's report.
    fn dispatch_context_command<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        viewport: Rect,
        command: ContextCommand,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let scale = canvas.scale;
        let theme = canvas.theme();
        let toolbar = canvas.chrome.toolbar;
        let frame = Frame {
            scale,
            theme,
            viewport,
            toolbar,
        };
        let (changed, close) = match command {
            ContextCommand::Open => (
                activate(browser, overlays, acts, frame, BundleIntent::Launch),
                false,
            ),
            ContextCommand::OpenWith => begin_open_with(browser, overlays, acts, canvas),
            ContextCommand::Rename => begin_rename(
                browser,
                &mut overlays.rename,
                scale,
                theme,
                viewport,
                toolbar,
            ),
            ContextCommand::Cut => apply_clipboard_verb(browser, overlays, ClipboardVerb::Cut),
            ContextCommand::Copy => apply_clipboard_verb(browser, overlays, ClipboardVerb::Copy),
            ContextCommand::Paste => apply_clipboard_verb(browser, overlays, ClipboardVerb::Paste),
            ContextCommand::SelectAll => {
                return apply_selection_verb(browser, frame, SelectionVerb::All, damage);
            }
            ContextCommand::ClearSelection => {
                return apply_selection_verb(browser, frame, SelectionVerb::Clear, damage);
            }
            ContextCommand::GetInfo => ask_properties(browser, acts.properties),
            // The same modal-confirmed removal the `Delete` key opens: the menu
            // adds no authority — the confirmed walk is the user's own
            // permission-checked `fs_unlink`s.
            ContextCommand::Delete => begin_delete(browser, overlays),
        };
        (whole_if(changed), close)
    }

    /// Open the "Open With…" application chooser for the selected regular
    /// file, in its own popup window above this one.
    ///
    /// The chooser is offered only for a regular file — a directory descends
    /// and a bundle launches itself, so neither has an application to pick (the
    /// context-menu model already disables the command otherwise; this guards
    /// it again, fail closed). The candidates are the installed bundles whose
    /// declared associations claim the file's type ([`applications_for`] over
    /// the scan the reader keeps warm, keyed off the leaf name, never a
    /// hard-coded viewer), so opening it reads nothing here.
    ///
    /// It is a **popup window**, not an overlay drawn inside the listing: the
    /// panel is sized to its own candidates, and a surface sized to its content
    /// cannot be one that borrows another window's extent. When no installed
    /// application claims the file the refusal is stated fail-loud on `stderr`
    /// and nothing is opened — an honest answer, never an empty chooser or a
    /// fabricated open.
    fn begin_open_with<S: DirectorySource>(
        browser: &Browser<S>,
        overlays: &mut Overlays,
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
    ) -> (bool, bool) {
        let Some(target) = open_with_target(browser) else {
            return (false, false);
        };
        adopt_bundles(acts.reads, acts.installed);
        let held = acts.installed.borrow();
        let apps = applications_for(&target.name, held.as_deref().unwrap_or_default());
        let chooser = OpenWithChooser::new(&apps, &target.path, &target.name);
        drop(held);
        let Some(chooser) = chooser else {
            report_error(&alloc::format!("no application to open {}", target.name));
            return (false, false);
        };
        let opened = open_chooser_pane(acts, canvas, chooser);
        if let Some(overlay) = opened {
            overlays.set_chooser(acts.menu.client, Some(overlay));
        }
        (false, false)
    }

    /// Open `chooser`'s own popup above the window `canvas` describes, drawn
    /// once.
    ///
    /// The popup is exactly the size the chooser wants — its panel sized to the
    /// candidates it holds, measured against the *screen* rather than the
    /// parent window, so a long list is not shrunk by a narrow file-manager
    /// window — centred over the parent's client. A window smaller than the
    /// popup therefore yields a negative offset, which is a legitimate request:
    /// the session resolves it against the parent's screen position and clamps
    /// the whole popup on screen.
    ///
    /// `None` — with the reason already on `stderr` — opens nothing: a refusal
    /// shows no chooser and the window carries on.
    fn open_chooser_pane(
        acts: &mut Acts<'_>,
        canvas: Canvas<'_>,
        chooser: OpenWithChooser,
    ) -> Option<ChooserOverlay> {
        let screen = acts.popup.screen;
        let (w, h) = tairix_browse::render::open_with_chooser_extent(
            &chooser,
            canvas.scale,
            canvas.theme(),
            screen,
        );
        let extent = (w.min(screen.width.max(1)), h.min(screen.height.max(1)));
        if extent.0 == 0 || extent.1 == 0 {
            report_error("the chooser has no drawable extent; not shown");
            return None;
        }
        let parent = canvas.window();
        let offset = (
            centre_offset(parent.width, extent.0),
            centre_offset(parent.height, extent.1),
        );
        let mode = app::mode_for(extent.0, extent.1);
        let Some(surface) = Surface::new(extent.0, extent.1) else {
            report_error("the chooser surface was refused; not shown");
            return None;
        };
        let Some(server) = acts.popup.server else {
            report_error("the serving session is unknown; the chooser was not shown");
            return None;
        };
        let pane = match WindowPane::open_popup(
            acts.menu.client,
            acts.menu.window,
            server,
            acts.popup.event_endpoint,
            &mode,
            offset,
        ) {
            Ok(pane) => pane,
            Err(err) => {
                report_error(&alloc::format!("{err}; the chooser was not shown"));
                return None;
            }
        };
        let mut overlay = ChooserOverlay {
            chooser,
            clicks: DoubleClickTracker::new(),
            pane,
            surface,
            owed: false,
        };
        if present_chooser(&mut overlay, acts.menu.client, canvas, acts.icons).is_err() {
            report_error("the chooser present was refused; not shown");
            let _ = overlay.pane.close(acts.menu.client);
            return None;
        }
        Some(overlay)
    }

    /// The offset that centres an `inner` extent within an `outer` one,
    /// negative when the inner extent is the larger of the two.
    fn centre_offset(outer: u32, inner: u32) -> i32 {
        // Display extents halved stay far inside `i32`; a mode that says
        // otherwise centres at the origin rather than wrapping.
        i32::try_from((i64::from(outer) - i64::from(inner)) / 2).unwrap_or(0)
    }

    /// Paint the chooser popup whole and present it.
    ///
    /// The panel fills the popup, so there is nothing of the surface a partial
    /// present could leave standing: every round draws all of it.
    fn present_chooser(
        overlay: &mut ChooserOverlay,
        client: &mut WindowClient<app::RtWindowTransport>,
        canvas: Canvas<'_>,
        icons: &RefCell<IconPipeline>,
    ) -> Result<(), Errno> {
        let mode = *overlay.pane.mode();
        let viewport = Rect::new(0, 0, mode.width_px, mode.height_px);
        let surface = &mut overlay.surface;
        {
            let mut pipeline = icons.borrow_mut();
            draw_open_with_chooser(
                surface,
                &overlay.chooser,
                canvas.scale,
                canvas.theme(),
                viewport,
                &mut pipeline.source(),
            );
        }
        overlay
            .pane
            .present(client, surface, DamageRect::full(&mode))
    }

    /// Handle one event delivered to the "Open With…" chooser's own popup.
    ///
    /// `Escape` dismisses it, as does the Cancel action and the window manager
    /// asking the popup to close. Up/Down/Home/End move the current candidate
    /// and scroll the least that keeps it in view; `Enter`, the Open action, or
    /// a primary press on a candidate row hands the file over. The scroll
    /// gutter owns a press that lands on it, and a press on nothing resolves to
    /// nothing — a chooser in its own window is not dismissed by a click inside
    /// itself.
    ///
    /// The hand-off is the same [`Launcher::launch_viewer`] the default open
    /// uses: the file opened read-only in the manager's own table and wired
    /// onto the child's `STDIN`, so the application reads it with no filesystem
    /// capability of its own.
    fn apply_chooser_event(
        overlays: &mut Overlays,
        launcher: &RefCell<Launcher>,
        client: &mut WindowClient<app::RtWindowTransport>,
        canvas: Canvas<'_>,
        double_click: Duration64,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> bool {
        let scale = canvas.scale;
        let theme = canvas.theme();
        let Some(overlay) = overlays.open_with.as_mut() else {
            return false;
        };
        let mode = *overlay.pane.mode();
        let viewport = Rect::new(0, 0, mode.width_px, mode.height_px);
        let chooser = &mut overlay.chooser;
        let reveal = |chooser: &mut OpenWithChooser, moved: bool| {
            open_with_reveal(chooser, scale, theme, viewport) || moved
        };
        match event {
            WindowEvent::CloseRequested { .. } => {
                overlays.set_chooser(client, None);
                false
            }
            WindowEvent::ContentReleased { .. } => {
                overlay.pane.release_frames();
                false
            }
            WindowEvent::RedrawRequested { .. } => true,
            WindowEvent::Key {
                key: KeyInput::Pressed { key, .. },
                ..
            } => match key {
                KeyValue::Named(NamedKeyCode::Escape) => {
                    overlays.set_chooser(client, None);
                    false
                }
                KeyValue::Named(NamedKeyCode::Enter) => {
                    launch_open_with(overlays, launcher, client);
                    false
                }
                KeyValue::Named(NamedKeyCode::Up) => {
                    let moved = chooser.step(-1);
                    reveal(chooser, moved)
                }
                KeyValue::Named(NamedKeyCode::Down) => {
                    let moved = chooser.step(1);
                    reveal(chooser, moved)
                }
                KeyValue::Named(NamedKeyCode::Home) => {
                    let moved = chooser.select(0);
                    reveal(chooser, moved)
                }
                KeyValue::Named(NamedKeyCode::End) => {
                    let moved = chooser.select(usize::MAX);
                    reveal(chooser, moved)
                }
                _ => false,
            },
            WindowEvent::Scrolled { dx, dy, .. } => {
                open_with_scroll_wheel(chooser, scale, theme, viewport, (*dx, *dy), damage)
            }
            WindowEvent::Pointer {
                x,
                y,
                action,
                at_ns,
                ..
            } => apply_chooser_pointer(
                overlays,
                launcher,
                client,
                canvas,
                double_click,
                (*x, *y, *action, *at_ns),
                damage,
            ),
            _ => false,
        }
    }

    /// Route one pointer event over the chooser's popup.
    ///
    /// The scroll gutter owns a press that lands on it, so dragging the thumb
    /// scrolls the list instead of resolving to a row. A press then resolves
    /// against the action band, then the candidate rows, and a press on the
    /// panel's own plate resolves to nothing.
    fn apply_chooser_pointer(
        overlays: &mut Overlays,
        launcher: &RefCell<Launcher>,
        client: &mut WindowClient<app::RtWindowTransport>,
        canvas: Canvas<'_>,
        double_click: Duration64,
        pointer: (u32, u32, PointerAction, u64),
        damage: &mut Region,
    ) -> bool {
        let (x, y, action, at_ns) = pointer;
        let scale = canvas.scale;
        let theme = canvas.theme();
        let Some(overlay) = overlays.open_with.as_mut() else {
            return false;
        };
        let mode = *overlay.pane.mode();
        let viewport = Rect::new(0, 0, mode.width_px, mode.height_px);
        let point = pointer_point(x, y);
        let mut scrolled = None;
        for input in pointer_input_events(action, point) {
            if let Some(repaint) = open_with_scroll_pointer(
                &mut overlay.chooser,
                scale,
                theme,
                viewport,
                point,
                &input,
                damage,
            ) {
                scrolled = Some(scrolled.unwrap_or(false) || repaint);
            }
        }
        if let Some(repaint) = scrolled {
            return repaint;
        }
        let Some(point) = press_point(action, x, y) else {
            return false;
        };
        if let Some(action) = open_with_action_at(&overlay.chooser, viewport, scale, theme, point) {
            match action {
                OpenWithAction::Open => launch_open_with(overlays, launcher, client),
                OpenWithAction::Cancel => overlays.set_chooser(client, None),
            }
            return false;
        }
        // A press on the panel's own plate or identity band picks nothing and
        // launches nothing; the chooser is left standing, and the run of
        // presses is broken so a click through it and back is never read as a
        // double.
        let Some(index) = open_with_row_at(&overlay.chooser, viewport, scale, theme, point) else {
            overlay.clicks.reset();
            return false;
        };
        let moved = overlay.chooser.select(index);
        // A single press picks; opening is a second, deliberate act — a
        // double-click, Enter, or the Open button. A press that launched at
        // once left the Open button with nothing to do and spawned an
        // application on a mis-click, with no chance to look at the choice.
        // The pairing is the same shared tracker the listing behind it uses,
        // so a double-click means one thing.
        let subject = u64::try_from(index).unwrap_or(u64::MAX);
        if overlay
            .clicks
            .register(at_ns, subject, PointerButton::Primary, double_click)
            == ClickKind::Double
        {
            launch_open_with(overlays, launcher, client);
            return false;
        }
        moved
    }

    /// Hand the chooser's file to its current candidate and close the chooser.
    ///
    /// The chooser is taken out first, so the launch runs from owned state and
    /// a second activation cannot reach a chooser that is already gone.
    fn launch_open_with(
        overlays: &mut Overlays,
        launcher: &RefCell<Launcher>,
        client: &mut WindowClient<app::RtWindowTransport>,
    ) {
        let Some(overlay) = overlays.open_with.take() else {
            return;
        };
        let _ = overlay.pane.close(client);
        if let Some(candidate) = overlay.chooser.chosen() {
            launcher.borrow_mut().launch_viewer(
                client,
                candidate.bundle_path(),
                candidate.writes_documents(),
                overlay.chooser.file_path(),
                overlay.chooser.display_name(),
            );
        }
    }

    /// Run a toolbar `command` against the browser through the one shared
    /// [`tairix_browse::apply_command`] dispatch (so a toolbar click and its
    /// keyboard accelerator can never diverge), revealing the selection and
    /// reporting a repaint when the view changed. A navigation refused by the
    /// VFS leaves the browser exactly where it was (fail closed) and repaints
    /// nothing.
    fn apply_toolbar_command<S: DirectorySource>(
        browser: &mut Browser<S>,
        scale: Scale,
        theme: &Theme,
        viewport: Rect,
        toolbar: ToolbarBand,
        command: ToolbarCommand,
    ) -> (bool, bool) {
        match tairix_browse::apply_command(browser, command) {
            Ok(true) => {
                tairix_browse::render::reveal_selection(browser, scale, theme, viewport, toolbar);
                (true, false)
            }
            Ok(false) | Err(_) => (false, false),
        }
    }

    /// Dispatch a manager write `tool` (a toolbar click). New Folder creates
    /// and inline-renames a folder; the Trash tool navigates to the user's
    /// Trash location; Empty Trash opens the permanent-removal confirmation
    /// (`plans/NEW-FILEMANAGER.md` `FM11`).
    fn apply_manager_tool<S: DirectorySource>(
        browser: &mut Browser<S>,
        overlays: &mut Overlays,
        tool: ManagerTool,
    ) -> (bool, bool) {
        match tool {
            ManagerTool::NewFolder => begin_new_entry(browser, overlays, NewEntry::Folder),
            ManagerTool::Trash => go_to_trash(overlays),
            ManagerTool::EmptyTrash => begin_empty_trash(browser, overlays),
        }
    }

    /// The manager write-tools' enable state for `browser`: the Empty Trash
    /// verb ([`ManagerTool::EmptyTrash`]) is offered only when the current
    /// directory *is* the user's Trash and it is non-empty. Computed here
    /// rather than in the shared engine because it depends on the user's
    /// `HOME`, which the engine does not know (`plans/NEW-FILEMANAGER.md`
    /// `FM11`).
    fn manager_tool_model<S: DirectorySource>(browser: &Browser<S>) -> ManagerToolModel {
        ManagerToolModel::for_browser(browser, current_is_populated_trash(browser))
    }

    /// Whether the current directory is the user's Trash *and* it holds at
    /// least one item — the one gate on offering the Empty Trash verb. Fail
    /// closed: an absent/root `HOME`, or a current directory that is not the
    /// `Library/Trash` subtree, is not the Trash, so the verb is not offered.
    fn current_is_populated_trash<S: DirectorySource>(browser: &Browser<S>) -> bool {
        if browser.entries().is_empty() {
            return false;
        }
        let Some(home) = home_components() else {
            return false;
        };
        if home.is_empty() {
            return false;
        }
        browser.folder() == Some(trash_dir(&home).as_slice())
    }

    /// Show the user's Trash (`plans/NEW-FILEMANAGER.md` `FM11`): the reader
    /// makes sure the fixed `Library/Trash` subtree exists, and the window
    /// goes there once it has. An absent home is stated in the window.
    fn go_to_trash(overlays: &mut Overlays) -> (bool, bool) {
        let repeats = |then: &BrowserWrite| matches!(then, BrowserWrite::Trash(_));
        if let Some(changed) = overlays.while_writing(NO_TRASH, repeats) {
            return (changed, false);
        }
        let Some(home) = home_components().filter(|home| !home.is_empty()) else {
            let reason = "there is no home folder, so there is no Trash";
            overlays
                .notices
                .refuse(NoticeSource::Write, NO_TRASH, reason);
            return (true, false);
        };
        let trash = trash_dir(&home);
        overlays.writing = Some(Writing::new(
            WriteAct::EnsureTrash(trash.clone()),
            BrowserWrite::Trash(trash),
        ));
        (false, false)
    }

    /// Ask to **empty** the user's Trash — permanently remove its contents
    /// (`plans/NEW-FILEMANAGER.md` `FM11`).
    ///
    /// The Trash location is recomputed, so a stale click can never empty
    /// another directory (fail closed). The reader reads what the Trash holds
    /// now, which the listing on show may lag behind, and the confirmation it
    /// opens names exactly that; an empty Trash opens none. Emptying is always
    /// permanent (there is no trash-of-the-trash), so it is confirmed with the
    /// [`DeleteDisposition::Permanent`] wording and carried out as an ordinary
    /// permanent delete, under the user's own `fs_readdir`/`fs_unlink`.
    fn begin_empty_trash<S: DirectorySource>(
        browser: &Browser<S>,
        overlays: &mut Overlays,
    ) -> (bool, bool) {
        let Some(home) = home_components().filter(|home| !home.is_empty()) else {
            return (false, false);
        };
        let trash = trash_dir(&home);
        if browser.folder() != Some(trash.as_slice()) {
            return (false, false);
        }
        let repeats = |then: &BrowserWrite| matches!(then, BrowserWrite::EmptyTrash(_));
        if let Some(changed) = overlays.while_writing(NOT_EMPTIED, repeats) {
            return (changed, false);
        }
        overlays.writing = Some(Writing::new(
            WriteAct::PlanEmptyTrash(trash.clone()),
            BrowserWrite::EmptyTrash(trash),
        ));
        (false, false)
    }

    /// Ask for `entry` in the current directory under the first free name of
    /// its kind. Once the reader has made it, the inline rename opens on it so
    /// the user names it at once.
    ///
    /// A folder is an `fs_mkdir`; a document is an empty file created
    /// exclusively, so an existing file is never truncated. Either is an
    /// ordinary permission-checked call under the user's own identity.
    fn begin_new_entry<S: DirectorySource>(
        browser: &Browser<S>,
        overlays: &mut Overlays,
        entry: NewEntry,
    ) -> (bool, bool) {
        // A second New ▸ before the first is made would suggest the very same
        // name, so it is the same request again.
        let repeats = |then: &BrowserWrite| matches!(then, BrowserWrite::Entry(_));
        if let Some(changed) = overlays.while_writing(NOT_MADE, repeats) {
            return (changed, false);
        }
        let name = entry.suggest_name(browser.entries());
        let pending = match browser.prepare_entry(&name) {
            Ok(pending) => pending,
            Err(err) => {
                overlays
                    .notices
                    .refuse(NoticeSource::Write, NOT_MADE, err.message());
                return (true, false);
            }
        };
        let path = String::from(pending.path());
        let act = match entry {
            NewEntry::Folder => WriteAct::MakeFolder(path),
            NewEntry::Document(_) => WriteAct::MakeFile(path),
        };
        overlays.writing = Some(Writing::new(act, BrowserWrite::Entry(pending)));
        (false, false)
    }

    /// The title of the notice a refused New ▸ is stated in.
    const NOT_MADE: &str = "Not made";

    /// The title of the notice a place that cannot be opened is stated in.
    const NOT_OPENED: &str = "Not opened";

    /// The title of the notice a Trash that cannot be shown is stated in.
    const NO_TRASH: &str = "No Trash";

    /// The title of the notice a delete that cannot be asked is stated in.
    const NOT_DELETED: &str = "Not deleted";

    /// The title of the notice a Trash that cannot be emptied is stated in.
    const NOT_EMPTIED: &str = "Trash not emptied";

    /// Begin an in-place rename of the selected item: open a focused
    /// [`TextField`] pre-filled with the current name (bounded by the kernel's
    /// own `FS_NAME_MAX`), then reveal the row so the editor is on screen.
    /// With nothing selected, or a rename still with the volume, it is a
    /// no-op.
    fn begin_rename<S: DirectorySource>(
        browser: &mut Browser<S>,
        rename: &mut RenameEdit,
        scale: Scale,
        theme: &Theme,
        viewport: Rect,
        toolbar: ToolbarBand,
    ) -> (bool, bool) {
        let endings = browser.presentation().endings;
        let Some((name, stem)) = browser.chosen_entry().map(|entry| {
            let (shown, selected) = tairix_browse::rename_text(entry, endings);
            (shown.to_string(), selected)
        }) else {
            return (false, false);
        };
        let mut field = TextField::new()
            .with_text(&name)
            .with_max_len(FS_NAME_MAX)
            .with_selection(stem);
        field.set_focused(true);
        if !rename.open(field, endings) {
            return (false, false);
        }
        tairix_browse::render::reveal_selection(browser, scale, theme, viewport, toolbar);
        (true, false)
    }

    /// Record a Properties window for the browser's selected node.
    ///
    /// The node is resolved here, from the listing that named it, so the
    /// window the round opens afterwards describes exactly what was selected
    /// however the listing moves on. With nothing selected (an empty
    /// directory) it is a silent no-op; a node that can no longer be named
    /// states the refusal and opens nothing.
    fn ask_properties<S: DirectorySource>(
        browser: &Browser<S>,
        request: &mut Option<PropertiesRequest>,
    ) -> (bool, bool) {
        let (Some(entry), Some(folder)) = (browser.chosen_entry(), browser.folder()) else {
            return (false, false);
        };
        let kind = entry.kind();
        let target = entry.target().map(String::from);
        let art = tairix_browse::icon_for_entry(entry, folder);
        let drive = entry.drive().is_some();
        let Some(Ok(path)) = browser.chosen_target_path() else {
            report_error("that item's location could not be resolved; no window opened");
            return (false, false);
        };
        *request = Some(PropertiesRequest {
            path,
            kind,
            target,
            art,
            drive,
        });
        (false, false)
    }

    /// What an item's Info window is called after the item's name.
    const INFO_WINDOW_NAME: &str = "Info";

    /// Open an Info window on the node `request` names.
    ///
    /// The read leaves the loop: a node is one `fs_stat` plus one call per
    /// extended-attribute key, which on a contended volume is a visible stall
    /// rather than a frame. The window opens stating that it is reading and
    /// adopts the summary when it lands.
    ///
    /// Several are open at once, each pinned to its own node by *path* — so
    /// the listing behind them may move on, be reloaded, or be navigated away
    /// from without any of them describing or writing to something else.
    #[allow(clippy::too_many_arguments)] // The run's window state, threaded explicitly.
    fn open_properties(
        request: PropertiesRequest,
        windows: &mut alloc::vec::Vec<OpenWindow>,
        client: &mut WindowClient<app::RtWindowTransport>,
        desktop: &Desktop,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        reads: &Reads,
        event_endpoint: u64,
        can_chown: bool,
    ) {
        let PropertiesRequest {
            path,
            kind,
            target,
            art,
            drive,
        } = request;
        let mut title = String::new();
        tairix_browse::vfs::write_document_title(
            &mut title,
            path_leaf(&path),
            false,
            false,
            INFO_WINDOW_NAME,
        );
        // A mount point that does not parse names no drive: it is described
        // as a node, never read as whatever volume an empty path would match.
        let drive = if drive { drive_job(&path) } else { None };
        let strip = match drive {
            Some(_) => PropertiesStrip::Drive,
            None => PropertiesStrip::Node,
        };
        // The extent is already resolved at the desktop's density (the row
        // pitch it counts is a physical one), so only the screen cap is left
        // to apply — never a window larger than the display it appears on.
        let (w, h) =
            tairix_browse::render::properties_window_extent(strip, desktop.scale(), grounds.popups);
        let screen = desktop.screen();
        let mode = app::mode_for(w.min(screen.width), h.min(screen.height));
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            report_error("window surface refused; no Info window opened");
            return;
        };
        let sizing = tairix_browse::properties_sizing(desktop.scale(), grounds.popups);
        let pane = match WindowPane::open(client, event_endpoint, &mode, &title, sizing) {
            Ok((pane, _)) => pane,
            Err(err) => {
                report_error(&alloc::format!("{err}; no Info window opened"));
                return;
            }
        };
        let mut win = OpenWindow {
            pane,
            title,
            surface,
            owed: Owed::new(),
            kind: WindowKind::Properties(Box::new(PropertiesWindow {
                path,
                kind,
                target,
                art,
                state: PropertiesState::Reading,
                rows: RowList::new(0),
                scroll: ScrollColumn::new(),
                editor: attribute_editor(),
                owner: None,
                tab: strip.first(),
                perms: PermsCursor::default(),
                section: PermsPanel::default(),
                detail: PropertiesWindow::detail_for(kind, None),
                can_chown,
                writing: None,
                rereading: false,
                notices: Notices::default(),
                strip,
                drive: drive.map(|job| DriveWatch {
                    drive: Drive::new(),
                    job,
                    due: None,
                    asked: true,
                }),
            })),
        };
        let id = win.pane.id();
        if let WindowKind::Properties(props) = &win.kind {
            reads.want_properties(id, props.job());
            if let Some(watch) = &props.drive {
                reads.want_drive(id, watch.job.clone());
            }
        }
        if present_whole(&mut win, client, grounds, icons, desktop.scale()).is_err() {
            reads.forget_properties(id);
            reads.forget_drive(id);
            let _ = win.pane.close(client);
            report_error("the Info window could not be painted; it was closed again");
            return;
        }
        windows.push(win);
    }

    /// A fresh, focused `key = value` editor.
    ///
    /// Focused from the start because it is the window's one text surface: a
    /// user who clicks a row and types expects the typing to land there.
    fn attribute_editor() -> TextField {
        let mut editor = TextField::new()
            .with_max_len(ATTR_LINE_MAX)
            .with_placeholder(ATTR_EDITOR_HINT);
        editor.set_focused(true);
        editor
    }

    /// What the empty attribute editor prompts for.
    const ATTR_EDITOR_HINT: &str = "namespace.name = value";

    /// Most characters the attribute `key = value` line accepts.
    ///
    /// The kernel's own key and value bounds are what actually gate a write;
    /// this only keeps the field from growing past what either could hold
    /// together, so a line that cannot possibly be applied is not typed.
    const ATTR_LINE_MAX: usize =
        tairix_abi::FS_ATTR_KEY_MAX + tairix_abi::FS_ATTR_VALUE_MAX + " = ".len();

    /// Adopt whatever the reader has answered for `win`, reporting whether the
    /// window has something new to draw.
    ///
    /// The attribute list is re-sized to the answer, which clamps a cursor a
    /// removal left past the end.
    fn adopt_properties(win: &mut PropertiesWindow, answer: Result<Properties, Errno>) {
        win.rereading = false;
        match answer {
            Ok(props) => {
                let props = match &win.target {
                    Some(target) => props.with_target(target),
                    None => props,
                };
                win.rows.resize(props.attributes().visible().len());
                win.state = PropertiesState::Ready(props);
            }
            Err(err) => {
                win.rows.resize(0);
                win.state = PropertiesState::Refused(alloc::format!("could not be read: {err}"));
            }
        }
        win.refresh_detail();
    }

    /// Handle one event delivered to a Properties window.
    ///
    /// `Left`/`Right` walk the section strip until the permissions section
    /// takes the keyboard (Down or Tab), after which they walk its flags and
    /// Tab or `Escape` hands the keyboard back. `Escape` steps back out of
    /// whatever is open — the owning-id editor, the permissions cursor, then a
    /// typed attribute line — and closes the window when none is. The rest of
    /// the keyboard belongs to the section that draws a control for it: the
    /// permissions cursor, or the attributes list and its editor, so no key
    /// reaches a control the selected section does not draw. A press resolves
    /// through the one shared hit-test, so it acts on exactly the control the
    /// user saw.
    #[allow(clippy::too_many_arguments)] // The window, its geometry, and the event.
    fn apply_properties_event(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
        theme: &Theme,
        scale: Scale,
        window: Rect,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        match event {
            WindowEvent::CloseRequested { .. } => return (Repaint::Nothing, true),
            WindowEvent::Key {
                key: KeyInput::Pressed { key, modifiers },
                ..
            } => {
                return apply_properties_key(
                    win, window_id, reads, theme, scale, window, *key, *modifiers, damage,
                )
            }
            WindowEvent::Pointer { x, y, action, .. } => {
                return apply_properties_pointer(
                    win, window_id, reads, theme, scale, window, *x, *y, *action, damage,
                )
            }
            // A wheel turn scrolls the section on show through its own bar,
            // which reports itself and the column it slid.
            WindowEvent::Scrolled { dx, dy, .. } => {
                let view = win.view();
                let moved = match &win.state {
                    PropertiesState::Ready(props) => properties_scroll_wheel(
                        &mut win.scroll,
                        props,
                        view,
                        sections_shown(
                            win.can_chown,
                            win.owner.as_ref(),
                            &win.section,
                            win.drive.as_ref(),
                        ),
                        (window, scale, theme),
                        (*dx, *dy),
                        damage,
                    ),
                    PropertiesState::Reading | PropertiesState::Refused(_) => false,
                };
                return (Repaint::reported_if(moved), false);
            }
            _ => {}
        }
        (Repaint::Nothing, false)
    }

    /// What a Properties window's sections are composed from besides the
    /// node, taken from its fields alone so it can stand beside a borrow of
    /// its scroll column.
    fn sections_shown<'a>(
        can_chown: bool,
        owner: Option<&'a OwnerEditor>,
        section: &'a PermsPanel,
        drive: Option<&'a DriveWatch>,
    ) -> SectionsShown<'a> {
        SectionsShown {
            can_chown,
            owner: owner.map(|ed| (ed.field, &ed.editor)),
            panel: section,
            drive: drive.map(|watch| &watch.drive),
        }
    }

    /// The reading a drive window at `path` asks for, or `None` for a path
    /// that names no mount point.
    fn drive_job(path: &str) -> Option<DriveJob> {
        let mount_point = tairix_browse::vfs::components_from_absolute_path(path).ok()?;
        Some(DriveJob { mount_point })
    }

    /// Scroll the section on show the least that shows its keyboard cursor,
    /// reporting the column and its bar when it moved.
    fn reveal_cursor(
        win: &mut PropertiesWindow,
        (window, scale, theme): (Rect, Scale, &Theme),
        damage: &mut Region,
    ) -> bool {
        let view = win.view();
        match &win.state {
            PropertiesState::Ready(props) => properties_reveal(
                &mut win.scroll,
                props,
                view,
                sections_shown(
                    win.can_chown,
                    win.owner.as_ref(),
                    &win.section,
                    win.drive.as_ref(),
                ),
                (window, scale, theme),
                damage,
            ),
            PropertiesState::Reading | PropertiesState::Refused(_) => false,
        }
    }

    /// Feed one key press to a Properties window.
    #[allow(clippy::too_many_arguments)] // The window, its geometry, and the key.
    fn apply_properties_key(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
        theme: &Theme,
        scale: Scale,
        window: Rect,
        key: KeyValue,
        modifiers: AbiModifiers,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        // The owning-id editor owns the keyboard while it is open: its keys
        // commit or cancel the ownership change and none of them reaches the
        // attribute line beneath it.
        if let Some(field) = win.owner.as_ref().map(|ed| ed.field) {
            let bounds = win
                .props()
                .and_then(|props| {
                    tairix_browse::render::properties_owner_editor_rect(
                        props,
                        win.view(),
                        win.sections(),
                        (window, scale, theme),
                        field,
                    )
                })
                .unwrap_or(Rect::EMPTY);
            let (editor_key, mods) = to_editor_key(key, modifiers);
            let action = win
                .owner
                .as_mut()
                .and_then(|ed| ed.editor.on_key(editor_key, mods, bounds, damage));
            return match action {
                Some(TextAction::Submitted) => commit_owner(win, window_id, reads),
                Some(TextAction::Cancelled) => {
                    win.owner = None;
                    (Repaint::Whole, false)
                }
                Some(TextAction::Edited) => {
                    if let Some(ed) = win.owner.as_mut() {
                        let text = ed.editor.text().to_string();
                        ed.editor.set_message(owner_id_message(&text));
                    }
                    (Repaint::Whole, false)
                }
                None => (Repaint::Nothing, false),
            };
        }
        match route::properties_key(win.tab, win.perms.holds(), key) {
            route::PropertiesKey::Section(steps) => {
                let next = win.strip.stepped(win.tab, steps);
                return (whole_if(show_section(win, next)), false);
            }
            route::PropertiesKey::Close => return (Repaint::Nothing, true),
            route::PropertiesKey::Ignored => return (Repaint::Nothing, false),
            route::PropertiesKey::Permissions => {
                return apply_permissions_key(
                    win, window_id, reads, theme, scale, window, key, modifiers, damage,
                )
            }
            route::PropertiesKey::Attributes => {}
        }
        match key {
            KeyValue::Named(NamedKeyCode::Escape) => {
                // The typed line is abandoned before the window is: a user
                // half-way through an attribute has something to step back
                // out of.
                if win.editor.text().is_empty() {
                    return (Repaint::Nothing, true);
                }
                win.editor = attribute_editor();
                (Repaint::Whole, false)
            }
            KeyValue::Named(NamedKeyCode::Down | NamedKeyCode::Up) => {
                let delta = if key == KeyValue::Named(NamedKeyCode::Down) {
                    1
                } else {
                    -1
                };
                let moved = win.rows.step(delta);
                let scrolled = reveal_cursor(win, (window, scale, theme), damage);
                (whole_if(moved || scrolled), false)
            }
            _ => {
                let bounds =
                    tairix_browse::render::properties_attr_editor_rect(window, scale, theme)
                        .unwrap_or(Rect::EMPTY);
                let (editor_key, mods) = to_editor_key(key, modifiers);
                match win.editor.on_key(editor_key, mods, bounds, damage) {
                    Some(TextAction::Submitted) => set_attribute(win, window_id, reads),
                    Some(TextAction::Cancelled) => {
                        win.editor = attribute_editor();
                        (Repaint::Whole, false)
                    }
                    Some(TextAction::Edited) => (Repaint::Reported, false),
                    None => (Repaint::Nothing, false),
                }
            }
        }
    }

    /// Feed one key press to the Permissions section's cursor, acting on the
    /// control it activates exactly as a press on that control would.
    ///
    /// A cursor that only moved reports the rows and flags it repainted; a
    /// read that has not landed has nothing for the cursor to rest on.
    #[allow(clippy::too_many_arguments)] // The window, its geometry, and the key.
    fn apply_permissions_key(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
        theme: &Theme,
        scale: Scale,
        window: Rect,
        key: KeyValue,
        modifiers: AbiModifiers,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let Some(props) = win.props() else {
            return (Repaint::Nothing, false);
        };
        let (key, modifiers) = to_editor_key(key, modifiers);
        let keyed = tairix_browse::render::properties_permissions_key(
            props,
            win.view(),
            win.controls(),
            window,
            scale,
            theme,
            Keystroke {
                key,
                modifiers,
                at_ns: tairix_rt::clock_get(),
            },
            damage,
        );
        let moved = keyed.cursor != win.perms;
        win.perms = keyed.cursor;
        let scrolled = moved && reveal_cursor(win, (window, scale, theme), damage);
        let acted = match keyed.target {
            Some(PropertiesTarget::Permission(bit)) => {
                toggle_permission(win, window_id, reads, bit)
            }
            Some(PropertiesTarget::Owner(field)) => begin_owner_edit(win, field),
            _ => (Repaint::Nothing, false),
        };
        (
            acted.0.merged(Repaint::reported_if(moved || scrolled)),
            acted.1,
        )
    }

    /// Feed one pointer event to a Properties window.
    #[allow(clippy::too_many_arguments)] // The window, its geometry, and the event.
    fn apply_properties_pointer(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
        theme: &Theme,
        scale: Scale,
        window: Rect,
        x: u32,
        y: u32,
        action: PointerAction,
        damage: &mut Region,
    ) -> (Repaint, bool) {
        let point = pointer_point(x, y);
        // The scroll gutter owns a press that lands on it, so a drag on the
        // bar moves the section instead of pressing a control beneath it.
        let mut scrolled = None;
        for input in pointer_input_events(action, point) {
            // Only a landed read lays out a section to scroll.
            let view = win.view();
            let routed = match &win.state {
                PropertiesState::Ready(props) => properties_scroll_pointer(
                    &mut win.scroll,
                    props,
                    view,
                    sections_shown(
                        win.can_chown,
                        win.owner.as_ref(),
                        &win.section,
                        win.drive.as_ref(),
                    ),
                    (window, scale, theme),
                    (point, &input),
                    damage,
                ),
                PropertiesState::Reading | PropertiesState::Refused(_) => None,
            };
            if let Some(moved) = routed {
                scrolled = Some(scrolled.unwrap_or(false) || moved);
            }
        }
        // The bar reported its own look, and a move the column it slid, so a
        // bar that only brightened is repainted too and one that changed
        // nothing costs nothing.
        if let Some(repainted) = scrolled {
            return (Repaint::reported_if(repainted), false);
        }
        let Some(point) = press_point(action, x, y) else {
            return (Repaint::Nothing, false);
        };
        let Some(target) = win.props().and_then(|props| {
            tairix_browse::render::properties_hit(
                props,
                win.view(),
                win.controls(),
                window,
                scale,
                theme,
                point,
            )
        }) else {
            return (Repaint::Nothing, false);
        };
        match target {
            PropertiesTarget::Tab(tab) => (whole_if(show_section(win, tab)), false),
            PropertiesTarget::Permission(bit) => toggle_permission(win, window_id, reads, bit),
            PropertiesTarget::Owner(field) => begin_owner_edit(win, field),
            PropertiesTarget::Attribute(index) => (whole_if(select_attribute(win, index)), false),
            PropertiesTarget::Editor => (Repaint::Nothing, false),
            PropertiesTarget::Action(AttrAction::Set) => set_attribute(win, window_id, reads),
            PropertiesTarget::Action(AttrAction::Remove) => remove_attribute(win, window_id, reads),
        }
    }

    /// Show `tab`, reporting whether the window changed section.
    ///
    /// Switching away from a half-typed ownership id abandons it, and hands
    /// the keyboard back to the strip: both belong to the section they are
    /// drawn in, and either left live behind a tab the user cannot see would
    /// take their next keystroke.
    fn show_section(win: &mut PropertiesWindow, tab: PropertiesTab) -> bool {
        if win.tab == tab {
            return false;
        }
        win.tab = tab;
        win.owner = None;
        win.perms = PermsCursor::default();
        win.scroll.set_offset(0);
        // A drive is read only while its section shows: coming back reads it
        // at once and starts its trace afresh, going away lets the reading out
        // be the last.
        if let Some(watch) = &mut win.drive {
            if tab != PropertiesTab::Drive {
                watch.drive.pause();
            }
            watch.due = (tab == PropertiesTab::Drive && !watch.asked).then_some(0);
        }
        true
    }

    /// Make attribute row `index` current, loading it into the editor so a
    /// value can be changed in place.
    ///
    /// A value whose bytes a typed line could not reproduce loads its key
    /// alone and says so: offering the bytes back lossily would write
    /// something other than what is stored.
    fn select_attribute(win: &mut PropertiesWindow, index: usize) -> bool {
        let moved = win.rows.select(index);
        let Some(attr) = win
            .props()
            .and_then(|props| props.attributes().visible().get(index))
        else {
            return moved;
        };
        // A key the grammar allows but a typed line could not reproduce is
        // offered back as nothing at all: the volume, not the kernel, decided
        // those bytes.
        let Some(key) = attr.key_text() else {
            app::report(
                APP_NAME,
                format_args!(
                    "{} has a key that cannot be edited as text",
                    attr.key_display()
                ),
            );
            return true;
        };
        let line = if let Some(text) = attr.text() {
            alloc::format!("{key} = {text}")
        } else {
            app::report(
                APP_NAME,
                format_args!(
                    "{key} holds bytes that cannot be edited as text; \
                 its key is offered without them"
                ),
            );
            alloc::format!("{key} = ")
        };
        win.editor = attribute_editor().with_text(line);
        true
    }

    /// Flip one permission `bit` of the node's mode: the reader makes the
    /// `fs_set_mode` call, and the window reads the node again once it has.
    fn toggle_permission(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
        bit: u32,
    ) -> (Repaint, bool) {
        if !mode_flip_allowed(win.writing.is_some(), win.rereading) {
            return (
                refuse_properties_write(win, &PropertiesWrite::Mode, STILL_WRITING),
                false,
            );
        }
        let Some(mode) = win.props().map(|props| (props.mode() & FS_MODE_MASK) ^ bit) else {
            return (Repaint::Nothing, false);
        };
        if let Err(err) = tairix_browse::mode_edit::validate_mode(mode) {
            app::report(APP_NAME, err.message());
            return (Repaint::Nothing, false);
        }
        let Some(act) = win.change(Change::Mode(mode)) else {
            return (Repaint::Nothing, false);
        };
        properties_write(win, window_id, reads, act, PropertiesWrite::Mode)
    }

    /// Hand a Properties window's write to the reader, or take its answer at
    /// once when there is no reader.
    fn properties_write(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
        act: WriteAct,
        then: PropertiesWrite,
    ) -> (Repaint, bool) {
        match reads.write(act) {
            WriteSubmit::Pending(ticket) => {
                win.writing = Some((ticket, then));
                (Repaint::Nothing, false)
            }
            WriteSubmit::Done(answer) => (
                apply_properties_write(win, window_id, reads, &then, answer.status()),
                false,
            ),
            WriteSubmit::Unsent => {
                report_error(WRITES_WAITING);
                (refuse_properties_write(win, &then, WRITES_WAITING), false)
            }
        }
    }

    /// Act on a Properties window's write answer: a write that landed reads
    /// the node again, and a refusal is stated where the window can show it,
    /// and on `stderr` for the record.
    fn apply_properties_write(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
        then: &PropertiesWrite,
        result: Result<(), Errno>,
    ) -> Repaint {
        if result.is_ok() {
            match then {
                PropertiesWrite::Owner => win.owner = None,
                PropertiesWrite::Attribute {
                    removing: false, ..
                } => win.editor = attribute_editor(),
                // A message beside the line answered something asked meanwhile,
                // which this landing has settled.
                PropertiesWrite::Attribute { removing: true, .. } => win.editor.set_message(None),
                PropertiesWrite::Mode => {}
            }
            reads.want_properties(window_id, win.job());
            win.rereading = true;
            return Repaint::Whole;
        }
        let Err(errno) = result else {
            return Repaint::Nothing;
        };
        // The name holds another node now: show that one, which the next
        // change is computed from.
        if errno == Errno::Stale {
            reads.want_properties(window_id, win.job());
            win.rereading = true;
        }
        let reason = change_refusal(errno);
        let title = then.refusal_title();
        match then {
            PropertiesWrite::Attribute { shown, .. } => app::report(
                APP_NAME,
                format_args!("{}: {title}, {shown}: {reason} ({errno})", win.path),
            ),
            PropertiesWrite::Mode | PropertiesWrite::Owner => app::report(
                APP_NAME,
                format_args!("{}: {title}: {reason} ({errno})", win.path),
            ),
        }
        refuse_properties_write(win, then, reason)
    }

    /// Show `reason` for a refused Properties write in the editor that made
    /// it, or in a notice for a permission flip, which has none.
    fn refuse_properties_write(
        win: &mut PropertiesWindow,
        then: &PropertiesWrite,
        reason: &str,
    ) -> Repaint {
        match then {
            PropertiesWrite::Owner => {
                if let Some(ed) = win.owner.as_mut() {
                    ed.editor.set_message(Some(String::from(reason)));
                }
            }
            PropertiesWrite::Attribute { .. } => win.editor.set_message(Some(String::from(reason))),
            PropertiesWrite::Mode => {
                win.notices
                    .refuse(NoticeSource::Write, then.refusal_title(), reason);
            }
        }
        Repaint::Whole
    }

    /// Open the inline id editor over the clicked owning value, pre-filled
    /// with the current id and bounded to a `u32`'s ten digits.
    ///
    /// The caller has already confirmed the press landed on that value; the
    /// kernel still authorises the eventual commit under the user's own
    /// identity (the editor holds no authority).
    fn begin_owner_edit(win: &mut PropertiesWindow, field: OwnerField) -> (Repaint, bool) {
        if !win.can_chown {
            return (Repaint::Nothing, false);
        }
        let Some(current) = win.props().map(|props| match field {
            OwnerField::Uid => props.uid(),
            OwnerField::Gid => props.gid(),
        }) else {
            return (Repaint::Nothing, false);
        };
        let mut editor = TextField::new()
            .with_text(current.to_string())
            .with_max_len(OWNER_ID_MAX_DIGITS);
        editor.set_focused(true);
        win.owner = Some(OwnerEditor { field, editor });
        (Repaint::Whole, false)
    }

    /// Commit the owner editor's id: the reader makes the `fs_set_owner` call,
    /// and the editor closes once it has; a refusal is stated in the editor.
    fn commit_owner(win: &mut PropertiesWindow, window_id: u64, reads: &Reads) -> (Repaint, bool) {
        if win.writing.is_some() {
            return (
                refuse_properties_write(win, &PropertiesWrite::Owner, STILL_WRITING),
                false,
            );
        }
        let Some(ed) = win.owner.as_ref() else {
            return (Repaint::Nothing, false);
        };
        let field = ed.field;
        let text = ed.editor.text().to_string();
        let Ok(id) = text.parse::<u32>() else {
            if let Some(ed) = win.owner.as_mut() {
                ed.editor.set_message(Some(String::from(OWNER_ID_HINT)));
            }
            return (Repaint::Whole, false);
        };
        let change = match field {
            OwnerField::Uid => OwnerChange::user(id),
            OwnerField::Gid => OwnerChange::group(id),
        };
        let (uid, gid) = match tairix_browse::owner_edit::owner_ids(change) {
            Ok(ids) => ids,
            Err(err) => {
                if let Some(ed) = win.owner.as_mut() {
                    ed.editor.set_message(Some(String::from(err.message())));
                }
                return (Repaint::Whole, false);
            }
        };
        let Some(act) = win.change(Change::Owner { uid, gid }) else {
            return (Repaint::Nothing, false);
        };
        properties_write(win, window_id, reads, act, PropertiesWrite::Owner)
    }

    /// Set the attribute the editor's `key=value` line names: the reader makes
    /// the call, and the editor clears once it has.
    fn set_attribute(win: &mut PropertiesWindow, window_id: u64, reads: &Reads) -> (Repaint, bool) {
        let line = win.editor.text().to_string();
        let Ok((key, value)) = tairix_fsmeta::attr::parse_assignment(&line) else {
            win.editor.set_message(Some(String::from(ATTR_EDITOR_HINT)));
            return (Repaint::Whole, false);
        };
        let then = PropertiesWrite::Attribute {
            shown: String::from(key.as_str()),
            removing: false,
        };
        if win.writing.is_some() {
            return (refuse_properties_write(win, &then, STILL_WRITING), false);
        }
        let Some(act) = win.change(Change::SetAttr {
            key: String::from(key.as_str()),
            value: String::from(value),
        }) else {
            return (refuse_properties_write(win, &then, NOT_READ), false);
        };
        properties_write(win, window_id, reads, act, then)
    }

    /// Remove the attribute under the cursor: the reader makes the call.
    fn remove_attribute(
        win: &mut PropertiesWindow,
        window_id: u64,
        reads: &Reads,
    ) -> (Repaint, bool) {
        let cursor = win.rows.cursor();
        let Some((key, shown)) = win
            .props()
            .and_then(|props| props.attributes().visible().get(cursor))
            .map(|attr| (String::from(attr.key()), attr.key_display()))
        else {
            return (Repaint::Nothing, false);
        };
        let then = PropertiesWrite::Attribute {
            shown,
            removing: true,
        };
        if win.writing.is_some() {
            return (refuse_properties_write(win, &then, STILL_WRITING), false);
        }
        let Some(act) = win.change(Change::RemoveAttr(key)) else {
            return (Repaint::Nothing, false);
        };
        properties_write(win, window_id, reads, act, then)
    }

    /// The live-validation message for a typed owner/group id, or `None` when
    /// the text is a well-formed, assignable `u32` id. It never blocks typing;
    /// it only flags a value the commit would reject.
    fn owner_id_message(text: &str) -> Option<String> {
        match text.parse::<u32>() {
            Ok(id) if id != tairix_abi::fs::FS_OWNER_UNCHANGED => None,
            _ => Some(String::from(OWNER_ID_HINT)),
        }
    }

    /// Feed one key to the open rename editor, reporting what it repainted
    /// into `damage`. A submit commits the name ([`commit_rename`]), a cancel
    /// abandons the edit, and an edit live-validates the typed name so a clash
    /// or a bad character is flagged as the user types.
    fn apply_rename_key<S: DirectorySource>(
        browser: &mut Browser<S>,
        edit: &mut RenameEdit,
        reads: &Reads,
        frame: Frame<'_>,
        key: KeyValue,
        modifiers: AbiModifiers,
        damage: &mut Region,
    ) -> Repaint {
        let (key, mods) = to_editor_key(key, modifiers);
        // The rectangle the editor is *drawn* at, so a caret the key moves
        // lands where its glyphs are.
        let bounds = editor_rect(browser, frame).unwrap_or(Rect::EMPTY);
        let mut keyed = damage::sink();
        let action = edit.key(key, mods, bounds, &mut keyed);
        for rect in keyed.rects() {
            damage.add(*rect);
        }
        let redrawn = Repaint::reported_if(!keyed.is_empty());
        match action {
            Some(TextAction::Submitted) => commit_rename(browser, edit, reads, frame, damage).1,
            Some(TextAction::Edited) => {
                let verdict = edit
                    .field()
                    .zip(browser.chosen_name())
                    .map(|(field, current)| {
                        validate_new_name(field.text(), current, browser.entries())
                    });
                if let (Some(verdict), Some(field)) = (verdict, edit.editing()) {
                    field.set_message(match verdict {
                        Ok(()) | Err(RenameError::Unchanged) => None,
                        Err(err) => Some(String::from(err.message())),
                    });
                }
                redrawn
            }
            Some(TextAction::Cancelled) | None => redrawn,
        }
    }

    /// Commit the open rename editor, reporting what it repainted into
    /// `damage`: an unchanged name closes it, a bad one states why in it, and
    /// a valid one goes to the volume — the editor closes, or states the
    /// volume's refusal, once the answer lands.
    fn commit_rename<S: DirectorySource>(
        browser: &mut Browser<S>,
        edit: &mut RenameEdit,
        reads: &Reads,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> (RenameCommit, Repaint) {
        if edit.in_flight() {
            return (RenameCommit::Submitted, Repaint::Nothing);
        }
        let Some(new_name) = edit.field().map(|field| field.text().to_string()) else {
            return (RenameCommit::Refused, Repaint::Nothing);
        };
        match browser.prepare_rename(&new_name, edit.endings()) {
            Ok(pending) => (
                RenameCommit::Submitted,
                submit_rename(browser, edit, reads, pending, frame, damage),
            ),
            Err(RenameError::Unchanged) => (
                RenameCommit::Unchanged,
                close_editor(browser, edit, frame, damage),
            ),
            Err(err) => {
                edit.refuse(rename_refusal(err));
                (
                    RenameCommit::Refused,
                    Repaint::reported_if(report_editor(browser, frame, damage)),
                )
            }
        }
    }

    /// Hand `pending` to the volume, reporting what it repainted into
    /// `damage`: nothing until the answer lands, unless there was no reader
    /// to take it and it was carried out here.
    fn submit_rename<S: DirectorySource>(
        browser: &mut Browser<S>,
        edit: &mut RenameEdit,
        reads: &Reads,
        pending: PendingRename,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> Repaint {
        let act = WriteAct::Rename {
            from: String::from(pending.from()),
            to: String::from(pending.to()),
        };
        match reads.write(act) {
            WriteSubmit::Done(answer) => {
                apply_rename_outcome(browser, edit, &pending, answer.status(), frame, damage)
            }
            WriteSubmit::Pending(ticket) => {
                edit.submitted(ticket, pending);
                Repaint::Nothing
            }
            WriteSubmit::Unsent => {
                edit.refuse(String::from(WRITES_WAITING));
                Repaint::reported_if(edit.is_open() && report_editor(browser, frame, damage))
            }
        }
    }

    /// Apply what the volume said to `pending`, reporting what it repainted
    /// into `damage`: a rename that took re-lists the folder it was made in,
    /// closing the editor and bringing the entry under its new name into
    /// view, and a refusal states its reason in the editor, or on stderr once
    /// that has closed.
    fn apply_rename_outcome<S: DirectorySource>(
        browser: &mut Browser<S>,
        edit: &mut RenameEdit,
        pending: &PendingRename,
        moved: Result<(), Errno>,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> Repaint {
        match browser.finish_rename(pending, moved) {
            Ok(true) => {
                let closed = close_editor(browser, edit, frame, damage);
                // A re-list read on the spot may have moved every entry; one
                // under way repaints when it lands.
                if browser.is_listing() {
                    closed
                } else {
                    Repaint::Whole
                }
            }
            Ok(false) => Repaint::Nothing,
            Err(err) => {
                edit.refuse(rename_refusal(err));
                Repaint::reported_if(edit.is_open() && report_editor(browser, frame, damage))
            }
        }
    }

    /// The words a refused rename is stated in, the volume's own reason
    /// included.
    fn rename_refusal(err: RenameError) -> String {
        match err {
            RenameError::Refused(errno) => alloc::format!("the volume refused it: {errno}"),
            other => String::from(other.message()),
        }
    }

    /// State each window's refused rename in the window: the editor is one row
    /// tall, which leaves no room to draw the reason beneath it.
    fn state_rename_refusals(windows: &mut [OpenWindow]) {
        for win in windows.iter_mut() {
            let Some(state) = win.browser() else {
                continue;
            };
            if let Some(reason) = state.overlays.rename.take_refusal() {
                state
                    .overlays
                    .notices
                    .refuse(NoticeSource::Rename, "Not renamed", &reason);
                win.owed.owe_whole();
            }
        }
    }

    /// Close the rename editor and bring the chosen entry into view,
    /// reporting both into `damage`.
    fn close_editor<S: DirectorySource>(
        browser: &mut Browser<S>,
        edit: &mut RenameEdit,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> Repaint {
        let mut moved = edit.is_open() && report_editor(browser, frame, damage);
        edit.close();
        let mark = ViewMark::of(browser, frame);
        tairix_browse::render::reveal_selection(
            browser,
            frame.scale,
            frame.theme,
            frame.viewport,
            frame.toolbar,
        );
        moved |= mark.report(browser, frame, damage);
        Repaint::reported_if(moved)
    }

    /// Where the rename editor is drawn: over the chosen entry's name, as far
    /// as that shows.
    fn editor_rect<S: DirectorySource>(browser: &Browser<S>, frame: Frame<'_>) -> Option<Rect> {
        tairix_browse::render::selection_name_rect(
            browser,
            frame.scale,
            frame.theme,
            frame.viewport,
            frame.toolbar,
        )
    }

    /// Report where the rename editor is drawn into `damage`, and the tile it
    /// stands over — whose name is withheld while it is open and drawn again
    /// once it closes — answering whether any of it shows.
    fn report_editor<S: DirectorySource>(
        browser: &Browser<S>,
        frame: Frame<'_>,
        damage: &mut Region,
    ) -> bool {
        let Some(rect) = editor_rect(browser, frame) else {
            return false;
        };
        damage.add(rect);
        let tile = browser.chosen_index().and_then(|index| {
            tairix_browse::render::entry_rect(
                browser,
                frame.scale,
                frame.theme,
                frame.viewport,
                frame.toolbar,
                index,
            )
        });
        if let Some(tile) = tile {
            damage.add(tile);
        }
        true
    }

    /// Take every write the reader carried out to the window that asked:
    /// a rename's editor, a browser window's write, or a Properties window's.
    /// An answer for a window that has gone belongs to nobody and is dropped.
    /// Answers whether a window changed.
    fn collect_writes(
        windows: &mut [OpenWindow],
        reads: &Reads,
        theme: &Theme,
        scale: Scale,
    ) -> bool {
        let mut owed = false;
        while let Some(done) = reads.take_written() {
            for win in windows.iter_mut() {
                let mode = *win.pane.mode();
                let id = win.pane.id();
                let repaint = match &mut win.kind {
                    WindowKind::Browser(state) if state.overlays.rename.awaits(done.ticket) => {
                        let Some(pending) = state.overlays.rename.answered(done.ticket) else {
                            break;
                        };
                        let mut damage = damage::sink();
                        let frame = frame_of(state, &mode, theme, scale);
                        let repaint = apply_rename_outcome(
                            &mut state.browser,
                            &mut state.overlays.rename,
                            &pending,
                            done.answer.status(),
                            frame,
                            &mut damage,
                        );
                        owe(win, repaint, &damage);
                        owed |= repaint != Repaint::Nothing;
                        break;
                    }
                    WindowKind::Browser(state)
                        if state
                            .overlays
                            .writing
                            .as_ref()
                            .is_some_and(|writing| writing.ticket == Some(done.ticket)) =>
                    {
                        let Some(writing) = state.overlays.writing.take() else {
                            break;
                        };
                        let frame = frame_of(state, &mode, theme, scale);
                        apply_browser_write(state, writing.then, done.answer, frame)
                    }
                    WindowKind::Properties(props)
                        if props.writing.as_ref().map(|(ticket, _)| *ticket)
                            == Some(done.ticket) =>
                    {
                        let Some((_, then)) = props.writing.take() else {
                            break;
                        };
                        apply_properties_write(props, id, reads, &then, done.answer.status())
                            != Repaint::Nothing
                    }
                    _ => continue,
                };
                if repaint {
                    win.owed.owe_whole();
                    owed = true;
                }
                break;
            }
        }
        owed
    }

    /// Map the window channel's wire key event onto the desktop control
    /// vocabulary the shared [`TextField`] consumes.
    fn to_editor_key(key: KeyValue, mods: AbiModifiers) -> (Key, Modifiers) {
        let modifiers = Modifiers {
            shift: mods.shift,
            ctrl: mods.ctrl,
            alt: mods.alt,
            meta: mods.meta,
        };
        let key = match key {
            KeyValue::Char(ch) => Key::Char(ch),
            KeyValue::Named(named) => Key::Named(named_to_editor(named)),
        };
        (key, modifiers)
    }

    /// Map a wire [`NamedKeyCode`] onto the desktop [`NamedKey`]. The two sets
    /// are the producer/consumer halves of one keyboard vocabulary, so this is
    /// a total mapping with no guessing.
    fn named_to_editor(named: NamedKeyCode) -> NamedKey {
        match named {
            NamedKeyCode::Enter => NamedKey::Enter,
            NamedKeyCode::Escape => NamedKey::Escape,
            NamedKeyCode::Backspace => NamedKey::Backspace,
            NamedKeyCode::Tab => NamedKey::Tab,
            NamedKeyCode::Delete => NamedKey::Delete,
            NamedKeyCode::Insert => NamedKey::Insert,
            NamedKeyCode::Home => NamedKey::Home,
            NamedKeyCode::End => NamedKey::End,
            NamedKeyCode::PageUp => NamedKey::PageUp,
            NamedKeyCode::PageDown => NamedKey::PageDown,
            NamedKeyCode::Left => NamedKey::Left,
            NamedKeyCode::Right => NamedKey::Right,
            NamedKeyCode::Up => NamedKey::Up,
            NamedKeyCode::Down => NamedKey::Down,
            NamedKeyCode::F1 => NamedKey::Function { number: 1 },
            NamedKeyCode::F2 => NamedKey::Function { number: 2 },
            NamedKeyCode::F3 => NamedKey::Function { number: 3 },
            NamedKeyCode::F4 => NamedKey::Function { number: 4 },
            NamedKeyCode::F5 => NamedKey::Function { number: 5 },
            NamedKeyCode::F6 => NamedKey::Function { number: 6 },
            NamedKeyCode::F7 => NamedKey::Function { number: 7 },
            NamedKeyCode::F8 => NamedKey::Function { number: 8 },
            NamedKeyCode::F9 => NamedKey::Function { number: 9 },
            NamedKeyCode::F10 => NamedKey::Function { number: 10 },
            NamedKeyCode::F11 => NamedKey::Function { number: 11 },
            NamedKeyCode::F12 => NamedKey::Function { number: 12 },
        }
    }

    /// Bind the app's own event mailbox through the shared shell and add this
    /// app's own any-child member to the wait-set it built.
    ///
    /// A bundle the file manager launched exiting wakes the park so it is
    /// reaped promptly (never left a zombie). Adding that member needs no
    /// capability — a process may always wait on its own children — so a
    /// refusal here is a genuine bring-up failure.
    fn bind_event_mailbox() -> Result<(u64, u64), i32> {
        let binding =
            app::bind_event_mailbox().map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        if tairix_rt::waitset_ctl(
            binding.set(),
            WaitSetOp::Add,
            WaitSourceKind::Child,
            WAITSET_CHILD_ANY,
            CHILD_TOKEN,
        ) != 0
        {
            return Err(app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                "child wait refused",
            ));
        }
        Ok((binding.endpoint(), binding.set()))
    }

    /// The transient overlay/clipboard state the event loop threads, all
    /// closed at start-up.
    fn initial_overlays() -> Overlays {
        Overlays {
            rename: RenameEdit::default(),
            rename_on_arrival: None,
            delete: None,
            open_with: None,
            starting: None,
            notices: Notices::default(),
            writing: None,
            clipboard: None,
            double_click: DoubleClickTracker::new(),
            drag: None,
            carrying: None,
            collapse_on_release: None,
            rename_arm: None,
            sweep: None,
        }
    }

    /// Whether the launching user holds `CAP_FS_CHOWN`, read once from the
    /// kernel-attested self-origin (a refused query fails closed to "not
    /// held").
    ///
    /// A session that cannot reassign an owner is never shown the control; the
    /// kernel authorises the change either way.
    fn holds_chown() -> bool {
        tairix_rt::self_origin()
            .is_ok_and(|origin| origin.capabilities().holds_cap(CapabilityId::FS_CHOWN))
    }

    /// State a command line the program cannot act on — the reason, then the
    /// usage banner — and hand back the usage exit code for `main`.
    fn usage_error(reason: &str) -> i32 {
        report_error(reason);
        let _ = writeln!(Stderr, "{USAGE}");
        EXIT_USAGE
    }

    /// The live directory listing seam the browser reads through: one
    /// `read_dir_all` under the launching user's own identity, so every
    /// listing is an ordinary permission-checked read and the app holds no
    /// authority of its own.
    fn list_directory(path: &str) -> Result<alloc::vec::Vec<u8>, Errno> {
        tairix_rt::read_dir_all(path.as_bytes()).map_err(Errno::from_syscall)
    }

    /// A window asked for and not yet opened: its first listing is with the
    /// reader, and it opens, fitted to that listing, once one lands.
    struct PendingWindow {
        browser: Browser<DeferredSource>,
        listing: FilesClient,
        /// Where it may open, the place being tried first; none for the
        /// network, which has no place to fall back to.
        ladder: Option<Ladder>,
        /// Whether the process ends when it is given up: the window an
        /// ordinary file manager was started to show.
        vital: bool,
    }

    /// The windows asked for and not yet open.
    type Opening = RefCell<alloc::vec::Vec<PendingWindow>>;

    /// Ask for a window at the first place on `ladder`. A place refused at
    /// once is stated and the next one tried; `None`, stated, when none is
    /// left.
    fn open_browser(
        reads: &alloc::sync::Arc<Reads>,
        mut ladder: Ladder,
        settings: &Preferences,
    ) -> Option<PendingWindow> {
        let listing = reads.listing_client();
        let Some(browser) = browse_ladder(reads, listing, &mut ladder, settings) else {
            reads.forget_listing(listing);
            return None;
        };
        Some(PendingWindow {
            browser,
            listing,
            ladder: Some(ladder),
            vital: false,
        })
    }

    /// A browser at the first place on `ladder` whose listing is not refused
    /// at once, its listing asked of the reader; `None`, stated, when every
    /// place is.
    ///
    /// Each place gets a browser of its own, so a refused one is in no window's
    /// history.
    fn browse_ladder(
        reads: &alloc::sync::Arc<Reads>,
        listing: FilesClient,
        ladder: &mut Ladder,
        settings: &Preferences,
    ) -> Option<Browser<DeferredSource>> {
        loop {
            let place = ladder.place()?.to_vec();
            let source = DeferredSource(alloc::sync::Arc::clone(reads), listing);
            match Browser::open_at(source, place) {
                Ok(mut browser) => {
                    browser.set_view_mode(settings.view);
                    browser.set_sort_mode(settings.sort);
                    if let Err(err) = browser.set_presentation(settings.presentation) {
                        report_error(&alloc::format!("listing refused ({err})"));
                    }
                    return Some(browser);
                }
                Err(err) => {
                    if !step_down(ladder, err) {
                        return None;
                    }
                }
            }
        }
    }

    /// Step down `ladder` past the place `err` refused, stating the move — or,
    /// for a file asked for, on to the folder that shows it — answering
    /// `false`, stated, when no place is left.
    fn step_down(ladder: &mut Ladder, err: tairix_browse::BrowseError) -> bool {
        if ladder.show_instead(err) {
            return true;
        }
        let Some(reason) = ladder.step(command::listing_refusal(err)) else {
            report_error(&alloc::format!("listing refused ({err}); no window opened"));
            return false;
        };
        report_error(&reason);
        true
    }

    /// Open each asked-for window whose first listing has landed, fitted to it,
    /// and move one whose listing was refused on down its ladder; one with no
    /// place left is given up, as stated. One landing where a window already
    /// shows raises that window instead, so each folder has one.
    ///
    /// Answers the exit code the process owes when the window given up was the
    /// one it was started to show.
    #[allow(clippy::too_many_arguments)] // The window set's whole surround, threaded explicitly.
    fn open_landed_windows(
        windows: &mut alloc::vec::Vec<OpenWindow>,
        opening: &Opening,
        client: &mut WindowClient<app::RtWindowTransport>,
        desktop: &Desktop,
        places: &Places,
        grounds: Grounds<'_>,
        icons: &RefCell<IconPipeline>,
        reads: &alloc::sync::Arc<Reads>,
        (event_endpoint, settings): (u64, &Preferences),
    ) -> Option<i32> {
        let mut opening = opening.borrow_mut();
        let mut at = 0;
        while at < opening.len() {
            let asked = &mut opening[at];
            if let Err(err) = asked.browser.resume() {
                let next = asked.ladder.as_mut().and_then(|ladder| {
                    step_down(ladder, err)
                        .then(|| browse_ladder(reads, asked.listing, ladder, settings))
                        .flatten()
                });
                if let Some(browser) = next {
                    // Looked at again at once: a place read on this thread has
                    // already answered.
                    asked.browser = browser;
                    continue;
                }
                let given_up = opening.remove(at);
                reads.forget_listing(given_up.listing);
                if given_up.vital {
                    return Some(EXIT_NO_LISTING);
                }
                continue;
            }
            if asked.browser.is_listing() {
                at += 1;
                continue;
            }
            let landed = opening.remove(at);
            let (listing, vital) = (landed.listing, landed.vital);
            // A window already showing where this one landed — reached by a
            // fallback, or since — is raised in its place, saying why.
            let shown_at = landed.browser.location();
            if let Some(win) = windows.iter_mut().find(|win| win.shows(&shown_at)) {
                let shows = landed
                    .ladder
                    .as_ref()
                    .zip(landed.browser.folder())
                    .and_then(|(ladder, folder)| ladder.shows_in(folder));
                if let Some(name) = shows {
                    reveal_entry(win, name);
                }
                if raise_showing(client, win.pane.id()) {
                    let moved = landed.ladder.as_ref().and_then(Ladder::moved);
                    if let (Some(reason), Some(state)) = (moved, win.browser()) {
                        state.overlays.notices.refuse(
                            NoticeSource::Listing,
                            OPENED_ELSEWHERE,
                            &reason,
                        );
                        win.owed.owe_whole();
                    }
                    reads.forget_listing(listing);
                    continue;
                }
            }
            match open_window(
                client,
                event_endpoint,
                desktop,
                places,
                grounds,
                icons,
                (landed, settings),
            ) {
                Ok(win) => windows.push(win),
                Err(code) => {
                    reads.forget_listing(listing);
                    if vital {
                        return Some(code);
                    }
                }
            }
        }
        None
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the
    /// runtime is set up and routes its return value through the `exit`
    /// syscall.
    #[allow(clippy::too_many_lines)] // One linear bring-up plus one event loop; splitting would obscure the flow.
    fn main() -> i32 {
        // --- The sandbox-worker role, before any other argument handling: the
        // grid's icon artwork is untrusted input, so it is decoded by a
        // capability-empty child this same binary is re-entered as with the
        // reserved role argument. That child serves rasterisation requests over
        // its wired standard streams and nothing else — it never becomes the
        // file manager.
        if worker_role() {
            return serve_stdio(&mut ImageRenderService::default()).exit_code();
        }

        // From here this task drives a user-facing loop, so declare the
        // frame it owes. A debug image then reports any span that overruns,
        // naming the call that spent it; a shippable one arms nothing and
        // answers zero, which is why the result is not examined.
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);

        // --- The command line: an optional starting directory, the reserved
        // short-help switches, and nothing else. A location the program cannot
        // accept is stated and recovered from below; a command line it cannot
        // act on at all is refused here.
        let parsed = tairix_rt::args()
            .ok_or(UsageError::NotUtf8)
            .and_then(|arguments| command::parse(&arguments));
        let start = match parsed {
            Ok(Command::Open(start)) => start,
            Ok(Command::Help) => return tairix_help::print_own_short_help(APP_NAME, Some(USAGE)),
            Err(err) => return usage_error(&alloc::format!("{err}")),
        };

        if let Some(reason) = &start.refused {
            report_error(reason);
        }

        // --- What the desktop is, before anything is sized or painted, so the
        // first frame is right rather than a guess corrected once the user has
        // seen it. The reply also tells this process who the session is, which
        // is the identity it requires of every event's attested sender — and
        // it does so without a window, which is what lets a component declare
        // its slot and start answering it before it opens one.
        let mut client = WindowClient::new(app::RtWindowTransport);
        let (mut desktop, mut themes) = match app::bring_up_desktop(&mut client) {
            Ok(pair) => pair,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let Some(server) = client.session() else {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "the desktop session did not identify itself",
            );
        };

        // --- The event mailbox the app parks on, bound and added to a
        // fresh wait-set (a bring-up refusal exits fail-loud with its code).
        let (event_endpoint, set) = match bind_event_mailbox() {
            Ok(pair) => pair,
            Err(code) => return code,
        };

        // --- The file manager's own settings, read before any window so the
        // first opens as they say, and written from here on by a worker so no
        // change waits on the store.
        let (preferences, refusals) = tairix_appdata::loaded::<Preferences>(
            &tairix_appdata::Settings::open_without_defaults(&mut tairix_appdata::RtHost),
        );
        for refusal in refusals {
            app::report(APP_NAME, refusal);
        }
        let writer = alloc::sync::Arc::new(tairix_appdata::publish_worker::<Preferences>(
            tairix_appdata::Opening::Bare,
        ));
        app::start_worker(APP_NAME, &writer, set, SETTINGS_TOKEN, "settings");
        let kept = RefCell::new(Kept {
            publication: Publication::new(preferences),
            writer: alloc::sync::Arc::clone(&writer),
        });

        // The places rail: the user's own shortcuts at once, and what is
        // mounted once the reader has asked the System Information service for
        // it — never a round trip on this thread. This is the process's copy,
        // the one a component's slot menu is declared from; each window takes
        // its own so its focus and hover are its own.
        let mut places = Places::new(&home_components().unwrap_or_default(), &[]);

        // --- The icon-bar presence, before this process owns any window. A
        // declared presence belongs to the *process*, so declaring it first is
        // what makes the slot carry this menu and this click from the moment
        // it appears; declare it after a window and the session derives a slot
        // from that window meanwhile — one that opens no menu and does nothing
        // when clicked. For a component that is the whole point: its slot is
        // all it has until the user asks for a window.
        declare_app_bar(&mut client, event_endpoint, start.role, &places);

        // --- The grid's icon artwork: the decode cache and the resolver its
        // misses go to — budgeted from one window's frame size and shared by
        // every window this process opens, so one decode serves them all.
        // Shared, too, between the present path (which draws through it) and
        // the parked event source (which trims it when the machine reports a
        // different memory-pressure band); dropping it releases the retained
        // pixels.
        // The cache-report rows go with this process on every way out.
        // `bind_event_mailbox` already primed the gauge with the band in
        // force now (`tairix_procinfo::pressure::watch`), so the cache never
        // runs on the fail-closed unknown state before the first draw.
        let _cache_report = tairix_rt::cachereport::ReportGuard;

        // --- The reader: every directory listing, folder cue, and program-store
        // walk this app makes, run on a worker so a slow or contended disk
        // cannot stall the window. A pipe the kernel refuses, or a thread it
        // will not grant, leaves those reads on this task — where they used to
        // be, and stated once.
        // The network view's doorbell, bound once: a refusal is stated when
        // the network is asked for, and every folder still lists.
        let doorbell = network_doorbell(set);
        let reads = alloc::sync::Arc::new(Reads::new(
            tairix_rt::sync::WorkerWake::create(),
            set,
            doorbell.as_ref().map(PrivatePort::id).map_err(|err| *err),
        ));
        let reader = if reads.wake.is_armed() {
            spawn_reader(&reads)
        } else {
            report_error("no reader wake pipe; directory listings happen on the event loop");
            None
        };
        if reader.is_none() {
            reads.alone();
        }
        // Answers wake the loop over the reader's pipe, so a refused pipe
        // leaves operations on the loop too.
        let operations = if reads.wake.is_armed() {
            spawn_operations(&reads)
        } else {
            None
        };
        if operations.is_none() {
            reads.operations_alone();
        }
        // Declared after the reader's handle, so it runs first: the desks stop,
        // the operations worker is joined, then the reader detaches.
        let _reads_guard = ReadsGuard {
            reads: alloc::sync::Arc::clone(&reads),
            operations,
        };
        if let Err(err) = app::watch_wake(set, &reads.wake, READS_TOKEN) {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                format_args!("reader wake wait refused ({err})"),
            );
        }
        // The places rail is what is mounted, so it converges on the mount
        // table rather than being re-read by a gesture: a newly attached
        // volume appears on its own. A refused member is fatal — the rail
        // would otherwise silently stop matching the machine.
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::Mounts.as_u32()),
            MOUNTS_TOKEN,
        ) != 0
        {
            return app::fail(APP_NAME, app::EXIT_NO_EVENTS, "mount-change wake refused");
        }

        // The machine's memory bounds what decoded artwork may be kept: one
        // query at bring-up, and a machine that cannot answer keeps a
        // screenful.
        let memory_bytes = tairix_procinfo::memory_total_for_budget(&IpcTransport);
        let icons = RefCell::new(open_icons(
            (screen_bytes(&desktop), memory_bytes),
            &reads,
            reader.is_some(),
        ));

        // --- The windows, each opened once its first listing lands. An
        // ordinary file manager was started to show a location, so a first
        // window that will not open leaves it nothing to do and it says so; a
        // component opens none until the user asks, and an empty list is a
        // perfectly good state for either to sit in afterwards.
        let mut windows: alloc::vec::Vec<OpenWindow> = alloc::vec::Vec::new();
        let opening: Opening = RefCell::new(alloc::vec::Vec::new());
        if start.role == Role::Window {
            let ladder = ladder_for(start.location, &preferences);
            let Some(first) = open_browser(&reads, ladder, &preferences) else {
                return ended(&kept, EXIT_NO_LISTING);
            };
            opening.borrow_mut().push(PendingWindow {
                vital: true,
                ..first
            });
        }

        // --- The launched-bundle bookkeeping: shared between the event
        // source (which reaps an exited bundle on a child-exit wake) and the
        // activation path below (which spawns one), so a launch and its reap
        // agree on the same in-flight set.
        // The program stores are the same for every window, so one scan serves
        // them all: the worker fills this and every quick offer, chooser and
        // open reads it, which is what keeps a click free of I/O.
        let installed: alloc::rc::Rc<Installed> = alloc::rc::Rc::new(RefCell::new(None));
        let launcher = RefCell::new(Launcher::new(
            alloc::sync::Arc::clone(&reads),
            alloc::rc::Rc::clone(&installed),
        ));
        let can_chown = holds_chown();

        // --- The event loop. A turn serves every queued event, folding what
        // each changed into its window's account; adopts what the reader has
        // answered; paints each window that owes, once; and parks only when a
        // turn found nothing at all. A dead channel ends the app fail-loud; a
        // clean close ends it at zero.
        let desktop_moved = Cell::new(false);
        let places_read: PlacesRead = RefCell::new(reads.want_places());
        let due = Cell::new(None);
        let network_rung = Cell::new(false);
        let mut browse = Browse::default();
        let mut events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(event_endpoint, server),
            set,
            launcher: &launcher,
            icons: &icons,
            reads: &reads,
            desktop_moved: &desktop_moved,
            settings: &writer,
            places_read: &places_read,
            due: &due,
            network_rung: &network_rung,
        });
        // The event a park woke for, served first by the turn after it.
        let mut woken = None;
        // The application has every alert up it may, so nothing more is put
        // up until one is answered.
        let mut alerts_full = false;
        loop {
            // Report what the icon cache holds at the head of the turn: this
            // is the one point every path through the body passes through
            // *before* anything can park (the park is inside `events.wait`
            // below), so a decode from the previous turn is never left
            // unreported while the app sits waiting. Silent unless a figure
            // actually moved.
            tairix_rt::cachereport::publish_if_due();
            // Every queued event before anything is painted, so a burst of
            // pointer samples costs one frame. Bounded to one mailbox's worth,
            // so a flood cannot hold the frame off for ever.
            let mut drained = false;
            for _ in 0..EVENT_MAILBOX_CAPACITY {
                let delivered = match woken.take() {
                    Some(event) => Ok(Some(event)),
                    None => events.try_wait(&mut client),
                };
                let event = match delivered {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        drained = true;
                        break;
                    }
                    // A malformed frame from the authenticated session is
                    // refused and the drain carries on (never guessed at).
                    Err(EventError::Undecodable(_)) => continue,
                    Err(EventError::Refused(refused)) => {
                        report_error(&alloc::format!("{refused}"));
                        continue;
                    }
                    Err(EventError::Mailbox(_)) => {
                        let code =
                            app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "event channel lost");
                        return ended(&kept, code);
                    }
                };
                // An answered alert frees one of the application's, so what
                // waited for the room may go up.
                alerts_full &= !matches!(event, WindowEvent::AlertClosed { .. });
                // The desktop belongs to the seat, not to one window, so a
                // change is adopted once and every window owes it whole.
                if adopt_desktop(&mut desktop, &mut themes, &desktop_moved) {
                    let grounds = themes.grounds(MANAGER_WINDOW_GROUND);
                    owe_desktop(&mut windows, &mut client, (&desktop, &icons), grounds);
                }
                if let Some(code) = route_event(
                    &mut windows,
                    &opening,
                    &mut client,
                    &mut desktop,
                    &mut places,
                    themes.grounds(MANAGER_WINDOW_GROUND),
                    &icons,
                    &launcher,
                    (&reads, &places_read),
                    &installed,
                    event_endpoint,
                    start.role,
                    can_chown,
                    &kept,
                    &event,
                ) {
                    return ended(&kept, code);
                }
            }

            // What landed while the input was served, folded into the windows
            // it moves. `moved` keeps the loop turning while a step produced
            // work a later step adopts, as a listing a watch re-read does.
            let mut moved = false;
            if adopt_desktop(&mut desktop, &mut themes, &desktop_moved) {
                let grounds = themes.grounds(MANAGER_WINDOW_GROUND);
                owe_desktop(&mut windows, &mut client, (&desktop, &icons), grounds);
                moved = true;
            }
            moved |= serve_due(
                &mut windows,
                &reads,
                themes.active(),
                (desktop.scale(), desktop.refresh()),
            );
            moved |= collect_writes(&mut windows, &reads, themes.active(), desktop.scale());
            moved |= adopt_settings(&mut windows, &kept);
            moved |= step_network(
                (&mut windows, &opening),
                &mut browse,
                &reads,
                &network_rung,
                &mut launcher.borrow_mut().notices,
                (themes.active(), desktop.scale()),
            );
            submit_writes(&mut windows, &reads, themes.active(), desktop.scale());
            state_rename_refusals(&mut windows);
            open_started_operations(
                &mut windows,
                &mut client,
                (&desktop, themes.grounds(MANAGER_WINDOW_GROUND), &icons),
                event_endpoint,
            );
            let pacing = (themes.active().motion(), desktop.refresh());
            adopt_operations(&mut windows, &mut client, &reads, pacing);
            moved |= step_here_operations(&mut windows, &mut client, &reads, pacing);
            if let Some(code) = open_landed_windows(
                &mut windows,
                &opening,
                &mut client,
                &desktop,
                &places,
                themes.grounds(MANAGER_WINDOW_GROUND),
                &icons,
                &reads,
                (event_endpoint, kept.borrow().live()),
            ) {
                return ended(&kept, code);
            }
            // A mount-table change the reader has answered: the rail is what
            // is mounted, so it is rebuilt and every window's copy with it.
            let landed = places_read
                .borrow_mut()
                .take()
                .or_else(|| reads.take_places());
            if let Some((home, volumes)) = landed {
                places = Places::new(&home, &volumes);
                for win in &mut windows {
                    let Some(state) = win.browser() else {
                        continue;
                    };
                    sidebar::refresh_places(&mut state.places, &home, &volumes);
                    // A listing no volume records changes for — the
                    // namespace's own roots, the mount points under
                    // `/Storage` — moves with the mount table.
                    if !state.browser.follows() && !state.browser.is_listing() {
                        if let Err(err) = state.browser.refresh() {
                            report_error(&alloc::format!("listing refused ({err})"));
                        }
                    }
                    state.browser.mark_volume_roots(&state.places);
                    // Only a listing draws the rail.
                    win.owed.owe_whole();
                }
                if start.role == Role::Desktop {
                    declare_app_bar(&mut client, event_endpoint, start.role, &places);
                }
                moved = true;
            }
            // A bundle scan the reader has answered is taken into the one
            // store every window's quick offers and chooser are built from, so
            // the next gesture reads an answer that has already landed rather
            // than waiting on a disk.
            if let Some(found) = reads.take_bundles() {
                *installed.borrow_mut() = Some(found);
                launcher.borrow_mut().scan_landed(&mut client);
            }
            // A document the reader opened goes to the application it was
            // opened for.
            while let Some(opened) = reads.take_opened() {
                launcher.borrow_mut().opened(&mut client, opened);
            }
            // A listing the reader has answered is adopted here: the browser
            // holds the navigation it could not complete, and resuming it is
            // what turns the answer into entries. A listing is a whole new set
            // of entries, so no reading of the old state describes where
            // anything is now and the window owes the whole of itself.
            for win in &mut windows {
                let mode = *win.pane.mode();
                let Some(state) = win.browser() else {
                    continue;
                };
                // A menu, a rename or a drag acts on the entries it was opened
                // over: the listing that replaces them waits.
                if state.listing_is_held() {
                    continue;
                }
                let restyled = state
                    .settings_owed
                    .take()
                    .is_some_and(|settings| state.adopt_settings(&settings));
                let replaced = match state.browser.resume() {
                    Ok(true) => {
                        state.lost = false;
                        state.browser.mark_volume_roots(&state.places);
                        state.overlays.double_click.reset();
                        state.rename_arrived(Canvas {
                            theme: themes.active(),
                            mode: &mode,
                            scale: desktop.scale(),
                            chrome: state.chrome,
                        });
                        true
                    }
                    Ok(false) => false,
                    // A folder gone from its path is left for the nearest one
                    // above rather than shown as it was.
                    Err(_) if state.lost => matches!(state.climb(), Repaint::Whole),
                    Err(err) => state.listing_refused(err),
                };
                if replaced {
                    // A new listing is a new set of entries: no click armed on
                    // the old one names any of them.
                    state.overlays.rename_arm = None;
                }
                if replaced || restyled {
                    win.owed.owe_whole();
                    moved = true;
                }
            }
            // What each window's watch reported, applied to that window alone
            // and repainted only where it altered a row.
            for win in &mut windows {
                let mode = *win.pane.mode();
                let mut damage = Region::new();
                let repaint = match win.browser() {
                    Some(state) => {
                        let canvas = Canvas {
                            theme: themes.active(),
                            mode: &mode,
                            scale: desktop.scale(),
                            chrome: state.chrome,
                        };
                        state.follow_watch(&reads, canvas, &mut damage)
                    }
                    None => Repaint::Nothing,
                };
                moved |= repaint != Repaint::Nothing;
                owe(win, repaint, &damage);
            }
            // A node's description the reader has answered, adopted by the
            // window that asked for it. Each window collects its own answer,
            // so one window's read never lands in another.
            if reads.take_properties_landed() {
                for win in &mut windows {
                    let id = win.pane.id();
                    let WindowKind::Properties(props) = &mut win.kind else {
                        continue;
                    };
                    let Some(answer) = reads.take_properties(id) else {
                        continue;
                    };
                    adopt_properties(props, answer);
                    win.owed.owe_whole();
                    moved = true;
                }
            }
            // A drive reading the reader has answered: each window repaints
            // the panels it moved, and the whole of itself only when what it
            // states moved its layout or its identity line.
            if reads.take_drives_landed() {
                let now = tairix_rt::clock_get();
                let (theme, scale) = (
                    themes.grounds(MANAGER_WINDOW_GROUND).popups,
                    desktop.scale(),
                );
                for win in &mut windows {
                    let id = win.pane.id();
                    let mode = *win.pane.mode();
                    let WindowKind::Properties(props) = &mut win.kind else {
                        continue;
                    };
                    let Some(answer) = reads.take_drive(id) else {
                        continue;
                    };
                    let detail = props.detail.clone();
                    let change = props.adopt_drive(answer, now);
                    props.refresh_detail();
                    if props.detail != detail {
                        win.owed.owe_whole();
                        moved = true;
                        continue;
                    }
                    let mut damage = Region::new();
                    let client = Rect::new(0, 0, mode.width_px, mode.height_px);
                    if properties_drive_damage(
                        props.view(),
                        props.sections(),
                        (client, scale, theme),
                        change,
                        &mut damage,
                    ) {
                        owe(win, Repaint::Reported, &damage);
                        moved = true;
                    }
                }
            }
            // A folder-cue batch the reader has answered. The answers are
            // latched onto the entries here — the worker only produced them —
            // and a window whose icons moved owes the frame that shows them;
            // a batch that answered nothing a window shows costs no frame.
            if reads.take_probes_landed() {
                for win in &mut windows {
                    let mode = *win.pane.mode();
                    let Some(state) = win.browser() else {
                        continue;
                    };
                    let canvas = Canvas {
                        theme: themes.active(),
                        mode: &mode,
                        scale: desktop.scale(),
                        chrome: state.chrome,
                    };
                    if resolve_visible_occupancy(&mut state.browser, &state.places, canvas) {
                        win.owed.owe_whole();
                        moved = true;
                    }
                }
                // Every window just asked about the folders it shows, so one
                // none of them asked about has left the screen.
                reads.sweep_probes();
            }
            // The reader nudges on its drained queue, so what landed is a
            // whole batch, and each window repaints only the pictures it moved.
            let landed = reads.take_artwork_landed();
            let arted = !landed.is_empty();
            if arted {
                moved |= adopt_artwork(
                    &mut windows,
                    &icons,
                    themes.grounds(MANAGER_WINDOW_GROUND),
                    desktop.scale(),
                    &landed,
                );
            }

            raise_alerts(&mut windows, &mut client, event_endpoint, &mut alerts_full);
            if !alerts_full {
                let notices = &mut launcher.borrow_mut().notices;
                alerts_full = raise_notice(notices, &mut client, event_endpoint, None);
            }
            let grounds = themes.grounds(MANAGER_WINDOW_GROUND);
            if present_owed_windows(&mut windows, &mut client, grounds, &icons, desktop.scale())
                .is_err()
            {
                let code = app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "present refused");
                return ended(&kept, code);
            }
            // Every window asked after each picture it shows, drawn this pass
            // or not, so a thumbnail none of them asked for is off screen.
            if arted {
                reads.sweep_thumbnails();
            }
            if moved || !drained {
                continue;
            }
            let now = tairix_rt::clock_get();
            due.set(
                windows
                    .iter_mut()
                    .filter_map(|win| match &mut win.kind {
                        WindowKind::Browser(state) => state.due(now, desktop.refresh()),
                        WindowKind::Properties(props) => props.due(now),
                        WindowKind::Progress(progress) => progress.operation.due(),
                        WindowKind::Settings(page) => page.due(now),
                    })
                    .chain(browse.retry_at())
                    .min(),
            );
            // A park the reader interrupted has no event; the next turn
            // adopts what it woke for.
            match events.wait(&mut client) {
                Ok(event) => woken = event,
                // A malformed frame from the authenticated session is refused
                // and the app keeps waiting (never guessed at).
                Err(EventError::Undecodable(_)) => {}
                Err(EventError::Refused(refused)) => report_error(&alloc::format!("{refused}")),
                Err(EventError::Mailbox(_)) => {
                    return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "event channel lost")
                }
            }
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry — the freestanding `tairix-rt` `_start` path — is not compiled, so
// this inert `main` keeps the crate building under the host tooling. It
// performs no I/O.
#[cfg(not(freestanding))]
fn main() {}
