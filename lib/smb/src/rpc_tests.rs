//! Unit tests for the DCE/RPC PDUs, the `NetrShareEnum` stubs, and the
//! listing's checks on what a server answers.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    read_share_enum, read_share_enum_request, write_share_enum, write_share_enum_request, Bind,
    BindAck, Pdu, RequestFragment, ResponseFragment, ShareKind, ShareList, ShareListing,
    ShareRecord, Syntax, ACCEPTANCE, MAX_FRAGMENT, NDR, SHARE_ENUM, SPECIAL, SRVSVC,
};
use crate::file::create::{CreateAction, CreateResponse, OplockLevel};
use crate::file::io::{CloseResponse, ReadResponse, WriteResponse};
use crate::file::{FileAttributes, FileTimes};
use crate::header::{Address, Command, Flags, Header};
use crate::ioctl::IoctlResponse;
use crate::status::NtStatus;
use crate::vectors;
use crate::wire::Encoder;
use crate::SmbError;
use tairix_abi::netshare_ipc::{REMARK_MAX, SHARES_MAX};

const PIPE_ID: [u8; 16] = [0x42; 16];

fn pdu(value: &Pdu<'_>) -> Vec<u8> {
    let mut enc = Encoder::new();
    value.encode(&mut enc);
    enc.finish().expect("encodes")
}

fn stub(encode: impl FnOnce(&mut Encoder)) -> Vec<u8> {
    let mut enc = Encoder::new();
    encode(&mut enc);
    enc.finish().expect("encodes")
}

/// MS-SRVS §2.2.2.4's flags for a share not kept across restarts, and for one
/// a cluster serves: neither changes what kind of share it is.
const STYPE_TEMPORARY: u32 = 0x4000_0000;
const STYPE_CLUSTER_FS: u32 = 0x0200_0000;

fn records<'a>(names: &[&'a str]) -> Vec<ShareRecord<'a>> {
    names
        .iter()
        .map(|name| ShareRecord {
            name,
            share_type: ShareKind::Disk.code(),
            remark: "",
        })
        .collect()
}

/// The names of `list`'s shares, in order.
fn names(list: &ShareList) -> Vec<&str> {
    list.shares
        .iter()
        .map(|share| share.name.as_str())
        .collect()
}

fn ack() -> BindAck<'static> {
    BindAck {
        call_id: 1,
        max_xmit_frag: 4280,
        max_recv_frag: 4280,
        assoc_group: 0x0001_2345,
        secondary_address: b"\\PIPE\\srvsvc\0",
        result: ACCEPTANCE,
        reason: 0,
        transfer_syntax: NDR,
    }
}

#[test]
fn the_srvsvc_bind_is_the_one_every_client_sends() {
    let bind = Pdu::Bind(Bind {
        call_id: 1,
        max_xmit_frag: 4280,
        max_recv_frag: 4280,
        context: 0,
        abstract_syntax: SRVSVC,
        transfer_syntax: NDR,
    });
    let bytes = pdu(&bind);
    assert_eq!(
        bytes,
        vectors::bytes(concat!(
            "05000b031000000048000000010000",
            "00b810b81000000000010000000000",
            "0100c84f324b7016d3011278",
            "5a47bf6ee18803000000045d888aeb1c",
            "c9119fe808002b10486002000000",
        ))
    );
    assert_eq!(Pdu::decode(&bytes), Ok(bind));
}

#[test]
fn a_bind_ack_pads_its_address_to_four_bytes() {
    let bytes = pdu(&Pdu::BindAck(ack()));
    assert_eq!(bytes.len(), 68);
    assert_eq!(bytes[26..39], *b"\\PIPE\\srvsvc\0");
    assert_eq!(bytes[39], 0, "padding");
    assert_eq!(bytes[40], 1, "one result");
    assert_eq!(Pdu::decode(&bytes), Ok(Pdu::BindAck(ack())));
}

