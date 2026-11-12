//! `FC`: its command line and its line matching, then each compare run at
//! the shell's prompt.

use super::lines::{matching, resync, Compare};
use super::{request, switch, Refused, Request};
use crate::dos::harness::screen;
use crate::dos::utility::tests::{run, with_utilities};
use alloc::vec::Vec;

fn lines(text: &[&str]) -> Vec<Vec<u8>> {
    text.iter().map(|line| line.as_bytes().to_vec()).collect()
}

fn plain() -> Compare {
    Compare::new(0, core::array::from_fn(|n| 0x80 + u8::try_from(n).unwrap()))
}

#[test]
fn fc_reads_its_switches_and_files() {
    let got = |tail: &'static [u8]| request(tail);
    let text = Request {
        switches: switch::LINES,
        sync: 2,
        lines: 100,
        files: [b"a.txt", b"b.txt"],
    };
    assert_eq!(got(b" a.txt b.txt"), Ok(text));
    assert_eq!(
        got(b" a.exe b.exe").map(|r| r.switches),
        Ok(switch::BINARY),
        "a program's file is compared by byte"
    );
    assert_eq!(
        got(b" /b a.txt b.txt").map(|r| r.switches),
        Ok(switch::BINARY)
    );
    let all = switch::LINES
        | switch::IGNORE_CASE
        | switch::IGNORE_SPACE
        | switch::NUMBERS
        | switch::ABBREVIATE
        | switch::KEEP_TABS;
    assert_eq!(got(b" /c /w /n /a /t a b").map(|r| r.switches), Ok(all));
    assert_eq!(got(b" /lb50 a b").map(|r| r.lines), Ok(50));
    assert_eq!(
        got(b" /5 a.com b").map(|r| (r.sync, r.switches)),
        Ok((5, switch::LINES)),
        "a count asks for lines"
    );
    assert_eq!(
        got(b" a.txt/n b.txt").map(|r| (r.files[0], r.switches)),
        Ok((&b"a.txt"[..], switch::LINES | switch::NUMBERS))
    );
    assert_eq!(got(b" /n/3 a b").map(|r| r.sync), Ok(3));
    assert_eq!(
        got(b" /3/n a b"),
        Err(Refused::Usage),
        "a count runs to its word's end"
    );
    assert_eq!(got(b" /b /l a b"), Err(Refused::Incompatible));
    assert_eq!(got(b" /b /n a b"), Err(Refused::Incompatible));
    assert_eq!(got(b" /b /3 a b"), Err(Refused::Incompatible));
    assert_eq!(got(b" /lb0 a b"), Err(Refused::Usage));
    assert_eq!(got(b" /x a b"), Err(Refused::Usage));
    assert_eq!(got(b" a"), Err(Refused::Usage));
    assert_eq!(got(b" a b c"), Err(Refused::Usage));
}

#[test]
fn fc_finds_where_the_files_match_again() {
    let compare = plain();
    let (one, two) = (lines(&["a", "b", "c", "d"]), lines(&["a", "x", "c", "d"]));
    assert_eq!(
        matching(&one, &two, &compare),
        0,
        "the line before the difference is kept"
    );
    assert_eq!(
        resync(&one, &two, 2, &compare),
        Some((2, 2)),
        "a change: each shown to the match"
    );
    let (one, two) = (lines(&["a", "b", "c"]), lines(&["a", "x", "b", "c"]));
    assert_eq!(
        resync(&one, &two, 2, &compare),
        Some((1, 2)),
        "an insertion"
    );
    let (one, two) = (lines(&["a", "x", "b", "c"]), lines(&["a", "b", "c"]));
    assert_eq!(resync(&one, &two, 2, &compare), Some((2, 1)), "a deletion");
    assert_eq!(
        resync(&lines(&["a", "b"]), &lines(&["x", "y"]), 2, &compare),
        None
    );
    assert_eq!(
        resync(&lines(&[]), &lines(&["x"]), 2, &compare),
        None,
        "one file at its end"
    );
    let same = lines(&["a", "b"]);
    assert_eq!(
        matching(&same, &same, &compare),
        2,
        "nothing different: every line dropped"
    );
}

#[test]
fn fc_compares_case_and_spaces_as_asked() {
    let upper = core::array::from_fn(|n| 0x80 + u8::try_from(n).unwrap());
    let cased = Compare::new(switch::IGNORE_CASE, upper);
    assert!(cased.same(b"Hello", b"hELLO"));
    assert!(!plain().same(b"Hello", b"hELLO"));
    let spaced = Compare::new(switch::IGNORE_SPACE, upper);
    assert!(spaced.same(b"a  b\t c", b"abc"));
    assert!(!plain().same(b"a b", b"ab"));
    assert!(
        plain().same(b"same\0one", b"same\0two"),
        "a line ends at a NUL, as C's does"
    );
}

const ONE: &[u8] = b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\n";
const TWO: &[u8] = b"first\r\nSECOND\r\nthird\r\nfourth\r\nfifth\r\n";

