//! Five-byte reals: the two accumulators, packing with BASIC II's rounding,
//! conversion to and from integers, and the four operations carried out as
//! BASIC II carries them out, so every result has its bits.
//!
//! An accumulator holds a sign (bit 7), an exponent widened to sixteen bits
//! by an overflow byte while an operation runs, and a 40-bit mantissa: the
//! four packed bytes and a rounding byte. A normalised mantissa has its top
//! bit set; zero has a clear top byte. Each operation works in forty bits
//! and rounds its result to the packed thirty-two; only the decimal scaling
//! keeps all forty from step to step.

use super::ws::zp as z;
use crate::rom::asm::{imm, lo, zp, Acc, Asm, Label, RTS};

/// An accumulator's fields.
#[derive(Clone, Copy)]
pub struct Real(u16);

impl Real {
    pub const fn sign(self) -> u16 {
        self.0
    }
    pub const fn over(self) -> u16 {
        self.0 + 1
    }
    pub const fn exp(self) -> u16 {
        self.0 + 2
    }
    /// The 40-bit mantissa's byte `i`, most significant first; byte 4 is the
    /// rounding byte.
    pub const fn m(self, i: u16) -> u16 {
        self.0 + 3 + i
    }
}

pub const FAC: Real = Real(z::FAC);
pub const FAC2: Real = Real(z::FAC2);
/// A packed real's length.
pub const PACKED: u8 = 5;
/// An accumulator's length.
const FIELDS: u8 = 8;
/// FAC2's distance from FAC, so an indexed field serves either.
const FAC2_FROM_FAC: u8 = (z::FAC2 - z::FAC).to_le_bytes()[0];
/// The exponent of a mantissa read as a 32-bit integer.
const INTEGER_EXP: u8 = 0xA0;

/// The multiplier or divisor.
const WORK: u16 = z::FP_WORK;
/// The quotient bits a division finds.
const QUOTIENT_BITS: u8 = 39;
/// Whether converting to an integer dropped a fraction.
pub const FRACTION: u16 = z::INT_WORK;

pub fn emit(a: &mut Asm) {
    moves(a);
    packing(a);
    integers(a);
    to_integer(a);
    normalising(a);
    adding(a);
    let zero = a.local();
    multiplying(a, zero);
    dividing(a, zero);
    comparing(a);
    tens(a);
}

fn shift_right(a: &mut Asm, r: Real) {
    a.lsr(zp(r.m(0)));
    for i in 1..5 {
        a.ror(zp(r.m(i)));
    }
}

fn rotate_left(a: &mut Asm, r: Real) {
    a.rol(zp(r.m(4)));
    for i in (0..4).rev() {
        a.rol(zp(r.m(i)));
    }
}

/// `to` += `from`, 40 bits; C the carry out.
fn add40(a: &mut Asm, to: Real, from: u16) {
    a.clc();
    for i in (0..5).rev() {
        a.lda(zp(to.m(i)));
        a.adc(zp(from + i));
        a.sta(zp(to.m(i)));
    }
}

/// The exponent's sixteen bits less one.
fn decrement_exp(a: &mut Asm) {
    a.lda(zp(FAC.exp()));
    let low = a.local();
    a.bne(low);
    a.dec(zp(FAC.over()));
    a.bind(low);
    a.dec(zp(FAC.exp()));
}

fn moves(a: &mut Asm) {
    a.here("fac_zero");
    a.lda(imm(0));
    a.ldx(imm(FIELDS - 1));
    let clear = a.here_local();
    a.sta(zp(z::FAC).x());
    a.dex();
    a.bpl(clear);
    a.rts();

    a.here("fac_from_fac2");
    a.ldx(imm(FIELDS - 1));
    let copy = a.here_local();
    a.lda(zp(z::FAC2).x());
    a.sta(zp(z::FAC).x());
    a.dex();
    a.bpl(copy);
    a.rts();

    a.here("fac_to_fac2");
    a.ldx(imm(FIELDS - 1));
    let copy = a.here_local();
    a.lda(zp(z::FAC).x());
    a.sta(zp(z::FAC2).x());
    a.dex();
    a.bpl(copy);
    a.rts();

    a.here("fac_swap");
    a.ldx(imm(FIELDS - 1));
    let swap = a.here_local();
    a.lda(zp(z::FAC).x());
    a.ldy(zp(z::FAC2).x());
    a.sta(zp(z::FAC2).x());
    a.sty(zp(z::FAC).x());
    a.dex();
    a.bpl(swap);
    a.rts();

    a.here("fac_negate");
    a.lda(zp(FAC.sign()));
    a.eor(imm(0x80));
    a.sta(zp(FAC.sign()));
    a.rts();
}

