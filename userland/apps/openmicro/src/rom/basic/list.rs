//! Listing: a program line's number and its text, each token spelt out and
//! each line reference shown as its number; and the decimal numbers both
//! print.

use tairix_openbasic::keyword::{word, PSEUDO_LHS};
use tairix_openbasic::list::Options;
use tairix_openbasic::{lineref, program};

use super::crunch::{KEYWORD, PSEUDO_TOKENS, READ};
use super::editor::LISTING_LOOPS;
use super::ws::zp as z;
use crate::rom::asm::byte;
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, RTS};
use crate::rom::os::CR;

/// The record being listed.
pub(super) const LINE: u16 = z::SCRATCH + 9;
/// The number being converted, and its five digits.
const NUMBER: u16 = LINE + 2;
const DIGITS: u16 = NUMBER + 2;
/// A byte `print_token` leaves alone, for a caller's Y.
pub(super) const TOKEN_KEPT: u16 = z::SCRATCH + 8;
/// The loops a listing is inside, one count for each of `Options::LOOPS`.
const OPEN_LOOPS: u16 = LISTING_LOOPS;
/// LISTO's space after the number is its bit 0, the one space it counts.
const _: () = assert!(Options::SPACE == 1);

/// The powers of ten each digit is counted in.
const POWERS: [u16; 5] = [10_000, 1_000, 100, 10, 1];
const DIGIT_COUNT: u8 = byte(POWERS.len());

pub fn emit(a: &mut Asm) {
    lines(a);
    keywords(a);
    decimal(a);
}

fn lines(a: &mut Asm) {
    // The record at LINE: its number in five columns, LISTO's space and
    // indent, its text and a new line; the loops it opens and closes
    // counted for the lines after it.
    a.here("list_line");
    a.ldy(imm(2));
    a.lda(zp(LINE).ind_y());
    a.tax();
    a.dey();
    a.lda(zp(LINE).ind_y());
    a.tay();
    a.jsr("print_line_number");
    a.lda(zp(z::LISTO));
    a.and(imm(Options::SPACE));
    a.tax();
    a.jsr("print_spaces");
    // Each kind of loop shown: two spaces a loop, but one for 128 or more,
    // as BASIC II prints them, so the indent may pass 255.
    let (kind, shown, few) = (a.local(), a.local(), a.local());
    a.ldy(imm(byte(Options::LOOPS.len()) - 1));
    a.bind(kind);
    a.lda(zp(z::LISTO));
    a.and(abs("listing_loop_options").y());
    a.beq(shown);
    a.lda(abs(OPEN_LOOPS).y());
    a.asl(Acc);
    a.bcc(few);
    a.lda(imm(1));
    a.bind(few);
    a.tax();
    a.jsr("print_spaces");
    a.bind(shown);
    a.dey();
    a.bpl(kind);
    a.later(|a| {
        a.here("listing_loop_options");
        a.bytes(&Options::LOOPS);
    });
    a.ldy(imm(byte(program::HEADER)));
    a.here("list_text");
    a.lda(zp(LINE).ind_y());
    a.cmp(imm(CR));
    a.beq("list_end");
    a.iny();
    a.cmp(imm(0x80));
    let character = a.local();
    a.bcc(character);
    a.sty(zp(TOKEN_KEPT));
    a.cmp(imm(lineref::MARKER));
    a.beq("list_reference");
    // A FOR or REPEAT opens its loop, a NEXT or UNTIL closes one if any
    // is open.
    let (open, close, counted) = (a.local(), a.local(), a.local());
    a.ldx(imm(0));
    a.cmp(imm(word::FOR.token));
    a.beq(open);
    a.cmp(imm(word::NEXT.token));
    a.beq(close);
    a.inx();
    a.cmp(imm(word::REPEAT.token));
    a.beq(open);
    a.cmp(imm(word::UNTIL.token));
    a.bne(counted);
    a.bind(close);
    a.ldy(zp(OPEN_LOOPS).x());
    a.beq(counted);
    a.dec(zp(OPEN_LOOPS).x());
    // C is set: both ways in compared equal.
    a.bcs(counted);
    a.bind(open);
    a.inc(zp(OPEN_LOOPS).x());
    a.bind(counted);
    a.jsr("print_token");
    a.ldy(zp(TOKEN_KEPT));
    a.jmp("list_text");
    a.bind(character);
    a.jsr("print_char");
    a.jmp("list_text");
    a.here("list_end");
    a.jmp("print_newline");

    // An encoded line number: its two bytes rebuilt from the three.
    a.here("list_reference");
    a.jsr("decode_reference");
    a.jsr("print_unsigned");
    a.lda(zp(TOKEN_KEPT));
    a.clc();
    a.adc(imm(byte(lineref::ENCODED)));
    a.tay();
    a.jmp("list_text");

    // XY = the line number encoded at (&4C) + Y.
    a.here("decode_reference");
    a.lda(zp(LINE).ind_y());
    a.asl(Acc);
    a.asl(Acc);
    a.tax();
    a.and(imm(0xC0));
    a.iny();
    a.eor(zp(LINE).ind_y());
    a.pha();
    a.txa();
    a.asl(Acc);
    a.asl(Acc);
    a.and(imm(0xC0));
    a.iny();
    a.eor(zp(LINE).ind_y());
    a.tay();
    a.pla();
    a.tax();
    a.rts();
}

