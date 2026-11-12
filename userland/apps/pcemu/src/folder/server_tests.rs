use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{Server, FILES};
use crate::folder::memhost::{MemDir, MemHost};
use crate::folder::tests::{name, path};
use crate::folder::{wanted, Access, Creation, Facts, Folder, From, Op, Refusal, Reply};

const EPOCH: u32 = 0x00AB_CDEF;

fn serve(host: &MemHost, writable: bool) -> Server<MemDir> {
    Server::new(host.root(), name("SHARED"), writable, EPOCH)
}

fn look(server: &mut Server<MemDir>, at: &str) -> Result<Facts, Refusal> {
    match server.call(&Op::Look { path: path(at) })? {
        Reply::Looked(facts) => Ok(facts),
        other => panic!("not what a look answers: {other:?}"),
    }
}

/// Every name a search of `dir` for `pattern` finds, followed to its end.
fn found(
    server: &mut Server<MemDir>,
    dir: &str,
    pattern: &str,
    attributes: u8,
) -> Vec<(String, Facts)> {
    let mut found = Vec::new();
    let mut from = From::Start { dir: path(dir) };
    loop {
        let op = Op::Find {
            pattern: name(pattern),
            attributes,
            from,
        };
        match server.call(&op) {
            Ok(Reply::Found { facts, table, next }) => {
                let mut spelled = [0u8; 13];
                let len = crate::dos::name::spell(facts.name.bytes(), &mut spelled);
                found.push((
                    String::from(core::str::from_utf8(&spelled[..len]).expect("ASCII")),
                    facts,
                ));
                from = From::Next { table, index: next };
            }
            Err(Refusal::NoMore) => return found,
            other => panic!("a search answers an entry or no more: {other:?}"),
        }
    }
}

fn names(found: &[(String, Facts)]) -> Vec<&str> {
    found.iter().map(|(name, _)| name.as_str()).collect()
}

fn open(
    server: &mut Server<MemDir>,
    at: &str,
    access: Access,
    creation: Creation,
) -> Result<(u32, Facts), Refusal> {
    match server.call(&Op::Open {
        path: path(at),
        access,
        creation,
    })? {
        Reply::Opened { handle, facts, .. } => Ok((handle, facts)),
        other => panic!("not what an open answers: {other:?}"),
    }
}

fn read(server: &mut Server<MemDir>, handle: u32, offset: u32, len: u32) -> Vec<u8> {
    match server.call(&Op::Read {
        handle,
        offset,
        len,
    }) {
        Ok(Reply::Read(bytes)) => bytes.to_vec(),
        other => panic!("not what a read answers: {other:?}"),
    }
}

#[test]
fn the_root_is_the_share_labelled_and_a_node_is_what_the_host_says() {
    let host = MemHost::new()
        .file("/README.TXT", b"hello")
        .dir("/GAMES")
        .file("/.hidden", b"x");
    let mut server = serve(&host, true);
    let root = look(&mut server, "").expect("the root");
    assert!(root.directory && root.writable);
    assert_eq!(root.name, name("SHARED"));
    let file = look(&mut server, "\\README.TXT").expect("the file");
    assert_eq!((file.directory, file.size, file.hidden), (false, 5, false));
    assert_eq!(file.modified, host.modified("/README.TXT").expect("held"));
    assert!(
        look(&mut server, "\\GAMES")
            .expect("the directory")
            .directory
    );
    assert!(look(&mut server, "\\HIDDEN~1").expect("the dotfile").hidden);
    assert_eq!(look(&mut server, "\\NONE.TXT"), Err(Refusal::NotFound));
    assert_eq!(
        look(&mut server, "\\NONE\\A.TXT"),
        Err(Refusal::PathNotFound)
    );
    assert_eq!(
        look(&mut server, "\\README.TXT\\A"),
        Err(Refusal::PathNotFound),
        "a file is no directory on the way"
    );
}