/// The packed real at `from` unpacked into `to`: zero only when all five
/// bytes are, so an exponent of 0 with a mantissa is a number.
fn unpack(a: &mut Asm, name: &'static str, from: u16, to: Real) {
    a.here(name);
    a.ldy(imm(PACKED - 1));
    for i in (1..4).rev() {
        a.lda(zp(from).ind_y());
        a.sta(zp(to.m(i)));
        a.dey();
    }
    a.lda(zp(from).ind_y());
    a.sta(zp(to.sign()));
    a.ora(imm(0x80));
    a.sta(zp(to.m(0)));
    a.dey();
    a.lda(zp(from).ind_y());
    a.sta(zp(to.exp()));
    let (zero, number) = (a.local(), a.local());
    a.bne(number);
    a.lda(zp(to.sign()));
    for i in 1..4 {
        a.ora(zp(to.m(i)));
    }
    a.beq(zero);
    a.bind(number);
    a.lda(zp(to.sign()));
    a.and(imm(0x80));
    a.sta(zp(to.sign()));
    a.sty(zp(to.m(4)));
    a.sty(zp(to.over()));
    a.rts();
    a.bind(zero);
    a.sta(zp(to.m(0)));
    a.sta(zp(to.sign()));
    a.sta(zp(to.m(4)));
    a.sta(zp(to.over()));
    a.rts();
}

/// FAC rounded and packed at `to`.
fn pack(a: &mut Asm, name: &'static str, to: u16) {
    a.here(name);
    a.jsr("fac_round");
    a.ldy(imm(0));
    a.lda(zp(FAC.m(0)));
    let zero = a.local();
    a.beq(zero);
    a.lda(zp(FAC.exp()));
    a.sta(zp(to).ind_y());
    a.iny();
    a.lda(zp(FAC.m(0)));
    a.and(imm(0x7F));
    a.ora(zp(FAC.sign()));
    a.sta(zp(to).ind_y());
    for i in 1..4 {
        a.iny();
        a.lda(zp(FAC.m(i)));
        a.sta(zp(to).ind_y());
    }
    a.rts();
    a.bind(zero);
    let clear = a.here_local();
    a.sta(zp(to).ind_y());
    a.iny();
    a.cpy(imm(PACKED));
    a.bne(clear);
    a.rts();
}

fn packing(a: &mut Asm) {
    unpack(a, "fac_load", z::VAR, FAC);
    unpack(a, "fac2_load", z::VAR, FAC2);
    pack(a, "fac_store", z::VAR);

    // An operation's result normalised, then rounded to thirty-two bits.
    a.here("fac_finish");
    a.jsr("fac_normalise");

    // To nearest, a tie to the odd neighbour; the rounding byte spent.
    a.here("fac_round");
    a.lda(zp(FAC.m(4)));
    let done = a.local();
    a.bpl(done);
    a.cmp(imm(0x80));
    let tie = a.local();
    a.beq(tie);
    for i in (1..4).rev() {
        a.inc(zp(FAC.m(i)));
        a.bne(done);
    }
    a.inc(zp(FAC.m(0)));
    a.bne(done);
    a.sec();
    a.ror(zp(FAC.m(0)));
    a.inc(zp(FAC.exp()));
    a.bne(done);
    a.jmp("err_too_big");
    a.bind(tie);
    a.lda(zp(FAC.m(3)));
    a.ora(imm(1));
    a.sta(zp(FAC.m(3)));
    a.bind(done);
    a.lda(imm(0));
    a.sta(zp(FAC.m(4)));
    a.rts();
}

fn integers(a: &mut Asm) {
    // FAC = IAC, unsigned or signed; normalising keeps the sign.
    a.here("fac_from_unsigned");
    a.lda(imm(0));
    let signed = a.local();
    a.beq(signed);
    a.here("fac_from_int");
    a.lda(zp(z::IAC + 3));
    a.and(imm(0x80));
    a.pha();
    let positive = a.local();
    a.bpl(positive);
    a.jsr("int_negate");
    a.bind(positive);
    a.pla();
    a.bind(signed);
    a.sta(zp(FAC.sign()));
    a.lda(imm(0));
    a.sta(zp(FAC.m(4)));
    a.sta(zp(FAC.over()));
    for i in 0..4 {
        a.lda(zp(z::IAC + 3 - i));
        a.sta(zp(FAC.m(i)));
    }
    a.lda(imm(INTEGER_EXP));
    a.sta(zp(FAC.exp()));
    a.jmp("fac_normalise");
}

