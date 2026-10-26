extern crate std;

use alloc::sync::Arc;
use core::ptr::NonNull;
use std::sync::{Arc as StdArc, Mutex};
use std::thread;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrsProvider, FilesystemRead, FilesystemStats, FilesystemWrite, NodeKind,
    Unpinned, VolumeHost,
};
use tairix_abi::fsserve::{
    decode_request, encode_request, Op, ServeOffer, FSSERVE_REPLY_MAX, FSSERVE_REQUEST_MAX,
    FSSERVE_WINDOW_LEN,
};
use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::time::Time64;
use tairix_abi::{DriverError, DriverHandle};
use tairix_caps::CapabilitySet;
use tairix_fsserve::{Clock, ServedHost, Server};
use tairix_kernel_ipc::{CallEndpoint, CallEndpointLimits, EndpointId, RecvCall};
use tairix_kernel_sec::{ProcessId, TaskCapabilities, UserId};
use tairix_log::Sink;

use super::{shows_life, HoldCall, Liveness, ServedFs, ServerActivity, Unsettled};
use crate::fs::memfs::{RwMockFs, ADMIN_RECORD};
use crate::fs::wrapper_conformance::{
    assert_attrs_forwards, assert_read_forwards, assert_security_forwards, assert_stats_forwards,
    assert_write_forwards_keeping_its_timer, fixture,
};
use crate::sharedreg::KernelHold;

struct NullSink;

impl Sink for NullSink {
    fn write_event(&self, _event: &tairix_log::Event<'_>) {}
}

static SINK: NullSink = NullSink;

struct TestClock;

impl Clock for TestClock {
    fn now_ns(&self) -> Option<u64> {
        None
    }

    fn wall_time(&self) -> Option<Time64> {
        None
    }
}

fn fresh_endpoint_id() -> u64 {
    static NEXT: core::sync::atomic::AtomicU64 =
        core::sync::atomic::AtomicU64::new(0xF55E_7E57_0000_0000);
    NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

fn caps(process: u64) -> TaskCapabilities {
    TaskCapabilities::derive(
        ProcessId(process),
        UserId(0),
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        &SINK,
    )
}

fn endpoint() -> Arc<CallEndpoint> {
    let endpoint = Arc::new(
        CallEndpoint::create(
            EndpointId(fresh_endpoint_id()),
            &caps(0x7E57_0002),
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: u32::try_from(FSSERVE_REQUEST_MAX).unwrap(),
                max_reply: u32::try_from(FSSERVE_REPLY_MAX).unwrap(),
                capacity: 4,
            },
            tairix_abi::RLIMIT_INFINITY,
            &SINK,
        )
        .expect("endpoint"),
    );
    crate::callreg::register(Arc::clone(&endpoint), &SINK).expect("register");
    endpoint
}

/// The window a test leaks for the proxy and its server to share, each
/// touching it only while the protocol gives it the turn.
#[derive(Clone, Copy)]
struct Window(NonNull<u8>);

// SAFETY: leaked test memory alive for the process; the request/reply
// protocol alternates access to it.
unsafe impl Send for Window {}

impl Window {
    fn leak() -> Self {
        Self(
            NonNull::new(
                crate::forever(vec![0u8; FSSERVE_WINDOW_LEN].into_boxed_slice()).as_mut_ptr(),
            )
            .expect("a window"),
        )
    }

    fn hold(self) -> KernelHold {
        KernelHold::for_test(self.0, FSSERVE_WINDOW_LEN)
    }

    /// The server's view, while it answers a call.
    fn bytes(&self) -> &'static mut [u8] {
        // SAFETY: the leaked window is `FSSERVE_WINDOW_LEN` bytes alive for
        // the process, and the proxy leaves it alone while a call is in
        // service, so this borrow is the only one in use.
        unsafe { core::slice::from_raw_parts_mut(self.0.as_ptr(), FSSERVE_WINDOW_LEN) }
    }
}

