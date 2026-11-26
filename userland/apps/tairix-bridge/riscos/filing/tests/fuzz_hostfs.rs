//! Deterministic fuzz harness for HostFS's relay: the server answering
//! hostile requests from a session worker it cannot trust, the codec on both
//! sides, and a session whose replies arrive damaged.
//!
//! The invariants:
//!
//! * nothing panics, and every refusal is a value;
//! * every request that decodes is answered with a reply that answers it, and
//!   one that does not is refused;
//! * whatever decodes re-encodes to the very bytes it came from;
//! * nothing beside the roots changes, nor anything in a root the session may
//!   not write, nor a delivered file not delivered writable;
//! * a session given damaged replies still answers.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use tairix_fscopy::fixture::{Handle, Tree};
use tairix_fscopy::tree::Roots;
use tairix_fuzzseed::Prng;
use tairix_riscos_filing::hostfs::protocol::{Refusal, Reply, Request};
use tairix_riscos_filing::hostfs::server::{Given, Server};
use tairix_riscos_filing::session::{application_slot, SLOT_BYTES};
use tairix_riscos_kernel::corpus::StillHost;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Ending, Host, Os, Pulse, Regs};
use tairix_riscos_kernel::swi::os::{FILE, FIND, FS_CONTROL, GBPB};
use tairix_riscos_kernel::swi::X;

/// Runs a plain `cargo test` makes of each sweep.
const SMOKE_RUNS: u64 = 40;
/// Requests in one run.
const STEPS: usize = 120;

/// The leaves hostile paths are built from: names there, names not, names
/// RISC OS cannot hold, and the parent and root specials.
const LEAVES: [&[u8]; 12] = [
    b"File", b"FILE", b"Dir", b"Inner", b"Lock", b"1", b"Doc", b"x", b"^", b"$", b"a b", b"",
];

/// The tree a run serves: a writable root, a read-only one, and what lies
/// beside them.
fn tree() -> Tree {
    Tree::new()
        .file("/rw/File", b"read and write")
        .file("/rw/Dir/Inner", b"inner")
        .file("/rw/Lock", b"locked")
        .attr("/rw/Lock", "acorn.attr", b"LWR/")
        .file("/ro/File", b"read only")
        .file("/outside/Secret", b"never reached")
        .file("/Doc", b"delivered")
}

fn server(tree: &Tree) -> Server<Handle> {
    let root = |path, name: &str, writable| Given {
        dir: tree.open_dir(path, false).expect("a root"),
        name: name.to_string(),
        writable,
    };
    let mut server = Server::new(
        Some(root("/rw", "RW", true)),
        vec![root("/ro", "RO", false)],
        [5; 16],
        3,
    );
    let doc = tree
        .open_file("/Doc", false, false)
        .expect("the delivered file");
    server.deliver("Doc", doc, false).expect("delivered");
    server
}

/// A path of a few hostile leaves.
fn path(rng: &mut Prng) -> Vec<u8> {
    let mut path = Vec::new();
    for n in 0..rng.below(4) {
        if n > 0 {
            path.push(b'.');
        }
        path.extend_from_slice(rng.pick(&LEAVES));
    }
    if rng.below(8) == 0 {
        let length = rng.below(24);
        path = rng.noise(length);
    }
    path
}

/// A word, most often one near the edges a field has.
fn word(rng: &mut Prng, handles: &[u32]) -> u32 {
    match rng.below(6) {
        0 => 0,
        1 => u32::MAX,
        2 if !handles.is_empty() => *rng.pick(handles),
        3 => rng.next_u32() & 0x1FFF,
        _ => rng.next_u32(),
    }
}

