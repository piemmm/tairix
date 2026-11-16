//! Expressions: operands through IEVAL, binary operators by precedence —
//! OR, AND, NOT, the relations, + and −, × and ÷, unary minus, ↑ — the
//! left operand pushed rounded, and the syntax checks and conversions
//! statements share.

use super::float::{M3, M4};
use super::strings::NEW_LENGTH;
use super::tokens::token;
use super::ws::{vector, zp};
use crate::cpu::STACK_PAGE;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp as z, Asm, RTS};

/// Precedences, the highest binding tightest.
mod precedence {
    pub const NONE: u8 = 0;
    pub const OR: u8 = 1;
    pub const AND: u8 = 2;
    pub const NOT: u8 = 3;
    pub const RELATION: u8 = 4;
    pub const SUM: u8 = 5;
    pub const PRODUCT: u8 = 6;
    pub const NEGATION: u8 = 7;
    pub const POWER: u8 = 8;
}

/// The binary operators from token `+` to `OR`: precedence and routine.
const OPERATORS: [(u8, &str); 7] = [
    (precedence::SUM, "fadd"),
    (precedence::SUM, "fsub"),
    (precedence::PRODUCT, "fmul"),
    (precedence::PRODUCT, "fdiv"),
    (precedence::POWER, "power"),
    (precedence::AND, "operator_and"),
    (precedence::OR, "operator_or"),
];
/// The relations' tokens follow the operators'.
const RELATIONS: u8 = 3;
const _: () = assert!(token::OR + 1 == token::GREATER);
/// The operator index a relation takes on the stack, its relations in
/// [`zp::OPMASK`].
const RELATION: u8 = 0x80;
/// The relation bits: greater, equal, less.
const GREATER: u8 = 1;
const EQUAL: u8 = 2;
const LESS: u8 = 4;
/// The stack an expression level takes: its precedence, two returns, an
/// operand, its type and its operator.
const LEVEL_STACK: u8 = 12;
/// The operator being pushed and its precedence.
const OPERATOR: u16 = zp::OPPTR;
const OPERATOR_PRECEDENCE: u16 = zp::OPPTR + 1;
/// The left operand popped: a real, or a string's descriptor's address.
pub const LEFT: u16 = zp::TEMPF2;

/// The flowing code; [`eval`] is pinned by the module above.
pub fn emit(a: &mut Asm) {
    a.whole(expression);
    a.whole(|a| {
        binary(a);
        relations(a);
    });
    a.whole(logic);
    a.whole(operands);
    a.whole(unary);
    a.whole(functions);
    a.whole(checks);
    a.later(tables);
}

fn tables(a: &mut Asm) {
    a.here("operator_precedence");
    for (rank, _) in OPERATORS {
        a.byte(rank);
    }
    for _ in 0..RELATIONS {
        a.byte(precedence::RELATION);
    }
    a.here("operator_low");
    for (_, name) in OPERATORS {
        a.byte(lo(sym(name) + -1));
    }
    a.here("operator_high");
    for (_, name) in OPERATORS {
        a.byte(hi(sym(name) + -1));
    }
    a.here("eval_characters");
    for (character, _) in EVAL_CASES {
        a.byte(character);
    }
    a.here("eval_low");
    for (_, name) in EVAL_CASES {
        a.byte(lo(sym(name) + -1));
    }
    a.here("eval_high");
    for (_, name) in EVAL_CASES {
        a.byte(hi(sym(name) + -1));
    }
}

/// The operands `eval` dispatches by their first character.
const EVAL_CASES: [(u8, &str); 7] = [
    (token::PI, "eval_pi"),
    (b'"', "str_literal"),
    (token::NOT, "eval_not"),
    (token::FN, "fn_call"),
    (token::MINUS, "eval_minus"),
    (token::PLUS, "eval"),
    (b'(', "eval_bracket"),
];

/// `evaluate`: the expression at TXTPTR into FAC, or a string's descriptor
/// into M3 (low) and M4 with VALTYP set; TXTPTR at what follows.
/// `expression`: the same from just before TXTPTR, binding only operators
/// above precedence A.
fn expression(a: &mut Asm) {
    a.here("evaluate");
    let no_borrow = a.local();
    a.lda(z(zp::TXTPTR));
    a.bne(no_borrow);
    a.dec(z(zp::TXTPTR + 1));
    a.bind(no_borrow);
    a.dec(z(zp::TXTPTR));
    a.lda(imm(precedence::NONE));
    a.here("expression");
    let next = a.local();
    let arithmetic = a.local();
    let done = a.local();
    a.pha();
    a.lda(imm(LEVEL_STACK));
    a.jsr("check_stack");
    a.jsr("eval_vector");
    // C clear only at a digit, below the operators either way.
    a.bind(next);
    a.jsr(zp::CHRGOT);
    a.sbc(imm(token::PLUS));
    a.bcc(done);
    a.cmp(imm(crate::rom::asm::byte(OPERATORS.len()) + RELATIONS));
    a.bcs(done);
    a.tay();
    a.lda(abs("operator_precedence").y());
    a.tsx();
    a.cmp(abs(STACK_PAGE + 1).x());
    a.bcc(done);
    a.beq(done);
    a.tax();
    a.tya();
    a.cpy(imm(crate::rom::asm::byte(OPERATORS.len())));
    a.bcc(arithmetic);
    a.jsr("relation_mask");
    a.lda(imm(RELATION));
    a.bind(arithmetic);
    a.jsr("binary");
    a.jmp(next);
    a.bind(done);
    a.pla();
    a.rts();

    a.here("eval_vector");
    a.jmp(abs(vector::IEVAL).ind());
}