fn to_integer(a: &mut Asm) {
    // IAC = FAC, toward zero; Too big from 2^31 either way. FRACTION is
    // nonzero if bits below the point were dropped.
    a.here("int_from_fac");
    a.lda(zp(FAC.m(4)));
    a.sta(zp(FRACTION));
    a.lda(imm(INTEGER_EXP));
    a.sec();
    a.sbc(zp(FAC.exp()));
    let (fits, too_big, shifting) = (a.local(), a.local(), a.local());
    a.beq(too_big);
    a.bcs(fits);
    a.bind(too_big);
    a.jmp("err_too_big");
    a.bind(fits);
    a.cmp(imm(32));
    a.bcc(shifting);
    a.lda(zp(FAC.m(0)));
    a.sta(zp(FRACTION));
    a.jmp("int_zero");
    a.bind(shifting);
    a.tay();
    for i in 0..4 {
        a.ldx(zp(FAC.m(i)));
        a.stx(zp(z::IAC + 3 - i));
    }
    let (bytes, bits, signed) = (a.here_local(), a.local(), a.local());
    a.cpy(imm(8));
    a.bcc(bits);
    a.lda(zp(z::IAC));
    a.ora(zp(FRACTION));
    a.sta(zp(FRACTION));
    for i in 0..3 {
        a.ldx(zp(z::IAC + i + 1));
        a.stx(zp(z::IAC + i));
    }
    a.ldx(imm(0));
    a.stx(zp(z::IAC + 3));
    a.tya();
    a.sbc(imm(8));
    a.tay();
    a.bne(bytes);
    a.bind(bits);
    a.tya();
    a.beq(signed);
    let shift = a.here_local();
    a.lsr(zp(z::IAC + 3));
    a.ror(zp(z::IAC + 2));
    a.ror(zp(z::IAC + 1));
    a.ror(zp(z::IAC));
    // A bit dropped marks FRACTION with Y, the shifts left, never 0 here; a
    // count of the bits could wrap to 0.
    let kept = a.local();
    a.bcc(kept);
    a.sty(zp(FRACTION));
    a.bind(kept);
    a.dey();
    a.bne(shift);
    a.bind(signed);
    a.lda(zp(FAC.sign()));
    a.bpl(RTS);
    a.fall_into("int_negate");

    // IAC = -IAC; and the integer at X negated.
    a.here("int_negate");
    a.ldx(imm(lo(z::IAC)));
    a.here("int_negate_x");
    a.ldy(imm(4));
    a.sec();
    let byte = a.here_local();
    a.lda(imm(0));
    a.sbc(zp(0).x());
    a.sta(zp(0).x());
    a.inx();
    a.dey();
    a.bne(byte);
    a.rts();

    a.here("int_zero");
    a.lda(imm(0));
    for i in 0..4 {
        a.sta(zp(z::IAC + i));
    }
    a.rts();
}

fn normalising(a: &mut Asm) {
    // The mantissa shifted up to its top bit, the exponent with it, and the
    // exponent then checked: below 0 is zero, over 255 Too big. An
    // exponent of 0 stays, though a real packed with one reads as zero.
    a.here("fac_normalise");
    let (check, bits) = (a.local(), a.local());
    let bytes = a.here_local();
    a.lda(zp(FAC.m(0)));
    a.bmi(check);
    a.bne(bits);
    for i in 1..5 {
        a.ora(zp(FAC.m(i)));
    }
    let zero = a.local();
    a.beq(zero);
    for i in 0..4 {
        a.ldx(zp(FAC.m(i + 1)));
        a.stx(zp(FAC.m(i)));
    }
    a.ldx(imm(0));
    a.stx(zp(FAC.m(4)));
    a.lda(zp(FAC.exp()));
    a.sec();
    a.sbc(imm(8));
    a.sta(zp(FAC.exp()));
    a.bcs(bytes);
    a.dec(zp(FAC.over()));
    a.jmp(bytes);
    a.bind(bits);
    let shift = a.here_local();
    decrement_exp(a);
    a.asl(zp(FAC.m(4)));
    for i in (0..4).rev() {
        a.rol(zp(FAC.m(i)));
    }
    a.bpl(shift);
    a.bind(check);
    a.lda(zp(FAC.over()));
    a.bmi(zero);
    a.beq(RTS);
    a.jmp("err_too_big");
    a.bind(zero);
    a.jmp("fac_zero");
}