/// A random request's bytes, well formed or not.
fn request(rng: &mut Prng, handles: &[u32]) -> Vec<u8> {
    let (root, path, other) = (
        u32::try_from(rng.below(4)).unwrap_or(0),
        path(rng),
        path(rng),
    );
    let length = rng.below(64);
    let data = rng.noise(length);
    let (a, b, c) = (word(rng, handles), word(rng, handles), word(rng, handles));
    let request = match rng.below(15) {
        0 => Request::Roots,
        1 => Request::Read { root, path: &path },
        2 => Request::List {
            root,
            path: &path,
            offset: a & 0xFF,
            count: b & 0x7F,
        },
        3 => Request::Open {
            root,
            path: &path,
            update: a & 1 != 0,
        },
        4 => Request::ReadBytes {
            handle: a,
            offset: b & 0xFFFF,
            length: c & 0x1_FFFF,
        },
        5 => Request::WriteBytes {
            handle: a,
            offset: b & 0xFFFF,
            bytes: &data,
        },
        6 => Request::SetExtent {
            handle: a,
            extent: b & 0xFFFF,
        },
        7 => Request::Sync { handle: a },
        8 => Request::Close {
            handle: a,
            stamp: (b & 1 != 0).then_some((c, b)),
        },
        9 => Request::Create {
            root,
            path: &path,
            load: a,
            exec: b,
            length: c & 0xFFFF,
        },
        10 => Request::CreateDir {
            root,
            path: &path,
            load: a,
            exec: b,
        },
        11 => Request::WriteInfo {
            root,
            path: &path,
            load: a,
            exec: b,
            attributes: c.to_le_bytes()[0],
        },
        12 => Request::Delete { root, path: &path },
        13 => Request::Rename {
            root,
            from: &path,
            to: &other,
        },
        _ => Request::Space { root },
    };
    let mut bytes = Vec::new();
    request.encode(&mut bytes);
    if rng.below(6) == 0 {
        bytes = rng.mutated(&bytes);
    }
    bytes
}

/// What must never change: every path outside the writable root, with its
/// bytes.
fn untouchable(tree: &Tree) -> Vec<(String, Option<Vec<u8>>)> {
    tree.paths()
        .into_iter()
        .filter(|path| !path.starts_with("/rw"))
        .map(|path| {
            let bytes = tree.bytes(&path);
            (path, bytes)
        })
        .collect()
}

#[test]
fn any_request_is_answered_within_the_roots_the_session_may_reach() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_request_is_answered_within_the_roots_the_session_may_reach",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let tree = tree();
        let before = untouchable(&tree);
        let mut server = server(&tree);
        let mut handles = Vec::new();
        let mut failed = Vec::new();
        Reply::Refused(Refusal::Failed).encode(&mut failed);
        for _ in 0..STEPS {
            let bytes = request(&mut rng, &handles);
            let mut out = Vec::new();
            server.serve(&bytes, &mut out);
            match Request::decode(&bytes) {
                Some(asked) => {
                    let reply = Reply::decode(&out, &asked);
                    assert!(reply.is_some(), "{asked:?} answered {out:?}");
                    if let Some(Reply::Opened { handle, .. }) = reply {
                        handles.push(handle);
                    }
                }
                None => assert_eq!(out, failed, "an undecodable request is refused as failed"),
            }
        }
        drop(server);
        assert_eq!(untouchable(&tree), before, "only the writable root changes");
        runs += 1;
    }
}

#[test]
fn whatever_decodes_reencodes_to_its_own_bytes() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "whatever_decodes_reencodes_to_its_own_bytes",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let tree = tree();
    let mut server = server(&tree);
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS * 50 => break,
            _ => {}
        }
        let bytes = request(&mut rng, &[]);
        if let Some(request) = Request::decode(&bytes) {
            let mut again = Vec::new();
            request.encode(&mut again);
            assert_eq!(again, bytes, "{request:?}");
            let mut out = Vec::new();
            server.serve(&bytes, &mut out);
            let reply = if rng.below(2) == 0 {
                rng.mutated(&out)
            } else {
                out
            };
            if let Some(decoded) = Reply::decode(&reply, &request) {
                let mut again = Vec::new();
                decoded.encode(&mut again);
                assert_eq!(again, reply, "{decoded:?}");
            }
        }
        runs += 1;
    }
}

/// A session's host whose relay carries every request to the server and
/// damages some of the replies.
struct Damaging {
    still: StillHost,
    server: Server<Handle>,
    rng: Rc<RefCell<Prng>>,
}

impl Host for Damaging {
    fn monotonic(&mut self) -> u64 {
        self.still.monotonic()
    }

