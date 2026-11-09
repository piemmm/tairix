//! The app-side shell every windowed `Run` binary composes.
//!
//! Bringing a windowed application up is the same sequence every time: one
//! `ipc_call` transport to the reserved window endpoint, one `port_bind`-bound
//! event mailbox parked on through a wait-set that also carries the machine's
//! memory-pressure band and the desktop's own state, the
//! screen/density/appearance queried before anything is sized, and one
//! retained drawing surface behind a shared frame region that survives
//! resizes and releases. It lives here beside the
//! rest of the app half of the channel so there is one of each rather than one
//! per bundle.
//!
//! # One window or many
//!
//! [`WindowPane`] is one window's channel-side state — its id, its region, its
//! layout — and the protocol over it. [`AppWindow`] pairs one with the retained
//! surface and takes the paint as a closure, which is what a single-window app
//! wants; an app that opens a window per document, or a popup above one, holds
//! panes itself and keeps whatever retained picture it actually paints from.
//!
//! # What stays with the application
//!
//! Its own wait-set members and the tokens for them ([`FIRST_APP_TOKEN`]
//! onward), what it *does* about a pressure-band change or a desktop change,
//! and what it paints.
//! The shell owns the frame-region and damage bookkeeping without owning a
//! single pixel of anyone's window.

use core::fmt;

use alloc::sync::Arc;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayFormat, DisplayMode};
use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
use tairix_abi::window_ipc::{
    AlertSeverity, DialogModality, LayerDepth, WindowEvent, WindowSizing, WindowTitle,
    WINDOW_ENDPOINT,
};
use tairix_abi::{Errno, WaitSetOp, WaitSourceKind, CALL_DEADLINE_NONE};
use tairix_raster::Surface;
use tairix_rt::shm::SharedRegion;
use tairix_rt::work::{Desk, Worker};
use tairix_theme::{Accessibility, ThemeRegistry};
use tairix_visible::Shown;

use crate::client::{
    notice_alert, presented_damage, retained_damage, Answer, PayloadRegion, PostedRequest,
    PresentState, Refused, WindowClient, WindowTransport,
};
use crate::desktop::Desktop;
use crate::frames::{draw_frame, granted_region, FrameRing, WindowFrames};
pub use crate::frames::{region_bytes, FRAME_COUNT};
use crate::server::{DialogSpec, LayerSpec, PopupSpec, ToolSpec, TransientSpec};

/// Exit code when the shared frame region could not be created or granted to
/// the window endpoint. A reserved, fail-closed value.
pub const EXIT_NO_FRAMES: i32 = 81;

/// Exit code when the event mailbox could not be bound or observed through the
/// wait-set. A reserved, fail-closed value: the app exits rather than degrade
/// into a busy re-poll.
pub const EXIT_NO_EVENTS: i32 = 82;

/// Exit code when the desktop session refused the window create (no graphical
/// session, or the channel refused the geometry). A reserved, fail-closed
/// value.
pub const EXIT_NO_WINDOW: i32 = 83;

/// Exit code when a present was refused or the event channel died (the session
/// went away). A reserved, fail-closed value.
pub const EXIT_CHANNEL_LOST: i32 = 84;

/// The wait-set token of the event-mailbox member.
pub const EVENT_TOKEN: u64 = 1;

/// The wait-set token of the memory-pressure member: the kernel wakes the park
/// when the machine's pressure band changes, so a cache is trimmed as memory
/// tightens instead of being held until something else is starved.
pub const PRESSURE_TOKEN: u64 = 2;

/// The wait-set token of the desktop-notice member: the kernel wakes the park
/// when the session publishes a new screen extent, density, or appearance, so
/// a window follows a light/dark switch instead of sitting in the appearance
/// the user just left.
pub const DESKTOP_TOKEN: u64 = 3;

/// The wait-set token of the window-reply member: the kernel wakes the park
/// when a request the client posted to the session is answered, and keeps it
/// ready until the event drain takes the answer in
/// ([`crate::client::WindowEvents::try_wait`], which
/// [`crate::client::WindowEvents::wait`] runs before every park).
pub const REPLY_TOKEN: u64 = 4;

/// The lowest token an application may give a wait-set member of its own.
///
/// The shell's own members hold every value below it, so an app numbering from
/// here cannot collide with them however many it adds.
pub const FIRST_APP_TOKEN: u64 = 5;

/// A bring-up refusal: the reserved exit code, the reason to state, and the
/// typed error a caller that must answer in [`Errno`] hands on.
///
/// The reason carries no application name — the app prefixes its own, which is
/// the one part of a diagnostic that genuinely differs per bundle. The errno is
/// carried rather than derived from the exit code, because one code covers
/// several distinct refusals: a window already being open and a surface that
/// could not be allocated are both [`EXIT_NO_WINDOW`], and flattening them to
/// one errno would report an out-of-memory as a programming mistake or the
/// reverse.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShellError {
    code: i32,
    reason: &'static str,
    errno: Errno,
}

impl ShellError {
    const fn new(code: i32, reason: &'static str, errno: Errno) -> Self {
        Self {
            code,
            reason,
            errno,
        }
    }

    /// The reserved exit code to hand back from `main`.
    #[must_use]
    pub const fn code(&self) -> i32 {
        self.code
    }

    /// The typed error, for a caller whose own contract answers in [`Errno`]
    /// rather than an exit code.
    #[must_use]
    pub const fn errno(&self) -> Errno {
        self.errno
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason, self.errno)
    }
}

/// State `reason` on `stderr` under the application's `name`, each hidden
/// character in it shown as U+FFFD: a reason quotes names from elsewhere, and
/// a terminal must never take one for a command.
pub fn report(name: &str, reason: impl fmt::Display) {
    tairix_rt::io::write_stderr_line(format_args!("{name}: {}", Shown(reason)));
}

/// State one ready-made diagnostic `line` on `stderr`, shown as [`report`]
/// shows a reason; only its own line ending is kept, so no name in it can
/// start a line of its own.
pub fn report_line(line: &str) {
    tairix_rt::io::write_stderr_line(Shown(line.strip_suffix('\n').unwrap_or(line)));
}