#[test]
fn a_search_finds_each_matching_entry_once_by_a_stable_name_and_kind() {
    let host = MemHost::new()
        .file("/Long File Name.txt", b"1")
        .file("/Long File Number Two.txt", b"22")
        .file("/readme.txt", b"333")
        .file("/.config", b"")
        .dir("/Docs");
    let mut server = serve(&host, true);
    let plain = found(&mut server, "", "????????.???", 0);
    assert_eq!(
        names(&plain),
        ["LONGFI~1.TXT", "LONGFI~2.TXT", "README.TXT"],
        "a directory and a hidden file only where asked for"
    );
    let all = found(
        &mut server,
        "",
        "????????.???",
        wanted::DIRECTORY | wanted::HIDDEN,
    );
    assert_eq!(
        names(&all),
        [
            "CONFIG~1",
            "DOCS",
            "LONGFI~1.TXT",
            "LONGFI~2.TXT",
            "README.TXT"
        ]
    );
    assert_eq!(names(&found(&mut server, "", "*.TXT", 0)), names(&plain));
    assert_eq!(
        names(&found(&mut server, "", "README.*", 0)),
        ["README.TXT"]
    );
    assert_eq!(
        found(&mut server, "", "????????.???", 0),
        plain,
        "the same entries go by the same names"
    );
    assert_eq!(plain[2].1.size, 3);
    assert_eq!(found(&mut server, "\\DOCS", "????????.???", 0), Vec::new());
    assert_eq!(
        names(&found(
            &mut server,
            "\\DOCS",
            "????????.???",
            wanted::DIRECTORY
        )),
        [".", ".."],
        "a directory below the root lists itself and its parent"
    );
    assert_eq!(
        names(&found(&mut server, "", "????????.???", wanted::DIRECTORY))[0],
        "DOCS",
        "the root does not"
    );
    let stale = server.call(&Op::Find {
        pattern: name("*.*"),
        attributes: 0,
        from: From::Next {
            table: 999,
            index: 0,
        },
    });
    assert_eq!(
        stale,
        Err(Refusal::NoMore),
        "a listing no longer kept finds no more"
    );
}

#[test]
fn an_entry_made_elsewhere_is_found_and_a_name_keeps_its_alias_beside_it() {
    let host = MemHost::new().file("/Long File Name.txt", b"1");
    let mut server = serve(&host, true);
    assert_eq!(names(&found(&mut server, "", "*.*", 0)), ["LONGFI~1.TXT"]);
    host.put("/Long File Again.txt", b"2");
    assert_eq!(
        names(&found(&mut server, "", "*.*", 0)),
        ["LONGFI~1.TXT", "LONGFI~2.TXT"],
        "the older name keeps its alias though the newer sorts first"
    );
    host.put("/later.txt", b"3");
    assert!(
        look(&mut server, "\\LATER.TXT").is_ok(),
        "a lookup reads afresh"
    );
    let lists = host.lists();
    assert_eq!(look(&mut server, "\\NOTHERE.TXT"), Err(Refusal::NotFound));
    assert_eq!(
        host.lists(),
        lists,
        "an unchanged directory is not read again"
    );
}

#[test]
fn a_search_under_way_keeps_its_place_as_entries_go_beside_it() {
    let host = MemHost::new()
        .file("/A.TXT", b"a")
        .file("/B.TXT", b"b")
        .file("/C.TXT", b"c");
    let mut server = serve(&host, true);
    let first = server.call(&Op::Find {
        pattern: name("*.*"),
        attributes: 0,
        from: From::Start { dir: path("") },
    });
    let Ok(Reply::Found { facts, table, next }) = first else {
        panic!("the first entry: {first:?}");
    };
    assert_eq!(facts.name, name("A.TXT"));
    assert_eq!(
        server.call(&Op::Remove {
            path: path("\\B.TXT"),
            directory: false,
        }),
        Ok(Reply::Done)
    );
    let second = server.call(&Op::Find {
        pattern: name("*.*"),
        attributes: 0,
        from: From::Next { table, index: next },
    });
    assert!(
        matches!(second, Ok(Reply::Found { facts, .. }) if facts.name == name("C.TXT")),
        "{second:?}"
    );
}

#[test]
fn an_entry_renamed_in_its_directory_keeps_its_place_and_one_moved_takes_a_new_one() {
    let host = MemHost::new()
        .file("/A.TXT", b"a")
        .file("/B.TXT", b"b")
        .dir("/SUB");
    let mut server = serve(&host, true);
    let rename = |server: &mut Server<MemDir>, from: &str, to: &str| {
        server
            .call(&Op::Rename {
                from: path(from),
                to: path(to),
            })
            .map(|reply| reply == Reply::Done)
    };
    assert_eq!(rename(&mut server, "\\A.TXT", "\\Z.TXT"), Ok(true));
    assert_eq!(
        names(&found(&mut server, "", "*.*", wanted::DIRECTORY)),
        ["Z.TXT", "B.TXT", "SUB"],
        "renamed where it lay, as a FAT directory renames an entry"
    );
    assert_eq!(rename(&mut server, "\\B.TXT", "\\SUB\\B.TXT"), Ok(true));
    assert_eq!(names(&found(&mut server, "SUB", "*.*", 0)), ["B.TXT"]);
    assert_eq!(names(&found(&mut server, "", "*.*", 0)), ["Z.TXT"]);
    assert_eq!(
        rename(&mut server, "\\Z.TXT", "\\SUB\\B.TXT"),
        Err(Refusal::Denied),
        "a name taken is never replaced"
    );
}

