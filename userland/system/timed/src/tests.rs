//! Host tests for the service's orchestration.
//!
//! Every case drives the whole parent path — the clock policy, the engine, the
//! sandboxed evaluation over the in-process loopback worker, the persisted
//! record, and the audit records — with no processes and no sockets.

use super::{
    events, retune, watch_verdict, Clock, NetworkSettings, NetworkTime, RecordStore, Retune,
    RtcSource, Step, Timed, TimedConfig, Transport, WatchVerdict, CONFIG_RETRY_ATTEMPTS,
    CONFIG_RETRY_BASE_NANOS, RTC_RETRY_ATTEMPTS, RTC_RETRY_BASE_NANOS,
};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use tairix_abi::driver::rtc::RtcStatus;
use tairix_abi::rtc_ipc::RtcReading;
use tairix_abi::time::{Duration64, Time64};
use tairix_abi::{Errno, FieldValue, WallClockReading, WallTimeState, RELEASE_EPOCH_SECS};
use tairix_log::{Event, EventId, Sink};
use tairix_net::addr::{IpAddr, Ipv4Addr};
use tairix_net::ntp::{NtpTimestamp, PACKET_LEN};
use tairix_sandbox::host::ParserSandbox;
use tairix_sandbox::loopback::LoopbackLauncher;
use tairix_sandbox::timesync::TimeSyncService;
use tairix_sysconfig::RefreshCadence;
use tairix_timesync::{select_servers, ServerSelection, ServerSource, SyncRecord, STALE_BOOT_GAP};
use tairix_util::retry::RetryLadder;

/// One captured record: its event id and its string fields.
type Recorded = (EventId, Vec<(String, String)>);

/// Captures every logged event id and its string fields, so a case can assert
/// both what was audited and what the record said.
#[derive(Clone, Default)]
struct RecordingSink {
    events: Rc<RefCell<Vec<Recorded>>>,
}

impl Sink for RecordingSink {
    fn write_event(&self, event: &Event<'_>) {
        let strings = event
            .fields
            .iter()
            .filter_map(|field| match field.value {
                FieldValue::Str(text) => Some((String::from(field.key), String::from(text))),
                _ => None,
            })
            .collect();
        self.events.borrow_mut().push((event.id, strings));
    }
}

impl RecordingSink {
    fn saw(&self, id: EventId) -> bool {
        self.count(id) > 0
    }

    fn count(&self, id: EventId) -> usize {
        self.events.borrow().iter().filter(|e| e.0 == id).count()
    }

    /// The value of the `key` string field on the first `id` record.
    fn field(&self, id: EventId, key: &str) -> Option<String> {
        self.events
            .borrow()
            .iter()
            .find(|record| record.0 == id)?
            .1
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    }
}

/// A settable in-memory clock: the monotonic reading the test advances, and
/// the wall reading the service reads and writes.
#[derive(Clone)]
struct FakeClock {
    monotonic: Rc<RefCell<Duration64>>,
    wall: Rc<RefCell<Result<WallClockReading, Errno>>>,
    set_result: Result<(), Errno>,
    sets: Rc<RefCell<Vec<(Time64, WallTimeState)>>>,
}