/// State on `stderr` under `name`, and in an error alert holding window
/// `owner` when it is some, that what `headline` names was not done because
/// of `detail`: the user acted, and is owed the answer where they can see it.
/// An alert that could not be asked for is reported, and its id is `None`;
/// one the desktop will not put up is refused later, as any request is.
pub fn refuse<T: WindowTransport>(
    client: &mut WindowClient<T>,
    (name, event_endpoint): (&str, u64),
    owner: Option<u64>,
    headline: &str,
    detail: &str,
) -> Option<u64> {
    report(name, format_args!("{headline}: {detail}"));
    let raised = notice_alert(AlertSeverity::Error, headline, detail)
        .and_then(|alert| client.open_alert(event_endpoint, owner, &alert));
    if let Err(err) = raised {
        report(
            name,
            format_args!("the desktop would not show that ({err})"),
        );
    }
    raised.ok()
}

/// A worker pool with one participant per `online` CPU
/// ([`tairix_procinfo::online_count`]), the calling thread among them, for
/// work described as `doing`.
///
/// Fewer workers than the machine has cores is a refusal stated under `name`:
/// the work still gets done, more slowly than the hardware allows.
#[must_use]
pub fn pool_across(name: &str, online: usize, doing: &str) -> tairix_parallel::Pool {
    let pool = tairix_parallel::Pool::for_cpus(online);
    let wanted = online.saturating_sub(1);
    if pool.worker_count() < wanted {
        report(
            name,
            format_args!(
                "{doing} on {} of {online} cores (the kernel granted {} of {wanted} threads)",
                pool.worker_count().saturating_add(1),
                pool.worker_count()
            ),
        );
    }
    pool
}

/// State the abnormal-exit `reason` under `name` and hand `code` back.
pub fn fail(name: &str, code: i32, reason: impl fmt::Display) -> i32 {
    report(name, reason);
    code
}

/// What an application states when a window keeps its size
/// ([`Fitted::Kept`]).
pub const SIZE_KEPT: &str = "the window's new size could not be made; it keeps the size it had";

/// What a window did with a size it was asked to take.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fitted {
    /// It has the size: repaint at it.
    Adopted,
    /// An earlier resize is unanswered, or this process's own calls fill its
    /// place at the session; the size is delivered again as a `Resized` event
    /// once one is answered. Nothing to report.
    Deferred,
    /// It keeps the size it had and stays drawable: no region could be made
    /// for the new one, or the session could not be asked.
    Kept,
}

/// The production [`WindowTransport`]: the reserved window endpoint, called
/// or posted to.
///
/// The session attests the caller kernel-side on every request, so the
/// transport carries no claimed authority.
pub struct RtWindowTransport;

impl WindowTransport for RtWindowTransport {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(WINDOW_ENDPOINT, request, reply).map_err(Errno::from_syscall)
    }

    fn post(&mut self, request: &[u8]) -> Result<u64, Errno> {
        tairix_rt::call_post(WINDOW_ENDPOINT, request, CALL_DEADLINE_NONE)
            .map_err(Errno::from_syscall)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        match tairix_rt::call_reap(WINDOW_ENDPOINT, ticket, reply) {
            Ok(len) => Ok(Some(len)),
            Err(ret) => match Errno::from_syscall(ret) {
                Errno::WouldBlock => Ok(None),
                err => Err(err),
            },
        }
    }

    fn await_reply(&mut self, ticket: u64, reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::call_await(WINDOW_ENDPOINT, ticket, reply)
    }

    type Region = SharedRegion;

    fn grant_region(&mut self, len: usize) -> Result<(SharedRegion, u64), Errno> {
        granted_region(len).ok_or(Errno::OutOfMemory)
    }
}

impl PayloadRegion for SharedRegion {
    fn bytes_mut(&mut self) -> &mut [u8] {
        SharedRegion::bytes_mut(self)
    }
}

/// What a park woke for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Wake {
    /// The event mailbox has a frame to drain.
    Event,
    /// The machine's memory-pressure band changed, so a cache should be
    /// trimmed. What to hand back is the application's own business — several
    /// hold more than a glyph cache.
    PressureChanged,
    /// The pressure member woke but the band is unchanged: nothing is owed.
    PressureUnchanged,
    /// The session published a new desktop state. What the application owes
    /// is [`adopt_desktop`], which answers whether anything it draws from
    /// actually moved — the read is the app's because the [`Desktop`] is.
    DesktopChanged,
    /// One of the application's own members, by its token.
    App(u64),
}

impl Wake {
    /// Whether this wake gives way to a deadline that has come by `now()`: an
    /// event or an answer stays queued for the drain, and an unchanged band
    /// owes nothing, so the loop's due frame is served first and a member left
    /// ready can never hold it back. The clock is read only for such a wake.
    pub fn yields_to_due(self, deadline_ns: u64, now: impl FnOnce() -> u64) -> bool {
        let queued = matches!(self, Self::Event | Self::PressureUnchanged);
        crate::park::deadline_first(queued, deadline_ns, now)
    }
}

fn classify(token: u64) -> Wake {
    match token {
        // An answer to a posted request is taken in by the same drain that
        // reads events, so it wakes the loop as one.
        EVENT_TOKEN | REPLY_TOKEN => Wake::Event,
        PRESSURE_TOKEN => {
            if tairix_procinfo::pressure::refresh() {
                Wake::PressureChanged
            } else {
                Wake::PressureUnchanged
            }
        }
        DESKTOP_TOKEN => Wake::DesktopChanged,
        other => Wake::App(other),
    }
}

/// Park on `set` until a member is ready, and say which.
///
/// # Errors
///
/// The kernel's refusal, which for a live set handle means the set was torn
/// down under the caller. Never a timeout: this park has no deadline.
pub fn park(set: u64) -> Result<Wake, Errno> {
    let mut token = 0u64;
    let rc = tairix_rt::waitset_wait(set, u64::MAX, &mut token);
    if rc == 0 {
        return Ok(classify(token));
    }
    Err(Errno::from_syscall(rc))
}

/// Park on `set` until a member is ready or `budget_ns` **elapses from now**,
/// answering `None` when the budget ran out.
///
/// The budget is a duration, not an instant: `u64::MAX` is the way to say "no
/// deadline at all". A caller holding the *instant* its next event is due wants
/// [`park_until`] instead, which is the same park with the arithmetic done for
/// it — handing an absolute clock reading to this one parks for the machine's
/// whole uptime.
///
/// One-shot by construction: the budget is the caller's next due event, so a
/// loop that has nothing pending passes `u64::MAX` and the CPU is given up
/// entirely.
///
/// # Errors
///
/// The kernel's refusal. An exhausted budget is `Ok(None)`, not an error.
pub fn park_for(set: u64, budget_ns: u64) -> Result<Option<Wake>, Errno> {
    let mut token = 0u64;
    let rc = tairix_rt::waitset_wait(set, budget_ns, &mut token);
    if rc == 0 {
        return Ok(Some(classify(token)));
    }
    match Errno::from_syscall(rc) {
        Errno::TimedOut => Ok(None),
        err => Err(err),
    }
}

