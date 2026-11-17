//! The tokeniser and lister against lib/openbasic's BASIC II: a typed line is
//! kept as the library tokenises it, and a program lists as it lists one.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_fuzzseed::Prng;
use tairix_openbasic::basic2::KEYWORDS;
use tairix_openbasic::list::{Lister, Options};
use tairix_openbasic::program::{self, Line};
use tairix_openbasic::tokenise::{tokenise, Start};
use tairix_openbasic::{lineref, Dialect};

use super::super::editor::LISTING_LOOPS;
use super::super::ws::zp as z;
use super::{after, basic, from, typed};
use crate::machine::bbc::Machine;
use crate::scenario::{mode7_text, type_text, wait};

/// The program from PAGE to TOP.
fn kept(machine: &Machine) -> Vec<u8> {
    let page = u16::from(machine.peek(z::PAGE)) << 8;
    let top = u16::from_le_bytes([machine.peek(z::TOP), machine.peek(z::TOP + 1)]);
    (page..top).map(|at| machine.peek(at)).collect()
}

/// The program `typed` leaves, as the library keeps one: each line
/// replacing any of its number, a number alone deleting one.
fn program_of<S: AsRef<str>>(typed: &[S]) -> BTreeMap<u16, Vec<u8>> {
    let mut program = BTreeMap::new();
    for line in typed {
        let line = line.as_ref();
        let mut bytes = Vec::new();
        tokenise(Dialect::BasicII, line.as_bytes(), Start::Line, &mut bytes).expect("memory");
        let (reference, text) = bytes.split_at(1 + lineref::ENCODED);
        assert_eq!(reference[0], lineref::MARKER, "{line} is numbered");
        let number = lineref::decode(reference[1..].try_into().expect("its bytes"));
        if text.is_empty() {
            program.remove(&number);
        } else {
            let stored =
                program::stored(Dialect::BasicII, text, Options::default()).expect("memory");
            program.insert(number, stored.into_owned());
        }
    }
    program
}

fn image(program: &BTreeMap<u16, Vec<u8>>) -> Vec<u8> {
    program::image(program.iter().map(|(&number, text)| Line { number, text })).expect("a program")
}

/// `program` listed by the library under `listo`, as mode 7's rows show
/// it: a line a row each forty columns and one more, `#`, `_` and `£`
/// exchanged as it stores them, trailing spaces unseen.
fn listed(program: &BTreeMap<u16, Vec<u8>>, listo: u8) -> Vec<String> {
    let mut lister = Lister::new(Dialect::BasicII, Options::new(listo));
    let mut out = Vec::new();
    for (&number, text) in program {
        lister.line(number, text, &mut out).expect("memory");
    }
    let shown: Vec<char> = out
        .iter()
        .map(|&b| match b {
            b'#' => '_',
            b'_' => '`',
            b'`' => '#',
            b => char::from(b),
        })
        .collect();
    let mut rows = Vec::new();
    for line in shown.split(|&c| c == '\n').filter(|line| !line.is_empty()) {
        for at in (0..=line.len()).step_by(COLUMNS) {
            let row: String = line[at..line.len().min(at + COLUMNS)].iter().collect();
            rows.push(String::from(row.trim_end_matches(' ')));
        }
    }
    rows
}

/// Lines through each of the tokeniser's rules, then one replaced and one
/// deleted.
const LINES: [&str; 30] = [
    "10PRINT\"A\";",
    "20 GOTO 10,20",
    "30TIMER=TIME:X=TI.X",
    "40TIME=0:PAGE=&1900:P.PAGE",
    "50REM PRINT 10",
    "60DATA GOTO,1",
    "70*CAT PRINT",
    "80X=2*PI:Y=.5E3",
    "90PROCDRAW(FNSIZE)",
    "100GOTO 70000,20",
    "110IF X THEN 10 ELSE 20",
    "120ON X GOSUB 10,20",
    "130A=&FF+&DEAD:B=&FG",
    "140PRINTAND OR. DE. T.",
    "150RESTORE 10:TRACE 20",
    "160A$=\"OPEN",
    "170FOR I=1 TO 2 STEP 3:NEXT",
    "180REPEAT UNTIL TRUE",
    "190AUTO 10:DELETE 5,6",
    "200X_1=`2:LET Y=1",
    "210CLOSE#X:BPUT#X,1",
    "220 10 20 30",
    "230LINE INPUT A$:PRINT TOP",
    "240ENDPROC:END:ENDX",
    "250ERROR 1:ON ERROR OFF",
    "0260GOTO 65535:GOTO 65536",
    "270X=1:PTR#X=0:HIMEM=LOMEM",
    "280  PRINT  ",
    "10 PRINT",
    "220",
];

#[test]
fn a_typed_line_is_kept_as_the_library_tokenises_it() {
    let machine = typed(&LINES, 50);
    let program = program_of(&LINES);
    assert_eq!(
        program.len(),
        LINES.len() - 3,
        "one replaced and one deleted"
    );
    assert_eq!(kept(&machine), image(&program));
}

#[test]
fn a_program_lists_as_the_library_lists_it() {
    let program = [
        "10FOR I=1 TO 2",
        "20REPEAT:FOR J=1 TO 2",
        "30PRINT I,J",
        "40NEXT",
        "50UNTIL TRUE",
        "60TIME=0:NEXT",
        "70GOTO 10",
    ];
    for listo in 0..8 {
        let set = format!("LISTO {listo}");
        let mut lines = Vec::from(program);
        lines.extend([set.as_str(), "LIST"]);
        let mut expected = listed(&program_of(&program), listo);
        expected.insert(0, String::from(">LIST"));
        expected.push(String::from(">"));
        assert_eq!(from(&after(&lines, 50), ">LIST"), expected, "LISTO {listo}");
    }
}

