//! ResourceFS's tests: names found as RISC OS's ResourceFS finds them, the
//! registrations and their refusals, and what they tell.

use tairix_riscos_arm::psr;

use super::{Object, DEREGISTER_FILES, REGISTER_FILES};
use crate::filing::filetype;
use crate::filing::fs::{attribute, kind, stamp};

/// The attributes every ResourceFS object has: its owner may read and write it.
const OWNER_READ_WRITE: u32 = attribute::OWNER_READ | attribute::OWNER_WRITE;
use crate::msgtrans::tests::{block, register, session, swi, BLOCK};
use crate::swi::X;
use crate::testkit::{asm, Session, CODE, DATA};

/// What `name` finds.
fn found(s: &mut Session, name: &[u8]) -> Option<Object> {
    s.kernel.os().resource(name).expect("looked up")
}

/// The text of the file `name` finds.
fn text(s: &mut Session, name: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    match found(s, name)? {
        Object::File(entry) => Some(s.read(entry.data, entry.length as usize)),
        Object::Directory { .. } => None,
    }
}

#[test]
fn the_rom_s_files_are_found_by_name_and_their_directories_implied() {
    let mut s = session();
    let kernel = text(&mut s, b"$.Resources.Kernel.Messages").expect("the kernel's file");
    assert!(kernel.starts_with(b"# The kernel's messages"));
    assert_eq!(
        text(&mut s, b"Resources.Kernel.Messages"),
        Some(kernel),
        "with or without the root"
    );
    assert!(matches!(
        found(&mut s, b"Resources.Kernel"),
        Some(Object::Directory { .. })
    ));
    assert!(matches!(
        found(&mut s, b"$"),
        Some(Object::Directory { load: 0, exec: 0 })
    ));
    assert!(matches!(found(&mut s, b""), Some(Object::Directory { .. })));
    assert_eq!(
        found(&mut s, b"Resources.Kern"),
        None,
        "a directory ends at a full stop"
    );
    assert_eq!(found(&mut s, b"Resources.Kernel.Messages.More"), None);
}

#[test]
fn without_a_territory_names_keep_their_case() {
    let mut s = session();
    s.kernel.os().world.resource_fs.upper = None;
    s.kernel.os().world.resource_fs.index = None;
    assert_eq!(found(&mut s, b"resources.kernel.messages"), None);
}

