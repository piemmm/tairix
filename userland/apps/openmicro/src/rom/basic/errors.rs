//! Errors: each a BRK with its number and message that a failing routine
//! jumps to, and BRKV's handler, which reports it and returns to the prompt.

use super::ws::zp as z;
use crate::rom::asm::{imm, zp, Asm, RTS};
use crate::rom::os::{entry, ERROR_POINTER};

/// Each error's routine, number and message, as the User Guide gives them.
const ERRORS: [(&str, u8, &str); 46] = [
    ("err_no_room", 0, "No room"),
    ("err_out_of_range", 1, "Out of range"),
    ("err_byte", 2, "Byte"),
    ("err_index", 3, "Index"),
    ("err_stop", 0, "STOP"),
    ("err_silly", 0, "Silly"),
    ("err_mistake", 4, "Mistake"),
    ("err_missing_comma", 5, "Missing ,"),
    ("err_type_mismatch", 6, "Type mismatch"),
    ("err_no_fn", 7, "No FN"),
    ("err_missing_quote", 9, "Missing \""),
    ("err_bad_dim", 10, "Bad DIM"),
    ("err_dim_space", 11, "DIM space"),
    ("err_not_local", 12, "Not LOCAL"),
    ("err_no_proc", 13, "No PROC"),
    ("err_array", 14, "Array"),
    ("err_subscript", 15, "Subscript"),
    ("err_syntax", 16, "Syntax error"),
    ("err_escape", 17, "Escape"),
    ("err_division_by_zero", 18, "Division by zero"),
    ("err_string_too_long", 19, "String too long"),
    ("err_too_big", 20, "Too big"),
    ("err_negative_root", 21, "-ve root"),
    ("err_log_range", 22, "Log range"),
    ("err_accuracy_lost", 23, "Accuracy lost"),
    ("err_exp_range", 24, "Exp range"),
    ("err_bad_mode", 25, "Bad MODE"),
    ("err_no_such_variable", 26, "No such variable"),
    ("err_missing_bracket", 27, "Missing )"),
    ("err_bad_hex", 28, "Bad HEX"),
    ("err_no_such_fn_proc", 29, "No such FN/PROC"),
    ("err_arguments", 31, "Arguments"),
    ("err_no_for", 32, "No FOR"),
    ("err_cant_match_for", 33, "Can't match FOR"),
    ("err_for_variable", 34, "FOR variable"),
    ("err_too_many_fors", 35, "Too many FORs"),
    ("err_no_to", 36, "No TO"),
    ("err_too_many_gosubs", 37, "Too many GOSUBs"),
    ("err_no_gosub", 38, "No GOSUB"),
    ("err_on_syntax", 39, "ON syntax"),
    ("err_on_range", 40, "ON range"),
    ("err_no_such_line", 41, "No such line"),
    ("err_out_of_data", 42, "Out of DATA"),
    ("err_no_repeat", 43, "No REPEAT"),
    ("err_too_many_repeats", 44, "Too many REPEATs"),
    ("err_missing_hash", 45, "Missing #"),
];

pub fn emit(a: &mut Asm) {
    for (routine, number, message) in ERRORS {
        a.here(routine);
        a.error(number, message);
    }

    // BRKV: the message on a line of its own, at the line being run if a
    // program runs, then the prompt.
    a.here("error");
    a.cld();
    a.ldx(imm(0xFF));
    a.txs();
    a.inx();
    a.stx(zp(z::ASM_QUIET));
    a.jsr("current_line");
    a.stx(zp(z::ERL));
    a.sty(zp(z::ERL + 1));
    // ON ERROR's statements run unless the error is fatal.
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
    // counted as PRINT prints.
    a.here("report");
    a.jsr("print_newline");
    a.ldy(imm(1));
    let next = a.here_local();
    a.lda(zp(ERROR_POINTER).ind_y());
    a.beq(RTS);
    a.jsr("print_char");
    a.iny();
    a.bne(next);
    a.rts();
}
