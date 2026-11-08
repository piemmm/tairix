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
    use tairix_draw::pdfopen::{Answered, Asked, OpenPdf};
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
        read_drawing, read_pages, read_placed, Access as ReadAccess, DrawEditService, DrawFailure,
        DrawRefusal, WORKER_MEMORY,
    };
    use tairix_sandbox::imagerender::{send_document, upload_document, UploadFailure};
    use tairix_sandbox::rt::{document_room, serve_stdio, worker_role, RtLauncher};
    use tairix_sandbox::ParserSandbox;
    use tairix_theme::Theme;
    use tairix_util::defer::JobQueue;
    use tairix_util::secret::WipedBuf;
    use tairix_window::app;
    use tairix_window::docapp::settings::{Edit, SettingsRequest, Then};
    use tairix_window::docapp::{
        self, AnswerWake, DocumentApp, Handle, Host, Outcome, Stamp, APP_TOKEN,
    };
    use tairix_window::document::{Access, ReadFailure, SaveJob, SavedDocument, UNTITLED};
    use tairix_window::{clipboard, Desktop};

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
        /// Open a document: a PDF at `page` where one was chosen, and with
        /// `password` where one was given.
        Open {
            handle: Arc<Handle>,
            name: String,
            page: Option<u32>,
            password: Option<WipedBuf>,
        },
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
        /// A PDF that asks something before it opens: which page, or its
        /// password, `password` being the one already given.
        Asks {
            handle: Arc<Handle>,
            name: String,
            asked: Asked,
            password: Option<WipedBuf>,
        },
        Pasted(Result<Drawing, String>),
        Placed {
            name: String,
            at: Option<tairix_geometry::Point>,
            placed: Result<Image, String>,
        },
    }

    type LoadDesk = JobQueue<Option<LoadJob>, Option<LoadReply>>;
    type LoadWorker = Worker<Reader, Option<LoadJob>, Option<LoadReply>, LoadDesk>;

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

    /// What opening a document came to: its drawing, or a question a PDF
    /// asks first.
    enum Opened {
        Read(Result<(Read, Ids), ReadRefusal>),
        Asks(Asked),
    }

    /// What a read that failed answers: a PDF's password asked for again
    /// where it wanted one, and why the document is unreadable otherwise.
    fn failed(failure: DrawFailure) -> Opened {
        match failure {
            DrawFailure::Refused(DrawRefusal::NeedsPassword) => {
                Opened::Asks(Asked::Password { wrong: false })
            }
            DrawFailure::Refused(DrawRefusal::WrongPassword) => {
                Opened::Asks(Asked::Password { wrong: true })
            }
            failure => Opened::Read(Err(ReadRefusal::Unreadable(alloc::format!("{failure}")))),
        }
    }

    /// Read `handle`'s document: a PDF of several pages asks which first,
    /// unless `page` already says, and one protected by a password asks for
    /// it unless `password` opens it.
    fn open(
        sandbox: &mut Reader,
        handle: &Handle,
        page: Option<u32>,
        password: Option<&[u8]>,
    ) -> Opened {
        let fd = handle.fd();
        let length = match document_length(handle, sandbox.document_room()) {
            Ok(length) => length,
            Err(refusal) => return Opened::Read(Err(refusal)),
        };
        let uploaded = upload_document(sandbox, length, |offset, into| {
            tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
        })
        .map_err(ReadRefusal::Upload);
        let access = ReadAccess {
            page: page.unwrap_or(0),
            password,
        };
        let opened = uploaded.map_or_else(
            |refusal| Opened::Read(Err(refusal)),
            |()| read_uploaded(sandbox, length, page.is_none(), &access),
        );
        // The worker that read this document reads no other.
        sandbox.release();
        opened
    }

    /// The document uploaded to `sandbox`, `length` bytes long, read with
    /// `access`; its pages counted first when `choosing`.
    fn read_uploaded(
        sandbox: &mut Reader,
        length: usize,
        choosing: bool,
        access: &ReadAccess<'_>,
    ) -> Opened {
        let handed = length as u64;
        if choosing {
            match read_pages(sandbox, handed, access) {
                Ok(pages) if pages.count > 1 => {
                    return Opened::Asks(Asked::Page {
                        count: pages.count,
                        sizes: pages.sizes,
                    });
                }
                Ok(_) => {}
                Err(failure) => return failed(failure),
            }
        }
        let mut ids = Ids::new();
        match read_drawing(sandbox, handed, access, &mut ids) {
            Ok(read) => Opened::Read(Ok((read, ids))),
            Err(failure) => failed(failure),
        }
    }

    fn paste(sandbox: &mut Reader, bytes: &[u8]) -> Result<Drawing, String> {
        let read = send_document(sandbox, bytes)
            .map_err(|failure| {
                alloc::format!("The clipboard's drawing could not be handed over: {failure}")
            })
            .and_then(|()| {
                read_drawing(
                    sandbox,
                    bytes.len() as u64,
                    &ReadAccess::default(),
                    &mut Ids::new(),
                )
                .map_err(|failure| alloc::format!("The clipboard holds no drawing: {failure}"))
            });
        sandbox.release();
        read.map(|read| read.drawing)
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

    fn serve_load(sandbox: &mut Reader, job: &mut Option<LoadJob>) -> Option<LoadReply> {
        let LoadJob { stamp, work } = job.take()?;
        let answer = match work {
            LoadWork::Open {
                handle,
                name,
                page,
                password,
            } => match open(sandbox, &handle, page, password.as_deref()) {
                Opened::Read(read) => LoadAnswer::Opened {
                    read,
                    recorded: tairix_browse::recorded_filetype_of(&handle),
                    handle,
                    name,
                },
                Opened::Asks(asked) => LoadAnswer::Asks {
                    handle,
                    name,
                    // A password asked again is not kept.
                    password: password.filter(|_| matches!(asked, Asked::Page { .. })),
                    asked,
                },
            },
            LoadWork::Paste(bytes) => LoadAnswer::Pasted(paste(sandbox, &bytes)),
            LoadWork::Place { handle, name, at } => LoadAnswer::Placed {
                placed: place(sandbox, &handle),
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
        _loader_guard: WorkerGuard<Reader, Option<LoadJob>, Option<LoadReply>, LoadDesk>,
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

    /// What a PDF's question is asked for: the window loading it, its file,
    /// and the password already given.
    struct Ticket {
        stamp: Stamp,
        handle: Arc<Handle>,
        name: String,
        password: Option<WipedBuf>,
    }

    fn adopt_load(host: &mut Host<Draw>, reply: LoadReply) {
        let Some(index) = host.showing(reply.stamp) else {
            return;
        };
        match reply.answer {
            LoadAnswer::Asks {
                handle,
                name,
                asked,
                password,
            } => {
                let ticket = Ticket {
                    stamp: reply.stamp,
                    handle,
                    name: name.clone(),
                    password,
                };
                let question = DrawWindow::OpenPdf(OpenPdf::new(&name, asked, ticket));
                if host.open_app_window(question, Some(index)).is_none() {
                    host.not_opened(index, &name, &"there is no room to ask how to open it");
                }
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
                ParserSandbox::new(RtLauncher::own_binary(WORKER_MEMORY), tairix_rt::LogSink),
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

        fn withdraw(&mut self, window: u64) {
            self.loader
                .retain_waiting(|job| job.as_ref().is_none_or(|job| job.stamp.window() != window));
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
                    page: None,
                    password: None,
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
                DrawWindowAsk::OpenPdf(answered) => {
                    host.close_app_window(index);
                    match answered {
                        Answered::Open {
                            ticket,
                            page,
                            password,
                        } => open_answered(host, ticket, page, password),
                        Answered::Cancelled(ticket) => turned_down(host, &ticket),
                    }
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
                window.view.adopt_pressure(band);
            }
        }

        fn request(host: &mut Host<Self>, index: usize, request: Own) {
            match request {
                Own::Copy(drawing) => {
                    if !host.queue(index, Work::Clip(drawing)) {
                        host.windows[index].state("There is no room to copy now");
                    }
                }
                Own::Paste => {
                    let id = host.windows[index].id();
                    let taken = clipboard::take(&mut host.client, id);
                    let bytes = match taken {
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
                Own::NewWindow => host.new_window(),
                Own::Compute(compute) => {
                    if !host.queue(index, Work::Compute(*compute)) {
                        host.windows[index].state("There is no room for that work now");
                    }
                }
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
                        Ok(text) => clipboard::put(
                            &mut host.client,
                            id,
                            ClipboardKind::Text,
                            text.as_bytes(),
                        )
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

    /// Open the PDF `ticket` is for as its question was answered: at `page`
    /// where one was chosen, with `password` where one was given, else the
    /// one given before.
    fn open_answered(
        host: &mut Host<Draw>,
        ticket: Ticket,
        page: Option<u32>,
        password: Option<WipedBuf>,
    ) {
        let Some(index) = host.showing(ticket.stamp) else {
            return;
        };
        let name = ticket.name.clone();
        let job = LoadJob {
            stamp: ticket.stamp,
            work: LoadWork::Open {
                handle: ticket.handle,
                name: ticket.name,
                page,
                password: password.or(ticket.password),
            },
        };
        match host.app.loader.submit(Some(job)) {
            Ok(answered) => host.note_answered(answered),
            Err(_) => host.not_opened(index, &name, &ReadFailure::NoMemory),
        }
    }

    /// The PDF `ticket` is for was turned down: its window is given up.
    fn turned_down(host: &mut Host<Draw>, ticket: &Ticket) {
        if let Some(index) = host.showing(ticket.stamp) {
            host.not_wanted(index);
        }
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
