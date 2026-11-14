//! The `OpenMicro.app` bundle's `Run`: the library of machines and each
//! machine's window, and, re-entered as a capability-empty sandbox worker, a
//! machine itself.
//!
//! The front end holds the files, the windows, the sound streams and the
//! library, and drives each machine through `tairix_openmicro::worker`; a
//! worker holds its ROM images, its media and its guest, and nothing else. On
//! the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
extern crate alloc;

#[cfg(freestanding)]
#[path = "run/files.rs"]
mod files;
#[cfg(freestanding)]
#[path = "run/machine.rs"]
mod machine;
#[cfg(freestanding)]
#[path = "run/store.rs"]
mod store;

#[cfg(freestanding)]
mod program {

    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;

    use tairix_abi::input::PointerButtonCode;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::process::{DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, STDIN};
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::window_ipc::{
        AlertOutcome, AppMenu, AppMenuItemId, ClipboardKind, DocumentName, MenuOutcome,
        PickPurpose, PointerAction, WindowEvent, WindowRegion,
    };
    use tairix_abi::{Errno, ProcId};
    use tairix_browse::MediaType;
    use tairix_controls::StatusTone;
    use tairix_geometry::{Region, Scale};
    use tairix_input::InputEvent;
    use tairix_machines::assistant::{self as assistance, Assistant};
    use tairix_machines::keeping::Suspends;
    use tairix_machines::menu::{card_command, card_menu, CardCommand, Effect, Followed};
    use tairix_machines::record::Kept;
    use tairix_machines::shelf::{Entry, LibraryLook, Pictures, Request, Shelf};
    use tairix_machines::state::Suspended;
    use tairix_machines::store::{Resumable, Shelved};
    use tairix_machines::window::{
        self as library_window, BarAsk, LibraryEvent, LibraryWindow, SheetEvent, SheetWindow,
    };
    use tairix_machines::{Found, Library, Standing, Writes};
    use tairix_openmicro::front::assistant as offers;
    use tairix_openmicro::front::library::{self, Held, Machine, Socket};
    use tairix_openmicro::front::media::{Chosen, Disc, FileWork, Holding, Purpose, Saving, Tape};
    use tairix_openmicro::front::menu::{self, Command, Place};
    use tairix_openmicro::front::settings::{self, Settings};
    use tairix_openmicro::front::{events, folder, media};
    use tairix_openmicro::machine::bbc::fdc::PhysicalDrive;
    use tairix_openmicro::machine::Model;
    use tairix_openmicro::worker::MachineService;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_rt::{File, LogSink};
    use tairix_sandbox::rt::{serve_session_stdio, session_worker_role, spawn_share};
    use tairix_theme::{Theme, ThemeRegistry};
    use tairix_window::app::{self, LazyPool, Wake, WindowPane};
    use tairix_window::{
        Answer, Desktop, EventDrain, EventError, EventMailbox, EventSource, Parked,
        Target as Opened, WindowClient, WindowEvents,
    };

    use crate::files::{self, FileDone, FileJob, FileWorker, Speaker};
    use crate::machine::{self, Event, Life, MachineWindow, Pending};
    use crate::store::{self, Done, Job, Keeping, StoreWorker};

    /// The name this program states its refusals under: its manifest's.
    pub(crate) const APP_NAME: &str = "OpenMicro";
    /// The library window's title.
    const APP_TITLE: &str = "Open Micro Emulator";

    /// The sound asked to be queued ahead of the device, and the least kept.
    pub(crate) const LATENCY_FRAMES: u32 = 2_400;
    pub(crate) const LEAST_RING: u32 = 1_920;

    /// The window channel the windows are served over.
    pub(crate) type Client = WindowClient<app::RtWindowTransport>;

    const STORE_TOKEN: u64 = app::FIRST_APP_TOKEN;
    const FILES_TOKEN: u64 = STORE_TOKEN + 1;
    const FIRST_MACHINE_TOKEN: u64 = FILES_TOKEN + 1;
    /// A machine's session's read and write pipes and its sound.
    const TOKENS_PER_MACHINE: u64 = 3;

    // Every token is a bit of one word.
    const _: () = assert!(2 + TOKENS_PER_MACHINE * Machine::MAX_MACHINES as u64 <= 64);

    /// Store and file jobs waiting at once: a few for each machine.
    const STORE_JOBS: usize = 4 * Machine::MAX_MACHINES;
    const FILE_JOBS: usize = 8 * Machine::MAX_MACHINES;

    /// How long the program waits, once its windows are gone, for the store to
    /// take what it was last asked to keep.
    const DRAIN_NS: u64 = 5_000_000_000;

    /// How long a machine asked to suspend has to send its snapshot before it
    /// is turned off.
    const SUSPEND_GRACE_NS: u64 = 10_000_000_000;

    /// What a machine's deletion takes with it, as its question says.
    const DELETE_TAKES: &str =
        "Its settings and saved state are deleted; its folder in Documents is kept.";

    /// Record `id` with `message` in the system log, best-effort.
    pub(crate) fn record(id: tairix_log::EventId, message: &str) {
        tairix_log::log(
            &LogSink,
            &tairix_log::Event {
                level: tairix_log::Level::Info,
                id,
                message,
                fields: &[],
            },
        );
    }

