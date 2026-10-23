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

    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;

    use tairix_abi::driver::audio::StreamDirection;
    use tairix_abi::driver::display::{DamageRect, DisplayMode};
    use tairix_abi::input::KeyInput;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::net_ipc::{NetServerAddr, MAX_RESOLVER_SERVERS};
    use tairix_abi::pinboard_ipc::PinboardDocument;
    use tairix_abi::pinboard_ipc::{
        decode_sound_reading, decode_sound_themes, PinboardRequest, PINBOARD_ENDPOINT,
        PINBOARD_MAX_REPLY, PINBOARD_MAX_REQUEST,
    };
    use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::sysinfo::{SysinfoQueryId, Uptime};
    use tairix_abi::thermal_ipc::{
        decode_status, ThermalRequest, THERMAL_ENDPOINT, THERMAL_MAX_REPLY, THERMAL_REQUEST_LEN,
    };
    use tairix_abi::time::{Time64, NANOS_PER_SEC};
    use tairix_abi::window_ipc::{
        AuthorisedOutcome, DialogModality, PreviewOutcome, PreviewSubject, SystemTask, WindowEvent,
    };
    use tairix_abi::{Errno, ProcId};
    use tairix_abi::{NoticeTopic, WaitSetOp, WaitSourceKind};
    use tairix_appconf::Registry;
    use tairix_appdata::RtHost;
    use tairix_audio::live::{live_captures, RtAudio};
    use tairix_audio::stream::{devices, set_control, ControlQueue, DeviceControl};
    use tairix_controls::zone::{self, Minute};
    use tairix_controls::{Keystroke, PlaceDialog, PlaceOutcome, PressTiming};
    use tairix_geometry::{Rect, Region, Scale};
    use tairix_icon::{
        artwork_cache, ArtworkCache, IconArtworkSource, InlineArtwork, NoArtworkSeam,
    };
    use tairix_input::InputEvent;
    use tairix_procinfo::{for_each_mount, IpcTransport, WalkStep};
    use tairix_raster::Surface;
    use tairix_reclaim::PressureBand;
    use tairix_settings::{
        notified, win_sizing, AccountFacts, Authorised, DesktopAnswer, DesktopAsk, DesktopAsks,
        MachineFacts, OwnAccount, Pane, PictureWanted, Renders, Roster, Round, Shell, ShellOutcome,
        VolumeReading, MOST_OUTSTANDING, WINDOW_GROUND, WIN_HEIGHT, WIN_WIDTH,
    };
    use tairix_settings::{Board, Cooling, SaverEntry, SoundReading};
    use tairix_settings::{EffectsAsk, EffectsAsks, EffectsReading, EffectsTrip};
    use tairix_sysconfig::SystemConfig;
    use tairix_theme::{CursorSetId, Theme, ThemeRegistry};
    use tairix_tz::ZoneInfo;
    use tairix_users::{Salt, SALT_LEN};
    use tairix_wallpaper::{ApplyOutcome, CatalogItem, DesktopSettings, PINBOARD_PUBLISHER};
    use tairix_window::app::{self, AppWindow, Fitted, Wake, WindowPane, EXIT_CHANNEL_LOST};
    use tairix_window::{
        key_input_event, pointer_input_events, pointer_point, present_damage, scroll_input_events,
        Desktop, EventDrain, EventError, EventMailbox, EventSource, Owed, Parked, Repaint, Target,
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

    /// The wait-set token of the network-reading desk's wake: readable
    /// exactly when a fresh reading of the stack's resolver set has landed.
    ///
    /// Its own desk rather than the machine desk's: the two are wanted by
    /// different panes, and folding them together would spend a network
    /// round trip every time the About pane came on show.
    const NETWORK_TOKEN: u64 = app::FIRST_APP_TOKEN + 3;

    /// The wait-set token of the account-reading desk's wake: readable
    /// exactly when a fresh reading of the caller's own record, the two
    /// public directories and a salt has landed.
    ///
    /// Its own desk for the same reason the network one is: it is wanted by
    /// one pane, and folding it into another's would spend three round
    /// trips every time that other pane came on show.
    const ACCOUNTS_TOKEN: u64 = app::FIRST_APP_TOKEN + 4;

    /// The wait-set token of the preview asker's wake: readable exactly when
    /// the desktop has answered a render this window asked for.
    const ASK_TOKEN: u64 = app::FIRST_APP_TOKEN + 5;

    /// The wait-set token of the desktop asker's wake: readable exactly when
    /// the session has answered a lock, a screensaver preview, which sources
    /// have notified, a system task, or a task's output.
    const DESKTOP_TOKEN: u64 = app::FIRST_APP_TOKEN + 6;

    /// The wait-set token of the sound desk's wake: a round trip to the audio
    /// service has come back.
    const SOUND_TOKEN: u64 = app::FIRST_APP_TOKEN + 7;

    /// The wait-set token both audio notices are watched under: a device, a
    /// control or a recording moved, whoever moved it.
    const AUDIO_NOTICE_TOKEN: u64 = app::FIRST_APP_TOKEN + 8;

    /// The wait-set token of the sound-choices desk's wake: a round trip to
    /// the desktop session about the user's sounds has come back.
    const EFFECTS_TOKEN: u64 = app::FIRST_APP_TOKEN + 9;

    /// The wait-set token of the cooling desk's wake: the cooling service's
    /// status has come back.
    const COOLING_TOKEN: u64 = app::FIRST_APP_TOKEN + 10;

    /// How often the Raspberry Pi pane re-reads the cooling service while it
    /// is on show: the temperature moves on its own.
    const COOLING_REFRESH_NS: u64 = 2_000_000_000;

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

    /// The cooling desk: one ungated round trip to the cooling service.
    type CoolingDesk = tairix_rt::work::Worker<(), (), Cooling>;

    /// The cooling desk's body.
    fn read_cooling(_: &mut (), (): &mut ()) -> Cooling {
        let mut request = [0u8; THERMAL_REQUEST_LEN];
        let mut reply = [0u8; THERMAL_MAX_REPLY];
        let status = ThermalRequest::Status
            .encode(&mut request)
            .ok()
            .and_then(|len| tairix_rt::ipc_call(THERMAL_ENDPOINT, &request[..len], &mut reply).ok())
            .and_then(|len| decode_status(reply.get(..len)?).ok());
        status.map_or(Cooling::Unanswered, Cooling::Read)
    }

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

    /// The Sound pane's round trips: apply what is asked, then read the
    /// devices, this user's captures, and how many are recording.
    type SoundDesk =
        tairix_rt::work::Worker<RtAudio, Vec<(u32, DeviceControl)>, Option<SoundReading>>;

    /// The sound desk's body. A refused control is stated and the devices
    /// are read regardless, so the pane shows what the service holds.
    fn serve_sound(
        audio: &mut RtAudio,
        controls: &mut Vec<(u32, DeviceControl)>,
    ) -> Option<SoundReading> {
        for (device_id, control) in controls.drain(..) {
            if let Err(err) = set_control(audio, device_id, control) {
                app::report(
                    APP_NAME,
                    format_args!("a sound control was refused ({err})"),
                );
            }
        }
        let listed = devices(audio, StreamDirection::Playback).and_then(|mut sinks| {
            sinks.append(&mut devices(audio, StreamDirection::Capture)?);
            Ok(sinks)
        });
        let devices = match listed {
            Ok(devices) => devices,
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("the sound devices could not be read ({err}); the pane says so"),
                );
                return None;
            }
        };
        let mut captures = Vec::new();
        if let Err(err) = tairix_procinfo::for_each_audio_stream(
            &IpcTransport,
            tairix_procinfo::StreamScope::Own,
            |stream| {
                if stream.direction == StreamDirection::Capture {
                    captures.push(*stream);
                }
                Ok(tairix_procinfo::WalkStep::Continue)
            },
        ) {
            app::report(
                APP_NAME,
                format_args!("your recordings could not be read ({err:?})"),
            );
        }
        Some(SoundReading {
            devices,
            captures,
            recording: live_captures(),
        })
    }

    /// One round trip to the desktop session about the user's sound choices.
    enum EffectsJob {
        /// Read them, and the themes to choose among.
        Read,
        /// Ask for a change, or for a sound's file to be chosen.
        Ask(EffectsTrip),
    }

    /// What a sound-choices round trip came to.
    enum EffectsDone {
        /// What the session said the choices are, boxed so a queue slot holds
        /// what every answer costs rather than what the largest does.
        Read(Result<alloc::boxed::Box<EffectsReading>, Errno>),
        /// Whether the session made the change.
        Asked(Result<(), Errno>),
    }

    /// Every sound-choices round trip, each answered in turn; a pick waits for
    /// the user, so the queue holds what the reader asks meanwhile.
    type EffectsDesk = tairix_rt::work::Worker<
        (),
        EffectsJob,
        EffectsDone,
        tairix_util::defer::JobQueue<EffectsJob, EffectsDone>,
    >;

    /// Sound-choices round trips one window holds at once: a pick waiting on
    /// the user, and the changes and reads asked meanwhile. A containment
    /// bound, not a capacity: one past it is refused and said.
    const EFFECTS_JOBS: usize = 8;

    fn serve_effects((): &mut (), job: &mut EffectsJob) -> EffectsDone {
        match job {
            EffectsJob::Read => EffectsDone::Read(read_effects().map(alloc::boxed::Box::new)),
            EffectsJob::Ask(ask) => EffectsDone::Asked(ask_effects(ask)),
        }
    }

    /// One call on the desktop session's pinboard rendezvous, answering the
    /// reply's length.
    fn pinboard_call(request: &PinboardRequest, reply: &mut [u8]) -> Result<usize, Errno> {
        let mut frame = [0u8; PINBOARD_MAX_REQUEST];
        let len = request.encode(&mut frame)?;
        tairix_rt::ipc_call(PINBOARD_ENDPOINT, &frame[..len], reply).map_err(Errno::from_syscall)
    }

    /// The user's sound choices as the session says they are in effect, and
    /// every theme it names, page by page.
    fn read_effects() -> Result<EffectsReading, Errno> {
        let mut reply = [0u8; PINBOARD_MAX_REPLY];
        let len = pinboard_call(&PinboardRequest::SoundRead, &mut reply)?;
        let reading = decode_sound_reading(reply.get(..len).ok_or(Errno::LengthOutOfRange)?)?;
        let mut themes = Vec::new();
        let mut page = [0u8; PINBOARD_MAX_REPLY];
        loop {
            let from = u16::try_from(themes.len()).map_err(|_| Errno::LengthOutOfRange)?;
            let len = pinboard_call(&PinboardRequest::SoundThemes { from }, &mut page)?;
            let named = decode_sound_themes(page.get(..len).ok_or(Errno::LengthOutOfRange)?)?;
            themes.extend(named.names().map(alloc::string::String::from));
            if named.is_last() {
                break;
            }
        }
        EffectsReading::of(&reading, themes)
    }

    /// Ask the session for `trip`, answering whether it was made.
    fn ask_effects(trip: &EffectsTrip) -> Result<(), Errno> {
        let request = match trip {
            EffectsTrip::Apply(text) => PinboardRequest::SoundApply {
                document: PinboardDocument::new(text)?,
            },
            EffectsTrip::Pick(event) => PinboardRequest::SoundPick { event: *event },
            EffectsTrip::UseTheme(event) => PinboardRequest::SoundUseTheme { event: *event },
        };
        let mut reply = [0u8; STATUS_REPLY_LEN];
        let len = pinboard_call(&request, &mut reply)?;
        decode_status_reply(reply.get(..len).ok_or(Errno::LengthOutOfRange)?)
    }

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
            identity: tairix_procinfo::system_identity(&IpcTransport).ok(),
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
            DesktopAsk::Savers => DesktopAnswer::Savers(fetch_savers(client)),
            DesktopAsk::Authorise { window_id, task } => {
                DesktopAnswer::Authorise(client.authorise(*window_id, task))
            }
            DesktopAsk::TakeOutput {
                window_id,
                exit_code,
            } => DesktopAnswer::Output {
                exit_code: *exit_code,
                output: client.take_authorised_output(*window_id),
            },
        }
    }

    /// The most times one listing of the screensavers starts over because the
    /// list moved while it was read.
    const SAVER_RELISTS: usize = 2;

    /// The screensavers the desktop offers, one round trip apiece: the
    /// session answers from the catalogue it holds.
    ///
    /// Each entry is whole, but the catalogue may move between two of them,
    /// so a listing whose total moved, or that ran past a catalogue that
    /// shrank, starts over, a bounded number of times; one still moving after
    /// the last is refused rather than kept half old.
    fn fetch_savers(
        client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
    ) -> Result<Vec<SaverEntry>, Errno> {
        let mut frame = [0u8; tairix_abi::window_ipc::WINDOW_SCREENSAVER_REPLY_MAX];
        let mut savers: Vec<SaverEntry> = Vec::new();
        for _ in 0..=SAVER_RELISTS {
            savers.clear();
            let (mut total, mut index, mut moved) = (None, 0u16, false);
            while total.is_none_or(|total| index < total) {
                let (now, entry) = match client.screensaver(index, &mut frame) {
                    Ok(answered) => answered,
                    Err(Errno::NotFound) if index > 0 => {
                        moved = true;
                        break;
                    }
                    Err(errno) => return Err(errno),
                };
                moved |= total.is_some_and(|total| total != now);
                total = Some(now);
                SaverEntry::list(&mut savers, index, &entry);
                index = index.saturating_add(1);
            }
            if !moved {
                return Ok(savers);
            }
        }
        Err(Errno::Interrupted)
    }

    /// What the desktop said a system task came to, as the pane adopts it, or
    /// `None` for a capture, whose output is fetched first.
    const fn authorised_of(outcome: AuthorisedOutcome) -> Option<Authorised> {
        match outcome {
            AuthorisedOutcome::Ran { exit_code } => Some(Authorised::Ran(exit_code)),
            AuthorisedOutcome::Launched => Some(Authorised::Launched),
            AuthorisedOutcome::Overran { .. } => Some(Authorised::Overran),
            AuthorisedOutcome::Declined => Some(Authorised::Declined),
            AuthorisedOutcome::Captured { .. } => None,
        }
    }

    /// Adopt what the desktop answered into `shell`, stating a refusal.
    fn adopt_desktop_answer(shell: &mut Shell, answer: DesktopAnswer) {
        report_desktop(&answer);
        answer.adopt(shell);
    }

    /// State what the desktop refused.
    fn report_desktop(answer: &DesktopAnswer) {
        if let Some((what, err)) = answer.refusal() {
            app::report(
                APP_NAME,
                format_args!("the desktop would not {what} ({err})"),
            );
        }
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

        /// Ask which screensavers the desktop offers if their pane wants
        /// them, answering whether an answer landed at once.
        fn request_savers(&mut self, shell: &mut Shell) -> bool {
            shell.savers_wanted() && self.submit(shell, &DesktopAsk::Savers)
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

        /// Wait out every request already handed over. One held behind it is
        /// not asked: each names the window, which is gone.
        fn see_out(&self) {
            while let Some(answer) = self.worker.wait() {
                report_desktop(&answer);
            }
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

    /// The cooling desk's client half: one reading at a time, and the next
    /// due on its period while the pane that states it is on show.
    struct CoolingRead<'a> {
        worker: &'a CoolingDesk,
        pending: bool,
        refresh_ns: Option<u64>,
    }

    impl CoolingRead<'_> {
        /// Ask for a reading if the pane wants one and none is outstanding,
        /// answering whether anything on screen changed.
        fn request(&mut self, shell: &mut Shell) -> bool {
            if self.pending || !shell.cooling_wanted() {
                return false;
            }
            if self.worker.submit(()) {
                return self.settle(shell);
            }
            self.pending = true;
            false
        }

        /// Ask again once the period has passed, answering whether anything on
        /// screen changed.
        fn refresh(&mut self, shell: &mut Shell) -> bool {
            if self
                .refresh_ns
                .is_none_or(|due| due > tairix_rt::clock_get())
            {
                return false;
            }
            self.refresh_ns = None;
            shell.cooling_due();
            self.request(shell)
        }

        /// Adopt a landed reading, answering whether anything changed.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let Some(reading) = self.worker.collect() else {
                return false;
            };
            self.pending = false;
            shell.adopt_cooling(reading);
            self.refresh_ns = shell
                .states_cooling()
                .then(|| tairix_rt::clock_get().saturating_add(COOLING_REFRESH_NS));
            true
        }

        /// When the next reading is due.
        const fn due(&self) -> Option<u64> {
            self.refresh_ns
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

    /// Adopt what the desktop answered: state a refusal, then show what the
    /// store holds.
    ///
    /// Persist-then-adopt. The rows showed the reader's choice at once; the
    /// durable value is what the store answered with, read on the worker, so
    /// a refusal puts the row back rather than leaving a value on screen the
    /// next login would not restore.
    fn adopt_apply(shell: &mut Shell, Applied { outcome, in_effect }: Applied) {
        report_apply(&outcome);
        match in_effect {
            Some(settings) => shell.adopt_settings(settings),
            None => shell.revert_settings(),
        }
    }

    /// State an apply the desktop did not make.
    fn report_apply(outcome: &ApplyOutcome) {
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

    /// The desktop's zone store, read through the read-only delegation the
    /// session mints this application, or `None` — stated — where it cannot
    /// be had: the Language & Region rows then offer the default alone, and
    /// the clock reads UTC.
    fn fetch_zones(
        client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
    ) -> Option<&'static ZoneInfo<'static>> {
        let unavailable = |why: &dyn core::fmt::Display| {
            app::report(
                APP_NAME,
                format_args!(
                    "the desktop's zone store could not be read ({why}); dates and times \
                              read UTC"
                ),
            );
        };
        let handle = match client.zone_store() {
            Ok(handle) => handle,
            Err(err) => {
                unavailable(&err);
                return None;
            }
        };
        let Some(session) = client.session() else {
            unavailable(&Errno::NotFound);
            return None;
        };
        let file = match tairix_rt::File::from_delegation_by(handle, session) {
            Ok(file) => file,
            Err(raw) => {
                unavailable(&Errno::from_syscall(raw));
                return None;
            }
        };
        match tairix_tz::load(&file) {
            Ok(zones) => Some(zones),
            Err(err) => {
                unavailable(&err);
                None
            }
        }
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
        /// When the window next has something timed to do — a password
        /// marker's dots, a held scrollbar press's step — written by the loop
        /// just before it parks. `None` arms no timer at all.
        timed_due: &'a Cell<Option<u64>>,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let woken = match self.timed_due.get() {
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
                Wake::App(AUDIO_NOTICE_TOKEN) => {
                    self.workers.sound_moved.set(true);
                    Ok(Parked::Interrupted)
                }
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
        /// The reader asked for a system task: ask the desktop to perform it.
        Authorise(Box<SystemTask>),
        /// The desktop said what came of the task it was asked for.
        Authorised(AuthorisedOutcome),
        /// The desktop queued a target: drain the queue and show what it
        /// named.
        Opened,
        /// The reader changed a sound device's control: ask the audio service.
        Sound {
            /// The device.
            device_id: u32,
            /// What to change.
            control: DeviceControl,
        },
        /// The reader changed a sound choice: ask the desktop session.
        Effects(EffectsAsk),
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
        /// The reader asked to choose the time zone: open the chooser.
        ChooseZone,
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
        let concluded = acted_of;
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
                match surface.window.resize(mode) {
                    app::Fitted::Adopted => surface.mode = mode,
                    app::Fitted::Deferred => {}
                    app::Fitted::Kept => app::report(APP_NAME, app::SIZE_KEPT),
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
                pointer_input_events(*action, pointer_point(*x, *y)),
                damage,
                |input, damage| shell.on_pointer(input, viewport, scale, theme, damage),
            )),
            WindowEvent::Scrolled { x, y, dx, dy, .. } => concluded(apply_pointer(
                scroll_input_events(pointer_point(*x, *y), *dx, *dy),
                damage,
                |input, damage| shell.on_pointer(input, viewport, scale, theme, damage),
            )),
            // The desktop queued at least one target for this instance.
            WindowEvent::OpenRequested => Acted::Opened,
            // What moved while the reader was elsewhere is asked for again;
            // the answer redraws whatever it changes.
            WindowEvent::Focus { focused: true, .. } => {
                shell.focused();
                Acted::Idle
            }
            WindowEvent::Authorised { outcome, .. } => Acted::Authorised(*outcome),
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
            // desktop's, declares no file association, owns no desktop layer
            // and no tool window — and `ContentReleased` is the caller's,
            // which owns the region it lets go of.
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
            | WindowEvent::AlertClosed { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::Pinch { .. }
            | WindowEvent::Key { .. }
            | WindowEvent::Focus { focused: false, .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::RedrawRequested { .. }
            | WindowEvent::ContentReleased { .. }
            | WindowEvent::FilePicked { .. }
            | WindowEvent::PickCancelled { .. }
            | WindowEvent::FolderPicked { .. }
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

    /// What the loop does with what the shell concluded.
    fn acted_of(outcome: ShellOutcome) -> Acted {
        match outcome {
            ShellOutcome::Idle => Acted::Idle,
            ShellOutcome::Changed => Acted::Changed,
            ShellOutcome::Apply(document) => Acted::Apply(document),
            ShellOutcome::Authorise(task) => Acted::Authorise(task),
            ShellOutcome::LockScreen => Acted::LockScreen,
            ShellOutcome::ChooseZone => Acted::ChooseZone,
            ShellOutcome::PreviewScreensaver(document) => Acted::PreviewScreensaver(document),
            ShellOutcome::Sound {
                device_id, control, ..
            } => Acted::Sound { device_id, control },
            ShellOutcome::Effects(ask) => Acted::Effects(ask),
        }
    }

    /// Route one wire pointer event through `feed`: a move to `at` to sync
    /// the pointer, then the press or release the action names.
    ///
    /// A press and its release are two inputs of one gesture, so the
    /// stronger of what they concluded is the gesture's: an apply the
    /// release asked for is not lost behind the press's bare repaint.
    fn apply_pointer(
        inputs: impl Iterator<Item = InputEvent>,
        damage: &mut Region,
        mut feed: impl FnMut(&InputEvent, &mut Region) -> ShellOutcome,
    ) -> ShellOutcome {
        let mut concluded = ShellOutcome::Idle;
        for input in inputs {
            let acted = feed(&input, damage);
            // A conclusion the caller must act on outranks a repaint: a
            // press and its release are two events, and the one that asked
            // for something must not be lost to the one that did not.
            let asks = |outcome: &ShellOutcome| {
                matches!(
                    outcome,
                    ShellOutcome::Apply(_)
                        | ShellOutcome::Authorise(_)
                        | ShellOutcome::LockScreen
                        | ShellOutcome::ChooseZone
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

    /// Adopt the desktop the session published, if the park said it moved,
    /// and the output it is drawn on, which decides the resolutions offered.
    fn adopt_desktop(
        shell: &mut Shell,
        desktop: &mut Desktop,
        themes: &mut ThemeRegistry,
        moved: &Cell<bool>,
    ) -> bool {
        if !moved.replace(false) {
            return false;
        }
        match app::adopt_desktop(desktop, themes) {
            Ok(changed) => {
                shell.adopt_output(output_of(desktop));
                shell.adopt_machine_zone(desktop.info().machine_zone());
                changed
            }
            Err(err) => {
                app::report(APP_NAME, format_args!("desktop change refused: {err}"));
                false
            }
        }
    }

    /// The output `desktop` is drawn on, as its record states it.
    fn output_of(desktop: &Desktop) -> (u32, u32) {
        let info = desktop.info();
        (info.output_width_px(), info.output_height_px())
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
        if !adopt_desktop(shell, desktop, themes, moved) {
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
        /// The endpoint the session delivers this application's events to.
        endpoint: u64,
        desktop: &'a mut Desktop,
        themes: &'a mut ThemeRegistry,
        shell: &'a mut Shell,
        desktop_moved: &'a Cell<bool>,
        pressure_moved: &'a Cell<bool>,
        timed_due: &'a Cell<Option<u64>>,
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
        /// The render requests the panes' pictures cost.
        asker: &'a Asker,
        /// What only the desktop answers.
        desktop: DesktopDesk<'a>,
        /// The audio service's controls and readings the Sound pane needs.
        sound: SoundRead<'a>,
        /// The session's round trips the Sound Effects pane needs.
        effects: EffectsRead<'a>,
        /// The cooling service's status the Raspberry Pi pane states.
        cooling: CoolingRead<'a>,
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
                asker: &workers.asker,
                desktop: DesktopDesk {
                    worker: &workers.desktop,
                    asks: DesktopAsks::new(),
                },
                sound: SoundRead {
                    worker: &workers.sound,
                    queue: ControlQueue::new(),
                    asked: false,
                    moved: &workers.sound_moved,
                },
                effects: EffectsRead {
                    worker: &workers.effects,
                    asked: false,
                    asks: EffectsAsks::new(),
                },
                cooling: CoolingRead {
                    worker: &workers.cooling,
                    pending: false,
                    refresh_ns: None,
                },
            }
        }

        /// See every change still owed out before the program ends, so one
        /// made just before it ended is never dropped with its worker. Only
        /// what is refused is stated, on `stderr`, the window being gone; the
        /// desks that only read are left to stop.
        fn see_out(&mut self) {
            while let Some(Applied { outcome, .. }) = self.applier.wait() {
                report_apply(&outcome);
            }
            self.sound.see_out();
            self.effects.see_out();
            self.desktop.see_out();
        }
    }

    /// The sound-choices desk's client half: one ask at a time, the newest
    /// value of each setting carried together meanwhile.
    struct EffectsRead<'a> {
        worker: &'a EffectsDesk,
        /// A reading the pane wants has been asked for.
        asked: bool,
        asks: EffectsAsks,
    }

    impl EffectsRead<'_> {
        /// Ask for a fresh reading if the pane wants one, answering whether
        /// anything on screen changed.
        fn request(&mut self, shell: &mut Shell) -> bool {
            if !shell.effects_wanted() || self.asked {
                return false;
            }
            self.asked = true;
            self.submit(shell, EffectsJob::Read)
        }

        /// Ask the session for `ask`, answering whether anything on screen
        /// changed at once.
        fn ask(&mut self, shell: &mut Shell, ask: EffectsAsk) -> bool {
            self.asks.ask(ask);
            self.start(shell)
        }

        /// Start the ask waiting, if none is in flight.
        fn start(&mut self, shell: &mut Shell) -> bool {
            match self.asks.next_trip() {
                Some(trip) => self.submit(shell, EffectsJob::Ask(trip)),
                None => false,
            }
        }

        fn submit(&mut self, shell: &mut Shell, job: EffectsJob) -> bool {
            match self.worker.submit(job) {
                Ok(true) => self.settle(shell),
                Ok(false) => false,
                Err(job) => {
                    app::report(
                        APP_NAME,
                        "too many sound requests are waiting on the desktop; this one is dropped",
                    );
                    match job {
                        EffectsJob::Read => self.asked = false,
                        EffectsJob::Ask(_) => self.asks.landed(),
                    }
                    false
                }
            }
        }

        /// Adopt every landed answer, reading the choices again after a change
        /// so the pane shows what the session holds; answering whether any
        /// landed.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let mut landed = false;
            let mut changed = false;
            let Self {
                worker,
                asked,
                asks,
            } = self;
            worker.collect_landed(|done| {
                landed = true;
                match done {
                    EffectsDone::Read(reading) => {
                        *asked = false;
                        if let Err(err) = &reading {
                            app::report(
                                APP_NAME,
                                format_args!(
                                    "the desktop would not say what its sounds are ({err})"
                                ),
                            );
                        }
                        shell.adopt_effects(reading.ok().map(|reading| *reading));
                    }
                    EffectsDone::Asked(result) => {
                        report_effects(result);
                        asks.landed();
                        changed = true;
                    }
                }
            });
            if changed {
                shell.effects_moved();
                landed |= self.request(shell);
                landed |= self.start(shell);
            }
            landed
        }

        /// Make every change still waiting, each in turn. A pick is not
        /// started: it asks the reader to choose a file, and the window it
        /// would be asked over is gone.
        fn see_out(&mut self) {
            loop {
                while let Some(done) = self.worker.wait() {
                    if let EffectsDone::Asked(result) = done {
                        report_effects(result);
                        self.asks.landed();
                    }
                }
                match self.asks.next_trip() {
                    Some(EffectsTrip::Pick(_)) => self.asks.landed(),
                    Some(trip) => {
                        if self.worker.submit(EffectsJob::Ask(trip)).is_err() {
                            app::report(APP_NAME, "a change to the desktop's sounds was not made");
                            self.asks.landed();
                        }
                    }
                    None => return,
                }
            }
        }
    }

    /// State a change to the desktop's sounds it did not make.
    fn report_effects(result: Result<(), Errno>) {
        if let Err(err) = result {
            app::report(
                APP_NAME,
                format_args!("the desktop would not change its sounds ({err})"),
            );
        }
    }

    /// The sound desk's client half: one round trip at a time, the latest
    /// control of each kind for each device winning meanwhile.
    struct SoundRead<'a> {
        worker: &'a SoundDesk,
        queue: ControlQueue,
        /// A reading the pane wants has been asked for.
        asked: bool,
        /// Set by the park when the audio service announces a change.
        moved: &'a Cell<bool>,
    }

    impl SoundRead<'_> {
        /// Ask for a fresh reading if the pane wants one, answering whether
        /// anything on screen changed.
        fn request(&mut self, shell: &mut Shell) -> bool {
            if self.moved.take() {
                shell.sound_moved();
            }
            if shell.sound_wanted() && !self.asked {
                self.asked = true;
                self.queue.refresh();
            }
            self.start(shell)
        }

        /// Ask for `control` on `device_id`.
        fn ask(&mut self, shell: &mut Shell, device_id: u32, control: DeviceControl) -> bool {
            self.queue.ask(device_id, control);
            self.start(shell)
        }

        /// Start the round trip owed, if none is in flight.
        fn start(&mut self, shell: &mut Shell) -> bool {
            let Some(controls) = self.queue.next_trip() else {
                return false;
            };
            if self.worker.submit(controls) {
                return self.settle(shell);
            }
            false
        }

        /// Adopt every landed reading, answering whether any landed.
        fn settle(&mut self, shell: &mut Shell) -> bool {
            let mut landed = false;
            while let Some(reading) = self.worker.collect() {
                self.queue.landed();
                self.asked = false;
                shell.adopt_sound(reading);
                landed = true;
            }
            if landed {
                self.start(shell);
            }
            landed
        }

        /// Apply every control still waiting, a trip at a time. A trip owed
        /// only for a listing is not made: nothing is left to show it.
        fn see_out(&mut self) {
            loop {
                while self.worker.wait().is_some() {
                    self.queue.landed();
                }
                match self.queue.next_trip() {
                    Some(controls) if !controls.is_empty() => {
                        let _ = self.worker.submit(controls);
                    }
                    _ => return,
                }
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
        landed |= desks.desktop.settle(shell);
        landed |= desks.sound.settle(shell);
        landed |= desks.effects.settle(shell);
        landed |= desks.cooling.settle(shell);
        if !landed {
            return true;
        }
        repaint_pane(surface, shell, themes, desktop)
    }

    /// Redraw the pane, whose rows a landed answer rebuilt, answering whether
    /// the window is still presentable.
    fn repaint_pane(
        surface: &mut SettingsWindow,
        shell: &mut Shell,
        themes: &ThemeRegistry,
        desktop: &Desktop,
    ) -> bool {
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

    /// Step a held scrollbar press whose repeat is due and present what moved,
    /// answering whether the window is still presentable.
    fn step_held_bars(
        surface: &mut SettingsWindow,
        shell: &mut Shell,
        themes: &ThemeRegistry,
        desktop: &Desktop,
    ) -> bool {
        let mut damage = tairix_controls::damage::sink();
        let outcome = shell.repeat(
            tairix_rt::clock_get(),
            surface.viewport(),
            desktop.scale(),
            themes.active(),
            &mut damage,
        );
        if !outcome.changed() {
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
            Acted::Authorise(task) => {
                // Submitted, not awaited: the desktop puts its prompt up from
                // its own loop, and what the task came to arrives as an event.
                let Some(window_id) = surface.window.window_id() else {
                    shell.adopt_authorisation(Authorised::Refused(String::from(
                        "The settings window is not open.",
                    )));
                    return Carried::Answered;
                };
                Carried::answered_if(desks.desktop.submit(
                    shell,
                    &DesktopAsk::Authorise {
                        window_id,
                        task: **task,
                    },
                ))
            }
            Acted::Authorised(AuthorisedOutcome::Captured { exit_code, .. }) => {
                // What it printed is fetched before the pane is told.
                let Some(window_id) = surface.window.window_id() else {
                    shell.adopt_authorisation(Authorised::Refused(String::from(
                        "The settings window is not open.",
                    )));
                    return Carried::Answered;
                };
                Carried::answered_if(desks.desktop.submit(
                    shell,
                    &DesktopAsk::TakeOutput {
                        window_id,
                        exit_code: *exit_code,
                    },
                ))
            }
            Acted::Authorised(outcome) => {
                if let Some(authorised) = authorised_of(*outcome) {
                    shell.adopt_authorisation(authorised);
                }
                Carried::Answered
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
            Acted::Sound { device_id, control } => {
                Carried::answered_if(desks.sound.ask(shell, *device_id, *control))
            }
            Acted::Effects(ask) => Carried::answered_if(desks.effects.ask(shell, ask.clone())),
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
            // The loop opens the chooser: it holds the window.
            Acted::Idle | Acted::Changed | Acted::Whole | Acted::ChooseZone => Carried::Nothing,
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
            desks.desktop.request_savers(shell),
            desks.sound.request(shell),
            desks.effects.request(shell),
            desks.cooling.request(shell),
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
            || !step_held_bars(surface, shell, themes, desktop)
        {
            return false;
        }
        // The temperature's period is the clock's, not an event's.
        if desks.cooling.refresh(shell) && !repaint_pane(surface, shell, themes, desktop) {
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

    /// The wall clock's reading, where it has been set.
    fn wall_now() -> Option<Time64> {
        tairix_rt::wall_time()
            .ok()
            .filter(|reading| reading.state().is_set())
            .map(|reading| reading.time())
    }

    /// The zone chooser: an owned dialog window over the zone store's places,
    /// the machine's zone first.
    struct ZoneChooser {
        pane: WindowPane,
        surface: Surface,
        dialog: PlaceDialog,
        owed: Owed,
        written: Minute,
    }

    impl ZoneChooser {
        /// Open the chooser over the shell's zone rows, held by the settings
        /// window, and draw it; a chooser that cannot open says why in an
        /// alert.
        fn open(
            shell: &Shell,
            window: &mut SettingsWindow,
            endpoint: u64,
            (themes, desktop): (&ThemeRegistry, &Desktop),
        ) -> Option<Self> {
            let mut opened =
                Self::opened(shell, window, endpoint, (themes.active(), desktop.scale()));
            if let Some(chooser) = opened.as_mut() {
                if chooser
                    .present(window.window.client(), desktop.scale(), themes.active())
                    .is_err()
                {
                    app::report(APP_NAME, "the zone chooser could not be drawn");
                }
            }
            opened
        }

        /// The chooser's window, opened and owing its whole first frame.
        fn opened(
            shell: &Shell,
            window: &mut SettingsWindow,
            endpoint: u64,
            (theme, scale): (&Theme, Scale),
        ) -> Option<Self> {
            let owner = window.window.window_id();
            let refused = |window: &mut SettingsWindow, detail: &str| {
                let _ = app::refuse(
                    window.window.client(),
                    (APP_NAME, endpoint),
                    owner,
                    "The time zone cannot be chosen",
                    detail,
                );
            };
            let now = wall_now();
            let Some((rows, held)) = shell.zone_rows(now) else {
                refused(
                    window,
                    "The desktop's list of time zones could not be read.",
                );
                return None;
            };
            let dialog = zone::dialog(rows, held);
            let (width, height) = dialog.opening_extent(scale, theme);
            let (Some(owner), Some(server), Some(surface)) = (
                owner,
                window.window.client().session(),
                Surface::new(width, height),
            ) else {
                refused(window, "The settings window is not open.");
                return None;
            };
            let opened = WindowPane::open_dialog(
                window.window.client(),
                (owner, server),
                endpoint,
                &app::mode_for(width, height),
                DialogModality::OwnerModal,
                dialog.title(),
            );
            match opened {
                Ok(pane) => {
                    let mut owed = Owed::new();
                    owed.owe_whole();
                    Some(Self {
                        pane,
                        surface,
                        dialog,
                        owed,
                        written: Minute::of(now),
                    })
                }
                Err(err) => {
                    refused(window, &alloc::format!("{err}"));
                    None
                }
            }
        }

        /// Route one of the chooser's own events, answering what it came to.
        fn route(
            &mut self,
            event: &WindowEvent,
            client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
            desktop: &Desktop,
            theme: &Theme,
        ) -> Option<PlaceOutcome> {
            let scale = desktop.scale();
            let mode = *self.pane.mode();
            let bounds = Rect::new(0, 0, mode.width_px, mode.height_px);
            let timing = PressTiming {
                now_ns: tairix_rt::clock_get(),
                double_click: desktop.info().double_click(),
            };
            let mut damage = tairix_controls::damage::sink();
            let mut outcome = None;
            match *event {
                WindowEvent::Pointer { x, y, action, .. } => {
                    for input in pointer_input_events(action, pointer_point(x, y)) {
                        outcome = outcome.or(self.dialog.on_pointer(
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
                        outcome = outcome.or(self.dialog.on_pointer(
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
                        outcome =
                            self.dialog
                                .on_key(key, modifiers, bounds, scale, theme, &mut damage);
                    }
                }
                WindowEvent::Focus { focused: false, .. } => {
                    self.dialog.pointer_left(bounds, scale, theme, &mut damage);
                }
                WindowEvent::CloseRequested { .. } => outcome = Some(PlaceOutcome::Cancelled),
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => {
                    let mode = app::mode_for(width_px, height_px);
                    match self.pane.resize_with(client, &mode, &mut self.surface) {
                        Fitted::Adopted => self.owed.owe_whole(),
                        Fitted::Deferred => {}
                        Fitted::Kept => app::report(APP_NAME, app::SIZE_KEPT),
                    }
                }
                WindowEvent::ContentReleased { .. } => self.pane.release_frames(),
                WindowEvent::RedrawRequested { .. } => self.owed.owe_whole(),
                _ => {}
            }
            self.owed.owe(&mode, Repaint::Reported, &damage);
            outcome
        }

        /// Paint and present what the chooser owes; nothing while the
        /// session holds none of its pixels.
        fn present(
            &mut self,
            client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
            scale: Scale,
            theme: &Theme,
        ) -> Result<(), Errno> {
            if self.pane.content_released() {
                return Ok(());
            }
            let mode = *self.pane.mode();
            let Some(parts) = self.owed.take(&mode) else {
                return Ok(());
            };
            let bounds = Rect::new(0, 0, mode.width_px, mode.height_px);
            for rect in parts.rects() {
                let dialog = &self.dialog;
                self.surface
                    .with_clip(rect.x, rect.y, rect.width_px, rect.height_px, |surface| {
                        dialog.render(surface, bounds, scale, theme);
                    });
            }
            self.pane.present_list(client, &self.surface, &parts)
        }

        /// Rewrite the readings once the minute has turned and present what
        /// that moved, answering the monotonic instant the next minute turns
        /// at; nothing while the clock is not set.
        fn keep_current(
            &mut self,
            shell: &Shell,
            client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
            (theme, scale): (&Theme, Scale),
        ) -> Option<u64> {
            let now = wall_now()?;
            let mode = *self.pane.mode();
            let bounds = Rect::new(0, 0, mode.width_px, mode.height_px);
            let mut damage = tairix_controls::damage::sink();
            shell.refresh_zone_readings(
                (&mut self.dialog, &mut self.written),
                now,
                (bounds, scale, theme),
                &mut damage,
            );
            self.owed.owe(&mode, Repaint::Reported, &damage);
            if self.present(client, scale, theme).is_err() {
                app::report(APP_NAME, "the zone chooser could not be drawn");
            }
            let into = u64::try_from(now.secs().rem_euclid(60))
                .unwrap_or(0)
                .saturating_mul(u64::from(NANOS_PER_SEC))
                .saturating_add(u64::from(now.subsec_nanos()));
            let minute = 60 * u64::from(NANOS_PER_SEC);
            Some(tairix_rt::clock_get().saturating_add(minute.saturating_sub(into)))
        }

        /// Take the chooser off the screen.
        fn close(self, client: &mut tairix_window::WindowClient<app::RtWindowTransport>) {
            if let Err(err) = self.pane.close(client) {
                app::report(
                    APP_NAME,
                    format_args!("the zone chooser would not close ({err})"),
                );
            }
        }
    }

    /// Serve `event` where it is the zone chooser's own, answering `None`
    /// where it is not: what the chooser chose is posted as any other choice
    /// is, and it closes on a choice or a cancel. A desktop that `redrew`
    /// owes the chooser its whole frame too.
    ///
    /// The `Err` is the settings window's refused present, which ends the
    /// program.
    fn serve_chooser(
        (event, redrew): (&WindowEvent, bool),
        zones: &mut Option<ZoneChooser>,
        (surface, shell): (&mut SettingsWindow, &mut Shell),
        (themes, desktop): (&ThemeRegistry, &Desktop),
        (desks, pictures): (&mut Desks<'_>, &mut Pictures),
    ) -> Option<Result<(), Errno>> {
        let chooser = zones.as_mut()?;
        if redrew {
            chooser.owed.owe_whole();
        }
        if event.window_id() != Some(chooser.pane.id()) {
            return None;
        }
        let chosen = match chooser.route(event, surface.window.client(), desktop, themes.active()) {
            Some(PlaceOutcome::Chose { row }) => Some(row),
            Some(PlaceOutcome::Cancelled) => None,
            None => {
                present_chooser(zones, surface, desktop, themes);
                return Some(Ok(()));
            }
        };
        if let Some(chooser) = zones.take() {
            chooser.close(surface.window.client());
        }
        let Some(ShellOutcome::Apply(document)) = chosen.map(|row| shell.choose_zone(row)) else {
            return Some(Ok(()));
        };
        let mut damage = tairix_controls::damage::sink();
        let _ = act(
            &Acted::Apply(document),
            (surface, shell),
            (themes, desktop),
            (desks, pictures),
            &mut damage,
        );
        shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
        let whole = DamageRect::full(&surface.mode);
        Some(surface.present(shell, themes, desktop.scale(), whole))
    }

    /// Present what the zone chooser owes, if one is open; a refusal is
    /// stated and the chooser stays.
    fn present_chooser(
        zones: &mut Option<ZoneChooser>,
        surface: &mut SettingsWindow,
        desktop: &Desktop,
        themes: &ThemeRegistry,
    ) {
        if let Some(chooser) = zones.as_mut() {
            if chooser
                .present(surface.window.client(), desktop.scale(), themes.active())
                .is_err()
            {
                app::report(APP_NAME, "the zone chooser could not be drawn");
            }
        }
    }

    /// What waiting for the loop's next event came to.
    enum Waited {
        /// An event to apply.
        Event(WindowEvent),
        /// Nothing to apply: wait again.
        Again,
        /// The program ends with this status.
        Ended(i32),
    }

    /// Wait for the next event. A wait that ended without one is the desktop
    /// notice, and a malformed frame from the authenticated session is
    /// refused rather than guessed at: either way the round is the re-theme
    /// alone.
    fn wait_event(
        events: &mut WindowEvents<RtEventSource<'_>>,
        (surface, shell): (&mut SettingsWindow, &mut Shell),
        (themes, desktop): (&mut ThemeRegistry, &mut Desktop),
        (desktop_moved, pictures): (&Cell<bool>, &mut Pictures),
    ) -> Waited {
        match events.wait(surface.window.client()) {
            Ok(Some(event)) => Waited::Event(event),
            Ok(None) | Err(EventError::Undecodable(_)) => {
                if adopt_desktop_round(surface, shell, themes, desktop, desktop_moved, pictures) {
                    Waited::Again
                } else {
                    Waited::Ended(app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused"))
                }
            }
            Err(EventError::Refused(refused)) => {
                app::report(APP_NAME, refused);
                Waited::Again
            }
            Err(EventError::Mailbox(_)) => {
                Waited::Ended(app::fail(APP_NAME, EXIT_CHANNEL_LOST, "event channel lost"))
            }
        }
    }

    /// When the window next has something timed to do: a password marker's
    /// next step, a held scrollbar press's, or the zone chooser's next minute,
    /// whose readings are brought up to date here first.
    fn next_due(
        shell: &mut Shell,
        zones: &mut Option<ZoneChooser>,
        surface: &mut SettingsWindow,
        (themes, desktop): (&ThemeRegistry, &Desktop),
    ) -> Option<u64> {
        // A verdict that landed while parked may have answered it.
        let held = shell.repeat_deadline(tairix_rt::clock_get());
        let turn = zones.as_mut().and_then(|chooser| {
            chooser.keep_current(
                shell,
                surface.window.client(),
                (themes.active(), desktop.scale()),
            )
        });
        shell
            .secret_deadline_ns()
            .into_iter()
            .chain(held)
            .chain(turn)
            .min()
    }

    /// The event loop: park, apply, repaint.
    fn run_event_loop(session: Session<'_>, mut events: WindowEvents<RtEventSource<'_>>) -> i32 {
        let Session {
            surface,
            endpoint,
            desktop,
            themes,
            shell,
            desktop_moved,
            pressure_moved,
            timed_due,
            pictures,
            mut desks,
        } = session;
        let mut zones: Option<ZoneChooser> = None;
        let code = 'serve: loop {
            if !catch_up(
                (surface, shell),
                (themes, desktop),
                (pictures, &mut desks),
                pressure_moved,
            ) {
                break 'serve app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
            }
            let due = next_due(shell, &mut zones, surface, (themes, desktop));
            timed_due.set(due.into_iter().chain(desks.cooling.due()).min());
            let event = match wait_event(
                &mut events,
                (surface, shell),
                (themes, desktop),
                (desktop_moved, pictures),
            ) {
                Waited::Event(event) => event,
                Waited::Again => continue,
                Waited::Ended(code) => break 'serve code,
            };

            let redraw = adopt_desktop(shell, desktop, themes, desktop_moved);
            if redraw {
                shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
                pictures.restart();
            }
            match serve_chooser(
                (&event, redraw),
                &mut zones,
                (surface, shell),
                (themes, desktop),
                (&mut desks, pictures),
            ) {
                Some(Ok(())) => continue,
                Some(Err(_)) => {
                    break 'serve app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused")
                }
                None => {}
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
                break 'serve 0;
            }
            if acted == Acted::ChooseZone && zones.is_none() {
                zones = ZoneChooser::open(shell, surface, endpoint, (themes, desktop));
            }
            let landed = request_readings(shell, &mut desks);
            if matches!(event, WindowEvent::ContentReleased { .. }) {
                surface.window.release_frames();
                continue;
            }
            let round = Round {
                whole: redraw || carried == Carried::Moved || matches!(acted, Acted::Whole),
                acted: !matches!(acted, Acted::Idle),
                answered: landed || carried == Carried::Answered,
            };
            if present_round(round, &mut damage, (surface, shell), (themes, desktop)).is_err() {
                break 'serve app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
            }
        };
        desks.see_out();
        code
    }

    /// Present what one round changed: the pane laid out again where its rows
    /// moved, then the damage reported or the pane reshaped.
    fn present_round(
        round: Round,
        damage: &mut Region,
        (surface, shell): (&mut SettingsWindow, &mut Shell),
        (themes, desktop): (&ThemeRegistry, &Desktop),
    ) -> Result<(), Errno> {
        if round.relays() {
            shell.lay_out(surface.viewport(), desktop.scale(), themes.active());
        }
        let pane = shell.pane_region(surface.viewport(), desktop.scale(), themes.active());
        let repaint = round.repaint(damage, pane);
        let Some(area) = present_damage(&surface.mode, repaint, damage) else {
            return Ok(());
        };
        surface.present(shell, themes, desktop.scale(), area)
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
        // And the zone store, which the session holds open for exactly this.
        shell.adopt_zones(fetch_zones(surface.window.client()));
        // The resolutions on offer are what the output can show.
        shell.adopt_output(output_of(desktop));
        // And *Default* is the machine's own zone, as the desktop states it.
        shell.adopt_machine_zone(desktop.info().machine_zone());
        // The font store is read once per boot, so its families are too.
        shell.adopt_text_choices(tairix_font::families(), *themes.selected().fonts());
        // A pane the launch named, if it named one: a fresh process is
        // given it as its one operand, exactly as a running instance is
        // handed it over the channel. `args` has already dropped the
        // program's own name, so the operand is the first of them.
        if let Some(argv) = tairix_rt::args().filter(|argv| !argv.is_empty()) {
            let mut sink = tairix_controls::damage::sink();
            let viewport = surface.viewport();
            let named = Pane::launched(&argv, shell.board()).is_some_and(|pane| {
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
        asker: Arc<Asker>,
        desktop: Arc<DesktopAsker>,
        sound: Arc<SoundDesk>,
        /// Set when the audio service announces a change, for the sound desk.
        sound_moved: Cell<bool>,
        effects: Arc<EffectsDesk>,
        cooling: Arc<CoolingDesk>,
    }

    impl Workers {
        /// Start every desk, its answer wake watched on `set`. A machine
        /// that grants no thread or no wake runs the work on the event loop
        /// instead, which each start states for itself.
        ///
        /// # Errors
        ///
        /// The exit code of the stated failure when the asker's queue could
        /// not be held.
        fn started(set: u64) -> Result<Self, i32> {
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
            let Ok(effects) = EffectsDesk::queued(
                serve_effects,
                (),
                tairix_rt::sync::WorkerWake::create(),
                EFFECTS_JOBS,
            ) else {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    "no room for the desktop's sound requests",
                ));
            };
            Ok(Self {
                applier: started(
                    Applier::new(send_apply, (), tairix_rt::sync::WorkerWake::create()),
                    (set, APPLY_TOKEN),
                    "apply",
                ),
                mounts: started(
                    Mounts::new(read_mounts, (), tairix_rt::sync::WorkerWake::create()),
                    (set, MOUNTS_TOKEN),
                    "mount-table",
                ),
                machine: started(
                    Machine::new(read_machine, (), tairix_rt::sync::WorkerWake::create()),
                    (set, MACHINE_TOKEN),
                    "machine-readings",
                ),
                network: started(
                    Network::new(read_network, (), tairix_rt::sync::WorkerWake::create()),
                    (set, NETWORK_TOKEN),
                    "network-readings",
                ),
                accounts: started(
                    Accounts::new(read_accounts, (), tairix_rt::sync::WorkerWake::create()),
                    (set, ACCOUNTS_TOKEN),
                    "account-readings",
                ),
                asker: started(asker, (set, ASK_TOKEN), "preview-request"),
                desktop: started(desktop, (set, DESKTOP_TOKEN), "desktop-request"),
                sound: started(
                    SoundDesk::new(
                        serve_sound,
                        RtAudio::new(),
                        tairix_rt::sync::WorkerWake::create(),
                    ),
                    (set, SOUND_TOKEN),
                    "sound",
                ),
                sound_moved: Cell::new(false),
                effects: started(effects, (set, EFFECTS_TOKEN), "sound-choices"),
                cooling: started(
                    CoolingDesk::new(read_cooling, (), tairix_rt::sync::WorkerWake::create()),
                    (set, COOLING_TOKEN),
                    "cooling",
                ),
            })
        }

        /// Each desk's wake and the token it is watched under.
        fn wakes(&self) -> [(&tairix_rt::sync::WorkerWake, u64); 10] {
            [
                (self.applier.wake(), APPLY_TOKEN),
                (self.mounts.wake(), MOUNTS_TOKEN),
                (self.machine.wake(), MACHINE_TOKEN),
                (self.network.wake(), NETWORK_TOKEN),
                (self.accounts.wake(), ACCOUNTS_TOKEN),
                (self.asker.wake(), ASK_TOKEN),
                (self.desktop.wake(), DESKTOP_TOKEN),
                (self.sound.wake(), SOUND_TOKEN),
                (self.effects.wake(), EFFECTS_TOKEN),
                (self.cooling.wake(), COOLING_TOKEN),
            ]
        }

        /// Put the audio service's notices on `set`, so the sound desk hears
        /// of a device change while the loop is parked.
        ///
        /// # Errors
        ///
        /// The exit code of the stated failure when the wait-set refuses one.
        fn watched(set: u64) -> Result<(), i32> {
            for topic in [NoticeTopic::AudioDevices, NoticeTopic::AudioCapture] {
                if tairix_rt::waitset_ctl(
                    set,
                    WaitSetOp::Add,
                    WaitSourceKind::SystemNotice,
                    u64::from(topic.as_u32()),
                    AUDIO_NOTICE_TOKEN,
                ) != 0
                {
                    return Err(app::fail(
                        APP_NAME,
                        app::EXIT_NO_EVENTS,
                        "sound notice wait refused",
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
            let Some((wake, _)) = self
                .wakes()
                .into_iter()
                .find(|&(_, watched)| watched == token)
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
            self.asker.stop();
            self.sound.stop();
            self.desktop.stop();
            self.effects.stop();
            self.cooling.stop();
        }
    }

    /// Put `worker` on a desk of its own and start it, its answer wake
    /// watched on `set` under `token`.
    fn started<S, Req, Ans, D>(
        worker: tairix_rt::work::Worker<S, Req, Ans, D>,
        (set, token): (u64, u64),
        what: &str,
    ) -> Arc<tairix_rt::work::Worker<S, Req, Ans, D>>
    where
        S: Send + 'static,
        Req: Send + 'static,
        Ans: Send + 'static,
        D: tairix_rt::work::Desk<Req, Ans> + Send + 'static,
    {
        let worker = Arc::new(worker);
        app::start_worker(APP_NAME, &worker, set, token, what);
        worker
    }

    /// The window's icon cache, budgeted from the `frame_bytes` of the
    /// window it draws into and the machine's memory, and registered with the
    /// process's cache report.
    fn icon_cache(frame_bytes: usize) -> ArtworkCache {
        // The reclaim bookkeeping's audit sink. The shared constructor takes
        // a `'static` borrow, and the runtime sink owns nothing.
        static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;
        let cache = artwork_cache(
            "settings.icon-artwork",
            SEAT_PRIMARY,
            (
                frame_bytes,
                tairix_procinfo::memory_total_for_budget(&IpcTransport),
            ),
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
        // The board decides the categories only some boards show, so it is
        // read before the strip is drawn; an unanswered read shows none of
        // them.
        let board = tairix_procinfo::platform_compatible(&IpcTransport)
            .map_or_else(|_| Board::default(), |platform| Board::of(&platform));
        let Some(mut shell) = Shell::new(settings_in_effect(), board) else {
            return app::fail(
                APP_NAME,
                EXIT_CHANNEL_LOST,
                "the settings registry holds no categories",
            );
        };
        let workers = match Workers::started(binding.set()) {
            Ok(workers) => workers,
            Err(code) => return code,
        };
        if let Err(code) = Workers::watched(binding.set()) {
            return code;
        }

        seat_first_frame(&mut shell, &mut surface, &desktop, &themes);

        let server = match surface.open(event_endpoint, &shell, &themes, desktop.scale()) {
            Ok(server) => server,
            Err(code) => return code,
        };

        let desktop_moved = Cell::new(false);
        let pressure_moved = Cell::new(false);
        let timed_due = Cell::new(None);
        let events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(event_endpoint, server),
            set: binding.set(),
            workers: &workers,
            desktop_moved: &desktop_moved,
            pressure_moved: &pressure_moved,
            timed_due: &timed_due,
        });
        run_event_loop(
            Session {
                surface: &mut surface,
                endpoint: event_endpoint,
                desktop: &mut desktop,
                themes: &mut themes,
                shell: &mut shell,
                desktop_moved: &desktop_moved,
                pressure_moved: &pressure_moved,
                timed_due: &timed_due,
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