/// How the test server answers: honestly through `lib/fsserve`, or with a
/// reply a tamper rewrites.
type Tamper = std::boxed::Box<dyn FnMut(Op, &mut [u8], &mut usize) + Send>;

/// A served volume under test: the proxy, its server thread, and what the
/// server saw.
struct Served {
    fs: ServedFs,
    endpoint: Arc<CallEndpoint>,
    activity: Arc<ServerActivity>,
    seen: StdArc<Mutex<Vec<Op>>>,
    server: Option<thread::JoinHandle<()>>,
    /// The driver and window a server not yet started will serve.
    waiting: Option<(RwMockFs, Window)>,
}

impl Served {
    fn new(inner: RwMockFs, tamper: Option<Tamper>) -> Self {
        Self::build(inner, tamper, FSSERVE_REPLY_MAX)
    }

    fn honest(inner: RwMockFs) -> Self {
        Self::new(inner, None)
    }

    /// An honest server whose replies hold at most `reply_len` bytes, so a
    /// listing spans batches without the protocol's whole bound of entries.
    fn replying_within(inner: RwMockFs, reply_len: usize) -> Self {
        Self::build(inner, None, reply_len)
    }

    fn build(inner: RwMockFs, tamper: Option<Tamper>, reply_len: usize) -> Self {
        let mut served = Self::stopped(inner);
        served.start(tamper, reply_len);
        served
    }

    /// A volume whose server has not started, so nothing answers its calls.
    fn stopped(mut inner: RwMockFs) -> Self {
        let endpoint = endpoint();
        let window = Window::leak();
        let offer = ServeOffer {
            root: inner.root(),
            name_matching: inner.name_matching(),
            node_identity: inner.node_identity(),
            attrs: inner.attrs_fs().is_some(),
            write_refusal: None,
            format: tairix_abi::volume::FormatName::Ext4,
        };
        let activity = Arc::new(ServerActivity::default());
        let fs = ServedFs::new(
            Arc::clone(&endpoint),
            window.hold(),
            offer,
            Arc::clone(&activity),
            &SINK,
        )
        .expect("a proxy");
        Self {
            fs,
            endpoint,
            activity,
            seen: StdArc::new(Mutex::new(Vec::new())),
            server: None,
            waiting: Some((inner, window)),
        }
    }

    /// Start the server, each reply rewritten by `tamper` and held to
    /// `reply_len` bytes.
    fn start(&mut self, tamper: Option<Tamper>, reply_len: usize) {
        let (inner, window) = self.waiting.take().expect("a server not yet started");
        let endpoint = Arc::clone(&self.endpoint);
        let seen = StdArc::clone(&self.seen);
        self.server = Some(thread::spawn(move || {
            serve(&endpoint, window, inner, tamper, &seen, reply_len);
        }));
    }

    fn ops(&self) -> Vec<Op> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.endpoint.destroy(&SINK);
        if let Some(server) = self.server.take() {
            server.join().expect("the server thread ends");
        }
    }
}

fn serve(
    endpoint: &CallEndpoint,
    window: Window,
    inner: RwMockFs,
    mut tamper: Option<Tamper>,
    seen: &Mutex<Vec<Op>>,
    reply_len: usize,
) {
    let host = ServedHost::new(TestClock);
    let mut server = Server::new(inner, &host).expect("a server");
    let mut out = vec![0u8; reply_len];
    while !endpoint.is_closed() {
        let RecvCall::Received(call) = endpoint.recv_call(usize::MAX) else {
            thread::yield_now();
            continue;
        };
        let op = decode_request(&call.request).map(|(_, r)| r.op());
        if let Ok(op) = op {
            seen.lock().unwrap().push(op);
        }
        let mut len = server.answer(&call.request, window.bytes(), &mut out);
        if let (Some(tamper), Ok(op)) = (tamper.as_mut(), op) {
            tamper(op, &mut out, &mut len);
        }
        let _ = endpoint.reply(call.ticket, &out[..len], &SINK);
    }
}

