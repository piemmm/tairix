//! The `TextEdit.app` bundle's `Run` entry point: the desktop editor.
//!
//! The manifest requests no filesystem capability. Every document is the
//! user's own act — a descriptor a launcher opened for this program, or a
//! grant the session's trusted picker delegated — and every save writes
//! through that descriptor. Colouring, detecting and validating a document
//! parse untrusted bytes, so they run in a capability-empty worker this binary
//! is re-entered as.
//!
//! The editor is the host-tested engine ([`tairix_textedit`]). This binary
//! composes it over the live syscalls and keeps everything that waits off the
//! loop that owes the user a frame: a document worker reads, writes, searches
//! and converts, and a syntax worker drives the sandbox. The loop takes in
//! what has landed and what was typed, then paints each window once.
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
    use core::cell::Cell;
    use core::fmt;
    use core::ops::ControlFlow;

    use tairix_abi::fs::{FileKind, FileStat, FS_IO_MAX};
    use tairix_abi::input::KeyInput;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::window_ipc::{
        AppBar, AppBarClick, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuRow, ClipboardKind,
        CursorShape, DocumentName, MenuOutcome, PickPurpose, WindowEvent, WindowRegion,
    };
    use tairix_abi::{Errno, ProcId, DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, STDIN};
    use tairix_controls::damage;
    use tairix_font::BitmapFont;
    use tairix_geometry::{Point, Rect, Region, Scale};
    use tairix_help::{own_short_help, BundleHelp};
    use tairix_input::InputEvent;
    use tairix_raster::Surface;
    use tairix_rt::io::{Stderr, Stdout, Write};
    use tairix_sandbox::rt::{serve_stdio, worker_role, RtLauncher};
    use tairix_sandbox::textsyntax::{
        detect, lex_lines, validate_document, LexedBatch, SyntaxFailure, TextSyntaxService,
        MAX_HEAD_LEN, MAX_VALIDATE_LEN,
    };
    use tairix_sandbox::{ParserSandbox, ServeEnd};
    use tairix_syntax::{Diagnostic, Format, Severity};
    use tairix_textedit::detect::format_for_name;
    use tairix_textedit::document::{Document, OutOfMemory, Snapshot, STEP_BYTES};
    use tairix_textedit::editor::{Conversion, Converted, Editor, Mode};
    use tairix_textedit::file::{FileState, PickFor, SaveJob, SaveStep};
    use tairix_textedit::find::{Search, Step};
    use tairix_textedit::highlight::LexJob;
    use tairix_textedit::layout::{Faces, Layout, WINDOW_SIZE};
    use tairix_textedit::load::{ReadStep, Reading};
    use tairix_textedit::paint::render_into;
    use tairix_textedit::view::{Access, Outcome, Request, View, UNTITLED};
    use tairix_theme::{TextRole, Theme, ThemeRegistry};
    use tairix_util::defer::JobQueue;
    use tairix_window::app::{self, Wake, WindowPane};
    use tairix_window::{
        clipboard, key_input_event, pointer_input_events, pointer_point, present_damage, Desktop,
        EventDrain, EventError, EventMailbox, EventSource, Parked, Repaint, Target, WindowClient,
        WindowEvents, WindowSizing, QUIT_ROW,
    };

    /// The name this program's bundle, help and refusals go by.
    const APP_NAME: &str = "TextEdit";

    /// The wait-set token of the document worker's answer wake.
    const DOCUMENT_TOKEN: u64 = app::FIRST_APP_TOKEN;

    /// The wait-set token of the syntax worker's answer wake.
    const SYNTAX_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;

    /// The icon-bar row that opens a new window, numbered past the
    /// convention's own so the two never collide.
    const NEW_WINDOW_ROW: u16 = QUIT_ROW + 1;

    type Client = WindowClient<app::RtWindowTransport>;

    /// State the abnormal-exit reason on `stderr` and hand `code` back.
    fn fail(code: i32, reason: &str) -> i32 {
        report(reason);
        code
    }

    /// State a bring-up refusal the shared shell reported.
    fn fail_shell(err: app::ShellError) -> i32 {
        fail(err.code(), &alloc::format!("{err}"))
    }

    /// Report something the user should know that is not fatal.
    fn report(reason: &str) {
        let _ = writeln!(Stderr, "{APP_NAME}: {reason}");
    }

    /// The descriptor a document is read from and saved through, whether it
    /// was cloned in at spawn or redeemed from a grant: it closes once its
    /// last holder — the window, or a save still being written — lets go.
    type Handle = tairix_rt::File;

    /// Why a document could not be opened.
    #[derive(Copy, Clone, Debug)]
    enum LoadRefusal {
        Unreadable(Errno),
        NotAFile,
        TooLarge,
    }

    impl fmt::Display for LoadRefusal {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Unreadable(err) => write!(f, "it could not be read ({err})"),
                Self::NotAFile => f.write_str("it is not a file"),
                Self::TooLarge => f.write_str("there is not enough memory to hold it"),
            }
        }
    }

    /// How long the regular file open at `fd` measures.
    ///
    /// One `fs_stat`, which the descriptor's own backing authorises, so a
    /// holder with no filesystem capability can describe what it was handed.
    fn regular_length(fd: u32) -> Result<u64, LoadRefusal> {
        let mut record = [0u8; FileStat::WIRE_LEN];
        let read = tairix_rt::fs_stat_raw(fd, &mut record)
            .map_err(|raw| LoadRefusal::Unreadable(Errno::from_syscall(raw)))?;
        if read < FileStat::WIRE_LEN {
            return Err(LoadRefusal::Unreadable(Errno::BufferTooSmall));
        }
        let stat = FileStat::decode(&record).map_err(LoadRefusal::Unreadable)?;
        if stat.kind != FileKind::Regular {
            return Err(LoadRefusal::NotAFile);
        }
        Ok(stat.size)
    }

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
        fn step(&mut self) -> Option<Result<Editor, LoadRefusal>> {
            let fd = self.handle.fd();
            let reading = match &mut self.reading {
                Some(reading) => reading,
                None => match regular_length(fd) {
                    Ok(length) => self.reading.insert(Reading::new(length)),
                    Err(why) => return Some(Err(why)),
                },
            };
            let chunks = match reading.step(STEP_BYTES, FS_IO_MAX, |offset, into| {
                tairix_rt::fs_read_full(fd, offset, into)
            }) {
                ReadStep::Partial => return None,
                ReadStep::Done(chunks) => chunks,
                ReadStep::Refused(err) => return Some(Err(LoadRefusal::Unreadable(err))),
                ReadStep::NoMemory => return Some(Err(LoadRefusal::TooLarge)),
            };
            let format = format_for_name(&self.name).unwrap_or(Format::PlainText);
            Some(
                Document::from_chunks(chunks)
                    .map(|document| Editor::new(document, format))
                    .map_err(|OutOfMemory| LoadRefusal::TooLarge),
            )
        }
    }

    /// Write `snapshot` over the file open at `fd`, cut the file to its
    /// length, and make it durable.
    ///
    /// The pieces are gathered into runs of what one call moves; a run the
    /// allocator refuses writes piece by piece instead: slower, never wrong.
    fn write_out(fd: u32, snapshot: &Snapshot) -> Result<(), Errno> {
        let mut run = Vec::new();
        let _ = run.try_reserve_exact(snapshot.len().min(FS_IO_MAX));
        let mut offset = 0u64;
        snapshot.gather(&mut run, |bytes| {
            tairix_rt::fs_write_all(fd, offset, bytes)?;
            offset = offset.saturating_add(bytes.len() as u64);
            Ok(())
        })?;
        status(tairix_rt::fs_truncate(fd, offset))?;
        status(tairix_rt::fs_sync(fd))
    }

    /// A kernel status result as a `Result`.
    fn status(ret: i64) -> Result<(), Errno> {
        if ret < 0 {
            Err(Errno::from_syscall(ret))
        } else {
            Ok(())
        }
    }

    /// A job for the document worker, and the window it is for.
    struct DocumentJob {
        window: u64,
        work: DocumentWork,
    }

    enum DocumentWork {
        /// Read one more step of a document in.
        Load(Load),
        /// Write a save out; `name` is what the document was called, for a
        /// refusal stated once its window has gone, and `grew` whether the
        /// queue grew its room to take it.
        Save {
            job: SaveJob<Handle>,
            name: String,
            grew: bool,
        },
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

    /// A line-ending conversion under way, over the document it began on.
    struct Converting {
        generation: u64,
        snapshot: Arc<Snapshot>,
        conversion: Conversion,
    }

    impl DocumentWork {
        /// A save is carried out whatever becomes of the window that asked.
        const fn is_save(&self) -> bool {
            matches!(self, Self::Save { .. })
        }
    }

    struct DocumentReply {
        window: u64,
        answer: DocumentAnswer,
    }

    enum DocumentAnswer {
        /// A load went a step on; `editor` once the whole document is in.
        Loaded {
            load: Load,
            editor: Option<Result<Box<Editor>, LoadRefusal>>,
        },
        Saved {
            target: Arc<Handle>,
            generation: u64,
            rename: Option<String>,
            name: String,
            grew: bool,
            result: Result<(), Errno>,
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

    /// The job travels in an `Option` so the worker takes it by value and
    /// hands back what it read and stepped without copying any of it.
    ///
    /// Each job is answered in turn: two saves of one file must reach it in
    /// the order asked, and every job stands for something a window awaits.
    type DocumentWorker = tairix_rt::work::Worker<
        (),
        Option<DocumentJob>,
        Option<DocumentReply>,
        JobQueue<Option<DocumentJob>, Option<DocumentReply>>,
    >;

    /// Room the document queue holds for each window: a load, a search, a
    /// conversion — each kept to one by withdrawing what a newer ask replaces
    /// — and its save in flight with the one a close writes behind it.
    ///
    /// A containment bound derived from what a window can have outstanding,
    /// not a capacity: the queue grows by it as windows open.
    const JOBS_PER_WINDOW: usize = 5;

    fn serve_document(_: &mut (), job: &mut Option<DocumentJob>) -> Option<DocumentReply> {
        let DocumentJob { window, work } = job.take()?;
        let answer = match work {
            DocumentWork::Load(mut load) => DocumentAnswer::Loaded {
                editor: load.step().map(|editor| editor.map(Box::new)),
                load,
            },
            DocumentWork::Save { job, name, grew } => DocumentAnswer::Saved {
                result: write_out(job.target.fd(), &job.snapshot),
                target: job.target,
                generation: job.generation,
                rename: job.rename,
                name,
                grew,
            },
            DocumentWork::Search(mut searching) => DocumentAnswer::Searched {
                step: searching.search.step(&*searching.snapshot, STEP_BYTES),
                searching,
            },
            DocumentWork::Convert(mut converting) => {
                let done = match converting.conversion.step(&converting.snapshot, STEP_BYTES) {
                    Converted::Partial => None,
                    Converted::Done(converted) => Some(converted),
                };
                DocumentAnswer::Converted { converting, done }
            }
        };
        Some(DocumentReply { window, answer })
    }

    type SyntaxSandbox = ParserSandbox<RtLauncher, tairix_rt::LogSink>;

    struct SyntaxJob {
        window: u64,
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
        window: u64,
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

    type SyntaxWorker = tairix_rt::work::Worker<SyntaxSandbox, SyntaxJob, SyntaxReply>;

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
            window: job.window,
            answer,
        }
    }

    /// `snapshot` as one run of bytes, for a store short enough to check.
    fn materialise(snapshot: &Snapshot) -> Result<Vec<u8>, SyntaxFailure> {
        if snapshot.len() > MAX_VALIDATE_LEN {
            return Err(SyntaxFailure::TooLarge);
        }
        let mut text = Vec::new();
        text.try_reserve_exact(snapshot.len())
            .map_err(|_| SyntaxFailure::TooLarge)?;
        snapshot.walk(0, |bytes| {
            text.extend_from_slice(bytes);
            ControlFlow::Continue(())
        });
        Ok(text)
    }

    /// Where a window's document stands.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum Phase {
        /// Being read in: input waits until it lands.
        Loading,
        /// Open, and the format its opening bytes name is still to be asked
        /// for.
        Unnamed,
        Open,
    }

    struct Window {
        pane: WindowPane,
        /// Held for the window's life, so a clipped repaint leaves the pixels
        /// outside the clip alone.
        surface: Surface,
        view: View,
        layout: Layout,
        /// Where the document came from and saves to, its saves, and a
        /// chooser open for it.
        file: FileState<Handle>,
        phase: Phase,
        /// The menu open over it, so an outcome is matched to the gesture
        /// that asked for it.
        menu: Option<u64>,
        /// The session refused its last present, which has been reported.
        present_refused: bool,
        /// The title the session shows.
        title: String,
        /// The title as it now reads, written here before it is compared
        /// with the one shown so a paint builds no title of its own.
        title_draft: String,
        shape: CursorShape,
        focused: bool,
        owed: Repaint,
        damage: Region,
    }

    impl Window {
        fn id(&self) -> u64 {
            self.pane.id()
        }

        fn owe(&mut self, repaint: Repaint) {
            self.owed = self.owed.merged(repaint);
        }

        /// Owe what the engine reported, when it reported anything: a round
        /// that changed nothing on screen paints nothing.
        fn owe_reported(&mut self) {
            self.owe(Repaint::reported_if(!self.damage.is_empty()));
        }

        /// Lay the window out again for its size, bring the view into line
        /// with it, and owe the whole window.
        fn relayout(&mut self, theme: &Theme, scale: Scale, faces: Faces) {
            let mode = *self.pane.mode();
            self.layout = self
                .view
                .layout(mode.width_px, mode.height_px, theme, scale, faces);
            self.view.settle(&self.layout, &mut self.damage);
            self.owe(Repaint::Whole);
        }

        /// What a new document may be opened into in place.
        fn pristine(&self) -> bool {
            self.phase != Phase::Loading && self.file.pristine(&self.view)
        }

        /// Paint what the window owes and present it.
        fn paint(
            &mut self,
            client: &mut Client,
            theme: &Theme,
            scale: Scale,
            faces: Faces,
        ) -> Result<(), Errno> {
            let owed = core::mem::replace(&mut self.owed, Repaint::Nothing);
            if owed == Repaint::Nothing {
                return Ok(());
            }
            self.view.write_title(&mut self.title_draft);
            if self.title_draft != self.title
                && client.set_title(self.id(), &self.title_draft).is_ok()
            {
                core::mem::swap(&mut self.title, &mut self.title_draft);
            }
            let repaint = if self.pane.content_released() {
                Repaint::Whole
            } else {
                owed
            };
            let mode = *self.pane.mode();
            let Some(area) = present_damage(&mode, repaint, &self.damage) else {
                self.damage.clear();
                return Ok(());
            };
            let (view, layout, focused) = (&self.view, &self.layout, self.focused);
            // Each reported rectangle is painted under its own clip, so a
            // keystroke rasterises its row and the status band rather than
            // everything their bounding box spans.
            let whole = [Rect::new(0, 0, mode.width_px, mode.height_px)];
            let parts = if repaint == Repaint::Reported && !self.damage.is_empty() {
                self.damage.rects()
            } else {
                &whole
            };
            for part in parts {
                let (Ok(x), Ok(y)) = (u32::try_from(part.left()), u32::try_from(part.top())) else {
                    continue;
                };
                self.surface
                    .with_clip(x, y, part.width, part.height, |clipped| {
                        render_into(clipped, view, layout, theme, scale, faces, focused);
                    });
            }
            self.damage.clear();
            self.pane.present(client, &self.surface, area)
        }

        /// Ask the session to show `shape` over the window when it is not
        /// already showing. A refusal is not asked again until the shape
        /// wanted changes.
        fn show_shape(&mut self, client: &mut Client, shape: CursorShape) {
            if shape != self.shape {
                self.shape = shape;
                let _ = client.set_cursor(self.id(), shape);
            }
        }
    }

    /// The shape the pointer takes at `at`: the I-beam over text that takes a
    /// caret, the arrow over everything else.
    fn shape_at(window: &Window, at: Point) -> CursorShape {
        let layout = &window.layout;
        let text = [layout.grid(), layout.find_field(), layout.replace_field()];
        if window.phase != Phase::Loading
            && window.view.modal().is_none()
            && text.iter().any(|rect| rect.contains(at))
        {
            CursorShape::Text
        } else {
            CursorShape::Arrow
        }
    }

    /// The faces the window's text is set in at `scale`.
    fn faces(theme: &Theme, scale: Scale) -> Faces {
        Faces {
            grid: BitmapFont::for_role(theme.fonts(), TextRole::Monospace, scale),
            status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale),
        }
    }

    /// Everything the loop owns: each field the one copy the process holds.
    struct App {
        client: Client,
        windows: Vec<Window>,
        /// Reads, writes, searches and conversions, each answered in turn.
        documents: Arc<DocumentWorker>,
        /// Colouring, format naming and store checks, one at a time.
        syntax: Arc<SyntaxWorker>,
        syntax_busy: bool,
        /// The window after the one whose syntax job ran last, so no window
        /// can starve another's colouring.
        next_syntax: usize,
        /// A job ran on the loop for want of a worker, so its answer is
        /// already waiting to be taken in.
        answered: bool,
        /// Saves asked for and not yet landed, whichever window asked: the
        /// process does not end under one.
        saves: usize,
        event_endpoint: u64,
        server: ProcId,
        desktop: Desktop,
        themes: ThemeRegistry,
        faces: Faces,
        /// Quit was chosen: the process ends once every window has closed and
        /// no save is left to land.
        quitting: bool,
    }

    impl App {
        fn index_of(&self, id: u64) -> Option<usize> {
            self.windows.iter().position(|window| window.id() == id)
        }

        /// Hand `work` for window `window` to the document worker, answering
        /// whether it was taken.
        ///
        /// A search or a conversion withdraws the one of its kind still
        /// waiting for the window, which it supersedes, so a window never has
        /// more than one of each outstanding. A save is never turned away
        /// while the memory for it can be had.
        fn queue(&mut self, window: u64, work: DocumentWork) -> bool {
            let kind = core::mem::discriminant(&work);
            if matches!(work, DocumentWork::Search(_) | DocumentWork::Convert(_)) {
                self.documents.retain_waiting(|job| {
                    job.as_ref().is_none_or(|job| {
                        job.window != window || core::mem::discriminant(&job.work) != kind
                    })
                });
            }
            let save = work.is_save();
            let mut job = Some(DocumentJob { window, work });
            loop {
                let refused = match self.documents.submit(job) {
                    Ok(answered) => {
                        self.answered |= answered;
                        self.saves += usize::from(save);
                        return true;
                    }
                    Err(refused) => refused,
                };
                // Room grown for a save is given back when that save lands.
                let mut refused = refused;
                let Some(DocumentJob {
                    work: DocumentWork::Save { grew, .. },
                    ..
                }) = &mut refused
                else {
                    return false;
                };
                if *grew {
                    self.documents.shrink(1);
                    return false;
                }
                if self.documents.grow(1).is_err() {
                    return false;
                }
                *grew = true;
                job = refused;
            }
        }

        /// State `message` where the user will see it: in the window with the
        /// keyboard, else the newest, else a new one — and on `stderr`.
        fn tell(&mut self, message: String) {
            report(&message);
            if self.windows.is_empty() {
                self.new_window();
            }
            let index = self
                .windows
                .iter()
                .position(|window| window.focused)
                .or_else(|| self.windows.len().checked_sub(1));
            if let Some(index) = index {
                state(&mut self.windows[index], message);
            }
        }

        /// Open a window showing `view`, answering its index.
        ///
        /// A refusal is stated and answers `None`: the editor carries on with
        /// the windows it has, and is still on the icon bar.
        fn open_window(&mut self, view: View, phase: Phase) -> Option<usize> {
            let theme = self.themes.active();
            let scale = self.desktop.scale();
            let (width, height) = self.desktop.window_size(WINDOW_SIZE.0, WINDOW_SIZE.1);
            let mode = app::mode_for(width, height);
            let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
                report("no drawing surface; no window opened");
                return None;
            };
            if self.documents.grow(JOBS_PER_WINDOW).is_err() {
                report("no room for another window's work; no window opened");
                return None;
            }
            let least = Layout::min_size(theme, scale, self.faces);
            let sizing = WindowSizing::Resizable {
                min_width_px: least.0,
                min_height_px: least.1,
                max_width_px: 0,
                max_height_px: 0,
            };
            let mut title = String::new();
            view.write_title(&mut title);
            let (pane, replied) = match WindowPane::open(
                &mut self.client,
                self.event_endpoint,
                &mode,
                &title,
                sizing,
            ) {
                Ok(opened) => opened,
                Err(err) => {
                    self.documents.shrink(JOBS_PER_WINDOW);
                    report(&alloc::format!("{err}; no window opened"));
                    return None;
                }
            };
            // A reply from any other sender is something else answering for
            // the window endpoint.
            if replied != self.server {
                let _ = pane.close(&mut self.client);
                self.documents.shrink(JOBS_PER_WINDOW);
                report("a window reply came from another sender; no window opened");
                return None;
            }
            let layout = view.layout(mode.width_px, mode.height_px, theme, scale, self.faces);
            let mut window = Window {
                pane,
                surface,
                view,
                layout,
                file: FileState::new(),
                phase,
                menu: None,
                present_refused: false,
                title,
                title_draft: String::new(),
                shape: CursorShape::Arrow,
                focused: true,
                owed: Repaint::Whole,
                damage: damage::sink(),
            };
            window.view.settle(&window.layout, &mut window.damage);
            self.windows.push(window);
            Some(self.windows.len() - 1)
        }

        /// An empty view of a document called `name`, pairing clicks at the
        /// desktop's interval.
        fn blank_view(&self, name: String, access: Access) -> View {
            View::new(
                Editor::new(Document::new(), Format::PlainText),
                name,
                access,
                self.desktop.info().double_click(),
            )
        }

        /// Open a new, empty window.
        fn new_window(&mut self) {
            let _ = self.open_window(
                self.blank_view(String::from(UNTITLED), Access::Untitled),
                Phase::Open,
            );
        }

        /// Read `handle`'s document, called `name`, into a window: `into`
        /// when the user opened it from a pristine window, a new one
        /// otherwise. The window shows that it is opening and takes no input
        /// until the document lands.
        fn open_document(
            &mut self,
            handle: Handle,
            name: String,
            writable: bool,
            into: Option<usize>,
        ) {
            let access = if writable {
                Access::Writable
            } else {
                Access::ReadOnly
            };
            let mut placeholder = self.blank_view(name.clone(), access);
            placeholder.say(alloc::format!("Opening {name}\u{2026}"));
            let index = match into {
                Some(index) => {
                    let window = &mut self.windows[index];
                    window.view = placeholder;
                    window.phase = Phase::Loading;
                    let (theme, scale) = (self.themes.active(), self.desktop.scale());
                    window.relayout(theme, scale, self.faces);
                    index
                }
                None => match self.open_window(placeholder, Phase::Loading) {
                    Some(index) => index,
                    None => return,
                },
            };
            let window = self.windows[index].id();
            let load = Load {
                handle: Arc::new(handle),
                name: name.clone(),
                reading: None,
            };
            if !self.queue(window, DocumentWork::Load(load)) {
                loaded(self, index, None, name, Err(LoadRefusal::TooLarge));
            }
        }

        /// Close window `index`, its menu and any pick going with it. The
        /// work it asked for goes with it — all but its saves, which still
        /// land, and the saves asked for behind the one in flight, written now.
        ///
        /// Each save it leaves behind keeps one room in the queue until it
        /// lands; the rest of its share is given up now.
        fn close_window(&mut self, index: usize) {
            let mut window = self.windows.remove(index);
            let id = window.id();
            self.documents.retain_waiting(|job| {
                job.as_ref()
                    .is_none_or(|job| job.window != id || job.work.is_save())
            });
            let in_flight = usize::from(window.file.saving());
            let flushed = window.file.close(&window.view);
            let held = in_flight + flushed.len();
            let room =
                if held > JOBS_PER_WINDOW && self.documents.grow(held - JOBS_PER_WINDOW).is_err() {
                    JOBS_PER_WINDOW
                } else {
                    self.documents.shrink(JOBS_PER_WINDOW.saturating_sub(held));
                    held
                };
            let name = String::from(window.view.name());
            for (queued, job) in flushed.into_iter().enumerate() {
                let fits = in_flight + queued < room;
                let save = DocumentWork::Save {
                    job,
                    name: name.clone(),
                    grew: false,
                };
                if !(fits && self.queue(id, save)) {
                    if fits {
                        self.documents.shrink(1);
                    }
                    report(&alloc::format!(
                        "{name} could not be saved: there is no room for the save"
                    ));
                }
            }
            let _ = window.pane.close(&mut self.client);
        }
    }

    /// The park: the event mailbox, both workers' answer wakes, the
    /// memory-pressure band, the desktop state, and the check deadline.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        documents: &'a DocumentWorker,
        syntax: &'a SyntaxWorker,
        /// When the next store check falls due, or `None`, in which case the
        /// park arms no timer.
        deadline_ns: &'a Cell<Option<u64>>,
        pressure: &'a Cell<bool>,
        desktop_moved: &'a Cell<bool>,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let woken = match self.deadline_ns.get() {
                Some(deadline) => match app::park_until(self.set, deadline)? {
                    Some(woken) => woken,
                    None => return Ok(Parked::Interrupted),
                },
                None => app::park(self.set)?,
            };
            // A worker's readiness is a level peek: left undrained it would
            // report ready for ever and turn the park into a spin.
            match woken {
                Wake::App(DOCUMENT_TOKEN) => {
                    self.documents.wake().drain();
                    Ok(Parked::Interrupted)
                }
                Wake::App(SYNTAX_TOKEN) => {
                    self.syntax.wake().drain();
                    Ok(Parked::Interrupted)
                }
                Wake::PressureChanged => {
                    tairix_font::trim_glyph_cache();
                    self.pressure.set(true);
                    Ok(Parked::Interrupted)
                }
                Wake::DesktopChanged => {
                    self.desktop_moved.set(true);
                    Ok(Parked::Interrupted)
                }
                Wake::Event | Wake::PressureUnchanged | Wake::App(_) => Ok(Parked::Served),
            }
        }
    }

    /// Print the bundle's own short help and answer the exit code.
    fn print_help() -> i32 {
        let locale = tairix_rt::env_var(b"LANG").and_then(|raw| core::str::from_utf8(raw).ok());
        let Some(bytes) = own_short_help(&BundleHelp::new(APP_NAME), locale, APP_NAME) else {
            return fail(1, "this bundle's help documents could not be read");
        };
        match Stdout.write_all(&bytes) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }

    /// The document this program was handed on [`STDIN`] at spawn, its name,
    /// and whether it may be written, when it was launched with one.
    fn launched_document() -> Option<(Handle, String, bool)> {
        let writable = match tairix_rt::arg(1)? {
            arg if arg == DOCUMENT_WRITABLE_ROLE_ARG => true,
            arg if arg == DOCUMENT_ROLE_ARG => false,
            _ => return None,
        };
        let name = tairix_rt::arg(2)
            .and_then(|raw| core::str::from_utf8(raw).ok())
            .map(|path| String::from(path.rsplit('/').next().unwrap_or(path)))
            .unwrap_or_default();
        Some((Handle::adopt(STDIN), name, writable))
    }

    /// This editor's icon-bar presence: a click opens a window when none is
    /// open, and its menu offers another.
    fn app_bar(endpoint: u64) -> Result<AppBar, Errno> {
        let new_window = AppMenuItem::new(
            AppMenuItemId::new(NEW_WINDOW_ROW)?,
            AppMenuLabel::new("New window")?,
        );
        tairix_window::declaration(
            endpoint,
            AppBarClick::RaiseOrOpen,
            &[AppMenuRow::Item(new_window)],
        )
    }

    /// Start `worker` and put its answer wake on `set` under `token`.
    fn start_worker<S, Req, Ans, D>(
        worker: &Arc<tairix_rt::work::Worker<S, Req, Ans, D>>,
        set: u64,
        token: u64,
        what: &str,
    ) -> Result<(), i32>
    where
        S: Send + 'static,
        Req: Send + 'static,
        Ans: Send + 'static,
        D: tairix_rt::work::Desk<Req, Ans> + Send + 'static,
    {
        if let Err(reason) = tairix_rt::work::Worker::start(worker) {
            report(&alloc::format!(
                "no {what} worker ({reason:?}); its work runs on the event loop"
            ));
        }
        app::watch_wake(set, worker.wake(), token).map_err(|err| {
            fail(
                app::EXIT_NO_EVENTS,
                &alloc::format!("{what} answer wake refused ({err})"),
            )
        })
    }

    /// The editor's whole life.
    #[allow(
        clippy::too_many_lines,
        reason = "one linear bring-up and one loop; splitting the loop would part the park from the drain it follows"
    )]
    fn main() -> i32 {
        // The sandbox-worker role first: a document is untrusted input, so it
        // is parsed by a capability-empty child this binary is re-entered as,
        // which never becomes the editor.
        if worker_role() {
            return match serve_stdio(&mut TextSyntaxService) {
                ServeEnd::Finished | ServeEnd::Ended => 0,
                ServeEnd::Failed(_) => 1,
            };
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return print_help();
        }
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);

        let mut client = WindowClient::new(app::RtWindowTransport);
        let (desktop, themes) = match app::bring_up_desktop(&mut client) {
            Ok(pair) => pair,
            Err(err) => return fail_shell(err),
        };
        let Some(server) = client.session() else {
            return fail(app::EXIT_NO_WINDOW, "the desktop did not identify itself");
        };
        let binding = match app::bind_event_mailbox() {
            Ok(binding) => binding,
            Err(err) => return fail_shell(err),
        };
        let (event_endpoint, set) = (binding.endpoint(), binding.set());

        // The queue's room grows with each window opened.
        let Ok(documents) =
            DocumentWorker::queued(serve_document, (), tairix_rt::sync::WorkerWake::create(), 0)
        else {
            return fail(app::EXIT_NO_EVENTS, "no room for the document queue");
        };
        let documents = Arc::new(documents);
        let syntax = Arc::new(SyntaxWorker::new(
            serve_syntax,
            ParserSandbox::new(RtLauncher::own_binary(), tairix_rt::LogSink),
            tairix_rt::sync::WorkerWake::create(),
        ));
        if let Err(code) = start_worker(&documents, set, DOCUMENT_TOKEN, "document") {
            return code;
        }
        if let Err(code) = start_worker(&syntax, set, SYNTAX_TOKEN, "syntax") {
            return code;
        }
        let _document_guard = tairix_rt::work::WorkerGuard::new(&documents);
        let _syntax_guard = tairix_rt::work::WorkerGuard::new(&syntax);

        if let Err(refused) = tairix_window::declare_app_bar(&mut client, app_bar(event_endpoint)) {
            report(&alloc::format!("{refused}"));
        }

        let faces = faces(themes.active(), desktop.scale());
        let mut app = App {
            client,
            windows: Vec::new(),
            documents: Arc::clone(&documents),
            syntax: Arc::clone(&syntax),
            syntax_busy: false,
            next_syntax: 0,
            answered: false,
            saves: 0,
            event_endpoint,
            server,
            desktop,
            themes,
            faces,
            quitting: false,
        };
        match launched_document() {
            Some((handle, name, writable)) => app.open_document(handle, name, writable, None),
            None => app.new_window(),
        }

        let deadline = Cell::new(None);
        let pressure = Cell::new(false);
        let desktop_moved = Cell::new(false);
        let mut events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(event_endpoint, server),
            set,
            documents: &documents,
            syntax: &syntax,
            deadline_ns: &deadline,
            pressure: &pressure,
            desktop_moved: &desktop_moved,
        });

        loop {
            // Take in everything that has landed and everything queued before
            // painting, so a burst costs one frame.
            app.answered = false;
            while let Some(answer) = documents.collect() {
                if let Some(reply) = answer {
                    adopt_document(&mut app, reply);
                }
            }
            while let Some(reply) = syntax.collect() {
                app.syntax_busy = false;
                adopt_syntax(&mut app, reply);
            }
            if pressure.replace(false) {
                let band = tairix_rt::pressure::gauge().band();
                for window in &mut app.windows {
                    window.view.editor_mut().adopt_pressure(band);
                }
            }
            if desktop_moved.replace(false) {
                adopt_desktop(&mut app);
            }
            loop {
                match events.try_wait(&mut app.client) {
                    Ok(Some(event)) => route(&mut app, &event),
                    Ok(None) => break,
                    Err(EventError::Mailbox(_)) => {
                        return leave(
                            &mut app,
                            fail(app::EXIT_CHANNEL_LOST, "the event channel died"),
                        )
                    }
                    Err(EventError::Undecodable(_)) => {
                        report("a malformed window event was refused");
                    }
                }
            }

            let now = tairix_rt::clock_get();
            let answered = submit_syntax(&mut app, now) | app.answered;
            paint_all(&mut app);
            if app.quitting && app.windows.is_empty() && app.saves == 0 {
                return 0;
            }
            if answered {
                // A job carried out here for want of a worker: its answer is
                // already on the desk.
                continue;
            }

            deadline.set(next_check(&mut app, now));
            match events.wait(&mut app.client) {
                Ok(Some(event)) => route(&mut app, &event),
                Ok(None) => {}
                Err(EventError::Mailbox(_)) => {
                    return leave(
                        &mut app,
                        fail(app::EXIT_CHANNEL_LOST, "the event channel died"),
                    )
                }
                Err(EventError::Undecodable(_)) => report("a malformed window event was refused"),
            }
        }
    }

    /// End with `code` once every save asked for has landed, stating any
    /// that failed: a process ending mid-write leaves a file part new, part
    /// old. The work nobody can now see is withdrawn first.
    fn leave(app: &mut App, code: i32) -> i32 {
        // Closing each window queues the saves chained behind its save in
        // flight, which would otherwise never be written.
        while let Some(last) = app.windows.len().checked_sub(1) {
            app.close_window(last);
        }
        app.documents
            .retain_waiting(|job| job.as_ref().is_some_and(|job| job.work.is_save()));
        while let Some(answer) = app.documents.wait() {
            if let Some(DocumentReply {
                answer:
                    DocumentAnswer::Saved {
                        name,
                        result: Err(err),
                        ..
                    },
                ..
            }) = answer
            {
                report(&alloc::format!("{name} could not be saved ({err})"));
            }
        }
        code
    }

    /// Present what every window owes. A present the session refuses is that
    /// window's trouble alone: it is said once, and the window paints whole
    /// at its next chance, while every other window carries on.
    fn paint_all(app: &mut App) {
        let theme = app.themes.active();
        let scale = app.desktop.scale();
        for window in &mut app.windows {
            match window.paint(&mut app.client, theme, scale, app.faces) {
                Ok(()) => window.present_refused = false,
                Err(err) => {
                    if !window.present_refused {
                        report(&alloc::format!(
                            "{} could not be shown ({err}); it is drawn again at its next change",
                            window.view.name()
                        ));
                    }
                    window.present_refused = true;
                    window.owe(Repaint::Whole);
                }
            }
        }
    }

    /// Adopt a new desktop state: every window restyled at its density.
    fn adopt_desktop(app: &mut App) {
        match app::adopt_desktop(&mut app.desktop, &mut app.themes) {
            Ok(true) => {
                let theme = app.themes.active();
                let scale = app.desktop.scale();
                app.faces = faces(theme, scale);
                for window in &mut app.windows {
                    window.relayout(theme, scale, app.faces);
                }
            }
            Ok(false) => {}
            Err(err) => report(&alloc::format!("desktop change refused: {err}")),
        }
    }

    /// When the soonest store check falls due, for the park to wake at.
    fn next_check(app: &mut App, now: u64) -> Option<u64> {
        app.windows
            .iter_mut()
            .filter(|window| window.phase != Phase::Loading)
            .filter_map(|window| window.view.check_due(now))
            .filter(|&due| due > now)
            .min()
    }

    /// Hand the syntax worker the next job any window wants, answering
    /// whether the answer is already waiting.
    ///
    /// A window names the format of a document first, since colour depends on
    /// it; then colours what it shows; then checks a store whose edits have
    /// paused.
    fn submit_syntax(app: &mut App, now: u64) -> bool {
        if app.syntax_busy {
            return false;
        }
        let count = app.windows.len();
        for turn in 0..count {
            let index = (app.next_syntax + turn) % count;
            let window = &mut app.windows[index];
            let Some(work) = syntax_work(window, now) else {
                continue;
            };
            let job = SyntaxJob {
                window: window.id(),
                work,
            };
            app.next_syntax = (index + 1) % count;
            app.syntax_busy = true;
            return app.syntax.submit(job);
        }
        false
    }

    /// The syntax job `window` wants next, if any.
    fn syntax_work(window: &mut Window, now: u64) -> Option<SyntaxWork> {
        match window.phase {
            Phase::Loading => return None,
            Phase::Unnamed => {
                window.phase = Phase::Open;
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
            Phase::Open => {}
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

    fn adopt_document(app: &mut App, reply: DocumentReply) {
        let DocumentReply { window: id, answer } = reply;
        let index = app.index_of(id);
        if let DocumentAnswer::Saved { grew, .. } = answer {
            app.saves = app.saves.saturating_sub(1);
            // The room this save held: what the queue grew for it, and the
            // share its window kept for it on closing.
            app.documents
                .shrink(usize::from(grew) + usize::from(index.is_none()));
        }
        let Some(index) = index else {
            // A window since closed has nothing left to show, but a save it
            // asked for that failed is still said.
            if let DocumentAnswer::Saved {
                name,
                result: Err(err),
                ..
            } = answer
            {
                report(&alloc::format!("{name} could not be saved ({err})"));
            }
            return;
        };
        match answer {
            DocumentAnswer::Loaded { load, editor: None } => {
                if !app.queue(id, DocumentWork::Load(load)) {
                    let name = String::from(app.windows[index].view.name());
                    loaded(app, index, None, name, Err(LoadRefusal::TooLarge));
                }
            }
            DocumentAnswer::Loaded {
                load,
                editor: Some(editor),
            } => loaded(app, index, Some(load.handle), load.name, editor),
            DocumentAnswer::Saved {
                target,
                generation,
                rename,
                name,
                result,
                ..
            } => saved(app, index, target, generation, rename, &name, result),
            DocumentAnswer::Searched { searching, step } => searched(app, index, searching, step),
            DocumentAnswer::Converted { converting, done } => {
                converted(app, index, converting, done);
            }
        }
    }

    /// A search of window `index` went a step on. One of a document since
    /// changed is answered as that rather than scanned on to an answer nobody
    /// can use.
    fn searched(app: &mut App, index: usize, searching: Searching, step: Step) {
        let window = &mut app.windows[index];
        if !window.view.wants_search(searching.id) {
            return;
        }
        if step == Step::Partial && searching.generation == window.view.editor().generation() {
            let id = window.id();
            if !app.queue(id, DocumentWork::Search(searching)) {
                let window = &mut app.windows[index];
                state(window, String::from("There is no room to go on searching"));
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
        apply(app, index, outcome);
    }

    /// A conversion of window `index` went a step on, or through. One of a
    /// document since changed is put down, and the window says so.
    fn converted(
        app: &mut App,
        index: usize,
        converting: Converting,
        done: Option<Result<Option<Vec<Vec<u8>>>, OutOfMemory>>,
    ) {
        let window = &mut app.windows[index];
        let (generation, to) = (converting.generation, converting.conversion.to());
        let chunks = match done {
            None if generation == window.view.editor().generation() => {
                let id = window.id();
                if !app.queue(id, DocumentWork::Convert(converting)) {
                    let window = &mut app.windows[index];
                    state(window, String::from("There is no room to go on converting"));
                }
                return;
            }
            None => None,
            Some(Ok(chunks)) => chunks,
            Some(Err(OutOfMemory)) => {
                state(
                    window,
                    String::from("There is not enough memory to convert the line endings"),
                );
                return;
            }
        };
        let outcome =
            window
                .view
                .converted(generation, chunks, to, &window.layout, &mut window.damage);
        apply(app, index, outcome);
    }

    /// A document landed in window `index` from `handle`, or could not be
    /// read.
    fn loaded(
        app: &mut App,
        index: usize,
        handle: Option<Arc<Handle>>,
        name: String,
        editor: Result<Box<Editor>, LoadRefusal>,
    ) {
        let window = &mut app.windows[index];
        let access = window.view.access();
        match editor {
            Ok(editor) => {
                let unnamed = format_for_name(&name).is_none() && editor.mode() == Mode::Text;
                window.phase = if unnamed { Phase::Unnamed } else { Phase::Open };
                window.view = View::new(*editor, name, access, app.desktop.info().double_click());
                window.file.opened(handle);
            }
            Err(why) => {
                // The window stays, empty and untitled, saying why: an empty
                // window under the file's name would read as an empty file.
                report(&alloc::format!("{name} could not be opened: {why}"));
                window.view = View::new(
                    Editor::new(Document::new(), Format::PlainText),
                    String::from(UNTITLED),
                    Access::Untitled,
                    app.desktop.info().double_click(),
                );
                window
                    .view
                    .say(alloc::format!("{name} could not be opened: {why}"));
                window.file.opened(None);
                window.phase = Phase::Open;
            }
        }
        let (theme, scale) = (app.themes.active(), app.desktop.scale());
        window.relayout(theme, scale, app.faces);
    }

    /// A save of window `index` through `target` landed, or was refused.
    fn saved(
        app: &mut App,
        index: usize,
        target: Arc<Handle>,
        generation: u64,
        rename: Option<String>,
        name: &str,
        result: Result<(), Errno>,
    ) {
        if let Err(err) = result {
            report(&alloc::format!("{name} could not be saved ({err})"));
        }
        let window = &mut app.windows[index];
        let landed = window
            .file
            .saved(&mut window.view, target, generation, rename, result);
        window.damage.add(window.layout.status());
        window.owe_reported();
        // What was to close stays open to say why, and a quit waiting on it
        // is given up.
        app.quitting &= !landed.close_abandoned;
        if let Some(next) = landed.next {
            carry_out_save(app, index, next);
        }
        if landed.close {
            app.close_window(index);
        }
    }

    fn adopt_syntax(app: &mut App, reply: SyntaxReply) {
        let Some(index) = app.index_of(reply.window) else {
            return;
        };
        let window = &mut app.windows[index];
        if window.phase == Phase::Loading {
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
                apply(app, index, outcome);
                return;
            }
            SyntaxAnswer::Detected(Ok(None)) => {}
            SyntaxAnswer::Detected(Err(err)) => {
                report(&alloc::format!(
                    "the document's format could not be read ({err:?})"
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
                        report(&alloc::format!(
                            "a {} check failed ({err:?})",
                            format.label()
                        ));
                    }
                    alloc::vec![unchecked(format, err)]
                });
                window.view.checked(generation, diagnostics, layout, damage);
            }
        }
        window.owe_reported();
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

    /// Route one delivered event. One naming a window this editor no longer
    /// has is dropped.
    fn route(app: &mut App, event: &WindowEvent) {
        match event {
            WindowEvent::AppBarDefault => {
                app.new_window();
                return;
            }
            WindowEvent::AppBarMenu { item } if tairix_window::is_quit(*item) => {
                quit(app);
                return;
            }
            WindowEvent::AppBarMenu { item } => {
                if item.get() == NEW_WINDOW_ROW {
                    app.new_window();
                }
                return;
            }
            WindowEvent::OpenRequested => {
                drain_open_targets(app);
                return;
            }
            _ => {}
        }
        let Some(index) = event.window_id().and_then(|id| app.index_of(id)) else {
            // A file chosen for a window since closed is let go at once, not
            // left held for the life of the process.
            if let WindowEvent::FilePicked { handle, .. } = event {
                release(*handle);
            }
            return;
        };
        act(app, index, event);
        if app.quitting && quit_abandoned(app) {
            app.quitting = false;
        }
    }

    /// Quit: every window closes, and one holding changes asks first.
    fn quit(app: &mut App) {
        app.quitting = true;
        let ids: Vec<u64> = app.windows.iter().map(Window::id).collect();
        for id in ids {
            let Some(index) = app.index_of(id) else {
                continue;
            };
            let window = &mut app.windows[index];
            if window.phase == Phase::Loading || !window.view.editor().is_modified() {
                app.close_window(index);
            } else if window.view.modal().is_none() && !window.file.closing() {
                let outcome = window
                    .view
                    .close_requested(&window.layout, &mut window.damage);
                apply(app, index, outcome);
            }
        }
    }

    /// Whether the user turned a quit down: a window they were asked about
    /// is still open with no question showing and nothing about to close it.
    fn quit_abandoned(app: &App) -> bool {
        app.windows.iter().any(|window| {
            window.view.modal().is_none() && !window.file.closing() && !window.file.picking()
        })
    }

    /// Route one window-scoped event to the window at `index`.
    fn act(app: &mut App, index: usize, event: &WindowEvent) {
        let window = &mut app.windows[index];
        match event {
            WindowEvent::CloseRequested { .. } | WindowEvent::AlternateCloseRequested { .. } => {
                if window.phase == Phase::Loading {
                    app.close_window(index);
                    return;
                }
                let outcome = window
                    .view
                    .close_requested(&window.layout, &mut window.damage);
                apply(app, index, outcome);
            }
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => {
                let mode = app::mode_for(*width_px, *height_px);
                if !window
                    .pane
                    .resize_with(&mut app.client, &mode, &mut window.surface)
                {
                    report("the desktop refused a resize; the window keeps its size");
                }
                let (theme, scale) = (app.themes.active(), app.desktop.scale());
                window.relayout(theme, scale, app.faces);
            }
            WindowEvent::RedrawRequested { .. } => window.owe(Repaint::Whole),
            WindowEvent::ContentReleased { .. } => window.pane.release_frames(),
            WindowEvent::Focus { focused, .. } => {
                window.focused = *focused;
                window
                    .view
                    .focus_changed(*focused, &window.layout, &mut window.damage);
                window.owe_reported();
            }
            WindowEvent::FilePicked {
                window_id,
                handle,
                writable,
            } => picked(app, index, *window_id, *handle, *writable),
            WindowEvent::PickCancelled { .. } => {
                let _ = window.file.end_pick();
            }
            WindowEvent::MenuClosed {
                open_id, outcome, ..
            } => {
                if window.menu != Some(*open_id) {
                    return;
                }
                window.menu = None;
                if let MenuOutcome::Chosen(item) = outcome {
                    if window.phase != Phase::Loading {
                        let outcome = window
                            .view
                            .chosen(*item, &window.layout, &mut window.damage);
                        apply(app, index, outcome);
                    }
                }
            }
            WindowEvent::Key { key, .. } => keyed(app, index, *key),
            WindowEvent::Pointer {
                x,
                y,
                action,
                modifiers,
                ..
            } => {
                let at = pointer_point(*x, *y);
                let shape = shape_at(window, at);
                window.show_shape(&mut app.client, shape);
                if window.phase == Phase::Loading {
                    return;
                }
                let modifiers = key_input_event(KeyInput::ModifiersChanged {
                    modifiers: *modifiers,
                });
                let inputs = core::iter::once(modifiers).chain(pointer_input_events(*action, at));
                pointed(app, index, inputs);
            }
            WindowEvent::Scrolled { dx, dy, .. } => {
                if window.phase != Phase::Loading {
                    pointed(
                        app,
                        index,
                        core::iter::once(InputEvent::PointerScrolled { dx: *dx, dy: *dy }),
                    );
                }
            }
            WindowEvent::Minimized { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::PreviewRendered { .. } => {}
        }
    }

    /// Feed a key to the window at `index`.
    fn keyed(app: &mut App, index: usize, key: KeyInput) {
        let window = &mut app.windows[index];
        if window.phase == Phase::Loading {
            return;
        }
        let outcome = match key_input_event(key) {
            InputEvent::KeyPressed { key, modifiers } => window.view.on_key(
                key,
                modifiers,
                &window.layout,
                app.desktop.scale(),
                app.themes.active(),
                &mut window.damage,
            ),
            modifiers @ InputEvent::ModifiersChanged { .. } => window.view.on_pointer(
                &modifiers,
                tairix_rt::clock_get(),
                &window.layout,
                app.desktop.scale(),
                app.themes.active(),
                &mut window.damage,
            ),
            _ => return,
        };
        apply(app, index, outcome);
    }

    /// Feed pointer input to the window at `index`, carrying out what the
    /// last of it asked for.
    fn pointed(app: &mut App, index: usize, inputs: impl Iterator<Item = InputEvent>) {
        let now = tairix_rt::clock_get();
        let (theme, scale) = (app.themes.active(), app.desktop.scale());
        let window = &mut app.windows[index];
        let mut outcome = Outcome::default();
        for input in inputs {
            let next = window.view.on_pointer(
                &input,
                now,
                &window.layout,
                scale,
                theme,
                &mut window.damage,
            );
            outcome.relayout |= next.relayout;
            if next.request.is_some() {
                outcome.request = next.request;
            }
        }
        apply(app, index, outcome);
    }

    /// Paint what the engine reported for the window at `index`, and carry
    /// out what it asked for.
    fn apply(app: &mut App, index: usize, outcome: Outcome) {
        let window = &mut app.windows[index];
        window.owe_reported();
        if outcome.relayout {
            let (theme, scale) = (app.themes.active(), app.desktop.scale());
            window.relayout(theme, scale, app.faces);
        }
        if let Some(request) = outcome.request {
            carry_out(app, index, request);
        }
    }

    /// Carry out one request of the window at `index`.
    fn carry_out(app: &mut App, index: usize, request: Request) {
        let window = &mut app.windows[index];
        match request {
            Request::Save => save(app, index, None, false),
            Request::SaveThenClose => save(app, index, None, true),
            Request::SaveAs => ask_where(app, index, false),
            Request::Open => ask_pick(app, index, &PickPurpose::Open, PickFor::Open),
            Request::NewWindow => app.new_window(),
            Request::Close => app.close_window(index),
            Request::Copy(bytes) => {
                let kind = if core::str::from_utf8(&bytes).is_ok() {
                    ClipboardKind::Text
                } else {
                    ClipboardKind::Octets
                };
                if let Err(err) = clipboard::put(&mut app.client, window.id(), kind, &bytes) {
                    state(window, alloc::format!("Could not copy: {err}"));
                }
            }
            Request::Paste => match clipboard::take(&mut app.client, window.id()) {
                Ok(Some((_, bytes))) => {
                    let outcome = window
                        .view
                        .paste(&bytes, &window.layout, &mut window.damage);
                    apply(app, index, outcome);
                }
                Ok(None) => {}
                Err(err) => state(window, alloc::format!("Could not paste: {err}")),
            },
            Request::Menu { kind, anchor } => open_menu(window, &mut app.client, kind, anchor),
            Request::Search {
                id,
                search,
                replacement,
            } => match window.view.editor_mut().snapshot() {
                Ok((generation, snapshot)) => {
                    let work = DocumentWork::Search(Searching {
                        id,
                        generation,
                        snapshot,
                        search,
                        replacement,
                    });
                    let window_id = window.id();
                    if !app.queue(window_id, work) {
                        let window = &mut app.windows[index];
                        state(window, String::from("There is no room to search"));
                    }
                }
                Err(OutOfMemory) => {
                    state(window, String::from("There is not enough memory to search"));
                }
            },
            Request::Convert(to) => match window.view.editor_mut().snapshot() {
                Ok((generation, snapshot)) => {
                    let work = DocumentWork::Convert(Converting {
                        generation,
                        snapshot,
                        conversion: Conversion::new(to),
                    });
                    let window_id = window.id();
                    if !app.queue(window_id, work) {
                        let window = &mut app.windows[index];
                        state(window, String::from("There is no room to convert"));
                    }
                }
                Err(OutOfMemory) => state(
                    window,
                    String::from("There is not enough memory to convert"),
                ),
            },
        }
    }

    /// Say `message` in the window's status band.
    fn state(window: &mut Window, message: String) {
        window.view.say(message);
        window.damage.add(window.layout.status());
        window.owe_reported();
    }

    /// Save the window at `index` — through `save_as` for a Save As, else
    /// where its document came from, else asking where — closing it once
    /// saved when `then_close`.
    fn save(app: &mut App, index: usize, save_as: Option<(Arc<Handle>, String)>, then_close: bool) {
        let window = &mut app.windows[index];
        let step = window.file.save(&mut window.view, save_as, then_close);
        carry_out_save(app, index, step);
    }

    /// Carry out what asking window `index` to save came to.
    fn carry_out_save(app: &mut App, index: usize, step: SaveStep<Handle>) {
        let window = &mut app.windows[index];
        match step {
            SaveStep::Write(job) => {
                state(window, String::from("Saving\u{2026}"));
                let (id, name) = (window.id(), String::from(window.view.name()));
                let (target, generation, rename) =
                    (Arc::clone(&job.target), job.generation, job.rename.clone());
                let save = DocumentWork::Save {
                    job,
                    name,
                    grew: false,
                };
                if !app.queue(id, save) {
                    // Refused as a save the file never saw, so the window
                    // says so and whatever waited on it is given its answer.
                    let name = String::from(app.windows[index].view.name());
                    saved(
                        app,
                        index,
                        target,
                        generation,
                        rename,
                        &name,
                        Err(Errno::OutOfMemory),
                    );
                }
            }
            SaveStep::Queued => state(window, String::from("Saving\u{2026}")),
            SaveStep::AskWhere { then_close } => ask_where(app, index, then_close),
            SaveStep::NoMemory => {
                state(window, String::from("There is not enough memory to save"));
            }
        }
    }

    /// Ask the picker where to save the window at `index`.
    fn ask_where(app: &mut App, index: usize, then_close: bool) {
        let window = &app.windows[index];
        let offered = if window.view.access() == Access::Untitled {
            alloc::format!("{UNTITLED}.txt")
        } else {
            String::from(window.view.name())
        };
        let Ok(suggested) = DocumentName::new(&offered).or_else(|_| DocumentName::new(UNTITLED))
        else {
            return;
        };
        ask_pick(
            app,
            index,
            &PickPurpose::Save { suggested },
            PickFor::SaveAs { then_close },
        );
    }

    /// Ask the session's picker for `purpose` on the window at `index`.
    fn ask_pick(app: &mut App, index: usize, purpose: &PickPurpose, pick: PickFor) {
        let window = &mut app.windows[index];
        if window.file.picking() {
            state(
                window,
                String::from("A file chooser is already open for this window"),
            );
            return;
        }
        match app.client.pick_file(window.id(), *purpose) {
            Ok(()) => {
                let _ = window.file.start_pick(pick);
            }
            Err(err) => state(
                window,
                alloc::format!("The desktop offered no file chooser ({err})"),
            ),
        }
    }

    /// Let go of file grant `grant` this editor has no use for, so it is not
    /// held for the life of the process.
    fn release(grant: u64) {
        drop(tairix_rt::File::from_delegation(grant));
    }

    /// The user chose a file in the picker for the window at `index`.
    fn picked(app: &mut App, index: usize, window_id: u64, grant: u64, writable: bool) {
        let window = &mut app.windows[index];
        let Some(pick) = window.file.end_pick() else {
            release(grant);
            return;
        };
        let file = match tairix_rt::File::from_delegation(grant) {
            Ok(file) => file,
            Err(raw) => {
                state(
                    window,
                    alloc::format!(
                        "The chosen file could not be taken ({})",
                        Errno::from_syscall(raw)
                    ),
                );
                return;
            }
        };
        // A name the session no longer holds leaves the document named as it
        // was, or untitled.
        let name = app
            .client
            .take_picked_name(window_id)
            .ok()
            .filter(|name| !name.is_empty());
        match pick {
            PickFor::Open => {
                let into = app.windows[index].pristine().then_some(index);
                let name = name.unwrap_or_else(|| String::from(UNTITLED));
                app.open_document(file, name, writable, into);
            }
            PickFor::SaveAs { then_close } => {
                let name = name.unwrap_or_else(|| String::from(app.windows[index].view.name()));
                save(app, index, Some((Arc::new(file), name)), then_close);
            }
        }
    }

    /// Ask the session to open the window's `kind` menu at `anchor`.
    fn open_menu(
        window: &mut Window,
        client: &mut Client,
        kind: tairix_textedit::view::MenuKind,
        anchor: Rect,
    ) {
        let Ok(region) =
            WindowRegion::new(anchor.left(), anchor.top(), anchor.width, anchor.height)
        else {
            return;
        };
        match client.open_menu(window.id(), region, &window.view.menu(kind)) {
            Ok(open) => window.menu = Some(open),
            Err(err) => state(
                window,
                alloc::format!("The desktop composes no menu ({err})"),
            ),
        }
    }

    /// Open a window at every document the desktop has handed this instance.
    fn drain_open_targets(app: &mut App) {
        loop {
            let target = match app.client.take_open_target() {
                Ok(Some(target)) => target,
                Ok(None) => return,
                Err(err) => {
                    report(&alloc::format!("cannot take an open target ({err})"));
                    return;
                }
            };
            match target {
                Target::Document {
                    name,
                    grant,
                    writable,
                } => match tairix_rt::File::from_delegation(grant) {
                    Ok(file) => app.open_document(file, name, writable, None),
                    Err(raw) => app.tell(alloc::format!(
                        "{name} could not be opened: it could not be taken over ({})",
                        Errno::from_syscall(raw)
                    )),
                },
                // Unreachable from the desktop, which hands this editor an
                // open document because it holds no authority to open a name.
                Target::Path(path) => report(&alloc::format!(
                    "{path} was handed over as a path; this editor holds no filesystem authority \
                     and can only be given an open document"
                )),
                Target::Pane(pane) => report(&alloc::format!(
                    "{pane} was handed over, but this editor has no places to go to"
                )),
            }
        }
    }

    tairix_rt::entry!(main);
}

/// The host stub: this binary is a freestanding program on the Tier-1
/// targets, so on the host it exists only to keep the file covered by the
/// workspace build, clippy, and fmt.
#[cfg(not(freestanding))]
fn main() {}
