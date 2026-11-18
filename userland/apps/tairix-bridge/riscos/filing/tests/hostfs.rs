//! HostFS through FileSwitch: a session whose roots are served in-process,
//! over the fixture tree, reached by the SWIs a program calls.

mod common;

use common::served::{session, session_with, Relay};
use common::{block, call, info, load, read, save, text};

use std::cell::Cell;
use std::rc::Rc;

use tairix_fscopy::fixture::Tree;
use tairix_fscopy::tree::Roots;
use tairix_riscos_filing::hostfs::server::{Given, Server};
use tairix_riscos_filing::hostfs::{self, protocol::Request};
use tairix_riscos_kernel::os::Os;
use tairix_riscos_kernel::swi::os::{
    fs_control, ARGS, BPUT, FILE, FIND, FS_CONTROL, GBPB, READ_VAR_VAL,
};

/// `name` canonicalised.
fn canonical(os: &mut Os<'_>, name: &[u8]) -> Vec<u8> {
    let name = text(os, name);
    let buffer = os.rma_claim(256).expect("room");
    call(
        os,
        FS_CONTROL,
        &[fs_control::CANONICALISE, name, buffer, 0, 0, 256],
    )
    .expect("canonical");
    os.read_zstring(buffer, 256).expect("its name")
}

#[test]
fn the_users_own_disc_is_the_sessions_home() {
    let tree = Tree::new().dir("/home").dir("/apps");
    let mut session = session_with(
        &tree,
        Some(("/home", "RISC OS")),
        &[("/apps", "Apps", false)],
    );
    let mut os = session.kernel.os();
    assert_eq!(
        hostfs::home(&mut os).ok(),
        Some((b"HostFS::RISC\xA0OS.$".to_vec(), true))
    );
    assert_eq!(
        info(&mut os, b"HostFS::Choices.$").map_err(|(number, _)| number),
        Err(0x1_99D4),
        "a session with a disc of its own keeps no choices in memory"
    );
}

#[test]
fn a_session_with_no_disc_of_its_own_keeps_choices_on_a_memory_disc() {
    let tree = Tree::new().dir("/apps");
    let mut session = session(&tree, &[("/apps", "Apps", false)]);
    let mut os = session.kernel.os();
    assert_eq!(
        hostfs::home(&mut os).ok(),
        Some((b"HostFS::Choices.$".to_vec(), false))
    );
    save(&mut os, b"HostFS::Choices.$.App", 0xFFF, b"chosen").expect("written to memory");
    assert_eq!(
        load(&mut os, b"HostFS::Choices.$.App", 6).ok().as_deref(),
        Some(&b"chosen"[..])
    );
    assert_eq!(
        tree.names("/"),
        ["apps"],
        "nothing reached the session's roots"
    );
}

#[test]
fn each_root_and_scrap_and_transfer_are_discs_of_hostfs() {
    let tree = Tree::new().dir("/docs").dir("/apps");
    let mut session = session(
        &tree,
        &[("/docs", "RISC OS", true), ("/apps", "Apps", false)],
    );
    let mut os = session.kernel.os();
    assert_eq!(
        canonical(&mut os, b"HostFS:$.x"),
        b"HostFS::RISC\xA0OS.$.x",
        "the first root is the default"
    );
    assert_eq!(
        canonical(&mut os, b"HostFS::apps.$"),
        b"HostFS::Apps.$",
        "a disc named ignoring case"
    );
    for disc in [&b"Scrap"[..], b"Transfer"] {
        let mut name = b"HostFS::".to_vec();
        name.extend_from_slice(disc);
        name.extend_from_slice(b".$");
        assert_eq!(info(&mut os, &name).map(|info| info.0), Ok(2), "{disc:?}");
    }
    let variable = text(&mut os, b"Wimp$ScrapDir");
    let value = os.rma_claim(64).expect("room");
    let regs = call(&mut os, READ_VAR_VAL, &[variable, value, 64, 0, 0]).expect("set");
    assert_eq!(
        read(&mut os, value, regs.r[2] as usize),
        b"HostFS::Scrap.$",
        "Wimp$ScrapDir names Scrap"
    );
    let missing = info(&mut os, b"HostFS::Nowhere.$");
    assert!(missing.is_err(), "no such disc: {missing:?}");
}

