//! `:dis file`: an object code file disassembled a screen at a time, each
//! instruction at its address from the file's load address, spelt as the
//! BBC's own assembler spells it: `&` hexadecimal, `#` immediate, `A` the
//! accumulator, and an opcode the manufacturer does not document as `EQUB`.
//! `j` or RETURN steps on an instruction and SPACE a screen; `k` or `b`
//! goes back to the view before; `q` or ESCAPE returns to the text.
//!
//! The tables are generated from the emulator's own instruction set, so the
//! ROM and the CPU it runs on cannot disagree about an opcode.

use alloc::vec::Vec;

use super::entry::ESCAPE;
use super::normal::cursor_key;
use super::ws::{page, show, zp as z};
use crate::cpu::isa::{Mnemonic, Mode, OPCODES};
use crate::rom::asm::{abs, byte, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{entry, CR};

/// What `dis_ops` holds for an undocumented opcode.
const UNDOCUMENTED: u8 = 0xFF;
/// OSFIND's call that opens a file to read.
const OPEN_IN: u8 = 0x40;
/// OSARGS's call that sets a file's pointer.
const SET_PTR: u8 = 1;
/// OSFILE's call that reads a file's catalogue information, and what it
/// finds for a file.
const READ_INFO: u8 = 5;
const A_FILE: u8 = 1;
/// The VDU code that clears the screen.
const VDU_CLEAR: u8 = 12;
/// The narrowest row the instructions' bytes are shown on.
const BYTES_SHOWN_FROM: u8 = 30;
/// The views back the disassembler keeps; a power of two.
const BACK: u8 = 32;
const _: () = assert!(BACK.is_power_of_two() && 2 * BACK == page::DIS_BACK_LEN);

/// A mode's form: the kind of operand it shows, whether `#` or `(` go
/// before it, and the text after it.
mod form {
    pub const NONE: u8 = 0;
    pub const BYTE: u8 = 1;
    pub const WORD: u8 = 2;
    pub const TARGET: u8 = 3;
    pub const KIND: u8 = 0x03;
    pub const HASH: u8 = 0x04;
    pub const OPEN: u8 = 0x08;
}

/// Each mode's form and the text after its operand, by [`mode_index`].
const FORMS: [(u8, &[u8]); 13] = [
    (form::NONE, b""),
    (form::NONE, b"A"),
    (form::BYTE | form::HASH, b""),
    (form::BYTE, b""),
    (form::BYTE, b",X"),
    (form::BYTE, b",Y"),
    (form::WORD, b""),
    (form::WORD, b",X"),
    (form::WORD, b",Y"),
    (form::WORD | form::OPEN, b")"),
    (form::BYTE | form::OPEN, b",X)"),
    (form::BYTE | form::OPEN, b"),Y"),
    (form::TARGET, b""),
];
/// The longest text after an operand.
const SUFFIX_LEN: usize = 3;
/// An instruction's bytes, by its form's kind.
const SIZES: [u8; 4] = [1, 2, 3, 2];

const fn mode_index(mode: Mode) -> u8 {
    match mode {
        Mode::Implied => 0,
        Mode::Accumulator => 1,
        Mode::Immediate => 2,
        Mode::ZeroPage => 3,
        Mode::ZeroPageX => 4,
        Mode::ZeroPageY => 5,
        Mode::Absolute => 6,
        Mode::AbsoluteX => 7,
        Mode::AbsoluteY => 8,
        Mode::Indirect => 9,
        Mode::IndexedIndirect => 10,
        Mode::IndirectIndexed => 11,
        Mode::Relative => 12,
    }
}

/// The documented mnemonics, each once, in opcode order.
fn names() -> Vec<Mnemonic> {
    let mut names = Vec::new();
    for opcode in OPCODES {
        if opcode.documented && !names.contains(&opcode.mnemonic) {
            names.push(opcode.mnemonic);
        }
    }
    names
}

pub(super) fn emit(a: &mut Asm) {
    opening(a);
    viewing(a);
    drawing(a);
    reading(a);
    operands(a);
    hexadecimal(a);
    tables(a);
}

/// `ex_dis`: the file named opened, its address and length taken, and the
/// view from its start.
fn opening(a: &mut Asm) {
    a.here("ex_dis");
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    crate::rom::far(a, Asm::bne, "no_name");
    a.jsr("arg_end");
    a.lda(zp(z::EX_AT));
    a.clc();
    a.adc(imm(lo(page::INPUT)));
    a.sta(abs(page::BLOCK));
    a.pha();
    a.lda(imm(hi(page::INPUT)));
    a.adc(imm(0));
    a.sta(abs(page::BLOCK + 1));
    a.pha();
    a.lda(imm(READ_INFO));
    a.jsr("osfile");
    a.cmp(imm(A_FILE));
    let a_file = a.local();
    a.beq(a_file);
    a.pla();
    a.pla();
    a.jmp("say_not_file");
    a.bind(a_file);
    for (to, from) in [
        (page::DIS_BASE, page::BLOCK + 2),
        (page::DIS_LEN, page::BLOCK + 10),
    ] {
        for at in 0..2 {
            a.lda(abs(from + at));
            a.sta(abs(to + at));
        }
    }
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.lda(abs(page::BLOCK + 12));
    a.ora(abs(page::BLOCK + 13));
    let short = a.local();
    a.beq(short);
    a.jmp("say_too_big");
    a.bind(short);
    a.lda(imm(OPEN_IN));
    a.jsr(entry::OSFIND);
    a.sta(abs(page::HANDLE));
    a.tay();
    let opened = a.local();
    a.bne(opened);
    a.jmp("say_not_file");
    a.bind(opened);
    a.lda(imm(0));
    for at in [
        page::DIS_TOP,
        page::DIS_TOP + 1,
        page::DIS_DEPTH,
        page::DIS_AT,
    ] {
        a.sta(abs(at));
    }
    a.fall_into("dis_view");
}

/// `dis_view`: the view drawn and a key done, until `q` or ESCAPE.
fn viewing(a: &mut Asm) {
    a.here("dis_view");
    a.jsr("dis_draw");
    let key = a.here_local();
    a.jsr("os_key");
    let quit = a.local();
    let step = a.local();
    let screen = a.local();
    let back = a.local();
    for (code, action) in [
        (b'q', quit),
        (ESCAPE, quit),
        (b'j', step),
        (cursor_key::DOWN, step),
        (CR, step),
        (b' ', screen),
        (b'k', back),
        (cursor_key::UP, back),
        (b'b', back),
    ] {
        a.cmp(imm(code));
        a.beq(action);
    }
    a.jmp(key);
    a.bind(quit);
    a.jsr("close_file");
    a.lda(imm(show::ALL));
    a.jmp("owe");
    // On: to the next instruction or the next screen, where the file goes on.
    a.bind(step);
    a.lda(abs(page::DIS_TOP));
    a.clc();
    a.adc(abs(page::DIS_FIRST));
    a.sta(zp(z::N1));
    a.lda(abs(page::DIS_TOP + 1));
    a.adc(imm(0));
    a.sta(zp(z::N1 + 1));
    let moved = a.local();
    a.jmp(moved);
    a.bind(screen);
    a.lda(abs(page::DIS_NEXT));
    a.sta(zp(z::N1));
    a.lda(abs(page::DIS_NEXT + 1));
    a.sta(zp(z::N1 + 1));
    a.bind(moved);
    a.lda(zp(z::N1));
    a.cmp(abs(page::DIS_LEN));
    a.lda(zp(z::N1 + 1));
    a.sbc(abs(page::DIS_LEN + 1));
    crate::rom::far(a, Asm::bcc, key);
    // The view before kept, the oldest given up once all are held.
    a.ldx(abs(page::DIS_AT));
    a.lda(abs(page::DIS_TOP));
    a.sta(abs(page::DIS_BACK).x());
    a.lda(abs(page::DIS_TOP + 1));
    a.sta(abs(page::DIS_BACK + u16::from(BACK)).x());
    a.inx();
    a.txa();
    a.and(imm(BACK - 1));
    a.sta(abs(page::DIS_AT));
    a.lda(abs(page::DIS_DEPTH));
    a.cmp(imm(BACK));
    let full = a.local();
    a.bcs(full);
    a.inc(abs(page::DIS_DEPTH));
    a.bind(full);
    let taken = a.local();
    a.bind(taken);
    a.lda(zp(z::N1));
    a.sta(abs(page::DIS_TOP));
    a.lda(zp(z::N1 + 1));
    a.sta(abs(page::DIS_TOP + 1));
    a.jmp("dis_view");
    // Back: the view kept last, or a byte back where none is.
    a.bind(back);
    a.lda(abs(page::DIS_DEPTH));
    let none_kept = a.local();
    a.beq(none_kept);
    a.dec(abs(page::DIS_DEPTH));
    a.lda(abs(page::DIS_AT));
    a.sec();
    a.sbc(imm(1));
    a.and(imm(BACK - 1));
    a.sta(abs(page::DIS_AT));
    a.tax();
    a.lda(abs(page::DIS_BACK).x());
    a.sta(zp(z::N1));
    a.lda(abs(page::DIS_BACK + u16::from(BACK)).x());
    a.sta(zp(z::N1 + 1));
    a.jmp(taken);
    a.bind(none_kept);
    a.lda(abs(page::DIS_TOP));
    a.ora(abs(page::DIS_TOP + 1));
    crate::rom::far(a, Asm::bne, key);
    a.lda(abs(page::DIS_TOP));
    a.sec();
    a.sbc(imm(1));
    a.sta(zp(z::N1));
    a.lda(abs(page::DIS_TOP + 1));
    a.sbc(imm(0));
    a.sta(zp(z::N1 + 1));
    a.jmp(taken);
}

/// `dis_draw`: a screen of instructions from the view's top, the size of
/// the first and where the next screen starts kept.
fn drawing(a: &mut Asm) {
    let ended = a.local();
    a.bind(ended);
    a.lda(zp(z::N0));
    a.sta(abs(page::DIS_NEXT));
    a.lda(zp(z::N0 + 1));
    a.sta(abs(page::DIS_NEXT + 1));
    a.rts();

    a.here("dis_draw");
    a.lda(imm(VDU_CLEAR));
    a.jsr(entry::OSWRCH);
    for (at, from) in [(z::N1, page::DIS_TOP), (z::N0, page::DIS_TOP)] {
        a.lda(abs(from));
        a.sta(zp(at));
        a.lda(abs(from + 1));
        a.sta(zp(at + 1));
    }
    a.lda(imm(0));
    a.sta(zp(z::N2));
    a.sta(zp(z::N2 + 1));
    a.sta(zp(z::ROW));
    a.lda(imm(SET_PTR));
    a.ldx(imm(byte(usize::from(z::N1))));
    a.ldy(abs(page::HANDLE));
    a.jsr(entry::OSARGS);
    let row = a.here_local();
    a.lda(zp(z::ROW));
    a.cmp(zp(z::ROWS));
    a.bcs(ended);
    a.lda(zp(z::N0));
    a.cmp(abs(page::DIS_LEN));
    a.lda(zp(z::N0 + 1));
    a.sbc(abs(page::DIS_LEN + 1));
    a.bcs(ended);
    // The address, then the opcode and what it is.
    a.clc();
    a.lda(zp(z::N0));
    a.adc(abs(page::DIS_BASE));
    a.sta(zp(z::P1));
    a.lda(zp(z::N0 + 1));
    a.adc(abs(page::DIS_BASE + 1));
    a.sta(zp(z::P1 + 1));
    a.jsr("hex_word");
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    instruction_read(a);
    bytes_column(a);
    a.jsr("dis_text");
    a.jsr(entry::OSNEWL);
    a.lda(zp(z::ROW));
    let later_row = a.local();
    a.bne(later_row);
    a.lda(zp(z::T2));
    a.sta(abs(page::DIS_FIRST));
    a.bind(later_row);
    a.inc(zp(z::ROW));
    a.jmp(row);
}

/// The instruction at N0 read: its opcode in T0, its form in T1, its size
/// in T2 and its mnemonic in T3, `UNDOCUMENTED` for one shown as `EQUB`,
/// and its operand in P2.
fn instruction_read(a: &mut Asm) {
    a.jsr("dis_byte");
    a.sta(zp(z::T0));
    a.tax();
    a.lda(abs("dis_ops").x());
    a.sta(zp(z::T3));
    a.lda(imm(1));
    a.sta(zp(z::T2));
    a.lda(imm(form::NONE));
    a.sta(zp(z::T1));
    a.lda(zp(z::T3));
    a.cmp(imm(UNDOCUMENTED));
    let formed = a.local();
    a.beq(formed);
    a.jsr("dis_mode");
    a.lda(abs("dis_forms").x());
    a.sta(zp(z::T1));
    a.and(imm(form::KIND));
    a.tay();
    a.lda(abs("dis_sizes").y());
    a.sta(zp(z::T2));
    // An instruction the file ends inside is shown as its bytes.
    a.sec();
    a.lda(abs(page::DIS_LEN));
    a.sbc(zp(z::N0));
    a.tax();
    a.lda(abs(page::DIS_LEN + 1));
    a.sbc(zp(z::N0 + 1));
    a.bne(formed);
    a.ldy(zp(z::T2));
    a.dey();
    a.sty(zp(z::T4));
    a.cpx(zp(z::T4));
    a.bcs(formed);
    a.lda(imm(UNDOCUMENTED));
    a.sta(zp(z::T3));
    a.lda(imm(1));
    a.sta(zp(z::T2));
    a.bind(formed);
    for operand in [z::P2, z::P2 + 1] {
        let absent = a.local();
        a.lda(zp(z::T2));
        a.cmp(imm(byte(usize::from(operand - z::P2)) + 2));
        a.bcc(absent);
        a.jsr("dis_byte");
        a.sta(zp(operand));
        a.bind(absent);
    }
}

/// The instruction's bytes, where the row has room for them.
fn bytes_column(a: &mut Asm) {
    a.lda(zp(z::WIDTH));
    a.cmp(imm(BYTES_SHOWN_FROM));
    let bytes_done = a.local();
    a.bcc(bytes_done);
    a.lda(zp(z::T0));
    a.jsr("hex_space");
    for (at, operand) in [(2, z::P2), (3, z::P2 + 1)] {
        let blank = a.local();
        let shown = a.local();
        a.lda(zp(z::T2));
        a.cmp(imm(at));
        a.bcc(blank);
        a.lda(zp(operand));
        a.jsr("hex_space");
        a.jmp(shown);
        a.bind(blank);
        a.call_with_text("print_inline", &["   "]);
        a.bind(shown);
    }
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    a.bind(bytes_done);
}

/// `dis_byte` and `dis_mode`: the file's next byte, and an opcode's mode.
fn reading(a: &mut Asm) {
    // `dis_byte`: the file's next byte, N0 counting it.
    a.here("dis_byte");
    a.ldy(abs(page::HANDLE));
    a.jsr(entry::OSBGET);
    a.inc(zp(z::N0));
    a.bne(RTS);
    a.inc(zp(z::N0 + 1));
    a.rts();

    // `dis_mode`: the mode of opcode T0, by `mode_index`, in X.
    a.here("dis_mode");
    a.lda(zp(z::T0));
    a.lsr(Acc);
    a.tax();
    a.lda(abs("dis_modes").x());
    let low = a.local();
    a.bcc(low);
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.bind(low);
    a.and(imm(0x0F));
    a.tax();
    a.rts();
}

/// `dis_text`: the instruction's mnemonic and operand, or its byte as
/// `EQUB`.
fn operands(a: &mut Asm) {
    a.here("dis_text");
    a.lda(zp(z::T3));
    a.cmp(imm(UNDOCUMENTED));
    let documented = a.local();
    a.bne(documented);
    a.call_with_text("print_inline", &["EQUB &"]);
    a.lda(zp(z::T0));
    a.jmp("hex_byte");
    a.bind(documented);
    a.asl(Acc);
    a.adc(zp(z::T3));
    a.tax();
    for at in 0..3 {
        a.lda(abs(sym("dis_names") + at).x());
        a.jsr(entry::OSWRCH);
    }
    a.jsr("dis_mode");
    a.txa();
    a.asl(Acc);
    a.asl(Acc);
    a.sta(zp(z::T4));
    a.tax();
    a.lda(zp(z::T1));
    a.and(imm(form::KIND));
    let operand = a.local();
    a.bne(operand);
    a.lda(abs("dis_suffixes").x());
    a.beq(RTS);
    a.bind(operand);
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    for (bit, ch) in [(form::OPEN, b'('), (form::HASH, b'#')] {
        let without = a.local();
        a.lda(zp(z::T1));
        a.and(imm(bit));
        a.beq(without);
        a.lda(imm(ch));
        a.jsr(entry::OSWRCH);
        a.bind(without);
    }
    a.lda(zp(z::T1));
    a.and(imm(form::KIND));
    let suffix = a.local();
    a.beq(suffix);
    a.tax();
    a.lda(imm(b'&'));
    a.jsr(entry::OSWRCH);
    a.cpx(imm(form::BYTE));
    let wider = a.local();
    a.bne(wider);
    a.lda(zp(z::P2));
    a.jsr("hex_byte");
    a.jmp(suffix);
    a.bind(wider);
    a.cpx(imm(form::WORD));
    let target = a.local();
    a.bne(target);
    a.lda(zp(z::P2 + 1));
    a.ldx(zp(z::P2));
    let word = a.local();
    a.jmp(word);
    // A branch's target: past it, by its signed displacement.
    a.bind(target);
    a.lda(zp(z::P2));
    a.clc();
    a.adc(imm(2));
    a.php();
    a.clc();
    a.adc(zp(z::P1));
    a.tax();
    a.lda(zp(z::P2));
    a.and(imm(0x80));
    let forward = a.local();
    a.beq(forward);
    a.lda(imm(0xFF));
    a.bind(forward);
    a.adc(zp(z::P1 + 1));
    a.plp();
    a.adc(imm(0));
    a.bind(word);
    a.jsr("hex_byte");
    a.txa();
    a.jsr("hex_byte");
    a.bind(suffix);
    a.ldx(zp(z::T4));
    let each = a.here_local();
    a.lda(abs("dis_suffixes").x());
    a.beq(RTS);
    a.jsr(entry::OSWRCH);
    a.inx();
    a.bne(each);
    a.rts();
}

/// Numbers in hexadecimal.
fn hexadecimal(a: &mut Asm) {
    // `hex_word`: P1 in hexadecimal. `hex_space`: A in hexadecimal and a
    // space. `hex_byte`: A in hexadecimal. X and Y kept.
    a.here("hex_word");
    a.lda(zp(z::P1 + 1));
    a.jsr("hex_byte");
    a.lda(zp(z::P1));
    a.jmp("hex_byte");

    a.here("hex_space");
    a.jsr("hex_byte");
    a.lda(imm(b' '));
    a.jmp(abs(entry::OSWRCH));

    a.here("hex_byte");
    a.pha();
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.jsr("hex_digit");
    a.pla();
    a.and(imm(0x0F));
    a.here("hex_digit");
    a.cmp(imm(10));
    let digit = a.local();
    a.bcc(digit);
    a.adc(imm(b'A' - b'9' - 2));
    a.bind(digit);
    a.adc(imm(b'0'));
    a.jmp(abs(entry::OSWRCH));
}

fn tables(a: &mut Asm) {
    a.later(|a| {
        let names = names();
        a.here("dis_names");
        for name in &names {
            a.bytes(name.name().as_bytes());
        }
        a.here("dis_ops");
        for opcode in OPCODES {
            let index = names.iter().position(|&name| name == opcode.mnemonic);
            a.byte(match index {
                Some(index) if opcode.documented => byte(index),
                _ => UNDOCUMENTED,
            });
        }
        a.here("dis_modes");
        for pair in OPCODES.as_chunks::<2>().0 {
            a.byte(mode_index(pair[0].mode) | mode_index(pair[1].mode) << 4);
        }
        a.here("dis_forms");
        for (form, _) in FORMS {
            a.byte(form);
        }
        a.here("dis_suffixes");
        for (_, suffix) in FORMS {
            let mut padded = [0; SUFFIX_LEN + 1];
            padded[..suffix.len()].copy_from_slice(suffix);
            a.bytes(&padded);
        }
        a.here("dis_sizes");
        a.bytes(&SIZES);
    });
}