#[test]
fn the_newest_registration_wins_and_leaving_reveals_the_one_before() {
    let mut s = session();
    register(
        &mut s,
        BLOCK,
        &[("Test.File", &b"older"[..]), ("Test.Other", b"other")],
    );
    register(
        &mut s,
        BLOCK + 0x200,
        &[
            ("Test.File", &b"newer"[..]),
            ("Test.File", b"later in its block"),
        ],
    );
    assert_eq!(text(&mut s, b"Test.File"), Some(b"newer".to_vec()));
    assert_eq!(text(&mut s, b"Test.Other"), Some(b"other".to_vec()));
    swi(
        &mut s,
        DEREGISTER_FILES,
        [BLOCK + 0x200, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("deregistered");
    assert_eq!(text(&mut s, b"Test.File"), Some(b"older".to_vec()));
}

#[test]
fn a_block_registered_twice_or_never_is_refused() {
    let mut s = session();
    register(&mut s, BLOCK, &[("Test.File", b"x")]);
    let twice = swi(
        &mut s,
        REGISTER_FILES,
        [BLOCK, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        twice.expect_err("already"),
        (
            0x1_2E00,
            b"ResourceFS has these files registered already".to_vec()
        )
    );
    let never = swi(
        &mut s,
        DEREGISTER_FILES,
        [BLOCK + 0x400, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        never.expect_err("not registered"),
        (
            0x1_2E01,
            b"ResourceFS has no such files registered".to_vec()
        )
    );
    let beyond = swi(&mut s, super::CHUNK + 2, [0; 13]);
    assert_eq!(
        beyond.expect_err("no such SWI"),
        (0x110, b"Module ResourceFS provides no such SWI".to_vec())
    );
}

#[test]
fn wildcards_find_the_first_name_they_match() {
    let mut s = session();
    register(
        &mut s,
        BLOCK,
        &[
            ("Test.Alpha.Data", &b"alpha"[..]),
            ("Test.Beta.Data", b"beta"),
        ],
    );
    assert_eq!(text(&mut s, b"Test.B*.Data"), Some(b"beta".to_vec()));
    assert_eq!(text(&mut s, b"Test.#lpha.Data"), Some(b"alpha".to_vec()));
    assert!(matches!(
        found(&mut s, b"Test.*"),
        Some(Object::Directory { .. })
    ));
    assert_eq!(found(&mut s, b"Test.*.Nothing"), None);
}

#[test]
fn a_malformed_block_ends_where_its_entries_stop_making_sense() {
    let mut s = session();
    let mut bytes = block(&[("Test.Good", b"good")]);
    let end = bytes.len() - 4;
    // An entry too small to be one, where the next should be.
    bytes[end..end + 4].copy_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 32]);
    s.write(BLOCK, &bytes);
    swi(
        &mut s,
        REGISTER_FILES,
        [BLOCK, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("registered");
    assert_eq!(text(&mut s, b"Test.Good"), Some(b"good".to_vec()));
    assert_eq!(found(&mut s, b"Test.Bad"), None);
    // One whose offset would wrap the address space.
    let mut wrapping = block(&[("Test.Wrap", b"w")]);
    wrapping[..4].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    s.write(BLOCK + 0x400, &wrapping);
    swi(
        &mut s,
        REGISTER_FILES,
        [BLOCK + 0x400, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("registered");
    assert_eq!(
        text(&mut s, b"Test.Wrap"),
        Some(b"w".to_vec()),
        "its first entry still read"
    );
    assert_eq!(text(&mut s, b"Test.Good"), Some(b"good".to_vec()));
}

#[test]
fn a_registration_tells_the_filer_its_files_changed() {
    let mut s = session();
    let seen = DATA + 0x800;
    let code = [
        asm::mov(12, seen),
        asm::str(0, 12, 0),
        asm::str(8, 12, 4),
        asm::str(9, 12, 8),
        asm::mov_pc_lr(),
    ];
    s.poke(CODE, &code);
    let (claimed, _) = s.user_swi(X | 0x1F, [0x1D, CODE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(!claimed.flag(psr::V), "UpCallV claimed");
    register(&mut s, BLOCK, &[("Test.File", b"x")]);
    assert_eq!(
        [s.peek(seen), s.peek(seen + 4), s.peek(seen + 8)],
        [3, 0x2E | 1 << 16, 7]
    );
    swi(
        &mut s,
        DEREGISTER_FILES,
        [BLOCK, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("deregistered");
    assert_eq!(s.peek(seen + 8), 6);
}

#[test]
fn star_resourcefs_selects_its_filing_system() {
    let mut s = crate::cli::tests::session();
    crate::cli::tests::filing_system(&mut s, false);
    crate::cli::tests::cli(&mut s, "ResourceFS").expect("selected");
    let seen = crate::cli::tests::SEEN;
    assert_eq!(s.peek(seen), 14);
    let name = s.peek(seen + 4);
    assert_eq!(s.read(name, 10), b"Resources\0");
}

/// Where a test hands the filing system a path and room to answer in.
const PATH: u32 = DATA + 0x800;
const ROOM: u32 = DATA + 0x900;

/// `entry` called as FileSwitch calls it, R1 the path `path` where given.
fn call_fs(
    s: &mut Session,
    entry: fn(&mut crate::os::Os<'_>, &mut crate::os::Regs) -> Result<(), crate::os::Fail>,
    path: Option<&[u8]>,
    registers: [u32; 8],
) -> Result<crate::os::Regs, u32> {
    let mut regs = crate::os::Regs::default();
    regs.r[..8].copy_from_slice(&registers);
    if let Some(path) = path {
        let mut text = path.to_vec();
        text.push(0);
        s.write(PATH, &text);
        regs.r[1] = PATH;
    }
    match entry(&mut s.kernel.os(), &mut regs) {
        Ok(()) => Ok(regs),
        Err(crate::os::Fail::Error(block)) => Err(s.peek(block)),
        Err(unwind) => panic!("unwound: {unwind:?}"),
    }
}

/// Three files, two of them in a directory, one of those registered first.
fn tree(s: &mut Session) {
    register(
        s,
        BLOCK,
        &[
            ("Box.Lid", &b"lid"[..]),
            ("Box.inner.Second", b"2"),
            ("Box.Inner.First", b"one"),
            ("Box.apple", b"a"),
        ],
    );
}

#[test]
fn a_file_is_read_by_its_catalogue_and_its_bytes_through_a_buffer() {
    let mut s = session();
    tree(&mut s);
    let info = call_fs(
        &mut s,
        super::fs::file,
        Some(b"$.Box.Lid"),
        [5, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!(info.r[0], 1, "a file");
    assert_eq!(
        info.r[2..6],
        [stamp::typed(filetype::TEXT, 0), 0, 3, OWNER_READ_WRITE]
    );
    let opened = call_fs(
        &mut s,
        super::fs::open,
        Some(b"$.Box.Lid"),
        [0, 0, 0, 0xFF, 0, 0, 0, 0],
    )
    .expect("opened");
    assert_eq!(opened.r[0], 1 << 30, "readable");
    assert_eq!(
        opened.r[2..5],
        [1024, 3, 3],
        "a buffer, the extent and what is allocated"
    );
    let handle = opened.r[1];
    let read = call_fs(
        &mut s,
        super::fs::get_bytes,
        None,
        [0, handle, ROOM, 8, 1, 0, 0, 0],
    );
    assert!(read.is_ok());
    assert_eq!(s.read(ROOM, 8), b"id\0\0\0\0\0\0", "zeros past the end");
    let extent =
        call_fs(&mut s, super::fs::args, None, [2, handle, 0, 0, 0, 0, 0, 0]).expect("extent");
    assert_eq!(extent.r[2], 3);
    assert_eq!(
        call_fs(
            &mut s,
            super::fs::args,
            None,
            [7, handle, 100, 0, 0, 0, 0, 0]
        )
        .expect_err("no room"),
        0x40E
    );
    assert!(call_fs(
        &mut s,
        super::fs::close,
        None,
        [0, handle, 0, 0, 0, 0, 0, 0]
    )
    .is_ok());
    assert!(
        call_fs(
            &mut s,
            super::fs::get_bytes,
            None,
            [0, handle, ROOM, 1, 0, 0, 0, 0]
        )
        .is_err(),
        "closed"
    );
}

#[test]
fn nothing_found_opens_as_no_handle_and_the_root_has_its_own_attributes() {
    let mut s = session();
    tree(&mut s);
    let none = call_fs(
        &mut s,
        super::fs::open,
        Some(b"$.Box.None"),
        [0, 0, 0, 0xFF, 0, 0, 0, 0],
    )
    .expect("asked");
    assert_eq!(none.r[1], 0);
    let root = call_fs(
        &mut s,
        super::fs::file,
        Some(b"$"),
        [5, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!((root.r[0], root.r[5]), (2, 0x19));
    let missing = call_fs(
        &mut s,
        super::fs::file,
        Some(b"$.Box.None"),
        [5, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!(missing.r[0], 0);
}

#[test]
fn a_directory_is_described_by_its_first_file_in_name_order() {
    let mut s = session();
    tree(&mut s);
    let inner = call_fs(
        &mut s,
        super::fs::file,
        Some(b"$.Box.Inner"),
        [5, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!(inner.r[0], 2);
    let first = found(&mut s, b"Box.Inner.First");
    let Some(Object::File(first)) = first else {
        panic!("the first file");
    };
    assert_eq!(inner.r[2..6], [first.load, first.exec, 0, 0]);
}

#[test]
fn children_are_listed_in_name_order_each_named_by_its_first_registered_file() {
    let mut s = session();
    tree(&mut s);
    let names = call_fs(
        &mut s,
        super::fs::func,
        Some(b"$.Box"),
        [14, 0, ROOM, 8, 0, 200, 0, 0],
    )
    .expect("listed");
    assert_eq!((names.r[3], names.r[4]), (3, 3));
    assert_eq!(
        s.read(ROOM, 16),
        b"apple\0inner\0Lid\0",
        "case kept from the first registered"
    );
    let rest = call_fs(
        &mut s,
        super::fs::func,
        Some(b"$.Box"),
        [14, 0, ROOM, 8, 3, 200, 0, 0],
    )
    .expect("listed");
    assert_eq!((rest.r[3], rest.r[4]), (0, u32::MAX), "none left");
    let records = call_fs(
        &mut s,
        super::fs::func,
        Some(b"$.Box"),
        [15, 0, ROOM, 8, 1, 200, 0, 0],
    )
    .expect("listed");
    assert_eq!((records.r[3], records.r[4]), (2, 3));
    let words: alloc::vec::Vec<u32> = s
        .read(ROOM, 20)
        .chunks(4)
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect();
    assert_eq!(
        words,
        [
            stamp::typed(filetype::TEXT, 0),
            0,
            1,
            OWNER_READ_WRITE,
            kind::DIRECTORY
        ],
        "the directory as its first registered file says"
    );
    assert_eq!(s.read(ROOM + 20, 6), b"inner\0");
    let tight = call_fs(
        &mut s,
        super::fs::func,
        Some(b"$.Box"),
        [14, 0, ROOM, 8, 0, 7, 0, 0],
    )
    .expect("listed");
    assert_eq!((tight.r[3], tight.r[4]), (1, 1), "only what the room holds");
}

#[test]
fn every_change_is_refused_as_read_only_and_the_rest_not_done() {
    let mut s = session();
    tree(&mut s);
    for reason in [0, 1, 4, 6, 7, 8] {
        assert_eq!(
            call_fs(
                &mut s,
                super::fs::file,
                Some(b"$.Box.Lid"),
                [reason, 0, 0, 0, 0, 0, 0, 0]
            ),
            Err(0x113),
            "FSEntry_File {reason}"
        );
    }
    assert_eq!(
        call_fs(
            &mut s,
            super::fs::open,
            Some(b"$.Box.Lid"),
            [2, 0, 0, 0xFF, 0, 0, 0, 0]
        ),
        Err(0x113)
    );
    assert_eq!(
        call_fs(
            &mut s,
            super::fs::func,
            Some(b"$.Box.Lid"),
            [8, 0, 0, 0, 0, 0, 0, 0]
        ),
        Err(0x113)
    );
    assert_eq!(
        call_fs(
            &mut s,
            super::fs::func,
            Some(b"$.Box"),
            [19, 0, ROOM, 8, 0, 200, 0, 0]
        ),
        Err(0x112)
    );
    assert_eq!(
        call_fs(&mut s, super::fs::put_bytes, None, [0; 8]),
        Err(0x113)
    );
}

#[test]
fn the_disc_has_no_name() {
    let mut s = session();
    s.write(ROOM, &[0xEE, 0xEE]);
    assert!(call_fs(&mut s, super::fs::func, None, [11, 0, ROOM, 0, 0, 0, 0, 0]).is_ok());
    assert_eq!(s.read(ROOM, 2), [0, 0]);
}
