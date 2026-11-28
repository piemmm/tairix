//! The front end: the desktop brought up, the workers started, the shared
//! session's window opened and its documents folder asked for, and the loop
//! that serves every window, session and worker.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::process::{DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, STDIN};
use tairix_abi::window_ipc::{DocumentsRefusal, PickPurpose, WindowEvent};
use tairix_abi::zone::ZoneName;
use tairix_abi::Errno;
use tairix_appdata::{
    collect_published, see_out_published, submit_published, Opening, Publication, PublishWorker,
    Refusal as StoreRefusal, RtHost, Settings as Store,
};
use tairix_collections::OrderedMap;
use tairix_geometry::{Region, Scale};
use tairix_input::InputEvent;
use tairix_machines::window::{Sheet, SheetEvent, SheetWindow};
use tairix_raster::Surface;
use tairix_riscos_app::begin::{real_time, zone};
use tairix_riscos_app::events::{LAUNCH_PLACED, SESSION_STARTED};
use tairix_riscos_app::provenance::{placement, Asked as Placed, Placement, Volume};
use tairix_riscos_app::settings::{Edited, Form, Settings};
use tairix_riscos_app::tasks::{Command, Kind};
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::relay::Side;
use tairix_riscos_kernel::USA_COUNTRY;
use tairix_riscos_session::host::{Host, Unstarted};
use tairix_riscos_session::protocol::Begin;
use tairix_rt::sync::WorkerWake;
use tairix_rt::work::{Worker, WorkerGuard};
use tairix_rt::{File, LogSink};
use tairix_sandbox::rt::SessionMembers;
use tairix_theme::{Theme, ThemeRegistry};
use tairix_window::app::{self, Wake};
use tairix_window::{
    Answer, Desktop, EventDrain, EventError, EventMailbox, EventSource, Parked, PostedRequest,
    Target, WindowClient, WindowEvents,
};

use crate::files::{self, FileWorker, Handed, Start};
use crate::fonts::{self, FontWorker};
use crate::session::{Around, Need, Session};

/// The name this program states its own refusals under: its manifest's.
pub const APP_NAME: &str = "TAIRiXBridge";

/// The window client.
pub type Client = WindowClient<app::RtWindowTransport>;

/// A session, by the order it was made in: never used again.
pub type SessionId = u64;

/// Wait-set tokens: the workers', then each session's read and write pipes
/// by its id.
const FILES_TOKEN: u64 = app::FIRST_APP_TOKEN;
const FONTS_TOKEN: u64 = FILES_TOKEN + 1;
const SETTINGS_TOKEN: u64 = FONTS_TOKEN + 1;
const FIRST_SESSION_TOKEN: u64 = SETTINGS_TOKEN + 1;

/// Session `id`'s read and write tokens.
const fn session_tokens(id: SessionId) -> (u64, u64) {
    let first = FIRST_SESSION_TOKEN + 2 * id;
    (first, first + 1)
}

/// Record `id` with `message` in the system log, best-effort.
pub fn record(id: tairix_log::EventId, message: &str) {
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

/// What woke the loop.
#[derive(Default)]
struct Woke {
    /// The tokens that fired, each once.
    tokens: Vec<u64>,
    desktop: bool,
    pressure: bool,
}

struct RtEventSource<'a> {
    mailbox: EventMailbox,
    set: u64,
    files: &'a FileWorker,
    fonts: &'a FontWorker,
    writer: &'a PublishWorker<Settings>,
    due: &'a core::cell::Cell<Option<u64>>,
    woke: &'a RefCell<Woke>,
}

impl EventDrain for RtEventSource<'_> {
    fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
        self.mailbox.try_next(event)
    }
}

impl EventSource for RtEventSource<'_> {
    fn park(&mut self) -> Result<Parked, Errno> {
        let mut woke = self.woke.borrow_mut();
        Ok(match app::park_until_due(self.set, self.due.get())? {
            None => Parked::Interrupted,
            Some(Wake::App(token)) => {
                // A worker's readiness is a level, taken here or the park
                // would spin on it.
                match token {
                    FILES_TOKEN => self.files.wake().drain(),
                    FONTS_TOKEN => self.fonts.wake().drain(),
                    SETTINGS_TOKEN => self.writer.wake().drain(),
                    _ => {}
                }
                // A token lost to a full list fires again: the wait-set is
                // level-triggered.
                if !woke.tokens.contains(&token) && woke.tokens.try_reserve(1).is_ok() {
                    woke.tokens.push(token);
                }
                Parked::Interrupted
            }
            Some(Wake::DesktopChanged) => {
                woke.desktop = true;
                Parked::Interrupted
            }
            Some(Wake::PressureChanged) => {
                woke.pressure = true;
                tairix_font::trim_glyph_cache();
                Parked::Interrupted
            }
            Some(Wake::Event | Wake::PressureUnchanged) => Parked::Served,
        })
    }
}

