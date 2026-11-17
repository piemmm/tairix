//! System variables from guest code: each type set and read, GSTrans on
//! the way in, probes and short buffers, wildcards and their context,
//! deletion, refusals, and a code variable's two entries.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::Regs;
use crate::testkit::{asm, with_application, Session, CODE, DATA};

const X: u32 = 0x2_0000;
const READ_VAR_VAL: u32 = 0x23;
const SET_VAR_VAL: u32 = 0x24;
const NAME: u32 = DATA;
const VALUE: u32 = DATA + 0x100;
const BUFFER: u32 = DATA + 0x200;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn set(s: &mut Session, name: &[u8], value: &[u8], length: u32, kind: u32) -> Regs {
    s.write(NAME, name);
    s.write(NAME + crate::os::len32(name.len()), b"\0");
    s.write(VALUE, value);
    s.user_swi(
        X | SET_VAR_VAL,
        [NAME, VALUE, length, 0, kind, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .0
}

fn read(s: &mut Session, name: &[u8], room: u32, kind: u32) -> (Regs, Vec<u8>) {
    s.write(NAME, name);
    s.write(NAME + crate::os::len32(name.len()), b"\0");
    let (regs, _) = s.user_swi(
        X | READ_VAR_VAL,
        [NAME, BUFFER, room, 0, kind, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    let length = if regs.flag(psr::V) {
        0
    } else {
        regs.r[2] as usize
    };
    let value = s.read(BUFFER, length);
    (regs, value)
}

#[test]
fn a_string_is_gstransd_on_the_way_in_and_read_back_as_type_0() {
    let mut s = session();
    assert!(!set(&mut s, b"Corpus$Name", b"bar\0", 0, 0).flag(psr::V));
    let (regs, value) = read(&mut s, b"Corpus$Name", 64, 0);
    assert_eq!((value.as_slice(), regs.r[4]), (&b"bar"[..], 0));
    set(&mut s, b"Corpus$Other", b"\"a<Corpus$Name>|Mb\"\0", 0, 0);
    let (_, value) = read(&mut s, b"Corpus$Other", 64, 0);
    assert_eq!(value, b"abar\rb");
    set(&mut s, b"Corpus$Literal", b"x<y>", 4, 4);
    let (regs, value) = read(&mut s, b"Corpus$Literal", 64, 0);
    assert_eq!(
        (value.as_slice(), regs.r[4]),
        (&b"x<y>"[..], 0),
        "untranslated"
    );
}

#[test]
fn numbers_macros_and_expressions_keep_their_types() {
    let mut s = session();
    set(&mut s, b"Corpus$N", &(-5i32).to_le_bytes(), 0, 1);
    let (regs, value) = read(&mut s, b"Corpus$N", 64, 0);
    assert_eq!(
        (value.as_slice(), regs.r[4]),
        (&(-5i32).to_le_bytes()[..], 1)
    );
    let (regs, value) = read(&mut s, b"Corpus$N", 64, 3);
    assert_eq!((value.as_slice(), regs.r[4]), (&b"-5"[..], 0), "expanded");
    set(&mut s, b"Corpus$Name", b"bar\0", 0, 0);
    set(&mut s, b"Corpus$M", b"<Corpus$Name>!\0", 0, 2);
    let (regs, value) = read(&mut s, b"Corpus$M", 64, 0);
    assert_eq!((value.as_slice(), regs.r[4]), (&b"<Corpus$Name>!"[..], 2));
    let (regs, value) = read(&mut s, b"Corpus$M", 64, 3);
    assert_eq!(value, b"bar!");
    assert_eq!(regs.r[4], 2, "a macro read expanded still says 2");
    let regs = set(&mut s, b"Corpus$E", b"1+2\0", 0, 3);
    assert_eq!(regs.r[4], 1, "an integer result is a number");
    let (_, value) = read(&mut s, b"Corpus$E", 64, 3);
    assert_eq!(value, b"3");
    let regs = set(&mut s, b"Corpus$S", b"\"ab\"+\"c\"\0", 0, 3);
    assert_eq!(regs.r[4], 4, "a string result says 4");
    let (regs, value) = read(&mut s, b"Corpus$S", 64, 0);
    assert_eq!((value.as_slice(), regs.r[4]), (&b"abc"[..], 0));
}

#[test]
fn a_probe_gives_the_length_s_complement_and_a_short_buffer_overflows() {
    let mut s = session();
    set(&mut s, b"Corpus$Name", b"value\0", 0, 0);
    let (regs, _) = read(&mut s, b"Corpus$Name", 0x8000_0000, 0);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1E4);
    assert_eq!(regs.r[2], !5);
    s.write(BUFFER, b"......");
    let (regs, _) = read(&mut s, b"Corpus$Name", 3, 0);
    assert!(regs.flag(psr::V));
    assert_eq!(s.read(BUFFER, 4), b"val.");
}

#[test]
fn wildcards_enumerate_in_order_through_the_context_and_end_not_found() {
    let mut s = session();
    for name in [&b"Corpus$B"[..], b"Corpus$A", b"Corpus$C", b"Other"] {
        set(&mut s, name, b"x\0", 0, 0);
    }
    s.write(NAME, b"Corpus$*\0");
    let mut context = 0;
    let mut found = Vec::new();
    loop {
        let (regs, _) = s.user_swi(
            X | READ_VAR_VAL,
            [NAME, BUFFER, 64, context, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        if regs.flag(psr::V) {
            assert_eq!(s.peek(regs.r[0]), 0x124);
            break;
        }
        context = regs.r[3];
        let (name, _) = s
            .kernel
            .os()
            .read_until(context, 64, |b| b == 0)
            .expect("a name");
        found.push(name);
    }
    assert_eq!(found, [&b"Corpus$A"[..], b"Corpus$B", b"Corpus$C"]);
}

#[test]
fn deletion_honours_wildcards_and_a_missing_variable_is_not_found() {
    let mut s = session();
    set(&mut s, b"Corpus$A", b"x\0", 0, 0);
    set(&mut s, b"Corpus$B", b"x\0", 0, 0);
    let regs = set(&mut s, b"Corpus$*", b"", u32::MAX, 0);
    assert!(!regs.flag(psr::V));
    let (regs, _) = read(&mut s, b"Corpus$A", 64, 0);
    assert!(regs.flag(psr::V), "the first match deleted");
    let (regs, _) = read(&mut s, b"Corpus$B", 64, 0);
    assert!(!regs.flag(psr::V), "the second kept");
    let regs = set(&mut s, b"Corpus$Missing", b"", u32::MAX, 0);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x124);
    assert_eq!(s.error_text(regs.r[0]), b"Variable not found");
}

#[test]
fn bad_names_types_and_macros_are_refused() {
    let mut s = session();
    let regs = set(&mut s, b"Wild*Card", b"x\0", 0, 0);
    assert_eq!(s.peek(regs.r[0]), 0x121);
    let regs = set(&mut s, b"Corpus$T", b"x\0", 0, 5);
    assert_eq!(s.peek(regs.r[0]), 0x122);
    let regs = set(&mut s, b"Corpus$M", b"ab\x07", 0, 2);
    assert_eq!(
        s.peek(regs.r[0]),
        0x120,
        "a macro ending in another control"
    );
}

#[test]
fn a_code_variable_s_entries_answer_its_reads_and_take_its_writes() {
    let mut s = session();
    // +0 write entry: the length written kept at DATA + 0x400. +4 read
    // entry: the value "code" at DATA + 0x410.
    let kept = DATA + 0x400;
    let text = DATA + 0x410;
    s.write(text, b"code");
    let code = [
        asm::b(0, 8),
        asm::b(4, 20),
        // Write: STR R2, [R10] with R10 = kept.
        asm::mov(10, kept & 0xFF00),
        asm::str(2, 10, 0),
        asm::mov_pc_lr(),
        // Read: R0 = text, R2 = 4.
        asm::mov(0, text & 0xFF00),
        asm::add(0, 0, text & 0xFF),
        asm::mov(2, 4),
        asm::mov_pc_lr(),
    ];
    let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    s.write(CODE, &bytes);
    s.write(NAME, b"Corpus$Code\0");
    let length = crate::os::len32(bytes.len());
    let (regs, _) = s.user_swi(
        X | SET_VAR_VAL,
        [NAME, CODE, length, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(!regs.flag(psr::V));
    let (regs, value) = read(&mut s, b"Corpus$Code", 64, 0);
    assert_eq!((value.as_slice(), regs.r[4]), (&b"code"[..], 0));
    set(&mut s, b"Corpus$Code", b"hello", 5, 4);
    assert_eq!(s.peek(kept), 5, "the write entry took the value");
    let regs = set(&mut s, b"Corpus$Code", b"", u32::MAX, 0);
    assert!(!regs.flag(psr::V));
    let (regs, _) = read(&mut s, b"Corpus$Code", 64, 0);
    assert!(!regs.flag(psr::V), "only a delete of type 16 removes it");
}

#[test]
fn a_name_ends_at_a_space_however_long_what_follows_it() {
    // The space ends the name, so text past the longest a name may be does
    // not count against it.
    let mut s = session();
    let mut name = b"Spaced ".to_vec();
    name.extend_from_slice(&[b'x'; 300]);
    let made = set(&mut s, &name, b"value\r", 0, 0);
    assert!(!made.flag(psr::V), "set");
    let (found, bytes) = read(&mut s, &name, 64, 0);
    assert!(!found.flag(psr::V), "read");
    assert_eq!(bytes, b"value");
    let (whole, _) = read(&mut s, b"Spaced", 64, 0);
    assert!(!whole.flag(psr::V), "the same variable");
}
