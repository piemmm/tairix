//! Strings: three-byte descriptors and the stack of temporary ones,
//! string space taken downward from MEMSIZ and compacted when it runs out,
//! literals, concatenation, comparison, and LEN, STR$, VAL, ASC, CHR$,
//! LEFT$, RIGHT$ and MID$.

use super::decimal::NUMBER_TEXT;
use super::errors::{number, raise};
use super::expr::LEFT;
use super::float::{M3, M4};
use super::vars::{VALUE, VARIABLE};
use super::ws::zp;
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Asm, Label, RTS};

/// Past the temporary stack's last descriptor.
const TEMPST_END: u8 = 0x22;
/// A descriptor's size.
pub const DESCRIPTOR: u8 = 3;
/// The string being made: its length; its address is FRESPC.
pub const NEW_LENGTH: u16 = zp::FOUR6;
/// A substring's first character.
const START: u16 = zp::TANSGN;
/// The collection's state: the strings it has yet to place lie from
/// UNPLACED up to below LIMIT; the highest found so far and its
/// descriptor; the array being walked and its end.
const UNPLACED: u16 = zp::TEMPF1;
const BEST: u16 = zp::TEMPF1 + 2;
const BEST_DESCRIPTOR: u16 = zp::RESHO;
const LIMIT: u16 = zp::FRESPC;
const ARRAY_END: u16 = zp::RESHO + 2;
/// The right operand's text and length while the left's is found.
const RIGHT_TEXT: u16 = zp::TEMPF2 + 2;
const RIGHT_LENGTH: u16 = zp::TEMPF2 + 4;
/// An array's count of dimensions, and its header before them.
pub const ARRAY_DIMENSIONS: u8 = 4;
pub const ARRAY_HEADER: u8 = 5;

pub fn emit(a: &mut Asm) {
    a.movable(temporaries);
    a.movable(allocation);
    a.movable(|a| {
        collection(a);
        considering(a);
    });
    a.movable(concatenation);
    a.movable(comparison);
    a.movable(|a| {
        let illegal = a.local();
        functions(a, illegal);
        substrings(a, illegal);
    });
}

/// `str_literal`: the string from after the quote at TXTPTR to the next
/// quote or the line's end, left where it is, as a temporary; TXTPTR past
/// the closing quote. `text_temp`: the Y characters at TXTPTR as a
/// temporary, TXTPTR on past them and C more. `push_temp`: a temporary
/// descriptor of `NEW_LENGTH` and FRESPC, its address in M3 (low) and M4,
/// VALTYP a string; a fourth is FORMULA TOO COMPLEX. `free_string`: the
/// descriptor at INDEX popped if it is the last temporary, its string's
/// room given back if the newest; A its length (Z for none) and INDEX its
/// text. `free_at`: the same for the descriptor at A (low) and Y;
/// `free_fac_string` for FAC's.
fn temporaries(a: &mut Asm) {
    a.here("str_literal");
    let scan = a.local();
    let end = a.local();
    a.lda(imm(1));
    a.jsr("add_txtptr");
    a.ldy(imm(0xFF));
    a.bind(scan);
    a.iny();
    a.lda(z(zp::TXTPTR).ind_y());
    a.beq(end);
    a.cmp(imm(b'"'));
    a.bne(scan);
    // C set past a closing quote.
    a.bind(end);
    a.cmp(imm(b'"'));
    a.fall_into("text_temp");

    a.here("text_temp");
    a.sty(z(NEW_LENGTH));
    a.lda(z(zp::TXTPTR));
    a.sta(z(zp::FRESPC));
    a.lda(z(zp::TXTPTR + 1));
    a.sta(z(zp::FRESPC + 1));
    a.tya();
    a.adc(imm(0));
    a.jsr("add_txtptr");
    a.fall_into("push_temp");

    a.here("push_temp");
    let room = a.local();
    a.ldx(z(zp::TEMPPT));
    a.cpx(imm(TEMPST_END));
    a.bne(room);
    raise(a, number::FORMULA_TOO_COMPLEX);
    a.bind(room);
    a.lda(z(NEW_LENGTH));
    a.sta(z(0).x());
    a.lda(z(zp::FRESPC));
    a.sta(z(1).x());
    a.lda(z(zp::FRESPC + 1));
    a.sta(z(2).x());
    a.stx(z(M3));
    a.stx(z(zp::LASTPT));
    a.ldy(imm(0));
    a.sty(z(M4));
    a.sty(z(zp::LASTPT + 1));
    a.inx();
    a.inx();
    a.inx();
    a.stx(z(zp::TEMPPT));
    a.dey();
    a.sty(z(zp::VALTYP));
    a.rts();

    a.here("free_fac_string");
    a.lda(z(M3));
    a.ldy(z(M4));
    a.here("free_at");
    a.sta(z(zp::INDEX));
    a.sty(z(zp::INDEX + 1));
    a.here("free_string");
    let not_temp = a.local();
    a.ldy(imm(0));
    a.lda(z(zp::INDEX).ind_y());
    a.pha();
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.tax();
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.tay();
    a.lda(z(zp::INDEX + 1));
    a.bne(not_temp);
    a.lda(z(zp::INDEX));
    a.cmp(z(zp::LASTPT));
    a.bne(not_temp);
    a.sta(z(zp::TEMPPT));
    a.sbc(imm(DESCRIPTOR));
    a.sta(z(zp::LASTPT));
    // The text, X (low) and Y, at FRETOP: the newest, its room given back.
    a.cpx(z(zp::FRETOP));
    a.bne(not_temp);
    a.cpy(z(zp::FRETOP + 1));
    a.bne(not_temp);
    a.pla();
    a.pha();
    a.clc();
    a.adc(z(zp::FRETOP));
    a.sta(z(zp::FRETOP));
    a.bcc(not_temp);
    a.inc(z(zp::FRETOP + 1));
    a.bind(not_temp);
    a.stx(z(zp::INDEX));
    a.sty(z(zp::INDEX + 1));
    a.pla();
    a.rts();
}

