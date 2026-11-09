//! The document host: every window of a document application, the events that
//! reach them, the file each is saved to, and the one queue its saves are
//! written on.
//!
//! The application supplies the engine a window shows ([`DocumentView`]) and
//! what only it does ([`DocumentApp`]): reading a document in, its own
//! requests, its own workers, its pixels. Everything else — opening, closing
//! and quitting with changes asked about first, the trusted picker, saving in
//! the order asked, documents handed over on the icon bar, painting only what
//! changed — is here once.
//!
//! # The queue
//!
//! Saves share the application's queue with whatever else it must order with
//! them, each job answered in turn: two saves of one file reach it in the
//! order they were asked. Every window holds [`DocumentApp::JOBS_PER_WINDOW`]
//! rooms, and its own work past them is refused; a save beyond them grows
//! the queue by one for as long as it is outstanding, and a save is never
//! turned away while the memory for it can be had. A window that closes
//! keeps a room for each job it leaves behind until that job lands, and the
//! process ends only once every save has.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;
use core::fmt::Display;
use core::mem;
use core::ops::ControlFlow;

use tairix_abi::driver::display::DisplayMode;
use tairix_abi::input::{KeyInput, Modifiers as AbiModifiers};
use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
use tairix_abi::window_ipc::{
    AlertOutcome, AppBar, AppBarClick, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuRow,
    AppMenuSymbol, ClipboardKind, CursorShape, DialogModality, DocumentName, MenuOutcome,
    PickPurpose, ToolOver, WindowEvent, WindowRegion, WindowSizing,
};
use tairix_abi::{Errno, ProcId, DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, STDIN};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_rt::sync::WorkerWake;
use tairix_rt::work::{Desk, Worker, WorkerGuard};
use tairix_theme::{Theme, ThemeRegistry};
use tairix_util::defer::JobQueue;

use super::closing::{
    next_quit_step, reveal_refusal, save_changes_alert, CloseAnswer, QuitStanding, QuitStep,
};
use super::members::{members_of, route, Route};
use super::offers::{FileOffer, FileOffers, OfferStep};
use super::refit::Refit;
use super::title::{opening, Title, Withheld};
use super::{
    AppRequest, AppView, DocumentView, Outcome, Relayout, Request, SaveTarget, ToolGone, ToolMove,
    ViewOutcome,
};
use crate::app::{self, fail, report, RtWindowTransport, Wake, WindowPane};
use crate::appbar::{declaration, declare_app_bar, is_quit, QUIT_ROW};
use crate::client::{
    pinch_input_events, pointer_input_events, pointer_point, present_damage_list,
    scroll_input_events, Answer, DeclaredTip, EventDrain, EventError, EventSource, Parked,
    PostedRequest, Refused, Repaint, Target, WindowClient, WindowEvents,
};
use crate::desktop::Desktop;
use crate::document::{Access, DocumentFile, PickFor, SaveJob, SaveStep, SavedDocument, UNTITLED};
use crate::key_input_event;
use crate::mailbox::EventMailbox;
use tairix_util::utf8::refusal_headline;

/// The descriptor a document is read from and saved through, whether it was
/// cloned in at spawn or redeemed from a grant: it closes once its last
/// holder — the window, or a save still being written — lets go.
pub type Handle = tairix_rt::File;

/// A document application's window channel.
pub type Client = WindowClient<RtWindowTransport>;

type ViewOf<A> = <A as DocumentApp>::View;
type FacesOf<A> = <ViewOf<A> as DocumentView>::Faces;
type MenuKindOf<A> = <ViewOf<A> as DocumentView>::MenuKind;
type RequestOf<A> = Request<MenuKindOf<A>, <ViewOf<A> as DocumentView>::Own>;
type SnapshotOf<A> = <A as DocumentApp>::Snapshot;
type AppViewOf<A> = <A as DocumentApp>::AppView;
type AppOwnOf<A> = <AppViewOf<A> as AppView>::Own;

/// The wait-set token of the queue's answer wake.
const QUEUE_TOKEN: u64 = app::FIRST_APP_TOKEN;

/// The wait-set token of the font worker's answer wake.
const FONT_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;

/// The lowest token an application's own worker may be watched under.
pub const APP_TOKEN: u64 = app::FIRST_APP_TOKEN + 2;

/// The one thread that waits on the font service: the windows draw without
/// waiting, and it fetches what they drew provisionally.
type FontWorker = Worker<(), (), bool>;

/// The font worker's job: fetch what the windows drew provisionally,
/// answering whether anything landed.
fn fetch_fonts(_: &mut (), _: &mut ()) -> bool {
    tairix_font::fetch_deferred()
}

/// The icon-bar row that opens a new window, numbered past the convention's
/// own so the two never collide.
const NEW_WINDOW_ROW: u16 = QUIT_ROW + 1;

/// The first of the application's own icon-bar rows, after *New window*.
const FIRST_APP_ROW: u16 = NEW_WINDOW_ROW + 1;

/// A job for the queue, and the window it is for.
struct Job<W> {
    window: u64,
    work: W,
}

/// What the queue answered, and the window it is for.
struct Reply<A> {
    window: u64,
    answer: A,
}

/// What the queue carries out: a save, or work of the application's own
/// asked for by the document its window showed in `epoch`.
enum Queued<S, W> {
    Save(Save<S>),
    Own { epoch: u64, work: W },
}

/// What the queue answers.
enum Answered<E, A> {
    Saved(Saved<E>),
    Own { epoch: u64, answer: A },
}

type QueuedOf<A> = Queued<SnapshotOf<A>, <A as DocumentApp>::Work>;
type AnsweredOf<A> = Answered<<A as DocumentApp>::Failure, <A as DocumentApp>::Answer>;

/// The queue a document application's saves are written on, each job
/// answered in turn. The job travels in an `Option` so the worker takes it by
/// value.
type Queue<A> = Worker<
    (),
    Option<Job<QueuedOf<A>>>,
    Option<Reply<AnsweredOf<A>>>,
    JobQueue<Option<Job<QueuedOf<A>>>, Option<Reply<AnsweredOf<A>>>>,
>;

/// Carry out one job of application `A`'s queue, on its worker.
fn serve<A: DocumentApp>(
    _: &mut (),
    job: &mut Option<Job<QueuedOf<A>>>,
) -> Option<Reply<AnsweredOf<A>>> {
    let Job { window, work } = job.take()?;
    let answer = match work {
        Queued::Save(save) => Answered::Saved(save.write(A::write)),
        Queued::Own { epoch, work } => Answered::Own {
            epoch,
            answer: A::work(work),
        },
    };
    Some(Reply { window, answer })
}

/// A save for the queue to write.
struct Save<S> {
    job: SaveJob<Handle, S>,
    /// What the file written is called: what a refusal is stated under, even
    /// once its window has gone.
    name: String,
    /// The queue grew its room to take it.
    grew: bool,
}

impl<S> Save<S> {
    const fn new(job: SaveJob<Handle, S>, name: String) -> Self {
        Self {
            job,
            name,
            grew: false,
        }
    }

    /// Carry the save out through `write`, which is handed the job and the
    /// file's name, answering how it landed.
    fn write<E>(
        self,
        write: impl FnOnce(&SaveJob<Handle, S>, &str) -> Result<Option<Cow<'static, str>>, E>,
    ) -> Saved<E> {
        let result = write(&self.job, &self.name);
        Saved {
            target: self.job.target,
            generation: self.job.generation,
            rename: self.job.rename,
            name: self.name,
            grew: self.grew,
            result,
        }
    }
}

/// How a [`Save`] landed: on success, what its format could not keep.
struct Saved<E> {
    target: Arc<Handle>,
    generation: u64,
    rename: Option<String>,
    name: String,
    grew: bool,
    result: Result<Option<Cow<'static, str>>, E>,
}

/// A worker whose answer wake the park drains when its token fires: left
/// undrained, a wake reports ready for ever and turns the park into a spin.
pub trait AnswerWake {
    /// Consume the wake's readiness.
    fn drain(&self);
}

impl<S, Req, Ans, D: Desk<Req, Ans>> AnswerWake for Worker<S, Req, Ans, D> {
    fn drain(&self) {
        self.wake().drain();
    }
}

/// What a document application supplies beyond the engine its windows show.
pub trait DocumentApp: Sized {
    /// The engine a window shows.
    type View: DocumentView<Snapshot = Self::Snapshot>;
    /// What the application's own windows, which hold no document, show;
    /// [`NoAppView`](super::NoAppView) for an application with none.
    type AppView: AppView<Faces = <Self::View as DocumentView>::Faces>;
    /// Its frozen document, which crosses to the queue's worker to be written.
    type Snapshot: Send + Sync + 'static;
    /// What the application keeps for each window beside the host's own.
    type Extra: Default;
    /// Work of its own it orders with its saves on the queue.
    type Work: Send + 'static;
    /// What that work answers.
    type Answer: Send + 'static;
    /// Why a save may not reach its file.
    type Failure: Display + Send + 'static;

    /// The name its bundle, help and refusals go by.
    const NAME: &'static str;
    /// The size a window opens at, in logical pixels.
    const WINDOW_SIZE: (u32, u32);
    /// Room a window holds in the queue: everything an open window can have
    /// outstanding at once, its own work past it refused. A containment
    /// bound derived from what one window can ask for, not a capacity: the
    /// queue grows by it as windows open.
    const JOBS_PER_WINDOW: usize;
    /// The application's own icon-bar rows, after *New window*, in order:
    /// each one's function and label.
    const BAR_ROWS: &'static [(AppMenuSymbol, &'static str)] = &[];

    /// Bring the application's own state up for `desktop`: its own workers
    /// started ([`app::start_worker`]) and their answer wakes watched on `set`
    /// from [`APP_TOKEN`] on.
    ///
    /// # Errors
    ///
    /// The exit code to end with, the reason already stated.
    fn start(desktop: &Desktop, set: u64) -> Result<Self, i32>;

    /// The application's own workers, each with the token its answer wake is
    /// watched under.
    fn wakes(&self) -> Vec<(u64, Arc<dyn AnswerWake>)>;

    /// The faces text is set in under `theme` at `scale`.
    fn faces(theme: &Theme, scale: Scale) -> <Self::View as DocumentView>::Faces;

    /// The region a window's engine reports what it repaints into.
    fn damage_sink() -> Region;

    /// Write `job` to the file called `name` it goes through, on the queue's
    /// worker: the document, cut to its length and made durable. Answers
    /// what the format could not keep of it, said with the save.
    ///
    /// # Errors
    ///
    /// Why it did not reach the file.
    fn write(
        job: &SaveJob<Handle, Self::Snapshot>,
        name: &str,
    ) -> Result<Option<Cow<'static, str>>, Self::Failure>;

    /// Carry out `work`, on the queue's worker.
    fn work(work: Self::Work) -> Self::Answer;

    /// Whether queueing `work` withdraws `waiting`, the same window's work
    /// of an earlier ask it supersedes.
    fn supersedes(_work: &Self::Work, _waiting: &Self::Work) -> bool {
        false
    }

    /// Whether `work` is still wanted once the document that asked for it
    /// has gone — its window closed, or showing another — so it is not
    /// withdrawn, and its answer reaches [`orphaned`](Self::orphaned).
    fn outlives_document(_work: &Self::Work) -> bool {
        false
    }

    /// Take the room in its own workers one more window needs.
    ///
    /// # Errors
    ///
    /// Why not: no window opens.
    fn reserve_window(&mut self) -> Result<(), &'static str> {
        Ok(())
    }

    /// Give back what [`reserve_window`](Self::reserve_window) took.
    fn release_window(&mut self) {}

    /// Withdraw what window `window` has waiting on the application's own
    /// workers: it closed, or shows another document.
    fn withdraw(&mut self, _window: u64) {}

    /// A view of a new document.
    ///
    /// # Errors
    ///
    /// Why none could be made.
    fn untitled(host: &Host<Self>) -> Result<Self::View, String>;

    /// The view a window shows while the document `name`, handed over with
    /// `access`, is read in.
    ///
    /// # Errors
    ///
    /// Why none could be made.
    fn placeholder(host: &Host<Self>, name: &str, access: Access) -> Result<Self::View, String>;

    /// Begin reading `handle`, called `name`, into window `index`.
    fn load(host: &mut Host<Self>, index: usize, handle: Arc<Handle>, name: String);

    /// Take in what the application's own workers have answered.
    fn collect(host: &mut Host<Self>);

    /// Carry out what came due by `now_ns`, answering whether an answer is
    /// already waiting, carried out here for want of a worker.
    fn turn(host: &mut Host<Self>, now_ns: u64) -> bool;

    /// When the loop is next to wake with no event to wake it.
    fn deadline(host: &mut Host<Self>, now_ns: u64) -> Option<u64>;

    /// The machine's memory-pressure band moved.
    fn pressure(host: &mut Host<Self>);

    /// Carry out a request of window `index` only this application makes.
    fn request(host: &mut Host<Self>, index: usize, request: <Self::View as DocumentView>::Own);

    /// What window `index`'s own work on the queue answered, for the
    /// document it still shows.
    fn answered(host: &mut Host<Self>, index: usize, answer: Self::Answer);

    /// What window `window`'s own work on the queue answered once the
    /// document that asked had gone: by default it is let go.
    fn orphaned(_host: &mut Host<Self>, _window: u64, _answer: Self::Answer) {}

    /// The user chose where the window at `index` writes a copy of its
    /// document: `file`, called `name`. The application writes it — on its
    /// queue, never on the event loop — leaving the document's own file and
    /// name as they are. An application offering no copies never sees this.
    fn copy_chosen(_host: &mut Host<Self>, _index: usize, _file: Arc<Handle>, _name: String) {}

    /// The user chose a file to read into the window at `index`'s document —
    /// through the picker, or by dropping it at `at`, a window-local point its
    /// view answered places it: `file`, called `name`, handed over read-only.
    /// The application reads it — on its queue, never on the event loop. One
    /// that asks for no import and places nothing never sees this.
    fn import_chosen(
        _host: &mut Host<Self>,
        _index: usize,
        _file: Handle,
        _name: String,
        _at: Option<Point>,
    ) {
    }

    /// What the clipboard held when the window at `index` asked to paste
    /// ([`Host::paste`]): `None` when it was empty. An application that
    /// pastes nothing never sees this.
    fn pasted(
        _host: &mut Host<Self>,
        _index: usize,
        _contents: Result<Option<(ClipboardKind, Vec<u8>)>, Errno>,
    ) {
    }

    /// Window `index`'s engine was driven, and what it asked is about to be
    /// carried out: what it changed is handed to the other windows of its
    /// document here, so a save asked now freezes it.
    fn driven(_host: &mut Host<Self>, _index: usize) {}

    /// The process is ending: see the application's own outstanding work out,
    /// as the host sees its saves out, so nothing asked for is dropped.
    fn leaving(_host: &mut Host<Self>) {}

    /// Icon-bar row `row` of [`BAR_ROWS`](Self::BAR_ROWS) was chosen, under
    /// the activation the choice grants, so a window raised now is given the
    /// keyboard.
    fn bar_chosen(_host: &mut Host<Self>, _row: usize) {}

    /// Application window `index` is about to close, by its close mark, its
    /// own asking, a quit, or with the document window it asks on behalf of.
    fn app_closing(_host: &mut Host<Self>, _index: usize) {}

    /// Carry out a request of application window `index` only this
    /// application makes.
    fn app_request(_host: &mut Host<Self>, _index: usize, _request: AppOwnOf<Self>) {}

    /// Draw application window `view` into `surface`, as far as its clip
    /// admits.
    fn render_app(
        &mut self,
        surface: &mut Surface,
        view: &Self::AppView,
        style: (&Theme, Scale, <Self::View as DocumentView>::Faces),
        focused: bool,
    );

    /// Draw `view` into `surface`, as far as its clip admits.
    fn render(
        &mut self,
        surface: &mut Surface,
        view: &Self::View,
        style: (&Theme, Scale, <Self::View as DocumentView>::Faces),
        focused: bool,
    );
}

