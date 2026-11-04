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
//! [`SessionPicker`] is the host-testable engine: the single picker slot
//! (one pick UI at a time, the session's modality policy), the browser
//! state, and the navigation that ends in a [`PickStep`]. The privileged
//! tail — opening the chosen file and minting the delegation — stays in the
//! session's `Run` binary, which holds the syscalls and carries the open out
//! off its loop; the picker waits for that answer ([`SessionPicker::opened`])
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

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::fs::Filetype;
use tairix_abi::fs::OpenFlags;
use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::window_ipc::{AlertButtonRole, PickPurpose, SaveFormats, WINDOW_TITLE_MAX};
use tairix_abi::Errno;
use tairix_browse::media::media_for_name;
use tairix_browse::render::{
    entry_index_at, header_sort_at, listing_damage, listing_floor_height, manager_art, render_into,
    reveal_selection, scroll_pointer, scroll_wheel, shown_listing, toolbar_command_at,
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

use crate::choice::{question, Choice, ChoicePrompt};
use crate::drag::{PictureGrab, SaveDrop, SavedDocument};
use crate::menu::ChainOutcome;
use crate::savepanel::{
    format_media, places, where_menu, PanelFocus, PanelHit, PanelLayout, Place, Said, SavePanel,
};
use crate::shell::DesktopShell;

/// Fixed prefix of an open picker's title — on the taskbar and in the window
/// chrome, so the user always sees which UI is asking on an app's behalf. The
/// directory being browsed follows it.
pub const PICKER_TITLE: &str = "Choose a file";

/// Fixed prefix of a save picker's title.
pub const SAVE_TITLE: &str = "Save as";

/// Fixed prefix of a folder picker's title.
pub const FOLDER_TITLE: &str = "Choose a folder";

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

/// Top-left of the picker window, in screen pixels. One deterministic
/// spot (clear of the first cascade slots), exported so a host-side
/// observer (the AW5 QEMU vertical's click script) drives the picker
/// where the session actually places it — never a re-derived guess.
pub const PICKER_ORIGIN: Point = Point::new(120, 90);

/// One-shot: a frame carrying the picker, with its listing landed, reached
/// the display.
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
/// rather than on a delay.
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

/// A folder band's committing answer, and what it says until it is pressed.
const OPEN_LABEL: &str = "Open";
const FOLDER_PROMPT: &str = "Opens the files in the folder shown";

/// How the file a pick chose is opened.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PickAccess {
    /// An existing file, opened as its requester is handed any document:
    /// read-write where its signed manifest edits documents and the user may
    /// write the file, read-only otherwise.
    Read,
    /// A new file, to write.
    Create,
    /// The existing file the user agreed to replace, to write.
    Replace,
    /// A folder, whose files the requester opens are each delegated to it
    /// read-only.
    Folder,
}

