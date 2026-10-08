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
//! **save** chooses where a document goes: a name field and its two answers
//! sit under the listing, a name that already names a file is replaced only
//! once the user says so, and the file is delegated write-only.
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

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::fs::OpenFlags;
use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_abi::window_ipc::{PickPurpose, SaveEndings, WINDOW_TITLE_MAX};
use tairix_abi::{Errno, FS_NAME_MAX};
use tairix_browse::render::{
    entry_index_at, listing_damage, render_into, reveal_selection, scroll_pointer, scroll_wheel,
    shown_listing, toolbar_command_at,
};
use tairix_browse::ManagerChrome;
use tairix_browse::ToolbarBand;
use tairix_browse::{
    apply_command, vfs, BrowseError, Browser, DirectorySource, Ending, EntryKind, WatchUpdate,
    WIN_HEIGHT, WIN_WIDTH,
};
use tairix_controls::{damage, Button, ButtonContent, ControlRole, TextAction, TextField};
use tairix_font::BitmapFont;
use tairix_geometry::Scale;
use tairix_icon::NoArtwork;
use tairix_theme::{TextRole, Theme};
use tairix_wm::{Compositor, InputEvent, Point, PointerButton, Rect, Region, Surface, WindowId};

use crate::shell::DesktopShell;

/// Fixed prefix of an open picker's title — on the taskbar and in the window
/// chrome, so the user always sees which UI is asking on an app's behalf. The
/// directory being browsed follows it.
pub const PICKER_TITLE: &str = "Choose a file";

/// Fixed prefix of a save picker's title.
pub const SAVE_TITLE: &str = "Save as";

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

/// The save band's committing answer, and what it becomes while the user is
/// asked whether to replace a file.
const SAVE_LABEL: &str = "Save";
const REPLACE_LABEL: &str = "Replace";
const CANCEL_LABEL: &str = "Cancel";

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
}

