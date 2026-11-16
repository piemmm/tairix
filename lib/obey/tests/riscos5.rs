//! The crate held to genuine RISC OS 5: every case in `riscos5/cases.txt`,
//! run through the crate in order on one store as `cargo xtask riscos
//! --reference` ran it on the machine, answering as the machine did in
//! `riscos5/answers.txt`. Each of the crate's errors stands for one RISC OS
//! error number throughout, and no two for the same one. A command line or an
//! Obey file is read as data: it answers whether it failed and with what,
//! and the variables it leaves are compared by the cases that read them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use tairix_obey::args::substitute;
use tairix_obey::command::{read_line, read_only, Context, Observer};
use tairix_obey::expr::evaluate;
use tairix_obey::gstrans::{gstrans, Flags, Read, Reader};
use tairix_obey::number::{read_unsigned, Reading};
use tairix_obey::readargs::read_args;
use tairix_obey::vars::{read_expanded, set, Kind, Request};
use tairix_obey::{Answer, Budget, Error, Store, Value, Variables};

const CASES: &str = include_str!("riscos5/cases.txt");
const ANSWERS: &str = include_str!("riscos5/answers.txt");

/// What the machine leaves in each byte of a buffer it does not write.
const UNWRITTEN: u8 = 0xEE;

/// Where the machine's Obey files were.
const OBEY_DIR: &[u8] = b"HostFS:$";

/// The references one case may expand: more than any case asks.
const REFERENCES: usize = 1 << 20;

/// A case: its kind and its fields' bytes.
struct Case {
    line: usize,
    text: String,
    kind: String,
    fields: Vec<Vec<u8>>,
}

/// An outcome as the probe writes it, but for each error: the crate's, by
/// name, where the machine's has its number.
type Said = String;

fn quote(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    for &b in bytes {
        if !(0x20..=0x7E).contains(&b) || b == b'"' || b == b'\\' {
            let _ = write!(out, "\\x{b:02X}");
        } else {
            out.push(char::from(b));
        }
    }
    out.push('"');
    out
}

/// The fields of a case line after its kind, each a quoted string.
fn fields(text: &str) -> Vec<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut fields = Vec::new();
    let mut at = 0;
    while let Some(open) = bytes[at..].iter().position(|&b| b == b'"') {
        let mut field = Vec::new();
        at += open + 1;
        while let Some(&b) = bytes.get(at) {
            at += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).expect("hex");
                    field.push(u8::from_str_radix(hex, 16).expect("an escape"));
                    at += 3;
                }
                other => field.push(other),
            }
        }
        fields.push(field);
    }
    fields
}

fn cases() -> Vec<Case> {
    CASES
        .lines()
        .enumerate()
        .filter(|(_, text)| !text.is_empty() && !text.starts_with('#'))
        .map(|(index, text)| {
            let (kind, rest) = text.split_once(' ').unwrap_or((text, ""));
            Case {
                line: index + 1,
                text: text.to_string(),
                kind: kind.to_string(),
                fields: fields(rest),
            }
        })
        .collect()
}

/// Each case line the answers echo, with the answer after it.
fn answers() -> Vec<(String, String)> {
    let mut lines = ANSWERS.lines().filter(|line| !line.starts_with('#'));
    let mut answers = Vec::new();
    while let Some(case) = lines.next() {
        let answer = lines
            .next()
            .and_then(|line| line.strip_prefix("= "))
            .unwrap_or_else(|| panic!("an answer after {case:?}"));
        answers.push((case.to_string(), answer.to_string()));
    }
    answers
}

/// A number field as BASIC's `EVAL` reads one, decimal, `-` decimal or `&`
/// hex, as the word it passes in a register.
fn word(field: &[u8]) -> u32 {
    let text = std::str::from_utf8(field).expect("a number field");
    match text.strip_prefix('&') {
        Some(hex) => u32::from_str_radix(hex, 16).expect("hex"),
        None => text.parse::<i32>().expect("a number").cast_unsigned(),
    }
}

/// A number field that is a size, which no case makes negative.
fn size(field: &[u8]) -> usize {
    usize::try_from(word(field)).expect("a size")
}

/// A number field that may be negative.
fn signed(field: &[u8]) -> i32 {
    word(field).cast_signed()
}

/// `bytes`, then the CR the probe ends a text field with.
fn line(bytes: &[u8]) -> Vec<u8> {
    let mut text = bytes.to_vec();
    text.push(b'\r');
    text
}

/// How many of a buffer's bytes the probe shows, given a byte past what the
/// call reported: as many as fit in `room`.
fn shown(reported: usize, room: usize) -> usize {
    reported.min(room)
}

