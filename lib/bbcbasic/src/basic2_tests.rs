use alloc::vec::Vec;

use super::*;
use crate::keyword::PSEUDO_LHS;
use crate::lineref::MARKER;

/// The User Guide's shortest abbreviation of each keyword, before its `.`;
/// the whole name where it gives none.
const ABBREVIATIONS: [(&str, &str); 121] = [
    ("AND", "A"),
    ("ABS", "ABS"),
    ("ACS", "ACS"),
    ("ADVAL", "AD"),
    ("ASC", "ASC"),
    ("ASN", "ASN"),
    ("ATN", "ATN"),
    ("AUTO", "AU"),
    ("BGET", "B"),
    ("BPUT", "BP"),
    ("COLOUR", "C"),
    ("CALL", "CA"),
    ("CHAIN", "CH"),
    ("CHR$", "CHR"),
    ("CLEAR", "CL"),
    ("CLOSE", "CLO"),
    ("CLG", "CLG"),
    ("CLS", "CLS"),
    ("COS", "COS"),
    ("COUNT", "COU"),
    ("DATA", "D"),
    ("DEF", "DEF"),
    ("DEG", "DEG"),
    ("DELETE", "DEL"),
    ("DIM", "DIM"),
    ("DIV", "DIV"),
    ("DRAW", "DR"),
    ("ENDPROC", "E"),
    ("ELSE", "EL"),
    ("END", "END"),
    ("ENVELOPE", "ENV"),
    ("EOF", "EOF"),
    ("EOR", "EOR"),
    ("ERL", "ERL"),
    ("ERROR", "ERR"),
    ("ERR", "ERR"),
    ("EVAL", "EV"),
    ("EXP", "EXP"),
    ("EXT", "EXT"),
    ("FOR", "F"),
    ("FALSE", "FA"),
    ("FN", "FN"),
    ("GOTO", "G"),
    ("GCOL", "GC"),
    ("GET$", "GE"),
    ("GET", "GET"),
    ("GOSUB", "GOS"),
    ("HIMEM", "H"),
    ("INPUT", "I"),
    ("IF", "IF"),
    ("INKEY$", "INK"),
    ("INKEY", "INKEY"),
    ("INSTR(", "INS"),
    ("INT", "INT"),
    ("LIST", "L"),
    ("LEFT$(", "LE"),
    ("LEN", "LEN"),
    ("LET", "LET"),
    ("LINE", "LIN"),
    ("LN", "LN"),
    ("LOAD", "LO"),
    ("LOCAL", "LOC"),
    ("LOG", "LOG"),
    ("LOMEM", "LOM"),
    ("MID$(", "M"),
    ("MODE", "MO"),
    ("MOD", "MOD"),
    ("MOVE", "MOV"),
    ("NEXT", "N"),
    ("NEW", "NEW"),
    ("NOT", "NOT"),
    ("OLD", "O"),
    ("OFF", "OFF"),
    ("ON", "ON"),
    ("OPENIN", "OP"),
    ("OPENOUT", "OPENO"),
    ("OPENUP", "OPENUP"),
    ("OR", "OR"),
    ("OSCLI", "OS"),
    ("PRINT", "P"),
    ("PAGE", "PA"),
    ("PI", "PI"),
    ("PLOT", "PL"),
    ("POINT(", "PO"),
    ("POS", "POS"),
    ("PROC", "PRO"),
    ("PTR", "PT"),
    ("RETURN", "R"),
    ("RAD", "RAD"),
    ("READ", "REA"),
    ("REM", "REM"),
    ("RENUMBER", "REN"),
    ("REPEAT", "REP"),
    ("REPORT", "REPO"),
    ("RESTORE", "RES"),
    ("RIGHT$(", "RI"),
    ("RND", "RND"),
    ("RUN", "RUN"),
    ("STEP", "S"),
    ("SAVE", "SA"),
    ("SGN", "SGN"),
    ("SIN", "SIN"),
    ("SOUND", "SO"),
    ("SPC", "SPC"),
    ("SQR", "SQR"),
    ("STOP", "STO"),
    ("STR$", "STR"),
    ("STRING$(", "STRI"),
    ("TAN", "T"),
    ("TAB(", "TAB("),
    ("THEN", "TH"),
    ("TIME", "TI"),
    ("TO", "TO"),
    ("TRACE", "TR"),
    ("TRUE", "TRUE"),
    ("UNTIL", "U"),
    ("USR", "USR"),
    ("VDU", "V"),
    ("VAL", "VAL"),
    ("VPOS", "VP"),
    ("WIDTH", "W"),
];

/// The keyword `token` stands for, a pseudo-variable's either way.
fn keyword(token: u8) -> Option<&'static Keyword<u8>> {
    KEYWORDS.iter().find(|keyword| {
        let byte = keyword.word.token;
        byte == token || (keyword.flags.has(Flags::PSEUDO) && byte | PSEUDO_LHS == token)
    })
}

/// The keyword `prefix.` stands for: the first that begins with it.
fn abbreviated(prefix: &str) -> Option<&'static str> {
    KEYWORDS
        .iter()
        .find(|keyword| keyword.word.name.starts_with(prefix))
        .map(|keyword| keyword.word.name)
}

#[test]
fn every_token_but_the_line_reference_and_the_unassigned_names_one_keyword() {
    let assigned = (0x80..=0xFFu8)
        .filter(|&token| token != MARKER && token != 0xCE && !(0xCF..=0xD3).contains(&token));
    for token in assigned {
        let named = KEYWORDS
            .iter()
            .filter(|keyword| keyword.word.token == token)
            .count();
        assert_eq!(named, 1, "token {token:02X}");
    }
    assert_eq!(keyword(0xD1).map(|keyword| keyword.word.name), Some("TIME"));
    assert!(keyword(MARKER).is_none());
}

#[test]
fn each_abbreviation_is_the_user_guide_s_shortest() {
    assert_eq!(ABBREVIATIONS.len(), KEYWORDS.len());
    for (keyword, &(name, abbreviation)) in KEYWORDS.iter().zip(&ABBREVIATIONS) {
        let spelt = keyword.word.name;
        assert_eq!(spelt, name);
        let length = abbreviation.len();
        if length < spelt.len() {
            assert_eq!(abbreviated(abbreviation), Some(spelt), "{abbreviation}.");
        }
        // No shorter abbreviation reaches it, nor any that saves typing for a
        // word the guide gives none.
        let reach = if length < spelt.len() {
            length
        } else {
            spelt.len().saturating_sub(2) + 1
        };
        for prefix in (1..reach).map(|end| &spelt[..end]) {
            assert_ne!(abbreviated(prefix), Some(spelt), "{prefix}.");
        }
    }
}

#[test]
fn only_pseudo_variables_take_a_statement_token() {
    let pseudo: Vec<&str> = KEYWORDS
        .iter()
        .filter(|keyword| keyword.flags.has(Flags::PSEUDO))
        .map(|keyword| keyword.word.name)
        .collect();
    assert_eq!(pseudo, ["HIMEM", "LOMEM", "PAGE", "PTR", "TIME"]);
}
