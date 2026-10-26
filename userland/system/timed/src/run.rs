//! The `Run` entry-point binary of the time service, installed at
//! `/System/Services/timed.app/Run` (`plans/TIMESYNC.md` TS-2).
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt` — never the C ABI. `tairix-rt` provides
//! `_start`, the per-process stack canary, the panic handler, the
//! `#[global_allocator]`, the wall-clock and monotonic syscall wrappers, the
//! datagram socket and wait-set reactor, and the file reads and writes the
//! persisted record goes through; `tairix_rt::entry!` names this program's
//! `main`.
//!
//! # The reactor
//!
//! One wait-set over the delivery port the network stack posts this service's
//! datagrams to and a watch on the machine configuration, armed with the
//! timeout the engine's single folded deadline implies. The loop parks; it
//! never polls. A wake is a datagram (evaluate it in the sandbox worker, apply
//! the verdict), a configuration change (read it, apply `tairix_timed::retune`'s
//! answer), or the deadline lapsing (send the next request). With no deadline
//! left the service parks on the watch alone, since the configuration may yet
//! switch network time on or name a server; only with no watch either does it
//! exit, rather than hold a task and a bound port doing nothing.
//!
//! # Where the servers come from
//!
//! The three tiers of `tairix_timesync::select_servers`: the operator's
//! `time.servers`, else what DHCP offered, else the built-in fallback. The
//! service starts before either of the first two is knowable, so it starts on
//! the fallback and re-selects on a bounded ladder, replacing its
//! servers only when a *strictly better* tier appears. The root holding the
//! configuration is waited for on the mount table's notice instead: the
//! configuration is watched, and the record read again, once it is served.
//!
//! # Two roles, one binary
//!
//! Started with the sandbox worker argument, this binary *is* the
//! capability-less NTP evaluation worker (`tairix_sandbox::rt`); started
//! normally it is the service. That is the seam's canonical shape: the parent
//! spawns its own binary through the kernel's sandbox spawn mode, so the
//! worker holds two pipe ends and nothing else.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
// Compiled only for the freestanding service binary, which links the optional
// `tairix-rt` runtime through the default `program` feature. Host tooling
// builds only this crate's *library*, so this module (and `tairix-rt`) never
// enter that build.
#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::vec;
    use alloc::vec::Vec;

    use tairix_abi::driver_store::SystemConfigFile;
    use tairix_abi::net::{SocketAddr, SocketDatagram, SocketDelivery, SocketId};
    use tairix_abi::net_ipc::{ip_from_parts, NetAddrFamily};
    use tairix_abi::rtc_ipc::{self, RtcOp, RtcReading, RTC_ENDPOINT};
    use tairix_abi::time::{Duration64, Time64};
    use tairix_abi::waitset::WaitSourceKind;
    use tairix_abi::{Errno, WallClockReading, WallTimeState, MAX_TIME_SERVERS};
    use tairix_log::{Event, EventId, Level};
    use tairix_net::addr::IpAddr;
    use tairix_net::ntp::PORT;
    use tairix_procinfo::{IpcTransport, WalkStep};
    use tairix_resolver::RtDnsTransport;
    use tairix_rng::{FastRng, RandU64};
    use tairix_rt::follow::{Follow, Followed, Verdict};
    use tairix_rt::{LogSink, PrivatePort, WaitSet};
    use tairix_sandbox::host::ParserSandbox;
    use tairix_sandbox::rt::{worker_role, RtLauncher};
    use tairix_sandbox::timesync::TimeSyncService;
    use tairix_sysconfig::{Switch, SystemConfig, CONFIG_PATH, MAX_CONFIG_LEN};
    use tairix_timed::{
        retune, selection_for, Clock, NetworkSettings, NetworkTime, RecordStore, Retune, RtcSource,
        Timed, TimedConfig, Transport, CONFIG_RETRY_ATTEMPTS, CONFIG_RETRY_BASE_NANOS,
    };
    use tairix_timesync::events::{
        CONFIG_NOT_FOLLOWED, CONFIG_REFUSED, SERVERS_FROM_FALLBACK, SERVICE_UNAVAILABLE,
        TIMED_RANGE_START,
    };
    use tairix_timesync::{ServerSelection, ServerSource, TimeServer, RECORD_LEN, RECORD_PATH};
    use tairix_util::retry::RetryLadder;

    /// Exit code when the service cannot serve: its reactor could not be
    /// armed, or the kernel CSPRNG cannot key its nonces. The reason is
    /// recorded first; the service exits rather than degrading into a poll or
    /// a predictable nonce, and PID 1 relaunches it.
    const EXIT_UNAVAILABLE: i32 = 70;

    /// Delivery-port mailbox depth. One request is in flight at a time, but a
    /// late reply from a previous transaction can still be arriving; this
    /// headroom lets a few queue rather than back-pressure the stack.
    const DELIVER_CAPACITY: usize = 8;

    /// Wait-set token for the delivery port.
    const DELIVER_TOKEN: u64 = 1;

    /// Wait-set token for the configuration watch.
    const WATCH_TOKEN: u64 = 2;

    /// The audit sink every record is written through.
    static LOG_SINK: LogSink = LogSink;

    /// The production [`Clock`]: the kernel's monotonic and wall clocks.
    struct RtClock;

    impl Clock for RtClock {
        fn monotonic(&self) -> Duration64 {
            Duration64::from_nanos(tairix_rt::clock_get())
        }

        fn wall(&self) -> Result<WallClockReading, Errno> {
            tairix_rt::wall_time().map_err(Errno::from_syscall)
        }

        fn set_wall(&self, time: Time64, state: WallTimeState) -> Result<(), Errno> {
            let ret = tairix_rt::wall_time_set(time, state);
            if ret < 0 {
                Err(Errno::from_syscall(ret))
            } else {
                Ok(())
            }
        }
    }

    /// The production [`RtcSource`]: the board's clock chip, reached through
    /// the autoloaded RTC driver's well-known call endpoint.
    ///
    /// A board with no clock chip has no driver serving the endpoint, so the
    /// call fails `NotFound` and the engine's bounded ladder decides how long
    /// to keep looking. Every refusal is typed and in-band; nothing here
    /// retries on the spot.
    struct RtRtc;

    impl RtRtc {
        /// Issue one request and return the framed reply bytes.
        fn call(op: RtcOp, time: Time64, reply: &mut [u8]) -> Result<usize, Errno> {
            let mut request = [0u8; rtc_ipc::REQUEST_LEN];
            rtc_ipc::encode_request(&mut request, op, time)?;
            tairix_rt::ipc_call(RTC_ENDPOINT, &request, reply).map_err(Errno::from_syscall)
        }
    }

    impl RtcSource for RtRtc {
        fn read(&mut self) -> Result<RtcReading, Errno> {
            let mut reply = [0u8; rtc_ipc::REPLY_LEN];
            let n = Self::call(RtcOp::Read, Time64::UNIX_EPOCH, &mut reply)?;
            rtc_ipc::decode_reading(&reply[..n])
        }

        fn set(&mut self, time: Time64) -> Result<(), Errno> {
            let mut reply = [0u8; rtc_ipc::REPLY_LEN];
            let n = Self::call(RtcOp::Set, time, &mut reply)?;
            rtc_ipc::decode_ack(&reply[..n])
        }
    }

    /// The production [`RecordStore`]: the fixed-length document under
    /// `/System/Settings/Time`, one of the two writable paths beneath the
    /// read-only `/System`.
    struct RtRecordStore;

    impl RecordStore for RtRecordStore {
        fn read(&self) -> Result<Option<Vec<u8>>, Errno> {
            // One byte past the record's length is read, so a longer document
            // is refused by the record's own length check rather than read as
            // its first bytes.
            match tairix_rt::read_path_to_end(RECORD_PATH.as_bytes(), RECORD_LEN) {
                Ok(bytes) => Ok(Some(bytes)),
                // A machine that has never synchronised has no record; that
                // is the normal first-boot case, not a failure.
                Err(ret) if Errno::from_syscall(ret) == Errno::NotFound => Ok(None),
                Err(ret) => Err(Errno::from_syscall(ret)),
            }
        }

        fn write(&self, bytes: &[u8]) -> Result<(), Errno> {
            // The directory is absent only on a volume laid out before this
            // service existed, so it is created only when found missing: an
            // unconditional attempt under the system-owned `/System/Settings`
            // would be refused, and audited, on every sync.
            tairix_rt::replace_file(
                RECORD_PATH,
                bytes,
                tairix_rt::ParentDir::CreatedIfMissing,
                tairix_abi::fs::FS_NEW_FILE_MODE,
            )
        }
    }

    /// The production [`Transport`]: a UDP datagram socket per address family,
    /// with each configured server's name resolved once and cached.
    ///
    /// Resolution is lazy and per-server rather than a start-up sweep: a
    /// machine whose network comes up after this service does would otherwise
    /// have resolved nothing and never retried. A name that does not resolve
    /// yet simply fails the send, and the engine's own timeout and bounded
    /// backoff pace the next attempt.
    ///
    /// The stub-resolver transport is opened lazily too, and only for a server
    /// that is *not* an address literal: a machine configured with literals
    /// never binds a resolver delivery port or a wait-set it would not use.
    struct RtTransport {
        servers: Vec<ServerEntry>,
        dns: Option<RtDnsTransport>,
        /// The process-private port the stack delivers replies to.
        deliver: u64,
        v4: Option<SocketId>,
        v6: Option<SocketId>,
    }

    /// One selected server: its spelling and, once known, its address.
    struct ServerEntry {
        name: Vec<u8>,
        address: Option<IpAddr>,
    }

    impl RtTransport {
        /// Build the transport over the selected servers.
        ///
        /// A network-supplied server arrives as an address and is entered
        /// already resolved, so a machine whose only DNS advice would have
        /// come from the same lease never needs a resolver to keep time.
        fn new(servers: &[TimeServer], deliver: u64) -> Self {
            let servers = servers
                .iter()
                .take(MAX_TIME_SERVERS)
                .map(|server| ServerEntry {
                    name: server.name.as_bytes().to_vec(),
                    address: server.address,
                })
                .collect();
            Self {
                servers,
                dns: None,
                deliver,
                v4: None,
                v6: None,
            }
        }

        /// The address of the server at `index`, resolving and caching it on
        /// first use.
        fn address_of(&mut self, index: u8) -> Option<IpAddr> {
            let entry = self.servers.get(usize::from(index))?;
            if let Some(address) = entry.address {
                return Some(address);
            }
            let name = core::str::from_utf8(&entry.name).ok()?;
            // An address literal is answered by the one shared spelling
            // policy without a resolver at all, which is what lets a machine
            // with no DNS configured still keep time.
            let resolved = if let Some(address) = tairix_resolver::literal_address(name, None) {
                address
            } else {
                if self.dns.is_none() {
                    self.dns = RtDnsTransport::open().ok();
                }
                self.dns.as_mut()?.host_address(name, None)?
            };
            self.servers.get_mut(usize::from(index))?.address = Some(resolved);
            Some(resolved)
        }

        /// The datagram socket for `family`, opened on first use with a
        /// CSPRNG-drawn ephemeral source port and cached thereafter.
        fn socket_for(&mut self, family: NetAddrFamily) -> Result<SocketId, Errno> {
            let cached = match family {
                NetAddrFamily::V4 => &mut self.v4,
                NetAddrFamily::V6 => &mut self.v6,
            };
            if let Some(socket) = *cached {
                return Ok(socket);
            }
            let socket = tairix_rt::net::socket(family, self.deliver)?;
            // A local port of 0 asks the stack for a CSPRNG-drawn ephemeral
            // port, widening an off-path spoofer's search space beyond the
            // nonce alone.
            let local = SocketAddr {
                family,
                addr: [0u8; 16],
                port: 0,
            };
            tairix_rt::net::bind(socket, local)?;
            *cached = Some(socket);
            Ok(socket)
        }
    }

    impl Transport for RtTransport {
        fn send(&mut self, index: u8, packet: &[u8]) -> Result<(), Errno> {
            let address = self.address_of(index).ok_or(Errno::NetworkUnreachable)?;
            let (family, addr) = tairix_abi::net_ipc::address_parts(address);
            let socket = self.socket_for(family)?;
            let dest = SocketAddr {
                family,
                addr,
                port: PORT,
            };
            tairix_rt::net::send(socket, Some(dest), None, packet)
        }
    }

    impl Drop for RtTransport {
        fn drop(&mut self) {
            // Best-effort teardown so the ephemeral ports do not linger.
            if let Some(socket) = self.v4.take() {
                let _ = tairix_rt::net::close(socket);
            }
            if let Some(socket) = self.v6.take() {
                let _ = tairix_rt::net::close(socket);
            }
        }
    }

    /// Record a start-up or reactor outcome.
    fn record(id: EventId, level: Level, message: &str) {
        tairix_log::log(
            &LOG_SINK,
            &Event {
                level,
                id,
                message,
                fields: &[],
            },
        );
    }

    /// Read the network time servers this host's DHCP client(s) learned, over
    /// the ungated system-information query `sysinfod` fronts.
    ///
    /// Advice, not authority: the answer is a set of addresses to *ask*, and
    /// every sample one returns is still nonce-gated and plausibility-checked
    /// before the clock moves. An unavailable service (the usual case at
    /// start-up, before `sysinfod` and `netstack` are up) is an
    /// empty set, which simply leaves a lower tier in place; the caller's
    /// bounded ladder is what looks again.
    fn learned_time_servers() -> Vec<IpAddr> {
        let mut servers = Vec::new();
        let _ = tairix_procinfo::for_each_time_server(&IpcTransport, |record| {
            servers.push(ip_from_parts(record.family, record.addr));
            Ok(WalkStep::Continue)
        });
        servers
    }

    /// Read the network-time settings of the machine configuration as it
    /// stands: the documented defaults where neither layer holds a document,
    /// and `None`, recorded, for one that cannot be read or fully understood —
    /// never a guess at a partial intent.
    ///
    /// Before the root is served the layers have no backing at all, which is
    /// indistinguishable from a volume-less boot and equally yields the
    /// defaults; the mount table's notice is what tells the caller to read
    /// again.
    fn load_settings() -> Option<NetworkSettings> {
        let read = SystemConfigFile::System
            .read_effective(|path| tairix_rt::read_document(path.as_bytes(), MAX_CONFIG_LEN));
        let parsed = match read {
            Ok(Some(text)) => SystemConfig::parse(&text).ok(),
            Ok(None) | Err(Errno::NotImplemented) => Some(SystemConfig::default()),
            Err(_) => None,
        };
        if parsed.is_none() {
            record(
                CONFIG_REFUSED,
                Level::Warn,
                "timed: the machine configuration could not be read or understood; the settings in use stand",
            );
        }
        parsed.as_ref().map(settings_of)
    }

    /// The network-time half of `config`.
    fn settings_of(config: &SystemConfig) -> NetworkSettings {
        NetworkSettings {
            on: config.time_network == Switch::On,
            configured: config.time_servers.clone(),
            refresh: config.time_refresh.interval(),
        }
    }

    /// Look for the configuration again, replacing `watch`; answers whether
    /// the settings tree was there. Until it can be reached, the mount table's
    /// notice wakes the loop under the watch's token instead. A tree that is
    /// there but cannot be watched is recorded, since its changes then wait for
    /// a restart.
    fn refollow(set: &WaitSet, watch: &mut Option<Followed>) -> bool {
        if let Some(old) = watch.take() {
            old.leave(set);
        }
        match Followed::follow(CONFIG_PATH, set, WATCH_TOKEN) {
            Follow::NotYet => false,
            Follow::Watching(followed) => {
                *watch = Some(followed);
                true
            }
            Follow::Unwatched(err) => {
                tairix_log::log(
                    &LOG_SINK,
                    &Event {
                        level: Level::Warn,
                        id: CONFIG_NOT_FOLLOWED,
                        message: "timed: the machine configuration cannot be watched; a change to it applies when the service restarts",
                        fields: &[tairix_log::Field {
                            key: "error",
                            value: tairix_abi::FieldValue::Error(err),
                        }],
                    },
                );
                true
            }
        }
    }

    /// The running service.
    type Service = Timed<RtClock, RtRecordStore, RtTransport, RtLauncher, LogSink, RtRtc>;

    /// Whether there is nothing better to look for: the operator named a
    /// server, or switched network time off.
    fn settled(service: &Service, held: &NetworkSettings) -> bool {
        !held.on || service.source() == Some(ServerSource::Configured)
    }

    /// The service as it starts, asking the network as `held` says.
    fn start(held: &NetworkSettings, deliver: u64, rng: &mut FastRng) -> Service {
        Timed::new(TimedConfig {
            clock: RtClock,
            store: RtRecordStore,
            sandbox: ParserSandbox::new(
                RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Messages),
                LOG_SINK,
            ),
            sink: LOG_SINK,
            rtc: RtRtc,
            network: held.on.then(|| {
                network_time(
                    selection_for(held, learned_time_servers),
                    held.refresh,
                    deliver,
                    rng.next_u64(),
                )
            }),
        })
    }

    /// Climb `retry`'s rung at `woken`, stopping once nothing better can be
    /// found or the ladder is spent; a spent ladder says so when the fallback
    /// stands.
    fn climb(
        retry: &mut Option<RetryLadder>,
        service: &Service,
        held: &NetworkSettings,
        woken: u64,
    ) {
        if settled(service, held) {
            *retry = None;
        } else if !retry.as_mut().is_some_and(|rung| rung.advance(woken)) {
            if service.source() == Some(ServerSource::Fallback) {
                record(
                    SERVERS_FROM_FALLBACK,
                    Level::Info,
                    "timed: no configured or network-supplied time server was found within the start-up window; the built-in fallback stands",
                );
            }
            *retry = None;
        }
    }

    /// Read the configuration again and apply what it asks of `service`,
    /// which last applied `held`.
    fn reread(service: &mut Service, held: &mut NetworkSettings, deliver: u64, rng: &mut FastRng) {
        let Some(next) = load_settings() else {
            return;
        };
        match retune(held, service.source(), &next, learned_time_servers) {
            Retune::Keep => {}
            Retune::Off => service.replace_network(None),
            Retune::Select(selection) => service.replace_network(Some(network_time(
                selection,
                next.refresh,
                deliver,
                rng.next_u64(),
            ))),
            Retune::Refresh(refresh) => service.set_refresh(refresh),
        }
        *held = next;
    }

    /// Hand `service` every datagram the stack delivered to `deliver`; the
    /// receive discards any other sender's post.
    fn drain_datagrams(service: &mut Service, deliver: u64, scratch: &mut [u8], woken: Duration64) {
        while let Ok(delivery) = tairix_rt::net::recv(deliver, scratch) {
            // Its sockets join no group, so a link edge never arrives; were
            // one to, it carries nothing for the clock.
            if let SocketDelivery::Datagram(datagram) = delivery {
                service.on_datagram(woken, datagram.payload);
            }
        }
    }

    /// The network half over `selection`, sending from `deliver`.
    fn network_time(
        selection: ServerSelection,
        refresh: Duration64,
        deliver: u64,
        entropy: u64,
    ) -> NetworkTime<RtTransport> {
        NetworkTime {
            transport: RtTransport::new(&selection.servers, deliver),
            selection,
            refresh,
            entropy,
        }
    }

    /// Widen a non-negative monotonic [`Duration64`] to the `u64` nanosecond
    /// count the wait-set and clock syscalls speak.
    fn nanos(span: Duration64) -> u64 {
        span.saturating_total_nanos()
    }

    /// The reactor's remaining wait until the earlier of two absolute
    /// nanosecond deadlines, either of which may be absent.
    ///
    /// [`None`] means there is nothing left to wait for at all. Zero means a
    /// deadline has already lapsed, so the next turn of the loop runs at once.
    fn timeout_until(now: u64, engine: Option<u64>, config: Option<u64>) -> Option<u64> {
        let soonest = match (engine, config) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return None,
        };
        Some(soonest.saturating_sub(now))
    }

    /// Bind the delivery port and arm the reactor's wait-set over it,
    /// returning the set and the port.
    ///
    /// [`None`] once the failure has been recorded: the service exits rather
    /// than degrading into a poll, and PID 1 relaunches it.
    fn arm_reactor() -> Option<(WaitSet, PrivatePort)> {
        let Ok(deliver) = PrivatePort::bind(SocketDatagram::MAX_WIRE_LEN, DELIVER_CAPACITY) else {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "timed: the delivery port could not be bound",
            );
            return None;
        };
        let Ok(set) = WaitSet::create() else {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "timed: the reactor wait-set could not be created",
            );
            return None;
        };
        if set
            .add(WaitSourceKind::Port, deliver.id(), DELIVER_TOKEN)
            .is_err()
        {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "timed: the reactor wait-set could not be armed",
            );
            return None;
        }
        Some((set, deliver))
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    fn main() -> i32 {
        // The worker role first, before any other argument handling: a worker
        // never behaves as the service.
        if worker_role() {
            return tairix_sandbox::rt::serve_stdio(&mut TimeSyncService).exit_code();
        }

        // Every nonce and jitter draw comes from this generator, keyed from
        // the kernel CSPRNG — never a counter or a clock reading, which would
        // hand an off-path attacker a predictable target.
        let Ok(mut rng) = <FastRng>::keyed_by(tairix_rt::random_fill) else {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "timed: the kernel random source cannot key the request nonces",
            );
            return EXIT_UNAVAILABLE;
        };
        let Some((set, deliver)) = arm_reactor() else {
            return EXIT_UNAVAILABLE;
        };

        // Watched before it is read, so no change falls between the two.
        let mut watch = None;
        let mut reachable = refollow(&set, &mut watch);
        // Built immediately, from whatever is knowable right now — which at
        // start-up is usually neither the store (the root is not mounted yet) nor a lease (no NIC is up), so it is usually the
        // built-in fallback. Waiting here instead would hold the boot up
        // behind a service nothing else is waiting for, and the fallback tier
        // is exactly what makes waiting unnecessary.
        let mut held = load_settings().unwrap_or_else(|| settings_of(&SystemConfig::default()));
        let mut service = start(&held, deliver.id(), &mut rng);
        let mut retry = RetryLadder::arm(
            tairix_rt::clock_get(),
            CONFIG_RETRY_BASE_NANOS,
            CONFIG_RETRY_ATTEMPTS,
            settled(&service, &held),
        );

        let mut scratch = vec![0u8; SocketDatagram::MAX_WIRE_LEN];
        let mut token = 0u64;
        loop {
            let now = tairix_rt::clock_get();
            let engine_at = service.next_deadline().map(nanos);
            let timeout = match timeout_until(now, engine_at, retry.as_ref().map(|r| r.at)) {
                Some(timeout) => timeout,
                // Nothing is due, but the configuration may yet switch network
                // time on or name a server, or the root holding it be served.
                None if watch.is_some() || !reachable => u64::MAX,
                // Nothing is due and nothing can change. Exit rather than hold
                // a task and a bound delivery port for the rest of the boot
                // doing nothing; a restart picks a later configuration up, and
                // PID 1 audits the exit.
                None => return 0,
            };
            let waited = tairix_rt::waitset_wait(set.handle(), timeout, &mut token);
            let woken = Duration64::from_nanos(tairix_rt::clock_get());
            let mut changed = false;
            if waited == 0 && token == WATCH_TOKEN {
                match watch.as_ref().map_or(Verdict::Rewatch, Followed::drain) {
                    Verdict::Unconcerned => {}
                    Verdict::Reread => changed = true,
                    Verdict::Rewatch => {
                        let was_reachable = reachable;
                        reachable = refollow(&set, &mut watch);
                        if reachable && !was_reachable {
                            service.store_reachable(rng.next_u64());
                        }
                        changed = true;
                    }
                }
            } else if waited == 0 {
                drain_datagrams(&mut service, deliver.id(), &mut scratch, woken);
            } else if Errno::from_syscall(waited) != Errno::TimedOut {
                // A dead wait-set would degrade the loop into a busy poll;
                // exit fail-loud instead and let PID 1 relaunch the service.
                record(
                    SERVICE_UNAVAILABLE,
                    Level::Warn,
                    "timed: the reactor wait-set failed",
                );
                return EXIT_UNAVAILABLE;
            }

            // The ladder's rung, if one is due: a DHCP lease may have been
            // acquired since the last.
            let rung_due = retry.as_ref().is_some_and(|rung| nanos(woken) >= rung.at);
            if rung_due || changed {
                reread(&mut service, &mut held, deliver.id(), &mut rng);
            }
            if rung_due {
                climb(&mut retry, &service, &held, nanos(woken));
            }

            service.poll(woken, rng.next_u64());
        }
    }

    // Touch the reserved-range constant so the audit range this service owns
    // is linked into the program that emits it.
    const _: u32 = TIMED_RANGE_START;

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry — the freestanding `tairix-rt` `_start` path — is not compiled, so
// this inert `main` keeps the crate building under the host tooling. It
// performs no I/O; the engine and the sandboxed evaluation are host-tested in
// their own modules and the whole path is exercised by the QEMU vertical.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