/// A buffer of `room` bytes, `bytes` at its start, the rest unwritten.
fn buffer(bytes: &[u8], room: usize) -> Vec<u8> {
    let mut out = vec![UNWRITTEN; room.max(bytes.len())];
    out[..bytes.len()].copy_from_slice(bytes);
    out
}

/// Where reading stopped, as the probe says it: an offset in the text, or
/// `elsewhere` where R0 is not in it.
fn at(next: Option<usize>) -> String {
    next.map_or_else(|| "elsewhere".into(), |n| n.to_string())
}

fn error(e: Error) -> Said {
    format!("error <{e:?}>")
}

/// The errors the machine raises where the crate answers nothing, the
/// call's own: no variable to read or delete, and a type no value has.
const NOT_FOUND: &str = "error <NotFound>";
const BAD_TYPE: &str = "error <BadType>";

/// Told of no sprite file.
struct Unobserved;

impl Observer for Unobserved {
    fn icon_sprites(&mut self, _: &[u8], _: &mut dyn Variables, _: &mut Budget) {}
}

/// The machine's room for a variable's value: no limit any case reaches.
const LONGEST: usize = 1 << 16;

fn store() -> Store {
    Store::with_room(4096, LONGEST, 256).expect("a store")
}

fn gstrans_case(case: &Case, vars: &mut Store, budget: &mut Budget) -> Said {
    let text = line(&case.fields[0]);
    let flags = Flags::from_bits(word(&case.fields[1]));
    let room = size(&case.fields[2]);
    let mut out = buffer(&[], room);
    let t = gstrans(&text, flags, vars, budget, &mut out);
    let head = match t.ended {
        Ok(ended) => format!("carry {} next {}", u8::from(ended.is_none()), at(t.next)),
        Err(e) => error(e),
    };
    format!(
        "{head} stored {} out {}",
        t.stored,
        quote(&out[..shown(t.stored + 1, room)])
    )
}

fn gsread_case(case: &Case, vars: &mut Store, budget: &mut Budget) -> Said {
    let text = line(&case.fields[0]);
    let mut reader = Reader::new(&text, Flags::from_bits(word(&case.fields[1])));
    let mut said = format!(
        "first {} empty {} at {} reads",
        reader.first(),
        u8::from(reader.is_empty()),
        reader.position()
    );
    for _ in 0..600 {
        match reader.read(vars, budget) {
            Ok(Read::Char(c)) => {
                let _ = write!(said, " {}", c.cast_signed());
            }
            Ok(Read::End(b)) => {
                let next = (!reader.expanding()).then(|| reader.position());
                let _ = write!(said, " end {b} next {}", at(next));
                break;
            }
            Err(e) => {
                let _ = write!(said, " {}", error(e));
                break;
            }
        }
    }
    said
}

fn evaluate_case(case: &Case, vars: &mut Store, budget: &mut Budget) -> Said {
    let room = size(&case.fields[1]);
    match evaluate(&line(&case.fields[0]), vars, budget) {
        Ok(Answer::Integer(n)) => format!("integer {n}"),
        Ok(Answer::String(bytes)) if bytes.len() > room => error(Error::BufferOverflow),
        Ok(Answer::String(bytes)) => {
            let out = buffer(&bytes, room);
            format!(
                "string {} out {}",
                bytes.len(),
                quote(&out[..shown(bytes.len() + 1, room)])
            )
        }
        Err(e) => error(e),
    }
}

/// The buffer address the machine gave a `readargs` case.
fn base(theirs: &str) -> u32 {
    theirs
        .split(' ')
        .find_map(|word| word.strip_prefix('&'))
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .unwrap_or(0)
}

fn readargs_case(case: &Case, vars: &mut Store, budget: &mut Budget, theirs: &str) -> Said {
    let room = size(&case.fields[2]);
    let at = base(theirs);
    match read_args(
        &line(&case.fields[0]),
        &line(&case.fields[1]),
        vars,
        budget,
        at,
        room,
    ) {
        Ok(read) => format!(
            "left {} base &{at:X} out {}",
            read.left,
            quote(&buffer(&read.buffer, room + 1)[..=room])
        ),
        Err(e) => error(e),
    }
}

fn substitute_case(case: &Case) -> Said {
    let room = size(&case.fields[2]);
    let append = word(&case.fields[3]) & 0x8000_0000 == 0;
    match substitute(&line(&case.fields[0]), &case.fields[1], append, room) {
        Ok(bytes) => {
            let out = buffer(&bytes, room + 1);
            format!(
                "length {} out {}",
                bytes.len(),
                quote(&out[..shown(bytes.len() + 1, room + 1)])
            )
        }
        Err(e) => error(e),
    }
}

