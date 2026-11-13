//! PROC and FN: finding a definition and keeping it in its list, calls
//! with their parameters, LOCAL, ENDPROC and `=`.
//!
//! A call stacks its actuals, each with its kind below it, then a frame:
//! the PROC or FN token, the statement pointer, the stack below the
//! actuals, and a copy of the 6502 stack. The formals are made local, then
//! given the stacked values, which were all found before any formal
//! changed. A local's record is its type, its address and its value as
//! pushed. Leaving restores the locals down to the frame, then the frame
//! and the 6502 stack, and returns where the call was made: the top byte
//! of each record says which it is, so no frame pointer is kept.

use super::float::PACKED;
use super::vars::{FROM, NAME_END, S_LENGTH, TNAME};
use super::ws::{kind, page, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm, Label, RTS};

const PROC: u8 = 0xF2;
const FN: u8 = 0xA4;
const DEF: u8 = 0xDD;

/// A frame's fields from its lowest byte, then the 6502 stack's length
/// and bytes.
const F_OFFSET: u8 = 1;
const F_TEXT: u8 = 2;
const F_BASE: u8 = 4;
const F_LENGTH: u8 = 6;
const F_BYTES: u8 = 7;
/// A local's record: its type, address, then value.
const L_ADDRESS: u8 = 1;
const L_VALUE: u8 = 3;
const STACK_TOP: u8 = 0xFF;

/// A call's kind and count of actuals; the stack below its actuals, then a
/// formal's location; the walk through the actuals.
const KIND: u16 = z::STATEMENT;
const COUNT: u16 = KIND + 1;
const BASE: u16 = COUNT + 1;
const WALK: u16 = BASE + 2;
const LOCATION: u16 = BASE;
const LOCATION_TYPE: u16 = KIND;
/// The definition being compared, biased as TNAME is.
const DEFINED: u16 = BASE;

pub fn emit(a: &mut Asm) {
    let (search, found) = finding(a);
    definition_search(a, search, found);
    calling(a);
    call_frame(a);
    actuals(a);
    locals(a);
    clearing(a);
    leaving(a);
}

fn finding(a: &mut Asm) -> (Label, Label) {
    // The definition of the PROC or FN whose token is A, named at the
    // statement pointer: from its list, or found among the lines that start
    // with DEF and added to the list. TEXT_B the address after the name in
    // its DEF line; the statement pointer past the name.
    a.here("find_definition");
    a.pha();
    a.ldx(imm(lo(page::FN_LIST)));
    a.cmp(imm(FN));
    let listed = a.local();
    a.beq(listed);
    a.ldx(imm(lo(page::PROC_LIST)));
    a.bind(listed);
    a.stx(zp(z::VAR));
    a.lda(imm(hi(page::PROC_LIST)));
    a.sta(zp(z::VAR + 1));
    a.ldy(zp(z::OFFSET_A));
    a.dey();
    let letters = a.here_local();
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.jsr("is_name");
    a.bcs(letters);
    a.tya();
    a.sec();
    a.sbc(zp(z::OFFSET_A));
    a.clc();
    a.adc(imm(2));
    a.sta(zp(NAME_END));
    a.lda(zp(z::OFFSET_A));
    a.sty(zp(z::OFFSET_A));
    a.clc();
    a.adc(zp(z::TEXT_A));
    a.sta(zp(TNAME));
    a.lda(zp(z::TEXT_A + 1));
    a.adc(imm(0));
    a.sta(zp(TNAME + 1));
    a.lda(zp(TNAME));
    a.sec();
    a.sbc(imm(2));
    a.sta(zp(TNAME));
    let same = a.local();
    a.bcs(same);
    a.dec(zp(TNAME + 1));
    a.bind(same);
    a.jsr("search_list");
    let (search, found) = (a.local(), a.local());
    a.bcs(search);
    a.ldy(imm(0));
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(z::TEXT_B));
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(z::TEXT_B + 1));
    a.pla();
    a.rts();
    (search, found)
}