#[test]
fn a_file_is_made_written_read_cut_and_stamped() {
    let host = MemHost::new();
    let mut server = serve(&host, true);
    let (handle, facts) =
        open(&mut server, "\\NEW.TXT", Access::Both, Creation::Truncate).expect("made");
    assert_eq!(handle >> 8, EPOCH);
    assert_eq!((facts.size, facts.name), (0, name("NEW.TXT")));
    assert_eq!(
        server.call(&Op::Write {
            handle,
            offset: 0,
            bytes: b"hello, world",
        }),
        Ok(Reply::Wrote(12))
    );
    assert_eq!(read(&mut server, handle, 7, 100), b"world");
    assert_eq!(
        server.call(&Op::Resize { handle, size: 5 }),
        Ok(Reply::Done)
    );
    assert_eq!(server.call(&Op::Size { handle }), Ok(Reply::Size(5)));
    let stamp = Time64::from_secs(900_000_000);
    assert_eq!(
        server.call(&Op::Close {
            handle,
            stamp: Some(stamp),
        }),
        Ok(Reply::Done)
    );
    assert_eq!(host.bytes("/NEW.TXT").as_deref(), Some(&b"hello"[..]));
    assert_eq!(host.modified("/NEW.TXT"), Some(stamp));
    assert_eq!(
        server.call(&Op::Read {
            handle,
            offset: 0,
            len: 1,
        }),
        Err(Refusal::Handle),
        "closed"
    );
    assert_eq!(
        open(&mut server, "\\NEW.TXT", Access::Write, Creation::New),
        Err(Refusal::Exists)
    );
    let (handle, _) =
        open(&mut server, "\\NEW.TXT", Access::Write, Creation::Truncate).expect("emptied");
    assert_eq!(host.bytes("/NEW.TXT").as_deref(), Some(&b""[..]));
    assert_eq!(
        server.call(&Op::Read {
            handle,
            offset: 0,
            len: 4,
        }),
        Err(Refusal::Denied),
        "a handle open for writing alone reads nothing"
    );
}

#[test]
fn a_read_only_share_and_a_locked_file_refuse_every_change() {
    let host = MemHost::new()
        .file("/FILE.TXT", b"data")
        .file("/LOCKED.TXT", b"keep")
        .locked("/LOCKED.TXT")
        .dir("/D");
    let mut server = serve(&host, false);
    assert!(!look(&mut server, "").expect("the root").writable);
    assert!(!look(&mut server, "\\FILE.TXT").expect("the file").writable);
    for op in [
        Op::Open {
            path: path("\\FILE.TXT"),
            access: Access::Write,
            creation: Creation::Existing,
        },
        Op::Open {
            path: path("\\NEW.TXT"),
            access: Access::Read,
            creation: Creation::Truncate,
        },
        Op::Remove {
            path: path("\\FILE.TXT"),
            directory: false,
        },
        Op::Rename {
            from: path("\\FILE.TXT"),
            to: path("\\OTHER.TXT"),
        },
        Op::MakeDir { path: path("\\E") },
    ] {
        assert_eq!(server.call(&op), Err(Refusal::Denied), "{op:?}");
    }
    let (handle, _) =
        open(&mut server, "\\FILE.TXT", Access::Read, Creation::Existing).expect("read");
    assert_eq!(
        server.call(&Op::Write {
            handle,
            offset: 0,
            bytes: b"x",
        }),
        Err(Refusal::Denied)
    );
    let mut writable = serve(&host, true);
    assert!(
        !look(&mut writable, "\\LOCKED.TXT")
            .expect("the file")
            .writable
    );
    assert_eq!(
        open(
            &mut writable,
            "\\LOCKED.TXT",
            Access::Both,
            Creation::Existing
        ),
        Err(Refusal::Denied)
    );
    assert_eq!(host.bytes("/FILE.TXT").as_deref(), Some(&b"data"[..]));
}

