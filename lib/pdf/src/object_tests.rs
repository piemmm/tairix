//! Dictionaries: sorted for lookup, the last value of a repeated key kept.

use alloc::format;
use alloc::vec::Vec;

use alloc::borrow::Cow;

use tairix_collections::OrderedMap;

use super::{owned, Dict, Name, ObjRef, Object, Stream};

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
fn completing_from_an_older_dictionary_adds_only_the_keys_it_lacks() {
    let mut dict = written(&[("B", 1), ("D", 2)]);
    dict.complete_from(&written(&[("A", 5), ("B", 9), ("C", 3), ("E", 7)]))
        .expect("room");
    let entries: Vec<(&[u8], &Object)> = dict
        .iter()
        .map(|(key, value)| (key.as_bytes(), value))
        .collect();
    assert_eq!(
        entries,
        [
            (&b"A"[..], &Object::Int(5)),
            (b"B", &Object::Int(1)),
            (b"C", &Object::Int(3)),
            (b"D", &Object::Int(2)),
            (b"E", &Object::Int(7)),
        ]
    );
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
    assert_eq!(owned(Cow::Borrowed(&object)), Ok(object.clone()));
    assert_eq!(owned(Cow::Owned(Object::Int(1))), Ok(Object::Int(1)));
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
