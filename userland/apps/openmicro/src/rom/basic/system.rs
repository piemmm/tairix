//! The statements that drive the MOS: VDU, MODE, CLS, CLG, COLOUR, GCOL,
//! PLOT, MOVE, DRAW, SOUND, ENVELOPE, OSCLI, CALL and WIDTH; and the
//! pseudo-variables assigned: TIME, PAGE, LOMEM and HIMEM; and CLEAR.

use super::ws::{page, zp as z};
use crate::rom::asm::{abs, imm, lo, zp, Acc, Asm};
use crate::rom::os::{entry, osbyte};

/// SOUND's and ENVELOPE's OSWORD block, once every value is found.
const BLOCK: u16 = z::STATEMENT;

pub fn emit(a: &mut Asm) {
    vdu(a);
    screen(a);
    lists(a);
    calls(a);
    memory(a);
}

fn vdu(a: &mut Asm) {
    // VDU: each value a byte, or before `;` a word, low byte first.
    a.here("statement_vdu");
    let (next, ended, value) = (a.here_local(), a.local(), a.local());
    a.jsr("next_a");
    a.bind(value);
    a.jsr("ends_statement");
    a.beq(ended);
    a.dec(zp(z::OFFSET_A));
    a.jsr("expr_integer");
    a.cpx(imm(b';'));
    let byte = a.local();
    a.bne(byte);
    a.jsr("vdu_iac_word");
    a.jmp(next);
    a.bind(byte);
    a.lda(zp(z::IAC));
    a.jsr(entry::OSWRCH);
    a.cpx(imm(b','));
    a.beq(next);
    a.txa();
    a.jmp(value);
    a.bind(ended);
    a.tax();
    a.jmp("statement_done_x");

    // MODE: the screen's new start becomes HIMEM, the BASIC stack empty;
    // Bad MODE where it would meet the variables or a call is in progress.
    a.here("statement_mode");
    a.jsr("expr_integer");
    a.stx(zp(z::OP));
    a.lda(zp(z::STACK));
    a.cmp(zp(z::HIMEM));
    a.bne("bad_mode");
    a.lda(zp(z::STACK + 1));
    a.cmp(zp(z::HIMEM + 1));
    a.bne("bad_mode");
    a.lda(imm(osbyte::HIMEM_MODE));
    a.ldx(zp(z::IAC));
    a.jsr(entry::OSBYTE);
    a.cpx(zp(z::VARTOP));
    a.tya();
    a.sbc(zp(z::VARTOP + 1));
    a.bcc("bad_mode");
    a.lda(imm(22));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::IAC));
    a.jsr(entry::OSWRCH);
    a.lda(imm(osbyte::HIMEM));
    a.jsr(entry::OSBYTE);
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.beq("himem_xy");
    a.here("bad_mode");
    a.jmp("err_bad_mode");

    // HIMEM = h: the BASIC stack starts again below it.
    a.here("statement_himem");
    a.jsr("pseudo_value");
    a.ldx(zp(z::IAC));
    a.ldy(zp(z::IAC + 1));
    a.here("himem_xy");
    a.stx(zp(z::HIMEM));
    a.stx(zp(z::STACK));
    a.sty(zp(z::HIMEM + 1));
    a.sty(zp(z::STACK + 1));
    a.jmp("pseudo_done");
}

