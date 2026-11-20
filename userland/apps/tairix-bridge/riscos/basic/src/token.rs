//! The tokens the interpreter meets, as bytes to match on: each the one
//! `lib/openbasic` defines, never a second copy of its value.

use tairix_openbasic::basic5;
use tairix_openbasic::keyword::{word, Token, Word, PSEUDO_LHS};

/// The byte of a two-byte keyword after its prefix.
const fn second(word: Word) -> u8 {
    match word.token {
        Token::Two(_, b) | Token::One(b) => b,
    }
}

macro_rules! one {
    ($($name:ident = $word:expr;)*) => {
        $(
            #[doc = concat!("`", stringify!($name), "`.")]
            pub(crate) const $name: u8 = $word.token;
        )*
    };
}

one! {
    AND = word::AND;
    DIV = word::DIV;
    EOR = word::EOR;
    MOD = word::MOD;
    OR = word::OR;
    ERROR = word::ERROR;
    LINE = word::LINE;
    OFF = word::OFF;
    STEP = word::STEP;
    SPC = word::SPC;
    TAB = word::TAB;
    ELSE = word::ELSE;
    THEN = word::THEN;
    OPENIN = word::OPENIN;
    PTR = word::PTR;
    PAGE = word::PAGE;
    TIME = word::TIME;
    LOMEM = word::LOMEM;
    HIMEM = word::HIMEM;
    ABS = word::ABS;
    ACS = word::ACS;
    ADVAL = word::ADVAL;
    ASC = word::ASC;
    ASN = word::ASN;
    ATN = word::ATN;
    BGET = word::BGET;
    COS = word::COS;
    COUNT = word::COUNT;
    DEG = word::DEG;
    ERL = word::ERL;
    ERR = word::ERR;
    EVAL = word::EVAL;
    EXP = word::EXP;
    EXT = word::EXT;
    FALSE = word::FALSE;
    FN = word::FN;
    GET = word::GET;
    INKEY = word::INKEY;
    INSTR = word::INSTR;
    INT = word::INT;
    LEN = word::LEN;
    LN = word::LN;
    LOG = word::LOG;
    NOT = word::NOT;
    OPENUP = word::OPENUP;
    OPENOUT = word::OPENOUT;
    PI = word::PI;
    POINT = word::POINT;
    POS = word::POS;
    RAD = word::RAD;
    RND = word::RND;
    SGN = word::SGN;
    SIN = word::SIN;
    SQR = word::SQR;
    TAN = word::TAN;
    TO = word::TO;
    TRUE = word::TRUE;
    USR = word::USR;
    VAL = word::VAL;
    VPOS = word::VPOS;
    CHR = word::CHR;
    GET_STRING = word::GET_STRING;
    INKEY_STRING = word::INKEY_STRING;
    LEFT = word::LEFT;
    MID = word::MID;
    RIGHT = word::RIGHT;
    STR = word::STR;
    STRING = word::STRING;
    EOF = word::EOF;
    SOUND = word::SOUND;
    BPUT = word::BPUT;
    CALL = word::CALL;
    CHAIN = word::CHAIN;
    CLEAR = word::CLEAR;
    CLOSE = word::CLOSE;
    CLG = word::CLG;
    CLS = word::CLS;
    DATA = word::DATA;
    DEF = word::DEF;
    DIM = word::DIM;
    DRAW = word::DRAW;
    END = word::END;
    ENDPROC = word::ENDPROC;
    ENVELOPE = word::ENVELOPE;
    FOR = word::FOR;
    GOSUB = word::GOSUB;
    GOTO = word::GOTO;
    GCOL = word::GCOL;
    IF = word::IF;
    INPUT = word::INPUT;
    LET = word::LET;
    LOCAL = word::LOCAL;
    MODE = word::MODE;
    MOVE = word::MOVE;
    NEXT = word::NEXT;
    ON = word::ON;
    VDU = word::VDU;
    PLOT = word::PLOT;
    PRINT = word::PRINT;
    PROC = word::PROC;
    READ = word::READ;
    REM = word::REM;
    REPEAT = word::REPEAT;
    REPORT = word::REPORT;
    RESTORE = word::RESTORE;
    RETURN = word::RETURN;
    RUN = word::RUN;
    STOP = word::STOP;
    COLOUR = word::COLOUR;
    TRACE = word::TRACE;
    UNTIL = word::UNTIL;
    WIDTH = word::WIDTH;
    OSCLI = word::OSCLI;
    OTHERWISE = basic5::OTHERWISE;
    WHEN = basic5::WHEN;
    OF = basic5::OF;
    ENDCASE = basic5::ENDCASE;
    ELSE_BLOCK = basic5::ELSE_BLOCK;
    ENDIF = basic5::ENDIF;
    ENDWHILE = basic5::ENDWHILE;
}

/// A line number held in a line: the marker before its three bytes.
pub(crate) const LINE_NUMBER: u8 = tairix_openbasic::lineref::MARKER;

/// A pseudo-variable's token at a statement's start, where it is assigned.
pub(crate) const fn assigned(token: u8) -> u8 {
    token + PSEUDO_LHS
}

/// `PTR` assigned.
pub(crate) const PTR_LHS: u8 = assigned(PTR);
/// `PAGE` assigned.
pub(crate) const PAGE_LHS: u8 = assigned(PAGE);
/// `TIME` assigned.
pub(crate) const TIME_LHS: u8 = assigned(TIME);
/// `LOMEM` assigned.
pub(crate) const LOMEM_LHS: u8 = assigned(LOMEM);
/// `HIMEM` assigned.
pub(crate) const HIMEM_LHS: u8 = assigned(HIMEM);

