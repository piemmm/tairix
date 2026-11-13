//! Control: GOTO and GOSUB to a line, RETURN, FOR and NEXT, REPEAT and
//! UNTIL, IF with THEN and ELSE, ON with GOTO and GOSUB, and TRACE.
//!
//! The control stacks share page 5: FOR's fifteen-byte entries from its
//! start, then REPEAT's and GOSUB's return points, each a low and a high
//! byte in two tables. A return point is the address of the character a
//! statement ended at.

use super::editor::{LINE_NUMBER, RECORD};
use super::ws::{kind, page, place, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm, Label, RTS};

const TO: u8 = 0xB8;
const STEP: u8 = 0x88;
const THEN: u8 = 0x8C;
const ELSE: u8 = 0x8B;
const GOTO: u8 = 0xE5;
const GOSUB: u8 = 0xE4;
const LINE_REFERENCE: u8 = super::tokens::LINE_REFERENCE;
const CR: u8 = 0x0D;

/// A FOR entry: the variable's address and type, the step, the limit,
/// and the return point.
const FOR_ENTRY: u8 = 15;
const FOR_TYPE: u16 = 2;
const FOR_STEP: u16 = 3;
const FOR_LIMIT: u16 = 8;
const FOR_RETURN: u16 = 13;
const FOR_DEPTH: u8 = 10;
const REPEAT_DEPTH: u8 = 20;
const GOSUB_DEPTH: u8 = 26;
const REPEAT_LOW: u16 = page::STACKS + FOR_ENTRY as u16 * FOR_DEPTH as u16;
const REPEAT_HIGH: u16 = REPEAT_LOW + REPEAT_DEPTH as u16;
const GOSUB_LOW: u16 = REPEAT_HIGH + REPEAT_DEPTH as u16;
const GOSUB_HIGH: u16 = GOSUB_LOW + GOSUB_DEPTH as u16;
const _: () = assert!(GOSUB_HIGH + GOSUB_DEPTH as u16 <= page::STRING);

/// The FOR entry being worked on.
const ENTRY: u16 = z::STATEMENT;

pub fn emit(a: &mut Asm) {
    lines(a);
    returns(a);
    loops(a);
    for_entries(a);
    let (real, carry_on, finished) = next_loop(a);
    next_real(a, real, carry_on, finished);
    conditions(a);
    on(a);
}

fn lines(a: &mut Asm) {
    // The line a GOTO, GOSUB, THEN, ELSE or RESTORE names: an encoded
    // number, or an expression's value. LINE_NUMBER the number.
    a.here("line_target");
    a.jsr("next_a");
    a.cmp(imm(LINE_REFERENCE));
    let expression = a.local();
    a.bne(expression);
    a.lda(zp(z::TEXT_A));
    a.sta(zp(super::list::LINE));
    a.lda(zp(z::TEXT_A + 1));
    a.sta(zp(super::list::LINE + 1));
    a.jsr("decode_reference");
    a.stx(zp(LINE_NUMBER));
    a.sty(zp(LINE_NUMBER + 1));
    a.lda(zp(z::OFFSET_A));
    a.clc();
    a.adc(imm(3));
    a.sta(zp(z::OFFSET_A));
    a.rts();
    a.bind(expression);
    a.dec(zp(z::OFFSET_A));
    a.jsr("expr_integer");
    a.dec(zp(z::OFFSET_A));
    a.lda(zp(z::IAC));
    a.sta(zp(LINE_NUMBER));
    a.lda(zp(z::IAC + 1));
    a.sta(zp(LINE_NUMBER + 1));
    a.rts();

    // On to the line numbered LINE_NUMBER, No such line if there is none.
    a.here("goto_line");
    a.jsr("find_line");
    let found = a.local();
    a.beq(found);
    a.jmp("err_no_such_line");
    a.bind(found);
    a.lda(zp(RECORD));
    a.sta(zp(z::TEXT_A));
    a.lda(zp(RECORD + 1));
    a.sta(zp(z::TEXT_A + 1));
    a.jmp("run_record");

    a.here("statement_goto");
    a.jsr("line_target");
    a.jmp("goto_line");

    // TRACE ON, OFF, or the highest line to report.
    a.here("statement_trace");
    a.jsr("next_a");
    a.ldx(imm(0));
    a.cmp(imm(0x87));
    let (on, set) = (a.local(), a.local());
    a.beq(set);
    a.dex();
    a.stx(zp(z::TRACE_LIMIT));
    a.stx(zp(z::TRACE_LIMIT + 1));
    a.cmp(imm(0xEE));
    a.beq(on);
    a.dec(zp(z::OFFSET_A));
    a.jsr("line_target");
    a.lda(zp(LINE_NUMBER));
    a.sta(zp(z::TRACE_LIMIT));
    a.lda(zp(LINE_NUMBER + 1));
    a.sta(zp(z::TRACE_LIMIT + 1));
    a.ldx(imm(0xFF));
    a.bind(on);
    a.bind(set);
    a.stx(zp(z::TRACE));
    a.jmp("statement_done");

    // A line's number in brackets as it starts, while TRACE reports it.
    a.here("trace_line");
    a.ldy(imm(2));
    a.lda(zp(z::TRACE_LIMIT));
    a.cmp(zp(z::TEXT_A).ind_y());
    a.dey();
    a.lda(zp(z::TRACE_LIMIT + 1));
    a.sbc(zp(z::TEXT_A).ind_y());
    a.bcc(RTS);
    a.lda(imm(b'['));
    a.jsr("print_char");
    a.ldy(imm(2));
    a.lda(zp(z::TEXT_A).ind_y());
    a.tax();
    a.dey();
    a.lda(zp(z::TEXT_A).ind_y());
    a.tay();
    a.jsr("print_unsigned");
    a.lda(imm(b']'));
    a.jsr("print_char");
    a.jmp("print_space");
}

