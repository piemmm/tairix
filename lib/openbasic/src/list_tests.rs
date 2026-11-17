use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::tokenise::{tokenise, Start};

/// `program`'s lines, each typed with its number, listed under `listo`.
fn listed(dialect: Dialect, listo: u8, program: &[&str]) -> Result<String, Error> {
    let mut lister = Lister::new(dialect, Options::new(listo));
    let mut out = Vec::new();
    for typed in program {
        let mut bytes = Vec::new();
        tokenise(dialect, typed.as_bytes(), Start::Line, &mut bytes).unwrap();
        let number = lineref::decode([bytes[1], bytes[2], bytes[3]]);
        lister.line(number, &bytes[4..], &mut out)?;
    }
    Ok(String::from_utf8(out).unwrap())
}

#[test]
fn a_line_is_its_number_in_five_columns_then_its_text_spelt_out() {
    for dialect in [Dialect::BasicII, Dialect::BasicV] {
        assert_eq!(
            listed(dialect, 0, &["10PRINT\"A\";", "20 GOTO 10", "65000 P."]),
            Ok(String::from(
                "   10PRINT\"A\";\n   20 GOTO 10\n65000 PRINT\n"
            ))
        );
        assert_eq!(
            listed(dialect, 1, &["10TIME=0"]),
            Ok(String::from("   10 TIME=0\n")),
            "a space after the number, an assigning token spelt as its own"
        );
    }
}

#[test]
fn basic_ii_indents_lines_inside_its_loops() {
    let program = [
        "10FOR I=1 TO 2",
        "20REPEAT",
        "30PRINT I",
        "40UNTIL TRUE",
        "50NEXT",
        "60END",
    ];
    assert_eq!(
        listed(Dialect::BasicII, 2, &program),
        Ok(String::from(
            "   10FOR I=1 TO 2\n   20  REPEAT\n   30  PRINT I\n   40  UNTIL TRUE\n   50  NEXT\n   60END\n"
        )),
        "FOR's loops only; a closing line keeps its loop's indent"
    );
    assert_eq!(
        listed(Dialect::BasicII, 7, &program),
        Ok(String::from(
            "   10 FOR I=1 TO 2\n   20   REPEAT\n   30     PRINT I\n   40     UNTIL TRUE\n   50   NEXT\n   60 END\n"
        ))
    );
}

#[test]
fn basic_v_indents_structures_with_their_opening_and_closing_lines_outside() {
    let program = [
        "10FOR I=1 TO 2",
        "20IF I=1 THEN",
        "30PRINT I",
        "40ELSE",
        "50WHILE FALSE:ENDWHILE",
        "60ENDIF",
        "70NEXT",
    ];
    assert_eq!(
        listed(Dialect::BasicV, 2, &program),
        Ok(String::from(
            "   10FOR I=1 TO 2\n   20  IF I=1 THEN\n   30    PRINT I\n   40    ELSE\n   50    WHILE FALSE:ENDWHILE\n   60  ENDIF\n   70NEXT\n"
        ))
    );
}

#[test]
fn basic_v_splits_statements_hides_numbers_and_lowers_keywords() {
    assert_eq!(
        listed(Dialect::BasicV, 4, &["10PRINT 1:PRINT \"a:b\":REM x:y"]),
        Ok(String::from(
            "   10PRINT 1:\n     PRINT \"a:b\":\n     REM x:y\n"
        ))
    );
    assert_eq!(
        listed(Dialect::BasicV, 16 | 8, &["10PRINT TIME"]),
        Ok(String::from("print time\n"))
    );
    let mut lister = Lister::new(Dialect::BasicV, Options::new(Options::NO_NUMBERS));
    let mut out = Vec::new();
    let mut goto = Vec::new();
    tokenise(Dialect::BasicV, b"GOTO 100", Start::Statement, &mut goto).unwrap();
    assert_eq!(
        lister.line(10, &goto, &mut out),
        Err(Error::ReferenceListed)
    );
    assert_eq!(out, b"GOTO 100\n");
}

#[test]
fn a_top_bit_byte_in_a_string_is_a_keyword_to_basic_ii_and_itself_to_basic_v() {
    let line = [b'"', 0xF1, b'"', 0xF4, 0xF1];
    let list = |dialect| {
        let mut out = Vec::new();
        Lister::new(dialect, Options::default())
            .line(1, &line, &mut out)
            .unwrap();
        out
    };
    assert_eq!(list(Dialect::BasicII), b"    1\"PRINT\"REMPRINT\n");
    assert_eq!(list(Dialect::BasicV), b"    1\"\xF1\"REM\xF1\n");
}

