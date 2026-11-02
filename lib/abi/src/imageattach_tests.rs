//! Unit tests for the image-attachment records: each round-trips, the largest
//! fits its bound, and every frame out of shape is refused.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    ImageAccess, ImageAttach, ImageAttachReply, ImageRefusal, IMAGE_ATTACH_MAX,
    IMAGE_ATTACH_REPLY_LEN, IMAGE_ATTACH_VERSION,
};
use crate::fs::FS_NAME_MAX;
use crate::netattach::{AttachReply, Presentation};
use crate::sysinfo::MOUNT_TARGET_MAX;
use crate::volume::{VolumeFsType, VolumeFsTypes, VOLUME_NAME_MAX};
use crate::Errno;

fn attach<'a>(name: Option<&'a str>, target: Option<&'a str>) -> ImageAttach<'a> {
    ImageAttach {
        name,
        presentation: Presentation::Private,
        access: ImageAccess::WritableWherePossible,
        target,
        expects: None,
        adopt: false,
        file_name: None,
    }
}

/// The set holding only `fstype`.
fn only(fstype: VolumeFsType) -> Option<VolumeFsTypes> {
    VolumeFsTypes::of([fstype])
}

fn encoded(attach: &ImageAttach<'_>) -> Vec<u8> {
    let mut out = vec![0; IMAGE_ATTACH_MAX];
    let len = attach.encode(&mut out).expect("encodes");
    out.truncate(len);
    out
}

#[test]
fn every_attach_round_trips_and_the_largest_fits_its_bound() {
    let name = "a".repeat(VOLUME_NAME_MAX);
    let leaf = "f".repeat(FS_NAME_MAX);
    let target = {
        let mut target = String::from("/Users/a/");
        target.push_str(&"t".repeat(MOUNT_TARGET_MAX - target.len()));
        target
    };
    let attaches = [
        attach(None, None),
        attach(Some("workbench"), None),
        ImageAttach {
            access: ImageAccess::ReadOnly,
            expects: only(VolumeFsType::Affs),
            ..attach(None, Some("/Users/a/disk"))
        },
        ImageAttach {
            access: ImageAccess::Writable,
            ..attach(Some("floppy"), None)
        },
        ImageAttach {
            presentation: Presentation::Shared,
            expects: VolumeFsTypes::of([VolumeFsType::Udf, VolumeFsType::Iso9660]),
            ..attach(Some("iso"), None)
        },
        ImageAttach {
            presentation: Presentation::Explicit {
                uid: 1000,
                gid: 100,
                dir_mode: 0o750,
                file_mode: 0o640,
            },
            file_name: Some(&leaf),
            ..attach(Some(&name), Some(&target))
        },
        ImageAttach {
            adopt: true,
            ..attach(None, None)
        },
        ImageAttach {
            access: ImageAccess::Writable,
            file_name: Some("Backup.zip"),
            ..attach(None, None)
        },
    ];
    for one in attaches {
        let bytes = encoded(&one);
        assert_eq!(ImageAttach::decode(&bytes), Ok(one));
    }
    assert_eq!(encoded(&attaches[5]).len(), IMAGE_ATTACH_MAX);
}

#[test]
fn an_access_accepts_only_a_volume_as_writable_as_it_asks() {
    for (access, read_only, accepted) in [
        (ImageAccess::ReadOnly, true, true),
        (ImageAccess::ReadOnly, false, false),
        (ImageAccess::WritableWherePossible, true, true),
        (ImageAccess::WritableWherePossible, false, true),
        (ImageAccess::Writable, true, false),
        (ImageAccess::Writable, false, true),
    ] {
        assert_eq!(
            access.accepts(read_only),
            accepted,
            "{access:?} {read_only}"
        );
    }
}

#[test]
fn an_attach_out_of_shape_is_never_encoded() {
    let mut out = [0u8; IMAGE_ATTACH_MAX];
    for bad in [
        attach(Some(""), None),
        attach(Some("Upper"), None),
        attach(Some("bad name"), None),
        attach(None, Some("relative/dir")),
        attach(None, Some("/Users/a/\u{7}")),
        ImageAttach {
            presentation: Presentation::Explicit {
                uid: 0,
                gid: 0,
                dir_mode: 0o10_000,
                file_mode: 0o600,
            },
            ..attach(None, None)
        },
    ] {
        assert_eq!(bad.encode(&mut out), Err(Errno::OutOfRange), "{bad:?}");
    }
    assert_eq!(
        attach(Some("disk"), None).encode(&mut [0u8; 8]),
        Err(Errno::BufferTooSmall)
    );
}

