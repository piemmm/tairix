//! Expressions, by precedence from OR and EOR down to a factor: AND, the
//! comparisons, `+` and `-`, `*`, `/`, DIV and MOD, then `^`.
//!
//! Every level returns the value's kind in A, its value in IAC, FAC or the
//! string buffer, and the character after it, already read, in X. An
//! operator pushes its left operand on the BASIC stack and its kind and own
//! token on the 6502 stack, so nothing the right operand runs, an FN
//! included, can disturb them.

use super::byte;
use super::float::FAC;
use super::ws::{kind, page, place, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};

const AND: u8 = 0x80;
const DIV: u8 = 0x81;
const EOR: u8 = 0x82;
const MOD: u8 = 0x83;
const OR: u8 = 0x84;

/// A comparison's outcomes that make it true: less, equal, greater.
const LESS: u8 = 1;
const EQUAL: u8 = 2;
const GREATER: u8 = 4;

/// The left operand's kind while the pair is made numbers.
const LEFT: u16 = z::INT_WORK + 3;

const CR: u8 = 0x0D;

pub fn emit(a: &mut Asm) {
    levels(a);
    operands(a);
    arithmetic(a);
    bitwise(a);
    integers(a);
    dividing(a);
    comparisons(a);
    strings(a);
    factor(a);
    factors(a);
    indirections(a);
    coercions(a);
}

/// One operator step of a level: the left operand pushed, the right
/// evaluated by `next`, then `combine` called with the left's kind in A,
/// the right's in Y and the operator in X; the step after loops to `again`.
fn step(a: &mut Asm, next: &'static str, combine: &'static str, again: &'static str) {
    a.stx(zp(z::OP));
    a.jsr("push_value");
    a.pha();
    a.lda(zp(z::OP));
    a.pha();
    a.jsr(next);
    a.stx(zp(z::OP));
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.jsr(combine);
    a.ldx(zp(z::OP));
    a.jmp(again);
}

fn levels(a: &mut Asm) {
    // The levels' return when no operator of theirs follows.
    a.rts();
    a.here("expr");
    a.jsr("level_and");
    a.here("expr_next");
    a.cpx(imm(OR));
    let op = a.local();
    a.beq(op);
    a.cpx(imm(EOR));
    a.bne(RTS);
    a.bind(op);
    step(a, "level_and", "logical", "expr_next");

    a.here("level_and");
    a.jsr("level_compare");
    a.here("and_next");
    a.cpx(imm(AND));
    a.bne(RTS);
    step(a, "level_compare", "logical", "and_next");

    // The comparisons: the outcomes that make each true travel as its
    // operator; a compound operator's characters are adjacent.
    a.here("level_compare");
    a.jsr("level_add");
    a.here("compare_next");
    let (equal, less, compare) = (a.local(), a.local(), a.local());
    a.cpx(imm(b'='));
    a.beq(equal);
    a.cpx(imm(b'<'));
    a.beq(less);
    a.cpx(imm(b'>'));
    a.bne(RTS);
    a.ldx(imm(GREATER));
    a.ldy(imm(b'='));
    a.bne(compare);
    a.bind(less);
    a.ldx(imm(LESS));
    a.ldy(imm(b'='));
    a.jsr("compound");
    a.ldy(imm(b'>'));
    a.bne(compare);
    a.bind(equal);
    a.ldx(imm(EQUAL));
    a.ldy(imm(0));
    a.bind(compare);
    a.jsr("compound");
    step(a, "level_add", "compare_values", "compare_next");

    // A compound operator: X gains EQUAL, or GREATER for `<>`, if the next
    // character is Y, which is then passed; A kept.
    a.here("compound");
    a.pha();
    a.sty(zp(z::OP));
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(zp(z::OP));
    let unchanged = a.local();
    a.bne(unchanged);
    a.inc(zp(z::OFFSET_A));
    a.txa();
    a.ora(imm(EQUAL));
    a.cpx(imm(LESS));
    let set = a.local();
    a.bne(set);
    a.lda(zp(z::OP));
    a.cmp(imm(b'>'));
    a.lda(imm(LESS | EQUAL));
    a.bcc(set);
    a.lda(imm(LESS | GREATER));
    a.bind(set);
    a.tax();
    a.bind(unchanged);
    a.pla();
    a.rts();

    a.here("level_add");
    a.jsr("level_multiply");
    a.here("add_next");
    a.cpx(imm(b'+'));
    let op = a.local();
    a.beq(op);
    a.cpx(imm(b'-'));
    a.bne(RTS);
    a.bind(op);
    step(a, "level_multiply", "add_values", "add_next");

    a.here("level_multiply");
    a.jsr("level_power");
    a.here("multiply_next");
    let op = a.local();
    for operator in [b'*', b'/', DIV] {
        a.cpx(imm(operator));
        a.beq(op);
    }
    a.cpx(imm(MOD));
    a.bne(RTS);
    a.bind(op);
    step(a, "level_power", "multiply_values", "multiply_next");

    a.here("level_power");
    a.jsr("factor");
    a.here("power_next");
    a.cpx(imm(b'^'));
    a.bne(RTS);
    step(a, "factor", "power_values", "power_next");
}

