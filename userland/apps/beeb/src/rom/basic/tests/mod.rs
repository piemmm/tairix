//! BASIC against BASIC II as seen from outside: lines typed at the keyboard
//! of a machine running the clean-room MOS, and what the screen and memory
//! show, as BASIC II showed them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::build;
use crate::machine::Machine;
use crate::scenario::{mode7_text, type_text, wait};

mod assembler;
mod control;
mod editor;
mod numbers;
mod output;
mod values;

/// How long the machine takes to reach BASIC's first prompt.
const BOOT: u64 = 150;
/// A prompt and the first of a typed line's characters on its screen line.
const PROMPTED: usize = 39;

/// A machine with the clean-room MOS and BASIC, at the first prompt.
fn basic() -> Machine {
    let os = crate::rom::mos::build().expect("the MOS assembles");
    let rom = build().expect("BASIC assembles");
    let mut machine = Machine::new(&os.bytes).expect("a 16 KiB OS");
    machine.insert_rom(15, &rom.bytes).expect("slot 15 exists");
    wait(&mut machine, BOOT);
    machine
}

/// The machine after typing `lines`, each followed by RETURN, and a pause
/// for the last to run.
fn typed(lines: &[&str], centiseconds: u64) -> Machine {
    let mut machine = basic();
    for line in lines {
        type_text(&mut machine, line).expect("a key types each character");
        type_text(&mut machine, "\n").expect("a key types each character");
    }
    wait(&mut machine, centiseconds);
    machine
}

/// The screen's lines after typing `lines`.
fn after(lines: &[&str], centiseconds: u64) -> Vec<String> {
    mode7_text(&typed(lines, centiseconds))
}

/// The screen from the first line's prompt to its last text, after typing
/// `lines`, is what BASIC II showed.
fn assert_screen(lines: &[&str], centiseconds: u64, expected: &[&str]) {
    let first = format!(">{}", &lines[0][..lines[0].len().min(PROMPTED)]);
    assert_screen_from(lines, centiseconds, &first, expected);
}

/// The screen from the line `first` to its last text, after typing
/// `lines`, is what BASIC II showed.
fn assert_screen_from(lines: &[&str], centiseconds: u64, first: &str, expected: &[&str]) {
    assert_eq!(from(&after(lines, centiseconds), first), expected);
}

/// The packed reals a program stores from `at`, a row of `names.len()` for
/// each row of `expected`, are BASIC II's, each given in hex.
fn bytes_after<const N: usize>(
    program: &[&str],
    centiseconds: u64,
    at: u16,
    names: &[&str; N],
    expected: &[[&str; N]],
) {
    let machine = typed(program, centiseconds);
    let mut address = at;
    for (row, results) in expected.iter().enumerate() {
        for (name, result) in names.iter().zip(results) {
            let stored: Vec<u8> = (0..5u16).map(|i| machine.peek(address + i)).collect();
            let wanted: Vec<u8> = (0..5)
                .map(|k| u8::from_str_radix(&result[2 * k..2 * k + 2], 16).expect("hex"))
                .collect();
            assert_eq!(stored, wanted, "{name} in row {row}");
            address += 5;
        }
    }
}

/// The screen's lines from the first that is `from`, to the last with text.
fn from(screen: &[String], from: &str) -> Vec<String> {
    let start = screen
        .iter()
        .position(|line| line == from)
        .unwrap_or_else(|| panic!("{from:?} on the screen: {screen:?}"));
    let end = screen
        .iter()
        .rposition(|line| !line.is_empty())
        .map_or(start, |last| last + 1);
    screen[start..end].to_vec()
}

#[test]
fn the_header_names_basic_ii() {
    let image = build().expect("BASIC assembles");
    let bytes = &image.bytes;
    assert_eq!(bytes.len(), 0x4000);
    assert_eq!(&bytes[..3], &[0xC9, 0x01, 0xF0], "the entry code first");
    assert_eq!(bytes[6], 0x60, "a language with a relocation address");
    assert_eq!(bytes[8], 1, "BASIC II's version");
    assert_eq!(&bytes[9..14], b"BASIC");
    let copyright = usize::from(bytes[7]);
    assert_eq!(&bytes[copyright..copyright + 4], b"\0(C)");
}

#[test]
fn it_starts_at_the_prompt() {
    let machine = basic();
    let screen = mode7_text(&machine);
    assert_eq!(screen[..6], ["", "BBC Computer 32K", "", "BASIC", "", ">"]);
}

#[test]
fn a_numbered_line_is_kept_tokenised_and_listed_back() {
    let screen = after(&["20P.\"B\"", "10PRINT\"A\";", "30GOTO 10", "LIST"], 100);
    assert_eq!(
        from(&screen, ">LIST"),
        [
            ">LIST",
            "   10PRINT\"A\";",
            "   20PRINT\"B\"",
            "   30GOTO 10",
            ">"
        ]
    );
}

#[test]
fn names_beginning_with_conditional_keywords_stay_names() {
    let screen = after(&["10TIMER=TIME", "20PRINTX", "LIST"], 100);
    assert_eq!(
        from(&screen, ">LIST"),
        [">LIST", "   10TIMER=TIME", "   20PRINTX", ">"]
    );
}

#[test]
fn names_holding_keywords_stay_names() {
    let screen = after(
        &[
            "ATO=5",
            "PRINT ATO",
            "XPRINT=3",
            "PRINT XPRINT",
            "BTOP=7",
            "PRINT BTOP",
        ],
        100,
    );
    assert_eq!(
        from(&screen, ">ATO=5"),
        [
            ">ATO=5",
            ">PRINT ATO",
            "         5",
            ">XPRINT=3",
            ">PRINT XPRINT",
            "         3",
            ">BTOP=7",
            ">PRINT BTOP",
            "         7",
            ">"
        ]
    );
}

#[test]
fn run_runs_the_program_in_line_order() {
    let screen = after(&["20PRINT\"B\"", "10PRINT\"A\";", "RUN"], 100);
    assert_eq!(from(&screen, ">RUN"), [">RUN", "AB", ">"]);
}

#[test]
fn a_line_number_alone_deletes_its_line_and_with_a_space_keeps_one() {
    let screen = after(&["10PRINT\"A\"", "20PRINT\"B\"", "10", "30 ", "LIST"], 100);
    assert_eq!(
        from(&screen, ">LIST"),
        [">LIST", "   20PRINT\"B\"", "   30", ">"]
    );
}

#[test]
fn old_takes_back_what_new_did() {
    let screen = after(&["10PRINT\"A\"", "NEW", "LIST", "OLD", "LIST"], 100);
    assert_eq!(
        from(&screen, ">NEW"),
        [">NEW", ">LIST", ">OLD", ">LIST", "   10PRINT\"A\"", ">"]
    );
}

#[test]
fn errors_are_reported_on_a_line_of_their_own_with_the_line_run() {
    let screen = after(&["32768", "Q", "10Q", "RUN"], 100);
    assert_eq!(
        from(&screen, ">32768"),
        [
            ">32768",
            "",
            "Syntax error",
            ">Q",
            "",
            "Mistake",
            ">10Q",
            ">RUN",
            "",
            "Mistake at line 10",
            ">"
        ]
    );
}
