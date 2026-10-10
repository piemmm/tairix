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
    AppBar, AppBarClick, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuRow, CursorShape,
    DocumentName, MenuOutcome, PickPurpose, ToolOver, WindowEvent, WindowRegion, WindowSizing,
};
use tairix_abi::{Errno, ProcId, DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, STDIN};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_rt::sync::WorkerWake;
use tairix_rt::work::{Desk, Worker, WorkerGuard};
use tairix_theme::{Theme, ThemeRegistry};
use tairix_util::defer::JobQueue;

use super::{
    AppRequest, AppView, DocumentView, Outcome, Relayout, Request, ToolGone, ToolMove, ViewOutcome,
};
use crate::app::{self, fail, report, RtWindowTransport, Wake, WindowPane};
use crate::appbar::{declaration, declare_app_bar, is_quit, QUIT_ROW};
use crate::client::{
    key_input_event, pinch_input_events, pointer_input_events, pointer_point, present_damage_list,
    scroll_input_events, DeclaredTip, EventDrain, EventError, EventSource, Parked, Repaint, Target,
    WindowClient, WindowEvents,
};
use crate::desktop::Desktop;
use crate::document::{Access, DocumentFile, PickFor, SaveJob, SaveStep, SavedDocument, UNTITLED};
use crate::mailbox::EventMailbox;

/// The descriptor a document is read from and saved through, whether it was
/// cloned in at spawn or redeemed from a grant: it closes once its last
/// holder — the window, or a save still being written — lets go.
pub type Handle = tairix_rt::File;

/// A document application's window channel.
pub type Client = WindowClient<RtWindowTransport>;

type ViewOf<A> = <A as DocumentApp>::View;
type LayoutOf<A> = <ViewOf<A> as DocumentView>::Layout;
type FacesOf<A> = <ViewOf<A> as DocumentView>::Faces;
type MenuKindOf<A> = <ViewOf<A> as DocumentView>::MenuKind;
type RequestOf<A> = Request<MenuKindOf<A>, <ViewOf<A> as DocumentView>::Own>;
type SnapshotOf<A> = <A as DocumentApp>::Snapshot;
type AppViewOf<A> = <A as DocumentApp>::AppView;
type AppLayoutOf<A> = <AppViewOf<A> as AppView>::Layout;
type AppOwnOf<A> = <AppViewOf<A> as AppView>::Own;

/// The wait-set token of the queue's answer wake.
const QUEUE_TOKEN: u64 = app::FIRST_APP_TOKEN;

/// The lowest token an application's own worker may be watched under.
pub const APP_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;

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
        write: impl FnOnce(&SaveJob<Handle, S>, &str) -> Result<Option<&'static str>, E>,
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
    result: Result<Option<&'static str>, E>,
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
    /// The application's own icon-bar rows, after *New window*, in order.
    const BAR_ROWS: &'static [&'static str] = &[];

    /// Bring the application's own state up for `desktop`: its own workers
    /// started ([`start_worker`]) and their answer wakes watched on `set`
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
    ) -> Result<Option<&'static str>, Self::Failure>;

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

    /// The process is ending: see the application's own outstanding work out,
    /// as the host sees its saves out, so nothing asked for is dropped.
    fn leaving(_host: &mut Host<Self>) {}

    /// Icon-bar row `row` of [`BAR_ROWS`](Self::BAR_ROWS) was chosen, under
    /// the activation the choice grants, so a window raised now is given the
    /// keyboard.
    fn bar_chosen(_host: &mut Host<Self>, _row: usize) {}

    /// Application window `index` is about to close, by its close mark, its
    /// own asking, or a quit.
    fn app_closing(_host: &mut Host<Self>, _index: usize) {}

    /// Carry out a request of application window `index` only this
    /// application makes.
    fn app_request(_host: &mut Host<Self>, _index: usize, _request: AppOwnOf<Self>) {}

    /// Draw application window `view` laid out as `layout` into `surface`,
    /// as far as its clip admits.
    fn render_app(
        &mut self,
        surface: &mut Surface,
        view: &Self::AppView,
        layout: &AppLayoutOf<Self>,
        style: (&Theme, Scale, <Self::View as DocumentView>::Faces),
        focused: bool,
    );

    /// Draw `view` laid out as `layout` into `surface`, as far as its clip
    /// admits.
    fn render(
        &mut self,
        surface: &mut Surface,
        view: &Self::View,
        layout: &<Self::View as DocumentView>::Layout,
        style: (&Theme, Scale, <Self::View as DocumentView>::Faces),
        focused: bool,
    );
}

/// One window, and the document it shows.
pub struct DocWindow<A: DocumentApp> {
    /// The engine it shows.
    pub view: A::View,
    /// Where the engine draws, for the window's size.
    pub layout: LayoutOf<A>,
    /// What the engine reported changed since the window was last painted.
    pub damage: Region,
    /// What the application keeps for it.
    pub extra: A::Extra,
    chrome: Chrome,
    file: DocumentFile<Handle, SnapshotOf<A>>,
    /// Its document is being read in: input waits until it lands.
    loading: bool,
    /// Counts the documents it has shown, so an answer to the one before is
    /// told apart.
    epoch: u64,
    /// Its own work on the queue not yet taken in: the room it keeps on
    /// closing.
    own_held: usize,
    /// The menu open over it, so an outcome is matched to the gesture that
    /// asked for it.
    menu: Option<u64>,
    /// The tool windows it has open, as its view last asked.
    tools: Vec<ToolPane>,
    /// Whether its view was last told it has the keyboard: it has while the
    /// window or one of its tool windows does.
    focus_told: bool,
}

