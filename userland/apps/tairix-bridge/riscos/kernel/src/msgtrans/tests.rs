//! MessageTrans's tests: files opened in place and in buffers, lookups with
//! their defaults and the global file, the error buffers, enumeration,
//! menus, and files following ResourceFS's changes. The helpers here serve
//! ResourceFS's tests too.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use super::{
    flag, CLOSE_FILE, COPY_ERROR, DICTIONARY, ENUMERATE_TOKENS, ERROR_LOOKUP, FAST, FILE_INFO,
    GS_LOOKUP, LOOKUP, MAKE_MENUS, OPEN_FILE,
};
use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::resourcefs::{DEREGISTER_FILES, REGISTER_FILES};
use crate::testkit::{with_application, Session, DATA};

pub(crate) const X: u32 = 1 << 17;
/// Where a test's file block, descriptor, buffers and names go.
pub(crate) const BLOCK: u32 = DATA + 0x1000;
const DESCRIPTOR: u32 = DATA + 0x600;
const BUFFER: u32 = DATA + 0x100;
const NAME: u32 = DATA + 0x400;
const TOKEN: u32 = DATA + 0x500;

/// A session with an application slot.
pub(crate) fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

/// `SWI number` with `registers`: its registers, or its error's number and
/// text.
pub(crate) fn swi(
    s: &mut Session,
    number: u32,
    registers: [u32; 13],
) -> Result<crate::os::Regs, (u32, Vec<u8>)> {
    let (regs, _) = s.user_swi(X | number, registers);
    if regs.flag(psr::V) {
        let number = s.peek(regs.r[0]);
        Err((number, s.error_text(regs.r[0])))
    } else {
        Ok(regs)
    }
}

/// A ResourceFS block of `files`, each a name below `$` and its text.
pub(crate) fn block(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (name, data) in files {
        let name_room = (name.len() + 1).next_multiple_of(4);
        let data_room = data.len().next_multiple_of(4);
        let entry = 20 + name_room + 4 + data_room;
        let length = crate::os::len32(data.len());
        for word in [crate::os::len32(entry), 0xFFFF_FF00, 0, length, 3] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(name.as_bytes());
        bytes.resize(bytes.len() + name_room - name.len(), 0);
        bytes.extend_from_slice(&(length + 4).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len() + data_room - data.len(), 0);
    }
    bytes.extend_from_slice(&[0; 4]);
    bytes
}