/// One window, and the document it shows.
pub struct DocWindow<A: DocumentApp> {
    /// The engine it shows.
    pub view: A::View,
    /// What the engine reported changed since the window was last painted.
    pub damage: Region,
    /// What the application keeps for it.
    pub extra: A::Extra,
    chrome: Chrome,
    file: DocumentFile<Handle, SnapshotOf<A>>,
    reading: Reading,
    /// Kept off the screen while the document it was opened for is read in,
    /// so a read that lands soon shows that document's own window first.
    withheld: Withheld,
    /// The least size the session was last told it may take.
    least: (u32, u32),
    /// Counts the documents it has shown, so an answer to the one before is
    /// told apart.
    epoch: u64,
    /// Its own work on the queue not yet taken in: the room it keeps on
    /// closing.
    own_held: usize,
    /// The menu open over it or one of its tool windows: the open, so an
    /// outcome is matched to the gesture that asked for it, and the window it
    /// opened over, which holds the text an entry row commits.
    menu: Option<(u64, u64)>,
    /// The tool windows and the dialog it has open, as its view last asked.
    owned: Vec<OwnedPane>,
    /// Whether its view was last told it has the keyboard: it has while the
    /// window or one of its tool windows does.
    focus_told: bool,
    /// The alert asking whether to save its changes before it closes, while
    /// that is up.
    closing: Option<u64>,
    /// The quit under way asked it whether to save its changes, so its
    /// keeping them turns the quit down.
    quit_asked: bool,
    /// Closing it is to show the user where its document lives, as a
    /// secondary press on its close button asks, for as long as that close is
    /// under way.
    reveal_on_close: bool,
    /// The window whose document it shows, for a member window
    /// ([`Host::open_member`]): its file is that window's.
    primary: Option<u64>,
    /// A file the picker chose for it, kept while its name is asked for.
    chosen: Option<Chosen>,
    /// The quick-entry row of the menu open named that was committed, while
    /// its text is asked for.
    entering: Option<(u64, AppMenuItemId)>,
}

/// A file the picker handed a window, and what the pick was for.
struct Chosen {
    pick: PickFor,
    file: Handle,
    writable: bool,
}

/// Whether a window's document is still being read in; input waits until it
/// lands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reading {
    /// It shows its document.
    Done,
    /// A document is being read in over the one it showed.
    Over,
    /// It was opened for the document being read in and shows nothing else,
    /// so one that will not open takes it down.
    Alone,
}

/// A window a document window has open beside it — a torn-out palette or its
/// dialog — showing `rect` of the window's drawing: one more pane and surface.
struct OwnedPane {
    kind: Owned,
    /// The part of the view's drawing it shows.
    rect: Rect,
    /// How the desktop was last told it may be sized: fixed for a dialog.
    sizing: WindowSizing,
    /// How far into `rect` its client starts: past nothing unless the desktop
    /// fitted the window smaller than `rect`, whose rest is then scrolled to.
    scroll: Point,
    /// What a wheel turn has moved it short of a whole pixel, across then
    /// down.
    wheel_carry: (i64, i64),
    /// What the view reported changed in what it shows, in its own pixels.
    damage: Region,
    chrome: Chrome,
}

/// Which of a document window's views an input is fed to: the window's own,
/// or its dialog's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fed {
    Window,
    Dialog,
}

impl Fed {
    /// Where input landing on an owned window of `kind` goes: a tool window's
    /// is the window's own.
    const fn of(kind: Owned) -> Self {
        match kind {
            Owned::Tool(_) => Self::Window,
            Owned::Dialog(_) => Self::Dialog,
        }
    }
}

/// What an owned window is to its document window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Owned {
    /// The tool window the view calls by this id.
    Tool(u32),
    /// The window's dialog, holding the window as this says.
    Dialog(DialogModality),
}

impl OwnedPane {
    fn new(
        kind: Owned,
        (rect, sizing): (Rect, WindowSizing),
        chrome: Chrome,
        damage: Region,
    ) -> Self {
        Self {
            kind,
            rect,
            sizing,
            scroll: Point::ORIGIN,
            wheel_carry: (0, 0),
            damage,
            chrome,
        }
    }

    /// Show `wanted`'s rectangle of the drawing held to its sizing, its band
    /// reading `title`: re-mapped where the size moved, its range restated
    /// once it is, and repainted where it moved or was retitled. One wanted
    /// at another kind of sizing is left for its owner to open afresh.
    fn fit(&mut self, client: &mut Client, wanted: (Rect, WindowSizing), title: &str, name: &str) {
        let Some(refit) = Refit::of((self.rect, self.sizing), wanted) else {
            return;
        };
        if let Some((width, height)) = refit.resize {
            self.chrome.resize(client, width, height, name);
            self.scroll = Point::ORIGIN;
        }
        if refit.repaint {
            self.chrome.owe(Repaint::Whole);
        }
        self.rect = wanted.0;
        if let Some(sizing) = refit.restate {
            match client.set_sizing(self.chrome.id(), sizing) {
                Ok(()) => self.sizing = sizing,
                Err(err) => report(
                    name,
                    format_args!("a palette's size range was not restated: {err}"),
                ),
            }
        }
        if self.chrome.title.draft() != title {
            self.chrome.title.read(title);
            self.chrome.owe(Repaint::Whole);
        }
    }

    /// The user resized it to `size`: its surface follows, it shows that
    /// much of the drawing from its rectangle's corner, and it is drawn again
    /// whole.
    fn resized(&mut self, client: &mut Client, (width, height): (u32, u32), name: &str) {
        self.chrome.resize(client, width, height, name);
        self.rect = Rect::new(self.rect.left(), self.rect.top(), width, height);
        self.scroll = Point::ORIGIN;
        self.chrome.owe(Repaint::Whole);
    }

    /// Scroll what it shows by a wheel turn of `(dx, dy)` scroll units at
    /// `scale`, kept within what the desktop fitted it smaller than, and
    /// answer whether it moved. One that shows the whole of `rect` takes no
    /// scroll, so the turn is its view's.
    fn scroll_by(&mut self, units: (i32, i32), scale: Scale) -> bool {
        let mode = *self.chrome.pane.mode();
        let room = (
            self.rect.width.saturating_sub(mode.width_px),
            self.rect.height.saturating_sub(mode.height_px),
        );
        let before = self.scroll;
        self.scroll = super::scroll::scrolled(before, room, units, &mut self.wheel_carry, scale);
        room != (0, 0) && self.scroll != before
    }

    const fn tool(&self) -> Option<u32> {
        match self.kind {
            Owned::Tool(id) => Some(id),
            Owned::Dialog(_) => None,
        }
    }

    /// The part of the view's drawing its client shows now.
    fn shown(&self) -> Rect {
        let mode = self.chrome.pane.mode();
        Rect::new(
            self.rect.left().saturating_add(self.scroll.x),
            self.rect.top().saturating_add(self.scroll.y),
            mode.width_px.min(self.rect.width),
            mode.height_px.min(self.rect.height),
        )
    }

    /// `at` in the window's own pixels, in the view's drawing.
    fn to_view(&self, at: Point) -> Point {
        let shown = self.shown();
        Point::new(
            shown.left().saturating_add(at.x),
            shown.top().saturating_add(at.y),
        )
    }

    /// Take the part of `damage`, in the view's drawing, that it shows,
    /// owing a paint of it.
    fn take_damage(&mut self, damage: &Region) {
        let shown = self.shown();
        for rect in damage.rects() {
            let inside = rect.intersection(&shown);
            if !inside.is_empty() {
                self.damage.add(Rect::new(
                    inside.left() - shown.left(),
                    inside.top() - shown.top(),
                    inside.width,
                    inside.height,
                ));
            }
        }
        self.chrome
            .owe(Repaint::reported_if(!self.damage.is_empty()));
    }
}

/// A window of the application's own, holding no document.
pub struct AppWindow<A: DocumentApp> {
    /// What it shows.
    pub view: A::AppView,
    /// What it reported changed since the window was last painted.
    pub damage: Region,
    chrome: Chrome,
    /// The least size the session was last told it may take.
    least: (u32, u32),
    /// The document window it asks on behalf of, which it closes with.
    owner: Option<u64>,
}

impl<A: DocumentApp> AppWindow<A> {
    /// The session's id for the window.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.chrome.id()
    }

    /// Owe a paint of what the view reported, when it reported anything.
    pub fn owe_reported(&mut self) {
        self.chrome
            .owe(Repaint::reported_if(!self.damage.is_empty()));
    }

    /// Lay the window out again for its size, owing what the view reported,
    /// and the whole when `whole`.
    fn lay_out(
        &mut self,
        client: &mut Client,
        (theme, scale, faces): (&Theme, Scale, FacesOf<A>),
        whole: bool,
    ) {
        let least = self.view.min_size(theme, scale, faces);
        restate_least::<A>(client, &self.chrome.pane, &mut self.least, least);
        let mode = *self.chrome.pane.mode();
        self.view.lay_out(
            mode.width_px,
            mode.height_px,
            theme,
            scale,
            faces,
            &mut self.damage,
        );
        self.owe_reported();
        if whole {
            self.chrome.owe(Repaint::Whole);
        }
        self.chrome.pointing.owed = true;
    }
}

/// What every window the host keeps has, whatever it shows: its pane and the
/// surface it is drawn on, whether it has the keyboard, the pointer over it,
/// its title, and what it owes the screen.
struct Chrome {
    pane: WindowPane,
    /// Held for the window's life, so a clipped repaint leaves the pixels
    /// outside the clip alone.
    surface: Surface,
    focused: bool,
    pointing: Pointing,
    /// The session refused its last present, which has been said.
    present_refused: bool,
    title: Title,
    owed: Repaint,
}

impl Chrome {
    fn new(pane: WindowPane, surface: Surface, title: String) -> Self {
        Self {
            pane,
            surface,
            focused: true,
            pointing: Pointing::new(),
            present_refused: false,
            title: Title::new(title),
            owed: Repaint::Whole,
        }
    }

    const fn id(&self) -> u64 {
        self.pane.id()
    }

    fn owe(&mut self, repaint: Repaint) {
        self.owed = self.owed.merged(repaint);
    }

    /// Fit the pane to a size the session reported, saying so where the
    /// window keeps its size.
    fn resize(&mut self, client: &mut Client, width_px: u32, height_px: u32, name: &str) {
        let mode = app::mode_for(width_px, height_px);
        if self.pane.resize_with(client, &mode, &mut self.surface) == app::Fitted::Kept {
            report(name, app::SIZE_KEPT);
        }
    }

    /// Show `shape` and declare `tip` for where the pointer is.
    fn settle_pointer(
        &mut self,
        client: &mut Client,
        shape: CursorShape,
        tip: Option<(Rect, &str)>,
    ) {
        let id = self.id();
        if self.pointing.at.is_some() {
            self.pointing.show_shape(client, id, shape);
        }
        self.pointing.tip.declare(client, id, tip);
    }

    /// Paint what the window owes of `damage` through `render` and present
    /// it, its title first brought into line with the one it now reads.
    ///
    /// Each reported rectangle is painted under its own clip, so a keystroke
    /// rasterises what it changed rather than everything the changes' bounds
    /// span.
    fn present(
        &mut self,
        client: &mut Client,
        damage: &mut Region,
        mut render: impl FnMut(&mut Surface, bool),
    ) -> Result<(), Errno> {
        let owed = mem::replace(&mut self.owed, Repaint::Nothing);
        if owed == Repaint::Nothing {
            return Ok(());
        }
        let id = self.id();
        if self
            .title
            .changed()
            .is_some_and(|title| client.set_title(id, title).is_ok())
        {
            self.title.sent();
        }
        let repaint = if self.pane.content_released() {
            Repaint::Whole
        } else {
            owed
        };
        let mode = *self.pane.mode();
        let Some(parts) = present_damage_list(&mode, repaint, damage) else {
            damage.clear();
            return Ok(());
        };
        let focused = self.focused;
        for part in parts.rects() {
            self.surface
                .with_clip(part.x, part.y, part.width_px, part.height_px, |clipped| {
                    render(clipped, focused);
                });
        }
        damage.clear();
        self.pane.present_list(client, &self.surface, &parts)
    }

    /// Note how a present went: a refusal is said once, under `named`, and
    /// the window paints whole at its next chance.
    fn presented(&mut self, painted: Result<(), Errno>, app: &str, named: &str) {
        match painted {
            Ok(()) => self.present_refused = false,
            Err(err) => {
                if !self.present_refused {
                    report(
                        app,
                        format!("{named} could not be shown ({err}); it is drawn again at its next change"),
                    );
                }
                self.present_refused = true;
                self.owe(Repaint::Whole);
            }
        }
    }
}

/// What a window shows of the pointer: its shape, and the tip for what it is
/// over.
struct Pointing {
    /// Where the pointer last was over the window.
    at: Option<Point>,
    /// What it is over may have changed since its shape and tip were settled.
    owed: bool,
    shape: CursorShape,
    tip: DeclaredTip,
}

impl Pointing {
    const fn new() -> Self {
        Self {
            at: None,
            owed: true,
            shape: CursorShape::Arrow,
            tip: DeclaredTip::new(),
        }
    }

    /// Ask the session to show `shape` over window `id` when it is not
    /// already showing. A refusal is not asked again until the shape wanted
    /// changes.
    fn show_shape(&mut self, client: &mut Client, id: u64, shape: CursorShape) {
        if shape != self.shape {
            self.shape = shape;
            let _ = client.set_cursor(id, shape);
        }
    }
}

/// Which window asked, and for which of the documents it has shown: what an
/// application's own work carries, so its answer reaches only the document
/// that asked ([`Host::showing`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    window: u64,
    epoch: u64,
}

impl Stamp {
    /// The window that asked.
    #[must_use]
    pub const fn window(self) -> u64 {
        self.window
    }
}