/// The settings form as a sheet.
struct SettingsSheet(Form);

impl Sheet for SettingsSheet {
    type Answer = Edited;

    fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        self.0.extent(scale, theme)
    }

    fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        self.0.render(surface, scale, theme);
    }

    fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Edited> {
        self.0.on_input(event, at_ns, looks, damage)
    }
}

/// Where the documents folder's grant stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Documents {
    /// It is asked for.
    Asking,
    /// It was granted: its volume.
    Granted(Volume),
    /// It was refused.
    Refused,
}

impl Documents {
    const fn volume(self) -> Option<Volume> {
        match self {
            Self::Granted(volume) => Some(volume),
            Self::Asking | Self::Refused => None,
        }
    }
}

/// A pick under way in a session's window.
#[derive(Clone, Copy)]
struct Picking {
    /// The file runs in the shared session.
    shared: bool,
    /// The file picked, while its name is asked.
    picked: Option<(u64, bool)>,
}

/// The front end.
struct App {
    client: Client,
    desktop: Desktop,
    themes: ThemeRegistry,
    endpoint: u64,
    set: u64,
    files: Arc<FileWorker>,
    fonts: Arc<FontWorker>,
    writer: Arc<PublishWorker<Settings>>,
    settings: Publication<Settings>,
    refusals: Vec<StoreRefusal>,
    sessions: Vec<Session>,
    next_id: SessionId,
    /// The shared session, where it runs.
    shared: Option<SessionId>,
    documents: Documents,
    /// Files to run that came before the documents folder's grant settled,
    /// which their placement depends on.
    unplaced: Vec<(Handed, Volume, bool)>,
    /// The zone store, once read.
    zone: Option<Vec<u8>>,
    font: DesktopFont,
    /// The settings, and the window they are owned by.
    sheet: Option<(u64, SheetWindow<SettingsSheet>)>,
    picking: OrderedMap<u64, Picking>,
    quit: bool,
}

/// Hand `job` to `worker`, its queue grown where full: false where there
/// was no room even so.
fn submit<S, J, D>(
    worker: &Worker<S, J, D, tairix_util::defer::JobQueue<J, D>>,
    job: J,
    grow: usize,
) -> bool
where
    S: Send + 'static,
    J: Send + 'static,
    D: Send + 'static,
{
    match worker.submit(job) {
        Ok(_) => true,
        Err(job) => worker.grow(grow).is_ok() && worker.submit(job).is_ok(),
    }
}

impl App {
    fn live(&self) -> Settings {
        *self.settings.live()
    }

    /// The loop's own borrow of what a session needs.
    fn around(&mut self, now: u64) -> (Around<'_>, &mut Vec<Session>) {
        let live = *self.settings.live();
        (
            Around {
                client: &mut self.client,
                endpoint: self.endpoint,
                desktop: &self.desktop,
                theme: self.themes.active(),
                now,
                scaling: live.scaling,
                order: live.buttons,
            },
            &mut self.sessions,
        )
    }