/// `get_space`: A bytes of string space at FRESPC and INDEX2, `NEW_LENGTH`
/// A, below FRETOP and above STREND, the strings compacted once if they
/// do not fit; still no room is OUT OF MEMORY. `copy_text`: A bytes from
/// INDEX to INDEX2 onward; `free_and_copy` first frees the descriptor at
/// A (low) and Y, its text the source.
fn allocation(a: &mut Asm) {
    a.here("get_space");
    let attempt = a.local();
    let no_room = a.local();
    a.sta(z(NEW_LENGTH));
    a.lda(imm(0));
    a.sta(z(zp::GARBFL));
    a.bind(attempt);
    a.lda(z(zp::FRETOP));
    a.sec();
    a.sbc(z(NEW_LENGTH));
    a.tay();
    a.lda(z(zp::FRETOP + 1));
    a.sbc(imm(0));
    a.tax();
    a.cpy(z(zp::STREND));
    a.sbc(z(zp::STREND + 1));
    a.bcc(no_room);
    for byte in [zp::FRETOP, zp::FRESPC, zp::INDEX2] {
        a.sty(z(byte));
        a.stx(z(byte + 1));
    }
    a.rts();
    a.bind(no_room);
    let collect = a.local();
    a.bit(z(zp::GARBFL));
    a.bpl(collect);
    a.jmp("out_of_memory_error");
    // GARBFL's top bit set from 0, C clear.
    a.bind(collect);
    a.jsr("collect");
    a.sec();
    a.ror(z(zp::GARBFL));
    a.bcc(attempt);

    a.here("free_and_copy");
    a.jsr("free_at");
    a.here("copy_text");
    let copy = a.local();
    a.tay();
    a.beq(RTS);
    a.bind(copy);
    a.dey();
    a.lda(z(zp::INDEX).ind_y());
    a.sta(z(zp::INDEX2).ind_y());
    a.tya();
    a.bne(copy);
    a.rts();
}