#[test]
fn a_file_saved_through_hostfs_is_the_hosts_and_loads_back() {
    let tree = Tree::new()
        .dir("/docs")
        .file("/docs/Prog,ffb", b"\r\x00\x0a\x05\xf1\r\xff");
    let mut session = session(&tree, &[("/docs", "Docs", true)]);
    let mut os = session.kernel.os();
    save(&mut os, b"HostFS:$.Letter", 0xFFF, b"Dear all").expect("saved");
    assert_eq!(tree.bytes("/docs/Letter"), Some(b"Dear all".to_vec()));
    let (kind, load_word, _, length) = info(&mut os, b"HostFS:$.Letter").expect("its info");
    assert_eq!((kind, (load_word >> 8) & 0xFFF, length), (1, 0xFFF, 8));
    assert_eq!(
        load(&mut os, b"HostFS:$.letter", 8),
        Ok(b"Dear all".to_vec()),
        "found ignoring case"
    );
    let (_, prog, _, _) = info(&mut os, b"HostFS:$.Prog").expect("the program");
    assert_eq!((prog >> 8) & 0xFFF, 0xFFB, "typed by its suffix");
}

#[test]
fn a_stream_writes_extends_and_reads_back_a_host_file() {
    let tree = Tree::new().dir("/docs");
    let mut session = session(&tree, &[("/docs", "Docs", true)]);
    let mut os = session.kernel.os();
    let name = text(&mut os, b"HostFS:$.Log");
    let handle = call(&mut os, FIND, &[0x80, name]).expect("opened").r[0];
    let data: Vec<u8> = (0..5000u32).map(|n| (n % 251) as u8).collect();
    let from = block(&mut os, &data);
    call(&mut os, GBPB, &[2, handle, from, 5000]).expect("written");
    call(&mut os, BPUT, &[u32::from(b'!'), handle]).expect("put");
    call(&mut os, ARGS, &[3, handle, 6000]).expect("extended");
    call(&mut os, FIND, &[0, handle]).expect("closed");
    let mut expected = data.clone();
    expected.push(b'!');
    expected.resize(6000, 0);
    assert_eq!(tree.bytes("/docs/Log"), Some(expected.clone()));
    let handle = call(&mut os, FIND, &[0x40, name]).expect("opened").r[0];
    assert_eq!(
        call(&mut os, ARGS, &[2, handle]).expect("its extent").r[2],
        6000
    );
    let to = os.rma_claim(6000).expect("room");
    call(&mut os, GBPB, &[4, handle, to, 6000]).expect("read");
    call(&mut os, FIND, &[0, handle]).expect("closed");
    assert_eq!(read(&mut os, to, 6000), expected);
}

#[test]
fn a_directory_is_listed_and_its_objects_renamed_copied_and_wiped() {
    let tree = Tree::new()
        .file("/docs/Alpha", b"a")
        .file("/docs/Read Me.txt", b"r")
        .file("/docs/Dir/Inside", b"i");
    let mut session = session(&tree, &[("/docs", "Docs", true)]);
    let mut os = session.kernel.os();
    let dir = text(&mut os, b"HostFS:$");
    let buffer = os.rma_claim(512).expect("room");
    let regs = call(&mut os, GBPB, &[9, dir, buffer, 8, 0, 512, 0]).expect("listed");
    assert_eq!(regs.r[3], 3);
    let names: Vec<Vec<u8>> = read(&mut os, buffer, 512)
        .split(|&byte| byte == 0)
        .take(3)
        .map(<[u8]>::to_vec)
        .collect();
    assert_eq!(
        names,
        [&b"Alpha"[..], b"Dir", b"Read\xA0Me/txt"],
        "a space a hard space"
    );
    let (from, to) = (
        text(&mut os, b"HostFS:$.Alpha"),
        text(&mut os, b"HostFS:$.Beta"),
    );
    call(&mut os, FS_CONTROL, &[fs_control::RENAME, from, to]).expect("renamed");
    assert!(tree.exists("/docs/Beta") && !tree.exists("/docs/Alpha"));
    let copy = text(&mut os, b"HostFS:$.Gamma");
    call(&mut os, FS_CONTROL, &[fs_control::COPY, to, copy, 0]).expect("copied");
    assert_eq!(tree.bytes("/docs/Gamma"), Some(b"a".to_vec()));
    let wiped = text(&mut os, b"HostFS:$.Dir");
    call(&mut os, FS_CONTROL, &[fs_control::WIPE, wiped, 0, 1]).expect("wiped");
    assert!(!tree.exists("/docs/Dir"));
}

