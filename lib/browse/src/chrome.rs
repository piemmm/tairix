//! The file-manager frame *model* (`plans/NEW-FILEMANAGER.md` `FM4b`): toolbar
//! command state, derived purely from the shared [`Browser`].
//!
//! The model is what a surface asks "is Back enabled here?", so a drawn
//! toolbar and a pointer hit-test answer from one place rather than each
//! re-deriving it. The window itself carries the location — there is no path
//! bar.
//!
//! * [`ToolbarModel`] reports, from a [`Browser`], whether each
//!   [`ToolbarCommand`] is currently actionable and which view and sort are
//!   active — the enable/disable and pressed state the drawn toolbar paints.
//!   A tool whose action cannot apply renders *disabled*, never hidden, so the
//!   toolbar's shape is stable.
//! * [`ContextMenuModel`] reports, from a [`Browser`] and the app's held
//!   clipboard state, whether each [`ContextCommand`] the right-click menu
//!   offers is currently actionable, and why not when it is not
//!   ([`reason`](ContextMenuModel::reason)). [`context_menu`] turns that into
//!   the row model the desktop's own menu service renders
//!   (`plans/NEW-MENUS.md`), read back by [`context_choice_from_item`]; the
//!   file manager draws no menu pixel. The root plate holds Display ▸, the
//!   selection's own submenu (`File ‘notes.txt’ ▸`, or `Selection ▸`), New ▸
//!   ([`prepare_entry`](crate::Browser::prepare_entry)), Select All, Clear
//!   Selection and Paste ([`plan_paste`](crate::clipboard::plan_paste)); the
//!   selection's submenu holds Open
//!   ([`activate_selected`](crate::Browser::activate_selected)), Open With…
//!   (the [`open_with`](crate::open_with) chooser over a regular file), Get
//!   Info… ([`Properties`](crate::properties::Properties)), Rename
//!   ([`prepare_rename`](crate::Browser::prepare_rename)), Cut/Copy
//!   ([`clipboard`](crate::Browser::clipboard)) and Delete
//!   ([`plan_delete`](crate::Browser::plan_delete)).
//!
//! The model decides *what is offered*; it performs no navigation or I/O
//! itself, so composing it grants nothing (the read-only picker builds the
//! same model and simply never invokes a write action).

use alloc::string::String;

use tairix_abi::window_ipc::{
    AppMenu, AppMenuBundle, AppMenuEntry, AppMenuEntryText, AppMenuItem, AppMenuItemId,
    AppMenuLabel, AppMenuMark, AppMenuReason, AppMenuRole, AppMenuRow, AppMenuShortcut,
    AppMenuSymbol, APP_MENU_LABEL_MAX,
};
use tairix_abi::Errno;
use tairix_icon::IconKind;

use crate::browser::Browser;
use crate::entry::{Entry, EntryKind, LinkTarget};
use crate::error::BrowseError;
use crate::layout::ViewMode;
use crate::media::BlankDocument;
use crate::open_with::{AppAssociation, OPEN_WITH_QUICK_MAX};
use crate::presentation::HiddenEndings;
use crate::render::ListColumns;
use crate::sort::{SortDirection, SortKey, SortMode};
use crate::source::DirectorySource;
use crate::vfs::push_shown_name;

/// A read-only file-manager toolbar command.
///
/// Each variant maps to a `Browser` operation the drawn `lib/controls`
/// `Toolbar` binds an `IconButton` (with a keyboard equivalent) to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ToolbarCommand {
    /// Return to the previous directory in the navigation history
    /// ([`go_back`](Browser::go_back)).
    Back,
    /// Advance to the next directory in the navigation history
    /// ([`go_forward`](Browser::go_forward)).
    Forward,
    /// Climb to the parent directory ([`go_up`](Browser::go_up)).
    Up,
    /// Re-read the current directory ([`refresh`](Browser::refresh)).
    Refresh,
    /// Step to the next display — large icons, small icons, the list
    /// ([`set_view_mode`](Browser::set_view_mode)).
    ToggleView,
    /// Change the listing sort order ([`set_sort_mode`](Browser::set_sort_mode)).
    Sort,
}

/// The complete set of toolbar commands, in their left-to-right toolbar order.
///
/// The drawn chrome iterates this so a new command is one entry here rather
/// than a hand-maintained list duplicated per surface.
pub const TOOLBAR_COMMANDS: &[ToolbarCommand] = &[
    ToolbarCommand::Back,
    ToolbarCommand::Forward,
    ToolbarCommand::Up,
    ToolbarCommand::Refresh,
    ToolbarCommand::ToggleView,
    ToolbarCommand::Sort,
];

impl ToolbarCommand {
    /// The built-in glyph the drawn toolbar paints for this command.
    ///
    /// One definition so the toolbar's icon set and the icon vocabulary can
    /// never drift; adding a command adds its glyph here.
    #[must_use]
    pub const fn icon(self) -> IconKind {
        match self {
            Self::Back => IconKind::NavBack,
            Self::Forward => IconKind::NavForward,
            Self::Up => IconKind::NavUp,
            Self::Refresh => IconKind::Refresh,
            Self::ToggleView => IconKind::ViewToggle,
            Self::Sort => IconKind::Sort,
        }
    }
}

/// Apply the toolbar `command` to `browser`, returning whether the view
/// changed (and must be re-presented).
///
/// Every command is a read-only navigation or presentation action, so the
/// trusted read-only picker composes the same [`Browser`] and can invoke this
/// too — it grants no authority. Back/Forward/Up/Refresh are the browser's own
/// transactional, fail-closed navigation (a refused re-listing leaves the
/// browser exactly where it was); the view toggle and the sort cycle are pure
/// rearrangements that always change the view. The caller reveals the
/// selection and repaints when this reports a change.
///
/// # Errors
///
/// Returns [`BrowseError::Source`] when a navigation command's target
/// directory can no longer be listed; the browser is left untouched.
pub fn apply_command<S: DirectorySource>(
    browser: &mut Browser<S>,
    command: ToolbarCommand,
) -> Result<bool, BrowseError> {
    match command {
        ToolbarCommand::Back => browser.go_back(),
        ToolbarCommand::Forward => browser.go_forward(),
        ToolbarCommand::Up => browser.go_up(),
        ToolbarCommand::Refresh => browser.refresh().map(|()| true),
        ToolbarCommand::ToggleView => {
            browser.set_view_mode(browser.view_mode().next());
            Ok(true)
        }
        ToolbarCommand::Sort => {
            browser.set_sort_mode(browser.sort_mode().next());
            Ok(true)
        }
    }
}

