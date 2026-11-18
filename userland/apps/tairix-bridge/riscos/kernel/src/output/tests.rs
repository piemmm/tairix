//! Character output from guest code: each SWI's characters through
//! `WrchV`, and `OS_WriteS` returning past its string.

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::Outcome;
use crate::swi::os::{BYTE, CLAIM, NEW_LINE, PRINT_CHAR, WRITE_0, WRITE_C, WRITE_N, WRITE_S};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, DATA, SWI_AT};

const WRITE_I: u32 = 0x100;

fn session() -> Session {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    s.capture_output();
    s
}

#[test]
fn each_output_swi_writes_its_characters_through_wrchv() {
    let mut s = session();
    s.user_swi(
        X | WRITE_C,
        [b'A'.into(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    s.user_swi(X | WRITE_I | u32::from(b'B'), [0; 13]);
    s.write(DATA, b"cd\0ef");
    let (regs, _) = s.user_swi(X | WRITE_0, [DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(regs.r[0], DATA + 3, "past the terminator");
    s.user_swi(X | WRITE_N, [DATA + 3, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    s.user_swi(X | NEW_LINE, [0; 13]);
    assert_eq!(s.output(), b"ABcdef\n\r");
}

#[test]
fn write_s_writes_the_string_after_it_and_returns_past_it_word_aligned() {
    let mut s = session();
    let after = SWI_AT + 12;
    s.poke(SWI_AT, &[asm::swi(X | WRITE_S)]);
    s.write(SWI_AT + 4, b"Hello\0\0\0");
    s.poke(after, &[asm::b(after, after)]);
    s.enter(Bits::Bit32, SWI_AT, [0; 13]);
    assert_eq!(s.resume(), Outcome::Budget);
    assert_eq!(s.kernel.state().pc, after);
    assert_eq!(s.output(), b"Hello");
}

#[test]
fn the_printer_hears_only_what_os_byte_3_lets_through() {
    const BPUT_V: u32 = 0x0B;
    const FIND_V: u32 = 0x0D;
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    let call = |s: &mut Session, swi: u32, r0: u32, r1: u32, r2: u32| {
        s.user_swi(X | swi, [r0, r1, r2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    };
    let (find, bput, printed) = (DATA + 0x400, DATA + 0x420, DATA + 0x500);
    s.poke(find, &[asm::mov(0, 0x77), asm::claim()]);
    s.poke(
        bput,
        &[
            asm::ldr(11, 12, 0),
            asm::strb_post(0, 11, 1),
            asm::str(11, 12, 0),
            asm::claim(),
        ],
    );
    s.poke(printed, &[printed + 4]);
    call(&mut s, CLAIM, FIND_V, find, 0);
    call(&mut s, CLAIM, BPUT_V, bput, printed);
    crate::cli::tests::cli(&mut s, "Set PrinterType$1 lp").expect("set");
    call(&mut s, BYTE, 5, 1, 0);
    call(&mut s, BYTE, 3, 8, 0);
    call(&mut s, WRITE_C, u32::from(b'a'), 0, 0);
    call(&mut s, BYTE, 3, 8 | 64, 0);
    call(&mut s, WRITE_C, u32::from(b'b'), 0, 0);
    call(&mut s, PRINT_CHAR, u32::from(b'c'), 0, 0);
    call(&mut s, BYTE, 3, 0, 0);
    call(&mut s, PRINT_CHAR, u32::from(b'd'), 0, 0);
    assert_eq!(s.read(printed + 4, 2), b"ad");
    assert_eq!(s.peek(printed), printed + 6, "and nothing more");
}
