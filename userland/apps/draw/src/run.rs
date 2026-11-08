//! Draw's `Run` binary: the document host's windows over the Draw engine, the
//! workers that keep reading, rendering and writing off the event loop, and
//! the sandbox worker this binary is re-entered as to read documents.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::fmt;

    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::window_ipc::{AppMenuSymbol, ClipboardKind, CLIPBOARD_MAX_BYTES};
    use tairix_abi::Errno;
    use tairix_appdata::{
        Opening, Publication, PublishAnswer, PublishJob, PublishWorker, Refusal as StoreRefusal,
        RtHost, Settings,
    };
    use tairix_bezier::{Affine, Bounds, Point};
    use tairix_collections::try_arc;
    use tairix_controls::{damage, SettingsLayout};
    use tairix_draw::compute::{work_out, Compute, Computed};
    use tairix_draw::copy::CopyFormat;
    use tairix_draw::document::{blank, Document};
    use tairix_draw::glyphs::{Fetched, Lettering};
    use tairix_draw::layout::{Faces, Layout, WINDOW_SIZE};
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
    use tairix_sandbox::drawedit::{read_drawing, read_placed, DrawEditService, WORKER_MEMORY};
    use tairix_sandbox::imagerender::{send_document, upload_document, UploadFailure};
    use tairix_sandbox::rt::{document_room, serve_stdio, worker_role, RtLauncher};
    use tairix_sandbox::ParserSandbox;
    use tairix_theme::Theme;
    use tairix_util::defer::JobQueue;
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
        Open { handle: Arc<Handle>, name: String },
        Paste(Vec<u8>),
        Place { handle: Handle, name: String },
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
        },
        Pasted(Result<Drawing, String>),
        Placed {
            name: String,
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

    fn open(sandbox: &mut Reader, handle: &Handle) -> Result<(Read, Ids), ReadRefusal> {
        let fd = handle.fd();
        let length = document_length(handle, sandbox.document_room())?;
        let uploaded = upload_document(sandbox, length, |offset, into| {
            tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
        })
        .map_err(ReadRefusal::Upload);
        let mut ids = Ids::new();
        let read = uploaded.and_then(|()| {
            read_drawing(sandbox, length as u64, &mut ids)
                .map_err(|failure| ReadRefusal::Unreadable(alloc::format!("{failure}")))
        });
        // The worker that read this document reads no other.
        sandbox.release();
        read.map(|read| (read, ids))
    }

    fn paste(sandbox: &mut Reader, bytes: &[u8]) -> Result<Drawing, String> {
        let read = send_document(sandbox, bytes)
            .map_err(|failure| {
                alloc::format!("The clipboard's drawing could not be handed over: {failure}")
            })
            .and_then(|()| {
                read_drawing(sandbox, bytes.len() as u64, &mut Ids::new())
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
            LoadWork::Open { handle, name } => LoadAnswer::Opened {
                read: open(sandbox, &handle),
                handle,
                name,
            },
            LoadWork::Paste(bytes) => LoadAnswer::Pasted(paste(sandbox, &bytes)),
            LoadWork::Place { handle, name } => LoadAnswer::Placed {
                placed: place(sandbox, &handle),
                name,
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
        Encode(DrawingError),
        Write(Errno),
    }

    impl fmt::Display for SaveFailure {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Encode(DrawingError::OutOfMemory) => {
                    f.write_str("there is not enough memory to write it")
                }
                Self::Encode(_) => f.write_str("it could not be written as SVG"),
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
        let saved = match copied.note {
            Some(note) => alloc::format!("Saved a copy as {name}, {note}"),
            None => alloc::format!("Saved a copy as {name}"),
        };
        let reopened = matches!(format, CopyFormat::Svg | CopyFormat::Draw);
        Ok(
            if reopened && copied.bytes.len() as u64 > document_room(WORKER_MEMORY) {
                alloc::format!("{saved}. {PAST_OPENING}")
            } else {
                saved
            },
        )
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

    fn adopt_load(host: &mut Host<Draw>, reply: LoadReply) {
        let Some(index) = host.showing(reply.stamp) else {
            return;
        };
        match reply.answer {
            LoadAnswer::Opened { handle, name, read } => match read {
                Ok((read, ids)) => {
                    let access = host.windows[index].view.access();
                    let document = Document::new(read.drawing, ids, true);
                    let mut view = View::new(document, name, access, read.unkept, scale_of(host));
                    view.begin(host.app.preferences.live());
                    host.show(index, view, Some(handle));
                }
                Err(refusal) => host.not_opened(index, &name, &refusal),
            },
            LoadAnswer::Pasted(pasted) => {
                let window = &mut host.windows[index];
                window
                    .view
                    .pasted(pasted, &mut window.damage, &window.layout);
                window.owe_reported();
            }
            LoadAnswer::Placed { name, placed } => {
                let window = &mut host.windows[index];
                window
                    .view
                    .placed_picture(&name, placed, &mut window.damage, &window.layout);
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
        window.view.rendered(
            drawn.scene,
            (&drawn.keys, landed),
            &window.layout,
            &mut window.damage,
        );
        window.owe_reported();
    }

    impl DocumentApp for Draw {
        type View = View;
        type AppView = DrawSettings;
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
            _name: &str,
        ) -> Result<Option<&'static str>, SaveFailure> {
            let text =
                tairix_drawing::svg::write(&job.snapshot.drawing).map_err(SaveFailure::Encode)?;
            write_all(&job.target, text.as_bytes()).map_err(SaveFailure::Write)?;
            Ok((text.len() as u64 > document_room(WORKER_MEMORY)).then_some(PAST_OPENING))
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
            if let Some(index) = host.app_windows.len().checked_sub(1) {
                host.raise_app_window(index);
                return;
            }
            let record = host.app.preferences.live().clone();
            let _ = host.open_app_window(DrawSettings::new(record, Category::General.index()));
        }

        /// What the settings window was in the middle of is written as it
        /// goes.
        fn app_closing(host: &mut Host<Self>, _index: usize) {
            let job = host.app.preferences.settle();
            submit_settings(host, job);
        }

        fn app_request(
            host: &mut Host<Self>,
            _index: usize,
            request: SettingsRequest<Preferences>,
        ) {
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

        /// Step what each window's held controls have brought due, then ask
        /// for the tiles each window lacks, whatever moved it.
        fn turn(host: &mut Host<Self>, now_ns: u64) -> bool {
            let (theme, scale) = (host.themes.active(), host.desktop.scale());
            for window in &mut host.windows {
                if window.loading() || window.view.deadline_ns().is_none_or(|due| due > now_ns) {
                    continue;
                }
                let layout: Layout = window.layout;
                window
                    .view
                    .tick(now_ns, &layout, scale, theme, &mut window.damage);
                window.owe_reported();
            }
            for index in 0..host.windows.len() {
                let window = &mut host.windows[index];
                if window.loading() {
                    continue;
                }
                let layout: Layout = window.layout;
                if let Some(job) = window.view.wanted_render(&layout) {
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
                    let layout: Layout = window.layout;
                    window.view.arm_deadline(now_ns, &layout, scale, theme);
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
                    window
                        .view
                        .computed(*computed, &window.layout, &mut window.damage);
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

        fn import_chosen(host: &mut Host<Self>, index: usize, file: Handle, name: String) {
            let job = LoadJob {
                stamp: host.windows[index].stamp(),
                work: LoadWork::Place { handle: file, name },
            };
            match host.app.loader.submit(Some(job)) {
                Ok(answered) => host.note_answered(answered),
                Err(_) => host.windows[index].state("There is no room to place a picture now"),
            }
        }

        fn render_app(
            &mut self,
            surface: &mut Surface,
            view: &DrawSettings,
            layout: &SettingsLayout,
            (theme, scale, _): (&Theme, Scale, Faces),
            _focused: bool,
        ) {
            let mut inline = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
            let mut source = IconArtworkSource::new(&mut self.artwork, &mut inline);
            view.render(surface, layout, (theme, scale), &mut source);
        }

        fn render(
            &mut self,
            surface: &mut Surface,
            view: &View,
            layout: &Layout,
            style: (&Theme, Scale, Faces),
            _focused: bool,
        ) {
            let mut inline = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
            let mut source = IconArtworkSource::new(&mut self.artwork, &mut inline);
            view.render(surface, layout, style, &mut source);
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
            window
                .view
                .say(said.clone(), &window.layout, &mut window.damage);
            window.owe_reported();
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
            if window
                .view
                .adopt(&preferences, &window.layout, &mut window.damage)
            {
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
