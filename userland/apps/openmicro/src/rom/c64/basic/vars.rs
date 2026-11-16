//! Variables: names, the simple variables between VARTAB and ARYTAB, the
//! arrays between ARYTAB and STREND as the Programmer's Reference Guide
//! lays them out, TI, TI$ and ST, LET and its string rules, DIM, DEF FN
//! and FN, and ADRAY1's conversion.

use super::decimal::{weight_row, CLOCK_TEXT, CLOCK_WEIGHTS, DIGIT_TEXT};
use super::errors::{number, raise};
use super::exec::SUBSCRIPTS_REFUSED;
use super::float::{M1, M2, M3, M4};
use super::strings::{ARRAY_DIMENSIONS, ARRAY_HEADER, DESCRIPTOR, NEW_LENGTH};
use super::text::LINE_FOUND;
use super::tokens::token;
use super::ws::zp;
use crate::cpu::STACK_PAGE;
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm, Label, RTS};
use crate::rom::c64::kernal::entry as kernal;

/// A simple variable's size, and where its value starts in it.
pub const VARIABLE: u8 = 7;
pub const VALUE: u8 = 2;
/// The size an array's dimension takes when it is first used undeclared.
const DEFAULT_SIZE: u8 = 11;
/// DIMFLG: the reference only reads, so a simple variable is not made.
const READING: u8 = 0x40;
/// DIMFLG: a DIM makes the array.
const DIMENSIONING: u8 = 0x80;
/// Name bits: the second character's marks a string, both an integer;
/// the first's alone FN.
const NAME_MARK: u8 = 0x80;
/// TI, ST and their names as VARNAM holds them.
const TI: [u8; 2] = *b"TI";
const ST: [u8; 2] = *b"ST";
/// The simple variables' next to look at.
const SCAN: u16 = zp::INDEX;
/// An array reference's array, its offset or size, and a multiplier.
const ARRAY: u16 = LINE_FOUND;
const OFFSET: u16 = zp::INDEX2;
const FACTOR: u16 = zp::DEFPNT;
/// A multiplication's addend and sum, and the dimensions an array's
/// header has left to write.
const ADDEND: u16 = zp::TEMPF2;
const LOOP: u16 = zp::ENDCHR;
/// The stack while an array reference's subscripts are read: above the
/// caller's return, DIMFLG, VALTYP, INTFLG and VARNAM, then each subscript
/// pushed high byte first.
const SAVED_STATE: u8 = 5;

pub fn emit(a: &mut Asm) {
    a.whole(|a| {
        simple(a);
        creation(a);
    });
    a.whole(names);
    a.whole(|a| {
        reading(a);
        clock(a);
    });
    a.whole(assignment);
    a.whole(|a| {
        arrays(a);
        array_creation(a);
        subscripts(a);
    });
    a.whole(dim);
    a.whole(|a| {
        functions(a);
        function_calls(a);
    });
}

/// `find_variable`: the variable named at TXTPTR, from its first letter in A:
/// VARNAM its name with its type's marks, VALTYP and INTFLG its type,
/// VARPNT and A (low) and Y its value — an array's element, made if new;
/// a simple variable made unless DIMFLG reads, where a missing one is a
/// zero. SUBFLG may refuse an integer or a subscript. `parse_name`: the
/// name alone, A the character after it.
fn names(a: &mut Asm) {
    let refused = a.local();
    a.here("parse_name");
    let letter = a.local();
    let second = a.local();
    let skip = a.local();
    let suffix = a.local();
    let not_integer = a.local();
    let marked = a.local();
    a.ldx(imm(0));
    a.stx(z(zp::VALTYP));
    a.stx(z(zp::INTFLG));
    a.stx(z(zp::VARNAM + 1));
    a.jsr("is_letter");
    a.bcs(letter);
    a.bind(refused);
    a.jmp("syntax_error");
    a.bind(letter);
    a.sta(z(zp::VARNAM));
    a.jsr(zp::CHRGET);
    a.bcc(second);
    a.jsr("is_letter");
    a.bcc(suffix);
    a.bind(second);
    a.sta(z(zp::VARNAM + 1));
    a.bind(skip);
    a.jsr(zp::CHRGET);
    a.bcc(skip);
    a.jsr("is_letter");
    a.bcs(skip);
    a.bind(suffix);
    a.cmp(imm(b'%'));
    a.bne(not_integer);
    a.lda(z(zp::SUBFLG));
    a.bne(refused);
    a.lda(imm(NAME_MARK));
    a.sta(z(zp::INTFLG));
    a.ora(z(zp::VARNAM));
    a.sta(z(zp::VARNAM));
    a.bne(marked);
    a.bind(not_integer);
    a.cmp(imm(b'$'));
    a.bne(RTS);
    a.dec(z(zp::VALTYP));
    a.bind(marked);
    a.lda(imm(NAME_MARK));
    a.ora(z(zp::VARNAM + 1));
    a.sta(z(zp::VARNAM + 1));
    a.jmp(abs(zp::CHRGET));
}

