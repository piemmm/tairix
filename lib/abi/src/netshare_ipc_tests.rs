//! Unit tests for `netshare-v1`: every request and reply round-trips, the
//! largest of each fits its bound, and every frame out of shape is refused.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::{
    is_host_name, is_share_name, Connect, Credentials, DialectFloor, Endpoint, MountEntry,
    MountState, NetshareError, NetshareReply, NetshareRequest, Op, Pin, Records, Rejection,
    ShareEntry, Shares, Shortfall, ACCOUNT_NAME_MAX, HOST_MAX, MOUNTS_MAX, NETSHARE_ENDPOINT,
    NETSHARE_MAX_REPLY, NETSHARE_MAX_REQUEST, REMARK_MAX, SHARES_MAX, SHARE_NAME_MAX, SMB_PORT,
};
use crate::account::MAX_PASSWORD_LEN;
use crate::fsserve::OfferId;
use crate::ipc::is_reserved_endpoint;
use crate::net_ipc::IF_NAME_LEN;
use crate::secret::Secret;
use crate::Errno;

const GUID: [u8; 16] = *b"tairix test peer";
const PASSWORD: &str = "correct horse battery staple";

fn interface(name: &str) -> [u8; IF_NAME_LEN] {
    let mut out = [0; IF_NAME_LEN];
    out[..name.len()].copy_from_slice(name.as_bytes());
    out
}

fn offer(raw: u64) -> OfferId {
    OfferId::new(raw).expect("a nonzero offer")
}

fn pin() -> Pin {
    Pin {
        server_guid: GUID,
        address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)),
    }
}

fn connect(pinned: bool) -> Connect<'static> {
    Connect {
        endpoint: Endpoint {
            host: "nas.example",
            address: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))),
            interface: None,
            port: SMB_PORT,
        },
        pin: pinned.then(pin),
        floor: DialectFloor::Smb300,
        credentials: Credentials {
            user: "alice",
            domain: "WORKGROUP",
            password: Secret::new(PASSWORD),
        },
    }
}

fn encoded(request: &NetshareRequest<'_>) -> Vec<u8> {
    let mut out = vec![0; NETSHARE_MAX_REQUEST];
    let len = request.encode(&mut out).expect("encodes");
    out.truncate(len);
    out
}

fn encoded_reply(reply: &NetshareReply<'_>) -> Vec<u8> {
    let mut out = vec![0; NETSHARE_MAX_REPLY];
    let len = reply.encode(&mut out).expect("encodes");
    out.truncate(len);
    out
}

fn refused(request: &NetshareRequest<'_>) -> Result<usize, Errno> {
    request.encode(&mut vec![0; NETSHARE_MAX_REQUEST])
}

#[test]
fn the_endpoint_is_reserved() {
    assert!(is_reserved_endpoint(NETSHARE_ENDPOINT));
}

#[test]
fn share_and_host_names_take_what_a_tree_path_can() {
    let longest_share = "s".repeat(SHARE_NAME_MAX);
    for name in ["share", "IPC$", "Média", longest_share.as_str()] {
        assert!(is_share_name(name), "{name}");
    }
    let too_long_share = "s".repeat(SHARE_NAME_MAX + 1);
    for name in ["", r"a\b", "a/b", "a\0b", "\u{85}", too_long_share.as_str()] {
        assert!(!is_share_name(name), "{name:?}");
    }
    let longest_host = "h".repeat(HOST_MAX);
    for host in ["nas.example", "192.0.2.7", "fe80::1", longest_host.as_str()] {
        assert!(is_host_name(host), "{host}");
    }
    let too_long_host = "h".repeat(HOST_MAX + 1);
    for host in ["", "a b", r"a\b", "a/b", "a\tb", too_long_host.as_str()] {
        assert!(!is_host_name(host), "{host:?}");
    }
}

