use alloc::vec::Vec;

use super::*;
use crate::keyword::Token;
use crate::lineref::MARKER;
use crate::tokenise::{tokenise, Start};
use crate::Dialect;

/// The token the text `typed` makes as an expression.
fn token_of(typed: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    tokenise(
        Dialect::BasicV,
        typed.as_bytes(),
        Start::Expression,
        &mut bytes,
    )
    .unwrap();
    bytes
}

#[test]
fn every_token_a_program_can_hold_has_one_spelling() {
    for byte in 0x7F..=0xFFu8 {
        let spelt = SPELLINGS.of_token(Token::One(byte));
        let reserved = byte == MARKER || PREFIXES.contains(&byte);
        assert_eq!(spelt.is_none(), reserved, "&{byte:02X}");
    }
    for (prefix, seconds) in [
        (FUNCTION, 0x8E..=0x8F),
        (COMMAND, 0x8E..=0x9F),
        (STATEMENT, 0x8E..=0xA3),
    ] {
        for second in seconds {
            assert!(
                SPELLINGS.of_token(Token::Two(prefix, second)).is_some(),
                "&{prefix:02X} &{second:02X}"
            );
        }
        assert_eq!(SPELLINGS.of_token(Token::Two(prefix, 0xA4)), None);
        assert_eq!(SPELLINGS.of_token(Token::Two(prefix, 0x8D)), None);
    }
}

#[test]
fn a_token_lists_as_its_first_keyword_and_the_stored_forms_as_theirs() {
    let spelt = |token| SPELLINGS.of_token(token);
    assert_eq!(
        spelt(Token::One(word::COLOUR.token)),
        Some("COLOUR"),
        "not COLOR"
    );
    assert_eq!(spelt(Token::One(ELSE_BLOCK.token)), Some("ELSE"));
    assert_eq!(spelt(INSTALL_STATEMENT.token), Some("INSTALL"));
    assert_eq!(
        spelt(Token::One(0xCF)),
        Some("PTR"),
        "PTR's assigning token"
    );
    assert_eq!(spelt(Token::One(0xD3)), Some("HIMEM"));
    assert_eq!(spelt(Token::One(0x7F)), Some("OTHERWISE"));
}

#[test]
fn an_abbreviation_is_the_first_keyword_in_its_letter_s_run_it_begins() {
    for (typed, spelt) in [
        ("A.", "AND"),
        ("B.", "BGET"),
        ("C.", "COLOUR"),
        ("D.", "DATA"),
        ("E.", "ENDPROC"),
        ("F.", "FOR"),
        ("G.", "GOTO"),
        ("H.", "HIMEM"),
        ("I.", "INPUT"),
        ("L.", "LIST"),
        ("M.", "MID$("),
        ("N.", "NEXT"),
        ("O.", "OLD"),
        ("P.", "PRINT"),
        ("Q.", "QUIT"),
        ("R.", "RETURN"),
        ("S.", "STEP"),
        ("T.", "TAN"),
        ("U.", "UNTIL"),
        ("V.", "VDU"),
        ("W.", "WHILE"),
        ("DE.", "DEG"),
        ("DI.", "DIV"),
        ("OR.", "ORIGIN"),
        ("TW.", "TWINO"),
        ("ENDW.", "ENDWHILE"),
    ] {
        let bytes = token_of(typed);
        let token = match bytes[..] {
            [prefix, second] if PREFIXES.contains(&prefix) => Token::Two(prefix, second),
            [byte, ..] => Token::One(byte),
            [] => panic!("{typed} made nothing"),
        };
        assert_eq!(SPELLINGS.of_token(token), Some(spelt), "{typed}");
    }
}
