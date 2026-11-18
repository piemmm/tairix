use alloc::vec::Vec;

use super::{if_there, IfThere};

#[test]
fn if_there_splits_at_the_first_else_word_in_either_case() {
    assert_eq!(
        if_there(b"RAM:$.F Then Set A yes Else Set A no"),
        Some(IfThere {
            object: b"RAM:$.F",
            then: b"Set A yes",
            otherwise: b"Set A no",
        })
    );
    assert_eq!(
        if_there(b"F THEN Set A b ELSE Set A c Else d"),
        Some(IfThere {
            object: b"F",
            then: b"Set A b",
            otherwise: b"Set A c Else d",
        })
    );
}

#[test]
fn if_there_takes_else_only_as_a_whole_word() {
    assert_eq!(
        if_there(b"F then Set H aElseb"),
        Some(IfThere {
            object: b"F",
            then: b"Set H aElseb",
            otherwise: b"",
        })
    );
}

#[test]
fn if_there_without_then_after_the_object_is_refused() {
    assert_eq!(if_there(b"F Set Z x"), None);
    assert_eq!(if_there(b"F"), None);
    assert_eq!(if_there(b""), None);
    assert_eq!(if_there(b"F Thenx Set Z x"), None);
}

#[test]
fn if_there_with_nothing_after_then_or_else_chooses_nothing() {
    assert_eq!(
        if_there(b"F Then "),
        Some(IfThere {
            object: b"F",
            then: b"",
            otherwise: b"",
        })
    );
    assert_eq!(
        if_there(b"F Then Set I a Else "),
        Some(IfThere {
            object: b"F",
            then: b"Set I a",
            otherwise: b"",
        })
    );
}

#[test]
fn a_quoted_parameter_runs_to_the_next_quotation_mark_and_loses_its_marks() {
    let found: Vec<&[u8]> = super::parameters(b"  a \"b c\"  d ").collect();
    assert_eq!(found, [&b"a"[..], b"b c", b"d"]);
    let unclosed: Vec<&[u8]> = super::parameters(b"x \"y z").collect();
    assert_eq!(
        unclosed,
        [&b"x"[..], b"y z"],
        "an unclosed quotation runs to the end"
    );
    assert_eq!(super::parameters(b"   ").count(), 0);
}