#[test]
fn every_request_round_trips() {
    let link_local = Connect {
        endpoint: Endpoint {
            host: "office.local",
            address: Some(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 7))),
            interface: Some(interface("eth0")),
            port: 4450,
        },
        floor: DialectFloor::Smb311,
        ..connect(false)
    };
    let unresolved = Connect {
        endpoint: Endpoint {
            address: None,
            ..connect(false).endpoint
        },
        credentials: Credentials {
            domain: "",
            password: Secret::new(""),
            ..connect(false).credentials
        },
        ..connect(false)
    };
    for request in [
        NetshareRequest::Shares {
            connect: connect(true),
            special: true,
        },
        NetshareRequest::Shares {
            connect: unresolved,
            special: false,
        },
        NetshareRequest::Mount {
            connect: link_local,
            share: "Backups",
            read_only: true,
        },
        NetshareRequest::Mount {
            connect: connect(false),
            share: "share",
            read_only: false,
        },
        NetshareRequest::Unmount { offer: offer(7) },
        NetshareRequest::Mounts,
    ] {
        let bytes = encoded(&request);
        assert_eq!(NetshareRequest::decode(&bytes), Ok(request), "{request:?}");
    }
}

#[test]
fn the_largest_request_fits_its_bound_exactly() {
    let host = "h".repeat(HOST_MAX);
    let user = "u".repeat(ACCOUNT_NAME_MAX);
    let domain = "d".repeat(ACCOUNT_NAME_MAX);
    let password = "p".repeat(MAX_PASSWORD_LEN);
    let share = "\u{1f600}".repeat(SHARE_NAME_MAX);
    let request = NetshareRequest::Mount {
        connect: Connect {
            endpoint: Endpoint {
                host: &host,
                address: Some(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))),
                interface: Some(interface("eth0")),
                port: SMB_PORT,
            },
            pin: Some(pin()),
            floor: DialectFloor::Smb302,
            credentials: Credentials {
                user: &user,
                domain: &domain,
                password: Secret::new(&password),
            },
        },
        share: &share,
        read_only: true,
    };
    let bytes = encoded(&request);
    assert_eq!(bytes.len(), NETSHARE_MAX_REQUEST);
    assert_eq!(NetshareRequest::decode(&bytes), Ok(request));
}

#[test]
fn no_debug_shows_the_password() {
    let request = NetshareRequest::Mount {
        connect: connect(true),
        share: "share",
        read_only: false,
    };
    let shown = format!("{request:?}");
    assert!(!shown.contains(PASSWORD), "{shown}");
    assert!(shown.contains("alice"));
}

/// Assert that a listing request over each of `cases` is refused unencoded.
fn never_encoded(cases: &[(&str, Connect<'_>)]) {
    for (what, connect) in cases {
        let request = NetshareRequest::Shares {
            connect: *connect,
            special: false,
        };
        assert_eq!(refused(&request), Err(Errno::OutOfRange), "{what}");
    }
}

#[test]
fn an_endpoint_out_of_shape_is_never_encoded() {
    let base = connect(false);
    let at = |endpoint| Connect { endpoint, ..base };
    let mut cases: Vec<(&str, Connect<'_>)> = ["", "a b", r"nas\share"]
        .map(|host| {
            let endpoint = Endpoint {
                host,
                ..base.endpoint
            };
            ("a host out of shape", at(endpoint))
        })
        .to_vec();
    for address in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V4(Ipv4Addr::BROADCAST),
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb)),
    ] {
        let endpoint = Endpoint {
            address: Some(address),
            interface: Some(interface("eth0")),
            ..base.endpoint
        };
        cases.push(("an address naming no one node", at(endpoint)));
    }
    let link_local = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
    cases.extend([
        (
            "port zero",
            at(Endpoint {
                port: 0,
                ..base.endpoint
            }),
        ),
        (
            "a link-local address with no link",
            at(Endpoint {
                address: Some(link_local),
                ..base.endpoint
            }),
        ),
        (
            "an interface name out of shape",
            at(Endpoint {
                interface: Some([b' '; IF_NAME_LEN]),
                ..base.endpoint
            }),
        ),
    ]);
    never_encoded(&cases);
    let small = NetshareRequest::Shares {
        connect: base,
        special: false,
    };
    assert_eq!(small.encode(&mut [0; 16]), Err(Errno::BufferTooSmall));
}