    fn session_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|session| session.id() == id)
    }

    /// A new session's window, of `kind`: its id.
    fn open_session(&mut self, kind: Kind) -> Option<SessionId> {
        let id = self.next_id;
        self.next_id += 1;
        let (read, write) = session_tokens(id);
        let members = SessionMembers::new(self.set, read, write);
        let offers = !self.live().isolate_all && kind == Kind::Shared;
        let (mut around, sessions) = self.around(tairix_rt::clock_get());
        match Session::open(&mut around, (id, kind), offers, members) {
            Ok(session) => {
                if sessions.try_reserve(1).is_err() {
                    session.close(around.client);
                    return None;
                }
                sessions.push(session);
                Some(id)
            }
            Err(why) => {
                app::refuse(
                    &mut self.client,
                    (APP_NAME, self.endpoint),
                    None,
                    "A RISC OS session's window could not be opened",
                    &why,
                );
                None
            }
        }
    }

    /// Session `id`'s worker asked to start, `own` holding the documents
    /// folder, `handed` its first launch.
    fn start(&mut self, id: SessionId, own: bool, handed: Option<Handed>) {
        let job = files::Job::Start(Start {
            session: id,
            own,
            handed,
            setting: self.live().ceiling_bytes(),
        });
        if !submit(&self.files, job, files::JOBS) {
            if let Some(session) = self.session_mut(id) {
                session.refused("there was no room to start it");
            }
        }
    }

    /// The launch documents this process was started with.
    fn launched_with(&mut self) {
        let writable = match tairix_rt::arg(1) {
            Some(arg) if arg == DOCUMENT_WRITABLE_ROLE_ARG => true,
            Some(arg) if arg == DOCUMENT_ROLE_ARG => false,
            _ => return,
        };
        let name = tairix_rt::arg(2)
            .and_then(|raw| core::str::from_utf8(raw).ok())
            .map(|path| String::from(path.rsplit('/').next().unwrap_or(path)))
            .unwrap_or_default();
        let handed = Handed {
            file: File::adopt(STDIN),
            name,
            writable,
        };
        self.look(handed, false);
    }

    /// `handed` placed by its provenance once its volume is known.
    fn look(&mut self, handed: Handed, shared: bool) {
        let job = files::Job::Look {
            handed: Some(handed),
            shared,
        };
        if !submit(&self.files, job, files::JOBS) {
            app::report(
                APP_NAME,
                "a file to run was let go: there was no room to look at it",
            );
        }
    }

    /// A file granted as `handle`, named `name`, taken to run.
    fn take(&mut self, handle: u64, name: String, writable: bool, shared: bool) {
        let job = files::Job::Take {
            handle,
            name,
            writable,
            shared,
        };
        if !submit(&self.files, job, files::JOBS) {
            drop(File::from_delegation(handle));
            app::report(
                APP_NAME,
                "a file to run was let go: there was no room to take it",
            );
        }
    }

    /// `handed`, on `volume`, run where its provenance places it, once the
    /// documents folder's grant has settled.
    fn place(&mut self, handed: Handed, volume: Volume, shared: bool) {
        if self.documents == Documents::Asking {
            if self.unplaced.try_reserve(1).is_ok() {
                self.unplaced.push((handed, volume, shared));
            } else {
                app::report(
                    APP_NAME,
                    "a file to run was let go: there was no room to keep it",
                );
            }
            return;
        }
        let live = self.live();
        let asked = if shared {
            Placed::Shared
        } else {
            Placed::ByProvenance
        };
        let placed = placement(volume, self.documents.volume(), asked, live.isolate_all);
        record(
            LAUNCH_PLACED,
            match placed {
                Placement::Shared => "TAIRiX Bridge: a program runs in the shared session",
                Placement::Isolated => "TAIRiX Bridge: a program runs in an isolated session",
            },
        );
        match (placed, self.shared) {
            (Placement::Shared, Some(session)) => {
                let job = files::Job::Deliver {
                    session,
                    handed: Some(handed),
                };
                if !submit(&self.files, job, files::JOBS) {
                    app::report(
                        APP_NAME,
                        "a file to run was let go: there was no room to hand it over",
                    );
                }
            }
            _ => {
                if let Some(id) = self.open_session(Kind::Isolated) {
                    self.start(id, false, Some(handed));
                }
            }
        }
    }

    /// The shared session's window opened, its documents folder asked for.
    fn open_shared(&mut self) {
        let Some(id) = self.open_session(Kind::Shared) else {
            return;
        };
        self.shared = Some(id);
        let window = self.session_mut(id).map(|session| session.window_id());
        let asked = window.map(|window| self.client.request_documents_folder(window));
        if !matches!(asked, Some(Ok(()))) {
            self.documents_refused(id, "it could not be asked for");
        }
    }

    fn documents_refused(&mut self, id: SessionId, why: &str) {
        if let Some(session) = self.session_mut(id) {
            session.note(alloc::format!(
                "The RISC OS folder in Documents was not granted: {why}. The session runs without its own disc."
            ));
        }
        // Started first, so a file waiting for the shared session is handed
        // to a session that has its files.
        self.start(id, false, None);
        self.documents_settled(Documents::Refused);
    }

    /// The documents folder's grant settled as `documents`: the files that
    /// waited for it placed.
    fn documents_settled(&mut self, documents: Documents) {
        self.documents = documents;
        for (handed, volume, shared) in core::mem::take(&mut self.unplaced) {
            self.place(handed, volume, shared);
        }
    }

    /// The zone store asked for: a blocking request, made at bring-up while
    /// no frame is owed.
    fn ask_zone(&mut self) {
        let (Ok(handle), Some(grantor)) = (self.client.zone_store(), self.client.session()) else {
            return;
        };
        if !submit(
            &self.files,
            files::Job::Zone { handle, grantor },
            files::JOBS,
        ) {
            app::report(
                APP_NAME,
                "the time zones could not be read: there was no room",
            );
        }
    }

    /// The desktop font worked out from the desktop's text.
    fn ask_font(&mut self) {
        let text = self.desktop.info().text();
        if !submit(&self.fonts, fonts::Job::Desktop(text), fonts::JOBS) {
            app::report(
                APP_NAME,
                "the desktop font could not be worked out: there was no room",
            );
        }
    }

    /// What the workers finished.
    fn collect(&mut self) {
        let mut landed = Vec::new();
        self.files.collect_landed(|done| landed.push(done));
        for done in landed {
            self.landed_file(done);
        }
        let mut landed = Vec::new();
        self.fonts.collect_landed(|done| landed.push(done));
        for done in landed {
            match done {
                fonts::Done::Relayed {
                    session,
                    serial,
                    reply,
                } => {
                    if let Some(session) = self.session_mut(session) {
                        session.answer(serial, reply.as_deref());
                    }
                }
                fonts::Done::Desktop(font) => {
                    self.font = font;
                    for session in &mut self.sessions {
                        session.font(font);
                    }
                }
            }
        }
        if collect_published(&*self.writer, &mut self.settings, &mut self.refusals) {
            self.settings_landed();
        }
    }

    fn landed_file(&mut self, done: files::Done) {
        match done {
            files::Done::Documents(Ok(volume)) => {
                if let Some(id) = self.shared {
                    self.start(id, true, None);
                }
                self.documents_settled(Documents::Granted(volume));
            }
            files::Done::Documents(Err(why)) => {
                if let Some(id) = self.shared {
                    self.documents_refused(id, &why);
                }
            }
            files::Done::Zone(store) => self.zone = store,
            files::Done::Started { session, started } => self.started(session, started),
            files::Done::Relayed {
                session,
                serial,
                reply,
            } => {
                if let Some(session) = self.session_mut(session) {
                    session.answer(serial, reply.as_deref());
                }
            }
            files::Done::Taken { taken, shared } => match taken {
                Ok((handed, volume)) => self.place(handed, volume, shared),
                Err(why) => app::report(
                    APP_NAME,
                    alloc::format!("a file to run could not be taken: {why}"),
                ),
            },
            files::Done::Delivered { session, line } => match (self.session_mut(session), line) {
                (Some(session), Ok(line)) => session.launch(line),
                (Some(session), Err(why)) => {
                    session.note(alloc::format!("A file could not be run: {why}."));
                }
                (None, _) => {}
            },
            files::Done::LetGo => {}
        }
    }

    fn started(&mut self, id: SessionId, started: Result<files::Started, String>) {
        let started = match started {
            Ok(started) => started,
            Err(why) => {
                if let Some(session) = self.session_mut(id) {
                    session.refused(&why);
                }
                return;
            }
        };
        let name = self.desktop.info().zone();
        let zoned = zone(self.zone.as_deref(), name.as_ref().map(ZoneName::as_str));
        let wall = tairix_rt::wall_time().map(|reading| reading.time());
        let begin = Begin {
            ram_pages: started.plan.guest.pages(),
            memo_pages: started.plan.memo_pages,
            slot_bytes: started.plan.slot_bytes,
            real_time: wall.map_or(0, real_time),
            zone: zoned,
            keyboard: USA_COUNTRY,
            font: self.font,
            pressure: tairix_rt::pressure::gauge().band(),
        };
        let host = Host::start(started.channel, LogSink, &begin);
        let plan = started.plan;
        let Some(session) = self.session_mut(id) else {
            return;
        };
        match host {
            Ok(host) => {
                session.started(alloc::boxed::Box::new(host), &plan, started.line);
                let kind = match session.kind() {
                    Kind::Shared => "the shared",
                    Kind::Isolated => "an isolated",
                };
                record(
                    SESSION_STARTED,
                    &alloc::format!(
                        "TAIRiX Bridge: {kind} session started, its guest {} MiB",
                        plan.guest.bytes() >> 20
                    ),
                );
            }
            Err(unstarted) => {
                let why = match unstarted {
                    Unstarted::Session(_) => "the sandbox would not admit its worker",
                    Unstarted::Share => "its worker was handed no screen memory",
                    Unstarted::NoMemory => "there was no memory for it",
                };
                session.refused(why);
            }
        }
    }

    /// What the settings writer answered.
    fn settings_landed(&mut self) {
        for refusal in self.refusals.drain(..) {
            app::report(APP_NAME, refusal);
        }
    }

    /// The settings changed as the form says: what each change touches
    /// applied, the store committed where the control settled.
    fn edited(&mut self, now: Settings, settled: bool) {
        let was = self.live();
        self.settings.edit(&was, &now);
        if was.isolate_all != now.isolate_all {
            for session in &mut self.sessions {
                let offers = !now.isolate_all && session.kind() == Kind::Shared;
                session.offer_shared(offers);
            }
        }
        if was.scaling != now.scaling {
            for session in &mut self.sessions {
                session.scale(now.scaling);
            }
        }
        if settled {
            let job = self.settings.settle();
            if submit_published(&*self.writer, &mut self.settings, job, &mut self.refusals) {
                self.settings_landed();
            }
        }
    }

    fn open_settings(&mut self, owner: u64) {
        if self.sheet.is_some() {
            return;
        }
        let sheet = SettingsSheet(Form::new(self.live()));
        let looks = (self.desktop.scale(), self.themes.active());
        let title = tairix_riscos_app::settings::TITLE;
        match SheetWindow::open(
            &mut self.client,
            (owner, self.endpoint),
            looks,
            sheet,
            title,
        ) {
            Ok(window) => self.sheet = Some((owner, window)),
            Err(why) => {
                app::refuse(
                    &mut self.client,
                    (APP_NAME, self.endpoint),
                    Some(owner),
                    "The settings could not be opened",
                    &why,
                );
            }
        }
    }

    fn sheet_event(&mut self, event: &WindowEvent) {
        let Some((_, sheet)) = self.sheet.as_mut() else {
            return;
        };
        let looks = (self.desktop.scale(), self.themes.active());
        match sheet.event(&mut self.client, event, looks) {
            SheetEvent::Nothing => {}
            SheetEvent::Closed | SheetEvent::Answered(Edited::Closed) => self.close_sheet(),
            SheetEvent::Answered(Edited::Changed { settled }) => {
                let now = *sheet.sheet().0.settings();
                self.edited(now, settled);
            }
        }
    }

    fn close_sheet(&mut self) {
        if let Some((_, sheet)) = self.sheet.take() {
            sheet.close(&mut self.client);
        }
    }

    /// A command from session `id`'s window.
    fn command(&mut self, id: SessionId, command: Command) {
        let Some(window) = self.session_mut(id).map(|session| session.window_id()) else {
            return;
        };
        match command {
            // A window's open pick concludes first: the desktop refuses a
            // second, and its refusal must not take the first's place.
            Command::Run | Command::RunShared if self.picking.contains_key(&window) => {}
            Command::Run | Command::RunShared => {
                let shared = command == Command::RunShared;
                let picking = Picking {
                    shared,
                    picked: None,
                };
                if self.picking.try_insert(window, picking).is_err() {
                    app::report(
                        APP_NAME,
                        "no file could be asked for: there was no memory to follow it",
                    );
                } else if let Err(err) = self.client.pick_file(window, PickPurpose::Open) {
                    self.picking.remove(&window);
                    app::report(
                        APP_NAME,
                        alloc::format!("no file could be asked for ({err})"),
                    );
                }
            }
            Command::Settings => self.open_settings(window),
            Command::Pause(..) | Command::Quit(_) => {}
        }
    }

    /// One relay a session asked for, carried by the worker it is for.
    fn relay(&mut self, id: SessionId, side: Side, serial: u32, request: Vec<u8>) {
        let taken = match side {
            Side::Files => submit(
                &self.files,
                files::Job::Relay {
                    session: id,
                    serial,
                    request,
                },
                files::JOBS,
            ),
            Side::Fonts => submit(
                &self.fonts,
                fonts::Job::Relay {
                    session: id,
                    serial,
                    request,
                },
                fonts::JOBS,
            ),
        };
        if !taken {
            if let Some(session) = self.session_mut(id) {
                session.answer(serial, None);
            }
        }
    }

    /// What each session's worker sent taken, and what that needs done.
    fn pump(&mut self, now: u64) {
        let mut needs: Vec<(SessionId, Need)> = Vec::new();
        let mut ended = Vec::new();
        {
            let (mut around, sessions) = self.around(now);
            for session in sessions.iter_mut() {
                let mut asked = Vec::new();
                session.pump(&mut around, &mut asked);
                if session.take_ended() {
                    ended.push(session.id());
                }
                needs.extend(asked.into_iter().map(|need| (session.id(), need)));
            }
        }
        for (id, need) in needs {
            match need {
                Need::Relay {
                    side,
                    serial,
                    request,
                } => self.relay(id, side, serial, request),
                Need::Command(command) => self.command(id, command),
            }
        }
        for id in ended {
            self.files_let_go(id);
        }
        let mut finished = Vec::new();
        self.sessions.retain(|session| {
            let keep = !session.finished() || session.kind() == Kind::Shared;
            if !keep {
                finished.push(session.window_id());
            }
            keep
        });
        for window in finished {
            let _ = self.client.close(window);
        }
    }

    /// Session `at` let go: its worker ended, its windows and settings
    /// closed, its files let go; the application ends with its last session.
    fn let_go(&mut self, at: usize) {
        let session = self.sessions.remove(at);
        if self
            .sheet
            .as_ref()
            .is_some_and(|(owner, _)| *owner == session.window_id())
        {
            self.close_sheet();
        }
        let id = session.id();
        session.close(&mut self.client);
        self.files_let_go(id);
        self.quit = self.sessions.is_empty();
    }

    /// Session `id`'s files let go, and it the shared session no longer.
    fn files_let_go(&mut self, id: SessionId) {
        if !submit(&self.files, files::Job::Ended { session: id }, files::JOBS) {
            app::report(
                APP_NAME,
                "a session's files stay open: there was no room to let them go",
            );
        }
        if self.shared == Some(id) {
            self.shared = None;
        }
    }

    /// An event the application answers itself rather than a session's
    /// window: whether `event` was one.
    fn app_event(&mut self, event: &WindowEvent) -> bool {
        match *event {
            WindowEvent::OpenRequested => {
                if let Err(err) = self.client.ask_open_targets() {
                    app::report(
                        APP_NAME,
                        alloc::format!("what was handed over could not be asked for ({err})"),
                    );
                }
            }
            WindowEvent::DocumentsFolder { handle, .. } => {
                if !submit(&self.files, files::Job::Documents { handle }, files::JOBS) {
                    drop(tairix_rt::Dir::from_delegation(handle));
                    if let Some(id) = self.shared {
                        self.documents_refused(id, "there was no room to take it");
                    }
                }
            }
            WindowEvent::DocumentsFolderRefused { refusal, .. } => {
                let why = match refusal {
                    DocumentsRefusal::NotTheirs => "it is not this application's",
                    DocumentsRefusal::NotAFolder => "it is not a folder",
                    DocumentsRefusal::Unavailable => "it is unavailable",
                };
                if let Some(id) = self.shared {
                    self.documents_refused(id, why);
                }
            }
            WindowEvent::FilePicked {
                window_id,
                handle,
                writable,
            } => match self.picking.get_mut(&window_id) {
                Some(picking) => {
                    picking.picked = Some((handle, writable));
                    if self.client.ask_picked_name(window_id).is_err() {
                        self.picking.remove(&window_id);
                        drop(File::from_delegation(handle));
                    }
                }
                None => drop(File::from_delegation(handle)),
            },
            WindowEvent::PickCancelled { window_id } => {
                self.picking.remove(&window_id);
            }
            WindowEvent::FileOffered {
                window_id, serial, ..
            } => {
                if let Err(err) = self.client.ask_offered_file(window_id, serial) {
                    app::report(
                        APP_NAME,
                        alloc::format!("a dropped file could not be judged ({err})"),
                    );
                }
            }
            WindowEvent::CloseRequested { window_id } => {
                let Some(at) = self
                    .sessions
                    .iter()
                    .position(|session| session.window_id() == window_id)
                else {
                    return false;
                };
                self.let_go(at);
            }
            _ => return false,
        }
        true
    }

    fn handle(&mut self, event: &WindowEvent, now: u64) {
        let window = event.window_id();
        if let (Some(window), Some((_, sheet))) = (window, self.sheet.as_ref()) {
            if sheet.id() == window {
                self.sheet_event(event);
                return;
            }
        }
        if self.app_event(event) {
            return;
        }
        let Some(window) = window else {
            return;
        };
        let mut needs = Vec::new();
        let mut owner = None;
        {
            let (mut around, sessions) = self.around(now);
            if let Some(session) = sessions.iter_mut().find(|session| session.owns(window)) {
                owner = Some(session.id());
                session.event(&mut around, event, &mut needs);
            }
        }
        if let Some(id) = owner {
            for need in needs {
                match need {
                    Need::Relay {
                        side,
                        serial,
                        request,
                    } => self.relay(id, side, serial, request),
                    Need::Command(command) => self.command(id, command),
                }
            }
        }
    }

    fn answer(&mut self, answer: Answer) {
        match answer {
            Answer::FrameReturned { window_id } => {
                if let Some((_, sheet)) = self
                    .sheet
                    .as_mut()
                    .filter(|(_, sheet)| sheet.id() == window_id)
                {
                    if let Err(err) = sheet.present_deferred(&mut self.client) {
                        app::report(
                            APP_NAME,
                            alloc::format!("the settings could not be presented ({err})"),
                        );
                    }
                    return;
                }
                let client = &mut self.client;
                if let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| session.owns(window_id))
                {
                    session.frame_returned(client, window_id);
                }
            }
            Answer::PickedName { window_id, name } => {
                let Some(picking) = self.picking.remove(&window_id) else {
                    return;
                };
                let Some((handle, writable)) = picking.picked else {
                    return;
                };
                match name {
                    Ok(name) => self.take(handle, name, writable, picking.shared),
                    Err(_) => self.take(handle, String::new(), writable, picking.shared),
                }
            }
            Answer::OfferedFile(offer) => {
                let places = offer.media.is_ok()
                    && self
                        .sessions
                        .iter()
                        .any(|session| session.window_id() == offer.window_id);
                if let Err(err) =
                    self.client
                        .answer_file_offer(offer.window_id, offer.serial, places)
                {
                    app::report(
                        APP_NAME,
                        alloc::format!("a dropped file could not be answered ({err})"),
                    );
                }
            }
            Answer::OpenTarget(Ok(Some(target))) => match target {
                Target::Document {
                    name,
                    grant,
                    writable,
                } => self.take(grant, name, writable, false),
                Target::Placed { name, grant, .. } => self.take(grant, name, false, false),
                Target::Path(_) | Target::Image(_) | Target::Pane(_) => {}
            },
            Answer::OpenTarget(Err(err)) => {
                app::report(APP_NAME, alloc::format!("a file to run was lost ({err})"));
            }
            _ => {}
        }
    }

    fn refused(&mut self, refused: &tairix_window::Refused) {
        let window = refused.window_id;
        match refused.request {
            PostedRequest::Resize => {
                for session in &mut self.sessions {
                    session.resize_refused(window);
                }
            }
            PostedRequest::PickFile => {
                self.picking.remove(&window);
                app::report(
                    APP_NAME,
                    alloc::format!("no file could be picked ({})", refused.errno),
                );
            }
            PostedRequest::DocumentsFolder => {
                if let Some(id) = self.shared {
                    let why = alloc::format!("the desktop refused it ({})", refused.errno);
                    self.documents_refused(id, &why);
                }
            }
            PostedRequest::Create => {
                if let Some(at) = self
                    .sessions
                    .iter()
                    .position(|session| session.window_id() == window)
                {
                    app::report(
                        APP_NAME,
                        "the desktop refused a session's window: the session ends",
                    );
                    self.let_go(at);
                } else {
                    for session in &mut self.sessions {
                        session.screen_refused(window);
                    }
                }
            }
            request => app::report(
                APP_NAME,
                alloc::format!("the desktop refused {request:?} ({})", refused.errno),
            ),
        }
    }

    /// The desktop's look or text changed.
    fn desktop_moved(&mut self) {
        match app::adopt_desktop(&mut self.desktop, &mut self.themes) {
            Ok(true) => {
                for session in &mut self.sessions {
                    session.relook();
                }
                self.ask_font();
            }
            Ok(false) => {}
            Err(err) => app::report(
                APP_NAME,
                alloc::format!("the desktop could not be followed ({err})"),
            ),
        }
    }

    /// The machine's memory pressure moved.
    fn pressure_moved(&mut self) {
        let band = tairix_rt::pressure::gauge().band();
        for session in &mut self.sessions {
            session.pressure(band);
        }
    }

    /// The pipes that fired served.
    fn serve_tokens(&mut self, tokens: &[u64]) {
        for session in &mut self.sessions {
            let (read, write) = session_tokens(session.id());
            let fired_read = tokens.contains(&read);
            let fired_write = tokens.contains(&write);
            if fired_read || fired_write {
                session.fired(fired_read, fired_write);
            }
        }
    }

    /// Everything owed painted, and the wait-set kept in step.
    fn settle(&mut self, now: u64) {
        {
            let (mut around, sessions) = self.around(now);
            for session in sessions.iter_mut() {
                session.tick(now);
                session.paint(&mut around);
                session.sync();
            }
        }
        if let Some((_, sheet)) = self.sheet.as_mut() {
            if let Err(err) = sheet.present(&mut self.client) {
                app::report(
                    APP_NAME,
                    alloc::format!("the settings could not be presented ({err})"),
                );
            }
        }
    }

    fn due(&self) -> Option<u64> {
        self.sessions.iter().filter_map(Session::due).min()
    }
}