    fn real_time(&mut self) -> u64 {
        self.still.real_time()
    }

    fn pulse(&mut self) -> Pulse {
        self.still.pulse()
    }

    fn wait(&mut self, until: Option<u64>, ending: Ending) -> Pulse {
        self.still.wait(until, ending)
    }

    fn input(&mut self) -> Option<InputEvent> {
        self.still.input()
    }

    fn relay(&mut self, request: &[u8], reply: &mut Vec<u8>) -> bool {
        let mut rng = self.rng.borrow_mut();
        if rng.below(12) == 0 {
            return false;
        }
        let mut out = Vec::new();
        self.server.serve(request, &mut out);
        reply.extend_from_slice(&if rng.below(4) == 0 {
            rng.mutated(&out)
        } else {
            out
        });
        true
    }
    fn font(&mut self, request: &[u8], reply: &mut Vec<u8>) -> bool {
        self.still.font(request, reply)
    }
}

/// The names a session's calls are given.
const NAMES: [&[u8]; 8] = [
    b"HostFS:$.File\0",
    b"HostFS:$.Dir\0",
    b"HostFS:$.Dir.Inner\0",
    b"HostFS::RO.$.File\0",
    b"HostFS::Transfer.$.1.Doc\0",
    b"HostFS::Scrap.$.Kept\0",
    b"HostFS:$.New\0",
    b"HostFS:$\0",
];

#[test]
fn a_session_given_damaged_replies_still_answers() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let rng = Rc::new(RefCell::new(Prng::new(tairix_fuzzseed::start(
        "a_session_given_damaged_replies_still_answers",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ))));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs * 4 >= SMOKE_RUNS => break,
            _ => {}
        }
        let tree = tree();
        let before = untouchable(&tree);
        let host = Damaging {
            still: StillHost,
            server: server(&tree),
            rng: Rc::clone(&rng),
        };
        let mut kernel = common::kernel_on(Box::new(host));
        application_slot(&mut kernel, SLOT_BYTES);
        let mut os = kernel.os();
        let names: Vec<u32> = NAMES.iter().map(|name| claim(&mut os, name)).collect();
        // A load writes a file whole, however long a damaged reply makes it,
        // so the buffer is the application slot, whose end aborts it.
        let buffer = APPLICATION;
        for _ in 0..STEPS {
            let mut rng = rng.borrow_mut();
            let (name, other) = (*rng.pick(&names), *rng.pick(&names));
            let (swi, args) = match rng.below(6) {
                0 => (FILE, [5, name, 0, 0, 0, 0, 0]),
                1 => (FILE, [255, name, buffer, 0, 0, 0, 0]),
                2 => (
                    FILE,
                    [
                        10,
                        name,
                        0xFFF,
                        0,
                        buffer,
                        buffer + (rng.next_u32() & 0xFFF),
                        0,
                    ],
                ),
                3 => (FIND, [*rng.pick(&[0x40, 0x80, 0xC0]), name, 0, 0, 0, 0, 0]),
                4 => (GBPB, [10, name, buffer, 8, 0, 4096, 0]),
                _ => (FS_CONTROL, [25, name, other, 0, 0, 0, 0]),
            };
            drop(rng);
            let mut regs = Regs::default();
            regs.r[..7].copy_from_slice(&args);
            if os.perform(X | swi, &mut regs).is_ok() && swi == FIND && regs.r[0] != 0 {
                let mut close = Regs::default();
                close.r[1] = regs.r[0];
                let _ = os.perform(X | FIND, &mut close);
            }
        }
        let mut monotonic = Regs::default();
        assert!(
            os.perform(X | 0x42, &mut monotonic).is_ok(),
            "the session still answers"
        );
        drop(kernel);
        assert_eq!(untouchable(&tree), before, "only the writable root changes");
        runs += 1;
    }
}

/// `name` in the RMA.
fn claim(os: &mut Os<'_>, name: &[u8]) -> u32 {
    let at = os
        .rma_claim(u32::try_from(name.len()).expect("short"))
        .expect("room");
    os.write_bytes(at, name).expect("written");
    at
}