/// Park on `set` until a member is ready or the monotonic clock reaches
/// `deadline_ns`, answering `None` on the deadline.
///
/// The absolute-deadline half of [`park_for`], for the common loop that knows
/// *when* its next frame or tick is due rather than how long away it is. The
/// clock read and the subtraction live here so no caller repeats them, and so
/// none can hand an instant to a call that wanted a duration.
///
/// A wake that leaves its work queued ([`Wake::yields_to_due`]) is answered as
/// the deadline once that has come, and the queued work is taken in on the
/// loop's next turn.
///
/// # Errors
///
/// As [`park_for`].
pub fn park_until(set: u64, deadline_ns: u64) -> Result<Option<Wake>, Errno> {
    let woken = park_for(
        set,
        crate::park::remaining_ns(deadline_ns, tairix_rt::clock_get()),
    )?;
    Ok(woken.filter(|wake| !wake.yields_to_due(deadline_ns, tairix_rt::clock_get)))
}

/// [`park_until`] for a loop that may owe no wake: with no deadline the park
/// gives the CPU up until a member is ready. `None` is the deadline reached.
///
/// # Errors
///
/// As [`park_for`].
pub fn park_until_due(set: u64, due_ns: Option<u64>) -> Result<Option<Wake>, Errno> {
    match due_ns {
        Some(deadline_ns) => park_until(set, deadline_ns),
        None => park(set).map(Some),
    }
}

/// The app's bound event mailbox endpoint and the wait-set it parks on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    endpoint: u64,
    set: u64,
}

impl Binding {
    /// The endpoint the session delivers this app's events to.
    #[must_use]
    pub const fn endpoint(&self) -> u64 {
        self.endpoint
    }

    /// The wait-set handle, for [`park`] and for adding members of the app's
    /// own.
    #[must_use]
    pub const fn set(&self) -> u64 {
        self.set
    }
}

/// Put a worker's answer `wake` on wait-set `set` under `token`, so the loop
/// is woken when an answer lands.
///
/// A wake that is not armed has no read end: its worker never started, the
/// work runs on the loop, and there is nothing to wait for.
///
/// # Errors
///
/// The wait-set's refusal — fatal to the caller, since an answer nothing
/// wakes the loop for is never taken in.
pub fn watch_wake(set: u64, wake: &tairix_rt::sync::WorkerWake, token: u64) -> Result<(), Errno> {
    let Some(read) = wake.read_end() else {
        return Ok(());
    };
    let ret = tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::Stream,
        u64::from(read),
        token,
    );
    if ret == 0 {
        Ok(())
    } else {
        Err(Errno::from_syscall(ret))
    }
}

/// Start `worker`, the application `name`'s `what` worker, with its answer
/// wake watched on `set` under `token`, answering whether a thread of its
/// own now serves it.
///
/// Never fatal. A wake the set refuses stops the desk before any thread runs,
/// and a thread the machine will not grant leaves it stopped: either way each
/// submission is carried out where it is made and its answer is waiting at
/// once, so the work is slower under load and never lost. Both are said.
pub fn start_worker<S, Req, Ans, D>(
    name: &str,
    worker: &Arc<Worker<S, Req, Ans, D>>,
    set: u64,
    token: u64,
    what: &str,
) -> bool
where
    S: Send + 'static,
    Req: Send + 'static,
    Ans: Send + 'static,
    D: Desk<Req, Ans> + Send + 'static,
{
    if let Err(err) = watch_wake(set, worker.wake(), token) {
        worker.stop();
        report(
            name,
            format_args!("{what} wake refused ({err}); its work is done on the event loop"),
        );
        return false;
    }
    match Worker::start(worker) {
        Ok(_) => true,
        Err(reason) => {
            report(
                name,
                format_args!("no {what} worker ({reason:?}); its work is done on the event loop"),
            );
            false
        }
    }
}

/// Bind this process's event mailbox, admitting to it only the desktop
/// session serving windows, and add it, with the memory-pressure band, to a
/// fresh wait-set.
///
/// Fails closed rather than degrading into a re-poll: an app that cannot be
/// woken by its own events has no correct way to carry on.
///
/// # Errors
///
/// [`EXIT_NO_EVENTS`] for every refusal along the way, with the step named.
pub fn bind_event_mailbox() -> Result<Binding, ShellError> {
    let Ok(origin) = tairix_rt::self_origin() else {
        return Err(ShellError::new(
            EXIT_NO_EVENTS,
            "own identity unavailable",
            Errno::NotFound,
        ));
    };
    let endpoint = tairix_abi::window_ipc::event_endpoint_for(origin.pid());
    if tairix_abi::ipc::is_reserved_endpoint(endpoint)
        || tairix_rt::port_bind(
            endpoint,
            WindowEvent::WIRE_LEN,
            crate::client::EVENT_MAILBOX_CAPACITY,
        ) != 0
    {
        return Err(ShellError::new(
            EXIT_NO_EVENTS,
            "event mailbox bind refused",
            Errno::NotFound,
        ));
    }
    // The id is derived from this process's pid, so any process could
    // otherwise fill the mailbox and starve the session's deliveries.
    tairix_rt::port_admit(endpoint, WINDOW_ENDPOINT)
        .map_err(|err| ShellError::new(EXIT_NO_EVENTS, "event mailbox admission refused", err))?;
    let set = tairix_rt::waitset_create();
    if set < 0 {
        return Err(ShellError::new(
            EXIT_NO_EVENTS,
            "wait-set refused",
            Errno::NotFound,
        ));
    }
    #[allow(clippy::cast_sign_loss)] // `set >= 0` checked above; it is a kernel handle.
    let set = set as u64;
    if tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::Port,
        endpoint,
        EVENT_TOKEN,
    ) != 0
    {
        return Err(ShellError::new(
            EXIT_NO_EVENTS,
            "event mailbox wait refused",
            Errno::NotFound,
        ));
    }
    if !tairix_procinfo::pressure::watch(set, PRESSURE_TOKEN) {
        return Err(ShellError::new(
            EXIT_NO_EVENTS,
            "memory-pressure wake refused",
            Errno::NotFound,
        ));
    }
    if tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::SystemNotice,
        u64::from(NoticeTopic::Desktop.as_u32()),
        DESKTOP_TOKEN,
    ) != 0
    {
        return Err(ShellError::new(
            EXIT_NO_EVENTS,
            "desktop-change wake refused",
            Errno::NotFound,
        ));
    }
    if tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::CallReply,
        WINDOW_ENDPOINT,
        REPLY_TOKEN,
    ) != 0
    {
        return Err(ShellError::new(
            EXIT_NO_EVENTS,
            "window-reply wake refused",
            Errno::NotFound,
        ));
    }
    Ok(Binding { endpoint, set })
}