/// The desktop, the workers and the shared session's window brought up, and
/// the loop run until the last session's window closes.
pub fn run() -> i32 {
    let mut client = WindowClient::new(app::RtWindowTransport);
    let (desktop, themes) = match app::bring_up_desktop(&mut client) {
        Ok(brought) => brought,
        Err(err) => return app::fail(APP_NAME, err.code(), err),
    };
    let binding = match app::bind_event_mailbox() {
        Ok(binding) => binding,
        Err(err) => return app::fail(APP_NAME, err.code(), err),
    };
    let (endpoint, set) = (binding.endpoint(), binding.set());
    let no_worker = || app::fail(APP_NAME, app::EXIT_NO_WINDOW, "no memory for the workers");
    let Ok(files) = FileWorker::queued(
        files::serve,
        files::Held::new(),
        WorkerWake::create(),
        files::JOBS,
    ) else {
        return no_worker();
    };
    let files = Arc::new(files);
    app::start_worker(APP_NAME, &files, set, FILES_TOKEN, "files");
    let Ok(fonts) = FontWorker::queued(fonts::serve, (), WorkerWake::create(), fonts::JOBS) else {
        return no_worker();
    };
    let fonts = Arc::new(fonts);
    app::start_worker(APP_NAME, &fonts, set, FONTS_TOKEN, "fonts");
    // Read here, before any window: no frame is owed anyone yet.
    let (settings, refusals) =
        tairix_appdata::loaded::<Settings>(&Store::open_without_defaults(&mut RtHost));
    let writer = Arc::new(tairix_appdata::publish_worker::<Settings>(Opening::Bare));
    app::start_worker(APP_NAME, &writer, set, SETTINGS_TOKEN, "settings");
    let _guards = (
        WorkerGuard::new(&files),
        WorkerGuard::new(&fonts),
        WorkerGuard::new(&writer),
    );
    let Some(server) = client.session() else {
        return app::fail(
            APP_NAME,
            app::EXIT_CHANNEL_LOST,
            "the desktop named no session",
        );
    };
    let mut app = App {
        client,
        desktop,
        themes,
        endpoint,
        set,
        files,
        fonts,
        writer,
        settings: Publication::new(settings),
        refusals,
        sessions: Vec::new(),
        next_id: 0,
        shared: None,
        documents: Documents::Asking,
        unplaced: Vec::new(),
        zone: None,
        font: DesktopFont::SHIPPED,
        sheet: None,
        picking: OrderedMap::new(),
        quit: false,
    };
    app.settings_landed();
    app.ask_zone();
    app.ask_font();
    app.open_shared();
    app.launched_with();
    let due = core::cell::Cell::new(None);
    let woke = RefCell::new(Woke::default());
    let (files, fonts, writer) = (
        Arc::clone(&app.files),
        Arc::clone(&app.fonts),
        Arc::clone(&app.writer),
    );
    let source = RtEventSource {
        mailbox: EventMailbox::new(endpoint, server),
        set,
        files: &files,
        fonts: &fonts,
        writer: &writer,
        due: &due,
        woke: &woke,
    };
    let code = serve(&mut app, WindowEvents::new(source), (&due, &woke));
    see_out_published(&*app.writer, &mut app.settings, &mut app.refusals);
    code
}

