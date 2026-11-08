//! The `view.app` bundle's `Run` entry point: the picture and document
//! viewer.
//!
//! # The capability story
//!
//! The manifest requests **no filesystem capability**: on its own this
//! program can open, list, and stat nothing. A document reaches it one of two
//! ways, and both are the user's own act — a read-only descriptor the file
//! manager had the kernel clone in at spawn, or a one-shot `fd_grant` the
//! session's trusted picker delegated after the user chose a file in the
//! *session's* UI under the *session's* authority.
//!
//! The document is then untrusted input, so it is never decoded here: the
//! bytes are streamed to a capability-empty worker this same binary is
//! re-entered as, which holds no filesystem reach at all and answers pixels.
//! A malformed or hostile file crashes that worker and nothing else.
//!
//! # One instance, many windows, resident
//!
//! The viewer is a *single* instance with a window per document: a second
//! document opens a second window in this process rather than a second
//! process, which is the rule for every application with an icon-bar slot.
//! It stays on that slot with no window open — launched by the user it shows
//! nothing at all until it has something to display — and only the slot's
//! *Quit* row ends it.
//!
//! Each window holds its own engine, its own sandbox in the decode worker,
//! and its own open and render state, so a malformed file refuses in its own
//! window and disturbs no other.
//!
//! # What runs where
//!
//! Everything with behaviour worth testing is in the host-tested
//! [`tairix_view`] engine: the document model, the viewport, the one layout,
//! the command set, the input routing and the renderers. This binary composes
//! them over the live syscalls, and keeps two things off the loop that owes
//! the user a frame — reading the file, and driving the sandbox — because
//! both wait on something. They run on the shared worker desk; the loop
//! submits and carries on drawing.
//!
//! A movie is played rather than drawn: the open knows it by its signature and
//! hands its descriptor to a thread of the window's own, which decodes it in a
//! sandboxed session of this binary, plays its sound through the audio
//! service, and posts each picture when it is due. The window loop only tells
//! it what the user asked and adopts what it posted.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::format;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use core::num::NonZeroUsize;

    use tairix_abi::input::KeyInput;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::window_ipc::{
        AppBarClick, AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuRow,
        AppMenuShortcut, AppMenuSymbol, MenuOutcome, PickPurpose, WindowEvent, WindowRegion,
    };
    use tairix_abi::{Errno, ProcId, DOCUMENT_ROLE_ARG, STDIN};
    use tairix_controls::damage;
    use tairix_font::BitmapFont;
    use tairix_geometry::{Point, Rect, Region, Scale};
    use tairix_icon::NoArtwork;
    use tairix_input::InputEvent;
    use tairix_player::rt::{EngineWaits, RtSpeaker, Tokens};
    use tairix_player::Failure;
    use tairix_raster::Surface;
    use tairix_rt::sync::{Mutex, WorkerWake};
    use tairix_rt::thread::Thread;

    use tairix_browse::{media_for_document, MediaType};
    use tairix_sandbox::imagerender::{
        close_view, open_view, render_page, select_page, upload_document, ImageRenderService,
        UploadFailure, ViewDocument, ViewFailure, ViewFormat, ViewPage, ViewRefusal,
    };
    use tairix_sandbox::moviedecode::{
        is_movie, MovieDecodeService, MovieInfo, RingLayout, RING_SLOTS, SIGNATURE_LEN,
    };
    use tairix_sandbox::rt::{
        draw_seed, serve_session_stdio, serve_stdio, session_worker_role, spawn_share,
        worker_ceiling, worker_role, RtLauncher, RtSessionLauncher,
    };
    use tairix_sandbox::{ParserSandbox, WorkerMemory};
    use tairix_theme::{TextRole, Theme};
    use tairix_view::movie::{self, Note, Order, Player, Release, Status, Stop};
    use tairix_view::view::{Command, Outcome, View};
    use tairix_view::{Answer, Layout, Refusal, Request, WIN_HEIGHT, WIN_WIDTH};
    use tairix_window::app::{self, Wake, WindowPane};
    use tairix_window::{
        key_input_event, pinch_input_events, pointer_input_events, pointer_point,
        scroll_input_events, DeclaredTip, Desktop, EventDrain, EventError, EventMailbox,
        EventSource, Owed, Parked, Repaint, Target, WindowClient, WindowEvents, WindowSizing,
        EVENT_MAILBOX_CAPACITY,
    };

    /// The name this program states its own refusals under.
    const APP_NAME: &str = "view";

    /// The wait-set token of the sandbox worker's answer wake.
    const WORKER_TOKEN: u64 = app::FIRST_APP_TOKEN;

    /// The wait-set token of the wake every movie's playback thread posts on.
    const PLAYBACK_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;

    /// A playback thread's own wait-set tokens: its decoder, its stream's
    /// mailbox, and its window's orders.
    const MOVIE: Tokens = Tokens {
        decoder_read: 1,
        decoder_write: 2,
        notify: 3,
    };
    const ORDERS: u64 = 4;

    // ---- the sandbox session -------------------------------------------

    /// How long the document behind the read-only descriptor `handle` is, in
    /// bytes, refused as too long past `room`, what its worker may hold. The
    /// descriptor was cloned in at spawn or redeemed from the picker's
    /// one-shot grant, and closes on every path out of the value that holds
    /// it.
    ///
    /// One `fs_stat`, which is ungated at the dispatcher precisely for this
    /// case: the descriptor's own backing decides the authority, so a holder
    /// with no filesystem capability of its own can describe the file it was
    /// handed. Measuring by reading to the end instead would read the whole
    /// document twice — once to size it and once to send it — for a figure
    /// the kernel already knows.
    fn document_length(handle: &tairix_rt::File, room: u64) -> Result<usize, Refusal> {
        let length = handle
            .regular_len()
            .map_err(Refusal::Unreadable)?
            .ok_or(Refusal::Unreadable(Errno::OutOfRange))?;
        if length > room {
            return Err(Refusal::TooLong);
        }
        usize::try_from(length).map_err(|_| Refusal::TooLong)
    }

    /// What only this binary knows about a document: the descriptor it is
    /// read from, what to call it, and the format to read it as.
    struct Source {
        handle: tairix_rt::File,
        /// The document's own file name, empty when the hand-off did not
        /// carry one.
        name: String,
    }

    /// One job the worker carries out, and the window it belongs to.
    ///
    /// The engine's [`Request`] says *what* is wanted; this adds what only
    /// this binary holds — the descriptor a document is read from, and which
    /// window asked. An open therefore cannot be asked for without a source
    /// to open, and an answer cannot land in a window that did not ask.
    struct Job {
        /// The window this is for.
        window: u64,
        /// What to carry out.
        work: Work,
    }

    /// The work half of a [`Job`].
    enum Work {
        /// Stream `source` into the window's sandbox and open it.
        Open {
            /// The open this answers, echoed so one the window has abandoned
            /// is dropped rather than adopted.
            open_id: u64,
            /// The document to read.
            source: Source,
        },
        /// The engine's render request, carried through unchanged.
        Show {
            /// The entry to hold decoded.
            page: u32,
            /// The extent the page is scaled to, in page space.
            extent: (u32, u32),
            /// The rectangle of that scaling to draw, in page space.
            window: Rect,
            /// The buffer to draw into, handed back in the answer.
            pixels: Vec<u8>,
        },
        /// End the decoders of these closed windows.
        ///
        /// A job rather than a call from the loop, because the sandboxes are
        /// the worker's and the loop may not reach them — which is the point
        /// of the worker owning its state.
        Forget(Vec<u64>),
    }

    /// What came back from one job.
    enum Reply {
        /// An answer for the window that asked.
        Window {
            /// The window that asked.
            window: u64,
            /// What came back, for that window's own engine.
            answer: Answer,
        },
        /// The decoders of closed windows have been ended.
        Forgotten,
        /// The document is a movie, for a playback of the window's own.
        Movie {
            /// The window that asked.
            window: u64,
            /// The open this answers.
            open_id: u64,
            /// The movie's file.
            source: Source,
            /// Its length in bytes.
            length: u64,
        },
    }

    /// What the worker keeps between jobs: **one sandbox per window**, and so
    /// each window's open document and the page it holds decoded.
    ///
    /// A view is a *session* — open once, then draw from the page held — so a
    /// sandbox must outlive one job; and a window's decode must be its own, or
    /// one document would land in another window and a malformed file would
    /// take every window's picture down with it. It lives here, reachable only
    /// from the thread carrying work out, so the loop can never touch it.
    #[derive(Default)]
    struct Session {
        sandboxes: alloc::collections::BTreeMap<u64, ParserSandbox<RtLauncher, tairix_rt::LogSink>>,
    }

    impl Session {
        /// The sandbox serving `window`, started on first use.
        ///
        /// A window that never opens a document never spawns a decoder, which
        /// is what keeps a viewer sitting on the icon bar costing nothing.
        fn sandbox(&mut self, window: u64) -> &mut ParserSandbox<RtLauncher, tairix_rt::LogSink> {
            self.sandboxes.entry(window).or_insert_with(|| {
                ParserSandbox::new(
                    RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Documents),
                    tairix_rt::LogSink,
                )
            })
        }

        /// Drop `window`'s sandbox, ending its decoder.
        fn forget(&mut self, window: u64) {
            self.sandboxes.remove(&window);
        }
    }

    /// The worker desk: one job in flight, latest-wins, answers arriving as a
    /// wake on the loop's own wait-set.
    type Worker = tairix_rt::work::Worker<Session, Job, Reply>;

    /// Carry out one job against the window's own sandbox.
    ///
    /// The work is taken out of the job, so the buffer a render was lent is
    /// drawn into and handed straight back in the answer rather than a
    /// window's worth of pixels being allocated per pointer sample, and a
    /// movie's descriptor moves on to its playback.
    fn serve_job(session: &mut Session, job: &mut Job) -> Reply {
        let window = job.window;
        let answer = match core::mem::replace(&mut job.work, Work::Forget(Vec::new())) {
            Work::Open { open_id, source } => match movie_length(&source) {
                Ok(Some(length)) => {
                    // A movie's decoder is its playback's own.
                    session.forget(window);
                    return Reply::Movie {
                        window,
                        open_id,
                        source,
                        length,
                    };
                }
                Ok(None) => Answer::Opened {
                    open_id,
                    opened: open_source(session.sandbox(window), &source),
                },
                Err(refusal) => Answer::Opened {
                    open_id,
                    opened: Err(refusal),
                },
            },
            Work::Show {
                page,
                extent,
                window: rect,
                mut pixels,
            } => {
                let (decoded, outcome) =
                    draw(session.sandbox(window), page, extent, rect, &mut pixels);
                Answer::Shown {
                    page,
                    extent,
                    window: rect,
                    decoded,
                    pixels,
                    outcome,
                }
            }
            Work::Forget(closed) => {
                for window in closed {
                    session.forget(window);
                }
                return Reply::Forgotten;
            }
        };
        Reply::Window { window, answer }
    }

    /// The length of the movie `source` holds, or `None` where it holds none:
    /// known by its first bytes, which is stronger than its name.
    fn movie_length(source: &Source) -> Result<Option<u64>, Refusal> {
        let mut head = [0u8; SIGNATURE_LEN];
        let read = source
            .handle
            .read_at(0, &mut head)
            .map_err(|err| Refusal::Unreadable(Errno::from_syscall(err)))?;
        if !is_movie(&head[..read]) {
            return Ok(None);
        }
        let length = source
            .handle
            .regular_len()
            .map_err(Refusal::Unreadable)?
            .ok_or(Refusal::Unreadable(Errno::OutOfRange))?;
        Ok(Some(length))
    }

    /// Stream `source` into the worker and open it, answering what the
    /// container declares.
    fn open_source(
        sandbox: &mut ParserSandbox<RtLauncher, tairix_rt::LogSink>,
        source: &Source,
    ) -> Result<(ViewDocument, String, u64), Refusal> {
        // The sandbox may hold a document and everything decoded from it;
        // dropping that first is what keeps a replacement from paying for
        // both at once. A sandbox with nothing open refuses the close, which
        // is nothing to act on.
        let _ = close_view(sandbox);
        let length = upload(sandbox, &source.handle)?;
        let format = format_for(
            &source.name,
            tairix_browse::recorded_filetype_of(&source.handle),
        );
        // The viewer holds a font client, so a drawing carrying `<text>`
        // is supplied its glyphs across the sandbox rather than refused.
        let declared = open_view(sandbox, format, &mut tairix_font::ServiceFonts::new())
            .map_err(Refusal::Failed)?;
        Ok((declared, source.name.clone(), length))
    }

    /// Measure the descriptor and stream it to the worker, answering how
    /// many bytes it holds.
    ///
    /// At no point does this process hold more than one chunk of an
    /// untrusted file, and the worker's room bounds what is *resident*
    /// rather than what is addressable, because the reads are positional.
    fn upload(
        sandbox: &mut ParserSandbox<RtLauncher, tairix_rt::LogSink>,
        handle: &tairix_rt::File,
    ) -> Result<u64, Refusal> {
        let fd = handle.fd();
        let length = document_length(handle, sandbox.document_room())?;
        upload_document(sandbox, length, |offset, into| {
            tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
        })
        .map_err(|failure| match failure {
            UploadFailure::Read(err) => Refusal::Unreadable(err),
            // The file is shorter than it measured, so the declaration the
            // worker holds can no longer be satisfied: fail closed rather
            // than pad the document with anything.
            UploadFailure::Shrank => Refusal::Unreadable(Errno::OutOfRange),
            UploadFailure::NoMemory => Refusal::Unholdable,
            UploadFailure::Document(err) => Refusal::Failed(ViewFailure::Document(err)),
        })?;
        Ok(length as u64)
    }

    /// Bring the session to `page` and draw `window` of it scaled to
    /// `extent`, into `pixels`.
    fn draw(
        sandbox: &mut ParserSandbox<RtLauncher, tairix_rt::LogSink>,
        page: u32,
        extent: (u32, u32),
        window: Rect,
        pixels: &mut Vec<u8>,
    ) -> (Option<ViewPage>, Result<(), Refusal>) {
        let decoded = match select_page(sandbox, page) {
            Ok(decoded) => decoded,
            Err(err) => return (None, Err(Refusal::Failed(err))),
        };
        let (Some(wanted), Some((x, y))) = (pixel_len(window), window.surface_origin()) else {
            return (
                Some(decoded),
                Err(Refusal::Failed(ViewFailure::Refused(
                    ViewRefusal::MalformedRequest,
                ))),
            );
        };
        if pixels.len() != wanted && !tairix_util::fallible::grow_to(pixels, wanted, 0) {
            return (Some(decoded), Err(Refusal::Unholdable));
        }
        pixels.truncate(wanted);
        let outcome = render_page(
            sandbox,
            extent,
            tairix_raster::Region {
                x,
                y,
                width: window.width,
                height: window.height,
            },
            pixels,
        )
        .map_err(Refusal::Failed);
        (Some(decoded), outcome)
    }

    /// The straight-alpha byte count `window` holds, or `None` for a window
    /// no buffer could describe.
    fn pixel_len(window: Rect) -> Option<usize> {
        usize::try_from(window.width)
            .ok()?
            .checked_mul(usize::try_from(window.height).ok()?)?
            .checked_mul(4)
    }

    // ---- a movie's playback --------------------------------------------

    /// A movie window's latest orders, until its playback thread takes them,
    /// the slots it is done with, and whether the window is leaving. One batch
    /// at most waits: the next stays in the window's own state, which folds it
    /// into the latest.
    #[derive(Default)]
    struct Orders {
        batch: Option<[Option<Order>; 2]>,
        /// Each slot of the ring the window has handed back, by its place.
        released: [Option<Release>; RING_SLOTS],
        leaving: bool,
    }

    /// What a playback thread last posted, for its window to take.
    #[derive(Default)]
    struct Board {
        status: Option<Status>,
        opened: Option<Result<MovieInfo, Stop>>,
        /// Decoders replaced since the window last looked.
        replaced: u32,
        silenced: Option<Failure>,
        picture: Option<movie::Picture>,
        /// A wake is on its way that the window has not yet read the board
        /// for, so a further post needs none.
        woken: bool,
    }

    /// What a window takes from its playback's board.
    struct Posted {
        status: Option<Status>,
        opened: Option<Result<MovieInfo, Stop>>,
        replaced: u32,
        silenced: Option<Failure>,
        picture: Option<movie::Picture>,
    }

    /// Where a movie window and its playback thread meet.
    struct Link {
        orders: Mutex<Orders>,
        ordered: WorkerWake,
        board: Mutex<Board>,
        /// The wake every playback posts on.
        posted: Arc<WorkerWake>,
    }

    /// The player a playback thread runs: its decoders are this binary in
    /// the sandbox's session role.
    type MoviePlayer<R> = Player<RtSessionLauncher, tairix_rt::LogSink, R, RtSpeaker>;

    impl Link {
        fn new(posted: Arc<WorkerWake>) -> Self {
            Self {
                orders: Mutex::new(Orders::default()),
                ordered: WorkerWake::create(),
                board: Mutex::new(Board::default()),
                posted,
            }
        }

        /// Tell the playback what `view` now calls for, once it has taken the
        /// last batch.
        fn tell(&self, view: &mut View) {
            let mut orders = self.orders.lock();
            if orders.batch.is_some() {
                return;
            }
            let batch = view.take_orders();
            if batch.iter().all(Option::is_none) {
                return;
            }
            orders.batch = Some(batch);
            drop(orders);
            self.ordered.nudge();
        }

        fn leave(&self) {
            self.orders.lock().leaving = true;
            self.ordered.nudge();
        }

        /// The batch waiting, the slots handed back, and whether the window
        /// is leaving.
        fn take_orders(&self) -> ([Option<Order>; 2], [Option<Release>; RING_SLOTS], bool) {
            let mut orders = self.orders.lock();
            let released = core::mem::take(&mut orders.released);
            (
                orders.batch.take().unwrap_or([None; 2]),
                released,
                orders.leaving,
            )
        }

        /// Hand back the slot of a picture the window has copied out, and wake
        /// the thread, which may draw into it again.
        fn give_back(&self, release: Release) {
            if let Some(entry) = self.orders.lock().released.get_mut(release.index()) {
                *entry = Some(release);
            }
            self.ordered.nudge();
        }

        /// Write the board through `write`, waking the window unless a wake is
        /// already on its way.
        fn publish(&self, write: impl FnOnce(&mut Board)) {
            let wake = {
                let mut board = self.board.lock();
                write(&mut board);
                !core::mem::replace(&mut board.woken, true)
            };
            if wake {
                self.posted.nudge();
            }
        }

        /// Post what the player has to say since it last said anything, at
        /// `now`.
        fn post<R>(&self, player: &mut MoviePlayer<R>, now: u64)
        where
            R: FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
        {
            let notes = player.take_notes();
            let picture = player.take_picture();
            if !player.take_changed() && notes.is_empty() && picture.is_none() {
                return;
            }
            let mut unseen = None;
            self.publish(|board| {
                board.status = Some(player.status());
                for note in notes {
                    match note {
                        Note::Opened(info) => board.opened = Some(Ok(info)),
                        Note::Unopened(stop) => board.opened = Some(Err(stop)),
                        Note::Replaced => board.replaced = board.replaced.saturating_add(1),
                        Note::SoundLost(failure) => board.silenced = Some(failure),
                    }
                }
                unseen = picture.and_then(|picture| board.picture.replace(picture));
            });
            // Handed back once the board is let go: the player may then read
            // the file, which the window must never wait behind.
            if let Some(unseen) = unseen {
                player.release(now, unseen.slot.release());
            }
        }

        /// Post that the movie will not open, and why.
        fn post_unopened(&self, stop: Stop) {
            self.publish(|board| board.opened = Some(Err(stop)));
        }

        /// What the thread posted since the window last looked.
        fn take(&self) -> Option<Posted> {
            let mut board = self.board.lock();
            if !core::mem::take(&mut board.woken) {
                return None;
            }
            Some(Posted {
                status: board.status.take(),
                opened: board.opened.take(),
                replaced: core::mem::take(&mut board.replaced),
                silenced: board.silenced.take(),
                picture: board.picture.take(),
            })
        }
    }

    /// A movie window's playback thread: the player, its decoder handed a
    /// ring of slots the size of `screen`, parked on what it waits for, until
    /// the window leaves or a wait fails.
    fn playback(link: &Link, file: &tairix_rt::File, length: u64, screen: (u32, u32)) {
        let read =
            |offset: u64, into: &mut [u8]| file.read_at(offset, into).map_err(Errno::from_syscall);
        let ring = RingLayout::new(screen.0, screen.1)
            .and_then(|layout| Some((layout, NonZeroUsize::new(layout.ring_bytes())?)));
        let Some((layout, ring_bytes)) = ring else {
            return link.post_unopened(Stop::NoDecoder);
        };
        let memory = WorkerMemory::Documents;
        let made = Player::new(
            RtSessionLauncher::own_binary(memory).sharing(ring_bytes),
            tairix_rt::LogSink,
            (read, length),
            (worker_ceiling(memory), layout),
            RtSpeaker::new(),
            draw_seed,
        );
        let mut player: MoviePlayer<_> = match made {
            Ok(player) => player,
            Err(stop) => return link.post_unopened(stop),
        };
        let unwatched = |what, errno| Stop::Abandoned(Failure::Wait { what, errno });
        let set = match tairix_rt::WaitSet::create() {
            Ok(set) => set,
            Err(errno) => return link.post_unopened(unwatched("make a wait-set", errno)),
        };
        if let Err(errno) = app::watch_wake(set.handle(), &link.ordered, ORDERS) {
            return link.post_unopened(unwatched("watch the window's orders", errno));
        }
        let mut waits = EngineWaits::new(set.handle(), MOVIE);
        player.begin(tairix_rt::clock_get());
        loop {
            let members = player.decoder_members();
            if let Err(failure) = waits.watch(members, player.speaker().notify_port()) {
                player.abandon(failure);
                return link.post(&mut player, tairix_rt::clock_get());
            }
            link.post(&mut player, tairix_rt::clock_get());
            let woken = match set.wait_until(player.deadline().unwrap_or(u64::MAX)) {
                Ok(woken) => woken,
                Err(errno) => {
                    let what = "wait for playback";
                    player.abandon(Failure::Wait { what, errno });
                    return link.post(&mut player, tairix_rt::clock_get());
                }
            };
            let now = tairix_rt::clock_get();
            match woken {
                None => player.on_timer(now),
                Some(token) if token == MOVIE.decoder_read => player.on_decoder(now, true, false),
                Some(token) if token == MOVIE.decoder_write => player.on_decoder(now, false, true),
                Some(token) if token == MOVIE.notify => player.on_notify(now),
                Some(ORDERS) => {
                    link.ordered.drain();
                    let (batch, released, leaving) = link.take_orders();
                    if leaving {
                        player.close();
                        return;
                    }
                    for release in released.into_iter().flatten() {
                        player.release(now, release);
                    }
                    for order in batch.into_iter().flatten() {
                        player.order(now, order);
                    }
                }
                Some(_) => {}
            }
        }
    }

    /// A movie window's playback: the link to the thread playing it, and what
    /// the window knows of the open it answers.
    struct Playback {
        link: Arc<Link>,
        open_id: u64,
        name: String,
        bytes: u64,
    }

    // ---- the document a launch was given -------------------------------

    /// The document this program was handed on [`STDIN`] by its launcher: a
    /// read-only descriptor the kernel cloned in at spawn, so it is read with
    /// no filesystem capability of this program's own.
    fn inherited() -> Source {
        let name = tairix_rt::arg(2)
            .and_then(|raw| core::str::from_utf8(raw).ok())
            .map(leaf_of)
            .unwrap_or_default();
        Source {
            handle: tairix_rt::File::adopt(STDIN),
            name,
        }
    }

    /// A document delegated to this process: a one-shot `fd_grant` redeemed
    /// into a read-only descriptor whose reads the kernel authorises under
    /// the identity of whoever opened it.
    ///
    /// `name` is what to call it, and is empty where the hand-off could not
    /// name it. The viewer then states what it knows and invents nothing. A
    /// delegation that cannot be taken over hands the name back, to say which
    /// document it was.
    fn delegated(handle: u64, name: String) -> Result<Source, String> {
        match tairix_rt::File::from_delegation(handle) {
            Ok(handle) => Ok(Source { handle, name }),
            Err(_) => Err(name),
        }
    }

    /// The headline of an alert saying the document `name` could not be
    /// opened.
    fn not_opened_headline(name: &str) -> String {
        if name.is_empty() {
            String::from("The document could not be opened")
        } else {
            tairix_util::utf8::refusal_headline(name, "opened")
        }
    }

    /// The last component of a path.
    fn leaf_of(path: &str) -> String {
        String::from(path.rsplit('/').next().unwrap_or(path))
    }

    /// The format to read a document called `name`, whose volume records
    /// `recorded` as its filetype, as in place of its own signature, or `None`
    /// to let the decoder sniff it.
    ///
    /// Only the one format that carries no signature is named: a RISC OS
    /// sprite area's first word is its sprite count, so a typed one whose
    /// header does not state its own length still opens as one. Every other
    /// format is recognised from its bytes, which is stronger than trusting a
    /// file name — a document named `.png` that is a JPEG opens as the JPEG it
    /// is.
    fn format_for(name: &str, recorded: Option<tairix_abi::fs::Filetype>) -> Option<ViewFormat> {
        (media_for_document(name, recorded) == Some(MediaType::ImageSprite))
            .then_some(ViewFormat::Sprite)
    }

    // ---- the app-declared menu -----------------------------------------

    /// The commands the app's own menu offers, in the order they are shown.
    ///
    /// One ordered list, so a row's label, its accelerator caption, its
    /// function and the command it runs are the same position — a menu whose
    /// rows and actions could be listed separately is one that can be wired up
    /// wrong.
    const MENU: [(&str, &str, AppMenuSymbol, Command); 9] = [
        ("Open…", "O", AppMenuSymbol::Open, Command::OpenDocument),
        ("Zoom in", "+", AppMenuSymbol::ZoomIn, Command::ZoomIn),
        ("Zoom out", "-", AppMenuSymbol::ZoomOut, Command::ZoomOut),
        (
            "Fit in window",
            "0",
            AppMenuSymbol::ZoomFit,
            Command::FitWindow,
        ),
        ("Fit width", "2", AppMenuSymbol::FitWidth, Command::FitWidth),
        (
            "Actual size",
            "1",
            AppMenuSymbol::ZoomActual,
            Command::ActualSize,
        ),
        (
            "Rotate right",
            "]",
            AppMenuSymbol::RotateRight,
            Command::RotateRight,
        ),
        (
            "Mirror",
            "M",
            AppMenuSymbol::FlipHorizontal,
            Command::Mirror,
        ),
        ("Information", "I", AppMenuSymbol::Info, Command::ToggleInfo),
    ];

    /// Build the app's menu, marking the rows whose state is a toggle.
    ///
    /// A refused row is dropped rather than the whole menu being abandoned:
    /// the menu is incidental to the viewer's purpose, so the user gets the
    /// rows that fit and is told how many did not.
    fn build_menu(view: &View) -> (AppMenu, usize) {
        let mut menu = AppMenu::EMPTY;
        let mut skipped = 0;
        for (index, &(label, shortcut, symbol, command)) in MENU.iter().enumerate() {
            let (Some(id), Ok(label)) = (AppMenuItemId::for_index(index), AppMenuLabel::new(label))
            else {
                skipped += 1;
                continue;
            };
            let mut item = AppMenuItem::new(id, label, symbol);
            if let Ok(caption) = AppMenuShortcut::new(shortcut) {
                item = item.with_shortcut(caption);
            }
            if matches!(command, Command::ToggleInfo) && view.info_open() {
                item = item.with_mark(AppMenuMark::Check);
            }
            if menu.push(AppMenuRow::Item(item)).is_err() {
                skipped += 1;
            }
        }
        (menu, skipped)
    }

    /// The command a chosen menu row names.
    fn menu_command(id: AppMenuItemId) -> Option<Command> {
        MENU.get(id.index()).map(|&(_, _, _, command)| command)
    }

    // ---- one window per document ---------------------------------------

    /// One of the viewer's windows: its channel-side state, its retained
    /// picture, and the engine that draws into it.
    ///
    /// Every window is independent of its siblings — its own document, its
    /// own decode sandbox, its own menu and title — so a malformed file
    /// refuses in the window that opened it and disturbs no other.
    struct Window {
        /// Its channel-side state: the id its events arrive under, its shared
        /// frame region, and the geometry both are shaped as.
        pane: WindowPane,
        /// The surface every frame of it is drawn into, held for the window's
        /// life so a clipped repaint leaves the pixels outside the clip alone.
        surface: Surface,
        /// The viewer looking at this window's document.
        view: View,
        /// The document this window is waiting for, once the embedder holds
        /// one: submitted when the engine asks for its open.
        pending: Option<Source>,
        /// The name of the document it last asked to open, so a refusal of
        /// it can say which.
        opening: String,
        /// The menu open over the window, so an outcome is matched to the
        /// gesture that asked for it rather than to whichever was last.
        menu: Option<u64>,
        tip: DeclaredTip,
        /// The title the session was last told, so it is only set again when
        /// the document changes.
        title: String,
        /// Whether any frame of this window has reached the session.
        presented: bool,
        /// What the window owes the screen since it last presented, folded
        /// across every event of the turn so a burst costs one frame.
        owed: Owed,
        /// The movie's playback, while the window shows one.
        playback: Option<Playback>,
    }

    impl Window {
        /// Tell the window's playback what its state now calls for, and draw
        /// its movie's picture again where the canvas has moved.
        fn tell(&mut self, theme: &Theme, scale: Scale) {
            if self.playback.is_none() {
                return;
            }
            let layout = self.layout(theme, scale);
            if let Some(playback) = &self.playback {
                playback.link.tell(&mut self.view);
            }
            let mut owed = damage::sink();
            if self.view.redraw_movie(&layout, &mut owed) {
                self.owe(Repaint::Reported, &owed);
            }
        }

        /// Resolve the layout for the window's current extent.
        fn layout(&mut self, theme: &Theme, scale: Scale) -> Layout {
            let mode = *self.pane.mode();
            self.view.layout(
                mode.width_px,
                mode.height_px,
                theme,
                scale,
                face(theme, scale),
            )
        }

        /// Whether presenting now would put an empty window on screen.
        ///
        /// The session shows a served window on its first present, so
        /// withholding that present is withholding the window. Every window
        /// waiting for its document must do it: one waiting on a pick would
        /// otherwise sit blank behind the chooser for as long as the choice
        /// takes, and one handed a document would appear at the default
        /// extent and then be resized to the picture, which reads as a
        /// flash. Once anything of the window has been on screen it is never
        /// withheld again, whatever the viewer goes on to show.
        fn withholding(&self) -> bool {
            !self.presented && self.view.nothing_to_show()
        }

        /// Fold `repaint`, and what it `reported`, into what the window owes.
        fn owe(&mut self, repaint: Repaint, reported: &Region) {
            let mode = *self.pane.mode();
            self.owed.owe(&mode, repaint, reported);
        }

        /// Paint what the window owes and present it, once a turn however many
        /// events folded into it.
        ///
        /// The whole viewer is re-derived under each rectangle's clip, so a
        /// partial repaint lands the pixels a whole one would have — there is
        /// no second "paint just this part" recipe.
        fn present(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            theme: &Theme,
            scale: Scale,
        ) -> Result<(), Errno> {
            if self.withholding() {
                return Ok(());
            }
            let mode = *self.pane.mode();
            // Nothing of what was on screen survives where the session gave
            // its copy of the region back, or where the window has never been
            // on screen at all.
            if self.pane.content_released() || !self.presented {
                self.owed.owe_whole();
            }
            let Some(parts) = self.owed.take(&mode) else {
                return Ok(());
            };
            let layout = self.layout(theme, scale);
            let view = &self.view;
            for rect in parts.rects() {
                self.surface
                    .with_clip(rect.x, rect.y, rect.width_px, rect.height_px, |clipped| {
                        tairix_view::paint::render_into(
                            clipped,
                            view,
                            &layout,
                            theme,
                            scale,
                            face(theme, scale),
                            &mut NoArtwork,
                        );
                    });
            }
            let landed = self.pane.present_list(client, &self.surface, &parts);
            if landed.is_ok() {
                self.presented = true;
            }
            landed
        }

        /// Close this window, its menu and its playback going with it.
        fn close(self, client: &mut WindowClient<app::RtWindowTransport>) {
            if let Some(playback) = &self.playback {
                playback.link.leave();
            }
            let _ = self.pane.close(client);
        }
    }

    /// Open a window for `source`, or for the user to pick into when there is
    /// none, answering it and the session identity it is served by.
    ///
    /// A refusal is put up as an alert and answers `None`: the viewer carries
    /// on with the windows it has, which for a resident application is the
    /// honest outcome — it is still on the icon bar and still clickable.
    fn open_window(
        client: &mut WindowClient<app::RtWindowTransport>,
        event_endpoint: u64,
        server: ProcId,
        desktop: &Desktop,
        theme: &Theme,
        scale: Scale,
        source: Option<Source>,
    ) -> Option<Window> {
        let (width, height) = desktop.window_size(WIN_WIDTH, WIN_HEIGHT);
        let mode = app::mode_for(width, height);
        let refused = |client: &mut WindowClient<app::RtWindowTransport>, why: &str| {
            let _ = app::refuse(client, (APP_NAME, event_endpoint), None, NO_WINDOW, why);
        };
        let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
            refused(client, "there is not enough memory to draw another window");
            return None;
        };
        // Declared in *physical* pixels, derived from the theme's metrics at
        // the desktop's own density: what the toolbar needs to keep a tool and
        // both its overflow affordances reachable, and the chrome plus a
        // strip of canvas down.
        let least = tairix_view::min_client_size(theme, scale, face(theme, scale));
        let sizing = WindowSizing::Resizable {
            min_width_px: least.0,
            min_height_px: least.1,
            // No ceiling: a picture or a page shows more of itself, or shows
            // itself larger, at every size the window is given.
            max_width_px: 0,
            max_height_px: 0,
        };
        let (pane, replied) =
            match WindowPane::open(client, event_endpoint, &mode, APP_TITLE, sizing) {
                Ok(opened) => opened,
                Err(err) => {
                    refused(client, &format!("the desktop would not open it ({err})"));
                    return None;
                }
            };
        // A reply from any other sender is something else answering for the
        // window endpoint: the window it named is closed rather than drawn
        // into, exactly as a popup's reply is checked.
        if replied != server {
            let _ = pane.close(client);
            refused(client, "a window reply came from another sender");
            return None;
        }
        Some(Window {
            pane,
            surface,
            // A window handed a document asks for its open from the start; one
            // the user will pick into asks for nothing until they have chosen.
            view: View::new(source.is_some()),
            pending: source,
            opening: String::new(),
            menu: None,
            tip: DeclaredTip::new(),
            title: String::from(APP_TITLE),
            presented: false,
            owed: Owed::new(),
            playback: None,
        })
    }

    /// The face the viewer sets its own text in.
    fn face(theme: &Theme, scale: Scale) -> BitmapFont {
        BitmapFont::for_role(theme.fonts(), TextRole::Body, scale)
    }

    // ---- the event source ----------------------------------------------

    /// The app's park: its event mailbox, the memory-pressure band, the
    /// worker's answer wake, the desktop state, and the animation deadline.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        worker: &'a Worker,
        /// The wake the movies' playback threads post on.
        playback: &'a WorkerWake,
        /// Set when the session published a new desktop state, for the loop
        /// to adopt.
        desktop_moved: &'a Cell<bool>,
        /// When the next animation frame is due, or `None` when nothing is
        /// timed — in which case the park has no deadline at all and the CPU
        /// is given up entirely.
        ///
        /// Shared with the loop through a cell because the loop owns the
        /// deadline and the source owns the park: one writes it just before
        /// the other reads it, on the one thread both run on.
        deadline_ns: &'a Cell<Option<u64>>,
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
                Wake::App(WORKER_TOKEN) => {
                    // The readiness is a level peek, so leaving it undrained
                    // would report ready for ever and turn the park into a
                    // spin.
                    self.worker.wake().drain();
                    Ok(Parked::Interrupted)
                }
                Wake::App(PLAYBACK_TOKEN) => {
                    self.playback.drain();
                    Ok(Parked::Interrupted)
                }
                Wake::PressureChanged => {
                    tairix_font::trim_glyph_cache();
                    Ok(Parked::Served)
                }
                Wake::DesktopChanged => {
                    self.desktop_moved.set(true);
                    Ok(Parked::Interrupted)
                }
                _ => Ok(Parked::Served),
            }
        }
    }

    // ---- the run -------------------------------------------------------

    /// The title a window opens under, replaced by the document's own name
    /// once one is open and named.
    const APP_TITLE: &str = "View";

    /// The headline of an alert saying a window could not be opened.
    const NO_WINDOW: &str = "No window could be opened";

    /// Declare, or withdraw, the tooltip for whatever the pointer is over.
    fn set_tip(
        window: &mut Window,
        client: &mut WindowClient<app::RtWindowTransport>,
        layout: &Layout,
        (scale, theme): (Scale, &Theme),
    ) {
        let wanted = window.view.tool_tip(layout, scale, theme);
        window.tip.declare(client, window.pane.id(), wanted);
    }

    /// Tell the session what the window is showing, once, when it changes.
    fn retitle(window: &mut Window, client: &mut WindowClient<app::RtWindowTransport>) {
        let wanted = window
            .view
            .name()
            .filter(|name| !name.is_empty())
            .map_or_else(|| String::from(APP_TITLE), String::from);
        if wanted == window.title {
            return;
        }
        if client.set_title(window.pane.id(), &wanted).is_ok() {
            window.title = wanted;
        }
    }

    /// Ask the picker for a document for `window`, answering whether the
    /// desktop offered a chooser. A refusal is put up as an alert, held by the
    /// window when it shows a document; one waiting on the chooser alone has
    /// nothing to show, so its caller takes it down.
    fn ask_for_document(
        window: &Window,
        client: &mut WindowClient<app::RtWindowTransport>,
        event_endpoint: u64,
    ) -> bool {
        let Err(err) = client.pick_file(window.pane.id(), PickPurpose::Open) else {
            return true;
        };
        let owner = window.view.name().is_some().then(|| window.pane.id());
        let _ = app::refuse(
            client,
            (APP_NAME, event_endpoint),
            owner,
            "No file chooser could be opened",
            &format!("the desktop would not offer one ({err}); open a document from the Files app"),
        );
        false
    }

    /// The document window `index` asked for did not open, for `refusal`: an
    /// error alert says why, and the window, which has nothing to show, comes
    /// down.
    fn not_opened(app: &mut App<'_>, index: usize, refusal: &Refusal) {
        let window = app.windows.remove(index);
        let headline = not_opened_headline(&window.opening);
        let detail = format!("{refusal}");
        let _ = app::refuse(
            app.client,
            (APP_NAME, app.event_endpoint),
            None,
            &headline,
            &detail,
        );
        app.desk.closed.push(window.pane.id());
        window.close(app.client);
    }

    /// Adopt the worker's `answer` for window `id`, folding what it changed
    /// into what the window owes; one for a window this viewer has closed is
    /// dropped, the window it described being gone.
    fn adopt_answer(
        app: &mut App<'_>,
        (id, answer): (u64, Answer),
        (theme, scale): (&Theme, Scale),
    ) {
        let Some(index) = index_of(app.windows, id) else {
            return;
        };
        // A document that has just opened is the one moment the window sizes
        // itself to the picture; a render, a zoom, or a resize never does, so
        // it cannot fight the user's own drag.
        let just_opened = matches!(
            &answer,
            Answer::Opened { opened: Ok(_), .. } | Answer::Movie { opened: Ok(_), .. }
        );
        let refused_open = matches!(
            &answer,
            Answer::Opened { opened: Err(_), .. } | Answer::Movie { opened: Err(_), .. }
        );
        let pictured = matches!(&answer, Answer::Opened { opened: Ok(_), .. });
        let mut reported = damage::sink();
        let layout = app.windows[index].layout(theme, scale);
        let changed = app.windows[index]
            .view
            .deliver(answer, &layout, &mut reported)
            .changed;
        // A picture opened in a movie's window ends the movie's playback.
        if pictured && changed {
            if let Some(playback) = app.windows[index].playback.take() {
                playback.link.leave();
            }
        }
        // An open that was waited for and refused leaves the window nothing
        // to show; one abandoned since changes nothing.
        let refusal = app.windows[index].view.refusal().copied();
        if let Some(refusal) = refusal.filter(|_| refused_open && changed) {
            not_opened(app, index, &refusal);
            return;
        }
        let window = &mut app.windows[index];
        retitle(window, app.client);
        let resized = just_opened && hug_picture(window, app.client, app.desktop, theme, scale);
        let repaint = if resized {
            Repaint::Whole
        } else {
            Repaint::reported_if(changed)
        };
        window.owe(repaint, &reported);
    }

    /// Play the movie `source` holds in window `id` on a thread of its own; one
    /// the window has stopped waiting for is let go unplayed.
    fn start_playback(
        app: &mut App<'_>,
        id: u64,
        (open_id, source, length): (u64, Source, u64),
        (posted, dressing): (&Arc<WorkerWake>, (&Theme, Scale)),
    ) {
        let Some(index) = index_of(app.windows, id) else {
            return;
        };
        let window = &mut app.windows[index];
        if !window.view.awaiting(open_id) {
            return;
        }
        if let Some(old) = window.playback.take() {
            old.link.leave();
        }
        let link = Arc::new(Link::new(Arc::clone(posted)));
        let thread_link = Arc::clone(&link);
        let file = source.handle;
        let shown = app.desktop.screen();
        let screen = (shown.width, shown.height);
        match Thread::spawn(move || playback(&thread_link, &file, length, screen)) {
            Ok(_) => {
                window.playback = Some(Playback {
                    link,
                    open_id,
                    name: source.name,
                    bytes: length,
                });
            }
            Err(errno) => {
                let opened = Err(Refusal::NoPlayback(errno));
                adopt_answer(app, (id, Answer::Movie { open_id, opened }), dressing);
            }
        }
    }

    /// Adopt what each movie window's playback has posted since the loop last
    /// looked.
    fn collect_playbacks(app: &mut App<'_>, dressing: (&Theme, Scale)) {
        // Backwards, so a window a refused open closes moves none still to come.
        for index in (0..app.windows.len()).rev() {
            let posted = app.windows[index]
                .playback
                .as_ref()
                .and_then(|playback| playback.link.take());
            if let Some(posted) = posted {
                adopt_posted(app, index, posted, dressing);
            }
        }
    }

    /// Adopt what window `index`'s playback posted: the open's outcome first,
    /// then what became of the decoder and the sound, where the movie is, and
    /// the picture due.
    fn adopt_posted(
        app: &mut App<'_>,
        index: usize,
        posted: Posted,
        (theme, scale): (&Theme, Scale),
    ) {
        let id = app.windows[index].pane.id();
        if let Some(opened) = posted.opened {
            let Some(playback) = app.windows[index].playback.as_ref() else {
                return;
            };
            let opened = opened
                .map(|info| (info, playback.name.clone(), playback.bytes))
                .map_err(Refusal::Movie);
            let answer = Answer::Movie {
                open_id: playback.open_id,
                opened,
            };
            adopt_answer(app, (id, answer), (theme, scale));
        }
        // A refused open has closed the window.
        let Some(index) = index_of(app.windows, id) else {
            return;
        };
        let window = &mut app.windows[index];
        let layout = window.layout(theme, scale);
        let mut reported = damage::sink();
        let mut changed = false;
        for _ in 0..posted.replaced {
            changed |= window
                .view
                .movie_note(Note::Replaced, &layout, &mut reported);
        }
        if let Some(failure) = posted.silenced {
            let lost = Note::SoundLost(failure);
            changed |= window.view.movie_note(lost, &layout, &mut reported);
        }
        if let Some(status) = posted.status {
            changed |= window.view.movie_status(status, &layout, &mut reported);
        }
        if let Some(picture) = posted.picture {
            changed |= window.view.show_frame(&picture, &layout, &mut reported);
            if let Some(playback) = &window.playback {
                playback.link.give_back(picture.slot.release());
            }
        }
        window.owe(Repaint::reported_if(changed), &reported);
    }

    /// What a delivery carrying no event calls for: the channel dying ends the
    /// viewer with the status this answers, while a malformed frame — refused
    /// rather than guessed at — and a request the session refused are stated
    /// and carried on from.
    fn refused_delivery(err: EventError) -> Option<i32> {
        match err {
            EventError::Mailbox(_) => {
                let code = app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "the event channel died");
                Some(code)
            }
            EventError::Undecodable(_) => {
                app::report(APP_NAME, "a malformed window event was refused");
                None
            }
            EventError::Refused(refused) => {
                app::report(APP_NAME, refused);
                None
            }
        }
    }

    /// The viewer's whole life.
    #[allow(
        clippy::too_many_lines,
        reason = "one linear bring-up plus one event loop; splitting the loop would separate the park from the drain it must follow"
    )]
    fn main() -> i32 {
        // The sandbox-worker role, before anything else: a document is
        // untrusted input, so it is decoded by a capability-empty child this
        // same binary is re-entered as, serving over its wired standard
        // streams and nothing else. It never becomes the viewer.
        if worker_role() {
            return serve_stdio(&mut ImageRenderService::default()).exit_code();
        }
        if session_worker_role() {
            // A worker handed no ring refuses every open, and the viewer says
            // the movie could not be opened.
            let ring = spawn_share().ok();
            return serve_session_stdio(&mut MovieDecodeService::new(ring)).exit_code();
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }

        // A user-facing loop, so declare the frame it owes. A debug image
        // reports any span that overruns; a shippable one arms nothing.
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);

        // How the viewer starts depends on how it was launched: handed a
        // document at spawn it opens a window on it, and launched by the user
        // it opens nothing at all and sits on the icon bar.
        let inherited_source = tairix_rt::arg(1)
            .is_some_and(|arg| arg == DOCUMENT_ROLE_ARG)
            .then(inherited);

        let mut client = WindowClient::new(app::RtWindowTransport);
        let (mut desktop, mut themes) = match app::bring_up_desktop(&mut client) {
            Ok(pair) => pair,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let theme = themes.active();
        let scale = desktop.scale();
        // The serving session's own identity, learned by the desktop query.
        // An application that may own no window needs it from there: it is
        // what authenticates the icon-bar events it still receives, and
        // without it there is nothing to accept them against (fail closed).
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
        let event_endpoint = binding.endpoint();
        let set = binding.set();

        // Reading the file and driving the sandbox both wait on something, so
        // both go on the worker; the loop submits and carries on drawing.
        let worker = Arc::new(Worker::new(
            serve_job,
            Session::default(),
            tairix_rt::sync::WorkerWake::create(),
        ));
        if let Err(reason) = Worker::start(&worker) {
            app::report(
                APP_NAME,
                format_args!("no decode worker ({reason:?}); documents are read on the event loop"),
            );
        }
        let _worker_guard = tairix_rt::work::WorkerGuard::new(&worker);
        if let Err(err) = app::watch_wake(set, worker.wake(), WORKER_TOKEN) {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                format_args!("decode answer wake refused ({err})"),
            );
        }
        // Every movie's playback thread posts on this one wake, and the loop
        // reads each movie window's board when it fires.
        let posted = Arc::new(WorkerWake::create());
        if let Err(err) = app::watch_wake(set, &posted, PLAYBACK_TOKEN) {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                format_args!("playback wake refused ({err})"),
            );
        }

        // A refused declaration is an answer, not a death: the viewer's
        // windows are still reachable, though nothing reaches it with none open.
        let declared = tairix_window::info_and_quit(event_endpoint, AppBarClick::RaiseOrOpen);
        if let Err(refused) = tairix_window::declare_app_bar(&mut client, declared) {
            app::report(APP_NAME, format_args!("{refused}"));
        }

        // The windows open, in the order they were opened. A viewer launched
        // by the user starts with none: it is resident on the icon bar, and
        // shows nothing until it has something to show.
        let mut windows: Vec<Window> = Vec::new();
        if inherited_source.is_some() {
            if let Some(opened) = open_window(
                &mut client,
                event_endpoint,
                server,
                &desktop,
                theme,
                scale,
                inherited_source,
            ) {
                windows.push(opened);
            }
        }
        // The first frame of each window is the whole of it: nothing is on
        // screen yet. A window with nothing to show withholds it, and the
        // window with it.
        for window in &mut windows {
            if window.present(&mut client, theme, scale).is_err() {
                return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "first present refused");
            }
        }

        let mut desk = Desk::default();
        let deadline = Cell::new(None);
        let desktop_moved = Cell::new(false);
        let mut events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(event_endpoint, server),
            set,
            worker: &worker,
            playback: &posted,
            desktop_moved: &desktop_moved,
            deadline_ns: &deadline,
        });

        // The event the last park ended on, served first in the next drain.
        let mut woken = None;
        loop {
            // The theme and density every window draws from moved: each owes
            // its whole frame in the new state. A refused state is stated and
            // the last good one stands.
            if desktop_moved.replace(false) {
                match app::adopt_desktop(&mut desktop, &mut themes) {
                    Ok(true) => {
                        for window in &mut windows {
                            window.owed.owe_whole();
                        }
                    }
                    Ok(false) => {}
                    Err(err) => {
                        app::report(APP_NAME, format_args!("desktop change refused: {err}"));
                    }
                }
            }
            let (theme, scale) = (themes.active(), desktop.scale());

            // An answer the worker landed first, so a picture appears the
            // moment it is ready rather than at whatever later input arrives.
            if let Some(reply) = worker.collect() {
                desk.outstanding = false;
                let mut app = App {
                    windows: &mut windows,
                    client: &mut client,
                    desk: &mut desk,
                    event_endpoint,
                    server,
                    desktop: &desktop,
                };
                match reply {
                    Reply::Window { window, answer } => {
                        adopt_answer(&mut app, (window, answer), (theme, scale));
                    }
                    Reply::Movie {
                        window,
                        open_id,
                        source,
                        length,
                    } => {
                        let movie = (open_id, source, length);
                        start_playback(&mut app, window, movie, (&posted, (theme, scale)));
                    }
                    Reply::Forgotten => {}
                }
            }
            // What the movies' playbacks posted, for the same reason.
            collect_playbacks(
                &mut App {
                    windows: &mut windows,
                    client: &mut client,
                    desk: &mut desk,
                    event_endpoint,
                    server,
                    desktop: &desktop,
                },
                (theme, scale),
            );

            // Then every event queued, the one the last park ended on first,
            // each only recording what it changed: a burst of pointer motion
            // costs one frame rather than one each.
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
                    Err(err) => {
                        if let Some(code) = refused_delivery(err) {
                            return code;
                        }
                        continue;
                    }
                };
                let routed = route(
                    &mut App {
                        windows: &mut windows,
                        client: &mut client,
                        desk: &mut desk,
                        event_endpoint,
                        server,
                        desktop: &desktop,
                    },
                    &event,
                    theme,
                    scale,
                );
                if matches!(routed, Routed::Quit) {
                    return 0;
                }
            }

            // Each window tells its playback what the turn asked of it, and
            // presents what the turn owes it, once.
            for window in &mut windows {
                window.tell(theme, scale);
                if window.present(&mut client, theme, scale).is_err() {
                    return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "present refused");
                }
            }
            // A drain cut short goes on without parking.
            if !drained {
                continue;
            }
            // Nothing queued: submit whatever any window now calls for. The
            // answer is collected on the next turn either way — a deferred job
            // wakes the park, and one carried out inline for want of a worker
            // thread is already on the desk — so what `submit` reports about
            // where it ran changes nothing here.
            if submit_next(&mut windows, &worker, &mut desk, theme, scale) {
                continue;
            }

            // Nothing to submit: arm the animation deadline — one shot, to the
            // next frame any window actually asks for — and park. With nothing
            // animating, nothing is armed.
            let now = tairix_rt::clock_get();
            deadline.set(next_deadline(&mut windows, now, theme, scale));
            let parked = events.wait(&mut client);
            // A frame may be due whether the park ended on the deadline or on
            // an event; what it moved is presented with the next turn.
            let now = tairix_rt::clock_get();
            for window in &mut windows {
                // Each window's own sink: one window's rectangles are no
                // description of another's surface.
                let mut owed = damage::sink();
                let layout = window.layout(theme, scale);
                if window.view.tick(now, &layout, scale, theme, &mut owed) {
                    window.owe(Repaint::Reported, &owed);
                }
            }
            match parked {
                // Served first in the next turn's drain: a park consumes the
                // event it woke on, so nothing else would ever see it again.
                Ok(Some(event)) => woken = Some(event),
                // A park the worker's answer interrupted carries no event; the
                // collect at the top of the next turn adopts what it woke for.
                Ok(None) => {}
                Err(err) => {
                    if let Some(code) = refused_delivery(err) {
                        return code;
                    }
                }
            }
        }
    }

    /// Everything routing one event may reach.
    ///
    /// Bundled because a viewer with several windows threads all of it to
    /// every arm; each field is the one copy the process holds.
    struct App<'a> {
        windows: &'a mut Vec<Window>,
        client: &'a mut WindowClient<app::RtWindowTransport>,
        desk: &'a mut Desk,
        event_endpoint: u64,
        server: ProcId,
        desktop: &'a Desktop,
    }

    /// What the loop knows about the decode desk.
    ///
    /// The desk takes one job at a time latest-wins, so submitting while one
    /// is in flight would displace a job whose answer a window is waiting
    /// for and leave that window's render outstanding for ever. Exactly one
    /// is therefore submitted at a time, and the window it is asked *for*
    /// rotates, so an animating window cannot starve another's open.
    #[derive(Default)]
    struct Desk {
        /// Whether a job is in flight.
        outstanding: bool,
        /// The window after the one last served — where the next scan starts.
        next: usize,
        /// Closed windows whose decoders have still to be ended.
        closed: Vec<u64>,
    }

    /// The index of the window `id` names, or `None` for one already closed.
    fn index_of(windows: &[Window], id: u64) -> Option<usize> {
        windows.iter().position(|open| open.pane.id() == id)
    }

    /// Submit the first request any window is asking for, answering whether
    /// one was submitted.
    ///
    /// The desk takes one job at a time latest-wins, so exactly one is
    /// submitted per turn and the next turn collects its answer and asks
    /// again — which is what keeps several windows decoding in turn rather
    /// than one starving the rest.
    fn submit_next(
        windows: &mut [Window],
        worker: &Worker,
        desk: &mut Desk,
        theme: &Theme,
        scale: Scale,
    ) -> bool {
        if desk.outstanding {
            return false;
        }
        // A closed window's decoder first: it is a whole process holding
        // memory for a window nobody can see any more.
        if !desk.closed.is_empty() {
            worker.submit(Job {
                window: 0,
                work: Work::Forget(core::mem::take(&mut desk.closed)),
            });
            desk.outstanding = true;
            return true;
        }
        let count = windows.len();
        for turn in 0..count {
            let index = (desk.next + turn) % count;
            let id = windows[index].pane.id();
            // The layout the request is derived from, at this window's own
            // current extent.
            let _ = windows[index].layout(theme, scale);
            let work = match windows[index].view.next_request() {
                // The open stays outstanding until it is answered, so this
                // arm repeats while the worker is reading. Only a source is
                // worth submitting; without one the picker's conclusion is
                // what brings it, and that arrives as an event.
                Some(Request::Open { open_id }) => windows[index].pending.take().map(|source| {
                    windows[index].opening.clone_from(&source.name);
                    Work::Open { open_id, source }
                }),
                Some(Request::Show {
                    page,
                    extent,
                    window,
                    pixels,
                }) => Some(Work::Show {
                    page,
                    extent,
                    window,
                    pixels,
                }),
                None => None,
            };
            if let Some(work) = work {
                worker.submit(Job { window: id, work });
                desk.outstanding = true;
                desk.next = (index + 1) % count;
                return true;
            }
        }
        false
    }

    /// The nearest animation frame or held control's repeat any window owes,
    /// or `None` when none is timed — in which case the park arms no timer at
    /// all.
    fn next_deadline(windows: &mut [Window], now: u64, theme: &Theme, scale: Scale) -> Option<u64> {
        let mut nearest = None;
        for window in windows.iter_mut() {
            let layout = window.layout(theme, scale);
            window.view.arm_deadline(now, &layout, scale, theme);
            if let Some(due) = window.view.deadline_ns() {
                nearest = Some(nearest.map_or(due, |held: u64| held.min(due)));
            }
        }
        nearest
    }

    /// What routing one event decided for the process.
    enum Routed {
        /// Carry on serving.
        Served,
        /// *Quit* was chosen: close every window and end.
        Quit,
    }

    /// What routing one event decided for the window it named.
    enum Acted {
        /// Something drawn changed, at this scope.
        Changed(Repaint),
        /// This window closed. The viewer keeps its slot and carries on.
        Close,
        /// Nothing to do.
        Idle,
    }

    /// Route one delivered window event, folding what it changed into what
    /// the window it names owes.
    ///
    /// An event naming a window this viewer no longer has is dropped: the
    /// window it addressed is gone, and there is nothing left to apply it to.
    fn route(app: &mut App<'_>, event: &WindowEvent, theme: &Theme, scale: Scale) -> Routed {
        // The application-scoped events first: they name no window, and two of
        // them are the whole of a resident viewer's own lifecycle.
        match event {
            // The slot's primary click, delivered only while the viewer owns
            // no window: open one and ask what to put in it.
            WindowEvent::AppBarDefault => {
                open_and_pick(app, theme, scale);
                return Routed::Served;
            }
            WindowEvent::AppBarMenu { item, .. } if tairix_window::is_quit(*item) => {
                for window in app.windows.drain(..) {
                    window.close(app.client);
                }
                return Routed::Quit;
            }
            // At least one document has been handed to this instance: drain
            // them, opening a window at each.
            WindowEvent::OpenRequested => {
                drain_open_targets(app, theme, scale);
                return Routed::Served;
            }
            _ => {}
        }

        let Some(id) = event.window_id() else {
            return Routed::Served;
        };
        let Some(index) = index_of(app.windows, id) else {
            return Routed::Served;
        };
        // The event's own sink: one window's rectangles are no description of
        // another's surface.
        let mut reported = damage::sink();
        match act(app, index, event, theme, scale, &mut reported) {
            Acted::Changed(repaint) => app.windows[index].owe(repaint, &reported),
            Acted::Close => {
                // The viewer is not its windows: it keeps its icon-bar slot
                // with none open, and a click there opens the next. Only
                // *Quit* ends it.
                let window = app.windows.remove(index);
                app.desk.closed.push(window.pane.id());
                window.close(app.client);
            }
            Acted::Idle => {}
        }
        Routed::Served
    }

    /// Route one event to the window at `index`.
    #[allow(
        clippy::too_many_lines,
        reason = "one dispatch over the window-scoped event vocabulary; splitting it would hide the ordering"
    )]
    fn act(
        app: &mut App<'_>,
        index: usize,
        event: &WindowEvent,
        theme: &Theme,
        scale: Scale,
        reported: &mut Region,
    ) -> Acted {
        let layout = app.windows[index].layout(theme, scale);
        match event {
            WindowEvent::CloseRequested { .. } | WindowEvent::AlternateCloseRequested { .. } => {
                Acted::Close
            }
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => {
                let mode = app::mode_for(*width_px, *height_px);
                let window = &mut app.windows[index];
                if window
                    .pane
                    .resize_with(app.client, &mode, &mut window.surface)
                    == app::Fitted::Kept
                {
                    app::report(APP_NAME, app::SIZE_KEPT);
                }
                // The reported client size is what the layout follows either
                // way, so the whole window is redrawn regardless.
                Acted::Changed(Repaint::Whole)
            }
            WindowEvent::RedrawRequested { .. } => Acted::Changed(Repaint::Whole),
            WindowEvent::ContentReleased { .. } => {
                app.windows[index].pane.release_frames();
                Acted::Idle
            }
            WindowEvent::FilePicked {
                window_id, handle, ..
            } => {
                // A name the session no longer holds leaves the document
                // unnamed rather than refusing it.
                let name = app.client.take_picked_name(*window_id).unwrap_or_default();
                let showing = app.windows[index].view.document().is_some();
                let source = match delegated(*handle, name) {
                    Ok(source) => source,
                    Err(name) => {
                        let _ = app::refuse(
                            app.client,
                            (APP_NAME, app.event_endpoint),
                            showing.then_some(*window_id),
                            &not_opened_headline(&name),
                            "it could not be taken over",
                        );
                        return if showing { Acted::Idle } else { Acted::Close };
                    }
                };
                if showing {
                    // The document shown stays: the one chosen opens in a
                    // window of its own, as every other document does.
                    open_document(app, source, theme, scale);
                    return Acted::Idle;
                }
                app.windows[index].pending = Some(source);
                app.windows[index].view.expect_document();
                reported.add(layout.window());
                Acted::Changed(Repaint::Reported)
            }
            // The user chose nothing, so there is nothing to display: the
            // window they were choosing into closes rather than appearing to
            // state a refusal they already know about. A window that already
            // holds a document keeps it. The viewer asks for no folder, so a
            // folder answer is nothing chosen too.
            WindowEvent::PickCancelled { .. } | WindowEvent::FolderPicked { .. } => {
                if app.windows[index].view.document().is_some() {
                    Acted::Idle
                } else {
                    Acted::Close
                }
            }
            WindowEvent::Key {
                key: pressed @ KeyInput::Pressed { .. },
                ..
            } => {
                let InputEvent::KeyPressed { key, modifiers } = key_input_event(*pressed) else {
                    return Acted::Idle;
                };
                let outcome = app.windows[index]
                    .view
                    .on_key(key, modifiers, &layout, reported);
                apply(app, index, outcome)
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
                let inputs = core::iter::once(held)
                    .chain(pointer_input_events(*action, pointer_point(*x, *y)));
                route_pointer(app, index, inputs, &layout, (scale, theme), reported)
            }
            WindowEvent::MenuClosed {
                open_id, outcome, ..
            } => {
                if app.windows[index].menu != Some(*open_id) {
                    // An answer to a gesture another open has superseded.
                    return Acted::Idle;
                }
                app.windows[index].menu = None;
                let MenuOutcome::Chosen(item) = outcome else {
                    return Acted::Idle;
                };
                let Some(command) = menu_command(*item) else {
                    return Acted::Idle;
                };
                let outcome = app.windows[index].view.run(command, &layout, reported);
                apply(app, index, outcome)
            }
            // The wheel pans the canvas or scrolls the strip of tools,
            // whichever it turned over; with Ctrl it zooms the canvas.
            WindowEvent::Scrolled {
                x,
                y,
                dx,
                dy,
                modifiers,
                ..
            } => {
                let held = key_input_event(KeyInput::ModifiersChanged {
                    modifiers: *modifiers,
                });
                let inputs = core::iter::once(held).chain(scroll_input_events(
                    pointer_point(*x, *y),
                    *dx,
                    *dy,
                ));
                route_pointer(app, index, inputs, &layout, (scale, theme), reported)
            }
            // A pinch zooms the canvas, following the fingers.
            WindowEvent::Pinch {
                x,
                y,
                phase,
                scale: spread,
                modifiers,
                ..
            } => {
                let held = key_input_event(KeyInput::ModifiersChanged {
                    modifiers: *modifiers,
                });
                let inputs = core::iter::once(held).chain(pinch_input_events(
                    pointer_point(*x, *y),
                    *phase,
                    *spread,
                ));
                route_pointer(app, index, inputs, &layout, (scale, theme), reported)
            }
            // A released key or a modifier change runs no command, and focus
            // or minimizing changes nothing drawn. The application-scoped
            // events carry no window, so they were routed before this; the
            // layer feeds, a tool window's moves and the wallpaper answer
            // reply to requests this viewer never makes.
            WindowEvent::Key { .. }
            | WindowEvent::Focus { .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::AppBarEntered { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::AlertClosed { .. }
            | WindowEvent::Authorised { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::SaveOffered { .. }
            | WindowEvent::SaveOfferEnded { .. }
            | WindowEvent::ItemsOffered { .. }
            | WindowEvent::ItemsOfferEnded { .. }
            | WindowEvent::FileOffered { .. }
            | WindowEvent::PreviewRendered { .. } => Acted::Idle,
        }
    }

    /// Shrink `window` to hug the picture it has just opened, answering
    /// whether its geometry moved.
    ///
    /// Shrink-only and once per document: the viewer asks for the client
    /// whose canvas is exactly the picture's own pixels, capped at the window
    /// it opens at and floored at the smallest client it lays out for, so a
    /// small picture is shown at 100% without a frame of empty canvas round
    /// it and a photograph keeps the default window. A refused re-map leaves
    /// the window at the size it had.
    fn hug_picture(
        window: &mut Window,
        client: &mut WindowClient<app::RtWindowTransport>,
        desktop: &Desktop,
        theme: &Theme,
        scale: Scale,
    ) -> bool {
        let Some((want_w, want_h)) =
            window
                .view
                .preferred_client_size(theme, scale, face(theme, scale))
        else {
            return false;
        };
        // Never larger than the display the window has to appear on.
        let screen = desktop.screen();
        let mode = app::mode_for(want_w.min(screen.width), want_h.min(screen.height));
        let held = window.pane.mode();
        if held.width_px == mode.width_px && held.height_px == mode.height_px {
            return false;
        }
        window.pane.resize_with(client, &mode, &mut window.surface) == app::Fitted::Adopted
    }

    /// Carry out whatever an engine outcome asked the embedder for.
    /// Hand window `index`'s view each of `inputs` in order — the modifiers
    /// held, the place, then what happened there — carry out what they asked,
    /// and declare the tip of whatever is now under the pointer: a press can
    /// move a slider and a turn can scroll the strip of tools.
    fn route_pointer(
        app: &mut App<'_>,
        index: usize,
        inputs: impl Iterator<Item = InputEvent>,
        layout: &Layout,
        (scale, theme): (Scale, &Theme),
        reported: &mut Region,
    ) -> Acted {
        let mut changed = false;
        let mut asked = None;
        for input in inputs {
            let outcome = app.windows[index]
                .view
                .on_pointer(&input, layout, scale, theme, reported);
            changed |= outcome.changed;
            if outcome.pick || outcome.menu.is_some() || outcome.close {
                asked = Some(outcome);
            }
        }
        set_tip(&mut app.windows[index], app.client, layout, (scale, theme));
        match asked {
            Some(outcome) => apply(app, index, outcome),
            None if changed => Acted::Changed(Repaint::Reported),
            None => Acted::Idle,
        }
    }

    fn apply(app: &mut App<'_>, index: usize, outcome: Outcome) -> Acted {
        if outcome.close {
            return Acted::Close;
        }
        if outcome.pick
            && !ask_for_document(&app.windows[index], app.client, app.event_endpoint)
            && app.windows[index].view.document().is_none()
        {
            return Acted::Close;
        }
        if let Some(at) = outcome.menu {
            open_menu(&mut app.windows[index], app.client, at);
        }
        if outcome.changed {
            Acted::Changed(Repaint::Reported)
        } else {
            Acted::Idle
        }
    }

    /// Open a window with nothing in it and ask the picker what to put there.
    ///
    /// What the icon-bar slot's primary click means. Its present is withheld
    /// until there is something to show, so the window appears with the
    /// document in it rather than sitting blank behind the chooser.
    fn open_and_pick(app: &mut App<'_>, theme: &Theme, scale: Scale) {
        let Some(opened) = open_window(
            app.client,
            app.event_endpoint,
            app.server,
            app.desktop,
            theme,
            scale,
            None,
        ) else {
            return;
        };
        if !ask_for_document(&opened, app.client, app.event_endpoint) {
            opened.close(app.client);
            return;
        }
        app.windows.push(opened);
    }

    /// Drain every document the desktop has handed to this instance, opening
    /// a window at each.
    ///
    /// How a launch that names a document reaches an instance already
    /// running: the desktop relays the authority to this process and wakes
    /// it, rather than starting a second viewer.
    fn drain_open_targets(app: &mut App<'_>, theme: &Theme, scale: Scale) {
        loop {
            let target = match app.client.take_open_target() {
                Ok(Some(target)) => target,
                Ok(None) => return,
                Err(err) => {
                    app::report(APP_NAME, format_args!("cannot take an open target ({err})"));
                    return;
                }
            };
            let source = match target {
                Target::Document { name, grant, .. } => match delegated(grant, name) {
                    Ok(source) => source,
                    Err(name) => {
                        let _ = app::refuse(
                            app.client,
                            (APP_NAME, app.event_endpoint),
                            None,
                            &not_opened_headline(&name),
                            "it could not be taken over",
                        );
                        continue;
                    }
                },
                // Honest, and unreachable from the desktop, which hands this
                // viewer a descriptor precisely because it holds no
                // filesystem authority to open a name with.
                Target::Path(path) | Target::Image(path) => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{path} was handed over as a path; this viewer holds no filesystem \
                         authority and can only be given an open document"
                        ),
                    );
                    continue;
                }
                // This viewer places nothing in a document, so it answers no
                // file offer and no drop is placed in it; one that reached it
                // is let go rather than left pending.
                Target::Placed { name, grant, .. } => {
                    drop(tairix_rt::File::from_delegation(grant));
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{name} was dropped to be placed, but this viewer places nothing"
                        ),
                    );
                    continue;
                }
                // This viewer is one place: it shows a document, and has no
                // pane a launch could name.
                Target::Pane(pane) => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{pane} was handed over, but this viewer shows a document and has no \
                         places to go to"
                        ),
                    );
                    continue;
                }
            };
            open_document(app, source, theme, scale);
        }
    }

    /// Open a window at `source`, withheld from the screen until its document
    /// is in.
    fn open_document(app: &mut App<'_>, source: Source, theme: &Theme, scale: Scale) {
        let Some(opened) = open_window(
            app.client,
            app.event_endpoint,
            app.server,
            app.desktop,
            theme,
            scale,
            Some(source),
        ) else {
            return;
        };
        app.windows.push(opened);
    }

    /// Ask the session to open the app's own menu at `at`.
    ///
    /// The plate is the session's — the app draws no menu pixel — and a
    /// session that composes none is reported and carried on from.
    fn open_menu(
        window: &mut Window,
        client: &mut WindowClient<app::RtWindowTransport>,
        at: Point,
    ) {
        let (menu, skipped) = build_menu(&window.view);
        if skipped > 0 {
            app::report(
                APP_NAME,
                format_args!("{skipped} menu row(s) do not fit and are not shown"),
            );
        }
        let Ok(anchor) = WindowRegion::new(at.x, at.y, 0, 0) else {
            return;
        };
        match client.open_menu(window.pane.id(), anchor, &menu) {
            Ok(open) => window.menu = Some(open),
            Err(_) => app::report(APP_NAME, "the desktop composes no menu service"),
        }
    }

    tairix_rt::entry!(main);
}

/// The host stub: this binary is a freestanding program on the Tier-1
/// targets, so on the host it exists only to keep the file covered by the
/// workspace build, clippy, and fmt.
#[cfg(not(freestanding))]
fn main() {}