#[test]
fn hostfs_numbers_its_own_errors_beside_its_filing_system() {
    let tree = Tree::new().dir("/docs");
    let mut session = session(&tree, &[("/docs", "Docs", true)]);
    let mut os = session.kernel.os();
    assert_eq!(
        info(&mut os, b"HostFS::Nowhere.$").map_err(|(number, _)| number),
        Err(0x1_99D4),
        "disc not found, as RPCEmu's HostFS numbers it under RISC OS 5"
    );
    assert_eq!(
        info(&mut os, b"HostFS:$.Missing").map(|(kind, ..)| kind),
        Ok(0),
        "nothing there is FileSwitch's answer, not an error of HostFS's"
    );
}

#[test]
fn a_root_the_session_may_not_write_refuses_every_change() {
    let tree = Tree::new().file("/apps/App", b"x");
    let mut session = session(&tree, &[("/apps", "Apps", false)]);
    let mut os = session.kernel.os();
    assert!(save(&mut os, b"HostFS:$.New", 0xFFF, b"n").is_err());
    let name = text(&mut os, b"HostFS:$.App");
    assert!(
        call(&mut os, FIND, &[0xC0, name]).is_err(),
        "opened for update"
    );
    assert!(call(&mut os, FILE, &[6, name]).is_err(), "deleted");
    assert_eq!(tree.names("/apps"), ["App"]);
    assert_eq!(
        load(&mut os, b"HostFS:$.App", 1),
        Ok(b"x".to_vec()),
        "still read"
    );
}

#[test]
fn scrap_is_held_in_the_sessions_own_memory() {
    let tree = Tree::new().dir("/docs");
    let mut session = session(&tree, &[("/docs", "Docs", true)]);
    let mut os = session.kernel.os();
    let data: Vec<u8> = (0..10_000u32).map(|n| (n % 241) as u8).collect();
    save(&mut os, b"HostFS::Scrap.$.Handed", 0xFFD, &data).expect("saved");
    assert_eq!(
        load(&mut os, b"HostFS::Scrap.$.Handed", data.len()),
        Ok(data)
    );
    assert_eq!(session.reads.get(), 0, "nothing crossed the relay");
    assert!(tree.names("/docs").is_empty(), "nor reached the host");
    let name = text(&mut os, b"HostFS::Scrap.$.Handed");
    call(&mut os, FILE, &[6, name]).expect("deleted");
    assert_eq!(
        info(&mut os, b"HostFS::Scrap.$.Handed").map(|info| info.0),
        Ok(0)
    );
}

#[test]
fn a_file_read_again_comes_from_the_read_cache_until_it_changes() {
    let tree = Tree::new().file("/docs/Big", b"");
    let first: Vec<u8> = (0..10_000u32).map(|n| (n % 239) as u8).collect();
    tree.write("/docs/Big", &first);
    let mut session = session(&tree, &[("/docs", "Docs", true)]);
    let mut os = session.kernel.os();
    assert_eq!(
        load(&mut os, b"HostFS:$.Big", first.len()),
        Ok(first.clone())
    );
    let fetched = session.reads.get();
    assert_eq!(fetched, 3, "a block at a time");
    assert_eq!(load(&mut os, b"HostFS:$.Big", first.len()), Ok(first));
    assert_eq!(session.reads.get(), fetched, "the version read already");
    let second = vec![7; 10_000];
    tree.write("/docs/Big", &second);
    assert_eq!(
        load(&mut os, b"HostFS:$.Big", second.len()),
        Ok(second),
        "never a version gone"
    );
    assert_eq!(session.reads.get(), fetched + 3);
}

