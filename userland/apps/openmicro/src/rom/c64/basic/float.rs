//! FAC and ARG: loading and storing reals, rounding, normalising, the sum,
//! product and quotient to the original's bits (`plans/C64.md` §5's pinned
//! behaviour), the sign, comparison, INT and the integer conversions.

use super::errors::{number, raise};
use super::ws::zp;
use crate::rom::asm::{abs, imm, zp as z, Acc, Asm, Label, RTS};

/// FAC's mantissa, most significant byte first.
pub const M1: u16 = zp::FACHO;
pub const M2: u16 = zp::FACHO + 1;
pub const M3: u16 = zp::FACHO + 2;
pub const M4: u16 = zp::FACLO;
/// ARG's mantissa.
const A1: u16 = zp::ARGHO;
const A2: u16 = zp::ARGHO + 1;
const A3: u16 = zp::ARGHO + 2;
const A4: u16 = zp::ARGLO;

/// The byte an ARG being aligned shifts into, added beneath FAC's
/// mantissa; while a product forms, whether its previous multiplier byte
/// was zero.
const EXTENSION: u16 = zp::BITS;
const PREVIOUS_ZERO: u16 = zp::BITS;
/// FAC's own byte beneath its mantissa while an ARG's alignment borrows
/// it; while a product or quotient forms, its exponent's ninth bit, signed.
const OLD_OVERFLOW: u16 = zp::JMPER + 2;
const EXPONENT_HIGH: u16 = zp::JMPER + 2;
/// The quotient's steps left, and whether the last step's remainder ran
/// past 32 bits. JMPER's first byte, its `JMP`, is never scratch.
const STEPS: u16 = zp::BITS;
const OVERRUN: u16 = zp::JMPER + 1;

/// The exponent of a real whose integer part fills 32 bits.
pub const LONG_EXPONENT: u8 = 0xA0;
/// The exponent of a real whose integer part fills 16 bits.
pub const WORD_EXPONENT: u8 = 0x90;
/// The exponent from which a real has no fraction bits in 40.
const WHOLE_EXPONENT: u8 = 0xA8;

pub fn emit(a: &mut Asm) {
    a.movable(loads);
    a.movable(stores);
    a.movable(round);
    a.movable(normalise);
    a.movable(sum);
    a.movable(shifts);
    a.movable(product);
    a.movable(quotient);
    a.movable(exponents);
    a.movable(signs);
    a.movable(compare);
    a.movable(int);
    a.movable(integers);
}

/// `fac_load` and `arg_load`: the real at A (low) and Y into FAC or ARG,
/// through `load_real` into the exponent at zero page X, its mantissa
/// and sign after it. `arg_load` and `signs_differ` leave whether the
/// signs differ in ARISGN and FACEXP in A.
fn loads(a: &mut Asm) {
    a.here("fac_load");
    a.ldx(imm(zp_index(zp::FACEXP)));
    a.jsr("load_real");
    a.sty(z(zp::FACOV));
    a.rts();

    a.here("arg_load");
    a.ldx(imm(zp_index(zp::ARGEXP)));
    a.jsr("load_real");
    a.fall_into("signs_differ");
    a.here("signs_differ");
    a.lda(z(zp::FACSGN));
    a.eor(z(zp::ARGSGN));
    a.sta(z(zp::ARISGN));
    a.lda(z(zp::FACEXP));
    a.rts();

    // Y is left zero.
    a.here("load_real");
    a.sta(z(zp::INDEX));
    a.sty(z(zp::INDEX + 1));
    a.ldy(imm(4));
    for byte in [4, 3, 2] {
        a.lda(z(zp::INDEX).ind_y());
        a.sta(z(byte).x());
        a.dey();
    }
    a.lda(z(zp::INDEX).ind_y());
    a.sta(z(SIGN_OFFSET).x());
    a.ora(imm(0x80));
    a.sta(z(1).x());
    a.dey();
    a.lda(z(zp::INDEX).ind_y());
    a.sta(z(0).x());
    a.rts();
}

