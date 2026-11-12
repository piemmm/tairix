use alloc::vec::Vec;

use tairix_abi::driver::filesystem::VolumeStats;
use tairix_abi::time::Time64;
use tairix_sandbox::wire::{Reader, WireError, Writer};

use super::{
    decode_answer, encode_answer, Access, Creation, DosName, Facts, From, Op, Refusal, Reply,
    SharePath, DEPTH, IO_MOST, REFUSALS,
};

/// The name — or the pattern — `text` spells, as DOS forms it.
pub(crate) fn name(text: &str) -> DosName {
    let field = crate::dos::name::field(text.as_bytes(), true).expect("a DOS name");
    DosName::new(field, true).expect("a DOS name")
}

/// The share path `names` spell, `\`-separated.
pub(crate) fn path(names: &str) -> SharePath {
    let names: Vec<DosName> = names
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(name)
        .collect();
    SharePath::of(&names).expect("a share path")
}

/// [`IO_MOST`] as the wire's word.
fn io_most() -> u32 {
    u32::try_from(IO_MOST).expect("a word")
}

fn facts() -> Facts {
    Facts {
        name: name("README.TXT"),
        directory: false,
        size: 1234,
        modified: Time64::new(1_000_000_000, 500).expect("canonical"),
        writable: true,
        hidden: false,
    }
}

fn op_round_trips(op: Op<'_>) {
    let mut w = Writer::new();
    op.encode(&mut w);
    let bytes = w.finish();
    let mut r = Reader::new(&bytes);
    assert_eq!(Op::decode(&mut r), Ok(op));
    assert!(r.is_exhausted(), "{op:?} decodes whole");
}

fn answer_round_trips(answer: Result<Reply<'_>, Refusal>) {
    let mut w = Writer::new();
    encode_answer(&answer, &mut w);
    let bytes = w.finish();
    let mut r = Reader::new(&bytes);
    assert_eq!(decode_answer(&mut r), Ok(answer));
    assert!(r.is_exhausted(), "{answer:?} decodes whole");
}

#[test]
fn a_dos_name_is_what_dos_could_have_made() {
    assert!(DosName::new(*b"README  TXT", false).is_some());
    assert!(DosName::new(*b"A          ", false).is_some());
    assert!(
        DosName::new(*b"\x05BC     TXT", false).is_some(),
        "05h stands for E5h"
    );
    for refused in [
        *b"           ",
        *b"readme  TXT",
        *b"READ ME TXT",
        *b"READ.ME TXT",
        *b"READ*   TXT",
        *b"READ+   TXT",
        *b"AB\x05     TXT",
        *b"README  T X",
    ] {
        assert_eq!(DosName::new(refused, false), None, "{refused:?}");
    }
    assert_eq!(DosName::new(*b"READ????TXT", false), None);
    let pattern = DosName::new(*b"READ????TXT", true).expect("a pattern");
    assert!(pattern.is_wild() && pattern.matches(&name("README.TXT")));
    assert!(!pattern.matches(&name("READ.DOC")));
}

#[test]
fn a_share_path_holds_no_pattern_and_no_more_than_dos_names() {
    assert_eq!(SharePath::ROOT.names().count(), 0);
    assert_eq!(SharePath::ROOT.split_last(), None);
    let (parent, leaf) = path("\\GAMES\\DOOM\\DOOM.EXE")
        .split_last()
        .expect("not the root");
    assert_eq!(parent, path("\\GAMES\\DOOM"));
    assert_eq!(leaf, name("DOOM.EXE"));
    assert_eq!(SharePath::of(&[name("*.TXT")]), None);
    assert_eq!(
        SharePath::of(&[super::DosName::DOT]),
        None,
        "no path names a dotted entry"
    );
    let deep = [name("A"); DEPTH + 1];
    assert_eq!(SharePath::of(&deep), None);
    let deepest = SharePath::of(&deep[..DEPTH]).expect("as deep as DOS names");
    assert_eq!(deepest.names().count(), DEPTH);
    let long = [name("LONGNAME.TXT"); 5];
    assert_eq!(SharePath::of(&long), None, "longer than a DOS path");
    assert!(SharePath::of(&long[..4]).is_some());
    let sigma = DosName::new(*b"\x05BC     TXT", false).expect("E5h kept as 05h");
    let kept = SharePath::of(&[name("DIR"), sigma]).expect("a share path");
    assert_eq!(kept.names().collect::<Vec<_>>(), [name("DIR"), sigma]);
    assert_eq!(kept.split_last(), Some((path("DIR"), sigma)));
}

#[test]
fn a_share_path_crosses_the_wire_as_dos_spells_it() {
    let mut w = Writer::new();
    Op::Look {
        path: path("\\GAMES\\DOOM.EXE"),
    }
    .encode(&mut w);
    let bytes = w.finish();
    assert_eq!(&bytes[1..], b"\x0eGAMES\\DOOM.EXE");
    assert_eq!(
        Op::decode(&mut Reader::new(&bytes)),
        Ok(Op::Look {
            path: path("\\GAMES\\DOOM.EXE")
        })
    );
}