#[test]
fn the_proxy_forwards_the_whole_driver_surface() {
    let mut served = Served::honest(fixture());
    assert_read_forwards(&mut served.fs);
    assert_write_forwards_keeping_its_timer(&mut served.fs);
    assert_security_forwards(&mut served.fs);
    assert_stats_forwards(&mut served.fs);
    assert_attrs_forwards(&mut served.fs);
}

#[test]
fn file_data_larger_than_the_window_crosses_in_several_calls() {
    let mut served = Served::honest(RwMockFs::new());
    let root = served.fs.root();
    let file = served
        .fs
        .create(root, b"big", NodeKind::RegularFile, &ADMIN_RECORD)
        .unwrap();
    let len = FSSERVE_WINDOW_LEN * 2 + 77;
    // A prime period, so a chunk landing at the wrong offset reads wrong;
    // repeated by doubling copies, as a byte at a time it is minutes
    // interpreted.
    let mut body = (0..251u8).collect::<Vec<u8>>().repeat(len / 251 + 1);
    body.truncate(len);
    assert_eq!(served.fs.write_at(file, 0, &body), Ok(body.len()));
    let mut back = vec![0u8; body.len() + 10];
    assert_eq!(served.fs.read_at(file, 0, &mut back), Ok(body.len()));
    assert_eq!(&back[..body.len()], &body[..]);
    let writes = served.ops().iter().filter(|op| **op == Op::WriteAt).count();
    assert_eq!(writes, 3, "a window's worth per call");
    // Reading from past the end is end of file at once.
    assert_eq!(
        served
            .fs
            .read_at(file, body.len() as u64 + 1, &mut back[..16]),
        Ok(0)
    );
}

#[test]
fn a_listing_spanning_batches_is_handed_over_once_and_resumes_where_it_stopped() {
    let mut inner = RwMockFs::new();
    let root = inner.root();
    let mut names: Vec<Vec<u8>> = (0..40)
        .map(|n| std::format!("listed-entry-{n:03}").into_bytes())
        .collect();
    for name in &names {
        inner
            .create(root, name, NodeKind::RegularFile, &ADMIN_RECORD)
            .unwrap();
    }
    names.sort();
    // A reply a fraction of the protocol's bound spans batches with a few
    // dozen entries rather than the bound's worth.
    let mut served = Served::replying_within(inner, 2048);
    let mut seen = Vec::new();
    served
        .fs
        .read_dir(root, 0, &[], &mut |_, name| {
            seen.push(name.to_vec());
            DirVisit::Take
        })
        .unwrap();
    seen.sort();
    assert_eq!(seen, names);
    let batches = served.ops().iter().filter(|op| **op == Op::ReadDir).count();
    assert!(batches >= 3, "the listing spans several replies");
    // Equal entries pack every reply but the last alike, so fewer replies
    // than entries puts two or more in the first: a stop after one is
    // mid-batch.
    assert!(batches < names.len(), "a reply holds several entries");
    let mut first = Vec::new();
    let mut at = 0;
    let mut after = Vec::new();
    served
        .fs
        .read_dir(root, 0, &[], &mut |entry, name| {
            if !first.is_empty() {
                return DirVisit::Stop;
            }
            first.push(name.to_vec());
            at = entry.next_cursor;
            after = name.to_vec();
            DirVisit::Take
        })
        .unwrap();
    let mut rest = Vec::new();
    served
        .fs
        .read_dir(root, at, &after, &mut |_, name| {
            rest.push(name.to_vec());
            DirVisit::Take
        })
        .unwrap();
    first.extend(rest);
    first.sort();
    assert_eq!(first, names, "each entry exactly once across the stop");
}

