//! Variables and the heap: finding a name in its initial's list, making a
//! variable at the top of the heap, arrays and their elements, loading and
//! storing a location of any type, and the BASIC stack below HIMEM.
//!
//! An entry is a link, the name's characters after its initial with any `%`,
//! `$` or `(`, a zero, then the value. A string's value is a descriptor: the
//! text's address, the room it has and its length; a string that outgrows
//! its room moves to the top of the heap, unless it is already there.

use super::float::PACKED;
use super::ws::{kind, page, place, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, Label, RTS};

/// The pointer that reads the name with the entry's offsets.
pub(super) const TNAME: u16 = z::RUN;
/// The entry offset of the name's zero.
pub(super) const NAME_END: u16 = TNAME + 2;
/// The name's type, with `ARRAY` for an array.
const NAME_TYPE: u16 = NAME_END + 1;
const ARRAY: u8 = 0x40;
/// Two pointers and a length for moving strings.
pub const FROM: u16 = NAME_TYPE + 1;
pub const TO: u16 = FROM + 2;
pub const LENGTH: u16 = TO + 2;

/// A string descriptor's fields.
const S_ADDRESS: u8 = 0;
const S_ROOM: u8 = 2;
pub(super) const S_LENGTH: u8 = 3;
const _: () = assert!(S_ROOM == S_ADDRESS + 2 && S_LENGTH == S_ROOM + 1);
/// A string given more room gets this much over its length from this
/// length on.
const SLACK_FROM: u8 = 8;
const SLACK: u8 = 8;
const CR: u8 = 0x0D;

pub fn emit(a: &mut Asm) {
    names(a);
    lookup(a);
    let (subscript, range) = arrays(a);
    element_end(a, subscript, range);
    let block = dim(a);
    dim_block(a, block);
    dim_room(a);
    sizing(a);
    creating(a);
    entries(a);
    loading(a);
    storing(a);
    string_stores(a);
    stack(a);
}

/// An array element's subscript work: the dimension's size, and the
/// running offset; and DIM's count of dimensions and where it goes on.
const SIZE: u16 = z::INT_WORK;
const OFFSET: u16 = SIZE + 2;
const DIMS: u16 = z::STATEMENT;
const AFTER: u16 = DIMS + 1;
/// The 6502 stack frame an element is found in, from the stack pointer
/// after its pushes.
const FRAME_INDEX: u16 = 0x0101;
const FRAME_OFFSET: u16 = 0x0102;
const FRAME_BLOCK: u16 = 0x0104;

fn arrays(a: &mut Asm) -> (Label, Label) {
    // The element named by the subscripts after the array's `(`, VAR the
    // array's block: each subscript checked against its size, too many a
    // Missing ), too few an Array. The block, type, offset and next size's
    // place wait on the 6502 stack while each subscript is found.
    a.here("array_element");
    a.lda(zp(z::VTYPE));
    a.and(imm(!ARRAY));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(imm(0));
    a.pha();
    a.pha();
    a.lda(imm(1));
    a.pha();
    let subscript = a.here_local();
    a.jsr("expr_integer");
    a.stx(zp(z::OP));
    a.tsx();
    a.lda(abs(FRAME_BLOCK).x());
    a.sta(zp(z::VAR));
    a.lda(abs(FRAME_BLOCK + 1).x());
    a.sta(zp(z::VAR + 1));
    a.ldy(abs(FRAME_INDEX).x());
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(SIZE));
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(SIZE + 1));
    a.iny();
    a.tya();
    a.sta(abs(FRAME_INDEX).x());
    let range = a.local();
    a.lda(zp(z::IAC + 2));
    a.ora(zp(z::IAC + 3));
    a.bne(range);
    a.lda(zp(z::IAC));
    a.cmp(zp(SIZE));
    a.lda(zp(z::IAC + 1));
    a.sbc(zp(SIZE + 1));
    a.bcs(range);
    // OFFSET = OFFSET × SIZE + subscript.
    a.lda(abs(FRAME_OFFSET).x());
    a.sta(zp(OFFSET));
    a.lda(abs(FRAME_OFFSET + 1).x());
    a.sta(zp(OFFSET + 1));
    a.jsr("times_size");
    a.clc();
    a.lda(zp(z::IAC));
    a.adc(zp(OFFSET));
    a.tsx();
    a.sta(abs(FRAME_OFFSET).x());
    a.lda(zp(z::IAC + 1));
    a.adc(zp(OFFSET + 1));
    a.sta(abs(FRAME_OFFSET + 1).x());
    (subscript, range)
}

