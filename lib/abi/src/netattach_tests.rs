//! Unit tests for the network-attachment records: each round-trips, the
//! largest fits its bound, and every frame out of shape is refused.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    AttachReply, FsserveCtl, NetworkAttach, NetworkOffer, OfferGrant, OfferId, Presentation,
    ATTACH_REPLY_LEN, NETWORK_ATTACH_MAX, NETWORK_OFFER_MAX, OFFER_GRANT_LEN,
};
use crate::driver::filesystem::{NameMatching, NodeId, NodeIdentity};
use crate::fsserve::{Lanes, ServeFrame, ServeGrant, ServeOffer};
use crate::sysinfo::{MOUNT_SOURCE_MAX, MOUNT_TARGET_MAX};
use crate::volume::{FormatName, VOLUME_NAME_MAX};
use crate::{DriverError, Errno};

fn serve(format: FormatName) -> ServeOffer {
    ServeOffer {
        root: NodeId::from_raw(1),
        name_matching: NameMatching::AsciiCaseInsensitive,
        node_identity: NodeIdentity::Stable,
        attrs: false,
        write_refusal: None,
        format,
    }
}

fn lanes() -> Lanes {
    Lanes::new(8, 64 * 1024).unwrap()
}

fn offer(source: &str) -> NetworkOffer<'_> {
    NetworkOffer {
        serve: serve(FormatName::Smb3),
        for_uid: 1000,
        key: *b"tairix share key",
        lanes: lanes(),
        source,
    }
}

fn encoded_offer(offer: &NetworkOffer<'_>) -> Vec<u8> {
    let mut out = vec![0; NETWORK_OFFER_MAX];
    let len = offer.encode(&mut out).expect("encodes");
    out.truncate(len);
    out
}

#[test]
fn an_offer_round_trips_and_the_largest_fits_its_bound() {
    let small = offer("//nas/share");
    assert_eq!(NetworkOffer::decode(&encoded_offer(&small)), Ok(small));
    let longest = "s".repeat(MOUNT_SOURCE_MAX);
    let large = NetworkOffer {
        lanes: Lanes::ONE,
        ..offer(&longest)
    };
    let bytes = encoded_offer(&large);
    assert_eq!(bytes.len(), NETWORK_OFFER_MAX);
    assert_eq!(NetworkOffer::decode(&bytes), Ok(large));
}

#[test]
fn an_offer_out_of_shape_is_never_encoded() {
    let long = "s".repeat(MOUNT_SOURCE_MAX + 1);
    for (what, bad) in [
        (
            "a local format",
            NetworkOffer {
                serve: serve(FormatName::Ext4),
                ..offer("//nas/share")
            },
        ),
        ("an empty source", offer("")),
        ("a control character", offer("//nas/sh\nare")),
        ("a source past its bound", offer(&long)),
    ] {
        assert_eq!(
            bad.encode(&mut [0; NETWORK_OFFER_MAX]),
            Err(Errno::OutOfRange),
            "{what}"
        );
    }
    let mut short = [0xA5; 20];
    assert_eq!(
        offer("//nas/share").encode(&mut short),
        Err(Errno::BufferTooSmall)
    );
    assert_eq!(
        short, [0xA5; 20],
        "a refused encode leaves the buffer as it was"
    );
}

