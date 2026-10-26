//! The `Run` entry-point binary of the graphical login screen, installed at
//! `/System/Services/greeter.app/Run` — the screen the `login` authority
//! spawns as the `greeter` service account to ask who is at the machine
//! (`plans/NEW-DESKTOP-LOGIN.md` G3).
//!
//! This is a **pure-Rust** program: it links the Rust userland runtime
//! `tairix-rt` for `_start`, the stack canary, the panic handler, the
//! `#[global_allocator]`, and the seat / shared-memory / wait-set / IPC /
//! clock syscall wrappers. `tairix_rt::entry!` names this program's `main`.
//!
//! # What it does
//!
//! Acquire the boot seat's exclusive lease, query the display mode, map a
//! double-buffered frame region and grant it to the display service, page the
//! offerable accounts off `SESSION_ENDPOINT`, raise the ribbon of light behind
//! the column, paint the first frame, and then **park**: one wait set holding
//! the seat's input, with a timeout set to the next thing that actually needs
//! a repaint — the ribbon's next frame among them.
//!
//! Left untouched for the energy-saving wait, the screen goes back to rest
//! and black and its display is switched off, or kept black where it cannot
//! be; from then on it presents nothing and arms no timer until input wakes
//! it, and the gesture that does reaches nothing behind it.
//!
//! A verified secret fades the screen to black and then exits `0`; the
//! authority is watching for that exit and starts the session itself, and the
//! desktop comes up out of the same black. The fade is bounded and cannot
//! fail: a lost seat, a refused present, or a stopped clock ends it early and
//! the exit is still `0`. Everything else keeps asking.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
// Compiled only for the freestanding service binary, which links the optional
// `tairix-rt` runtime through the default `program` feature. Host tooling
// builds only this crate's *library*, so this module never enters that build.
#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;

    use tairix_abi::display_ipc::DISPLAY_ENDPOINT;
    use tairix_abi::driver::display::{DamageRect, DisplayMode, OutputMode, RefreshRate};
    use tairix_abi::input::{KeyInput, TimedPointer};
    use tairix_abi::seat::ReleaseSurface;
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::session_ipc::SESSION_ENDPOINT;
    use tairix_abi::sysinfo::{SysinfoQueryId, SystemIdentity};
    use tairix_abi::time::{Time64, WallTimeState};
    use tairix_abi::touch::TouchFrame;
    use tairix_abi::DriverError;
    use tairix_abi::NoticeTopic;
    use tairix_abi::{Errno, WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
    use tairix_display::{
        establish_display, DisplayClient, DisplayTransport, FramesRefused, Presenter, RtDisplay,
        SwitchedOff,
    };
    use tairix_geometry::Scale;
    use tairix_greeter::{Courier, Ticket, Verdict};
    use tairix_greeter_service::accounts::{load_accounts, DirectoryError, SessionTransport};
    use tairix_greeter_service::chrome::identity_line;
    use tairix_greeter_service::cursor::pointer_image;
    use tairix_greeter_service::events::{
        ACCOUNTS_UNAVAILABLE, AUTHORITY_ON_LOOP, AUTHORITY_UNREACHABLE, DISPLAY_ASLEEP,
        DISPLAY_KEPT_BLACK, DISPLAY_WAKE_REFUSED, POINTER_UNAVAILABLE, RIBBON_UNAVAILABLE,
        SCREEN_READY, SCREEN_READY_MESSAGE, SCREEN_UNAVAILABLE, VERDICT_RECEIVED,
        ZONES_UNAVAILABLE,
    };
    use tairix_greeter_service::frame::{Present, Scanout};
    use tairix_greeter_service::screen::LoginScreen;
    use tairix_greeter_service::verify::{exchange, Check, Checked};
    use tairix_greeter_service::zones::zone_store_grant;
    use tairix_log::{Event, EventId, Field, FieldValue, Level};
    use tairix_procinfo::{call, IpcTransport};
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_rt::LogSink;
    use tairix_theme::{Appearance, ThemeRegistry};
    use tairix_tz::{CivilClock, ZoneInfo};
    use tairix_window::app;

    /// Wait-set token for "the seat has input waiting".
    const SEAT_TOKEN: u64 = 0;

    /// Wait-set token for "the display's output moved" (`plans/DISPLAY.md`
    /// D12).
    const DISPLAY_OUTPUT_TOKEN: u64 = 1;

    /// Wait-set token for "the authority answered a check".
    const AUTHORITY_TOKEN: u64 = 2;

    /// Wait-set token for "the display latched the last frame".
    const DISPLAY_EVENT_TOKEN: u64 = 3;

    /// The display the screen presents through: the frames its driver
    /// allocated, mapped by the ring and unmapped with it, and the port the
    /// driver posts each latch to.
    type Ring = RtDisplay<RtDisplayTransport>;

    /// The thread every check goes to the authority on, so the key derivation
    /// it waits on never holds a frame.
    type AuthorityWorker = Worker<RtSessionTransport, Check, Checked>;

    /// The screen as it runs: asking through its worker.
    type Screen = LoginScreen<ToWorker>;

    /// The seat's lease was refused, or taken away while the screen ran, so
    /// there is no screen to own.
    const EXIT_NO_SEAT: i32 = 3;

    /// The display could not describe a screen with pixels on it.
    const EXIT_NO_DISPLAY: i32 = 4;

    /// The shared frame region was refused.
    const EXIT_NO_FRAMES: i32 = 5;

    /// The wait set could not be built, so the loop could only busy-poll.
    const EXIT_NO_WAITSET: i32 = 6;

    /// The name this program states its refusals under.
    const APP_NAME: &str = "greeter";

    /// State one reason on `stderr` and in the audit trail and answer `code`,
    /// so no abnormal exit is a bare number. The reason never names an
    /// account or a secret.
    fn fail(code: i32, reason: &str) -> i32 {
        record(SCREEN_UNAVAILABLE, Level::Error, reason);
        app::fail(APP_NAME, code, reason)
    }

    /// The audit sink every record goes through.
    static LOG_SINK: LogSink = LogSink;

    /// Emit one audit record.
    fn record(id: EventId, level: Level, message: &str) {
        let _ = tairix_log::log(
            &LOG_SINK,
            &Event {
                level,
                id,
                message,
                fields: &[Field {
                    key: "service",
                    value: FieldValue::Str(APP_NAME),
                }],
            },
        );
    }

    /// The production [`DisplayTransport`]: one synchronous `ipc_call` to the
    /// reserved display endpoint. The service re-checks this task's live seat
    /// lease kernel-side on every request, so the transport carries no claimed
    /// authority.
    struct RtDisplayTransport;

    impl DisplayTransport for RtDisplayTransport {
        fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            tairix_rt::ipc_call(DISPLAY_ENDPOINT, request, reply).map_err(Errno::from_syscall)
        }
    }

    /// The production [`SessionTransport`]: one synchronous `ipc_call` to the
    /// session authority. The authority checks this task's *attested* uid and
    /// console on every request; nothing here claims anything.
    struct RtSessionTransport;

    impl SessionTransport for RtSessionTransport {
        fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            tairix_rt::ipc_call(SESSION_ENDPOINT, request, reply).map_err(Errno::from_syscall)
        }
    }

    /// The [`Courier`] the running screen asks through: its worker, which
    /// carries a check on the screen's own thread only where the kernel
    /// granted no thread.
    struct ToWorker(Arc<AuthorityWorker>);

    impl Courier for ToWorker {
        type Answer = Checked;

        fn send(&mut self, ticket: Ticket, account: &str, secret: &str) -> Result<(), Checked> {
            self.0.submit(Check::new(ticket, account, secret)?);
            Ok(())
        }

        fn receive(&mut self) -> Option<Checked> {
            self.0.collect()
        }
    }

    /// The machine's identity line, from what the information service says
    /// of it: an unreachable or refusing service leaves it naming the OS
    /// alone.
    fn identity() -> String {
        let reported = call(&IpcTransport, SysinfoQueryId::SYSTEM_IDENTITY, &[])
            .ok()
            .and_then(|payload| SystemIdentity::from_bytes(&payload).ok());
        identity_line(reported.as_ref())
    }

    /// The clock the screen tells the time in: the machine's own zone, which
    /// login hands it as `TZ`, else the zone store's, in the store's region,
    /// since no one has signed in to choose another; UTC, recorded, where no
    /// store can be had.
    fn civil() -> CivilClock<'static> {
        match zones() {
            Ok(zones) => {
                let machine =
                    tairix_rt::env_var(b"TZ").and_then(|tz| core::str::from_utf8(tz).ok());
                let (zone, _) = tairix_tz::resolve_zone(Some(zones), machine.as_slice());
                CivilClock::new(zone, zones.default_region().as_ref())
            }
            Err(reason) => {
                record(
                    ZONES_UNAVAILABLE,
                    Level::Info,
                    &alloc::format!("greeter: {reason}; the clock reads UTC"),
                );
                CivilClock::UTC
            }
        }
    }

    /// The zone store, read through the delegation the session authority
    /// mints this process, or why it could not be had.
    fn zones() -> Result<&'static ZoneInfo<'static>, String> {
        let handle = zone_store_grant(&mut RtSessionTransport)
            .map_err(|refusal| String::from(refusal.reason()))?;
        let file = tairix_rt::File::from_delegation(handle.get()).map_err(|raw| {
            alloc::format!(
                "the zone store delegation could not be taken ({})",
                Errno::from_syscall(raw)
            )
        })?;
        tairix_tz::load(&file).map_err(|error| alloc::format!("{error}"))
    }

    /// The wall clock, or `None` when no trusted time has been set.
    fn wall_now() -> Option<Time64> {
        let reading = tairix_rt::wall_time().ok()?;
        (reading.state() != WallTimeState::Unset).then(|| reading.time())
    }

    /// The machine's offerable accounts as chooser tiles.
    ///
    /// An unreadable directory is logged and becomes an empty list: the
    /// chooser always carries its typed-name tile, so a user can still log in.
    fn tiles() -> Vec<tairix_greeter::AccountTile> {
        let mut transport = RtSessionTransport;
        match load_accounts(&mut transport) {
            Ok(tiles) => tiles,
            Err(err) => {
                unavailable_directory(err);
                Vec::new()
            }
        }
    }

    /// Audit an account directory that could not be read.
    fn unavailable_directory(err: DirectoryError) {
        let _ = tairix_log::log(
            &LOG_SINK,
            &Event {
                level: Level::Warn,
                id: ACCOUNTS_UNAVAILABLE,
                message: "greeter: the account directory could not be read",
                fields: &[
                    Field {
                        key: "service",
                        value: FieldValue::Str(APP_NAME),
                    },
                    Field {
                        key: "reason",
                        value: FieldValue::Str(err.reason()),
                    },
                ],
            },
        );
    }

    /// Audit one answer the authority gave.
    fn audit(answer: Verdict) {
        let (id, level, message) = match answer {
            Verdict::Verified => (
                VERDICT_RECEIVED,
                Level::Info,
                "greeter: a secret was verified",
            ),
            Verdict::Refused => (
                VERDICT_RECEIVED,
                Level::Info,
                "greeter: a secret was refused",
            ),
            Verdict::Unreachable => (
                AUTHORITY_UNREACHABLE,
                Level::Warn,
                "greeter: the authority gave no answer",
            ),
        };
        record(id, level, message);
    }

    /// Audit a display's refusal to change its power, naming the refusal.
    fn refused(id: EventId, message: &str, refusal: DriverError) {
        let _ = tairix_log::log(
            &LOG_SINK,
            &Event {
                level: Level::Warn,
                id,
                message,
                fields: &[
                    Field {
                        key: "service",
                        value: FieldValue::Str(APP_NAME),
                    },
                    Field {
                        key: "error",
                        value: FieldValue::Error(refusal.as_errno()),
                    },
                ],
            },
        );
    }

    /// Hand `present` to the display, when there is an output to show it on,
    /// answering what it could not take yet: a display still reading the
    /// last frame keeps the frame owed until its latch.
    fn show(display: &mut Option<Ring>, frame: &[u8], present: Present) -> Present {
        let Some(display) = display.as_mut() else {
            return Present::Nothing;
        };
        if !display.ready() {
            return present;
        }
        let whole = [DamageRect::full(&display.mode())];
        let damage = match &present {
            Present::Nothing => return Present::Nothing,
            Present::Region(list) => list.rects(),
            Present::Whole => &whole,
        };
        match display.present(frame, damage) {
            Err(DriverError::Busy) => present,
            // Any other refusal is the output's to announce, or a lost seat
            // the next drain observes.
            Ok(()) | Err(_) => Present::Nothing,
        }
    }

    /// Hold `ring` as the screen's display in place of whatever it held, its
    /// latch port waited on in `set` in place of the last one's.
    ///
    /// # Errors
    ///
    /// The wait-set's refusal of the port, which leaves no display held: a
    /// frame the device queued would never be heard to latch.
    fn hold_display(set: u64, display: &mut Option<Ring>, ring: Option<Ring>) -> Result<(), Errno> {
        if let Some(old) = display.take() {
            let _ = tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Del,
                WaitSourceKind::Port,
                old.events_port(),
                DISPLAY_EVENT_TOKEN,
            );
        }
        let Some(ring) = ring else {
            return Ok(());
        };
        let added = tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Port,
            ring.events_port(),
            DISPLAY_EVENT_TOKEN,
        );
        if added < 0 {
            return Err(Errno::from_syscall(added));
        }
        *display = Some(ring);
        Ok(())
    }

    /// How draining one of the seat's channels ended.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum Drained {
        /// The channel is empty; the loop may park.
        Empty,
        /// The seat gave neither a whole record nor an empty channel. The
        /// lease is gone or the channel is unreadable, and a lost lease reads
        /// ready forever, so parking again would spin.
        Lost,
    }

    /// Adopt every answer the authority has given, auditing each, answering
    /// whether one verified the secret the screen was waiting on.
    fn adopt_answers(screen: &mut Screen) -> bool {
        while let Some(step) = screen.collect(tairix_rt::clock_get()) {
            audit(step.answer.verdict);
            if step.verified {
                return true;
            }
        }
        false
    }

    /// Drain the seat's pointer channel into the screen, which owes its frame
    /// what the burst changed until the round draws it once.
    fn drain_pointer(screen: &mut Screen) -> Drained {
        let mut wire = [0u8; TimedPointer::WIRE_LEN];
        loop {
            let read = tairix_rt::pointer_read(SEAT_PRIMARY, &mut wire);
            if read == 0 {
                break Drained::Empty;
            }
            // Anything but one whole record is unreadable: a short count would
            // decode fresh bytes together with the tail of the last record.
            if usize::try_from(read).ok() != Some(TimedPointer::WIRE_LEN) {
                break Drained::Lost;
            }
            let Ok(timed) = TimedPointer::from_bytes(&wire) else {
                continue;
            };
            screen.on_pointer(&timed.input, timed.at_ns);
        }
    }

    /// Drain the seat's touch channel through the screen's recogniser, then act
    /// on any touch whose deadline has passed, every queued frame having been
    /// read; the screen owes its frame what they changed.
    fn drain_touch(screen: &mut Screen) -> Drained {
        let mut wire = [0u8; TouchFrame::WIRE_LEN];
        let mut gestures = Vec::new();
        loop {
            let read = tairix_rt::touch_read(SEAT_PRIMARY, &mut wire);
            if read == 0 {
                screen.expire_touch(tairix_rt::clock_get(), &mut gestures);
            } else if usize::try_from(read).ok() != Some(TouchFrame::WIRE_LEN) {
                break Drained::Lost;
            } else if let Ok(frame) = TouchFrame::from_bytes(&wire) {
                screen.feed_touch(&frame, &mut gestures);
            }
            for (gesture, at_ns) in gestures.drain(..) {
                screen.on_gesture(gesture, at_ns);
            }
            if read == 0 {
                break Drained::Empty;
            }
        }
    }

    /// Drain the seat's keyboard channel into the screen, which owes its frame
    /// what the keystrokes changed until the round draws it once.
    fn drain_keyboard(screen: &mut Screen) -> Drained {
        let mut wire = [0u8; KeyInput::WIRE_LEN];
        loop {
            let read = tairix_rt::keyboard_read(SEAT_PRIMARY, &mut wire);
            if read == 0 {
                break Drained::Empty;
            }
            // Anything but one whole record is unreadable: a short count would
            // decode fresh bytes together with the tail of the last record.
            if usize::try_from(read).ok() != Some(KeyInput::WIRE_LEN) {
                break Drained::Lost;
            }
            let Ok(input) = KeyInput::from_bytes(&wire) else {
                continue;
            };
            let event = tairix_window::key_input_event(input);
            screen.on_input(&event, tairix_rt::clock_get());
        }
    }

    /// The frames the display allocated for `mode`, taken the one way every
    /// presenter does.
    fn frames_for(mode: &DisplayMode) -> Result<Ring, FramesRefused> {
        establish_display(DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY), *mode)
    }

    /// The output the screen comes up on, and frames for it: the one
    /// connected now, or the first to arrive, parked for on `set`, which
    /// already holds the output's announcement so none is missed between a
    /// read and the park.
    ///
    /// `Err` is the exit code the failure earns, its reason already stated.
    fn first_output(set: u64) -> Result<(Ring, OutputMode), i32> {
        let mut client = DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY);
        let mut wait = false;
        loop {
            if wait {
                let mut token = 0u64;
                if tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token) < 0 {
                    return Err(fail(EXIT_NO_WAITSET, "the display output's wait failed"));
                }
            }
            let Ok(output) = client.query() else {
                return Err(fail(
                    EXIT_NO_DISPLAY,
                    "the display service refused the output query, so there is no screen to draw on",
                ));
            };
            // With no output, and when the output moved before frames reached
            // it, its announcement is what comes next.
            wait = true;
            let Some(output) = output else {
                continue;
            };
            match frames_for(&output.mode) {
                Ok(ring) => return Ok((ring, output)),
                Err(FramesRefused::Configure(Errno::Stale)) => {}
                Err(refused) => return Err(fail(EXIT_NO_FRAMES, refused.reason())),
            }
        }
    }

    /// Converge on the display's output once (`plans/DISPLAY.md` D12): with
    /// none, present nothing; with one, frames established afresh — the
    /// output may be a new sink, or a restarted service holding none of the
    /// old ones — and the screen rebuilt in place when the mode is another,
    /// answering the whole of it to show.
    fn follow_output(screen: &mut Screen, (display, set): (&mut Option<Ring>, u64)) -> Present {
        let output = DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY).query();
        let _ = hold_display(set, display, None);
        // A refused query is a lost seat, which the next drain observes.
        let Ok(Some(OutputMode { mode, refresh })) = output else {
            return Present::Nothing;
        };
        screen.set_refresh_rate(refresh);
        let rebuilt = if &mode == screen.mode() {
            None
        } else {
            let Some(scanout) = Scanout::new(mode) else {
                record(
                    SCREEN_UNAVAILABLE,
                    Level::Warn,
                    "greeter: the display's new mode describes no drawable screen",
                );
                return Present::Nothing;
            };
            Some(scanout)
        };
        match frames_for(&mode) {
            Ok(ring) => {
                if hold_display(set, display, Some(ring)).is_err() {
                    record(
                        SCREEN_UNAVAILABLE,
                        Level::Warn,
                        "greeter: the display's latches cannot be waited on",
                    );
                    return Present::Nothing;
                }
            }
            // It moved again, and that announcement brings the screen onto
            // where it went.
            Err(FramesRefused::Configure(Errno::Stale)) => return Present::Nothing,
            Err(refused) => {
                record(SCREEN_UNAVAILABLE, Level::Warn, refused.reason());
                return Present::Nothing;
            }
        }
        match rebuilt {
            Some(scanout) => screen.adopt_scanout(scanout, tairix_rt::clock_get()),
            None => screen.repaint(),
        }
    }

    /// The login screen on `scanout`, paced by `refresh`, asking through
    /// `courier`: its ribbon raised and painted across every online CPU, and
    /// its pointer drawn.
    fn login_screen(scanout: Scanout, refresh: Option<RefreshRate>, courier: ToWorker) -> Screen {
        // The ribbon is a night sky, so the column is drawn in the dark theme
        // whichever appearance the desktop defaults to: a dark ink on it
        // would be unreadable.
        let mut themes = ThemeRegistry::with_builtins();
        let _ = themes.set_appearance(Appearance::Dark);
        let online = tairix_procinfo::online_cpus(&IpcTransport);
        let mut screen = LoginScreen::new(
            scanout,
            themes.active().clone(),
            Scale::ONE,
            (identity(), civil()),
            tiles(),
            courier,
            alloc::boxed::Box::new(app::pool_across(
                APP_NAME,
                online,
                "drawing the login screen",
            )),
        );
        screen.set_refresh_rate(refresh);
        if !screen.raise_ribbon(tairix_rt::clock_get()) {
            record(
                RIBBON_UNAVAILABLE,
                Level::Info,
                "greeter: the ribbon could not be drawn, so the flat desktop colour stands in",
            );
        }
        match pointer_image(Scale::ONE) {
            Some(image) => screen.set_pointer(image),
            None => record(
                POINTER_UNAVAILABLE,
                Level::Info,
                "greeter: the pointer cursor could not be drawn",
            ),
        }
        screen
    }

    /// Own the seat and run the screen until a secret is verified.
    ///
    /// Split from `main` so every exit after the acquire flows back through
    /// the one owner-checked release.
    fn session() -> i32 {
        // A negative return is the kernel's refusal, and it is exactly what
        // the conversion rejects, so one check covers both. The output's
        // announcement joins before the output is read.
        let Ok(set) = u64::try_from(tairix_rt::waitset_create()) else {
            return fail(EXIT_NO_WAITSET, "the wait set could not be created");
        };
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::DisplayOutput.as_u32()),
            DISPLAY_OUTPUT_TOKEN,
        ) < 0
        {
            return fail(
                EXIT_NO_WAITSET,
                "the display's output could not be waited on",
            );
        }
        let (ring, OutputMode { mode, refresh }) = match first_output(set) {
            Ok(first) => first,
            Err(code) => return code,
        };
        let Some(scanout) = Scanout::new(mode) else {
            return fail(
                EXIT_NO_DISPLAY,
                "the queried mode describes no drawable screen, or no memory holds its frame",
            );
        };
        let mut display = None;
        if hold_display(set, &mut display, Some(ring)).is_err() {
            return fail(
                EXIT_NO_WAITSET,
                "the display's latches could not be waited on",
            );
        }

        let worker = Arc::new(Worker::new(
            exchange::<RtSessionTransport>,
            RtSessionTransport,
            WorkerWake::create(),
        ));
        let threaded = Worker::start(&worker).is_ok();
        let _worker_guard = WorkerGuard::new(&worker);
        let woken = worker.wake().read_end().is_some_and(|read| {
            tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                AUTHORITY_TOKEN,
            ) >= 0
        });
        if !threaded || !woken {
            // An answer that could never wake the loop would never be adopted,
            // so the checks are carried on this thread instead.
            worker.stop();
            record(
                AUTHORITY_ON_LOOP,
                Level::Warn,
                "greeter: no worker can carry checks to the authority, so the screen waits for each answer",
            );
        }

        let mut screen = login_screen(scanout, refresh, ToWorker(Arc::clone(&worker)));
        // The opening frame is the veil at full black, with the chrome already
        // up behind it, so the screen appears out of the black the seat was
        // handed over cleared to rather than snapping onto it; the park loop
        // below runs the fade off the same deadline as every other animation.
        let opening = screen.open(tairix_rt::clock_get(), wall_now());
        let owed = show(&mut display, screen.frame(), opening);
        record(SCREEN_READY, Level::Info, SCREEN_READY_MESSAGE);

        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::SeatInput,
            SEAT_PRIMARY,
            SEAT_TOKEN,
        ) < 0
        {
            return fail(EXIT_NO_WAITSET, "the seat's input could not be waited on");
        }
        park_loop(&mut screen, &mut display, (set, worker.wake()), owed)
    }

    /// Serve the screen until it is finished, parking between wakes.
    ///
    /// Each round drains the seat, adopts whatever the authority has answered,
    /// wakes a sleeping display for whatever input reached it, brings the
    /// clock, any lockout and the ribbon up to date, draws and presents what
    /// all of that changed once, puts the display to sleep once the screen has
    /// been left alone long enough, and then parks on `set` until input or an
    /// answer arrives or the nearest of those falls due. A sleeping screen
    /// arms no timer at all.
    fn park_loop(
        screen: &mut Screen,
        display: &mut Option<Ring>,
        (set, answered): (u64, &WorkerWake),
        mut owed: Present,
    ) -> i32 {
        let mut told_unswitchable = false;
        let mut told_unwakeable = false;
        let mut token = SEAT_TOKEN;
        loop {
            if token == DISPLAY_EVENT_TOKEN {
                if let Some(ring) = display.as_mut() {
                    ring.take_events();
                }
            }
            let mut round = if token == DISPLAY_OUTPUT_TOKEN {
                follow_output(screen, (display, set))
            } else {
                core::mem::replace(&mut owed, Present::Nothing)
            };
            if token == AUTHORITY_TOKEN {
                answered.drain();
            }
            let lost = drain_keyboard(screen) == Drained::Lost
                || drain_pointer(screen) == Drained::Lost
                || drain_touch(screen) == Drained::Lost;
            if lost {
                return fail(
                    EXIT_NO_SEAT,
                    "the seat stopped delivering readable input, so the screen is no longer ours",
                );
            }
            if adopt_answers(screen) {
                let verified = round.merged(screen.draw(), screen.mode());
                let owed = show(display, screen.frame(), verified);
                fade_out(screen, display, set, owed);
                return 0;
            }
            let now = tairix_rt::clock_get();
            let wall = wall_now();
            let woke = match display.as_mut() {
                Some(ring) => screen.wake(ring, now, wall),
                None => Ok(Present::Nothing),
            };
            match woke {
                Ok(Present::Nothing) => {}
                Ok(opening) => {
                    told_unwakeable = false;
                    round = round.merged(opening, screen.mode());
                }
                Err(refusal) => {
                    if !told_unwakeable {
                        told_unwakeable = true;
                        refused(
                            DISPLAY_WAKE_REFUSED,
                            "greeter: the display would not switch back on",
                            refusal,
                        );
                    }
                }
            }
            // The clock, a running lockout and the ribbon ask the authority
            // nothing, so they add to the round's frame and to no verdict.
            screen.refresh(now, wall);
            round = round.merged(screen.draw(), screen.mode());
            owed = show(display, screen.frame(), round);
            let slept = display
                .as_mut()
                .and_then(|ring| screen.sleep_if_idle(ring, tairix_rt::clock_get()));
            match slept {
                Some(SwitchedOff::Off) => record(
                    DISPLAY_ASLEEP,
                    Level::Info,
                    "greeter: the screen sat untouched, so its display was switched off",
                ),
                Some(SwitchedOff::Blanked) if !told_unswitchable => {
                    told_unswitchable = true;
                    record(
                        DISPLAY_KEPT_BLACK,
                        Level::Info,
                        "greeter: this display cannot switch itself off, so the sleeping screen is kept black",
                    );
                }
                Some(SwitchedOff::Refused(refusal)) => refused(
                    DISPLAY_KEPT_BLACK,
                    "greeter: the display would not switch off, so the sleeping screen is kept black",
                    refusal,
                ),
                Some(SwitchedOff::Blanked) | None => {}
            }
            let timeout = screen.park_timeout(tairix_rt::clock_get(), wall);
            token = SEAT_TOKEN;
            let woken = tairix_rt::waitset_wait(set, timeout, &mut token);
            // Ready and the armed deadline are the two ways round the loop;
            // any other answer would leave nothing to park on, so exit rather
            // than spin.
            if woken < 0 && Errno::from_syscall(woken) != Errno::TimedOut {
                return fail(EXIT_NO_WAITSET, "the seat's input wait failed");
            }
        }
    }

    /// Take the screen to black, then let the caller leave.
    ///
    /// The desktop cannot appear until this process exits, so the screen goes
    /// black first and the desktop comes up out of the same black. It is a
    /// cosmetic step on an already-made decision, so every way it can go
    /// wrong ends it rather than the login: the loop is bounded by the frames
    /// the fade can ask for, a wait that fails or a seat that stops
    /// delivering ends it, and a refused present is ignored like every other.
    ///
    /// The seat's channels are still drained, though nothing acts on them:
    /// input left unread reads ready forever, and the park would return at
    /// once instead of pacing the fade.
    fn fade_out(screen: &mut Screen, display: &mut Option<Ring>, set: u64, owed: Present) {
        let opening = owed.merged(
            screen.begin_session_fade(tairix_rt::clock_get()),
            screen.mode(),
        );
        let mut owed = show(display, screen.frame(), opening);
        for _ in 0..screen.session_fade_budget() {
            let Some(due) = screen.session_fade_due(tairix_rt::clock_get()) else {
                return;
            };
            if drain_keyboard(screen) == Drained::Lost
                || drain_pointer(screen) == Drained::Lost
                || drain_touch(screen) == Drained::Lost
            {
                return;
            }
            let mut token = 0u64;
            let woken = tairix_rt::waitset_wait(set, due, &mut token);
            if woken < 0 && Errno::from_syscall(woken) != Errno::TimedOut {
                return;
            }
            if token == DISPLAY_EVENT_TOKEN {
                if let Some(ring) = display.as_mut() {
                    ring.take_events();
                }
            }
            let darkened = screen.session_fade_step(tairix_rt::clock_get());
            owed = show(
                display,
                screen.frame(),
                owed.merged(darkened, screen.mode()),
            );
        }
    }

    /// The program's entry point.
    ///
    /// Exit codes: `0` once a secret is verified and the screen has faded to
    /// black, and otherwise the bring-up code that names what was missing,
    /// each stated on `stderr`.
    fn main() -> i32 {
        // This task drives a user-facing loop, so declare the frame it owes.
        // A debug image then reports any span that overruns, naming the call
        // that spent it; a shippable one arms nothing and answers zero, which
        // is why the result is not examined.
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
        if tairix_rt::display_acquire(SEAT_PRIMARY) < 1 {
            return fail(
                EXIT_NO_SEAT,
                "the seat is already held, so there is no screen to own",
            );
        }
        let code = session();
        // Owner-checked release on every exit path: a lease already lost
        // refuses with a typed error, ignored — heal, never widen.
        //
        // A clean exit means a verified secret and a screen already faded to
        // black, with the authority about to start a desktop on this seat, so
        // the screen is handed on cleared rather than replaying the text
        // console into the gap. Every other exit is going back to the text
        // login, which needs its console to say why.
        let next = if code == 0 {
            ReleaseSurface::Handover
        } else {
            ReleaseSurface::Text
        };
        let _ = tairix_rt::display_release(SEAT_PRIMARY, next);
        code
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// Whenever the freestanding `tairix-rt` `_start` path is not compiled — on the
// host (`cargo build --workspace`, clippy, fmt), or for a `program`-less build
// of this crate — this inert `main` keeps the crate building under the host
// tooling. It performs no I/O.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