impl<A: DocumentApp> DocWindow<A> {
    /// The session's id for the window.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.chrome.id()
    }

    /// Whether its document is still being read in.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.reading != Reading::Done
    }

    /// What work asked for now carries: this window, and the document it
    /// shows.
    #[must_use]
    pub const fn stamp(&self) -> Stamp {
        Stamp {
            window: self.id(),
            epoch: self.epoch,
        }
    }

    /// Whether it has the keyboard: it, or one of its tool windows.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.chrome.focused
            || self
                .owned
                .iter()
                .any(|pane| pane.tool().is_some() && pane.chrome.focused)
    }

    /// Whether a document opened now may take its place: never a member
    /// window's, which shows its primary's.
    #[must_use]
    pub fn pristine(&self) -> bool {
        self.primary.is_none() && !self.loading() && self.file.pristine(&self.view)
    }

    /// Whether it is a member window ([`Host::open_member`]).
    #[must_use]
    pub const fn is_member(&self) -> bool {
        self.primary.is_some()
    }

    /// Whether the document shown may be replaced in place: no save or file
    /// chooser of it is out, either of which would land on its replacement.
    fn replaceable(&self) -> bool {
        !self.file.saving() && !self.file.picking()
    }

    /// Owe a paint of what the engine reported, when it reported anything: a
    /// round that changed nothing on screen paints nothing.
    pub fn owe_reported(&mut self) {
        self.owe(Repaint::reported_if(!self.damage.is_empty()));
    }

    /// Whether a question that is part of closing it is up: whether to save
    /// its changes, or a view's own question of how.
    #[must_use]
    pub fn asking_to_close(&self) -> bool {
        self.closing.is_some() || self.view.asking_to_close()
    }

    /// Whether a close of it is under way: one of its questions is up, or a
    /// save it closes after.
    fn closing_under_way(&self) -> bool {
        self.asking_to_close() || self.file.closing()
    }

    /// Where it stands in the quit under way; `leaving` when it is about to
    /// close.
    fn quit_standing(&self, leaving: bool) -> QuitStanding {
        if leaving || self.primary.is_some() {
            QuitStanding::Clear
        } else if self.asking_to_close() || self.file.picking() {
            QuitStanding::Asking
        } else if self.file.closing() {
            QuitStanding::Saving
        } else if self.quit_asked {
            QuitStanding::Kept
        } else if !self.loading() && self.view.is_modified() {
            QuitStanding::Unasked
        } else {
            QuitStanding::Clear
        }
    }

    /// Say `message` where the window shows what it is told.
    pub fn state(&mut self, message: impl Into<String>) {
        self.view.say(message.into());
        self.damage.add(self.view.message_area());
        self.owe_reported();
    }

    fn owe(&mut self, repaint: Repaint) {
        self.chrome.owe(repaint);
    }

    /// Lay the window out again for its size, and owe it whole.
    fn relayout(&mut self, client: &mut Client, theme: &Theme, scale: Scale, faces: FacesOf<A>) {
        self.lay_out(client, theme, scale, faces);
        self.owe(Repaint::Whole);
        for pane in &mut self.owned {
            pane.chrome.owe(Repaint::Whole);
        }
    }

    /// Lay the window out again for its size, owing only what the view
    /// reported moved.
    fn lay_out(&mut self, client: &mut Client, theme: &Theme, scale: Scale, faces: FacesOf<A>) {
        let least = self.view.min_size(theme, scale, faces);
        restate_least::<A>(client, &self.chrome.pane, &mut self.least, least);
        let mode = *self.chrome.pane.mode();
        self.view.lay_out(
            mode.width_px,
            mode.height_px,
            theme,
            scale,
            faces,
            &mut self.damage,
        );
        self.owe_reported();
        self.owe_pointer_checks();
    }

    /// Owe the pointer's shape and tip a check over the window and each
    /// window it owns.
    fn owe_pointer_checks(&mut self) {
        self.chrome.pointing.owed = true;
        for pane in &mut self.owned {
            pane.chrome.pointing.owed = true;
        }
    }

    /// The window's own area of the view's drawing.
    fn area(&self) -> Rect {
        let mode = self.chrome.pane.mode();
        Rect::new(0, 0, mode.width_px, mode.height_px)
    }

    /// Show the pointer's shape and declare the tip for what it is over, as
    /// the window and its tool windows now stand, where a check is owed: busy
    /// and with no tip while its document is read in, which takes no input.
    /// A tip is declared on whichever of them the control it explains is in.
    fn settle_pointer(&mut self, client: &mut Client, scale: Scale, theme: &Theme) {
        let owed =
            self.chrome.pointing.owed || self.owned.iter().any(|pane| pane.chrome.pointing.owed);
        if !owed {
            return;
        }
        let wanted = if self.loading() {
            None
        } else {
            self.view.tool_tip(scale, theme)
        };
        let area = self.area();
        if mem::take(&mut self.chrome.pointing.owed) {
            let shape = match self.chrome.pointing.at {
                Some(_) if self.loading() => CursorShape::Busy,
                Some(at) => self.view.cursor(at),
                None => self.chrome.pointing.shape,
            };
            let tip = wanted.filter(|(rect, _)| within(*rect, area));
            self.chrome.settle_pointer(client, shape, tip);
        }
        for pane in &mut self.owned {
            if mem::take(&mut pane.chrome.pointing.owed) {
                let shape = match pane.chrome.pointing.at {
                    Some(at) => self.view.cursor(at),
                    None => pane.chrome.pointing.shape,
                };
                let shown = pane.shown();
                let tip = wanted
                    .filter(|(rect, _)| within(*rect, shown))
                    .map(|(rect, text)| {
                        let left = rect.left() - shown.left();
                        (
                            Rect::new(left, rect.top() - shown.top(), rect.width, rect.height),
                            text,
                        )
                    });
                pane.chrome.settle_pointer(client, shape, tip);
            }
        }
    }

    /// Paint what the window and its tool windows owe through `render`, and
    /// present each, a refusal said once for the one refused.
    ///
    /// Each reported rectangle is painted under its own clip, so a keystroke
    /// rasterises what it changed rather than everything the changes' bounds
    /// span. What the view reported inside a tool window is that window's to
    /// paint, so a change in a palette costs the window nothing.
    fn paint(&mut self, client: &mut Client, mut render: impl FnMut(&mut Surface, &A::View, bool)) {
        if !self.owned.is_empty() {
            for pane in &mut self.owned {
                pane.take_damage(&self.damage);
            }
            self.damage.clip(self.area());
            if self.chrome.owed == Repaint::Reported && self.damage.is_empty() {
                self.chrome.owed = Repaint::Nothing;
            }
        }
        let view = &self.view;
        if self.chrome.owed != Repaint::Nothing {
            self.chrome.title.write(|title| view.write_title(title));
        }
        let focused = self.focus_told;
        let painted = self.chrome.present(client, &mut self.damage, |surface, _| {
            render(surface, view, focused);
        });
        self.chrome.presented(painted, A::NAME, view.name());
        for pane in &mut self.owned {
            let Some((x, y)) = pane.shown().surface_origin() else {
                continue;
            };
            let painted = pane.chrome.present(client, &mut pane.damage, |surface, _| {
                surface.with_origin(x, y, |surface| render(surface, view, focused));
            });
            pane.chrome.presented(painted, A::NAME, view.name());
        }
    }

    /// Tell the view the keyboard came to the window or one of its tool
    /// windows when it did. Its going is told once the round's events are in
    /// ([`Host::settle_focus`]): moving between the window and its own tool
    /// window takes it from one before giving it to the other, and the view
    /// should see neither.
    fn note_focus(&mut self) {
        if self.focused() && !self.focus_told {
            self.tell_focus(true);
        }
    }

    /// The window at `owned` gained or lost the keyboard: a tool window's is
    /// this window's, a dialog's is the view's to show.
    fn owned_focus(&mut self, owned: usize, focused: bool) {
        let Some(pane) = self.owned.get_mut(owned) else {
            return;
        };
        pane.chrome.focused = focused;
        match pane.kind {
            Owned::Tool(_) => self.note_focus(),
            // Its look follows the keyboard, and it is small: repainted whole.
            Owned::Dialog(_) => {
                self.view.dialog_focus_changed(focused, &mut self.damage);
                pane.chrome.owe(Repaint::Whole);
                self.owe_reported();
            }
        }
    }

    /// Tell the view whether it has the keyboard.
    fn tell_focus(&mut self, focused: bool) {
        self.focus_told = focused;
        self.view.focus_changed(focused, &mut self.damage);
        self.owe_reported();
    }
}

/// The headline of an alert saying a window could not be opened.
const NO_NEW_WINDOW: &str = "No new window could be opened";

/// Why a window could not be opened when it could not be drawn.
const NO_SURFACE: &str = "There is not enough memory to draw another window.";

/// Whether `inner` lies wholly inside `outer`.
fn within(inner: Rect, outer: Rect) -> bool {
    inner.intersection(&outer) == inner
}

/// Everything a document application's loop owns.
pub struct Host<A: DocumentApp> {
    /// The window channel.
    pub client: Client,
    /// The open windows, oldest first.
    pub windows: Vec<DocWindow<A>>,
    /// The application's own windows, oldest first.
    pub app_windows: Vec<AppWindow<A>>,
    /// The desktop the windows are on.
    pub desktop: Desktop,
    /// The themes; every window is drawn under the active one.
    pub themes: ThemeRegistry,
    faces: FacesOf<A>,
    /// The application's own state.
    pub app: A,
    queue: Arc<Queue<A>>,
    fonts: Arc<FontWorker>,
    /// What deferred font fetches had brought when the windows were last
    /// laid out.
    text_drawn: u64,
    /// A job ran on the loop for want of a worker, so its answer is already
    /// waiting to be taken in.
    answered: bool,
    /// Saves asked for and not yet landed, whichever window asked: the
    /// process does not end under one.
    saves: usize,
    /// Jobs closed windows left behind with no room kept for them, the room
    /// to keep having been refused: each lands with none to give back.
    unroomed: usize,
    event_endpoint: u64,
    /// Quit was chosen: the process ends once every window has closed and no
    /// save is left to land.
    quitting: bool,
    /// The documents asked to be shown in their folders and not yet answered,
    /// by the reveal's token, named for the alert a refusal raises.
    reveals: Vec<(u64, String)>,
    /// Files offered over the application's windows, and the read of their
    /// drag's media type their answers wait on.
    offers: FileOffers,
}