/// What a drawn line's text is made of beside keywords: every character the
/// keyboard types with CAPS LOCK on but RETURN.
const PUNCTUATION: &[u8] = b" !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
const NAME: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_`";
/// A drawn line's longest text, well inside the input buffer.
const MOST_TEXT: usize = 30;
/// The rows a listing may fill, past which it scrolls and is compared by
/// its program alone, and the columns a row holds.
const ROWS: usize = 22;
const COLUMNS: usize = 40;

/// A piece of a drawn line's text.
fn piece(rng: &mut Prng, out: &mut String) {
    let char_of = |b: &u8| char::from(*b);
    match rng.below(7) {
        0 | 1 => out.push_str(rng.pick(&KEYWORDS).word.name),
        2 => {
            let name = rng.pick(&KEYWORDS).word.name;
            out.push_str(&name[..=rng.below(name.len())]);
            out.push('.');
        }
        3 => out.extend((0..=rng.below(3)).map(|_| char_of(rng.pick(NAME)))),
        4 => {
            let number = *rng.pick(&["0", "7", "65", "999", "32767", "65535", "65536", "70000"]);
            out.push_str(number);
            if rng.below(3) == 0 {
                out.push_str(rng.pick(&[".", ".5", "E3", ".1E-2"]));
            }
        }
        5 => {
            out.push('"');
            out.extend((0..rng.below(6)).map(|_| char_of(rng.pick(NAME))));
            if rng.below(4) != 0 {
                out.push('"');
            }
        }
        _ => out.extend((0..=rng.below(2)).map(|_| char_of(rng.pick(PUNCTUATION)))),
    }
}

/// A drawn line: a number, often one of a few so lines replace each other,
/// then text that cannot run on its digits, or now and then none, deleting.
fn drawn_line(rng: &mut Prng) -> String {
    let number = if rng.below(2) == 0 {
        1 + rng.below(8)
    } else {
        1 + rng.below(32_767)
    };
    let mut line = format!("{number}");
    if rng.below(10) == 0 {
        return line;
    }
    let mut text = String::new();
    while text.is_empty() || text.starts_with(|c: char| c.is_ascii_digit()) {
        text.clear();
        piece(rng, &mut text);
    }
    while text.len() < MOST_TEXT && rng.below(6) != 0 {
        piece(rng, &mut text);
    }
    line.push_str(&text);
    line
}

fn type_line(machine: &mut Machine, line: &str) {
    type_text(machine, line).expect("a key types each character");
    type_text(machine, "\n").expect("RETURN");
}

#[test]
fn a_drawn_program_is_kept_and_listed_as_the_library_does() {
    let name = "a_drawn_program_is_kept_and_listed_as_the_library_does";
    let mut rng = Prng::new(tairix_fuzzseed::start(name, tairix_fuzzseed::FUZZ_SEED_ENV));
    let mut machine = basic();
    for _ in 0..6 {
        type_line(&mut machine, "NEW");
        let lines: Vec<String> = (0..=rng.below(8)).map(|_| drawn_line(&mut rng)).collect();
        for line in &lines {
            type_line(&mut machine, line);
        }
        wait(&mut machine, 20);
        let program = program_of(&lines);
        assert_eq!(kept(&machine), image(&program), "{lines:?}");

        let listo = u8::try_from(rng.below(8)).expect("a LISTO");
        let listing = listed(&program, listo);
        if listing.len() <= ROWS {
            for line in [format!("LISTO {listo}").as_str(), "CLS", "LIST"] {
                type_line(&mut machine, line);
            }
            wait(&mut machine, 50);
            let mut expected = listing;
            expected.insert(0, String::from(">LIST"));
            expected.push(String::from(">"));
            let screen = mode7_text(&machine);
            assert_eq!(from(&screen, ">LIST"), expected, "LISTO {listo}: {lines:?}");
        }
    }
}

/// The regression: LISTO's indent was the loops summed and doubled in a
/// byte, where BASIC II prints each kind's in full, but one space for 128 or
/// more of a kind.
#[test]
fn listo_s_indent_is_each_loop_kind_s_in_full() {
    let run = |keyword: &str, count: usize| {
        format!("{keyword}{}", format!(":{keyword}").repeat(count - 1))
    };
    let (fors, repeats) = (run("FOR", 57), run("REPEAT", 33));
    let program = [
        format!("10{fors}"),
        format!("20{fors}"),
        format!("30{repeats}"),
        format!("40{repeats}"),
        String::from("50X"),
        format!("60{fors}"),
        String::from("70X"),
    ];
    let mut lines: Vec<&str> = program.iter().map(String::as_str).collect();
    lines.extend(["LISTO 6", "CLS", "LIST"]);
    let screen = after(&lines, 300);
    let shown = from(&screen, &screen[0]);
    let mut expected = listed(&program_of(&program), 6);
    expected.push(String::from(">"));
    assert_eq!(shown, expected[expected.len() - shown.len()..]);
}

/// The regression: a NEXT or UNTIL with more than 128 of its loops open
/// left the count as it was, its decrement undone, where one is closed.
#[test]
fn a_loop_closes_however_many_are_open() {
    let fors = format!("FOR{}", ":FOR".repeat(56));
    let lines: Vec<String> = (1..=3)
        .map(|n| format!("{n}0{fors}"))
        .chain([
            String::from("40NEXT:NEXT:REPEAT:UNTIL"),
            String::from("LIST"),
        ])
        .collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let machine = typed(&lines, 100);
    let open = [machine.peek(LISTING_LOOPS), machine.peek(LISTING_LOOPS + 1)];
    assert_eq!(open, [3 * 57 - 2, 0], "FOR's and REPEAT's");
}