/// The sign's place from the exponent's, in FAC and ARG alike.
const SIGN_OFFSET: u16 = zp::FACSGN - zp::FACEXP;
const _: () = assert!(zp::ARGSGN - zp::ARGEXP == SIGN_OFFSET);

/// `fac_store`: FAC rounded and packed at X (low) and Y. `fac_to_arg`:
/// FAC rounded and copied to ARG. `arg_to_fac`: ARG copied to FAC.
fn stores(a: &mut Asm) {
    a.here("fac_store");
    a.stx(z(zp::INDEX));
    a.sty(z(zp::INDEX + 1));
    a.jsr("round");
    a.ldy(imm(4));
    let low_bytes = a.here_local();
    a.lda(abs(zp::FACEXP).y());
    a.sta(z(zp::INDEX).ind_y());
    a.dey();
    a.cpy(imm(2));
    a.bcs(low_bytes);
    a.lda(z(zp::FACSGN));
    a.ora(imm(0x7F));
    a.and(z(M1));
    a.sta(z(zp::INDEX).ind_y());
    a.dey();
    a.lda(z(zp::FACEXP));
    a.sta(z(zp::INDEX).ind_y());
    a.rts();

    a.here("fac_to_arg");
    a.jsr("round");
    a.ldx(imm(6));
    let copy = a.here_local();
    a.lda(z(zp::FACEXP - 1).x());
    a.sta(z(zp::ARGEXP - 1).x());
    a.dex();
    a.bne(copy);
    a.stx(z(zp::ARISGN));
    a.rts();

    a.here("arg_to_fac");
    a.ldx(imm(6));
    let back = a.here_local();
    a.lda(z(zp::ARGEXP - 1).x());
    a.sta(z(zp::FACEXP - 1).x());
    a.dex();
    a.bne(back);
    a.stx(z(zp::FACOV));
    a.rts();
}

/// `round`: FAC's mantissa rounded half up at its byte beneath, which is
/// then clear. `mantissa_up`: the mantissa one more, a carry out of it
/// renormalising.
fn round(a: &mut Asm) {
    a.here("round");
    let clear = a.local();
    a.lda(z(zp::FACEXP));
    a.beq(clear);
    a.lda(z(zp::FACOV));
    a.bpl(clear);
    a.jsr("mantissa_up");
    a.bind(clear);
    a.lda(imm(0));
    a.sta(z(zp::FACOV));
    a.rts();

    a.here("mantissa_up");
    a.inc(z(M4));
    a.bne(RTS);
    a.inc(z(M3));
    a.bne(RTS);
    a.inc(z(M2));
    a.bne(RTS);
    a.inc(z(M1));
    a.bne(RTS);
    a.lda(imm(0x80));
    a.sta(z(M1));
    a.fall_into("exponent_up");

    // The exponent one more, past 255 an overflow.
    a.here("exponent_up");
    a.inc(z(zp::FACEXP));
    a.bne(RTS);
    a.jmp("overflow_error");
}

/// `normalise`: FAC's 40 bits shifted up until the top one is set; a
/// result with no bit above the byte beneath is zero, as is one whose
/// exponent runs out. `normalise_40`: the same, the byte beneath's bits
/// counted. `fac_zero`: FAC made zero.
fn normalise(a: &mut Asm) {
    a.here("normalise");
    let bytes = a.local();
    let bits = a.local();
    let shift = a.local();
    a.lda(z(M1));
    a.ora(z(M2));
    a.ora(z(M3));
    a.ora(z(M4));
    a.beq("fac_zero");
    a.here("normalise_40");
    a.bind(bytes);
    a.lda(z(M1));
    a.bne(bits);
    a.lda(z(M2));
    a.sta(z(M1));
    a.lda(z(M3));
    a.sta(z(M2));
    a.lda(z(M4));
    a.sta(z(M3));
    a.lda(z(zp::FACOV));
    a.sta(z(M4));
    a.lda(imm(0));
    a.sta(z(zp::FACOV));
    a.lda(z(zp::FACEXP));
    a.sec();
    a.sbc(imm(8));
    a.sta(z(zp::FACEXP));
    a.beq("fac_zero");
    a.bcs(bytes);
    a.bcc("fac_zero");
    a.bind(bits);
    a.bmi(RTS);
    a.bind(shift);
    a.dec(z(zp::FACEXP));
    a.beq("fac_zero");
    a.jsr("shift_left");
    a.bpl(shift);
    a.rts();

    // FAC's 40 bits shifted left once, N their new top bit.
    a.here("shift_left");
    a.asl(z(zp::FACOV));
    a.rol(z(M4));
    a.rol(z(M3));
    a.rol(z(M2));
    a.rol(z(M1));
    a.rts();

    a.here("fac_zero");
    a.lda(imm(0));
    a.sta(z(zp::FACEXP));
    a.sta(z(zp::FACSGN));
    a.rts();
}