#[test]
fn credentials_or_a_share_out_of_shape_are_never_encoded() {
    let base = connect(false);
    let long_name = "n".repeat(ACCOUNT_NAME_MAX + 1);
    let long_password = "p".repeat(MAX_PASSWORD_LEN + 1);
    let signing_in = |credentials| Connect {
        credentials,
        ..base
    };
    never_encoded(&[
        (
            "an anonymous user",
            signing_in(Credentials {
                user: "",
                ..base.credentials
            }),
        ),
        (
            "a control character",
            signing_in(Credentials {
                user: "al\nice",
                ..base.credentials
            }),
        ),
        (
            "a user past its bound",
            signing_in(Credentials {
                user: &long_name,
                ..base.credentials
            }),
        ),
        (
            "a domain past its bound",
            signing_in(Credentials {
                domain: &long_name,
                ..base.credentials
            }),
        ),
        (
            "a password past its bound",
            signing_in(Credentials {
                password: Secret::new(&long_password),
                ..base.credentials
            }),
        ),
    ]);
    for share in ["", r"a\b", "a\0"] {
        let request = NetshareRequest::Mount {
            connect: base,
            share,
            read_only: false,
        };
        assert_eq!(refused(&request), Err(Errno::OutOfRange), "{share:?}");
    }
}

/// The offset, in a [`connect`] Shares request's frame, of each field.
mod at {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 4;
    pub const OP: usize = 6;
    pub const FLAGS: usize = 7;
    /// The host is "nas.example": a two-byte length and eleven bytes.
    pub const FAMILY: usize = 8 + 2 + 11;
    pub const ADDRESS: usize = FAMILY + 1;
    pub const INTERFACE: usize = ADDRESS + 16;
    pub const PORT: usize = INTERFACE + 16;
    pub const PINNED: usize = PORT + 2;
    pub const PIN: usize = PINNED + 1;
    pub const FLOOR: usize = PIN + 33;
}

#[test]
fn a_frame_out_of_shape_is_refused() {
    let good = encoded(&NetshareRequest::Shares {
        connect: connect(false),
        special: false,
    });
    assert!(NetshareRequest::decode(&good).is_ok());
    let changed = |at: usize, byte: u8| {
        let mut bytes = good.clone();
        bytes[at] = byte;
        bytes
    };
    let mut trailing = good.clone();
    trailing.push(0);
    let mut oversized = good.clone();
    oversized.resize(NETSHARE_MAX_REQUEST + 1, 0);
    for (what, bytes, refusal) in [
        ("another magic", changed(at::MAGIC, b'X'), Errno::BadMagic),
        (
            "another version",
            changed(at::VERSION, 2),
            Errno::OutOfRange,
        ),
        (
            "an unknown operation",
            changed(at::OP, 9),
            Errno::OutOfRange,
        ),
        (
            "a flag Shares does not carry",
            changed(at::FLAGS, 0x02),
            Errno::OutOfRange,
        ),
        (
            "an unknown address family",
            changed(at::FAMILY, 5),
            Errno::OutOfRange,
        ),
        (
            "bytes past an IPv4 address",
            changed(at::ADDRESS + 4, 1),
            Errno::OutOfRange,
        ),
        (
            "the limited broadcast",
            {
                let mut bytes = good.clone();
                bytes[at::ADDRESS..at::ADDRESS + 4].copy_from_slice(&[0xff; 4]);
                bytes
            },
            Errno::OutOfRange,
        ),
        (
            "an interface out of shape",
            changed(at::INTERFACE, b' '),
            Errno::OutOfRange,
        ),
        (
            "port zero",
            {
                let mut bytes = changed(at::PORT, 0);
                bytes[at::PORT + 1] = 0;
                bytes
            },
            Errno::OutOfRange,
        ),
        (
            "an unknown pin marker",
            changed(at::PINNED, 2),
            Errno::OutOfRange,
        ),
        (
            "bytes in an absent pin",
            changed(at::PIN, 1),
            Errno::OutOfRange,
        ),
        ("an unknown floor", changed(at::FLOOR, 3), Errno::OutOfRange),
        ("a byte past the end", trailing, Errno::LengthOutOfRange),
        ("a frame past its bound", oversized, Errno::LengthOutOfRange),
        (
            "a short frame",
            good[..good.len() - 1].to_vec(),
            Errno::LengthOutOfRange,
        ),
    ] {
        assert_eq!(NetshareRequest::decode(&bytes), Err(refusal), "{what}");
    }

    let mounts = encoded(&NetshareRequest::Mounts);
    let mut flagged = mounts.clone();
    flagged[at::FLAGS] = 1;
    assert_eq!(NetshareRequest::decode(&flagged), Err(Errno::OutOfRange));
    let mut unmount = encoded(&NetshareRequest::Unmount { offer: offer(1) });
    unmount[8] = 0;
    assert_eq!(
        NetshareRequest::decode(&unmount),
        Err(Errno::OutOfRange),
        "offer zero names none"
    );
}

