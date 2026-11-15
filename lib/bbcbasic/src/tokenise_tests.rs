use alloc::vec::Vec;

use super::*;
use crate::basic5::{CASE, ORIGIN, WHILE};
use crate::keyword::Word;

/// A line reference to `n`, as its four bytes.
fn reference(n: u16) -> Vec<u8> {
    let mut bytes = Vec::from([lineref::MARKER]);
    bytes.extend_from_slice(&lineref::encode(n));
    bytes
}

/// What a line's bytes are made of: text as it is, a keyword's token, a
/// reference to a line, or one byte.
enum Part<'a> {
    Text(&'a str),
    Keyword(Token),
    Ref(u16),
    Byte(u8),
}

use Part::{Byte, Ref, Text};

/// A keyword, of either width, as a part.
trait Tokened {
    fn part(self) -> Part<'static>;
}

impl Tokened for Word<u8> {
    fn part(self) -> Part<'static> {
        Part::Keyword(Token::One(self.token))
    }
}

impl Tokened for Word {
    fn part(self) -> Part<'static> {
        Part::Keyword(self.token)
    }
}

fn w(word: impl Tokened) -> Part<'static> {
    word.part()
}

fn bytes(parts: &[Part<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        match part {
            Text(text) => out.extend_from_slice(text.as_bytes()),
            Part::Keyword(Token::One(b)) | Byte(b) => out.push(*b),
            Part::Keyword(Token::Two(p, b)) => out.extend_from_slice(&[*p, *b]),
            Ref(n) => out.extend(reference(*n)),
        }
    }
    out
}

fn line(dialect: Dialect, text: &str) -> Vec<u8> {
    tokenise(dialect, text.as_bytes(), Start::Line)
        .unwrap()
        .bytes
}

fn ii(text: &str) -> Vec<u8> {
    line(Dialect::BasicII, text)
}

fn v(text: &str) -> Vec<u8> {
    line(Dialect::BasicV, text)
}

#[test]
fn a_typed_line_s_number_and_keywords_are_tokenised() {
    for dialect in [Dialect::BasicII, Dialect::BasicV] {
        assert_eq!(
            line(dialect, "10PRINT\"A\";"),
            bytes(&[Ref(10), w(word::PRINT), Text("\"A\";")])
        );
        assert_eq!(
            line(dialect, "30GOTO 10,20\r junk"),
            bytes(&[
                Ref(30),
                w(word::GOTO),
                Text(" "),
                Ref(10),
                Text(","),
                Ref(20)
            ])
        );
        assert_eq!(
            line(dialect, "10 20"),
            bytes(&[Ref(10), Text(" "), Ref(20)]),
            "a second number too"
        );
        assert_eq!(line(dialect, "007"), bytes(&[Ref(7)]), "leading zeros go");
    }
}

#[test]
fn names_stay_names_and_a_keyword_ends_where_its_spelling_does() {
    for dialect in [Dialect::BasicII, Dialect::BasicV] {
        assert_eq!(
            line(dialect, "TIMER=TIME"),
            bytes(&[Text("TIMER="), w(word::TIME)])
        );
        assert_eq!(
            line(dialect, "PRINTER"),
            bytes(&[w(word::PRINT), Text("ER")])
        );
        assert_eq!(line(dialect, "ATO=5"), bytes(&[Text("ATO=5")]));
        assert_eq!(line(dialect, "XPRINT=3"), bytes(&[Text("XPRINT=3")]));
        assert_eq!(
            line(dialect, "print x"),
            bytes(&[Text("print x")]),
            "lower case is a name"
        );
        assert_eq!(
            line(dialect, "PROCdraw(FNsize)"),
            bytes(&[w(word::PROC), Text("draw("), w(word::FN), Text("size)")])
        );
    }
}

#[test]
fn strings_remarks_data_and_star_commands_stay_as_typed() {
    for dialect in [Dialect::BasicII, Dialect::BasicV] {
        assert_eq!(
            line(dialect, "PRINT\"PRINT\"PRINT"),
            bytes(&[w(word::PRINT), Text("\"PRINT\""), w(word::PRINT)])
        );
        assert_eq!(
            line(dialect, "REM PRINT 10"),
            bytes(&[w(word::REM), Text(" PRINT 10")])
        );
        assert_eq!(
            line(dialect, "DATA GOTO,1"),
            bytes(&[w(word::DATA), Text(" GOTO,1")])
        );
        assert_eq!(line(dialect, "*CAT PRINT"), bytes(&[Text("*CAT PRINT")]));
        assert_eq!(
            line(dialect, "X=2*PI"),
            bytes(&[Text("X=2*"), w(word::PI)]),
            "a star mid-statement"
        );
    }
}

#[test]
fn a_pseudo_variable_takes_its_assigning_token_where_a_statement_starts() {
    for dialect in [Dialect::BasicII, Dialect::BasicV] {
        assert_eq!(line(dialect, "TIME=0"), bytes(&[Byte(0xD1), Text("=0")]));
        assert_eq!(
            line(dialect, "X=TIME:PAGE=X"),
            bytes(&[Text("X="), w(word::TIME), Text(":"), Byte(0xD0), Text("=X")])
        );
        let evaluated = tokenise(dialect, b"TIME", Start::Expression).unwrap().bytes;
        assert_eq!(evaluated, bytes(&[w(word::TIME)]));
    }
}

