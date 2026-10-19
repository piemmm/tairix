//! The `netmap.app` bundle's `Run` entry point: the network map window.
//!
//! Everything with behaviour lives in the host-tested `tairix_netmap` engine;
//! this binary composes it over the live window channel. Every read of the
//! network's state and every call to the discovery service runs on the reader
//! worker, and every reverse DNS lookup on a second, so the loop only submits
//! and collects and the window answers within a frame whatever the network is
//! doing. The neighbour cache and routes change with no event to wake on, so
//! while a window is open the map is re-read on a one-shot deadline; the
//! discovery service rings the map's doorbell port, a member of the event
//! wait-set, when its answers move. A spread of services moving out or in owes
//! frames, each a one-shot deadline beside the refresh one. On the host it is
//! an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::boxed::Box;
    use alloc::collections::{BTreeMap, BTreeSet};
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use core::net::IpAddr;

    use tairix_abi::discovery_ipc::DOORBELL_LEN;
    use tairix_abi::driver::display::{DamageRect, DisplayMode};
    use tairix_abi::input::KeyInput;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::net::{SocketAddr, SocketEcho, SocketId, SocketStreamEvent, StreamCloseReason};
    use tairix_abi::net_ipc::{address_parts, ip_from_parts, NetAddrFamily};
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::window_ipc::{
        AppBarClick, ClipboardKind, MenuOutcome, WindowEvent, WindowRegion, WindowSizing,
    };
    use tairix_abi::{Errno, FieldValue, ProcId, ORIGIN_WIRE_LEN};
    use tairix_appdata::{Publication, PublishJob, Published, RtHost, Settings};
    use tairix_controls::damage::{self, Repaint};
    use tairix_controls::Keystroke;
    use tairix_devids::{DbKind, DevIds};
    use tairix_discovery::{DiscoveryError, RtTransport, Session};
    use tairix_geometry::{Rect, Region};
    use tairix_input::InputEvent;
    use tairix_log::{Event, EventId, Field, Level};
    use tairix_netmap::events::{
        PROBE_FINISHED, PROBE_FINISHED_MESSAGE, PROBE_REFUSED, PROBE_STARTED,
    };
    use tairix_netmap::settings::{ProbePane, Request as SettingsRequest};
    use tairix_netmap::{
        layout, settings, win_sizing, AdvertisedService, Collect, Command, ConnectOutcome,
        Discovery, Inventory, MapView, NodeMenu, OpenSession, Posture, Probe, ProbeAction,
        ProbeCampaign, ProbeSettings, Reachability, Reaction, Snapshot, REFRESH_NS, WIN_HEIGHT,
        WIN_WIDTH,
    };
    use tairix_procinfo::IpcTransport;
    use tairix_raster::Surface;
    use tairix_resolver::RtDnsTransport;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_rt::{LogSink, PrivatePort, WaitSet};
    use tairix_theme::ThemeRegistry;
    use tairix_window::app::WindowPane;
    use tairix_window::app::{self, AppWindow, Wake, EXIT_CHANNEL_LOST};
    use tairix_window::{
        clipboard, damage_in, key_input_event, pinch_input_events, pointer_input_events,
        pointer_point, scroll_input_events, DeclaredTip, Desktop, EventDrain, EventError,
        EventMailbox, EventSource, Parked, WindowClient, WindowEvents,
    };

    /// The name this program states its refusals under.
    const APP_NAME: &str = "netmap";
    /// The window's title once the network has been read at least once.
    const APP_TITLE: &str = "Network Map";
    /// The window's title before the first read lands, so a window opened on
    /// an empty map says it is still gathering rather than reading as blank.
    const LOADING_TITLE: &str = "Network Map — Reading…";
    /// The smallest window the map and its panel are usable in, logical.
    const MIN_WIDTH: u32 = 480;
    const MIN_HEIGHT: u32 = 360;
    /// The reader's wake token on the event wait-set.
    const READER_TOKEN: u64 = app::FIRST_APP_TOKEN;
    /// The namer's wake token on the event wait-set.
    const NAMER_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;
    /// The discovery doorbell's token on the event wait-set.
    const DOORBELL_TOKEN: u64 = app::FIRST_APP_TOKEN + 2;
    /// The prober's wake token on the event wait-set.
    const PROBER_TOKEN: u64 = app::FIRST_APP_TOKEN + 3;
    /// The settings publisher's wake token on the event wait-set.
    const PUBLISH_TOKEN: u64 = app::FIRST_APP_TOKEN + 4;
    /// The audit sink every probe campaign is recorded through.
    static LOG_SINK: LogSink = LogSink;
    /// Doorbells the port holds: one ring says "collect", so a few cover a
    /// burst the loop has not yet drained.
    const DOORBELL_CAPACITY: usize = 4;
    /// Jobs the reader queues: a read and a discovery collect at most, each
    /// asked for only while none of its kind is outstanding.
    const READER_QUEUE: usize = 2;
    /// Reverse lookups the namer queues; the rest wait for the next read.
    const NAMER_QUEUE: usize = 16;

    /// A request to the reader.
    enum Job {
        /// Read the network's state now.
        Read,
        /// Collect what the discovery service holds, and bring its requests in
        /// line, asking it to name these link-local addresses.
        Discover(BTreeSet<IpAddr>),
    }

    /// What the discovery service holds for the map now.
    struct Discovered {
        services: Vec<AdvertisedService>,
        names: BTreeMap<IpAddr, String>,
        truncated: bool,
        refused: Option<DiscoveryError>,
        stalled: Option<DiscoveryError>,
        /// Work was left for the next read: an ask stalled, or a lost
        /// request held back.
        unsettled: bool,
        locked_out: bool,
    }

    /// The reader's answer.
    enum Answer {
        Network(Snapshot),
        Discovered(Result<Discovered, DiscoveryError>),
    }

    /// What the reader keeps from one job to the next: the doorbell its
    /// discovery session rings, the session once one opened, and the bundled
    /// table of hardware makers once it was read.
    struct Reader {
        doorbell: u64,
        discovery: Option<(Session<RtTransport>, Discovery)>,
        /// The open session, for the doorbell drain to match.
        open_session: Arc<OpenSession>,
        makers: Option<Result<DevIds<Vec<u8>>, String>>,
    }

    impl Reader {
        /// The bundle's OUI table, read and validated once and kept, or why
        /// it cannot be had.
        fn makers(&mut self) -> Result<&DevIds<Vec<u8>>, &str> {
            self.makers
                .get_or_insert_with(|| {
                    let bytes = tairix_devids::bundled::load(DbKind::Oui, APP_NAME)
                        .map_err(|err| err.to_string())?;
                    DevIds::parse(DbKind::Oui, bytes)
                        .map_err(|err| format!("the table of makers is damaged ({err})"))
                })
                .as_ref()
                .map_err(String::as_str)
        }

        fn discover(&mut self, named: &BTreeSet<IpAddr>) -> Result<Discovered, DiscoveryError> {
            if self.discovery.is_none() {
                let session = Session::open(RtTransport, self.doorbell)?;
                self.open_session.open(session.id());
                self.discovery = Some((session, Discovery::new()));
            }
            let Some((session, engine)) = self.discovery.as_mut() else {
                return Err(DiscoveryError::Unavailable);
            };
            let collected = (|| {
                while session.collect(&mut |entry| engine.apply(entry))? {}
                Ok(())
            })();
            if let Err(err) = collected {
                // A session the service has lost is opened afresh next time.
                self.discovery = None;
                self.open_session.close();
                return Err(err);
            }
            engine.reconcile(session, named, tairix_rt::clock_get());
            Ok(Discovered {
                services: engine.services(),
                names: engine.names(),
                truncated: engine.truncated(),
                refused: engine.refused().copied(),
                stalled: engine.stalled().copied(),
                unsettled: engine.stalled().is_some() || engine.deferred(),
                locked_out: engine.locked_out(),
            })
        }
    }

    fn read(reader: &mut Reader, job: &mut Job) -> Answer {
        match job {
            Job::Read => {
                let mut snapshot = Snapshot::read(&IpcTransport);
                match reader.makers() {
                    Ok(table) => snapshot.name_vendors(table),
                    Err(why) => snapshot.vendors = Err(String::from(why)),
                }
                Answer::Network(snapshot)
            }
            Job::Discover(named) => Answer::Discovered(reader.discover(named)),
        }
    }

    type ReadWorker = Worker<Reader, Job, Answer, tairix_util::defer::JobQueue<Job, Answer>>;

    /// One reverse DNS answer: `None` when the address has no name, or the
    /// refusal of the transport that would have asked.
    struct Named {
        addr: IpAddr,
        name: Result<Option<String>, Errno>,
    }

    /// Look `addr` up over the namer's one transport, opened on first use.
    fn name(transport: &mut Option<RtDnsTransport>, addr: &mut IpAddr) -> Named {
        let name = match transport {
            Some(transport) => Ok(transport.reverse_name(*addr)),
            None => RtDnsTransport::open().map(|mut opened| {
                let name = opened.reverse_name(*addr);
                *transport = Some(opened);
                name
            }),
        };
        Named { addr: *addr, name }
    }

    type NameWorker =
        Worker<Option<RtDnsTransport>, IpAddr, Named, tairix_util::defer::JobQueue<IpAddr, Named>>;

    /// Delivered echo frames the prober's port holds before it drains them.
    const ECHO_DELIVER_CAPACITY: usize = 64;
    /// Delivered stream events the prober's port holds before it drains them.
    const STREAM_DELIVER_CAPACITY: usize = 64;
    /// The prober's own wait-set tokens: the two delivery ports it parks on.
    const PROBE_ECHO_TOKEN: u64 = 1;
    const PROBE_STREAM_TOKEN: u64 = 2;

    /// The sockets and buffers one prober thread owns across campaigns: one
    /// unconnected echo socket per family, a pool of stream sockets for the
    /// well-known-port check, and the wait-set it parks on. Only this thread
    /// ever blocks on them, so the window stays responsive while it sweeps.
    struct Prober {
        echo_port: PrivatePort,
        stream_port: PrivatePort,
        set: WaitSet,
        echo_v4: Option<SocketId>,
        echo_v6: Option<SocketId>,
        /// Each outstanding connect's socket and the engine token it answers.
        connects: Vec<(SocketId, u64)>,
        echo_buf: Vec<u8>,
        stream_buf: Vec<u8>,
        /// The next echo sequence, carried across campaigns so a late reply to
        /// one cannot be read as an answer in the next.
        next_sequence: u16,
    }

    impl Prober {
        /// The echo socket for `family`, opened and kept on first use.
        fn echo_socket(&mut self, family: NetAddrFamily) -> Option<SocketId> {
            let slot = match family {
                NetAddrFamily::V4 => &mut self.echo_v4,
                NetAddrFamily::V6 => &mut self.echo_v6,
            };
            if let Some(socket) = *slot {
                return Some(socket);
            }
            let opened = tairix_rt::net::icmp_echo_socket(family, self.echo_port.id()).ok();
            *slot = opened;
            opened
        }

        fn send_echo(&mut self, dest: IpAddr, sequence: u16, probe: &mut Probe) {
            let (family, bytes) = address_parts(dest);
            let Some(socket) = self.echo_socket(family) else {
                probe.fail_echo(sequence);
                return;
            };
            let to = SocketAddr {
                family,
                addr: bytes,
                port: 0,
            };
            if tairix_rt::net::send_echo(socket, Some(to), sequence, None, &[]).is_err() {
                probe.fail_echo(sequence);
            }
        }

        fn open_connect(&mut self, target: IpAddr, port: u16, token: u64, probe: &mut Probe) {
            let (family, bytes) = address_parts(target);
            let Ok(socket) = tairix_rt::net::stream_socket(family, self.stream_port.id()) else {
                probe.fail_connect(token);
                return;
            };
            let to = SocketAddr {
                family,
                addr: bytes,
                port,
            };
            if tairix_rt::net::connect(socket, to).is_err() {
                let _ = tairix_rt::net::close(socket);
                probe.fail_connect(token);
                return;
            }
            self.connects.push((socket, token));
        }

        fn close_connect(&mut self, token: u64) {
            if let Some(socket) = self.take_connect_by_token(token) {
                let _ = tairix_rt::net::close(socket);
            }
        }

        fn take_connect_by_token(&mut self, token: u64) -> Option<SocketId> {
            let at = self.connects.iter().position(|(_, held)| *held == token)?;
            Some(self.connects.swap_remove(at).0)
        }

        fn take_connect_by_socket(&mut self, socket: SocketId) -> Option<u64> {
            let at = self.connects.iter().position(|(held, _)| *held == socket)?;
            Some(self.connects.swap_remove(at).1)
        }

        fn wait_and_drain(&mut self, until_ns: u64, probe: &mut Probe) {
            let _ = self.set.wait_until(until_ns);
            self.drain_echo(probe);
            self.drain_stream(probe);
        }

        fn drain_echo(&mut self, probe: &mut Probe) {
            let port = self.echo_port.id();
            while let Ok(echo) = tairix_rt::net::recv_echo(port, &mut self.echo_buf) {
                let sequence = echo.sequence;
                let source = ip_from_parts(echo.source.family, echo.source.addr);
                let outcome = echo.outcome;
                let hop = echo.reply_hop_limit.map(core::num::NonZeroU8::get);
                probe.answer_echo(sequence, source, outcome, hop, tairix_rt::clock_get());
            }
        }

        fn drain_stream(&mut self, probe: &mut Probe) {
            let port = self.stream_port.id();
            loop {
                // Only Copy fields are kept, so the borrow of the buffer ends
                // before the socket table is touched below.
                let verdict = match tairix_rt::net::stream_recv(port, &mut self.stream_buf) {
                    Ok(SocketStreamEvent::Connected { socket }) => {
                        Some((socket, ConnectOutcome::Open))
                    }
                    Ok(SocketStreamEvent::Closed { socket, reason }) => Some((
                        socket,
                        match reason {
                            StreamCloseReason::Refused => ConnectOutcome::Refused,
                            _ => ConnectOutcome::Inconclusive,
                        },
                    )),
                    Ok(_) => continue,
                    Err(_) => break,
                };
                let Some((socket, outcome)) = verdict else {
                    break;
                };
                if let Some(token) = self.take_connect_by_socket(socket) {
                    probe.answer_connect(token, outcome, tairix_rt::clock_get());
                    let _ = tairix_rt::net::close(socket);
                }
            }
        }
    }

    type ProbeWorker = Worker<Prober, ProbeCampaign, Reachability>;

    /// Run one whole campaign on the prober thread, blocking only here, and
    /// answer what it found. An empty or passive campaign is a no-op.
    fn run_campaign(prober: &mut Prober, campaign: &mut ProbeCampaign) -> Reachability {
        let campaign = core::mem::take(campaign);
        if !campaign.profile.probe || campaign.targets.is_empty() {
            return Reachability::new();
        }
        let count = u64::try_from(campaign.targets.len()).unwrap_or(u64::MAX);
        audit(
            PROBE_STARTED,
            "netmap: reachability probe started",
            &[
                Field {
                    key: "posture",
                    value: FieldValue::Str(campaign.profile.label()),
                },
                Field {
                    key: "targets",
                    value: FieldValue::UnsignedInt(count),
                },
            ],
        );
        let first = prober.next_sequence;
        prober.next_sequence =
            first.wrapping_add(u16::try_from(campaign.targets.len()).unwrap_or(0));
        let mut probe = Probe::new(campaign, first);
        loop {
            match probe.poll(tairix_rt::clock_get()) {
                ProbeAction::SendEcho { dest, sequence } => {
                    prober.send_echo(dest, sequence, &mut probe);
                }
                ProbeAction::OpenConnect {
                    target,
                    port,
                    token,
                } => prober.open_connect(target, port, token, &mut probe),
                ProbeAction::CloseConnect { token } => prober.close_connect(token),
                ProbeAction::Wait { until_ns } => prober.wait_and_drain(until_ns, &mut probe),
                ProbeAction::Done => break,
            }
        }
        let reach = probe.into_reachability();
        audit(
            PROBE_FINISHED,
            PROBE_FINISHED_MESSAGE,
            &[Field {
                key: "live",
                value: FieldValue::UnsignedInt(
                    u64::try_from(reach.live_count()).unwrap_or(u64::MAX),
                ),
            }],
        );
        reach
    }

    /// Emit one audit record best-effort.
    fn audit(id: EventId, message: &str, fields: &[Field<'_>]) {
        tairix_log::log(
            &LOG_SINK,
            &Event {
                level: Level::Info,
                id,
                message,
                fields,
            },
        );
    }

    /// The production event source: the app's own authenticated mailbox,
    /// parked on with the worker's wake until the earlier of the refresh and
    /// the next animation frame.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        reader: &'a ReadWorker,
        namer: &'a NameWorker,
        prober: Option<&'a ProbeWorker>,
        publisher: Option<&'a Publisher>,
        doorbell: Option<&'a PrivatePort>,
        open_session: &'a OpenSession,
        deadlines: &'a Deadlines,
        desktop_moved: &'a Cell<bool>,
        rung: &'a Cell<bool>,
    }

    /// The two one-shot deadlines the loop parks against, each `None` when
    /// unarmed: the next re-read of the network, and the next animation frame.
    #[derive(Default)]
    struct Deadlines {
        refresh_ns: Cell<Option<u64>>,
        frame_ns: Cell<Option<u64>>,
    }

    impl Deadlines {
        fn earliest(&self) -> Option<u64> {
            match (self.refresh_ns.get(), self.frame_ns.get()) {
                (Some(refresh), Some(frame)) => Some(refresh.min(frame)),
                (refresh, frame) => refresh.or(frame),
            }
        }

        /// Whether `cell`'s deadline has come at `now`, disarming it if so.
        fn take_due(cell: &Cell<Option<u64>>, now: u64) -> bool {
            let due = cell.get().is_some_and(|deadline| now >= deadline);
            if due {
                cell.set(None);
            }
            due
        }
    }

    impl RtEventSource<'_> {
        /// Drain the doorbell port, noting whether the discovery service rang
        /// the open session: anything else posted there — another process's
        /// message, a ring for a session already given up — is dropped
        /// unread, so it can make the map collect nothing.
        fn drain_doorbell(&self) {
            let Some(doorbell) = self.doorbell else {
                return;
            };
            let mut frame = [0u8; DOORBELL_LEN];
            let mut sender = [0u8; ORIGIN_WIRE_LEN];
            while let Ok(len) = tairix_rt::ipc_recv(doorbell.id(), &mut frame, &mut sender) {
                if self.open_session.rung_by(&sender, &frame[..len]) {
                    self.rung.set(true);
                }
            }
        }
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let woken = match self.deadlines.earliest() {
                Some(deadline) => match app::park_until(self.set, deadline)? {
                    Some(woken) => woken,
                    None => return Ok(Parked::Interrupted),
                },
                None => app::park(self.set)?,
            };
            match woken {
                // Each readiness is a level, so an undrained one would turn the
                // park into a spin.
                Wake::App(READER_TOKEN) => {
                    self.reader.wake().drain();
                    Ok(Parked::Interrupted)
                }
                Wake::App(NAMER_TOKEN) => {
                    self.namer.wake().drain();
                    Ok(Parked::Interrupted)
                }
                Wake::App(PROBER_TOKEN) => {
                    if let Some(prober) = self.prober {
                        prober.wake().drain();
                    }
                    Ok(Parked::Interrupted)
                }
                Wake::App(PUBLISH_TOKEN) => {
                    if let Some(publisher) = self.publisher {
                        publisher.wake().drain();
                    }
                    Ok(Parked::Interrupted)
                }
                Wake::App(DOORBELL_TOKEN) => {
                    self.drain_doorbell();
                    Ok(Parked::Interrupted)
                }
                Wake::PressureChanged => {
                    tairix_font::trim_glyph_cache();
                    Ok(Parked::Served)
                }
                Wake::DesktopChanged => {
                    self.desktop_moved.set(true);
                    Ok(Parked::Interrupted)
                }
                Wake::Event | Wake::PressureUnchanged | Wake::App(_) => Ok(Parked::Served),
            }
        }
    }

    /// Declare the icon-bar presence: the information row and *Quit*, the
    /// session raising the window or asking for one. A refusal is reported and
    /// the map carries on without a slot of its own.
    fn declare_app_bar(client: &mut WindowClient<app::RtWindowTransport>, endpoint: u64) {
        let declared = tairix_window::info_and_quit(endpoint, AppBarClick::RaiseOrOpen)
            .map_err(|err| alloc::format!("{err:?}"))
            .and_then(|bar| {
                client
                    .set_app_bar(&bar)
                    .map_err(|err| alloc::format!("{err}"))
            });
        if let Err(reason) = declared {
            app::report(
                APP_NAME,
                format_args!("no icon-bar presence ({reason}); carrying on without one"),
            );
        }
    }

    /// What one delivered event concluded.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum Acted {
        Idle,
        Changed,
        Close,
        Open,
        Quit,
    }

    /// The program's whole state.
    struct NetMap {
        window: AppWindow,
        desktop: Desktop,
        themes: ThemeRegistry,
        view: MapView,
        inventory: Inventory,
        mode: DisplayMode,
        /// Whether the network has been read at least once, which the window's
        /// title reflects: a window opened before the first read says it is
        /// still gathering.
        loaded: bool,
        /// What the last read could not see, stated on the map.
        read_notices: Vec<String>,
        /// Why the advertised services are missing or incomplete, if they are.
        discovery_notice: Option<String>,
        /// Why hosts are not named by DNS, if they cannot be.
        naming_notice: Option<String>,
        /// Why reachability is not actively probed, if it is not.
        probe_notice: Option<String>,
        /// The context menu asked for and not yet answered, by its open id.
        menu: Option<(u64, Box<NodeMenu>)>,
        /// The tooltip last declared over the window.
        tip: DeclaredTip,
        /// The probe posture, published off the loop and read at start.
        choice: Publication<ProbeSettings>,
        /// The settings window, while it is open.
        settings: Option<SettingsPane>,
        /// The event endpoint the settings window opens on.
        event_endpoint: u64,
        /// The session serving the map's window, which must answer the settings
        /// window too; `None` until the map's window first opens.
        server: Option<ProcId>,
    }

    impl NetMap {
        fn canvas(&self) -> Rect {
            Rect::new(0, 0, self.mode.width_px, self.mode.height_px)
        }

        fn open(&mut self, event_endpoint: u64) -> Result<ProcId, i32> {
            let scale = self.desktop.scale();
            let sizing = win_sizing(
                scale.scale_length(MIN_WIDTH),
                scale.scale_length(MIN_HEIGHT),
            );
            let title = if self.loaded {
                APP_TITLE
            } else {
                LOADING_TITLE
            };
            let server = self
                .window
                .open(event_endpoint, &self.mode, title, sizing)
                .map_err(|err| app::fail(APP_NAME, err.code(), err))?;
            self.tip = DeclaredTip::new();
            self.view.resize(self.canvas());
            let whole = DamageRect::full(&self.mode);
            if self.present(tairix_rt::clock_get(), whole).is_err() {
                let _ = self.window.close();
                return Err(app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused"));
            }
            self.server = Some(server);
            Ok(server)
        }

        /// Open the probe-settings window, on the posture now in force. A
        /// refusal is reported and the map carries on; a window already open is
        /// left as it is.
        fn open_settings(&mut self) {
            if self.settings.is_some() {
                return;
            }
            let Some(server) = self.server else {
                return;
            };
            let posture = self.choice.live().posture;
            match SettingsPane::open(
                self.window.client(),
                self.event_endpoint,
                server,
                posture,
                &self.desktop,
                &self.themes,
            ) {
                Ok(pane) => self.settings = Some(pane),
                Err(reason) => app::report(
                    APP_NAME,
                    format_args!("probe settings not shown ({reason})"),
                ),
            }
        }

        /// Close the settings window if it is open.
        fn close_settings(&mut self) {
            if let Some(pane) = self.settings.take() {
                pane.close(self.window.client());
            }
        }

        /// Route an event the settings window owns to its pane, answering what
        /// it asked and owing whatever moved.
        fn apply_settings(&mut self, event: &WindowEvent, now: u64) -> Option<SettingsRequest> {
            let scale = self.desktop.scale();
            let theme = self.themes.active();
            let pane = self.settings.as_mut()?;
            let mut sink = Region::new();
            let request = match *event {
                WindowEvent::Pointer {
                    x,
                    y,
                    action,
                    modifiers,
                    ..
                } => {
                    let held = key_input_event(KeyInput::ModifiersChanged { modifiers });
                    let mut request = None;
                    for input in core::iter::once(held)
                        .chain(pointer_input_events(action, pointer_point(x, y)))
                    {
                        if let Some(asked) =
                            pane.content.on_pointer(&input, scale, theme, &mut sink)
                        {
                            request = Some(asked);
                        }
                    }
                    request
                }
                WindowEvent::Key {
                    key: pressed @ KeyInput::Pressed { .. },
                    ..
                } => Keystroke::pressed(key_input_event(pressed), now)
                    .and_then(|stroke| pane.content.on_key(stroke, scale, theme, &mut sink)),
                WindowEvent::CloseRequested { .. } => Some(SettingsRequest::Close),
                WindowEvent::ContentReleased { .. } => {
                    pane.pane.release_frames();
                    None
                }
                _ => None,
            };
            pane.owe(sink);
            request
        }

        /// Reflect the durable posture the store settled on in the open window.
        fn adopt_posture(&mut self, posture: Posture) {
            if let Some(pane) = self.settings.as_mut() {
                let mut sink = Region::new();
                pane.content.adopt(posture, &mut sink);
                pane.owe(sink);
            }
        }

        /// Re-fit the settings window for the desktop as it stands now, so a
        /// scale or theme change while it is open does not leave it rendering at
        /// the wrong size.
        fn refit_settings(&mut self) {
            let Some(pane) = self.settings.as_mut() else {
                return;
            };
            let wanted = pane.content.fit(self.desktop.scale(), self.themes.active());
            if wanted != (pane.surface.width(), pane.surface.height()) {
                let _ = pane.pane.resize_with(
                    self.window.client(),
                    &app::mode_for(wanted.0, wanted.1),
                    &mut pane.surface,
                );
            }
            pane.content
                .set_extent((pane.surface.width(), pane.surface.height()));
            pane.owed = Repaint::Whole;
        }

        /// Paint the settings window if it owes anything.
        fn paint_settings(&mut self) -> Result<(), Errno> {
            let (Some(pane), desktop, themes) =
                (self.settings.as_mut(), &self.desktop, &self.themes)
            else {
                return Ok(());
            };
            pane.paint(self.window.client(), desktop, themes)
        }

        /// Draw `area` of the map as it stands at `now_ns`, and present it.
        fn present(&mut self, now_ns: u64, area: DamageRect) -> Result<(), Errno> {
            let scale = self.desktop.scale();
            let theme = self.themes.active();
            let view = &mut self.view;
            self.window.present(area, |surface| {
                view.render(surface, scale, theme, now_ns);
            })
        }

        /// What a frame that only moves services repaints, or `None` with
        /// nothing of it in the window.
        fn frame_area(&mut self) -> Option<DamageRect> {
            let rect = self
                .view
                .frame_damage(self.desktop.scale(), self.themes.active())?;
            damage_in(&self.mode, rect)
        }

        /// Ask the desktop for the menu the view built, at the press that
        /// asked for it. The answer arrives as one `MenuClosed` naming the id
        /// minted here; a refusal is an answer, reported, and the map draws no
        /// menu of its own.
        fn open_menu(&mut self, asked: Result<Box<NodeMenu>, Errno>) {
            let menu = match asked {
                Ok(menu) => menu,
                Err(err) => {
                    app::report(APP_NAME, format_args!("menu not built ({err}); not shown"));
                    return;
                }
            };
            let Some(window_id) = self.window.window_id() else {
                return;
            };
            let at = menu.anchor();
            let anchor = match WindowRegion::new(at.x, at.y, 0, 0) {
                Ok(anchor) => anchor,
                Err(err) => {
                    app::report(
                        APP_NAME,
                        format_args!("menu anchor refused ({err}); not shown"),
                    );
                    return;
                }
            };
            match self
                .window
                .client()
                .open_menu(window_id, anchor, menu.model())
            {
                Ok(open_id) => self.menu = Some((open_id, menu)),
                Err(err) => app::report(APP_NAME, format_args!("menu refused ({err}); not shown")),
            }
        }

        /// Act on the desktop's one answer to the menu opened as `open_id`,
        /// answering whether the picture changed. An answer naming any other
        /// open settles a gesture already over, so acting on it would run a
        /// stale command.
        fn menu_closed(&mut self, open_id: u64, outcome: MenuOutcome, now_ns: u64) -> bool {
            let Some((asked, menu)) = self.menu.take() else {
                return false;
            };
            if asked != open_id {
                self.menu = Some((asked, menu));
                return false;
            }
            match outcome {
                // A row id this map never declared names no command.
                MenuOutcome::Chosen(item) => match menu.command(item) {
                    Some(Command::Steer(steer)) => self.view.steer(steer, now_ns),
                    Some(Command::Copy(text)) => {
                        self.copy(text);
                        false
                    }
                    Some(Command::OpenSettings) => {
                        self.open_settings();
                        false
                    }
                    None => false,
                },
                // The menu declares no quick-entry field.
                MenuOutcome::Entered(_) | MenuOutcome::Dismissed => false,
                MenuOutcome::Refused(reason) => {
                    let why = reason.describe();
                    app::report(APP_NAME, format_args!("no menu was shown: {why}"));
                    false
                }
            }
        }

        /// Put `text` on the desktop clipboard from the map's window.
        fn copy(&mut self, text: &str) {
            let Some(window_id) = self.window.window_id() else {
                return;
            };
            let put = clipboard::put(
                self.window.client(),
                window_id,
                ClipboardKind::Text,
                text.as_bytes(),
            );
            if let Err(err) = put {
                app::report(APP_NAME, format_args!("nothing was copied ({err})"));
            }
        }

        /// Declare the tooltip for the plate under the pointer, or withdraw
        /// the last one.
        fn declare_tip(&mut self, now_ns: u64) {
            let Some(window_id) = self.window.window_id() else {
                return;
            };
            let wanted = self.view.tip(self.desktop.scale(), now_ns);
            self.tip.declare(
                self.window.client(),
                window_id,
                wanted
                    .as_ref()
                    .map(|(region, line)| (*region, line.as_str())),
            );
        }

        fn adopt(&mut self, answer: Answer) {
            match answer {
                Answer::Network(snapshot) => {
                    self.read_notices = snapshot.apply(&mut self.inventory);
                }
                Answer::Discovered(Ok(discovered)) => {
                    self.inventory.replace_services(discovered.services);
                    self.inventory.replace_segment_names(discovered.names);
                    self.discovery_notice = if let Some(refusal) =
                        discovered.refused.filter(|_| discovered.locked_out)
                    {
                        Some(format!("Service discovery is not permitted: {refusal}"))
                    } else if let Some(refusal) = discovered.refused {
                        Some(format!("Some advertised services are not shown: {refusal}"))
                    } else if let Some(stall) = discovered.stalled {
                        Some(format!(
                            "Some advertised services are not shown yet: {stall}"
                        ))
                    } else if discovered.truncated {
                        Some(String::from(
                            "Not every advertised service is shown: the map's own bounds were reached",
                        ))
                    } else {
                        None
                    };
                }
                Answer::Discovered(Err(err)) => {
                    self.discovery_notice = Some(format!("Service discovery unavailable: {err}"));
                }
            }
        }

        /// Project the inventory into the map shown.
        fn rebuild(&mut self) {
            let mut map = self.inventory.build(tairix_rt::clock_get());
            layout(&mut map);
            self.view.set_map(map);
            let mut notices = self.read_notices.clone();
            notices.extend(self.discovery_notice.clone());
            notices.extend(self.naming_notice.clone());
            notices.extend(self.probe_notice.clone());
            self.view.set_notices(notices);
        }

        /// Feed the input `event` carries to the view, opening any menu it
        /// asks for, and answer whether the picture changed; `None` for an
        /// event that carries no input.
        fn route_input(&mut self, event: &WindowEvent, now_ns: u64) -> Option<bool> {
            let scale = self.desktop.scale();
            let held = |modifiers| key_input_event(KeyInput::ModifiersChanged { modifiers });
            let mut asked = None;
            let view = &mut self.view;
            let mut route = |inputs: &mut dyn Iterator<Item = InputEvent>| {
                let mut changed = false;
                for input in inputs {
                    match view.on_input(&input, scale, now_ns) {
                        Reaction::Idle => {}
                        Reaction::Repaint => changed = true,
                        Reaction::Menu(menu) => asked = Some(menu),
                    }
                }
                changed
            };
            let changed = match *event {
                WindowEvent::Key { key, .. } => route(&mut core::iter::once(key_input_event(key))),
                WindowEvent::Pointer {
                    x,
                    y,
                    action,
                    modifiers,
                    ..
                } => route(
                    &mut core::iter::once(held(modifiers))
                        .chain(pointer_input_events(action, pointer_point(x, y))),
                ),
                WindowEvent::Scrolled {
                    x,
                    y,
                    dx,
                    dy,
                    modifiers,
                    ..
                } => route(
                    &mut core::iter::once(held(modifiers)).chain(scroll_input_events(
                        pointer_point(x, y),
                        dx,
                        dy,
                    )),
                ),
                WindowEvent::Pinch {
                    x,
                    y,
                    phase,
                    scale: ratio,
                    ..
                } => route(&mut pinch_input_events(pointer_point(x, y), phase, ratio)),
                _ => return None,
            };
            if let Some(menu) = asked {
                self.open_menu(menu);
            }
            Some(changed)
        }

        fn apply(&mut self, event: &WindowEvent, now_ns: u64) -> Acted {
            let changed = match *event {
                WindowEvent::CloseRequested { .. } => return Acted::Close,
                WindowEvent::AppBarMenu { item } if tairix_window::is_quit(item) => {
                    return Acted::Quit
                }
                WindowEvent::AppBarDefault => return Acted::Open,
                WindowEvent::Key { .. }
                | WindowEvent::Pointer { .. }
                | WindowEvent::Scrolled { .. }
                | WindowEvent::Pinch { .. } => self.route_input(event, now_ns).unwrap_or(false),
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => {
                    let mode = app::mode_for(width_px, height_px);
                    match self.window.resize(mode) {
                        app::Fitted::Adopted => {
                            self.mode = mode;
                            self.view.resize(self.canvas());
                        }
                        app::Fitted::Deferred => {}
                        app::Fitted::Kept => app::report(APP_NAME, app::SIZE_KEPT),
                    }
                    true
                }
                WindowEvent::RedrawRequested { .. } => true,
                WindowEvent::ContentReleased { .. } => {
                    self.window.release_frames();
                    false
                }
                WindowEvent::MenuClosed {
                    open_id, outcome, ..
                } => self.menu_closed(open_id, outcome, now_ns),
                // The map opens no documents, pickers, drags or layer
                // surfaces, so none of their answers can arrive for it.
                WindowEvent::AlternateCloseRequested { .. }
                | WindowEvent::AppBarMenu { .. }
                | WindowEvent::TerrainChanged { .. }
                | WindowEvent::AlertClosed { .. }
                | WindowEvent::Authorised { .. }
                | WindowEvent::LayerPointer { .. }
                | WindowEvent::ToolMoved { .. }
                | WindowEvent::Focus { .. }
                | WindowEvent::Minimized { .. }
                | WindowEvent::FilePicked { .. }
                | WindowEvent::PickCancelled { .. }
                | WindowEvent::FolderPicked { .. }
                | WindowEvent::DragOver { .. }
                | WindowEvent::DragEnded { .. }
                | WindowEvent::PreviewRendered { .. }
                | WindowEvent::OpenRequested => false,
            };
            if changed {
                Acted::Changed
            } else {
                Acted::Idle
            }
        }
    }

    /// The work the map has asked of its workers and not yet taken back.
    #[derive(Default)]
    struct Outstanding {
        /// A read of the network's state.
        read: bool,
        collect: Collect,
        /// The link-local addresses the last collect asked to be named.
        asked_names: BTreeSet<IpAddr>,
        /// Addresses handed to the namer and not yet answered.
        naming: BTreeSet<IpAddr>,
        /// An answer is already waiting: with no worker thread the job ran on
        /// this one, and no wake will come to say so.
        landed: bool,
    }

    impl Outstanding {
        fn ask_read(&mut self, reader: &ReadWorker) {
            if self.read {
                return;
            }
            if let Ok(landed) = reader.submit(Job::Read) {
                self.read = true;
                self.landed |= landed;
            }
        }

        fn ask_discover(&mut self, reader: &ReadWorker, inventory: &Inventory) {
            if self.collect != Collect::Due {
                return;
            }
            let named = inventory.link_local_hosts();
            if let Ok(landed) = reader.submit(Job::Discover(named.clone())) {
                self.collect = Collect::Asked { again: false };
                self.asked_names = named;
                self.landed |= landed;
            }
        }

        /// Hand the namer each routable host not yet named or being named,
        /// while it has room.
        fn ask_names(&mut self, namer: &NameWorker, inventory: &Inventory) {
            for addr in inventory.unnamed_hosts() {
                if self.naming.contains(&addr) {
                    continue;
                }
                let Ok(landed) = namer.submit(addr) else {
                    return;
                };
                self.naming.insert(addr);
                self.landed |= landed;
            }
        }
    }

    /// The workers the loop hands work to.
    struct Workers<'a> {
        reader: &'a ReadWorker,
        namer: &'a NameWorker,
        /// The prober, when probing is on (`CAP_NET_RAW` granted and a thread of
        /// its own); the posture it probes at is the published choice.
        prober: Option<&'a ProbeWorker>,
        /// The settings-store worker the posture is published through.
        publisher: Option<&'a Publisher>,
        /// Where collects start: off unless the reader can make them.
        collect: Collect,
        /// Whether the namer has a thread of its own, and so names hosts.
        naming: bool,
    }

    /// Which workers run on a thread of their own: one without runs its jobs
    /// on the event loop, which a blocking job must never stall.
    #[derive(Clone, Copy)]
    struct Threaded {
        reader: bool,
        namer: bool,
    }

    /// Take every settings answer the store has given back, adopt the posture
    /// it settled on, and submit the next owed write. Shared by the loop's
    /// wake path and the inline write a settle does when there is no worker.
    fn adopt_settings(app_state: &mut NetMap, publisher: &Publisher) {
        let mut refusals = Vec::new();
        let mut adopted = false;
        while let Some(answer) = publisher.collect() {
            adopted = true;
            let owed = app_state.choice.adopt(answer, &mut refusals);
            if !owed.is_some_and(|job| publisher.submit(job)) {
                break;
            }
        }
        for refusal in refusals {
            app::report(APP_NAME, format_args!("settings not saved ({refusal:?})"));
        }
        if adopted {
            let posture = app_state.choice.adopted().posture;
            app_state.adopt_posture(posture);
        }
    }

    /// The chooser settled on `posture`: show it live, write it off the loop,
    /// and probe at it at once (a passive posture probes nothing).
    fn settle_posture(
        app_state: &mut NetMap,
        prober: Option<&ProbeWorker>,
        publisher: Option<&Publisher>,
        posture: Posture,
    ) {
        let was = *app_state.choice.live();
        app_state.choice.edit(&was, &ProbeSettings { posture });
        if let Some(publisher) = publisher {
            if let Some(job) = app_state.choice.settle() {
                if publisher.submit(job) {
                    adopt_settings(app_state, publisher);
                }
            }
        }
        if let Some(prober) = prober {
            let profile = posture.profile();
            let targets = app_state.inventory.probe_targets(&profile);
            if !targets.is_empty() {
                prober.submit(ProbeCampaign { targets, profile });
            }
        }
    }

    /// The event loop: wait, drain the burst, then present at most once.
    #[allow(
        clippy::too_many_lines,
        reason = "one loop drains events, adopts every worker's answers and paints once"
    )]
    fn run(
        app_state: &mut NetMap,
        workers: &Workers<'_>,
        event_endpoint: u64,
        deadlines: &Deadlines,
        desktop_moved: &Cell<bool>,
        rung: &Cell<bool>,
        mut events: WindowEvents<RtEventSource<'_>>,
    ) -> i32 {
        let mut outstanding = Outstanding {
            collect: workers.collect,
            ..Outstanding::default()
        };
        outstanding.ask_read(workers.reader);
        outstanding.ask_discover(workers.reader, &app_state.inventory);
        let mut burst: Vec<WindowEvent> = Vec::new();
        loop {
            let first = if core::mem::take(&mut outstanding.landed) {
                None
            } else {
                match events.wait(app_state.window.client()) {
                    Ok(event) => event,
                    Err(EventError::Undecodable(_)) => None,
                    Err(EventError::Refused(refused)) => {
                        app::report(APP_NAME, refused);
                        None
                    }
                    Err(EventError::Mailbox(_)) => {
                        return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "event channel lost")
                    }
                }
            };
            burst.clear();
            burst.extend(first);
            loop {
                match events.try_wait(app_state.window.client()) {
                    Ok(Some(event)) => burst.push(event),
                    Ok(None) | Err(EventError::Undecodable(_)) => break,
                    Err(EventError::Refused(refused)) => app::report(APP_NAME, refused),
                    Err(EventError::Mailbox(_)) => {
                        return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "event channel lost")
                    }
                }
            }

            let now = tairix_rt::clock_get();
            let mut repaint = false;
            if desktop_moved.replace(false) {
                match app::adopt_desktop(&mut app_state.desktop, &mut app_state.themes) {
                    Ok(changed) => {
                        app_state
                            .view
                            .set_motion(app_state.themes.active().motion());
                        app_state.refit_settings();
                        repaint |= changed;
                    }
                    Err(err) => {
                        app::report(APP_NAME, format_args!("desktop change refused: {err}"));
                    }
                }
            }
            if rung.replace(false) {
                outstanding.collect.moved();
            }
            let mut moved = false;
            let mut read_landed = false;
            let mut retitle = false;
            workers.reader.collect_landed(|answer| {
                match answer {
                    Answer::Network(_) => {
                        outstanding.read = false;
                        read_landed = true;
                        if app_state.window.is_open() {
                            deadlines
                                .refresh_ns
                                .set(Some(now.saturating_add(REFRESH_NS)));
                        }
                    }
                    Answer::Discovered(ref discovered) => {
                        outstanding
                            .collect
                            .answered(discovered.as_ref().map(|found| !found.unsettled));
                    }
                }
                app_state.adopt(answer);
                moved = true;
            });
            workers.namer.collect_landed(|named| {
                outstanding.naming.remove(&named.addr);
                // A refused transport answers nothing about the address, so
                // it is asked again after the next read.
                app_state.naming_notice = match named.name {
                    Ok(name) => {
                        app_state.inventory.record_dns_name(named.addr, name);
                        None
                    }
                    Err(err) => Some(format!("Hosts are not named by DNS: {err}")),
                };
                moved = true;
            });
            if let Some(prober) = workers.prober {
                while let Some(reachability) = prober.collect() {
                    app_state.inventory.record_reachability(reachability);
                    moved = true;
                }
            }
            if let Some(publisher) = workers.publisher {
                adopt_settings(app_state, publisher);
            }
            if moved {
                app_state.rebuild();
                repaint = true;
                if app_state.inventory.link_local_hosts() != outstanding.asked_names {
                    outstanding.collect.moved();
                }
            }
            // Only after a read, so a namer that keeps refusing is asked once
            // per refresh rather than on every answer it gives.
            if read_landed {
                outstanding.collect.read_landed();
                if workers.naming {
                    outstanding.ask_names(workers.namer, &app_state.inventory);
                }
                // A fresh read gives the campaign its latest targets at the
                // chosen posture; the latest-wins desk collapses a still-running
                // one, so this never piles up (the loop only submits, never
                // probes).
                if let Some(prober) = workers.prober {
                    let profile = app_state.choice.live().posture.profile();
                    let targets = app_state.inventory.probe_targets(&profile);
                    if !targets.is_empty() {
                        prober.submit(ProbeCampaign { targets, profile });
                    }
                }
                retitle = !app_state.loaded;
                app_state.loaded = true;
            }
            for event in &burst {
                // An event the settings window owns is the pane's; everything
                // else is the map's.
                let for_settings = event.window_id().is_some()
                    && event.window_id() == app_state.settings.as_ref().map(SettingsPane::id);
                if for_settings {
                    match app_state.apply_settings(event, now) {
                        Some(SettingsRequest::Settle(posture)) => {
                            settle_posture(app_state, workers.prober, workers.publisher, posture);
                        }
                        Some(SettingsRequest::Close) => app_state.close_settings(),
                        None => {}
                    }
                    continue;
                }
                match app_state.apply(event, now) {
                    Acted::Quit => return 0,
                    Acted::Close => {
                        app_state.close_settings();
                        let _ = app_state.window.close();
                        app_state.menu = None;
                        deadlines.refresh_ns.set(None);
                        deadlines.frame_ns.set(None);
                    }
                    Acted::Open => {
                        if app_state.open(event_endpoint).is_ok() {
                            outstanding.ask_read(workers.reader);
                        }
                    }
                    Acted::Changed => repaint = true,
                    Acted::Idle => {}
                }
            }
            if Deadlines::take_due(&deadlines.refresh_ns, now) {
                outstanding.ask_read(workers.reader);
            }
            let frame_due = Deadlines::take_due(&deadlines.frame_ns, now);
            outstanding.ask_discover(workers.reader, &app_state.inventory);
            // A frame that only moves services repaints what they can reach;
            // anything else may have moved the whole map.
            let area = if repaint {
                Some(DamageRect::full(&app_state.mode))
            } else if frame_due {
                app_state.frame_area()
            } else {
                None
            };
            if let Some(area) = area {
                if app_state.present(now, area).is_err() {
                    return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
                }
            } else if frame_due {
                // Nothing that moves is in sight, so nothing is drawn, but the
                // movement still runs its course.
                app_state.view.settle(now);
            }
            if (area.is_some() || frame_due) && app_state.window.is_open() {
                let next = app_state.view.next_frame_in(now);
                deadlines
                    .frame_ns
                    .set(next.map(|wait| now.saturating_add(wait)));
            }
            if app_state.paint_settings().is_err() {
                app::report(
                    APP_NAME,
                    "the settings window could not be drawn; closing it",
                );
                app_state.close_settings();
            }
            app_state.declare_tip(now);
            // Only once the frame carrying the first read is presented: the
            // session serves a window's requests in order, so the new title
            // reaches the screen with that frame rather than over the empty one.
            if retitle {
                if let Err(err) = app_state.window.set_title(APP_TITLE) {
                    app::report(APP_NAME, format_args!("window title not set ({err})"));
                }
            }
        }
    }

    /// The doorbell the discovery service rings when its answers move, a
    /// member of the event wait-set. A map that cannot have one still draws
    /// everything else, and the notice says why its services are missing.
    fn doorbell(set: u64) -> (Option<PrivatePort>, Option<String>) {
        let doorbell = PrivatePort::bind(DOORBELL_LEN, DOORBELL_CAPACITY).and_then(|port| {
            match tairix_rt::waitset_ctl(
                set,
                tairix_abi::waitset::WaitSetOp::Add,
                WaitSourceKind::Port,
                port.id(),
                DOORBELL_TOKEN,
            ) {
                0 => Ok(port),
                refused => Err(Errno::from_syscall(refused)),
            }
        });
        match doorbell {
            Ok(port) => (Some(port), None),
            Err(err) => (
                None,
                Some(format!(
                    "Service discovery unavailable: no doorbell ({err})"
                )),
            ),
        }
    }

    /// The reader, whose discovery session rings `doorbell` and is published
    /// in `open_session`, and the namer, each started on a thread of its own
    /// where one can be had and woken through the event wait-set, with which
    /// of them got one.
    ///
    /// # Errors
    ///
    /// The exit code, once the failure is stated.
    fn start_workers(
        set: u64,
        doorbell: u64,
        open_session: Arc<OpenSession>,
    ) -> Result<(Arc<ReadWorker>, Arc<NameWorker>, Threaded), i32> {
        let reader = Reader {
            doorbell,
            discovery: None,
            open_session,
            makers: None,
        };
        let (Ok(reader), Ok(namer)) = (
            Worker::queued(
                read,
                reader,
                tairix_rt::sync::WorkerWake::create(),
                READER_QUEUE,
            ),
            Worker::queued(
                name,
                None,
                tairix_rt::sync::WorkerWake::create(),
                NAMER_QUEUE,
            ),
        ) else {
            return Err(app::fail(
                APP_NAME,
                app::EXIT_NO_EVENTS,
                "no memory for the workers",
            ));
        };
        let (reader, namer) = (Arc::new(reader), Arc::new(namer));
        let mut threaded = Threaded {
            reader: true,
            namer: true,
        };
        for (started, role, has_thread) in [
            (
                Worker::start(&reader).map(drop),
                "reader",
                &mut threaded.reader,
            ),
            (
                Worker::start(&namer).map(drop),
                "namer",
                &mut threaded.namer,
            ),
        ] {
            if let Err(reason) = started {
                *has_thread = false;
                app::report(APP_NAME, format_args!("no {role} thread ({reason:?})"));
            }
        }
        for (wake, token, role) in [
            (reader.wake(), READER_TOKEN, "reader"),
            (namer.wake(), NAMER_TOKEN, "namer"),
        ] {
            if let Err(err) = app::watch_wake(set, wake, token) {
                return Err(app::fail(
                    APP_NAME,
                    app::EXIT_NO_EVENTS,
                    format_args!("{role} wake refused ({err})"),
                ));
            }
        }
        Ok((reader, namer, threaded))
    }

    /// Start the prober on a thread of its own, parking on its own echo and
    /// stream delivery ports. Probing needs `CAP_NET_RAW` and a thread of its
    /// own; without either it is off and the notice says why, so the map still
    /// draws everything else (fail loud, degrade gracefully).
    fn start_prober(set: u64) -> (Option<Arc<ProbeWorker>>, Option<String>) {
        let (Ok(echo_port), Ok(stream_port)) = (
            PrivatePort::bind(SocketEcho::MAX_WIRE_LEN, ECHO_DELIVER_CAPACITY),
            PrivatePort::bind(SocketStreamEvent::MAX_WIRE_LEN, STREAM_DELIVER_CAPACITY),
        ) else {
            return (
                None,
                Some(String::from(
                    "Reachability probing is off: no delivery port",
                )),
            );
        };
        let Ok(waitset) = WaitSet::create() else {
            return (
                None,
                Some(String::from("Reachability probing is off: no wait-set")),
            );
        };
        let wired = waitset
            .add(WaitSourceKind::Port, echo_port.id(), PROBE_ECHO_TOKEN)
            .and_then(|()| waitset.add(WaitSourceKind::Port, stream_port.id(), PROBE_STREAM_TOKEN));
        if wired.is_err() {
            return (
                None,
                Some(String::from(
                    "Reachability probing is off: wait-set refused",
                )),
            );
        }
        // A throwaway socket is the CAP_NET_RAW check; the worker opens its own
        // lazily, so nothing is held across the fallible start-up below.
        match tairix_rt::net::icmp_echo_socket(NetAddrFamily::V4, echo_port.id()) {
            Ok(socket) => {
                let _ = tairix_rt::net::close(socket);
            }
            Err(err) => {
                audit(
                    PROBE_REFUSED,
                    "netmap: reachability probing refused",
                    &[Field {
                        key: "reason",
                        value: FieldValue::Str("raw network access"),
                    }],
                );
                return (
                    None,
                    Some(format!("Reachability probing needs CAP_NET_RAW ({err})")),
                );
            }
        }
        let prober = Prober {
            echo_port,
            stream_port,
            set: waitset,
            echo_v4: None,
            echo_v6: None,
            connects: Vec::new(),
            echo_buf: alloc::vec![0u8; SocketEcho::MAX_WIRE_LEN],
            stream_buf: alloc::vec![0u8; SocketStreamEvent::MAX_WIRE_LEN],
            next_sequence: 0,
        };
        let worker = Arc::new(Worker::new(
            run_campaign,
            prober,
            tairix_rt::sync::WorkerWake::create(),
        ));
        if Worker::start(&worker).is_err() {
            return (
                None,
                Some(String::from(
                    "Reachability probing is off: it needs a thread of its own",
                )),
            );
        }
        if app::watch_wake(set, worker.wake(), PROBER_TOKEN).is_err() {
            return (
                None,
                Some(String::from("Reachability probing is off: wake refused")),
            );
        }
        (Some(worker), None)
    }

    /// The worker the probe posture is written through, off the loop.
    type Publisher = Worker<(), PublishJob<ProbeSettings>, Result<Published<ProbeSettings>, Errno>>;

    /// Write one settings job to the app's own store and read the result back.
    fn write_settings(
        _: &mut (),
        job: &mut PublishJob<ProbeSettings>,
    ) -> Result<Published<ProbeSettings>, Errno> {
        tairix_appdata::publish(&mut Settings::open_without_defaults(&mut RtHost), job)
    }

    /// Start the settings-store worker and put its answer wake on `set`. Where
    /// neither the wake nor the thread can be had, the write falls back to the
    /// loop (its desk stopped), so a choice is never lost — slower, never wrong.
    fn start_publisher(publisher: &Arc<Publisher>, set: u64) {
        let added = publisher.wake().read_end().is_some_and(|read| {
            tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                PUBLISH_TOKEN,
            ) == 0
        });
        if !added {
            publisher.stop();
            app::report(
                APP_NAME,
                "settings writes stay on the loop: no store-worker wake",
            );
            return;
        }
        if Publisher::start(publisher).is_err() {
            publisher.stop();
            app::report(
                APP_NAME,
                "settings writes stay on the loop: no store-worker thread",
            );
        }
    }

    /// The settings window on screen: its pane on the map's own channel, the
    /// picture retained between paints, and what of it is owed.
    struct SettingsPane {
        pane: WindowPane,
        surface: Surface,
        content: ProbePane,
        owed: Repaint,
    }

    impl SettingsPane {
        /// Open the window on `client`, refusing a create answered by any
        /// session but `server`, the one serving the map's window.
        fn open(
            client: &mut WindowClient<app::RtWindowTransport>,
            endpoint: u64,
            server: ProcId,
            posture: Posture,
            desktop: &Desktop,
            themes: &ThemeRegistry,
        ) -> Result<Self, String> {
            let content = ProbePane::new(posture, desktop.scale(), themes.active());
            let (width, height) = content.extent();
            let surface = Surface::new(width, height)
                .ok_or_else(|| String::from("no memory for the window"))?;
            let mode = app::mode_for(width, height);
            let (pane, replied) = WindowPane::open(
                client,
                endpoint,
                &mode,
                settings::TITLE,
                WindowSizing::Fixed,
            )
            .map_err(|err| alloc::format!("{err}"))?;
            if replied != server {
                let _ = pane.close(client);
                return Err(String::from(
                    "the settings window was answered by another session",
                ));
            }
            Ok(Self {
                pane,
                surface,
                content,
                owed: Repaint::Whole,
            })
        }

        /// The session's id for the window.
        const fn id(&self) -> u64 {
            self.pane.id()
        }

        /// Owe what an event reported.
        fn owe(&mut self, sink: Region) {
            self.owed.merge(Repaint::Parts(sink));
        }

        /// Paint what is owed and present only that.
        fn paint(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            desktop: &Desktop,
            themes: &ThemeRegistry,
        ) -> Result<(), Errno> {
            if self.pane.content_released() {
                self.owed = Repaint::Whole;
            }
            if self.owed.is_clean() {
                return Ok(());
            }
            let area = self.owed.area(self.surface.width(), self.surface.height());
            let (content, scale, theme) = (&self.content, desktop.scale(), themes.active());
            damage::paint_parts(&mut self.surface, area.rects(), |surface| {
                content.render(surface, scale, theme);
            });
            let Some(rect) = damage_in(self.pane.mode(), area.bounds()) else {
                self.owed = Repaint::clean();
                return Ok(());
            };
            match self.pane.present(client, &self.surface, rect) {
                Ok(()) => {
                    self.owed = Repaint::clean();
                    Ok(())
                }
                Err(err) => {
                    self.owed = Repaint::Whole;
                    Err(err)
                }
            }
        }

        fn close(self, client: &mut WindowClient<app::RtWindowTransport>) {
            let _ = self.pane.close(client);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one-time wiring of the window, the four workers and the event loop"
    )]
    fn main() -> i32 {
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
        let mut window = AppWindow::new();
        let (desktop, themes) = match app::bring_up_desktop(window.client()) {
            Ok(pair) => pair,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let (width, height) = desktop.window_size(WIN_WIDTH, WIN_HEIGHT);
        let mode = app::mode_for(width, height);
        let binding = match app::bind_event_mailbox() {
            Ok(binding) => binding,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let event_endpoint = binding.endpoint();
        let set = binding.set();

        let (doorbell, mut discovery_notice) = doorbell(set);
        let open_session = Arc::new(OpenSession::default());
        let (reader, namer, threaded) = match start_workers(
            set,
            doorbell.as_ref().map_or(0, PrivatePort::id),
            open_session.clone(),
        ) {
            Ok(workers) => workers,
            Err(code) => return code,
        };
        // A lookup makes many blocking calls, which only a thread of its own
        // may make, as a collect does; a read of the network stays inline.
        let collect = Collect::start(doorbell.is_some(), threaded.reader);
        if doorbell.is_some() && !threaded.reader {
            discovery_notice = Some(String::from(
                "Service discovery is off: it needs a thread of its own",
            ));
        }
        let naming_notice = (!threaded.namer)
            .then(|| String::from("Hosts are not named: naming needs a thread of its own"));
        let (prober, probe_notice) = start_prober(set);
        let (loaded_settings, settings_refusals) =
            tairix_appdata::loaded(&Settings::open_without_defaults(&mut RtHost));
        for refusal in settings_refusals {
            app::report(APP_NAME, format_args!("settings not read ({refusal:?})"));
        }
        let publisher = Arc::new(Publisher::new(
            write_settings,
            (),
            tairix_rt::sync::WorkerWake::create(),
        ));
        start_publisher(&publisher, set);
        let _reader_guard = WorkerGuard::new(&reader);
        let _namer_guard = WorkerGuard::new(&namer);
        let _prober_guard = prober.as_ref().map(WorkerGuard::new);
        let _publisher_guard = WorkerGuard::new(&publisher);

        declare_app_bar(window.client(), event_endpoint);
        let canvas = Rect::new(0, 0, mode.width_px, mode.height_px);
        let view = MapView::new(canvas, themes.active().motion());
        let mut state = NetMap {
            window,
            desktop,
            themes,
            view,
            inventory: Inventory::new(),
            mode,
            loaded: false,
            read_notices: Vec::new(),
            discovery_notice,
            naming_notice,
            probe_notice,
            menu: None,
            tip: DeclaredTip::new(),
            choice: Publication::new(loaded_settings),
            settings: None,
            event_endpoint,
            server: None,
        };
        // The map was started to be looked at, so a first window that will
        // not open leaves it nothing to be; later ones are a click on its slot.
        let server = match state.open(event_endpoint) {
            Ok(server) => server,
            Err(code) => return code,
        };

        let deadlines = Deadlines::default();
        let desktop_moved = Cell::new(false);
        let rung = Cell::new(false);
        let events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(event_endpoint, server),
            set,
            reader: &reader,
            namer: &namer,
            prober: prober.as_deref(),
            publisher: Some(&publisher),
            doorbell: doorbell.as_ref(),
            open_session: &open_session,
            deadlines: &deadlines,
            desktop_moved: &desktop_moved,
            rung: &rung,
        });
        run(
            &mut state,
            &Workers {
                reader: &reader,
                namer: &namer,
                prober: prober.as_deref(),
                publisher: Some(&publisher),
                collect,
                naming: threaded.namer,
            },
            event_endpoint,
            &deadlines,
            &desktop_moved,
            &rung,
            events,
        )
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