#[test]
fn fc_shows_each_difference_between_the_lines_either_side() {
    let codes = b"@echo off\r\nfc one.txt one.txt >nul\r\nif not errorlevel 1 echo same\r\nfc one.txt two.txt >nul\r\nif errorlevel 1 echo different\r\n";
    let mut m = with_utilities(&[("ONE.TXT", ONE), ("TWO.TXT", TWO), ("CODES.BAT", codes)]);
    let text = run(&mut m, "fc one.txt two.txt");
    assert!(
        text.contains(
            "\n***** one.txt\nfirst\nsecond\nthird\n***** two.txt\nfirst\nSECOND\nthird\n*****\n"
        ),
        "{text}"
    );
    assert!(
        run(&mut m, "fc /c one.txt two.txt").contains("fc: no differences encountered"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "fc /n one.txt two.txt");
    assert!(text.contains("    2:  SECOND\n"), "{text}");
    assert!(
        run(&mut m, "codes").contains("same\ndifferent\n"),
        "{}",
        screen(&m)
    );
}

#[test]
fn fc_shows_added_lines_and_abbreviates_a_long_run() {
    let long: Vec<u8> = b"a\r\n"
        .iter()
        .copied()
        .chain((1..=6).flat_map(|n| alloc::format!("new {n}\r\n").into_bytes()))
        .chain(*b"b\r\nc\r\n")
        .collect();
    let mut m = with_utilities(&[("SHORT.TXT", b"a\r\nb\r\nc\r\n"), ("LONG.TXT", &long)]);
    let text = run(&mut m, "fc short.txt long.txt");
    assert!(
        text.contains("***** short.txt\na\nb\n***** long.txt\na\nnew 1\n"),
        "{text}"
    );
    assert!(text.contains("new 6\nb\n*****\n"), "{text}");
    let text = run(&mut m, "fc /a short.txt long.txt");
    assert!(
        text.contains("***** long.txt\na\n...\nb\n*****\n"),
        "{text}"
    );
}

#[test]
fn fc_expands_tabs_and_passes_over_spaces_when_asked() {
    let files: [(&str, &[u8]); 3] = [
        ("TABS.TXT", b"a\tb\r\n"),
        ("SPACED.TXT", b"a       b\r\n"),
        ("PACKED.TXT", b"ab\r\n\r\n"),
    ];
    let mut m = with_utilities(&files);
    assert!(
        run(&mut m, "fc tabs.txt spaced.txt").contains("no differences"),
        "a tab is spaces to the next eighth column:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fc /t tabs.txt spaced.txt").contains("*****"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fc /w spaced.txt packed.txt").contains("no differences"),
        "white space and blank lines passed over:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fc spaced.txt packed.txt").contains("*****"),
        "{}",
        screen(&m)
    );
}

#[test]
fn fc_cuts_a_long_line_as_dos_does() {
    let mut long = alloc::vec![b'x'; 130];
    long.extend_from_slice(b"\r\n");
    let mut cut = alloc::vec![b'x'; 127];
    cut.extend_from_slice(b"\r\n\r\nxxx\r\n");
    let mut m = with_utilities(&[("LONG.TXT", &long), ("CUT.TXT", &cut)]);
    let text = run(&mut m, "fc long.txt cut.txt");
    assert!(
        text.contains("***** cut.txt\n") && text.contains("\n\nxxx\n*****"),
        "127 characters, then the rest a line of its own:\n{text}"
    );
}

#[test]
fn fc_compares_bytes_and_says_which_file_is_longer() {
    let files: [(&str, &[u8]); 4] = [
        ("A.BIN", b"ABCDEF"),
        ("B.BIN", b"ABXDEFGH"),
        ("A.COM", b"\x90\x90"),
        ("B.COM", b"\x90\xC3"),
    ];
    let mut m = with_utilities(&files);
    let text = run(&mut m, "fc /b a.bin b.bin");
    assert!(
        text.contains("\n00000002: 43 58\nfc: b.bin longer than a.bin\n"),
        "{text}"
    );
    let text = run(&mut m, "fc a.com b.com");
    assert!(
        text.contains("00000001: 90 C3\n"),
        "a program is compared by byte:\n{text}"
    );
    assert!(
        run(&mut m, "fc /b a.bin a.bin").contains("fc: no differences encountered"),
        "{}",
        screen(&m)
    );
}

#[test]
fn fc_says_what_it_cannot_do() {
    let mut m = with_utilities(&[("ONE.TXT", ONE)]);
    assert!(
        run(&mut m, "fc gone.txt one.txt")
            .contains("fc: cannot open gone.txt - No such file or directory"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fc one.txt")
            .contains("usage: fc [/a] [/b] [/c] [/l] [/lbNN] [/w] [/t] [/n] [/NNNN] file1 file2"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "fc /b /n one.txt one.txt").contains("fc: Incompatible switches"),
        "{}",
        screen(&m)
    );
    let other: Vec<u8> = (0..120)
        .flat_map(|n| alloc::format!("other {n}\r\n").into_bytes())
        .collect();
    let mine: Vec<u8> = (0..120)
        .flat_map(|n| alloc::format!("mine {n}\r\n").into_bytes())
        .collect();
    let mut m = with_utilities(&[("MINE.TXT", &mine), ("OTHER.TXT", &other)]);
    let text = run(&mut m, "fc /lb10 mine.txt other.txt | find \"Resync\"");
    assert!(
        text.contains("\nResync failed.  Files are too different\n"),
        "{text}"
    );
}
