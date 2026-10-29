//! Unit tests for the worker, driven against the scripted SMB 3 server.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::netshare_ipc::SHARES_MAX;
use tairix_abi::time::Time64;
use tairix_sandbox::proto::ProtoError;
use tairix_sandbox::session::{FrameOut, SessionService, SessionStep};
use tairix_smb::crypt::NONCE_SEED_LEN;
use tairix_smb::negotiate::{Dialect, SALT_LEN};
use tairix_smb::ntlm::NtlmCredential;
use tairix_smb::peer::{Fault, Peer, PeerConfig, DISK_SHARE, SERVER_GUID};
use tairix_smb::status::NtStatus;
use tairix_smb::tree::ShareType;
use tairix_smb::SmbError;

use super::Worker;
use crate::wire::{Credential, FileOp, Order, Report};

/// Collects the frames a worker writes.
#[derive(Default)]
struct Frames(Vec<Vec<u8>>);

impl FrameOut for Frames {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.0.push(payload.to_vec());
        Ok(())
    }
}

/// A report, held.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Held {
    Negotiated([u8; 16], Dialect),
    Established,
    Rejected(NtStatus),
    Share(u32, String, bool),
    Listed(u32, u32, u32, u32),
    Connected(u32, ShareType),
    Refused(u32, SmbError),
    Failed(SmbError),
    Stat(u32, bool, u64),
    Data(u32, Vec<u8>),
    Entry(u32, String),
    Page(u32, bool),
    Done(u32, u32),
    Space(u32),
    Opened(u32, u64, u8),
    Break([u8; 16], u8, bool),
}

/// A worker wired to a scripted server, the bytes between them carried as
/// the front would carry them.
struct Bench {
    worker: Worker,
    peer: Peer,
    config: PeerConfig,
    seen: Vec<Held>,
    /// The tree the last connect reported.
    tree: Option<u32>,
}

impl Bench {
    fn new(config: PeerConfig) -> Self {
        Self {
            worker: Worker::new(),
            peer: Peer::new(config.clone()),
            config,
            seen: Vec::new(),
            tree: None,
        }
    }

    /// Hand `order` to the worker, then carry bytes both ways until neither
    /// side has more; whether the worker carries on.
    fn order(&mut self, order: &Order<'_>) -> SessionStep {
        let mut bytes = Vec::new();
        order.encode(&mut bytes).expect("encodes");
        match self.handle(&bytes) {
            SessionStep::Continue => self.pump(),
            SessionStep::Finished => SessionStep::Finished,
        }
    }

    /// Carry the server's bytes to the worker until it has no more to say.
    fn pump(&mut self) -> SessionStep {
        loop {
            let to_worker = self.peer.take_output();
            if to_worker.is_empty() {
                return SessionStep::Continue;
            }
            for chunk in to_worker.chunks(crate::wire::CHUNK_MAX) {
                let mut bytes = Vec::new();
                Order::Received(chunk).encode(&mut bytes).expect("encodes");
                if self.handle(&bytes) == SessionStep::Finished {
                    return SessionStep::Finished;
                }
            }
        }
    }

    fn handle(&mut self, bytes: &[u8]) -> SessionStep {
        let mut frames = Frames::default();
        let step = self.worker.handle(bytes, &mut frames);
        for frame in &frames.0 {
            match Report::decode(frame).expect("a report the front reads") {
                Report::Send(bytes) => self
                    .peer
                    .receive(bytes)
                    .expect("the server reads what the worker sends"),
                report => {
                    if let Report::Connected { tree, .. } = report {
                        self.tree = Some(tree);
                    }
                    self.seen.push(held(&report));
                }
            }
        }
        step
    }

    fn open(&mut self) {
        let step = self.order(&Order::Open {
            floor: Dialect::Smb300,
            client_guid: [0x11; 16],
            salt: [0x22; SALT_LEN],
            server: self.config.host,
        });
        assert_eq!(step, SessionStep::Continue);
    }

    fn sign_in(&mut self, password: &str) {
        let key = *NtlmCredential::new(self.config.user, self.config.domain, password)
            .expect("fits")
            .response_key();
        let step = self.order(&Order::SignIn {
            credential: Credential {
                user: self.config.user,
                domain: self.config.domain,
                key: &key,
                client_challenge: [0xcc; 8],
                now: Time64::from_secs(1_790_000_000),
            },
            nonce_seed: [0x3c; NONCE_SEED_LEN],
            previous: None,
        });
        assert_eq!(step, SessionStep::Continue);
    }

    fn established(config: PeerConfig) -> Self {
        let mut bench = Self::new(config);
        bench.open();
        let password = bench.config.password;
        bench.sign_in(password);
        assert_eq!(
            bench.take(),
            [
                Held::Negotiated(SERVER_GUID, bench.config.dialect),
                Held::Established
            ]
        );
        bench
    }

