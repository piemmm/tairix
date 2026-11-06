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
//! a decode worker reads documents and pastes through the sandbox, the host's
//! queue encodes saves and copies and carries out fills and transforms beside
//! them, so nothing that grows with the picture waits on the loop, and a
//! preview worker of its own works out previews, histograms, the Layers pane's
//! thumbnails and the composite beneath an opacity being changed across every
//! core, which an Apply's completion, a merge or a flatten on the queue takes
//! first, so no save waits behind them.
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
    use core::sync::atomic::{AtomicUsize, Ordering};

    use tairix_abi::fs::Filetype;
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::window_ipc::{AppMenuSymbol, ClipboardKind};
    use tairix_abi::Errno;
    use tairix_appdata::{
        Opening, Publication, PublishAnswer, PublishJob, PublishWorker, Refusal as StoreRefusal,
        RtHost, Settings,
    };
    use tairix_controls::damage;
    use tairix_geometry::{Region, Scale};
    use tairix_icon::{
        artwork_cache, ArtworkCache, IconArtworkSource, InlineArtwork, NoArtworkSeam,
    };
    use tairix_image::{encode_png, EncodeError};
    use tairix_paint::area::{render_area, AreaView, SpriteOpening};
    use tairix_paint::canvas::{Canvas, Kind, OutOfMemory, Sample};
    use tairix_paint::document::{Document, Entry, Picture, Snapshot};
    use tairix_paint::engine::{Engine, EngineLayout};
    use tairix_paint::layout::{Faces, WINDOW_SIZE};
    use tairix_paint::load::{Assembly, Refusal};
    use tairix_paint::preferences::Preferences;
    use tairix_paint::preview::{self, PreviewAnswer, PreviewJob};
    use tairix_paint::render::render_into;
    use tairix_paint::save::{
        encode, format_for, format_named, lost_in, write_back, SaveFormat, SaveRefusal,
    };
    use tairix_paint::selection::adapt_pasted;
    use tairix_paint::settings::{Category, SettingsLayout, SettingsRequest, SettingsWindow};
    use tairix_paint::view::{compute, Clip, Compute, Computed, Own, View};
    use tairix_parallel::{Pool, SERIAL};
    use tairix_raster::Surface;
    use tairix_rt::sync::{Condvar, Mutex, WorkerWake};
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
        self, AnswerWake, DocWindow, DocumentApp, DocumentView, Handle, Host, Outcome, Request,
        Stamp, APP_TOKEN,
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
    fn named_format(name: &str, recorded: Option<Filetype>) -> Option<ViewFormat> {
        matches!(format_named(name, recorded), Ok(Some(SaveFormat::Sprites)))
            .then_some(ViewFormat::Sprite)
    }

    /// A document being read into a window: the file, what it is called, and
    /// the filetype its volume records, read by the worker that reads it.
    struct Load {
        handle: Arc<Handle>,
        name: String,
        recorded: Option<Filetype>,
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
        .and_then(|()| {
            let format = named_format(&load.name, load.recorded);
            decode(sandbox, format, length, Entries::All)
        });
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
            DecodeWork::Load(mut load) => {
                load.recorded = tairix_browse::recorded_filetype_of(&load.handle);
                DecodeAnswer::Loaded {
                    document: load_document(sandbox, &load),
                    load,
                }
            }
            DecodeWork::Paste { bytes, kind } => {
                DecodeAnswer::Pasted(decode_pasted(sandbox, &bytes, kind))
            }
        };
        Some(DecodeReply { stamp, answer })
    }

    /// Room the decode queue holds for each window: a load and a paste.
    const DECODES_PER_WINDOW: usize = 2;

    /// A preview job, with the document that asked.
    type StampedJob = Option<(Stamp, PreviewJob)>;
    /// A preview's answer, with the document that asked.
    type StampedAnswer = Option<(Stamp, PreviewAnswer)>;
    type PreviewDesk = JobQueue<StampedJob, StampedAnswer>;

    /// Each job's blocks are spread across the cores the worker shares with the
    /// queue's work, which takes them first; a window's newer job takes the
    /// place of the one it has waiting.
    type PreviewWorker = Worker<Arc<Cores>, StampedJob, StampedAnswer, PreviewDesk>;

    fn serve_preview(cores: &mut Arc<Cores>, job: &mut StampedJob) -> StampedAnswer {
        let (stamp, job) = job.take()?;
        Some((stamp, cores.preview(job)))
    }

    /// The cores an operation is spread across, shared by the preview worker
    /// and the queue's work spread across them — an Apply's completion, a
    /// merge, a flatten — the queue's first: no preview starts while that is
    /// due, and one under way gives way at its next block, so neither finds
    /// the pool taken and works its blocks out on one core.
    struct Cores {
        pool: Pool,
        /// The queue's work due or under way that wants the cores, which each
        /// block of a preview reads.
        completing: AtomicUsize,
        /// Whether a preview holds the pool, which the queue's work waits out.
        previewing: Mutex<bool>,
        changed: Condvar,
    }

    impl Cores {
        fn new(pool: Pool) -> Self {
            Self {
                pool,
                completing: AtomicUsize::new(0),
                previewing: Mutex::new(false),
                changed: Condvar::new(),
            }
        }

        /// Whether the queue's work wants the cores, so a preview asked now
        /// would only give way.
        fn due(&self) -> bool {
            self.completing.load(Ordering::Relaxed) > 0
        }

        /// Carry out `work` on the ordered queue: an operation's completion, a
        /// merge or a flatten, spread across the cores, takes them ahead of
        /// the preview worker once a preview under way has given way.
        fn work_out(&self, work: Compute) -> Computed {
            if !matches!(work, Compute::Op { .. } | Compute::Compose { .. }) {
                return compute(work, &self.pool);
            }
            self.completing.fetch_add(1, Ordering::Relaxed);
            let mut previewing = self.previewing.lock();
            while *previewing {
                previewing = self.changed.wait(previewing);
            }
            drop(previewing);
            let computed = compute(work, &self.pool);
            self.completing.fetch_sub(1, Ordering::Relaxed);
            computed
        }

        /// Work `job` out on the preview worker, giving way at its next block
        /// to the queue's work falling due. One asked while that is due is
        /// answered as given way at once, never waited on: this may be the
        /// event loop itself, for want of a preview worker, and the loop asks
        /// again once that work has landed.
        fn preview(&self, job: PreviewJob) -> PreviewAnswer {
            let mut previewing = self.previewing.lock();
            if self.due() {
                drop(previewing);
                return preview::serve(job, &SERIAL, &|| true);
            }
            *previewing = true;
            drop(previewing);
            let answer = preview::serve(job, &self.pool, &|| self.due());
            *self.previewing.lock() = false;
            self.changed.notify_all();
            answer
        }
    }

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
        let recorded = tairix_browse::recorded_filetype_of(file);
        let format = format_for(name, recorded, &snapshot.entries, snapshot.origin)
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
        /// A fill, a transform, an operation completed, a merge, a flatten, or
        /// a selection put down or cleared, an operation's blocks and a
        /// merge's tiles spread across `cores`.
        Compute {
            job: u64,
            work: Compute,
            cores: Arc<Cores>,
        },
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

    /// The wait-set token the preview worker's answer wake arrives under.
    const PREVIEW_TOKEN: u64 = APP_TOKEN + 2;

    /// What the painter keeps beside the host's own.
    struct Paint {
        decoder: Arc<DecodeWorker>,
        _decoder_guard: WorkerGuard<Decoder, Option<DecodeJob>, Option<DecodeReply>, DecodeDesk>,
        /// The toolbar glyphs, rasterised once for every window.
        artwork: ArtworkCache,
        /// The settings in force, and the ones the store last said.
        preferences: Publication<Preferences>,
        writer: Arc<PublishWorker<Preferences>>,
        _writer_guard: WorkerGuard<Opening, PublishJob<Preferences>, PublishAnswer<Preferences>>,
        /// The worker the windows' speculative work runs on.
        previews: Arc<PreviewWorker>,
        _previews_guard: WorkerGuard<Arc<Cores>, StampedJob, StampedAnswer, PreviewDesk>,
        /// The cores the preview worker and the queue's work spread their
        /// blocks across, the queue's first.
        cores: Arc<Cores>,
    }

    impl DocumentApp for Paint {
        type View = Engine;
        type AppView = SettingsWindow;
        type Snapshot = Snapshot;
        type Extra = ();
        type Work = Work;
        type Answer = Answer;
        type Failure = SaveFailure;

        const NAME: &'static str = APP_NAME;
        const WINDOW_SIZE: (u32, u32) = WINDOW_SIZE;
        /// A copy in flight with the newer one that replaces it waiting, one
        /// of the picture's own jobs — a fill, a transform, an operation
        /// completed, a selection put down or cleared, one at a time, the
        /// picture taking no edit while it is out — and the save in flight;
        /// previews, histograms and the Layers pane's thumbnails and
        /// composite run on the preview worker.
        const JOBS_PER_WINDOW: usize = 4;
        const BAR_ROWS: &'static [(AppMenuSymbol, &'static str)] =
            &[(AppMenuSymbol::Settings, "Settings\u{2026}")];

        fn start(desktop: &Desktop, set: u64) -> Result<Self, i32> {
            // The decode queue's room grows with each window opened.
            let Ok(decoder) = DecodeWorker::queued(
                serve_decode,
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
                    "no room for the decode queue",
                ));
            };
            let decoder = Arc::new(decoder);
            app::start_worker(APP_NAME, &decoder, set, APP_TOKEN, "decode");
            let guard = WorkerGuard::new(&decoder);
            // Read here, before any window: nothing is on screen yet, so
            // there is no frame to owe anyone.
            let (preferences, refusals) =
                tairix_appdata::loaded(&Settings::open_without_defaults(&mut RtHost));
            for refusal in refusals {
                app::report(APP_NAME, refusal);
            }
            let writer = Arc::new(tairix_appdata::publish_worker(Opening::Bare));
            app::start_worker(APP_NAME, &writer, set, SETTINGS_TOKEN, "settings");
            let writer_guard = WorkerGuard::new(&writer);
            let online = tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport);
            let pool = app::pool_across(APP_NAME, online, "working out previews");
            let cores = Arc::new(Cores::new(pool));
            // The preview queue's room grows with each window opened.
            let Ok(previews) =
                PreviewWorker::queued(serve_preview, Arc::clone(&cores), WorkerWake::create(), 0)
            else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the preview queue",
                ));
            };
            let previews = Arc::new(previews);
            app::start_worker(APP_NAME, &previews, set, PREVIEW_TOKEN, "preview");
            let previews_guard = WorkerGuard::new(&previews);
            Ok(Self {
                decoder,
                _decoder_guard: guard,
                artwork: icon_cache(desktop),
                preferences: Publication::new(preferences),
                writer,
                _writer_guard: writer_guard,
                previews,
                _previews_guard: previews_guard,
                cores,
            })
        }

        fn wakes(&self) -> Vec<(u64, Arc<dyn AnswerWake>)> {
            let decoder: Arc<dyn AnswerWake> = self.decoder.clone();
            let writer: Arc<dyn AnswerWake> = self.writer.clone();
            let previews: Arc<dyn AnswerWake> = self.previews.clone();
            alloc::vec![
                (APP_TOKEN, decoder),
                (SETTINGS_TOKEN, writer),
                (PREVIEW_TOKEN, previews)
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
                Work::Compute { job, work, cores } => Answer::Computed {
                    job,
                    answer: cores.work_out(work),
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
                .map_err(|_| "no room for another window's decoding")?;
            if self.previews.grow(preview::PREVIEW_ROOM).is_err() {
                self.decoder.shrink(DECODES_PER_WINDOW);
                return Err("no room for another window's previews");
            }
            Ok(())
        }

        fn release_window(&mut self) {
            self.decoder.shrink(DECODES_PER_WINDOW);
            self.previews.shrink(preview::PREVIEW_ROOM);
        }

        /// The window's waiting decodes and previews go with it; a preview
        /// already running is abandoned by the window's ticket.
        fn withdraw(&mut self, window: u64) {
            self.decoder
                .retain_waiting(|job| job.as_ref().is_none_or(|job| job.stamp.window() != window));
            self.previews.retain_waiting(|job| {
                job.as_ref()
                    .is_none_or(|(stamp, _)| stamp.window() != window)
            });
        }

        fn untitled(host: &Host<Self>) -> Result<Engine, String> {
            let preferences = host.app.preferences.live();
            let canvas = preferences
                .new
                .canvas()
                .map_err(|err| alloc::format!("{err}"))?;
            Ok(new_picture_engine(host, canvas, preferences.format))
        }

        /// A single clear pixel stands in until the document lands, so
        /// nothing is shown that the file might be taken to hold.
        fn placeholder(host: &Host<Self>, name: &str, access: Access) -> Result<Engine, String> {
            let canvas = Canvas::new(1, 1, Kind::Rgba, Sample::Rgba([0; 4]))
                .map_err(|_| String::from("there is not enough memory"))?;
            let mut view = View::new(
                Document::new(Picture::plain(canvas)),
                String::from(name),
                None,
                access,
            );
            view.begin(host.app.preferences.live());
            Ok(Engine::Picture(alloc::boxed::Box::new(view)))
        }

        fn load(host: &mut Host<Self>, index: usize, handle: Arc<Handle>, name: String) {
            let load = Load {
                handle,
                name: name.clone(),
                recorded: None,
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
            let previews = Arc::clone(&host.app.previews);
            previews.collect_landed(|answer| {
                if let Some((stamp, answer)) = answer {
                    adopt_preview(host, stamp, answer);
                }
            });
            collect_settings(host);
        }

        fn leaving(host: &mut Host<Self>) {
            let paint = &mut host.app;
            let mut refusals = Vec::new();
            tairix_appdata::see_out_published(
                &*paint.writer,
                &mut paint.preferences,
                &mut refusals,
            );
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
                    let pictures = || {
                        host.windows
                            .iter()
                            .enumerate()
                            .filter(|(_, window)| window.view.picture().is_some())
                    };
                    let front = pictures()
                        .find(|(_, window)| window.focused())
                        .or_else(|| pictures().next_back())
                        .map(|(at, _)| at);
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
                    if let Some(view) = host.windows[front].view.picture() {
                        now.panes.clone_from(view.panes());
                    }
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
                let (Engine::Picture(view), EngineLayout::Picture(layout)) =
                    (&mut window.view, &window.layout)
                else {
                    continue;
                };
                if view.deadline_ns().is_some_and(|due| due <= now_ns) {
                    view.tick(now_ns, layout, scale, theme, &mut window.damage);
                    window.owe_reported();
                }
            }
            // A sprite file window opens what it was made to, its sprite
            // windows follow it, and it asks for the thumbnails it lacks,
            // whatever moved it: an input, a save landing, a resize.
            let mut at = 0;
            while let Some(window) = host.windows.get_mut(at) {
                let id = window.id();
                if let Some(area) = window.view.area_mut() {
                    let pending = area.take_pending();
                    if !pending.is_empty() {
                        open_sprites(host, at, &pending);
                    }
                    if let Some(file) = host.index_of(id) {
                        sync_sprite_windows(host, file);
                    }
                }
                at = host.index_of(id).map_or(at, |now| now + 1);
            }
            // A picture window asks the preview worker for what it wants
            // next, whatever moved it: an input, an answer, a resize. None is
            // asked while the queue's work wants the cores, which a preview
            // would only give way to; that work's landing wakes the loop.
            if host.app.cores.due() {
                return false;
            }
            for at in 0..host.windows.len() {
                let wanted = host.windows.get_mut(at).and_then(|window| {
                    match (&mut window.view, &window.layout) {
                        (Engine::Picture(view), EngineLayout::Picture(layout)) => {
                            view.wanted_preview(layout, scale)
                        }
                        _ => None,
                    }
                });
                if let Some(request) = wanted {
                    host.apply(at, Outcome::asking(request));
                }
            }
            false
        }

        /// When the soonest window deadline falls.
        fn deadline(host: &mut Host<Self>, now_ns: u64) -> Option<u64> {
            let (theme, scale) = (host.themes.active(), host.desktop.scale());
            host.windows
                .iter_mut()
                .filter_map(|window| {
                    let (Engine::Picture(view), EngineLayout::Picture(layout)) =
                        (&mut window.view, &window.layout)
                    else {
                        return None;
                    };
                    view.arm_deadline(now_ns, layout, scale, theme);
                    view.deadline_ns()
                })
                .min()
        }

        fn pressure(host: &mut Host<Self>) {
            tairix_font::trim_glyph_cache();
            let band = tairix_rt::pressure::gauge().band();
            for window in &mut host.windows {
                window
                    .view
                    .adopt_pressure(band, &window.layout, &mut window.damage);
                window.owe_reported();
            }
        }

        fn request(host: &mut Host<Self>, index: usize, request: Own) {
            carry_out(host, index, request);
        }

        /// A sprite window's edit becomes its file's as it lands, and a sprite
        /// file window's names, changes and deletions reach its sprite windows.
        fn driven(host: &mut Host<Self>, index: usize) {
            if let Some(file) = host.primary_of(index) {
                hand_to_file(host, index, file);
            } else if host.windows[index].view.area().is_some() {
                sync_sprite_windows(host, index);
            }
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
            view: &Engine,
            layout: &EngineLayout,
            (theme, scale, faces): (&Theme, Scale, Faces),
            _focused: bool,
        ) {
            match (view, layout) {
                (Engine::Picture(view), EngineLayout::Picture(layout)) => {
                    // Every picture drawn is built in, so the resolver
                    // refuses every asset tier and the cache answers from its
                    // glyphs.
                    let mut resolver = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
                    let mut source = IconArtworkSource::new(&mut self.artwork, &mut resolver);
                    render_into(surface, view, layout, theme, scale, faces, &mut source);
                }
                (Engine::Area(view), EngineLayout::Area(layout)) => {
                    render_area(surface, view, layout, theme, faces, scale);
                }
                _ => {}
            }
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
        let mut refusals = Vec::new();
        let paint = &mut host.app;
        if tairix_appdata::submit_published(
            &*paint.writer,
            &mut paint.preferences,
            job,
            &mut refusals,
        ) {
            settings_adopted(host, &refusals);
        }
    }

    /// Adopt what the settings writer answered and make the write owed behind
    /// it.
    fn collect_settings(host: &mut Host<Paint>) {
        let mut refusals = Vec::new();
        let paint = &mut host.app;
        if tairix_appdata::collect_published(&*paint.writer, &mut paint.preferences, &mut refusals)
        {
            settings_adopted(host, &refusals);
        }
    }

    /// Say what the store refused, in the settings window and on `stderr`,
    /// and bring every window up to the settings now in force.
    fn settings_adopted(host: &mut Host<Paint>, refusals: &[StoreRefusal]) {
        let said = refusals.iter().find_map(|refusal| match refusal {
            StoreRefusal::NotSaved(_) | StoreRefusal::NotRestored(_) => {
                Some(alloc::format!("{refusal}"))
            }
            _ => None,
        });
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

    /// Bring every window up to the settings in force: each picture window
    /// draws as they say, and the settings window shows them.
    fn apply_settings(host: &mut Host<Paint>) {
        let preferences = host.app.preferences.live();
        for window in &mut host.windows {
            if let (Engine::Picture(view), EngineLayout::Picture(layout)) =
                (&mut window.view, &window.layout)
            {
                view.adopt(preferences, layout, &mut window.damage);
                window.owe_reported();
            }
        }
        for window in &mut host.app_windows {
            window
                .view
                .adopt(preferences, &window.layout, &mut window.damage);
            window.owe_reported();
        }
    }

    /// The toolbar's glyph cache, budgeted from a window's frame and the
    /// machine's memory, and registered with the process's cache report.
    fn icon_cache(desktop: &Desktop) -> ArtworkCache {
        // The reclaim bookkeeping's audit sink: the shared constructor takes
        // a `'static` borrow, and the runtime sink owns nothing.
        static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;
        let (width, height) = desktop.window_size(WINDOW_SIZE.0, WINDOW_SIZE.1);
        let frame = app::region_bytes(&app::mode_for(width, height), app::FRAME_COUNT).unwrap_or(0);
        let cache = artwork_cache(
            "paint.icon-artwork",
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

    /// Put what a copy encoded on the clipboard through window `index`, or
    /// say why it could not be.
    fn put_copy(host: &mut Host<Paint>, index: usize, copied: Result<Vec<u8>, String>) {
        let window = &mut host.windows[index];
        let refused = match copied {
            Ok(bytes) => clipboard::put(&mut host.client, window.id(), ClipboardKind::Png, &bytes)
                .err()
                .map(|err| alloc::format!("Could not copy: {err}")),
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
                if let (Engine::Picture(view), EngineLayout::Picture(layout)) =
                    (&mut window.view, &window.layout)
                {
                    let outcome = view.pasted(pasted, layout, &mut window.damage);
                    host.apply(index, outcome);
                }
            }
        }
    }

    /// `load`'s `document` landed in window `index`.
    fn loaded(host: &mut Host<Paint>, index: usize, load: Load, document: Document) {
        let writable = host.windows[index].view.access() == Access::Writable;
        let refusal = write_back(&load.name, load.recorded, &document).err();
        let access = if writable && refusal.is_none() {
            Access::Writable
        } else {
            Access::ReadOnly
        };
        let said = refusal
            .filter(|_| writable)
            .map(|refusal| alloc::format!("{refusal}: Save asks where to save it"));
        // A sprite file is shown as its sprites, each opened in a window of
        // its own.
        let mut engine = if document.is_sprite_area() {
            let double_click = host.desktop.info().double_click();
            Engine::Area(alloc::boxed::Box::new(AreaView::new(
                document,
                load.name,
                load.recorded,
                access,
                double_click,
            )))
        } else {
            let mut view = View::new(document, load.name, load.recorded, access);
            view.begin(host.app.preferences.live());
            Engine::Picture(alloc::boxed::Box::new(view))
        };
        if let Some(said) = said {
            SavedDocument::say(&mut engine, said);
        }
        host.show(index, engine, Some(load.handle));
        ask_thumbnails(host, index);
    }

    /// The engine a new picture of `canvas`, to be saved as `format`, is
    /// shown in: a sprite file window holding it as its one sprite where
    /// it is to be a sprite file.
    fn new_picture_engine(host: &Host<Paint>, canvas: Canvas, format: SaveFormat) -> Engine {
        let mut picture = Picture::plain(canvas);
        if format == SaveFormat::Sprites {
            if let Some(name) = tairix_image::SpriteName::new(tairix_paint::document::SPRITE_STEM) {
                picture.sprite = Some(tairix_paint::document::SpriteInfo::for_canvas(
                    name,
                    picture.canvas(),
                ));
            }
            let document = Document::new_as(picture, format);
            return Engine::Area(alloc::boxed::Box::new(
                AreaView::new(
                    document,
                    String::from(UNTITLED),
                    None,
                    Access::Untitled,
                    host.desktop.info().double_click(),
                )
                .opening_its_sprites(),
            ));
        }
        let mut view = View::new(
            Document::new_as(picture, format),
            String::from(UNTITLED),
            None,
            Access::Untitled,
        );
        view.begin(host.app.preferences.live());
        Engine::Picture(alloc::boxed::Box::new(view))
    }

    /// Ask for the thumbnails a sprite file window at `index` still lacks.
    fn ask_thumbnails(host: &mut Host<Paint>, index: usize) {
        let wanted = host
            .windows
            .get_mut(index)
            .and_then(|window| window.view.area_mut())
            .and_then(AreaView::wanted);
        if let Some(request) = wanted {
            host.apply(index, Outcome::asking(request));
        }
    }

    /// Hand sprite window `index`'s edit, where it made one, to its sprite
    /// file window `file`.
    fn hand_to_file(host: &mut Host<Paint>, index: usize, file: usize) {
        let Some(view) = host.windows[index].view.picture_mut() else {
            return;
        };
        let (Some(id), Some(picture)) = (view.member(), view.take_edit()) else {
            return;
        };
        let window = &mut host.windows[file];
        let (Engine::Area(area), EngineLayout::Area(layout)) = (&mut window.view, &window.layout)
        else {
            return;
        };
        let outcome = area.adopt_edited(id, picture, layout, &mut window.damage);
        host.apply(file, outcome);
    }

    /// Bring sprite file window `file`'s sprite windows into line with it:
    /// each retitled by its sprite's name and the file's state, and the
    /// window of a sprite no longer in it closed.
    fn sync_sprite_windows(host: &mut Host<Paint>, file: usize) {
        if !host.windows[file]
            .view
            .area_mut()
            .is_some_and(AreaView::members_moved)
        {
            ask_thumbnails(host, file);
            return;
        }
        let Some(area) = host.windows[file].view.area() else {
            return;
        };
        let modified = area.is_modified();
        let file_name = String::from(area.name());
        let members: Vec<(usize, Option<tairix_image::SpriteName>)> = host
            .members(file)
            .map(|member| {
                let named = host.windows[member]
                    .view
                    .picture()
                    .and_then(View::member)
                    .and_then(|id| area.sprite_name(id));
                (member, named)
            })
            .collect();
        let mut closing = Vec::new();
        for (member, named) in members {
            let window = &mut host.windows[member];
            match (named, window.view.picture_mut()) {
                (Some(name), Some(view)) => {
                    if view.set_member(name, &file_name, modified) {
                        window
                            .damage
                            .add(<Engine as DocumentView>::message_area(&window.layout));
                        window.owe_reported();
                    }
                }
                _ => closing.push(window.id()),
            }
        }
        for id in closing {
            if let Some(member) = host.index_of(id) {
                host.apply(member, Outcome::asking(Request::Close));
            }
        }
        ask_thumbnails(host, file);
    }

    /// Open each of `ids`, sprites of sprite file window `file`, in a window
    /// of its own, or bring forward the one it has.
    fn open_sprites(host: &mut Host<Paint>, file: usize, ids: &[tairix_paint::document::EntryId]) {
        let mut opening = SpriteOpening::of(ids.len());
        for &id in ids {
            if !opening.goes_on() {
                break;
            }
            let open = host.members(file).find(|&member| {
                host.windows[member].view.picture().and_then(View::member) == Some(id)
            });
            if let Some(member) = open {
                host.raise_window(member);
                opening.opened();
                continue;
            }
            let Some(area) = host.windows[file].view.area() else {
                return;
            };
            let Some(mut view) = sprite_view(area, id) else {
                opening.short();
                continue;
            };
            view.begin(host.app.preferences.live());
            match host.open_member(file, Engine::Picture(alloc::boxed::Box::new(view))) {
                Some(_) => opening.opened(),
                None => opening.refused(),
            }
        }
        if let Some(said) = opening.said() {
            host.windows[file].state(said);
        }
    }

    /// A sprite window over sprite `id` of `area`, or `None` for a sprite
    /// that is not a picture here or memory refused.
    fn sprite_view(area: &AreaView, id: tairix_paint::document::EntryId) -> Option<View> {
        let document = area.document();
        let index = document.index_of(id)?;
        let Entry::Picture(picture) = &document.entries()[index] else {
            return None;
        };
        let name = *document.entries()[index].name()?;
        let single = Document::of(
            alloc::vec![Entry::Picture(picture.try_clone().ok()?)],
            document.origin(),
            document.unkept(),
            document.settings(),
        )?;
        Some(View::sprite_window(
            single,
            (String::from(area.name()), area.is_modified()),
            (id, name),
            area.access(),
        ))
    }

    /// Carry out a request of window `index`'s that only the painter makes.
    fn carry_out(host: &mut Host<Paint>, index: usize, request: Own) {
        let window = &mut host.windows[index];
        let id = window.id();
        match request {
            Own::OpenSprites(ids) => open_sprites(host, index, &ids),
            Own::ShowSpriteFile => {
                if let Some(file) = host.primary_of(index) {
                    host.raise_window(file);
                }
            }
            Own::NewWindow { picture, format } => {
                let pristine = window.pristine();
                match picture.canvas() {
                    Ok(canvas) => {
                        let engine = new_picture_engine(host, canvas, format);
                        if pristine {
                            host.show(index, engine, None);
                        } else if host.open_view(engine).is_none() {
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
                Ok(Some((ClipboardKind::Png | ClipboardKind::Octets, bytes))) => {
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
                let cores = Arc::clone(&host.app.cores);
                if !host.queue(index, Work::Compute { job, work, cores }) {
                    refuse_compute(host, index, job);
                }
            }
            Own::Preview { job, stamp, work } => ask_preview(host, index, job, (stamp, work)),
        }
    }

    /// Hand window `index`'s preview job `job` to the preview worker in place
    /// of the one the window has waiting; one it has no room for is refused
    /// to the window, which asks again after its next input.
    fn ask_preview(
        host: &mut Host<Paint>,
        index: usize,
        job: u64,
        (stamp, work): (preview::Stamp, preview::PreviewWork),
    ) {
        let window = &host.windows[index];
        let asked = PreviewJob {
            window: window.id(),
            job,
            stamp,
            work,
        };
        let document = window.stamp();
        host.app.previews.retain_waiting(|waiting| {
            waiting
                .as_ref()
                .is_none_or(|(_, waiting)| !preview::supersedes(&asked, waiting))
        });
        match host.app.previews.submit(Some((document, asked))) {
            Ok(answered) => host.note_answered(answered),
            Err(_) => {
                if let Some(view) = host.windows[index].view.picture_mut() {
                    view.preview_refused(job);
                }
            }
        }
    }

    /// Land what the preview worker answered on the document that asked for
    /// it: one answered for a document since gone is let go.
    fn adopt_preview(host: &mut Host<Paint>, stamp: Stamp, answer: PreviewAnswer) {
        let Some(index) = host.showing(stamp) else {
            return;
        };
        let window = &mut host.windows[index];
        if let (Engine::Picture(view), EngineLayout::Picture(layout)) =
            (&mut window.view, &window.layout)
        {
            view.previewed(answer, layout, &mut window.damage);
            window.owe_reported();
        }
    }

    /// Answer job `job` of window `index` as refused, so the window stops
    /// waiting on it.
    fn refuse_compute(host: &mut Host<Paint>, index: usize, job: u64) {
        let window = &mut host.windows[index];
        // A sprite file window asks again once something changes, never at
        // once: the queue just said it had no room.
        if let Some(area) = window.view.area_mut() {
            area.refused(job);
            return;
        }
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
