//! Deterministic fuzz harness for `smbfs`'s trust boundary.
//!
//! A connection's worker parses whatever its server sent and may be
//! compromised by it; the front must stay whole whatever its workers say. The
//! invariants:
//!
//! 1. Both directions' codecs are canonical and total: no input panics either
//!    decoder, and a frame that decodes re-encodes to exactly its bytes.
//! 2. Against workers each of whose frames may be dropped, repeated,
//!    corrupted or replaced, and servers whose answers may be held back, the
//!    front never panics, answers every call — a client's, and the kernel's
//!    on a mounted volume — exactly once with a reply its decoder reads, and,
//!    once every login session has ended and every deadline passed, holds no
//!    socket and asks to be woken for nothing.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use std::cell::RefCell;
use std::rc::Rc;

use tairix_abi::driver::filesystem::{NodeId, NodeKind, NodeSecurity};
use tairix_abi::fs::SeekRegion;
use tairix_abi::fsserve::{decode_reply, Answer, Request, Span};
use tairix_abi::netattach::OfferId;
use tairix_abi::netshare_ipc::{NetshareReply, NetshareRequest};
use tairix_abi::time::Time64;
use tairix_fuzzseed::Prng;
use tairix_smb::crypt::NONCE_SEED_LEN;
use tairix_smb::negotiate::{Dialect, SALT_LEN};
use tairix_smb::peer::{Fault, PeerConfig};
use tairix_smb::status::NtStatus;
use tairix_smb::tree::ShareType;
use tairix_smb::SmbError;
use tairix_smbfs::front::{Caller, CALL_DEADLINE_NS, LINGER_NS, SETUP_DEADLINE_NS};
use tairix_smbfs::testing::{
    alice, connect, mount, nas, session, shares, Bench, Hand, World, BOB, PASSWORD,
};
use tairix_smbfs::wire::{
    Credential, FileOp, Leasing, Order, Report, Space, Stat, CHUNK_MAX, IO_MAX, KEY_LEN,
};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 64;

/// Largest noise input.
const MAX_NOISE: usize = 64;

const KEY: [u8; KEY_LEN] = [0x5a; KEY_LEN];

fn canonical_orders() -> Vec<Vec<u8>> {
    let credential = Credential {
        user: "alice",
        domain: "WORKGROUP",
        key: &KEY,
        client_challenge: [0xcc; 8],
        now: Time64::from_secs(1_790_000_000),
    };
    let orders = [
        Order::Open {
            floor: Dialect::Smb302,
            client_guid: [0x11; 16],
            salt: [0x22; SALT_LEN],
            server: "nas.example",
        },
        Order::Received(b"\x00\x00\x00\x04abcd"),
        Order::SignIn {
            credential,
            nonce_seed: [0x33; NONCE_SEED_LEN],
            previous: Some(7),
        },
        Order::Renew(credential),
        Order::List {
            request: 7,
            special: true,
        },
        Order::Connect {
            request: 8,
            share: "Media",
        },
        Order::Disconnect { tree: 9 },
        Order::Echo,
        Order::Stage {
            request: 10,
            chunk: b"staged",
        },
        Order::File {
            request: 11,
            tree: 12,
            op: FileOp::Write {
                path: "a\\b",
                offset: 4,
                len: 6,
            },
        },
        Order::File {
            request: 13,
            tree: 12,
            op: FileOp::Rename {
                from: "a",
                to: "b",
                replace: true,
                lease: Some([0x44; 16]),
            },
        },
        Order::File {
            request: 14,
            tree: 12,
            op: FileOp::Open {
                path: "",
                handle: 2,
            },
        },
    ];
    orders
        .iter()
        .map(|order| {
            let mut bytes = Vec::new();
            order.encode(&mut bytes).expect("encodes");
            bytes
        })
        .collect()
}

