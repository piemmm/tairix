//! One session in the front end: its worker driven through the session's
//! host, its window — the Task Manager's view of it — each context's screen
//! window, and the launches waiting for its boot to end.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::DamageRect;
use tairix_abi::window_ipc::{CursorShape, WindowEvent, WindowSizing};
use tairix_collections::OrderedMap;
use tairix_geometry::{Region, Scale};
use tairix_raster::Surface;
use tairix_reclaim::PressureBand;
use tairix_riscos_app::bar::Command as ScreenCommand;
use tairix_riscos_app::input::ButtonOrder;
use tairix_riscos_app::memory::Plan;
use tairix_riscos_app::settings::Scaling;
use tairix_riscos_app::tasks::{shown, Command, Kind, Task, Tasks};
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::memory::Shared;
use tairix_riscos_kernel::relay::Side;
use tairix_riscos_session::front::Why;
use tairix_riscos_session::host::Host;
use tairix_riscos_session::protocol::{Context, Cursor, Ending, Event, Screen};
use tairix_rt::LogSink;
use tairix_sandbox::rt::{RtSessionChannel, SessionMembers};
use tairix_theme::Theme;
use tairix_window::app::{self, Fitted, WindowPane};
use tairix_window::{pointer_input_events, pointer_point, Desktop};

use crate::front::{record, Client, SessionId, APP_NAME};
use crate::screen::{Asked, ScreenWindow};

/// A session's host.
pub type SessionHost = Host<RtSessionChannel, LogSink>;

/// What the boot's task is called in the session's window.
const BOOT_LINE: &str = "Boot sequence";

/// Where a session's worker stands.
enum Stage {
    /// Its worker is being started.
    Starting,
    /// It runs.
    Running(Box<SessionHost>),
    /// It ended: why, where it did not end of itself.
    Ended(Option<String>),
}

/// A launch asked and not yet answered: its command line.
struct Asking {
    line: Vec<u8>,
}

/// What a session needs of the front end as it is driven.
pub struct Around<'a> {
    /// The window client.
    pub client: &'a mut Client,
    /// The windows' event endpoint.
    pub endpoint: u64,
    /// The desktop.
    pub desktop: &'a Desktop,
    /// The theme windows are drawn in.
    pub theme: &'a Theme,
    /// The clock.
    pub now: u64,
    /// How new screens are scaled.
    pub scaling: Scaling,
    /// Which button is which.
    pub order: ButtonOrder,
}

impl<'a> Around<'a> {
    fn looks(&self) -> (Scale, &'a Theme) {
        (self.desktop.scale(), self.theme)
    }
}

/// What a session asks of the front end.
pub enum Need {
    /// A relay carried, its reply to come back.
    Relay {
        /// To whom.
        side: Side,
        /// Its serial.
        serial: u32,
        /// The request.
        request: Vec<u8>,
    },
    /// A command from its window.
    Command(Command),
}

/// What an event the worker sent says, held past the host's lending of it.
enum Telling {
    Launched {
        serial: u32,
        context: Context,
    },
    Refused {
        serial: u32,
        error: Option<String>,
    },
    Screen {
        context: Context,
        screen: Screen,
    },
    Cursor {
        context: Context,
        cursor: Option<Cursor>,
    },
    PointerAt {
        context: Context,
        at: (i32, i32),
    },
    PointerSet {
        context: Context,
    },
    Hourglass {
        context: Context,
        shown: bool,
    },
    Own {
        context: Context,
        slot: u32,
        screen: u32,
    },
    Shared(Shared),
    Relay {
        side: Side,
        serial: u32,
        request: Vec<u8>,
    },
    BootFailed {
        line: String,
        error: String,
    },
    BootOmitted {
        count: u32,
    },
    Ended {
        context: Context,
        ending: Option<String>,
    },
    Nothing,
}

/// What an ending says, where it is worth saying: none for a program that
/// exited cleanly or was quit.
fn ending_said(ending: &Ending<'_>) -> Option<String> {
    match ending {
        Ending::Exit { code: 0 } | Ending::Quit => None,
        Ending::Exit { code } => Some(format!("it exited with {code}")),
        Ending::Error(error) => Some(format!("{} (&{:X})", shown(error.text), error.number)),
        Ending::Fault => Some(String::from("it faulted")),
        Ending::Reset { .. } => Some(String::from("it reset the machine")),
        Ending::Unmodelled(_) => Some(String::from(
            "it drove hardware TAIRiX Bridge does not model",
        )),
    }
}

