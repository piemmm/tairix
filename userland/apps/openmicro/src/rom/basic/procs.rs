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

use tairix_openbasic::keyword::word;

use super::editor::LENGTH_AT;
use super::float::PACKED;
use super::vars::{FROM, NAME_END, S_LENGTH, TNAME};
use super::ws::{kind, page, zp as z};
use crate::cpu::STACK_PAGE;
use crate::rom::asm::{abs, byte, hi, imm, lo, zp, Asm, Label, RTS};

/// A frame from its lowest byte: what it keeps of page zero, then the 6502
/// stack's length and bytes.
const FRAME: [u16; 7] = [
    KIND,
    z::OFFSET_A,
    z::TEXT_A,
    z::TEXT_A + 1,
    BASE,
    BASE + 1,
    z::BORROWED,
];
const F_LENGTH: u8 = byte(FRAME.len());
const F_BYTES: u8 = F_LENGTH + 1;
/// A local's record: its type, address, then value.
const L_ADDRESS: u8 = 1;
const L_VALUE: u8 = 3;
const STACK_TOP: u8 = 0xFF;

/// A call's kind and count of actuals; the stack below its actuals, then a
/// formal's location; the walk through the actuals, clear of the fraction a
/// conversion to an integer marks.
const KIND: u16 = z::IAC2;
const COUNT: u16 = KIND + 1;
const BASE: u16 = COUNT + 1;
const WALK: u16 = z::INT_WORK + 1;
const LOCATION: u16 = BASE;
const LOCATION_TYPE: u16 = KIND;
/// The definition being compared, biased as TNAME is.
const DEFINED: u16 = BASE;

pub fn emit(a: &mut Asm) {
    let (search, found) = finding(a);
    definition_search(a, search, found);
    calling(a);
    let (formals, mismatch, body) = call_frame(a);
    binding(a, formals, mismatch, body);
    walking(a);
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
    a.cmp(imm(word::FN.token));
    let listed = a.local();
    a.beq(listed);
    a.ldx(imm(lo(page::PROC_LIST)));
    a.bind(listed);
    a.stx(zp(z::VAR));
    a.lda(imm(hi(page::PROC_LIST)));
    a.sta(zp(z::VAR + 1));
    // The name read through TNAME as an entry holds it, its length unknown
    // until a comparison ends it: TNAME is TEXT_A plus OFFSET_A less two,
    // which is below 0 for a name typed at the prompt.
    a.ldx(imm(0));
    a.stx(zp(NAME_END));
    a.lda(zp(z::OFFSET_A));
    a.sec();
    a.sbc(imm(2));
    let positive = a.local();
    a.bcs(positive);
    a.dex();
    a.bind(positive);
    a.clc();
    a.adc(zp(z::TEXT_A));
    a.sta(zp(TNAME));
    a.txa();
    a.adc(zp(z::TEXT_A + 1));
    a.sta(zp(TNAME + 1));
    let (search, found) = (a.local(), a.local());
    let listed_next = a.here_local();
    a.jsr("search_list");
    a.bcs(search);
    // An entry naming only the start of the name is not it.
    a.lda(zp(TNAME).ind_y());
    a.jsr("is_name");
    a.bcs(listed_next);
    past_name(a);
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(z::TEXT_B));
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(z::TEXT_B + 1));
    a.pla();
    a.rts();
    (search, found)
}