fn definition_search(a: &mut Asm, search: Label, found: Label) {
    // Each line from PAGE whose first statement is DEF of this kind and
    // name; the list's last entry kept to link the new one from.
    a.bind(search);
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(imm(0));
    a.sta(zp(z::TEXT_B));
    a.lda(zp(z::PAGE));
    a.sta(zp(z::TEXT_B + 1));
    let (line, next_line) = (a.here_local(), a.local());
    a.ldy(imm(1));
    a.lda(zp(z::TEXT_B).ind_y());
    let present = a.local();
    a.bpl(present);
    a.jmp("err_no_such_fn_proc");
    a.bind(present);
    a.ldy(imm(3));
    a.jsr("skip_spaces_b");
    a.cmp(imm(DEF));
    a.bne(next_line);
    a.jsr("skip_spaces_b");
    // The kind waits below the list entry's address.
    a.tsx();
    a.cmp(abs(0x0103).x());
    a.bne(next_line);
    // The name from Y + 1, read with TNAME's bias.
    a.tya();
    a.sec();
    a.sbc(imm(1));
    a.clc();
    a.adc(zp(z::TEXT_B));
    a.sta(zp(DEFINED));
    a.lda(zp(z::TEXT_B + 1));
    a.adc(imm(0));
    a.sta(zp(DEFINED + 1));
    a.ldy(imm(2));
    let (compare, ended) = (a.here_local(), a.local());
    a.cpy(zp(NAME_END));
    a.beq(ended);
    a.lda(zp(DEFINED).ind_y());
    a.cmp(zp(TNAME).ind_y());
    a.bne(next_line);
    a.iny();
    a.bne(compare);
    a.bind(ended);
    a.lda(zp(DEFINED).ind_y());
    a.jsr("is_name");
    a.bcc(found);
    a.bind(next_line);
    a.ldy(imm(3));
    a.lda(zp(z::TEXT_B).ind_y());
    a.clc();
    a.adc(zp(z::TEXT_B));
    a.sta(zp(z::TEXT_B));
    a.bcc(line);
    a.inc(zp(z::TEXT_B + 1));
    a.jmp(line);
    // Found: the address after the name, kept in a new list entry.
    a.bind(found);
    a.tya();
    a.clc();
    a.adc(zp(DEFINED));
    a.sta(zp(z::TEXT_B));
    a.lda(zp(DEFINED + 1));
    a.adc(imm(0));
    a.sta(zp(z::TEXT_B + 1));
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.lda(imm(2));
    a.ldx(imm(0));
    a.jsr("create_entry");
    a.ldy(imm(0));
    a.lda(zp(z::TEXT_B));
    a.sta(zp(z::VAR).ind_y());
    a.iny();
    a.lda(zp(z::TEXT_B + 1));
    a.sta(zp(z::VAR).ind_y());
    a.pla();
    a.rts();

    // From (TEXT_B),Y + 1, the first character that is not a space; Y at it.
    a.here("skip_spaces_b");
    let space = a.here_local();
    a.iny();
    a.lda(zp(z::TEXT_B).ind_y());
    a.cmp(imm(b' '));
    a.beq(space);
    a.rts();
}

fn calling(a: &mut Asm) {
    // PROC name[(actuals)].
    a.here("statement_proc");
    a.lda(imm(PROC));
    a.jsr("call_routine");
    a.jmp("statement_done");

    // FN name[(actuals)], its value as any function's.
    a.here("fn_fn");
    a.lda(imm(FN));
    a.jsr("call_routine");
    a.lda(zp(z::TYPE));
    a.rts();

    // The call of the PROC or FN whose token is A. The kind, definition,
    // stack and count wait on the 6502 stack while the actuals are found.
    a.here("call_routine");
    a.pha();
    a.jsr("find_definition");
    a.lda(zp(z::TEXT_B));
    a.pha();
    a.lda(zp(z::TEXT_B + 1));
    a.pha();
    a.lda(zp(z::STACK));
    a.pha();
    a.lda(zp(z::STACK + 1));
    a.pha();
    a.lda(imm(0));
    a.pha();
    a.jsr("next_a");
    a.cmp(imm(b'('));
    let (none, found) = (a.local(), a.local());
    a.bne(none);
    let actual = a.here_local();
    a.jsr("expr");
    a.stx(zp(z::OP));
    a.sta(zp(z::TYPE));
    a.jsr("push_value");
    a.lda(imm(1));
    a.sec();
    a.jsr("stack_claim");
    a.ldy(imm(0));
    a.lda(zp(z::TYPE));
    a.sta(zp(z::STACK).ind_y());
    a.pla();
    a.clc();
    a.adc(imm(1));
    a.pha();
    a.ldx(zp(z::OP));
    a.cpx(imm(b','));
    a.beq(actual);
    a.jsr("closing_bracket");
    a.jmp(found);
    a.bind(none);
    a.dec(zp(z::OFFSET_A));
    a.bind(found);
    a.pla();
    a.sta(zp(COUNT));
    a.pla();
    a.sta(zp(BASE + 1));
    a.pla();
    a.sta(zp(BASE));
    a.pla();
    a.sta(zp(z::TEXT_B + 1));
    a.pla();
    a.sta(zp(z::TEXT_B));
    a.pla();
    a.sta(zp(KIND));
    a.lda(zp(z::STACK));
    a.sta(zp(WALK));
    a.lda(zp(z::STACK + 1));
    a.sta(zp(WALK + 1));
}

