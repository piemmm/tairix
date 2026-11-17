//! ResourceFS's tests: names found as RISC OS's ResourceFS finds them, the
//! registrations and their refusals, and what they tell.

use tairix_riscos_arm::psr;

use super::{Object, DEREGISTER_FILES, REGISTER_FILES};
use crate::msgtrans::tests::{block, register, session, swi, BLOCK, X};
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