fn adding(a: &mut Asm) {
    // FAC = FAC2 - FAC, and FAC2 + FAC, rounded.
    a.here("fac_subtract");
    a.jsr("fac_negate");
    a.here("fac_add");
    a.jsr("fac_sum");
    a.jmp("fac_round");

    // FAC = FAC2 + FAC in forty bits: the smaller shifted to the larger's
    // exponent, what falls below them lost.
    a.here("fac_sum");
    let (done, to_fac2) = (a.local(), a.local());
    a.lda(zp(FAC2.m(0)));
    a.beq(done);
    a.lda(zp(FAC.m(0)));
    a.beq(to_fac2);
    a.lda(zp(FAC.exp()));
    a.sec();
    a.sbc(zp(FAC2.exp()));
    let (aligned, larger, shift_by) = (a.local(), a.local(), a.local());
    a.beq(aligned);
    a.ldx(imm(FAC2_FROM_FAC));
    a.bcs(larger);
    // FAC is the smaller: it moves to FAC2's exponent.
    a.eor(imm(0xFF));
    a.adc(imm(1));
    a.cmp(imm(40));
    let near = a.local();
    a.bcc(near);
    a.bind(to_fac2);
    a.jmp("fac_from_fac2");
    a.bind(done);
    a.rts();
    a.bind(near);
    a.ldy(zp(FAC2.exp()));
    a.sty(zp(FAC.exp()));
    a.ldx(imm(0));
    a.beq(shift_by);
    a.bind(larger);
    a.cmp(imm(40));
    a.bcs(done);
    // The accumulator X bytes from FAC shifted down A places.
    a.bind(shift_by);
    let bytes = a.here_local();
    let bits = a.local();
    a.cmp(imm(8));
    a.bcc(bits);
    for i in (0..4).rev() {
        a.ldy(zp(FAC.m(i)).x());
        a.sty(zp(FAC.m(i + 1)).x());
    }
    a.ldy(imm(0));
    a.sty(zp(FAC.m(0)).x());
    a.sbc(imm(8));
    a.bne(bytes);
    a.beq(aligned);
    a.bind(bits);
    a.tay();
    let shift = a.here_local();
    a.lsr(zp(FAC.m(0)).x());
    for i in 1..5 {
        a.ror(zp(FAC.m(i)).x());
    }
    a.dey();
    a.bne(shift);
    a.bind(aligned);
    a.lda(zp(FAC.sign()));
    a.eor(zp(FAC2.sign()));
    let difference = a.local();
    a.bmi(difference);
    a.clc();
    a.jsr("fac_add_carry");
    a.jmp("fac_normalise");
    a.bind(difference);
    a.sec();
    for i in (0..5).rev() {
        a.lda(zp(FAC.m(i)));
        a.sbc(zp(FAC2.m(i)));
        a.sta(zp(FAC.m(i)));
    }
    let normalise = a.local();
    a.bcs(normalise);
    a.ldx(imm(4));
    a.sec();
    let negate = a.here_local();
    a.lda(imm(0));
    a.sbc(zp(FAC.m(0)).x());
    a.sta(zp(FAC.m(0)).x());
    a.dex();
    a.bpl(negate);
    a.jsr("fac_negate");
    a.bind(normalise);
    a.jmp("fac_normalise");

    // FAC's forty bits plus FAC2's and C; a carry out shifts them down a
    // place, the exponent up one.
    a.here("fac_add_carry");
    for i in (0..5).rev() {
        a.lda(zp(FAC.m(i)));
        a.adc(zp(FAC2.m(i)));
        a.sta(zp(FAC.m(i)));
    }
    a.bcc(RTS);
    for i in 0..5 {
        a.ror(zp(FAC.m(i)));
    }
    a.inc(zp(FAC.exp()));
    a.bne(RTS);
    a.inc(zp(FAC.over()));
    a.rts();
}