#[test]
fn a_malformed_reply_fails_the_call_and_degrades_the_volume_until_a_clean_one() {
    fn garble_stats(op: Op, out: &mut [u8], len: &mut usize) {
        if op == Op::Stats {
            out[*len - 1] ^= 0xFF;
            *len -= 3;
        }
    }
    let mut served = Served::new(RwMockFs::new(), Some(std::boxed::Box::new(garble_stats)));
    let state = served.fs.state();
    assert_eq!(served.fs.stats(), Err(DriverError::DeviceFault));
    assert_eq!(state.availability(), MountAvailability::Degraded);
    let root = served.fs.root();
    assert!(served.fs.node_info(root).is_ok());
    assert_eq!(state.availability(), MountAvailability::Available);
}

#[test]
fn a_listing_that_never_advances_is_refused() {
    // The first batch is answered as continuing, and every later one is a
    // replay of it: resuming after its entry yields that entry again.
    let mut first: Option<Vec<u8>> = None;
    let stuck = move |op: Op, out: &mut [u8], len: &mut usize| {
        if op != Op::ReadDir {
            return;
        }
        match &first {
            None => {
                let trailer = *len - 5;
                out[trailer] = 0;
                first = Some(out[..*len].to_vec());
            }
            Some(replay) => {
                out[..replay.len()].copy_from_slice(replay);
                *len = replay.len();
            }
        }
    };
    let mut inner = RwMockFs::new();
    let root = inner.root();
    inner
        .create(root, b"only", NodeKind::RegularFile, &ADMIN_RECORD)
        .unwrap();
    let mut served = Served::new(inner, Some(std::boxed::Box::new(stuck)));
    let mut taken = 0;
    let listed = served.fs.read_dir(root, 0, &[], &mut |_, _| {
        taken += 1;
        DirVisit::Take
    });
    // The first batch hands over its entry and asks for more from after it;
    // the second resumes from that same entry, which is no progress.
    assert_eq!(listed, Err(DriverError::DeviceFault));
    assert_eq!(taken, 2);
}

#[test]
fn a_quarantined_window_is_released_before_file_data_moves_again() {
    let mut served = Served::honest(RwMockFs::new());
    let root = served.fs.root();
    let file = served
        .fs
        .create(root, b"f", NodeKind::RegularFile, &ADMIN_RECORD)
        .unwrap();
    served.fs.write_at(file, 0, b"data").unwrap();
    served.fs.quarantined = true;
    // A call that moves no data needs no release.
    served.fs.node_info(file).unwrap();
    assert!(served.fs.quarantined);
    let mut back = [0u8; 4];
    assert_eq!(served.fs.read_at(file, 0, &mut back), Ok(4));
    assert!(!served.fs.quarantined);
    let ops = served.ops();
    let tail = &ops[ops.len() - 3..];
    assert_eq!(tail, &[Op::NodeInfo, Op::Release, Op::ReadAt]);
    served.fs.quarantined = true;
    served.fs.write_at(file, 0, b"atad").unwrap();
    let ops = served.ops();
    assert_eq!(&ops[ops.len() - 2..], &[Op::Release, Op::WriteAt]);
}

#[test]
fn a_driver_whose_endpoint_is_gone_reads_as_an_offline_device() {
    let mut served = Served::honest(RwMockFs::new());
    served.endpoint.destroy(&SINK);
    let root = served.fs.root();
    assert_eq!(served.fs.node_info(root), Err(DriverError::DeviceOffline));
}