/// Connect to the session ([`WindowClient::connect`]) and build this app's
/// [`Desktop`] model and [`ThemeRegistry`], with the session's current
/// appearance and accessibility axes already applied.
///
/// The screen, the density, and the look are current before anything is sized
/// or painted, so the first frame is right rather than a guess corrected once
/// the user has seen it.
///
/// # Errors
///
/// [`EXIT_NO_WINDOW`] for a refused connect, or for a desktop this client cannot
/// draw at (a scale outside the range [`Desktop`] admits is refused, never
/// clamped — drawing at a density the session did not ask for would misplace
/// every hit-test in the window).
pub fn bring_up_desktop<T: WindowTransport>(
    client: &mut WindowClient<T>,
) -> Result<(Desktop, ThemeRegistry), ShellError> {
    let info = client
        .connect()
        .map_err(|err| ShellError::new(EXIT_NO_WINDOW, "the desktop refused the connect", err))?;
    let desktop = Desktop::new(info)
        .map_err(|err| ShellError::new(EXIT_NO_WINDOW, "cannot draw this desktop", err))?;
    let mut themes = ThemeRegistry::with_builtins();
    follow(&mut themes, &desktop);
    Ok((desktop, themes))
}

/// Bring `themes` into step with `desktop`: its appearance, its
/// accessibility axes and the user's text, which a surface drawn on some
/// without the others would get partly wrong.
fn follow(themes: &mut ThemeRegistry, desktop: &Desktop) {
    themes.set_appearance(desktop.appearance());
    themes.set_accessibility(Accessibility::of(&desktop.info()));
    themes.set_text(desktop.info().text());
}

/// Read the desktop state the session published, adopt it into `desktop`, and
/// bring `themes` into step — answering whether anything the application
/// draws from actually moved.
///
/// What a [`Wake::DesktopChanged`] owes, and the exact pair
/// [`bring_up_desktop`] establishes at start-up, so an application follows a
/// light/dark switch — or a contrast, density, motion or text change — with
/// the same one call it opened with. The read is a
/// plain syscall rather than a call to the session, so it costs no IPC round
/// trip on the loop that owes the user a frame. The answer is whether the
/// window must be redrawn, as [`Desktop::adopt`] gives it, so a wake with
/// nothing in it, or with a new frame rate alone, costs no repaint.
///
/// # Errors
///
/// The kernel's refusal — [`Errno::NotFound`] before the session has published
/// anything, which is the ordinary state of an application started in a
/// text-only session — or [`Errno::OutOfRange`] for a state this build cannot
/// draw at. Both `desktop` and `themes` keep the state they had, so a refusal
/// leaves the window drawing correctly and the application reports it.
pub fn adopt_desktop(desktop: &mut Desktop, themes: &mut ThemeRegistry) -> Result<bool, Errno> {
    let mut buf = [0u8; NOTICE_PAYLOAD_MAX];
    let read = tairix_rt::notice_read(NoticeTopic::Desktop, &mut buf);
    if read < 0 {
        return Err(Errno::from_syscall(read));
    }
    // Narrowed rather than cast: `usize` is 32 bits on a wasm32 build, so a
    // cast would truncate a length the kernel reported.
    let read = usize::try_from(read).map_err(|_| Errno::OutOfRange)?;
    let bytes = buf.get(..read).ok_or(Errno::OutOfRange)?;
    let Notice::Desktop(info) = Notice::decode(NoticeTopic::Desktop, bytes)? else {
        // The topic's own decode answers its own variant; anything else
        // would be the kernel describing a different topic.
        return Err(Errno::BadMagic);
    };
    if !desktop.adopt(info)? {
        return Ok(false);
    }
    follow(themes, desktop);
    Ok(true)
}

/// Bytes per pixel of a [`mode_for`] surface, which an app that writes the
/// shared frame itself takes its own arithmetic from rather than restating.
pub const BYTES_PER_PIXEL: u32 = 4;

/// A `width_px` × `height_px` RGBA window mode, one frame's worth per row.
///
/// The one place a window's mode is shaped, so a create, a resize, and every
/// present agree on stride and format.
#[must_use]
pub fn mode_for(width_px: u32, height_px: u32) -> DisplayMode {
    DisplayMode {
        width_px,
        height_px,
        stride_bytes: width_px.saturating_mul(BYTES_PER_PIXEL),
        format: DisplayFormat::Rgba8888,
    }
}

/// One open window's channel-side state: the id the session knows it by, the
/// shared frame region every present crosses, and the pixel layout both are
/// shaped as.
///
/// It holds no picture. What a window *looks* like is the application's — a
/// plain [`Surface`] for most, a screen model carrying its own cell diff for a
/// terminal — and a pane that owned a surface would force a second
/// window-sized allocation on every app whose retained picture is not literally
/// one. So the pane owns the protocol: attach, encode, present, re-map, close.
///
/// A single-window app composes one inside [`AppWindow`], which pairs it with
/// the retained surface and takes the paint as a closure. An app that opens a
/// window per document, or a popup above one, holds them itself.
///
/// Dropping a pane unmaps its region but tells the session nothing, so a caller
/// that means to take a window off the screen calls [`Self::close`].
pub struct WindowPane {
    window: u64,
    frames: WindowFrames,
    mode: DisplayMode,
    ring: FrameRing,
    /// What was drawn into the picture while both frames were the session's,
    /// presented once one comes back ([`Self::present_deferred`]).
    deferred: Option<DamageList>,
}

impl WindowPane {
    /// The pane over window `window`'s fresh `frames`, shaped as `mode`.
    fn adopt(window: u64, frames: WindowFrames, mode: &DisplayMode) -> Self {
        Self {
            window,
            frames,
            mode: *mode,
            ring: FrameRing::new(mode),
            deferred: None,
        }
    }