fn operands(a: &mut Asm) {
    // Both operands numbers: C clear for two integers, IAC2 the left and
    // IAC the right; C set otherwise, both made reals, FAC2 the left and
    // FAC the right. A string either side is a Type mismatch.
    a.here("numeric_pair");
    a.sta(zp(LEFT));
    a.tax();
    let mismatch = a.local();
    a.beq(mismatch);
    a.tya();
    a.beq(mismatch);
    a.ora(zp(LEFT));
    a.cmp(imm(kind::INTEGER));
    let reals = a.local();
    a.bne(reals);
    a.jsr("pop_int");
    a.clc();
    a.rts();
    a.bind(mismatch);
    a.jmp("err_type_mismatch");
    a.bind(reals);
    a.tya();
    let right_real = a.local();
    a.bmi(right_real);
    a.jsr("fac_from_int");
    a.bind(right_real);
    a.lda(zp(LEFT));
    let left_integer = a.local();
    a.bpl(left_integer);
    a.jsr("pop_real");
    a.sec();
    a.rts();
    a.bind(left_integer);
    a.jsr("pop_int");
    a.jsr("fac_to_fac2");
    a.jsr("int2_to_fac");
    a.jsr("fac_swap");
    a.sec();
    a.rts();

    // FAC = IAC2.
    a.here("int2_to_fac");
    for i in 0..4 {
        a.lda(zp(z::IAC2 + i));
        a.sta(zp(z::IAC + i));
    }
    a.jmp("fac_from_int");

    // Two integers made reals: FAC2 the left, FAC the right.
    a.here("ints_to_reals");
    a.jsr("fac_from_int");
    a.jsr("fac_to_fac2");
    a.jsr("int2_to_fac");
    a.jmp("fac_swap");
}