/// `bytes` copied, none where there is no memory for them.
fn copied(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut owned = Vec::new();
    owned.try_reserve_exact(bytes.len()).ok()?;
    owned.extend_from_slice(bytes);
    Some(owned)
}

impl Telling {
    /// What `event` says; none where there was no memory to keep it.
    fn of(event: &Event<'_>) -> Option<Self> {
        Some(match *event {
            Event::Launched { serial, context } => Self::Launched { serial, context },
            Event::Refused { serial, error } => Self::Refused {
                serial,
                error: error.map(|error| shown(error.text)),
            },
            Event::Screen { context, screen } => Self::Screen { context, screen },
            Event::Cursor { context, cursor } => Self::Cursor { context, cursor },
            Event::PointerAt { context, x, y } => Self::PointerAt {
                context,
                at: (x, y),
            },
            Event::PointerSet { context } => Self::PointerSet { context },
            Event::Hourglass { context, shown } => Self::Hourglass { context, shown },
            Event::Own {
                context,
                slot,
                screen,
            } => Self::Own {
                context,
                slot,
                screen,
            },
            Event::Shared(shared) => Self::Shared(shared),
            Event::Relay {
                serial,
                side,
                request,
                ..
            } => Self::Relay {
                side,
                serial,
                request: copied(request)?,
            },
            Event::BootFailed { line, error, .. } => Self::BootFailed {
                line: shown(line),
                error: shown(error.text),
            },
            Event::BootOmitted { count, .. } => Self::BootOmitted { count },
            Event::Ended { context, ending } => Self::Ended {
                context,
                ending: ending_said(&ending),
            },
            Event::Credit { .. } | Event::Damage { .. } | Event::Pointer { .. } => Self::Nothing,
        })
    }
}

/// One session.
pub struct Session {
    id: SessionId,
    kind: Kind,
    pane: WindowPane,
    surface: Surface,
    tasks: Tasks,
    /// The window is to be painted whole.
    owed: bool,
    stage: Stage,
    members: SessionMembers,
    screens: OrderedMap<Context, ScreenWindow>,
    /// Launches made before the boot ended, in order.
    waiting: VecDeque<Vec<u8>>,
    /// The boot's serial, then its context, until it ends.
    boot: Option<(u32, Option<Context>)>,
    asking: OrderedMap<u32, Asking>,
    /// The screen windows of contexts reset, by the serial of the launch
    /// whose context takes each over.
    parked: OrderedMap<u32, ScreenWindow>,
    /// Each running context's command line.
    lines: OrderedMap<Context, Vec<u8>>,
    /// Contexts quit to be launched again in their windows.
    resetting: OrderedMap<Context, ()>,
    /// It ended since the front end last asked.
    just_ended: bool,
    /// Its window's pointer is the desktop's busy one.
    busy: bool,
}

impl Session {
    /// The window of session `id` of `kind`, its worker yet to start,
    /// offering a run in the shared session where `offers_shared`.
    ///
    /// # Errors
    ///
    /// Why its window could not be opened.
    pub fn open(
        around: &mut Around<'_>,
        (id, kind): (SessionId, Kind),
        offers_shared: bool,
        members: SessionMembers,
    ) -> Result<Self, String> {
        let tasks = Tasks::new(kind, 0, offers_shared);
        let (width, height) = tasks.extent(around.desktop.scale(), around.theme);
        let surface = Surface::new(width, height).ok_or("no memory for its window")?;
        let pane = WindowPane::open(
            around.client,
            around.endpoint,
            &app::mode_for(width, height),
            &tasks.title(),
            WindowSizing::Fixed,
        )
        .map_err(|err| format!("its window could not be opened ({err})"))?;
        Ok(Self {
            id,
            kind,
            pane,
            surface,
            tasks,
            owed: true,
            stage: Stage::Starting,
            members,
            screens: OrderedMap::new(),
            waiting: VecDeque::new(),
            boot: None,
            asking: OrderedMap::new(),
            parked: OrderedMap::new(),
            lines: OrderedMap::new(),
            resetting: OrderedMap::new(),
            just_ended: false,
            busy: false,
        })
    }