    /// Create and grant a `mode`-shaped frame region.
    fn region(mode: &DisplayMode) -> Result<(WindowFrames, u64), ShellError> {
        let Some(len) = region_bytes(mode, FRAME_COUNT) else {
            return Err(ShellError::new(
                EXIT_NO_FRAMES,
                "frame region larger than the address width",
                Errno::OutOfRange,
            ));
        };
        let Some(frames) = WindowFrames::create(len) else {
            return Err(ShellError::new(
                EXIT_NO_FRAMES,
                "shared frame region refused",
                Errno::OutOfMemory,
            ));
        };
        let Some(grant) = frames.grant() else {
            return Err(ShellError::new(
                EXIT_NO_FRAMES,
                "frame region grant refused",
                Errno::OutOfMemory,
            ));
        };
        Ok((frames, grant))
    }

    /// Open a top-level window of `mode`, titled `title`, sized as `sizing`
    /// allows. The session's refusal arrives later as an
    /// [`EventError::Refused`](crate::client::EventError::Refused) of its
    /// create ([`refused_create`]).
    ///
    /// # Errors
    ///
    /// [`EXIT_NO_FRAMES`] when the shared region could not be sized, created,
    /// or granted, and [`EXIT_NO_WINDOW`] when the create could not be asked.
    /// Every refusal unmaps whatever it had allocated.
    pub fn open<T: WindowTransport>(
        client: &mut WindowClient<T>,
        event_endpoint: u64,
        mode: &DisplayMode,
        title: &str,
        sizing: WindowSizing,
    ) -> Result<Self, ShellError> {
        let (frames, grant) = Self::region(mode)?;
        let window = client
            .create(grant, event_endpoint, FRAME_COUNT, mode, title, sizing)
            .map_err(|err| {
                ShellError::new(EXIT_NO_WINDOW, "the window could not be asked for", err)
            })?;
        Ok(Self::adopt(window, frames, mode))
    }

    /// Open an undecorated popup of `mode` at `offset` from `parent`'s client
    /// origin.
    ///
    /// A negative offset is legitimate: the session resolves it against the
    /// parent's screen position and clamps the popup on screen, so a popup
    /// larger than its parent still shows whole.
    ///
    /// # Errors
    ///
    /// [`EXIT_NO_FRAMES`] for the region, and [`EXIT_NO_WINDOW`] when the
    /// popup could not be asked for.
    pub fn open_popup<T: WindowTransport>(
        client: &mut WindowClient<T>,
        parent: u64,
        event_endpoint: u64,
        mode: &DisplayMode,
        offset: (i32, i32),
    ) -> Result<Self, ShellError> {
        let (frames, grant) = Self::region(mode)?;
        let window = client
            .create_popup(&PopupSpec {
                transient: TransientSpec {
                    parent_window_id: parent,
                    shm_handle: grant,
                    event_endpoint,
                    frame_count: FRAME_COUNT,
                    surface: *mode,
                },
                offset_x: offset.0,
                offset_y: offset.1,
            })
            .map_err(|err| {
                ShellError::new(EXIT_NO_WINDOW, "the popup could not be asked for", err)
            })?;
        Ok(Self::adopt(window, frames, mode))
    }

    /// Open a tool window of client `mode` titled `title`, sized as `sizing`
    /// allows, hung from `parent` at `offset` from its client origin or
    /// carried by the press it holds at `carry` along the band.
    ///
    /// # Errors
    ///
    /// [`EXIT_NO_FRAMES`] for the region, and [`EXIT_NO_WINDOW`] for a refused
    /// title or a tool window that could not be asked for.
    pub fn open_tool<T: WindowTransport>(
        client: &mut WindowClient<T>,
        parent: u64,
        event_endpoint: u64,
        (mode, sizing): (&DisplayMode, WindowSizing),
        offset: (i32, i32),
        carry: Option<u32>,
        title: &str,
    ) -> Result<Self, ShellError> {
        let title = WindowTitle::new(title).map_err(|err| {
            ShellError::new(EXIT_NO_WINDOW, "the tool window's title was refused", err)
        })?;
        let (frames, grant) = Self::region(mode)?;
        let window = client
            .create_tool(&ToolSpec {
                transient: TransientSpec {
                    parent_window_id: parent,
                    shm_handle: grant,
                    event_endpoint,
                    frame_count: FRAME_COUNT,
                    surface: *mode,
                },
                offset_x: offset.0,
                offset_y: offset.1,
                carry,
                title,
                sizing,
            })
            .map_err(|err| {
                ShellError::new(
                    EXIT_NO_WINDOW,
                    "the tool window could not be asked for",
                    err,
                )
            })?;
        Ok(Self::adopt(window, frames, mode))
    }

    /// Open a dialog window of client `mode` titled `title`, owned by `owner`
    /// and holding it as `modality` says.
    ///
    /// # Errors
    ///
    /// [`EXIT_NO_FRAMES`] for the region, and [`EXIT_NO_WINDOW`] for a refused
    /// title or a dialog that could not be asked for.
    pub fn open_dialog<T: WindowTransport>(
        client: &mut WindowClient<T>,
        owner: u64,
        event_endpoint: u64,
        mode: &DisplayMode,
        modality: DialogModality,
        title: &str,
    ) -> Result<Self, ShellError> {
        let title = WindowTitle::new(title).map_err(|err| {
            ShellError::new(EXIT_NO_WINDOW, "the dialog's title was refused", err)
        })?;
        let (frames, grant) = Self::region(mode)?;
        let window = client
            .create_dialog(&DialogSpec {
                transient: TransientSpec {
                    parent_window_id: owner,
                    shm_handle: grant,
                    event_endpoint,
                    frame_count: FRAME_COUNT,
                    surface: *mode,
                },
                modality,
                title,
            })
            .map_err(|err| {
                ShellError::new(EXIT_NO_WINDOW, "the dialog could not be asked for", err)
            })?;
        Ok(Self::adopt(window, frames, mode))
    }

    /// Open a desktop layer surface of `mode` at screen point `at` in
    /// stacking layer `depth`.
    ///
    /// The same region, present, and resize bookkeeping every pane has —
    /// a layer surface is a window in the session's one registry, so it
    /// needs no second copy of any of it. What differs is only the opening:
    /// it requires `CAP_DESKTOP_LAYER`, and it names a screen point instead
    /// of a parent. The refusal of an account whose ceiling does not carry the
    /// capability arrives as the create's does, and is an ordinary outcome,
    /// not a fault: the caller carries on without a desktop presence.
    ///
    /// # Errors
    ///
    /// [`EXIT_NO_FRAMES`] for the region, and [`EXIT_NO_WINDOW`] when the
    /// surface could not be asked for.
    pub fn open_layer<T: WindowTransport>(
        client: &mut WindowClient<T>,
        event_endpoint: u64,
        mode: &DisplayMode,
        at: (i32, i32),
        depth: LayerDepth,
    ) -> Result<Self, ShellError> {
        let (frames, grant) = Self::region(mode)?;
        let window = client
            .open_layer(&LayerSpec {
                shm_handle: grant,
                event_endpoint,
                frame_count: FRAME_COUNT,
                surface: *mode,
                x: at.0,
                y: at.1,
                depth,
            })
            .map_err(|err| {
                ShellError::new(
                    EXIT_NO_WINDOW,
                    "the layer surface could not be asked for",
                    err,
                )
            })?;
        Ok(Self::adopt(window, frames, mode))
    }

