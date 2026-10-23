//! The `Run` binary of the cooling service, installed at
//! `/System/Services/thermald.app/Run`.
//!
//! It serves the ungated `thermal-v1` status, follows the machine
//! configuration's `cooling.fan.*` keys through its directory's watch, and
//! re-reads the hardware tree whenever its generation moves, driving
//! [`Thermald`](tairix_thermald::Thermald) from each. It parks on one wait set throughout; the only
//! deadline it arms there is an automatic fan's next sample, and each call to a
//! driver carries its own. On the host it is an inert stub.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::vec::Vec;

    use tairix_abi::driver::gpio::{
        decode_lines, decode_status, HeaderRequest, LineTable, Polarity, HEADER_MAX_REPLY,
        HEADER_REQUEST_LEN,
    };
    use tairix_abi::driver::thermal::{
        decode_reading, encode_read, Millicelsius, SENSOR_REPLY_LEN, SENSOR_REQUEST_LEN,
    };
    use tairix_abi::driver_store::SystemConfigFile;
    use tairix_abi::hwtree::HW_TREE_READ_START;
    use tairix_abi::thermal_ipc::{
        encode_status, FanPolicy, ThermalRequest, THERMAL_ENDPOINT, THERMAL_MAX_REPLY,
        THERMAL_REQUEST_LEN,
    };
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
    use tairix_abi::{Errno, FieldValue, HwNode, HwTreeHeader, NoticeTopic};
    use tairix_caps::CapabilitySet;
    use tairix_log::{EventId, Field, Level};
    use tairix_rt::follow::{Follow, Followed, Verdict};
    use tairix_rt::{LogSink, WaitSet};
    use tairix_sysconfig::{SystemConfig, CONFIG_PATH, MAX_CONFIG_LEN};
    use tairix_thermald::{Entry, Inventory, Journal, Machine, Thermald};
    use tairix_util::fallible::read_snapshot;

    const ENDPOINT_TOKEN: u64 = 0;
    const WATCH_TOKEN: u64 = 1;
    const TREE_TOKEN: u64 = 2;
    const MOUNTS_TOKEN: u64 = 3;

    /// Status calls queued at once; each is answered before the next.
    const ENDPOINT_CAPACITY: usize = 8;

    /// Status calls one wake answers before returning to the wait set, so a
    /// flood cannot hold the fan's sample back.
    const CALLS_PER_WAKE: usize = 8;

    /// How often a storm of configuration changes may report; an isolated
    /// change reports at once.
    const WATCH_LATENCY_NS: u64 = 1_000_000_000;

    /// Change batches one wake drains before reading the configuration
    /// anyway.
    const WATCH_DRAIN_BATCHES: usize = 16;

    /// How long a sensor or the header has to answer one call: a wedged
    /// driver costs the fan a reading, which runs it, never the service.
    const CALL_DEADLINE_NS: u64 = 250_000_000;

    const EVENT_READY: EventId = EventId(32_000);
    const EVENT_CLAIMED: EventId = EventId(32_001);
    const EVENT_RELEASED: EventId = EventId(32_002);
    const EVENT_SWITCHED: EventId = EventId(32_003);
    const EVENT_FAULT: EventId = EventId(32_004);
    const EVENT_RECOVERED: EventId = EventId(32_005);
    const EVENT_CONFIG_REFUSED: EventId = EventId(32_006);
    const EVENT_CONFIG_UNFOLLOWED: EventId = EventId(32_007);
    const EVENT_TREE_UNREAD: EventId = EventId(32_008);
    const EVENT_UNAVAILABLE: EventId = EventId(32_009);
    const EVENT_LOST: EventId = EventId(32_010);

    /// The status a service that cannot run ends with.
    const EXIT_UNAVAILABLE: i32 = 1;

    fn main() -> i32 {
        let (Some(set), Ok(calls)) = (serving(), WaitSet::create()) else {
            record(
                EVENT_UNAVAILABLE,
                Level::Error,
                "thermald: the endpoint or its wait sets could not be made",
                &[],
            );
            return EXIT_UNAVAILABLE;
        };
        let mut machine = RtMachine { calls };
        let mut journal = LogJournal;
        let mut service = Thermald::default();
        let mut tree = Vec::new();
        // Followed before it is read, so no change falls between the two.
        let mut watch = None;
        refollow(&set, &mut watch);
        if let Some(inventory) = inventory(&mut tree) {
            service.inventory_changed(inventory, &mut machine, &mut journal);
        }
        if let Some(policy) = load_policy() {
            service.policy_changed(policy, &mut machine, &mut journal);
        }
        record(EVENT_READY, Level::Info, "thermald: serving", &[]);
        let mut token = 0u64;
        loop {
            let now = machine.now_us();
            let timeout = service.deadline_us().map_or(WAITSET_TIMEOUT_NONE, |due| {
                due.saturating_sub(now).saturating_mul(1_000)
            });
            let waited = tairix_rt::waitset_wait(set.handle(), timeout, &mut token);
            if waited < 0 && Errno::from_syscall(waited) != Errno::TimedOut {
                // A dead wait set would degrade the loop into a busy poll.
                record(
                    EVENT_UNAVAILABLE,
                    Level::Error,
                    "thermald: the wait set failed",
                    &[],
                );
                return EXIT_UNAVAILABLE;
            }
            if waited == 0 {
                match token {
                    ENDPOINT_TOKEN => answer(&mut service, &mut machine),
                    WATCH_TOKEN | MOUNTS_TOKEN => {
                        let reread = match (token, &watch) {
                            (WATCH_TOKEN, Some(followed)) => followed.drain(WATCH_DRAIN_BATCHES),
                            _ => Verdict::Rewatch,
                        };
                        if reread == Verdict::Rewatch {
                            refollow(&set, &mut watch);
                        }
                        if reread != Verdict::Unconcerned {
                            if let Some(policy) = load_policy() {
                                service.policy_changed(policy, &mut machine, &mut journal);
                            }
                        }
                    }
                    TREE_TOKEN => {
                        if let Some(inventory) = inventory(&mut tree) {
                            service.inventory_changed(inventory, &mut machine, &mut journal);
                        }
                    }
                    _ => {}
                }
            }
            if service
                .deadline_us()
                .is_some_and(|due| due <= machine.now_us())
            {
                service.expired(&mut machine, &mut journal);
            }
        }
    }

    /// Bind the status endpoint and the wait set every wake arrives on.
    fn serving() -> Option<WaitSet> {
        let bound = tairix_rt::call_create(
            THERMAL_ENDPOINT,
            &CapabilitySet::empty(),
            &CapabilitySet::empty(),
            THERMAL_REQUEST_LEN,
            THERMAL_MAX_REPLY,
            ENDPOINT_CAPACITY,
        );
        if bound != 0 {
            return None;
        }
        let set = WaitSet::create().ok()?;
        set.add(WaitSourceKind::Endpoint, THERMAL_ENDPOINT, ENDPOINT_TOKEN)
            .ok()?;
        set.add(WaitSourceKind::HardwareTree, 0, TREE_TOKEN).ok()?;
        Some(set)
    }

    /// Follow the configuration again, replacing `watch`. Until its
    /// directories exist the mount table is waited on instead: the volume
    /// holding them being mounted is what makes them appear.
    fn refollow(set: &WaitSet, watch: &mut Option<Followed>) {
        if let Some(old) = watch.take() {
            old.leave(set);
        }
        let mounts = NoticeTopic::Mounts.as_u32().into();
        match Followed::follow(CONFIG_PATH, set, WATCH_TOKEN, WATCH_LATENCY_NS) {
            Follow::Watching(followed) => {
                // Gone already once a previous refollow left it.
                let _ = tairix_rt::waitset_ctl(
                    set.handle(),
                    WaitSetOp::Del,
                    WaitSourceKind::SystemNotice,
                    mounts,
                    MOUNTS_TOKEN,
                );
                *watch = Some(followed);
            }
            Follow::NotYet => {
                // Already a member once a previous refollow joined it.
                let _ = set.add(WaitSourceKind::SystemNotice, mounts, MOUNTS_TOKEN);
            }
            Follow::Unwatched(err) => record(
                EVENT_CONFIG_UNFOLLOWED,
                Level::Warn,
                "thermald: the machine configuration cannot be watched; a change to it applies when the service restarts",
                &[error_field(err)],
            ),
        }
    }

    /// The fan policy the machine configuration states, defaults where there
    /// is none; [`None`], stated, where it cannot be read or understood, so
    /// the policy in use stands.
    fn load_policy() -> Option<FanPolicy> {
        let read = SystemConfigFile::System
            .read_effective(|path| tairix_rt::read_document(path.as_bytes(), MAX_CONFIG_LEN));
        let parsed = match read {
            Ok(Some(text)) => SystemConfig::parse(&text).ok(),
            Ok(None) | Err(Errno::NotImplemented) => Some(SystemConfig::default()),
            Err(_) => None,
        };
        if parsed.is_none() {
            record(
                EVENT_CONFIG_REFUSED,
                Level::Warn,
                "thermald: the machine configuration could not be read or understood; the fan policy in use stands",
                &[],
            );
        }
        parsed.map(|config| config.cooling_fan)
    }

    /// What the hardware tree holds now, read into `buffer`.
    fn inventory(buffer: &mut Vec<u8>) -> Option<Inventory> {
        let read = read_snapshot(buffer, HW_TREE_READ_START, |into| {
            tairix_rt::hw_tree_read(into).map_err(Errno::from_syscall)
        });
        let nodes = read.and_then(|len| decode_tree(buffer.get(..len).unwrap_or_default()));
        match nodes {
            Ok(nodes) => Some(Inventory::of(&nodes)),
            Err(err) => {
                record(
                    EVENT_TREE_UNREAD,
                    Level::Warn,
                    "thermald: the hardware tree could not be read; what it held stands",
                    &[error_field(err)],
                );
                None
            }
        }
    }

    fn decode_tree(snapshot: &[u8]) -> Result<Vec<HwNode>, Errno> {
        let header = HwTreeHeader::from_bytes(snapshot)?;
        let body = snapshot
            .get(HwTreeHeader::WIRE_LEN..)
            .ok_or(Errno::BadMagic)?;
        let (records, trailing) = body.as_chunks::<{ HwNode::WIRE_LEN }>();
        if !trailing.is_empty() || u64::try_from(records.len()) != Ok(header.node_count()) {
            return Err(Errno::BadMagic);
        }
        records
            .iter()
            .map(|record| HwNode::from_bytes(record))
            .collect()
    }

    /// Answer the status calls queued, at most [`CALLS_PER_WAKE`].
    fn answer(service: &mut Thermald, machine: &mut RtMachine) {
        let mut request = [0u8; THERMAL_REQUEST_LEN];
        let mut reply = [0u8; THERMAL_MAX_REPLY];
        for _ in 0..CALLS_PER_WAKE {
            let Ok(Some(call)) = tairix_rt::call_recv_ready(THERMAL_ENDPOINT, &mut request) else {
                return;
            };
            let status = ThermalRequest::decode(request.get(..call.len).unwrap_or_default())
                .map(|ThermalRequest::Status| service.status(machine));
            // A reply that cannot be framed goes out empty, which the caller
            // decodes as a refusal.
            let len = encode_status(&mut reply, status.as_ref().map_err(|err| *err)).unwrap_or(0);
            // A caller that ended before its answer has nobody left to tell.
            let _ = tairix_rt::call_reply(THERMAL_ENDPOINT, call.ticket, &reply[..len]);
        }
    }

    /// The sensors and the header, over their channels.
    struct RtMachine {
        /// Where the one outstanding call's reply, or its deadline, wakes
        /// the service.
        calls: WaitSet,
    }

    impl RtMachine {
        /// Call `endpoint`, its answer in `reply` within [`CALL_DEADLINE_NS`].
        fn call(&self, endpoint: u64, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            self.calls.add(WaitSourceKind::CallReply, endpoint, 0)?;
            let answer = self.answered(endpoint, request, reply);
            let _ = tairix_rt::waitset_ctl(
                self.calls.handle(),
                WaitSetOp::Del,
                WaitSourceKind::CallReply,
                endpoint,
                0,
            );
            answer
        }

        fn answered(
            &self,
            endpoint: u64,
            request: &[u8],
            reply: &mut [u8],
        ) -> Result<usize, Errno> {
            let ticket = tairix_rt::call_post(endpoint, request, CALL_DEADLINE_NS)
                .map_err(Errno::from_syscall)?;
            loop {
                match tairix_rt::call_reap(endpoint, ticket, reply).map_err(Errno::from_syscall) {
                    Err(Errno::WouldBlock) => {}
                    reaped => return reaped,
                }
                // The deadline readies the member too, so this wait ends.
                if let Err(err) = self.calls.wait_until(u64::MAX) {
                    let _ = tairix_rt::call_cancel(endpoint, ticket);
                    return Err(err);
                }
            }
        }

        fn header(
            &self,
            endpoint: u64,
            request: HeaderRequest,
            reply: &mut [u8],
        ) -> Result<usize, Errno> {
            let mut frame = [0u8; HEADER_REQUEST_LEN];
            request.encode(&mut frame)?;
            self.call(endpoint, &frame, reply)
        }

        fn change(&self, endpoint: u64, request: HeaderRequest) -> Result<(), Errno> {
            let mut reply = [0u8; HEADER_MAX_REPLY];
            let len = self.header(endpoint, request, &mut reply)?;
            decode_status(reply.get(..len).unwrap_or_default())
        }
    }

    impl Machine for RtMachine {
        fn read_sensor(&mut self, endpoint: u64) -> Result<Millicelsius, Errno> {
            let mut request = [0u8; SENSOR_REQUEST_LEN];
            encode_read(&mut request)?;
            let mut reply = [0u8; SENSOR_REPLY_LEN];
            let len = self.call(endpoint, &request, &mut reply)?;
            decode_reading(reply.get(..len).unwrap_or_default())
        }

        fn claim(
            &mut self,
            endpoint: u64,
            line: u8,
            polarity: Polarity,
            asserted: bool,
        ) -> Result<(), Errno> {
            self.change(
                endpoint,
                HeaderRequest::Claim {
                    line,
                    polarity,
                    asserted,
                },
            )
        }

        fn drive(&mut self, endpoint: u64, line: u8, asserted: bool) -> Result<(), Errno> {
            self.change(endpoint, HeaderRequest::Drive { line, asserted })
        }

        fn release(&mut self, endpoint: u64, line: u8) -> Result<(), Errno> {
            self.change(endpoint, HeaderRequest::Release { line })
        }

        fn lines(&mut self, endpoint: u64) -> Result<LineTable, Errno> {
            let mut reply = [0u8; HEADER_MAX_REPLY];
            let len = self.header(endpoint, HeaderRequest::Lines, &mut reply)?;
            decode_lines(reply.get(..len).unwrap_or_default())
        }

        fn now_us(&self) -> u64 {
            tairix_rt::clock_get() / 1_000
        }
    }

    /// The fan's decisions, written to the system log.
    struct LogJournal;

    impl Journal for LogJournal {
        fn record(&mut self, entry: Entry) {
            let line_field = |line: u8| Field {
                key: "line",
                value: FieldValue::UnsignedInt(u64::from(line)),
            };
            match entry {
                Entry::Claimed { line } => record(
                    EVENT_CLAIMED,
                    Level::Info,
                    "thermald: the fan's line was taken",
                    &[line_field(line)],
                ),
                Entry::Released { line } => record(
                    EVENT_RELEASED,
                    Level::Info,
                    "thermald: the fan's line was given back",
                    &[line_field(line)],
                ),
                Entry::Lost { line, refusal } => record(
                    EVENT_LOST,
                    Level::Warn,
                    "thermald: the header no longer honours the fan's line; taking it again",
                    &[line_field(line), error_field(refusal)],
                ),
                Entry::Switched { running } => record(
                    EVENT_SWITCHED,
                    Level::Info,
                    if running {
                        "thermald: the fan was started"
                    } else {
                        "thermald: the fan was stopped"
                    },
                    &[],
                ),
                Entry::Fault { fault, refusal } => {
                    let fault = Field {
                        key: "fault",
                        value: FieldValue::UnsignedInt(fault as u64),
                    };
                    match refusal {
                        Some(err) => record(
                            EVENT_FAULT,
                            Level::Warn,
                            "thermald: the fan is not as its policy asks",
                            &[fault, error_field(err)],
                        ),
                        None => record(
                            EVENT_FAULT,
                            Level::Warn,
                            "thermald: the fan is not as its policy asks",
                            &[fault],
                        ),
                    }
                }
                Entry::Recovered => record(
                    EVENT_RECOVERED,
                    Level::Info,
                    "thermald: the fan is as its policy asks again",
                    &[],
                ),
            }
        }
    }

    fn error_field(err: Errno) -> Field<'static> {
        Field {
            key: "error",
            value: FieldValue::Error(err),
        }
    }

    fn record(id: EventId, level: Level, message: &'static str, fields: &[Field<'_>]) {
        tairix_log::log(
            &LogSink,
            &tairix_log::Event {
                level,
                id,
                message,
                fields,
            },
        );
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
