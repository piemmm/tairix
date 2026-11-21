//! HostFS's server over the in-memory tree: names, case, metadata, listings,
//! open files and changes, as RISC OS sees each.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_fscopy::fixture::{Handle, Tree};
use tairix_fscopy::tree::Roots;
use tairix_fsmeta::preset::acorn;

use super::{Given, Server};
use crate::hostfs::protocol::{Kind, Object, Refusal, Reply, Request, Version};

pub(super) const SEED: [u8; 16] = [3; 16];

pub(super) fn given(tree: &Tree, path: &str, name: &str, writable: bool) -> Given<Handle> {
    Given {
        dir: tree.open_dir(path, false).expect("a root"),
        name: String::from(name),
        writable,
    }
}

fn server(tree: &Tree, writable: bool) -> Server<Handle> {
    Server::new(None, vec![given(tree, "/", "Disc", writable)], SEED, 0x12)
}

/// The server's encoded answer to `request`.
fn answer(server: &mut Server<Handle>, request: &Request<'_>) -> Vec<u8> {
    let mut bytes = Vec::new();
    request.encode(&mut bytes);
    let mut out = Vec::new();
    server.serve(&bytes, &mut out);
    out
}

/// The server's reply to `request`, which carries no bytes of a file.
pub(super) fn ask(server: &mut Server<Handle>, request: &Request<'_>) -> Reply<'static> {
    let out = answer(server, request);
    match Reply::decode(&out, request).expect("a reply that answers the request") {
        Reply::Roots { seed, own, discs } => Reply::Roots { seed, own, discs },
        Reply::Object(object) => Reply::Object(object),
        Reply::Entries { next, entries } => Reply::Entries { next, entries },
        Reply::Opened {
            handle,
            extent,
            writable,
            version,
        } => Reply::Opened {
            handle,
            extent,
            writable,
            version,
        },
        Reply::Wrote(count) => Reply::Wrote(count),
        Reply::Done => Reply::Done,
        Reply::Space { free, total } => Reply::Space { free, total },
        Reply::Refused(refusal) => Reply::Refused(refusal),
        Reply::Bytes(_) => panic!("bytes are read through `bytes`"),
    }
}

/// `length` bytes of the file on `handle` from `offset`, or the refusal.
pub(super) fn bytes(
    server: &mut Server<Handle>,
    handle: u32,
    offset: u32,
    length: u32,
) -> Result<Vec<u8>, Refusal> {
    let request = Request::ReadBytes {
        handle,
        offset,
        length,
    };
    let out = answer(server, &request);
    match Reply::decode(&out, &request).expect("a reply that answers the request") {
        Reply::Bytes(bytes) => Ok(bytes.to_vec()),
        Reply::Refused(refusal) => Err(refusal),
        other => panic!("not what a read answers: {other:?}"),
    }
}

fn read(server: &mut Server<Handle>, path: &[u8]) -> Reply<'static> {
    ask(server, &Request::Read { root: 0, path })
}

fn object(server: &mut Server<Handle>, path: &[u8]) -> Object {
    match read(server, path) {
        Reply::Object(object) => object,
        other => panic!("an object for {path:?}: {other:?}"),
    }
}

fn names(server: &mut Server<Handle>, path: &[u8]) -> Vec<Vec<u8>> {
    match ask(
        server,
        &Request::List {
            root: 0,
            path,
            offset: 0,
            count: 64,
        },
    ) {
        Reply::Entries { entries, .. } => entries.into_iter().map(|(name, _)| name).collect(),
        other => panic!("a listing: {other:?}"),
    }
}

/// The type a typed load word gives.
pub(super) fn filetype(object: &Object) -> u32 {
    tairix_riscos_kernel::filing::fs::stamp::type_of(object.load)
        .unwrap_or_else(|| panic!("typed: {object:?}"))
}