#[test]
fn an_offer_frame_out_of_shape_is_refused() {
    let good = encoded_offer(&offer("//nas/share"));
    let at = 16;
    let changed = |index: usize, byte: u8| {
        let mut bytes = good.clone();
        bytes[index] = byte;
        bytes
    };
    let mut refusal = good.clone();
    refusal[..16].copy_from_slice(&ServeFrame::Refuse(DriverError::DeviceFault).encode());
    let mut local = good.clone();
    local[..16].copy_from_slice(&ServeFrame::Serve(serve(FormatName::Fat32)).encode());
    let mut trailing = good.clone();
    trailing.push(0);
    let mut odd_span = good.clone();
    odd_span[at + 8..at + 12].copy_from_slice(&(48u32 * 1024).to_le_bytes());
    for (what, bytes, refused) in [
        ("a refusal frame", refusal, Errno::OutOfRange),
        ("a local format", local, Errno::OutOfRange),
        ("no lanes", changed(at + 4, 0), Errno::OutOfRange),
        ("too many lanes", changed(at + 4, 17), Errno::OutOfRange),
        ("a span not a power of two", odd_span, Errno::OutOfRange),
        ("a pad", changed(at + 5, 1), Errno::OutOfRange),
        ("another pad", changed(at + 7, 1), Errno::OutOfRange),
        (
            "a source control character",
            changed(good.len() - 1, 0x07),
            Errno::OutOfRange,
        ),
        ("a byte past the source", trailing, Errno::LengthOutOfRange),
        (
            "a short frame",
            good[..good.len() - 1].to_vec(),
            Errno::LengthOutOfRange,
        ),
    ] {
        assert_eq!(NetworkOffer::decode(&bytes), Err(refused), "{what}");
    }
}