/// A named corruption of an encoded frame, and the refusal it earns.
type Mangling = (&'static str, fn(&mut Vec<u8>), Errno);

#[test]
fn an_attach_frame_out_of_shape_is_refused() {
    let good = encoded(&ImageAttach {
        expects: only(VolumeFsType::Iso9660),
        ..attach(Some("disc"), Some("/Users/a/disc"))
    });
    assert!(ImageAttach::decode(&good).is_ok());
    let cases: [Mangling; 13] = [
        ("version", |b| b[0] = 2, Errno::AbiVersionUnsupported),
        ("presentation kind", |b| b[2] = 9, Errno::OutOfRange),
        (
            "owner on a private presentation",
            |b| b[4] = 1,
            Errno::OutOfRange,
        ),
        ("unknown flag", |b| b[3] |= 0x80, Errno::OutOfRange),
        (
            "read-only and writable both",
            |b| b[3] |= 0x01 | 0x04,
            Errno::OutOfRange,
        ),
        ("unknown filesystem", |b| b[21] |= 0x80, Errno::OutOfRange),
        (
            "a claim naming no filesystem",
            |b| b[20..24].fill(0),
            Errno::OutOfRange,
        ),
        (
            "filesystem with no claim",
            |b| b[3] &= !0x02,
            Errno::OutOfRange,
        ),
        (
            "name past the frame",
            |b| b[16] = 40,
            Errno::LengthOutOfRange,
        ),
        (
            "target short of the frame",
            |b| b[18] = 1,
            Errno::LengthOutOfRange,
        ),
        (
            "a file name past the frame",
            |b| b[17] = 1,
            Errno::LengthOutOfRange,
        ),
        ("name not UTF-8", |b| b[24] = 0xFF, Errno::OutOfRange),
        (
            "frame cut short",
            |b| b.truncate(23),
            Errno::LengthOutOfRange,
        ),
    ];
    for (what, corrupt, errno) in cases {
        let mut bad = good.clone();
        corrupt(&mut bad);
        assert_eq!(ImageAttach::decode(&bad), Err(errno), "{what}");
    }
    let mut long = good.clone();
    long.resize(IMAGE_ATTACH_MAX + 1, 0);
    assert_eq!(ImageAttach::decode(&long), Err(Errno::LengthOutOfRange));
    assert_eq!(u16::from_le_bytes([good[0], good[1]]), IMAGE_ATTACH_VERSION);
}

#[test]
fn every_refusal_round_trips_through_its_code_and_spelling() {
    let mut spellings = Vec::new();
    for (at, refusal) in ImageRefusal::ALL.into_iter().enumerate() {
        assert_eq!(usize::from(refusal.as_u8()), at + 1);
        assert_eq!(ImageRefusal::from_u8(refusal.as_u8()), Ok(refusal));
        assert!(!spellings.contains(&refusal.as_str()));
        spellings.push(refusal.as_str());
    }
    assert_eq!(ImageRefusal::from_u8(0), Err(Errno::OutOfRange));
    let past = u8::try_from(ImageRefusal::ALL.len()).expect("a byte") + 1;
    assert_eq!(ImageRefusal::from_u8(past), Err(Errno::OutOfRange));
}

#[test]
fn every_refusal_is_told_in_a_clause_of_its_own() {
    let mut told = Vec::new();
    for refusal in ImageRefusal::ALL {
        let reason = refusal.reason();
        assert!(
            reason.starts_with(char::is_lowercase) && !reason.ends_with('.'),
            "{refusal:?}"
        );
        assert!(!told.contains(&reason), "{refusal:?}");
        told.push(reason);
    }
}

#[test]
fn a_reply_round_trips_and_refuses_what_it_could_not_carry() {
    let volume = AttachReply::new([7; 16], "workbench", true).expect("a reply");
    let replies = [
        ImageAttachReply::Attached(volume),
        ImageAttachReply::AlreadyAttached(volume),
        ImageAttachReply::Refused(ImageRefusal::Partitioned),
        ImageAttachReply::TargetRefused(Errno::NotEmpty),
        ImageAttachReply::NameRefused(Errno::AlreadyExists),
    ];
    for reply in replies {
        assert_eq!(ImageAttachReply::decode(&reply.encode()), Ok(reply));
    }
    assert_ne!(
        replies[3].encode()[0],
        replies[4].encode()[0],
        "a refused target and a refused name are told apart"
    );
    assert_eq!(
        ImageAttachReply::decode(&[0; IMAGE_ATTACH_REPLY_LEN - 1]),
        Err(Errno::LengthOutOfRange)
    );
    let attached = replies[0].encode();
    let refused = replies[2].encode();
    let target = replies[3].encode();
    let name = replies[4].encode();
    let cases: [(&str, [u8; IMAGE_ATTACH_REPLY_LEN], usize, u8); 14] = [
        ("unknown outcome", attached, 0, 9),
        ("no outcome", attached, 0, 0),
        ("pad", attached, 3, 1),
        ("a volume carrying an errno", attached, 5, 1),
        ("a volume carrying a refusal", attached, 1, 1),
        ("a refusal carrying a volume", refused, 8, 1),
        ("a refusal carrying an errno", refused, 4, 1),
        ("unknown refusal", refused, 1, 0xEE),
        ("a refused target carrying a volume", target, 8, 1),
        ("a refused target carrying a refusal", target, 1, 1),
        ("an errno the ABI does not define", target, 7, 0x7F),
        ("a refused name carrying a volume", name, 8, 1),
        ("a refused name carrying a refusal", name, 1, 1),
        (
            "a refused name's errno the ABI does not define",
            name,
            7,
            0x7F,
        ),
    ];
    for (what, good, at, value) in cases {
        let mut bad = good;
        bad[at] = value;
        assert_eq!(
            ImageAttachReply::decode(&bad),
            Err(Errno::OutOfRange),
            "{what}"
        );
    }
}

#[test]
fn an_image_attach_is_the_network_attach_frame_with_its_own_version_and_claim() {
    use crate::netattach::{NetworkAttach, NETWORK_ATTACH_MAX};
    let presentation = Presentation::Explicit {
        uid: 1000,
        gid: 100,
        dir_mode: 0o750,
        file_mode: 0o640,
    };
    let image = ImageAttach {
        name: Some("floppy"),
        presentation,
        access: ImageAccess::ReadOnly,
        target: Some("/Users/a/Home/disk"),
        expects: None,
        adopt: false,
        file_name: None,
    };
    let network = NetworkAttach {
        name: "floppy",
        presentation,
        read_only: true,
        target: Some("/Users/a/Home/disk"),
    };
    let mut out = [0; NETWORK_ATTACH_MAX];
    let len = network.encode(&mut out).expect("encodes");
    let image_bytes = encoded(&image);
    assert_eq!(IMAGE_ATTACH_MAX, NETWORK_ATTACH_MAX + FS_NAME_MAX);
    assert_eq!(image_bytes.len(), len);
    assert_eq!(image_bytes[2..], out[2..len], "one layout past the version");
    let claimed = encoded(&ImageAttach {
        expects: only(VolumeFsType::Affs),
        ..image
    });
    assert_eq!(
        u32::from_le_bytes([claimed[20], claimed[21], claimed[22], claimed[23]]),
        1 << VolumeFsType::Affs.as_u8(),
        "the frame's own word"
    );
}

#[test]
fn an_expected_set_round_trips_and_unknown_bits_are_refused() {
    let disc = VolumeFsTypes::of([VolumeFsType::Udf, VolumeFsType::Iso9660]).expect("a set");
    assert!(disc.contains(VolumeFsType::Udf) && disc.contains(VolumeFsType::Iso9660));
    assert!(!disc.contains(VolumeFsType::Affs));
    assert_eq!(VolumeFsTypes::from_bits(disc.bits()), Ok(disc));
    let every = VolumeFsTypes::of(VolumeFsType::ALL).expect("a set");
    assert!(VolumeFsType::ALL
        .into_iter()
        .all(|fstype| every.contains(fstype)));
    assert_eq!(VolumeFsTypes::of([]), None, "an empty set is no claim");
    assert_eq!(VolumeFsTypes::from_bits(0), Err(Errno::OutOfRange));
    assert_eq!(
        VolumeFsTypes::from_bits(every.bits() | 1 << VolumeFsType::ALL.len()),
        Err(Errno::OutOfRange),
        "a bit naming no filesystem"
    );
}

#[test]
fn a_file_name_is_one_directory_entry() {
    for refused in [".", "..", "a/b"] {
        let attach = ImageAttach {
            file_name: Some(refused),
            ..attach(None, None)
        };
        assert_eq!(
            attach.encode(&mut [0; IMAGE_ATTACH_MAX]),
            Err(Errno::OutOfRange),
            "{refused:?}"
        );
    }
    let mut bytes = encoded(&ImageAttach {
        file_name: Some("a.b"),
        ..attach(None, None)
    });
    let dot = bytes.len() - 2;
    bytes[dot] = b'/';
    assert_eq!(ImageAttach::decode(&bytes), Err(Errno::OutOfRange));
}
