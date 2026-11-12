//! The desktop session's **trusted file picker** (`plans/APPWIN.md` AW5,
//! `plans/CAPABILITY_USE.md` CU6).
//!
//! When an app asks the window channel to pick a file
//! (`WindowRequest::PickFile`), the *session* — not the app — browses the
//! filesystem: the picker is a session-owned window driven by the one
//! shared `lib/browse` engine (the same model and renderer the files app
//! composes), listing directories under the session's own identity and
//! authority. The app never sees a path it was not handed and never
//! browses anything itself; it receives exactly one conclusion — a
//! one-shot `fd_grant` delegation for the chosen file, or a cancellation
//! — delivered over its ordinary event channel.
//!
//! A pick has a purpose. An **open** chooses an existing file to read. A
//! **save** chooses where a document goes, in a dialog of the requester's own
//! window (`savepanel.rs`): the document's icon, its name and
//! where it goes, the folders to choose from shown or hidden above them, and
//! the icon dragged to a folder saves it there. A name that already names a
//! file is replaced only once the user says so, and the file is delegated
//! write-only. A **folder**
//! chooses the folder shown, and the session delegates the files in it the
//! requester opens; the requester never holds the folder.
//!
//! [`SessionPicker`] is the host-testable engine: a table of picks, each
//! named by the [`PickId`] it was minted with and every message about it
//! naming it, so the picks of different applications — and of one
//! application's different windows — are up at once, each holding only the
//! window that asked and concluding on its own; the browser state behind
//! each; and the navigation that ends in a [`PickStep`]. The privileged tail
//! — opening the chosen file and minting the delegation — stays in the
//! session's `Run` binary, which holds the syscalls and carries the open out
//! off its loop; the pick waits for that answer ([`SessionPicker::opened`])
//! so a refused save is stated where the user made it.
//!
//! [`PickerSlot`] is the narrow face the window-channel bridge
//! ([`ShellWindowHost`](crate::ShellWindowHost)) drives: accepting a
//! validated pick request, and aborting a pick whose requesting window
//! died. Keeping the trait object-safe keeps the bridge non-generic.
//!
//! The session also picks for itself ([`Requester::Sound`]): the file a
//! desktop sound plays, whose path it keeps in its user's sound choices and
//! hands to nobody.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::num::NonZeroU64;
use core::ops::Bound;

use tairix_abi::fs::Filetype;
use tairix_abi::fs::OpenFlags;
use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::window_ipc::{AlertButtonRole, PickPurpose, SaveFormats, WINDOW_TITLE_MAX};
use tairix_abi::Errno;
use tairix_browse::media::media_for_name;
use tairix_browse::render::{
    entry_index_at, header_sort_at, listing_damage, listing_floor_height, manager_art, render_into,
    reveal_selection, scroll_pointer, scroll_wheel, shown_listing, toolbar_command_at, ShownArt,
};
use tairix_browse::ManagerChrome;
use tairix_browse::ToolbarBand;
use tairix_browse::{
    apply_command, vfs, BrowseError, Browser, DirectorySource, Entry, EntryKind, ViewMode,
    WatchUpdate, WIN_HEIGHT, WIN_WIDTH,
};
use tairix_controls::{
    damage, paint_focus_ring, Button, ButtonContent, ChainModel, ControlRole, TextAction, TextField,
};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Scale};
use tairix_icon::{IconArtwork, IconRequest, Landed};
use tairix_theme::{TextRole, Theme};
use tairix_util::utf8::shown;
use tairix_wm::{
    past_drag_slop, Compositor, InputEvent, Key, Modifiers, NamedKey, Point, PointerButton, Rect,
    Region, Surface, WindowId,
};

use crate::alert::{cascade_step, dialog_title, Speaker, ALERT_ARM_NS};
use crate::choice::{question, Choice, ChoicePrompt};
use crate::drag::{PictureGrab, SaveDrop, SavedDocument};
use crate::menu::ChainOutcome;
use crate::savepanel::{
    format_media, places, where_menu, PanelFocus, PanelHit, PanelLayout, Place, Said, SavePanel,
};
use crate::shell::DesktopShell;
use crate::shown::Announced;

/// What an open picker's title says it asks — on the taskbar and in the window
/// chrome, after the application it asks for, so the user always sees which UI
/// is asking and on whose behalf. The directory being browsed follows it.
pub const PICKER_TITLE: &str = "Choose a file";

/// Fixed prefix of a save picker's title.
pub const SAVE_TITLE: &str = "Save as";

/// Fixed prefix of a folder picker's title.
pub const FOLDER_TITLE: &str = "Choose a folder";

/// Fixed prefix of the title of a picker handing a folder over.
pub const SUBTREE_TITLE: &str = "Share a folder";

/// Between the fixed prefix and the location it is showing.
const PICKER_TITLE_SEPARATOR: &str = ": ";

/// The chrome the picker draws: no manager surface at all, and the shared
/// read-only command toolbar. Named once so the painted band and the three
/// hit-tests that invert it cannot disagree about whether there is one.
const PICKER_CHROME: ManagerChrome<'static> = ManagerChrome::none();

/// The band the picker's chrome shows, for the layout questions that take it
/// alone.
///
/// Public for the same reason [`PICKER_ORIGIN`] is: a host-side observer
/// reconstructs a picker row's rectangle through the shared renderer, and it
/// must lay out over the band the picker actually draws rather than a guess at
/// it.
pub const PICKER_TOOLBAR: ToolbarBand = PICKER_CHROME.toolbar;

/// Top-left of the first picker of its own, in screen pixels. One
/// deterministic spot (clear of the first cascade slots), exported so a
/// host-side observer (the AW5 QEMU vertical's click script) drives the
/// picker where the session actually places it — never a re-derived guess.
/// Each further one up at once stands in a cascade slot of its own
/// ([`picker_origin`]).
pub const PICKER_ORIGIN: Point = Point::new(120, 90);

/// Top-left of the picker of its own standing in cascade slot `slot` at
/// `scale`: [`PICKER_ORIGIN`] for the first, and each further one set down and
/// across by the step alerts cascade by, so none hides another exactly.
#[must_use]
pub fn picker_origin(slot: usize, scale: Scale) -> Point {
    let step = to_i32(cascade_step(scale, u32::try_from(slot).unwrap_or(u32::MAX)));
    Point::new(
        PICKER_ORIGIN.x.saturating_add(step),
        PICKER_ORIGIN.y.saturating_add(step),
    )
}

/// A pick's first frame: its folder as `browser` lists it, with the save or
/// folder band `bands` holds.
fn first_frame<S: DirectorySource>(
    browser: &Browser<S>,
    bands: (Option<&SaveBand>, Option<&FolderBand>),
    shell: &mut DesktopShell,
    compositor: &Compositor,
) -> Result<Surface, Errno> {
    let scale = compositor.scale();
    let (theme, mut artwork) = shell.theme_and_artwork();
    let (width, height) = pick_size(bands.0, bands.1, scale, theme);
    let mut surface = Surface::new(width, height).ok_or(Errno::LengthOutOfRange)?;
    paint_pick(&mut surface, browser, bands, (scale, theme), &mut artwork);
    Ok(surface)
}

/// Open a picker of its own at cascade slot `slot`, beneath the window the
/// user is in unless `focus`, its close control cancelling it as Escape does.
fn open_own_window(
    (slot, focus): (usize, bool),
    surface: Surface,
    titled: &str,
    shell: &mut DesktopShell,
    compositor: &mut Compositor,
) -> Result<WindowId, Errno> {
    let origin = picker_origin(slot, compositor.scale());
    let title = String::from(titled);
    let wm = if focus {
        shell.open_window(compositor, origin, surface, title)
    } else {
        shell.open_window_behind(compositor, origin, surface, title)
    }
    .ok_or(Errno::NoSpace)?;
    shell.decorate_window(compositor, wm, titled, false);
    shell.keep_on_work_area(compositor, wm);
    Ok(wm)
}

/// The most picks the desktop shows at once, of every application together.
///
/// A containment bound, not a capacity: each holds a window, a listing and a
/// watch, an application has at most
/// [`PICK_MAX_OPEN`](tairix_abi::window_ipc::PICK_MAX_OPEN) of them, and one
/// past this is refused [`Errno::Busy`].
pub const PICKS_MAX: usize = 8;

/// Names one pick for its life.
///
/// Minted from a counter and never reused, so an answer for a pick that has
/// closed — a file opened, a folder made, a menu chosen — reaches no later
/// one, and every message about a pick names it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PickId(NonZeroU64);

impl PickId {
    /// The number it carries, by which the token its folder watch reports
    /// under is told from any other.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// The pick numbered `n`, for a test standing in for the picker.
    #[cfg(test)]
    pub(crate) fn numbered(n: u64) -> Self {
        Self(NonZeroU64::new(n).expect("a pick is numbered from one"))
    }
}

/// Builds the directory source each pick lists through.
///
/// A source sharing one listing desk with others needs the pick it serves, so
/// the desk keeps each pick's slot and watch apart; any `FnMut() -> S` builds
/// every pick's source alike.
pub trait PickSources<S> {
    /// The source pick `pick` lists through.
    fn source_for(&mut self, pick: PickId) -> S;
}

impl<S, F: FnMut() -> S> PickSources<S> for F {
    fn source_for(&mut self, _pick: PickId) -> S {
        self()
    }
}

/// One-shot: a frame carrying the picker, with its listing landed, reached
/// the display, and the picker takes the clicks and keys that choose.
///
/// The sibling of [`MENU_SHOWN`](crate::MENU_SHOWN) for the picker, and
/// necessary for the same reason: the picker is a session-owned compositor
/// window, so the window channel says nothing about its pixels, and the
/// requesting app learns only that its `PickFile` was *accepted*. Acceptance
/// is not readiness either — the listing is read on a worker, so a picker can
/// be on screen showing its "listing…" cue with no row to choose yet. So "the
/// picker is up and there is something to choose" is announced here or
/// nowhere, which is what lets a user diagnosing a picker that never appeared,
/// or a QEMU vertical deciding when a row is worth clicking, wait on a fact
/// rather than on a delay. A picker ignores a choosing click or key until it
/// has been on screen for the arm delay, so it is announced only then: a
/// click it invites is never one the picker drops.
pub const PICKER_SHOWN: tairix_log::EventId = tairix_log::EventId(20_008);

/// The exact message [`PICKER_SHOWN`] is emitted with. A log consumer keys on
/// this rendered text, so it is defined once beside the id and imported by
/// both sides.
pub const PICKER_SHOWN_MESSAGE: &str = "file picker on screen";

/// The save band's committing answer, the answer to the question whether to
/// replace a file, and the answer that declines either.
pub(crate) const SAVE_LABEL: &str = "Save";
const REPLACE_LABEL: &str = "Replace";
pub(crate) const CANCEL_LABEL: &str = "Cancel";

/// What a save says while its file is opened, or while the window it was
/// dropped on names its folder.
const SAVING: &str = "Saving…";

/// What a save says while a folder it asked for is made.
const MAKING: &str = "Making the folder…";

/// A folder band's committing answers, and what each says until it is pressed.
const OPEN_LABEL: &str = "Open";
const FOLDER_PROMPT: &str = "Opens the files in the folder shown";
const SHARE_LABEL: &str = "Share";
const SUBTREE_PROMPT: &str = "Shares the folder shown, and all it holds";
const TOP_NOT_SHARED: &str = "Choose a folder to share: the top level is not one";

/// How the file a pick chose is opened.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PickAccess {
    /// An existing file, opened as its requester is handed any document:
    /// read-write where its signed manifest edits documents and the user may
    /// write the file, read-only otherwise.
    Read,
    /// An existing file read into a document, opened read-only whatever its
    /// requester's manifest says.
    Import,
    /// A new file, to write.
    Create,
    /// The existing file the user agreed to replace, to write.
    Replace,
    /// A folder, whose files the requester opens are each delegated to it
    /// read-only.
    Folder,
    /// A folder handed over whole as a directory capability: writable where
    /// the requester's signed manifest edits documents and the user may write
    /// the folder, read-only otherwise.
    Subtree,
}

impl PickAccess {
    /// The flags a file chosen to save into is opened with, write-only; `None`
    /// for an existing file chosen to open.
    #[must_use]
    pub const fn save_flags(self) -> Option<OpenFlags> {
        match self {
            Self::Read | Self::Import | Self::Folder | Self::Subtree => None,
            // Refusing a name that exists is what keeps an unconfirmed save
            // from overwriting a file, and a link planted in its place from
            // redirecting the write.
            Self::Create => Some(
                OpenFlags::WRITE
                    .union(OpenFlags::CREATE)
                    .union(OpenFlags::EXCLUSIVE)
                    .union(OpenFlags::NO_FOLLOW),
            ),
            // Never truncated here: the requester writes from the start and
            // cuts the file to its own length, so a save abandoned before it
            // writes leaves the old contents whole.
            Self::Replace => Some(
                OpenFlags::WRITE
                    .union(OpenFlags::CREATE)
                    .union(OpenFlags::NO_FOLLOW),
            ),
        }
    }

    /// Whether an existing file — or a folder handed over — is opened to be
    /// changed, for a requester whose manifest edits documents where
    /// `requester_edits`: a document it opens, never a file it reads into one.
    #[must_use]
    pub const fn opens_to_edit(self, requester_edits: bool) -> bool {
        matches!(self, Self::Read | Self::Subtree) && requester_edits
    }
}

/// Who a pick is for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Requester {
    /// An application's window, by its window-channel id: its owner is handed
    /// what the user chose, through the embedder ([`PickStep`]).
    Window(u64),
    /// The session itself, choosing the file a desktop sound plays: the
    /// picker concludes it on its own ([`OwnPick`]), since nothing is opened
    /// or handed to anyone.
    Sound(SoundEvent),
}

/// A pick the session made for itself, concluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnPick {
    /// What it was for.
    pub requester: Requester,
    /// The chosen file's absolute path, or `None` where the user walked away.
    pub path: Option<String>,
}