#[test]
fn the_sessions_own_disc_comes_first_and_is_named_as_its_reply_says() {
    let tree = Tree::new().dir("/home").dir("/apps");
    let mut server = Server::new(
        Some(given(&tree, "/home", "RISC OS", true)),
        vec![given(&tree, "/apps", "Apps", false)],
        SEED,
        1,
    );
    let Reply::Roots { own, discs, .. } = ask(&mut server, &Request::Roots) else {
        panic!("the roots");
    };
    assert!(own, "the first disc is the session's own");
    assert_eq!(discs, [&b"RISC\xA0OS"[..], b"Apps", b"Transfer"]);
}

#[test]
fn each_root_is_a_disc_named_for_its_folder_and_made_unique() {
    let tree = Tree::new().dir("/a").dir("/b").dir("/c");
    let mut server = Server::new(
        None,
        vec![
            given(&tree, "/a", "RISC OS", true),
            given(&tree, "/b", "risc os", false),
            given(&tree, "/c", "My:Apps", false),
        ],
        SEED,
        1,
    );
    let Reply::Roots { seed, own, discs } = ask(&mut server, &Request::Roots) else {
        panic!("the roots");
    };
    assert_eq!(seed, SEED);
    assert!(!own, "no root was given as the session's own");
    assert_eq!(
        discs,
        [&b"RISC\xA0OS"[..], b"risc\xA0os2", b"My_Apps", b"Transfer"],
        "a space is a hard space, a clash ignoring case is numbered, a reserved character is `_`, Transfer last"
    );
    let named_transfer = Server::new(None, vec![given(&tree, "/a", "transfer", true)], SEED, 1);
    let mut named_transfer = named_transfer;
    let Reply::Roots { discs, .. } = ask(&mut named_transfer, &Request::Roots) else {
        panic!("the roots");
    };
    assert_eq!(
        discs,
        [&b"transfer"[..], b"Transfer2"],
        "Transfer's own name is made unique too"
    );
}

#[test]
fn a_files_type_comes_from_its_attributes_then_its_suffix_then_its_name() {
    let tree = Tree::new()
        .file("/kept", b"k")
        .attr(
            "/kept",
            "acorn.loadaddr",
            &acorn::addr_to_value(0xFFFF_F912),
        )
        .attr(
            "/kept",
            "acorn.execaddr",
            &acorn::addr_to_value(0x3456_789A),
        )
        .file("/Sprites,ff9", b"s")
        .file("/notes.txt", b"hello")
        .file("/mystery", b"?")
        .dir("/Sub");
    let mut server = server(&tree, true);
    let kept = object(&mut server, b"kept");
    assert_eq!(
        (kept.load, kept.exec),
        (0xFFFF_F912, 0x3456_789A),
        "the volume's own words"
    );
    assert_eq!(
        filetype(&object(&mut server, b"Sprites")),
        0xFF9,
        "the suffix, taken off the name"
    );
    let text = object(&mut server, b"notes/txt");
    assert_eq!(
        (filetype(&text), text.length, text.kind),
        (0xFFF, 5, Kind::File)
    );
    assert_eq!(filetype(&object(&mut server, b"mystery")), 0xFFD, "Data");
    assert_eq!(object(&mut server, b"Sub").kind, Kind::Directory);
    assert_eq!(object(&mut server, b"Gone"), Object::NOTHING);
    assert_eq!(object(&mut server, b"").kind, Kind::Directory, "the root");
    assert_eq!(
        text.attributes, 0x13,
        "the owner's read and write and the public's read"
    );
}

#[test]
fn a_name_is_found_ignoring_case_an_exact_one_first_and_never_guessed() {
    let tree = Tree::new()
        .file("/ReadMe", b"1")
        .file("/Both", b"2")
        .file("/BOTH", b"3")
        .file("/Twin", b"4")
        .file("/twin", b"5");
    let mut server = server(&tree, true);
    assert_eq!(object(&mut server, b"readme").length, 1, "case ignored");
    assert_eq!(
        read(&mut server, b"both"),
        Reply::Refused(Refusal::Ambiguous),
        "two, neither exact"
    );
    assert_eq!(
        object(&mut server, b"Twin").kind,
        Kind::File,
        "an exact match wins"
    );
    assert_eq!(read(&mut server, b"a b"), Reply::Refused(Refusal::BadName));
}