/// A tool window a document window has open: one more pane and surface,
/// showing `rect` of the window's drawing.
struct ToolPane {
    /// The view's name for it.
    id: u32,
    /// The part of the view's drawing it shows.
    rect: Rect,
    /// What the view reported changed inside `rect`, in the tool window's own
    /// pixels.
    damage: Region,
    chrome: Chrome,
}

impl ToolPane {
    /// `at` in the tool window's pixels, in the view's drawing.
    fn to_view(&self, at: Point) -> Point {
        Point::new(
            self.rect.left().saturating_add(at.x),
            self.rect.top().saturating_add(at.y),
        )
    }

    /// Take the part of `damage`, in the view's drawing, that lies in this
    /// tool window, owing a paint of it.
    fn take_damage(&mut self, damage: &Region) {
        for rect in damage.rects() {
            let inside = rect.intersection(&self.rect);
            if !inside.is_empty() {
                self.damage.add(Rect::new(
                    inside.left() - self.rect.left(),
                    inside.top() - self.rect.top(),
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
    /// Where it draws, for the window's size.
    pub layout: AppLayoutOf<A>,
    /// What it reported changed since the window was last painted.
    pub damage: Region,
    chrome: Chrome,
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

    /// Lay the window out again for its size and bring the view into line
    /// with it, owing what the view reported, and the whole when `whole`.
    fn lay_out(&mut self, theme: &Theme, scale: Scale, faces: FacesOf<A>, whole: bool) {
        let mode = *self.chrome.pane.mode();
        self.layout = self
            .view
            .layout(mode.width_px, mode.height_px, theme, scale, faces);
        self.view.settle(&self.layout, &mut self.damage);
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
    title: String,
    /// The title as it now reads, written here before it is compared with
    /// the one shown, so a paint builds no title of its own.
    title_draft: String,
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
            title,
            title_draft: String::new(),
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
    /// desktop refuses.
    fn resize(&mut self, client: &mut Client, width_px: u32, height_px: u32, name: &str) {
        let mode = app::mode_for(width_px, height_px);
        if !self.pane.resize_with(client, &mode, &mut self.surface) {
            report(
                name,
                "the desktop refused a resize; the window keeps its size",
            );
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
    /// it, its title first brought into line with the one in
    /// `title_draft`.
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
        if self.title_draft != self.title && client.set_title(self.id(), &self.title_draft).is_ok()
        {
            mem::swap(&mut self.title, &mut self.title_draft);
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
    pub const fn loading(&self) -> bool {
        self.loading
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
        self.chrome.focused || self.tools.iter().any(|tool| tool.chrome.focused)
    }

    /// Whether a document opened now may take its place.
    #[must_use]
    pub fn pristine(&self) -> bool {
        !self.loading && self.file.pristine(&self.view)
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

    /// Say `message` where the window shows what it is told.
    pub fn state(&mut self, message: impl Into<String>) {
        self.view.say(message.into());
        self.damage
            .add(<A::View as DocumentView>::message_area(&self.layout));
        self.owe_reported();
    }

    fn owe(&mut self, repaint: Repaint) {
        self.chrome.owe(repaint);
    }

    /// Lay the window out again for its size, bring the view into line with
    /// it, and owe it whole.
    fn relayout(&mut self, theme: &Theme, scale: Scale, faces: FacesOf<A>) {
        self.lay_out(theme, scale, faces);
        self.owe(Repaint::Whole);
        for tool in &mut self.tools {
            tool.chrome.owe(Repaint::Whole);
        }
    }

    /// Lay the window out again for its size and bring the view into line
    /// with it, owing only what the view reported moved.
    fn lay_out(&mut self, theme: &Theme, scale: Scale, faces: FacesOf<A>) {
        let mode = *self.chrome.pane.mode();
        self.layout = self
            .view
            .layout(mode.width_px, mode.height_px, theme, scale, faces);
        self.view.settle(&self.layout, &mut self.damage);
        self.owe_reported();
        self.owe_pointer_checks();
    }

    /// Owe the pointer's shape and tip a check over the window and each of
    /// its tool windows.
    fn owe_pointer_checks(&mut self) {
        self.chrome.pointing.owed = true;
        for tool in &mut self.tools {
            tool.chrome.pointing.owed = true;
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
            self.chrome.pointing.owed || self.tools.iter().any(|tool| tool.chrome.pointing.owed);
        if !owed {
            return;
        }
        let wanted = if self.loading {
            None
        } else {
            self.view.tool_tip(&self.layout, scale, theme)
        };
        let area = self.area();
        if mem::take(&mut self.chrome.pointing.owed) {
            let shape = match self.chrome.pointing.at {
                Some(_) if self.loading => CursorShape::Busy,
                Some(at) => self.view.cursor(&self.layout, at),
                None => self.chrome.pointing.shape,
            };
            let tip = wanted.filter(|(rect, _)| within(*rect, area));
            self.chrome.settle_pointer(client, shape, tip);
        }
        for tool in &mut self.tools {
            if mem::take(&mut tool.chrome.pointing.owed) {
                let shape = match tool.chrome.pointing.at {
                    Some(at) => self.view.cursor(&self.layout, at),
                    None => tool.chrome.pointing.shape,
                };
                let tip =
                    wanted
                        .filter(|(rect, _)| within(*rect, tool.rect))
                        .map(|(rect, text)| {
                            let left = rect.left() - tool.rect.left();
                            (
                                Rect::new(
                                    left,
                                    rect.top() - tool.rect.top(),
                                    rect.width,
                                    rect.height,
                                ),
                                text,
                            )
                        });
                tool.chrome.settle_pointer(client, shape, tip);
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
    fn paint(
        &mut self,
        client: &mut Client,
        mut render: impl FnMut(&mut Surface, &A::View, &LayoutOf<A>, bool),
    ) {
        if !self.tools.is_empty() {
            for tool in &mut self.tools {
                tool.take_damage(&self.damage);
            }
            self.damage.clip(self.area());
            if self.chrome.owed == Repaint::Reported && self.damage.is_empty() {
                self.chrome.owed = Repaint::Nothing;
            }
        }
        if self.chrome.owed != Repaint::Nothing {
            self.view.write_title(&mut self.chrome.title_draft);
        }
        let focused = self.focus_told;
        let (view, layout) = (&self.view, &self.layout);
        let painted = self.chrome.present(client, &mut self.damage, |surface, _| {
            render(surface, view, layout, focused);
        });
        self.chrome.presented(painted, A::NAME, view.name());
        for tool in &mut self.tools {
            let Some((x, y)) = tool.rect.surface_origin() else {
                continue;
            };
            let painted = tool.chrome.present(client, &mut tool.damage, |surface, _| {
                surface.with_origin(x, y, |surface| render(surface, view, layout, focused));
            });
            tool.chrome.presented(painted, A::NAME, view.name());
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

    /// Tell the view whether it has the keyboard.
    fn tell_focus(&mut self, focused: bool) {
        self.focus_told = focused;
        self.view
            .focus_changed(focused, &self.layout, &mut self.damage);
        self.owe_reported();
    }
}

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
    server: ProcId,
    /// Quit was chosen: the process ends once every window has closed and no
    /// save is left to land.
    quitting: bool,
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

    /// Say `message` as [`notify`](Self::notify) does, opening a window to
    /// say it in when none is open: the user acted, and expects one.
    fn tell(&mut self, message: String) {
        if self.windows.is_empty() {
            if let Ok(view) = A::untitled(self) {
                let _ = self.open_window(view, false);
            }
        }
        self.notify(message);
    }

    /// Open a window on a new document, or say why not.
    pub fn new_window(&mut self) {
        match A::untitled(self) {
            Ok(view) => {
                let _ = self.open_window(view, false);
            }
            Err(why) => self.tell(format!("No new window could be opened: {why}")),
        }
    }

    /// Open a window showing `view`, answering its index. A refusal is stated
    /// and answers `None`: the application carries on with the windows it
    /// has, and is still on the icon bar.
    pub fn open_view(&mut self, view: A::View) -> Option<usize> {
        self.open_window(view, false)
    }

    /// Show `view` in window `index` in place of what it showed, its document
    /// from `handle`, or from nowhere yet — in a window of its own when the
    /// document shown is not [`replaceable`](DocWindow::replaceable).
    pub fn show(&mut self, index: usize, view: A::View, handle: Option<Arc<Handle>>) {
        if !self.windows[index].replaceable() {
            if let Some(index) = self.open_window(view, false) {
                self.windows[index].file.opened(handle);
            }
            return;
        }
        self.move_on(index);
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.windows[index];
        window.view = view;
        window.file.opened(handle);
        window.loading = false;
        window.relayout(theme, scale, self.faces);
    }

    /// The document `name` could not be read into window `index`, which
    /// stays open — untitled, where a new document can be made, and saying
    /// why: an empty window under the file's name would read as an empty file.
    /// A document that is not [`replaceable`](DocWindow::replaceable) stays.
    pub fn not_opened(&mut self, index: usize, name: &str, why: &dyn Display) {
        let message = format!("{name} could not be opened: {why}");
        self.report(&message);
        if !self.windows[index].replaceable() {
            self.windows[index].state(message);
            return;
        }
        self.move_on(index);
        let fresh = A::untitled(self);
        let (theme, scale) = (self.themes.active(), self.desktop.scale());
        let window = &mut self.windows[index];
        if let Ok(view) = fresh {
            window.view = view;
        }
        window.view.say(message);
        window.file.opened(None);
        window.loading = false;
        window.relayout(theme, scale, self.faces);
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
            Relayout::Reported => window.lay_out(theme, scale, self.faces),
            Relayout::Whole => window.relayout(theme, scale, self.faces),
        }
        if let Some(request) = outcome.request {
            self.carry_out(index, request);
        }
    }

    /// Open a window showing `view`, `loading` when its document is still to
    /// be read in.
    fn open_window(&mut self, view: A::View, loading: bool) -> Option<usize> {
        let theme = self.themes.active();
        let scale = self.desktop.scale();
        let (width, height) = self.desktop.window_size(A::WINDOW_SIZE.0, A::WINDOW_SIZE.1);
        let mode = app::mode_for(width, height);
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            self.report("no drawing surface; no window opened");
            return None;
        };
        if self.queue.grow(A::JOBS_PER_WINDOW).is_err() {
            self.report("no room for another window's work; no window opened");
            return None;
        }
        if let Err(why) = self.app.reserve_window() {
            self.queue.shrink(A::JOBS_PER_WINDOW);
            self.report(format!("{why}; no window opened"));
            return None;
        }
        let least = view.min_size(theme, scale, self.faces);
        let mut title = String::new();
        view.write_title(&mut title);
        let opened = open_pane(
            &mut self.client,
            (self.event_endpoint, self.server),
            &mode,
            &title,
            least,
        );
        let pane = match opened {
            Ok(pane) => pane,
            Err(why) => {
                self.give_back_window();
                self.report(format!("{why}; no window opened"));
                return None;
            }
        };
        let layout = view.layout(mode.width_px, mode.height_px, theme, scale, self.faces);
        let mut window: DocWindow<A> = DocWindow {
            view,
            layout,
            damage: A::damage_sink(),
            extra: A::Extra::default(),
            chrome: Chrome::new(pane, surface, title),
            file: DocumentFile::new(),
            loading,
            epoch: 0,
            own_held: 0,
            menu: None,
            tools: Vec::new(),
            focus_told: true,
        };
        window.view.settle(&window.layout, &mut window.damage);
        self.windows.push(window);
        Some(self.windows.len() - 1)
    }

    /// Open a window of the application's own showing `view`, answering its
    /// index. A refusal is stated and answers `None`.
    pub fn open_app_window(&mut self, view: A::AppView) -> Option<usize> {
        let theme = self.themes.active();
        let scale = self.desktop.scale();
        let (width, height) = view.size();
        let (width, height) = self.desktop.window_size(width, height);
        let mode = app::mode_for(width, height);
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            self.report("no drawing surface; no window opened");
            return None;
        };
        let least = view.min_size(theme, scale, self.faces);
        let title = String::from(view.title());
        let opened = open_pane(
            &mut self.client,
            (self.event_endpoint, self.server),
            &mode,
            &title,
            least,
        );
        let pane = match opened {
            Ok(pane) => pane,
            Err(why) => {
                self.report(format!("{why}; no window opened"));
                return None;
            }
        };
        let layout = view.layout(mode.width_px, mode.height_px, theme, scale, self.faces);
        let mut window: AppWindow<A> = AppWindow {
            view,
            layout,
            damage: A::damage_sink(),
            chrome: Chrome::new(pane, surface, title),
        };
        window.view.settle(&window.layout, &mut window.damage);
        if self.app_windows.try_reserve(1).is_err() {
            let _ = window.chrome.pane.close(&mut self.client);
            self.report("no room to hold another window; no window opened");
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
        A::app_closing(self, index);
        let window = self.app_windows.remove(index);
        let _ = window.chrome.pane.close(&mut self.client);
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
            Relayout::Reported => window.lay_out(theme, scale, self.faces, false),
            Relayout::Whole => window.lay_out(theme, scale, self.faces, true),
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
    /// it answers later is told apart.
    fn move_on(&mut self, index: usize) {
        let withdrawn = self.withdraw(self.windows[index].id());
        let window = &mut self.windows[index];
        window.epoch = window.epoch.wrapping_add(1);
        window.own_held = window.own_held.saturating_sub(withdrawn);
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
                self.tell(format!("{name} could not be opened: {why}"));
                return;
            }
        };
        view.say(format!("Opening {name}\u{2026}"));
        let index = match into {
            Some(index) => {
                self.move_on(index);
                let (theme, scale) = (self.themes.active(), self.desktop.scale());
                let window = &mut self.windows[index];
                window.view = view;
                window.loading = true;
                window.relayout(theme, scale, self.faces);
                index
            }
            None => match self.open_window(view, true) {
                Some(index) => index,
                None => return,
            },
        };
        A::load(self, index, Arc::new(handle), name);
    }

    /// Close window `index`, its menu and any pick going with it. The work it
    /// asked for goes with it — all but its saves, which still land, the
    /// saves asked for behind the one in flight, written now, and the work
    /// the application still wants once the document has gone.
    fn close_window(&mut self, index: usize) {
        let mut window = self.windows.remove(index);
        let id = window.id();
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
                    "{target} could not be saved: there is no room for the save"
                ));
            }
        }
        for tool in window.tools.drain(..) {
            let _ = tool.chrome.pane.close(&mut self.client);
        }
        let _ = window.chrome.pane.close(&mut self.client);
    }

    /// Carry out one request of window `index`.
    fn carry_out(&mut self, index: usize, request: RequestOf<A>) {
        match request {
            Request::Save => self.save(index, None, false),
            Request::SaveThenClose => self.save(index, None, true),
            Request::SaveAs => self.ask_how(index, false),
            Request::SaveWhere { then_close } => self.ask_where(index, then_close),
            Request::Open => self.ask_pick(index, &PickPurpose::Open, PickFor::Open),
            Request::Close => self.close_window(index),
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
            Some((_, name)) => Some(name.as_str()),
            None => window.file.plain_destination().or_else(|| {
                window
                    .file
                    .writes_back(&window.view)
                    .then(|| window.view.name())
            }),
        };
        if let Some(refusal) = destination.and_then(|name| window.view.refuse_save(name)) {
            self.quitting = false;
            window.state(refusal);
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
            SaveStep::NoMemory => window.state("There is not enough memory to save"),
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
            self.report(format!("{name} could not be saved: {err}"));
        }
        let window = &mut self.windows[index];
        let landed = window
            .file
            .saved(&mut window.view, target, generation, rename, result);
        window
            .damage
            .add(<A::View as DocumentView>::message_area(&window.layout));
        window.owe_reported();
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
        match window
            .view
            .ask_how(then_close, &window.layout, &mut window.damage)
        {
            Some(outcome) => self.apply(index, outcome),
            None => self.ask_where(index, then_close),
        }
    }

    /// Ask the picker where to save window `index`, offering its name, or
    /// for a new document or one whose name cannot be written, a name in the
    /// form it would be written — held to the endings it can be written
    /// under, or saying why it can be written under none.
    fn ask_where(&mut self, index: usize, then_close: bool) {
        let window = &mut self.windows[index];
        let endings = match window.view.save_endings() {
            Ok(endings) => endings,
            Err(why) => {
                self.quitting = false;
                window.state(why);
                return;
            }
        };
        let view = &window.view;
        let offered = if view.access() == Access::Untitled {
            format!("{UNTITLED}.{}", view.offered_extension())
        } else if view.refuse_save(view.name()).is_some() {
            format!("{}.{}", stem(view.name()), view.offered_extension())
        } else {
            String::from(view.name())
        };
        let Ok(suggested) = DocumentName::new(&offered).or_else(|_| DocumentName::new(UNTITLED))
        else {
            return;
        };
        self.ask_pick(
            index,
            &PickPurpose::Save { suggested, endings },
            PickFor::SaveAs { then_close },
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
            Err(err) => window.state(format!("The desktop offered no file chooser ({err})")),
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
                window.state(format!(
                    "The chosen file could not be taken ({})",
                    Errno::from_syscall(raw)
                ));
                return;
            }
        };
        // A name the session no longer holds leaves the document named as it
        // was, or untitled.
        let name = self
            .client
            .take_picked_name(window_id)
            .ok()
            .filter(|name| !name.is_empty());
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
        }
    }

    /// Ask the session to open window `index`'s `kind` menu at `anchor`.
    fn open_menu(&mut self, index: usize, kind: MenuKindOf<A>, anchor: Rect) {
        let window = &mut self.windows[index];
        let Ok(region) =
            WindowRegion::new(anchor.left(), anchor.top(), anchor.width, anchor.height)
        else {
            return;
        };
        match self
            .client
            .open_menu(window.id(), region, &window.view.menu(kind))
        {
            Ok(open) => window.menu = Some(open),
            Err(err) => window.state(format!("The desktop composes no menu ({err})")),
        }
    }

    /// Take the one answer to window `index`'s open menu. An answer naming
    /// another gesture is one already settled, and is not acted on.
    fn menu_closed(&mut self, index: usize, open_id: u64, outcome: MenuOutcome) {
        let window = &mut self.windows[index];
        if window.menu != Some(open_id) {
            return;
        }
        window.menu = None;
        if window.loading {
            return;
        }
        let outcome = match outcome {
            MenuOutcome::Chosen(item) => {
                window.view.chosen(item, &window.layout, &mut window.damage)
            }
            MenuOutcome::Entered(item) => match self.client.take_menu_text(window.id(), open_id) {
                Ok(Some(text)) => {
                    window
                        .view
                        .entered(item, &text, &window.layout, &mut window.damage)
                }
                Ok(None) => return,
                Err(err) => {
                    window.state(format!("The entry could not be read ({err})"));
                    return;
                }
            },
            MenuOutcome::Refused(reason) => {
                window.state(format!("No menu was shown: {}", reason.describe()));
                return;
            }
            MenuOutcome::Dismissed => return,
        };
        self.apply(index, outcome);
    }

    /// Window `index`'s pointer is at `at` in its view's drawing — over the
    /// window, or over its tool window `tool` — with `modifiers` held, and
    /// `inputs` are what it did there: note where it is, owe the tip and
    /// cursor beneath it a check, and feed the view the modifiers and then
    /// `inputs` unless the window is still loading.
    fn point(
        &mut self,
        (index, tool): (usize, Option<usize>),
        at: Point,
        modifiers: AbiModifiers,
        inputs: impl Iterator<Item = InputEvent>,
    ) {
        let window = &mut self.windows[index];
        let pointing = match tool.and_then(|tool| window.tools.get_mut(tool)) {
            Some(tool) => &mut tool.chrome.pointing,
            None => &mut window.chrome.pointing,
        };
        pointing.at = Some(at);
        pointing.owed = true;
        if window.loading {
            return;
        }
        let held = key_input_event(KeyInput::ModifiersChanged { modifiers });
        self.feed(index, core::iter::once(held).chain(inputs));
    }

    fn feed(&mut self, index: usize, inputs: impl Iterator<Item = InputEvent>) {
        let id = self.windows[index].id();
        let now = tairix_rt::clock_get();
        for input in inputs {
            let Some(index) = self.index_of(id) else {
                return;
            };
            let (theme, scale) = (self.themes.active(), self.desktop.scale());
            let window = &mut self.windows[index];
            let outcome = window.view.input(
                &input,
                now,
                &window.layout,
                scale,
                theme,
                &mut window.damage,
            );
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
                    Some(at) => window.view.cursor(&window.layout, at),
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
            WindowEvent::AppBarMenu { item } if is_quit(*item) => {
                self.quit();
                return;
            }
            WindowEvent::AppBarMenu { item } => {
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
                self.drain_open_targets();
                return;
            }
            _ => {}
        }
        let id = event.window_id();
        let Some(index) = id.and_then(|id| self.index_of(id)) else {
            if let Some(index) = id.and_then(|id| self.app_index_of(id)) {
                self.act_app(index, event);
            } else if let Some(tool) = id.and_then(|id| self.tool_of(id)) {
                self.act_tool(tool, event);
            } else if let WindowEvent::FilePicked { handle, .. } = event {
                // A file chosen for a window since closed is let go at once,
                // not left held for the life of the process.
                release(*handle);
            }
            return;
        };
        self.act(index, event);
        if self.quitting && self.quit_abandoned() {
            self.quitting = false;
        }
    }

    /// Route one window-scoped event to window `index`.
    fn act(&mut self, index: usize, event: &WindowEvent) {
        let window = &mut self.windows[index];
        match event {
            WindowEvent::CloseRequested { .. } | WindowEvent::AlternateCloseRequested { .. } => {
                if window.loading {
                    self.close_window(index);
                    return;
                }
                let outcome = window
                    .view
                    .close_requested(&window.layout, &mut window.damage);
                self.apply(index, outcome);
            }
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => {
                window
                    .chrome
                    .resize(&mut self.client, *width_px, *height_px, A::NAME);
                window.relayout(self.themes.active(), self.desktop.scale(), self.faces);
            }
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
            // A document app asks for no folder, so a folder answer ends its
            // pick as a cancellation does.
            WindowEvent::PickCancelled { .. } | WindowEvent::FolderPicked { .. } => {
                let _ = window.file.end_pick();
            }
            WindowEvent::MenuClosed {
                open_id, outcome, ..
            } => self.menu_closed(index, *open_id, *outcome),
            WindowEvent::Key { key, .. } => {
                if !window.loading {
                    self.feed(index, core::iter::once(key_input_event(*key)));
                }
            }
            WindowEvent::Pointer {
                x,
                y,
                action,
                modifiers,
                ..
            } => {
                let at = pointer_point(*x, *y);
                self.point(
                    (index, None),
                    at,
                    *modifiers,
                    pointer_input_events(*action, at),
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
                self.point((index, None), at, *modifiers, pinch);
            }
            WindowEvent::Minimized { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
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
                window.lay_out(theme, scale, self.faces, true);
            }
            WindowEvent::RedrawRequested { .. } => window.chrome.owe(Repaint::Whole),
            WindowEvent::ContentReleased { .. } => window.chrome.pane.release_frames(),
            WindowEvent::Focus { focused, .. } => {
                window.chrome.focused = *focused;
                window
                    .view
                    .focus_changed(*focused, &window.layout, &mut window.damage);
                window.owe_reported();
            }
            // It asks for no file; one handed to it anyway is let go.
            WindowEvent::FilePicked { handle, .. } => release(*handle),
            WindowEvent::Key { key, .. } => {
                self.feed_app(index, core::iter::once(key_input_event(*key)));
            }
            WindowEvent::Pointer {
                x,
                y,
                action,
                modifiers,
                ..
            } => {
                let at = pointer_point(*x, *y);
                self.point_app(index, at, *modifiers, pointer_input_events(*action, at));
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
                self.point_app(index, at, *modifiers, scroll_input_events(at, *dx, *dy));
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
                self.point_app(index, at, *modifiers, pinch);
            }
            WindowEvent::PickCancelled { .. }
            | WindowEvent::FolderPicked { .. }
            | WindowEvent::MenuClosed { .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::PreviewRendered { .. } => {}
        }
    }

    /// The document window and the place among its tool windows of the tool
    /// window the session calls `id`.
    fn tool_of(&self, id: u64) -> Option<(usize, usize)> {
        self.windows.iter().enumerate().find_map(|(index, window)| {
            window
                .tools
                .iter()
                .position(|tool| tool.chrome.id() == id)
                .map(|tool| (index, tool))
        })
    }

    /// Route one window-scoped event to tool window `tool` of document window
    /// `index`: its input is the window's own, in the window's drawing; its
    /// close mark and its moves are its view's to answer.
    fn act_tool(&mut self, (index, tool): (usize, usize), event: &WindowEvent) {
        let window = &mut self.windows[index];
        let pane = &mut window.tools[tool];
        let id = pane.id;
        match event {
            WindowEvent::CloseRequested { .. } | WindowEvent::AlternateCloseRequested { .. } => {
                let outcome =
                    window
                        .view
                        .tool_gone(id, ToolGone::Closed, &window.layout, &mut window.damage);
                self.apply(index, outcome);
            }
            WindowEvent::ToolMoved { over, ended, .. } => {
                let over = match *over {
                    ToolOver::Parent { x, y } => Some(pointer_point(x, y)),
                    ToolOver::Elsewhere => None,
                };
                let (theme, scale) = (self.themes.active(), self.desktop.scale());
                let moved = ToolMove {
                    id,
                    over,
                    ended: *ended,
                };
                let outcome =
                    window
                        .view
                        .tool_moved(moved, &window.layout, scale, theme, &mut window.damage);
                self.apply(index, outcome);
            }
            WindowEvent::Focus { focused, .. } => {
                pane.chrome.focused = *focused;
                window.note_focus();
            }
            WindowEvent::RedrawRequested { .. } => pane.chrome.owe(Repaint::Whole),
            WindowEvent::ContentReleased { .. } => pane.chrome.pane.release_frames(),
            // It asks for no file; one handed to it anyway is let go.
            WindowEvent::FilePicked { handle, .. } => release(*handle),
            WindowEvent::Key { key, .. } => {
                if !window.loading {
                    self.feed(index, core::iter::once(key_input_event(*key)));
                }
            }
            WindowEvent::Pointer {
                x,
                y,
                action,
                modifiers,
                ..
            } => {
                let at = pane.to_view(pointer_point(*x, *y));
                let inputs = pointer_input_events(*action, at);
                self.point((index, Some(tool)), at, *modifiers, inputs);
            }
            WindowEvent::Scrolled {
                x,
                y,
                dx,
                dy,
                modifiers,
                ..
            } => {
                let at = pane.to_view(pointer_point(*x, *y));
                let inputs = scroll_input_events(at, *dx, *dy);
                self.point((index, Some(tool)), at, *modifiers, inputs);
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
                self.point((index, Some(tool)), at, *modifiers, pinch);
            }
            // A tool window is never resized but by its view, never minimised
            // but with its window, and asks for no picker, menu or preview.
            WindowEvent::Resized { .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::PickCancelled { .. }
            | WindowEvent::FolderPicked { .. }
            | WindowEvent::MenuClosed { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::PreviewRendered { .. } => {}
        }
    }

    /// Open, resize, retitle and close every document window's tool windows
    /// to match what its view now asks for.
    fn settle_tools(&mut self) {
        let mut index = 0;
        while index < self.windows.len() {
            self.settle_window_tools(index);
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
        let mut at = window.tools.len();
        while let Some(back) = at.checked_sub(1) {
            at = back;
            let id = window.tools[at].id;
            let wanted = (0..)
                .map_while(|place| window.view.tool_window(&window.layout, place))
                .any(|wanted| wanted.id == id && wanted.fits_beside(area));
            if !wanted {
                let _ = window.tools.remove(at).chrome.pane.close(&mut self.client);
            }
        }
        let window_id = window.id();
        let mut refused: Vec<u32> = Vec::new();
        let mut place = 0;
        loop {
            let window = &mut self.windows[index];
            let Some(wanted) = window.view.tool_window(&window.layout, place) else {
                return;
            };
            place += 1;
            let (id, rect) = (wanted.id, wanted.rect);
            if !wanted.fits_beside(area) || refused.contains(&id) {
                continue;
            }
            if let Some(tool) = window.tools.iter_mut().find(|tool| tool.id == id) {
                if (tool.rect.width, tool.rect.height) != (rect.width, rect.height) {
                    tool.chrome
                        .resize(&mut self.client, rect.width, rect.height, A::NAME);
                    tool.chrome.owe(Repaint::Whole);
                } else if tool.rect.origin != rect.origin {
                    tool.chrome.owe(Repaint::Whole);
                }
                tool.rect = rect;
                if tool.chrome.title != wanted.title {
                    tool.chrome.title_draft.clear();
                    tool.chrome.title_draft.push_str(wanted.title);
                    tool.chrome.owe(Repaint::Whole);
                }
                continue;
            }
            let title = String::from(wanted.title);
            let opening = window.view.tool_opening(id);
            let parent = (window_id, self.server);
            let opened = open_tool(
                &mut self.client,
                parent,
                self.event_endpoint,
                rect,
                opening,
                title,
            )
            .and_then(|chrome| {
                if window.tools.try_reserve(1).is_ok() {
                    return Ok(chrome);
                }
                let _ = chrome.pane.close(&mut self.client);
                Err(String::from("no room to hold another palette"))
            });
            match opened {
                Ok(chrome) => window.tools.push(ToolPane {
                    id,
                    rect,
                    damage: A::damage_sink(),
                    chrome,
                }),
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

    /// The desktop would not open window `index`'s tool window `id`, for
    /// `why`: say so, and hand it back to the view.
    fn tool_refused(&mut self, index: usize, id: u32, why: &str) {
        self.report(format!(
            "a palette could not float ({why}); it stays in its window"
        ));
        let window = &mut self.windows[index];
        let outcome =
            window
                .view
                .tool_gone(id, ToolGone::Refused, &window.layout, &mut window.damage);
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
    ) {
        let window = &mut self.app_windows[index];
        window.chrome.pointing.at = Some(at);
        window.chrome.pointing.owed = true;
        let held = key_input_event(KeyInput::ModifiersChanged { modifiers });
        self.feed_app(index, core::iter::once(held).chain(inputs));
    }

    fn feed_app(&mut self, index: usize, inputs: impl Iterator<Item = InputEvent>) {
        let id = self.app_windows[index].id();
        let now = tairix_rt::clock_get();
        for input in inputs {
            let Some(index) = self.app_index_of(id) else {
                return;
            };
            let (theme, scale) = (self.themes.active(), self.desktop.scale());
            let window = &mut self.app_windows[index];
            let outcome = window.view.input(
                &input,
                now,
                &window.layout,
                scale,
                theme,
                &mut window.damage,
            );
            self.apply_app(index, outcome);
        }
    }

    /// Quit: every window closes, and one holding changes asks first.
    fn quit(&mut self) {
        self.quitting = true;
        let ids: Vec<u64> = self.windows.iter().map(DocWindow::id).collect();
        for id in ids {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            let window = &mut self.windows[index];
            if window.loading || !window.view.is_modified() {
                self.close_window(index);
            } else if !window.view.asking_to_close() && !window.file.closing() {
                let outcome = window
                    .view
                    .close_requested(&window.layout, &mut window.damage);
                self.apply(index, outcome);
            }
        }
    }

    /// Close every application window.
    fn close_app_windows(&mut self) {
        while let Some(last) = self.app_windows.len().checked_sub(1) {
            self.close_app_window(last);
        }
    }

    /// Whether the user turned a quit down: a window they were asked about is
    /// still open, not asking to close, with nothing about to close it.
    fn quit_abandoned(&self) -> bool {
        self.windows.iter().any(|window| {
            !window.view.asking_to_close() && !window.file.closing() && !window.file.picking()
        })
    }

    /// Open a window at every document the desktop has handed this instance.
    fn drain_open_targets(&mut self) {
        loop {
            let target = match self.client.take_open_target() {
                Ok(Some(target)) => target,
                Ok(None) => return,
                Err(err) => {
                    self.report(format!("cannot take an open target ({err})"));
                    return;
                }
            };
            match target {
                Target::Document {
                    name,
                    grant,
                    writable,
                } => match Handle::from_delegation(grant) {
                    Ok(file) => self.open_document(file, name, writable, None),
                    Err(raw) => self.tell(format!(
                        "{name} could not be opened: it could not be taken over ({})",
                        Errno::from_syscall(raw)
                    )),
                },
                // Unreachable from the desktop, which hands a document
                // application an open document because it holds no authority
                // to open a name.
                Target::Path(path) => self.report(format!(
                    "{path} was handed over as a path; {} holds no filesystem authority and \
                     can only be given an open document",
                    A::NAME
                )),
                Target::Pane(pane) => self.report(format!(
                    "{pane} was handed over, but {} has no places to go to",
                    A::NAME
                )),
            }
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
                        self.report(format!("{} could not be saved: {err}", saved.name));
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

    /// Present what every window owes. A present the session refuses is that
    /// window's trouble alone: it is said once, and the window paints whole at
    /// its next chance, while every other window carries on.
    fn paint_all(&mut self) {
        let theme = self.themes.active();
        let scale = self.desktop.scale();
        let faces = self.faces;
        let app = &mut self.app;
        for window in &mut self.windows {
            window.paint(&mut self.client, |surface, view, layout, focused| {
                app.render(surface, view, layout, (theme, scale, faces), focused);
            });
        }
        for window in &mut self.app_windows {
            if window.chrome.owed != Repaint::Nothing {
                window.chrome.title_draft.clear();
                window.chrome.title_draft.push_str(window.view.title());
            }
            let (view, layout) = (&window.view, &window.layout);
            let painted =
                window
                    .chrome
                    .present(&mut self.client, &mut window.damage, |surface, focused| {
                        app.render_app(surface, view, layout, (theme, scale, faces), focused);
                    });
            window
                .chrome
                .presented(painted, A::NAME, window.view.title());
        }
    }

    /// Adopt a new desktop state: every window restyled at its density.
    fn adopt_desktop(&mut self) {
        match app::adopt_desktop(&mut self.desktop, &mut self.themes) {
            Ok(true) => {
                let theme = self.themes.active();
                let scale = self.desktop.scale();
                self.faces = A::faces(theme, scale);
                for window in &mut self.windows {
                    window.relayout(theme, scale, self.faces);
                }
                for window in &mut self.app_windows {
                    window.lay_out(theme, scale, self.faces, true);
                }
            }
            Ok(false) => {}
            Err(err) => self.report(format!("desktop change refused: {err}")),
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

/// `name` without the extension or RISC OS file type that ends it.
fn stem(name: &str) -> &str {
    match name.rfind(['.', ',']) {
        Some(at) if at > 0 => &name[..at],
        _ => name,
    }
}

/// Open a resizable pane of `mode` titled `title`, held to at least `least`,
/// its events sent to `endpoint` and its reply taken from `server` alone,
/// answering why where it could not be.
fn open_pane(
    client: &mut Client,
    (endpoint, server): (u64, ProcId),
    mode: &DisplayMode,
    title: &str,
    least: (u32, u32),
) -> Result<WindowPane, String> {
    let sizing = WindowSizing::Resizable {
        min_width_px: least.0,
        min_height_px: least.1,
        max_width_px: 0,
        max_height_px: 0,
    };
    match WindowPane::open(client, endpoint, mode, title, sizing) {
        // A reply from any other sender is something else answering for the
        // window endpoint.
        Ok((pane, replied)) if replied == server => Ok(pane),
        Ok((pane, _)) => {
            let _ = pane.close(client);
            Err(String::from("a window reply came from another sender"))
        }
        Err(err) => Err(format!("{err}")),
    }
}

/// Open a tool window over `rect` of window `parent`'s drawing, titled
/// `title` and opening as `opening` says, its events sent to `endpoint` and
/// its reply taken from the session alone; it opens without the keyboard.
fn open_tool(
    client: &mut Client,
    parent: (u64, ProcId),
    endpoint: u64,
    rect: Rect,
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
        &mode,
        opening.offset,
        carry,
        &title,
    )
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
    let item = |id: u16, label: &str| -> Result<AppMenuRow, Errno> {
        Ok(AppMenuRow::Item(AppMenuItem::new(
            AppMenuItemId::new(id)?,
            AppMenuLabel::new(label)?,
        )))
    };
    let mut rows = Vec::new();
    rows.try_reserve_exact(1 + A::BAR_ROWS.len())
        .map_err(|_| Errno::OutOfMemory)?;
    rows.push(item(NEW_WINDOW_ROW, "New window")?);
    for (index, label) in A::BAR_ROWS.iter().enumerate() {
        let id = u16::try_from(index)
            .ok()
            .and_then(|index| FIRST_APP_ROW.checked_add(index))
            .ok_or(Errno::OutOfRange)?;
        rows.push(item(id, label)?);
    }
    declaration(endpoint, AppBarClick::RaiseOrOpen, &rows)
}

/// Start `worker`, the application `name`'s `what` worker, and watch its
/// answer wake on `set` under `token`. A worker the machine does not grant
/// leaves its work on the loop, which is said.
///
/// # Errors
///
/// The exit code to end with when the wake cannot be watched, which is
/// stated: an answer nothing wakes the loop for is never taken in.
pub fn start_worker<S, Req, Ans, D>(
    name: &str,
    worker: &Arc<Worker<S, Req, Ans, D>>,
    set: u64,
    token: u64,
    what: &str,
) -> Result<(), i32>
where
    S: Send + 'static,
    Req: Send + 'static,
    Ans: Send + 'static,
    D: Desk<Req, Ans> + Send + 'static,
{
    if let Err(reason) = Worker::start(worker) {
        report(
            name,
            format!("no {what} worker ({reason:?}); its work runs on the event loop"),
        );
    }
    app::watch_wake(set, worker.wake(), token).map_err(|err| {
        fail(
            name,
            app::EXIT_NO_EVENTS,
            format!("{what} answer wake refused ({err})"),
        )
    })
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
        let woken = match self.signals.deadline_ns.get() {
            Some(deadline) => match app::park_until(self.set, deadline)? {
                Some(woken) => woken,
                None => return Ok(Parked::Interrupted),
            },
            None => app::park(self.set)?,
        };
        match woken {
            Wake::App(QUEUE_TOKEN) => {
                self.queue.drain();
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
        let now = tairix_rt::clock_get();
        let answered = A::turn(self, now) | self.answered;
        self.settle_tools();
        self.settle_focus();
        self.settle_pointers();
        self.paint_all();
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
            signals.deadline_ns.set(A::deadline(self, now));
            let polled = events.wait(&mut self.client);
            self.take_event(polled)?;
        }
        ControlFlow::Continue(())
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
    if let Err(code) = start_worker(A::NAME, &queue, set, QUEUE_TOKEN, "work") {
        return code;
    }
    let _queue_guard = WorkerGuard::new(&queue);
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
        answered: false,
        saves: 0,
        unroomed: 0,
        event_endpoint: endpoint,
        server,
        quitting: false,
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
        wakes: &wakes,
        signals: &signals,
    });
    host.serve(&mut events, &signals)
}
