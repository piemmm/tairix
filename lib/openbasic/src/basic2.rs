//! BBC BASIC II, the BBC Micro's and the Electron's: its own eight command
//! tokens, and every keyword in the order its tokeniser tries them, so a word
//! ending in `.` is the first keyword it begins.

use crate::keyword::{k, word, Flags, Keyword, Spellings, Table, Word};

/// `AUTO`.
pub const AUTO: Word<u8> = Word::one("AUTO", 0xC6);
/// `DELETE`.
pub const DELETE: Word<u8> = Word::one("DELETE", 0xC7);
/// `LOAD`.
pub const LOAD: Word<u8> = Word::one("LOAD", 0xC8);
/// `LIST`.
pub const LIST: Word<u8> = Word::one("LIST", 0xC9);
/// `NEW`.
pub const NEW: Word<u8> = Word::one("NEW", 0xCA);
/// `OLD`.
pub const OLD: Word<u8> = Word::one("OLD", 0xCB);
/// `RENUMBER`.
pub const RENUMBER: Word<u8> = Word::one("RENUMBER", 0xCC);
/// `SAVE`.
pub const SAVE: Word<u8> = Word::one("SAVE", 0xCD);

/// The highest line number a program holds.
pub const LAST_LINE: u16 = 32_767;

const C: u8 = Flags::CONDITIONAL;
const M: u8 = Flags::MIDDLE;
const S: u8 = Flags::START;
const L: u8 = Flags::LINE_NUMBER;
const F: u8 = Flags::NAME;
const R: u8 = Flags::RAW;
const P: u8 = Flags::PSEUDO;

/// Every keyword, in the order the tokeniser tries them: the User Guide's
/// tokens, and the flags its tokeniser keeps.
pub const KEYWORDS: [Keyword<u8>; 121] = [
    k(word::AND, 0),
    k(word::ABS, 0),
    k(word::ACS, 0),
    k(word::ADVAL, 0),
    k(word::ASC, 0),
    k(word::ASN, 0),
    k(word::ATN, 0),
    k(AUTO, L),
    k(word::BGET, C),
    k(word::BPUT, C | M),
    k(word::COLOUR, M),
    k(word::CALL, M),
    k(word::CHAIN, M),
    k(word::CHR, 0),
    k(word::CLEAR, C),
    k(word::CLOSE, C | M),
    k(word::CLG, C),
    k(word::CLS, C),
    k(word::COS, 0),
    k(word::COUNT, C),
    k(word::DATA, R),
    k(word::DEF, 0),
    k(word::DEG, 0),
    k(DELETE, L),
    k(word::DIM, M),
    k(word::DIV, 0),
    k(word::DRAW, M),
    k(word::ENDPROC, C),
    k(word::ELSE, S | L),
    k(word::END, C),
    k(word::ENVELOPE, M),
    k(word::EOF, C),
    k(word::EOR, 0),
    k(word::ERL, C),
    k(word::ERROR, S),
    k(word::ERR, C),
    k(word::EVAL, 0),
    k(word::EXP, 0),
    k(word::EXT, C),
    k(word::FOR, M),
    k(word::FALSE, C),
    k(word::FN, F),
    k(word::GOTO, M | L),
    k(word::GCOL, M),
    k(word::GET_STRING, 0),
    k(word::GET, 0),
    k(word::GOSUB, M | L),
    k(word::HIMEM, C | M | P),
    k(word::INPUT, M),
    k(word::IF, M),
    k(word::INKEY_STRING, 0),
    k(word::INKEY, 0),
    k(word::INSTR, 0),
    k(word::INT, 0),
    k(LIST, L),
    k(word::LEFT, 0),
    k(word::LEN, 0),
    k(word::LET, S),
    k(word::LINE, 0),
    k(word::LN, 0),
    k(LOAD, M),
    k(word::LOCAL, M),
    k(word::LOG, 0),
    k(word::LOMEM, C | M | P),
    k(word::MID, 0),
    k(word::MODE, M),
    k(word::MOD, 0),
    k(word::MOVE, M),
    k(word::NEXT, M),
    k(NEW, C),
    k(word::NOT, 0),
    k(OLD, C),
    k(word::OFF, 0),
    k(word::ON, M),
    k(word::OPENIN, 0),
    k(word::OPENOUT, 0),
    k(word::OPENUP, 0),
    k(word::OR, 0),
    k(word::OSCLI, M),
    k(word::PRINT, M),
    k(word::PAGE, C | M | P),
    k(word::PI, C),
    k(word::PLOT, M),
    k(word::POINT, 0),
    k(word::POS, C),
    k(word::PROC, M | F),
    k(word::PTR, C | M | P),
    k(word::RETURN, C),
    k(word::RAD, 0),
    k(word::READ, M),
    k(word::REM, R),
    k(RENUMBER, L),
    k(word::REPEAT, 0),
    k(word::REPORT, C),
    k(word::RESTORE, M | L),
    k(word::RIGHT, 0),
    k(word::RND, C),
    k(word::RUN, C),
    k(word::STEP, 0),
    k(SAVE, M),
    k(word::SGN, 0),
    k(word::SIN, 0),
    k(word::SOUND, M),
    k(word::SPC, 0),
    k(word::SQR, 0),
    k(word::STOP, C),
    k(word::STR, 0),
    k(word::STRING, 0),
    k(word::TAN, 0),
    k(word::TAB, 0),
    k(word::THEN, S | L),
    k(word::TIME, C | M | P),
    k(word::TO, 0),
    k(word::TRACE, M | L),
    k(word::TRUE, C),
    k(word::UNTIL, M),
    k(word::USR, 0),
    k(word::VDU, M),
    k(word::VAL, 0),
    k(word::VPOS, C),
    k(word::WIDTH, M),
];

/// Each token's spelling.
pub static SPELLINGS: Spellings = Spellings::of_bytes(&KEYWORDS);

pub(crate) static TABLE: Table<u8> = Table::of(&KEYWORDS);

#[cfg(test)]
#[path = "basic2_tests.rs"]
mod tests;