fn returns(a: &mut Asm) {
    // The address of the next character to read, in XY.
    a.here("return_point");
    a.lda(zp(z::OFFSET_A));
    a.clc();
    a.adc(zp(z::TEXT_A));
    a.tax();
    a.lda(zp(z::TEXT_A + 1));
    a.adc(imm(0));
    a.tay();
    a.rts();

    // Carry on from the address XY: at its next statement, or ending the
    // statement there.
    a.here("resume_statement");
    a.jsr("resume_pointer");
    a.jmp("statement");
    a.here("resume_at");
    a.jsr("resume_pointer");
    a.jmp("statement_done");
    a.here("resume_pointer");
    a.stx(zp(z::TEXT_A));
    a.sty(zp(z::TEXT_A + 1));
    a.lda(imm(0));
    a.sta(zp(z::OFFSET_A));
    a.rts();

    a.here("statement_gosub");
    a.jsr("line_target");
    a.here("gosub_line");
    a.ldx(zp(z::GOSUBS));
    a.cpx(imm(GOSUB_DEPTH));
    let room = a.local();
    a.bcc(room);
    a.jmp("err_too_many_gosubs");
    a.bind(room);
    a.jsr("return_point");
    a.tya();
    a.ldy(zp(z::GOSUBS));
    a.sta(abs(GOSUB_HIGH).y());
    a.txa();
    a.sta(abs(GOSUB_LOW).y());
    a.inc(zp(z::GOSUBS));
    a.jmp("goto_line");

    a.here("statement_return");
    a.ldy(zp(z::GOSUBS));
    let some = a.local();
    a.bne(some);
    a.jmp("err_no_gosub");
    a.bind(some);
    a.dey();
    a.sty(zp(z::GOSUBS));
    a.ldx(abs(GOSUB_LOW).y());
    a.lda(abs(GOSUB_HIGH).y());
    a.tay();
    a.jmp("resume_at");

    // REPEAT: its return point kept, then the statement after it.
    a.here("statement_repeat");
    a.ldx(zp(z::REPEATS));
    a.cpx(imm(REPEAT_DEPTH));
    let room = a.local();
    a.bcc(room);
    a.jmp("err_too_many_repeats");
    a.bind(room);
    a.jsr("return_point");
    a.tya();
    a.ldy(zp(z::REPEATS));
    a.sta(abs(REPEAT_HIGH).y());
    a.txa();
    a.sta(abs(REPEAT_LOW).y());
    a.inc(zp(z::REPEATS));
    a.jmp("statement");

    // UNTIL: back to after the REPEAT while the condition is false.
    a.here("statement_until");
    a.lda(zp(z::REPEATS));
    let some = a.local();
    a.bne(some);
    a.jmp("err_no_repeat");
    a.bind(some);
    a.jsr("condition");
    let done = a.local();
    a.bne(done);
    a.ldy(zp(z::REPEATS));
    a.dey();
    a.lda(abs(REPEAT_LOW).y());
    a.tax();
    a.lda(abs(REPEAT_HIGH).y());
    a.tay();
    a.jmp("resume_statement");
    a.bind(done);
    a.dec(zp(z::REPEATS));
    a.jmp("statement_done_x");
}

