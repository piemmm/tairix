use alloc::vec::Vec;

use super::*;

fn store() -> Store {
    Store::with_room(16, 64).unwrap()
}

fn budget() -> Budget {
    Budget::new(1000)
}

fn string(vars: &mut Store, name: &[u8]) -> Option<Vec<u8>> {
    match vars.lookup(name).unwrap()? {
        Value::String(bytes) | Value::Macro(bytes) => Some(bytes),
        Value::Number(n) => Some(crate::number::Decimal::of(n).as_bytes().to_vec()),
    }
}

#[test]
fn a_variable_is_found_by_its_name_in_either_case_and_keeps_its_spelling() {
    let mut vars = store();
    vars.assign(b"Edit$Dir", Value::String(b"a".to_vec()))
        .unwrap();
    vars.assign(b"EDIT$DIR", Value::String(b"b".to_vec()))
        .unwrap();
    assert_eq!(string(&mut vars, b"edit$dir"), Some(b"b".to_vec()));
    assert_eq!(vars.entries.len(), 1);
    assert_eq!(vars.entries[0].0, b"Edit$Dir");
    assert_eq!(vars.lookup(b"Other").unwrap(), None);
}

#[test]
fn a_wildcard_means_the_first_match_in_sort_order() {
    let mut vars = store();
    for name in [&b"Zeta$Dir"[..], b"Alpha$Dir", b"Mid$Path"] {
        vars.assign(name, Value::Number(1)).unwrap();
    }
    let names: Vec<&[u8]> = vars.entries.iter().map(|(n, _)| n.as_slice()).collect();
    assert_eq!(names, [&b"Alpha$Dir"[..], b"Mid$Path", b"Zeta$Dir"]);
    vars.assign(b"*$Dir", Value::Number(7)).unwrap();
    assert_eq!(vars.lookup(b"Alpha$Dir").unwrap(), Some(Value::Number(7)));
    assert_eq!(vars.lookup(b"Zeta$Dir").unwrap(), Some(Value::Number(1)));
    assert_eq!(vars.lookup(b"M#d$Path").unwrap(), Some(Value::Number(1)));
}

#[test]
fn a_new_variable_needs_a_plain_name_and_room() {
    let mut vars = Store::with_room(1, 4).unwrap();
    assert_eq!(
        vars.assign(b"*", Value::Number(1)),
        Err(Error::BadVariableName)
    );
    assert_eq!(
        vars.assign(b"", Value::Number(1)),
        Err(Error::BadVariableName)
    );
    assert_eq!(
        vars.assign(b"A", Value::String(b"12345".to_vec())),
        Err(Error::NoRoomForVariable)
    );
    vars.assign(b"A", Value::Number(1)).unwrap();
    assert_eq!(
        vars.assign(b"B", Value::Number(1)),
        Err(Error::NoRoomForVariable)
    );
    assert_eq!(
        vars.assign(b"A rest", Value::Number(2)),
        Ok(()),
        "a name ends at a space"
    );
    assert_eq!(vars.lookup(b"A").unwrap(), Some(Value::Number(2)));
}

#[test]
fn unset_deletes_every_match() {
    let mut vars = store();
    for name in [&b"App$Dir"[..], b"App$Path", b"Other"] {
        vars.assign(name, Value::Number(1)).unwrap();
    }
    assert_eq!(vars.delete(b"App$*"), Ok(2));
    assert_eq!(vars.delete(b"App$*"), Ok(0));
    assert_eq!(vars.delete(b"other"), Ok(1));
    assert!(vars.entries.is_empty());
}