/// The product's or quotient's sign.
fn result_sign(a: &mut Asm) {
    a.lda(zp(FAC.sign()));
    a.eor(zp(FAC2.sign()));
    a.sta(zp(FAC.sign()));
}

/// `zero` is the product's or quotient's exit when it is zero.
fn multiplying(a: &mut Asm, zero: Label) {
    // FAC = FAC2 × FAC: for each multiplier bit from the top, the
    // multiplicand's forty bits shift down one and are added if it is set.
    a.here("fac_multiply");
    a.lda(zp(FAC.m(0)));
    a.beq(RTS);
    a.lda(zp(FAC2.m(0)));
    a.beq(zero);
    result_sign(a);
    // The exponents' sum less &80, in sixteen bits.
    a.lda(zp(FAC.exp()));
    a.clc();
    a.adc(zp(FAC2.exp()));
    a.sta(zp(FAC.exp()));
    a.lda(imm(0));
    a.rol(Acc);
    a.sta(zp(FAC.over()));
    a.lda(zp(FAC.exp()));
    a.sec();
    a.sbc(imm(0x80));
    a.sta(zp(FAC.exp()));
    let low = a.local();
    a.bcs(low);
    a.dec(zp(FAC.over()));
    a.bind(low);
    a.ldx(imm(3));
    let take = a.here_local();
    a.lda(zp(FAC.m(0)).x());
    a.sta(zp(WORK).x());
    a.lda(imm(0));
    a.sta(zp(FAC.m(0)).x());
    a.dex();
    a.bpl(take);
    a.sta(zp(FAC.m(4)));
    a.ldx(imm(0));
    let byte = a.here_local();
    a.ldy(imm(8));
    let bit = a.here_local();
    shift_right(a, FAC2);
    a.asl(zp(WORK).x());
    let skip = a.local();
    a.bcc(skip);
    add40(a, FAC, FAC2.m(0));
    a.bind(skip);
    a.dey();
    a.bne(bit);
    a.inx();
    a.cpx(imm(4));
    a.bne(byte);
    a.jmp("fac_finish");
}

fn dividing(a: &mut Asm, zero: Label) {
    // FAC = FAC2 / FAC: the quotient's bits from one down to 2^-38 into the
    // forty-bit mantissa's top thirty-nine, the last bit clear, as BASIC II
    // divides.
    a.here("fac_divide");
    a.lda(zp(FAC.m(0)));
    let divisor = a.local();
    a.bne(divisor);
    a.jmp("err_division_by_zero");
    a.bind(divisor);
    a.lda(zp(FAC2.m(0)));
    let both = a.local();
    a.bne(both);
    a.bind(zero);
    a.jmp("fac_zero");
    a.bind(both);
    result_sign(a);
    // The exponents' difference plus &81, in sixteen bits.
    a.lda(zp(FAC2.exp()));
    a.sec();
    a.sbc(zp(FAC.exp()));
    a.sta(zp(FAC.exp()));
    a.lda(imm(0));
    a.sbc(imm(0));
    a.sta(zp(FAC.over()));
    a.lda(zp(FAC.exp()));
    a.clc();
    a.adc(imm(0x81));
    a.sta(zp(FAC.exp()));
    let low = a.local();
    a.bcc(low);
    a.inc(zp(FAC.over()));
    a.bind(low);
    // Both operands are rounded, so their fifth bytes are clear and the
    // remainder's stays so: four bytes and the bit shifted out above them.
    a.ldx(imm(3));
    let take = a.here_local();
    a.lda(zp(FAC.m(0)).x());
    a.sta(zp(WORK).x());
    a.dex();
    a.bpl(take);
    a.ldx(imm(QUOTIENT_BITS));
    a.clc();
    let step = a.here_local();
    let (subtract, decided, shift) = (a.local(), a.local(), a.local());
    a.bcs(subtract);
    for i in 0..4 {
        a.lda(zp(FAC2.m(i)));
        a.cmp(zp(WORK + i));
        if i < 3 {
            a.bne(decided);
        }
    }
    a.bind(decided);
    a.bcc(shift);
    a.bind(subtract);
    a.sec();
    for i in (0..4).rev() {
        a.lda(zp(FAC2.m(i)));
        a.sbc(zp(WORK + i));
        a.sta(zp(FAC2.m(i)));
    }
    a.sec();
    a.bind(shift);
    rotate_left(a, FAC);
    a.asl(zp(FAC2.m(3)));
    for i in (0..3).rev() {
        a.rol(zp(FAC2.m(i)));
    }
    a.dex();
    a.bne(step);
    a.clc();
    rotate_left(a, FAC);
    a.jmp("fac_finish");
}

