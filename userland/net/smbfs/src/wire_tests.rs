//! Unit tests for the front ↔ worker frames.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::netshare_ipc::{ACCOUNT_NAME_MAX, REMARK_MAX, SHARE_NAME_MAX};
use tairix_abi::time::Time64;
use tairix_smb::crypt::NONCE_SEED_LEN;
use tairix_smb::negotiate::{Dialect, SALT_LEN};
use tairix_smb::status::NtStatus;
use tairix_smb::tree::ShareType;
use tairix_smb::{Refusal, SmbError};

use super::{
    encode_stage, Credential, Durability, FileOp, FrameError, Leasing, Order, Report, Space, Stat,
    CHUNK_MAX, KEY_LEN, ORDER_MAX, ORDER_QUEUE, PIPELINE, REPORT_MAX, REPORT_QUEUE,
};
use tairix_abi::fs::FS_PATH_MAX;

const KEY: [u8; KEY_LEN] = [0x5a; KEY_LEN];

fn credential(user: &str) -> Credential<'_> {
    Credential {
        user,
        domain: "WORKGROUP",
        key: &KEY,
        client_challenge: [0xcc; 8],
        now: Time64::new(-42, 999_999_999).expect("valid"),
    }
}

fn encoded_order(order: &Order<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    order.encode(&mut out).expect("encodes");
    out
}

fn encoded_report(report: &Report<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    report.encode(&mut out).expect("encodes");
    out
}

fn orders() -> Vec<Order<'static>> {
    vec![
        Order::Open {
            floor: Dialect::Smb302,
            client_guid: [0x11; 16],
            salt: [0x22; SALT_LEN],
            server: "nas.example",
        },
        Order::Received(b"\x00\x00\x00\x04abcd"),
        Order::SignIn {
            credential: credential("alice"),
            nonce_seed: [0x33; NONCE_SEED_LEN],
            previous: None,
        },
        Order::SignIn {
            credential: credential("alice"),
            nonce_seed: [0x33; NONCE_SEED_LEN],
            previous: Some(0x0123_4567_89ab_cdef),
        },
        Order::Renew(credential("bob")),
        Order::List {
            request: 7,
            special: true,
        },
        Order::List {
            request: 8,
            special: false,
        },
        Order::Connect {
            request: u32::MAX,
            share: "Media",
        },
        Order::Disconnect { tree: 9 },
        Order::Stage {
            request: 10,
            chunk: b"staged bytes",
        },
        Order::Echo,
    ]
    .into_iter()
    .chain(file_ops().into_iter().map(|op| Order::File {
        request: 11,
        tree: 12,
        op,
    }))
    .collect()
}

fn file_ops() -> Vec<FileOp<'static>> {
    let mut ops = vec![
        FileOp::Stat {
            path: "docs\\a.txt",
        },
        FileOp::Stat { path: "" },
        FileOp::Read {
            path: "a",
            offset: u64::MAX,
            len: 1,
        },
        FileOp::Read {
            path: "a",
            offset: 0,
            len: 64 << 10,
        },
        FileOp::Write {
            path: "a",
            offset: 7,
            len: 1,
        },
        FileOp::Write {
            path: "a",
            offset: u64::MAX,
            len: 64 << 10,
        },
        FileOp::Open {
            path: "docs",
            handle: u32::MAX,
        },
        FileOp::Page { handle: 3 },
        FileOp::Close { handle: 3 },
        FileOp::Create {
            path: "new",
            directory: true,
        },
        FileOp::Create {
            path: "new",
            directory: false,
        },
        FileOp::Remove {
            path: "old",
            lease: None,
        },
        FileOp::Remove {
            path: "old",
            lease: Some([0x44; 16]),
        },
        FileOp::Rename {
            from: "a",
            to: "b\\a",
            replace: true,
            lease: Some([0x55; 16]),
        },
        FileOp::Rename {
            from: "a",
            to: "A",
            replace: false,
            lease: None,
        },
        FileOp::Truncate {
            path: "a",
            size: u64::MAX,
        },
        FileOp::Space,
    ];
    ops.extend(held_ops());
    ops
}