/// The enable state and pressed state of the file-manager toolbar, taken from
/// a [`Browser`].
///
/// A snapshot: the drawn toolbar rebuilds it whenever the browser's state
/// changes and paints each [`ToolbarCommand`] enabled or disabled from
/// [`is_enabled`](Self::is_enabled), reflecting the active view and sort from
/// [`view_mode`](Self::view_mode) / [`sort_mode`](Self::sort_mode).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ToolbarModel {
    back: bool,
    forward: bool,
    up: bool,
    view_mode: ViewMode,
    sort_mode: SortMode,
}

impl ToolbarModel {
    /// Every command actionable — the state a *measurement* of the strip is
    /// taken against.
    ///
    /// A disabled tool renders in place (muted, never hidden), so a tool's
    /// width is independent of its enable state and the widest strip is the
    /// one with every tool in it. The same reasoning
    /// [`ManagerToolModel::new(true)`](ManagerToolModel::new) is used under.
    #[must_use]
    pub fn all_enabled() -> Self {
        Self {
            back: true,
            forward: true,
            up: true,
            view_mode: ViewMode::default(),
            sort_mode: SortMode::default(),
        }
    }

    /// Build the toolbar state from `browser`.
    ///
    /// Back/Forward reflect the navigation history
    /// ([`can_go_back`](Browser::can_go_back) /
    /// [`can_go_forward`](Browser::can_go_forward)); Up reflects whether there
    /// is a parent to climb to ([`can_go_up`](Browser::can_go_up)); Refresh,
    /// the view toggle, and sort are always actionable.
    #[must_use]
    pub fn for_browser<S: DirectorySource>(browser: &Browser<S>) -> Self {
        Self {
            back: browser.can_go_back(),
            forward: browser.can_go_forward(),
            up: browser.can_go_up(),
            view_mode: browser.view_mode(),
            sort_mode: browser.sort_mode(),
        }
    }

    /// Whether `command` is currently actionable and should render enabled.
    ///
    /// [`Refresh`](ToolbarCommand::Refresh),
    /// [`ToggleView`](ToolbarCommand::ToggleView), and
    /// [`Sort`](ToolbarCommand::Sort) are always available; the three
    /// navigation commands depend on the browser's history and depth.
    #[must_use]
    pub fn is_enabled(&self, command: ToolbarCommand) -> bool {
        match command {
            ToolbarCommand::Back => self.back,
            ToolbarCommand::Forward => self.forward,
            ToolbarCommand::Up => self.up,
            ToolbarCommand::Refresh | ToolbarCommand::ToggleView | ToolbarCommand::Sort => true,
        }
    }

    /// The item view currently shown — the pressed state of the view toggle.
    #[must_use]
    pub const fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// The listing sort order currently in force — what the sort control shows.
    #[must_use]
    pub const fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }
}

/// A file-manager **write** tool — a manager-only toolbar action that mutates
/// the filesystem, deliberately kept in a vocabulary separate from the
/// read-only [`ToolbarCommand`] set.
///
/// The shared read-only toolbar ([`ToolbarCommand`] / [`apply_command`]) is
/// composed by *both* the file manager and the trusted read-only file picker,
/// so it must never carry an action that writes. Write tools live here instead:
/// only a write-capable consumer (the file manager) renders them — by handing
/// [`MANAGER_TOOLS`] to the renderer — and dispatches them in its own
/// capability-checked tail, under the user's own identity (no new capability;
/// the per-inode permission model gates the write). The picker hands the
/// renderer no write tools and therefore cannot express one: the separation is
/// enforced by the type system, not a runtime flag.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ManagerTool {
    /// Create a new folder in the current directory
    /// ([`prepare_entry`](Browser::prepare_entry)), which the file manager then
    /// opens the inline rename on.
    NewFolder,
    /// Go to the user's Trash — the navigable Trash location
    /// (`plans/NEW-FILEMANAGER.md` `FM11`). The file manager navigates the
    /// [`Browser`] to the `Library/Trash` subtree
    /// ([`navigate_to`](Browser::navigate_to)); it is a manager-only tool (not
    /// the picker's) because a trusted read-only pick has no business managing
    /// the Trash. Always offered — a Trash that cannot be reached is reported
    /// as a refusal, not hidden.
    Trash,
    /// Empty the user's Trash — permanently remove its contents
    /// ([`empty_trash_plan`](crate::trash::empty_trash_plan) carried out by
    /// the recursive removal a delete uses). Offered only when the
    /// current directory *is* the user's Trash and it is non-empty (the
    /// [`ManagerToolModel`] the file manager builds); it renders disabled
    /// elsewhere, never hidden, so the toolbar's shape is stable.
    EmptyTrash,
}

/// Whether a browse view draws its command toolbar.
///
/// The strip is chrome the *view* owns, not a fixed part of the layout, so
/// every measurement of the listing takes this and paint and hit-test can
/// never disagree about whether the band is there. Hidden means hidden: the
/// band reserves no height and no point in the window resolves to a toolbar
/// command or a manager tool.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ToolbarBand {
    /// Drawn across the top of the window, reserving its band above the
    /// listing and the places rail.
    Shown,
    /// Not drawn: the listing starts at the top of the window.
    Hidden,
}

impl ToolbarBand {
    /// Whether the strip is drawn and reserves its band.
    #[must_use]
    pub const fn is_shown(self) -> bool {
        matches!(self, Self::Shown)
    }

    /// The other state, for a toggle.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Shown => Self::Hidden,
            Self::Hidden => Self::Shown,
        }
    }
}

/// The complete set of manager-only write tools, in their left-to-right toolbar
/// order (drawn after the shared read-only [`TOOLBAR_COMMANDS`]).
///
/// The file manager hands this to the renderer; the read-only picker hands an
/// empty slice, so it never draws or dispatches a write tool. New Folder first,
/// then the Trash location and the Empty Trash verb — grouped so the
/// Trash-related tools read together.
pub const MANAGER_TOOLS: &[ManagerTool] = &[
    ManagerTool::NewFolder,
    ManagerTool::Trash,
    ManagerTool::EmptyTrash,
];

impl ManagerTool {
    /// The built-in glyph the drawn toolbar paints for this tool (one
    /// definition, mirroring [`ToolbarCommand::icon`]).
    #[must_use]
    pub const fn icon(self) -> IconKind {
        match self {
            Self::NewFolder => IconKind::NewFolder,
            Self::Trash => IconKind::Trash,
            Self::EmptyTrash => IconKind::EmptyTrash,
        }
    }
}