impl<A: DocumentApp> Host<A> {
    /// The index of the window the session calls `id`.
    #[must_use]
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.windows.iter().position(|window| window.id() == id)
    }

    /// The window `stamp` names while it still shows the document that asked.
    #[must_use]
    pub fn showing(&self, stamp: Stamp) -> Option<usize> {
        self.index_of(stamp.window)
            .filter(|&index| self.windows[index].epoch == stamp.epoch)
    }

    /// State `reason` on `stderr` under the application's name.
    pub fn report(&self, reason: impl Display) {
        report(A::NAME, reason);
    }

    /// Ask what the clipboard holds for window `index`, handed to
    /// [`DocumentApp::pasted`] once it lands.
    pub fn paste(&mut self, index: usize) {
        let id = self.windows[index].id();
        if let Err(err) = self.client.ask_clipboard(id) {
            self.windows[index].state(format!("Could not paste: {err}"));
        }
    }

    /// A job was handed to one of the application's own workers; `answered`
    /// says it ran here for want of one, so its answer is already waiting.
    pub fn note_answered(&mut self, answered: bool) {
        self.answered |= answered;
    }

    /// Hand `work` for window `index`'s document to the queue, answering
    /// whether it was taken. What it supersedes of the window's is withdrawn
    /// first.
    pub fn queue(&mut self, index: usize, work: A::Work) -> bool {
        let window = &self.windows[index];
        let (id, epoch) = (window.id(), window.epoch);
        let mut superseded = 0;
        self.queue.retain_waiting(|job| {
            let gone = job.as_ref().is_some_and(|job| {
                job.window == id
                    && matches!(&job.work, Queued::Own { work: waiting, .. } if A::supersedes(&work, waiting))
            });
            superseded += usize::from(gone);
            !gone
        });
        let window = &mut self.windows[index];
        window.own_held = window.own_held.saturating_sub(superseded);
        // The queue's bound is shared, so a window past its own rooms would
        // be borrowing another's, and the rooms a close gives up would be
        // some other window's work.
        let held = window.own_held + usize::from(window.file.saving());
        if held >= A::JOBS_PER_WINDOW {
            return false;
        }
        let taken = self.submit(id, Queued::Own { epoch, work });
        self.windows[index].own_held += usize::from(taken);
        taken
    }

    /// Hand `queued` for window `window` to the queue, answering whether it
    /// was taken. A save is never turned away while the memory for it can be
    /// had: the queue grows a room for it, given back when it lands.
    fn submit(&mut self, window: u64, queued: QueuedOf<A>) -> bool {
        let save = matches!(queued, Queued::Save(_));
        let mut job = Some(Job {
            window,
            work: queued,
        });
        loop {
            let mut refused = match self.queue.submit(job) {
                Ok(answered) => {
                    self.answered |= answered;
                    self.saves += usize::from(save);
                    return true;
                }
                Err(refused) => refused,
            };
            let Some(Job {
                work: Queued::Save(save),
                ..
            }) = refused.as_mut()
            else {
                return false;
            };
            if save.grew {
                self.queue.shrink(1);
                return false;
            }
            if self.queue.grow(1).is_err() {
                return false;
            }
            save.grew = true;
            job = refused;
        }
    }

    /// Say `message` on `stderr`, and in the window with the keyboard, else
    /// the newest, when one is open.
    pub fn notify(&mut self, message: String) {
        self.report(&message);
        let index = self
            .windows
            .iter()
            .position(DocWindow::focused)
            .or_else(|| self.windows.len().checked_sub(1));
        if let Some(index) = index {
            self.windows[index].state(message);
        }
    }

    /// State, in an error alert holding window `owner` when it is some and
    /// on `stderr`, that what `headline` names was not done because of
    /// `detail`.
    fn alert(&mut self, owner: Option<usize>, headline: &str, detail: &str) {
        let window = owner
            .and_then(|index| self.windows.get(index))
            .map(DocWindow::id);
        let _ = app::refuse(
            &mut self.client,
            (A::NAME, self.event_endpoint),
            window,
            headline,
            detail,
        );
    }

    /// Ask whether to save window `index`'s changes before it closes, unless
    /// that is being asked already. A question the desktop cannot put up
    /// keeps the window open, saying so: closing it would lose the changes.
    fn ask_to_close(&mut self, index: usize) {
        if self.windows[index].closing.is_some() {
            return;
        }
        let question = save_changes_alert(self.windows[index].view.name());
        let id = self.windows[index].id();
        let asked = question.ok_or(Errno::OutOfRange).and_then(|question| {
            self.client
                .open_alert(self.event_endpoint, Some(id), &question)
        });
        match asked {
            Ok(alert) => self.windows[index].closing = Some(alert),
            Err(err) => {
                self.quitting = false;
                let window = &mut self.windows[index];
                window.state(format!(
                    "The window stays open: whether to save could not be asked ({err})"
                ));
                self.report(format_args!(
                    "{}: whether to save could not be asked ({err})",
                    self.windows[index].view.name()
                ));
            }
        }
    }

    /// The answer to alert `alert_id`: the one a window asked before closing
    /// is acted on, and an error alert's needs nothing.
    fn alert_answered(&mut self, alert_id: u64, outcome: AlertOutcome) {
        let Some(index) = self
            .windows
            .iter()
            .position(|window| window.closing == Some(alert_id))
        else {
            return;
        };
        self.windows[index].closing = None;
        let id = self.windows[index].id();
        match CloseAnswer::of(outcome) {
            CloseAnswer::Save => {
                let window = &mut self.windows[index];
                let outcome = window.view.save_then_close(&mut window.damage);
                self.apply(index, outcome);
                self.carry_quit_on(None);
            }
            CloseAnswer::Discard => {
                // The next question goes up before this window comes down,
                // while the keyboard the user answered with is still its.
                self.carry_quit_on(Some(id));
                if let Some(index) = self.index_of(id) {
                    self.close_window(index);
                }
            }
            CloseAnswer::Keep => {
                self.quitting = false;
                self.windows[index].reveal_on_close = false;
            }
        }
    }

    /// Open a window on a new document, or say why not.
    pub fn new_window(&mut self) {
        match A::untitled(self) {
            Ok(view) => {
                let _ = self.open_view(view);
            }
            Err(why) => self.alert(None, NO_NEW_WINDOW, &why),
        }
    }

    /// Open a window showing `view`, answering its index. A refusal is put
    /// up as an alert and answers `None`: the application carries on with the
    /// windows it has, and is still on the icon bar.
    pub fn open_view(&mut self, view: A::View) -> Option<usize> {
        self.open_window(view, Reading::Done)
            .map_err(|why| self.alert(None, NO_NEW_WINDOW, &why))
            .ok()
    }

    /// Show `view` in window `index` in place of what it showed, its document
    /// from `handle`, or from nowhere yet — in a window of its own when the
    /// document shown is not [`replaceable`](DocWindow::replaceable).
    pub fn show(&mut self, index: usize, view: A::View, handle: Option<Arc<Handle>>) {
        if !self.windows[index].replaceable() {
            if let Some(index) = self.open_view(view) {
                self.windows[index].file.opened(handle);
            }
            return;
        }
        let Some(index) = self.moved_on(index) else {
            return;
        };
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        // A window opened for this document alone takes the size it asks.
        let faces = A::faces(theme, Scale::ONE);
        let fit = (self.windows[index].reading == Reading::Alone)
            .then(|| view.open_size(theme, faces))
            .flatten()
            .map(|(width, height)| {
                let (width, height) = self.desktop.window_size(width, height);
                app::mode_for(width, height)
            });
        let window = &mut self.windows[index];
        window.view = view;
        window.file.opened(handle);
        window.reading = Reading::Done;
        window.withheld.end();
        // The least first, which the size asked for may be below.
        let least = window.view.min_size(theme, scale, self.faces);
        restate_least::<A>(
            &mut self.client,
            &window.chrome.pane,
            &mut window.least,
            least,
        );
        if let Some(mode) = fit {
            let _ =
                window
                    .chrome
                    .pane
                    .resize_with(&mut self.client, &mode, &mut window.chrome.surface);
        }
        window.relayout(&mut self.client, theme, scale, self.faces);
    }

    /// The document `name` could not be read into window `index`, which an
    /// error alert says. A window opened for it alone goes, since it shows
    /// nothing; one that showed a document before stays — untitled, where a
    /// new document can be made, since an empty window under the file's name
    /// would read as an empty file — and a document that is not
    /// [`replaceable`](DocWindow::replaceable) stays as it was.
    pub fn not_opened(&mut self, index: usize, name: &str, why: &dyn Display) {
        let headline = refusal_headline(name, "opened");
        let detail = format!("{why}");
        self.give_up(index, Some((&headline, &detail)));
    }

    /// The document window `index` was loading is not to be opened after
    /// all, the user having turned it down: a window opened for it alone
    /// closes, and one it was to replace holds a fresh document again.
    pub fn not_wanted(&mut self, index: usize) {
        self.give_up(index, None);
    }

    /// Give up the load of window `index`, saying `alert` where there is
    /// something to say.
    fn give_up(&mut self, index: usize, alert: Option<(&str, &str)>) {
        if self.windows[index].reading == Reading::Alone {
            self.close_window(index);
            if let Some((headline, detail)) = alert {
                self.alert(None, headline, detail);
            }
            return;
        }
        if let Some((headline, detail)) = alert {
            self.alert(Some(index), headline, detail);
        }
        if !self.windows[index].replaceable() {
            return;
        }
        let Some(index) = self.moved_on(index) else {
            return;
        };
        let fresh = A::untitled(self);
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.windows[index];
        if let Ok(view) = fresh {
            window.view = view;
        }
        window.file.opened(None);
        window.reading = Reading::Done;
        window.relayout(&mut self.client, theme, scale, self.faces);
    }

    /// Paint what the engine reported for window `index`, and carry out what
    /// it asked for.
    pub fn apply(&mut self, index: usize, outcome: ViewOutcome<A::View>) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.windows[index];
        window.owe_reported();
        // What the pointer is over may have changed shape without moving: a
        // job's end, a question raised from a key.
        window.owe_pointer_checks();
        match outcome.relayout {
            Relayout::None => {}
            Relayout::Reported => window.lay_out(&mut self.client, theme, scale, self.faces),
            Relayout::Whole => window.relayout(&mut self.client, theme, scale, self.faces),
        }
        let id = window.id();
        A::driven(self, index);
        // What the application did may have closed windows, this one among
        // them.
        let Some(index) = self.index_of(id) else {
            return;
        };
        if let Some(request) = outcome.request {
            self.carry_out(index, request);
        }
    }

    /// Open a window showing `view`, its document `reading` as said,
    /// answering its index, or why it could not open.
    fn open_window(&mut self, view: A::View, reading: Reading) -> Result<usize, String> {
        let theme = self.themes.active();
        let scale = self.desktop.scale();
        let (width, height) = view
            .open_size(theme, A::faces(theme, Scale::ONE))
            .unwrap_or(A::WINDOW_SIZE);
        let (width, height) = self.desktop.window_size(width, height);
        let mode = app::mode_for(width, height);
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            return Err(String::from(NO_SURFACE));
        };
        if self.queue.grow(A::JOBS_PER_WINDOW).is_err() {
            return Err(String::from(
                "There is not enough memory for another window\u{2019}s work.",
            ));
        }
        if let Err(why) = self.app.reserve_window() {
            self.queue.shrink(A::JOBS_PER_WINDOW);
            return Err(String::from(why));
        }
        let least = view.min_size(theme, scale, self.faces);
        let mut title = String::new();
        view.write_title(&mut title);
        let opened = open_pane(&mut self.client, self.event_endpoint, &mode, &title, least);
        let pane = match opened {
            Ok(pane) => pane,
            Err(why) => {
                self.give_back_window();
                return Err(why);
            }
        };
        let mut window: DocWindow<A> = DocWindow {
            view,
            damage: A::damage_sink(),
            extra: A::Extra::default(),
            chrome: Chrome::new(pane, surface, title),
            file: DocumentFile::new(),
            reading,
            withheld: if reading == Reading::Alone {
                Withheld::since(tairix_rt::clock_get())
            } else {
                Withheld::default()
            },
            least,
            epoch: 0,
            own_held: 0,
            menu: None,
            owned: Vec::new(),
            focus_told: true,
            closing: None,
            quit_asked: false,
            reveal_on_close: false,
            primary: None,
            chosen: None,
            entering: None,
        };
        window.view.lay_out(
            mode.width_px,
            mode.height_px,
            theme,
            scale,
            self.faces,
            &mut window.damage,
        );
        self.windows.push(window);
        Ok(self.windows.len() - 1)
    }

    /// Open a further window onto window `index`'s document, showing `view`,
    /// answering its index: a member window, whose saves are its primary's —
    /// `index`'s, or `index`'s own primary's — which is never asked whether to
    /// save, and which closes with its primary. A refusal is put up as an
    /// alert over the primary and answers `None`.
    pub fn open_member(&mut self, index: usize, view: A::View) -> Option<usize> {
        let window = &self.windows[index];
        let primary = window.primary.unwrap_or_else(|| window.id());
        match self.open_window(view, Reading::Done) {
            Ok(member) => {
                self.windows[member].primary = Some(primary);
                Some(member)
            }
            Err(why) => {
                self.alert(Some(index), NO_NEW_WINDOW, &why);
                None
            }
        }
    }

    /// The index of the window whose document member window `index` shows,
    /// or `None` for a window that is no member.
    #[must_use]
    pub fn primary_of(&self, index: usize) -> Option<usize> {
        self.windows[index]
            .primary
            .and_then(|primary| self.index_of(primary))
    }

    /// The indices of the member windows onto window `index`'s document.
    pub fn members(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let primary = self.windows[index].id();
        self.windows
            .iter()
            .enumerate()
            .filter_map(move |(at, window)| (window.primary == Some(primary)).then_some(at))
    }

    /// Bring document window `index` forward and give it the keyboard, while
    /// one of this application's windows holds it; a refusal is said.
    pub fn raise_window(&mut self, index: usize) {
        let id = self.windows[index].id();
        if let Err(err) = self.client.activate_window(id) {
            self.report(format!("a window could not be brought forward ({err})"));
        }
    }

    /// Tell the member windows onto window `index`'s document the name and
    /// access it took.
    fn tell_members(&mut self, index: usize, renamed: Option<&str>, gained: Option<Access>) {
        let members: Vec<usize> = self.members(index).collect();
        for member in members {
            let window = &mut self.windows[member];
            if let Some(name) = renamed {
                window.view.rename(String::from(name));
            }
            if let Some(access) = gained {
                window.view.set_access(access);
            }
            window.owe(Repaint::Whole);
        }
    }

    /// Close the member windows onto window `id`'s document: it closed, or
    /// shows another. A member opens after its primary and windows keep their
    /// order, so this never moves the primary's own index.
    fn close_members(&mut self, id: u64) {
        let closing: Vec<u64> = members_of(
            id,
            self.windows
                .iter()
                .map(|window| (window.id(), window.primary)),
        )
        .collect();
        for member in closing {
            if let Some(index) = self.index_of(member) {
                self.close_window(index);
            }
        }
    }

    /// Open a window of the application's own showing `view`, answering its
    /// index: one asking on behalf of document window `owner` closes with it,
    /// and as it moves on to another document. A refusal is put up as an
    /// alert and answers `None`.
    pub fn open_app_window(&mut self, view: A::AppView, owner: Option<usize>) -> Option<usize> {
        let owner = owner.map(|index| self.windows[index].id());
        let theme = self.themes.active();
        let scale = self.desktop.scale();
        let (width, height) = view.size();
        let (width, height) = self.desktop.window_size(width, height);
        let mode = app::mode_for(width, height);
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            self.alert(None, NO_NEW_WINDOW, NO_SURFACE);
            return None;
        };
        let least = view.min_size(theme, scale, self.faces);
        let title = String::from(view.title());
        let opened = open_pane(&mut self.client, self.event_endpoint, &mode, &title, least);
        let pane = match opened {
            Ok(pane) => pane,
            Err(why) => {
                self.alert(None, NO_NEW_WINDOW, &why);
                return None;
            }
        };
        let mut window: AppWindow<A> = AppWindow {
            view,
            damage: A::damage_sink(),
            chrome: Chrome::new(pane, surface, title),
            least,
            owner,
        };
        window.view.lay_out(
            mode.width_px,
            mode.height_px,
            theme,
            scale,
            self.faces,
            &mut window.damage,
        );
        if self.app_windows.try_reserve(1).is_err() {
            let _ = window.chrome.pane.close(&mut self.client);
            self.alert(
                None,
                NO_NEW_WINDOW,
                "There is not enough memory to hold another window.",
            );
            return None;
        }
        self.app_windows.push(window);
        Some(self.app_windows.len() - 1)
    }

    /// The index of the application window the session calls `id`.
    #[must_use]
    pub fn app_index_of(&self, id: u64) -> Option<usize> {
        self.app_windows.iter().position(|window| window.id() == id)
    }

    /// Bring application window `index` forward and give it the keyboard,
    /// under the activation an icon-bar choice grants; a refusal is said.
    pub fn raise_app_window(&mut self, index: usize) {
        let id = self.app_windows[index].id();
        if let Err(err) = self.client.activate_window(id) {
            self.report(format!("a window could not be brought forward ({err})"));
        }
    }

    /// Close application window `index`.
    pub fn close_app_window(&mut self, index: usize) {
        let id = self.app_windows[index].id();
        // What its closing sets off may close its owner, and what that owns:
        // it is told once, and found again after.
        self.app_windows[index].owner = None;
        A::app_closing(self, index);
        if let Some(index) = self.app_index_of(id) {
            let window = self.app_windows.remove(index);
            let _ = window.chrome.pane.close(&mut self.client);
        }
    }

    /// Close the windows of the application's own asking on behalf of
    /// document window `id`: it closed, or shows another document.
    fn close_asked_for(&mut self, id: u64) {
        let closing: Vec<u64> = members_of(
            id,
            self.app_windows
                .iter()
                .map(|window| (window.id(), window.owner)),
        )
        .collect();
        for asking in closing {
            if let Some(index) = self.app_index_of(asking) {
                self.close_app_window(index);
            }
        }
    }

    /// Paint what application window `index`'s view reported, and carry out
    /// what it asked for.
    pub fn apply_app(&mut self, index: usize, outcome: Outcome<AppRequest<AppOwnOf<A>>>) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.app_windows[index];
        window.owe_reported();
        window.chrome.pointing.owed = true;
        match outcome.relayout {
            Relayout::None => {}
            Relayout::Reported => {
                window.lay_out(&mut self.client, (theme, scale, self.faces), false);
            }
            Relayout::Whole => window.lay_out(&mut self.client, (theme, scale, self.faces), true),
        }
        match outcome.request {
            None => {}
            Some(AppRequest::Close) => self.close_app_window(index),
            Some(AppRequest::Own(own)) => A::app_request(self, index, own),
        }
    }

    /// Give back the room a window took.
    fn give_back_window(&mut self) {
        self.queue.shrink(A::JOBS_PER_WINDOW);
        self.app.release_window();
    }

    /// Window `index` moves on from the document it shows: what that
    /// document asked for and is still waiting is withdrawn, and whatever of
    /// it answers later is told apart. Answers where the window is now, if it
    /// is still open: what closes with its document may set off the
    /// application's own closing of windows.
    fn moved_on(&mut self, index: usize) -> Option<usize> {
        let id = self.windows[index].id();
        let withdrawn = self.withdraw(id);
        let window = &mut self.windows[index];
        window.epoch = window.epoch.wrapping_add(1);
        window.own_held = window.own_held.saturating_sub(withdrawn);
        self.close_members(id);
        self.close_asked_for(id);
        self.index_of(id)
    }

    /// Withdraw what window `id`'s document has waiting, on the queue and on
    /// the application's own workers — all but its saves, and the work the
    /// application still wants once the document has gone — answering how
    /// many of its own jobs on the queue went.
    fn withdraw(&mut self, id: u64) -> usize {
        let mut withdrawn = 0;
        self.queue.retain_waiting(|job| {
            let Some(job) = job.as_ref().filter(|job| job.window == id) else {
                return true;
            };
            let keep = match &job.work {
                Queued::Save(_) => true,
                Queued::Own { work, .. } => A::outlives_document(work),
            };
            withdrawn += usize::from(!keep);
            keep
        });
        self.app.withdraw(id);
        withdrawn
    }

    /// A job a closed window left behind landed: give back the room kept for
    /// it, when one was.
    fn give_back_room(&mut self) {
        match self.unroomed.checked_sub(1) {
            Some(left) => self.unroomed = left,
            None => self.queue.shrink(1),
        }
    }

    /// Read `handle`'s document, called `name`, into a window: `into` when
    /// the user opened it from a pristine window, a new one otherwise. The
    /// window shows that it is opening and takes no input until it lands.
    fn open_document(&mut self, handle: Handle, name: String, writable: bool, into: Option<usize>) {
        let access = if writable {
            Access::Writable
        } else {
            Access::ReadOnly
        };
        let mut view = match A::placeholder(self, &name, access) {
            Ok(view) => view,
            Err(why) => {
                self.alert(None, &refusal_headline(&name, "opened"), &why);
                return;
            }
        };
        view.say(opening(&name));
        let index = match into {
            Some(index) => {
                let Some(index) = self.moved_on(index) else {
                    return;
                };
                let (theme, scale) = (self.themes.active(), self.desktop.scale());
                let window = &mut self.windows[index];
                window.view = view;
                window.reading = Reading::Over;
                window.relayout(&mut self.client, theme, scale, self.faces);
                index
            }
            None => match self.open_window(view, Reading::Alone) {
                Ok(index) => index,
                Err(why) => {
                    self.alert(None, &refusal_headline(&name, "opened"), &why);
                    return;
                }
            },
        };
        A::load(self, index, Arc::new(handle), name);
    }

    /// Close window `index`, its menu and any pick going with it. The work it
    /// asked for goes with it — all but its saves, which still land, the
    /// saves asked for behind the one in flight, written now, and the work
    /// the application still wants once the document has gone.
    fn close_window(&mut self, index: usize) {
        // While the window is still the user's, so the file manager may be
        // brought forward over it.
        if self.windows[index].reveal_on_close {
            self.reveal_document(index);
        }
        let mut window = self.windows.remove(index);
        let id = window.id();
        let step = self.offers.closed(id);
        self.take_offer_step(step);
        let withdrawn = self.withdraw(id);
        self.app.release_window();
        // On the queue already: the save in flight, and the window's own work
        // still wanted.
        let kept = usize::from(window.file.saving()) + window.own_held.saturating_sub(withdrawn);
        let flushed = window.file.close(&window.view);
        let held = kept + flushed.len();
        let share = A::JOBS_PER_WINDOW;
        // Each job left behind keeps one room until it lands; the rest of the
        // window's share is given up now.
        let room = if held > share && self.queue.grow(held - share).is_err() {
            share
        } else {
            self.queue.shrink(share.saturating_sub(held));
            held
        };
        self.unroomed += kept.saturating_sub(room);
        let name = String::from(window.view.name());
        for (queued, job) in flushed.into_iter().enumerate() {
            let fits = kept + queued < room;
            let target = job.rename.clone().unwrap_or_else(|| name.clone());
            if !(fits && self.submit(id, Queued::Save(Save::new(job, target.clone())))) {
                if fits {
                    self.queue.shrink(1);
                }
                self.report(format!(
                    "{} could not be saved: there is no room for the save",
                    tairix_util::utf8::shown(&target)
                ));
            }
        }
        for pane in window.owned.drain(..) {
            let _ = pane.chrome.pane.close(&mut self.client);
        }
        let _ = window.chrome.pane.close(&mut self.client);
        self.close_members(id);
        self.close_asked_for(id);
    }

    /// Have the desktop show the user, in the file manager, where window
    /// `index`'s document lives. The window learns nothing of where that is:
    /// its descriptor tells the desktop. A refusal is said in an alert, since
    /// the window it would have shown is going.
    fn reveal_document(&mut self, index: usize) {
        let held = self.primary_of(index).unwrap_or(index);
        let (window, file) = (&self.windows[index], &self.windows[held].file);
        let Some(handle) = file.handle() else {
            return;
        };
        let (id, fd) = (window.id(), handle.fd());
        let name = String::from(window.view.name());
        let shown = self
            .client
            .session()
            .ok_or(Errno::NotSupported)
            .and_then(|session| tairix_rt::fd_grant_location(fd, session))
            .and_then(|location| self.client.reveal_document(id, location));
        match shown {
            Ok(token) if self.reveals.try_reserve(1).is_ok() => self.reveals.push((token, name)),
            // With no room to name it the answer is not said: the reveal is
            // incidental to the close under way.
            Ok(_) => {}
            Err(err) => self.not_revealed(&name, err),
        }
    }

    /// The desktop answered the reveal `token` named: a refusal is said in an
    /// alert, since the window it would have shown has gone.
    fn revealed(&mut self, token: u64, shown: Result<(), Errno>) {
        let Some(at) = self.reveals.iter().position(|(asked, _)| *asked == token) else {
            return;
        };
        let (_, name) = self.reveals.swap_remove(at);
        if let Err(err) = shown {
            self.not_revealed(&name, err);
        }
    }

    /// Say that the document `name` was not shown in its folder, for `err`.
    fn not_revealed(&mut self, name: &str, err: Errno) {
        self.report(format_args!(
            "{} was not shown in its folder: {err}",
            tairix_util::utf8::shown(name)
        ));
        self.alert(
            None,
            &refusal_headline(name, "shown in its folder"),
            reveal_refusal(err),
        );
    }

    /// Carry out one request of window `index`: a member's request about its
    /// document is its primary's.
    fn carry_out(&mut self, index: usize, request: RequestOf<A>) {
        if self.windows[index].is_member() {
            let primary = self.primary_of(index);
            match (route(&request), primary) {
                (Route::Member, _) => {}
                (Route::Close, _) | (Route::Primary | Route::SaveThenClose, None) => {
                    self.close_window(index);
                    return;
                }
                (Route::Primary, Some(primary)) => {
                    self.carry_out(primary, request);
                    return;
                }
                (Route::SaveThenClose, Some(primary)) => {
                    let member = self.windows[index].id();
                    self.save(primary, None, false);
                    if let Some(index) = self.index_of(member) {
                        self.close_window(index);
                    }
                    return;
                }
            }
        }
        match request {
            Request::Save => self.save(index, None, false),
            Request::SaveThenClose => self.save(index, None, true),
            Request::SaveAs => self.ask_how(index, false),
            Request::SaveWhere { then_close } => self.ask_where(index, then_close),
            Request::Open => self.ask_pick(index, &PickPurpose::Open, PickFor::Open),
            Request::Import => self.ask_pick(index, &PickPurpose::Import, PickFor::Import),
            Request::SaveCopy => self.ask_copy_where(index),
            Request::Close => self.close_window(index),
            Request::AskToClose => self.ask_to_close(index),
            Request::Menu { kind, anchor } => self.open_menu(index, kind, anchor),
            Request::Own(own) => A::request(self, index, own),
        }
    }

    /// Save window `index` — through `save_as` for a Save As, else where its
    /// document came from, else asking where — closing it once saved when
    /// `then_close`.
    fn save(&mut self, index: usize, save_as: Option<(Arc<Handle>, String)>, then_close: bool) {
        let window = &mut self.windows[index];
        // A save is refused before any file is touched when the name it would
        // be written under cannot hold the document; one with nowhere to go
        // yet asks where instead, and the chooser offers a name that can.
        let destination = match &save_as {
            Some((_, name)) => Some((name.as_str(), SaveTarget::Another)),
            None => window
                .file
                .plain_destination()
                .map(|name| (name, SaveTarget::Another))
                .or_else(|| {
                    window
                        .file
                        .writes_back(&window.view)
                        .then(|| (window.view.name(), SaveTarget::Own))
                }),
        };
        let refused = destination.and_then(|(name, target)| window.view.refuse_save(name, target));
        if let Some(refusal) = refused {
            self.quitting = false;
            let headline = refusal_headline(window.view.name(), "saved");
            self.alert(Some(index), &headline, &refusal);
            return;
        }
        let step = window.file.save(&mut window.view, save_as, then_close);
        self.carry_out_save(index, step);
    }

    /// Carry out what asking window `index` to save came to.
    fn carry_out_save(&mut self, index: usize, step: SaveStep<Handle, SnapshotOf<A>>) {
        let window = &mut self.windows[index];
        match step {
            SaveStep::Write(job) => {
                window.state("Saving\u{2026}");
                let id = window.id();
                let name = job
                    .rename
                    .clone()
                    .unwrap_or_else(|| String::from(window.view.name()));
                let (target, generation, rename) =
                    (Arc::clone(&job.target), job.generation, job.rename.clone());
                if !self.submit(id, Queued::Save(Save::new(job, name.clone()))) {
                    // Refused as a save the file never saw, so the window says
                    // so and whatever waited on it is given its answer.
                    let refused = Saved {
                        target,
                        generation,
                        rename,
                        name,
                        grew: false,
                        result: Err("there is no room for the save"),
                    };
                    self.saved(index, refused);
                }
            }
            SaveStep::Queued => window.state("Saving\u{2026}"),
            SaveStep::AskWhere { then_close } => self.ask_how(index, then_close),
            SaveStep::NoMemory => {
                let headline = refusal_headline(window.view.name(), "saved");
                self.alert(
                    Some(index),
                    &headline,
                    "There is not enough memory to save it. Close something and save again.",
                );
            }
        }
    }

    /// A save of window `index` landed, or was refused.
    fn saved<E: Display>(&mut self, index: usize, saved: Saved<E>) {
        let Saved {
            target,
            generation,
            rename,
            name,
            result,
            ..
        } = saved;
        if let Err(err) = &result {
            self.alert(
                Some(index),
                &refusal_headline(&name, "saved"),
                &format!("{err}"),
            );
        }
        let window = &mut self.windows[index];
        let named = rename.is_some().then(|| String::from(window.view.name()));
        let access = window.view.access();
        let landed = window.file.saved(
            &mut window.view,
            target,
            generation,
            rename,
            result.as_ref().map(Option::as_deref),
        );
        window.damage.add(window.view.message_area());
        window.owe_reported();
        // A Save As that landed renamed the document, and may have made it
        // writable: its members show it by that name and as that.
        let renamed = named
            .filter(|before| before != window.view.name())
            .map(|_| String::from(window.view.name()));
        let gained = (window.view.access() != access).then(|| window.view.access());
        if renamed.is_some() || gained.is_some() {
            self.tell_members(index, renamed.as_deref(), gained);
        }
        // What was to close stays open to say why, and a quit waiting on it
        // is given up.
        self.quitting &= !landed.close_abandoned;
        if let Some(next) = landed.next {
            self.carry_out_save(index, next);
        }
        if landed.close {
            self.close_window(index);
        }
    }

    /// Ask window `index` how its document is to be saved, where it asks,
    /// else go on to ask where.
    fn ask_how(&mut self, index: usize, then_close: bool) {
        let window = &mut self.windows[index];
        match window.view.ask_how(then_close, &mut window.damage) {
            Some(outcome) => self.apply(index, outcome),
            None => self.ask_where(index, then_close),
        }
    }

    /// Ask the picker where to save window `index`, offering its name, or
    /// for a new document or one whose name cannot be written, a name in the
    /// form it would be written — in the formats it can be written in, or
    /// saying why it can be written in none.
    fn ask_where(&mut self, index: usize, then_close: bool) {
        let window = &self.windows[index];
        let formats = match window.view.save_formats() {
            Ok(formats) => formats,
            Err(why) => {
                self.quitting = false;
                let headline = refusal_headline(window.view.name(), "saved");
                self.alert(Some(index), &headline, &why);
                return;
            }
        };
        let view = &window.view;
        let offered = if view.access() == Access::Untitled {
            offered_name(UNTITLED, view.offered_extension())
        } else if view.refuse_save(view.name(), SaveTarget::Another).is_some() {
            offered_name(stem(view.name()), view.offered_extension())
        } else {
            String::from(view.name())
        };
        let Ok(suggested) = DocumentName::new(&offered).or_else(|_| DocumentName::new(UNTITLED))
        else {
            return;
        };
        self.ask_pick(
            index,
            &PickPurpose::Save { suggested, formats },
            PickFor::SaveAs { then_close },
        );
    }

    /// Ask the session's picker where window `index` writes a copy, in the
    /// formats its view offers, or say why it writes none.
    fn ask_copy_where(&mut self, index: usize) {
        let window = &self.windows[index];
        let formats = match window.view.copy_formats() {
            Ok(formats) => formats,
            Err(why) => {
                let headline = refusal_headline(window.view.name(), "copied");
                self.alert(Some(index), &headline, &why);
                return;
            }
        };
        let offered = offered_name(stem(window.view.name()), window.view.copy_extension());
        let Ok(suggested) = DocumentName::new(&offered).or_else(|_| DocumentName::new(UNTITLED))
        else {
            return;
        };
        self.ask_pick(
            index,
            &PickPurpose::Save { suggested, formats },
            PickFor::Copy,
        );
    }

    /// Ask the session's picker for `purpose` on window `index`.
    fn ask_pick(&mut self, index: usize, purpose: &PickPurpose, pick: PickFor) {
        let window = &mut self.windows[index];
        if window.file.picking() {
            window.state("A file chooser is already open for this window");
            return;
        }
        match self.client.pick_file(window.id(), *purpose) {
            Ok(()) => {
                let _ = window.file.start_pick(pick);
            }
            Err(err) => {
                self.quitting = false;
                self.alert(
                    Some(index),
                    "No file chooser could be opened",
                    &format!("The desktop would not offer one ({err})."),
                );
            }
        }
    }

    /// The user chose a file in the picker for window `index`.
    fn picked(&mut self, index: usize, window_id: u64, grant: u64, writable: bool) {
        let window = &mut self.windows[index];
        let Some(pick) = window.file.end_pick() else {
            release(grant);
            return;
        };
        let file = match Handle::from_delegation(grant) {
            Ok(file) => file,
            Err(raw) => {
                self.quitting = false;
                self.alert(
                    Some(index),
                    "The chosen file could not be used",
                    &format!("It could not be taken over ({}).", Errno::from_syscall(raw)),
                );
                return;
            }
        };
        let window = &mut self.windows[index];
        // A file chosen again before the first's name landed replaces it.
        window.chosen = Some(Chosen {
            pick,
            file,
            writable,
        });
        if let Err(err) = self.client.ask_picked_name(window_id) {
            self.report(format!(
                "the chosen file's name could not be asked for ({err})"
            ));
            self.name_chosen(index, None);
        }
    }

    /// The name of the file window `index`'s picker chose has landed: `None`,
    /// one the session no longer holds, leaves the document named as it was,
    /// or untitled.
    fn name_chosen(&mut self, index: usize, name: Option<String>) {
        let Some(Chosen {
            pick,
            file,
            writable,
        }) = self.windows[index].chosen.take()
        else {
            return;
        };
        match pick {
            PickFor::Open => {
                let into = self.windows[index].pristine().then_some(index);
                let name = name.unwrap_or_else(|| String::from(UNTITLED));
                self.open_document(file, name, writable, into);
            }
            PickFor::SaveAs { then_close } => {
                let name = name.unwrap_or_else(|| String::from(self.windows[index].view.name()));
                self.save(index, Some((Arc::new(file), name)), then_close);
            }
            PickFor::Copy => {
                let name = name.unwrap_or_else(|| String::from(UNTITLED));
                A::copy_chosen(self, index, Arc::new(file), name);
            }
            PickFor::Import => {
                let name = name.unwrap_or_else(|| String::from(UNTITLED));
                A::import_chosen(self, index, file, name, None);
            }
        }
    }

    /// Ask the session to open window `index`'s `kind` menu at `anchor` in its
    /// view's drawing: over the tool window showing that part of it, if one
    /// does, else over the window.
    fn open_menu(&mut self, index: usize, kind: MenuKindOf<A>, anchor: Rect) {
        let window = &mut self.windows[index];
        let tools = window
            .owned
            .iter()
            .filter(|pane| pane.tool().is_some())
            .map(|pane| (pane.chrome.id(), pane.shown()));
        let (tool, anchor) = super::menu_over(anchor, tools);
        let over = tool.unwrap_or_else(|| window.id());
        let Ok(region) =
            WindowRegion::new(anchor.left(), anchor.top(), anchor.width, anchor.height)
        else {
            return;
        };
        match self.client.open_menu(over, region, &window.view.menu(kind)) {
            Ok(open) => window.menu = Some((open, over)),
            Err(err) => window.state(format!("The desktop composes no menu ({err})")),
        }
    }

    /// Take the one answer to window `index`'s open menu. An answer naming
    /// another gesture is one already settled, and is not acted on.
    fn menu_closed(&mut self, index: usize, open_id: u64, outcome: MenuOutcome) {
        let window = &mut self.windows[index];
        let Some((_, over)) = window.menu.filter(|&(open, _)| open == open_id) else {
            return;
        };
        window.menu = None;
        if window.loading() {
            return;
        }
        let outcome = match outcome {
            MenuOutcome::Chosen(item) => window.view.chosen(item, &mut window.damage),
            MenuOutcome::Entered(item) => {
                if let Err(err) = self.client.ask_menu_text(over, open_id) {
                    window.state(format!("The entry could not be read ({err})"));
                } else {
                    window.entering = Some((open_id, item));
                }
                return;
            }
            MenuOutcome::Refused(reason) => {
                window.state(format!("No menu was shown: {}", reason.describe()));
                return;
            }
            MenuOutcome::Dismissed => return,
        };
        self.apply(index, outcome);
    }

    /// Window `index`'s pointer is at `at` in its view's drawing — over the
    /// window, or over the window it owns at `owned` — with `modifiers` held,
    /// and `inputs` are what it did there at `at_ns`: note where it is, owe the
    /// tip and cursor beneath it a check, and feed the view the modifiers and
    /// then `inputs` unless the window is still loading. What lands on its
    /// dialog is the dialog's.
    fn point(
        &mut self,
        (index, owned): (usize, Option<usize>),
        at: Point,
        modifiers: AbiModifiers,
        inputs: impl Iterator<Item = InputEvent>,
        at_ns: u64,
    ) {
        let window = &mut self.windows[index];
        let (pointing, fed) = match owned.and_then(|owned| window.owned.get_mut(owned)) {
            Some(pane) => (&mut pane.chrome.pointing, Fed::of(pane.kind)),
            None => (&mut window.chrome.pointing, Fed::Window),
        };
        pointing.at = Some(at);
        pointing.owed = true;
        if window.loading() {
            return;
        }
        let held = key_input_event(KeyInput::ModifiersChanged { modifiers });
        self.feed(index, fed, core::iter::once(held).chain(inputs), at_ns);
    }

    /// Feed window `index`'s view `inputs`, which happened at `now`.
    fn feed(&mut self, index: usize, fed: Fed, inputs: impl Iterator<Item = InputEvent>, now: u64) {
        let id = self.windows[index].id();
        for input in inputs {
            let Some(index) = self.index_of(id) else {
                return;
            };
            let (theme, scale) = (self.themes.active(), self.desktop.scale());
            let window = &mut self.windows[index];
            let (view, damage) = (&mut window.view, &mut window.damage);
            let outcome = match fed {
                Fed::Window => view.input(&input, now, scale, theme, damage),
                Fed::Dialog => view.dialog_input(&input, now, scale, theme, damage),
            };
            self.apply(index, outcome);
        }
    }

    /// Show the pointer's shape and declare its tip once in every window
    /// whose input may have moved them, rather than once an event.
    fn settle_pointers(&mut self) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        for window in &mut self.windows {
            window.settle_pointer(&mut self.client, scale, theme);
        }
        for window in &mut self.app_windows {
            if mem::take(&mut window.chrome.pointing.owed) {
                let shape = match window.chrome.pointing.at {
                    Some(at) => window.view.cursor(at),
                    None => window.chrome.pointing.shape,
                };
                window.chrome.settle_pointer(&mut self.client, shape, None);
            }
        }
    }

    /// Route one delivered event. One naming a window no longer here is
    /// dropped.
    fn route(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::AppBarDefault => {
                self.new_window();
                return;
            }
            WindowEvent::AppBarMenu { item, .. } if is_quit(*item) => {
                self.quit();
                return;
            }
            WindowEvent::AppBarMenu { item, .. } => {
                match item.get() {
                    NEW_WINDOW_ROW => self.new_window(),
                    row => {
                        let own = row
                            .checked_sub(FIRST_APP_ROW)
                            .map(usize::from)
                            .filter(|&row| row < A::BAR_ROWS.len());
                        if let Some(row) = own {
                            A::bar_chosen(self, row);
                        }
                    }
                }
                return;
            }
            WindowEvent::OpenRequested => {
                if let Err(err) = self.client.ask_open_targets() {
                    self.report(format!("cannot ask for what to open ({err})"));
                }
                return;
            }
            WindowEvent::AlertClosed { alert_id, outcome } => {
                self.alert_answered(*alert_id, *outcome);
                return;
            }
            _ => {}
        }
        let id = event.window_id();
        let Some(index) = id.and_then(|id| self.index_of(id)) else {
            if let Some(index) = id.and_then(|id| self.app_index_of(id)) {
                self.act_app(index, event);
            } else if let Some(owned) = id.and_then(|id| self.owned_of(id)) {
                self.act_owned(owned, event);
            } else if let WindowEvent::FilePicked { handle, .. } = event {
                // A file chosen for a window since closed is let go at once,
                // not left held for the life of the process.
                release(*handle);
            }
            return;
        };
        self.act(index, event);
    }

    /// The user asked window `index` to close: `alternate` for a secondary
    /// press on its close button, which shows where its document lives once
    /// it has. One still reading its document in has nothing to show yet.
    fn close_asked(&mut self, index: usize, alternate: bool) {
        let window = &mut self.windows[index];
        if window.loading() {
            self.close_window(index);
            return;
        }
        window.reveal_on_close = alternate;
        let outcome = window.view.close_requested(&mut window.damage);
        self.apply(index, outcome);
    }

    /// Window `index` was resized to `(width, height)` physical pixels: its
    /// surface follows and it is laid out again.
    fn resized(&mut self, index: usize, (width, height): (u32, u32)) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.windows[index];
        window
            .chrome
            .resize(&mut self.client, width, height, A::NAME);
        window.relayout(&mut self.client, theme, scale, self.faces);
    }

    /// Route one window-scoped event to window `index`.
    fn act(&mut self, index: usize, event: &WindowEvent) {
        let window = &mut self.windows[index];
        // A close the user turned back, however, shows nothing when the window
        // later closes some other way.
        window.reveal_on_close &= window.closing_under_way();
        match event {
            WindowEvent::CloseRequested { .. } => self.close_asked(index, false),
            WindowEvent::AlternateCloseRequested { .. } => self.close_asked(index, true),
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => self.resized(index, (*width_px, *height_px)),
            WindowEvent::RedrawRequested { .. } => window.owe(Repaint::Whole),
            WindowEvent::ContentReleased { .. } => window.chrome.pane.release_frames(),
            WindowEvent::Focus { focused, .. } => {
                window.chrome.focused = *focused;
                window.note_focus();
            }
            WindowEvent::FilePicked {
                window_id,
                handle,
                writable,
            } => self.picked(index, *window_id, *handle, *writable),
            WindowEvent::FileOffered { .. } => self.file_offer(event),
            WindowEvent::ItemsOfferEnded { window_id, .. } => self.offers.ended(*window_id),
            // A document app asks for no folder, so a folder answer ends its
            // pick as a cancellation does.
            WindowEvent::PickCancelled { .. } | WindowEvent::FolderPicked { .. } => {
                let _ = window.file.end_pick();
            }
            WindowEvent::MenuClosed {
                open_id, outcome, ..
            } => self.menu_closed(index, *open_id, *outcome),
            WindowEvent::Key { key, .. } => {
                if !window.loading() {
                    let key = core::iter::once(key_input_event(*key));
                    self.feed(index, Fed::Window, key, tairix_rt::clock_get());
                }
            }
            WindowEvent::Pointer {
                x,
                y,
                action,
                modifiers,
                at_ns,
                ..
            } => {
                let at = pointer_point(*x, *y);
                self.point(
                    (index, None),
                    at,
                    *modifiers,
                    pointer_input_events(*action, at),
                    *at_ns,
                );
            }
            // A scrolled strip puts another tool under the pointer, which is
            // why a turn is owed a hover check as a move is.
            WindowEvent::Scrolled {
                x,
                y,
                dx,
                dy,
                modifiers,
                ..
            } => {
                let at = pointer_point(*x, *y);
                self.point(
                    (index, None),
                    at,
                    *modifiers,
                    scroll_input_events(at, *dx, *dy),
                    tairix_rt::clock_get(),
                );
            }
            WindowEvent::Pinch {
                x,
                y,
                phase,
                scale,
                modifiers,
                ..
            } => {
                let at = pointer_point(*x, *y);
                let pinch = pinch_input_events(at, *phase, *scale);
                self.point((index, None), at, *modifiers, pinch, tairix_rt::clock_get());
            }
            WindowEvent::Minimized { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::AppBarEntered { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::AlertClosed { .. }
            | WindowEvent::Authorised { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::SaveOffered { .. }
            | WindowEvent::SaveOfferEnded { .. }
            | WindowEvent::ItemsOffered { .. }
            | WindowEvent::PreviewRendered { .. } => {}
        }
    }

    /// Route one window-scoped event to application window `index`.
    fn act_app(&mut self, index: usize, event: &WindowEvent) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.app_windows[index];
        match event {
            WindowEvent::CloseRequested { .. } | WindowEvent::AlternateCloseRequested { .. } => {
                self.close_app_window(index);
            }
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => {
                window
                    .chrome
                    .resize(&mut self.client, *width_px, *height_px, A::NAME);
                window.lay_out(&mut self.client, (theme, scale, self.faces), true);
            }
            WindowEvent::RedrawRequested { .. } => window.chrome.owe(Repaint::Whole),
            WindowEvent::ContentReleased { .. } => window.chrome.pane.release_frames(),
            WindowEvent::Focus { focused, .. } => {
                window.chrome.focused = *focused;
                window.view.focus_changed(*focused, &mut window.damage);
                window.owe_reported();
            }
            // It asks for no file; one handed to it anyway is let go.
            WindowEvent::FilePicked { handle, .. } => release(*handle),
            WindowEvent::Key { key, .. } => {
                let key = core::iter::once(key_input_event(*key));
                self.feed_app(index, key, tairix_rt::clock_get());
            }
            WindowEvent::Pointer {
                x,
                y,
                action,
                modifiers,
                at_ns,
                ..
            } => {
                let at = pointer_point(*x, *y);
                let inputs = pointer_input_events(*action, at);
                self.point_app(index, at, *modifiers, inputs, *at_ns);
            }
            WindowEvent::Scrolled {
                x,
                y,
                dx,
                dy,
                modifiers,
                ..
            } => {
                let at = pointer_point(*x, *y);
                let inputs = scroll_input_events(at, *dx, *dy);
                self.point_app(index, at, *modifiers, inputs, tairix_rt::clock_get());
            }
            WindowEvent::Pinch {
                x,
                y,
                phase,
                scale,
                modifiers,
                ..
            } => {
                let at = pointer_point(*x, *y);
                let pinch = pinch_input_events(at, *phase, *scale);
                self.point_app(index, at, *modifiers, pinch, tairix_rt::clock_get());
            }
            WindowEvent::PickCancelled { .. }
            | WindowEvent::FolderPicked { .. }
            | WindowEvent::MenuClosed { .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::AppBarEntered { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::AlertClosed { .. }
            | WindowEvent::Authorised { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::SaveOffered { .. }
            | WindowEvent::SaveOfferEnded { .. }
            | WindowEvent::ItemsOffered { .. }
            | WindowEvent::ItemsOfferEnded { .. }
            | WindowEvent::FileOffered { .. }
            | WindowEvent::PreviewRendered { .. } => {}
        }
    }

    /// The document window and the place among the windows it owns of the
    /// one the session calls `id`.
    fn owned_of(&self, id: u64) -> Option<(usize, usize)> {
        self.windows.iter().enumerate().find_map(|(index, window)| {
            window
                .owned
                .iter()
                .position(|pane| pane.chrome.id() == id)
                .map(|owned| (index, owned))
        })
    }

    /// Follow the desktop's resize of the window at `owned` that document
    /// window `index` owns: a dialog it fitted to the screen scrolls to what
    /// no longer shows, a tool window the user may resize is laid out at the
    /// size it was given, and a fixed one is never resized.
    fn owned_resized(
        &mut self,
        (index, owned): (usize, usize),
        size: (u32, u32),
        event: &WindowEvent,
    ) {
        let pane = &mut self.windows[index].owned[owned];
        match pane.kind {
            Owned::Dialog(_) => {
                pane.chrome
                    .resize(&mut self.client, size.0, size.1, A::NAME);
                let _ = pane.scroll_by((0, 0), self.desktop.scale());
                pane.chrome.owe(Repaint::Whole);
            }
            Owned::Tool(id) if pane.sizing.resizable() => {
                pane.resized(&mut self.client, size, A::NAME);
                self.act_tool(index, id, event);
            }
            Owned::Tool(_) => {}
        }
    }

    /// Route one window-scoped event to the window at `owned` that document
    /// window `index` owns: a tool window's input is the window's own and its
    /// close mark, moves and resizes are its view's to answer; a dialog's
    /// input is the dialog's, and a dialog the desktop fitted to the screen
    /// scrolls by the wheel.
    fn act_owned(&mut self, (index, owned): (usize, usize), event: &WindowEvent) {
        let window = &mut self.windows[index];
        let pane = &mut window.owned[owned];
        let fed = Fed::of(pane.kind);
        match event {
            WindowEvent::CloseRequested { .. }
            | WindowEvent::AlternateCloseRequested { .. }
            | WindowEvent::ToolMoved { .. } => {
                if let Owned::Tool(id) = pane.kind {
                    self.act_tool(index, id, event);
                }
            }
            WindowEvent::Focus { focused, .. } => window.owned_focus(owned, *focused),
            WindowEvent::RedrawRequested { .. } => pane.chrome.owe(Repaint::Whole),
            WindowEvent::ContentReleased { .. } => pane.chrome.pane.release_frames(),
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => self.owned_resized((index, owned), (*width_px, *height_px), event),
            // It asks for no file; one handed to it anyway is let go.
            WindowEvent::FilePicked { handle, .. } => release(*handle),
            WindowEvent::Key { key, .. } => {
                if !window.loading() {
                    let key = core::iter::once(key_input_event(*key));
                    self.feed(index, fed, key, tairix_rt::clock_get());
                }
            }
            WindowEvent::Pointer {
                x,
                y,
                action,
                modifiers,
                at_ns,
                ..
            } => {
                let at = pane.to_view(pointer_point(*x, *y));
                let inputs = pointer_input_events(*action, at);
                self.point((index, Some(owned)), at, *modifiers, inputs, *at_ns);
            }
            WindowEvent::Scrolled {
                x,
                y,
                dx,
                dy,
                modifiers,
                ..
            } => {
                if pane.scroll_by((*dx, *dy), self.desktop.scale()) {
                    pane.chrome.owe(Repaint::Whole);
                    pane.chrome.pointing.owed = true;
                    return;
                }
                let at = pane.to_view(pointer_point(*x, *y));
                let inputs = scroll_input_events(at, *dx, *dy);
                let now = tairix_rt::clock_get();
                self.point((index, Some(owned)), at, *modifiers, inputs, now);
            }
            WindowEvent::Pinch {
                x,
                y,
                phase,
                scale,
                modifiers,
                ..
            } => {
                let at = pane.to_view(pointer_point(*x, *y));
                let pinch = pinch_input_events(at, *phase, *scale);
                let now = tairix_rt::clock_get();
                self.point((index, Some(owned)), at, *modifiers, pinch, now);
            }
            // A tool window's menu is its window's.
            WindowEvent::MenuClosed {
                open_id, outcome, ..
            } if fed == Fed::Window => self.menu_closed(index, *open_id, *outcome),
            // Never minimised but with its window, and asking for no picker
            // or preview, nor a dialog for a menu.
            WindowEvent::Minimized { .. }
            | WindowEvent::PickCancelled { .. }
            | WindowEvent::FolderPicked { .. }
            | WindowEvent::MenuClosed { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::AppBarEntered { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::AlertClosed { .. }
            | WindowEvent::Authorised { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::SaveOffered { .. }
            | WindowEvent::SaveOfferEnded { .. }
            | WindowEvent::ItemsOffered { .. }
            | WindowEvent::ItemsOfferEnded { .. }
            | WindowEvent::FileOffered { .. }
            | WindowEvent::PreviewRendered { .. } => {}
        }
    }

    /// Tool window `id` of document window `index` was closed by its mark,
    /// moved by its band or resized by its edges, as `event` says: its view's
    /// to answer.
    fn act_tool(&mut self, index: usize, id: u32, event: &WindowEvent) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.windows[index];
        let (view, damage) = (&mut window.view, &mut window.damage);
        let outcome = match *event {
            WindowEvent::ToolMoved { over, ended, .. } => {
                let over = match over {
                    ToolOver::Parent { x, y } => Some(pointer_point(x, y)),
                    ToolOver::Elsewhere => None,
                };
                view.tool_moved(ToolMove { id, over, ended }, scale, theme, damage)
            }
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => view.tool_resized(id, (width_px, height_px), scale, damage),
            _ => view.tool_gone(id, ToolGone::Closed, damage),
        };
        self.apply(index, outcome);
    }

    /// Open, resize, retitle and close every document window's tool windows
    /// and dialog to match what its view now asks for.
    fn settle_owned(&mut self) {
        let mut index = 0;
        while index < self.windows.len() {
            self.settle_window_tools(index);
            if index < self.windows.len() {
                self.settle_window_dialog(index);
            }
            index += 1;
        }
    }

    /// Bring window `index`'s tool windows into line with its view: close
    /// those it no longer wants, fit those it still does to their rectangle
    /// and title, and open the rest. One the desktop refuses is said and
    /// handed back to the view, and not asked again this round.
    fn settle_window_tools(&mut self, index: usize) {
        let window = &mut self.windows[index];
        let area = window.area();
        let mut at = window.owned.len();
        while let Some(back) = at.checked_sub(1) {
            at = back;
            let Some(id) = window.owned[at].tool() else {
                continue;
            };
            // One wanted at another kind of sizing goes, to open afresh as
            // asked: the desktop never changes that of an open window.
            let shown = (window.owned[at].rect, window.owned[at].sizing);
            let wanted = (0..)
                .map_while(|place| window.view.tool_window(place))
                .any(|wanted| {
                    wanted.id == id
                        && wanted.fits_beside(area)
                        && Refit::of(shown, (wanted.rect, wanted.sizing)).is_some()
                });
            if !wanted {
                let _ = window.owned.remove(at).chrome.pane.close(&mut self.client);
            }
        }
        let window_id = window.id();
        let mut refused: Vec<u32> = Vec::new();
        let mut place = 0;
        loop {
            let window = &mut self.windows[index];
            let Some(wanted) = window.view.tool_window(place) else {
                return;
            };
            place += 1;
            let (id, rect, sizing) = (wanted.id, wanted.rect, wanted.sizing);
            if !wanted.fits_beside(area) || refused.contains(&id) {
                continue;
            }
            if let Some(tool) = window
                .owned
                .iter_mut()
                .find(|pane| pane.kind == Owned::Tool(id))
            {
                tool.fit(&mut self.client, (rect, sizing), wanted.title, A::NAME);
                continue;
            }
            // A placeholder's tool windows would only go again once its
            // document lands.
            if window.reading != Reading::Done {
                continue;
            }
            let title = String::from(wanted.title);
            let opening = window.view.tool_opening(id);
            let opened = open_tool(
                &mut self.client,
                window_id,
                self.event_endpoint,
                (rect, sizing),
                opening,
                title,
            )
            .and_then(|chrome| {
                if window.owned.try_reserve(1).is_ok() {
                    return Ok(chrome);
                }
                let _ = chrome.pane.close(&mut self.client);
                Err(String::from("no room to hold another palette"))
            });
            match opened {
                Ok(chrome) => window.owned.push(OwnedPane::new(
                    Owned::Tool(id),
                    (rect, sizing),
                    chrome,
                    A::damage_sink(),
                )),
                Err(why) => {
                    if refused.try_reserve(1).is_err() {
                        return;
                    }
                    refused.push(id);
                    self.tool_refused(index, id, &why);
                    // The view's answer may have moved what it wants.
                    if self.windows.get(index).map(DocWindow::id) != Some(window_id) {
                        return;
                    }
                    place = 0;
                }
            }
        }
    }

    /// Bring window `index`'s dialog into line with its view: close one it no
    /// longer wants or that now holds the window differently, fit one it
    /// still does, and open one it newly asks for. One the desktop refuses is
    /// said and handed back to the view.
    fn settle_window_dialog(&mut self, index: usize) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.windows[index];
        let area = window.area();
        let wanted = window.view.dialog_window(scale, theme);
        let misplaced = wanted.is_some_and(|wanted| {
            !super::fits_beside(wanted.rect, area)
                || window.owned.iter().any(|pane| {
                    pane.tool().is_some() && !pane.rect.intersection(&wanted.rect).is_empty()
                })
        });
        let wanted = wanted.filter(|_| !misplaced);
        let open = window
            .owned
            .iter()
            .position(|pane| matches!(pane.kind, Owned::Dialog(_)));
        if let Some(at) = open {
            match wanted {
                Some(wanted) if window.owned[at].kind == Owned::Dialog(wanted.modality) => {
                    let wanted_at = (wanted.rect, WindowSizing::Fixed);
                    window.owned[at].fit(&mut self.client, wanted_at, wanted.title, A::NAME);
                    return;
                }
                _ => {
                    let _ = window.owned.remove(at).chrome.pane.close(&mut self.client);
                }
            }
        }
        if misplaced {
            self.dialog_refused(index, "it is drawn over the window's own content");
            return;
        }
        let Some(wanted) = wanted else {
            return;
        };
        let (rect, modality) = (wanted.rect, wanted.modality);
        let opened = open_dialog(
            &mut self.client,
            window.id(),
            self.event_endpoint,
            (rect, modality),
            String::from(wanted.title),
        )
        .and_then(|chrome| {
            if window.owned.try_reserve(1).is_ok() {
                return Ok(chrome);
            }
            let _ = chrome.pane.close(&mut self.client);
            Err(String::from("no room to hold a dialog"))
        });
        match opened {
            Ok(chrome) => window.owned.push(OwnedPane::new(
                Owned::Dialog(modality),
                (rect, WindowSizing::Fixed),
                chrome,
                A::damage_sink(),
            )),
            Err(why) => self.dialog_refused(index, &why),
        }
    }

    /// Window `index`'s dialog could not be shown, for `why`: say so, and hand
    /// it back to the view, which stops wanting it.
    fn dialog_refused(&mut self, index: usize, why: &str) {
        self.report(format!("a dialog could not be shown ({why})"));
        let window = &mut self.windows[index];
        let outcome = window.view.dialog_refused(why, &mut window.damage);
        self.apply(index, outcome);
    }

    /// The desktop would not open window `index`'s tool window `id`, for
    /// `why`: say so, and hand it back to the view.
    fn tool_refused(&mut self, index: usize, id: u32, why: &str) {
        self.report(format!(
            "a palette could not float ({why}); it stays in its window"
        ));
        let window = &mut self.windows[index];
        let outcome = window
            .view
            .tool_gone(id, ToolGone::Refused, &mut window.damage);
        self.apply(index, outcome);
    }

    /// Tell each document window's view that it lost the keyboard where it
    /// has, now that the round's events are in.
    fn settle_focus(&mut self) {
        for window in &mut self.windows {
            if window.focus_told && !window.focused() {
                window.tell_focus(false);
            }
        }
    }

    /// Application window `index`'s pointer is at `at`, as [`point`] notes a
    /// document window's.
    ///
    /// [`point`]: Self::point
    fn point_app(
        &mut self,
        index: usize,
        at: Point,
        modifiers: AbiModifiers,
        inputs: impl Iterator<Item = InputEvent>,
        at_ns: u64,
    ) {
        let window = &mut self.app_windows[index];
        window.chrome.pointing.at = Some(at);
        window.chrome.pointing.owed = true;
        let held = key_input_event(KeyInput::ModifiersChanged { modifiers });
        self.feed_app(index, core::iter::once(held).chain(inputs), at_ns);
    }

    /// Feed application window `index`'s view `inputs`, which happened at
    /// `now`.
    fn feed_app(&mut self, index: usize, inputs: impl Iterator<Item = InputEvent>, now: u64) {
        let id = self.app_windows[index].id();
        for input in inputs {
            let Some(index) = self.app_index_of(id) else {
                return;
            };
            let (theme, scale) = (self.themes.active(), self.desktop.scale());
            let window = &mut self.app_windows[index];
            let outcome = window
                .view
                .input(&input, now, scale, theme, &mut window.damage);
            self.apply_app(index, outcome);
        }
    }

    /// Quit: every window closes, and those holding changes ask first, one
    /// at a time.
    fn quit(&mut self) {
        self.quitting = true;
        for window in &mut self.windows {
            window.quit_asked = false;
            window.reveal_on_close &= window.closing_under_way();
        }
        // The first question goes up before the rest come down, while the
        // user's own quit still lets it come forward.
        self.carry_quit_on(None);
        // A member closes with its primary, so a quit the primary's question
        // turns down leaves it open.
        let done: Vec<u64> = self
            .windows
            .iter()
            .filter(|window| {
                !window.is_member() && (window.loading() || !window.view.is_modified())
            })
            .map(DocWindow::id)
            .collect();
        for id in done {
            if let Some(index) = self.index_of(id) {
                self.close_window(index);
            }
        }
    }

    /// Carry a quit on: given up once the user kept a window they were asked
    /// about, else the next window holding changes is asked whether to save
    /// them, once nothing else is being asked. `leaving`, about to close, is
    /// passed over.
    fn carry_quit_on(&mut self, leaving: Option<u64>) {
        if !self.quitting {
            return;
        }
        let standing = self
            .windows
            .iter()
            .map(|window| window.quit_standing(Some(window.id()) == leaving));
        let index = match next_quit_step(standing) {
            QuitStep::Ask(index) => index,
            QuitStep::GiveUp => {
                self.quitting = false;
                return;
            }
            QuitStep::Wait | QuitStep::Done => return,
        };
        let window = &mut self.windows[index];
        window.quit_asked = true;
        let outcome = window.view.close_requested(&mut window.damage);
        self.apply(index, outcome);
    }

    /// Close every application window.
    fn close_app_windows(&mut self) {
        while let Some(last) = self.app_windows.len().checked_sub(1) {
            self.close_app_window(last);
        }
    }

    /// Open a window at `target`, one of the documents the desktop has handed
    /// this instance.
    fn open_target(&mut self, target: Target) {
        match target {
            Target::Document {
                name,
                grant,
                writable,
            } => match Handle::from_delegation(grant) {
                Ok(file) => self.open_document(file, name, writable, None),
                Err(raw) => self.alert(
                    None,
                    &refusal_headline(name.as_str(), "opened"),
                    &format!(
                        "The document could not be taken over ({}).",
                        Errno::from_syscall(raw)
                    ),
                ),
            },
            // Unreachable from the desktop, which hands a document
            // application an open document because it holds no authority
            // to open a name.
            Target::Path(path) | Target::Image(path) => self.report(format!(
                "{path} was handed over as a path; {} holds no filesystem authority and \
                 can only be given an open document",
                A::NAME
            )),
            Target::Pane(pane) => self.report(format!(
                "{pane} was handed over, but {} has no places to go to",
                A::NAME
            )),
            Target::Placed {
                window_id,
                at,
                name,
                grant,
            } => self.placed(window_id, pointer_point(at.0, at.1), name, grant),
        }
    }

    /// A file another application's drag holds over a window, offered at a
    /// point: answered with whether a drop there places it, as the window's
    /// view says for the file's media type, read once for every offer the
    /// drag makes over the application's windows and never waited on.
    fn file_offer(&mut self, event: &WindowEvent) {
        let &WindowEvent::FileOffered {
            window_id,
            drag,
            serial,
            x,
            y,
        } = event
        else {
            return;
        };
        let step = self.offers.offered(FileOffer {
            window_id,
            drag,
            serial,
            at: pointer_point(x, y),
        });
        self.take_offer_step(step);
    }

    /// Take in every answer that has landed.
    fn take_answers(&mut self) {
        while let Some(answer) = self.client.take_answer() {
            self.take_answer(answer);
        }
    }

    /// Act on one landed answer.
    fn take_answer(&mut self, answer: Answer) {
        match answer {
            Answer::OfferedFile(read) => {
                let (step, failed) = self.offers.landed(read);
                if let Some(err) = failed {
                    self.report(format!("cannot read a file offered ({err})"));
                }
                self.take_offer_step(step);
            }
            Answer::OpenTarget(Ok(Some(target))) => self.open_target(target),
            Answer::OpenTarget(Err(err)) => {
                self.report(format!("cannot take an open target ({err})"));
            }
            Answer::PickedName { window_id, name } => {
                if let Some(index) = self.index_of(window_id) {
                    self.name_chosen(index, name.ok().filter(|name| !name.is_empty()));
                }
            }
            Answer::MenuText {
                window_id: _,
                open_id,
                text,
            } => self.entered(open_id, text),
            Answer::Revealed { token, shown } => self.revealed(token, shown),
            Answer::Clipboard {
                window_id,
                contents,
            } => {
                if let Some(index) = self.index_of(window_id) {
                    A::pasted(self, index, contents);
                }
            }
            Answer::FrameReturned { window_id } => self.frame_returned(window_id),
            // The end of the open targets, and nothing else is asked for here.
            _ => {}
        }
    }

    /// The text committed into the quick-entry row of menu open `open_id` has
    /// landed.
    fn entered(&mut self, open_id: u64, text: Result<Option<String>, Errno>) {
        let Some(index) = self
            .windows
            .iter()
            .position(|window| window.entering.is_some_and(|(open, _)| open == open_id))
        else {
            return;
        };
        let window = &mut self.windows[index];
        let Some((_, item)) = window.entering.take() else {
            return;
        };
        if window.loading() {
            return;
        }
        let outcome = match text {
            Ok(Some(text)) => window.view.entered(item, &text, &mut window.damage),
            Ok(None) => return,
            Err(err) => {
                window.state(format!("The entry could not be read ({err})"));
                return;
            }
        };
        self.apply(index, outcome);
    }

    /// Window `window_id`'s last present was answered, so what was drawn
    /// behind it is presented now.
    fn frame_returned(&mut self, window_id: u64) {
        let chrome = if let Some(index) = self.index_of(window_id) {
            &mut self.windows[index].chrome
        } else if let Some(index) = self.app_index_of(window_id) {
            &mut self.app_windows[index].chrome
        } else if let Some((index, at)) = self.owned_of(window_id) {
            &mut self.windows[index].owned[at].chrome
        } else {
            return;
        };
        let presented = chrome
            .pane
            .present_deferred(&mut self.client, &chrome.surface);
        if presented.is_err() {
            chrome.owe(Repaint::Whole);
        }
    }

    /// Act on a request the session refused: what it created never existed,
    /// and a pick or a menu it would not offer is said.
    fn refused(&mut self, refused: Refused) {
        let window_id = refused.window_id;
        match refused.request {
            PostedRequest::Create => {
                let why = format!("{}", refused.errno);
                if let Some(index) = self.index_of(window_id) {
                    self.close_window(index);
                    self.alert(None, NO_NEW_WINDOW, &why);
                } else if let Some(index) = self.app_index_of(window_id) {
                    self.close_app_window(index);
                    self.alert(None, NO_NEW_WINDOW, &why);
                } else if let Some((index, at)) = self.owned_of(window_id) {
                    let pane = self.windows[index].owned.remove(at);
                    match pane.kind {
                        Owned::Tool(id) => self.tool_refused(index, id, &why),
                        Owned::Dialog(_) => self.dialog_refused(index, &why),
                    }
                }
                return;
            }
            PostedRequest::PickFile => {
                if let Some(index) = self.index_of(window_id) {
                    let _ = self.windows[index].file.end_pick();
                    self.quitting = false;
                    self.alert(
                        Some(index),
                        "No file chooser could be opened",
                        &format!("The desktop would not offer one ({}).", refused.errno),
                    );
                    return;
                }
            }
            PostedRequest::SetClipboard => {
                if let Some(index) = self.index_of(window_id) {
                    self.windows[index].state(format!("Could not copy: {}", refused.errno));
                    return;
                }
            }
            PostedRequest::OpenMenu(open_id) => {
                let window = self
                    .windows
                    .iter_mut()
                    .find(|window| window.menu.is_some_and(|(open, _)| open == open_id));
                if let Some(window) = window {
                    window.menu = None;
                    window.state(format!("The desktop composes no menu ({})", refused.errno));
                    return;
                }
            }
            _ => {}
        }
        self.report(refused);
    }

    /// Carry out what a file offer, or its read landing, called for.
    fn take_offer_step(&mut self, step: OfferStep) {
        match step {
            OfferStep::Rest => {}
            OfferStep::Ask(offer) => {
                if let Err(err) = self.client.ask_offered_file(offer.window_id, offer.serial) {
                    self.offers.unasked(offer);
                    self.report(format!("cannot read a file offered ({err})"));
                }
            }
            OfferStep::Answer(offer) => self.answer_offer(offer),
        }
    }

    /// Answer `offer` with whether a drop there places the file, as its
    /// window's view says for the media type its drag holds.
    fn answer_offer(&mut self, offer: FileOffer) {
        let Some(window) = self
            .windows
            .iter()
            .find(|window| window.id() == offer.window_id)
        else {
            return;
        };
        let places = !window.loading()
            && self
                .offers
                .media(offer.drag)
                .is_some_and(|media| window.view.places(offer.at, media));
        if let Err(err) = self
            .client
            .answer_file_offer(offer.window_id, offer.serial, places)
        {
            self.report(format!("cannot answer a file offered ({err})"));
        }
    }

    /// A file dropped on window `window_id` at `at`, called `name`, to be
    /// placed there: taken over through `grant` and read into its document,
    /// or let go with the reason said where the window has since closed.
    fn placed(&mut self, window_id: u64, at: Point, name: String, grant: u64) {
        let file = match Handle::from_delegation(grant) {
            Ok(file) => file,
            Err(raw) => {
                self.alert(
                    self.index_of(window_id),
                    &refusal_headline(name.as_str(), "placed"),
                    &format!(
                        "The file could not be taken over ({}).",
                        Errno::from_syscall(raw)
                    ),
                );
                return;
            }
        };
        match self.index_of(window_id) {
            Some(index) => A::import_chosen(self, index, file, name, Some(at)),
            None => self.report(format!(
                "{name} was dropped on a window that has since closed"
            )),
        }
    }

    /// Take in one answer from the queue.
    fn adopt(&mut self, reply: Reply<AnsweredOf<A>>) {
        let Reply { window, answer } = reply;
        let index = self.index_of(window);
        match answer {
            Answered::Saved(saved) => {
                self.saves = self.saves.saturating_sub(1);
                self.queue.shrink(usize::from(saved.grew));
                if let Some(index) = index {
                    self.saved(index, saved);
                } else {
                    // A window since closed has nothing left to show, but a
                    // save it asked for that failed is still said.
                    self.give_back_room();
                    if let Err(err) = &saved.result {
                        self.report(format!(
                            "{} could not be saved: {err}",
                            tairix_util::utf8::shown(&saved.name)
                        ));
                    }
                }
            }
            Answered::Own { epoch, answer } => {
                if let Some(index) = index {
                    let asker = &mut self.windows[index];
                    asker.own_held = asker.own_held.saturating_sub(1);
                    if asker.epoch == epoch {
                        A::answered(self, index, answer);
                    } else {
                        A::orphaned(self, window, answer);
                    }
                } else {
                    self.give_back_room();
                    A::orphaned(self, window, answer);
                }
            }
        }
    }

    /// Present what every window owes at `now_ns`, but for one still kept off
    /// the screen while its document is read in. A present the session
    /// refuses is that window's trouble alone: it is said once, and the window
    /// paints whole at its next chance, while every other window carries on.
    fn paint_all(&mut self, now_ns: u64) {
        let theme = self.themes.active();
        let scale = self.desktop.scale();
        let faces = self.faces;
        let app = &mut self.app;
        for window in &mut self.windows {
            if window.withheld.holds(now_ns) {
                continue;
            }
            window.paint(&mut self.client, |surface, view, focused| {
                app.render(surface, view, (theme, scale, faces), focused);
            });
        }
        for window in &mut self.app_windows {
            if window.chrome.owed != Repaint::Nothing {
                window.chrome.title.read(window.view.title());
            }
            let view = &window.view;
            let painted =
                window
                    .chrome
                    .present(&mut self.client, &mut window.damage, |surface, focused| {
                        app.render_app(surface, view, (theme, scale, faces), focused);
                    });
            window
                .chrome
                .presented(painted, A::NAME, window.view.title());
        }
    }

    /// Adopt a new desktop state: every window restyled at its density.
    fn adopt_desktop(&mut self) {
        match app::adopt_desktop(&mut self.desktop, &mut self.themes) {
            Ok(true) => self.lay_out_all(),
            Ok(false) => {}
            Err(err) => self.report(format!("desktop change refused: {err}")),
        }
    }

    /// Lay every window out again in the active look, its faces made anew:
    /// after a change of look, or once glyphs drawn provisionally have
    /// arrived, so text is measured and drawn with them.
    fn lay_out_all(&mut self) {
        let theme = self.themes.active();
        let scale = self.desktop.scale();
        self.faces = A::faces(theme, scale);
        for window in &mut self.windows {
            window.relayout(&mut self.client, theme, scale, self.faces);
        }
        for window in &mut self.app_windows {
            window.lay_out(&mut self.client, (theme, scale, self.faces), true);
        }
    }

    /// End with `code` once every save asked for has landed, stating any that
    /// failed: a process ending mid-write leaves a file part new, part old.
    /// Closing every window first withdraws the work nobody can now see.
    fn leave(&mut self, code: i32) -> i32 {
        A::leaving(self);
        // Closing each window queues the saves chained behind its save in
        // flight, which would otherwise never be written.
        while let Some(last) = self.windows.len().checked_sub(1) {
            self.close_window(last);
        }
        self.close_app_windows();
        while let Some(answer) = self.queue.wait() {
            if let Some(reply) = answer {
                self.adopt(reply);
            }
        }
        code
    }
}

/// Cut `file`, just written from its start, to the `len` bytes the save wrote
/// and make it durable: how every save ends.
///
/// # Errors
///
/// The kernel's refusal of either.
pub fn commit(file: &Handle, len: u64) -> Result<(), Errno> {
    file.truncate(len).map_err(Errno::from_syscall)?;
    file.sync().map_err(Errno::from_syscall)
}

/// The name `stem` is offered under with `extension`: none for a format named
/// by its file type alone.
fn offered_name(stem: &str, extension: &str) -> String {
    if extension.is_empty() {
        String::from(stem)
    } else {
        format!("{stem}.{extension}")
    }
}

/// `name` without the extension or RISC OS file type that ends it.
fn stem(name: &str) -> &str {
    match name.rfind(['.', ',']) {
        Some(at) if at > 0 => &name[..at],
        _ => name,
    }
}

/// Tell the session afresh that `pane` may be sized from `least` up, where
/// that is not `told`, the least it was last told: held to a stale least, a
/// window whose view shrank could not be made its size.
fn restate_least<A: DocumentApp>(
    client: &mut Client,
    pane: &WindowPane,
    told: &mut (u32, u32),
    least: (u32, u32),
) {
    if least == *told {
        return;
    }
    match client.set_sizing(pane.id(), resizable(least)) {
        Ok(()) => *told = least,
        Err(err) => report(
            A::NAME,
            format_args!("a window's least size was not restated: {err}"),
        ),
    }
}

/// A window any size from `least` up.
const fn resizable(least: (u32, u32)) -> WindowSizing {
    WindowSizing::Resizable {
        min_width_px: least.0,
        min_height_px: least.1,
        max_width_px: 0,
        max_height_px: 0,
    }
}

/// Open a resizable pane of `mode` titled `title`, held to at least `least`,
/// its events sent to `endpoint`, answering why where it could not be.
fn open_pane(
    client: &mut Client,
    endpoint: u64,
    mode: &DisplayMode,
    title: &str,
    least: (u32, u32),
) -> Result<WindowPane, String> {
    WindowPane::open(client, endpoint, mode, title, resizable(least))
        .map_err(|err| format!("{err}"))
}

/// Open a tool window over `rect` of window `parent`'s drawing, sized as
/// `sizing` allows, titled `title` and opening as `opening` says, its events
/// sent to `endpoint`; it opens without the keyboard.
fn open_tool(
    client: &mut Client,
    parent: u64,
    endpoint: u64,
    (rect, sizing): (Rect, WindowSizing),
    opening: super::ToolOpening,
    title: String,
) -> Result<Chrome, String> {
    let mode = app::mode_for(rect.width, rect.height);
    let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
        return Err(String::from("no drawing surface"));
    };
    let carry = opening
        .carry
        .map(|along| along.min(rect.width.saturating_sub(1)));
    let pane = WindowPane::open_tool(
        client,
        parent,
        endpoint,
        (&mode, sizing),
        opening.offset,
        carry,
        &title,
    )
    .map_err(|err| format!("{err}"))?;
    let mut chrome = Chrome::new(pane, surface, title);
    chrome.focused = false;
    Ok(chrome)
}

/// Open the dialog of window `owner` showing a `rect` of its drawing and holding
/// it as `modality` says, titled `title`, its events sent to `endpoint`.
fn open_dialog(
    client: &mut Client,
    owner: u64,
    endpoint: u64,
    (rect, modality): (Rect, DialogModality),
    title: String,
) -> Result<Chrome, String> {
    let mode = app::mode_for(rect.width, rect.height);
    let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
        return Err(String::from("no drawing surface"));
    };
    let pane = WindowPane::open_dialog(client, owner, endpoint, &mode, modality, &title)
        .map_err(|err| format!("{err}"))?;
    let mut chrome = Chrome::new(pane, surface, title);
    chrome.focused = false;
    Ok(chrome)
}

/// Let go of file grant `grant` the application has no use for, so it is not
/// held for the life of the process.
fn release(grant: u64) {
    drop(Handle::from_delegation(grant));
}

/// The document this program was handed on [`STDIN`] at spawn, its name, and
/// whether it may be written, when it was launched with one.
fn launched_document() -> Option<(Handle, String, bool)> {
    let writable = match tairix_rt::arg(1)? {
        arg if arg == DOCUMENT_WRITABLE_ROLE_ARG => true,
        arg if arg == DOCUMENT_ROLE_ARG => false,
        _ => return None,
    };
    let name = tairix_rt::arg(2)
        .and_then(|raw| core::str::from_utf8(raw).ok())
        .map(|path| String::from(path.rsplit('/').next().unwrap_or(path)))
        .unwrap_or_default();
    Some((Handle::adopt(STDIN), name, writable))
}

/// A document application's icon-bar presence: a click opens a window when
/// none is open, and its menu offers another, then the application's own rows.
fn app_bar<A: DocumentApp>(endpoint: u64) -> Result<AppBar, Errno> {
    let item = |id: u16, symbol: AppMenuSymbol, label: &str| -> Result<AppMenuRow, Errno> {
        Ok(AppMenuRow::Item(AppMenuItem::new(
            AppMenuItemId::new(id)?,
            AppMenuLabel::new(label)?,
            symbol,
        )))
    };
    let mut rows = Vec::new();
    rows.try_reserve_exact(1 + A::BAR_ROWS.len())
        .map_err(|_| Errno::OutOfMemory)?;
    rows.push(item(
        NEW_WINDOW_ROW,
        AppMenuSymbol::NewWindow,
        "New window",
    )?);
    for (index, &(symbol, label)) in A::BAR_ROWS.iter().enumerate() {
        let id = u16::try_from(index)
            .ok()
            .and_then(|index| FIRST_APP_ROW.checked_add(index))
            .ok_or(Errno::OutOfRange)?;
        rows.push(item(id, symbol, label)?);
    }
    declaration(endpoint, AppBarClick::RaiseOrOpen, &rows)
}

/// What the park saw that the loop is to take in.
#[derive(Default)]
struct Signals {
    /// When the application next needs waking, or `None`, in which case the
    /// park arms no timer.
    deadline_ns: Cell<Option<u64>>,
    pressure: Cell<bool>,
    desktop_moved: Cell<bool>,
}

/// The park: the event mailbox, the answer wakes of the queue and of the
/// application's own workers, the memory-pressure band, the desktop state,
/// and the application's deadline.
struct Park<'a> {
    mailbox: EventMailbox,
    set: u64,
    queue: &'a dyn AnswerWake,
    fonts: &'a dyn AnswerWake,
    wakes: &'a [(u64, Arc<dyn AnswerWake>)],
    signals: &'a Signals,
}

impl EventDrain for Park<'_> {
    fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
        self.mailbox.try_next(event)
    }
}

