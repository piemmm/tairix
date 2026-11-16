use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::vars::{Store, Value};

const BASE: u32 = 0x8000;

fn run(template: &[u8], args: &[u8], capacity: usize) -> Result<ReadArgs, Error> {
    let mut vars = Store::with_room(4, 64, 64).unwrap();
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

/// An argument with the bytes the buffer holds for it.
#[derive(Debug, PartialEq, Eq)]
enum Got {
    Absent,
    Switch,
    Text(Vec<u8>),
    Translated(Vec<u8>),
    Evaluated(Answer),
}

fn args(template: &[u8], line: &[u8]) -> Result<Vec<Got>, Error> {
    run(template, line, 1024).map(|read| {
        read.args
            .into_iter()
            .map(|arg| match arg {
                Arg::Absent => Got::Absent,
                Arg::Switch => Got::Switch,
                Arg::Text(at) => Got::Text(read.buffer[at].to_vec()),
                Arg::Translated(at) => Got::Translated(read.buffer[at].to_vec()),
                Arg::Evaluated(answer) => Got::Evaluated(answer),
            })
            .collect()
    })
}

fn text(t: &[u8]) -> Got {
    Got::Text(t.to_vec())
}

fn word(buffer: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buffer[at..at + 4].try_into().unwrap())
}

#[test]
fn positional_items_fill_the_entries_a_keyword_has_not() {
    assert_eq!(args(b"a,b", b"-b x y"), Ok(vec![text(b"y"), text(b"x")]),);
    assert_eq!(args(b"a,b", b"one"), Ok(vec![text(b"one"), Got::Absent]));
    assert_eq!(
        args(b"a", b"one two"),
        Err(Error::BadParameters),
        "no entry left"
    );
    assert_eq!(args(b"a/K,b", b"one"), Ok(vec![Got::Absent, text(b"one")]));
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
        Ok(vec![Got::Switch, text(b"file"), Got::Absent])
    );
    assert_eq!(
        args(template, b"-V"),
        Ok(vec![Got::Switch, Got::Absent, Got::Absent])
    );
    assert_eq!(
        args(template, b"-ve"),
        Err(Error::BadParameters),
        "no longer abbreviation"
    );
    assert_eq!(args(template, b"-verbosity"), Err(Error::BadParameters));
    assert_eq!(
        args(template, b"-silent"),
        Ok(vec![Got::Absent, Got::Absent, Got::Switch])
    );
    assert_eq!(
        args(template, b"-ofile"),
        Ok(vec![Got::Absent, text(b"file"), Got::Absent])
    );
    assert_eq!(
        args(template, b"-output=x"),
        Ok(vec![Got::Absent, text(b"=x"), Got::Absent])
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
        Ok(vec![Got::Switch, Got::Switch, Got::Absent])
    );
    assert_eq!(
        args(template, b"-aofile"),
        Ok(vec![Got::Switch, Got::Absent, text(b"file")])
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
        Ok(vec![text(b"x"), Got::Absent]),
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
    assert_eq!(read.args[0], Arg::Text(21..25));
    assert_eq!(read.args[2], Arg::Evaluated(Answer::Integer(6)));
    assert_eq!(read.args[3], Arg::Translated(28..31));
    let buffer = &read.buffer;
    // The slots, then the /E value at 16, the text at 21, the /G value at 26.
    assert_eq!(word(buffer, 0), BASE + 21);
    assert_eq!(word(buffer, 4), SWITCH);
    assert_eq!(word(buffer, 8), BASE + 16);
    assert_eq!(word(buffer, 12), BASE + 26);
    assert_eq!(&buffer[16..21], &[0, 6, 0, 0, 0]);
    assert_eq!(&buffer[21..26], b"name\0");
    assert_eq!(&buffer[26..31], &[3, 0, b'a', b'\r', b'b']);
    assert_eq!(read.left, 64 - 34, "the integer counted as eight bytes");
}

#[test]
fn a_value_is_laid_over_the_text_it_was_read_from() {
    let read = run(b"n/E", b"(-3+6)", 64).unwrap();
    assert_eq!(
        &read.buffer[4..11],
        &[0, 3, 0, 0, 0, b')', 0],
        "the copy's tail"
    );
    let read = run(b"n/E,a", b"1 z", 64).unwrap();
    assert_eq!(word(&read.buffer, 4), BASE + 13, "the next laid after five");
    let read = run(b"g/G,a", b"<Five> z", 64).unwrap();
    assert_eq!(
        &read.buffer[8..15],
        &[1, 0, b'5', b'z', 0, b'>', 0],
        "the terminator written, then laid over"
    );
    assert_eq!(read.left, 64 - 13);
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
    assert_eq!(run(b"e/E", b"12", 11).err(), Some(Error::BufferOverflow));
    assert_eq!(run(b"e/E", b"12", 12).map(|read| read.left), Ok(0));
}