fn arithmetic(a: &mut Asm) {
    // + and -, X the operator: strings join, numbers add.
    a.here("add_values");
    a.cpx(imm(b'-'));
    let subtract = a.local();
    a.beq(subtract);
    a.tax();
    let (numbers, mismatch) = (a.local(), a.local());
    a.bne(numbers);
    a.tya();
    a.bne(mismatch);
    a.jmp("concatenate");
    a.bind(mismatch);
    a.jmp("err_type_mismatch");
    a.bind(numbers);
    a.jsr("numeric_pair");
    let real = a.local();
    a.bcs(real);
    // IAC = IAC2 + IAC, A its kind.
    a.here("int_add");
    a.clc();
    for i in 0..4 {
        a.lda(zp(z::IAC2 + i));
        a.adc(zp(z::IAC + i));
        a.sta(zp(z::IAC + i));
    }
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(real);
    a.jsr("fac_add");
    a.lda(imm(kind::REAL));
    a.rts();
    a.bind(subtract);
    a.jsr("numeric_pair");
    let real = a.local();
    a.bcs(real);
    a.sec();
    for i in 0..4 {
        a.lda(zp(z::IAC2 + i));
        a.sbc(zp(z::IAC + i));
        a.sta(zp(z::IAC + i));
    }
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(real);
    a.jsr("fac_subtract");
    a.lda(imm(kind::REAL));
    a.rts();

    // *, /, DIV and MOD, X the operator.
    a.here("multiply_values");
    a.cpx(imm(DIV));
    let integer_division = a.local();
    a.beq(integer_division);
    a.cpx(imm(MOD));
    a.beq(integer_division);
    a.cpx(imm(b'/'));
    let divide = a.local();
    a.beq(divide);
    a.jsr("numeric_pair");
    let real = a.local();
    a.bcs(real);
    a.jmp("int_multiply");
    a.bind(real);
    a.jsr("fac_multiply");
    a.lda(imm(kind::REAL));
    a.rts();
    a.bind(divide);
    a.jsr("numeric_pair");
    let reals = a.local();
    a.bcs(reals);
    a.jsr("ints_to_reals");
    a.bind(reals);
    a.jsr("fac_divide");
    a.lda(imm(kind::REAL));
    a.rts();
    a.bind(integer_division);
    a.jsr("integer_pair");
    a.txa();
    a.pha();
    a.jsr("int_divide");
    a.pla();
    a.cmp(imm(MOD));
    let quotient = a.local();
    a.bne(quotient);
    a.ldx(imm(3));
    let remainder = a.here_local();
    a.lda(zp(z::INT_WORK).x());
    a.sta(zp(z::IAC).x());
    a.dex();
    a.bpl(remainder);
    a.bind(quotient);
    a.lda(imm(kind::INTEGER));
    a.rts();

    // ^: a real raised to a real, by the functions' EXP and LN.
    a.here("power_values");
    a.jsr("numeric_pair");
    let reals = a.local();
    a.bcs(reals);
    a.jsr("ints_to_reals");
    a.bind(reals);
    a.jmp("fac_power");
}

fn bitwise(a: &mut Asm) {
    // AND, OR and EOR on integers, X the operator.
    a.here("logical");
    a.jsr("integer_pair");
    let (or, eor) = (a.local(), a.local());
    a.cpx(imm(OR));
    a.beq(or);
    a.cpx(imm(EOR));
    a.beq(eor);
    for (label, op) in [(None, 0u8), (Some(or), 1), (Some(eor), 2)] {
        if let Some(label) = label {
            a.bind(label);
        }
        a.ldx(imm(3));
        let byte = a.here_local();
        a.lda(zp(z::IAC).x());
        match op {
            0 => a.and(zp(z::IAC2).x()),
            1 => a.ora(zp(z::IAC2).x()),
            _ => a.eor(zp(z::IAC2).x()),
        }
        a.sta(zp(z::IAC).x());
        a.dex();
        a.bpl(byte);
        a.lda(imm(kind::INTEGER));
        a.rts();
    }

    // Both operands integers: the left from the stack into IAC2, either
    // made one from a real; X kept.
    a.here("integer_pair");
    a.sta(zp(LEFT));
    a.tya();
    a.jsr("make_integer");
    a.lda(zp(LEFT));
    let (left_real, mismatch) = (a.local(), a.local());
    a.bmi(left_real);
    a.beq(mismatch);
    a.jmp("pop_int");
    a.bind(left_real);
    // The left real: popped, made an integer, then the right restored.
    a.txa();
    a.pha();
    for i in 0..4 {
        a.lda(zp(z::IAC + i));
        a.pha();
    }
    a.jsr("pop_real");
    a.jsr("fac_from_fac2");
    a.jsr("int_from_fac");
    for i in 0..4 {
        a.lda(zp(z::IAC + i));
        a.sta(zp(z::IAC2 + i));
    }
    for i in (0..4).rev() {
        a.pla();
        a.sta(zp(z::IAC + i));
    }
    a.pla();
    a.tax();
    a.rts();
    a.bind(mismatch);
    a.jmp("err_type_mismatch");
}