#[test]
fn entries_are_removed_renamed_and_made_as_dos_asks() {
    let host = MemHost::new()
        .file("/A.TXT", b"a")
        .file("/D/INSIDE.TXT", b"i")
        .dir("/EMPTY");
    let mut server = serve(&host, true);
    assert_eq!(
        server.call(&Op::Remove {
            path: path("\\D"),
            directory: true,
        }),
        Err(Refusal::NotEmpty)
    );
    assert_eq!(
        server.call(&Op::Remove {
            path: path("\\D"),
            directory: false,
        }),
        Err(Refusal::Denied),
        "a directory is not deleted as a file"
    );
    assert_eq!(
        server.call(&Op::Remove {
            path: path("\\A.TXT"),
            directory: true,
        }),
        Err(Refusal::PathNotFound),
        "a file is not removed as a directory"
    );
    assert_eq!(
        server.call(&Op::Remove {
            path: path("\\EMPTY"),
            directory: true,
        }),
        Ok(Reply::Done)
    );
    assert_eq!(
        server.call(&Op::Rename {
            from: path("\\A.TXT"),
            to: path("\\D\\INSIDE.TXT"),
        }),
        Err(Refusal::Denied),
        "a taken name"
    );
    assert_eq!(
        server.call(&Op::Rename {
            from: path("\\A.TXT"),
            to: path("\\D\\MOVED.TXT"),
        }),
        Ok(Reply::Done)
    );
    assert_eq!(host.names("/D"), ["INSIDE.TXT", "MOVED.TXT"]);
    assert!(look(&mut server, "\\D\\MOVED.TXT").is_ok());
    assert_eq!(look(&mut server, "\\A.TXT"), Err(Refusal::NotFound));
    assert_eq!(
        server.call(&Op::MakeDir {
            path: path("\\D\\SUB"),
        }),
        Ok(Reply::Done)
    );
    assert_eq!(
        server.call(&Op::MakeDir {
            path: path("\\D\\SUB"),
        }),
        Err(Refusal::Denied),
        "a directory already there"
    );
    assert!(look(&mut server, "\\D\\SUB").expect("made").directory);
}

#[test]
fn a_link_is_reached_as_what_it_names_and_a_dangling_one_is_passed_over() {
    let host = MemHost::new()
        .file("/data/real.txt", b"through")
        .link("/alias.txt", "/data/real.txt")
        .link("/folder", "/data")
        .link("/broken", "/nowhere");
    let mut server = serve(&host, true);
    let (handle, facts) =
        open(&mut server, "\\ALIAS.TXT", Access::Read, Creation::Existing).expect("followed");
    assert_eq!(facts.size, 7);
    assert_eq!(read(&mut server, handle, 0, 16), b"through");
    assert!(look(&mut server, "\\FOLDER\\REAL.TXT").is_ok());
    assert_eq!(
        names(&found(&mut server, "", "*.*", wanted::DIRECTORY)),
        ["ALIAS.TXT", "DATA", "FOLDER"]
    );
}

#[test]
fn handles_are_the_shares_own_and_bounded() {
    let host = MemHost::new().file("/F.TXT", b"f");
    let mut server = serve(&host, true);
    let (handle, _) =
        open(&mut server, "\\F.TXT", Access::Read, Creation::Existing).expect("opened");
    let foreign = (handle & 0xFF) | (EPOCH + 1) << 8;
    assert_eq!(
        server.call(&Op::Commit { handle: foreign }),
        Err(Refusal::Handle),
        "another share's"
    );
    assert_eq!(server.call(&Op::Commit { handle }), Ok(Reply::Done));
    let mut held = alloc::vec![handle];
    while held.len() < FILES {
        let (handle, _) =
            open(&mut server, "\\F.TXT", Access::Read, Creation::Existing).expect("opened");
        held.push(handle);
    }
    assert_eq!(
        open(&mut server, "\\F.TXT", Access::Read, Creation::Existing),
        Err(Refusal::TooMany)
    );
    assert_eq!(
        server.call(&Op::Close {
            handle: held[3],
            stamp: None,
        }),
        Ok(Reply::Done)
    );
    assert!(open(&mut server, "\\F.TXT", Access::Read, Creation::Existing).is_ok());
}

#[test]
fn times_are_on_the_machines_clock_both_ways() {
    let host = MemHost::new().file("/F.TXT", b"f");
    let mut server = serve(&host, true);
    let held = host.modified("/F.TXT").expect("held");
    server.set_clock_offset(3600);
    let shown = look(&mut server, "\\F.TXT").expect("the file").modified;
    assert_eq!(shown.secs(), held.secs() + 3600);
    let (handle, _) =
        open(&mut server, "\\F.TXT", Access::Both, Creation::Existing).expect("opened");
    let set = Time64::from_secs(700_003_600);
    assert_eq!(
        server.call(&Op::Close {
            handle,
            stamp: Some(set),
        }),
        Ok(Reply::Done)
    );
    assert_eq!(
        host.modified("/F.TXT"),
        Some(Time64::from_secs(700_000_000))
    );
}

#[test]
fn a_full_volume_writes_nothing_more_and_space_is_the_volumes() {
    let host = MemHost::new().file("/F.TXT", &[1; 1024]);
    let mut server = serve(&host, true);
    let Ok(Reply::Space(space)) = server.call(&Op::Space) else {
        panic!("the volume's space");
    };
    assert_eq!(space.total_blocks - space.free_blocks, 2);
    let full = host.clone().full();
    let mut server = serve(&full, true);
    let (handle, _) =
        open(&mut server, "\\F.TXT", Access::Write, Creation::Existing).expect("opened");
    assert_eq!(
        server.call(&Op::Write {
            handle,
            offset: 0,
            bytes: b"more",
        }),
        Ok(Reply::Wrote(0))
    );
}