fn loops(a: &mut Asm) {
    // FOR v = start TO limit [STEP step]: the entry reserved first, so a
    // loop an FN in an expression runs stacks above it; then the variable
    // set and the limit and step stored in the variable's type.
    a.here("statement_for");
    a.jsr("lvalue");
    let (bad_variable, mistake) = (a.local(), a.local());
    a.bcs(bad_variable);
    a.lda(zp(z::VTYPE));
    a.cmp(imm(place::INTEGER));
    let numeric = a.local();
    a.beq(numeric);
    a.cmp(imm(place::REAL));
    a.beq(numeric);
    a.bind(bad_variable);
    a.jmp("err_for_variable");
    a.bind(numeric);
    a.ldx(zp(z::FORS));
    a.cpx(imm(FOR_ENTRY * FOR_DEPTH));
    let room = a.local();
    a.bcc(room);
    a.jmp("err_too_many_fors");
    a.bind(room);
    a.txa();
    a.clc();
    a.adc(imm(FOR_ENTRY));
    a.sta(zp(z::FORS));
    a.lda(zp(z::VAR));
    a.sta(abs(page::STACKS).x());
    a.lda(zp(z::VAR + 1));
    a.sta(abs(page::STACKS + 1).x());
    a.lda(zp(z::VTYPE));
    a.sta(abs(page::STACKS + FOR_TYPE).x());
    a.jsr("next_a");
    a.cmp(imm(b'='));
    a.bne(mistake);
    a.jsr("expr");
    a.stx(zp(z::OP));
    a.pha();
    a.jsr("for_entry");
    a.lda(abs(page::STACKS).x());
    a.sta(zp(z::VAR));
    a.lda(abs(page::STACKS + 1).x());
    a.sta(zp(z::VAR + 1));
    a.lda(abs(page::STACKS + FOR_TYPE).x());
    a.sta(zp(z::VTYPE));
    a.pla();
    a.jsr("store_variable");
    a.lda(zp(z::OP));
    a.cmp(imm(TO));
    let to = a.local();
    a.beq(to);
    a.jmp("err_no_to");
    a.bind(mistake);
    a.jmp("err_mistake");
    a.bind(to);
    a.jsr("expr");
    a.ldy(imm(lo(FOR_LIMIT)));
    a.jsr("store_for_value");
    a.cpx(imm(STEP));
    let (stepped, given) = (a.local(), a.local());
    a.beq(stepped);
    a.stx(zp(z::OP));
    a.jsr("int_one");
    a.jmp(given);
    a.bind(stepped);
    a.jsr("expr");
    a.stx(zp(z::OP));
    a.bind(given);
    a.ldy(imm(lo(FOR_STEP)));
    a.jsr("store_for_value");
    // NEXT returns to the character the FOR ended at.
    a.dec(zp(z::OFFSET_A));
    a.jsr("return_point");
    a.inc(zp(z::OFFSET_A));
    a.txa();
    a.pha();
    a.jsr("for_entry");
    a.pla();
    a.sta(abs(page::STACKS + FOR_RETURN).x());
    a.tya();
    a.sta(abs(page::STACKS + FOR_RETURN + 1).x());
    a.ldx(zp(z::OP));
    a.jmp("statement_done_x");
}

fn for_entries(a: &mut Asm) {
    // X the FOR entry being made.
    a.here("for_entry");
    a.lda(zp(z::FORS));
    a.sec();
    a.sbc(imm(FOR_ENTRY));
    a.tax();
    a.rts();

    // The value of kind A stored in the entry's field at offset Y, made
    // the entry's type; X kept.
    a.here("store_for_value");
    a.sta(zp(z::TYPE));
    a.stx(zp(ENTRY));
    a.jsr("for_entry");
    a.tya();
    a.stx(zp(z::VAR));
    a.clc();
    a.adc(zp(z::VAR));
    a.sta(zp(z::VAR));
    a.lda(imm(hi(page::STACKS)));
    a.sta(zp(z::VAR + 1));
    a.lda(abs(page::STACKS + FOR_TYPE).x());
    a.sta(zp(z::VTYPE));
    a.lda(zp(z::TYPE));
    a.jsr("store_variable");
    a.ldx(zp(ENTRY));
    a.rts();

    a.here("int_one");
    a.jsr("int_zero");
    a.inc(zp(z::IAC));
    a.lda(imm(kind::INTEGER));
    a.rts();
}