fn entries<'a>(names: &'a [String], remark: &'a str) -> Vec<ShareEntry<'a>> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| ShareEntry {
            name,
            special: index % 2 == 1,
            remark,
        })
        .collect()
}

/// Assert that `reply` decodes back to itself, within its operation's bound.
fn assert_round_trips(reply: NetshareReply<'_>) {
    let bytes = encoded_reply(&reply);
    assert_eq!(NetshareReply::decode(&bytes), Ok(reply), "{reply:?}");
    let op = match reply {
        NetshareReply::Shares(_) => Op::Shares,
        NetshareReply::Mounted { .. } => Op::Mount,
        NetshareReply::Unmounted | NetshareReply::Unreadable => Op::Unmount,
        NetshareReply::Mounts(_) => Op::Mounts,
        NetshareReply::Refused { op, .. } => op,
    };
    assert!(bytes.len() <= op.max_reply(), "{reply:?}");
}

#[test]
fn every_answer_round_trips() {
    let names = [String::from("share"), String::from("C$")];
    let listed = entries(&names, "The test share");
    let mounts = [
        (3, "nas.example", "share", false, MountState::Serving),
        (4, "office.local", "Backups", true, MountState::Reconnecting),
        (5, "nas.example", "Media", false, MountState::Lost),
    ]
    .map(|(raw, host, share, read_only, state)| MountEntry {
        offer: offer(raw),
        host,
        share,
        read_only,
        state,
    });
    for (hidden, shares) in [(2, &listed[..]), (0, &[])] {
        assert_round_trips(NetshareReply::Shares(Shares {
            pin: pin(),
            hidden,
            unnamed: 1,
            shares: Records::new(shares),
        }));
    }
    assert_round_trips(NetshareReply::Mounted {
        pin: pin(),
        offer: offer(9),
    });
    assert_round_trips(NetshareReply::Unmounted);
    assert_round_trips(NetshareReply::Mounts(Records::new(&mounts)));
    assert_round_trips(NetshareReply::Mounts(Records::new(&[])));
    assert_round_trips(NetshareReply::Unreadable);
}