#[test]
fn basic_ii_reads_as_its_rom_does() {
    assert_eq!(
        ii("A=&ff"),
        bytes(&[Text("A=&ff")]),
        "hex digits are upper case"
    );
    assert_eq!(
        ii("X=TI.X"),
        bytes(&[Text("X="), w(word::TIME), Text("X")]),
        "an abbreviation is not checked"
    );
    assert_eq!(
        ii("GOTO 70000,20"),
        bytes(&[w(word::GOTO), Text(" 70000,"), Ref(20)])
    );
    assert_eq!(
        ii("AUTO 100"),
        bytes(&[w(crate::basic2::AUTO), Text(" "), Ref(100)])
    );
    assert_eq!(ii("DE."), bytes(&[w(word::DEF)]));
    assert_eq!(
        ii("OR."),
        bytes(&[w(word::OR), Text(".")]),
        "OR spelt whole"
    );
    assert_eq!(
        ii("X=Y:DIM A(2)"),
        bytes(&[Text("X=Y:"), w(word::DIM), Text(" A(2)")])
    );
    assert_eq!(ii("PRINTAND"), bytes(&[w(word::PRINT), w(word::AND)]));
    let tokenised = tokenise(Dialect::BasicII, b"PRINT\"open", Start::Line).unwrap();
    assert_eq!(
        tokenised.warnings,
        Warnings::default(),
        "BASIC II warns of nothing"
    );
}

#[test]
fn basic_v_reads_as_risc_os_5_does() {
    assert_eq!(v("A=&ff"), bytes(&[Text("A=&ff")]));
    assert_eq!(
        v("TI.X"),
        bytes(&[Text("TI.X")]),
        "a conditional abbreviation is checked"
    );
    assert_eq!(v("COLOR 1"), bytes(&[w(word::COLOUR), Text(" 1")]));
    assert_eq!(v("OR."), bytes(&[w(ORIGIN)]));
    assert_eq!(
        v("CASE X OF"),
        bytes(&[w(CASE), Text(" X "), w(crate::basic5::OF)])
    );
    assert_eq!(
        v("WHILE TRUE"),
        bytes(&[w(WHILE), Text(" "), w(word::TRUE)])
    );
    assert_eq!(v("A=1\nB=2"), bytes(&[Text("A=1")]), "an LF ends the line");
}

#[test]
fn basic_v_takes_line_numbers_as_its_tokeniser_tests_show() {
    assert_eq!(
        v("IF I% 922 ELSE 920"),
        bytes(&[
            w(word::IF),
            Text(" I% 922 "),
            w(word::ELSE),
            Text(" "),
            Ref(920)
        ])
    );
    assert_eq!(v("RESTORE +5"), bytes(&[w(word::RESTORE), Text(" +5")]));
    assert_eq!(
        v("ON I% GOTO 910,911,912"),
        bytes(&[
            w(word::ON),
            Text(" I% "),
            w(word::GOTO),
            Text(" "),
            Ref(910),
            Text(","),
            Ref(911),
            Text(","),
            Ref(912)
        ])
    );
    assert_eq!(
        v("TRACE STEP 964"),
        bytes(&[
            w(word::TRACE),
            Text(" "),
            w(word::STEP),
            Text(" "),
            Ref(964)
        ])
    );
    assert_eq!(
        v("X=TRACE 100"),
        bytes(&[Text("X="), w(word::TRACE), Text(" 100")]),
        "TRACE's handle"
    );
    assert_eq!(
        v("GOTO 65279"),
        bytes(&[w(word::GOTO), Text(" "), Ref(65_279)])
    );
}

#[test]
fn vdu_spelt_whole_keeps_a_p_point_after_it_as_text() {
    assert_eq!(v("VDUP.S16"), bytes(&[w(word::VDU), Text("P.S16")]));
    assert_eq!(v("V.P."), bytes(&[w(word::VDU), w(word::PRINT)]));
    assert_eq!(
        v("VDU P."),
        bytes(&[w(word::VDU), Text(" "), w(word::PRINT)])
    );
    assert_eq!(v("VDUPR."), bytes(&[w(word::VDU), w(word::PRINT)]));
}

#[test]
fn basic_v_warns_of_brackets_a_number_too_big_and_an_open_string() {
    let warned = |text: &str| {
        tokenise(Dialect::BasicV, text.as_bytes(), Start::Line)
            .unwrap()
            .warnings
    };
    assert_eq!(
        warned("PRINT (1"),
        Warnings {
            brackets: true,
            ..Warnings::default()
        }
    );
    assert_eq!(
        warned("PRINT )("),
        Warnings {
            brackets: true,
            ..Warnings::default()
        },
        "a close first"
    );
    assert_eq!(
        warned("PRINT LEFT$(A$,1)"),
        Warnings::default(),
        "a keyword's bracket counts"
    );
    assert_eq!(warned("PRINT \"(\""), Warnings::default());
    assert_eq!(
        warned("GOTO 65280"),
        Warnings {
            line_number: true,
            ..Warnings::default()
        }
    );
    assert_eq!(
        warned("PRINT \"open"),
        Warnings {
            quote: true,
            ..Warnings::default()
        }
    );
    assert_eq!(warned("REM \"open"), Warnings::default());
    assert_eq!(
        v("GOTO 65280,20"),
        bytes(&[w(word::GOTO), Text(" 65280,20")])
    );
}