/// `collect`: the strings in string space moved to its top, highest
/// first, each found as the highest below the last placed, so their order
/// holds; FRETOP then the bottom of them.
fn collection(a: &mut Asm) {
    a.here("collect");
    let gather = a.local();
    let temps = a.local();
    let variables = a.local();
    let variable = a.local();
    let not_string = a.local();
    let arrays = a.local();
    let set_limit = a.local();
    a.lda(z(zp::FRETOP));
    a.sta(z(UNPLACED));
    a.lda(z(zp::FRETOP + 1));
    a.sta(z(UNPLACED + 1));
    a.lda(z(zp::MEMSIZ));
    a.ldx(z(zp::MEMSIZ + 1));
    a.sta(z(zp::FRETOP));
    a.stx(z(zp::FRETOP + 1));
    a.bind(set_limit);
    a.sta(z(LIMIT));
    a.stx(z(LIMIT + 1));
    a.bind(gather);
    a.lda(imm(0));
    for byte in [BEST, BEST + 1, zp::INDEX + 1] {
        a.sta(z(byte));
    }
    a.lda(imm(zp::TEMPST.to_le_bytes()[0]));
    a.bind(temps);
    a.cmp(z(zp::TEMPPT));
    a.beq(variables);
    a.sta(z(zp::INDEX));
    a.jsr("consider_string");
    a.lda(z(zp::INDEX));
    a.clc();
    a.adc(imm(DESCRIPTOR));
    a.bne(temps);

    a.bind(variables);
    a.lda(z(zp::VARTAB));
    a.sta(z(zp::INDEX));
    a.lda(z(zp::VARTAB + 1));
    a.sta(z(zp::INDEX + 1));
    a.bind(variable);
    a.lda(z(zp::INDEX));
    a.cmp(z(zp::ARYTAB));
    a.lda(z(zp::INDEX + 1));
    a.sbc(z(zp::ARYTAB + 1));
    a.bcs(arrays);
    a.ldy(imm(0));
    a.lda(z(zp::INDEX).ind_y());
    a.bmi(not_string);
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.bpl(not_string);
    a.lda(imm(VALUE));
    a.jsr("index_up");
    a.jsr("consider_string");
    a.lda(imm(VARIABLE - VALUE));
    a.skip_over(|a| {
        a.bind(not_string);
        a.lda(imm(VARIABLE));
    });
    a.jsr("index_up");
    a.jmp(variable);
    collected(a, arrays, set_limit);
}

/// `collect` run on at `arrays`: INDEX walks the arrays from ARYTAB, each
/// from its header; then the highest string found moves to just below
/// FRETOP, C set by the bound, and the search goes again from `set_limit`
/// with it the limit.
fn collected(a: &mut Asm, arrays: Label, set_limit: Label) {
    let (array, elements, next_array, found, copy) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.bind(arrays);
    a.ldx(imm(zp::ARYTAB.to_le_bytes()[0]));
    a.skip_over(|a| {
        a.bind(next_array);
        a.ldx(imm(ARRAY_END.to_le_bytes()[0]));
    });
    a.lda(z(0).x());
    a.sta(z(zp::INDEX));
    a.lda(z(1).x());
    a.sta(z(zp::INDEX + 1));
    a.bind(array);
    a.lda(z(zp::INDEX));
    a.cmp(z(zp::STREND));
    a.lda(z(zp::INDEX + 1));
    a.sbc(z(zp::STREND + 1));
    a.bcs(found);
    a.ldy(imm(2));
    a.lda(z(zp::INDEX).ind_y());
    a.clc();
    a.adc(z(zp::INDEX));
    a.sta(z(ARRAY_END));
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.adc(z(zp::INDEX + 1));
    a.sta(z(ARRAY_END + 1));
    a.ldy(imm(0));
    a.lda(z(zp::INDEX).ind_y());
    a.bmi(next_array);
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.bpl(next_array);
    a.ldy(imm(ARRAY_DIMENSIONS));
    a.lda(z(zp::INDEX).ind_y());
    a.asl(crate::rom::asm::Acc);
    a.adc(imm(ARRAY_HEADER));
    a.jsr("index_up");
    a.bind(elements);
    a.lda(z(zp::INDEX));
    a.cmp(z(ARRAY_END));
    a.lda(z(zp::INDEX + 1));
    a.sbc(z(ARRAY_END + 1));
    a.bcs(next_array);
    a.jsr("consider_string");
    a.lda(imm(DESCRIPTOR));
    a.jsr("index_up");
    a.jmp(elements);
    a.bind(found);
    a.lda(z(BEST));
    a.ora(z(BEST + 1));
    a.beq(RTS);
    a.ldy(imm(0));
    a.lda(z(BEST_DESCRIPTOR).ind_y());
    a.tay();
    a.eor(imm(0xFF));
    a.adc(z(zp::FRETOP));
    a.sta(z(zp::FRETOP));
    a.bcs(copy);
    a.dec(z(zp::FRETOP + 1));
    a.bind(copy);
    a.dey();
    a.lda(z(BEST).ind_y());
    a.sta(z(zp::FRETOP).ind_y());
    a.tya();
    a.bne(copy);
    a.iny();
    a.lda(z(zp::FRETOP));
    a.sta(z(BEST_DESCRIPTOR).ind_y());
    a.iny();
    a.lda(z(zp::FRETOP + 1));
    a.sta(z(BEST_DESCRIPTOR).ind_y());
    a.lda(z(BEST));
    a.ldx(z(BEST + 1));
    a.jmp(set_limit);
}

