use alloc::vec::Vec;

use super::*;
use crate::vars::Store;

fn vars() -> Store {
    let mut vars = Store::with_room(16, 1024, 1024).unwrap();
    vars.assign(b"Name", Value::String(b"Fred".to_vec()))
        .unwrap();
    vars.assign(b"Raw", Value::String(b"a b\r<x>|M".to_vec()))
        .unwrap();
    vars.assign(b"Count", Value::Number(-42)).unwrap();
    vars.assign(b"Greet", Value::Macro(b"Hi <Name>|J".to_vec()))
        .unwrap();
    vars
}

/// `text` translated with `flags`, or its error.
fn trans(text: &[u8], flags: Flags) -> Result<Vec<u8>, Error> {
    translated(text, flags, &mut vars(), &mut Budget::new(1000), 4096).map(|(bytes, _)| bytes)
}

fn plain(text: &[u8]) -> Vec<u8> {
    trans(text, Flags::NONE).unwrap()
}

#[test]
fn every_escape_reads_as_the_table_says() {
    assert_eq!(
        plain(b"|@|A|a|Z|z|[|{|\\|]|}|^|~|_|`"),
        [0, 1, 1, 26, 26, 27, 27, 28, 29, 29, 30, 30, 31, 31]
    );
    assert_eq!(plain(b"|\"|||<|?"), b"\"|<\x7F");
    assert_eq!(
        plain(b"|1|#| x"),
        b"1# x",
        "other printable bytes stand for themselves"
    );
    assert_eq!(plain(b"|\xE9"), [0xC9], "a top-bit byte has bit 5 flipped");
    assert_eq!(plain(b"|\x7F"), [0x7F]);
    for bad in [&b"ab|"[..], b"|\x01", b"|\r"] {
        assert_eq!(trans(bad, Flags::NONE), Err(Error::BadString), "{bad:?}");
    }
    assert_eq!(trans(b"|M", Flags::NO_ESCAPES), Ok(b"|M".to_vec()));
}

#[test]
fn the_top_bit_escape_marks_the_next_character_however_it_is_made() {
    assert_eq!(plain(b"|!A"), [0xC1]);
    assert_eq!(plain(b"|!|@"), [0x80]);
    assert_eq!(plain(b"|!|?"), [0xFF]);
    assert_eq!(plain(b"|!|!A"), [0xC1]);
    assert_eq!(plain(b"|!<65>"), [0xC1]);
    assert_eq!(plain(b"|!<Name>"), b"\xC6red", "only a value's first byte");
    assert_eq!(
        plain(b"|!<Unset>x"),
        [0xF8],
        "an empty expansion passes it on"
    );
    assert_eq!(plain(b"|!"), b"", "the end drops it");
    assert_eq!(trans(b"|!\x01", Flags::NONE), Err(Error::BadString));
}

#[test]
fn quotes_open_a_string_only_at_its_start_and_spaces_end_it_only_outside() {
    assert_eq!(plain(b"  \"a  b\"rest"), b"a  b");
    assert_eq!(plain(b"\"say \"\"hi\"\"\""), b"say \"hi\"");
    assert_eq!(plain(b"ab\"c"), b"ab\"c");
    assert_eq!(trans(b"\"open", Flags::NONE), Err(Error::BadString));
    assert_eq!(trans(b"one two", Flags::SPACE_ENDS), Ok(b"one".to_vec()));
    assert_eq!(
        trans(b"\"one two\" three", Flags::SPACE_ENDS),
        Ok(b"one two".to_vec())
    );
    assert_eq!(trans(b"\"a\"", Flags::NO_QUOTES), Ok(b"\"a\"".to_vec()));
}

#[test]
fn a_reference_is_a_character_code_or_a_variable_up_to_its_bracket() {
    assert_eq!(plain(b"<65><&42><2_1000011>"), b"AB\x43");
    assert_eq!(plain(b"<321>"), b"A", "the low byte is stored");
    assert_eq!(plain(b"<Name> and <name>"), b"Fred and Fred");
    assert_eq!(
        plain(b"<N#me>"),
        b"Fred",
        "a wildcard finds the first match"
    );
    assert_eq!(plain(b"[<Unset>]"), b"[]");
    assert_eq!(plain(b"<> < x> <a b> <open"), b"<> < x> <a b> <open");
    assert_eq!(
        plain(b"<<Name>>"),
        b">",
        "a name is everything up to its bracket: `<Name`"
    );
    assert_eq!(plain(b"<65a>"), b"", "not a number: a variable, unset");
    let long = [b'x'; 256];
    let mut text = Vec::from(&b"<"[..]);
    text.extend_from_slice(&long);
    text.push(b'>');
    assert_eq!(plain(&text).len(), 258, "too long to be a reference");
}

#[test]
fn each_type_is_delivered_as_its_type_says() {
    assert_eq!(plain(b"<Raw>"), b"a b\r<x>|M", "a string as it is");
    assert_eq!(
        trans(b"<Raw>!", Flags::SPACE_ENDS),
        Ok(b"a b\r<x>|M!".to_vec())
    );
    assert_eq!(plain(b"<Count>"), b"-42");
    assert_eq!(plain(b"<Greet>"), b"Hi Fred\n", "a macro read again");
    assert_eq!(
        trans(b"<Greet>x", Flags::SPACE_ENDS),
        Ok(b"Hi Fred\nx".to_vec())
    );
}

