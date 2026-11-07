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
    use tairix_abi::window_ipc::ClipboardKind;
    use tairix_abi::Errno;
    use tairix_bezier::Affine;
    use tairix_controls::damage;
    use tairix_draw::compute::{work_out, Compute, Computed};
    use tairix_draw::document::{blank, Document};
    use tairix_draw::glyphs::{Fetched, Lettering};
    use tairix_draw::layout::{Faces, Layout, WINDOW_SIZE};
    use tairix_draw::tiles::{RenderJob, TileKey, TILE};
    use tairix_draw::view::{Own, Snapshot, View};
    use tairix_draw::window::CopyFormat;
    use tairix_drawing::render::{artwork, Afresh, Mode, View as Square};
    use tairix_drawing::svg::Read;
    use tairix_drawing::unkept::Unkept;
    use tairix_drawing::{Drawing, DrawingError, Ids};
    use tairix_geometry::{Region, Scale};
    use tairix_icon::{
        artwork_cache, ArtworkCache, IconArtworkSource, InlineArtwork, NoArtworkSeam,
    };
    use tairix_image::{encode_png, Picture};
    use tairix_parallel::{JobRunner, Pool};
    use tairix_raster::Surface;
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_sandbox::drawedit::{read_drawing, DrawEditService, DrawFormat};
    use tairix_sandbox::imagerender::{send_document, upload_document, MAX_DOCUMENT_BYTES};
    use tairix_sandbox::rt::{serve_stdio, worker_role, RtLauncher};
    use tairix_sandbox::ParserSandbox;
    use tairix_theme::Theme;
    use tairix_util::defer::JobQueue;
    use tairix_util::mathf::{fmax, fmin, round_i32};
    use tairix_window::app;
    use tairix_window::docapp::{
        self, AnswerWake, DocumentApp, Handle, Host, NoAppView, Stamp, APP_TOKEN,
    };
    use tairix_window::document::{Access, ReadFailure, SaveJob, SavedDocument, UNTITLED};
    use tairix_window::{clipboard, Desktop};

    const APP_NAME: &str = "Draw";

    /// The renderer's wake.
    const RENDER_TOKEN: u64 = APP_TOKEN + 1;

    /// The most pixels a PNG copy's longer side is rendered at: a page larger
    /// than this is rendered scaled down to fit.
    const MOST_PNG_SIDE: f64 = 8192.0;

    type Reader = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    /// What reading a document came to.
    #[derive(Debug)]
    enum ReadRefusal {
        Read(ReadFailure),
        TooLong,
        Unreadable(String),
    }

    impl fmt::Display for ReadRefusal {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Read(failure) => write!(f, "{failure}"),
                Self::TooLong => write!(
                    f,
                    "it is longer than the {} MiB a drawing file may be",
                    MAX_DOCUMENT_BYTES >> 20
                ),
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
    }

    type LoadDesk = JobQueue<Option<LoadJob>, Option<LoadReply>>;
    type LoadWorker = Worker<Reader, Option<LoadJob>, Option<LoadReply>, LoadDesk>;

    fn document_length(file: &Handle) -> Result<usize, ReadRefusal> {
        let length = file
            .regular_len()
            .map_err(|err| ReadRefusal::Read(ReadFailure::Unreadable(err)))?
            .ok_or(ReadRefusal::Read(ReadFailure::NotAFile))?;
        usize::try_from(length)
            .ok()
            .filter(|&length| length <= MAX_DOCUMENT_BYTES)
            .ok_or(ReadRefusal::TooLong)
    }

    fn open(sandbox: &mut Reader, handle: &Handle) -> Result<(Read, Ids), ReadRefusal> {
        let fd = handle.fd();
        let length = document_length(handle)?;
        let uploaded = upload_document(sandbox, length, |offset, into| {
            tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
        })
        .map_err(|_| ReadRefusal::Read(ReadFailure::NoMemory));
        let mut ids = Ids::new();
        let read = uploaded.and_then(|()| {
            read_drawing(sandbox, DrawFormat::Svg, &mut ids)
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
                read_drawing(sandbox, DrawFormat::Svg, &mut Ids::new())
                    .map_err(|failure| alloc::format!("The clipboard holds no drawing: {failure}"))
            });
        sandbox.release();
        read.map(|read| read.drawing)
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
    }

    /// What the queue's work came to.
    enum Answer {
        Computed(Box<Computed>),
        Copied(Result<String, String>),
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

    /// The page of `drawing` rendered as a PNG at its own size, or scaled to
    /// fit [`MOST_PNG_SIDE`].
    fn png(drawing: &Drawing) -> Result<Vec<u8>, String> {
        let size = drawing.page().size();
        let fit = MOST_PNG_SIDE / fmax(fmax(size.x, size.y), 1.0);
        let scale = fmin(fit, 1.0);
        let pixels_of =
            |length: f64| u32::try_from(round_i32(fmax(length * scale, 1.0))).unwrap_or(1);
        let (width, height) = (pixels_of(size.x), pixels_of(size.y));
        let glyphs = Lettering::new(Fetched);
        let mut rgba = tairix_util::fallible::zeroed(
            usize::try_from(u64::from(width) * u64::from(height) * 4)
                .map_err(|_| String::from("the page is too large"))?,
        )
        .ok_or_else(|| String::from("there is not enough memory to render the page"))?;
        let mut tile =
            Surface::new(TILE, TILE).ok_or_else(|| String::from("there is not enough memory"))?;
        let mut row = 0;
        while row < height {
            let mut column = 0;
            while column < width {
                tile.fill(tairix_raster::Color::rgba(0, 0, 0, 0));
                let square = Square {
                    to_device: Affine::scale(scale, scale),
                    origin: tairix_bezier::Point::new(f64::from(column), f64::from(row)),
                    side: f64::from(TILE),
                    design: TILE * 256,
                    mode: tairix_drawing::render::Mode::Painted,
                };
                let art = artwork(drawing, &square, &glyphs, &Afresh)
                    .map_err(|err| alloc::format!("{err:?}"))?;
                if !tile.draw_artwork(&art.nodes, square.design) {
                    return Err(String::from(
                        "there is not enough memory to render the page",
                    ));
                }
                let pitch = usize::try_from(TILE).unwrap_or(0);
                let across = usize::try_from(TILE.min(width - column)).unwrap_or(0);
                let down = usize::try_from(TILE.min(height - row)).unwrap_or(0);
                for (y, line) in tile.pixels().chunks_exact(pitch).take(down).enumerate() {
                    let first = u64::from(row) + u64::try_from(y).unwrap_or(0);
                    let start = usize::try_from((first * u64::from(width) + u64::from(column)) * 4)
                        .unwrap_or(usize::MAX);
                    let Some(out) = rgba.get_mut(start..start.saturating_add(across * 4)) else {
                        continue;
                    };
                    let (quads, _) = out.as_chunks_mut::<4>();
                    for (pixel, rgba) in line.iter().zip(quads) {
                        let pixel = pixel.unpremultiply();
                        rgba.copy_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
                    }
                }
                column += TILE;
            }
            row += TILE;
        }
        let picture = Picture::rgba(width, height, rgba).map_err(|err| alloc::format!("{err}"))?;
        encode_png(&picture).map_err(|err| alloc::format!("{err}"))
    }

    /// Write a copy of `drawing` to `target`, in the format its name says.
    fn copy(target: &Handle, name: &str, drawing: &Drawing) -> Result<String, String> {
        let format = CopyFormat::of(name)
            .ok_or_else(|| String::from("name the copy with .pdf, .png or .svg"))?;
        let bytes = match format {
            CopyFormat::Svg => tairix_drawing::svg::write(drawing)
                .map(String::into_bytes)
                .map_err(|err| alloc::format!("{err:?}"))?,
            CopyFormat::Pdf => {
                let written = tairix_drawing::pdf::pdf(drawing, &Lettering::new(Fetched))
                    .map_err(|err| alloc::format!("{err:?}"))?;
                written.bytes
            }
            CopyFormat::Png => png(drawing)?,
        };
        write_all(target, &bytes)
            .map_err(|err| alloc::format!("it could not be written ({err})"))?;
        Ok(alloc::format!("Saved a copy as {name}"))
    }

    struct Draw {
        loader: Arc<LoadWorker>,
        _loader_guard: WorkerGuard<Reader, Option<LoadJob>, Option<LoadReply>, LoadDesk>,
        renderer: Arc<RenderWorker>,
        _renderer_guard: WorkerGuard<Pool, RenderAsk, RenderAnswer, RenderDesk>,
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
        let mut ids = Ids::new();
        let drawing = blank(&mut ids).map_err(|_| String::from("there is not enough memory"))?;
        let document = Document::new(drawing, ids, true);
        Ok(View::new(
            document,
            String::from(name),
            access,
            Unkept::NONE,
            scale_of(host),
        ))
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
                    let view = View::new(document, name, access, read.unkept, scale_of(host));
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
        type AppView = NoAppView<Faces>;
        type Snapshot = Snapshot;
        type Extra = ();
        type Work = Work;
        type Answer = Answer;
        type Failure = SaveFailure;

        const NAME: &'static str = APP_NAME;
        const WINDOW_SIZE: (u32, u32) = WINDOW_SIZE;
        const JOBS_PER_WINDOW: usize = 4;

        fn start(desktop: &Desktop, set: u64) -> Result<Self, i32> {
            let Ok(loader) = LoadWorker::queued(
                serve_load,
                ParserSandbox::new(
                    RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Documents),
                    tairix_rt::LogSink,
                ),
                WorkerWake::create(),
                0,
            ) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the reading queue",
                ));
            };
            let loader = Arc::new(loader);
            app::start_worker(APP_NAME, &loader, set, APP_TOKEN, "read");
            let loader_guard = WorkerGuard::new(&loader);
            let online = tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport);
            let pool = app::pool_across(APP_NAME, online, "rendering drawings");
            let Ok(renderer) = RenderWorker::queued(serve_render, pool, WorkerWake::create(), 0)
            else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the render queue",
                ));
            };
            let renderer = Arc::new(renderer);
            app::start_worker(APP_NAME, &renderer, set, RENDER_TOKEN, "render");
            let renderer_guard = WorkerGuard::new(&renderer);
            Ok(Self {
                loader,
                _loader_guard: loader_guard,
                renderer,
                _renderer_guard: renderer_guard,
                artwork: icon_cache(desktop),
            })
        }

        fn wakes(&self) -> Vec<(u64, Arc<dyn AnswerWake>)> {
            let loader: Arc<dyn AnswerWake> = self.loader.clone();
            let renderer: Arc<dyn AnswerWake> = self.renderer.clone();
            alloc::vec![(APP_TOKEN, loader), (RENDER_TOKEN, renderer)]
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
            Ok(None)
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
                Own::Copy(text) => {
                    let id = host.windows[index].id();
                    if let Err(err) =
                        clipboard::put(&mut host.client, id, ClipboardKind::Text, text.as_bytes())
                    {
                        host.windows[index].state(alloc::format!("Could not copy: {err}"));
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
            let window = &mut host.windows[index];
            match answer {
                Answer::Computed(computed) => {
                    window
                        .view
                        .computed(*computed, &window.layout, &mut window.damage);
                }
                Answer::Copied(Ok(message)) => window.state(message),
                Answer::Copied(Err(why)) => {
                    window.state(alloc::format!("The copy was not saved: {why}"));
                }
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

        fn render_app(
            &mut self,
            _: &mut Surface,
            view: &NoAppView<Faces>,
            (): &(),
            _: (&Theme, Scale, Faces),
            _: bool,
        ) {
            let () = view.never();
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