fn element_end(a: &mut Asm, subscript: Label, range: Label) {
    // The last subscript when the next size's place is the dimension byte.
    a.ldy(imm(0));
    a.lda(abs(FRAME_INDEX).x());
    a.cmp(zp(z::VAR).ind_y());
    let (last, more) = (a.local(), a.local());
    a.beq(last);
    a.lda(zp(z::OP));
    a.cmp(imm(b','));
    a.beq(subscript);
    a.jmp("err_array");
    a.bind(range);
    a.jmp("err_subscript");
    a.bind(last);
    a.lda(zp(z::OP));
    a.cmp(imm(b')'));
    a.beq(more);
    a.jmp("err_missing_bracket");
    a.bind(more);
    // VAR = the block past its sizes, plus the offset in elements.
    a.pla();
    a.sta(zp(SIZE));
    a.pla();
    a.sta(zp(OFFSET));
    a.pla();
    a.sta(zp(OFFSET + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VTYPE));
    a.lda(zp(OFFSET));
    a.ldx(zp(OFFSET + 1));
    a.asl(zp(OFFSET));
    a.rol(zp(OFFSET + 1));
    a.asl(zp(OFFSET));
    a.rol(zp(OFFSET + 1));
    a.ldy(zp(z::VTYPE));
    a.cpy(imm(place::REAL));
    let four = a.local();
    a.bne(four);
    a.clc();
    a.adc(zp(OFFSET));
    a.sta(zp(OFFSET));
    a.txa();
    a.adc(zp(OFFSET + 1));
    a.sta(zp(OFFSET + 1));
    a.bind(four);
    a.lda(zp(SIZE));
    a.clc();
    a.adc(zp(OFFSET));
    a.tay();
    a.lda(imm(0));
    a.adc(zp(OFFSET + 1));
    a.tax();
    a.tya();
    a.clc();
    a.adc(zp(z::VAR));
    a.sta(zp(z::VAR));
    a.txa();
    a.adc(zp(z::VAR + 1));
    a.sta(zp(z::VAR + 1));
    a.clc();
    a.rts();
}

fn dim(a: &mut Asm) -> Label {
    // DIM: arrays and byte blocks, separated by commas.
    a.here("statement_dim");
    a.jsr("next_a");
    a.dec(zp(z::OFFSET_A));
    a.lda(zp(z::OFFSET_A));
    a.pha();
    a.jsr("find_name");
    // A name without a bracket takes a byte block; an array that exists
    // is Bad DIM.
    a.lda(zp(NAME_TYPE));
    a.and(imm(ARRAY));
    let (absent, bad, block) = (a.local(), a.local(), a.local());
    a.beq(block);
    a.bcs(absent);
    a.bind(bad);
    a.jmp("err_bad_dim");
    a.bind(absent);
    // Each dimension pushed as an integer, its size one more; the count in
    // the first frame byte.
    a.lda(imm(0));
    a.pha();
    let dimension = a.here_local();
    a.jsr("expr_integer");
    a.lda(zp(z::IAC + 3));
    a.ora(zp(z::IAC + 2));
    a.bne(bad);
    a.inc(zp(z::IAC));
    let low = a.local();
    a.bne(low);
    a.inc(zp(z::IAC + 1));
    a.beq(bad);
    a.bind(low);
    a.txa();
    a.pha();
    a.lda(imm(kind::INTEGER));
    a.jsr("push_value");
    a.pla();
    a.tax();
    a.pla();
    a.clc();
    a.adc(imm(1));
    a.pha();
    a.cpx(imm(b','));
    a.beq(dimension);
    a.jsr("closing_bracket");
    // The name found again, then the block: the dimension byte, sizes and
    // zeroed elements.
    a.pla();
    a.sta(zp(DIMS));
    a.pla();
    a.ldx(zp(z::OFFSET_A));
    a.stx(zp(AFTER));
    a.sta(zp(z::OFFSET_A));
    a.jsr("find_name");
    a.lda(zp(AFTER));
    a.sta(zp(z::OFFSET_A));
    a.jsr("array_bytes");
    a.jsr("dim_fits");
    a.jsr("create_entry");
    a.ldy(imm(0));
    a.lda(zp(DIMS));
    a.asl(Acc);
    a.ora(imm(1));
    a.sta(zp(z::VAR).ind_y());
    a.tay();
    let size = a.here_local();
    a.dey();
    let sized = a.local();
    a.beq(sized);
    a.tya();
    a.pha();
    a.jsr("pop_int");
    a.pla();
    a.tay();
    a.lda(zp(z::IAC2 + 1));
    a.sta(zp(z::VAR).ind_y());
    a.dey();
    a.lda(zp(z::IAC2));
    a.sta(zp(z::VAR).ind_y());
    a.jmp(size);
    a.bind(sized);
    a.jmp("dim_next");
    block
}

