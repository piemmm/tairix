//! The keyword table: each keyword's spelling, token and tokenising flags, in
//! the order an abbreviation is resolved — a word ending in `.` is the first
//! keyword it begins.
//!
//! Tokens are the User Guide's; the flags are what the tokeniser does
//! around each word.

/// What the tokeniser does around a keyword.
pub mod flag {
    /// Not a keyword when a letter, digit or `_` follows it, so `TIMER` is a
    /// name.
    pub const CONDITIONAL: u8 = 0x01;
    /// What follows is the middle of a statement.
    pub const MIDDLE: u8 = 0x02;
    /// What follows starts a statement.
    pub const START: u8 = 0x04;
    /// Numbers that follow are line numbers, encoded.
    pub const LINE_NUMBER: u8 = 0x08;
    /// A name follows, left as it is typed.
    pub const NAME: u8 = 0x10;
    /// The rest of the line is left as it is typed.
    pub const RAW: u8 = 0x20;
    /// A pseudo-variable, whose token at a statement's start is `+ &40`.
    pub const PSEUDO: u8 = 0x40;
}

/// A pseudo-variable's token at a statement's start, where it is assigned.
pub const PSEUDO_LHS: u8 = 0x40;
/// An encoded line number: this token and three bytes.
pub const LINE_REFERENCE: u8 = 0x8D;

/// One keyword.
#[derive(Clone, Copy, Debug)]
pub struct Keyword {
    pub name: &'static str,
    pub token: u8,
    pub flags: u8,
}

const C: u8 = flag::CONDITIONAL;
const M: u8 = flag::MIDDLE;
const S: u8 = flag::START;
const L: u8 = flag::LINE_NUMBER;
const F: u8 = flag::NAME;
const R: u8 = flag::RAW;
const P: u8 = flag::PSEUDO;

const fn k(name: &'static str, token: u8, flags: u8) -> Keyword {
    Keyword { name, token, flags }
}

/// Every keyword, in resolution order.
pub const KEYWORDS: [Keyword; 121] = [
    k("AND", 0x80, 0),
    k("ABS", 0x94, 0),
    k("ACS", 0x95, 0),
    k("ADVAL", 0x96, 0),
    k("ASC", 0x97, 0),
    k("ASN", 0x98, 0),
    k("ATN", 0x99, 0),
    k("AUTO", 0xC6, L),
    k("BGET", 0x9A, C),
    k("BPUT", 0xD5, C | M),
    k("COLOUR", 0xFB, M),
    k("CALL", 0xD6, M),
    k("CHAIN", 0xD7, M),
    k("CHR$", 0xBD, 0),
    k("CLEAR", 0xD8, C),
    k("CLOSE", 0xD9, C | M),
    k("CLG", 0xDA, C),
    k("CLS", 0xDB, C),
    k("COS", 0x9B, 0),
    k("COUNT", 0x9C, C),
    k("DATA", 0xDC, R),
    k("DEF", 0xDD, 0),
    k("DEG", 0x9D, 0),
    k("DELETE", 0xC7, L),
    k("DIM", 0xDE, M),
    k("DIV", 0x81, 0),
    k("DRAW", 0xDF, M),
    k("ENDPROC", 0xE1, C),
    k("ELSE", 0x8B, S | L),
    k("END", 0xE0, C),
    k("ENVELOPE", 0xE2, M),
    k("EOF", 0xC5, C),
    k("EOR", 0x82, 0),
    k("ERL", 0x9E, C),
    k("ERROR", 0x85, S),
    k("ERR", 0x9F, C),
    k("EVAL", 0xA0, 0),
    k("EXP", 0xA1, 0),
    k("EXT", 0xA2, C),
    k("FOR", 0xE3, M),
    k("FALSE", 0xA3, C),
    k("FN", 0xA4, F),
    k("GOTO", 0xE5, M | L),
    k("GCOL", 0xE6, M),
    k("GET$", 0xBE, 0),
    k("GET", 0xA5, 0),
    k("GOSUB", 0xE4, M | L),
    k("HIMEM", 0x93, C | M | P),
    k("INPUT", 0xE8, M),
    k("IF", 0xE7, M),
    k("INKEY$", 0xBF, 0),
    k("INKEY", 0xA6, 0),
    k("INSTR(", 0xA7, 0),
    k("INT", 0xA8, 0),
    k("LIST", 0xC9, L),
    k("LEFT$(", 0xC0, 0),
    k("LEN", 0xA9, 0),
    k("LET", 0xE9, S),
    k("LINE", 0x86, 0),
    k("LN", 0xAA, 0),
    k("LOAD", 0xC8, M),
    k("LOCAL", 0xEA, M),
    k("LOG", 0xAB, 0),
    k("LOMEM", 0x92, C | M | P),
    k("MID$(", 0xC1, 0),
    k("MODE", 0xEB, M),
    k("MOD", 0x83, 0),
    k("MOVE", 0xEC, M),
    k("NEXT", 0xED, M),
    k("NEW", 0xCA, C),
    k("NOT", 0xAC, 0),
    k("OLD", 0xCB, C),
    k("OFF", 0x87, 0),
    k("ON", 0xEE, M),
    k("OPENIN", 0x8E, 0),
    k("OPENOUT", 0xAE, 0),
    k("OPENUP", 0xAD, 0),
    k("OR", 0x84, 0),
    k("OSCLI", 0xFF, M),
    k("PRINT", 0xF1, M),
    k("PAGE", 0x90, C | M | P),
    k("PI", 0xAF, C),
    k("PLOT", 0xF0, M),
    k("POINT(", 0xB0, 0),
    k("POS", 0xB1, C),
    k("PROC", 0xF2, M | F),
    k("PTR", 0x8F, C | M | P),
    k("RETURN", 0xF8, C),
    k("RAD", 0xB2, 0),
    k("READ", 0xF3, M),
    k("REM", 0xF4, R),
    k("RENUMBER", 0xCC, L),
    k("REPEAT", 0xF5, 0),
    k("REPORT", 0xF6, C),
    k("RESTORE", 0xF7, M | L),
    k("RIGHT$(", 0xC2, 0),
    k("RND", 0xB3, C),
    k("RUN", 0xF9, C),
    k("STEP", 0x88, 0),
    k("SAVE", 0xCD, M),
    k("SGN", 0xB4, 0),
    k("SIN", 0xB5, 0),
    k("SOUND", 0xD4, M),
    k("SPC", 0x89, 0),
    k("SQR", 0xB6, 0),
    k("STOP", 0xFA, C),
    k("STR$", 0xC3, 0),
    k("STRING$(", 0xC4, 0),
    k("TAN", 0xB7, 0),
    k("TAB(", 0x8A, 0),
    k("THEN", 0x8C, S | L),
    k("TIME", 0x91, C | M | P),
    k("TO", 0xB8, 0),
    k("TRACE", 0xFC, M | L),
    k("TRUE", 0xB9, C),
    k("UNTIL", 0xFD, M),
    k("USR", 0xBA, 0),
    k("VDU", 0xEF, M),
    k("VAL", 0xBB, 0),
    k("VPOS", 0xBC, C),
    k("WIDTH", 0xFE, M),
];

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