#[test]
fn a_grant_round_trips_for_the_lanes_it_was_made_for() {
    let grant = OfferGrant {
        grant: ServeGrant {
            endpoint: 0x4653_0000_0000_0007,
            window: 0x7000_0000,
            window_len: u32::try_from(lanes().window_len()).unwrap(),
        },
        offer: OfferId::new(3).unwrap(),
    };
    let bytes = grant.encode();
    assert_eq!(bytes.len(), OFFER_GRANT_LEN);
    assert_eq!(OfferGrant::decode(&bytes, lanes()), Ok(grant));
    assert_eq!(
        OfferGrant::decode(&bytes, Lanes::ONE),
        Err(Errno::LengthOutOfRange)
    );
    let mut none = bytes;
    none[24..].copy_from_slice(&[0; 8]);
    assert_eq!(OfferGrant::decode(&none, lanes()), Err(Errno::OutOfRange));
    assert_eq!(
        OfferGrant::decode(&bytes[1..], lanes()),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_server_asks_only_what_it_may() {
    assert_eq!(FsserveCtl::from_u32(1), Ok(FsserveCtl::Alive));
    assert_eq!(FsserveCtl::from_u32(2), Ok(FsserveCtl::Withdraw));
    for raw in [0, 3, u32::MAX] {
        assert_eq!(FsserveCtl::from_u32(raw), Err(Errno::OutOfRange));
    }
    assert_eq!(OfferId::new(0), None);
    assert_eq!(OfferId::new(9).map(OfferId::get), Some(9));
}

fn attach(target: Option<&str>) -> NetworkAttach<'_> {
    NetworkAttach {
        name: "share",
        presentation: Presentation::Private,
        read_only: false,
        target,
    }
}

fn encoded_attach(attach: &NetworkAttach<'_>) -> Vec<u8> {
    let mut out = vec![0; NETWORK_ATTACH_MAX];
    let len = attach.encode(&mut out).expect("encodes");
    out.truncate(len);
    out
}

#[test]
fn every_attach_round_trips_and_the_largest_fits_its_bound() {
    let explicit = Presentation::Explicit {
        uid: 1000,
        gid: 100,
        dir_mode: 0o750,
        file_mode: 0o640,
    };
    for attach in [
        attach(None),
        NetworkAttach {
            presentation: Presentation::Shared,
            read_only: true,
            ..attach(Some("/Users/ann/Home/nas"))
        },
        NetworkAttach {
            presentation: explicit,
            ..attach(None)
        },
    ] {
        assert_eq!(NetworkAttach::decode(&encoded_attach(&attach)), Ok(attach));
    }
    let name = "n".repeat(VOLUME_NAME_MAX);
    let target = String::from("/") + &"t".repeat(MOUNT_TARGET_MAX - 1);
    let largest = NetworkAttach {
        name: &name,
        presentation: explicit,
        read_only: true,
        target: Some(&target),
    };
    let bytes = encoded_attach(&largest);
    assert_eq!(bytes.len(), NETWORK_ATTACH_MAX);
    assert_eq!(NetworkAttach::decode(&bytes), Ok(largest));
}

#[test]
fn an_attach_out_of_shape_is_never_encoded() {
    let long_target = String::from("/") + &"t".repeat(MOUNT_TARGET_MAX);
    for (what, bad) in [
        (
            "a name the catalog refuses",
            NetworkAttach {
                name: "-share",
                ..attach(None)
            },
        ),
        (
            "a mode past the permission bits",
            NetworkAttach {
                presentation: Presentation::Explicit {
                    uid: 0,
                    gid: 0,
                    dir_mode: 0o2775,
                    file_mode: 0o644,
                },
                ..attach(None)
            },
        ),
        ("a relative target", attach(Some("Users/ann"))),
        ("a target past its bound", attach(Some(&long_target))),
        ("a control character", attach(Some("/Users/a\nn"))),
    ] {
        assert_eq!(
            bad.encode(&mut [0; NETWORK_ATTACH_MAX]),
            Err(Errno::OutOfRange),
            "{what}"
        );
    }
    assert_eq!(attach(None).encode(&mut [0; 8]), Err(Errno::BufferTooSmall));
}

#[test]
fn an_attach_frame_out_of_shape_is_refused() {
    let good = encoded_attach(&attach(Some("/Storage/x")));
    let changed = |index: usize, byte: u8| {
        let mut bytes = good.clone();
        bytes[index] = byte;
        bytes
    };
    let mut trailing = good.clone();
    trailing.push(b'x');
    for (what, bytes, refused) in [
        (
            "another version",
            changed(0, 9),
            Errno::AbiVersionUnsupported,
        ),
        ("an unknown presentation", changed(2, 4), Errno::OutOfRange),
        (
            "an owner on a private one",
            changed(4, 1),
            Errno::OutOfRange,
        ),
        ("a mode on a private one", changed(12, 1), Errno::OutOfRange),
        ("an unknown flag", changed(3, 2), Errno::OutOfRange),
        ("a pad", changed(17, 1), Errno::OutOfRange),
        ("a relative target", changed(25, b'x'), Errno::OutOfRange),
        ("a byte past the target", trailing, Errno::LengthOutOfRange),
        (
            "a short frame",
            good[..good.len() - 1].to_vec(),
            Errno::LengthOutOfRange,
        ),
    ] {
        assert_eq!(NetworkAttach::decode(&bytes), Err(refused), "{what}");
    }
}

#[test]
fn an_attach_reply_round_trips_and_refuses_what_it_could_not_carry() {
    let reply = AttachReply::new(*b"0123456789abcdef", "share-2").unwrap();
    assert_eq!(reply.name(), "share-2");
    let bytes = reply.encode();
    assert_eq!(bytes.len(), ATTACH_REPLY_LEN);
    assert_eq!(AttachReply::decode(&bytes), Ok(reply));
    assert_eq!(AttachReply::new([0; 16], "-bad"), Err(Errno::OutOfRange));
    let changed = |index: usize, byte: u8| {
        let mut bytes = bytes;
        bytes[index] = byte;
        bytes
    };
    for (what, bad) in [
        ("a pad", changed(17, 1)),
        ("a byte past the name", changed(ATTACH_REPLY_LEN - 1, b'x')),
        ("a name too long", changed(16, 33)),
    ] {
        assert_eq!(AttachReply::decode(&bad), Err(Errno::OutOfRange), "{what}");
    }
    assert_eq!(
        AttachReply::decode(&bytes[1..]),
        Err(Errno::LengthOutOfRange)
    );
}