fn next_loop(a: &mut Asm) -> (Label, Label, Label) {
    // NEXT [v[, v...]]: the entry for the variable, or the innermost; the
    // step added, and back to the FOR unless past the limit: above it for
    // a step not below zero, below it otherwise.
    a.here("statement_next");
    a.lda(zp(z::FORS));
    let some = a.local();
    a.bne(some);
    a.jmp("err_no_for");
    a.bind(some);
    a.jsr("next_a");
    a.cmp(imm(b'@'));
    let (named, innermost) = (a.local(), a.local());
    a.beq(named);
    a.jsr("is_name");
    a.bcs(named);
    // C is set from for_entry: the depth is at least an entry.
    a.dec(zp(z::OFFSET_A));
    a.jsr("for_entry");
    a.bcs(innermost);
    a.bind(named);
    a.dec(zp(z::OFFSET_A));
    a.jsr("variable");
    let (search, matched) = (a.local(), a.local());
    a.lda(zp(z::FORS));
    a.bind(search);
    a.sec();
    a.sbc(imm(FOR_ENTRY));
    let unmatched = a.local();
    a.bcc(unmatched);
    a.tax();
    a.lda(abs(page::STACKS).x());
    a.cmp(zp(z::VAR));
    let other = a.local();
    a.bne(other);
    a.lda(abs(page::STACKS + 1).x());
    a.cmp(zp(z::VAR + 1));
    a.beq(matched);
    a.bind(other);
    a.txa();
    a.bne(search);
    a.bind(unmatched);
    a.jmp("err_cant_match_for");
    a.bind(matched);
    a.txa();
    // The entry, X and A, is innermost now.
    a.bind(innermost);
    a.sta(zp(ENTRY));
    a.clc();
    a.adc(imm(FOR_ENTRY));
    a.sta(zp(z::FORS));
    a.lda(abs(page::STACKS).x());
    a.sta(zp(z::VAR));
    a.lda(abs(page::STACKS + 1).x());
    a.sta(zp(z::VAR + 1));
    a.lda(abs(page::STACKS + FOR_TYPE).x());
    a.cmp(imm(place::REAL));
    let (real, carry_on, finished, decide) = (a.local(), a.local(), a.local(), a.local());
    a.beq(real);
    // An integer loop: v += step; equal to the limit carries on, else the
    // difference's sign against the step's decides.
    a.ldy(imm(0));
    a.clc();
    for i in 0..4 {
        if i > 0 {
            a.iny();
        }
        a.lda(zp(z::VAR).ind_y());
        a.adc(abs(page::STACKS + FOR_STEP + i).x());
        a.sta(zp(z::VAR).ind_y());
        a.sta(zp(z::IAC + i));
    }
    for i in 0..4 {
        a.lda(abs(page::STACKS + FOR_LIMIT + i).x());
        a.cmp(zp(z::IAC + i));
        a.bne(decide);
    }
    a.beq(carry_on);
    a.bind(decide);
    a.lda(abs(page::STACKS + FOR_LIMIT).x());
    a.cmp(zp(z::IAC));
    for i in 1..4 {
        a.lda(abs(page::STACKS + FOR_LIMIT + i).x());
        a.sbc(zp(z::IAC + i));
    }
    let signed = a.local();
    a.bvc(signed);
    a.eor(imm(0x80));
    a.bind(signed);
    a.eor(abs(page::STACKS + FOR_STEP + 3).x());
    a.bmi(finished);
    a.bpl(carry_on);
    (real, carry_on, finished)
}

fn next_real(a: &mut Asm, real: Label, carry_on: Label, finished: Label) {
    // A real loop: v += step in reals, then the comparison.
    a.bind(real);
    a.jsr("fac_load");
    a.ldy(imm(lo(FOR_STEP)));
    a.jsr("var_at_field");
    a.jsr("fac2_load");
    a.jsr("fac_add");
    a.ldx(zp(ENTRY));
    a.lda(abs(page::STACKS).x());
    a.sta(zp(z::VAR));
    a.lda(abs(page::STACKS + 1).x());
    a.sta(zp(z::VAR + 1));
    a.jsr("fac_store");
    a.ldy(imm(lo(FOR_LIMIT)));
    a.jsr("var_at_field");
    a.jsr("fac2_load");
    a.jsr("fac_compare");
    a.beq(carry_on);
    a.ldx(zp(ENTRY));
    a.eor(abs(page::STACKS + FOR_STEP + 1).x());
    a.bpl(carry_on);
    a.bind(finished);
    a.lda(zp(ENTRY));
    a.sta(zp(z::FORS));
    a.jsr("next_a");
    a.cmp(imm(b','));
    let ended = a.local();
    a.bne(ended);
    a.jmp("statement_next");
    a.bind(ended);
    a.tax();
    a.jmp("statement_done_x");
    a.bind(carry_on);
    a.ldx(zp(ENTRY));
    a.ldy(abs(page::STACKS + FOR_RETURN + 1).x());
    a.lda(abs(page::STACKS + FOR_RETURN).x());
    a.tax();
    a.jmp("resume_at");

    // VAR at the entry's field Y.
    a.here("var_at_field");
    a.tya();
    a.clc();
    a.adc(zp(ENTRY));
    a.sta(zp(z::VAR));
    a.lda(imm(hi(page::STACKS)));
    a.sta(zp(z::VAR + 1));
    a.rts();
}