fn dim_block(a: &mut Asm, block: Label) {
    // A byte block: the variable holds its address, the block one more than
    // the size given.
    let near_bad = a.local();
    a.bind(near_bad);
    a.jmp("err_bad_dim");
    a.bind(block);
    a.pla();
    a.sta(zp(z::OFFSET_A));
    a.jsr("lvalue");
    a.bcs(near_bad);
    a.lda(zp(z::VTYPE));
    a.bmi(near_bad);
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VTYPE));
    a.pha();
    a.jsr("expr_integer");
    a.stx(zp(z::OP));
    a.lda(zp(z::IAC + 3));
    a.bmi(near_bad);
    a.ora(zp(z::IAC + 2));
    let space = a.local();
    a.bne(space);
    a.pla();
    a.sta(zp(z::VTYPE));
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.ldy(zp(z::VARTOP));
    a.ldx(zp(z::VARTOP + 1));
    a.tya();
    a.pha();
    a.txa();
    a.pha();
    a.lda(zp(z::IAC));
    a.ldx(zp(z::IAC + 1));
    a.clc();
    a.adc(imm(1));
    let carried = a.local();
    a.bcc(carried);
    a.inx();
    a.bind(carried);
    a.ldy(imm(0));
    a.sty(zp(NAME_END));
    a.jsr("dim_fits");
    a.jsr("heap_claim");
    a.pla();
    a.sta(zp(z::IAC + 1));
    a.pla();
    a.sta(zp(z::IAC));
    a.lda(imm(0));
    a.sta(zp(z::IAC + 2));
    a.sta(zp(z::IAC + 3));
    a.lda(imm(kind::INTEGER));
    a.jsr("store_variable");
    a.ldx(zp(z::OP));
    a.dec(zp(z::OFFSET_A));
    a.here("dim_next");
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.bne("dim_end");
    a.jmp("statement_dim");
    a.here("dim_end");
    a.tax();
    a.jmp("statement_done_x");
    a.bind(space);
    a.jmp("err_dim_space");
}

fn dim_room(a: &mut Asm) {
    // DIM space unless XA bytes and an entry's NAME_END + 1 fit below the
    // BASIC stack; XA kept.
    a.here("dim_fits");
    a.pha();
    a.sec();
    a.adc(zp(NAME_END));
    a.tay();
    a.txa();
    a.adc(imm(0));
    let full = a.local();
    a.bcs(full);
    a.pha();
    a.tya();
    a.clc();
    a.adc(zp(z::VARTOP));
    a.tay();
    a.pla();
    a.adc(zp(z::VARTOP + 1));
    a.bcs(full);
    a.cpy(zp(z::STACK));
    a.sbc(zp(z::STACK + 1));
    a.bcs(full);
    a.pla();
    a.rts();
    a.bind(full);
    a.jmp("err_dim_space");

    // The bytes of an array of DIMS dimensions whose sizes are stacked:
    // the dimension byte, two per size, and each element; DIM space past
    // sixteen bits. XA the count.
    a.here("array_bytes");
    a.lda(imm(1));
    a.sta(zp(OFFSET));
    a.lda(imm(0));
    a.sta(zp(OFFSET + 1));
    a.ldy(imm(0));
    let each = a.here_local();
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(SIZE));
    a.iny();
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(SIZE + 1));
    a.iny();
    a.iny();
    a.iny();
    a.tya();
    a.pha();
    a.jsr("times_size_checked");
    a.pla();
    a.tay();
    a.lsr(Acc);
    a.lsr(Acc);
    a.cmp(zp(DIMS));
    a.bne(each);
    // × the element size, plus the sizes and dimension byte.
    a.lda(zp(NAME_TYPE));
    a.and(imm(0x0F));
    a.cmp(imm(place::REAL));
    a.lda(imm(4));
    let sized = a.local();
    a.bcc(sized);
    a.lda(imm(PACKED));
    a.bind(sized);
    a.sta(zp(SIZE));
    a.lda(imm(0));
    a.sta(zp(SIZE + 1));
    a.jsr("times_size_checked");
    a.lda(zp(DIMS));
    a.asl(Acc);
    a.sec();
    a.adc(zp(OFFSET));
    a.ldx(zp(OFFSET + 1));
    a.bcc(RTS);
    a.inx();
    a.bne(RTS);
    a.jmp("err_dim_space");
}