#[test]
fn the_drivers_reported_state_reaches_the_kernel() {
    struct Reserve(Mutex<Vec<bool>>);
    impl VolumeHost for Reserve {
        fn now_ns(&self) -> Option<u64> {
            None
        }
        fn wall_time(&self) -> Option<Time64> {
            None
        }
        fn writeback_due(&self, _: DriverHandle, _: Option<u64>) {
            panic!("a served volume never reports a commit deadline to the kernel");
        }
        fn reserve_crossed(&self, _: DriverHandle, in_reserve: bool) {
            self.0.lock().unwrap().push(in_reserve);
        }
    }
    fn reserve_and_unpublished(_: Op, out: &mut [u8], _: &mut usize) {
        out[3] = 0b11;
    }
    let host: &'static Reserve =
        crate::forever(std::boxed::Box::new(Reserve(Mutex::new(Vec::new()))));
    let mut served = Served::new(
        RwMockFs::new(),
        Some(std::boxed::Box::new(reserve_and_unpublished)),
    );
    served
        .fs
        .set_volume_host(DriverHandle::from_raw(5).unwrap(), host);
    let state = served.fs.state();
    assert!(!state.holds_unpublished());
    served.fs.flush().unwrap();
    assert!(state.holds_unpublished());
    served.fs.flush().unwrap();
    assert_eq!(
        *host.0.lock().unwrap(),
        vec![true],
        "told once, on the edge"
    );
}

#[test]
fn a_driver_shows_life_inside_a_transfer_or_by_finishing_one() {
    assert!(shows_life(Some(3), None), "inside a transfer");
    assert!(shows_life(None, Some(3)), "finished the one it was in");
    assert!(shows_life(Some(3), Some(4)), "finished one in between");
    assert!(!shows_life(Some(3), Some(3)), "nothing since");
}

#[test]
fn activity_counts_a_transfer_once_it_ends() {
    let activity = ServerActivity::default();
    let before = activity.mark();
    let guard = activity.enter();
    assert_eq!(activity.mark(), None);
    drop(guard);
    assert!(shows_life(before, activity.mark()));
}

/// Post `call` as the proxy does and leave it unsettled under `liveness`, as
/// a kill or a stall of its caller would.
fn abandon(fs: &mut ServedFs, call: HoldCall, liveness: Liveness) {
    let len = encode_request(&call.request(), fs.class, &mut fs.frame).expect("encodes");
    let ticket = fs.post(len, None).expect("posts");
    fs.unsettled = Some(Unsettled::Asked {
        ticket,
        call,
        liveness,
    });
}

/// A pin abandoned unanswered is waited out before the next call, and the
/// hold it was granted, which no descriptor owns, is let go: the node's count
/// is the kernel's own again.
#[test]
fn an_abandoned_pin_is_waited_out_and_a_hold_it_was_granted_let_go() {
    let mut served = Served::honest(RwMockFs::new());
    let root = served.fs.root();
    let file = served
        .fs
        .create(root, b"f", NodeKind::RegularFile, &ADMIN_RECORD)
        .unwrap();
    abandon(&mut served.fs, HoldCall::Pin(file), Liveness::from(None, 0));
    served.fs.node_info(file).unwrap();
    let ops = served.ops();
    assert_eq!(&ops[ops.len() - 3..], &[Op::Pin, Op::Unpin, Op::NodeInfo]);
    assert!(served.fs.unsettled.is_none());
    let held = served.fs.pin(file).unwrap();
    assert_eq!(
        served.fs.unpin(held),
        Ok(Unpinned::Released),
        "one hold, not two"
    );
}

/// A release abandoned unanswered is waited out too, and owes nothing.
#[test]
fn an_abandoned_release_is_waited_out_and_owes_nothing() {
    let mut served = Served::honest(RwMockFs::new());
    let root = served.fs.root();
    let file = served
        .fs
        .create(root, b"f", NodeKind::RegularFile, &ADMIN_RECORD)
        .unwrap();
    let held = served.fs.pin(file).unwrap();
    abandon(
        &mut served.fs,
        HoldCall::Unpin(held),
        Liveness::from(None, 0),
    );
    served.fs.node_info(file).unwrap();
    let ops = served.ops();
    assert_eq!(&ops[ops.len() - 2..], &[Op::Unpin, Op::NodeInfo]);
    assert_eq!(
        served.fs.unpin(held),
        Err(DriverError::NotFound),
        "the release landed"
    );
}