/// The enable state of the manager-only write tools, since some depend on
/// context the [`Browser`] alone does not carry.
///
/// [`NewFolder`](ManagerTool::NewFolder) and [`Trash`](ManagerTool::Trash) are
/// always actionable, but [`EmptyTrash`](ManagerTool::EmptyTrash) is offered
/// only when the current directory *is* the user's Trash and it is non-empty —
/// a fact the file manager computes from the user's `HOME` (which the engine
/// does not know), so it is threaded in here rather than derived from the
/// browser. A disabled tool renders muted, never hidden, and a click on it
/// resolves to nothing (fail closed).
///
/// The trusted read-only picker draws no write tools at all, so it uses
/// [`none`](Self::none); the model then never enables anything.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ManagerToolModel {
    can_empty_trash: bool,
    can_create: bool,
}

impl ManagerToolModel {
    /// Build the model for a file manager whose current directory is (or is
    /// not) the user's non-empty Trash.
    ///
    /// `can_empty_trash` is the file manager's own computed answer to "is the
    /// current directory the user's Trash, and does it hold anything?" — the
    /// one gate on offering [`ManagerTool::EmptyTrash`].
    #[must_use]
    pub const fn new(can_empty_trash: bool) -> Self {
        Self {
            can_empty_trash,
            can_create: true,
        }
    }

    /// The model for a file manager showing `browser`, whose Trash it may
    /// empty when `can_empty_trash`: nothing is made where no folder is shown.
    #[must_use]
    pub fn for_browser<S: DirectorySource>(browser: &Browser<S>, can_empty_trash: bool) -> Self {
        Self {
            can_empty_trash,
            can_create: browser.folder().is_some(),
        }
    }

    /// The model for a consumer that draws no write tools (the trusted
    /// read-only picker): nothing is enabled because nothing is drawn.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            can_empty_trash: false,
            can_create: false,
        }
    }

    /// Whether `tool` is currently actionable and should render enabled.
    ///
    /// [`Trash`](ManagerTool::Trash) is always available;
    /// [`NewFolder`](ManagerTool::NewFolder) wherever a folder is shown; and
    /// [`EmptyTrash`](ManagerTool::EmptyTrash) only when the model was built
    /// for a non-empty Trash (the `can_empty_trash` argument to
    /// [`new`](Self::new)).
    #[must_use]
    pub const fn is_enabled(&self, tool: ManagerTool) -> bool {
        match tool {
            ManagerTool::NewFolder => self.can_create,
            ManagerTool::Trash => true,
            ManagerTool::EmptyTrash => self.can_empty_trash,
        }
    }
}

/// A file-manager context-menu command: one chooseable row of the menu.
///
/// The verbs that act on the selection sit on the plate its own row opens
/// ([`acts_on_selection`](Self::acts_on_selection)); the rest act on the folder
/// and sit on the root plate. Display ▸ and New ▸ are not among them: each is a
/// submenu, never chosen itself, whose rows answer as
/// [`ContextChoice::Display`], [`ContextChoice::NewFolder`] and
/// [`ContextChoice::NewDocument`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ContextCommand {
    /// Activate the selected entry — descend, launch a bundle, or open a file
    /// ([`activate_selected`](Browser::activate_selected)).
    Open,
    /// Choose an application to open the selected regular file with, from the
    /// installed bundles whose declared associations claim the file's type
    /// ([`applications_for`](crate::open_with::applications_for)). Offered only
    /// for a regular file — a directory descends and a bundle launches itself,
    /// so neither has an application to choose.
    OpenWith,
    /// Rename the selected entry in place
    /// ([`prepare_rename`](Browser::prepare_rename)).
    Rename,
    /// Open the selected entry's Info window: what it is, its permissions and
    /// its attributes ([`Properties`](crate::properties::Properties)).
    GetInfo,
    /// Capture the selection onto a move clipboard
    /// ([`clipboard`](Browser::clipboard) with [`ClipboardOp::Cut`](crate::clipboard::ClipboardOp::Cut)).
    Cut,
    /// Capture the selection onto a copy clipboard
    /// ([`clipboard`](Browser::clipboard) with [`ClipboardOp::Copy`](crate::clipboard::ClipboardOp::Copy)).
    Copy,
    /// Paste the held clipboard into the current directory
    /// ([`plan_paste`](crate::clipboard::plan_paste)).
    Paste,
    /// Select every entry the listing holds ([`select_all`](Browser::select_all)).
    SelectAll,
    /// Drop the whole selection ([`clear_selection`](Browser::clear_selection)).
    ClearSelection,
    /// Delete the selected entry — the modal-confirmed recursive removal the
    /// app plans through [`plan_delete`](Browser::plan_delete) and carries out
    /// through the shared engine (`lib/fscopy`). Acts on the selection, so it
    /// is offered only when one exists.
    Delete,
}

/// The complete set of context-menu commands: the selection's plate top to
/// bottom, then the root plate's after its submenus.
///
/// The drawn menu iterates this so a new command is one entry here rather than
/// a hand-maintained list duplicated per surface (mirrors
/// [`TOOLBAR_COMMANDS`]).
pub const CONTEXT_COMMANDS: &[ContextCommand] = &[
    ContextCommand::Open,
    ContextCommand::OpenWith,
    ContextCommand::GetInfo,
    ContextCommand::Rename,
    ContextCommand::Cut,
    ContextCommand::Copy,
    ContextCommand::Delete,
    ContextCommand::SelectAll,
    ContextCommand::ClearSelection,
    ContextCommand::Paste,
];