impl FakeClock {
    fn unset() -> Self {
        Self {
            monotonic: Rc::new(RefCell::new(Duration64::ZERO)),
            wall: Rc::new(RefCell::new(Ok(WallClockReading::new(
                Time64::from_secs(0),
                WallTimeState::Unset,
            )))),
            set_result: Ok(()),
            sets: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn reading(time: Time64, state: WallTimeState) -> Self {
        let clock = Self::unset();
        *clock.wall.borrow_mut() = Ok(WallClockReading::new(time, state));
        clock
    }
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration64 {
        *self.monotonic.borrow()
    }

    fn wall(&self) -> Result<WallClockReading, Errno> {
        *self.wall.borrow()
    }

    fn set_wall(&self, time: Time64, state: WallTimeState) -> Result<(), Errno> {
        self.set_result?;
        self.sets.borrow_mut().push((time, state));
        *self.wall.borrow_mut() = Ok(WallClockReading::new(time, state));
        Ok(())
    }
}

/// An in-memory record document.
#[derive(Clone, Default)]
struct FakeStore {
    document: Rc<RefCell<Option<Vec<u8>>>>,
    /// What every write is refused with, if anything; shared, so a case can
    /// make the store writable partway through.
    write_result: Rc<Cell<Option<Errno>>>,
}

impl RecordStore for FakeStore {
    fn read(&self) -> Result<Option<Vec<u8>>, Errno> {
        Ok(self.document.borrow().clone())
    }

    fn write(&self, bytes: &[u8]) -> Result<(), Errno> {
        if let Some(err) = self.write_result.get() {
            return Err(err);
        }
        *self.document.borrow_mut() = Some(bytes.to_vec());
        Ok(())
    }
}

/// One `(server index, packet)` pair a fake transport was handed.
type SentPacket = (u8, Vec<u8>);

/// A transport that records every packet it was handed, and can refuse.
#[derive(Clone, Default)]
struct FakeTransport {
    sent: Rc<RefCell<Vec<SentPacket>>>,
    refuse: Option<Errno>,
}

impl Transport for FakeTransport {
    fn send(&mut self, index: u8, packet: &[u8]) -> Result<(), Errno> {
        if let Some(err) = self.refuse {
            return Err(err);
        }
        self.sent.borrow_mut().push((index, packet.to_vec()));
        Ok(())
    }
}

/// An [`RtcSource`] double: answers a programmed reading and records every
/// instant written back.
#[derive(Clone)]
struct FakeRtc {
    /// What each successive `read` answers; the last entry repeats, so a test
    /// that never changes it models a chip with a settled answer.
    reads: Rc<RefCell<Vec<Result<RtcReading, Errno>>>>,
    written: Rc<RefCell<Vec<Time64>>>,
    refuse_set: Option<Errno>,
}

impl FakeRtc {
    /// A board with no clock chip: nothing serves the endpoint, ever, so the
    /// service climbs its whole ladder looking for one.
    fn absent() -> Self {
        Self::answering(vec![Err(Errno::NotFound)])
    }

    /// A chip that answered but has nothing to vouch for — a flat backup
    /// cell. The question is settled, so no ladder is armed; this is the
    /// default for every case that is about the network engine rather than
    /// the RTC.
    fn no_reading() -> Self {
        Self::answering(vec![Ok(reading(None))])
    }

    /// A chip vouching for `time`.
    fn holding(time: Time64) -> Self {
        Self::answering(vec![Ok(reading(Some(time)))])
    }

    fn answering(reads: Vec<Result<RtcReading, Errno>>) -> Self {
        Self {
            reads: Rc::new(RefCell::new(reads)),
            written: Rc::new(RefCell::new(Vec::new())),
            refuse_set: None,
        }
    }
}

impl RtcSource for FakeRtc {
    fn read(&mut self) -> Result<RtcReading, Errno> {
        let mut reads = self.reads.borrow_mut();
        if reads.len() > 1 {
            reads.remove(0)
        } else {
            reads.first().copied().unwrap_or(Err(Errno::NotFound))
        }
    }

    fn set(&mut self, time: Time64) -> Result<(), Errno> {
        if let Some(err) = self.refuse_set {
            return Err(err);
        }
        self.written.borrow_mut().push(time);
        Ok(())
    }
}

/// A chip reading carrying `time` (or none), with an otherwise healthy
/// one-second, battery-backed status.
fn reading(time: Option<Time64>) -> RtcReading {
    RtcReading {
        time,
        status: RtcStatus {
            precision: Duration64::from_secs(1),
            battery_backed: true,
            oscillator_stopped: time.is_none(),
        },
    }
}

type TestService = Timed<
    FakeClock,
    FakeStore,
    FakeTransport,
    LoopbackLauncher<fn() -> TimeSyncService>,
    RecordingSink,
    FakeRtc,
>;

/// A wall instant comfortably inside the plausibility window.
fn plausible() -> Time64 {
    Time64::from_secs(RELEASE_EPOCH_SECS + 86_400)
}

/// The servers a test machine boots with: two, named by the operator.
fn configured() -> ServerSelection {
    select_servers(&[String::from("a.test"), String::from("b.test")], &[])
}

/// A machine nobody named a server for, and whose network offered none: the
/// built-in fallback stands.
fn fallback() -> ServerSelection {
    select_servers(&[], &[])
}

fn service(
    clock: FakeClock,
    store: FakeStore,
    transport: FakeTransport,
    selection: ServerSelection,
    sink: RecordingSink,
) -> TestService {
    service_with_rtc(
        clock,
        store,
        transport,
        selection,
        sink,
        FakeRtc::no_reading(),
    )
}

fn service_with_rtc(
    clock: FakeClock,
    store: FakeStore,
    transport: FakeTransport,
    selection: ServerSelection,
    sink: RecordingSink,
    rtc: FakeRtc,
) -> TestService {
    Timed::new(TimedConfig {
        clock,
        store,
        sandbox: ParserSandbox::new(
            LoopbackLauncher::new(TimeSyncService::default as fn() -> TimeSyncService),
            sink.clone(),
        ),
        sink,
        rtc,
        network: Some(network_time(transport, selection)),
    })
}

/// The network half over `selection` at the default cadence.
fn network_time(
    transport: FakeTransport,
    selection: ServerSelection,
) -> NetworkTime<FakeTransport> {
    NetworkTime {
        transport,
        selection,
        refresh: RefreshCadence::Daily.interval(),
        entropy: 0x0F0F_0F0F_0F0F_0F0F,
    }
}

/// A service whose network time is switched off.
fn service_off(clock: FakeClock, sink: RecordingSink, rtc: FakeRtc) -> TestService {
    Timed::new(TimedConfig {
        clock,
        store: FakeStore::default(),
        sandbox: ParserSandbox::new(
            LoopbackLauncher::new(TimeSyncService::default as fn() -> TimeSyncService),
            sink.clone(),
        ),
        sink,
        rtc,
        network: None,
    })
}

/// Seconds from the NTP epoch (1900) to the Unix epoch (1970).
const NTP_UNIX_DELTA: i64 = 2_208_988_800;

/// A well-formed stratum-2 server reply echoing `nonce` and reporting `at`.
fn reply(nonce: u64, at: Time64) -> [u8; PACKET_LEN] {
    let field = u32::try_from((at.secs() + NTP_UNIX_DELTA).rem_euclid(1 << 32))
        .expect("reduced modulo 2^32");
    let ts = NtpTimestamp::from_raw(u64::from(field) << 32);
    let mut p = [0u8; PACKET_LEN];
    p[0] = (4 << 3) | 4; // version 4, mode 4 (server)
    p[1] = 2; // stratum
    p[24..32].copy_from_slice(&nonce.to_be_bytes());
    p[32..40].copy_from_slice(&ts.raw().to_be_bytes());
    p[40..48].copy_from_slice(&ts.raw().to_be_bytes());
    p
}

/// Drive the service to an in-flight request, returning the nonce it carries
/// and a receive instant 20 ms later.
fn in_flight(svc: &mut TestService) -> (u64, Duration64) {
    const NONCE: u64 = 0xDEAD_BEEF_FEED_FACE;
    let due = svc.next_deadline().expect("a query is scheduled");
    assert_eq!(svc.poll(due, NONCE), Step::Queried(0));
    (
        NONCE,
        Duration64::from_nanos(due.saturating_total_nanos() + 20_000_000),
    )
}

#[test]
fn an_unset_clock_is_set_from_a_validated_sample_and_recorded() {
    let clock = FakeClock::unset();
    let store = FakeStore::default();
    let sink = RecordingSink::default();
    let mut svc = service(
        clock.clone(),
        store.clone(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    assert!(sink.saw(events::SERVICE_READY));

    let (nonce, at) = in_flight(&mut svc);
    let Step::ClockSet(update) = svc.on_datagram(at, &reply(nonce, plausible())) else {
        panic!("a well-formed reply must set the clock");
    };

    // A network sample is always `Trusted`: the reading comes wholly from the
    // network time source.
    assert_eq!(update.state, WallTimeState::Trusted);
    // The instant applied is exactly the one the update named — the server's
    // transmit instant advanced by half the measured round trip, never the
    // raw packet field.
    assert_eq!(
        clock.sets.borrow().as_slice(),
        &[(update.wall, WallTimeState::Trusted)]
    );
    assert_eq!(update.wall.secs(), plausible().secs());
    assert_eq!(update.wall.subsec_nanos(), 10_000_000);
    assert!(sink.saw(events::CLOCK_SET));

    // The record now says when time was last seen, so a future boot can tell
    // a short power-off from a long one.
    let document = store
        .document
        .borrow()
        .clone()
        .expect("a record was written");
    assert_eq!(
        SyncRecord::from_bytes(&document),
        SyncRecord::EMPTY.synced_at(update.wall)
    );
    assert_eq!(svc.record(), SyncRecord::EMPTY.synced_at(update.wall));
}

#[test]
fn a_wrong_nonce_reply_is_refused_and_leaves_the_transaction_outstanding() {
    // The anti-spoof gate: an injected flood must neither set the clock nor
    // cancel the real answer, or it becomes a denial of service.
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let mut svc = service(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    let outstanding = svc.next_deadline();

    for spoof in 1..8u64 {
        assert_eq!(
            svc.on_datagram(at, &reply(nonce ^ spoof, plausible())),
            Step::Idle
        );
    }
    assert!(clock.sets.borrow().is_empty(), "the clock must not be set");
    assert_eq!(
        svc.next_deadline(),
        outstanding,
        "the real answer is still awaited"
    );
    assert!(!sink.saw(events::CLOCK_SET));

    // ...and the genuine reply still lands.
    assert!(matches!(
        svc.on_datagram(at, &reply(nonce, plausible())),
        Step::ClockSet(_)
    ));
    assert_eq!(clock.sets.borrow().len(), 1);
}

#[test]
fn an_implausible_server_instant_never_reaches_the_clock() {
    // The window is a validation bound: a server claiming a time before this
    // release existed, or a century hence, is refused whole and audited.
    for claimed in [
        Time64::from_secs(RELEASE_EPOCH_SECS - 1),
        Time64::from_secs(0),
        Time64::from_secs(RELEASE_EPOCH_SECS + tairix_abi::PLAUSIBLE_FUTURE_SECS + 1),
    ] {
        let clock = FakeClock::unset();
        let sink = RecordingSink::default();
        let mut svc = service(
            clock.clone(),
            FakeStore::default(),
            FakeTransport::default(),
            configured(),
            sink.clone(),
        );
        let (nonce, at) = in_flight(&mut svc);
        assert_eq!(svc.on_datagram(at, &reply(nonce, claimed)), Step::NoSample);
        assert!(
            clock.sets.borrow().is_empty(),
            "{claimed:?} must not reach the clock"
        );
        assert!(sink.saw(events::SAMPLE_REFUSED));
    }
}

#[test]
fn a_believed_clock_waits_the_refresh_cadence_instead_of_querying_at_boot() {
    // A machine with a working RTC must not query a public server merely
    // because it rebooted.
    let clock = FakeClock::reading(plausible(), WallTimeState::Firmware);
    let store = FakeStore::default();
    *store.document.borrow_mut() =
        Some(SyncRecord::EMPTY.synced_at(plausible()).to_bytes().to_vec());
    let sink = RecordingSink::default();
    let transport = FakeTransport::default();
    let mut svc = service(clock, store, transport.clone(), configured(), sink);

    // Nothing is due for the best part of a day, and polling early sends
    // nothing.
    let due = svc.next_deadline().expect("a refresh is scheduled");
    assert!(
        due.secs() > 3_600,
        "the refresh must not be imminent: {due:?}"
    );
    assert_eq!(svc.poll(Duration64::from_secs(60), 1), Step::Idle);
    assert!(transport.sent.borrow().is_empty());
}

#[test]
fn a_record_from_a_long_power_off_makes_the_boot_query_urgent() {
    // The one input that distinguishes "off for an hour" from "off for a
    // month", which no clock reading alone can tell.
    let seen = plausible();
    let now = seen
        .saturating_add(STALE_BOOT_GAP)
        .saturating_add(Duration64::from_secs(3_600));
    let store = FakeStore::default();
    *store.document.borrow_mut() = Some(SyncRecord::EMPTY.synced_at(seen).to_bytes().to_vec());
    let sink = RecordingSink::default();
    let svc = service(
        FakeClock::reading(now, WallTimeState::Firmware),
        store,
        FakeTransport::default(),
        configured(),
        sink,
    );
    let due = svc.next_deadline().expect("a query is scheduled");
    assert!(
        due <= tairix_timesync::INITIAL_DELAY_SPAN,
        "a stale boot must query at once, not on the refresh cadence: {due:?}"
    );
}

#[test]
fn a_corrupt_record_makes_no_rule_fire_rather_than_firing_on_a_fiction() {
    let store = FakeStore::default();
    *store.document.borrow_mut() = Some(vec![0xFF; 33]);
    let sink = RecordingSink::default();
    let svc = service(
        FakeClock::reading(plausible(), WallTimeState::Firmware),
        store,
        FakeTransport::default(),
        configured(),
        sink,
    );
    assert_eq!(svc.record(), SyncRecord::EMPTY);
    // The clock is believed, so only the refresh cadence applies.
    let due = svc.next_deadline().expect("a refresh is scheduled");
    assert!(due.secs() > 3_600, "{due:?}");
}

/// A machine nobody named a server for still keeps time: the built-in
/// fallback is the lowest tier, so there is always somewhere to ask.
///
/// This is the behaviour that replaced "no servers configured, so the clock
/// is never set from the network" — the reason a stock installation used to
/// boot with an unset clock forever.
#[test]
fn a_machine_with_nothing_named_queries_the_built_in_fallback() {
    let sink = RecordingSink::default();
    let transport = FakeTransport::default();
    let mut svc = service(
        FakeClock::unset(),
        FakeStore::default(),
        transport.clone(),
        fallback(),
        sink.clone(),
    );
    assert!(sink.saw(events::SERVICE_READY));
    assert!(!svc.is_exhausted(), "the fallback tier is never empty");
    let due = svc.next_deadline().expect("a query is scheduled");
    assert_eq!(svc.poll(due, 1), Step::Queried(0));
    assert_eq!(
        transport.sent.borrow().len(),
        1,
        "the fallback servers are actually queried"
    );
}

/// The start-up record names the tier, so an operator can tell whose server
/// their machine is asking without reading the configuration.
#[test]
fn the_startup_record_names_the_source_of_the_servers() {
    for (selection, expected) in [
        (configured(), "configured"),
        (
            select_servers(&[], &[IpAddr::V4(Ipv4Addr::new(192, 168, 66, 1))]),
            "network",
        ),
        (fallback(), "fallback"),
    ] {
        let sink = RecordingSink::default();
        let _svc = service(
            FakeClock::unset(),
            FakeStore::default(),
            FakeTransport::default(),
            selection,
            sink.clone(),
        );
        assert_eq!(
            sink.field(events::SERVICE_READY, "source").as_deref(),
            Some(expected),
        );
    }
}

/// A network-supplied server needs no resolver: the address came with it.
#[test]
fn a_network_supplied_server_is_queried_without_a_name_to_resolve() {
    let learned = IpAddr::V4(Ipv4Addr::new(192, 168, 66, 1));
    let sink = RecordingSink::default();
    let selection = select_servers(&[], &[learned]);
    assert_eq!(selection.servers[0].address, Some(learned));
    let transport = FakeTransport::default();
    let mut svc = service(
        FakeClock::unset(),
        FakeStore::default(),
        transport.clone(),
        selection,
        sink.clone(),
    );
    let due = svc.next_deadline().expect("a query is scheduled");
    assert_eq!(svc.poll(due, 1), Step::Queried(0));
    assert_eq!(
        sink.field(events::SERVICE_READY, "source").as_deref(),
        Some("network")
    );
}

/// Exhaustion is a state the engine can sit in for the rest of the boot, so
/// the warning is edge-triggered.
///
/// It used to be re-emitted on every poll while the state held, which filled
/// the console with the same line indefinitely — a real Pi boot log showed it
/// repeating from 3 s to past 65 s.
#[test]
fn the_exhaustion_warning_is_reported_once_not_on_every_poll() {
    let sink = RecordingSink::default();
    let learned = IpAddr::V4(Ipv4Addr::new(192, 168, 66, 1));
    let mut svc = service(
        FakeClock::unset(),
        FakeStore::default(),
        FakeTransport::default(),
        select_servers(&[], &[learned]),
        sink.clone(),
    );
    // Retire the one server the way a real one refuses: a Kiss-o'-Death.
    let (nonce, at) = in_flight(&mut svc);
    let mut kiss = reply(nonce, plausible());
    kiss[1] = 0;
    kiss[12..16].copy_from_slice(b"DENY");
    assert_eq!(svc.on_datagram(at, &kiss), Step::NoSample);
    assert!(svc.is_exhausted(), "the only server refused");
    for secs in [1_i64, 2, 5, 30, 60] {
        assert_eq!(svc.poll(Duration64::from_secs(secs), 1), Step::Idle);
    }
    assert_eq!(
        sink.count(events::SERVERS_EXHAUSTED),
        1,
        "the state must be announced on entry, never re-announced per poll"
    );
}

#[test]
fn a_refused_send_is_audited_and_left_to_the_engines_own_backoff() {
    // No packet left the machine, so the request must not be retried on the
    // spot: the response timeout ends the transaction and the backoff paces
    // the next attempt.
    let sink = RecordingSink::default();
    let transport = FakeTransport {
        refuse: Some(Errno::NetworkUnreachable),
        ..FakeTransport::default()
    };
    let mut svc = service(
        FakeClock::unset(),
        FakeStore::default(),
        transport,
        configured(),
        sink.clone(),
    );
    let due = svc.next_deadline().expect("a query is scheduled");
    assert_eq!(
        svc.poll(due, 1),
        Step::NotSent(Errno::NetworkUnreachable),
        "the refusal is surfaced, not swallowed"
    );
    assert!(sink.saw(events::QUERY_NOT_SENT));
    // A second poll at the same instant sends nothing more: the transaction
    // is awaiting its own timeout.
    assert_eq!(svc.poll(due, 2), Step::Idle);
    let next = svc.next_deadline().expect("still scheduled");
    assert!(next > due, "the next attempt is later, never immediate");
}

#[test]
fn a_kernel_refusal_to_set_the_clock_is_audited_and_leaves_no_record() {
    // The service holds `CAP_TIME_SET`, so a refusal is a defect or a revoked
    // grant. It must be loud, and it must not claim a sync that never landed.
    let clock = FakeClock {
        set_result: Err(Errno::PermissionDenied),
        ..FakeClock::unset()
    };
    let store = FakeStore::default();
    let sink = RecordingSink::default();
    let mut svc = service(
        clock,
        store.clone(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    assert_eq!(
        svc.on_datagram(at, &reply(nonce, plausible())),
        Step::ClockRefused(Errno::PermissionDenied)
    );
    assert!(sink.saw(events::CLOCK_SET_REFUSED));
    assert!(!sink.saw(events::CLOCK_SET));
    assert!(
        store.document.borrow().is_none(),
        "an unapplied sample must not be recorded as one"
    );
    assert_eq!(svc.record(), SyncRecord::EMPTY);
}

#[test]
fn a_clock_that_was_set_survives_an_unwritable_record() {
    // Losing the record costs a future boot the short-vs-long power-off
    // distinction; it must never cost the clock.
    let clock = FakeClock::unset();
    let store = FakeStore {
        write_result: Rc::new(Cell::new(Some(Errno::PermissionDenied))),
        ..FakeStore::default()
    };
    let sink = RecordingSink::default();
    let mut svc = service(
        clock.clone(),
        store,
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    assert!(matches!(
        svc.on_datagram(at, &reply(nonce, plausible())),
        Step::ClockSet(_)
    ));
    assert_eq!(clock.sets.borrow().len(), 1);
    assert!(sink.saw(events::CLOCK_SET));
    assert!(sink.saw(events::RECORD_NOT_WRITTEN));
}

#[test]
fn a_kiss_of_death_retires_the_server_and_is_audited() {
    let sink = RecordingSink::default();
    let clock = FakeClock::unset();
    let mut svc = service(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    let mut kiss = reply(nonce, plausible());
    kiss[1] = 0; // stratum 0 — a Kiss-o'-Death
    kiss[12..16].copy_from_slice(b"DENY");
    assert_eq!(svc.on_datagram(at, &kiss), Step::NoSample);
    assert!(sink.saw(events::SERVER_RETIRED));
    assert!(clock.sets.borrow().is_empty());
}

#[test]
fn a_rate_limit_kiss_widens_the_interval_and_is_audited() {
    let sink = RecordingSink::default();
    let mut svc = service(
        FakeClock::unset(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    let mut kiss = reply(nonce, plausible());
    kiss[1] = 0;
    kiss[12..16].copy_from_slice(b"RATE");
    assert_eq!(svc.on_datagram(at, &kiss), Step::NoSample);
    assert!(sink.saw(events::SERVER_RATE_LIMITED));
}

#[test]
fn a_datagram_arriving_with_nothing_outstanding_changes_nothing() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let mut svc = service(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink,
    );
    // No query has gone out yet.
    assert_eq!(
        svc.on_datagram(Duration64::ZERO, &reply(1, plausible())),
        Step::Idle
    );
    assert!(clock.sets.borrow().is_empty());
}

#[test]
fn the_event_ids_are_frozen_inside_the_reserved_range() {
    // The identifiers are a contract with audit-log consumers; renumbering
    // them is a break this test refuses.
    for id in [
        events::SERVICE_READY,
        events::SERVICE_UNAVAILABLE,
        events::SERVERS_FROM_FALLBACK,
        events::CLOCK_SET,
        events::CLOCK_SET_REFUSED,
        events::SAMPLE_REFUSED,
        events::SERVER_RETIRED,
        events::SERVER_RATE_LIMITED,
        events::EVALUATION_FAILED,
        events::QUERY_NOT_SENT,
        events::RECORD_NOT_WRITTEN,
        events::SERVERS_EXHAUSTED,
        events::RTC_CLOCK_SET,
        events::RTC_NO_READING,
        events::RTC_UNAVAILABLE,
        events::RTC_WRITEBACK,
        events::RTC_WRITEBACK_REFUSED,
        events::NETWORK_TIME_CHANGED,
        events::CLOCK_DOUBTED,
        events::CONFIG_REFUSED,
        events::CONFIG_NOT_FOLLOWED,
    ] {
        assert!(
            (events::TIMED_RANGE_START..events::TIMED_RANGE_END).contains(&id.0),
            "{id:?} is outside the reserved range"
        );
    }
    assert_eq!(events::SERVICE_READY, EventId(23_001));
    assert_eq!(events::RTC_WRITEBACK_REFUSED, EventId(23_017));
    assert_eq!(events::NETWORK_TIME_CHANGED, EventId(23_018));
    assert_eq!(events::CLOCK_DOUBTED, EventId(23_019));
    assert_eq!(events::CONFIG_NOT_FOLLOWED, EventId(23_021));
    assert_eq!(events::TIMED_RANGE_START, 23_000);
    assert_eq!(events::TIMED_RANGE_END, 24_000);
}

/// The service starts before the root holding its store is mounted, so its first read finds nothing. The ladder must arm on that, or
/// the service never re-reads and the clock is never set — which is exactly
/// what stranded it when a failed open was mistaken for a volume-less boot.
#[test]
fn the_reread_ladder_arms_whenever_no_server_is_configured() {
    let armed = RetryLadder::arm(0, CONFIG_RETRY_BASE_NANOS, CONFIG_RETRY_ATTEMPTS, false)
        .expect("an unconfigured service re-reads");
    assert_eq!(armed.at, CONFIG_RETRY_BASE_NANOS);
}

/// Nothing to wait for once a server is known.
#[test]
fn the_reread_ladder_stays_disarmed_when_a_server_is_configured() {
    assert_eq!(
        RetryLadder::arm(0, CONFIG_RETRY_BASE_NANOS, CONFIG_RETRY_ATTEMPTS, true),
        None
    );
}

/// The rungs double and the ladder is finite, so a machine that genuinely has
/// no server configured stops reading instead of re-reading for the whole
/// boot. The window must still outlast an unlock by a wide margin.
#[test]
fn the_reread_ladder_doubles_and_is_spent_after_its_attempts() {
    let mut rung =
        RetryLadder::arm(0, CONFIG_RETRY_BASE_NANOS, CONFIG_RETRY_ATTEMPTS, false).expect("armed");
    let mut at = rung.at;
    let mut climbed = 1;
    while rung.advance(at) {
        assert!(rung.at > at, "each rung waits longer than the last");
        at = rung.at;
        climbed += 1;
    }
    assert_eq!(climbed, CONFIG_RETRY_ATTEMPTS);
    // Rungs on time sum to (2^attempts - 1) base waits: ~17 minutes, against
    // a root mount measured in seconds once any passphrase is in.
    let window = u64::from(2u32.pow(CONFIG_RETRY_ATTEMPTS) - 1) * CONFIG_RETRY_BASE_NANOS;
    assert_eq!(at, window);
    assert!(
        at >= 600_000_000_000,
        "the window must outlast any unlock, got {at} ns"
    );
}

/// The deadline is absolute, so a rung reached late schedules from when it
/// actually ran rather than compounding the lateness.
#[test]
fn a_late_rung_schedules_from_the_moment_it_ran() {
    let mut rung =
        RetryLadder::arm(0, CONFIG_RETRY_BASE_NANOS, CONFIG_RETRY_ATTEMPTS, false).expect("armed");
    let late = rung.at + 60_000_000_000;
    assert!(rung.advance(late));
    assert_eq!(rung.at, late + 2 * CONFIG_RETRY_BASE_NANOS);
}

/// A clock near the end of its range must not wrap the deadline backwards and
/// fire the ladder continuously.
#[test]
fn the_ladder_saturates_instead_of_wrapping_a_late_clock() {
    let mut rung = RetryLadder::arm(
        u64::MAX,
        CONFIG_RETRY_BASE_NANOS,
        CONFIG_RETRY_ATTEMPTS,
        false,
    )
    .expect("armed");
    assert_eq!(rung.at, u64::MAX);
    assert!(rung.advance(u64::MAX));
    assert_eq!(rung.at, u64::MAX);
}

// --- The board's real-time clock ----------------------------------------

#[test]
fn a_chip_reading_establishes_the_clock_as_firmware_before_any_network() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let at = plausible();
    let rtc = FakeRtc::holding(at);
    let _service = service_with_rtc(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        rtc,
    );
    // The service — not the driver — decides the provenance, and it is the
    // firmware rung: a chip reading is believed until the network corrects it.
    assert_eq!(
        clock.sets.borrow().as_slice(),
        &[(at, WallTimeState::Firmware)]
    );
    assert!(sink.saw(events::RTC_CLOCK_SET));
}

#[test]
fn a_chip_with_nothing_to_vouch_for_leaves_the_clock_unset_and_says_why() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let service = service_with_rtc(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        FakeRtc::no_reading(),
    );
    // No fabricated instant reaches the clock.
    assert!(clock.sets.borrow().is_empty());
    assert!(sink.saw(events::RTC_NO_READING));
    // The chip answered, so there is nothing to come back for: the only
    // deadline left is the engine's own first query, which the base RTC delay
    // does not front-run.
    let due = service.next_deadline().expect("a query is scheduled");
    assert!(
        due.saturating_total_nanos() != RTC_RETRY_BASE_NANOS,
        "no RTC rung remains: {due:?}"
    );
}

#[test]
fn a_board_with_no_chip_climbs_a_bounded_ladder_then_stops_asking() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let mut service = service_with_rtc(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        FakeRtc::absent(),
    );
    // The first rung is armed at the base delay, and it is what the reactor
    // wakes for.
    assert_eq!(
        service.next_deadline(),
        Some(Duration64::from_nanos(RTC_RETRY_BASE_NANOS))
    );
    // Climb every rung; the ladder is finite, so the service stops waking for
    // a chip that is never going to answer. Rung `k` falls due at
    // `base * (2^(k+1) - 1)` — the running sum of the doubling waits — so
    // polling at each of those instants climbs exactly one rung per poll.
    let mut now = Duration64::ZERO;
    for k in 0..RTC_RETRY_ATTEMPTS {
        now = Duration64::from_nanos(RTC_RETRY_BASE_NANOS * ((1u64 << (k + 1)) - 1));
        service.poll(now, 1);
    }
    assert!(sink.saw(events::RTC_UNAVAILABLE));
    assert!(clock.sets.borrow().is_empty(), "and never invents a time");
    // The ladder is spent, so a further poll neither wakes for it nor audits
    // its exhaustion twice.
    let before = sink.count(events::RTC_UNAVAILABLE);
    service.poll(
        Duration64::from_nanos(
            now.saturating_total_nanos()
                .saturating_add(3_600_000_000_000),
        ),
        1,
    );
    assert_eq!(sink.count(events::RTC_UNAVAILABLE), before);
}

#[test]
fn a_chip_that_binds_late_still_establishes_the_clock() {
    // The driver is autoloaded after this service starts, so the
    // first read finds no endpoint and the second finds the chip.
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let at = plausible();
    let rtc = FakeRtc::answering(vec![Err(Errno::NotFound), Ok(reading(Some(at)))]);
    let mut service = service_with_rtc(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        rtc,
    );
    assert!(clock.sets.borrow().is_empty(), "not on the first attempt");
    service.poll(Duration64::from_nanos(RTC_RETRY_BASE_NANOS), 1);
    assert_eq!(
        clock.sets.borrow().as_slice(),
        &[(at, WallTimeState::Firmware)]
    );
    assert!(sink.saw(events::RTC_CLOCK_SET));
}

#[test]
fn a_validated_sample_is_written_back_to_the_chip() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let rtc = FakeRtc::no_reading();
    let written = Rc::clone(&rtc.written);
    let mut svc = service_with_rtc(
        clock,
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        rtc,
    );

    // Drive one successful sync and confirm the applied instant reached the
    // chip, so the next boot starts from it with no network.
    let (nonce, at) = in_flight(&mut svc);
    let Step::ClockSet(update) = svc.on_datagram(at, &reply(nonce, plausible())) else {
        panic!("a well-formed reply must set the clock");
    };
    assert_eq!(written.borrow().as_slice(), &[update.wall]);
    assert!(sink.saw(events::RTC_WRITEBACK));
}

#[test]
fn a_board_with_no_chip_records_no_refused_write_back() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let mut rtc = FakeRtc::no_reading();
    rtc.refuse_set = Some(Errno::NotFound);
    let mut svc = service_with_rtc(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        rtc,
    );
    let (nonce, at) = in_flight(&mut svc);
    assert!(matches!(
        svc.on_datagram(at, &reply(nonce, plausible())),
        Step::ClockSet(_)
    ));
    assert!(!sink.saw(events::RTC_WRITEBACK_REFUSED));
    assert!(!sink.saw(events::RTC_WRITEBACK));
    assert_eq!(
        clock.sets.borrow().last().map(|(_, state)| *state),
        Some(WallTimeState::Trusted)
    );
}

#[test]
fn a_chip_that_refuses_the_write_back_does_not_undo_the_sync() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let mut rtc = FakeRtc::no_reading();
    rtc.refuse_set = Some(Errno::PermissionDenied);
    let written = Rc::clone(&rtc.written);
    let mut svc = service_with_rtc(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        rtc,
    );
    let (nonce, at) = in_flight(&mut svc);
    assert!(matches!(
        svc.on_datagram(at, &reply(nonce, plausible())),
        Step::ClockSet(_)
    ));
    assert!(written.borrow().is_empty());
    assert!(sink.saw(events::RTC_WRITEBACK_REFUSED));
    // The machine clock is still correct; only the next boot loses its head
    // start.
    assert_eq!(
        clock.sets.borrow().last().map(|(_, state)| *state),
        Some(WallTimeState::Trusted)
    );
}

// --- Network time on and off -----------------------------------------------

/// Move the fake clock's uptime to `secs`.
fn advance(clock: &FakeClock, secs: i64) {
    *clock.monotonic.borrow_mut() = Duration64::from_secs(secs);
}

/// The servers an operator named after the machine had booted.
fn reconfigured() -> ServerSelection {
    select_servers(&[String::from("c.test")], &[])
}

#[test]
fn network_time_off_still_sets_the_clock_from_the_chip_and_asks_no_server() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let mut svc = service_off(clock.clone(), sink.clone(), FakeRtc::holding(plausible()));
    assert_eq!(
        clock.sets.borrow().as_slice(),
        &[(plausible(), WallTimeState::Firmware)]
    );
    assert_eq!(
        sink.field(events::SERVICE_READY, "network").as_deref(),
        Some("off")
    );
    assert_eq!(svc.source(), None);
    assert_eq!(
        svc.next_deadline(),
        None,
        "nothing to ask and nothing to wait for"
    );
    assert_eq!(svc.poll(Duration64::from_secs(60), 1), Step::Idle);
    assert_eq!(
        svc.on_datagram(Duration64::from_secs(60), &reply(1, plausible())),
        Step::Idle
    );
    assert!(!svc.is_exhausted(), "network time off is not a refusal");
    assert!(!sink.saw(events::SERVERS_EXHAUSTED));
}

#[test]
fn switching_network_time_off_abandons_the_query_in_flight() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let transport = FakeTransport::default();
    let mut svc = service(
        clock.clone(),
        FakeStore::default(),
        transport.clone(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    svc.replace_network(None);
    assert_eq!(svc.source(), None);
    assert_eq!(svc.next_deadline(), None);
    assert_eq!(
        sink.field(events::NETWORK_TIME_CHANGED, "network")
            .as_deref(),
        Some("off")
    );
    // The answer to the abandoned request sets nothing.
    assert_eq!(svc.on_datagram(at, &reply(nonce, plausible())), Step::Idle);
    assert!(clock.sets.borrow().is_empty());
    assert_eq!(transport.sent.borrow().len(), 1);
}

#[test]
fn switching_network_time_off_shuts_the_evaluation_worker_down() {
    let sink = RecordingSink::default();
    let mut svc = service(
        FakeClock::unset(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink,
    );
    let (nonce, at) = in_flight(&mut svc);
    // The reply is evaluated in a worker, which stays for the next one.
    assert!(matches!(
        svc.on_datagram(at, &reply(nonce, plausible())),
        Step::ClockSet(_)
    ));
    assert!(svc.evaluating());
    svc.replace_network(None);
    assert!(!svc.evaluating());
}

#[test]
fn switching_network_time_on_asks_promptly_but_never_inside_the_poll_floor() {
    let clock = FakeClock::reading(plausible(), WallTimeState::Adjusted);
    let sink = RecordingSink::default();
    let mut svc = service_off(clock.clone(), sink.clone(), FakeRtc::no_reading());
    advance(&clock, 1_000);
    let transport = FakeTransport::default();
    svc.replace_network(Some(network_time(transport.clone(), configured())));
    assert_eq!(svc.source(), Some(ServerSource::Configured));
    assert_eq!(
        sink.field(events::NETWORK_TIME_CHANGED, "urgency")
            .as_deref(),
        Some("switched_on")
    );
    let due = svc.next_deadline().expect("a query is scheduled");
    assert!(
        due.saturating_total_nanos()
            <= Duration64::from_secs(1_000).saturating_total_nanos()
                + tairix_timesync::INITIAL_DELAY_SPAN.saturating_total_nanos(),
        "a hand-set clock is corrected promptly once network time is on, got {due:?}"
    );
    assert_eq!(svc.poll(due, 7), Step::Queried(0));

    // Off and on again at once: the next request still waits out the floor
    // after the one just sent.
    svc.replace_network(None);
    advance(&clock, 1_010);
    svc.replace_network(Some(network_time(transport.clone(), configured())));
    let floor = due.saturating_total_nanos() + tairix_net::ntp::MIN_POLL.saturating_total_nanos();
    let next = svc.next_deadline().expect("a query is scheduled");
    assert_eq!(next.saturating_total_nanos(), floor);
    assert_eq!(svc.poll(Duration64::from_nanos(floor - 1), 8), Step::Idle);
    assert_eq!(transport.sent.borrow().len(), 1);
}

/// Replacing the network half once rebuilt the whole service, which read the
/// clock chip again and set the clock back to its reading.
#[test]
fn replacing_the_servers_reads_the_clock_chip_no_more() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let rtc = FakeRtc::answering(vec![
        Ok(reading(Some(plausible()))),
        Ok(reading(Some(Time64::from_secs(plausible().secs() + 60)))),
    ]);
    let mut svc = service_with_rtc(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        fallback(),
        sink.clone(),
        rtc,
    );
    advance(&clock, 40);
    svc.replace_network(Some(network_time(FakeTransport::default(), reconfigured())));
    assert_eq!(svc.source(), Some(ServerSource::Configured));
    assert_eq!(
        clock.sets.borrow().as_slice(),
        &[(plausible(), WallTimeState::Firmware)],
        "the chip is read once, at start"
    );
    assert_eq!(sink.count(events::RTC_CLOCK_SET), 1);
    assert_eq!(
        sink.field(events::NETWORK_TIME_CHANGED, "source")
            .as_deref(),
        Some("configured")
    );
    assert_eq!(
        sink.count(events::SERVICE_READY),
        1,
        "the service did not restart"
    );
}

#[test]
fn a_new_refresh_cadence_keeps_a_retired_server_retired() {
    let clock = FakeClock::unset();
    let sink = RecordingSink::default();
    let mut svc = service(
        clock.clone(),
        FakeStore::default(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    let mut kiss = reply(nonce, plausible());
    kiss[1] = 0;
    kiss[12..16].copy_from_slice(b"DENY");
    assert_eq!(svc.on_datagram(at, &kiss), Step::NoSample);
    *clock.monotonic.borrow_mut() = at;
    svc.set_refresh(RefreshCadence::TwoDays.interval());
    assert_eq!(
        sink.field(events::NETWORK_TIME_CHANGED, "network")
            .as_deref(),
        Some("on")
    );
    // Every later request goes to the server that did not refuse.
    for _ in 0..3 {
        let due = svc.next_deadline().expect("a query is scheduled");
        assert_eq!(svc.poll(due, 9), Step::Queried(1));
        let lapsed = Duration64::from_nanos(
            due.saturating_total_nanos()
                + tairix_net::ntp::RESPONSE_TIMEOUT.saturating_total_nanos(),
        );
        assert_eq!(svc.poll(lapsed, 10), Step::Idle);
    }
}

#[test]
fn a_record_read_once_the_store_is_reachable_doubts_a_chip_that_went_back() {
    let clock = FakeClock::unset();
    let store = FakeStore::default();
    let sink = RecordingSink::default();
    // Before the root is mounted there is no record: the chip is believed.
    let mut svc = service_with_rtc(
        clock.clone(),
        store.clone(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
        FakeRtc::holding(plausible()),
    );
    let believed = svc.next_deadline().expect("a refresh is scheduled");
    assert!(believed > Duration64::from_secs(3_600));
    // Mounted: the record says time was seen a day later than the chip reads.
    let seen = Time64::from_secs(plausible().secs() + 86_400);
    *store.document.borrow_mut() = Some(SyncRecord::EMPTY.synced_at(seen).to_bytes().to_vec());
    advance(&clock, 30);
    svc.store_reachable(5);
    assert_eq!(
        sink.field(events::CLOCK_DOUBTED, "urgency").as_deref(),
        Some("went_backwards")
    );
    assert_eq!(svc.record(), SyncRecord::EMPTY.synced_at(seen));
    let due = svc.next_deadline().expect("a query is scheduled");
    assert!(
        due.saturating_total_nanos()
            <= Duration64::from_secs(30).saturating_total_nanos()
                + tairix_timesync::INITIAL_DELAY_SPAN.saturating_total_nanos()
    );
    // Reachable twice changes nothing more.
    svc.store_reachable(6);
    assert_eq!(sink.count(events::CLOCK_DOUBTED), 1);
}

#[test]
fn a_sync_before_the_store_was_reachable_is_kept_and_written_once_it_is() {
    let clock = FakeClock::unset();
    let store = FakeStore {
        write_result: Rc::new(Cell::new(Some(Errno::NotFound))),
        ..FakeStore::default()
    };
    let sink = RecordingSink::default();
    let mut svc = service(
        clock.clone(),
        store.clone(),
        FakeTransport::default(),
        configured(),
        sink.clone(),
    );
    let (nonce, at) = in_flight(&mut svc);
    let Step::ClockSet(update) = svc.on_datagram(at, &reply(nonce, plausible())) else {
        panic!("a well-formed reply sets the clock");
    };
    assert!(sink.saw(events::RECORD_NOT_WRITTEN));
    // Mounted, holding an older boot's record.
    let older = Time64::from_secs(plausible().secs() - 3_600);
    *store.document.borrow_mut() = Some(SyncRecord::EMPTY.synced_at(older).to_bytes().to_vec());
    store.write_result.set(None);
    svc.store_reachable(5);
    let kept = SyncRecord::EMPTY.synced_at(older).synced_at(update.wall);
    assert_eq!(svc.record(), kept);
    let written = store.document.borrow().clone().expect("a record");
    assert_eq!(SyncRecord::from_bytes(&written), kept);
    assert!(
        !sink.saw(events::CLOCK_DOUBTED),
        "a clock synced this boot is believed"
    );
}

// --- Following the configuration ------------------------------------------

fn settings(on: bool, configured: &[&str], refresh: RefreshCadence) -> NetworkSettings {
    NetworkSettings {
        on,
        configured: configured.iter().map(|name| String::from(*name)).collect(),
        refresh: refresh.interval(),
    }
}

fn no_lease() -> Vec<IpAddr> {
    Vec::new()
}

#[test]
fn switching_network_time_off_and_on_is_answered_in_kind() {
    let on = settings(true, &[], RefreshCadence::Daily);
    let off = settings(false, &[], RefreshCadence::Daily);
    assert_eq!(
        retune(&on, Some(ServerSource::Fallback), &off, no_lease),
        Retune::Off
    );
    assert_eq!(retune(&off, None, &off, no_lease), Retune::Keep);
    assert_eq!(
        retune(&off, None, &on, no_lease),
        Retune::Select(fallback())
    );
}

#[test]
fn an_operators_server_change_applies_whatever_its_tier() {
    let named = settings(true, &["a.test", "b.test"], RefreshCadence::Daily);
    let renamed = settings(true, &["c.test"], RefreshCadence::Daily);
    let cleared = settings(true, &[], RefreshCadence::Daily);
    assert_eq!(
        retune(&named, Some(ServerSource::Configured), &renamed, no_lease),
        Retune::Select(reconfigured())
    );
    // Clearing them falls to a worse tier, and that applies too.
    assert_eq!(
        retune(&named, Some(ServerSource::Configured), &cleared, no_lease),
        Retune::Select(fallback())
    );
    assert_eq!(
        retune(&named, Some(ServerSource::Configured), &named, || panic!(
            "an operator's servers outrank the lease, so it is not asked for"
        )),
        Retune::Keep
    );
}

#[test]
fn only_a_strictly_better_tier_replaces_the_servers_in_use() {
    let held = settings(true, &[], RefreshCadence::Daily);
    let lease = || vec![IpAddr::V4(Ipv4Addr::new(192, 168, 66, 1))];
    let other_lease = || vec![IpAddr::V4(Ipv4Addr::new(192, 168, 66, 2))];
    assert_eq!(
        retune(&held, Some(ServerSource::Fallback), &held, lease),
        Retune::Select(select_servers(&[], &lease()))
    );
    assert_eq!(
        retune(&held, Some(ServerSource::Network), &held, other_lease),
        Retune::Keep,
        "a renewed lease naming another server keeps the rotation it has"
    );
}

#[test]
fn a_cadence_change_alone_applies_in_place() {
    let held = settings(true, &["a.test"], RefreshCadence::Daily);
    let next = settings(true, &["a.test"], RefreshCadence::SixHours);
    assert_eq!(
        retune(&held, Some(ServerSource::Configured), &next, no_lease),
        Retune::Refresh(RefreshCadence::SixHours.interval())
    );
    let off = settings(false, &["a.test"], RefreshCadence::SixHours);
    let still_off = settings(false, &["b.test"], RefreshCadence::Weekly);
    assert_eq!(retune(&off, None, &still_off, no_lease), Retune::Keep);
}

/// One drained batch over `changes`.
fn batch(status: tairix_abi::fs::DirWatchStatus, more: bool, names: &[&[u8]]) -> Vec<u8> {
    use tairix_abi::fs::{DirChange, DirChangeBatch};
    let mut out = vec![0u8; 4096];
    let count = u32::try_from(names.len()).expect("few");
    let mut at = DirChangeBatch::encode_header(&mut out, status, more, count).expect("fits");
    for name in names {
        at += DirChange::Absent(name)
            .encode_into(&mut out[at..])
            .expect("fits");
    }
    out.truncate(at);
    out
}

#[test]
fn the_watch_rereads_on_its_file_and_rewatches_on_its_directory() {
    use tairix_abi::fs::DirWatchStatus;
    let file = b"system.conf".as_slice();
    let dir = b"Configuration".as_slice();
    assert_eq!(
        watch_verdict(&batch(DirWatchStatus::Changes, true, &[file]), true, file),
        (WatchVerdict::Reread, true)
    );
    assert_eq!(
        watch_verdict(
            &batch(DirWatchStatus::Changes, false, &[b"network.conf"]),
            true,
            file
        ),
        (WatchVerdict::Unconcerned, false)
    );
    assert_eq!(
        watch_verdict(&batch(DirWatchStatus::Rescan, false, &[]), true, file),
        (WatchVerdict::Reread, false)
    );
    assert_eq!(
        watch_verdict(&batch(DirWatchStatus::Gone, false, &[]), true, file),
        (WatchVerdict::Rewatch, false)
    );
    // Watching the parent: the directory's own arrival means watch it.
    assert_eq!(
        watch_verdict(&batch(DirWatchStatus::Changes, false, &[dir]), false, dir),
        (WatchVerdict::Rewatch, false)
    );
    assert_eq!(
        watch_verdict(
            &batch(DirWatchStatus::Changes, false, &[b"Time"]),
            false,
            dir
        ),
        (WatchVerdict::Unconcerned, false)
    );
    assert_eq!(
        watch_verdict(&[0xFF; 8], true, file),
        (WatchVerdict::Rewatch, false)
    );
}