fn conditions(a: &mut Asm) {
    // A condition: an expression's truth in Z (clear for true), X the
    // character after it.
    a.here("condition");
    a.jsr("expr");
    a.tay();
    let real = a.local();
    a.bmi(real);
    a.bne("condition_integer");
    a.jmp("err_type_mismatch");
    a.here("condition_integer");
    a.lda(zp(z::IAC));
    a.ora(zp(z::IAC + 1));
    a.ora(zp(z::IAC + 2));
    a.ora(zp(z::IAC + 3));
    a.rts();
    a.bind(real);
    a.lda(zp(super::float::FAC.m(0)));
    a.rts();

    // IF: on true, THEN's line or statements, else the statements after the
    // condition; on false, the line's ELSE part or its next line.
    a.here("statement_if");
    a.jsr("condition");
    let false_ = a.local();
    a.beq(false_);
    a.cpx(imm(THEN));
    let then = a.local();
    a.beq(then);
    a.dec(zp(z::OFFSET_A));
    a.jmp("statement");
    a.bind(then);
    a.here("branch_part");
    a.jsr("next_a");
    a.cmp(imm(LINE_REFERENCE));
    let statements = a.local();
    a.bne(statements);
    a.dec(zp(z::OFFSET_A));
    a.jmp("statement_goto");
    a.bind(statements);
    a.dec(zp(z::OFFSET_A));
    a.jmp("statement");
    a.bind(false_);
    a.dec(zp(z::OFFSET_A));
    a.ldy(zp(z::OFFSET_A));
    let (scan, found) = (a.here_local(), a.local());
    a.lda(zp(z::TEXT_A).ind_y());
    a.iny();
    a.cmp(imm(ELSE));
    a.beq(found);
    a.cmp(imm(CR));
    a.bne(scan);
    a.dey();
    a.sty(zp(z::OFFSET_A));
    a.jmp("statement_done");
    a.bind(found);
    a.sty(zp(z::OFFSET_A));
    a.jmp("branch_part");
}

fn on(a: &mut Asm) {
    // ON n GOTO|GOSUB lines [ELSE statements]: the nth line, else the ELSE
    // part, else ON range.
    a.here("statement_on");
    a.jsr("next_a");
    a.cmp(imm(0x85));
    let not_error = a.local();
    a.bne(not_error);
    a.jmp("statement_on_error");
    a.bind(not_error);
    a.dec(zp(z::OFFSET_A));
    a.jsr("expr_integer");
    a.cpx(imm(GOTO));
    let kind = a.local();
    a.beq(kind);
    a.cpx(imm(GOSUB));
    a.beq(kind);
    a.jmp("err_on_syntax");
    a.bind(kind);
    a.txa();
    a.pha();
    // The count down from n through the list; past its end, or out of
    // range, the ELSE part if there is one.
    let (item, chosen, range, skip, else_part, missing) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(zp(z::IAC + 1));
    a.ora(zp(z::IAC + 2));
    a.ora(zp(z::IAC + 3));
    a.bne(range);
    a.ldx(zp(z::IAC));
    a.beq(range);
    a.bind(item);
    a.dex();
    a.beq(chosen);
    a.bind(skip);
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.beq(item);
    a.cmp(imm(ELSE));
    a.beq(else_part);
    a.cmp(imm(CR));
    a.beq(missing);
    a.cmp(imm(b':'));
    a.bne(skip);
    a.bind(missing);
    a.jmp("err_on_range");
    a.bind(range);
    a.jsr("next_a");
    a.cmp(imm(ELSE));
    a.beq(else_part);
    a.cmp(imm(CR));
    a.beq(missing);
    a.cmp(imm(b':'));
    a.bne(range);
    a.beq(missing);
    a.bind(else_part);
    a.pla();
    a.jmp("branch_part");
    a.bind(chosen);
    a.jsr("line_target");
    // The statement's end, for GOSUB's return.
    let end = a.here_local();
    a.jsr("next_a");
    a.jsr("ends_statement");
    a.bne(end);
    a.dec(zp(z::OFFSET_A));
    a.pla();
    a.cmp(imm(GOTO));
    let gosub = a.local();
    a.bne(gosub);
    a.jmp("goto_line");
    a.bind(gosub);
    a.jmp("gosub_line");
}