/// `binary`: operator A at precedence X applied: the left operand pushed
/// rounded with its type, the right evaluated binding tighter, then the
/// types matched and the operator run with the left in ARG.
fn binary(a: &mut Asm) {
    a.here("binary");
    let numeric_left = a.local();
    let push = a.local();
    let pull = a.local();
    let numeric = a.local();
    let mismatch = a.local();
    a.sta(z(OPERATOR));
    a.stx(z(OPERATOR_PRECEDENCE));
    a.lda(z(zp::VALTYP));
    a.beq(numeric_left);
    a.lda(z(M3));
    a.sta(z(LEFT));
    a.lda(z(M4));
    a.sta(z(LEFT + 1));
    a.jmp(push);
    a.bind(numeric_left);
    a.ldx(imm(lo(LEFT)));
    a.ldy(imm(hi(LEFT)));
    a.jsr("fac_store");
    a.bind(push);
    a.ldx(imm(4));
    let byte = a.here_local();
    a.lda(z(LEFT).x());
    a.pha();
    a.dex();
    a.bpl(byte);
    a.lda(z(zp::VALTYP));
    a.pha();
    a.lda(z(OPERATOR));
    a.pha();
    a.lda(z(zp::OPMASK));
    a.pha();
    a.lda(z(OPERATOR_PRECEDENCE));
    a.jsr("expression");
    // The right in FAC; the left's relations, operator, type and five
    // bytes popped.
    a.pla();
    a.sta(z(zp::OPMASK));
    a.pla();
    a.sta(z(OPERATOR));
    a.pla();
    a.cmp(z(zp::VALTYP));
    a.bne(mismatch);
    a.ldx(imm(0));
    a.bind(pull);
    a.pla();
    a.sta(z(LEFT).x());
    a.inx();
    a.cpx(imm(5));
    a.bne(pull);
    a.lda(z(OPERATOR));
    a.bmi("compare_operands");
    a.ldx(z(zp::VALTYP));
    a.beq(numeric);
    // Strings join by + alone.
    a.tay();
    a.bne(mismatch);
    a.jmp("concatenate");
    a.bind(numeric);
    a.tax();
    a.lda(abs("operator_high").x());
    a.pha();
    a.lda(abs("operator_low").x());
    a.pha();
    a.lda(imm(lo(LEFT)));
    a.ldy(imm(hi(LEFT)));
    a.jmp("arg_load");
    a.bind(mismatch);
    a.jmp("type_mismatch_error");
}

/// `relation_mask`: OPMASK the relations from TXTPTR, each once, TXTPTR
/// left at the last; X is kept. `compare_operands`: the left and right compared,
/// as numbers rounded or as strings byte by byte, the shorter a prefix
/// being the less; FAC −1 where a relation holds, else 0.
fn relations(a: &mut Asm) {
    a.here("relation_mask");
    let next = a.local();
    let repeated = a.local();
    a.lda(imm(0));
    a.sta(z(zp::OPMASK));
    a.bind(next);
    // C set by the relation's token, kept for the second subtraction.
    a.jsr(zp::CHRGOT);
    a.sbc(imm(token::GREATER));
    a.tay();
    a.lda(abs("relation_bits").y());
    a.bit(z(zp::OPMASK));
    a.bne(repeated);
    a.ora(z(zp::OPMASK));
    a.sta(z(zp::OPMASK));
    a.ldy(imm(1));
    a.lda(z(zp::TXTPTR).ind_y());
    a.sbc(imm(token::GREATER));
    a.cmp(imm(3));
    let more = a.local();
    a.bcc(more);
    a.rts();
    a.bind(more);
    a.jsr(zp::CHRGET);
    // A relation's token, never a statement's end.
    a.bne(next);
    a.bind(repeated);
    a.jmp("syntax_error");
    a.later(|a| {
        a.here("relation_bits");
        a.bytes(&[GREATER, EQUAL, LESS]);
    });

    a.here("compare_operands");
    let strings = a.local();
    let result = a.local();
    a.lda(z(zp::VALTYP));
    a.bne(strings);
    a.lda(imm(lo(LEFT)));
    a.ldy(imm(hi(LEFT)));
    a.jsr("compare_fac");
    a.jmp(result);
    a.bind(strings);
    a.jsr("compare_strings");
    // A 1, 0 or $FF as the right is greater, equal or less: the relation
    // bit the left takes to the right.
    a.bind(result);
    a.tax();
    a.inx();
    a.lda(abs("relation_bits").x());
    a.and(z(zp::OPMASK));
    a.beq("false_result");
    a.lda(imm(0xFF));
    a.here("false_result");
    a.ldx(imm(0));
    a.stx(z(zp::VALTYP));
    a.jmp("signed_byte_to_fac");
}