/// What a pick asks of the embedder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickStep {
    /// Open the file at `path` with `access`, and answer through
    /// [`SessionPicker::opened`] with `pick` and `serial`. The picker waits,
    /// still showing.
    Open {
        /// The pick asking.
        pick: PickId,
        /// Names this attempt, so an answer for one the user has since
        /// abandoned is told apart.
        serial: u64,
        /// The window-channel id of the requesting app's window, whose owner
        /// the file is opened for.
        for_window: u64,
        /// The chosen file's absolute path — the session's to open, never
        /// disclosed to the requesting app. A save's name may yet be
        /// finished as `typing` says.
        path: String,
        /// How to open it.
        access: PickAccess,
        /// How a save's file is typed where it is made.
        typing: Typing,
    },
    /// The user dismissed the picker without choosing; it is closed.
    Cancelled {
        /// The pick dismissed.
        pick: PickId,
        /// The window-channel id of the requesting app's window.
        for_window: u64,
    },
    /// Make the folder at `path`, and answer through
    /// [`SessionPicker::folder_made`] with `pick` and `serial`. The picker
    /// waits, still showing.
    NewFolder {
        /// The pick asking.
        pick: PickId,
        /// Names this attempt.
        serial: u64,
        /// The new folder's absolute path.
        path: String,
    },
    /// Open `model` as a menu the picker owns, anchored on `anchor` in screen
    /// pixels, and answer through [`SessionPicker::menu_answered`] with
    /// `pick`.
    Menu {
        /// The pick whose panel opens it.
        pick: PickId,
        /// What the menu offers.
        model: ChainModel,
        /// The control it opens from.
        anchor: Rect,
        /// The window the pick is for, whose going takes the menu down with
        /// the pick; `None` for a pick the session made for itself.
        for_window: Option<u64>,
    },
}

/// How an answered open ended a pick. The picker is closed either way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickEnd {
    /// The file was opened: it is `for_window`'s, named `name`.
    Chosen {
        /// The pick concluded.
        pick: PickId,
        /// The window-channel id of the requesting app's window.
        for_window: u64,
        /// The chosen file's own name, for the app to title it with.
        name: String,
    },
    /// The file could not be opened, and nothing was chosen.
    Refused {
        /// The pick concluded.
        pick: PickId,
        /// The window-channel id of the requesting app's window.
        for_window: u64,
    },
}

/// A pick an application's window asked for, which the window engine has
/// validated and bounded.
#[derive(Clone, Copy, Debug)]
pub struct PickAsk<'a> {
    /// The asking window's channel id.
    pub window_id: u64,
    /// Its compositor window, where the session has it: held until the pick
    /// concludes, and the window a save's dialog opens over.
    pub held: Option<WindowId>,
    /// Who the pick speaks for — the asking application's attested name —
    /// which its title names first.
    pub speaker: &'a str,
    /// What the user is asked to choose.
    pub purpose: &'a PickPurpose,
    /// Whether the application holds the front, so the picker takes the
    /// keyboard as it opens; otherwise it opens beneath the window that has
    /// it.
    pub focus: bool,
}

/// The narrow face the window-channel bridge drives — object-safe so
/// [`ShellWindowHost`](crate::ShellWindowHost) stays non-generic.
pub trait PickerSlot {
    /// Open the picker for `ask`, a `PickFile` the window engine accepted,
    /// answering the pick and the window it shows, which holds the asking one
    /// until the pick concludes.
    ///
    /// # Errors
    ///
    /// * [`Errno::AlreadyExists`] — the asking window has a pick open.
    /// * [`Errno::Busy`] — [`PICKS_MAX`] picks are open.
    /// * Any [`Errno`] the initial root listing surfaces (the session's
    ///   filesystem reach refused) or the UI cannot come up; nothing is
    ///   recorded and the refusal is relayed to the requesting app.
    fn begin(
        &mut self,
        ask: &PickAsk<'_>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(PickId, WindowId), Errno>;

    /// The window-channel window `window_id` is gone (closed by its owner
    /// or torn down after the owner exited): the pick it asked for, if any,
    /// is taken down, and every pick awaiting a drop on it hears of no
    /// folder. No conclusion is delivered — the engine already dropped the
    /// window's pending pick with its record.
    fn abort_for(&mut self, window_id: u64, shell: &mut DesktopShell, compositor: &mut Compositor);

    /// The application of window `window_id` answered the save dropped on it,
    /// numbered `serial`, with `folder`: the absolute path its location told,
    /// or `None` where it declined. The open it leads to is owed to the
    /// embedder ([`SessionPicker::take_owed_step`]).
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when no pick awaits such a drop.
    fn save_folder_answered(
        &mut self,
        dropped: (u64, u32),
        folder: Option<String>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Errno>;

    /// Whether a picker or the question of one has the keyboard, `focused`
    /// holding it.
    fn holds_keyboard(&self, focused: Option<WindowId>) -> bool;
}

/// An open the embedder was asked for and has not answered.
struct Waiting {
    serial: u64,
    /// The chosen file's own name.
    name: String,
    /// Where it was chosen, whatever the picker shows by the time the answer
    /// lands.
    path: String,
    access: PickAccess,
    /// The folder a save was dropped in, or `None` for the one shown.
    into: Option<Vec<String>>,
    /// How a save's file is typed where it is made.
    typing: Typing,
}

/// How a save's file is typed where it is made, which only the folder it is
/// made in can decide.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Typing {
    /// The RISC OS file type to record it as, on a volume recording one.
    pub filetype: Option<Filetype>,
    /// Its name carries no ending of its type, so on a volume recording no
    /// file type it takes the type's own `,xxx`.
    pub bare: bool,
}

impl Typing {
    /// The name a file asked for as `name` is made under, in a folder whose
    /// volume `records` file types or does not, or `None` where that name is
    /// too long for a file to have.
    #[must_use]
    pub fn leaf(self, name: &str, records: bool) -> Option<String> {
        let suffixed = match self.filetype {
            Some(filetype) if self.bare && !records => {
                format!("{name},{:03x}", filetype.get())
            }
            _ => String::from(name),
        };
        tairix_path::validate_file_name(&suffixed).ok()?;
        Some(suffixed)
    }

    /// The same typing for the file a save found already named so: the name
    /// is exact now, whatever folder it is in.
    #[must_use]
    pub const fn exact(self) -> Self {
        Self {
            filetype: self.filetype,
            bare: false,
        }
    }
}

/// A name a save holds and how its file is typed.
struct Held {
    name: String,
    typing: Typing,
}

/// A file the user is being asked whether to replace: the name the question
/// shows, the one path a yes replaces, the folder a save was dropped in, and
/// the question, an alert holding the picker.
struct Replacing {
    name: String,
    path: String,
    into: Option<Vec<String>>,
    typing: Typing,
    question: ChoicePrompt,
}

/// A save dropped on a window, waiting for its application to name the
/// folder: the drop, and the name the save goes under.
struct AwaitedFolder {
    window_id: u64,
    serial: u32,
    held: Held,
}

/// A save pick's own controls, and the question it may be asking.
struct SaveBand {
    panel: SavePanel,
    /// The file the user is being asked whether to replace.
    replacing: Option<Replacing>,
    /// How tall the folder browser stands above the panel while it shows, in
    /// physical pixels, decided as it opens.
    listing_height: u32,
}

impl SaveBand {
    fn new(suggested: &str, formats: &SaveFormats) -> Self {
        Self {
            panel: SavePanel::new(suggested, formats),
            replacing: None,
            listing_height: 0,
        }
    }

    /// `name` as the requester can write it in the chosen format, and how
    /// its file is typed: as typed where an ending names the format, given
    /// the format's extension where it has none and the format is named by
    /// one, left for the folder to type where the format is named by its file
    /// type alone, and refused, saying how to name it, where an ending names
    /// another type.
    fn held(&self, name: String) -> Result<Held, String> {
        let formats = self.panel.formats();
        let Some(format) = self.panel.format().and_then(|index| formats.get(index)) else {
            return Ok(Held {
                name,
                typing: Typing::default(),
            });
        };
        let filetype = format.filetype();
        let named = media_for_name(&name);
        if format.types(&name) || named.is_some_and(|media| format_media(format) == Some(media)) {
            return Ok(Held {
                name,
                typing: Typing {
                    filetype,
                    bare: false,
                },
            });
        }
        if named.is_some() {
            let mut refusal = String::from("Name it to end in");
            let mut extensions = format.extensions().peekable();
            if extensions.peek().is_none() {
                refusal.push_str(" no ending");
            }
            for extension in extensions {
                refusal.push(' ');
                refusal.push_str(extension);
            }
            if let Some(filetype) = filetype {
                let _ = write!(refusal, " or ,{:03x}", filetype.get());
            }
            return Err(refusal);
        }
        let Some(extension) = format.extensions().next() else {
            let typing = Typing {
                filetype,
                bare: true,
            };
            // Checked as long as it may yet be made.
            typing
                .leaf(&name, false)
                .ok_or_else(|| String::from("The name is too long"))?;
            return Ok(Held { name, typing });
        };
        let mut named = name;
        named.push_str(extension);
        tairix_path::validate_file_name(&named).map_err(|err| err.to_string())?;
        Ok(Held {
            name: named,
            typing: Typing {
                filetype,
                bare: false,
            },
        })
    }

    /// Ask, in an alert holding the picker `picker`, whether to replace the
    /// file `name` at `path`. A question that cannot be put up refuses the
    /// save in the panel instead, so nothing is replaced unasked.
    fn ask_to_replace(
        &mut self,
        ((name, path, into), typing): ((String, String, Option<Vec<String>>), Typing),
        picker: WindowId,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        self.edit(Said::Nothing, shell, compositor);
        let asked = replace_question(&name, &path)
            .and_then(|spec| ChoicePrompt::open(&spec, Some(picker), shell, compositor));
        match asked {
            Some(question) => {
                self.replacing = Some(Replacing {
                    name,
                    path,
                    into,
                    typing: typing.exact(),
                    question,
                });
            }
            None => self.panel.say(Said::Refusal(already_exists(&name))),
        }
    }

    /// Go back to editing the name, the status line saying `said`: a question
    /// about replacing a file comes down.
    fn edit(&mut self, said: Said, shell: &mut DesktopShell, compositor: &mut Compositor) {
        self.panel.say(said);
        if let Some(replacing) = self.replacing.take() {
            replacing.question.close(shell, compositor);
        }
    }
}

/// What a folder pick does with the folder chosen.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum FolderUse {
    /// Its files are opened, each delegated read-only.
    Files,
    /// It is handed over whole.
    Whole,
}

impl FolderUse {
    const fn title(self) -> &'static str {
        match self {
            Self::Files => FOLDER_TITLE,
            Self::Whole => SUBTREE_TITLE,
        }
    }

    const fn prompt(self) -> &'static str {
        match self {
            Self::Files => FOLDER_PROMPT,
            Self::Whole => SUBTREE_PROMPT,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Files => OPEN_LABEL,
            Self::Whole => SHARE_LABEL,
        }
    }

    const fn access(self) -> PickAccess {
        match self {
            Self::Files => PickAccess::Folder,
            Self::Whole => PickAccess::Subtree,
        }
    }

    /// What its band says before the reason a folder was not taken.
    const fn refused(self) -> &'static str {
        match self {
            Self::Files => "Not opened",
            Self::Whole => "Not shared",
        }
    }
}

/// A folder pick's own controls, under the listing: what choosing does, and
/// the two answers.
struct FolderBand {
    purpose: FolderUse,
    cancel: Button,
    open: Button,
    /// Progress or a refusal, shown in place of the prompt.
    message: Option<String>,
}

/// What `purpose` asks of a pick: the save band or the folder band it
/// shows, and how what it picks is opened.
fn bands(purpose: &PickPurpose) -> (Option<SaveBand>, Option<FolderBand>, PickAccess) {
    match purpose {
        PickPurpose::Open => (None, None, PickAccess::Read),
        PickPurpose::Import => (None, None, PickAccess::Import),
        PickPurpose::Folder => (
            None,
            Some(FolderBand::new(FolderUse::Files)),
            PickAccess::Read,
        ),
        PickPurpose::Subtree => (
            None,
            Some(FolderBand::new(FolderUse::Whole)),
            PickAccess::Read,
        ),
        PickPurpose::Save { suggested, formats } => (
            Some(SaveBand::new(suggested.as_str(), formats)),
            None,
            PickAccess::Read,
        ),
    }
}

impl FolderBand {
    fn new(purpose: FolderUse) -> Self {
        Self {
            purpose,
            cancel: Button::labelled(CANCEL_LABEL),
            open: Button::new(
                ButtonContent::Label(String::from(purpose.label())),
                ControlRole::Primary,
            ),
            message: None,
        }
    }
}

/// One pick on screen: who it is for, the picker's compositor window, and the
/// browser state behind it.
struct Pick<S: DirectorySource> {
    id: PickId,
    requester: Requester,
    /// Who it speaks for, which its title names first.
    speaker: String,
    wm: WindowId,
    /// The window it holds, the one that asked, which gets the keyboard back
    /// when the picker closes holding it; `None` for a pick the session made
    /// for itself.
    held: Option<WindowId>,
    standing: Standing,
    /// When its committing clicks arm; `None` until the serve loop first sees
    /// it.
    armed_at: Option<u64>,
    browser: Browser<S>,
    /// The save band, for a pick choosing where to save.
    save: Option<SaveBand>,
    /// The folder band, for a pick choosing a folder.
    folder: Option<FolderBand>,
    /// How a file it chooses is opened.
    reading: PickAccess,
    /// The open asked for and not yet answered; input waits while it is.
    waiting: Option<Waiting>,
    /// How far its [`PICKER_SHOWN`] announcement has come.
    announced: Announced,
    /// The folder shown went from its path, or is the pick's start still
    /// being read: if reading it fails, the pick climbs to the nearest folder
    /// still there rather than showing it as it was.
    lost: bool,
    /// The folder's watch asked for a re-read while another listing was in
    /// flight: owed once that listing is refused, since it answers for the
    /// folder only by landing.
    reread_owed: bool,
    /// The menu the save panel has open, and what its rows stand for.
    menu: Option<PanelMenu>,
    /// Where a press on the save panel's icon landed, while it is held, and
    /// where on the picture it holds it: a drag carried from it is the
    /// document saved where it is dropped.
    icon_press: Option<(Point, PictureGrab)>,
    /// The save dropped on a window, until its application names the folder.
    awaiting: Option<AwaitedFolder>,
    /// The folder asked to be made, and the attempt naming it, until the
    /// embedder answers.
    making: Option<(u64, Vec<String>)>,
    /// What input changed in the window since it was last painted.
    owed: Region,
    /// The open a window's answer to a dropped save led to, for the embedder
    /// to carry out.
    step: Option<PickStep>,
    /// The turn just applied ended the pick.
    ending: Option<Ending>,
}

/// How a picker's window stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Standing {
    /// A dialog of the window it holds: a save.
    Dialog,
    /// A window of its own, in this cascade slot.
    Own(usize),
}

/// How a turn ended its pick.
struct Ending {
    /// What the session's own pick chose, or `None`.
    chosen: Option<String>,
}