fn call_frame(a: &mut Asm) {
    // The frame, the 6502 stack copied into it from just above its pointer.
    a.tsx();
    a.stx(zp(z::OP));
    a.txa();
    a.eor(imm(STACK_TOP));
    a.pha();
    a.clc();
    a.adc(imm(F_BYTES));
    let fits = a.local();
    a.bcc(fits);
    a.jmp("err_no_room");
    a.bind(fits);
    a.sec();
    a.jsr("stack_claim");
    a.ldy(imm(F_LENGTH));
    a.pla();
    a.sta(zp(z::STACK).ind_y());
    for (field, from) in [
        (0, KIND),
        (F_OFFSET, z::OFFSET_A),
        (F_TEXT, z::TEXT_A),
        (F_TEXT + 1, z::TEXT_A + 1),
        (F_BASE, BASE),
        (F_BASE + 1, BASE + 1),
    ] {
        a.ldy(imm(field));
        a.lda(zp(from));
        a.sta(zp(z::STACK).ind_y());
    }
    a.ldy(imm(F_BYTES));
    a.ldx(zp(z::OP));
    let (copy, copied) = (a.here_local(), a.local());
    a.cpx(imm(STACK_TOP));
    a.beq(copied);
    a.inx();
    a.lda(abs(0x0100).x());
    a.sta(zp(z::STACK).ind_y());
    a.iny();
    a.bne(copy);
    a.bind(copied);
    a.ldx(imm(STACK_TOP));
    a.txs();
    // The body from the definition: its formals first.
    a.lda(zp(z::TEXT_B));
    a.sta(zp(z::TEXT_A));
    a.lda(zp(z::TEXT_B + 1));
    a.sta(zp(z::TEXT_A + 1));
    a.lda(imm(0));
    a.sta(zp(z::OFFSET_A));
    a.jsr("next_a");
    a.cmp(imm(b'('));
    let (formals, mismatch) = (a.local(), a.local());
    a.beq(formals);
    a.dec(zp(z::OFFSET_A));
    a.lda(zp(COUNT));
    a.bne(mismatch);
    a.jmp("statement");
    a.bind(mismatch);
    a.jmp("err_arguments");
    a.bind(formals);
    let formal = a.here_local();
    a.jsr("make_local");
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VTYPE));
    a.pha();
    a.dec(zp(COUNT));
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.beq(formal);
    a.tax();
    a.jsr("closing_bracket");
    a.lda(zp(COUNT));
    a.bne(mismatch);
    // The actuals from the lowest, the last formal's first.
    let (bind, bound) = (a.here_local(), a.local());
    a.tsx();
    a.cpx(imm(STACK_TOP));
    a.beq(bound);
    a.pla();
    a.sta(zp(LOCATION_TYPE));
    a.pla();
    a.sta(zp(LOCATION + 1));
    a.pla();
    a.sta(zp(LOCATION));
    a.jsr("load_actual");
    a.ldx(zp(LOCATION));
    a.stx(zp(z::VAR));
    a.ldx(zp(LOCATION + 1));
    a.stx(zp(z::VAR + 1));
    a.ldx(zp(LOCATION_TYPE));
    a.stx(zp(z::VTYPE));
    a.jsr("store_variable");
    a.jmp(bind);
    a.bind(bound);
    a.jmp("statement");
}