#[test]
fn every_pdu_round_trips() {
    let stub = [1, 2, 3, 4, 5];
    for value in [
        Pdu::BindNak {
            call_id: 1,
            reason: 4,
        },
        Pdu::Request(RequestFragment {
            call_id: 2,
            first: true,
            last: false,
            context: 0,
            opnum: SHARE_ENUM,
            stub: &stub,
        }),
        Pdu::Response(ResponseFragment {
            call_id: 2,
            first: false,
            last: true,
            context: 0,
            stub: &stub,
        }),
        Pdu::Fault {
            call_id: 2,
            status: 0x1c01_0002,
        },
    ] {
        let bytes = pdu(&value);
        assert_eq!(Pdu::decode(&bytes), Ok(value), "{value:?}");
    }
}

#[test]
fn a_pdu_out_of_shape_is_refused() {
    let good = pdu(&Pdu::BindAck(ack()));
    let mut cases: Vec<(&str, Vec<u8>, SmbError)> = Vec::new();
    let mut version = good.clone();
    version[0] = 4;
    cases.push(("a version", version, SmbError::Malformed));
    let mut big_endian = good.clone();
    big_endian[4] = 0x00;
    cases.push(("big-endian", big_endian, SmbError::Malformed));
    let mut long = good.clone();
    long[8] += 1;
    cases.push(("a fragment length past the PDU", long, SmbError::Malformed));
    let mut authenticated = good.clone();
    authenticated[10] = 8;
    cases.push(("authentication", authenticated, SmbError::Protocol));
    let mut unknown = good.clone();
    unknown[2] = 14;
    cases.push(("an alter-context", unknown, SmbError::Malformed));
    let mut trailing = good.clone();
    trailing.extend_from_slice(&[0; 4]);
    trailing[8] += 4;
    cases.push(("bytes past the result", trailing, SmbError::Malformed));
    let mut results = good.clone();
    results[40] = 2;
    cases.push(("two results", results, SmbError::Malformed));
    let mut object = pdu(&Pdu::Request(RequestFragment {
        call_id: 2,
        first: true,
        last: true,
        context: 0,
        opnum: SHARE_ENUM,
        stub: &[0; 16],
    }));
    object[3] |= 0x80;
    cases.push(("an object UUID", object, SmbError::Malformed));
    for (what, bytes, refused) in cases {
        assert_eq!(Pdu::decode(&bytes), Err(refused), "{what}");
    }
}

#[test]
fn the_share_enum_request_asks_for_every_share_at_level_one() {
    let bytes = stub(|enc| write_share_enum_request(enc, r"\\nas"));
    assert_eq!(
        bytes,
        vectors::bytes(concat!(
            "00000200",
            "06000000",
            "00000000",
            "06000000",
            "5c005c006e00610073000000",
            "01000000",
            "01000000",
            "04000200",
            "00000000",
            "00000000",
            "ffffffff",
            "00000000",
        ))
    );
    assert_eq!(read_share_enum_request(&bytes), Ok(String::from(r"\\nas")));
    let mut level = bytes.clone();
    level[28] = 2;
    assert_eq!(read_share_enum_request(&level), Err(SmbError::Malformed));
}

#[test]
fn a_listing_keeps_the_disk_shares_and_the_special_ones_only_when_asked() {
    let listed = [
        ShareRecord {
            name: "share",
            share_type: ShareKind::Disk.code(),
            remark: "The test share",
        },
        ShareRecord {
            name: "IPC$",
            share_type: ShareKind::Ipc.code() | SPECIAL,
            remark: "",
        },
        ShareRecord {
            name: "printer",
            share_type: ShareKind::PrintQueue.code() | STYPE_TEMPORARY,
            remark: "Second floor",
        },
        ShareRecord {
            name: "clustered",
            share_type: ShareKind::Disk.code() | STYPE_CLUSTER_FS,
            remark: "A clustered share",
        },
        ShareRecord {
            name: "odd",
            share_type: STYPE_CLUSTER_FS | 7,
            remark: "",
        },
        ShareRecord {
            name: "ADMIN$",
            share_type: ShareKind::Disk.code() | SPECIAL,
            remark: "Remote Admin",
        },
        ShareRecord {
            name: "scratch",
            share_type: ShareKind::Disk.code() | STYPE_TEMPORARY,
            remark: "",
        },
    ];
    let bytes = stub(|enc| write_share_enum(enc, &listed, 0));
    let plain = read_share_enum(&bytes, false).expect("a listing");
    assert_eq!(names(&plain), ["share", "clustered", "scratch"]);
    assert_eq!((plain.hidden, plain.unnamed, plain.unlisted), (1, 0, 0));
    assert_eq!(plain.shares[0].remark, "The test share");
    assert!(plain.shares.iter().all(|share| !share.special));

    let special = read_share_enum(&bytes, true).expect("a listing");
    assert_eq!(names(&special), ["share", "clustered", "ADMIN$", "scratch"]);
    assert_eq!(
        (special.hidden, special.unnamed, special.unlisted),
        (0, 0, 0)
    );
    assert_eq!(
        special
            .shares
            .iter()
            .map(|share| (share.special, share.remark.as_str()))
            .nth(2),
        Some((true, "Remote Admin"))
    );
    assert_eq!(
        read_share_enum(&stub(|enc| write_share_enum(enc, &[], 0)), true),
        Ok(ShareList::default())
    );
}

