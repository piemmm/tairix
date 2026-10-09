//! The `Paint.app` bundle's `Run` entry point: the desktop image editor.
//!
//! The manifest requests no filesystem capability. Every document is the
//! user's own act — a descriptor a launcher opened for this program, or a
//! grant the session's trusted picker delegated — and every save writes
//! through that descriptor. A document is untrusted input, and so is what the
//! clipboard holds, so both are decoded by a capability-empty worker this
//! binary is re-entered as; each document gets a fresh one, so a hostile file
//! can reach no other document's decode.
//!
//! The editor is the host-tested engine ([`tairix_paint`]), run in the shared
//! document host (`tairix_window::docapp`). What is the painter's own is here:
//! a decode worker reads documents and pastes through the sandbox, and the
//! host's queue encodes saves and copies and carries out fills and transforms
//! beside them, so nothing that grows with the picture waits on the loop.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::fmt;

    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::window_ipc::ClipboardKind;
    use tairix_abi::Errno;
    use tairix_appdata::{
        Publication, PublishJob, Published, Refusal as StoreRefusal, RtHost, Settings,
    };
    use tairix_controls::damage;
    use tairix_geometry::{Region, Scale};
    use tairix_icon::{
        artwork_cache, ArtworkCache, IconArtworkSource, InlineArtwork, NoArtworkSeam,
    };
    use tairix_image::{encode_png, EncodeError};
    use tairix_paint::canvas::{Canvas, Kind, OutOfMemory, Sample};
    use tairix_paint::document::{Document, Entry, Picture, Snapshot};
    use tairix_paint::layout::{Faces, Layout, WINDOW_SIZE};
    use tairix_paint::load::{Assembly, Refusal};
    use tairix_paint::preferences::Preferences;
    use tairix_paint::render::render_into;
    use tairix_paint::save::{
        encode, format_for, format_named, lost_in, write_back, SaveFormat, SaveRefusal,
    };
    use tairix_paint::selection::adapt_pasted;
    use tairix_paint::settings::{Category, SettingsLayout, SettingsRequest, SettingsWindow};
    use tairix_paint::view::{compute, Clip, Compute, Computed, Own, View};
    use tairix_raster::Surface;
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_sandbox::imageedit::{
        close_edit, open_edit, read_kept, read_rows, select_entry, EditDocument, EditEntry,
        EditFailure, EditKind,
    };
    use tairix_sandbox::imagerender::{
        upload_document, ImageRenderService, UploadFailure, ViewFormat, MAX_DOCUMENT_BYTES,
    };
    use tairix_sandbox::rt::{serve_stdio, worker_role, RtLauncher};
    use tairix_sandbox::ParserSandbox;
    use tairix_theme::Theme;
    use tairix_util::defer::JobQueue;
    use tairix_window::app;
    use tairix_window::docapp::{
        self, AnswerWake, DocWindow, DocumentApp, Handle, Host, Stamp, APP_TOKEN,
    };
    use tairix_window::document::{Access, ReadFailure, SaveJob, SavedDocument, UNTITLED};
    use tairix_window::{clipboard, Desktop};

    /// The name this program's bundle, help and refusals go by.
    const APP_NAME: &str = "Paint";

    type Decoder = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    /// Why a document could not be opened.
    #[derive(Copy, Clone, Debug)]
    enum LoadRefusal {
        Read(ReadFailure),
        TooLong,
        Undecodable(EditFailure),
    }

    impl fmt::Display for LoadRefusal {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Read(failure) => write!(f, "{failure}"),
                Self::TooLong => write!(
                    f,
                    "it is longer than the {} MiB a picture file may be",
                    MAX_DOCUMENT_BYTES >> 20
                ),
                Self::Undecodable(err) => write!(f, "{err}"),
            }
        }
    }

    impl From<ReadFailure> for LoadRefusal {
        fn from(failure: ReadFailure) -> Self {
            Self::Read(failure)
        }
    }

    impl From<EditFailure> for LoadRefusal {
        fn from(err: EditFailure) -> Self {
            Self::Undecodable(err)
        }
    }

    impl From<Refusal> for LoadRefusal {
        fn from(refusal: Refusal) -> Self {
            match refusal {
                Refusal::NoMemory => Self::Read(ReadFailure::NoMemory),
                Refusal::Unbelieved => Self::Undecodable(EditFailure::ReplyMalformed),
            }
        }
    }

    /// How long the regular file `file` measures, within what a worker takes.
    fn document_length(file: &Handle) -> Result<usize, LoadRefusal> {
        let length = file
            .regular_len()
            .map_err(ReadFailure::Unreadable)?
            .ok_or(ReadFailure::NotAFile)?;
        usize::try_from(length)
            .ok()
            .filter(|&length| length <= MAX_DOCUMENT_BYTES)
            .ok_or(LoadRefusal::TooLong)
    }

    /// The format to name in place of reading a document's signature: only
    /// a RISC OS sprite area, whose first word is a count, carries none.
    fn named_format(name: &str) -> Option<ViewFormat> {
        matches!(format_named(name), Ok(Some(SaveFormat::Sprites))).then_some(ViewFormat::Sprite)
    }

    /// A document being read into a window: the file, and what it is called.
    struct Load {
        handle: Arc<Handle>,
        name: String,
    }

    /// Hand `length` bytes to the worker, each run read by `read_at`.
    fn upload(
        sandbox: &mut Decoder,
        length: usize,
        read_at: impl FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
    ) -> Result<(), LoadRefusal> {
        upload_document(sandbox, length, read_at).map_err(|failure| match failure {
            UploadFailure::Read(err) => ReadFailure::Unreadable(err).into(),
            UploadFailure::Shrank => ReadFailure::Unreadable(Errno::OutOfRange).into(),
            UploadFailure::NoMemory => ReadFailure::NoMemory.into(),
            UploadFailure::Document(err) => LoadRefusal::Undecodable(EditFailure::Document(err)),
        })
    }

    /// Open the `length` bytes uploaded as `format`, or as their signature
    /// says, and read every entry out.
    fn decode(
        sandbox: &mut Decoder,
        format: Option<ViewFormat>,
        length: usize,
        entries: Entries,
    ) -> Result<Document, LoadRefusal> {
        let opened = open_edit(sandbox, format)?;
        // A layered document's layers are one picture, so all are read.
        let count = match entries {
            Entries::First if opened.kind != EditKind::Layers => opened.count.min(1),
            _ => opened.count,
        };
        let assembled = EditDocument { count, ..opened };
        let mut assembly = Assembly::new(assembled, length)?;
        for index in 0..count {
            match select_entry(sandbox, opened, index)? {
                EditEntry::Picture(picture) => {
                    let mut rows = assembly.canvas_for(&picture)?;
                    read_rows(sandbox, &picture, |y, samples, mask| {
                        rows.row(y, samples, mask);
                    })?;
                    assembly.picture(rows, &picture)?;
                }
                EditEntry::Kept(kept) => {
                    assembly.claim_kept(&kept)?;
                    let mut bytes = Vec::new();
                    read_kept(sandbox, &kept, &mut bytes)?;
                    assembly.kept(&kept, bytes);
                }
            }
        }
        let _ = close_edit(sandbox);
        assembly
            .finish()
            .ok_or(LoadRefusal::Undecodable(EditFailure::ReplyMalformed))
    }

    /// Read `load`'s document through a fresh worker.
    fn load_document(sandbox: &mut Decoder, load: &Load) -> Result<Document, LoadRefusal> {
        let fd = load.handle.fd();
        let length = document_length(&load.handle)?;
        let outcome = upload(sandbox, length, |offset, into| {
            tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
        })
        .and_then(|()| decode(sandbox, named_format(&load.name), length, Entries::All));
        // The worker that read this document reads no other.
        sandbox.release();
        outcome
    }

    /// Decode what the clipboard held: a picture, the first of any held.
    fn decode_paste(sandbox: &mut Decoder, bytes: &[u8]) -> Result<Canvas, String> {
        let outcome = upload(sandbox, bytes.len(), |offset, into| {
            let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
            let rest = bytes.get(start..).ok_or(Errno::OutOfRange)?;
            let take = rest.len().min(into.len());
            into[..take].copy_from_slice(&rest[..take]);
            Ok(take)
        })
        .and_then(|()| decode(sandbox, None, bytes.len(), Entries::First));
        sandbox.release();
        match outcome {
            Ok(document) => match document.into_entries().into_iter().next() {
                Some(Entry::Picture(picture)) => picture
                    .flattened()
                    .map_err(|OutOfMemory| String::from("There is not enough memory to paste")),
                _ => Err(String::from("The clipboard's picture cannot be pasted")),
            },
            Err(err) => Err(alloc::format!("The clipboard holds no picture: {err}")),
        }
    }

    /// How much of a document a decode reads in.
    #[derive(Clone, Copy)]
    enum Entries {
        /// Every entry: a document opened.
        All,
        /// The first: a picture pasted, of which no more is used.
        First,
    }

    /// `bytes`, a picture from the clipboard, decoded and laid out to float
    /// over a picture of `kind` — its colours the nearest that picture holds
    /// — on this worker rather than the window's loop.
    fn decode_pasted(
        sandbox: &mut Decoder,
        bytes: &[u8],
        kind: Kind,
    ) -> Result<(Canvas, Kind), String> {
        let canvas = decode_paste(sandbox, bytes)?;
        let adapted = adapt_pasted(&canvas, &kind)
            .map_err(|err| alloc::format!("The picture cannot be pasted: {err}"))?;
        Ok((adapted, kind))
    }

    /// Decoding for the document `stamp` names.
    struct DecodeJob {
        stamp: Stamp,
        work: DecodeWork,
    }

    enum DecodeWork {
        Load(Load),
        Paste { bytes: Vec<u8>, kind: Kind },
    }

    struct DecodeReply {
        stamp: Stamp,
        answer: DecodeAnswer,
    }

    enum DecodeAnswer {
        Loaded {
            load: Load,
            document: Result<Document, LoadRefusal>,
        },
        Pasted(Result<(Canvas, Kind), String>),
    }

    type DecodeDesk = JobQueue<Option<DecodeJob>, Option<DecodeReply>>;

    /// Each job is answered in turn; the job travels in an `Option` so the
    /// worker takes it by value.
    type DecodeWorker = Worker<Decoder, Option<DecodeJob>, Option<DecodeReply>, DecodeDesk>;

    fn serve_decode(sandbox: &mut Decoder, job: &mut Option<DecodeJob>) -> Option<DecodeReply> {
        let DecodeJob { stamp, work } = job.take()?;
        let answer = match work {
            DecodeWork::Load(load) => DecodeAnswer::Loaded {
                document: load_document(sandbox, &load),
                load,
            },
            DecodeWork::Paste { bytes, kind } => {
                DecodeAnswer::Pasted(decode_pasted(sandbox, &bytes, kind))
            }
        };
        Some(DecodeReply { stamp, answer })
    }

    /// Room the decode queue holds for each window: a load and a paste.
    const DECODES_PER_WINDOW: usize = 2;

    /// Why a save did not reach its file.
    #[derive(Clone, Debug)]
    enum SaveFailure {
        Encode(EncodeError),
        Write(Errno),
        Unwritable(SaveRefusal),
    }

    impl fmt::Display for SaveFailure {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Encode(err) => write!(f, "{err}"),
                Self::Write(err) => write!(f, "it could not be written ({err})"),
                Self::Unwritable(refusal) => write!(f, "{refusal}"),
            }
        }
    }

    /// Encode `snapshot` in the format its entries take under `name` and
    /// write it over `file`, cut to its length and made durable, answering
    /// what the format could not keep.
    fn write_out(
        file: &Handle,
        snapshot: &Snapshot,
        name: &str,
    ) -> Result<Option<&'static str>, SaveFailure> {
        let format = format_for(name, &snapshot.entries, snapshot.origin)
            .map_err(SaveFailure::Unwritable)?;
        // Found before the file is touched, so a refusal leaves it whole.
        let lost = lost_in(snapshot, format)
            .map_err(|OutOfMemory| SaveFailure::Encode(EncodeError::OutOfMemory))?;
        let bytes = encode(snapshot, format, name).map_err(SaveFailure::Encode)?;
        tairix_rt::fs_write_all(file.fd(), 0, &bytes).map_err(SaveFailure::Write)?;
        docapp::commit(file, bytes.len() as u64).map_err(SaveFailure::Write)?;
        Ok(lost)
    }

    /// The painter's own work on the host's queue, beside its saves.
    enum Work {
        /// Cut out and encode a copy for the clipboard.
        Copy(Clip),
        /// A fill, a transform, or a selection put down or cleared.
        Compute { job: u64, work: Compute },
    }

    enum Answer {
        /// The encoded copy, or why there is none.
        Copied(Result<Vec<u8>, String>),
        Computed {
            job: u64,
            answer: Computed,
        },
    }

    /// The wait-set token the settings writer's answer wake arrives under.
    const SETTINGS_TOKEN: u64 = APP_TOKEN + 1;

    /// What the store said after a settings write, or why it said nothing.
    type SettingsAnswer = Result<Published<Preferences>, Errno>;

    /// The worker Paint's settings are written through, off the loop.
    type SettingsWriter = Worker<(), PublishJob<Preferences>, SettingsAnswer>;

    /// Carry out one settings write against Paint's own store.
    fn write_settings(_: &mut (), job: &mut PublishJob<Preferences>) -> SettingsAnswer {
        tairix_appdata::publish(&mut Settings::open_without_defaults(&mut RtHost), job)
    }

    /// What the painter keeps beside the host's own.
    struct Paint {
        decoder: Arc<DecodeWorker>,
        _decoder_guard: WorkerGuard<Decoder, Option<DecodeJob>, Option<DecodeReply>, DecodeDesk>,
        /// The toolbar glyphs, rasterised once for every window.
        artwork: ArtworkCache,
        /// The settings in force, and the ones the store last said.
        preferences: Publication<Preferences>,
        writer: Arc<SettingsWriter>,
        _writer_guard: WorkerGuard<(), PublishJob<Preferences>, SettingsAnswer>,
    }

    impl DocumentApp for Paint {
        type View = View;
        type AppView = SettingsWindow;
        type Snapshot = Snapshot;
        type Extra = ();
        type Work = Work;
        type Answer = Answer;
        type Failure = SaveFailure;

        const NAME: &'static str = APP_NAME;
        const WINDOW_SIZE: (u32, u32) = WINDOW_SIZE;
        /// A copy in flight with the newer one that replaces it waiting, one
        /// of the picture's own jobs — a fill, a transform, a selection put
        /// down or cleared, one at a time, the picture taking no edit while
        /// it is out — the save in flight, and the open adjustment's one job,
        /// its preview or its histogram.
        const JOBS_PER_WINDOW: usize = 5;
        const BAR_ROWS: &'static [&'static str] = &["Settings\u{2026}"];

        fn start(desktop: &Desktop, set: u64) -> Result<Self, i32> {
            // The decode queue's room grows with each window opened.
            let Ok(decoder) = DecodeWorker::queued(
                serve_decode,
                ParserSandbox::new(RtLauncher::own_binary(), tairix_rt::LogSink),
                WorkerWake::create(),
                0,
            ) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the decode queue",
                ));
            };
            let decoder = Arc::new(decoder);
            docapp::start_worker(APP_NAME, &decoder, set, APP_TOKEN, "decode")?;
            let guard = WorkerGuard::new(&decoder);
            // Read here, before any window: nothing is on screen yet, so
            // there is no frame to owe anyone.
            let (preferences, refusals) =
                tairix_appdata::loaded(&Settings::open_without_defaults(&mut RtHost));
            for refusal in refusals {
                app::report(APP_NAME, refusal);
            }
            let writer = Arc::new(SettingsWriter::new(
                write_settings,
                (),
                WorkerWake::create(),
            ));
            docapp::start_worker(APP_NAME, &writer, set, SETTINGS_TOKEN, "settings")?;
            let writer_guard = WorkerGuard::new(&writer);
            Ok(Self {
                decoder,
                _decoder_guard: guard,
                artwork: icon_cache(desktop),
                preferences: Publication::new(preferences),
                writer,
                _writer_guard: writer_guard,
            })
        }

        fn wakes(&self) -> Vec<(u64, Arc<dyn AnswerWake>)> {
            let decoder: Arc<dyn AnswerWake> = self.decoder.clone();
            let writer: Arc<dyn AnswerWake> = self.writer.clone();
            alloc::vec![(APP_TOKEN, decoder), (SETTINGS_TOKEN, writer)]
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
        ) -> Result<Option<&'static str>, SaveFailure> {
            write_out(&job.target, &job.snapshot, name)
        }

        fn work(work: Work) -> Answer {
            match work {
                Work::Copy(clip) => Answer::Copied(
                    clip.pixels()
                        .map_err(|err| alloc::format!("{err}"))
                        .and_then(|pixels| {
                            encode_png(&pixels).map_err(|err| alloc::format!("{err}"))
                        }),
                ),
                Work::Compute { job, work } => Answer::Computed {
                    job,
                    answer: compute(work),
                },
            }
        }

        /// A newer copy withdraws one still waiting: the clipboard holds only
        /// the last.
        fn supersedes(work: &Work, waiting: &Work) -> bool {
            matches!((work, waiting), (Work::Copy(_), Work::Copy(_)))
        }

        /// A copy is the user's, not the picture's: it still reaches the
        /// clipboard once the picture has gone.
        fn outlives_document(work: &Work) -> bool {
            matches!(work, Work::Copy(_))
        }

        fn reserve_window(&mut self) -> Result<(), &'static str> {
            self.decoder
                .grow(DECODES_PER_WINDOW)
                .map_err(|_| "no room for another window's decoding")
        }

        fn release_window(&mut self) {
            self.decoder.shrink(DECODES_PER_WINDOW);
        }

        fn withdraw(&mut self, window: u64) {
            self.decoder
                .retain_waiting(|job| job.as_ref().is_none_or(|job| job.stamp.window() != window));
        }

        fn untitled(host: &Host<Self>) -> Result<View, String> {
            let preferences = host.app.preferences.live();
            let canvas = preferences
                .new
                .canvas()
                .map_err(|err| alloc::format!("{err}"))?;
            let mut view = View::new(
                Document::new_as(Picture::plain(canvas), preferences.format),
                String::from(UNTITLED),
                Access::Untitled,
            );
            view.begin(preferences);
            Ok(view)
        }

        /// A single clear pixel stands in until the document lands, so
        /// nothing is shown that the file might be taken to hold.
        fn placeholder(host: &Host<Self>, name: &str, access: Access) -> Result<View, String> {
            let canvas = Canvas::new(1, 1, Kind::Rgba, Sample::Rgba([0; 4]))
                .map_err(|_| String::from("there is not enough memory"))?;
            let mut view = View::new(
                Document::new(Picture::plain(canvas)),
                String::from(name),
                access,
            );
            view.begin(host.app.preferences.live());
            Ok(view)
        }

        fn load(host: &mut Host<Self>, index: usize, handle: Arc<Handle>, name: String) {
            let load = Load {
                handle,
                name: name.clone(),
            };
            match host.app.decoder.submit(Some(DecodeJob {
                stamp: host.windows[index].stamp(),
                work: DecodeWork::Load(load),
            })) {
                Ok(answered) => host.note_answered(answered),
                Err(_) => host.not_opened(index, &name, &ReadFailure::NoMemory),
            }
        }

        fn collect(host: &mut Host<Self>) {
            let decoder = Arc::clone(&host.app.decoder);
            decoder.collect_landed(|answer| {
                if let Some(reply) = answer {
                    adopt_decode(host, reply);
                }
            });
            collect_settings(host);
        }

        fn leaving(host: &mut Host<Self>) {
            let paint = &mut host.app;
            let mut refusals = Vec::new();
            if let Some(job) = paint.preferences.settle() {
                let _ = paint.writer.submit(job);
            }
            while let Some(answer) = paint.writer.wait() {
                if let Some(job) = paint.preferences.adopt(answer, &mut refusals) {
                    let _ = paint.writer.submit(job);
                }
            }
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
            let _ = host.open_app_window(SettingsWindow::new(record, Category::General));
        }

        /// What the settings window was in the middle of is written as it
        /// goes.
        fn app_closing(host: &mut Host<Self>, _index: usize) {
            let job = host.app.preferences.settle();
            submit_settings(host, job);
        }

        fn app_request(host: &mut Host<Self>, index: usize, request: SettingsRequest) {
            match request {
                SettingsRequest::Edit { was, now, settled } => {
                    host.app.preferences.edit(&was, &now);
                    apply_settings(host);
                    if settled {
                        let job = host.app.preferences.settle();
                        submit_settings(host, job);
                    }
                }
                SettingsRequest::Restore => {
                    let job = host.app.preferences.restore();
                    submit_settings(host, job);
                }
                SettingsRequest::TakePanes => {
                    let front = host
                        .windows
                        .iter()
                        .position(DocWindow::focused)
                        .or_else(|| host.windows.len().checked_sub(1));
                    let Some(front) = front else {
                        let window = &mut host.app_windows[index];
                        window.view.say(
                            Some(String::from(
                                "No picture window is open to take the panes of",
                            )),
                            &window.layout,
                            &mut window.damage,
                        );
                        window.owe_reported();
                        return;
                    };
                    let was = host.app.preferences.live().clone();
                    let mut now = was.clone();
                    now.panes.clone_from(host.windows[front].view.panes());
                    host.app.preferences.edit(&was, &now);
                    apply_settings(host);
                    let job = host.app.preferences.settle();
                    submit_settings(host, job);
                }
            }
        }

        /// Carry out what each window's deadlines have brought due.
        fn turn(host: &mut Host<Self>, now_ns: u64) -> bool {
            let (theme, scale) = (host.themes.active(), host.desktop.scale());
            for window in &mut host.windows {
                if window.view.deadline_ns().is_some_and(|due| due <= now_ns) {
                    window
                        .view
                        .tick(now_ns, &window.layout, scale, theme, &mut window.damage);
                    window.owe_reported();
                }
            }
            false
        }

        /// When the soonest window deadline falls.
        fn deadline(host: &mut Host<Self>, now_ns: u64) -> Option<u64> {
            host.windows
                .iter_mut()
                .filter_map(|window| {
                    window.view.arm_deadline(now_ns);
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
            carry_out(host, index, request);
        }

        fn answered(host: &mut Host<Self>, index: usize, answer: Answer) {
            match answer {
                Answer::Copied(copied) => put_copy(host, index, copied),
                Answer::Computed { job, answer } => {
                    let window = &mut host.windows[index];
                    let outcome =
                        window
                            .view
                            .computed(job, answer, &window.layout, &mut window.damage);
                    host.apply(index, outcome);
                }
            }
        }

        /// A copy whose picture has gone is put through its window, else
        /// through whichever of this program's has the keyboard — the session
        /// takes a copy from no other — and said to be lost when neither is
        /// open.
        fn orphaned(host: &mut Host<Self>, window: u64, answer: Answer) {
            let Answer::Copied(copied) = answer else {
                return;
            };
            let through = host
                .index_of(window)
                .or_else(|| host.windows.iter().position(DocWindow::focused));
            match through {
                Some(index) => put_copy(host, index, copied),
                None => host.notify(String::from(
                    "A copy was lost: its window closed before the copy was ready",
                )),
            }
        }

        fn render(
            &mut self,
            surface: &mut Surface,
            view: &View,
            layout: &Layout,
            (theme, scale, faces): (&Theme, Scale, Faces),
            _focused: bool,
        ) {
            // Every picture drawn is built in, so the resolver refuses every
            // asset tier and the cache answers from its glyphs.
            let mut resolver = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
            let mut source = IconArtworkSource::new(&mut self.artwork, &mut resolver);
            render_into(surface, view, layout, theme, scale, faces, &mut source);
        }

        fn render_app(
            &mut self,
            surface: &mut Surface,
            view: &SettingsWindow,
            layout: &SettingsLayout,
            style: (&Theme, Scale, Faces),
            _focused: bool,
        ) {
            let mut resolver = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
            let mut source = IconArtworkSource::new(&mut self.artwork, &mut resolver);
            view.render(surface, layout, style, &mut source);
        }
    }

    /// Hand `job`, the settings write owed now, to the writer, and take in
    /// its answer where it ran here for want of a worker.
    fn submit_settings(host: &mut Host<Paint>, job: Option<PublishJob<Preferences>>) {
        if job.is_some_and(|job| host.app.writer.submit(job)) {
            collect_settings(host);
        }
    }

    /// Adopt what the settings writer answered, say what it refused in the
    /// settings window and on `stderr`, make the write owed behind it, and
    /// bring every window up to the settings now in force.
    fn collect_settings(host: &mut Host<Paint>) {
        let mut refusals = Vec::new();
        let mut adopted = false;
        while let Some(answer) = host.app.writer.collect() {
            adopted = true;
            let owed = host.app.preferences.adopt(answer, &mut refusals);
            if !owed.is_some_and(|job| host.app.writer.submit(job)) {
                break;
            }
        }
        if !adopted {
            return;
        }
        let said = refusals.iter().find_map(|refusal| match refusal {
            StoreRefusal::NotSaved(_) | StoreRefusal::NotRestored(_) => {
                Some(alloc::format!("{refusal}"))
            }
            _ => None,
        });
        for refusal in &refusals {
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

    /// Bring every window up to the settings in force: each picture window
    /// draws as they say, and the settings window shows them.
    fn apply_settings(host: &mut Host<Paint>) {
        let preferences = host.app.preferences.live();
        for window in &mut host.windows {
            window
                .view
                .adopt(preferences, &window.layout, &mut window.damage);
            window.owe_reported();
        }
        for window in &mut host.app_windows {
            window
                .view
                .adopt(preferences, &window.layout, &mut window.damage);
            window.owe_reported();
        }
    }

    /// The toolbar's glyph cache, budgeted from a window's frame and
    /// registered with the process's cache report.
    fn icon_cache(desktop: &Desktop) -> ArtworkCache {
        // The reclaim bookkeeping's audit sink: the shared constructor takes
        // a `'static` borrow, and the runtime sink owns nothing.
        static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;
        let (width, height) = desktop.window_size(WINDOW_SIZE.0, WINDOW_SIZE.1);
        let frame = app::region_bytes(&app::mode_for(width, height), app::FRAME_COUNT).unwrap_or(0);
        let cache = artwork_cache(
            "paint.icon-artwork",
            SEAT_PRIMARY,
            frame,
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
        );
        if let Some(ledger) = cache.ledger() {
            tairix_rt::cachereport::register(ledger);
        }
        cache
    }

    /// Put what a copy encoded on the clipboard through window `index`, or
    /// say why it could not be.
    fn put_copy(host: &mut Host<Paint>, index: usize, copied: Result<Vec<u8>, String>) {
        let window = &mut host.windows[index];
        let refused = match copied {
            Ok(bytes) => {
                clipboard::put(&mut host.client, window.id(), ClipboardKind::Octets, &bytes)
                    .err()
                    .map(|err| alloc::format!("Could not copy: {err}"))
            }
            Err(err) => Some(alloc::format!("Could not copy: {err}")),
        };
        if let Some(message) = refused {
            window.state(message);
        }
    }

    /// Take in what the decode worker answered, for the document that asked
    /// for it: one answered for a document since gone is let go.
    fn adopt_decode(host: &mut Host<Paint>, reply: DecodeReply) {
        let DecodeReply { stamp, answer } = reply;
        let Some(index) = host.showing(stamp) else {
            return;
        };
        match answer {
            DecodeAnswer::Loaded {
                load,
                document: Ok(document),
            } => loaded(host, index, load, document),
            DecodeAnswer::Loaded {
                load,
                document: Err(why),
            } => host.not_opened(index, &load.name, &why),
            DecodeAnswer::Pasted(pasted) => {
                let window = &mut host.windows[index];
                let outcome = window
                    .view
                    .pasted(pasted, &window.layout, &mut window.damage);
                host.apply(index, outcome);
            }
        }
    }

    /// `load`'s `document` landed in window `index`.
    fn loaded(host: &mut Host<Paint>, index: usize, load: Load, document: Document) {
        let writable = host.windows[index].view.access() == Access::Writable;
        let refusal = write_back(&load.name, &document).err();
        let access = if writable && refusal.is_none() {
            Access::Writable
        } else {
            Access::ReadOnly
        };
        let mut view = View::new(document, load.name, access);
        view.begin(host.app.preferences.live());
        if let Some(refusal) = refusal.filter(|_| writable) {
            view.say(alloc::format!("{refusal}: Save asks where to save it"));
        }
        host.show(index, view, Some(load.handle));
    }

    /// Carry out a request of window `index`'s that only the painter makes.
    fn carry_out(host: &mut Host<Paint>, index: usize, request: Own) {
        let window = &mut host.windows[index];
        let id = window.id();
        match request {
            Own::NewWindow { picture, format } => {
                let pristine = window.pristine();
                match picture.canvas() {
                    Ok(canvas) => {
                        let mut view = View::new(
                            Document::new_as(Picture::plain(canvas), format),
                            String::from(UNTITLED),
                            Access::Untitled,
                        );
                        view.begin(host.app.preferences.live());
                        if pristine {
                            host.show(index, view, None);
                        } else if host.open_view(view).is_none() {
                            // The reason went to stderr; the window that
                            // asked says that it was refused.
                            host.windows[index].state("No new window could be opened");
                        }
                    }
                    Err(err) => host.windows[index]
                        .state(alloc::format!("No new picture could be made: {err}")),
                }
            }
            Own::Copy(clip) => {
                if !host.queue(index, Work::Copy(clip)) {
                    host.windows[index].state("There is no room to copy");
                }
            }
            Own::Cut { clip, job, work } => {
                if host.queue(index, Work::Copy(clip)) {
                    carry_out(host, index, Own::Compute { job, work });
                } else {
                    // Nothing is cleared that could not be copied first.
                    refuse_compute(host, index, job);
                    host.windows[index].state("There is no room to cut");
                }
            }
            Own::Paste(kind) => match clipboard::take(&mut host.client, id) {
                Ok(Some((ClipboardKind::Octets, bytes))) => {
                    match host.app.decoder.submit(Some(DecodeJob {
                        stamp: host.windows[index].stamp(),
                        work: DecodeWork::Paste { bytes, kind },
                    })) {
                        Ok(answered) => host.note_answered(answered),
                        Err(_) => host.windows[index].state("There is no room to paste"),
                    }
                }
                Ok(Some(_)) => window.state("The clipboard holds no picture"),
                Ok(None) => window.state("The clipboard is empty"),
                Err(err) => window.state(alloc::format!("Could not paste: {err}")),
            },
            Own::Compute { job, work } => {
                if !host.queue(index, Work::Compute { job, work }) {
                    refuse_compute(host, index, job);
                }
            }
        }
    }

    /// Answer job `job` of window `index` as refused, so the window stops
    /// waiting on it.
    fn refuse_compute(host: &mut Host<Paint>, index: usize, job: u64) {
        let window = &mut host.windows[index];
        let outcome = window.view.computed(
            job,
            Computed::Tiles(Err(OutOfMemory)),
            &window.layout,
            &mut window.damage,
        );
        host.apply(index, outcome);
    }

    /// The painter's whole life.
    fn main() -> i32 {
        // The sandbox-worker role first: a document is untrusted input, so it
        // is decoded by a capability-empty child this binary is re-entered as,
        // which never becomes the painter.
        if worker_role() {
            return serve_stdio(&mut ImageRenderService::default()).exit_code();
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }
        docapp::run::<Paint>()
    }

    tairix_rt::entry!(main);
}

/// The host stub: this binary is a freestanding program on the Tier-1
/// targets, so on the host it exists only to keep the file covered by the
/// workspace build, clippy, and fmt.
#[cfg(not(freestanding))]
fn main() {}