#[test]
fn a_template_is_names_and_qualifiers_whatever_else_it_holds() {
    assert_eq!(
        args(b"no-wait/s", b"-no"),
        Ok(vec![Got::Switch]),
        "a name is its keyword characters"
    );
    assert_eq!(
        args(b",x", b"- y"),
        Ok(vec![text(b"y"), Got::Absent]),
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
        Ok(vec![Got::Evaluated(Answer::Integer(5))]),
        "/E outranks /G"
    );
}

#[test]
fn a_translated_value_s_length_word_is_its_length() {
    let mut vars = Store::with_room(4, 2048, 8).unwrap();
    vars.assign(b"V", Value::String(vec![b'v'; 1000])).unwrap();
    let mut budget = Budget::new(100);
    let read = read_args(b"g/G", b"<V>", &mut vars, &mut budget, BASE, 0x2000).unwrap();
    assert_eq!(
        &read.buffer[4..6],
        &1000u16.to_le_bytes(),
        "the longest GSTrans gives"
    );
}

/// The entry a `-` and the bytes at `at` name, found by trying every name of
/// `entries` in template order.
fn keyword_by_every_name(
    entries: &[Vec<Vec<u8>>],
    args: &[u8],
    at: usize,
) -> Option<(usize, usize, bool)> {
    let fold = |b: u8| b.to_ascii_uppercase();
    let names = || {
        entries
            .iter()
            .enumerate()
            .flat_map(|(entry, names)| names.iter().map(move |name| (entry, name)))
    };
    let whole = names().find(|(_, name)| {
        args.get(at..at + name.len())
            .is_some_and(|text| text.eq_ignore_ascii_case(name))
            && !is_keyword_byte(byte(args, at + name.len()))
    });
    if let Some((entry, name)) = whole {
        return Some((entry, at + name.len(), true));
    }
    names()
        .find(|(_, name)| {
            name.len() > 1
                && fold(name[0]) == fold(byte(args, at))
                && fold(name[1]) != fold(byte(args, at + 1))
        })
        .map(|(entry, _)| (entry, at + 1, false))
}

#[test]
fn an_indexed_keyword_is_the_one_every_name_tried_in_order_finds() {
    let mut rng = tairix_fuzzseed::Prng::new(0x0BE7_4A26_0000_0001);
    let alphabet = b"abAB_1";
    for _ in 0..400 {
        let word = |rng: &mut tairix_fuzzseed::Prng| -> Vec<u8> {
            (0..rng.below(4)).map(|_| *rng.pick(alphabet)).collect()
        };
        let entries: Vec<Vec<Vec<u8>>> = (0..=rng.below(8))
            .map(|_| (0..=rng.below(3)).map(|_| word(&mut rng)).collect())
            .collect();
        let text: Vec<u8> = entries
            .iter()
            .map(|names| names.join(&b'='))
            .collect::<Vec<_>>()
            .join(&b',');
        let t = template(&text).unwrap();
        for _ in 0..20 {
            let mut args = word(&mut rng);
            args.extend_from_slice(rng.pick(&[&b""[..], b" x", b"-", b"9"]));
            assert_eq!(
                keyword(&t, &args, 0),
                keyword_by_every_name(&entries, &args, 0),
                "-{:?} against {:?}",
                core::str::from_utf8(&args),
                core::str::from_utf8(&text)
            );
        }
    }
}

#[test]
fn positional_items_skip_every_entry_a_keyword_or_switch_has_taken() {
    let template: Vec<u8> = (0..300)
        .map(|n| alloc::format!("e{n}{}", if n % 3 == 1 { "/K" } else { "" }))
        .collect::<Vec<_>>()
        .join(",")
        .into_bytes();
    let mut line: Vec<u8> = b"-e0 zero ".to_vec();
    for n in 0..199 {
        line.extend_from_slice(alloc::format!("p{n} ").as_bytes());
    }
    let read = run(&template, &line, 1 << 16).unwrap();
    assert_eq!(read.args[0], Arg::Text(1200..1204));
    let taken: Vec<usize> = (0..300)
        .filter(|&n| matches!(read.args[n], Arg::Text(_)))
        .collect();
    let expected: Vec<usize> = core::iter::once(0)
        .chain((2..300).filter(|n| n % 3 != 1).take(199))
        .collect();
    assert_eq!(taken, expected);
}