#[test]
fn every_op_crosses_the_wire_whole() {
    let data = [7u8; 64];
    for op in [
        Op::Look {
            path: SharePath::ROOT,
        },
        Op::Look {
            path: path("\\A\\B.TXT"),
        },
        Op::Find {
            pattern: name("*.*"),
            attributes: 0x12,
            from: From::Start { dir: path("\\A") },
        },
        Op::Find {
            pattern: name("????????.???"),
            attributes: 0,
            from: From::Next {
                table: 3,
                index: u32::MAX,
            },
        },
        Op::Open {
            path: path("\\F.TXT"),
            access: Access::Both,
            creation: Creation::Truncate,
        },
        Op::Open {
            path: path("\\F.TXT"),
            access: Access::Read,
            creation: Creation::Existing,
        },
        Op::Open {
            path: path("\\F.TXT"),
            access: Access::Write,
            creation: Creation::New,
        },
        Op::Read {
            handle: 0x0102_0301,
            offset: 512,
            len: io_most(),
        },
        Op::Write {
            handle: 9,
            offset: u32::MAX - 64,
            bytes: &data,
        },
        Op::Resize { handle: 9, size: 0 },
        Op::Close {
            handle: 9,
            stamp: None,
        },
        Op::Close {
            handle: 9,
            stamp: Some(Time64::from_secs(-5)),
        },
        Op::Commit { handle: 9 },
        Op::Size { handle: 9 },
        Op::Remove {
            path: path("\\D"),
            directory: true,
        },
        Op::Rename {
            from: path("\\A.TXT"),
            to: path("\\D\\B.TXT"),
        },
        Op::MakeDir { path: path("\\D") },
        Op::Space,
    ] {
        op_round_trips(op);
    }
}

#[test]
fn every_answer_crosses_the_wire_whole() {
    let bytes = [3u8; 100];
    let stats = VolumeStats {
        block_size: 4096,
        in_reserve: false,
        total_blocks: 100,
        free_blocks: 50,
        avail_blocks: 40,
        files: 0,
        files_free: 0,
    };
    for answer in [
        Ok(Reply::Done),
        Ok(Reply::Looked(facts())),
        Ok(Reply::Found {
            facts: Facts {
                directory: true,
                size: 0,
                hidden: true,
                ..facts()
            },
            table: 2,
            next: 7,
        }),
        Ok(Reply::Opened {
            handle: 0x100,
            file: 0xDEAD_BEEF,
            facts: facts(),
        }),
        Ok(Reply::Read(&bytes)),
        Ok(Reply::Read(&[])),
        Ok(Reply::Wrote(0)),
        Ok(Reply::Wrote(io_most())),
        Ok(Reply::Size(u32::MAX)),
        Ok(Reply::Space(stats)),
    ] {
        answer_round_trips(answer);
    }
    for refusal in REFUSALS {
        answer_round_trips(Err(refusal));
    }
}

/// Whether `bytes` decode to an op, and why not.
fn decode(bytes: &[u8]) -> Result<(), WireError> {
    Op::decode(&mut Reader::new(bytes)).map(drop)
}

#[test]
fn a_malformed_op_or_answer_is_refused() {
    let mut w = Writer::new();
    Op::Read {
        handle: 1,
        offset: 0,
        len: 1,
    }
    .encode(&mut w);
    let mut read = w.finish();
    read[9..13].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(decode(&read), Err(WireError::Malformed), "nothing to read");
    read[9..13].copy_from_slice(&(io_most() + 1).to_le_bytes());
    assert_eq!(
        decode(&read),
        Err(WireError::Malformed),
        "past one read's most"
    );
    assert_eq!(decode(&[99]), Err(WireError::Malformed));
    let lower = [
        1, 1, b'a', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
    ];
    assert_eq!(
        decode(&lower),
        Err(WireError::Malformed),
        "a name in lower case"
    );
    let longest = u8::try_from(super::SPELLED + 1).expect("a byte");
    assert_eq!(
        decode(&[1, longest]),
        Err(WireError::Malformed),
        "longer than a DOS path"
    );
    for spelled in [
        &b"A\\\\B"[..],
        b"A\\",
        b"\\A",
        b"A.B.C",
        b"A.",
        b"*.TXT",
        b"..",
    ] {
        let mut bytes = alloc::vec![1, u8::try_from(spelled.len()).expect("a byte")];
        bytes.extend_from_slice(spelled);
        assert_eq!(
            decode(&bytes),
            Err(WireError::Malformed),
            "{spelled:?} is no path's spelling"
        );
    }
    let mut w = Writer::new();
    Op::Remove {
        path: path("\\A"),
        directory: false,
    }
    .encode(&mut w);
    let mut removal = w.finish();
    removal[1] = 0;
    assert_eq!(
        decode(&removal[..2]),
        Err(WireError::Malformed),
        "the root is not removed"
    );
    let mut w = Writer::new();
    encode_answer(
        &Ok(Reply::Looked(Facts {
            directory: true,
            size: 0,
            ..facts()
        })),
        &mut w,
    );
    let mut sized = w.finish();
    sized[13..17].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        decode_answer(&mut Reader::new(&sized)),
        Err(WireError::Malformed),
        "a directory has no length"
    );
    let past = u8::try_from(REFUSALS.len()).expect("a byte");
    assert_eq!(
        decode_answer(&mut Reader::new(&[0, past])),
        Err(WireError::Malformed)
    );
}