impl ContextCommand {
    /// The label the drawn menu row shows for this command.
    ///
    /// One definition so the menu text has a single source.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::OpenWith => "Open With\u{2026}",
            Self::GetInfo => "Get Info\u{2026}",
            Self::Rename => "Rename",
            Self::Cut => "Cut",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::SelectAll => "Select All",
            Self::ClearSelection => "Clear Selection",
            Self::Delete => "Delete",
        }
    }

    /// The keyboard-equivalent caption the drawn menu row shows for this
    /// command, matching the accelerator the app's navigation keys bind, so
    /// the menu advertises exactly the key that also drives the verb.
    #[must_use]
    pub const fn shortcut(self) -> &'static str {
        match self {
            Self::Open => "Enter",
            // Open With… binds no keyboard accelerator, so it advertises none;
            // the menu's own traversal is how a keyboard reaches it.
            Self::OpenWith => "",
            Self::GetInfo => "Alt+Enter",
            Self::Rename => "F2",
            Self::Cut => "Ctrl+X",
            Self::Copy => "Ctrl+C",
            Self::Paste => "Ctrl+V",
            Self::SelectAll => "Ctrl+A",
            Self::ClearSelection => "Ctrl+Shift+A",
            Self::Delete => "Delete",
        }
    }

    /// Whether this command acts on the selection, and so sits on the plate
    /// the selection's own row opens rather than on the root plate.
    #[must_use]
    pub const fn acts_on_selection(self) -> bool {
        matches!(
            self,
            Self::Open
                | Self::OpenWith
                | Self::GetInfo
                | Self::Rename
                | Self::Cut
                | Self::Copy
                | Self::Delete
        )
    }

    /// Whether this command begins a new visual group on its plate, so the
    /// menu draws a divider above it.
    ///
    /// On the selection's plate: the opening commands, then what the item
    /// *is*, then the editing verbs, then the removal on its own. On the root
    /// plate, after the submenus: the selection as a whole, then the paste.
    const fn opens_group(self) -> bool {
        matches!(
            self,
            Self::GetInfo | Self::Rename | Self::Delete | Self::SelectAll | Self::Paste
        )
    }

    /// Whether carrying this command out destroys something, so its row draws
    /// with the destructive emphasis rather than the neutral one.
    const fn is_destructive(self) -> bool {
        matches!(self, Self::Delete)
    }

    /// The function this command's row shows.
    #[must_use]
    pub const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::Open => AppMenuSymbol::Open,
            Self::OpenWith => AppMenuSymbol::OpenWith,
            Self::GetInfo => AppMenuSymbol::Properties,
            Self::Rename => AppMenuSymbol::Rename,
            Self::Cut => AppMenuSymbol::Cut,
            Self::Copy => AppMenuSymbol::Copy,
            Self::Paste => AppMenuSymbol::Paste,
            Self::SelectAll => AppMenuSymbol::SelectAll,
            Self::ClearSelection => AppMenuSymbol::SelectNone,
            Self::Delete => AppMenuSymbol::Delete,
        }
    }
}

/// What a chosen row of the file manager's context menu asks for.
///
/// An id that means "the user typed a name" must not be readable as one that
/// means "the user clicked Rename", so the ids are distinct by construction
/// and this is their exact inverse.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ContextChoice {
    /// One of the menu's command rows was chosen.
    Command(ContextCommand),
    /// The Rename row's quick-entry field was committed. The caller pulls the
    /// text the user typed and renames with it.
    RenameCommit,
    /// The candidate at this index of the list the menu was built from — the
    /// quick candidates, in their ranked order — was chosen from the
    /// "Open With…" submenu.
    OpenWithCandidate(usize),
    /// New ▸ Folder was chosen.
    NewFolder,
    /// The document at this index of the list the menu was built from was
    /// chosen from New ▸.
    NewDocument(usize),
    /// This display was chosen from Display ▸.
    Display(ViewMode),
    /// The listing is to be ordered by this key, chosen from Display ▸.
    SortBy(SortKey),
    /// The listing is to be ordered in this direction, chosen from Display ▸.
    SortIn(SortDirection),
}

/// The id the Rename row's quick-entry field answers with: the one past the
/// commands.
const RENAME_COMMIT: usize = CONTEXT_COMMANDS.len();

/// The first quick candidate's id. The block is as long as a plate offers
/// candidates, so the ids after it do not move with how many there are.
const FIRST_CANDIDATE: usize = RENAME_COMMIT + 1;

/// The id New ▸ Folder answers with.
const NEW_FOLDER: usize = FIRST_CANDIDATE + OPEN_WITH_QUICK_MAX;

/// The id the first of Display ▸'s displays answers with, the rest following
/// in [`ViewMode::ALL`] order. They come before the documents, whose count is
/// the machine's, so no display's id moves with what is installed.
const FIRST_DISPLAY: usize = NEW_FOLDER + 1;

/// The id the first of Display ▸'s sort keys answers with, the rest following
/// in [`SortKey::ALL`] order whichever of them a listing offers.
const FIRST_SORT_KEY: usize = FIRST_DISPLAY + ViewMode::ALL.len();

/// The id the first of Display ▸'s sort directions answers with, the other
/// following in [`SortDirection::ALL`] order.
const FIRST_SORT_DIRECTION: usize = FIRST_SORT_KEY + SortKey::ALL.len();

/// The first New ▸ document's id.
const FIRST_DOCUMENT: usize = FIRST_SORT_DIRECTION + SortDirection::ALL.len();

/// New ▸'s label, which also titles its plate.
const NEW_LABEL: &str = "New";

/// New ▸ Folder's label.
const NEW_FOLDER_LABEL: &str = "Folder";

/// Display ▸'s label, which also titles its plate.
const DISPLAY_LABEL: &str = "Display";

/// Why the display in force cannot be chosen again.
const REASON_CURRENT_DISPLAY: &str = "the window shows its items this way";

/// Why the order in force cannot be chosen again.
const REASON_CURRENT_ORDER: &str = "the window is sorted this way";

/// The selection row's label while it names no one item.
const SELECTION_LABEL: &str = "Selection";

/// The quotes the selection row sets one item's name between.
const NAME_QUOTES: (char, char) = ('\u{2018}', '\u{2019}');

/// Why a verb that acts on the selection, or the plate holding them, cannot
/// be used.
const REASON_NO_SELECTION: &str = "nothing selected";

impl ViewMode {
    /// The label Display ▸ offers this display under.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LargeIcons => "Large icons",
            Self::SmallIcons => "Small icons",
            Self::List => "List",
        }
    }

    /// The function this display's row shows.
    #[must_use]
    pub const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::LargeIcons => AppMenuSymbol::LargeIcons,
            Self::SmallIcons => AppMenuSymbol::SmallIcons,
            Self::List => AppMenuSymbol::List,
        }
    }
}