    /// Whether it ended since last asked: once, as its worker was let go.
    pub const fn take_ended(&mut self) -> bool {
        core::mem::replace(&mut self.just_ended, false)
    }

    /// Which session it is.
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Its kind.
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// Its window's id.
    pub const fn window_id(&self) -> u64 {
        self.pane.id()
    }

    /// Whether `window` is one of its windows.
    pub fn owns(&self, window: u64) -> bool {
        self.pane.id() == window || self.screens.values().any(|screen| screen.id() == window)
    }

    /// Whether it ended of itself, nothing left to say in its window.
    pub const fn finished(&self) -> bool {
        matches!(self.stage, Stage::Ended(None))
    }

    /// Whether its worker runs.
    pub const fn running(&self) -> bool {
        matches!(self.stage, Stage::Running(_))
    }

    /// `note` said in its window.
    pub fn note(&mut self, note: String) {
        self.tasks.note(note);
        self.owed = true;
    }

    /// Whether a file may be run in the shared session from its window.
    pub fn offer_shared(&mut self, offers: bool) {
        self.tasks.offer_shared(offers);
        self.owed = true;
    }

    /// Its worker started as `host` with `plan`'s memory; `line` launched
    /// once its boot has ended.
    pub fn started(&mut self, mut host: Box<SessionHost>, plan: &Plan, line: Option<Vec<u8>>) {
        self.tasks.ram(plan.guest.bytes());
        host.run(true);
        let Some(serial) = host.boot() else {
            self.stage = Stage::Running(host);
            self.end(Some(String::from("its boot could not be asked for")));
            return;
        };
        self.boot = Some((serial, None));
        self.waiting.extend(line);
        self.stage = Stage::Running(host);
        self.owed = true;
    }

    /// Its worker could not be started, for `why`.
    pub fn refused(&mut self, why: &str) {
        self.note(format!("The session could not start: {why}."));
        self.stage = Stage::Ended(Some(String::from(why)));
    }

    /// `line` launched, once its boot has ended.
    pub fn launch(&mut self, line: Vec<u8>) {
        if self.boot.is_some() || !self.running() {
            self.waiting.push_back(line);
        } else {
            self.ask_launch(line);
        }
    }

    /// `line` launched now: the serial its answer names.
    fn ask_launch(&mut self, line: Vec<u8>) -> Option<u32> {
        let Stage::Running(host) = &mut self.stage else {
            return None;
        };
        // Room to follow the launch is held first: once asked, it runs.
        if self.asking.try_reserve(1).is_err() {
            self.note(format!(
                "{} could not be launched: there was no memory to follow it.",
                shown(&line)
            ));
            return None;
        }
        let Some(serial) = host.launch(&line) else {
            self.note(format!("{} could not be launched now.", shown(&line)));
            return None;
        };
        if self.asking.try_insert(serial, Asking { line }).is_err() {
            self.note(String::from("A launch's command line was lost."));
        }
        Some(serial)
    }

    /// The machine's memory pressure is now `band`.
    pub fn pressure(&mut self, band: PressureBand) {
        if let Stage::Running(host) = &mut self.stage {
            host.pressure(band);
        }
    }

    /// The desktop's text is now `font`.
    pub fn font(&mut self, font: DesktopFont) {
        if let Stage::Running(host) = &mut self.stage {
            host.font(font);
        }
    }

    /// Relay `serial`'s reply, or none where it could not be carried.
    pub fn answer(&mut self, serial: u32, reply: Option<&[u8]>) {
        if let Stage::Running(host) = &mut self.stage {
            match reply {
                Some(reply) => host.answer(serial, reply),
                None => host.unanswered(serial),
            };
        }
    }

    /// Every screen scaled `scaling`.
    pub fn scale(&mut self, scaling: Scaling) {
        for screen in self.screens.values_mut() {
            screen.scale(scaling);
        }
    }

    /// The wait-set kept in step with what the session wants.
    pub fn sync(&mut self) {
        let (descriptors, read, write) = match &self.stage {
            Stage::Running(host) => (host.descriptors(), host.wants_read(), host.wants_write()),
            Stage::Starting | Stage::Ended(_) => (None, false, false),
        };
        if self.members.sync(descriptors, read, write).is_err() {
            app::report(APP_NAME, "a session's pipes could not be watched");
        }
    }

