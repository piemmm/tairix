//! Keywords: a token, what a tokeniser does around one, the words both
//! dialects spell and number alike, and the index a lister spells tokens by.

use crate::basic5::PREFIXES;

/// A token: one byte, or one of BASIC V's prefixes and a byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// A single byte, `&7F` or above.
    One(u8),
    /// A prefix, `&C6`, `&C7` or `&C8`, and the byte after it.
    Two(u8, u8),
}

/// What a tokeniser does around a keyword, a bit each. The bits are the
/// ones BASIC II's ROM keeps in its table, so its builder emits them as they
/// are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(u8);

impl Flags {
    /// Not a keyword where a name character follows it, so `TIMER` is a name.
    pub const CONDITIONAL: u8 = 1;
    /// What follows is the middle of a statement.
    pub const MIDDLE: u8 = 1 << 1;
    /// What follows starts a statement.
    pub const START: u8 = 1 << 2;
    /// Numbers that follow are line numbers.
    pub const LINE_NUMBER: u8 = 1 << 3;
    /// A name follows, left as it is typed: BASIC II's `FN` and `PROC`.
    pub const NAME: u8 = 1 << 4;
    /// The rest of the line is left as it is typed.
    pub const RAW: u8 = 1 << 5;
    /// A pseudo-variable, whose token at a statement's start is
    /// [`PSEUDO_LHS`] higher.
    pub const PSEUDO: u8 = 1 << 6;
    /// Its spelling opens a bracket, which BASIC V counts.
    pub const BRACKET: u8 = 1 << 7;

    /// The flags `bits` hold.
    #[must_use]
    pub const fn new(bits: u8) -> Self {
        Self(bits)
    }

    /// The bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether `flag` is among them.
    #[must_use]
    pub const fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// The bit a pseudo-variable's token gains at a statement's start, where it
/// is assigned.
pub const PSEUDO_LHS: u8 = 0x40;

/// A keyword's spelling and token: `T` is `u8` where the token is one byte,
/// as each of BASIC II's is, and [`Token`] where it may be either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Word<T = Token> {
    /// The spelling, upper case, a bracket in it where the keyword has one.
    pub name: &'static str,
    /// The token.
    pub token: T,
}

impl Word<u8> {
    /// A single-byte keyword.
    #[must_use]
    pub const fn one(name: &'static str, token: u8) -> Self {
        Self { name, token }
    }

    /// The same keyword, its token as either kind.
    pub(crate) const fn either(self) -> Word {
        Word {
            name: self.name,
            token: Token::One(self.token),
        }
    }
}

impl Word {
    /// A two-byte keyword.
    #[must_use]
    pub const fn two(name: &'static str, prefix: u8, token: u8) -> Self {
        Self {
            name,
            token: Token::Two(prefix, token),
        }
    }
}

/// A keyword as one dialect's tokeniser takes it, `T` as its [`Word`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Keyword<T = Token> {
    /// Its spelling and token.
    pub word: Word<T>,
    /// What the tokeniser does around it.
    pub flags: Flags,
}

/// `word` with `flags`.
pub(crate) const fn k<T>(word: Word<T>, flags: u8) -> Keyword<T> {
    Keyword {
        word,
        flags: Flags(flags),
    }
}

/// A dialect's keywords in its tokeniser's order, and where each capital's
/// lie: a table keeps each letter's together, so a tokeniser tries only those
/// its text's letter begins.
pub(crate) struct Table<T: 'static> {
    keywords: &'static [Keyword<T>],
    runs: [(usize, usize); 26],
}

impl<T> Table<T> {
    /// The table of `keywords`.
    ///
    /// # Panics
    ///
    /// At compile time, where a keyword does not begin with a capital or a
    /// letter's keywords do not lie together.
    pub(crate) const fn of(keywords: &'static [Keyword<T>]) -> Self {
        let mut runs = [(0, 0); 26];
        let mut i = 0;
        while i < keywords.len() {
            let letter = keywords[i].word.name.as_bytes()[0];
            assert!(
                letter.is_ascii_uppercase(),
                "a keyword begins with a capital"
            );
            let run = &mut runs[(letter - b'A') as usize];
            if run.0 == run.1 {
                run.0 = i;
            } else {
                assert!(run.1 == i, "a letter's keywords lie together");
            }
            run.1 = i + 1;
            i += 1;
        }
        Self { keywords, runs }
    }

    /// The keywords that begin with `letter`, in order.
    pub(crate) fn beginning(&self, letter: u8) -> &'static [Keyword<T>] {
        let run = letter
            .checked_sub(b'A')
            .and_then(|i| self.runs.get(usize::from(i)));
        let Some(&(start, end)) = run else {
            return &[];
        };
        self.keywords.get(start..end).unwrap_or(&[])
    }
}

