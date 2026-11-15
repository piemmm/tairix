//! BBC BASIC V and VI, RISC OS's: the tokens BASIC II does not have, the
//! two-byte tokens behind the prefixes `&C6` (functions), `&C7` (commands) and
//! `&C8` (statements), and every keyword in the order its tokeniser tries
//! them. BASIC VI tokenises and lists as BASIC V does.

use crate::keyword::{k, word, Flags, Keyword, Spellings, Table, Token, Word};

/// `OTHERWISE`.
pub const OTHERWISE: Word<u8> = Word::one("OTHERWISE", 0x7F);
/// `WHEN`.
pub const WHEN: Word<u8> = Word::one("WHEN", 0xC9);
/// `OF`.
pub const OF: Word<u8> = Word::one("OF", 0xCA);
/// `ENDCASE`.
pub const ENDCASE: Word<u8> = Word::one("ENDCASE", 0xCB);
/// `ELSE` starting a line, which ends a block `IF`'s `THEN` part; the
/// tokeniser gives [`word::ELSE`] and a stored line's first `ELSE` becomes it.
pub const ELSE_BLOCK: Word<u8> = Word::one("ELSE", 0xCC);
/// `ENDIF`.
pub const ENDIF: Word<u8> = Word::one("ENDIF", 0xCD);
/// `ENDWHILE`.
pub const ENDWHILE: Word<u8> = Word::one("ENDWHILE", 0xCE);
/// `COLOR`, read as `COLOUR` and listed so.
pub const COLOR: Word<u8> = Word::one("COLOR", word::COLOUR.token);

/// The functions' prefix.
pub const FUNCTION: u8 = 0xC6;
/// The commands' prefix.
pub const COMMAND: u8 = 0xC7;
/// The statements' prefix.
pub const STATEMENT: u8 = 0xC8;

/// The prefixes, in the order a spelling index keeps them.
pub const PREFIXES: [u8; 3] = [FUNCTION, COMMAND, STATEMENT];

macro_rules! words {
    ($($name:ident = $spelling:literal $prefix:ident $token:literal;)*) => {
        $(
            #[doc = concat!("`", $spelling, "`.")]
            pub const $name: Word = Word::two($spelling, $prefix, $token);
        )*
    };
}

words! {
    SUM = "SUM" FUNCTION 0x8E;
    BEAT = "BEAT" FUNCTION 0x8F;
    APPEND = "APPEND" COMMAND 0x8E;
    AUTO = "AUTO" COMMAND 0x8F;
    CRUNCH = "CRUNCH" COMMAND 0x90;
    DELETE = "DELETE" COMMAND 0x91;
    EDIT = "EDIT" COMMAND 0x92;
    HELP = "HELP" COMMAND 0x93;
    LIST = "LIST" COMMAND 0x94;
    LOAD = "LOAD" COMMAND 0x95;
    LVAR = "LVAR" COMMAND 0x96;
    NEW = "NEW" COMMAND 0x97;
    OLD = "OLD" COMMAND 0x98;
    RENUMBER = "RENUMBER" COMMAND 0x99;
    SAVE = "SAVE" COMMAND 0x9A;
    TEXTLOAD = "TEXTLOAD" COMMAND 0x9B;
    TEXTSAVE = "TEXTSAVE" COMMAND 0x9C;
    TWIN = "TWIN" COMMAND 0x9D;
    TWINO = "TWINO" COMMAND 0x9E;
    INSTALL = "INSTALL" COMMAND 0x9F;
    CASE = "CASE" STATEMENT 0x8E;
    CIRCLE = "CIRCLE" STATEMENT 0x8F;
    FILL = "FILL" STATEMENT 0x90;
    ORIGIN = "ORIGIN" STATEMENT 0x91;
    POINT = "POINT" STATEMENT 0x92;
    RECTANGLE = "RECTANGLE" STATEMENT 0x93;
    SWAP = "SWAP" STATEMENT 0x94;
    WHILE = "WHILE" STATEMENT 0x95;
    WAIT = "WAIT" STATEMENT 0x96;
    MOUSE = "MOUSE" STATEMENT 0x97;
    QUIT = "QUIT" STATEMENT 0x98;
    SYS = "SYS" STATEMENT 0x99;
    INSTALL_STATEMENT = "INSTALL" STATEMENT 0x9A;
    LIBRARY = "LIBRARY" STATEMENT 0x9B;
    TINT = "TINT" STATEMENT 0x9C;
    ELLIPSE = "ELLIPSE" STATEMENT 0x9D;
    BEATS = "BEATS" STATEMENT 0x9E;
    TEMPO = "TEMPO" STATEMENT 0x9F;
    VOICES = "VOICES" STATEMENT 0xA0;
    VOICE = "VOICE" STATEMENT 0xA1;
    STEREO = "STEREO" STATEMENT 0xA2;
    OVERLAY = "OVERLAY" STATEMENT 0xA3;
}

/// The highest line number a program holds.
pub const LAST_LINE: u16 = 65_279;

/// A one-byte keyword as this table holds it.
const fn one(word: Word<u8>, flags: u8) -> Keyword {
    k(word.either(), flags)
}

/// A two-byte keyword.
const fn two(word: Word, flags: u8) -> Keyword {
    k(word, flags)
}

const C: u8 = Flags::CONDITIONAL;
const M: u8 = Flags::MIDDLE;
const S: u8 = Flags::START;
const L: u8 = Flags::LINE_NUMBER;
const R: u8 = Flags::RAW;
const P: u8 = Flags::PSEUDO;
const B: u8 = Flags::BRACKET;

