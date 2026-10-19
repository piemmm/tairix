//! The `datetime.app` bundle's `Run` entry point: the Date & Time dialog the
//! desktop clock's *Set Date & Time…* row starts through the system
//! authorisation prompt.
//!
//! # What this binary is, and what stays in the library
//!
//! The readout, the zone's changes, the composition of a hand-set instant,
//! the controls and the window's layout and paint are the host-tested
//! `tairix_datetime` engine. This binary composes them over the live
//! syscalls: a window and its owned zone chooser, one event mailbox parked on
//! through a wait-set, a tick on the wall clock's own second, and one worker
//! that carries out every read and write, so the window never waits on a
//! store, a service or a device.
//!
//! # It is *given* the authority, and never assumes it
//!
//! Stepping the clock and setting the clock chip need `CAP_TIME_SET`, and
//! changing network time or the machine's zone `CAP_SETTINGS_WRITE`; the
//! signed manifest requests both, and the kernel grants them only as
//! `manifest ∩ the launching account's ceiling`. A refusal is an ordinary
//! outcome: it is stated in an alert held by the window and on `stderr`, the
//! window shows what is really in force, and the dialog keeps running.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;

    use tairix_abi::driver::display::DisplayMode;
    use tairix_abi::driver_store::SystemConfigFile;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::origin::ProcId;
    use tairix_abi::rtc_ipc::{self, RtcOp, RTC_ENDPOINT};
    use tairix_abi::settings_ipc::{
        decode_reply, encode_set, set_request_len, SETTINGS_ENDPOINT, SETTINGS_REPLY_MAX,
    };
    use tairix_abi::time::{Time64, WallTimeState, NANOS_PER_SEC};
    use tairix_abi::window_ipc::{
        AlertButton, AlertButtonRole, AlertOutcome, AlertSeverity, AlertSpec, AppBarClick,
        DialogModality, WindowEvent, WindowSizing,
    };
    use tairix_abi::zone::ZoneName;
    use tairix_abi::Errno;
    use tairix_controls::zone::{self, Minute};
    use tairix_controls::{PlaceDialog, PlaceOutcome, PressTiming};
    use tairix_datetime::calendar::Date;
    use tairix_datetime::time_entry::TimeOfDay;
    use tairix_datetime::view::{Request, Window, TITLE};
    use tairix_datetime::{
        compose, implausible_question, readout, repeated_question, skipped_reason, Clock, ClockSet,
        Composed, MachineTime, SettingChange,
    };
    use tairix_geometry::{Rect, Region, Scale};
    use tairix_input::InputEvent;
    use tairix_raster::Surface;
    use tairix_rt::work::Worker;
    use tairix_sysconfig::SystemConfig;
    use tairix_theme::{Theme, ThemeRegistry};
    use tairix_timesync::{SyncRecord, RECORD_LEN, RECORD_PATH};
    use tairix_tz::listing::{label, Listing};
    use tairix_tz::{resolve_zone, CivilClock, Disambiguation, ZoneInfo};
    use tairix_util::defer::JobQueue;
    use tairix_window::app::{self, Fitted, Wake, WindowPane, EXIT_CHANNEL_LOST};
    use tairix_window::{
        key_input_event, pointer_input_events, pointer_point, scroll_input_events, Desktop,
        EventDrain, EventError, EventMailbox, EventSource, Owed, Parked, Repaint, WindowClient,
        WindowEvents,
    };

    /// The name this program states its refusals under.
    const APP_NAME: &str = "datetime";

    /// The wait-set token the worker's answers wake the loop on.
    const WORKER_TOKEN: u64 = app::FIRST_APP_TOKEN;

    /// How many requests may wait for the worker at once: far more than a
    /// person makes before the first is answered.
    const QUEUE: usize = 8;

    /// How far past the wall clock's next whole second the tick is due, so
    /// it lands on the new second rather than a hair before it.
    const TICK_SLACK_NS: u64 = 2_000_000;

    type Client = WindowClient<app::RtWindowTransport>;

    /// What the worker is asked.
    enum Job {
        /// Read the machine's time settings, what the time service last did,
        /// and, on the first read, the zone store.
        Read {
            /// Whether the zone store is wanted.
            zones: bool,
        },
        /// Change one of the machine's time settings.
        Change(SettingChange),
        /// Step the clock to this instant, then set the clock chip.
        SetClock(Time64),
    }

    /// What the worker answers.
    enum Answer {
        Read(Readings),
        Changed(SettingChange, Result<SystemConfig, Errno>),
        ClockSet(ClockSet),
    }

    /// What one read found.
    struct Readings {
        config: Result<SystemConfig, Errno>,
        record: SyncRecord,
        zones: Option<Result<&'static ZoneInfo<'static>, String>>,
    }

    type Desk = Worker<(), Job, Answer, JobQueue<Job, Answer>>;

    /// The worker's one body: every wait the dialog makes is made here.
    fn work(_: &mut (), job: &mut Job) -> Answer {
        match *job {
            Job::Read { zones } => Answer::Read(Readings {
                config: read_config(),
                record: read_record(),
                zones: zones.then(read_zones),
            }),
            Job::Change(change) => Answer::Changed(change, ask_settings(change)),
            Job::SetClock(at) => Answer::ClockSet(set_clock(at)),
        }
    }

    /// The machine configuration as every surface reads it, or the store's
    /// defaults where it has none.
    fn read_config() -> Result<SystemConfig, Errno> {
        match tairix_procinfo::system_config(&tairix_procinfo::IpcTransport) {
            Ok(config) => Ok(config.unwrap_or_default()),
            Err(tairix_procinfo::CallError::Service(err)) => Err(err),
            Err(_) => Err(Errno::NotSupported),
        }
    }

    /// What the time service last recorded; empty where it never has.
    fn read_record() -> SyncRecord {
        // A byte past the record is read, so a longer document is refused by
        // the record's own length check rather than read as its first bytes.
        tairix_rt::read_path_to_end(RECORD_PATH.as_bytes(), RECORD_LEN + 1)
            .map_or(SyncRecord::EMPTY, |bytes| SyncRecord::from_bytes(&bytes))
    }

    /// The zone store, read and validated once for the life of the process.
    fn read_zones() -> Result<&'static ZoneInfo<'static>, String> {
        let file = tairix_rt::File::open(
            tairix_tz::STORE_PATH.as_bytes(),
            tairix_abi::fs::OpenFlags::READ,
        )
        .map_err(|raw| alloc::format!("{}", Errno::from_syscall(raw)))?;
        tairix_tz::load(&file).map_err(|err| alloc::format!("{err}"))
    }

    /// Ask the settings service for `change`, answering the document it
    /// wrote.
    fn ask_settings(change: SettingChange) -> Result<SystemConfig, Errno> {
        let setting = [change.setting()];
        let mut request = buffer(set_request_len(&setting))?;
        let len = encode_set(SystemConfigFile::System, &setting, &mut request)?;
        let mut reply = buffer(SETTINGS_REPLY_MAX)?;
        let got = tairix_rt::ipc_call(SETTINGS_ENDPOINT, &request[..len], &mut reply)
            .map_err(Errno::from_syscall)?;
        let written = decode_reply(&reply[..got])?;
        SystemConfig::parse(written).map_err(|_| Errno::BadMagic)
    }

    /// A zeroed buffer of `len` bytes, or the refusal of its memory.
    fn buffer(len: usize) -> Result<Vec<u8>, Errno> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| Errno::OutOfMemory)?;
        bytes.resize(len, 0);
        Ok(bytes)
    }

    /// Step the clock to `at` as a person at the keyboard does — a
    /// correction, not a synchronised source — then give the clock chip the
    /// clock's own reading, so the time it keeps across a restart is the one
    /// just set.
    fn set_clock(at: Time64) -> ClockSet {
        let ret = tairix_rt::wall_time_set(at, WallTimeState::Adjusted);
        if ret != 0 {
            return ClockSet {
                clock: Err(Errno::from_syscall(ret)),
                chip: None,
            };
        }
        let now = tairix_rt::wall_time().map_or(at, |reading| reading.time());
        ClockSet {
            clock: Ok(()),
            chip: Some(set_chip(now)),
        }
    }

    /// Write `at` to the board's clock chip.
    fn set_chip(at: Time64) -> Result<(), Errno> {
        let mut request = [0u8; rtc_ipc::REQUEST_LEN];
        let len = rtc_ipc::encode_request(&mut request, RtcOp::Set, at)?;
        let mut reply = [0u8; rtc_ipc::REPLY_LEN];
        let got = tairix_rt::ipc_call(RTC_ENDPOINT, &request[..len], &mut reply)
            .map_err(Errno::from_syscall)?;
        rtc_ipc::decode_ack(&reply[..got])
    }

    /// The production [`EventSource`]: the app's own mailbox, parked on
    /// through the wait-set until an event, a worker's answer, a desktop
    /// change or the next tick, accepting only the session the create reply
    /// named.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        /// The monotonic instant the next tick is due.
        deadline: &'a Cell<u64>,
        /// Set when a park woke for something the loop serves itself.
        woke: &'a Cell<Woke>,
    }

    /// What a park woke for besides an event, gathered until the loop serves
    /// it.
    #[derive(Copy, Clone, Default)]
    struct Woke {
        tick: bool,
        worker: bool,
        desktop: bool,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let mut woke = self.woke.get();
            let parked = match app::park_until(self.set, self.deadline.get())? {
                None => {
                    woke.tick = true;
                    Parked::Interrupted
                }
                Some(Wake::PressureChanged) => {
                    tairix_font::trim_glyph_cache();
                    Parked::Served
                }
                Some(Wake::DesktopChanged) => {
                    woke.desktop = true;
                    Parked::Interrupted
                }
                Some(Wake::App(WORKER_TOKEN)) => {
                    woke.worker = true;
                    Parked::Interrupted
                }
                Some(Wake::Event | Wake::PressureUnchanged | Wake::App(_)) => Parked::Served,
            };
            self.woke.set(woke);
            Ok(parked)
        }
    }

    /// A window the loop paints: its pane, the picture it keeps, and what it
    /// owes the screen.
    struct Drawn {
        pane: WindowPane,
        surface: Surface,
        owed: Owed,
    }

    impl Drawn {
        fn id(&self) -> u64 {
            self.pane.id()
        }

        fn mode(&self) -> DisplayMode {
            *self.pane.mode()
        }

        fn owe(&mut self, damage: &Region) {
            let mode = self.mode();
            self.owed.owe(&mode, Repaint::Reported, damage);
        }

        /// Paint what is owed through `paint`, each rectangle under its own
        /// clip, and present them; nothing while the session holds none of
        /// the window's pixels.
        fn present(
            &mut self,
            client: &mut Client,
            paint: impl Fn(&mut Surface),
        ) -> Result<(), Errno> {
            if self.pane.content_released() {
                return Ok(());
            }
            let mode = self.mode();
            let Some(parts) = self.owed.take(&mode) else {
                return Ok(());
            };
            for rect in parts.rects() {
                self.surface
                    .with_clip(rect.x, rect.y, rect.width_px, rect.height_px, |surface| {
                        paint(surface);
                    });
            }
            self.pane.present_list(client, &self.surface, &parts)
        }

        /// Take the size `width_px` × `height_px`, owing the whole window when
        /// it is adopted.
        fn resize(&mut self, client: &mut Client, width_px: u32, height_px: u32) {
            let mode = app::mode_for(width_px, height_px);
            match self.pane.resize_with(client, &mode, &mut self.surface) {
                Fitted::Adopted => self.owed.owe_whole(),
                Fitted::Deferred => {}
                Fitted::Kept => app::report(APP_NAME, app::SIZE_KEPT),
            }
        }
    }

    /// The zone chooser: an owned dialog window over the store's places.
    struct Chooser {
        pane: Drawn,
        dialog: PlaceDialog,
        written: Minute,
    }

    /// A question put to the user in an alert, awaiting its answer, and the
    /// monotonic instant *Set* was pressed at: the time typed is what the
    /// clock should read then, so the seconds the question was up are added
    /// to it.
    enum Question {
        /// Which of the two times a twice-passed local time names.
        Repeated {
            alert: u64,
            date: Date,
            time: TimeOfDay,
            set_at_ns: u64,
        },
        /// Whether a time outside the plausible window is meant.
        Implausible {
            alert: u64,
            at: Time64,
            set_at_ns: u64,
        },
    }

    /// Everything the dialog knows and shows.
    struct Dialog {
        window: Window,
        main: Drawn,
        server: ProcId,
        endpoint: u64,
        machine: MachineTime,
        zones: Option<&'static ZoneInfo<'static>>,
        places: Option<Listing<'static>>,
        civil: CivilClock<'static>,
        place: String,
        last_sync: Option<Time64>,
        clock: Clock,
        chooser: Option<Chooser>,
        question: Option<Question>,
        /// Whether a step of the clock is with the worker, so a second *Set*
        /// cannot queue behind it and step the clock back to what the first
        /// already set.
        stepping: bool,
        /// The extent last asked for, so a size the session imposed is not
        /// argued with until the content asks for another.
        asked: (u32, u32),
    }

    impl Dialog {
        fn theme_scale<'a>(themes: &'a ThemeRegistry, desktop: &Desktop) -> (&'a Theme, Scale) {
            (themes.active(), desktop.scale())
        }

        /// Adopt a read: the settings in force, when the network last set the
        /// clock, and the zone store the first time it lands.
        fn adopt_readings(&mut self, readings: Readings, (theme, scale): (&Theme, Scale)) {
            self.last_sync = readings.record.last_sync;
            if let Some(zones) = readings.zones {
                match zones {
                    Ok(zones) => self.zones = Some(zones),
                    Err(why) => app::report(
                        APP_NAME,
                        format_args!("the time zones could not be read ({why}); times read UTC"),
                    ),
                }
            }
            let mut damage = Region::new();
            let machine = match readings.config {
                Ok(config) => MachineTime::of(&config),
                Err(err) => {
                    let why = alloc::format!(
                        "The machine's settings could not be read ({err}); what is shown is \
                         the system's default."
                    );
                    app::report(APP_NAME, &why);
                    self.window.set_reason(Some(why), scale, theme, &mut damage);
                    MachineTime::of(&SystemConfig::default())
                }
            };
            self.main.owe(&damage);
            self.adopt_machine(machine, (theme, scale));
        }

        /// Adopt the machine's time settings as `machine` holds them.
        fn adopt_machine(&mut self, machine: MachineTime, (theme, scale): (&Theme, Scale)) {
            let zone_moved = machine.zone != self.machine.zone || self.places.is_none();
            self.machine = machine;
            let mut damage = Region::new();
            if zone_moved {
                self.civil = civil_clock(self.zones, machine.zone.as_ref());
                self.place = self.zones.map_or_else(
                    || String::from(self.civil.zone().name()),
                    |zones| label(zones, self.civil.zone().name()),
                );
                self.places = self
                    .zones
                    .map(|zones| Listing::of(zones, zones.default_region()));
                self.window.set_zone(&self.place, scale, theme, &mut damage);
            }
            self.window
                .set_network(machine.network, scale, theme, &mut damage);
            self.main.owe(&damage);
            let _ = self.tick((theme, scale));
        }

        /// Read the clock and show it: the readout, and the controls where
        /// they follow it. Answers whether the clock's provenance moved — another
        /// source set it — so what that source recorded is read again.
        fn tick(&mut self, (theme, scale): (&Theme, Scale)) -> bool {
            let previous = self.clock;
            self.clock = match tairix_rt::wall_time() {
                Ok(reading) => Clock::Read {
                    reading,
                    last_sync: self.last_sync,
                },
                Err(_) => Clock::Unreadable,
            };
            let mut damage = Region::new();
            let shown = readout(self.clock, self.machine.network, &self.place, &self.civil);
            self.window.adopt_readout(shown, scale, theme, &mut damage);
            let now = self.clock.now().map(|now| local_parts(&self.civil, now));
            self.window.follow(now, scale, theme, &mut damage);
            self.main.owe(&damage);
            self.refresh_chooser(theme, scale);
            state_of(previous) != state_of(self.clock)
        }

        /// Write the chooser's readings again when the minute has turned.
        fn refresh_chooser(&mut self, theme: &Theme, scale: Scale) {
            let (Some(chooser), Some(places), Some(at)) = (
                self.chooser.as_mut(),
                self.places.as_ref(),
                self.clock.now(),
            ) else {
                return;
            };
            let mode = chooser.pane.mode();
            let bounds = Rect::new(0, 0, mode.width_px, mode.height_px);
            let mut damage = Region::new();
            zone::refresh(
                &mut chooser.dialog,
                &mut chooser.written,
                (places, 0),
                at,
                (bounds, scale, theme),
                &mut damage,
            );
            chooser.pane.owe(&damage);
        }

        /// Resize the window to what it now holds, where that changed.
        fn refit(&mut self, client: &mut Client, (theme, scale): (&Theme, Scale)) {
            let extent = self.window.extent(scale, theme);
            if extent != self.asked {
                self.asked = extent;
                self.main.resize(client, extent.0, extent.1);
            }
        }

        /// Act on what the window asked; `true` ends the dialog.
        fn serve(
            &mut self,
            request: Request,
            client: &mut Client,
            desk: &Desk,
            (theme, scale): (&Theme, Scale),
        ) -> bool {
            let mut damage = Region::new();
            match request {
                Request::Close => return true,
                Request::Network(on) => {
                    self.window.set_network(on, scale, theme, &mut damage);
                    if !self.submit(Job::Change(SettingChange::Network(on)), client, desk) {
                        self.window
                            .set_network(self.machine.network, scale, theme, &mut damage);
                    }
                }
                Request::ChooseZone => self.open_chooser(client, (theme, scale)),
                Request::SetClock { date, time } => {
                    let set_at_ns = tairix_rt::clock_get();
                    let asked = (date, time, Disambiguation::Reject, set_at_ns);
                    self.set_by_hand(asked, client, desk, (theme, scale));
                }
            }
            self.main.owe(&damage);
            false
        }

        /// Set the clock to `date` at `time` in the machine's zone, `choice`
        /// deciding between two instants, as *Set* asked at `set_at_ns`;
        /// asking first where the local time names two instants or an
        /// implausible one.
        fn set_by_hand(
            &mut self,
            (date, time, choice, set_at_ns): (Date, TimeOfDay, Disambiguation, u64),
            client: &mut Client,
            desk: &Desk,
            (theme, scale): (&Theme, Scale),
        ) {
            if self.stepping {
                return;
            }
            let mut damage = Region::new();
            match compose(self.civil.zone(), date, time, choice) {
                Composed::At(at) => {
                    self.window.set_reason(None, scale, theme, &mut damage);
                    match implausible_question(at, &self.civil) {
                        Some((headline, detail)) => {
                            let buttons = [
                                (String::from("Cancel"), AlertButtonRole::Cancel),
                                (String::from("Set Anyway"), AlertButtonRole::Plain),
                            ];
                            if let Some(alert) = self.ask(
                                client,
                                AlertSeverity::Warning,
                                (&headline, &detail),
                                &buttons,
                            ) {
                                self.question = Some(Question::Implausible {
                                    alert,
                                    at,
                                    set_at_ns,
                                });
                            }
                        }
                        None => self.step(at, set_at_ns, client, desk),
                    }
                }
                Composed::Skipped { from, to } => {
                    let why = skipped_reason(date, time, &from, &to, &self.civil);
                    self.window.set_reason(Some(why), scale, theme, &mut damage);
                    self.window.focus_time(scale, theme, &mut damage);
                }
                Composed::Repeated { earlier, later, at } => {
                    let (headline, detail, [first, second]) =
                        repeated_question(date, time, (&earlier, &later, &at), &self.civil);
                    let buttons = [
                        (String::from("Cancel"), AlertButtonRole::Cancel),
                        (first, AlertButtonRole::Plain),
                        (second, AlertButtonRole::Plain),
                    ];
                    if let Some(alert) = self.ask(
                        client,
                        AlertSeverity::Question,
                        (&headline, &detail),
                        &buttons,
                    ) {
                        self.question = Some(Question::Repeated {
                            alert,
                            date,
                            time,
                            set_at_ns,
                        });
                    }
                }
            }
            self.main.owe(&damage);
        }

        /// Step the clock to `at`, as it should read at the monotonic
        /// `set_at_ns`: the seconds since are added, so a question that was
        /// up a while does not leave the clock behind by as long.
        fn step(&mut self, at: Time64, set_at_ns: u64, client: &mut Client, desk: &Desk) {
            let since = tairix_rt::clock_get().saturating_sub(set_at_ns);
            let at = at.saturating_add(tairix_abi::time::Duration64::from_nanos(since));
            self.stepping = true;
            if !self.submit(Job::SetClock(at), client, desk) {
                self.stepping = false;
            }
        }

        /// Put a question to the user in an alert held by the window,
        /// answering its id; a question the desktop will not put is stated
        /// and answered `None`, so nothing is done.
        fn ask(
            &self,
            client: &mut Client,
            severity: AlertSeverity,
            (headline, detail): (&str, &str),
            buttons: &[(String, AlertButtonRole)],
        ) -> Option<u64> {
            let buttons: Result<Vec<AlertButton>, Errno> = buttons
                .iter()
                .map(|(label, role)| AlertButton::new(label, *role))
                .collect();
            let asked = buttons
                .and_then(|buttons| AlertSpec::fitted(severity, headline, detail, &buttons))
                .and_then(|spec| client.open_alert(self.endpoint, Some(self.main.id()), &spec));
            match asked {
                Ok(alert) => Some(alert),
                Err(err) => {
                    app::report(
                        APP_NAME,
                        format_args!("{headline}: the question could not be asked ({err}); nothing was changed"),
                    );
                    None
                }
            }
        }

        /// An alert came down: carry out what its answer asks.
        fn answered(
            &mut self,
            alert_id: u64,
            outcome: AlertOutcome,
            client: &mut Client,
            desk: &Desk,
            (theme, scale): (&Theme, Scale),
        ) {
            let asked =
                match &self.question {
                    Some(
                        Question::Repeated { alert, .. } | Question::Implausible { alert, .. },
                    ) if *alert == alert_id => self.question.take(),
                    _ => return,
                };
            match (asked, outcome) {
                (
                    Some(Question::Repeated {
                        date,
                        time,
                        set_at_ns,
                        ..
                    }),
                    AlertOutcome::Chosen { index },
                ) => {
                    let choice = match index {
                        1 => Disambiguation::Earlier,
                        2 => Disambiguation::Later,
                        _ => return,
                    };
                    self.set_by_hand(
                        (date, time, choice, set_at_ns),
                        client,
                        desk,
                        (theme, scale),
                    );
                }
                (
                    Some(Question::Implausible { at, set_at_ns, .. }),
                    AlertOutcome::Chosen { index: 1 },
                ) => self.step(at, set_at_ns, client, desk),
                _ => {}
            }
        }

        /// Hand `job` to the worker, answering whether it was taken; a full
        /// queue is stated, and nothing done.
        fn submit(&mut self, job: Job, client: &mut Client, desk: &Desk) -> bool {
            if desk.submit(job).is_ok() {
                return true;
            }
            let _ = app::refuse(
                client,
                (APP_NAME, self.endpoint),
                Some(self.main.id()),
                "That was not done",
                "The dialog is still finishing what was asked before it. Try again in a moment.",
            );
            false
        }

        /// Take in what the worker answered.
        fn take_answer(
            &mut self,
            answer: Answer,
            client: &mut Client,
            (theme, scale): (&Theme, Scale),
        ) {
            match answer {
                Answer::Read(readings) => self.adopt_readings(readings, (theme, scale)),
                Answer::Changed(_, Ok(config)) => {
                    self.adopt_machine(MachineTime::of(&config), (theme, scale));
                }
                Answer::Changed(change, Err(err)) => {
                    let mut damage = Region::new();
                    self.window
                        .set_network(self.machine.network, scale, theme, &mut damage);
                    self.main.owe(&damage);
                    let (headline, detail) = change.refusal(err);
                    let _ = app::refuse(
                        client,
                        (APP_NAME, self.endpoint),
                        Some(self.main.id()),
                        headline,
                        &detail,
                    );
                }
                Answer::ClockSet(set) => {
                    self.stepping = false;
                    if let Some((headline, detail)) = set.refusal() {
                        let _ = app::refuse(
                            client,
                            (APP_NAME, self.endpoint),
                            Some(self.main.id()),
                            headline,
                            &detail,
                        );
                    } else {
                        let mut damage = Region::new();
                        self.window.release_edit(scale, theme, &mut damage);
                        self.window
                            .set_reason(set.note(), scale, theme, &mut damage);
                        self.main.owe(&damage);
                        let _ = self.tick((theme, scale));
                    }
                }
            }
        }

        /// Open the zone chooser over the store's places, the machine's zone
        /// highlighted; a chooser already open is left to the session to
        /// raise.
        fn open_chooser(&mut self, client: &mut Client, (theme, scale): (&Theme, Scale)) {
            if self.chooser.is_some() {
                return;
            }
            let Some(places) = self.places.as_ref() else {
                let _ = app::refuse(
                    client,
                    (APP_NAME, self.endpoint),
                    Some(self.main.id()),
                    "The time zone cannot be changed",
                    "The system's list of time zones could not be read.",
                );
                return;
            };
            let held =
                ZoneName::new(self.civil.zone().name()).and_then(|zone| places.index_of(&zone));
            let dialog = zone::dialog(zone::rows(places, self.clock.now()).collect(), held);
            let (width, height) = dialog.opening_extent(scale, theme);
            let mode = app::mode_for(width, height);
            let Some(surface) = Surface::new(width, height) else {
                app::report(APP_NAME, "no memory for the zone chooser");
                return;
            };
            let opened = WindowPane::open_dialog(
                client,
                (self.main.id(), self.server),
                self.endpoint,
                &mode,
                DialogModality::OwnerModal,
                dialog.title(),
            );
            match opened {
                Ok(pane) => {
                    let mut pane = Drawn {
                        pane,
                        surface,
                        owed: Owed::new(),
                    };
                    pane.owed.owe_whole();
                    self.chooser = Some(Chooser {
                        pane,
                        dialog,
                        written: Minute::of(self.clock.now()),
                    });
                }
                Err(err) => {
                    let _ = app::refuse(
                        client,
                        (APP_NAME, self.endpoint),
                        Some(self.main.id()),
                        "The time zone chooser could not be opened",
                        &alloc::format!("{err}"),
                    );
                }
            }
        }

        /// Close the zone chooser, if it is open.
        fn close_chooser(&mut self, client: &mut Client) {
            if let Some(chooser) = self.chooser.take() {
                if let Err(err) = chooser.pane.pane.close(client) {
                    app::report(
                        APP_NAME,
                        format_args!("the zone chooser would not close ({err})"),
                    );
                }
            }
        }

        /// Close both windows, so a close asked for takes effect at once
        /// while the work it left owed is seen out.
        fn close(mut self, client: &mut Client) {
            self.close_chooser(client);
            if let Err(err) = self.main.pane.close(client) {
                app::report(APP_NAME, format_args!("the window would not close ({err})"));
            }
        }

        /// Route one event to the zone chooser.
        fn chooser_event(
            &mut self,
            event: &WindowEvent,
            client: &mut Client,
            desk: &Desk,
            (desktop, theme, scale): (&Desktop, &Theme, Scale),
        ) {
            let Some(chooser) = self.chooser.as_mut() else {
                return;
            };
            let mode = chooser.pane.mode();
            let bounds = Rect::new(0, 0, mode.width_px, mode.height_px);
            let mut damage = Region::new();
            let timing = PressTiming {
                now_ns: tairix_rt::clock_get(),
                double_click: desktop.info().double_click(),
            };
            let mut outcome = None;
            match *event {
                WindowEvent::Pointer { x, y, action, .. } => {
                    for input in pointer_input_events(action, pointer_point(x, y)) {
                        outcome = outcome.or(chooser.dialog.on_pointer(
                            &input,
                            timing,
                            bounds,
                            scale,
                            theme,
                            &mut damage,
                        ));
                    }
                }
                WindowEvent::Scrolled { x, y, dx, dy, .. } => {
                    for input in scroll_input_events(pointer_point(x, y), dx, dy) {
                        outcome = outcome.or(chooser.dialog.on_pointer(
                            &input,
                            timing,
                            bounds,
                            scale,
                            theme,
                            &mut damage,
                        ));
                    }
                }
                WindowEvent::Key { key, .. } => {
                    if let InputEvent::KeyPressed { key, modifiers } = key_input_event(key) {
                        outcome = chooser.dialog.on_key(
                            key,
                            modifiers,
                            bounds,
                            scale,
                            theme,
                            &mut damage,
                        );
                    }
                }
                WindowEvent::Focus { focused: false, .. } => {
                    chooser
                        .dialog
                        .pointer_left(bounds, scale, theme, &mut damage);
                }
                WindowEvent::CloseRequested { .. } => outcome = Some(PlaceOutcome::Cancelled),
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => chooser.pane.resize(client, width_px, height_px),
                WindowEvent::ContentReleased { .. } => chooser.pane.pane.release_frames(),
                WindowEvent::RedrawRequested { .. } => chooser.pane.owed.owe_whole(),
                _ => {}
            }
            chooser.pane.owe(&damage);
            match outcome {
                Some(PlaceOutcome::Chose { row }) => {
                    let chosen = self.places.as_ref().and_then(|places| places.zone(row));
                    self.close_chooser(client);
                    if let Some(zone) = chosen.filter(|zone| self.machine.zone != Some(*zone)) {
                        let _ =
                            self.submit(Job::Change(SettingChange::Zone(Some(zone))), client, desk);
                    }
                }
                Some(PlaceOutcome::Cancelled) => self.close_chooser(client),
                None => {}
            }
        }

        /// Route one event to the main window; `true` ends the dialog.
        fn main_event(
            &mut self,
            event: &WindowEvent,
            client: &mut Client,
            desk: &Desk,
            (theme, scale): (&Theme, Scale),
        ) -> bool {
            let mut damage = Region::new();
            let mut requests = Vec::new();
            match *event {
                WindowEvent::Pointer { x, y, action, .. } => {
                    for input in pointer_input_events(action, pointer_point(x, y)) {
                        requests.extend(self.window.on_pointer(&input, scale, theme, &mut damage));
                    }
                }
                WindowEvent::Scrolled { x, y, dx, dy, .. } => {
                    for input in scroll_input_events(pointer_point(x, y), dx, dy) {
                        requests.extend(self.window.on_pointer(&input, scale, theme, &mut damage));
                    }
                }
                WindowEvent::Key { key, .. } => {
                    if let InputEvent::KeyPressed { key, modifiers } = key_input_event(key) {
                        requests.extend(self.window.on_key(
                            key,
                            modifiers,
                            scale,
                            theme,
                            &mut damage,
                        ));
                    }
                }
                WindowEvent::Focus { focused: false, .. } => {
                    self.window.pointer_left(scale, theme, &mut damage);
                }
                WindowEvent::CloseRequested { .. } => return true,
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => self.main.resize(client, width_px, height_px),
                WindowEvent::ContentReleased { .. } => self.main.pane.release_frames(),
                WindowEvent::RedrawRequested { .. } => self.main.owed.owe_whole(),
                _ => {}
            }
            self.main.owe(&damage);
            for request in requests {
                if self.serve(request, client, desk, (theme, scale)) {
                    return true;
                }
            }
            false
        }

        /// Present whatever both windows owe, the main one first fitted to
        /// what it now holds: a tick can rewrap a line as surely as an answer.
        fn present(
            &mut self,
            client: &mut Client,
            (theme, scale): (&Theme, Scale),
        ) -> Result<(), Errno> {
            self.refit(client, (theme, scale));
            let window = &self.window;
            self.main
                .present(client, |surface| window.render(surface, scale, theme))?;
            if let Some(chooser) = self.chooser.as_mut() {
                let mode = chooser.pane.mode();
                let bounds = Rect::new(0, 0, mode.width_px, mode.height_px);
                let dialog = &chooser.dialog;
                chooser.pane.present(client, |surface| {
                    dialog.render(surface, bounds, scale, theme);
                })?;
            }
            Ok(())
        }

        /// When the next tick is due: just past the clock's next whole
        /// second, from the reading the last tick took.
        fn next_tick(&self) -> u64 {
            let into = match self.clock {
                Clock::Read { reading, .. } => u64::from(reading.time().subsec_nanos()),
                Clock::Unreadable => 0,
            };
            tairix_rt::clock_get()
                .saturating_add(u64::from(NANOS_PER_SEC).saturating_sub(into))
                .saturating_add(TICK_SLACK_NS)
        }
    }

    /// The provenance a clock reading carries, to notice another source
    /// setting it.
    fn state_of(clock: Clock) -> Option<WallTimeState> {
        match clock {
            Clock::Read { reading, .. } => Some(reading.state()),
            Clock::Unreadable => None,
        }
    }

    /// The machine's zone read with the store's own region: the clock the
    /// login screen reads, which is the one this dialog sets.
    fn civil_clock(
        zones: Option<&'static ZoneInfo<'static>>,
        zone: Option<&ZoneName>,
    ) -> CivilClock<'static> {
        let preference: Vec<&str> = zone.map(ZoneName::as_str).into_iter().collect();
        let (resolved, _) = resolve_zone(zones, &preference);
        let region = zones.and_then(ZoneInfo::default_region);
        CivilClock::new(resolved, region.as_ref())
    }

    /// `now`'s local day and time of day.
    fn local_parts(civil: &CivilClock<'_>, now: Time64) -> (Date, TimeOfDay) {
        let local = civil.local(now).civil;
        let date = Date::new(local.year, local.month, local.day).unwrap_or(FALLBACK_DATE);
        let time = TimeOfDay::new(local.hour, local.minute, local.second).unwrap_or_default();
        (date, time)
    }

    /// The day the controls show where the clock reads a year past the
    /// calendar's.
    const FALLBACK_DATE: Date = Date {
        year: 1970,
        month: 1,
        day: 1,
    };

    /// Declare the icon-bar presence: the session's information row and
    /// *Quit*, the primary click raising the window. A refused declaration is
    /// stated and the dialog carries on without a slot of its own.
    fn declare_app_bar(client: &mut Client, endpoint: u64) {
        let declared = tairix_window::info_and_quit(endpoint, AppBarClick::Raise);
        if let Err(refused) = tairix_window::declare_app_bar(client, declared) {
            app::report(APP_NAME, format_args!("{refused}"));
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime is
    /// set up and routes its return value through the `exit` syscall.
    #[allow(clippy::too_many_lines)] // One linear bring-up, then one drain-then-paint loop whose steps share the windows and the worker.
    fn main() -> i32 {
        // This task drives a user-facing loop, so it declares the frame it
        // owes; a shippable image arms nothing and answers zero.
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
        let mut client = WindowClient::new(app::RtWindowTransport);
        let (mut desktop, mut themes) = match app::bring_up_desktop(&mut client) {
            Ok(pair) => pair,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let binding = match app::bind_event_mailbox() {
            Ok(binding) => binding,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        declare_app_bar(&mut client, binding.endpoint());

        let desk = match Desk::queued(work, (), tairix_rt::sync::WorkerWake::create(), QUEUE) {
            Ok(desk) => Arc::new(desk),
            Err(_) => return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "no memory for the worker"),
        };
        let _guard = tairix_rt::work::WorkerGuard::new(&desk);
        if let Err(reason) = Worker::start(&desk) {
            app::report(
                APP_NAME,
                format_args!("no worker ({reason:?}); its work is done on the event loop"),
            );
        }
        if let Err(err) = app::watch_wake(binding.set(), desk.wake(), WORKER_TOKEN) {
            return app::fail(
                APP_NAME,
                EXIT_CHANNEL_LOST,
                format_args!("worker wake refused ({err})"),
            );
        }

        // Nothing is owed the screen until the window opens, so the first
        // read is waited for: the window then opens on the machine's own
        // settings and zone rather than flashing a guess first.
        if desk.submit(Job::Read { zones: true }).is_err() {
            return app::fail(
                APP_NAME,
                EXIT_CHANNEL_LOST,
                "the first read could not be asked",
            );
        }
        let Some(Answer::Read(readings)) = desk.wait() else {
            return app::fail(
                APP_NAME,
                EXIT_CHANNEL_LOST,
                "the first read was not answered",
            );
        };
        desk.wake().drain();

        let (theme, scale) = Dialog::theme_scale(&themes, &desktop);
        let zones = readings
            .zones
            .as_ref()
            .and_then(|zones| zones.as_ref().ok().copied());
        let machine = readings
            .config
            .as_ref()
            .map_or(MachineTime::of(&SystemConfig::default()), MachineTime::of);
        let civil = civil_clock(zones, machine.zone.as_ref());
        let place = zones.map_or_else(
            || String::from(civil.zone().name()),
            |zones| label(zones, civil.zone().name()),
        );
        let clock = match tairix_rt::wall_time() {
            Ok(reading) => Clock::Read {
                reading,
                last_sync: readings.record.last_sync,
            },
            Err(_) => Clock::Unreadable,
        };
        let (today, time) = clock
            .now()
            .map_or((FALLBACK_DATE, TimeOfDay::default()), |now| {
                local_parts(&civil, now)
            });
        let window = Window::new(
            readout(clock, machine.network, &place, &civil),
            (machine.network, &place),
            (civil.first_weekday(), civil.hour_cycle()),
            (today, time),
            (scale, theme),
        );
        let extent = window.extent(scale, theme);
        let mode = app::mode_for(extent.0, extent.1);
        let Some(surface) = Surface::new(extent.0, extent.1) else {
            return app::fail(APP_NAME, app::EXIT_NO_WINDOW, "no memory for the window");
        };
        let (pane, server) = match WindowPane::open(
            &mut client,
            binding.endpoint(),
            &mode,
            TITLE,
            WindowSizing::Fixed,
        ) {
            Ok(opened) => opened,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let mut dialog = Dialog {
            window,
            main: Drawn {
                pane,
                surface,
                owed: Owed::new(),
            },
            server,
            endpoint: binding.endpoint(),
            machine: MachineTime::default(),
            zones: None,
            places: None,
            civil,
            place,
            last_sync: None,
            clock,
            chooser: None,
            question: None,
            stepping: false,
            asked: extent,
        };
        dialog.main.owed.owe_whole();
        dialog.adopt_readings(readings, (theme, scale));
        if dialog.present(&mut client, (theme, scale)).is_err() {
            return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "first present refused");
        }

        let deadline = Cell::new(dialog.next_tick());
        let woke = Cell::new(Woke::default());
        let mut events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(binding.endpoint(), server),
            set: binding.set(),
            deadline: &deadline,
            woke: &woke,
        });
        let code = 'serve: loop {
            // Park for the first event or wake, then drain whatever else is
            // queued, so a burst of input costs one frame.
            let mut first = true;
            loop {
                let next = if first {
                    events.wait(&mut client)
                } else {
                    events.try_wait(&mut client)
                };
                first = false;
                let event = match next {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(EventError::Undecodable(_)) => continue,
                    Err(EventError::Refused(refused)) => {
                        app::report(APP_NAME, refused);
                        continue;
                    }
                    Err(EventError::Mailbox(_)) => {
                        break 'serve app::fail(APP_NAME, EXIT_CHANNEL_LOST, "event channel lost");
                    }
                };
                let (theme, scale) = Dialog::theme_scale(&themes, &desktop);
                match event {
                    WindowEvent::AlertClosed { alert_id, outcome } => {
                        dialog.answered(alert_id, outcome, &mut client, &desk, (theme, scale));
                    }
                    WindowEvent::AppBarMenu { item } if tairix_window::is_quit(item) => {
                        dialog.close(&mut client);
                        break 'serve 0;
                    }
                    _ => {
                        let id = event.window_id();
                        if id.is_some() && id == dialog.chooser.as_ref().map(|c| c.pane.id()) {
                            dialog.chooser_event(
                                &event,
                                &mut client,
                                &desk,
                                (&desktop, theme, scale),
                            );
                        } else if id == Some(dialog.main.id())
                            && dialog.main_event(&event, &mut client, &desk, (theme, scale))
                        {
                            dialog.close(&mut client);
                            break 'serve 0;
                        }
                    }
                }
            }

            let due = woke.replace(Woke::default());
            if due.desktop {
                match app::adopt_desktop(&mut desktop, &mut themes) {
                    Ok(true) => {
                        let (theme, scale) = Dialog::theme_scale(&themes, &desktop);
                        dialog.window.fit(scale, theme);
                        dialog.main.owed.owe_whole();
                        if let Some(chooser) = dialog.chooser.as_mut() {
                            chooser.pane.owed.owe_whole();
                        }
                    }
                    Ok(false) => {}
                    Err(err) => {
                        app::report(APP_NAME, format_args!("desktop change refused: {err}"));
                    }
                }
            }
            let (theme, scale) = Dialog::theme_scale(&themes, &desktop);
            if due.worker {
                desk.wake().drain();
                desk.collect_landed(|answer| {
                    dialog.take_answer(answer, &mut client, (theme, scale));
                });
            }
            if due.tick || tairix_rt::clock_get() >= deadline.get() {
                if dialog.tick((theme, scale)) {
                    let _ = dialog.submit(Job::Read { zones: false }, &mut client, &desk);
                }
                deadline.set(dialog.next_tick());
            }
            if dialog.present(&mut client, (theme, scale)).is_err() {
                break 'serve app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
            }
        };
        see_out(&desk);
        code
    }

    /// See the work left owed out before the program ends, so a change or a
    /// clock step already asked for is never dropped with the worker; what
    /// it comes to is stated on `stderr`, the window being gone.
    fn see_out(desk: &Desk) {
        while let Some(answer) = desk.wait() {
            match answer {
                Answer::Changed(change, Err(err)) => {
                    let (headline, detail) = change.refusal(err);
                    app::report(APP_NAME, format_args!("{headline}: {detail}"));
                }
                Answer::ClockSet(set) => {
                    if let Some((headline, detail)) = set.refusal() {
                        app::report(APP_NAME, format_args!("{headline}: {detail}"));
                    } else if let Some(note) = set.note() {
                        app::report(APP_NAME, note);
                    }
                }
                Answer::Read(_) | Answer::Changed(_, Ok(_)) => {}
            }
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry — the freestanding `tairix-rt` `_start` path — is not compiled, so
// this inert `main` keeps the crate building under the host tooling. It
// performs no I/O.
#[cfg(not(freestanding))]
fn main() {}