/// `simple_variable`: VARNAM found among the simple variables, or made, or
/// for a reading a zero. `value_at`: VARPNT, and A (low) and Y, the value
/// past the name at the pointer in zero page X.
fn simple(a: &mut Asm) {
    a.here("find_variable");
    let simple = a.local();
    let refused = a.local();
    a.jsr("parse_name");
    a.cmp(imm(b'('));
    a.bne(simple);
    a.lda(z(zp::SUBFLG));
    a.bne(refused);
    a.jmp("array_element");
    a.bind(refused);
    a.jmp("syntax_error");
    a.bind(simple);
    a.lda(imm(0));
    a.sta(z(zp::SUBFLG));
    a.here("simple_variable");
    let ordinary = a.local();
    let search = a.local();
    let next = a.local();
    let absent = a.local();
    a.jsr("special_variable");
    a.bcc(ordinary);
    a.lda(imm(0));
    a.sta(z(zp::VARPNT + 1));
    a.tay();
    a.bit(z(zp::DIMFLG));
    a.bvc(RTS);
    a.jmp("read_special");
    a.bind(ordinary);
    a.lda(z(zp::VARTAB));
    a.sta(z(SCAN));
    a.lda(z(zp::VARTAB + 1));
    a.sta(z(SCAN + 1));
    a.bind(search);
    a.lda(z(SCAN));
    a.cmp(z(zp::ARYTAB));
    a.lda(z(SCAN + 1));
    a.sbc(z(zp::ARYTAB + 1));
    a.bcs(absent);
    a.ldy(imm(0));
    a.lda(z(SCAN).ind_y());
    a.iny();
    a.cmp(z(zp::VARNAM));
    a.bne(next);
    a.lda(z(SCAN).ind_y());
    a.cmp(z(zp::VARNAM + 1));
    a.bne(next);
    a.ldx(imm(lo(SCAN)));
    a.here("value_at");
    a.lda(z(0).x());
    a.clc();
    a.adc(imm(VALUE));
    a.sta(z(zp::VARPNT));
    a.lda(z(1).x());
    a.adc(imm(0));
    a.sta(z(zp::VARPNT + 1));
    a.tay();
    a.lda(z(zp::VARPNT));
    a.rts();
    a.bind(next);
    a.lda(imm(VARIABLE));
    a.jsr("index_up");
    a.jmp(search);
    a.bind(absent);
    a.bit(z(zp::DIMFLG));
    a.bvc("make_variable");
    a.lda(imm(lo("zero_value")));
    a.ldy(imm(hi("zero_value")));
    a.sta(z(zp::VARPNT));
    a.sty(z(zp::VARPNT + 1));
    a.rts();
}

/// `make_variable`: a new simple variable at ARYTAB, the arrays moved up
/// to make room, its value zero. `make_room`: A bytes above STREND below
/// the strings; `make_room16`: OFFSET bytes; each compacting the strings
/// once if need be, which keeps OFFSET and ARRAY, else OUT OF MEMORY.
fn creation(a: &mut Asm) {
    a.here("make_variable");
    let fill = a.local();
    let put = a.local();
    a.lda(imm(VARIABLE));
    a.jsr("make_room");
    a.lda(z(zp::ARYTAB));
    a.sta(z(zp::INDEX));
    a.lda(z(zp::ARYTAB + 1));
    a.sta(z(zp::INDEX + 1));
    a.lda(imm(VARIABLE));
    a.ldx(imm(lo(zp::STREND)));
    a.jsr("open_gap");
    a.ldy(imm(VARIABLE - 1));
    a.bind(fill);
    a.lda(imm(0));
    a.cpy(imm(VALUE));
    a.bcs(put);
    a.lda(abs(zp::VARNAM).y());
    a.bind(put);
    a.sta(z(zp::ARYTAB).ind_y());
    a.dey();
    a.bpl(fill);
    a.ldx(imm(lo(zp::ARYTAB)));
    a.jsr("value_at");
    a.lda(imm(VARIABLE));
    a.jsr("add_to_pointer");
    a.lda(z(zp::VARPNT));
    a.rts();

    a.here("make_room");
    a.sta(z(OFFSET));
    a.lda(imm(0));
    a.sta(z(OFFSET + 1));
    a.here("make_room16");
    let attempt = a.local();
    let fits = a.local();
    let out = a.local();
    a.lda(imm(0));
    a.sta(z(zp::GARBFL));
    a.bind(attempt);
    a.lda(z(zp::STREND));
    a.clc();
    a.adc(z(OFFSET));
    a.tax();
    a.lda(z(zp::STREND + 1));
    a.adc(z(OFFSET + 1));
    a.bcs(out);
    a.cpx(z(zp::FRETOP));
    a.sbc(z(zp::FRETOP + 1));
    a.bcc(fits);
    a.bit(z(zp::GARBFL));
    a.bmi(out);
    a.jsr("collect");
    a.lda(imm(0x80));
    a.sta(z(zp::GARBFL));
    a.bne(attempt);
    a.bind(fits);
    a.rts();
    a.bind(out);
    a.jmp("out_of_memory_error");
}