fn integers(a: &mut Asm) {
    // IAC = IAC2 × IAC: magnitudes within sixteen bits multiply here;
    // larger ones, or a product past 31 bits, multiply as reals, the
    // product an integer again whenever it fits.
    a.here("int_multiply");
    a.lda(zp(z::IAC2 + 3));
    a.eor(zp(z::IAC + 3));
    a.pha();
    a.ldx(imm(lo(z::IAC2)));
    a.jsr("int_abs");
    a.ldx(imm(lo(z::IAC)));
    a.jsr("int_abs");
    a.lda(zp(z::IAC2 + 2));
    for byte in [z::IAC2 + 3, z::IAC + 2, z::IAC + 3] {
        a.ora(zp(byte));
    }
    let (large, real, signed) = (a.local(), a.local(), a.local());
    a.bne(large);
    // The product and the multiplier's remaining bits share IAC.
    a.ldx(imm(16));
    a.lsr(zp(z::IAC + 1));
    a.ror(zp(z::IAC));
    let bit = a.here_local();
    let skip = a.local();
    a.bcc(skip);
    a.clc();
    a.lda(zp(z::IAC + 2));
    a.adc(zp(z::IAC2));
    a.sta(zp(z::IAC + 2));
    a.lda(zp(z::IAC + 3));
    a.adc(zp(z::IAC2 + 1));
    a.sta(zp(z::IAC + 3));
    a.bind(skip);
    for i in (0..4).rev() {
        a.ror(zp(z::IAC + i));
    }
    a.dex();
    a.bne(bit);
    a.lda(zp(z::IAC + 3));
    a.bmi(real);
    let apply_sign = a.here_local();
    a.pla();
    a.bpl(signed);
    a.jsr("int_negate");
    a.bind(signed);
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(real);
    a.jsr("fac_from_unsigned");
    a.jmp("product_sign");
    a.bind(large);
    a.jsr("fac_from_unsigned");
    a.jsr("fac_to_fac2");
    for i in 0..4 {
        a.lda(zp(z::IAC2 + i));
        a.sta(zp(z::IAC + i));
    }
    a.jsr("fac_from_unsigned");
    a.jsr("fac_swap");
    a.jsr("fac_multiply");
    a.lda(zp(FAC.exp()));
    a.cmp(imm(0xA0));
    a.bcs("product_sign");
    a.jsr("int_from_fac");
    a.jmp(apply_sign);
    // The real product's sign.
    a.here("product_sign");
    a.pla();
    a.and(imm(0x80));
    a.sta(zp(FAC.sign()));
    a.lda(imm(kind::REAL));
    a.rts();

    // The integer at X made its magnitude.
    a.here("int_abs");
    a.lda(zp(3).x());
    a.bpl(RTS);
    a.jmp("int_negate_x");
}

