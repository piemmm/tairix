//! The `Run` entry-point binary of `music.app`: the window, the playback
//! thread, and the worker.
//!
//! A pure-Rust program over `tairix-rt`, and its own two sandboxed decoders:
//! re-entered in the sandbox's session-worker role it serves the audio decode
//! protocol, and in its worker role the image renderer, each over the two
//! pipes the sandbox wires and nothing else.
//!
//! # Five threads, none of which polls
//!
//! The **window** thread owns the window and the player's state and never
//! waits on anything but its own wait-set. Playback is the shared engine on a
//! **playback** thread of its own, parked on the decoder's pipes, the stream's
//! mailbox and the orders the window posts; it posts its status back and wakes
//! the window. Everything else that waits is a job for one of three
//! **workers**, whose answers wake the window in turn: one reads each track's
//! tags, one decodes the cover heard, and one lists the output devices and
//! saves the listener's settings. A track's tags and its cover are a
//! stranger's bytes, each read on a worker of its own, so a hostile file holds
//! up neither the other nor the player's own business.
//!
//! The app holds no filesystem capability. It plays exactly the files the user
//! chose in the session's picker or handed it from the file manager, each a
//! delegated descriptor shared by the window, the workers and the playback
//! thread until the last of them lets it go.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::collections::{BTreeMap, VecDeque};
    use alloc::format;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;

    use tairix_abi::audio::AudioGain;
    use tairix_abi::driver::audio::StreamDirection;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::window_ipc::{
        AlertSeverity, AlertSpec, AppBarClick, MenuOutcome, PickPurpose, WindowEvent, WindowRegion,
    };
    use tairix_abi::{Errno, DOCUMENT_ROLE_ARG, STDIN};
    use tairix_appdata::{RtHost, Settings as Store};
    use tairix_audio::live::RtAudio;
    use tairix_audio::stream::devices;
    use tairix_controls::damage;
    use tairix_font::BitmapFont;
    use tairix_geometry::{Point, Region, Scale};
    use tairix_hash::BuildFastHash;
    use tairix_input::InputEvent;
    use tairix_music::view::{Command, ContextMenu, Device, Outcome, Player, Request, Saved};
    use tairix_music::{Edit, Layout, Playlist, Repeat, WIN_HEIGHT, WIN_WIDTH};
    use tairix_player::rt::{live, EngineWaits, HeldFiles, RtEngine, RtTags, Tokens};
    use tairix_player::{
        Control, EntryId, Failure, Note, Outcome as Ended, Settings, Status, Tags,
    };
    use tairix_raster::{Color, Surface};
    use tairix_reclaim::desktop::working_set_ui_cache;
    use tairix_reclaim::{CachedBytes, ReclaimCache};
    use tairix_rt::sync::{Mutex, WorkerWake};
    use tairix_rt::thread::Thread;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_rt::File;
    use tairix_sandbox::audiodecode::AudioDecodeService;
    use tairix_sandbox::imagerender::{
        render_thumbnail, upload_document, ImageRenderService, MAX_ICON_SIDE,
    };
    use tairix_sandbox::rt::{
        serve_session_stdio, serve_stdio, session_worker_role, worker_role, RtLauncher,
    };
    use tairix_sandbox::ParserSandbox;
    use tairix_sound::{CoverRange, Metadata};
    use tairix_theme::{TextRole, Theme};
    use tairix_util::defer::JobQueue;
    use tairix_window::app::{self, ShellError, Wake, WindowPane};
    use tairix_window::notices::{Notices, Raised};
    use tairix_window::{
        held_modifiers, key_input_event, pointer_input_events, pointer_point, present_damage,
        scroll_input_events, Answer, Desktop, EventDrain, EventError, EventMailbox, EventSource,
        Parked, PostedRequest, Refused, Repaint, Target, WindowClient, WindowEvents, WindowSizing,
    };

    /// The name this program states its refusals under.
    const APP_NAME: &str = "music";

    /// The window's title.
    const APP_TITLE: &str = "Music";

    /// The window loop's wake tokens: the chores worker's answers, the
    /// playback thread's posts, then the tags and art workers' answers.
    const CHORES_TOKEN: u64 = app::FIRST_APP_TOKEN;
    const PLAYBACK_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;
    const TAGS_TOKEN: u64 = app::FIRST_APP_TOKEN + 2;
    const ART_TOKEN: u64 = app::FIRST_APP_TOKEN + 3;

    /// The playback thread's wait-set tokens.
    const ENGINE: Tokens = Tokens {
        decoder_read: 1,
        decoder_write: 2,
        notify: 3,
    };
    const ORDERS: u64 = 4;

    /// Jobs a worker holds at once; the window keeps the rest in order and
    /// hands them on as answers come back, so none is dropped.
    const WORKER_QUEUE: usize = 16;

    /// What an art cache entry costs beyond its pixels: its key and its place.
    const ART_ENTRY_METADATA_BYTES: usize = 128;

    /// The settings' keys.
    mod key {
        pub const VOLUME: &str = "volume";
        pub const SHUFFLE: &str = "shuffle";
        pub const REPEAT: &str = "repeat";
        pub const NORMALISE: &str = "normalise";
        pub const DEVICE: &str = "device";
    }

    static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;

    // ---- settings ------------------------------------------------------

    /// What the listener set last time, read tolerantly: a missing or damaged
    /// value is its default.
    fn load_settings() -> Saved {
        let mut host = RtHost;
        let store = Store::open_without_defaults(&mut host);
        let defaults = Saved::default();
        let gain = store
            .i64(key::VOLUME)
            .ok()
            .flatten()
            .and_then(|millibel| i32::try_from(millibel).ok())
            .and_then(|millibel| AudioGain::new(millibel).ok())
            .unwrap_or(defaults.gain);
        Saved {
            gain,
            shuffle: store
                .bool(key::SHUFFLE)
                .ok()
                .flatten()
                .unwrap_or(defaults.shuffle),
            repeat: store
                .get(key::REPEAT)
                .and_then(Repeat::from_word)
                .unwrap_or(defaults.repeat),
            normalise: store
                .bool(key::NORMALISE)
                .ok()
                .flatten()
                .unwrap_or(defaults.normalise),
            device: store
                .get(key::DEVICE)
                .filter(|target| !target.is_empty())
                .map(String::from),
        }
    }

    /// Store `saved`.
    fn save_settings(saved: &Saved) -> Result<(), Errno> {
        let mut host = RtHost;
        let mut store = Store::open_without_defaults(&mut host);
        if let Some(refusal) = store.store_refusal() {
            return Err(refusal);
        }
        let refused = |_| Errno::OutOfRange;
        store
            .set_i64(key::VOLUME, i64::from(saved.gain.millibel()))
            .map_err(refused)?;
        store
            .set_bool(key::SHUFFLE, saved.shuffle)
            .map_err(refused)?;
        store
            .set(key::REPEAT, saved.repeat.word())
            .map_err(refused)?;
        store
            .set_bool(key::NORMALISE, saved.normalise)
            .map_err(refused)?;
        match &saved.device {
            Some(target) => store.set(key::DEVICE, target).map_err(refused)?,
            None => store.unset(key::DEVICE),
        }
        store.commit()
    }

    // ---- the playback thread -------------------------------------------

    /// What the window asks the playback thread, in order.
    enum Order {
        Control(Control),
        Edit(Edit),
        Add(Vec<(EntryId, Arc<File>)>),
    }

    /// The orders not yet carried out, and whether the window is leaving.
    #[derive(Default)]
    struct Orders {
        queue: VecDeque<Order>,
        leaving: bool,
    }

    /// What the playback thread last said, for the window.
    #[derive(Default)]
    struct Board {
        status: Option<Status>,
        notes: Vec<Note>,
        /// What further links of chained files say of themselves.
        links: Vec<(EntryId, u32, Metadata)>,
        ended: Option<Ended>,
        /// The thread stopped for good, and why.
        gone: Option<Failure>,
        /// A wake is on its way that the window has not yet read the board
        /// for, so a further change needs none.
        woken: bool,
    }

    /// Where the two threads meet.
    struct Link {
        orders: Mutex<Orders>,
        ordered: WorkerWake,
        board: Mutex<Board>,
        posted: WorkerWake,
    }

    impl Link {
        fn new() -> Self {
            Self {
                orders: Mutex::new(Orders::default()),
                ordered: WorkerWake::create(),
                board: Mutex::new(Board::default()),
                posted: WorkerWake::create(),
            }
        }

        /// Post `order`; a level displaces any level not yet carried out, so a
        /// dragged slider costs the stream one call however fast it moves.
        fn order(&self, order: Order) {
            let mut orders = self.orders.lock();
            if matches!(order, Order::Control(Control::SetGain(_))) {
                orders
                    .queue
                    .retain(|queued| !matches!(queued, Order::Control(Control::SetGain(_))));
            }
            orders.queue.push_back(order);
            drop(orders);
            self.ordered.nudge();
        }

        fn leave(&self) {
            self.orders.lock().leaving = true;
            self.ordered.nudge();
        }

        /// Post what the engine has to say since it last said anything.
        fn post(&self, engine: &mut Engine) {
            let notes = engine.take_notes();
            let links = engine.take_link_metadata();
            if !engine.take_changed() && notes.is_empty() && links.is_empty() {
                return;
            }
            let wake = {
                let mut board = self.board.lock();
                board.status = Some(engine.status().clone());
                board.notes.extend(notes);
                board.links.extend(links);
                board.ended = engine.outcome();
                !core::mem::replace(&mut board.woken, true)
            };
            if wake {
                self.posted.nudge();
            }
        }

        fn post_gone(&self, failure: Failure) {
            self.board.lock().gone = Some(failure);
            self.posted.nudge();
        }
    }

    type Engine = RtEngine<Playlist<Arc<File>>, HeldFiles>;

    /// The playback thread: the engine, parked on what it waits for, until the
    /// window leaves or the wait itself fails.
    fn playback(link: &Link, settings: Settings, opening: [Edit; 2]) {
        let mut playlist = Playlist::default();
        for edit in opening {
            playlist.apply(edit);
        }
        let mut engine = match live(playlist, settings, HeldFiles::new()) {
            Ok(engine) => engine,
            Err(failure) => return link.post_gone(failure),
        };
        let created = tairix_rt::waitset_create();
        let Ok(set) = u64::try_from(created) else {
            return link.post_gone(Failure::Wait {
                what: "make a wait-set to play from",
                errno: Errno::from_syscall(created),
            });
        };
        if let Err(errno) = app::watch_wake(set, &link.ordered, ORDERS) {
            return link.post_gone(Failure::Wait {
                what: "watch the window's orders",
                errno,
            });
        }
        let mut waits = EngineWaits::new(set, ENGINE);
        engine.begin(tairix_rt::clock_get());
        loop {
            waits.sync(&mut engine);
            link.post(&mut engine);
            let now = tairix_rt::clock_get();
            let timeout = engine
                .deadline()
                .map_or(u64::MAX, |at| at.saturating_sub(now));
            let mut token = 0;
            let waited = tairix_rt::waitset_wait(set, timeout, &mut token);
            let now = tairix_rt::clock_get();
            if waited != 0 {
                match Errno::from_syscall(waited) {
                    Errno::TimedOut => engine.on_timer(now),
                    errno => {
                        engine.abandon(Failure::Wait {
                            what: "wait for playback",
                            errno,
                        });
                        link.post(&mut engine);
                        return link.post_gone(Failure::Wait {
                            what: "wait for playback",
                            errno,
                        });
                    }
                }
                continue;
            }
            if waits.deliver(&mut engine, token, now) || token != ORDERS {
                continue;
            }
            link.ordered.drain();
            let (orders, leaving) = {
                let mut held = link.orders.lock();
                (core::mem::take(&mut held.queue), held.leaving)
            };
            for order in orders {
                match order {
                    Order::Control(control) => engine.on_control(now, control),
                    Order::Edit(edit) => engine.edit(now, |playlist| playlist.apply(edit)),
                    Order::Add(files) => engine.edit(now, |playlist| playlist.add(files)),
                }
            }
            if leaving {
                engine.on_control(now, Control::Stop);
                return;
            }
        }
    }

    /// What the chores worker is asked: the player's own business.
    enum Chore {
        Devices,
        Save(Saved),
    }

    /// What a chore came to.
    enum ChoreDone {
        Devices(Result<Vec<Device>, Errno>),
        Saved(Result<(), Errno>),
    }

    fn do_chore(_: &mut (), chore: &mut Chore) -> ChoreDone {
        match chore {
            Chore::Devices => ChoreDone::Devices(list_devices()),
            Chore::Save(saved) => ChoreDone::Saved(save_settings(saved)),
        }
    }

    /// A track whose tags are to be read.
    struct Probe {
        entry: EntryId,
        file: Arc<File>,
    }

    /// What its tags came to.
    struct Probed {
        entry: EntryId,
        tags: Tags,
    }

    /// Read a track's tags in the tags worker's decoder, started on first use
    /// and kept, so a folder of tracks costs one decoder process, not one each.
    fn probe(reader: &mut Option<RtTags>, job: &mut Probe) -> Probed {
        if reader.is_none() {
            *reader = RtTags::new();
        }
        let tags = reader
            .as_mut()
            .map_or(Tags::Unread, |held| held.read_tags(&job.file));
        Probed {
            entry: job.entry,
            tags,
        }
    }

    /// A cover to decode: the picture `file` holds at `cover`, to a square of
    /// `side`.
    struct Art {
        entry: EntryId,
        file: Arc<File>,
        cover: CoverRange,
        side: u32,
    }

    /// What a cover was drawn as.
    struct Drawn {
        entry: EntryId,
        side: u32,
        picture: Option<Surface>,
    }

    /// The art worker's image sandbox, kept from one cover to the next.
    type Renderer = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    fn draw_art(renderer: &mut Renderer, art: &mut Art) -> Drawn {
        Drawn {
            entry: art.entry,
            side: art.side,
            picture: cover_picture(renderer, &art.file, art.cover, art.side),
        }
    }

    /// The cover `file` holds at `cover`, decoded in the image sandbox to a
    /// square of `side`.
    fn cover_picture(
        renderer: &mut ParserSandbox<RtLauncher, tairix_rt::LogSink>,
        file: &File,
        cover: CoverRange,
        side: u32,
    ) -> Option<Surface> {
        let length = usize::try_from(cover.len).ok()?;
        upload_document(renderer, length, |offset, into: &mut [u8]| {
            let left = usize::try_from(cover.len.saturating_sub(offset)).unwrap_or(usize::MAX);
            let take = into.len().min(left);
            file.read_at(cover.offset.saturating_add(offset), &mut into[..take])
                .map_err(Errno::from_syscall)
        })
        .ok()?;
        let side = side.min(MAX_ICON_SIDE);
        let fitted = render_thumbnail(renderer, side).ok()?;
        Surface::from_rgba8(side, side, &fitted.pixels)
    }

    /// The output devices the audio service lists to this session.
    fn list_devices() -> Result<Vec<Device>, Errno> {
        let sinks = devices(&mut RtAudio::new(), StreamDirection::Playback)?;
        let mut listed = Vec::new();
        listed
            .try_reserve_exact(sinks.len())
            .map_err(|_| Errno::OutOfMemory)?;
        listed.extend(sinks.iter().map(Device::of));
        Ok(listed)
    }

    /// A worker of the window's, and the jobs it had no room for, which the
    /// window keeps in order and hands on as answers come back so none is
    /// dropped.
    struct Lane<S, Req, Ans> {
        worker: Arc<Worker<S, Req, Ans, JobQueue<Req, Ans>>>,
        waiting: VecDeque<Req>,
        _stops: WorkerGuard<S, Req, Ans, JobQueue<Req, Ans>>,
    }

    impl<S: Send + 'static, Req: Send + 'static, Ans: Send + 'static> Lane<S, Req, Ans> {
        /// A worker carrying out `run` over `state`, its answers waking the
        /// window's wait-set `set` as `token`.
        fn start(
            run: fn(&mut S, &mut Req) -> Ans,
            state: S,
            set: u64,
            token: u64,
        ) -> Result<Self, String> {
            let worker = Worker::queued(run, state, WorkerWake::create(), WORKER_QUEUE)
                .map(Arc::new)
                .map_err(|_| String::from("no memory for a worker"))?;
            if let Err(reason) = Worker::start(&worker) {
                app::report(
                    APP_NAME,
                    format_args!("no worker thread ({reason:?}); jobs run on the window's thread"),
                );
            }
            let stops = WorkerGuard::new(&worker);
            app::watch_wake(set, worker.wake(), token)
                .map_err(|err| format!("worker wake refused ({err})"))?;
            Ok(Self {
                worker,
                waiting: VecDeque::new(),
                _stops: stops,
            })
        }

        /// Ask for `job`, after every job asked before it.
        fn ask(&mut self, job: Req) {
            self.waiting.push_back(job);
            self.hand_on();
        }

        /// Give the worker as many waiting jobs as it has room for.
        fn hand_on(&mut self) {
            while let Some(job) = self.waiting.pop_front() {
                if let Err(job) = self.worker.submit(job) {
                    self.waiting.push_front(job);
                    return;
                }
            }
        }
    }

    /// The window's three workers. A track's tags and its cover are a
    /// stranger's bytes, each read on a worker of its own, so a hostile file
    /// holds up neither the other nor the player's own business.
    struct Workers {
        /// The output devices, and the listener's settings.
        chores: Lane<(), Chore, ChoreDone>,
        /// Each track's tags.
        tags: Lane<Option<RtTags>, Probe, Probed>,
        /// The cover of the track heard.
        art: Lane<Renderer, Art, Drawn>,
    }

    impl Workers {
        /// Start all three, waking the window's wait-set `set`.
        fn start(set: u64) -> Result<Self, String> {
            let renderer = ParserSandbox::new(
                RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Documents),
                tairix_rt::LogSink,
            );
            Ok(Self {
                chores: Lane::start(do_chore, (), set, CHORES_TOKEN)?,
                tags: Lane::start(probe, None, set, TAGS_TOKEN)?,
                art: Lane::start(draw_art, renderer, set, ART_TOKEN)?,
            })
        }

        /// Give each as many waiting jobs as it has room for.
        fn hand_on(&mut self) {
            self.chores.hand_on();
            self.tags.hand_on();
            self.art.hand_on();
        }
    }

    // ---- the album art cache -------------------------------------------

    /// A decoded cover.
    struct Cover(Surface);

    impl CachedBytes for Cover {
        fn payload_bytes(&self) -> usize {
            usize::try_from(u64::from(self.0.width()) * u64::from(self.0.height()) * 4)
                .unwrap_or(usize::MAX)
        }

        fn wipe(&mut self) {
            let (width, height) = (self.0.width(), self.0.height());
            self.0
                .fill_rect(0, 0, width, height, Color::rgba(0, 0, 0, 0));
        }
    }

    /// Covers by entry, for one art side: a resize that changes the side
    /// leaves the old pictures behind.
    type Covers = ReclaimCache<EntryId, Cover, u32, BuildFastHash>;

    // ---- the window ----------------------------------------------------

    /// The window's park: its mailbox, the memory-pressure band, the desktop
    /// state, the workers' answers and the playback thread's posts.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        /// Each worker's wake, by the token it fires as.
        answers: [(u64, &'a WorkerWake); 3],
        link: &'a Link,
        desktop_moved: &'a Cell<bool>,
        pressure_moved: &'a Cell<bool>,
        /// When the list's held scrollbar press next steps, or `None` with
        /// nothing held, so the park sleeps outright. The loop writes it just
        /// before each wait.
        deadline_ns: &'a Cell<Option<u64>>,
    }

    impl RtEventSource<'_> {
        /// Drain the wake of the worker `token` names, whose answers the loop
        /// then collects.
        fn worker_woke(&self, token: u64) -> Parked {
            match self.answers.iter().find(|(fired, _)| *fired == token) {
                Some((_, wake)) => {
                    wake.drain();
                    Parked::Interrupted
                }
                None => Parked::Served,
            }
        }
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let Some(woken) = app::park_until_due(self.set, self.deadline_ns.get())? else {
                return Ok(Parked::Interrupted);
            };
            match woken {
                // Each wake is a level, so one left undrained would make the
                // park spin.
                Wake::App(PLAYBACK_TOKEN) => {
                    self.link.posted.drain();
                    Ok(Parked::Interrupted)
                }
                Wake::App(token) => Ok(self.worker_woke(token)),
                Wake::PressureChanged => {
                    tairix_font::trim_glyph_cache();
                    self.pressure_moved.set(true);
                    Ok(Parked::Interrupted)
                }
                Wake::DesktopChanged => {
                    self.desktop_moved.set(true);
                    Ok(Parked::Interrupted)
                }
                _ => Ok(Parked::Served),
            }
        }
    }

    /// What a refusal the player states came from, each keeping at most one
    /// alert waiting.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum Spoke {
        /// A file the user chose or handed over.
        Chosen,
        /// A folder the user chose.
        Folder,
        /// A track that would not play.
        Track,
        /// Playback itself.
        Playback,
        /// The player's settings.
        Settings,
    }

    /// The player's window and the surface it is painted from.
    struct PlayerWindow {
        pane: WindowPane,
        surface: Surface,
    }

    /// Why the player's window would not open.
    enum WindowRefused {
        /// No memory for the surface it is painted from.
        NoSurface,
        /// The window shell's own refusal.
        Shell(ShellError),
    }

    impl WindowRefused {
        /// The exit code a refusal at start ends the program with.
        const fn code(&self) -> i32 {
            match self {
                Self::NoSurface => app::EXIT_NO_WINDOW,
                Self::Shell(err) => err.code(),
            }
        }
    }

    impl core::fmt::Display for WindowRefused {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::NoSurface => f.write_str("no drawing surface"),
                Self::Shell(err) => err.fmt(f),
            }
        }
    }

    /// Open the player's window at the size `desktop` gives it.
    fn open_window(
        client: &mut WindowClient<app::RtWindowTransport>,
        endpoint: u64,
        desktop: &Desktop,
    ) -> Result<PlayerWindow, WindowRefused> {
        let (width, height) = desktop.window_size(WIN_WIDTH, WIN_HEIGHT);
        let mode = app::mode_for(width, height);
        let surface =
            Surface::new(mode.width_px, mode.height_px).ok_or(WindowRefused::NoSurface)?;
        let least = desktop.window_size(WIN_WIDTH / 2, WIN_HEIGHT / 2);
        let sizing = WindowSizing::Resizable {
            min_width_px: least.0,
            min_height_px: least.1,
            max_width_px: 0,
            max_height_px: 0,
        };
        let pane = WindowPane::open(client, endpoint, &mode, APP_TITLE, sizing)
            .map_err(WindowRefused::Shell)?;
        Ok(PlayerWindow { pane, surface })
    }

    /// Everything the window loop holds.
    struct App {
        client: WindowClient<app::RtWindowTransport>,
        /// The window while it is open: closed, the player stays on the icon
        /// bar, playing.
        window: Option<PlayerWindow>,
        /// The window was asked for while closed — a click on the icon bar, or
        /// a picker — so the loop opens one.
        reopen: bool,
        /// The picker a row asked for while the window was closed, put up
        /// over the window once it is open.
        pending_pick: Option<PickPurpose>,
        player: Player,
        layout: Layout,
        /// The files the playlist names, held until no entry names them.
        files: BTreeMap<EntryId, Arc<File>>,
        link: Arc<Link>,
        workers: Workers,
        covers: Covers,
        /// The context menu while it is open.
        menu: Option<ContextMenu>,
        presented: bool,
        /// The refusals stated in alerts, one up at a time.
        notices: Notices<Spoke>,
        /// The player has every alert up it may, so nothing more is put up
        /// until one is answered.
        alerts_full: bool,
        /// The file the picker chose, while its name is asked for: the
        /// `fd_redeem` handle.
        chosen: Option<u64>,
        /// The files a folder pick delegated, read in as each lands.
        folder: Option<FolderRead>,
        /// The documents handed over since the queue was last read to its end.
        handed: Vec<(String, File)>,
    }

    /// A folder pick's files as they land: those opened, how many could not
    /// be, and how many the pick left out.
    struct FolderRead {
        opened: Vec<(String, File)>,
        refused: u32,
        left_out: u32,
    }

    impl App {
        /// The layout for the window open, at `theme` and `scale`, if one is.
        fn layout_for(&self, theme: &Theme, scale: Scale) -> Option<Layout> {
            let mode = self.window.as_ref()?.pane.mode();
            Some(Layout::for_window(
                mode.width_px,
                mode.height_px,
                theme,
                scale,
                face(theme, scale),
            ))
        }

        /// Adopt the layout for the window open, if one is.
        fn relayout(&mut self, theme: &Theme, scale: Scale) {
            if let Some(layout) = self.layout_for(theme, scale) {
                self.layout = layout;
                self.player.relayout(&self.layout);
            }
        }

        fn window_id(&self) -> Option<u64> {
            self.window.as_ref().map(|window| window.pane.id())
        }

        /// Close the window, leaving the player on the icon bar, playing.
        fn close_window(&mut self) {
            if let Some(window) = self.window.take() {
                let _ = window.pane.close(&mut self.client);
            }
            // A menu hangs from the window, and goes with it.
            self.menu = None;
            self.presented = false;
        }

        /// Open the window again, as the desktop now has it, and put up the
        /// picker a row asked for meanwhile. One the desktop would not open
        /// is stated, the player staying on the icon bar.
        fn reopen(&mut self, desktop: &Desktop, endpoint: u64, (theme, scale): (&Theme, Scale)) {
            if self.window.is_some() {
                return;
            }
            match open_window(&mut self.client, endpoint, desktop) {
                Ok(window) => {
                    self.window = Some(window);
                    self.presented = false;
                    self.relayout(theme, scale);
                    if let Some(purpose) = self.pending_pick.take() {
                        self.pick(&purpose);
                    }
                }
                Err(err) => {
                    self.pending_pick = None;
                    app::report(APP_NAME, format_args!("the window would not open ({err})"));
                }
            }
        }

        /// Put the picker for `purpose` up over the window, opening the window
        /// first where it is closed.
        fn pick(&mut self, purpose: &PickPurpose) {
            let Some(window_id) = self.window_id() else {
                self.pending_pick = Some(*purpose);
                self.reopen = true;
                return;
            };
            if let Err(err) = self.client.pick_file(window_id, *purpose) {
                self.notices.refuse(
                    Spoke::Chosen,
                    "No file chooser could be opened",
                    &format!("the desktop would not offer one ({err})"),
                );
            }
        }

        /// Add `opened` to the playlist and have the playback thread and the
        /// tags worker learn of it.
        fn add(&mut self, opened: Vec<(String, File)>, reported: &mut Region) {
            if opened.is_empty() {
                return;
            }
            let (names, files): (Vec<String>, Vec<File>) = opened.into_iter().unzip();
            let (entries, outcome) = self.player.add(names, &self.layout, reported);
            let held: Vec<(EntryId, Arc<File>)> = entries
                .into_iter()
                .zip(files.into_iter().map(Arc::new))
                .collect();
            for (entry, file) in &held {
                self.files.insert(*entry, Arc::clone(file));
            }
            // The entries reach the playback thread before anything that names
            // them does.
            self.link.order(Order::Add(held));
            self.act(outcome);
        }

        /// Carry out what an input came to, answering whether the listener
        /// asked to quit.
        fn act(&mut self, outcome: Outcome) -> bool {
            for command in outcome.commands {
                if let Command::Edit(edit) = &command {
                    self.forget_removed(edit);
                }
                self.link.order(match command {
                    Command::Control(control) => Order::Control(control),
                    Command::Edit(edit) => Order::Edit(edit),
                });
            }
            for request in outcome.requests {
                self.request(request);
            }
            if let Some(purpose) = &outcome.pick {
                self.pick(purpose);
            }
            if let Some(at) = outcome.menu {
                self.open_menu(at);
            }
            outcome.quit
        }

        /// Let go of the files an edit took out of the playlist.
        fn forget_removed(&mut self, edit: &Edit) {
            match edit {
                Edit::Remove(gone) => {
                    for entry in gone {
                        self.files.remove(entry);
                        self.covers.invalidate(entry);
                    }
                }
                Edit::Clear => {
                    self.files.clear();
                    self.covers.teardown();
                }
                Edit::Move { .. } | Edit::Shuffle(_) | Edit::Repeat(_) => {}
            }
        }

        fn request(&mut self, request: Request) {
            match request {
                Request::Probe(entry) => {
                    if let Some(file) = self.files.get(&entry) {
                        self.workers.tags.ask(Probe {
                            entry,
                            file: Arc::clone(file),
                        });
                    }
                }
                Request::Art { entry, cover, side } => {
                    // A cover already held is drawn by the paint the request
                    // came with.
                    if self.covers.peek(&side, &entry).is_some() {
                        return;
                    }
                    if let Some(file) = self.files.get(&entry) {
                        self.workers.art.ask(Art {
                            entry,
                            file: Arc::clone(file),
                            cover,
                            side,
                        });
                    }
                }
                Request::Devices => self.workers.chores.ask(Chore::Devices),
                Request::Save(saved) => self.workers.chores.ask(Chore::Save(saved)),
            }
        }

        /// Adopt every answer the workers have landed.
        fn collect(&mut self, reported: &mut Region) {
            while let Some(done) = self.workers.chores.worker.collect() {
                self.chore_done(done, reported);
            }
            while let Some(Probed { entry, tags }) = self.workers.tags.worker.collect() {
                let read = match &tags {
                    Tags::Read(info, metadata) => Some((*info, metadata)),
                    Tags::Unread => None,
                };
                let outcome = self.player.probed(entry, read, &self.layout, reported);
                self.act(outcome);
            }
            while let Some(drawn) = self.workers.art.worker.collect() {
                let Drawn {
                    entry,
                    side,
                    picture,
                } = drawn;
                if let Some(picture) = picture.filter(|_| self.files.contains_key(&entry)) {
                    self.covers.retain(&side, entry, Cover(picture));
                    self.player.art_landed(entry, &self.layout, reported);
                }
            }
        }

        fn chore_done(&mut self, done: ChoreDone, reported: &mut Region) {
            match done {
                ChoreDone::Devices(Ok(devices)) => {
                    self.player.devices_listed(devices);
                    self.restate_menu();
                }
                ChoreDone::Devices(Err(err)) => self.player.notify(
                    format!("The output devices could not be listed: {err}"),
                    &self.layout,
                    reported,
                ),
                ChoreDone::Saved(Ok(())) => {}
                ChoreDone::Saved(Err(err)) => self.notices.refuse(
                    Spoke::Settings,
                    "Your settings could not be saved",
                    &format!("{err}"),
                ),
            }
        }

        /// Adopt what the playback thread posted.
        fn adopt_board(&mut self, reported: &mut Region) {
            let (status, notes, links, ended, gone) = {
                let mut board = self.link.board.lock();
                board.woken = false;
                (
                    board.status.take(),
                    core::mem::take(&mut board.notes),
                    core::mem::take(&mut board.links),
                    board.ended.take(),
                    board.gone.take(),
                )
            };
            for (entry, link, metadata) in &links {
                self.player
                    .link_read(*entry, *link, metadata, &self.layout, reported);
            }
            if let Some(status) = status {
                let outcome = self.player.adopt(status, &self.layout, reported);
                self.act(outcome);
            }
            for note in notes {
                let (headline, entry, why) = match note {
                    Note::Skipped { entry, why } => ("could not be played", entry, why),
                    Note::Cut { entry, why } => ("stopped part-way through", entry, why),
                    Note::Opened { .. } => continue,
                };
                let named = self
                    .player
                    .playlist()
                    .get(entry)
                    .map(|row| row.name.as_str());
                let headline = match named {
                    Some(name) if !name.is_empty() => {
                        format!(
                            "\u{201c}{}\u{201d} {headline}",
                            tairix_util::utf8::shown(name)
                        )
                    }
                    _ => format!("A track {headline}"),
                };
                self.notices
                    .refuse(Spoke::Track, &headline, &format!("{why}"));
            }
            if let Some(Ended::Failed(failure)) = ended {
                self.notices
                    .refuse(Spoke::Playback, "Playback stopped", &format!("{failure}"));
            }
            if let Some(failure) = gone {
                self.notices.refuse(
                    Spoke::Playback,
                    "Playback cannot go on",
                    &format!("{failure}"),
                );
            }
        }

        /// Open the context menu at `at`, asking for the output devices again:
        /// the menu is restated with them once they land.
        fn open_menu(&mut self, at: Point) {
            self.request(Request::Devices);
            let devices = self.player.devices().to_vec();
            let rows = self.player.context_menu(&devices);
            let (Ok(anchor), Some(window_id)) =
                (WindowRegion::new(at.x, at.y, 0, 0), self.window_id())
            else {
                return;
            };
            match self.client.open_menu(window_id, anchor, &rows) {
                Ok(open_id) => self.menu = Some(ContextMenu::new(open_id, devices, rows)),
                Err(_) => app::report(APP_NAME, "the desktop composes no menu service"),
            }
        }

        fn menu_is(&self, open_id: u64) -> bool {
            self.menu
                .as_ref()
                .is_some_and(|shown| shown.open_id() == open_id)
        }

        /// Show the open context menu as the player now stands.
        fn restate_menu(&mut self) {
            let (Some(window_id), Some(shown)) = (self.window_id(), self.menu.as_mut()) else {
                return;
            };
            let Some(restatement) = shown.follow(&self.player) else {
                return;
            };
            let restated = self.client.restate_menu(
                window_id,
                shown.open_id(),
                restatement.revision,
                &restatement.rows,
            );
            match restated {
                Ok(()) => shown.restated(restatement),
                Err(err) => {
                    app::report(APP_NAME, format_args!("the menu was not restated ({err})"));
                }
            }
        }

        /// Declare the icon bar's menu as the player's settings now stand.
        fn declare_bar(&mut self, endpoint: u64) {
            let rows = self.player.bar_rows();
            let declared = tairix_window::declaration(endpoint, AppBarClick::RaiseOrOpen, &rows);
            if let Err(refused) = tairix_window::declare_app_bar(&mut self.client, declared) {
                app::report(APP_NAME, format_args!("{refused}"));
            }
        }

        /// Ask for every file a folder pick delegated, each taken in as it
        /// lands ([`Self::picked_file`]).
        fn take_folder(&mut self, left_out: u32, reported: &mut Region) {
            self.folder = Some(FolderRead {
                opened: Vec::new(),
                refused: 0,
                left_out,
            });
            let asked = self.window_id().map_or(Err(Errno::NotFound), |window_id| {
                self.client.ask_picked_files(window_id)
            });
            if let Err(err) = asked {
                app::report(
                    APP_NAME,
                    format_args!("the folder's files could not be asked for ({err})"),
                );
                self.folder_read(reported);
            }
        }

        /// A file the folder pick delegated landed, or the end of them.
        fn picked_file(
            &mut self,
            file: Result<tairix_window::Picked, Errno>,
            reported: &mut Region,
        ) {
            let Some(folder) = self.folder.as_mut() else {
                if let Ok(picked) = file {
                    drop(File::from_delegation(picked.handle));
                }
                return;
            };
            match file {
                Ok(picked) => match File::from_delegation(picked.handle) {
                    Ok(opened) => folder.opened.push((picked.name, opened)),
                    Err(_) => folder.refused = folder.refused.saturating_add(1),
                },
                Err(_) => self.folder_read(reported),
            }
        }

        /// Add what the folder pick brought, saying what it could not.
        fn folder_read(&mut self, reported: &mut Region) {
            let Some(FolderRead {
                opened,
                refused,
                left_out,
            }) = self.folder.take()
            else {
                return;
            };
            if opened.is_empty() && left_out == 0 && refused == 0 {
                self.notices.refuse(
                    Spoke::Folder,
                    "Nothing in that folder can be played",
                    "it holds no file of a kind this player opens",
                );
            }
            self.add(opened, reported);
            let missed = left_out.saturating_add(refused);
            if missed > 0 {
                let unit = if missed == 1 { "file" } else { "files" };
                let notice = AlertSpec::acknowledged(
                    AlertSeverity::Warning,
                    "Not everything in that folder was added",
                    &format!("{missed} more {unit} in it could not be opened."),
                );
                if let Ok(notice) = notice {
                    self.notices.state(Spoke::Folder, notice);
                }
            }
        }

        /// A document handed over landed, or the end of them: the documents
        /// are added together once the queue is read to its end.
        fn handed_over(&mut self, target: Result<Option<Target>, Errno>, reported: &mut Region) {
            match target {
                Ok(Some(Target::Document { name, grant, .. })) => {
                    match File::from_delegation(grant) {
                        Ok(file) => self.handed.push((name, file)),
                        Err(raw) => self.notices.refuse(
                            Spoke::Chosen,
                            &not_opened(&name),
                            &format!("it could not be taken over ({})", Errno::from_syscall(raw)),
                        ),
                    }
                }
                Ok(Some(_)) => {}
                ended => {
                    if let Err(err) = ended {
                        app::report(APP_NAME, format_args!("cannot take an open target ({err})"));
                    }
                    let handed = core::mem::take(&mut self.handed);
                    self.add(handed, reported);
                }
            }
        }

        /// The name of the file the picker chose landed: empty where the
        /// session holds none.
        fn name_chosen(&mut self, name: String, reported: &mut Region) {
            let Some(handle) = self.chosen.take() else {
                return;
            };
            match File::from_delegation(handle) {
                Ok(file) => self.add(alloc::vec![(name, file)], reported),
                Err(ret) => self.notices.refuse(
                    Spoke::Chosen,
                    &not_opened(&name),
                    &format!("it could not be taken over ({})", Errno::from_syscall(ret)),
                ),
            }
        }

        /// Take every answer that has landed.
        fn take_answers(&mut self, reported: &mut Region) {
            while let Some(answer) = self.client.take_answer() {
                match answer {
                    Answer::PickedFile { file, .. } => self.picked_file(file, reported),
                    Answer::OpenTarget(target) => self.handed_over(target, reported),
                    Answer::PickedName { name, .. } => {
                        self.name_chosen(name.unwrap_or_default(), reported);
                    }
                    Answer::FrameReturned { window_id } if Some(window_id) == self.window_id() => {
                        let Some(window) = self.window.as_mut() else {
                            continue;
                        };
                        let presented = window
                            .pane
                            .present_deferred(&mut self.client, &window.surface);
                        if presented.is_err() {
                            reported.add(self.layout.window());
                        }
                    }
                    _ => {}
                }
            }
        }

        /// Act on a request the session refused, which is stated: the window
        /// it would not open is let go, the player staying on the icon bar; an
        /// alert it would not show waits for room or is stated, and a menu it
        /// would not open is forgotten.
        fn refused(&mut self, refused: &Refused) {
            app::report(APP_NAME, refused);
            if self
                .window_id()
                .is_some_and(|window_id| app::refused_create(refused, window_id))
            {
                self.window = None;
                self.menu = None;
                self.presented = false;
                return;
            }
            match refused.request {
                PostedRequest::OpenAlert(alert) => match self.notices.refused(alert, refused.errno)
                {
                    Raised::Full => self.alerts_full = true,
                    Raised::Refused(err, notice) => app::report(
                        APP_NAME,
                        format_args!(
                            "{}: {} (the desktop would not show this: {err})",
                            notice.headline(),
                            notice.detail()
                        ),
                    ),
                    Raised::Nothing | Raised::Up => {}
                },
                PostedRequest::OpenMenu(open) if self.menu_is(open) => self.menu = None,
                _ => {}
            }
        }

        /// Put the next waiting refusal up, unless one is up or the player
        /// has every alert up it may.
        fn raise_notices(&mut self, endpoint: u64) {
            while !self.alerts_full {
                let owner = self.window_id();
                match self.notices.raise(&mut self.client, endpoint, owner) {
                    Raised::Nothing | Raised::Up => return,
                    Raised::Full => self.alerts_full = true,
                    Raised::Refused(err, notice) => app::report(
                        APP_NAME,
                        format_args!(
                            "{}: {} (the desktop would not show this: {err})",
                            notice.headline(),
                            notice.detail()
                        ),
                    ),
                }
            }
        }

        /// Paint what is owed and present it: nothing when nothing was
        /// reported, nor while the window is closed or hidden.
        fn present(
            &mut self,
            repaint: Repaint,
            reported: &Region,
            theme: &Theme,
            scale: Scale,
        ) -> Result<(), Errno> {
            let Some(window) = self.window.as_mut() else {
                return Ok(());
            };
            if self.client.is_hidden(window.pane.id()) {
                return Ok(());
            }
            let mode = *window.pane.mode();
            let repaint = if window.pane.content_released() || !self.presented {
                Repaint::Whole
            } else if repaint == Repaint::Reported && reported.is_empty() {
                Repaint::Nothing
            } else {
                repaint
            };
            let Some(area) = present_damage(&mode, repaint, reported) else {
                return Ok(());
            };
            let clip = tairix_geometry::Rect::new(
                i32::try_from(area.x).unwrap_or(i32::MAX),
                i32::try_from(area.y).unwrap_or(i32::MAX),
                area.width_px,
                area.height_px,
            );
            let art_side = self.layout.art().width;
            let art = self
                .player
                .heard()
                .and_then(|entry| self.covers.peek(&art_side, &entry))
                .map(|cover| &cover.0);
            let (player, layout) = (&self.player, &self.layout);
            let surface = &mut window.surface;
            surface.with_clip(area.x, area.y, area.width_px, area.height_px, |clipped| {
                tairix_music::paint::paint(clipped, player, layout, art, scale, theme, clip);
            });
            let landed = window.pane.present(&mut self.client, &window.surface, area);
            if landed.is_ok() {
                self.presented = true;
            }
            landed
        }

        /// Route one window event, answering whether the player is to quit.
        #[allow(
            clippy::too_many_lines,
            reason = "one arm an event; the arms share the loop's state and the damage they report"
        )]
        fn route(
            &mut self,
            event: &WindowEvent,
            endpoint: u64,
            theme: &Theme,
            scale: Scale,
            reported: &mut Region,
        ) -> bool {
            let now = tairix_rt::clock_get();
            match event {
                WindowEvent::AppBarMenu { item, .. } => {
                    if tairix_window::is_quit(*item) {
                        return true;
                    }
                    // The icon bar's menu offers no output device.
                    let outcome = self.player.choose(item.get(), &[], &self.layout, reported);
                    self.declare_bar(endpoint);
                    return self.act(outcome);
                }
                WindowEvent::OpenRequested => {
                    if let Err(err) = self.client.ask_open_targets() {
                        app::report(
                            APP_NAME,
                            format_args!("cannot ask for what to open ({err})"),
                        );
                    }
                }
                // Closing the window leaves the player on the icon bar, playing;
                // only Quit ends it.
                WindowEvent::CloseRequested { .. }
                | WindowEvent::AlternateCloseRequested { .. } => self.close_window(),
                WindowEvent::AppBarDefault => self.reopen = true,
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => {
                    let mode = app::mode_for(*width_px, *height_px);
                    let adopted = self.window.as_mut().is_some_and(|window| {
                        window
                            .pane
                            .resize_with(&mut self.client, &mode, &mut window.surface)
                            == app::Fitted::Adopted
                    });
                    if adopted {
                        self.relayout(theme, scale);
                        reported.add(self.layout.window());
                    }
                }
                WindowEvent::ContentReleased { .. } => {
                    if let Some(window) = self.window.as_mut() {
                        window.pane.release_frames();
                    }
                }
                WindowEvent::FilePicked { handle, .. } => {
                    // One chosen again before the first's name landed replaces
                    // it; the first is let go rather than held.
                    if let Some(earlier) = self.chosen.replace(*handle) {
                        drop(File::from_delegation(earlier));
                    }
                    let asked = self
                        .window_id()
                        .is_some_and(|window_id| self.client.ask_picked_name(window_id).is_ok());
                    if !asked {
                        self.name_chosen(String::new(), reported);
                    }
                }
                WindowEvent::AlertClosed { alert_id, .. } => {
                    self.alerts_full = false;
                    self.notices.answered(*alert_id);
                }
                WindowEvent::FolderPicked { left_out, .. } => self.take_folder(*left_out, reported),
                WindowEvent::MenuClosed {
                    open_id,
                    revision,
                    outcome,
                    ..
                } => {
                    let Some(shown) = self.menu.take_if(|shown| shown.open_id() == *open_id) else {
                        return false;
                    };
                    if let MenuOutcome::Chosen(item) = outcome {
                        let devices = shown.offered(*revision);
                        let outcome =
                            self.player
                                .choose(item.get(), devices, &self.layout, reported);
                        self.declare_bar(endpoint);
                        return self.act(outcome);
                    }
                }
                WindowEvent::MenuChosen {
                    open_id,
                    revision,
                    item,
                    ..
                } => {
                    let Some(shown) = self.menu.take_if(|shown| shown.open_id() == *open_id) else {
                        return false;
                    };
                    let outcome = self.player.choose(
                        item.get(),
                        shown.offered(*revision),
                        &self.layout,
                        reported,
                    );
                    self.menu = Some(shown);
                    self.declare_bar(endpoint);
                    let quit = self.act(outcome);
                    self.restate_menu();
                    return quit;
                }
                WindowEvent::Key { key, .. } => {
                    let input = key_input_event(*key);
                    let outcome =
                        self.player
                            .input(&input, now, &self.layout, scale, theme, reported);
                    let settings = outcome.commands.iter().any(|command| {
                        matches!(command, Command::Edit(Edit::Shuffle(_) | Edit::Repeat(_)))
                    });
                    let quit = self.act(outcome);
                    if settings {
                        self.declare_bar(endpoint);
                    }
                    return quit;
                }
                WindowEvent::Pointer {
                    x,
                    y,
                    action,
                    modifiers,
                    at_ns,
                    ..
                } => {
                    let held = held_modifiers(*modifiers);
                    let inputs: Vec<InputEvent> = core::iter::once(held)
                        .chain(pointer_input_events(*action, pointer_point(*x, *y)))
                        .collect();
                    for input in inputs {
                        let outcome =
                            self.player
                                .input(&input, *at_ns, &self.layout, scale, theme, reported);
                        let settings = outcome.commands.iter().any(|command| {
                            matches!(command, Command::Edit(Edit::Shuffle(_) | Edit::Repeat(_)))
                        });
                        if self.act(outcome) {
                            return true;
                        }
                        if settings {
                            self.declare_bar(endpoint);
                        }
                    }
                }
                WindowEvent::Scrolled { x, y, dx, dy, .. } => {
                    for input in scroll_input_events(pointer_point(*x, *y), *dx, *dy) {
                        let outcome =
                            self.player
                                .input(&input, now, &self.layout, scale, theme, reported);
                        self.act(outcome);
                    }
                }
                // A cancelled pick leaves the playlist as it was.
                _ => {}
            }
            false
        }
    }

    /// The headline of an alert saying the file `name` could not be opened.
    fn not_opened(name: &str) -> String {
        if name.is_empty() {
            String::from("The file could not be opened")
        } else {
            tairix_util::utf8::refusal_headline(name, "opened")
        }
    }

    /// The face the player sets its text in.
    fn face(theme: &Theme, scale: Scale) -> BitmapFont {
        BitmapFont::for_role(theme.fonts(), TextRole::Body, scale)
    }

    /// A fresh draw to seed the player's shuffles.
    fn shuffle_seed() -> u64 {
        let mut word = [0u8; 8];
        let _ = tairix_rt::random_fill(&mut word);
        u64::from_le_bytes(word)
    }

    /// The document this program was handed on [`STDIN`] at spawn: a
    /// read-only descriptor the kernel cloned in, named by the argument after
    /// the role.
    fn inherited() -> (String, File) {
        let name = tairix_rt::arg(2)
            .and_then(|raw| core::str::from_utf8(raw).ok())
            .map(|path| String::from(path.rsplit('/').next().unwrap_or(path)))
            .unwrap_or_default();
        (name, File::adopt(STDIN))
    }

    /// The device `saved` names, among `devices`: its id this boot, or the
    /// default's when it is not connected.
    fn device_id(saved: &Saved, devices: &[Device]) -> Option<u32> {
        let target = saved.device.as_deref()?;
        devices
            .iter()
            .find(|device| device.target == target)
            .map(|device| device.id)
    }

    /// The player's whole life.
    #[allow(
        clippy::too_many_lines,
        reason = "one linear bring-up plus one event loop; splitting the loop would separate the park from the drain it must follow"
    )]
    fn main() -> i32 {
        // The two sandbox roles, before anything else: a sound file or a
        // picture is untrusted input, decoded by a capability-empty child
        // this same binary is re-entered as.
        if session_worker_role() {
            serve_session_stdio(&mut AudioDecodeService::new());
            return 0;
        }
        if worker_role() {
            return serve_stdio(&mut ImageRenderService::default()).exit_code();
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
        let handed = tairix_rt::arg(1)
            .is_some_and(|arg| arg == DOCUMENT_ROLE_ARG)
            .then(inherited);

        // Read once, before any window exists: the loop never waits on a
        // service.
        let saved = load_settings();
        let devices = list_devices().unwrap_or_default();
        let device = device_id(&saved, &devices);
        let missing_device = saved.device.is_some() && device.is_none();
        let settings = Settings {
            gain: saved.gain,
            device_id: device.unwrap_or(0),
            normalise: saved.normalise,
        };

        let mut client = WindowClient::new(app::RtWindowTransport);
        let (mut desktop, mut themes) = match app::bring_up_desktop(&mut client) {
            Ok(pair) => pair,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let Some(server) = client.session() else {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "the desktop did not identify itself",
            );
        };
        let binding = match app::bind_event_mailbox() {
            Ok(binding) => binding,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let (endpoint, set) = (binding.endpoint(), binding.set());

        let player = Player::new(saved, shuffle_seed(), desktop.info().double_click());
        let link = Arc::new(Link::new());
        let opening = player.opening_edits();
        let thread_link = Arc::clone(&link);
        if let Err(err) = Thread::spawn(move || playback(&thread_link, settings, opening)) {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                format_args!("no playback thread ({err})"),
            );
        }
        if let Err(err) = app::watch_wake(set, &link.posted, PLAYBACK_TOKEN) {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                format_args!("playback wake refused ({err})"),
            );
        }
        let workers = match Workers::start(set) {
            Ok(workers) => workers,
            Err(why) => return app::fail(APP_NAME, app::EXIT_NO_EVENTS, why),
        };

        let theme = themes.active();
        let scale = desktop.scale();
        let window = match open_window(&mut client, endpoint, &desktop) {
            Ok(window) => window,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let mode = *window.pane.mode();
        let frame_bytes = usize::try_from(u64::from(mode.width_px) * u64::from(mode.height_px) * 4)
            .unwrap_or(usize::MAX);
        let covers = working_set_ui_cache(
            "music.covers",
            SEAT_PRIMARY,
            (
                frame_bytes,
                tairix_procinfo::memory_total_for_budget(&tairix_procinfo::IpcTransport),
            ),
            ART_ENTRY_METADATA_BYTES,
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
            BuildFastHash::new(),
        );
        let layout = Layout::for_window(
            mode.width_px,
            mode.height_px,
            theme,
            scale,
            face(theme, scale),
        );
        let mut app_state = App {
            client,
            window: Some(window),
            reopen: false,
            pending_pick: None,
            player,
            layout,
            files: BTreeMap::new(),
            link,
            workers,
            covers,
            menu: None,
            presented: false,
            notices: Notices::default(),
            alerts_full: false,
            chosen: None,
            folder: None,
            handed: Vec::new(),
        };
        app_state.player.relayout(&app_state.layout);
        app_state.player.devices_listed(devices);
        app_state.declare_bar(endpoint);
        let mut reported = damage::sink();
        if missing_device {
            app_state.player.notify(
                String::from("Your chosen output is not connected; playing on the default"),
                &app_state.layout,
                &mut reported,
            );
        }
        if let Some(document) = handed {
            app_state.add(alloc::vec![document], &mut reported);
        }
        if app_state
            .present(Repaint::Whole, &reported, theme, scale)
            .is_err()
        {
            return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "first present refused");
        }

        let desktop_moved = Cell::new(false);
        let pressure_moved = Cell::new(false);
        let deadline_ns = Cell::new(None);
        let (chores, tags, art) = (
            Arc::clone(&app_state.workers.chores.worker),
            Arc::clone(&app_state.workers.tags.worker),
            Arc::clone(&app_state.workers.art.worker),
        );
        let link_handle = Arc::clone(&app_state.link);
        let mut events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(endpoint, server),
            set,
            answers: [
                (CHORES_TOKEN, chores.wake()),
                (TAGS_TOKEN, tags.wake()),
                (ART_TOKEN, art.wake()),
            ],
            link: &link_handle,
            desktop_moved: &desktop_moved,
            pressure_moved: &pressure_moved,
            deadline_ns: &deadline_ns,
        });
        // The event the last park woke for, served with the rest of the input
        // that arrived since, so the whole burst costs one frame.
        let mut woken: Option<WindowEvent> = None;
        loop {
            let mut reported = damage::sink();
            let mut repaint = Repaint::Reported;
            if desktop_moved.replace(false) {
                match app::adopt_desktop(&mut desktop, &mut themes) {
                    Ok(true) => {
                        app_state.relayout(themes.active(), desktop.scale());
                        repaint = Repaint::Whole;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        app::report(APP_NAME, format_args!("desktop change refused: {err}"));
                    }
                }
            }
            if pressure_moved.replace(false) {
                app_state.covers.enforce_pressure();
            }
            let (theme, scale) = (themes.active(), desktop.scale());

            // What has landed first, so a picture or a track's name appears the
            // moment it is ready rather than at the next input.
            app_state.collect(&mut reported);
            app_state.workers.hand_on();
            app_state.adopt_board(&mut reported);

            // Then every queued input, before anything is painted, so a burst
            // costs one frame.
            if let Some(event) = woken.take() {
                if app_state.route(&event, endpoint, theme, scale, &mut reported) {
                    app_state.link.leave();
                    return 0;
                }
            }
            loop {
                match events.try_wait(&mut app_state.client) {
                    Ok(Some(event)) => {
                        if app_state.route(&event, endpoint, theme, scale, &mut reported) {
                            app_state.link.leave();
                            return 0;
                        }
                    }
                    Ok(None) => break,
                    Err(EventError::Mailbox(_)) => {
                        app_state.link.leave();
                        return app::fail(
                            APP_NAME,
                            app::EXIT_CHANNEL_LOST,
                            "the event channel died",
                        );
                    }
                    Err(EventError::Undecodable(_)) => {
                        app::report(APP_NAME, "a malformed window event was refused");
                    }
                    Err(EventError::Refused(refused)) => app_state.refused(&refused),
                }
            }
            app_state.take_answers(&mut reported);
            if core::mem::take(&mut app_state.reopen) {
                app_state.reopen(&desktop, endpoint, (theme, scale));
            }
            // A held scrollbar press may be due whether the last wait ended on
            // its deadline or on input.
            let now = tairix_rt::clock_get();
            app_state
                .player
                .scroll_repeat(now, &app_state.layout, scale, theme, &mut reported);
            app_state.raise_notices(endpoint);
            if app_state.present(repaint, &reported, theme, scale).is_err() {
                app_state.link.leave();
                return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "present refused");
            }
            deadline_ns.set(app_state.player.scroll_repeat_deadline(now));
            match events.wait(&mut app_state.client) {
                Ok(Some(event)) => woken = Some(event),
                Ok(None) => {}
                Err(EventError::Mailbox(_)) => {
                    app_state.link.leave();
                    return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "the event channel died");
                }
                Err(EventError::Undecodable(_)) => {
                    app::report(APP_NAME, "a malformed window event was refused");
                }
                Err(EventError::Refused(refused)) => app_state.refused(&refused),
            }
        }
    }

    tairix_rt::entry!(main);
}

/// The host stub: this binary is a freestanding program on the Tier-1
/// targets, so on the host it exists only to keep the file covered by the
/// workspace's build, lint and format passes.
#[cfg(not(freestanding))]
fn main() {}
