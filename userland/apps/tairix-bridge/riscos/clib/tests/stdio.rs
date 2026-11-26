//! The C library's streams in C programs: chunk 2 taken beside the library
//! kernel, the standard streams laid by `_clib_initialise`, `_main` reading
//! the command line into `main`'s arguments and redirections, and every
//! stream delivered and closed as the program ends.

mod common;

use common::{program, run, run_program, Build, Calls, Probe};
use tairix_riscos_filing::session::disc_file;
use tairix_riscos_kernel::os::{Ended, Outcome};

/// Where the C library's statics start in the library's, which the kit
/// saves whole.
const C_STATICS: u32 = 0x31C;
/// A `FILE` block's bytes and its flags' and extra block's words.
const FILE_BYTES: u32 = 40;
const FLAG: u32 = 12;
const EXTRA: u32 = 36;

/// `_main` called with the command line and `mainfn`, which `body` follows.
fn entering_main(body: &str) -> String {
    let mut calls = Calls::default();
    calls.call(7, &[], false);
    calls.raw("ADR r1, mainfn");
    calls.raw("BL cvec+4*18");
    calls.raw(".mainfn");
    for line in body.lines() {
        calls.raw(line);
    }
    calls.body(PUTTING)
}

/// `putarg`: the string at R0 written to `stdout`, then a bar.
const PUTTING: &str = r"
.putarg STMFD r13!, {r14}
LDR r1, Lstdout
BL cvec+4*104
MOV r0, #124
LDR r1, Lstdout
BL cvec+4*103
LDMFD r13!, {pc}
.Lstdout DCD cstat+44
";

fn file(probe: &Probe, name: &str) -> Vec<u8> {
    disc_file(&probe.disc, name).unwrap_or_default()
}

fn ended(probe: &Probe) -> Option<u32> {
    match probe.outcome {
        Outcome::Ended(Ended::Exit { code }) => Some(code),
        _ => None,
    }
}

#[test]
fn a_c_program_takes_both_chunks_and_starts_with_its_streams_laid() {
    let probe = run(&Build::c("B tail\n"));
    assert_eq!(probe.word(28), 0, "LibInit took chunk 2 too");
    let stream = |n: u32, word: u32| probe.statics_word(C_STATICS + 4 + FILE_BYTES * n + word);
    assert_eq!(
        [stream(0, FLAG), stream(1, FLAG), stream(2, FLAG)],
        [1, 2, 2],
        "stdin to read, stdout and stderr to write"
    );
    let first = stream(0, EXTRA);
    assert_ne!(first, 0);
    assert_eq!(
        [stream(1, EXTRA), stream(2, EXTRA)],
        [first + 36, first + 72],
        "their extra blocks in a row"
    );
}

#[test]
fn a_stream_left_open_is_delivered_and_closed_as_the_program_ends() {
    let mut calls = Calls::default();
    calls.c(87, &["LDR r0, Lp_lname", "LDR r1, Lp_lmode"], false);
    calls.raw("STR r0, keptfp");
    calls.c(104, &["LDR r0, Lp_ltext", "LDR r1, keptfp"], false);
    let data = ".keptfp DCD 0\n.lname EQUS \"HostFS:$.Left\"\nEQUB 0\n.lmode EQUS \"w\"\nEQUB 0\n.ltext EQUS \"kept\"\nEQUB 10\nEQUB 0\nALIGN\n";
    let probe = run(&Build::c(&calls.body(data)));
    assert_ne!(probe.out(0), 0, "opened");
    assert_eq!(probe.out(4), 0, "written to its buffer");
    assert_eq!(
        file(&probe, "Left"),
        b"kept\n",
        "delivered by the C library's finalisation"
    );
}

