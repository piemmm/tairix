//! `XCOPY`: its command line and how it lists a source, then each copy run
//! at the shell's prompt.

use super::{listed_base, request, switch, Refused, Request};
use crate::dos::harness::{screen, settle, type_ahead, typed};
use crate::dos::utility::tests::{run, with_utilities};
use crate::dos::utility::Refusal;

#[test]
fn xcopy_reads_its_names_and_switches() {
    let got = request(b" a:*.txt b: /s /e /v /m /p /w /d:1-2-90");
    let all = switch::SUBDIRECTORIES
        | switch::EMPTY_TOO
        | switch::VERIFY
        | switch::MOVE_ARCHIVE
        | switch::PROMPT
        | switch::WAIT
        | switch::SINCE;
    assert_eq!(
        got,
        Ok(Request {
            source: b"a:*.txt",
            destination: Some(b"b:"),
            switches: all,
            since: Some(b"1-2-90")
        })
    );
    assert_eq!(request(b" a.txt").map(|r| r.destination), Ok(None));
    assert_eq!(request(b""), Err(Refused::Fields));
    assert_eq!(request(b" a b c"), Err(Refused::Fields));
    assert_eq!(
        request(b" a /q"),
        Err(Refused::Parser(Refusal::Switch(b"/q")))
    );
    assert_eq!(
        request(b" a /d:"),
        Err(Refused::Parser(Refusal::Switch(b"/d:")))
    );
}

#[test]
fn xcopy_lists_a_source_as_dos_does() {
    assert_eq!(listed_base(b"a:*.txt", false), b"A:");
    assert_eq!(listed_base(b"a:\\sub\\*.txt", false), b"A:\\SUB\\");
    assert_eq!(listed_base(b"*.txt", false), b"");
    assert_eq!(listed_base(b"a:\\sub", true), b"A:\\SUB\\");
    assert_eq!(listed_base(b"a:", true), b"A:");
    assert_eq!(listed_base(b"a:\\", true), b"A:\\");
}

#[test]
fn xcopy_copies_a_tree_and_counts_the_files() {
    let mut m = with_utilities(&[("ONE.TXT", b"1"), ("TWO.TXT", b"2")]);
    for line in [
        "md src",
        "md src\\sub",
        "md src\\empty",
        "copy one.txt src",
        "copy two.txt src\\sub",
    ] {
        typed(&mut m, line);
    }
    let text = run(&mut m, "xcopy src dst\\ /s");
    assert!(text.contains("Reading source file(s)...\nSRC\\ONE.TXT\nSRC\\SUB\\TWO.TXT\n        2 File(s) copied\n"), "{text}");
    assert!(
        run(&mut m, "type dst\\sub\\two.txt").contains("\n2"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "dir dst\\empty").contains("File not found"),
        "an empty directory is made only with /E:\n{}",
        screen(&m)
    );
    typed(&mut m, "xcopy src dst2\\ /s /e");
    assert!(
        !run(&mut m, "dir dst2\\empty").contains("File not found"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "xcopy src src\\sub /s").contains("Cannot perform a cyclic copy"),
        "{}",
        screen(&m)
    );
}

#[test]
fn xcopy_asks_whether_a_new_destination_is_a_file_or_a_directory() {
    let mut m = with_utilities(&[("ONE.TXT", b"1")]);
    let text = run(&mut m, "xcopy one.txt new");
    assert!(text.trim_end().ends_with("Does NEW specify a file name\nor directory name on the target\n(F = file, D = directory)?"), "{text}");
    type_ahead(&mut m, "d");
    settle(&mut m);
    assert!(
        screen(&m).contains("ONE.TXT\n        1 File(s) copied"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "type new\\one.txt").contains("\n1"),
        "{}",
        screen(&m)
    );
    run(&mut m, "xcopy one.txt copy.txt");
    type_ahead(&mut m, "f");
    settle(&mut m);
    assert!(
        run(&mut m, "type copy.txt").contains("\n1"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "xcopy one.txt *.bak").contains("1 File(s) copied"),
        "a pattern names the copy without asking:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "type one.bak").contains("\n1"),
        "{}",
        screen(&m)
    );
}

#[test]
fn xcopy_takes_what_its_switches_say() {
    let mut m = with_utilities(&[("OLD.TXT", b"o"), ("NEW.TXT", b"n")]);
    typed(&mut m, "md out");
    typed(&mut m, "attrib -a old.txt");
    let text = run(&mut m, "xcopy *.txt out /m");
    assert!(text.contains("NEW.TXT\n        1 File(s) copied"), "{text}");
    assert!(
        run(&mut m, "attrib new.txt").contains("           A:\\NEW.TXT"),
        "/M clears the archive bit:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "xcopy *.txt out /a").contains("0 File(s) copied"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "xcopy gone.txt out");
    assert!(
        text.contains("File not found - GONE.TXT\n        0 File(s) copied"),
        "{text}"
    );
    assert!(
        run(&mut m, "xcopy *.txt out /d:13-45-99").contains("Invalid date"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "xcopy *.txt out /p");
    assert!(text.trim_end().ends_with("OLD.TXT (Y/N)?"), "{text}");
    type_ahead(&mut m, "n");
    settle(&mut m);
    assert!(
        screen(&m).trim_end().ends_with("NEW.TXT (Y/N)?"),
        "{}",
        screen(&m)
    );
    type_ahead(&mut m, "y");
    settle(&mut m);
    assert!(screen(&m).contains("1 File(s) copied"), "{}", screen(&m));
}

#[test]
fn xcopy_ends_with_dos_5_s_codes() {
    let codes = b"@echo off\r\nxcopy one.txt out >nul\r\nif not errorlevel 1 echo copied\r\nxcopy gone.txt out >nul\r\nif errorlevel 1 if not errorlevel 2 echo none\r\nxcopy one.txt out /q >nul\r\nif errorlevel 4 echo refused\r\n";
    let mut m = with_utilities(&[("ONE.TXT", b"1"), ("CODES.BAT", codes)]);
    typed(&mut m, "md out");
    let text = run(&mut m, "codes");
    assert!(
        text.contains("copied\nnone\nInvalid switch - /Q\nrefused\n"),
        "a refusal goes to the standard error:\n{text}"
    );
}

#[test]
fn an_input_with_no_answer_left_ends_the_copy_or_answers_no() {
    let ended = b"@echo off\r\nxcopy one.txt new <nul >nul\r\nif errorlevel 2 if not errorlevel 3 echo broken\r\n";
    let mut m = with_utilities(&[("ONE.TXT", b"1"), ("ENDED.BAT", ended)]);
    assert!(
        run(&mut m, "ended").contains("\nbroken\n"),
        "the copy ends as Ctrl+C would end it:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "dir new").contains("File not found"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "md out");
    let text = run(&mut m, "xcopy *.txt out /p <nul");
    assert!(
        text.contains("ONE.TXT (Y/N)?\n        0 File(s) copied"),
        "no answer left is no:\n{text}"
    );
}
