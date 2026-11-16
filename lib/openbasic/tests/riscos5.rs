//! The crate's BASIC V held to genuine RISC OS 5: every program in
//! `riscos5/programs.txt`, typed into BASIC V, saved and listed by `cargo xtask
//! riscos --reference`, which wrote what BASIC made of it to
//! `riscos5/answers.txt`. Each typed line tokenises, warns and is stored as
//! BASIC did it, and each program BASIC kept lists under each `LISTO` as BASIC
//! listed it, stopping where it stopped.

use tairix_openbasic::lineref;
use tairix_openbasic::list::{Lister, Options};
use tairix_openbasic::program;
use tairix_openbasic::tokenise::{tokenise, Start, Warnings};
use tairix_openbasic::{Dialect, Error};

const PROGRAMS: &str = include_str!("riscos5/programs.txt");
const ANSWERS: &str = include_str!("riscos5/answers.txt");

/// The bytes of every quoted string on `line`, `\xHH` read as its byte.
fn quoted(line: &str) -> Vec<Vec<u8>> {
    let bytes = line.as_bytes();
    let mut strings = Vec::new();
    let mut at = 0;
    while let Some(open) = bytes[at..].iter().position(|&b| b == b'"') {
        let mut string = Vec::new();
        at += open + 1;
        while let Some(&b) = bytes.get(at) {
            at += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).expect("hex");
                    string.push(u8::from_str_radix(hex, 16).expect("an escape"));
                    at += 3;
                }
                other => string.push(other),
            }
        }
        strings.push(string);
    }
    strings
}

/// The one quoted string on `line`.
fn one(line: &str) -> Vec<u8> {
    let [text] = quoted(line).try_into().expect("one quoted string");
    text
}

/// What BASIC made of a typed line.
#[derive(Debug, PartialEq, Eq)]
enum Kept {
    /// Stored as these bytes, with these warnings.
    Stored(Vec<u8>, Warnings),
    /// Refused as longer than a line holds.
    Refused,
}

/// A program as typed, and what BASIC made of it.
#[derive(Default)]
struct Program {
    entry: u8,
    listos: Vec<u8>,
    lines: Vec<Vec<u8>>,
    poked: bool,
    kept: Vec<Kept>,
    /// Each listing, and the line whose reference stopped it.
    listed: Vec<(u8, Vec<u8>, Option<u16>)>,
}

/// A `program ENTRY LISTO...` line's numbers.
fn numbers(rest: &str) -> (u8, Vec<u8>) {
    let numbers: Vec<u8> = rest
        .split(' ')
        .map(|n| n.parse().expect("a LISTO"))
        .collect();
    (numbers[0], numbers[1..].to_vec())
}

/// The programs typed: the cases' own.
fn typed() -> Vec<Program> {
    let mut programs: Vec<Program> = Vec::new();
    for line in PROGRAMS
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        if let Some(rest) = line.strip_prefix("program ") {
            let (entry, listos) = numbers(rest);
            programs.push(Program {
                entry,
                listos,
                ..Program::default()
            });
            continue;
        }
        let program = programs.last_mut().expect("a program");
        match line.split_once(' ').expect("a kind").0 {
            "line" => program.lines.push(one(line)),
            "poke" => program.poked = true,
            other => panic!("no kind {other}"),
        }
    }
    programs
}

fn warnings(names: &str) -> Warnings {
    let mut warnings = Warnings::default();
    for name in names.split(' ') {
        match name {
            "quote" => warnings.quote = true,
            "brackets" => warnings.brackets = true,
            "line" => warnings.line_number = true,
            "none" => {}
            other => panic!("no warning {other}"),
        }
    }
    warnings
}

/// What the answers record: each program, the calibration first.
fn recorded() -> Vec<Program> {
    let mut programs: Vec<Program> = Vec::new();
    let mut lines = ANSWERS.lines().filter(|l| !l.starts_with('#'));
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("program ") {
            let (entry, listos) = numbers(rest);
            programs.push(Program {
                entry,
                listos,
                ..Program::default()
            });
            continue;
        }
        let program = programs.last_mut().expect("a program");
        let answer = lines
            .next()
            .and_then(|l| l.strip_prefix("= "))
            .unwrap_or_else(|| panic!("an answer after {line:?}"));
        if line.starts_with("line ") {
            program.lines.push(one(line));
            program.kept.push(if answer == "refused long" {
                Kept::Refused
            } else {
                let (stored, warned) = answer
                    .strip_prefix("stored ")
                    .and_then(|rest| rest.rsplit_once(" warns "))
                    .expect("stored bytes and warnings");
                Kept::Stored(one(stored), warnings(warned))
            });
        } else if let Some(listo) = line.strip_prefix("listo ") {
            let (listing, stopped) = match answer.rsplit_once("\" stopped ") {
                Some((listing, at)) => (format!("{listing}\""), Some(at.parse().expect("a line"))),
                None => (answer.to_string(), None),
            };
            program
                .listed
                .push((listo.parse().expect("a LISTO"), one(&listing), stopped));
        } else {
            panic!("no answer kind for {line:?}");
        }
    }
    programs
}