/// A menu the save panel opened.
enum PanelMenu {
    /// *Where*: the places its rows stand for, in row order.
    Where(Vec<Place>),
    /// *Format*: the endings, in row order.
    Format,
}

impl<S: DirectorySource> Pick<S> {
    fn title_prefix(&self) -> &'static str {
        title_prefix(self.save.is_some(), self.folder.as_ref())
    }

    /// Its title while it shows `folder`: who it asks for, then what it asks
    /// and where.
    fn title_at(&self, folder: Option<&[String]>) -> String {
        dialog_title(
            Speaker::App(&self.speaker),
            &picker_title(self.title_prefix(), folder),
        )
    }

    /// The window-channel window it is for, or `None` for a pick the session
    /// made for itself.
    const fn for_window(&self) -> Option<u64> {
        match self.requester {
            Requester::Window(for_window) => Some(for_window),
            Requester::Sound(_) => None,
        }
    }

    /// Its question whether to replace a file, while it is up.
    fn question_wm(&self) -> Option<WindowId> {
        let replacing = self.save.as_ref()?.replacing.as_ref()?;
        Some(replacing.question.wm())
    }

    fn replacing_mut(&mut self) -> Option<&mut Replacing> {
        self.save.as_mut()?.replacing.as_mut()
    }

    /// Whether its committing clicks answer input at `now_ns`: a click begun
    /// as it appeared was aimed at what was there before.
    fn armed(&self, now_ns: u64) -> bool {
        self.armed_at.is_some_and(|at| now_ns >= at)
    }

    /// End the pick once the turn is done, the session's own concluding with
    /// `chosen`.
    fn end(&mut self, chosen: Option<String>) {
        self.ending = Some(Ending { chosen });
    }

    /// [`listing_viewport`] of this pick.
    fn viewport(&self, scale: Scale) -> Rect {
        listing_viewport(self.save.as_ref(), scale)
    }

    /// [`pick_size`] of this pick.
    fn size(&self, shell: &DesktopShell, compositor: &Compositor) -> (u32, u32) {
        pick_size(
            self.save.as_ref(),
            self.folder.as_ref(),
            compositor.scale(),
            shell.session().active_theme(),
        )
    }

    /// Whether the listing shows: always, but for a save whose browser is
    /// folded away.
    fn lists(&self) -> bool {
        self.save.as_ref().is_none_or(|band| band.panel.expanded())
    }

    /// Whether the pick waits on an answer — an open, or the folder a window
    /// names — and hears only *Cancel* and `Escape` until it lands.
    const fn answering(&self) -> bool {
        self.waiting.is_some() || self.awaiting.is_some() || self.making.is_some()
    }

    /// Show the folder the browser is at as the save's *Where*.
    fn restate_where(&mut self) {
        if let Some(band) = self.save.as_mut() {
            band.panel.set_where(folder_label(self.browser.folder()));
        }
    }
}

/// The title prefix of a save, a folder, or an open pick.
const fn title_prefix(saving: bool, folder: Option<&FolderBand>) -> &'static str {
    match folder {
        _ if saving => SAVE_TITLE,
        Some(band) => band.purpose.title(),
        None => PICKER_TITLE,
    }
}

/// The session's picker engine over an injected directory-source factory
/// (`F` builds the session-authority source each pick lists through — the
/// live VFS listing calls in production, an in-memory tree in tests).
///
/// A table of picks, as on macOS: those of different applications, and of one
/// application's different windows, are up at once, each holding only the
/// window that asked and concluding on its own.
pub struct SessionPicker<S: DirectorySource, F: PickSources<S>> {
    source: F,
    shared: Shared,
    picks: BTreeMap<PickId, Pick<S>>,
    /// The pick each picker window and replace question belongs to, which
    /// routes their input.
    by_window: BTreeMap<WindowId, PickId>,
    /// The pick each requesting window asked for: one at most.
    by_requester: BTreeMap<u64, PickId>,
    /// The number the last pick was minted with.
    minted: u64,
    /// The session's own pick concluded and not yet taken.
    own: Option<OwnPick>,
}

/// What every pick shares.
struct Shared {
    /// Root-first components of the directory each pick opens at — the
    /// user's `Home` folder in production, so the picker starts among the
    /// user's own files rather than at the storage-forest root. Empty means
    /// the root `/`.
    start: Vec<String>,
    /// The serial the last attempt of any pick asked for.
    serial: u64,
    /// The folder the last save went to, which *Where* offers.
    last_saved: Option<Vec<String>>,
}

impl<S: DirectorySource, F: PickSources<S>> SessionPicker<S, F> {
    /// An idle picker over `source`, opening each pick at the root `/`.
    pub const fn new(source: F) -> Self {
        Self {
            source,
            shared: Shared {
                start: Vec::new(),
                serial: 0,
                last_saved: None,
            },
            picks: BTreeMap::new(),
            by_window: BTreeMap::new(),
            by_requester: BTreeMap::new(),
            minted: 0,
            own: None,
        }
    }

    /// Open each pick at the directory named by root-first `start` instead of
    /// the root — the session points its picker at the logged-in user's
    /// `Home` folder so the user lands among their own files. A start that
    /// cannot be listed climbs to the nearest folder above it that can, and
    /// only a refusal of the root itself refuses the pick (see
    /// [`begin`](PickerSlot::begin)).
    #[must_use]
    pub fn starting_at(mut self, start: Vec<String>) -> Self {
        self.shared.start = start;
        self
    }

    /// Where pick `pick`'s save panel lays its parts out, for a test that
    /// presses one.
    #[cfg(test)]
    pub(crate) fn panel_for_test(
        &self,
        pick: PickId,
        scale: Scale,
        theme: &Theme,
    ) -> Option<PanelLayout> {
        let band = self.picks.get(&pick)?.save.as_ref()?;
        Some(panel_layout(band, scale, theme))
    }