#[test]
fn every_refusal_round_trips() {
    let mut errors = vec![
        NetshareError::UnknownHost,
        NetshareError::Unreachable,
        NetshareError::PinMismatch(pin()),
        NetshareError::Tampered,
        NetshareError::Protocol,
        NetshareError::TooLarge,
        NetshareError::NoSuchShare,
        NetshareError::Denied,
        NetshareError::NoSession,
        NetshareError::NoSuchMount,
        NetshareError::Limit,
        NetshareError::Failed,
    ];
    errors.extend(
        [
            Rejection::Credentials,
            Rejection::Disabled,
            Rejection::Locked,
            Rejection::Expired,
            Rejection::Restricted,
        ]
        .map(NetshareError::Rejected),
    );
    errors.extend(
        [
            Shortfall::Dialect,
            Shortfall::Encryption,
            Shortfall::Signing,
            Shortfall::Authentication,
            Shortfall::Guest,
        ]
        .map(NetshareError::Unsupported),
    );
    for error in errors {
        for op in [Op::Shares, Op::Mount, Op::Unmount, Op::Mounts] {
            assert_round_trips(NetshareReply::Refused { op, error });
        }
    }
}

#[test]
fn a_listing_reads_its_records_back_in_order() {
    let names = [String::from("a"), String::from("b"), String::from("c")];
    let listed = entries(&names, "");
    let reply = NetshareReply::Shares(Shares {
        pin: pin(),
        hidden: 0,
        unnamed: 0,
        shares: Records::new(&listed),
    });
    let bytes = encoded_reply(&reply);
    let Ok(NetshareReply::Shares(shares)) = NetshareReply::decode(&bytes) else {
        panic!("a listing");
    };
    assert_eq!(shares.shares.len(), 3);
    assert!(!shares.shares.is_empty());
    assert_eq!(
        shares
            .shares
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
}

#[test]
fn the_largest_reply_fits_its_bound_exactly() {
    let names = vec!["\u{1f600}".repeat(SHARE_NAME_MAX); SHARES_MAX];
    let remark = "r".repeat(REMARK_MAX);
    let listed = entries(&names, &remark);
    let reply = NetshareReply::Shares(Shares {
        pin: Pin {
            server_guid: GUID,
            address: IpAddr::V6(Ipv6Addr::LOCALHOST),
        },
        hidden: u32::MAX,
        unnamed: u32::MAX,
        shares: Records::new(&listed),
    });
    let bytes = encoded_reply(&reply);
    assert_eq!(bytes.len(), NETSHARE_MAX_REPLY);
    assert_eq!(bytes.len(), Op::Shares.max_reply());
    assert_eq!(NetshareReply::decode(&bytes), Ok(reply));

    let host = "h".repeat(HOST_MAX);
    let share = "\u{1f600}".repeat(SHARE_NAME_MAX);
    let mounts = [MountEntry {
        offer: offer(u64::MAX),
        host: &host,
        share: &share,
        read_only: true,
        state: MountState::Lost,
    }; MOUNTS_MAX];
    let reply = NetshareReply::Mounts(Records::new(&mounts));
    assert_eq!(encoded_reply(&reply).len(), Op::Mounts.max_reply());
    let mismatch = NetshareReply::Refused {
        op: Op::Unmount,
        error: NetshareError::PinMismatch(Pin {
            server_guid: GUID,
            address: IpAddr::V6(Ipv6Addr::LOCALHOST),
        }),
    };
    assert_eq!(encoded_reply(&mismatch).len(), Op::Unmount.max_reply());
    let mounted = NetshareReply::Mounted {
        pin: pin(),
        offer: offer(1),
    };
    assert_eq!(encoded_reply(&mounted).len(), Op::Mount.max_reply());
}

#[test]
fn a_listing_past_its_bound_is_refused_both_ways() {
    let names: Vec<String> = (0..=SHARES_MAX).map(|index| format!("s{index}")).collect();
    let listed = entries(&names, "");
    let too_many = NetshareReply::Shares(Shares {
        pin: pin(),
        hidden: 0,
        unnamed: 0,
        shares: Records::new(&listed),
    });
    assert_eq!(
        too_many.encode(&mut vec![0; 2 * NETSHARE_MAX_REPLY]),
        Err(Errno::OutOfRange)
    );
    let fitting = NetshareReply::Shares(Shares {
        pin: pin(),
        hidden: 0,
        unnamed: 0,
        shares: Records::new(&listed[..SHARES_MAX]),
    });
    let mut bytes = encoded_reply(&fitting);
    // The record count sits past the header, the pin and the two counters.
    let count_at = 9 + 33 + 8;
    bytes[count_at..count_at + 2]
        .copy_from_slice(&u16::try_from(SHARES_MAX + 1).expect("fits").to_le_bytes());
    assert_eq!(NetshareReply::decode(&bytes), Err(Errno::OutOfRange));

    let mounts = [MountEntry {
        offer: offer(1),
        host: "nas.example",
        share: "share",
        read_only: false,
        state: MountState::Serving,
    }; MOUNTS_MAX + 1];
    assert_eq!(
        NetshareReply::Mounts(Records::new(&mounts)).encode(&mut vec![0; NETSHARE_MAX_REPLY]),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_reply_out_of_shape_is_refused() {
    let names = [String::from("share")];
    let listed = entries(&names, "remark");
    let shares = encoded_reply(&NetshareReply::Shares(Shares {
        pin: pin(),
        hidden: 0,
        unnamed: 0,
        shares: Records::new(&listed),
    }));
    let refused = encoded_reply(&NetshareReply::Refused {
        op: Op::Shares,
        error: NetshareError::Rejected(Rejection::Locked),
    });
    let changed = |bytes: &[u8], at: usize, byte: u8| {
        let mut bytes = bytes.to_vec();
        bytes[at] = byte;
        bytes
    };
    // The first record: its name's length, the name, then the special byte.
    let name_at = 9 + 33 + 8 + 2;
    let special_at = name_at + 2 + 5;
    let mut trailing = refused.clone();
    trailing.push(0);
    let mut unreadable = encoded_reply(&NetshareReply::Unreadable);
    unreadable.push(0);
    for (what, bytes, refusal) in [
        (
            "an unreadable request's answer with more",
            unreadable,
            Errno::LengthOutOfRange,
        ),
        ("another magic", changed(&shares, 0, b'X'), Errno::BadMagic),
        (
            "an unknown operation",
            changed(&shares, 6, 9),
            Errno::OutOfRange,
        ),
        (
            "an answer with a detail",
            changed(&shares, 8, 1),
            Errno::OutOfRange,
        ),
        (
            "an unreadable request's answer with a refusal",
            changed(&refused, 6, 0),
            Errno::OutOfRange,
        ),
        (
            "an unknown outcome",
            changed(&refused, 7, 15),
            Errno::OutOfRange,
        ),
        (
            "a rejection with no reason",
            changed(&refused, 8, 0),
            Errno::OutOfRange,
        ),
        (
            "an unknown rejection",
            changed(&refused, 8, 9),
            Errno::OutOfRange,
        ),
        (
            "a share name out of shape",
            changed(&shares, name_at + 2, b'/'),
            Errno::OutOfRange,
        ),
        (
            "a special byte out of range",
            changed(&shares, special_at, 2),
            Errno::OutOfRange,
        ),
        ("a byte past the end", trailing, Errno::LengthOutOfRange),
        (
            "a short reply",
            shares[..shares.len() - 1].to_vec(),
            Errno::LengthOutOfRange,
        ),
    ] {
        assert_eq!(NetshareReply::decode(&bytes), Err(refusal), "{what}");
    }

    let mounts = encoded_reply(&NetshareReply::Mounts(Records::new(&[MountEntry {
        offer: offer(1),
        host: "nas.example",
        share: "share",
        read_only: false,
        state: MountState::Serving,
    }])));
    let state_at = mounts.len() - 1;
    assert_eq!(
        NetshareReply::decode(&changed(&mounts, state_at, 0)),
        Err(Errno::OutOfRange),
        "an unknown mount state"
    );
    let offer_at = 9 + 2;
    let mut no_offer = mounts.clone();
    no_offer[offer_at..offer_at + 8].copy_from_slice(&[0; 8]);
    assert_eq!(NetshareReply::decode(&no_offer), Err(Errno::OutOfRange));
}