#[test]
fn a_share_no_tree_connect_could_name_is_left_out_and_counted() {
    let long = "x".repeat(81);
    let wide = "\u{1f600}".repeat(81);
    let listed = [
        r"a\b",
        "a/b",
        "nul\0in",
        long.as_str(),
        wide.as_str(),
        "kept",
    ];
    let mut bytes = stub(|enc| write_share_enum(enc, &records(&listed), 0));
    let list = read_share_enum(&bytes, false).expect("a listing");
    assert_eq!(list.unnamed, 5);
    assert_eq!(names(&list), ["kept"]);

    // A lone surrogate in the last name's first unit.
    let kept_at = bytes
        .windows(8)
        .rposition(|window| window == [b'k', 0, b'e', 0, b'p', 0, b't', 0])
        .expect("the name");
    bytes[kept_at..kept_at + 2].copy_from_slice(&0xdc00_u16.to_le_bytes());
    let list = read_share_enum(&bytes, false).expect("a listing");
    assert_eq!((list.shares.len(), list.unnamed), (0, 6));
}

#[test]
fn a_remark_that_does_not_decode_keeps_its_share() {
    let mut bytes = stub(|enc| {
        write_share_enum(
            enc,
            &[ShareRecord {
                name: "share",
                share_type: 0,
                remark: "ab",
            }],
            0,
        );
    });
    // The remark's first unit, after the name's string.
    let remark_at = bytes
        .windows(6)
        .rposition(|window| window == [b'a', 0, b'b', 0, 0, 0])
        .expect("the remark");
    bytes[remark_at..remark_at + 2].copy_from_slice(&0xd800_u16.to_le_bytes());
    let list = read_share_enum(&bytes, false).expect("a listing");
    assert_eq!(list.shares[0].remark, "\u{fffd}b");
}

#[test]
fn a_long_remark_is_cut_at_a_character_within_its_bound() {
    let fits = "€".repeat(REMARK_MAX / 3) + "a";
    let past = "€".repeat(REMARK_MAX / 3 + 1);
    let paired = "a".repeat(REMARK_MAX - 2) + "\u{1f600}";
    for (remark, kept) in [
        (fits.as_str(), fits.as_str()),
        (past.as_str(), &past[..REMARK_MAX - 1]),
        (paired.as_str(), &paired[..REMARK_MAX - 2]),
    ] {
        let bytes = stub(|enc| {
            write_share_enum(
                enc,
                &[ShareRecord {
                    name: "share",
                    share_type: 0,
                    remark,
                }],
                0,
            );
        });
        let list = read_share_enum(&bytes, false).expect("a listing");
        assert_eq!(list.shares[0].remark, kept);
        assert!(list.shares[0].remark.len() <= REMARK_MAX);
    }
}

#[test]
fn a_failed_call_is_its_status() {
    let bytes = stub(|enc| write_share_enum(enc, &records(&["share"]), 5));
    assert_eq!(read_share_enum(&bytes, false), Err(SmbError::Rpc(5)));
}

