//! Validation strings and special fields held to what RISC OS 5's DeviceFS
//! 0.73 gave a driver for each, recorded on RPCEmu.

use super::*;

const A: u32 = ABSENT;

fn words(validation: &str, fields: &[&str]) -> Result<Vec<u32>, tairix_obey::Error> {
    let fields: Vec<&[u8]> = fields.iter().map(|field| field.as_bytes()).collect();
    let parsed = Validation::parse(validation.as_bytes()).expect("room");
    parsed.words(&fields).map(|words| words.expect("room"))
}

fn given(validation: &str, field: &str) -> Vec<u32> {
    words(validation, &[field]).expect("read")
}

#[test]
fn commands_follow_one_another_with_no_separator() {
    let v = "baud,b/Ndata/Nmike,dennis/S";
    assert_eq!(given(v, "baud9600;data7;mike"), [9600, 7, 0]);
    assert_eq!(given(v, "dennis"), [A, A, 1]);
    assert_eq!(given(v, "data7"), [A, 7, A]);
    assert_eq!(given(v, "mike"), [A, A, 0]);
    assert_eq!(given(v, "b300;mike;data2"), [300, 2, 0], "in any order");
    assert_eq!(given(v, ""), [A, A, A]);
    assert_eq!(
        given(v, "zzz"),
        [A, A, A],
        "an unknown keyword is passed over"
    );
}

#[test]
fn a_separator_in_the_validation_string_belongs_to_the_keyword_after_it() {
    assert_eq!(
        given("baud,b/N data/N mike,dennis/S", "baud9600;data7;mike"),
        [9600, A, A]
    );
    assert_eq!(given("baud/N;data/N", "data7"), [A, A]);
    assert_eq!(given("baud/N;data/N", "baud5;data7"), [5, A]);
    assert_eq!(given("baud/N;;data/N", "data7"), [A, A]);
    assert_eq!(given("baud,b/N;data/N;mike,dennis/S", "dennis"), [A, A, 1]);
}

#[test]
fn an_empty_keyword_begins_every_field_and_wants_its_number() {
    let v = "baud/N,data/N";
    assert_eq!(words(v, &["data7"]), Err(tairix_obey::Error::BadNumber));
    assert_eq!(words(v, &["baud5"]), Err(tairix_obey::Error::BadNumber));
}

#[test]
fn a_keyword_is_whole_and_matched_without_case() {
    let v = "baud,b/N";
    assert_eq!(given(v, "BAUD9600"), [9600]);
    assert_eq!(
        given(v, "bau9600"),
        [A],
        "a b followed by a letter is not b"
    );
    assert_eq!(given(v, "buffer5"), [A]);
    assert_eq!(given(v, "9600"), [A]);
}

#[test]
fn a_number_is_read_to_its_end_after_any_equals_sign() {
    let v = "baud,b/N";
    assert_eq!(given(v, "baud=9600"), [9600]);
    assert_eq!(given(v, "baud12x"), [12]);
    assert_eq!(given(v, "baud&10"), [16], "as OS_ReadUnsigned reads one");
    assert_eq!(words(v, &["baud"]), Err(tairix_obey::Error::BadNumber));
    assert_eq!(
        words(v, &["baud99999999999"]),
        Err(tairix_obey::Error::NumberTooBig)
    );
}

#[test]
fn the_first_keyword_given_wins_the_special_field_before_the_options() {
    let v = "baud,b/Ndata/Nmike,dennis/S";
    assert_eq!(given(v, "baud9600;baud300"), [9600, A, A]);
    assert_eq!(
        words(v, &["baud300", "baud110;dennis"]),
        Ok(alloc::vec![300, A, 1])
    );
    assert_eq!(
        words(v, &["", "data5"]),
        Ok(alloc::vec![A, 5, A]),
        "the options alone"
    );
}

#[test]
fn a_keyword_s_later_escapes_read_only_what_a_separator_leads_to() {
    assert_eq!(given("x/N/N", "x5"), [5, A]);
    assert_eq!(given("x/N/N", "x5;x6"), [5, A], "not the second x");
    assert_eq!(given("x/N/N", "x5,6"), [5, 6]);
}

#[test]
fn a_character_out_of_place_in_a_number_separates_numbers_as_a_comma_does() {
    assert_eq!(given("x/N/N", "x5 6"), [5, 6]);
    assert_eq!(given("x/N/N", "x5x6"), [5, 6]);
    assert_eq!(given("x/N/N", "x5x"), [5, A], "nothing after the separator");
    assert_eq!(
        given("m,d/S/N", "m5"),
        [0, 5],
        "no separator before the first number"
    );
    assert_eq!(
        given("m,d/S/N", "d,5"),
        [1, 5],
        "a comma before it passed over"
    );
}

#[test]
fn an_escape_the_syntax_gives_no_meaning_fills_nothing() {
    assert_eq!(given("a/Xb/N", "a1;b2"), [A, 2]);
    assert_eq!(given("a//N", "a7"), [A, 7], "an escape with no character");
    assert_eq!(
        given("plain", "plain"),
        Vec::<u32>::new(),
        "a command with no escape"
    );
}