/// `index_up`: INDEX up by A; X is lost. `consider_string`: the descriptor
/// at INDEX, if its string is unplaced, below LIMIT and the highest yet,
/// made the best.
fn considering(a: &mut Asm) {
    a.here("index_up");
    a.ldx(imm(lo(zp::INDEX)));
    a.jmp("add_to_pointer");

    a.here("consider_string");
    let done = a.local();
    a.ldy(imm(0));
    a.lda(z(zp::INDEX).ind_y());
    a.beq(done);
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.tax();
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.tay();
    // The text, Y (high) and X: at least UNPLACED, below LIMIT, above BEST.
    a.cpx(z(UNPLACED));
    a.sbc(z(UNPLACED + 1));
    a.bcc(done);
    a.tya();
    a.cpx(z(LIMIT));
    a.sbc(z(LIMIT + 1));
    a.bcs(done);
    a.txa();
    a.clc();
    a.sbc(z(BEST));
    a.tya();
    a.sbc(z(BEST + 1));
    a.bcc(done);
    a.stx(z(BEST));
    a.sty(z(BEST + 1));
    a.lda(z(zp::INDEX));
    a.sta(z(BEST_DESCRIPTOR));
    a.lda(z(zp::INDEX + 1));
    a.sta(z(BEST_DESCRIPTOR + 1));
    a.bind(done);
    a.rts();
}

/// `concatenate`: the left string then the right, past 255 STRING TOO
/// LONG, as a new temporary; both operands freed.
fn concatenation(a: &mut Asm) {
    a.here("concatenate");
    let fits = a.local();
    a.ldy(imm(0));
    a.lda(z(LEFT).ind_y());
    a.pha();
    a.clc();
    a.adc(z(M3).ind_y());
    a.bcc(fits);
    a.jmp("string_too_long_error");
    a.bind(fits);
    a.jsr("get_space");
    // The right, the last temporary, popped first; the left copied; the
    // right freed again for its text, below the stack's top by then.
    a.jsr("free_fac_string");
    a.lda(z(LEFT));
    a.ldy(z(LEFT + 1));
    a.jsr("free_and_copy");
    a.pla();
    a.ldx(imm(lo(zp::INDEX2)));
    a.jsr("add_to_pointer");
    a.jsr("free_fac_string");
    a.jmp("copy_and_push");
}

/// `compare_strings`: A 1, 0 or `$FF` as the right string is greater than,
/// equal to or less than the left, byte by byte, a prefix the less; both
/// freed.
fn comparison(a: &mut Asm) {
    a.here("compare_strings");
    let shorter = a.local();
    let step = a.local();
    let differ = a.local();
    let lengths = a.local();
    a.jsr("free_fac_string");
    a.sta(z(RIGHT_LENGTH));
    a.lda(z(zp::INDEX));
    a.sta(z(RIGHT_TEXT));
    a.lda(z(zp::INDEX + 1));
    a.sta(z(RIGHT_TEXT + 1));
    a.lda(z(LEFT));
    a.ldy(z(LEFT + 1));
    a.jsr("free_at");
    a.tax();
    a.cmp(z(RIGHT_LENGTH));
    a.bcc(shorter);
    a.ldx(z(RIGHT_LENGTH));
    a.bind(shorter);
    a.stx(z(NEW_LENGTH));
    a.sta(z(START));
    a.ldy(imm(0));
    a.bind(step);
    a.cpy(z(NEW_LENGTH));
    a.beq(lengths);
    a.lda(z(RIGHT_TEXT).ind_y());
    a.cmp(z(zp::INDEX).ind_y());
    a.bne(differ);
    a.iny();
    a.bne(step);
    // C set by the count's match; equal lengths return 0.
    a.bind(lengths);
    a.lda(z(RIGHT_LENGTH));
    a.sbc(z(START));
    a.beq(RTS);
    // C set: the right is the greater.
    a.bind(differ);
    a.lda(imm(1));
    a.bcs(RTS);
    a.lda(imm(0xFF));
    a.rts();
}

