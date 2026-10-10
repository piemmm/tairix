//! The `Run` entry-point binary of `music.app`: the window, the playback
//! thread, and the worker.
//!
//! A pure-Rust program over `tairix-rt`, and its own two sandboxed decoders:
//! re-entered in the sandbox's session-worker role it serves the audio decode
//! protocol, and in its worker role the image renderer, each over the two
//! pipes the sandbox wires and nothing else.
//!
//! # Three threads, none of which polls
//!
//! The **window** thread owns the window and the player's state and never
//! waits on anything but its own wait-set. Playback is the shared engine on a
//! **playback** thread of its own, parked on the decoder's pipes, the stream's
//! mailbox and the orders the window posts; it posts its status back and wakes
//! the window. Everything else that waits — a track's tags, its cover, the
//! list of output devices, the listener's settings — is a job for the
//! **worker**, whose answers wake the window in turn.
//!
//! The app holds no filesystem capability. It plays exactly the files the user
//! chose in the session's picker or handed it from the file manager, each a
//! delegated descriptor shared by the window, the worker and the playback
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
    use tairix_abi::input::KeyInput;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::window_ipc::{AppBarClick, MenuOutcome, WindowEvent, WindowRegion};
    use tairix_abi::{Errno, DOCUMENT_ROLE_ARG, STDIN};
    use tairix_appdata::{RtHost, Settings as Store};
    use tairix_audio::live::RtAudio;
    use tairix_audio::stream::devices;
    use tairix_controls::damage;
    use tairix_font::BitmapFont;
    use tairix_geometry::{Point, Region, Scale};
    use tairix_hash::BuildFastHash;
    use tairix_input::InputEvent;
    use tairix_music::view::{Command, Device, Outcome, Player, Request, Saved};
    use tairix_music::{Edit, Layout, Playlist, Repeat, WIN_HEIGHT, WIN_WIDTH};
    use tairix_player::rt::{draw_seed, live, EngineWaits, HeldFiles, RtEngine, Tokens};
    use tairix_player::{Control, EntryId, Failure, Note, Outcome as Ended, Settings, Status};
    use tairix_raster::{Color, Surface};
    use tairix_reclaim::desktop::working_set_ui_cache;
    use tairix_reclaim::{CachedBytes, ReclaimCache};
    use tairix_rt::sync::{Mutex, WorkerWake};
    use tairix_rt::thread::Thread;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_rt::File;
    use tairix_sandbox::audiodecode::{
        session_bounds, AudioDecodeClient, AudioDecodeService, DecodeEvent,
    };
    use tairix_sandbox::imagerender::{
        render_thumbnail, upload_document, ImageRenderService, MAX_ICON_SIDE,
    };
    use tairix_sandbox::rt::{
        serve_session_stdio, serve_stdio, session_worker_role, worker_role, RtLauncher,
        RtSessionLauncher, SessionMembers,
    };
    use tairix_sandbox::session::SessionError;
    use tairix_sandbox::supervise::SupervisedSession;
    use tairix_sandbox::ParserSandbox;
    use tairix_sound::{CoverRange, Metadata, SoundInfo};
    use tairix_theme::{TextRole, Theme};
    use tairix_util::defer::JobQueue;
    use tairix_window::app::{self, Wake, WindowPane};
    use tairix_window::{
        key_input_event, pointer_input_events, pointer_point, present_damage, scroll_input_events,
        EventDrain, EventError, EventMailbox, EventSource, Parked, Repaint, Target, WindowClient,
        WindowEvents, WindowSizing,
    };

    /// The name this program states its refusals under.
    const APP_NAME: &str = "music";

    /// The window's title.
    const APP_TITLE: &str = "Music";

    /// The window loop's wake tokens: the worker's answers, then the playback
    /// thread's posts.
    const WORKER_TOKEN: u64 = app::FIRST_APP_TOKEN;
    const PLAYBACK_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;

    /// The playback thread's wait-set tokens.
    const ENGINE: Tokens = Tokens {
        decoder_read: 1,
        decoder_write: 2,
        notify: 3,
    };
    const ORDERS: u64 = 4;

    /// The prober's wait-set tokens.
    const PROBE_READ: u64 = 1;
    const PROBE_WRITE: u64 = 2;

    /// Jobs the worker holds at once; the window keeps the rest in order and
    /// hands them on as answers come back, so none is dropped.
    const WORKER_QUEUE: usize = 16;

    /// The longest a track's tags may take to read before the decoder reading
    /// them is given up as hung: generous for a slow disk, and a bound, so
    /// one stuck file cannot hold every later job behind it.
    const PROBE_BUDGET_NS: u64 = 10_000_000_000;

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
            if !engine.take_changed() && notes.is_empty() {
                return;
            }
            let wake = {
                let mut board = self.board.lock();
                board.status = Some(engine.status().clone());
                board.notes.extend(notes);
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

    // ---- the worker ----------------------------------------------------

    /// What the worker is asked.
    enum Job {
        Probe {
            entry: EntryId,
            file: Arc<File>,
        },
        Art {
            entry: EntryId,
            file: Arc<File>,
            cover: CoverRange,
            side: u32,
        },
        Devices,
        Save(Saved),
    }

    /// What it answers.
    enum Answer {
        Probed {
            entry: EntryId,
            read: Option<(SoundInfo, Metadata)>,
        },
        Art {
            entry: EntryId,
            side: u32,
            picture: Option<Surface>,
        },
        Devices(Result<Vec<Device>, Errno>),
        Saved(Result<(), Errno>),
    }

    /// What the worker keeps between jobs: the decoders, started on first use
    /// and kept, so a folder of tracks costs one decoder process, not one each.
    #[derive(Default)]
    struct Workshop {
        prober: Option<Prober>,
        renderer: Option<ParserSandbox<RtLauncher, tairix_rt::LogSink>>,
    }

    fn serve(shop: &mut Workshop, job: &mut Job) -> Answer {
        match job {
            Job::Probe { entry, file } => {
                if shop.prober.is_none() {
                    shop.prober = Prober::new();
                }
                let read = shop.prober.as_mut().and_then(|prober| prober.probe(file));
                Answer::Probed {
                    entry: *entry,
                    read,
                }
            }
            Job::Art {
                entry,
                file,
                cover,
                side,
            } => {
                let renderer = shop.renderer.get_or_insert_with(|| {
                    ParserSandbox::new(RtLauncher::own_binary(), tairix_rt::LogSink)
                });
                Answer::Art {
                    entry: *entry,
                    side: *side,
                    picture: cover_picture(renderer, file, *cover, *side),
                }
            }
            Job::Devices => Answer::Devices(list_devices()),
            Job::Save(saved) => Answer::Saved(save_settings(saved)),
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
        let fitted = render_thumbnail(renderer, side, None).ok()?;
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

    /// A track's tags and shape, read by a decoder in the parser sandbox the
    /// worker drives to completion.
    struct Prober {
        session: SupervisedSession<RtSessionLauncher, tairix_rt::LogSink>,
        client: AudioDecodeClient,
        set: u64,
        members: SessionMembers,
        reading: Vec<u8>,
    }

    /// What one frame from the prober's decoder came to.
    enum Heard {
        Need { offset: u64, len: usize },
        Opened,
        Refused,
        Other,
        Broken,
    }

    impl Prober {
        fn new() -> Option<Self> {
            let bounds = session_bounds().ok()?;
            let set = u64::try_from(tairix_rt::waitset_create()).ok()?;
            Some(Self {
                session: SupervisedSession::new(
                    RtSessionLauncher::own_binary(),
                    bounds,
                    tairix_rt::LogSink,
                ),
                client: AudioDecodeClient::new(),
                set,
                members: SessionMembers::new(set, PROBE_READ, PROBE_WRITE),
                reading: Vec::new(),
            })
        }

        /// `file`'s stream and tags, or `None` when no decoder can read it in
        /// the budget.
        fn probe(&mut self, file: &File) -> Option<(SoundInfo, Metadata)> {
            let len = file.regular_len().ok()??;
            let started = tairix_rt::clock_get();
            let budget_ends = started.saturating_add(PROBE_BUDGET_NS);
            if !self.ensure_live(budget_ends) {
                return None;
            }
            self.client.open(len, None, draw_seed()?).ok()?;
            loop {
                let now = tairix_rt::clock_get();
                if now >= budget_ends {
                    self.give_up(now);
                    return None;
                }
                self.send(now);
                match self.take(now) {
                    Heard::Need { offset, len } => self.serve_need(file, offset, len)?,
                    Heard::Opened => {
                        let info = *self.client.info()?;
                        return Some((info, self.client.metadata()?.clone()));
                    }
                    Heard::Refused => return None,
                    Heard::Broken => {
                        self.give_up(now);
                        return None;
                    }
                    Heard::Other => {}
                }
                self.park(budget_ends)?;
            }
        }

        /// Have a decoder live, waiting out a paced replacement within the
        /// budget.
        fn ensure_live(&mut self, budget_ends: u64) -> bool {
            for _ in 0..2 {
                if self.session.is_live() {
                    return true;
                }
                let now = tairix_rt::clock_get();
                if let Some(at) = self.session.restart_deadline().filter(|&at| at > now) {
                    if at >= budget_ends {
                        return false;
                    }
                    let mut token = 0;
                    let _ = tairix_rt::waitset_wait(self.set, at - now, &mut token);
                }
                if self.session.start(tairix_rt::clock_get()).is_some() {
                    self.client = AudioDecodeClient::new();
                }
            }
            self.session.is_live()
        }

        fn send(&mut self, now: u64) {
            while let Some(frame) = self.client.outgoing() {
                match self.session.send(frame) {
                    Ok(()) => self.client.sent(),
                    Err(SessionError::OutboundFull) => break,
                    Err(_) => {
                        self.give_up(now);
                        return;
                    }
                }
            }
            let _ = self.session.on_writable(now);
        }

        fn take(&mut self, now: u64) -> Heard {
            let client = &mut self.client;
            match self
                .session
                .recv(now, |frame| match client.on_frame(frame) {
                    Ok(DecodeEvent::Need { offset, len }) => Heard::Need { offset, len },
                    Ok(DecodeEvent::Opened) => Heard::Opened,
                    Ok(DecodeEvent::Refused(_)) => Heard::Refused,
                    Ok(_) => Heard::Other,
                    Err(_) => Heard::Broken,
                }) {
                Ok(Some(heard)) => heard,
                Ok(None) => Heard::Other,
                Err(_) => Heard::Broken,
            }
        }

        fn serve_need(&mut self, file: &File, offset: u64, len: usize) -> Option<()> {
            if self.reading.len() < len {
                self.reading.try_reserve(len - self.reading.len()).ok()?;
                self.reading.resize(len, 0);
            }
            let read = file.read_at(offset, &mut self.reading[..len]);
            let supplied = match read {
                Ok(read) if read == len => self.client.supply(&self.reading[..len]),
                _ => self.client.unreadable(),
            };
            supplied.ok()
        }

        /// Wait for the decoder's next answer or room, no later than
        /// `budget_ends`.
        fn park(&mut self, budget_ends: u64) -> Option<()> {
            let (descriptors, read, write) = (
                self.session.descriptors(),
                self.session.wants_read(),
                self.session.wants_write(),
            );
            self.members.sync(descriptors, read, write).ok()?;
            let now = tairix_rt::clock_get();
            let mut token = 0;
            let waited =
                tairix_rt::waitset_wait(self.set, budget_ends.saturating_sub(now), &mut token);
            let now = tairix_rt::clock_get();
            if waited != 0 {
                return (Errno::from_syscall(waited) == Errno::TimedOut).then_some(());
            }
            let failed = match token {
                PROBE_READ => self.session.on_readable(now).is_err(),
                PROBE_WRITE => self.session.on_writable(now).is_err(),
                _ => false,
            };
            (!failed).then_some(())
        }

        /// The decoder broke or hung: replace it before the next probe.
        fn give_up(&mut self, now: u64) {
            self.session
                .condemn(now, "a track's tags were not read in the time allowed");
            self.client = AudioDecodeClient::new();
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
    /// state, the worker's answers and the playback thread's posts.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        worker: &'a Workers,
        link: &'a Link,
        desktop_moved: &'a Cell<bool>,
        pressure_moved: &'a Cell<bool>,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            match app::park(self.set)? {
                // Each wake is a level, so one left undrained would make the
                // park spin.
                Wake::App(WORKER_TOKEN) => {
                    self.worker.wake().drain();
                    Ok(Parked::Interrupted)
                }
                Wake::App(PLAYBACK_TOKEN) => {
                    self.link.posted.drain();
                    Ok(Parked::Interrupted)
                }
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

    type Workers = Worker<Workshop, Job, Answer, JobQueue<Job, Answer>>;

    /// Everything the window loop holds.
    struct App {
        client: WindowClient<app::RtWindowTransport>,
        pane: WindowPane,
        surface: Surface,
        player: Player,
        layout: Layout,
        /// The files the playlist names, held until no entry names them.
        files: BTreeMap<EntryId, Arc<File>>,
        link: Arc<Link>,
        worker: Arc<Workers>,
        /// Jobs the worker had no room for, in the order they were asked.
        backlog: VecDeque<Job>,
        covers: Covers,
        /// The open context menu's id.
        menu: Option<u64>,
        presented: bool,
    }

    impl App {
        fn layout_for(&self, theme: &Theme, scale: Scale) -> Layout {
            let mode = self.pane.mode();
            Layout::for_window(
                mode.width_px,
                mode.height_px,
                theme,
                scale,
                face(theme, scale),
            )
        }

        /// Add `opened` to the playlist and have the playback thread and the
        /// worker learn of it.
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
            if let Some(purpose) = outcome.pick {
                if let Err(err) = self.client.pick_file(self.pane.id(), purpose) {
                    app::report(
                        APP_NAME,
                        format_args!("the desktop offered no file chooser ({err})"),
                    );
                }
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
            let job = match request {
                Request::Probe(entry) => {
                    let Some(file) = self.files.get(&entry) else {
                        return;
                    };
                    Job::Probe {
                        entry,
                        file: Arc::clone(file),
                    }
                }
                Request::Art { entry, cover, side } => {
                    // A cover already held is drawn by the paint the request
                    // came with.
                    if self.covers.peek(&side, &entry).is_some() {
                        return;
                    }
                    let Some(file) = self.files.get(&entry) else {
                        return;
                    };
                    Job::Art {
                        entry,
                        file: Arc::clone(file),
                        cover,
                        side,
                    }
                }
                Request::Devices => Job::Devices,
                Request::Save(saved) => Job::Save(saved),
            };
            self.backlog.push_back(job);
            self.hand_on();
        }

        /// Give the worker as much of the backlog as it has room for.
        fn hand_on(&mut self) {
            while let Some(job) = self.backlog.pop_front() {
                if let Err(job) = self.worker.submit(job) {
                    self.backlog.push_front(job);
                    return;
                }
            }
        }

        /// Adopt one answer from the worker.
        fn answered(&mut self, answer: Answer, reported: &mut Region) {
            match answer {
                Answer::Probed { entry, read } => {
                    let read = read.as_ref().map(|(info, metadata)| (*info, metadata));
                    let outcome = self.player.probed(entry, read, &self.layout, reported);
                    self.act(outcome);
                }
                Answer::Art {
                    entry,
                    side,
                    picture,
                } => {
                    if let Some(picture) = picture.filter(|_| self.files.contains_key(&entry)) {
                        self.covers.retain(&side, entry, Cover(picture));
                        self.player.art_landed(entry, &self.layout, reported);
                    }
                }
                Answer::Devices(Ok(devices)) => self.player.devices_listed(devices),
                Answer::Devices(Err(err)) => self.player.notify(
                    format!("The output devices could not be listed: {err}"),
                    &self.layout,
                    reported,
                ),
                Answer::Saved(Ok(())) => {}
                Answer::Saved(Err(err)) => self.player.notify(
                    format!("Your settings could not be saved: {err}"),
                    &self.layout,
                    reported,
                ),
            }
        }

        /// Adopt what the playback thread posted.
        fn adopt_board(&mut self, reported: &mut Region) {
            let (status, notes, ended, gone) = {
                let mut board = self.link.board.lock();
                board.woken = false;
                (
                    board.status.take(),
                    core::mem::take(&mut board.notes),
                    board.ended.take(),
                    board.gone.take(),
                )
            };
            if let Some(status) = status {
                let outcome = self.player.adopt(status, &self.layout, reported);
                self.act(outcome);
            }
            for note in notes {
                let (Note::Skipped { entry, why } | Note::Cut { entry, why }) = note else {
                    continue;
                };
                let name = self
                    .player
                    .playlist()
                    .get(entry)
                    .map_or_else(String::new, |row| row.name.clone());
                self.player
                    .notify(format!("{name}: {why}"), &self.layout, reported);
            }
            if let Some(Ended::Failed(failure)) = ended {
                self.player.notify(
                    format!("Playback stopped: {failure}"),
                    &self.layout,
                    reported,
                );
            }
            if let Some(failure) = gone {
                app::report(APP_NAME, format_args!("playback ended: {failure}"));
                self.player.notify(
                    format!("Playback cannot go on: {failure}"),
                    &self.layout,
                    reported,
                );
            }
        }

        fn open_menu(&mut self, at: Point) {
            self.request(Request::Devices);
            let menu = self.player.context_menu();
            let Ok(anchor) = WindowRegion::new(at.x, at.y, 0, 0) else {
                return;
            };
            match self.client.open_menu(self.pane.id(), anchor, &menu) {
                Ok(open) => self.menu = Some(open),
                Err(_) => app::report(APP_NAME, "the desktop composes no menu service"),
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

        /// Take every file a folder pick delegated.
        fn take_folder(&mut self, left_out: u32, reported: &mut Region) {
            let mut opened = Vec::new();
            let mut refused = 0u32;
            while let Ok(picked) = self.client.take_picked_file(self.pane.id()) {
                match File::from_delegation(picked.handle) {
                    Ok(file) => opened.push((String::from(picked.name.as_str()), file)),
                    Err(_) => refused = refused.saturating_add(1),
                }
            }
            if opened.is_empty() && left_out == 0 && refused == 0 {
                self.player.notify(
                    String::from("That folder holds nothing this player opens"),
                    &self.layout,
                    reported,
                );
            }
            self.add(opened, reported);
            let missed = left_out.saturating_add(refused);
            if missed > 0 {
                self.player.notify(
                    format!("{missed} more files in that folder were not opened"),
                    &self.layout,
                    reported,
                );
            }
        }

        /// Take every document handed over since the last wake.
        fn take_targets(&mut self, reported: &mut Region) {
            let mut opened = Vec::new();
            while let Ok(Some(target)) = self.client.take_open_target() {
                let Target::Document { name, grant, .. } = target else {
                    continue;
                };
                if let Ok(file) = File::from_delegation(grant) {
                    opened.push((name, file));
                }
            }
            self.add(opened, reported);
        }

        /// Paint what is owed and present it.
        fn present(
            &mut self,
            repaint: Repaint,
            reported: &Region,
            theme: &Theme,
            scale: Scale,
        ) -> Result<(), Errno> {
            let mode = *self.pane.mode();
            let repaint = if self.pane.content_released() || !self.presented {
                Repaint::Whole
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
            let (player, layout, surface) = (&self.player, &self.layout, &mut self.surface);
            surface.with_clip(area.x, area.y, area.width_px, area.height_px, |clipped| {
                tairix_music::paint::paint(clipped, player, layout, art, scale, theme, clip);
            });
            let landed = self.pane.present(&mut self.client, &self.surface, area);
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
                WindowEvent::AppBarMenu { item } => {
                    if tairix_window::is_quit(*item) {
                        return true;
                    }
                    let outcome = self.player.choose(item.get(), &self.layout, reported);
                    self.declare_bar(endpoint);
                    return self.act(outcome);
                }
                WindowEvent::OpenRequested => self.take_targets(reported),
                WindowEvent::CloseRequested { .. }
                | WindowEvent::AlternateCloseRequested { .. } => {
                    return true;
                }
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => {
                    let mode = app::mode_for(*width_px, *height_px);
                    if self
                        .pane
                        .resize_with(&mut self.client, &mode, &mut self.surface)
                    {
                        self.layout = self.layout_for(theme, scale);
                        self.player.relayout(&self.layout);
                        reported.add(self.layout.window());
                    }
                }
                WindowEvent::ContentReleased { .. } => self.pane.release_frames(),
                WindowEvent::FilePicked { handle, .. } => {
                    let name = self
                        .client
                        .take_picked_name(self.pane.id())
                        .unwrap_or_default();
                    match File::from_delegation(*handle) {
                        Ok(file) => self.add(alloc::vec![(name, file)], reported),
                        Err(ret) => self.player.notify(
                            format!(
                                "The chosen file could not be opened: {}",
                                Errno::from_syscall(ret)
                            ),
                            &self.layout,
                            reported,
                        ),
                    }
                }
                WindowEvent::FolderPicked { left_out, .. } => self.take_folder(*left_out, reported),
                WindowEvent::MenuClosed {
                    open_id, outcome, ..
                } => {
                    if self.menu != Some(*open_id) {
                        return false;
                    }
                    self.menu = None;
                    if let MenuOutcome::Chosen(item) = outcome {
                        let outcome = self.player.choose(item.get(), &self.layout, reported);
                        self.declare_bar(endpoint);
                        return self.act(outcome);
                    }
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
                    ..
                } => {
                    let held = key_input_event(KeyInput::ModifiersChanged {
                        modifiers: *modifiers,
                    });
                    let inputs: Vec<InputEvent> = core::iter::once(held)
                        .chain(pointer_input_events(*action, pointer_point(*x, *y)))
                        .collect();
                    for input in inputs {
                        let outcome =
                            self.player
                                .input(&input, now, &self.layout, scale, theme, reported);
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
                // The window is open for the player's whole life, so a click
                // on its icon-bar slot only raises it, which the session does;
                // a cancelled pick leaves the playlist as it was.
                _ => {}
            }
            false
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
        let worker = match Worker::queued(
            serve,
            Workshop::default(),
            WorkerWake::create(),
            WORKER_QUEUE,
        ) {
            Ok(worker) => Arc::new(worker),
            Err(_) => return app::fail(APP_NAME, app::EXIT_NO_EVENTS, "no memory for the worker"),
        };
        if let Err(reason) = Worker::start(&worker) {
            app::report(
                APP_NAME,
                format_args!("no worker thread ({reason:?}); jobs run on the window's thread"),
            );
        }
        let _worker_guard = WorkerGuard::new(&worker);
        if let Err(err) = app::watch_wake(set, worker.wake(), WORKER_TOKEN) {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                format_args!("worker wake refused ({err})"),
            );
        }

        let theme = themes.active();
        let scale = desktop.scale();
        let (width, height) = desktop.window_size(WIN_WIDTH, WIN_HEIGHT);
        let mode = app::mode_for(width, height);
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            return app::fail(APP_NAME, app::EXIT_NO_WINDOW, "no drawing surface");
        };
        let least = desktop.window_size(WIN_WIDTH / 2, WIN_HEIGHT / 2);
        let sizing = WindowSizing::Resizable {
            min_width_px: least.0,
            min_height_px: least.1,
            max_width_px: 0,
            max_height_px: 0,
        };
        let (pane, replied) =
            match WindowPane::open(&mut client, endpoint, &mode, APP_TITLE, sizing) {
                Ok(opened) => opened,
                Err(err) => return app::fail(APP_NAME, err.code(), err),
            };
        if replied != server {
            let _ = pane.close(&mut client);
            return app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "a window reply came from another sender",
            );
        }
        let frame_bytes = usize::try_from(u64::from(mode.width_px) * u64::from(mode.height_px) * 4)
            .unwrap_or(usize::MAX);
        let covers = working_set_ui_cache(
            "music.covers",
            SEAT_PRIMARY,
            frame_bytes,
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
            pane,
            surface,
            player,
            layout,
            files: BTreeMap::new(),
            link,
            worker,
            backlog: VecDeque::new(),
            covers,
            menu: None,
            presented: false,
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
        let worker_handle = Arc::clone(&app_state.worker);
        let link_handle = Arc::clone(&app_state.link);
        let mut events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(endpoint, server),
            set,
            worker: &worker_handle,
            link: &link_handle,
            desktop_moved: &desktop_moved,
            pressure_moved: &pressure_moved,
        });
        loop {
            let mut reported = damage::sink();
            let mut repaint = Repaint::Reported;
            if desktop_moved.replace(false) {
                match app::adopt_desktop(&mut desktop, &mut themes) {
                    Ok(true) => {
                        let (theme, scale) = (themes.active(), desktop.scale());
                        app_state.layout = app_state.layout_for(theme, scale);
                        app_state.player.relayout(&app_state.layout);
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
            while let Some(answer) = app_state.worker.collect() {
                app_state.answered(answer, &mut reported);
            }
            app_state.hand_on();
            app_state.adopt_board(&mut reported);

            // Then every queued input, before anything is painted, so a burst
            // costs one frame.
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
                }
            }
            if app_state.present(repaint, &reported, theme, scale).is_err() {
                app_state.link.leave();
                return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "present refused");
            }
            match events.wait(&mut app_state.client) {
                Ok(Some(event)) => {
                    let mut reported = damage::sink();
                    if app_state.route(&event, endpoint, theme, scale, &mut reported) {
                        app_state.link.leave();
                        return 0;
                    }
                    if app_state
                        .present(Repaint::Reported, &reported, theme, scale)
                        .is_err()
                    {
                        app_state.link.leave();
                        return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "present refused");
                    }
                }
                Ok(None) => {}
                Err(EventError::Mailbox(_)) => {
                    app_state.link.leave();
                    return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "the event channel died");
                }
                Err(EventError::Undecodable(_)) => {
                    app::report(APP_NAME, "a malformed window event was refused");
                }
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