/// `fadd`: FAC = ARG + FAC; `fsub`: FAC = ARG − FAC; `fadd_mem` takes ARG
/// from A (low) and Y. The smaller operand's 40 bits shift right; an ARG
/// shifted fills the byte beneath FAC's mantissa, which is added with FAC's
/// own; operands of one exponent carry one more into that byte when added.
/// `carry_in`: C, set, shifted in above the 40 bits, the exponent one more.
fn sum(a: &mut Asm) {
    let both = a.local();
    let differ = a.local();
    let arg_smaller = a.local();
    let combine = a.local();
    let subtract = a.local();

    a.here("fsub");
    a.lda(z(zp::FACSGN));
    a.eor(imm(0xFF));
    a.sta(z(zp::FACSGN));
    a.jsr("signs_differ");
    a.jmp("fadd");

    a.here("fadd_mem");
    a.jsr("arg_load");
    a.here("fadd");
    a.lda(z(zp::ARGEXP));
    a.beq(RTS);
    a.lda(z(zp::FACEXP));
    a.bne(both);
    a.jmp("arg_to_fac");

    a.bind(both);
    a.sec();
    a.sbc(z(zp::ARGEXP));
    a.bne(differ);
    a.sta(z(EXTENSION));
    a.beq(combine);
    a.bind(differ);
    a.bcs(arg_smaller);
    // FAC smaller: its 40 bits move down to ARG's exponent.
    a.eor(imm(0xFF));
    a.adc(imm(1));
    a.ldx(z(zp::ARGEXP));
    a.stx(z(zp::FACEXP));
    a.ldx(imm(zp_index(M1)));
    a.jsr("shift_right");
    a.lda(imm(0));
    a.sta(z(EXTENSION));
    a.clc();
    a.bcc(combine);

    a.bind(arg_smaller);
    a.ldx(z(zp::FACOV));
    a.stx(z(OLD_OVERFLOW));
    a.ldx(imm(0));
    a.stx(z(zp::FACOV));
    a.ldx(imm(zp_index(A1)));
    a.jsr("shift_right");
    a.lda(z(zp::FACOV));
    a.sta(z(EXTENSION));
    a.lda(z(OLD_OVERFLOW));
    a.sta(z(zp::FACOV));
    a.clc();

    a.bind(combine);
    a.bit(z(zp::ARISGN));
    a.bmi(subtract);
    a.lda(z(zp::FACOV));
    a.adc(z(EXTENSION));
    a.sta(z(zp::FACOV));
    a.ldx(imm(3));
    let add = a.here_local();
    a.lda(z(M1).x());
    a.adc(z(A1).x());
    a.sta(z(M1).x());
    a.dex();
    a.bpl(add);
    a.bcc(RTS);
    a.here("carry_in");
    a.jsr("exponent_up");
    a.ror(z(M1));
    a.ror(z(M2));
    a.ror(z(M3));
    a.ror(z(M4));
    a.ror(z(zp::FACOV));
    a.rts();
    difference(a, subtract);
}