fn canonical_reports() -> Vec<Vec<u8>> {
    let stat = Stat {
        directory: false,
        read_only: true,
        size: 10,
        allocated: 4096,
        file_id: 77,
        created: Time64::from_secs(-1),
        modified: Time64::from_secs(1_790_000_000),
        accessed: Time64::from_secs(0),
        changed: Time64::from_secs(4_102_444_800),
    };
    let reports = [
        Report::Send(b"bytes"),
        Report::Negotiated {
            server_guid: *b"tairix test peer",
            dialect: Dialect::Smb311,
            leasing: Leasing::Files,
        },
        Report::Break {
            key: [0x55; 16],
            new: 1,
            ack: true,
        },
        Report::Established { session: 0x25 },
        Report::Rejected(NtStatus::LOGON_FAILURE),
        Report::Expired,
        Report::Renewed,
        Report::Share {
            request: 3,
            name: "C$",
            special: true,
            remark: "Default share",
        },
        Report::Listed {
            request: 3,
            hidden: 1,
            unnamed: 2,
            unlisted: 3,
        },
        Report::Connected {
            request: 4,
            tree: 5,
            kind: ShareType::Disk,
        },
        Report::Refused {
            request: 6,
            error: SmbError::Status(NtStatus::BAD_NETWORK_NAME),
        },
        Report::Failed(SmbError::Integrity),
        Report::Stat { request: 7, stat },
        Report::Data {
            request: 8,
            chunk: b"data",
        },
        Report::Entry {
            request: 9,
            name: "entry",
            stat,
        },
        Report::Page {
            request: 9,
            more: true,
        },
        Report::Done {
            request: 10,
            count: 6,
            file_id: 77,
        },
        Report::Space {
            request: 11,
            space: Space {
                unit: 4096,
                total: 100,
                free: 50,
                available: 25,
            },
        },
        Report::Held {
            request: 12,
            stat,
            lease: 0b111,
            file_id: [0x7b; 16],
            durable_ms: Some(60_000),
        },
    ];
    reports
        .iter()
        .map(|report| {
            let mut bytes = Vec::new();
            report.encode(&mut bytes).expect("encodes");
            bytes
        })
        .collect()
}

fn canonical_frames() -> Vec<Vec<u8>> {
    let mut frames = canonical_orders();
    frames.extend(canonical_reports());
    frames
}

/// Both decoders over `bytes`: neither panics, and what either accepts
/// re-encodes to exactly `bytes`.
fn decoded_frames_are_canonical(bytes: &[u8]) {
    if let Ok(order) = Order::decode(bytes) {
        let mut again = Vec::new();
        order.encode(&mut again).expect("a decoded order encodes");
        assert_eq!(again, bytes, "{order:?}");
    }
    if let Ok(report) = Report::decode(bytes) {
        let mut again = Vec::new();
        report.encode(&mut again).expect("a decoded report encodes");
        assert_eq!(again, bytes, "{report:?}");
    }
}

#[test]
fn the_frame_codecs_are_canonical_and_total() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_frame_codecs_are_canonical_and_total",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let frames = canonical_frames();
    let mut iterations = 0u64;
    loop {
        for frame in &frames {
            decoded_frames_are_canonical(frame);
            let mut flipped = frame.clone();
            let bit = rng.below(flipped.len() * 8);
            flipped[bit / 8] ^= 1 << (bit % 8);
            decoded_frames_are_canonical(&flipped);
            let cut = rng.below(frame.len());
            decoded_frames_are_canonical(&frame[..cut]);
        }
        let mut noise = vec![0; 1 + rng.below(MAX_NOISE)];
        rng.fill(&mut noise);
        noise[0] = u8::try_from(rng.below(20)).expect("small");
        decoded_frames_are_canonical(&noise);
        iterations += 1;
        if !(tairix_fuzzseed::within_budget(deadline)
            || (deadline.is_none() && iterations < SMOKE_ITERATIONS * 64))
        {
            break;
        }
    }
}

/// A worker's frame, as a chaotic channel delivers it.
fn perturbed(rng: &Rc<RefCell<Prng>>) -> Hand {
    let rng = Rc::clone(rng);
    Hand::Chaos(
        Box::default(),
        Box::new(move |frame, out| {
            let mut rng = rng.borrow_mut();
            match rng.below(20) {
                0..=13 => out.frame(frame),
                14 => Ok(()),
                15 => out.frame(frame).and_then(|()| out.frame(frame)),
                16 | 17 => {
                    let mut flipped = frame.to_vec();
                    let bit = rng.below(flipped.len() * 8);
                    flipped[bit / 8] ^= 1 << (bit % 8);
                    out.frame(&flipped)
                }
                _ => {
                    let mut noise = vec![0; 1 + rng.below(MAX_NOISE.min(CHUNK_MAX))];
                    rng.fill(&mut noise);
                    out.frame(&noise)
                }
            }
        }),
    )
}

/// The names a served call draws from: two that are one name in either case,
/// so the share's case rules are exercised too.
const NAMES: [&[u8]; 4] = [b"a", b"A", b"b", b"dir"];

