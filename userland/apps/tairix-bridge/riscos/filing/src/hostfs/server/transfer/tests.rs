//! `Transfer` as the session sees it through the server: each delivery in a
//! directory of its own, its files read, and written only where delivered
//! writable.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_fscopy::fixture::{Handle, Tree};
use tairix_fsmeta::preset::acorn;

use super::super::tests::{ask, bytes, filetype, given, SEED};
use super::super::Server;
use crate::hostfs::protocol::{Kind, Object, Refusal, Reply, Request};

/// `Transfer`'s root number on a server of one root.
const TRANSFER: u32 = 1;

fn server(tree: &Tree) -> Server<Handle> {
    Server::new(None, vec![given(tree, "/", "Disc", true)], SEED, 7)
}

fn delivered(tree: &Tree, path: &str, writable: bool) -> Handle {
    tree.open_file(path, false, writable)
        .expect("a file to deliver")
}

fn object(server: &mut Server<Handle>, path: &[u8]) -> Object {
    match ask(
        server,
        &Request::Read {
            root: TRANSFER,
            path,
        },
    ) {
        Reply::Object(object) => object,
        other => panic!("an object for {path:?}: {other:?}"),
    }
}

fn names(server: &mut Server<Handle>, path: &[u8]) -> Vec<Vec<u8>> {
    match ask(
        server,
        &Request::List {
            root: TRANSFER,
            path,
            offset: 0,
            count: 64,
        },
    ) {
        Reply::Entries { entries, next } => {
            assert_eq!(next, u32::MAX, "one page");
            entries.into_iter().map(|(name, _)| name).collect()
        }
        other => panic!("a listing of {path:?}: {other:?}"),
    }
}

#[test]
fn each_delivery_stands_in_a_directory_of_its_own() {
    let tree = Tree::new()
        .file("/a/Notes.txt", b"first")
        .file("/b/Notes.txt", b"second!")
        .file("/My Pic.png", b"p");
    let mut server = server(&tree);
    let paths: Vec<Vec<u8>> = [
        ("Notes.txt", "/a/Notes.txt"),
        ("Notes.txt", "/b/Notes.txt"),
        ("My Pic.png", "/My Pic.png"),
    ]
    .into_iter()
    .map(|(name, path)| {
        server
            .deliver(name, delivered(&tree, path, false), false)
            .expect("delivered")
    })
    .collect();
    assert_eq!(
        paths,
        [
            &b"HostFS::Transfer.$.1.Notes/txt"[..],
            b"HostFS::Transfer.$.2.Notes/txt",
            b"HostFS::Transfer.$.3.My\xA0Pic/png"
        ]
    );
    assert_eq!(names(&mut server, b""), [&b"1"[..], b"2", b"3"]);
    assert_eq!(names(&mut server, b"2"), [&b"Notes/txt"[..]]);
    let second = object(&mut server, b"2.NOTES/TXT");
    assert_eq!(
        (second.kind, second.length, filetype(&second)),
        (Kind::File, 7, 0xFFF),
        "found ignoring case"
    );
    assert_eq!(
        filetype(&object(&mut server, b"3.My\xA0Pic/png")),
        0xB60,
        "typed by its own name"
    );
    assert_eq!(object(&mut server, b"1").kind, Kind::Directory);
    assert_eq!(object(&mut server, b"").kind, Kind::Directory, "the root");
    for missing in [&b"4"[..], b"0", b"01", b"1.Other", b"x"] {
        assert_eq!(object(&mut server, missing), Object::NOTHING, "{missing:?}");
    }
    assert_eq!(
        ask(
            &mut server,
            &Request::Read {
                root: TRANSFER,
                path: b"4.Notes/txt"
            }
        ),
        Reply::Refused(Refusal::NotFound),
        "beneath no delivery"
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Read {
                root: TRANSFER,
                path: b"1.Notes/txt.x"
            }
        ),
        Reply::Refused(Refusal::NotADirectory)
    );
    let Reply::Entries { next, entries } = ask(
        &mut server,
        &Request::List {
            root: TRANSFER,
            path: b"",
            offset: 1,
            count: 1,
        },
    ) else {
        panic!("a page");
    };
    assert_eq!((next, entries.len()), (2, 1));
}