    /// Move this layer surface to screen point `at` in stacking layer
    /// `depth`. The session clamps the point onto the work area.
    ///
    /// # Errors
    ///
    /// A transport failure; the session's refusal — a pane that is not a
    /// layer surface — arrives later, as the client's refusals do.
    pub fn place<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        at: (i32, i32),
        depth: LayerDepth,
    ) -> Result<(), Errno> {
        client.place_layer(self.window, at.0, at.1, depth)
    }

    /// The window's id, which its delivered events name.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.window
    }

    /// The layout the region and every present are shaped as.
    #[must_use]
    pub const fn mode(&self) -> &DisplayMode {
        &self.mode
    }

    /// Whether the session has released its copy of the window's pixels and
    /// this side has given the region back.
    ///
    /// The next present sends the whole window from the surface it is handed,
    /// so a caller whose surface does not already hold the whole picture paints
    /// it whole first.
    #[must_use]
    pub const fn content_released(&self) -> bool {
        self.frames.is_released()
    }

    /// Copy `damage` of `surface` into the shared frame and present exactly
    /// that rectangle.
    ///
    /// The frame drawn into is the one the session is not reading, brought up
    /// to date from `surface` with whatever the other frame's presents changed
    /// since it was last drawn; `surface` is therefore the whole window's
    /// picture. While the window's last present is unanswered both frames are
    /// the session's, so nothing is drawn: the rectangle is kept and presented
    /// from the picture once [`Answer::FrameReturned`] says one is back
    /// ([`Self::present_deferred`]), and the window paints at most once per
    /// present the desktop serves. A region the session released is
    /// re-attached first and sent whole, since the fresh region holds none of
    /// the pixels a partial present would leave standing — but not while the
    /// window is hidden ([`WindowClient::is_hidden`]): nothing is presented
    /// then, since nobody can see it and re-attaching would map again the
    /// memory the release gave back. Nor is anything presented while the
    /// re-attach waits for room at the session; the window is asked to paint
    /// again once it has some.
    ///
    /// # Errors
    ///
    /// [`Errno::NotAttached`] when the region could not be re-attached,
    /// whatever the frame codec refuses (a rectangle outside the surface or
    /// past the region), and a transport failure.
    pub fn present<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        surface: &Surface,
        damage: DamageRect,
    ) -> Result<(), Errno> {
        let damage = DamageList::new(&[damage])?;
        self.present_list(client, surface, &damage)
    }

    /// [`present`](Self::present) for several rectangles at once: each copied
    /// into the shared frame, and the present naming exactly those, so the
    /// session converts and recomposes what moved rather than the box
    /// spanning it.
    ///
    /// # Errors
    ///
    /// [`present`](Self::present)'s.
    pub fn present_list<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        surface: &Surface,
        damage: &DamageList,
    ) -> Result<(), Errno> {
        let (window, mode) = (self.window, self.mode);
        if client.is_hidden(window) {
            self.deferred = None;
            return Ok(());
        }
        if client.present_state(window) == PresentState::InFlight {
            self.defer(damage);
            client.owe_frame(window);
            return Ok(());
        }
        let fresh = self.frames.is_released() || client.content_released(window);
        if fresh {
            self.deferred = None;
            let grant = self.frames.reattach().ok_or(Errno::NotAttached)?;
            if let Err(errno) = client.resize(window, grant, FRAME_COUNT, &mode) {
                // The session never took the fresh region, so none is kept.
                self.frames.release();
                // A resize in flight, or this process's own calls filling its
                // place at the session: the client asks for the paint again
                // once one is answered.
                return if errno == Errno::WouldBlock {
                    Ok(())
                } else {
                    Err(Errno::NotAttached)
                };
            }
            self.ring.reset(&mode);
        }
        let wanted = match self.deferred.take() {
            Some(held) => DamageList::fitted(held.rects().iter().chain(damage.rects()).copied())
                .unwrap_or(*damage),
            None => *damage,
        };
        let damage = presented_damage(&mode, fresh, wanted)?;
        let pixels = self.frames.pixels().ok_or(Errno::NotAttached)?;
        let sent = self.ring.present(client, window, damage, |index, owed| {
            draw_frame(pixels, index, owed, surface, &mode, &damage)
        });
        match sent {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.defer(&damage);
                Ok(())
            }
            Err(errno) => {
                self.defer(&wanted);
                Err(errno)
            }
        }
    }

    /// Present from `surface` what was drawn while both frames were the
    /// session's: the answer to [`Answer::FrameReturned`] for this window.
    ///
    /// # Errors
    ///
    /// [`present`](Self::present)'s.
    pub fn present_deferred<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        surface: &Surface,
    ) -> Result<(), Errno> {
        match self.deferred.take() {
            Some(deferred) => self.present_list(client, surface, &deferred),
            None => Ok(()),
        }
    }

    /// Keep `damage` for the present a returned frame makes.
    fn defer(&mut self, damage: &DamageList) {
        self.deferred = Some(match self.deferred {
            Some(held) => DamageList::fitted(held.rects().iter().chain(damage.rects()).copied())
                .unwrap_or(*damage),
            None => *damage,
        });
    }

    /// Re-map the frame region onto `new_mode`.
    ///
    /// The resize is posted, not awaited: the fresh region is adopted at once
    /// and painted into, and the session serves the resize before any present
    /// made after it. A refusal arrives later, as an
    /// [`EventError::Refused`](crate::client::EventError::Refused) followed by
    /// the size the session kept, which this is asked to take again. A size
    /// asked while an earlier resize is unanswered, or while this process's
    /// own calls fill its place at the session, is [`Fitted::Deferred`]: no
    /// region is kept for it, and it is delivered again once one is answered.
    pub fn resize<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        new_mode: &DisplayMode,
    ) -> Fitted {
        if client.resize_in_flight(self.window) {
            return Fitted::Deferred;
        }
        self.remap(client, new_mode)
    }

    /// Post the resize onto a fresh region for `new_mode` and adopt it.
    fn remap<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        new_mode: &DisplayMode,
    ) -> Fitted {
        let Some(len) = region_bytes(new_mode, FRAME_COUNT) else {
            return Fitted::Kept;
        };
        let Some(spare) = WindowFrames::create(len) else {
            return Fitted::Kept;
        };
        let Some(grant) = spare.grant() else {
            return Fitted::Kept;
        };
        match client.resize(self.window, grant, FRAME_COUNT, new_mode) {
            Ok(()) => {}
            // This process's own calls fill its place at the endpoint; the
            // size is delivered again once one of them is answered.
            Err(Errno::WouldBlock) => return Fitted::Deferred,
            Err(_) => return Fitted::Kept,
        }
        self.frames = spare;
        self.mode = *new_mode;
        self.ring.reset(new_mode);
        // The fresh region is painted whole.
        self.deferred = None;
        Fitted::Adopted
    }

    /// [`resize`](Self::resize) together with `surface`, the one the window
    /// is drawn into, adopted with the region.
    pub fn resize_with<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        new_mode: &DisplayMode,
        surface: &mut Surface,
    ) -> Fitted {
        if client.resize_in_flight(self.window) {
            return Fitted::Deferred;
        }
        let Some(fresh) = Surface::new(new_mode.width_px, new_mode.height_px) else {
            return Fitted::Kept;
        };
        let fitted = self.remap(client, new_mode);
        if fitted == Fitted::Adopted {
            *surface = fresh;
        }
        fitted
    }

    /// Answer the session's release of its own copy by giving this side's
    /// region back, so the pages are actually freed.
    ///
    /// The pages only go when both halves let go, which is the whole point.
    pub fn release_frames(&mut self) {
        self.frames.release();
        self.deferred = None;
    }

    /// Close the window.
    ///
    /// The pane is consumed, so the frame region is unmapped and nothing is
    /// left pinned whatever the session answers, and the client forgets the
    /// window at once.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn close<T: WindowTransport>(self, client: &mut WindowClient<T>) -> Result<(), Errno> {
        client.close(self.window)
    }
}