    /// Stop watching `sound`'s notices.
    pub(crate) fn unwatch_sound(set: u64, sound: &Speaker, token: u64) {
        let _ = tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Del,
            WaitSourceKind::Port,
            sound.port(),
            token,
        );
    }

    /// The bit of the wake word `token` is.
    const fn member_bit(token: u64) -> u64 {
        match token.checked_sub(STORE_TOKEN) {
            Some(at) if at < 64 => 1 << at,
            _ => 0,
        }
    }

    /// The read, write and sound tokens of the machine in wait-set place
    /// `slot`.
    const fn machine_tokens(slot: usize) -> (u64, u64, u64) {
        let first = FIRST_MACHINE_TOKEN + TOKENS_PER_MACHINE * slot as u64;
        (first, first + 1, first + 2)
    }

    /// What woke the loop.
    #[derive(Clone, Copy, Default)]
    struct Woke {
        members: u64,
        desktop: bool,
    }

    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        store: &'a StoreWorker,
        files: &'a FileWorker,
        due: &'a Cell<Option<u64>>,
        woke: &'a Cell<Woke>,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let mut woke = self.woke.get();
            let parked = match app::park_until_due(self.set, self.due.get())? {
                None => Parked::Interrupted,
                Some(Wake::App(token)) => {
                    // A worker's readiness is a level, so it is taken here or
                    // the park would spin on it.
                    match token {
                        STORE_TOKEN => self.store.wake().drain(),
                        FILES_TOKEN => self.files.wake().drain(),
                        _ => {}
                    }
                    woke.members |= member_bit(token);
                    Parked::Interrupted
                }
                Some(Wake::DesktopChanged) => {
                    woke.desktop = true;
                    Parked::Interrupted
                }
                Some(Wake::PressureChanged) => {
                    tairix_font::trim_glyph_cache();
                    Parked::Served
                }
                Some(Wake::Event | Wake::PressureUnchanged) => Parked::Served,
            };
            self.woke.set(woke);
            Ok(parked)
        }
    }

    /// How far the library has been read.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Shelving {
        Reading,
        Read,
        Unread,
    }

    /// Whether the application's documents folder can be reached.
    #[derive(Clone, PartialEq, Eq)]
    enum Documents {
        /// Not yet asked for.
        Unasked,
        /// Asked for, not yet answered.
        Asked,
        /// Granted, its grant redeemed on the file worker.
        Granted,
        /// Refused, and why.
        Refused(String),
    }

    /// What a dialog over a window holds.
    enum Sheet {
        New(Box<Assistant>),
        Settings { id: u32, settings: Box<Settings> },
    }

    /// What a dialog answered.
    enum SheetAnswer {
        New(assistance::Answer),
        Settings(u32, settings::Answer),
    }

    impl library_window::Sheet for Sheet {
        type Answer = SheetAnswer;

        fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
            match self {
                Self::New(assistant) => assistant.extent(scale, theme),
                Self::Settings { settings, .. } => settings.extent(scale, theme),
            }
        }

        fn render(&self, surface: &mut tairix_raster::Surface, scale: Scale, theme: &Theme) {
            match self {
                Self::New(assistant) => assistant.render(surface, scale, theme),
                Self::Settings { settings, .. } => settings.render(surface, scale, theme),
            }
        }

        fn on_input(
            &mut self,
            event: &InputEvent,
            at_ns: u64,
            looks: (Scale, &Theme),
            damage: &mut Region,
        ) -> Option<SheetAnswer> {
            match self {
                Self::New(assistant) => assistant
                    .on_input(event, at_ns, looks, damage)
                    .map(SheetAnswer::New),
                Self::Settings { id, settings } => settings
                    .on_input(event, at_ns, looks, damage)
                    .map(|answer| SheetAnswer::Settings(*id, answer)),
            }
        }
    }

    /// What a machine's folder is made for.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FolderFor {
        /// A new machine's blank disc, for its drive 0 while that is empty.
        FirstDisc,
        /// The blank disc or tape asked for.
        Blank(Purpose),
    }

    /// A machine's folder asked for and not yet made.
    struct FolderOwed {
        id: u32,
        made_for: FolderFor,
        /// Whether the file worker was asked to make it.
        asked: bool,
    }

    /// What of a running machine's folder waits for the documents folder to
    /// be reached.
    enum MediaOwed {
        /// Put in as a machine started afresh takes them.
        Insert(u32),
        /// Taken back as the files of the media its snapshot restored, as
        /// they were recorded when it resumed.
        Rebind {
            id: u32,
            discs: [Option<Held>; 2],
            tape: Option<Held>,
        },
    }

    impl MediaOwed {
        const fn id(&self) -> u32 {
            match self {
                Self::Insert(id) | Self::Rebind { id, .. } => *id,
            }
        }
    }

    /// A document handed to the application, for a machine to take.
    struct Handed {
        file: Option<File>,
        grant: Option<u64>,
        writable: bool,
        name: String,
    }

    impl Handed {
        /// Let the file go.
        fn drop_file(self) {
            if let Some(grant) = self.grant {
                drop(File::from_delegation(grant));
            }
        }
    }

    /// Everything the loop holds.
    struct App {
        client: Client,
        endpoint: u64,
        set: u64,
        desktop: Desktop,
        themes: ThemeRegistry,
        store: Arc<StoreWorker>,
        files: Arc<FileWorker>,
        library: Library<Machine>,
        shelving: Shelving,
        writes: Writes,
        shelf: Option<LibraryWindow>,
        sheet: Option<SheetWindow<Sheet>>,
        /// The menu up over a card of the library, its machine, and the rows
        /// it was last given.
        card_menu: Option<Followed<u32>>,
        /// The deletion asked about: its alert and its machine.
        asking: Option<(u64, u32)>,
        pictures: Pictures,
        /// The machines whose state in the store holds a snapshot.
        suspended: Vec<u32>,
        /// The machines suspended whose state is not yet kept, each holding
        /// its window open until it is.
        keeping: Suspends<WindowPane>,
        machines: Vec<MachineWindow>,
        /// The ROM images installed in the store.
        installed: Vec<String>,
        documents: Documents,
        /// The last start's ticket.
        tickets: u64,
        /// The workers the picture tube spreads across.
        pool: LazyPool,
        /// The socket an image is being picked for, over the dialog that
        /// asked, and the picked file's delegation while its name is asked.
        installing: Option<(Socket, Option<u64>)>,
        /// The machine whose window had the keyboard last.
        focused: Option<u32>,
        /// A document handed over for a machine that is starting.
        waiting: Option<(u32, Handed)>,
        /// The document the application was opened with, until the library
        /// is read.
        launched: Option<Handed>,
        /// The window the dialog up is over.
        sheet_owner: Option<u64>,
        /// The machines' folders asked for: waiting for the documents folder,
        /// or being made, so each machine's is made once.
        folders_owed: Vec<FolderOwed>,
        /// What of running machines' folders waits for the documents folder.
        media_owed: Vec<MediaOwed>,
        /// When leaving must end, however much is still being kept.
        leaving: Option<u64>,
        quit: bool,
    }

    /// Record that `window`, leaving, lets go of what it still offered to
    /// save.
    fn report_unsaved(window: &MachineWindow) {
        if !window.rack.is_settled() {
            app::report(
                APP_NAME,
                format_args!(
                    "the discs, tapes or snapshots {} offered to be saved were let go",
                    window.name
                ),
            );
        }
    }

    /// Where machine `id` stands, by its window, its suspend and the store.
    fn standing_of(
        (machines, keeping, suspended): (&[MachineWindow], &Suspends<WindowPane>, &[u32]),
        id: u32,
    ) -> Standing {
        if keeping.holds(id) {
            Standing::Saving
        } else if machines.iter().any(|machine| machine.id == id) {
            Standing::Running
        } else if suspended.contains(&id) {
            Standing::Suspended
        } else {
            Standing::Off
        }
    }

    impl App {
        /// The window an alert about the library is held over.
        fn owner(&self) -> Option<u64> {
            self.shelf.as_ref().map(LibraryWindow::id)
        }

        /// Tell the user `headline` was not done, and why, over `owner`.
        fn refuse_over(&mut self, owner: Option<u64>, headline: &str, why: &str) {
            let _ = app::refuse(
                &mut self.client,
                (APP_NAME, self.endpoint),
                owner,
                headline,
                why,
            );
        }

        fn refuse(&mut self, headline: &str, why: &str) {
            let owner = self.owner();
            self.refuse_over(owner, headline, why);
        }

        fn standing(&self, id: u32) -> Standing {
            standing_of((&self.machines, &self.keeping, &self.suspended), id)
        }

        fn index(&self, id: u32) -> Option<usize> {
            self.machines.iter().position(|machine| machine.id == id)
        }

        /// Ask the store worker for `job`: false, said, where it holds as many
        /// as it can.
        fn ask(&mut self, job: Job) -> bool {
            let taken = self.store.submit(job).is_ok();
            if !taken {
                app::report(APP_NAME, "too much is waiting on the store");
            }
            taken
        }

        /// Ask the file worker for `job`; refused, the user is told under
        /// `headline` and any delegation it carries is let go.
        fn file(&mut self, job: FileJob, headline: &str) {
            if let Err(job) = self.files.submit(job) {
                if let FileJob::Take { grant, .. } | FileJob::SaveTo { grant, .. } = job {
                    drop(File::from_delegation(grant));
                }
                self.refuse(headline, "Too much is waiting on files.");
            }
        }

        /// The library changed: write it, or again once the write under way
        /// lands.
        fn save(&mut self) {
            if self.shelving != Shelving::Read || !self.writes.begin() {
                return;
            }
            match self.library.record() {
                Ok(record) => {
                    if !self.ask(Job::Record(record)) {
                        self.recorded(Err(String::from("the store is too busy")));
                    }
                }
                Err(err) => self.recorded(Err(format!("its record could not be made ({err:?})"))),
            }
        }

        /// A record write landed: what waited on it goes, or waits for the
        /// next.
        fn recorded(&mut self, result: Result<u64, String>) {
            let landed = self.writes.landed(result.is_ok());
            match result {
                Ok(generation) => {
                    self.library.written(generation);
                    for id in landed.removals {
                        self.ask(Job::Remove { id });
                    }
                }
                Err(why) if landed.newly_failed => {
                    self.refuse("The machine library could not be saved", &why);
                }
                Err(_) => {}
            }
            if landed.again {
                self.save();
            }
        }

        /// Machine `id` changed as `change` says, and the library is written.
        fn remember(&mut self, id: u32, change: impl FnOnce(&mut Machine)) {
            let Some(mut machine) = self.library.machine(id).cloned() else {
                return;
            };
            change(&mut machine);
            if self.library.replace(machine).is_ok() {
                self.save();
                self.show();
            }
        }

        /// Adopt what the workers landed.
        fn collect(&mut self) {
            let mut landed = Vec::new();
            self.store.collect_landed(|done| landed.push(done));
            for done in landed {
                self.adopt(done);
            }
            let mut filed = Vec::new();
            self.files.collect_landed(|done| filed.push(done));
            for done in filed {
                self.filed(done);
            }
        }

        fn adopt(&mut self, done: Done) {
            match done {
                Done::Loaded(loaded) => self.shelved(loaded),
                Done::Recorded(result) => self.recorded(result),
                Done::Removed(result) => {
                    if let Err(why) = result {
                        app::report(APP_NAME, format_args!("a machine's state stays ({why})"));
                    }
                }
                Done::Copied { from, to, result } => {
                    if let Err(why) = result {
                        app::report(
                            APP_NAME,
                            format_args!("a copy's state was not kept ({why})"),
                        );
                    } else if self.suspended.contains(&from)
                        && self.suspended.try_reserve(1).is_ok()
                    {
                        self.suspended.push(to);
                        self.show();
                    }
                }
                Done::Kept {
                    id,
                    picture,
                    why,
                    result,
                } => {
                    if let Some(picture) = picture.filter(|_| self.library.machine(id).is_some()) {
                        self.pictures.put(id, &picture);
                        self.picture_drawn(id);
                    }
                    self.state_kept(id, why, result);
                }
                Done::Pictured(pictures) => {
                    for (id, picture) in pictures {
                        self.pictures.put(id, &picture);
                        self.picture_drawn(id);
                    }
                }
                Done::Started { id, ticket, read } => self.started(id, ticket, read),
                Done::Installed { socket, blob } => self.installed(socket, blob),
            }
        }

        /// The library was read: show it, or make the first machine.
        fn shelved(&mut self, loaded: Result<(Shelved<Machine>, Vec<String>), String>) {
            let (shelved, installed) = match loaded {
                Ok(loaded) => loaded,
                Err(why) => {
                    self.shelving = Shelving::Unread;
                    self.refuse("The machine library could not be read", &why);
                    return;
                }
            };
            self.library = shelved.loaded.library;
            self.suspended = shelved.suspended;
            self.installed = installed;
            self.shelving = Shelving::Read;
            for (id, picture) in shelved.pictures {
                self.pictures.put(id, &picture);
            }
            if shelved.unswept > 0 {
                app::report(
                    APP_NAME,
                    format_args!(
                        "{} files of machines no longer kept could not be removed",
                        shelved.unswept
                    ),
                );
            }
            match shelved.loaded.found {
                Found::Fresh => {
                    let name = self.library.fresh_name(Model::ModelB.title());
                    self.create(library::Fitting::model_b(), &name, true);
                }
                Found::Unreadable => self.refuse(
                    "The machine library could not be read",
                    "Neither copy of it checks out, so the library starts empty.",
                ),
                Found::Read => {}
            }
            self.show();
            record(events::LIBRARY_READ, events::LIBRARY_READ_MESSAGE);
            if let Some(handed) = self.launched.take() {
                self.handed(handed);
            }
        }

        /// Open the library window, or raise the one open.
        fn open_shelf(&mut self) {
            if let Some(shelf) = &self.shelf {
                let _ = self.client.activate_window(shelf.id());
                return;
            }
            let look = LibraryLook::titled(APP_TITLE);
            let looks = (&self.desktop, self.themes.active());
            match LibraryWindow::open(&mut self.client, self.endpoint, looks, look) {
                Ok(win) => {
                    let id = win.id();
                    self.shelf = Some(win);
                    self.show();
                    self.paint_whole_shelf();
                    self.ask_documents(id);
                }
                Err(why) => app::report(APP_NAME, why),
            }
        }

        /// Ask for the documents folder over window `window_id`, once.
        fn ask_documents(&mut self, window_id: u64) {
            if self.documents != Documents::Unasked {
                return;
            }
            self.documents = match self.client.request_documents_folder(window_id) {
                Ok(()) => Documents::Asked,
                Err(err) => Documents::Refused(format!("{err}")),
            };
        }

        /// Show the library as it stands, repainting what that changed.
        fn show(&mut self) {
            let looks = (self.desktop.scale(), self.themes.active());
            let Some(win) = &mut self.shelf else {
                return;
            };
            let held = (
                self.machines.as_slice(),
                &self.keeping,
                self.suspended.as_slice(),
            );
            let entries = self.library.machines().iter().map(|machine| Entry {
                id: machine.id,
                name: machine.name.clone(),
                about: library::describe(machine),
                standing: standing_of(held, machine.id),
            });
            win.show(entries, looks, &self.pictures);
        }

        fn paint_whole_shelf(&mut self) {
            let looks = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.shelf {
                win.paint_whole(looks, &self.pictures);
            }
        }

        fn picture_drawn(&mut self, id: u32) {
            let looks = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.shelf {
                win.picture_drawn(id, looks, &self.pictures);
            }
        }

        fn shelf_event(&mut self, event: &WindowEvent) {
            let looks = (self.desktop.scale(), self.themes.active());
            let interval = self.desktop.info().double_click();
            let Some(win) = &mut self.shelf else {
                return;
            };
            match win.event(&mut self.client, event, (looks, interval), &self.pictures) {
                LibraryEvent::Nothing => {}
                LibraryEvent::Closed => self.close_shelf(),
                LibraryEvent::Menu {
                    open_id,
                    item,
                    kept,
                } => self.card_answered(open_id, item, kept),
                LibraryEvent::Request(request) => self.request(request),
            }
        }

        /// The library window closed, leaving the program on the icon bar.
        fn close_shelf(&mut self) {
            if let Some(owner) = self.owner() {
                self.close_sheet_over(owner);
            }
            self.card_menu = None;
            if let Some(win) = self.shelf.take() {
                win.close(&mut self.client);
            }
            self.end_once_left();
        }

        /// The dialog up closed where it is over window `owner`, which is
        /// closing.
        fn close_sheet_over(&mut self, owner: u64) {
            if self.sheet_owner == Some(owner) {
                self.close_sheet();
            }
        }

        fn request(&mut self, request: Request) {
            match self.shelving {
                Shelving::Read => {}
                Shelving::Reading => {
                    self.refuse(
                        "The machine library is still being read",
                        "Try again in a moment.",
                    );
                    return;
                }
                Shelving::Unread => {
                    self.refuse(
                        "The machine library could not be read",
                        "No machine can be made or changed until it is.",
                    );
                    return;
                }
            }
            match request {
                Request::New => self.open_assistant(),
                Request::Start(id) => self.start(id),
                Request::Settings(id) => self.open_settings(id, self.owner()),
                Request::Duplicate(id) => self.duplicate(id),
                Request::Delete(id) => self.ask_delete(id),
                Request::Menu { id, at } => self.open_card_menu(id, at),
            }
        }

        /// The rows of machine `id`'s card menu as it stands now.
        fn card_rows(&self, id: u32) -> Option<AppMenu> {
            let running = self
                .index(id)
                .map(|n| &self.machines[n])
                .filter(|machine| machine.life == Life::Running)
                .map(|machine| (machine.shown(), machine.offered.clone()));
            let rows = running.as_ref().map(|(shown, offered)| {
                move |menu: &mut AppMenu| menu::push_rows(menu, (shown, offered), Place::Card)
            });
            card_menu(self.standing(id), rows).ok()
        }

        fn open_card_menu(&mut self, id: u32, at: tairix_geometry::Point) {
            let (Some(rows), Some(win)) = (self.card_rows(id), &self.shelf) else {
                return;
            };
            match win.open_menu(&mut self.client, at, &rows) {
                Ok(open) => self.card_menu = Some(Followed::new(open, id, rows)),
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("the menu could not be opened ({err})"),
                ),
            }
        }

        fn card_answered(&mut self, open_id: u64, item: Option<AppMenuItemId>, kept: bool) {
            let Some(id) = self
                .card_menu
                .as_ref()
                .filter(|card| card.open() == open_id)
                .map(Followed::kind)
            else {
                return;
            };
            if !kept {
                self.card_menu = None;
            }
            match item.map(card_command) {
                None => {}
                Some(CardCommand::Start) => self.request(Request::Start(id)),
                Some(CardCommand::Settings) => self.request(Request::Settings(id)),
                Some(CardCommand::Duplicate) => self.request(Request::Duplicate(id)),
                Some(CardCommand::Delete) => self.request(Request::Delete(id)),
                Some(CardCommand::Machine(item)) => {
                    if let (Some(n), Some(command)) = (self.index(id), menu::command(item)) {
                        self.command(n, command);
                    }
                }
            }
        }

        /// Every menu up shown as what it is for now stands.
        fn follow_menus(&mut self) {
            for machine in self
                .machines
                .iter_mut()
                .filter(|machine| machine.menu.is_some())
            {
                let mut rows = AppMenu::EMPTY;
                let built = menu::push_rows(
                    &mut rows,
                    (&machine.shown(), &machine.offered),
                    Place::Window,
                );
                let window_id = machine.window_id();
                if let (Some(up), Ok(())) = (machine.menu.as_mut(), built) {
                    let open = up.open();
                    if let Some(rows) = up.follow(rows) {
                        library_window::restate(
                            &mut self.client,
                            (APP_NAME, window_id),
                            open,
                            rows,
                        );
                    }
                }
            }
            let Some(window_id) = self.owner() else {
                self.card_menu = None;
                return;
            };
            let rows = self
                .card_menu
                .as_ref()
                .and_then(|card| self.card_rows(card.kind()));
            if let (Some(card), Some(rows)) = (self.card_menu.as_mut(), rows) {
                let open = card.open();
                if let Some(rows) = card.follow(rows) {
                    library_window::restate(&mut self.client, (APP_NAME, window_id), open, rows);
                }
            }
        }

        fn open_assistant(&mut self) {
            let names = offers::MODELS
                .iter()
                .map(|model| self.library.fresh_name(model.title()))
                .collect();
            let owner = self.owner();
            self.open_sheet(
                Sheet::New(Box::new(offers::assistant(names))),
                assistance::TITLE,
                owner,
            );
        }

        /// Open machine `id`'s settings over window `owner`.
        fn open_settings(&mut self, id: u32, owner: Option<u64>) {
            let Some(machine) = self.library.machine(id) else {
                return;
            };
            let settings = Box::new(Settings::new(
                machine,
                self.standing(id),
                self.installed.clone(),
            ));
            self.open_sheet(Sheet::Settings { id, settings }, settings::TITLE, owner);
        }

        /// Open `sheet` as an owner-modal dialog over window `owner`.
        fn open_sheet(&mut self, sheet: Sheet, title: &str, owner: Option<u64>) {
            let Some(owner) = owner else {
                return;
            };
            if self.sheet.is_some() {
                return;
            }
            let looks = (self.desktop.scale(), self.themes.active());
            match SheetWindow::open(
                &mut self.client,
                (owner, self.endpoint),
                looks,
                sheet,
                title,
            ) {
                Ok(win) => {
                    self.sheet = Some(win);
                    self.sheet_owner = Some(owner);
                }
                Err(why) => {
                    self.refuse_over(Some(owner), &format!("{title} could not be opened"), &why);
                }
            }
        }

        fn close_sheet(&mut self) {
            self.sheet_owner = None;
            if let Some(win) = self.sheet.take() {
                win.close(&mut self.client);
            }
            if let Some((_, Some(grant))) = self.installing.take() {
                drop(File::from_delegation(grant));
            }
        }

        fn sheet_event(&mut self, event: &WindowEvent) {
            match *event {
                WindowEvent::FilePicked {
                    window_id, handle, ..
                } => {
                    self.rom_picked(window_id, handle);
                    return;
                }
                WindowEvent::PickCancelled { .. } => {
                    self.installing = None;
                    return;
                }
                _ => {}
            }
            let looks = (self.desktop.scale(), self.themes.active());
            let Some(win) = &mut self.sheet else {
                return;
            };
            match win.event(event, looks) {
                SheetEvent::Nothing => self.pick_rom(),
                SheetEvent::Closed => self.close_sheet(),
                SheetEvent::Answered(answer) => {
                    self.close_sheet();
                    self.answered(answer);
                }
            }
        }

        /// A settings dialog asked for an image to install: the picker opened
        /// over it.
        fn pick_rom(&mut self) {
            let Some(win) = &mut self.sheet else {
                return;
            };
            let window_id = win.id();
            let Some(socket) = (match win.sheet() {
                Sheet::Settings { .. } => {
                    let mut asked = None;
                    win.change((self.desktop.scale(), self.themes.active()), |sheet, _| {
                        if let Sheet::Settings { settings, .. } = sheet {
                            asked = settings.take_install();
                        }
                    });
                    asked
                }
                Sheet::New(_) => None,
            }) else {
                return;
            };
            if self.installing.is_some() {
                return;
            }
            match self.client.pick_file(window_id, PickPurpose::Open) {
                Ok(()) => self.installing = Some((socket, None)),
                Err(err) => self.refuse_over(
                    Some(window_id),
                    "The file picker could not be opened",
                    &format!("{err}"),
                ),
            }
        }

        /// The file picked for an install: its name asked, its delegation held.
        fn rom_picked(&mut self, window_id: u64, handle: u64) {
            let Some((socket, held)) = self.installing.take() else {
                drop(File::from_delegation(handle));
                return;
            };
            if let Some(stale) = held {
                drop(File::from_delegation(stale));
            }
            match self.client.ask_picked_name(window_id) {
                Ok(()) => self.installing = Some((socket, Some(handle))),
                Err(err) => {
                    drop(File::from_delegation(handle));
                    self.refuse_over(
                        Some(window_id),
                        "The picked file could not be named",
                        &format!("{err}"),
                    );
                }
            }
        }

        /// The installed image's name came: it is kept in the store.
        fn rom_named(&mut self, name: Result<String, Errno>) {
            let Some((socket, Some(grant))) = self.installing.take() else {
                return;
            };
            let name = name.unwrap_or_else(|_| String::from("image"));
            if !self.ask(Job::Install {
                grant,
                name,
                socket,
            }) {
                drop(File::from_delegation(grant));
            }
        }

        /// An image was installed, or not.
        fn installed(&mut self, socket: Socket, blob: Result<String, String>) {
            let blob = match blob {
                Ok(blob) => blob,
                Err(why) => {
                    let owner = self.sheet.as_ref().map(SheetWindow::id).or(self.owner());
                    self.refuse_over(owner, "The ROM image could not be installed", &why);
                    return;
                }
            };
            if !self.installed.contains(&blob) && self.installed.try_reserve(1).is_ok() {
                self.installed.push(blob.clone());
            }
            let looks = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.sheet {
                win.change(looks, |sheet, damage| {
                    if let Sheet::Settings { settings, .. } = sheet {
                        settings.installed(socket, blob, looks, damage);
                    }
                });
            }
        }

        fn answered(&mut self, answer: SheetAnswer) {
            match answer {
                SheetAnswer::New(assistance::Answer::Create {
                    model,
                    name,
                    choice,
                }) => {
                    if let Some((fitting, disc)) = offers::chosen(model, choice) {
                        self.create(fitting, &name, disc);
                    }
                }
                SheetAnswer::Settings(id, settings::Answer::Apply(choices)) => {
                    self.apply(id, choices);
                }
                SheetAnswer::New(assistance::Answer::Cancel)
                | SheetAnswer::Settings(_, settings::Answer::Cancel) => {}
            }
        }

        /// Machine `id` takes `choices`: what is fitted only while it is off.
        fn apply(&mut self, id: u32, choices: settings::Choices) {
            let Some(mut machine) = self.library.machine(id).cloned() else {
                return;
            };
            if machine.fitting != choices.fitting {
                if let Some(why) = self.standing(id).locked() {
                    self.refuse("The settings could not be applied", why);
                    return;
                }
                machine.fitting = choices.fitting;
            }
            machine.name.clone_from(&choices.name);
            machine.volume = choices.volume;
            machine.fast_tape = choices.fast_tape;
            if let Err(err) = self.library.replace(machine) {
                self.refuse(
                    "The settings could not be applied",
                    &format!("the library refused them ({err:?})"),
                );
                return;
            }
            if let Some(n) = self.index(id) {
                let window = &mut self.machines[n];
                window.volume = choices.volume;
                window.fast_tape = choices.fast_tape;
                if window.name != choices.name {
                    window.rename(&mut self.client, &choices.name);
                }
            }
            self.save();
            self.show();
        }

        /// Make a machine called `name` fitted so, a blank disc made for its
        /// drive 0 where `disc`.
        fn create(&mut self, fitting: library::Fitting, name: &str, disc: bool) {
            let id = match self.library.mint() {
                Ok(id) => id,
                Err(err) => {
                    self.refuse(
                        "No machine can be made",
                        &format!("the library is full ({err:?})"),
                    );
                    return;
                }
            };
            let machine = Machine::new(id, String::from(name), fitting);
            if let Err(err) = self.library.add(machine) {
                self.refuse(
                    &format!("\u{201c}{name}\u{201d} could not be made"),
                    &format!("the library refused it ({err:?})"),
                );
                return;
            }
            self.save();
            self.show();
            if disc {
                self.make_folder(id, FolderFor::FirstDisc);
            }
        }

        /// Make machine `id`'s folder for `made_for`, once the documents
        /// folder is reached; one already asked for takes a blank asked for.
        fn make_folder(&mut self, id: u32, made_for: FolderFor) {
            if let Some(owed) = self.folders_owed.iter_mut().find(|owed| owed.id == id) {
                if let FolderFor::Blank(_) = made_for {
                    owed.made_for = made_for;
                }
                return;
            }
            if matches!(self.documents, Documents::Refused(_))
                || self.folders_owed.try_reserve(1).is_err()
            {
                return;
            }
            self.folders_owed.push(FolderOwed {
                id,
                made_for,
                asked: false,
            });
            self.ask_folders();
        }

        /// The folders owed asked of the file worker, once it can reach the
        /// documents folder.
        fn ask_folders(&mut self) {
            if self.documents != Documents::Granted {
                return;
            }
            let mut jobs = Vec::new();
            for owed in self.folders_owed.iter_mut().filter(|owed| !owed.asked) {
                if let Some(machine) = self.library.machine(owed.id) {
                    owed.asked = true;
                    jobs.push(FileJob::MakeFolder {
                        id: owed.id,
                        name: machine.name.clone(),
                    });
                }
            }
            for job in jobs {
                self.file(job, "The machine's folder could not be made");
            }
        }

        /// Copy machine `id`, its state and its folder with it.
        fn duplicate(&mut self, id: u32) {
            let Some(source) = self.library.machine(id).cloned() else {
                return;
            };
            if !self.standing(id).at_rest() {
                return;
            }
            let to = match self.library.mint() {
                Ok(to) => to,
                Err(err) => {
                    self.refuse(
                        "No machine can be made",
                        &format!("the library is full ({err:?})"),
                    );
                    return;
                }
            };
            let name = self.library.copy_name(&source.name);
            let folder = source.folder.clone();
            let copy = Machine {
                id: to,
                name: name.clone(),
                folder: None,
                discs: [None, None],
                tape: None,
                ..source.clone()
            };
            if self.library.add(copy).is_err() {
                self.refuse(
                    &format!("\u{201c}{name}\u{201d} could not be made"),
                    "the library refused it",
                );
                return;
            }
            self.pictures.copy(id, to);
            self.ask(Job::Copy { from: id, to });
            if let (Some(folder), Documents::Granted) = (folder, &self.documents) {
                self.file(
                    FileJob::CopyFolder { to, folder, name },
                    "The machine's folder could not be copied",
                );
                self.waiting_media(to, &source);
            }
            self.save();
            self.show();
        }

        /// The copy `to` of `source` holds the media of its folder its source
        /// held, once its folder is copied.
        fn waiting_media(&mut self, to: u32, source: &Machine) {
            let unsuspended = |held: &Option<Held>| {
                held.as_ref().map(|held| Held {
                    name: held.name.clone(),
                    digest: None,
                })
            };
            let discs = source.discs.each_ref().map(unsuspended);
            let tape = unsuspended(&source.tape);
            self.remember(to, |copy| {
                copy.discs = discs;
                copy.tape = tape;
            });
        }

        fn ask_delete(&mut self, id: u32) {
            let Some(machine) = self.library.machine(id) else {
                return;
            };
            let Some(question) = library_window::delete_question(&machine.name, DELETE_TAKES)
            else {
                return;
            };
            if let Some((alert, _)) = self.asking.take() {
                let _ = self.client.close_alert(alert);
            }
            let owner = self.owner();
            match self.client.open_alert(self.endpoint, owner, &question) {
                Ok(alert) => self.asking = Some((alert, id)),
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("the desktop would not ask about the deletion ({err})"),
                ),
            }
        }

        /// Delete machine `id`: out of the library at once, its state once a
        /// record without it is kept; its folder is the user's, and stays.
        fn delete(&mut self, id: u32) {
            if !self.standing(id).at_rest() || self.library.remove(id).is_none() {
                return;
            }
            self.pictures.forget(id);
            self.suspended.retain(|&held| held != id);
            self.folders_owed.retain(|owed| owed.id != id || owed.asked);
            self.writes.doom(id);
            self.save();
            self.show();
        }

        /// Start machine `id`, resuming it where it is suspended, or raise its
        /// window.
        fn start(&mut self, id: u32) {
            if let Some(n) = self.index(id) {
                let _ = self.client.activate_window(self.machines[n].window_id());
                return;
            }
            if !self.standing(id).startable() {
                return;
            }
            let Some(machine) = self.library.machine(id).cloned() else {
                return;
            };
            let Some(slot) = (0..Machine::MAX_MACHINES)
                .find(|slot| self.machines.iter().all(|m| m.slot != *slot))
            else {
                self.refuse(
                    "The machine could not start",
                    "As many machines as can run are running.",
                );
                return;
            };
            self.tickets = self.tickets.wrapping_add(1);
            let ticket = self.tickets;
            let (read, write, _) = machine_tokens(slot);
            let looks = (&self.desktop, self.themes.active());
            let window = match MachineWindow::open(
                &mut self.client,
                (self.endpoint, self.set),
                looks,
                &machine,
                (slot, ticket),
                (read, write),
            ) {
                Ok(window) => window,
                Err(why) => {
                    self.refuse("The machine could not start", &why);
                    return;
                }
            };
            if self.machines.try_reserve(1).is_err() {
                let _ = window.into_pane().close(&mut self.client);
                return;
            }
            self.machines.push(window);
            let suspended = self.suspended.contains(&id);
            let job = Job::Start {
                id,
                ticket,
                fitting: Box::new(machine.fitting.clone()),
                suspended,
            };
            if !self.ask(job) {
                self.stop(
                    self.machines.len() - 1,
                    Some(String::from("the store is too busy")),
                );
                return;
            }
            self.file(
                FileJob::Sound { id, ticket },
                "The machine's sound could not be opened",
            );
            if let (Some(folder), Documents::Granted) = (machine.folder.clone(), &self.documents) {
                self.file(
                    FileJob::List { id, folder },
                    "The machine's folder could not be listed",
                );
            }
            self.show();
        }

        /// Machine `id`'s images were read for the start `ticket`, or not.
        fn started(
            &mut self,
            id: u32,
            ticket: u64,
            read: Result<(Box<store::Images>, Resumable), String>,
        ) {
            let Some(n) = self
                .index(id)
                .filter(|&n| self.machines[n].life == Life::Starting(ticket))
            else {
                return;
            };
            let (images, resume) = match read {
                Ok(read) => read,
                Err(why) => {
                    self.stop(n, Some(why));
                    return;
                }
            };
            let (resume, damaged) = match resume {
                Resumable::Kept(kept) => {
                    let (screen, suspended) = *kept;
                    (Some((screen, suspended.kept)), false)
                }
                Resumable::Nothing => (None, false),
                Resumable::Damaged => (None, true),
            };
            let resumed = resume.is_some();
            if let Err(why) = self.machines[n].started(images, resume, tairix_rt::clock_get()) {
                self.stop(n, Some(why));
                return;
            }
            if damaged {
                self.machines[n].say(
                    "Its saved state does not read; it starts afresh.",
                    StatusTone::Warning,
                );
                self.suspended.retain(|&held| held != id);
            }
            // A resumed machine has its snapshot's media, taken back from its
            // folder once it is restored.
            if !resumed {
                self.insert_folder_media(id);
            }
        }

        /// What of machine `id`'s folder its drives and deck held put in
        /// again, as a machine started afresh takes them.
        fn insert_folder_media(&mut self, id: u32) {
            let Some(machine) = self.library.machine(id).cloned() else {
                return;
            };
            let Some(folder) = machine.folder.clone() else {
                return;
            };
            if !self.documents_reachable(MediaOwed::Insert(id)) {
                return;
            }
            let model = machine.model();
            let purposes = PhysicalDrive::ALL
                .iter()
                .zip(&machine.discs)
                .map(|(drive, held)| (Purpose::Disc(*drive), held))
                .chain(core::iter::once((Purpose::Tape, &machine.tape)));
            for (purpose, held) in purposes {
                let Some(held) = held else {
                    continue;
                };
                if matches!(purpose, Purpose::Disc(_)) && !model.has_drives() {
                    continue;
                }
                let job = FileJob::FromFolder {
                    id,
                    folder: folder.clone(),
                    name: held.name.clone(),
                    purpose,
                    digest: None,
                };
                self.file(
                    job,
                    "A disc or tape of the machine's folder could not be opened",
                );
            }
        }

        /// Turn machine window `n` off, keeping its last screen if it ran,
        /// saying `why` it stopped where something went wrong.
        fn stop(&mut self, n: usize, why: Option<String>) {
            let mut window = self.machines.remove(n);
            report_unsaved(&window);
            let (_, _, sound_token) = machine_tokens(window.slot);
            window.retire(self.set, sound_token);
            if let Some(why) = why {
                self.refuse(&format!("\u{201c}{}\u{201d} stopped", window.name), &why);
            }
            if window.ran {
                if let Some(screen) = window.last_screen() {
                    let picture = self.pictures.size();
                    self.ask(Job::Keep {
                        id: window.id,
                        screen,
                        suspended: None,
                        picture,
                        why: Keeping::Off,
                    });
                }
            }
            if self.focused == Some(window.id) {
                self.focused = None;
            }
            self.close_sheet_over(window.window_id());
            let _ = window.into_pane().close(&mut self.client);
            self.show();
            self.end_once_left();
        }

        /// Machine window `n` closed: its machine suspended, its window held
        /// until it is kept; one not yet running is turned off.
        fn close(&mut self, n: usize) {
            let window = &mut self.machines[n];
            match window.life {
                Life::Running if window.live() => {
                    window.suspend(tairix_rt::clock_get().saturating_add(SUSPEND_GRACE_NS));
                }
                Life::Suspending(_) | Life::ShuttingDown(_) => {}
                Life::Starting(_) | Life::Running => self.stop(n, None),
            }
        }

        /// Machine window `n`'s snapshot came for its suspend: kept with its
        /// last screen, the digests of its folder's media recorded with it.
        fn suspended_whole(&mut self, n: usize, kept: Vec<u8>) {
            let Some(screen) = self.machines[n].last_screen() else {
                self.stop(n, None);
                return;
            };
            let id = self.machines[n].id;
            let rack = &self.machines[n].rack;
            let digest = |held: Option<(&String, &Vec<u8>, bool)>| {
                held.filter(|(_, _, of_folder)| *of_folder)
                    .map(|(name, image, _)| Held {
                        name: name.clone(),
                        digest: Some(folder::digest(image)),
                    })
            };
            let discs = PhysicalDrive::ALL.map(|drive| {
                digest(
                    rack.disc(drive)
                        .map(|disc| (&disc.name, &disc.image, disc.of_folder)),
                )
            });
            let tape = digest(
                rack.tape()
                    .map(|tape| (&tape.name, &tape.image, tape.of_folder)),
            );
            self.remember(id, |machine| {
                if machine.folder.is_some() {
                    machine.discs = discs;
                    machine.tape = tape;
                }
            });
            if !self.keeping.reserve() {
                self.stop(n, Some(String::from("there is no memory to keep it")));
                return;
            }
            let mut window = self.machines.remove(n);
            report_unsaved(&window);
            let (_, _, sound_token) = machine_tokens(window.slot);
            window.retire(self.set, sound_token);
            self.close_sheet_over(window.window_id());
            if self.focused == Some(id) {
                self.focused = None;
            }
            let at = tairix_machines::store::calendar_secs();
            let name = window.name.clone();
            self.keeping.suspend(
                (id, name),
                (screen, Suspended { kept, at }),
                window.into_pane(),
            );
            if let Some((_, screen, suspended)) = self.keeping.synced(id, true) {
                let picture = self.pictures.size();
                if !self.ask(Job::Keep {
                    id,
                    screen,
                    suspended,
                    picture,
                    why: Keeping::Suspended,
                }) {
                    self.state_kept(
                        id,
                        Keeping::Suspended,
                        Err(String::from("the store is too busy")),
                    );
                }
            }
            self.show();
        }

        /// Machine `id`'s state was kept as `why` says, or not.
        fn state_kept(&mut self, id: u32, why: Keeping, result: Result<(), String>) {
            match why {
                Keeping::Suspended => {
                    let Some(ended) = self.keeping.end(id) else {
                        return;
                    };
                    let _ = ended.window.close(&mut self.client);
                    match result {
                        Ok(()) => {
                            if !self.suspended.contains(&id)
                                && self.suspended.try_reserve(1).is_ok()
                            {
                                self.suspended.push(id);
                            }
                            record(events::MACHINE_SUSPENDED, events::MACHINE_SUSPENDED_MESSAGE);
                        }
                        Err(why) => self.refuse(
                            &format!("\u{201c}{}\u{201d} could not be suspended", ended.name),
                            &why,
                        ),
                    }
                    self.show();
                    self.end_once_left();
                }
                Keeping::SetAside | Keeping::Off => match result {
                    // What is kept now holds no snapshot.
                    Ok(()) => {
                        self.suspended.retain(|&held| held != id);
                        self.show();
                    }
                    Err(why) => app::report(
                        APP_NAME,
                        format_args!("a machine's last screen was not kept ({why})"),
                    ),
                },
            }
        }

        /// End once nothing is running, being kept or shown.
        fn end_once_left(&mut self) {
            if self.leaving.is_some()
                && self.machines.is_empty()
                && self.keeping.is_empty()
                && self.shelf.is_none()
            {
                self.quit = true;
            }
        }

        /// Leave: every machine running suspended, every other turned off, and
        /// the program ended once they are kept, or at a deadline.
        fn leave(&mut self) {
            self.close_sheet();
            if let Some(win) = self.shelf.take() {
                win.close(&mut self.client);
            }
            for n in (0..self.machines.len()).rev() {
                self.close(n);
            }
            self.leaving = Some(tairix_rt::clock_get().saturating_add(DRAIN_NS));
            self.end_once_left();
        }

        /// Machines that did not answer their suspend in time turned off, and
        /// leaving ended at its deadline.
        fn give_up(&mut self, now: u64) {
            for n in (0..self.machines.len()).rev() {
                let (Life::Suspending(by) | Life::ShuttingDown(Some(by))) = self.machines[n].life
                else {
                    continue;
                };
                if !self.machines[n].live() {
                    self.stop(n, Some(String::from("it stopped before it was done with")));
                } else if by <= now {
                    self.stop(n, Some(String::from("it did not answer in time")));
                }
            }
            if self.leaving.is_some_and(|by| by <= now) {
                self.quit = true;
            }
        }

        /// What the file worker landed.
        fn filed(&mut self, done: FileDone) {
            match done {
                FileDone::Documents(result) => {
                    self.documents = match result {
                        Ok(()) => Documents::Granted,
                        Err(why) => Documents::Refused(why),
                    };
                    self.documents_reached();
                }
                FileDone::Taken {
                    id,
                    purpose,
                    name,
                    bytes,
                    medium,
                    writable,
                    of_folder,
                } => self.taken(id, (purpose, name, bytes), (medium, writable, of_folder)),
                FileDone::Rebound {
                    id,
                    purpose,
                    name,
                    bytes,
                    medium,
                } => self.rebound(id, purpose, (name, bytes, medium)),
                FileDone::Written(Ok(())) | FileDone::Nothing => {}
                FileDone::Written(Err(why)) => self.refuse("A file could not be saved", &why),
                FileDone::Sound {
                    id,
                    ticket,
                    speaker,
                } => self.sounded(id, ticket, speaker),
                FileDone::FolderMade { id, folder } => self.folder_made(id, folder),
                FileDone::Listed { id, names } => {
                    if let (Some(n), Ok(names)) = (self.index(id), names) {
                        self.machines[n].offered = folder::offered(&names);
                    }
                }
                FileDone::FolderCopied { to, folder } => match folder {
                    Ok(folder) => self.remember(to, |machine| machine.folder = Some(folder)),
                    Err(why) => {
                        self.remember(to, |machine| {
                            machine.discs = [None, None];
                            machine.tape = None;
                        });
                        self.refuse("The machine's folder could not be copied", &why);
                    }
                },
                FileDone::Failed(why) => self.refuse("A file could not be used", &why),
            }
        }

        /// The documents folder was reached, or refused: each machine running
        /// told what its folder offers.
        fn documents_reached(&mut self) {
            if let Documents::Refused(why) = &self.documents {
                app::report(
                    APP_NAME,
                    format_args!("the documents folder cannot be reached ({why})"),
                );
                self.folders_owed.clear();
                self.media_owed.clear();
                return;
            }
            let lists: Vec<(u32, String)> = self
                .machines
                .iter()
                .filter_map(|window| {
                    Some((window.id, self.library.machine(window.id)?.folder.clone()?))
                })
                .collect();
            for (id, folder) in lists {
                self.file(
                    FileJob::List { id, folder },
                    "The machine's folder could not be listed",
                );
            }
            for window in &mut self.machines {
                window.offered.reachable = true;
            }
            self.ask_folders();
            for owed in core::mem::take(&mut self.media_owed) {
                if self.index(owed.id()).is_none() {
                    continue;
                }
                match owed {
                    MediaOwed::Insert(id) => self.insert_folder_media(id),
                    MediaOwed::Rebind { id, discs, tape } => {
                        self.rebind_folder_media(id, (discs, tape));
                    }
                }
            }
        }

        /// Machine `id`'s folder was made, or not: then the blank it was made
        /// for made in it.
        fn folder_made(&mut self, id: u32, folder: Result<String, String>) {
            let at = self.folders_owed.iter().position(|owed| owed.id == id);
            let made_for = at.map(|at| self.folders_owed.swap_remove(at).made_for);
            let folder = match folder {
                Ok(folder) => folder,
                Err(why) => {
                    self.refuse("The machine's folder could not be made", &why);
                    return;
                }
            };
            self.remember(id, |machine| machine.folder = Some(folder.clone()));
            let Some(machine) = self.library.machine(id) else {
                return;
            };
            let running = self.index(id).map(|n| &self.machines[n]);
            let blank = match made_for {
                Some(FolderFor::Blank(purpose)) => Some(purpose),
                Some(FolderFor::FirstDisc) => {
                    let empty = running.is_none_or(|window| {
                        !window.shown().discs[0] && window.rack.disc(PhysicalDrive::Zero).is_none()
                    });
                    (machine.model().has_drives() && machine.discs[0].is_none() && empty)
                        .then_some(Purpose::Disc(PhysicalDrive::Zero))
                }
                None => None,
            };
            let job = match blank {
                Some(purpose) => FileJob::New {
                    id,
                    folder,
                    purpose,
                },
                None if running.is_some() => FileJob::List { id, folder },
                None => return,
            };
            self.file(job, "The machine's folder could not be used");
        }

        /// A file was read for machine `id`: put in where it runs, or, made
        /// for one at rest, kept with it to be put in when it starts.
        fn taken(
            &mut self,
            id: u32,
            (purpose, name, bytes): (Purpose, String, Vec<u8>),
            (medium, writable, of_folder): (Option<u32>, bool, bool),
        ) {
            let Some(n) = self
                .index(id)
                .filter(|&n| self.machines[n].life == Life::Running)
            else {
                if of_folder {
                    let held = Some(Held { name, digest: None });
                    self.remember(id, |machine| match purpose {
                        Purpose::Disc(drive) => machine.discs[drive.index()] = held,
                        Purpose::Tape => machine.tape = held,
                        Purpose::Snapshot | Purpose::Rom(_) => {}
                    });
                }
                if let Some(medium) = medium {
                    let _ = self.files.submit(FileJob::Forget(medium));
                }
                return;
            };
            let window = &mut self.machines[n];
            match purpose {
                Purpose::Disc(drive) => {
                    let format = media::format_of(&name);
                    let disc = Disc {
                        name,
                        format,
                        medium,
                        of_folder,
                        image: bytes,
                    };
                    window.ask(Pending::InsertDisc {
                        drive,
                        write_protected: !writable,
                        disc,
                    });
                }
                Purpose::Tape => window.ask(Pending::InsertTape(Tape {
                    name,
                    medium,
                    of_folder,
                    image: bytes,
                })),
                Purpose::Snapshot => window.ask(Pending::RestoreSnapshot(bytes)),
                Purpose::Rom(_) => {}
            }
            if of_folder {
                if let Some(folder) = self
                    .library
                    .machine(id)
                    .and_then(|machine| machine.folder.clone())
                {
                    self.file(
                        FileJob::List { id, folder },
                        "The machine's folder could not be listed",
                    );
                }
            }
        }

        /// A resumed machine's disc or tape is its folder's file again: the
        /// file still holds what the machine has of it from its snapshot.
        fn rebound(
            &mut self,
            id: u32,
            purpose: Purpose,
            (name, image, medium): (String, Vec<u8>, u32),
        ) {
            let adopted = self.index(id).is_some_and(|n| {
                let rack = &mut self.machines[n].rack;
                match purpose {
                    Purpose::Disc(drive) => rack.adopt_disc(
                        drive,
                        Disc {
                            format: media::format_of(&name),
                            name,
                            medium: Some(medium),
                            of_folder: true,
                            image,
                        },
                    ),
                    Purpose::Tape => rack.adopt_tape(Tape {
                        name,
                        medium: Some(medium),
                        of_folder: true,
                        image,
                    }),
                    Purpose::Snapshot | Purpose::Rom(_) => false,
                }
            });
            if !adopted {
                let _ = self.files.submit(FileJob::Forget(medium));
            }
        }

        /// The files of machine `id`'s folder its snapshot's media came from,
        /// taken back as their media where each still holds what was written
        /// to it at the suspend.
        fn rebind_folder_media(
            &mut self,
            id: u32,
            (discs, tape): ([Option<Held>; 2], Option<Held>),
        ) {
            let Some(folder) = self
                .library
                .machine(id)
                .and_then(|machine| machine.folder.clone())
            else {
                return;
            };
            let purposes = PhysicalDrive::ALL
                .iter()
                .zip(&discs)
                .map(|(drive, held)| (Purpose::Disc(*drive), held))
                .chain(core::iter::once((Purpose::Tape, &tape)));
            let wanted = purposes
                .clone()
                .any(|(_, held)| held.as_ref().is_some_and(|held| held.digest.is_some()));
            if !wanted
                || !self.documents_reachable(MediaOwed::Rebind {
                    id,
                    discs: discs.clone(),
                    tape: tape.clone(),
                })
            {
                return;
            }
            for (purpose, held) in purposes {
                let Some(Held {
                    name,
                    digest: Some(digest),
                }) = held
                else {
                    continue;
                };
                let job = FileJob::FromFolder {
                    id,
                    folder: folder.clone(),
                    name: name.clone(),
                    purpose,
                    digest: Some(*digest),
                };
                self.file(
                    job,
                    "A disc or tape of the machine's folder could not be opened",
                );
            }
        }

        /// Whether the documents folder is reached, `owed` kept for when it is
        /// where it is still being asked for.
        fn documents_reachable(&mut self, owed: MediaOwed) -> bool {
            match self.documents {
                Documents::Granted => true,
                Documents::Unasked | Documents::Asked => {
                    let id = owed.id();
                    self.media_owed.retain(|held| held.id() != id);
                    if self.media_owed.try_reserve(1).is_ok() {
                        self.media_owed.push(owed);
                    }
                    false
                }
                Documents::Refused(_) => false,
            }
        }

        /// Machine `id`'s start `ticket`'s sound stream, if one opened.
        fn sounded(&mut self, id: u32, ticket: u64, speaker: Option<Speaker>) {
            let Some(speaker) = speaker else {
                return;
            };
            let Some(n) = self
                .index(id)
                .filter(|&n| self.machines[n].ticket == ticket)
            else {
                speaker.close();
                return;
            };
            let (_, _, token) = machine_tokens(self.machines[n].slot);
            let watched = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Add,
                WaitSourceKind::Port,
                speaker.port(),
                token,
            );
            if watched == 0 {
                self.machines[n].sound = Some(speaker);
            } else {
                speaker.close();
                self.machines[n].say(
                    "The sound stream cannot be waited on; the machine is silent.",
                    StatusTone::Warning,
                );
            }
        }

        /// What machine window `n`'s worker answered.
        fn machine_answered(&mut self, n: usize, frame: &[u8]) {
            match self.machines[n].answered(frame) {
                Event::Nothing => {}
                Event::Suspended(kept) => self.suspended_whole(n, kept),
                Event::Snapshot(kept) => {
                    let suggested = format!("{}.snapshot", self.machines[n].name);
                    self.machines[n].offer(Saving {
                        suggested,
                        holding: Holding::Snapshot,
                        bytes: kept,
                    });
                    self.next_save(n);
                }
                Event::Files(work) => {
                    self.file_work(work);
                    self.next_save(n);
                    // A machine shutting down keeps what its folder lent it.
                    if self.machines[n].life == Life::Running {
                        self.folder_media_changed(n);
                    }
                }
                Event::Resumed(work) => {
                    let id = self.machines[n].id;
                    self.file_work(work);
                    if let Some(held) = self
                        .library
                        .machine(id)
                        .map(|machine| (machine.discs.clone(), machine.tape.clone()))
                    {
                        self.rebind_folder_media(id, held);
                    }
                    self.next_save(n);
                    if let Some(screen) = self.machines[n].last_screen() {
                        let picture = self.pictures.size();
                        self.ask(Job::Keep {
                            id,
                            screen,
                            suspended: None,
                            picture,
                            why: Keeping::SetAside,
                        });
                    }
                    self.machines[n].say("Resumed.", StatusTone::Ink);
                    record(events::MACHINE_RESUMED, events::MACHINE_RESUMED_MESSAGE);
                }
                Event::NotResumed { what, detail } => {
                    let owner = Some(self.machines[n].window_id());
                    self.refuse_over(owner, what, detail);
                    let id = self.machines[n].id;
                    self.insert_folder_media(id);
                }
                Event::Unsaved(detail) => {
                    self.stop(n, Some(format!("Its state could not be saved. {detail}")));
                }
                Event::Refused { what, detail, work } => {
                    self.file_work(work);
                    let owner = Some(self.machines[n].window_id());
                    self.refuse_over(owner, what, detail);
                }
            }
        }

        /// Machine `id` turned off once it is shutting down, its media out and
        /// every save they offered answered.
        fn shut_down_if_done(&mut self, id: u32) {
            let Some(n) = self.index(id) else {
                return;
            };
            let window = &mut self.machines[n];
            if !matches!(window.life, Life::ShuttingDown(_)) || !window.settled() {
                return;
            }
            if window.rack.is_settled() {
                self.stop(n, None);
            } else {
                window.life = Life::ShuttingDown(None);
            }
        }

        /// The files a machine's media came from written back and let go, in
        /// order.
        fn file_work(&mut self, work: Vec<FileWork>) {
            for job in work {
                match job {
                    FileWork::WriteBack { medium, bytes } => self.file(
                        FileJob::WriteBack { medium, bytes },
                        "A disc or tape could not be written back",
                    ),
                    FileWork::Forget(medium) => {
                        let _ = self.files.submit(FileJob::Forget(medium));
                    }
                }
            }
        }

        /// What of the machine's folder its drives and deck hold, kept with
        /// it.
        fn folder_media_changed(&mut self, n: usize) {
            let window = &self.machines[n];
            let id = window.id;
            let held = |name: Option<&String>| {
                name.map(|name| Held {
                    name: name.clone(),
                    digest: None,
                })
            };
            let discs = PhysicalDrive::ALL.map(|drive| {
                held(
                    window
                        .rack
                        .disc(drive)
                        .filter(|disc| disc.of_folder)
                        .map(|disc| &disc.name),
                )
            });
            let tape = held(
                window
                    .rack
                    .tape()
                    .filter(|tape| tape.of_folder)
                    .map(|tape| &tape.name),
            );
            let changed = self
                .library
                .machine(id)
                .is_some_and(|machine| machine.discs != discs || machine.tape != tape);
            if changed {
                self.remember(id, |machine| {
                    machine.discs = discs;
                    machine.tape = tape;
                });
            }
        }

        /// The next save offered by machine window `n`, once no picker is
        /// open.
        fn next_save(&mut self, n: usize) {
            while let Some((suggested, holding)) = self.machines[n].rack.choose_to_save() {
                let window_id = self.machines[n].window_id();
                let opened = DocumentName::new(&suggested)
                    .map_err(|_| String::from("Its name could not be offered."))
                    .and_then(|suggested| {
                        self.client
                            .pick_file(
                                window_id,
                                PickPurpose::Save {
                                    suggested,
                                    formats: machine::save_formats(holding),
                                },
                            )
                            .map_err(|err| format!("{err}"))
                    });
                match opened {
                    Ok(()) => return,
                    Err(why) => {
                        self.machines[n].rack.chosen();
                        self.refuse_over(Some(window_id), "It could not be saved", &why);
                    }
                }
            }
        }

        /// A picker opened over machine window `n` for a file to open as
        /// `purpose`.
        fn pick_open(&mut self, n: usize, purpose: Purpose) {
            let window_id = self.machines[n].window_id();
            if !self.machines[n].rack.choose_to_open(purpose) {
                self.machines[n].say(
                    "Another file is being chosen; finish with it first.",
                    StatusTone::Warning,
                );
                return;
            }
            if let Err(err) = self.client.pick_file(window_id, PickPurpose::Open) {
                self.machines[n].rack.chosen();
                self.refuse_over(
                    Some(window_id),
                    "The file picker could not be opened",
                    &format!("{err}"),
                );
                self.next_save(n);
            }
        }

        /// A file picked over machine window `n`: its name asked, the grant
        /// held until it lands.
        fn picked(&mut self, n: usize, handle: u64, writable: bool) {
            let window_id = self.machines[n].window_id();
            if let Some((stale, _)) = self.machines[n].picked.replace((handle, writable)) {
                drop(File::from_delegation(stale));
            }
            if let Err(err) = self.client.ask_picked_name(window_id) {
                self.machines[n].picked = None;
                drop(File::from_delegation(handle));
                self.machines[n].rack.chosen();
                self.refuse_over(
                    Some(window_id),
                    "The picked file could not be named",
                    &format!("{err}"),
                );
                self.next_save(n);
            }
        }

        fn pick_cancelled(&mut self, n: usize) {
            if let Some((grant, _)) = self.machines[n].picked.take() {
                drop(File::from_delegation(grant));
            }
            self.machines[n].rack.chosen();
            self.next_save(n);
            let id = self.machines[n].id;
            self.shut_down_if_done(id);
        }

        /// The name of the file picked over machine window `n`.
        fn named(&mut self, n: usize, name: Result<String, Errno>) {
            let Some((grant, writable)) = self.machines[n].picked.take() else {
                return;
            };
            let id = self.machines[n].id;
            match self.machines[n].rack.chosen() {
                Some(Chosen::Open(purpose)) => {
                    let name = name.unwrap_or_else(|_| String::from("Untitled"));
                    self.file(
                        FileJob::Take {
                            id,
                            grant,
                            writable,
                            name,
                            purpose,
                        },
                        "The file could not be used",
                    );
                }
                Some(Chosen::Save(bytes)) => {
                    self.file(FileJob::SaveTo { grant, bytes }, "It could not be saved");
                }
                None => drop(File::from_delegation(grant)),
            }
            self.next_save(n);
            self.shut_down_if_done(id);
        }

        /// What row `command` of machine window `n`'s menu asks.
        fn command(&mut self, n: usize, command: Command) {
            let id = self.machines[n].id;
            if !matches!(command, Command::Settings | Command::Library) {
                let busy = match self.machines[n].life {
                    Life::Running => None,
                    Life::Starting(_) => Some("It is still starting."),
                    Life::Suspending(_) | Life::ShuttingDown(_) => {
                        Some("It is being put away; it takes nothing more.")
                    }
                };
                if let Some(busy) = busy {
                    self.machines[n].say(busy, StatusTone::Muted);
                    return;
                }
            }
            match command {
                Command::InsertDisc(drive) => self.pick_open(n, Purpose::Disc(drive)),
                Command::FolderDisc(drive, at) => {
                    self.insert_from_folder(n, Purpose::Disc(drive), at);
                }
                Command::NewDisc(drive) => self.new_medium(n, Purpose::Disc(drive)),
                Command::EjectDisc(drive) => self.machines[n].ask(Pending::EjectDisc(drive)),
                Command::SaveDisc(drive) => {
                    if let Some(disc) = self.machines[n].rack.disc(drive) {
                        let saving = Saving {
                            suggested: disc.name.clone(),
                            holding: Holding::Disc,
                            bytes: disc.image.clone(),
                        };
                        self.machines[n].offer(saving);
                        self.next_save(n);
                    }
                }
                Command::InsertTape => self.pick_open(n, Purpose::Tape),
                Command::FolderTape(at) => self.insert_from_folder(n, Purpose::Tape, at),
                Command::NewTape => self.new_medium(n, Purpose::Tape),
                Command::RewindTape => self.machines[n].ask(Pending::RewindTape),
                Command::Record => {
                    let on = !self.machines[n].recording();
                    self.machines[n].ask(Pending::RecordTape(on));
                }
                Command::EjectTape => self.machines[n].ask(Pending::EjectTape),
                Command::Break => self.machines[n].press_break(),
                Command::Reset => self.machines[n].ask(Pending::PowerOn),
                Command::Pause => self.machines[n].toggle_pause(),
                Command::Speed(speed) => self.machines[n].set_speed(speed),
                Command::FullScreen => self.machines[n].toggle_full_screen(&mut self.client),
                Command::Scaling(scaling) => self.restyle(id, |machine| machine.scaling = scaling),
                Command::ScanLines => self.restyle(id, |machine| {
                    machine.tube = Effect::ScanLines.toggled(machine.tube);
                }),
                Command::Keyboard(mapping) => {
                    self.machines[n].set_mapping(mapping);
                    self.remember(id, |machine| machine.keyboard = mapping);
                }
                Command::Paste => {
                    let window_id = self.machines[n].window_id();
                    if let Err(err) = self.client.ask_clipboard(window_id) {
                        self.machines[n].say(
                            format!("The clipboard could not be read ({err})."),
                            StatusTone::Warning,
                        );
                    }
                }
                Command::SaveSnapshot => self.machines[n].ask(Pending::TakeSnapshot),
                Command::RestoreSnapshot => self.pick_open(n, Purpose::Snapshot),
                Command::Settings => {
                    let owner = Some(self.machines[n].window_id());
                    self.open_settings(id, owner);
                }
                Command::Library => self.open_shelf(),
                Command::ShutDown => {
                    self.machines[n]
                        .shut_down(tairix_rt::clock_get().saturating_add(SUSPEND_GRACE_NS));
                }
            }
        }

        /// Machine `id` shown as `change` restyles it, in its window and its
        /// record.
        fn restyle(&mut self, id: u32, change: impl FnOnce(&mut Machine)) {
            self.remember(id, change);
            if let (Some(n), Some(machine)) = (self.index(id), self.library.machine(id)) {
                let (tube, scaling) = (machine.tube, machine.scaling);
                self.machines[n].set_look(tube, scaling);
            }
        }

        /// The image of machine window `n`'s folder listed at `at` put in as
        /// `purpose`.
        fn insert_from_folder(&mut self, n: usize, purpose: Purpose, at: u8) {
            let window = &self.machines[n];
            let names = match purpose {
                Purpose::Disc(_) => &window.offered.discs,
                _ => &window.offered.tapes,
            };
            let (Some(name), Some(folder)) = (
                names.get(usize::from(at)).cloned(),
                self.library
                    .machine(window.id)
                    .and_then(|machine| machine.folder.clone()),
            ) else {
                return;
            };
            let job = FileJob::FromFolder {
                id: window.id,
                folder,
                name,
                purpose,
                digest: None,
            };
            self.file(job, "The file could not be opened");
        }

        /// A blank disc or tape made in machine window `n`'s folder and put in;
        /// the folder made first where it has none.
        fn new_medium(&mut self, n: usize, purpose: Purpose) {
            let id = self.machines[n].id;
            if let Some(folder) = self
                .library
                .machine(id)
                .and_then(|machine| machine.folder.clone())
            {
                self.file(
                    FileJob::New {
                        id,
                        folder,
                        purpose,
                    },
                    "It could not be made",
                );
            } else {
                self.make_folder(id, FolderFor::Blank(purpose));
                let follows = match purpose {
                    Purpose::Disc(_) => "Its folder is being made; the blank disc follows.",
                    _ => "Its folder is being made; the blank tape follows.",
                };
                self.machines[n].say(follows, StatusTone::Muted);
            }
        }

        /// A document handed to the application: to the machine whose window
        /// had the keyboard last where it takes it, else to the first machine
        /// of the library that does, started for it.
        fn handed(&mut self, handed: Handed) {
            let takes = |model: Model| Purpose::of(&handed.name, model).is_some();
            let running = self
                .focused
                .and_then(|id| self.index(id))
                .filter(|&n| takes(self.machines[n].model))
                .or_else(|| self.machines.iter().position(|window| takes(window.model)));
            if let Some(n) = running {
                self.give(n, handed);
                return;
            }
            let at_rest = self
                .library
                .machines()
                .iter()
                .find(|machine| takes(machine.model()) && self.standing(machine.id).startable())
                .map(|machine| machine.id);
            let Some(id) = at_rest else {
                let name = handed.name.clone();
                handed.drop_file();
                self.open_shelf();
                self.refuse(
                    &format!("\u{201c}{name}\u{201d} could not be opened"),
                    "No machine in the library takes it; make one that does.",
                );
                return;
            };
            self.start(id);
            if self.index(id).is_some() {
                if let Some((_, stale)) = self.waiting.replace((id, handed)) {
                    stale.drop_file();
                }
            } else {
                handed.drop_file();
            }
        }

        /// `handed` given to machine window `n`.
        fn give(&mut self, n: usize, handed: Handed) {
            let window = &self.machines[n];
            let Some(purpose) = Purpose::of(&handed.name, window.model) else {
                handed.drop_file();
                return;
            };
            let id = window.id;
            let Handed {
                file,
                grant,
                writable,
                name,
            } = handed;
            let job = match (file, grant) {
                (Some(file), _) => FileJob::Handed {
                    id,
                    file: Some(file),
                    writable,
                    name,
                    purpose,
                },
                (None, Some(grant)) => FileJob::Take {
                    id,
                    grant,
                    writable,
                    name,
                    purpose,
                },
                (None, None) => return,
            };
            self.file(job, "The file could not be used");
        }

        /// An open target arrived.
        fn opened(&mut self, target: Opened) {
            match target {
                Opened::Document {
                    name,
                    grant,
                    writable,
                } => self.handed(Handed {
                    file: None,
                    grant: Some(grant),
                    writable,
                    name,
                }),
                Opened::Placed {
                    window_id,
                    name,
                    grant,
                    ..
                } => {
                    let handed = Handed {
                        file: None,
                        grant: Some(grant),
                        writable: false,
                        name,
                    };
                    match self
                        .machines
                        .iter()
                        .position(|window| window.window_id() == window_id)
                    {
                        Some(n) => self.give(n, handed),
                        None => handed.drop_file(),
                    }
                }
                Opened::Path(_) | Opened::Image(_) | Opened::Pane(_) => {}
            }
        }

        /// The disc or tape the application was opened with.
        fn launched_document(&mut self) {
            let writable = match tairix_rt::arg(1) {
                Some(arg) if arg == DOCUMENT_WRITABLE_ROLE_ARG => true,
                Some(arg) if arg == DOCUMENT_ROLE_ARG => false,
                _ => return,
            };
            let name = tairix_rt::arg(2)
                .and_then(|raw| core::str::from_utf8(raw).ok())
                .map(|path| String::from(path.rsplit('/').next().unwrap_or(path)))
                .unwrap_or_default();
            self.launched = Some(Handed {
                file: Some(File::adopt(STDIN)),
                grant: None,
                writable,
                name,
            });
        }

        /// Act on an event for the whole application.
        fn app_event(&mut self, event: &WindowEvent) {
            match *event {
                WindowEvent::AppBarDefault | WindowEvent::AppBarMenu { .. }
                    if self.leaving.is_some() => {}
                WindowEvent::AppBarDefault | WindowEvent::AppBarMenu { .. } => {
                    match library_window::bar_ask(event) {
                        Some(BarAsk::Quit) => self.leave(),
                        Some(BarAsk::Library) => self.open_shelf(),
                        None => {}
                    }
                }
                WindowEvent::OpenRequested => {
                    if let Err(err) = self.client.ask_open_targets() {
                        app::report(
                            APP_NAME,
                            format_args!("what was handed over could not be asked for ({err})"),
                        );
                    }
                }
                WindowEvent::AlertClosed { alert_id, outcome } => {
                    if let Some((asked, id)) = self.asking {
                        if asked == alert_id {
                            self.asking = None;
                            if outcome == (AlertOutcome::Chosen { index: 0 }) {
                                self.delete(id);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        /// Route one window event to the window it names.
        fn handle(&mut self, event: &WindowEvent) {
            match *event {
                WindowEvent::DocumentsFolder { handle, .. } => {
                    self.file(
                        FileJob::Documents(handle),
                        "The documents folder could not be opened",
                    );
                    return;
                }
                WindowEvent::DocumentsFolderRefused { refusal, .. } => {
                    self.documents = Documents::Refused(format!("{refusal:?}"));
                    self.documents_reached();
                    return;
                }
                _ => {}
            }
            let Some(window_id) = event.window_id() else {
                self.app_event(event);
                return;
            };
            if self.owner() == Some(window_id) {
                self.shelf_event(event);
            } else if self.sheet.as_ref().is_some_and(|win| win.id() == window_id) {
                self.sheet_event(event);
            } else if let Some(n) = self
                .machines
                .iter()
                .position(|window| window.window_id() == window_id)
            {
                self.machine_event(n, event);
            } else {
                app::release_carried(event);
            }
        }

        /// One event for machine window `n`.
        fn machine_event(&mut self, n: usize, event: &WindowEvent) {
            match *event {
                WindowEvent::CloseRequested { .. } => self.close(n),
                WindowEvent::Key { key, .. } => self.machines[n].key(&key),
                WindowEvent::Focus { focused, .. } => {
                    if focused {
                        self.focused = Some(self.machines[n].id);
                    } else {
                        self.machines[n].release_keys();
                    }
                }
                WindowEvent::Pointer {
                    x,
                    y,
                    action: PointerAction::Pressed(PointerButtonCode::Secondary),
                    ..
                } => self.open_machine_menu(n, (x, y)),
                WindowEvent::MenuChosen { open_id, item, .. } => {
                    self.machine_menu(n, open_id, Some(item), true);
                }
                WindowEvent::MenuClosed {
                    open_id, outcome, ..
                } => {
                    let item = match outcome {
                        MenuOutcome::Chosen(item) => Some(item),
                        _ => None,
                    };
                    self.machine_menu(n, open_id, item, false);
                }
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    state,
                    ..
                } => self.machines[n].resized(
                    &mut self.client,
                    (width_px, height_px),
                    state.is_fullscreen(),
                ),
                WindowEvent::RedrawRequested { .. } => self.machines[n].owe_whole(),
                WindowEvent::ContentReleased { .. } => self.machines[n].released(),
                WindowEvent::FilePicked {
                    handle, writable, ..
                } => self.picked(n, handle, writable),
                WindowEvent::PickCancelled { .. } => self.pick_cancelled(n),
                WindowEvent::FileOffered {
                    window_id, serial, ..
                } => {
                    if let Err(err) = self.client.ask_offered_file(window_id, serial) {
                        app::report(
                            APP_NAME,
                            format_args!("a dropped file could not be judged ({err})"),
                        );
                    }
                }
                _ => {}
            }
        }

        fn open_machine_menu(&mut self, n: usize, (x, y): (u32, u32)) {
            let window = &self.machines[n];
            let mut rows = AppMenu::EMPTY;
            let built =
                menu::push_rows(&mut rows, (&window.shown(), &window.offered), Place::Window);
            let at = |n: u32| i32::try_from(n).map_err(|_| Errno::OutOfRange);
            let window_id = window.window_id();
            let opened = built.and_then(|()| {
                let anchor = WindowRegion::new(at(x)?, at(y)?, 0, 0)?;
                self.client.open_menu(window_id, anchor, &rows)
            });
            match opened {
                Ok(open) => self.machines[n].menu = Some(Followed::new(open, (), rows)),
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("the menu could not be opened ({err})"),
                ),
            }
        }

        fn machine_menu(
            &mut self,
            n: usize,
            open_id: u64,
            item: Option<AppMenuItemId>,
            kept: bool,
        ) {
            if self.machines[n]
                .menu
                .as_ref()
                .is_none_or(|up| up.open() != open_id)
            {
                return;
            }
            if !kept {
                self.machines[n].menu = None;
            }
            if let Some(command) = item.and_then(menu::command) {
                self.command(n, command);
            }
        }

        /// What the client's answers say.
        fn answer(&mut self, answer: Answer) {
            match answer {
                Answer::FrameReturned { window_id } => {
                    if let Some(win) = self.shelf.as_mut().filter(|win| win.id() == window_id) {
                        let _ = win.present_deferred(&mut self.client);
                    } else if let Some(win) =
                        self.sheet.as_mut().filter(|win| win.id() == window_id)
                    {
                        let _ = win.present_deferred(&mut self.client);
                    } else if let Some(window) = self
                        .machines
                        .iter_mut()
                        .find(|w| w.window_id() == window_id)
                    {
                        window.present_deferred(&mut self.client);
                    }
                }
                Answer::PickedName { window_id, name } => {
                    if self.sheet.as_ref().is_some_and(|win| win.id() == window_id) {
                        self.rom_named(name);
                    } else if let Some(n) = self
                        .machines
                        .iter()
                        .position(|w| w.window_id() == window_id)
                    {
                        self.named(n, name);
                    }
                }
                Answer::OfferedFile(offer) => {
                    let ours = offer
                        .media
                        .as_deref()
                        .ok()
                        .and_then(MediaType::from_media_str)
                        .is_some_and(|media| {
                            matches!(
                                media,
                                MediaType::AcornDfsDisc | MediaType::TapeUef | MediaType::TapeCsw
                            )
                        });
                    let _ = self
                        .client
                        .answer_file_offer(offer.window_id, offer.serial, ours);
                }
                Answer::OpenTarget(Ok(Some(target))) => self.opened(target),
                Answer::OpenTarget(Err(err)) => {
                    app::report(APP_NAME, format_args!("a file to open was lost ({err})"));
                }
                Answer::Clipboard {
                    window_id,
                    contents,
                } => {
                    let Some(n) = self
                        .machines
                        .iter()
                        .position(|w| w.window_id() == window_id)
                    else {
                        return;
                    };
                    match contents {
                        Ok(Some((ClipboardKind::Text, bytes))) => {
                            self.machines[n].type_text(&String::from_utf8_lossy(&bytes));
                        }
                        Ok(_) => {
                            self.machines[n].say("The clipboard holds no text.", StatusTone::Muted);
                        }
                        Err(err) => self.machines[n].say(
                            format!("The clipboard could not be read ({err})."),
                            StatusTone::Warning,
                        ),
                    }
                }
                _ => {}
            }
        }

        /// Serve what woke the loop: the desktop, the workers, and each
        /// machine's session and sound.
        fn woken(&mut self, woke: Woke, now: u64) {
            if woke.desktop {
                self.desktop_moved();
            }
            if woke.members & (member_bit(STORE_TOKEN) | member_bit(FILES_TOKEN)) != 0 {
                self.collect();
            }
            for n in (0..self.machines.len()).rev() {
                let (read, write, sound) = machine_tokens(self.machines[n].slot);
                if woke.members & member_bit(sound) != 0 {
                    if let Some(speaker) = self.machines[n].sound.as_mut() {
                        speaker.drain();
                    }
                }
                let ready = (
                    woke.members & member_bit(read) != 0,
                    woke.members & member_bit(write) != 0,
                );
                let id = self.machines[n].id;
                for frame in self.machines[n].serve(now, ready) {
                    // An answer may end the machine's window.
                    let Some(n) = self.index(id) else {
                        break;
                    };
                    self.machine_answered(n, &frame);
                    self.shut_down_if_done(id);
                }
            }
            for window in &mut self.machines {
                window.restart_due(now);
                window.step(now);
            }
            self.give_up(now);
            self.deliver_waiting();
            if let Some(win) = &mut self.shelf {
                let looks = (self.desktop.scale(), self.themes.active());
                win.step(now, looks, &self.pictures);
            }
        }

        /// A document waiting for a machine starting, given once it runs.
        fn deliver_waiting(&mut self) {
            let Some((id, _)) = &self.waiting else {
                return;
            };
            let id = *id;
            match self.index(id) {
                Some(n) if self.machines[n].life == Life::Running => {
                    if let Some((_, handed)) = self.waiting.take() {
                        self.give(n, handed);
                    }
                }
                Some(_) => {}
                None => {
                    if let Some((_, handed)) = self.waiting.take() {
                        handed.drop_file();
                    }
                }
            }
        }

        /// The desktop's look or scale changed.
        fn desktop_moved(&mut self) {
            match app::adopt_desktop(&mut self.desktop, &mut self.themes) {
                Ok(true) => {}
                Ok(false) => return,
                Err(err) => {
                    app::report(
                        APP_NAME,
                        format_args!("the desktop's new look could not be read ({err})"),
                    );
                    return;
                }
            }
            let size = Shelf::picture_size(self.desktop.scale(), self.themes.active());
            if self.pictures.resize(size) {
                if let Some(picture) = size {
                    let ids = self
                        .library
                        .machines()
                        .iter()
                        .map(|machine| machine.id)
                        .collect();
                    self.ask(Job::Pictures { ids, picture });
                }
            }
            let looks = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.shelf {
                win.relook(looks, &self.pictures);
            }
            if let Some(win) = &mut self.sheet {
                win.relook(&mut self.client, looks);
            }
            for window in &mut self.machines {
                window.owe_whole();
            }
        }

        /// Ask each machine for what waits, present every window.
        fn settle(&mut self, now: u64) {
            for window in &mut self.machines {
                window.pump(now);
                window.sync(now);
            }
            self.follow_menus();
            let (desktop, theme) = (&self.desktop, self.themes.active());
            for window in &mut self.machines {
                window.paint(&mut self.client, (desktop, theme), (now, &self.pool));
            }
            if let Some(win) = &mut self.shelf {
                let _ = win.present(&mut self.client);
            }
            if let Some(win) = &mut self.sheet {
                let _ = win.present(&mut self.client);
            }
        }

        /// When the loop next wants to run: a machine's, the library's held
        /// press, or leaving's end.
        fn due(&mut self, now: u64) -> Option<u64> {
            let machines = self
                .machines
                .iter_mut()
                .filter_map(|window| window.due(now))
                .min();
            let scroll = self.shelf.as_mut().and_then(|win| win.deadline(now));
            [machines, scroll, self.leaving].into_iter().flatten().min()
        }

        /// What is still owed the store kept, within a deadline, as the program
        /// ends.
        fn shutdown(&mut self) {
            self.close_sheet();
            if let Some(win) = self.shelf.take() {
                win.close(&mut self.client);
            }
            while !self.machines.is_empty() {
                self.stop(0, None);
            }
            if self.writes.failed() {
                self.save();
            }
            let deadline = tairix_rt::clock_get().saturating_add(DRAIN_NS);
            while self.store.outstanding() {
                match self.store.wait_until(deadline) {
                    Some(Done::Recorded(result)) => self.recorded(result),
                    Some(Done::Kept {
                        id, why, result, ..
                    }) => self.state_kept(id, why, result),
                    Some(_) => {}
                    None => break,
                }
            }
            while self.files.outstanding() {
                match self.files.wait_until(deadline) {
                    Some(FileDone::Sound {
                        speaker: Some(speaker),
                        ..
                    }) => speaker.close(),
                    Some(_) => {}
                    None => break,
                }
            }
            if !self.keeping.is_empty() {
                app::report(
                    APP_NAME,
                    format_args!(
                        "{} suspended machines could not be kept in time, and start afresh",
                        self.keeping.len()
                    ),
                );
            }
        }
    }

    tairix_rt::entry!(main);

    fn main() -> i32 {
        if session_worker_role() {
            return match spawn_share().ok().and_then(MachineService::new) {
                Some(mut service) => serve_session_stdio(&mut service).exit_code(),
                None => 1,
            };
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }
        run()
    }

    /// The desktop, the workers and the library window brought up, and the
    /// library read.
    fn bring_up() -> Result<(App, ProcId), i32> {
        let mut client = WindowClient::new(app::RtWindowTransport);
        let (desktop, themes) = app::bring_up_desktop(&mut client)
            .map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        let binding =
            app::bind_event_mailbox().map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        let (endpoint, set) = (binding.endpoint(), binding.set());
        let no_worker = || app::fail(APP_NAME, app::EXIT_NO_WINDOW, "no memory for the workers");
        let store = Worker::queued(
            store::serve,
            (),
            tairix_rt::sync::WorkerWake::create(),
            STORE_JOBS,
        )
        .map(Arc::new)
        .map_err(|_| no_worker())?;
        app::start_worker(APP_NAME, &store, set, STORE_TOKEN, "store");
        let files = Worker::queued(
            files::serve,
            files::Held::new(),
            tairix_rt::sync::WorkerWake::create(),
            FILE_JOBS,
        )
        .map(Arc::new)
        .map_err(|_| no_worker())?;
        app::start_worker(APP_NAME, &files, set, FILES_TOKEN, "files");
        if let Err(why) = library_window::declare_bar(&mut client, endpoint) {
            app::report(APP_NAME, why);
        }
        let picture = Shelf::picture_size(desktop.scale(), themes.active());
        let online = tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport);
        let mut pictures = Pictures::default();
        pictures.resize(picture);
        let mut app = App {
            client,
            endpoint,
            set,
            desktop,
            themes,
            store,
            files,
            library: Library::new(),
            shelving: Shelving::Reading,
            writes: Writes::default(),
            shelf: None,
            sheet: None,
            card_menu: None,
            asking: None,
            pictures,
            suspended: Vec::new(),
            keeping: Suspends::new(),
            machines: Vec::new(),
            installed: Vec::new(),
            documents: Documents::Unasked,
            tickets: 0,
            pool: LazyPool::new(APP_NAME, online, "the picture"),
            installing: None,
            focused: None,
            waiting: None,
            launched: None,
            sheet_owner: None,
            folders_owed: Vec::new(),
            media_owed: Vec::new(),
            leaving: None,
            quit: false,
        };
        if !app.ask(Job::Load { picture }) {
            return Err(app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "the store would not take the library's read",
            ));
        }
        app.open_shelf();
        let server = app.client.session().ok_or_else(|| {
            app::fail(
                APP_NAME,
                app::EXIT_CHANNEL_LOST,
                "the desktop did not identify itself",
            )
        })?;
        Ok((app, server))
    }

    fn run() -> i32 {
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
        let (mut app, server) = match bring_up() {
            Ok(up) => up,
            Err(code) => return code,
        };
        app.launched_document();
        let store = Arc::clone(&app.store);
        let files = Arc::clone(&app.files);
        let _store = WorkerGuard::new(&store);
        let _files = WorkerGuard::new(&files);
        let due = Cell::new(None);
        let woke = Cell::new(Woke::default());
        let events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(app.endpoint, server),
            set: app.set,
            store: &store,
            files: &files,
            due: &due,
            woke: &woke,
        });
        let code = serve(&mut app, events, (&due, &woke));
        app.shutdown();
        code
    }

    /// The loop: what woke it served, the windows' events taken, each machine
    /// asked for its next slice, and one paint.
    fn serve(
        app: &mut App,
        mut events: WindowEvents<RtEventSource<'_>>,
        (due, woke): (&Cell<Option<u64>>, &Cell<Woke>),
    ) -> i32 {
        let mut waited = Ok(None);
        loop {
            let now = tairix_rt::clock_get();
            app.woken(woke.replace(Woke::default()), now);
            loop {
                match waited {
                    Ok(Some(event)) => app.handle(&event),
                    Ok(None) => break,
                    Err(EventError::Undecodable(_)) => {}
                    Err(EventError::Refused(refused)) => app::report(APP_NAME, refused),
                    Err(EventError::Mailbox(_)) => {
                        return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "event channel lost");
                    }
                }
                waited = events.try_wait(&mut app.client);
            }
            while let Some(answer) = app.client.take_answer() {
                app.answer(answer);
            }
            if app.quit {
                return 0;
            }
            app.settle(now);
            due.set(app.due(now));
            waited = events.wait(&mut app.client);
        }
    }
}

#[cfg(not(freestanding))]
fn main() {}