impl SortKey {
    /// The label Display ▸ offers ordering by this key under.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Sort by Name",
            Self::Kind => "Sort by Type",
            Self::Size => "Sort by Size",
            Self::Modified => "Sort by Date Modified",
            Self::Free => "Sort by Free Space",
            Self::Filesystem => "Sort by Filesystem",
            Self::Health => "Sort by Health",
            Self::Address => "Sort by Address",
            Self::Workgroup => "Sort by Workgroup",
        }
    }

    /// The function this key's row shows.
    #[must_use]
    pub const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::Name => AppMenuSymbol::SortByName,
            Self::Kind => AppMenuSymbol::SortByKind,
            Self::Size => AppMenuSymbol::SortBySize,
            Self::Modified => AppMenuSymbol::SortByDate,
            Self::Free => AppMenuSymbol::SortByFree,
            Self::Filesystem => AppMenuSymbol::SortByFilesystem,
            Self::Health => AppMenuSymbol::SortByHealth,
            Self::Address => AppMenuSymbol::SortByAddress,
            Self::Workgroup => AppMenuSymbol::SortByWorkgroup,
        }
    }
}

impl SortDirection {
    /// The label Display ▸ offers this direction under.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ascending => "Ascending",
            Self::Descending => "Descending",
        }
    }

    /// The function this direction's row shows.
    #[must_use]
    pub const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::Ascending => AppMenuSymbol::SortAscending,
            Self::Descending => AppMenuSymbol::SortDescending,
        }
    }
}

/// The accelerator that also makes a new folder.
const NEW_FOLDER_SHORTCUT: &str = "Ctrl+Shift+N";

/// The choice the chosen row `item` names, or `None` for an id this menu never
/// declared (fail closed — an outcome is never guessed at).
///
/// The exact inverse of the numbering [`context_menu`] assigns: the commands
/// in [`CONTEXT_COMMANDS`] order, the Rename row's field, the quick
/// candidates, New ▸ Folder, the displays, every sort key, both sort
/// directions, then the New ▸ documents. A candidate's or a document's index
/// is its position in the list the menu was *given*, not among the rows that
/// fitted, and a key's is its place in [`SortKey::ALL`], not among the keys a
/// listing offers, so neither a row the plate could not seat nor a listing of
/// drives shifts another row's meaning.
#[must_use]
pub fn context_choice_from_item(item: AppMenuItemId) -> Option<ContextChoice> {
    let index = item.index();
    if let Some(command) = CONTEXT_COMMANDS.get(index) {
        return Some(ContextChoice::Command(*command));
    }
    Some(match index {
        RENAME_COMMIT => ContextChoice::RenameCommit,
        NEW_FOLDER => ContextChoice::NewFolder,
        _ if index < NEW_FOLDER => ContextChoice::OpenWithCandidate(index - FIRST_CANDIDATE),
        _ if index < FIRST_SORT_KEY => {
            ContextChoice::Display(*ViewMode::ALL.get(index - FIRST_DISPLAY)?)
        }
        _ if index < FIRST_SORT_DIRECTION => {
            ContextChoice::SortBy(*SortKey::ALL.get(index - FIRST_SORT_KEY)?)
        }
        _ if index < FIRST_DOCUMENT => {
            ContextChoice::SortIn(*SortDirection::ALL.get(index - FIRST_SORT_DIRECTION)?)
        }
        _ => ContextChoice::NewDocument(index - FIRST_DOCUMENT),
    })
}

/// The id at `index` of the numbering, refused past the id space.
fn item_id(index: usize) -> Result<AppMenuItemId, Errno> {
    AppMenuItemId::for_index(index).ok_or(Errno::OutOfRange)
}

/// What the context menu offers beyond its commands: the chosen entry, the
/// applications that can open it, and the documents New ▸ can make.
///
/// All three are things the *caller* already holds — the browser's chosen
/// entry, and a bundle scan it keeps warm on a worker — so building the menu
/// reads no file and a right-click waits on nothing. Any may be empty: the
/// selection's row then says only what is selected, the Rename row opens only
/// the in-place editor, the "Open With…" row carries no chevron and opens the
/// chooser, and New ▸ offers only Folder.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextQuick<'a> {
    /// The one chosen entry, whose name the selection's row names and the
    /// Rename row's field starts out holding as
    /// [`rename_text`](crate::rename_text) gives it.
    pub chosen: Option<&'a Entry>,
    /// The applications that can open the selection, highest-ranked first
    /// ([`quick_applications`](crate::open_with::quick_applications)); a plate
    /// offers at most [`OPEN_WITH_QUICK_MAX`].
    pub candidates: &'a [&'a AppAssociation],
    /// The documents New ▸ offers after Folder
    /// ([`blank_documents`](crate::open_with::blank_documents)).
    pub documents: &'a [BlankDocument],
}

/// Build the row model a secondary press asks the desktop to open, under the
/// root `title`: Display ▸ (the views, then the listing's orders), then the
/// selection's own submenu and New ▸, then
/// Select All and Clear Selection, then Paste. The selection's submenu holds
/// the verbs that act on it ([`ContextCommand::acts_on_selection`]) and is
/// labelled with what it acts on — `File ‘notes.txt’`, the name shortened to
/// fit with its extension kept, or `Selection` for several items.
///
/// A command the `model` reports inactionable is declared **disabled with its
/// reason** rather than left out, so a plate's shape does not move with the
/// selection and a row says why it cannot be chosen; with nothing selected the
/// selection's submenu is itself disabled with its reason and holds nothing.
/// Removal declares the destructive emphasis.
///
/// The Rename row additionally carries a **quick-entry field** pre-filled with
/// the selection's name, and the "Open With…" row a **submenu** of the
/// compatible applications, each naming the bundle its icon comes from. Both
/// are offered only where the row itself is actionable, so a field can never
/// commit a rename the model says cannot happen (fail closed); a name the field
/// cannot hold leaves the row opening the in-place editor instead. New ▸ always
/// opens, onto Folder and then `quick`'s documents: it acts on the directory,
/// and whether the directory takes a new entry is the VFS's answer at create
/// time. The commands and Display ▸'s fixed rows are pushed before New ▸'s
/// documents and the candidates, so neither a long candidate list nor the
/// documents can crowd a fixed row off the plate; a row the text budget cannot
/// seat is left out, and a candidate whose path will not fit keeps its row and
/// loses only its picture.
///
/// The menu performs nothing — the caller dispatches the chosen row in its own
/// capability-checked tail — so composing it grants no authority; the
/// read-only picker opens no write context menu, so it builds none.
///
/// # Errors
///
/// Any [`Errno`] the shared menu bounds refuse: a `title` that is not
/// admissible display text, or rows that do not fit the format bounds. The
/// commands are fixed and a name is shown in a form the bounds always admit,
/// so a refusal past the title can only mean those bounds changed under this
/// menu; the caller reports it and opens nothing rather than showing a menu it
/// could not describe.
pub fn context_menu(
    model: ContextMenuModel,
    title: &str,
    quick: ContextQuick<'_>,
) -> Result<AppMenu, Errno> {
    let mut menu = AppMenu::titled(AppMenuLabel::new(title)?);
    let name = quick.chosen.map_or("", |entry| model.endings.shown(entry));
    let display_row = push_submenu(
        &mut menu,
        AppMenuLabel::new(DISPLAY_LABEL)?,
        AppMenuSymbol::Display,
        "",
    )?;
    menu.push(AppMenuRow::Separator)?;
    let selected = model.selected > 0;
    let selection_row = push_submenu(
        &mut menu,
        selection_label(model.chosen, name)?,
        selection_symbol(model.chosen),
        if selected { "" } else { REASON_NO_SELECTION },
    )?;
    let new_row = push_submenu(
        &mut menu,
        AppMenuLabel::new(NEW_LABEL)?,
        AppMenuSymbol::NewDocument,
        "",
    )?;
    let mut open_with_row = None;
    for (index, command) in CONTEXT_COMMANDS.iter().copied().enumerate() {
        let plate = if command.acts_on_selection() {
            // A plate that never opens holds nothing.
            if !selected {
                continue;
            }
            Some(selection_row)
        } else {
            None
        };
        if command.opens_group() {
            push_on(&mut menu, &AppMenuRow::Separator, plate)?;
        }
        let reason = model.reason(command);
        let enabled = reason.is_empty();
        let item = command_item(index, command, reason, (quick.chosen, model.endings))?;
        push_on(&mut menu, &AppMenuRow::Item(item), plate)?;
        if command == ContextCommand::OpenWith && enabled && !quick.candidates.is_empty() {
            open_with_row = Some(menu.len() - 1);
        }
    }
    push_display_rows(&mut menu, display_row, model.display)?;
    push_sort_rows(&mut menu, display_row, model.sort, model.sort_keys)?;
    push_new_rows(&mut menu, new_row, quick.documents)?;
    if let Some(parent) = open_with_row {
        push_candidates(&mut menu, parent, quick.candidates)?;
    }
    Ok(menu)
}

