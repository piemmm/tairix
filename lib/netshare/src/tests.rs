//! Host tests for the client: each request crosses as the protocol encodes
//! it, each answer is accepted only for the request it answers, and every
//! failure is reported as what it is.

extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr};
use std::collections::BTreeSet;

use tairix_abi::netattach::OfferId;
use tairix_abi::netshare_ipc::{
    Connect, Credentials, DialectFloor, Endpoint, MountEntry, MountState, NetshareError,
    NetshareReply, NetshareRequest, Op, Pin, Records, Rejection, ShareEntry, Shares, Shortfall,
    NETSHARE_MAX_REPLY, SMB_PORT,
};
use tairix_abi::secret::Secret;
use tairix_abi::Errno;

use super::{Client, Error, Mounted, Transport};

/// A service that answers every call with one frame, or one refusal of the
/// call itself, and keeps what it was sent.
struct Service {
    answer: Result<Vec<u8>, Errno>,
    sent: Vec<u8>,
    room: usize,
    calls: usize,
}

impl Transport for Service {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        self.calls += 1;
        self.sent = request.to_vec();
        self.room = reply.len();
        let answer = self.answer.clone()?;
        reply
            .get_mut(..answer.len())
            .ok_or(Errno::BufferTooSmall)?
            .copy_from_slice(&answer);
        Ok(answer.len())
    }
}

fn answering(reply: &NetshareReply<'_>) -> Client<Service> {
    let mut bytes = vec![0; NETSHARE_MAX_REPLY];
    let len = reply.encode(&mut bytes).expect("encodes");
    bytes.truncate(len);
    sending(Ok(bytes))
}

fn sending(answer: Result<Vec<u8>, Errno>) -> Client<Service> {
    Client::new(Service {
        answer,
        sent: Vec::new(),
        room: 0,
        calls: 0,
    })
}

fn pin() -> Pin {
    Pin {
        server_guid: *b"tairix test peer",
        address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)),
    }
}

fn offer(raw: u64) -> OfferId {
    OfferId::new(raw).expect("nonzero")
}

fn connect() -> Connect<'static> {
    Connect {
        endpoint: Endpoint {
            host: "nas.example",
            address: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))),
            interface: None,
            port: SMB_PORT,
        },
        pin: Some(pin()),
        floor: DialectFloor::Smb300,
        credentials: Credentials {
            user: "alice",
            domain: "",
            password: Secret::new("hunter2"),
        },
    }
}

fn sent(client: &Client<Service>) -> NetshareRequest<'_> {
    NetshareRequest::decode(&client.transport.sent).expect("the request decodes")
}

#[test]
fn a_listing_answers_the_request_sent() {
    let listed = [
        ShareEntry {
            name: "share",
            special: false,
            remark: "The test share",
        },
        ShareEntry {
            name: "C$",
            special: true,
            remark: "",
        },
    ];
    let mut client = answering(&NetshareReply::Shares(Shares {
        pin: pin(),
        hidden: 0,
        unnamed: 1,
        unlisted: 4,
        shares: Records::new(&listed),
    }));
    let shares = client.shares(connect(), true).expect("a listing");
    let names: Vec<String> = shares
        .shares
        .iter()
        .map(|entry| entry.name.to_string())
        .collect();
    assert_eq!(
        (shares.pin, shares.hidden, shares.unnamed, shares.unlisted),
        (pin(), 0, 1, 4)
    );
    assert_eq!(names, ["share", "C$"]);
    assert_eq!(
        sent(&client),
        NetshareRequest::Shares {
            connect: connect(),
            special: true,
        }
    );
    assert_eq!(client.transport.room, Op::Shares.max_reply());
}

#[test]
fn a_mount_answers_its_offer() {
    let mut client = answering(&NetshareReply::Mounted {
        pin: pin(),
        offer: offer(9),
    });
    assert_eq!(
        client.mount(connect(), "share", true),
        Ok(Mounted {
            pin: pin(),
            offer: offer(9),
        })
    );
    assert_eq!(
        sent(&client),
        NetshareRequest::Mount {
            connect: connect(),
            share: "share",
            read_only: true,
        }
    );
    assert_eq!(client.transport.room, Op::Mount.max_reply());
}

#[test]
fn an_unmount_and_a_listing_of_mounts_answer_theirs() {
    let mut client = answering(&NetshareReply::Unmounted);
    assert_eq!(client.unmount(offer(3)), Ok(()));
    assert_eq!(sent(&client), NetshareRequest::Unmount { offer: offer(3) });

    let held = [MountEntry {
        offer: offer(3),
        host: "nas.example",
        share: "share",
        read_only: false,
        state: MountState::Reconnecting,
    }];
    let mut client = answering(&NetshareReply::Mounts(Records::new(&held)));
    let mounts = client.mounts().expect("the mounts");
    assert_eq!(mounts.iter().collect::<Vec<_>>(), held);
    assert_eq!(sent(&client), NetshareRequest::Mounts);
    assert_eq!(client.transport.room, Op::Mounts.max_reply());
}