/// `fadd` run on at `subtract`: the mantissas' difference, the sign ARG's
/// where it went below zero, normalised. `negate_mantissa`: FAC's 40 bits
/// negated.
fn difference(a: &mut Asm, subtract: Label) {
    a.bind(subtract);
    let positive = a.local();
    a.sec();
    a.lda(z(zp::FACOV));
    a.sbc(z(EXTENSION));
    a.sta(z(zp::FACOV));
    a.ldx(imm(3));
    let take = a.here_local();
    a.lda(z(M1).x());
    a.sbc(z(A1).x());
    a.sta(z(M1).x());
    a.dex();
    a.bpl(take);
    a.bcs(positive);
    a.lda(z(zp::ARGSGN));
    a.sta(z(zp::FACSGN));
    a.jsr("negate_mantissa");
    a.bind(positive);
    a.jmp("normalise");

    a.here("negate_mantissa");
    a.sec();
    a.lda(imm(0));
    a.sbc(z(zp::FACOV));
    a.sta(z(zp::FACOV));
    a.ldx(imm(3));
    let negate = a.here_local();
    a.lda(imm(0));
    a.sbc(z(M1).x());
    a.sta(z(M1).x());
    a.dex();
    a.bpl(negate);
    a.rts();
}

/// A zero-page address as the byte X indexes it from.
const fn zp_index(address: u16) -> u8 {
    address.to_le_bytes()[0]
}

/// `shift_right`: the four bytes from zero page X, then FACOV, shifted
/// right A places, the bits past FACOV lost. `shift_mantissa`: FAC's 40
/// bits shifted right as far as exponent A is above FACEXP.
fn shifts(a: &mut Asm) {
    a.here("shift_mantissa");
    a.sec();
    a.sbc(z(zp::FACEXP));
    a.ldx(imm(zp_index(M1)));

    a.here("shift_right");
    let bytes = a.local();
    let remainder = a.local();
    let bit = a.local();
    a.cmp(imm(40));
    a.bcc(bytes);
    a.lda(imm(0));
    for offset in 0..4 {
        a.sta(z(offset).x());
    }
    a.sta(z(zp::FACOV));
    a.rts();
    a.bind(bytes);
    a.cmp(imm(8));
    a.bcc(remainder);
    a.ldy(z(3).x());
    a.sty(z(zp::FACOV));
    a.ldy(z(2).x());
    a.sty(z(3).x());
    a.ldy(z(1).x());
    a.sty(z(2).x());
    a.ldy(z(0).x());
    a.sty(z(1).x());
    a.ldy(imm(0));
    a.sty(z(0).x());
    a.sbc(imm(8));
    a.bcs(bytes);
    a.bind(remainder);
    a.tay();
    a.beq(RTS);
    a.bind(bit);
    a.lsr(z(0).x());
    a.ror(z(1).x());
    a.ror(z(2).x());
    a.ror(z(3).x());
    a.ror(z(zp::FACOV));
    a.dey();
    a.bne(bit);
    a.rts();
}

/// `fmul`: FAC = ARG × FAC; `fmul_mem` takes ARG from A (low) and Y. The
/// multiplier is FAC's five bytes from the byte beneath up, each bit
/// adding ARG to the top 32 bits of a 40-bit product that then shifts
/// right; a zero byte shifts it eight places, nine after a zero byte.
fn product(a: &mut Asm) {
    a.here("fmul_mem");
    a.jsr("arg_load");
    a.here("fmul");
    a.lda(z(zp::FACEXP));
    a.beq(RTS);
    a.jsr("product_setup");
    a.lda(z(zp::ARGEXP));
    a.jsr("exponent_sum");
    a.lda(imm(0));
    a.sta(z(PREVIOUS_ZERO));
    for byte in [zp::FACOV, M4, M3, M2, M1] {
        a.lda(z(byte));
        a.jsr("multiply_byte");
    }
    a.jmp("result_from_resho");

    // The product stepped by multiplier byte A.
    a.here("multiply_byte");
    let bits = a.local();
    let mark = a.local();
    let step = a.local();
    let no_add = a.local();
    a.bne(bits);
    a.ldx(imm(3));
    let down = a.here_local();
    a.lda(z(zp::RESHO).x());
    a.sta(z(zp::RESHO + 1).x());
    a.dex();
    a.bpl(down);
    a.lda(imm(0));
    a.sta(z(zp::RESHO));
    a.lda(z(PREVIOUS_ZERO));
    a.beq(mark);
    a.lsr(z(zp::RESHO));
    for offset in 1..5 {
        a.ror(z(zp::RESHO + offset));
    }
    a.bind(mark);
    a.lda(imm(1));
    a.sta(z(PREVIOUS_ZERO));
    a.rts();
    a.bind(bits);
    a.ldy(imm(0));
    a.sty(z(PREVIOUS_ZERO));
    a.lsr(Acc);
    a.ora(imm(0x80));
    a.bind(step);
    a.tay();
    a.bcc(no_add);
    a.clc();
    for (product, arg) in [(3, A4), (2, A3), (1, A2), (0, A1)] {
        a.lda(z(zp::RESHO + product));
        a.adc(z(arg));
        a.sta(z(zp::RESHO + product));
    }
    a.bind(no_add);
    a.ror(z(zp::RESHO));
    for offset in 1..5 {
        a.ror(z(zp::RESHO + offset));
    }
    a.tya();
    a.lsr(Acc);
    a.bne(step);
    a.rts();
}