/// Build the row model a secondary press over the network asks the desktop to
/// open, under the root `title`: Display ▸ (the views, then the network's
/// orders), then Open for the one item chosen.
///
/// The network has no path, so nothing that writes, copies or creates is
/// offered; the ids are [`context_menu`]'s, so [`context_choice_from_item`]
/// reads either.
///
/// # Errors
///
/// Any [`Errno`] the shared menu bounds refuse, as [`context_menu`]'s.
pub fn network_context_menu<S: DirectorySource>(
    browser: &Browser<S>,
    title: &str,
) -> Result<AppMenu, Errno> {
    let mut menu = AppMenu::titled(AppMenuLabel::new(title)?);
    let display_row = push_submenu(
        &mut menu,
        AppMenuLabel::new(DISPLAY_LABEL)?,
        AppMenuSymbol::Display,
        "",
    )?;
    menu.push(AppMenuRow::Separator)?;
    let open = CONTEXT_COMMANDS
        .iter()
        .position(|&command| command == ContextCommand::Open)
        .ok_or(Errno::OutOfRange)?;
    let item = command_item(
        open,
        ContextCommand::Open,
        network_open_reason(browser),
        (None, browser.presentation().endings),
    )?;
    menu.push(AppMenuRow::Item(item))?;
    push_display_rows(&mut menu, display_row, browser.view_mode())?;
    push_sort_rows(
        &mut menu,
        display_row,
        browser.sort_mode(),
        ListColumns::Network.keys(),
    )?;
    Ok(menu)
}

/// Why Open cannot act on what is chosen over the network, or `""` when it
/// can.
fn network_open_reason<S: DirectorySource>(browser: &Browser<S>) -> &'static str {
    match (browser.chosen_network_item(), browser.selection().len()) {
        (Some(item), _) => match item.endpoint() {
            Some(endpoint) if endpoint.is_available() => "",
            Some(_) => "it cannot be reached now",
            None => "there is nothing to open",
        },
        (None, 0) => REASON_NO_SELECTION,
        (None, _) => "several items selected",
    }
}

/// The row for `command`, numbered by its `index` in [`CONTEXT_COMMANDS`]:
/// disabled with `reason` when there is one, and — for an actionable Rename —
/// carrying a quick-entry field holding `chosen`'s name as `endings` show it,
/// its stem selected ([`rename_text`](crate::rename_text)), when the field can
/// hold it.
///
/// # Errors
///
/// Any [`Errno`] the shared bounds refuse for the command's own fixed text.
fn command_item(
    index: usize,
    command: ContextCommand,
    reason: &str,
    (chosen, endings): (Option<&Entry>, HiddenEndings),
) -> Result<AppMenuItem, Errno> {
    let mut item = AppMenuItem::new(
        item_id(index)?,
        AppMenuLabel::new(command.label())?,
        command.symbol(),
    )
    .with_shortcut(AppMenuShortcut::new(command.shortcut())?);
    if command.is_destructive() {
        item = item.with_role(AppMenuRole::Destructive);
    }
    if !reason.is_empty() {
        return Ok(item.disabled().with_reason(AppMenuReason::new(reason)?));
    }
    if command == ContextCommand::Rename {
        let field = chosen.and_then(|entry| {
            let (text, selected) = crate::rename_text(entry, endings);
            AppMenuEntryText::new(text)
                .ok()
                .filter(|initial| !initial.is_empty())
                .map(|initial| (initial, selected))
        });
        if let Some((initial, selected)) = field {
            item = item.with_entry(
                AppMenuEntry::new(item_id(RENAME_COMMIT)?, initial).with_selection(selected)?,
            );
        }
    }
    Ok(item)
}

/// Push `row` onto the root plate, or under the row at `parent`.
fn push_on(menu: &mut AppMenu, row: &AppMenuRow, parent: Option<usize>) -> Result<(), Errno> {
    match parent {
        Some(parent) => menu.push_under(*row, parent),
        None => menu.push(*row),
    }
}

/// Push a submenu row onto the root plate, disabled with `reason` when there
/// is one, answering its index for the rows it opens to name.
fn push_submenu(
    menu: &mut AppMenu,
    label: AppMenuLabel,
    symbol: AppMenuSymbol,
    reason: &str,
) -> Result<usize, Errno> {
    menu.push(AppMenuRow::Submenu {
        label,
        enabled: reason.is_empty(),
        symbol,
        reason: AppMenuReason::new(reason)?,
    })?;
    Ok(menu.len() - 1)
}

