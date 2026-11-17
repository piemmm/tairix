//! Errors: each a BRK with its number and message that a failing routine
//! jumps to, and BRKV's handler, which reports it and returns to the prompt.

use alloc::vec::Vec;

use tairix_openbasic::keyword::{word, Word};

use super::list::TOKEN_KEPT;
use super::ws::zp as z;
use crate::cpu::STACK_PAGE;
use crate::rom::asm::{abs, imm, zp, Asm, RTS};
use crate::rom::os::{entry, ERROR_POINTER};

/// A message's text, and each keyword in it, which BASIC II keeps as its
/// token.
#[derive(Clone, Copy)]
enum Part {
    Text(&'static str),
    Keyword(Word<u8>),
}
use Part::{Keyword as K, Text as T};

/// Each error's routine, number and message, as BASIC II gives them.
const ERRORS: [(&str, u8, &[Part]); 46] = [
    ("err_no_room", 0, &[T("No room")]),
    ("err_out_of_range", 1, &[T("Out of range")]),
    ("err_byte", 2, &[T("Byte")]),
    ("err_index", 3, &[T("Index")]),
    ("err_stop", 0, &[T("STOP")]),
    ("err_silly", 0, &[T("Silly")]),
    ("err_mistake", 4, &[T("Mistake")]),
    ("err_missing_comma", 5, &[T("Missing ,")]),
    ("err_type_mismatch", 6, &[T("Type mismatch")]),
    ("err_no_fn", 7, &[T("No "), K(word::FN)]),
    ("err_missing_quote", 9, &[T("Missing \"")]),
    ("err_bad_dim", 10, &[T("Bad "), K(word::DIM)]),
    ("err_dim_space", 11, &[K(word::DIM), T(" space")]),
    ("err_not_local", 12, &[T("Not "), K(word::LOCAL)]),
    ("err_no_proc", 13, &[T("No "), K(word::PROC)]),
    ("err_array", 14, &[T("Array")]),
    ("err_subscript", 15, &[T("Subscript")]),
    ("err_syntax", 16, &[T("Syntax error")]),
    ("err_escape", 17, &[T("Escape")]),
    ("err_division_by_zero", 18, &[T("Division by zero")]),
    ("err_string_too_long", 19, &[T("String too long")]),
    ("err_too_big", 20, &[T("Too big")]),
    ("err_negative_root", 21, &[T("-ve root")]),
    ("err_log_range", 22, &[T("Log range")]),
    ("err_accuracy_lost", 23, &[T("Accuracy lost")]),
    ("err_exp_range", 24, &[T("Exp range")]),
    ("err_bad_mode", 25, &[T("Bad "), K(word::MODE)]),
    ("err_no_such_variable", 26, &[T("No such variable")]),
    ("err_missing_bracket", 27, &[T("Missing )")]),
    ("err_bad_hex", 28, &[T("Bad HEX")]),
    (
        "err_no_such_fn_proc",
        29,
        &[T("No such "), K(word::FN), T("/"), K(word::PROC)],
    ),
    ("err_arguments", 31, &[T("Arguments")]),
    ("err_no_for", 32, &[T("No "), K(word::FOR)]),
    ("err_cant_match_for", 33, &[T("Can't Match "), K(word::FOR)]),
    ("err_for_variable", 34, &[K(word::FOR), T(" variable")]),
    (
        "err_too_many_fors",
        35,
        &[T("Too many "), K(word::FOR), T("s")],
    ),
    ("err_no_to", 36, &[T("No "), K(word::TO)]),
    (
        "err_too_many_gosubs",
        37,
        &[T("Too many "), K(word::GOSUB), T("s")],
    ),
    ("err_no_gosub", 38, &[T("No "), K(word::GOSUB)]),
    ("err_on_syntax", 39, &[K(word::ON), T(" syntax")]),
    ("err_on_range", 40, &[K(word::ON), T(" range")]),
    ("err_no_such_line", 41, &[T("No such line")]),
    ("err_out_of_data", 42, &[T("Out of "), K(word::DATA)]),
    ("err_no_repeat", 43, &[T("No "), K(word::REPEAT)]),
    (
        "err_too_many_repeats",
        44,
        &[T("Too many "), K(word::REPEAT), T("s")],
    ),
    ("err_missing_hash", 45, &[T("Missing #")]),
];

/// `parts` as the bytes of a message.
fn message(parts: &[Part]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in parts {
        match *part {
            Part::Text(text) => bytes.extend_from_slice(text.as_bytes()),
            Part::Keyword(keyword) => bytes.push(keyword.token),
        }
    }
    bytes
}

const _: () = assert!(z::TEXT_A == z::OFFSET_A + 1);

pub fn emit(a: &mut Asm) {
    let messages: Vec<Vec<u8>> = ERRORS.iter().map(|&(_, _, parts)| message(parts)).collect();
    let blocks: Vec<(&str, u8, &[u8])> = ERRORS
        .iter()
        .zip(&messages)
        .map(|(&(name, number, _), text)| (name, number, text.as_slice()))
        .collect();
    a.error_run(&blocks);

    // BRKV: the message on a line of its own, at the line being run if a
    // program runs, then the prompt.
    a.here("error");
    a.cld();
    a.ldx(imm(0xFF));
    a.txs();
    a.inx();
    a.stx(zp(z::ASM_QUIET));
    // In borrowed text the statement pointer is back from its mark first,
    // the stack below the mark still as the error left it.
    let plain = a.local();
    a.ldx(zp(z::BORROWED));
    a.beq(plain);
    a.ldy(imm(3));
    let restore = a.here_local();
    a.lda(abs(STACK_PAGE + 2).x());
    a.sta(abs(z::OFFSET_A - 1).y());
    a.inx();
    a.dey();
    a.bne(restore);
    a.sty(zp(z::BORROWED));
    a.bind(plain);
    a.jsr("current_line");
    a.stx(zp(z::ERL));
    a.sty(zp(z::ERL + 1));
    // ON ERROR's statements run unless the error is fatal, with every loop,
    // PROC and FN the error left behind dropped and READ from the first
    // DATA, as BASIC II runs them.
    a.jsr("clear_stacks");
    a.ldy(imm(0));
    a.lda(zp(ERROR_POINTER).ind_y());
    let reported = a.local();
    a.beq(reported);
    a.lda(zp(z::ON_ERROR + 1));
    a.beq(reported);
    a.sta(zp(z::TEXT_A + 1));
    a.lda(zp(z::ON_ERROR));
    a.sta(zp(z::TEXT_A));
    a.sty(zp(z::OFFSET_A));
    a.jmp("statement");
    a.bind(reported);
    a.jsr("report");
    a.lda(zp(z::ERL));
    a.ora(zp(z::ERL + 1));
    let shown = a.local();
    a.beq(shown);
    a.ldx(imm(super::AT_LINE));
    a.jsr("print_message");
    a.ldx(zp(z::ERL));
    a.ldy(zp(z::ERL + 1));
    a.jsr("print_unsigned");
    a.bind(shown);
    a.jsr(entry::OSNEWL);
    a.jmp("prompt_checked");

    // REPORT: a new line, then the last error's message, printed and
    // counted as PRINT prints, a top-bit byte spelt as its keyword.
    a.here("report");
    a.jsr("print_newline");
    a.ldy(imm(1));
    let next = a.here_local();
    a.lda(zp(ERROR_POINTER).ind_y());
    a.beq(RTS);
    a.sty(zp(TOKEN_KEPT));
    a.jsr("print_byte");
    a.ldy(zp(TOKEN_KEPT));
    a.iny();
    a.bne(next);
    a.rts();
}