/// `fdiv`: FAC = ARG ÷ FAC; `fdiv_mem` takes ARG from A (low) and Y. The
/// divisor is FAC rounded to 32 bits; the quotient is 34 bits from its
/// integer bit, beneath them zeros.
fn quotient(a: &mut Asm) {
    a.here("fdiv_mem");
    a.jsr("arg_load");
    a.here("fdiv");
    let divisor = a.local();
    let exponent_set = a.local();
    let step = a.local();
    let take = a.local();
    let shift = a.local();
    let align = a.local();
    a.jsr("round");
    a.lda(z(zp::FACEXP));
    a.bne(divisor);
    raise(a, number::DIVISION_BY_ZERO);
    a.bind(divisor);
    a.jsr("product_setup");
    // FACEXP = ARGEXP + 129 − FACEXP, its ninth bit signed.
    a.lda(z(zp::ARGEXP));
    a.clc();
    a.adc(imm(129));
    a.tax();
    a.lda(imm(0));
    a.rol(Acc);
    a.sta(z(EXPONENT_HIGH));
    a.txa();
    a.sec();
    a.sbc(z(zp::FACEXP));
    a.sta(z(zp::FACEXP));
    a.bcs(exponent_set);
    a.dec(z(EXPONENT_HIGH));
    a.bind(exponent_set);
    a.lda(imm(0));
    a.sta(z(OVERRUN));
    a.lda(imm(34));
    a.sta(z(STEPS));
    a.bind(step);
    a.sec();
    a.lda(z(A4));
    a.sbc(z(M4));
    a.pha();
    a.lda(z(A3));
    a.sbc(z(M3));
    a.pha();
    a.lda(z(A2));
    a.sbc(z(M2));
    a.tax();
    a.lda(z(A1));
    a.sbc(z(M1));
    a.tay();
    a.lda(z(OVERRUN));
    a.bne(take);
    a.bcs(take);
    a.pla();
    a.pla();
    a.clc();
    a.bcc(shift);
    a.bind(take);
    a.sty(z(A1));
    a.stx(z(A2));
    a.pla();
    a.sta(z(A3));
    a.pla();
    a.sta(z(A4));
    a.sec();
    a.bind(shift);
    for offset in [4, 3, 2, 1, 0] {
        a.rol(z(zp::RESHO + offset));
    }
    a.asl(z(A4));
    a.rol(z(A3));
    a.rol(z(A2));
    a.rol(z(A1));
    a.lda(imm(0));
    a.rol(Acc);
    a.sta(z(OVERRUN));
    a.dec(z(STEPS));
    a.bne(step);
    a.ldx(imm(6));
    a.bind(align);
    a.asl(z(zp::RESHO + 4));
    for offset in [3, 2, 1, 0] {
        a.rol(z(zp::RESHO + offset));
    }
    a.dex();
    a.bne(align);
    a.fall_into("result_from_resho");
}

