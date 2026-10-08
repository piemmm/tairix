//! The `settings.app` bundle's `Run` entry point: the windowed Settings
//! application (`plans/NEW-DESKTOP-SETTINGS.md`).
//!
//! # It browses pictures it may not read
//!
//! The Wallpaper and Screensaver panes offer pictures of the shipped stores,
//! and this application holds no filesystem capability and no sandbox to
//! decode a picture with. Both are *served*: the desktop session lists the
//! wallpaper store once and answers a catalog page, and renders one picture
//! at a time into a shared-memory region this program created and granted —
//! which is the one thing its `CAP_SHM` already allows. So there is one
//! sandboxed decode path on the desktop rather than two, and no untrusted
//! picture is ever decoded in the address space of the application that
//! browses them.
//!
//! Everything with behaviour worth testing lives in the host-tested shell
//! (`tairix_settings`); this binary only composes it over the live window
//! channel, exactly as `userland/apps/widgets` composes its gallery:
//!
//! * one `shm_create`d frame region granted to the reserved window endpoint;
//! * one `port_bind`-bound event mailbox the app **parks** on through its
//!   wait-set — never a poll loop — accepting only events whose
//!   kernel-attested sender is the desktop session the create reply named;
//! * the `WindowClient` calls and the `WindowEvents` typed wait over it.
//!
//! The window is resizable: a `Resized` event re-maps the frame region and
//! the shell lays itself out to the new client, shedding the strip where it
//! no longer fits. Every bring-up refusal exits fail-loud with a reserved
//! code and a stated reason on `stderr`.
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

    use tairix_abi::driver::display::{DamageRect, DisplayMode};
    use tairix_abi::elevate::{ElevateArgv, ElevateReply, ElevateRequest, ELEVATE_MAX_REPLY};
    use tairix_abi::input::KeyInput;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::net_ipc::{NetServerAddr, MAX_RESOLVER_SERVERS};
    use tairix_abi::pinboard_ipc::PinboardDocument;
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::sysinfo::{SysinfoQueryId, SystemIdentity, Uptime};
    use tairix_abi::window_ipc::{PreviewOutcome, PreviewSubject, WindowEvent};
    use tairix_abi::{Errno, ProcId};
    use tairix_appdata::RtHost;
    use tairix_controls::Keystroke;
    use tairix_geometry::{Rect, Region, Scale};
    use tairix_icon::{
        artwork_cache, ArtworkCache, IconArtworkSource, InlineArtwork, NoArtworkSeam,
    };
    use tairix_input::InputEvent;
    use tairix_procinfo::{for_each_mount, IpcTransport, WalkStep};
    use tairix_reclaim::PressureBand;
    use tairix_settings::{
        notified, win_sizing, AccountFacts, DesktopAnswer, DesktopAsk, DesktopAsks, ElevateRefusal,
        Elevated, Elevation, MachineFacts, OwnAccount, Pane, PictureWanted, Renders, Roster,
        RunMode, Shell, ShellOutcome, VolumeReading, MOST_OUTSTANDING, WINDOW_GROUND, WIN_HEIGHT,
        WIN_WIDTH,
    };
    use tairix_sysconfig::SystemConfig;
    use tairix_theme::{CursorSetId, Theme, ThemeRegistry};
    use tairix_users::{Salt, SALT_LEN};
    use tairix_wallpaper::{ApplyOutcome, CatalogItem, DesktopSettings, PINBOARD_PUBLISHER};
    use tairix_window::app::{self, AppWindow, Wake, EXIT_CHANNEL_LOST};
    use tairix_window::{
        key_input_event, pointer_input_events, pointer_point, present_damage, scroll_input_events,
        Desktop, EventDrain, EventError, EventMailbox, EventSource, Parked, Repaint, Target,
        WindowEvents,
    };

    /// The wait-set token of the applier's wake pipe: readable exactly when
    /// the desktop session has answered an apply, so the rows are brought up
    /// to date through the park the loop is already in rather than by
    /// waiting for it.
    const APPLY_TOKEN: u64 = app::FIRST_APP_TOKEN;

    /// The wait-set token of the mount walk's wake: readable exactly when a
    /// fresh mount table has landed, so the Storage pane is brought up to
    /// date through the park the loop is already in.
    ///
    /// Its own desk rather than the applier's: the two carry different work
    /// and a shared latest-wins desk would let one evict the other.
    const MOUNTS_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;

    /// The wait-set token of the machine-reading desk's wake: readable
    /// exactly when a fresh reading of the machine's configuration and
    /// facts has landed.
    const MACHINE_TOKEN: u64 = app::FIRST_APP_TOKEN + 2;

    /// The wait-set token of the elevated-run desk's wake: readable exactly
    /// when the console's broker has answered an offered account.
    ///
    /// Its own desk because it is the one round trip that blocks for as
    /// long as another program takes to run — a window that waited on it
    /// would stop answering for the whole of an authentication and a store
    /// write.
    const ELEVATE_TOKEN: u64 = app::FIRST_APP_TOKEN + 3;

    /// The wait-set token of the network-reading desk's wake: readable
    /// exactly when a fresh reading of the stack's resolver set has landed.
    ///
    /// Its own desk rather than the machine desk's: the two are wanted by
    /// different panes, and folding them together would spend a network
    /// round trip every time the About pane came on show.
    const NETWORK_TOKEN: u64 = app::FIRST_APP_TOKEN + 4;

    /// The wait-set token of the account-reading desk's wake: readable
    /// exactly when a fresh reading of the caller's own record, the two
    /// public directories and a salt has landed.
    ///
    /// Its own desk for the same reason the network one is: it is wanted by
    /// one pane, and folding it into another's would spend three round
    /// trips every time that other pane came on show.
    const ACCOUNTS_TOKEN: u64 = app::FIRST_APP_TOKEN + 5;

    /// The wait-set token of the preview asker's wake: readable exactly when
    /// the desktop has answered a render this window asked for.
    const ASK_TOKEN: u64 = app::FIRST_APP_TOKEN + 6;

    /// The wait-set token of the desktop asker's wake: readable exactly when
    /// the session has answered a lock, a screensaver preview, or which
    /// sources have notified.
    const DESKTOP_TOKEN: u64 = app::FIRST_APP_TOKEN + 7;

    /// The desktop settings in effect for the launching user, so every
    /// composed row opens on what the desktop is actually drawn with.
    ///
    /// Read from the desktop session's **published** app-data scope, which
    /// is the sanctioned channel one application reaches another's values
    /// through: this program names the publisher and nothing else, so the
    /// request shape it sends cannot ask for the session's private
    /// settings. It never writes them — an application publishes only its
    /// own scope — so a change is a request the session decides on.
    ///
    /// A desktop that has published **nothing** means the documented
    /// defaults and is not an error: a fresh account has never applied a
    /// setting. Anything else that stops the document being used says so on
    /// `stderr` rather than showing values the user cannot account for.
    fn settings_in_effect() -> DesktopSettings {
        let document = match tairix_appdata::read_published(&mut RtHost, PINBOARD_PUBLISHER) {
            Ok(document) => document,
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!(
                        "the desktop's settings could not be read ({err:?}); showing the \
                     defaults"
                    ),
                );
                return DesktopSettings::default();
            }
        };
        let (settings, refused) = DesktopSettings::load(&document);
        for key in refused {
            app::report(
                APP_NAME,
                format_args!(
                    "the desktop publishes a `{key}` this build does not accept; showing \
                 its default"
                ),
            );
        }
        settings
    }

    /// The applier: the session round trip an apply costs, and the read of
    /// what the store then holds, carried out on a worker thread.
    ///
    /// The session answers only once its own publisher has written the
    /// store, so making the choice wait for it would freeze this window for
    /// a disk commit — and freeze it again on every further choice. The loop
    /// encodes the document (in memory, and refusable on the spot), submits,
    /// and adopts the answer on the wake it nudges.
    type Applier = tairix_rt::work::Worker<(), PinboardDocument, Applied>;

    /// What an apply answered, and the settings the rows are to show for it.
    struct Applied {
        outcome: ApplyOutcome,
        /// What the store holds once the session answered; `None` for an
        /// apply refused before it was asked, which changed nothing.
        in_effect: Option<DesktopSettings>,
    }

    /// The worker's body: the shared apply client's one round trip, then the
    /// store's own account of what it holds.
    fn send_apply(_: &mut (), document: &mut PinboardDocument) -> Applied {
        Applied {
            outcome: tairix_wallpaper::apply(*document),
            in_effect: Some(settings_in_effect()),
        }
    }

    /// The mount walk: the system mount table, read through the one shared
    /// client every surface reads it through.
    ///
    /// Carried out on a worker thread because it is a paged IPC round trip,
    /// and a window that waited on it would stop answering for as long as
    /// the service took. A walk that fails part way keeps what arrived —
    /// listing the volumes it did learn about is better than listing none —
    /// and states the reason rather than showing a shorter table silently.
    type Mounts = tairix_rt::work::Worker<(), (), Vec<VolumeReading>>;

    /// The machine readings: the boot-time configuration store, and the
    /// facts the About and Date & Time panes state.
    ///
    /// One desk for all of them because they are one refresh: a pane that
    /// comes on show wants the reading that backs it, and the queries are
    /// each a single ungated round trip. A reading that is refused is left
    /// absent, so its row states that rather than showing a value the
    /// reader could not account for.
    type Machine = tairix_rt::work::Worker<(), (), (Option<SystemConfig>, MachineFacts)>;

    /// The machine readings' body.
    fn read_machine(_: &mut (), (): &mut ()) -> (Option<SystemConfig>, MachineFacts) {
        let config = match tairix_procinfo::system_config(&IpcTransport) {
            Ok(config) => config.or_else(|| Some(SystemConfig::default())),
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!(
                        "the machine's configuration could not be read ({err:?}); its rows \
                     show no value"
                    ),
                );
                None
            }
        };
        (config, machine_facts())
    }

    /// The network readings: the resolver set the stack is actually using.
    ///
    /// Carried out on a worker thread because it is a paged IPC round trip
    /// like the mount walk, and a window that waited on it would stop
    /// answering for as long as the service took.
    type Network = tairix_rt::work::Worker<(), (), Option<Vec<NetServerAddr>>>;

    /// The network readings' body.
    ///
    /// A refused or undecodable walk leaves the set absent rather than
    /// empty: "no server answered the question" and "the stack holds no
    /// server" are different facts, and the pane states which it has.
    fn read_network(_: &mut (), (): &mut ()) -> Option<Vec<NetServerAddr>> {
        let mut resolvers = Vec::with_capacity(MAX_RESOLVER_SERVERS);
        match tairix_procinfo::for_each_resolver_server(&IpcTransport, |record| {
            resolvers.push(*record);
            // The stack bounds its own set, so a reply that keeps going
            // past the bound is a service this window will not grow a heap
            // for. Stopping is an ordinary success, not a failure.
            if resolvers.len() >= MAX_RESOLVER_SERVERS {
                return Ok(tairix_procinfo::WalkStep::Stop);
            }
            Ok(tairix_procinfo::WalkStep::Continue)
        }) {
            Ok(()) => Some(resolvers),
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("the name servers could not be read ({err:?}); the pane says so"),
                );
                None
            }
        }
    }

    /// The account readings: the caller's own record, the two ungated
    /// directories, and a fresh salt for hashing a password with.
    ///
    /// One worker job rather than four round trips from the loop, for the
    /// same reason the machine desk bundles its readings: the pane wants
    /// them together and a window that waited on any of them would stop
    /// answering.
    type Accounts = tairix_rt::work::Worker<(), (), (AccountFacts, Option<Salt>)>;

    /// The account readings' body.
    ///
    /// Each directory is absent rather than empty when its walk failed:
    /// "the directory could not be read" and "the machine holds no
    /// account" are different facts, and the pane states which it has. The
    /// listing half is left `Unasked` — only an authenticated run answers
    /// it, and this desk holds no authority at all.
    fn read_accounts(_: &mut (), (): &mut ()) -> (AccountFacts, Option<Salt>) {
        let facts = AccountFacts {
            own: own_account(),
            users: directory(|sink| {
                tairix_procinfo::for_each_user(&IpcTransport, |record| {
                    sink((
                        record.uid,
                        tairix_procinfo::field_lossy(record.name_bytes()),
                    ));
                    Ok(tairix_procinfo::WalkStep::Continue)
                })
            }),
            groups: directory(|sink| {
                tairix_procinfo::for_each_group(&IpcTransport, |record| {
                    sink((
                        record.gid,
                        tairix_procinfo::field_lossy(record.name_bytes()),
                    ));
                    Ok(tairix_procinfo::WalkStep::Continue)
                })
            }),
            roster: Roster::Unasked,
        };
        (facts, draw_salt())
    }

    /// The caller's own account record, read ungated against the uid the
    /// kernel attested.
    ///
    /// Three answers, told apart: a record, a uid no database holds, and a
    /// read that could not be taken. A display that collapsed the last two
    /// would report the machine's state for its own failure.
    fn own_account() -> OwnAccount {
        match tairix_procinfo::self_account(&IpcTransport) {
            Ok(Some(record)) => OwnAccount::Known(alloc::boxed::Box::new(record)),
            Ok(None) => OwnAccount::Unknown,
            Err(err) => {
                app::report(APP_NAME, format_args!("this session's own account could not be read ({err:?}); the pane                      says so"));
                OwnAccount::Unmeasured
            }
        }
    }

    /// Collect one id-and-name directory, or `None` where the walk failed.
    fn directory(
        walk: impl FnOnce(&mut dyn FnMut((u32, String))) -> Result<(), tairix_procinfo::ListError>,
    ) -> Option<Vec<(u32, String)>> {
        let mut listed = Vec::new();
        let outcome = walk(&mut |entry| listed.push(entry));
        match outcome {
            Ok(()) => Some(listed),
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("a directory could not be read ({err:?}); the pane says so"),
                );
                None
            }
        }
    }

    /// One fresh salt from the kernel CSPRNG through the unprivileged
    /// `sys:random` resource.
    ///
    /// Refuses, never guesses: a failed draw leaves the pane with no salt,
    /// which refuses a password apply rather than hashing under something
    /// predictable.
    fn draw_salt() -> Option<Salt> {
        let fd = u32::try_from(tairix_rt::resource_open(
            b"sys:random",
            tairix_abi::OpenFlags::READ,
        ))
        .ok()?;
        let mut salt = [0u8; SALT_LEN];
        let outcome = tairix_rt::fs_read(fd, 0, &mut salt);
        let _ = tairix_rt::fs_close(fd);
        match outcome {
            Ok(read) if read == SALT_LEN => Some(salt),
            _ => {
                app::report(
                    APP_NAME,
                    "no randomness was available; a new password cannot be hashed here",
                );
                None
            }
        }
    }

    /// The machine facts the About and Date & Time panes state, each taken
    /// on its own so one refusal costs one reading rather than all of them.
    fn machine_facts() -> MachineFacts {
        MachineFacts {
            identity: read_scalar(SysinfoQueryId::SYSTEM_IDENTITY)
                .and_then(|reply| SystemIdentity::from_bytes(&reply).ok()),
            uptime: read_scalar(SysinfoQueryId::UPTIME)
                .and_then(|reply| Uptime::from_bytes(&reply).ok()),
            cpus: tairix_procinfo::cpu_info(&IpcTransport).unwrap_or_default(),
            memory_bytes: tairix_procinfo::memory_total_bytes(&IpcTransport).ok(),
            clock: wall_clock(),
        }
    }

    /// One scalar System Information API reading.
    fn read_scalar(query: SysinfoQueryId) -> Option<Vec<u8>> {
        tairix_procinfo::call(&IpcTransport, query, &[]).ok()
    }

    /// The machine's wall clock, or `None` where it could not be read.
    fn wall_clock() -> Option<tairix_abi::time::WallClockReading> {
        tairix_rt::wall_time().ok()
    }

    /// The elevated run: the console broker's round trip, carried out on a
    /// worker thread.
    ///
    /// The broker re-authenticates the offered account, runs the program as
    /// it, and answers only once that program has exited — which is exactly
    /// as long as an authentication and a store write take. The loop
    /// submits and collects the answer on the wake it nudges, so the window
    /// keeps drawing throughout.
    type Elevator = tairix_rt::work::Worker<(), Elevation, Elevated>;

    /// A render asked of the desktop for a pane's picture, and the grant of the
    /// region it is drawn into.
    #[derive(Copy, Clone)]
    struct PreviewAsk {
        window_id: u64,
        grant: u64,
        wanted: PictureWanted,
    }

    /// What the desktop answered a [`PreviewAsk`].
    type Asked = (PreviewAsk, Result<(), Errno>);

    /// The worker that carries a render request to the desktop, one at a time
    /// and each answered.
    type Asker = tairix_rt::work::Worker<
        tairix_window::WindowClient<app::RtWindowTransport>,
        PreviewAsk,
        Asked,
        tairix_util::defer::JobQueue<PreviewAsk, Asked>,
    >;

    /// Ask the desktop for one render: a round trip to its serve loop, which
    /// the event loop must not wait on.
    fn ask_preview(
        client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
        ask: &mut PreviewAsk,
    ) -> Asked {
        let wanted = ask.wanted;
        let answer = client.render_preview(
            (ask.window_id, ask.grant),
            wanted.subject,
            (wanted.width, wanted.height),
        );
        (*ask, answer)
    }

    /// The worker that carries what only the desktop answers, each request in
    /// turn and at most one of each kind outstanding.
    type DesktopAsker = tairix_rt::work::Worker<
        tairix_window::WindowClient<app::RtWindowTransport>,
        DesktopAsk,
        DesktopAnswer,
        tairix_util::defer::JobQueue<DesktopAsk, DesktopAnswer>,
    >;

    /// Carry one request to the desktop's serve loop, which the event loop
    /// must not wait on.
    fn ask_desktop(
        client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
        ask: &mut DesktopAsk,
    ) -> DesktopAnswer {
        match ask {
            DesktopAsk::Lock => DesktopAnswer::Lock(client.lock_screen()),
            DesktopAsk::Preview(document) => {
                DesktopAnswer::Preview(client.preview_screensaver(document.as_str()))
            }
            DesktopAsk::NotifySources => {
                let mut frame = [0u8; tairix_abi::window_ipc::WINDOW_NOTIFY_SOURCES_REPLY_MAX];
                DesktopAnswer::NotifySources(
                    client
                        .notify_sources(&mut frame)
                        .map(|answered| notified(&answered)),
                )
            }
        }
    }

    /// Adopt what the desktop answered into `shell`, stating a refusal.
    fn adopt_desktop_answer(shell: &mut Shell, answer: DesktopAnswer) {
        if let Some((what, err)) = answer.refusal() {
            app::report(
                APP_NAME,
                format_args!("the desktop would not {what} ({err})"),
            );
        }
        answer.adopt(shell);
    }

    /// The desktop asker's client half: its desk, and which kinds of request
    /// are outstanding.
    struct DesktopDesk<'a> {
        worker: &'a DesktopAsker,
        asks: DesktopAsks,
    }

    impl DesktopDesk<'_> {
        /// Submit `ask` unless one of its kind is outstanding, answering
        /// whether an answer landed at once — as one does with no worker to
        /// serve it, the request made on this thread.
        fn submit(&mut self, shell: &mut Shell, ask: &DesktopAsk) -> bool {
            self.asks.ask(ask) && self.send(shell, ask)
        }

        /// Hand `ask`, already marked outstanding, to the worker.
        fn send(&mut self, shell: &mut Shell, ask: &DesktopAsk) -> bool {
            match self.worker.submit(*ask) {
                Ok(true) => self.settle(shell),
                Ok(false) => false,
                // The desk holds one of each kind, so this is never reached;
                // were it, the kind is free to be asked again.
                Err(ask) => {
                    self.asks.withdraw(&ask);
                    false
                }
            }
        }

        /// Ask which sources have notified if the pane that lists them has
        /// come on show, answering whether an answer landed at once.
        fn request_sources(&mut self, shell: &mut Shell) -> bool {
            shell.notify_sources_wanted() && self.submit(shell, &DesktopAsk::NotifySources)
        }

        /// Adopt every answer that has landed, answering whether any had.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let mut landed = false;
            let mut follow = None;
            let asks = &mut self.asks;
            self.worker.collect_landed(|answer| {
                follow = asks.answered(&answer).or(follow);
                adopt_desktop_answer(shell, answer);
                landed = true;
            });
            match follow {
                Some(ask) => self.send(shell, &ask) || landed,
                None => landed,
            }
        }
    }

    /// The elevated run's body: one posted request, one verdict.
    ///
    /// The offered password is wiped as soon as the exchange resolves,
    /// whichever way it went: the request it was encoded into is erased by
    /// the runtime's own wiped buffer, and the copy this desk was handed is
    /// erased here.
    fn send_elevate(_: &mut (), asked: &mut Elevation) -> Elevated {
        let argv: Vec<&str> = asked.argv.iter().map(String::as_str).collect();
        let verdict = elevate_once(asked, &argv);
        // Before the desk's own drop, because it keeps the job it was
        // handed until the next one replaces it.
        asked.erase();
        verdict
    }

    /// Post one elevation request and turn the reply into a verdict.
    fn elevate_once(asked: &Elevation, argv: &[&str]) -> Elevated {
        // A secret that is not text was never one the broker could check,
        // so it is refused here rather than put on the wire.
        let Ok(password) = core::str::from_utf8(&asked.password) else {
            return Elevated::Refused(ElevateRefusal::Credentials);
        };
        let Ok(carried) = ElevateArgv::new(argv) else {
            return Elevated::Refused(ElevateRefusal::NotRun(String::from(
                "That is more than one command can carry.",
            )));
        };
        let request = match asked.mode {
            RunMode::Wait => ElevateRequest::Run {
                username: &asked.account,
                password,
                program: asked.program,
                argv: carried,
            },
            RunMode::Capture => ElevateRequest::Capture {
                username: &asked.account,
                password,
                program: asked.program,
                argv: carried,
            },
            RunMode::Leave => ElevateRequest::Launch {
                username: &asked.account,
                password,
                program: asked.program,
            },
        };
        let mut reply = [0u8; ELEVATE_MAX_REPLY];
        match tairix_rt::elevate(&request, &mut reply) {
            Ok(ElevateReply::Completed { exit_code }) => Elevated::Finished(exit_code),
            Ok(ElevateReply::Captured { exit_code, output }) => {
                Elevated::Printed(exit_code, output.to_vec())
            }
            Ok(ElevateReply::Overran { .. }) => Elevated::Overran,
            // A launch is started and left running; there is no exit code
            // to wait for and none is invented.
            Ok(ElevateReply::Launched { .. }) => Elevated::Finished(0),
            Ok(ElevateReply::Refused(Errno::PermissionDenied)) => {
                Elevated::Refused(ElevateRefusal::Credentials)
            }
            Ok(ElevateReply::Refused(err)) => Elevated::Refused(ElevateRefusal::NotRun(
                alloc::format!("The account was accepted, but nothing ran ({err})."),
            )),
            // A reply to a request this program did not send: nothing ran
            // on an answer it does not understand.
            Ok(ElevateReply::Verified) | Err(_) => Elevated::Refused(ElevateRefusal::NotRun(
                String::from("This console has no way to ask for an account."),
            )),
        }
    }

    /// The mount walk's body.
    fn read_mounts(_: &mut (), (): &mut ()) -> Vec<VolumeReading> {
        let mut volumes = Vec::new();
        if let Err(err) = for_each_mount(&IpcTransport, |record| {
            volumes.push(VolumeReading::of(record));
            Ok(WalkStep::Continue)
        }) {
            app::report(
                APP_NAME,
                format_args!(
                    "the mount table could not be read ({err:?}); the storage pane shows \
                 what arrived"
                ),
            );
        }
        volumes
    }

    /// The mount walk's client half: the desk it is submitted to, and
    /// whether one is outstanding.
    ///
    /// One walk at a time, because the pane only ever wants the latest
    /// answer and re-asking while one is in flight would spend a round trip
    /// on a table it is about to be told anyway.
    struct MountWalk<'a> {
        worker: &'a Mounts,
        pending: bool,
    }

    impl MountWalk<'_> {
        /// Ask for a fresh mount table if the pane wants one and none is
        /// outstanding, answering whether the pane changed.
        ///
        /// Submitted, never awaited: the answer arrives as an ordinary wake.
        fn request(&mut self, shell: &mut Shell) -> bool {
            if self.pending || !shell.volumes_wanted() {
                return false;
            }
            // With no worker the walk was made on this thread and its
            // answer is already on the desk.
            if self.worker.submit(()) {
                return self.settle(shell);
            }
            self.pending = true;
            false
        }

        /// Adopt a landed mount table, answering whether the pane changed.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let Some(volumes) = self.worker.collect() else {
                return false;
            };
            self.pending = false;
            shell.adopt_volumes(volumes);
            true
        }
    }

    /// The machine-reading desk's client half.
    ///
    /// One reading at a time, because a pane only ever wants the latest
    /// and re-asking while one is in flight would spend a round trip on
    /// readings it is about to be told anyway.
    struct MachineRead<'a> {
        worker: &'a Machine,
        pending: bool,
    }

    impl MachineRead<'_> {
        /// Ask for a fresh reading if a pane wants one and none is
        /// outstanding, answering whether anything on screen changed.
        fn request(&mut self, shell: &mut Shell) -> bool {
            if self.pending || !shell.config_wanted() {
                return false;
            }
            if self.worker.submit(()) {
                return self.settle(shell);
            }
            self.pending = true;
            false
        }

        /// Adopt a landed reading, answering whether anything changed.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let Some((config, facts)) = self.worker.collect() else {
                return false;
            };
            self.pending = false;
            shell.adopt_config(config);
            shell.adopt_machine(facts);
            true
        }
    }

    /// The network-reading desk's client half.
    ///
    /// One reading at a time, for the same reason the machine desk holds
    /// one: a pane only ever wants the latest.
    struct NetworkRead<'a> {
        worker: &'a Network,
        pending: bool,
    }

    impl NetworkRead<'_> {
        /// Ask for a fresh reading if a pane wants one and none is
        /// outstanding, answering whether anything on screen changed.
        fn request(&mut self, shell: &mut Shell) -> bool {
            if self.pending || !shell.network_wanted() {
                return false;
            }
            if self.worker.submit(()) {
                return self.settle(shell);
            }
            self.pending = true;
            false
        }

        /// Adopt a landed reading, answering whether anything changed.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let Some(facts) = self.worker.collect() else {
                return false;
            };
            self.pending = false;
            shell.adopt_resolvers(facts);
            true
        }
    }

    /// The account-reading desk's client half.
    ///
    /// One reading at a time, for the same reason every other desk holds
    /// one: the pane only ever wants the latest.
    struct AccountRead<'a> {
        worker: &'a Accounts,
        pending: bool,
    }

    impl AccountRead<'_> {
        /// Ask for a fresh reading if the pane wants one, or wants a salt
        /// it has spent, and none is outstanding.
        fn request(&mut self, shell: &mut Shell) -> bool {
            if self.pending || !(shell.accounts_wanted() || shell.salt_wanted()) {
                return false;
            }
            if self.worker.submit(()) {
                return self.settle(shell);
            }
            self.pending = true;
            false
        }

        /// Adopt a landed reading, answering whether anything changed.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let Some((facts, salt)) = self.worker.collect() else {
                return false;
            };
            self.pending = false;
            shell.adopt_accounts(facts);
            shell.adopt_salt(salt);
            true
        }
    }

    /// Ask the desktop session to adopt `document`, off the event loop.
    ///
    /// A document this program cannot even encode is refused here, where it
    /// costs nothing; everything else goes to the worker and is answered on
    /// a later wake.
    fn submit_apply(applier: &Applier, document: &str) -> Option<Applied> {
        match PinboardDocument::new(document) {
            // With no worker the call was made on this thread and its answer
            // is already on the desk.
            Ok(document) if applier.submit(document) => applier.collect(),
            Ok(_) => None,
            Err(_) => Some(Applied {
                outcome: ApplyOutcome::Refused(String::from("settings document out of range")),
                in_effect: None,
            }),
        }
    }

    /// Ask the console's broker to run what an offered account authorises,
    /// off the event loop.
    fn submit_elevate(elevator: &Elevator, asked: Elevation) -> Option<Elevated> {
        // With no worker the call was made on this thread and its answer is
        // already on the desk.
        if elevator.submit(asked) {
            return elevator.collect();
        }
        None
    }

    /// Adopt what the desktop answered: state a refusal, then show what the
    /// store holds.
    ///
    /// Persist-then-adopt. The rows showed the reader's choice at once; the
    /// durable value is what the store answered with, read on the worker, so
    /// a refusal puts the row back rather than leaving a value on screen the
    /// next login would not restore.
    fn adopt_apply(shell: &mut Shell, Applied { outcome, in_effect }: Applied) {
        match outcome {
            ApplyOutcome::Applied | ApplyOutcome::Applying => {}
            ApplyOutcome::Refused(reason) => {
                app::report(
                    APP_NAME,
                    format_args!("the desktop refused the change: {reason}"),
                );
            }
            ApplyOutcome::NoDesktop => {
                app::report(APP_NAME, "no desktop session answered; nothing was changed");
            }
        }
        match in_effect {
            Some(settings) => shell.adopt_settings(settings),
            None => shell.revert_settings(),
        }
    }

    /// The shipped picture catalog the desktop offers, read once.
    ///
    /// The store is on the read-only `/System` volume, so the session
    /// listed it at bring-up and answers from memory: this costs a round
    /// trip per page and no I/O either side, which is why it is done here
    /// rather than deferred. A page that cannot be read leaves the gallery
    /// with whatever arrived — an empty gallery still offers "no picture"
    /// — and states the reason once.
    fn fetch_catalog(
        client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
    ) -> Vec<CatalogItem> {
        let mut page = [0u8; tairix_abi::window_ipc::WINDOW_WALLPAPERS_REPLY_MAX];
        let mut catalog: Vec<CatalogItem> = Vec::new();
        loop {
            let Ok(from) = u16::try_from(catalog.len()) else {
                return catalog;
            };
            let answered = match client.wallpapers(from, &mut page) {
                Ok(answered) => answered,
                Err(err) => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "the desktop's picture catalog could not be read ({err}); the \
                         gallery shows what arrived"
                        ),
                    );
                    return catalog;
                }
            };
            let total = answered.total;
            // An empty page with entries still outstanding is a session
            // that cannot answer them, not a queue to keep asking: stop
            // rather than turn the read into a spin.
            if answered.is_empty() {
                return catalog;
            }
            for entry in answered.entries() {
                let (Ok(category), Ok(file)) = (
                    core::str::from_utf8(entry.category),
                    core::str::from_utf8(entry.file),
                ) else {
                    continue;
                };
                catalog.push(CatalogItem {
                    category: String::from(category),
                    file: String::from(file),
                });
            }
            if catalog.len() >= usize::from(total) {
                return catalog;
            }
        }
    }

    /// The cursor sets the desktop offers, read once.
    ///
    /// The store is on the read-only `/System` volume, so the session
    /// listed it at bring-up and answers from memory: this costs one round
    /// trip and no I/O either side. An answer that cannot be read leaves
    /// the pointer-set row offering the built-in set alone — which is
    /// honest, since that is the one set every desktop has — and states
    /// the reason once.
    fn fetch_cursor_sets(
        client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
    ) -> Vec<CursorSetId> {
        let mut frame = [0u8; tairix_abi::window_ipc::WINDOW_CURSOR_SETS_REPLY_MAX];
        let answered = match client.cursor_sets(&mut frame) {
            Ok(answered) => answered,
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!(
                        "the desktop's cursor sets could not be read ({err}); the \
                     pointer row offers the built-in set alone"
                    ),
                );
                return Vec::new();
            }
        };
        // A name this build would not accept as a set is dropped rather
        // than offered: choosing it would post a document the desktop
        // refuses whole.
        answered
            .names()
            .filter_map(|name| core::str::from_utf8(name).ok())
            .filter_map(CursorSetId::new)
            .collect()
    }

    /// The client half of the panes' pictures: the renders outstanding and
    /// the regions they land in ([`Renders`]), asked for as fast as the
    /// desktop will take them.
    struct Pictures {
        renders: Renders<tairix_rt::shm::SharedRegion>,
    }

    impl Pictures {
        const fn new() -> Self {
            Self {
                renders: Renders::new(),
            }
        }

        /// Ask the desktop for every picture a pane wants that it will take
        /// now, nearest to what is seen first.
        fn request(
            &mut self,
            shell: &mut Shell,
            surface: &mut SettingsWindow,
            (theme, scale): (&Theme, Scale),
            asker: &Asker,
        ) {
            asker.collect_landed(|answer| self.adopt_asked(shell, answer));
            let roomy = tairix_rt::pressure::gauge().band() == PressureBand::Normal;
            let viewport = surface.viewport();
            while self.renders.may_ask() {
                let renders = &self.renders;
                let outstanding = (renders.changes(), |subject| renders.asked(subject));
                let Some(wanted) =
                    shell.next_picture_wanted(viewport, (scale, theme), roomy, outstanding)
                else {
                    return;
                };
                let Some(window_id) = surface.window.window_id() else {
                    return;
                };
                let Some(region) = self
                    .renders
                    .region(wanted.bytes(), tairix_rt::shm::SharedRegion::create)
                else {
                    app::report(
                        APP_NAME,
                        "no shared region for a picture; it keeps its placeholder",
                    );
                    shell.mark_picture_refused(wanted.subject);
                    return;
                };
                let grant =
                    tairix_rt::shm_grant(region.id(), tairix_abi::window_ipc::WINDOW_ENDPOINT);
                let Some(grant) = u64::try_from(grant).ok().filter(|grant| *grant >= 1) else {
                    app::report(
                        APP_NAME,
                        "a picture's region could not be granted; it keeps its \
                         placeholder",
                    );
                    self.renders.unused(region);
                    shell.mark_picture_refused(wanted.subject);
                    return;
                };
                self.renders.submitted(wanted, region);
                let ask = PreviewAsk {
                    window_id,
                    grant,
                    wanted,
                };
                match asker.submit(ask) {
                    // No worker: it was asked on this thread, so its answer is
                    // already here.
                    Ok(true) => asker.collect_landed(|answer| self.adopt_asked(shell, answer)),
                    Ok(false) => {}
                    // No room on the desk: asked for again on a later turn.
                    Err(ask) => {
                        self.renders.withdraw(ask.wanted);
                        return;
                    }
                }
            }
        }

        /// Adopt the desktop's answer to a render this window asked for.
        fn adopt_asked(&mut self, shell: &mut Shell, (ask, answer): Asked) {
            let Err(err) = answer else {
                self.renders.confirmed();
                return;
            };
            if self.renders.declined(ask.wanted, err) {
                app::report(
                    APP_NAME,
                    format_args!("the desktop refused a picture ({err}); it keeps its placeholder"),
                );
                shell.mark_picture_refused(ask.wanted.subject);
            }
        }

        /// Adopt the conclusion of a render, reporting the picture it changed.
        ///
        /// An answer for a render this program is not waiting on, or of
        /// another subject or size, is dropped: a picture keeps waiting rather
        /// than drawing pixels of the wrong shape.
        fn settle(
            &mut self,
            shell: &mut Shell,
            (subject, width, height, outcome): (PreviewSubject, u16, u16, PreviewOutcome),
            (viewport, scale, theme): (Rect, Scale, &Theme),
            damage: &mut Region,
        ) {
            self.renders
                .concluded((subject, width, height), |wanted, region| match outcome {
                    PreviewOutcome::Rendered => {
                        shell.set_picture(
                            wanted,
                            region.bytes_mut(),
                            (viewport, scale, theme),
                            damage,
                        );
                        // One picture fitting is the sign memory freed.
                        shell.retry_unavailable_pictures();
                    }
                    PreviewOutcome::Refused => shell.mark_picture_refused(subject),
                    PreviewOutcome::Unavailable => shell.mark_picture_unavailable(subject),
                });
        }

        /// Memory pressure moved: let go at once of the pictures its band no
        /// longer keeps, and of the regions no render is using.
        fn trim(
            &mut self,
            shell: &mut Shell,
            surface: &SettingsWindow,
            theme: &Theme,
            scale: Scale,
        ) {
            let roomy = tairix_rt::pressure::gauge().band() == PressureBand::Normal;
            shell.trim_pictures(surface.viewport(), (scale, theme), roomy);
            self.renders.trim();
        }

        /// The desktop moved: every render outstanding was asked for at a size
        /// that may no longer be drawn, so its answer is waited for and let go.
        fn restart(&mut self) {
            self.renders.restart();
        }
    }

    /// The name this program states its refusals under.
    const APP_NAME: &str = "settings";

    /// The production [`EventSource`]: drain the app's own event mailbox,
    /// parking on the wait-set whenever it is empty.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        /// The desks whose wakes the park drains.
        workers: &'a Workers,
        /// Set when the park woke for a desktop change, cleared when the loop
        /// adopts it.
        desktop_moved: &'a Cell<bool>,
        /// Set when the memory-pressure band moved, cleared when the loop has
        /// trimmed the window's icon cache to it.
        pressure_moved: &'a Cell<bool>,
        /// When a password marker on show next moves its dots, written by the
        /// loop just before it parks. `None` arms no timer at all.
        secret_due: &'a Cell<Option<u64>>,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let woken = match self.secret_due.get() {
                Some(due) => match app::park_until(self.set, due)? {
                    Some(woken) => woken,
                    None => return Ok(Parked::Interrupted),
                },
                None => app::park(self.set)?,
            };
            match woken {
                // A desk answered. Draining is the whole of noticing it, and
                // the answer is the loop's to adopt, so the wait ends here
                // rather than parking again on a ready source.
                Wake::App(token) if self.workers.drain(token) => Ok(Parked::Interrupted),
                Wake::PressureChanged => {
                    tairix_font::trim_glyph_cache();
                    self.pressure_moved.set(true);
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

    /// The app's channel to the desktop, the window it may or may not have
    /// open, and the client extent that window is currently showing.
    struct SettingsWindow {
        window: AppWindow,
        mode: DisplayMode,
        /// The title the session is carrying for the window.
        title: &'static str,
        /// The built-in pictures the shell draws — the categories' badges —
        /// rasterised once per side rather than once per frame.
        artwork: ArtworkCache,
    }

    impl SettingsWindow {
        /// Open a window at the current mode and present the shell's first
        /// frame, answering the session's [`ProcId`] or the reserved exit code
        /// for the refusal.
        fn open(
            &mut self,
            event_endpoint: u64,
            shell: &Shell,
            themes: &ThemeRegistry,
            scale: Scale,
        ) -> Result<ProcId, i32> {
            self.title = shell.title();
            let server = self
                .window
                .open(event_endpoint, &self.mode, self.title, win_sizing(scale))
                .map_err(|err| app::fail(APP_NAME, err.code(), err))?;
            // Before the first present, so no frame is shown unfrosted.
            self.apply_backdrop(themes);
            if self
                .present(shell, themes, scale, DamageRect::full(&self.mode))
                .is_err()
            {
                self.close();
                return Err(app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused"));
            }
            Ok(server)
        }

        /// Close the open window, leaving the app on the icon bar.
        fn close(&mut self) {
            let _ = self.window.close();
        }

        /// Ask the compositor for the blur the window's ground is drawn over.
        /// A refusal is stated, and the window keeps the blur it had.
        fn apply_backdrop(&mut self, themes: &ThemeRegistry) {
            let blur = themes.active_on(WINDOW_GROUND).backdrop_blur();
            if let Err(err) = self.window.set_backdrop_blur(blur) {
                app::report(APP_NAME, format_args!("backdrop blur refused: {err}"));
            }
        }

        /// The client rectangle the window is showing.
        fn viewport(&self) -> Rect {
            Rect::new(0, 0, self.mode.width_px, self.mode.height_px)
        }

        /// Draw the shell and present `damage`, then retitle the window if
        /// the pane on show has changed.
        ///
        /// The retitle follows the present so the title bar never names a pane
        /// the frame beneath it does not show yet. A refused retitle keeps the
        /// remembered title, so the next present asks again.
        fn present(
            &mut self,
            shell: &Shell,
            themes: &ThemeRegistry,
            scale: Scale,
            damage: DamageRect,
        ) -> Result<(), Errno> {
            let viewport = self.viewport();
            let artwork = &mut self.artwork;
            // Nothing this window draws is read from a file: every picture is
            // compiled in, so the resolver refuses every asset tier and the
            // cache answers from its built-in one.
            let mut resolver = InlineArtwork::new(NoArtworkSeam, NoArtworkSeam);
            self.window.present(damage, |surface| {
                shell.render(
                    surface,
                    viewport,
                    scale,
                    themes.grounds(WINDOW_GROUND),
                    &mut IconArtworkSource::new(artwork, &mut resolver),
                );
            })?;
            let wanted = shell.title();
            if wanted != self.title {
                if let Some(id) = self.window.window_id() {
                    if self.window.client().set_title(id, wanted).is_ok() {
                        self.title = wanted;
                    }
                }
            }
            Ok(())
        }
    }

    /// What one delivered event concluded.
    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Acted {
        /// Nothing on screen changed.
        Idle,
        /// The shell changed and must be re-presented.
        Changed,
        /// The whole client changed and no report could describe it.
        Whole,
        /// The reader chose a setting: ask the desktop to adopt it, then
        /// re-read what it holds.
        Apply(String),
        /// The reader offered an account: ask the console's broker to run
        /// what it authorises, then adopt the verdict.
        Elevate(Elevation),
        /// The desktop queued a target: drain the queue and show what it
        /// named.
        Opened,
        /// A picture a pane asked for is in the shared region, or was refused.
        Rendered {
            /// What was asked for.
            subject: PreviewSubject,
            /// The width it was rendered at.
            width: u16,
            /// The height it was rendered at.
            height: u16,
            /// Whether the region holds the picture, and if not, why not.
            outcome: PreviewOutcome,
        },
        /// The reader asked for the screen to be locked: ask the desktop.
        LockScreen,
        /// The reader asked to see the screensaver: ask the desktop to show
        /// this document's.
        PreviewScreensaver(String),
        /// End the program.
        Quit,
    }

    /// Apply one delivered event to the shell.
    fn apply_event(
        surface: &mut SettingsWindow,
        shell: &mut Shell,
        theme: &Theme,
        scale: Scale,
        event: &WindowEvent,
        damage: &mut Region,
    ) -> Acted {
        let viewport = surface.viewport();
        let concluded = |outcome: ShellOutcome| match outcome {
            ShellOutcome::Idle => Acted::Idle,
            ShellOutcome::Changed => Acted::Changed,
            ShellOutcome::Apply(document) => Acted::Apply(document),
            ShellOutcome::Elevate(asked) => Acted::Elevate(asked),
            ShellOutcome::LockScreen => Acted::LockScreen,
            ShellOutcome::PreviewScreensaver(document) => Acted::PreviewScreensaver(document),
        };
        match event {
            WindowEvent::CloseRequested { .. } => Acted::Quit,
            // The desktop resized the client: re-map the frame region to the
            // new extent, then lay the shell out to it.
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => {
                let mode = app::mode_for(*width_px, *height_px);
                if surface.window.resize(mode) {
                    surface.mode = mode;
                } else {
                    // A refused resize leaves the old geometry standing and
                    // still drawable, so the window keeps the size it had.
                    app::report(
                        APP_NAME,
                        "the desktop refused a resize; the window keeps its size",
                    );
                }
                // The reported client extent is what the shell lays out to
                // either way, so the whole window is redrawn regardless.
                shell.lay_out(surface.viewport(), scale, theme);
                Acted::Whole
            }
            WindowEvent::Key {
                key: pressed @ KeyInput::Pressed { .. },
                ..
            } => Keystroke::pressed(key_input_event(*pressed), tairix_rt::clock_get())
                .map_or(Acted::Idle, |stroke| {
                    concluded(shell.on_key(stroke, viewport, scale, theme, damage))
                }),
            WindowEvent::Pointer { x, y, action, .. } => concluded(apply_pointer(
                shell,
                pointer_input_events(*action, pointer_point(*x, *y)),
                viewport,
                scale,
                theme,
                damage,
            )),
            WindowEvent::Scrolled { x, y, dx, dy, .. } => concluded(apply_pointer(
                shell,
                scroll_input_events(pointer_point(*x, *y), *dx, *dy),
                viewport,
                scale,
                theme,
                damage,
            )),
            // The desktop queued at least one target for this instance.
            WindowEvent::OpenRequested => Acted::Opened,
            // A picture a pane asked for. Answered by the loop, which holds
            // the region it was rendered into.
            WindowEvent::PreviewRendered {
                subject,
                width,
                height,
                outcome,
                ..
            } => Acted::Rendered {
                subject: *subject,
                width: *width,
                height: *height,
                outcome: *outcome,
            },
            // A redraw needs nothing here: the client library re-presents the
            // last frame and the shell it drew has not changed. The rest are
            // events this surface does not act on — it opens no chain of the
            // desktop's, declares no file association, and owns no desktop
            // layer — and `ContentReleased` is the caller's, which owns the
            // region it lets go of.
            // Settings is part of the desktop rather than an application
            // the user manages: its signed manifest presents no icon-bar
            // slot and it declares none, so neither icon-bar event can
            // reach it. A secondary press on Close asks to leave what the
            // window shows, and this window shows only itself.
            WindowEvent::AlternateCloseRequested { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::MenuClosed { .. }
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::Pinch { .. }
            | WindowEvent::Key { .. }
            | WindowEvent::Focus { .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::RedrawRequested { .. }
            | WindowEvent::ContentReleased { .. }
            | WindowEvent::FilePicked { .. }
            | WindowEvent::PickCancelled { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. } => Acted::Idle,
        }
    }

    /// Drain every target the desktop queued for this instance and show the
    /// last pane it named, answering whether the window moved.
    ///
    /// One event may cover several targets, and another may arrive while
    /// this drain is running, so it loops until the queue answers empty. A
    /// target that is not a pane is not one this application can act on —
    /// it declares no file association and holds no filesystem capability —
    /// and is stated rather than silently dropped. A pane it does not carry
    /// leaves the window where it is, which is the whole point of a target
    /// that confers nothing.
    fn drain_open_targets(
        shell: &mut Shell,
        surface: &mut SettingsWindow,
        theme: &Theme,
        scale: Scale,
    ) -> bool {
        let viewport = surface.viewport();
        let mut moved = false;
        loop {
            match surface.window.client().take_open_target() {
                Ok(Some(Target::Pane(pane))) => {
                    let mut sink = tairix_controls::damage::sink();
                    if shell.go_to_pane(&pane, viewport, scale, theme, &mut sink) {
                        moved = true;
                    } else {
                        app::report(
                            APP_NAME,
                            format_args!("there is no `{pane}` here; the window stays where it is"),
                        );
                    }
                }
                Ok(Some(Target::Path(path))) => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{path} was handed over, but this window shows settings, not \
                         files"
                        ),
                    );
                }
                Ok(Some(Target::Document { name, .. })) => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{name} was handed over as a document, which this window has \
                         nowhere to show"
                        ),
                    );
                }
                Ok(None) => return moved,
                Err(err) => {
                    app::report(APP_NAME, format_args!("cannot take an open target: {err}"));
                    return moved;
                }
            }
        }
    }

    /// Route one wire pointer event: a move to `at` to sync the pointer, then
    /// the press or release the action names.
    ///
    /// A press and its release are two inputs of one gesture, so the
    /// stronger of what they concluded is the gesture's: an apply the
    /// release asked for is not lost behind the press's bare repaint.
    fn apply_pointer(
        shell: &mut Shell,
        inputs: impl Iterator<Item = InputEvent>,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        let mut concluded = ShellOutcome::Idle;
        for input in inputs {
            let acted = shell.on_pointer(&input, viewport, scale, theme, damage);
            // A conclusion the caller must act on outranks a repaint: a
            // press and its release are two events, and the one that asked
            // for something must not be lost to the one that did not.
            let asks = |outcome: &ShellOutcome| {
                matches!(
                    outcome,
                    ShellOutcome::Apply(_)
                        | ShellOutcome::Elevate(_)
                        | ShellOutcome::LockScreen
                        | ShellOutcome::PreviewScreensaver(_)
                )
            };
            let (asked, held) = (asks(&acted), asks(&concluded));
            if asked || (acted.changed() && !held) {
                concluded = acted;
            }
        }
        concluded
    }

    /// Adopt the desktop the session published, if the park said it moved.
    fn adopt_desktop(
        desktop: &mut Desktop,
        themes: &mut ThemeRegistry,
        moved: &Cell<bool>,
    ) -> bool {
        if !moved.replace(false) {
            return false;
        }
        match app::adopt_desktop(desktop, themes) {
            Ok(changed) => changed,
            Err(err) => {
                app::report(APP_NAME, format_args!("desktop change refused: {err}"));
                false
            }
        }
    }

    /// Present the whole client, answering whether the session took it.
    fn present_whole(
        surface: &mut SettingsWindow,
        shell: &Shell,
        themes: &ThemeRegistry,
        desktop: &Desktop,
    ) -> bool {
        let whole = DamageRect::full(&surface.mode);
        surface
            .present(shell, themes, desktop.scale(), whole)
            .is_ok()
    }

    /// Adopt a desktop change the park reported, answering whether the
    /// window is still presentable.
    ///
    /// A new density or theme re-measures every length the layout is
    /// derived from, and stales every rendered picture — a picture is
    /// square at one side only.
    fn adopt_desktop_round(
        surface: &mut SettingsWindow,
        shell: &mut Shell,
        themes: &mut ThemeRegistry,
        desktop: &mut Desktop,
        moved: &Cell<bool>,
        pictures: &mut Pictures,
    ) -> bool {
        if !adopt_desktop(desktop, themes, moved) {
            return true;
        }
        // A theme switch can move the blur the ground asks for.
        surface.apply_backdrop(themes);
        shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
        pictures.restart();
        present_whole(surface, shell, themes, desktop)
    }

    /// The long-lived state one turn of the event loop reads and writes.
    ///
    /// Grouped because every path through the loop needs all of it: the
    /// window it presents to, the desktop and theme it draws with, the
    /// shell it routes into, and the channels it answers on.
    struct Session<'a> {
        surface: &'a mut SettingsWindow,
        desktop: &'a mut Desktop,
        themes: &'a mut ThemeRegistry,
        shell: &'a mut Shell,
        desktop_moved: &'a Cell<bool>,
        pressure_moved: &'a Cell<bool>,
        secret_due: &'a Cell<Option<u64>>,
        pictures: &'a mut Pictures,
        desks: Desks<'a>,
    }

    /// The worker desks the loop submits to and collects from.
    ///
    /// One group, because every path through the loop reaches all of them
    /// and each is the same shape: submit, carry on drawing, adopt the
    /// answer on the wake it nudges.
    struct Desks<'a> {
        /// The desktop apply an immediate row posts.
        applier: &'a Applier,
        /// The mount-table walk the Storage pane lists.
        mounts: MountWalk<'a>,
        /// The machine's configuration and facts the General panes state.
        machine: MachineRead<'a>,
        /// The network readings the DNS pane states.
        network: NetworkRead<'a>,
        /// The account readings the Users pane states, and the salt a new
        /// password is hashed under.
        accounts: AccountRead<'a>,
        /// The broker round trip an offered account costs.
        elevator: &'a Elevator,
        /// The render requests the panes' pictures cost.
        asker: &'a Asker,
        /// What only the desktop answers.
        desktop: DesktopDesk<'a>,
    }

    impl<'a> Desks<'a> {
        /// The loop's view of `workers`, with no reading yet asked for.
        fn of(workers: &'a Workers) -> Self {
            Self {
                applier: &workers.applier,
                mounts: MountWalk {
                    worker: &workers.mounts,
                    pending: false,
                },
                machine: MachineRead {
                    worker: &workers.machine,
                    pending: false,
                },
                network: NetworkRead {
                    worker: &workers.network,
                    pending: false,
                },
                accounts: AccountRead {
                    worker: &workers.accounts,
                    pending: false,
                },
                elevator: &workers.elevator,
                asker: &workers.asker,
                desktop: DesktopDesk {
                    worker: &workers.desktop,
                    asks: DesktopAsks::new(),
                },
            }
        }
    }

    /// Adopt whatever a worker answered while the loop was parked, and
    /// redraw if anything did, answering whether the window is still
    /// presentable.
    ///
    /// An answer lands a new set of rows, so the pane is redrawn rather than
    /// the one row a choice reported.
    fn adopt_answers(
        surface: &mut SettingsWindow,
        shell: &mut Shell,
        themes: &ThemeRegistry,
        desktop: &Desktop,
        desks: &mut Desks<'_>,
    ) -> bool {
        let mut landed = false;
        if let Some(applied) = desks.applier.collect() {
            adopt_apply(shell, applied);
            landed = true;
        }
        landed |= desks.mounts.settle(shell);
        landed |= desks.machine.settle(shell);
        landed |= desks.network.settle(shell);
        landed |= desks.accounts.settle(shell);
        if let Some(verdict) = desks.elevator.collect() {
            shell.adopt_elevation(verdict);
            landed = true;
        }
        landed |= desks.desktop.settle(shell);
        if !landed {
            return true;
        }
        let viewport = surface.viewport();
        shell.lay_out(viewport, desktop.scale(), themes.active());
        let mut damage = tairix_controls::damage::sink();
        damage.add(shell.pane_region(viewport, desktop.scale(), themes.active()));
        let Some(area) = present_damage(&surface.mode, Repaint::Reported, &damage) else {
            return true;
        };
        surface
            .present(shell, themes, desktop.scale(), area)
            .is_ok()
    }

    /// Step every password marker whose dots are due and present what moved,
    /// answering whether the window is still presentable.
    fn advance_secrets(
        surface: &mut SettingsWindow,
        shell: &mut Shell,
        themes: &ThemeRegistry,
        desktop: &Desktop,
    ) -> bool {
        let Some(due) = shell.secret_deadline_ns() else {
            return true;
        };
        let now_ns = tairix_rt::clock_get();
        if due > now_ns {
            return true;
        }
        let mut damage = tairix_controls::damage::sink();
        let place = (desktop.scale(), themes.active());
        shell.advance_secrets(now_ns, surface.viewport(), place, &mut damage);
        if damage.is_empty() {
            return true;
        }
        let Some(area) = present_damage(&surface.mode, Repaint::Reported, &damage) else {
            return true;
        };
        surface
            .present(shell, themes, desktop.scale(), area)
            .is_ok()
    }

    /// What carrying out an event's conclusion changed beyond what the event
    /// itself reported.
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Carried {
        /// Nothing more.
        Nothing,
        /// An answer landed at once and was adopted into the pane.
        Answered,
        /// The window moved to another pane.
        Moved,
        /// The program is to end.
        Quit,
    }

    impl Carried {
        /// `Answered` where an answer `landed`, else `Nothing`.
        const fn answered_if(landed: bool) -> Self {
            if landed {
                Self::Answered
            } else {
                Self::Nothing
            }
        }
    }

    /// Carry out what one event concluded, answering what that changed.
    fn act(
        acted: &Acted,
        (surface, shell): (&mut SettingsWindow, &mut Shell),
        (themes, desktop): (&ThemeRegistry, &Desktop),
        (desks, pictures): (&mut Desks<'_>, &mut Pictures),
        damage: &mut Region,
    ) -> Carried {
        let carried = match acted {
            Acted::Quit => {
                surface.close();
                return Carried::Quit;
            }
            Acted::Elevate(asked) => {
                // Submitted, not awaited: the broker answers only once the
                // program it started has exited, and a window that waited
                // would stop drawing for the whole of it.
                let verdict = submit_elevate(desks.elevator, asked.clone());
                let landed = verdict.is_some();
                if let Some(verdict) = verdict {
                    shell.adopt_elevation(verdict);
                }
                Carried::answered_if(landed)
            }
            Acted::Apply(document) => {
                // Submitted, not awaited: the answer arrives on the wake
                // the worker nudges. With no worker to serve it the call
                // was made here and its answer is already in hand.
                let applied = submit_apply(desks.applier, document);
                let landed = applied.is_some();
                if let Some(applied) = applied {
                    adopt_apply(shell, applied);
                }
                Carried::answered_if(landed)
            }
            Acted::LockScreen => {
                // Submitted, not awaited: the desktop puts its own lock up
                // from its own loop, and its answer arrives on the wake the
                // worker nudges.
                Carried::answered_if(desks.desktop.submit(shell, &DesktopAsk::Lock))
            }
            Acted::Opened => {
                if drain_open_targets(shell, surface, themes.active(), desktop.scale()) {
                    Carried::Moved
                } else {
                    Carried::Nothing
                }
            }
            Acted::PreviewScreensaver(document) => {
                // Submitted, not awaited, as the lock is; a document the wire
                // cannot carry is refused here, in memory.
                let landed = match PinboardDocument::new(document) {
                    Ok(document) => desks.desktop.submit(shell, &DesktopAsk::Preview(document)),
                    Err(err) => {
                        adopt_desktop_answer(shell, DesktopAnswer::Preview(Err(err)));
                        true
                    }
                };
                Carried::answered_if(landed)
            }
            Acted::Rendered {
                subject,
                width,
                height,
                outcome,
            } => {
                let viewport = surface.viewport();
                pictures.settle(
                    shell,
                    (*subject, *width, *height, *outcome),
                    (viewport, desktop.scale(), themes.active()),
                    damage,
                );
                Carried::Nothing
            }
            Acted::Idle | Acted::Changed | Acted::Whole => Carried::Nothing,
        };
        if carried != Carried::Nothing {
            shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
        }
        carried
    }

    /// Ask for every reading a pane this round put on show states, answering
    /// whether any landed at once — as one does with no worker to serve it,
    /// the read made here and the pane already holding its answer.
    ///
    /// Every desk is asked whatever the others answered, and a reading that
    /// landed rebuilt its pane's rows, so any one of them redraws the pane.
    fn request_readings(shell: &mut Shell, desks: &mut Desks<'_>) -> bool {
        let landed = [
            desks.mounts.request(shell),
            desks.machine.request(shell),
            desks.network.request(shell),
            desks.accounts.request(shell),
            desks.desktop.request_sources(shell),
        ];
        landed.contains(&true)
    }

    /// Bring the window up to date with what happened while it was parked —
    /// a pressure move, a desk's answer, a password marker falling due — and
    /// ask for the pictures the panes now want, answering whether the window
    /// is still presentable.
    fn catch_up(
        (surface, shell): (&mut SettingsWindow, &mut Shell),
        (themes, desktop): (&ThemeRegistry, &Desktop),
        (pictures, desks): (&mut Pictures, &mut Desks<'_>),
        pressure_moved: &Cell<bool>,
    ) -> bool {
        // Memory goes back when the machine asks for it, not at whatever later
        // frame happens to draw an icon.
        if pressure_moved.take() {
            surface.artwork.trim();
            pictures.trim(shell, surface, themes.active(), desktop.scale());
            shell.retry_unavailable_pictures();
        }
        if !adopt_answers(surface, shell, themes, desktop, desks)
            || !advance_secrets(surface, shell, themes, desktop)
        {
            return false;
        }
        // Every turn, not only an event's: a navigation, a resize and an
        // answered request all move what the panes wait for.
        pictures.request(
            shell,
            surface,
            (themes.active(), desktop.scale()),
            desks.asker,
        );
        true
    }

    /// The event loop: park, apply, repaint.
    fn run_event_loop(session: Session<'_>, mut events: WindowEvents<RtEventSource<'_>>) -> i32 {
        let Session {
            surface,
            desktop,
            themes,
            shell,
            desktop_moved,
            pressure_moved,
            secret_due,
            pictures,
            mut desks,
        } = session;
        loop {
            if !catch_up(
                (surface, shell),
                (themes, desktop),
                (pictures, &mut desks),
                pressure_moved,
            ) {
                return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
            }
            secret_due.set(shell.secret_deadline_ns());
            let event = match events.wait(surface.window.client()) {
                Ok(Some(event)) => event,
                // A wait that ended without an event is the desktop notice; a
                // malformed frame from the authenticated session is refused
                // rather than guessed at. Either way the round is the
                // re-theme alone.
                Ok(None) | Err(EventError::Undecodable(_)) => {
                    if !adopt_desktop_round(
                        surface,
                        shell,
                        themes,
                        desktop,
                        desktop_moved,
                        pictures,
                    ) {
                        return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
                    }
                    continue;
                }
                Err(EventError::Mailbox(_)) => {
                    return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "event channel lost")
                }
            };

            let redraw = adopt_desktop(desktop, themes, desktop_moved);
            if redraw {
                shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
                pictures.restart();
            }
            let mut damage = tairix_controls::damage::sink();
            let acted = apply_event(
                surface,
                shell,
                themes.active(),
                desktop.scale(),
                &event,
                &mut damage,
            );
            let carried = act(
                &acted,
                (surface, shell),
                (themes, desktop),
                (&mut desks, pictures),
                &mut damage,
            );
            if carried == Carried::Quit {
                return 0;
            }
            let landed = request_readings(shell, &mut desks);
            if landed {
                shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
            }
            if matches!(event, WindowEvent::ContentReleased { .. }) {
                surface.window.release_frames();
                continue;
            }
            // An answer adopted at once rebuilt the pane's rows, which report
            // nothing of their own; another pane moves the trail and the strip
            // too.
            if landed || carried == Carried::Answered {
                damage.add(shell.pane_region(surface.viewport(), desktop.scale(), themes.active()));
            }
            let repaint = if redraw || carried == Carried::Moved || matches!(acted, Acted::Whole) {
                Repaint::Whole
            } else {
                // A round that changed something but reported no rectangle
                // presents the whole window rather than leave a stale frame.
                Repaint::reported_if(!matches!(acted, Acted::Idle) || !damage.is_empty())
            };
            let Some(area) = present_damage(&surface.mode, repaint, &damage) else {
                continue;
            };
            if surface
                .present(shell, themes, desktop.scale(), area)
                .is_err()
            {
                return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
            }
        }
    }

    /// Read everything the window's first frame needs, before it opens.
    ///
    /// Each of these is answered from memory or from one ungated round
    /// trip, so taking them here costs a moment at start-up and spares the
    /// reader a first frame that states nothing it could have stated.
    fn seat_first_frame(
        shell: &mut Shell,
        surface: &mut SettingsWindow,
        desktop: &Desktop,
        themes: &ThemeRegistry,
    ) {
        // The session lists the read-only picture store once at its own
        // bring-up and answers this from memory.
        shell.adopt_catalog(fetch_catalog(surface.window.client()));
        // And the pointer rows their choice space, for the same reason.
        shell.adopt_cursor_sets(fetch_cursor_sets(surface.window.client()));
        // A pane the launch named, if it named one: a fresh process is
        // given it as its one operand, exactly as a running instance is
        // handed it over the channel. `args` has already dropped the
        // program's own name, so the operand is the first of them.
        if let Some(argv) = tairix_rt::args().filter(|argv| !argv.is_empty()) {
            let mut sink = tairix_controls::damage::sink();
            let viewport = surface.viewport();
            let named = Pane::launched(&argv).is_some_and(|pane| {
                shell.go_to(pane, viewport, desktop.scale(), themes.active(), &mut sink)
            });
            if !named {
                app::report(
                    APP_NAME,
                    "that is not a pane here; the window opens where it always does",
                );
            }
        }
        // After the launch target, so a window opened *at* the storage pane
        // has its volumes on its first frame rather than on a later wake.
        // The pane asks for them again each time it comes on show, because
        // unlike the picture store the mount table moves.
        shell.adopt_volumes(read_mounts(&mut (), &mut ()));
        // And the machine's own readings, for the same reason: the window
        // opens on General, whose panes state them.
        let (config, facts) = read_machine(&mut (), &mut ());
        shell.adopt_config(config);
        shell.adopt_machine(facts);
        // And the sources that have notified, should the launch have named
        // the pane that lists them.
        if shell.notify_sources_wanted() {
            let answer = ask_desktop(surface.window.client(), &mut DesktopAsk::NotifySources);
            adopt_desktop_answer(shell, answer);
        }
        shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
    }

    /// Every worker desk this window runs, started and owned together.
    ///
    /// One desk per kind of work rather than one shared desk: the eight
    /// carry different jobs and a latest-wins desk would let any of them
    /// evict another's answer. Each would otherwise stall the window for a
    /// round trip.
    struct Workers {
        applier: Arc<Applier>,
        mounts: Arc<Mounts>,
        machine: Arc<Machine>,
        network: Arc<Network>,
        accounts: Arc<Accounts>,
        elevator: Arc<Elevator>,
        asker: Arc<Asker>,
        desktop: Arc<DesktopAsker>,
    }

    impl Workers {
        /// Start every desk. A machine that grants no thread runs the work
        /// on the event loop instead, which each start states for itself.
        ///
        /// # Errors
        ///
        /// The exit code of the stated failure when the asker's queue could
        /// not be held.
        fn started() -> Result<Self, i32> {
            // One render in flight at a time: the desktop takes one per window.
            let Ok(asker) = Asker::queued(
                ask_preview,
                tairix_window::WindowClient::new(app::RtWindowTransport),
                tairix_rt::sync::WorkerWake::create(),
                1,
            ) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the preview queue",
                ));
            };
            let Ok(desktop) = DesktopAsker::queued(
                ask_desktop,
                tairix_window::WindowClient::new(app::RtWindowTransport),
                tairix_rt::sync::WorkerWake::create(),
                MOST_OUTSTANDING,
            ) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the desktop's requests",
                ));
            };
            Ok(Self {
                applier: started(
                    Applier::new(send_apply, (), tairix_rt::sync::WorkerWake::create()),
                    "apply",
                ),
                mounts: started(
                    Mounts::new(read_mounts, (), tairix_rt::sync::WorkerWake::create()),
                    "mount-table",
                ),
                machine: started(
                    Machine::new(read_machine, (), tairix_rt::sync::WorkerWake::create()),
                    "machine-readings",
                ),
                network: started(
                    Network::new(read_network, (), tairix_rt::sync::WorkerWake::create()),
                    "network-readings",
                ),
                accounts: started(
                    Accounts::new(read_accounts, (), tairix_rt::sync::WorkerWake::create()),
                    "account-readings",
                ),
                elevator: started(
                    Elevator::new(send_elevate, (), tairix_rt::sync::WorkerWake::create()),
                    "elevated-run",
                ),
                asker: started(asker, "preview-request"),
                desktop: started(desktop, "desktop-request"),
            })
        }

        /// Each desk's wake, the token it is watched under, and how a refused
        /// watch is stated.
        fn wakes(&self) -> [(&tairix_rt::sync::WorkerWake, u64, &'static str); 8] {
            [
                (self.applier.wake(), APPLY_TOKEN, "apply wake refused"),
                (self.mounts.wake(), MOUNTS_TOKEN, "mount-table wake refused"),
                (
                    self.machine.wake(),
                    MACHINE_TOKEN,
                    "machine-readings wake refused",
                ),
                (
                    self.network.wake(),
                    NETWORK_TOKEN,
                    "network-readings wake refused",
                ),
                (
                    self.accounts.wake(),
                    ACCOUNTS_TOKEN,
                    "account-readings wake refused",
                ),
                (
                    self.elevator.wake(),
                    ELEVATE_TOKEN,
                    "elevated-run wake refused",
                ),
                (self.asker.wake(), ASK_TOKEN, "preview-request wake refused"),
                (
                    self.desktop.wake(),
                    DESKTOP_TOKEN,
                    "desktop-request wake refused",
                ),
            ]
        }

        /// Put every desk's wake on `set`, so a landed answer ends the park
        /// the loop is already in.
        ///
        /// A refused add is fatal rather than tolerated: an answer nobody
        /// collects would leave every row showing a value the desktop may
        /// never have adopted, or a storage pane waiting for ever on a table
        /// that has already landed.
        fn watched(&self, set: u64) -> Result<(), i32> {
            for (wake, token, refusal) in self.wakes() {
                if let Err(err) = app::watch_wake(set, wake, token) {
                    return Err(app::fail(
                        APP_NAME,
                        app::EXIT_NO_EVENTS,
                        format_args!("{refusal} ({err})"),
                    ));
                }
            }
            Ok(())
        }

        /// Drain the wake watched under `token`, answering whether it is one
        /// of these desks'.
        ///
        /// A wake's readiness is a level peek, so one left undrained would
        /// report ready for ever and turn the park into a spin.
        fn drain(&self, token: u64) -> bool {
            let Some((wake, ..)) = self
                .wakes()
                .into_iter()
                .find(|&(_, watched, _)| watched == token)
            else {
                return false;
            };
            wake.drain();
            true
        }
    }

    impl Drop for Workers {
        /// Ask every desk to leave. What the per-worker guards did
        /// individually, in the one place that owns them all.
        fn drop(&mut self) {
            self.applier.stop();
            self.mounts.stop();
            self.machine.stop();
            self.network.stop();
            self.accounts.stop();
            self.elevator.stop();
            self.asker.stop();
            self.desktop.stop();
        }
    }

    /// Put `worker` on a desk of its own and start it.
    fn started<S, Req, Ans, D>(
        worker: tairix_rt::work::Worker<S, Req, Ans, D>,
        what: &str,
    ) -> Arc<tairix_rt::work::Worker<S, Req, Ans, D>>
    where
        S: Send + 'static,
        Req: Send + 'static,
        Ans: Send + 'static,
        D: tairix_rt::work::Desk<Req, Ans> + Send + 'static,
    {
        let worker = Arc::new(worker);
        start_worker(&worker, what);
        worker
    }

    /// Start a worker, stating in this program's own words why a machine
    /// that grants none will do its work on the event loop instead.
    ///
    /// Not a failure: a program with no thread is exactly as correct and
    /// only as responsive as it was before there was a worker at all.
    fn start_worker<S, Req, Ans, D>(
        worker: &Arc<tairix_rt::work::Worker<S, Req, Ans, D>>,
        what: &str,
    ) where
        S: Send + 'static,
        Req: Send + 'static,
        Ans: Send + 'static,
        D: tairix_rt::work::Desk<Req, Ans> + Send + 'static,
    {
        if let Err(reason) = tairix_rt::work::Worker::start(worker) {
            app::report(
                APP_NAME,
                format_args!("no {what} worker ({reason:?}); it is done on the event loop"),
            );
        }
    }

    /// The window's icon cache, budgeted from the `frame_bytes` of the
    /// window it draws into, and registered with the process's cache report.
    fn icon_cache(frame_bytes: usize) -> ArtworkCache {
        // The reclaim bookkeeping's audit sink. The shared constructor takes
        // a `'static` borrow, and the runtime sink owns nothing.
        static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;
        let cache = artwork_cache(
            "settings.icon-artwork",
            SEAT_PRIMARY,
            frame_bytes,
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
        );
        if let Some(ledger) = cache.ledger() {
            tairix_rt::cachereport::register(ledger);
        }
        cache
    }

    /// Program entry point.
    fn main() -> i32 {
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);

        let mut window = AppWindow::new();
        let (mut desktop, mut themes) = match app::bring_up_desktop(window.client()) {
            Ok(pair) => pair,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let (initial_w, initial_h) = desktop.window_size(WIN_WIDTH, WIN_HEIGHT);
        let mode = app::mode_for(initial_w, initial_h);
        // A frame region that cannot be sized is a window that can never open,
        // so it is stated here rather than as a refused create later.
        let Some(frame_bytes) = app::region_bytes(&mode, app::FRAME_COUNT) else {
            return app::fail(
                APP_NAME,
                app::EXIT_NO_FRAMES,
                "window frame larger than the address width",
            );
        };
        let mut surface = SettingsWindow {
            window,
            mode,
            title: "",
            artwork: icon_cache(frame_bytes),
        };

        let binding = match app::bind_event_mailbox() {
            Ok(binding) => binding,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let event_endpoint = binding.endpoint();

        // An empty registry would leave the window nothing to show at all, so
        // it ends fail-loud rather than opening a blank frame.
        let Some(mut shell) = Shell::new(settings_in_effect()) else {
            return app::fail(
                APP_NAME,
                EXIT_CHANNEL_LOST,
                "the settings registry holds no categories",
            );
        };
        let workers = match Workers::started() {
            Ok(workers) => workers,
            Err(code) => return code,
        };
        if let Err(code) = workers.watched(binding.set()) {
            return code;
        }

        seat_first_frame(&mut shell, &mut surface, &desktop, &themes);

        let server = match surface.open(event_endpoint, &shell, &themes, desktop.scale()) {
            Ok(server) => server,
            Err(code) => return code,
        };

        let desktop_moved = Cell::new(false);
        let pressure_moved = Cell::new(false);
        let secret_due = Cell::new(None);
        let events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(event_endpoint, server),
            set: binding.set(),
            workers: &workers,
            desktop_moved: &desktop_moved,
            pressure_moved: &pressure_moved,
            secret_due: &secret_due,
        });
        run_event_loop(
            Session {
                surface: &mut surface,
                desktop: &mut desktop,
                themes: &mut themes,
                shell: &mut shell,
                desktop_moved: &desktop_moved,
                pressure_moved: &pressure_moved,
                secret_due: &secret_due,
                pictures: &mut Pictures::new(),
                desks: Desks::of(&workers),
            },
            events,
        )
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