    /// Whether any pick is open: a trusted surface, which nothing that may
    /// draw over windows or watch the pointer stands over.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.picks.is_empty()
    }

    /// The compositor window pick `pick` shows in.
    #[must_use]
    pub fn window_of(&self, pick: PickId) -> Option<WindowId> {
        self.picks.get(&pick).map(|open| open.wm)
    }

    /// Pick `pick`'s question whether to replace a file, while it is up: an
    /// alert holding the picker, which takes no input of its own until it is
    /// answered.
    #[must_use]
    pub fn question_of(&self, pick: PickId) -> Option<WindowId> {
        self.picks.get(&pick)?.question_wm()
    }

    /// The pick showing in picker window `wm`. The embedder routes that
    /// window's key, click, pointer and wheel input to it — through
    /// [`handle_key`](Self::handle_key), [`handle_click`](Self::handle_click),
    /// [`handle_pointer`](Self::handle_pointer) and [`scroll`](Self::scroll) —
    /// instead of the served-window channel.
    #[must_use]
    pub fn shown_in(&self, wm: WindowId) -> Option<PickId> {
        let pick = *self.by_window.get(&wm)?;
        (self.picks.get(&pick)?.wm == wm).then_some(pick)
    }

    /// The pick asking whether to replace a file in question window `wm`.
    #[must_use]
    pub fn asking_in(&self, wm: WindowId) -> Option<PickId> {
        let pick = *self.by_window.get(&wm)?;
        (self.picks.get(&pick)?.wm != wm).then_some(pick)
    }

    /// The open pick numbered `number`: the number the token its folder watch
    /// reports under carries.
    #[must_use]
    pub fn numbered(&self, number: u64) -> Option<PickId> {
        NonZeroU64::new(number)
            .map(PickId)
            .filter(|pick| self.picks.contains_key(pick))
    }

    /// Whether a picker or the question of one has the keyboard — `focused`
    /// is one of their windows — and so holds the seat as the lock does.
    #[must_use]
    pub fn holds_keyboard(&self, focused: Option<WindowId>) -> bool {
        focused.is_some_and(|wm| self.by_window.contains_key(&wm))
    }

    /// Paint what input changed in each picker and replace question since the
    /// last paint: once, after the input that changed them drained. A picker
    /// whose pixels were given back while it was hidden is painted whole once
    /// it shows.
    pub fn paint_owed(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        for pick in self.picks.values_mut() {
            pick.paint_owed(shell, compositor);
        }
    }

    /// Start the arm delay of each pick, and of each replace question, put up
    /// since the last wake at `now_ns`, the frame it is first on screen.
    pub fn stamp(&mut self, now_ns: u64) {
        for pick in self.picks.values_mut() {
            if pick.armed_at.is_none() {
                pick.armed_at = Some(now_ns.saturating_add(ALERT_ARM_NS));
            }
            if let Some(replacing) = pick.replacing_mut() {
                replacing.question.stamp(now_ns);
            }
        }
    }

    /// A primary-button click at `local` on pick `pick`'s question, at
    /// `now_ns`.
    pub fn question_click(
        &mut self,
        pick: PickId,
        local: Point,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.question_click(local, now_ns, cx, shell, compositor)
        })
        .flatten()
    }

    /// One key press on pick `pick`'s question, at `now_ns`: `Escape`
    /// declines.
    pub fn question_key(
        &mut self,
        pick: PickId,
        record: &KeyInput,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.question_key(record, now_ns, cx, shell, compositor)
        })
        .flatten()
    }

    /// Apply one key press to pick `pick`.
    ///
    /// An open: `Down`/`Up` move the selection, `Enter` descends into a
    /// selected directory or chooses a selected regular file, and
    /// `Backspace` climbs to the parent. A save: every key edits the name,
    /// `Enter` saves. `Escape` cancels either. A refused navigation changes
    /// nothing, and while an answer is awaited only `Escape` is heard.
    pub fn handle_key(
        &mut self,
        pick: PickId,
        record: &KeyInput,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.handle_key(record, now_ns, cx, shell, compositor)
        })
        .flatten()
    }

    /// Apply one primary-button press, at `now_ns`, at the picker-window-local
    /// position `local` of pick `pick`.
    ///
    /// A press before the pick's committing clicks arm, while it may still be
    /// catching a click aimed at what was there before it appeared, does
    /// nothing. A press on the listing's scroll bar is the bar's
    /// ([`handle_pointer`](Self::handle_pointer)). A click on a toolbar command
    /// runs it (the read-only navigation the picker shares with the file
    /// manager — Back/Forward/Up/Refresh, the view toggle, and sort — through
    /// the one shared `tairix_browse::apply_command`); a click on an entry row
    /// resolves through the shared hit-test
    /// (`tairix_browse::render::entry_index_at` — exactly the rows the
    /// renderer drew): a directory row descends, and a regular-file row
    /// chooses that file — or, in a save, offers its name. A press on the
    /// save band reaches its field or answers with a button. A click on a
    /// disabled tool, past the listing, or on an unresolvable coordinate
    /// changes nothing, and while an answer is awaited only *Cancel* is.
    pub fn handle_click(
        &mut self,
        pick: PickId,
        local: Point,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.handle_click(local, now_ns, cx, shell, compositor)
        })
        .flatten()
    }

    /// Route a pointer `event` at the picker-window-local position `local` of
    /// pick `pick`: to the save panel's buttons, which light under the
    /// pointer, and to the listing's scroll bar, answering whether the picker
    /// took it — so a press on the bar is never also a press on a row, and a
    /// press held on the save panel's icon is the drag's until it ends.
    ///
    /// The bar keeps what a press on it started: an end button or the track
    /// steps, the thumb drags, and the moves and the release that follow are
    /// the bar's until the release ends them. What either repainted is owed,
    /// and painted once with the turn.
    pub fn handle_pointer(
        &mut self,
        pick: PickId,
        local: Point,
        event: &InputEvent,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        self.turn(pick, shell, compositor, |open, _, shell, compositor| {
            open.handle_pointer(local, event, shell, compositor)
        })
        .unwrap_or(false)
    }

    /// Carry the save's document out of pick `pick`'s panel icon, once the
    /// press held on it has travelled past the drag slop to `local` at
    /// `scale`: the press is the drag's from here, so the panel lets go of it.
    /// Answers what the drag carries and the window the save is for.
    pub fn icon_drag(
        &mut self,
        pick: PickId,
        local: Point,
        scale: Scale,
    ) -> Option<(SavedDocument, u64)> {
        self.picks.get_mut(&pick)?.icon_drag(local, scale)
    }

    /// What became of the save's document carried out of pick `pick`'s panel:
    /// dropped in a folder the desktop resolved, it is saved there; on a
    /// window, the panel waits for that window's application to name the
    /// folder; and anywhere else nothing changes.
    ///
    /// The name must hold as *Save* holds it, a refusal stated in the panel.
    pub fn save_dropped(
        &mut self,
        pick: PickId,
        dropped: SaveDrop,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.save_dropped(dropped, cx, shell, compositor)
        })
        .flatten()
    }

    /// Whether pick `pick` waits on the window `window_id` to name the folder
    /// its save was dropped on with the offer numbered `serial`.
    #[must_use]
    pub fn awaits_drop(&self, pick: PickId, window_id: u64, serial: u32) -> bool {
        self.picks
            .get(&pick)
            .is_some_and(|open| open.awaits_drop(window_id, serial))
    }

    /// Pick `pick` stops waiting on the window its save was dropped on, which
    /// could not be told where: nothing is saved, and the panel says so.
    pub fn drop_unreached(
        &mut self,
        pick: PickId,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let _ = self.turn(pick, shell, compositor, |open, _, shell, compositor| {
            if open.awaiting.take().is_some() {
                open.refuse_save(String::from(NOT_GIVEN), shell, compositor);
            }
        });
    }

    /// The open a window's answer to a dropped save led to, for the embedder
    /// to carry out as it does any other [`PickStep`].
    pub fn take_owed_step(&mut self) -> Option<PickStep> {
        self.picks.values_mut().find_map(|open| open.step.take())
    }

    /// When the soonest press a picker's listing bar holds steps next, or
    /// `None` when nothing held would move.
    pub fn scroll_repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.picks
            .values_mut()
            .filter_map(|open| open.browser.scroll_repeat_deadline(now_ns))
            .min()
    }

    /// Step each press a picker's listing bar holds whose repeat is due at
    /// `now_ns`, repainting what it moved into that picker's own buffer.
    pub fn scroll_repeat(
        &mut self,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        self.each(shell, compositor, |open, _, shell, compositor| {
            open.scroll_repeat(now_ns, shell, compositor);
        });
    }

    /// Scroll pick `pick`'s listing by a wheel turn of `(dx, dy)`, in the
    /// seat's scroll units, through the listing's own bar, answering whether
    /// it moved.
    ///
    /// The bar carries what is short of a pixel into the next turn. Only what
    /// the turn moved — the items and the bar — is repainted, into the
    /// picker's own buffer.
    pub fn scroll(
        &mut self,
        pick: PickId,
        delta: (i32, i32),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        self.turn(pick, shell, compositor, |open, _, shell, compositor| {
            open.scroll(delta, shell, compositor)
        })
        .unwrap_or(false)
    }

    /// Repaint, in every picker, the slots whose pictures the `landed` decodes
    /// moved — the listing's rows or tiles, and a save's own icon — which were
    /// drawn with their glyphs until the decodes came back.
    pub fn artwork_landed(
        &mut self,
        landed: &Landed,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if landed.is_empty() {
            return;
        }
        self.each(shell, compositor, |open, _, shell, compositor| {
            open.artwork_landed(landed, shell, compositor);
        });
    }

    /// Ask for the picture of every slot each pick shows, drawing nothing, so
    /// a sweep after a pass that repainted only part of a picker withdraws
    /// what no picker shows and nothing else.
    pub fn want_shown_artwork(&self, shell: &mut DesktopShell, compositor: &Compositor) {
        let scale = compositor.scale();
        for open in self.picks.values() {
            shell.want_artwork(|theme, want| {
                open.each_shown_art((scale, theme), &mut |art| want(art.request, art.side));
            });
        }
    }

    /// Redraw every pick, and its question, in the look and at the scale now
    /// in use: a session window keeps the pixels it was painted in.
    pub fn restyle(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        self.each(shell, compositor, |open, _, shell, compositor| {
            open.restyle(shell, compositor);
        });
    }

    /// Ask the source again for each pick's navigation whose listing had not
    /// arrived, repainting and retitling it when it lands.
    ///
    /// This is what the session calls on the wake that says its listing
    /// readers finished — never a poll. A pick with nothing pending is left as
    /// it is. A listing the source now refuses drops the pending navigation
    /// and repaints, so the "listing" cue clears and the picker is left
    /// exactly where it was (fail closed) — unless the folder went from its
    /// path, when the pick moves to its parent.
    pub fn resume(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        self.each(shell, compositor, |open, cx, shell, compositor| {
            open.reread(cx, shell, compositor, Browser::resume);
        });
    }

    /// Follow what each pick's folder watch reported, `update_of` answering
    /// for the pick it names: the changed entries are merged in place and only
    /// the rows they altered repainted; a rescan, or a folder gone from its
    /// path, reads the folder afresh — once a navigation already reading one
    /// is refused, if one is — and a folder that cannot be read again after
    /// going leaves the pick at the nearest folder above that is still there.
    pub fn follow_each(
        &mut self,
        mut update_of: impl FnMut(PickId) -> WatchUpdate,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        self.each(shell, compositor, |open, cx, shell, compositor| {
            let update = update_of(open.id);
            open.follow(update, cx, shell, compositor);
        });
    }

    /// Dismiss pick `pick` without choosing, closing its picker window.
    ///
    /// What both dismissals mean, so they cannot diverge: the Escape key the
    /// engine routes itself, and the title bar's close control, which the
    /// window manager raises for the session to interpret because a window
    /// the session paints is the session's to close.
    ///
    /// Returns the concluded pick, or `None` for one the session made for
    /// itself, or one not open.
    pub fn cancel(
        &mut self,
        pick: PickId,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        self.turn(pick, shell, compositor, |open, _, _, _| open.cancel())
            .flatten()
    }

    /// The embedder's answer to pick `pick`'s open `serial` names: whether the
    /// file could be opened, and the path a save made it at, its name finished
    /// as its typing asked — `None` for the path asked for.
    ///
    /// An opened file ends the pick. A refused open ends an open pick too, but
    /// a save stays up to say why — or, for a name that was taken after the
    /// listing was read, to ask whether to replace it. An answer for an
    /// attempt that has gone (the user cancelled, the requesting window
    /// closed, the pick ended) is `None`: whatever it opened is the embedder's
    /// to close.
    pub fn opened(
        &mut self,
        pick: PickId,
        serial: u64,
        answer: (Result<(), Errno>, Option<String>),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickEnd> {
        self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.opened(serial, answer, cx, shell, compositor)
        })
        .flatten()
    }

    /// Announce, through `report`, each pick a presented frame has carried
    /// with its listing landed and whose choosing input is armed at `now_ns`;
    /// `presented` says a frame has just reached the display.
    ///
    /// Called after every successful present, like its window and menu
    /// siblings, and on every wake, since the arm can fall due with nothing to
    /// paint. One-shot per pick. A pick still waiting on its listing, or on
    /// its arm, announces nothing *yet*, so the announcement never runs ahead
    /// of the rows a user picks from nor invites a click the picker ignores.
    pub fn report_newly_shown(&mut self, now_ns: u64, presented: bool, mut report: impl FnMut()) {
        for open in self.picks.values_mut() {
            let landed = presented && !open.browser.is_listing();
            if open.announced.advance(landed, open.armed_at, now_ns) {
                report();
            }
        }
    }

    /// When the next announcement [`report_newly_shown`](Self::report_newly_shown)
    /// holds back for a pick's arm falls due.
    #[must_use]
    pub fn shown_deadline(&self) -> Option<u64> {
        self.picks
            .values()
            .filter_map(|open| open.announced.due(open.armed_at))
            .min()
    }

    /// The embedder's answer to the folder pick `pick` asked to be made with
    /// `serial`: made, the save goes on into it with the document's name;
    /// refused, the field still names the folder and says why. An answer for
    /// an attempt that has gone is dropped.
    pub fn folder_made(
        &mut self,
        pick: PickId,
        serial: u64,
        made: Result<(), Errno>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let _ = self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.folder_made(serial, made, cx, shell, compositor);
        });
    }

    /// What became of a menu pick `pick`'s save panel opened: a place chosen
    /// is shown, a format chosen puts the name in it, and anything else
    /// changes nothing.
    pub fn menu_answered(
        &mut self,
        pick: PickId,
        outcome: &ChainOutcome,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let _ = self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            open.menu_answered(outcome, cx, shell, compositor);
        });
    }

    /// Give back the pixels of every picker the user cannot see, which are
    /// painted again from its state once it shows: what memory pressure asks
    /// of the desktop's own windows. Answers the bytes given back.
    pub fn release_hidden(&self, compositor: &mut Compositor) -> usize {
        self.picks.values().fold(0, |freed, open| {
            freed.saturating_add(compositor.release_hidden_content(open.wm))
        })
    }

    /// The pick the session made for itself that has concluded since this was
    /// last asked.
    pub fn take_own(&mut self) -> Option<OwnPick> {
        self.own.take()
    }

    /// Open the picker for the session's own pick of `purpose` for
    /// `requester`. It answers what the user asked for in the desktop's own
    /// Settings, so it takes the keyboard.
    ///
    /// # Errors
    ///
    /// As [`PickerSlot::begin`].
    pub fn begin_for(
        &mut self,
        requester: Requester,
        purpose: &PickPurpose,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<PickId, Errno> {
        let ask = (Speaker::Desktop.name(), purpose, true);
        self.open_pick((requester, None), ask, shell, compositor)
            .map(|(pick, _)| pick)
    }

    /// Open a pick for `requester`, holding `held`, speaking for `speaker`,
    /// choosing for `purpose`, and taking the keyboard when `focus`: a save
    /// opens as a dialog of the window it holds, and anything else as a window
    /// of its own in the lowest cascade slot free.
    fn open_pick(
        &mut self,
        (requester, held): (Requester, Option<WindowId>),
        (speaker, purpose, focus): (&str, &PickPurpose, bool),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(PickId, WindowId), Errno> {
        if let Requester::Window(window_id) = requester {
            if self.by_requester.contains_key(&window_id) {
                return Err(Errno::AlreadyExists);
            }
        }
        if self.picks.len() >= PICKS_MAX {
            return Err(Errno::Busy);
        }
        let pick = self.mint()?;
        // Listed under the session's authority before any UI state exists, so
        // a refused root refuses the pick with nothing half-open. A save starts
        // where the last one went, climbing to what is still there.
        let mut at = match purpose {
            PickPurpose::Save { .. } => self.shared.last_saved.clone(),
            PickPurpose::Open
            | PickPurpose::Import
            | PickPurpose::Folder
            | PickPurpose::Subtree => None,
        }
        .unwrap_or_else(|| self.shared.start.clone());
        let browser = loop {
            match Browser::open_at(self.source.source_for(pick), at.clone()) {
                Ok(browser) => break browser,
                Err(err) => {
                    if at.pop().is_none() {
                        return Err(err.source_errno().unwrap_or(Errno::PermissionDenied));
                    }
                }
            }
        };
        let (mut save, folder, reading) = bands(purpose);
        if let Some(band) = save.as_mut() {
            band.panel.set_where(folder_label(browser.folder()));
        }
        let surface = first_frame(
            &browser,
            (save.as_ref(), folder.as_ref()),
            shell,
            compositor,
        )?;
        let prefix = title_prefix(save.is_some(), folder.as_ref());
        let titled = dialog_title(
            Speaker::App(speaker),
            &picker_title(prefix, browser.folder()),
        );
        // The title says which UI is asking and on whose behalf. A save reads
        // as that document's question, so it opens as a dialog over the window
        // that asked; an open or a folder pick is a window of its own, whose
        // close control cancels it as Escape does. One the application has not
        // earned the keyboard for waits beneath the window the user is in.
        let (wm, standing) = if let Some(owner) = held.filter(|_| save.is_some()) {
            let wm = shell
                .open_dialog_window(compositor, owner, surface, &titled, (true, focus, !focus))
                .ok_or(Errno::NoSpace)?;
            (wm, Standing::Dialog)
        } else {
            let slot = self.free_slot();
            let wm = open_own_window((slot, focus), surface, &titled, shell, compositor)?;
            (wm, Standing::Own(slot))
        };
        let lost = browser.is_listing();
        self.picks.insert(
            pick,
            Pick {
                id: pick,
                requester,
                speaker: String::from(speaker),
                wm,
                held,
                standing,
                armed_at: None,
                browser,
                save,
                folder,
                reading,
                waiting: None,
                announced: Announced::Unseen,
                lost,
                reread_owed: false,
                menu: None,
                icon_press: None,
                awaiting: None,
                making: None,
                owed: Region::new(),
                step: None,
                ending: None,
            },
        );
        self.by_window.insert(wm, pick);
        if let Requester::Window(window_id) = requester {
            self.by_requester.insert(window_id, pick);
        }
        Ok((pick, wm))
    }

    /// Mint the next pick's id, never one handed out before.
    fn mint(&mut self) -> Result<PickId, Errno> {
        let next = self.minted.checked_add(1).ok_or(Errno::NoSpace)?;
        let pick = NonZeroU64::new(next).map(PickId).ok_or(Errno::NoSpace)?;
        self.minted = next;
        Ok(pick)
    }

    /// The lowest cascade slot no picker of its own stands in. Fewer than
    /// [`PICKS_MAX`] picks are open whenever one is placed, so one is free.
    fn free_slot(&self) -> usize {
        (0..PICKS_MAX)
            .find(|slot| {
                !self
                    .picks
                    .values()
                    .any(|open| open.standing == Standing::Own(*slot))
            })
            .unwrap_or(0)
    }

    /// Apply `act` to pick `pick`, keeping the windows that route to it in
    /// step with its question and closing it if `act` ended it; `None` for a
    /// pick not open, whose late answer is nobody's.
    fn turn<T>(
        &mut self,
        pick: PickId,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        act: impl FnOnce(&mut Pick<S>, &mut Shared, &mut DesktopShell, &mut Compositor) -> T,
    ) -> Option<T> {
        let open = self.picks.get_mut(&pick)?;
        let asked = open.question_wm();
        let done = act(open, &mut self.shared, shell, compositor);
        let asking = open.question_wm();
        let ending = open.ending.take();
        if asking != asked {
            if let Some(gone) = asked {
                self.by_window.remove(&gone);
            }
            if let Some(up) = asking {
                self.by_window.insert(up, pick);
            }
        }
        if let Some(Ending { chosen }) = ending {
            self.close(pick, chosen, shell, compositor);
        }
        Some(done)
    }

    /// Apply `act` to every open pick in turn, as [`turn`](Self::turn) does.
    fn each(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        mut act: impl FnMut(&mut Pick<S>, &mut Shared, &mut DesktopShell, &mut Compositor),
    ) {
        let mut next = self.picks.keys().next().copied();
        while let Some(pick) = next {
            let _ = self.turn(pick, shell, compositor, &mut act);
            next = self
                .picks
                .range((Bound::Excluded(pick), Bound::Unbounded))
                .next()
                .map(|(id, _)| *id);
        }
    }

    /// Close pick `pick`: its question and its window come down, the keyboard
    /// going back to the window it held if the picker had it, and its listing
    /// consumer and watch go with its browser. The session's own pick
    /// concludes with `chosen`, kept for [`Self::take_own`].
    fn close(
        &mut self,
        pick: PickId,
        chosen: Option<String>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some(mut closing) = self.picks.remove(&pick) else {
            return;
        };
        self.by_window.remove(&closing.wm);
        if let Some(question) = closing.question_wm() {
            self.by_window.remove(&question);
        }
        if let Requester::Window(window_id) = closing.requester {
            self.by_requester.remove(&window_id);
        }
        if let Some(band) = closing.save.as_mut() {
            band.edit(Said::Nothing, shell, compositor);
        }
        if closing.standing == Standing::Dialog {
            let _ = shell.close_transient_window(compositor, closing.wm, closing.held);
        } else {
            let had = shell.router().focused() == Some(closing.wm);
            let _ = shell.close_window(compositor, closing.wm);
            if let Some(held) = closing.held.filter(|_| had) {
                let _ = shell.focus_holder(compositor, held);
            }
        }
        if let requester @ Requester::Sound(_) = closing.requester {
            self.own = Some(OwnPick {
                requester,
                path: chosen,
            });
        }
    }
}

impl<S: DirectorySource> Pick<S> {
    /// Paint what input changed in the picker and its question since the last
    /// paint, and the whole picker where its pixels went while it was hidden.
    fn paint_owed(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let shows = compositor
            .window(self.wm)
            .is_some_and(tairix_wm::Window::is_visible);
        if shows && !compositor.keeps_content(self.wm, self.size(shell, compositor)) {
            redraw(self, shell, compositor);
        }
        if !self.owed.is_empty() && paint(self, shell, compositor) {
            self.owed.clear();
        }
        if let Some(replacing) = self.replacing_mut() {
            replacing.question.paint_owed(shell, compositor);
        }
    }

    /// A primary-button click at `local` on its question, at `now_ns`.
    fn question_click(
        &mut self,
        local: Point,
        now_ns: u64,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let replacing = self.replacing_mut()?;
        let choice = replacing.question.click(local, now_ns, shell, compositor)?;
        self.answer_replace(choice, cx, shell, compositor)
    }

    /// One key press on its question, at `now_ns`: `Escape` declines.
    fn question_key(
        &mut self,
        record: &KeyInput,
        now_ns: u64,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let replacing = self.replacing_mut()?;
        let choice = replacing.question.key(record, now_ns, shell, compositor)?;
        self.answer_replace(choice, cx, shell, compositor)
    }

    /// The question was answered `choice`: the file is replaced, or the user
    /// goes back to the name.
    fn answer_replace(
        &mut self,
        choice: Choice,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let Replacing {
            name,
            path,
            into,
            typing,
            question,
        } = self.save.as_mut()?.replacing.take()?;
        // The keyboard goes back to the picker with it.
        question.close(shell, compositor);
        match choice {
            Choice::Accepted => self.request_at(
                (name, path, into),
                (PickAccess::Replace, typing),
                cx,
                shell,
                compositor,
            ),
            Choice::Declined => None,
        }
    }

    /// While the question is up the picker answers nothing: a press or a key
    /// meant for it hands the keyboard to the question. Answers whether it
    /// was up.
    fn held_by_question(&self, shell: &mut DesktopShell, compositor: &mut Compositor) -> bool {
        let Some(question) = self.question_wm() else {
            return false;
        };
        shell.focus_holder(compositor, question);
        true
    }

    /// One key press on the picker.
    fn handle_key(
        &mut self,
        record: &KeyInput,
        now_ns: u64,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let KeyInput::Pressed { key: Some(key), .. } = record else {
            return None;
        };
        if self.held_by_question(shell, compositor) {
            return None;
        }
        // An Enter begun as the picker appeared was meant for what was there
        // before it, as a click is.
        if matches!(key, KeyValue::Named(NamedKeyCode::Enter)) && !self.armed(now_ns) {
            return None;
        }
        if self.answering() {
            return matches!(key, KeyValue::Named(NamedKeyCode::Escape))
                .then(|| self.cancel_waiting(shell, compositor))
                .flatten();
        }
        if self.save.is_some() {
            return self.save_key(record, cx, shell, compositor);
        }
        let folder = self.folder.is_some();
        match key {
            KeyValue::Named(NamedKeyCode::Down) => {
                self.navigate(cx, shell, compositor, |browser| {
                    browser.select_next();
                    NavOutcome::Redraw
                })
            }
            KeyValue::Named(NamedKeyCode::Up) => self.navigate(cx, shell, compositor, |browser| {
                browser.select_previous();
                NavOutcome::Redraw
            }),
            KeyValue::Named(NamedKeyCode::Enter) if folder => {
                let into = self.browser.chosen_index().filter(|&index| {
                    self.browser
                        .entries()
                        .get(index)
                        .is_some_and(Entry::is_directory)
                });
                match into {
                    Some(index) => {
                        self.navigate(cx, shell, compositor, |browser| descend(browser, index))
                    }
                    None => self.choose_folder(cx, shell, compositor),
                }
            }
            KeyValue::Named(NamedKeyCode::Enter) => {
                self.navigate(cx, shell, compositor, |browser| {
                    browser
                        .chosen_index()
                        .map_or(NavOutcome::None, |index| open_or_choose(browser, index))
                })
            }
            KeyValue::Named(NamedKeyCode::Backspace) => {
                self.navigate(cx, shell, compositor, |browser| {
                    if browser.go_up().unwrap_or(false) {
                        NavOutcome::Redraw
                    } else {
                        NavOutcome::None
                    }
                })
            }
            KeyValue::Named(NamedKeyCode::Escape) => self.cancel(),
            _ => None,
        }
    }

    /// One primary-button press at the picker-window-local `local`, at
    /// `now_ns`.
    fn handle_click(
        &mut self,
        local: Point,
        now_ns: u64,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        // A click begun as the picker appeared was aimed at what was there
        // before it.
        if !self.armed(now_ns) || self.held_by_question(shell, compositor) {
            return None;
        }
        if self.answering() {
            return self
                .cancel_pressed(local, shell.session().active_theme(), compositor.scale())
                .then(|| self.cancel_waiting(shell, compositor))
                .flatten();
        }
        let press = InputEvent::PointerPressed {
            button: PointerButton::Primary,
        };
        if self.handle_pointer(local, &press, shell, compositor) {
            return None;
        }
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let viewport = self.viewport(scale);
        if !self.lists() || !viewport.contains(local) {
            return self.band_click(local, cx, shell, compositor);
        }
        // Hit-test at the same scale and theme the picker renders with, so a
        // click resolves to exactly the item the user saw (list row or grid
        // tile), and a click on the scrollbar gutter resolves to nothing.
        // A toolbar command takes priority over the item area it sits above;
        // an enabled command runs, a disabled one resolves to nothing.
        let toolbar =
            toolbar_command_at(&self.browser, scale, theme, viewport, PICKER_TOOLBAR, local);
        if let Some(command) = toolbar {
            return self.navigate(cx, shell, compositor, move |browser| {
                match apply_command(browser, command) {
                    Ok(true) => NavOutcome::Redraw,
                    Ok(false) | Err(_) => NavOutcome::None,
                }
            });
        }
        let sorted = header_sort_at(&self.browser, scale, theme, viewport, PICKER_TOOLBAR, local);
        if let Some(order) = sorted {
            return self.navigate(cx, shell, compositor, move |browser| {
                browser.set_sort_mode(order);
                NavOutcome::Redraw
            });
        }
        let index = entry_index_at(&self.browser, scale, theme, viewport, PICKER_TOOLBAR, local)?;
        if self.save.is_some() {
            return self.offer_entry(index, cx, shell, compositor);
        }
        if self.folder.is_some() {
            return self.navigate(cx, shell, compositor, |browser| descend(browser, index));
        }
        self.navigate(cx, shell, compositor, |browser| {
            open_or_choose(browser, index)
        })
    }

    /// Route a pointer `event` at the picker-window-local `local` to its save
    /// panel's buttons and its listing's scroll bar, answering whether the
    /// picker took it.
    fn handle_pointer(
        &mut self,
        local: Point,
        event: &InputEvent,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> bool {
        if self.question_wm().is_some() {
            return true;
        }
        if self.icon_press.is_some() {
            if matches!(
                event,
                InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled
            ) {
                self.icon_press = None;
            }
            return true;
        }
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let lit = self.save.as_mut().map(|band| {
            let layout = panel_layout(band, scale, theme);
            let mut lit = Region::new();
            band.panel.on_pointer(event, &layout, &mut lit);
            lit
        });
        if let Some(lit) = lit.filter(|lit| !lit.is_empty()) {
            repaint(self, &lit);
        }
        if !self.lists() {
            return false;
        }
        let mut drew = damage::sink();
        let viewport = self.viewport(scale);
        let Some(repainted) = scroll_pointer(
            &mut self.browser,
            scale,
            theme,
            viewport,
            PICKER_TOOLBAR,
            local,
            event,
            &mut drew,
        ) else {
            return false;
        };
        if repainted {
            repaint(self, &drew);
        }
        true
    }

    /// Carry the save's document out of the panel's icon, once the press held
    /// on it has travelled past the drag slop to `local` at `scale`.
    fn icon_drag(&mut self, local: Point, scale: Scale) -> Option<(SavedDocument, u64)> {
        let (from, grab) = self.icon_press?;
        let for_window = self.for_window()?;
        if self.answering() || !past_drag_slop(from, local, scale) {
            return None;
        }
        self.icon_press = None;
        let panel = &self.save.as_ref()?.panel;
        let document = SavedDocument {
            name: panel.name.text().to_string(),
            icon: panel.icon(),
            grab,
        };
        Some((document, for_window))
    }

    /// What became of the save's document carried out of the panel.
    fn save_dropped(
        &mut self,
        dropped: SaveDrop,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        if matches!(dropped, SaveDrop::Nothing) || self.answering() {
            return None;
        }
        if matches!(dropped, SaveDrop::Unanswered) {
            self.refuse_save(String::from(UNANSWERED), shell, compositor);
            return None;
        }
        let held = self.dropped_name(shell, compositor)?;
        match dropped {
            SaveDrop::Nothing | SaveDrop::Unanswered => None,
            SaveDrop::Folder(folder) => self.save_into(held, &folder, cx, shell, compositor),
            SaveDrop::Offer { window_id, serial } => {
                let band = self.save.as_mut()?;
                band.edit(Said::Progress(String::from(SAVING)), shell, compositor);
                self.awaiting = Some(AwaitedFolder {
                    window_id,
                    serial,
                    held,
                });
                redraw_band(self, shell, compositor);
                None
            }
        }
    }

    /// Whether it waits on the window `window_id` to name the folder its save
    /// was dropped on with the offer numbered `serial`.
    fn awaits_drop(&self, window_id: u64, serial: u32) -> bool {
        self.awaiting
            .as_ref()
            .is_some_and(|awaited| awaited.window_id == window_id && awaited.serial == serial)
    }

    /// The name the panel's field holds, as a dropped save goes under it, or
    /// `None` with the refusal stated in the panel.
    fn dropped_name(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Held> {
        let band = self.save.as_mut()?;
        let typed = band.panel.name.text().to_string();
        let held = tairix_path::validate_file_name(&typed)
            .map_err(|err| err.to_string())
            .and_then(|()| band.held(typed));
        match held {
            Ok(held) => Some(held),
            Err(refusal) => {
                band.edit(Said::Refusal(refusal), shell, compositor);
                redraw_band(self, shell, compositor);
                None
            }
        }
    }

    /// Save as `held` in the folder at the absolute path `folder`, a name
    /// already there asked about before it is replaced.
    fn save_into(
        &mut self,
        Held { name, typing }: Held,
        folder: &str,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let into = match vfs::components_from_absolute_path(folder) {
            Ok(into) => into,
            Err(err) => {
                self.refuse_save(format!("Not saved: {err}"), shell, compositor);
                return None;
            }
        };
        let Some(path) = shown_path(&into, &name) else {
            self.refuse_save(
                String::from("Not saved: the name is too long there"),
                shell,
                compositor,
            );
            return None;
        };
        self.request_at(
            (name, path, Some(into)),
            (PickAccess::Create, typing),
            cx,
            shell,
            compositor,
        )
    }

    /// State `refusal` in the save panel, which goes back to editing the name.
    fn refuse_save(
        &mut self,
        refusal: String,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if let Some(band) = self.save.as_mut() {
            band.edit(Said::Refusal(refusal), shell, compositor);
            redraw_band(self, shell, compositor);
        }
    }

    /// Whether a press at `local` lands on its *Cancel*.
    fn cancel_pressed(&self, local: Point, theme: &Theme, scale: Scale) -> bool {
        if let Some(band) = self.save.as_ref() {
            return panel_layout(band, scale, theme).hit(local) == Some(PanelHit::Cancel);
        }
        self.folder
            .as_ref()
            .is_some_and(|band| band_layout(band, scale, theme).cancel.contains(local))
    }

    /// Step the press its listing bar holds if its repeat is due at `now_ns`.
    fn scroll_repeat(&mut self, now_ns: u64, shell: &DesktopShell, compositor: &Compositor) {
        if self.question_wm().is_some() || !self.lists() {
            return;
        }
        let scale = compositor.scale();
        let mut drew = damage::sink();
        let viewport = self.viewport(scale);
        if tairix_browse::render::scroll_repeat(
            &mut self.browser,
            (scale, shell.session().active_theme()),
            viewport,
            PICKER_TOOLBAR,
            now_ns,
            &mut drew,
        ) {
            repaint(self, &drew);
        }
    }

    /// Scroll its listing by a wheel turn of `(dx, dy)`, answering whether it
    /// moved.
    fn scroll(
        &mut self,
        (dx, dy): (i32, i32),
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> bool {
        // A wheel over a folded-away listing scrolls nothing anyone can see.
        if self.question_wm().is_some() || !self.lists() {
            return false;
        }
        let scale = compositor.scale();
        let mut moved = damage::sink();
        let viewport = self.viewport(scale);
        if !scroll_wheel(
            &mut self.browser,
            scale,
            shell.session().active_theme(),
            viewport,
            PICKER_TOOLBAR,
            (dx, dy),
            &mut moved,
        ) {
            return false;
        }
        repaint(self, &moved);
        true
    }

    /// Repaint the slots whose pictures the `landed` decodes moved.
    fn artwork_landed(&mut self, landed: &Landed, shell: &DesktopShell, compositor: &Compositor) {
        let lookup = (compositor.scale(), shell.session().active_theme());
        let mut moved = Region::new();
        self.each_shown_art(lookup, &mut |art| {
            if landed.resolves(art.request, art.side) {
                moved.add(art.at);
            }
        });
        if !moved.is_empty() {
            repaint(self, &moved);
        }
    }

    /// Visit each picture the pick shows at `scale` in `theme`: its listing's
    /// rows or tiles while it lists, by the walk its paint draws them by, and
    /// a save's own document picture.
    fn each_shown_art(&self, (scale, theme): (Scale, &Theme), visit: &mut dyn FnMut(ShownArt<'_>)) {
        if self.lists() {
            manager_art(
                &self.browser,
                (scale, theme),
                self.viewport(scale),
                &PICKER_CHROME,
                visit,
            );
        }
        if let Some(band) = self.save.as_ref() {
            let icon = panel_layout(band, scale, theme).icon;
            visit(ShownArt {
                request: IconRequest::kind(band.panel.icon()),
                side: icon.width,
                at: icon,
            });
        }
    }

    /// Redraw it, and its question, in the look and at the scale now in use.
    fn restyle(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        redraw(self, shell, compositor);
        if let Some(replacing) = self.replacing_mut() {
            replacing.question.repaint(shell, compositor);
        }
    }

    /// Run `read` — a listing collected, or one asked afresh — on its browser,
    /// repainting once it answers (`Ok(false)`: not yet). A pick whose folder
    /// went moves to its parent when the folder's read fails.
    fn reread(
        &mut self,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        read: impl FnOnce(&mut Browser<S>) -> Result<bool, BrowseError>,
    ) {
        let (lost, owed) = (self.lost, self.reread_owed);
        let mut in_flight = false;
        let _ = self.navigate(cx, shell, compositor, |browser| {
            let mut outcome = read(browser);
            if outcome.is_err() {
                if lost {
                    outcome = browser.climb();
                } else if owed {
                    outcome = browser.refresh().map(|()| !browser.is_listing());
                }
            }
            in_flight = browser.is_listing();
            if matches!(outcome, Ok(false)) {
                NavOutcome::None
            } else {
                NavOutcome::Redraw
            }
        });
        if !in_flight {
            self.lost = false;
            self.reread_owed = false;
        }
    }

    /// Follow what its folder watch reported.
    fn follow(
        &mut self,
        update: WatchUpdate,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let gone = matches!(update, WatchUpdate::Gone);
        match update {
            WatchUpdate::Quiet => {}
            WatchUpdate::Changes(changes) => {
                let scale = compositor.scale();
                let theme = shell.session().active_theme();
                let viewport = self.viewport(scale);
                let before = shown_listing(&self.browser, scale, theme, viewport, PICKER_TOOLBAR);
                match self.browser.apply_changes(changes) {
                    Ok(Some(_)) => {}
                    Ok(None) => return,
                    // Without the memory to merge them, the folder is read again.
                    Err(_) => return self.follow(WatchUpdate::Rescan, cx, shell, compositor),
                }
                let mut moved = Region::new();
                if self.lists()
                    && listing_damage(
                        &before,
                        &self.browser,
                        scale,
                        theme,
                        viewport,
                        PICKER_TOOLBAR,
                        &mut moved,
                    )
                {
                    repaint(self, &moved);
                }
            }
            WatchUpdate::Rescan | WatchUpdate::Gone => {
                self.lost |= gone;
                self.reread_owed = true;
                if self.browser.is_listing() {
                    return;
                }
                self.reread(cx, shell, compositor, |browser| {
                    browser.refresh().map(|()| !browser.is_listing())
                });
            }
        }
    }

    /// Dismiss it without choosing: what `Escape`, *Cancel* and its close
    /// control mean. `None` for a pick the session made for itself.
    fn cancel(&mut self) -> Option<PickStep> {
        self.end(None);
        let for_window = self.for_window()?;
        Some(PickStep::Cancelled {
            pick: self.id,
            for_window,
        })
    }

    /// What `Escape` or *Cancel* do while it waits: a folder being made is let
    /// go, its answer heeded no more, and the field names the document again;
    /// any other wait is the pick cancelled.
    fn cancel_waiting(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        if self.making.take().is_some() {
            self.leave_naming(shell, compositor);
            return None;
        }
        self.cancel()
    }

    /// The embedder's answer to its open `serial` names.
    fn opened(
        &mut self,
        serial: u64,
        (result, made): (Result<(), Errno>, Option<String>),
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickEnd> {
        let waiting = self.waiting.take_if(|waiting| waiting.serial == serial)?;
        let Waiting {
            mut name,
            mut path,
            access,
            into,
            typing,
            ..
        } = waiting;
        if let Some(made) = made {
            if let Some((_, leaf)) = made.rsplit_once('/') {
                name = String::from(leaf);
            }
            path = made;
        }
        if let (Some(band), Err(err)) = (self.folder.as_mut(), result) {
            band.message = Some(format!("{}: {err}", band.purpose.refused()));
            redraw_band(self, shell, compositor);
            return None;
        }
        let pick = self.id;
        let Some(band) = self.save.as_mut() else {
            self.end(None);
            let for_window = self.for_window()?;
            return Some(match result {
                Ok(()) => PickEnd::Chosen {
                    pick,
                    for_window,
                    name,
                },
                Err(_) => PickEnd::Refused { pick, for_window },
            });
        };
        match result {
            Ok(()) => {
                // Where the file was made, whatever the listing shows by now.
                cx.last_saved = vfs::components_from_absolute_path(&path)
                    .ok()
                    .and_then(|mut folder| folder.pop().map(|_| folder));
                self.end(None);
                let for_window = self.for_window()?;
                Some(PickEnd::Chosen {
                    pick,
                    for_window,
                    name,
                })
            }
            Err(Errno::AlreadyExists) if access == PickAccess::Create => {
                // Asked only about a file the user can see the question is
                // about: one in the folder dropped on, or still showing.
                let shown = self
                    .browser
                    .folder()
                    .and_then(|folder| shown_path(folder, &name));
                if into.is_some() || shown.as_deref() == Some(path.as_str()) {
                    band.ask_to_replace(((name, path, into), typing), self.wm, shell, compositor);
                } else {
                    band.edit(Said::Refusal(already_exists(&name)), shell, compositor);
                }
                redraw_band(self, shell, compositor);
                None
            }
            // The name is taken by something no save replaces.
            Err(Errno::IsADirectory) => {
                band.edit(
                    Said::Refusal(taken_by(&name, "a folder")),
                    shell,
                    compositor,
                );
                redraw_band(self, shell, compositor);
                None
            }
            Err(Errno::LinkLoop) => {
                band.edit(
                    Said::Refusal(taken_by(&name, "a shortcut")),
                    shell,
                    compositor,
                );
                redraw_band(self, shell, compositor);
                None
            }
            Err(err) => {
                band.edit(
                    Said::Refusal(format!("Not saved: {err}")),
                    shell,
                    compositor,
                );
                redraw_band(self, shell, compositor);
                None
            }
        }
    }

    /// A key a save routes to its name field.
    fn save_key(
        &mut self,
        record: &KeyInput,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let InputEvent::KeyPressed { key, modifiers } =
            tairix_input::wire::key_input_event(*record)
        else {
            return None;
        };
        let band = self.save.as_mut()?;
        let (focus, naming, expanded) = (
            band.panel.focus(),
            band.panel.naming(),
            band.panel.expanded(),
        );
        match key {
            Key::Named(NamedKey::Tab) => {
                band.panel.move_focus(modifiers.shift);
                let ringed =
                    (focus == PanelFocus::Listing) != (band.panel.focus() == PanelFocus::Listing);
                redraw_band(self, shell, compositor);
                if ringed {
                    let view = listing_viewport(self.save.as_ref(), compositor.scale());
                    self.owed.add(view);
                }
                return None;
            }
            Key::Char('n' | 'N') if modifiers.ctrl && modifiers.shift && expanded => {
                return self.new_folder(shell, compositor);
            }
            Key::Named(NamedKey::Escape) if naming => {
                self.leave_naming(shell, compositor);
                return None;
            }
            _ => {}
        }
        if focus == PanelFocus::Listing {
            return self.listing_key(key, cx, shell, compositor);
        }
        if focus != PanelFocus::Name {
            return self.control_key(focus, (key, modifiers), cx, shell, compositor);
        }
        let mut drew = damage::sink();
        let band = self.save.as_mut()?;
        let field = panel_layout(band, compositor.scale(), shell.session().active_theme()).name;
        let action = band.panel.name.on_key(key, modifiers, field, &mut drew);
        match action {
            Some(TextAction::Submitted) => return self.submit(cx, shell, compositor),
            Some(TextAction::Cancelled) => return self.cancel(),
            // A refusal of the name before is not one of this name, and the
            // ending typed may be another format.
            Some(TextAction::Edited) => {
                band.edit(Said::Nothing, shell, compositor);
                band.panel.restate_format();
                redraw_band(self, shell, compositor);
            }
            None if !drew.is_empty() => repaint(self, &drew),
            None => {}
        }
        None
    }

    /// A key on the panel control `focus`, which is no text field: `Enter` or
    /// `Space` answers it, `Down` opens a chooser, and `Escape` cancels.
    fn control_key(
        &mut self,
        focus: PanelFocus,
        (key, modifiers): (Key, Modifiers),
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let activates = matches!(key, Key::Named(NamedKey::Enter) | Key::Char(' '))
            && !modifiers.ctrl
            && !modifiers.alt;
        let opens = activates || key == Key::Named(NamedKey::Down);
        if key == Key::Named(NamedKey::Escape) {
            return self.cancel();
        }
        let hit = match focus {
            PanelFocus::Where if opens => PanelHit::Where,
            PanelFocus::Format if opens => PanelHit::Format,
            PanelFocus::Browse if activates => PanelHit::Browse,
            PanelFocus::NewFolder if activates => PanelHit::NewFolder,
            PanelFocus::Cancel if activates => PanelHit::Cancel,
            PanelFocus::Save if activates => PanelHit::Save,
            _ => return None,
        };
        self.panel_answer(hit, cx, shell, compositor)
    }

    /// A key on the save's folder listing: `Up` and `Down` move the
    /// selection, `Enter` goes into a folder or offers a file's name, and
    /// `Backspace` climbs to the folder above.
    fn listing_key(
        &mut self,
        key: Key,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        match key {
            Key::Named(NamedKey::Down) => self.navigate(cx, shell, compositor, |browser| {
                browser.select_next();
                NavOutcome::Redraw
            }),
            Key::Named(NamedKey::Up) => self.navigate(cx, shell, compositor, |browser| {
                browser.select_previous();
                NavOutcome::Redraw
            }),
            Key::Named(NamedKey::Enter) => {
                let index = self.browser.chosen_index()?;
                self.offer_entry(index, cx, shell, compositor)
            }
            Key::Named(NamedKey::Backspace) => self.navigate(cx, shell, compositor, |browser| {
                if browser.go_up().unwrap_or(false) {
                    NavOutcome::Redraw
                } else {
                    NavOutcome::None
                }
            }),
            Key::Named(NamedKey::Escape) => self.cancel(),
            _ => None,
        }
    }

    /// Name a new folder in the folder shown, the field offering the first
    /// name no entry there holds.
    fn new_folder(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let suggested = tairix_browse::NewEntry::Folder.suggest_name(self.browser.entries());
        let band = self.save.as_mut()?;
        band.edit(Said::Nothing, shell, compositor);
        band.panel.begin_naming(&suggested);
        redraw_band(self, shell, compositor);
        None
    }

    /// Stop naming a new folder: the field holds the document's name again.
    fn leave_naming(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if let Some(band) = self.save.as_mut() {
            band.edit(Said::Nothing, shell, compositor);
            band.panel.end_naming();
            redraw_band(self, shell, compositor);
        }
    }

    /// Ask the embedder to make the folder the field names in the folder
    /// shown, a name the listing already shows refused here.
    fn make_folder(
        &mut self,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let shown = self.browser.folder()?.to_vec();
        let band = self.save.as_mut()?;
        let name = band.panel.name.text().to_string();
        let checked = tairix_browse::validate_new_entry_name(&name, self.browser.entries())
            .map_err(|err| String::from(err.message()))
            .and_then(|()| {
                shown_path(&shown, &name).ok_or_else(|| String::from("That name is too long."))
            });
        let path = match checked {
            Ok(path) => path,
            Err(refusal) => {
                band.edit(Said::Refusal(refusal), shell, compositor);
                redraw_band(self, shell, compositor);
                return None;
            }
        };
        let mut into = shown;
        into.push(name);
        cx.serial += 1;
        let serial = cx.serial;
        band.edit(Said::Progress(String::from(MAKING)), shell, compositor);
        self.making = Some((serial, into));
        redraw_band(self, shell, compositor);
        Some(PickStep::NewFolder {
            pick: self.id,
            serial,
            path,
        })
    }

    /// The embedder's answer to the folder `serial` asked to be made.
    fn folder_made(
        &mut self,
        serial: u64,
        made: Result<(), Errno>,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some((_, into)) = self.making.take_if(|(asked, _)| *asked == serial) else {
            return;
        };
        let Some(band) = self.save.as_mut() else {
            return;
        };
        if let Err(err) = made {
            band.edit(Said::Refusal(format!("Not made: {err}")), shell, compositor);
            redraw_band(self, shell, compositor);
            return;
        }
        band.edit(Said::Nothing, shell, compositor);
        band.panel.end_naming();
        let _ = self.navigate(cx, shell, compositor, move |browser| {
            match browser.navigate_to(into) {
                Ok(_) => NavOutcome::Redraw,
                Err(_) => NavOutcome::None,
            }
        });
    }

    /// A press below the listing, or on a save's panel.
    fn band_click(
        &mut self,
        local: Point,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        if let Some(band) = self.folder.as_ref() {
            let layout = band_layout(band, scale, theme);
            if layout.open.contains(local) {
                return self.choose_folder(cx, shell, compositor);
            }
            if layout.cancel.contains(local) {
                return self.cancel();
            }
            return None;
        }
        let band = self.save.as_mut()?;
        let layout = panel_layout(band, scale, theme);
        match layout.hit(local)? {
            // While the field names a folder it holds no document name to
            // carry, so the icon starts no drag.
            PanelHit::Icon => {
                if !band.panel.naming() {
                    let icon = layout.icon;
                    let grab = PictureGrab {
                        offset: (
                            local.x - icon.left() - to_i32(icon.width / 2),
                            local.y - icon.top() - to_i32(icon.height / 2),
                        ),
                        side: icon.width,
                    };
                    self.icon_press = Some((local, grab));
                }
                None
            }
            PanelHit::Name => {
                band.panel.set_focus(PanelFocus::Name);
                let mut drew = damage::sink();
                // The router reports a click, so the press and the release land
                // where it was pressed.
                for event in [
                    InputEvent::PointerMoved { to: local },
                    InputEvent::PointerPressed {
                        button: PointerButton::Primary,
                    },
                    InputEvent::PointerReleased {
                        button: PointerButton::Primary,
                    },
                ] {
                    let _ =
                        band.panel
                            .name
                            .on_pointer(&event, layout.name, scale, theme, &mut drew);
                }
                redraw_band(self, shell, compositor);
                None
            }
            hit => self.panel_answer(hit, cx, shell, compositor),
        }
    }

    /// Answer the save panel's control `hit`, pressed or reached from the
    /// keyboard: while a folder is named, *Save* makes it and *Cancel* goes
    /// back to the document's name.
    fn panel_answer(
        &mut self,
        hit: PanelHit,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let (scale, theme) = (compositor.scale(), shell.session().active_theme());
        let band = self.save.as_mut()?;
        let naming = band.panel.naming();
        let layout = panel_layout(band, scale, theme);
        match hit {
            PanelHit::Save if naming => self.make_folder(cx, shell, compositor),
            PanelHit::Save => self.submit(cx, shell, compositor),
            PanelHit::Cancel if naming => {
                self.leave_naming(shell, compositor);
                None
            }
            PanelHit::Cancel => self.cancel(),
            PanelHit::Browse => {
                let expanded = !band.panel.expanded();
                if expanded {
                    band.listing_height = save_listing_height(
                        (band, self.browser.view_mode()),
                        self.wm,
                        shell,
                        compositor,
                    );
                }
                band.panel.set_expanded(expanded);
                refit(self, shell, compositor);
                None
            }
            PanelHit::NewFolder => self.new_folder(shell, compositor),
            PanelHit::Where => self.open_where(layout.where_box, cx, compositor),
            PanelHit::Format if naming => None,
            PanelHit::Format => {
                let anchor = layout.format_box?;
                self.open_menu(PanelMenu::Format, anchor, compositor)
            }
            PanelHit::Icon | PanelHit::Name => None,
        }
    }

    /// Open *Where*: the folder shown, the user's own places and the last
    /// save's folder.
    fn open_where(
        &mut self,
        anchor: Rect,
        cx: &Shared,
        compositor: &Compositor,
    ) -> Option<PickStep> {
        let offered = places(&cx.start, self.browser.folder(), cx.last_saved.as_deref());
        self.open_menu(PanelMenu::Where(offered), anchor, compositor)
    }

    /// Ask the embedder to open `menu` from the panel's control at `anchor`,
    /// window-local.
    fn open_menu(
        &mut self,
        menu: PanelMenu,
        anchor: Rect,
        compositor: &Compositor,
    ) -> Option<PickStep> {
        let client = compositor.window_client_rect(self.wm)?;
        let model = match &menu {
            PanelMenu::Where(offered) => where_menu(offered, self.browser.folder()),
            PanelMenu::Format => self.save.as_ref()?.panel.format_menu(),
        };
        self.menu = Some(menu);
        Some(PickStep::Menu {
            pick: self.id,
            model,
            anchor: Rect::new(
                client.left().saturating_add(anchor.left()),
                client.top().saturating_add(anchor.top()),
                anchor.width,
                anchor.height,
            ),
            for_window: self.for_window(),
        })
    }

    /// What became of a menu its save panel opened.
    fn menu_answered(
        &mut self,
        outcome: &ChainOutcome,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        let ChainOutcome::Chosen(item) = outcome else {
            return;
        };
        let index = item.index();
        match menu {
            PanelMenu::Where(offered) => {
                let Some(place) = offered.into_iter().nth(index) else {
                    return;
                };
                let _ = self.navigate(cx, shell, compositor, move |browser| {
                    match browser.navigate_to(place.components) {
                        Ok(_) => NavOutcome::Redraw,
                        Err(_) => NavOutcome::None,
                    }
                });
            }
            PanelMenu::Format => {
                let Some(band) = self.save.as_mut() else {
                    return;
                };
                if band.panel.set_format(index) {
                    band.edit(Said::Nothing, shell, compositor);
                    redraw_band(self, shell, compositor);
                }
            }
        }
    }

    /// A click on the listing's entry `index` in a save: a folder is gone
    /// into, and a file offers its name.
    fn offer_entry(
        &mut self,
        index: usize,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let entry = self.browser.entries().get(index)?;
        if entry.is_directory() {
            return self.navigate(cx, shell, compositor, move |browser| {
                match browser.open_index(index) {
                    Ok(()) => NavOutcome::Redraw,
                    Err(_) => NavOutcome::None,
                }
            });
        }
        let name = String::from(entry.name());
        let band = self.save.as_mut()?;
        // The field names a folder being made, which a file's name is not.
        if band.panel.naming() {
            return None;
        }
        band.panel.offer(&name);
        band.edit(Said::Nothing, shell, compositor);
        redraw_band(self, shell, compositor);
        None
    }

    /// Save under the name the field holds — or, while asked, replace the file
    /// it names.
    fn submit(
        &mut self,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let band = self.save.as_mut()?;
        if band.panel.naming() {
            return self.make_folder(cx, shell, compositor);
        }
        let typed = band.panel.name.text().to_string();
        // A name naming a folder goes into it, whatever a file must end in.
        let folder = self
            .browser
            .entries()
            .iter()
            .position(|entry| entry.name() == typed && entry.kind().is_directory());
        let held = tairix_path::validate_file_name(&typed)
            .map_err(|err| err.to_string())
            .and_then(|()| match folder {
                Some(_) => Ok(Held {
                    name: typed,
                    typing: Typing::default(),
                }),
                None => band.held(typed),
            });
        let Held { name, typing } = match held {
            Ok(held) => held,
            Err(refusal) => {
                band.edit(Said::Refusal(refusal), shell, compositor);
                redraw_band(self, shell, compositor);
                return None;
            }
        };
        if typing.bare && folder.is_none() {
            // Its name is the folder's to finish, so whether it is taken is
            // known once the file is asked for: an exclusive create asks.
            return self.request(name, (PickAccess::Create, typing), cx, shell, compositor);
        }
        let taken = folder.or_else(|| {
            self.browser
                .entries()
                .iter()
                .position(|entry| entry.name() == name)
        });
        let Some(index) = taken else {
            // A name the listing does not show is created exclusively, so one
            // that appeared since is asked about rather than overwritten.
            return self.request(name, (PickAccess::Create, typing), cx, shell, compositor);
        };
        let kind = self.browser.entries()[index].kind();
        match kind {
            EntryKind::File => {
                let path = shown_path(self.browser.folder()?, &name)?;
                band.ask_to_replace(((name, path, None), typing), self.wm, shell, compositor);
                redraw_band(self, shell, compositor);
                None
            }
            // Naming a folder goes into it, and the name offered comes back.
            _ if kind.is_directory() => {
                let suggested = band.panel.suggested.clone();
                band.panel.offer(&suggested);
                band.edit(Said::Nothing, shell, compositor);
                self.navigate(cx, shell, compositor, move |browser| {
                    match browser.open_index(index) {
                        Ok(()) => NavOutcome::Redraw,
                        Err(_) => NavOutcome::None,
                    }
                })
            }
            _ => {
                let what = if kind.is_bundle() {
                    "an application"
                } else {
                    "a shortcut"
                };
                band.edit(Said::Refusal(taken_by(&name, what)), shell, compositor);
                redraw_band(self, shell, compositor);
                None
            }
        }
    }

    /// Choose the folder the picker shows, asking the embedder to delegate
    /// its files or the folder itself.
    fn choose_folder(
        &mut self,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let purpose = self.folder.as_ref()?.purpose;
        let components = self.browser.folder()?;
        // The top level is every place the user reaches, not a folder to hand
        // over.
        if purpose == FolderUse::Whole && components.is_empty() {
            if let Some(band) = self.folder.as_mut() {
                band.message = Some(String::from(TOP_NOT_SHARED));
            }
            redraw_band(self, shell, compositor);
            return None;
        }
        let path = vfs::absolute_path(components).ok()?;
        let name = components
            .last()
            .map_or_else(|| String::from("/"), String::clone);
        self.request_at(
            (name, path, None),
            (purpose.access(), Typing::default()),
            cx,
            shell,
            compositor,
        )
    }

    /// Ask the embedder to open the file `name` names in the directory the
    /// picker shows.
    fn request(
        &mut self,
        name: String,
        how: (PickAccess, Typing),
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        // A name the shared spelling refuses is not chosen rather than
        // guessed at.
        let path = shown_path(self.browser.folder()?, &name)?;
        self.request_at((name, path, None), how, cx, shell, compositor)
    }

    /// Ask the embedder to open the file `name` at `path`, in the folder
    /// `into` a save was dropped in or, for `None`, the one shown; the
    /// session's own pick keeps the path and ends.
    fn request_at(
        &mut self,
        (name, path, into): (String, String, Option<Vec<String>>),
        (access, typing): (PickAccess, Typing),
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let Some(for_window) = self.for_window() else {
            self.end(Some(path));
            return None;
        };
        cx.serial += 1;
        let serial = cx.serial;
        if let Some(band) = self.save.as_mut() {
            band.edit(
                Said::Progress(saving_into(into.as_deref())),
                shell,
                compositor,
            );
            redraw_band(self, shell, compositor);
        }
        if let Some(band) = self.folder.as_mut() {
            band.message = Some(String::from("Opening…"));
            redraw_band(self, shell, compositor);
        }
        self.waiting = Some(Waiting {
            serial,
            name,
            path: path.clone(),
            access,
            into,
            typing,
        });
        Some(PickStep::Open {
            pick: self.id,
            serial,
            for_window,
            path,
            access,
            typing,
        })
    }

    /// Run one navigation step against its browser, repaint on a change,
    /// retitle the window when the step moved to another directory, and ask
    /// for the file when the step chose one.
    fn navigate(
        &mut self,
        cx: &mut Shared,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        step: impl FnOnce(&mut Browser<S>) -> NavOutcome,
    ) -> Option<PickStep> {
        let scale = compositor.scale();
        let titled = self.title_at(self.browser.folder());
        match step(&mut self.browser) {
            NavOutcome::None => None,
            NavOutcome::Redraw => {
                self.restate_where();
                // Keep the (possibly moved) selection on screen before the
                // repaint, scrolling the shared view the least it can.
                let viewport = self.viewport(scale);
                reveal_selection(
                    &mut self.browser,
                    scale,
                    shell.session().active_theme(),
                    viewport,
                    PICKER_TOOLBAR,
                );
                redraw(self, shell, compositor);
                // The picker is session-owned and has no window channel of
                // its own, so it retitles through the compositor. A step that
                // only moved the selection leaves the title alone rather than
                // re-presenting the taskbar for an unchanged label.
                let located = self.title_at(self.browser.folder());
                if located != titled {
                    shell.retitle_window(compositor, self.wm, &located);
                    // A question about a file in the folder left behind is
                    // not one about anything showing now.
                    let asked = self.save.as_mut().filter(|band| band.replacing.is_some());
                    if let Some(band) = asked {
                        band.edit(Said::Nothing, shell, compositor);
                        redraw_band(self, shell, compositor);
                    }
                    // A refusal stated for the folder left behind is not one
                    // about the folder showing now.
                    let stated = self.folder.as_mut().filter(|band| band.message.is_some());
                    if let Some(band) = stated {
                        band.message = None;
                        redraw_band(self, shell, compositor);
                    }
                }
                None
            }
            NavOutcome::Chosen(name) => self.request(
                name,
                (self.reading, Typing::default()),
                cx,
                shell,
                compositor,
            ),
        }
    }
}

/// Keep, of a chosen folder's `entries`, the regular files whose content type
/// — by name, else by the filetype their volume records — is one of `kinds`,
/// in the order the picker lists them and at most the most one folder pick
/// delegates, answering how many matching files the bound left out.
///
/// A link is never kept: a pick hands over only what the folder itself holds.
pub fn folder_selection(entries: &mut Vec<Entry>, kinds: &[String]) -> u32 {
    entries.retain(|entry| {
        entry.kind() == EntryKind::File
            && tairix_browse::media_for_document(entry.name(), entry.filetype())
                .is_some_and(|media| kinds.iter().any(|kind| kind == media.as_str()))
    });
    tairix_browse::sort_entries(entries, tairix_browse::SortMode::default_order().into());
    let left_out = entries
        .len()
        .saturating_sub(tairix_abi::window_ipc::WINDOW_FOLDER_PICK_MAX);
    entries.truncate(tairix_abi::window_ipc::WINDOW_FOLDER_PICK_MAX);
    u32::try_from(left_out).unwrap_or(u32::MAX)
}

/// The path of the file `name` in the folder `components` names, or `None`
/// for a name the shared spelling refuses.
fn shown_path(components: &[String], name: &str) -> Option<String> {
    let mut path = components.to_vec();
    path.push(String::from(name));
    vfs::absolute_path(&path).ok()
}

impl<S: DirectorySource, F: PickSources<S>> PickerSlot for SessionPicker<S, F> {
    fn begin(
        &mut self,
        ask: &PickAsk<'_>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(PickId, WindowId), Errno> {
        self.open_pick(
            (Requester::Window(ask.window_id), ask.held),
            (ask.speaker, ask.purpose, ask.focus),
            shell,
            compositor,
        )
    }

    fn abort_for(&mut self, window_id: u64, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if let Some(pick) = self.by_requester.get(&window_id).copied() {
            // The window it held is going, so the keyboard goes nowhere
            // through it.
            if let Some(open) = self.picks.get_mut(&pick) {
                open.held = None;
            }
            self.close(pick, None, shell, compositor);
        }
        // A save dropped on a window that has gone will hear of no folder.
        self.each(shell, compositor, |open, _, shell, compositor| {
            if open
                .awaiting
                .take_if(|awaited| awaited.window_id == window_id)
                .is_some()
            {
                open.refuse_save(String::from(TARGET_GONE), shell, compositor);
            }
        });
    }

    fn save_folder_answered(
        &mut self,
        (window_id, serial): (u64, u32),
        folder: Option<String>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Errno> {
        let pick = self
            .picks
            .iter()
            .find_map(|(pick, open)| open.awaits_drop(window_id, serial).then_some(*pick))
            .ok_or(Errno::NotFound)?;
        let _ = self.turn(pick, shell, compositor, |open, cx, shell, compositor| {
            let Some(awaited) = open.awaiting.take() else {
                return;
            };
            match folder {
                Some(folder) => {
                    open.step = open.save_into(awaited.held, &folder, cx, shell, compositor);
                }
                None => open.refuse_save(String::from(NOT_GIVEN), shell, compositor),
            }
        });
        Ok(())
    }

    fn holds_keyboard(&self, focused: Option<WindowId>) -> bool {
        Self::holds_keyboard(self, focused)
    }
}

/// What a save's *Where* names `folder` by: its own name, or the root's.
fn folder_label(folder: Option<&[String]>) -> &str {
    folder.map_or("/", |folder| folder.last().map_or("/", String::as_str))
}

/// Spell the picker window's title: the fixed `prefix` and the directory the
/// picker is showing, fitted to the bounded title field.
///
/// `components` is the browser's own root-first location, never a path an app
/// supplied. Fitting is the shared title spelling
/// ([`vfs::spell_title_location`]), which drops whole leading components
/// behind the shared ellipsis and always keeps the folder the user is in, so
/// the result never exceeds [`WINDOW_TITLE_MAX`] bytes.
#[must_use]
fn picker_title(prefix: &str, folder: Option<&[String]>) -> String {
    let budget = WINDOW_TITLE_MAX - prefix.len() - PICKER_TITLE_SEPARATOR.len();
    let mut title = String::from(prefix);
    title.push_str(PICKER_TITLE_SEPARATOR);
    title.push_str(&vfs::spell_title_of(folder, budget));
    title
}

/// What one navigation step did.
enum NavOutcome {
    /// Nothing changed (a refused move, an unresolvable click).
    None,
    /// The view changed; repaint the picker window.
    Redraw,
    /// The user chose the regular file of this name in the directory shown.
    Chosen(String),
}

/// Descend into the entry at `index` when it is a directory, or choose it
/// when it is a regular file — the one open-or-choose rule the Enter key
/// and the row click share.
fn open_or_choose<S: DirectorySource>(browser: &mut Browser<S>, index: usize) -> NavOutcome {
    let Some(entry) = browser.entries().get(index) else {
        return NavOutcome::None;
    };
    if entry.is_directory() {
        return descend(browser, index);
    }
    NavOutcome::Chosen(String::from(entry.name()))
}

/// Descend into the entry at `index` when it is a directory; anything else,
/// and a refused descent, changes nothing.
fn descend<S: DirectorySource>(browser: &mut Browser<S>, index: usize) -> NavOutcome {
    if !browser
        .entries()
        .get(index)
        .is_some_and(Entry::is_directory)
    {
        return NavOutcome::None;
    }
    match browser.open_index(index) {
        Ok(()) => NavOutcome::Redraw,
        Err(_) => NavOutcome::None,
    }
}

/// A save dialog's width, in logical pixels: its panel's, and the folder
/// browser's above it while that shows.
const SAVE_WIDTH: u32 = 520;

/// The tallest a save's folder browser opens, in logical pixels: half an
/// open's, so the panel stays the heart of the dialog.
const SAVE_LISTING_HEIGHT: u32 = WIN_HEIGHT / 2;

/// The listing's part of the picker window at `scale`, which every paint and
/// hit-test of the listing lays out in: the shared browser view for an open
/// or a folder pick, the browser a save shows above its panel for a save.
fn listing_viewport(save: Option<&SaveBand>, scale: Scale) -> Rect {
    match save {
        Some(band) => Rect::new(0, 0, scale.scale_length(SAVE_WIDTH), band.listing_height),
        None => Rect::new(
            0,
            0,
            scale.scale_length(WIN_WIDTH),
            scale.scale_length(WIN_HEIGHT),
        ),
    }
}

/// Where a folder band's controls sit, in picker-window-local pixels.
struct BandLayout {
    band: Rect,
    field: Rect,
    cancel: Rect,
    open: Rect,
}

/// The folder band `folder` under the listing at `scale`: the line it states
/// what choosing does, or a refusal, and the two answers beside it.
fn band_layout(folder: &FolderBand, scale: Scale, theme: &Theme) -> BandLayout {
    let listing = listing_viewport(None, scale);
    let metrics = theme.metrics();
    let inset = scale.scale_length(metrics.control_inset);
    let gap = scale.scale_length(metrics.control_gap);
    let row = TextField::height(scale, theme);
    let message = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale)
        .line_height()
        .saturating_add(inset);
    let height = inset
        .saturating_mul(2)
        .saturating_add(row)
        .saturating_add(message);
    let top = listing.bottom();
    let band = Rect::new(0, top, listing.width, height);
    let open_w = folder.open.measured_width(scale, theme);
    let cancel_w = folder.cancel.measured_width(scale, theme);
    let row_top = top.saturating_add(i32::try_from(inset).unwrap_or(i32::MAX));
    let right = listing.width.saturating_sub(inset);
    let open_x = right.saturating_sub(open_w);
    let cancel_x = open_x.saturating_sub(gap).saturating_sub(cancel_w);
    let field_w = cancel_x.saturating_sub(gap).saturating_sub(inset);
    let at = |x: u32| i32::try_from(x).unwrap_or(i32::MAX);
    BandLayout {
        band,
        field: Rect::new(at(inset), row_top, field_w, row.saturating_add(message)),
        cancel: Rect::new(at(cancel_x), row_top, cancel_w, row),
        open: Rect::new(at(open_x), row_top, open_w, row),
    }
}

/// Where a save's panel sits: under the listing while the browser shows, the
/// whole window otherwise.
fn panel_layout(band: &SaveBand, scale: Scale, theme: &Theme) -> PanelLayout {
    let listing = listing_viewport(Some(band), scale);
    let top = if band.panel.expanded() {
        listing.bottom()
    } else {
        0
    };
    band.panel.layout(top, listing.width, scale, theme)
}

/// The picker window's physical size: the listing, and the folder band under
/// it for a folder; a save's panel, under the listing while its browser shows.
fn pick_size(
    save: Option<&SaveBand>,
    folder: Option<&FolderBand>,
    scale: Scale,
    theme: &Theme,
) -> (u32, u32) {
    let listing = listing_viewport(save, scale);
    if let Some(band) = save {
        let panel = panel_layout(band, scale, theme).panel;
        return (
            listing.width,
            u32::try_from(panel.bottom()).unwrap_or(listing.height),
        );
    }
    if let Some(folder) = folder {
        let band = band_layout(folder, scale, theme).band;
        return (listing.width, listing.height.saturating_add(band.height));
    }
    (listing.width, listing.height)
}

/// How tall a save's browser opens over the window `wm`: its own height,
/// less what the work area cannot hold beside the panel and the frame, but
/// never shorter than a listing needs to show a row.
fn save_listing_height(
    (band, view): (&SaveBand, ViewMode),
    wm: WindowId,
    shell: &DesktopShell,
    compositor: &Compositor,
) -> u32 {
    let scale = compositor.scale();
    let theme = shell.session().active_theme();
    let frame = compositor.window(wm).map_or(0, |window| {
        window
            .bounds()
            .height
            .saturating_sub(window.client_rect().height)
    });
    let room = shell
        .work_area(compositor)
        .height
        .saturating_sub(frame)
        .saturating_sub(panel_layout(band, scale, theme).panel.height);
    let floor = listing_floor_height(view, PICKER_TOOLBAR, scale, theme);
    scale.scale_length(SAVE_LISTING_HEIGHT).min(room).max(floor)
}

/// Paint the picker's listing into `surface`, its pictures through `artwork`.
/// The picker draws no manager chrome: no write tools, which are the file
/// manager's alone, and no places rail, since a pick is bounded to the tree
/// its requester may be shown.
fn paint_listing<S: DirectorySource>(
    surface: &mut Surface,
    browser: &Browser<S>,
    (save, scale, theme): (Option<&SaveBand>, Scale, &Theme),
    artwork: &mut dyn IconArtwork,
) {
    render_into(
        surface,
        browser,
        scale,
        theme,
        listing_viewport(save, scale),
        &PICKER_CHROME,
        artwork,
    );
}

/// Paint the pick: the listing where it shows, then a save's panel or a
/// folder's band, every picture through `artwork`.
fn paint_pick<S: DirectorySource>(
    surface: &mut Surface,
    browser: &Browser<S>,
    (save, folder): (Option<&SaveBand>, Option<&FolderBand>),
    (scale, theme): (Scale, &Theme),
    artwork: &mut dyn IconArtwork,
) {
    if save.is_none_or(|band| band.panel.expanded()) {
        paint_listing(surface, browser, (save, scale, theme), artwork);
        if let Some(band) = save.filter(|band| band.panel.focus() == PanelFocus::Listing) {
            let view = listing_viewport(Some(band), scale);
            if let (Ok(x), Ok(y)) = (u32::try_from(view.left()), u32::try_from(view.top())) {
                paint_focus_ring(surface, (x, y, view.width, view.height), scale, theme);
            }
        }
    }
    if let Some(band) = save {
        band.panel.render(
            surface,
            &panel_layout(band, scale, theme),
            scale,
            theme,
            artwork,
        );
    }
    if let Some(band) = folder {
        paint_folder_band(surface, band, scale, theme);
    }
}

/// Paint a folder pick's band under the listing.
fn paint_folder_band(surface: &mut Surface, band: &FolderBand, scale: Scale, theme: &Theme) {
    let layout = band_layout(band, scale, theme);
    let palette = theme.palette();
    let (Ok(x), Ok(y)) = (
        u32::try_from(layout.band.left()),
        u32::try_from(layout.band.top()),
    ) else {
        return;
    };
    surface.fill_rect(
        x,
        y,
        layout.band.width,
        layout.band.height,
        palette.surface.into(),
    );
    surface.fill_rect(x, y, layout.band.width, 1, palette.on_surface_muted.into());
    {
        let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let text = band.message.as_deref().unwrap_or(band.purpose.prompt());
        let colour = if band.message.is_some() {
            palette.on_surface
        } else {
            palette.on_surface_muted
        };
        let (Ok(left), Ok(top)) = (
            u32::try_from(layout.field.left()),
            u32::try_from(layout.field.top()),
        ) else {
            return;
        };
        surface.with_clip(
            left,
            top,
            layout.field.width,
            layout.field.height,
            |surface| {
                font.draw_text(
                    surface,
                    layout.field.left(),
                    layout.field.top(),
                    text,
                    colour.into(),
                );
            },
        );
        band.cancel.render(surface, layout.cancel, scale, theme);
        band.open.render(surface, layout.open, scale, theme);
    }
}

/// Owe the parts of the picker window `area` covers a paint, made once the
/// input that changed them has drained.
fn repaint<S: DirectorySource>(active: &mut Pick<S>, area: &Region) {
    active.owed.add_region(area);
}

/// Paint what `active` owes into the buffer its window already holds,
/// answering whether it was painted. A buffer that cannot be kept is painted
/// whole, and one the heap will not give leaves the previous frame on screen
/// (fail closed) and the paint still owed.
fn paint<S: DirectorySource>(
    active: &Pick<S>,
    shell: &mut DesktopShell,
    compositor: &mut Compositor,
) -> bool {
    let scale = compositor.scale();
    let (theme, mut artwork) = shell.theme_and_artwork();
    let parts = (active.save.as_ref(), active.folder.as_ref());
    let size = pick_size(parts.0, parts.1, scale, theme);
    compositor.repaint_window(active.wm, size, &active.owed, |surface, rects| {
        for rect in rects {
            let Some((x, y)) = rect.surface_origin() else {
                continue;
            };
            surface.with_clip(x, y, rect.width, rect.height, |surface| {
                paint_pick(
                    surface,
                    &active.browser,
                    parts,
                    (scale, theme),
                    &mut artwork,
                );
            });
        }
    })
}

/// Owe the whole picker window a paint after a navigation change: a step
/// changes what the window shows, never its size.
fn redraw<S: DirectorySource>(pick: &mut Pick<S>, shell: &DesktopShell, compositor: &Compositor) {
    let (width, height) = pick.size(shell, compositor);
    pick.owed.add(Rect::new(0, 0, width, height));
}

/// Owe a save's panel or a folder's band a paint alone: a change to the name,
/// a question, progress, or a refusal moves nothing in the listing.
fn redraw_band<S: DirectorySource>(
    active: &mut Pick<S>,
    shell: &DesktopShell,
    compositor: &Compositor,
) {
    let (scale, theme) = (compositor.scale(), shell.session().active_theme());
    let band = match (active.save.as_ref(), active.folder.as_ref()) {
        (Some(save), _) => panel_layout(save, scale, theme).panel,
        (None, Some(folder)) => band_layout(folder, scale, theme).band,
        (None, None) => return,
    };
    active.owed.add(band);
}

/// Put the picker window at the size of what it now shows, painted whole — a
/// save's browser shown or folded away — keeping its bottom edge where it was,
/// so the panel the pointer is on stays put. A window that would leave the
/// work area comes back onto it.
fn refit<S: DirectorySource>(
    active: &mut Pick<S>,
    shell: &mut DesktopShell,
    compositor: &mut Compositor,
) {
    let scale = compositor.scale();
    let parts = (active.save.as_ref(), active.folder.as_ref());
    let before = compositor.window(active.wm).map(tairix_wm::Window::bounds);
    {
        let (theme, mut artwork) = shell.theme_and_artwork();
        let (width, height) = pick_size(parts.0, parts.1, scale, theme);
        let Some(mut surface) = Surface::new(width, height) else {
            return;
        };
        paint_pick(
            &mut surface,
            &active.browser,
            parts,
            (scale, theme),
            &mut artwork,
        );
        if !compositor.set_surface(active.wm, surface) {
            return;
        }
    }
    // Painted whole, so nothing earlier is owed any more.
    active.owed.clear();
    let after = compositor.window(active.wm).map(tairix_wm::Window::bounds);
    if let (Some(before), Some(after)) = (before, after) {
        let rise = i32::try_from(after.height)
            .unwrap_or(i32::MAX)
            .saturating_sub(i32::try_from(before.height).unwrap_or(i32::MAX));
        compositor.move_window(
            active.wm,
            Point::new(after.origin.x, before.origin.y.saturating_sub(rise)),
        );
    }
    shell.keep_on_work_area(compositor, active.wm);
}

/// The question whether to replace the file `name` at `path`, each named as
/// shown, so a save dropped on a folder other than the one shown says which.
fn replace_question(name: &str, path: &str) -> Option<tairix_abi::window_ipc::AlertSpec> {
    let body = match path.rsplit('/').nth(1).filter(|folder| !folder.is_empty()) {
        Some(folder) => format!(
            "A file of that name is already in \u{201c}{}\u{201d}. Replacing it overwrites \
             what it holds.",
            shown(folder)
        ),
        None => String::from(
            "A file of that name is already there. Replacing it overwrites what it holds.",
        ),
    };
    question(
        &format!(
            "\u{201c}{}\u{201d} already exists. Replace it?",
            shown(name)
        ),
        &body,
        CANCEL_LABEL,
        (REPLACE_LABEL, AlertButtonRole::Destructive),
    )
}

/// What the status line says while a save is written: the folder it goes
/// into where it was dropped there rather than saved into the one shown.
fn saving_into(into: Option<&[String]>) -> String {
    match into.and_then(<[String]>::last) {
        Some(folder) => format!("Saving into \u{201c}{}\u{201d}\u{2026}", shown(folder)),
        None => String::from(SAVING),
    }
}

/// What the band says of a save refused because `name` is taken.
fn already_exists(name: &str) -> String {
    format!("Not saved: \u{201c}{}\u{201d} already exists", shown(name))
}

/// What the panel says when the window a save was dropped on names no folder.
const NOT_GIVEN: &str = "Not saved: the folder there could not be reached";

/// What the panel says of a save dropped before the window under it said
/// whether it takes one.
const UNANSWERED: &str = "Not saved: the window had not answered; drop it there again";

/// What the panel says when the window a save was dropped on closes before
/// naming its folder.
const TARGET_GONE: &str = "Not saved: the window it was dropped on has closed";

/// What the band says of a name `what`, an application or a shortcut,
/// already holds.
fn taken_by(name: &str, what: &str) -> String {
    format!(
        "\u{201c}{}\u{201d} is {what}; choose another name",
        shown(name)
    )
}

#[cfg(test)]
#[path = "picker_tests.rs"]
mod tests;