impl EventSource for Park<'_> {
    fn park(&mut self) -> Result<Parked, Errno> {
        let Some(woken) = app::park_until_due(self.set, self.signals.deadline_ns.get())? else {
            return Ok(Parked::Interrupted);
        };
        match woken {
            Wake::App(QUEUE_TOKEN) => {
                self.queue.drain();
                Ok(Parked::Interrupted)
            }
            Wake::App(FONT_TOKEN) => {
                self.fonts.drain();
                Ok(Parked::Interrupted)
            }
            Wake::App(token) => match self.wakes.iter().find(|(watched, _)| *watched == token) {
                Some((_, wake)) => {
                    wake.drain();
                    Ok(Parked::Interrupted)
                }
                None => Ok(Parked::Served),
            },
            Wake::PressureChanged => {
                self.signals.pressure.set(true);
                Ok(Parked::Interrupted)
            }
            Wake::DesktopChanged => {
                self.signals.desktop_moved.set(true);
                Ok(Parked::Interrupted)
            }
            Wake::Event | Wake::PressureUnchanged => Ok(Parked::Served),
        }
    }
}

type Events<'a> = WindowEvents<Park<'a>>;

impl<A: DocumentApp> Host<A> {
    /// Serve until the application ends, answering its exit code.
    fn serve(&mut self, events: &mut Events<'_>, signals: &Signals) -> i32 {
        loop {
            if let ControlFlow::Break(code) = self.round(events, signals) {
                return code;
            }
        }
    }