fn keywords(a: &mut Asm) {
    // A printed, a token as its keyword: N from loading it.
    a.here("print_byte");
    a.bmi("print_token");
    a.jmp("print_char");

    // The keyword A stands for, a pseudo-variable's assigning token as its
    // own.
    a.here("print_token");
    a.cmp(imm(PSEUDO_TOKENS.0 | PSEUDO_LHS));
    let plain = a.local();
    a.bcc(plain);
    a.cmp(imm((PSEUDO_TOKENS.1 | PSEUDO_LHS) + 1));
    a.bcs(plain);
    a.eor(imm(PSEUDO_LHS));
    a.bind(plain);
    a.sta(zp(z::SCRATCH + 7));
    a.lda(imm(lo("keywords")));
    a.sta(zp(KEYWORD));
    a.lda(imm(hi("keywords")));
    a.sta(zp(KEYWORD + 1));
    // The table read from its start, the read position no bias.
    a.ldy(imm(0));
    a.sty(zp(READ));
    let (search, found) = (a.here_local(), a.local());
    a.lda(zp(KEYWORD).ind_y());
    a.iny();
    a.cmp(imm(0x80));
    a.bcc(search);
    a.cmp(zp(z::SCRATCH + 7));
    a.beq(found);
    a.jsr("next_keyword");
    a.bne(search);
    a.rts();
    a.bind(found);
    a.ldy(imm(0));
    let letter = a.here_local();
    a.lda(zp(KEYWORD).ind_y());
    a.bmi(RTS);
    a.jsr("print_char");
    a.iny();
    a.bne(letter);
    a.rts();
}

fn decimal(a: &mut Asm) {
    // XY in five columns, spaces before its first digit; or, unsigned,
    // from its first digit.
    a.here("print_line_number");
    a.lda(imm(b' '));
    let padded = a.local();
    a.bne(padded);
    a.here("print_unsigned");
    a.lda(imm(0));
    a.bind(padded);
    a.pha();
    a.stx(zp(NUMBER));
    a.sty(zp(NUMBER + 1));
    a.ldx(imm(0));
    let power = a.here_local();
    a.lda(imm(b'0'));
    a.sta(zp(DIGITS).x());
    let count = a.here_local();
    a.lda(zp(NUMBER));
    a.sec();
    a.sbc(abs("powers_low").x());
    a.tay();
    a.lda(zp(NUMBER + 1));
    a.sbc(abs("powers_high").x());
    let next = a.local();
    a.bcc(next);
    a.sta(zp(NUMBER + 1));
    a.sty(zp(NUMBER));
    a.inc(zp(DIGITS).x());
    a.bcs(count);
    a.bind(next);
    a.inx();
    a.cpx(imm(DIGIT_COUNT));
    a.bne(power);
    // Each leading zero but the last as the pad, or nothing.
    a.pla();
    a.tay();
    a.ldx(imm(0));
    let (leading, unpadded, digits) = (a.here_local(), a.local(), a.local());
    a.lda(zp(DIGITS).x());
    a.cmp(imm(b'0'));
    a.bne(digits);
    a.tya();
    a.beq(unpadded);
    a.jsr("print_char");
    a.bind(unpadded);
    a.inx();
    a.cpx(imm(DIGIT_COUNT - 1));
    a.bne(leading);
    a.bind(digits);
    a.lda(zp(DIGITS).x());
    a.jsr("print_char");
    a.inx();
    a.cpx(imm(DIGIT_COUNT));
    a.bne(digits);
    a.rts();
    a.here("powers_low");
    for power in POWERS {
        a.byte(power.to_le_bytes()[0]);
    }
    a.here("powers_high");
    for power in POWERS {
        a.byte(power.to_le_bytes()[1]);
    }
}
