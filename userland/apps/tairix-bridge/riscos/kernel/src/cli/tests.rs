//! The interpreter's tests: how a line is read, the order commands are found
//! in, aliases, prefixes, redirection and the bounds that keep it finite.
//! The helpers here are the command tests' too.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::input::InputEvent;
use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::Outcome;
use crate::swi::os::CLAIM;
use crate::swi::os::CLI;
use crate::testkit::asm::{self, Condition};
use crate::testkit::{with_application, OneFile, Session, CODE, DATA};

pub(crate) const X: u32 = 1 << 17;
const FSCV: u32 = 0x0F;
/// Where a test's claimants record what they were given.
pub(crate) const SEEN: u32 = DATA + 0x800;

/// A session with an application slot.
pub(crate) fn plain() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

/// A session with an application slot, what it writes captured.
pub(crate) fn session() -> Session {
    let mut s = plain();
    s.capture_output();
    s
}

/// `line` run by `OS_CLI`: the error's number and text if it gives one.
pub(crate) fn cli(s: &mut Session, line: &str) -> Result<(), (u32, Vec<u8>)> {
    let mut text = line.as_bytes().to_vec();
    text.push(b'\r');
    s.write(DATA, &text);
    let (regs, _) = s.user_swi(X | CLI, [DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    if regs.flag(psr::V) {
        let number = s.peek(regs.r[0]);
        Err((number, s.error_text(regs.r[0])))
    } else {
        Ok(())
    }
}

/// What `line` writes, run by `OS_CLI` to its end without an error.
pub(crate) fn written(s: &mut Session, line: &str) -> Vec<u8> {
    if let Err((number, text)) = cli(s, line) {
        panic!(
            "{line}: &{number:X} {}",
            alloc::string::String::from_utf8_lossy(&text)
        );
    }
    s.output()
}

/// The text of the error `line` gives.
pub(crate) fn refused(s: &mut Session, line: &str) -> Vec<u8> {
    match cli(s, line) {
        Ok(()) => panic!("{line} succeeded"),
        Err((_, text)) => text,
    }
}

/// Claims `FSCV` with code that keeps the last call's R0 to R2 at
/// [`SEEN`] and claims starting an application, running a file — refused
/// with "File not found" when `refuses_runs` — the options and selecting a
/// filing system; it passes the rest on.
pub(crate) fn filing_system(s: &mut Session, refuses_runs: bool) {
    s.poke(DATA + 0xF00, &[0x1_00D6]);
    s.write(DATA + 0xF04, b"File not found\0");
    let at = CODE;
    let run = if refuses_runs {
        alloc::vec![
            asm::mov(0, DATA + 0xF00),
            asm::set_flags(psr::V),
            asm::claim()
        ]
    } else {
        alloc::vec![asm::claim()]
    };
    let mut code = alloc::vec![
        asm::mov(12, SEEN),
        asm::str(0, 12, 0),
        asm::str(1, 12, 4),
        asm::str(2, 12, 8),
        asm::cmp_imm(0, 4),
        asm::when(Condition::Eq, asm::b(at + 20, at + 44)),
        asm::cmp_imm(0, 2),
        asm::when(Condition::Ne, asm::cmp_imm(0, 10)),
        asm::when(Condition::Ne, asm::cmp_imm(0, 14)),
        asm::when(Condition::Ne, asm::mov_pc_lr()),
        asm::claim(),
    ];
    code.extend(run);
    s.poke(at, &code);
    let (claimed, _) = s.user_swi(X | CLAIM, [FSCV, at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(!claimed.flag(psr::V), "FSCV claimed");
}

#[test]
fn a_line_loses_its_leading_spaces_and_stars_and_a_bar_starts_a_comment() {
    let mut s = session();
    assert_eq!(written(&mut s, "  ** * Echo hello"), b"hello\n\r");
    assert_eq!(written(&mut s, "| Echo nothing"), b"");
    assert_eq!(written(&mut s, "%| Echo nothing"), b"");
    assert_eq!(written(&mut s, "   "), b"");
}

#[test]
fn a_line_past_the_longest_is_refused() {
    let mut s = session();
    let long = alloc::format!("Echo {}", "x".repeat(1100));
    assert_eq!(cli(&mut s, &long), Err((0x1E0, b"Too long".to_vec())));
    let longest = alloc::format!("  Set TairixL {}", "x".repeat(1023 - 14));
    assert_eq!(cli(&mut s, &longest), Ok(()), "1023 characters and the end");
    let over = alloc::format!("  Set TairixL {}", "x".repeat(1024 - 14));
    assert_eq!(
        cli(&mut s, &over).map_err(|(n, _)| n),
        Err(0x1E0),
        "the spaces count"
    );
}

#[test]
fn commands_are_found_whole_or_abbreviated_in_either_case() {
    let mut s = session();
    assert_eq!(written(&mut s, "echo one"), b"one\n\r");
    assert_eq!(written(&mut s, "Ec. two"), b"two\n\r");
    assert_eq!(written(&mut s, "ECHO three"), b"three\n\r");
}

#[test]
fn an_old_command_runs_on_into_its_number() {
    let mut s = session();
    // *FX0 is OS_Byte 0, whose answer is the version error.
    assert_eq!(cli(&mut s, "FX0").map_err(|(number, _)| number), Err(0xF7));
    assert_eq!(cli(&mut s, "FX 0").map_err(|(number, _)| number), Err(0xF7));
}

#[test]
fn a_wrong_number_of_parameters_gives_the_syntax_message() {
    let mut s = session();
    assert_eq!(
        cli(&mut s, "Set lonely"),
        Err((0xDC, b"Syntax: *Set <varname> <value>".to_vec()))
    );
    assert_eq!(
        cli(&mut s, "Close now"),
        Err((0xDC, b"Syntax: *Close".to_vec()))
    );
}

#[test]
fn an_unknown_command_is_run_by_the_filing_system() {
    let mut s = session();
    filing_system(&mut s, true);
    assert_eq!(
        refused(&mut s, "Nonesuch with parameters"),
        b"File not found"
    );
    assert_eq!(s.peek(SEEN), 4, "OS_FSControl 4 ran it");
    let name = s.peek(SEEN + 4);
    assert_eq!(s.read(name, 8), b"Nonesuch");
    // A slash asks for the run whatever the command would have been.
    assert_eq!(refused(&mut s, "/Echo"), b"File not found");
    let name = s.peek(SEEN + 4);
    assert_eq!(s.read(name, 4), b"Echo");
}

#[test]
fn an_alias_runs_with_its_parameters_substituted_line_by_line() {
    let mut s = session();
    written(&mut s, "Set Alias$Greet Echo hello %0|MEcho then %*1");
    assert_eq!(
        written(&mut s, "Greet world and all"),
        b"hello world\n\rthen and all\n\r"
    );
    assert_eq!(
        written(&mut s, "Gr. you"),
        b"hello you\n\rthen \n\r",
        "an alias abbreviated"
    );
    // A % before the name skips the aliases.
    filing_system(&mut s, true);
    assert_eq!(refused(&mut s, "%Greet world"), b"File not found");
}

#[test]
fn an_alias_that_calls_itself_ends_at_the_nesting_bound() {
    let mut s = session();
    written(&mut s, "Set Alias$Loop Loop");
    assert_eq!(
        cli(&mut s, "Loop").map_err(|(number, _)| number),
        Err(0x1E7)
    );
    // The interpreter is left as it was found.
    assert_eq!(written(&mut s, "Echo after"), b"after\n\r");
}

#[test]
fn a_module_prefix_runs_the_command_in_that_module_alone() {
    let mut s = session();
    assert_eq!(
        written(&mut s, "UtilityModule:Echo prefixed"),
        b"prefixed\n\r"
    );
    assert_eq!(
        written(&mut s, "Module#UtilityModule:Echo again"),
        b"again\n\r"
    );
    assert_eq!(
        cli(&mut s, "UtilityModule:Nonesuch").map_err(|(number, _)| number),
        Err(0xFE)
    );
}

#[test]
fn redirection_sends_a_command_s_output_to_a_file_and_then_ends() {
    let mut s = plain();
    let file = s.one_file(b"");
    assert_eq!(cli(&mut s, "Echo redirected { > out }"), Ok(()));
    // The space before the group stays, as RISC OS keeps it.
    assert_eq!(file.bytes(&mut s), b"redirected \n\r");
    assert_eq!(file.word(&mut s, OneFile::HANDLE), 0, "the file closed");
}

#[test]
fn a_command_waiting_for_input_keeps_what_it_has_done() {
    // *Build's lines come in after it has started waiting for them: each
    // wait is the host's, so the command is never begun again.
    let mut s = plain();
    let file = s.one_file(b"");
    for (n, &key) in b"one\rtwo\r\x1B".iter().enumerate() {
        s.arrive(100 * (n as u64 + 1), InputEvent::Key(key));
    }
    s.write(DATA, b"Build typed\r");
    let outcome = s.start_swi(X | CLI, [DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Outcome::Budget, "it ran to its end");
    assert!(!s.regs().flag(psr::V), "no error");
    assert_eq!(file.bytes(&mut s), b"one\rtwo\r");
}

#[test]
fn a_redirected_write_the_file_refuses_ends_redirection_with_its_error() {
    let mut s = plain();
    s.one_file(b"");
    // A BPutV claimant over the file's, refusing every byte.
    s.poke(DATA + 0xF00, &[0x1_00C1]);
    s.write(DATA + 0xF04, b"Disc full\0");
    let at = CODE + 0x100;
    s.poke(
        at,
        &[
            asm::mov(0, DATA + 0xF00),
            asm::set_flags(psr::V),
            asm::claim(),
        ],
    );
    let (claimed, _) = s.user_swi(X | CLAIM, [0x0B, at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(!claimed.flag(psr::V), "BPutV claimed");
    assert_eq!(refused(&mut s, "Echo lost { > out }"), b"Disc full");
    let (unchanged, _) = s.user_swi(
        X | 0x5E,
        [u32::MAX, u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(unchanged.r[..2], [0, 0], "no redirection left");
}

#[test]
fn a_redirection_has_exactly_one_space_between_its_elements() {
    use crate::cli::redirect::find;
    assert!(find(b"Echo x { > out }").is_some());
    assert!(find(b"Echo x { > out < in }").is_some(), "two in one group");
    for line in [
        &b"Echo x {  > out }"[..],
        b"Echo x { >  out }",
        b"Echo x { > out  }",
        b"Echo x { > out}",
        b"Echo x {> out }",
        b"Echo x { }",
    ] {
        assert!(
            find(line).is_none(),
            "{}",
            alloc::string::String::from_utf8_lossy(line)
        );
    }
}