#[test]
fn a_string_is_translated_when_set_and_sees_the_old_value() {
    let mut vars = store();
    set(
        &mut vars,
        b"X",
        Request::String(b"one|Mtwo"),
        64,
        &mut budget(),
    )
    .unwrap();
    assert_eq!(string(&mut vars, b"X"), Some(b"one\rtwo".to_vec()));
    set(
        &mut vars,
        b"X",
        Request::String(b"<X><X>"),
        64,
        &mut budget(),
    )
    .unwrap();
    assert_eq!(string(&mut vars, b"X"), Some(b"one\rtwoone\rtwo".to_vec()));
    set(
        &mut vars,
        b"Q",
        Request::String(b"\"a b\" dropped"),
        64,
        &mut budget(),
    )
    .unwrap();
    assert_eq!(
        string(&mut vars, b"Q"),
        Some(b"a b".to_vec()),
        "the closing quote ends it"
    );
    assert_eq!(
        set(&mut vars, b"Bad", Request::String(b"a|"), 64, &mut budget()),
        Err(Error::BadString)
    );
    assert_eq!(vars.lookup(b"Bad").unwrap(), None, "a failure sets nothing");
    assert_eq!(
        set(
            &mut vars,
            b"Long",
            Request::String(&[b'x'; 65]),
            64,
            &mut budget()
        ),
        Err(Error::NoRoomForVariable)
    );
}

#[test]
fn a_macro_is_kept_as_written_to_its_terminator() {
    let mut vars = store();
    let kind = set(
        &mut vars,
        b"M",
        Request::Macro(b"<Obey$Dir>.x\rjunk"),
        64,
        &mut budget(),
    );
    assert_eq!(kind, Ok(Kind::Macro));
    assert_eq!(
        vars.lookup(b"M").unwrap(),
        Some(Value::Macro(b"<Obey$Dir>.x".to_vec()))
    );
    assert_eq!(
        set(&mut vars, b"M", Request::Macro(b"a\tb"), 64, &mut budget()),
        Err(Error::BadMacroValue)
    );
}

#[test]
fn an_expression_sets_a_number_or_a_literal_string() {
    let mut vars = store();
    assert_eq!(
        set(
            &mut vars,
            b"N",
            Request::Expanded(b"6*7"),
            64,
            &mut budget()
        ),
        Ok(Kind::Number)
    );
    assert_eq!(vars.lookup(b"N").unwrap(), Some(Value::Number(42)));
    assert_eq!(
        set(
            &mut vars,
            b"S",
            Request::Expanded(b"\"a|Mb\""),
            64,
            &mut budget()
        ),
        Ok(Kind::String)
    );
    assert_eq!(
        string(&mut vars, b"S"),
        Some(b"a|Mb".to_vec()),
        "no further expansion"
    );
    set(
        &mut vars,
        b"L",
        Request::Literal(b"<N>|M"),
        64,
        &mut budget(),
    )
    .unwrap();
    assert_eq!(string(&mut vars, b"L"), Some(b"<N>|M".to_vec()));
    set(&mut vars, b"I", Request::Number(-3), 64, &mut budget()).unwrap();
    assert_eq!(vars.lookup(b"I").unwrap(), Some(Value::Number(-3)));
}

#[test]
fn a_variable_read_expanded_writes_a_number_and_translates_a_macro() {
    let mut vars = store();
    vars.assign(b"N", Value::Number(-7)).unwrap();
    vars.assign(b"S", Value::String(b"<N>".to_vec())).unwrap();
    vars.assign(b"M", Value::Macro(b"  \"<N>,|M\"".to_vec()))
        .unwrap();
    let read =
        |vars: &mut Store, name: &[u8], longest| read_expanded(vars, name, &mut budget(), longest);
    assert_eq!(read(&mut vars, b"N", 64), Ok(Some(b"-7".to_vec())));
    assert_eq!(
        read(&mut vars, b"S", 64),
        Ok(Some(b"<N>".to_vec())),
        "a string as it is"
    );
    assert_eq!(read(&mut vars, b"M", 64), Ok(Some(b"-7,\r".to_vec())));
    assert_eq!(read(&mut vars, b"Unset", 64), Ok(None));
    assert_eq!(read(&mut vars, b"S", 2), Err(Error::BufferOverflow));
}
