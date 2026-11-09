//! Dictionaries: sorted for lookup, the last value of a repeated key kept.

use alloc::format;
use alloc::vec::Vec;

use tairix_collections::OrderedMap;

use super::{Dict, Name, ObjRef, Object, Resolved, Stream};

fn name(text: &str) -> Name {
    Name(text.as_bytes().to_vec())
}

fn written(entries: &[(&str, i64)]) -> Dict {
    let entries: Vec<(Name, Object)> = entries
        .iter()
        .map(|&(key, value)| (name(key), Object::Int(value)))
        .collect();
    Dict::from_written(entries).expect("room for a dictionary")
}

#[test]
fn a_small_dictionary_finds_every_key_and_keeps_the_last_repeat() {
    let dict = written(&[
        ("Type", 1),
        ("Kids", 2),
        ("Count", 3),
        ("Kids", 4),
        ("A", 5),
    ]);
    assert_eq!(dict.iter().count(), 4);
    assert_eq!(dict.get(b"Kids"), Some(&Object::Int(4)));
    assert_eq!(dict.get(b"Type"), Some(&Object::Int(1)));
    assert_eq!(dict.get(b"Count"), Some(&Object::Int(3)));
    assert_eq!(dict.get(b"A"), Some(&Object::Int(5)));
    assert_eq!(dict.get(b"Missing"), None);
    let keys: Vec<&[u8]> = dict.iter().map(|(key, _)| key.as_bytes()).collect();
    assert_eq!(keys, [&b"A"[..], b"Count", b"Kids", b"Type"]);
}

#[test]
fn a_large_dictionary_finds_every_key_and_keeps_the_last_repeat() {
    let keys: Vec<alloc::string::String> = (0..200).rev().map(|at| format!("K{at}")).collect();
    let mut entries: Vec<(&str, i64)> = keys.iter().map(|key| (key.as_str(), 0)).collect();
    // Every fifth key written again later, with the value that must win.
    let repeats: Vec<(&str, i64)> = keys
        .iter()
        .step_by(5)
        .map(|key| (key.as_str(), 7))
        .collect();
    entries.extend(repeats);
    let dict = written(&entries);
    assert_eq!(dict.iter().count(), 200);
    for (at, key) in keys.iter().enumerate() {
        let expected = if at % 5 == 0 { 7 } else { 0 };
        assert_eq!(
            dict.get(key.as_bytes()),
            Some(&Object::Int(expected)),
            "{key}"
        );
    }
    assert!(dict
        .iter()
        .zip(dict.iter().skip(1))
        .all(|((a, _), (b, _))| a < b));
}

#[test]
fn a_type_and_a_name_are_read_through_the_dictionary() {
    let dict =
        Dict::from_written(alloc::vec![(name("Type"), Object::Name(name("Page")))]).expect("room");
    assert!(dict.is_type(b"Page"));
    assert!(!dict.is_type(b"Pages"));
    assert_eq!(dict.name(b"Type"), Some(&b"Page"[..]));
}

#[test]
fn a_copy_is_the_object_however_deeply_it_nests() {
    let inner = Dict::from_written(alloc::vec![
        (name("S"), Object::String(b"text".to_vec())),
        (name("N"), Object::Name(name("Name"))),
        (name("R"), Object::Ref(ObjRef { num: 7, gen: 1 })),
    ])
    .expect("room");
    let object = Object::Array(alloc::vec![
        Object::Null,
        Object::Bool(true),
        Object::Int(-3),
        Object::Real(0.5),
        Object::Dict(inner.try_clone().expect("room")),
        Object::Stream(Stream {
            dict: inner,
            data: 4..9,
            owner: ObjRef { num: 2, gen: 0 },
        }),
    ]);
    assert_eq!(object.try_clone(), Ok(object.clone()));
    assert_eq!(Resolved::Borrowed(&object).into_owned(), Ok(object.clone()));
    assert_eq!(
        Resolved::Owned(Object::Int(1)).into_owned(),
        Ok(Object::Int(1))
    );
    let shared = alloc::sync::Arc::new(object.clone());
    let held = alloc::sync::Arc::clone(&shared);
    assert_eq!(
        Resolved::Shared(shared).into_owned(),
        Ok(object.clone()),
        "copied"
    );
    assert_eq!(
        Resolved::Shared(held).into_owned(),
        Ok(object),
        "the last holder's taken"
    );
}

#[test]
fn a_name_is_found_by_its_bytes() {
    let mut names = OrderedMap::new();
    names
        .try_insert(Name::of(b"CS0").expect("room"), 1)
        .expect("room");
    assert_eq!(names.get(&b"CS0"[..]), Some(&1));
    assert_eq!(names.get(&b"CS1"[..]), None);
}