/// Whether `refused` is the refusal of window `window`'s create: the window
/// never existed, so its pane is dropped and whatever it was opened for
/// treated as not done.
#[must_use]
pub fn refused_create(refused: &Refused, window: u64) -> bool {
    refused.request == PostedRequest::Create && refused.window_id == window
}

/// A single-window app's open window: the pane and the surface every frame is
/// drawn into.
///
/// The surface is held for the life of the window because allocating and
/// zeroing one per present would be a whole-window pass of its own, and holding
/// it is what makes a clipped repaint sound — every pixel outside the clip is
/// the one already on screen.
struct Retained {
    pane: WindowPane,
    surface: Surface,
    /// Where the retained picture no longer matches the screen: a paint
    /// refused, or a present failed, part-way through this rectangle.
    torn: Option<DamageRect>,
}

/// The live window channel an app owns, and the one window it may or may not
/// have open.
///
/// An app is on the icon bar whether or not a window is open, so the channel
/// outlives every window that crosses it. An app that opens more than one holds
/// [`WindowPane`]s itself instead.
pub struct AppWindow {
    client: WindowClient<RtWindowTransport>,
    retained: Option<Retained>,
}

impl AppWindow {
    /// A channel with no window open yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            client: WindowClient::new(RtWindowTransport),
            retained: None,
        }
    }

    /// The channel itself, for the requests the shell does not wrap (the
    /// app-bar declaration, a pick, a menu, a tooltip, a retitle).
    pub fn client(&mut self) -> &mut WindowClient<RtWindowTransport> {
        &mut self.client
    }

    /// Whether a window is open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.retained.is_some()
    }

    /// The open window's id, or `None` with none open.
    #[must_use]
    pub const fn window_id(&self) -> Option<u64> {
        match &self.retained {
            Some(held) => Some(held.pane.id()),
            None => None,
        }
    }

    /// Whether the session has released its copy of the window's pixels.
    ///
    /// [`Self::present`] performs the whole-window promotion itself for the
    /// rectangle it is handed; this is for the callers that must decide
    /// earlier.
    #[must_use]
    pub const fn content_released(&self) -> bool {
        match &self.retained {
            Some(held) => held.pane.content_released(),
            None => false,
        }
    }

    /// The open window's current shape, or `None` with none open.
    #[must_use]
    pub const fn mode(&self) -> Option<&DisplayMode> {
        match &self.retained {
            Some(held) => Some(held.pane.mode()),
            None => None,
        }
    }

    /// Open a window of `mode`, titled `title`, sized as `sizing` allows.
    ///
    /// The drawing surface is allocated before the session is asked, so a
    /// window the app could not draw into is never put on screen. The
    /// session's refusal arrives later, and [`Self::refused`] ends the window
    /// over it.
    ///
    /// # Errors
    ///
    /// [`EXIT_NO_WINDOW`] when the surface could not be allocated or the
    /// create could not be asked, [`EXIT_NO_FRAMES`] for the shared region,
    /// and [`EXIT_NO_WINDOW`] again when a window is already open: an app that
    /// asks for a second through this one channel has lost track of the first.
    pub fn open(
        &mut self,
        event_endpoint: u64,
        mode: &DisplayMode,
        title: &str,
        sizing: WindowSizing,
    ) -> Result<(), ShellError> {
        if self.retained.is_some() {
            return Err(ShellError::new(
                EXIT_NO_WINDOW,
                "a window is already open",
                Errno::AlreadyExists,
            ));
        }
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            return Err(ShellError::new(
                EXIT_NO_WINDOW,
                "no memory for the window surface",
                Errno::OutOfMemory,
            ));
        };
        let pane = WindowPane::open(&mut self.client, event_endpoint, mode, title, sizing)?;
        self.retained = Some(Retained {
            pane,
            surface,
            torn: None,
        });
        Ok(())
    }

    /// The oldest answer landed for the application, a frame returned to the
    /// open window presented here rather than handed on: what its paint was
    /// deferred for is sent from the retained picture.
    pub fn take_answer(&mut self) -> Option<Answer> {
        loop {
            let answer = self.client.take_answer()?;
            let Answer::FrameReturned { window_id } = answer else {
                return Some(answer);
            };
            let Some(held) = self
                .retained
                .as_mut()
                .filter(|held| held.pane.id() == window_id)
            else {
                return Some(answer);
            };
            // A failed present keeps what it was to send for the next one.
            let _ = held.pane.present_deferred(&mut self.client, &held.surface);
        }
    }

    /// Whether `refused` ended the open window: its create was refused, so it
    /// never existed and is dropped, leaving the app on the icon bar.
    pub fn refused(&mut self, refused: &Refused) -> bool {
        let ended = self
            .window_id()
            .is_some_and(|window| refused_create(refused, window));
        if ended {
            self.retained = None;
        }
        ended
    }

    /// Draw `damage` of the window through `paint` and present that rectangle,
    /// clipped to the window, with whatever an earlier present left torn.
    ///
    /// A region the session released while the window was hidden is re-attached
    /// and presented whole, because it holds none of the pixels a partial
    /// present would leave standing. With no window open, nothing of the
    /// rectangle inside it, or the window still hidden, this is a no-op, so a
    /// loop need not sort its repaints by whether one is showing.
    ///
    /// # Errors
    ///
    /// [`Errno::NotAttached`] when the region could not be re-attached, and
    /// otherwise the session's refusal of the present.
    pub fn present(
        &mut self,
        damage: DamageRect,
        paint: impl FnOnce(&mut Surface),
    ) -> Result<(), Errno> {
        match self.try_present(damage, |surface| {
            paint(surface);
            Ok::<(), core::convert::Infallible>(())
        })? {
            Ok(()) => Ok(()),
            Err(never) => match never {},
        }
    }

    /// [`present`](Self::present) for a paint that can refuse.
    ///
    /// A refused paint presents nothing, so the window keeps showing its last
    /// frame; the pixels the paint may have left half-drawn are repainted by
    /// the next present, whose paint is handed a clip covering them.
    ///
    /// # Errors
    ///
    /// `Ok(Err(_))` carries the paint's own refusal, with nothing presented;
    /// the outer error is [`present`](Self::present)'s.
    pub fn try_present<E>(
        &mut self,
        damage: DamageRect,
        paint: impl FnOnce(&mut Surface) -> Result<(), E>,
    ) -> Result<Result<(), E>, Errno> {
        let Some(held) = self.retained.as_mut() else {
            return Ok(Ok(()));
        };
        // Nobody can see it; the present after the session asks for it again
        // sends the whole window.
        if self.client.is_hidden(held.pane.id()) {
            return Ok(Ok(()));
        }
        // Painted whole after either side let the frames go: a paint skipped
        // while the window was hidden left the picture behind.
        let released = held.pane.content_released() || self.client.content_released(held.pane.id());
        let Some(damage) = retained_damage(held.pane.mode(), released, held.torn.take(), damage)
        else {
            return Ok(Ok(()));
        };
        let mut painted = Ok(());
        held.surface.with_clip(
            damage.x,
            damage.y,
            damage.width_px,
            damage.height_px,
            |surface| painted = paint(surface),
        );
        if painted.is_err() {
            held.torn = Some(damage);
            return Ok(painted);
        }
        if let Err(err) = held.pane.present(&mut self.client, &held.surface, damage) {
            held.torn = Some(damage);
            return Err(err);
        }
        Ok(Ok(()))
    }

    /// Re-map the open window onto `new_mode`, as [`WindowPane::resize_with`]
    /// does: [`Fitted::Adopted`] repaints at it, [`Fitted::Deferred`] waits
    /// for the size to be delivered again, and [`Fitted::Kept`] — no window
    /// open, or none of its region could be made — leaves the window at the
    /// size it had and still drawable.
    pub fn resize(&mut self, new_mode: DisplayMode) -> Fitted {
        let Some(held) = self.retained.as_mut() else {
            return Fitted::Kept;
        };
        let fitted = held
            .pane
            .resize_with(&mut self.client, &new_mode, &mut held.surface);
        if fitted == Fitted::Adopted {
            // The old geometry's torn rectangle names pixels of a surface
            // that is gone, and the caller repaints the fresh one whole.
            held.torn = None;
        }
        fitted
    }

    /// Restate the range the window manager may resize the open window
    /// within.
    ///
    /// For an app whose content constraints move while its window is open —
    /// a board switching to a larger one, a layout remeasured at a new
    /// desktop density — because the range declared at open describes
    /// content it is no longer showing. A window the new range no longer
    /// holds is brought inside it and told so with a resize event. With no
    /// window open there is no range to restate, which is not a failure: the
    /// next open declares the range that is current then.
    ///
    /// # Errors
    ///
    /// A transport failure; the session's refusal arrives later, the
    /// previously declared range standing and the window still resizable
    /// within it.
    pub fn set_sizing(&mut self, sizing: WindowSizing) -> Result<(), Errno> {
        let Some(held) = self.retained.as_ref() else {
            return Ok(());
        };
        self.client.set_sizing(held.pane.id(), sizing)
    }

    /// Ask the compositor to blur what is behind the open window by
    /// `radius_px` logical pixels, `0` for none. With no window open there is
    /// nothing to blur, which is not a failure: the next open asks again.
    ///
    /// # Errors
    ///
    /// A radius past the bound, or a transport failure; the session's refusal
    /// arrives later, the window keeping the blur it had.
    pub fn set_backdrop_blur(&mut self, radius_px: u16) -> Result<(), Errno> {
        let Some(held) = self.retained.as_ref() else {
            return Ok(());
        };
        self.client.set_backdrop_blur(held.pane.id(), radius_px)
    }

    /// Retitle the open window to `title`, replacing the title given at open.
    /// With no window open there is nothing to retitle, which is not a
    /// failure: the next open states the title that is current then.
    ///
    /// # Errors
    ///
    /// A title the protocol refuses, or a transport failure; the session's
    /// refusal arrives later, the window keeping the title it had.
    pub fn set_title(&mut self, title: &str) -> Result<(), Errno> {
        let Some(held) = self.retained.as_ref() else {
            return Ok(());
        };
        self.client.set_title(held.pane.id(), title)
    }

    /// Answer the session's release of its own copy by giving this side's
    /// region back, so the pages are actually freed.
    pub fn release_frames(&mut self) {
        if let Some(held) = self.retained.as_mut() {
            held.pane.release_frames();
        }
    }

    /// Close the open window, if any, leaving the app on the icon bar.
    ///
    /// The pane is dropped whatever the session answers, so the frame region
    /// is unmapped and nothing is left pinned.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn close(&mut self) -> Result<(), Errno> {
        match self.retained.take() {
            Some(held) => held.pane.close(&mut self.client),
            None => Ok(()),
        }
    }
}

impl Default for AppWindow {
    fn default() -> Self {
        Self::new()
    }
}