/// A server whose `Transfer` holds `/Doc`, delivered read-only as `1.Doc`.
fn read_only_delivery() -> (Tree, Server<Handle>) {
    let tree = Tree::new().file("/Doc", b"contents");
    let mut server = server(&tree);
    server
        .deliver("Doc", delivered(&tree, "/Doc", false), false)
        .expect("delivered");
    (tree, server)
}

#[test]
fn a_delivery_not_made_writable_is_read_and_never_written() {
    let (tree, mut server) = read_only_delivery();
    let doc = object(&mut server, b"1.Doc");
    assert_eq!(doc.attributes & 0x22, 0, "no write access");
    assert_ne!(doc.attributes & 0x08, 0, "locked");
    let Reply::Opened {
        handle,
        extent,
        writable,
        ..
    } = ask(
        &mut server,
        &Request::Open {
            root: TRANSFER,
            path: b"1.Doc",
            update: false,
        },
    )
    else {
        panic!("opened");
    };
    assert_eq!((extent, writable), (8, false));
    assert_eq!(bytes(&mut server, handle, 4, 10), Ok(b"ents".to_vec()));
    assert_eq!(
        ask(
            &mut server,
            &Request::WriteBytes {
                handle,
                offset: 0,
                bytes: b"x"
            }
        ),
        Reply::Refused(Refusal::AccessViolation)
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Close {
                handle,
                stamp: Some((1, 2))
            }
        ),
        Reply::Done
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Open {
                root: TRANSFER,
                path: b"1.Doc",
                update: true
            }
        ),
        Reply::Refused(Refusal::AccessViolation)
    );
    assert_eq!(tree.bytes("/Doc"), Some(b"contents".to_vec()));
}