fn held_ops() -> Vec<FileOp<'static>> {
    vec![
        FileOp::Hold {
            path: "a",
            handle: 9,
            key: [0x66; 16],
            directory: false,
            write: true,
            durability: Durability::Ask {
                create_guid: [0x67; 16],
                replay: false,
            },
        },
        FileOp::Hold {
            path: "a",
            handle: 9,
            key: [0x66; 16],
            directory: false,
            write: true,
            durability: Durability::Ask {
                create_guid: [0x67; 16],
                replay: true,
            },
        },
        FileOp::Hold {
            path: "a\\b",
            handle: 1,
            key: [0x68; 16],
            directory: false,
            write: false,
            durability: Durability::Reclaim {
                create_guid: [0x69; 16],
                file_id: [0x6a; 16],
            },
        },
        FileOp::Hold {
            path: "",
            handle: u32::MAX,
            key: [0; 16],
            directory: true,
            write: false,
            durability: Durability::None,
        },
        FileOp::ReadHeld {
            handle: 9,
            offset: u64::MAX,
            len: 64 << 10,
        },
        FileOp::WriteHeld {
            handle: 9,
            offset: 3,
            len: 1,
            replay: false,
        },
        FileOp::WriteHeld {
            handle: 9,
            offset: u64::MAX,
            len: 64 << 10,
            replay: true,
        },
        FileOp::Sync { handle: 9 },
        FileOp::Resize {
            handle: 9,
            size: u64::MAX,
        },
        FileOp::Ack {
            key: [0x77; 16],
            state: 0b101,
        },
    ]
}

fn stat(directory: bool) -> Stat {
    Stat {
        directory,
        read_only: !directory,
        size: if directory { 0 } else { u64::MAX },
        allocated: 4096,
        file_id: 0x0123_4567_89ab_cdef,
        created: Time64::new(-1, 1).expect("valid"),
        modified: Time64::from_secs(i64::MAX),
        accessed: Time64::from_secs(0),
        changed: Time64::new(1_700_000_000, 999_999_999).expect("valid"),
    }
}

fn reports() -> Vec<Report<'static>> {
    let errors = [
        SmbError::Malformed,
        SmbError::Protocol,
        SmbError::Refused(Refusal::Dialect),
        SmbError::Refused(Refusal::Encryption),
        SmbError::Refused(Refusal::Signing),
        SmbError::Refused(Refusal::Guest),
        SmbError::Refused(Refusal::Integrity),
        SmbError::Refused(Refusal::Downgrade),
        SmbError::Refused(Refusal::Validation),
        SmbError::Refused(Refusal::Mechanism),
        SmbError::Refused(Refusal::Ntlm),
        SmbError::Integrity,
        SmbError::Status(NtStatus::BAD_NETWORK_NAME),
        SmbError::OutOfMemory,
        SmbError::TooLarge,
        SmbError::NoCredit,
        SmbError::TimeOutOfRange,
        SmbError::Name,
        SmbError::Rpc(5),
    ];
    let mut reports = vec![
        Report::Send(b"bytes"),
        Report::Negotiated {
            server_guid: *b"tairix test peer",
            dialect: Dialect::Smb311,
            leasing: Leasing::Files,
        },
        Report::Negotiated {
            server_guid: [0; 16],
            dialect: Dialect::Smb300,
            leasing: Leasing::None,
        },
        Report::Negotiated {
            server_guid: [1; 16],
            dialect: Dialect::Smb302,
            leasing: Leasing::Directories,
        },
        Report::Break {
            key: [0x88; 16],
            new: 0b001,
            ack: true,
        },
        Report::Break {
            key: [0x99; 16],
            new: 0,
            ack: false,
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
            unlisted: u32::MAX,
        },
    ];
    reports.extend(
        [ShareType::Disk, ShareType::Pipe, ShareType::Print].map(|kind| Report::Connected {
            request: 4,
            tree: 5,
            kind,
        }),
    );
    for error in errors {
        reports.push(Report::Refused { request: 6, error });
        reports.push(Report::Failed(error));
    }
    reports.extend(file_reports());
    reports
}