/// The keywords BASIC II and BASIC V spell and number alike.
pub mod word {
    use super::Word;

    macro_rules! words {
        ($($name:ident = $spelling:literal $token:literal;)*) => {
            $(
                #[doc = concat!("`", $spelling, "`.")]
                pub const $name: Word<u8> = Word::one($spelling, $token);
            )*
        };
    }

    words! {
        AND = "AND" 0x80;
        DIV = "DIV" 0x81;
        EOR = "EOR" 0x82;
        MOD = "MOD" 0x83;
        OR = "OR" 0x84;
        ERROR = "ERROR" 0x85;
        LINE = "LINE" 0x86;
        OFF = "OFF" 0x87;
        STEP = "STEP" 0x88;
        SPC = "SPC" 0x89;
        TAB = "TAB(" 0x8A;
        ELSE = "ELSE" 0x8B;
        THEN = "THEN" 0x8C;
        OPENIN = "OPENIN" 0x8E;
        PTR = "PTR" 0x8F;
        PAGE = "PAGE" 0x90;
        TIME = "TIME" 0x91;
        LOMEM = "LOMEM" 0x92;
        HIMEM = "HIMEM" 0x93;
        ABS = "ABS" 0x94;
        ACS = "ACS" 0x95;
        ADVAL = "ADVAL" 0x96;
        ASC = "ASC" 0x97;
        ASN = "ASN" 0x98;
        ATN = "ATN" 0x99;
        BGET = "BGET" 0x9A;
        COS = "COS" 0x9B;
        COUNT = "COUNT" 0x9C;
        DEG = "DEG" 0x9D;
        ERL = "ERL" 0x9E;
        ERR = "ERR" 0x9F;
        EVAL = "EVAL" 0xA0;
        EXP = "EXP" 0xA1;
        EXT = "EXT" 0xA2;
        FALSE = "FALSE" 0xA3;
        FN = "FN" 0xA4;
        GET = "GET" 0xA5;
        INKEY = "INKEY" 0xA6;
        INSTR = "INSTR(" 0xA7;
        INT = "INT" 0xA8;
        LEN = "LEN" 0xA9;
        LN = "LN" 0xAA;
        LOG = "LOG" 0xAB;
        NOT = "NOT" 0xAC;
        OPENUP = "OPENUP" 0xAD;
        OPENOUT = "OPENOUT" 0xAE;
        PI = "PI" 0xAF;
        POINT = "POINT(" 0xB0;
        POS = "POS" 0xB1;
        RAD = "RAD" 0xB2;
        RND = "RND" 0xB3;
        SGN = "SGN" 0xB4;
        SIN = "SIN" 0xB5;
        SQR = "SQR" 0xB6;
        TAN = "TAN" 0xB7;
        TO = "TO" 0xB8;
        TRUE = "TRUE" 0xB9;
        USR = "USR" 0xBA;
        VAL = "VAL" 0xBB;
        VPOS = "VPOS" 0xBC;
        CHR = "CHR$" 0xBD;
        GET_STRING = "GET$" 0xBE;
        INKEY_STRING = "INKEY$" 0xBF;
        LEFT = "LEFT$(" 0xC0;
        MID = "MID$(" 0xC1;
        RIGHT = "RIGHT$(" 0xC2;
        STR = "STR$" 0xC3;
        STRING = "STRING$(" 0xC4;
        EOF = "EOF" 0xC5;
        SOUND = "SOUND" 0xD4;
        BPUT = "BPUT" 0xD5;
        CALL = "CALL" 0xD6;
        CHAIN = "CHAIN" 0xD7;
        CLEAR = "CLEAR" 0xD8;
        CLOSE = "CLOSE" 0xD9;
        CLG = "CLG" 0xDA;
        CLS = "CLS" 0xDB;
        DATA = "DATA" 0xDC;
        DEF = "DEF" 0xDD;
        DIM = "DIM" 0xDE;
        DRAW = "DRAW" 0xDF;
        END = "END" 0xE0;
        ENDPROC = "ENDPROC" 0xE1;
        ENVELOPE = "ENVELOPE" 0xE2;
        FOR = "FOR" 0xE3;
        GOSUB = "GOSUB" 0xE4;
        GOTO = "GOTO" 0xE5;
        GCOL = "GCOL" 0xE6;
        IF = "IF" 0xE7;
        INPUT = "INPUT" 0xE8;
        LET = "LET" 0xE9;
        LOCAL = "LOCAL" 0xEA;
        MODE = "MODE" 0xEB;
        MOVE = "MOVE" 0xEC;
        NEXT = "NEXT" 0xED;
        ON = "ON" 0xEE;
        VDU = "VDU" 0xEF;
        PLOT = "PLOT" 0xF0;
        PRINT = "PRINT" 0xF1;
        PROC = "PROC" 0xF2;
        READ = "READ" 0xF3;
        REM = "REM" 0xF4;
        REPEAT = "REPEAT" 0xF5;
        REPORT = "REPORT" 0xF6;
        RESTORE = "RESTORE" 0xF7;
        RETURN = "RETURN" 0xF8;
        RUN = "RUN" 0xF9;
        STOP = "STOP" 0xFA;
        COLOUR = "COLOUR" 0xFB;
        TRACE = "TRACE" 0xFC;
        UNTIL = "UNTIL" 0xFD;
        WIDTH = "WIDTH" 0xFE;
        OSCLI = "OSCLI" 0xFF;
    }
}