/// The statement pointer past the name whose zero, in an entry's terms, Y
/// is; C clear. The sum passes 255 for a name ending a line of the longest
/// length, so the two it is ahead by are taken off whatever its carry.
fn past_name(a: &mut Asm) {
    a.tya();
    a.adc(zp(z::OFFSET_A));
    a.sec();
    a.sbc(imm(2));
    a.sta(zp(z::OFFSET_A));
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
    a.cmp(imm(word::DEF.token));
    a.bne(next_line);
    a.jsr("skip_spaces_b");
    // The kind waits below the list entry's address.
    a.tsx();
    a.cmp(abs(STACK_PAGE + 3).x());
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
    a.lda(zp(DEFINED).ind_y());
    a.jsr("is_name");
    a.bcc(ended);
    a.cmp(zp(TNAME).ind_y());
    a.bne(next_line);
    a.iny();
    a.bne(compare);
    // The definition's name ended: the call's must end with it.
    a.bind(ended);
    a.lda(zp(TNAME).ind_y());
    a.jsr("is_name");
    a.bcc(found);
    a.bind(next_line);
    a.ldy(imm(LENGTH_AT));
    a.lda(zp(z::TEXT_B).ind_y());
    a.clc();
    a.adc(zp(z::TEXT_B));
    a.sta(zp(z::TEXT_B));
    a.bcc(line);
    a.inc(zp(z::TEXT_B + 1));
    a.jmp(line);
    // Found: the address after the name, kept in a new list entry.
    a.bind(found);
    a.sty(zp(NAME_END));
    past_name(a);
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
    // FN name[(actuals)], its value as any function's: leaving it returns
    // to the factor's end, so its frame keeps no return address of its own.
    a.here("fn_fn");
    a.lda(imm(word::FN.token));
    a.bne("call_routine");

    // PROC name[(actuals)]: its frame keeps no return address, as ENDPROC
    // goes on to the statement after it.
    a.here("statement_proc");
    a.lda(imm(word::PROC.token));
    a.fall_into("call_routine");

    // The call of the PROC or FN whose token is A. The kind, definition,
    // stack and count wait on the 6502 stack while the actuals are found.
    a.here("call_routine");
    a.pha();
    a.jsr("find_definition");
    a.jsr("next_a");
    a.cmp(imm(b'('));
    let (actuals, kind_kept) = (a.local(), a.local());
    a.beq(actuals);
    // No actuals: nothing on the BASIC stack is the call's.
    a.dec(zp(z::OFFSET_A));
    a.lda(zp(z::STACK));
    a.sta(zp(BASE));
    a.lda(zp(z::STACK + 1));
    a.sta(zp(BASE + 1));
    a.lda(imm(0));
    a.sta(zp(COUNT));
    a.beq(kind_kept);
    a.bind(actuals);
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
    a.tsx();
    a.inc(abs(STACK_PAGE + 1).x());
    a.ldx(zp(z::OP));
    a.cpx(imm(b','));
    a.beq(actual);
    a.jsr("closing_bracket");
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
    a.lda(zp(z::STACK));
    a.sta(zp(WALK));
    a.lda(zp(z::STACK + 1));
    a.sta(zp(WALK + 1));
    a.bind(kind_kept);
    a.pla();
    a.sta(zp(KIND));
}

fn call_frame(a: &mut Asm) -> (Label, Label, Label) {
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
    for from in FRAME.into_iter().rev() {
        a.dey();
        a.lda(zp(from));
        a.sta(zp(z::STACK).ind_y());
    }
    a.ldy(imm(F_BYTES));
    a.ldx(zp(z::OP));
    let copied = a.local();
    a.cpx(imm(STACK_TOP));
    a.beq(copied);
    a.inx();
    let copy = a.here_local();
    a.lda(abs(STACK_PAGE).x());
    a.sta(zp(z::STACK).ind_y());
    a.iny();
    a.inx();
    a.bne(copy);
    a.bind(copied);
    a.ldx(imm(STACK_TOP));
    a.txs();
    // The body from the definition: its formals first.
    a.lda(zp(z::TEXT_B));
    a.sta(zp(z::TEXT_A));
    a.lda(zp(z::TEXT_B + 1));
    a.sta(zp(z::TEXT_A + 1));
    // The frame keeps any borrow mark; the body's statements are its own.
    a.lda(imm(0));
    a.sta(zp(z::OFFSET_A));
    a.sta(zp(z::BORROWED));
    a.jsr("next_a");
    a.cmp(imm(b'('));
    let (formals, mismatch, body) = (a.local(), a.local(), a.local());
    a.beq(formals);
    a.ldx(zp(COUNT));
    a.bne(mismatch);
    // A body after a colon starts past it.
    a.cmp(imm(b':'));
    a.beq(body);
    a.dec(zp(z::OFFSET_A));
    a.bind(body);
    a.jmp("statement");
    a.bind(mismatch);
    a.jmp("err_arguments");
    (formals, mismatch, body)
}