/// `special_variable`: C set for TI, TI$ or ST. `eval_variable`: the
/// variable at TXTPTR, from its first letter in A, read into FAC, a
/// string's descriptor into M3 (low) and M4; `read_special`: TI, TI$ or
/// ST, a signed byte, read, VARPNT's high byte zero.
fn reading(a: &mut Asm) {
    a.here("special_variable");
    let not_s = a.local();
    let not = a.local();
    a.lda(z(zp::VARNAM + 1));
    a.ldx(z(zp::VARNAM));
    a.cpx(imm(ST[0]));
    a.bne(not_s);
    a.cmp(imm(ST[1]));
    a.beq(RTS);
    a.bind(not_s);
    a.cpx(imm(TI[0]));
    a.bne(not);
    a.and(imm(!NAME_MARK));
    a.cmp(imm(TI[1]));
    a.beq(RTS);
    a.bind(not);
    a.clc();
    a.rts();

    a.here("eval_variable");
    let numeric = a.local();
    let integer = a.local();
    a.ldx(imm(READING));
    a.stx(z(zp::DIMFLG));
    a.jsr("find_variable");
    a.ldx(imm(0));
    a.stx(z(zp::DIMFLG));
    a.ldx(z(zp::VARPNT + 1));
    a.beq(RTS);
    a.ldx(z(zp::VALTYP));
    a.beq(numeric);
    a.sta(z(M3));
    a.sty(z(M4));
    a.rts();
    a.bind(numeric);
    a.ldx(z(zp::INTFLG));
    a.bmi(integer);
    a.jmp("fac_load");
    a.bind(integer);
    a.ldy(imm(0));
    a.lda(z(zp::VARPNT).ind_y());
    a.pha();
    a.iny();
    a.lda(z(zp::VARPNT).ind_y());
    a.tay();
    a.pla();
    a.jmp("word_to_fac");

    a.here("read_special");
    let not_st = a.local();
    a.lda(z(zp::VARNAM));
    a.cmp(imm(ST[0]));
    a.bne(not_st);
    a.jsr(kernal::READST);
    a.jmp("signed_byte_to_fac");
    a.bind(not_st);
    a.lda(z(zp::VALTYP));
    a.bne("read_ti_string");
    a.fall_into("read_ti");
}

const _: () = assert!(DIGIT_TEXT < 0x100, "the digits lie in page zero");