fn dividing(a: &mut Asm) {
    // IAC = IAC2 DIV IAC toward zero, INT_WORK the remainder with the
    // dividend's sign; Division by zero for a zero divisor.
    a.here("int_divide");
    a.lda(zp(z::IAC));
    a.ora(zp(z::IAC + 1));
    a.ora(zp(z::IAC + 2));
    a.ora(zp(z::IAC + 3));
    let divisor = a.local();
    a.bne(divisor);
    a.jmp("err_division_by_zero");
    a.bind(divisor);
    a.lda(zp(z::IAC2 + 3));
    a.pha();
    a.eor(zp(z::IAC + 3));
    a.pha();
    a.lda(zp(z::IAC + 3));
    let positive = a.local();
    a.bpl(positive);
    a.jsr("int_negate");
    a.bind(positive);
    a.ldx(imm(lo(z::IAC2)));
    a.jsr("int_abs");
    // Long division: the dividend shifts through the remainder, the
    // quotient's bits shift in behind it. A dividend below 2^16 starts at
    // the top, sixteen steps from the end.
    a.lda(imm(0));
    for i in 0..4 {
        a.sta(zp(z::INT_WORK + i));
    }
    let bit = a.local();
    a.ldx(imm(32));
    a.lda(zp(z::IAC2 + 3));
    a.ora(zp(z::IAC2 + 2));
    a.bne(bit);
    a.lda(zp(z::IAC2 + 1));
    a.sta(zp(z::IAC2 + 3));
    a.lda(zp(z::IAC2));
    a.sta(zp(z::IAC2 + 2));
    a.lda(imm(0));
    a.sta(zp(z::IAC2 + 1));
    a.sta(zp(z::IAC2));
    a.ldx(imm(16));
    a.bind(bit);
    a.asl(zp(z::IAC2));
    for i in 1..4 {
        a.rol(zp(z::IAC2 + i));
    }
    for i in 0..4 {
        a.rol(zp(z::INT_WORK + i));
    }
    a.sec();
    for i in 0..3 {
        a.lda(zp(z::INT_WORK + i));
        a.sbc(zp(z::IAC + i));
        a.pha();
    }
    a.lda(zp(z::INT_WORK + 3));
    a.sbc(zp(z::IAC + 3));
    let (smaller, next) = (a.local(), a.local());
    a.bcc(smaller);
    a.sta(zp(z::INT_WORK + 3));
    for i in (0..3).rev() {
        a.pla();
        a.sta(zp(z::INT_WORK + i));
    }
    a.inc(zp(z::IAC2));
    a.bne(next);
    a.bind(smaller);
    a.pla();
    a.pla();
    a.pla();
    a.bind(next);
    a.dex();
    a.bne(bit);
    for i in 0..4 {
        a.lda(zp(z::IAC2 + i));
        a.sta(zp(z::IAC + i));
    }
    a.pla();
    let quotient = a.local();
    a.bpl(quotient);
    a.jsr("int_negate");
    a.bind(quotient);
    a.pla();
    a.bpl(RTS);
    a.ldx(imm(lo(z::INT_WORK)));
    a.jmp("int_negate_x");
}

fn comparisons(a: &mut Asm) {
    // A comparison, X its true outcomes: TRUE or FALSE.
    a.here("compare_values");
    a.stx(zp(z::INT_WORK + 2));
    a.tax();
    let (numbers, decide, mismatch) = (a.local(), a.local(), a.local());
    a.bne(numbers);
    a.tya();
    a.bne(mismatch);
    a.jmp("compare_strings");
    a.bind(mismatch);
    a.jmp("err_type_mismatch");
    a.bind(numbers);
    a.jsr("numeric_pair");
    let real = a.local();
    a.bcs(real);
    // Integers: equal, or the sign of the difference with overflow
    // corrected.
    a.ldx(imm(3));
    let byte = a.here_local();
    let differ = a.local();
    a.lda(zp(z::IAC2).x());
    a.cmp(zp(z::IAC).x());
    a.bne(differ);
    a.dex();
    a.bpl(byte);
    a.lda(imm(0));
    a.beq(decide);
    a.bind(differ);
    a.lda(zp(z::IAC2));
    a.cmp(zp(z::IAC));
    for i in 1..4 {
        a.lda(zp(z::IAC2 + i));
        a.sbc(zp(z::IAC + i));
    }
    let signed = a.local();
    a.bvc(signed);
    a.eor(imm(0x80));
    a.bind(signed);
    a.ora(imm(1));
    a.bne(decide);
    a.bind(real);
    a.jsr("fac_compare");
    // A's sign as an outcome bit, tested against the operator's.
    a.here("decide");
    a.bind(decide);
    a.ldx(imm(EQUAL));
    let outcome = a.local();
    a.tay();
    a.beq(outcome);
    a.ldx(imm(LESS));
    a.tya();
    a.bmi(outcome);
    a.ldx(imm(GREATER));
    a.bind(outcome);
    a.txa();
    a.and(zp(z::INT_WORK + 2));
    let truth = a.local();
    a.beq(truth);
    a.lda(imm(0xFF));
    a.bind(truth);
    for i in 0..4 {
        a.sta(zp(z::IAC + i));
    }
    a.lda(imm(kind::INTEGER));
    a.rts();
}