#[test]
fn a_listing_out_of_shape_is_refused() {
    let good = stub(|enc| write_share_enum(enc, &records(&["share"]), 0));
    let mut level = good.clone();
    level[0] = 2;
    let mut conformance = good.clone();
    conformance[20] = 2;
    let mut offset = good.clone();
    offset[40] = 1;
    let mut unterminated = good.clone();
    let terminator = 48 + 10;
    unterminated[terminator] = b'!';
    let mut trailing = good.clone();
    trailing.extend_from_slice(&[0; 4]);
    for (what, bytes) in [
        ("another level", level),
        ("a conformance other than the count", conformance),
        ("a string with an offset", offset),
        ("a string without its terminator", unterminated),
        ("bytes after the status", trailing),
    ] {
        assert_eq!(
            read_share_enum(&bytes, false),
            Err(SmbError::Malformed),
            "{what}"
        );
    }
    let mut overcounted = good.clone();
    // The count and the array's conformance, claiming more shares than the
    // stub holds.
    overcounted[12..16].copy_from_slice(&1000_u32.to_le_bytes());
    overcounted[20..24].copy_from_slice(&1000_u32.to_le_bytes());
    assert_eq!(
        read_share_enum(&overcounted, false),
        Err(SmbError::Malformed)
    );
}

#[test]
fn past_the_first_shares_the_rest_are_counted_not_listed() {
    let numbered: Vec<String> = (0..SHARES_MAX + 3)
        .map(|index| format!("s{index}"))
        .collect();
    let mut listed: Vec<ShareRecord<'_>> = (0..10)
        .map(|_| ShareRecord {
            name: "printer",
            share_type: ShareKind::PrintQueue.code(),
            remark: "",
        })
        .collect();
    listed.extend(records(
        &numbered.iter().map(String::as_str).collect::<Vec<_>>(),
    ));
    listed.extend(records(&["a/b"]));
    listed.push(ShareRecord {
        name: "D$",
        share_type: ShareKind::Disk.code() | SPECIAL,
        remark: "",
    });
    let bytes = stub(|enc| write_share_enum(enc, &listed, 0));
    let list = read_share_enum(&bytes, false).expect("a listing");
    assert_eq!(list.shares.len(), SHARES_MAX, "the printers take no place");
    assert_eq!(names(&list)[..2], ["s0", "s1"]);
    assert_eq!(list.shares[SHARES_MAX - 1].name.as_str(), "s255");
    assert_eq!((list.hidden, list.unnamed, list.unlisted), (1, 1, 3));
}

/// A response to the listing's request, behind a header of `command` with
/// `status`.
fn answer(command: Command, status: NtStatus, encode: impl FnOnce(&mut Encoder)) -> Vec<u8> {
    let mut enc = Encoder::new();
    Header {
        credit_charge: 1,
        status,
        command,
        credits: 1,
        flags: Flags::SERVER_TO_REDIR,
        next_command: 0,
        message_id: 1,
        address: Address::Sync { tree_id: 1 },
        session_id: 1,
        signature: [0; 16],
    }
    .encode(&mut enc);
    encode(&mut enc);
    enc.finish().expect("encodes")
}

fn opened() -> Vec<u8> {
    let epoch = tairix_abi::time::Time64::UNIX_EPOCH;
    let response = CreateResponse {
        oplock: OplockLevel::None,
        reparse_point: false,
        action: CreateAction::Opened,
        times: FileTimes {
            created: epoch,
            accessed: epoch,
            written: epoch,
            changed: epoch,
        },
        allocation_size: 0,
        end_of_file: 0,
        attributes: FileAttributes::NORMAL,
        file_id: PIPE_ID,
        lease: None,
        durable_timeout_ms: None,
        maximal_access: None,
        disk_id: None,
    };
    answer(Command::Create, NtStatus::SUCCESS, |enc| {
        response.encode(enc);
    })
}

/// A listing that has bound, and now sends its call.
fn bound() -> ShareListing {
    let mut listing = ShareListing::new("nas", false).expect("a listing");
    listing.answer(&opened());
    let bind_len = 72;
    listing.answer(&answer(Command::Write, NtStatus::SUCCESS, |enc| {
        WriteResponse { count: bind_len }.encode(enc);
    }));
    let ack = pdu(&Pdu::BindAck(ack()));
    listing.answer(&answer(Command::Read, NtStatus::SUCCESS, |enc| {
        ReadResponse { data: &ack }.encode(enc);
    }));
    assert_eq!(listing.command(), Some(Command::Ioctl));
    listing
}