#[test]
fn a_reading_expands_no_more_than_its_budget_or_the_active_bound() {
    let mut store = vars();
    store
        .assign(b"Me", Value::Macro(b"a<Me>".to_vec()))
        .unwrap();
    let (bytes, _) = translated(
        b"<Me>",
        Flags::NONE,
        &mut store,
        &mut Budget::new(1000),
        4096,
    )
    .unwrap();
    assert_eq!(
        bytes, [b'a'; MOST_ACTIVE],
        "one more than the bound expands to nothing"
    );
    let mut tight = Budget::new(3);
    assert_eq!(
        translated(
            b"<Name><Name><Name><Name>",
            Flags::NONE,
            &mut store,
            &mut tight,
            4096
        ),
        Err(Error::ExpansionTooComplex)
    );
    assert_eq!(tight.left(), 0);
}

#[test]
fn gstrans_stores_the_terminator_and_overflows_a_byte_early() {
    let mut store = vars();
    let mut buffer = [0xAAu8; 8];
    let done = gstrans(
        b"<Name> x\r",
        Flags::SPACE_ENDS,
        &mut store,
        &mut Budget::new(10),
        &mut buffer,
    );
    assert_eq!(done.stored, 4);
    assert_eq!(done.ended, Ok(Some(b' ')));
    assert_eq!(&buffer[..5], b"Fred ");
    assert_eq!(done.next, Some(7), "past the space");
    let mut exact = [0u8; 4];
    let full = gstrans(
        b"Fred",
        Flags::NONE,
        &mut store,
        &mut Budget::new(10),
        &mut exact,
    );
    assert_eq!(
        (full.stored, full.ended),
        (4, Ok(None)),
        "no room for the terminator"
    );
    let none = gstrans(b"x", Flags::NONE, &mut store, &mut Budget::new(10), &mut []);
    assert_eq!((none.stored, none.ended), (0, Ok(None)));
    let quoted = gstrans(
        b"\"a\"  b",
        Flags::NONE,
        &mut store,
        &mut Budget::new(10),
        &mut buffer,
    );
    assert_eq!(
        (quoted.stored, quoted.ended, quoted.next),
        (1, Ok(Some(b'b')), Some(5))
    );
    store
        .assign(b"Closes", Value::Macro(b"a\"b".to_vec()))
        .unwrap();
    let inside = gstrans(
        b"\"<Closes>\"\r",
        Flags::NONE,
        &mut store,
        &mut Budget::new(10),
        &mut buffer,
    );
    assert_eq!(
        (inside.stored, inside.ended, inside.next),
        (1, Ok(Some(b'b')), None),
        "the string ends inside the macro, which is no place in the text"
    );
    let bad = gstrans(
        b"ab|",
        Flags::NONE,
        &mut store,
        &mut Budget::new(10),
        &mut buffer,
    );
    assert_eq!((bad.stored, bad.ended), (2, Err(Error::BadString)));
}

#[test]
fn gsinit_answers_the_first_character_and_whether_the_string_is_empty() {
    let quoted = Reader::new(b"  \"x\"", Flags::NONE);
    assert_eq!(quoted.first(), b'"');
    assert!(!quoted.is_empty());
    assert!(Reader::new(b"   \r", Flags::NONE).is_empty());
    assert!(Reader::new(b"", Flags::NONE).is_empty());
    let plain = Reader::new(b" \"x\"", Flags::NO_QUOTES);
    assert_eq!(plain.first(), b'"');
    let mut store = vars();
    let mut reader = Reader::new(b"<65>", Flags::NONE);
    assert_eq!(
        reader.read(&mut store, &mut Budget::new(1)),
        Ok(Read::Char(65))
    );
    let mut wide = Reader::new(b"<&12345678>", Flags::NONE);
    assert_eq!(
        wide.read(&mut store, &mut Budget::new(1)),
        Ok(Read::Char(0x1234_5678)),
        "OS_GSRead answers the whole number"
    );
}

#[test]
fn a_wildcard_reference_costs_a_reference_for_each_variable_it_may_examine() {
    let mut vars = Store::with_room(8, 64, 4).unwrap();
    for name in [&b"Ab"[..], b"Ac", b"B"] {
        vars.assign(name, Value::String(b"1".to_vec())).unwrap();
    }
    assert_eq!(
        vars.assign(b"Abcde", Value::String(b"1".to_vec())),
        Err(Error::NoRoomForVariable),
        "a name past the bound"
    );
    assert_eq!((vars.cost(b"A*"), vars.cost(b"Ab")), (3, 1));
    assert_eq!(
        (vars.lookup(b"*abcde*"), vars.cost(b"*abcde*")),
        (Ok(None), 1),
        "more literal bytes than any name holds, matched against none"
    );
    let three = translated(b"<A*>", Flags::NONE, &mut vars, &mut Budget::new(3), 64);
    assert_eq!(three.map(|(bytes, _)| bytes), Ok(b"1".to_vec()));
    assert_eq!(
        translated(b"<A*>", Flags::NONE, &mut vars, &mut Budget::new(2), 64),
        Err(Error::ExpansionTooComplex)
    );
}