/// `product_setup`: for ARG zero, FAC made zero and the caller's caller
/// returned to; else the signs compared and RESHO cleared. A product's or
/// quotient's exponent. `exponent_sum`: FACEXP made
/// A + FACEXP − 128 with its ninth bit, a sum that cannot come to an
/// exponent being zero or too large at once. `result_from_resho`: the 40
/// bits formed in RESHO into FAC, shifted up once if their top bit is
/// clear, and the exponent checked: above 255 an overflow, at or below
/// zero a zero.
fn exponents(a: &mut Asm) {
    a.here("result_from_resho");
    let shifted = a.local();
    let check = a.local();
    let low = a.local();
    let zero = a.local();
    a.lda(z(zp::ARISGN));
    a.sta(z(zp::FACSGN));
    a.ldx(imm(3));
    let copy = a.here_local();
    a.lda(z(zp::RESHO).x());
    a.sta(z(M1).x());
    a.dex();
    a.bpl(copy);
    a.lda(z(zp::RESHO + 4));
    a.sta(z(zp::FACOV));
    a.lda(z(M1));
    a.bmi(check);
    a.jsr("shift_left");
    a.lda(z(zp::FACEXP));
    a.bne(shifted);
    a.dec(z(EXPONENT_HIGH));
    a.bind(shifted);
    a.dec(z(zp::FACEXP));
    a.bind(check);
    a.lda(z(EXPONENT_HIGH));
    a.beq(low);
    a.bmi(zero);
    a.jmp("overflow_error");
    a.bind(low);
    a.lda(z(zp::FACEXP));
    a.beq(zero);
    a.rts();
    a.bind(zero);
    a.jmp("fac_zero");

    a.here("exponent_sum");
    let high = a.local();
    let store = a.local();
    let keep = a.local();
    let abandon = a.local();
    a.clc();
    a.adc(z(zp::FACEXP));
    a.bcs(high);
    a.cmp(imm(0x81));
    a.bcs(store);
    a.bind(abandon);
    a.pla();
    a.pla();
    a.jmp("fac_zero");
    a.bind(high);
    a.cmp(imm(0x81));
    a.bcc(store);
    a.jmp("overflow_error");
    a.bind(store);
    a.ldx(imm(0));
    a.eor(imm(0x80));
    a.sta(z(zp::FACEXP));
    a.bne(keep);
    a.inx();
    a.bind(keep);
    a.stx(z(EXPONENT_HIGH));
    a.rts();

    a.here("product_setup");
    let clear = a.local();
    a.lda(z(zp::ARGEXP));
    a.beq(abandon);
    a.jsr("signs_differ");
    a.lda(imm(0));
    a.ldx(imm(4));
    a.bind(clear);
    a.sta(z(zp::RESHO).x());
    a.dex();
    a.bpl(clear);
    a.rts();
}

/// `negate`: FAC's sign changed, a zero's kept. `sign`: A 0, 1 or `$FF`
/// as FAC is zero, positive or negative. `fn_sgn` and `fn_abs`: SGN and
/// ABS of FAC.
fn signs(a: &mut Asm) {
    a.here("negate");
    a.lda(z(zp::FACEXP));
    a.beq(RTS);
    a.lda(z(zp::FACSGN));
    a.eor(imm(0xFF));
    a.sta(z(zp::FACSGN));
    a.rts();

    a.here("sign");
    let positive = a.local();
    a.lda(z(zp::FACEXP));
    a.beq(RTS);
    a.lda(z(zp::FACSGN));
    a.asl(Acc);
    a.lda(imm(1));
    a.bcc(positive);
    a.lda(imm(0xFF));
    a.bind(positive);
    a.rts();

    a.here("fn_sgn");
    a.jsr("sign");
    a.jmp("signed_byte_to_fac");

    a.here("fn_abs");
    a.lsr(z(zp::FACSGN));
    a.rts();
}

/// FAC's bytes `compare_fac` keeps from its exponent up, beside FACOV.
const SAVED: u8 = 5;