/// AND, OR and NOT: each operand floored to a signed 16-bit integer, the
/// bits combined, the result a signed integer.
fn logic(a: &mut Asm) {
    for (name, combine) in [("operator_and", true), ("operator_or", false)] {
        a.here(name);
        a.jsr("integer_operands");
        if combine {
            a.and(z(M4));
        } else {
            a.ora(z(M4));
        }
        a.tay();
        a.txa();
        if combine {
            a.and(z(M3));
        } else {
            a.ora(z(M3));
        }
        a.jmp("word_to_fac");
    }

    // The right in FAC, then the left in ARG, each made a signed integer:
    // the left in M3 (high) and M4, the right in X (high) and A.
    a.here("integer_operands");
    a.jsr("fac_to_integer");
    a.lda(z(M4));
    a.pha();
    a.lda(z(M3));
    a.pha();
    a.jsr("arg_to_fac");
    a.jsr("fac_to_integer");
    a.pla();
    a.tax();
    a.pla();
    a.rts();

    a.here("operator_not");
    a.jsr("fac_to_integer");
    a.lda(z(M4));
    a.eor(imm(0xFF));
    a.tay();
    a.lda(z(M3));
    a.eor(imm(0xFF));
    a.jmp("word_to_fac");
}

/// `eval`, IEVAL's default: the operand after TXTPTR — a number, π, a
/// string in quotes, a variable, a function, FN, a bracketed expression,
/// or one after unary minus, plus or NOT.
pub(super) fn eval(a: &mut Asm) {
    a.here("eval");
    let number = a.local();
    let not_number = a.local();
    let scan = a.local();
    let found = a.local();
    let not_function = a.local();
    let syntax = a.local();
    a.lda(imm(0));
    a.sta(z(zp::VALTYP));
    a.jsr(zp::CHRGET);
    a.bcs(not_number);
    a.bind(number);
    a.jmp("text_to_fac");
    a.bind(not_number);
    a.cmp(imm(b'.'));
    a.beq(number);
    a.ldx(imm(crate::rom::asm::byte(EVAL_CASES.len() - 1)));
    a.bind(scan);
    a.cmp(abs("eval_characters").x());
    a.beq(found);
    a.dex();
    a.bpl(scan);
    a.cmp(imm(token::SGN));
    a.bcc(not_function);
    a.cmp(imm(token::MID + 1));
    a.bcs(syntax);
    a.jmp("function_call");
    a.bind(not_function);
    a.jsr("is_letter");
    a.bcc(syntax);
    a.jmp("eval_variable");
    a.bind(syntax);
    a.jmp("syntax_error");
    a.bind(found);
    a.lda(abs("eval_high").x());
    a.pha();
    a.lda(abs("eval_low").x());
    a.pha();
    a.rts();
}

/// π, a bracketed expression, and whether A is a letter.
fn operands(a: &mut Asm) {
    a.here("eval_pi");
    a.lda(imm(lo("real_pi")));
    a.ldy(imm(hi("real_pi")));
    a.jsr("fac_load");
    a.jmp(abs(zp::CHRGET));

    a.here("eval_bracket");
    a.lda(imm(precedence::NONE));
    a.jsr("expression");
    a.jmp("expect_close");

    // C set when A is a letter.
    a.here("is_letter");
    let not = a.local();
    a.cmp(imm(b'['));
    a.bcs(not);
    a.cmp(imm(b'A'));
    a.rts();
    a.bind(not);
    a.clc();
    a.rts();
}

/// Unary minus and NOT: their operand, binding only tighter operators,
/// then negated or its bits inverted.
fn unary(a: &mut Asm) {
    a.here("eval_minus");
    a.lda(imm(precedence::NEGATION));
    a.jsr("expression");
    a.jsr("check_numeric");
    a.jmp("negate");

    a.here("eval_not");
    a.lda(imm(precedence::NOT));
    a.jsr("expression");
    a.jsr("check_numeric");
    a.jmp("operator_not");
}