fn actuals(a: &mut Asm) {
    // The actual at WALK loaded, WALK past it: A its kind.
    a.here("load_actual");
    a.ldy(imm(0));
    a.lda(zp(WALK).ind_y());
    a.pha();
    a.jsr("walk_one");
    a.pla();
    a.pha();
    let (string, real, loaded) = (a.local(), a.local(), a.local());
    a.beq(string);
    a.bmi(real);
    a.ldy(imm(3));
    let copy = a.here_local();
    a.lda(zp(WALK).ind_y());
    a.sta(abs(z::IAC).y());
    a.dey();
    a.bpl(copy);
    a.lda(imm(4));
    a.clc();
    a.bcc(loaded);
    a.bind(real);
    a.lda(zp(WALK));
    a.sta(zp(z::VAR));
    a.lda(zp(WALK + 1));
    a.sta(zp(z::VAR + 1));
    a.jsr("fac_load");
    a.lda(imm(PACKED));
    a.clc();
    a.bcc(loaded);
    a.bind(string);
    a.lda(zp(WALK).ind_y());
    a.sta(zp(z::STRING_LENGTH));
    a.tay();
    let text = a.here_local();
    let texted = a.local();
    a.beq(texted);
    a.lda(zp(WALK).ind_y());
    a.sta(abs(page::STRING - 1).y());
    a.dey();
    a.jmp(text);
    a.bind(texted);
    a.lda(zp(z::STRING_LENGTH));
    a.sec();
    a.adc(imm(0));
    a.bind(loaded);
    a.jsr("walk_by");
    a.pla();
    a.rts();

    // WALK on one byte; and on A, 256 more with C set.
    a.here("walk_one");
    a.lda(imm(1));
    a.clc();
    a.here("walk_by");
    let low = a.local();
    a.bcc(low);
    a.inc(zp(WALK + 1));
    a.clc();
    a.bind(low);
    a.adc(zp(WALK));
    a.sta(zp(WALK));
    let same = a.local();
    a.bcc(same);
    a.inc(zp(WALK + 1));
    a.bind(same);
    a.rts();
}

fn locals(a: &mut Asm) {
    // LOCAL v[, v...]: each made local and cleared; only within a PROC or
    // FN.
    a.here("statement_local");
    a.jsr("in_routine");
    let inside = a.local();
    a.bcc(inside);
    a.jmp("err_not_local");
    a.bind(inside);
    let next = a.here_local();
    a.jsr("make_local");
    a.jsr("clear_location");
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.beq(next);
    a.tax();
    a.jmp("statement_done_x");

    // C clear if the stack's top is a local's record or a frame; A its
    // first byte.
    a.here("in_routine");
    a.lda(zp(z::STACK));
    a.cmp(zp(z::HIMEM));
    a.lda(zp(z::STACK + 1));
    a.sbc(zp(z::HIMEM + 1));
    let empty = a.local();
    a.bcs(empty);
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.clc();
    a.rts();
    a.bind(empty);
    a.rts();

    // The variable at the statement pointer made local: its value, address
    // and type pushed; VAR and VTYPE its location. A byte, as in BASIC II,
    // is neither kept nor, after, put back.
    a.here("make_local");
    a.jsr("lvalue");
    let located = a.local();
    a.bcc(located);
    a.jmp("err_mistake");
    a.bind(located);
    a.lda(zp(z::VTYPE));
    a.beq(RTS);
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VAR));
    a.pha();
    a.jsr("load_variable");
    a.jsr("push_value");
    a.lda(imm(L_VALUE));
    a.sec();
    a.jsr("stack_claim");
    a.ldy(imm(0));
    a.lda(zp(z::VTYPE));
    a.sta(zp(z::STACK).ind_y());
    for byte in 0..2 {
        a.iny();
        a.pla();
        a.sta(zp(z::VAR + byte));
        a.sta(zp(z::STACK).ind_y());
    }
    a.rts();
}