/// `compare_fac`: A 0, 1 or `$FF` as FAC rounded is equal to, greater or less
/// than the real at A (low) and Y; FAC itself is left as it was. The
/// restore's index wraps in zero page onto FACEXP.
fn compare(a: &mut Asm) {
    a.here("compare_fac");
    let save = a.local();
    let restore = a.local();
    a.sta(z(zp::INDEX2));
    a.sty(z(zp::INDEX2 + 1));
    a.lda(z(zp::FACOV));
    a.pha();
    a.ldx(imm(SAVED - 1));
    a.bind(save);
    a.lda(z(zp::FACEXP).x());
    a.pha();
    a.dex();
    a.bpl(save);
    a.jsr("round");
    a.jsr("compare_rounded");
    a.tay();
    a.ldx(imm(0u8.wrapping_sub(SAVED)));
    a.bind(restore);
    a.pla();
    a.sta(z(zp::FACEXP + u16::from(SAVED)).x());
    a.inx();
    a.bne(restore);
    a.pla();
    a.sta(z(zp::FACOV));
    a.tya();
    a.rts();

    a.here("compare_rounded");
    let both = a.local();
    let same_sign = a.local();
    let mantissa = a.local();
    let differ = a.local();
    let result = a.local();
    let positive = a.local();
    a.ldy(imm(0));
    a.lda(z(zp::INDEX2).ind_y());
    a.beq("sign");
    a.iny();
    a.lda(z(zp::INDEX2).ind_y());
    a.ldx(z(zp::FACEXP));
    a.bne(both);
    a.eor(imm(0xFF));
    a.jmp(result);
    a.bind(both);
    a.eor(z(zp::FACSGN));
    a.bpl(same_sign);
    a.clc();
    a.bcc(differ);
    a.bind(same_sign);
    a.dey();
    a.lda(z(zp::INDEX2).ind_y());
    a.cmp(z(zp::FACEXP));
    a.bne(differ);
    a.iny();
    a.lda(z(zp::INDEX2).ind_y());
    a.ora(imm(0x80));
    a.cmp(z(M1));
    a.bne(differ);
    a.ldx(imm(1));
    a.bind(mantissa);
    a.iny();
    a.lda(z(zp::INDEX2).ind_y());
    a.cmp(z(M1).x());
    a.bne(differ);
    a.inx();
    a.cpx(imm(4));
    a.bne(mantissa);
    a.lda(imm(0));
    a.rts();
    // C set: the memory's magnitude is the larger.
    a.bind(differ);
    a.lda(z(zp::FACSGN));
    a.bcc(result);
    a.eor(imm(0xFF));
    // A's top bit set: FAC is the less.
    a.bind(result);
    a.asl(Acc);
    a.lda(imm(1));
    a.bcc(positive);
    a.lda(imm(0xFF));
    a.bind(positive);
    a.rts();
}

/// `fn_int`: INT of FAC, the floor of its 40 bits: their fraction shifted
/// out, below zero from one less and then one more, the ceiling of the
/// magnitude.
fn int(a: &mut Asm) {
    a.here("fn_int");
    let shift = a.local();
    let renormalise = a.local();
    a.lda(z(zp::FACEXP));
    a.beq(RTS);
    a.cmp(imm(WHOLE_EXPONENT));
    a.bcs(RTS);
    a.bit(z(zp::FACSGN));
    a.bpl(shift);
    a.jsr("negate_mantissa");
    a.jsr("increment_40");
    a.jsr("negate_mantissa");
    a.bind(shift);
    a.lda(imm(WHOLE_EXPONENT));
    a.jsr("shift_mantissa");
    a.lda(imm(WHOLE_EXPONENT));
    a.sta(z(zp::FACEXP));
    a.bit(z(zp::FACSGN));
    a.bpl(renormalise);
    a.jsr("increment_40");
    a.bind(renormalise);
    a.jmp("normalise_40");

    // FAC's 40 bits one more.
    a.here("increment_40");
    a.inc(z(zp::FACOV));
    a.bne(RTS);
    a.jmp("mantissa_up");
}