impl PickAccess {
    /// The flags a file chosen to save into is opened with, write-only; `None`
    /// for an existing file chosen to open.
    #[must_use]
    pub const fn save_flags(self) -> Option<OpenFlags> {
        match self {
            Self::Read => None,
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
        /// disclosed to the requesting app.
        path: String,
        /// How to open it.
        access: PickAccess,
    },
    /// The user dismissed the picker without choosing; it is closed.
    Cancelled {
        /// The window-channel id of the requesting app's window.
        for_window: u64,
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
    /// A validated `PickFile` for `for_window` was accepted by the window
    /// engine; open the picker UI for `purpose`.
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
        for_window: u64,
        purpose: &PickPurpose,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Errno>;

    /// The window-channel window `window_id` is gone (closed by its owner
    /// or torn down after the owner exited): if its pick is showing, take
    /// the picker down. No conclusion is delivered — the engine already
    /// dropped the window's pending pick with its record.
    fn abort_for(&mut self, window_id: u64, shell: &mut DesktopShell, compositor: &mut Compositor);
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
}

/// A file the user is being asked whether to replace: the name the question
/// shows, and the one path a yes replaces.
struct Replacing {
    name: String,
    path: String,
}

/// A save pick's own controls, under the listing: the name to save as, and
/// the two answers.
struct SaveBand {
    name: TextField,
    cancel: Button,
    save: Button,
    /// The name offered when the pick began, put back after the field named a
    /// folder the picker then went into.
    suggested: String,
    /// The endings the requester can write a name under.
    endings: SaveEndings,
    /// The file the user is being asked whether to replace.
    replacing: Option<Replacing>,
}

impl SaveBand {
    fn new(suggested: &str, endings: SaveEndings) -> Self {
        let mut name = TextField::new()
            .with_text(suggested)
            .with_max_len(FS_NAME_MAX);
        name.set_focused(true);
        Self {
            name,
            cancel: Button::labelled(CANCEL_LABEL),
            save: save_button(false),
            suggested: String::from(suggested),
            endings,
            replacing: None,
        }
    }

    /// `name` as the requester can write it: as typed where it ends as the
    /// requester asks, given the first ending where it has none, and refused,
    /// with the endings it may take, where it has another.
    fn held(&self, name: String) -> Result<String, String> {
        if self.endings.accepts(&name) {
            return Ok(name);
        }
        match self.endings.iter().next() {
            Some(first) if Ending::of(&name).is_none() => {
                let mut named = name;
                named.push_str(first);
                tairix_path::validate_file_name(&named).map_err(|err| err.to_string())?;
                Ok(named)
            }
            _ => {
                let mut refusal = String::from("Name it to end in one of:");
                for ending in self.endings.iter() {
                    refusal.push(' ');
                    refusal.push_str(ending);
                }
                Err(refusal)
            }
        }
    }

    /// Ask whether to replace the file `name` at `path`.
    fn ask_to_replace(&mut self, name: String, path: String) {
        self.name
            .set_message(Some(format!("“{name}” already exists. Replace it?")));
        self.save = save_button(true);
        self.replacing = Some(Replacing { name, path });
    }

    /// Go back to editing the name, stating `message` if there is one.
    fn edit(&mut self, message: Option<String>) {
        self.name.set_message(message);
        if self.replacing.take().is_some() {
            self.save = save_button(false);
        }
    }
}

/// The band's committing button: the ordinary answer, or the destructive one
/// while a replacement is being asked about.
fn save_button(replacing: bool) -> Button {
    if replacing {
        Button::new(
            ButtonContent::Label(String::from(REPLACE_LABEL)),
            ControlRole::Destructive,
        )
    } else {
        Button::new(
            ButtonContent::Label(String::from(SAVE_LABEL)),
            ControlRole::Primary,
        )
    }
}

/// One live pick: the requesting window, the picker's compositor window,
/// and the browser state behind it.
struct ActivePick<S: DirectorySource> {
    for_window: u64,
    wm: WindowId,
    browser: Browser<S>,
    /// The save band, for a pick choosing where to save.
    save: Option<SaveBand>,
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
}

impl<S: DirectorySource> ActivePick<S> {
    fn title_prefix(&self) -> &'static str {
        if self.save.is_some() {
            SAVE_TITLE
        } else {
            PICKER_TITLE
        }
    }
}

/// The session's picker engine over an injected directory-source factory
/// (`F` builds the session-authority source each pick starts from — the
/// live VFS listing calls in production, an in-memory tree in tests).
pub struct SessionPicker<S: DirectorySource, F: FnMut() -> S> {
    source: F,
    /// Root-first components of the directory each pick opens at — the
    /// user's `UserFiles` in production, so the picker starts among the
    /// user's own files rather than at the storage-forest root. Empty means
    /// the root `/`.
    start: Vec<String>,
    active: Option<ActivePick<S>>,
    /// The serial the last open asked for.
    serial: u64,
}

impl<S: DirectorySource, F: FnMut() -> S> SessionPicker<S, F> {
    /// An idle picker over `source`, opening each pick at the root `/`.
    pub const fn new(source: F) -> Self {
        Self {
            source,
            start: Vec::new(),
            active: None,
            serial: 0,
        }
    }