fn file_reports() -> Vec<Report<'static>> {
    vec![
        Report::Stat {
            request: 7,
            stat: stat(false),
        },
        Report::Stat {
            request: 7,
            stat: stat(true),
        },
        Report::Data {
            request: 8,
            chunk: b"read bytes",
        },
        Report::Entry {
            request: 9,
            name: "caf\u{e9}.txt",
            stat: stat(false),
        },
        Report::Page {
            request: 9,
            more: true,
        },
        Report::Page {
            request: 9,
            more: false,
        },
        Report::Done {
            request: 10,
            count: u32::MAX,
            file_id: 0,
        },
        Report::Space {
            request: 11,
            space: Space {
                unit: 4096,
                total: u64::MAX,
                free: 1,
                available: 0,
            },
        },
        Report::Held {
            request: 12,
            stat: stat(false),
            lease: 0b111,
            file_id: [0x7b; 16],
            durable_ms: Some(60_000),
        },
        Report::Held {
            request: 12,
            stat: stat(true),
            lease: 0,
            file_id: [0; 16],
            durable_ms: None,
        },
    ]
}

#[test]
fn every_order_round_trips() {
    for order in orders() {
        let bytes = encoded_order(&order);
        assert_eq!(Order::decode(&bytes), Ok(order), "{order:?}");
    }
}

#[test]
fn every_report_round_trips() {
    for report in reports() {
        let bytes = encoded_report(&report);
        assert_eq!(Report::decode(&bytes), Ok(report), "{report:?}");
    }
}

#[test]
fn a_frame_cut_short_or_running_past_its_end_is_refused() {
    let frames: Vec<Vec<u8>> = orders()
        .iter()
        .map(encoded_order)
        .chain(reports().iter().map(encoded_report))
        .collect();
    let decodes = |bytes: &[u8], order: bool| {
        if order {
            Order::decode(bytes).map(|_| ())
        } else {
            Report::decode(bytes).map(|_| ())
        }
    };
    let orders = orders().len();
    for (at, frame) in frames.iter().enumerate() {
        let order = at < orders;
        for cut in 0..frame.len() {
            assert_eq!(
                decodes(&frame[..cut], order),
                Err(FrameError::Malformed),
                "{at} cut at {cut}"
            );
        }
        let mut longer = frame.clone();
        longer.push(0);
        assert_eq!(decodes(&longer, order), Err(FrameError::Malformed), "{at}");
    }
}

#[test]
fn staged_parts_encode_as_the_order_carrying_them_joined() {
    let joined: Vec<u8> = (0..CHUNK_MAX).map(|at| at.to_le_bytes()[0]).collect();
    let (head, tail) = joined.split_at(100);
    let (middle, tail) = tail.split_at(4000);
    let mut whole = Vec::new();
    Order::Stage {
        request: 7,
        chunk: &joined,
    }
    .encode(&mut whole)
    .expect("encodes");
    let mut parted = Vec::new();
    encode_stage(7, &[head, middle, tail], &mut parted).expect("encodes");
    assert_eq!(parted, whole);
    assert_eq!(
        Order::decode(&parted),
        Ok(Order::Stage {
            request: 7,
            chunk: &joined
        })
    );
    let over = [0; 1];
    assert_eq!(
        encode_stage(7, &[&joined, &over], &mut parted),
        Err(FrameError::Malformed),
        "past one chunk"
    );
    assert_eq!(
        encode_stage(7, &[&[], &[]], &mut parted),
        Err(FrameError::Malformed),
        "nothing"
    );
}