    /// The session's pipes fired: `read` and `write` readiness.
    pub fn fired(&mut self, read: bool, write: bool) {
        if let Stage::Running(host) = &mut self.stage {
            if read {
                host.on_readable();
            }
            if write {
                host.on_writable();
            }
        }
    }

    /// Everything the worker sent taken; what the front end must do for it
    /// pushed onto `needs`.
    pub fn pump(&mut self, around: &mut Around<'_>, needs: &mut Vec<Need>) {
        loop {
            let Stage::Running(host) = &mut self.stage else {
                return;
            };
            let telling = match host.told() {
                Ok(None) => return,
                Ok(Some(told)) => match told.event {
                    Event::Damage { context, first, .. } => {
                        if let Some(screen) = self.screens.get_mut(&context) {
                            screen.damage(first, told.pixels);
                        }
                        continue;
                    }
                    Event::Pointer { context, shape } => {
                        if let Some(screen) = self.screens.get_mut(&context) {
                            screen.pointer(around.client, shape.as_ref());
                        }
                        continue;
                    }
                    ref event => Telling::of(event),
                },
                Err(why) => {
                    let running: Vec<String> =
                        host.running().map(|(_, line)| task_line(line)).collect();
                    self.ended_for(why, &running);
                    return;
                }
            };
            match telling {
                Some(telling) => self.tell(telling, around, needs),
                None => self.note(String::from(
                    "An event was lost: there was no memory to keep it.",
                )),
            }
        }
    }

    fn tell(&mut self, telling: Telling, around: &mut Around<'_>, needs: &mut Vec<Need>) {
        match telling {
            Telling::Launched { serial, context } => self.launched(around.client, serial, context),
            Telling::Refused { serial, error } => self.refused_launch(around, serial, error),
            Telling::Screen { context, screen } => self.shown(around, context, screen),
            Telling::Cursor { context, cursor } => {
                if let Some(window) = self.screens.get_mut(&context) {
                    window.cursor(cursor, around.now);
                }
            }
            Telling::PointerAt { context, at } => {
                if let Some(window) = self.screens.get_mut(&context) {
                    window.pointer_at(at.0, at.1);
                }
            }
            Telling::PointerSet { context } => {
                if let Some(window) = self.screens.get_mut(&context) {
                    window.pointer_set();
                }
            }
            Telling::Hourglass { context, shown } => {
                self.tasks.busy(context, shown);
                self.owed = true;
            }
            Telling::Own {
                context,
                slot,
                screen,
            } => {
                self.tasks.own(context, slot, screen);
                self.owed = true;
            }
            Telling::Shared(shared) => {
                self.tasks.shared(shared);
                self.owed = true;
            }
            Telling::Relay {
                side,
                serial,
                request,
            } => needs.push(Need::Relay {
                side,
                serial,
                request,
            }),
            Telling::BootFailed { line, error } => {
                self.tasks.boot_failed(&line, &error);
                self.owed = true;
            }
            Telling::BootOmitted { count } => self.tasks.boot_omitted(count),
            Telling::Ended { context, ending } => self.context_ended(around, context, ending),
            Telling::Nothing => {}
        }
    }

    /// Launch `serial` was refused with `error`, none where the session had
    /// no room.
    fn refused_launch(&mut self, around: &mut Around<'_>, serial: u32, error: Option<String>) {
        if let Some(window) = self.parked.remove(&serial) {
            window.close(around.client);
        }
        let why = error.unwrap_or_else(|| String::from("the session had no room"));
        let line = if self.boot.is_some_and(|(boot, _)| boot == serial) {
            self.boot_ended();
            String::from(BOOT_LINE)
        } else {
            self.asking
                .remove(&serial)
                .map_or_else(String::new, |asking| task_line(&asking.line))
        };
        let said = format!("{line} could not run: {why}.");
        if self.kind == Kind::Isolated && self.idle() {
            self.end(Some(said));
        } else {
            self.note(said);
        }
    }

    /// The boot is over: the launches waiting for it made.
    fn boot_ended(&mut self) {
        self.boot = None;
        self.tasks.boot_ended();
        self.owed = true;
        while let Some(line) = self.waiting.pop_front() {
            self.ask_launch(line);
        }
    }