/// TI: the jiffy clock as a number. TI$: its hours, minutes and seconds as
/// six digits. TI$ assigned: six characters, each less `0` a digit of the
/// weights TI$ shows, set the clock modulo 2²⁴; another length is ILLEGAL
/// QUANTITY.
fn clock(a: &mut Asm) {
    a.here("read_ti");
    a.jsr(kernal::RDTIM);
    a.sty(z(M1));
    a.stx(z(M2));
    a.sta(z(M3));
    a.lda(imm(0));
    a.sta(z(M4));
    a.sta(z(zp::FACOV));
    a.sta(z(zp::FACSGN));
    a.lda(imm(super::float::WORD_EXPONENT + 8));
    a.sta(z(zp::FACEXP));
    a.jmp("normalise");

    // The room first: a compaction would overwrite the digits.
    a.here("read_ti_string");
    a.lda(imm(CLOCK_TEXT));
    a.jsr("get_space");
    a.jsr(kernal::RDTIM);
    a.sta(z(M4));
    a.stx(z(M3));
    a.sty(z(M2));
    a.lda(imm(0));
    a.sta(z(M1));
    a.sta(z(zp::INDEX + 1));
    a.ldy(imm(CLOCK_WEIGHTS));
    a.lda(imm(CLOCK_TEXT));
    a.jsr("make_digits");
    a.lda(imm(lo(DIGIT_TEXT)));
    a.sta(z(zp::INDEX));
    a.jmp("new_text");

    a.here("assign_clock");
    let six = a.local();
    let character = a.local();
    let add = a.local();
    let added = a.local();
    a.jsr("free_fac_string");
    a.cmp(imm(CLOCK_TEXT));
    a.beq(six);
    a.jmp("illegal_quantity_error");
    a.bind(six);
    a.lda(imm(0));
    a.sta(z(M1));
    a.sta(z(M2));
    a.sta(z(M3));
    a.tay();
    a.bind(character);
    a.lda(z(zp::INDEX).ind_y());
    a.sec();
    a.sbc(imm(b'0'));
    a.sta(z(zp::COUNT));
    a.beq(added);
    a.bind(add);
    a.clc();
    for (rank, byte) in [(3, M3), (2, M2), (1, M1)] {
        a.lda(z(byte));
        a.adc(abs(weight_row(rank) + i32::from(CLOCK_WEIGHTS)).y());
        a.sta(z(byte));
    }
    a.dec(z(zp::COUNT));
    a.bne(add);
    a.bind(added);
    a.iny();
    a.cpy(imm(CLOCK_TEXT));
    a.bne(character);
    a.lda(z(M3));
    a.ldx(z(M2));
    a.ldy(z(M1));
    a.jmp(abs(kernal::SETTIM));
}

/// LET: the variable at TXTPTR given the expression after `=`, of its
/// type; TI and ST are SYNTAX before it. FORPNT is left the variable.
/// `assign`: FAC given to the variable at FORPNT, VALTYP and INTFLG its
/// type: a real stored rounded, an integer floored to 16 bits, a string
/// by its rules, as `string_assignment` gives it; TI$ sets the clock, TI
/// and ST are SYNTAX.
fn assignment(a: &mut Asm) {
    let syntax = a.local();
    a.here("st_let");
    let ordinary = a.local();
    let same_type = a.local();
    a.jsr("find_variable");
    a.cpy(imm(0));
    a.bne(ordinary);
    a.ldx(z(zp::VALTYP));
    a.beq(syntax);
    a.bind(ordinary);
    a.sta(z(zp::FORPNT));
    a.sty(z(zp::FORPNT + 1));
    a.lda(imm(token::EQUAL));
    a.jsr("expect_char");
    a.lda(z(zp::INTFLG));
    a.pha();
    a.lda(z(zp::VALTYP));
    a.pha();
    a.jsr("evaluate");
    a.pla();
    a.cmp(z(zp::VALTYP));
    a.beq(same_type);
    a.jmp("type_mismatch_error");
    a.bind(same_type);
    a.pla();
    a.sta(z(zp::INTFLG));
    a.here("assign");
    let variable = a.local();
    let integer = a.local();
    let string = a.local();
    a.lda(z(zp::FORPNT + 1));
    a.bne(variable);
    a.lda(z(zp::VALTYP));
    a.beq(syntax);
    a.jmp("assign_clock");
    a.bind(syntax);
    a.jmp("syntax_error");
    a.bind(variable);
    a.lda(z(zp::VALTYP));
    a.bne(string);
    a.lda(z(zp::INTFLG));
    a.bmi(integer);
    a.ldx(z(zp::FORPNT));
    a.ldy(z(zp::FORPNT + 1));
    a.jmp("fac_store");
    a.bind(integer);
    a.jsr("fac_to_integer");
    a.ldy(imm(0));
    a.lda(z(M3));
    a.sta(z(zp::FORPNT).ind_y());
    a.iny();
    a.lda(z(M4));
    a.sta(z(zp::FORPNT).ind_y());
    a.rts();
    string_assignment(a, string);
}