fn strings(a: &mut Asm) {
    // The stacked string joined by the buffer's: String too long past 255.
    a.here("concatenate");
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(super::vars::LENGTH));
    a.clc();
    a.adc(zp(z::STRING_LENGTH));
    let fits = a.local();
    a.bcc(fits);
    a.jmp("err_string_too_long");
    a.bind(fits);
    a.pha();
    a.ldy(zp(z::STRING_LENGTH));
    let (raise, raised) = (a.here_local(), a.local());
    a.beq(raised);
    a.dey();
    a.tya();
    a.clc();
    a.adc(zp(super::vars::LENGTH));
    a.tax();
    a.lda(abs(page::STRING).y());
    a.sta(abs(page::STRING).x());
    a.tya();
    a.jmp(raise);
    a.bind(raised);
    a.ldy(zp(super::vars::LENGTH));
    let (copy, copied) = (a.here_local(), a.local());
    a.beq(copied);
    a.lda(zp(z::STACK).ind_y());
    a.sta(abs(page::STRING - 1).y());
    a.dey();
    a.jmp(copy);
    a.bind(copied);
    a.pla();
    a.sta(zp(z::STRING_LENGTH));
    a.jsr("pop_string");
    a.lda(imm(kind::STRING));
    a.rts();

    // The stacked string against the buffer's, character by character,
    // then by length.
    a.here("compare_strings");
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(super::vars::LENGTH));
    let (byte, differ, lengths) = (a.here_local(), a.local(), a.local());
    a.cpy(zp(super::vars::LENGTH));
    a.beq(lengths);
    a.cpy(zp(z::STRING_LENGTH));
    a.beq(lengths);
    a.iny();
    a.lda(zp(z::STACK).ind_y());
    a.cmp(abs(page::STRING - 1).y());
    a.beq(byte);
    a.bind(differ);
    a.lda(imm(0));
    a.ror(Acc);
    a.eor(imm(0x80));
    a.ora(imm(1));
    let order = a.local();
    a.bind(order);
    a.pha();
    a.jsr("pop_string");
    a.pla();
    a.jmp("decide");
    a.bind(lengths);
    a.lda(zp(super::vars::LENGTH));
    a.cmp(zp(z::STRING_LENGTH));
    a.bne(differ);
    a.lda(imm(0));
    a.beq(order);
}