    fn take(&mut self) -> Vec<Held> {
        core::mem::take(&mut self.seen)
    }

    fn file(&mut self, request: u32, tree: u32, op: FileOp<'_>) {
        assert_eq!(
            self.order(&Order::File { request, tree, op }),
            SessionStep::Continue
        );
    }

    fn stage(&mut self, request: u32, chunk: &[u8]) {
        assert_eq!(
            self.order(&Order::Stage { request, chunk }),
            SessionStep::Continue
        );
    }

    /// Orders handed to the worker one after another before any answer
    /// reaches it, then the answers carried.
    fn at_once(&mut self, orders: &[Order<'_>]) {
        let mut bytes = Vec::new();
        for order in orders {
            order.encode(&mut bytes).expect("encodes");
            assert_eq!(self.handle(&bytes), SessionStep::Continue);
        }
        assert_eq!(self.pump(), SessionStep::Continue);
    }
}

/// A bench with the disk share's tree connected, and the tree.
fn on_disk(config: PeerConfig) -> (Bench, u32) {
    let mut bench = Bench::established(config);
    bench.order(&Order::Connect {
        request: 1,
        share: DISK_SHARE,
    });
    assert_eq!(bench.take(), [Held::Connected(1, ShareType::Disk)]);
    let tree = bench.tree.expect("connected");
    (bench, tree)
}

/// Make the file `path` holding `data` on `tree`.
fn put(bench: &mut Bench, tree: u32, path: &str, data: &[u8]) {
    bench.file(
        90,
        tree,
        FileOp::Create {
            path,
            directory: false,
        },
    );
    bench.stage(91, data);
    let len = u32::try_from(data.len()).expect("a length");
    bench.file(
        91,
        tree,
        FileOp::Write {
            path,
            offset: 0,
            len,
        },
    );
    assert_eq!(
        bench.take(),
        [Held::Stat(90, false, 0), Held::Done(91, len)]
    );
}

fn held(report: &Report<'_>) -> Held {
    match *report {
        Report::Negotiated {
            server_guid,
            dialect,
            ..
        } => Held::Negotiated(server_guid, dialect),
        Report::Established { .. } => Held::Established,
        Report::Rejected(status) => Held::Rejected(status),
        Report::Share {
            request,
            name,
            special,
            ..
        } => Held::Share(request, String::from(name), special),
        Report::Listed {
            request,
            hidden,
            unnamed,
            unlisted,
        } => Held::Listed(request, hidden, unnamed, unlisted),
        Report::Connected { request, kind, .. } => Held::Connected(request, kind),
        Report::Refused { request, error } => Held::Refused(request, error),
        Report::Failed(error) => Held::Failed(error),
        Report::Stat { request, stat } => Held::Stat(request, stat.directory, stat.size),
        Report::Data { request, chunk } => Held::Data(request, chunk.to_vec()),
        Report::Entry { request, name, .. } => Held::Entry(request, String::from(name)),
        Report::Page { request, more } => Held::Page(request, more),
        Report::Done { request, count, .. } => Held::Done(request, count),
        Report::Space { request, .. } => Held::Space(request),
        Report::Held {
            request,
            stat,
            lease,
            ..
        } => Held::Opened(request, stat.size, lease),
        Report::Break { key, new, ack } => Held::Break(key, new, ack),
        Report::Send(_) | Report::Expired | Report::Renewed => {
            panic!("not expected here: {report:?}")
        }
    }
}

#[test]
fn a_worker_negotiates_signs_in_and_lists_the_disk_shares() {
    for dialect in Dialect::ALL {
        let mut bench = Bench::established(PeerConfig::new(dialect));
        bench.order(&Order::List {
            request: 1,
            special: false,
        });
        assert_eq!(
            bench.take(),
            [
                Held::Share(1, String::from(DISK_SHARE), false),
                Held::Listed(1, 1, 0, 0)
            ],
            "{dialect:?}"
        );
        bench.order(&Order::List {
            request: 2,
            special: true,
        });
        assert_eq!(
            bench.take(),
            [
                Held::Share(2, String::from(DISK_SHARE), false),
                Held::Share(2, String::from("C$"), true),
                Held::Listed(2, 0, 0, 0)
            ],
            "{dialect:?}"
        );
        assert_eq!(bench.peer.open_pipes(), 0);
    }
}

#[test]
fn a_listing_past_its_bound_keeps_the_first_shares_and_counts_the_rest() {
    let mut bench = Bench::established(PeerConfig {
        extra_shares: 300,
        ..PeerConfig::new(Dialect::Smb311)
    });
    bench.order(&Order::List {
        request: 9,
        special: false,
    });
    let seen = bench.take();
    assert_eq!(seen.len(), SHARES_MAX + 1);
    assert_eq!(seen.last(), Some(&Held::Listed(9, 1, 0, 45)));
}

#[test]
fn a_tree_connects_or_is_refused_with_the_server_s_status() {
    let mut bench = Bench::established(PeerConfig::new(Dialect::Smb311));
    bench.order(&Order::Connect {
        request: 3,
        share: DISK_SHARE,
    });
    assert_eq!(bench.take(), [Held::Connected(3, ShareType::Disk)]);
    bench.order(&Order::Connect {
        request: 4,
        share: "nosuch",
    });
    assert_eq!(
        bench.take(),
        [Held::Refused(
            4,
            SmbError::Status(NtStatus::BAD_NETWORK_NAME)
        )]
    );
}

#[test]
fn a_server_granting_one_credit_a_response_still_serves_every_request() {
    let mut bench = Bench::established(PeerConfig {
        grant: 1,
        ..PeerConfig::new(Dialect::Smb311)
    });
    let mut bytes = Vec::new();
    // Three requests at once: each waits for the credit the last one freed.
    for order in [
        Order::List {
            request: 1,
            special: false,
        },
        Order::Connect {
            request: 2,
            share: DISK_SHARE,
        },
        Order::List {
            request: 3,
            special: true,
        },
    ] {
        order.encode(&mut bytes).expect("encodes");
        assert_eq!(bench.handle(&bytes), SessionStep::Continue);
    }
    assert_eq!(bench.pump(), SessionStep::Continue);
    let seen = bench.take();
    assert!(seen.contains(&Held::Listed(1, 1, 0, 0)), "{seen:?}");
    assert!(
        seen.contains(&Held::Connected(2, ShareType::Disk)),
        "{seen:?}"
    );
    assert!(seen.contains(&Held::Listed(3, 0, 0, 0)), "{seen:?}");
}

#[test]
fn a_wrong_password_is_the_server_s_refusal() {
    let mut bench = Bench::new(PeerConfig::new(Dialect::Smb311));
    bench.open();
    bench.sign_in("not the password");
    assert_eq!(
        bench.take(),
        [
            Held::Negotiated(SERVER_GUID, Dialect::Smb311),
            Held::Rejected(NtStatus::LOGON_FAILURE)
        ]
    );
}

#[test]
fn a_listing_the_server_denies_is_refused_with_its_status() {
    let mut bench = Bench::established(PeerConfig {
        fault: Fault::DenyShareEnum,
        ..PeerConfig::new(Dialect::Smb311)
    });
    bench.order(&Order::List {
        request: 5,
        special: false,
    });
    assert_eq!(bench.take(), [Held::Refused(5, SmbError::Rpc(5))]);
}

#[test]
fn a_server_breaking_the_protocol_ends_the_connection_once() {
    let mut bench = Bench::established(PeerConfig::new(Dialect::Smb311));
    let mut bytes = Vec::new();
    Order::Received(&[0, 0, 0, 4, 0xfe, b'S', b'M', b'B'])
        .encode(&mut bytes)
        .expect("encodes");
    assert_eq!(bench.handle(&bytes), SessionStep::Continue);
    let seen = bench.take();
    assert!(matches!(seen[..], [Held::Failed(_)]), "{seen:?}");
    assert_eq!(bench.handle(&bytes), SessionStep::Continue);
    bench.order(&Order::List {
        request: 1,
        special: false,
    });
    assert_eq!(bench.take(), [], "nothing more is reported");
}

#[test]
fn an_order_the_front_never_sends_ends_the_session() {
    let mut fresh = Bench::new(PeerConfig::new(Dialect::Smb311));
    assert_eq!(
        fresh.order(&Order::List {
            request: 1,
            special: false,
        }),
        SessionStep::Finished,
        "an order before the connection is opened"
    );
    let mut bench = Bench::established(PeerConfig::new(Dialect::Smb311));
    let open = Order::Open {
        floor: Dialect::Smb300,
        client_guid: [0; 16],
        salt: [0; SALT_LEN],
        server: "nas.example",
    };
    assert_eq!(bench.order(&open), SessionStep::Finished, "a second open");
    assert_eq!(
        Worker::new().handle(&[0xff], &mut Frames::default()),
        SessionStep::Finished,
        "a frame no order is"
    );
}

#[test]
fn a_write_whose_staged_bytes_fell_short_is_refused_unsent() {
    let (mut bench, tree) = on_disk(PeerConfig::new(Dialect::Smb311));
    put(&mut bench, tree, "f", b"before");
    bench.stage(5, b"hel");
    bench.file(
        5,
        tree,
        FileOp::Write {
            path: "f",
            offset: 0,
            len: 5,
        },
    );
    assert_eq!(bench.take(), [Held::Refused(5, SmbError::OutOfMemory)]);
    bench.stage(6, b"aaa");
    bench.stage(7, b"bbb");
    for request in [6, 7] {
        bench.file(
            request,
            tree,
            FileOp::Write {
                path: "f",
                offset: 0,
                len: 3,
            },
        );
    }
    assert_eq!(
        bench.take(),
        [Held::Refused(6, SmbError::OutOfMemory), Held::Done(7, 3)],
        "only the bytes staged last are held"
    );
    assert_eq!(bench.peer.share().contents(&["f"]), Some(&b"bbbore"[..]));
}

#[test]
fn a_read_moves_no_more_than_it_asked_and_nothing_from_the_end_on() {
    let (mut bench, tree) = on_disk(PeerConfig::new(Dialect::Smb311));
    put(&mut bench, tree, "f", b"0123456789");
    let read = |offset, len| FileOp::Read {
        path: "f",
        offset,
        len,
    };
    bench.file(10, tree, read(8, 64));
    assert_eq!(
        bench.take(),
        [Held::Data(10, b"89".to_vec()), Held::Done(10, 2)]
    );
    for offset in [10, 11, u64::MAX] {
        bench.file(11, tree, read(offset, 4));
        assert_eq!(bench.take(), [Held::Done(11, 0)], "{offset}");
    }
    assert_eq!(bench.peer.share().attached_opens(), 0);

    let (mut long, tree) = on_disk(PeerConfig {
        fault: Fault::LongRead,
        ..PeerConfig::new(Dialect::Smb311)
    });
    put(&mut long, tree, "f", b"0123456789");
    long.file(12, tree, read(0, 4));
    assert_eq!(
        long.take(),
        [Held::Refused(12, SmbError::Protocol)],
        "more than was asked for is no data"
    );
}

#[test]
fn an_open_a_failed_compound_left_behind_is_closed() {
    let (mut bench, tree) = on_disk(PeerConfig {
        fault: Fault::ChainedFailure,
        ..PeerConfig::new(Dialect::Smb311)
    });
    put(&mut bench, tree, "f", b"0123456789");
    bench.file(
        10,
        tree,
        FileOp::Read {
            path: "f",
            offset: 10,
            len: 4,
        },
    );
    assert_eq!(bench.take(), [Held::Done(10, 0)]);
    assert_eq!(bench.peer.share().attached_opens(), 0);
}

#[test]
fn a_listing_let_go_while_it_opens_is_closed_once_open() {
    let (mut bench, tree) = on_disk(PeerConfig::new(Dialect::Smb311));
    bench.at_once(&[
        Order::File {
            request: 20,
            tree,
            op: FileOp::Open {
                path: "",
                handle: 1,
            },
        },
        Order::File {
            request: 21,
            tree,
            op: FileOp::Close { handle: 1 },
        },
    ]);
    assert_eq!(bench.take(), [Held::Done(21, 0), Held::Done(20, 0)]);
    assert_eq!(bench.peer.share().attached_opens(), 0);
}

#[test]
fn a_disconnected_tree_takes_its_listings_with_it() {
    let (mut bench, tree) = on_disk(PeerConfig::new(Dialect::Smb311));
    bench.file(
        30,
        tree,
        FileOp::Open {
            path: "",
            handle: 4,
        },
    );
    assert_eq!(bench.take(), [Held::Done(30, 0)]);
    bench.order(&Order::Disconnect { tree });
    bench.file(31, tree, FileOp::Page { handle: 4 });
    assert_eq!(bench.take(), [Held::Refused(31, SmbError::Protocol)]);
}

#[test]
fn a_rename_replaces_a_file_only_when_asked() {
    let (mut bench, tree) = on_disk(PeerConfig::new(Dialect::Smb311));
    put(&mut bench, tree, "a", b"new");
    put(&mut bench, tree, "b", b"old");
    let rename = |replace| FileOp::Rename {
        from: "a",
        to: "b",
        replace,
        lease: None,
    };
    bench.file(40, tree, rename(false));
    assert_eq!(
        bench.take(),
        [Held::Refused(
            40,
            SmbError::Status(NtStatus::OBJECT_NAME_COLLISION)
        )]
    );
    bench.file(41, tree, rename(true));
    assert_eq!(bench.take(), [Held::Done(41, 0)]);
    assert_eq!(bench.peer.share().contents(&["b"]), Some(&b"new"[..]));
    assert_eq!(bench.peer.share().contents(&["a"]), None);
}