    /// Open each pick at the directory named by root-first `start` instead of
    /// the root — the session points its picker at the logged-in user's
    /// `UserFiles` so the user lands among their own files. A start that
    /// cannot be listed climbs to the nearest folder above it that can, and
    /// only a refusal of the root itself refuses the pick (see
    /// [`begin`](PickerSlot::begin)).
    #[must_use]
    pub fn starting_at(mut self, start: Vec<String>) -> Self {
        self.start = start;
        self
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

    /// Apply one key press to the showing picker.
    ///
    /// An open: `Down`/`Up` move the selection, `Enter` descends into a
    /// selected directory or chooses a selected regular file, and
    /// `Backspace` climbs to the parent. A save: every key edits the name,
    /// `Enter` saves. `Escape` cancels either — or, while the user is asked
    /// whether to replace a file, goes back to the name. A refused
    /// navigation changes nothing, and while an open is being answered only
    /// `Escape` is heard.
    pub fn handle_key(
        &mut self,
        record: &KeyInput,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let KeyInput::Pressed { key, .. } = record else {
            return None;
        };
        let (saving, waiting) = self
            .active
            .as_ref()
            .map(|active| (active.save.is_some(), active.waiting.is_some()))?;
        if waiting {
            return matches!(key, KeyValue::Named(NamedKeyCode::Escape))
                .then(|| self.cancel(shell, compositor))
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
    /// changes nothing, and none is heard while an open is being answered.
    pub fn handle_click(
        &mut self,
        local: Point,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        if self.active.as_ref()?.waiting.is_some() {
            return None;
        }
        let press = InputEvent::PointerPressed {
            button: PointerButton::Primary,
        };
        if self.handle_pointer(local, &press, shell, compositor) {
            return None;
        }
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let viewport = picker_viewport(scale);
        if !viewport.contains(local) {
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
        self.navigate(shell, compositor, move |browser| {
            open_or_choose(browser, index)
        })
    }

    /// Route a pointer `event` at the picker-window-local position `local` to
    /// the listing's scroll bar, answering whether the bar took it — so a
    /// press on the bar is never also a press on a row.
    ///
    /// The bar keeps what a press on it started: an end button or the track
    /// steps, the thumb drags, and the moves and the release that follow are
    /// the bar's until the release ends them. Only what it repainted — the bar,
    /// and the items a move slid — is painted again, into the picker's own
    /// buffer.
    pub fn handle_pointer(
        &mut self,
        local: Point,
        event: &InputEvent,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        let scale = compositor.scale();
        let mut drew = damage::sink();
        let Some(repainted) = scroll_pointer(
            &mut active.browser,
            scale,
            shell.session().active_theme(),
            picker_viewport(scale),
            PICKER_TOOLBAR,
            local,
            event,
            &mut drew,
        ) else {
            return false;
        };
        if repainted {
            repaint(active, &drew, shell, compositor);
        }
        true
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
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let mut moved = damage::sink();
        if !scroll_wheel(
            &mut active.browser,
            scale,
            theme,
            picker_viewport(scale),
            PICKER_TOOLBAR,
            (dx, dy),
            &mut moved,
        ) {
            return false;
        }
        repaint(active, &moved, shell, compositor);
        true
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
                let viewport = picker_viewport(scale);
                let before = shown_listing(&active.browser, scale, theme, viewport, PICKER_TOOLBAR);
                match active.browser.apply_changes(changes) {
                    Ok(Some(_)) => {}
                    Ok(None) => return,
                    // Without the memory to merge them, the folder is read again.
                    Err(_) => return self.follow(WatchUpdate::Rescan, shell, compositor),
                }
                let mut moved = Region::new();
                if listing_damage(
                    &before,
                    &active.browser,
                    scale,
                    theme,
                    viewport,
                    PICKER_TOOLBAR,
                    &mut moved,
                ) {
                    repaint(active, &moved, shell, compositor);
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
        let for_window = self.close(shell, compositor)?;
        Some(PickStep::Cancelled { for_window })
    }

    /// The embedder's answer to the open `serial` names: whether the file
    /// could be opened.
    ///
    /// An opened file ends the pick. A refused open ends an open pick too, but
    /// a save stays up to say why — or, for a name that was taken after the
    /// listing was read, to ask whether to replace it. An answer for an
    /// attempt that has gone (the user cancelled, the requesting window
    /// closed) is `None`: whatever it opened is the embedder's to close.
    pub fn opened(
        &mut self,
        serial: u64,
        result: Result<(), Errno>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickEnd> {
        let active = self.active.as_mut()?;
        let waiting = active.waiting.take_if(|waiting| waiting.serial == serial)?;
        let Some(band) = active.save.as_mut() else {
            let for_window = self.close(shell, compositor)?;
            return Some(match result {
                Ok(()) => PickEnd::Chosen {
                    for_window,
                    name: waiting.name,
                },
                Err(_) => PickEnd::Refused { for_window },
            });
        };
        match result {
            Ok(()) => {
                let for_window = self.close(shell, compositor)?;
                Some(PickEnd::Chosen {
                    for_window,
                    name: waiting.name,
                })
            }
            Err(Errno::AlreadyExists) if waiting.access == PickAccess::Create => {
                // Asked only about the folder still showing: a yes must
                // replace the file the user can see the question is about.
                let shown = shown_path(active.browser.components(), &waiting.name);
                if shown.as_deref() == Some(waiting.path.as_str()) {
                    band.ask_to_replace(waiting.name, waiting.path);
                } else {
                    band.edit(Some(format!(
                        "Not saved: “{}” already exists",
                        waiting.name
                    )));
                }
                redraw_band(active, shell, compositor);
                None
            }
            Err(err) => {
                band.edit(Some(format!("Not saved: {err}")));
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
        let field = band_layout(compositor.scale(), shell.session().active_theme()).field;
        let active = self.active.as_mut()?;
        let mut drew = damage::sink();
        let band = active.save.as_mut()?;
        let action = band.name.on_key(key, modifiers, field, &mut drew);
        match action {
            Some(TextAction::Submitted) => return self.submit(shell, compositor),
            Some(TextAction::Cancelled) => {
                if band.replacing.is_none() {
                    return self.cancel(shell, compositor);
                }
                band.edit(None);
                redraw_band(active, shell, compositor);
            }
            // A different name is not the one the question was about.
            Some(TextAction::Edited) => {
                band.edit(None);
                redraw_band(active, shell, compositor);
            }
            None if !drew.is_empty() => repaint(active, &drew, shell, compositor),
            None => {}
        }
        None
    }

    /// A press on a save's band, below the listing.
    fn band_click(
        &mut self,
        local: Point,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let scale = compositor.scale();
        let theme = shell.session().active_theme();
        let layout = band_layout(scale, theme);
        let active = self.active.as_mut()?;
        let band = active.save.as_mut()?;
        if layout.save.contains(local) {
            return self.submit(shell, compositor);
        }
        if layout.cancel.contains(local) {
            if band.replacing.is_none() {
                return self.cancel(shell, compositor);
            }
            band.edit(None);
            redraw_band(active, shell, compositor);
            return None;
        }
        if layout.field.contains(local) {
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
                let _ = band
                    .name
                    .on_pointer(&event, layout.field, scale, theme, &mut drew);
            }
            repaint(active, &drew, shell, compositor);
        }
        None
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
        band.name.set_text(&name);
        band.edit(None);
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
        if let Some(Replacing { name, path }) = band.replacing.take() {
            return self.request_at(name, path, PickAccess::Replace, shell, compositor);
        }
        let typed = band.name.text().to_string();
        // A name naming a folder goes into it, whatever a file must end in.
        let folder = active
            .browser
            .entries()
            .iter()
            .position(|entry| entry.name() == typed && entry.kind().is_directory());
        let held = tairix_path::validate_file_name(&typed)
            .map_err(|err| err.to_string())
            .and_then(|()| match folder {
                Some(_) => Ok(typed),
                None => band.held(typed),
            });
        let name = match held {
            Ok(name) => name,
            Err(refusal) => {
                band.edit(Some(refusal));
                redraw_band(active, shell, compositor);
                return None;
            }
        };
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
            return self.request(name, PickAccess::Create, shell, compositor);
        };
        let kind = active.browser.entries()[index].kind();
        match kind {
            EntryKind::File => {
                let path = shown_path(active.browser.components(), &name)?;
                band.ask_to_replace(name, path);
                redraw_band(active, shell, compositor);
                None
            }
            // Naming a folder goes into it, and the name offered comes back.
            _ if kind.is_directory() => {
                let suggested = band.suggested.clone();
                band.name.set_text(&suggested);
                band.edit(None);
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
                band.edit(Some(format!("“{name}” is {what}; choose another name")));
                redraw_band(active, shell, compositor);
                None
            }
        }
    }

    /// Ask the embedder to open the file `name` names in the directory the
    /// picker shows.
    fn request(
        &mut self,
        name: String,
        access: PickAccess,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        // A name the shared spelling refuses is not chosen rather than
        // guessed at.
        let path = shown_path(self.active.as_ref()?.browser.components(), &name)?;
        self.request_at(name, path, access, shell, compositor)
    }

    /// Ask the embedder to open the file `name` at `path`.
    fn request_at(
        &mut self,
        name: String,
        path: String,
        access: PickAccess,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<PickStep> {
        let active = self.active.as_mut()?;
        self.serial += 1;
        let serial = self.serial;
        if let Some(band) = active.save.as_mut() {
            band.edit(Some(String::from("Saving…")));
            redraw_band(active, shell, compositor);
        }
        active.waiting = Some(Waiting {
            serial,
            name,
            path: path.clone(),
            access,
        });
        Some(PickStep::Open {
            serial,
            for_window: active.for_window,
            path,
            access,
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
        let titled = picker_title(prefix, active.browser.components());
        match step(&mut active.browser) {
            NavOutcome::None => None,
            NavOutcome::Redraw => {
                // Keep the (possibly moved) selection on screen before the
                // repaint, scrolling the shared view the least it can.
                reveal_selection(
                    &mut active.browser,
                    scale,
                    shell.session().active_theme(),
                    picker_viewport(scale),
                    PICKER_TOOLBAR,
                );
                redraw(active, shell, compositor);
                // The picker is session-owned and has no window channel of
                // its own, so it retitles through the compositor. A step that
                // only moved the selection leaves the title alone rather than
                // re-presenting the taskbar for an unchanged label.
                let located = picker_title(prefix, active.browser.components());
                if located != titled {
                    shell.retitle_window(compositor, active.wm, &located);
                    // A question about a file in the folder left behind is
                    // not one about anything showing now.
                    if let Some(band) = active.save.as_mut().filter(|band| band.replacing.is_some())
                    {
                        band.edit(None);
                        redraw_band(active, shell, compositor);
                    }
                }
                None
            }
            NavOutcome::Chosen(name) => self.request(name, PickAccess::Read, shell, compositor),
        }
    }

    /// Close the picker window, answering the window the pick was for.
    fn close(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) -> Option<u64> {
        let active = self.active.take()?;
        let _ = shell.close_window(compositor, active.wm);
        Some(active.for_window)
    }
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
        for_window: u64,
        purpose: &PickPurpose,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Result<(), Errno> {
        if self.active.is_some() {
            return Err(Errno::AlreadyExists);
        }
        // Listed under the session's authority before any UI state exists, so
        // a refused root refuses the pick with nothing half-open.
        let mut at = self.start.clone();
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
        let save = match purpose {
            PickPurpose::Open => None,
            PickPurpose::Save { suggested, endings } => {
                Some(SaveBand::new(suggested.as_str(), *endings))
            }
        };
        let scale = compositor.scale();
        let surface = {
            let theme = shell.session().active_theme();
            let (width, height) = window_size(save.is_some(), scale, theme);
            let mut surface = Surface::new(width, height).ok_or(Errno::LengthOutOfRange)?;
            paint_listing(&mut surface, &browser, scale, theme);
            if let Some(band) = &save {
                paint_band(&mut surface, band, scale, theme);
            }
            surface
        };
        let prefix = if save.is_some() {
            SAVE_TITLE
        } else {
            PICKER_TITLE
        };
        let titled = picker_title(prefix, browser.components());
        let wm = shell
            .open_window(compositor, PICKER_ORIGIN, surface, titled.clone())
            .ok_or(Errno::NoSpace)?;
        // A dialog, so it wears the window manager's frame: the title says
        // which UI is asking on the application's behalf, and the close
        // control cancels the pick exactly as Escape does. Fixed-size,
        // because the shared browser view renders at one geometry.
        shell.decorate_window(compositor, wm, &titled, false);
        let lost = browser.is_listing();
        self.active = Some(ActivePick {
            for_window,
            wm,
            browser,
            save,
            waiting: None,
            shown: false,
            lost,
            reread_owed: false,
        });
        Ok(())
    }

    fn abort_for(&mut self, window_id: u64, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.for_window == window_id)
        {
            let _ = self.close(shell, compositor);
        }
    }
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
fn picker_title(prefix: &str, components: &[String]) -> String {
    let budget = WINDOW_TITLE_MAX - prefix.len() - PICKER_TITLE_SEPARATOR.len();
    let mut title = String::from(prefix);
    title.push_str(PICKER_TITLE_SEPARATOR);
    title.push_str(&vfs::spell_title_location(components, budget));
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
        return match browser.open_index(index) {
            Ok(()) => NavOutcome::Redraw,
            // A refused descent (unreadable directory) changes nothing.
            Err(_) => NavOutcome::None,
        };
    }
    NavOutcome::Chosen(String::from(entry.name()))
}

/// The listing's part of the picker window at `scale`: the shared
/// browser-view physical geometry, which every paint and hit-test of the
/// listing lays out in.
fn picker_viewport(scale: Scale) -> Rect {
    Rect::new(
        0,
        0,
        scale.scale_length(WIN_WIDTH),
        scale.scale_length(WIN_HEIGHT),
    )
}

/// Where a save band's controls sit, in picker-window-local pixels.
struct BandLayout {
    band: Rect,
    field: Rect,
    cancel: Rect,
    save: Rect,
}

/// The save band under the listing at `scale`: the name field, with room
/// beneath it for the one line it states a refusal or a question in, and the
/// two answers beside it.
fn band_layout(scale: Scale, theme: &Theme) -> BandLayout {
    let listing = picker_viewport(scale);
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
    // The committing answer is as wide as its wider label, so asking to
    // replace moves nothing.
    let save_w = save_button(false)
        .measured_width(scale, theme)
        .max(save_button(true).measured_width(scale, theme));
    let cancel_w = Button::labelled(CANCEL_LABEL).measured_width(scale, theme);
    let row_top = top.saturating_add(i32::try_from(inset).unwrap_or(i32::MAX));
    let right = listing.width.saturating_sub(inset);
    let save_x = right.saturating_sub(save_w);
    let cancel_x = save_x.saturating_sub(gap).saturating_sub(cancel_w);
    let field_w = cancel_x.saturating_sub(gap).saturating_sub(inset);
    let at = |x: u32| i32::try_from(x).unwrap_or(i32::MAX);
    BandLayout {
        band,
        field: Rect::new(at(inset), row_top, field_w, row.saturating_add(message)),
        cancel: Rect::new(at(cancel_x), row_top, cancel_w, row),
        save: Rect::new(at(save_x), row_top, save_w, row),
    }
}

/// The picker window's physical size: the listing, and the save band under it
/// for a save.
fn window_size(saving: bool, scale: Scale, theme: &Theme) -> (u32, u32) {
    let listing = picker_viewport(scale);
    if saving {
        let band = band_layout(scale, theme).band;
        (listing.width, listing.height.saturating_add(band.height))
    } else {
        (listing.width, listing.height)
    }
}

/// Paint the picker's listing into `surface` through the active theme.
fn paint_listing<S: DirectorySource>(
    surface: &mut Surface,
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
) {
    // The picker is strictly read-only, so it draws no manager chrome at all:
    // no write tools (New Folder, the Trash location, and Empty Trash are the
    // file manager's alone — no write authority here) and no places rail (a
    // pick is bounded to the tree the requesting application was authorised to
    // be shown, and one-click jumps to arbitrary volumes would widen it).
    // The picker has no per-entry artwork cache yet, so it resolves every grid
    // tile to its built-in glyph through the always-empty artwork lookup.
    render_into(
        surface,
        browser,
        scale,
        theme,
        picker_viewport(scale),
        &PICKER_CHROME,
        &mut NoArtwork,
    );
}

/// Paint a save's band under the listing.
fn paint_band(surface: &mut Surface, band: &SaveBand, scale: Scale, theme: &Theme) {
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
    band.name.render(surface, layout.field, scale, theme);
    band.cancel.render(surface, layout.cancel, scale, theme);
    band.save.render(surface, layout.save, scale, theme);
}

/// Repaint the parts of the picker window `area` covers into the buffer it
/// already holds. A buffer that cannot be kept is painted whole, and one the
/// heap will not give leaves the previous frame on screen (fail closed).
fn repaint<S: DirectorySource>(
    active: &ActivePick<S>,
    area: &Region,
    shell: &DesktopShell,
    compositor: &mut Compositor,
) {
    let scale = compositor.scale();
    let theme = shell.session().active_theme();
    let size = window_size(active.save.is_some(), scale, theme);
    compositor.repaint_window(active.wm, size, area, |surface, rects| {
        for rect in rects {
            let Some((x, y)) = rect.surface_origin() else {
                continue;
            };
            surface.with_clip(x, y, rect.width, rect.height, |surface| {
                paint_listing(surface, &active.browser, scale, theme);
                if let Some(band) = &active.save {
                    paint_band(surface, band, scale, theme);
                }
            });
        }
    });
}

/// Repaint the whole picker window after a navigation change, into the buffer
/// it already holds: a step changes what the window shows, never its size.
fn redraw<S: DirectorySource>(
    active: &ActivePick<S>,
    shell: &DesktopShell,
    compositor: &mut Compositor,
) {
    let (width, height) = window_size(
        active.save.is_some(),
        compositor.scale(),
        shell.session().active_theme(),
    );
    let mut whole = Region::new();
    whole.add(Rect::new(0, 0, width, height));
    repaint(active, &whole, shell, compositor);
}

/// Repaint a save's band alone: a change to the name, a question, or a
/// refusal moves nothing in the listing.
fn redraw_band<S: DirectorySource>(
    active: &ActivePick<S>,
    shell: &DesktopShell,
    compositor: &mut Compositor,
) {
    let mut band = Region::new();
    band.add(band_layout(compositor.scale(), shell.session().active_theme()).band);
    repaint(active, &band, shell, compositor);
}
