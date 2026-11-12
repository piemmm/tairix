//! `DOSKEY`: its command line, then the resident copy at the shell's
//! prompt — commands recalled and edited, macros run, lists, and a
//! Ctrl+C that breaks as DOS's own line input would.

use super::*;
use crate::chips::keyboard::Scan;
use crate::dos::harness::{control, press, screen, settle, type_ahead, typed, C};
use crate::dos::utility::tests::{run, with_utilities};
use crate::machine::Machine;
use alloc::string::String;

/// Presses the key of scan code `code`, then lets the machine run until it
/// waits for a key again.
fn key(m: &mut Machine, code: u8) {
    let scan = Scan::plain(code);
    press(m, &[(scan, true), (scan, false)]);
    settle(m);
}

/// Types `text` with no Enter after it.
fn keyed(m: &mut Machine, text: &str) {
    type_ahead(m, text);
    settle(m);
}

/// The screen's last line with text on it.
fn last_line(m: &Machine) -> String {
    screen(m)
        .lines()
        .rev()
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .into()
}

/// A shell at its prompt with `DOSKEY` resident.
fn installed() -> Machine {
    let mut m = with_utilities(&[]);
    assert!(
        run(&mut m, "doskey").contains("DOSKey installed."),
        "{}",
        screen(&m)
    );
    m
}

const UP: u8 = 0x48;
const DOWN: u8 = 0x50;
const LEFT: u8 = 0x4B;
const HOME: u8 = 0x47;
const INSERT: u8 = 0x52;
const F7: u8 = 0x41;
const F9: u8 = 0x43;

#[test]
fn doskey_reads_its_switches_and_a_macro_to_the_lines_end() {
    let all = request(b" /bufsize=1024 /m /HISTORY /insert /reinstall").unwrap();
    assert_eq!(
        (all.size, all.macros, all.history, all.insert, all.reinstall),
        (Some(1024), true, true, Some(true), true)
    );
    let defined = request(b" /overstrike  d=dir /w  $*").unwrap();
    assert_eq!(
        defined.definition,
        Some((&b"d"[..], &b"dir /w  $*"[..])),
        "switches after the name are the macro's"
    );
    assert_eq!(defined.insert, Some(false));
    assert_eq!(
        request(b" gone=").unwrap().definition,
        Some((&b"gone"[..], &b""[..])),
        "no text deletes"
    );
    assert_eq!(
        request(b" /bufsize=100"),
        Err(Refused::Parse(Refusal::Range(b"/bufsize=100")))
    );
    assert_eq!(request(b" /x"), Err(Refused::Parse(Refusal::Switch(b"/x"))));
    assert_eq!(request(b" nothing"), Err(Refused::Macro));
    assert_eq!(request(b" two words=x"), Err(Refused::Macro));
    assert_eq!(request(b""), Ok(Request::default()));
}

#[test]
fn commands_typed_are_recalled_and_run_again() {
    let mut m = installed();
    typed(&mut m, "echo one");
    typed(&mut m, "echo two");
    key(&mut m, UP);
    assert!(last_line(&m).ends_with(">echo two"), "{}", screen(&m));
    key(&mut m, UP);
    assert!(last_line(&m).ends_with(">echo one"), "{}", screen(&m));
    key(&mut m, DOWN);
    assert!(last_line(&m).ends_with(">echo two"), "{}", screen(&m));
    typed(&mut m, "");
    let text = screen(&m);
    assert_eq!(text.matches("\ntwo\n").count(), 2, "run again:\n{text}");
    assert!(
        run(&mut m, "doskey /h").contains("\necho one\necho two\necho two\ncls\ndoskey /h\n"),
        "{}",
        screen(&m)
    );
}

#[test]
fn the_line_is_edited_over_or_into_and_cleared() {
    let mut m = installed();
    keyed(&mut m, "echo abc");
    key(&mut m, LEFT);
    key(&mut m, LEFT);
    keyed(&mut m, "X");
    assert!(
        last_line(&m).ends_with(">echo aXc"),
        "typed over: {}",
        screen(&m)
    );
    key(&mut m, INSERT);
    key(&mut m, LEFT);
    keyed(&mut m, "Y");
    assert!(
        last_line(&m).ends_with(">echo aYXc"),
        "put in: {}",
        screen(&m)
    );
    key(&mut m, HOME);
    keyed(&mut m, "\x1b");
    assert!(
        last_line(&m).ends_with('>'),
        "Esc clears it: {}",
        screen(&m)
    );
    keyed(&mut m, "echo done\r");
    assert!(screen(&m).contains("\ndone\n"), "{}", screen(&m));
}