    /// Whether nothing runs, is asked or waits.
    fn idle(&self) -> bool {
        self.boot.is_none()
            && self.lines.is_empty()
            && self.asking.is_empty()
            && self.waiting.is_empty()
    }

    fn launched(&mut self, client: &mut Client, serial: u32, context: Context) {
        let line = match self.boot {
            Some((boot, None)) if boot == serial => {
                self.boot = Some((boot, Some(context)));
                Vec::new()
            }
            _ => {
                if let Some(window) = self.parked.remove(&serial) {
                    self.keep_screen(client, context, window);
                }
                self.asking
                    .remove(&serial)
                    .map(|asking| asking.line)
                    .unwrap_or_default()
            }
        };
        let task = Task {
            context,
            line: task_line(&line),
            slot: 0,
            screen: 0,
            paused: false,
            busy: false,
        };
        if self.lines.try_insert(context, line).is_err() {
            self.note(String::from(
                "A task's command line was lost: no memory to keep it.",
            ));
        }
        self.tasks.launched(task);
        self.owed = true;
    }

    fn shown(&mut self, around: &mut Around<'_>, context: Context, screen: Screen) {
        if let Some(window) = self.screens.get_mut(&context) {
            window.screen(screen);
            return;
        }
        let title = self
            .lines
            .get(&context)
            .map_or_else(|| String::from(BOOT_LINE), |line| task_line(line));
        let looks = (around.endpoint, around.desktop, around.theme);
        match ScreenWindow::open(around.client, looks, screen, &title, around.scaling) {
            Ok(window) => self.keep_screen(around.client, context, window),
            Err(why) => self.note(format!("{title}'s screen could not be shown: {why}.")),
        }
    }

    /// `window` kept as `context`'s screen, or closed where there is no
    /// room to keep it.
    fn keep_screen(&mut self, client: &mut Client, context: Context, window: ScreenWindow) {
        if self.screens.try_reserve(1).is_err() {
            window.close(client);
            self.note(String::from(
                "A screen was closed: there was no memory to keep it.",
            ));
            return;
        }
        if let Ok(Some(old)) = self.screens.try_insert(context, window) {
            old.close(client);
        }
    }

    fn context_ended(&mut self, around: &mut Around<'_>, context: Context, ending: Option<String>) {
        let line = self.lines.remove(&context).unwrap_or_default();
        self.tasks.ended(context);
        self.owed = true;
        let window = self.screens.remove(&context);
        let reset = self.resetting.remove(&context).is_some() && !line.is_empty();
        let relaunched = if reset {
            self.ask_launch(line.clone())
        } else {
            None
        };
        match (window, relaunched) {
            (Some(window), Some(serial)) => {
                if self.parked.try_reserve(1).is_ok() {
                    if let Ok(Some(old)) = self.parked.try_insert(serial, window) {
                        old.close(around.client);
                    }
                } else {
                    window.close(around.client);
                }
            }
            (Some(window), None) => window.close(around.client),
            (None, _) => {}
        }
        if let (Some(ending), false) = (ending, reset) {
            self.note(format!("{} ended: {ending}.", task_line(&line)));
        }
        if self.boot.is_some_and(|(_, booted)| booted == Some(context)) {
            self.boot_ended();
        }
        if self.kind == Kind::Isolated && self.idle() {
            record(
                tairix_riscos_app::events::SESSION_ENDED,
                "TAIRiX Bridge: an isolated session ended with its last task",
            );
            self.end(None);
            self.close_screens(around.client);
        }
    }

    /// The session ended for `why` with `running` still running.
    fn ended_for(&mut self, why: Why, running: &[String]) {
        let mut said = format!("The session ended: {}.", why.reason());
        if !running.is_empty() {
            said.push_str(" It was running ");
            said.push_str(&running.join(", "));
            said.push('.');
        }
        app::report(APP_NAME, format_args!("{said}"));
        if matches!(why, Why::Unbelievable(_)) {
            record(tairix_riscos_app::events::WORKER_CONTAINED, &said);
        }
        record(tairix_riscos_app::events::SESSION_ENDED, &said);
        self.end(Some(said));
    }