fn screen(a: &mut Asm) {
    // CLS and CLG, CLS starting the count again.
    a.here("statement_cls");
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.lda(imm(12));
    a.bne("one_code");
    a.here("statement_clg");
    a.lda(imm(16));
    a.here("one_code");
    a.jsr(entry::OSWRCH);
    a.jmp("statement_done");

    // COLOUR n and GCOL a, n.
    a.here("statement_colour");
    a.lda(imm(17));
    a.jsr(entry::OSWRCH);
    a.jmp("vdu_last");
    a.here("statement_gcol");
    a.lda(imm(18));
    a.jsr(entry::OSWRCH);
    a.jsr("vdu_byte_comma");
    a.here("vdu_last");
    a.jsr("expr_integer");
    a.lda(zp(z::IAC));
    a.jsr(entry::OSWRCH);
    a.jmp("statement_done_x");

    // PLOT k, x, y; MOVE and DRAW are PLOT 4 and 5.
    a.here("statement_plot");
    a.lda(imm(25));
    a.jsr(entry::OSWRCH);
    a.jsr("vdu_byte_comma");
    a.jmp("plot_point");
    a.here("statement_move");
    a.lda(imm(4));
    a.bne("plot_code");
    a.here("statement_draw");
    a.lda(imm(5));
    a.here("plot_code");
    a.pha();
    a.lda(imm(25));
    a.jsr(entry::OSWRCH);
    a.pla();
    a.jsr(entry::OSWRCH);
    a.here("plot_point");
    a.jsr("vdu_word_comma");
    a.jsr("expr_integer");
    a.jsr("vdu_iac_word");
    a.jmp("statement_done_x");

    // The next value as a byte, or a word, then its comma.
    a.here("vdu_byte_comma");
    a.jsr("expr_integer");
    a.lda(zp(z::IAC));
    a.jsr(entry::OSWRCH);
    a.jmp("comma");
    a.here("vdu_word_comma");
    a.jsr("expr_integer");
    a.jsr("vdu_iac_word");
    a.jmp("comma");
    a.here("vdu_iac_word");
    a.lda(zp(z::IAC));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::IAC + 1));
    a.jmp(abs(entry::OSWRCH));
}

fn lists(a: &mut Asm) {
    // SOUND c, a, p, d: four words to OSWORD 7.
    a.here("statement_sound");
    a.ldy(imm((SOUND_BYTES / 2) | WORDS));
    a.lda(imm(7));
    a.bne("osword_list");

    // ENVELOPE and its fourteen bytes to OSWORD 8.
    a.here("statement_envelope");
    a.ldy(imm(ENVELOPE_BYTES));
    a.lda(imm(8));

    // Y values for OSWORD A, words when Y has WORDS: the OSWORD and each
    // value's bytes pushed as found, the count left on top while a value
    // is found, then pulled into the block from its end.
    a.here("osword_list");
    a.pha();
    let (value, byte, done, pull) = (a.here_local(), a.local(), a.local(), a.local());
    a.tya();
    a.pha();
    a.jsr("expr_integer");
    a.pla();
    a.tay();
    a.lda(zp(z::IAC));
    a.pha();
    a.tya();
    a.bpl(byte);
    a.lda(zp(z::IAC + 1));
    a.pha();
    a.bind(byte);
    a.dey();
    a.tya();
    a.and(imm(!WORDS));
    a.beq(done);
    a.jsr("comma");
    a.beq(value);
    a.bind(done);
    a.stx(zp(z::OP));
    a.ldx(imm(SOUND_BYTES - 1));
    a.tya();
    a.bmi(pull);
    a.ldx(imm(ENVELOPE_BYTES - 1));
    a.bind(pull);
    a.pla();
    a.sta(zp(BLOCK).x());
    a.dex();
    a.bpl(pull);
    a.pla();
    a.ldx(imm(lo(BLOCK)));
    a.jsr("osword_zp");
    a.jmp("pseudo_done");

    // OSWORD A with its block at IAC, its fifth byte FAC's sign, which
    // nothing holds between values; or at zero page X.
    a.here("osword_iac");
    a.ldx(imm(lo(z::IAC)));
    a.here("osword_zp");
    a.ldy(imm(0));
    a.jmp(abs(entry::OSWORD));
}

/// A count's flag that its values are words, and each block's size.
const WORDS: u8 = 0x80;
const SOUND_BYTES: u8 = 8;
const ENVELOPE_BYTES: u8 = 14;
const _: () = assert!(BLOCK + ENVELOPE_BYTES as u16 <= 0x70);

