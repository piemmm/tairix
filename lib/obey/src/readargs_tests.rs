use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::vars::{Store, Value};

const BASE: u32 = 0x8000;

fn run(template: &[u8], args: &[u8], capacity: usize) -> Result<ReadArgs, Error> {
    let mut vars = Store::with_room(4, 64).unwrap();
    vars.assign(b"Five", Value::Number(5)).unwrap();
    read_args(
        template,
        args,
        &mut vars,
        &mut Budget::new(100),
        BASE,
        capacity,
    )
}

fn args(template: &[u8], line: &[u8]) -> Result<Vec<Arg>, Error> {
    run(template, line, 1024).map(|read| read.args)
}

fn text(t: &[u8]) -> Arg {
    Arg::Text(t.to_vec())
}

fn word(buffer: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buffer[at..at + 4].try_into().unwrap())
}

#[test]
fn positional_items_fill_the_entries_a_keyword_has_not() {
    assert_eq!(args(b"a,b", b"-b x y"), Ok(vec![text(b"y"), text(b"x")]),);
    assert_eq!(args(b"a,b", b"one"), Ok(vec![text(b"one"), Arg::Absent]));
    assert_eq!(
        args(b"a", b"one two"),
        Err(Error::BadParameters),
        "no entry left"
    );
    assert_eq!(args(b"a/K,b", b"one"), Ok(vec![Arg::Absent, text(b"one")]));
    assert_eq!(args(b"a/A,b", b""), Err(Error::BadParameters));
    assert_eq!(
        args(b"a/a", b"x"),
        Ok(vec![text(b"x")]),
        "qualifiers in either case"
    );
}

#[test]
fn a_keyword_is_whole_or_its_first_letter() {
    let template = b"verbose/S,output/K,quiet=silent/S";
    assert_eq!(
        args(template, b"-verbose -output file"),
        Ok(vec![Arg::Switch, text(b"file"), Arg::Absent])
    );
    assert_eq!(
        args(template, b"-V"),
        Ok(vec![Arg::Switch, Arg::Absent, Arg::Absent])
    );
    assert_eq!(
        args(template, b"-ve"),
        Err(Error::BadParameters),
        "no longer abbreviation"
    );
    assert_eq!(args(template, b"-verbosity"), Err(Error::BadParameters));
    assert_eq!(
        args(template, b"-silent"),
        Ok(vec![Arg::Absent, Arg::Absent, Arg::Switch])
    );
    assert_eq!(
        args(template, b"-ofile"),
        Ok(vec![Arg::Absent, text(b"file"), Arg::Absent])
    );
    assert_eq!(
        args(template, b"-output=x"),
        Ok(vec![Arg::Absent, text(b"=x"), Arg::Absent])
    );
    assert_eq!(args(template, b"-unknown"), Err(Error::BadParameters));
    assert_eq!(
        args(template, b"-output"),
        Err(Error::BadParameters),
        "a keyword wants a value"
    );
    assert_eq!(args(template, b"-output -v"), Err(Error::BadParameters));
}

#[test]
fn abbreviated_switches_run_together() {
    let template = b"all/S,brief/S,output/K";
    assert_eq!(
        args(template, b"-ab"),
        Ok(vec![Arg::Switch, Arg::Switch, Arg::Absent])
    );
    assert_eq!(
        args(template, b"-aofile"),
        Ok(vec![Arg::Switch, Arg::Absent, text(b"file")])
    );
    assert_eq!(args(template, b"-aa"), Err(Error::ArgumentRepeated));
    assert_eq!(args(template, b"-ax"), Err(Error::BadParameters));
    assert_eq!(args(template, b"-all -all"), Err(Error::ArgumentRepeated));
}