fn transceived(output: &[u8], status: NtStatus) -> Vec<u8> {
    answer(Command::Ioctl, status, |enc| {
        IoctlResponse {
            ctl_code: super::FSCTL_PIPE_TRANSCEIVE,
            file_id: PIPE_ID,
            output: output.to_vec(),
        }
        .encode(enc);
    })
}

fn fragment(first: bool, last: bool, stub: &[u8]) -> Vec<u8> {
    pdu(&Pdu::Response(ResponseFragment {
        call_id: 2,
        first,
        last,
        context: 0,
        stub,
    }))
}

fn closed() -> Vec<u8> {
    answer(Command::Close, NtStatus::SUCCESS, |enc| {
        CloseResponse { attributes: None }.encode(enc);
    })
}

#[test]
fn a_listing_reads_an_answer_split_across_reads_and_fragments() {
    let mut listing = bound();
    let listed = stub(|enc| write_share_enum(enc, &records(&["share"]), 0));
    let (head, tail) = listed.split_at(10);
    let first = fragment(true, false, head);
    let (start, end) = first.split_at(7);
    listing.answer(&transceived(start, NtStatus::BUFFER_OVERFLOW));
    assert_eq!(listing.command(), Some(Command::Read));
    listing.answer(&answer(Command::Read, NtStatus::SUCCESS, |enc| {
        ReadResponse { data: end }.encode(enc);
    }));
    assert_eq!(listing.command(), Some(Command::Read));
    let last = fragment(false, true, tail);
    listing.answer(&answer(Command::Read, NtStatus::SUCCESS, |enc| {
        ReadResponse { data: &last }.encode(enc);
    }));
    assert_eq!(listing.command(), Some(Command::Close));
    listing.answer(&closed());
    assert_eq!(listing.command(), None);
    let shares = listing.finish().expect("listed").shares;
    assert_eq!(shares[0].name.as_str(), "share");
}

#[test]
fn a_listing_that_fails_still_closes_its_pipe() {
    let fault = pdu(&Pdu::Fault {
        call_id: 2,
        status: 0x1c01_0002,
    });
    let listed = stub(|enc| write_share_enum(enc, &records(&["share"]), 0));
    let oversized = vec![0; MAX_FRAGMENT + 1];
    let cases: Vec<(&str, Vec<u8>, SmbError)> = vec![
        (
            "a fault",
            transceived(&fault, NtStatus::SUCCESS),
            SmbError::Rpc(0x1c01_0002),
        ),
        (
            "another call's answer",
            transceived(
                &pdu(&Pdu::Response(ResponseFragment {
                    call_id: 9,
                    first: true,
                    last: true,
                    context: 0,
                    stub: &listed,
                })),
                NtStatus::SUCCESS,
            ),
            SmbError::Protocol,
        ),
        (
            "an answer that does not open as one",
            transceived(&fragment(false, true, &listed), NtStatus::SUCCESS),
            SmbError::Protocol,
        ),
        (
            "a fragment past the bound",
            transceived(&oversized, NtStatus::SUCCESS),
            SmbError::TooLarge,
        ),
        (
            "a failed transceive",
            transceived(&[], NtStatus::ACCESS_DENIED),
            SmbError::Status(NtStatus::ACCESS_DENIED),
        ),
    ];
    for (what, message, failure) in cases {
        let mut listing = bound();
        listing.answer(&message);
        assert_eq!(listing.command(), Some(Command::Close), "{what}");
        listing.answer(&closed());
        assert_eq!(listing.command(), None, "{what}");
        assert_eq!(listing.finish(), Err(failure), "{what}");
    }
}