fn factor(a: &mut Asm) {
    // A factor: a number, a string, a bracketed expression, a unary
    // operator and its factor, an indirection, a variable, or a function.
    a.here("factor");
    a.jsr("next_a");
    a.cmp(imm(0x80));
    let token = a.local();
    a.bcs(token);
    let (number, not_digit) = (a.local(), a.local());
    a.cmp(imm(b'0'));
    a.bcc(not_digit);
    a.cmp(imm(b'9' + 1));
    a.bcc(number);
    a.bind(not_digit);
    a.cmp(imm(b'@'));
    let name = a.local();
    a.beq(name);
    a.jsr("is_name");
    a.bcs(name);
    // The other factors, by their first character.
    a.ldx(imm(byte(FACTORS.len() - 1)));
    let (search, dispatch) = (a.here_local(), a.local());
    a.cmp(abs("factor_characters").x());
    a.beq(dispatch);
    a.dex();
    a.bpl(search);
    a.jmp("err_no_such_variable");
    a.bind(dispatch);
    a.lda(abs("factor_high").x());
    a.pha();
    a.lda(abs("factor_low").x());
    a.pha();
    a.rts();
    a.bind(token);
    a.jmp("factor_token");
    a.bind(number);
    a.here("factor_number");
    a.dey();
    a.jsr("read_number");
    a.sty(zp(z::OFFSET_A));
    a.jmp("factor_done");
    a.bind(name);
    a.dec(zp(z::OFFSET_A));
    a.jsr("variable");
    let found = a.local();
    a.bcc(found);
    // The assembler with OPT's errors off takes it as P%.
    a.bit(zp(z::ASM_QUIET));
    let quiet = a.local();
    a.bmi(quiet);
    a.jmp("err_no_such_variable");
    a.bind(quiet);
    a.ldx(imm(3));
    let copy = a.here_local();
    a.lda(abs(page::resident(b'P')).x());
    a.sta(zp(z::IAC).x());
    a.dex();
    a.bpl(copy);
    a.lda(imm(kind::INTEGER));
    a.jmp("factor_done");
    a.bind(found);
    a.jsr("load_variable");
    // A variable may be followed by ? or !, an address and an offset.
    a.sta(zp(z::TYPE));
    a.jsr("next_a");
    a.cmp(imm(b'?'));
    let (binary, plain) = (a.local(), a.local());
    a.beq(binary);
    a.cmp(imm(b'!'));
    a.bne(plain);
    a.bind(binary);
    a.pha();
    a.lda(zp(z::TYPE));
    a.jsr("make_integer");
    a.jsr("push_value");
    a.jsr("factor_integer");
    a.jsr("pop_int");
    a.jsr("int_add");
    a.pla();
    a.ldy(imm(place::BYTE));
    a.cmp(imm(b'?'));
    let typed = a.local();
    a.beq(typed);
    a.ldy(imm(place::INTEGER));
    a.bind(typed);
    a.tya();
    a.jmp("load_at_iac");
    a.bind(plain);
    a.tax();
    a.lda(zp(z::TYPE));
    a.rts();
}

fn factors(a: &mut Asm) {
    // The character after a factor read into X; A its kind.
    a.here("factor_done");
    a.sta(zp(z::TYPE));
    a.jsr("next_a");
    a.tax();
    a.lda(zp(z::TYPE));
    a.rts();

    a.here("factor_bracket");
    a.jsr("expr");
    a.cpx(imm(b')'));
    a.beq("factor_done");
    a.jmp("err_missing_bracket");

    a.here("factor_minus");
    a.jsr("factor");
    a.tay();
    let string = a.local();
    a.beq(string);
    a.txa();
    a.pha();
    a.tya();
    let real = a.local();
    a.bmi(real);
    a.jsr("int_negate");
    a.pla();
    a.tax();
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(real);
    a.jsr("fac_negate");
    a.pla();
    a.tax();
    a.lda(imm(kind::REAL));
    a.rts();
    a.bind(string);
    a.jmp("err_type_mismatch");

    // A quoted string, "" inside it a quote.
    a.here("factor_string");
    a.ldx(imm(0));
    let (character, closed, unclosed) = (a.here_local(), a.local(), a.local());
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(CR));
    a.beq(unclosed);
    a.iny();
    a.cmp(imm(b'"'));
    a.bne(closed);
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'"'));
    let ended = a.local();
    a.bne(ended);
    a.iny();
    a.bind(closed);
    a.sta(abs(page::STRING).x());
    a.inx();
    a.bne(character);
    a.bind(unclosed);
    a.jmp("err_missing_quote");
    a.bind(ended);
    a.stx(zp(z::STRING_LENGTH));
    a.sty(zp(z::OFFSET_A));
    a.lda(imm(kind::STRING));
    a.jmp("factor_done");
}