/// `assign` run on at `string`: a string in the program's text is shared;
/// the last temporary, its string in string space, is adopted; any other is
/// copied into string space.
fn string_assignment(a: &mut Asm, string: Label) {
    let (not_text, share, copy, put) = (a.local(), a.local(), a.local(), a.local());
    a.bind(string);
    a.lda(z(M3));
    a.sta(z(zp::INDEX));
    a.lda(z(M4));
    a.sta(z(zp::INDEX + 1));
    a.ldy(imm(1));
    a.lda(z(zp::INDEX).ind_y());
    a.tax();
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    // A (high) and X its text, tested against each region's bounds.
    a.cpx(z(zp::TXTTAB));
    a.pha();
    a.sbc(z(zp::TXTTAB + 1));
    a.pla();
    a.bcc(not_text);
    a.cpx(z(zp::VARTAB));
    a.pha();
    a.sbc(z(zp::VARTAB + 1));
    a.pla();
    a.bcc(share);
    a.bind(not_text);
    a.cpx(z(zp::FRETOP));
    a.sbc(z(zp::FRETOP + 1));
    a.bcc(copy);
    a.lda(z(zp::INDEX + 1));
    a.bne(copy);
    a.lda(z(zp::INDEX));
    a.cmp(z(zp::LASTPT));
    a.bne(copy);
    // Adopted: the temporary popped, its string's room kept.
    a.sta(z(zp::TEMPPT));
    a.sbc(imm(DESCRIPTOR));
    a.sta(z(zp::LASTPT));
    a.bcs(put);
    // Shared: a temporary for it popped; its text is never the newest.
    a.bind(share);
    a.jsr(put);
    a.jmp("free_string");
    a.bind(put);
    a.ldy(imm(2));
    let bytes = a.here_local();
    a.lda(z(zp::INDEX).ind_y());
    a.sta(z(zp::FORPNT).ind_y());
    a.dey();
    a.bpl(bytes);
    a.rts();
    // Copied: room taken first, which may move the source.
    a.bind(copy);
    a.ldy(imm(0));
    a.lda(z(zp::INDEX).ind_y());
    a.jsr("get_space");
    a.lda(z(M3));
    a.ldy(z(M4));
    a.jsr("free_and_copy");
    a.lda(z(NEW_LENGTH));
    a.ldy(imm(0));
    a.sta(z(zp::FORPNT).ind_y());
    a.iny();
    a.lda(z(zp::FRESPC));
    a.sta(z(zp::FORPNT).ind_y());
    a.iny();
    a.lda(z(zp::FRESPC + 1));
    a.sta(z(zp::FORPNT).ind_y());
    a.rts();
}

/// `array_element`: the subscripts in brackets after the name, each a
/// non-negative integer; the array found, or made — eleven elements a
/// dimension unless DIM gives them — and DIM of one already made REDIM'D
/// ARRAY; the element's address, each subscript within its dimension and
/// as many as the array has, else BAD SUBSCRIPT.
fn arrays(a: &mut Asm) {
    a.here("array_element");
    let subscript = a.local();
    let not_negative = a.local();
    let search = a.local();
    let next = a.local();
    let found = a.local();
    for byte in [
        zp::DIMFLG,
        zp::VALTYP,
        zp::INTFLG,
        zp::VARNAM + 1,
        zp::VARNAM,
    ] {
        a.lda(z(byte));
        a.pha();
    }
    a.lda(imm(0));
    a.sta(z(zp::COUNT));
    a.bind(subscript);
    a.lda(z(zp::COUNT));
    a.pha();
    a.jsr(zp::CHRGET);
    a.jsr("evaluate_number");
    a.jsr("fac_to_integer");
    a.pla();
    a.sta(z(zp::COUNT));
    a.lda(z(M3));
    a.bpl(not_negative);
    a.jmp("illegal_quantity_error");
    a.bind(not_negative);
    a.pha();
    a.lda(z(M4));
    a.pha();
    a.inc(z(zp::COUNT));
    a.jsr(zp::CHRGOT);
    a.cmp(imm(b','));
    a.beq(subscript);
    a.jsr("expect_close");
    // The name and type back from beneath the subscripts.
    a.tsx();
    a.txa();
    a.clc();
    a.adc(z(zp::COUNT));
    a.adc(z(zp::COUNT));
    a.tax();
    for (offset, byte) in [
        (1, zp::VARNAM),
        (2, zp::VARNAM + 1),
        (3, zp::INTFLG),
        (4, zp::VALTYP),
        (5, zp::DIMFLG),
    ] {
        a.lda(abs(STACK_PAGE + offset).x());
        a.sta(z(byte));
    }
    a.lda(z(zp::ARYTAB));
    a.ldx(z(zp::ARYTAB + 1));
    a.bind(search);
    a.sta(z(ARRAY));
    a.stx(z(ARRAY + 1));
    a.cmp(z(zp::STREND));
    a.txa();
    a.sbc(z(zp::STREND + 1));
    a.bcs("make_array");
    a.ldy(imm(0));
    a.lda(z(ARRAY).ind_y());
    a.iny();
    a.cmp(z(zp::VARNAM));
    a.bne(next);
    a.lda(z(ARRAY).ind_y());
    a.cmp(z(zp::VARNAM + 1));
    a.beq(found);
    a.bind(next);
    a.ldy(imm(2));
    a.lda(z(ARRAY).ind_y());
    a.clc();
    a.adc(z(ARRAY));
    a.pha();
    a.iny();
    a.lda(z(ARRAY).ind_y());
    a.adc(z(ARRAY + 1));
    a.tax();
    a.pla();
    // The arrays end below the address space's end: no carry.
    a.bcc(search);
    a.bind(found);
    let dimensions = a.local();
    a.bit(z(zp::DIMFLG));
    a.bpl(dimensions);
    raise(a, number::REDIMD_ARRAY);
    a.bind(dimensions);
    let bad = a.local();
    a.ldy(imm(ARRAY_DIMENSIONS));
    a.lda(z(ARRAY).ind_y());
    a.cmp(z(zp::COUNT));
    a.bne(bad);
    a.jmp("array_offset");
    a.bind(bad);
    a.jmp("bad_subscript_error");
}