fn sizing(a: &mut Asm) {
    // OFFSET × SIZE, DIM space past sixteen bits.
    a.here("times_size_checked");
    a.jsr("times_size");
    a.lda(zp(TO + 2));
    a.beq(RTS);
    a.jmp("err_dim_space");

    // OFFSET × SIZE in OFFSET, sixteen bits, past them in TO + 2; an
    // element's first subscript has nothing to multiply.
    a.here("times_size");
    a.lda(imm(0));
    a.sta(zp(TO));
    a.sta(zp(TO + 1));
    a.sta(zp(TO + 2));
    a.lda(zp(OFFSET));
    a.ora(zp(OFFSET + 1));
    a.beq(RTS);
    a.ldy(imm(16));
    let bit = a.here_local();
    a.asl(zp(TO));
    a.rol(zp(TO + 1));
    a.rol(zp(TO + 2));
    a.asl(zp(OFFSET));
    a.rol(zp(OFFSET + 1));
    let skip = a.local();
    a.bcc(skip);
    a.clc();
    a.lda(zp(TO));
    a.adc(zp(SIZE));
    a.sta(zp(TO));
    a.lda(zp(TO + 1));
    a.adc(zp(SIZE + 1));
    a.sta(zp(TO + 1));
    let no_carry = a.local();
    a.bcc(no_carry);
    a.inc(zp(TO + 2));
    a.bind(no_carry);
    a.bind(skip);
    a.dey();
    a.bne(bit);
    a.lda(zp(TO));
    a.sta(zp(OFFSET));
    a.lda(zp(TO + 1));
    a.sta(zp(OFFSET + 1));
    a.rts();
}

fn names(a: &mut Asm) {
    // The name at the statement pointer. C clear: VAR its value, VTYPE and
    // NAME_TYPE its type. C set: no such name, VAR the entry to link a new
    // one from. The pointer is left past the name in both.
    a.here("find_name");
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'Z' + 1));
    let general = a.local();
    a.bcs(general);
    a.tax();
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'%'));
    let general_x = a.local();
    a.bne(general_x);
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'('));
    a.beq(general_x);
    // A resident integer, @% to Z%, at &400 + 4n.
    a.sty(zp(z::OFFSET_A));
    a.txa();
    a.asl(Acc);
    a.asl(Acc);
    a.sta(zp(z::VAR));
    a.lda(imm(hi(page::RESIDENT)));
    a.sta(zp(z::VAR + 1));
    a.lda(imm(place::INTEGER));
    a.sta(zp(z::VTYPE));
    a.sta(zp(NAME_TYPE));
    a.clc();
    a.rts();
    a.bind(general_x);
    a.txa();
    a.bind(general);
    // The initial's list head at &400 + 2 × initial.
    a.asl(Acc);
    a.sta(zp(z::VAR));
    a.lda(imm(hi(page::CATALOGUE)));
    a.sta(zp(z::VAR + 1));
    a.lda(zp(z::TEXT_A));
    a.clc();
    a.adc(zp(z::OFFSET_A));
    a.sta(zp(TNAME));
    a.lda(zp(z::TEXT_A + 1));
    a.adc(imm(0));
    a.sta(zp(TNAME + 1));
    a.lda(zp(TNAME));
    let low = a.local();
    a.bne(low);
    a.dec(zp(TNAME + 1));
    a.bind(low);
    a.dec(zp(TNAME));
    a.ldy(zp(z::OFFSET_A));
    let letters = a.here_local();
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.jsr("is_name");
    a.bcs(letters);
    a.ldx(imm(place::REAL));
    a.cmp(imm(b'%'));
    let (string, suffixed) = (a.local(), a.local());
    a.bne(string);
    a.ldx(imm(place::INTEGER));
    a.bne(suffixed);
    a.bind(string);
    a.cmp(imm(b'$'));
    let plain = a.local();
    a.bne(plain);
    a.ldx(imm(place::STRING));
    a.bind(suffixed);
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.bind(plain);
    a.cmp(imm(b'('));
    let scalar = a.local();
    a.bne(scalar);
    a.txa();
    a.ora(imm(ARRAY));
    a.tax();
    a.iny();
    a.bind(scalar);
    a.stx(zp(NAME_TYPE));
    a.stx(zp(z::VTYPE));
    // The entry offset of the name's zero: its length after the initial,
    // plus the link.
    a.tya();
    a.sec();
    a.sbc(zp(z::OFFSET_A));
    a.clc();
    a.adc(imm(1));
    a.sta(zp(NAME_END));
    a.sty(zp(z::OFFSET_A));
}