/// Registers `files` as a block at `at`.
pub(crate) fn register(s: &mut Session, at: u32, files: &[(&str, &[u8])]) {
    s.write(at, &block(files));
    swi(s, REGISTER_FILES, [at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("registered");
}

/// A test's file in ResourceFS: tokens enough to be indexed.
const FILE: &[u8] = b"# A test file\n\
    Hello:Hello, %0!\n\
    Err?:Wild\n\
    Err7:Exact\n\
    One/Two:Either\n\
    Alone\n\
    Shared:Both\n\
    Plain:Just text\n\
    Menu:Menu\n\
    Item1:First\n\
    Item2:A longer second\n\
    Title:T\n";

/// Opens `name` through the test's descriptor, held by MessageTrans.
fn open(s: &mut Session, name: &str) -> Result<crate::os::Regs, (u32, Vec<u8>)> {
    let mut text = name.as_bytes().to_vec();
    text.push(0);
    s.write(NAME, &text);
    swi(
        s,
        OPEN_FILE,
        [DESCRIPTOR, NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
}

/// `token`'s value in the test's descriptor, expanded with `parameter`.
fn looked_up(
    s: &mut Session,
    descriptor: u32,
    token: &str,
    parameter: Option<&str>,
) -> Result<Vec<u8>, (u32, Vec<u8>)> {
    let mut text = token.as_bytes().to_vec();
    text.push(0);
    s.write(TOKEN, &text);
    let r4 = match parameter {
        Some(p) => {
            let mut p = p.as_bytes().to_vec();
            p.push(0);
            s.write(TOKEN + 0x80, &p);
            TOKEN + 0x80
        }
        None => 0,
    };
    let regs = swi(
        s,
        LOOKUP,
        [descriptor, TOKEN, BUFFER, 64, r4, 0, 0, 0, 0, 0, 0, 0, 0],
    )?;
    Ok(s.read(regs.r[2], regs.r[3] as usize))
}

fn with_file() -> Session {
    let mut s = session();
    register(&mut s, BLOCK, &[("Test.Messages", FILE)]);
    open(&mut s, "Resources:$.Test.Messages").expect("opened");
    s
}

#[test]
fn a_file_in_resourcefs_is_held_in_place() {
    let mut s = with_file();
    assert_eq!(s.peek(DESCRIPTOR), FAST);
    assert_ne!(s.peek(DESCRIPTOR + 4) & flag::PROXY, 0);
    let held = s.peek(DESCRIPTOR + 8);
    assert_eq!(
        s.peek(held + 4) & flag::IN_PLACE,
        flag::IN_PLACE,
        "used in place"
    );
    s.write(TOKEN, b"Plain\0");
    let regs = swi(
        &mut s,
        LOOKUP,
        [DESCRIPTOR, TOKEN, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("found");
    assert!(
        (BLOCK..BLOCK + 0x400).contains(&regs.r[2]),
        "the value in the file itself"
    );
    assert_eq!(s.read(regs.r[2], regs.r[3] as usize), b"Just text");
    assert_eq!(regs.r[1], TOKEN + 5, "R1 past the token");
}

#[test]
fn a_value_is_expanded_with_its_parameters() {
    let mut s = with_file();
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Hello", Some("world")),
        Ok(b"Hello, world!".to_vec())
    );
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Hello", None),
        Ok(b"Hello, %0!".to_vec()),
        "a parameter not given is kept"
    );
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Two", None),
        Ok(b"Either".to_vec())
    );
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Alone", None),
        Ok(b"Both".to_vec())
    );
}

#[test]
fn an_indexed_file_finds_an_exact_token_before_an_earlier_wildcard() {
    let mut s = with_file();
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Err7", None),
        Ok(b"Exact".to_vec())
    );
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Err8", None),
        Ok(b"Wild".to_vec())
    );
    s.write(DESCRIPTOR + 0x40, &[0; 32]);
    swi(
        &mut s,
        OPEN_FILE,
        [
            DESCRIPTOR + 0x40,
            NAME,
            0,
            u32::from_le_bytes(*b"SLOW"),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    )
    .expect("opened");
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR + 0x40, "Err7", None),
        Ok(b"Wild".to_vec()),
        "a walk finds the first"
    );
}

#[test]
fn a_default_answers_where_the_file_has_no_value_and_the_global_file_is_not_asked() {
    let mut s = with_file();
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Nowhere:Its default", None),
        Ok(b"Its default".to_vec())
    );
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "BufOFlo:Mine", None),
        Ok(b"Mine".to_vec())
    );
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "BufferOverflow", None),
        Ok(b"Buffer overflow".to_vec()),
        "the global file without one"
    );
    assert_eq!(
        looked_up(&mut s, 0, "BufferOverflow", None),
        Ok(b"Buffer overflow".to_vec())
    );
    let missing = looked_up(&mut s, DESCRIPTOR, "Nowhere", None).expect_err("no such token");
    assert_eq!(
        missing,
        (0xAC2, b"Message token Nowhere not found".to_vec())
    );
}