/// `make_array`: the array at STREND made: its header, each dimension's
/// size the subscript plus one for DIM, else eleven, then its elements,
/// zero; then, for DIM as for a use, the element the subscripts name.
fn array_creation(a: &mut Asm) {
    a.here("make_array");
    let size = a.local();
    let write = a.local();
    let zeroed = a.local();
    a.lda(z(zp::COUNT));
    a.asl(Acc);
    a.adc(imm(ARRAY_HEADER));
    a.jsr("make_room");
    a.ldy(imm(0));
    a.lda(z(zp::VARNAM));
    a.sta(z(ARRAY).ind_y());
    a.iny();
    a.lda(z(zp::VARNAM + 1));
    a.sta(z(ARRAY).ind_y());
    a.ldy(imm(ARRAY_DIMENSIONS));
    a.lda(z(zp::COUNT));
    a.sta(z(ARRAY).ind_y());
    a.sta(z(LOOP));
    a.lda(imm(1));
    a.sta(z(OFFSET));
    a.lda(imm(0));
    a.sta(z(OFFSET + 1));
    a.tsx();
    a.bind(size);
    a.lda(imm(DEFAULT_SIZE));
    a.sta(z(FACTOR));
    a.lda(imm(0));
    a.sta(z(FACTOR + 1));
    a.sta(z(ADDEND));
    a.sta(z(ADDEND + 1));
    a.bit(z(zp::DIMFLG));
    a.bpl(write);
    a.lda(abs(STACK_PAGE + 1).x());
    a.clc();
    a.adc(imm(1));
    a.sta(z(FACTOR));
    a.lda(abs(STACK_PAGE + 2).x());
    a.adc(imm(0));
    a.sta(z(FACTOR + 1));
    a.bind(write);
    a.iny();
    a.lda(z(FACTOR + 1));
    a.sta(z(ARRAY).ind_y());
    a.iny();
    a.lda(z(FACTOR));
    a.sta(z(ARRAY).ind_y());
    a.inx();
    a.inx();
    a.jsr("multiply_add");
    a.dec(z(LOOP));
    a.bne(size);
    // The header's bytes besides.
    a.iny();
    a.sty(z(LOOP));
    a.jsr("element_bytes");
    a.ldy(imm(2));
    a.lda(z(OFFSET));
    a.sta(z(ARRAY).ind_y());
    a.iny();
    a.lda(z(OFFSET + 1));
    a.sta(z(ARRAY).ind_y());
    a.jsr("make_room16");
    // The elements, from past the header to the new end, zero.
    a.lda(z(LOOP));
    a.clc();
    a.adc(z(ARRAY));
    a.sta(z(zp::INDEX));
    a.lda(z(ARRAY + 1));
    a.adc(imm(0));
    a.sta(z(zp::INDEX + 1));
    a.lda(z(OFFSET));
    a.clc();
    a.adc(z(ARRAY));
    a.sta(z(zp::STREND));
    a.lda(z(OFFSET + 1));
    a.adc(z(ARRAY + 1));
    a.sta(z(zp::STREND + 1));
    a.ldy(imm(0));
    let clear = a.here_local();
    a.lda(z(zp::INDEX));
    a.cmp(z(zp::STREND));
    a.lda(z(zp::INDEX + 1));
    a.sbc(z(zp::STREND + 1));
    a.bcs(zeroed);
    a.tya();
    a.sta(z(zp::INDEX).ind_y());
    a.inc(z(zp::INDEX));
    a.bne(clear);
    a.inc(z(zp::INDEX + 1));
    a.jmp(clear);
    // DIM's element is the last, within the array just made.
    a.bind(zeroed);
    a.fall_into("array_offset");
}