#[test]
fn an_order_out_of_shape_is_never_encoded() {
    let long_share = "s".repeat(SHARE_NAME_MAX + 1);
    let long_user = "u".repeat(ACCOUNT_NAME_MAX + 1);
    let chunk = vec![0; CHUNK_MAX + 1];
    for (what, order) in [
        ("no bytes", Order::Received(&[])),
        ("too many bytes", Order::Received(&chunk)),
        (
            "a share no tree connect takes",
            Order::Connect {
                request: 1,
                share: r"a\b",
            },
        ),
        (
            "a share too long",
            Order::Connect {
                request: 1,
                share: &long_share,
            },
        ),
        ("an empty user", Order::Renew(credential(""))),
        ("a user too long", Order::Renew(credential(&long_user))),
        (
            "a previous session of id zero",
            Order::SignIn {
                credential: credential("alice"),
                nonce_seed: [0; NONCE_SEED_LEN],
                previous: Some(0),
            },
        ),
        (
            "a server that is no host",
            Order::Open {
                floor: Dialect::Smb300,
                client_guid: [0; 16],
                salt: [0; SALT_LEN],
                server: "a b",
            },
        ),
        (
            "no staged bytes",
            Order::Stage {
                request: 1,
                chunk: &[],
            },
        ),
        (
            "too many staged bytes",
            Order::Stage {
                request: 1,
                chunk: &chunk,
            },
        ),
    ] {
        assert_eq!(
            order.encode(&mut Vec::new()),
            Err(FrameError::Malformed),
            "{what}"
        );
    }
}

#[test]
fn a_file_operation_out_of_shape_is_never_encoded() {
    for (what, op) in [
        (
            "an empty read",
            FileOp::Read {
                path: "a",
                offset: 0,
                len: 0,
            },
        ),
        (
            "a read past one call",
            FileOp::Read {
                path: "a",
                offset: 0,
                len: (64 << 10) + 1,
            },
        ),
        (
            "an empty write",
            FileOp::Write {
                path: "a",
                offset: 0,
                len: 0,
            },
        ),
        (
            "a write past one call",
            FileOp::Write {
                path: "a",
                offset: 0,
                len: (64 << 10) + 1,
            },
        ),
        (
            "a path with a leading separator",
            FileOp::Stat { path: "\\a" },
        ),
        (
            "a path holding a dot name",
            FileOp::Remove {
                path: "a\\..",
                lease: None,
            },
        ),
        (
            "a path holding a stream",
            FileOp::Rename {
                from: "a",
                to: "b:s",
                replace: false,
                lease: None,
            },
        ),
        (
            "an empty held read",
            FileOp::ReadHeld {
                handle: 1,
                offset: 0,
                len: 0,
            },
        ),
        (
            "a held write past one call",
            FileOp::WriteHeld {
                handle: 1,
                offset: 0,
                len: (64 << 10) + 1,
                replay: false,
            },
        ),
        (
            "a lease bit no lease has",
            FileOp::Ack {
                key: [0; 16],
                state: 0b1000,
            },
        ),
    ] {
        let order = Order::File {
            request: 1,
            tree: 1,
            op,
        };
        assert_eq!(
            order.encode(&mut Vec::new()),
            Err(FrameError::Malformed),
            "{what}"
        );
    }
}

#[test]
fn a_report_out_of_shape_is_never_encoded() {
    let chunk = vec![0; CHUNK_MAX + 1];
    let long_remark = "r".repeat(REMARK_MAX + 1);
    for (what, report) in [
        ("no bytes", Report::Send(&[])),
        ("too many bytes", Report::Send(&chunk)),
        (
            "a share no tree connect takes",
            Report::Share {
                request: 1,
                name: "a/b",
                special: false,
                remark: "",
            },
        ),
        (
            "a remark too long",
            Report::Share {
                request: 1,
                name: "share",
                special: false,
                remark: &long_remark,
            },
        ),
    ] {
        assert_eq!(
            report.encode(&mut Vec::new()),
            Err(FrameError::Malformed),
            "{what}"
        );
    }
}