#[test]
fn a_delivered_file_is_read_from_transfer() {
    let tree = Tree::new().file("/drop/Note.txt", b"from the desktop");
    let mut session = session(&tree, &[("/drop", "Docs", true)]);
    let file = tree
        .open_file("/drop/Note.txt", false, false)
        .expect("the dropped file");
    let path = session
        .server
        .borrow_mut()
        .deliver("Note.txt", file, false)
        .expect("delivered");
    assert_eq!(path, b"HostFS::Transfer.$.1.Note/txt");
    let mut os = session.kernel.os();
    let (kind, load_word, _, length) = info(&mut os, &path).expect("its info");
    assert_eq!((kind, (load_word >> 8) & 0xFFF, length), (1, 0xFFF, 16));
    assert_eq!(load(&mut os, &path, 16), Ok(b"from the desktop".to_vec()));
    assert!(
        save(&mut os, &path, 0xFFF, b"over").is_err(),
        "not delivered writable"
    );
    assert_eq!(
        tree.bytes("/drop/Note.txt"),
        Some(b"from the desktop".to_vec())
    );
}

#[test]
fn a_relay_that_answers_wrongly_fails_the_call_and_the_session_goes_on() {
    let tree = Tree::new().file("/docs/File", b"f");
    let given = vec![Given {
        dir: tree.open_dir("/docs", false).expect("a root"),
        name: "Docs".to_string(),
        writable: true,
    }];
    let mut server = Server::new(None, given, [1; 16], 1);
    let mut kernel = common::kernel_on(Box::new(Relay {
        answer: move |request: &[u8], reply: &mut Vec<u8>| match Request::decode(request) {
            Some(Request::Roots) => {
                server.serve(request, reply);
                true
            }
            Some(Request::Read { .. }) => {
                reply.extend_from_slice(&[1, 9]);
                true
            }
            _ => false,
        },
    }));
    let mut os = kernel.os();
    assert!(
        info(&mut os, b"HostFS:$.File").is_err(),
        "a malformed reply"
    );
    assert!(load(&mut os, b"HostFS:$.File", 1).is_err(), "no reply");
    assert_eq!(
        info(&mut os, b"Resources:$.Resources").map(|info| info.0),
        Ok(2),
        "the session goes on"
    );
}

#[test]
fn a_session_with_no_relay_has_scrap_alone() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    assert_eq!(canonical(&mut os, b"HostFS:$"), b"HostFS::Scrap.$");
}

#[test]
fn a_file_too_large_to_buffer_is_refused_and_let_go() {
    let tree = Tree::new().file("/docs/Huge", b"h");
    let given = vec![Given {
        dir: tree.open_dir("/docs", false).expect("a root"),
        name: "Docs".to_string(),
        writable: true,
    }];
    let mut server = Server::new(None, given, [1; 16], 1);
    let (opens, closes) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let (opens_seen, closes_seen) = (Rc::clone(&opens), Rc::clone(&closes));
    let mut kernel = common::kernel_on(Box::new(Relay {
        answer: move |request: &[u8], reply: &mut Vec<u8>| {
            server.serve(request, reply);
            match Request::decode(request) {
                Some(Request::Open { .. }) => {
                    opens_seen.set(opens_seen.get() + 1);
                    // The extent, after the reply's tag and handle.
                    reply[5..9].copy_from_slice(&u32::MAX.to_le_bytes());
                }
                Some(Request::Close { .. }) => closes_seen.set(closes_seen.get() + 1),
                _ => {}
            }
            true
        },
    }));
    let mut os = kernel.os();
    let name = text(&mut os, b"HostFS:$.Huge");
    assert!(
        call(&mut os, FIND, &[0x40, name]).is_err(),
        "too large to buffer"
    );
    assert_eq!(
        (opens.get(), closes.get()),
        (1, 1),
        "the server's handle let go"
    );
}

#[test]
fn an_ensure_past_the_largest_file_answers_without_overflowing() {
    let tree = Tree::new().file("/docs/Small", b"s");
    let mut session = session(&tree, &[("/docs", "Docs", true)]);
    let mut os = session.kernel.os();
    let name = text(&mut os, b"HostFS:$.Small");
    let handle = call(&mut os, FIND, &[0xC0, name]).expect("opened").r[0];
    let ensured = call(&mut os, ARGS, &[6, handle, u32::MAX - 1]).expect("ensured");
    assert!(
        ensured.r[2] >= u32::MAX - 1,
        "at least what was asked: {:#x}",
        ensured.r[2]
    );
    call(&mut os, FIND, &[0, handle]).expect("closed");
}