/// `array_offset`: the element the subscripts on the stack name, the last
/// first against the sizes as the header holds them, each within its
/// size; the subscripts and the saved state dropped, VARPNT and A (low)
/// and Y its address. `element_bytes`: OFFSET times the element's size,
/// plus Y, the header's bytes. `multiply_add`: OFFSET times FACTOR plus
/// ADDEND; past 16 bits OUT OF MEMORY. X and Y are kept.
fn subscripts(a: &mut Asm) {
    a.here("array_offset");
    let step = a.local();
    let within = a.local();
    a.lda(imm(0));
    a.sta(z(OFFSET));
    a.sta(z(OFFSET + 1));
    a.ldy(imm(ARRAY_HEADER));
    a.bind(step);
    a.pla();
    a.sta(z(ADDEND));
    a.pla();
    a.sta(z(ADDEND + 1));
    a.lda(z(ARRAY).ind_y());
    a.sta(z(FACTOR + 1));
    a.iny();
    a.lda(z(ARRAY).ind_y());
    a.sta(z(FACTOR));
    a.iny();
    a.lda(z(ADDEND));
    a.cmp(z(FACTOR));
    a.lda(z(ADDEND + 1));
    a.sbc(z(FACTOR + 1));
    a.bcc(within);
    a.jmp("bad_subscript_error");
    a.bind(within);
    a.jsr("multiply_add");
    a.dec(z(zp::COUNT));
    a.bne(step);
    for _ in 0..SAVED_STATE {
        a.pla();
    }
    a.jsr("element_bytes");
    a.clc();
    a.lda(z(ARRAY));
    a.adc(z(OFFSET));
    a.sta(z(zp::VARPNT));
    a.lda(z(ARRAY + 1));
    a.adc(z(OFFSET + 1));
    a.sta(z(zp::VARPNT + 1));
    a.tay();
    a.lda(z(zp::VARPNT));
    a.rts();

    // Five bytes a real, three a string's descriptor, two an integer.
    a.here("element_bytes");
    let not_integer = a.local();
    let set = a.local();
    a.ldx(imm(5));
    a.lda(z(zp::INTFLG));
    a.bpl(not_integer);
    a.ldx(imm(2));
    a.bind(not_integer);
    a.lda(z(zp::VALTYP));
    a.beq(set);
    a.ldx(imm(3));
    a.bind(set);
    a.stx(z(FACTOR));
    a.sty(z(ADDEND));
    a.lda(imm(0));
    a.sta(z(FACTOR + 1));
    a.sta(z(ADDEND + 1));
    a.fall_into("multiply_add");

    a.here("multiply_add");
    let bit = a.local();
    let skip = a.local();
    let done = a.local();
    let overflow = a.local();
    a.bind(bit);
    a.lsr(z(FACTOR + 1));
    a.ror(z(FACTOR));
    a.bcc(skip);
    a.clc();
    a.lda(z(ADDEND));
    a.adc(z(OFFSET));
    a.sta(z(ADDEND));
    a.lda(z(ADDEND + 1));
    a.adc(z(OFFSET + 1));
    a.sta(z(ADDEND + 1));
    a.bcs(overflow);
    a.bind(skip);
    a.lda(z(FACTOR));
    a.ora(z(FACTOR + 1));
    a.beq(done);
    a.asl(z(OFFSET));
    a.rol(z(OFFSET + 1));
    a.bcc(bit);
    a.bind(overflow);
    a.jmp("out_of_memory_error");
    a.bind(done);
    a.lda(z(ADDEND));
    a.sta(z(OFFSET));
    a.lda(z(ADDEND + 1));
    a.sta(z(OFFSET + 1));
    a.rts();
}

/// DIM: each array, or simple variable, named made.
fn dim(a: &mut Asm) {
    a.here("st_dim");
    let variable = a.here_local();
    a.ldx(imm(DIMENSIONING));
    a.stx(z(zp::DIMFLG));
    a.jsr("find_variable");
    a.lda(imm(0));
    a.sta(z(zp::DIMFLG));
    a.jsr("comma_next");
    a.bcc(variable);
    a.rts();
}

