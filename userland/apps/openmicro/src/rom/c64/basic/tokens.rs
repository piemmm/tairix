//! BASIC V2's keywords in token order, as the Programmer's Reference Guide
//! lists them, and the tables that dispatch statements and functions.

use crate::rom::asm::{hi, lo, sym, Asm};

/// The tokens the interpreter tests for.
pub mod token {
    pub const END: u8 = 0x80;
    pub const FOR: u8 = 0x81;
    pub const DATA: u8 = 0x83;
    pub const GOTO: u8 = 0x89;
    pub const GOSUB: u8 = 0x8D;
    pub const REM: u8 = 0x8F;
    pub const PRINT: u8 = 0x99;
    /// The last statement's token.
    pub const NEW: u8 = 0xA2;
    pub const TAB: u8 = 0xA3;
    pub const TO: u8 = 0xA4;
    pub const FN: u8 = 0xA5;
    pub const SPC: u8 = 0xA6;
    pub const THEN: u8 = 0xA7;
    pub const NOT: u8 = 0xA8;
    pub const STEP: u8 = 0xA9;
    pub const PLUS: u8 = 0xAA;
    pub const MINUS: u8 = 0xAB;
    pub const OR: u8 = 0xB0;
    pub const GREATER: u8 = 0xB1;
    pub const EQUAL: u8 = 0xB2;
    /// The first function's token.
    pub const SGN: u8 = 0xB4;
    pub const FRE: u8 = 0xB8;
    pub const POS: u8 = 0xB9;
    pub const LEN: u8 = 0xC3;
    pub const VAL: u8 = 0xC5;
    pub const ASC: u8 = 0xC6;
    pub const LEFT: u8 = 0xC8;
    /// The last function's token.
    pub const MID: u8 = 0xCA;
    pub const GO: u8 = 0xCB;
    /// π, its own character.
    pub const PI: u8 = 0xFF;
}

/// The keywords from token `$80`.
pub const KEYWORDS: [&str; 76] = [
    "END", "FOR", "NEXT", "DATA", "INPUT#", "INPUT", "DIM", "READ", "LET", "GOTO", "RUN", "IF",
    "RESTORE", "GOSUB", "RETURN", "REM", "STOP", "ON", "WAIT", "LOAD", "SAVE", "VERIFY", "DEF",
    "POKE", "PRINT#", "PRINT", "CONT", "LIST", "CLR", "CMD", "SYS", "OPEN", "CLOSE", "GET", "NEW",
    "TAB(", "TO", "FN", "SPC(", "THEN", "NOT", "STEP", "+", "-", "*", "/", "^", "AND", "OR", ">",
    "=", "<", "SGN", "INT", "ABS", "USR", "FRE", "POS", "SQR", "RND", "LOG", "EXP", "COS", "SIN",
    "TAN", "ATN", "PEEK", "LEN", "STR$", "VAL", "ASC", "CHR$", "LEFT$", "RIGHT$", "MID$", "GO",
];

const _: () = {
    assert!(
        KEYWORDS.len() == (token::GO - 0x80 + 1) as usize,
        "a keyword for every token"
    );
    let mut total = 0;
    let mut index = 0;
    while index < KEYWORDS.len() {
        assert!(
            !KEYWORDS[index].is_empty(),
            "every keyword has a last character"
        );
        total += KEYWORDS[index].len();
        index += 1;
    }
    assert!(total < 256, "the table is reached by one index register");
};

/// The statements' routines, from token `$80` to [`token::NEW`].
const STATEMENTS: [&str; 35] = [
    "st_end",
    "st_for",
    "st_next",
    "st_data",
    "st_input_file",
    "st_input",
    "st_dim",
    "st_read",
    "st_let",
    "goto_line",
    "st_run",
    "st_if",
    "restore_data",
    "st_gosub",
    "st_return",
    "st_rem",
    "st_stop",
    "st_on",
    "st_wait",
    "st_load",
    "st_save",
    "st_verify",
    "st_def",
    "st_poke",
    "st_print_file",
    "st_print",
    "st_cont",
    "st_list",
    "st_clr",
    "st_cmd",
    "st_sys",
    "st_open",
    "st_close",
    "st_get",
    "st_new",
];

/// The functions' routines, from [`token::SGN`] to [`token::MID`].
const FUNCTIONS: [&str; 23] = [
    "fn_sgn", "fn_int", "fn_abs", "fn_usr", "fn_fre", "fn_pos", "fn_sqr", "fn_rnd", "fn_log",
    "fn_exp", "fn_cos", "fn_sin", "fn_tan", "fn_atn", "fn_peek", "fn_len", "fn_str", "fn_val",
    "fn_asc", "fn_chr", "fn_left", "fn_right", "fn_mid",
];

const _: () = assert!(STATEMENTS.len() == (token::NEW - 0x80 + 1) as usize);
const _: () = assert!(FUNCTIONS.len() == (token::MID - token::SGN + 1) as usize);

pub fn emit(a: &mut Asm) {
    a.later(|a| {
        a.here("keywords");
        for keyword in KEYWORDS {
            let text = keyword.as_bytes();
            let last = text.len() - 1;
            a.bytes(&text[..last]);
            a.byte(text[last] | 0x80);
        }
        a.byte(0);
    });
    a.later(|a| {
        // Each routine's address less one, which RTS dispatches to.
        a.here("statement_low");
        for name in STATEMENTS {
            a.byte(lo(sym(name) + -1));
        }
        a.here("statement_high");
        for name in STATEMENTS {
            a.byte(hi(sym(name) + -1));
        }
    });
    a.later(|a| {
        a.here("function_low");
        for name in FUNCTIONS {
            a.byte(lo(sym(name) + -1));
        }
        a.here("function_high");
        for name in FUNCTIONS {
            a.byte(hi(sym(name) + -1));
        }
    });
}