    /// The session's worker let go, `why` said where it did not end of
    /// itself.
    fn end(&mut self, why: Option<String>) {
        // The wait-set keys its members by descriptor, so they go before the
        // host's pipes close.
        if self.members.sync(None, false, false).is_err() {
            app::report(APP_NAME, "a session's pipes could not be let go");
        }
        self.tasks.boot_ended();
        self.tasks.clear_busy();
        if let Some(why) = &why {
            self.tasks.note(why.clone());
        }
        let was_running = self.running();
        self.stage = Stage::Ended(why);
        self.boot = None;
        self.lines.clear();
        self.asking.clear();
        self.waiting.clear();
        self.owed = true;
        self.just_ended |= was_running;
    }

    /// The desktop refused to make `window`: where it was one of its screens',
    /// that screen let go, and said.
    pub fn screen_refused(&mut self, window: u64) {
        let refused = self
            .screens
            .iter()
            .find(|(_, screen)| screen.id() == window)
            .map(|(&context, _)| context);
        if let Some(context) = refused {
            self.screens.remove(&context);
            self.note(String::from(
                "A screen's window could not be opened: the desktop refused it.",
            ));
        }
    }

    /// Every screen window closed, those waiting for a reset's launch too.
    pub fn close_screens(&mut self, client: &mut Client) {
        while let Some((_, window)) = self.screens.pop_first() {
            window.close(client);
        }
        while let Some((_, window)) = self.parked.pop_first() {
            window.close(client);
        }
    }

    /// The session ended by the user: its worker let go, its windows closed.
    pub fn close(mut self, client: &mut Client) {
        self.end(None);
        self.close_screens(client);
        let _ = self.pane.close(client);
    }

    /// When a screen next has timed work.
    pub fn due(&self) -> Option<u64> {
        self.screens.values().filter_map(ScreenWindow::due).min()
    }

    /// Timed work done at `now`.
    pub fn tick(&mut self, now: u64) {
        for window in self.screens.values_mut() {
            window.tick(now);
        }
    }

    /// One event for one of its windows.
    pub fn event(&mut self, around: &mut Around<'_>, event: &WindowEvent, needs: &mut Vec<Need>) {
        let Some(window_id) = event.window_id() else {
            return;
        };
        if window_id == self.pane.id() {
            self.own_event(around, event, needs);
            return;
        }
        let Some(context) = self
            .screens
            .iter()
            .find(|(_, window)| window.id() == window_id)
            .map(|(&context, _)| context)
        else {
            return;
        };
        let mut sent = Vec::new();
        let asked = self.screens.get_mut(&context).and_then(|window| {
            window.event(
                around.client,
                event,
                (around.looks(), around.order),
                &mut |input| {
                    sent.push(input);
                },
            )
        });
        if let Stage::Running(host) = &mut self.stage {
            for input in sent {
                host.input(context, input);
            }
        }
        match asked {
            Some(Asked::Close) => self.quit(context),
            Some(Asked::Command(command)) => self.screen_command(around, context, command),
            None => {}
        }
    }

    fn screen_command(
        &mut self,
        around: &mut Around<'_>,
        context: Context,
        command: ScreenCommand,
    ) {
        match command {
            ScreenCommand::FullScreen => {
                if let Some(window) = self.screens.get_mut(&context) {
                    window.toggle_fullscreen(around.client);
                }
            }
            ScreenCommand::Scaling(scaling) => {
                if let Some(window) = self.screens.get_mut(&context) {
                    window.scale(scaling);
                }
            }
            ScreenCommand::Pause => {
                let paused = !self.tasks.is_paused(context);
                self.pause(context, paused);
            }
            ScreenCommand::Reset => {
                if self.resetting.try_insert(context, ()).is_ok() {
                    self.quit(context);
                } else {
                    self.note(String::from(
                        "A program could not be reset: there was no memory to follow it.",
                    ));
                }
            }
        }
    }

    fn pause(&mut self, context: Context, paused: bool) {
        let Stage::Running(host) = &mut self.stage else {
            return;
        };
        if host.pause(context, paused) {
            self.tasks.paused(context, paused);
            if let Some(window) = self.screens.get_mut(&context) {
                window.paused(paused);
            }
            self.owed = true;
        }
    }

    fn quit(&mut self, context: Context) {
        if let Stage::Running(host) = &mut self.stage {
            host.quit(context);
        }
    }