fn serve(
    app: &mut App,
    mut events: WindowEvents<RtEventSource<'_>>,
    (due, woke): (&core::cell::Cell<Option<u64>>, &RefCell<Woke>),
) -> i32 {
    let mut waited = Ok(None);
    loop {
        let now = tairix_rt::clock_get();
        let fired = core::mem::take(&mut *woke.borrow_mut());
        app.collect();
        app.serve_tokens(&fired.tokens);
        if fired.desktop {
            app.desktop_moved();
        }
        if fired.pressure {
            app.pressure_moved();
        }
        loop {
            match waited {
                Ok(Some(event)) => app.handle(&event, now),
                Ok(None) => break,
                Err(EventError::Undecodable(_)) => {}
                Err(EventError::Refused(refused)) => app.refused(&refused),
                Err(EventError::Mailbox(err)) => {
                    return app::fail(
                        APP_NAME,
                        app::EXIT_CHANNEL_LOST,
                        alloc::format!("the desktop went away ({err})"),
                    );
                }
            }
            waited = events.try_wait(&mut app.client);
        }
        while let Some(answer) = app.client.take_answer() {
            app.answer(answer);
        }
        app.pump(now);
        if app.quit {
            return 0;
        }
        app.settle(now);
        due.set(app.due());
        waited = events.wait(&mut app.client);
    }
}