/// Each formal made local, then given its actual.
fn binding(a: &mut Asm, formals: Label, mismatch: Label, body: Label) {
    a.bind(formals);
    let formal = a.here_local();
    // A formal may be an array element, its subscripts any BASIC.
    a.lda(zp(COUNT));
    a.pha();
    a.lda(zp(WALK));
    a.pha();
    a.lda(zp(WALK + 1));
    a.pha();
    a.jsr("make_local");
    a.pla();
    a.sta(zp(WALK + 1));
    a.pla();
    a.sta(zp(WALK));
    a.pla();
    a.sta(zp(COUNT));
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
    let bind = a.here_local();
    a.tsx();
    a.cpx(imm(STACK_TOP));
    a.beq(body);
    a.pla();
    a.sta(zp(LOCATION_TYPE));
    a.pla();
    a.sta(zp(LOCATION + 1));
    a.pla();
    a.sta(zp(LOCATION));
    actual(a);
    a.ldx(zp(LOCATION));
    a.stx(zp(z::VAR));
    a.ldx(zp(LOCATION + 1));
    a.stx(zp(z::VAR + 1));
    a.ldx(zp(LOCATION_TYPE));
    a.stx(zp(z::VTYPE));
    a.jsr("store_variable");
    a.jmp(bind);
}

/// The actual at WALK loaded, WALK past it: A its kind.
fn actual(a: &mut Asm) {
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
    let texted = a.local();
    a.beq(texted);
    let text = a.here_local();
    a.lda(zp(WALK).ind_y());
    a.sta(abs(page::STRING - 1).y());
    a.dey();
    a.bne(text);
    a.bind(texted);
    a.lda(zp(z::STRING_LENGTH));
    a.sec();
    a.adc(imm(0));
    a.bind(loaded);
    a.jsr("walk_by");
    a.pla();
}

fn walking(a: &mut Asm) {
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
    a.jmp("statement_done_a");

    // C clear if the stack's top is a local's record or a frame; A its
    // first byte.
    a.here("in_routine");
    a.lda(zp(z::STACK));
    a.cmp(zp(z::HIMEM));
    a.lda(zp(z::STACK + 1));
    a.sbc(zp(z::HIMEM + 1));
    a.bcs(RTS);
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.clc();
    a.rts();

    // The variable at the statement pointer made local: its value, address
    // and type pushed; VAR and VTYPE its location. A byte, as in BASIC II,
    // is neither kept nor, after, put back.
    a.here("make_local");
    a.jsr("location");
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
    a.lda(imm(word::PROC.token));
    a.bne("leave");

    // `=` in an FN: its value, then leaving it.
    a.here("statement_return_value");
    a.jsr("expr");
    a.sta(zp(z::TYPE));
    a.lda(imm(word::FN.token));

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
    a.cmp(imm(word::PROC.token));
    a.beq(frame);
    a.cmp(imm(word::FN.token));
    a.beq(frame);
    a.jsr("pop_local");
    a.jmp(unwind);
    a.bind(frame);
    a.cmp(zp(KIND));
    let right = a.local();
    a.beq(right);
    a.bind(outside);
    a.lda(zp(KIND));
    a.cmp(imm(word::FN.token));
    let proc_error = a.local();
    a.bne(proc_error);
    a.jmp("err_no_fn");
    a.bind(proc_error);
    a.jmp("err_no_proc");
    a.bind(right);
    a.pla();
    a.sta(zp(z::STRING_LENGTH));
    a.ldy(imm(1));
    for to in &FRAME[1..] {
        a.lda(zp(z::STACK).ind_y());
        a.sta(zp(*to));
        a.iny();
    }
    // The 6502 stack's pointer from the length kept, and its bytes back.
    a.lda(zp(z::STACK).ind_y());
    a.eor(imm(STACK_TOP));
    a.tax();
    a.txs();
    a.iny();
    let restored = a.local();
    a.cpx(imm(STACK_TOP));
    a.beq(restored);
    a.inx();
    let back = a.here_local();
    a.lda(zp(z::STACK).ind_y());
    a.sta(abs(STACK_PAGE).x());
    a.iny();
    a.inx();
    a.bne(back);
    a.bind(restored);
    a.lda(zp(BASE));
    a.sta(zp(z::STACK));
    a.lda(zp(BASE + 1));
    a.sta(zp(z::STACK + 1));
    // A PROC goes on to the statement after its call, an FN back into its
    // expression.
    a.lda(zp(KIND));
    a.cmp(imm(word::PROC.token));
    let function = a.local();
    a.bne(function);
    a.jmp("statement_done");
    a.bind(function);
    a.lda(zp(z::TYPE));
    a.rts();
}