#[test]
fn an_answer_past_its_bound_is_refused() {
    let mut listing = bound();
    let chunk = vec![0; 4096];
    listing.answer(&transceived(
        &fragment(true, false, &chunk),
        NtStatus::SUCCESS,
    ));
    let mut read = 1;
    for _ in 0..=super::MAX_STUB / chunk.len() {
        if listing.command() != Some(Command::Read) {
            break;
        }
        let next = fragment(false, false, &chunk);
        listing.answer(&answer(Command::Read, NtStatus::SUCCESS, |enc| {
            ReadResponse { data: &next }.encode(enc);
        }));
        read += 1;
    }
    assert_eq!(listing.command(), Some(Command::Close), "the bound ends it");
    assert_eq!(read * chunk.len(), super::MAX_STUB + chunk.len());
    listing.answer(&closed());
    assert_eq!(listing.finish(), Err(SmbError::TooLarge));
}

#[test]
fn a_bind_the_pipe_did_not_take_whole_ends_the_listing() {
    let mut listing = ShareListing::new("nas", false).expect("a listing");
    listing.answer(&opened());
    listing.answer(&answer(Command::Write, NtStatus::SUCCESS, |enc| {
        WriteResponse { count: 71 }.encode(enc);
    }));
    assert_eq!(listing.command(), Some(Command::Close));
    listing.answer(&closed());
    assert_eq!(listing.finish(), Err(SmbError::Protocol));
}

#[test]
fn a_refused_bind_ends_the_listing() {
    let mut listing = ShareListing::new("nas", false).expect("a listing");
    listing.answer(&opened());
    listing.answer(&answer(Command::Write, NtStatus::SUCCESS, |enc| {
        WriteResponse { count: 72 }.encode(enc);
    }));
    let nak = pdu(&Pdu::BindNak {
        call_id: 1,
        reason: 4,
    });
    listing.answer(&answer(Command::Read, NtStatus::SUCCESS, |enc| {
        ReadResponse { data: &nak }.encode(enc);
    }));
    listing.answer(&closed());
    assert_eq!(listing.finish(), Err(SmbError::Rpc(4)));

    let mut unopened = ShareListing::new("nas", false).expect("a listing");
    unopened.answer(&answer(Command::Create, NtStatus::ACCESS_DENIED, |enc| {
        enc.u16(9);
        enc.zeros(7);
    }));
    assert_eq!(unopened.command(), None, "nothing to close");
    assert_eq!(
        unopened.finish(),
        Err(SmbError::Status(NtStatus::ACCESS_DENIED))
    );
}

#[test]
fn a_bind_ack_must_accept_what_was_offered() {
    let other_syntax = Syntax {
        uuid: [0x71; 16],
        version: 1,
    };
    for (what, ack, failure) in [
        (
            "a rejection",
            BindAck {
                result: 2,
                reason: 1,
                ..ack()
            },
            SmbError::Rpc(1),
        ),
        (
            "a syntax never offered",
            BindAck {
                transfer_syntax: other_syntax,
                ..ack()
            },
            SmbError::Protocol,
        ),
        (
            "another call's",
            BindAck {
                call_id: 9,
                ..ack()
            },
            SmbError::Protocol,
        ),
        (
            "too small to take the call",
            BindAck {
                max_recv_frag: 16,
                ..ack()
            },
            SmbError::Protocol,
        ),
    ] {
        let mut listing = ShareListing::new("nas", false).expect("a listing");
        listing.answer(&opened());
        listing.answer(&answer(Command::Write, NtStatus::SUCCESS, |enc| {
            WriteResponse { count: 72 }.encode(enc);
        }));
        let answered = pdu(&Pdu::BindAck(ack));
        listing.answer(&answer(Command::Read, NtStatus::SUCCESS, |enc| {
            ReadResponse { data: &answered }.encode(enc);
        }));
        assert_eq!(listing.command(), Some(Command::Close), "{what}");
        listing.answer(&closed());
        assert_eq!(listing.finish(), Err(failure), "{what}");
    }
}

#[test]
fn the_syntaxes_are_spelled_as_dce_puts_them_on_the_wire() {
    assert_eq!(
        SRVSVC,
        Syntax {
            uuid: vectors::array("c84f324b7016d30112785a47bf6ee188"),
            version: 3,
        }
    );
    assert_eq!(
        NDR,
        Syntax {
            uuid: vectors::array("045d888aeb1cc9119fe808002b104860"),
            version: 2,
        }
    );
}