/// DEF FN: in a program, the function's variable made — its name's first
/// character marked — holding where its expression starts and its
/// argument's variable.
fn functions(a: &mut Asm) {
    let syntax = a.local();
    a.here("st_def");
    a.jsr("check_program");
    a.lda(imm(token::FN));
    a.jsr("expect_char");
    a.jsr("fn_variable");
    a.sta(z(zp::DEFPNT));
    a.sty(z(zp::DEFPNT + 1));
    a.jsr("expect_open");
    a.ldx(imm(SUBSCRIPTS_REFUSED));
    a.stx(z(zp::SUBFLG));
    a.jsr("find_variable");
    a.jsr("check_numeric");
    a.jsr("expect_close");
    a.lda(imm(token::EQUAL));
    a.jsr("expect_char");
    a.ldy(imm(0));
    for byte in [zp::TXTPTR, zp::TXTPTR + 1, zp::VARPNT, zp::VARPNT + 1] {
        a.lda(z(byte));
        a.sta(z(zp::DEFPNT).ind_y());
        a.iny();
    }
    a.jmp("st_data");

    // The function named at TXTPTR, from its letter in A.
    a.here("fn_variable");
    a.ldx(imm(SUBSCRIPTS_REFUSED));
    a.stx(z(zp::SUBFLG));
    a.jsr("parse_name");
    a.lda(z(zp::VALTYP));
    a.bne(syntax);
    a.lda(z(zp::VARNAM));
    a.ora(imm(NAME_MARK));
    a.sta(z(zp::VARNAM));
    a.lda(imm(0));
    a.sta(z(zp::SUBFLG));
    a.jmp("simple_variable");
    a.bind(syntax);
    a.jmp("syntax_error");
}

/// FN: the argument evaluated into the function's argument variable, the
/// expression evaluated, the variable restored; none defined is UNDEF'D
/// FUNCTION.
fn function_calls(a: &mut Asm) {
    a.here("fn_call");
    let defined = a.local();
    let save = a.local();
    let restore = a.local();
    a.jsr(zp::CHRGET);
    a.ldx(imm(READING));
    a.stx(z(zp::DIMFLG));
    a.jsr("fn_variable");
    a.ldx(imm(0));
    a.stx(z(zp::DIMFLG));
    a.pha();
    a.tya();
    a.pha();
    a.ldy(imm(0));
    a.lda(z(zp::VARPNT).ind_y());
    a.iny();
    a.ora(z(zp::VARPNT).ind_y());
    a.bne(defined);
    raise(a, number::UNDEFD_FUNCTION);
    a.bind(defined);
    a.jsr("expect_open");
    a.jsr("evaluate_number");
    a.jsr("expect_close");
    a.pla();
    a.sta(z(zp::DEFPNT + 1));
    a.pla();
    a.sta(z(zp::DEFPNT));
    a.ldy(imm(2));
    a.lda(z(zp::DEFPNT).ind_y());
    a.sta(z(zp::INDEX));
    a.iny();
    a.lda(z(zp::DEFPNT).ind_y());
    a.sta(z(zp::INDEX + 1));
    a.ldy(imm(4));
    a.bind(save);
    a.lda(z(zp::INDEX).ind_y());
    a.pha();
    a.dey();
    a.bpl(save);
    for byte in [zp::INDEX + 1, zp::INDEX, zp::TXTPTR + 1, zp::TXTPTR] {
        a.lda(z(byte));
        a.pha();
    }
    a.ldx(z(zp::INDEX));
    a.ldy(z(zp::INDEX + 1));
    a.jsr("fac_store");
    a.ldy(imm(0));
    a.lda(z(zp::DEFPNT).ind_y());
    a.sta(z(zp::TXTPTR));
    a.iny();
    a.lda(z(zp::DEFPNT).ind_y());
    a.sta(z(zp::TXTPTR + 1));
    a.jsr("evaluate_number");
    for byte in [zp::TXTPTR, zp::TXTPTR + 1, zp::INDEX, zp::INDEX + 1] {
        a.pla();
        a.sta(z(byte));
    }
    a.ldy(imm(0));
    a.bind(restore);
    a.pla();
    a.sta(z(zp::INDEX).ind_y());
    a.iny();
    a.cpy(imm(5));
    a.bne(restore);
    a.rts();
}

/// ADRAY1's default: FAC floored to a signed integer in A (high) and Y.
pub(super) fn fac_to_ay(a: &mut Asm) {
    a.here("fac_to_ay");
    a.jsr("fac_to_integer");
    a.lda(z(M3));
    a.ldy(z(M4));
    a.rts();
}
