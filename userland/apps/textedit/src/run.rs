//! The `TextEdit.app` bundle's `Run` entry point: the desktop editor.
//!
//! The manifest requests no filesystem capability. Every document is the
//! user's own act — a descriptor a launcher opened for this program, or a
//! grant the session's trusted picker delegated — and every save writes
//! through that descriptor. Colouring, detecting and validating a document
//! parse untrusted bytes, so they run in a capability-empty worker this binary
//! is re-entered as.
//!
//! The editor is the host-tested engine ([`tairix_textedit`]), run in the
//! shared document host (`tairix_window::docapp`). What is the editor's own is
//! here: a document is read in and searched and converted a step at a time on
//! the host's queue, beside its saves, and a syntax worker drives the sandbox.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

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
    use core::ops::ControlFlow;

    use tairix_abi::fs::FS_IO_MAX;
    use tairix_abi::window_ipc::ClipboardKind;
    use tairix_abi::Errno;
    use tairix_controls::damage;
    use tairix_font::BitmapFont;
    use tairix_geometry::{Region, Scale};
    use tairix_raster::Surface;
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_sandbox::decode::{self, DecodeService, Isa, MAX_INPUT};
    use tairix_sandbox::imageinspect::{self, inspect_document, InspectFailure, InspectService};
    use tairix_sandbox::imagerender::{
        upload_document, UploadFailure, ViewFormat, MAX_DOCUMENT_BYTES,
    };
    use tairix_sandbox::rt::{serve_stdio, serves, worker_role, RtLauncher};
    use tairix_sandbox::textsyntax::{
        detect, lex_lines, validate_document, LexedBatch, SyntaxFailure, TextSyntaxService,
        MAX_HEAD_LEN, MAX_VALIDATE_LEN,
    };
    use tairix_sandbox::{ParserSandbox, WorkerExit};
    use tairix_syntax::{Diagnostic, Format, Severity};
    use tairix_textedit::answer::Unlisted;
    use tairix_textedit::code::{list_code, CodeListing};
    use tairix_textedit::data::{self, Structure};
    use tairix_textedit::detect::format_for_name;
    use tairix_textedit::document::{Document, OutOfMemory, Snapshot, STEP_BYTES};
    use tairix_textedit::editor::{Conversion, Converted, Editor, Mode};
    use tairix_textedit::find::{Search, Step};
    use tairix_textedit::highlight::LexJob;
    use tairix_textedit::layout::{Faces, Layout, WINDOW_SIZE};
    use tairix_textedit::load::{ReadStep, Reading};
    use tairix_textedit::paint::render_into;
    use tairix_textedit::view::{Own, View};
    use tairix_theme::{TextRole, Theme};
    use tairix_util::report::ReportLine;
    use tairix_window::docapp::{
        self, AnswerWake, DocWindow, DocumentApp, Handle, Host, NoAppView, Stamp, APP_TOKEN,
    };
    use tairix_window::document::{Access, ReadFailure, SaveJob, SavedDocument, UNTITLED};
    use tairix_window::{clipboard, Desktop};

    /// The name this program's bundle, help and refusals go by.
    const APP_NAME: &str = "TextEdit";

    /// A document being read into a window, a step at a time: the file, what
    /// it is called, and how far the read has got — `None` until the file has
    /// been measured.
    struct Load {
        handle: Arc<Handle>,
        name: String,
        reading: Option<Reading>,
    }

    impl Load {
        /// Read on a step, answering the editor once the whole document is in:
        /// coloured as its name says, and shown as its bytes suggest.
        fn step(&mut self) -> Option<Result<Editor, ReadFailure>> {
            let fd = self.handle.fd();
            let reading = match &mut self.reading {
                Some(reading) => reading,
                None => match self.handle.regular_len() {
                    Ok(Some(length)) => self.reading.insert(Reading::new(length)),
                    Ok(None) => return Some(Err(ReadFailure::NotAFile)),
                    Err(err) => return Some(Err(ReadFailure::Unreadable(err))),
                },
            };
            let chunks = match reading.step(STEP_BYTES, FS_IO_MAX, |offset, into| {
                tairix_rt::fs_read_full(fd, offset, into)
            }) {
                ReadStep::Partial => return None,
                ReadStep::Done(chunks) => chunks,
                ReadStep::Refused(err) => return Some(Err(ReadFailure::Unreadable(err))),
                ReadStep::NoMemory => return Some(Err(ReadFailure::NoMemory)),
            };
            let format = format_for_name(&self.name).unwrap_or(Format::PlainText);
            Some(
                Document::from_chunks(chunks)
                    .map(|document| Editor::new(document, format))
                    .map_err(|OutOfMemory| ReadFailure::NoMemory),
            )
        }
    }

    /// Write `snapshot` over `file`, cut it to its length, and make it
    /// durable.
    ///
    /// The pieces are gathered into runs of what one call moves; a run the
    /// allocator refuses writes piece by piece instead: slower, never wrong.
    fn write_out(file: &Handle, snapshot: &Snapshot) -> Result<(), Errno> {
        let mut run = Vec::new();
        let _ = run.try_reserve_exact(snapshot.len().min(FS_IO_MAX));
        let mut offset = 0u64;
        snapshot.gather(&mut run, |bytes| {
            tairix_rt::fs_write_all(file.fd(), offset, bytes)?;
            offset = offset.saturating_add(bytes.len() as u64);
            Ok(())
        })?;
        docapp::commit(file, offset)
    }

    /// The editor's own work on the host's queue, ordered with its saves.
    enum Work {
        /// Read one more step of a document in.
        Load(Load),
        /// Run one more step of a search.
        Search(Searching),
        /// Convert one more step of a document's line breaks.
        Convert(Converting),
    }

    /// A search under way: which one, over which document, and how far.
    struct Searching {
        id: u64,
        generation: u64,
        snapshot: Arc<Snapshot>,
        search: Search,
        replacement: Option<Vec<u8>>,
    }

    /// A line-ending conversion under way: which one, over the document it
    /// began on.
    struct Converting {
        id: u64,
        generation: u64,
        snapshot: Arc<Snapshot>,
        conversion: Conversion,
    }

    enum Answer {
        /// A load went a step on; `editor` once the whole document is in.
        Loaded {
            load: Load,
            editor: Option<Result<Box<Editor>, ReadFailure>>,
        },
        Searched {
            searching: Searching,
            step: Step,
        },
        /// A conversion went a step on; `done` once it has been through.
        Converted {
            converting: Converting,
            done: Option<Result<Option<Vec<Vec<u8>>>, OutOfMemory>>,
        },
    }

    type SyntaxSandbox = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    /// Syntax work for the document `stamp` names.
    struct SyntaxJob {
        stamp: Stamp,
        work: SyntaxWork,
    }

    enum SyntaxWork {
        /// Colour one batch of lines.
        Lex(LexJob),
        /// Name the format of a document's opening bytes.
        Detect(Vec<u8>),
        /// Check a settings store as its own parser would.
        Validate {
            generation: u64,
            format: Format,
            snapshot: Arc<Snapshot>,
        },
    }

    struct SyntaxReply {
        stamp: Stamp,
        answer: SyntaxAnswer,
    }

    enum SyntaxAnswer {
        Lexed {
            id: u64,
            batch: Result<LexedBatch, SyntaxFailure>,
        },
        Detected(Result<Option<Format>, SyntaxFailure>),
        Validated {
            generation: u64,
            format: Format,
            diagnostics: Result<Vec<Diagnostic>, SyntaxFailure>,
        },
    }

    type SyntaxWorker = Worker<SyntaxSandbox, SyntaxJob, SyntaxReply>;

    type CodeSandbox = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    /// What each worker is spawned to serve: a worker apiece, so an
    /// executable's or a picture's parsers never share an address space with
    /// the ones that colour text, or with each other.
    const CODE_SERVICE: &[u8] = b"code";
    const SYNTAX_SERVICE: &[u8] = b"syntax";
    const DATA_SERVICE: &[u8] = b"data";

    /// The decode worker's answer wake.
    const CODE_TOKEN: u64 = APP_TOKEN + 1;

    /// The picture worker's answer wake.
    const DATA_TOKEN: u64 = APP_TOKEN + 2;

    type DataSandbox = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    /// A picture's structure report of the document `stamp` names, asked as
    /// `id`.
    struct DataJob {
        stamp: Stamp,
        id: u64,
        snapshot: Arc<Snapshot>,
        named: Option<ViewFormat>,
    }

    impl DataJob {
        const fn structure(&self) -> Structure {
            Structure::Picture(self.named)
        }
    }

    struct DataReply {
        stamp: Stamp,
        id: u64,
        report: Result<Vec<ReportLine>, Unlisted>,
    }

    type DataWorker = Worker<DataSandbox, DataJob, DataReply>;

    /// Hand the worker the snapshot a run at a time, so the editor never
    /// holds a second copy of a picture, then ask for its report.
    fn serve_data(sandbox: &mut DataSandbox, job: &mut DataJob) -> DataReply {
        let snapshot = &job.snapshot;
        let sent = upload_document(sandbox, snapshot.len(), |offset, into| {
            Ok::<_, core::convert::Infallible>(read_at(snapshot, offset, into))
        });
        let report = match sent {
            Ok(()) => inspect_document(sandbox, job.named).map_err(data::unreported),
            Err(failure) if failure.out_of_memory() => Err(job.structure().no_memory()),
            Err(UploadFailure::Document(failure)) => {
                Err(data::unreported(InspectFailure::Document(failure)))
            }
            Err(UploadFailure::Shrank | UploadFailure::NoMemory | UploadFailure::Read(_)) => {
                Err(job.structure().no_memory())
            }
        };
        DataReply {
            stamp: job.stamp,
            id: job.id,
            report,
        }
    }

    /// Fill `into` from `snapshot` at `offset`, answering how much it holds.
    fn read_at(snapshot: &Snapshot, offset: u64, into: &mut [u8]) -> usize {
        let Ok(offset) = usize::try_from(offset) else {
            return 0;
        };
        let mut filled = 0;
        snapshot.walk(offset, |bytes| {
            let take = bytes.len().min(into.len() - filled);
            into[filled..filled + take].copy_from_slice(&bytes[..take]);
            filled += take;
            if filled == into.len() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        filled
    }

    /// What the decode worker is asked for a document: its code listed as
    /// an instruction set, or a manifest's report.
    #[derive(Copy, Clone)]
    enum CodeWork {
        List {
            isa: Option<Isa>,
            system: Option<Isa>,
        },
        Report,
    }

    /// The decode worker's work for the document `stamp` names, asked as
    /// `id`.
    struct CodeJob {
        stamp: Stamp,
        id: u64,
        snapshot: Arc<Snapshot>,
        work: CodeWork,
    }

    enum CodeAnswer {
        Listing(Result<CodeListing, Unlisted>),
        Report(Result<Vec<ReportLine>, Unlisted>),
    }

    struct CodeReply {
        stamp: Stamp,
        id: u64,
        answer: CodeAnswer,
    }

    type CodeWorker = Worker<CodeSandbox, CodeJob, CodeReply>;

    fn serve_code(sandbox: &mut CodeSandbox, job: &mut CodeJob) -> CodeReply {
        let bytes = bytes_of(&job.snapshot);
        let answer = match job.work {
            CodeWork::List { isa, system } => CodeAnswer::Listing(match bytes {
                Some(image) => list_code(sandbox, &image, (isa, system), &mut tairix_rt::clock_get),
                None => Err(Unlisted::no_memory()),
            }),
            CodeWork::Report => CodeAnswer::Report(match bytes {
                Some(manifest) => data::report_manifest(sandbox, &manifest),
                None => Err(Structure::Manifest.no_memory()),
            }),
        };
        CodeReply {
            stamp: job.stamp,
            id: job.id,
            answer,
        }
    }

    fn serve_syntax(sandbox: &mut SyntaxSandbox, job: &mut SyntaxJob) -> SyntaxReply {
        let answer = match &job.work {
            SyntaxWork::Lex(batch) => {
                let lines: Vec<&[u8]> = batch.lines().collect();
                SyntaxAnswer::Lexed {
                    id: batch.id,
                    batch: lex_lines(sandbox, batch.format, batch.state, &lines),
                }
            }
            SyntaxWork::Detect(head) => SyntaxAnswer::Detected(detect(sandbox, head)),
            SyntaxWork::Validate {
                generation,
                format,
                snapshot,
            } => SyntaxAnswer::Validated {
                generation: *generation,
                format: *format,
                diagnostics: materialise(snapshot)
                    .and_then(|text| validate_document(sandbox, *format, &text)),
            },
        };
        SyntaxReply {
            stamp: job.stamp,
            answer,
        }
    }

    /// `snapshot` as one run of bytes, for a store short enough to check.
    fn materialise(snapshot: &Snapshot) -> Result<Vec<u8>, SyntaxFailure> {
        if snapshot.len() > MAX_VALIDATE_LEN {
            return Err(SyntaxFailure::TooLarge);
        }
        bytes_of(snapshot).ok_or(SyntaxFailure::TooLarge)
    }

    /// `snapshot` as one run of bytes, or `None` when the allocator will not
    /// hold them.
    fn bytes_of(snapshot: &Snapshot) -> Option<Vec<u8>> {
        let mut run = Vec::new();
        run.try_reserve_exact(snapshot.len()).ok()?;
        snapshot.walk(0, |bytes| {
            run.extend_from_slice(bytes);
            ControlFlow::Continue(())
        });
        Some(run)
    }

    /// What the editor keeps beside the host's own.
    struct TextEdit {
        /// Colouring, format naming and store checks, one at a time.
        syntax: Arc<SyntaxWorker>,
        _syntax_guard: WorkerGuard<SyntaxSandbox, SyntaxJob, SyntaxReply>,
        syntax_busy: bool,
        /// The window after the one whose syntax job ran last, so no window
        /// can starve another's colouring.
        next_syntax: usize,
        /// Code listings, one at a time.
        code: Arc<CodeWorker>,
        _code_guard: WorkerGuard<CodeSandbox, CodeJob, CodeReply>,
        code_busy: bool,
        /// The window after the one whose listing was asked last.
        next_code: usize,
        /// Picture structure reports, one at a time.
        data: Arc<DataWorker>,
        _data_guard: WorkerGuard<DataSandbox, DataJob, DataReply>,
        data_busy: bool,
        /// The window after the one whose report was asked last.
        next_data: usize,
        /// The instruction set this machine runs, which an `rxe` is read as.
        system: Option<Isa>,
    }

    /// What the editor keeps for each window.
    #[derive(Default)]
    struct Detect {
        /// The format its opening bytes name is still to be asked for.
        pending: bool,
    }

    type Window = DocWindow<TextEdit>;

    impl DocumentApp for TextEdit {
        type View = View;
        type AppView = NoAppView<Faces>;
        type Snapshot = Snapshot;
        type Extra = Detect;
        type Work = Work;
        type Answer = Answer;
        type Failure = Errno;

        const NAME: &'static str = APP_NAME;
        const WINDOW_SIZE: (u32, u32) = WINDOW_SIZE;
        /// A search and a conversion, each in flight with the newer ask that
        /// replaces it waiting, and the save in flight. A load runs alone:
        /// the window takes no input while one is out.
        const JOBS_PER_WINDOW: usize = 5;

        fn start(_: &Desktop, set: u64) -> Result<Self, i32> {
            let syntax = Arc::new(SyntaxWorker::new(
                serve_syntax,
                ParserSandbox::new(
                    RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Messages)
                        .serving(SYNTAX_SERVICE),
                    tairix_rt::LogSink,
                ),
                WorkerWake::create(),
            ));
            tairix_window::app::start_worker(APP_NAME, &syntax, set, APP_TOKEN, "syntax");
            let guard = WorkerGuard::new(&syntax);
            let code = Arc::new(CodeWorker::new(
                serve_code,
                ParserSandbox::new(
                    RtLauncher::own_binary(decode::WORKER_MEMORY).serving(CODE_SERVICE),
                    tairix_rt::LogSink,
                ),
                WorkerWake::create(),
            ));
            tairix_window::app::start_worker(APP_NAME, &code, set, CODE_TOKEN, "code");
            let code_guard = WorkerGuard::new(&code);
            let data = Arc::new(DataWorker::new(
                serve_data,
                ParserSandbox::new(
                    RtLauncher::own_binary(imageinspect::WORKER_MEMORY).serving(DATA_SERVICE),
                    tairix_rt::LogSink,
                ),
                WorkerWake::create(),
            ));
            tairix_window::app::start_worker(APP_NAME, &data, set, DATA_TOKEN, "data");
            let data_guard = WorkerGuard::new(&data);
            Ok(Self {
                syntax,
                _syntax_guard: guard,
                syntax_busy: false,
                next_syntax: 0,
                code,
                _code_guard: code_guard,
                code_busy: false,
                next_code: 0,
                data,
                _data_guard: data_guard,
                data_busy: false,
                next_data: 0,
                system: tairix_rt::boot_facts()
                    .ok()
                    .map(|facts| Isa::of_arch(facts.arch)),
            })
        }

        fn wakes(&self) -> Vec<(u64, Arc<dyn AnswerWake>)> {
            let syntax: Arc<dyn AnswerWake> = self.syntax.clone();
            let code: Arc<dyn AnswerWake> = self.code.clone();
            let data: Arc<dyn AnswerWake> = self.data.clone();
            alloc::vec![(APP_TOKEN, syntax), (CODE_TOKEN, code), (DATA_TOKEN, data)]
        }

        fn faces(theme: &Theme, scale: Scale) -> Faces {
            Faces {
                grid: BitmapFont::for_role(theme.fonts(), TextRole::Monospace, scale),
                status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale),
            }
        }

        fn damage_sink() -> Region {
            damage::sink()
        }

        fn write(
            job: &SaveJob<Handle, Snapshot>,
            _name: &str,
        ) -> Result<Option<&'static str>, Errno> {
            write_out(&job.target, &job.snapshot).map(|()| None)
        }

        fn work(work: Work) -> Answer {
            match work {
                Work::Load(mut load) => Answer::Loaded {
                    editor: load.step().map(|editor| editor.map(Box::new)),
                    load,
                },
                Work::Search(mut searching) => Answer::Searched {
                    step: searching.search.step(&*searching.snapshot, STEP_BYTES),
                    searching,
                },
                Work::Convert(mut converting) => {
                    let done = match converting.conversion.step(&converting.snapshot, STEP_BYTES) {
                        Converted::Partial => None,
                        Converted::Done(converted) => Some(converted),
                    };
                    Answer::Converted { converting, done }
                }
            }
        }

        /// A search or a conversion withdraws the one of its kind still
        /// waiting, so a window never has more than one of each outstanding.
        fn supersedes(work: &Work, waiting: &Work) -> bool {
            matches!(
                (work, waiting),
                (Work::Search(_), Work::Search(_)) | (Work::Convert(_), Work::Convert(_))
            )
        }

        fn untitled(host: &Host<Self>) -> Result<View, String> {
            Ok(blank(host, String::from(UNTITLED), Access::Untitled))
        }

        fn placeholder(host: &Host<Self>, name: &str, access: Access) -> Result<View, String> {
            Ok(blank(host, String::from(name), access))
        }

        fn load(host: &mut Host<Self>, index: usize, handle: Arc<Handle>, name: String) {
            let load = Load {
                handle,
                name: name.clone(),
                reading: None,
            };
            if !host.queue(index, Work::Load(load)) {
                host.not_opened(index, &name, &ReadFailure::NoMemory);
            }
        }

        fn collect(host: &mut Host<Self>) {
            while let Some(reply) = host.app.syntax.collect() {
                host.app.syntax_busy = false;
                adopt_syntax(host, reply);
            }
            while let Some(reply) = host.app.code.collect() {
                host.app.code_busy = false;
                adopt_code(host, reply);
            }
            while let Some(reply) = host.app.data.collect() {
                host.app.data_busy = false;
                adopt_data(host, reply);
            }
        }

        fn turn(host: &mut Host<Self>, now_ns: u64) -> bool {
            step_held_bars(host, now_ns);
            let syntax = submit_syntax(host, now_ns);
            let code = submit_code(host);
            let data = submit_data(host);
            syntax || code || data
        }

        /// When the soonest store check, or held scrollbar press, falls due.
        fn deadline(host: &mut Host<Self>, now_ns: u64) -> Option<u64> {
            host.windows
                .iter_mut()
                .filter(|window| !window.loading())
                .filter_map(|window| {
                    let check = window.view.check_due(now_ns).filter(|&due| due > now_ns);
                    let held = window.view.scroll_repeat_deadline(now_ns);
                    check.into_iter().chain(held).min()
                })
                .min()
        }

        fn pressure(host: &mut Host<Self>) {
            tairix_font::trim_glyph_cache();
            let band = tairix_rt::pressure::gauge().band();
            for window in &mut host.windows {
                window.view.editor_mut().adopt_pressure(band);
            }
        }

        fn request(host: &mut Host<Self>, index: usize, request: Own) {
            carry_out(host, index, request);
        }

        fn answered(host: &mut Host<Self>, index: usize, answer: Answer) {
            match answer {
                Answer::Loaded { load, editor: None } => {
                    let name = load.name.clone();
                    if !host.queue(index, Work::Load(load)) {
                        host.not_opened(index, &name, &ReadFailure::NoMemory);
                    }
                }
                Answer::Loaded {
                    load,
                    editor: Some(Ok(editor)),
                } => loaded(host, index, load, *editor),
                Answer::Loaded {
                    load,
                    editor: Some(Err(why)),
                } => host.not_opened(index, &load.name, &why),
                Answer::Searched { searching, step } => searched(host, index, searching, step),
                Answer::Converted { converting, done } => {
                    converted(host, index, converting, done);
                }
            }
        }

        fn render(
            &mut self,
            surface: &mut Surface,
            view: &View,
            layout: &Layout,
            (theme, scale, faces): (&Theme, Scale, Faces),
            focused: bool,
        ) {
            render_into(surface, view, layout, theme, scale, faces, focused);
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
    }

    /// An empty view of a document called `name`, pairing clicks at the
    /// desktop's interval.
    fn blank(host: &Host<TextEdit>, name: String, access: Access) -> View {
        View::new(
            Editor::new(Document::new(), Format::PlainText),
            name,
            access,
            host.desktop.info().double_click(),
        )
    }

    /// The whole of `load`'s document landed in window `index` as `editor`.
    fn loaded(host: &mut Host<TextEdit>, index: usize, load: Load, editor: Editor) {
        let access = host.windows[index].view.access();
        let unnamed = format_for_name(&load.name).is_none() && editor.mode() == Mode::Text;
        let view = View::new(
            editor,
            load.name,
            access,
            host.desktop.info().double_click(),
        );
        host.show(index, view, Some(load.handle));
        host.windows[index].extra.pending = unnamed;
    }

    /// Step each window's held scrollbar press whose repeat is due at
    /// `now_ns`, owing the window the paint it moved.
    fn step_held_bars(host: &mut Host<TextEdit>, now_ns: u64) {
        let (theme, scale) = (host.themes.active(), host.desktop.scale());
        for window in host.windows.iter_mut().filter(|window| !window.loading()) {
            window
                .view
                .scroll_repeat(now_ns, &window.layout, scale, theme, &mut window.damage);
            window.owe_reported();
        }
    }

    /// Hand the syntax worker the next job any window wants, answering
    /// whether the answer is already waiting.
    ///
    /// A window names the format of a document first, since colour depends on
    /// it; then colours what it shows; then checks a store whose edits have
    /// paused.
    fn submit_syntax(host: &mut Host<TextEdit>, now: u64) -> bool {
        if host.app.syntax_busy {
            return false;
        }
        let count = host.windows.len();
        for turn in 0..count {
            let index = (host.app.next_syntax + turn) % count;
            let window = &mut host.windows[index];
            let Some(work) = syntax_work(window, now) else {
                continue;
            };
            let job = SyntaxJob {
                stamp: window.stamp(),
                work,
            };
            host.app.next_syntax = (index + 1) % count;
            host.app.syntax_busy = true;
            return host.app.syntax.submit(job);
        }
        false
    }

    /// Hand the decode worker the next listing or manifest report any window
    /// wants, answering whether the answer is already waiting.
    ///
    /// A document past what one decode may carry is refused here rather than
    /// frozen for a worker that could only refuse it.
    fn submit_code(host: &mut Host<TextEdit>) -> bool {
        if host.app.code_busy {
            return false;
        }
        let count = host.windows.len();
        for turn in 0..count {
            let index = (host.app.next_code + turn) % count;
            let window = &mut host.windows[index];
            if window.loading() {
                continue;
            }
            let (id, work) = if let Some((id, isa)) = window.view.code_want() {
                let system = host.app.system;
                (id, CodeWork::List { isa, system })
            } else if window.view.data_structure() == Some(Structure::Manifest) {
                match window.view.data_want() {
                    Some((id, _)) => (id, CodeWork::Report),
                    None => continue,
                }
            } else {
                continue;
            };
            let snapshot = if window.view.editor().document().len() > MAX_INPUT {
                Err(match work {
                    CodeWork::List { .. } => Unlisted::too_large(),
                    CodeWork::Report => Structure::Manifest.too_large(),
                })
            } else {
                window
                    .view
                    .editor_mut()
                    .snapshot()
                    .map_err(|OutOfMemory| match work {
                        CodeWork::List { .. } => Unlisted::no_memory(),
                        CodeWork::Report => Structure::Manifest.no_memory(),
                    })
            };
            let snapshot = match snapshot {
                Ok((_, snapshot)) => snapshot,
                Err(unlisted) => {
                    let (layout, damage) = (&window.layout, &mut window.damage);
                    window.view.display_refused(unlisted, layout, damage);
                    window.owe_reported();
                    continue;
                }
            };
            let job = CodeJob {
                stamp: window.stamp(),
                id,
                snapshot,
                work,
            };
            host.app.next_code = (index + 1) % count;
            host.app.code_busy = true;
            return host.app.code.submit(job);
        }
        false
    }

    /// Hand the picture worker the next report any window wants, answering
    /// whether the answer is already waiting.
    ///
    /// A document past what one report may read is refused here rather than
    /// frozen for a worker that could only refuse it.
    fn submit_data(host: &mut Host<TextEdit>) -> bool {
        if host.app.data_busy {
            return false;
        }
        let count = host.windows.len();
        for turn in 0..count {
            let index = (host.app.next_data + turn) % count;
            let window = &mut host.windows[index];
            if window.loading() {
                continue;
            }
            if !matches!(window.view.data_structure(), Some(Structure::Picture(_))) {
                continue;
            }
            let Some((id, structure @ Structure::Picture(named))) = window.view.data_want() else {
                continue;
            };
            let snapshot = if window.view.editor().document().len() > MAX_DOCUMENT_BYTES {
                Err(structure.too_large())
            } else {
                window
                    .view
                    .editor_mut()
                    .snapshot()
                    .map_err(|OutOfMemory| structure.no_memory())
            };
            let snapshot = match snapshot {
                Ok((_, snapshot)) => snapshot,
                Err(unlisted) => {
                    let (layout, damage) = (&window.layout, &mut window.damage);
                    window.view.display_refused(unlisted, layout, damage);
                    window.owe_reported();
                    continue;
                }
            };
            let job = DataJob {
                stamp: window.stamp(),
                id,
                snapshot,
                named,
            };
            host.app.next_data = (index + 1) % count;
            host.app.data_busy = true;
            return host.app.data.submit(job);
        }
        false
    }

    /// Take in a report the picture worker read, for the document that asked
    /// for it: one for a document since gone is let go.
    fn adopt_data(host: &mut Host<TextEdit>, reply: DataReply) {
        let Some(index) = host.showing(reply.stamp) else {
            return;
        };
        let window = &mut host.windows[index];
        if window.loading() {
            return;
        }
        let (layout, damage) = (&window.layout, &mut window.damage);
        window
            .view
            .data_reported(reply.id, reply.report, layout, damage);
        window.owe_reported();
    }

    /// Take in what the decode worker answered, for the document that asked
    /// for it: one for a document since gone is let go.
    fn adopt_code(host: &mut Host<TextEdit>, reply: CodeReply) {
        let Some(index) = host.showing(reply.stamp) else {
            return;
        };
        let window = &mut host.windows[index];
        if window.loading() {
            return;
        }
        let (layout, damage) = (&window.layout, &mut window.damage);
        match reply.answer {
            CodeAnswer::Listing(listing) => {
                window.view.code_listed(reply.id, listing, layout, damage);
            }
            CodeAnswer::Report(report) => {
                window.view.data_reported(reply.id, report, layout, damage);
            }
        }
        window.owe_reported();
    }

    /// The syntax job `window` wants next, if any.
    fn syntax_work(window: &mut Window, now: u64) -> Option<SyntaxWork> {
        if window.loading() {
            return None;
        }
        if core::mem::take(&mut window.extra.pending) {
            let document = window.view.editor().document();
            let mut head = Vec::new();
            // A head the allocator refuses leaves the name's reading.
            if document
                .copy_range(0..document.len().min(MAX_HEAD_LEN), &mut head)
                .is_ok()
            {
                return Some(SyntaxWork::Detect(head));
            }
        }
        if let Some(job) = window.view.lex_job(&window.layout) {
            return Some(SyntaxWork::Lex(job));
        }
        if window.view.check_due(now).is_some_and(|due| due <= now) {
            let format = window.view.editor().format();
            // A store past the checker's bound is answered here, rather than
            // frozen whole for a worker that could only refuse it.
            if window.view.editor().document().len() > MAX_VALIDATE_LEN {
                let generation = window.view.editor().generation();
                let refused = alloc::vec![unchecked(format, SyntaxFailure::TooLarge)];
                let (layout, damage) = (&window.layout, &mut window.damage);
                window.view.checked(generation, refused, layout, damage);
                window.owe_reported();
            } else if let Ok((generation, snapshot)) = window.view.editor_mut().snapshot() {
                return Some(SyntaxWork::Validate {
                    generation,
                    format,
                    snapshot,
                });
            }
        }
        None
    }

    /// A search of window `index` went a step on. One of a document since
    /// changed is answered as that rather than scanned on to an answer nobody
    /// can use.
    fn searched(host: &mut Host<TextEdit>, index: usize, searching: Searching, step: Step) {
        let window = &mut host.windows[index];
        if !window.view.wants_search(searching.id) {
            return;
        }
        if step == Step::Partial && searching.generation == window.view.editor().generation() {
            if !host.queue(index, Work::Search(searching)) {
                host.windows[index].state("There is no room to go on searching");
            }
            return;
        }
        let outcome = window.view.found(
            searching.id,
            searching.generation,
            step,
            searching.replacement.as_deref(),
            &window.layout,
            &mut window.damage,
        );
        host.apply(index, outcome);
    }

    /// A conversion of window `index` went a step on, or through. One a newer
    /// ask has overtaken goes no further; one of a document since changed is
    /// put down, and the window says so.
    fn converted(
        host: &mut Host<TextEdit>,
        index: usize,
        converting: Converting,
        done: Option<Result<Option<Vec<Vec<u8>>>, OutOfMemory>>,
    ) {
        let window = &mut host.windows[index];
        if !window.view.wants_conversion(converting.id) {
            return;
        }
        let (id, generation, to) = (
            converting.id,
            converting.generation,
            converting.conversion.to(),
        );
        let chunks = match done {
            None if generation == window.view.editor().generation() => {
                if !host.queue(index, Work::Convert(converting)) {
                    host.windows[index].state("There is no room to go on converting");
                }
                return;
            }
            None => None,
            Some(Ok(chunks)) => chunks,
            Some(Err(OutOfMemory)) => {
                window.state("There is not enough memory to convert the line endings");
                return;
            }
        };
        let outcome = window.view.converted(
            id,
            generation,
            chunks,
            to,
            &window.layout,
            &mut window.damage,
        );
        host.apply(index, outcome);
    }

    /// Take in what the syntax worker answered, for the document that asked
    /// for it: one answered for a document since gone is let go.
    fn adopt_syntax(host: &mut Host<TextEdit>, reply: SyntaxReply) {
        let Some(index) = host.showing(reply.stamp) else {
            return;
        };
        let window = &mut host.windows[index];
        if window.loading() {
            return;
        }
        let (layout, damage) = (&window.layout, &mut window.damage);
        match reply.answer {
            SyntaxAnswer::Lexed {
                id,
                batch: Ok(batch),
            } => window.view.lexed(id, &batch, layout, damage),
            SyntaxAnswer::Lexed { id, batch: Err(_) } => window.view.lex_failed(id, layout, damage),
            SyntaxAnswer::Detected(Ok(Some(format))) => {
                let outcome = window.view.detected(format, layout, damage);
                host.apply(index, outcome);
                return;
            }
            SyntaxAnswer::Detected(Ok(None)) => {}
            SyntaxAnswer::Detected(Err(err)) => {
                host.report(alloc::format!(
                    "the document's format could not be read: {err}"
                ));
            }
            SyntaxAnswer::Validated {
                generation,
                format,
                diagnostics,
            } => {
                // An answer about a format the window no longer uses is about
                // another reading of the document.
                if format != window.view.editor().format() {
                    return;
                }
                let diagnostics = diagnostics.unwrap_or_else(|err| {
                    if err != SyntaxFailure::TooLarge {
                        tairix_window::app::report(
                            APP_NAME,
                            alloc::format!("a {} check failed: {err}", format.label()),
                        );
                    }
                    alloc::vec![unchecked(format, err)]
                });
                window.view.checked(generation, diagnostics, layout, damage);
            }
        }
        host.windows[index].owe_reported();
    }

    /// What the window states when its store could not be checked, so the
    /// document is not asked about again until it changes.
    fn unchecked(format: Format, err: SyntaxFailure) -> Diagnostic {
        let (severity, message) = if err == SyntaxFailure::TooLarge {
            (
                Severity::Error,
                alloc::format!("Too long to check as {}", format.label()),
            )
        } else {
            (
                Severity::Warning,
                String::from("This file could not be checked"),
            )
        };
        Diagnostic {
            line: None,
            severity,
            message,
        }
    }

    /// Carry out a request of window `index`'s that only this editor makes.
    fn carry_out(host: &mut Host<TextEdit>, index: usize, request: Own) {
        let window = &mut host.windows[index];
        let id = window.id();
        match request {
            Own::NewWindow => host.new_window(),
            Own::Copy(bytes) => {
                let kind = if core::str::from_utf8(&bytes).is_ok() {
                    ClipboardKind::Text
                } else {
                    ClipboardKind::Octets
                };
                if let Err(err) = clipboard::put(&mut host.client, id, kind, &bytes) {
                    window.state(alloc::format!("Could not copy: {err}"));
                }
            }
            Own::Paste => match clipboard::take(&mut host.client, id) {
                Ok(Some((_, bytes))) => {
                    let outcome = window
                        .view
                        .paste(&bytes, &window.layout, &mut window.damage);
                    host.apply(index, outcome);
                }
                Ok(None) => window.state("The clipboard is empty"),
                Err(err) => window.state(alloc::format!("Could not paste: {err}")),
            },
            Own::Search {
                id: search,
                search: pattern,
                replacement,
            } => match window.view.editor_mut().snapshot() {
                Ok((generation, snapshot)) => {
                    let work = Work::Search(Searching {
                        id: search,
                        generation,
                        snapshot,
                        search: pattern,
                        replacement,
                    });
                    if !host.queue(index, work) {
                        host.windows[index].state("There is no room to search");
                    }
                }
                Err(OutOfMemory) => window.state("There is not enough memory to search"),
            },
            Own::Convert { id: conversion, to } => match window.view.editor_mut().snapshot() {
                Ok((generation, snapshot)) => {
                    let work = Work::Convert(Converting {
                        id: conversion,
                        generation,
                        snapshot,
                        conversion: Conversion::new(to),
                    });
                    if !host.queue(index, work) {
                        host.windows[index].state("There is no room to convert");
                    }
                }
                Err(OutOfMemory) => window.state("There is not enough memory to convert"),
            },
        }
    }

    /// The editor's whole life.
    fn main() -> i32 {
        // The sandbox-worker role first: a document is untrusted input, so it
        // is parsed by a capability-empty child this binary is re-entered as,
        // which never becomes the editor.
        if worker_role() {
            return if serves(CODE_SERVICE) {
                serve_stdio(&mut DecodeService).exit_code()
            } else if serves(SYNTAX_SERVICE) {
                serve_stdio(&mut TextSyntaxService).exit_code()
            } else if serves(DATA_SERVICE) {
                serve_stdio(&mut InspectService::default()).exit_code()
            } else {
                WorkerExit::Unserved.code()
            };
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }
        docapp::run::<TextEdit>()
    }

    tairix_rt::entry!(main);
}

/// The host stub: this binary is a freestanding program on the Tier-1
/// targets, so on the host it exists only to keep the file covered by the
/// workspace build, clippy, and fmt.
#[cfg(not(freestanding))]
fn main() {}