/// A kill abandons the wait on a call on the driver's holds as on any other,
/// without withdrawing it: the call after settles it, letting go of the pin
/// the driver granted once its caller was gone.
#[test]
fn a_kill_abandons_a_hold_call_that_the_next_call_settles() {
    let _kills = crate::procsignal::running_kill_test_lock();
    let (_, task) = crate::test_boot::claim_scheduler();
    let mut served = Served::stopped(RwMockFs::new());
    let root = served.fs.root();
    crate::procsignal::clear_kill_gate(task);
    crate::procsignal::install_gate(task, ProcessId(task)).expect("a gate installs");
    let death = crate::procsignal::DeferredTeardown::Exit {
        process: ProcessId(task),
        status: 137,
    };
    assert_eq!(
        crate::procsignal::claim_group_kill(None, death, None).len(),
        1
    );
    assert_eq!(served.fs.pin(root), Err(DriverError::Interrupted));
    assert!(matches!(
        served.fs.unsettled,
        Some(Unsettled::Asked {
            call: HoldCall::Pin(_),
            ..
        })
    ));
    crate::procsignal::clear_kill_gate(task);
    served.start(None, FSSERVE_REPLY_MAX);
    served.fs.node_info(root).unwrap();
    assert_eq!(served.ops(), [Op::Pin, Op::Unpin, Op::NodeInfo]);
    let held = served.fs.pin(root).unwrap();
    assert_eq!(
        served.fs.unpin(held),
        Ok(Unpinned::Released),
        "one hold, not two"
    );
}

/// A hold call whose driver is found stopped fails every later call at once,
/// unposted, recording the stall once, until the driver shows life and
/// answers it.
#[test]
fn a_stalled_hold_call_fails_later_calls_at_once_until_it_is_answered() {
    let (_, _) = crate::test_boot::claim_scheduler();
    let mut served = Served::stopped(RwMockFs::new());
    let root = served.fs.root();
    let stopped = Liveness::from(served.activity.mark(), 0);
    let judged = Liveness {
        judged_at: 0,
        ..stopped
    };
    abandon(&mut served.fs, HoldCall::Pin(root), judged);
    for _ in 0..2 {
        assert_eq!(served.fs.node_info(root), Err(DriverError::TimedOut));
    }
    assert!(matches!(
        served.fs.unsettled,
        Some(Unsettled::Asked { liveness, .. }) if liveness.recorded
    ));
    assert_eq!(
        served.fs.state().availability(),
        MountAvailability::Degraded
    );
    assert_eq!(
        served.endpoint.outstanding(),
        1,
        "nothing after the pin was posted"
    );
    served.start(None, FSSERVE_REPLY_MAX);
    drop(served.activity.enter());
    served.fs.node_info(root).unwrap();
    assert_eq!(served.ops(), [Op::Pin, Op::Unpin, Op::NodeInfo]);
}

/// A release that never reached the driver answers busy, so its registration
/// keeps it, and a pin owed back stays owed until it can be sent.
#[test]
fn what_never_reached_the_driver_is_kept_to_be_sent() {
    let mut served = Served::honest(RwMockFs::new());
    let root = served.fs.root();
    let file = served
        .fs
        .create(root, b"f", NodeKind::RegularFile, &ADMIN_RECORD)
        .unwrap();
    let held = served.fs.pin(file).unwrap();
    served.endpoint.destroy(&SINK);
    assert_eq!(served.fs.unpin(held), Err(DriverError::Busy));
    served.fs.unsettled = Some(Unsettled::Owed(held));
    assert_eq!(served.fs.node_info(file), Err(DriverError::DeviceOffline));
    assert!(matches!(served.fs.unsettled, Some(Unsettled::Owed(owed)) if owed == held));
}