/// The first second byte a prefixed token has.
const FIRST_SECOND: u8 = 0x8E;

/// The second bytes a prefixed token may have, from [`FIRST_SECOND`].
const SECONDS: usize = 32;

/// Each token's spelling in one dialect: the first keyword its table gives
/// for it, then any that only a listing spells.
#[derive(Debug)]
pub struct Spellings {
    one: [Option<&'static str>; 256],
    two: [[Option<&'static str>; SECONDS]; PREFIXES.len()],
}

const fn prefix_index(prefix: u8) -> Option<usize> {
    let mut i = 0;
    while i < PREFIXES.len() {
        if PREFIXES[i] == prefix {
            return Some(i);
        }
        i += 1;
    }
    None
}

impl Spellings {
    const EMPTY: Self = Self {
        one: [None; 256],
        two: [[None; SECONDS]; PREFIXES.len()],
    };

    /// The index of `table`'s spellings, each token one byte, built where a
    /// dialect's index is declared.
    pub(crate) const fn of_bytes(table: &[Keyword<u8>]) -> Self {
        let mut index = Self::EMPTY;
        let mut i = 0;
        while i < table.len() {
            let Keyword { word, flags } = table[i];
            index.add(word.name, word.token, flags);
            i += 1;
        }
        index
    }

    /// The index of `table`'s spellings, then `listed`'s, built where a
    /// dialect's index is declared.
    ///
    /// # Panics
    ///
    /// At compile time, where a pseudo-variable's token is two bytes or a
    /// two-byte token lies outside the prefixes' range.
    pub(crate) const fn of(table: &[Keyword], listed: &[Word]) -> Self {
        let mut index = Self::EMPTY;
        let mut i = 0;
        while i < table.len() + listed.len() {
            let (word, flags) = if i < table.len() {
                (table[i].word, table[i].flags)
            } else {
                (listed[i - table.len()], Flags(0))
            };
            match word.token {
                Token::One(byte) => index.add(word.name, byte, flags),
                Token::Two(prefix, byte) => {
                    assert!(!flags.has(Flags::PSEUDO), "a pseudo-variable of two bytes");
                    let Some(p) = prefix_index(prefix) else {
                        panic!("a prefix other than &C6, &C7 or &C8");
                    };
                    assert!(byte >= FIRST_SECOND && ((byte - FIRST_SECOND) as usize) < SECONDS);
                    let slot = &mut index.two[p][(byte - FIRST_SECOND) as usize];
                    if slot.is_none() {
                        *slot = Some(word.name);
                    }
                }
            }
            i += 1;
        }
        index
    }

    /// `name` for `byte` where nothing earlier spells it, and for a
    /// pseudo-variable's assigning token too, which lists as it does.
    const fn add(&mut self, name: &'static str, byte: u8, flags: Flags) {
        if self.one[byte as usize].is_none() {
            self.one[byte as usize] = Some(name);
        }
        if flags.has(Flags::PSEUDO) {
            assert!(
                byte & PSEUDO_LHS == 0,
                "a pseudo-variable's token with its assigning bit"
            );
            let assigning = (byte | PSEUDO_LHS) as usize;
            if self.one[assigning].is_none() {
                self.one[assigning] = Some(name);
            }
        }
    }

    /// The spelling of `token`, or `None` for one the dialect has none for.
    #[must_use]
    pub fn of_token(&self, token: Token) -> Option<&'static str> {
        match token {
            Token::One(byte) => self.one[usize::from(byte)],
            Token::Two(prefix, byte) => {
                let slot = usize::from(byte.checked_sub(FIRST_SECOND)?);
                *self.two[prefix_index(prefix)?].get(slot)?
            }
        }
    }
}