fn lookup(a: &mut Asm) {
    // The list from VAR searched for the name TNAME reads up to NAME_END.
    a.here("search_list");
    let (search, compare, ended) = (a.here_local(), a.local(), a.local());
    a.ldy(imm(0));
    a.lda(zp(z::VAR).ind_y());
    a.tax();
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    let missing = a.local();
    a.beq(missing);
    a.sta(zp(z::VAR + 1));
    a.stx(zp(z::VAR));
    a.ldy(imm(2));
    a.bind(compare);
    a.cpy(zp(NAME_END));
    a.beq(ended);
    a.lda(zp(z::VAR).ind_y());
    a.cmp(zp(TNAME).ind_y());
    a.bne(search);
    a.iny();
    a.bne(compare);
    a.bind(ended);
    a.lda(zp(z::VAR).ind_y());
    a.bne(search);
    a.tya();
    a.sec();
    a.adc(zp(z::VAR));
    a.sta(zp(z::VAR));
    let same = a.local();
    a.bcc(same);
    a.inc(zp(z::VAR + 1));
    a.bind(same);
    a.clc();
    a.rts();
    a.bind(missing);
    a.sec();
    a.rts();

    // The variable at the statement pointer as a location: an array's
    // element, or a name made when `make` and absent. C set if absent and
    // not made.
    a.here("variable");
    a.jsr("find_name");
    let found = a.local();
    a.bcc(found);
    a.lda(zp(NAME_TYPE));
    a.and(imm(ARRAY));
    let not_array = a.local();
    a.beq(not_array);
    a.jmp("err_array");
    a.bind(not_array);
    a.sec();
    a.rts();
    a.bind(found);
    a.lda(zp(NAME_TYPE));
    a.and(imm(ARRAY));
    a.beq(RTS);
    a.jmp("array_element");
}

fn creating(a: &mut Asm) {
    // A scalar of NAME_TYPE named at TNAME, made at VARTOP and linked from
    // VAR; VAR then its value, zeroed.
    a.here("create_variable");
    a.ldx(imm(0));
    a.lda(zp(NAME_TYPE));
    a.and(imm(0x0F));
    a.cmp(imm(place::REAL));
    let sized = a.local();
    a.beq(sized);
    a.lda(imm(4));
    a.bind(sized);
    a.fall_into("create_entry");
}