#[test]
fn stored_and_unknown_tokens_list_as_basic_v_lists_them() {
    let mut out = Vec::new();
    let line = [0xCC, b' ', 0xC8, 0x9A, b' ', 0xC8, 0xFF, 0xC7];
    Lister::new(Dialect::BasicV, Options::default())
        .line(5, &line, &mut out)
        .unwrap();
    assert_eq!(out, b"    5ELSE INSTALL \n");
}

/// The indent `lister` gives a last line after `lines`, each listed first.
fn indent_after(lister: &mut Lister, lines: &[&[u8]]) -> usize {
    let mut out = Vec::new();
    for line in lines {
        lister.line(1, line, &mut out).unwrap();
    }
    out.clear();
    lister.line(2, b"X", &mut out).unwrap();
    out.iter().skip(5).take_while(|&&b| b == b' ').count()
}

/// Observed of BASIC II through the reference harness.
#[test]
fn basic_ii_indents_two_spaces_a_loop_but_one_for_128_or_more_of_a_kind() {
    let fors = |n| [word::FOR.token].repeat(n);
    let repeats = |n| [word::REPEAT.token].repeat(n);
    let both = Options::new(Options::FOR_LOOPS | Options::REPEAT_LOOPS);
    let mut lister = Lister::new(Dialect::BasicII, both);
    assert_eq!(
        indent_after(&mut lister, &[&fors(129), &[word::NEXT.token; 2]]),
        254,
        "127 open"
    );
    let mut lister = Lister::new(Dialect::BasicII, both);
    assert_eq!(
        indent_after(&mut lister, &[&fors(171), &[word::NEXT.token; 2]]),
        1,
        "169 open"
    );
    let mut lister = Lister::new(Dialect::BasicII, both);
    assert_eq!(
        indent_after(&mut lister, &[&fors(100), &repeats(40)]),
        280,
        "each kind in full"
    );
    let mut lister = Lister::new(Dialect::BasicII, both);
    assert_eq!(
        indent_after(&mut lister, &[&fors(130), &repeats(10)]),
        21,
        "one for the 130"
    );
    let mut lister = Lister::new(
        Dialect::BasicII,
        Options::new(Options::SPACE | Options::FOR_LOOPS | Options::REPEAT_LOOPS),
    );
    let mut out = Vec::new();
    for line in [fors(127), repeats(127)] {
        lister.line(1, &line, &mut out).unwrap();
    }
    out.clear();
    lister.line(65535, b"X", &mut out).unwrap();
    assert_eq!(
        out.len(),
        MOST_LISTED_BEFORE + 1,
        "the deepest line, at its bound"
    );
    let mut lister = Lister::new(Dialect::BasicII, both);
    assert_eq!(
        indent_after(&mut lister, &[&fors(250), &fors(50)]),
        88,
        "300 open, 44 counted in a byte"
    );
    let mut lister = Lister::new(
        Dialect::BasicII,
        Options::new(Options::FOR_LOOPS | Options::SPACE),
    );
    assert_eq!(
        indent_after(&mut lister, &[&[word::NEXT.token], &fors(1)]),
        3,
        "none below none"
    );
}

#[test]
fn basic_v_indents_by_its_count_s_low_byte_and_none_below_zero() {
    let fors = [word::FOR.token].repeat(130);
    let mut lister = Lister::new(Dialect::BasicV, Options::new(Options::INDENT));
    assert_eq!(indent_after(&mut lister, &[&fors]), 4, "260 is 4 in a byte");
    let mut lister = Lister::new(Dialect::BasicV, Options::new(Options::INDENT));
    let mut out = Vec::new();
    lister
        .line(1, &[word::NEXT.token, word::NEXT.token], &mut out)
        .unwrap();
    assert_eq!(out, b"    1NEXTNEXT\n", "below zero indents none");
    assert_eq!(
        indent_after(&mut lister, &[]),
        0,
        "and the count starts again from none"
    );
}

#[test]
fn basic_v_takes_a_line_s_length_for_the_byte_before_its_first() {
    let opener = |length: usize| {
        let mut text = Vec::from([0x95]);
        text.resize(length, b' ');
        let mut lister = Lister::new(Dialect::BasicV, Options::new(Options::INDENT));
        indent_after(&mut lister, &[&text])
    };
    assert_eq!(
        opener(usize::from(basic5::STATEMENT) - program::HEADER),
        2,
        "WHILE's second byte"
    );
    assert_eq!(opener(100), 0);
}

#[test]
fn a_line_lists_to_its_first_cr() {
    for dialect in [Dialect::BasicII, Dialect::BasicV] {
        let mut out = alloc::vec::Vec::new();
        Lister::new(dialect, Options::new(0))
            .line(10, &[word::PRINT.token, b'\r', word::GOTO.token], &mut out)
            .unwrap();
        assert_eq!(out, b"   10PRINT\n", "{dialect:?}: nothing past it");
    }
}