#[test]
fn the_reply_buffer_is_kept_and_sized_to_each_request() {
    let mut client = answering(&NetshareReply::Unmounted);
    assert_eq!(client.unmount(offer(3)), Ok(()));
    assert_eq!(client.transport.room, Op::Unmount.max_reply());
    client.transport.answer = Err(Errno::TimedOut);
    assert!(client.shares(connect(), false).is_err());
    assert_eq!(client.transport.room, NETSHARE_MAX_REPLY);
    assert!(client.unmount(offer(3)).is_err());
    assert_eq!(client.transport.room, Op::Unmount.max_reply());
    assert!(client.reply.len() >= NETSHARE_MAX_REPLY);
}

#[test]
fn a_refusal_is_the_service_s_reason() {
    for error in [
        NetshareError::PinMismatch(pin()),
        NetshareError::Rejected(Rejection::Locked),
        NetshareError::Unsupported(Shortfall::Encryption),
        NetshareError::NoSuchShare,
        NetshareError::NoSession,
    ] {
        let mut client = answering(&NetshareReply::Refused {
            op: Op::Mount,
            error,
        });
        assert_eq!(
            client.mount(connect(), "share", false),
            Err(Error::Refused(error))
        );
    }
}

#[test]
fn an_answer_to_another_request_is_malformed() {
    let refused_listing = NetshareReply::Refused {
        op: Op::Shares,
        error: NetshareError::Denied,
    };
    for reply in [refused_listing, NetshareReply::Unmounted] {
        let mut client = answering(&reply);
        assert_eq!(
            client.mount(connect(), "share", false),
            Err(Error::Malformed),
            "{reply:?}"
        );
    }
    let refused_mount = NetshareReply::Refused {
        op: Op::Mount,
        error: NetshareError::NoSuchShare,
    };
    for reply in [refused_mount, NetshareReply::Mounts(Records::new(&[]))] {
        let mut client = answering(&reply);
        assert_eq!(client.unmount(offer(1)), Err(Error::Malformed), "{reply:?}");
    }
    let mut client = answering(&NetshareReply::Mounted {
        pin: pin(),
        offer: offer(1),
    });
    assert!(matches!(client.mounts(), Err(Error::Malformed)));
    assert!(matches!(
        client.shares(connect(), false),
        Err(Error::Malformed)
    ));
}

#[test]
fn a_reply_that_does_not_decode_is_malformed() {
    let mut client = sending(Ok(b"NSHA".to_vec()));
    assert_eq!(client.unmount(offer(1)), Err(Error::Malformed));
}

#[test]
fn an_unreadable_request_is_reported_as_one() {
    let mut client = answering(&NetshareReply::Unreadable);
    assert_eq!(client.unmount(offer(1)), Err(Error::Unreadable));
}

#[test]
fn a_failed_call_says_whether_any_service_is_there() {
    let mut client = sending(Err(Errno::NotFound));
    assert_eq!(client.unmount(offer(1)), Err(Error::Unavailable));
    let mut client = sending(Err(Errno::PermissionDenied));
    assert_eq!(client.unmount(offer(1)), Err(Error::NotPermitted));
    let mut client = sending(Err(Errno::TimedOut));
    assert_eq!(
        client.unmount(offer(1)),
        Err(Error::Transport(Errno::TimedOut))
    );
}

#[test]
fn a_request_the_protocol_refuses_is_never_sent() {
    let mut client = answering(&NetshareReply::Unmounted);
    assert_eq!(client.mount(connect(), "a/b", false), Err(Error::Invalid));
    let anonymous = Connect {
        credentials: Credentials {
            user: "",
            ..connect().credentials
        },
        ..connect()
    };
    assert!(matches!(
        client.shares(anonymous, false),
        Err(Error::Invalid)
    ));
    assert_eq!(client.transport.calls, 0);
}

#[test]
fn a_dialect_shortfall_names_no_dialect_of_its_own() {
    // The floor the caller asked for may be any, and a reason naming one
    // misstates the others.
    let reason = Error::Refused(NetshareError::Unsupported(Shortfall::Dialect)).to_string();
    assert!(!reason.contains("3."), "{reason}");
}

#[test]
fn every_failure_states_a_reason_of_its_own() {
    let mut refusals = vec![
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
        NetshareError::AlreadyMounted,
    ];
    refusals.extend(
        [
            Rejection::Credentials,
            Rejection::Disabled,
            Rejection::Locked,
            Rejection::Expired,
            Rejection::Restricted,
        ]
        .map(NetshareError::Rejected),
    );
    refusals.extend(
        [
            Shortfall::Dialect,
            Shortfall::Encryption,
            Shortfall::Signing,
            Shortfall::Authentication,
            Shortfall::Guest,
        ]
        .map(NetshareError::Unsupported),
    );
    let mut errors: Vec<Error> = refusals.into_iter().map(Error::Refused).collect();
    errors.extend([
        Error::Invalid,
        Error::Unavailable,
        Error::NotPermitted,
        Error::Unreadable,
        Error::Transport(Errno::TimedOut),
        Error::Malformed,
        Error::OutOfMemory,
    ]);
    let messages: BTreeSet<String> = errors.iter().map(|error| format!("{error}")).collect();
    assert_eq!(messages.len(), errors.len());
    assert!(messages.iter().all(|message| !message.is_empty()));
}