/// Every keyword, in the order the tokeniser tries them, each first letter's
/// together: the first that matches whole, or that the input leaves at a
/// `.`, is the one. `FN` and `PROC` take the name after them by token.
pub const KEYWORDS: [Keyword; 161] = [
    one(word::AND, M),
    one(word::ABS, 0),
    one(word::ACS, 0),
    one(word::ADVAL, 0),
    one(word::ASC, 0),
    one(word::ASN, 0),
    one(word::ATN, 0),
    two(AUTO, L),
    two(APPEND, M),
    one(word::BGET, C),
    one(word::BPUT, C | M),
    two(BEATS, M),
    two(BEAT, M),
    one(word::COLOUR, M),
    one(word::CALL, M),
    two(CASE, M),
    one(word::CHAIN, M),
    one(word::CHR, 0),
    one(word::CLEAR, C),
    one(word::CLOSE, C | M),
    one(word::CLG, C),
    one(word::CLS, C),
    one(word::COS, 0),
    one(word::COUNT, C),
    two(CIRCLE, M),
    two(CRUNCH, M),
    one(COLOR, M),
    one(word::DATA, R),
    one(word::DEG, 0),
    one(word::DEF, 0),
    two(DELETE, L),
    one(word::DIV, 0),
    one(word::DIM, M),
    one(word::DRAW, M),
    one(word::ENDPROC, C),
    two(EDIT, R),
    one(ENDWHILE, C),
    one(ENDCASE, C),
    one(ENDIF, C),
    one(word::END, C),
    one(word::ENVELOPE, M),
    one(word::ELSE, S | L),
    one(word::EVAL, 0),
    one(word::ERL, C),
    one(word::ERROR, S),
    one(word::EOF, C),
    one(word::EOR, M),
    one(word::ERR, C),
    one(word::EXP, 0),
    one(word::EXT, C),
    two(ELLIPSE, M),
    one(word::FOR, M),
    one(word::FALSE, C),
    two(FILL, M),
    one(word::FN, M),
    one(word::GOTO, M | L),
    one(word::GET_STRING, 0),
    one(word::GET, 0),
    one(word::GOSUB, M | L),
    one(word::GCOL, M),
    one(word::HIMEM, C | M | P),
    two(HELP, C),
    one(word::INPUT, M),
    one(word::IF, M),
    one(word::INKEY_STRING, 0),
    one(word::INKEY, 0),
    one(word::INT, 0),
    one(word::INSTR, B),
    two(INSTALL, M),
    two(LIST, L),
    one(word::LINE, M),
    two(LOAD, M),
    one(word::LOMEM, C | M | P),
    one(word::LOCAL, M),
    one(word::LEFT, B),
    one(word::LEN, 0),
    one(word::LET, S),
    one(word::LOG, 0),
    one(word::LN, 0),
    two(LIBRARY, M),
    two(LVAR, C),
    one(word::MID, B),
    one(word::MODE, M),
    one(word::MOD, 0),
    one(word::MOVE, M),
    two(MOUSE, M),
    one(word::NEXT, M),
    two(NEW, C),
    one(word::NOT, 0),
    two(OLD, C),
    one(word::ON, M),
    one(word::OFF, 0),
    one(OF, 0),
    two(ORIGIN, M),
    one(word::OR, M),
    one(word::OPENIN, 0),
    one(word::OPENOUT, 0),
    one(word::OPENUP, 0),
    one(word::OSCLI, M),
    one(OTHERWISE, S),
    two(OVERLAY, M),
    one(word::PRINT, M),
    one(word::PAGE, C | M | P),
    one(word::PTR, C | M | P),
    one(word::PI, C),
    one(word::PLOT, M),
    one(word::POINT, B),
    two(POINT, M),
    one(word::PROC, M),
    one(word::POS, C),
    two(QUIT, M),
    one(word::RETURN, C),
    one(word::REPEAT, 0),
    one(word::REPORT, C),
    one(word::READ, M),
    one(word::REM, R),
    one(word::RUN, C),
    one(word::RAD, 0),
    one(word::RESTORE, M | L),
    one(word::RIGHT, B),
    one(word::RND, C),
    two(RECTANGLE, M),
    two(RENUMBER, L),
    one(word::STEP, 0),
    two(SAVE, M),
    one(word::SGN, 0),
    one(word::SIN, 0),
    one(word::SQR, 0),
    one(word::SOUND, M),
    one(word::SPC, 0),
    one(word::STR, 0),
    one(word::STRING, B),
    one(word::STOP, C),
    two(STEREO, M),
    two(SUM, M),
    two(SWAP, M),
    two(SYS, M),
    one(word::TAN, 0),
    one(word::TAB, B),
    two(TEMPO, M),
    two(TEXTLOAD, M),
    two(TEXTSAVE, M),
    one(word::THEN, S | L),
    one(word::TIME, C | M | P),
    two(TINT, M),
    one(word::TO, 0),
    one(word::TRACE, M | L),
    one(word::TRUE, C),
    two(TWINO, M),
    two(TWIN, C),
    one(word::UNTIL, M),
    one(word::USR, 0),
    one(word::VDU, M),
    one(word::VAL, 0),
    one(word::VPOS, C),
    two(VOICES, M),
    two(VOICE, M),
    two(WHILE, M),
    one(WHEN, M),
    two(WAIT, C),
    one(word::WIDTH, M),
];

/// Each token's spelling, the two a program can hold that no line is
/// tokenised to among them.
pub static SPELLINGS: Spellings =
    Spellings::of(&KEYWORDS, &[ELSE_BLOCK.either(), INSTALL_STATEMENT]);

pub(crate) static TABLE: Table<Token> = Table::of(&KEYWORDS);

#[cfg(test)]
#[path = "basic5_tests.rs"]
mod tests;