/// The selection's row label: the one chosen item's kind and its name,
/// shortened to the label bound with its extension kept — `File ‘notes.txt’`
/// — or [`SELECTION_LABEL`] when no one item is chosen.
///
/// # Errors
///
/// None in practice: the name is spelled as a reader is shown it, which the
/// bound always admits.
fn selection_label(chosen: Option<EntryKind>, name: &str) -> Result<AppMenuLabel, Errno> {
    let Some(kind) = chosen.filter(|_| !name.is_empty()) else {
        return AppMenuLabel::new(SELECTION_LABEL);
    };
    let (opens, closes) = NAME_QUOTES;
    let mut label = String::with_capacity(APP_MENU_LABEL_MAX);
    label.push_str(kind.noun());
    label.push(' ');
    label.push(opens);
    let budget = APP_MENU_LABEL_MAX
        .saturating_sub(label.len())
        .saturating_sub(closes.len_utf8());
    push_shown_name(&mut label, name, budget);
    label.push(closes);
    AppMenuLabel::new(&label)
}

/// The function the selection's row shows: a folder for a folder, an
/// application for an application, the item menu for anything else.
const fn selection_symbol(chosen: Option<EntryKind>) -> AppMenuSymbol {
    match chosen {
        Some(EntryKind::Directory | EntryKind::Link(LinkTarget::Directory)) => {
            AppMenuSymbol::Folder
        }
        Some(EntryKind::Bundle | EntryKind::Link(LinkTarget::Bundle)) => AppMenuSymbol::Application,
        _ => AppMenuSymbol::File,
    }
}

/// Push New ▸'s rows under the submenu row at `parent`: Folder, then one row
/// per document, each admitted only while the text block can hold its label.
///
/// # Errors
///
/// Any [`Errno`] the shared bounds refuse for a reason other than space.
fn push_new_rows(
    menu: &mut AppMenu,
    parent: usize,
    documents: &[BlankDocument],
) -> Result<(), Errno> {
    let folder = AppMenuItem::new(
        item_id(NEW_FOLDER)?,
        AppMenuLabel::new(NEW_FOLDER_LABEL)?,
        AppMenuSymbol::NewFolder,
    )
    .with_shortcut(AppMenuShortcut::new(NEW_FOLDER_SHORTCUT)?);
    menu.push_under(AppMenuRow::Item(folder), parent)?;
    for (index, document) in documents.iter().enumerate() {
        let label = AppMenuLabel::new(document.noun())?;
        if menu.text_remaining() < label.as_str().len() {
            break;
        }
        let id = FIRST_DOCUMENT
            .checked_add(index)
            .ok_or(Errno::OutOfRange)
            .and_then(item_id)?;
        menu.push_under(
            AppMenuRow::Item(AppMenuItem::new(id, label, AppMenuSymbol::NewDocument)),
            parent,
        )?;
    }
    Ok(())
}

/// Push Display ▸'s rows under the submenu row at `parent`: one per display,
/// the one in force marked as the group's choice and refused with its reason,
/// as the desktop's own sort and arrangement rows are.
///
/// # Errors
///
/// Any [`Errno`] the shared bounds refuse. The rows are fixed, so a refusal
/// can only mean those bounds changed under this menu.
fn push_display_rows(menu: &mut AppMenu, parent: usize, shown: ViewMode) -> Result<(), Errno> {
    for (index, display) in ViewMode::ALL.into_iter().enumerate() {
        let mut item = AppMenuItem::new(
            item_id(FIRST_DISPLAY + index)?,
            AppMenuLabel::new(display.label())?,
            display.symbol(),
        );
        if display == shown {
            item = item
                .with_mark(AppMenuMark::Radio)
                .disabled()
                .with_reason(AppMenuReason::new(REASON_CURRENT_DISPLAY)?);
        }
        menu.push_under(AppMenuRow::Item(item), parent)?;
    }
    Ok(())
}

/// Push Display ▸'s orders under the submenu row at `parent`, after the
/// displays: a group of the `keys` the listing's columns order by, then a group
/// of both directions, the order in force marked in each and refused with its
/// reason.
///
/// # Errors
///
/// Any [`Errno`] the shared bounds refuse. The rows are fixed, so a refusal
/// can only mean those bounds changed under this menu.
fn push_sort_rows(
    menu: &mut AppMenu,
    parent: usize,
    order: SortMode,
    keys: [SortKey; 4],
) -> Result<(), Errno> {
    menu.push_under(AppMenuRow::Separator, parent)?;
    for key in keys {
        let row = (sort_key_id(key)?, key.label(), key.symbol());
        push_order_row(menu, parent, row, key == order.key)?;
    }
    menu.push_under(AppMenuRow::Separator, parent)?;
    for (at, direction) in SortDirection::ALL.into_iter().enumerate() {
        let row = (
            item_id(FIRST_SORT_DIRECTION + at)?,
            direction.label(),
            direction.symbol(),
        );
        push_order_row(menu, parent, row, direction == order.direction)?;
    }
    Ok(())
}

/// The id Display ▸ numbers ordering by `key` with: its place in
/// [`SortKey::ALL`].
fn sort_key_id(key: SortKey) -> Result<AppMenuItemId, Errno> {
    let at = SortKey::ALL
        .iter()
        .position(|&each| each == key)
        .ok_or(Errno::OutOfRange)?;
    item_id(FIRST_SORT_KEY + at)
}

/// Push one of Display ▸'s orders under `parent`, marked as its group's choice
/// and refused with its reason when it is `in_force`.
fn push_order_row(
    menu: &mut AppMenu,
    parent: usize,
    (id, label, symbol): (AppMenuItemId, &str, AppMenuSymbol),
    in_force: bool,
) -> Result<(), Errno> {
    let mut item = AppMenuItem::new(id, AppMenuLabel::new(label)?, symbol);
    if in_force {
        item = item
            .with_mark(AppMenuMark::Radio)
            .disabled()
            .with_reason(AppMenuReason::new(REASON_CURRENT_ORDER)?);
    }
    menu.push_under(AppMenuRow::Item(item), parent)
}