    /// One turn of the loop: take in everything that has landed and every
    /// event queued before painting, so a burst costs one frame, then park.
    fn round(&mut self, events: &mut Events<'_>, signals: &Signals) -> ControlFlow<i32> {
        self.answered = false;
        let queue = Arc::clone(&self.queue);
        queue.collect_landed(|answer| {
            if let Some(reply) = answer {
                self.adopt(reply);
            }
        });
        A::collect(self);
        let _ = self.fonts.collect();
        if tairix_font::text_generation() != self.text_drawn {
            self.text_drawn = tairix_font::text_generation();
            self.lay_out_all();
        }
        if signals.pressure.replace(false) {
            A::pressure(self);
        }
        if signals.desktop_moved.replace(false) {
            self.adopt_desktop();
        }
        loop {
            match events.try_wait(&mut self.client) {
                Ok(None) => break,
                polled => self.take_event(polled)?,
            }
        }
        self.take_answers();
        let now = tairix_rt::clock_get();
        let answered = A::turn(self, now) | self.answered;
        self.advance_app_windows(now);
        self.carry_quit_on(None);
        self.settle_owned();
        self.settle_focus();
        self.settle_pointers();
        self.paint_all(now);
        // What was drawn provisionally is fetched off the loop; a request
        // already waiting absorbs this one.
        if tairix_font::has_deferred() {
            let _ = self.fonts.submit(());
        }
        if self.quitting && self.windows.is_empty() {
            // The quit can no longer be turned down, so the application's own
            // windows, kept until now in case it was, go.
            self.close_app_windows();
            if self.saves == 0 {
                // Work that outlives its document, a copy, is seen out and its
                // fate said rather than dropped with the worker.
                return ControlFlow::Break(self.leave(0));
            }
        }
        // A job carried out here for want of a worker has its answer already
        // on the desk, so the loop goes round again rather than parking.
        if !answered {
            let own = A::deadline(self, now);
            let held = self
                .app_windows
                .iter_mut()
                .filter_map(|window| window.view.deadline_ns(now))
                .min();
            let shown = self
                .windows
                .iter()
                .filter_map(|window| window.withheld.until());
            signals
                .deadline_ns
                .set(own.into_iter().chain(held).chain(shown).min());
            let polled = events.wait(&mut self.client);
            self.take_event(polled)?;
        }
        ControlFlow::Continue(())
    }