/// A call the kernel could make on a volume, on what it may have named.
fn served_request(rng: &mut Prng) -> Request<'static> {
    let node = |rng: &mut Prng| NodeId::from_raw(1 + u64::try_from(rng.below(12)).expect("small"));
    let name = |rng: &mut Prng| NAMES[rng.below(NAMES.len())];
    let small = |rng: &mut Prng| u64::try_from(rng.below(64)).expect("small");
    let span = |rng: &mut Prng| {
        let size = [1, CHUNK_MAX, CHUNK_MAX + 1, IO_MAX][rng.below(4)];
        Span::new(rng.below(4) * IO_MAX, size).expect("in the window")
    };
    let kind = |rng: &mut Prng| {
        if rng.below(2) == 0 {
            NodeKind::RegularFile
        } else {
            NodeKind::Directory
        }
    };
    match rng.below(17) {
        15 => Request::Flush,
        16 => Request::ReleasePins,
        0 => Request::NodeInfo { node: node(rng) },
        1 => Request::Lookup {
            dir: node(rng),
            name: name(rng),
        },
        2 => Request::Parent { dir: node(rng) },
        3 => Request::ReadAt {
            file: node(rng),
            offset: small(rng),
            span: span(rng),
        },
        4 => Request::ReadDir {
            dir: node(rng),
            cursor: small(rng) % 4,
            after: b"",
        },
        5 => Request::Pin { node: node(rng) },
        6 => Request::Unpin { pinned: node(rng) },
        7 => {
            let kind = kind(rng);
            Request::Create {
                dir: node(rng),
                name: name(rng),
                kind,
                record: NodeSecurity::unrecorded(kind),
            }
        }
        8 => Request::WriteAt {
            file: node(rng),
            offset: small(rng),
            span: span(rng),
        },
        9 => Request::Truncate {
            file: node(rng),
            size: small(rng),
        },
        10 => Request::Remove {
            dir: node(rng),
            name: name(rng),
        },
        11 => Request::Rename {
            src_dir: node(rng),
            src_name: name(rng),
            dst_dir: node(rng),
            dst_name: name(rng),
        },
        12 => Request::Release { span: span(rng) },
        13 => Request::Stats,
        _ => Request::SeekRegion {
            file: node(rng),
            offset: small(rng),
            region: SeekRegion::Hole,
        },
    }
}

/// Every served call answered at most once, exactly once unless the kernel
/// never handed it over, with a reply the kernel's decoder reads.
fn served_calls_answered(bench: &mut Bench, served: &[(OfferId, u64, Request<'static>)]) {
    for (offer, ticket, request) in served {
        let volume = bench.world().volume(*offer).expect("offered");
        let taken = !volume.waiting.iter().any(|(waiting, _)| waiting == ticket);
        let answers: Vec<&Vec<u8>> = volume
            .answers
            .iter()
            .filter(|(answered, _)| answered == ticket)
            .map(|(_, reply)| reply)
            .collect();
        assert_eq!(
            answers.len(),
            usize::from(taken),
            "call {ticket}: {request:?}"
        );
        for reply in answers {
            assert!(
                decode_reply(reply, request).is_ok(),
                "call {ticket}: {request:?} answered {reply:?}"
            );
        }
    }
}

/// Rounds that once failed, replayed first every run: each left a call of
/// a client's unanswered, a worker having reported for a call one honest
/// worker never reports for.
const REGRESSIONS: [u64; 8] = [
    7_709_534_336_231_760_337,
    5_008_199_298_281_875_682,
    12_297_210_883_875_553_119,
    16_411_319_856_701_676_613,
    18_039_479_377_900_873_074,
    8_012_142_995_814_365_305,
    10_227_073_867_269_801_183,
    17_331_598_967_299_530_928,
];

/// Names the round under way if it fails: `one_round` of that seed replays it
/// alone.
struct Replay(u64);

impl Drop for Replay {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("[smbfs fuzz] the failing round's seed: {}", self.0);
        }
    }
}