const fn kind_number(kind: Kind) -> u8 {
    match kind {
        Kind::String => 0,
        Kind::Number => 1,
        Kind::Macro => 2,
    }
}

fn setvar_case(case: &Case, vars: &mut Store, budget: &mut Budget) -> Said {
    let name = line(&case.fields[0]);
    let value = &case.fields[1];
    let Ok(length) = usize::try_from(signed(&case.fields[2])) else {
        return match vars.delete(&name) {
            Ok(false) => NOT_FOUND.into(),
            Ok(true) => "type 0".into(),
            Err(e) => error(e),
        };
    };
    let kind = word(&case.fields[3]);
    let text = line(value);
    let request = match kind {
        0 => Request::String(&text),
        1 => Request::Number(i32::from_le_bytes(
            value[..4].try_into().expect("four bytes"),
        )),
        2 => Request::Macro(&text),
        3 => Request::Expanded(&text),
        4 => Request::Literal(&value[..length]),
        _ => return BAD_TYPE.into(),
    };
    // A literal, and an expression's string, is reported as type 4.
    match set(vars, &name, request, LONGEST, budget) {
        Ok(Kind::String) if kind >= 3 => "type 4".into(),
        Ok(kind) => format!("type {}", kind_number(kind)),
        Err(e) => error(e),
    }
}

fn readvar_case(case: &Case, vars: &mut Store, budget: &mut Budget) -> Said {
    let name = line(&case.fields[0]);
    let room = signed(&case.fields[1]);
    let expand = word(&case.fields[2]) == 3;
    let value = match vars.lookup(&name) {
        Ok(Some(value)) => value,
        Ok(None) => return NOT_FOUND.into(),
        Err(e) => return error(e),
    };
    // An expanded macro is GSTrans'd into the buffer, which holds its
    // terminator too; anything else is copied, a number in decimal when
    // expanded.
    let (bytes, kind, terminated) = match (value, expand) {
        (Value::Macro(_), true) => match read_expanded(vars, &name, budget, LONGEST) {
            Ok(Some(bytes)) => (bytes, 2, true),
            Ok(None) => return NOT_FOUND.into(),
            Err(e) => return error(e),
        },
        (Value::Number(_) | Value::String(_), true) => {
            match read_expanded(vars, &name, budget, LONGEST) {
                Ok(Some(bytes)) => (bytes, 0, false),
                Ok(None) => return NOT_FOUND.into(),
                Err(e) => return error(e),
            }
        }
        (Value::String(bytes), false) => (bytes, 0, false),
        (Value::Number(n), false) => (n.to_le_bytes().to_vec(), 1, false),
        (Value::Macro(bytes), false) => (bytes, 2, false),
    };
    // A room below zero asks only whether the variable is there.
    let Some(room) = usize::try_from(room)
        .ok()
        .filter(|&room| bytes.len() + usize::from(terminated) <= room)
    else {
        return error(Error::BufferOverflow);
    };
    let length = bytes.len();
    let mut written = bytes;
    if terminated {
        written.push(b'\r');
    }
    let out = buffer(&written, room);
    format!(
        "length {length} type {kind} out {}",
        quote(&out[..shown(length + 1, room)])
    )
}

fn readunsigned_case(case: &Case) -> Said {
    let flags = word(&case.fields[0]);
    let text = line(&case.fields[1]);
    let limit = word(&case.fields[2]);
    let wide = word(&case.fields[3]) != 0 && flags & 1 << 28 != 0;
    let most = if wide { u64::MAX } else { u64::from(u32::MAX) };
    let reading = Reading {
        base: flags & 0xFF,
        limit: if flags & 1 << 30 != 0 {
            255
        } else if flags & 1 << 29 != 0 {
            u64::from(limit)
        } else {
            most
        },
        wide,
        terminated: flags & 1 << 31 != 0,
    };
    match read_unsigned(&text, reading) {
        Ok((value, next)) => {
            let [low, high] = [value & u64::from(u32::MAX), value >> 32]
                .map(|half| u32::try_from(half).expect("a word").cast_signed());
            format!(
                "value {low} high {} next {next}",
                if wide { high } else { 0 }
            )
        }
        Err(e) => error(e),
    }
}