fn entries(a: &mut Asm) {
    // The entry for the name with XA bytes of value, all zero.
    a.here("create_entry");
    a.sta(zp(LENGTH));
    a.stx(zp(LENGTH + 1));
    a.ldy(imm(0));
    a.lda(zp(z::VARTOP));
    a.sta(zp(z::VAR).ind_y());
    a.iny();
    a.lda(zp(z::VARTOP + 1));
    a.sta(zp(z::VAR).ind_y());
    a.lda(zp(z::VARTOP));
    a.sta(zp(z::VAR));
    a.lda(zp(z::VARTOP + 1));
    a.sta(zp(z::VAR + 1));
    a.lda(zp(NAME_END));
    a.sec();
    a.adc(zp(LENGTH));
    a.ldx(zp(LENGTH + 1));
    let low = a.local();
    a.bcc(low);
    a.inx();
    a.bind(low);
    a.jsr("heap_claim");
    // The link, the name and its zero.
    a.ldy(zp(NAME_END));
    a.lda(imm(0));
    a.sta(zp(z::VAR).ind_y());
    let name = a.here_local();
    a.dey();
    a.cpy(imm(1));
    let named = a.local();
    a.beq(named);
    a.lda(zp(TNAME).ind_y());
    a.sta(zp(z::VAR).ind_y());
    a.bne(name);
    a.bind(named);
    a.lda(imm(0));
    a.sta(zp(z::VAR).ind_y());
    a.dey();
    a.sta(zp(z::VAR).ind_y());
    a.lda(zp(NAME_END));
    a.sec();
    a.adc(zp(z::VAR));
    a.sta(zp(z::VAR));
    let same = a.local();
    a.bcc(same);
    a.inc(zp(z::VAR + 1));
    a.bind(same);
    a.fall_into("zero_value");

    // LENGTH bytes at VAR zeroed; VAR kept.
    a.here("zero_value");
    a.lda(zp(z::VAR));
    a.sta(zp(TO));
    a.lda(zp(z::VAR + 1));
    a.sta(zp(TO + 1));
    a.ldy(imm(0));
    a.ldx(zp(LENGTH + 1));
    let (pages, rest) = (a.local(), a.local());
    a.beq(rest);
    a.tya();
    a.bind(pages);
    let page_loop = a.here_local();
    a.sta(zp(TO).ind_y());
    a.iny();
    a.bne(page_loop);
    a.inc(zp(TO + 1));
    a.dex();
    a.bne(pages);
    a.bind(rest);
    a.ldy(zp(LENGTH));
    a.beq(RTS);
    a.lda(imm(0));
    let bytes = a.here_local();
    a.dey();
    a.sta(zp(TO).ind_y());
    a.bne(bytes);
    a.rts();

    // VARTOP moved up XA bytes; No room if that meets the BASIC stack.
    a.here("heap_claim");
    a.clc();
    a.adc(zp(z::VARTOP));
    a.tay();
    a.txa();
    a.adc(zp(z::VARTOP + 1));
    let full = a.local();
    a.bcs(full);
    a.tax();
    a.cpy(zp(z::STACK));
    a.sbc(zp(z::STACK + 1));
    a.bcs(full);
    a.sty(zp(z::VARTOP));
    a.stx(zp(z::VARTOP + 1));
    a.rts();
    a.bind(full);
    a.jmp("err_no_room");
}

fn loading(a: &mut Asm) {
    // The location VAR of type VTYPE loaded: A its kind.
    a.here("load_variable");
    a.ldy(zp(z::VTYPE));
    let (string, real, integer) = (a.local(), a.local(), a.local());
    a.bmi(string);
    a.cpy(imm(place::REAL));
    a.beq(real);
    a.ldy(imm(0));
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(z::IAC));
    a.lda(zp(z::VTYPE));
    a.bne(integer);
    // A byte, as an integer.
    a.sty(zp(z::IAC + 1));
    a.sty(zp(z::IAC + 2));
    a.sty(zp(z::IAC + 3));
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(integer);
    for i in 1..4 {
        a.iny();
        a.lda(zp(z::VAR).ind_y());
        a.sta(zp(z::IAC + i));
    }
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(real);
    a.jsr("fac_load");
    a.lda(imm(kind::REAL));
    a.rts();
    // A string variable's text through its descriptor; a fixed string's
    // to its CR, which, as BASIC II has it, is empty with no CR in 256
    // bytes.
    a.bind(string);
    a.tya();
    a.lsr(Acc);
    a.ldy(imm(0));
    let fixed = a.local();
    a.bcc(fixed);
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(FROM));
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(FROM + 1));
    a.ldy(imm(S_LENGTH));
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(z::STRING_LENGTH));
    a.tay();
    let (copy, copied) = (a.here_local(), a.local());
    a.beq(copied);
    a.dey();
    a.lda(zp(FROM).ind_y());
    a.sta(abs(page::STRING).y());
    a.tya();
    a.jmp(copy);
    a.bind(copied);
    a.lda(imm(kind::STRING));
    a.rts();
    a.bind(fixed);
    let text = a.here_local();
    a.lda(zp(z::VAR).ind_y());
    a.cmp(imm(CR));
    let ended = a.local();
    a.beq(ended);
    a.sta(abs(page::STRING).y());
    a.iny();
    a.bne(text);
    a.bind(ended);
    a.sty(zp(z::STRING_LENGTH));
    a.lda(imm(kind::STRING));
    a.rts();
}