#[test]
fn a_listing_names_what_risc_os_can_in_order_a_page_at_a_time() {
    let tree = Tree::new()
        .file("/beta", b"")
        .file("/Alpha,fff", b"")
        .file("/my file", b"")
        .file("/no\u{a0}break", b"")
        .file("/wild*card", b"")
        .file("/gamma.txt", b"")
        .dir("/Delta");
    let mut server = server(&tree, true);
    assert_eq!(
        names(&mut server, b""),
        [
            &b"Alpha"[..],
            b"beta",
            b"Delta",
            b"gamma/txt",
            b"my\xA0file"
        ],
        "suffix off, a space a hard space, unholdable names left out, ordered ignoring case"
    );
    assert_eq!(
        object(&mut server, b"MY\xA0FILE").kind,
        Kind::File,
        "found by its hard space"
    );
    let Reply::Entries { next, entries } = ask(
        &mut server,
        &Request::List {
            root: 0,
            path: b"",
            offset: 1,
            count: 2,
        },
    ) else {
        panic!("a page");
    };
    assert_eq!((next, entries.len()), (3, 2));
    let Reply::Entries { next, .. } = ask(
        &mut server,
        &Request::List {
            root: 0,
            path: b"",
            offset: 3,
            count: 2,
        },
    ) else {
        panic!("the last page");
    };
    assert_eq!(next, u32::MAX);
}

#[test]
fn a_file_is_opened_read_written_and_restamped_as_it_closes() {
    let tree = Tree::new().file("/data", b"abcdef");
    let mut server = server(&tree, true);
    let Reply::Opened {
        handle,
        extent,
        writable,
        ..
    } = ask(
        &mut server,
        &Request::Open {
            root: 0,
            path: b"data",
            update: true,
        },
    )
    else {
        panic!("opened");
    };
    assert_eq!((extent, writable), (6, true));
    assert_eq!(
        bytes(&mut server, handle, 2, 10),
        Ok(b"cdef".to_vec()),
        "fewer bytes only at the end"
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::WriteBytes {
                handle,
                offset: 6,
                bytes: b"gh"
            }
        ),
        Reply::Wrote(2)
    );
    assert_eq!(
        ask(&mut server, &Request::SetExtent { handle, extent: 7 }),
        Reply::Done
    );
    let stamp = acorn::encode_typed(acorn::Filetype::from_low_bits(0xFFF), 0x55_0000_0000)
        .expect("a stamp");
    assert_eq!(
        ask(
            &mut server,
            &Request::Close {
                handle,
                stamp: Some(stamp)
            }
        ),
        Reply::Done
    );
    assert_eq!(tree.bytes("/data"), Some(b"abcdefg".to_vec()));
    let kept = object(&mut server, b"data");
    assert_eq!(
        (kept.load, kept.exec),
        stamp,
        "kept as the volume's attributes"
    );
    assert_eq!(
        bytes(&mut server, handle, 0, 1),
        Err(Refusal::Unknown),
        "a closed handle reaches nothing"
    );
    assert_eq!(
        bytes(&mut server, handle ^ 0x100, 0, 1),
        Err(Refusal::Unknown),
        "nor one of another session"
    );
}

#[test]
fn a_read_only_root_refuses_every_change() {
    let tree = Tree::new().file("/data", b"x");
    let mut server = server(&tree, false);
    assert_eq!(
        ask(
            &mut server,
            &Request::Open {
                root: 0,
                path: b"data",
                update: true
            }
        ),
        Reply::Refused(Refusal::WriteProtected)
    );
    for request in [
        Request::Create {
            root: 0,
            path: b"New",
            load: 0,
            exec: 0,
            length: 0,
        },
        Request::CreateDir {
            root: 0,
            path: b"Dir",
            load: 0,
            exec: 0,
        },
        Request::WriteInfo {
            root: 0,
            path: b"data",
            load: 0,
            exec: 0,
            attributes: 0,
        },
        Request::Delete {
            root: 0,
            path: b"data",
        },
        Request::Rename {
            root: 0,
            from: b"data",
            to: b"other",
        },
    ] {
        assert_eq!(
            ask(&mut server, &request),
            Reply::Refused(Refusal::WriteProtected),
            "{request:?}"
        );
    }
    assert_eq!(tree.names("/"), ["data"]);
}