fn sweep_case(case: &Case, vars: &mut Store, budget: &mut Budget) -> Said {
    let (prefix, suffix, pattern) = (&case.fields[0], &case.fields[1], &case.fields[4]);
    let from = u8::try_from(word(&case.fields[2])).expect("a byte");
    let to = u8::try_from(word(&case.fields[3])).expect("a byte");
    let mut said = String::from("set ");
    for b in from..=to {
        let mut command = prefix.clone();
        command.push(b);
        command.extend_from_slice(suffix);
        command.push(b'\r');
        let mut cx = Context {
            vars,
            longest: LONGEST,
            observer: &mut Unobserved,
            budget,
        };
        let _ = read_line(&command, &mut cx);
        let found = vars.exists(pattern).expect("a lookup");
        said.push(if found { '1' } else { '0' });
        vars.unset(pattern).expect("a deletion");
    }
    said
}

/// What the crate answers `case`.
fn answer(case: &Case, vars: &mut Store, theirs: &str) -> Said {
    let mut budget = Budget::new(REFERENCES);
    let budget = &mut budget;
    match case.kind.as_str() {
        "gstrans" => gstrans_case(case, vars, budget),
        "gsread" => gsread_case(case, vars, budget),
        "evaluate" => evaluate_case(case, vars, budget),
        "readargs" => readargs_case(case, vars, budget, theirs),
        "substitute" => substitute_case(case),
        "setvar" => setvar_case(case, vars, budget),
        "readvar" => readvar_case(case, vars, budget),
        "readunsigned" => readunsigned_case(case),
        "sweep" => sweep_case(case, vars, budget),
        "cli" | "obey" => {
            let mut cx = Context {
                vars,
                longest: LONGEST,
                observer: &mut Unobserved,
                budget,
            };
            let read = if case.kind == "cli" {
                read_line(&line(&case.fields[0]), &mut cx)
            } else {
                read_only(&case.fields[0], OBEY_DIR, &line(&case.fields[1]), &mut cx)
            };
            match read {
                Ok(()) => "done".into(),
                Err(e) => error(e),
            }
        }
        other => panic!("{}: no kind {other}", case.line),
    }
}

/// Each error the crate answered with, and the number the machine answered.
#[derive(Default)]
struct Numbers {
    by_error: BTreeMap<String, String>,
    by_number: BTreeMap<String, String>,
}

impl Numbers {
    /// Whether `ours` and `theirs` say the same, each of our errors standing
    /// for the number it first met. An error is all a call said where it is
    /// all the crate says.
    fn agree(&mut self, ours: &str, theirs: &str) -> bool {
        let ours: Vec<&str> = ours.split(' ').collect();
        let mut theirs: Vec<&str> = theirs.split(' ').collect();
        if ours.len() == 2 && ours[0] == "error" && theirs.first() == Some(&"error") {
            theirs.truncate(2);
        }
        if ours.len() != theirs.len() {
            return false;
        }
        ours.iter().zip(&theirs).all(|(&mine, &machine)| {
            match (
                mine.strip_prefix('<').and_then(|e| e.strip_suffix('>')),
                machine.strip_prefix('&'),
            ) {
                (Some(error), Some(number)) => {
                    let named = self
                        .by_error
                        .entry(error.into())
                        .or_insert_with(|| number.into());
                    let numbered = self
                        .by_number
                        .entry(number.into())
                        .or_insert_with(|| error.into());
                    named == number && numbered == error
                }
                _ => mine == machine,
            }
        })
    }
}

#[test]
fn every_case_answers_as_genuine_risc_os_5_did() {
    let cases = cases();
    let answers = answers();
    assert_eq!(
        cases.iter().map(|case| case.text.as_str()).collect::<Vec<_>>(),
        answers.iter().map(|(case, _)| case.as_str()).collect::<Vec<_>>(),
        "the answers were recorded for these cases: record them again with `cargo xtask riscos --reference`"
    );
    let mut vars = store();
    let mut numbers = Numbers::default();
    let mut differ = Vec::new();
    for (case, (_, theirs)) in cases.iter().zip(&answers) {
        let ours = answer(case, &mut vars, theirs);
        // What a command printed is its own, never the reading's.
        let theirs = theirs
            .split_once(" said ")
            .map_or(theirs.as_str(), |(head, _)| head);
        if !numbers.agree(&ours, theirs) {
            differ.push(format!(
                "cases.txt:{}: {}\n  RISC OS: {theirs}\n  ours:    {ours}",
                case.line, case.text
            ));
        }
    }
    assert!(
        differ.is_empty(),
        "{} of {} cases answer otherwise:\n{}",
        differ.len(),
        cases.len(),
        differ.join("\n")
    );
}