#[test]
fn a_macro_runs_its_commands_with_the_words_given() {
    let mut m = installed();
    typed(&mut m, "doskey mc=md $1$tcd $1");
    run(&mut m, "mc games");
    assert!(
        last_line(&m).ends_with("A:\\GAMES>"),
        "made and entered:\n{}",
        screen(&m)
    );
    typed(&mut m, "cd \\");
    let text = run(&mut m, "doskey /m");
    assert!(text.contains("\nMC=md $1$tcd $1\n"), "{text}");
    run(&mut m, " mc x");
    assert!(
        screen(&m).contains("Bad command or file name"),
        "a blank first passes the macro by:\n{}",
        screen(&m)
    );
    typed(&mut m, "doskey mc=");
    assert!(
        !run(&mut m, "doskey /m").contains("MC="),
        "deleted:\n{}",
        screen(&m)
    );
}

#[test]
fn f7_lists_the_commands_and_f9_recalls_one_by_its_number() {
    let mut m = installed();
    typed(&mut m, "echo first");
    typed(&mut m, "echo second");
    key(&mut m, F7);
    let text = screen(&m);
    assert!(
        text.contains("\n1: echo first\n2: echo second\n"),
        "the line DOSKEY was started by is not its:\n{text}"
    );
    key(&mut m, F9);
    assert!(last_line(&m).ends_with("Line number:"), "{}", screen(&m));
    keyed(&mut m, "1\r");
    assert!(last_line(&m).ends_with("echo first"), "{}", screen(&m));
    typed(&mut m, "");
    assert!(screen(&m).contains("\nfirst\n"), "{}", screen(&m));
}

#[test]
fn ctrl_c_breaks_the_line_as_dos_does() {
    let mut m = installed();
    keyed(&mut m, "echo never");
    control(&mut m, C);
    settle(&mut m);
    let text = screen(&m);
    assert!(text.contains("echo never^C"), "{text}");
    assert!(last_line(&m).ends_with('>'), "a new prompt:\n{text}");
    typed(&mut m, "echo after");
    assert!(screen(&m).contains("\nafter\n"), "{}", screen(&m));
}

#[test]
fn a_second_run_changes_the_resident_copy_and_says_what_it_cannot() {
    let mut m = installed();
    let text = run(&mut m, "doskey /bufsize=1024");
    assert!(
        text.contains("Cannot change BUFSIZE") && !text.contains("installed"),
        "{text}"
    );
    assert!(
        run(&mut m, "doskey /x").contains("Invalid switch - /X"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "doskey nothing").contains("Invalid macro definition"),
        "{}",
        screen(&m)
    );
    let long = alloc::format!("doskey big={}", "x".repeat(100));
    typed(&mut m, &long);
    typed(&mut m, &long.replace("big", "bigger"));
    typed(&mut m, &long.replace("big", "biggest"));
    typed(&mut m, &long.replace("big", "huge"));
    assert!(
        run(&mut m, &long.replace("big", "last")).contains("Insufficient memory to store macro."),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "doskey /reinstall").contains("DOSKey installed."),
        "{}",
        screen(&m)
    );
    assert!(
        !run(&mut m, "doskey /m").contains("BIG="),
        "a new copy starts empty:\n{}",
        screen(&m)
    );
}

#[test]
fn a_line_from_a_file_is_passed_on_into_the_callers_buffer() {
    let mut m = crate::dos::utility::tests::with_utilities(&[("ANSWER.TXT", b"1-2-1990\r\n")]);
    run(&mut m, "doskey");
    run(&mut m, "date <answer.txt");
    assert!(
        run(&mut m, "date <nul").contains("01-02-1990"),
        "read through DOS into the shell's own buffer:\n{}",
        screen(&m)
    );
}

#[test]
fn breaks_never_use_up_the_resident_copys_levels() {
    let mut m = installed();
    typed(&mut m, "echo kept");
    for _ in 0..5 {
        keyed(&mut m, "abc");
        control(&mut m, C);
        settle(&mut m);
    }
    key(&mut m, UP);
    assert!(
        last_line(&m).ends_with(">echo kept"),
        "each break's call dropped, DOSKEY still answering:\n{}",
        screen(&m)
    );
}