#[test]
fn a_volume_that_keeps_no_risc_os_metadata_keeps_a_type_as_a_suffix() {
    let tree = Tree::new()
        .refuses_attr("acorn.loadaddr", Errno::NotSupported)
        .dir("/d");
    let mut server = server(&tree, true);
    let typed = acorn::encode_typed(acorn::Filetype::from_low_bits(0xFF9), 0x55_0000_0000)
        .expect("a stamp");
    assert_eq!(
        ask(
            &mut server,
            &Request::Create {
                root: 0,
                path: b"Pic",
                load: typed.0,
                exec: typed.1,
                length: 4
            }
        ),
        Reply::Done
    );
    assert!(tree.exists("/Pic,ff9"), "{:?}", tree.names("/"));
    assert_eq!(filetype(&object(&mut server, b"Pic")), 0xFF9);
    let text = acorn::encode_typed(acorn::Filetype::from_low_bits(0xFFF), 0x55_0000_0000)
        .expect("a stamp");
    assert_eq!(
        ask(
            &mut server,
            &Request::Create {
                root: 0,
                path: b"note/txt",
                load: text.0,
                exec: text.1,
                length: 0
            }
        ),
        Reply::Done
    );
    assert!(
        tree.exists("/note.txt"),
        "a name that already says its type keeps no suffix"
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Create {
                root: 0,
                path: b"Code",
                load: 0x8000,
                exec: 0x8000,
                length: 0
            }
        ),
        Reply::Refused(Refusal::Unrepresentable),
        "an untyped stamp nothing keeps is refused, never dropped"
    );
}

#[test]
fn objects_are_deleted_and_renamed_as_risc_os_asks() {
    let tree = Tree::new()
        .file("/A", b"a")
        .file("/Locked", b"l")
        .attr("/Locked", "acorn.attr", b"LWR/")
        .file("/Full/inside", b"i")
        .file("/Pic,ff9", b"p");
    let mut server = server(&tree, true);
    assert_eq!(
        ask(
            &mut server,
            &Request::Delete {
                root: 0,
                path: b"Full"
            }
        ),
        Reply::Refused(Refusal::NotEmpty)
    );
    let Reply::Object(gone) = ask(
        &mut server,
        &Request::Delete {
            root: 0,
            path: b"a",
        },
    ) else {
        panic!("the deleted object");
    };
    assert_eq!((gone.kind, gone.length), (Kind::File, 1));
    assert!(!tree.exists("/A"));
    assert_eq!(
        ask(
            &mut server,
            &Request::Delete {
                root: 0,
                path: b"Gone"
            }
        ),
        Reply::Object(Object::NOTHING)
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Delete {
                root: 0,
                path: b"Locked"
            }
        ),
        Reply::Refused(Refusal::Locked)
    );
    assert!(tree.exists("/Locked"));
    assert_eq!(
        ask(
            &mut server,
            &Request::Rename {
                root: 0,
                from: b"Pic",
                to: b"Full"
            }
        ),
        Reply::Refused(Refusal::Exists)
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Rename {
                root: 0,
                from: b"Pic",
                to: b"Picture"
            }
        ),
        Reply::Done
    );
    assert!(tree.exists("/Picture,ff9"), "a renamed file keeps its type");
    assert_eq!(
        ask(
            &mut server,
            &Request::Rename {
                root: 0,
                from: b"picture",
                to: b"PICTURE"
            }
        ),
        Reply::Done
    );
    assert!(
        tree.exists("/PICTURE,ff9"),
        "a change of case alone is a rename"
    );
}

#[test]
fn a_volumes_space_is_its_own() {
    let tree = Tree::new().file("/f", &[0; 1024]);
    let mut server = server(&tree, true);
    let Reply::Space { free, total } = ask(&mut server, &Request::Space { root: 0 }) else {
        panic!("its space");
    };
    assert!(free < total, "{free} of {total}");
    assert_eq!(
        ask(&mut server, &Request::Space { root: 9 }),
        Reply::Refused(Refusal::Unknown)
    );
}