#[test]
fn main_is_entered_with_the_command_line_s_words_and_its_redirections_made() {
    let body = entering_main("MOV r4, r1\nSTR r0, [r11]\nLDR r0, [r4]\nBL putarg\nLDR r0, [r4, #4]\nBL putarg\nLDR r0, [r4, #8]\nBL putarg\nLDR r0, [r4, #12]\nSTR r0, [r11, #4]\nB tail");
    let probe = run_program(
        &program(&Build::c(&body)),
        "Run HostFS:$.CTest one \"two three\" >HostFS:$.Out",
        &[],
    );
    assert_eq!(probe.out(0), 3, "argc");
    assert_eq!(probe.out(4), 0, "argv ends with a null pointer");
    let written = file(&probe, "Out");
    let words: Vec<&[u8]> = written.split(|&b| b == b'|').collect();
    assert_eq!(
        words[1..],
        [&b"one"[..], b"two three", b""],
        "the quoted words one argument, stdout the file"
    );
    assert!(
        words[0].ends_with(b"CTest"),
        "argv[0] the program as typed: {:?}",
        words[0].escape_ascii().to_string()
    );
}

#[test]
fn main_returning_ends_the_program_with_its_answer_and_its_streams_delivered() {
    let body = entering_main("STMFD r13!, {r14}\nLDR r0, Lp_bye\nBL putarg\nMOV r0, #7\nLDMFD r13!, {pc}\n.bye EQUS \"bye\"\nEQUB 0\nALIGN");
    let probe = run_program(
        &program(&Build::c(&body)),
        "Run HostFS:$.CTest >>HostFS:$.Out",
        &[("Out", b"was ")],
    );
    assert_eq!(ended(&probe), Some(7), "exit(main's answer)");
    assert_eq!(file(&probe, "Out"), b"was bye|", "appended to");
}

#[test]
fn exit_calls_what_atexit_registered_then_ends_with_its_status() {
    let body = entering_main("ADR r0, byefn\nBL cvec+4*71\nMOV r0, #3\nBL cvec+4*72\n.byefn STMFD r13!, {r14}\nADR r0, byetext\nBL putarg\nLDMFD r13!, {pc}\n.byetext EQUS \"bye\"\nEQUB 0\nALIGN");
    let probe = run_program(
        &program(&Build::c(&body)),
        "Run HostFS:$.CTest >HostFS:$.Out",
        &[],
    );
    assert_eq!(ended(&probe), Some(3), "exit's status");
    assert_eq!(file(&probe, "Out"), b"bye|", "the function called first");
}

#[test]
fn the_program_s_own_exit_is_finalised_and_ends_it_with_the_return_code_it_asks_for() {
    let body = entering_main("STMFD r13!, {r14}\nADR r0, kept\nBL putarg\nMOV r0, #0\nLDR r1, Labex\nMOV r2, #7\nSWI \"OS_Exit\"\n.Labex DCD &58454241\n.kept EQUS \"kept\"\nEQUB 0\nALIGN");
    let probe = run_program(
        &program(&Build::c(&body)),
        "Run HostFS:$.CTest >HostFS:$.Out",
        &[],
    );
    assert_eq!(ended(&probe), Some(7), "OS_Exit's return code");
    assert_eq!(file(&probe, "Out"), b"kept|", "stdout delivered first");
}

#[test]
fn a_redirection_that_cannot_be_made_is_reported_on_stderr_and_ends_the_program_failing() {
    let body = entering_main("B tail");
    let line = "Run HostFS:$.CTest 2>HostFS:$.Err <HostFS:$.Missing";
    let probe = run_program(&program(&Build::c(&body)), line, &[]);
    assert_eq!(ended(&probe), Some(1));
    assert_eq!(
        file(&probe, "Err"),
        b"HostFS:$.Missing could not be opened for I/O redirection\n"
    );
}

#[test]
fn stderr_sent_where_stdout_goes_writes_after_it_into_one_file() {
    let body = entering_main("LDR r0, Lp_first\nBL putarg\nLDR r0, Lp_second\nLDR r1, Lstderr\nBL cvec+4*104\nLDR r0, Lp_third\nBL putarg\nB tail\n.Lstderr DCD cstat+84\n.first EQUS \"out\"\nEQUB 0\n.second EQUS \"err\"\nEQUB 0\n.third EQUS \"more\"\nEQUB 0\nALIGN");
    let probe = run_program(
        &program(&Build::c(&body)),
        "Run HostFS:$.CTest >HostFS:$.Both 2>&1",
        &[],
    );
    assert_eq!(
        file(&probe, "Both"),
        b"out|more|err",
        "each line-buffered on its own, delivered at the file's pointer as it closes in turn"
    );
}
