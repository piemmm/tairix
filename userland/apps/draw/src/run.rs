//! Draw's `Run` binary: the document host's windows over the Draw engine, the
//! workers that keep reading, rendering and writing off the event loop, and
//! the sandbox worker this binary is re-entered as to read documents.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::borrow::Cow;
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::fmt;

    use tairix_abi::fs::Filetype;
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::window_ipc::{AppMenuSymbol, ClipboardKind, CLIPBOARD_MAX_BYTES};
    use tairix_abi::Errno;
    use tairix_appdata::{
        Opening, Publication, PublishAnswer, PublishJob, PublishWorker, Refusal as StoreRefusal,
        RtHost, Settings,
    };
    use tairix_bezier::{Affine, Bounds, Point};
    use tairix_collections::try_arc;
    use tairix_controls::damage;
    use tairix_draw::appwindow::{DrawWindow, DrawWindowAsk};
    use tairix_draw::compute::{work_out, Compute, Computed};
    use tairix_draw::copy::{CopyFormat, SaveFormat, FLATTENED};
    use tairix_draw::document::{blank, Document};
    use tairix_draw::glyphs::{Fetched, Lettering};
    use tairix_draw::layout::{Faces, WINDOW_SIZE};
    use tairix_draw::pdfopen::{Answered, Asked, Given, OpenPdf};
    use tairix_draw::preferences::Preferences;
    use tairix_draw::settings::{Category, DrawSettings};
    use tairix_draw::tiles::{RenderJob, TileKey, TILE};
    use tairix_draw::view::{Own, Snapshot, View};
    use tairix_drawing::render::{artwork, Afresh, Mode, View as Square};
    use tairix_drawing::unkept::Unkept;
    use tairix_drawing::Read;
    use tairix_drawing::{Drawing, DrawingError, Ids, Image};
    use tairix_geometry::{Region, Scale};
    use tairix_icon::{
        artwork_cache, ArtworkCache, IconArtworkSource, InlineArtwork, NoArtworkSeam,
    };
    use tairix_parallel::{JobRunner, Pool};
    use tairix_raster::Surface;
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_sandbox::drawedit::{
        self, open_drawing, page_size, read_page, read_placed, DrawEditService, DrawFailure,
        DrawRefusal, WORKER_MEMORY,
    };
    use tairix_sandbox::imagerender::{send_document, upload_document, UploadFailure};
    use tairix_sandbox::rt::{document_room, serve_stdio, worker_role, RtLauncher};
    use tairix_sandbox::ParserSandbox;
    use tairix_theme::Theme;
    use tairix_util::defer::JobQueue;
    use tairix_util::fallible;
    use tairix_window::app;
    use tairix_window::docapp::settings::{Edit, SettingsRequest, Then};
    use tairix_window::docapp::{
        self, AnswerWake, DocumentApp, Handle, Host, Outcome, Stamp, APP_TOKEN,
    };
    use tairix_window::document::{Access, ReadFailure, SaveJob, SavedDocument, UNTITLED};
    use tairix_window::Desktop;

    const APP_NAME: &str = "Draw";

    /// The renderer's wake.
    const RENDER_TOKEN: u64 = APP_TOKEN + 1;

    /// The settings writer's wake.
    const SETTINGS_TOKEN: u64 = APP_TOKEN + 2;

    type Reader = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    /// What reading a document came to.
    #[derive(Debug)]
    enum ReadRefusal {
        Read(ReadFailure),
        /// Longer than the worker may hold: its room, in bytes.
        TooLong(u64),
        Upload(UploadFailure<Errno>),
        Unreadable(String),
    }

    impl fmt::Display for ReadRefusal {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Read(failure) => write!(f, "{failure}"),
                Self::TooLong(room) => write!(
                    f,
                    "it is longer than the {} MiB a drawing file may be on this computer",
                    room >> 20
                ),
                Self::Upload(failure) => write!(f, "{failure}"),
                Self::Unreadable(why) => f.write_str(why),
            }
        }
    }

    struct LoadJob {
        stamp: Stamp,
        work: LoadWork,
    }

    enum LoadWork {
        /// Open a document afresh.
        Open {
            handle: Arc<Handle>,
            name: String,
        },
        /// Answer what the PDF kept for the job's window asked, the document
        /// named `name`.
        Answer {
            name: String,
            given: Given,
        },
        /// Size a page, counting from 0, of the PDF kept for the job's window.
        PageSize(u32),
        /// Let the PDF kept for the job's window go.
        Forget,
        Paste(Vec<u8>),
        Place {
            handle: Handle,
            name: String,
            at: Option<tairix_geometry::Point>,
        },
    }

    struct LoadReply {
        stamp: Stamp,
        answer: LoadAnswer,
    }

    enum LoadAnswer {
        Opened {
            handle: Arc<Handle>,
            name: String,
            read: Result<(Read, Ids), ReadRefusal>,
            recorded: Option<Filetype>,
        },
        /// A PDF that asks something before it opens, kept until it is
        /// answered or let go.
        Asks {
            name: String,
            asked: Asked,
        },
        /// The size of a page, counting from 0, of the PDF kept for the
        /// window, where it could be had.
        PageSize {
            page: u32,
            size: Option<(f64, f64)>,
        },
        /// What the PDF kept for the window was answered with found none
        /// kept.
        Gone {
            name: String,
        },
        Pasted(Result<Drawing, String>),
        Placed {
            name: String,
            at: Option<tairix_geometry::Point>,
            placed: Result<Image, String>,
        },
    }

    type LoadDesk = JobQueue<Option<LoadJob>, Option<LoadReply>>;
    type LoadWorker = Worker<Loader, Option<LoadJob>, Option<LoadReply>, LoadDesk>;

    /// How long the regular file `file` measures, within `room`, what a
    /// worker may hold.
    fn document_length(file: &Handle, room: u64) -> Result<usize, ReadRefusal> {
        let length = file
            .regular_len()
            .map_err(|err| ReadRefusal::Read(ReadFailure::Unreadable(err)))?
            .ok_or(ReadRefusal::Read(ReadFailure::NotAFile))?;
        if length > room {
            return Err(ReadRefusal::TooLong(room));
        }
        usize::try_from(length).map_err(|_| ReadRefusal::TooLong(room))
    }

    /// The documents being read: the sandbox the next is read in, and the
    /// workers each keeping a PDF whose window asks something of it.
    struct Loader {
        sandbox: Reader,
        held: Vec<Held>,
    }

    /// A worker keeping the PDF `stamp`'s window asks something of, and what
    /// it was opened from: a document `handed` bytes long.
    struct Held {
        stamp: Stamp,
        sandbox: Reader,
        handed: u64,
        handle: Arc<Handle>,
        name: String,
    }

    /// A sandbox whose worker, started by its first request, reads one
    /// document.
    fn reader() -> Reader {
        ParserSandbox::new(RtLauncher::own_binary(WORKER_MEMORY), tairix_rt::LogSink)
    }

    /// The answer of a document that is read, or could not be.
    fn opened(
        handle: Arc<Handle>,
        name: String,
        read: Result<(Read, Ids), ReadRefusal>,
    ) -> LoadAnswer {
        LoadAnswer::Opened {
            read,
            recorded: tairix_browse::recorded_filetype_of(&handle),
            handle,
            name,
        }
    }

    /// What a PDF that failed to open asks: its password, where it wanted
    /// one; anything else is why it is unreadable.
    fn failed(failure: DrawFailure) -> Result<Asked, ReadRefusal> {
        match failure {
            DrawFailure::Refused(DrawRefusal::NeedsPassword) => {
                Ok(Asked::Password { wrong: false })
            }
            DrawFailure::Refused(DrawRefusal::WrongPassword) => Ok(Asked::Password { wrong: true }),
            failure => Err(ReadRefusal::Unreadable(alloc::format!("{failure}"))),
        }
    }

    impl Loader {
        /// The worker kept for `stamp`'s window, taken.
        fn take(&mut self, stamp: Stamp) -> Option<Held> {
            let index = self.held.iter().position(|held| held.stamp == stamp)?;
            Some(self.held.swap_remove(index))
        }

        /// Read `handle`'s document, called `name`, for `stamp`'s window: a
        /// PDF of several pages asks which, and one protected by a password
        /// asks for it, its worker kept until it is answered.
        fn open(&mut self, stamp: Stamp, handle: Arc<Handle>, name: String) -> LoadAnswer {
            // A window asks one thing at a time.
            self.held
                .retain(|held| held.stamp.window() != stamp.window());
            let length = match document_length(&handle, self.sandbox.document_room()) {
                Ok(length) => length,
                Err(refusal) => return opened(handle, name, Err(refusal)),
            };
            let fd = handle.fd();
            let uploaded = upload_document(&mut self.sandbox, length, |offset, into| {
                tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
            });
            // The worker that reads this document reads no other.
            let mut sandbox = core::mem::replace(&mut self.sandbox, reader());
            if let Err(failure) = uploaded {
                return opened(handle, name, Err(ReadRefusal::Upload(failure)));
            }
            let handed = length as u64;
            let mut ids = Ids::new();
            let opening = open_drawing(&mut sandbox, handed, None, true, &mut ids);
            let held = Held {
                stamp,
                sandbox,
                handed,
                handle,
                name,
            };
            self.settle(held, opening.map(|opening| (opening, ids)))
        }

        /// Answer what the PDF kept for `stamp`'s window asked with `given`,
        /// where one is kept.
        fn answer(&mut self, stamp: Stamp, given: Given) -> Option<LoadAnswer> {
            let mut held = self.take(stamp)?;
            let mut ids = Ids::new();
            let opening = match given {
                Given::Password(password) => open_drawing(
                    &mut held.sandbox,
                    held.handed,
                    Some(&password),
                    true,
                    &mut ids,
                ),
                Given::Page(page) => read_page(&mut held.sandbox, held.handed, page, &mut ids)
                    .map(drawedit::Opening::Read),
            };
            Some(self.settle(held, opening.map(|opening| (opening, ids))))
        }

        /// Page `page`'s size, of the PDF kept for `stamp`'s window, where one
        /// is kept and its size could be had.
        fn page_size(&mut self, stamp: Stamp, page: u32) -> Option<(f64, f64)> {
            let held = self.held.iter_mut().find(|held| held.stamp == stamp)?;
            page_size(&mut held.sandbox, page).ok()
        }

        /// What `held`'s document came to, `opening`: its worker kept where
        /// it asks something, and let go otherwise.
        fn settle(
            &mut self,
            mut held: Held,
            opening: Result<(drawedit::Opening, Ids), DrawFailure>,
        ) -> LoadAnswer {
            let asked = match opening {
                Ok((drawedit::Opening::Read(read), ids)) => {
                    return opened(held.handle, held.name, Ok((read, ids)));
                }
                Ok((drawedit::Opening::Pages { count }, _)) => {
                    let first = page_size(&mut held.sandbox, 0).ok();
                    return self.keep(held, Asked::Page { count, first });
                }
                Err(failure) => match failed(failure) {
                    Ok(asked) => asked,
                    Err(refusal) => return opened(held.handle, held.name, Err(refusal)),
                },
            };
            self.keep(held, asked)
        }

        /// Keep `held`'s worker until what its document asks, `asked`, is
        /// answered or let go.
        fn keep(&mut self, held: Held, asked: Asked) -> LoadAnswer {
            if self.held.try_reserve(1).is_err() {
                return opened(
                    held.handle,
                    held.name,
                    Err(ReadRefusal::Read(ReadFailure::NoMemory)),
                );
            }
            let name = held.name.clone();
            self.held.push(held);
            LoadAnswer::Asks { name, asked }
        }
    }

    fn paste(sandbox: &mut Reader, bytes: &[u8]) -> Result<Drawing, String> {
        let read = send_document(sandbox, bytes)
            .map_err(|failure| {
                alloc::format!("The clipboard's drawing could not be handed over: {failure}")
            })
            .and_then(|()| {
                open_drawing(sandbox, bytes.len() as u64, None, false, &mut Ids::new())
                    .map_err(|failure| alloc::format!("The clipboard holds no drawing: {failure}"))
            });
        sandbox.release();
        match read? {
            drawedit::Opening::Read(read) => Ok(read.drawing),
            drawedit::Opening::Pages { .. } => Err(String::from("The clipboard holds no drawing")),
        }
    }

    /// The picture `handle` holds, decoded in the sandbox and made a picture
    /// a drawing holds, its levels built here rather than on the event loop.
    fn place(sandbox: &mut Reader, handle: &Handle) -> Result<Image, String> {
        let fd = handle.fd();
        let length = match document_length(handle, sandbox.document_room()) {
            Ok(length) => length,
            Err(ReadRefusal::TooLong(room)) => {
                return Err(alloc::format!(
                    "it is longer than the {} MiB a picture placed may be on this computer",
                    room >> 20
                ));
            }
            Err(refusal) => return Err(alloc::format!("{refusal}")),
        };
        let placed = upload_document(sandbox, length, |offset, into| {
            tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
        })
        .map_err(|failure: UploadFailure<Errno>| alloc::format!("{failure}"))
        .and_then(|()| {
            read_placed(sandbox, length as u64).map_err(|failure| alloc::format!("{failure}"))
        });
        // The worker that read this picture reads no other.
        sandbox.release();
        let placed = placed?;
        let place = Bounds::from_points([Point::ZERO, placed.natural]);
        Image::new(placed.pixels, place, placed.source).map_err(|error| match error {
            DrawingError::OutOfMemory => String::from("there is not enough memory to hold it"),
            DrawingError::TooComplex => String::from("it is larger than a drawing holds"),
            DrawingError::Invalid => String::from("it states a size a drawing cannot hold"),
        })
    }

    fn serve_load(loader: &mut Loader, job: &mut Option<LoadJob>) -> Option<LoadReply> {
        let LoadJob { stamp, work } = job.take()?;
        let answer = match work {
            LoadWork::Open { handle, name } => loader.open(stamp, handle, name),
            LoadWork::Answer { name, given } => loader
                .answer(stamp, given)
                .unwrap_or(LoadAnswer::Gone { name }),
            LoadWork::PageSize(page) => LoadAnswer::PageSize {
                page,
                size: loader.page_size(stamp, page),
            },
            LoadWork::Forget => {
                drop(loader.take(stamp));
                return None;
            }
            LoadWork::Paste(bytes) => LoadAnswer::Pasted(paste(&mut loader.sandbox, &bytes)),
            LoadWork::Place { handle, name, at } => LoadAnswer::Placed {
                placed: place(&mut loader.sandbox, &handle),
                name,
                at,
            },
        };
        Some(LoadReply { stamp, answer })
    }

    /// One tile rendered.
    struct Rendered {
        key: TileKey,
        surface: Surface,
    }

    /// What a render job came to: the scene it drew, the tiles it was asked
    /// for, and those of them it drew.
    struct Drawn {
        scene: u64,
        keys: Vec<TileKey>,
        tiles: Vec<Rendered>,
    }

    type RenderAsk = Option<(Stamp, RenderJob)>;
    type RenderAnswer = Option<(Stamp, Drawn)>;
    type RenderDesk = JobQueue<RenderAsk, RenderAnswer>;
    type RenderWorker = Worker<Pool, RenderAsk, RenderAnswer, RenderDesk>;

    /// A core's share of a render job: the glyphs its tiles share, each made
    /// once, and the tiles it drew.
    struct Lane {
        glyphs: Lettering<Fetched>,
        drawn: Vec<Rendered>,
    }

    /// Draw a job's tiles across the cores, each core taking the next tile as
    /// it finishes one.
    fn serve_render(pool: &mut Pool, job: &mut RenderAsk) -> RenderAnswer {
        let (stamp, job) = job.take()?;
        let width = pool.width().clamp(1, job.keys.len().max(1));
        let mut lanes: Vec<Lane> = Vec::new();
        if lanes.try_reserve(width).is_ok() {
            lanes.extend((0..width).map(|_| Lane {
                glyphs: Lettering::new(Fetched),
                drawn: Vec::new(),
            }));
            let keys = job.keys.iter().copied();
            tairix_parallel::for_each_ordered(&*pool, &mut lanes, keys, &|lane, _, key| {
                let Some(surface) = render_tile(&job.drawing, key, job.mode, &lane.glyphs) else {
                    return;
                };
                if lane.drawn.try_reserve(1).is_ok() {
                    lane.drawn.push(Rendered { key, surface });
                }
            });
        }
        let mut tiles = Vec::new();
        let count = lanes.iter().map(|lane| lane.drawn.len()).sum();
        if tiles.try_reserve(count).is_ok() {
            for lane in lanes {
                tiles.extend(lane.drawn);
            }
        }
        Some((
            stamp,
            Drawn {
                scene: job.scene,
                keys: job.keys,
                tiles,
            },
        ))
    }

    /// The tile at `key` of `drawing`, drawn as `mode` says with `glyphs`,
    /// or `None` where memory would not hold it.
    fn render_tile(
        drawing: &Drawing,
        key: TileKey,
        mode: Mode,
        glyphs: &Lettering<Fetched>,
    ) -> Option<Surface> {
        let mut surface = Surface::new(TILE, TILE)?;
        let square = Square {
            to_device: Affine::scale(key.pixels(), key.pixels()),
            origin: key.origin(),
            side: f64::from(TILE),
            design: TILE * 256,
            mode,
            ground: false,
        };
        let art = artwork(drawing, &square, glyphs, &Afresh).ok()?;
        surface
            .draw_artwork(&art.nodes, square.design)
            .then_some(surface)
    }

    /// Work on the host's queue.
    enum Work {
        Compute(Compute),
        Copy {
            target: Arc<Handle>,
            name: String,
            drawing: Drawing,
        },
        /// The selection, to write as SVG for the clipboard.
        Clip(Drawing),
    }

    /// What the queue's work came to.
    enum Answer {
        Computed(Box<Computed>),
        Copied(Result<String, String>),
        Clipped(Result<String, &'static str>),
    }

    /// `drawing` as the SVG text the clipboard takes.
    fn clipped(drawing: &Drawing) -> Result<String, &'static str> {
        let text = tairix_drawing::svg::write(drawing).map_err(|error| match error {
            DrawingError::OutOfMemory => "there is not enough memory to write it",
            DrawingError::Invalid | DrawingError::TooComplex => "it could not be written as SVG",
        })?;
        if text.len() > CLIPBOARD_MAX_BYTES {
            return Err("it is larger than the clipboard holds");
        }
        Ok(text)
    }

    /// Why a save failed.
    enum SaveFailure {
        /// Its name says no format Draw saves in.
        Format,
        /// The drawing could not be written in its format, for this reason.
        Encode(String),
        Write(Errno),
    }

    impl fmt::Display for SaveFailure {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Format => f.write_str("its name says no format Draw saves in"),
                Self::Encode(why) => f.write_str(why),
                Self::Write(errno) => write!(f, "the file could not be written ({errno})"),
            }
        }
    }

    fn write_all(file: &Handle, bytes: &[u8]) -> Result<(), Errno> {
        tairix_rt::fs_write_all(file.fd(), 0, bytes)?;
        docapp::commit(file, u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    }

    /// Said of a file Draw writes that its sandbox worker, holding a document
    /// to its share of this computer, would not read back.
    const PAST_OPENING: &str =
        "The file is larger than Draw opens on this computer, so Draw cannot open it again here";

    /// `note`, what a file written could not keep, and `said`, whether Draw
    /// can open it again, as one caveat.
    fn caveat(note: Option<String>, said: Option<&'static str>) -> Option<Cow<'static, str>> {
        match (note, said) {
            (None, None) => None,
            (None, Some(said)) => Some(Cow::Borrowed(said)),
            (Some(note), None) => Some(Cow::Owned(note)),
            (Some(note), Some(said)) => Some(Cow::Owned(alloc::format!("{note}. {said}"))),
        }
    }

    /// Whether Draw's worker would read back `bytes` of a file it opens.
    fn past_opening(bytes: &[u8]) -> bool {
        bytes.len() as u64 > document_room(WORKER_MEMORY)
    }

    /// Write a copy of `drawing` to `target`, in the format its name says.
    fn copy(target: &Handle, name: &str, drawing: &Drawing) -> Result<String, String> {
        let recorded = tairix_browse::recorded_filetype_of(target);
        let format = CopyFormat::of(name, recorded).ok_or_else(|| {
            String::from(
                "name the copy with .pdf, .eps, .png, .jpg, .tif or .svg, or ,aff for a Draw file",
            )
        })?;
        let copied = tairix_draw::copy::copy(format, drawing, &Lettering::new(Fetched))?;
        write_all(target, &copied.bytes)
            .map_err(|err| alloc::format!("it could not be written ({err})"))?;
        let reopened = matches!(format, CopyFormat::Svg | CopyFormat::Draw);
        let past = (reopened && past_opening(&copied.bytes)).then_some(PAST_OPENING);
        Ok(match caveat(copied.note, past) {
            Some(caveat) => alloc::format!("Saved a copy as {name}. {caveat}"),
            None => alloc::format!("Saved a copy as {name}"),
        })
    }

    struct Draw {
        loader: Arc<LoadWorker>,
        _loader_guard: WorkerGuard<Loader, Option<LoadJob>, Option<LoadReply>, LoadDesk>,
        /// The windows whose kept PDF is to be let go once the loader has
        /// room for the asking.
        owed: Vec<Stamp>,
        renderer: Arc<RenderWorker>,
        _renderer_guard: WorkerGuard<Pool, RenderAsk, RenderAnswer, RenderDesk>,
        /// The settings in force, the ones on screen, and what the store said.
        preferences: Publication<Preferences>,
        writer: Arc<PublishWorker<Preferences>>,
        _writer_guard: WorkerGuard<Opening, PublishJob<Preferences>, PublishAnswer<Preferences>>,
        artwork: ArtworkCache,
    }

    fn icon_cache(desktop: &Desktop) -> ArtworkCache {
        // The reclaim bookkeeping's audit sink, borrowed for the program's life.
        static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;
        let (width, height) = desktop.window_size(WINDOW_SIZE.0, WINDOW_SIZE.1);
        let frame = app::region_bytes(&app::mode_for(width, height), app::FRAME_COUNT).unwrap_or(0);
        let cache = artwork_cache(
            "draw.icon-artwork",
            SEAT_PRIMARY,
            (
                frame,
                tairix_procinfo::memory_total_for_budget(&tairix_procinfo::IpcTransport),
            ),
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
        );
        if let Some(ledger) = cache.ledger() {
            tairix_rt::cachereport::register(ledger);
        }
        cache
    }

    fn scale_of(host: &Host<Draw>) -> f64 {
        f64::from(host.desktop.scale().percent()) / 100.0
    }

    fn new_view(host: &Host<Draw>, name: &str, access: Access) -> Result<View, String> {
        let preferences = host.app.preferences.live();
        let mut ids = Ids::new();
        let drawing =
            blank(&mut ids, preferences).map_err(|_| String::from("there is not enough memory"))?;
        let document = Document::new(drawing, ids, true);
        let mut view = View::new(
            document,
            String::from(name),
            access,
            Unkept::NONE,
            scale_of(host),
        );
        view.begin(preferences);
        Ok(view)
    }

    /// Ask the renderer for `job`: a window has one out at a time, so it never
    /// waits behind another of its own.
    fn submit_render(host: &mut Host<Draw>, index: usize, job: RenderJob) {
        let stamp = host.windows[index].stamp();
        match host.app.renderer.submit(Some((stamp, job))) {
            Ok(answered) => host.note_answered(answered),
            Err(refused) => {
                if let Some((_, job)) = refused {
                    host.windows[index].view.render_refused(&job.keys);
                }
            }
        }
    }

    /// What a PDF's question is asked for: the window loading it, and the
    /// document's name.
    struct Ticket {
        stamp: Stamp,
        name: String,
    }

    impl Draw {
        /// Let the PDF the loader keeps for `stamp`'s window go, now or once
        /// the loader has room for the asking, answering whether the asking
        /// ran here.
        fn forget(&mut self, stamp: Stamp) -> bool {
            let job = LoadJob {
                stamp,
                work: LoadWork::Forget,
            };
            if let Ok(answered) = self.loader.submit(Some(job)) {
                return answered;
            }
            if !fallible::push(&mut self.owed, stamp) {
                app::report(APP_NAME, "a document's reader could not be let go");
            }
            false
        }
    }

    /// Let the PDF the loader keeps for `stamp`'s window go.
    fn forget(host: &mut Host<Draw>, stamp: Stamp) {
        let answered = host.app.forget(stamp);
        host.note_answered(answered);
    }

    /// Ask again to let go of what the loader had no room to be asked to.
    fn forget_owed(host: &mut Host<Draw>) {
        for stamp in core::mem::take(&mut host.app.owed) {
            forget(host, stamp);
        }
    }

    fn adopt_load(host: &mut Host<Draw>, reply: LoadReply) {
        let Some(index) = host.showing(reply.stamp) else {
            // What the loader keeps for a window that has gone goes too.
            if matches!(reply.answer, LoadAnswer::Asks { .. }) {
                forget(host, reply.stamp);
            }
            return;
        };
        match reply.answer {
            LoadAnswer::Asks { name, asked } => {
                let ticket = Ticket {
                    stamp: reply.stamp,
                    name: name.clone(),
                };
                let question = DrawWindow::OpenPdf(OpenPdf::new(&name, asked, ticket));
                // A window refused is said by the host, in an alert of its own.
                if host.open_app_window(question, Some(index)).is_none() {
                    forget(host, reply.stamp);
                    host.not_wanted(index);
                }
            }
            LoadAnswer::PageSize { page, size } => sized(host, reply.stamp, page, size),
            LoadAnswer::Gone { name } => {
                host.not_opened(index, &name, &"the reader that opened it has stopped");
            }
            LoadAnswer::Opened {
                handle,
                name,
                read,
                recorded,
            } => match read {
                Ok((read, ids)) => {
                    let access = host.windows[index].view.access();
                    let document = Document::new(read.drawing, ids, true);
                    let mut view = View::new(document, name, access, read.unkept, scale_of(host))
                        .with_recorded(recorded);
                    view.begin(host.app.preferences.live());
                    host.show(index, view, Some(handle));
                }
                Err(refusal) => host.not_opened(index, &name, &refusal),
            },
            LoadAnswer::Pasted(pasted) => {
                let window = &mut host.windows[index];
                window.view.pasted(pasted, &mut window.damage);
                window.owe_reported();
            }
            LoadAnswer::Placed { name, at, placed } => {
                let window = &mut host.windows[index];
                window
                    .view
                    .placed_picture((&name, at), placed, &mut window.damage);
                window.owe_reported();
            }
        }
    }

    fn adopt_render(host: &mut Host<Draw>, stamp: Stamp, drawn: Drawn) {
        let Some(index) = host.showing(stamp) else {
            return;
        };
        let window = &mut host.windows[index];
        let landed = drawn
            .tiles
            .into_iter()
            .map(|Rendered { key, surface }| (key, surface));
        window
            .view
            .rendered(drawn.scene, (&drawn.keys, landed), &mut window.damage);
        window.owe_reported();
    }

    impl DocumentApp for Draw {
        type View = View;
        type AppView = DrawWindow<Ticket>;
        type Snapshot = Snapshot;
        type Extra = ();
        type Work = Work;
        type Answer = Answer;
        type Failure = SaveFailure;

        const NAME: &'static str = APP_NAME;
        const WINDOW_SIZE: (u32, u32) = WINDOW_SIZE;
        const JOBS_PER_WINDOW: usize = 4;
        const BAR_ROWS: &'static [(AppMenuSymbol, &'static str)] =
            &[(AppMenuSymbol::Settings, "Settings\u{2026}")];

        fn start(desktop: &Desktop, set: u64) -> Result<Self, i32> {
            let loader = LoadWorker::queued(
                serve_load,
                Loader {
                    sandbox: reader(),
                    held: Vec::new(),
                },
                WorkerWake::create(),
                0,
            );
            let Some(loader) = loader.ok().and_then(|loader| try_arc(loader).ok()) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the reading queue",
                ));
            };
            app::start_worker(APP_NAME, &loader, set, APP_TOKEN, "read");
            let loader_guard = WorkerGuard::new(&loader);
            let online = tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport);
            let pool = app::pool_across(APP_NAME, online, "rendering drawings");
            let renderer = RenderWorker::queued(serve_render, pool, WorkerWake::create(), 0);
            let Some(renderer) = renderer.ok().and_then(|renderer| try_arc(renderer).ok()) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the render queue",
                ));
            };
            app::start_worker(APP_NAME, &renderer, set, RENDER_TOKEN, "render");
            let renderer_guard = WorkerGuard::new(&renderer);
            // Read here, before any window: nothing is on screen yet, so
            // there is no frame to owe anyone.
            let (preferences, refusals) =
                tairix_appdata::loaded(&Settings::open_without_defaults(&mut RtHost));
            for refusal in refusals {
                app::report(APP_NAME, refusal);
            }
            let Ok(writer) = try_arc(tairix_appdata::publish_worker(Opening::Bare)) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the settings writer",
                ));
            };
            app::start_worker(APP_NAME, &writer, set, SETTINGS_TOKEN, "settings");
            let writer_guard = WorkerGuard::new(&writer);
            Ok(Self {
                loader,
                _loader_guard: loader_guard,
                owed: Vec::new(),
                renderer,
                _renderer_guard: renderer_guard,
                preferences: Publication::new(preferences),
                writer,
                _writer_guard: writer_guard,
                artwork: icon_cache(desktop),
            })
        }

        fn wakes(&self) -> Vec<(u64, Arc<dyn AnswerWake>)> {
            let loader: Arc<dyn AnswerWake> = self.loader.clone();
            let renderer: Arc<dyn AnswerWake> = self.renderer.clone();
            let writer: Arc<dyn AnswerWake> = self.writer.clone();
            alloc::vec![
                (APP_TOKEN, loader),
                (RENDER_TOKEN, renderer),
                (SETTINGS_TOKEN, writer),
            ]
        }

        fn faces(theme: &Theme, scale: Scale) -> Faces {
            Faces::of(theme, scale)
        }

        fn damage_sink() -> Region {
            damage::sink()
        }

        fn write(
            job: &SaveJob<Handle, Snapshot>,
            name: &str,
        ) -> Result<Option<Cow<'static, str>>, SaveFailure> {
            let recorded = tairix_browse::recorded_filetype_of(&job.target);
            let format = SaveFormat::of(name, recorded).ok_or(SaveFailure::Format)?;
            let written = tairix_draw::copy::copy(
                format.written(),
                &job.snapshot.drawing,
                &Lettering::new(Fetched),
            )
            .map_err(SaveFailure::Encode)?;
            write_all(&job.target, &written.bytes).map_err(SaveFailure::Write)?;
            let said = if !format.keeps_editing() {
                Some(FLATTENED)
            } else if past_opening(&written.bytes) {
                Some(PAST_OPENING)
            } else {
                None
            };
            Ok(caveat(written.note, said))
        }

        fn work(work: Work) -> Answer {
            match work {
                Work::Compute(compute) => {
                    Answer::Computed(Box::new(work_out(compute, &Lettering::new(Fetched))))
                }
                Work::Copy {
                    target,
                    name,
                    drawing,
                } => Answer::Copied(copy(&target, &name, &drawing)),
                Work::Clip(drawing) => Answer::Clipped(clipped(&drawing)),
            }
        }

        fn outlives_document(work: &Work) -> bool {
            matches!(work, Work::Copy { .. })
        }

        fn reserve_window(&mut self) -> Result<(), &'static str> {
            self.loader
                .grow(2)
                .map_err(|_| "no room for another window's reading")?;
            if self.renderer.grow(2).is_err() {
                self.loader.shrink(2);
                return Err("no room for another window's rendering");
            }
            Ok(())
        }

        fn release_window(&mut self) {
            self.loader.shrink(2);
            self.renderer.shrink(2);
        }

        /// An answer to a PDF's question withdrawn still lets the PDF go,
        /// and letting one go is never withdrawn.
        fn withdraw(&mut self, window: u64) {
            let mut answered = Vec::new();
            self.loader.retain_waiting(|job| {
                let Some(job) = job.as_ref().filter(|job| job.stamp.window() == window) else {
                    return true;
                };
                match job.work {
                    LoadWork::Forget => true,
                    // Run for nothing where it cannot be noted, but never
                    // leaving its worker kept.
                    LoadWork::Answer { .. } => !fallible::push(&mut answered, job.stamp),
                    _ => false,
                }
            });
            for stamp in answered {
                self.forget(stamp);
            }
            self.renderer.retain_waiting(|job| {
                job.as_ref()
                    .is_none_or(|(stamp, _)| stamp.window() != window)
            });
        }

        fn untitled(host: &Host<Self>) -> Result<View, String> {
            new_view(host, UNTITLED, Access::Untitled)
        }

        fn placeholder(host: &Host<Self>, name: &str, access: Access) -> Result<View, String> {
            new_view(host, name, access)
        }

        fn load(host: &mut Host<Self>, index: usize, handle: Arc<Handle>, name: String) {
            let job = LoadJob {
                stamp: host.windows[index].stamp(),
                work: LoadWork::Open {
                    handle,
                    name: name.clone(),
                },
            };
            match host.app.loader.submit(Some(job)) {
                Ok(answered) => host.note_answered(answered),
                Err(_) => host.not_opened(index, &name, &ReadFailure::NoMemory),
            }
        }

        fn collect(host: &mut Host<Self>) {
            let loader = Arc::clone(&host.app.loader);
            loader.collect_landed(|answer| {
                if let Some(reply) = answer {
                    adopt_load(host, reply);
                }
            });
            forget_owed(host);
            let renderer = Arc::clone(&host.app.renderer);
            renderer.collect_landed(|answer| {
                if let Some((stamp, drawn)) = answer {
                    adopt_render(host, stamp, drawn);
                }
            });
            collect_settings(host);
        }

        fn leaving(host: &mut Host<Self>) {
            let draw = &mut host.app;
            let mut refusals = Vec::new();
            tairix_appdata::see_out_published(&*draw.writer, &mut draw.preferences, &mut refusals);
            for refusal in refusals {
                app::report(APP_NAME, refusal);
            }
        }

        fn bar_chosen(host: &mut Host<Self>, _row: usize) {
            let settings = host
                .app_windows
                .iter()
                .position(|window| matches!(window.view, DrawWindow::Settings(_)));
            if let Some(index) = settings {
                host.raise_app_window(index);
                return;
            }
            let record = host.app.preferences.live().clone();
            let settings = DrawSettings::new(record, Category::General.index());
            let _ = host.open_app_window(DrawWindow::Settings(settings), None);
        }

        /// What the settings window was in the middle of is written as it
        /// goes; a PDF's question closed unanswered turns the document down.
        fn app_closing(host: &mut Host<Self>, index: usize) {
            match &mut host.app_windows[index].view {
                DrawWindow::Settings(_) => {
                    let job = host.app.preferences.settle();
                    submit_settings(host, job);
                }
                DrawWindow::OpenPdf(question) => {
                    if let Some(ticket) = question.take_ticket() {
                        turned_down(host, &ticket);
                    }
                }
            }
        }

        fn app_request(host: &mut Host<Self>, index: usize, request: DrawWindowAsk<Ticket>) {
            match request {
                DrawWindowAsk::Settings(request) => settings_asked(host, request),
                DrawWindowAsk::OpenPdf(Answered::Size(page)) => size_asked(host, index, page),
                DrawWindowAsk::OpenPdf(Answered::Open { ticket, given }) => {
                    host.close_app_window(index);
                    open_answered(host, &ticket, given);
                }
                DrawWindowAsk::OpenPdf(Answered::Cancelled(ticket)) => {
                    host.close_app_window(index);
                    turned_down(host, &ticket);
                }
            }
        }

        /// Step what each window's held controls have brought due, then ask
        /// for the tiles each window lacks, whatever moved it.
        fn turn(host: &mut Host<Self>, now_ns: u64) -> bool {
            let (theme, scale) = (host.themes.active(), host.desktop.scale());
            for window in &mut host.windows {
                if window.loading() || window.view.deadline_ns().is_none_or(|due| due > now_ns) {
                    continue;
                }
                window.view.tick(now_ns, scale, theme, &mut window.damage);
                window.owe_reported();
            }
            for index in 0..host.windows.len() {
                let window = &mut host.windows[index];
                if window.loading() {
                    continue;
                }
                if let Some(job) = window.view.wanted_render() {
                    submit_render(host, index, job);
                }
            }
            false
        }

        /// When the soonest held control of any window next steps.
        fn deadline(host: &mut Host<Self>, now_ns: u64) -> Option<u64> {
            let (theme, scale) = (host.themes.active(), host.desktop.scale());
            host.windows
                .iter_mut()
                .filter(|window| !window.loading())
                .filter_map(|window| {
                    window.view.arm_deadline(now_ns, scale, theme);
                    window.view.deadline_ns()
                })
                .min()
        }

        fn pressure(host: &mut Host<Self>) {
            tairix_font::trim_glyph_cache();
            let band = tairix_rt::pressure::gauge().band();
            for window in &mut host.windows {
                window.view.adopt_pressure(band, &mut window.damage);
                window.owe_reported();
            }
        }

        fn request(host: &mut Host<Self>, index: usize, request: Own) {
            match request {
                Own::Copy(drawing) => {
                    if !host.queue(index, Work::Clip(drawing)) {
                        host.windows[index].state("There is no room to copy now");
                    }
                }
                Own::Paste => host.paste(index),
                Own::NewWindow => host.new_window(),
                Own::Compute(compute) => {
                    if !host.queue(index, Work::Compute(*compute)) {
                        host.windows[index].state("There is no room for that work now");
                    }
                }
            }
        }

        fn pasted(
            host: &mut Host<Self>,
            index: usize,
            contents: Result<Option<(ClipboardKind, Vec<u8>)>, Errno>,
        ) {
            let bytes = match contents {
                Ok(Some((ClipboardKind::Text, bytes))) => bytes,
                Ok(_) => {
                    host.windows[index].state("The clipboard holds no drawing");
                    return;
                }
                Err(err) => {
                    host.windows[index].state(alloc::format!("Could not paste: {err}"));
                    return;
                }
            };
            let job = LoadJob {
                stamp: host.windows[index].stamp(),
                work: LoadWork::Paste(bytes),
            };
            match host.app.loader.submit(Some(job)) {
                Ok(answered) => host.note_answered(answered),
                Err(_) => host.windows[index].state("There is no room to paste now"),
            }
        }

        fn answered(host: &mut Host<Self>, index: usize, answer: Answer) {
            let said = match answer {
                Answer::Computed(computed) => {
                    let window = &mut host.windows[index];
                    window.view.computed(*computed, &mut window.damage);
                    None
                }
                Answer::Copied(Ok(message)) => Some(message),
                Answer::Copied(Err(why)) => Some(alloc::format!("The copy was not saved: {why}")),
                Answer::Clipped(clipped) => {
                    let id = host.windows[index].id();
                    let put = match clipped {
                        Ok(text) => host
                            .client
                            .set_clipboard(id, ClipboardKind::Text, text.as_bytes())
                            .map_err(|err| alloc::format!("{err}")),
                        Err(why) => Err(String::from(why)),
                    };
                    put.err().map(|why| alloc::format!("Could not copy: {why}"))
                }
            };
            let window = &mut host.windows[index];
            if let Some(said) = said {
                window.state(said);
            }
            window.owe_reported();
        }

        fn orphaned(host: &mut Host<Self>, _window: u64, answer: Answer) {
            if let Answer::Copied(Err(why)) = answer {
                host.notify(alloc::format!("The copy was not saved: {why}"));
            }
        }

        fn copy_chosen(host: &mut Host<Self>, index: usize, file: Arc<Handle>, name: String) {
            let window = &mut host.windows[index];
            let drawing = window.view.copied(&mut window.damage);
            window.owe_reported();
            if !host.queue(
                index,
                Work::Copy {
                    target: file,
                    name,
                    drawing,
                },
            ) {
                host.windows[index].state("There is no room to save a copy now");
            }
        }

        fn import_chosen(
            host: &mut Host<Self>,
            index: usize,
            file: Handle,
            name: String,
            at: Option<tairix_geometry::Point>,
        ) {
            let job = LoadJob {
                stamp: host.windows[index].stamp(),
                work: LoadWork::Place {
                    handle: file,
                    name,
                    at,
                },
            };
            match host.app.loader.submit(Some(job)) {
                Ok(answered) => host.note_answered(answered),
                Err(_) => host.windows[index].state("There is no room to place a picture now"),
            }
        }

        fn render_app(
            &mut self,
            surface: &mut Surface,
            view: &DrawWindow<Ticket>,
            (theme, scale, _): (&Theme, Scale, Faces),
            _focused: bool,
        ) {
            match view {
                DrawWindow::Settings(view) => {
                    let mut inline = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
                    let mut source = IconArtworkSource::new(&mut self.artwork, &mut inline);
                    view.render(surface, (theme, scale), &mut source);
                }
                DrawWindow::OpenPdf(view) => view.render(surface, scale, theme),
            }
        }

        fn render(
            &mut self,
            surface: &mut Surface,
            view: &View,
            style: (&Theme, Scale, Faces),
            _focused: bool,
        ) {
            let mut inline = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
            let mut source = IconArtworkSource::new(&mut self.artwork, &mut inline);
            view.render(surface, style, &mut source);
        }
    }

    /// Carry out what the settings window changed.
    fn settings_asked(host: &mut Host<Draw>, request: SettingsRequest<Preferences>) {
        if let Some(Edit { was, now, settled }) = request.edit {
            host.app.preferences.edit(&was, &now);
            apply_settings(host);
            if settled {
                let job = host.app.preferences.settle();
                submit_settings(host, job);
            }
        }
        match request.then {
            Some(Then::Restore) => {
                let job = host.app.preferences.restore();
                submit_settings(host, job);
            }
            Some(Then::Ask(never)) => match never {},
            None => {}
        }
    }

    /// Open the PDF `ticket` is for as its question was answered, with
    /// `given`: the loader keeps it, so nothing is read or handed over again.
    fn open_answered(host: &mut Host<Draw>, ticket: &Ticket, given: Given) {
        let Some(index) = host.showing(ticket.stamp) else {
            forget(host, ticket.stamp);
            return;
        };
        let job = LoadJob {
            stamp: ticket.stamp,
            work: LoadWork::Answer {
                name: ticket.name.clone(),
                given,
            },
        };
        if let Ok(answered) = host.app.loader.submit(Some(job)) {
            host.note_answered(answered);
            return;
        }
        forget(host, ticket.stamp);
        host.not_opened(index, &ticket.name, &ReadFailure::NoMemory);
    }

    /// The PDF `ticket` is for was turned down: the loader lets it go, and
    /// its window is given up.
    fn turned_down(host: &mut Host<Draw>, ticket: &Ticket) {
        forget(host, ticket.stamp);
        if let Some(index) = host.showing(ticket.stamp) {
            host.not_wanted(index);
        }
    }

    /// The question in app window `index` wants the size of page `page`,
    /// counting from 0.
    fn size_asked(host: &mut Host<Draw>, index: usize, page: u32) {
        let Some(stamp) = host.app_windows[index]
            .view
            .question()
            .and_then(OpenPdf::ticket)
            .map(|ticket| ticket.stamp)
        else {
            return;
        };
        let job = LoadJob {
            stamp,
            work: LoadWork::PageSize(page),
        };
        match host.app.loader.submit(Some(job)) {
            Ok(answered) => host.note_answered(answered),
            Err(_) => sized(host, stamp, page, None),
        }
    }

    /// Page `page`'s size reached the question `stamp`'s window asks, or
    /// could not be had.
    fn sized(host: &mut Host<Draw>, stamp: Stamp, page: u32, size: Option<(f64, f64)>) {
        let asking = host.app_windows.iter().position(|window| {
            window
                .view
                .question()
                .and_then(OpenPdf::ticket)
                .is_some_and(|ticket| ticket.stamp == stamp)
        });
        let Some(index) = asking else {
            return;
        };
        let window = &mut host.app_windows[index];
        let outcome = window.view.sized(page, size, &mut window.damage);
        host.apply_app(index, outcome);
    }

    /// Hand `job`, the settings write owed now, to the writer, and take in
    /// its answer where it ran here for want of a worker.
    fn submit_settings(host: &mut Host<Draw>, job: Option<PublishJob<Preferences>>) {
        let mut refusals = Vec::new();
        let draw = &mut host.app;
        if tairix_appdata::submit_published(
            &*draw.writer,
            &mut draw.preferences,
            job,
            &mut refusals,
        ) {
            settings_adopted(host, &refusals);
        }
    }

    /// Adopt what the settings writer answered and make the write owed behind
    /// it.
    fn collect_settings(host: &mut Host<Draw>) {
        let mut refusals = Vec::new();
        let draw = &mut host.app;
        if tairix_appdata::collect_published(&*draw.writer, &mut draw.preferences, &mut refusals) {
            settings_adopted(host, &refusals);
        }
    }

    /// Say what the store refused, in the settings window and on `stderr`,
    /// and bring every window up to the settings now in force.
    fn settings_adopted(host: &mut Host<Draw>, refusals: &[StoreRefusal]) {
        let said = refusals
            .iter()
            .find(|refusal| refusal.is_write())
            .map(|refusal| alloc::format!("{refusal}"));
        for refusal in refusals {
            app::report(APP_NAME, refusal);
        }
        apply_settings(host);
        for window in &mut host.app_windows {
            if let DrawWindow::Settings(view) = &mut window.view {
                view.say(said.clone(), &mut window.damage);
                window.owe_reported();
            }
        }
    }

    /// Bring every window up to the settings in force: each drawing window
    /// moves things as they say, and the settings window shows them.
    fn apply_settings(host: &mut Host<Draw>) {
        let preferences = host.app.preferences.live().clone();
        for window in &mut host.windows {
            window.view.adopt(&preferences);
        }
        for index in 0..host.app_windows.len() {
            let window = &mut host.app_windows[index];
            let DrawWindow::Settings(view) = &mut window.view else {
                continue;
            };
            if view.adopt(&preferences, &mut window.damage) {
                host.apply_app(index, Outcome::relaid());
            } else {
                window.owe_reported();
            }
        }
    }

    fn main() -> i32 {
        // A document is untrusted input: it is read by a capability-empty
        // child this binary is re-entered as, which never becomes the editor.
        if worker_role() {
            return serve_stdio(&mut DrawEditService::default()).exit_code();
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }
        docapp::run::<Draw>()
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
