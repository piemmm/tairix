//! `OS_Byte` and `OS_Word` from guest code: dispatch, the variables and
//! their rule, the fixed answers, CMOS, the clocks, and reasons the kernel
//! does not know.

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::testkit::{asm, with_application, Session, CODE, DATA};

const X: u32 = 0x2_0000;
const BYTE: u32 = 0x06;
const WORD: u32 = 0x07;
const CLAIM: u32 = 0x1F;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn byte(s: &mut Session, r0: u32, r1: u32, r2: u32) -> crate::os::Regs {
    s.user_swi(X | BYTE, [r0, r1, r2, 0x1234, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .0
}

#[test]
fn reason_0_names_the_version_or_fails_with_the_release() {
    let mut s = session();
    let regs = byte(&mut s, 0, 1, 0);
    assert!(!regs.flag(psr::V));
    assert_eq!((regs.r[1], regs.r[2]), (6, 0));
    let regs = byte(&mut s, 0, 0, 0);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0xF7);
    assert_eq!(
        s.error_text(regs.r[0]),
        b"RISC OS 5.30 (24 Apr 2024)",
        "the version and the date it was made"
    );
}

#[test]
fn the_reason_is_r0_s_low_byte_and_r0_and_r3_come_back_as_passed() {
    let mut s = session();
    let regs = byte(&mut s, 0x1DB, 0, 0xFF);
    assert_eq!(regs.r[0], 0x1DB);
    assert_eq!(regs.r[3], 0x1234);
    assert_eq!(
        (regs.r[1], regs.r[2]),
        (9, 0x1B),
        "TABch, then ESCch beside it"
    );
}

#[test]
fn variables_follow_the_and_eor_rule() {
    let mut s = session();
    let regs = byte(&mut s, 0xDB, 5, 0);
    assert_eq!(regs.r[1], 9, "the old value");
    assert_eq!(byte(&mut s, 0xDB, 0, 0xFF).r[1], 5);
    byte(&mut s, 0xDB, 0x03, 0xFF);
    assert_eq!(byte(&mut s, 0xDB, 0, 0xFF).r[1], 6, "5 EOR 3");
    byte(&mut s, 0xDB, 0, 0xF0);
    assert_eq!(byte(&mut s, 0xDB, 0, 0xFF).r[1], 0, "6 AND &F0");
}

#[test]
fn a_low_write_reason_takes_r1_whatever_r2_holds() {
    let mut s = session();
    byte(&mut s, 4, 2, 0x77);
    let regs = byte(&mut s, 4, 1, 0x77);
    assert_eq!(regs.r[1], 2, "R2 forced to zero: a plain write");
}

#[test]
fn the_fixed_answers_are_risc_os_s() {
    let mut s = session();
    let regs = byte(&mut s, 129, 0, 0xFF);
    assert_eq!((regs.r[1], regs.r[2]), (0xAA, 0));
    let regs = byte(&mut s, 130, 0, 0);
    assert_eq!((regs.r[1], regs.r[2]), (0xFF, 0xFF));
    let regs = byte(&mut s, 131, 0, 0);
    assert_eq!((regs.r[1], regs.r[2]), (0, 0x80));
}

#[test]
fn reasons_166_and_167_give_the_variables_place_in_zero_page_a_byte_each() {
    let mut s = session();
    let regs = byte(&mut s, 166, 0, 0xFF);
    assert_eq!((regs.r[1], regs.r[2]), (0xFA, 0x08), "&08FA, a byte each");
    let regs = byte(&mut s, 167, 0, 0xFF);
    assert_eq!(regs.r[1], 0x08);
    let escape = crate::memory::layout::HIGH_ZERO_PAGE + 0x08FA + 0xDC;
    assert_eq!(s.peek(escape) & 0xFF, 0x1B, "variable n at &08FA + n");
}

#[test]
fn a_reason_nobody_knows_is_not_recognised_and_a_claimant_may_answer() {
    let mut s = session();
    let regs = byte(&mut s, 50, 7, 8);
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0xFE);
    assert_eq!(s.error_text(regs.r[0]), b"Unknown command");
    assert_eq!((regs.r[1], regs.r[2]), (7, 0), "R2 zeroed for a low reason");
    // ByteV claimed for it.
    s.poke(CODE, &[asm::mov(1, 0x33), asm::claim()]);
    s.user_swi(X | CLAIM, [6, CODE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let regs = byte(&mut s, 50, 7, 8);
    assert!(!regs.flag(psr::V));
    assert_eq!(regs.r[1], 0x33);
}

#[test]
fn cmos_is_read_and_written_by_logical_address_its_checksum_kept() {
    let mut s = session();
    let checksum = byte(&mut s, 161, 0xEF, 0).r[2];
    let old = byte(&mut s, 161, 0x30, 0).r[2];
    byte(&mut s, 162, 0x30, old ^ 0x5A);
    assert_eq!(byte(&mut s, 161, 0x30, 0).r[2], old ^ 0x5A);
    assert_ne!(
        byte(&mut s, 161, 0xEF, 0).r[2],
        checksum,
        "the checksum follows"
    );
    let station = byte(&mut s, 161, 0, 0).r[2];
    byte(&mut s, 162, 0, station ^ 1);
    assert_eq!(
        byte(&mut s, 161, 0, 0).r[2],
        station,
        "the station number refused"
    );
    let past = byte(&mut s, 161, 0xF0, 0).r[2];
    byte(&mut s, 162, 0xF0, past ^ 1);
    assert_eq!(
        byte(&mut s, 161, 0xF0, 0).r[2],
        past ^ 1,
        "past the checksum too"
    );
    let good = byte(&mut s, 161, 0xEF, 0).r[2];
    byte(&mut s, 162, 0xEF, good ^ 0x10);
    byte(&mut s, 162, 0x30, old);
    assert_eq!(
        byte(&mut s, 161, 0xEF, 0).r[2] ^ 0x10,
        checksum,
        "a wrong checksum stays as wrong"
    );
}

#[test]
fn os_word_reads_and_writes_time_and_the_interval_timer() {
    let mut s = session();
    s.write(DATA, &[0xE8, 0x03, 0, 0, 0]);
    s.user_swi(X | WORD, [2, DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    s.clock.set(50);
    s.write(DATA, &[0; 5]);
    s.user_swi(X | WORD, [1, DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        s.read(DATA, 5),
        [0x1A, 0x04, 0, 0, 0],
        "1000 cs set, 50 later"
    );
    s.write(DATA, &[100, 0, 0, 0, 0]);
    s.user_swi(X | WORD, [4, DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    s.clock.set(80);
    s.user_swi(X | WORD, [3, DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(s.read(DATA, 5), [130, 0, 0, 0, 0], "counting up from 100");
}

#[test]
fn an_os_word_nobody_knows_returns_untouched_without_error() {
    let mut s = session();
    for reason in [23, 50] {
        s.poke(DATA, &[0x5555_5555]);
        let (regs, _) = s.user_swi(X | WORD, [reason, DATA, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(!regs.flag(psr::V));
        assert_eq!(&regs.r[..3], &[reason, DATA, 9]);
        assert_eq!(s.peek(DATA), 0x5555_5555, "reason {reason} reads no line");
    }
}