/// Push the quick candidates under the "Open With…" row at `parent`, at most
/// [`OPEN_WITH_QUICK_MAX`] of them.
///
/// Each row is admitted only if the menu's shared text block can still hold
/// what it says, asked *before* the push rather than by pushing and swallowing
/// the refusal — so which candidates the plate offers is decided rather than
/// discovered. A row's bundle path is dropped before the row itself is, since a
/// row with no picture still opens the right application.
///
/// # Errors
///
/// Any [`Errno`] the shared bounds refuse for a reason other than space: a
/// candidate name that is not admissible display text, or an id past the
/// numbering.
fn push_candidates(
    menu: &mut AppMenu,
    parent: usize,
    candidates: &[&AppAssociation],
) -> Result<(), Errno> {
    for (index, candidate) in candidates.iter().take(OPEN_WITH_QUICK_MAX).enumerate() {
        let Ok(label) = AppMenuLabel::new(candidate.name()) else {
            continue;
        };
        if menu.text_remaining() < label.as_str().len() {
            break;
        }
        let mut item = AppMenuItem::new(
            item_id(FIRST_CANDIDATE + index)?,
            label,
            AppMenuSymbol::Application,
        );
        let path = candidate.bundle_path();
        if menu.text_remaining() >= candidate.name().len() + path.len() {
            if let Ok(bundle) = AppMenuBundle::new(path) {
                item = item.with_icon_bundle(bundle);
            }
        }
        menu.push_under(AppMenuRow::Item(item), parent)?;
    }
    Ok(())
}

/// The enable state of the file-manager context menu, taken from a [`Browser`]
/// and the app's held clipboard.
///
/// A snapshot: the drawn menu rebuilds it when it is opened and paints each
/// [`ContextCommand`] enabled or disabled from [`is_enabled`](Self::is_enabled),
/// so an inapplicable command renders *disabled*, never hidden, and the menu's
/// shape stays stable. The model performs no action itself — it only reports
/// what is offered — so composing it grants nothing (the read-only picker
/// builds the same model and simply never invokes a write command).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ContextMenuModel {
    /// The kind of the one entry a single-entry verb acts on, through the one
    /// shared [`EntryKind`] classifier, so the kind-scoped rule (Open With…
    /// wants a regular file) reads the same classification every other
    /// surface does.
    chosen: Option<EntryKind>,
    /// How many entries the set verbs — cut, copy, delete — act on.
    selected: usize,
    /// How many entries the listing holds, which Select All needs to know
    /// whether any is left to select.
    listed: usize,
    has_clipboard: bool,
    /// How the window shows its entries, which Display ▸ marks.
    display: ViewMode,
    /// How the window orders its entries, which Display ▸ marks.
    sort: SortMode,
    /// The keys the listing's columns order by, which Display ▸ offers.
    sort_keys: [SortKey; 4],
    /// The endings the window shows names without, which the selection's row
    /// and the Rename field show the chosen name without too.
    endings: HiddenEndings,
}

impl ContextMenuModel {
    /// Build the context-menu state from `browser` and whether the app
    /// currently holds a cut/copy clipboard (`has_clipboard`).
    ///
    /// The clipboard lives in the app, not the browser
    /// ([`clipboard`](Browser::clipboard) *captures* a fresh one from the
    /// selection rather than storing one), so whether a paste is possible is
    /// the caller's own state, threaded in here.
    #[must_use]
    pub fn for_browser<S: DirectorySource>(browser: &Browser<S>, has_clipboard: bool) -> Self {
        Self {
            chosen: browser.chosen_entry().map(Entry::kind),
            selected: browser.selection().len(),
            listed: browser.entries().len(),
            has_clipboard,
            display: browser.view_mode(),
            sort: browser.sort_mode(),
            sort_keys: ListColumns::of_browser(browser).keys(),
            endings: browser.presentation().endings,
        }
    }

    /// Whether `command` is currently actionable and should render enabled.
    ///
    /// Derived from [`reason`](Self::reason), so a row is enabled exactly when
    /// there is nothing to say about why it is not — one rule, rather than a
    /// predicate and an explanation that could disagree.
    #[must_use]
    pub fn is_enabled(&self, command: ContextCommand) -> bool {
        self.reason(command).is_empty()
    }

    /// Why `command` cannot be carried out right now, or `""` when it can.
    ///
    /// [`Cut`](ContextCommand::Cut), [`Copy`](ContextCommand::Copy) and
    /// [`Delete`](ContextCommand::Delete) act on every selected entry, so they
    /// need a selection. [`Open`](ContextCommand::Open),
    /// [`Rename`](ContextCommand::Rename), [`GetInfo`](ContextCommand::GetInfo)
    /// and the other single-entry verbs act on the one entry selected, so they
    /// need exactly one. [`Paste`](ContextCommand::Paste) targets the current
    /// directory and needs only a held clipboard, not a selection.
    /// [`SelectAll`](ContextCommand::SelectAll) needs an entry left to select
    /// and [`ClearSelection`](ContextCommand::ClearSelection) a selection.
    ///
    /// The text is display text a menu row states beside its label; it names
    /// what the user must do, never what they may not (an authority a
    /// principal lacks is the desktop's own to state, and an application has
    /// no way to claim it).
    #[must_use]
    pub fn reason(&self, command: ContextCommand) -> &'static str {
        const DANGLING: &str = "the link leads nowhere";
        let single = || match (self.chosen, self.selected) {
            (Some(kind), _) => Ok(kind),
            (None, 0) => Err(REASON_NO_SELECTION),
            (None, _) => Err("several items selected"),
        };
        match command {
            ContextCommand::Cut
            | ContextCommand::Copy
            | ContextCommand::Delete
            | ContextCommand::ClearSelection => {
                if self.selected > 0 {
                    ""
                } else {
                    REASON_NO_SELECTION
                }
            }
            ContextCommand::Open | ContextCommand::Rename | ContextCommand::GetInfo => {
                single().map_or_else(|reason| reason, |_| "")
            }
            // Open With… offers a chooser of applications, which only a regular
            // file has: a directory descends and a bundle launches itself, so
            // neither has an application to pick. A link offers the chooser
            // for what it *names*, because that is what opening it reaches.
            ContextCommand::OpenWith => match single() {
                Err(reason) => reason,
                Ok(kind) => match kind.resolved() {
                    Some(EntryKind::File) => "",
                    Some(_) => "only a file opens with an application",
                    None => DANGLING,
                },
            },
            ContextCommand::Paste => {
                if self.has_clipboard {
                    ""
                } else {
                    "nothing to paste"
                }
            }
            ContextCommand::SelectAll => {
                if self.listed == 0 {
                    "the folder is empty"
                } else if self.selected >= self.listed {
                    "everything is selected"
                } else {
                    ""
                }
            }
        }
    }
}