fn storing(a: &mut Asm) {
    // The value of kind A stored at VAR as its VTYPE: a number as a string
    // or a string as a number is a Type mismatch; reals and integers
    // convert, integers toward zero.
    a.here("store_variable");
    a.ldy(zp(z::VTYPE));
    let (to_string, mismatch) = (a.local(), a.local());
    a.bmi(to_string);
    a.tax();
    a.beq(mismatch);
    a.cpy(imm(place::REAL));
    let to_integer = a.local();
    a.bne(to_integer);
    a.tax();
    let real = a.local();
    a.bmi(real);
    a.jsr("fac_from_int");
    a.bind(real);
    a.jmp("fac_store");
    a.bind(to_integer);
    a.tax();
    let integer = a.local();
    a.bpl(integer);
    a.jsr("int_from_fac");
    a.bind(integer);
    a.ldx(imm(4));
    a.ldy(zp(z::VTYPE));
    let byte = a.local();
    a.beq(byte);
    a.ldy(imm(0));
    let copy = a.here_local();
    a.lda(abs(z::IAC).y());
    a.sta(zp(z::VAR).ind_y());
    a.iny();
    a.dex();
    a.bne(copy);
    a.rts();
    a.bind(byte);
    a.lda(zp(z::IAC));
    a.sta(zp(z::VAR).ind_y());
    a.rts();
    a.bind(mismatch);
    a.jmp("err_type_mismatch");
    a.bind(to_string);
    a.tax();
    a.bne(mismatch);
    a.lda(imm(lo(page::STRING)));
    a.sta(zp(FROM));
    a.lda(imm(hi(page::STRING)));
    a.sta(zp(FROM + 1));
    a.fall_into("store_string");
}

fn string_stores(a: &mut Asm) {
    // STRING_LENGTH bytes from FROM stored in the string at VAR of either
    // kind: a fixed string's text and a CR at its address.
    a.here("store_string");
    a.lda(zp(z::VTYPE));
    a.lsr(Acc);
    a.bcs("store_string_variable");
    a.ldy(zp(z::STRING_LENGTH));
    a.lda(imm(CR));
    a.sta(zp(z::VAR).ind_y());
    a.lda(zp(z::VAR));
    a.sta(zp(TO));
    a.lda(zp(z::VAR + 1));
    a.sta(zp(TO + 1));
    a.jmp("store_text");

    // ... in the string variable at VAR: room enough where it is, or more
    // at the top of the heap, taken in place when the text is already the
    // heap's last. More is the length, and from SLACK_FROM on SLACK over
    // it, at most 255, as BASIC II gives.
    a.here("store_string_variable");
    let (placed, at_top, fits, elsewhere) = (a.local(), a.local(), a.local(), a.local());
    a.ldy(imm(S_ROOM));
    a.lda(zp(z::STRING_LENGTH));
    a.cmp(zp(z::VAR).ind_y());
    a.bcc(fits);
    a.beq(fits);
    let sized = a.local();
    a.cmp(imm(SLACK_FROM));
    a.bcc(sized);
    a.adc(imm(SLACK - 1));
    a.bcc(sized);
    a.lda(imm(0xFF));
    a.bind(sized);
    a.pha();
    a.lda(zp(z::VAR).ind_y());
    a.ldy(imm(S_ADDRESS));
    a.clc();
    a.adc(zp(z::VAR).ind_y());
    a.tax();
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    a.adc(imm(0));
    a.cpx(zp(z::VARTOP));
    a.bne(elsewhere);
    a.cmp(zp(z::VARTOP + 1));
    a.beq(at_top);
    a.bind(elsewhere);
    a.lda(zp(z::VARTOP + 1));
    a.sta(zp(z::VAR).ind_y());
    a.dey();
    a.lda(zp(z::VARTOP));
    a.sta(zp(z::VAR).ind_y());
    a.pla();
    a.pha();
    a.ldx(imm(0));
    a.jsr("heap_claim");
    a.bcc(placed);
    a.bind(at_top);
    a.iny();
    a.pla();
    a.pha();
    a.sec();
    a.sbc(zp(z::VAR).ind_y());
    a.ldx(imm(0));
    a.jsr("heap_claim");
    a.bind(placed);
    a.ldy(imm(S_ROOM));
    a.pla();
    a.sta(zp(z::VAR).ind_y());
    a.lda(zp(z::STRING_LENGTH));
    a.bind(fits);
    a.iny();
    a.sta(zp(z::VAR).ind_y());
    a.ldy(imm(S_ADDRESS));
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(TO));
    a.iny();
    a.lda(zp(z::VAR).ind_y());
    a.sta(zp(TO + 1));

    // STRING_LENGTH bytes from FROM copied to TO.
    a.here("store_text");
    a.ldy(zp(z::STRING_LENGTH));
    let text = a.here_local();
    a.beq(RTS);
    a.dey();
    a.lda(zp(FROM).ind_y());
    a.sta(zp(TO).ind_y());
    a.tya();
    a.jmp(text);
}