#[test]
fn a_file_in_a_caller_s_buffer_is_copied_and_held_by_no_one() {
    let mut s = session();
    register(&mut s, BLOCK, &[("Test.Messages", FILE)]);
    s.write(NAME, b"Resources:$.Test.Messages\0");
    let info = swi(
        &mut s,
        FILE_INFO,
        [0, NAME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("found");
    assert_eq!(info.r[0], 1, "usable in place");
    assert_eq!(info.r[2], crate::os::len32(FILE.len()) + 4);
    let buffer = DATA + 0x2000;
    swi(
        &mut s,
        OPEN_FILE,
        [DESCRIPTOR, NAME, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("opened");
    assert_eq!(s.peek(DESCRIPTOR + 8), buffer);
    assert_eq!(s.read(buffer + 4, FILE.len()), FILE);
    let original = BLOCK + 20 + 16 + 4;
    assert_eq!(s.read(original, 13), b"# A test file");
    s.write(original + 14, b"J");
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Hello", Some("x")),
        Ok(b"Hello, x!".to_vec()),
        "the copy is the file now"
    );
    swi(
        &mut s,
        CLOSE_FILE,
        [DESCRIPTOR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("closed");
}

#[test]
fn a_name_resourcefs_does_not_hold_is_filing_s_error() {
    let mut s = session();
    let refused = open(&mut s, "Resources:$.No.Such").expect_err("absent");
    assert_eq!(
        refused,
        (0xD6, b"File 'Resources:$.No.Such' not found".to_vec())
    );
    let refused = open(&mut s, "Resources:$.Resources").expect_err("a directory");
    assert_eq!(refused.0, 0xA8);
}

#[test]
fn error_lookup_makes_its_block_in_the_ring_or_the_caller_s_buffer() {
    let mut s = with_file();
    s.poke(TOKEN, &[0x1234]);
    s.write(TOKEN + 4, b"Hello\0");
    s.write(TOKEN + 0x80, b"there\0");
    let (regs, _) = s.user_swi(
        X | ERROR_LOOKUP,
        [
            TOKEN,
            DESCRIPTOR,
            0,
            0,
            TOKEN + 0x80,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    );
    assert!(regs.flag(psr::V), "always an error");
    let first = regs.r[0];
    assert_eq!(s.peek(first), 0x1234);
    assert_eq!(s.error_text(first), b"Hello, there!");
    let mut seen = alloc::vec![first];
    for _ in 0..9 {
        let (regs, _) = s.user_swi(
            X | ERROR_LOOKUP,
            [
                TOKEN,
                DESCRIPTOR,
                0,
                0,
                TOKEN + 0x80,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
        assert!(!seen.contains(&regs.r[0]), "the PRM's ten buffers in turn");
        seen.push(regs.r[0]);
    }
    let (regs, _) = s.user_swi(
        X | ERROR_LOOKUP,
        [TOKEN, DESCRIPTOR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(regs.r[0], first, "then the first again");
    let (regs, _) = s.user_swi(
        X | ERROR_LOOKUP,
        [
            TOKEN,
            DESCRIPTOR,
            BUFFER,
            12,
            TOKEN + 0x80,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    );
    assert_eq!(regs.r[0], BUFFER);
    assert_eq!(s.error_text(BUFFER), b"Hello, ", "cut to the buffer");
    let (copy, _) = s.user_swi(X | COPY_ERROR, [BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(copy.flag(psr::V));
    assert_eq!(
        (s.peek(copy.r[0]), s.error_text(copy.r[0])),
        (0x1234, b"Hello, ".to_vec())
    );
}

#[test]
fn a_buffer_too_small_for_its_terminator_or_its_error_is_left_alone() {
    let mut s = with_file();
    s.write(TOKEN, b"Hello\0");
    s.write(BUFFER, &[0xA5; 16]);
    let regs = swi(
        &mut s,
        LOOKUP,
        [DESCRIPTOR, TOKEN, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("found");
    assert_eq!(regs.r[3], 0);
    assert_eq!(s.read(BUFFER, 16), [0xA5; 16], "no room, nothing written");
    s.poke(TOKEN + 0x40, &[0x1234]);
    s.write(TOKEN + 0x44, b"Hello\0");
    for size in [1, 4] {
        let (regs, _) = s.user_swi(
            X | ERROR_LOOKUP,
            [
                TOKEN + 0x40,
                DESCRIPTOR,
                BUFFER,
                size,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
        assert!(regs.flag(psr::V));
        assert_ne!(
            regs.r[0], BUFFER,
            "MessageTrans's own error, in its own buffer"
        );
        assert_eq!(s.error_text(regs.r[0]), b"Buffer overflow");
        assert_eq!(
            s.read(BUFFER, 16),
            [0xA5; 16],
            "{size} bytes: nothing written"
        );
    }
    let (regs, _) = s.user_swi(
        X | ERROR_LOOKUP,
        [
            TOKEN + 0x40,
            DESCRIPTOR,
            BUFFER,
            5,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    );
    assert_eq!(
        (regs.r[0], s.peek(BUFFER), s.read(BUFFER + 4, 2)),
        (BUFFER, 0x1234, alloc::vec![0, 0xA5]),
        "room for the number and a terminator"
    );
}

#[test]
fn gs_lookup_translates_what_it_finds() {
    let mut s = session();
    register(&mut s, BLOCK, &[("Test.Messages", b"Bar:a|Mb<Gs$Test>\n")]);
    open(&mut s, "Resources:$.Test.Messages").expect("opened");
    s.write(TOKEN, b"Gs$Test\0");
    s.write(TOKEN + 0x20, b"c\0");
    swi(
        &mut s,
        0x24,
        [TOKEN, TOKEN + 0x20, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("set");
    s.write(TOKEN, b"Bar\0");
    let regs = swi(
        &mut s,
        GS_LOOKUP,
        [DESCRIPTOR, TOKEN, BUFFER, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("found");
    assert_eq!(s.read(BUFFER, regs.r[3] as usize), b"a\rbc");
}

#[test]
fn enumeration_gives_each_token_a_pattern_names_in_turn() {
    let mut s = with_file();
    s.write(TOKEN, b"Item*\0");
    let mut at = 0;
    let mut seen = Vec::new();
    loop {
        let regs = swi(
            &mut s,
            ENUMERATE_TOKENS,
            [DESCRIPTOR, TOKEN, BUFFER, 32, at, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .expect("walked");
        if regs.r[2] == 0 {
            break;
        }
        seen.push(s.read(BUFFER, regs.r[3] as usize));
        assert_eq!(
            s.read(BUFFER + regs.r[3], 1),
            [0],
            "the pattern's terminator"
        );
        at = regs.r[4];
    }
    assert_eq!(seen, [b"Item1".to_vec(), b"Item2".to_vec()]);
    s.write(TOKEN, b"I*m\0");
    let misplaced = swi(
        &mut s,
        ENUMERATE_TOKENS,
        [DESCRIPTOR, TOKEN, BUFFER, 32, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(misplaced.expect_err("a misplaced star").0, 0xAC0);
    s.write(TOKEN, b"Item1\0");
    let small = swi(
        &mut s,
        ENUMERATE_TOKENS,
        [DESCRIPTOR, TOKEN, BUFFER, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        small.expect_err("no room"),
        (0x2C1, b"Buffer overflow".to_vec())
    );
}

#[test]
fn menus_are_built_from_their_tokens() {
    let mut s = with_file();
    let definition = DATA + 0x3000;
    let mut bytes = b"Menu\0".to_vec();
    bytes.extend_from_slice(&[7, 2, 7, 0, 44, 0]);
    let item = |bytes: &mut Vec<u8>, token: &[u8], flags: u32| {
        bytes.extend_from_slice(token);
        bytes.push(0);
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        for word in [flags, 0, 0x0700_0021] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    };
    // The definition is word-aligned where it sits, as the macros lay it.
    item(&mut bytes, b"Item1", 0);
    item(&mut bytes, b"Item2", 1 << 7);
    bytes.push(0);
    s.write(definition, &bytes);
    let out = DATA + 0x3800;
    let regs = swi(
        &mut s,
        MAKE_MENUS,
        [DESCRIPTOR, definition, out, 256, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("built");
    assert_eq!(s.read(out, 5), b"Menu\0");
    assert_eq!(s.read(out + 12, 4), [7, 2, 7, 0]);
    assert_eq!((s.peek(out + 20), s.peek(out + 24)), (44, 0));
    assert_eq!(s.peek(out + 28), 0, "the first item's flags");
    assert_eq!(s.read(out + 28 + 12, 6), b"First\0");
    assert_eq!(
        s.read(out + 52 + 12, 12),
        b"A longer se\0",
        "eleven characters and a zero"
    );
    assert_eq!(s.peek(out + 16), 11 * 16 + 12, "the widest item");
    assert_eq!(regs.r[2], out + 28 + 48);
    assert_eq!(
        regs.r[3],
        256 - 28,
        "a menu's header taken from the size, as RISC OS takes it"
    );
    let short = DATA + 0x3C00;
    s.write(short, &[0xA5; 96]);
    let room = 28 + 24 + 4;
    let refused = swi(
        &mut s,
        MAKE_MENUS,
        [
            DESCRIPTOR, definition, short, room, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
    );
    assert_eq!(refused.expect_err("no room for the second item").0, 0x2C1);
    assert_eq!(
        s.read(short + room, 40),
        [0xA5; 40],
        "nothing past the buffer, where RISC OS's runs over"
    );
}

#[test]
fn closing_frees_what_is_held_and_a_missing_block_is_refused() {
    let mut s = with_file();
    let held = s.peek(DESCRIPTOR + 8);
    assert!(s.kernel.os().world.message_trans.open.contains(&held));
    swi(
        &mut s,
        CLOSE_FILE,
        [DESCRIPTOR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("closed");
    assert!(!s.kernel.os().world.message_trans.open.contains(&held));
    assert_eq!(s.read(DESCRIPTOR, 12), [0; 12]);
    let refused = swi(&mut s, CLOSE_FILE, [0; 13]).expect_err("no block");
    assert_eq!(
        refused,
        (0xAC1, b"Not a valid message file descriptor".to_vec())
    );
}

#[test]
fn a_file_held_in_place_follows_a_newer_registration() {
    let mut s = with_file();
    register(
        &mut s,
        BLOCK + 0x800,
        &[("Test.Messages", b"Plain:Replaced\n")],
    );
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Plain", None),
        Ok(b"Replaced".to_vec())
    );
    swi(
        &mut s,
        DEREGISTER_FILES,
        [BLOCK + 0x800, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("deregistered");
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Plain", None),
        Ok(b"Just text".to_vec())
    );
}

#[test]
fn the_kernel_s_errors_come_through_messagetrans_once_its_file_is_open() {
    let mut s = session();
    assert!(s.kernel.os().world.kernel_messages.is_open());
    let workspace = s
        .kernel
        .os()
        .world
        .message_trans
        .workspace
        .expect("running");
    let refused = s.user_swi(X | 0x7_FFFF, [0; 13]).0;
    assert!(refused.flag(psr::V));
    assert!(
        (workspace..workspace + super::WORKSPACE).contains(&refused.r[0]),
        "in MessageTrans's ring"
    );
    register(
        &mut s,
        BLOCK,
        &[(
            "Resources.Kernel.Messages",
            b"UnknownSwiNumber:Unheard-of SWI &%0\n",
        )],
    );
    let refused = s.user_swi(X | 0x7_FFFF, [0; 13]).0;
    assert_eq!(
        s.error_text(refused.r[0]),
        b"Unheard-of SWI &5FFFF",
        "the newer file's text"
    );
}

#[test]
fn a_dead_messagetrans_leaves_its_callers_to_open_again_and_the_kernel_its_own_texts() {
    let mut s = with_file();
    crate::cli::tests::cli(&mut s, "RMKill MessageTrans").expect("killed");
    assert_eq!(s.peek(DESCRIPTOR + 4) & flag::REOPEN, flag::REOPEN);
    let refused = s.user_swi(X | 0x7_FFFF, [0; 13]).0;
    assert_eq!(
        s.error_text(refused.r[0]),
        b"SWI &5FFFF not known",
        "the kernel's own text"
    );
    crate::cli::tests::cli(&mut s, "RMReInit MessageTrans").expect("started again");
    assert_eq!(
        looked_up(&mut s, DESCRIPTOR, "Plain", None),
        Ok(b"Just text".to_vec()),
        "opened again at its use"
    );
}

#[test]
fn the_dictionary_is_the_kernel_s_and_a_bad_swi_names_messagetrans() {
    let mut s = session();
    let regs = swi(&mut s, DICTIONARY, [0; 13]).expect("found");
    assert_eq!(s.read(regs.r[0], 1), [0], "no words");
    let refused = swi(&mut s, super::CHUNK + 10, [0; 13]).expect_err("no such SWI");
    assert_eq!(
        refused,
        (0x110, b"Module MessageTrans provides no such SWI".to_vec())
    );
}