/// `text` typed, as BASIC V's line entry keeps it under `entry`: its number,
/// and its text stored, or refused; and its warnings.
fn kept(text: &[u8], entry: u8) -> (u16, Result<Vec<u8>, Error>, Warnings) {
    let mut bytes = Vec::new();
    let tokenised = tokenise(Dialect::BasicV, text, Start::Line, &mut bytes).expect("memory");
    assert_eq!(bytes[0], lineref::MARKER, "a typed line's number first");
    let number = lineref::decode([bytes[1], bytes[2], bytes[3]]);
    let stored = program::stored(Dialect::BasicV, &bytes[4..], Options::new(entry))
        .map(|text| text.to_vec());
    (number, stored, tokenised.warnings)
}

/// The program `lines` make, numbered and stored, listed under `listo`, each
/// LF a screen's LF CR; and the line whose reference stopped it.
fn listing(lines: &[(u16, &[u8])], listo: u8) -> (Vec<u8>, Option<u16>) {
    let mut lister = Lister::new(Dialect::BasicV, Options::new(listo));
    let mut out = Vec::new();
    let mut stopped = None;
    for &(number, text) in lines {
        match lister.line(number, text, &mut out) {
            Ok(()) => {}
            Err(Error::ReferenceListed) => {
                stopped = Some(number);
                break;
            }
            Err(error) => panic!("listing line {number}: {error:?}"),
        }
    }
    let mut screen = Vec::with_capacity(out.len() * 2);
    for b in out {
        screen.push(b);
        if b == b'\n' {
            screen.push(b'\r');
        }
    }
    (screen, stopped)
}

fn shown(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) && b != b'\\' {
                char::from(b).to_string()
            } else {
                format!("\\x{b:02X}")
            }
        })
        .collect()
}

#[test]
fn every_program_is_kept_and_listed_as_genuine_risc_os_5_did() {
    let typed = typed();
    let recorded = recorded();
    assert_eq!(
        recorded.len(),
        typed.len() + 1,
        "the calibration and each program: record them again with `cargo xtask riscos --reference`"
    );
    for (typed, recorded) in typed.iter().zip(&recorded[1..]) {
        assert!(
            typed.entry == recorded.entry
                && typed.listos == recorded.listos
                && typed.lines == recorded.lines,
            "the answers were recorded for these programs: record them again with `cargo xtask riscos --reference`"
        );
    }
    let mut differ = Vec::new();
    for (index, program) in recorded.iter().enumerate() {
        let poked = index > 0 && typed[index - 1].poked;
        let mut lines: Vec<(u16, &[u8])> = Vec::new();
        for (text, kept_by_basic) in program.lines.iter().zip(&program.kept) {
            let (number, stored, warned) = kept(text, program.entry);
            let ours = match stored {
                Ok(bytes) => Kept::Stored(bytes, warned),
                Err(Error::LineTooLong) => Kept::Refused,
                Err(error) => panic!("{}: {error:?}", shown(text)),
            };
            let agrees = match (&ours, kept_by_basic) {
                // A poked program's bytes are its pokes', not its typing's.
                (Kept::Stored(_, ours), Kept::Stored(_, basic)) if poked => ours == basic,
                (ours, basic) => ours == basic,
            };
            if !agrees {
                differ.push(format!(
                    "program {index}: {}\n  RISC OS: {kept_by_basic:?}\n  ours:    {ours:?}",
                    shown(text)
                ));
            }
            if let Kept::Stored(bytes, _) = kept_by_basic {
                lines.push((number, bytes));
            }
        }
        for (listo, listed, stopped) in &program.listed {
            let (ours, our_stop) = listing(&lines, *listo);
            if (&ours, our_stop) != (listed, *stopped) {
                differ.push(format!(
                    "program {index} under LISTO {listo}:\n  RISC OS: {} stopped {stopped:?}\n  ours:    {} stopped {our_stop:?}",
                    shown(listed),
                    shown(&ours)
                ));
            }
        }
    }
    assert!(
        differ.is_empty(),
        "{} differences:\n{}",
        differ.len(),
        differ.join("\n")
    );
}