/// The version `path`'s open gives, none where its volume names none.
fn version(server: &mut Server<Handle>, path: &[u8]) -> Option<Version> {
    let Reply::Opened {
        handle, version, ..
    } = ask(
        server,
        &Request::Open {
            root: 0,
            path,
            update: false,
        },
    )
    else {
        panic!("opened {path:?}");
    };
    assert_eq!(
        ask(
            server,
            &Request::Close {
                handle,
                stamp: None
            }
        ),
        Reply::Done
    );
    version
}

#[test]
fn an_open_names_the_version_of_the_data_it_opened() {
    let tree = Tree::new().file("/data", b"one");
    let mut server = server(&tree, true);
    assert_eq!(
        version(&mut server, b"data"),
        None,
        "a file whose volume names no version of it"
    );
    tree.write("/data", b"two");
    let first = version(&mut server, b"data").expect("a version");
    assert_eq!(
        version(&mut server, b"data"),
        Some(first),
        "unchanged, the same version"
    );
    tree.write("/data", b"three");
    let second = version(&mut server, b"data").expect("a version");
    assert_ne!(first, second, "changed, another");
    assert_eq!(
        (first.volume, first.node),
        (second.volume, second.node),
        "of the one file"
    );
}

#[test]
fn a_files_attributes_decide_how_it_may_be_opened_and_saved_over() {
    let tree = Tree::new()
        .file("/Shut", b"s")
        .attr("/Shut", "acorn.attr", b"/")
        .file("/Fixed", b"f")
        .attr("/Fixed", "acorn.attr", b"LR/")
        .file("/ReadOnly", b"r")
        .attr("/ReadOnly", "acorn.attr", b"R/r");
    let mut server = server(&tree, true);
    assert_eq!(
        ask(
            &mut server,
            &Request::Open {
                root: 0,
                path: b"Shut",
                update: false
            }
        ),
        Reply::Refused(Refusal::AccessViolation),
        "no read access"
    );
    assert_eq!(
        ask(
            &mut server,
            &Request::Open {
                root: 0,
                path: b"ReadOnly",
                update: true
            }
        ),
        Reply::Refused(Refusal::AccessViolation),
        "no write access"
    );
    let typed = acorn::encode_typed(acorn::Filetype::from_low_bits(0xFFF), 0x55_0000_0000)
        .expect("a stamp");
    for (path, refusal) in [
        (&b"Fixed"[..], Refusal::Locked),
        (b"ReadOnly", Refusal::AccessViolation),
    ] {
        assert_eq!(
            ask(
                &mut server,
                &Request::Create {
                    root: 0,
                    path,
                    load: typed.0,
                    exec: typed.1,
                    length: 0
                }
            ),
            Reply::Refused(refusal),
            "{path:?}"
        );
    }
    assert_eq!(
        ask(
            &mut server,
            &Request::Rename {
                root: 0,
                from: b"Fixed",
                to: b"Moved"
            }
        ),
        Reply::Refused(Refusal::Locked)
    );
    assert_eq!(
        (tree.bytes("/Fixed"), tree.bytes("/ReadOnly")),
        (Some(b"f".to_vec()), Some(b"r".to_vec()))
    );
}

#[test]
fn a_save_nothing_can_keep_leaves_the_file_it_would_replace_and_makes_none() {
    let tree = Tree::new()
        .refuses_attr("acorn.loadaddr", Errno::NotSupported)
        .file("/Code", b"precious");
    let mut server = server(&tree, true);
    for path in [&b"Code"[..], b"Fresh"] {
        assert_eq!(
            ask(
                &mut server,
                &Request::Create {
                    root: 0,
                    path,
                    load: 0x8000,
                    exec: 0x8000,
                    length: 4
                }
            ),
            Reply::Refused(Refusal::Unrepresentable),
            "{path:?}"
        );
    }
    assert_eq!(
        tree.bytes("/Code"),
        Some(b"precious".to_vec()),
        "not emptied"
    );
    assert_eq!(tree.names("/"), ["Code"], "nothing made");
}