#[test]
fn a_frame_holding_what_no_encoder_writes_is_refused() {
    let changed = |bytes: &[u8], at: usize, byte: u8| {
        let mut bytes = bytes.to_vec();
        bytes[at] = byte;
        bytes
    };
    let open = encoded_order(&orders()[0]);
    let sign_in = encoded_order(&orders()[2]);
    let list = encoded_order(&orders()[5]);
    let previous_at = sign_in.len() - 9;
    for (what, bytes) in [
        ("an unknown order", changed(&open, 0, 0)),
        ("an unknown dialect", changed(&open, 1, 3)),
        ("a flag that is neither", changed(&list, 5, 2)),
        (
            "no previous session, but an id",
            changed(&sign_in, sign_in.len() - 1, 1),
        ),
        (
            "a previous session marked twice",
            changed(&sign_in, previous_at, 2),
        ),
    ] {
        assert_eq!(Order::decode(&bytes), Err(FrameError::Malformed), "{what}");
    }
    let mut empty_user = sign_in.clone();
    // The user's length, then the user: an empty one with its bytes gone.
    empty_user[1..5].copy_from_slice(&0u32.to_le_bytes());
    empty_user.drain(5..5 + "alice".len());
    assert_eq!(Order::decode(&empty_user), Err(FrameError::Malformed));
    let mut nanos = encoded_order(&Order::Renew(credential("bob")));
    let nanos_at = nanos.len() - 4;
    nanos[nanos_at..].copy_from_slice(&1_000_000_000_u32.to_le_bytes());
    assert_eq!(Order::decode(&nanos), Err(FrameError::Malformed), "nanos");

    let failed = encoded_report(&Report::Failed(SmbError::Protocol));
    let refused = encoded_report(&Report::Refused {
        request: 1,
        error: SmbError::Refused(Refusal::Ntlm),
    });
    let connected = encoded_report(&Report::Connected {
        request: 1,
        tree: 2,
        kind: ShareType::Disk,
    });
    let share = encoded_report(&Report::Share {
        request: 1,
        name: "share",
        special: false,
        remark: "",
    });
    for (what, bytes) in [
        ("an unknown report", changed(&failed, 0, 0)),
        ("an unknown error", changed(&failed, 1, 12)),
        (
            "an error with a detail it has none of",
            changed(&failed, 2, 1),
        ),
        ("an unknown refusal", changed(&refused, 6, 10)),
        ("an unknown share type", changed(&connected, 9, 4)),
        ("a special flag that is neither", changed(&share, 14, 2)),
        (
            "a share name no tree connect takes",
            changed(&share, 9, b'/'),
        ),
    ] {
        assert_eq!(Report::decode(&bytes), Err(FrameError::Malformed), "{what}");
    }
}

#[test]
fn the_largest_frames_fit_their_bounds_exactly() {
    let user = "u".repeat(ACCOUNT_NAME_MAX);
    let sign_in = encoded_order(&Order::SignIn {
        credential: Credential {
            domain: &user,
            ..credential(&user)
        },
        nonce_seed: [0; NONCE_SEED_LEN],
        previous: Some(1),
    });
    let chunk = vec![0x42; CHUNK_MAX];
    let received = encoded_order(&Order::Received(&chunk));
    let deep = "n\\".repeat(FS_PATH_MAX / 2 - 1) + "nn";
    assert_eq!(deep.len(), FS_PATH_MAX);
    let rename = encoded_order(&Order::File {
        request: 1,
        tree: 2,
        op: FileOp::Rename {
            from: &deep,
            to: &deep,
            replace: true,
            lease: Some([0; 16]),
        },
    });
    assert_eq!(
        sign_in.len().max(received.len()).max(rename.len()),
        ORDER_MAX
    );

    let name = "\u{1f600}".repeat(SHARE_NAME_MAX);
    let remark = "r".repeat(REMARK_MAX);
    let share = encoded_report(&Report::Share {
        request: 1,
        name: &name,
        special: true,
        remark: &remark,
    });
    let send = encoded_report(&Report::Send(&chunk));
    let data = encoded_report(&Report::Data {
        request: 1,
        chunk: &chunk,
    });
    assert!(share.len() < REPORT_MAX && send.len() < REPORT_MAX);
    assert_eq!(data.len(), REPORT_MAX);
    assert_eq!(ORDER_QUEUE, PIPELINE * (4 + ORDER_MAX));
    assert_eq!(REPORT_QUEUE, PIPELINE * (4 + REPORT_MAX));
}

#[test]
fn a_credential_s_key_is_never_shown() {
    let shown = format!("{:?}", credential("alice"));
    assert!(shown.contains("alice"));
    let key = String::from_utf8(vec![0x5a; KEY_LEN]).expect("ASCII");
    assert!(!shown.contains(&key), "{shown}");
    assert!(!shown.contains("90"), "{shown}");
}