/// The token's depth on the stack while LEFT$, RIGHT$ or MID$ reads its
/// number: beneath the number and the descriptor.
const TOKEN_DEPTH: u8 = 4;

/// A function after TXTPTR: its argument in brackets, of the type it
/// takes — FRE and POS any — then its routine. LEFT$ and RIGHT$ take a
/// string and a number, MID$ a string and one or two numbers.
fn functions(a: &mut Asm) {
    a.here("function_call");
    let string_function = a.local();
    let wants_string = a.local();
    let checked = a.local();
    let dispatch = a.local();
    a.cmp(imm(token::LEFT));
    a.bcs(string_function);
    a.pha();
    a.jsr("open_argument");
    a.jsr("expect_close");
    a.pla();
    a.tax();
    a.cpx(imm(token::FRE));
    a.beq(checked);
    a.cpx(imm(token::POS));
    a.beq(checked);
    for string_token in [token::LEN, token::VAL, token::ASC] {
        a.cpx(imm(string_token));
        a.beq(wants_string);
    }
    // Returns with A, VALTYP, zero.
    a.jsr("check_numeric");
    a.beq(checked);
    a.bind(wants_string);
    a.jsr("check_string");
    a.bind(checked);
    a.bind(dispatch);
    let base = -i32::from(token::SGN);
    a.lda(abs(sym("function_high") + base).x());
    a.pha();
    a.lda(abs(sym("function_low") + base).x());
    a.pha();
    a.rts();

    a.bind(string_function);
    let length_given = a.local();
    a.pha();
    a.jsr("open_argument");
    a.jsr("check_string");
    a.lda(z(M4));
    a.pha();
    a.lda(z(M3));
    a.pha();
    a.jsr("expect_comma");
    a.jsr("evaluate_byte");
    a.tay();
    a.txa();
    a.pha();
    a.tya();
    a.ldx(imm(0xFF));
    a.cmp(imm(b','));
    a.bne(length_given);
    // MID$ alone takes a length; another's comma fails the bracket.
    a.tsx();
    a.lda(abs(STACK_PAGE + u16::from(TOKEN_DEPTH)).x());
    a.cmp(imm(token::MID));
    a.bne(length_given);
    a.jsr(zp::CHRGET);
    a.jsr("evaluate_byte");
    a.bind(length_given);
    a.jsr("expect_close");
    // X the length MID$ takes; then from the stack the number, the
    // descriptor and the token.
    a.stx(z(NEW_LENGTH));
    a.pla();
    a.tay();
    a.pla();
    a.sta(z(zp::DSCPNT));
    a.pla();
    a.sta(z(zp::DSCPNT + 1));
    a.pla();
    a.tax();
    a.bne(dispatch);

    // The argument after the function's token and its opening bracket.
    a.here("open_argument");
    a.jsr(zp::CHRGET);
    a.jsr("expect_open");
    a.jmp("evaluate");
}

/// `expect_char`: A the character at TXTPTR, then CHRGET, else SYNTAX. The
/// brackets and comma, the type checks, and the conversions statements
/// take their numbers by.
fn checks(a: &mut Asm) {
    a.here("expect_close");
    a.lda(imm(b')'));
    a.skip_over(|a| {
        a.here("expect_open");
        a.lda(imm(b'('));
    });
    a.skip_over(|a| {
        a.here("expect_comma");
        a.lda(imm(b','));
    });
    a.here("expect_char");
    let matched = a.local();
    a.ldy(imm(0));
    a.cmp(z(zp::TXTPTR).ind_y());
    a.beq(matched);
    a.jmp("syntax_error");
    a.bind(matched);
    a.jmp(abs(zp::CHRGET));

    a.here("evaluate_number");
    a.jsr("evaluate");
    a.here("check_numeric");
    let mismatch = a.local();
    a.lda(z(zp::VALTYP));
    a.bne(mismatch);
    a.rts();
    a.here("check_string");
    a.lda(z(zp::VALTYP));
    a.bne(RTS);
    a.bind(mismatch);
    a.jmp("type_mismatch_error");

    // The expression at TXTPTR as a byte in X, A the character after it.
    a.here("evaluate_byte");
    a.jsr("evaluate_number");
    a.jsr("fac_to_byte");
    a.jmp(abs(zp::CHRGOT));

    // LINNUM is left as it was.
    a.here("fac_to_byte");
    let illegal = a.local();
    a.jsr("integer_part");
    a.tax();
    a.beq(RTS);
    a.bit(z(zp::FACSGN));
    a.bmi(illegal);
    a.lda(z(M3));
    a.bne(illegal);
    a.ldx(z(M4));
    a.rts();
    a.bind(illegal);
    a.jmp("illegal_quantity_error");

    // The expression at TXTPTR as an address in LINNUM.
    a.here("getadr_expression");
    a.jsr("evaluate_number");
    a.jmp("fac_to_word");
}