fn clearing(a: &mut Asm) {
    // The location as LOCAL leaves it: a number 0, and a string of either
    // kind its descriptor's length 0, as BASIC II clears both; a byte as
    // it was.
    a.here("clear_location");
    a.lda(zp(z::VTYPE));
    a.beq(RTS);
    let string = a.local();
    a.bmi(string);
    a.jsr("int_zero");
    a.lda(imm(kind::INTEGER));
    a.jmp("store_variable");
    a.bind(string);
    a.ldy(imm(S_LENGTH));
    a.lda(imm(0));
    a.sta(zp(z::VAR).ind_y());
    a.rts();

    // The local whose record is the stack's top restored, the record
    // dropped. A string's text goes back without the string buffer, which
    // may hold an FN's value.
    a.here("pop_local");
    a.ldy(imm(L_ADDRESS));
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(z::VAR));
    a.iny();
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(z::VAR + 1));
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(z::VTYPE));
    let string = a.local();
    a.bmi(string);
    // A number's bytes, its type their count, copied from the top down
    // once the record's head is dropped.
    a.tay();
    a.lda(imm(L_VALUE));
    a.jsr("stack_release");
    let copy = a.here_local();
    a.dey();
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(z::VAR).ind_y());
    a.tya();
    a.bne(copy);
    a.lda(zp(z::VTYPE));
    a.jmp("stack_release");
    a.bind(string);
    a.ldy(imm(L_VALUE));
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(z::STRING_LENGTH));
    a.lda(zp(z::STACK));
    a.clc();
    a.adc(imm(L_VALUE + 1));
    a.sta(zp(FROM));
    a.lda(zp(z::STACK + 1));
    a.adc(imm(0));
    a.sta(zp(FROM + 1));
    a.jsr("store_string");
    a.lda(imm(L_VALUE));
    a.jsr("stack_release");
    a.jmp("pop_string");
}

fn leaving(a: &mut Asm) {
    // ENDPROC: leaving a PROC.
    a.here("statement_endproc");
    a.lda(imm(PROC));
    a.jmp("leave");

    // `=` in an FN: its value, then leaving it.
    a.here("statement_return_value");
    a.jsr("expr");
    a.sta(zp(z::TYPE));
    a.lda(imm(FN));

    // Leaving the innermost call of kind A: its locals restored, then its
    // frame and the 6502 stack, and back where it was made. The string
    // buffer's length is kept for an FN's value.
    a.here("leave");
    a.sta(zp(KIND));
    a.lda(zp(z::STRING_LENGTH));
    a.pha();
    let (unwind, frame, outside) = (a.here_local(), a.local(), a.local());
    a.jsr("in_routine");
    a.bcs(outside);
    a.cmp(imm(PROC));
    a.beq(frame);
    a.cmp(imm(FN));
    a.beq(frame);
    a.jsr("pop_local");
    a.jmp(unwind);
    a.bind(frame);
    a.cmp(zp(KIND));
    let right = a.local();
    a.beq(right);
    a.bind(outside);
    a.lda(zp(KIND));
    a.cmp(imm(FN));
    let proc_error = a.local();
    a.bne(proc_error);
    a.jmp("err_no_fn");
    a.bind(proc_error);
    a.jmp("err_no_proc");
    a.bind(right);
    a.pla();
    a.sta(zp(z::STRING_LENGTH));
    for (field, to) in [
        (F_OFFSET, z::OFFSET_A),
        (F_TEXT, z::TEXT_A),
        (F_TEXT + 1, z::TEXT_A + 1),
        (F_BASE, BASE),
        (F_BASE + 1, BASE + 1),
    ] {
        a.ldy(imm(field));
        a.lda(zp(z::STACK).ind_y());
        a.sta(zp(to));
    }
    // The 6502 stack's pointer from the length kept, and its bytes back.
    a.ldy(imm(F_LENGTH));
    a.lda(zp(z::STACK).ind_y());
    a.eor(imm(STACK_TOP));
    a.tax();
    a.txs();
    a.ldy(imm(F_BYTES));
    let (back, restored) = (a.here_local(), a.local());
    a.cpx(imm(STACK_TOP));
    a.beq(restored);
    a.inx();
    a.lda(zp(z::STACK).ind_y());
    a.sta(abs(0x0100).x());
    a.iny();
    a.bne(back);
    a.bind(restored);
    a.lda(zp(BASE));
    a.sta(zp(z::STACK));
    a.lda(zp(BASE + 1));
    a.sta(zp(z::STACK + 1));
    a.lda(zp(z::TYPE));
    a.rts();
}