#[test]
fn quoted_items_keep_spaces_and_a_doubled_quote() {
    assert_eq!(
        args(b"a,b", b"\"x \"\"y\"\" z\" w"),
        Ok(vec![text(b"x \"y\" z"), text(b"w")])
    );
    assert_eq!(
        args(b"a,b", b"\"ab\"cd"),
        Ok(vec![text(b"ab"), text(b"cd")])
    );
    assert_eq!(
        args(b"a,b", b"\"-b\" x"),
        Ok(vec![text(b"-b"), text(b"x")]),
        "never a keyword"
    );
    assert_eq!(args(b"a", b"\"open"), Err(Error::BadString));
    assert_eq!(
        args(b"a,b", b"x\ty"),
        Ok(vec![text(b"x"), Arg::Absent]),
        "a tab ends the line"
    );
}

#[test]
fn the_buffer_holds_a_word_per_entry_then_the_values_in_the_order_given() {
    let read = run(
        b"File/A,TabExpand/S,n/E,g/G",
        b"-n Five+1 -tabexpand name -g a|Mb",
        64,
    )
    .unwrap();
    assert_eq!(read.args[0], text(b"name"));
    assert_eq!(read.args[2], Arg::Evaluated(Answer::Integer(6)));
    assert_eq!(read.args[3], Arg::Translated(b"a\rb".to_vec()));
    let buffer = &read.buffer;
    // The slots, then the /E value at 16, the text at 21, the /G value at 26.
    assert_eq!(word(buffer, 0), BASE + 21);
    assert_eq!(word(buffer, 4), SWITCH);
    assert_eq!(word(buffer, 8), BASE + 16);
    assert_eq!(word(buffer, 12), BASE + 26);
    assert_eq!(&buffer[16..21], &[0, 6, 0, 0, 0]);
    assert_eq!(&buffer[21..26], b"name\0");
    assert_eq!(&buffer[26..31], &[3, 0, b'a', b'\r', b'b']);
    assert_eq!(read.left, 64 - 31);
}

#[test]
fn an_evaluated_string_is_its_type_length_and_bytes() {
    let read = run(b"e/E", b"\"\"\"ab\"\"+\"\"c\"\"\"", 32).unwrap();
    assert_eq!(
        read.args[0],
        Arg::Evaluated(Answer::String(b"abc".to_vec()))
    );
    assert_eq!(&read.buffer[4..10], &[1, 3, 0, b'a', b'b', b'c']);
    assert_eq!(read.left, 32 - 10);
}

#[test]
fn a_buffer_too_small_overflows_where_it_fills() {
    assert_eq!(
        run(b"a,b,c", b"", 11).err(),
        Some(Error::BufferOverflow),
        "the slots alone"
    );
    assert_eq!(
        run(b"a", b"abc", 7).err(),
        Some(Error::BufferOverflow),
        "text and its NUL"
    );
    assert!(run(b"a", b"abc", 8).is_ok());
    assert_eq!(
        run(b"g/G", b"abc", 9).err(),
        Some(Error::BufferOverflow),
        "GSTrans's terminator"
    );
    assert!(run(b"g/G", b"abc", 10).is_ok());
    assert_eq!(
        run(b"g/G", b"\"\"", 6).err(),
        Some(Error::BufferOverflow),
        "no room for even an empty translation's terminator"
    );
    assert_eq!(run(b"g/G", b"\"\"", 7).map(|read| read.buffer.len()), Ok(7));
    assert_eq!(run(b"e/E", b"12", 8).err(), Some(Error::BufferOverflow));
    assert!(run(b"e/E", b"12", 9).is_ok());
}

#[test]
fn a_template_is_names_and_qualifiers_whatever_else_it_holds() {
    assert_eq!(
        args(b"no-wait/s", b"-no"),
        Ok(vec![Arg::Switch]),
        "a name is its keyword characters"
    );
    assert_eq!(
        args(b",x", b"- y"),
        Ok(vec![text(b"y"), Arg::Absent]),
        "a bare - names the entry with no name"
    );
    assert_eq!(
        args(b"a//s", b"x"),
        Ok(vec![text(b"x")]),
        "an unknown qualifier is ignored"
    );
    assert_eq!(
        args(b"", b"x"),
        Ok(vec![text(b"x")]),
        "an empty template has one entry"
    );
    assert_eq!(
        args(b"a/E/G", b"Five"),
        Ok(vec![Arg::Evaluated(Answer::Integer(5))]),
        "/E outranks /G"
    );
}