fn calls(a: &mut Asm) {
    // OSCLI: the string, with a CR, to the command line interpreter.
    a.here("statement_oscli");
    a.jsr("expr_string");
    a.stx(zp(z::OP));
    a.jsr("string_cr");
    a.jsr(entry::OSCLI);
    a.jmp("pseudo_done");

    // CALL address[, location...]: the block at &600 counts them, each with
    // its address and type, then the code is called with A%, X%, Y% and C%
    // as USR does. The address, the locations and their count wait on the
    // 6502 stack, the count on top.
    a.here("statement_call");
    a.jsr("expr_integer");
    a.lda(zp(z::IAC));
    a.pha();
    a.lda(zp(z::IAC + 1));
    a.pha();
    a.lda(imm(0));
    a.pha();
    let (more, made) = (a.here_local(), a.local());
    a.cpx(imm(b','));
    a.bne(made);
    a.jsr("lvalue");
    let located = a.local();
    a.bcc(located);
    a.jmp("err_mistake");
    a.bind(located);
    a.pla();
    a.tay();
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VTYPE));
    a.pha();
    a.iny();
    a.tya();
    a.pha();
    a.jsr("next_a");
    a.tax();
    a.bne(more);
    a.bind(made);
    a.stx(zp(z::OP));
    a.pla();
    a.sta(abs(page::STRING));
    a.tay();
    // The entries from the last, whose type is at 3n.
    let (entry_, entries) = (a.local(), a.local());
    a.beq(entries);
    a.asl(Acc);
    a.adc(abs(page::STRING));
    a.tax();
    a.bind(entry_);
    a.pla();
    a.sta(abs(page::STRING).x());
    a.pla();
    a.sta(abs(page::STRING - 1).x());
    a.pla();
    a.sta(abs(page::STRING - 2).x());
    a.dex();
    a.dex();
    a.dex();
    a.dey();
    a.bne(entry_);
    a.bind(entries);
    a.pla();
    a.sta(zp(z::IAC + 1));
    a.pla();
    a.sta(zp(z::IAC));
    a.jsr("call_iac");
    a.cld();
    a.jmp("pseudo_done");
}

fn memory(a: &mut Asm) {
    // TIME = t: the clock's five bytes.
    a.here("statement_time");
    a.jsr("pseudo_value");
    a.lda(imm(0));
    a.sta(zp(z::IAC + 4));
    a.lda(imm(2));
    a.jsr("osword_iac");
    a.jmp("pseudo_done");

    // PAGE = p: its high byte.
    a.here("statement_page");
    a.jsr("pseudo_value");
    a.lda(zp(z::IAC + 1));
    a.sta(zp(z::PAGE));
    a.jmp("pseudo_done");

    // LOMEM = l: the variables start there, and are gone.
    a.here("statement_lomem");
    a.jsr("pseudo_value");
    a.lda(zp(z::IAC));
    a.sta(zp(z::LOMEM));
    a.lda(zp(z::IAC + 1));
    a.sta(zp(z::LOMEM + 1));
    a.jsr("clear_lists");
    a.jmp("pseudo_done");

    // `=` and the value a pseudo-variable is given; and the statement's end
    // after it.
    a.here("pseudo_value");
    a.jsr("next_a");
    a.cmp(imm(b'='));
    let given = a.local();
    a.beq(given);
    a.jmp("err_mistake");
    a.bind(given);
    a.jsr("expr_integer");
    a.stx(zp(z::OP));
    a.rts();
    a.here("pseudo_done");
    a.ldx(zp(z::OP));
    a.jmp("statement_done_x");

    // CLEAR: the variables gone.
    a.here("statement_clear");
    a.jsr("clear_variables");
    a.jmp("statement_done");

    // WIDTH n: a new line after every n characters, none for 0.
    a.here("statement_width");
    a.jsr("expr_integer");
    a.ldy(zp(z::IAC));
    a.dey();
    a.sty(zp(z::WIDTH));
    a.jmp("statement_done_x");
}