fn stack(a: &mut Asm) {
    // The value of kind A pushed on the BASIC stack; A kept.
    a.here("push_value");
    a.tax();
    let (string, integer) = (a.local(), a.local());
    a.beq(string);
    a.bpl(integer);
    a.lda(imm(PACKED));
    a.sec();
    a.jsr("stack_claim");
    a.jsr("var_at_stack");
    a.jsr("fac_store");
    a.lda(imm(kind::REAL));
    a.rts();
    a.bind(integer);
    a.lda(imm(4));
    a.sec();
    a.jsr("stack_claim");
    a.ldy(imm(3));
    let copy = a.here_local();
    a.lda(abs(z::IAC).y());
    a.sta(zp(z::STACK).ind_y());
    a.dey();
    a.bpl(copy);
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(string);
    a.lda(zp(z::STRING_LENGTH));
    a.clc();
    a.jsr("stack_claim");
    a.ldy(imm(0));
    a.lda(zp(z::STRING_LENGTH));
    a.sta(zp(z::STACK).ind_y());
    let (text, done) = (a.here_local(), a.local());
    a.cpy(zp(z::STRING_LENGTH));
    a.beq(done);
    a.lda(abs(page::STRING).y());
    a.iny();
    a.sta(zp(z::STACK).ind_y());
    a.bne(text);
    a.bind(done);
    a.lda(imm(kind::STRING));
    a.rts();

    // The BASIC stack down A bytes, one more with C clear; No room if that
    // meets the heap.
    a.here("stack_claim");
    a.sta(zp(LENGTH));
    a.lda(zp(z::STACK));
    a.sbc(zp(LENGTH));
    a.tay();
    a.lda(zp(z::STACK + 1));
    a.sbc(imm(0));
    let full = a.local();
    a.bcc(full);
    a.tax();
    a.cpy(zp(z::VARTOP));
    a.sbc(zp(z::VARTOP + 1));
    a.bcc(full);
    a.sty(zp(z::STACK));
    a.stx(zp(z::STACK + 1));
    a.rts();
    a.bind(full);
    a.jmp("err_no_room");

    // The integer on the BASIC stack popped into IAC2.
    a.here("pop_int");
    a.ldy(imm(3));
    let copy = a.here_local();
    a.lda(zp(z::STACK).ind_y());
    a.sta(abs(z::IAC2).y());
    a.dey();
    a.bpl(copy);
    a.lda(imm(4));
    a.fall_into("stack_release");

    // The BASIC stack up A bytes.
    a.here("stack_release");
    a.clc();
    a.adc(zp(z::STACK));
    a.sta(zp(z::STACK));
    a.bcc(RTS);
    a.inc(zp(z::STACK + 1));
    a.rts();

    // The real on the BASIC stack popped into FAC2.
    a.here("pop_real");
    a.jsr("var_at_stack");
    a.jsr("fac2_load");
    a.lda(imm(PACKED));
    a.jmp("stack_release");

    // VAR at the BASIC stack's top.
    a.here("var_at_stack");
    a.lda(zp(z::STACK));
    a.sta(zp(z::VAR));
    a.lda(zp(z::STACK + 1));
    a.sta(zp(z::VAR + 1));
    a.rts();

    // The string on the BASIC stack dropped.
    a.here("pop_string");
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sec();
    a.adc(imm(0));
    a.bcc("stack_release");
    a.inc(zp(z::STACK + 1));
    a.rts();
}