    fn own_event(&mut self, around: &mut Around<'_>, event: &WindowEvent, needs: &mut Vec<Need>) {
        match *event {
            WindowEvent::Pointer { x, y, action, .. } => {
                let mut damage = Region::new();
                for input in pointer_input_events(action, pointer_point(x, y)) {
                    if let Some(command) = self.tasks.on_input(&input, around.looks(), &mut damage)
                    {
                        match command {
                            Command::Pause(context, paused) => self.pause(context, paused),
                            Command::Quit(context) => self.quit(context),
                            command => needs.push(Need::Command(command)),
                        }
                    }
                }
                if !damage.is_empty() {
                    self.owed = true;
                }
            }
            WindowEvent::Scrolled { dx, dy, .. } => {
                let mut damage = Region::new();
                let input = tairix_input::InputEvent::PointerScrolled { dx, dy };
                self.tasks.on_input(&input, around.looks(), &mut damage);
                if !damage.is_empty() {
                    self.owed = true;
                }
            }
            WindowEvent::ContentReleased { .. } => self.pane.release_frames(),
            WindowEvent::RedrawRequested { .. } => self.owed = true,
            _ => {}
        }
    }

    /// Its window's pointer the desktop's busy one while a task's program
    /// shows its hourglass. A refusal is said once, not asked again.
    fn settle_pointer(&mut self, client: &mut Client) {
        let busy = self.tasks.any_busy();
        if busy == self.busy {
            return;
        }
        self.busy = busy;
        let shape = if busy {
            CursorShape::Busy
        } else {
            CursorShape::Arrow
        };
        if let Err(err) = client.set_cursor(self.pane.id(), shape) {
            app::report(
                APP_NAME,
                format_args!("a session's pointer could not be set ({err})"),
            );
        }
    }

    /// The window `window` held back a present for a frame that came back.
    pub fn frame_returned(&mut self, client: &mut Client, window: u64) {
        if self.pane.id() == window {
            if let Err(err) = self.pane.present_deferred(client, &self.surface) {
                app::report(
                    APP_NAME,
                    format_args!("a session's window could not be presented ({err})"),
                );
            }
            return;
        }
        if let Some(screen) = self
            .screens
            .values_mut()
            .find(|screen| screen.id() == window)
        {
            screen.present_deferred(client);
        }
    }

    /// The desktop refused a resize of `window`.
    pub fn resize_refused(&mut self, window: u64) {
        if let Some(screen) = self
            .screens
            .values_mut()
            .find(|screen| screen.id() == window)
        {
            screen.view_refused();
        }
    }

    /// Everything owed painted and presented; an ended session's screens
    /// closed.
    pub fn paint(&mut self, around: &mut Around<'_>) {
        let (scale, theme) = around.looks();
        if matches!(self.stage, Stage::Ended(_)) {
            self.close_screens(around.client);
        }
        self.settle_pointer(around.client);
        for screen in self.screens.values_mut() {
            screen.paint(around.client, (scale, theme));
        }
        if !self.owed {
            return;
        }
        let (width, height) = self.tasks.extent(scale, theme);
        if (width, height) != (self.surface.width(), self.surface.height()) {
            let Some(fresh) = Surface::new(width, height) else {
                app::report(APP_NAME, "no memory to resize a session's window");
                return;
            };
            match self
                .pane
                .resize(around.client, &app::mode_for(width, height))
            {
                Fitted::Adopted => self.surface = fresh,
                Fitted::Deferred => return,
                Fitted::Kept => app::report(APP_NAME, app::SIZE_KEPT),
            }
        }
        self.owed = false;
        self.tasks.render(&mut self.surface, scale, theme);
        let whole = DamageRect {
            x: 0,
            y: 0,
            width_px: self.surface.width(),
            height_px: self.surface.height(),
        };
        if let Err(err) = self.pane.present(around.client, &self.surface, whole) {
            app::report(
                APP_NAME,
                format_args!("a session's window could not be presented ({err})"),
            );
        }
    }

    /// The desktop's look changed: everything painted again.
    pub fn relook(&mut self) {
        self.owed = true;
    }
}

/// A task's command line as its window shows it.
fn task_line(line: &[u8]) -> String {
    if line.is_empty() {
        String::from(BOOT_LINE)
    } else {
        shown(line)
    }
}