/// One round of calls against chaotic workers, drawn from `seed` alone so a
/// failing round replays by itself.
fn one_round(seed: u64) {
    let rng = &Rc::new(RefCell::new(Prng::new(seed)));
    // Taken from the seed, not drawn, so a seed replays the stream it did;
    // the short-writing server takes bits no regression seed has.
    let fault = if (seed >> 3) & 3 == 1 {
        Fault::ShortWrite
    } else {
        Fault::None
    };
    let mut world = World::serving(PeerConfig {
        directory_leasing: seed & 1 == 1,
        fault,
        ..PeerConfig::new(Dialect::Smb311)
    });
    for _ in 0..4 {
        world.hands.push_back(perturbed(rng));
    }
    let mut bench = Bench::new(world);
    let callers = [
        alice(),
        Caller {
            uid: BOB,
            session: Some(session(2)),
        },
        Caller {
            uid: BOB,
            session: None,
        },
    ];
    let mut tickets = Vec::new();
    let mut served = Vec::new();
    let calls = 1 + rng.borrow_mut().below(12);
    for _ in 0..calls {
        let (pick, which) = {
            let mut rng = rng.borrow_mut();
            (rng.below(callers.len()), rng.below(8))
        };
        if which >= 5 {
            let live = bench
                .world()
                .volumes
                .iter()
                .filter(|volume| !volume.gone)
                .map(|volume| volume.offer)
                .collect::<Vec<_>>();
            let held = rng.borrow_mut().below(4) == 0;
            bench.world().hold(held);
            let at = rng.borrow_mut().below(live.len().max(1));
            if let Some(&offer) = live.get(at) {
                let request = served_request(&mut rng.borrow_mut());
                let ticket = bench.serve(offer, &request);
                served.push((offer, ticket, request));
            }
            continue;
        }
        let caller = callers[pick];
        let offers: Vec<OfferId> = bench.world().offers.iter().map(|offer| offer.0).collect();
        let ticket = match which {
            0 => bench.call(caller, &shares(connect(PASSWORD))),
            1 => bench.call(caller, &mount(connect(PASSWORD), "share")),
            2 => bench.call(caller, &mount(connect(PASSWORD), "nosuch")),
            3 => bench.call(caller, &NetshareRequest::Mounts),
            _ => {
                let offer = offers
                    .first()
                    .copied()
                    .or(OfferId::new(1))
                    .expect("not zero");
                bench.call(caller, &NetshareRequest::Unmount { offer })
            }
        };
        tickets.push(ticket);
        if rng.borrow_mut().below(3) == 0 {
            bench.pass(1 + u64::try_from(rng.borrow_mut().below(1 << 20)).expect("small"));
        }
    }
    bench.world().hold(false);
    bench.run();
    bench.pass(SETUP_DEADLINE_NS + CALL_DEADLINE_NS);
    served_calls_answered(&mut bench, &served);
    for anchor in [session(1), session(2)] {
        bench.front.peer_exited(bench.now, anchor);
    }
    bench.run();
    bench.pass(LINGER_NS + 1);
    for ticket in &tickets {
        let answers = bench
            .world()
            .replies
            .iter()
            .filter(|(answered, _)| answered == ticket)
            .count();
        assert_eq!(answers, 1, "ticket {ticket} answered {answers} times");
    }
    for (ticket, reply) in &bench.world().replies {
        assert!(
            NetshareReply::decode(reply).is_ok(),
            "ticket {ticket}: {reply:?}"
        );
    }
    served_calls_answered(&mut bench, &served);
    assert_eq!(bench.world().live(), 0, "a connection outlived everything");
    assert_eq!(bench.front.next_deadline(), None);
}

/// The floor, by construction: with every worker honest, a listing and a
/// mount both succeed, so the sweep's changes land on a front that would
/// otherwise reach the deep states.
fn honest_prologue() {
    let mut bench = Bench::new(nas());
    let listed = bench.call(alice(), &shares(connect(PASSWORD)));
    let reply = bench.reply(listed);
    assert!(matches!(
        NetshareReply::decode(&reply),
        Ok(NetshareReply::Shares(_))
    ));
    let mounted = bench.call(alice(), &mount(connect(PASSWORD), "share"));
    let (_, offer) = bench.mounted(mounted);
    // A served round trip, so the volume's own paths are reached too.
    let made = Request::Create {
        dir: NodeId::from_raw(1),
        name: b"a",
        kind: NodeKind::RegularFile,
        record: NodeSecurity::unrecorded(NodeKind::RegularFile),
    };
    let ticket = bench.serve(offer, &made);
    let reply = bench.answer(offer, ticket);
    let Ok(Answer::Node(file)) = decode_reply(&reply, &made).expect("read").result else {
        panic!("not made");
    };
    let span = Span::new(0, 4).expect("in the window");
    for request in [
        Request::WriteAt {
            file,
            offset: 0,
            span,
        },
        Request::ReadAt {
            file,
            offset: 0,
            span,
        },
    ] {
        let ticket = bench.serve(offer, &request);
        let reply = bench.answer(offer, ticket);
        assert_eq!(
            decode_reply(&reply, &request).expect("read").result,
            Ok(Answer::Count(4)),
            "{request:?}"
        );
    }
}

#[test]
fn the_front_stays_whole_whatever_its_workers_say() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_front_stays_whole_whatever_its_workers_say",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    honest_prologue();
    for seed in REGRESSIONS {
        let _replay = Replay(seed);
        one_round(seed);
    }
    let mut iterations = 0u64;
    loop {
        let seed = rng.next_u64();
        let _replay = Replay(seed);
        one_round(seed);
        iterations += 1;
        if !(tairix_fuzzseed::within_budget(deadline)
            || (deadline.is_none() && iterations < SMOKE_ITERATIONS))
        {
            break;
        }
    }
}