/// The conversions from reals to integers. `fac_to_integer`: FAC floored to
/// a signed 16-bit integer in M3 (high) and M4. `fac_to_word`: FAC floored
/// to an unsigned 16-bit integer in M3 and M4, copied to LINNUM, its high
/// byte in A and low in Y. Out of range is ILLEGAL QUANTITY.
fn integers(a: &mut Asm) {
    let illegal = a.local();
    let zero = a.local();

    a.here("fac_to_integer");
    let negative = a.local();
    let in_range = a.local();
    a.jsr("integer_part");
    a.beq(zero);
    a.bit(z(zp::FACSGN));
    a.bmi(negative);
    a.lda(z(M3));
    a.bmi(illegal);
    a.rts();
    a.bind(negative);
    a.lda(z(M3));
    a.cmp(imm(0x80));
    a.bcc(in_range);
    a.bne(illegal);
    a.lda(z(M4));
    a.bne(illegal);
    a.bind(in_range);
    a.sec();
    a.lda(imm(0));
    a.sbc(z(M4));
    a.sta(z(M4));
    a.lda(imm(0));
    a.sbc(z(M3));
    a.sta(z(M3));
    a.rts();

    a.here("fac_to_word");
    let unsigned = a.local();
    a.jsr("integer_part");
    a.beq(zero);
    a.bit(z(zp::FACSGN));
    a.bpl(unsigned);
    a.bind(illegal);
    a.jmp("illegal_quantity_error");
    a.bind(unsigned);
    a.lda(z(M3));
    a.sta(z(zp::LINNUM + 1));
    a.ldy(z(M4));
    a.sty(z(zp::LINNUM));
    a.rts();
    a.bind(zero);
    a.sta(z(M3));
    a.sta(z(M4));
    a.sta(z(zp::LINNUM));
    a.sta(z(zp::LINNUM + 1));
    a.tay();
    a.rts();

    // FAC floored, its magnitude below 65536 made a 32-bit integer in M1
    // to M4; Z set for zero.
    a.here("integer_part");
    let shift = a.local();
    a.jsr("fn_int");
    a.lda(z(zp::FACEXP));
    a.bne(shift);
    a.rts();
    a.bind(shift);
    a.cmp(imm(WORD_EXPONENT + 1));
    a.bcs(illegal);
    a.lda(imm(LONG_EXPONENT));
    a.jsr("shift_mantissa");
    a.lda(imm(1));
    a.rts();
}

/// `byte_to_fac`: FAC made unsigned Y; `word_to_fac`, ADRAY2's default, the
/// signed integer in A (high) and Y; `signed_byte_to_fac` signed A;
/// `magnitude_to_fac` M1 and M2 at exponent X, signed as FACSGN already
/// is. `word_to_fac` lies [`WORD_TO_FAC`] bytes in.
pub(super) fn conversions(a: &mut Asm) {
    a.here("byte_to_fac");
    a.lda(imm(0));
    a.here("word_to_fac");
    a.sta(z(M1));
    a.sty(z(M2));
    a.ldx(imm(WORD_EXPONENT));
    // FAC made the signed value in M1 and M2 at exponent X.
    a.here("signed_to_fac");
    let positive = a.local();
    a.lda(z(M1));
    a.sta(z(zp::FACSGN));
    a.here("magnitude_to_fac");
    a.stx(z(zp::FACEXP));
    a.lda(imm(0));
    a.sta(z(M3));
    a.sta(z(M4));
    a.sta(z(zp::FACOV));
    a.bit(z(zp::FACSGN));
    a.bpl(positive);
    a.jsr("negate_mantissa");
    a.bind(positive);
    a.jmp("normalise");

    a.here("signed_byte_to_fac");
    a.sta(z(M1));
    a.lda(imm(0));
    a.sta(z(M2));
    a.ldx(imm(WORD_EXPONENT - 8));
    a.bne("signed_to_fac");
}

/// Where [`conversions`]' `word_to_fac` lies from its start: past
/// `byte_to_fac`'s two bytes.
pub const WORD_TO_FAC: u16 = 2;