    /// Step what each application window's held controls have brought due.
    fn advance_app_windows(&mut self, now_ns: u64) {
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        for window in &mut self.app_windows {
            if window
                .view
                .deadline_ns(now_ns)
                .is_some_and(|due| due <= now_ns)
            {
                window
                    .view
                    .advance(now_ns, scale, theme, &mut window.damage);
                window.owe_reported();
            }
        }
    }

    /// Route the event `polled` brought, if any. A malformed one is refused
    /// and said; a dead channel ends the application, its saves seen out.
    fn take_event(&mut self, polled: Result<Option<WindowEvent>, EventError>) -> ControlFlow<i32> {
        match polled {
            Ok(Some(event)) => self.route(&event),
            Ok(None) => {}
            Err(EventError::Mailbox(_)) => {
                let code = fail(A::NAME, app::EXIT_CHANNEL_LOST, "the event channel died");
                return ControlFlow::Break(self.leave(code));
            }
            Err(EventError::Undecodable(_)) => {
                self.report("a malformed window event was refused");
            }
            Err(EventError::Refused(refused)) => self.refused(refused),
        }
        ControlFlow::Continue(())
    }
}

/// What a document application's loop is brought up on.
struct Session {
    client: Client,
    desktop: Desktop,
    themes: ThemeRegistry,
    /// The session's process, the only sender a window reply is taken from.
    server: ProcId,
    /// The endpoint window events are delivered to.
    endpoint: u64,
    /// The wait-set the loop parks on.
    set: u64,
}