impl PickAccess {
    /// The flags a file chosen to save into is opened with, write-only; `None`
    /// for an existing file chosen to open.
    #[must_use]
    pub const fn save_flags(self) -> Option<OpenFlags> {
        match self {
            Self::Read | Self::Folder => None,
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

/// What the showing pick asks of the embedder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickStep {
    /// Open the file at `path` with `access`, and answer through
    /// [`SessionPicker::opened`] with `serial`. The picker waits, still
    /// showing.
    Open {
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
        /// The window-channel id of the requesting app's window.
        for_window: u64,
    },
    /// Make the folder at `path`, and answer through
    /// [`SessionPicker::folder_made`] with `serial`. The picker waits, still
    /// showing.
    NewFolder {
        /// Names this attempt.
        serial: u64,
        /// The new folder's absolute path.
        path: String,
    },
    /// Open `model` as a menu the picker owns, anchored on `anchor` in screen
    /// pixels, and answer through [`SessionPicker::menu_answered`].
    Menu {
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
        /// The window-channel id of the requesting app's window.
        for_window: u64,
        /// The chosen file's own name, for the app to title it with.
        name: String,
    },
    /// The file could not be opened, and nothing was chosen.
    Refused {
        /// The window-channel id of the requesting app's window.
        for_window: u64,
    },
}

/// The narrow face the window-channel bridge drives — object-safe so
/// [`ShellWindowHost`](crate::ShellWindowHost) stays non-generic.
pub trait PickerSlot {
    /// A validated `PickFile` for `requested` — the asking window's channel
    /// id and, where the session has it, its compositor window — was accepted
    /// by the window engine; open the picker UI for `purpose`, answering the
    /// window it shows, which holds the asking one until the pick concludes.
    ///
    /// # Errors
    ///
    /// * [`Errno::AlreadyExists`] — the single picker slot is taken by
    ///   another window's pick (the session shows one picker at a time).
    /// * Any [`Errno`] the initial root listing surfaces (the session's
    ///   filesystem reach refused) or the UI cannot come up; nothing is
    ///   recorded and the refusal is relayed to the requesting app.
    fn begin(
        &mut self,
        requested: (u64, Option<WindowId>),
        purpose: &PickPurpose,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<Option<WindowId>, Errno>;

    /// The window-channel window `window_id` is gone (closed by its owner
    /// or torn down after the owner exited): if its pick is showing, take
    /// the picker down. No conclusion is delivered — the engine already
    /// dropped the window's pending pick with its record.
    fn abort_for(&mut self, window_id: u64, shell: &mut DesktopShell, compositor: &mut Compositor);

    /// The application of window `window_id` answered the save dropped on it,
    /// numbered `serial`, with `folder`: the absolute path its location told,
    /// or `None` where it declined. The open it leads to is owed to the
    /// embedder ([`SessionPicker::take_owed_step`]).
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when the showing save awaits no such drop.
    fn save_folder_answered(
        &mut self,
        dropped: (u64, u32),
        folder: Option<String>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Errno>;
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

/// A folder pick's own controls, under the listing: what choosing does, and
/// the two answers.
struct FolderBand {
    cancel: Button,
    open: Button,
    /// Progress or a refusal, shown in place of the prompt.
    message: Option<String>,
}

impl FolderBand {
    fn new() -> Self {
        Self {
            cancel: Button::labelled(CANCEL_LABEL),
            open: Button::new(
                ButtonContent::Label(String::from(OPEN_LABEL)),
                ControlRole::Primary,
            ),
            message: None,
        }
    }
}

/// One live pick: who it is for, the picker's compositor window, and the
/// browser state behind it.
struct ActivePick<S: DirectorySource> {
    requester: Requester,
    wm: WindowId,
    /// The window a save's dialog opened over, which gets the keyboard back
    /// when the dialog closes; `None` for a picker opened on its own.
    over: Option<WindowId>,
    browser: Browser<S>,
    /// The save band, for a pick choosing where to save.
    save: Option<SaveBand>,
    /// The folder band, for a pick choosing a folder.
    folder: Option<FolderBand>,
    /// The open asked for and not yet answered; input waits while it is.
    waiting: Option<Waiting>,
    /// Whether [`PICKER_SHOWN`] has been announced for this pick.
    shown: bool,
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
}

/// A menu the save panel opened.
enum PanelMenu {
    /// *Where*: the places its rows stand for, in row order.
    Where(Vec<Place>),
    /// *Format*: the endings, in row order.
    Format,
}

impl<S: DirectorySource> ActivePick<S> {
    fn title_prefix(&self) -> &'static str {
        title_prefix(self.save.is_some(), self.folder.is_some())
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
const fn title_prefix(saving: bool, folder: bool) -> &'static str {
    if saving {
        SAVE_TITLE
    } else if folder {
        FOLDER_TITLE
    } else {
        PICKER_TITLE
    }
}

/// The session's picker engine over an injected directory-source factory
/// (`F` builds the session-authority source each pick starts from — the
/// live VFS listing calls in production, an in-memory tree in tests).
pub struct SessionPicker<S: DirectorySource, F: FnMut() -> S> {
    source: F,
    /// Root-first components of the directory each pick opens at — the
    /// user's `Home` folder in production, so the picker starts among the
    /// user's own files rather than at the storage-forest root. Empty means
    /// the root `/`.
    start: Vec<String>,
    active: Option<ActivePick<S>>,
    /// The serial the last open asked for.
    serial: u64,
    /// The session's own pick concluded and not yet taken.
    own: Option<OwnPick>,
    /// The folder the last save went to, which *Where* offers.
    last_saved: Option<Vec<String>>,
    /// The open a window's answer led to, for the embedder to carry out.
    owed: Option<PickStep>,
}

impl<S: DirectorySource, F: FnMut() -> S> SessionPicker<S, F> {
    /// An idle picker over `source`, opening each pick at the root `/`.
    pub const fn new(source: F) -> Self {
        Self {
            source,
            start: Vec::new(),
            active: None,
            serial: 0,
            own: None,
            last_saved: None,
            owed: None,
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
        self.start = start;
        self
    }

    /// Where the showing save's panel lays its parts out, for a test that
    /// presses one.
    #[cfg(test)]
    pub(crate) fn panel_for_test(&self, scale: Scale, theme: &Theme) -> Option<PanelLayout> {
        let band = self.active.as_ref()?.save.as_ref()?;
        Some(panel_layout(band, scale, theme))
    }

    /// The compositor window of the showing picker, if one is active.
    /// The embedder routes this window's key, click, pointer, and wheel input
    /// into [`handle_key`](Self::handle_key),
    /// [`handle_click`](Self::handle_click),
    /// [`handle_pointer`](Self::handle_pointer), and [`scroll`](Self::scroll)
    /// instead of the served-window channel.
    #[must_use]
    pub fn wm_id(&self) -> Option<WindowId> {
        self.active.as_ref().map(|active| active.wm)
    }

    /// Whether the picker has the keyboard — `focused` is its window or its
    /// replace question — and so holds the seat as the lock does.
    #[must_use]
    pub fn holds_keyboard(&self, focused: Option<WindowId>) -> bool {
        focused.is_some() && (focused == self.wm_id() || focused == self.question_wm())
    }

    /// The question whether to replace a file, while it is up: an alert
    /// holding the picker, which takes no input of its own until it is
    /// answered.
    #[must_use]
    pub fn question_wm(&self) -> Option<WindowId> {
        let replacing = self.active.as_ref()?.save.as_ref()?.replacing.as_ref()?;
        Some(replacing.question.wm())
    }

    /// Paint what input changed in the picker, and in the replace question,
    /// since the last paint: once, after the input that changed it drained.
    pub fn paint_owed(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if let Some(active) = self.active.as_mut() {
            if !active.owed.is_empty() && paint(active, shell, compositor) {
                active.owed.clear();
            }
        }
        if let Some(replacing) = self.replacing_mut() {
            replacing.question.paint_owed(shell, compositor);
        }
    }

    /// Start the question's arm delay at `now_ns`, the frame it is first on
    /// screen.
    pub fn stamp(&mut self, now_ns: u64) {
        if let Some(replacing) = self.replacing_mut() {
            replacing.question.stamp(now_ns);
        }
    }

    /// A primary-button click at `local` in the question, at `now_ns`.
    pub fn question_click(
        &mut self,
        local: Point,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let replacing = self.replacing_mut()?;
        let choice = replacing.question.click(local, now_ns, shell, compositor)?;
        self.answer_replace(choice, shell, compositor)
    }

    /// One key press on the question, at `now_ns`: `Escape` declines.
    pub fn question_key(
        &mut self,
        record: &KeyInput,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let replacing = self.replacing_mut()?;
        let choice = replacing.question.key(record, now_ns, shell, compositor)?;
        self.answer_replace(choice, shell, compositor)
    }

    fn replacing_mut(&mut self) -> Option<&mut Replacing> {
        self.active.as_mut()?.save.as_mut()?.replacing.as_mut()
    }

    /// The question was answered `choice`: the file is replaced, or the user
    /// goes back to the name.
    fn answer_replace(
        &mut self,
        choice: Choice,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let Replacing {
            name,
            path,
            into,
            typing,
            question,
        } = self.active.as_mut()?.save.as_mut()?.replacing.take()?;
        // The keyboard goes back to the picker with it.
        question.close(shell, compositor);
        match choice {
            Choice::Accepted => self.request_at(
                (name, path, into),
                (PickAccess::Replace, typing),
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

    /// Apply one key press to the showing picker.
    ///
    /// An open: `Down`/`Up` move the selection, `Enter` descends into a
    /// selected directory or chooses a selected regular file, and
    /// `Backspace` climbs to the parent. A save: every key edits the name,
    /// `Enter` saves. `Escape` cancels either. A refused navigation changes
    /// nothing, and while an answer is awaited only `Escape` is heard.
    pub fn handle_key(
        &mut self,
        record: &KeyInput,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let KeyInput::Pressed { key, .. } = record else {
            return None;
        };
        if self.held_by_question(shell, compositor) {
            return None;
        }
        let (saving, folder, waiting) = self.active.as_ref().map(|active| {
            (
                active.save.is_some(),
                active.folder.is_some(),
                active.answering(),
            )
        })?;
        if waiting {
            return matches!(key, KeyValue::Named(NamedKeyCode::Escape))
                .then(|| self.cancel_waiting(shell, compositor))
                .flatten();
        }
        if saving {
            return self.save_key(record, shell, compositor);
        }
        match key {
            KeyValue::Named(NamedKeyCode::Down) => self.navigate(shell, compositor, |browser| {
                browser.select_next();
                NavOutcome::Redraw
            }),
            KeyValue::Named(NamedKeyCode::Up) => self.navigate(shell, compositor, |browser| {
                browser.select_previous();
                NavOutcome::Redraw
            }),
            KeyValue::Named(NamedKeyCode::Enter) if folder => {
                let into = self.active.as_ref().and_then(|active| {
                    let index = active.browser.chosen_index()?;
                    active
                        .browser
                        .entries()
                        .get(index)
                        .filter(|entry| entry.is_directory())
                        .map(|_| index)
                });
                match into {
                    Some(index) => {
                        self.navigate(shell, compositor, move |browser| descend(browser, index))
                    }
                    None => self.choose_folder(shell, compositor),
                }
            }
            KeyValue::Named(NamedKeyCode::Enter) => self.navigate(shell, compositor, |browser| {
                browser
                    .chosen_index()
                    .map_or(NavOutcome::None, |index| open_or_choose(browser, index))
            }),
            KeyValue::Named(NamedKeyCode::Backspace) => {
                self.navigate(shell, compositor, |browser| {
                    if browser.go_up().unwrap_or(false) {
                        NavOutcome::Redraw
                    } else {
                        NavOutcome::None
                    }
                })
            }
            KeyValue::Named(NamedKeyCode::Escape) => self.cancel(shell, compositor),
            _ => None,
        }
    }

    /// Apply one primary-button press at the picker-window-local position
    /// `local`.
    ///
    /// A press on the listing's scroll bar is the bar's
    /// ([`handle_pointer`](Self::handle_pointer)). A click on a toolbar command
    /// runs it (the read-only navigation the
    /// picker shares with the file manager — Back/Forward/Up/Refresh, the view
    /// toggle, and sort — through the one shared
    /// `tairix_browse::apply_command`); a click on an entry row resolves
    /// through the shared hit-test
    /// (`tairix_browse::render::entry_index_at` — exactly the rows the
    /// renderer drew): a directory row descends, and a regular-file row
    /// chooses that file — or, in a save, offers its name. A press on the
    /// save band reaches its field or answers with a button. A click on a
    /// disabled tool, past the listing, or on an unresolvable coordinate
    /// changes nothing, and while an answer is awaited only *Cancel* is.
    pub fn handle_click(
        &mut self,
        local: Point,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        if self.held_by_question(shell, compositor) {
            return None;
        }
        if self.active.as_ref()?.answering() {
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
        let viewport = self.active.as_ref()?.viewport(scale);
        if !self.active.as_ref()?.lists() || !viewport.contains(local) {
            return self.band_click(local, shell, compositor);
        }
        // Hit-test at the same scale and theme the picker renders with, so a
        // click resolves to exactly the item the user saw (list row or grid
        // tile), and a click on the scrollbar gutter resolves to nothing.
        // A toolbar command takes priority over the item area it sits above;
        // an enabled command runs, a disabled one resolves to nothing.
        if let Some(command) = self.active.as_ref().and_then(|active| {
            toolbar_command_at(
                &active.browser,
                scale,
                theme,
                viewport,
                PICKER_TOOLBAR,
                local,
            )
        }) {
            return self.navigate(shell, compositor, move |browser| {
                match apply_command(browser, command) {
                    Ok(true) => NavOutcome::Redraw,
                    Ok(false) | Err(_) => NavOutcome::None,
                }
            });
        }
        if let Some(order) = self.active.as_ref().and_then(|active| {
            header_sort_at(
                &active.browser,
                scale,
                theme,
                viewport,
                PICKER_TOOLBAR,
                local,
            )
        }) {
            return self.navigate(shell, compositor, move |browser| {
                browser.set_sort_mode(order);
                NavOutcome::Redraw
            });
        }
        let index = self.active.as_ref().and_then(|active| {
            entry_index_at(
                &active.browser,
                scale,
                theme,
                viewport,
                PICKER_TOOLBAR,
                local,
            )
        })?;
        if self.active.as_ref()?.save.is_some() {
            return self.offer_entry(index, shell, compositor);
        }
        if self.active.as_ref()?.folder.is_some() {
            return self.navigate(shell, compositor, move |browser| descend(browser, index));
        }
        self.navigate(shell, compositor, move |browser| {
            open_or_choose(browser, index)
        })
    }

    /// Route a pointer `event` at the picker-window-local position `local`:
    /// to the save panel's buttons, which light under the pointer, and to the
    /// listing's scroll bar, answering whether the picker took it — so a press
    /// on the bar is never also a press on a row, and a press held on the save
    /// panel's icon is the drag's until it ends.
    ///
    /// The bar keeps what a press on it started: an end button or the track
    /// steps, the thumb drags, and the moves and the release that follow are
    /// the bar's until the release ends them. What either repainted is owed,
    /// and painted once with the turn.
    pub fn handle_pointer(
        &mut self,
        local: Point,
        event: &InputEvent,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        if self.question_wm().is_some() {
            return true;
        }
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if active.icon_press.is_some() {
            if matches!(
                event,
                InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled
            ) {
                active.icon_press = None;
            }
            return true;
        }
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let lit = active.save.as_mut().map(|band| {
            let layout = panel_layout(band, scale, theme);
            let mut lit = Region::new();
            band.panel.on_pointer(event, &layout, &mut lit);
            lit
        });
        if let Some(lit) = lit.filter(|lit| !lit.is_empty()) {
            repaint(active, &lit);
        }
        if !active.lists() {
            return false;
        }
        let mut drew = damage::sink();
        let viewport = active.viewport(scale);
        let Some(repainted) = scroll_pointer(
            &mut active.browser,
            scale,
            shell.session().active_theme(),
            viewport,
            PICKER_TOOLBAR,
            local,
            event,
            &mut drew,
        ) else {
            return false;
        };
        if repainted {
            repaint(active, &drew);
        }
        true
    }

    /// Carry the save's document out of the panel's icon, once the press held
    /// on it has travelled past the drag slop to `local` at `scale`: the
    /// press is the drag's from here, so the panel lets go of it. Answers what
    /// the drag carries and the window the save is for.
    pub fn icon_drag(&mut self, local: Point, scale: Scale) -> Option<(SavedDocument, u64)> {
        let active = self.active.as_mut()?;
        let (from, grab) = active.icon_press?;
        let Requester::Window(for_window) = active.requester else {
            return None;
        };
        if active.answering() || !past_drag_slop(from, local, scale) {
            return None;
        }
        active.icon_press = None;
        let panel = &active.save.as_ref()?.panel;
        let document = SavedDocument {
            name: panel.name.text().to_string(),
            icon: panel.icon(),
            grab,
        };
        Some((document, for_window))
    }

    /// What became of the save's document carried out of the panel: dropped
    /// in a folder the desktop resolved, it is saved there; on a window, the
    /// panel waits for that window's application to name the folder; and
    /// anywhere else nothing changes.
    ///
    /// The name must hold as *Save* holds it, a refusal stated in the panel.
    pub fn save_dropped(
        &mut self,
        dropped: SaveDrop,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        if matches!(dropped, SaveDrop::Nothing) || self.active.as_ref()?.answering() {
            return None;
        }
        if matches!(dropped, SaveDrop::Unanswered) {
            self.refuse_save(String::from(UNANSWERED), shell, compositor);
            return None;
        }
        let held = self.dropped_name(shell, compositor)?;
        match dropped {
            SaveDrop::Nothing | SaveDrop::Unanswered => None,
            SaveDrop::Folder(folder) => self.save_into(held, &folder, shell, compositor),
            SaveDrop::Offer { window_id, serial } => {
                let active = self.active.as_mut()?;
                let band = active.save.as_mut()?;
                band.edit(Said::Progress(String::from(SAVING)), shell, compositor);
                active.awaiting = Some(AwaitedFolder {
                    window_id,
                    serial,
                    held,
                });
                redraw_band(active, shell, compositor);
                None
            }
        }
    }

    /// Whether the pick waits on the window `window_id` to name the folder its
    /// save was dropped on with the offer numbered `serial`.
    pub fn awaits_drop(&self, window_id: u64, serial: u32) -> bool {
        self.active
            .as_ref()
            .and_then(|active| active.awaiting.as_ref())
            .is_some_and(|awaited| awaited.window_id == window_id && awaited.serial == serial)
    }

    /// Stop waiting on the window a save was dropped on, which could not be
    /// told where: nothing is saved, and the panel says so.
    pub fn drop_unreached(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if let Some(active) = self.active.as_mut() {
            if active.awaiting.take().is_some() {
                self.refuse_save(String::from(NOT_GIVEN), shell, compositor);
            }
        }
    }

    /// The open a window's answer to a dropped save led to, for the embedder
    /// to carry out as it does any other [`PickStep`].
    pub fn take_owed_step(&mut self) -> Option<PickStep> {
        self.owed.take()
    }

    /// The name the panel's field holds, as a dropped save goes under it, or
    /// `None` with the refusal stated in the panel.
    fn dropped_name(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Held> {
        let active = self.active.as_mut()?;
        let band = active.save.as_mut()?;
        let typed = band.panel.name.text().to_string();
        let held = tairix_path::validate_file_name(&typed)
            .map_err(|err| err.to_string())
            .and_then(|()| band.held(typed));
        match held {
            Ok(held) => Some(held),
            Err(refusal) => {
                band.edit(Said::Refusal(refusal), shell, compositor);
                redraw_band(active, shell, compositor);
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
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if let Some(band) = active.save.as_mut() {
            band.edit(Said::Refusal(refusal), shell, compositor);
            redraw_band(active, shell, compositor);
        }
    }

    /// Whether a press at `local` lands on the showing pick's *Cancel*.
    fn cancel_pressed(&self, local: Point, theme: &Theme, scale: Scale) -> bool {
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        if let Some(band) = active.save.as_ref() {
            return panel_layout(band, scale, theme).hit(local) == Some(PanelHit::Cancel);
        }
        active.folder.is_some() && band_layout(scale, theme).cancel.contains(local)
    }

    /// When the press the showing picker's listing bar holds steps next, or
    /// `None` when nothing held would move.
    pub fn scroll_repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.active.as_mut()?.browser.scroll_repeat_deadline(now_ns)
    }

    /// Step the press the showing picker's listing bar holds if its repeat is
    /// due at `now_ns`, repainting what it moved into the picker's own
    /// buffer.
    pub fn scroll_repeat(
        &mut self,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if self.question_wm().is_some() {
            return;
        }
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let scale = compositor.scale();
        let mut drew = damage::sink();
        let viewport = active.viewport(scale);
        if !active.lists() {
            return;
        }
        if tairix_browse::render::scroll_repeat(
            &mut active.browser,
            (scale, shell.session().active_theme()),
            viewport,
            PICKER_TOOLBAR,
            now_ns,
            &mut drew,
        ) {
            repaint(active, &drew);
        }
    }

    /// Scroll the showing picker's listing by a wheel turn of `(dx, dy)`, in
    /// the seat's scroll units, through the listing's own bar, answering
    /// whether it moved.
    ///
    /// The bar carries what is short of a pixel into the next turn. Only what
    /// the turn moved — the items and the bar — is repainted, into the
    /// picker's own buffer.
    pub fn scroll(
        &mut self,
        (dx, dy): (i32, i32),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        if self.question_wm().is_some() {
            return false;
        }
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let mut moved = damage::sink();
        // A wheel over a folded-away listing scrolls nothing anyone can see.
        if !active.lists() {
            return false;
        }
        let viewport = active.viewport(scale);
        if !scroll_wheel(
            &mut active.browser,
            scale,
            theme,
            viewport,
            PICKER_TOOLBAR,
            (dx, dy),
            &mut moved,
        ) {
            return false;
        }
        repaint(active, &moved);
        true
    }

    /// Repaint the slots whose pictures the `landed` decodes moved — the
    /// listing's rows or tiles, and a save's own icon — which were drawn with
    /// their glyphs until the decodes came back.
    pub fn artwork_landed(
        &mut self,
        landed: &Landed,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if landed.is_empty() {
            return;
        }
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let mut moved = Region::new();
        if active.lists() {
            manager_art(
                &active.browser,
                (scale, theme),
                active.viewport(scale),
                &PICKER_CHROME,
                &mut |art| {
                    if landed.resolves(art.request, art.side) {
                        moved.add(art.at);
                    }
                },
            );
        }
        if let Some(band) = active.save.as_ref() {
            let icon = panel_layout(band, scale, theme).icon;
            if landed.resolves(IconRequest::kind(band.panel.icon()), icon.width) {
                moved.add(icon);
            }
        }
        if !moved.is_empty() {
            repaint(active, &moved);
        }
    }

    /// Redraw the showing pick, and its question, in the look and at the
    /// scale now in use: a session window keeps the pixels it was painted in.
    pub fn restyle(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        redraw(active, shell, compositor);
        if let Some(replacing) = active
            .save
            .as_mut()
            .and_then(|band| band.replacing.as_mut())
        {
            replacing.question.repaint(shell, compositor);
        }
    }

    /// Ask the source again for a navigation whose listing had not arrived,
    /// repainting and retitling when it lands.
    ///
    /// This is what the session calls on the wake that says its listing worker
    /// finished — never a poll. With no pick showing, or nothing pending, it
    /// does nothing. A listing the source now refuses drops the pending
    /// navigation and repaints, so the "listing" cue clears and the picker is
    /// left exactly where it was (fail closed) — unless the folder went from
    /// its path, when the pick moves to its parent.
    pub fn resume(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        self.reread(shell, compositor, Browser::resume);
    }

    /// Run `read` — a listing collected, or one asked afresh — on the showing
    /// pick's browser, repainting once it answers (`Ok(false)`: not yet). A
    /// pick whose folder went moves to its parent when the folder's read fails.
    fn reread(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        read: impl FnOnce(&mut Browser<S>) -> Result<bool, BrowseError>,
    ) {
        let Some((lost, owed)) = self
            .active
            .as_ref()
            .map(|active| (active.lost, active.reread_owed))
        else {
            return;
        };
        let mut in_flight = false;
        let _ = self.navigate(shell, compositor, |browser| {
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
        if let Some(active) = self.active.as_mut().filter(|_| !in_flight) {
            active.lost = false;
            active.reread_owed = false;
        }
    }

    /// Follow what the showing pick's folder watch reported: the changed
    /// entries are merged in place and only the rows they altered repainted;
    /// a rescan, or a folder gone from its path, reads the folder afresh —
    /// once a navigation already reading one is refused, if one is — and a
    /// folder that cannot be read again after going leaves the pick at the
    /// nearest folder above that is still there.
    pub fn follow(
        &mut self,
        update: WatchUpdate,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let gone = matches!(update, WatchUpdate::Gone);
        match update {
            WatchUpdate::Quiet => {}
            WatchUpdate::Changes(changes) => {
                let scale = compositor.scale();
                let theme = shell.session().active_theme();
                let viewport = active.viewport(scale);
                let before = shown_listing(&active.browser, scale, theme, viewport, PICKER_TOOLBAR);
                match active.browser.apply_changes(changes) {
                    Ok(Some(_)) => {}
                    Ok(None) => return,
                    // Without the memory to merge them, the folder is read again.
                    Err(_) => return self.follow(WatchUpdate::Rescan, shell, compositor),
                }
                let mut moved = Region::new();
                if active.lists()
                    && listing_damage(
                        &before,
                        &active.browser,
                        scale,
                        theme,
                        viewport,
                        PICKER_TOOLBAR,
                        &mut moved,
                    )
                {
                    repaint(active, &moved);
                }
            }
            WatchUpdate::Rescan | WatchUpdate::Gone => {
                active.lost |= gone;
                active.reread_owed = true;
                if active.browser.is_listing() {
                    return;
                }
                self.reread(shell, compositor, |browser| {
                    browser.refresh().map(|()| !browser.is_listing())
                });
            }
        }
    }

    /// Dismiss the showing pick without choosing, closing the picker window.
    ///
    /// What both dismissals mean, so they cannot diverge: the Escape key the
    /// engine routes itself, and the title bar's close control, which the
    /// window manager raises for the session to interpret because a window the
    /// session paints is the session's to close.
    ///
    /// Returns the concluded pick, or `None` when no pick is showing.
    pub fn cancel(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let for_window = self.close_window_pick(None, shell, compositor)?;
        Some(PickStep::Cancelled { for_window })
    }

    /// What `Escape` or *Cancel* do while the pick waits: a folder being made
    /// is let go, its answer heeded no more, and the field names the document
    /// again; any other wait is the pick cancelled.
    fn cancel_waiting(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        if self
            .active
            .as_mut()
            .is_some_and(|active| active.making.take().is_some())
        {
            self.leave_naming(shell, compositor);
            return None;
        }
        self.cancel(shell, compositor)
    }

    /// The embedder's answer to the open `serial` names: whether the file
    /// could be opened, and the path a save made it at, its name finished as
    /// its typing asked — `None` for the path asked for.
    ///
    /// An opened file ends the pick. A refused open ends an open pick too, but
    /// a save stays up to say why — or, for a name that was taken after the
    /// listing was read, to ask whether to replace it. An answer for an
    /// attempt that has gone (the user cancelled, the requesting window
    /// closed) is `None`: whatever it opened is the embedder's to close.
    pub fn opened(
        &mut self,
        serial: u64,
        (result, made): (Result<(), Errno>, Option<String>),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickEnd> {
        let active = self.active.as_mut()?;
        let waiting = active.waiting.take_if(|waiting| waiting.serial == serial)?;
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
        if let (Some(band), Err(err)) = (active.folder.as_mut(), result) {
            band.message = Some(format!("Not opened: {err}"));
            redraw_band(active, shell, compositor);
            return None;
        }
        let Some(band) = active.save.as_mut() else {
            let for_window = self.close_window_pick(None, shell, compositor)?;
            return Some(match result {
                Ok(()) => PickEnd::Chosen { for_window, name },
                Err(_) => PickEnd::Refused { for_window },
            });
        };
        match result {
            Ok(()) => {
                // Where the file was made, whatever the listing shows by now.
                self.last_saved = tairix_browse::vfs::components_from_absolute_path(&path)
                    .ok()
                    .and_then(|mut folder| folder.pop().map(|_| folder));
                let for_window = self.close_window_pick(None, shell, compositor)?;
                Some(PickEnd::Chosen { for_window, name })
            }
            Err(Errno::AlreadyExists) if access == PickAccess::Create => {
                // Asked only about a file the user can see the question is
                // about: one in the folder dropped on, or still showing.
                let shown = active
                    .browser
                    .folder()
                    .and_then(|folder| shown_path(folder, &name));
                if into.is_some() || shown.as_deref() == Some(path.as_str()) {
                    band.ask_to_replace(((name, path, into), typing), active.wm, shell, compositor);
                } else {
                    band.edit(Said::Refusal(already_exists(&name)), shell, compositor);
                }
                redraw_band(active, shell, compositor);
                None
            }
            // The name is taken by something no save replaces.
            Err(Errno::IsADirectory) => {
                band.edit(
                    Said::Refusal(taken_by(&name, "a folder")),
                    shell,
                    compositor,
                );
                redraw_band(active, shell, compositor);
                None
            }
            Err(Errno::LinkLoop) => {
                band.edit(
                    Said::Refusal(taken_by(&name, "a shortcut")),
                    shell,
                    compositor,
                );
                redraw_band(active, shell, compositor);
                None
            }
            Err(err) => {
                band.edit(
                    Said::Refusal(format!("Not saved: {err}")),
                    shell,
                    compositor,
                );
                redraw_band(active, shell, compositor);
                None
            }
        }
    }

    /// Announce [`PICKER_SHOWN`] for a pick whose picker a presented frame has
    /// now carried with its listing landed.
    ///
    /// Called after a successful present, like its window and menu siblings.
    /// One-shot per pick: a repaint, a navigation, or any later frame
    /// announces nothing more. A pick still waiting on its listing announces
    /// nothing *yet* — the rows a user picks from are not on screen until it
    /// lands — so the announcement can never run ahead of the pixels.
    pub fn report_newly_shown(&mut self, report: impl FnOnce()) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.shown || active.browser.is_listing() {
            return;
        }
        active.shown = true;
        report();
    }

    /// A key a save routes to its name field.
    fn save_key(
        &mut self,
        record: &KeyInput,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let InputEvent::KeyPressed { key, modifiers } = crate::keyboard::to_input_event(*record)
        else {
            return None;
        };
        let band = self.active.as_mut()?.save.as_mut()?;
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
                let active = self.active.as_mut()?;
                redraw_band(active, shell, compositor);
                if ringed {
                    let view = listing_viewport(active.save.as_ref(), compositor.scale());
                    active.owed.add(view);
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
            return self.listing_key(key, shell, compositor);
        }
        if focus != PanelFocus::Name {
            return self.control_key(focus, (key, modifiers), shell, compositor);
        }
        let active = self.active.as_mut()?;
        let mut drew = damage::sink();
        let band = active.save.as_mut()?;
        let field = panel_layout(band, compositor.scale(), shell.session().active_theme()).name;
        let action = band.panel.name.on_key(key, modifiers, field, &mut drew);
        match action {
            Some(TextAction::Submitted) => return self.submit(shell, compositor),
            Some(TextAction::Cancelled) => return self.cancel(shell, compositor),
            // A refusal of the name before is not one of this name, and the
            // ending typed may be another format.
            Some(TextAction::Edited) => {
                band.edit(Said::Nothing, shell, compositor);
                band.panel.restate_format();
                redraw_band(active, shell, compositor);
            }
            None if !drew.is_empty() => repaint(active, &drew),
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
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let activates = matches!(key, Key::Named(NamedKey::Enter) | Key::Char(' '))
            && !modifiers.ctrl
            && !modifiers.alt;
        let opens = activates || key == Key::Named(NamedKey::Down);
        if key == Key::Named(NamedKey::Escape) {
            return self.cancel(shell, compositor);
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
        self.panel_answer(hit, shell, compositor)
    }

    /// A key on the save's folder listing: `Up` and `Down` move the
    /// selection, `Enter` goes into a folder or offers a file's name, and
    /// `Backspace` climbs to the folder above.
    fn listing_key(
        &mut self,
        key: Key,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        match key {
            Key::Named(NamedKey::Down) => self.navigate(shell, compositor, |browser| {
                browser.select_next();
                NavOutcome::Redraw
            }),
            Key::Named(NamedKey::Up) => self.navigate(shell, compositor, |browser| {
                browser.select_previous();
                NavOutcome::Redraw
            }),
            Key::Named(NamedKey::Enter) => {
                let index = self.active.as_ref()?.browser.chosen_index()?;
                self.offer_entry(index, shell, compositor)
            }
            Key::Named(NamedKey::Backspace) => self.navigate(shell, compositor, |browser| {
                if browser.go_up().unwrap_or(false) {
                    NavOutcome::Redraw
                } else {
                    NavOutcome::None
                }
            }),
            Key::Named(NamedKey::Escape) => self.cancel(shell, compositor),
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
        let active = self.active.as_mut()?;
        let suggested = tairix_browse::NewEntry::Folder.suggest_name(active.browser.entries());
        let band = active.save.as_mut()?;
        band.edit(Said::Nothing, shell, compositor);
        band.panel.begin_naming(&suggested);
        redraw_band(active, shell, compositor);
        None
    }

    /// Stop naming a new folder: the field holds the document's name again.
    fn leave_naming(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if let Some(band) = active.save.as_mut() {
            band.edit(Said::Nothing, shell, compositor);
            band.panel.end_naming();
            redraw_band(active, shell, compositor);
        }
    }

    /// Ask the embedder to make the folder the field names in the folder
    /// shown, a name the listing already shows refused here.
    fn make_folder(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let active = self.active.as_mut()?;
        let shown = active.browser.folder()?.to_vec();
        let band = active.save.as_mut()?;
        let name = band.panel.name.text().to_string();
        let checked = tairix_browse::validate_new_entry_name(&name, active.browser.entries())
            .map_err(|err| String::from(err.message()))
            .and_then(|()| {
                shown_path(&shown, &name).ok_or_else(|| String::from("That name is too long."))
            });
        let path = match checked {
            Ok(path) => path,
            Err(refusal) => {
                band.edit(Said::Refusal(refusal), shell, compositor);
                redraw_band(active, shell, compositor);
                return None;
            }
        };
        let mut into = shown;
        into.push(name);
        self.serial += 1;
        let serial = self.serial;
        band.edit(Said::Progress(String::from(MAKING)), shell, compositor);
        active.making = Some((serial, into));
        redraw_band(active, shell, compositor);
        Some(PickStep::NewFolder { serial, path })
    }

    /// The embedder's answer to the folder `serial` asked to be made: made, the
    /// save goes on into it with the document's name; refused, the field
    /// still names the folder and says why. An answer for an attempt that has
    /// gone is dropped.
    pub fn folder_made(
        &mut self,
        serial: u64,
        made: Result<(), Errno>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some((_, into)) = active.making.take_if(|(asked, _)| *asked == serial) else {
            return;
        };
        let Some(band) = active.save.as_mut() else {
            return;
        };
        if let Err(err) = made {
            band.edit(Said::Refusal(format!("Not made: {err}")), shell, compositor);
            redraw_band(active, shell, compositor);
            return;
        }
        band.edit(Said::Nothing, shell, compositor);
        band.panel.end_naming();
        let _ = self.navigate(shell, compositor, move |browser| {
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
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let active = self.active.as_mut()?;
        if active.folder.is_some() {
            let layout = band_layout(scale, theme);
            if layout.open.contains(local) {
                return self.choose_folder(shell, compositor);
            }
            if layout.cancel.contains(local) {
                return self.cancel(shell, compositor);
            }
            return None;
        }
        let band = active.save.as_mut()?;
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
                    active.icon_press = Some((local, grab));
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
                redraw_band(active, shell, compositor);
                None
            }
            hit => self.panel_answer(hit, shell, compositor),
        }
    }

    /// Answer the save panel's control `hit`, pressed or reached from the
    /// keyboard: while a folder is named, *Save* makes it and *Cancel* goes
    /// back to the document's name.
    fn panel_answer(
        &mut self,
        hit: PanelHit,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let (scale, theme) = (compositor.scale(), shell.session().active_theme());
        let active = self.active.as_mut()?;
        let band = active.save.as_mut()?;
        let naming = band.panel.naming();
        let layout = panel_layout(band, scale, theme);
        match hit {
            PanelHit::Save if naming => self.make_folder(shell, compositor),
            PanelHit::Save => self.submit(shell, compositor),
            PanelHit::Cancel if naming => {
                self.leave_naming(shell, compositor);
                None
            }
            PanelHit::Cancel => self.cancel(shell, compositor),
            PanelHit::Browse => {
                let expanded = !band.panel.expanded();
                if expanded {
                    band.listing_height = save_listing_height(
                        (band, active.browser.view_mode()),
                        active.wm,
                        shell,
                        compositor,
                    );
                }
                band.panel.set_expanded(expanded);
                refit(active, shell, compositor);
                None
            }
            PanelHit::NewFolder => self.new_folder(shell, compositor),
            PanelHit::Where => self.open_where(layout.where_box, compositor),
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
    fn open_where(&mut self, anchor: Rect, compositor: &Compositor) -> Option<PickStep> {
        let shown = self.active.as_ref()?.browser.folder();
        let offered = places(&self.start, shown, self.last_saved.as_deref());
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
        let active = self.active.as_mut()?;
        let client = compositor.window_client_rect(active.wm)?;
        let model = match &menu {
            PanelMenu::Where(offered) => where_menu(offered, active.browser.folder()),
            PanelMenu::Format => active.save.as_ref()?.panel.format_menu(),
        };
        active.menu = Some(menu);
        let for_window = match active.requester {
            Requester::Window(for_window) => Some(for_window),
            Requester::Sound(_) => None,
        };
        Some(PickStep::Menu {
            model,
            anchor: Rect::new(
                client.left().saturating_add(anchor.left()),
                client.top().saturating_add(anchor.top()),
                anchor.width,
                anchor.height,
            ),
            for_window,
        })
    }

    /// What became of a menu the save panel opened: a place chosen is shown,
    /// a format chosen puts the name in it, and anything else changes nothing.
    pub fn menu_answered(
        &mut self,
        outcome: &ChainOutcome,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some(menu) = active.menu.take() else {
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
                let _ = self.navigate(shell, compositor, move |browser| {
                    match browser.navigate_to(place.components) {
                        Ok(_) => NavOutcome::Redraw,
                        Err(_) => NavOutcome::None,
                    }
                });
            }
            PanelMenu::Format => {
                let Some(band) = active.save.as_mut() else {
                    return;
                };
                if band.panel.set_format(index) {
                    band.edit(Said::Nothing, shell, compositor);
                    redraw_band(active, shell, compositor);
                }
            }
        }
    }

    /// A click on the listing's entry `index` in a save: a folder is gone
    /// into, and a file offers its name.
    fn offer_entry(
        &mut self,
        index: usize,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let active = self.active.as_mut()?;
        let entry = active.browser.entries().get(index)?;
        if entry.is_directory() {
            return self.navigate(shell, compositor, move |browser| {
                match browser.open_index(index) {
                    Ok(()) => NavOutcome::Redraw,
                    Err(_) => NavOutcome::None,
                }
            });
        }
        let name = String::from(entry.name());
        let band = active.save.as_mut()?;
        // The field names a folder being made, which a file's name is not.
        if band.panel.naming() {
            return None;
        }
        band.panel.offer(&name);
        band.edit(Said::Nothing, shell, compositor);
        redraw_band(active, shell, compositor);
        None
    }

    /// Save under the name the field holds — or, while asked, replace the file
    /// it names.
    fn submit(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let active = self.active.as_mut()?;
        let band = active.save.as_mut()?;
        if band.panel.naming() {
            return self.make_folder(shell, compositor);
        }
        let typed = band.panel.name.text().to_string();
        // A name naming a folder goes into it, whatever a file must end in.
        let folder = active
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
                redraw_band(active, shell, compositor);
                return None;
            }
        };
        if typing.bare && folder.is_none() {
            // Its name is the folder's to finish, so whether it is taken is
            // known once the file is asked for: an exclusive create asks.
            return self.request(name, (PickAccess::Create, typing), shell, compositor);
        }
        let taken = folder.or_else(|| {
            active
                .browser
                .entries()
                .iter()
                .position(|entry| entry.name() == name)
        });
        let Some(index) = taken else {
            // A name the listing does not show is created exclusively, so one
            // that appeared since is asked about rather than overwritten.
            return self.request(name, (PickAccess::Create, typing), shell, compositor);
        };
        let kind = active.browser.entries()[index].kind();
        match kind {
            EntryKind::File => {
                let path = shown_path(active.browser.folder()?, &name)?;
                band.ask_to_replace(((name, path, None), typing), active.wm, shell, compositor);
                redraw_band(active, shell, compositor);
                None
            }
            // Naming a folder goes into it, and the name offered comes back.
            _ if kind.is_directory() => {
                let suggested = band.panel.suggested.clone();
                band.panel.offer(&suggested);
                band.edit(Said::Nothing, shell, compositor);
                self.navigate(shell, compositor, move |browser| {
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
                redraw_band(active, shell, compositor);
                None
            }
        }
    }

    /// Choose the folder the picker shows, asking the embedder to delegate
    /// its files.
    fn choose_folder(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let components = self.active.as_ref()?.browser.folder()?;
        let path = vfs::absolute_path(components).ok()?;
        let name = components
            .last()
            .map_or_else(|| String::from("/"), String::clone);
        self.request_at(
            (name, path, None),
            (PickAccess::Folder, Typing::default()),
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
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        // A name the shared spelling refuses is not chosen rather than
        // guessed at.
        let path = shown_path(self.active.as_ref()?.browser.folder()?, &name)?;
        self.request_at((name, path, None), how, shell, compositor)
    }

    /// Ask the embedder to open the file `name` at `path`, in the folder
    /// `into` a save was dropped in or, for `None`, the one shown.
    fn request_at(
        &mut self,
        (name, path, into): (String, String, Option<Vec<String>>),
        (access, typing): (PickAccess, Typing),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let active = self.active.as_mut()?;
        let Requester::Window(for_window) = active.requester else {
            // The session keeps the path; nothing is opened for it.
            let _ = self.close_window_pick(Some(path), shell, compositor);
            return None;
        };
        self.serial += 1;
        let serial = self.serial;
        if let Some(band) = active.save.as_mut() {
            band.edit(
                Said::Progress(saving_into(into.as_deref())),
                shell,
                compositor,
            );
            redraw_band(active, shell, compositor);
        }
        if let Some(band) = active.folder.as_mut() {
            band.message = Some(String::from("Opening…"));
            redraw_band(active, shell, compositor);
        }
        active.waiting = Some(Waiting {
            serial,
            name,
            path: path.clone(),
            access,
            into,
            typing,
        });
        Some(PickStep::Open {
            serial,
            for_window,
            path,
            access,
            typing,
        })
    }

    /// Run one navigation step against the active browser, repaint on a
    /// change, retitle the window when the step moved to another directory,
    /// and ask for the file when the step chose one.
    fn navigate(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        step: impl FnOnce(&mut Browser<S>) -> NavOutcome,
    ) -> Option<PickStep> {
        let active = self.active.as_mut()?;
        let scale = compositor.scale();
        let prefix = active.title_prefix();
        let titled = picker_title(prefix, active.browser.folder());
        match step(&mut active.browser) {
            NavOutcome::None => None,
            NavOutcome::Redraw => {
                active.restate_where();
                // Keep the (possibly moved) selection on screen before the
                // repaint, scrolling the shared view the least it can.
                let viewport = active.viewport(scale);
                reveal_selection(
                    &mut active.browser,
                    scale,
                    shell.session().active_theme(),
                    viewport,
                    PICKER_TOOLBAR,
                );
                redraw(active, shell, compositor);
                // The picker is session-owned and has no window channel of
                // its own, so it retitles through the compositor. A step that
                // only moved the selection leaves the title alone rather than
                // re-presenting the taskbar for an unchanged label.
                let located = picker_title(prefix, active.browser.folder());
                if located != titled {
                    shell.retitle_window(compositor, active.wm, &located);
                    // A question about a file in the folder left behind is
                    // not one about anything showing now.
                    if let Some(band) = active.save.as_mut().filter(|band| band.replacing.is_some())
                    {
                        band.edit(Said::Nothing, shell, compositor);
                        redraw_band(active, shell, compositor);
                    }
                    // A refusal stated for the folder left behind is not one
                    // about the folder showing now.
                    if let Some(band) = active.folder.as_mut().filter(|band| band.message.is_some())
                    {
                        band.message = None;
                        redraw_band(active, shell, compositor);
                    }
                }
                None
            }
            NavOutcome::Chosen(name) => self.request(
                name,
                (PickAccess::Read, Typing::default()),
                shell,
                compositor,
            ),
        }
    }

    /// Close the picker window, answering the window the pick was for; a pick
    /// the session made for itself concludes with `chosen` instead, kept for
    /// [`Self::take_own`].
    fn close_window_pick(
        &mut self,
        chosen: Option<String>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<u64> {
        let mut active = self.active.take()?;
        // What was owed the pick is owed nobody now.
        self.owed = None;
        if let Some(band) = active.save.as_mut() {
            band.edit(Said::Nothing, shell, compositor);
        }
        // A dialog is no task: it leaves the screen through its owner, which
        // takes the keyboard back.
        let _ = match active.over {
            Some(owner) => shell.close_transient_window(compositor, active.wm, Some(owner)),
            None => shell.close_window(compositor, active.wm),
        };
        match active.requester {
            Requester::Window(for_window) => Some(for_window),
            requester @ Requester::Sound(_) => {
                self.own = Some(OwnPick {
                    requester,
                    path: chosen,
                });
                None
            }
        }
    }

    /// The pick the session made for itself that has concluded since this was
    /// last asked.
    pub fn take_own(&mut self) -> Option<OwnPick> {
        self.own.take()
    }

    /// Open the picker for `requester`'s pick of `purpose`.
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
    ) -> Result<(), Errno> {
        self.begin_over(requester, None, purpose, shell, compositor)
    }

    /// Open the picker for `requester`'s pick of `purpose`; a save opens as a
    /// dialog of `over`, the window that asked, where there is one.
    fn begin_over(
        &mut self,
        requester: Requester,
        over: Option<WindowId>,
        purpose: &PickPurpose,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Errno> {
        if self.active.is_some() {
            return Err(Errno::AlreadyExists);
        }
        // Listed under the session's authority before any UI state exists, so
        // a refused root refuses the pick with nothing half-open. A save starts
        // where the last one went, climbing to what is still there.
        let mut at = match purpose {
            PickPurpose::Save { .. } => self.last_saved.clone(),
            PickPurpose::Open | PickPurpose::Folder => None,
        }
        .unwrap_or_else(|| self.start.clone());
        let browser = loop {
            match Browser::open_at((self.source)(), at.clone()) {
                Ok(browser) => break browser,
                Err(err) => {
                    if at.pop().is_none() {
                        return Err(err.source_errno().unwrap_or(Errno::PermissionDenied));
                    }
                }
            }
        };
        let mut save = match purpose {
            PickPurpose::Open | PickPurpose::Folder => None,
            PickPurpose::Save { suggested, formats } => {
                Some(SaveBand::new(suggested.as_str(), formats))
            }
        };
        if let Some(band) = save.as_mut() {
            band.panel.set_where(folder_label(browser.folder()));
        }
        let folder = matches!(purpose, PickPurpose::Folder).then(FolderBand::new);
        let scale = compositor.scale();
        let surface = {
            let (theme, mut artwork) = shell.theme_and_artwork();
            let (width, height) = pick_size(save.as_ref(), folder.is_some(), scale, theme);
            let mut surface = Surface::new(width, height).ok_or(Errno::LengthOutOfRange)?;
            paint_pick(
                &mut surface,
                &browser,
                (save.as_ref(), folder.as_ref()),
                (scale, theme),
                &mut artwork,
            );
            surface
        };
        let prefix = title_prefix(save.is_some(), folder.is_some());
        let titled = picker_title(prefix, browser.folder());
        // The title says which UI is asking on the application's behalf. A
        // save reads as that document's question, so it opens as a dialog
        // over the window that asked; an open or a folder pick is a window of
        // its own, whose close control cancels it as Escape does.
        let over = over.filter(|_| save.is_some());
        let wm = if let Some(over) = over {
            shell
                .open_dialog_window(compositor, over, surface, &titled, (true, true, false))
                .ok_or(Errno::NoSpace)?
        } else {
            let wm = shell
                .open_window(compositor, PICKER_ORIGIN, surface, titled.clone())
                .ok_or(Errno::NoSpace)?;
            shell.decorate_window(compositor, wm, &titled, false);
            wm
        };
        let lost = browser.is_listing();
        self.active = Some(ActivePick {
            requester,
            wm,
            over,
            browser,
            save,
            folder,
            waiting: None,
            shown: false,
            lost,
            reread_owed: false,
            menu: None,
            icon_press: None,
            awaiting: None,
            making: None,
            owed: Region::new(),
        });
        Ok(())
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
    tairix_browse::sort_entries(entries, tairix_browse::SortMode::default_order());
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

impl<S: DirectorySource, F: FnMut() -> S> PickerSlot for SessionPicker<S, F> {
    fn begin(
        &mut self,
        (for_window, over): (u64, Option<WindowId>),
        purpose: &PickPurpose,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<Option<WindowId>, Errno> {
        self.begin_over(
            Requester::Window(for_window),
            over,
            purpose,
            shell,
            compositor,
        )?;
        Ok(self.wm_id())
    }

    fn abort_for(&mut self, window_id: u64, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.requester == Requester::Window(window_id) {
            let _ = self.close_window_pick(None, shell, compositor);
            return;
        }
        // A save dropped on a window that has gone will hear of no folder.
        if active
            .awaiting
            .take_if(|awaited| awaited.window_id == window_id)
            .is_some()
        {
            self.refuse_save(String::from(TARGET_GONE), shell, compositor);
        }
    }

    fn save_folder_answered(
        &mut self,
        (window_id, serial): (u64, u32),
        folder: Option<String>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Errno> {
        let awaited = self
            .active
            .as_mut()
            .and_then(|active| {
                active
                    .awaiting
                    .take_if(|awaited| awaited.window_id == window_id && awaited.serial == serial)
            })
            .ok_or(Errno::NotFound)?;
        match folder {
            Some(folder) => {
                self.owed = self.save_into(awaited.held, &folder, shell, compositor);
            }
            None => self.refuse_save(String::from(NOT_GIVEN), shell, compositor),
        }
        Ok(())
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

impl<S: DirectorySource> ActivePick<S> {
    /// [`listing_viewport`] of this pick.
    fn viewport(&self, scale: Scale) -> Rect {
        listing_viewport(self.save.as_ref(), scale)
    }
}

/// Where a folder band's controls sit, in picker-window-local pixels.
struct BandLayout {
    band: Rect,
    field: Rect,
    cancel: Rect,
    open: Rect,
}

/// The folder band under the listing at `scale`: the line it states what
/// choosing does, or a refusal, and the two answers beside it.
fn band_layout(scale: Scale, theme: &Theme) -> BandLayout {
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
    let open_w = FolderBand::new().open.measured_width(scale, theme);
    let cancel_w = Button::labelled(CANCEL_LABEL).measured_width(scale, theme);
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
fn pick_size(save: Option<&SaveBand>, folder: bool, scale: Scale, theme: &Theme) -> (u32, u32) {
    let listing = listing_viewport(save, scale);
    if let Some(band) = save {
        let panel = panel_layout(band, scale, theme).panel;
        return (
            listing.width,
            u32::try_from(panel.bottom()).unwrap_or(listing.height),
        );
    }
    if folder {
        let band = band_layout(scale, theme).band;
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
    let layout = band_layout(scale, theme);
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
        let text = band.message.as_deref().unwrap_or(FOLDER_PROMPT);
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
fn repaint<S: DirectorySource>(active: &mut ActivePick<S>, area: &Region) {
    active.owed.add_region(area);
}

/// Paint what `active` owes into the buffer its window already holds,
/// answering whether it was painted. A buffer that cannot be kept is painted
/// whole, and one the heap will not give leaves the previous frame on screen
/// (fail closed) and the paint still owed.
fn paint<S: DirectorySource>(
    active: &ActivePick<S>,
    shell: &mut DesktopShell,
    compositor: &mut Compositor,
) -> bool {
    let scale = compositor.scale();
    let (theme, mut artwork) = shell.theme_and_artwork();
    let parts = (active.save.as_ref(), active.folder.as_ref());
    let size = pick_size(parts.0, parts.1.is_some(), scale, theme);
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
fn redraw<S: DirectorySource>(
    active: &mut ActivePick<S>,
    shell: &DesktopShell,
    compositor: &Compositor,
) {
    let (width, height) = pick_size(
        active.save.as_ref(),
        active.folder.is_some(),
        compositor.scale(),
        shell.session().active_theme(),
    );
    active.owed.add(Rect::new(0, 0, width, height));
}

/// Owe a save's panel or a folder's band a paint alone: a change to the name,
/// a question, progress, or a refusal moves nothing in the listing.
fn redraw_band<S: DirectorySource>(
    active: &mut ActivePick<S>,
    shell: &DesktopShell,
    compositor: &Compositor,
) {
    let (scale, theme) = (compositor.scale(), shell.session().active_theme());
    let band = match active.save.as_ref() {
        Some(save) => panel_layout(save, scale, theme).panel,
        None => band_layout(scale, theme).band,
    };
    active.owed.add(band);
}

/// Put the picker window at the size of what it now shows, painted whole — a
/// save's browser shown or folded away — keeping its bottom edge where it was,
/// so the panel the pointer is on stays put. A window that would leave the
/// work area comes back onto it.
fn refit<S: DirectorySource>(
    active: &mut ActivePick<S>,
    shell: &mut DesktopShell,
    compositor: &mut Compositor,
) {
    let scale = compositor.scale();
    let parts = (active.save.as_ref(), active.folder.as_ref());
    let before = compositor.window(active.wm).map(tairix_wm::Window::bounds);
    {
        let (theme, mut artwork) = shell.theme_and_artwork();
        let (width, height) = pick_size(parts.0, parts.1.is_some(), scale, theme);
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