/// The functions' prefix.
pub(crate) const FUNCTION: u8 = basic5::FUNCTION;
/// The commands' prefix.
pub(crate) const COMMAND: u8 = basic5::COMMAND;
/// The statements' prefix.
pub(crate) const STATEMENT: u8 = basic5::STATEMENT;

/// BASIC's token table as the module's code holds it for `TOKENADDR`: each
/// token's spelling, ended by its token's first byte — `&7F` or above —
/// the one-byte tokens in order, then each prefix's two-byte tokens.
pub(crate) mod table {
    use alloc::vec::Vec;

    use tairix_openbasic::basic5::{PREFIXES, SPELLINGS};
    use tairix_openbasic::keyword::Token;

    /// The second bytes a two-byte token may have.
    const SECONDS: core::ops::Range<u8> = 0x8E..0xAE;

    /// Whether `b` is a two-byte token's prefix.
    pub(crate) fn is_prefix(b: u8) -> bool {
        PREFIXES.contains(&b)
    }

    /// Every token with a spelling, in the table's order.
    fn tokens() -> impl Iterator<Item = (Token, &'static str)> {
        let ones = (0x7F..=0xFF_u8).filter(|&b| !is_prefix(b)).map(Token::One);
        let twos = PREFIXES
            .into_iter()
            .flat_map(|p| SECONDS.map(move |b| Token::Two(p, b)));
        ones.chain(twos)
            .filter_map(|token| Some((token, SPELLINGS.of_token(token)?)))
    }

    /// The first byte of `token`.
    const fn first(token: Token) -> u8 {
        match token {
            Token::One(b) | Token::Two(b, _) => b,
        }
    }

    /// The table's bytes.
    pub(crate) fn bytes() -> Result<Vec<u8>, alloc::collections::TryReserveError> {
        let mut out = Vec::new();
        for (token, spelling) in tokens() {
            out.try_reserve(spelling.len() + 1)?;
            out.extend_from_slice(spelling.as_bytes());
            out.push(first(token));
        }
        Ok(out)
    }

    /// Where `token`'s spelling starts in the table, its second byte given
    /// for a two-byte token.
    pub(crate) fn offset_of(token: u8, second: Option<u8>) -> Option<u32> {
        let wanted = match second {
            Some(b) => Token::Two(token, b),
            None => Token::One(token),
        };
        let mut offset = 0_u32;
        for (token, spelling) in tokens() {
            if token == wanted {
                return Some(offset);
            }
            offset += u32::try_from(spelling.len()).ok()? + 1;
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::{bytes, offset_of};
        use crate::token::{PRINT, STATEMENT};

        #[test]
        fn a_spelling_ends_at_its_token_s_first_byte() {
            let table = bytes().expect("memory");
            let print = offset_of(PRINT, None).expect("PRINT") as usize;
            assert_eq!(&table[print..print + 6], b"PRINT\xF1");
            let sys =
                offset_of(STATEMENT, Some(crate::token::statement::SYS)).expect("SYS") as usize;
            assert_eq!(&table[sys..sys + 4], b"SYS\xC8");
            assert!(
                table.iter().filter(|&&b| b >= 0x7F).count() > 150,
                "a terminator each"
            );
        }
    }
}

/// The two-byte functions after [`FUNCTION`].
pub(crate) mod function {
    use super::{basic5, second};

    /// `SUM`.
    pub(crate) const SUM: u8 = second(basic5::SUM);
    /// `BEAT`.
    pub(crate) const BEAT: u8 = second(basic5::BEAT);
}

/// The two-byte commands after [`COMMAND`].
pub(crate) mod command {
    use super::{basic5, second};

    macro_rules! two {
        ($($name:ident;)*) => {
            $(
                #[doc = concat!("`", stringify!($name), "`.")]
                pub(crate) const $name: u8 = second(basic5::$name);
            )*
        };
    }

    two! {
        APPEND; AUTO; CRUNCH; DELETE; EDIT; HELP; LIST; LOAD; LVAR; NEW; OLD;
        RENUMBER; SAVE; TEXTLOAD; TEXTSAVE; INSTALL;
    }
}

/// The two-byte statements after [`STATEMENT`].
pub(crate) mod statement {
    use super::{basic5, second};

    macro_rules! two {
        ($($name:ident = $word:ident;)*) => {
            $(
                #[doc = concat!("`", stringify!($name), "`.")]
                pub(crate) const $name: u8 = second(basic5::$word);
            )*
        };
    }

    two! {
        CASE = CASE; CIRCLE = CIRCLE; FILL = FILL; ORIGIN = ORIGIN; POINT = POINT;
        RECTANGLE = RECTANGLE; SWAP = SWAP; WHILE = WHILE; WAIT = WAIT;
        MOUSE = MOUSE; QUIT = QUIT; SYS = SYS; INSTALL = INSTALL_STATEMENT;
        LIBRARY = LIBRARY; TINT = TINT; ELLIPSE = ELLIPSE; BEATS = BEATS;
        TEMPO = TEMPO; VOICES = VOICES; VOICE = VOICE; STEREO = STEREO;
        OVERLAY = OVERLAY;
    }
}