impl Session {
    /// Reach the desktop for application `name` and bind its event mailbox.
    ///
    /// # Errors
    ///
    /// The exit code to end with, the reason already stated.
    fn connect(name: &str) -> Result<Self, i32> {
        let mut client = WindowClient::new(RtWindowTransport);
        let (desktop, themes) =
            app::bring_up_desktop(&mut client).map_err(|err| fail(name, err.code(), err))?;
        let Some(server) = client.session() else {
            return Err(fail(
                name,
                app::EXIT_NO_WINDOW,
                "the desktop did not identify itself",
            ));
        };
        let binding = app::bind_event_mailbox().map_err(|err| fail(name, err.code(), err))?;
        Ok(Self {
            client,
            desktop,
            themes,
            server,
            endpoint: binding.endpoint(),
            set: binding.set(),
        })
    }
}

/// Run document application `A` and answer its exit code.
///
/// It opens on the document it was launched with, else on a new one, and ends
/// on Quit once every window has closed and every save has landed — or, with
/// its saves seen out, when the session goes away.
pub fn run<A: DocumentApp>() -> i32 {
    let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
    let Session {
        mut client,
        desktop,
        themes,
        server,
        endpoint,
        set,
    } = match Session::connect(A::NAME) {
        Ok(session) => session,
        Err(code) => return code,
    };

    // The queue's room grows with each window opened.
    let Ok(queue) = Queue::<A>::queued(serve::<A>, (), WorkerWake::create(), 0) else {
        return fail(A::NAME, app::EXIT_NO_EVENTS, "no room for the work queue");
    };
    let queue = Arc::new(queue);
    app::start_worker(A::NAME, &queue, set, QUEUE_TOKEN, "work");
    let _queue_guard = WorkerGuard::new(&queue);
    // With no thread to fetch on, text is fetched as it is drawn.
    let fonts = Arc::new(FontWorker::new(fetch_fonts, (), WorkerWake::create()));
    if app::start_worker(A::NAME, &fonts, set, FONT_TOKEN, "font") {
        tairix_font::defer_fetches();
    }
    let _fonts_guard = WorkerGuard::new(&fonts);
    let own = match A::start(&desktop, set) {
        Ok(own) => own,
        Err(code) => return code,
    };
    let wakes = own.wakes();

    if let Err(refused) = declare_app_bar(&mut client, app_bar::<A>(endpoint)) {
        report(A::NAME, refused);
    }

    let faces = A::faces(themes.active(), desktop.scale());
    let mut host = Host {
        client,
        windows: Vec::new(),
        app_windows: Vec::new(),
        desktop,
        themes,
        faces,
        app: own,
        queue: Arc::clone(&queue),
        fonts: Arc::clone(&fonts),
        text_drawn: tairix_font::text_generation(),
        answered: false,
        saves: 0,
        unroomed: 0,
        event_endpoint: endpoint,
        quitting: false,
        reveals: Vec::new(),
        offers: FileOffers::new(<ViewOf<A> as DocumentView>::PLACES),
    };
    match launched_document() {
        Some((handle, name, writable)) => host.open_document(handle, name, writable, None),
        None => host.new_window(),
    }

    let signals = Signals::default();
    let mut events = WindowEvents::new(Park {
        mailbox: EventMailbox::new(endpoint, server),
        set,
        queue: &*queue,
        fonts: &*fonts,
        wakes: &wakes,
        signals: &signals,
    });
    host.serve(&mut events, &signals)
}