fn comparing(a: &mut Asm) {
    // A = &FF, 0 or 1 as FAC2 is below, at or above FAC; their rounding
    // bytes play no part.
    a.here("fac_compare");
    let (less, greater, equal, magnitude) = (a.local(), a.local(), a.local(), a.local());
    a.lda(zp(FAC2.m(0)));
    let left = a.local();
    a.bne(left);
    a.lda(zp(FAC.m(0)));
    a.beq(equal);
    a.lda(zp(FAC.sign()));
    a.bmi(greater);
    a.bpl(less);
    // FAC2's sign decides against a zero FAC or a sign of the other kind.
    a.bind(left);
    a.lda(zp(FAC.m(0)));
    let by_left = a.local();
    a.beq(by_left);
    a.lda(zp(FAC2.sign()));
    a.eor(zp(FAC.sign()));
    a.bpl(magnitude);
    a.bind(by_left);
    a.lda(zp(FAC2.sign()));
    a.bmi(less);
    a.bpl(greater);
    a.bind(magnitude);
    a.ldx(imm(0));
    let byte = a.here_local();
    let differ = a.local();
    a.lda(zp(FAC2.exp()).x());
    a.cmp(zp(FAC.exp()).x());
    a.bne(differ);
    a.inx();
    a.cpx(imm(5));
    a.bne(byte);
    a.bind(equal);
    a.lda(imm(0));
    a.rts();
    a.bind(differ);
    a.ror(Acc);
    a.eor(zp(FAC2.sign()));
    a.bmi(greater);
    a.bind(less);
    a.lda(imm(0xFF));
    a.rts();
    a.bind(greater);
    a.lda(imm(1));
    a.rts();
}

/// FAC2's mantissa = FAC's moved down `bytes` bytes; C the top bit of the
/// highest byte dropped, so the sum that follows rounds the shifted copy.
fn shifted_bytes(a: &mut Asm, bytes: u16) {
    for i in (0..5).rev() {
        if i >= bytes {
            a.lda(zp(FAC.m(i - bytes)));
        } else if i + 1 == bytes {
            a.lda(imm(0));
        }
        a.sta(zp(FAC2.m(i)));
    }
    a.lda(zp(FAC.m(5 - bytes)));
    a.asl(Acc);
}

/// FAC2's mantissa = FAC's shifted down `bits` bits; C the last bit out.
fn shifted_bits(a: &mut Asm, bits: u8) {
    for i in 0..5 {
        a.lda(zp(FAC.m(i)));
        a.sta(zp(FAC2.m(i)));
    }
    for _ in 0..bits {
        shift_right(a, FAC2);
    }
}

fn tens(a: &mut Asm) {
    // The decimal scaling, in forty bits: FAC × 10 as eight times itself
    // and a quarter more, the quarter rounded.
    a.here("fac_times_ten");
    a.lda(zp(FAC.m(0)));
    a.beq(RTS);
    a.lda(zp(FAC.exp()));
    a.clc();
    a.adc(imm(3));
    a.sta(zp(FAC.exp()));
    let low = a.local();
    a.bcc(low);
    a.inc(zp(FAC.over()));
    a.bind(low);
    shifted_bits(a, 2);
    a.jmp("fac_add_carry");

    // FAC / 10 as FAC × 3/32 × 17/16 × 257/256 × 65537/65536 × (1 + 2^-32),
    // each shifted copy rounded.
    a.here("fac_divide_ten");
    a.lda(zp(FAC.m(0)));
    a.beq(RTS);
    a.lda(zp(FAC.exp()));
    a.sec();
    a.sbc(imm(4));
    a.sta(zp(FAC.exp()));
    let low = a.local();
    a.bcs(low);
    a.dec(zp(FAC.over()));
    a.bind(low);
    for bits in [1, 4] {
        shifted_bits(a, bits);
        a.jsr("fac_add_carry");
    }
    for bytes in [1, 2] {
        shifted_bytes(a, bytes);
        a.jsr("fac_add_carry");
    }
    shifted_bytes(a, 4);
    a.jmp("fac_add_carry");
}