/// LEN, STR$, VAL, ASC and CHR$; `illegal` raises ILLEGAL QUANTITY.
fn functions(a: &mut Asm, illegal: Label) {
    a.here("fn_len");
    a.jsr("free_fac_string");
    a.jmp("byte_result");

    a.here("fn_asc");
    a.jsr("free_fac_string");
    a.beq(illegal);
    a.ldy(imm(0));
    a.lda(z(zp::INDEX).ind_y());
    a.here("byte_result");
    a.tay();
    a.lda(imm(0));
    a.sta(z(zp::VALTYP));
    a.jmp("byte_to_fac");

    a.here("fn_str");
    let measure = a.local();
    a.jsr("fac_to_text");
    a.ldy(imm(0xFF));
    a.bind(measure);
    a.iny();
    a.lda(abs(NUMBER_TEXT).y());
    a.bne(measure);
    a.tya();
    a.jsr("get_space");
    a.lda(imm(lo(NUMBER_TEXT)));
    a.sta(z(zp::INDEX));
    a.lda(imm(hi(NUMBER_TEXT)));
    a.sta(z(zp::INDEX + 1));
    a.jmp("new_text");

    a.here("fn_chr");
    a.jsr("fac_to_byte");
    a.txa();
    a.pha();
    a.lda(imm(1));
    a.jsr("get_space");
    a.pla();
    a.ldy(imm(0));
    a.sta(z(zp::FRESPC).ind_y());
    a.jmp("push_temp");

    // VAL: `text_to_fac` reads the string, ended for a moment by a zero.
    a.here("fn_val");
    let not_empty = a.local();
    a.lda(imm(0));
    a.sta(z(zp::VALTYP));
    a.jsr("free_fac_string");
    a.bne(not_empty);
    a.jmp("fac_zero");
    a.bind(not_empty);
    a.clc();
    a.adc(z(zp::INDEX));
    a.sta(z(zp::INDEX2));
    a.lda(z(zp::INDEX + 1));
    a.adc(imm(0));
    a.sta(z(zp::INDEX2 + 1));
    a.ldy(imm(0));
    a.lda(z(zp::INDEX2).ind_y());
    a.pha();
    a.tya();
    a.sta(z(zp::INDEX2).ind_y());
    a.lda(z(zp::TXTPTR + 1));
    a.pha();
    a.lda(z(zp::TXTPTR));
    a.pha();
    a.lda(z(zp::INDEX));
    a.sta(z(zp::TXTPTR));
    a.lda(z(zp::INDEX + 1));
    a.sta(z(zp::TXTPTR + 1));
    // `text_to_fac` leaves INDEX2, the string's end, alone.
    a.jsr("text_to_fac");
    a.pla();
    a.sta(z(zp::TXTPTR));
    a.pla();
    a.sta(z(zp::TXTPTR + 1));
    a.pla();
    a.ldy(imm(0));
    a.sta(z(zp::INDEX2).ind_y());
    a.rts();
    a.bind(illegal);
    a.jmp("illegal_quantity_error");
}

/// LEFT$, RIGHT$ and MID$ of the string whose descriptor is at DSCPNT:
/// LEFT$ and RIGHT$ take Y characters from its start or end, MID$
/// `NEW_LENGTH` characters from its Yth, which must be at least 1.
fn substrings(a: &mut Asm, illegal: Label) {
    let start = a.local();
    let within = a.local();
    let mid_take = a.local();
    a.here("fn_left");
    a.sty(z(NEW_LENGTH));
    a.lda(imm(0));
    a.beq(start);

    // From the end, but not before the start.
    a.here("fn_right");
    a.sty(z(NEW_LENGTH));
    a.ldy(imm(0));
    a.lda(z(zp::DSCPNT).ind_y());
    a.sec();
    a.sbc(z(NEW_LENGTH));
    a.bcs(start);
    a.lda(imm(0));
    a.beq(start);

    a.here("fn_mid");
    a.tya();
    a.beq(illegal);
    a.dey();
    a.tya();
    // NEW_LENGTH characters from the Ath, none from past the end.
    a.bind(start);
    a.sta(z(START));
    a.ldy(imm(0));
    a.cmp(z(zp::DSCPNT).ind_y());
    a.bcc(within);
    a.lda(imm(0));
    a.beq("substring");
    a.bind(within);
    a.lda(z(zp::DSCPNT).ind_y());
    a.sec();
    a.sbc(z(START));
    a.cmp(z(NEW_LENGTH));
    a.bcc(mid_take);
    a.lda(z(NEW_LENGTH));
    a.bind(mid_take);
    a.fall_into("substring");

    // A characters from the string at DSCPNT from its STARTth as a new
    // temporary; the source freed.
    a.here("substring");
    a.jsr("get_space");
    a.lda(z(zp::DSCPNT));
    a.ldy(z(zp::DSCPNT + 1));
    a.jsr("free_at");
    a.lda(z(START));
    a.jsr("index_up");
    // NEW_LENGTH bytes from INDEX as a new temporary; `copy_and_push` A.
    a.here("new_text");
    a.lda(z(NEW_LENGTH));
    a.here("copy_and_push");
    a.jsr("copy_text");
    a.jmp("push_temp");
}