fn indirections(a: &mut Asm) {
    // & and hex digits, the last eight kept: Bad HEX with none.
    a.here("factor_hex");
    a.jsr("int_zero");
    a.ldx(imm(0));
    let (digit, letter, ended) = (a.here_local(), a.local(), a.local());
    a.lda(zp(z::TEXT_A).ind_y());
    a.jsr("is_digit");
    let value = a.local();
    a.bcs(value);
    a.cmp(imm(b'A'));
    a.bcc(ended);
    a.cmp(imm(b'F' + 1));
    a.bcs(ended);
    a.bind(letter);
    a.sbc(imm(b'A' - 10 - 1));
    a.bind(value);
    a.and(imm(0x0F));
    a.pha();
    for _ in 0..4 {
        a.asl(zp(z::IAC));
        a.rol(zp(z::IAC + 1));
        a.rol(zp(z::IAC + 2));
        a.rol(zp(z::IAC + 3));
    }
    a.pla();
    a.ora(zp(z::IAC));
    a.sta(zp(z::IAC));
    a.iny();
    a.inx();
    a.bne(digit);
    a.bind(ended);
    a.txa();
    let some = a.local();
    a.bne(some);
    a.jmp("err_bad_hex");
    a.bind(some);
    a.sty(zp(z::OFFSET_A));
    a.lda(imm(kind::INTEGER));
    a.jmp("factor_done");

    // ?, ! and $ before a factor: the location it addresses.
    let indirect = a.local();
    a.here("factor_byte");
    a.lda(imm(place::BYTE));
    a.beq(indirect);
    a.here("factor_word");
    a.lda(imm(place::INTEGER));
    a.bne(indirect);
    a.here("factor_text");
    a.lda(imm(place::FIXED_STRING));
    a.bind(indirect);
    a.pha();
    a.jsr("factor_integer");
    a.pla();

    // The location of type A at IAC loaded; X kept.
    a.here("load_at_iac");
    a.sta(zp(z::VTYPE));
    a.lda(zp(z::IAC));
    a.sta(zp(z::VAR));
    a.lda(zp(z::IAC + 1));
    a.sta(zp(z::VAR + 1));
    a.jmp("load_variable");

    a.here("factor_characters");
    for (character, _) in FACTORS {
        a.byte(character);
    }
    a.here("factor_low");
    for (_, routine) in FACTORS {
        a.byte(lo(sym(routine) + -1));
    }
    a.here("factor_high");
    for (_, routine) in FACTORS {
        a.byte(hi(sym(routine) + -1));
    }
}

/// The factors known by their first character, and their routines; the
/// commonest last, as the search starts there.
const FACTORS: [(u8, &str); 9] = [
    (b'+', "factor"),
    (b'$', "factor_text"),
    (b'.', "factor_number"),
    (b'!', "factor_word"),
    (b'&', "factor_hex"),
    (b'?', "factor_byte"),
    (b'-', "factor_minus"),
    (b'"', "factor_string"),
    (b'(', "factor_bracket"),
];

fn coercions(a: &mut Asm) {
    // A factor as an integer.
    a.here("factor_integer");
    a.jsr("factor");
    a.fall_into("make_integer");

    // The value of kind A made an integer in IAC: from a real toward zero,
    // and a string a Type mismatch. X kept.
    a.here("make_integer");
    a.tay();
    let mismatch = a.local();
    a.beq(mismatch);
    a.bpl(RTS);
    a.txa();
    a.pha();
    a.jsr("int_from_fac");
    a.pla();
    a.tax();
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(mismatch);
    a.jmp("err_type_mismatch");

    // An expression as an integer, or a string; X the character after it.
    a.here("expr_integer");
    a.jsr("expr");
    a.jmp("make_integer");

    // The value of kind A made a real in FAC; X kept.
    a.here("make_real");
    a.tay();
    a.beq(mismatch);
    a.bmi(RTS);
    a.txa();
    a.pha();
    a.jsr("fac_from_int");
    a.pla();
    a.tax();
    a.lda(imm(kind::REAL));
    a.rts();

    a.here("expr_string");
    a.jsr("expr");
    a.tay();
    a.beq(RTS);
    a.jmp("err_type_mismatch");
}