/// Each change beside the read-only `1.Doc`, with the refusal it meets.
fn changes_refused() -> [(Request<'static>, Refusal); 9] {
    [
        (
            Request::Create {
                root: TRANSFER,
                path: b"1.Doc",
                load: 0,
                exec: 0,
                length: 0,
            },
            Refusal::AccessViolation,
        ),
        (
            Request::Create {
                root: TRANSFER,
                path: b"1.New",
                load: 0,
                exec: 0,
                length: 0,
            },
            Refusal::WriteProtected,
        ),
        (
            Request::WriteInfo {
                root: TRANSFER,
                path: b"1.Doc",
                load: 0,
                exec: 0,
                attributes: 3,
            },
            Refusal::AccessViolation,
        ),
        (
            Request::Delete {
                root: TRANSFER,
                path: b"1.Doc",
            },
            Refusal::WriteProtected,
        ),
        (
            Request::Delete {
                root: TRANSFER,
                path: b"1",
            },
            Refusal::WriteProtected,
        ),
        (
            Request::Rename {
                root: TRANSFER,
                from: b"1.Doc",
                to: b"1.Moved",
            },
            Refusal::WriteProtected,
        ),
        (
            Request::Rename {
                root: TRANSFER,
                from: b"1.Gone",
                to: b"1.Moved",
            },
            Refusal::NotFound,
        ),
        (
            Request::CreateDir {
                root: TRANSFER,
                path: b"1",
                load: 0,
                exec: 0,
            },
            Refusal::Exists,
        ),
        (
            Request::CreateDir {
                root: TRANSFER,
                path: b"Mine",
                load: 0,
                exec: 0,
            },
            Refusal::WriteProtected,
        ),
    ]
}

#[test]
fn nothing_is_made_renamed_or_deleted_beside_a_read_only_delivery() {
    let (tree, mut server) = read_only_delivery();
    for (request, refusal) in changes_refused() {
        assert_eq!(
            ask(&mut server, &request),
            Reply::Refused(refusal),
            "{request:?}"
        );
    }
    assert_eq!(
        ask(
            &mut server,
            &Request::Delete {
                root: TRANSFER,
                path: b"9"
            }
        ),
        Reply::Object(Object::NOTHING)
    );
    assert_eq!(
        ask(&mut server, &Request::Space { root: TRANSFER }),
        Reply::Space { free: 0, total: 0 }
    );
    assert_eq!(tree.bytes("/Doc"), Some(b"contents".to_vec()));
}

#[test]
fn a_writable_delivery_is_saved_over_in_place_its_type_its_names_where_nothing_else_keeps_one() {
    let tree = Tree::new()
        .refuses_attr("acorn.loadaddr", Errno::NotSupported)
        .file("/Letter.txt", b"Dear all");
    let mut server = server(&tree);
    server
        .deliver("Letter.txt", delivered(&tree, "/Letter.txt", true), true)
        .expect("delivered");
    let text = acorn::encode_typed(acorn::Filetype::from_low_bits(0xFFF), 0x55_0000_0000)
        .expect("a stamp");
    let sprite = acorn::encode_typed(acorn::Filetype::from_low_bits(0xFF9), 0x55_0000_0000)
        .expect("a stamp");
    assert_eq!(
        ask(
            &mut server,
            &Request::Create {
                root: TRANSFER,
                path: b"1.Letter/txt",
                load: sprite.0,
                exec: sprite.1,
                length: 2
            }
        ),
        Reply::Refused(Refusal::Unrepresentable),
        "a type its name cannot give"
    );
    assert_eq!(
        tree.bytes("/Letter.txt"),
        Some(b"Dear all".to_vec()),
        "refused before it was emptied"
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Create {
                root: TRANSFER,
                path: b"1.letter/txt",
                load: text.0,
                exec: text.1,
                length: 2
            }
        ),
        Reply::Done
    );
    assert_eq!(
        tree.bytes("/Letter.txt"),
        Some(vec![0, 0]),
        "emptied to its new length"
    );
    let Reply::Opened {
        handle, writable, ..
    } = ask(
        &mut server,
        &Request::Open {
            root: TRANSFER,
            path: b"1.Letter/txt",
            update: true,
        },
    )
    else {
        panic!("opened");
    };
    assert!(writable);
    assert_eq!(
        ask(
            &mut server,
            &Request::WriteBytes {
                handle,
                offset: 0,
                bytes: b"Hi"
            }
        ),
        Reply::Wrote(2)
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Close {
                handle,
                stamp: Some(text)
            }
        ),
        Reply::Done
    );
    assert_eq!(tree.bytes("/Letter.txt"), Some(b"Hi".to_vec()));
    let kept = object(&mut server, b"1.Letter/txt");
    assert_eq!((kept.load, kept.exec), text, "its stamp kept as its time");
}

#[test]
fn a_name_risc_os_cannot_hold_is_refused_delivery() {
    let tree = Tree::new().file("/f", b"");
    let mut server = server(&tree);
    for name in ["", "wild*card", "no\u{a0}break", "."] {
        assert_eq!(
            server.deliver(name, delivered(&tree, "/f", false), false),
            Err(Refusal::BadName),
            "{name:?}"
        );
    }
    assert_eq!(
        names(&mut server, b""),
        Vec::<Vec<u8>>::new(),
        "nothing delivered"
    );
}

#[test]
fn a_number_is_spelt_and_read_in_plain_decimal() {
    let mut spelt = Vec::new();
    for number in [0, 7, 10, 12_345, usize::MAX] {
        spelt.clear();
        super::push_decimal(&mut spelt, number);
        assert_eq!(spelt, alloc::format!("{number}").into_bytes());
        assert_eq!(super::parse_decimal(&spelt), Some(number));
    }
    for bad in [&b""[..], b"01", b"1a", b"-1", b"184467440737095516150"] {
        assert_eq!(super::parse_decimal(bad), None, "{bad:?}");
    }
}
