//! The kernel's own commands' tests: what each writes, sets and asks of the
//! SWIs it reaches the kernel through.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::cli::tests::{cli, filing_system, refused, session, written, SEEN, X};
use crate::memory::layout::APPLICATION;
use crate::os::{Ended, Outcome};
use crate::swi::os::{BYTE, READ_DYNAMIC_AREA, READ_VAR_VAL, SET_VAR_VAL};
use crate::testkit::{asm, Session, DATA};

/// `name`'s value as `OS_ReadVarVal` reads it unexpanded, if it is set.
fn value(s: &mut Session, name: &str) -> Option<Vec<u8>> {
    let mut text = name.as_bytes().to_vec();
    text.push(0);
    s.write(DATA + 0x400, &text);
    let (read, _) = s.user_swi(
        X | READ_VAR_VAL,
        [
            DATA + 0x400,
            DATA + 0x500,
            255,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    );
    (!read.flag(psr::V)).then(|| s.read(DATA + 0x500, read.r[2] as usize))
}

/// `OS_Byte` `reason` reading its variable.
fn byte_variable(s: &mut Session, reason: u32) -> u32 {
    let (read, _) = s.user_swi(X | BYTE, [reason, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    read.r[1]
}

#[test]
fn echo_writes_its_tail_gstransed_with_its_quotes() {
    let mut s = session();
    written(&mut s, "Set A one");
    assert_eq!(written(&mut s, "Echo <A>|M\"two\""), b"one\r\"two\"\n\r");
    assert_eq!(refused(&mut s, "Echo bad|"), b"Bad string");
    assert_eq!(s.output(), b"bad", "no newline after the error");
}

#[test]
fn error_raises_the_number_given_and_its_text_translated() {
    let mut s = session();
    written(&mut s, "Set A one");
    assert_eq!(
        cli(&mut s, "Error 123 Oops <A>"),
        Err((123, b"Oops one".to_vec()))
    );
    assert_eq!(cli(&mut s, "Error &1F Hex"), Err((0x1F, b"Hex".to_vec())));
    assert_eq!(
        cli(&mut s, "Error Oops"),
        Err((0, b"Oops".to_vec())),
        "no number is nought"
    );
    assert_eq!(
        cli(&mut s, "Error 7 as far as |"),
        Err((7, b"as far as ".to_vec()))
    );
}

#[test]
fn eval_shows_the_value_and_its_kind() {
    let mut s = session();
    assert_eq!(
        written(&mut s, "Eval 6*7"),
        b"Result is an integer, value : 42\n\r"
    );
    assert_eq!(
        written(&mut s, "Eval \"a|\" + \"<\""),
        b"Result is a string, value : a|||<\n\r"
    );
    assert_eq!(
        written(&mut s, "Eval -1"),
        b"Result is an integer, value : -1\n\r"
    );
    assert_eq!(refused(&mut s, "Eval 1+"), b"Missing operand");
}

#[test]
fn show_lists_each_variable_with_its_kind_paged() {
    let mut s = session();
    written(&mut s, "SetEval Shown$N 40+2");
    written(&mut s, "SetMacro Shown$M <A>|");
    written(&mut s, "Set Shown$S x|Gy\"");
    assert_eq!(
        written(&mut s, "Show Shown$N"),
        b"\x0EShown$N (Number) : 42\n\r\x0F"
    );
    assert_eq!(
        written(&mut s, "Show Shown$M"),
        b"\x0EShown$M (Macro) : <A>|\n\r\x0F"
    );
    assert_eq!(
        written(&mut s, "Show Shown$S"),
        b"\x0EShown$S : x|Gy|\"\n\r\x0F"
    );
    assert_eq!(
        written(&mut s, "Show Shown$*"),
        b"\x0EShown$M (Macro) : <A>|\n\rShown$N (Number) : 42\n\rShown$S : x|Gy|\"\n\r\x0F",
    );
}

#[test]
fn show_reads_a_value_longer_than_its_buffer_whole() {
    let mut s = session();
    let long = "z".repeat(600);
    written(&mut s, &alloc::format!("Set Long {long}"));
    let expected = alloc::format!("\x0ELong : {long}\n\r\x0F");
    assert_eq!(written(&mut s, "Show Long"), expected.as_bytes());
}

#[test]
fn set_setmacro_and_seteval_set_their_kinds_and_unset_deletes() {
    let mut s = session();
    written(&mut s, "Set Plain <Absent>x");
    assert_eq!(
        value(&mut s, "Plain"),
        Some(b"x".to_vec()),
        "GSTrans'd as it is set"
    );
    written(&mut s, "SetMacro Later <Plain>");
    assert_eq!(value(&mut s, "Later"), Some(b"<Plain>".to_vec()));
    written(&mut s, "SetEval Sum 1+2");
    assert_eq!(value(&mut s, "Sum"), Some(3u32.to_le_bytes().to_vec()));
    written(&mut s, "Unset Plain");
    assert_eq!(value(&mut s, "Plain"), None);
    assert_eq!(
        cli(&mut s, "Unset Nothing$Like$It"),
        Ok(()),
        "never an error"
    );
}

#[test]
fn unset_passes_a_code_variable_by_and_carries_on() {
    // A code variable's read entry gives "abc"; its write entry does nothing.
    let mut s = session();
    let code = DATA + 0x600;
    s.poke(
        code,
        &[
            asm::mov_pc_lr(),
            asm::adr(0, code + 4, code + 16),
            asm::mov(2, 3),
            asm::mov_pc_lr(),
            u32::from_le_bytes(*b"abc\0"),
        ],
    );
    s.write(DATA + 0x400, b"Code$Var\0");
    let (set, _) = s.user_swi(
        X | SET_VAR_VAL,
        [DATA + 0x400, code, 20, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(!set.flag(psr::V), "the code variable made");
    written(&mut s, "Set Code$Plain p");
    // A wildcard reaching the code variable once looped for ever.
    assert_eq!(cli(&mut s, "Unset Code$*"), Ok(()));
    assert_eq!(value(&mut s, "Code$Plain"), None, "the plain one deleted");
    assert_eq!(
        value(&mut s, "Code$Var"),
        Some(b"abc".to_vec()),
        "the code one kept"
    );
}

#[test]
fn key_sets_a_function_key_s_string_gstransed_now() {
    let mut s = session();
    written(&mut s, "Key 1 hello|M");
    assert_eq!(value(&mut s, "Key$1"), Some(b"hello\r".to_vec()));
    written(&mut s, "Key 15");
    assert_eq!(
        value(&mut s, "Key$15"),
        Some(Vec::new()),
        "set empty, not deleted"
    );
    assert_eq!(
        refused(&mut s, "Key 16 x"),
        b"Function key numbers run from 0 to 15"
    );
    assert_eq!(
        refused(&mut s, "Key x"),
        b"Function key numbers run from 0 to 15"
    );
}

#[test]
fn if_runs_the_command_its_expression_chooses() {
    let mut s = session();
    assert_eq!(
        written(&mut s, "IF 1=1 THEN Echo yes ELSE Echo no"),
        b"yes\n\r"
    );
    assert_eq!(
        written(&mut s, "IF 1=2 THEN Echo yes ELSE Echo no"),
        b"no\n\r"
    );
    assert_eq!(written(&mut s, "If 0 then Echo yes"), b"");
    assert_eq!(
        written(&mut s, "IF 1 THEN Echo only ELSE"),
        b"only\n\r",
        "an ELSE ending the line ends the command"
    );
    assert_eq!(
        refused(&mut s, "IF \"a\" THEN Echo x"),
        b"The condition is a string, not a number"
    );
    assert_eq!(refused(&mut s, "IF 1 Echo x"), b"*If needs a THEN");
    assert_eq!(refused(&mut s, "IF 1+ THEN Echo x"), b"Missing operand");
}

#[test]
fn fx_and_tv_call_os_byte_with_the_numbers_given() {
    let mut s = session();
    written(&mut s, "FX 220,65");
    assert_eq!(byte_variable(&mut s, 220), 65, "the escape character set");
    written(&mut s, "FX220 , 27");
    assert_eq!(byte_variable(&mut s, 220), 27);
    assert_eq!(refused(&mut s, "FX 1,2,3,4"), b"Too many parameters");
    assert_eq!(refused(&mut s, "FX 1,,2"), b"Bad number");
    // With no VDU drivers OS_Byte 144 is unknown: what matters is that *TV
    // alone reaches it, where RISC OS's fails to read a number.
    assert_eq!(cli(&mut s, "TV").map_err(|(number, _)| number), Err(0xFE));
    assert_eq!(refused(&mut s, "TV 1,2,3"), b"Too many parameters");
}

#[test]
fn opt_gives_the_filing_system_its_two_options() {
    let mut s = session();
    filing_system(&mut s, false);
    written(&mut s, "Opt 1, 2");
    assert_eq!(
        [s.peek(SEEN), s.peek(SEEN + 4), s.peek(SEEN + 8)],
        [10, 1, 2]
    );
    written(&mut s, "Opt 4");
    assert_eq!(
        [s.peek(SEEN), s.peek(SEEN + 4), s.peek(SEEN + 8)],
        [10, 4, 0]
    );
    written(&mut s, "Opt");
    assert_eq!(
        [s.peek(SEEN), s.peek(SEEN + 4), s.peek(SEEN + 8)],
        [10, 0, 0]
    );
    assert_eq!(
        refused(&mut s, "Opt 1,"),
        b"Bad number",
        "a comma needs its number"
    );
}

#[test]
fn go_starts_the_code_at_the_address_as_the_application() {
    let mut s = session();
    filing_system(&mut s, false);
    let entry = APPLICATION + 0x7000;
    s.poke(entry, &[asm::mov(1, 0), asm::swi(0x11)]);
    s.write(DATA, b"GO F000 ; with these\r");
    let outcome = s.start_swi(X | 0x05, [DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        outcome,
        Outcome::Ended(Ended::Exit { code: 0 }),
        "the code ran and exited"
    );
    assert_eq!(s.peek(SEEN), 2, "started as an application");
    let tail = s.peek(SEEN + 4);
    assert_eq!(s.read(tail, 10), b"with these");
    assert_eq!(s.peek(SEEN + 8), entry);
}

#[test]
fn quit_exits_the_application() {
    let mut s = session();
    s.write(DATA, b"Quit\r");
    let outcome = s.start_swi(X | 0x05, [DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(outcome, Outcome::Ended(Ended::Exit { code: 0 }));
}

#[test]
fn ignore_sets_the_printer_s_ignored_character_or_none() {
    let mut s = session();
    written(&mut s, "Ignore 7");
    assert_eq!(byte_variable(&mut s, 0xF6), 7);
    assert_eq!(byte_variable(&mut s, 0xB6), 0, "honoured");
    written(&mut s, "Ignore");
    assert_eq!(byte_variable(&mut s, 0xB6), 255, "none");
    assert_eq!(refused(&mut s, "Ignore 7x"), b"Bad number");
    assert_eq!(refused(&mut s, "Shadow 2x"), b"Bad number");
}

#[test]
fn cache_turns_the_caches_on_and_off_and_says_which() {
    let mut s = session();
    written(&mut s, "Cache Off");
    assert_eq!(written(&mut s, "Cache"), b"Cache Off\n\r");
    written(&mut s, "cache on");
    assert_eq!(written(&mut s, "Cache"), b"Cache On\n\r");
    assert_eq!(refused(&mut s, "Cache Maybe"), b"Invalid parameters");
    assert_eq!(
        refused(&mut s, "Cache On x"),
        b"Syntax: *Cache [On|Off]",
        "one parameter at most"
    );
}

#[test]
fn change_dynamic_area_sets_each_area_named_to_its_size() {
    let mut s = session();
    written(&mut s, "ChangeDynamicArea -RMASize 2M");
    let (rma, _) = s.user_swi(
        X | READ_DYNAMIC_AREA,
        [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(rma.r[1] >= 2 << 20, "the RMA is {:#X}", rma.r[1]);
    assert_eq!(
        refused(&mut s, "ChangeDynamicArea -RMASize 1x"),
        b"Bad number"
    );
}
